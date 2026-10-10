use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use super::*;

struct VolumeCapture {
    volumes: Mutex<Vec<i32>>,
    urls: Mutex<Vec<String>>,
    titles: Mutex<Vec<String>>,
    pauses: Mutex<u32>,
    stops: Mutex<u32>,
}

impl VolumeCapture {
    fn new() -> Self {
        Self {
            volumes: Mutex::new(Vec::new()),
            urls: Mutex::new(Vec::new()),
            titles: Mutex::new(Vec::new()),
            pauses: Mutex::new(0),
            stops: Mutex::new(0),
        }
    }
}

impl RendererCallback for VolumeCapture {
    fn set_media_url(&self, url: &str) {
        self.urls.lock().unwrap().push(url.to_string());
    }
    fn set_media_title(&self, title: &str) {
        self.titles.lock().unwrap().push(title.to_string());
    }
    fn set_media_pause(&self) {
        *self.pauses.lock().unwrap() += 1;
    }
    fn set_media_stop(&self) {
        *self.stops.lock().unwrap() += 1;
    }
    fn set_media_volume(&self, volume: i32) {
        self.volumes.lock().unwrap().push(volume);
    }
}

fn make_request(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> RtspRequest {
    let mut hdrs = HashMap::new();
    for (k, v) in headers {
        hdrs.insert(k.to_string(), v.to_string());
    }
    RtspRequest {
        method: method.to_string(),
        uri: uri.to_string(),
        headers: hdrs,
        body: body.to_string(),
    }
}

// ── get_header ──────────────────────────────────────────────────────

#[test]
fn get_header_exact_match() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("Content-Location".to_string(), "http://x".to_string());
    assert_eq!(get_header(&h, "Content-Location"), Some("http://x"));
}

#[test]
fn get_header_case_insensitive() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("content-location".to_string(), "http://x".to_string());
    assert_eq!(get_header(&h, "Content-Location"), Some("http://x"));
    assert_eq!(get_header(&h, "CONTENT-LOCATION"), Some("http://x"));
}

#[test]
fn get_header_missing() {
    crate::pin_test_dir();
    let h = HashMap::new();
    assert_eq!(get_header(&h, "CSeq"), None);
}

// ── random_mac ──────────────────────────────────────────────────────

#[test]
fn random_mac_format() {
    crate::pin_test_dir();
    let mac = random_mac();
    let parts: Vec<&str> = mac.split(':').collect();
    assert_eq!(parts.len(), 6, "MAC must have 6 colon-separated bytes");
    for p in &parts {
        assert_eq!(p.len(), 2, "each byte is two hex digits");
        u8::from_str_radix(p, 16).expect("valid hex");
    }
    assert!(mac.starts_with("02:"), "locally-administered prefix 02, got {mac}");
}

#[test]
fn random_mac_unique() {
    crate::pin_test_dir();
    let a = random_mac();
    let b = random_mac();
    assert_ne!(a, b, "two calls should produce different MACs");
}

// ── info_plist ──────────────────────────────────────────────────────

#[test]
fn info_plist_contains_device_id() {
    crate::pin_test_dir();
    let plist = info_plist("AA:BB:CC:DD:EE:FF", "TestBox");
    assert!(plist.contains("<key>deviceID</key>"));
    assert!(plist.contains("<string>AA:BB:CC:DD:EE:FF</string>"));
}

#[test]
fn info_plist_features_is_59() {
    crate::pin_test_dir();
    let plist = info_plist("AA:BB:CC:DD:EE:FF", "TestBox");
    assert!(plist.contains("<key>features</key>"));
    let expected = format!("<integer>{}</integer>", FEATURES);
    assert!(plist.contains(&expected), "features must be {FEATURES} (0x{FEATURES:02X})");
    assert_eq!(FEATURES, 0x01 | 0x02 | 0x08 | 0x10 | 0x20);
}

