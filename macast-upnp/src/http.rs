//! The HTTP face of the DLNA receiver: `GET /description.xml`,
//! `GET /dlna/<Service>.xml`, `POST /<service>/action`, and
//! `SUBSCRIBE`/`UNSUBSCRIBE` on `/<service>/event`.
//!
//! Python counterpart: `macast/protocol.py::DLNAHandler` — `GET` at
//! `protocol.py:3146`, `POST` at `:3153`, `SUBSCRIBE` at `:3168`,
//! `UNSUBSCRIBE` at `:3190 — mounted by `macast/server.py:140-210` with a
//! `MethodDispatcher` on `/` and a `staticdir` on `/dlna`.
//!
//! ## The three things that are not decoration
//!
//! * **Header order is the measurement.** Every one of the twenty-five answered
//!   requests in this module's witness set puts its headers out as
//!   `Content-Type, Server, Date, Allow, <variant>, Content-Length, Connection`.
//!   The variant slot is where `Ext:`, `Sid:`, `Timeout:`, `Location:`,
//!   `Last-Modified:` and `Accept-Ranges:` live, and which of them appear is a
//!   property of the **route**, not of the response — which is exactly why
//!   [`crate::soap::Reply`] has no `ext` field and this file does the pushing.
//! * **`Allow:` is gone, on purpose.** The shipped receiver emits an `Allow:`
//!   header whose value is *every uppercase name in the handler class*, because
//!   cherrypy's `MethodDispatcher` builds it with
//!   `[m for m in dir(resource) if m.isupper()]` (`_cpdispatch.py:459-488`).
//!   Measured value:
//!   `COOKIE_BROWSERS, COOKIE_JAR_NAME, FILE_GATE_DEFAULT, FILE_ROUTES, GET, HEAD, POST, POST_HELPER_PARAMS, POST_ROUTES, RESOLVE_JOBS_MAX, RESOLVE_JOB_TTL, SUBSCRIBE, UNSUBSCRIBE, _CODE_EXECUTION_PARAMS, _MANAGEMENT_PARAMS`.
//!   Six of those fifteen tokens are not HTTP methods — they are Python module
//!   constants that leaked into a wire header. A Rust port cannot reproduce that
//!   without reproducing the bug, and a corrected list would be a *different*
//!   answer, so this face emits no `Allow:` at all on any of the shapes.
//! * **A refusal is answered before the body is read.** The 411 that the shipped
//!   receiver raises comes from `RequestBody.process()`
//!   (`cherrypy/_cpreqbody.py:970-980`), which runs **before dispatch**, so a
//!   411 beats a 405 and a 413 never makes a client upload a megabyte to be
//!   told "no". [`pre_route_status`] is that gate and it is called from two
//!   places (the answer path and the connection reader) with the *same*
//!   arguments, because a size cap that only one of two callers enforces is not
//!   a cap.
//!
//! ## What a failure here does *not* do
//!
//! It does not decide *whether* an action is legal — that is
//! [`crate::soap::Soap::handle`]. It does not touch a state variable, queue a
//! GENA event, or own a subscriber: those are [`crate::store`] and
//! [`crate::eventing`]. This file's whole job is to turn bytes into a
//! [`Request`], hand it to a seam, and turn the answer back into the byte order
//! a control point was built to read.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use macast_core::logging::Logger;
use macast_core::paths;
use macast_core::setting::{self, Key, Setting};
use macast_core::timefmt;
use macast_core::token;

/// The refusal pages. CherryPy's own four bodies (1983, 1274, 1737, 1739 bytes
/// in the witness set) are a traceback-shaped HTML with a `<pre>` of the
/// exception; reproducing their prose would mean reproducing cherrypy's error
/// pages, which say nothing a control point can act on. Our body is one line.
const HTML_CONTENT_TYPE: &str = "text/html;charset=utf-8";

/// The SCPD documents. Measured **without** a charset — unlike the description
/// and the SOAP answers, which are `text/xml;charset=utf-8`. A control point
/// that sniffs this header for the encoding of `<?xml …encoding="utf-8"?>`
/// would be wrong either way, so the difference is preserved rather than
/// "fixed".
const SCPD_CONTENT_TYPE: &str = "application/xml";

/// The management-JSON fall-through. **The shipped string has a typo** — a
/// missing `=` between the charset and its value
/// (`protocol.py:2504`, `charset:utf-8`), and it is what a real control point
/// has been reading whenever it POSTs somewhere that is not an action route.
/// Kept verbatim: a header a sender already tolerates is not ours to repair.
const JSON_CONTENT_TYPE: &str = "application/json;charset:utf-8";

/// A request head is refused at this size. Ours — the shipped receiver inherits
/// cherrypy's default `max_request_header_size` of 200 KiB only when it is
/// configured, and `macast/server.py:140-210` configures nothing of the sort.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// A POST body is refused at this size, **before** it is read. Also ours, for
/// the same reason as `MAX_HEAD_BYTES`. For scale: a SOAP action envelope here
/// is a few hundred bytes.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Worker threads. Ours: cherrypy's `thread_pool` is 100 by default and
/// `server.py` does not change it, and a Rust accept loop that spawns without a
/// cap will happily spawn until the machine refuses.
const MAX_WORKERS: usize = 32;

/// The `TIMEOUT` cherrypy's `SUBSCRIBE` uses when the sender sent none
/// (`protocol.py:3171`: `TIMEOUT = TIMEOUT if TIMEOUT is not None else 'Second-1800'`).
/// Note that this is a default for **absence**, not for emptiness: an empty
/// `TIMEOUT:` header is a `ValueError` in Python, which is a 412 here.
const DEFAULT_SUBSCRIBE_TIMEOUT: &str = "Second-1800";

/// Per-connection read/write deadline. Ours, and generous: the shipped receiver
/// sets no `socket_timeout` either, so a stalled control point holds a worker
/// until cherrypy's own machinery gives up.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the accept loop looks at `running`. 20 ms is well inside the
/// `WAIT_NOTIFY` a control point tolerates for a state change, and keeps a stop
/// from spinning.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// The size of the bytes we ask the socket for at a time.
const READ_CHUNK: usize = 4096;

/// The fields that ride *next to* an action rather than being one
/// (`protocol.py:2343-2346`, verbatim: `token`, `key`, `value`, `remove`,
/// `module`, `cast-title`, `mirror-args`, `plugin-key`, `plugin-url`,
/// `plugin-type`, `candidate`, `job`, `target`). They must not be named back to
/// the caller as "unrecognised fields" — that would tell a user who just
/// correctly supplied a token that we did not understand them.
const POST_HELPER_PARAMS: [&str; 13] = [
    "token",
    "key",
    "value",
    "remove",
    "module",
    "cast-title",
    "mirror-args",
    "plugin-key",
    "plugin-url",
    "plugin-type",
    "candidate",
    "job",
    "target",
];

/// The one uploaded-file route (`protocol.py:2336`: `FILE_ROUTES`). It is a
/// field name the dispatcher does not put in `kwargs`, so it is excluded from
/// the "unrecognised fields" answer the same way the helpers are.
const FILE_ROUTE_PARAMS: [&str; 1] = ["plugin-file"];

/// Which `query=` values on `GET /api` are management actions, and what each
/// one demands first.
///
/// The gate is a **string parsed at check time**, not an enum written in the
/// table (AGENTS.md §4.7c: "门名不认识就拒绝（fail closed）"). A typo in our own
/// table must therefore be a runtime refusal that a test can ask about, not a
/// compile error that only proves the compiler was awake.
struct GetRoute {
    query: &'static str,
    gate: &'static str,
    action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// `query=cast&url=<绝对地址>`: hand a URL to the renderer.
    Cast,
    /// `query=cast-info`: the entry URL, **including the token**.
    CastInfo,
}

/// The two answers this port implements out of the shipped management API's
/// fifteen GET queries. Each is a **cumulative** check, not a choice: see
/// [`Face::gate_refusal`].
const GET_MANAGEMENT_ROUTES: [GetRoute; 2] = [
    GetRoute {
        query: "cast",
        // `GATE_TOKEN`: the loopback answer is *not* enough here. Any webpage
        // can issue `GET http://127.0.0.1:58880/api?query=cast&...` with an
        // `<img>` or `fetch`, so trusting "this came from this machine" would
        // let a random site start the player (AGENTS.md §4.7).
        gate: "token",
        action: Action::Cast,
    },
    GetRoute {
        query: "cast-info",
        // `GATE_MANAGEMENT`: loopback / HTTPS / token. This is the only place
        // the token is *revealed*, and that gating is the whole defence — if it
        // ever answers a LAN caller, the secret that guards `query=cast` is
        // gone (AGENTS.md §4.7).
        gate: "management",
        action: Action::CastInfo,
    },
];

/// How far an unrecognised `query=` value is echoed back to the caller. Ours.
/// The head is already capped by [`MAX_HEAD_BYTES`], so this only keeps the
/// log line and the JSON body a readable length.
const QUERY_ECHO_MAX: usize = 120;

/// The two refusals, shared by every route so one route cannot write its own
/// apology (AGENTS.md §4.7c: "一条路由不能自己写拒绝文案，也不能不写"). Both name
/// the way through, because a refusal that reads like a wrong password sends
/// the user to the wrong settings field.
const MANAGEMENT_REFUSAL: &str =
    "管理接口只回答本机、HTTPS 或带管理令牌的请求。令牌在设置页「状态 → 网页投屏入口」，\
     也可以在 Macast 所在的这台机器上访问。";
const TOKEN_REFUSAL: &str =
    "这个查询必须带 token=<管理令牌>，本机也不例外：任何网页都能向 127.0.0.1 发一个 GET。\
     令牌在设置页「状态 → 网页投屏入口」。";

/// The `GET /` answer.
///
/// **This is a deviation with a reason.** The shipped receiver serves the whole
/// settings page from this port — a single-file Vue2 + Element UI document,
/// measured at **227,140 bytes**, whose markup lives in
/// `macast/xml/setting.html`. That document is a *frontend*, not a DLNA face:
/// the Tauri shell loads it in its own window (`tauri.conf.json`'s
/// `frontendDist`), so this port has no page to hand out. An empty 303 would
/// read as "broken"; this says which four addresses this port does serve.
const ROOT_PAGE: &str = "<html><head><meta charset=\"utf-8\"><title>Macast</title></head>\
                         <body><h2>Macast DLNA 接收端</h2>\
                         <p>这个端口只提供 DLNA 需要的四个地址：\
                         <code>/description.xml</code>、<code>/dlna/&lt;Service&gt;.xml</code>、\
                         <code>/&lt;service&gt;/action</code>、<code>/&lt;service&gt;/event</code>。</p>\
                         <p>设置页面在 Macast 应用自己的窗口里，不在这个端口上。</p></body></html>";

/// The verbs this face knows.
///
/// Dispatch matching is **case-insensitive**, which is not a style choice:
/// cherrypy's `Dispatcher` looks the method up with
/// `request.method.replace(' ', '_').lower()` (`_cpdispatch.py`), so
/// `subscribe`, `SUBSCRIBE` and `SuBsCrIbE` all reach the same function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
    Get,
    Head,
    Post,
    Subscribe,
    Unsubscribe,
    Other,
}

impl Verb {
    pub fn of(method: &str) -> Verb {
        if method.eq_ignore_ascii_case("GET") {
            Verb::Get
        } else if method.eq_ignore_ascii_case("HEAD") {
            Verb::Head
        } else if method.eq_ignore_ascii_case("POST") {
            Verb::Post
        } else if method.eq_ignore_ascii_case("SUBSCRIBE") {
            Verb::Subscribe
        } else if method.eq_ignore_ascii_case("UNSUBSCRIBE") {
            Verb::Unsubscribe
        } else {
            Verb::Other
        }
    }

    /// Verbs that may arrive with no `Content-Length` at all.
    ///
    /// **This set is empirical, not source-derived, and it is documented as
    /// such.** The shipped receiver answers 411 for a body-less `PUT` and does
    /// not answer 411 for a body-less `GET`/`HEAD`/`SUBSCRIBE`/`UNSUBSCRIBE`
    /// (witness rows 11, 15, 16-18, 22, 24), but the place cherrypy decides
    /// that could not be located in the installed source. The measurement is
    /// the evidence; if cherrypy changes its mind on another host, this line is
    /// the thing to re-measure. A wrong answer here is *safe in both
    /// directions*: a verb wrongly exempt gets its normal route, a verb wrongly
    /// non-exempt hears 411 instead of its normal refusal.
    pub fn exempt(self) -> bool {
        matches!(self, Verb::Get | Verb::Head | Verb::Subscribe | Verb::Unsubscribe)
    }
}

