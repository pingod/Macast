//! Differential tests for the relay layer.
//!
//! Every test that opens a socket uses a real loopback listener; no mocks.
//! Fake-ffmpeg tests use a shell script on the PATH so the relay believes it
//! succeeded and writes a real file.

use super::*;
use std::net::TcpStream;
use std::sync::atomic::Ordering;

// ---------------------------------------------------------------------------
// parse_range
// ---------------------------------------------------------------------------

#[test]
fn parse_range_none_is_whole_file() {
    crate::pin_test_dir();
    assert_eq!(parse_range(None), Some((Some(0), None)));
    assert_eq!(parse_range(Some("")), Some((Some(0), None)));
}

#[test]
fn parse_range_normal_forms() {
    crate::pin_test_dir();
    assert_eq!(parse_range(Some("bytes=0-99")),  Some((Some(0), Some(99))));
    assert_eq!(parse_range(Some("bytes=100-")),  Some((Some(100), None)));
    assert_eq!(parse_range(Some("bytes=0-")),    Some((Some(0), None)));
}

#[test]
fn parse_range_junk_returns_none() {
    crate::pin_test_dir();
    assert_eq!(parse_range(Some("kilobytes=0-9")), None);
    assert_eq!(parse_range(Some("bytes=9-0")),     None);  // stop < start
    assert_eq!(parse_range(Some("bytes=abc")),     None);
    assert_eq!(parse_range(Some("bytes=0-9,10-19")), None); // multi-range
}

#[test]
fn parse_range_suffix_keeps_its_own_shape() {
    // `bytes=-500` is the last 500 bytes, and it must not be flattened into
    // "junk": both spellings read as a missing start, and the answers differ by
    // the whole file.
    crate::pin_test_dir();
    assert_eq!(parse_range(Some("bytes=-500")), Some((None, Some(500))));
    // `bytes=-` with nothing after the dash is **`(None, None)`** in the reference: an empty
    // suffix still parses as a Range, and the caller resolves it against the real length (which
    // yields "the last 0 bytes"). This assertion used to say `None` — measured against the
    // reference by differential probing, which is how the difference showed up.
    assert_eq!(parse_range(Some("bytes=-")), Some((None, None)));
}

/// Every `Range` header shape the reference parses, replayed against this port.
///
/// 36 rows produced by executing the reference's `parse_range` over the shapes a differential
/// probe turned up: whitespace in four positions, `+`-signed, uppercase unit, the empty suffix,
/// multi-range, `stop < start`, junk, and three values that exceed `i64`.
///
/// Three rows changed this port when it was first run, and one of them has a **user-visible**
/// consequence: a trailing space (`bytes=0-9 `) used to produce `None`, and `None` is what makes
/// `serve_plan` answer **416** — which a DLNA renderer reads as a dead file.
#[test]
fn every_reference_range_shape_replays_identically() {
    crate::pin_test_dir();
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../tests/golden/parse_range.json"))
            .expect("corpus is JSON");
    let rows = corpus.as_array().expect("rows");
    let mut parsed = 0usize;
    let mut junk = 0usize;
    for row in rows {
        let header = row["header"].as_str().expect("header");
        let got = parse_range(Some(header));
        match &row["out"] {
            serde_json::Value::Null => {
                assert_eq!(got, None, "header {header:?} should be junk");
                junk += 1;
            }
            expected => {
                let start = expected[0].as_u64();
                let stop = expected[1].as_u64();
                // A value beyond `u64` cannot be represented; the reference keeps an unbounded
                // int. What must hold is that the port does not silently answer a *small*
                // offset, so those rows assert "refused" and say so. The corpus marks them,
                // because inferring "too big" from `as_u64() == None` also matched the empty
                // header's `[0, null]` — which parses fine.
                if row.get("exceeds_u64").is_some() {
                    assert_eq!(got, None, "header {header:?} exceeds u64 and must be refused");
                } else {
                    assert_eq!(
                        got,
                        Some((start, stop)),
                        "header {header:?} parsed differently"
                    );
                }
                parsed += 1;
            }
        }
    }
    assert_eq!(rows.len(), 36, "the corpus lost rows");
    assert!(parsed > 10 && junk > 5, "the corpus must exercise both paths: {parsed}/{junk}");
}

/// The fields are parsed the way Python's `int()` parses them.
///
/// Found by differential probing: `int(" 5")`, `int("5 ")` and `int("5\n")` are all 5, while
/// Rust's `str::parse::<u64>` rejects all three. The consequence of refusing one is not "an
/// error": it is a **416**, which a DLNA renderer reads as a dead file. A trailing space is
/// exactly what a hand-written or buggy sender produces.
#[test]
fn parse_range_tolerates_the_whitespace_python_tolerates() {
    crate::pin_test_dir();
    // Each row asserts the numbers the string really contains, so a tolerant parser cannot pass
    // by answering the same thing for all of them.
    for (header, expected) in [
        ("bytes=0-9 ", (Some(0), Some(9))),
        ("bytes=0 - 9", (Some(0), Some(9))),
        ("bytes=0-9\n", (Some(0), Some(9))),
        ("bytes= 5-9", (Some(5), Some(9))),
        ("bytes= 7-9", (Some(7), Some(9))),
        ("bytes=3- 8", (Some(3), Some(8))),
    ] {
        assert_eq!(
            parse_range(Some(header)),
            Some(expected),
            "header {header:?} was refused"
        );
    }
    // Java-style `+5` is accepted by both, so it is not part of this fix.
    assert_eq!(parse_range(Some("bytes=+5-9")), Some((Some(5), Some(9))));
}

#[test]
fn serve_plan_honours_the_suffix_form() {
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(1000), Some("bytes=-500"));
    assert_eq!(sp.status, 206);
    assert_eq!(sp.start, 500);
    assert_eq!(sp.stop, Some(999));
    assert_eq!(sp.content_length, Some(500));
}

// ---------------------------------------------------------------------------
// serve_plan
// ---------------------------------------------------------------------------

#[test]
fn serve_plan_no_range_no_length_is_200_no_cl() {
    crate::pin_test_dir();
    let sp = serve_plan(false, None, None);
    assert_eq!(sp.status, 200);
    assert_eq!(sp.content_length, None);
    assert!(!sp.partial);
}

#[test]
fn serve_plan_no_range_declared_length_is_200_with_cl() {
    crate::pin_test_dir();
    let sp = serve_plan(false, Some(1000), None);
    assert_eq!(sp.status, 200);
    assert_eq!(sp.content_length, Some(1000));
    assert!(!sp.partial);
}

#[test]
fn serve_plan_no_range_header_is_200_not_206() {
    // The red line: no Range header -> 200 even if the origin honours Range.
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(1000), None);
    assert_eq!(sp.status, 200);
    assert!(!sp.partial);
}

#[test]
fn serve_plan_range_request_against_rangeable_origin_is_206() {
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(1000), Some("bytes=100-199"));
    assert_eq!(sp.status, 206);
    assert_eq!(sp.start, 100);
    assert_eq!(sp.stop, Some(199));
    assert_eq!(sp.content_length, Some(100));
    assert!(sp.partial);
}

