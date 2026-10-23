// AirPlay RTSP receiver — the video URL-cast path.
//
// Makes Macast appear as an AirPlay target so an iPhone / iPad / macOS can
// "AirPlay" a video to the computer. AirPlay also supports screen mirroring
// and audio-only (RAOP), but those require real-time encode / ALAC decode and
// are OUT OF SCOPE here. We implement the *video URL-cast* flow, which is the
// same "sender gives a URL, receiver plays it" model as DLNA/Chromecast:
//
//   mDNS  : _airplay._tcp.local  (port 7000)   -- sender discovery
//   RTSP  : port 7000            (OPTIONS / ANNOUNCE / SETUP / PLAY / ...)
//           ANNOUNCE carries Content-Location (the media URL) -> mpv
//
// Rust counterpart of `macast/protocol_airplay.py`.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macast_core::logging::Logger;
use rand::Rng;

/// The logger this module writes through, under the name the reference gives it
/// (`protocol_airplay.py:29` — `logging.getLogger("AirPlay")`).
///
/// It is **not** claimed, so the records land in `macast.log` exactly where
/// Python's go. Until 2026-10-18 this module used the `log` facade's free
/// macros, and nothing in the workspace installed a sink for them — every line
/// below, including the port-fallback warning a user needs when macOS's own
/// AirPlay Receiver has taken 7000, was written to nowhere.
fn logger() -> &'static std::sync::Arc<Logger> {
    static SELF: std::sync::OnceLock<std::sync::Arc<Logger>> = std::sync::OnceLock::new();
    SELF.get_or_init(|| Logger::named("AirPlay"))
}

/// How long the accept loop may sit out before it looks at the stop flag again.
///
/// The same budget `channel.rs` and `announce.rs` settled on: long enough that
/// polling is not a busy loop, short enough that a service stop is measured in
/// milliseconds rather than "whenever the next sender connects".
pub const ACCEPT_POLL: Duration = Duration::from_millis(200);

/// Default RTSP listen port. macOS ships its own AirPlay receiver that
/// occupies 7000; we try a few consecutive ports before giving up.
pub const AIRPLAY_PORT: u16 = 7000;

/// How many consecutive ports to try when the default is occupied.
pub const PORT_FALLBACK_RANGE: u16 = 20;

/// mDNS service type for AirPlay.
pub const AIRPLAY_SERVICE: &str = "_airplay._tcp.local.";

/// AirPlay "features" bitmask. Only advertise what we actually implement --
/// claiming a bit makes iOS offer a feature that would then fail.
///   0x01  Video
///   0x02  Photo
///   0x08  VideoVolumeControl
///   0x10  VideoHTTPLiveStreams
///   0x20  Slideshow
/// Deliberately NOT advertised: VideoFairPlay (0x04, no DRM decryption),
/// screen mirroring, AirPlay audio/RAOP, and AirPlay 2 multi-room.
pub const FEATURES: u32 = 0x01 | 0x02 | 0x08 | 0x10 | 0x20;

/// The RTSP methods we actually implement. One array, used both to advertise
/// `Public`/`Allow` and to decide whether a request is answerable, so the
/// two can never drift apart.
pub const SUPPORTED_METHODS: &[&str] = &[
    "OPTIONS",
    "ANNOUNCE",
    "SETUP",
    "RECORD",
    "PLAY",
    "PAUSE",
    "FLUSH",
    "STOP",
    "TEARDOWN",
    "SET_PARAMETER",
    "GET_PARAMETER",
];

/// RTSP status codes -> reason phrase (RFC 2326 section 12).
pub const REASON_PHRASES: &[(u16, &str)] = &[
    (200, "OK"),
    (400, "Bad Request"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (454, "Session Not Found"),
    (455, "Method Not Valid In This State"),
    (500, "Internal Server Error"),
    (501, "Not Implemented"),
];

/// How long an idle control connection may sit before we reclaim it. RTSP
/// clients keep the socket open between commands, so this must be generous;
/// its purpose is only to stop a sender that vanished without a FIN (Wi-Fi
/// drop, app killed) from parking a thread and an fd forever.
pub const IDLE_TIMEOUT_SECONDS: f64 = 120.0;

/// Look up a header by name, case-insensitively.
///
/// RTSP header names are case-insensitive, but the parser builds a plain
/// `HashMap<String, String>` verbatim from the wire. A client sending
/// `cseq:` (or `content-location:`) therefore missed every lookup and the
/// reply carried a fabricated `CSeq: 1`, leaving the client's sequence
/// numbers permanently out of step.
pub fn get_header<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    // Fast path: exact match.
    if let Some(v) = headers.get(name) {
        return Some(v.as_str());
    }
    // Slow path: case-insensitive scan.
    let wanted = name.to_ascii_lowercase();
    for (k, v) in headers {
        if k.to_ascii_lowercase() == wanted {
            return Some(v.as_str());
        }
    }
    None
}

