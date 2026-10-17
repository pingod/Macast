//! Witness cases for [`crate::push`] — the page card's "throw this at that
//! device" path, ported from `cast_local_file.py`'s `CastSender` / `DlnaSender`
//! / `push_url_to`.
//!
//! ## Three shapes of witness
//!
//! **Byte-level fixtures.** A literal frame or a literal envelope is the only
//! kind that catches a rewrite of the sentence a user reads when a TV refuses
//! them. The SOAP envelope, the DIDL, and the outgoing `LOAD` body are each
//! compared against a string written out here, not against a re-run of the
//! function that produced them.
//!
//! **A scripted TLS device (one connection per script).** [`CastSender`] speaks
//! CASTV2 over rustls to a localhost listener that replays a fixed list of
//! frames, one per request. That is the only way to witness the *wire*: which
//! namespace each verb travels in, which destination a `LOAD` reaches, and
//! which arm of the two LAUNCH refusals a given reply lands in. The device
//! never guesses an answer — a script that should stay silent stays silent, and
//! the push times out for real.
//!
//! **A real HTTP SOAP stub.** `DlnaSender` rides ureq, so its contract is
//! "one TCP connection per SOAP verb" (a fresh `no_proxy_agent` per call). The
//! stub accepts exactly as many connections as it has replies and records every
//! request line, header and body byte. That is what makes the proxy claim
//! testable: reaching a *localhost* stub at all is the witness that no system
//! proxy is in the path (AGENTS.md §4.2).
//!
//! **The project [`Logger`], one file per test.** Half of this module's
//! promises are sentences — 状态未知, "replied with something that is not
//! XML", the failure line that names the device. A test that only asserts a
//! `Result` shape proves the device disappeared and nothing about whether a
//! human was told. Every test names its own logger so no two read the same
//! file.
//!
//! ## What is *not* tested here, and why
//!
//! Two arms in [`CastSender::connect`] stay unwitnessed, and each says which:
//!
//! 1. `"没有可用的地址"` needs `(host, port).to_socket_addrs()` to return
//!    `Ok` with an **empty** iterator. Every name a caller can hand this
//!    function either resolves or errors; there is no fixture for "resolved to
//!    nothing", so the arm is unreachable from a test, not merely untested.
//! 2. `"目标地址无法作为 TLS 服务器名"` needs `ServerName::try_from` to fail on
//!    a host that already survived `!host.is_empty()` and a successful DNS
//!    lookup — rustls rejects empty and trailing-dot-free-but-invalid names
//!    before resolution, and a name that resolves is a name it accepts.
//!
//! Deviation ⑧'s "one error line per failure instead of `logger.exception`" is
//! a *shape* of the log, not a branch: the failure test asserts the error line
//! exists and names the device; it cannot assert the absence of a traceback
//! that Python's logger would have written and this one never could.
//!
//! Finally, two shapes that this round first recorded as **0 红** have been
//! re-measured against the tests that exist now, and each answers **1 红**:
//! deleting the `?` after `let _ = self.recv()?;` in `connect()` reddens
//! [`a_device_that_never_answers_the_challenge_is_not_a_connected_sender`], and
//! flipping the DIDL gate `duration != 0.0` to `duration > 0.0` reddens
//! [`build_didl_writes_zero_clock_for_an_infinite_duration`] on `-inf`. The
//! readings live in comments at the two sites that catch them — a note about
//! coverage belongs next to the question, not in an `assert!(true)` that asks
//! nothing.

use super::*;

use crate::proto::encode_auth_response;
use crate::targets::TargetItem;
use macast_core::logging::read_tail;
use std::io::BufRead;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::MutexGuard;
use std::thread;

// ---------------------------------------------------------------------------
// Shared scaffolding
// ---------------------------------------------------------------------------

/// The handoff list is process-global, and a *successful* cast push parks its
/// live TLS socket there. Two consequences: tests must not evict each other's
/// senders, and the scripted device never sees EOF until the list is drained —
/// so `join()` would sit out its read timeout. The gate serialises the list;
/// the drop drains it before anyone waits on a device thread.
static PUSH_GATE: Mutex<()> = Mutex::new(());

// Which thread currently owns the ledger, and therefore which drain is *his*.
//
// This exists because the drain used to be unconditional. `ScriptedDevice::finish()`
// has to drain before it joins, and the five refusal tests call `finish()` without
// ever holding the gate — so one of them finishing while the eviction test was
// midway through its four pushes cleared **someone else's** senders, and that test
// read `handoff_count() == 2` after four pushes it had all been told succeeded. The
// filtered `push_tests` run never caught it (fewer threads, different timing); the
// whole crate does. AGENTS.md §4.2's 「并行跑的用例共享夹具路径，单跑绿、整组红」
// family, in a new shape: the shared fixture here is not a path or an id, it is a
// `Mutex<Handoffs>` that lives for the whole process. First question after any
// "single run green, group run red": *is the fixture shared*.
thread_local! {
    static OWNS_PUSH_LEDGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct PushHeld {
    _gate: MutexGuard<'static, ()>,
}

impl Drop for PushHeld {
    fn drop(&mut self) {
        clear_handoffs();
        OWNS_PUSH_LEDGER.with(|owned| owned.set(false));
    }
}

fn hold_push() -> PushHeld {
    let held = PushHeld {
        _gate: PUSH_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    };
    OWNS_PUSH_LEDGER.with(|owned| owned.set(true));
    held
}

/// True only on the thread that holds the ledger. `finish()` asks this before it
/// drains, so a test that owns nothing can no longer close another test's sockets.
fn owns_push_ledger() -> bool {
    OWNS_PUSH_LEDGER.with(|owned| owned.get())
}

fn temp_dir_for(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "macast-cast-push-{}-{}",
        tag,
        crate::cert::unique_slot()
    ))
}

fn setting_file(tag: &str) -> PathBuf {
    macast_core::paths::setting_dir().join(format!("push-{}-{}.json", tag, crate::cert::unique_slot()))
}

fn lines(path: &Path) -> String {
    read_tail(path, 400).text
}

/// One logger, one file, one test — the module's sentences are half the point.
fn witness(name: &str) -> (Arc<Logger>, PathBuf) {
    let logger = Logger::named(name);
    let path = logger.claim();
    (logger, path)
}

// ---------------------------------------------------------------------------
// Harness A — the scripted TLS device
// ---------------------------------------------------------------------------

/// How long a silent device is allowed to keep a connection thread alive.
const DEVICE_READ_TIMEOUT: Duration = Duration::from_secs(3);

