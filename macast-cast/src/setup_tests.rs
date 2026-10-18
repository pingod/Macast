//! Witnesses for [`crate::setup`] — the face a sender reads **before** it casts.
//!
//! The reference for every expectation here is `macast/protocol_cast.py`'s
//! `_make_setup_handler`, and where a shape is only justified by what the *client*
//! does — pychromecast 14.0.15 in this repository's `.venv` — the file and line are
//! named in the test, because that is the half a stub cannot supply (the same rule
//! `docs/Rust-Port-Design.md` §13.3 writes for the acceptance script: a打桩 test can
//! prove self-consistency, not that a sender认账).
//!
//! Two habits carried in from `targets_tests`, both earned the hard way there:
//!
//! * every test opens with `crate::pin_test_dir()`, because a `cfg(test)` guard in
//!   `macast-core` does not protect a dependent crate's test binary;
//! * a silence assertion is phrased **by level** ([`loud`]), never by emptiness —
//!   the log threshold is one process-wide `AtomicU8`, and `DebugWindow` holders
//!   decide whether a file is empty if the test lets them.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use macast_core::logging::{read_tail, Logger};
use macast_core::paths;
use macast_core::setting::Setting;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn setting(dir_name: &str) -> Arc<Setting> {
    Arc::new(Setting::new(
        macast_core::paths::setting_dir().join(format!("setup-{dir_name}.json")),
    ))
}

fn witness(name: &str) -> (Arc<Logger>, std::path::PathBuf) {
    let logger = Logger::named(name);
    let path = logger.claim();
    (logger, path)
}

fn lines(path: &std::path::Path) -> String {
    read_tail(path, 200).text
}

/// Records at INFO or above. See `targets_tests::loud` for why silence is
/// asserted this way in this crate: a neighbour's open DEBUG window must not be
/// able to decide whether a witness file "said nothing".
fn loud(path: &std::path::Path) -> String {
    lines(path)
        .lines()
        .filter(|record| !record.contains("[DEBUG]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The top-level key order of a document, as the sender will read it.
fn keys(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

/// One GET on a real socket, returning the whole response as bytes so the test
/// can assert on the header block itself rather than on a re-parsed version of it.
fn get(port: u16, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback is available");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(format!("{request}\r\n\r\n").as_bytes())
        .expect("the request goes out");
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).expect("the answer is read");
    buffer
}

/// Split a response into `(status line, headers, body)`.
fn split(response: &[u8]) -> (String, Vec<String>, Vec<u8>) {
    let text = String::from_utf8_lossy(response).to_string();
    let head_end = text.find("\r\n\r\n").expect("the header block ends");
    let (head, rest) = text.split_at(head_end);
    let mut head_lines = head.split("\r\n");
    let status = head_lines.next().unwrap_or("").to_string();
    let headers = head_lines.map(str::to_string).collect();
    (status, headers, rest[4..].as_bytes().to_vec())
}

// ---------------------------------------------------------------------------
// A: the document, in the order a sender reads it
// ---------------------------------------------------------------------------

#[test]
fn a_the_top_level_keys_are_the_reference_s_order() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("a-order"), "");
    assert_eq!(
        keys(&doc),
        vec![
            "name",
            "device_info",
            "ssdp_udn",
            "version",
            "build_version",
            "connected",
            "settings",
        ],
        "the seven keys `_eureka_info` writes, in `protocol_cast.py:1265-1275` order"
    );
}

#[test]
fn a_device_info_is_nested_and_not_flattened() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("a-nested"), "");
    assert!(
        doc.get("device_info").map(Value::is_object).unwrap_or(false),
        "a flat answer is what strict clients reject: {doc}"
    );
    assert_eq!(
        keys(&doc["device_info"]),
        vec![
            "manufacturer",
            "model_name",
            "product_name",
            "friendly_name",
            "ssdp_udn",
            "mac_address",
            "capabilities",
        ]
    );
    // And the flattening nobody should ever try again: no dotted key sneaking in.
    assert!(doc.get("device_info.manufacturer").is_none());
}