/// Generate a stable, locally-administered MAC for the device-id.
pub fn random_mac() -> String {
    let mut rng = rand::thread_rng();
    let mac = [
        0x02u8,
        0x00,
        0x00,
        rng.r#gen(),
        rng.r#gen(),
        rng.r#gen(),
    ];
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Build the device capability plist served on GET /info.
pub fn info_plist(device_id: &str, friendly_name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>deviceID</key><string>{}</string>
<key>features</key><integer>{}</integer>
<key>model</key><string>Macast</string>
<key>name</key><string>{}</string>
<key>protocolVersion</key><string>1.0</string>
<key>sourceVersion</key><string>320.20</string>
</dict></plist>"#,
        device_id, FEATURES, friendly_name
    )
}

/// Parse Content-Location from either the header or the SDP body.
///
/// Header form: `Content-Location: <url>`
/// SDP body form: `a=content-location:<url>`
pub fn parse_content_location(headers: &HashMap<String, String>, body: &str) -> Option<String> {
    // Header form first.
    if let Some(cl) = get_header(headers, "Content-Location") {
        let trimmed = cl.trim().trim_start_matches('<').trim_end_matches('>');
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // SDP body form.
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.to_ascii_lowercase().starts_with("a=content-location:") {
            let url = trimmed.splitn(2, ':').nth(1)?.trim().trim_start_matches('<').trim_end_matches('>');
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Look up the reason phrase for an RTSP status code.
pub fn reason_phrase(code: u16) -> &'static str {
    REASON_PHRASES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, r)| *r)
        .unwrap_or("Unknown")
}

/// A parsed RTSP request.
#[derive(Debug)]
pub struct RtspRequest {
    pub method: String,
    pub uri: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Parse one RTSP request from a buffered reader.
///
/// Returns `Ok(None)` on clean EOF (client closed the connection).
/// Returns `Err(_)` on malformed input or I/O error.
pub fn read_request<R: BufRead>(reader: &mut R) -> std::io::Result<Option<RtspRequest>> {
    // Read the request line.
    let mut request_line = String::new();
    let bytes_read = reader.read_line(&mut request_line)?;
    if bytes_read == 0 {
        return Ok(None); // Clean EOF.
    }
    let request_line = request_line.trim_end_matches(|c| c == '\r' || c == '\n');

    // Read headers until blank line.
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF in headers",
            ));
        }
        let line = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    // Read body if Content-Length present.
    let content_length: usize = get_header(&headers, "Content-Length")
        .unwrap_or("0")
        .parse()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "non-numeric Content-Length",
            )
        })?;
    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf)?;
        body = String::from_utf8_lossy(&buf).into_owned();
    }

    // Parse request line: "METHOD URI RTSP/1.0"
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "malformed request line",
        ));
    }

    Ok(Some(RtspRequest {
        method: parts[0].to_ascii_uppercase(),
        uri: parts[1].to_string(),
        headers,
        body,
    }))
}

/// Format an RTSP response.
pub fn format_response(code: u16, headers: &HashMap<String, String>, body: &str) -> String {
    let reason = reason_phrase(code);
    let mut out = format!("RTSP/1.0 {} {}\r\n", code, reason);
    for (k, v) in headers {
        let _ = write!(out, "{}: {}\r\n", k, v);
    }
    let _ = write!(out, "Content-Length: {}\r\n", body.len());
    out.push_str("\r\n");
    out.push_str(body);
    out
}