struct ScriptedDevice {
    port: u16,
    seen: Arc<Mutex<Vec<CastMessage>>>,
    opened: Arc<AtomicUsize>,
    /// Connection indices whose read loop ended in a *peer* close (EOF), as
    /// opposed to timing out with the sender still holding the socket.
    closed: Arc<Mutex<Vec<usize>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ScriptedDevice {
    fn seen(&self) -> Vec<CastMessage> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    fn closed(&self) -> Vec<usize> {
        self.closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Drain the handoff list *first*, but only when this thread owns it, so the
    /// retained sockets close and each device thread stops waiting on its read
    /// timeout — then take the whole ledger back. A test that does not own the
    /// list must not drain it: the senders in there are somebody else's, and
    /// closing them mid-assertion is the collision described on `PUSH_GATE`.
    /// The temp dir is removed by the acceptor itself, so a test that never
    /// reaches this line still cannot leak it.
    fn finish(self) -> Vec<CastMessage> {
        let taken = self.seen();
        if owns_push_ledger() {
            clear_handoffs();
        }
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
        taken
    }
}

/// Wait for the *device's* ledger to say a connection closed, rather than
/// grabbing the instant after the sender dropped it. AGENTS.md §4.2: a
/// judgement about "that happened" that sits in somebody else's book has to
/// wait for that book. Returns the indices seen so far, so a timeout is
/// reported as a timeout instead of a false negative.
fn wait_closed(device: &ScriptedDevice, want: usize) -> Vec<usize> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let closed = device.closed();
        if closed.len() >= want || Instant::now() >= deadline {
            return closed;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Start a device that serves one connection per script. Each script is the
/// ordered list of frames to write back, one per frame received; a script that
/// runs out of frames goes silent, which is how a timeout gets witnessed.
///
/// One **acceptor** thread takes connections in order — so `closed` records the
/// *arrival* index, which is what "the oldest handoff is the one that pays"
/// means — and hands each connection to its own **serve** thread, which it joins
/// before exiting. Accept-and-serve in one loop would make connection *n* wait
/// for connection *n-1* to time out: a test that holds four sockets in the
/// handoff list and then pushes a fifth would sit a full `DEVICE_READ_TIMEOUT`
/// per socket and read the closure ledger before the oldest one reached it.
fn start_device(scripts: Vec<Vec<Vec<u8>>>) -> ScriptedDevice {
    let dir = temp_dir_for("device");
    std::fs::create_dir_all(&dir).unwrap();
    let log = Logger::named(&format!("PushCert{}", crate::cert::unique_slot()));
    let pair = crate::cert::ensure_pair(&dir, &log).expect("generate a test pair");
    let config = Arc::new(pair.server_config().expect("server config"));
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    let seen = Arc::new(Mutex::new(Vec::<CastMessage>::new()));
    let opened = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(Mutex::new(Vec::<usize>::new()));
    let (seen2, opened2, closed2, config2) =
        (Arc::clone(&seen), Arc::clone(&opened), Arc::clone(&closed), Arc::clone(&config));

    let handle = thread::spawn(move || {
        let mut serving = Vec::new();
        for (index, replies) in scripts.into_iter().enumerate() {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            opened2.fetch_add(1, Ordering::SeqCst);
            let (seen3, closed3, config3) = (
                Arc::clone(&seen2),
                Arc::clone(&closed2),
                Arc::clone(&config2),
            );
            serving.push(thread::spawn(move || {
                serve(stream, index, &config3, &replies, &seen3, &closed3)
            }));
        }
        drop(listener);
        for worker in serving {
            let _ = worker.join();
        }
        let _ = std::fs::remove_dir_all(&dir);
    });

    ScriptedDevice {
        port,
        seen,
        opened,
        closed,
        handle: Some(handle),
    }
}

fn serve(
    stream: TcpStream,
    index: usize,
    config: &Arc<rustls::ServerConfig>,
    replies: &[Vec<u8>],
    seen: &Arc<Mutex<Vec<CastMessage>>>,
    closed: &Arc<Mutex<Vec<usize>>>,
) {
    let _ = stream.set_read_timeout(Some(DEVICE_READ_TIMEOUT));
    // `ServerConnection::new` moves the Arc, so every accepted connection
    // needs its own clone.
    let Ok(conn) = rustls::ServerConnection::new(Arc::clone(config)) else {
        return;
    };
    let mut tls = rustls::StreamOwned::new(conn, stream);
    let mut reply_at = 0usize;
    loop {
        let mut header = [0u8; FRAME_HEADER_LEN];
        match tls.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                closed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(index);
                break;
            }
            Err(_) => break,
        }
        let len = frame_length(&header);
        if len > (1 << 20) {
            break;
        }
        let mut body = vec![0u8; len];
        if tls.read_exact(&mut body).is_err() {
            break;
        }
        let message = parse_cast_message(&body);
        seen.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(message);
        if reply_at < replies.len() {
            if tls.write_all(&replies[reply_at]).is_err() {
                break;
            }
            let _ = tls.flush();
            reply_at += 1;
        }
    }
    let _ = tls.conn.send_close_notify();
}

fn text_frame(destination: &str, namespace: &str, body: Value) -> Vec<u8> {
    wrap_frame(&encode_cast_message(
        RECEIVER_SOURCE,
        destination,
        namespace,
        body.to_string(),
    ))
}

fn auth_frame(destination: &str) -> Vec<u8> {
    wrap_frame(&encode_cast_message(
        RECEIVER_SOURCE,
        destination,
        NS_DEVICEAUTH,
        Payload::Binary(encode_auth_response()),
    ))
}

fn receiver_status(transport_id: &str) -> Value {
    json!({
        "type": "RECEIVER_STATUS",
        "status": {"applications": [{
            "displayName": "Default Media Receiver",
            "transportId": transport_id
        }]}
    })
}

/// The five frames a well-behaved receiver answers a whole push with.
fn success_script() -> Vec<Vec<u8>> {
    vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(SENDER_SOURCE, NS_RECEIVER, receiver_status("tp-1")),
        text_frame("tp-1", NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(
            "tp-1",
            NS_MEDIA,
            json!({"type": "MEDIA_STATUS", "status": [{"playerState": "PLAYING", "mediaSessionId": 3}]}),
        ),
    ]
}

/// A sender wired to a device that answers nothing at all.
fn silent_sender(port: u16) -> CastSender {
    CastSender::new("127.0.0.1", port, 1.0)
}

fn cast_target(port: u16, name: &str) -> TargetItem {
    TargetItem {
        id: "cast-1".to_owned(),
        kind: "cast",
        name: name.to_owned(),
        address: format!("127.0.0.1:{port}"),
    }
}

fn media<'a>(title: &'a str, content_type: &'a str) -> PushMedia<'a> {
    PushMedia {
        title,
        content_type,
        duration: 0.0,
        live: false,
    }
}

// ---------------------------------------------------------------------------
// Harness B — the real HTTP SOAP stub
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Captured {
    request_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> String {
        self.headers
            .iter()
            .find(|(key, _)| *key == name.to_lowercase())
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }
}

struct SoapStub {
    port: u16,
    captured: Arc<Mutex<Vec<Captured>>>,
    running: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl SoapStub {
    fn captured(&self) -> Vec<Captured> {
        self.captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn finish(self) -> Vec<Captured> {
        let taken = self.captured();
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
        taken
    }
}

/// `(status line body)` per connection, **plus one spare shape**. The stub
/// accepts exactly as many connections as it was handed replies and then answers
/// anything further with a 500 that names itself: a `Connection refused` reads
/// like "this module cannot reach a localhost port" — the exact wrong lesson
/// AGENTS.md §4.2 records for the proxy trap — while an over-length script shows
/// up as a captured row the test's own count rejects.
fn start_soap_stub(replies: Vec<(u16, String, String)>) -> SoapStub {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let captured2 = Arc::clone(&captured);
    let running = Arc::new(AtomicBool::new(true));
    let running2 = Arc::clone(&running);

    let handle = thread::spawn(move || {
        // Non-blocking accept so `finish()` can stop the thread rather than
        // leaving it parked in `accept()` forever. The accepted stream is set
        // back to blocking by hand: the flag a listener carries is not a promise
        // about its children.
        let _ = listener.set_nonblocking(true);
        let unplanned = (
            500u16,
            "Internal Server Error".to_owned(),
            "<soap:Envelope>stub ran out of script</soap:Envelope>".to_owned(),
        );
        let mut replies = replies.into_iter();
        loop {
            if !running2.load(Ordering::SeqCst) {
                break;
            }
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            let _ = stream.set_nonblocking(false);
            let (status, reason, body) = replies.next().unwrap_or_else(|| unplanned.clone());
            let _ = stream.set_read_timeout(Some(DEVICE_READ_TIMEOUT));
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut headers: Vec<(String, String)> = Vec::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                    break;
                }
                let folded = line.trim_end();
                if let Some((name, value)) = folded.split_once(':') {
                    let name = name.trim().to_lowercase();
                    if name == "content-length" {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    headers.push((name, value.trim().to_owned()));
                }
            }
            let mut raw = vec![0u8; length];
            let _ = std::io::Read::read_exact(&mut reader, &mut raw);
            let body_text = String::from_utf8_lossy(&raw).to_string();
            captured2
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Captured {
                    request_line,
                    headers,
                    body: body_text,
                });
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.as_bytes().len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    SoapStub {
        port,
        captured,
        running,
        handle: Some(handle),
    }
}

fn soap_reply(action: &str, inner: &str) -> String {
    format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action}Response xmlns:u=\"{DLNA_SERVICE}\">{inner}</u:{action}Response>\
         </s:Body></s:Envelope>"
    )
}

fn dlna_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/upnp/dev/1/AVTransport/control")
}

fn dlna_target(port: u16, kind: &'static str, name: &str) -> TargetItem {
    TargetItem {
        id: "dlna-1".to_owned(),
        kind,
        name: name.to_owned(),
        address: dlna_url(port),
    }
}

// ---------------------------------------------------------------------------
// Deviation ① — `payload_matches` requires a JSON object
// ---------------------------------------------------------------------------

/// Python `data.get('type')` on a non-dict raises `AttributeError`, which is
/// **not** in `_await`'s `except (socket.timeout, ssl.SSLError, OSError)`
/// family, so it escapes and the push fails loudly. Here a non-dict simply
/// never matches, the await times out, and the push fails with the LAUNCH
/// sentence. Both end in `push_url_to`'s failure line; the message differs,
/// the outcome does not.
#[test]
fn payload_matches_needs_a_json_object() {
    crate::pin_test_dir();
    assert!(CastSender::payload_matches(
        &json!({"type": "RECEIVER_STATUS"}),
        "RECEIVER_STATUS"
    ));
    // A bare string is Python's AttributeError, here a plain "no match".
    assert!(!CastSender::payload_matches(
        &json!("RECEIVER_STATUS"),
        "RECEIVER_STATUS"
    ));
    assert!(!CastSender::payload_matches(
        &json!([{"type": "RECEIVER_STATUS"}]),
        "RECEIVER_STATUS"
    ));
    assert!(!CastSender::payload_matches(
        &json!({"status": {}}),
        "RECEIVER_STATUS"
    ));
    // A non-string `type` cannot match either — Python would compare False to
    // the wanted name and answer False, the same outcome by a different road.
    assert!(!CastSender::payload_matches(
        &json!({"type": 7}),
        "RECEIVER_STATUS"
    ));
    assert!(!CastSender::payload_matches(&json!(null), "RECEIVER_STATUS"));
}

// ---------------------------------------------------------------------------
// Deviation ② — `split_cast_address` reads ASCII digits and refuses ports that
// cannot be dialled
// ---------------------------------------------------------------------------

/// Python's `port.isdigit()` accepts Unicode digits and then `int()`s them;
/// here they fall back to [`CAST_PORT`] exactly the way Python's
/// `else CAST_PORT` arm does, so the two agree on every *ASCII* shape and
/// diverge only where Python would go dial a port it should not have.
#[test]
fn split_cast_address_reads_ascii_digits_only() {
    crate::pin_test_dir();
    let (host, port) = split_cast_address("192.168.1.9:8009").unwrap();
    assert_eq!((host.as_str(), port), ("192.168.1.9", 8009));

    // Fullwidth digits: Python's `isdigit()` says yes and `int()` says 8009.
    // The ASCII rule says no, and the fallback answers 8009 for a different
    // reason — which is why both must be asserted, not just one.
    for shape in ["192.168.1.9:８００９", "192.168.1.9:٨٠٠٩", "192.168.1.9:12 3", "192.168.1.9:http"] {
        let (_, port) = split_cast_address(shape)
            .unwrap_or_else(|e| panic!("{shape} must not be refused: {e}"));
        assert_eq!(port, CAST_PORT, "{shape} is not ASCII digits");
    }
}

/// A port above 65535 crashes Python's `getaddrinfo` later, with a
/// `socket.gaierror` that reads like a DNS problem. Here it is refused by name
/// at the split, and a digit run too long for `u64` gets the same sentence.
#[test]
fn split_cast_address_refuses_an_out_of_range_port() {
    crate::pin_test_dir();
    for shape in ["10.0.0.2:99999", "10.0.0.2:65536", "10.0.0.2:99999999999999999999999"] {
        let err = split_cast_address(shape)
            .err()
            .unwrap_or_else(|| panic!("{shape} must be refused"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(
            err.to_string().contains("目标端口超出范围"),
            "{err} must name the port problem"
        );
    }
    // The top of the range still parses, and does not take the error road.
    let (host, port) = split_cast_address("10.0.0.2:65535").unwrap();
    assert_eq!((host.as_str(), port), ("10.0.0.2", 65535));
}

/// A row that carries no port at all is a cast row on the cast port, and a row
/// that carries nothing is still a row: the host is not policed here, because
/// [`CastSender::connect`] owns that question.
#[test]
fn split_cast_address_defaults_to_the_cast_port() {
    crate::pin_test_dir();
    for address in ["10.0.0.2", "10.0.0.2:", "10.0.0.2:http", ""] {
        let (host, port) = split_cast_address(address).unwrap();
        assert_eq!(port, CAST_PORT, "{address} has no usable port");
        assert_eq!(host, address.split(':').next().unwrap_or(address));
    }
}

/// `CastSender::new` rescues port 0 — the one shape `split_cast_address`
/// answers faithfully (0 *is* a digit run) and the constructor knows better.
#[test]
fn a_zero_port_is_left_for_the_sender_to_rescue() {
    crate::pin_test_dir();
    let (host, port) = split_cast_address("10.0.0.2:0").unwrap();
    assert_eq!((host.as_str(), port), ("10.0.0.2", 0));
    assert_eq!(CastSender::new("10.0.0.2", 0, 2.0).port, CAST_PORT);
}

// ---------------------------------------------------------------------------
// Deviations ③ and ④ — timeouts are answered finite, and the sender keeps
// Python's one-second floor
// ---------------------------------------------------------------------------

/// Python clamps at read time (`number(Setting.get(Timeout, 5), 5)`) and then
/// hands whatever it got to `settimeout`. Here [`socket_timeout`] answers a
/// finite number for every shape a hand-edited JSON can carry.
#[test]
fn socket_timeout_answers_finite_for_every_setting_shape() {
    crate::pin_test_dir();
    for bad in [f64::NAN, 0.0, -1.0, f64::NEG_INFINITY] {
        assert_eq!(socket_timeout(bad), DEFAULT_TIMEOUT_SECONDS, "{bad} is not a wait");
    }
    // Deviation ④'s ceiling: Python would pass `inf` into `settimeout`, which
    // is *accepted* and means "block forever" — a stuck device then hangs the
    // push until the CherryPy worker dies.
    assert_eq!(socket_timeout(f64::INFINITY), TIMEOUT_CEILING_SECONDS);
    assert_eq!(socket_timeout(1e9), TIMEOUT_CEILING_SECONDS);
    assert_eq!(socket_timeout(3.0), 3.0);
    assert_eq!(socket_timeout(DEFAULT_TIMEOUT_SECONDS), 5.0);
}

/// Deviation ④'s other half, on the sender: Python `max(1.0, timeout)`.
#[test]
fn the_sender_keeps_the_one_second_floor() {
    crate::pin_test_dir();
    assert_eq!(CastSender::new("h", 8009, 0.1).timeout_seconds, 1.0);
    assert_eq!(CastSender::new("h", 8009, 0.0).timeout_seconds, 1.0);
    assert_eq!(CastSender::new("h", 8009, 12.0).timeout_seconds, 12.0);
}

/// The setting is read through the same forgiving number rule the rest of the
/// port uses, and a fresh `Setting` records the default on the way out — the
/// same side effect AGENTS.md §4.2 warns about in Python.
///
/// The two halves are asked separately on purpose. [`push_timeout`] is
/// `number_or` and **nothing else**: it hands back `0.0` for `false` and
/// `1_000_000_000` for `"1e9"`. [`socket_timeout`] is where both of those get
/// rescued. Collapsing the two into one expectation is how a reader ends up
/// believing the getter clamps, and then writing a second caller that does not.
#[test]
fn push_timeout_reads_the_setting_through_the_same_number_rule() {
    crate::pin_test_dir();
    let setting = Setting::new(setting_file("timeout"));
    assert_eq!(push_timeout(&setting), DEFAULT_TIMEOUT_SECONDS);
    assert!(
        setting.has(Key::Timeout),
        "get() on an absent key inserts the default here, as it does in Python"
    );

    for (value, raw, on_the_socket) in [
        // A numeric string is a number, and survives both steps unchanged.
        (json!("7"), 7.0, 7.0),
        // Unparseable text falls to the default inside `number_or`.
        (json!("abc"), DEFAULT_TIMEOUT_SECONDS, DEFAULT_TIMEOUT_SECONDS),
        // `false` is `0.0` to `number_or` — a real answer, not a missing one.
        // The floor is the *socket* step that turns it into the default.
        (json!(false), 0.0, DEFAULT_TIMEOUT_SECONDS),
        // A stored null is "no answer", which `number_or` maps to the default.
        (json!(null), DEFAULT_TIMEOUT_SECONDS, DEFAULT_TIMEOUT_SECONDS),
        // `push_timeout` does not clamp; the ceiling belongs to `socket_timeout`.
        (json!("1e9"), 1_000_000_000.0, TIMEOUT_CEILING_SECONDS),
        // Arrays and objects are not numbers by any reading.
        (json!([5]), DEFAULT_TIMEOUT_SECONDS, DEFAULT_TIMEOUT_SECONDS),
    ] {
        let one = Setting::new(setting_file("timeout"));
        one.put(Key::Timeout, value.clone());
        let read = push_timeout(&one);
        assert_eq!(read, raw, "{value} must read as {raw}, got {read}");
        assert_eq!(
            socket_timeout(read),
            on_the_socket,
            "{value} must reach the socket as {on_the_socket}"
        );
    }
}

// ---------------------------------------------------------------------------
// Deviation ⑦ — an empty cast host is refused before dialing
// ---------------------------------------------------------------------------

/// Python `getaddrinfo("")` may answer loopback depending on the resolver. A
/// row whose address lost its host must fail with a sentence, not cast to
/// `127.0.0.1`.
#[test]
fn an_empty_cast_host_is_refused() {
    crate::pin_test_dir();
    for host in ["", "   "] {
        let err = CastSender::new(host, 8009, 1.0)
            .connect()
            .err()
            .unwrap_or_else(|| panic!("{host:?} must be refused, not dialled"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(
            err.to_string().contains("目标地址缺少主机名"),
            "{err} must name the host problem"
        );
        assert!(
            !err.to_string().contains("127.0.0.1"),
            "a refusal must not read like a loopback cast: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// Deviation ⑥ — an await with a zero timeout falls back to the instance budget
// ---------------------------------------------------------------------------

/// Python `timeout or self.timeout` (:1291) already behaves this way; it is
/// named here because Rust's `Option<Duration>` shape could silently change it
/// into "return immediately".
#[test]
fn a_zero_per_call_timeout_uses_the_instance_budget() {
    crate::pin_test_dir();
    // Answers the handshake, then goes silent: the await has to time out twice.
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
    ]]);
    let mut sender = silent_sender(device.port);
    sender.connect().expect("handshake completes with a scripted device");

    let started = Instant::now();
    let none = sender
        .await_message("RECEIVER_STATUS", Some(Duration::ZERO))
        .unwrap();
    let zero_took = started.elapsed();
    assert!(none.is_none(), "a silent device answers nothing");
    assert!(
        zero_took >= Duration::from_millis(900),
        "a zero per-call timeout must fall back to the 1 s instance budget, took {zero_took:?}"
    );

    let started = Instant::now();
    let none = sender
        .await_message("RECEIVER_STATUS", Some(Duration::from_millis(200)))
        .unwrap();
    let short_took = started.elapsed();
    assert!(none.is_none());
    assert!(
        short_took < zero_took,
        "an explicit 200 ms must not pay the instance budget again: {short_took:?} vs {zero_took:?}"
    );

    device.finish();
}

// ---------------------------------------------------------------------------
// The `applications` / `launch` arms — the sentence each shape lands in
// ---------------------------------------------------------------------------

/// `push.rs:459` says this module names which arm each shape lands in. The
/// docstring's claim is that a *truthy non-dict* status lands on the
/// transportId sentence, not the LAUNCH one.
#[test]
fn applications_names_which_arm_each_shape_lands_in() {
    crate::pin_test_dir();
    let good = receiver_status("tp-9");
    assert_eq!(CastSender::applications(&good).len(), 1);
    assert_eq!(
        CastSender::applications(&good)[0]["transportId"],
        json!("tp-9")
    );

    // Every shape that has no applications answers an empty list — including
    // the ones Python raises `AttributeError` on.
    for shape in [
        json!({"type": "RECEIVER_STATUS", "status": "PLAYING"}),
        json!({"type": "RECEIVER_STATUS", "status": [{"applications": []}]}),
        json!({"type": "RECEIVER_STATUS", "status": {}}),
        json!({"type": "RECEIVER_STATUS"}),
        json!({"type": "RECEIVER_STATUS", "status": {"applications": "none"}}),
        json!({"type": "RECEIVER_STATUS", "status": 5}),
    ] {
        assert!(
            CastSender::applications(&shape).is_empty(),
            "{shape} must answer no applications"
        );
    }
}

/// An unanswered LAUNCH: the only shape that earns 「无响应」.
#[test]
fn a_launch_refuses_a_device_that_never_answers() {
    crate::pin_test_dir();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
    ]]);
    let mut sender = silent_sender(device.port);
    sender.connect().expect("handshake completes");

    let started = Instant::now();
    let err = sender
        .launch(None)
        .err()
        .expect("a silent device must not launch");
    let took = started.elapsed();
    assert!(
        err.to_string().contains("目标设备没有启动媒体接收器（LAUNCH 无响应）"),
        "{err}"
    );
    // The floor is 1.0 s and the poll never goes below 200 ms, so the honest
    // window is [0.9, 5] seconds rather than "about one".
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_secs(5),
        "the LAUNCH budget read as {took:?}"
    );

    // The device saw exactly the handshake, the CONNECT, and the LAUNCH.
    let seen = device.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].namespace, NS_RECEIVER);
    let launched: Value = serde_json::from_str(&seen[2].payload_utf8).unwrap();
    assert_eq!(launched["type"], json!("LAUNCH"));
    assert_eq!(launched["appId"], json!(DEFAULT_MEDIA_APP_ID));
    device.finish();
}