#[test]
fn info_plist_contains_name_and_model() {
    crate::pin_test_dir();
    let plist = info_plist("AA:BB:CC:DD:EE:FF", "My Macast");
    assert!(plist.contains("<key>model</key><string>Macast</string>"));
    assert!(plist.contains("<key>name</key><string>My Macast</string>"));
    assert!(plist.contains("<key>protocolVersion</key><string>1.0</string>"));
    assert!(plist.contains("<key>sourceVersion</key><string>320.20</string>"));
}

// ── parse_content_location ──────────────────────────────────────────

#[test]
fn parse_content_location_from_header() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("Content-Location".to_string(), "<http://example.com/v.mp4>".to_string());
    let url = parse_content_location(&h, "");
    assert_eq!(url, Some("http://example.com/v.mp4".to_string()));
}

#[test]
fn parse_content_location_header_no_angle_brackets() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("Content-Location".to_string(), "http://example.com/v.mp4".to_string());
    let url = parse_content_location(&h, "");
    assert_eq!(url, Some("http://example.com/v.mp4".to_string()));
}

#[test]
fn parse_content_location_from_sdp_body() {
    crate::pin_test_dir();
    let h = HashMap::new();
    let body = "v=0\r\na=content-location:http://example.com/v.mp4\r\n";
    let url = parse_content_location(&h, body);
    assert_eq!(url, Some("http://example.com/v.mp4".to_string()));
}

#[test]
fn parse_content_location_header_wins_over_body() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("Content-Location".to_string(), "http://header.url".to_string());
    let body = "a=content-location:http://body.url\r\n";
    let url = parse_content_location(&h, body);
    assert_eq!(url, Some("http://header.url".to_string()));
}

#[test]
fn parse_content_location_missing() {
    crate::pin_test_dir();
    let h = HashMap::new();
    assert_eq!(parse_content_location(&h, "v=0\r\n"), None);
}

#[test]
fn parse_content_location_empty_header() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("Content-Location".to_string(), "  ".to_string());
    assert_eq!(parse_content_location(&h, ""), None);
}

// ── reason_phrase ───────────────────────────────────────────────────

#[test]
fn reason_phrase_known() {
    crate::pin_test_dir();
    assert_eq!(reason_phrase(200), "OK");
    assert_eq!(reason_phrase(454), "Session Not Found");
    assert_eq!(reason_phrase(501), "Not Implemented");
}

#[test]
fn reason_phrase_unknown() {
    crate::pin_test_dir();
    assert_eq!(reason_phrase(999), "Unknown");
}

// ── RTSP request parsing ────────────────────────────────────────────

#[test]
fn read_request_basic() {
    crate::pin_test_dir();
    let raw = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n";
    let mut reader = BufReader::new(&raw[..]);
    let req = read_request(&mut reader).unwrap().unwrap();
    assert_eq!(req.method, "OPTIONS");
    assert_eq!(req.uri, "*");
    assert_eq!(get_header(&req.headers, "CSeq"), Some("1"));
    assert_eq!(req.body, "");
}

#[test]
fn read_request_with_body() {
    crate::pin_test_dir();
    let raw = b"ANNOUNCE rtsp://x RTSP/1.0\r\nContent-Length: 5\r\nCSeq: 2\r\n\r\nhello";
    let mut reader = BufReader::new(&raw[..]);
    let req = read_request(&mut reader).unwrap().unwrap();
    assert_eq!(req.method, "ANNOUNCE");
    assert_eq!(req.body, "hello");
    assert_eq!(get_header(&req.headers, "Content-Length"), Some("5"));
}

#[test]
fn read_request_eof_returns_none() {
    crate::pin_test_dir();
    let raw = b"";
    let mut reader = BufReader::new(&raw[..]);
    assert!(read_request(&mut reader).unwrap().is_none());
}

#[test]
fn read_request_malformed_line() {
    crate::pin_test_dir();
    let raw = b"GARBAGE\r\n\r\n";
    let mut reader = BufReader::new(&raw[..]);
    assert!(read_request(&mut reader).is_err());
}

