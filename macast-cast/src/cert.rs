//! The certificate pair the Cast TLS face runs on, generated **in-process**.
//!
//! Python counterpart: `Service._ensure_self_signed_cert()` in `macast/server.py`
//! (lines 338-391) plus `ChromecastProtocol._ensure_cert()` in
//! `macast/protocol_cast.py` (478-435). Two functions, and the split is the
//! first thing to understand about this module: the *protocol* only ever asks
//! "do my two files exist", and when they do not it calls into the *service*,
//! which owns writing `macast.crt` / `macast.key` and returns those paths. So
//! the pair named `macast_cast.*` is a slot the receiver would prefer, and the
//! shared pair is what actually gets created — and both are adopted rather than
//! duplicated. [`ensure_pair`] keeps that order verbatim, because the order is
//! also the answer to "why does this machine have one certificate and that one
//! two".
//!
//! ## Why the key algorithm is not Python's
//!
//! Python shells out to `openssl req -x509 -newkey rsa:2048`. This generates an
//! **ECDSA P-256** certificate, and that is a written-down deviation rather than
//! an accident: `rcgen`'s `KeyPair::generate()` is
//! `generate_for(&PKCS_ECDSA_P256_SHA256)`, and its RSA generation path is
//! compiled only under the `aws_lc_rs` feature — which the workspace does not
//! enable, precisely so that no build needs a C toolchain (see the provider note
//! in the root manifest). A 256-bit ECDSA leaf is smaller and faster to verify
//! than the 2048-bit RSA Python ships, and nothing in this chain loses security
//! from the swap: our receiver is **uncertified**, senders never validate the
//! chain against a trust anchor, and the device-auth answers this crate encodes
//! in [`crate::proto`] carry an *empty* `signature` and no client certificate, so
//! these bytes never travel as attestation. They have to do exactly one thing —
//! complete a TLS handshake on port 8009.
//!
//! ## Why the validity window is not Python's either
//!
//! `-days 3650` versus rcgen's default of 1975-01-01 through 4096-01-01. The two
//! ends of that difference are not equal risk: a long window can only ever mean
//! "the receiver still works in a year", while a ten-year window expiring quietly
//! on a machine nobody re-tests is the failure mode AGENTS.md §4.2 calls a
//! silent one. Pinning the window would need `time::OffsetDateTime` as a direct
//! dependency (rcgen does not re-export `time`), so the deviation stays and is
//! written here instead of being paid for.
//!
//! ## Why nothing here touches the settings table
//!
//! Python's generator ends with `Setting.set(SettingProperty.Https_Cert)` and
//! `Https_Key`. This module returns paths and **does not** write them. Reading
//! `Setting.get` from a library crate would make every call site a potential
//! persistence side effect, and AGENTS.md §4.2's first bullet is that a setting
//! read has a side effect too; the shell that owns the tables decides whether the
//! pair is worth remembering.
//!
//! ## The two failure lessons this module is shaped by
//!
//! **§4.2, "写文件的代码要自己把目录建出来."** On a clean machine the config
//! directory did not exist — only `Setting.save()` created it — so openssl
//! exited non-zero, the one log line explained it by blaming `-addext`, and the
//! Cast TLS receiver never came up on a fresh install (including every CI
//! runner). [`ensure_pair`] therefore creates the directory *before* any write
//! and, if that fails, names the directory in the error instead of guessing at
//! the cause.
//!
//! **Never overwrite a pair we did not make.** Adoption is tested by file
//! existence alone, exactly like Python, and validation deliberately happens
//! later in [`Pair::server_config`]. That ordering is the guarantee: a foreign or
//! corrupt certificate already sitting in the config directory is reported by
//! name while the bytes stay in place, because a receiver that cannot start is
//! recoverable and one that deleted someone's certificate is not.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use macast_core::logging::Logger;
use macast_core::paths;
use rcgen::{CertificateParams, DnType, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

/// The slot `ChromecastProtocol.__init__` aims at first
/// (`protocol_cast.py:402-403`).
pub const CAST_CERT_FILE: &str = "macast_cast.crt";
/// [`CAST_CERT_FILE`]'s private key.
pub const CAST_KEY_FILE: &str = "macast_cast.key";
/// What `Service._ensure_self_signed_cert()` writes (`server.py:343`).
pub const SHARED_CERT_FILE: &str = "macast.crt";
/// [`SHARED_CERT_FILE`]'s private key (`server.py:344`).
pub const SHARED_KEY_FILE: &str = "macast.key";

/// `-subj /CN=Macast Local Service` (`server.py:372`), character for character:
/// this string is in the DER, and a sender or a user may be reading it.
pub const COMMON_NAME: &str = "Macast Local Service";

/// `-addext subjectAltName=DNS:localhost,IP:127.0.0.1`, **in that order**.
///
/// rcgen decides the SAN type by trying to parse each entry as an address, so
/// `"127.0.0.1"` becomes an IP entry and `"localhost"` a DNS name without this
/// module naming a type — the same two entries Python asks openssl for, which is
/// why the list is public and ordered rather than a `Vec` someone may reorder.
pub const SUBJECT_ALTERNATIVE_NAMES: [&str; 2] = ["localhost", "127.0.0.1"];

/// Which of the three outcomes produced the paths in a [`Pair`].
///
/// Not telemetry: the distinction is behaviour, because "the Cast slot already
/// had a pair" and "we just wrote one" mean different things to whoever is
/// wondering why the 8443 HTTPS face and the 8009 TLS face share a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Both `macast_cast.*` files existed — Python's first branch.
    CastPair,
    /// The Cast slot was incomplete but `macast.*` was not — the reuse path, and
    /// the one that makes the two faces agree.
    SharedPair,
    /// Neither existed, so this module wrote the shared pair — Python's
    /// `_ensure_self_signed_cert()` doing the only thing it can.
    Generated,
}