/// Deviation ①'s landing place: a `RECEIVER_STATUS` whose status is a *string*
/// earns the transportId sentence — and must **not** be reported as no reply,
/// because the device did reply.
#[test]
fn a_launch_refuses_a_payload_that_is_not_an_object() {
    crate::pin_test_dir();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(
            SENDER_SOURCE,
            NS_RECEIVER,
            json!({"type": "RECEIVER_STATUS", "status": "PLAYING"}),
        ),
    ]]);
    let mut sender = silent_sender(device.port);
    sender.connect().expect("handshake completes");

    let err = sender
        .launch(None)
        .err()
        .expect("a status with no applications cannot be launched into");
    assert!(
        err.to_string().contains("目标设备没有返回 transportId"),
        "{err}"
    );
    assert!(
        !err.to_string().contains("无响应"),
        "the device answered; saying it did not is the wrong sentence: {err}"
    );
    device.finish();
}

/// A RECEIVER_STATUS that arrives as a **binary** payload is skipped by
/// `payload_type != 0`, so the push hears nothing — which is the LAUNCH-timeout
/// arm, not the transportId arm. This is the one shape where the two roads
/// cross, and it must land on the honest one.
#[test]
fn a_binary_receiver_status_is_not_heard_as_a_reply() {
    crate::pin_test_dir();
    let body = receiver_status("tp-1").to_string();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        wrap_frame(&encode_cast_message(
            RECEIVER_SOURCE,
            SENDER_SOURCE,
            NS_RECEIVER,
            Payload::Binary(body.into_bytes()),
        )),
    ]]);
    let mut sender = silent_sender(device.port);
    sender.connect().expect("handshake completes");
    let err = sender.launch(None).err().expect("a binary status is not a reply");
    assert!(err.to_string().contains("LAUNCH 无响应"), "{err}");
    device.finish();
}

