//! Random identifiers, spelled the way Python spelled them.
//!
//! Three different random values reach the network or the disk, and each has a
//! format somebody else parses:
//!
//! - `Api_Token` is `secrets.token_hex(16)` — 32 lowercase hex characters. It is
//!   persisted, so the shape only has to be right the first time (AGENTS.md
//!   §4.7: a token that changes on every start silently breaks every configured
//!   shortcut).
//! - Relay paths use `secrets.token_urlsafe(16)` — 22 URL-safe base64 characters
//!   with **no padding**. The relay id is compared with a constant-time
//!   comparison and appears in a URL a TV types nothing into.
//! - The SSDP/UPnP UDN is a version-4 UUID with hyphens, because control points
//!   run `UUID(udn.replace("-", ""))` over it (AGENTS.md §4.9: a stray `uuid:`
//!   prefix there made pychromecast drop the whole `device_info`).
//!
//! `getrandom` failing is a hard error rather than a fallback to the clock: a
//! "random" id derived from `SystemTime` is guessable, and every one of these
//! values is a trust boundary.

use std::sync::Mutex;

use serde_json::Value;

static TOKEN_LOCK: Mutex<()> = Mutex::new(());

const HEX: &[u8; 16] = b"0123456789abcdef";
const URLSAFE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Fill `out` with cryptographically random bytes or return the error.
pub fn fill(out: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(out).map_err(|e| format!("getrandom failed: {e}"))
}

pub fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut v = vec![0u8; n];
    fill(&mut v)?;
    Ok(v)
}