#[test]
fn a_capabilities_spells_multizone_supported_false_and_last() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("a-multizone"), "");
    let capabilities = &doc["device_info"]["capabilities"];
    assert_eq!(capabilities["multizone_supported"], json!(false));
    assert_eq!(
        keys(capabilities),
        vec![
            "display_supported",
            "audio_supported",
            "video_out",
            "audio_out",
            "multizone_supported"
        ],
        "the explicit `false` is the last key, exactly where Python writes it"
    );
    // Why this row exists: pychromecast reads
    // `capabilities.get("multizone_supported", True)` whenever `device_info` is
    // present (`dial.py:163-165`), so *omitting* the key claims multi-room and
    // buys a `?params=multizone` probe on every discovery, forever. The absence
    // of the key is the bug, not a smaller answer.
    let mut stripped = capabilities.as_object().unwrap().clone();
    stripped.remove("multizone_supported");
    assert!(stripped.get("multizone_supported").is_none());
    assert_eq!(
        stripped.len(),
        4,
        "removing the key must leave four, which is what the default-true read sees"
    );
}

#[test]
fn a_the_udn_is_a_bare_uuid_at_both_levels_and_the_same_string() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("a-udn"), "");
    let top = doc["ssdp_udn"].as_str().unwrap();
    let nested = doc["device_info"]["ssdp_udn"].as_str().unwrap();
    assert_eq!(top, nested, "Python reads `Setting.get_usn()` once for both");
    assert!(!top.starts_with("uuid:"), "prefixed: {top}");
    // `UUID(udn.replace("-", ""))` (`dial.py`) answers None for the whole
    // `device_info` on anything it cannot parse, so the shape is worth naming:
    // 8-4-4-4-12 hex digits.
    let groups: Vec<usize> = top.split('-').map(str::len).collect();
    assert_eq!(groups, vec![8, 4, 4, 4, 12], "udn {top:?}");
    assert!(top
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == '-'));
}

#[test]
fn a_version_and_build_version_are_the_same_value_and_the_server_header_agrees() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("a-version"), "");
    assert_eq!(doc["version"], doc["build_version"]);
    assert_eq!(
        doc["version"].as_str().unwrap(),
        paths::version(),
        "`Setting.version` in Python is the app version, and so is this"
    );
    let response = answer(&setting("a-version"), "GET", EUREKA_PATH);
    let server = response
        .headers
        .iter()
        .find(|(name, _)| name == "Server")
        .map(|(_, value)| value.clone())
        .expect("a Server header");
    assert_eq!(server, format!("Macast/{}", paths::version()));
    // The divergence, asserted rather than narrated: we do not claim to be CPython.
    assert!(!server.contains("Python"), "Server said {server:?}");
}

#[test]
fn a_the_friendly_name_reaches_both_places_it_is_written() {
    crate::pin_test_dir();
    let store = setting("a-name");
    store.set_temp_friendly_name(Some("客厅电视"));
    let doc = eureka_info(&store, "");
    assert_eq!(doc["name"], json!("客厅电视"));
    assert_eq!(doc["device_info"]["friendly_name"], json!("客厅电视"));
    store.set_temp_friendly_name(None);
}

// ---------------------------------------------------------------------------
// B: the `params=` filter
// ---------------------------------------------------------------------------

#[test]
fn b_pychromecast_s_own_literal_request_answers_in_the_document_s_order() {
    crate::pin_test_dir();
    // `dial.py:157` asks `params=device_info,name` — the caller's order — and the
    // reference's dict comprehension answers in the **document's** order.
    // A reader who sorted by request, or built the answer by walking `wanted`,
    // would produce `device_info` first and still parse fine.
    let doc = eureka_info(&setting("b-order"), "params=device_info,name");
    assert_eq!(keys(&doc), vec!["name", "device_info"]);
}

#[test]
fn b_an_unknown_name_answers_an_empty_object_not_an_error() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("b-empty"), "params=multizone");
    assert!(doc.as_object().unwrap().is_empty(), "{doc}");
    // `get_multizone_status` then reads `"multizone" in status` (`dial.py:317`),
    // which is False on this object — the honest "no", and the reason the
    // `multizone_supported` key above is what keeps this probe from happening.
    let response = answer(&setting("b-empty"), "GET", "/setup/eureka_info?params=multizone");
    assert_eq!(response.status, 200, "an empty object is still a 200");
}

