//! The setup face: `GET /setup/eureka_info` and `GET /setup/icon.png`, served
//! twice — plain HTTP on 8008 and TLS on 8443.
//!
//! Ported from `macast/protocol_cast.py`'s `_make_setup_handler` (the handler
//! class), `_start_setup_server` and `_start_https_setup_server`. This is the
//! face a sender asks *before* it casts: pychromecast's `get_cast_type` reads
//! `https://host:8443/setup/eureka_info?params=device_info,name` to decide what
//! kind of device it found and who made it. The three facts worth knowing before
//! editing this file are all measurements of the client, not of us:
//!
//! * **the ports are hardcoded out there.** `dial.py:25-26` builds
//!   `http://{host}:8008` and `https://{host}:8443`. Neither number is negotiated,
//!   and the mDNS TXT advertises only the Cast socket port — so a setup listener
//!   bound anywhere else is a listener nobody will knock on.
//! * **`params=` is a filter over top-level keys, and the answer keeps the
//!   document's order, not the caller's.** `_eureka_info` ends in
//!   `{k: v for k, v in full.items() if k in wanted}`, so a client asking
//!   `params=device_info,name` — which is exactly what `dial.py:157` asks — gets
//!   `name` first, because that is where `name` sits in the document. Reordering
//!   to match the request would still parse, and would still be wrong.
//! * **a self-signed certificate is fine, because the client does not check.**
//!   `dial.py:133-134` sets `check_hostname = False` / `verify_mode = CERT_NONE`
//!   (as does `socket_client.py:360-361` on the Cast socket), which is why our
//!   `cert.rs` pair can serve 8443 at all. Nothing in that client branches on the
//!   `Server` or `Date` header; its only header behaviour is retrying with an
//!   empty `Host` after a **403** (`dial.py:88-96`) — and this face has no gate,
//!   so it never sends one.
//!
//! ## What the face is allowed to say, and to whom
//!
//! Nothing gates it: neither the reference nor this port requires a token, on
//! either port. That is a decision, not an oversight — the same three fields
//! (`friendly_name`, the bare USN, the version) are already broadcast to every
//! host on the subnet in the mDNS TXT records, and `?params=` only lets a caller
//! read a subset of what the announcement already gave it. The face carries no
//! credential and no control verb: it answers *what this device is*, never
//! *play this*. Adding a verb here means adding a gate here, and the management
//! gate's rules (`AGENTS.md` §4.7c) are the ones to follow when that happens.
//!
//! ## The two listeners are not the same shape
//!
//! 8008 binds **or does not**. The reference walks a twenty-port range from it
//! (`protocol_cast.py:1141`) so a busy 8008 can never fail a start, and this port
//! deliberately does not copy that, for two reasons that are the same reason:
//! `dial.py:25` hardcodes 8008, so a setup listener on 8010 is a listener nobody
//! will ever knock on — the fallback buys reachability for nobody and only keeps
//! the process warm — and on this machine the walk starts at 8009, which is the
//! Cast socket's own port. The reference can afford that overlap because it binds
//! its Cast listener *first* (`start()` calls `_start_tls_server()` before
//! `_start_setup_server()`), so 8009 is already taken and the loop moves on; a
//! shell that has not started the Cast channel yet would simply **steal the port
//! from it**. So: bind 8008, or say so and serve nothing.
//!
//! 8443 has no range in the reference either, for the same hardcoding reason, and
//! neither does it here: a taken 8443 means "this machine lost the `manufacturer`
//! field", and the log says exactly that and carries on.
//!
//! ## Deliberate divergences from the reference's bytes
//!
//! All three are in the error path, all are visible only to a client that reads
//! headers it does not read, and none of them is silent:
//!
//! * `Server:` names us (`Macast/<version>`) instead of
//!   `BaseHTTP/0.6 Python/3.12.15`. The reference's value is an artifact of the
//!   class it happens to subclass; writing CPython's name onto the wire would be
//!   a lie about who answered.
//! * A non-`GET` verb gets `501` with **no body**. Python's
//!   `BaseHTTPRequestHandler.default_request` answers 501 too, but with a
//!   generated `text/html` error page. Copying that would put a CPython template
//!   string in our bytes for a client that only reads the status code — and
//!   `dial.py` folds `HTTPError` into a warning anyway.
//! * `Date:` is not sent at all. It exists to let a cache decide freshness, and
//!   nothing on either port caches this document.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use macast_core::logging::Logger;
use macast_core::paths;
use macast_core::setting::Setting;
use macast_core::url::{parse_qs_first, parse_query};

