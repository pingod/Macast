//! Device push — the sender half of the page card's device dropdown.
//!
//! The dropdown rows come from [`crate::targets`] (discovery + two tables); this
//! module is what actually *pushes a URL* at the device a row names. Python:
//! `macast/plugins/renderer/cast_local_file.py` — `CastSender` :1252-1466,
//! `DlnaSender` :1479-1594, `build_didl` :1576-1594, `MAX_HANDOFFS` :1600,
//! `_remember_handoff` :1605-1619, `upnp_class_for` :1668, `push_url_to`
//! :1675-1715.
//!
//! **Red line (acceptance ④)**: a push *tells the device what to play and
//! nothing more*. Python's `push_url_to` docstring says exactly that
//! ("Deliberately not `LocalFileRenderer._push`"), and the Rust port keeps it
//! structurally: this file never imports `macast_core::store`, never touches
//! `set_state_*`, never writes an owner or a history entry. The playback state
//! of the machine running Macast is owned by whoever is currently playing; a
//! push to a *remote* device must not disturb it.
//!
//! **Scope cut to #22-B**: `CastSender`'s media verbs beyond `load` (pause,
//! seek, volume — Python :1388-1432) and `text_tracks` (:1367-1378) are not
//! ported here; the page card's push path never issues them. `load` itself is
//! the whole contract: the URL the card resolved, its content type, whether the
//! stream is live, and the title the card shows.
//!
//! `MAX_HANDOFFS = 4` (:1600): a push that succeeds hands the open TLS socket
//! to a global list so a later media verb could reuse the session. The page
//! card's push path issues no such verb, so the oldest handoff is closed as
//! soon as the list overflows — Python does the same (a device cannot keep
//! more than four of our sockets alive for nothing). In Python the lock is
//! `self.lock` around each `sendall`; here `&mut self` on the sender is the
//! lock, and the handoff list is a global `Mutex`.
//!
//! Deliberate deviations from Python, each with its witness test (the port
//! design doc's discipline: an undocumented difference is a bug, a documented
//! one is a decision):
//!
//! 1. **`payload_matches` requires a JSON object.** Python does
//!    `data.get('type')` on the parsed payload (:1306), which raises
//!    `AttributeError` on a non-dict and — inside `_await`'s `except
//!    (socket.timeout, ssl.SSLError, OSError)`? No: `AttributeError` is not in
//!    that family, so it escapes and the push fails loudly. Here a non-dict
//!    payload simply never matches, the await times out, and the push fails
//!    with the LAUNCH-timeout sentence. Both end in `push_url_to`'s failure
//!    line; the message differs, the outcome does not.
//!    (`payload_matches_needs_a_json_object`.)
//! 2. **`split_cast_address` reads ASCII digits and refuses out-of-range
//!    ports.** Python `port.isdigit()` accepts Unicode digits and then
//!    `int()` them; a port above 65535 crashes `getaddrinfo` later with a
//!    `socket.gaierror` that reads like a DNS problem. Here non-ASCII digits
//!    fall back to `CAST_PORT` (same as Python's `port.isdigit() else
//!    CAST_PORT` arm) and an out-of-range port is refused by name.
//!    (`split_cast_address_*`.)
//! 3. **`DlnaSender.timeout` is answered finite by [`socket_timeout`].** Python
//!    builds its opener from the raw setting; a negative or NaN timeout makes
//!    urllib raise, an infinite one blocks the worker forever. The setting is
//!    clamped at read time in Python (`number(Setting.get(Timeout, 5), 5)`),
//!    so these shapes only arrive through a hand-edited JSON — and they now
//!    get the default instead of a hang. (`socket_timeout_*`.)
//! 4. **`CastSender` keeps Python's `max(1.0, timeout)` floor, and +inf is
//!    capped at [`TIMEOUT_CEILING_SECONDS`] where the socket API needs a
//!    finite number.** Python would pass `inf` into `settimeout`, which is
//!    accepted and means "block forever"; a stuck device then hangs the push
//!    until the CherryPy worker dies. (`socket_timeout_*`, push timeout e2e.)
//! 5. **A SOAP answer is read as lossy UTF-8, not ET-parsed for encoding.**
//!    Python `ET.fromstring` sniffs the XML declaration; we read the response
//!    with `String::from_utf8_lossy` and a tolerant DOM. A device that lies
//!    about its encoding now answers "not XML" instead of an
//!    `ExpatError`-shaped failure. (`a_non_xml_transport_state_*`.)
//! 6. **An await with a zero timeout falls back to the instance timeout.**
//!    Python `timeout or self.timeout` (:1291) already does this; named here
//!    because Rust's `Option<Duration>` shape could silently change it.
//! 7. **An empty cast host is refused before dialing.** Python
//!    `getaddrinfo("")` may answer loopback depending on the resolver; a row
//!    whose address lost its host should fail with a sentence, not cast to
//!    `127.0.0.1`. (`an_empty_cast_host_is_refused`.)
//! 8. **Inherited crate-wide**: compact JSON separators (Python uses
//!    `json.dumps` defaults with spaces) — serde_json has `preserve_order`
//!    enabled at the workspace root, so the *key order* of every outgoing
//!    frame still matches Python byte for byte; one error line per failure
//!    instead of Python's `logger.exception` traceback; a non-string
//!    `playerState` reads 状态未知 where Python's `str.format` of a non-str
//!    would print the repr; `LOAD.currentTime` serializes as `0.0` where
//!    Python's `int` writes `0` — both are valid JSON numbers and every
//!    receiver parses either (`a_launch_refuses_*`/the five-push e2e pin the
//!    recorded LOAD body).
//! 9. **`build_didl` with an infinite duration writes `0:00:00`.** Python
//!    `int(inf)` raises `OverflowError`; the DIDL is built from measurements
//!    that can genuinely be infinite (a growing remux file has no length),
//!    and a DIDL that says "no duration" is the honest answer.
//!    (`build_didl_*`.)

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use macast_core::logging::Logger;
use macast_core::setting::{Key, Setting};
use macast_core::timefmt::dlna_clock;
use macast_upnp::scpd::Dom;
use macast_upnp::soap;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use serde_json::{json, Value};

