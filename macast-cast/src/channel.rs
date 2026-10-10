//! The Cast channel: TLS on 8009, the namespace-only dispatch, the sender and
//! session ledgers, volume, `LAUNCH_ERROR`, and the playback watcher.
//!
//! Python counterpart: `macast/protocol_cast.py` — this layer is
//! `ChromecastProtocol` minus the setup ports and the mDNS announcement: every
//! `_on_*` handler, both status builders, and the accept loop that
//! `macast-upnp/src/http.rs` already taught us how to write for Rust.
//!
//! ## The seam this crate stands on
//!
//! A Cast `LOAD` and a DLNA `SetAVTransportURI` are two doors into one living
//! room. Media goes out through [`store::Renderer`], and the state we report
//! back is read from [`store::StateStore`] — the same traits `macast-upnp`
//! defines and `src-tauri` implements over the one `StateTable`. Depending on
//! the traits is the whole point; depending on a second table would be the bug
//! AGENTS.md §4.8's oldest lesson says always shows up in front of a user.
//!
//! ## Key order is the wire format
//!
//! Both status faces are JSON objects whose **key order is the answer a sender
//! reads**. Python builds them as dicts, so insertion order is the wire format;
//! here [`serde_json`]'s `preserve_order` feature keeps insertion order, and the
//! builders below insert keys in the reference's order — the tests pin them
//! byte-for-byte against the reference, not against the spec's prose.
//!
//! ## What a failure here does *not* do
//!
//! A bad handshake, a malformed message, a volume value we cannot parse — none
//! of these may end the listener or another sender's session. The accept loop
//! never treats a single connection's failure as "the listener is gone"
//! (AGENTS.md §4.1's `except OSError: break` lesson), and a handler exception
//! is the client's problem: the connection is dropped, the next sender must
//! still be able to connect.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use macast_core::logging::Logger;
use macast_upnp::store::{self, SharedBroadcast, SharedRenderer, SharedStore, StateStore};

use crate::cert::Pair;
use crate::proto::{
    self, app_namespaces, encode_auth_response, encode_cast_message,
    parse_cast_message, wrap_frame, CastMessage, FRAME_HEADER_LEN, NS_CONNECTION,
    NS_DEVICEAUTH, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER,
};

pub const CAST_PORT: u16 = 8009;
pub const PORT_FALLBACK_RANGE: u16 = 20;
pub const DEFAULT_MEDIA_APP_ID: &str = "CC1AD845";
/// mkchromecast's `--hijack` treats any other displayName as "someone stole my
/// receiver" and re-issues LOAD every 5 seconds — an endless restart loop
/// against us. Senders key off this name.
pub const DEFAULT_MEDIA_APP_NAME: &str = "Default Media Receiver";
/// pychromecast's `IDLE_APP_ID` (`__init__.py:39`); real devices answer the
/// Backdrop app, and our own sender reads the same answer.
pub const APP_BACKDROP_ID: &str = "E8C28D3C";
/// The apps this receiver answers for. The mirroring app `0F5096E8` is
/// deliberately absent: before this table existed, the receiver accepted any
/// appId and answered naming someone else's app with no matching transportId,
/// and our own mirror sender then waited out its full 10 s handshake timeout
/// (measured 2026-10-05, a large part of 「投屏延迟大」).
pub const SERVED_APP_IDS: [&str; 2] = [DEFAULT_MEDIA_APP_ID, APP_BACKDROP_ID];

/// Ours, and generous. The reference is a plain list of sockets with no cap; a
/// Rust accept loop that spawns without one will happily spawn until the
/// machine refuses.
pub const MAX_CONNECTIONS: usize = 32;
/// Bound the handshake only; the session itself must not time out (senders
/// only PING every ~5s and may idle for longer).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the accept loop looks at `running`. 20 ms is well inside the
/// `WAIT_NOTIFY` a control point tolerates, and keeps a stop from spinning.
pub const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// How long to wait for the player to confirm playback before falling back
/// to the optimistic "PLAYING" reply. Long enough for a network stream to
/// start, short enough that a sender does not consider the load stalled.
pub const LOAD_GRACE_SECONDS: u64 = 8;
/// The watcher's poll step. Python waits on the stop event for 0.2 s.
pub const WATCH_POLL: Duration = Duration::from_millis(200);

pub const RECEIVER_SOURCE: &str = "receiver-0";
pub const TRANSPORT_ID: &str = "MacastTransport";

/// `media_resolve.TITLE_MAX_CHARS` — the cap `sender_media_title` truncates the
/// sender's title to. The Python value lives in `media_resolve.py`; that module
/// has no Rust counterpart yet, so the constant is pinned here and the
/// media-resolve layer must adopt it rather than mint a second one.
pub const TITLE_MAX_CHARS: usize = 120;