/// What [`CastSender::connect`] owes the pipe: Python's `connect` does a bare
/// `_recv()` and would mis-frame every later message if the device's auth
/// answer were left in it (:1332-1335). The witnessed contract is that the
/// challenge goes out, the answer is *consumed*, and a device that never
/// answers is a failed push — not a push that quietly continues.
#[test]
fn a_device_that_never_answers_the_challenge_is_not_a_connected_sender() {
    crate::pin_test_dir();
    let device = start_device(vec![vec![]]);
    let err = silent_sender(device.port)
        .connect()
        .err()
        .expect("a device that ignores the challenge is not a device");
    // The exact kind is rustls's, not ours: measured on this machine, a silent
    // read on a timed-out socket arrives as a timeout-shaped error. Assert the
    // family, not one spelling, so the witness survives a rustls bump.
    let kind = err.kind();
    assert!(
        matches!(
            kind,
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::UnexpectedEof
        ),
        "an unanswered challenge must read as a read failure, got {kind:?}: {err}"
    );
    let seen = device.seen();
    assert_eq!(seen.len(), 1, "only the challenge may leave the socket");
    assert_eq!(seen[0].namespace, NS_DEVICEAUTH);
    assert_eq!(seen[0].source_id, SENDER_SOURCE);
    // 定点变异 measured against this file: deleting the `?` after
    // `let _ = self.recv()?;` in `connect()` reddens exactly this test
    // (`602 passed; 1 failed`) — the read still fails, but without the `?`
    // `connect()` hands back a *connected* sender, and the `.err().expect(...)`
    // above is the question that notices. An earlier note in this file called
    // this shape **0 红**; that reading did not reproduce here, so the claim is
    // gone and this is the measurement that stands.
    device.finish();
}

// ---------------------------------------------------------------------------
// The whole push, on a real socket
// ---------------------------------------------------------------------------

/// A successful push: the card gets its sentence, the device gets the media,
/// and the handoff list keeps the socket.
#[test]
fn the_full_cast_push_reaches_the_device_and_the_handoff_list() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script()]);
    let (log, path) = witness("PushCardFull");
    let setting = Setting::new(setting_file("full"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(message, "已投给「客厅」");
    assert_eq!(handoff_count(), 1, "a successful push hands its socket over");

    let seen = device.seen();
    let namespaces: Vec<&str> = seen.iter().map(|m| m.namespace.as_str()).collect();
    assert_eq!(
        namespaces,
        vec![NS_DEVICEAUTH, NS_CONNECTION, NS_RECEIVER, NS_CONNECTION, NS_MEDIA]
    );
    let destinations: Vec<&str> = seen.iter().map(|m| m.destination_id.as_str()).collect();
    assert_eq!(
        destinations,
        vec!["receiver-0", "receiver-0", "receiver-0", "tp-1", "tp-1"]
    );

    let text = lines(&path);
    assert!(
        text.contains("page card pushed 第一部 to 客厅") && text.contains("PLAYING"),
        "{text}"
    );
    device.finish();
}

/// The same push twice in a row is two handoffs, not one socket reused: the
/// page card opens a fresh sender per press.
#[test]
fn two_presses_are_two_handoffs() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script(), success_script()]);
    let (log, _path) = witness("PushCardTwice");
    let setting = Setting::new(setting_file("twice"));

    for _ in 0..2 {
        let (ok, message) = push_url_to(
            &cast_target(device.port, "客厅"),
            "http://example.com/a.mp4",
            &media("第一部", "video/mp4"),
            &setting,
            &log,
        );
        assert!(ok, "{message}");
    }
    assert_eq!(handoff_count(), 2);
    assert_eq!(device.opened(), 2, "a press is a connection, not a verb");
    device.finish();
}