#[test]
fn b_blank_params_is_no_filter_at_all() {
    crate::pin_test_dir();
    // One store, asked twice: `usn()` inserts on read, so a second `setting()`
    // would answer with a *different* UUID and the comparison would fail for a
    // reason that has nothing to do with the filter. (This test found that by
    // being red, and the first reading — "the filter is broken" — was wrong.)
    let store = setting("b-blank");
    let full = eureka_info(&store, "");
    let asked = eureka_info(&store, "params=");
    assert_eq!(asked, full, "`parse_qs` drops the blank, so nothing was asked");
    // And the same blank in front of a real name: the blank is dropped **first**,
    // so the second occurrence becomes the answer.
    let one = eureka_info(&store, "params=&params=name");
    assert_eq!(keys(&one), vec!["name"]);
}

#[test]
fn b_an_encoded_comma_is_two_names() {
    crate::pin_test_dir();
    let store = setting("b-comma");
    let doc = eureka_info(&store, "params=name%2Cversion");
    assert_eq!(keys(&doc), vec!["name", "version"]);
    // The unencoded form answers the same question, which is the point: the split
    // happens **after** decoding, and a filter that split first would return
    // `{}` for this request.
    let plain = eureka_info(&store, "params=name,version");
    assert_eq!(doc, plain);
}

#[test]
fn b_a_plus_is_a_space_and_a_space_is_not_a_name() {
    crate::pin_test_dir();
    // `parse_qs` keeps `" "` (a space is not blank), and `_eureka_info` then runs
    // `if p.strip()` — which is why the strip lives in `wanted_params` and not
    // inside the shared query reader.
    let store = setting("b-plus");
    let full = eureka_info(&store, "");
    assert_eq!(eureka_info(&store, "params=+"), full);
    assert_eq!(eureka_info(&store, "params=%20"), full);
}

#[test]
fn b_whitespace_around_names_is_stripped() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("b-strip"), "params=%20name%20,%20version");
    assert_eq!(keys(&doc), vec!["name", "version"]);
    // Empty items between commas are dropped rather than becoming a lookup of "":
    // `params=,,name,` is one name.
    let one = eureka_info(&setting("b-strip"), "params=,,name,");
    assert_eq!(keys(&one), vec!["name"]);
}

#[test]
fn b_two_occurrences_are_one_filter_and_it_is_the_first() {
    crate::pin_test_dir();
    let doc = eureka_info(&setting("b-dup"), "params=name&params=version");
    assert_eq!(keys(&doc), vec!["name"], "`params[0]` is the first, not the last");
}

#[test]
fn b_the_query_is_decoded_once_not_twice() {
    crate::pin_test_dir();
    // `%25name` decodes to the literal `%name`, which is not a document key, so
    // the answer is `{}`. A reader that decoded twice would answer `name` — and
    // that second pass is exactly what `url.rs`'s byte-split rule forbids.
    let doc = eureka_info(&setting("b-once"), "params=%25name");
    assert!(doc.as_object().unwrap().is_empty(), "{doc}");
}

// ---------------------------------------------------------------------------
// C: routing, as a decision over bytes
// ---------------------------------------------------------------------------

#[test]
fn c_a_prefix_match_is_enough_and_a_longer_path_is_a_different_route() {
    crate::pin_test_dir();
    let store = setting("c-prefix");
    for target in [EUREKA_PATH, "/setup/eureka_info/", "/setup/eureka_info?params=name"] {
        assert_eq!(
            answer(&store, "GET", target).status,
            200,
            "startswith, like `protocol_cast.py:1281`: {target}"
        );
    }
    // A sibling path is **not** this route: prefix matching goes the other way
    // too, and `/setup/eureka_inf` must not be answered as the document.
    assert_eq!(answer(&store, "GET", "/setup/eureka_inf").status, 404);
}

#[test]
fn c_the_icon_declares_a_type_and_no_length() {
    crate::pin_test_dir();
    let response = answer(&setting("c-icon"), "GET", ICON_PATH);
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers,
        vec![
            (
                "Server".to_string(),
                format!("Macast/{}", paths::version())
            ),
            ("Content-Type".to_string(), "image/png".to_string()),
        ],
        "Server, then Content-Type, then nothing — and no Content-Length at all"
    );
    assert!(response.body.is_empty());
}