/// `SETUP_PORT` (`protocol_cast.py:38`). Hardcoded by `dial.py:25`.
pub const SETUP_PORT: u16 = 8008;
/// `HTTPS_SETUP_PORT` (`protocol_cast.py:45`). Hardcoded by `dial.py:26`, and the
/// reason this listener exists at all.
pub const HTTPS_SETUP_PORT: u16 = 8443;

/// `thread.name` for the two accept loops (`protocol_cast.py:1146`, `:1194`).
pub const SETUP_THREAD: &str = "CAST_SETUP";
pub const SETUP_TLS_THREAD: &str = "CAST_SETUP_TLS";
/// The per-connection thread's name; the reference has no name for it because
/// `ThreadingHTTPServer` does not label its workers.
pub const SETUP_CONN_THREAD: &str = "CAST_SETUP_CONN";

/// How often an accept loop looks at `running`. Same 20 ms as `channel.rs`, same
/// reason: shutdown must not wait for an accept to happen.
pub const ACCEPT_POLL: Duration = Duration::from_millis(20);
/// A sender opens a socket and asks. Someone who opens a socket and says nothing
/// is not owed a thread: this is the whole budget for the request line.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// The handshake budget, mirroring `channel.rs`'s `HANDSHAKE_TIMEOUT` habit — the
/// TLS face is a probe endpoint, and a stalled client must release its thread.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// No request head is answered past this: 64 KiB of request line is a flood, not
/// a probe.
pub const HEAD_MAX_BYTES: usize = 64 * 1024;

/// The reference's `logging.getLogger("Chromecast")` (`protocol_cast.py:35`), so
/// the setup face's lines land in the same `logs/Chromecast.log` the Cast channel
/// writes: both are one protocol's logsplit claim, and two files named after the
/// same protocol is how §4.10's "where do I look" question comes back.
pub const LOGGER_NAME: &str = "Chromecast";

/// The one path a sender probes. Compared with `startswith`, because that is what
/// the reference does (`protocol_cast.py:1281`) — a trailing slash or an extra
/// segment is still this route.
pub const EUREKA_PATH: &str = "/setup/eureka_info";
pub const ICON_PATH: &str = "/setup/icon.png";

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// `_eureka_info` (`protocol_cast.py:1236-1276`): the device description in the
/// shape a real Chromecast returns.
///
/// `query` is the **raw query string** (everything after the `?`, undecoded) and
/// it is read by [`macast_core::url`]'s owner — the same reader the DLNA face
/// uses, because "is `%2B` a plus or a space" must not have two answers in one
/// process.
///
/// Two details are the reason this is not a map: `device_info` is **nested**
/// (a flat answer is what makes strict clients reject the shape), and
/// `capabilities.multizone_supported` must be **explicitly false** — pychromecast
/// reads `capabilities.get("multizone_supported", True)` whenever `device_info` is
/// present, so *omitting* the key claims multi-room support and makes every
/// discovery send `?params=multizone` forever, one wasted request per poll. That
/// probe answers `{}` here (see [`filter_params`]), which is the second half of
/// why the key has to exist: an empty object is a real "no", while a missing
/// `device_info` reads as a broken device.
pub fn eureka_info(setting: &Setting, query: &str) -> Value {
    let udn = setting.usn();
    let name = setting.friendly_name();
    let version = paths::version();
    let full = json!({
        "name": name,
        "device_info": {
            "manufacturer": "Macast",
            "model_name": "Macast",
            "product_name": "macast",
            "friendly_name": name,
            // A **bare** UUID, not `uuid:<...>`. pychromecast parses this with
            // `UUID(udn.replace("-", ""))` and catches the ValueError by
            // returning None for the whole `device_info` — which is how a
            // prefixed value made Macast invisible on the host-scanning path that
            // only runs when mDNS is unavailable, while the mDNS TXT `id`,
            // already bare, was fine. `Setting::usn()` hands back the bare form,
            // so this line is a *guard*, not a conversion: a future edit that
            // writes a prefix would be caught by `setup_tests`, not by a sender's
            // warning log.
            "ssdp_udn": udn,
            "mac_address": "00:00:00:00:00:00",
            "capabilities": {
                "display_supported": true,
                "audio_supported": true,
                "video_out": true,
                "audio_out": true,
                "multizone_supported": false,
            },
        },
        "ssdp_udn": udn,
        "version": version,
        "build_version": version,
        "connected": true,
        "settings": {
            "control_notifications": false,
        },
    });
    let wanted = wanted_params(query);
    if wanted.is_empty() {
        return full;
    }
    filter_params(&full, &wanted)
}