use crate::channel::{CAST_PORT, DEFAULT_MEDIA_APP_ID, RECEIVER_SOURCE};
use crate::proto::{
    encode_auth_challenge, encode_cast_message, frame_length, parse_cast_message, wrap_frame,
    CastMessage, Payload, FRAME_HEADER_LEN, NS_CONNECTION, NS_DEVICEAUTH, NS_MEDIA, NS_RECEIVER,
};
use crate::resolve::{no_proxy_agent, number_or};
use crate::targets::{local_name, DLNA_SERVICE, DESCRIPTION_MAX_BYTES};

/// cast_local_file.py :1668 — the `upnp:class` a DLNA push declares.
pub const VIDEO_ITEM_CLASS: &str = "object.item.videoItem";
pub const AUDIO_ITEM_CLASS: &str = "object.item.audioItem.musicTrack";

/// cast_local_file.py :1253 — `timeout=5.0`.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 5.0;
/// Deviation ④'s ceiling: `settimeout(inf)` in Python means "block forever";
/// here the socket API needs a finite number, and a day is longer than any
/// push a user is watching.
const TIMEOUT_CEILING_SECONDS: f64 = 86_400.0;

/// cast_local_file.py :1600. The global handoff list keeps at most this many
/// open Cast sessions; pushing a fifth closes the first.
pub const MAX_HANDOFFS: usize = 4;

/// The sender's own source header. Python hardcodes `sender-0` in
/// `__init__` (:1255); our *receiver* side uses `sender-1` in tests — a sender
/// is the other party, and the two roles must not share a name.
const SENDER_SOURCE: &str = "sender-0";

/// Deviation ③: answer "what timeout should this socket actually use" once.
///
/// NaN or non-positive → default (Python would raise inside urllib); infinite
/// → the ceiling; otherwise the value itself, clamped to the ceiling. Every
/// socket timeout in this module goes through it.
pub(crate) fn socket_timeout(seconds: f64) -> f64 {
    if seconds.is_nan() || seconds <= 0.0 {
        DEFAULT_TIMEOUT_SECONDS
    } else {
        seconds.min(TIMEOUT_CEILING_SECONDS)
    }
}

/// cast_local_file.py :1680 — `timeout = number(Setting.get(Timeout, 5), 5)`.
pub(crate) fn push_timeout(setting: &Setting) -> f64 {
    number_or(&setting.get(Key::Timeout, json!(5)), DEFAULT_TIMEOUT_SECONDS)
}

fn json_payload(v: Value) -> Payload {
    Payload::Text(v.to_string())
}