#[test]
fn c_the_document_s_length_is_bytes_not_characters() {
    crate::pin_test_dir();
    // AGENTS.md §4.8's SOAP red line, in its HTTP form: `len(str)` truncates a
    // CJK name. The fixture is the name that would break a character-count
    // header, and the assertion is on the number the wire has to carry.
    let store = setting("c-length");
    store.set_temp_friendly_name(Some("客厅电视 · Chromecast"));
    let response = answer(&store, "GET", EUREKA_PATH);
    let declared = response
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Length")
        .map(|(_, value)| value.parse::<usize>().unwrap())
        .expect("a length for a body");
    assert_eq!(declared, response.body.len());
    // The trap this exists to fail on: for a name whose bytes outnumber its
    // characters, a `len(str)` header is a *shorter* promise than the body, and
    // the sender truncates the document instead of reporting an error.
    let text = String::from_utf8(response.body.clone()).expect("we write UTF-8");
    assert!(
        text.len() > text.chars().count(),
        "the fixture no longer separates bytes from characters, so this row proves nothing"
    );
    assert_ne!(declared, text.chars().count());
    store.set_temp_friendly_name(None);
}

#[test]
fn c_anything_else_is_a_404_with_no_body_and_the_path_is_never_decoded() {
    crate::pin_test_dir();
    let store = setting("c-404");
    let response = answer(&store, "GET", "/setup/nothing");
    assert_eq!((response.status, response.body.len()), (404, 0));
    // `%65` is an `e`, and a path decoder would turn this into the eureka route.
    // The DLNA face refuses the same trick for the same reason (its
    // `Request::segments` doc names `%2E`), and one rule needs one owner's
    // behaviour, not two crates' opinions.
    assert_eq!(answer(&store, "GET", "/setup/%65ureka_info").status, 404);
}

#[test]
fn c_a_verb_that_is_not_get_is_501_with_no_body() {
    crate::pin_test_dir();
    let store = setting("c-verb");
    for method in ["POST", "HEAD", "PUT", "DELETE"] {
        let response = answer(&store, method, EUREKA_PATH);
        assert_eq!((response.status, response.body.len()), (501, 0), "{method}");
    }
    // Deviation, recorded rather than hidden: Python's handler answers 501 too,
    // with a generated `text/html` error page. Ours carries no body — see the
    // module header.
    assert!(answer(&store, "POST", EUREKA_PATH).body.is_empty());
}

// ---------------------------------------------------------------------------
// D: the plain face on a real socket
// ---------------------------------------------------------------------------

#[test]
fn d_a_real_loopback_request_gets_the_document_and_the_log_gains_nothing() {
    crate::pin_test_dir();
    let (log, path) = witness("setup-d-live");
    let store = setting("d-live");
    let mut server =
        SetupServer::start_on(store, log, None, 0, 1).expect("an ephemeral port is available");
    // **Delta**, not emptiness: `start_on` legitimately writes one INFO line
    // (there was no certificate to offer for 8443), and asserting an empty file
    // would be asserting that the face never speaks about its own setup — which
    // is not what "serving a probe is not an event" means.
    let before = loud(&path);
    let response = get(server.port, &format!("GET {EUREKA_PATH} HTTP/1.1"));
    let (status, headers, body) = split(&response);
    assert_eq!(status, "HTTP/1.0 200 OK", "no keep-alive: the reference is HTTP/1.0");
    assert!(
        headers
            .iter()
            .any(|line| line.starts_with("Content-Type: application/json")),
        "{headers:?}"
    );
    let doc: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON on the wire");
    assert_eq!(doc["device_info"]["manufacturer"], json!("Macast"));
    // And the length we declared is the length that arrived: a header/body
    // mismatch is how `urllib` hangs instead of failing.
    let declared = headers
        .iter()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .expect("a declared length");
    assert_eq!(declared.parse::<usize>().unwrap(), body.len());
    server.shutdown();
    assert_eq!(loud(&path), before, "a answered probe wrote a log line");
}