/// A request head as it was parsed, before any routing decision.
#[derive(Clone, Debug)]
pub struct Head {
    pub method: String,
    pub target: String,
    /// Header names kept **verbatim**, in the order the sender wrote them.
    /// Lookup ignores case ([`find_header`]); the stored spelling does not, so a
    /// sender that writes `sid:` and one that writes `SID:` both get answered,
    /// and a test can still assert what we echoed.
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// `Err(status)` — always 400 here.
    ///
    /// Three refusals, all ours (the witness set contains no 400 row, so the
    /// reason phrase is ours too): a head that is not UTF-8, a request line that
    /// is not exactly three whitespace-separated parts (cherrypy does
    /// `.strip().split()` and unpacks three), and a header line with no colon.
    /// A header's *value* is everything after the first colon, trimmed — so a
    /// `TIMEOUT: Second-1800` keeps its hyphen and a `SID: uuid:…` keeps its
    /// colons.
    ///
    /// Lines are split on `\n` and one trailing `\r` is dropped, so an interior
    /// bare LF still parses; the **terminator** rule stays strict `\r\n\r\n`
    /// (see [`scan_head`]). Obsolete header folding (a continuation line that
    /// starts with a space) is not handled: it is not something any UPnP stack
    /// emits, and getting it wrong is worse than not doing it.
    pub fn parse(block: &[u8]) -> Result<Head, u16> {
        let text = std::str::from_utf8(block).map_err(|_| 400u16)?;
        let mut lines = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| !line.is_empty());
        let request_line = lines.next().ok_or(400u16)?;
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(400);
        }
        let mut headers = Vec::new();
        for line in lines {
            // A continuation line is a fold we do not support; a line without a
            // colon is simply not a header.
            let (name, value) = line.split_once(':').ok_or(400u16)?;
            if name.trim().is_empty() {
                return Err(400);
            }
            headers.push((name.to_string(), value.trim().to_string()));
        }
        Ok(Head {
            method: parts[0].to_string(),
            target: parts[1].to_string(),
            headers,
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

/// Where the head reader is.
#[derive(Debug)]
pub enum HeadState {
    /// A complete head, and how many bytes of the buffer it consumed — the rest
    /// of which is already body.
    Ready { head: Head, consumed: usize },
    /// No terminator yet and still room.
    Incomplete,
    /// Refuse this, with this status, before the rest arrives.
    Bad(u16),
}

/// Find the end of the request head.
///
/// The terminator is `\r\n\r\n`, exactly — not "a blank line". The strict form
/// is what lets `consumed` be the byte offset the body starts at, and a sender
/// that uses a bare LF never gets an answer rather than getting a wrong one.
pub fn scan_head(buf: &[u8]) -> HeadState {
    match find_head_end(buf) {
        Some(index) => match Head::parse(&buf[..index]) {
            Ok(head) => HeadState::Ready {
                head,
                consumed: index + 4,
            },
            Err(status) => HeadState::Bad(status),
        },
        None => {
            if buf.len() >= MAX_HEAD_BYTES {
                HeadState::Bad(400)
            } else {
                HeadState::Incomplete
            }
        }
    }
}

/// The index of `\r\n\r\n`, without scanning more than we must.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    // `windows(4)` reads as what it is; the search is over the head, which is
    // bounded by MAX_HEAD_BYTES, so no hot loop is being hidden here.
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// A parsed request: head, path split the way the dispatcher splits it, and the
/// body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    /// The dispatcher's `vpath`: every real segment of the path, in order.
    ///
    /// Two rules decide this, both measured off `macast/protocol.py:3146-3202`
    /// and cherrypy's `find_handler` (`_cpdispatch.py:293-446`):
    /// * Extras are **swallowed**, not rejected — `DLNAHandler.GET` binds only
    ///   `param` and takes `*args`, so `/description.xml/x` answers 200.
    /// * `%2F` becomes `/` **after** the split (`vpath = [x.replace('%2F','/')
    ///   for x in vpath]`), so an encoded slash never creates a boundary.
    ///
    /// Nothing else is decoded, and that is deliberate: a full percent-decoder
    /// would turn `/description%2Exml` into the description, whereas the
    /// measured receiver answers 303 for it, because the filesystem lookup that
    /// follows is on the *undecoded* name.
    pub segments: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub verb: Verb,
    /// The peer address of the socket this request arrived on, as `ip.to_string()`
    /// — **without** the port, and `""` when the socket could not say.
    ///
    /// This is the management gate's only piece of evidence about who is asking,
    /// and it is a weak one by design: it is the *TCP peer*, which a proxy would
    /// turn into the proxy's own address. There is no `X-Forwarded-For` reading
    /// here, deliberately — trusting a header the client writes is the mistake
    /// the gate exists to avoid.
    ///
    /// Empty means "unknown", and unknown is **not** loopback: a gate must fail
    /// closed on a socket that cannot answer.
    pub remote_ip: String,
}

impl Request {
    pub fn new(head: Head, body: Vec<u8>) -> Request {
        // Everything the borrow of `head.target` produces is turned into owned
        // values **before** `head` is taken apart, because `target` is the raw
        // request line and the readers below keep the whole of it: a control
        // point that asks for `/dlna/AVTransport.xml?t=1` must still be
        // redirectable to the exact URL it asked for.
        let method = head.method;
        let verb = Verb::of(&method);
        let target = head.target;
        let (path, query_string) = split_once_ascii(&target, '?');
        let path_owned = path.to_string();
        let query = parse_query(query_string);
        let segments = path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.replace("%2F", "/"))
            .collect();
        Request {
            method,
            target,
            path: path_owned,
            query,
            segments,
            headers: head.headers,
            body,
            verb,
            // A `Request` built from bytes alone has no socket to ask, so it
            // carries "unknown" — which the gate reads as *not* loopback.
            // `serve_stream` overwrites this with the real peer.
            remote_ip: String::new(),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// The declared body length, if the sender declared a usable one.
    pub fn declared_length(&self) -> Option<i64> {
        declared_length(&self.headers)
    }
}

/// Case-insensitive header lookup, shared by every reader in this file.
///
/// `name` is a literal at every call site, so the return borrows `headers`;
/// the lifetime is spelled out because two input references leave the compiler
/// to guess.
pub fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The body length as a number, or `None` for "absent, empty, or not a length".
///
/// A **negative** value is `None` rather than a length: Python would hand
/// `read(-1)` to a file object and read to EOF, which is not a cap. Both this
/// and [`pre_route_status`] answer such a request without reading a body.
fn declared_length(headers: &[(String, String)]) -> Option<i64> {
    let raw = find_header(headers, "Content-Length")?;
    match raw.trim().parse::<i64>() {
        Ok(value) if value >= 0 => Some(value),
        _ => None,
    }
}

fn split_once_ascii(haystack: &str, needle: char) -> (&str, &str) {
    match haystack.find(needle) {
        Some(index) => (&haystack[..index], &haystack[index + 1..]),
        None => (haystack, ""),
    }
}

/// `a=1&b&c=` → `[("a","1"), ("b",""), ("c","")]`.
///
/// A pair with no `=` keeps its name and an empty value; an **empty** pair
/// (from `&&`, or a trailing `&`) is skipped. No percent-decoding: the only
/// readers of these pairs are the `?query=` route keys and the form field names
/// in the "unrecognised fields" answer, and both show the sender back what it
/// wrote. cherrypy does decode, so a name containing a percent escape is
/// spelled differently here — a deviation that changes only a message we
/// invent, never a routing decision.
fn parse_query(query_string: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for piece in query_string.split('&') {
        if piece.is_empty() {
            continue;
        }
        let (name, value) = split_once_ascii(piece, '=');
        pairs.push((name.to_string(), value.to_string()));
    }
    pairs
}

/// The gate that runs **before dispatch**, and therefore before a body is read.
///
/// `None` means "answer the request". `Some(status)` means "refuse it", with
/// this status, and nothing else — not the route, not the SOAP layer — gets to
/// decide.
///
/// The order is the whole contract, and each step says who put it there:
/// 1. An unparseable `Content-Length` is **400** (ours; no witness row reaches
///    it, so the phrase is ours too). It is first because a length we cannot
///    read must not fall through to "no length was declared" — that reading
///    would tell a sender that sent a broken header to send a header.
/// 2. A non-exempt verb with **neither** length header is **411**. This is the
///    measured cherrypy behaviour (`_cpreqbody.py:970-980`), and being
///    pre-route is what makes 411 beat the 405 a `PUT` would otherwise earn.
/// 3. A `POST` with no `Content-Length` is also **411** — even when the sender
///    used `Transfer-Encoding: chunked`. **This one is a deviation**: the
///    shipped `POST` reaches
///    `cherrypy.request.headers['Content-Length']` **unguarded**
///    (`protocol.py:3154`), so a chunked POST raises `KeyError` there and the
///    sender gets an HTML 500. Chunked bodies are not decoded here at all, so
///    the honest answer is "I need a length", and 411 is that answer in one
///    round trip instead of a traceback.
/// 4. A declared body above [`MAX_BODY_BYTES`] is **413** (ours), refused
///    *before* the bytes arrive.
///
/// When both headers are present we follow `Content-Length` exactly as Python
/// does — there is no `Transfer-Encoding` precedence to invent, and the witness
/// rows show a `Content-Length` on every answered request.
pub fn pre_route_status(verb: &Verb, headers: &[(String, String)]) -> Option<u16> {
    if find_header(headers, "Content-Length").is_some() && declared_length(headers).is_none() {
        return Some(400);
    }
    let declared = declared_length(headers);
    let transfer = find_header(headers, "Transfer-Encoding").is_some();
    if !verb.exempt() && declared.is_none() && !transfer {
        return Some(411);
    }
    if *verb == Verb::Post && declared.is_none() {
        return Some(411);
    }
    if declared.map(|value| value > MAX_BODY_BYTES as i64) == Some(true) {
        return Some(413);
    }
    None
}

/// What [`Face::answer`] decided, in the shape this file puts on the wire.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    /// The variant slot — `Ext`, `Sid`, `Timeout`, `Location`, `Last-Modified`,
    /// `Accept-Ranges` — in **push order**, because that order is the
    /// measurement (`Last-Modified` before `Accept-Ranges`, as rows 2-4 show).
    pub variant: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// HEAD: the body is computed, its length is advertised, and it is not
    /// written. Witness row 11 is exactly that: `Content-Length: 2205` and a
    /// 0-byte body.
    pub omit_body: bool,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Response {
        Response {
            status,
            content_type,
            variant: Vec::new(),
            body,
            omit_body: false,
        }
    }

    /// The wire bytes, with a caller-supplied date and `Server` value.
    ///
    /// Split out from [`Response::to_bytes`] so a test can assert the *order*
    /// without racing a second boundary, and so the measured slot layout —
    /// variant after `Date`, before `Content-Length` — is written down once.
    pub(crate) fn wire(&self, date: &str, server: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"HTTP/1.1 ");
        out.extend_from_slice(self.status.to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(Self::reason(self.status).as_bytes());
        out.extend_from_slice(b"\r\n");
        push_header(&mut out, b"Content-Type", self.content_type.as_bytes());
        push_header(&mut out, b"Server", server.as_bytes());
        push_header(&mut out, b"Date", date.as_bytes());
        for (name, value) in &self.variant {
            push_header(&mut out, name.as_bytes(), value.as_bytes());
        }
        // The length is the body's length even when the body is not written.
        push_header(&mut out, b"Content-Length", self.body.len().to_string().as_bytes());
        push_header(&mut out, b"Connection", b"close");
        out.extend_from_slice(b"\r\n");
        if !self.omit_body {
            out.extend_from_slice(&self.body);
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.wire(
            &timefmt::rfc1123_gmt(timefmt::now_secs()),
            &paths::server_info(),
        )
    }

    /// Reason phrases come from two places, and the split matters for honesty.
    /// The five marked *measured* below are the exact strings in the witness
    /// set's status lines. The four marked *ours* never appear there — 400
    /// because no head in the set was malformed, 405 because every request in
    /// the set used a known verb, 413 because nothing in the set was over our
    /// (invented) cap, 403 because the set's refusals are cherrypy's own
    /// traceback pages, which this port does not reproduce — so those phrases
    /// are this port's choice, not a copy of cherrypy's. An unknown status
    /// answers the empty string, which is a broken status line; that is
    /// deliberate: a status we do not model should fail loudly rather than
    /// invent a second label.
    pub fn reason(status: u16) -> &'static str {
        match status {
            200 => "OK",                                // measured
            303 => "See Other",                         // measured
            400 => "Bad Request",                       // ours
            403 => "Forbidden",                         // ours
            405 => "Method Not Allowed",                // ours
            411 => "Length Required",                   // measured
            412 => "Precondition Failed",               // measured
            413 => "Request Entity Too Large",          // ours
            500 => "Internal Server Error",             // measured
            _ => "",
        }
    }
}

fn push_header(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.extend_from_slice(name);
    out.extend_from_slice(b": ");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
}

/// The SOAP seam, as one method.
///
/// [`crate::soap::Soap::handle`] already returns a [`crate::soap::Reply`] and is
/// already `Send + Sync` (every field is an `Arc`, a `SharedStore`, a
/// `SharedRenderer`, or a `SharedBroadcast`), so `impl Control for Soap` below is
/// the whole adapter. The trait exists so a test can answer a canned `Reply`
/// without constructing five collaborators, and so the face never names the
/// SOAP layer's own error types.
pub trait Control: Send + Sync {
    fn handle(&self, rawbody: &[u8]) -> crate::soap::Reply;
}

impl Control for crate::soap::Soap {
    fn handle(&self, raw: &[u8]) -> crate::soap::Reply {
        crate::soap::Soap::handle(self, raw)
    }
}

/// The GENA subscription seam — the three things `DLNAHandler.SUBSCRIBE` and
/// `UNSUBSCRIBE` ask of the subscriber table.
///
/// It is owned here, and implemented by [`crate::eventing`], because the HTTP
/// route and the table's owner must disagree about who decides: this file
/// decides **when** a subscription is renewed, added, or removed and what header
/// text comes back; `eventing` decides whether the SID exists, what the new SID
/// is, and who gets the next event.
///
/// The statuses are Python's, including the asymmetry a real control point
/// depends on: `remove_subscribe` returns **200 unconditionally**
/// (`protocol.py:776-794`) — it queues the removal only if the SID is present,
/// and still answers 200 — while `renew_subscribe` answers **412** for a SID it
/// does not know (`protocol.py:760-774`). A fresh subscription therefore
/// succeeds at `UNSUBSCRIBE` and fails at `RENEW` **in the same run**, which is
/// measured (rows 16-18 vs 19-21) and which this port must not "fix".
pub trait Subscription: Send + Sync {
    /// `renew_subscribe(SID, timeout)` — 200 or a refusal status (412).
    fn renew(&self, sid: &str, timeout: i64) -> u16;

    /// `add_subscribe(service, url, timeout)`.
    ///
    /// Returns the SID **and the exact `Timeout` header text** Python writes,
    /// because the two branches of `SUBSCRIBE` write different types into that
    /// slot: the renew branch writes the parsed `int` (`protocol.py:3177`,
    /// `headers['TIMEOUT'] = TIMEOUT`), the new-subscription branch writes
    /// `res['TIMEOUT']`, a string like `Second-1800` (`:3184`). Returning a
    /// `String` here is what lets each branch keep its own spelling.
    fn add(&self, service: &str, url: &str, timeout: i64) -> (String, String);

    /// `remove_subscribe(SID)` — 200 in Python, whatever the table thinks.
    fn remove(&self, sid: &str) -> u16;
}

/// The management seam: the two things `GET /api?query=…` can ask of the
/// player, and the only way this port reaches anything that *does* something.
///
/// It returns **sentences**, not a `serde_json::Value`, because the JSON shape
/// is this file's business (the two-key `{code, message}` document, the 4-space
/// indent, the `ensure_ascii` quoting) while the *content* of the sentence
/// belongs to whoever drives the player. An `Err` is a refusal the caller
/// should read, so it is a message too — never a status code.
pub trait Management: Send + Sync {
    /// Cast `url`, named `title` when the sender gave one (`cast-title`).
    /// `Ok` is the sentence for `code: 0`, `Err` the sentence for `code: 1`.
    fn cast_url(&self, url: &str, title: Option<&str>) -> Result<String, String>;
}

/// One name in [`GET_MANAGEMENT_ROUTES`]'s `gate` column, read at check time.
enum Gate {
    /// Loopback, or an HTTPS connection, or a request carrying the token.
    Management,
    /// The management check **and** the token.
    Token,
}

impl Gate {
    /// `None` for a name this port does not know — and `None` means refuse, not
    /// "let through" (AGENTS.md §4.7c). A wrong string in our own table costs a
    /// working endpoint, which is recoverable and loud; a right-by-accident
    /// string on a fail-open reading is the shape of the bug that section was
    /// written to prevent.
    fn parse(name: &str) -> Option<Gate> {
        match name {
            "management" => Some(Gate::Management),
            "token" => Some(Gate::Token),
            _ => None,
        }
    }
}

/// How the SCPD route asks "when was this file last changed".
///
/// Injectable because the answer is a property of the **machine**, not of the
/// program: the shipped receiver lets cherrypy's `staticdir` take the mtime from
/// `macast/xml/<Service>.xml` on disk, and in a bundled `.app` those files are
/// real files, whereas an embedded build may have no such path at all. So this
/// face asks, and **omits the header when the question has no answer** — the
/// same embedded-versus-disk rule as [`crate::scpd`]'s descriptors
/// (`scpd.rs:93-115`). A wrong date is worse than no date: it is what a control
/// point caches.
pub type MtimeProbe = Arc<dyn Fn(&str) -> Option<SystemTime> + Send + Sync>;

/// The real probe: the same relative path the shipped staticdir was mounted on.
///
/// `XMLPath.BASE_PATH` is the `macast/` package directory and `server.py:150`
/// mounts `/dlna` with `dir: "xml"`, so the file is `xml/<Service>.xml` under
/// the resource root. `paths::base_path` is that root's counterpart.
fn disk_mtime(relative: &str) -> Option<SystemTime> {
    std::fs::metadata(paths::base_path(relative))
        .ok()?
        .modified()
        .ok()
}

/// `xml/<Service>.xml` for the three descriptors this port serves, `None` for
/// anything else.
///
/// Keyed on the **file-name segment**, so the route strips `.xml` to get the
/// service name and the two tables cannot drift apart. `Dlna/Description.xml`
/// answers `None` — and therefore 303 — which is a deviation: the shipped
/// staticdir *does* have that file and serves it, but its contents are the
/// unfilled template with `{}` placeholders in it, i.e. a document that is not
/// the device's description. A test asserts this table and
/// [`crate::scpd::doc_for_service`] agree for every name.
pub fn scpd_file_rel(name: &str) -> Option<&'static str> {
    match name {
        "AVTransport.xml" => Some("xml/AVTransport.xml"),
        "RenderingControl.xml" => Some("xml/RenderingControl.xml"),
        "ConnectionManager.xml" => Some("xml/ConnectionManager.xml"),
        _ => None,
    }
}

/// The HTTP face: routing, headers, and the refusals.
///
/// Cheap to hand to threads and thread-safe by construction — everything
/// mutable is behind a seam it was given, so this is the shape cherrypy's
/// `MethodDispatcher` has here (one handler object, many worker threads), and
/// the reason the subscriber table next door is copy-on-write rather than
/// locked (AGENTS.md §4.2).
pub struct Face {
    log: Arc<Logger>,
    control: Arc<dyn Control>,
    subscription: Arc<dyn Subscription>,
    /// Drives the player for `GET /api?query=cast`. `Arc` because every worker
    /// thread reads it, and because the shell behind it outlives the face's
    /// handler calls.
    management: Arc<dyn Management>,
    setting: Arc<Setting>,
    /// Built **once**, at startup. See [`Face::new`] for what a failure here
    /// means.
    description: Vec<u8>,
    mtime: MtimeProbe,
}

impl Face {
    /// Build the face, including the device description.
    ///
    /// A `Err` here means the face must **not** start: `description::build_for`
    /// refuses rather than handing back placeholder bytes (`description.rs:348-352`
    /// — "A caller that gets `Err` must not answer with the previous bytes
    /// silently"), and a device that describes itself wrong is worse than one
    /// that is absent: the control point caches the wrong `UDN` and the user
    /// spends the afternoon casting to a name that no longer exists.
    pub fn new(
        control: Arc<dyn Control>,
        subscription: Arc<dyn Subscription>,
        management: Arc<dyn Management>,
        setting: Arc<Setting>,
    ) -> Result<Face, String> {
        let description = crate::description::build_for(&setting)?;
        Ok(Face {
            // `Protocol`, the same logger name `soap.rs` uses: `protocol.py:30`
            // builds one logger for the handler *and* the protocol, and the
            // per-module log file (`logsplit`) routes on that name.
            log: Logger::named("Protocol"),
            control,
            subscription,
            management,
            setting,
            description,
            mtime: Arc::new(disk_mtime),
        })
    }

    /// Replace the mtime probe. Tests use this to make `Last-Modified` a fact
    /// they chose instead of a fact about the machine.
    pub fn with_mtime_probe(mut self, probe: MtimeProbe) -> Face {
        self.mtime = probe;
        self
    }

    /// The port this face is configured to serve. Insert-on-read, mirroring
    /// `Setting.get`'s side effect (`setting.rs:151-157`), so a first run writes
    /// the default the same way the Python one does.
    pub fn port(&self) -> u16 {
        setting::application_port(&self.setting)
    }

    /// The bytes this face answers for `GET /description.xml`.
    pub fn description_bytes(&self) -> &[u8] {
        &self.description
    }

    /// One request in, one response out. Pure: no sockets, no threads.
    pub fn answer(&self, req: &Request) -> Response {
        if let Some(status) = pre_route_status(&req.verb, &req.headers) {
            return self.refusal(status);
        }
        let mut response = match req.verb {
            Verb::Get | Verb::Head => self.get(req),
            Verb::Post => self.post(req),
            Verb::Subscribe => self.subscribe(req),
            Verb::Unsubscribe => self.unsubscribe(req),
            // 405, and **no `Allow:`** — see the module doc: the shipped value is
            // a leaked namespace, and there is no correct list to write here
            // that the measurement supports.
            Verb::Other => self.refusal(405),
        };
        // One place, so a route cannot forget it. `Content-Length` still carries
        // the body's length; only the body is withheld (row 11).
        if req.verb == Verb::Head {
            response.omit_body = true;
        }
        response
    }