#[test]
fn serve_plan_range_past_end_is_416() {
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(500), Some("bytes=600-"));
    assert_eq!(sp.status, 416);
}

#[test]
fn serve_plan_junk_range_is_200_whole_file() {
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(1000), Some("kilobytes=0-9"));
    assert_eq!(sp.status, 200);
    assert_eq!(sp.content_length, Some(1000));
    assert!(!sp.partial);
}

#[test]
fn serve_plan_open_ended_range_to_eof() {
    crate::pin_test_dir();
    let sp = serve_plan(true, Some(1000), Some("bytes=500-"));
    assert_eq!(sp.status, 206);
    assert_eq!(sp.start, 500);
    assert_eq!(sp.stop, Some(999));
    assert_eq!(sp.content_length, Some(500));
}

// ---------------------------------------------------------------------------
// needs_adtstoasc / tells_adts
// ---------------------------------------------------------------------------

#[test]
fn adts_codec_detection() {
    crate::pin_test_dir();
    assert!(needs_adtstoasc("aac"));
    assert!(needs_adtstoasc("AAC"));
    assert!(needs_adtstoasc("mp4a"));
    assert!(!needs_adtstoasc("opus"));
    assert!(!needs_adtstoasc("mp2"));
    assert!(!needs_adtstoasc(""));
}

#[test]
fn adts_tells_detection() {
    crate::pin_test_dir();
    assert!(tells_adts("Malformed AAC bitstream detected: use aac_adtstoasc"));
    assert!(!tells_adts("some other ffmpeg error"));
    assert!(!tells_adts(""));
}

// ---------------------------------------------------------------------------
// choose_advertise_host
// ---------------------------------------------------------------------------

#[test]
fn advertise_host_empty_advertisable_returns_loopback() {
    crate::pin_test_dir();
    assert_eq!(choose_advertise_host(&[], &[]), "127.0.0.1");
}

#[test]
fn advertise_host_prefers_reachable_over_first() {
    crate::pin_test_dir();
    let advs = vec!["10.0.0.1".to_string(), "192.168.1.5".to_string()];
    let reach = vec!["192.168.1.5".to_string()];
    assert_eq!(choose_advertise_host(&advs, &reach), "192.168.1.5");
}

#[test]
fn advertise_host_falls_back_to_first_when_no_reachable_match() {
    crate::pin_test_dir();
    let advs = vec!["10.0.0.1".to_string()];
    let reach: Vec<String> = vec![];
    assert_eq!(choose_advertise_host(&advs, &reach), "10.0.0.1");
}

// ---------------------------------------------------------------------------
// RelayStore
// ---------------------------------------------------------------------------

fn dummy_relay(id: &str) -> Arc<Relay> {
    Arc::new(Relay {
        relay_id:     id.to_string(),
        url:          "http://example.com/x.mp4".to_string(),
        headers:      HashMap::new(),
        mode:         "proxy".to_string(),
        title:        "test".to_string(),
        content_type: Mutex::new("video/mp4".to_string()),
        created:      now_secs(),
        last_seen:    Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: false, length: None, error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    })
}

#[test]
fn store_add_and_get() {
    crate::pin_test_dir();
    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    let r = dummy_relay("abc");
    store.add(Arc::clone(&r));
    assert!(store.get("abc").is_some());
    assert!(store.get("xyz").is_none());
}

#[test]
fn store_hmac_eq_all_entries_checked() {
    // A timing-safe comparison must compare EVERY entry, not stop early.
    crate::pin_test_dir();
    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    store.add(dummy_relay("aaaa"));
    store.add(dummy_relay("bbbb"));
    store.add(dummy_relay("cccc"));
    assert!(store.get("cccc").is_some());
    assert!(store.get("aaaa").is_some());
    assert!(store.get("dddd").is_none());
}

#[test]
fn store_evicts_oldest_when_full() {
    crate::pin_test_dir();
    let store = RelayStore::new(2, RELAY_IDLE_TTL);
    let r1 = dummy_relay("r1");
    *r1.last_seen.lock().unwrap() = now_secs() - 10.0;
    store.add(Arc::clone(&r1));
    store.add(dummy_relay("r2"));
    store.add(dummy_relay("r3")); // evicts r1 (oldest)
    assert!(store.get("r1").is_none());
    assert!(store.get("r2").is_some());
    assert!(store.get("r3").is_some());
}

#[test]
fn store_drop_removes_and_fails() {
    crate::pin_test_dir();
    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    store.add(dummy_relay("x1"));
    store.drop_relay("x1");
    assert!(store.get("x1").is_none());
}

#[test]
fn store_peek_does_not_extend_ttl() {
    crate::pin_test_dir();
    let store = RelayStore::new(RELAY_MAX, 0.01); // 10ms TTL
    store.add(dummy_relay("p1"));
    // peek must not renew it
    store.peek("p1");
    std::thread::sleep(Duration::from_millis(50));
    // After TTL a get must return None
    assert!(store.get("p1").is_none());
}

// ---------------------------------------------------------------------------
// HTTP server: GET / HEAD / 404 / 405 over real loopback
// ---------------------------------------------------------------------------

/// One server per test, with its own temp directory and relay id.
///
/// A shared directory and a shared id would make these tests race: cargo runs
/// them in parallel threads, and two of them would read each other's
/// `{id}.mp4` — one passing for the wrong reason, one failing on a file it
/// never wrote.
fn make_relay_server(content: &[u8], mode: &str) -> (RelayServer, Arc<RelayStore>, Arc<Relay>, u16) {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!("relay_{}_{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst));
    let temp = std::env::temp_dir().join(format!("macast-relay-test-{}", id));
    std::fs::create_dir_all(&temp).unwrap();
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let server = RelayServer::start(Arc::clone(&store), temp.clone()).unwrap();
    let port = server.port;

    let relay = Arc::new(Relay {
        relay_id:     id.clone(),
        url:          "http://example.com/x.mp4".to_string(),
        headers:      HashMap::new(),
        mode:         mode.to_string(),
        title:        "Test".to_string(),
        content_type: Mutex::new("video/mp4".to_string()),
        created:      now_secs(),
        last_seen:    Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: true, length: Some(content.len() as u64),
            error: String::new(), probe_error: String::new(),
            path: if mode == "remux" {
                let p = temp.join(format!("{id}.mp4"));
                std::fs::write(&p, content).unwrap();
                p.to_string_lossy().into_owned()
            } else { String::new() },
            state: if mode == "remux" { RelayState::Complete } else { RelayState::Open },
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    store.add(Arc::clone(&relay));
    (server, store, relay, port)
}

fn http_get(port: u16, path: &str, extra: &[(&str, &str)]) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let _ = write!(s, "GET {} HTTP/1.0\r\n", path);
    for (k, v) in extra { let _ = write!(s, "{}: {}\r\n", k, v); }
    let _ = s.write_all(b"\r\n");
    read_response(&mut s)
}

fn http_head(port: u16, path: &str) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let _ = write!(s, "HEAD {} HTTP/1.0\r\n\r\n", path);
    read_response(&mut s)
}

fn read_response(s: &mut TcpStream) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let sep = b"\r\n\r\n";
    let split = buf.windows(4).position(|w| w == sep).unwrap_or(buf.len());
    let head = std::str::from_utf8(&buf[..split]).unwrap_or("");
    let body = if split + 4 <= buf.len() { buf[split+4..].to_vec() } else { Vec::new() };
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let code: u16 = first.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    (code, headers, body)
}