/// The names a `?params=` asks for, in the reference's exact reading:
/// `parse_qs(query).get("params")` — **then** `[0]` — then split on `,`, strip
/// each item, drop the empties (`protocol_cast.py:1264-1271`).
///
/// The order of those steps is not cosmetic, and each one is pinned at the owner
/// (`macast-core/src/url.rs`'s `parse_qs_first`):
///
/// * `?params=` asks for **nothing**, so the whole document comes back —
///   `parse_qs` drops blank values and `params` is not in the dict at all.
/// * `?params=&params=name` asks for `name`: the blank is dropped, so the
///   *second* occurrence becomes the first.
/// * `?params=a%2Cb` asks for **two** things: the value is decoded before it is
///   split, so an encoded comma is a separator, not a character.
/// * `?params=+` decodes to a space, which is *not* blank to `parse_qs` — the
///   strip below is what turns it into "no filter", and that is why the strip
///   lives here rather than inside the reader.
fn wanted_params(query: &str) -> Vec<String> {
    let pairs = parse_query(query);
    match parse_qs_first(&pairs, "params") {
        None => Vec::new(),
        Some(value) => value
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    }
}

/// `{k: v for k, v in full.items() if k in wanted}` — filtered, and still in the
/// **document's** order.
///
/// A request for `params=device_info,name` gets `name` first because `name` is
/// first in the document. A request for a name that does not exist gets an object
/// with **fewer** keys, and `params=multizone` — which pychromecast only sends
/// when it believes we are multizone-capable — gets `{}`. Both are real answers
/// rather than errors: an empty object is "no such field", while a 404 would be
/// "no such device" and a missing `device_info` reads as a broken receiver.
fn filter_params(full: &Value, wanted: &[String]) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(object) = full.as_object() {
        for (key, value) in object {
            if wanted.iter().any(|name| name == key) {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// The wire, as a decision
// ---------------------------------------------------------------------------

/// One answered request. `headers` are written in this order, then a blank line,
/// then `body`.
#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    fn write_to(&self, stream: &mut impl Write) {
        let _ = write!(stream, "HTTP/1.0 {} {}\r\n", self.status, self.reason);
        for (name, value) in &self.headers {
            let _ = write!(stream, "{name}: {value}\r\n");
        }
        let _ = stream.write_all(b"\r\n");
        let _ = stream.write_all(&self.body);
        let _ = stream.flush();
    }
}

/// `do_GET` and `default_request` folded into one pure decision over the request
/// line, so the routing can be tested without a socket and without a handshake.
///
/// `target` is the raw request-line target (`/setup/eureka_info?params=name`),
/// **undecoded** — the same rule the DLNA face applies to a path (`%2E` must not
/// become `.` there), and the reason the split on `?` is textual rather than a
/// parse of a whole URL.
pub fn answer(setting: &Setting, method: &str, target: &str) -> Response {
    let server = vec![(
        "Server".to_string(),
        format!("Macast/{}", paths::version()),
    )];
    if method != "GET" {
        return Response {
            status: 501,
            reason: "Not Implemented",
            headers: server,
            body: Vec::new(),
        };
    }
    let (path, query) = match target.find('?') {
        Some(index) => (&target[..index], &target[index + 1..]),
        None => (target, ""),
    };
    if path.starts_with(EUREKA_PATH) {
        let body = serde_json::to_vec(&eureka_info(setting, query)).unwrap_or_default();
        return Response {
            status: 200,
            reason: "OK",
            headers: vec![
                server.into_iter().next().unwrap(),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Content-Length".to_string(), body.len().to_string()),
            ],
            body,
        };
    }
    if path.starts_with(ICON_PATH) {
        // The reference declares a type and **no length**, then writes nothing: a
        // zero-byte image, which clients tolerate because it is only ever used as
        // a favicon. `Content-Length: 0` would describe the bytes more truly and
        // diverge from the shape the senders were written against, so the shape
        // wins — and this is the row the acceptance script checks by *omission*.
        return Response {
            status: 200,
            reason: "OK",
            headers: vec![
                server.into_iter().next().unwrap(),
                ("Content-Type".to_string(), "image/png".to_string()),
            ],
            body: Vec::new(),
        };
    }
    Response {
        status: 404,
        reason: "Not Found",
        headers: server,
        body: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The listeners
// ---------------------------------------------------------------------------

/// A running setup face: the plain listener always (some port in its range is
/// free), the TLS listener only when both a certificate and port 8443 were
/// available.
pub struct SetupServer {
    running: Arc<AtomicBool>,
    /// The port the **plain** listener actually bound. A sender will not use it
    /// unless the number is 8008 — that is the reference's trade too, and
    /// `src-tauri` logs it when they differ.
    pub port: u16,
    /// `Some` only when the TLS face is up on [`HTTPS_SETUP_PORT`].
    pub https_port: Option<u16>,
}

impl SetupServer {
    /// Start the plain face, then the TLS face if there is a certificate to serve.
    ///
    /// `config` is `None` when the machine has no usable cert/key pair: the plain
    /// face still starts and the TLS face logs the reference's sentence and gives
    /// up. A caller that has a pair but cannot build a config never gets here —
    /// `cert::Pair::server_config` already answered.
    pub fn start(
        setting: Arc<Setting>,
        log: Arc<Logger>,
        config: Option<Arc<rustls::ServerConfig>>,
    ) -> std::io::Result<Self> {
        // `1`, not `channel::PORT_FALLBACK_RANGE`: see the module header. The
        // width is not a knob here — it is the answer to "can a sender reach
        // this face", and only 8008 can be reached.
        Self::start_on(setting, log, config, SETUP_PORT, 1)
    }

    /// The seam. `start` is this with the port a sender hardcodes;
    /// `setup_tests` calls it with `0` and reads the port back, because a test
    /// that binds 8008 is a test that fails whenever the user's own Macast is
    /// running — and a test that binds nothing is a test that never asks whether
    /// a real socket gets a real answer.
    pub(crate) fn start_on(
        setting: Arc<Setting>,
        log: Arc<Logger>,
        config: Option<Arc<rustls::ServerConfig>>,
        plain_port: u16,
        range: u16,
    ) -> std::io::Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let port = bind_plain(&setting, &log, plain_port, range, &running)?;

        // The accept loops own their threads and those threads are detached on
        // purpose: `shutdown` flips one flag, and nothing joins. A face that
        // cannot be waited on is not a face that cannot be stopped — the loops
        // poll `running` every [`ACCEPT_POLL`] and leave.
        let https_port = match config {
            None => {
                log.info(&format!(
                    "No TLS certificate available, skipping the Cast HTTPS setup server on {HTTPS_SETUP_PORT}"
                ));
                None
            }
            Some(config) => {
                match TcpListener::bind(("0.0.0.0", HTTPS_SETUP_PORT)) {
                    Err(e) => {
                        // Not a fallback case: the client hardcodes the port, so a
                        // listener on another number would never be contacted.
                        // Losing the `manufacturer` field is cheaper than fighting
                        // a router UI or a dev server over 8443.
                        log.info(&format!(
                            "Cast HTTPS setup port {HTTPS_SETUP_PORT} unavailable ({e}); discovery still works over {port}"
                        ));
                        None
                    }
                    Ok(listener) => {
                        accept_loop(listener, setting, Arc::clone(&log), Some(config), Arc::clone(&running));
                        log.info(&format!("Cast HTTPS setup server on {HTTPS_SETUP_PORT}"));
                        Some(HTTPS_SETUP_PORT)
                    }
                }
            }
        };

        Ok(Self {
            running,
            port,
            https_port,
        })
    }

    /// `shutdown` in the reference: stop accepting. A connection already in
    /// flight finishes its one request.
    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // The listener itself goes when the handles' borrow ends; joining would
        // block a UI thread for up to one poll tick, and nothing reads the ports
        // after this point.
    }
}

/// Walk `port .. port + range`, binding the first free one, and start the plain
/// accept loop on it. The per-port failure is a DEBUG line — the reference writes
/// the same thing (`protocol_cast.py:1151`) — because on a machine with a busy
/// 8008 that is twenty ordinary lines, not a diagnosis.
fn bind_plain(
    setting: &Arc<Setting>,
    log: &Arc<Logger>,
    port: u16,
    range: u16,
    running: &Arc<AtomicBool>,
) -> std::io::Result<u16> {
    for candidate in port..port.saturating_add(range) {
        match TcpListener::bind(("0.0.0.0", candidate)) {
            Ok(listener) => {
                let listener_port = listener.local_addr()?.port();
                accept_loop(listener, Arc::clone(setting), Arc::clone(log), None, Arc::clone(running));
                // Read the number back rather than reporting `candidate`: they are
                // the same whenever a caller asks for a fixed port, and different
                // when a caller asks for `0` — which is how `setup_tests` binds an
                // ephemeral port without racing anybody for 8008. Reporting the
                // candidate would hand a test a port that has no listener on it.
                return Ok(listener_port);
            }
            Err(e) => log.debug(&format!("Cast setup bind port {candidate} failed: {e}")),
        }
    }
    log.warning("Cast setup HTTP server failed: no free port in range");
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "no free port in the setup range",
    ))
}