// ---------------------------------------------------------------------------
// TLS: the receiver never validates a sender's certificate, and a sender must
// not validate ours either. Python wraps with check_hostname=False,
// verify_mode=CERT_NONE (cast_local_file.py :1323-1325) — the Cast protocol
// authenticates by the deviceauth challenge, not by PKI.
// ---------------------------------------------------------------------------

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// Accepts every certificate, verifies handshake signatures for real.
///
/// `verify_server_cert` is where Python's `CERT_NONE` lands; the two signature
/// methods still run the real ring-backed verification, because they guard the
/// handshake's own integrity and need no trust anchor (the signature is made
/// with the certificate the peer just presented).
#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        let schemes = rustls::crypto::ring::default_provider().signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, &schemes)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        let schemes = rustls::crypto::ring::default_provider().signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, &schemes)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn insecure_client_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            Arc::new(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(NoVerifier))
                    .with_no_client_auth(),
            )
        })
        .clone()
}

// ---------------------------------------------------------------------------
// CastSender
// ---------------------------------------------------------------------------

/// Python `CastSender` (:1252-1466), minus the media verbs that are scope-cut
/// to #22. One instance drives one TLS session to one device.
pub struct CastSender {
    pub host: String,
    pub port: u16,
    pub timeout_seconds: f64,
    pub source_id: String,
    pub(crate) socket: Option<TlsStream>,
    pub transport_id: Option<String>,
    pub app_id: String,
    pub media_session_id: Value,
    request_id: u64,
}

impl CastSender {
    pub fn new(host: impl Into<String>, port: u16, timeout_seconds: f64) -> Self {
        CastSender {
            host: host.into(),
            port: if port == 0 { CAST_PORT } else { port },
            timeout_seconds: timeout_seconds.max(1.0),
            source_id: SENDER_SOURCE.to_owned(),
            socket: None,
            transport_id: None,
            app_id: DEFAULT_MEDIA_APP_ID.to_owned(),
            media_session_id: json!(1),
            request_id: 0,
        }
    }