/// Callback trait for renderer commands.
///
/// The AirPlay receiver calls these methods when the sender issues PLAY,
/// PAUSE, STOP, etc. The implementation bridges to the mpv renderer.
pub trait RendererCallback: Send + Sync {
    fn set_media_url(&self, url: &str);
    fn set_media_title(&self, title: &str);
    fn set_media_pause(&self);
    fn set_media_stop(&self);
    fn set_media_volume(&self, volume: i32);
}

/// Shared state for an AirPlay receiver session.
pub struct AirPlayState {
    /// Session counter, incremented on each SETUP.
    pub session: Mutex<u32>,
    /// The URL announced by the sender (from ANNOUNCE's Content-Location).
    pub current_url: Mutex<Option<String>>,
    /// The title announced by the sender (if any).
    pub current_title: Mutex<Option<String>>,
    /// Whether the receiver is running.
    pub running: AtomicBool,
    /// The device ID (a locally-administered MAC).
    pub device_id: String,
    /// The friendly name advertised over mDNS.
    pub friendly_name: String,
    /// The actual bound port (may differ from AIRPLAY_PORT if fallback occurred).
    pub port: Mutex<u16>,
}

impl AirPlayState {
    pub fn new(friendly_name: String) -> Self {
        Self {
            session: Mutex::new(0),
            current_url: Mutex::new(None),
            current_title: Mutex::new(None),
            running: AtomicBool::new(false),
            device_id: random_mac(),
            friendly_name,
            port: Mutex::new(AIRPLAY_PORT),
        }
    }

    /// Check if the client's Session header names a stale session.
    ///
    /// Returns `Some(454)` if stale, `None` if OK.
    ///
    /// A `Session` value may carry parameters (`1;timeout=60`); only the id
    /// before the semicolon identifies the session, and comparing the raw
    /// string rejected legal requests. A request that names no session at all
    /// is accepted: before SETUP there is nothing to compare against, and being
    /// strict there would break clients that PLAY without one.
    pub fn stale_session(&self, headers: &HashMap<String, String>) -> Option<u16> {
        let session = self.session.lock().unwrap();
        if *session == 0 {
            return None;
        }
        let sent = match get_header(headers, "Session") {
            Some(s) => s,
            None => return None,
        };
        let session_id = sent.split(';').next().unwrap_or("").trim();
        if session_id.is_empty() {
            return None;
        }
        if session_id != session.to_string() {
            logger().info(&format!(
                "AirPlay stale session {:?} (ours is {})",
                session_id, *session
            ));
            return Some(454);
        }
        None
    }