/// A failed push must not leave a socket in the list. This is the cheap half
/// of AGENTS.md §4.14's 「失败的中转立刻按 id 摘除」: a half-dead handoff is a
/// slot with a delay attached to it.
#[test]
fn a_push_that_fails_leaves_no_handoff() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    // No auth answer at all: the push fails inside `connect`.
    let device = start_device(vec![vec![]]);
    let (log, path) = witness("PushCardFail");
    let setting = Setting::new(setting_file("fail"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(!ok);
    assert!(
        message.starts_with("投给「客厅」失败："),
        "the card must name the device it could not reach: {message}"
    );
    assert_eq!(handoff_count(), 0, "a failed push owns nothing");

    let text = lines(&path);
    assert!(
        text.contains("pushing 第一部 to 客厅 failed"),
        "the log line must name the media and the device: {text}"
    );
    device.finish();
}

/// `MAX_HANDOFFS` is a cap, and the page card's push path issues no verb that
/// closes a socket — so the *only* thing standing between a user pressing
/// install-and-push fifty times and fifty open TLS sessions is this eviction.
#[test]
fn a_fifth_handoff_closes_the_first() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script(); 5]);
    let (log, _path) = witness("PushCardEvict");
    let setting = Setting::new(setting_file("evict"));

    for _ in 0..4 {
        let (ok, message) = push_url_to(
            &cast_target(device.port, "客厅"),
            "http://example.com/a.mp4",
            &media("第一部", "video/mp4"),
            &setting,
            &log,
        );
        assert!(ok, "{message}");
    }
    assert_eq!(handoff_count(), MAX_HANDOFFS);
    assert!(
        device.closed().is_empty(),
        "nothing may close while the list is at the cap: {:?}",
        device.closed()
    );

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(handoff_count(), MAX_HANDOFFS, "the cap holds, it does not grow");
    // The close is the *device's* book: `remember()` hands the evicted sender
    // over and the socket shutdown lands on another thread's ledger a moment
    // later. Reading `closed()` at the instant the fifth push returns is the
    // AGENTS.md §4.2 「抓一瞬间」 shape — wait for the entry instead.
    let closed = wait_closed(&device, 1);
    assert_eq!(
        closed,
        vec![0],
        "the *oldest* handoff is the one that pays, and only it"
    );
    device.finish();
}

/// The unit half of the same rule, against a list nobody else can disturb:
/// eviction starts at the fifth push, never the fourth.
#[test]
fn handoffs_evict_the_oldest_only_past_the_cap() {
    crate::pin_test_dir();
    let mut held = Handoffs::default();
    for index in 0..MAX_HANDOFFS {
        let evicted = held.remember(CastSender::new(format!("h{index}"), 8009, 1.0));
        assert!(
            evicted.is_empty(),
            "the {index}th push must not evict anything"
        );
    }
    assert_eq!(held.held.len(), MAX_HANDOFFS);

    let evicted = held.remember(CastSender::new("h4", 8009, 1.0));
    assert_eq!(evicted.len(), 1, "one over the cap evicts exactly one");
    assert_eq!(evicted[0].host, "h0", "the oldest pays, not the newest");
    assert_eq!(held.held.len(), MAX_HANDOFFS);
}

/// The ledger has one owner at a time, and a thread that is not the owner does
/// not get to drain it. Written because the unconditional drain was the bug this
/// round actually hit: a refusal test's `finish()` cleared the eviction test's
/// four senders mid-assertion (`handoff_count()` read 2 after four pushes that
/// each answered "ok"), green under the `push_tests` filter and red under the
/// whole crate. The idle device below is the refusal test — it holds nothing, so
/// its drain has no business reaching into our list.
#[test]
fn a_thread_that_does_not_own_the_ledger_leaves_it_alone() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script()]);
    let (log, _path) = witness("PushLedgerOwner");
    let setting = Setting::new(setting_file("owner"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(handoff_count(), 1, "our own push parked our own sender");

    let idle = start_device(vec![]);
    thread::spawn(move || idle.finish())
        .join()
        .expect("a device with nothing to serve finishes");

    assert_eq!(
        handoff_count(),
        1,
        "a non-owner's finish() must not close a sender it did not park"
    );
    device.finish();
}

// ---------------------------------------------------------------------------
// The outgoing LOAD frame — deviation ⑧'s byte-for-byte promise
// ---------------------------------------------------------------------------

/// The frame the device receives, pinned against Python's own dict written out
/// here. `currentTime` is `0.0` where Python's `int` writes `0`: both are valid
/// JSON numbers and every receiver parses either, but the difference must be a
/// recorded decision rather than a silent drift.
#[test]
fn the_outgoing_load_frame_matches_python_key_for_key() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script()]);
    let (log, _path) = witness("PushCardFrame");
    let setting = Setting::new(setting_file("frame"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");

    let seen = device.seen();
    let load = &seen[4];
    assert_eq!(load.namespace, NS_MEDIA);
    assert_eq!(
        load.payload_utf8,
        r#"{"type":"LOAD","requestId":2,"autoplay":true,"currentTime":0.0,"media":{"contentId":"http://example.com/a.mp4","streamType":"BUFFERED","metadata":{"metadataType":0,"title":"第一部"},"contentType":"video/mp4"}}"#
    );
    device.finish();
}

/// An empty `contentType` makes some receivers guess a handler and pick the
/// audio-only one; leaving the key out is the documented way (Python
/// :1369-1371 appends it *last*, after the metadata block).
#[test]
fn the_load_frame_drops_an_empty_content_type() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script()]);
    let (log, _path) = witness("PushCardNoType");
    let setting = Setting::new(setting_file("notype"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", ""),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    let load = &device.seen()[4];
    assert!(
        !load.payload_utf8.contains("contentType"),
        "an empty contentType must not be promised: {}",
        load.payload_utf8
    );
    // Still in the same place in the key order — `media` keeps its shape.
    assert!(load
        .payload_utf8
        .ends_with("\"metadataType\":0,\"title\":\"第一部\"}}}"));
    device.finish();
}

/// A live push is the one shape where `streamType` says LIVE, and the page
/// card's live flag must reach the device rather than stay in our own ledger.
#[allow(non_snake_case)]
#[test]
fn a_live_push_promises_LIVE_on_the_wire() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script()]);
    let (log, _path) = witness("PushCardLive");
    let setting = Setting::new(setting_file("live"));
    let mut picture = media("第一部", "video/mp4");
    picture.live = true;
    picture.duration = 90.0;

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &picture,
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    let load: Value = serde_json::from_str(&device.seen()[4].payload_utf8).unwrap();
    assert_eq!(load["media"]["streamType"], json!("LIVE"));
    device.finish();
}

// ---------------------------------------------------------------------------
// The state the device reports back
// ---------------------------------------------------------------------------

/// Deviation ⑧, third member: Python's `str.format` would print the repr of a
/// non-string `playerState`; here it reads 状态未知. The sentence is what the
/// user reads, so the *fallback* is the thing under test, not the parse.
#[test]
fn a_non_string_player_state_reads_unknown_state() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(SENDER_SOURCE, NS_RECEIVER, receiver_status("tp-1")),
        text_frame("tp-1", NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(
            "tp-1",
            NS_MEDIA,
            json!({"type": "MEDIA_STATUS", "status": [{"playerState": {"nested": 1}}]}),
        ),
    ]]);
    let (log, path) = witness("PushCardWeird");
    let setting = Setting::new(setting_file("weird"));

    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(message, "已投给「客厅」");
    let text = lines(&path);
    assert!(
        text.contains("状态未知"),
        "a device that says something unreadable must be reported as silent, not repr'd: {text}"
    );
    assert!(!text.contains("nested"), "the JSON's shape must not leak: {text}");
    device.finish();
}