#[test]
fn d_the_second_connection_is_answered_and_the_first_is_closed() {
    crate::pin_test_dir();
    let (log, _path) = witness("setup-d-twice");
    let server = SetupServer::start_on(setting("d-twice"), log, None, 0, 1).unwrap();
    let first = get(server.port, "GET /setup/icon.png HTTP/1.0");
    let second = get(server.port, &format!("GET {EUREKA_PATH} HTTP/1.0"));
    assert_eq!(split(&first).0, "HTTP/1.0 200 OK");
    // The icon's answer is headers only, so `read_to_end` returning here is the
    // witness that the face closes the connection instead of waiting for the
    // client to give up. A keep-alive face would leave this read hanging until the
    // timeout — a different symptom and the same wrong protocol version.
    assert!(split(&first).2.is_empty());
    assert_eq!(split(&second).0, "HTTP/1.0 200 OK");
}

#[test]
fn d_a_client_that_opens_the_socket_and_says_nothing_is_not_owed_a_thread() {
    crate::pin_test_dir();
    let (log, _path) = witness("setup-d-silent");
    let server = SetupServer::start_on(setting("d-silent"), log, None, 0, 1).unwrap();
    let parked = TcpStream::connect(("127.0.0.1", server.port)).expect("the listener is up");
    // Then a real request on a second connection: if the silent client could hold
    // the face, this is the call that hangs.
    let response = get(server.port, &format!("GET {EUREKA_PATH} HTTP/1.0"));
    assert_eq!(split(&response).0, "HTTP/1.0 200 OK");
    drop(parked);
}

#[test]
fn d_shutdown_takes_the_port_away_rather_than_leaving_it_deaf() {
    crate::pin_test_dir();
    let (log, path) = witness("setup-d-stop");
    let mut server = SetupServer::start_on(setting("d-stop"), log, None, 0, 1).unwrap();
    let port = server.port;
    let before = loud(&path);
    assert_eq!(split(&get(port, "GET /setup/nothing HTTP/1.0")).0, "HTTP/1.0 404 Not Found");
    server.shutdown();
    // **Wait for the fact**, do not grab a moment (AGENTS.md §4.2): the accept
    // loop notices `running` on its next poll, so a refusal is not instantaneous,
    // and a test that reads immediately would pass or fail on scheduling.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            break;
        }
        assert!(Instant::now() < deadline, "the listener outlived its shutdown");
        std::thread::sleep(ACCEPT_POLL);
    }
    // Stopping cleanly is not an event either: the reference's `shutdown()` is
    // silent, and a WARNING per service stop would be the log filling with a
    // sentence that means "the user closed the window".
    assert_eq!(loud(&path), before, "{}", lines(&path));
}

#[test]
fn d_the_bound_port_is_the_one_the_listener_actually_has() {
    crate::pin_test_dir();
    let (log, _path) = witness("setup-d-port");
    let server = SetupServer::start_on(setting("d-port"), log, None, 0, 1).unwrap();
    // The seam asks for `0`; reporting the *requested* number instead of the
    // bound one would hand every caller above this a port with nothing on it,
    // which is how a `?refresh=1`-style answer becomes a connection refused.
    assert_ne!(server.port, 0);
    assert_eq!(server.https_port, None, "no certificate was handed over");
    assert_eq!(split(&get(server.port, "GET / HTTP/1.0")).0, "HTTP/1.0 404 Not Found");
}

#[test]
fn d_no_certificate_skips_the_tls_face_and_says_which_reason_it_was() {
    crate::pin_test_dir();
    let (log, path) = witness("setup-d-nocert");
    let server = SetupServer::start_on(setting("d-nocert"), log, None, 0, 1).unwrap();
    assert_eq!(server.https_port, None);
    let text = lines(&path);
    assert!(
        text.contains("No TLS certificate available, skipping the Cast HTTPS setup server on 8443"),
        "{text}"
    );
    // The reference logs that at INFO because a machine without a cert is a
    // machine where 8443 *should* not be there; it is not a warning, and a
    // warning would be the log filling with a condition that is normal.
    assert!(text.contains("[INFO]"), "{text}");
}

// ---------------------------------------------------------------------------
// E: the TLS face on a real socket
// ---------------------------------------------------------------------------