/// A certificate and a private key that exist (or were just created) on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub source: Source,
}

/// Every way this module can refuse, and the only reason a `PathBuf` rides along
/// with most of them: each sentence has to name the file the user can move.
///
/// The shape is `macast_core::trash::TrashError`'s — a plain enum carrying the
/// paths and the reason string, [`CertError::message`] as the Chinese sentence a
/// person acts on, and `Display` delegating to it. No `thiserror`, because
/// nothing else in the workspace uses one and the sentences are ours anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertError {
    /// The directory could not be created — §4.2's failure, named here rather
    /// than blamed on whatever the generator would have said next.
    CreateDir(PathBuf, String),
    /// A file could not be written into a directory that does exist.
    Write(PathBuf, String),
    /// rcgen refused to produce a key or a certificate.
    Generate(String),
    /// One of the two files could not be read or decoded as PEM.
    Read(PathBuf, String),
    /// The certificate file parsed to **zero** certificates: present, readable,
    /// and not a PEM certificate at all.
    NoCertificate(PathBuf),
    /// The key file parsed to no private key. A file of zero bytes is the common
    /// shape (a download or an editor that truncated it).
    NoKey(PathBuf),
    /// Both files decoded, and rustls still will not accept the pair — a key
    /// that does not match the certificate, or a signing algorithm its provider
    /// does not offer.
    NotUsable(PathBuf, PathBuf, String),
}

impl CertError {
    /// The sentence a user reads. English stays in the log lines (AGENTS.md's
    /// Rust convention); this is the one place the answer is *shown*, so it is
    /// Chinese and it names a next step.
    pub fn message(&self) -> String {
        match self {
            CertError::CreateDir(dir, reason) => format!(
                "无法创建证书目录 {}：{}。Cast 的 TLS 接收端（8009 端口）需要先把这对证书写进去。请确认这个目录可写，或者换一个可写的配置目录后重启 Macast。",
                dir.display(),
                reason
            ),
            CertError::Write(path, reason) => format!(
                "无法写入 {}：{}。这一对文件就住在配置目录里，把同名的旧文件移到别处留档（移走它们不会丢别的东西），然后重启 Macast 重新生成一对。",
                path.display(),
                reason
            ),
            CertError::Generate(reason) => format!(
                "生成自签证书失败：{reason}。这不需要 openssl，是进程内生成的；如果配置目录本身可写，请重启 Macast 再试一次。"
            ),
            CertError::Read(path, reason) => format!(
                "读取证书文件 {} 失败：{}。把这一对文件（证书和私钥）移到别处留档，再重启 Macast 生成新的一对。",
                path.display(),
                reason
            ),
            CertError::NoCertificate(path) => format!(
                "{} 里没有读到任何 PEM 证书。这个文件我们不会覆盖，也不会替你删掉：确认它不是你在用的证书后，把这一对文件移走再重启 Macast 生成新的一对。",
                path.display()
            ),
            CertError::NoKey(path) => format!(
                "{} 里没有读到私钥。私钥必须和证书是同一对：把这一对文件一起移走，重启 Macast 会重新生成一对匹配的。",
                path.display()
            ),
            CertError::NotUsable(cert, key, reason) => format!(
                "证书 {} 和私钥 {} 无法一起用于 TLS 服务：{reason}。通常意味着这两份文件不是一对，把这一对一起移走再重启 Macast 重新生成。",
                cert.display(),
                key.display()
            ),
        }
    }
}

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CertError {}