    /// Handle an RTSP request and return the response.
    pub fn handle_request(
        &self,
        req: &RtspRequest,
        renderer: &dyn RendererCallback,
    ) -> (u16, HashMap<String, String>, String) {
        let mut headers = HashMap::new();
        // Echo the client's CSeq back if it sent one -- and only if. Inventing
        // `CSeq: 1` for a request that arrived without one desynchronises the
        // client's sequence bookkeeping for the rest of the connection.
        if let Some(cseq) = get_header(&req.headers, "CSeq") {
            headers.insert("CSeq".to_string(), cseq.to_string());
        }

        // GET /info serves device capabilities.
        if req.method == "GET" && req.uri.starts_with("/info") {
            headers.insert(
                "Content-Type".to_string(),
                "text/x-apple-plist+xml".to_string(),
            );
            let body = info_plist(&self.device_id, &self.friendly_name);
            return (200, headers, body);
        }

        match req.method.as_str() {
            "OPTIONS" => {
                headers.insert(
                    "Public".to_string(),
                    SUPPORTED_METHODS.join(", "),
                );
                // In AirPlay the *client* sends Apple-Challenge and the receiver
                // answers with a signed Apple-Response. We have no signing key,
                // so we deliberately do not promise one.
                if get_header(&req.headers, "Apple-Challenge").is_some() {
                    logger().info("Client sent Apple-Challenge; signature auth is not implemented, continuing without Apple-Response");
                }
                (200, headers, String::new())
            }

            "FLUSH" => {
                let session = self.session.lock().unwrap();
                let session_val = (*session).max(1);
                headers.insert("Session".to_string(), session_val.to_string());
                (200, headers, String::new())
            }

            "ANNOUNCE" => {
                let url = parse_content_location(&req.headers, &req.body);
                if url.is_none() {
                    // Answering 200 here promised a play that could never happen,
                    // and the PLAY that followed reported Range/RTP-Info for a
                    // stream that did not exist.
                    logger().warning("AirPlay ANNOUNCE carried no Content-Location; refusing rather than pretending to load");
                    return (400, headers, String::new());
                }
                let url = url.unwrap();
                logger().info(&format!("AirPlay ANNOUNCE url={}", url));
                *self.current_url.lock().unwrap() = Some(url);
                (200, headers, String::new())
            }

            "SETUP" => {
                let mut session = self.session.lock().unwrap();
                *session += 1;
                let session_val = *session;
                headers.insert("Session".to_string(), session_val.to_string());
                headers.insert(
                    "Transport".to_string(),
                    "RTP/AVP/TCP;unicast;interleaved=0-1".to_string(),
                );
                (200, headers, String::new())
            }

            "RECORD" => {
                let session = self.session.lock().unwrap();
                let session_val = (*session).max(1);
                headers.insert("Session".to_string(), session_val.to_string());
                (200, headers, String::new())
            }

            "PLAY" => {
                if let Some(code) = self.stale_session(&req.headers) {
                    return (code, headers, String::new());
                }
                let url = self.current_url.lock().unwrap().clone();
                if url.is_none() {
                    // Nothing was announced: 455 rather than a 200 that claims
                    // playback started.
                    logger().warning("AirPlay PLAY without a prior ANNOUNCE url");
                    return (455, headers, String::new());
                }
                let url = url.unwrap();
                renderer.set_media_url(&url);
                // mpv keeps the name it was given across a `loadfile`, so a PLAY that
                // only changes the address leaves the *previous* item's title on the
                // window. AirPlay's ANNOUNCE carries a URL and no title, so the address
                // is the honest answer.
                let title = title_for_play(&url);
                renderer.set_media_title(&title);
                let session = self.session.lock().unwrap();
                let session_val = (*session).max(1);
                headers.insert("Session".to_string(), session_val.to_string());
                headers.insert("Range".to_string(), "npt=now-".to_string());
                headers.insert("RTP-Info".to_string(), "seq=0;rtptime=0".to_string());
                (200, headers, String::new())
            }

            "PAUSE" => {
                if let Some(code) = self.stale_session(&req.headers) {
                    return (code, headers, String::new());
                }
                renderer.set_media_pause();
                let session = self.session.lock().unwrap();
                let session_val = (*session).max(1);
                headers.insert("Session".to_string(), session_val.to_string());
                (200, headers, String::new())
            }

            "SET_PARAMETER" => {
                if let Some(code) = self.stale_session(&req.headers) {
                    return (code, headers, String::new());
                }
                if req.body.to_ascii_lowercase().contains("volume:") {
                    // SET_PARAMETER with 'volume: <0..100>' -> renderer volume.
                    // The body is "volume: <float>" where float is 0.0-1.0 scale,
                    // but we multiply by 100 to get 0-100 for the renderer.
                    if let Some(vol_str) = req.body.split("volume:").nth(1) {
                        match vol_str.trim().parse::<f64>() {
                            Ok(vol) => {
                                let volume = (vol * 100.0) as i32;
                                // Said out loud, because this is the only place the
                                // sender's number becomes the player's number, and
                                // mpv does **not** emit a `property-change` event when
                                // the value it already holds is set again (measured
                                // 2026-10-18: a second SET_PARAMETER of the same 0.5
                                // left the log with one 50.0 line while the property
                                // read 50.0 both times). A check that watches mpv's
                                // events therefore sees a real volume change go
                                // unmentioned; this line is the durable half.
                                logger().info(&format!(
                                    "AirPlay SET_PARAMETER volume {} -> renderer volume {}",
                                    vol_str.trim(),
                                    volume
                                ));
                                renderer.set_media_volume(volume);
                            }
                            Err(_) => {
                                // 200 is still the right reply (the reference gives one
                                // too), but the reason nothing moved has to be readable:
                                // "the sender asked for a volume we could not parse" is
                                // a different fact from "we ignored the request".
                                logger().warning(&format!(
                                    "AirPlay SET_PARAMETER carried a volume this receiver could not read: {:?}",
                                    vol_str.trim()
                                ));
                            }
                        }
                    }
                }
                let session = self.session.lock().unwrap();
                let session_val = (*session).max(1);
                headers.insert("Session".to_string(), session_val.to_string());
                (200, headers, String::new())
            }

            "STOP" | "TEARDOWN" => {
                renderer.set_media_stop();
                *self.current_url.lock().unwrap() = None;
                (200, headers, String::new())
            }

            "GET_PARAMETER" => (200, headers, "volume: 100\r\n".to_string()),

            _ => {
                // Anything else: say so instead of answering 200 to a request we never
                // performed. `Allow` tells the client what it may ask for.
                logger().info(&format!("AirPlay unsupported RTSP method {:?} from a client", req.method));
                headers.insert("Allow".to_string(), SUPPORTED_METHODS.join(", "));
                (501, headers, String::new())
            }
        }
    }
}