/// Two questions about the same reply, asked in the order the code answers them.
/// First: which shapes of `MEDIA_STATUS.status` hand back an entry at all — the
/// silent shapes answer with nothing, the not-a-list shapes refuse with our own
/// sentence, and the split is what decides whether the card reads 状态未知 or a
/// failure. Second: the state that *does* come back is upper-cased exactly once,
/// in `push_inner`'s cast arm, and lands on the card in that shape. The second
/// half is witnessed through a real push against a device that answers in
/// lowercase — reading `to_uppercase()` off a value the test built itself would
/// only be a test of `str::to_uppercase` (AGENTS.md §4.2: 「用例比的是'说的是谁的名字'」).
#[test]
fn the_player_state_is_upper_cased() {
    crate::pin_test_dir();
    // A silent device is six shapes, and none of them is an error.
    for empty in [
        json!({"type": "m", "status": null}),
        json!({"type": "m", "status": []}),
        json!({"type": "m", "status": {}}),
        json!({"type": "m", "status": ""}),
        json!({"type": "m", "status": 0}),
        json!({"type": "m", "status": false}),
    ] {
        assert!(
            CastSender::media_status_entries(&empty)
                .unwrap()
                .is_empty(),
            "{empty} is a silent device, not a broken one"
        );
    }
    // A device that answers with something that is not a list is refused, in
    // words, by the one sentence that names the field.
    for bad in [
        json!({"type": "m", "status": "PLAYING"}),
        json!({"type": "m", "status": 3}),
        json!({"type": "m", "status": true}),
    ] {
        let err = CastSender::media_status_entries(&bad)
            .err()
            .unwrap_or_else(|| panic!("{bad} must be refused, not answered"));
        assert!(
            err.to_string().contains("MEDIA_STATUS 的 status 不是列表"),
            "{err}"
        );
    }
    // A two-entry list is two entries, and a bare object is one — Python's
    // `_await` returns the first, so the count is what the reader walks.
    assert_eq!(
        CastSender::media_status_entries(&json!({"type": "m", "status": [{"a": 1}, {"b": 2}]}))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        CastSender::media_status_entries(&json!({"type": "m", "status": {"playerState": "x"}}))
            .unwrap()
            .len(),
        1
    );

    // Now the upper-casing, through the sender's own reader and a real socket.
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(SENDER_SOURCE, NS_RECEIVER, receiver_status("tp-1")),
        text_frame("tp-1", NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(
            "tp-1",
            NS_MEDIA,
            // lowercase, the way a device that copies the JSON spec would write it
            json!({"type": "MEDIA_STATUS", "status": [{"playerState": "playing"}]}),
        ),
    ]]);
    let (log, path) = witness("StateCase");
    let (ok, message) = push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &Setting::new(setting_file("state-case")),
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(message, "已投给「客厅」", "the card says nothing about case: {message}");
    let text = lines(&path);
    assert!(
        text.contains("): PLAYING"),
        "the lowercase answer must reach the card upper-cased, after the address: {text}"
    );
    assert!(
        !text.contains("): playing"),
        "and must not reach it raw: {text}"
    );
    device.finish();
}

/// The session id witnesses what the device actually said. Python
/// `entry.get('mediaSessionId', 1)` keeps a *present null* as null; a device
/// that says "there is no session" must not be answered with a fabricated 1 by
/// us either — and an absent key still takes the default.
#[test]
fn the_media_session_id_records_what_the_device_actually_said() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![vec![
        auth_frame(SENDER_SOURCE),
        text_frame(SENDER_SOURCE, NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(SENDER_SOURCE, NS_RECEIVER, receiver_status("tp-1")),
        text_frame("tp-1", NS_CONNECTION, json!({"type": "CONNECT"})),
        text_frame(
            "tp-1",
            NS_MEDIA,
            json!({"type": "MEDIA_STATUS", "status": [{"playerState": "PLAYING", "mediaSessionId": 42}]}),
        ),
    ]]);
    let mut sender = silent_sender(device.port);
    sender.connect().expect("handshake completes");
    sender.launch(None).expect("the scripted device launches");
    assert_eq!(sender.media_session_id(), &json!(1), "nothing has loaded yet");

    sender
        .load("http://example.com/a.mp4", "video/mp4", false, "第一部", 0.0)
        .expect("the scripted device loads");
    assert_eq!(sender.media_session_id(), &json!(42), "42 is what the device said");

    // An empty status list leaves the previous answer alone — there is no
    // entry to read, so nothing is recorded either way.
    let empty = json!({"type": "MEDIA_STATUS", "status": []});
    let entry = CastSender::media_status_entries(&empty)
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    assert_eq!(entry, Value::Null);
    assert_eq!(sender.media_session_id(), &json!(42));
    device.finish();
}

/// The card's own sentence: the title when there is one, the URL when there is
/// not — and neither of them is ever the relay address we made up.
#[test]
fn push_url_to_names_the_title_and_falls_back_to_the_url() {
    crate::pin_test_dir();
    let _held = hold_push();
    clear_handoffs();
    let device = start_device(vec![success_script(), success_script()]);
    let (log, path) = witness("PushCardWords");
    let setting = Setting::new(setting_file("words"));

    push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("", "video/mp4"),
        &setting,
        &log,
    );
    push_url_to(
        &cast_target(device.port, "客厅"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );

    let text = lines(&path);
    assert!(
        text.contains("page card pushed http://example.com/a.mp4 to 客厅"),
        "a titleless push must name the url: {text}"
    );
    assert!(text.contains("page card pushed 第一部 to 客厅"), "{text}");
    device.finish();
}

/// A `LOAD` before a `LAUNCH` is a bug in *our* call order, and it must be
/// refused without touching the socket or inventing a session.
#[test]
fn a_load_before_launch_refuses_without_touching_the_socket() {
    crate::pin_test_dir();
    let mut sender = CastSender::new("10.0.0.2", 8009, 1.0);
    assert!(sender.transport_id.is_none());
    let err = sender
        .load("http://example.com/a.mp4", "video/mp4", false, "t", 0.0)
        .err()
        .expect("there is no transport to load onto");
    assert!(err.to_string().contains("尚未 LAUNCH"), "{err}");
    assert_eq!(sender.media_session_id(), &json!(1));
    // Sending on an unconnected sender is its own sentence, never a panic.
    let err = sender
        .send(RECEIVER_SOURCE, NS_RECEIVER, json_payload(json!({"type": "PING"})))
        .err()
        .expect("nothing is connected");
    assert_eq!(err.kind(), io::ErrorKind::NotConnected, "{err}");
    assert_eq!(err.to_string(), "尚未连接设备");
}

// ---------------------------------------------------------------------------
// The red line: a push owns nothing in our own playback state
// ---------------------------------------------------------------------------

/// The header's acceptance ④: a push tells the device what to play and nothing
/// more. This is a whole-file audit rather than a spot check — AGENTS.md §4.8
/// and Part 62's G-segment teach that a rule with no machine asking it decays
/// the day someone adds a convenient `set_media_url`.
#[test]
fn a_push_never_touches_our_own_playback_state() {
    crate::pin_test_dir();
    let source = include_str!("push.rs")
        .lines()
        // Drop whole comment lines, then trailing comments — cutting at `" //"`
        // and never at `"//"`, because the SOAP namespace literals contain
        // `http://` and a naive split would rewrite the envelope.
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| match line.find(" //") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in [
        "macast_core::store",
        "set_state",
        "set_media_url",
        "set_media_title",
        "remember_history",
        "push_history",
        "PlaybackGuard",
        "owner =",
    ] {
        assert!(
            !source.contains(forbidden),
            "push.rs must not reach into our own playback state via {forbidden}"
        );
    }
    // The header's own prose *does* say these words, which is why the scan
    // above strips comments first: without that, this test would be asking
    // "does the file mention the rule" instead of "does the file break it".
    assert!(
        source.contains("pub fn push_url_to"),
        "the audit is only meaningful if the file was read at all"
    );
}

/// Python hardcodes `sender-0` in `__init__` (:1255); our *receiver* side uses
/// `sender-1` in tests — a sender is the other party, and the two roles must
/// not share a name.
#[test]
fn the_sender_is_not_the_receiver() {
    crate::pin_test_dir();
    let sender = CastSender::new("10.0.0.2", 8009, 1.0);
    assert_eq!(sender.source_id, "sender-0");
    assert_eq!(sender.source_id, SENDER_SOURCE);
    assert_eq!(RECEIVER_SOURCE, "receiver-0");
    assert_ne!(sender.source_id, RECEIVER_SOURCE);
    // The receiver side of this crate answers as `sender-1`; if a push ever
    // started using that name, our own device would be talking to itself.
    let channel = include_str!("channel_tests.rs");
    assert!(channel.contains("sender-1"), "the receiver's peer name moved");
    let ours = CastSender::new("h", 8009, 1.0);
    assert!(
        !channel.contains(&format!("\"{}\"", ours.source_id)),
        "a push must never impersonate the peer our receiver tests dial"
    );
}

// ---------------------------------------------------------------------------
// The SOAP envelope and the DIDL — byte-level fixtures
// ---------------------------------------------------------------------------

/// Python builds this by concatenation, and the second literal ends with a
/// trailing space so `s:encodingStyle` follows exactly one space. That space is
/// the whole difference between "this parses" and "this parses on *that*
/// firmware".
#[test]
fn the_soap_envelope_is_python_s_byte_for_byte() {
    crate::pin_test_dir();
    assert_eq!(DLNA_SERVICE, "urn:schemas-upnp-org:service:AVTransport:1");
    let envelope = soap_envelope(
        "SetAVTransportURI",
        DLNA_SERVICE,
        "0",
        "<CurrentURI>x</CurrentURI>",
    );
    assert_eq!(
        envelope,
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" ",
            "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">",
            "<s:Body>",
            "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">",
            "<InstanceID>0</InstanceID>",
            "<CurrentURI>x</CurrentURI>",
            "</u:SetAVTransportURI>",
            "</s:Body>",
            "</s:Envelope>"
        )
    );
    // Exactly one space before `s:encodingStyle`, and none before `<s:Body>`.
    assert_eq!(envelope.matches("\" s:encodingStyle").count(), 1);
    assert!(envelope.contains("\">\"") || envelope.contains("\"><s:Body>"));
}