#[test]
fn read_request_eof_in_headers() {
    crate::pin_test_dir();
    let raw = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n";
    let mut reader = BufReader::new(&raw[..]);
    assert!(read_request(&mut reader).is_err());
}

#[test]
fn read_request_non_numeric_content_length() {
    crate::pin_test_dir();
    let raw = b"ANNOUNCE rtsp://x RTSP/1.0\r\nContent-Length: abc\r\n\r\n";
    let mut reader = BufReader::new(&raw[..]);
    assert!(read_request(&mut reader).is_err());
}

// ── format_response ─────────────────────────────────────────────────

#[test]
fn format_response_structure() {
    crate::pin_test_dir();
    let mut h = HashMap::new();
    h.insert("CSeq".to_string(), "1".to_string());
    let resp = format_response(200, &h, "body");
    assert!(resp.starts_with("RTSP/1.0 200 OK\r\n"));
    assert!(resp.contains("CSeq: 1\r\n"));
    assert!(resp.contains("Content-Length: 4\r\n"));
    assert!(resp.ends_with("\r\nbody"));
}

#[test]
fn format_response_empty_body() {
    crate::pin_test_dir();
    let h = HashMap::new();
    let resp = format_response(404, &h, "");
    assert!(resp.starts_with("RTSP/1.0 404 Not Found\r\n"));
    assert!(resp.contains("Content-Length: 0\r\n"));
}

// ── handle_request: OPTIONS ─────────────────────────────────────────

#[test]
fn options_returns_public_methods() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("OPTIONS", "*", &[("CSeq", "1")], "");
    let (code, headers, body) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    let public = get_header(&headers, "Public").unwrap();
    for m in SUPPORTED_METHODS {
        assert!(public.contains(m), "Public must list {m}");
    }
    assert_eq!(body, "");
}

// ── handle_request: GET /info ───────────────────────────────────────

#[test]
fn get_info_returns_plist() {
    crate::pin_test_dir();
    let state = AirPlayState::new("TestBox".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("GET", "/info", &[], "");
    let (code, headers, body) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(get_header(&headers, "Content-Type"), Some("text/x-apple-plist+xml"));
    assert!(body.contains("<key>deviceID</key>"));
    assert!(body.contains(&state.device_id));
    assert!(body.contains("TestBox"));
}

// ── handle_request: ANNOUNCE ────────────────────────────────────────

#[test]
fn announce_with_content_location() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://media.url/v.mp4"), ("CSeq", "1")],
        "",
    );
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(
        *state.current_url.lock().unwrap(),
        Some("http://media.url/v.mp4".to_string())
    );
}

#[test]
fn announce_without_content_location_refused() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("ANNOUNCE", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 400, "ANNOUNCE without Content-Location must be 400");
    assert_eq!(*state.current_url.lock().unwrap(), None);
}

// ── handle_request: SETUP ───────────────────────────────────────────

#[test]
fn setup_increments_session() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("SETUP", "rtsp://x", &[("CSeq", "1")], "");
    let (code, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    let session = get_header(&headers, "Session").unwrap();
    assert_eq!(session, "1");
    assert_eq!(*state.session.lock().unwrap(), 1);
    let transport = get_header(&headers, "Transport").unwrap();
    assert!(transport.contains("RTP/AVP/TCP"));
}

#[test]
fn setup_increments_each_time() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    for expected in 1..=3 {
        let req = make_request("SETUP", "rtsp://x", &[("CSeq", &expected.to_string())], "");
        let (_, headers, _) = state.handle_request(&req, renderer.as_ref());
        assert_eq!(get_header(&headers, "Session").unwrap(), expected.to_string());
    }
    assert_eq!(*state.session.lock().unwrap(), 3);
}

// ── handle_request: RECORD ──────────────────────────────────────────

#[test]
fn record_returns_session() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("RECORD", "rtsp://x", &[("CSeq", "1")], "");
    let (code, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(get_header(&headers, "Session").unwrap(), "1");
}