/// The title a sender declares, else a best-effort name for the address.
///
/// `sender_media_title` (`macast/protocol_cast.py:133`): the sender's
/// `metadata.title` when it is a non-blank string, truncated to
/// `media_resolve.TITLE_MAX_CHARS` — and even when it is absent we answer,
/// because mpv's `title` survives a later `loadfile` while a name never set
/// stays wrong for the whole session.
pub fn sender_media_title(
    media: &Value,
    url: &str,
    title_for_uri: impl FnOnce(&str) -> String,
) -> String {
    let raw = media
        .get("metadata")
        .and_then(|m| m.get("title"))
        .and_then(Value::as_str);
    match raw {
        Some(t) if !t.trim().is_empty() => truncate_chars(t.trim(), TITLE_MAX_CHARS),
        _ => title_for_uri(url),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The sender's own "is this a live stream" answer — three states, and
/// `None` is not `false` (AGENTS.md §4.1, last row): `None` means "keep today's
/// behaviour", which is the safe side for anything we do not understand.
///
/// `live_from_stream_type` (`macast/protocol_cast.py:155`): `None` unless
/// `media` is a dict **and** `streamType` is a string; then it is
/// `"LIVE"` (whitespace-trimmed, case-folded).
pub fn live_from_stream_type(media: &Value) -> Option<bool> {
    let stream = media.get("streamType")?.as_str()?;
    Some(stream.trim().eq_ignore_ascii_case("LIVE"))
}

/// `launch_refusal` (`macast/protocol_cast.py:77`): `Some(refusal)` for an app
/// we do not serve, `None` when the launch may proceed.
///
/// A missing or non-string `appId` falls back to the default media receiver —
/// a sender that cannot name its app gets the default answer, not a refusal
/// naming an app it never asked for. `reason` is protocol-visible
/// (`cast_channel.proto` `LaunchError.Reason`); `message` is the human half,
/// and our own mirror sender reads `message` **before** `reason` — every
/// refusal gives a next step (AGENTS.md §4.8).
pub fn launch_refusal(app_id: Option<&Value>, request_id: Option<Value>) -> Option<Value> {
    let asked = match app_id {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => DEFAULT_MEDIA_APP_ID.to_string(),
    };
    if SERVED_APP_IDS.contains(&asked.as_str()) {
        return None;
    }
    let message = refusal_message(&asked);
    Some(json!({
        "type": "LAUNCH_ERROR",
        "requestId": request_id,
        "appId": asked,
        "reason": "ERROR",
        "message": message,
    }))
}

fn refusal_message(app: &str) -> String {
    format!(
        "这个接收端不运行应用 {app}（本机只接受默认媒体接收器与待机画面）。屏幕镜像请把「投屏方式」改成不带「低延迟」的那一档。"
    )
}

/// `_random_session` (`macast/protocol_cast.py:1294`) = `uuid.uuid4().hex` —
/// 32 lowercase hex chars. No `uuid` crate in this crate's dependencies, so the
/// 16 random bytes come from the OS CSPRNG and the hex is ours.
pub fn random_session() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn fill_random(buf: &mut [u8]) {
    use std::fs::File;
    let mut f = File::open("/dev/urandom").expect("open /dev/urandom");
    f.read_exact(buf).expect("read /dev/urandom");
}

#[cfg(windows)]
fn fill_random(buf: &mut [u8]) {
    // RtlGenRandom (`SystemFunction036`) is the one CSPRNG that needs no
    // feature flags or extra crates; it is what `getrandom` itself reaches for
    // on older Windows.
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn SystemFunction036(random_buffer: *mut u8, random_buffer_length: u32) -> i32;
    }
    let ok = unsafe { SystemFunction036(buf.as_mut_ptr(), buf.len() as u32) };
    assert!(ok != 0, "SystemFunction036 failed");
}

/// One accepted connection. The socket lives behind a mutex so any thread —
/// the connection's own reader, the watcher, a status broadcast — can write a
/// frame, exactly the freedom Python's explicit `sock` parameter had.
type ConnStream = Mutex<Option<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>>;

/// The sender and session ledgers, the volume state, and the observation
/// window — `ChromecastProtocol.__init__`'s fields (`:364-403`) under one lock.
///
/// `_media` and `_media_live` are kept **separate** (`:383-387`): `_media` also
/// feeds MEDIA_STATUS replies and survives a second sender taking the player
/// over, and a stale "this is live" must not be what the next ordinary video
/// plays as.
struct Ledger {
    /// `source_id → connection id`. A source id rides on exactly one
    /// connection; the id survives connection teardown only until
    /// `forget_conn` runs.
    senders: HashMap<String, usize>,
    session_id: Option<String>,
    media: Option<Value>,
    media_live: Option<bool>,
    media_session_id: i64,
    position: f64,
    volume: f64,
    muted: bool,
    observed_transport: Option<String>,
    idle_reason: Option<String>,
    watch_generation: u64,
}

impl Ledger {
    fn new() -> Self {
        Ledger {
            senders: HashMap::new(),
            session_id: None,
            media: None,
            media_live: None,
            media_session_id: 1,
            position: 0.0,
            volume: 1.0,
            muted: false,
            observed_transport: None,
            idle_reason: None,
            watch_generation: 0,
        }
    }

    /// `_observed_state` (`macast/protocol_cast.py:820`).
    fn observed_state(&self) -> (String, Option<String>) {
        if self.media.is_none() {
            return ("IDLE".into(), self.idle_reason.clone());
        }
        if let Some(reason) = &self.idle_reason {
            return ("IDLE".into(), Some(reason.clone()));
        }
        (
            self.observed_transport
                .clone()
                .unwrap_or_else(|| "PLAYING".into()),
            None,
        )
    }

    /// Volume is real state, not a constant: senders compute the next volume
    /// from what we report (pychromecast does level ± 0.1), so answering 1.0
    /// forever freezes their volume slider.
    fn volume_state(&self) -> Value {
        json!({ "level": self.volume, "muted": self.muted })
    }

    /// Everything an explicit STOP/QUIT_APP or a session teardown clears, in
    /// one place, because the reference clears the same six fields at three
    /// sites (`:686-691`, `:906-912`, `:986-994`) and forgetting one of them is
    /// exactly the stale-session regression `_clear_session` exists for.
    fn wipe(&mut self) {
        self.session_id = None;
        self.media = None;
        self.media_live = None;
        self.observed_transport = None;
        self.idle_reason = None;
        self.watch_generation += 1;
    }
}

/// The Cast receiver: listener, TLS config, ledgers, and the two doors into the
/// shared living room.
pub struct CastChannel {
    log: Arc<Logger>,
    store: SharedStore,
    renderer: SharedRenderer,
    #[allow(dead_code)] // wired for broadcast when the wiring layer lands
    broadcast: SharedBroadcast,
    pub(crate) ledger: Mutex<Ledger>,
    /// `connection id → stream`. Inserted after a successful handshake, removed
    /// when the client loop ends; `stop()` drops every entry, which closes the
    /// sockets.
    pub(crate) conns: Mutex<HashMap<usize, Arc<ConnStream>>>,
    /// Test-only outbox for fake connections (a slot with no TLS stream): a
    /// handler's reply is written through [`CastChannel::write_frame`], and in
    /// tests the destination slot holds no TLS stream, so the frames land here
    /// and the wire bytes stay readable — which is how the status builders are
    /// pinned byte-for-byte against the reference.
    #[cfg(test)]
    pub(crate) test_outbox: Mutex<HashMap<usize, Vec<Vec<u8>>>>,
    running: AtomicBool,
    listener: Mutex<Option<TcpListener>>,
    config: Mutex<Option<Arc<rustls::ServerConfig>>>,
    port: Mutex<u16>,
}

impl CastChannel {
    pub fn new(
        log: Arc<Logger>,
        store: SharedStore,
        renderer: SharedRenderer,
        broadcast: SharedBroadcast,
    ) -> Arc<Self> {
        Arc::new(CastChannel {
            log,
            store,
            renderer,
            broadcast,
            ledger: Mutex::new(Ledger::new()),
            conns: Mutex::new(HashMap::new()),
            #[cfg(test)]
            test_outbox: Mutex::new(HashMap::new()),
            running: AtomicBool::new(false),
            listener: Mutex::new(None),
            config: Mutex::new(None),
            port: Mutex::new(CAST_PORT),
        })
    }

    /// The receiver's Cast port: 8009, or the first free port below it
    /// (`_start_tls_server`, `macast/protocol_cast.py:490`).
    pub fn port(&self) -> u16 {
        *self.port.lock().unwrap()
    }

    /// `start` minus the setup servers, the advertisement, and the cert
    /// generation — the caller hands us a ready [`Pair`], which is what
    /// `ensure_pair` exists to produce.
    pub fn start(self: &Arc<Self>, pair: &Pair) -> Result<(), String> {
        let config = pair.server_config().map_err(|e| e.to_string())?;
        let listener = bind_cast_port(&self.log)?;
        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(CAST_PORT);
        // Listen in the clear and wrap each *connection* instead of wrapping
        // the listener. Wrapping the listener made `accept()` run the TLS
        // handshake inline, so a single client that aborted mid-handshake
        // raised SSLError — and SSLError is an OSError, which the accept loop
        // treated as "listener is gone" and exited. Macast then stayed
        // advertised on mDNS while nothing answered on 8009: still
        // discoverable, no longer castable.
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("listener set_nonblocking: {e}"))?;
        *self.config.lock().unwrap() = Some(config);
        *self.port.lock().unwrap() = bound;
        *self.listener.lock().unwrap() = Some(listener);
        self.running.store(true, Ordering::SeqCst);
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("CAST_TLS".into())
            .spawn(move || me.accept_loop())
            .map_err(|e| format!("spawn CAST_TLS: {e}"))?;
        self.log
            .info(&format!("ChromecastProtocol started on port {bound}"));
        Ok(())
    }

    /// `stop` (`macast/protocol_cast.py:417`). A restart must be a clean slate:
    /// without this the phantom session from the previous cast survived the
    /// plugin being toggled off and on.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.senders.clear();
            ledger.wipe();
        }
        // Dropping the listener ends the accept loop's poll, and dropping every
        // connection ends each client loop in its next read — the Rust answer
        // to Python's "closing the plain listener reliably wakes the blocked
        // accept()".
        self.listener.lock().unwrap().take();
        self.conns.lock().unwrap().clear();
        self.config.lock().unwrap().take();
        self.log.info("ChromecastProtocol stopped");
    }

    /// The accept loop: nonblocking listener, 20 ms look at `running`, and a
    /// per-connection worker that owns the TLS handshake. Mirrors
    /// `macast-upnp/src/http.rs` — the in-tree precedent for "a stopped
    /// listener is not an error".
    pub(crate) fn accept_loop(self: Arc<Self>) {
        loop {
            if !self.running.load(Ordering::SeqCst) {
                return;
            }
            let (stream, addr) = {
                let guard = self.listener.lock().unwrap();
                let listener = match &*guard {
                    Some(l) => l,
                    None => return,
                };
                match listener.accept() {
                    Ok(pair) => pair,
                    // WouldBlock is the poll tick, not a failure.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        drop(guard);
                        std::thread::sleep(ACCEPT_POLL);
                        continue;
                    }
                    Err(e) => {
                        drop(guard);
                        // Transient: a full backlog, an aborted connect. Never
                        // give up the listener for it.
                        self.log
                            .warning(&format!("Cast accept failed, still listening: {e}"));
                        std::thread::sleep(ACCEPT_POLL);
                        continue;
                    }
                }
            };
            let me = Arc::clone(&self);
            let _ = std::thread::Builder::new()
                .name("CAST_CLIENT".into())
                .spawn(move || me.handle_connection(stream, addr));
        }
    }

    /// Bound the handshake only; the session itself must not time out. A bad
    /// handshake is the client's problem, not ours; the next sender must still
    /// be able to connect.
    pub(crate) fn handle_connection(self: Arc<Self>, stream: TcpStream, addr: SocketAddr) {
        // The accepted stream can inherit the listener's nonblocking flag —
        // and the bounded handshake wants blocking reads.
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        if stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).is_err() {
            return;
        }
        let Some(config) = self.config.lock().unwrap().clone() else {
            return;
        };
        let Ok(conn) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut stream = rustls::StreamOwned::new(conn, stream);
        if let Err(e) = stream.conn.complete_io(&mut stream.sock) {
            self.log
                .debug(&format!("Cast handshake failed from {addr}: {e}"));
            return;
        }
        // The session itself must not time out.
        if stream.sock.set_read_timeout(None).is_err() {
            return;
        }
        self.log.info(&format!("Cast connection from {addr}"));
        let slot = Arc::new(Mutex::new(Some(stream)));
        let conn_id = {
            let mut conns = self.conns.lock().unwrap();
            if conns.len() >= MAX_CONNECTIONS {
                None
            } else {
                let id = conns
                    .keys()
                    .copied()
                    .max()
                    .map_or(0, |m| m + 1);
                conns.insert(id, Arc::clone(&slot));
                Some(id)
            }
        };
        let Some(conn_id) = conn_id else {
            self.log
                .warning(&format!("Cast connection cap reached, dropping {addr}"));
            return;
        };
        let outcome = self.client_loop(&conn_id);
        if let Err(e) = outcome {
            self.log.debug(&format!("Cast client disconnected: {e}"));
        }
        self.conns.lock().unwrap().remove(&conn_id);
        // A sender that dies without a CLOSE (the phone sleeping, the app being
        // killed, Wi-Fi dropping — what VLC actually does) must still be
        // forgotten, otherwise the app stays "running" for the next one.
        self.forget_conn(conn_id);
    }

    /// `_handle_client` (`macast/protocol_cast.py:561`): the frame-read loop.
    pub(crate) fn client_loop(self: &Arc<Self>, conn_id: &usize) -> std::io::Result<()> {
        loop {
            let (header, body) = {
                let slot = self.conn_slot(*conn_id);
                let mut guard = slot.lock().unwrap();
                let stream = guard.as_mut().ok_or_else(broken_pipe)?;
                let mut header = [0u8; FRAME_HEADER_LEN];
                stream.read_exact(&mut header)?;
                let length = proto::frame_length(&header);
                let mut body = vec![0u8; length];
                stream.read_exact(&mut body)?;
                (header, body)
            };
            let _ = header;
            let msg = parse_cast_message(&body);
            self.on_message(*conn_id, &msg);
        }
    }

    pub(crate) fn conn_slot(&self, conn_id: usize) -> Arc<ConnStream> {
        self.conns
            .lock()
            .unwrap()
            .get(&conn_id)
            .cloned()
            .expect("connection slot exists while its client loop runs")
    }

    /// One frame out one connection. The payload is built by the caller in the
    /// reference's key order; `wrap_frame` + `encode_cast_message` own the
    /// bytes (proto.rs).
    pub(crate) fn send(&self, conn_id: usize, source_id: &str, dest_id: &str, namespace: &str, payload: Value) {
        let blob = wrap_frame(&encode_cast_message(
            source_id,
            dest_id,
            namespace,
            payload.to_string(),
        ));
        self.write_frame(conn_id, &blob);
    }

    pub(crate) fn send_binary(
        &self,
        conn_id: usize,
        source_id: &str,
        dest_id: &str,
        namespace: &str,
        bytes: Vec<u8>,
    ) {
        let blob = wrap_frame(&encode_cast_message(
            source_id,
            dest_id,
            namespace,
            proto::Payload::Binary(bytes),
        ));
        self.write_frame(conn_id, &blob);
    }

    pub(crate) fn write_frame(&self, conn_id: usize, blob: &[u8]) {
        #[cfg(test)]
        {
            // A fake connection is a slot with no TLS stream: its frames land
            // in the outbox instead, and the test reads the wire bytes back.
            if let Some(slot) = self.conns.lock().unwrap().get(&conn_id) {
                if slot.lock().unwrap().is_none() {
                    self.test_outbox
                        .lock()
                        .unwrap()
                        .entry(conn_id)
                        .or_default()
                        .push(blob.to_vec());
                    return;
                }
            }
        }
        let slot = self.conn_slot(conn_id);
        let mut guard = slot.lock().unwrap();
        if let Some(stream) = guard.as_mut() {
            if let Err(e) = stream.write_all(blob) {
                self.log.debug(&format!("Cast write failed: {e}"));
            }
        }
    }

    /// `_on_message` (`macast/protocol_cast.py:599`). Routing is **by namespace
    /// only; `destination_id` is not consulted** — senders CONNECT to our
    /// transportId but reply to whatever source_id we used, which is why the
    /// reply source is not echoed from the request.
    pub(crate) fn on_message(self: &Arc<Self>, conn_id: usize, msg: &CastMessage) {
        match msg.namespace.as_str() {
            NS_DEVICEAUTH => {
                self.log.info(&format!(
                    "Cast device-auth challenge from {} -> uncertified AuthResponse",
                    msg.source_id
                ));
                self.send_binary(
                    conn_id,
                    RECEIVER_SOURCE,
                    &msg.source_id,
                    msg.namespace.as_str(),
                    encode_auth_response(),
                );
            }
            NS_CONNECTION => self.on_connection(conn_id, msg),
            NS_HEARTBEAT => self.on_heartbeat(conn_id, msg),
            NS_RECEIVER => self.on_receiver(conn_id, msg),
            NS_MEDIA => self.on_media(conn_id, msg),
            ns => {
                let msg_type = self
                    .json_payload(msg)
                    .and_then(|d| d.get("type").and_then(Value::as_str).map(str::to_string));
                // Deliberately not answered. An unexpected reply is what wedges
                // a strict sender's state machine; logging it is the cheap half
                // — it is how we would ever learn that some client really does
                // need an INVALID_REQUEST instead of silence.
                self.log.debug(&format!(
                    "Unhandled Cast namespace {ns:?} (type {msg_type:?}) from {}",
                    msg.source_id
                ));
            }
        }
    }

    /// Decode the payload as JSON, or `None` for binary payloads that are not
    /// ours to read and for JSON that does not parse.
    fn json_payload(&self, msg: &CastMessage) -> Option<Value> {
        if msg.payload_type != 0 {
            return None;
        }
        serde_json::from_str(&msg.payload_utf8).ok()
    }

    pub(crate) fn on_connection(self: &Arc<Self>, conn_id: usize, msg: &CastMessage) {
        let Some(data) = self.json_payload(msg) else {
            return;
        };
        let msg_type = data.get("type").and_then(Value::as_str).unwrap_or("");
        match msg_type {
            "CONNECT" => {
                // Acknowledge; also lets the sender start receiving broadcasts.
                self.log
                    .info(&format!("Cast sender connected: {}", msg.source_id));
                self.ledger
                    .lock()
                    .unwrap()
                    .senders
                    .insert(msg.source_id.clone(), conn_id);
                self.send(
                    conn_id,
                    RECEIVER_SOURCE,
                    &msg.source_id,
                    NS_CONNECTION,
                    json!({ "type": "CONNECT" }),
                );
            }
            "CLOSE" => {
                self.log
                    .info(&format!("Cast sender disconnected: {}", msg.source_id));
                let last = {
                    let mut ledger = self.ledger.lock().unwrap();
                    ledger.senders.remove(&msg.source_id);
                    ledger.senders.is_empty()
                };
                if last {
                    self.clear_session("last sender closed the connection");
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_heartbeat(self: &Arc<Self>, conn_id: usize, msg: &CastMessage) {
        let Some(data) = self.json_payload(msg) else {
            return;
        };
        // Keep-alive: senders PING every ~5s.
        if data.get("type").and_then(Value::as_str) == Some("PING") {
            self.send(
                conn_id,
                RECEIVER_SOURCE,
                &msg.source_id,
                NS_HEARTBEAT,
                json!({ "type": "PONG" }),
            );
        }
    }

    pub(crate) fn on_receiver(self: &Arc<Self>, conn_id: usize, msg: &CastMessage) {
        let Some(data) = self.json_payload(msg) else {
            return;
        };
        let msg_type = data.get("type").and_then(Value::as_str).unwrap_or("");
        let request_id = data.get("requestId").cloned();
        match msg_type {
            "GET_STATUS" | "PING" => {
                self.send_receiver_status(conn_id, &msg.source_id, request_id)
            }
            "LAUNCH" => {
                if let Some(refusal) = launch_refusal(data.get("appId"), request_id.clone()) {
                    // Refuse on this connection, with the requestId the sender
                    // is waiting on, in the same round trip that a real
                    // receiver uses. Silence here would be worse than the wrong
                    // answer we used to give: the sender's own timeout decides
                    // when the user learns that this channel is not available.
                    self.log.info(&format!(
                        "Cast LAUNCH refused: appId={} from {}",
                        refusal["appId"].as_str().unwrap_or(""),
                        msg.source_id
                    ));
                    self.send(
                        conn_id,
                        RECEIVER_SOURCE,
                        &msg.source_id,
                        NS_RECEIVER,
                        refusal,
                    );
                    return;
                }
                self.ledger.lock().unwrap().session_id = Some(random_session());
                self.send_receiver_status(conn_id, &msg.source_id, request_id);
            }
            "STOP" | "QUIT_APP" => {
                // Receiver-namespace STOP is what pychromecast's quit_app()
                // sends; QUIT_APP is what the Google Cast SDK sends. Both mean
                // "close the app", and the media ledger has to go with the
                // session: answering GET_STATUS afterwards used to report
                // PLAYING for a cast the sender had already closed, because
                // only the `_session_id` was cleared.
                self.ledger.lock().unwrap().wipe();
                self.renderer.set_media_stop();
                self.send_receiver_status(conn_id, &msg.source_id, request_id);
            }
            "GET_APP_AVAILABILITY" => {
                // Report the apps we can serve — derived from the same table
                // that decides LAUNCH, because a receiver that answers
                // "available" for an app and then refuses to launch it is worse
                // than one that never heard of it: the sender retries a
                // handshake instead of falling back.
                let availability: Map<String, Value> = SERVED_APP_IDS
                    .iter()
                    .map(|app| (app.to_string(), json!("APP_AVAILABLE")))
                    .collect();
                self.send(
                    conn_id,
                    RECEIVER_SOURCE,
                    &msg.source_id,
                    NS_RECEIVER,
                    json!({
                        "type": "GET_APP_AVAILABILITY",
                        "requestId": request_id,
                        "availability": availability,
                    }),
                );
            }
            "SET_VOLUME" => {
                let empty = Map::new();
                let vol = data
                    .get("volume")
                    .and_then(Value::as_object)
                    .unwrap_or(&empty);
                self.apply_volume(vol);
                // A real receiver answers SET_VOLUME with the updated
                // RECEIVER_STATUS. pychromecast's set_volume() sits in
                // WaitResponse(REQUEST_TIMEOUT=10.0) until that reply shows up,
                // matched by requestId — staying silent made every volume
                // change stall for 10 seconds and then raise RequestTimeout on
                // the sender (mkchromecast's volume keys, Home Assistant's
                // slider).
                self.send_receiver_status(conn_id, &msg.source_id, request_id);
            }
            other => {
                // Ditto for the receiver namespace (SET_ACTIVE_INPUT_STATE and
                // the GET_APP_AVAILABILITY variants we do not answer): recorded
                // rather than answered, to avoid inventing a reply.
                self.log.debug(&format!(
                    "Unhandled Cast receiver message {other:?} from {}",
                    msg.source_id
                ));
            }
        }
    }

    /// `_apply_volume` (`macast/protocol_cast.py:722`). Everything here comes
    /// off the wire, so it is coerced defensively: an exception raised in a
    /// handler escapes to the client loop, which drops the whole sender
    /// connection.
    pub(crate) fn apply_volume(&self, vol: &Map<String, Value>) {
        if let Some(level) = vol.get("level") {
            match level.as_f64() {
                // JSON cannot carry NaN, so `as_f64` has no junk inputs; the
                // clamp is still the wire contract's shape.
                Some(v) => {
                    let level = v.clamp(0.0, 1.0);
                    self.ledger.lock().unwrap().volume = level;
                    self.renderer
                        .set_media_volume(json!((level * 100.0).round() as i64));
                }
                None => {
                    self.log
                        .debug(&format!("Ignoring malformed Cast volume level: {level}"));
                }
            }
        }
        if let Some(muted) = vol.get("muted") {
            let muted = muted.as_bool().unwrap_or(false);
            self.ledger.lock().unwrap().muted = muted;
            self.renderer.set_media_mute(json!(muted));
        }
    }

    /// Playback observations. `ProtocolGroup` fans every `set_state_*` to all
    /// children, so these are how a Chromecast-only setup learns what mpv is
    /// doing. They deliberately never touch the network: an unexpected status
    /// message is what wedges VLC's state machine (docs/Cast-AirPlay-Testing.md),
    /// so the only safe way to tell a sender that playback ended is to answer
    /// honestly the next time it asks.
    pub fn set_state_eof(&self) {
        self.note_stopped("FINISHED");
    }

    pub fn set_state_transport_error(&self) {
        self.note_stopped("ERROR");
    }

    pub fn set_state_stop(&self) {
        // mpv fires `idle` immediately after a normal end-of-file, so this must
        // not overwrite a more specific reason already recorded.
        self.note_stopped("CANCELLED");
    }

    pub fn set_state_pause(&self) {
        let mut ledger = self.ledger.lock().unwrap();
        if ledger.media.is_some() {
            ledger.observed_transport = Some("PAUSED_PLAYBACK".into());
        }
    }

    pub fn set_state_play(&self) {
        let mut ledger = self.ledger.lock().unwrap();
        ledger.observed_transport = Some("PLAYING".into());
        ledger.idle_reason = None;
    }

    /// `_note_stopped` (`macast/protocol_cast.py:796`).
    pub(crate) fn note_stopped(&self, reason: &str) {
        let mut ledger = self.ledger.lock().unwrap();
        if ledger.media.is_none() {
            return;
        }
        if reason != "ERROR" && ledger.observed_transport.is_none() {
            self.log.debug(&format!(
                "Ignoring {reason} for a media that never started playing (superseded LOAD)"
            ));
            return;
        }
        if ledger.idle_reason.is_none() {
            ledger.idle_reason = Some(reason.to_string());
        }
    }

    /// `media_is_live` (`macast/protocol_cast.py:926`): the stored answer, not
    /// a re-derivation. Stored at LOAD; `release_playback` clears it.
    pub fn media_is_live(&self) -> Option<bool> {
        self.ledger.lock().unwrap().media_live
    }

    /// `release_playback` (`macast/protocol_cast.py:946`): another protocol
    /// took the player. `_media` stays — the opinion must not.
    pub fn release_playback(&self) {
        self.ledger.lock().unwrap().media_live = None;
    }

    /// `(state, status)` from the shared state table. Missing answers degrade
    /// to `("", "")` rather than to a false failure: the transport state is
    /// DLNA-flavoured, so with DLNA switched off the table may answer
    /// `STOPPED`/`OK` regardless of what mpv is doing, and the watcher's
    /// optimistic fallback is the safe side of a missing answer.
    pub(crate) fn renderer_transport(&self) -> (String, String) {
        let read = |name: &str| self.store.read(name).map(|v| store::wire(&v));
        (read("TransportState").unwrap_or_default(), read("TransportStatus").unwrap_or_default())
    }

    pub(crate) fn start_playback_watch(
        self: &Arc<Self>,
        conn_id: usize,
        src: String,
        request_id: Option<Value>,
    ) -> bool {
        let generation = {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.watch_generation += 1;
            ledger.watch_generation
        };
        let me = Arc::clone(self);
        match std::thread::Builder::new()
            .name("CAST_STATUS".into())
            .spawn(move || me.watch_playback(conn_id, src, request_id, generation))
        {
            Ok(_) => true,
            Err(e) => {
                self.log
                    .warning(&format!("Cast status watcher failed to start: {e}"));
                false
            }
        }
    }

    /// `_watch_playback` (`macast/protocol_cast.py:1056`). Replying "PLAYING"
    /// the moment LOAD arrives was a lie: mpv has not even opened the URL yet,
    /// so a stream that never plays still looked healthy and the sender had no
    /// reason to retry. It also runs off the client thread on purpose —
    /// blocking that reader for seconds would stop us answering the sender's
    /// heartbeats, which is how a receiver gets declared dead mid-load.
    pub(crate) fn watch_playback(
        self: Arc<Self>,
        conn_id: usize,
        src: String,
        request_id: Option<Value>,
        generation: u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(LOAD_GRACE_SECONDS);
        let mut reported: Option<String> = None;
        while Instant::now() < deadline {
            if self.ledger.lock().unwrap().watch_generation != generation {
                return; // superseded by a newer LOAD
            }
            // `running` gates the accept loop, not the watcher: in tests the
            // channel is driven without `start()`, and a watcher that checked
            // it would answer nothing anywhere outside a running listener.
            std::thread::sleep(WATCH_POLL);
            let (state, status) = self.renderer_transport();
            if status == "ERROR_OCCURRED" {
                reported = Some("LOAD_FAILED".into());
                break;
            }
            if state == "PLAYING" || state == "PAUSED_PLAYBACK" {
                reported = Some(if state == "PAUSED_PLAYBACK" { "PAUSED" } else { "PLAYING" }.into());
                break;
            }
        }
        let reported = reported.unwrap_or_else(|| "PLAYING".into());
        if reported == "LOAD_FAILED" {
            self.log.warning("Cast load failed: the player reported an error");
            self.send(
                conn_id,
                TRANSPORT_ID,
                &src,
                NS_MEDIA,
                json!({ "type": "LOAD_FAILED", "requestId": request_id }),
            );
        } else {
            self.send_media_status(conn_id, &src, Some(&reported), request_id);
        }
    }

    pub(crate) fn on_media(self: &Arc<Self>, conn_id: usize, msg: &CastMessage) {
        let Some(data) = self.json_payload(msg) else {
            return;
        };
        {
            let mut ledger = self.ledger.lock().unwrap();
            if let Some(id) = data.get("mediaSessionId").and_then(Value::as_i64) {
                ledger.media_session_id = id;
            }
        }
        let msg_type = data.get("type").and_then(Value::as_str).unwrap_or("");
        let request_id = data.get("requestId").cloned();
        match msg_type {
            "LOAD" => self.on_load(conn_id, msg, &data, request_id),
            "PLAY" => {
                self.renderer.set_media_resume();
                self.send_media_status(conn_id, &msg.source_id, Some("PLAYING"), request_id);
            }
            "PAUSE" => {
                self.renderer.set_media_pause();
                self.send_media_status(conn_id, &msg.source_id, Some("PAUSED"), request_id);
            }
            "STOP" => {
                self.renderer.set_media_stop();
                // Closes the app for every connected sender, not just this one.
                self.clear_session("sender stopped playback");
                // A session teardown must not swallow this reply's terminal
                // state: the sender is waiting for IDLE/CANCELLED right now.
                self.ledger.lock().unwrap().idle_reason = Some("CANCELLED".into());
                self.send_media_status(conn_id, &msg.source_id, Some("IDLE"), request_id);
            }
            "SEEK" => {
                let position = data
                    .get("currentTime")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                self.ledger.lock().unwrap().position = position;
                // `Target` goes through as the sender's clock text; what it
                // means is the player's call
                // (store::Renderer::set_media_position).
                self.renderer.set_media_position(&format!("{position}"));
                self.send_media_status(conn_id, &msg.source_id, Some("PLAYING"), request_id);
            }
            "GET_STATUS" => {
                self.send_media_status(conn_id, &msg.source_id, None, request_id)
            }
            other => {
                // QUEUE_*, EDIT_TRACKS_INFO, SET_PLAYBACK_RATE and friends land
                // here. We do not advertise them in supportedMediaCommands, so
                // a compliant sender will not send them; answering anyway would
                // be a reply to a request we never accepted. Recorded, not
                // answered.
                self.log.debug(&format!(
                    "Unhandled Cast media message {other:?} from {}",
                    msg.source_id
                ));
            }
        }
    }

    pub(crate) fn on_load(
        self: &Arc<Self>,
        conn_id: usize,
        msg: &CastMessage,
        data: &Value,
        request_id: Option<Value>,
    ) {
        let empty = Value::Object(Map::new());
        let media = data.get("media").unwrap_or(&empty);
        let url = media
            .get("contentId")
            .or_else(|| media.get("contentUrl"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = sender_media_title(media, url, |u| self.renderer.title_for_uri(u));
        // Log what the sender says it is about to serve. This is what makes a
        // "sound but no picture" report diagnosable from the log alone: VLC
        // announces `audio/x-matroska` when it has decided to send no video at
        // all, which is indistinguishable from a receiver bug unless you record
        // it (see docs/Cast-AirPlay-Testing.md 10.6). The title is logged too:
        // "the player shows the wrong name" is otherwise a report about a field
        // we never read.
        self.log.info(&format!(
            "Cast LOAD url={url} title={title} contentType={} streamType={} duration={} tracks={}",
            py_repr(media.get("contentType").unwrap_or(&Value::Null)),
            py_repr(media.get("streamType").unwrap_or(&Value::Null)),
            data.get("duration")
                .filter(|d| !d.is_null())
                .or_else(|| media.get("duration"))
                .map(py_repr)
                .unwrap_or_else(|| "None".into()),
            media
                .get("tracks")
                .and_then(Value::as_array)
                .map(|t| t.len())
                .unwrap_or(0),
        ));
        if url.is_empty() {
            // Nothing playable: tell the sender instead of failing silently.
            self.send(
                conn_id,
                TRANSPORT_ID,
                &msg.source_id,
                NS_MEDIA,
                json!({ "type": "LOAD_FAILED", "requestId": request_id }),
            );
            return;
        }
        let live = live_from_stream_type(media);
        let start = {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.session_id = Some(random_session());
            ledger.media = Some(media.clone());
            ledger.media_live = live;
            ledger.position = data
                .get("currentTime")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            // A new LOAD starts a new observation window: whatever the
            // previous media ended with must not leak into this one.
            ledger.observed_transport = None;
            ledger.idle_reason = None;
            // `str(int(self._position))` — the reference truncates toward zero.
            (ledger.position as i64).to_string()
        };
        // `None` and `Some("0")` are the same request
        // (store::Renderer::set_media_url).
        let start = if start == "0" { None } else { Some(start) };
        self.renderer.note_media_live(live);
        self.renderer.set_media_url(url, start.as_deref());
        self.renderer.set_media_title(&title);
        self.send_media_status(conn_id, &msg.source_id, Some("BUFFERING"), request_id.clone());
        // PLAYING is reported once the player confirms it, not blindly.
        self.start_playback_watch(conn_id, msg.source_id.clone(), request_id);
    }

    /// `_send_receiver_status` (`macast/protocol_cast.py:835`). The key order
    /// inside both objects is the answer a sender reads; it is the reference's
    /// insertion order, byte for byte.
    pub(crate) fn send_receiver_status(&self, conn_id: usize, src: &str, request_id: Option<Value>) {
        let (session, volume) = {
            let ledger = self.ledger.lock().unwrap();
            (ledger.session_id.clone(), ledger.volume_state())
        };
        let mut apps: Vec<Value> = Vec::new();
        if let Some(session) = session {
            apps.push(json!({
                "appId": DEFAULT_MEDIA_APP_ID,
                "displayName": DEFAULT_MEDIA_APP_NAME,
                "sessionId": session,
                "statusText": DEFAULT_MEDIA_APP_NAME,
                "transportId": TRANSPORT_ID,
                // Senders establish a virtual connection to transportId and
                // only then speak the media namespace.
                "namespaces": app_namespaces(),
            }));
        }
        let status = json!({
            "requestId": request_id,
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": apps,
                "isActiveInput": true,
                // pychromecast's CastStatus.is_stand_by defaults to True for a
                // non-audio cast type when the key is missing (_parse_status),
                // which is what makes a working receiver show up as "standby".
                "isStandBy": false,
                "volume": volume,
            },
        });
        self.send(conn_id, RECEIVER_SOURCE, src, NS_RECEIVER, status);
    }

    /// `_forget_sender_socket` (`macast/protocol_cast.py:875`). The Cast
    /// receiver namespace has no "sender went away" event of its own, so the
    /// last connection closing is what ends the app on a real Chromecast — and
    /// senders rely on it: VLC's controller only leaves its Connecting state on
    /// the *next* RECEIVER_STATUS it receives (chromecast_ctrl.cpp,
    /// processReceiverMessage).
    pub(crate) fn forget_conn(&self, conn_id: usize) {
        let (gone, last_one_out) = {
            let mut ledger = self.ledger.lock().unwrap();
            let gone: Vec<String> = ledger
                .senders
                .iter()
                .filter(|(_, c)| **c == conn_id)
                .map(|(src, _)| src.clone())
                .collect();
            for src in &gone {
                ledger.senders.remove(src);
            }
            let last_one_out = ledger.senders.is_empty();
            (gone, last_one_out)
        };
        if !gone.is_empty() {
            self.log
                .info(&format!("Cast sender dropped: {}", gone.join(", ")));
        }
        if last_one_out {
            self.clear_session("sender connection ended");
        }
    }

    /// `_clear_session` (`macast/protocol_cast.py:894`).
    ///
    /// Regression: `_session_id` was only cleared by an explicit STOP, so after
    /// a sender disconnected the receiver kept answering GET_STATUS with
    /// `applications: [{...}]` and a sessionId nobody owned while nothing was
    /// playing. VLC reads that as "Media receiver application was already
    /// running", skips its LAUNCH, connects to the dead transportId and adopts
    /// the stale session; the phone then needed an explicit "stop casting" (or
    /// a plugin restart) before it would cast again. A receiver must report
    /// `applications: []` once the app is gone.
    pub(crate) fn clear_session(&self, reason: &str) {
        let mut ledger = self.ledger.lock().unwrap();
        if ledger.session_id.is_none() && ledger.media.is_none() {
            return;
        }
        ledger.wipe();
        drop(ledger);
        self.log.info(&format!("Cast session cleared ({reason})"));
    }

    /// `_send_media_status` (`macast/protocol_cast.py:1095`). `player_state` of
    /// `None` means "answer with what is really happening" (`_observed_state`).
    pub(crate) fn send_media_status(
        &self,
        conn_id: usize,
        src: &str,
        player_state: Option<&str>,
        request_id: Option<Value>,
    ) {
        let (state, idle_reason, snapshot) = {
            let ledger = self.ledger.lock().unwrap();
            let (state, idle_reason) = match player_state {
                None => {
                    // A GET_STATUS must answer with what is really happening.
                    // It used to answer PLAYING for as long as a media
                    // descriptor existed, so a video that had already finished
                    // still looked healthy to the sender — and a sender that
                    // never learns otherwise never offers to re-play it.
                    ledger.observed_state()
                }
                Some(state) => {
                    // pychromecast clears its idle_reason whenever the key is
                    // absent, so only the terminal states carry it (media.py,
                    // MediaStatus.update).
                    let reason = if state == "IDLE" {
                        ledger.idle_reason.clone()
                    } else {
                        None
                    };
                    (state.to_string(), reason)
                }
            };
            let snapshot = (
                ledger.media.clone(),
                ledger.position,
                ledger.media_session_id,
                ledger.volume_state(),
                ledger.session_id.clone(),
            );
            (state, idle_reason, snapshot)
        };
        let (media, position, media_session_id, volume, session) = snapshot;
        let mut entry = json!({
            "mediaSessionId": media_session_id,
            "playbackRate": 1,
            "playerState": state,
            "currentTime": position,
            "supportedMediaCommands": 15,
            "volume": volume,
            "sessionId": session.unwrap_or_else(random_session),
            "media": media,
        });
        if let Some(reason) = idle_reason {
            entry["idleReason"] = json!(reason);
        }
        let status = json!({
            "requestId": request_id,
            "type": "MEDIA_STATUS",
            "status": [entry],
        });
        self.send(conn_id, TRANSPORT_ID, src, NS_MEDIA, status);
    }
}

/// Python prints `None` for an absent field and the value itself for a present
/// one (`%s` formatting over `media.get(...)`). The LOAD diagnostic line keeps
/// that shape so a log diff against the Python receiver reads the same.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn broken_pipe() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "connection is gone")
}

/// `_start_tls_server`'s bind loop (`macast/protocol_cast.py:490`): 8009, then
/// the first free port in the fallback range.
fn bind_cast_port(log: &Logger) -> Result<TcpListener, String> {
    for port in CAST_PORT..CAST_PORT + PORT_FALLBACK_RANGE {
        match TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => return Ok(l),
            Err(e) => log.debug(&format!("Cast bind port {port} failed: {e}")),
        }
    }
    let message = "Cast TLS server failed: no free port in range";
    log.error(message);
    Err(message.to_string())
}

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