/// Deviation ⑨: Python's `int(inf)` raises `OverflowError`, and the DIDL is
/// built from measurements that can genuinely be infinite — a growing remux
/// file has no length. "No duration" is the honest answer, and it is written as
/// the clock's zero rather than omitted.
#[test]
fn build_didl_writes_zero_clock_for_an_infinite_duration() {
    crate::pin_test_dir();
    for infinite in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let didl = build_didl(
            "http://h/a.mp4",
            "t",
            "video/mp4",
            infinite,
            0,
            VIDEO_ITEM_CLASS,
        );
        assert!(
            didl.contains("duration=\"0:00:00\""),
            "{infinite} must answer the clock's zero, not a crash: {didl}"
        );
    }
    // The gate is `!= 0.0`, so a *negative* duration also gets a clock — and
    // `dlna_clock` floors it to the same zero. Recorded because the gate is
    // what makes both shapes arrive at the same honest sentence.
    let didl = build_didl("http://h/a.mp4", "t", "video/mp4", -5.0, 0, VIDEO_ITEM_CLASS);
    assert!(didl.contains("duration=\"0:00:00\""), "{didl}");
    // 定点变异 measured against this file: flipping the gate to `duration > 0.0`
    // reddens exactly this test (`602 passed; 1 failed`), and it is the *second*
    // shape in the loop that fires — `+inf > 0.0` is true, so `+inf` passes under
    // either gate and the negative/NaN rows are the whole witness. A "truthiness
    // looks like positivity" reading therefore needed this row to exist, which is
    // why it is an assertion rather than a sentence in the doc comment above.
}

/// A real measurement survives, and both zero measurements are omitted — a
/// `size="0"` is how a receiver reads "empty file".
#[test]
fn build_didl_omits_the_zero_measurements() {
    crate::pin_test_dir();
    let didl = build_didl(
        "http://h/a.mp4",
        "t",
        "video/mp4",
        30.0,
        1234,
        VIDEO_ITEM_CLASS,
    );
    assert!(didl.contains("duration=\"0:00:30\""), "{didl}");
    assert!(didl.contains("size=\"1234\""), "{didl}");

    let bare = build_didl("http://h/a.mp4", "t", "video/mp4", 0.0, 0, VIDEO_ITEM_CLASS);
    assert!(!bare.contains("duration="), "0.0 promises nothing: {bare}");
    assert!(!bare.contains("size="), "size 0 reads as an empty file: {bare}");
}

/// The `protocolInfo` is a promise about how the stream behaves, and the push
/// keeps it: `DLNA.ORG_OP=01` says "Range is honoured", which is exactly what
/// the relay arm does (AGENTS.md §4.1's live reading is the *other* value).
#[test]
fn build_didl_promises_the_ranges_the_push_can_keep() {
    crate::pin_test_dir();
    let didl = build_didl(
        "http://h/a.mp4",
        "标题",
        "video/mp4",
        0.0,
        0,
        VIDEO_ITEM_CLASS,
    );
    assert!(
        didl.contains("protocolInfo=\"http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0\""),
        "{didl}"
    );
    assert!(didl.contains("<dc:title>标题</dc:title>"), "{didl}");
    assert!(didl.contains("upnp:class>object.item.videoItem</upnp:class"));
    assert!(didl.ends_with("</DIDL-Lite>"));
}

/// A title is the sender's text, and it arrives over the network. `escape_text`
/// covers `&`, `<` and `>` — the three that change what the document *is*.
#[test]
fn build_didl_escapes_a_title_that_is_markup() {
    crate::pin_test_dir();
    let didl = build_didl(
        "http://h/a.mp4?x=1&y=2",
        "<img onerror=x> & co",
        "video/mp4",
        0.0,
        0,
        VIDEO_ITEM_CLASS,
    );
    assert!(didl.contains("&lt;img onerror=x&gt; &amp; co"), "{didl}");
    assert!(didl.contains("http://h/a.mp4?x=1&amp;y=2"), "{didl}");
    assert!(!didl.contains("<img"), "no markup may survive into the DIDL: {didl}");
}

/// Python `upnp_class_for` (:1668): an unknown or empty type is a video.
#[test]
fn upnp_class_for_sorts_audio_from_everything_else() {
    crate::pin_test_dir();
    assert_eq!(upnp_class_for("audio/mpeg"), AUDIO_ITEM_CLASS);
    assert_eq!(upnp_class_for("video/mp4"), VIDEO_ITEM_CLASS);
    assert_eq!(upnp_class_for("application/octet-stream"), VIDEO_ITEM_CLASS);
    assert_eq!(upnp_class_for(""), VIDEO_ITEM_CLASS);
    // `starts_with`, not a table: the case that matters is the type we have
    // never heard of, and guessing "video" is the shape that still plays.
    assert_eq!(upnp_class_for("Audio/mpeg"), VIDEO_ITEM_CLASS);
}

// ---------------------------------------------------------------------------
// Harness B's witnesses — the DLNA arm
// ---------------------------------------------------------------------------

/// `Dom` keeps original case in Clark notation (`{urn}CurrentTransportState`),
/// so the answer map is keyed on the *local* name lowercased. A raw-tag
/// `ends_with("response")` on the wrong case matches nothing and
/// `CurrentTransportState` silently reads empty — this is the arm that caught
/// it before the first run.
#[test]
fn the_dlna_arm_asks_for_the_transport_state_by_local_name() {
    crate::pin_test_dir();
    let stub = start_soap_stub(vec![(
        200,
        "OK".to_owned(),
        soap_reply(
            "GetTransportInfo",
            "<CurrentTransportState>PLAYING</CurrentTransportState>\
             <CurrentTransportStatus>OK</CurrentTransportStatus>\
             <CurrentSpeed>1</CurrentSpeed>",
        ),
    )]);
    let (log, _path) = witness("DlnaState");
    let mut sender = DlnaSender::new(dlna_url(stub.port), 5.0);
    let state = sender.transport_state(&log).expect("a well-formed answer");
    assert_eq!(state, "PLAYING");
    assert_eq!(sender.last_state, "PLAYING");

    let captured = stub.finish();
    assert_eq!(captured.len(), 1, "one verb is one connection");
    let body = &captured[0].body;
    assert!(body.contains("<u:GetTransportInfo xmlns:u="), "{body}");
    assert!(body.contains("<InstanceID>0</InstanceID>"), "{body}");
}

/// Deviation ⑤: a SOAP answer is read as lossy UTF-8, not ET-parsed for
/// encoding. A device that lies about its encoding now answers "not XML"
/// instead of an `ExpatError`-shaped failure — and the failure is a warning
/// plus an empty map, **not** an error, because the media was already handed
/// over by then.
#[test]
fn a_non_xml_transport_state_keeps_the_last_state_and_warns() {
    crate::pin_test_dir();
    let noise = (200, "OK".to_owned(), "upstream proxy error".to_owned());
    let stub = start_soap_stub(vec![
        (
            200,
            "OK".to_owned(),
            soap_reply("GetTransportInfo", "<CurrentTransportState>PLAYING</CurrentTransportState>"),
        ),
        // No markup at all — `<html><body>x</body></html>` *is* well-formed
        // XML, so a fixture written that way would test the parser instead of
        // this arm. Two of them: the bare `request` and the follow-up
        // `transport_state` are two verbs, and one verb is one connection.
        noise.clone(),
        noise,
    ]);
    let (log, path) = witness("DlnaNoXml");
    let mut sender = DlnaSender::new(dlna_url(stub.port), 5.0);
    assert_eq!(sender.transport_state(&log).unwrap(), "PLAYING");

    let values = sender
        .request("GetTransportInfo", &[], &log)
        .expect("a non-XML answer is an empty map, not an error");
    assert!(values.is_empty(), "{values:?}");
    assert_eq!(
        sender.transport_state(&log).unwrap(),
        "PLAYING",
        "a silent reply must not erase the last thing the device did say"
    );

    let text = lines(&path);
    assert!(
        text.contains("GetTransportInfo replied with something that is not XML"),
        "the refusal must name the action it failed on: {text}"
    );
    stub.finish();
}