    /// `GET`/`HEAD`. The three routes the DLNA face needs, then a redirect.
    fn get(&self, req: &Request) -> Response {
        match req.segments.first().map(|s| s.as_str()) {
            // `DLNAHandler.GET` binds only `param` and compares it to
            // `'description.xml'`, so extras are swallowed: `/description.xml/x`
            // is the same 200. Case-sensitive, like the Python comparison.
            Some("description.xml") => {
                Response::new(200, crate::soap::SOAP_CONTENT_TYPE, self.description.clone())
            }
            Some("dlna") => self.scpd(req),
            // The management face. `DLNAHandler.GET` binds `key = ''` as its
            // first route parameter, so `/api` and `/api/` are the same call and
            // `/api/anything-else` is a *different* one: it lands in the
            // `elif key` chain, is not a known query route, and redirects — which
            // is what the `_ =>` arm below already does.
            Some("api") => self.api(req),
            None => Response::new(200, HTML_CONTENT_TYPE, ROOT_PAGE.as_bytes().to_vec()),
            _ => self.redirect(req),
        }
    }

    /// `GET /api?query=…` — the management face's two answers.
    ///
    /// The shape is the shipped one: the query name is looked up in
    /// [`GET_MANAGEMENT_ROUTES`], an unknown name is **read back** rather than
    /// 404'd, and a known name pays its gate *before* anything reaches the
    /// player. Two ordering calls are **ours**, because this port has no POST
    /// management face to copy:
    ///
    /// * No `query` (absent, or there and empty) is a 200/`code: 1` that names
    ///   the missing parameter. In Python `GET()` binds `key=''`, the whole
    ///   `elif` chain falls through to the base handler, and that handler's
    ///   answer is the same "没有可执行的操作" document [`Face::fall_through`]
    ///   writes — so this is the shipped answer plus the name of what was absent.
    /// * An unknown `query` is answered **before** the gate. Hiding which names
    ///   we know is worth less to a caller than fixing their typo, and the value
    ///   comes back inside a JSON string, not inside markup
    ///   ([`unknown_query`] says why that is safe).
    fn api(&self, req: &Request) -> Response {
        // An extra path segment is **a different call**. The shipped `GET` binds
        // its first route parameter as `key`, so `/api/extra` arrives as
        // `key='extra'`, is not a name the chain knows, and ends in the redirect
        // — while this port takes the route name from `query=` and would
        // otherwise answer it as if the extras were never typed.
        if req.segments.len() > 1 {
            return self.redirect(req);
        }
        let query = query_value(req, "query").unwrap_or_default();
        if query.is_empty() {
            return json_answer(200, 1, "没有可执行的操作：缺少 query 参数。");
        }
        let Some(route) = GET_MANAGEMENT_ROUTES
            .iter()
            .find(|route| route.query == query)
        else {
            return unknown_query(&query);
        };
        if let Some(refusal) = self.gate_refusal(route.gate, req) {
            return refusal;
        }
        match route.action {
            Action::Cast => self.cast_entry(req),
            Action::CastInfo => self.cast_info(),
        }
    }

    /// The gate that [`GET_MANAGEMENT_ROUTES`] names, resolved and paid.
    ///
    /// `None` means "answer it". The two checks are **cumulative and ordered**:
    /// the token gate first pays the management check, so a LAN caller who typed
    /// a token hears the *management* sentence rather than the token one. That is
    /// what the shipped `_gate_refusal` does (AGENTS.md §4.7c) and what a reader
    /// needs — "this answers 本机 / HTTPS / 带令牌" is the right correction for
    /// "I did put a token in and it still refused".
    ///
    /// The order therefore chooses the **message**, never the verdict: swapping
    /// the two branches refuses exactly the same set of requests.
    fn gate_refusal(&self, gate: &str, req: &Request) -> Option<Response> {
        let Some(parsed) = Gate::parse(gate) else {
            // Fail closed. The caller gets the generic management sentence — a
            // typo in our own table is not information to hand out — while the
            // log gets the name, because the only person who can fix it is the
            // one reading this file.
            self.log.error(&format!(
                "GET route gate {gate:?} is not a gate this port knows; refusing the request"
            ));
            return Some(json_answer(403, 1, MANAGEMENT_REFUSAL));
        };
        let token_ok = self.token_ok(req);
        if !is_loopback(&req.remote_ip) && !token_ok {
            return Some(json_answer(403, 1, MANAGEMENT_REFUSAL));
        }
        if matches!(parsed, Gate::Token) && !token_ok {
            return Some(json_answer(403, 1, TOKEN_REFUSAL));
        }
        None
    }

    /// The token compared against the stored secret, in constant time.
    ///
    /// Reads with [`Setting::peek`], **never** [`macast_core::token::api_token`]:
    /// that one generates *and saves* when there is no token, and a gate is asked
    /// on every refused request. Two consequences, both wanted:
    ///
    /// * A machine with no token yet lets nobody through this gate. The secret is
    ///   created by the single route that has to reveal it — `query=cast-info`,
    ///   which the local settings page calls — and that is the shipped
    ///   「首次使用时生成」, in the only place where generating it is also useful.
    /// * A stored `Api_Token` that is not a non-empty string counts as absent: an
    ///   empty secret must not authenticate anyone. Same reading as the generator.
    fn token_ok(&self, req: &Request) -> bool {
        let given = query_value(req, "token").unwrap_or_default();
        if given.is_empty() {
            return false;
        }
        let stored = self
            .setting
            .peek(Key::ApiToken)
            .and_then(|value| value.as_str().map(String::from));
        match stored {
            Some(secret) => token::secret_eq(&given, &secret),
            None => false,
        }
    }

    /// `query=cast&url=<绝对地址>`. A shortcut, a bookmark, or `curl` — anything
    /// that can open a URL but cannot speak DLNA (AGENTS.md §4.7).
    ///
    /// The URL is checked for **shape**, not scheme: a non-empty scheme and a
    /// non-empty authority ([`is_absolute_url`]). A whitelist of `http`/`https`
    /// would be this port inventing a rule the reference does not have, and would
    /// refuse the rtsp and ftp addresses a renderer legitimately accepts. The
    /// refused shapes answer **without** touching the shell — a bad URL must not
    /// leave a half-change behind in the player's state.
    ///
    /// `cast-title` is passed through when present and non-empty; the shell owns
    /// what it does with a name (AGENTS.md §4.2: change the address, change the
    /// name), so an empty one here means "no opinion", not "call it nothing".
    fn cast_entry(&self, req: &Request) -> Response {
        let url = query_value(req, "url").unwrap_or_default();
        if url.trim().is_empty() {
            return json_answer(200, 1, "缺少参数 url：投屏入口需要一个完整的媒体地址。");
        }
        if !is_absolute_url(&url) {
            return json_answer(
                200,
                1,
                &format!(
                    "{} 不是一个完整的媒体地址：需要带协议和主机的绝对地址，例如 http://主机/路径。",
                    truncate_chars(url.trim(), QUERY_ECHO_MAX)
                ),
            );
        }
        let title = query_value(req, "cast-title")
            .filter(|value| !value.is_empty());
        match self.management.cast_url(&url, title.as_deref()) {
            Ok(message) => json_answer(200, 0, &message),
            Err(why) => json_answer(200, 1, &why),
        }
    }

    /// `query=cast-info` — the entry URL, **with the token in it**.
    ///
    /// The only route allowed to call [`macast_core::token::api_token`], and the
    /// only place the secret is ever revealed; that is why its gate is the
    /// management one and why a LAN caller must not get here (AGENTS.md §4.7).
    ///
    /// **Deviation, ours**: the shipped answer is a dict of fields
    /// (`{'code': 0, 'data': {...}}`-shaped), and this port's JSON writer emits
    /// the measured two-key `{code, message}` document, so the entry template is
    /// the **message**. Nothing on our side parses this body — the settings page
    /// is a Tauri window that asks the shell directly — and a third-party sender
    /// that did parse it would be reading a field name this branch never
    /// measured. Inventing a `data` key to look compatible would be the worse
    /// error: an unmeasured shape that reads like a witness.
    fn cast_info(&self) -> Response {
        let token = token::api_token(&self.setting);
        json_answer(
            200,
            0,
            &format!(
                "http://127.0.0.1:{}/api?query=cast&url=<媒体地址>&token={}",
                self.port(),
                token
            ),
        )
    }

    /// `/dlna/<Service>.xml` — the SCPD documents.
    ///
    /// `Last-Modified` **then** `Accept-Ranges: bytes`, which is the measured
    /// order (rows 2, 3, 4). `Last-Modified` is emitted only when the probe
    /// answers; `Accept-Ranges` always, because that header is cherrypy's
    /// staticdir promising that a `Range:` request will be honoured, and our
    /// SCPD bodies are fixed bytes that answer one.
    fn scpd(&self, req: &Request) -> Response {
        let name = match req.segments.get(1) {
            Some(name) => name.as_str(),
            None => return self.redirect(req),
        };
        let Some(relative) = scpd_file_rel(name) else {
            // Not 404: `/dlna/Nope.xml` reaches the base handler's GET, which
            // redirects (measured rows 5-10 are all 303 and none of them is a
            // description request).
            return self.redirect(req);
        };
        // `strip_suffix`, not `trim_end_matches`: `AVTransport.xml.xml` must
        // answer 303 the way the shipped staticdir does, because there is no
        // such file, whereas `trim_end_matches` would strip both dots and serve
        // the real descriptor for a name that was never asked for.
        let service = name.strip_suffix(".xml").unwrap_or(name);
        let Some(document) = crate::scpd::doc_for_service(service) else {
            return self.redirect(req);
        };
        let mut variant = Vec::new();
        if let Some(when) = (self.mtime)(relative) {
            variant.push((
                "Last-Modified".to_string(),
                timefmt::rfc1123_gmt(epoch_secs(when)),
            ));
        }
        variant.push(("Accept-Ranges".to_string(), "bytes".to_string()));
        Response {
            status: 200,
            content_type: SCPD_CONTENT_TYPE,
            variant,
            body: document.as_bytes().to_vec(),
            omit_body: false,
        }
    }

    /// The 303 answer, byte for byte.
    ///
    /// The body is `This resource can be found at <a href="{loc}">{loc}</a>.`
    /// with **no trailing newline**. Its length is `2*len(loc) + 46`; for the
    /// measured `http://127.0.0.1:58993/` that is 92, which is what all six
    /// witness rows carry. (An earlier hand-count of that template said 91. It
    /// was wrong, and the lesson is in the test: never hand-count a wire length.)
    fn redirect(&self, req: &Request) -> Response {
        let location = self.redirect_location(req.header("Host"));
        let body = format!(
            "This resource can be found at <a href=\"{location}\">{location}</a>."
        );
        Response {
            status: 303,
            content_type: HTML_CONTENT_TYPE,
            variant: vec![("Location".to_string(), location)],
            body: body.into_bytes(),
            omit_body: false,
        }
    }

    /// Where a redirect points: the `Host` header's authority, else this
    /// machine's loopback on the configured port.
    ///
    /// `Location:` is **absolute** because cherrypy's `HTTPRedirect` builds it
    /// from `request.base` — a relative `Location` would be honoured by browsers
    /// and ignored by the UPnP stacks this receiver is aimed at. An empty `Host`
    /// counts as absent. Always `http://`: the HTTPS channel is a separate
    /// listener (`server.py`'s `_bind_https`), and a redirect from one to the
    /// other would be the receiver inventing a certificate the sender never
    /// asked for.
    pub fn redirect_location(&self, host: Option<&str>) -> String {
        match host.map(str::trim).filter(|value| !value.is_empty()) {
            Some(authority) => format!("http://{authority}/"),
            None => format!("http://127.0.0.1:{}/", self.port()),
        }
    }

    /// `POST`. Either an action, or the base handler's fall-through.
    fn post(&self, req: &Request) -> Response {
        // Both debug lines are the shipped ones, in the shipped order:
        // `protocol.py:3156` logs `RAW: {body}` **before** the route test, and
        // `:3161` logs `RES: {reply}` **after** `Ext` is set. Two lines, one
        // route. Deviation: Python formats a `bytes` object with `repr`, so its
        // line starts `RAW: b'`; ours is lossy UTF-8. The content is the same
        // evidence and no log parser here depends on the `b'`.
        self.log
            .debug(&format!("RAW: {}", String::from_utf8_lossy(&req.body)));
        if req.segments.len() >= 2 && req.segments[1] == "action" {
            let reply = self.control.handle(&req.body);
            // `Ext:` belongs to the route, not to the SOAP answer — see
            // `crate::soap::Reply`'s doc. Measured: rows 12 (200) and 13 (500)
            // both carry `Ext: ''` and both are action responses; row 14, the
            // 500 for an **undeclared action**, carries no `Ext` at all, because
            // in Python that request never reaches the line that sets it. So the
            // rule is "a SOAP-shaped answer", which is exactly `content_type`.
            let mut variant = Vec::new();
            if reply.content_type == crate::soap::SOAP_CONTENT_TYPE {
                variant.push(("Ext".to_string(), String::new()));
            }
            self.log
                .debug(&format!("RES: {}", String::from_utf8_lossy(&reply.body)));
            return Response {
                status: reply.status,
                content_type: reply.content_type,
                variant,
                body: reply.body,
                omit_body: false,
            };
        }
        self.fall_through(req)
    }

    /// A `POST` that is not an action route.
    ///
    /// In Python this is `return super(DLNAHandler, self).POST(...)`
    /// (`protocol.py:3164`) — the base `Handler.POST`, whose body is the
    /// **POST** management API. That half of the management face is **not**
    /// ported, and the reason is structural rather than a backlog item: this
    /// face's only POST routes are `POST /<service>/action`, which the SOAP
    /// layer claims, so there is no field-driven management endpoint for the
    /// shipped table's §4.7c gates to be attached to. Two consequences worth
    /// naming instead of leaving silent — `_same_site()` and
    /// `_CODE_EXECUTION_PARAMS` (§4.7b) have **no target** here, and the
    /// fall-through below is therefore reached by *every* POST this port does
    /// not recognise, including a well-formed form body.
    ///
    /// The **GET** half, by contrast, *is* ported — see [`Face::api`]: the
    /// web-cast entry and `cast-info`, with the same gate-is-data shape
    /// (§4.7c) and the same fail-closed refusal. That asymmetry is the current
    /// state of this port, and it is why the paragraph above says "no POST
    /// management routes exist" rather than "the management API is missing".
    ///
    /// What this method answers is what the base handler answers for any request
    /// it does not recognise: **HTTP 200**, not 501 — `protocol.py:2543-2551`
    /// builds `{'code': 1, 'message': '没有可执行的操作…'}` and returns it, and
    /// cherrypy turns a returned dict into a 200 with the JSON tool. A 501 here
    /// would be a *better* HTTP answer and a **worse** port: the point of the
    /// shipped shape is "the request arrived and nothing matched", and a code
    /// that also means "I refuse to implement this" would read differently to a
    /// sender that has already learned the 200/`code: 1` pair.
    ///
    /// [`Face::api`]: Face::api
    fn fall_through(&self, req: &Request) -> Response {
        let mut all = BTreeSet::new();
        for (name, _) in &req.query {
            all.insert(name.clone());
        }
        for name in form_field_names(req) {
            all.insert(name);
        }
        // Python logs `sorted(kwargs)` — every name it was given, helpers
        // included. Deviation: Python prints `['a']`, Rust prints `["a"]`.
        self.log.error(&format!(
            "POST /api with no recognised action: {:?}",
            all.iter().collect::<Vec<_>>()
        ));
        let unknown: Vec<&str> = all
            .iter()
            .map(String::as_str)
            .filter(|name| !POST_HELPER_PARAMS.contains(name) && !FILE_ROUTE_PARAMS.contains(name))
            .collect();
        let mut message = String::from("没有可执行的操作");
        if !unknown.is_empty() {
            message.push('：');
            message.push_str("不认识字段 ");
            message.push_str(&unknown.join("、"));
        }
        Response::new(
            200,
            JSON_CONTENT_TYPE,
            py_json_dumps_indented(1, &message).into_bytes(),
        )
    }

    /// An HTML refusal. `status` decides the reason phrase and the one line.
    pub fn refusal(&self, status: u16) -> Response {
        let body = format!(
            "<html><body><h2>{status} {}</h2></body></html>",
            Response::reason(status)
        );
        Response::new(status, HTML_CONTENT_TYPE, body.into_bytes())
    }