/// The accept loop for either face: nonblocking listener, a 20 ms look at
/// `running`, one thread per connection.
///
/// `channel.rs`'s lesson applies verbatim and is the reason nothing here uses
/// `incoming()`: **a connection that fails is not a listener that died.** A
/// sender that aborts mid-handshake is normal traffic, and swallowing it must not
/// be able to take the face off the network while mDNS still advertises it.
fn accept_loop(
    listener: TcpListener,
    setting: Arc<Setting>,
    log: Arc<Logger>,
    config: Option<Arc<rustls::ServerConfig>>,
    running: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    let name = if config.is_some() { SETUP_TLS_THREAD } else { SETUP_THREAD };
    // One `Arc` for the loop, one kept back for the message below: the loop owns
    // its clone, and a listener that never started still has to be reported by
    // whoever asked for it.
    let loop_log = Arc::clone(&log);
    let loop_setting = Arc::clone(&setting);
    let handle = thread::Builder::new().name(name.into()).spawn(move || {
        while running.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, addr)) => {
                    let setting = Arc::clone(&loop_setting);
                    let conn_log = Arc::clone(&loop_log);
                    let config = config.clone();
                    let spawned = thread::Builder::new()
                        .name(SETUP_CONN_THREAD.into())
                        .spawn(move || match config {
                            None => serve_plain(stream, &setting, &conn_log, addr),
                            Some(config) => serve_tls(stream, &setting, &conn_log, &config, addr),
                        });
                    if let Err(e) = spawned {
                        loop_log.warning(&format!("Cast setup could not start a handler: {e}"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    loop_log.warning(&format!("Cast setup accept failed, still listening: {e}"));
                    thread::sleep(ACCEPT_POLL);
                }
            }
        }
    });
    if let Err(e) = handle {
        // The listener is bound but nobody will answer: say which face, because a
        // bound-and-deaf 8008 is exactly the "still discoverable, no longer
        // castable" symptom this crate has been burned by before.
        log.error(&format!("Cast setup accept loop never started: {e}"));
    }
}