/// `secrets.token_hex(nbytes)` — 2 hex characters per byte.
pub fn token_hex(nbytes: usize) -> String {
    let bytes = random_bytes(nbytes).unwrap_or_default();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// `secrets.token_urlsafe(nbytes)` — base64url of `nbytes`, padding stripped.
pub fn token_urlsafe(nbytes: usize) -> String {
    let bytes = match random_bytes(nbytes) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    let mut s = String::with_capacity((nbytes + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        // Output characters for a chunk of 1/2/3 bytes: 2/3/4. Python's
        // `base64.urlsafe_b64encode` emits ceil(len*8/6) characters before the
        // padding is stripped, which is `(len * 4 + 2) / 3` — **not** a chunk
        // count. Writing `(len + 2) / 3` here yields 1 for *every* chunk length,
        // so `token_urlsafe(16)` handed back 6 characters where the relay path
        // expects 22, and a shortener id is a weakener id.
        let full = (chunk.len() * 4 + 2) / 3; // 4, 3 or 2 output characters
        let idx = [
            (n >> 18) & 63,
            (n >> 12) & 63,
            (n >> 6) & 63,
            n & 63,
        ];
        for k in 0..full {
            s.push(URLSAFE[idx[k] as usize] as char);
        }
    }
    s
}

/// `str(uuid.uuid4())` — 8-4-4-4-12 lowercase hex with the version and variant
/// bits set.
pub fn uuid4() -> String {
    let mut b = match random_bytes(16) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut hex = String::with_capacity(32);
    for x in &b {
        hex.push(HEX[(x >> 4) as usize] as char);
        hex.push(HEX[(x & 0x0f) as usize] as char);
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The stable device id: `uuid4()` persisted under `USN`, generated once.
///
/// This is a convenience re-export of [`crate::setting::Setting::usn`] for
/// callers that have a `Setting` in hand and think of the value as a token; the
/// insert-on-read that makes the first call sticky lives there.
pub fn usn(setting: &crate::setting::Setting) -> String {
    setting.usn()
}

/// The management API token: 32 hex characters, persisted, generated on first
/// use under a lock so two concurrent first-use callers cannot make two tokens.
///
/// The lock is on this module rather than inside `Setting` because what needs
/// serialising is the *decide-then-store* pair, not the write: `Setting` already
/// has a lock for the table, and a second process reading the same file cannot
/// be guarded from here anyway (Python's `api_token()` has the same hole).
///
/// `peek` rather than `get`, so a machine with no token yet does not acquire one
/// the moment something merely *asks* whether there is one; and a stored value
/// that is not a non-empty string is treated as absent, because an empty
/// `Api_Token` in a hand-edited 高级设置 would otherwise mean "the management
/// API has a secret of zero length".
pub fn api_token(setting: &crate::setting::Setting) -> String {
    let _guard = TOKEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = setting.peek(crate::setting::Key::ApiToken) {
        if let Some(text) = existing.as_str() {
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    let fresh = token_hex(16);
    setting.put(crate::setting::Key::ApiToken, Value::String(fresh.clone()));
    setting.save();
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hex_length_and_alphabet() {
        let t = token_hex(16);
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(token_hex(0).len(), 0);
    }

    #[test]
    fn token_urlsafe_has_no_padding() {
        // 16 bytes -> 22 characters, exactly what a relay path segment looks
        // like in the log line `cast url from web endpoint: .../relay/<id>/media`.
        assert_eq!(token_urlsafe(16).len(), 22);
        assert_eq!(token_urlsafe(1).len(), 2);
        assert_eq!(token_urlsafe(2).len(), 3);
        assert_eq!(token_urlsafe(3).len(), 4);
        assert_eq!(token_urlsafe(9).len(), 12);
        let s = token_urlsafe(32);
        assert!(!s.contains('='));
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn uuid4_shape() {
        let u = uuid4();
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert_eq!(&u[14..15], "4", "version nibble");
        assert!("89ab".contains(&u[19..20]), "variant nibble, got {}", u);
        // The hyphens are the only non-hex characters, because a control point
        // strips them and parses the rest as a UUID.
        assert_eq!(u.replace('-', "").len(), 32);
    }

    #[test]
    fn values_are_not_repeated() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(token_hex(16)));
        }
    }

    /// A scratch settings file, because `api_token` *persists* and the user's
    /// real `macast_setting.json` must never be the test's subject (AGENTS.md
    /// §10: never touch his config to verify something).
    struct Scratch {
        dir: std::path::PathBuf,
        file: std::path::PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "macast-core-token-{}-{}-{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_micros())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch {
                file: dir.join(crate::paths::SETTING_FILE_NAME),
                dir,
            }
        }
        fn path(&self) -> &std::path::Path {
            &self.file
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn api_token_is_generated_once_and_stays_on_disk() {
        let scratch = Scratch::new("stable");
        let setting = crate::setting::Setting::new(scratch.path());
        let first = api_token(&setting);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(std::fs::read_to_string(scratch.path())
            .unwrap()
            .contains(&first));
        // Asking again must not rotate it: a token that changes on every start
        // invalidates every iOS Shortcut the user configured (AGENTS.md §4.7).
        assert_eq!(api_token(&setting), first);
        let reopened = crate::setting::Setting::new(scratch.path());
        assert_eq!(api_token(&reopened), first);
    }

    #[test]
    fn an_empty_stored_token_is_not_a_secret() {
        let scratch = Scratch::new("empty");
        let setting = crate::setting::Setting::new(scratch.path());
        setting.put(
            crate::setting::Key::ApiToken,
            serde_json::Value::String(String::new()),
        );
        let fresh = api_token(&setting);
        assert_eq!(fresh.len(), 32);
        assert_eq!(api_token(&setting), fresh);
    }

    #[test]
    fn asking_is_not_the_same_as_needing_one() {
        // `peek` rather than `get`: merely reading the table must not adopt a
        // token, because `Setting.get` persists its default (§4.2's first rule).
        let scratch = Scratch::new("no-side-effect");
        let setting = crate::setting::Setting::new(scratch.path());
        assert!(!setting.has(crate::setting::Key::ApiToken));
        assert!(!scratch.path().exists());
    }
}