    /// Dial, TLS-wrap, deviceauth-challenge, CONNECT. Python :1312-1336.
    ///
    /// Bare-IPv6-literal addresses (`::1:8009`) share Python's limitation: the
    /// address a dropdown row carries is `host:port` split on the *first*
    /// colon (Python `partition(':')` :1694), which mangles an IPv6 literal.
    /// Discovery never emits one (`items_from` formats `{}:{}` of an IPv4),
    /// so the caveat is documented rather than solved.
    pub fn connect(&mut self) -> io::Result<()> {
        let timeout = socket_timeout(self.timeout_seconds);
        let host = self.host.trim();
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目标地址缺少主机名",
            ));
        }
        // Python `create_connection((host, port), timeout)` tries every
        // resolved address in order; keep the last error so the failure line
        // names a real connect refusal.
        let mut last_err: Option<io::Error> = None;
        let mut stream: Option<TcpStream> = None;
        for addr in (host, self.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, Duration::from_secs_f64(timeout)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| {
            last_err
                .unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "没有可用的地址"))
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs_f64(timeout)))
            .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs_f64(timeout))))?;

        let server_name = ServerName::try_from(host.to_owned()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "目标地址无法作为 TLS 服务器名")
        })?;
        let conn = rustls::ClientConnection::new(insecure_client_config(), server_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut tls = rustls::StreamOwned::new(conn, stream);
        while tls.conn.is_handshaking() {
            tls.conn
                .complete_io(&mut tls.sock)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        }
        self.socket = Some(tls);

        // deviceauth CHALLENGE, then *wait for the reply*: Python's connect
        // does a bare `_recv()` and would mis-frame every later message if the
        // device's auth answer were left in the pipe (:1332-1335).
        self.send(
            RECEIVER_SOURCE,
            NS_DEVICEAUTH,
            Payload::Binary(encode_auth_challenge()),
        )?;
        let _ = self.recv()?;
        self.send(
            RECEIVER_SOURCE,
            NS_CONNECTION,
            json_payload(json!({"type": "CONNECT"})),
        )
    }

    /// Python `_send` (:1272-1282). `&mut self` *is* the lock: Python wraps
    /// every `sendall` in `self.lock`, and Rust's exclusive borrow is the same
    /// statement.
    fn send(&mut self, destination: &str, namespace: &str, payload: Payload) -> io::Result<()> {
        let socket = self
            .socket
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "尚未连接设备"))?;
        let blob = wrap_frame(&encode_cast_message(
            &self.source_id,
            destination,
            namespace,
            payload,
        ));
        socket.write_all(&blob)?;
        // flush() is load-bearing: rustls buffers, and the write error a dead
        // peer produces only surfaces on the flush after the write.
        socket.flush()
    }

    /// Python `_recv_exactly` (:1284-1290): fill the buffer, EOF → None.
    fn recv_exact(socket: &mut TlsStream, count: usize) -> io::Result<Option<Vec<u8>>> {
        let mut out = vec![0u8; count];
        let mut filled = 0;
        while filled < count {
            match socket.read(&mut out[filled..]) {
                Ok(0) => return Ok(None),
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(Some(out))
    }

    /// Python `_recv` (:1292-1302): 4-byte header → body → parse. An empty
    /// body is EOF, not a message.
    fn recv_frame(socket: &mut TlsStream) -> io::Result<Option<CastMessage>> {
        let header = match Self::recv_exact(socket, FRAME_HEADER_LEN)? {
            None => return Ok(None),
            Some(h) => h,
        };
        let mut header4 = [0u8; FRAME_HEADER_LEN];
        header4.copy_from_slice(&header);
        let len = frame_length(&header4);
        let body = match Self::recv_exact(socket, len)? {
            None => return Ok(None),
            Some(b) => b,
        };
        if body.is_empty() {
            return Ok(None);
        }
        Ok(Some(parse_cast_message(&body)))
    }

    fn recv(&mut self) -> io::Result<Option<CastMessage>> {
        match self.socket.as_mut() {
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "尚未连接设备")),
            Some(socket) => Self::recv_frame(socket),
        }
    }

    fn payload_matches(data: &Value, wanted: &str) -> bool {
        data.get("type").and_then(Value::as_str) == Some(wanted)
    }

    /// Python `_await` (:1304-1310): read frames until one of the wanted type
    /// arrives or the deadline passes. Timeouts, EOFs, and undecodable
    /// payloads are all "keep waiting"; the deadline is the only failure.
    fn await_message(
        &mut self,
        wanted: &str,
        timeout: Option<Duration>,
    ) -> io::Result<Option<Value>> {
        // Deviation ⑥: a zero/absent per-call timeout means the instance's.
        let budget = match timeout {
            Some(t) if !t.is_zero() => t,
            _ => Duration::from_secs_f64(socket_timeout(self.timeout_seconds)),
        };
        let deadline = Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if let Some(socket) = self.socket.as_mut() {
                let poll = remaining.max(Duration::from_millis(200));
                let _ = socket.sock.set_read_timeout(Some(poll));
            }
            let message = match self.socket.as_mut() {
                None => return Err(io::Error::new(io::ErrorKind::NotConnected, "尚未连接设备")),
                Some(socket) => match Self::recv_frame(socket) {
                    // Python catches (socket.timeout, ssl.SSLError, OSError)
                    // and returns None — a read failure is "no answer in
                    // time", not a distinct push failure.
                    Err(_) => return Ok(None),
                    Ok(None) => return Ok(None),
                    Ok(Some(m)) => m,
                },
            };
            if message.payload_type != 0 {
                continue;
            }
            let data: Value = match serde_json::from_str(if message.payload_utf8.is_empty() {
                "{}"
            } else {
                &message.payload_utf8
            }) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if Self::payload_matches(&data, wanted) {
                return Ok(Some(data));
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        self.request_id += 1;
        self.request_id
    }

    /// Python `_applications` (:1338-1341): `status.status.applications`.
    ///
    /// Python reads `(status.get('status') or {}).get('applications') or []` —
    /// a *truthy non-dict* status (a string, a list) raises `AttributeError`
    /// and the push fails loudly. Here a non-object status simply has no
    /// applications, so `launch` finds no first entry, and the missing
    /// transportId arm fires: 「目标设备没有返回 transportId」. The LAUNCH-no-reply
    /// sentence 「目标设备没有启动媒体接收器（LAUNCH 无响应）」 is a *different*
    /// arm — it only fires when `await_message` times out with no
    /// RECEIVER_STATUS at all. `push_tests` names which arm each shape lands in.
    fn applications(status: &Value) -> Vec<Value> {
        status
            .get("status")
            .and_then(Value::as_object)
            .and_then(|s| s.get("applications"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Python `launch` (:1343-1359): LAUNCH the default media receiver, wait
    /// for its RECEIVER_STATUS, CONNECT to the returned transportId.
    pub fn launch(&mut self, app_id: Option<&str>) -> io::Result<String> {
        let app = app_id
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_MEDIA_APP_ID);
        self.app_id = app.to_owned();
        let request = json!({
            "type": "LAUNCH",
            "appId": app,
            "requestId": self.next_id(),
        });
        self.send(RECEIVER_SOURCE, NS_RECEIVER, json_payload(request))?;
        let status = self
            .await_message("RECEIVER_STATUS", None)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "目标设备没有启动媒体接收器（LAUNCH 无响应）",
                )
            })?;
        let apps = Self::applications(&status);
        let entry = apps.first().cloned().unwrap_or(Value::Null);
        let transport = entry
            .get("transportId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "目标设备没有返回 transportId")
            })?
            .to_owned();
        self.send(
            &transport,
            NS_CONNECTION,
            json_payload(json!({"type": "CONNECT"})),
        )?;
        self.transport_id = Some(transport.clone());
        Ok(transport)
    }

    /// Python `load` (:1361-1386): the URL, its type, live or buffered, the
    /// title the card shows.
    pub fn load(
        &mut self,
        url: &str,
        content_type: &str,
        live: bool,
        title: &str,
        start: f64,
    ) -> io::Result<Value> {
        let transport = self
            .transport_id
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "尚未 LAUNCH，没有可用的 transportId"))?;
        let mut media = json!({
            "contentId": url,
            "streamType": if live { "LIVE" } else { "BUFFERED" },
            "metadata": {
                "metadataType": 0,
                "title": title,
            },
        });
        // An empty contentType makes some receivers guess a handler and pick
        // the audio-only one; leaving it out is the documented way. (Python
        // :1369-1371 — the key is appended *last*, after the metadata block.)
        if !content_type.is_empty() {
            media["contentType"] = json!(content_type);
        }
        let request = json!({
            "type": "LOAD",
            "requestId": self.next_id(),
            "autoplay": true,
            "currentTime": start,
            "media": media,
        });
        self.send(&transport, NS_MEDIA, json_payload(request))?;
        let status = self.await_message("MEDIA_STATUS", None)?.unwrap_or(Value::Null);
        let entries = Self::media_status_entries(&status)?;
        let entry = entries.into_iter().next().unwrap_or(Value::Null);
        // Python `entry.get('mediaSessionId', 1)` — a *present null* stays
        // null here too; the field witnesses what the device actually said.
        if let Some(obj) = entry.as_object() {
            self.media_session_id = obj
                .get("mediaSessionId")
                .cloned()
                .unwrap_or_else(|| json!(1));
        }
        Ok(entry)
    }

    /// Python :1373-1376 — `status.get('status') or []`, with the list-shape
    /// contract made explicit: a dict counts as one entry, an empty string or
    /// zero counts as none, and anything else is a device speaking nonsense —
    /// a nonsense answer must fail the push, never read as success.
    fn media_status_entries(status: &Value) -> io::Result<Vec<Value>> {
        fn not_list_error() -> io::Error {
            io::Error::new(io::ErrorKind::InvalidData, "MEDIA_STATUS 的 status 不是列表")
        }
        let raw = status.get("status").cloned().unwrap_or(Value::Null);
        match &raw {
            Value::Null => Ok(Vec::new()),
            Value::Array(items) => Ok(items.clone()),
            Value::Object(map) if !map.is_empty() => Ok(vec![raw.clone()]),
            Value::Object(_) => Ok(Vec::new()),
            Value::String(s) if s.is_empty() => Ok(Vec::new()),
            Value::String(_) => Err(not_list_error()),
            Value::Number(n) if n.as_f64() == Some(0.0) => Ok(Vec::new()),
            Value::Number(_) => Err(not_list_error()),
            Value::Bool(false) => Ok(Vec::new()),
            Value::Bool(true) => Err(not_list_error()),
        }
    }

    /// Python `close` (:1459-1465): drop the socket; errors on drop are
    /// nobody's business.
    pub fn close(&mut self) {
        self.socket = None;
    }

    /// The media session the last `load` created — what a later media verb
    /// (#22) would address.
    pub fn media_session_id(&self) -> &Value {
        &self.media_session_id
    }
}