// ── handle_request: PLAY ────────────────────────────────────────────

#[test]
fn play_without_announce_refused() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("PLAY", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 455, "PLAY without prior ANNOUNCE must be 455");
}

#[test]
fn play_after_announce_calls_renderer() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://media.url/video.mp4")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());
    let play = make_request("PLAY", "rtsp://x", &[("CSeq", "2")], "");
    let (code, headers, _) = state.handle_request(&play, renderer.as_ref());
    assert_eq!(code, 200);
    assert!(get_header(&headers, "Range").unwrap().starts_with("npt="));
    assert!(get_header(&headers, "RTP-Info").unwrap().contains("seq=0"));
    let urls = renderer.urls.lock().unwrap();
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0], "http://media.url/video.mp4");
}

#[test]
fn play_sets_title_from_url() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://media.url/path/my_video.mp4")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());
    let play = make_request("PLAY", "rtsp://x", &[("CSeq", "2")], "");
    state.handle_request(&play, renderer.as_ref());
    let titles = renderer.titles.lock().unwrap();
    assert_eq!(titles.len(), 1);
    assert_eq!(titles[0], "my_video.mp4");
}

// ── handle_request: PAUSE ───────────────────────────────────────────

#[test]
fn pause_calls_renderer() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("PAUSE", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(*renderer.pauses.lock().unwrap(), 1);
}

// ── handle_request: SET_PARAMETER ───────────────────────────────────

#[test]
fn set_parameter_volume() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("SET_PARAMETER", "rtsp://x", &[("CSeq", "1")], "volume: 0.75");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    let vols = renderer.volumes.lock().unwrap();
    assert_eq!(vols.len(), 1);
    assert_eq!(vols[0], 75, "0.75 * 100 = 75");
}

#[test]
fn set_parameter_volume_zero() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("SET_PARAMETER", "rtsp://x", &[("CSeq", "1")], "volume: 0.0");
    state.handle_request(&req, renderer.as_ref());
    assert_eq!(renderer.volumes.lock().unwrap()[0], 0);
}

#[test]
fn set_parameter_volume_full() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("SET_PARAMETER", "rtsp://x", &[("CSeq", "1")], "volume: 1.0");
    state.handle_request(&req, renderer.as_ref());
    assert_eq!(renderer.volumes.lock().unwrap()[0], 100);
}

#[test]
fn set_parameter_non_volume_is_ok() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("SET_PARAMETER", "rtsp://x", &[("CSeq", "1")], "something: else");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert!(renderer.volumes.lock().unwrap().is_empty());
}

// ── handle_request: STOP / TEARDOWN ─────────────────────────────────

#[test]
fn stop_calls_renderer_and_clears_url() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://media.url/v.mp4")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());
    assert!(state.current_url.lock().unwrap().is_some());
    let stop = make_request("STOP", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, _) = state.handle_request(&stop, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(*renderer.stops.lock().unwrap(), 1);
    assert_eq!(*state.current_url.lock().unwrap(), None);
}

#[test]
fn teardown_same_as_stop() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("TEARDOWN", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(*renderer.stops.lock().unwrap(), 1);
}

// ── handle_request: GET_PARAMETER ───────────────────────────────────

#[test]
fn get_parameter_returns_volume() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("GET_PARAMETER", "rtsp://x", &[("CSeq", "1")], "");
    let (code, _, body) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert!(body.contains("volume:"));
}

// ── handle_request: FLUSH ───────────────────────────────────────────

#[test]
fn flush_returns_session() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("FLUSH", "rtsp://x", &[("CSeq", "1")], "");
    let (code, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 200);
    assert_eq!(get_header(&headers, "Session").unwrap(), "1");
}

// ── handle_request: unsupported method ──────────────────────────────