/// Derive a title from a URL when the sender didn't provide one.
///
/// AirPlay's ANNOUNCE carries a URL and no title, so the address is the honest
/// answer. This extracts the last path segment as a fallback title.
/// What to call the stream when the sender named nothing — **the same function the
/// page and the Cast channel use** ([`crate::resolve::title_for_uri`], whose answer
/// is frozen against CPython's `media_resolve.title_for_uri`).
///
/// This module used to carry its own three-line version: last path segment, kept
/// with its container suffix. That difference is user-visible and was never asked
/// of the reference — an iPhone casting `…/clip.mp4` got a window titled `clip.mp4`
/// while the same file cast from the page got `clip`, which is AGENTS.md §4.2's
/// 「两处答案不一致比没有数字更糟」 with two owners of one rule.
///
/// The `Err` half is a **deliberate divergence**: the reference calls
/// `media_resolve.title_for_uri(url)` unguarded in `PLAY`, and a malformed address
/// (`http://[::1`) raises `ValueError` inside the RTSP handler, so the sender gets
/// **no answer at all** on that request. Handing back the address as the name keeps
/// the reply honest about the media it did accept — and the `fallback` argument is
/// the same string, so the only way this can come back as an error is the case where
/// nothing better exists. Recorded in `docs/Rust-Port-Design.md` §8.
fn title_for_play(url: &str) -> String {
    crate::resolve::title_for_uri(url, url).unwrap_or_else(|_| url.to_string())
}

/// Handle one RTSP connection.
pub fn handle_connection(
    stream: TcpStream,
    state: Arc<AirPlayState>,
    renderer: Arc<dyn RendererCallback>,
) {
    let addr = stream.peer_addr().ok();
    let _ = stream.set_read_timeout(Some(Duration::from_secs_f64(
        IDLE_TIMEOUT_SECONDS,
    )));
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    loop {
        let req = match read_request(&mut reader) {
            Ok(Some(req)) => req,
            Ok(None) => {
                logger().debug(&format!("RTSP connection closed by client {:?}", addr));
                break;
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
                    logger().info(&format!("RTSP idle timeout from {:?}, closing connection", addr));
                } else {
                    logger().debug(&format!("RTSP read error from {:?}: {}", addr, e));
                }
                break;
            }
        };

        let (code, resp_headers, body) = state.handle_request(&req, renderer.as_ref());
        let response = format_response(code, &resp_headers, &body);
        if let Err(e) = writer.write_all(response.as_bytes()) {
            logger().debug(&format!("RTSP write error to {:?}: {}", addr, e));
            break;
        }

        if req.method == "TEARDOWN" {
            break;
        }
    }
}

/// The AirPlay RTSP server.
pub struct AirPlayReceiver {
    state: Arc<AirPlayState>,
    listener: Option<TcpListener>,
}

impl AirPlayReceiver {
    pub fn new(friendly_name: String) -> Self {
        Self {
            state: Arc::new(AirPlayState::new(friendly_name)),
            listener: None,
        }
    }

    /// Start listening on the first available port in the fallback range.
    pub fn start(&mut self) -> std::io::Result<u16> {
        self.start_from(AIRPLAY_PORT)
    }