// ---------------------------------------------------------------------------
// Handoffs — Python :1600-1619
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Handoffs {
    held: Vec<CastSender>,
}

impl Handoffs {
    /// Python `_remember_handoff` without the lock (the caller holds it):
    /// append, then close-and-drop everything past `MAX_HANDOFFS`.
    fn remember(&mut self, sender: CastSender) -> Vec<CastSender> {
        self.held.push(sender);
        let keep_from = self.held.len().saturating_sub(MAX_HANDOFFS);
        self.held.drain(..keep_from).collect()
    }
}

fn handoffs() -> &'static Mutex<Handoffs> {
    static HANDOFFS: OnceLock<Mutex<Handoffs>> = OnceLock::new();
    HANDOFFS.get_or_init(|| Mutex::new(Handoffs::default()))
}

/// Python `_remember_handoff` (:1605-1619): remember a live session, closing
/// the oldest past [`MAX_HANDOFFS`]. The sockets are closed *outside* the
/// lock — dropping a `CastSender` closes its TLS stream, and that need not
/// hold up the next push.
pub fn remember_handoff(sender: CastSender) {
    let evicted = handoffs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remember(sender);
    drop(evicted);
}

#[cfg(test)]
pub(crate) fn clear_handoffs() {
    let evicted = handoffs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .held
        .drain(..)
        .collect::<Vec<_>>();
    drop(evicted);
}