    /// `SUBSCRIBE` (`protocol.py:3168-3188`).
    ///
    /// The reading order is the Python source's, and one of its steps is
    /// load-bearing in a way that looks like a bug: **`TIMEOUT` is parsed
    /// before the SID/CALLBACK branch** (`:3171-3172`), so a request with an
    /// unreadable `TIMEOUT` fails even when it would have failed on the missing
    /// SID anyway. Keeping that order is what makes the refusal the same one a
    /// control point sees today.
    ///
    /// Deviation: an unreadable `TIMEOUT` is **412** here. In Python
    /// `int('Second-abc')` raises `ValueError` inside the handler and the sender
    /// gets cherrypy's HTML 500. A 412 is the answer the handler clearly means to
    /// give for "your request headers do not work", and unlike a 500 it says
    /// nothing about our internals. Same reasoning, same file: an empty
    /// `CALLBACK` we cannot read a URL out of is 412, where Python's
    /// `re.findall(...)[0]` raises `IndexError` → 500.
    fn subscribe(&self, req: &Request) -> Response {
        let service = req
            .segments
            .first()
            .map(String::as_str)
            .unwrap_or("")
            .to_string();
        let param = req
            .segments
            .get(1)
            .map(String::as_str)
            .unwrap_or("")
            .to_string();
        // `return b''` sits **outside** the `if param == 'event'` block in
        // Python, so `SUBSCRIBE /AVTransport/not-event` answers 200 with an
        // empty body and **no Sid/Timeout** — this is the shape, and it is not
        // an oversight.
        if param != "event" {
            return Response::new(200, crate::soap::SOAP_CONTENT_TYPE, Vec::new());
        }
        // Truthy in Python means "not None"; here it also means "not empty",
        // because cherrypy answers an empty header with `''`, and an empty `SID`
        // cannot be renewed. Deviation, in the safe direction: Python would try
        // to renew `''` and get its 412 anyway. A `SID: ` header whose value is
        // a single space is truthy in Python and falsy here — no control point
        // does that, and 412 is the honest answer if one starts to.
        let sid = truthy(req.header("SID"));
        let callback = truthy(req.header("CALLBACK"));
        let raw_timeout = req
            .header("TIMEOUT")
            .unwrap_or(DEFAULT_SUBSCRIBE_TIMEOUT)
            .to_string();
        let Some(timeout) = parse_second_timeout(&raw_timeout) else {
            self.log.error(&format!(
                "SUBSCRIBE: cannot read TIMEOUT {raw_timeout:?}, expecting Second-<seconds>"
            ));
            return self.refusal(412);
        };
        if let Some(sid) = sid {
            // Three exclamation marks in the shipped line
            // (`"RENEW SUBSCRIBE:!!!!!!!" + service`). Dropped here as our
            // choice — there is no test anywhere pinning them
            // (`scripts/verify_cast_airplay.py` matches neither `RENEW SUBSCRIBE`
            // nor `SUBSCRIBE UPDATE`), and the facts the line has to carry are
            // which kind and which service.
            self.log.error(&format!("RENEW SUBSCRIBE: {service}"));
            let status = self.subscription.renew(&sid, timeout);
            if status != 200 {
                self.log.error("RENEW SUBSCRIBE: cannot find such sid.");
                return self.refusal(status);
            }
            return Response {
                status: 200,
                content_type: crate::soap::SOAP_CONTENT_TYPE,
                // The **int**, because `headers['TIMEOUT'] = TIMEOUT` here is
                // the already-parsed number (`:3177`). Source-derived, not
                // measured: no witness row ever reached a renew that succeeded,
                // so the three 412/1737 rows are all that exists on the wire.
                variant: vec![
                    ("Sid".to_string(), sid.to_string()),
                    ("Timeout".to_string(), timeout.to_string()),
                ],
                body: Vec::new(),
                omit_body: false,
            };
        }
        if let Some(callback) = callback {
            // The log line comes **first**, as in Python (`:3180`), i.e. before
            // the URL is read out of the callback. When the callback is
            // unreadable the log therefore shows an attempt the answer refuses —
            // which is exactly what the shipped receiver's log does, and is what
            // makes the 412 explainable after the fact.
            self.log.error(&format!("ADD SUBSCRIBE: {service}"));
            let Some(url) = first_angle_bracket(callback) else {
                self.log.error(&format!(
                    "SUBSCRIBE: cannot find a url between <> in CALLBACK {callback:?}"
                ));
                return self.refusal(412);
            };
            let (new_sid, new_timeout) = self.subscription.add(&service, url, timeout);
            return Response {
                status: 200,
                content_type: crate::soap::SOAP_CONTENT_TYPE,
                // Both verbatim from the subscriber (`res['SID']`,
                // `res['TIMEOUT']`) — `Second-{n}` form and all. Multiple
                // callbacks in one header subscribe only the first, as Python's
                // `[0]` does: a control point that lists two is asking for
                // redundancy this receiver does not have.
                variant: vec![("Sid".to_string(), new_sid), ("Timeout".to_string(), new_timeout)],
                body: Vec::new(),
                omit_body: false,
            };
        }
        self.log.error("SUBSCRIBE: cannot find sid and callback.");
        self.refusal(412)
    }

    /// `UNSUBSCRIBE` (`protocol.py:3190-3202`).
    ///
    /// The asymmetry with [`Face::subscribe`] is Python's and is reproduced:
    /// `UNSUBSCRIBE(self, service, param)` declares **no defaults**, and the
    /// handler returns `b''` only inside the `if`, so **everything** it does not
    /// return from inside — including a request whose second path segment is not
    /// `event` — falls through to `logger.error("UNSUBSCRIBE: error 412.")` and
    /// a 412. `SUBSCRIBE` has defaults and returns `b''` outside its `if`, so the
    /// same "wrong last segment" answers 200 there. Two verbs, two rules, one
    /// file.
    ///
    /// Deviation: a **one-segment** path (`UNSUBSCRIBE /event`) is 412 here,
    /// where Python's missing argument is a `TypeError` → HTML 500.
    fn unsubscribe(&self, req: &Request) -> Response {
        let service = req
            .segments
            .first()
            .map(String::as_str)
            .unwrap_or("")
            .to_string();
        let param = req
            .segments
            .get(1)
            .map(String::as_str)
            .unwrap_or("")
            .to_string();
        if param == "event" {
            if let Some(sid) = truthy(req.header("SID")) {
                self.log.error(&format!("REMOVE SUBSCRIBE: {service}"));
                let status = self.subscription.remove(sid);
                if status != 200 {
                    return self.refusal(status);
                }
                // Row 22 is the measurement: 200, body_len 0, and **no Sid and
                // no Timeout**. An unsubscribe that re-echoes a SID would tell a
                // control point it still has one.
                return Response::new(200, crate::soap::SOAP_CONTENT_TYPE, Vec::new());
            }
        }
        self.log.error("UNSUBSCRIBE: error 412.");
        self.refusal(412)
    }
}

/// `Some(value)` when the header carried something we can act on.
fn truthy(value: Option<&str>) -> Option<&str> {
    value.filter(|inner| !inner.is_empty())
}

/// `int(TIMEOUT.split('-')[-1])`, and `None` where Python would raise.
///
/// `split('-')[-1]` is the last field after the **final** hyphen, which is
/// `rsplit_once` — and when there is no hyphen at all it is the whole string,
/// which is the `unwrap_or`. Deliberately **not** a `Second-` prefix check:
/// `1800` alone parses, and the shipped receiver accepts it, so a sender that
/// says `TIMEOUT: 600` gets 600 seconds today and must continue to.
fn parse_second_timeout(raw: &str) -> Option<i64> {
    let tail = raw
        .rsplit_once('-')
        .map(|(_, seconds)| seconds)
        .unwrap_or(raw);
    tail.trim().parse::<i64>().ok()
}

/// `re.findall("<(.*?)>", callback)[0]`, and `None` where Python raises.
///
/// Non-greedy, so `<http://a><http://b>` yields the **first**, and `<>` yields
/// the empty string rather than refusing — that is what a regex with a lazy
/// quantifier does, and an empty URL is then the subscriber's problem, exactly
/// as it is in Python (`host` becomes `''` and the first NOTIFY fails).
fn first_angle_bracket(value: &str) -> Option<&str> {
    let start = value.find('<')?;
    let rest = &value[start + 1..];
    let end = rest.find('>')?;
    Some(&rest[..end])
}

/// `SystemTime` to whole seconds, in the direction `rfc1123_gmt` expects.
///
/// Both directions of `duration_since` are handled because a file can carry a
/// pre-epoch mtime, and `rfc1123_gmt` takes an `i64`.
fn epoch_secs(when: SystemTime) -> i64 {
    match when.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

/// The names an `application/x-www-form-urlencoded` body declares.
///
/// Read **only** when the `Content-Type` says form-urlencoded, because that is
/// the only body cherrypy merges into `kwargs`, and a SOAP envelope must not be
/// scanned for field names. Values are ignored; only the names reach the
/// "unrecognised fields" answer. Not percent-decoded — see [`parse_query`].
fn form_field_names(req: &Request) -> Vec<String> {
    let content_type = req.header("Content-Type").unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .contains("x-www-form-urlencoded")
    {
        return Vec::new();
    }
    // Names **by value**: the body lives in `req`, the text we parse is a lossy
    // copy of it, and a `&str` would have to borrow from a `String` this
    // function drops. The copy is the cheaper defect to own.
    let text = String::from_utf8_lossy(&req.body);
    parse_query(&text)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !name.is_empty())
        .collect()
}

/// The first value a query parameter carries, or `None` when the name is absent.
///
/// **First** pair wins, and that is **our** choice, not a measurement. cherrypy
/// merges the query string into `request.params` and a repeated key arrives
/// there as a **list**; the shipped handler then compares that value against a
/// string with `==`, which in Python never matches a list — so
/// `?query=cast&query=cast-info` is a route-not-found there, for a reason no
/// outside test can see. Neither reading has a witness, so the tie-break goes to
/// the one that keeps a prefix-shaped value usable.
fn query_value(req: &Request, name: &str) -> Option<String> {
    req.query
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// A JSON answer in the one shape [`py_json_dumps_indented`] measures.
///
/// `code` is the shipped protocol's own field (`0` = it happened, `1` = nothing
/// happened), and it is **not** the HTTP status: a management refusal is
/// **403** *and* `code: 1`, while a "your request made no sense" answer is
/// **200** *and* `code: 1`, because `200` is what the base handler returns when
/// it falls through. Keeping the two arguments separate is what lets a caller
/// say "refuse" without silently changing the status, or vice versa.
fn json_answer(status: u16, code: i64, message: &str) -> Response {
    Response::new(
        status,
        JSON_CONTENT_TYPE,
        py_json_dumps_indented(code, message).into_bytes(),
    )
}

/// The accepted peer's address, **without** its port, and `''` when the OS
/// cannot say.
///
/// The port is dropped because [`is_loopback`] parses the result, and
/// `127.0.0.1:52344` is not an `IpAddr`. `''` on failure is the matching
/// choice: an unknown peer is **not** a loopback peer, so the management gate
/// stays closed on a socket whose address could not be read.
fn peer_ip(stream: &TcpStream) -> String {
    stream
        .peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_default()
}

/// Is this peer the machine this listener is on?
///
/// An empty or unparseable address answers **false**. Fail closed: the one thing
/// this predicate is trusted with is keeping a LAN caller out of `cast-info`,
/// and "I could not tell who this was" must never be the same answer as "this
/// is us". `Request::remote_ip` is the socket peer with no port, so `''` really
/// does mean "the peer address was unavailable".
///
/// There is deliberately **no** `X-Forwarded-For` reading. A header a browser
/// cannot set is not a defence against a browser, and trusting one would reopen
/// the hole this gate exists for (§4.7: any webpage can issue a GET to
/// `127.0.0.1`).
///
/// **One arm of the shipped gate is absent, and that is a fact about this port,
/// not an oversight**: the reference also passes when
/// `request.scheme == 'https'`. This listener terminates no TLS — every request
/// that reaches it is plain HTTP — and [`Request`] carries no `scheme` field for
/// the arm to read. Implementing it would be dead code that reads like
/// coverage, so it is recorded here instead.
fn is_loopback(remote_ip: &str) -> bool {
    remote_ip
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Does this string carry enough structure to be handed to the renderer?
///
/// Two questions, both **shape** rather than policy: a **non-empty scheme** in
/// front of `://`, and a **non-empty authority** behind it (which ends at the
/// first `/`, `?` or `#`). So `http://host/path` and `rtsp://1.2.3.4/live` pass,
/// while `://only-a-separator`, `http://` (no host) and
/// `http:///absolute-empty-host` are refused.
///
/// **No scheme whitelist**, and that is our own decision: the entry exists for
/// callers that can only open a URL (shortcuts, bookmarks, `curl`), and the
/// shipped one hands whatever arrives to the player without grading the
/// protocol. Restricting it to `http`/`https` would break the file and stream
/// paths that work today, and a whitelist we invent here is not something any
/// sender was promised.
///
/// What this gate **is** for: a relative or empty value reaching the renderer
/// produces an error the user cannot read (`Failed to open` for a string that
/// was never an address), so refusing the shape at the door is the shorter
/// sentence, and it is the one place a caller's typo can be named back to them.
fn is_absolute_url(url: &str) -> bool {
    let trimmed = url.trim();
    let Some(scheme_end) = trimmed.find("://") else {
        return false;
    };
    if scheme_end == 0 {
        return false;
    }
    let authority = &trimmed[scheme_end + 3..];
    let authority_end = authority
        .find(|c| matches!(c, '/' | '?' | '#'))
        .unwrap_or(authority.len());
    !authority[..authority_end].is_empty()
}

/// `fn truncate_chars` — cut to `max` **characters**, not bytes.
///
/// Byte-slicing a query value would panic on a UTF-8 boundary, and a query value
/// is whatever a caller typed: `?query=投屏投屏投屏…` is a normal request from a
/// Chinese-named shortcut, not a malformed one.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The answer to a `query=` value this face has no route for.
///
/// **200** with `code: 1`, echoing the offending value — the same shape as the
/// POST fall-through's "unrecognised fields" answer, for the same reason
/// (§4.7c): a `code: 0, message: success` on a request that did nothing is a
/// lie the caller has no way to detect, and a mistyped `query=castt` would read
/// as a successful cast.
///
/// The echo is a **reflection**, so its safety is stated rather than assumed:
/// the value is written into a JSON **string** through [`py_json_quote`], whose
/// `ensure_ascii` encoder escapes everything above U+007F and every control
/// character, and the response carries `Content-Type: application/json`. There
/// is no HTML context here for a browser to sniff, and there is no
/// `text/html` anywhere on this path.
fn unknown_query(value: &str) -> Response {
    json_answer(
        200,
        1,
        &format!(
            "没有可执行的操作：query={} 不是这个接口认识的查询。",
            truncate_chars(value, QUERY_ECHO_MAX)
        ),
    )
}

/// `json.dumps(obj, indent=4)` for the one two-key dict the base handler
/// returns. Written out rather than pulled from a serde formatter because
/// **indent and key order are the wire format**: serde_json's pretty printer
/// uses two spaces, and a sorted-keys map would put `code` before `message` by
/// luck rather than by rule.
fn py_json_dumps_indented(code: i64, message: &str) -> String {
    format!(
        "{{\n    \"code\": {code},\n    \"message\": \"{}\"\n}}",
        py_json_quote(message)
    )
}

/// `json.dumps(..., ensure_ascii=True)`'s string encoder.
///
/// `ensure_ascii` is Python's **default**, and it is what makes the measured
/// body 84 bytes: `'没有可执行的操作'` becomes eight `\uXXXX` escapes (48
/// characters) on the wire. So every character above U+007F is escaped here,
/// including astral ones as a surrogate pair — that is Python's shape, and a
/// test asserts the 84.
fn py_json_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        let code = ch as u32;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Python does not escape `/`, and a `\/` would change the bytes.
            '/' => out.push('/'),
            _ if code < 0x20 => out.push_str(&format!("\\u{code:04x}")),
            // Python leaves printable ASCII raw (escapes iff c < 0x20 || c > 0x7e).
            _ if code <= 0x7E => out.push(ch),
            _ if code <= 0xFFFF => out.push_str(&format!("\\u{code:04x}")),
            _ => {
                let shifted = code - 0x10000;
                let high = 0xD800 + (shifted >> 10);
                let low = 0xDC00 + (shifted & 0x3FF);
                out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        }
    }
    out
}

/// The accept loop's thread bookkeeping — the same three shapes as
/// `ssdp.rs`'s, for the same reasons, duplicated rather than shared because a
/// shared lifecycle helper would have to know about both a UDP socket and a TCP
/// listener and would end up being either of them with extra steps.
#[derive(Default)]
struct ThreadSlot {
    /// Bumped by every `start()`. A thread that outlives the `stop()`/`start()`
    /// pair that spawned it must not clear the new run's `alive` flag, and this
    /// counter is the only reason it can tell.
    generation: u64,
    /// Taken out by `stop()` and joined with **no lock held** — joining while
    /// holding the mutex is a deadlock, because the thread being joined takes
    /// the same mutex on its way out.
    handle: Option<std::thread::JoinHandle<()>>,
    /// The running thread's id, so `stop()` called *from* that thread can skip
    /// the self-join that would panic (`cannot join current thread`).
    id: Option<ThreadId>,
}

/// Clears `alive` on the way out of the accept loop, but only if this is still
/// the run we belong to.
struct AliveGuard {
    alive: Arc<AtomicBool>,
    slot: Arc<Mutex<ThreadSlot>>,
    generation: u64,
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        if current_generation(&self.slot) == self.generation {
            self.alive.store(false, Ordering::SeqCst);
        }
    }
}

/// A poison here means the accept loop panicked, which is exactly the state
/// `is_healthy()` exists to report. Reading the value back is the only
/// non-panicking way to ask, and it keeps `stop()`'s join from turning a
/// reported failure into a second one.
fn current_generation(slot: &Arc<Mutex<ThreadSlot>>) -> u64 {
    match slot.lock() {
        Ok(guard) => guard.generation,
        Err(poisoned) => poisoned.into_inner().generation,
    }
}

/// One spawned worker, counted. Created only by the accept loop's closure, which
/// is also the only place that incremented `workers`, so the counter cannot be
/// driven negative by a test calling [`HttpServer::serve_stream`] directly.
struct WorkerGuard {
    workers: Arc<AtomicUsize>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.workers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs the HTTP face.
///
/// ## The two ways this differs from the shipped server, both loud
///
/// * **A taken port is a refusal, not a re-bind.** `server.py:24-40`
///   (`AutoPortServer`) catches `portend.Timeout` and re-binds to a **random**
///   free port, renames the friendly name to `Macast(0123)`, and resets the USN
///   (`Setting` writes the new `ApplicationPort`, and AGENTS.md §4.2's
///   "多实例会把端口写歪" entry is a record of what that costs a user). A Rust
///   port that does the same would silently change the `LOCATION` every control
///   point has cached. So: log the refusal, name the port, and stay down.
/// * **One request per connection.** The answer advertises `Connection: close`,
///   which is what all twenty-five witness rows carry, so the reader closes
///   after answering instead of parsing a second head.
pub struct HttpServer {
    face: Arc<Face>,
    port: Arc<AtomicU16>,
    running: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    workers: Arc<AtomicUsize>,
    slot: Arc<Mutex<ThreadSlot>>,
    listener: Arc<Mutex<Option<TcpListener>>>,
    log: Arc<Logger>,
}

impl HttpServer {
    pub fn new(face: Arc<Face>) -> Arc<HttpServer> {
        let port = face.port();
        Arc::new(HttpServer {
            face,
            port: Arc::new(AtomicU16::new(port)),
            running: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
            workers: Arc::new(AtomicUsize::new(0)),
            slot: Arc::new(Mutex::new(ThreadSlot::default())),
            listener: Arc::new(Mutex::new(None)),
            log: Logger::named("HttpServer"),
        })
    }