    /// [`start`](Self::start) with the search starting somewhere else.
    ///
    /// Exists so the fallback behaviour can be tested **without assuming port 7000 is free**.
    /// The old test bound a blocker on 7000 itself, and on a CI runner that bind failed with
    /// `AddrInUse` — the runner already had something there, so the test could not create its own
    /// precondition and went red for a reason that has nothing to do with the code. Starting the
    /// search from a dynamically chosen port tests the same logic on any machine.
    fn start_from(&mut self, first: u16) -> std::io::Result<u16> {
        let last = first.saturating_add(PORT_FALLBACK_RANGE);
        for port in first..last {
            let addr = format!("0.0.0.0:{}", port);
            match TcpListener::bind(&addr) {
                Ok(listener) => {
                    let _ = listener.set_nonblocking(false);
                    *self.state.port.lock().unwrap() = port;
                    self.state.running.store(true, Ordering::SeqCst);
                    self.listener = Some(listener);
                    if port != first {
                        logger().warning(&format!(
                            "AirPlay port {} already in use (macOS AirPlay Receiver?), using {} instead",
                            first, port
                        ));
                    }
                    logger().info(&format!("AirPlay RTSP server started on port {}", port));
                    return Ok(port);
                }
                Err(e) => {
                    logger().debug(&format!("AirPlay bind port {} failed: {}", port, e));
                    continue;
                }
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "no free port in range",
        ))
    }

    /// Serve connections until [`stop`] is called.
    ///
    /// [`stop`]: AirPlayReceiver::stop
    ///
    /// The accept is **polled, not parked**. A blocking `accept` does not look at
    /// the flag, so `stop()` would leave this thread asleep in the kernel holding
    /// the listening socket until some sender happened to connect — which is exactly
    /// the shape "the user switched AirPlay off and port 7000 is still ours" and the
    /// reason `Announcer` polls at [`READ_POLL`] and `channel.rs` refuses to hold a
    /// lock across a blocking read. The first version of this loop was blocking, and
    /// the only test that touched `serve` worked around it by taking the listener out
    /// of the receiver and accepting by hand — a test that cannot fail is worse than
    /// no test, because it says the row is covered.
    ///
    /// Each accepted socket is put back into **blocking** mode on the way out: on
    /// Linux an accepted socket inherits the listener's `O_NONBLOCK`, and
    /// `handle_connection` reads it with a timeout (macOS does not inherit, so the
    /// line is a no-op there and still needed — the same platform pair
    /// `mdns_reuseport_probe.py` measured).
    pub fn serve(&self, renderer: Arc<dyn RendererCallback>) -> std::io::Result<()> {
        let listener = self.listener.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "not started")
        })?;
        listener.set_nonblocking(true)?;

        while self.state.running.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    logger().debug(&format!("AirPlay RTSP connection from {}", addr));
                    if let Err(e) = stream.set_nonblocking(false) {
                        logger().warning(&format!("AirPlay socket from {addr}: {e}"));
                    }
                    let state = Arc::clone(&self.state);
                    let renderer = Arc::clone(&renderer);
                    std::thread::spawn(move || {
                        handle_connection(stream, state, renderer);
                    });
                }
                // Nothing waiting: ask again shortly, and look at the flag first.
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    if !self.state.running.load(Ordering::SeqCst) {
                        break;
                    }
                    // A *single* failed accept (ECONNABORTED, EMFILE) must not take
                    // down the whole server — same trap as Chromecast TLS (§4.1).
                    logger().warning(&format!("AirPlay accept error: {}", e));
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
        Ok(())
    }

    /// Stop the receiver.
    pub fn stop(&self) {
        self.state.running.store(false, Ordering::SeqCst);
        logger().info("AirPlay RTSP server stopped");
    }

    /// Get the actual bound port.
    pub fn port(&self) -> u16 {
        *self.state.port.lock().unwrap()
    }

    /// Get the device ID.
    pub fn device_id(&self) -> &str {
        &self.state.device_id
    }

    /// Get the current URL (if any).
    pub fn current_url(&self) -> Option<String> {
        self.state.current_url.lock().unwrap().clone()
    }

    /// Get the session counter.
    pub fn session(&self) -> u32 {
        *self.state.session.lock().unwrap()
    }
}

#[cfg(test)]
#[path = "airplay_tests.rs"]
mod tests;