/// How many sessions are currently held. A test seam, and an honest answer to
/// "is this keeping sockets alive".
pub fn handoff_count() -> usize {
    handoffs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .held
        .len()
}

// ---------------------------------------------------------------------------
// DlnaSender
// ---------------------------------------------------------------------------

/// Python `DlnaSender` (:1479-1594): SOAP over HTTP to an AVTransport control
/// URL. `timeout` is deliberately **unclamped** here (asymmetry documented in
/// deviation ③): Python clamps at `push_url_to`'s read, and every constructor
/// call site passes an already-answered value.
pub struct DlnaSender {
    pub control_url: String,
    pub timeout: f64,
    pub instance_id: String,
    pub last_state: String,
}

impl DlnaSender {
    pub fn new(control_url: impl Into<String>, timeout: f64) -> Self {
        DlnaSender {
            control_url: control_url.into(),
            timeout,
            instance_id: "0".to_owned(),
            last_state: String::new(),
        }
    }

    /// Python `_request` (:1479-1517). One walk of the reply, two findings:
    /// the `<u:<action>Response>` children become the value map, and any
    /// non-empty `<InstanceID>` echo teaches the sender its instance id for
    /// the verbs that follow.
    fn request(
        &mut self,
        action: &str,
        args: &[(&str, &str)],
        log: &Logger,
    ) -> io::Result<BTreeMap<String, String>> {
        let fields = args
            .iter()
            .map(|(name, value)| format!("<{name}>{}</{name}>", soap::escape_text(value)))
            .collect::<Vec<_>>()
            .join("");
        let envelope = soap_envelope(action, DLNA_SERVICE, &self.instance_id, &fields);
        // Content-Length is the UTF-8 **byte** count. ureq writes it from the
        // body bytes by construction — the test pins the byte count against a
        // multi-byte title, because Python learned this the hard way
        // (AGENTS.md §4.8: `len(str)` truncates a Chinese device name).
        let payload = envelope.into_bytes();
        let response = no_proxy_agent(Duration::from_secs_f64(socket_timeout(self.timeout)))
            .post(&self.control_url)
            .header("SOAPAction", &format!("\"{DLNA_SERVICE}#{action}\""))
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("Connection", "close")
            .send(payload)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut raw = Vec::new();
        response
            .into_body()
            .as_reader()
            .take(DESCRIPTION_MAX_BYTES as u64)
            .read_to_end(&mut raw)?;
        let text = String::from_utf8_lossy(&raw);
        let dom = match Dom::parse(&text) {
            Ok(dom) => dom,
            // Python: an unparseable reply raises inside ET.fromstring, the
            // exception escapes `_request`, and `push_url_to` reports it. A
            // transport *state* read, though, is deliberately forgiving
            // (:1546-1550): log and keep going, because the push already
            // happened and a bad status read must not undo it.
            Err(_) => {
                log.warning(&format!("{action} replied with something that is not XML"));
                return Ok(BTreeMap::new());
            }
        };
        let mut values = BTreeMap::new();
        let root = dom.root();
        for node in dom.iter_all(root) {
            let tag = dom.tag(node);
            if local_name(tag) == "instanceid" {
                if let Some(text) = dom.text(node) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        self.instance_id = trimmed.to_owned();
                    }
                }
            } else if local_name(tag).ends_with("response") {
                // Dom keeps original case in Clark notation (`{urn}SetAVTransportURIResponse` —
                // `resolve_name` → `clark()` never lowercases), so the suffix must be asked on
                // the *local* name, which is what `local_name` lowercases. A raw-tag
                // `ends_with("response")` matches nothing and `CurrentTransportState`
                // silently reads empty. That is not a guess: deleting just this one
                // `local_name()` call reddens four tests in `push_tests` —
                // `the_dlna_arm_asks_for_the_transport_state_by_local_name`,
                // `the_instance_id_echo_teaches_the_next_envelope`,
                // `a_non_xml_transport_state_keeps_the_last_state_and_warns`, and
                // `an_unknown_target_kind_pushes_over_the_wire_it_does_speak`.
                for child in dom.children(node) {
                    let name = local_name(dom.tag(*child));
                    let value = dom.text(*child).unwrap_or("").trim().to_owned();
                    values.insert(name, value);
                }
            }
        }
        Ok(values)
    }

    pub fn set_uri(
        &mut self,
        url: &str,
        didl: &str,
        log: &Logger,
    ) -> io::Result<BTreeMap<String, String>> {
        self.request(
            "SetAVTransportURI",
            &[("CurrentURI", url), ("CurrentURIMetaData", didl)],
            log,
        )
    }

    pub fn play(&mut self, log: &Logger) -> io::Result<BTreeMap<String, String>> {
        self.request("Play", &[("Speed", "1")], log)
    }

    /// Python `transport_state` (:1546-1550): ask, log-and-continue on a bad
    /// answer, remember the last non-empty state.
    pub fn transport_state(&mut self, log: &Logger) -> io::Result<String> {
        let values = self.request("GetTransportInfo", &[], log)?;
        let state = values
            .get("currenttransportstate")
            .cloned()
            .unwrap_or_default();
        if !state.is_empty() {
            self.last_state = state;
        }
        Ok(self.last_state.clone())
    }

    pub fn close(&self) {}
}