    /// Bind and run. Returns without starting anything if a run is already up,
    /// if the port is taken, or if the thread cannot be spawned — each of those
    /// leaves `is_running()` false, which is the honest answer.
    pub fn start(self: &Arc<Self>) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.alive.store(true, Ordering::SeqCst);
        let generation = {
            let mut slot = self.slot();
            slot.generation += 1;
            let generation = slot.generation;
            slot.handle = None;
            slot.id = None;
            generation
        };
        let wanted = self.face.port();
        let listener = match TcpListener::bind(("0.0.0.0", wanted)) {
            Ok(listener) => listener,
            Err(e) => {
                self.log.error(&format!(
                    "HTTP cannot listen on port {wanted}: {e}. \
                     Another instance owns that port; Macast will not move to a \
                     random one, because a moved port silently invalidates every \
                     control point that cached our LOCATION. \
                     Change the port in the settings, or stop the other instance."
                ));
                self.running.store(false, Ordering::SeqCst);
                self.alive.store(false, Ordering::SeqCst);
                return;
            }
        };
        // The bound port, not the asked-for one: a caller that passed port 0
        // (which tests do, and which is the only way to verify without touching
        // the user's 58880) needs the answer back.
        let bound = listener
            .local_addr()
            .map(|address| address.port())
            .unwrap_or(wanted);
        self.port.store(bound as u16, Ordering::SeqCst);
        *self.listener() = Some(listener);
        let me = self.clone();
        match std::thread::Builder::new()
            .name("HTTP_THREAD".to_string())
            .spawn(move || me.run_thread(generation))
        {
            Ok(handle) => self.slot().handle = Some(handle),
            Err(e) => {
                self.log
                    .error(&format!("HTTP cannot start its thread: {e}"));
                // Drop the listener: a `running == false` server that still
                // holds the port would make the next `start()` read as a
                // port conflict rather than as the thread failure it was.
                *self.listener() = None;
                self.running.store(false, Ordering::SeqCst);
                self.alive.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Stop, and join. No `byebye` argument, unlike `ssdp`'s stop: SSDP has a
    /// goodbye announcement to decide about, HTTP has nothing to announce.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let handle = self.slot().handle.take();
        let Some(handle) = handle else {
            return;
        };
        let is_self = {
            let slot = self.slot();
            slot.id == Some(std::thread::current().id())
        };
        if !is_self {
            // No lock held here — see `ThreadSlot::handle`.
            let _ = handle.join();
        }
    }

    /// "Is this thing alive" — and the answer for a server that was **never
    /// started** is `true`, deliberately: the periodic re-announcement in
    /// `ssdp.rs`'s counterpart restarts a listener only when it is unhealthy,
    /// and a deliberately-stopped server is not a fault. The failure this
    /// reports is the one a port check cannot see: `is_running()` is still true
    /// while the thread is gone, so the page would read "服务已启动" and no
    /// request would ever be answered.
    pub fn is_healthy(&self) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return true;
        }
        self.alive.load(Ordering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// The port actually bound, which is [`Face::port`] until `start()` succeeds.
    pub fn bound_port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn face(&self) -> Arc<Face> {
        self.face.clone()
    }

    fn slot(&self) -> MutexGuard<'_, ThreadSlot> {
        match self.slot.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn listener(&self) -> MutexGuard<'_, Option<TcpListener>> {
        match self.listener.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The accept loop. Owns the listener for its lifetime — taking it out of
    /// the mutex is what makes "two threads accepting on one socket" impossible
    /// rather than merely unlikely.
    fn run_thread(self: &Arc<Self>, generation: u64) {
        {
            let mut slot = self.slot();
            if slot.generation == generation {
                slot.id = Some(std::thread::current().id());
            }
        }
        let _guard = AliveGuard {
            alive: self.alive.clone(),
            slot: self.slot.clone(),
            generation,
        };
        let listener = self.listener().take();
        let Some(listener) = listener else {
            self.log.error("HTTP thread found no listener to accept on");
            return;
        };
        if let Err(e) = listener.set_nonblocking(true) {
            self.log
                .error(&format!("HTTP listener cannot be polled: {e}"));
            return;
        }
        loop {
            if !self.running.load(Ordering::SeqCst) {
                break;
            }
            if current_generation(&self.slot) != generation {
                break;
            }
            match listener.accept() {
                Ok((stream, _peer)) => {
                    // Counted before spawning so a burst cannot queue unbounded
                    // threads; the socket is dropped here, which is a refusal the
                    // client sees as an empty close.
                    if self.workers.fetch_add(1, Ordering::SeqCst) + 1 > MAX_WORKERS {
                        self.workers.fetch_sub(1, Ordering::SeqCst);
                        self.log.warning(&format!(
                            "HTTP refused a connection: {MAX_WORKERS} workers already busy"
                        ));
                        continue;
                    }
                    let me = self.clone();
                    let spawned = std::thread::Builder::new()
                        .name("HTTP_WORKER".to_string())
                        .spawn(move || {
                            let _worker = WorkerGuard {
                                workers: me.workers.clone(),
                            };
                            me.serve_stream(stream);
                        });
                    if let Err(e) = spawned {
                        self.workers.fetch_sub(1, Ordering::SeqCst);
                        self.log.error(&format!("HTTP cannot start a worker: {e}"));
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    // A real accept failure: report it, and let the guard clear
                    // `alive` so `is_healthy()` says the truth about the run that
                    // is still marked running.
                    self.log.error(&format!("HTTP accept failed: {e}"));
                    break;
                }
            }
        }
    }

    /// One connection: read the head, refuse early if the head says to, read a
    /// POST body, answer, close.
    ///
    /// Public so a test can bind `127.0.0.1:0` itself and hand us the accepted
    /// stream — the only way to exercise this path on this machine without
    /// touching the port the user's own instance is listening on.
    pub fn serve_stream(&self, mut stream: TcpStream) {
        // The accepted stream can inherit the listener's nonblocking flag
        // depending on the platform, and a timeout on a nonblocking socket does
        // not mean what it looks like. Say what we want: blocking, with deadlines.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));

        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];
        let (head, consumed) = loop {
            match scan_head(&buf) {
                HeadState::Ready { head, consumed } => break (head, consumed),
                HeadState::Bad(status) => {
                    self.write(&stream, &self.face.refusal(status).to_bytes());
                    return;
                }
                HeadState::Incomplete => match stream.read(&mut chunk) {
                    // A connection opened and closed with no request is not an
                    // error worth a log line: health checks and port scanners do
                    // exactly this.
                    Ok(0) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => return,
                },
            }
        };

        let verb = Verb::of(&head.method);
        // The same gate [`Face::answer`] applies, with the same arguments, and
        // called here **before the body is read** — which is the only reason a
        // 413 or 411 saves anything.
        if let Some(status) = pre_route_status(&verb, &head.headers) {
            self.write(&stream, &self.face.refusal(status).to_bytes());
            return;
        }
        // Bodies are read **only** for POST: every other verb this face answers
        // ignores the body, and reading it would be paying for bytes on a route
        // that cannot use them.
        let body = if verb == Verb::Post {
            self.read_body(&mut stream, &buf[consumed..], &head)
        } else {
            Vec::new()
        };
        let mut request = Request::new(head, body);
        // The peer address is what every gate on the management face reads, and
        // it is only knowable **here** — this is the one place the socket is in
        // hand. [`Request::new`] is also what the tests build their requests
        // with, so the field stays a plain assignment rather than a fifth
        // argument to a constructor that has no use for one elsewhere.
        request.remote_ip = peer_ip(&stream);
        let bytes = self.face.answer(&request).to_bytes();
        self.write(&stream, &bytes);
    }

    /// Read the declared body. A short read is **not** an error to die on: the
    /// SOAP layer then fails to parse what arrived and answers its own refusal,
    /// which is the shipped receiver's behaviour for a truncated envelope too.
    fn read_body(&self, stream: &mut TcpStream, already: &[u8], head: &Head) -> Vec<u8> {
        let want = declared_length(&head.headers).unwrap_or(0) as usize;
        let mut body = already.to_vec();
        if body.len() > want {
            // One request per connection, so the excess is either padding we do
            // not serve or a pipelined request we deliberately do not answer.
            body.truncate(want);
            return body;
        }
        // `max(1)` because a zero-length read from a blocking socket says
        // "end of stream", not "nothing asked for": a declared-empty body never
        // enters the loop below, but the buffer must still exist to get here.
        let mut chunk = vec![0u8; READ_CHUNK.min(want.saturating_sub(body.len())).max(1)];
        while body.len() < want {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.log.debug(&format!(
                        "HTTP body stopped after {}/{want} bytes: {e}",
                        body.len()
                    ));
                    break;
                }
            }
        }
        body.truncate(want);
        body
    }