/// The plain face: one HTTP/1.0 request, then the connection is dropped —
/// `protocol_version` is `HTTP/1.0` in the reference too, so there is no
/// keep-alive to honour and a sender opens a fresh socket per probe.
fn serve_plain(mut stream: TcpStream, setting: &Setting, log: &Logger, addr: std::net::SocketAddr) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
    match read_head(&mut stream) {
        Some(head) => match request_line(&head, log) {
            Some((method, target)) => answer(setting, &method, &target).write_to(&mut stream),
            None => Response {
                status: 400,
                reason: "Bad Request",
                headers: Vec::new(),
                body: Vec::new(),
            }
            .write_to(&mut stream),
        },
        None => log.debug(&format!("Cast setup connection from {addr} said nothing")),
    }
}

/// The TLS face: bound handshake, then the same one-request service.
fn serve_tls(
    stream: TcpStream,
    setting: &Setting,
    log: &Logger,
    config: &rustls::ServerConfig,
    addr: std::net::SocketAddr,
) {
    // The accepted socket inherits the listener's nonblocking flag, and both the
    // handshake and the bounded head read want blocking reads with a timeout.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let Ok(conn) = rustls::ServerConnection::new(Arc::new(config.clone())) else {
        log.debug(&format!("Cast HTTPS setup could not build a connection for {addr}"));
        return;
    };
    let mut stream = rustls::StreamOwned::new(conn, stream);
    if let Err(e) = stream.conn.complete_io(&mut stream.sock) {
        // A failed handshake is the client's problem, not ours: log it at DEBUG
        // and let the next sender connect. This is the same judgement `channel.rs`
        // makes about a failed Cast handshake, for the same reason.
        log.debug(&format!("Cast HTTPS setup handshake failed from {addr}: {e}"));
        return;
    }
    let _ = stream.sock.set_read_timeout(Some(HEAD_TIMEOUT));
    match read_head(&mut stream) {
        Some(head) => match request_line(&head, log) {
            Some((method, target)) => {
                answer(setting, &method, &target).write_to(&mut stream);
                // Then say goodbye **properly**. The reference never does this
                // explicitly — `socketserver.shutdown_request` calls
                // `shutdown(SHUT_WR)` on an already-wrapped TLS socket, and that
                // emits the close_notify — so dropping our stream without it is
                // not the same protocol, it is a truncation. A real client
                // notices: rustls raises `UnexpectedEof` ("peer closed connection
                // without sending TLS close_notify") on a read that runs to EOF,
                // and Python's `ssl` has the same rule. Caught by `setup_tests`
                // the first time this face was driven over a real handshake —
                // the plain-TCP tests could not see it.
                let _ = stream.conn.send_close_notify();
                let _ = stream.flush();
            }
            None => Response {
                status: 400,
                reason: "Bad Request",
                headers: Vec::new(),
                body: Vec::new(),
            }
            .write_to(&mut stream),
        },
        None => log.debug(&format!("Cast HTTPS setup connection from {addr} said nothing")),
    }
}