/// Python `soap_envelope` (:1445-1477 region — the module-level builder).
///
/// Two string literals joined with a space in Python; `concat!` here. The
/// second literal ends with a trailing space so `s:encodingStyle` follows one
/// space exactly as Python's concatenation produces — the test pins the whole
/// envelope byte for byte.
fn soap_envelope(action: &str, service: &str, instance: &str, fields: &str) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" ",
            "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">",
            "<s:Body>",
            "<u:{action} xmlns:u=\"{service}\">",
            "<InstanceID>{instance}</InstanceID>",
            "{fields}",
            "</u:{action}>",
            "</s:Body>",
            "</s:Envelope>",
        ),
        action = action,
        service = service,
        instance = instance,
        fields = fields,
    )
}

/// Python `build_didl` (:1576-1594): the DIDL-Lite metadata a DLNA push sends.
///
/// `duration`/`size` gate on truthiness exactly as Python's `if duration:` /
/// `if size:` — zero means omit (the device will ask for a Range probe
/// instead of trusting us), and a *measured* NaN duration clamps to
/// `0:00:00` (deviation ⑨).
pub fn build_didl(
    url: &str,
    title: &str,
    content_type: &str,
    duration: f64,
    size: u64,
    upnp_class: &str,
) -> String {
    let mut attrs = vec![format!(
        "protocolInfo=\"http-get:*:{}:DLNA.ORG_OP=01;DLNA.ORG_CI=0\"",
        content_type
    )];
    if duration != 0.0 {
        attrs.push(format!("duration=\"{}\"", dlna_clock(duration)));
    }
    if size > 0 {
        attrs.push(format!("size=\"{size}\""));
    }
    format!(
        concat!(
            "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" ",
            "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
            "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
            "<item id=\"0\" parentID=\"0\" restricted=\"1\">",
            "<dc:title>{title}</dc:title>",
            "<upnp:class>{class}</upnp:class>",
            "<res {attrs}>{url}</res>",
            "</item>",
            "</DIDL-Lite>",
        ),
        title = soap::escape_text(title),
        class = upnp_class,
        attrs = attrs.join(" "),
        url = soap::escape_text(url),
    )
}