/// Find — or, if there is nothing to find, make — the pair this receiver runs on.
///
/// `dir` is explicit rather than read from [`paths::setting_dir`] because the
/// caller is the service, and a test needs to point it somewhere that is not
/// anyone's configuration directory (AGENTS.md §10). The
/// [`ensure_in_setting_dir`] wrapper is the app-facing shape.
///
/// Writes exactly one log line, and only when it generated: Python logs nothing
/// on either adoption branch (`_ensure_cert` just reassigns two attributes) and
/// logs one INFO line after a successful generation.
pub fn ensure_pair(dir: &Path, log: &Logger) -> Result<Pair, CertError> {
    let cast = Pair {
        cert: dir.join(CAST_CERT_FILE),
        key: dir.join(CAST_KEY_FILE),
        source: Source::CastPair,
    };
    if pair_exists(&cast) {
        return Ok(cast);
    }
    let shared = Pair {
        cert: dir.join(SHARED_CERT_FILE),
        key: dir.join(SHARED_KEY_FILE),
        source: Source::SharedPair,
    };
    if pair_exists(&shared) {
        return Ok(shared);
    }
    let (cert, key) = generate(dir, log)?;
    Ok(Pair {
        cert,
        key,
        source: Source::Generated,
    })
}

/// [`ensure_pair`] against the config directory the process resolved.
pub fn ensure_in_setting_dir(log: &Logger) -> Result<Pair, CertError> {
    ensure_pair(&paths::setting_dir(), log)
}

fn pair_exists(pair: &Pair) -> bool {
    // Two `Path::exists` calls, no reading: this is Python's test
    // (`os.path.exists(cert) and os.path.exists(key)`) and it is the reason a
    // broken pair is reported by name later instead of being replaced now.
    pair.cert.exists() && pair.key.exists()
}

/// Write `macast.crt` / `macast.key` into `dir`, creating `dir` first.
fn generate(dir: &Path, log: &Logger) -> Result<(PathBuf, PathBuf), CertError> {
    // §4.2: the code that writes a file is the code that makes the directory.
    std::fs::create_dir_all(dir)
        .map_err(|e| CertError::CreateDir(dir.to_path_buf(), e.to_string()))?;

    let cert_path = dir.join(SHARED_CERT_FILE);
    let key_path = dir.join(SHARED_KEY_FILE);

    let key_pair = KeyPair::generate().map_err(|e| CertError::Generate(e.to_string()))?;
    let names: Vec<String> = SUBJECT_ALTERNATIVE_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut params =
        CertificateParams::new(names).map_err(|e| CertError::Generate(e.to_string()))?;
    params
        .distinguished_name
        .push(DnType::CommonName, COMMON_NAME);
    // `self_signed` consumes `params`, which is why the two writes are not
    // factored into one "sign and put both files somewhere" helper.
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| CertError::Generate(e.to_string()))?;

    // PEM on disk, in that order, because that is the shape Python has: the
    // generator writes `macast.crt` / `macast.key`, and the consumer opens them
    // again. `Pair::server_config` is the only reader, so the reuse path and the
    // fresh path go through the same parser instead of a second, less-tested one.
    write_file(&cert_path, cert.pem().as_bytes())?;
    write_file(&key_path, key_pair.serialize_pem().as_bytes())?;

    log.info(&format!(
        "Generated self-signed certificate: {}",
        cert_path.display()
    ));
    Ok((cert_path, key_path))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CertError> {
    std::fs::write(path, bytes).map_err(|e| CertError::Write(path.to_path_buf(), e.to_string()))
}