#[test]
fn http_get_known_relay_returns_200_with_body() {
    crate::pin_test_dir();
    let content = b"hello relay bytes";
    let (_server, _store, relay, port) = make_relay_server(content, "remux");
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50)); // let server start
    let (code, hdrs, body) = http_get(port, &path, &[]);
    assert_eq!(code, 200, "expected 200 got {}", code);
    assert_eq!(body, content);
    assert_eq!(hdrs.get("content-type").map(|s| s.as_str()), Some("video/mp4"));
}

#[test]
fn http_get_unknown_relay_returns_404() {
    crate::pin_test_dir();
    let (_server, _store, _relay, port) = make_relay_server(b"data", "remux");
    std::thread::sleep(Duration::from_millis(50));
    let (code, _, _) = http_get(port, "/relay/no_such_id/media", &[]);
    assert_eq!(code, 404);
}

#[test]
fn http_206_on_range_request() {
    crate::pin_test_dir();
    let content = b"0123456789abcdef";
    let (_server, _store, relay, port) = make_relay_server(content, "remux");
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let (code, hdrs, body) = http_get(port, &path, &[("Range", "bytes=4-7")]);
    assert_eq!(code, 206, "expected 206 got {}", code);
    assert_eq!(body, b"4567");
    assert!(hdrs.contains_key("content-range"));
}

#[test]
fn http_head_returns_no_body() {
    crate::pin_test_dir();
    let (_server, _store, relay, port) = make_relay_server(b"data", "remux");
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let (code, _, body) = http_head(port, &path);
    assert_eq!(code, 200);
    assert!(body.is_empty());
}

#[test]
fn http_post_returns_405() {
    crate::pin_test_dir();
    let (_server, _store, relay, port) = make_relay_server(b"data", "remux");
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let mut s = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let _ = write!(s, "POST {} HTTP/1.0\r\n\r\n", path);
    let (code, _, _) = read_response(&mut s);
    assert_eq!(code, 405);
}

#[test]
fn failed_relay_returns_502() {
    crate::pin_test_dir();
    let (_server, _store, relay, port) = make_relay_server(b"data", "remux");
    relay.inner.lock().unwrap().state = RelayState::Failed;
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let (code, _, _) = http_get(port, &path, &[]);
    assert_eq!(code, 502);
}

#[test]
fn relay_with_no_path_returns_410() {
    crate::pin_test_dir();
    let (_server, _store, relay, port) = make_relay_server(b"data", "remux");
    relay.inner.lock().unwrap().path = String::new();
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let (code, _, _) = http_get(port, &path, &[]);
    assert_eq!(code, 410);
}

#[test]
fn http_416_on_out_of_range_request() {
    crate::pin_test_dir();
    let content = b"short";
    let (_server, _store, relay, port) = make_relay_server(content, "remux");
    let path = format!("/relay/{}/media", relay.relay_id);
    std::thread::sleep(Duration::from_millis(50));
    let (code, hdrs, _) = http_get(port, &path, &[("Range", "bytes=9999-")]);
    assert_eq!(code, 416);
    assert!(hdrs.contains_key("content-range"));
}

// ---------------------------------------------------------------------------
// ffmpeg argv: the read side carries its own headers, the write side is fixed
// ---------------------------------------------------------------------------

#[test]
fn input_arguments_put_headers_in_front_of_their_own_input() {
    crate::pin_test_dir();
    let mut headers = HashMap::new();
    headers.insert("Referer".to_string(), "http://r/".to_string());
    headers.insert("User-Agent".to_string(), "ua".to_string());
    let args = input_arguments("http://a/v.mpd", &headers);
    // `-headers <blob>` must precede `-i`, or ffmpeg attaches it to the wrong
    // input — the second input of a merge would then fetch unauthenticated and
    // the origin answers 403.
    let h = args.iter().position(|a| a == "-headers").expect("headers");
    let i = args.iter().position(|a| a == "-i").expect("input");
    assert!(h < i, "headers must precede the input they belong to");
    assert_eq!(args[h + 1], "Referer: http://r/\r\nUser-Agent: ua");
    assert_eq!(args[i + 1], "http://a/v.mpd");
}

#[test]
fn input_arguments_without_headers_omit_the_flag() {
    crate::pin_test_dir();
    let args = input_arguments("http://a/v.mpd", &HashMap::new());
    assert!(!args.contains(&"-headers".to_string()));
    assert_eq!(args[0], "-protocol_whitelist");
}

#[test]
fn remux_arguments_are_the_growing_file_shape() {
    crate::pin_test_dir();
    let args = remux_arguments(false);
    let joined = args.join(" ");
    // A standard MP4 is not openable while ffmpeg writes it: the `moov` lands at
    // the end on exit. These three flags are what makes the prefix the file.
    assert!(joined.contains("+frag_keyframe+empty_moov+default_base_moof"), "{joined}");
    // Without `-flush_packets 1` the growing file sat at 28 bytes for 3 s.
    assert!(args.contains(&"1".to_string()));
    assert!(args.contains(&"mp4".to_string()));
    assert!(args.contains(&"copy".to_string()));
    assert!(!args.contains(&"aac_adtstoasc".to_string()));
}

#[test]
fn remux_arguments_add_the_filter_only_when_asked() {
    crate::pin_test_dir();
    let args = remux_arguments(true);
    let h = args.iter().position(|a| a == "-bsf:a").expect("filter flag");
    assert_eq!(args[h + 1], "aac_adtstoasc");
}

// ---------------------------------------------------------------------------
// content types
// ---------------------------------------------------------------------------

fn cand_with(url: &str, container: Option<&str>) -> Candidate {
    let mut c = Candidate::new(url.to_string(), "page".to_string());
    c.probe.container = container.map(|s| s.to_string());
    c
}

#[test]
fn content_type_prefers_the_probe_then_the_suffix() {
    crate::pin_test_dir();
    assert_eq!(content_type_for(&cand_with("http://a/stream", Some("mov,mp4,m4a,3gp"))), "video/mp4");
    assert_eq!(content_type_for(&cand_with("http://a/x.webm", None)), "video/webm");
    assert_eq!(content_type_for(&cand_with("http://a/x.m3u8", None)), "application/vnd.apple.mpegurl");
    assert_eq!(content_type_for(&cand_with("http://a/x", None)), "application/octet-stream");
}

#[test]
fn content_range_total_is_the_last_slash_number() {
    crate::pin_test_dir();
    assert_eq!(total_from_content_range("bytes 0-0/1234567"), Some(1234567));
    assert_eq!(total_from_content_range("bytes 0-0/ 42 "), Some(42));
    assert_eq!(total_from_content_range("bytes 0-0/*"), None);
    assert_eq!(total_from_content_range(""), None);
}