/// One GET over a real TLS session, trusting the pair we generated ourselves.
///
/// The trust anchor is ours rather than a dangerous verifier because
/// `cert::ensure_pair` puts `localhost` in the SAN — so a successful handshake is
/// also the witness for that, which the Cast socket's own tests rely on too.
/// `dial.py:133-134` does *not* check (`CERT_NONE`), so a real sender is more
/// forgiving than this client, not less.
///
/// The `read_to_end` at the end is the close_notify witness: a server that drops
/// the session instead of shutting it down makes rustls raise `UnexpectedEof`
/// here even though the bytes arrived. That is how this file caught the missing
/// `send_close_notify` in `serve_tls` — the plain-TCP tests cannot see it.
fn tls_get(port: u16, pair: &crate::cert::Pair, request: &str) -> Vec<u8> {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("the TLS listener is up");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for certificate in crate::cert::read_certs(&pair.cert).expect("read our own certificate") {
        roots.add(certificate.clone()).unwrap();
    }
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(client_config), name).unwrap();
    let mut tls = rustls::StreamOwned::new(conn, stream);
    tls.write_all(format!("{request}\r\n\r\n").as_bytes())
        .expect("the request goes out over TLS");
    let mut response = Vec::new();
    tls.read_to_end(&mut response)
        .expect("the answer comes back over TLS, cleanly closed");
    response
}