impl Pair {
    /// Build the rustls server configuration for this pair.
    ///
    /// Both files are read back from disk here, never handed over in memory from
    /// [`generate`] — again on purpose: the adoption branches have no in-memory
    /// form to hand over, so if this path took bytes the Cast receiver would have
    /// one config path for a fresh install and another for the machine that
    /// already has a certificate.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, CertError> {
        let chain = read_certs(&self.cert)?;
        if chain.is_empty() {
            return Err(CertError::NoCertificate(self.cert.clone()));
        }
        let key = read_key(&self.key)?;
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map(Arc::new)
            .map_err(|e| {
                CertError::NotUsable(self.cert.clone(), self.key.clone(), e.to_string())
            })
    }
}

/// PEM certificates → DER. `pub(crate)` because it is also the tests' only
/// route: rustls-pemfile does the base64, so no test needs a base64 dependency
/// to look at what we wrote.
pub(crate) fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, CertError> {
    let file = std::fs::File::open(path)
        .map_err(|e| CertError::Read(path.to_path_buf(), e.to_string()))?;
    let mut reader = BufReader::new(file);
    let mut chain = Vec::new();
    for entry in rustls_pemfile::certs(&mut reader) {
        chain.push(entry.map_err(|e| CertError::Read(path.to_path_buf(), e.to_string()))?);
    }
    Ok(chain)
}

fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>, CertError> {
    let file = std::fs::File::open(path)
        .map_err(|e| CertError::Read(path.to_path_buf(), e.to_string()))?;
    let mut reader = BufReader::new(file);
    match rustls_pemfile::private_key(&mut reader) {
        Ok(Some(key)) => Ok(key),
        // `Ok(None)` is "there was a file, it was readable, and it holds no key"
        // — a distinct answer from `Read`, and the one a truncated file gives.
        Ok(None) => Err(CertError::NoKey(path.to_path_buf())),
        Err(e) => Err(CertError::Read(path.to_path_buf(), e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own per test, because these tests write certificates
    /// and one shared temp directory would make "the bytes stayed in place" a
    /// claim about whichever test ran last.
    fn dir_for(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macast-cast-cert-{}-{}-{}",
            std::process::id(),
            tag,
            super::unique_slot()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        dir
    }

    fn log(tag: &str) -> Arc<Logger> {
        Logger::named(&format!("CertTest_{tag}"))
    }

    fn log_tail() -> String {
        macast_core::logging::read_tail(&paths::log_file(), 4000).text
    }

    #[test]
    fn a_clean_directory_gets_both_files_and_says_so() {
        crate::pin_test_dir();
        let dir = dir_for("fresh");
        let pair = ensure_pair(&dir, &log("fresh")).expect("a clean directory must generate");

        assert_eq!(pair.source, Source::Generated);
        assert_eq!(pair.cert, dir.join(SHARED_CERT_FILE));
        assert_eq!(pair.key, dir.join(SHARED_KEY_FILE));
        // Python's generator writes the **shared** names and `_ensure_cert()`
        // then adopts them; the `macast_cast.*` slot is only ever read.
        assert!(!dir.join(CAST_CERT_FILE).exists());
        assert!(!dir.join(CAST_KEY_FILE).exists());

        let cert = std::fs::read_to_string(&pair.cert).expect("the certificate was just written");
        let key = std::fs::read_to_string(&pair.key).expect("the key was just written");
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----"), "{cert}");
        assert!(
            key.starts_with("-----BEGIN EC PRIVATE KEY-----")
                || key.starts_with("-----BEGIN PRIVATE KEY-----"),
            "{key}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_is_made_by_the_code_that_writes_it() {
        crate::pin_test_dir();
        // The other tests all hand `ensure_pair` a directory that already
        // exists, which is why deleting the `create_dir_all` inside [`generate`]
        // reds only the *failure* wording: nothing else on this module was
        // asking whether the directory got made. That is exactly the shape of
        // §4.2's bug — on a clean machine the config directory is what is
        // missing, the generator is told to write into it anyway, and the log
        // blames the generator. So the success half is asked here, two levels
        // deep, against a root no helper pre-created.
        let root = dir_for("deep");
        let dir = root.join("Library").join("Application Support").join("Macast");
        assert!(!dir.exists(), "the root is fresh: nothing has made this path");

        let pair = ensure_pair(&dir, &log("deep"))
            .expect("the code that writes a file is the code that makes the directory");
        assert_eq!(pair.source, Source::Generated);
        assert!(dir.is_dir(), "the directory was created, not merely reported");
        assert!(pair.cert.exists() && pair.key.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_second_call_reuses_the_bytes_it_already_wrote() {
        crate::pin_test_dir();
        let dir = dir_for("reuse");
        let first = ensure_pair(&dir, &log("reuse1")).expect("generation");
        let cert_before = std::fs::read(&first.cert).expect("read cert");
        let key_before = std::fs::read(&first.key).expect("read key");

        let second = ensure_pair(&dir, &log("reuse2")).expect("adoption");
        assert_eq!(second.source, Source::SharedPair);
        assert_eq!(second, first.with_source(Source::SharedPair));
        assert_eq!(std::fs::read(&second.cert).expect("re-read"), cert_before);
        assert_eq!(std::fs::read(&second.key).expect("re-read"), key_before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pair_in_the_cast_slot_wins_and_the_shared_slot_is_left_alone() {
        crate::pin_test_dir();
        let dir = dir_for("castwins");
        // Not certificates — content nobody else could have written, so that
        // "untouched" is measured rather than assumed.
        std::fs::write(dir.join(CAST_CERT_FILE), b"cast-slot-cert").expect("seed cast cert");
        std::fs::write(dir.join(CAST_KEY_FILE), b"cast-slot-key").expect("seed cast key");
        std::fs::write(dir.join(SHARED_CERT_FILE), b"shared-slot-cert").expect("seed shared cert");
        std::fs::write(dir.join(SHARED_KEY_FILE), b"shared-slot-key").expect("seed shared key");

        let pair = ensure_pair(&dir, &log("castwins")).expect("the cast slot is complete");
        assert_eq!(pair.source, Source::CastPair);
        assert_eq!(pair.cert, dir.join(CAST_CERT_FILE));
        assert_eq!(pair.key, dir.join(CAST_KEY_FILE));
        assert_eq!(
            std::fs::read(dir.join(SHARED_CERT_FILE)).expect("re-read shared cert"),
            b"shared-slot-cert"
        );
        assert_eq!(
            std::fs::read(dir.join(SHARED_KEY_FILE)).expect("re-read shared key"),
            b"shared-slot-key"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_half_cast_slot_falls_to_the_shared_pair_without_writing_anything() {
        crate::pin_test_dir();
        let dir = dir_for("halfcast");
        std::fs::write(dir.join(CAST_CERT_FILE), b"orphan-cert").expect("seed orphan cert");
        std::fs::write(dir.join(SHARED_CERT_FILE), b"shared-cert").expect("seed shared cert");
        std::fs::write(dir.join(SHARED_KEY_FILE), b"shared-key").expect("seed shared key");

        let pair = ensure_pair(&dir, &log("halfcast")).expect("the shared pair is complete");
        assert_eq!(pair.source, Source::SharedPair);
        assert_eq!(pair.cert, dir.join(SHARED_CERT_FILE));
        // The orphan is still there, and no key was written to pair with it: a
        // missing half is *not* an invitation to generate.
        assert_eq!(
            std::fs::read(dir.join(CAST_CERT_FILE)).expect("re-read orphan"),
            b"orphan-cert"
        );
        assert!(!dir.join(CAST_KEY_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_that_cannot_be_made_names_the_directory() {
        crate::pin_test_dir();
        let blocker = dir_for("blocked");
        let blocker_file = blocker.join("not-a-directory");
        std::fs::write(&blocker_file, b"x").expect("seed blocker file");
        let dir = blocker_file.join("nested");

        let error = match ensure_pair(&dir, &log("blocked")) {
            Ok(pair) => panic!("a regular file cannot be a parent directory, got {:?}", pair),
            Err(e) => e,
        };
        assert!(
            matches!(error, CertError::CreateDir(_, _)),
            "the failure must be the directory, not a guess about the generator: {error:?}"
        );
        // The §4.2 lesson in one assertion: the sentence points at the directory
        // that has to be made writable, and never at a reason we invented.
        assert!(error.message().contains("not-a-directory"));
        assert!(error.message().contains(&dir.display().to_string()));
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&blocker);
    }

    #[test]
    fn the_generated_certificate_carries_both_subject_alternative_names() {
        crate::pin_test_dir();
        let dir = dir_for("der");
        let pair = ensure_pair(&dir, &log("der")).expect("generation");
        let chain = read_certs(&pair.cert).expect("the file we wrote is PEM");
        assert_eq!(chain.len(), 1, "one leaf, no chain");
        let der = &chain[0].as_ref()[..];

        assert!(
            der.windows(COMMON_NAME.len()).any(|w| w == COMMON_NAME.as_bytes()),
            "the CN a user may read in a sender's device list is missing from the DER"
        );
        // `GeneralName` is a CHOICE, so each entry carries its own context tag:
        // `[2]` = dNSName and `[7]` = iPAddress, both primitive and short. The
        // DER therefore spells 127.0.0.1 as `87 04 7f 00 00 01` — a text scan
        // can never find it, and the tag is part of the answer, not decoration:
        // `[6]` would be uniformResourceIdentifier, which no sender reads as an
        // address. Both needles are built **from** `SUBJECT_ALTERNATIVE_NAMES`
        // rather than copied out of it, so this asks the question the const
        // actually answers: did the generator type each entry the way the entry
        // reads, and did it keep the declared order — which is what Python asked
        // openssl for?
        let dns_entry = {
            let name = SUBJECT_ALTERNATIVE_NAMES[0].as_bytes();
            [&[0x82u8, name.len() as u8][..], name].concat()
        };
        let ip_entry = {
            let std::net::IpAddr::V4(loopback) = SUBJECT_ALTERNATIVE_NAMES[1]
                .parse()
                .expect("the second SAN entry is a v4 address")
            else {
                panic!("the second SAN entry is not a v4 address");
            };
            [&[0x87u8, 4][..], &loopback.octets()[..]].concat()
        };
        let dns = der
            .windows(dns_entry.len())
            .position(|w| w == &dns_entry[..]);
        let ip = der
            .windows(ip_entry.len())
            .position(|w| w == &ip_entry[..]);
        assert!(
            dns.is_some(),
            "the DNS subjectAltName is missing from the DER as a [2] dNSName entry"
        );
        assert!(
            ip.is_some(),
            "the IP subjectAltName is missing from the DER as a [7] iPAddress entry"
        );
        assert!(
            dns.expect("checked above") < ip.expect("checked above"),
            "subjectAltName must list localhost before 127.0.0.1, the order the const declares"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pair_builds_a_server_config() {
        crate::pin_test_dir();
        let dir = dir_for("config");
        let pair = ensure_pair(&dir, &log("config")).expect("generation");
        let config = pair
            .server_config()
            .expect("a pair we just generated must configure rustls");
        // The provider question, answered by the compiler rather than by this
        // line: `ServerConfig::builder()` resolves the process-default provider,
        // which exists here only because `ring` is the one feature enabled.
        // What we can ask without a handshake is what we *don't* speak: Cast
        // senders negotiate no ALPN protocol, and neither do we, so the list
        // must stay empty — a stray `h2` would make every sender's TLS 1.2
        // handshake fail on a server that otherwise looks healthy.
        assert!(config.alpn_protocols.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_certificate_and_a_key_that_are_not_a_pair_are_refused_by_name() {
        crate::pin_test_dir();
        let first = dir_for("cross1");
        let second = dir_for("cross2");
        ensure_pair(&first, &log("cross1")).expect("first generation");
        ensure_pair(&second, &log("cross2")).expect("second generation");
        let cert_bytes = std::fs::read(first.join(SHARED_CERT_FILE)).expect("read cert A");
        let key_bytes = std::fs::read(second.join(SHARED_KEY_FILE)).expect("read key B");

        // Two real pairs, cross-wired into the Cast slot. Existence still adopts
        // them — this module's promise is that it never deletes what it did not
        // make — so the refusal has to arrive at the reader, and it has to
        // arrive because rustls compares the key's SPKI against the
        // certificate's, not because a handshake went wrong.
        let dir = dir_for("crossed");
        std::fs::write(dir.join(CAST_CERT_FILE), &cert_bytes).expect("seed cert A");
        std::fs::write(dir.join(CAST_KEY_FILE), &key_bytes).expect("seed key B");
        let pair = ensure_pair(&dir, &log("crossed")).expect("both halves exist");
        assert_eq!(pair.source, Source::CastPair);

        let error = match pair.server_config() {
            Ok(_) => panic!("two different keypairs never make one TLS identity"),
            Err(e) => e,
        };
        assert!(
            matches!(&error, CertError::NotUsable(c, k, _)
                if *c == dir.join(CAST_CERT_FILE) && *k == dir.join(CAST_KEY_FILE)),
            "the refusal must name both files: {error:?}"
        );
        assert!(error.message().contains(CAST_CERT_FILE));
        assert!(error.message().contains(CAST_KEY_FILE));
        assert_eq!(
            std::fs::read(dir.join(CAST_CERT_FILE)).expect("re-read cert"),
            cert_bytes,
            "a mismatch is reported, not repaired"
        );
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_existing_pair_is_reported_and_its_bytes_stay() {
        crate::pin_test_dir();
        let dir = dir_for("corrupt");
        std::fs::write(dir.join(CAST_CERT_FILE), b"not a certificate").expect("seed cert");
        std::fs::write(dir.join(CAST_KEY_FILE), b"not a key").expect("seed key");

        // Adoption does not validate, so the pair is still "found"…
        let pair = ensure_pair(&dir, &log("corrupt")).expect("existence is the only test");
        assert_eq!(pair.source, Source::CastPair);
        // …and the refusal comes from the reader, naming both files.
        let error = match pair.server_config() {
            Ok(_) => panic!("a file of prose must never configure TLS"),
            Err(e) => e,
        };
        assert!(
            matches!(&error, CertError::NoCertificate(path) if *path == dir.join(CAST_CERT_FILE)),
            "the refusal must name the certificate file: {error:?}"
        );
        assert!(error.message().contains(CAST_CERT_FILE));
        assert_eq!(
            std::fs::read(dir.join(CAST_CERT_FILE)).expect("re-read cert"),
            b"not a certificate",
            "a receiver that cannot start may not delete someone's certificate"
        );
        assert_eq!(
            std::fs::read(dir.join(CAST_KEY_FILE)).expect("re-read key"),
            b"not a key"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_file_holding_nothing_names_the_key_file() {
        crate::pin_test_dir();
        let dir = dir_for("nokey");
        let pair = ensure_pair(&dir, &log("nokey")).expect("generation");
        // Truncate only the key: the certificate is valid, so the two files are
        // not "both garbage" and the answer must not be about the certificate.
        std::fs::write(&pair.key, b"").expect("truncate key");
        let error = match pair.server_config() {
            Ok(_) => panic!("an empty key file cannot work"),
            Err(e) => e,
        };
        assert!(
            matches!(&error, CertError::NoKey(path) if *path == pair.key),
            "the refusal must name the key file: {error:?}"
        );
        assert!(error.message().contains(SHARED_KEY_FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generating_logs_exactly_one_line_and_adoption_logs_none() {
        crate::pin_test_dir();
        let dir = dir_for("logline");
        // The needle is this run's own path, so a parallel test writing about a
        // certificate cannot be counted as this one, and a log that outlived the
        // run would not contain it either.
        let needle = dir.join(SHARED_CERT_FILE).display().to_string();

        ensure_pair(&dir, &log("logline")).expect("generation");
        let after_generation = log_tail();
        let hits = after_generation.lines().filter(|line| line.contains(&needle)).count();
        assert_eq!(
            hits, 1,
            "one INFO line per generated pair, exactly like Python; saw {hits} in {after_generation}"
        );
        assert!(
            after_generation.contains(&format!("Generated self-signed certificate: {needle}")),
            "the line must be Python's wording, not a paraphrase: {after_generation}"
        );

        // The adoption branch is silent in Python (`_ensure_cert` just reassigns
        // two attributes), so "still exactly one" is what proves the second call
        // did not add one.
        ensure_pair(&dir, &log("logline2")).expect("adoption");
        let hits_after_adoption = log_tail()
            .lines()
            .filter(|line| line.contains(&needle))
            .count();
        assert_eq!(hits_after_adoption, 1, "adoption must not log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    impl Pair {
        fn with_source(&self, source: Source) -> Pair {
            Pair {
                cert: self.cert.clone(),
                key: self.key.clone(),
                source,
            }
        }
    }
}

/// A number no other test in this process asks for, so per-test temp paths
/// cannot collide when `cargo test` runs them on different threads.
#[cfg(test)]
pub(crate) fn unique_slot() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SLOT: AtomicU32 = AtomicU32::new(0);
    SLOT.fetch_add(1, Ordering::SeqCst)
}