/// Read up to the blank line that ends the header block. `None` means "nothing
/// readable arrived" — a timeout, an EOF, or a client that sent more than
/// [`HEAD_MAX_BYTES`] without ending its headers.
///
/// Works over anything that reads, which is what lets the plain and TLS faces
/// share one body: `TcpStream` and `rustls::StreamOwned` both implement `Read`.
fn read_head(io: &mut impl Read) -> Option<String> {
    let mut buffer: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        match io.read(&mut chunk) {
            Ok(0) => return finish(&mut buffer),
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(end) = header_end(&buffer) {
                    return Some(String::from_utf8_lossy(&buffer[..end]).to_string());
                }
                if buffer.len() > HEAD_MAX_BYTES {
                    return None;
                }
            }
            Err(_) => return finish(&mut buffer),
        }
    }
}

/// A partial head is still a head if it holds a request line: Python's handler
/// would have raised inside `parse_request` and answered **400**, and an
/// unanswered close is the honest equivalent of that for a probe client. Only
/// bytes after the first line are dropped.
fn finish(buffer: &mut Vec<u8>) -> Option<String> {
    if buffer.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(buffer).to_string();
    let head = text.lines().next().unwrap_or("").to_string();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// `GET /setup/eureka_info?params=name HTTP/1.1` → `("GET", target)`.
fn request_line(head: &str, log: &Logger) -> Option<(String, String)> {
    let mut parts = head.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if parts.next().is_some() {
        log.debug(&format!("Cast setup read an odd request line: {head}"));
    }
    Some((method.to_string(), target.to_string()))
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod setup_tests;