#[test]
fn unsupported_method_returns_501_with_allow() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("DESCRIBE", "rtsp://x", &[("CSeq", "1")], "");
    let (code, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(code, 501);
    let allow = get_header(&headers, "Allow").unwrap();
    for m in SUPPORTED_METHODS {
        assert!(allow.contains(m), "Allow must list {m}");
    }
}

// ── CSeq echo ───────────────────────────────────────────────────────

#[test]
fn cseq_echoed_when_present() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("OPTIONS", "*", &[("CSeq", "42")], "");
    let (_, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(get_header(&headers, "CSeq"), Some("42"));
}

#[test]
fn cseq_not_invented_when_absent() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let req = make_request("OPTIONS", "*", &[], "");
    let (_, headers, _) = state.handle_request(&req, renderer.as_ref());
    assert_eq!(get_header(&headers, "CSeq"), None, "must not invent CSeq");
}

// ── stale session ───────────────────────────────────────────────────

#[test]
fn stale_session_detected() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let setup = make_request("SETUP", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&setup, renderer.as_ref());
    assert_eq!(*state.session.lock().unwrap(), 1);

    let mut headers = HashMap::new();
    headers.insert("Session".to_string(), "999".to_string());
    assert_eq!(state.stale_session(&headers), Some(454));
}

#[test]
fn stale_session_with_parameters() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let setup = make_request("SETUP", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&setup, renderer.as_ref());

    let mut headers = HashMap::new();
    headers.insert("Session".to_string(), "1;timeout=60".to_string());
    assert_eq!(state.stale_session(&headers), None, "session 1 with params is OK");
}

#[test]
fn no_session_header_is_ok() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let setup = make_request("SETUP", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&setup, renderer.as_ref());

    let headers = HashMap::new();
    assert_eq!(state.stale_session(&headers), None, "no Session header is OK");
}

#[test]
fn session_zero_always_ok() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let mut headers = HashMap::new();
    headers.insert("Session".to_string(), "999".to_string());
    assert_eq!(state.stale_session(&headers), None, "before SETUP any session is OK");
}

#[test]
fn play_with_stale_session() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let setup = make_request("SETUP", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&setup, renderer.as_ref());

    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://media.url/v.mp4")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());

    let play = make_request("PLAY", "rtsp://x", &[("CSeq", "2"), ("Session", "999")], "");
    let (code, _, _) = state.handle_request(&play, renderer.as_ref());
    assert_eq!(code, 454, "stale session on PLAY must be 454");
    assert!(renderer.urls.lock().unwrap().is_empty(), "renderer must not be called");
}

// ── derive_title_from_url (tested indirectly via PLAY) ──────────────

#[test]
fn title_from_url_with_path() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "http://cdn.example.com/path/to/clip.mp4")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());
    let play = make_request("PLAY", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&play, renderer.as_ref());
    assert_eq!(renderer.titles.lock().unwrap()[0], "clip.mp4");
}

#[test]
fn title_from_bare_url() {
    crate::pin_test_dir();
    let state = AirPlayState::new("Test".into());
    let renderer = Arc::new(VolumeCapture::new());
    let ann = make_request(
        "ANNOUNCE",
        "rtsp://x",
        &[("Content-Location", "not-a-real-url")],
        "",
    );
    state.handle_request(&ann, renderer.as_ref());
    let play = make_request("PLAY", "rtsp://x", &[("CSeq", "1")], "");
    state.handle_request(&play, renderer.as_ref());
    assert_eq!(renderer.titles.lock().unwrap()[0], "not-a-real-url");
}

// ── AirPlayReceiver ─────────────────────────────────────────────────

#[test]
fn receiver_start_and_stop() {
    crate::pin_test_dir();
    let mut recv = AirPlayReceiver::new("TestBox".into());
    let port = recv.start().expect("bind");
    assert!(port >= AIRPLAY_PORT && port < AIRPLAY_PORT + PORT_FALLBACK_RANGE);
    assert_eq!(recv.port(), port);
    assert!(recv.device_id().starts_with("02:"));
    recv.stop();
    assert!(!recv.state.running.load(Ordering::SeqCst));
}