/// A TV that answers every verb with garbage still got the media: the push is
/// reported as a success and the state reads 状态未知. Refusing to call this a
/// success would tell the user nothing they could act on — the URL went out.
#[test]
fn a_dlna_device_that_never_answers_xml_still_counts_as_pushed() {
    crate::pin_test_dir();
    let garbage = (200, "OK".to_owned(), "not xml at all".to_owned());
    let stub = start_soap_stub(vec![garbage.clone(), garbage.clone(), garbage]);
    let (log, path) = witness("DlnaGarbage");
    let setting = Setting::new(setting_file("dlna-garbage"));

    let (ok, message) = push_url_to(
        &dlna_target(stub.port, "dlna", "卧室"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message}");
    assert_eq!(message, "已投给「卧室」");

    let text = lines(&path);
    assert!(text.contains("状态未知"), "{text}");
    assert_eq!(stub.finish().len(), 3, "URI, Play, then the state question");
}

/// The device's `InstanceID` echo is a session claim: Python remembers it and
/// puts *that* value into the next envelope. The map also keeps the key — the
/// walk visits the response node and its children in document order, and the
/// `instanceid` branch reads the same node again.
#[test]
fn the_instance_id_echo_teaches_the_next_envelope() {
    crate::pin_test_dir();
    let stub = start_soap_stub(vec![
        (
            200,
            "OK".to_owned(),
            soap_reply(
                "SetAVTransportURI",
                "<InstanceID>7</InstanceID><CurrentURI>http://a</CurrentURI>\
                 <CurrentURIMetaData></CurrentURIMetaData>",
            ),
        ),
        (
            200,
            "OK".to_owned(),
            soap_reply("Play", "<InstanceID>7</InstanceID>"),
        ),
    ]);
    let (log, _path) = witness("DlnaInstance");
    let mut sender = DlnaSender::new(dlna_url(stub.port), 5.0);
    let values = sender
        .set_uri("http://a", "<DIDL-Lite/>", &log)
        .expect("a well-formed answer");
    assert_eq!(sender.instance_id, "7", "the echo teaches the next envelope");
    assert_eq!(
        values.get("instanceid").map(String::as_str),
        Some("7"),
        "the answer map keeps what it read"
    );
    assert_eq!(values.get("currenturi").map(String::as_str), Some("http://a"));

    sender.play(&log).expect("the second verb answers");
    let captured = stub.finish();
    assert!(captured[0].body.contains("<InstanceID>0</InstanceID>"), "the first envelope cannot know the session");
    assert!(captured[1].body.contains("<InstanceID>7</InstanceID>"), "the second one must: {}", captured[1].body);
}

/// AGENTS.md §4.8's red line, ported: `Content-Length` is the UTF-8 **byte**
/// count. `len(str)` truncates a Chinese device name on the wire, and the TV
/// answers with a SOAP fault that reads like "the remote is broken".
#[test]
fn the_sender_does_not_truncate_a_chinese_title() {
    crate::pin_test_dir();
    let stub = start_soap_stub(vec![
        (200, "OK".to_owned(), soap_reply("SetAVTransportURI", "")),
        (200, "OK".to_owned(), soap_reply("Play", "")),
    ]);
    let (log, _path) = witness("DlnaBytes");
    let mut sender = DlnaSender::new(dlna_url(stub.port), 5.0);
    let didl = build_didl(
        "http://example.com/片.mp4",
        "第一部：大结局",
        "video/mp4",
        0.0,
        0,
        VIDEO_ITEM_CLASS,
    );
    sender.set_uri("http://example.com/片.mp4", &didl, &log).unwrap();
    sender.play(&log).unwrap();

    let captured = stub.finish();
    let body = &captured[0].body;
    let claimed: usize = captured[0]
        .header("content-length")
        .parse()
        .expect("a numeric Content-Length");
    assert_eq!(claimed, body.as_bytes().len(), "the body must not be truncated");
    assert!(
        body.as_bytes().len() > body.chars().count(),
        "this fixture has to actually be multi-byte or the test asks nothing"
    );
    assert!(body.contains("第一部：大结局"), "and the title must be intact: {body}");
}

/// The three headers Python sends, and the only reason this module can talk
/// about proxies at all: reaching a **localhost** stub means no system proxy
/// sat in the path (AGENTS.md §4.2 — ureq's defaults read macOS' Wi-Fi proxy,
/// and the symptom was `io: Peer disconnected`).
#[test]
fn the_dlna_headers_are_the_ones_python_sends() {
    crate::pin_test_dir();
    let stub = start_soap_stub(vec![(200, "OK".to_owned(), soap_reply("Play", ""))]);
    let (log, _path) = witness("DlnaHeaders");
    let mut sender = DlnaSender::new(dlna_url(stub.port), 5.0);
    sender.play(&log).expect("the stub answered");

    let captured = &stub.finish()[0];
    assert!(captured.request_line.starts_with("POST /upnp/"), "{}", captured.request_line);
    assert_eq!(
        captured.header("soapaction"),
        format!("\"{DLNA_SERVICE}#Play\"")
    );
    assert_eq!(captured.header("content-type"), "text/xml; charset=\"utf-8\"");
    assert_eq!(captured.header("connection"), "close");
}

/// Deviation ③: `DlnaSender.timeout` is deliberately unclamped at the
/// constructor (Python clamps at `push_url_to`'s read), and rescued at the
/// socket. A negative or NaN value must neither poison the stored field nor
/// hang the verb — `Duration::from_secs_f64(-1.0)` would panic, so the rescue
/// is load-bearing rather than cosmetic.
#[test]
fn a_dlna_timeout_stays_unclamped_at_the_constructor_and_is_rescued_at_the_socket() {
    crate::pin_test_dir();
    assert_eq!(DlnaSender::new("http://x", -1.0).timeout, -1.0);
    assert!(DlnaSender::new("http://x", f64::NAN).timeout.is_nan());
    assert_eq!(socket_timeout(-1.0), DEFAULT_TIMEOUT_SECONDS);
    assert_eq!(socket_timeout(f64::NAN), DEFAULT_TIMEOUT_SECONDS);

    // Four verbs are four connections, so the script is four long. Leaving it
    // one short would push the loop into the stub's `unplanned` 500 and this
    // test would report "NaN is not dialled" — a lesson about the fixture.
    let reply = || (200, "OK".to_owned(), soap_reply("Play", ""));
    let stub = start_soap_stub(vec![reply(), reply(), reply(), reply()]);
    let (log, _path) = witness("DlnaNegative");
    for bad in [-1.0, f64::NAN, f64::NEG_INFINITY, 0.0] {
        let mut sender = DlnaSender::new(dlna_url(stub.port), bad);
        sender
            .play(&log)
            .unwrap_or_else(|e| panic!("{bad} must still be dialled: {e}"));
    }
    assert_eq!(stub.finish().len(), 4, "one connection per verb, four verbs");
}

/// A SOAP fault is a refusal the user can act on, and the card must say so:
/// ureq turns a non-2xx into an `Err` from `.send()`, so the arm fails and the
/// sentence names the device. The exact reason text belongs to ureq and is
/// asserted only as *present* — pinning it would be pinning a dependency's
/// prose.
#[test]
fn a_soap_fault_answer_fails_the_push_with_the_failure_sentence() {
    crate::pin_test_dir();
    let fault = (
        500,
        "Internal Server Error".to_owned(),
        soap_reply("SetAVTransportURI", "<error>upnp:action failed</error>"),
    );
    let stub = start_soap_stub(vec![fault]);
    let (log, path) = witness("DlnaFault");
    let setting = Setting::new(setting_file("fault"));

    let (ok, message) = push_url_to(
        &dlna_target(stub.port, "dlna", "厨房"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(!ok);
    assert!(
        message.starts_with("投给「厨房」失败："),
        "the failure must name the device: {message}"
    );
    assert!(message.len() > "投给「厨房」失败：".len(), "{message} must carry a reason");
    assert_eq!(
        stub.finish().len(),
        1,
        "a fault at the first verb means no second verb"
    );
    let text = lines(&path);
    assert!(text.contains("pushing 第一部 to 厨房 failed"), "{text}");
}

/// `items_from` only produces two kinds today, and the arm is an `else` rather
/// than a third match leg: a future kind falls into the arm whose wire it does
/// speak, never into silence. This is what makes the else *witnessed* instead
/// of merely reachable.
#[test]
fn an_unknown_target_kind_pushes_over_the_wire_it_does_speak() {
    crate::pin_test_dir();
    let ok_reply = (200, "OK".to_owned(), soap_reply("Play", ""));
    let stub = start_soap_stub(vec![
        (200, "OK".to_owned(), soap_reply("SetAVTransportURI", "")),
        ok_reply.clone(),
        (
            200,
            "OK".to_owned(),
            soap_reply("GetTransportInfo", "<CurrentTransportState>PLAYING</CurrentTransportState>"),
        ),
    ]);
    let (log, path) = witness("DlnaFutureKind");
    let setting = Setting::new(setting_file("future"));

    let (ok, message) = push_url_to(
        &dlna_target(stub.port, "miracast", "书房"),
        "http://example.com/a.mp4",
        &media("第一部", "video/mp4"),
        &setting,
        &log,
    );
    assert!(ok, "{message} — a future kind must not be dropped quietly");
    assert_eq!(message, "已投给「书房」");

    let captured = stub.finish();
    assert_eq!(captured.len(), 3, "the SOAP verbs ran, in order");
    assert!(captured[0].body.contains("SetAVTransportURI"));
    assert!(captured[1].body.contains("<u:Play"));
    let text = lines(&path);
    assert!(text.contains("): PLAYING"), "{text}");
}