/// A test key pair in its own directory, so no test can adopt another's.
fn test_pair(name: &str) -> (Arc<Logger>, crate::cert::Pair) {
    let dir = std::env::temp_dir().join(format!("macast-setup-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (log, _path) = witness(&format!("setup-{name}-log"));
    let pair = crate::cert::ensure_pair(&dir, &log).expect("a test key pair");
    (log, pair)
}

// ---------------------------------------------------------------------------
// E: the TLS face on a real socket
// ---------------------------------------------------------------------------

#[test]
fn e_a_real_tls_handshake_answers_the_document() {
    crate::pin_test_dir();
    let (log, pair) = test_pair("e-live");
    let config = pair.server_config().expect("a server config from that pair");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    accept_loop(
        listener,
        setting("e-live"),
        Arc::clone(&log),
        Some(config),
        Arc::clone(&running),
    );

    let response = tls_get(port, &pair, &format!("GET {EUREKA_PATH} HTTP/1.0"));
    let (status, headers, body) = split(&response);
    assert_eq!(status, "HTTP/1.0 200 OK");
    assert!(headers.iter().any(|line| line.starts_with("Content-Type: application/json")));
    let doc: serde_json::Value = serde_json::from_slice(&body).expect("JSON over TLS");
    assert_eq!(doc["device_info"]["model_name"], json!("Macast"));
    // The same filter rule the plain face answers, over the encrypted twin —
    // because pychromecast asks the **filtered** form on 8443 and only that.
    let filtered = tls_get(port, &pair, "GET /setup/eureka_info?params=device_info,name HTTP/1.0");
    let filtered: serde_json::Value = serde_json::from_slice(&split(&filtered).2).unwrap();
    assert_eq!(keys(&filtered), vec!["name", "device_info"]);
    running.store(false, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn e_an_aborted_handshake_does_not_take_the_face_off_the_network() {
    crate::pin_test_dir();
    // `AGENTS.md` §4.1's Chromecast row is the whole reason this test exists: one
    // failed `SSLError` once killed the accept loop, and the device stayed
    // advertised while nothing answered. Same shape, this listener.
    let (log, pair) = test_pair("e-abort");
    let config = pair.server_config().expect("a server config");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    accept_loop(
        listener,
        setting("e-abort"),
        Arc::clone(&log),
        Some(config),
        Arc::clone(&running),
    );

    // Garbage on a socket that claims to be TLS: the handshake must fail, quietly,
    // and the next client must still get an answer.
    let mut junk = TcpStream::connect(("127.0.0.1", port)).unwrap();
    junk.write_all(b"\x16\x03\x01\x00\x00\x00\x00").unwrap();
    junk.flush().unwrap();
    drop(junk);

    // The follow-up probe is a **TLS** client, not a plaintext one: this face
    // speaks TLS and nothing else, so the earlier version of this test waited for
    // an answer it had no way to receive and reported "the face died" — a red that
    // was the harness's own wrong question, not the product's.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ok = std::panic::catch_unwind(|| {
            tls_get(port, &pair, "GET /setup/icon.png HTTP/1.0").starts_with(b"HTTP/1.0 200")
        })
        .unwrap_or(false);
        if ok {
            break;
        }
        assert!(Instant::now() < deadline, "the face died with one bad handshake");
        std::thread::sleep(ACCEPT_POLL);
    }
    running.store(false, std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// F: what the face must never do
// ---------------------------------------------------------------------------

#[test]
fn d_a_taken_port_is_not_a_license_to_bind_the_cast_socket_s_neighbour() {
    crate::pin_test_dir();
    // The module header's divergence, asserted: a face that cannot bind the one
    // port senders knock on binds **nothing**, rather than walking the
    // reference's range and landing on a number nobody will ever ask — or, on a
    // shell that has not started its Cast channel yet, taking 8009 away from it.
    // Squat the way the face binds — a wildcard, not loopback. Measured the other
    // way first: holding `127.0.0.1:P` let `0.0.0.0:P` bind anyway, so the
    // "refusal" this test is supposed to prove never happened. Whatever the
    // mechanism is on this OS, a fixture the production path could not meet is not
    // a test of the refusal, so the holder is a wildcard now.
    let (log, path) = witness("setup-d-busy");
    let squatter = TcpListener::bind(("0.0.0.0", 0)).expect("an ephemeral port to squat");
    let taken = squatter.local_addr().unwrap().port();
    // The per-port line is a DEBUG record, and this crate's threshold is INFO by
    // default — so it is *not* in the file unless a DEBUG window is held while
    // the attempt happens. Found by writing the assertion first and watching it
    // fail against a correct product: the third time this round that the witness
    // level, not the write, decides what a log file can prove.
    let window = crate::DebugWindow::armed();
    let started = SetupServer::start_on(setting("d-busy"), log, None, taken, 1);
    let error = match started {
        Ok(server) => panic!(
            "bound port {} while a live listener holds {}",
            server.port, taken
        ),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    drop(window);
    assert!(
        lines(&path).contains(&format!("bind port {taken} failed")),
        "the log must name the port it tried: {}",
        lines(&path)
    );
    assert!(
        loud(&path).contains("no free port in range"),
        "and the loud half must say it bound nothing: {}",
        loud(&path)
    );
    drop(squatter);
}

#[test]
fn f_the_face_says_what_the_device_is_and_never_what_to_play() {
    crate::pin_test_dir();
    // The security statement of this module, asserted: there is no verb and no
    // credential in any answer on either port. `answer()` cannot express a
    // mutation, so the check is that no request line names a field the
    // management face owns.
    let store = setting("f-safe");
    for target in [
        EUREKA_PATH,
        ICON_PATH,
        "/setup/eureka_info?params=settings",
        "/setup/eureka_info?params=connected",
    ] {
        let response = answer(&store, "GET", target);
        let body = String::from_utf8_lossy(&response.body).to_string();
        for forbidden in ["Api_Token", "token", "cookies", "resolve_cookies", "set-media"] {
            assert!(
                !body.contains(forbidden),
                "the setup document leaked {forbidden:?}: {body}"
            );
        }
    }
    // And the only writable verb the face knows is the one it refuses outright.
    assert_eq!(answer(&store, "POST", EUREKA_PATH).status, 501);
}

#[test]
fn f_the_unfiltered_document_never_names_a_path_on_this_machine() {
    crate::pin_test_dir();
    // §4.14's cookie rule reaches here: this face has **no** gate, so anything it
    // prints is printed to any host on the subnet. The config directory, a
    // certificate path, or a cookie jar filename would each be a leak with no
    // token involved.
    let store = setting("f-path");
    let body = serde_json::to_string(&eureka_info(&store, "")).unwrap();
    let dir = paths::setting_dir().display().to_string();
    assert!(!body.contains(&dir), "the document contains {dir}");
    assert!(!body.contains(".pem") && !body.contains(".key"), "{body}");
    assert!(!body.contains("json"), "{body}");
}