    /// Write the answer and close the write side. A failure here is the
    /// client's, not ours: a control point that goes away mid-response has
    /// already decided not to read, and logging every one of those is how a
    /// busy LAN fills a log with nothing (AGENTS.md §4.2's broadcast lesson, in
    /// the other direction).
    fn write(&self, mut stream: &TcpStream, bytes: &[u8]) {
        let _ = stream.write_all(bytes);
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    const FIXED_SECS: i64 = 1_700_000_000;
    const FALL_THROUGH_BODY: &str =
        "{\n    \"code\": 1,\n    \"message\": \"\\u6ca1\\u6709\\u53ef\\u6267\\u884c\\u7684\\u64cd\\u4f5c\"\n}";
    const DUMPS_OK_BODY: &str = "{\n    \"code\": 0,\n    \"message\": \"ok\"\n}";
    const DUMPS_QUOTED_BODY: &str = "{\n    \"code\": 2,\n    \"message\": \"he said \\\"hi\\\"\"\n}";
    const REFUSAL_405_BODY: &str = "<html><body><h2>405 Method Not Allowed</h2></body></html>";
    const QUOTE_IN: &str = "a\"b\\c\nd\te\rf\u{8}g\u{c}h/i\u{1}j没k\u{1F600}";
    const QUOTE_OUT: &str = "a\\\"b\\\\c\\nd\\te\\rf\\bg\\fh/i\\u0001j\\u6ca1k\\ud83d\\ude00";

    fn fresh_setting(name: &str) -> Arc<Setting> {
        let path = std::env::temp_dir()
            .join(format!("macast-upnp-http-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Arc::new(Setting::new(path))
    }

    /// Every face this module builds gets its own recording management seam, so a
    /// test that does not care about `query=cast` never has to think about it and
    /// a test that does cares about **this** instance's call log.
    fn face_built(
        setting: Arc<Setting>,
        control: Arc<dyn Control>,
        subscription: Arc<dyn Subscription>,
        management: Arc<FakeManagement>,
    ) -> Arc<Face> {
        Face::new(control, subscription, management as Arc<dyn Management>, setting)
            .expect("face builds")
            .into()
    }

    fn face_for(
        name: &str,
        control: Arc<dyn Control>,
        subscription: Arc<dyn Subscription>,
    ) -> Arc<Face> {
        face_built(
            fresh_setting(name),
            control,
            subscription,
            Arc::new(FakeManagement::new()),
        )
    }

    fn harness(name: &str) -> (Arc<Face>, Arc<FakeControl>, Arc<FakeSubscription>) {
        let control = Arc::new(FakeControl::soap_ok());
        let subscription = Arc::new(FakeSubscription::new());
        (
            face_for(name, control.clone(), subscription.clone()),
            control,
            subscription,
        )
    }

    /// A face plus the seam it answers `query=cast` through, so a gate test can
    /// ask the question the shipped face makes expensive: **did the player get
    /// touched?** A refusal that casts anyway is the bug this whole table exists
    /// to prevent, and only the call log can tell the two apart.
    fn harness_management(name: &str) -> (Arc<Face>, Arc<FakeManagement>) {
        let management = Arc::new(FakeManagement::new());
        let face = face_built(
            fresh_setting(name),
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            management.clone(),
        );
        (face, management)
    }

    fn harness_probe(name: &str, probe: MtimeProbe) -> Arc<Face> {
        Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            fresh_setting(name),
        )
        .expect("face builds")
        .with_mtime_probe(probe)
        .into()
    }

    /// `ApplicationPort` is the key `cast-info` puts into the address it hands
    /// out, so a port is not decoration here: it is the thing being asserted.
    fn ported_setting(name: &str, port: u16) -> Arc<Setting> {
        let setting = fresh_setting(name);
        setting.set(setting::Key::ApplicationPort, serde_json::Value::from(i64::from(port)));
        setting
    }

    fn face_on_port(name: &str, port: u16) -> Arc<Face> {
        face_built(
            ported_setting(name, port),
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()),
        )
    }

    fn face_on_port_management(name: &str, port: u16) -> (Arc<Face>, Arc<FakeManagement>) {
        let management = Arc::new(FakeManagement::new());
        let face = face_built(
            ported_setting(name, port),
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            management.clone(),
        );
        (face, management)
    }

    /// A `GET /api?…` from a peer the caller names.
    ///
    /// `Request::remote_ip` is what [`serve_stream`] fills from the accepted
    /// socket, and the gate reads **nothing else** about who is asking: no
    /// `Host`, no `X-Forwarded-For`. So setting this one field is the whole of
    /// "pretend to be the LAN", and a test that leaves it empty is a test of a
    /// peer whose address could not be read — which the gate treats as not-us.
    fn get_api(target: &str, remote: &str) -> Request {
        let mut req = request("GET", target, &[]);
        req.remote_ip = remote.to_string();
        req
    }

    /// The body of a JSON answer, as the shipped writer puts it on the wire.
    ///
    /// Compared against [`py_json_dumps_indented`] rather than against the
    /// sentence itself because `ensure_ascii` escapes every CJK character: a
    /// `contains("已投屏")` on the body would be a test that **cannot** pass, and
    /// a test that cannot pass is worse than no test — it gets deleted, and the
    /// rule it was guarding goes with it.
    fn json_body(code: i64, message: &str) -> Vec<u8> {
        py_json_dumps_indented(code, message).into_bytes()
    }

    fn fixed_mtime() -> MtimeProbe {
        Arc::new(move |_: &str| -> Option<SystemTime> {
            Some(UNIX_EPOCH + Duration::from_secs(FIXED_SECS as u64))
        })
    }

    fn silent_mtime() -> MtimeProbe {
        Arc::new(|_: &str| -> Option<SystemTime> { None })
    }

    fn must_head(raw: &[u8]) -> Head {
        match Head::parse(raw) {
            Ok(head) => head,
            Err(status) => panic!("head was rejected with {status}"),
        }
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn status_line(bytes: &[u8]) -> String {
        text(bytes).split("\r\n").next().expect("status line").to_string()
    }

    fn wire_line(bytes: &[u8], name: &str) -> Option<String> {
        text(bytes).split("\r\n").find_map(|line| {
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|value| value.trim().to_string())
        })
    }

    fn request(method: &str, target: &str, headers: &[(&str, &str)]) -> Request {
        request_with_body(method, target, headers, b"")
    }

    fn request_with_body(
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Request {
        let mut block = format!("{method} {target} HTTP/1.1\r\n");
        for (name, value) in headers {
            block.push_str(&format!("{name}: {value}\r\n"));
        }
        block.push_str("\r\n");
        let mut raw = block.into_bytes();
        raw.extend_from_slice(body);
        match scan_head(&raw) {
            HeadState::Ready { head, consumed } => Request::new(head, raw[consumed..].to_vec()),
            _ => panic!("request helper expected a ready head"),
        }
    }

    struct FakeControl {
        reply: Mutex<crate::soap::Reply>,
        bodies: Mutex<Vec<Vec<u8>>>,
    }

    impl FakeControl {
        fn soap_ok() -> Self {
            FakeControl {
                reply: Mutex::new(crate::soap::Reply {
                    status: 200,
                    content_type: crate::soap::SOAP_CONTENT_TYPE,
                    body: b"<soap>ok</soap>".to_vec(),
                }),
                bodies: Mutex::new(Vec::new()),
            }
        }

        fn json_ok() -> Self {
            FakeControl {
                reply: Mutex::new(crate::soap::Reply {
                    status: 200,
                    content_type: JSON_CONTENT_TYPE,
                    body: b"{}".to_vec(),
                }),
                bodies: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.bodies.lock().expect("bodies").len()
        }

        fn last_body(&self) -> Vec<u8> {
            self.bodies
                .lock()
                .expect("bodies")
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    impl Control for FakeControl {
        fn handle(&self, rawbody: &[u8]) -> crate::soap::Reply {
            self.bodies.lock().expect("bodies").push(rawbody.to_vec());
            let guard = self.reply.lock().expect("reply");
            crate::soap::Reply {
                status: guard.status,
                content_type: guard.content_type,
                body: guard.body.clone(),
            }
        }
    }

    struct FakeSubscription {
        renew_status: AtomicU16,
        remove_status: AtomicU16,
        add_reply: Mutex<(String, String)>,
        events: Mutex<Vec<String>>,
    }

    impl FakeSubscription {
        fn new() -> Self {
            FakeSubscription {
                renew_status: AtomicU16::new(200),
                remove_status: AtomicU16::new(200),
                add_reply: Mutex::new(("GENA-sid-1".to_string(), "Second-900".to_string())),
                events: Mutex::new(Vec::new()),
            }
        }

        fn set_renew_status(&self, status: u16) {
            self.renew_status.store(status, Ordering::SeqCst);
        }

        fn set_remove_status(&self, status: u16) {
            self.remove_status.store(status, Ordering::SeqCst);
        }

        fn journal(&self) -> Vec<String> {
            self.events.lock().expect("events").clone()
        }
    }

    impl Subscription for FakeSubscription {
        fn renew(&self, sid: &str, timeout: i64) -> u16 {
            self.events
                .lock()
                .expect("events")
                .push(format!("renew {sid} {timeout}"));
            self.renew_status.load(Ordering::SeqCst)
        }

        fn add(&self, service: &str, url: &str, timeout: i64) -> (String, String) {
            self.events
                .lock()
                .expect("events")
                .push(format!("add {service} {url} {timeout}"));
            self.add_reply.lock().expect("add reply").clone()
        }

        fn remove(&self, sid: &str) -> u16 {
            self.events.lock().expect("events").push(format!("remove {sid}"));
            self.remove_status.load(Ordering::SeqCst)
        }
    }

    /// The management seam as a call log.
    ///
    /// It records the **(address, name) pair** rather than a sentence, because the
    /// two things a caller can get wrong here are exactly the two things the log
    /// can show: casting something that was refused, and changing the address
    /// without changing the name (AGENTS.md §4.2). What it *answers* is a string
    /// only this file reads, so a test that asserts on the answer is asserting on
    /// the fake — the assertions below therefore check the **document** shape
    /// (`code` 0 vs 1) and the log, not the wording.
    struct FakeManagement {
        refuses: AtomicBool,
        calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl FakeManagement {
        fn new() -> Self {
            FakeManagement {
                refuses: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// The shell's refusal shape: a sentence, never a status code.
        fn refusing() -> Self {
            let fake = FakeManagement::new();
            fake.refuses.store(true, Ordering::SeqCst);
            fake
        }

        fn calls(&self) -> Vec<(String, Option<String>)> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl Management for FakeManagement {
        fn cast_url(&self, url: &str, title: Option<&str>) -> Result<String, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((url.to_string(), title.map(str::to_string)));
            if self.refuses.load(Ordering::SeqCst) {
                return Err("播放器拒绝了这次投屏".to_string());
            }
            Ok(format!("已投屏：{url}"))
        }
    }

    #[test]
    fn verb_matching_is_case_insensitive() {
        crate::pin_test_dir();
        let (face, control, _) = harness("verb-case");
        let ok = face.answer(&request("gEt", "/", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.body, ROOT_PAGE.as_bytes());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn only_the_four_verbs_are_exempt_from_a_length() {
        crate::pin_test_dir();
        let (face, control, _) = harness("exempt");
        let ok = face.answer(&request("GET", "/", &[]));
        assert_eq!(ok.status, 200);
        let quiet = face.answer(&request("SUBSCRIBE", "/RenderingControl/not-event", &[]));
        assert_eq!(quiet.status, 200);
        assert_eq!(quiet.content_type, crate::soap::SOAP_CONTENT_TYPE);
        assert!(quiet.body.is_empty());
        assert!(quiet.variant.is_empty());
        let post = face.answer(&request("POST", "/RenderingControl/action", &[]));
        assert_eq!(post.status, 411);
        let put = face.answer(&request("PUT", "/", &[]));
        assert_eq!(put.status, 411);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn reason_phrases_and_the_empty_one_for_unknown() {
        crate::pin_test_dir();
        let (face, _, _) = harness("reasons");
        let cases: [(u16, &str); 9] = [
            (200, "OK"),
            (303, "See Other"),
            (400, "Bad Request"),
            (405, "Method Not Allowed"),
            (411, "Length Required"),
            (412, "Precondition Failed"),
            (413, "Request Entity Too Large"),
            (500, "Internal Server Error"),
            (418, ""),
        ];
        for (status, phrase) in cases {
            assert_eq!(Response::reason(status), phrase, "reason for {status}");
        }
        let wired = status_line(&face.refusal(405).wire("d", "s"));
        assert_eq!(wired, "HTTP/1.1 405 Method Not Allowed");
    }

    #[test]
    fn wire_puts_the_variant_slot_between_date_and_length() {
        crate::pin_test_dir();
        let (face, _, _) = harness("wire-order");
        let mut response = face.refusal(405);
        response.variant.push(("X-One".to_string(), "1".to_string()));
        response.variant.push(("X-Two".to_string(), "2".to_string()));
        let rendered = text(&response.wire("my-date", "my-server"));
        assert!(rendered.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(rendered.contains("Connection: close"));
        let at = |name: &str| rendered.find(&format!("{name}: ")).expect(name);
        assert!(at("Server") < at("Date"));
        assert!(at("Date") < at("X-One"));
        assert!(at("X-One") < at("X-Two"));
        assert!(at("X-Two") < at("Content-Length"));
    }

    #[test]
    fn wire_never_emits_allow() {
        crate::pin_test_dir();
        let (face, control, _) = harness("no-allow");
        let response = face.answer(&request_with_body("PUT", "/", &[("Content-Length", "2")], b"hi"));
        assert_eq!(response.status, 405);
        let bytes = response.wire("d", "s");
        assert!(wire_line(&bytes, "Allow").is_none());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn head_header_names_are_kept_verbatim() {
        crate::pin_test_dir();
        let head = must_head(b"GET / HTTP/1.1\r\nsId: first\r\nSID: second\r\n\r\n");
        assert_eq!(head.headers[0].0, "sId");
        assert_eq!(head.headers[1].0, "SID");
        let req = Request::new(head, Vec::new());
        assert_eq!(req.header("sid"), Some("first"));
    }

    #[test]
    fn head_parse_refours_the_four_broken_shapes() {
        crate::pin_test_dir();
        let not_utf8: &[u8] = &[0xffu8, 0xfeu8];
        let cases: [&[u8]; 4] = [
            not_utf8,
            b"GET /\r\n\r\n",
            b"GET / HTTP/1.1\r\nbadheader\r\n\r\n",
            b"GET / HTTP/1.1\r\n: value\r\n\r\n",
        ];
        for raw in cases {
            match Head::parse(raw) {
                Err(status) => assert_eq!(status, 400),
                Ok(_) => panic!("broken head was accepted"),
            }
        }
    }

    #[test]
    fn head_parse_keeps_a_bare_lf_line() {
        crate::pin_test_dir();
        let head = must_head(b"GET / HTTP/1.1\nHost: x\n\n");
        assert_eq!(head.header("host"), Some("x"));
    }

    #[test]
    fn scan_head_consumed_is_the_terminator_plus_four() {
        crate::pin_test_dir();
        let head_text = "GET / HTTP/1.1\r\nHost: x";
        let mut raw = head_text.as_bytes().to_vec();
        raw.extend_from_slice(b"\r\n\r\n");
        match scan_head(&raw) {
            HeadState::Ready { head, consumed } => {
                assert_eq!(head.target, "/");
                assert_eq!(consumed, head_text.len() + 4);
            }
            other => panic!("expected a ready head, got {other:?}"),
        }
    }

    #[test]
    fn scan_head_is_strict_about_the_terminator() {
        crate::pin_test_dir();
        assert!(matches!(
            scan_head(b"GET / HTTP/1.1\r\nHost: x\n\n"),
            HeadState::Incomplete
        ));
    }

    #[test]
    fn scan_head_refuses_an_oversized_head() {
        crate::pin_test_dir();
        let raw = format!("GET / HTTP/1.1\r\nHost: {}", "a".repeat(MAX_HEAD_BYTES + 1));
        match scan_head(raw.as_bytes()) {
            HeadState::Bad(status) => assert_eq!(status, 400),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(
            scan_head(b"GET / HTTP/1.1\r\n\r\n"),
            HeadState::Ready { .. }
        ));
    }

    #[test]
    fn find_header_is_case_insensitive_first_wins() {
        crate::pin_test_dir();
        let headers = vec![
            ("Content-Type".to_string(), "text/xml".to_string()),
            ("content-type".to_string(), "other".to_string()),
        ];
        assert_eq!(find_header(&headers, "content-type"), Some("text/xml"));
        assert_eq!(find_header(&headers, "missing"), None);
    }

    #[test]
    fn declared_length_reads_only_non_negative_numbers() {
        crate::pin_test_dir();
        let cases: [(&str, Option<i64>); 4] = [
            ("3", Some(3)),
            (" 7 ", Some(7)),
            ("-1", None),
            ("abc", None),
        ];
        for (value, expected) in cases {
            let req = request("POST", "/", &[("Content-Length", value)]);
            assert_eq!(req.declared_length(), expected, "declared length for {value:?}");
        }
        let req = request("POST", "/", &[]);
        assert_eq!(req.declared_length(), None);
    }

    #[test]
    fn parse_query_keeps_names_verbatim() {
        crate::pin_test_dir();
        let req = request("GET", "/?a=1&weird%20name=x&flag", &[]);
        assert_eq!(
            req.query,
            vec![
                ("a".to_string(), "1".to_string()),
                ("weird%20name".to_string(), "x".to_string()),
                ("flag".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn split_once_ascii_on_the_first_only() {
        crate::pin_test_dir();
        assert_eq!(split_once_ascii("a=b=c", '='), ("a", "b=c"));
        assert_eq!(split_once_ascii("plain", '='), ("plain", ""));
    }

    #[test]
    fn segments_swallow_extras_and_keep_encoded_dots() {
        crate::pin_test_dir();
        let (face, _, _) = harness("segments");
        let ok = face.answer(&request("GET", "/description.xml/x", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.body, face.description_bytes());
        let encoded_dot = face.answer(&request("GET", "/description%2Exml", &[]));
        assert_eq!(encoded_dot.status, 303);
    }

    #[test]
    fn encoded_slash_is_never_a_boundary() {
        crate::pin_test_dir();
        let (face, _, _) = harness("encoded-slash");
        let response = face.answer(&request("GET", "/dlna%2FAVTransport.xml", &[]));
        assert_eq!(response.status, 303);
    }

    #[test]
    fn pre_route_400_beats_411() {
        crate::pin_test_dir();
        let (face, control, _) = harness("pre-400");
        let response =
            face.answer(&request("POST", "/RenderingControl/action", &[("Content-Length", "abc")]));
        assert_eq!(response.status, 400);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn pre_route_411_beats_405() {
        crate::pin_test_dir();
        let (face, control, _) = harness("pre-411");
        let missing = face.answer(&request("DELETE", "/", &[]));
        assert_eq!(missing.status, 411);
        let declared =
            face.answer(&request_with_body("DELETE", "/", &[("Content-Length", "2")], b"hi"));
        assert_eq!(declared.status, 405);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn post_with_only_transfer_encoding_is_411() {
        crate::pin_test_dir();
        let (face, control, _) = harness("te-post");
        let response = face.answer(&request(
            "POST",
            "/RenderingControl/action",
            &[("Transfer-Encoding", "chunked")],
        ));
        assert_eq!(response.status, 411);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn non_post_with_transfer_encoding_reaches_routing() {
        crate::pin_test_dir();
        let (face, control, _) = harness("te-put");
        let response = face.answer(&request("PUT", "/", &[("Transfer-Encoding", "chunked")]));
        assert_eq!(response.status, 405);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn over_cap_length_is_413_before_the_body() {
        crate::pin_test_dir();
        let (face, control, _) = harness("over-cap");
        let too_big = (MAX_BODY_BYTES + 1).to_string();
        let response = face.answer(&request(
            "POST",
            "/RenderingControl/action",
            &[("Content-Length", too_big.as_str())],
        ));
        assert_eq!(response.status, 413);
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn both_headers_content_length_wins() {
        crate::pin_test_dir();
        let (face, control, _) = harness("both-headers");
        let server = HttpServer::new(face);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut client = TcpStream::connect(addr).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        client
            .write_all(
                b"POST /RenderingControl/action HTTP/1.1\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\nabc",
            )
            .expect("write request");
        let reader = std::thread::spawn(move || {
            let mut client = client;
            let mut bytes = Vec::new();
            let _ = client.read_to_end(&mut bytes);
            bytes
        });
        server.serve_stream(stream);
        let bytes = reader.join().expect("reader joins");
        assert!(text(&bytes).starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(control.last_body(), b"abc".to_vec());
    }

    #[test]
    fn refusal_bodies_carry_status_and_phrase() {
        crate::pin_test_dir();
        let (face, _, _) = harness("refusals");
        assert_eq!(face.refusal(405).body, REFUSAL_405_BODY.as_bytes());
        let body = text(&face.refusal(411).body);
        assert!(body.starts_with("<html><body><h2>411 "));
        assert!(body.contains("Length Required"));
    }

    #[test]
    fn redirect_body_is_the_measured_sentence() {
        crate::pin_test_dir();
        let (face, _, _) = harness("redirect-sentence");
        let response = face.answer(&request("GET", "/Nope", &[("Host", "tv.example:58880")]));
        assert_eq!(response.status, 303);
        let location = face.redirect_location(Some("tv.example:58880"));
        assert_eq!(
            response.body,
            format!("This resource can be found at <a href=\"{location}\">{location}</a>.")
                .into_bytes()
        );
        assert_eq!(
            wire_line(&response.wire("d", "s"), "Location"),
            Some(location)
        );
    }

    #[test]
    fn redirect_location_follows_host_or_loopback() {
        crate::pin_test_dir();
        let (face, _, _) = harness("redirect-host");
        assert_eq!(
            face.redirect_location(Some("  tv.example:58880 ")),
            "http://tv.example:58880/"
        );
        assert_eq!(
            face.redirect_location(None),
            format!("http://127.0.0.1:{}/", face.port())
        );
        assert_eq!(
            face.redirect_location(Some("   ")),
            format!("http://127.0.0.1:{}/", face.port())
        );
    }

    #[test]
    fn description_answers_with_the_soap_content_type() {
        crate::pin_test_dir();
        let (face, _, _) = harness("description");
        let ok = face.answer(&request("GET", "/description.xml", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.content_type, crate::soap::SOAP_CONTENT_TYPE);
        assert_eq!(ok.body, face.description_bytes());
        let wrong_case = face.answer(&request("GET", "/Description.xml", &[]));
        assert_eq!(wrong_case.status, 303);
    }

    #[test]
    fn root_page_names_the_four_addresses() {
        crate::pin_test_dir();
        let (face, _, _) = harness("root-page");
        let ok = face.answer(&request("GET", "/", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(text(&ok.body), ROOT_PAGE);
        for needle in [
            "<code>/description.xml</code>",
            "<code>/dlna/&lt;Service&gt;.xml</code>",
            "<code>/&lt;service&gt;/action</code>",
            "<code>/&lt;service&gt;/event</code>",
        ] {
            assert!(ROOT_PAGE.contains(needle), "root page must name {needle}");
        }
    }

    #[test]
    fn scpd_ships_accept_ranges_always() {
        crate::pin_test_dir();
        let face = harness_probe("scpd-silent", silent_mtime());
        let ok = face.answer(&request("GET", "/dlna/AVTransport.xml", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.content_type, SCPD_CONTENT_TYPE);
        let bytes = ok.wire("d", "s");
        assert!(text(&bytes).contains("Accept-Ranges: bytes"));
        assert_eq!(wire_line(&bytes, "Last-Modified"), None);
    }

    #[test]
    fn scpd_last_modified_comes_from_the_probe() {
        crate::pin_test_dir();
        let face = harness_probe("scpd-lm", fixed_mtime());
        let ok = face.answer(&request("GET", "/dlna/RenderingControl.xml", &[]));
        assert_eq!(ok.status, 200);
        let bytes = ok.wire("d", "s");
        let rendered = text(&bytes);
        let last_modified = rendered.find("Last-Modified:").expect("last-modified");
        let accept_ranges = rendered.find("Accept-Ranges:").expect("accept-ranges");
        assert!(last_modified < accept_ranges);
        assert_eq!(
            wire_line(&bytes, "Last-Modified"),
            Some(timefmt::rfc1123_gmt(FIXED_SECS))
        );
    }

    #[test]
    fn unknown_scpd_names_redirect() {
        crate::pin_test_dir();
        let (face, _, _) = harness("scpd-unknown");
        let response = face.answer(&request("GET", "/dlna/nope.xml", &[]));
        assert_eq!(response.status, 303);
    }

    #[test]
    fn scpd_file_rel_and_doc_for_service_agree() {
        crate::pin_test_dir();
        for name in ["AVTransport.xml", "RenderingControl.xml", "ConnectionManager.xml"] {
            let rel = scpd_file_rel(name).expect("rel");
            assert_eq!(rel, format!("xml/{name}"));
            let service = name.strip_suffix(".xml").expect("service name");
            assert!(
                crate::scpd::doc_for_service(service).is_some(),
                "{service} has no doc"
            );
        }
        assert!(scpd_file_rel("nope.xml").is_none());
    }

    #[test]
    fn double_xml_suffix_is_not_stripped_twice() {
        crate::pin_test_dir();
        let (face, _, _) = harness("double-suffix");
        let response = face.answer(&request("GET", "/dlna/AVTransport.xml.xml", &[]));
        assert_eq!(response.status, 303);
    }

    #[test]
    fn head_advertises_length_and_withholds_body() {
        crate::pin_test_dir();
        let (face, _, _) = harness("head-length");
        let response = face.answer(&request("HEAD", "/", &[]));
        assert_eq!(response.status, 200);
        let bytes = response.wire("d", "s");
        let rendered = text(&bytes);
        assert!(rendered.contains(&format!("Content-Length: {}", ROOT_PAGE.len())));
        assert_eq!(bytes.len(), rendered.find("\r\n\r\n").expect("head terminator") + 4);
    }

    #[test]
    fn post_action_passes_the_body_through() {
        crate::pin_test_dir();
        let (face, control, _) = harness("post-action");
        let ok = face.answer(&request_with_body(
            "POST",
            "/AVTransport/action",
            &[("Content-Length", "11")],
            b"<envelope/>",
        ));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.content_type, crate::soap::SOAP_CONTENT_TYPE);
        assert_eq!(control.last_body(), b"<envelope/>".to_vec());
        let ext = ok.variant.iter().find(|(name, _)| name.as_str() == "Ext");
        assert_eq!(ext, Some(&("Ext".to_string(), String::new())));
    }

    #[test]
    fn ext_header_only_for_soap_shaped_answers() {
        crate::pin_test_dir();
        let control = Arc::new(FakeControl::json_ok());
        let face = face_for("ext-json", control.clone(), Arc::new(FakeSubscription::new()));
        let ok = face.answer(&request_with_body(
            "POST",
            "/api/action",
            &[("Content-Length", "3")],
            b"abc",
        ));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.content_type, JSON_CONTENT_TYPE);
        assert_eq!(ok.body, b"{}".to_vec());
        assert!(ok.variant.iter().all(|(name, _)| name.as_str() != "Ext"));
        assert_eq!(control.calls(), 1);
    }

    #[test]
    fn post_without_action_segment_falls_through() {
        crate::pin_test_dir();
        let (face, control, _) = harness("fall-simple");
        let response = face.answer(&request("POST", "/api", &[("Content-Length", "0")]));
        assert_eq!(response.body, FALL_THROUGH_BODY.as_bytes());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn fall_through_names_unrecognised_fields() {
        crate::pin_test_dir();
        let (face, control, _) = harness("fall-unknown");
        let response = face.answer(&request(
            "POST",
            "/api?clear-logg=1&token=abc",
            &[("Content-Length", "0")],
        ));
        assert_eq!(
            response.body,
            py_json_dumps_indented(1, "没有可执行的操作：不认识字段 clear-logg").into_bytes()
        );
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn helper_fields_are_not_read_back() {
        crate::pin_test_dir();
        let (face, control, _) = harness("fall-helpers");
        let response =
            face.answer(&request("POST", "/api?job=7&target=tv", &[("Content-Length", "0")]));
        assert_eq!(response.body, FALL_THROUGH_BODY.as_bytes());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn form_body_names_join_the_list() {
        crate::pin_test_dir();
        let (face, control, _) = harness("fall-form");
        let response = face.answer(&request_with_body(
            "POST",
            "/api",
            &[
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Content-Length", "12"),
            ],
            b"weird-name=1",
        ));
        assert_eq!(
            response.body,
            py_json_dumps_indented(1, "没有可执行的操作：不认识字段 weird-name").into_bytes()
        );
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn a_soap_body_is_not_scanned_for_field_names() {
        crate::pin_test_dir();
        let (face, control, _) = harness("fall-soap");
        let response = face.answer(&request_with_body(
            "POST",
            "/api",
            &[
                ("Content-Type", "text/xml;charset=utf-8"),
                ("Content-Length", "13"),
            ],
            b"<weird-name/>",
        ));
        assert_eq!(response.body, FALL_THROUGH_BODY.as_bytes());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn py_json_quote_escapes_as_python_does() {
        crate::pin_test_dir();
        assert_eq!(py_json_quote(QUOTE_IN), QUOTE_OUT);
    }

    #[test]
    fn py_json_dumps_indented_layout() {
        crate::pin_test_dir();
        assert_eq!(py_json_dumps_indented(0, "ok"), DUMPS_OK_BODY);
        assert_eq!(py_json_dumps_indented(2, "he said \"hi\""), DUMPS_QUOTED_BODY);
    }

    #[test]
    fn subscribe_wrong_last_segment_is_a_quiet_200() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-quiet");
        let ok = face.answer(&request("SUBSCRIBE", "/AVTransport/not-event", &[]));
        assert_eq!(ok.status, 200);
        assert_eq!(ok.content_type, crate::soap::SOAP_CONTENT_TYPE);
        assert!(ok.body.is_empty());
        assert!(ok.variant.is_empty());
        assert!(subscription.journal().is_empty());
    }

    #[test]
    fn subscribe_renew_writes_the_int_timeout() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-renew");
        let ok = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9"), ("TIMEOUT", "Second-600")],
        ));
        assert_eq!(ok.status, 200);
        let bytes = ok.wire("d", "s");
        assert_eq!(wire_line(&bytes, "Sid"), Some("sid-9".to_string()));
        assert_eq!(wire_line(&bytes, "Timeout"), Some("600".to_string()));
        assert_eq!(subscription.journal(), vec!["renew sid-9 600".to_string()]);
    }

    #[test]
    fn subscribe_new_writes_verbatim_strings() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-new");
        let ok = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("CALLBACK", "<http://192.0.2.7:5000/cb>")],
        ));
        assert_eq!(ok.status, 200);
        let bytes = ok.wire("d", "s");
        assert_eq!(wire_line(&bytes, "Sid"), Some("GENA-sid-1".to_string()));
        assert_eq!(wire_line(&bytes, "Timeout"), Some("Second-900".to_string()));
        assert_eq!(
            subscription.journal(),
            vec!["add RenderingControl http://192.0.2.7:5000/cb 1800".to_string()]
        );
    }

    #[test]
    fn subscribe_refused_renew_passes_the_status() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-renew-refused");
        subscription.set_renew_status(412);
        let response = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9"), ("TIMEOUT", "Second-600")],
        ));
        assert_eq!(response.status, 412);
        assert_eq!(subscription.journal(), vec!["renew sid-9 600".to_string()]);
    }

    #[test]
    fn subscribe_reads_timeout_before_the_branch() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-bad-timeout");
        let response = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9"), ("TIMEOUT", "nonsense")],
        ));
        assert_eq!(response.status, 412);
        assert!(subscription.journal().is_empty());
    }

    #[test]
    fn subscribe_bare_seconds_timeout_is_accepted() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-bare");
        let ok = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9"), ("TIMEOUT", "600")],
        ));
        assert_eq!(ok.status, 200);
        assert_eq!(subscription.journal(), vec!["renew sid-9 600".to_string()]);
    }

    #[test]
    fn subscribe_absent_timeout_defaults_to_1800() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-default");
        let ok = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9")],
        ));
        assert_eq!(ok.status, 200);
        assert_eq!(subscription.journal(), vec!["renew sid-9 1800".to_string()]);
    }

    #[test]
    fn subscribe_unreadable_callback_is_412() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-bad-callback");
        let response = face.answer(&request(
            "SUBSCRIBE",
            "/RenderingControl/event",
            &[("CALLBACK", "no brackets here")],
        ));
        assert_eq!(response.status, 412);
        assert!(subscription.journal().is_empty());
    }

    #[test]
    fn subscribe_neither_sid_nor_callback_is_412() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("subscribe-empty");
        let response = face.answer(&request("SUBSCRIBE", "/RenderingControl/event", &[]));
        assert_eq!(response.status, 412);
        assert!(subscription.journal().is_empty());
    }

    #[test]
    fn first_angle_bracket_takes_the_first() {
        crate::pin_test_dir();
        assert_eq!(first_angle_bracket("<a><b>"), Some("a"));
        assert_eq!(first_angle_bracket("<>"), Some(""));
        assert_eq!(first_angle_bracket("no brackets"), None);
        assert_eq!(first_angle_bracket("<unclosed"), None);
    }

    #[test]
    fn unsubscribe_only_event_and_only_with_a_sid() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness("unsubscribe-gates");
        let wrong_route =
            face.answer(&request("UNSUBSCRIBE", "/RenderingControl/other", &[("SID", "sid-9")]));
        assert_eq!(wrong_route.status, 412);
        let no_sid = face.answer(&request("UNSUBSCRIBE", "/RenderingControl/event", &[]));
        assert_eq!(no_sid.status, 412);
        let empty_sid =
            face.answer(&request("UNSUBSCRIBE", "/RenderingControl/event", &[("SID", "")]));
        assert_eq!(empty_sid.status, 412);
        let ok = face.answer(&request(
            "UNSUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9")],
        ));
        assert_eq!(ok.status, 200);
        assert!(ok.variant.is_empty());
        assert_eq!(subscription.journal(), vec!["remove sid-9".to_string()]);
        subscription.set_remove_status(412);
        let refused = face.answer(&request(
            "UNSUBSCRIBE",
            "/RenderingControl/event",
            &[("SID", "sid-9")],
        ));
        assert_eq!(refused.status, 412);
        assert_eq!(
            subscription.journal(),
            vec!["remove sid-9".to_string(), "remove sid-9".to_string()]
        );
    }

    #[test]
    fn truthy_helpers() {
        crate::pin_test_dir();
        assert_eq!(truthy(None), None);
        assert_eq!(truthy(Some("")), None);
        assert_eq!(truthy(Some(" ")), Some(" "));
    }

    #[test]
    fn parse_second_timeout_shapes() {
        crate::pin_test_dir();
        assert_eq!(parse_second_timeout("Second-600"), Some(600));
        assert_eq!(parse_second_timeout("600"), Some(600));
        assert_eq!(parse_second_timeout("Second-600 "), Some(600));
        assert_eq!(parse_second_timeout("second-infinite"), None);
        assert_eq!(parse_second_timeout(""), None);
    }

    #[test]
    fn epoch_secs_both_directions() {
        crate::pin_test_dir();
        assert_eq!(epoch_secs(UNIX_EPOCH + Duration::from_secs(100)), 100);
        assert_eq!(
            epoch_secs(
                UNIX_EPOCH
                    .checked_sub(Duration::from_secs(50))
                    .expect("before epoch")
            ),
            -50
        );
    }

    #[test]
    fn form_field_names_gate() {
        crate::pin_test_dir();
        let form = request_with_body(
            "POST",
            "/api",
            &[
                ("Content-Type", "APPLICATION/X-WWW-FORM-URLENCODED"),
                ("Content-Length", "7"),
            ],
            b"aa=1&bb",
        );
        assert_eq!(
            form_field_names(&form),
            vec!["aa".to_string(), "bb".to_string()]
        );
        let no_type = request_with_body("POST", "/api", &[("Content-Length", "0")], b"");
        assert!(form_field_names(&no_type).is_empty());
        let xml = request_with_body(
            "POST",
            "/api",
            &[("Content-Type", "text/xml"), ("Content-Length", "4")],
            b"<a/>",
        );
        assert!(form_field_names(&xml).is_empty());
    }

    #[test]
    fn the_control_trait_accepts_a_soap() {
        crate::pin_test_dir();
        fn coerce(soap: Arc<crate::soap::Soap>) -> Arc<dyn Control> {
            soap
        }
        let _ = coerce as fn(Arc<crate::soap::Soap>) -> Arc<dyn Control>;
    }

    #[test]
    fn serve_stream_answers_over_a_real_socket() {
        crate::pin_test_dir();
        let (face, control, _) = harness("serve-get");
        let server = HttpServer::new(face);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut client = TcpStream::connect(addr).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").expect("write request");
        let reader = std::thread::spawn(move || {
            let mut client = client;
            let mut bytes = Vec::new();
            let _ = client.read_to_end(&mut bytes);
            bytes
        });
        server.serve_stream(stream);
        let bytes = reader.join().expect("reader joins");
        let rendered = text(&bytes);
        assert!(rendered.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(rendered.contains(ROOT_PAGE));
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn serve_stream_refuses_413_without_reading_the_body() {
        crate::pin_test_dir();
        let (face, control, _) = harness("serve-413");
        let server = HttpServer::new(face);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut client = TcpStream::connect(addr).expect("connect");
        let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
        let (stream, _) = listener.accept().expect("accept");
        let written = format!(
            "POST /x HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        client.write_all(written.as_bytes()).expect("write request");
        let started = Instant::now();
        let reader = std::thread::spawn(move || {
            let mut client = client;
            let mut bytes = Vec::new();
            let _ = client.read_to_end(&mut bytes);
            bytes
        });
        server.serve_stream(stream);
        let bytes = reader.join().expect("reader joins");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the 413 must not wait on a body that was never sent"
        );
        assert!(text(&bytes).starts_with("HTTP/1.1 413 "));
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn serve_stream_is_silent_for_a_closed_connection() {
        crate::pin_test_dir();
        let (face, control, _) = harness("serve-closed");
        let server = HttpServer::new(face);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut client = TcpStream::connect(addr).expect("connect");
        let _ = client.shutdown(std::net::Shutdown::Both);
        let (stream, _) = listener.accept().expect("accept");
        server.serve_stream(stream);
        let mut bytes = Vec::new();
        let _ = client.read_to_end(&mut bytes);
        assert!(bytes.is_empty());
        assert_eq!(control.calls(), 0);
    }

    #[test]
    fn server_lifecycle_on_an_ephemeral_port() {
        crate::pin_test_dir();
        let face = face_on_port("server-lifecycle", 0);
        let server = HttpServer::new(face);
        assert!(!server.is_running());
        server.start();
        assert!(server.is_running());
        let port = server.bound_port();
        assert!(port > 0);
        assert!(server.is_healthy());
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").expect("write request");
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).expect("read response");
        let rendered = text(&bytes);
        assert!(rendered.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(rendered.contains(ROOT_PAGE));
        server.stop();
        assert!(!server.is_running());
        assert!(server.is_healthy());
    }

    #[test]
    fn face_bakes_the_description_at_startup() {
        crate::pin_test_dir();
        let setting = fresh_setting("description-bake");
        let control: Arc<dyn Control> = Arc::new(FakeControl::soap_ok());
        let subscription: Arc<dyn Subscription> = Arc::new(FakeSubscription::new());
        let face = face_built(
            setting.clone(),
            control,
            subscription,
            Arc::new(FakeManagement::new()),
        );
        assert_eq!(
            face.description_bytes(),
            crate::description::build_for(&setting)
                .expect("build")
                .as_slice()
        );
        assert_eq!(face.port(), setting::application_port(&setting));
    }

    // -- GET /api: the management face --------------------------------------

    const CAST_TARGET: &str = "/api?query=cast&url=http://host/a.mp4";
    const LOCAL: &str = "127.0.0.1";
    const LAN: &str = "192.168.1.9";
    const SECRET: &str = "deadbeefcafe0123";
    const TEST_PORT: u16 = 58993;
    const MISSING_URL: &str = "缺少参数 url：投屏入口需要一个完整的媒体地址。";
    const NO_QUERY: &str = "没有可执行的操作：缺少 query 参数。";
    const OK_DOC: &str = "{\n    \"code\": 0,\n    \"message\": \"";
    const REFUSED_DOC: &str = "{\n    \"code\": 1,\n    \"message\": \"";

    /// Store a secret, or store **nothing**. The second half is the point: a
    /// helper that always wrote a value would leave `Api_Token` as `Some("")`,
    /// and every "no token has been born yet" assertion below would then be
    /// checking an empty string instead of an absent key.
    fn store_token(setting: &Arc<Setting>, token: Option<&str>) {
        if let Some(token) = token {
            setting.set(setting::Key::ApiToken, serde_json::Value::from(token));
        }
    }

    /// The shape-refusal sentence for one address, from the one template
    /// [`Face::cast_entry`] writes. Tests compare whole documents rather than
    /// grepping for a fragment, because a fragment passes for the wrong reason.
    fn not_absolute(value: &str) -> String {
        format!("{value} 不是一个完整的媒体地址：需要带协议和主机的绝对地址，例如 http://主机/路径。")
    }

    #[test]
    fn loopback_does_not_exempt_the_cast_entry() {
        crate::pin_test_dir();
        // AGENTS.md §4.7, the whole reason this route has its own gate: any
        // webpage can issue `GET http://127.0.0.1:58880/api?query=cast&...`, so
        // "this came from this machine" buys nothing here.
        let (face, management) = harness_management("cast-loopback");
        let response = face.answer(&get_api(CAST_TARGET, LOCAL));
        assert_eq!(response.status, 403);
        assert_eq!(response.body, json_body(1, TOKEN_REFUSAL));
        assert!(management.calls().is_empty());
    }

    #[test]
    fn cast_with_the_token_reaches_the_shell() {
        crate::pin_test_dir();
        let (face, management) = harness_management("cast-token");
        store_token(&face.setting, Some(SECRET));
        let target = format!("{CAST_TARGET}&token={SECRET}");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 200);
        assert!(
            response.body.starts_with(OK_DOC.as_bytes()),
            "a served cast is code 0: {}",
            text(&response.body)
        );
        assert_eq!(
            management.calls(),
            vec![("http://host/a.mp4".to_string(), None)]
        );
    }

    #[test]
    fn a_lan_caller_with_a_wrong_token_hears_the_management_sentence() {
        crate::pin_test_dir();
        // Cumulative and ordered: the token route pays the management check
        // **first**, so a LAN caller who typed a wrong token is told they are not
        // allowed in, not that they mistyped the secret. Swapping the branches
        // changes the message, never the verdict — this asks which message ships.
        let (face, management) = harness_management("lan-wrong-token");
        store_token(&face.setting, Some(SECRET));
        let target = format!("{CAST_TARGET}&token=not-the-secret");
        let response = face.answer(&get_api(&target, LAN));
        assert_eq!(response.status, 403);
        assert_eq!(response.body, json_body(1, MANAGEMENT_REFUSAL));
        assert!(management.calls().is_empty());
    }

    #[test]
    fn the_token_opens_the_gate_for_a_lan_peer() {
        crate::pin_test_dir();
        let (face, management) = harness_management("lan-right-token");
        store_token(&face.setting, Some(SECRET));
        assert!(!is_loopback(LAN), "the fixture peer must not be us");
        let target = format!("{CAST_TARGET}&token={SECRET}");
        let response = face.answer(&get_api(&target, LAN));
        assert_eq!(response.status, 200);
        assert_eq!(management.calls().len(), 1);
    }

    #[test]
    fn every_refusal_shape_leaves_the_player_untouched() {
        crate::pin_test_dir();
        // The bug this table exists to prevent is a refusal that casts anyway.
        // Only the call log can tell "we said no" from "we said no and played it".
        let (face, management) = harness_management("refusal-shapes");
        store_token(&face.setting, Some(SECRET));
        let shaped = format!("{CAST_TARGET}&token=");
        let longer = format!("{CAST_TARGET}&token={SECRET}x");
        let hollow = String::new();
        let refusals: [(&str, &str); 5] = [
            (CAST_TARGET, LAN),
            ("/api?query=cast-info", LAN),
            (&shaped, LOCAL),
            (&longer, LOCAL),
            (CAST_TARGET, hollow.as_str()),
        ];
        for (target, remote) in refusals {
            let response = face.answer(&get_api(&target, &remote));
            assert_eq!(response.status, 403, "{target} from {remote:?}");
            assert!(
                response.body.starts_with(REFUSED_DOC.as_bytes()),
                "a refusal is code 1: {}",
                text(&response.body)
            );
            assert!(management.calls().is_empty(), "{target} cast anyway");
        }
        // Positive control: the same face does serve a legal request, so the
        // loop above cannot be green because the route is unreachable.
        let served = format!("{CAST_TARGET}&token={SECRET}");
        assert_eq!(face.answer(&get_api(&served, LOCAL)).status, 200);
        assert_eq!(management.calls().len(), 1);
    }

    #[test]
    fn a_lan_caller_never_hears_the_token() {
        crate::pin_test_dir();
        // `cast-info` is the only place the secret is revealed, so its gate being
        // closed to the LAN is the whole defence for `query=cast`. And a gate that
        // **generates** a secret on a refused request would let an unauthenticated
        // caller flip the machine's state: `peek`, never `api_token`.
        let (face, _) = harness_management("lan-no-cast-info");
        let response = face.answer(&get_api("/api?query=cast-info", LAN));
        assert_eq!(response.status, 403);
        assert!(face.setting.peek(setting::Key::ApiToken).is_none());
        let body = text(&response.body);
        assert!(!body.contains("token="), "the refusal leaks the shape: {body}");
        assert!(!body.contains("http://127.0.0.1:"));
    }

    #[test]
    fn cast_info_mints_once_and_the_token_it_prints_opens_cast() {
        crate::pin_test_dir();
        let (face, management) = face_on_port_management("cast-info-mints", TEST_PORT);
        let first = face.answer(&get_api("/api?query=cast-info", LOCAL));
        let secret = face
            .setting
            .peek(setting::Key::ApiToken)
            .expect("the reveal route generates the secret")
            .as_str()
            .expect("a string secret")
            .to_string();
        let expected = format!(
            "http://127.0.0.1:{TEST_PORT}/api?query=cast&url=<媒体地址>&token={secret}"
        );
        assert_eq!(first.status, 200);
        assert_eq!(first.body, json_body(0, &expected));
        // The port in that address is `ApplicationPort`, not the default: this is
        // the route that hands out a URL people paste into a shortcut.
        let second = face.answer(&get_api("/api?query=cast-info", LOCAL));
        assert_eq!(first.body, second.body, "the secret must stay stable");
        let value: serde_json::Value =
            serde_json::from_slice(&first.body).expect("the answer is JSON");
        assert_eq!(value.as_object().expect("an object").len(), 2);
        assert!(value.get("data").is_none(), "no invented field");
        let with_secret = format!("{CAST_TARGET}&token={secret}");
        assert_eq!(face.answer(&get_api(&with_secret, LOCAL)).status, 200);
        assert_eq!(management.calls().len(), 1);
    }

    #[test]
    fn cast_info_prints_the_secret_that_is_stored() {
        crate::pin_test_dir();
        let (face, _) = harness_management("cast-info-stored");
        store_token(&face.setting, Some(SECRET));
        let response = face.answer(&get_api("/api?query=cast-info", LOCAL));
        assert_eq!(response.status, 200);
        assert!(text(&response.body).contains(SECRET));
    }

    #[test]
    fn an_empty_stored_secret_authenticates_nobody_and_is_replaced_on_reveal() {
        crate::pin_test_dir();
        // An empty `Api_Token` is a stored value that says nothing. It must not
        // let anyone through, and the one route that has to print a secret
        // replaces it — that is 「首次使用时生成」, not a repair.
        let (face, management) = harness_management("empty-secret");
        store_token(&face.setting, Some(""));
        let target = format!("{CAST_TARGET}&token=x");
        assert_eq!(face.answer(&get_api(&target, LOCAL)).status, 403);
        assert!(management.calls().is_empty());
        assert_eq!(face.answer(&get_api("/api?query=cast-info", LOCAL)).status, 200);
        let now = face
            .setting
            .peek(setting::Key::ApiToken)
            .expect("a secret")
            .as_str()
            .expect("a string")
            .to_string();
        assert!(!now.is_empty());
    }

    #[test]
    fn a_token_that_is_not_a_string_is_absent() {
        crate::pin_test_dir();
        let (face, management) = harness_management("non-string-secret");
        face.setting.set(setting::Key::ApiToken, serde_json::Value::from(42));
        let target = format!("{CAST_TARGET}&token=42");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 403);
        assert!(management.calls().is_empty());
    }

    #[test]
    fn the_cast_entry_names_the_parameter_it_needed() {
        crate::pin_test_dir();
        let (face, management) = harness_management("cast-no-url");
        store_token(&face.setting, Some(SECRET));
        for target in [
            format!("/api?query=cast&token={SECRET}"),
            format!("/api?query=cast&url=&token={SECRET}"),
        ] {
            let response = face.answer(&get_api(&target, LOCAL));
            assert_eq!(response.status, 200);
            assert_eq!(response.body, json_body(1, MISSING_URL), "{target}");
        }
        assert!(management.calls().is_empty());
    }

    #[test]
    fn query_values_arrive_exactly_as_typed() {
        crate::pin_test_dir();
        // Nothing on this path is percent-decoded, and that is the measured
        // behaviour of the shipped receiver. `%2Fpath.mp4` therefore is **not** an
        // address (a decoded slash would make it one), and `%20` survives
        // `.trim()` — so both land on the shape refusal, not the missing-one.
        let (face, management) = harness_management("no-decoding");
        store_token(&face.setting, Some(SECRET));
        for value in ["%2Fpath.mp4", "%20"] {
            let target = format!("/api?query=cast&url={value}&token={SECRET}");
            let response = face.answer(&get_api(&target, LOCAL));
            assert_eq!(response.status, 200);
            assert_eq!(response.body, json_body(1, &not_absolute(value)), "{value}");
        }
        assert!(management.calls().is_empty());
    }

    #[test]
    fn a_relative_or_hollow_address_is_refused_before_the_player() {
        crate::pin_test_dir();
        let (face, management) = harness_management("bad-shapes");
        store_token(&face.setting, Some(SECRET));
        for value in [
            "http://",
            "file:///tmp/x",
            "http:///absolute-empty-host",
            "://only-a-separator",
            "/relative/path.mp4",
        ] {
            let target = format!("/api?query=cast&url={value}&token={SECRET}");
            let response = face.answer(&get_api(&target, LOCAL));
            assert_eq!(response.status, 200);
            assert_eq!(response.body, json_body(1, &not_absolute(value)), "{value}");
        }
        assert!(management.calls().is_empty());
    }

    #[test]
    fn an_unusual_scheme_is_the_players_business_not_ours() {
        crate::pin_test_dir();
        // No scheme whitelist (our own decision, stated in the code): the entry
        // exists for callers that can only open a URL, and the renderer already
        // knows what it can play.
        let (face, management) = harness_management("rtsp-scheme");
        store_token(&face.setting, Some(SECRET));
        let target = format!("/api?query=cast&url=rtsp://1.2.3.4/live&token={SECRET}");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 200);
        assert!(response.body.starts_with(OK_DOC.as_bytes()));
        assert_eq!(
            management.calls(),
            vec![("rtsp://1.2.3.4/live".to_string(), None)]
        );
    }

    #[test]
    fn the_name_travels_with_the_address() {
        crate::pin_test_dir();
        // AGENTS.md §4.2: the route that changes the player's address must change
        // its name too. `cast-title` is the sender's own name for the media; an
        // empty one means "no opinion", not "call it nothing".
        let (face, management) = harness_management("cast-title");
        store_token(&face.setting, Some(SECRET));
        let named = format!("{CAST_TARGET}&cast-title=名称&token={SECRET}");
        assert_eq!(face.answer(&get_api(&named, LOCAL)).status, 200);
        assert_eq!(
            management.calls()[0].1,
            Some("名称".to_string()),
            "the title the caller gave"
        );
        let unnamed = format!("{CAST_TARGET}&cast-title=&token={SECRET}");
        assert_eq!(face.answer(&get_api(&unnamed, LOCAL)).status, 200);
        assert_eq!(management.calls()[1].1, None);
    }

    #[test]
    fn the_players_refusal_is_handed_back_verbatim() {
        crate::pin_test_dir();
        // A second apology from this port ("投屏失败") would hide the reason the
        // shell gave. The document shape changes (code 1), the sentence does not.
        let fake = Arc::new(FakeManagement::refusing());
        let face = face_built(
            fresh_setting("shell-refusal"),
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            fake.clone(),
        );
        store_token(&face.setting, Some(SECRET));
        let target = format!("{CAST_TARGET}&token={SECRET}");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, json_body(1, "播放器拒绝了这次投屏"));
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn api_without_a_query_names_the_missing_parameter() {
        crate::pin_test_dir();
        let (face, management) = harness_management("no-query");
        for target in ["/api", "/api/", "/api?query=", "/api?query=&token=whatever"] {
            let response = face.answer(&get_api(target, LOCAL));
            assert_eq!(response.status, 200, "{target}");
            assert_eq!(response.body, json_body(1, NO_QUERY), "{target}");
        }
        assert!(management.calls().is_empty());
        // `/api/` is the same call as `/api` (the shipped `GET` binds `key=''`),
        // so the segments must say so — `/api/extra` below is a different one.
        assert_eq!(get_api("/api/", LOCAL).segments, vec!["api".to_string()]);
    }

    #[test]
    fn an_unknown_query_is_answered_before_the_gate() {
        crate::pin_test_dir();
        // Ours, and it is a trade: hiding which names we know is worth less to a
        // caller than fixing their typo. The value comes back inside a JSON
        // string, never inside markup.
        let (face, _) = harness_management("unknown-query");
        let response = face.answer(&get_api("/api?query=castt", LAN));
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body,
            json_body(1, "没有可执行的操作：query=castt 不是这个接口认识的查询。")
        );
    }

    #[test]
    fn the_echo_is_cut_in_characters_not_bytes() {
        crate::pin_test_dir();
        // Byte-slicing a reflection would panic on a UTF-8 boundary, and a
        // Chinese-named shortcut is a normal request here.
        let (face, _) = harness_management("echo-cut");
        let ascii = format!("/api?query={}", "x".repeat(150));
        let body = text(&face.answer(&get_api(&ascii, LAN)).body);
        assert_eq!(body.matches('x').count(), QUERY_ECHO_MAX);
        let cjk = format!("/api?query={}", "投".repeat(150));
        let body = text(&face.answer(&get_api(&cjk, LAN)).body);
        assert_eq!(
            body.matches("\\u6295").count(),
            QUERY_ECHO_MAX,
            "ensure_ascii escapes each character; the count is the cut"
        );
    }

    #[test]
    fn an_extra_path_segment_redirects_instead_of_answering() {
        crate::pin_test_dir();
        // `/api/extra` arrives to the shipped `GET` as `key='extra'`: unknown, and
        // it ends in the redirect. This port takes the route name from `query=`,
        // so without that guard it would answer as if the extra were never typed.
        let face = face_on_port("api-extra-redirect", TEST_PORT);
        let response = face.answer(&get_api("/api/extra?query=cast-info", LOCAL));
        assert_eq!(response.status, 303);
        let location = format!("http://127.0.0.1:{TEST_PORT}/");
        assert_eq!(
            response.variant,
            vec![("Location".to_string(), location.clone())]
        );
        assert_eq!(
            text(&response.body),
            format!("This resource can be found at <a href=\"{location}\">{location}</a>.")
        );
        assert!(face.setting.peek(setting::Key::ApiToken).is_none());
    }

    #[test]
    fn head_admits_itself_to_the_route_and_withholds_the_body() {
        crate::pin_test_dir();
        // `HEAD` is exempt from the 411 check and shares the GET dispatch, so it
        // reaches `cast-info`; `answer` withholds the body in one place.
        let (face, _) = harness_management("head-cast-info");
        let mut req = request("HEAD", "/api?query=cast-info", &[]);
        req.remote_ip = LOCAL.to_string();
        let response = face.answer(&req);
        assert_eq!(response.status, 200);
        assert!(response.omit_body);
        assert!(!response.body.is_empty(), "the length is still advertised");
    }

    #[test]
    fn a_real_loopback_socket_is_the_peer_the_gate_reads() {
        crate::pin_test_dir();
        let (face, _) = harness_management("socket-loopback");
        let server = HttpServer::new(face);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut client = TcpStream::connect(addr).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        client
            .write_all(b"GET /api?query=cast-info HTTP/1.1\r\n\r\n")
            .expect("write request");
        let reader = std::thread::spawn(move || {
            let mut client = client;
            let mut bytes = Vec::new();
            let _ = client.read_to_end(&mut bytes);
            bytes
        });
        server.serve_stream(stream);
        let rendered = text(&reader.join().expect("reader joins"));
        assert!(rendered.starts_with("HTTP/1.1 200 OK\r\n"), "{rendered}");
        assert!(rendered.contains("token="), "the entry URL carries it");
        // The same route, from a peer whose address could not be read, stays shut
        // and stays un-minted: `''` is not "this is us".
        let (face, _) = harness_management("socket-unknown");
        let response = face.answer(&get_api("/api?query=cast-info", ""));
        assert_eq!(response.status, 403);
        assert!(face.setting.peek(setting::Key::ApiToken).is_none());
    }

    #[test]
    fn the_peer_address_is_read_without_its_port() {
        crate::pin_test_dir();
        // `is_loopback` parses an `IpAddr`; `127.0.0.1:52344` parses as nothing,
        // so the port has to come off before the question is asked.
        assert!(is_loopback("::1"));
        assert!(!is_loopback(""));
        assert!(!is_loopback("9.9.9.9"));
        assert!(!is_loopback("127.0.0.1:52344"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _client = TcpStream::connect(addr).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        let read = peer_ip(&stream);
        assert!(!read.contains(':'), "no port survives: {read}");
        assert!(is_loopback(&read));
    }

    #[test]
    fn the_get_route_table_is_well_formed() {
        crate::pin_test_dir();
        // The gate is a **string** in the table on purpose, so the table itself
        // needs asking: an empty scan is a scan that agrees with anything.
        assert!(GET_MANAGEMENT_ROUTES.len() >= 2);
        let mut names: Vec<&str> = GET_MANAGEMENT_ROUTES.iter().map(|r| r.query).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), GET_MANAGEMENT_ROUTES.len());
        for route in GET_MANAGEMENT_ROUTES.iter() {
            assert!(!route.query.is_empty());
            assert!(
                Gate::parse(route.gate).is_some(),
                "{} carries a gate name this port cannot read: {:?}",
                route.query,
                route.gate
            );
        }
        assert_eq!(
            GET_MANAGEMENT_ROUTES
                .iter()
                .filter(|r| r.action == Action::Cast)
                .count(),
            1
        );
        assert_eq!(
            GET_MANAGEMENT_ROUTES
                .iter()
                .filter(|r| r.action == Action::CastInfo)
                .count(),
            1
        );
    }

    #[test]
    fn a_gate_name_this_port_does_not_know_refuses() {
        crate::pin_test_dir();
        // Fail closed, asserted through the **response** (the `Face` log has no
        // injection seam, and no test in this file reads log lines): the same
        // request that the real `"token"` gate lets through is refused for a
        // name the table does not have.
        let (face, _) = harness_management("unknown-gate");
        store_token(&face.setting, Some(SECRET));
        let target = format!("{CAST_TARGET}&token={SECRET}");
        let req = get_api(&target, LOCAL);
        assert!(face.gate_refusal("token", &req).is_none());
        let refusal = face.gate_refusal("nonsense", &req).expect("refused");
        assert_eq!(refusal.status, 403);
        assert_eq!(refusal.body, json_body(1, MANAGEMENT_REFUSAL));
        assert!(Gate::parse("Management").is_none(), "exact match");
        assert!(Gate::parse("management ").is_none());
    }

    #[test]
    fn the_token_compare_is_constant_time_and_exact() {
        crate::pin_test_dir();
        assert!(token::secret_eq("", ""));
        assert!(token::secret_eq("a", "a"));
        assert!(!token::secret_eq("a", "b"));
        assert!(!token::secret_eq("abc", "abcd"));
        assert!(!token::secret_eq("abcd", "abc"));
        assert!(token::secret_eq("没", "没"));
        assert!(!token::secret_eq("没", "啊"));
    }

    #[test]
    fn a_repeated_query_parameter_keeps_its_first_value() {
        crate::pin_test_dir();
        // First pair wins, like the shipped parser. If `cast-info` won instead,
        // this loopback request would be served (200) rather than refused.
        let (face, management) = harness_management("repeated-query");
        store_token(&face.setting, Some(SECRET));
        let target = format!("/api?query=cast&query=cast-info&url=http://host/a.mp4&token=");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 403);
        assert_eq!(response.body, json_body(1, TOKEN_REFUSAL));
        assert!(management.calls().is_empty());
    }
}