/// Python `upnp_class_for` (:1668): an unknown/empty type is a video.
pub fn upnp_class_for(content_type: &str) -> &'static str {
    if content_type.starts_with("audio/") {
        AUDIO_ITEM_CLASS
    } else {
        VIDEO_ITEM_CLASS
    }
}

/// Python :1693-1700 — the `host:port` a cast row carries.
///
/// Deviation ② lives here: ASCII digits only, and an out-of-range port is
/// refused by name instead of dying later inside `getaddrinfo`.
pub(crate) fn split_cast_address(address: &str) -> io::Result<(String, u16)> {
    let (host, port) = match address.split_once(':') {
        Some((host, port)) => (host, port),
        None => (address, ""),
    };
    let port = if port.is_empty() {
        CAST_PORT
    } else if port.bytes().all(|b| b.is_ascii_digit()) {
        match port.parse::<u64>() {
            Ok(p) if p <= u64::from(u16::MAX) => p as u16,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("目标端口超出范围：{port}"),
                ))
            }
        }
    } else {
        CAST_PORT
    };
    Ok((host.to_owned(), port))
}

/// What the page card knows about the media it resolved. Python passes these
/// as loose arguments into `push_url_to`; named here so the two arms share one
/// shape and a future verb cannot forget one.
pub struct PushMedia<'a> {
    pub title: &'a str,
    pub content_type: &'a str,
    pub duration: f64,
    pub live: bool,
}

/// The two arms of Python's `push_url_to` (:1675-1715), minus the wrapper.
///
/// No fake-abstraction seam: the cast arm and the DLNA arm are tested against
/// real localhost sockets (a scripted TLS device, a real HTTP SOAP server),
/// because a fake `Wire` would only prove the arms call each other.
fn push_inner(
    target: &crate::targets::TargetItem,
    url: &str,
    media: &PushMedia,
    timeout: f64,
) -> io::Result<(String, Option<CastSender>)> {
    if target.kind == "cast" {
        let (host, port) = split_cast_address(&target.address)?;
        let mut sender = CastSender::new(host, port, timeout);
        sender.connect()?;
        sender.launch(None)?;
        let entry = sender.load(url, media.content_type, media.live, media.title, 0.0)?;
        let state = entry
            .get("playerState")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        Ok((state, Some(sender)))
    } else {
        // Every kind that is not `cast` is a DLNA renderer (`items_from` only
        // produces the two), so the arm is an else, not a third match leg: a
        // future kind falls into the arm whose wire it does speak, never into
        // silence.
        let mut sender = DlnaSender::new(&target.address, timeout);
        let log = Logger::named(crate::targets::LOGGER_NAME);
        let didl = build_didl(
            url,
            media.title,
            media.content_type,
            media.duration,
            0,
            upnp_class_for(media.content_type),
        );
        sender.set_uri(url, &didl, &log)?;
        sender.play(&log)?;
        let state = sender.transport_state(&log)?;
        sender.close();
        Ok((state, None))
    }
}

/// Python `push_url_to` (:1675-1715).
///
/// Returns `(bool, str)` — the tuple the page card renders. On success the
/// state the device claims (or 状态未知 when it says nothing readable) is
/// logged and the sender is handed to the handoff list; on failure the error
/// is logged and returned in the message. **Never** touches our own playback
/// state — see the header's red line.
pub fn push_url_to(
    target: &crate::targets::TargetItem,
    url: &str,
    media: &PushMedia,
    setting: &Setting,
    log: &Logger,
) -> (bool, String) {
    let timeout = push_timeout(setting);
    let what = if media.title.is_empty() { url } else { media.title };
    match push_inner(target, url, media, timeout) {
        Ok((state, sender)) => {
            if let Some(sender) = sender {
                remember_handoff(sender);
            }
            let shown = if state.is_empty() { "状态未知" } else { &state };
            log.info(&format!(
                "page card pushed {what} to {} ({}): {shown}",
                target.name, target.address
            ));
            (true, format!("已投给「{}」", target.name))
        }
        Err(e) => {
            log.error(&format!(
                "pushing {what} to {} failed: {e}",
                target.name
            ));
            (false, format!("投给「{}」失败：{e}", target.name))
        }
    }
}

// ---------------------------------------------------------------------------
// push_tests module wiring
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "push_tests.rs"]
mod push_tests;