#[test]
fn receiver_port_fallback_when_occupied() {
    crate::pin_test_dir();
    let blocker = TcpListener::bind(("127.0.0.1", AIRPLAY_PORT))
        .expect("bind blocker on default port");
    let mut recv = AirPlayReceiver::new("TestBox".into());
    let port = recv.start().expect("bind on fallback");
    assert_ne!(port, AIRPLAY_PORT, "must fall back to next port");
    assert!(port > AIRPLAY_PORT);
    recv.stop();
    drop(blocker);
}

#[test]
fn receiver_serve_accepts_connection() {
    crate::pin_test_dir();
    let mut recv = AirPlayReceiver::new("TestBox".into());
    let port = recv.start().expect("bind");
    let renderer = Arc::new(VolumeCapture::new());
    let renderer_clone = renderer.clone();
    let state_for_serve = recv.state.clone();
    let running = recv.state.running.load(Ordering::SeqCst);
    assert!(running, "receiver must be running after start");

    let listener = recv.listener.take().unwrap();
    let _ = listener.set_nonblocking(true);

    let client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let _ = client.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let mut w = std::io::BufWriter::new(&client);
    w.write_all(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n")
        .expect("write");
    w.flush().expect("flush");

    let accept_result = listener.accept();
    if let Ok((stream, _)) = accept_result {
        handle_connection(stream, state_for_serve, renderer_clone);
    }

    recv.stop();
}

#[test]
fn receiver_current_url_initially_none() {
    crate::pin_test_dir();
    let recv = AirPlayReceiver::new("TestBox".into());
    assert_eq!(recv.current_url(), None);
    assert_eq!(recv.session(), 0);
}

// ── SUPPORTED_METHODS and FEATURES constants ────────────────────────

#[test]
fn supported_methods_contains_core() {
    crate::pin_test_dir();
    for m in &["OPTIONS", "ANNOUNCE", "SETUP", "PLAY", "PAUSE", "STOP", "TEARDOWN"] {
        assert!(SUPPORTED_METHODS.contains(m), "{m} must be in SUPPORTED_METHODS");
    }
}

#[test]
fn features_no_drm_no_mirroring() {
    crate::pin_test_dir();
    assert_eq!(FEATURES & 0x04, 0, "no FairPlay/DRM bit");
    assert_eq!(FEATURES & 0x40, 0, "no screen mirroring bit (0x40)");
}

// ── every test pins the log directory ───────────────────────────────

#[test]
fn every_test_pins_the_log_directory_first() {
    crate::pin_test_dir();
    let src = include_str!("airplay_tests.rs");
    let mut test_count = 0;
    let mut pinned_count = 0;
    let mut in_test = false;
    let mut brace_depth: usize = 0;
    let mut found_pin = false;
    let mut first_statement_checked = false;

    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed == "#[test]" {
            in_test = true;
            found_pin = false;
            first_statement_checked = false;
            continue;
        }
        if in_test && trimmed.starts_with("fn ") && trimmed.contains('(') {
            test_count += 1;
            continue;
        }
        if in_test {
            if trimmed.starts_with('{') {
                brace_depth += 1;
            }
            if trimmed.starts_with('}') {
                if brace_depth == 0 {
                    if !found_pin {
                        panic!("test function did not call pin_test_dir()");
                    }
                    pinned_count += 1;
                    in_test = false;
                }
                brace_depth = brace_depth.saturating_sub(1);
            }
            if !first_statement_checked && !trimmed.is_empty() && trimmed != "{" && !trimmed.starts_with("//") {
                first_statement_checked = true;
                if trimmed.contains("pin_test_dir()") {
                    found_pin = true;
                }
            }
        }
    }
    assert!(test_count > 10, "expected at least 10 tests, found {test_count}");
    assert_eq!(pinned_count, test_count, "every test must call pin_test_dir()");
}