// ---------------------------------------------------------------------------
// the proxy path against a real origin
// ---------------------------------------------------------------------------

/// An origin on loopback: answers the `bytes=0-0` probe, then serves the body.
///
/// The header block **must** end with a blank line. The first version of this
/// stub omitted it, and a raw `read_to_end` client did not notice — it reads to
/// EOF — while ureq, a real parser, waited for the blank line, hit EOF and
/// reported `Peer disconnected`, which read like "the relay cannot talk to an
/// origin at all" rather than "my fixture is malformed".
fn origin_server(total: usize, rangeable: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let body = vec![b'x'; total];
        for _ in 0..2 {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let asked_range = String::from_utf8_lossy(&buf)
                .to_lowercase()
                .contains("range:");
            if rangeable && asked_range {
                let head = format!(
                    "HTTP/1.0 206 Partial Content\r\n\
                     Content-Type: video/mp4\r\n\
                     Content-Range: bytes 0-0/{total}\r\n\
                     Content-Length: 1\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body[..1]);
            } else {
                let head = format!(
                    "HTTP/1.0 200 OK\r\n\
                     Content-Type: video/mp4\r\n\
                     Content-Length: {total}\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        }
    });
    port
}

#[test]
fn probe_proxy_learns_ranges_and_length_from_a_206() {
    crate::pin_test_dir();
    let port = origin_server(4096, true);
    let relay = Arc::new(Relay {
        relay_id: "p1".into(),
        url: format!("http://127.0.0.1:{port}/x.mp4"),
        headers: HashMap::new(),
        mode: "proxy".into(),
        title: String::new(),
        content_type: Mutex::new("application/octet-stream".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: false, length: None, error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    assert!(probe_proxy(&relay));
    let inner = relay.inner.lock().unwrap();
    assert!(inner.ranges, "a 206 answer is the whole of the Range fact");
    assert_eq!(inner.length, Some(4096));
}

#[test]
fn probe_proxy_keeps_the_length_and_drops_the_ranges_fact() {
    crate::pin_test_dir();
    let port = origin_server(2048, false);
    let relay = Arc::new(Relay {
        relay_id: "p2".into(),
        url: format!("http://127.0.0.1:{port}/x.mp4"),
        headers: HashMap::new(),
        mode: "proxy".into(),
        title: String::new(),
        content_type: Mutex::new("application/octet-stream".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: false, length: None, error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    assert!(probe_proxy(&relay));
    let inner = relay.inner.lock().unwrap();
    // Length and Range support are two different facts: an origin that declares
    // 40 MB and ignores `Range` still deserves a `Content-Length`.
    assert!(!inner.ranges);
    assert_eq!(inner.length, Some(2048));
    let sp = serve_plan(inner.ranges, inner.length, Some("bytes=0-99"));
    assert_eq!(sp.status, 200, "a 206 here would be a lie: we hand over the whole file");
    assert_eq!(sp.content_length, Some(2048));
}

// ---------------------------------------------------------------------------
// remux / merge against a fake ffmpeg on PATH
// ---------------------------------------------------------------------------

/// A fake `ffmpeg` that records its argv one argument per line and then plays
/// the script named by `FAKE_FFMPEG_MODE`:
///
/// * `write`     — write real bytes to the output path (last argv), exit 0
/// * `adts-once` — first run: write 23 KB and print the ADTS complaint, exit 255
/// * `always-adts` — always the complaint, never any bytes: proves the retry
///   cannot loop
/// * `fail`      — exit 1 with one stderr line
fn fake_ffmpeg(name: &str, script_body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("macast-fake-ffmpeg-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ffmpeg");
    std::fs::write(&path, script_body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// One entry per spawn, each the full argv with `\u{1f}` between arguments.
///
/// The unit is the **run**, not the argument: a receipt that wrote one line per
/// argument makes "how many times did we spawn this?" a question the file
/// cannot answer, which is the one question the ADTS retry rows exist to ask.
fn argv_receipt(fake: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(fake.with_extension("argv"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\u{1f}').map(|s| s.to_string()).collect())
        .collect()
}

/// How many times the fake binary ran.
fn spawn_count(fake: &Path) -> usize {
    std::fs::read_to_string(fake.with_extension("runs"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn remux_relay(id: &str, dir: &Path, audio_url: &str, audio_codec: &str) -> Arc<Relay> {
    Arc::new(Relay {
        relay_id: id.into(),
        url: "http://a/v.mpd".into(),
        headers: HashMap::new(),
        mode: "remux".into(),
        title: String::new(),
        content_type: Mutex::new("video/mp4".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: false, length: None, error: String::new(), probe_error: String::new(),
            path: dir.join(format!("{id}.mp4")).to_string_lossy().into_owned(),
            state: RelayState::Open,
            audio_url: audio_url.into(),
            audio_headers: HashMap::new(),
            audio_codec: audio_codec.into(),
            adtstoasc: false,
            ffmpeg: String::new(),
            proc: None,
        })),
    })
}

/// Start a remux with an explicit binary, bypassing the PATH lookup.
///
/// The binary is a parameter rather than an environment variable on purpose:
/// these tests run in parallel threads, and a shared variable would be cleared
/// out from under a sibling that had just set it — the classic "the fixture, not
/// the product" red that looks like a relay bug.
fn start_with(relay: &Arc<Relay>, ffmpeg: &Path) -> bool {
    {
        let mut inner = relay.inner.lock().unwrap();
        inner.ffmpeg = ffmpeg.to_string_lossy().into_owned();
        inner.adtstoasc = needs_adtstoasc(&inner.audio_codec);
    }
    if !spawn_remux(relay) {
        panic!("spawn failed: {}", relay.inner.lock().unwrap().error);
    }
    watch_remux(Arc::clone(relay));
    true
}

/// Wait for the transmux to reach either end state.
///
/// Asking for `Complete` and then falling back to `Failed` costs two full
/// budgets on the answer that is `Failed` — twenty seconds spent proving
/// something about a fixture, not about the relay.
fn wait_settled(relay: &Relay) -> RelayState {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let state = relay.inner.lock().unwrap().state.clone();
        if state != RelayState::Open && state != RelayState::Running { return state; }
        thread::sleep(Duration::from_millis(20));
    }
    RelayState::Running
}

#[test]
fn merge_feeds_a_second_input_with_its_own_headers() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-relay-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = fake_ffmpeg("merge",
        "#!/bin/sh\n\
         D=$(dirname \"$0\")\n\
         echo x >> \"$D/ffmpeg.runs\"\n\
         LINE=$0\n\
         for a in \"$@\"; do LINE=\"$LINE$(printf '\\037')$a\"; done\n\
         printf '%s\\n' \"$LINE\" >> \"$D/ffmpeg.argv\"\n\
         exit 0\n",
    );
    let relay = remux_relay("merge_case", &dir, "http://a/audio.m4a", "aac");
    relay.inner.lock().unwrap().audio_headers =
        HashMap::from([("Referer".to_string(), "http://r/".to_string())]);
    assert!(start_with(&relay, &fake));
    // The fake exits 0 without writing a byte, so the honest end state here is
    // `failed`: a zero-byte output is not a file.
    assert_eq!(wait_settled(&relay), RelayState::Failed);

    let runs = argv_receipt(&fake);
    assert_eq!(runs.len(), 1, "one spawn for one merge");
    let argv = &runs[0];
    // Two `-i`, and the audio one carries its own `-headers` before it.
    let inputs: Vec<usize> = argv
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "-i")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(inputs.len(), 2, "a merge is one file with two inputs: {argv:?}");
    let headers: Vec<usize> = argv
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "-headers")
        .map(|(i, _)| i)
        .collect();
    // The video input of this fixture asks for no headers, so one `-headers` is
    // the right count. What has to hold is that the one that is there belongs to
    // the **second** input: a blanket `-headers` at the front would leave the
    // audio unauthenticated, the origin answers 403, ffmpeg exits, and the only
    // sentence the user ever sees is "the transmux failed".
    assert_eq!(headers.len(), 1, "{argv:?}");
    assert!(headers[0] > inputs[0] && headers[0] < inputs[1],
        "the headers must belong to the input that needed them");
    // The whole-file filter: aac says so up front.
    assert!(argv.iter().any(|a| a == "aac_adtstoasc"));
    // `mode` stays remux even though there are two inputs.
    assert_eq!(relay.mode, "remux");
}

#[test]
fn a_known_aac_codec_never_waits_for_ffmpeg_to_ask() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-relay-adts-a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = fake_ffmpeg("adts_a",
        "#!/bin/sh\n\
         D=$(dirname \"$0\")\n\
         echo x >> \"$D/ffmpeg.runs\"\n\
         LINE=$0\n\
         for a in \"$@\"; do LINE=\"$LINE$(printf '\\037')$a\"; done\n\
         printf '%s\\n' \"$LINE\" >> \"$D/ffmpeg.argv\"\n\
         echo '[mp4 @ 0x1] Malformed AAC bitstream detected: use the audio bitstream filter aac_adtstoasc to fix it' >&2\n\
         OUT=$(printf '%s' \"$LINE\" | tr '\\037' '\\n' | tail -1)\n\
         printf '%.0sx' {1..23000} > \"$OUT\"\n\
         exit 255\n",
    );
    let relay = remux_relay("adts_a", &dir, "", "aac");
    assert!(start_with(&relay, &fake));
    // The filter was already on, so the complaint is not acted on a second time.
    assert_eq!(wait_settled(&relay), RelayState::Failed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(spawn_count(&fake), 1, "the latch is what makes the retry once");
}

#[test]
fn an_unknown_codec_retries_once_on_ffmpegs_own_words() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-relay-adts-b-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // First run: junk + complaint + 255. Second run (the filter is now on):
    // real bytes + exit 0.
    let fake = fake_ffmpeg("adts_b",
        "#!/bin/sh\n\
         D=$(dirname \"$0\")\n\
         N=$(wc -l < \"$D/ffmpeg.runs\" 2>/dev/null || echo 0)\n\
         N=$((N+1)); echo \"$N\" >> \"$D/ffmpeg.runs\"\n\
         LINE=$0\n\
         for a in \"$@\"; do LINE=\"$LINE$(printf '\\037')$a\"; done\n\
         printf '%s\\n' \"$LINE\" >> \"$D/ffmpeg.argv\"\n\
         OUT=$(printf '%s' \"$LINE\" | tr '\\037' '\\n' | tail -1)\n\
         if [ \"$N\" = 1 ]; then\n\
           echo '[mp4 @ 0x1] Malformed AAC bitstream detected: use the audio bitstream filter aac_adtstoasc to fix it' >&2\n\
           printf '%.0sj' {1..23000} > \"$OUT\"; exit 255\n\
         fi\n\
         printf '%.0sk' {1..900} > \"$OUT\"; exit 0\n",
    );
    let relay = remux_relay("adts_b", &dir, "", "");
    assert!(start_with(&relay, &fake));
    assert_eq!(wait_settled(&relay), RelayState::Complete, "the retry should finish the file");
    assert!(relay.inner.lock().unwrap().adtstoasc, "the latch records that we asked");
    assert_eq!(spawn_count(&fake), 2, "exactly one retry: this row catches a loop");
    // The retry carried the filter the complaint asked for.
    let second = &argv_receipt(&fake)[1];
    assert!(second.contains(&"aac_adtstoasc".to_string()), "{second:?}");
}

#[test]
fn an_adts_complaint_that_never_stops_cannot_loop_forever() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-relay-adts-c-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = fake_ffmpeg("adts_c",
        "#!/bin/sh\n\
         D=$(dirname \"$0\")\n\
         echo x >> \"$D/ffmpeg.runs\"\n\
         echo 'Malformed AAC bitstream detected' >&2\n\
         exit 255\n",
    );
    let relay = remux_relay("adts_c", &dir, "", "");
    assert!(start_with(&relay, &fake));
    assert_eq!(wait_settled(&relay), RelayState::Failed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(spawn_count(&fake), 2, "two runs, then a sentence naming the exit code");
    let err = relay.inner.lock().unwrap().error.clone();
    assert!(err.contains("ffmpeg 退出码 255"), "{err}");
}

#[test]
fn eviction_kills_the_encoder_and_takes_its_file() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-relay-evict-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("evict.mp4");
    std::fs::write(&file, b"1234").unwrap();
    let store = RelayStore::new(1, RELAY_IDLE_TTL);
    let relay = Arc::new(Relay {
        relay_id: "old".into(),
        url: "http://a/v.mpd".into(),
        headers: HashMap::new(),
        mode: "remux".into(),
        title: String::new(),
        content_type: Mutex::new("video/mp4".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs() - 100.0)),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: true, length: Some(4), error: String::new(), probe_error: String::new(),
            path: file.to_string_lossy().into_owned(),
            state: RelayState::Complete,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    store.add(Arc::clone(&relay));
    store.add(dummy_relay("new"));
    // Eviction is not a promise in the future: the file is gone by the time
    // `add` returns, because shutdown runs outside the lock but before it.
    assert!(!file.exists(), "the temp file must not outlive the entry");
    assert!(store.get("old").is_none());
}

// ---------------------------------------------------------------------------
// "no bytes yet" is not a 200
// ---------------------------------------------------------------------------

/// A server with a short growth budget, so the rows below do not each pay the
/// production twenty seconds to learn the same thing.
fn quick_server(content: &[u8], mode: &str, budget_ms: u64) -> (RelayServer, Arc<Relay>, u16) {
    static SEQ2: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!("quick_{}_{}", std::process::id(), SEQ2.fetch_add(1, Ordering::SeqCst));
    let temp = std::env::temp_dir().join(format!("macast-relay-quick-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let server = RelayServer::start_with_budget(
        Arc::clone(&store), temp.clone(), Duration::from_millis(budget_ms),
    ).unwrap();
    let path = temp.join(format!("{id}.mp4"));
    if mode == "remux" { std::fs::write(&path, content).unwrap(); }
    let relay = Arc::new(Relay {
        relay_id: id.clone(),
        url: "http://example.com/x.mpd".into(),
        headers: HashMap::new(),
        mode: mode.into(),
        title: String::new(),
        content_type: Mutex::new("video/mp4".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: true, length: Some(content.len() as u64),
            error: String::new(), probe_error: String::new(),
            path: if mode == "remux" { path.to_string_lossy().into_owned() } else { String::new() },
            state: RelayState::Running,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    store.add(Arc::clone(&relay));
    let port = server.port;
    (server, relay, port)
}

#[test]
fn a_growing_file_with_no_bytes_yet_is_503_not_200() {
    crate::pin_test_dir();
    // The file exists but is empty: the transmux has started and written nothing.
    let (_server, relay, port) = quick_server(b"", "remux", 300);
    let path = format!("/relay/{}/media", relay.relay_id);
    let (code, _, body) = http_get(port, &path, &[]);
    assert_eq!(code, 503, "a 200 here is a lengthless answer every player reads as EOF");
    assert_ne!(body, b"", "and it must not hand over an empty success");
}

#[test]
fn a_transmux_that_failed_while_waiting_is_502() {
    crate::pin_test_dir();
    let (_server, relay, port) = quick_server(b"", "remux", 300);
    relay.inner.lock().unwrap().state = RelayState::Failed;
    let path = format!("/relay/{}/media", relay.relay_id);
    let (code, _, _) = http_get(port, &path, &[]);
    // 502 and 503 are two different sentences: one is broken, the other is not
    // ready. The page tells the user to retry only for the second.
    assert_eq!(code, 502);
}

#[test]
fn the_first_byte_releases_the_waiter() {
    crate::pin_test_dir();
    let (_server, relay, port) = quick_server(b"", "remux", 3000);
    // A growing file's length is a fact about the file, not a constant the
    // relay was built with: while ffmpeg writes, `length` stays `None` and only
    // the real size on disk is a true answer.
    relay.inner.lock().unwrap().length = None;
    let out = relay.inner.lock().unwrap().path.clone();
    let path = format!("/relay/{}/media", relay.relay_id);
    let writer = std::thread::spawn(move || {
        thread::sleep(Duration::from_millis(250));
        std::fs::write(&out, b"now there are bytes").unwrap();
    });
    let start = Instant::now();
    let (code, _, body) = http_get(port, &path, &[]);
    let elapsed = start.elapsed();
    writer.join().unwrap();
    assert_eq!(code, 200, "a real length on disk answers the read");
    assert_eq!(body, b"now there are bytes");
    assert!(elapsed >= Duration::from_millis(200), "it waited rather than answering early");
}

// ---------------------------------------------------------------------------
// proxy: the viewer's Range has to reach the origin
// ---------------------------------------------------------------------------

/// An origin that records the `Range` it was asked for and answers 206 with
/// exactly those bytes.
fn recording_origin(total: usize, seen: Arc<std::sync::Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let body: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        for _ in 0..3 {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let req = String::from_utf8_lossy(&buf).to_string();
            let asked = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("range:"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string());
            let Some(range) = asked else {
                let head = format!("HTTP/1.0 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {total}\r\n\r\n");
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
                continue;
            };
            seen.lock().unwrap().push(range.clone());
            let (from, to) = {
                let spec = range.trim_start_matches("bytes=");
                let mut it = spec.splitn(2, '-');
                let f: usize = it.next().unwrap_or("0").parse().unwrap_or(0);
                let t: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(total - 1);
                (f, t.min(total - 1))
            };
            let slice = &body[from..=to];
            let head = format!(
                "HTTP/1.0 206 Partial Content\r\nContent-Type: video/mp4\r\n\
                 Content-Range: bytes {from}-{to}/{total}\r\nContent-Length: {}\r\n\r\n",
                slice.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(slice);
        }
    });
    port
}

#[test]
fn the_viewers_range_is_translated_into_the_origins_range() {
    crate::pin_test_dir();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let total = 4096;
    let port = recording_origin(total, Arc::clone(&seen));
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let temp = std::env::temp_dir().join(format!("macast-relay-proxy-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let _server = RelayServer::start_with_budget(Arc::clone(&store), temp, Duration::from_millis(500)).unwrap();
    let relay = Arc::new(Relay {
        relay_id: "proxy1".into(),
        url: format!("http://127.0.0.1:{port}/x.mp4"),
        headers: HashMap::new(),
        mode: "proxy".into(),
        title: String::new(),
        content_type: Mutex::new("video/mp4".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            // What the probe learned: this origin does honour Range.
            ranges: true, length: Some(total as u64),
            error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    store.add(Arc::clone(&relay));

    let (code, hdrs, body) = http_get(_server.port, "/relay/proxy1/media", &[("Range", "bytes=100-199")]);
    assert_eq!(code, 206);
    assert_eq!(body.len(), 100);
    assert_eq!(hdrs.get("content-range").map(|s| s.as_str()), Some("bytes 100-199/4096"));
    // The relay asked the origin for exactly the bytes it promised.
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["bytes=100-199".to_string()],
        "a proxy that re-fetches the whole file hands the viewer a 200 it will read as a stall"
    );
}

#[test]
fn a_proxy_without_range_support_gets_the_whole_file_asked_for() {
    crate::pin_test_dir();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let total = 1024;
    let port = recording_origin(total, Arc::clone(&seen));
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let temp = std::env::temp_dir().join(format!("macast-relay-proxy2-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let server = RelayServer::start_with_budget(Arc::clone(&store), temp, Duration::from_millis(500)).unwrap();
    let relay = Arc::new(Relay {
        relay_id: "proxy2".into(),
        url: format!("http://127.0.0.1:{port}/x.mp4"),
        headers: HashMap::new(),
        mode: "proxy".into(),
        title: String::new(),
        content_type: Mutex::new("video/mp4".to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            // The origin declared a length but answered 200 to the probe.
            ranges: false, length: Some(total as u64),
            error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    });
    store.add(Arc::clone(&relay));

    let (code, hdrs, body) = http_get(server.port, "/relay/proxy2/media", &[("Range", "bytes=0-99")]);
    assert_eq!(code, 200, "206 here would be a lie: the bytes handed over are the whole file");
    assert_eq!(body.len(), total);
    assert!(!hdrs.contains_key("content-range"));
    assert_eq!(hdrs.get("content-length").map(|s| s.as_str()), Some("1024"));
    assert!(seen.lock().unwrap().is_empty(), "no Range is forwarded when the origin ignores it");
}

// ---------------------------------------------------------------------------
// the proxy path when the origin does not cooperate
// ---------------------------------------------------------------------------

/// A loopback origin that answers **one** request and then disappears.
///
/// The header block ends with a blank line — `origin_server`'s doc records what
/// its absence cost: a raw `read_to_end` client is satisfied by EOF, ureq is not.
///
/// One `accept` is the fixture's whole lifetime because the proxy probe asks
/// exactly once. A two-connection stub would leave a socket waiting for whoever
/// comes next, which is how a relay test ends up answering a sibling's request.
fn one_shot_origin(
    status: u16,
    reason: &str,
    content_type: Option<&str>,
    body: usize,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let typed = match content_type {
        Some(t) => format!("Content-Type: {t}\r\n"),
        None => String::new(),
    };
    let reason = reason.to_string();
    thread::spawn(move || {
        let Ok((mut s, _)) = listener.accept() else { return };
        let mut buf = [0u8; 2048];
        let _ = s.read(&mut buf);
        let head = format!("HTTP/1.0 {status} {reason}\r\n{typed}Content-Length: {body}\r\n\r\n");
        let payload = vec![b'x'; body];
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&payload);
    });
    port
}

/// A scratch directory per fixture, numbered so parallel tests never share one.
///
/// `make_relay_server`'s doc carries the original lesson: a directory shared
/// between rows means one row reads another's `<id>.mp4` and passes for the
/// wrong reason while the real failure lands on a row that never wrote a file.
static SEQ3: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_dir_for(tag: &str) -> PathBuf {
    let n = SEQ3.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("macast-relay-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Backdate a file's mtime.
///
/// `std::fs::File::set_modified` (stable since 1.75, and here it takes `&self`)
/// rather than a `filetime` dependency or a shell `touch`: the age rule is the
/// thing under test, so the fixture has to state the age, not arrange it through
/// a second toolchain.
fn age_file(path: &Path, seconds: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
        .unwrap();
}

/// The ffmpeg seam for a relay that must never ask for a binary.
///
/// A proxy candidate goes through `probe_proxy` and nothing else; if this is
/// called, the mode decision moved and the row below is measuring something
/// other than what it says.
fn no_ffmpeg_needed() -> Option<String> {
    panic!("a proxy relay never asks for ffmpeg");
}

/// The ffmpeg seam for a machine that has no binary on it.
fn no_ffmpeg_here() -> Option<String> {
    None
}

/// A relay whose origin is `port`, with `content_type` already in the field.
fn probe_relay(port: u16, content_type: &str) -> Arc<Relay> {
    Arc::new(Relay {
        relay_id: "probe1".into(),
        url: format!("http://127.0.0.1:{port}/media"),
        headers: HashMap::new(),
        mode: "proxy".into(),
        title: String::new(),
        content_type: Mutex::new(content_type.to_string()),
        created: now_secs(),
        last_seen: Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges: false, length: None, error: String::new(), probe_error: String::new(),
            path: String::new(), state: RelayState::Open,
            audio_url: String::new(), audio_headers: HashMap::new(),
            audio_codec: String::new(), adtstoasc: false, ffmpeg: String::new(), proc: None,
        })),
    })
}

/// A relay that never spoke, with the two message slots set by the caller.
fn inner_with(error: &str, probe_error: &str) -> RelayInner {
    RelayInner {
        ranges: false, length: None,
        error: error.into(), probe_error: probe_error.into(),
        path: String::new(), state: RelayState::Failed,
        audio_url: String::new(), audio_headers: HashMap::new(),
        audio_codec: String::new(), adtstoasc: false,
        ffmpeg: String::new(), proc: None,
    }
}

#[test]
fn a_refused_origin_is_refused_in_its_own_words_and_the_relay_is_dropped() {
    crate::pin_test_dir();
    let port = one_shot_origin(403, "Forbidden", None, 0);
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let dir = temp_dir_for("refuse403");
    let cand = cand_with(&format!("http://127.0.0.1:{port}/x.mp4"), None);
    assert_eq!(plan(&cand), "proxy");

    let outcome = open_relay_with(&cand, "", &store, &dir, &no_ffmpeg_needed);
    let Err(why) = outcome else { panic!("a 403 origin must not register a relay") };
    // Python's sentence, unprefixed: 「中转启动失败：」 belongs to the caller that
    // catches an unexpected raise (`protocol.py:2933`), not to the path that
    // produces this answer on purpose.
    assert_eq!(why, "源站返回 403");
    assert!(store.active().is_empty(), "a failed relay must leave no slot behind");
}

#[test]
fn a_five_hundred_is_refused_though_no_refused_list_contains_it() {
    crate::pin_test_dir();
    let port = one_shot_origin(500, "Internal Server Error", None, 0);
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let dir = temp_dir_for("refuse500");
    let cand = cand_with(&format!("http://127.0.0.1:{port}/x.mp4"), None);

    let outcome = open_relay_with(&cand, "", &store, &dir, &no_ffmpeg_needed);
    let Err(why) = outcome else {
        panic!("500 is not in the refused-status set, but it is not a playable relay either");
    };
    assert_eq!(why, "源站返回 500");
    assert!(store.active().is_empty());
}

#[test]
fn not_reaching_the_origin_is_a_failure_that_says_so() {
    crate::pin_test_dir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let dir = temp_dir_for("refuseconn");
    let cand = cand_with(&format!("http://127.0.0.1:{port}/x.mp4"), None);

    let outcome = open_relay_with(&cand, "", &store, &dir, &no_ffmpeg_needed);
    let Err(why) = outcome else { panic!("nothing is listening on {port}") };
    assert!(
        why.starts_with("无法连接源站："),
        "the reader has to know the origin was never reached, not that it refused: {why}"
    );
    assert!(store.active().is_empty());
}

#[test]
fn a_reachable_origin_still_registers() {
    crate::pin_test_dir();
    let port = origin_server(4096, true);
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let dir = temp_dir_for("refuseok");
    let cand = cand_with(&format!("http://127.0.0.1:{port}/x.mp4"), None);

    let outcome = open_relay_with(&cand, "", &store, &dir, &no_ffmpeg_needed);
    assert!(outcome.is_ok(), "the three refusals above need a positive control");
    let relay = outcome.unwrap();
    assert_eq!(relay.mode, "proxy");
    assert_eq!(store.active().len(), 1);
}

#[test]
fn a_manifest_without_a_transmuxer_is_refused_before_it_is_kept() {
    crate::pin_test_dir();
    let store = Arc::new(RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL));
    let dir = temp_dir_for("nofmpeg");
    let cand = cand_with("http://a/v.m3u8", None);
    assert_eq!(plan(&cand), "remux");

    let outcome = open_relay_with(&cand, "", &store, &dir, &no_ffmpeg_here);
    let Err(why) = outcome else { panic!("no ffmpeg means no remux relay") };
    assert_eq!(why, "这台机器没有 ffmpeg，无法把清单流转封装");
    assert!(store.active().is_empty(), "the slot taken before the spawn attempt is given back");
}

#[test]
fn refused_codes_are_exactly_the_set_the_resolver_matches() {
    crate::pin_test_dir();
    assert_eq!(REFUSED_CODES, &[401u16, 403, 404, 410][..]);
    for code in REFUSED_CODES {
        assert!(
            crate::resolve::REFUSED_STATUS.is_match(&code.to_string()),
            "{code} must be a status the resolver also refuses"
        );
    }
    // The two relay refusals above (403 and 500) differ here: the relay's rule is
    // "any error", this set is only the statuses that *mean* refusal.
    for other in ["500", "1404", "4041"] {
        assert!(
            !crate::resolve::REFUSED_STATUS.is_match(other),
            "{other} is not one of the refused statuses"
        );
    }
}

#[test]
fn refusal_prefers_the_first_slot_that_spoke() {
    crate::pin_test_dir();
    assert_eq!(refusal(&inner_with("源站返回 403", "无法连接源站：x")), "源站返回 403");
    assert_eq!(refusal(&inner_with("", "无法连接源站：x")), "无法连接源站：x");
    // Unreachable through `open_relay` — both writers set a message — so this
    // calls the helper directly. It is the sentence a viewer hears when the
    // relay died without speaking, and silence would render as 「」 plus a
    // sentence fragment.
    assert_eq!(refusal(&inner_with("", "")), "源站拒绝了这条地址");
}

// ---------------------------------------------------------------------------
// which Content-Type ends up on the relay
// ---------------------------------------------------------------------------

#[test]
fn the_origin_type_is_adopted_over_the_placeholder() {
    crate::pin_test_dir();
    let port = one_shot_origin(200, "OK", Some("video/quicktime; codecs=avc1.640028"), 8);
    let relay = probe_relay(port, UNKNOWN_TYPE);
    assert!(probe_proxy(&relay));
    assert_eq!(
        relay.content_type.lock().unwrap().as_str(),
        "video/quicktime",
        "a `/relay/<id>/media` URL has no suffix, so the origin's word is the only statement"
    );
}

#[test]
fn the_origin_type_never_overwrites_a_type_we_already_know() {
    crate::pin_test_dir();
    let port = one_shot_origin(200, "OK", Some("video/quicktime"), 8);
    let relay = probe_relay(port, "video/mp4");
    assert!(probe_proxy(&relay));
    assert_eq!(relay.content_type.lock().unwrap().as_str(), "video/mp4");
}

#[test]
fn octet_stream_is_not_a_statement_about_type() {
    crate::pin_test_dir();
    let port = one_shot_origin(200, "OK", Some(UNKNOWN_TYPE), 8);
    let relay = probe_relay(port, "");
    assert!(probe_proxy(&relay));
    assert_eq!(
        relay.content_type.lock().unwrap().as_str(),
        "",
        "adopting the catch-all type would turn 「无声明」 into 「声明了无声明」"
    );
}

#[test]
fn an_absent_content_type_is_adopted_as_nothing() {
    crate::pin_test_dir();
    let port = one_shot_origin(200, "OK", None, 8);
    let relay = probe_relay(port, UNKNOWN_TYPE);
    assert!(probe_proxy(&relay));
    assert_eq!(relay.content_type.lock().unwrap().as_str(), UNKNOWN_TYPE);
}

// ---------------------------------------------------------------------------
// the idle sweeper and the boot-time orphan sweep
// ---------------------------------------------------------------------------

#[test]
fn the_sweeper_expires_an_idle_relay_and_takes_its_temp_file_with_it() {
    crate::pin_test_dir();
    let dir = temp_dir_for("sweeper");
    // No `RelayServer` here on purpose: `start_with_budget` sweeps at boot, and
    // that one-shot sweep would race the periodic loop this row is watching.
    let store = Arc::new(RelayStore::new(RELAY_MAX, 0.01));
    let relay = remux_relay("swept", &dir, "", "");
    let path = dir.join("swept.mp4");
    std::fs::write(&path, b"y").unwrap();
    store.add(relay);

    assert!(RelayStore::start_sweeper(&store, Duration::from_millis(20)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if store.peek("swept").is_none() && !path.exists() { break; }
        thread::sleep(Duration::from_millis(20));
    }
    // `peek`, not `get`: the row must not be what keeps the entry alive.
    assert!(store.peek("swept").is_none(), "the idle relay should have expired");
    // Python's witness is `proc.killed is True`; the half of it Rust can measure
    // is that the encoder's temp file is gone with the entry.
    assert!(!path.exists(), "the temp file goes with the expired relay");
}

#[test]
fn the_sweeper_loop_starts_once_per_store() {
    crate::pin_test_dir();
    // A long TTL so nothing expires: this row is about the latch, not the work.
    let store = Arc::new(RelayStore::new(RELAY_MAX, 3600.0));
    assert!(RelayStore::start_sweeper(&store, Duration::from_millis(50)));
    assert!(!RelayStore::start_sweeper(&store, Duration::from_millis(50)));
    // Python proves idempotence by counting threads (`threading.enumerate()`).
    // Rust has no portable thread enumeration, so the latch's return value is the
    // whole answer — and the row above is what proves the loop actually runs.
}

#[test]
fn the_start_time_sweep_takes_only_old_unowned_mp4_files() {
    crate::pin_test_dir();
    let dir = temp_dir_for("orphans");
    let old = dir.join("orphan-old.mp4");
    let live = dir.join("watched-live.mp4");
    let text = dir.join("keepme.txt");
    let young = dir.join("orphan-young.mp4");
    for path in [&old, &live, &text, &young] {
        std::fs::write(path, b"y").unwrap();
    }
    age_file(&old, 4000);
    age_file(&live, 4000);
    age_file(&text, 4000);

    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    store.add(dummy_relay("watched-live"));

    let removed = store.sweep_orphans(&dir, RELAY_IDLE_TTL, now_secs());
    assert_eq!(removed, vec![old.clone()], "only the old, unowned .mp4 is an orphan");
    assert!(live.exists(), "a stem the store owns is not abandoned, however old it is");
    assert!(text.exists(), "sweeping relay files is not sweeping the directory");
    assert!(young.exists(), "another running instance may have written this a moment ago");
}

#[test]
fn the_sweep_of_a_directory_we_cannot_list_claims_nothing() {
    crate::pin_test_dir();
    let dir = temp_dir_for("sweep-missing");
    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    let missing = dir.join("sub"); // never created
    assert!(
        store.sweep_orphans(&missing, RELAY_IDLE_TTL, now_secs()).is_empty(),
        "a directory we cannot open is not a directory we cleaned"
    );
}

#[test]
fn the_sweep_names_the_files_it_took_in_a_file_someone_reads() {
    crate::pin_test_dir();
    // Two facts, one row. Until 2026-10-18 this module's warnings went through the
    // `log` facade, and the workspace installs no sink for it, so the sweep was
    // silent by construction. And a count alone cannot be acted on: the question a
    // reader has is "**which** file", because an id that is still on the page is a
    // different bug from a temp file an orphaned ffmpeg left behind.
    let dir = temp_dir_for("sweep-naming");
    let orphan = dir.join("relay-named-4f7a.mp4");
    std::fs::write(&orphan, b"y").unwrap();
    age_file(&orphan, 4000);
    let store = RelayStore::new(RELAY_MAX, RELAY_IDLE_TTL);
    let removed = store.sweep_orphans(&dir, RELAY_IDLE_TTL, now_secs());
    assert_eq!(removed.len(), 1, "the fixture is one orphan");
    let text = macast_core::logging::read_tail(&macast_core::paths::log_file(), 400).text;
    assert!(
        text.contains("relay-named-4f7a.mp4"),
        "the sweep said a number but not a name, or said nothing at all: {text}"
    );
    assert!(
        text.contains("swept 1 abandoned temp file(s)"),
        "the count went missing while the names arrived: {text}"
    );
}
