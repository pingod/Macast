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
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use macast_core::logging::Logger;
use macast_core::paths;
use macast_core::setting::{self, Key, Setting};
use macast_core::timefmt;
use macast_core::token;
// The query-string reader moved to `macast_core::url` on 2026-10-18, when the
// Chromecast setup face (`macast-cast/src/setup.rs`, `?params=a,b`) became its
// second reader. Same reason as everything else in that module: `macast-cast`
// depends on `macast-upnp` and never the reverse, so a rule both faces must
// agree on cannot be private to either — and the rule that would have drifted
// first is whether `%2B` is a `+` or a space.
use macast_core::url::{parse_query, split_once_ascii};

use crate::store::SharedStore;

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

/// `POST /api` routes that are wired in this port, keyed by the action field
/// they recognise (`protocol.py:2308-2336`, `POST_ROUTES`).
///
/// **Gate is management for resolve endpoints**: the URLs only ever travel to
/// `yt-dlp`/`ffprobe` as argv arguments — they do not become shell commands or
/// importable code, so these routes do NOT appear in `_CODE_EXECUTION_PARAMS`.
///
/// The table shape mirrors `protocol.py:2308-2336` one-for-one because "any
/// endpoint not in the table falls through" (§4.7c). Precedence is position:
/// the first matching field wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PostAction {
    ResolvePage,
    CastResolved,
    CastUrl,
    /// `resolve-cookies=<jar text>`: validate and save the whole jar.
    SaveCookies,
    /// `resolve-cookies-clear=`: drop the saved jar.
    ClearCookies,
    /// `resolve-cookie-browser=<spec>`: set or clear the browser reading.
    SetCookieBrowser,
}

struct PostRoute {
    field:  &'static str,
    gate:   &'static str,
    action: PostAction,
}

/// The POST routes this port implements, in the shipped order.
///
/// `cast-uri` (the original direct-URL entry, ported for backwards compat) is
/// kept at `GATE_MANAGEMENT`: same reasoning as `query=cast`, but POST removes
/// the CSRF vector that makes GET need the token.
///
/// **Its position is load-bearing, not cosmetic.** Precedence is positional
/// (`protocol.py:2310` puts it ahead of the two resolve routes at 2317/2318), so
/// a request that carries both `cast-uri` and `resolve-page` runs `cast-uri` and
/// never reaches the resolve job. Put `resolve-page` first and the same request
/// silently starts a parse instead of casting -- and nothing else changes.
const POST_MANAGEMENT_ROUTES: &[PostRoute] = &[
    PostRoute { field: "cast-uri",        gate: "management", action: PostAction::CastUrl },
    PostRoute { field: "resolve-page",    gate: "management", action: PostAction::ResolvePage },
    PostRoute { field: "cast-resolved",   gate: "management", action: PostAction::CastResolved },
    // The three cookie routes, `protocol.py:2325-2327`, straight after the two
    // resolve routes they belong to. Management-gated and **not** `GATE_CODE`,
    // which is the distinction worth keeping in mind when reading this: the jar
    // is data handed to yt-dlp as a *file path this code chose*, and the browser
    // value goes through `cookie_browser_spec` before it reaches argv. Neither
    // request hands us something that runs (AGENTS.md §4.7b's family is the one
    // where it does).
    PostRoute { field: "resolve-cookies",       gate: "management", action: PostAction::SaveCookies },
    PostRoute { field: "resolve-cookies-clear", gate: "management", action: PostAction::ClearCookies },
    PostRoute { field: "resolve-cookie-browser", gate: "management", action: PostAction::SetCookieBrowser },
];

/// `site_of()` (`protocol.py:1491`): `(host, port)` out of a URL or `Host`
/// value, `None` if unusable.
///
/// One normaliser for both sides of the same-site comparison, because the two
/// spellings differ in exactly the ways that would otherwise silently mismatch:
/// `Origin` is an absolute URL with no path, `Referer` has a path, and `Host`
/// carries neither scheme nor path but does carry the port. Lowercased, a
/// trailing dot dropped, a default 80/443 dropped so `http://host` and `host`
/// agree, and IPv6 **without** its brackets (`[::1]` and `::1` are the same
/// site).
fn site_of(target: &str) -> Option<(String, Option<u16>)> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    // A bare `Host` value has no scheme, so nothing tells us the port is 80.
    let with_scheme = if target.contains("://") {
        target.to_string()
    } else {
        format!("http://{target}")
    };
    let after_scheme = with_scheme
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    // The authority ends at the first `/`, `?` or `#`.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    // `_userinfo` / `_hostinfo` both split on the **last** `@`, and a host name
    // never contains one.
    let authority = authority.rsplit('@').next().unwrap_or("");
    // `urlsplit` raises for a netloc holding exactly one of `[` / `]`, which
    // Python reads as "unusable".
    if authority.contains('[') != authority.contains(']') {
        return None;
    }
    let (host, port_str) = match authority.find('[') {
        Some(start) => {
            // `_check_bracketed_netloc`: no data before the opening bracket, and
            // none between the closing bracket and the port delimiter.
            if start != 0 {
                return None;
            }
            let bracketed = &authority[start + 1..];
            let end = bracketed.find(']')?;
            let inner = &bracketed[..end];
            let after = &bracketed[end + 1..];
            if !after.is_empty() && !after.starts_with(':') {
                return None;
            }
            // `_check_bracketed_host` asks `ipaddress` about the bracketed text,
            // so an IPv4 in brackets and a name in brackets are both refused.
            // The two directions are not equal here: refusing a bracketed IPv4
            // is the one that matters, because `127.0.0.1` **is** one of the
            // page's own names and a bracket around it must not buy it a pass.
            // An IPv4-**mapped** IPv6 (`[::ffff:127.0.0.1]`) does carry a colon
            // and Python does accept it, so it is not caught here.
            if inner.parse::<std::net::Ipv4Addr>().is_ok()
                || (!inner.contains(':') && !inner.starts_with('v'))
            {
                return None;
            }
            (
                inner.to_string(),
                after.strip_prefix(':').map(|s| s.to_string()),
            )
        }
        // `_hostinfo()` partitions on the **first** colon, so `evil.test:abc`
        // has host `evil.test` and an unusable port -- not host `evil.test:abc`
        // with no port. An empty tail (`evil.test:`) is "no port".
        None => match authority.split_once(':') {
            Some((head, tail)) => (
                head.to_string(),
                if tail.is_empty() {
                    None
                } else {
                    Some(tail.to_string())
                },
            ),
            None => (authority.to_string(), None),
        },
    };
    if host.is_empty() {
        return None;
    }
    let port = match port_str {
        Some(p) => {
            // `.port` raises unless the text `isdigit() and isascii()`, which
            // rules out the `+80` that a numeric parse would happily take.
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            match p.parse::<u16>() {
                Ok(v) if v != 80 && v != 443 => Some(v),
                Ok(_) => None,
                // Past 65535 Python raises "Port out of range".
                Err(_) => return None,
            }
        }
        None => None,
    };
    Some((host.to_ascii_lowercase().trim_end_matches('.').to_string(), port))
}

/// `_page_host_names()` (`protocol.py:1524`): the host names the settings page
/// may legitimately be opened on.
///
/// The comparison is **not** "does this resolve to us" — under DNS rebinding the
/// attacker's own domain *does* resolve to `127.0.0.1`, and that is the whole
/// attack. So the list is the names this machine answers on regardless of what
/// anyone's resolver says: loopback, the addresses we advertise on the LAN, and
/// this host's own names.
fn page_host_names(setting: &Setting) -> Vec<String> {
    let mut names: Vec<String> = vec![
        "127.0.0.1".into(),
        "localhost".into(),
        "::1".into(),
    ];
    // The mDNS-side list. It already falls back to the unfiltered DLNA address
    // set, so one call covers both of Python's two `update()`s.
    names.extend(macast_core::net::advertised_addresses(setting));
    if let Ok(name) = std::str::from_utf8(paths::hostname().as_bytes()) {
        let host = name.to_ascii_lowercase();
        let host = host.trim_end_matches('.');
        if !host.is_empty() {
            names.push(host.to_string());
            names.push(host.split('.').next().unwrap_or(host).to_string());
            if !host.ends_with(".local") {
                names.push(format!("{host}.local"));
            }
        }
    }
    names.retain(|n| !n.is_empty());
    names
}

/// The refusal `_same_site()` raises (`protocol.py:1669`). It must say **both**
/// where the caller came from and which address to use instead: a 403 that
/// reads like a broken token sends the user to the wrong fix.
fn cross_site_refusal(origin: &str, port: u16) -> String {
    let host = match site_of(origin) {
        Some((host, _)) => host,
        None => return "无法识别的来源站点".to_string(),
    };
    format!(
        "禁止跨站操作：这个请求来自「{host}」，而设置页只服务本机自己。\
         如果你是从别的网址打开的页面，请改用 http://127.0.0.1:{port}/ 再试。"
    )
}

/// `_same_site()` (`protocol.py:1641`): the **first** question every POST is
/// asked, before the gate its route names (§4.7b, §4.7c).
///
/// * `Ok(None)` — no `Origin` and no `Referer`. That is **not a browser**: curl,
///   a phone Shortcut, Home Assistant. Those callers stay judged by address and
///   token alone, which is the gate they were already subject to.
/// * `Ok(Some(()))` — the name in the header is one of ours.
/// * `Err(sentence)` — cross-site, or a header we cannot read. Either way the
///   caller answers with this and does not run the route.
///
/// **The port is deliberately not compared.** A page we serve on one port
/// posting to another port of the same host is the same site for our purposes
/// (there is no cookie here to ride), and pinning it would break the port
/// fallback Macast itself does when 58880 is taken (§4.7b). `Origin` also wins
/// over `Referer`: a page can choose its own `Referer`.
fn same_site(req: &Request, setting: &Setting) -> Result<Option<()>, String> {
    let origin = match req.header("Origin").or_else(|| req.header("Referer")) {
        Some(v) => v,
        None => return Ok(None),
    };
    let Some((host, _)) = site_of(&origin) else {
        return Err(cross_site_refusal(&origin, setting::application_port(setting)));
    };
    if page_host_names(setting).iter().any(|n| *n == host) {
        return Ok(Some(()));
    }
    Logger::named("Protocol").warning(&format!(
        "blocked a cross-site API post from origin {origin}"
    ));
    Err(cross_site_refusal(&origin, setting::application_port(setting)))
}

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
    /// `query=resolve-status&job=<id>`: how far one background parse has got.
    ResolveStatus,
    /// `query=resolve-cookies`: the cookie panel's read — counts and names only.
    ResolveCookies,
    /// `query=cast-targets&refresh=`: the 网页地址投屏 card's device list.
    CastTargets,
    /// `query=status`: the settings page's whole status document — server identity,
    /// network picker facts, playback state, event clients, play history.
    Status,
    /// `query=interfaces`: the three facts behind the network picker, on their own.
    Interfaces,
}

/// The seven answers this port implements out of the shipped management API's
/// fifteen GET queries. Each is a **cumulative** check, not a choice: see
/// [`Face::gate_refusal`].
///
/// **Two deviations from Python's *structure*, both recorded rather than copied
/// silently** (`protocol.py:2066-2120`). First, Python asks the cast question
/// *before* it consults the sensitive-query list, and that arm only requires
/// `_token_present()` — so a LAN caller with no token hears the **cast** sentence.
/// [`Face::gate_refusal`] is cumulative with the management question first, which
/// is Python's own POST shape (`_gate_refusal`, `protocol.py:2594`), so the same
/// caller hears the **management** sentence here: same verdict, different words.
/// Second, Python gates `resolve-status` **inside** one combined membership test
/// with eleven other sensitive queries (`status`, `log`, `log-download`,
/// `log-modules`, `launch-param`, `interfaces`, `subscribers`, `module-settings`,
/// `cast-info`, `resolve-status`, `resolve-cookies`, `cast-targets`) under a
/// single `_management_allowed()` call — it is *not* per-route data the way the
/// POST table is. Here it is a row with its own gate, which is the shape AGENTS.md
/// §4.7c asks for; the consequence is that the *next* sensitive query must be added
/// **here**, not to a list elsewhere, or it answers a LAN caller.
const GET_MANAGEMENT_ROUTES: [GetRoute; 7] = [
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
    GetRoute {
        query: "resolve-status",
        // `GATE_MANAGEMENT`, and this one is *not* a token gate even though it
        // sits next to `cast` in the same face. Its answer carries the
        // candidate list, and every row of that list holds the site's own
        // signed, short-lived, header-requiring address (`describe()` writes
        // `url`). A LAN passer-by polling this would be harvesting those. The
        // relay is what keeps them away from the *television*; this gate is
        // what keeps them away from everyone else on this network.
        gate: "management",
        action: Action::ResolveStatus,
    },
    GetRoute {
        query: "resolve-cookies",
        // `GATE_MANAGEMENT`, and the reason is the `path` key. The answer is
        // built to carry **counts, never values** (`_cookie_state` in
        // `protocol.py:2794-2818`), but it does name the file on disk and the
        // browser spec this machine is configured to read. That is a map of
        // where this user's session cookies live, which is not something a LAN
        // passer-by gets to ask for. AGENTS.md §4.7 is blunt about the general
        // shape: this face being management-gated is not a licence to hand out
        // a second secret in the same response.
        gate: "management",
        action: Action::ResolveCookies,
    },
    GetRoute {
        query: "cast-targets",
        // `GATE_MANAGEMENT` in Python, and here it is a row like every other
        // query rather than an arm of an `if`. Its answer is a device list, so
        // on the face of it there is nothing secret — but the shipped gate is
        // the shipped behaviour, and a route that answers LAN callers while its
        // neighbours refuse them is the exact structural hole AGENTS.md §4.7c
        // was written to close.
        gate: "management",
        action: Action::CastTargets,
    },
    GetRoute {
        query: "status",
        // Management-gated in the reference, and it is the *most* informative
        // document this face writes: it prints the friendly name, every address
        // this machine publishes, which interface is pinned, the raw stored
        // protocol list, what is playing, and who is subscribed to our events.
        // None of those is a secret this port hands out on purpose — and one of
        // them is a live credential path: the `history` rows are the addresses the
        // user has cast, which for a signed site link is exactly the thing
        // `resolve-status` is gated over (see the comment on that row).
        gate: "management",
        action: Action::Status,
    },
    GetRoute {
        query: "interfaces",
        // Same facts as `status`'s `network` block, and the same gate — in Python
        // they are gated by the same membership test (`protocol.py:2080-2088`), so
        // a port that made this one public while `status` stays gated would leak the
        // machine's interface table through the door nobody thought to close.
        gate: "management",
        action: Action::Interfaces,
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

    /// The published subscriber table, as rows — the read `query=status` builds
    /// its `clients` list from.
    ///
    /// It is a **read of the table's own snapshot**, not a re-derivation: the
    /// owner (`crate::eventing`) decides what a row is and takes each lease exactly
    /// once, and this file decides which of the row's fields the page sees and under
    /// what names. Queued-but-unpublished subscriptions are deliberately absent
    /// here too; `query=subscribers` is where `pending_append` lives, and the two
    /// documents answer different questions on purpose (§4.2's 「空结果要说清是哪一种空」).
    fn rows(&self) -> Vec<crate::eventing::SubscriberRow>;
}

/// One management answer, in the order the shipped handler builds it.
///
/// Python returns a **dict**, and a dict there remembers insertion order --
/// `json.dumps` therefore prints the keys in the order the route wrote them.
/// That order is what a caller reads, so it is not cosmetic:
/// `_post_cast_resolved` reaches its success document through
/// `_cast_url` (`code, message, url, title`) and then *assigns* `relay` and
/// `note` and overwrites `message` **in place**, which keeps `message` in its
/// original slot (`protocol.py:2935-2948`). A `HashMap` would answer the same
/// six facts in a different order and every byte-level diff test would be a
/// coin-flip.
///
/// So: `code` first, `message` second (always -- that is true of every shape in
/// `protocol.py` that returns extras), then whatever this route added, in the
/// order it added it. `fields` is a `Vec` and not a map precisely so that
/// "in the order it added it" is a property of the type rather than of a
/// feature flag.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub code:    i64,
    pub message: String,
    pub fields:  Vec<(String, Value)>,
}

impl Answer {
    /// `code: 0` -- the thing happened, and this is the sentence for it.
    pub fn ok(message: impl Into<String>) -> Answer {
        Answer { code: 0, message: message.into(), fields: Vec::new() }
    }

    /// `code: 1` -- nothing happened. The status line is a separate question
    /// (see [`json_answer`]); a refusal is *this* plus a 403, and a request the
    /// dispatcher did not recognise is this plus a 200.
    pub fn fail(message: impl Into<String>) -> Answer {
        Answer { code: 1, message: message.into(), fields: Vec::new() }
    }

    /// Add one key after the ones already there. Chain in the order Python
    /// writes them, not in alphabetical order.
    pub fn with(mut self, name: &str, value: Value) -> Answer {
        self.fields.push((name.to_string(), value));
        self
    }

    /// A gate refusal: **`403` in the body's `code`**, which this port sends with
    /// a **200** status line.
    ///
    /// That pairing is the shipped handler's, not a mistake of ours, and it is
    /// **both** verbs: every POST answer — the same-site refusal, a file route's
    /// gate refusal, a route's gate refusal — goes out through `return _body({'code':
    /// 403, 'message': refusal})`, and `_body` is `json.dumps(res, indent=4).encode()`
    /// inside `POST()`, which never sets a status (`protocol.py:2518`, `:2530`,
    /// `:2539`). The three GET refusals are written the same way — `json.dumps({'code':
    /// 403, …}, indent=4).encode()` with no status set either (`:2082`, `:2092`,
    /// `:2104`) — so in Python a browser that fires an unauthorised GET also reads
    /// **HTTP 200 with a body that says 403**.
    ///
    /// **This port does not follow it on GET**, and that is our deviation, not
    /// Python's asymmetry: [`Face::gate_refusal`] answers 403 in the *status* with
    /// `code: 1` and the Chinese sentence. Nine tests assert it, and the frontend
    /// this port serves reads the status. `post()` therefore uses this shape where
    /// Python does, and `api()` deliberately does not — the difference is documented
    /// at each site rather than smoothed over.
    pub fn denied(message: impl Into<String>) -> Answer {
        Answer { code: 403, message: message.into(), fields: Vec::new() }
    }
}

/// The management seam: the things `GET /api` and `POST /api` can ask of the
/// player and of the resolver, and the only way this port reaches anything that
/// *does* something.
///
/// It returns [`Answer`]s, not a `serde_json::Value` it wants printed, because
/// the JSON shape is this file's business (the 4-space indent, the
/// `ensure_ascii` quoting, which key comes first) while the *content* belongs to
/// whoever drives the player. An `Answer` is therefore a **message plus the
/// extra keys that route puts in its own dict** -- never a status code.
///
/// `cast_url` is the one method that answers with a `Result` instead of a whole
/// `Answer`, and the difference is Python's: `_cast_url` returns either
/// `{'code': 0, 'message': 'success', 'url':…, 'title':…}` or
/// `{'code': 1, 'message': …}` and **nothing else** on the failure side, so a
/// sentence is the entire failure. The eight methods below are not like that --
/// their refusals carry keys (`'job': ''`, `'status': None`, `'targets': {…}`),
/// so they hand back a full `Answer` and this file never has to guess at their
/// extras. **`'targets'` on a refusal is the load-bearing one**: a page that reads
/// `targets.items` before it reads `code` gets `undefined` rather than an empty
/// list if the refusal is built as a bare sentence, and the card then spins
/// forever on a list that was never going to arrive.
///
/// **Why the extra methods are `Answer`-shaped and not `String`** — the shipped
/// key orders, quoted so a change here is a change against a witness:
///
/// * `_resolve_page` success (`protocol.py:2866-2867`):
///   `{'code': 0, 'message': 'success', 'job': job_id, 'url': url, 'note': note}`
///   and its full answer `{'code': 1, 'message': '已经有 {} 个解析在跑，请等其中一个结束'}`.
/// * `_resolve_status` not-found (`:2893-2896`): `{'code': 1, 'message':
///   '没有这个解析任务（超时或已重启）', 'job': '', 'status': None}`; found
///   (`:2898-2899`): `{'code': 0, 'message': 'success', 'job': job_id, 'status':
///   job.status()}`. Note `job` is the **empty string** in the first and the
///   **id that was asked for** in the second, and `status` is `null` versus a
///   seventeen-key object — the page reads both.
/// * `_post_cast_resolved` (`:2904+`) refuses a missing job with
///   `{'code': 1, 'message': '解析任务已经不在了，请重新解析'}`.
/// * `query=resolve-cookies` (`:2183-2191`): `{'code': 0, 'message': 'success',
///   'state': self._cookie_state()}`, and `_cookie_state` (`:2794-2818`) writes
///   **eight keys in this order** — `jar`, `cookies`, `bad`, `path`, `browser`,
///   `browsers`, `max_bytes`, `yt_dlp`. `browsers` is `list(COOKIE_BROWSERS)`, so
///   the array order is that tuple's alphabetical order, and `yt_dlp` is a
///   **boolean about this machine**, not about the request.
/// * `query=cast-targets` (`:2192-2199` → `:1765-1794`): success
///   `{'code': 0, 'message': 'success', 'targets': state}`; both refusals —
///   「应用还在启动，请一秒后再试」/「没有可用的设备发现插件…」 (`:1754-1763`) and
///   「读取设备列表失败：{e}」 — are the same two-line dict **plus**
///   `'targets': {'items': [], 'searching': False, 'known': False}`. `state['items']`
///   is `[self._local_target()] + 插件的 items` (`:1793`), and `_local_target()`
///   (`:1796-1807`) is `{'id': 'local', 'kind': 'local', 'name': …, 'address': ''}`.
/// * `_post_resolve_cookies` (`:2986-3030`) success
///   `{'code': 0, 'message': '已保存 {} 条 cookie{}', 'cookies': cookies, 'bad': bad}`
///   — the two counts are **after** the message, and the message is the only place
///   the suffix 「，另有 {} 行看不懂（一并存了）」 appears. Every failure is
///   `{'code': 1, 'message': …}` with **no extras**.
/// * `_post_resolve_cookies_clear` (`:3032-3043`) and `_post_resolve_cookie_browser`
///   (`:3045-3081`) each answer exactly `{'code': 0|1, 'message': …}`: nothing else
///   in those two dicts, which is why they are `Answer`s with no fields rather than
///   `String`s — the shape still has to be written somewhere, and writing it in the
///   face keeps one owner for the whole table.
pub trait Management: Send + Sync {
    /// Cast `url`, named `title` when the sender gave one (`cast-title`).
    ///
    /// `Ok` is the **whole success document** — `_cast_url`'s dict has the two
    /// extras (`'url'`, `'title'`) and both callers write it whole, so the shell
    /// that changed what is playing is the only one that can say what it played
    /// (see [`Face::cast_url_answer`]). `Err` stays a bare **sentence**, because
    /// that is all Python's failure dict carries.
    fn cast_url(&self, url: &str, title: Option<&str>) -> Result<Answer, String>;

    /// Start a background parse of the page (or share text) in `text`.
    ///
    /// The implementer owns everything the HTTP face must not decide: whether
    /// `text` is an address at all (`looks_like_url` / `resolve_target`), the
    /// random job id, the slot accounting (`RESOLVE_JOBS_MAX`, the TTL sweep,
    /// which finished job gets evicted), and the `note` sentence that goes back
    /// with it. `macast-upnp` cannot import `macast-cast` — the dependency runs
    /// the other way — so this call is the only shape between them.
    fn resolve_page(&self, text: &str) -> Answer;

    /// The `query=resolve-status` answer for one job id, including the "there is
    /// no such job" document. Empty `job_id` is a legitimate question: Python
    /// looks up `kwargs.get('job')`, which is `None` for a poll typed without it.
    fn resolve_status(&self, job_id: &str) -> Answer;

    /// Relay one candidate of a finished parse and hand the relay address to a
    /// device. `candidate` stays a **raw string**: which candidate index means
    /// what, and what to say when it names nothing, is decided where the job's
    /// candidate list lives (`'没有这个候选项'`), and this file must not become a
    /// second owner of that answer. `target` empty means "this machine", the
    /// only answer v0.18 had.
    fn cast_resolved(&self, job: &str, candidate: &str, target: &str) -> Answer;

    /// The `query=resolve-cookies` panel read: **counts and names, never values**.
    ///
    /// The eight-key document is the implementer's, because the facts in it live
    /// where the jar lives: whether `SETTING_DIR/resolve_cookies.txt` is on disk,
    /// how many lines `cookie_jar_state` can read, which browser spec this machine
    /// is configured to read, and whether *this* machine has `yt-dlp` installed at
    /// all. `macast-upnp` cannot ask any of those, and must not learn the path.
    ///
    /// **What this route must never put in the answer**: a cookie value, a line of
    /// the jar, or anything from `macast_setting.json` beyond the browser *name*.
    /// AGENTS.md §4.14 fixes that as a red line -- the settings file is something
    /// users export and paste into bug reports, and a jar is a live session
    /// credential for someone else's site. `path` is allowed precisely because it
    /// is where the user would look to check our count, not what they would send us.
    fn cookie_state(&self) -> Answer;

    /// The `query=cast-targets` device list, from the caches only.
    ///
    /// `refresh` has already been read off the wire by the face: Python's rule
    /// (`protocol.py:1776-1777`) is that the flag arrives as query-string **text**, so truth is
    /// a name (`'1'`/`'true'`/`'yes'`/`'on'`, stripped and lowered) and
    /// `?refresh=0` means "do not search". That normalization is the face's business
    /// -- the same place that reads `job` and `candidate` -- so the implementer gets
    /// a decision rather than a string it has to second-guess.
    ///
    /// `true` means "start the plugin's background search and answer with whatever
    /// the caches hold **right now**"; it never means "block until a probe finishes".
    /// Python's reason (`:1771-1777`) is worth keeping: a synchronous SSDP+mDNS
    /// round trip would hold a request worker for about three seconds to say
    /// nothing the next poll would not say anyway.
    ///
    /// The implementer also owns the **first row**: `local` means "hand it to
    /// whichever renderer is selected", and its `name` is that renderer's title --
    /// 「本机」 is not one thing, so the card has to say which machine pressing 投屏
    /// will actually reach. Both refusal documents carry an empty `targets` (see the
    /// trait doc); a shell with no device-discovery face says so in words rather
    /// than inventing devices.
    fn cast_targets(&self, refresh: bool) -> Answer;

    /// Validate `text` as a whole Netscape jar and make it the saved one.
    ///
    /// All of the deciding is the implementer's -- `cookie_jar_state`'s
    /// `(cookies, bad)` and the `ValueError` sentence, the 0600 write to a temporary
    /// name and the atomic replace, and `os.makedirs` of the settings directory --
    /// because the **file** is the product here, not the answer. Two rules travel
    /// with it (§4.14): validation happens **before** the write, since a malformed
    /// jar is not an error to yt-dlp (it resolves anonymously and returns *fewer*
    /// addresses, which reads as "this page has no video"); and no backup of the
    /// previous jar is kept, deliberately -- a feature that overwrites a credential
    /// cache should not leave copies of every session it ever held.
    ///
    /// The message is built from the counts, so the counts have to come back in the
    /// same document (`'cookies'`, `'bad'`): the card re-reads them from the answer
    /// rather than polling `resolve-cookies` again.
    fn save_cookies(&self, text: &str) -> Answer;

    /// Drop the saved jar. Three distinct answers, all `code: 0` or all refusals:
    /// there was nothing to remove (「本机没有保存过 cookie 文件」 — a *success*, because
    /// the caller's goal is "no jar"), removal failed (an `OSError`, named), and the
    /// one that matters for the panel — after clearing, the next parse reads from
    /// the browser if one is configured, and that sentence must say **which**.
    fn clear_cookies(&self) -> Answer;

    /// Set or clear `Resolve_Cookie_Browser` from `text`, one raw field.
    ///
    /// `text` is the whole field, not a browser name: the shipped grammar is
    /// `BROWSER[:profile]`, and the two sides of that colon are decided
    /// differently — the name half is lowercased and must be in the whitelist,
    /// the profile half is the user's path and is **kept byte-for-byte**. So this
    /// method cannot take a `&str` name; the whitelist re-check and the rejection
    /// of `+keyring` / `::container` / a leading `-` all belong to the reader
    /// (`cookie_browser_spec`), and a face that split the string first would
    /// become a second owner of that grammar (AGENTS.md §4.14: the write gate and
    /// the read gate have to agree on one table).
    ///
    /// Six spellings mean "off" (`('', '0', 'off', 'none', '关闭', '不用')` on the
    /// lowercased text, `:3058`) and unset the key rather than storing `false`
    /// — §4.2's rule that a disabled setting is *absent*, not `False`.
    fn set_cookie_browser(&self, text: &str) -> Answer;
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
    /// Where DLNA playback state lives — the door `query=status` reads its `media`
    /// block through, and the counterpart of Python's `self.protocol.get_state(name)`
    /// (`protocol.py:2034-2043`). Read-only here: the face that *writes* state is
    /// SOAP, and a second writer in the HTTP layer would be the same shape of mistake
    /// §5's ownership rule exists to prevent.
    store: SharedStore,
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
        store: SharedStore,
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
            store,
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
    /// player. Two ordering calls sit around that shape, and now that the POST
    /// half of this face exists ([`Face::post_api`]) each has a shipped answer to
    /// be read against rather than invented:
    ///
    /// * An unknown `query` is answered **before** the gate, and that is
    ///   measured on both faces rather than chosen: Python's sensitive-list
    ///   condition asks `query in (…) and not self._management_allowed()`
    ///   (`protocol.py:2092-2093`), so a name the chain does not know never
    ///   reaches a gate at all, and the POST walk asks the same question in the
    ///   same order — `if kwargs.get(name) is None: continue`, *then*
    ///   `_gate_refusal(gate)` (`:2534-2539`). Hiding which names we know is
    ///   worth less to a caller than fixing their typo, and the value comes back
    ///   inside a JSON string, not inside markup ([`unknown_query`] says why
    ///   that is safe).
    /// * No `query` (absent, or there and empty) is a 200/`code: 1` that names
    ///   the missing parameter. **This half is ours**: in Python an empty query
    ///   matches none of the `elif` branches, `res` keeps the two-line endpoint
    ///   index built at `protocol.py:2116-2119`, and `:2249` serves that dict —
    ///   a 200 that says nothing and looks like data. The answer written here is
    ///   the one the shipped face already gives when *no POST field* matched
    ///   (§4.7c's fix for `{'code': 0, 'message': 'success'}`): say "nothing ran"
    ///   and name what was missing.
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
            Action::ResolveStatus => self.resolve_status_entry(req),
            Action::ResolveCookies => self.cookie_state_entry(),
            Action::CastTargets => self.cast_targets_entry(req),
            Action::Status => self.status_entry(),
            Action::Interfaces => self.interfaces_entry(),
        }
    }

    /// The gate a route table names, resolved and paid — **the sentence only**,
    /// because the two faces ship the same refusal in different envelopes
    /// ([`Face::gate_refusal`] and [`Face::post_gate_refusal`]).
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
    fn gate_denial(&self, gate: &str, req: &Request) -> Option<&'static str> {
        let Some(parsed) = Gate::parse(gate) else {
            // Fail closed. The caller gets the generic management sentence — a
            // typo in our own table is not information to hand out — while the
            // log gets the name, because the only person who can fix it is the
            // one reading this file. The line says "route", not "GET route": the
            // same table-reading mistake is possible on either face, and a log
            // that names the wrong one sends the reader to the wrong table.
            self.log.error(&format!(
                "route gate {gate:?} is not a gate this port knows; refusing the request"
            ));
            return Some(MANAGEMENT_REFUSAL);
        };
        let token_ok = self.token_ok(req);
        if !is_loopback(&req.remote_ip) && !token_ok {
            return Some(MANAGEMENT_REFUSAL);
        }
        if matches!(parsed, Gate::Token) && !token_ok {
            return Some(TOKEN_REFUSAL);
        }
        None
    }

    /// [`Self::gate_denial`] in the **GET** envelope: HTTP **403** with `code: 1`.
    ///
    /// The deviation is recorded at [`Answer::denied`] — Python sends its GET
    /// refusals as 200 + `code: 403` exactly as it sends POST's, and this port
    /// answers GET refusals in the status because that is what the nine
    /// differential rows and the frontend it serves both read.
    fn gate_refusal(&self, gate: &str, req: &Request) -> Option<Response> {
        Some(json_answer(403, 1, self.gate_denial(gate, req)?))
    }

    /// [`Self::gate_denial`] in the **POST** envelope: HTTP **200** with
    /// `code: 403` in the body.
    ///
    /// This one is **not** a deviation — it is the shipped shape. Every POST
    /// refusal leaves `POST()` through `_body({'code': 403, …})`, and `_body`
    /// never sets a status (`protocol.py:2518`, `:2530`, `:2539`).
    fn post_gate_refusal(&self, gate: &str, req: &Request) -> Option<Response> {
        Some(answer_response(
            200,
            &Answer::denied(self.gate_denial(gate, req)?),
        ))
    }

    /// The token compared against the stored secret, in constant time.
    ///
    /// Two places, in the shipped order — `headers.get('X-Macast-Token') or
    /// self._param('token')` (`protocol.py:1620`): a **non-empty** header wins,
    /// and an empty one falls through to the parameter rather than answering
    /// "no token". The parameter is [`param_value`], i.e. query first and the
    /// form body behind it, because the settings page posts its token **in the
    /// body** (AGENTS.md §4.7b: `:data="{ …, token: cast_info.token }"`). Reading
    /// only the query string would have this port refuse the page that the whole
    /// token gate exists to serve.
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
        let from_param = param_value(req, "token");
        let given = req
            .header("X-Macast-Token")
            .filter(|value| !value.is_empty())
            .or(from_param.as_deref())
            .unwrap_or_default();
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
    /// Reads the two arguments and hands them to [`Face::cast_url_answer`], which
    /// is the same `_cast_url` the POST route `cast-uri` runs. **The name's key is
    /// this face's own**: `GET` reads `title` (`protocol.py:2086`) while the POST
    /// route reads `cast-title` (`:2705`), and the shipped code never reconciles
    /// them. Copying the POST key here would silently break every shortcut that
    /// passes a name, so the divergence is in the reader, not in the answer.
    fn cast_entry(&self, req: &Request) -> Response {
        let url = query_value(req, "url").unwrap_or_default();
        let title = query_value(req, "title").filter(|value| !value.is_empty());
        self.cast_url_answer(&url, title.as_deref())
    }

    /// `_cast_url` (`protocol.py:1674-1695`): the checks and the call, shared by
    /// both faces that can change what is playing.
    ///
    /// The URL is checked for **shape**, not scheme: a non-empty scheme and a
    /// non-empty authority ([`is_absolute_url`]). A whitelist of `http`/`https`
    /// would be this port inventing a rule the reference does not have, and would
    /// refuse the rtsp and ftp addresses a renderer legitimately accepts. The
    /// refused shapes answer **without** touching the shell — a bad URL must not
    /// leave a half-change behind in the player's state.
    ///
    /// **The address is stripped, the name is not** — and the difference is
    /// load-bearing twice. Python writes `url = (url or '').strip()` and then uses
    /// *that* value for the regex test, for `cast_uri`, and for the `'url'` key it
    /// echoes back, while `title` travels exactly as the caller wrote it. So
    /// `url= http://host/a.mp4 ` casts `http://host/a.mp4`, and a name with
    /// padding around it keeps the padding into the player's window title. Tidying
    /// the name here would be this port quietly disagreeing with what the other
    /// half of the row echoes.
    ///
    /// **The success document belongs to the shell, and it is more than two keys.**
    /// `_cast_url` returns `{'code': 0, 'message': 'success', 'url': url,
    /// 'title': title}'` and both callers write that dict whole (`:2086`, `:2540`),
    /// so the `Ok` arm ships the [`Answer`] the seam handed back rather than
    /// re-wrapping its sentence into `{code, message}`. The two shape refusals
    /// below keep the two-key writer: Python's own refusals from that same function
    /// are `{'code': 1, 'message': …}` and nothing else.
    fn cast_url_answer(&self, url: &str, title: Option<&str>) -> Response {
        let url = url.trim();
        if url.is_empty() {
            return json_answer(200, 1, "缺少参数 url：投屏入口需要一个完整的媒体地址。");
        }
        if !is_absolute_url(url) {
            return json_answer(
                200,
                1,
                &format!(
                    "{} 不是一个完整的媒体地址：需要带协议和主机的绝对地址，例如 http://主机/路径。",
                    truncate_chars(url, QUERY_ECHO_MAX)
                ),
            );
        }
        match self.management.cast_url(url, title) {
            Ok(answer) => answer_response(200, &answer),
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

    /// `query=resolve-status&job=<id>` — how far one background parse has got.
    ///
    /// There is **no shape check here**, and that is the shipped behaviour:
    /// `GET` hands `kwargs.get('job')` straight to `_resolve_status`, which does a
    /// dict lookup and answers `None` for anything not in the table. A job id this
    /// port has never seen is not an error to report to the caller — it is the
    /// ordinary answer to "the parse you were watching is gone", and the page
    /// polls this route a second later, so a `code: 1` here is a *state*, not a
    /// malformed request.
    ///
    /// The whole document comes from the seam, including `job` and `status`. This
    /// route is a **pure read**: it never starts, stops, or mutates a job, so an
    /// unanswered poll cannot consume a slot (`RESOLVE_JOBS_MAX` is accounted at
    /// `resolve-page` only).
    fn resolve_status_entry(&self, req: &Request) -> Response {
        let job_id = query_value(req, "job").unwrap_or_default();
        answer_response(200, &self.management.resolve_status(&job_id))
    }

    /// `GET ?query=resolve-cookies` — the cookie card's one read.
    ///
    /// No parameters at all: `:2183-2191` is
    /// `{'code': 0, 'message': 'success', 'state': self._cookie_state()}` and
    /// nothing on the request line enters it. What the caller learns about the
    /// machine (`yt_dlp`, the jar's byte size) is a fact about **this host**, so
    /// the answer cannot be steered by a LAN peer — which matters because this
    /// document names a **file path on disk**.
    fn cookie_state_entry(&self) -> Response {
        answer_response(200, &self.management.cookie_state())
    }

    /// `GET ?query=cast-targets` — the device dropdown's one read.
    ///
    /// `refresh` is the only parameter, and it is read as **text truth**, not as
    /// a boolean: `:1776-1777` turns whatever the query string carried into a
    /// string, strips and lowercases it, and compares against
    /// `('1', 'true', 'yes', 'on')`. So `?refresh=0` means "do **not** search",
    /// while `?refresh=TRUE` means search, and `?refresh=` (present and empty)
    /// means do not search. Collapsing that into "the parameter is there" would
    /// make every poll that passes `refresh=0` kick off a fresh SSDP+mDNS round
    /// trip — which is the expensive thing this normalization exists to avoid.
    /// The normalization lives **here**, in the face, so the shell receives a
    /// decision it can be tested against rather than a wire encoding.
    fn cast_targets_entry(&self, req: &Request) -> Response {
        answer_response(
            200,
            &self.management.cast_targets(query_flag(req, "refresh")),
        )
    }

    /// `query=status` — the settings page's whole status document.
    ///
    /// **No `code`/`message`**, which is the reference's shape and not an
    /// oversight: `GET` hands `self.get_status()` straight to `json.dumps`
    /// (`protocol.py:2200-2201`), so the body starts at `{"server": …}`. Every
    /// other answer on this face wraps, which is why this route cannot go through
    /// [`Answer`] — an `Answer` always writes `code` first, and a page that reads
    /// `status.server.port` would find the document one key shifted.
    fn status_entry(&self) -> Response {
        json_value_response(&self.status_document())
    }

    /// `query=interfaces` — the network picker's three facts on their own.
    ///
    /// Same facts as `status`'s `network` block, **different key names**: the
    /// standalone document calls the pinned interface `current` while the status
    /// block calls it `interface` (`protocol.py:2016-2020` against `:2146-2151`).
    /// Both spellings are pinned by tests, because the obvious tidy move — making
    /// them agree — breaks whichever page reads the one that changed.
    fn interfaces_entry(&self) -> Response {
        let (current, advertised, interfaces) = self.network_facts();
        json_value_response(&object(&[
            ("current", Value::String(current)),
            ("advertised", strings(advertised)),
            ("interfaces", interfaces),
        ]))
    }

    /// The `status` document, built in the reference's order: `server`, `media`,
    /// `clients`, `history`, `network` — and inside `server`, the twelve keys in
    /// the order its dict literal is written (`protocol.py:2002-2024`).
    ///
    /// Three readings are worth naming, because each is a place a "tidier" port
    /// changes the answer:
    ///
    /// * **`protocol` is the stored value, not the resolved one.** The page has to
    ///   show which boxes the user ticked, so a list of `["DLNA"]` prints as
    ///   `["DLNA"]` even though the run started `DLNA Protocol`. The gate built this
    ///   round resolves titles; this document deliberately does not, and `query=cast`
    ///   and the face-opening decision read the resolved one. Two answers to two
    ///   questions — see `status_protocol_reports_the_stored_value_not_the_resolved_one`.
    /// * **`ip` is `/`-joined while `network.advertised` is the list** — same
    ///   source, two shapes (`:2021` against `:2018`), both pinned.
    /// * **`running` is `true` because this route answered.** Python asks
    ///   `Setting.is_service_running()`, but `GET` raises 503 before reaching here
    ///   when the service is down (`protocol.py:2066-2067`), so a body that got as
    ///   far as building this document has already proved the thing the key reports.
    ///   A port that wired a real flag here could print `running: false` from a live
    ///   socket — which is the §4.2 rule about not treating a listening port as
    ///   evidence, read in the other direction.
    fn status_document(&self) -> Value {
        let (system, release) = paths::system_version();
        let (current, advertised, interfaces) = self.network_facts();
        let https_on = setting::https_enabled(&self.setting);
        let server = object(&[
            ("running", Value::Bool(true)),
            ("port", Value::from(setting::application_port(&self.setting))),
            ("version", Value::String(paths::version())),
            ("friendly_name", Value::String(self.setting.friendly_name())),
            ("renderer", self.stored(Key::MacastRenderer, Value::String(String::new()))),
            ("protocol", self.stored_protocols()),
            ("platform", Value::String(paths::sys_platform())),
            ("system", Value::String(system)),
            ("system_version", Value::String(release)),
            ("ip", Value::String(advertised.join("/"))),
            ("https_enabled", Value::Bool(https_on)),
            (
                "https_port",
                if https_on {
                    Value::from(setting::https_port(&self.setting))
                } else {
                    Value::Null
                },
            ),
        ]);
        let media = object(
            &STATUS_MEDIA_NAMES
                .iter()
                .map(|name| (*name, self.store.read(name).unwrap_or(Value::Null)))
                .collect::<Vec<(&str, Value)>>(),
        );
        let clients = Value::Array(
            self.subscription
                .rows()
                .iter()
                .map(|row| {
                    object(&[
                        ("host", Value::String(row.host.clone())),
                        ("url", Value::String(row.url.clone())),
                        ("service", Value::String(row.service.clone())),
                        ("sid", Value::String(row.sid.clone())),
                        ("path", Value::String(row.path.clone())),
                        ("timeout", Value::from(row.timeout)),
                    ])
                })
                .collect(),
        );
        // `history` is the table's rows, printed under the reference's three keys
        // (`protocol.py:1211-1215` writes `uri`, `title`, `time`).
        //
        // **This is a divergence, and §23 of the acceptance script is what found
        // it.** The reference reads `Setting.get(Play_History, [])` — its history
        // lives in the settings file, so it survives a restart. This shell's rows
        // live in the state table (see `StateTable::add_history` in
        // `src-tauri/src/host.rs`), and printing the settings key here would have
        // answered `[]` about a machine that had just cast something: an empty list
        // with two causes, and the page cannot tell them apart (§4.14's
        // 「空结果要说清是哪一种空」, this time in the document the page reads).
        // Rendering what the shell actually remembers is the honest half; making it
        // survive a restart is the other half, and it is a decision with a test on
        // it rather than a line in this function.
        let history = Value::Array(
            self.store
                .play_history()
                .into_iter()
                .map(|(title, uri, at)| {
                    object(&[
                        ("uri", Value::String(uri)),
                        ("title", Value::String(title)),
                        ("time", Value::from(at)),
                    ])
                })
                .collect(),
        );
        let network = object(&[
            ("interface", Value::String(current)),
            ("advertised", strings(advertised)),
            ("interfaces", interfaces),
        ]);
        object(&[
            ("server", server),
            ("media", media),
            ("clients", clients),
            ("history", history),
            ("network", network),
        ])
    }

    /// The three facts the network picker reads, in the order the two documents
    /// want them: the pinned interface, the advertised addresses, the annotated
    /// table.
    ///
    /// One reader of the machine for both routes, because `query=status` and
    /// `query=interfaces` are polled by the same page seconds apart and the answer
    /// must not depend on which one asked first.
    fn network_facts(&self) -> (String, Vec<String>, Value) {
        let rows = macast_core::net::network_interfaces(&self.setting);
        let advertised = macast_core::net::advertised_addresses(&self.setting);
        let current = match self.setting.peek(Key::NetworkInterface) {
            Some(Value::String(text)) => text,
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let table = serde_json::to_value(&rows).unwrap_or(Value::Null);
        (current, advertised, table)
    }

    /// `Setting.get(key, default)` **without the write**, for the keys this route owns
    /// its answer about. The shared reader takes its default into the table for the
    /// next `save` to persist (AGENTS.md §4.2), and a page polls `query=status` every
    /// few seconds — so a `get` here would turn "the user never chose a renderer" into
    /// "the user chose the empty renderer". The keys the picker and the port come from
    /// other helpers, and those keep Python's behaviour; the split is pinned by
    /// `the_status_read_creates_no_key_that_was_not_there`.
    fn stored(&self, key: Key, default: Value) -> Value {
        self.setting.peek(key).unwrap_or(default)
    }

    /// `Setting.get(Macast_Protocols, []) or ([Setting.get(Macast_Protocol, '')] if
    /// Setting.has(Macast_Protocol) else [])` (`protocol.py:2009-2012`).
    ///
    /// The legacy branch keeps Python's **nesting**: if the old key somehow holds a
    /// list, the answer is a list containing a list. That is what the reference
    /// prints, and a page that renders it does the same for both ports. Flattening
    /// here would be this port disagreeing about a stored value the gate also reads.
    fn stored_protocols(&self) -> Value {
        let stored = self.stored(Key::MacastProtocols, Value::Array(Vec::new()));
        if py_truthy(&stored) {
            return stored;
        }
        match self.setting.peek(Key::MacastProtocol) {
            Some(legacy) => Value::Array(vec![legacy]),
            None => Value::Array(Vec::new()),
        }
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
        if let Some(answered) = self.post_api(req) {
            return answered;
        }
        self.fall_through(req)
    }

    /// The **POST** management face: [`same_site`] first, then the route table.
    ///
    /// `None` means "no field in this request names a route this port knows",
    /// which is [`Face::fall_through`]'s answer rather than a 404.
    ///
    /// **`_same_site()` is the first question, and it is asked once.** In Python
    /// it is the first statement after the content type
    /// (`protocol.py:2515-2518`), so a cross-site POST hears the cross-site
    /// sentence *even when it also carries a field no route knows* — the order
    /// decides which of the two refusals the caller reads, and this port keeps it.
    /// Once rather than per route because the shipped loop asks it once too; the
    /// per-route check is `_gate_refusal`, a different question about a different
    /// thing (address and token, not who opened the browser).
    ///
    /// **The SOAP branch in [`Face::post`] never pays it**, and that is measured
    /// rather than inferred: `DLNAHandler.POST` reads the body, tests
    /// `param == 'action'`, and only otherwise calls `super().POST(...)`
    /// (`protocol.py:3151-3160`), so a control point's `SetAVTransportURI` —
    /// which arrives over plain HTTP from a television that sends no `Origin` and
    /// no `Referer` — is never judged by this question. And per [`same_site`], a
    /// request with neither is *not* a browser at all, so the two faces stay
    /// reachable for the senders that cannot send the header.
    ///
    /// There is no path condition on any of this, which is also measured: the
    /// base `Handler.POST` looks only at the fields, never at
    /// `cherrypy.request.path_info`. So `POST /dmr/weird` with a cross-site
    /// `Origin` is refused here exactly as it is refused there, and a
    /// same-site-or-no-header POST to any path with no known field ends in
    /// [`Face::fall_through`].
    fn post_api(&self, req: &Request) -> Option<Response> {
        if let Err(refusal) = same_site(req, &self.setting) {
            // [`same_site`] has already written its own warning line, and the
            // envelope is the shipped one: **200** with `code: 403` in the body
            // (`protocol.py:2518` — `_body` never sets a status).
            return Some(answer_response(200, &Answer::denied(refusal)));
        }
        for route in POST_MANAGEMENT_ROUTES {
            // The route is selected by the **name**, whatever its value: Python
            // asks `if kwargs.get(name) is None: continue` (`:2535`). So
            // `cast-uri=` present-and-empty runs `cast-uri` and lets that route
            // discover the url is missing, rather than falling through to an
            // answer that reads the caller's own field name back to them.
            if !param_present(req, route.field) {
                continue;
            }
            if let Some(refusal) = self.post_gate_refusal(route.gate, req) {
                return Some(refusal);
            }
            return Some(match route.action {
                PostAction::CastUrl => self.cast_url_answer(
                    &param_value(req, "cast-uri").unwrap_or_default(),
                    param_value(req, "cast-title")
                        .filter(|value| !value.is_empty())
                        .as_deref(),
                ),
                // Everything that decides *what a page is* — whether the text is
                // an address at all, which job slot it gets, what `note` comes
                // back with it — belongs to the seam. This call hands over one
                // string and prints the dict it gets.
                PostAction::ResolvePage => answer_response(
                    200,
                    &self
                        .management
                        .resolve_page(&param_value(req, "resolve-page").unwrap_or_default()),
                ),
                // `candidate` and `job` travel as **raw strings** so that
                // 「没有这个候选项」 keeps one owner — see [`Management`].
                PostAction::CastResolved => answer_response(
                    200,
                    &self.management.cast_resolved(
                        &param_value(req, "job").unwrap_or_default(),
                        &param_value(req, "candidate").unwrap_or_default(),
                        &param_value(req, "target").unwrap_or_default(),
                    ),
                ),
                // The jar arrives as **one string** and the whole of it is the
                // field's value: Python reads `kwargs.get('resolve-cookies')`
                // and hands the text to `save_cookie_jar` without touching it
                // (`protocol.py:2990-2996`). Counting the cookies, judging an
                // empty jar, and writing 0600 are the seam's decisions — the
                // face only names the field it read.
                PostAction::SaveCookies => answer_response(
                    200,
                    &self.management.save_cookies(
                        &param_value(req, "resolve-cookies").unwrap_or_default(),
                    ),
                ),
                // No argument at all: the presence of the field *is* the request.
                PostAction::ClearCookies => answer_response(200, &self.management.clear_cookies()),
                // Same shape as save — one string in, one dict out, and the
                // grammar of `名字:资料目录` is decided behind the seam.
                PostAction::SetCookieBrowser => answer_response(
                    200,
                    &self.management.set_cookie_browser(
                        &param_value(req, "resolve-cookie-browser").unwrap_or_default(),
                    ),
                ),
            });
        }
        None
    }

    /// A `POST` that is not an action route **and not a management route**.
    ///
    /// In Python this is `return super(DLNAHandler, self).POST(...)`
    /// (`protocol.py:3160`) arriving at the end of the base `Handler.POST`, after
    /// `_same_site()`, the upload loop, and the whole `POST_ROUTES` walk. The
    /// shipped table has **16** rows; [`POST_MANAGEMENT_ROUTES`] carries the six
    /// this port has implemented, so a field Python knows and this port has not
    /// ported yet (`clear-log`, `set-subtitle-show`, `mirror-action`,
    /// `install-webrtc-extras`, …) lands here and is echoed as 不认识字段. That is
    /// the honest half of a partial port: a page button whose backend does not
    /// exist yet must not read as "saved", and §4.7c's original bug was exactly
    /// `{'code': 0, 'message': 'success'}` on an unrecognised field.
    ///
    /// The **file** upload loop (`protocol.py:2521-2533`) is not ported at all,
    /// and its absence is what makes `plugin-file` reach the answer below: Python
    /// dispatches uploads by field name *before* the route table, so it never
    /// gets to `unknown` there. `FILE_ROUTE_PARAMS` exists to give this port the
    /// same silence about that one name without pretending to serve it.
    ///
    /// What this method answers is what the base handler answers for any request
    /// it does not recognise: **HTTP 200**, not 501 — `protocol.py:2548-2551`
    /// builds `{'code': 1, 'message': '没有可执行的操作…'}` and returns it, and
    /// cherrypy turns a returned dict into a 200 with the JSON tool. A 501 here
    /// would be a *better* HTTP answer and a **worse** port: the point of the
    /// shipped shape is "the request arrived and nothing matched", and a code
    /// that also means "I refuse to implement this" would read differently to a
    /// sender that has already learned the 200/`code: 1` pair.
    ///
    /// [`Management`]: crate::http::Management
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

/// The pairs an `application/x-www-form-urlencoded` body declares.
///
/// Read **only** when the `Content-Type` says form-urlencoded, because that is
/// the only body cherrypy merges into `kwargs`, and a SOAP envelope must not be
/// scanned for field names. Percent-decoded like the query string, from a lossy
/// copy of the bytes — see [`macast_core::url::unquote_plus`] for the one place
/// that reading differs from cherrypy's.
fn form_pairs(req: &Request) -> Vec<(String, String)> {
    let content_type = req.header("Content-Type").unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .contains("x-www-form-urlencoded")
    {
        return Vec::new();
    }
    // Pairs **by value**: the body lives in `req`, the text we parse is a lossy
    // copy of it, and a `&str` would have to borrow from a `String` this
    // function drops. The copy is the cheaper defect to own.
    let text = String::from_utf8_lossy(&req.body);
    parse_query(&text)
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

/// The names an `application/x-www-form-urlencoded` body declares — see
/// [`form_pairs`]. Values are ignored here; only the names reach the
/// "unrecognised fields" answer.
fn form_field_names(req: &Request) -> Vec<String> {
    form_pairs(req)
        .into_iter()
        .map(|(name, _)| name)
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

/// Whether a query parameter carries a **yes**, read the way the shipped card
/// reads it: as text, not as presence.
///
/// ```python
/// if isinstance(refresh, str):
///     refresh = refresh.strip().lower() in ('1', 'true', 'yes', 'on')
/// ```
///
/// (`protocol.py:1776-1777`, and the docstring above it says the plain words:
/// 「`?refresh=0` means "do not search"」.) So four names mean yes, everything
/// else means no — including the **empty** value, which a form or a hand-written
/// URL leaves behind more often than anyone admits. Absent is no.
///
/// Two deviations from Python, both inherited from [`query_value`] rather than
/// chosen here:
///
/// - **A repeated key.** cherrypy merges `?refresh=0&refresh=0` into a *list*,
///   so `isinstance(refresh, str)` is false and the normalization is skipped
///   entirely — and a non-empty list is truthy, so that request **searches**
///   there. Here it does not. The shipped behaviour on a repeated flag is an
///   accident of Python's truthiness, and no test can ask it.
/// - **`strip()`** runs before `lower()` on the whole text, so `%20true%20` is
///   yes. Percent-decoding already happened upstream, so this sees the spaces.
fn query_flag(req: &Request, name: &str) -> bool {
    query_value(req, name)
        .map(|value| {
            let text = value.trim().to_lowercase();
            matches!(text.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// One parameter, read the way cherrypy merges a request: **query first, then
/// the form body**. `None` means nobody said that word at all.
///
/// The order is a measurement, not a preference. `process_query_string` runs
/// **before** the body is processed and both write into **the same dict**
/// (`_cprequest.py:700-701` then `:712-713`, which hands `RequestBody`
/// `request_params=self.params` by reference at `:693`), and the second writer
/// *appends* where the key is already there:
///
/// ```text
/// if key in request_params:  ... list(...).append(value)
/// else:                      request_params[key] = value
/// ```
/// (`_cpreqbody.py:988-996`, and `process_urlencoded` at `:143-166` does the
/// same inside the body itself). So `?token=A` over a body `token=B` arrives as
/// `['A', 'B']`, and the shipped reader takes the **first** element
/// (`Handler._param`, `protocol.py:1597`) — which is the query's.
///
/// The consequence that matters for a gate: `?token=` (there, and empty) over a
/// body `token=secret` gives `['', 'secret'][0] == ''`, and
/// `headers.get(...) or self._param('token')` is therefore falsy. A caller who
/// leaves an empty `token=` in the query string while putting the real secret in
/// the form is **refused**, and this port refuses them too.
fn param_value(req: &Request, name: &str) -> Option<String> {
    if let Some(from_query) = query_value(req, name) {
        return Some(from_query);
    }
    form_pairs(req)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// Whether a request **names** a parameter, whatever value it carries.
///
/// This is the test the shipped route loop makes: `if kwargs.get(name) is None:
/// continue` (`protocol.py:2535`). It asks the **key**, never the value, so
/// `cast-uri=` (present and empty) *selects* that route and the route's own
/// handler is the one that discovers the url is missing — the answer the user
/// reads is 「缺少参数 url」, not a fall-through that reads their own field name
/// back to them. A value-based match would fall through instead, and the two
/// documents say opposite things about whether the request made sense.
fn param_present(req: &Request, name: &str) -> bool {
    req.query.iter().any(|(key, _)| key == name)
        || form_pairs(req).iter().any(|(key, _)| key == name)
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

/// The state variables `query=status` reads into its `media` block, in the order
/// Python's loop writes them (`protocol.py:2033-2043`).
///
/// The list is the reference's **superset**, not this port's: it asks for
/// `AbsoluteTimePosition`, `CurrentPlayMode` and `DisplayCurrentSubtitle` even where
/// nothing here has a value for them, and an absent variable prints as `null` —
/// which is Python's `except Exception: media[name] = None` reading, not an error.
/// Trimming the list to "names we happen to write" would move a key off the page
/// rather than leave it blank, and the page reads by name.
pub const STATUS_MEDIA_NAMES: [&str; 16] = [
    "CurrentURI",
    "CurrentTrackURI",
    "CurrentTrackTitle",
    "CurrentTrackDuration",
    "CurrentMediaDuration",
    "AbsoluteTimePosition",
    "RelativeTimePosition",
    "TransportState",
    "TransportStatus",
    "TransportPlaySpeed",
    "CurrentPlayMode",
    "Volume",
    "Mute",
    "NumberOfTracks",
    "CurrentTrack",
    "DisplayCurrentSubtitle",
];

/// A `200` whose body is a document this face wrote itself, with no `{code,
/// message}` wrapper — the shape `query=status` and `query=interfaces` answer in,
/// because the shipped handler passes those two dicts straight to `json.dumps`
/// while every other route builds `{'code': …, 'message': …}` first.
fn json_value_response(document: &Value) -> Response {
    Response::new(
        200,
        JSON_CONTENT_TYPE,
        py_json_dumps(document).into_bytes(),
    )
}

/// A JSON object with its keys **in the order given**.
///
/// `serde_json::Map` is a `BTreeMap` unless the `preserve_order` feature is on, and
/// this workspace turns it on precisely for this kind of document: Python's dicts
/// remember insertion order, `json.dumps` prints that order, and the status page's
/// rows are the twelve keys of `server` in the order `protocol.py` writes them. A
/// `(key, value)` slice keeps that order in the code instead of in a feature flag,
/// which is why [`Answer`] holds a `Vec` for the same reason.
fn object(rows: &[(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    for (name, value) in rows {
        map.insert((*name).to_string(), value.clone());
    }
    Value::Object(map)
}

/// A list of strings as a `Value`.
fn strings(items: Vec<String>) -> Value {
    Value::Array(items.into_iter().map(Value::String).collect())
}

/// Python's truthiness for a JSON value — the rule behind `x or y`.
///
/// Four of these decide a printed answer rather than an internal branch, so the
/// spellings are worth naming: `0` and `0.0` are **false** (a stored
/// `ApplicationPort: 0` is not what makes this fire — it is a stored
/// `Macast_Protocols: 0`, which prints the legacy key's list instead), `""` and
/// `[]` and `{}` and `null` are false, `"0"` is **true** (a string holding a zero
/// is a real value to `or`), and so is a list holding one empty string — `[""]` is
/// a non-empty list.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number
            .as_f64()
            .map(|value| value != 0.0)
            .unwrap_or(true),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// [`json_answer`] for the routes whose dict carries more than two keys.
///
/// `_resolve_page` answers `{code, message, job, url, note}` and
/// `_post_cast_resolved` reaches success through `_cast_url` and then adds
/// `relay`/`note` — so a `(code, message)` pair cannot describe what the shipped
/// handler writes, and the order of the rest is the route's own
/// ([`Answer`]).
fn answer_response(status: u16, answer: &Answer) -> Response {
    Response::new(status, JSON_CONTENT_TYPE, answer_bytes(answer))
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
    // The predicate has **one** owner (`macast_core::url`), because the same
    // question is about to be asked from the other direction: the description
    // reader compares a `LOCATION` host against the peer that answered it.
    // There it is `urlsplit`'s `hostname`, which may carry a `%scope` and may
    // not be an address at all, so that owner answers three states — true,
    // false, "not an IP literal". **This** call site is where the third state
    // becomes `false`, and the paragraph above is why.
    macast_core::host_is_loopback(remote_ip).unwrap_or(false)
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

/// `json.dumps(obj, indent=4)` for an arbitrary document.
///
/// Written out rather than pulled from a serde formatter because **indent and
/// key order are the wire format**: serde_json's pretty printer uses two spaces,
/// and `serde_json::to_string` never inserts the newlines at all. The recursion
/// is Python's own shape -- `_indent` levels of four spaces each, `,\n` between
/// items (the item separator loses its space the moment `indent` is set), `": "`
/// between key and value, and **`{}` / `[]` with no whitespace** for the empty
/// containers.
///
/// Numbers go out through serde_json's `Display`, which is shortest-roundtrip
/// like Python's `repr`, so a rounded `4.0` keeps its `.0` (`py_round1`'s answers
/// are floats and an integer-looking elapsed time is a different document).
/// `Value::Bool` and `Value::Null` agree with Python's `true`/`false`/`null`
/// because JSON inherited those spellings from the same place.
fn py_json_dumps(value: &Value) -> String {
    dump_at(value, 0)
}

/// [`py_json_dumps`] at one nesting level. Split out so the pad width is the
/// only arithmetic in the function, and so a test can reach a nested fragment.
fn dump_at(value: &Value, level: usize) -> String {
    let pad = " ".repeat((level + 1) * 4);
    let close = " ".repeat(level * 4);
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("\"{}\"", py_json_quote(text)),
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let mut out = String::from("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad);
                out.push_str(&dump_at(item, level + 1));
            }
            out.push('\n');
            out.push_str(&close);
            out.push(']');
            out
        }
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut out = String::from("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad);
                out.push('"');
                out.push_str(&py_json_quote(key));
                out.push_str("\": ");
                out.push_str(&dump_at(item, level + 1));
            }
            out.push('\n');
            out.push_str(&close);
            out.push('}');
            out
        }
    }
}

/// [`py_json_dumps_indented`] and [`answer_bytes`], given an [`Answer`].
///
/// The order is the one [`Answer`] documents: `code`, `message`, then the extra
/// keys as they were added. `serde_json::Map` is insertion-ordered here because
/// `preserve_order` is a workspace feature -- **and this is the one place that
/// depends on it**, so the two-key layout test that measures
/// [`py_json_dumps_indented`]'s 84-byte body is also the witness that the
/// feature is on.
fn answer_value(answer: &Answer) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("code".to_string(), Value::from(answer.code));
    map.insert("message".to_string(), Value::String(answer.message.clone()));
    for (name, value) in &answer.fields {
        map.insert(name.clone(), value.clone());
    }
    Value::Object(map)
}

/// The bytes `_body(...)` sends for an [`Answer`].
fn answer_bytes(answer: &Answer) -> Vec<u8> {
    py_json_dumps(&answer_value(answer)).into_bytes()
}

/// `json.dumps(obj, indent=4)` for the one two-key dict the base handler
/// returns. Kept as a named function because it is the shape the measured
/// bodies assert against, and because a caller that only has a sentence should
/// not have to build an [`Answer`] to say it.
fn py_json_dumps_indented(code: i64, message: &str) -> String {
    let mut map = serde_json::Map::new();
    map.insert("code".to_string(), Value::from(code));
    map.insert("message".to_string(), Value::String(message.to_string()));
    py_json_dumps(&Value::Object(map))
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
    use serde_json::json;
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
    const DENIED_DOC: &str = "{\n    \"code\": 403,\n    \"message\": \"";
    const QUOTE_IN: &str = "a\"b\\c\nd\te\rf\u{8}g\u{c}h/i\u{1}j没k\u{1F600}";
    const QUOTE_OUT: &str = "a\\\"b\\\\c\\nd\\te\\rf\\bg\\fh/i\\u0001j\\u6ca1k\\ud83d\\ude00";

    fn fresh_setting(name: &str) -> Arc<Setting> {
        let path = std::env::temp_dir()
            .join(format!("macast-upnp-http-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Arc::new(Setting::new(path))
    }

    /// DLNA playback state as a settable map — the door `query=status` reads its
    /// `media` block through.
    ///
    /// A test that wants a value **sets** it; nothing here defaults to a plausible
    /// answer, because "the page shows `PLAYING`" has to be the result of a write
    /// someone made, not of the fixture's kindliness. The playlist methods are inert
    /// for the same reason: no route in this file reaches them, and an answer that
    /// is never asked must not be able to drift.
    #[derive(Default)]
    struct TestStore {
        state: Mutex<serde_json::Map<String, Value>>,
        history: Mutex<Vec<(String, String, i64)>>,
    }

    impl TestStore {
        fn new() -> Arc<TestStore> {
            Arc::new(TestStore {
                state: Mutex::new(serde_json::Map::new()),
                history: Mutex::new(Vec::new()),
            })
        }

        fn put(&self, name: &str, value: Value) {
            self.state
                .lock()
                .expect("state")
                .insert(name.to_string(), value);
        }

        /// A cast this table remembers. `(title, uri, time)` in the trait's order —
        /// the reference's own row order, which `add_history` keeps newest-first.
        fn push_history(&self, title: &str, uri: &str, time: i64) {
            self.history
                .lock()
                .expect("history")
                .push((title.to_string(), uri.to_string(), time));
        }
    }

    impl crate::store::StateStore for TestStore {
        fn read(&self, name: &str) -> Option<Value> {
            self.state.lock().expect("state").get(name).cloned()
        }

        fn write(&self, name: &str, value: Value) -> bool {
            let mut table = self.state.lock().expect("state");
            let changed = table.get(name) != Some(&value);
            if changed {
                table.insert(name.to_string(), value);
            }
            changed
        }

        fn playlist_push(&self, uri: &str) -> (i64, i64) {
            (1, i64::from(!uri.is_empty()))
        }

        fn playlist(&self) -> Vec<String> {
            Vec::new()
        }

        fn playlist_index(&self) -> i64 {
            -1
        }

        fn set_playlist_index(&self, _index: i64) {}

        fn add_history(&self, _title: &str, _uri: &str) {}

        fn play_history(&self) -> Vec<(String, String, i64)> {
            self.history.lock().expect("history").clone()
        }
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
        Face::new(
            control,
            subscription,
            management as Arc<dyn Management>,
            setting,
            TestStore::new() as SharedStore,
        )
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
            TestStore::new() as SharedStore,
        )
        .expect("face builds")
        .with_mtime_probe(probe)
        .into()
    }

    /// A face over a **settable** state table and a settable subscriber table —
    /// the two doors `query=status` reads but does not own. A test that wants the
    /// page to read `PLAYING` writes `PLAYING`; a test that wants a client listed
    /// pushes a row. Neither fixture starts with either, so a green can only come
    /// from the route's reading, never from a friendly default.
    fn harness_status(name: &str) -> (Arc<Face>, Arc<TestStore>, Arc<FakeSubscription>) {
        let store = TestStore::new();
        let subscription = Arc::new(FakeSubscription::new());
        let face = Face::new(
            Arc::new(FakeControl::soap_ok()),
            subscription.clone() as Arc<dyn Subscription>,
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            fresh_setting(name),
            store.clone() as SharedStore,
        )
        .expect("face builds")
        .into();
        (face, store, subscription)
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
        /// What `query=status`'s `clients` list is built from. Empty by default,
        /// because "nobody subscribed" is the ordinary state of a face in a unit
        /// test — and a test that wants the other half sets a row rather than
        /// trusting an empty list to have proved something.
        rows: Mutex<Vec<crate::eventing::SubscriberRow>>,
    }

    impl FakeSubscription {
        fn new() -> Self {
            FakeSubscription {
                renew_status: AtomicU16::new(200),
                remove_status: AtomicU16::new(200),
                add_reply: Mutex::new(("GENA-sid-1".to_string(), "Second-900".to_string())),
                events: Mutex::new(Vec::new()),
                rows: Mutex::new(Vec::new()),
            }
        }

        /// A published subscriber, shaped the way `eventing` shapes one: host and
        /// path already split out of the callback URL by its owner.
        fn push_row(&self, row: crate::eventing::SubscriberRow) {
            self.rows.lock().expect("rows").push(row);
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

        fn rows(&self) -> Vec<crate::eventing::SubscriberRow> {
            self.rows.lock().expect("rows").clone()
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
    ///
    /// The three resolve methods log **their arguments** the same way, and each
    /// answers with the key order [`Management`] says the shipped face writes:
    /// `job` before `url`/`note`, `job` before `status`. A test that wants to read
    /// those extras back can then assert on **which keys came** and in what order
    /// (the serializer prints `fields` in order), which is a fact about this file,
    /// while the values themselves stay the fake's invention.
    ///
    /// The five cookie/target methods log the same way, and each answers with the
    /// key order [`Management`] says the shipped face writes: `state` after
    /// `message`, `targets` after `message`, `message` **before** `cookies`/`bad`
    /// (Python formats that message *from* the counts it then also sends, so a
    /// document that carries the counts first has inverted the dependency).
    /// `cast_targets` answers with the `refresh` bool it received written into
    /// `searching`, and `save_cookies`/`set_cookie_browser` answer with values
    /// derived from the **whole raw field** — so a test can see that this file
    /// split nothing and decoded nothing beyond reading the parameter.
    struct FakeManagement {
        refuses: AtomicBool,
        calls: Mutex<Vec<(String, Option<String>)>>,
        resolve_calls: Mutex<Vec<String>>,
        status_calls: Mutex<Vec<String>>,
        cast_resolved_calls: Mutex<Vec<(String, String, String)>>,
        cookie_state_calls: AtomicUsize,
        targets_calls: Mutex<Vec<bool>>,
        save_cookies_calls: Mutex<Vec<String>>,
        clear_cookies_calls: AtomicUsize,
        browser_calls: Mutex<Vec<String>>,
    }

    impl FakeManagement {
        fn new() -> Self {
            FakeManagement {
                refuses: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
                resolve_calls: Mutex::new(Vec::new()),
                status_calls: Mutex::new(Vec::new()),
                cast_resolved_calls: Mutex::new(Vec::new()),
                cookie_state_calls: AtomicUsize::new(0),
                targets_calls: Mutex::new(Vec::new()),
                save_cookies_calls: Mutex::new(Vec::new()),
                clear_cookies_calls: AtomicUsize::new(0),
                browser_calls: Mutex::new(Vec::new()),
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

        fn resolve_calls(&self) -> Vec<String> {
            self.resolve_calls.lock().expect("resolve calls").clone()
        }

        fn status_calls(&self) -> Vec<String> {
            self.status_calls.lock().expect("status calls").clone()
        }

        fn cast_resolved_calls(&self) -> Vec<(String, String, String)> {
            self.cast_resolved_calls
                .lock()
                .expect("cast-resolved calls")
                .clone()
        }

        fn cookie_state_calls(&self) -> usize {
            self.cookie_state_calls.load(Ordering::SeqCst)
        }

        fn targets_calls(&self) -> Vec<bool> {
            self.targets_calls.lock().expect("targets calls").clone()
        }

        fn save_cookies_calls(&self) -> Vec<String> {
            self.save_cookies_calls.lock().expect("save calls").clone()
        }

        fn clear_cookies_calls(&self) -> usize {
            self.clear_cookies_calls.load(Ordering::SeqCst)
        }

        fn browser_calls(&self) -> Vec<String> {
            self.browser_calls.lock().expect("browser calls").clone()
        }
    }

    impl Management for FakeManagement {
        fn cast_url(&self, url: &str, title: Option<&str>) -> Result<Answer, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((url.to_string(), title.map(str::to_string)));
            if self.refuses.load(Ordering::SeqCst) {
                return Err("播放器拒绝了这次投屏".to_string());
            }
            Ok(Answer::ok("success")
                .with("url", Value::String(url.to_string()))
                .with("title", match title {
                    Some(name) => Value::String(name.to_string()),
                    None => Value::Null,
                }))
        }

        fn resolve_page(&self, text: &str) -> Answer {
            self.resolve_calls
                .lock()
                .expect("resolve calls")
                .push(text.to_string());
            Answer::ok("success")
                .with("job", Value::String(format!("job-{}", text.chars().count())))
                .with("url", Value::String(text.to_string()))
                .with("note", Value::String("从分享原文里读到网址".to_string()))
        }

        fn resolve_status(&self, job_id: &str) -> Answer {
            self.status_calls
                .lock()
                .expect("status calls")
                .push(job_id.to_string());
            if job_id.is_empty() {
                return Answer::fail("没有这个解析任务（超时或已重启）")
                    .with("job", Value::String(String::new()))
                    .with("status", Value::Null);
            }
            Answer::ok("success")
                .with("job", Value::String(job_id.to_string()))
                .with(
                    "status",
                    serde_json::json!({"done": true, "step": 4}),
                )
        }

        fn cast_resolved(&self, job: &str, candidate: &str, target: &str) -> Answer {
            self.cast_resolved_calls
                .lock()
                .expect("cast-resolved calls")
                .push((job.to_string(), candidate.to_string(), target.to_string()));
            if job.is_empty() {
                return Answer::fail("解析任务已经不在了，请重新解析");
            }
            Answer::ok("success")
                .with("relay", Value::String(format!("/relay/{job}-{candidate}/media")))
                .with("target", Value::String(target.to_string()))
        }

        fn cookie_state(&self) -> Answer {
            self.cookie_state_calls.fetch_add(1, Ordering::SeqCst);
            Answer::ok("success").with(
                "state",
                serde_json::json!({
                    "jar": true,
                    "cookies": 2,
                    "bad": 0,
                    "path": "/tmp/resolve_cookies.txt",
                    "browser": "",
                    "browsers": ["brave", "chrome", "chromium", "edge", "firefox",
                                 "opera", "safari", "vivaldi", "whale"],
                    "max_bytes": 262144,
                    "yt_dlp": true,
                }),
            )
        }

        fn cast_targets(&self, refresh: bool) -> Answer {
            self.targets_calls.lock().expect("targets calls").push(refresh);
            Answer::ok("success").with(
                "targets",
                serde_json::json!({
                    "items": [{ "id": "local", "kind": "local",
                                "name": "MPV Renderer", "address": "" }],
                    "searching": refresh,
                    "known": true,
                }),
            )
        }

        fn save_cookies(&self, text: &str) -> Answer {
            self.save_cookies_calls
                .lock()
                .expect("save calls")
                .push(text.to_string());
            if text.is_empty() {
                return Answer::fail("这份文件里没有一条能读懂的 cookie（0 行看不懂）");
            }
            let lines = text.lines().count();
            Answer::ok(format!("已保存 {lines} 条 cookie"))
                .with("cookies", serde_json::json!(lines))
                .with("bad", serde_json::json!(0))
        }

        fn clear_cookies(&self) -> Answer {
            self.clear_cookies_calls.fetch_add(1, Ordering::SeqCst);
            Answer::ok("本机没有保存过 cookie 文件")
        }

        fn set_cookie_browser(&self, text: &str) -> Answer {
            self.browser_calls
                .lock()
                .expect("browser calls")
                .push(text.to_string());
            if text.is_empty() {
                return Answer::ok("不再从浏览器读取 cookie");
            }
            Answer::ok(format!("下次解析会从 {text} 读取 cookie"))
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
    fn parse_query_decodes_names_and_values() {
        crate::pin_test_dir();
        let req = request("GET", "/?a=1&weird%20name=x&flag&skip-me=&p=%2B+q&r=%zz&s=%", &[]);
        assert_eq!(
            req.query,
            vec![
                ("a".to_string(), "1".to_string()),
                // A name is decoded too, exactly as `unquote_plus(nv[0])` does.
                ("weird name".to_string(), "x".to_string()),
                ("flag".to_string(), "".to_string()),
                // An empty pair is skipped, whatever it decodes to.
                ("skip-me".to_string(), "".to_string()),
                // `%2B` is a literal `+`; a written `+` is a space.
                ("p".to_string(), "+ q".to_string()),
                // Not two hex digits: the `%` stays and the item follows it.
                ("r".to_string(), "%zz".to_string()),
                // A trailing `%` has no two characters to read.
                ("s".to_string(), "%".to_string()),
            ]
        );
    }

    #[test]
    fn an_encoded_byte_joins_the_run_it_arrives_in() {
        crate::pin_test_dir();
        // The decode is byte-level, so `é` and a lone `0xE7` land in one buffer
        // and the UTF-8 read hands back one replacement character — not the two
        // characters a per-character decoder would invent.
        let req = request("GET", "/?x=%C3%A9%E7", &[]);
        assert_eq!(req.query[0].1, "é\u{FFFD}");
        // `%C3%A9` on its own is a whole `é`, and a `%` that cannot start a
        // sequence does not eat the characters after it.
        let req = request("GET", "/?x=%C3%A9", &[]);
        assert_eq!(req.query[0].1, "é");
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
    fn query_values_arrive_percent_decoded() {
        crate::pin_test_dir();
        // This face decodes, and the answer changes shape because of it.
        // `%2Fpath.mp4` becomes `/path.mp4` — still refused, but the echo carries
        // the address the caller really sent, not the wire bytes. `%20` becomes a
        // space, and the strip in [`Face::cast_url_answer`] then finds nothing, so
        // the refusal is the missing-one rather than the bad-shape one. The two
        // sentences are different questions, and a reader on the other end of a
        // shortcut picks the wrong fix if we say the wrong one.
        let (face, management) = harness_management("decoding");
        store_token(&face.setting, Some(SECRET));
        for (value, expected) in [
            ("%2Fpath.mp4", json_body(1, &not_absolute("/path.mp4"))),
            ("%20", json_body(1, MISSING_URL)),
        ] {
            let target = format!("/api?query=cast&url={value}&token={SECRET}");
            let response = face.answer(&get_api(&target, LOCAL));
            assert_eq!(response.status, 200, "{value}");
            assert_eq!(response.body, expected, "{value}");
        }
        assert!(management.calls().is_empty());
    }

    #[test]
    fn an_encoded_address_is_still_an_address() {
        crate::pin_test_dir();
        // The reason decoding is load-bearing rather than cosmetic: a browser
        // building this URL (`FormData`, `URLSearchParams`) and `curl
        // --data-urlencode` percent-encode every `:` and `/`, so the only form of
        // the address that ever arrives here is `https%3A%2F%2F…`. Read verbatim
        // it has no scheme separator and no host, and the caller is told their
        // perfect address "不是一个完整的媒体地址" — the entry this route exists
        // for would be unusable from a browser.
        let (face, management) = harness_management("encoded-cast");
        store_token(&face.setting, Some(SECRET));
        let target =
            format!("/api?query=cast&url=https%3A%2F%2Fhost%2Fa.mp4&token={SECRET}");
        let response = face.answer(&get_api(&target, LOCAL));
        assert_eq!(response.status, 200);
        assert!(response.body.starts_with(OK_DOC.as_bytes()));
        assert_eq!(
            management.calls(),
            vec![("https://host/a.mp4".to_string(), None)],
            "the shell is handed the decoded address"
        );
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
        // its name too. **This face's key for the name is `title`** (`:2086`), not
        // the POST route's `cast-title` (`:2705`) -- the shipped code never
        // reconciles the two, so a shortcut that passes `cast-title` here gets a
        // silent "no opinion". An empty name means "no opinion", not
        // "call it nothing".
        let (face, management) = harness_management("cast-title");
        store_token(&face.setting, Some(SECRET));
        let named = format!("{CAST_TARGET}&title=名称&token={SECRET}");
        assert_eq!(face.answer(&get_api(&named, LOCAL)).status, 200);
        assert_eq!(
            management.calls()[0].1,
            Some("名称".to_string()),
            "the title the caller gave"
        );
        let unnamed = format!("{CAST_TARGET}&title=&token={SECRET}");
        assert_eq!(face.answer(&get_api(&unnamed, LOCAL)).status, 200);
        assert_eq!(management.calls()[1].1, None);
        // The other face's key names nothing on this one.
        let wrong_key = format!("{CAST_TARGET}&cast-title=名称&token={SECRET}");
        assert_eq!(face.answer(&get_api(&wrong_key, LOCAL)).status, 200);
        assert_eq!(
            management.calls()[2].1,
            None,
            "cast-title is the POST face's spelling; here it is a helper param"
        );
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
        // `127.0.0.1:52344` is not an IP literal, so the owner answers the third
        // state and this gate reads it as `false`; the port has to come off
        // before the question is asked.
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
        assert!(GET_MANAGEMENT_ROUTES.len() >= 7);
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
        for action in [
            Action::Cast,
            Action::CastInfo,
            Action::ResolveStatus,
            Action::ResolveCookies,
            Action::CastTargets,
            Action::Status,
            Action::Interfaces,
        ] {
            assert_eq!(
                GET_MANAGEMENT_ROUTES
                    .iter()
                    .filter(|r| r.action == action)
                    .count(),
                1,
                "{action:?} must be reachable by exactly one query name"
            );
        }
        // `cast` is the one row whose gate is *not* management, and the reason is
        // the whole of AGENTS.md §4.7: a `<img>` tag on any webpage can issue
        // this GET, so "it came from this machine" buys it nothing. If a future
        // edit re-labels this row `"management"`, the token requirement is gone
        // and nothing else in this file notices -- so this is the row that has to
        // be pinned by name rather than by the loop above.
        assert_eq!(
            GET_MANAGEMENT_ROUTES
                .iter()
                .find(|r| r.action == Action::Cast)
                .expect("cast")
                .gate,
            "token"
        );
    }

    #[test]
    fn the_post_route_table_is_well_formed() {
        crate::pin_test_dir();
        // The GET table is a `[GetRoute; N]` so its length is a compile-time fact;
        // this one is a `&[PostRoute]` slice, which means an empty scan here would
        // agree with anything. Same two questions, same reason: unique field names
        // (a duplicate would shadow the later one forever) and a gate every row can
        // actually be parsed to.
        assert!(POST_MANAGEMENT_ROUTES.len() >= 6);
        let mut fields: Vec<&str> = POST_MANAGEMENT_ROUTES.iter().map(|r| r.field).collect();
        fields.sort_unstable();
        fields.dedup();
        assert_eq!(fields.len(), POST_MANAGEMENT_ROUTES.len());
        for route in POST_MANAGEMENT_ROUTES.iter() {
            assert!(!route.field.is_empty());
            assert!(
                Gate::parse(route.gate).is_some(),
                "{} carries a gate name this port cannot read: {:?}",
                route.field,
                route.gate
            );
        }
        for action in [
            PostAction::CastUrl,
            PostAction::ResolvePage,
            PostAction::CastResolved,
            PostAction::SaveCookies,
            PostAction::ClearCookies,
            PostAction::SetCookieBrowser,
        ] {
            assert_eq!(
                POST_MANAGEMENT_ROUTES
                    .iter()
                    .filter(|r| r.action == action)
                    .count(),
                1,
                "{action:?} must be reachable by exactly one field name"
            );
        }
        // **Position is semantics**, and unlike the GET table there is no other
        // place a reader can see it: dispatch walks this slice in order and the
        // first *present* field wins, so a two-field request is served by whichever
        // row comes earlier. The shipped order (`protocol.py:2310-2327`) puts the
        // direct cast ahead of the parse, the parse ahead of the cast-of-a-parse,
        // and the three cookie routes in save/clear/browser order. Re-sorted -- say
        // by an editor that thinks a table of names should be alphabetical -- the
        // file still compiles, every route still works alone, and only a request
        // carrying two actions changes behaviour. That is exactly the class of drift
        // AGENTS.md §4.7c calls "structurally invisible", so it is pinned here as a
        // comparison rather than as a fixed sequence: a later row must still land
        // where `protocol.py` puts it, and these pairs stay true when it does.
        let index = |name: &str| {
            POST_MANAGEMENT_ROUTES
                .iter()
                .position(|r| r.field == name)
                .unwrap_or_else(|| panic!("no {name} row"))
        };
        for (earlier, later) in [
            ("cast-uri", "resolve-page"),
            ("resolve-page", "cast-resolved"),
            ("cast-resolved", "resolve-cookies"),
            ("resolve-cookies", "resolve-cookies-clear"),
            ("resolve-cookies-clear", "resolve-cookie-browser"),
        ] {
            assert!(
                index(earlier) < index(later),
                "{earlier} must be dispatched before {later}"
            );
        }
        // And the dispatch code has to actually honour the order it is written in:
        // see `only_the_earlier_of_two_posted_actions_runs`.
    }

    #[test]
    fn only_the_earlier_of_two_posted_actions_runs() {
        crate::pin_test_dir();
        // The table's order is only semantics if the scanner walks it in order.
        // `resolve-page` is ahead of `resolve-cookies`, so a body carrying both
        // starts exactly one parse and saves nothing -- and the reverse pairing
        // (`cast-uri` ahead of everything) is what §4.7c means by "precedence".
        let (face, management) = harness_management("post-precedence");
        let response = face.answer(&post_form(
            "/api",
            "resolve-page=http://host/watch&resolve-cookies=1",
            LOCAL,
            &[],
        ));
        assert_eq!(response.status, 200);
        assert_eq!(
            management.resolve_calls(),
            vec!["http://host/watch".to_string()]
        );
        assert!(
            management.save_cookies_calls().is_empty(),
            "the later row must not also run"
        );
    }

    #[test]
    fn the_cookie_state_read_is_refused_before_the_disk_is_read() {
        crate::pin_test_dir();
        // `query=resolve-cookies` answers with **counts**, but the counts sit
        // next to `path` -- the file on disk -- and the browser spec this machine
        // is configured to read (AGENTS.md §4.14: the jar never enters
        // `macast_setting.json`, so the settings file is not where this map is).
        // A LAN passer-by must therefore hear the management sentence and the
        // seam must not be called at all: a refused read that still reads is the
        // same hole as a refused cast that still casts.
        let (face, management) = harness_management("cookie-state-gate");
        let response = face.answer(&get_api("/api?query=resolve-cookies", LOCAL));
        assert_eq!(response.status, 200);
        // Whole bytes, with the document re-typed here rather than re-derived by
        // calling the seam again: that would prove nothing about the pass-through
        // and would move the counters this test reads.
        let expected = answer_bytes(&Answer::ok("success").with(
            "state",
            serde_json::json!({
                "jar": true,
                "cookies": 2,
                "bad": 0,
                "path": "/tmp/resolve_cookies.txt",
                "browser": "",
                "browsers": ["brave", "chrome", "chromium", "edge", "firefox",
                             "opera", "safari", "vivaldi", "whale"],
                "max_bytes": 262144,
                "yt_dlp": true,
            }),
        ));
        assert_eq!(response.body, expected);
        assert_eq!(management.cookie_state_calls(), 1);

        let refused = face.answer(&get_api("/api?query=resolve-cookies", LAN));
        assert_eq!(refused.status, 403);
        assert_eq!(refused.body, json_body(1, MANAGEMENT_REFUSAL));
        assert_eq!(
            management.cookie_state_calls(),
            1,
            "the refusal must not have read the disk"
        );
        let body = text(&refused.body);
        assert!(!body.contains("resolve_cookies.txt"), "{body}");

        store_token(&face.setting, Some(SECRET));
        let opened = face.answer(&get_api(
            &format!("/api?query=resolve-cookies&token={SECRET}"),
            LAN,
        ));
        assert_eq!(opened.status, 200);
        assert_eq!(opened.body, expected);
        assert_eq!(management.cookie_state_calls(), 2);
    }

    #[test]
    fn the_refresh_flag_is_a_text_question_answered_before_the_seam() {
        crate::pin_test_dir();
        // Python reads this one as a **string** (`protocol.py:1776-1777`): the
        // page polls `cast-targets` with no flag at all, and only the「重新搜索」
        // button sends `refresh=1`. Collapsing the question to "was the parameter
        // there" would turn every `refresh=0` poll into an SSDP + mDNS round trip,
        // so the truth table is asserted through the bool the seam received --
        // `searching` in the answer is that same bool written back, so the
        // document equality below asks the second half: was it forwarded?
        let (face, management) = harness_management("cast-targets-refresh");
        for (flag, expected) in [
            ("&refresh=1", true),
            ("&refresh=true", true),
            ("&refresh=YES", true),
            ("&refresh=On", true),
            ("&refresh=%20true%20", true),
            ("&refresh=0", false),
            ("&refresh=", false),
            ("&refresh=no", false),
            ("", false),
        ] {
            let target = format!("/api?query=cast-targets{flag}");
            let response = face.answer(&get_api(&target, LOCAL));
            assert_eq!(response.status, 200, "{target}");
            let calls = management.targets_calls();
            assert_eq!(calls.last().copied(), Some(expected), "{target}");
            let document = answer_bytes(&Answer::ok("success").with(
                "targets",
                serde_json::json!({
                    "items": [{ "id": "local", "kind": "local",
                                "name": "MPV Renderer", "address": "" }],
                    "searching": expected,
                    "known": true,
                }),
            ));
            assert_eq!(response.body, document, "{target}");
        }
        // A refused list read must not start a search either.
        let before = management.targets_calls().len();
        let refused = face.answer(&get_api("/api?query=cast-targets&refresh=1", LAN));
        assert_eq!(refused.status, 403);
        assert_eq!(refused.body, json_body(1, MANAGEMENT_REFUSAL));
        assert_eq!(management.targets_calls().len(), before);
    }

    #[test]
    fn a_pasted_jar_reaches_the_seam_as_one_undivided_text() {
        crate::pin_test_dir();
        // Three shapes a face can wreck on the way to the shell, all of them
        // invisible if the test only asks "did a save happen":
        //   * a line beginning `#HttpOnly_` is a **cookie**, not a comment
        //     (AGENTS.md §4.14 -- treating it as a comment rejects an entire real
        //     export, because session cookies are usually all of that shape);
        //   * tabs and newlines arrive percent-encoded, and a parser that
        //     decodes the query but not the body hands the shell literal `%09`;
        //   * the field is **not trimmed** here -- `protocol.py:3000` reads
        //     `kwargs.get('resolve-cookies')` and passes the text on untouched.
        let (face, management) = harness_management("post-jar");
        let jar = "#HttpOnly_host.example\tFALSE\t/\tTRUE\t0\tSESSIONID\tabc\n\
                   host.example\tFALSE\t/\tFALSE\t0\ttiny\tvalue with spaces\n\
                   # a comment line";
        let body = concat!(
            "resolve-cookies=%23HttpOnly_host.example%09FALSE%09%2F%09TRUE%090",
            "%09SESSIONID%09abc%0Ahost.example%09FALSE%09%2F%09FALSE%090%09tiny",
            "%09value%20with%20spaces%0A%23%20a%20comment%20line",
        );
        let response = face.answer(&post_form("/api", body, LOCAL, &[]));
        assert_eq!(response.status, 200);
        assert_eq!(management.save_cookies_calls(), vec![jar.to_string()]);
        // The document the stub built is forwarded byte-for-byte. Its message
        // text is the stub's own arithmetic and asserts nothing about the
        // product; what this equality pins is the **pass-through and key order**
        // (AGENTS.md §4.14: `message` is formatted *from* the counts it also
        // carries, so `message`, `cookies`, `bad` in that order).
        let expected = answer_bytes(
            &Answer::ok("已保存 3 条 cookie")
                .with("cookies", serde_json::json!(3))
                .with("bad", serde_json::json!(0)),
        );
        assert_eq!(response.body, expected);
        assert_ne!(response.body, FALL_THROUGH_BODY.as_bytes());

        // Padding and a leading `#` survive a `+`-encoded round trip untouched.
        face.answer(&post_form("/api", "resolve-cookies=+%23a+", LOCAL, &[]));
        assert_eq!(
            management.save_cookies_calls()[1],
            " #a ",
            "the face trims a media address, not a pasted jar"
        );

        // An empty field still **selects** the route (`param_present` only asks
        // for the name), so the shell is the one that says "nothing readable".
        let empty = face.answer(&post_query("/api?resolve-cookies=", LOCAL, &[]));
        assert_eq!(empty.status, 200);
        assert_eq!(management.save_cookies_calls().len(), 3);
        assert_eq!(
            management.save_cookies_calls()[2],
            String::new(),
            "the route runs and the seam gets the empty text"
        );
        assert_eq!(
            empty.body,
            json_body(1, "这份文件里没有一条能读懂的 cookie（0 行看不懂）")
        );
    }

    #[test]
    fn clearing_the_jar_and_naming_a_browser_are_two_routes() {
        crate::pin_test_dir();
        // Both are "one field, no arguments worth parsing" in shape but not in
        // meaning: `resolve-cookies-clear` carries nothing at all (its presence
        // *is* the request), while `resolve-cookie-browser` carries a grammar the
        // face must not interpret (`名字:资料目录`, and the off spellings are
        // decided behind the seam at `protocol.py:3058`).
        let (face, management) = harness_management("post-cookie-routes");
        let cleared = face.answer(&post_query("/api?resolve-cookies-clear=1", LOCAL, &[]));
        assert_eq!(cleared.status, 200);
        assert_eq!(management.clear_cookies_calls(), 1);
        assert_eq!(
            cleared.body,
            answer_bytes(&Answer::ok("本机没有保存过 cookie 文件"))
        );
        assert!(management.browser_calls().is_empty(), "the wrong route ran");

        let named = face.answer(&post_form(
            "/api",
            "resolve-cookie-browser=safari",
            LOCAL,
            &[],
        ));
        assert_eq!(named.status, 200);
        assert_eq!(management.cookie_state_calls(), 0);
        assert_eq!(
            named.body,
            answer_bytes(&Answer::ok("下次解析会从 safari 读取 cookie"))
        );

        face.answer(&post_form("/api", "resolve-cookie-browser=", LOCAL, &[]));
        face.answer(&post_form(
            "/api",
            "resolve-cookie-browser=%20safari%20",
            LOCAL,
            &[],
        ));
        // Three calls, the third still padded: this face does not read the
        // grammar, so it cannot decide that ` safari ` means `safari`.
        assert_eq!(
            management.browser_calls(),
            vec!["safari".to_string(), String::new(), " safari ".to_string()]
        );
        assert_eq!(
            management.clear_cookies_calls(),
            1,
            "the field name decides which route, not the value"
        );
    }

    #[test]
    fn a_lan_post_of_a_cookie_field_is_refused_at_http_200() {
        crate::pin_test_dir();
        // The POST envelope is the other one: HTTP 200 with `code: 403` inside,
        // because `_body` never sets a status (AGENTS.md §4.7c). Loopback **is**
        // the admission for these three rows -- they are `GATE_MANAGEMENT`, not
        // the `GATE_CODE` family, since neither writes anything that runs
        // (`protocol.py:2325-2327`). This test therefore cannot tell a management
        // row from a code row by behaviour: only `Gate::Token` has the second,
        // token-specific check. What it does pin is that a LAN caller without the
        // secret writes nothing and that the secret **in the body** counts --
        // which is how the settings page posts it.
        let (face, management) = harness_management("cookie-lan");
        for field in [
            "resolve-cookies=x",
            "resolve-cookies-clear=1",
            "resolve-cookie-browser=safari",
        ] {
            let response = face.answer(&post_form("/api", field, LAN, &[]));
            assert_eq!(response.status, 200, "{field}");
            assert_eq!(response.body, json_body(403, MANAGEMENT_REFUSAL), "{field}");
        }
        assert!(management.save_cookies_calls().is_empty());
        assert_eq!(management.clear_cookies_calls(), 0);
        assert!(management.browser_calls().is_empty());

        store_token(&face.setting, Some(SECRET));
        let in_body = face.answer(&post_form(
            "/api",
            &format!("resolve-cookie-browser=safari&token={SECRET}"),
            LAN,
            &[],
        ));
        assert_eq!(in_body.status, 200);
        assert!(
            in_body
                .body
                .starts_with(OK_DOC.as_bytes()),
            "{}",
            text(&in_body.body)
        );
        assert_eq!(management.browser_calls(), vec!["safari".to_string()]);

        // `?token=` present-but-empty outranks a body token: cherrypy merges the
        // query first and the body value is *appended*, and `Handler._param`
        // takes element `[0]` -- so this is a refusal, not a mistyped secret.
        // The fixture has to carry the **real** secret in the body for this to
        // ask anything: without it the request would be refused for the wrong
        // reason and the case would be the trivial one below.
        let hollow = face.answer(&post_form(
            "/api?token=",
            &format!("resolve-cookie-browser=safari&token={SECRET}"),
            LAN,
            &[],
        ));
        assert_eq!(hollow.status, 200);
        assert_eq!(hollow.body, json_body(403, MANAGEMENT_REFUSAL));
        let beaten = face.answer(&post_form(
            "/api",
            "resolve-cookie-browser=opera&token=not-the-secret",
            LAN,
            &[],
        ));
        assert_eq!(beaten.status, 200);
        assert_eq!(beaten.body, json_body(403, MANAGEMENT_REFUSAL));
        assert_eq!(
            management.browser_calls(),
            vec!["safari".to_string()],
            "the refused call must not have written"
        );
    }

    #[test]
    fn a_status_poll_passes_the_job_id_and_the_refusal_reads_nothing() {
        crate::pin_test_dir();
        // The page polls `resolve-status&job=<id>` once a second while a parse
        // runs. This face does not know the job table -- that lives behind the
        // seam -- so the only thing it can get wrong is the id it hands over, and
        // "there is no such job" must stay the seam's sentence, not a fall-through.
        let (face, management) = harness_management("resolve-status");
        let polling = face.answer(&get_api("/api?query=resolve-status&job=7", LOCAL));
        assert_eq!(polling.status, 200);
        assert_eq!(management.status_calls(), vec!["7".to_string()]);
        assert_eq!(
            polling.body,
            answer_bytes(
                &Answer::ok("success")
                    .with("job", Value::String("7".to_string()))
                    .with("status", serde_json::json!({"done": true, "step": 4}))
            )
        );

        let blank = face.answer(&get_api("/api?query=resolve-status&job=", LOCAL));
        assert_eq!(management.status_calls()[1], String::new());
        let missing = face.answer(&get_api("/api?query=resolve-status", LOCAL));
        assert_eq!(management.status_calls()[2], String::new());
        let refused = answer_bytes(
            &Answer::fail("没有这个解析任务（超时或已重启）")
                .with("job", Value::String(String::new()))
                .with("status", Value::Null),
        );
        assert_eq!(blank.body, refused);
        assert_eq!(missing.body, refused);
        assert_ne!(missing.body, FALL_THROUGH_BODY.as_bytes());

        let lan = face.answer(&get_api("/api?query=resolve-status&job=7", LAN));
        assert_eq!(lan.status, 403);
        assert_eq!(lan.body, json_body(1, MANAGEMENT_REFUSAL));
        assert_eq!(management.status_calls().len(), 3);
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

    /// A `POST /api?…` whose body is a form, from a peer the caller names.
    ///
    /// `Content-Length` is computed from the body rather than written by hand,
    /// because [`pre_route_status`] answers **411** for a POST that declares no
    /// length -- and 411 is a *pre-route* status, so a test that forgot it does
    /// not fail on the gate it meant to exercise but on a status line that came
    /// from before the dispatcher ran. That reads like a broken gate and is a
    /// broken fixture.
    fn post_form(target: &str, body: &str, remote: &str, headers: &[(&str, &str)]) -> Request {
        let length = body.as_bytes().len().to_string();
        let mut all = vec![
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Content-Length", length.as_str()),
        ];
        all.extend_from_slice(headers);
        let mut req = request_with_body("POST", target, &all, body.as_bytes());
        req.remote_ip = remote.to_string();
        req
    }

    /// A bodyless `POST /api?…` (`Content-Length: 0`), fields from the query only.
    fn post_query(target: &str, remote: &str, headers: &[(&str, &str)]) -> Request {
        let mut all = vec![("Content-Length", "0")];
        all.extend_from_slice(headers);
        let mut req = request("POST", target, &all);
        req.remote_ip = remote.to_string();
        req
    }

    #[test]
    fn a_cross_site_post_hears_the_browser_question_at_http_200() {
        crate::pin_test_dir();
        // AGENTS.md §4.7b: the page is served from a name, so the question is
        // asked against **names**. `evil.test` under DNS rebinding resolves to
        // 127.0.0.1 and is still cross-site -- which is why the fixture here is
        // a foreign name and not a foreign address.
        let (face, management) = face_on_port_management("post-cross-site", TEST_PORT);
        let response = face.answer(&post_form(
            "/api",
            "cast-uri=http://host/a.mp4",
            LAN,
            &[("Origin", "http://evil.test")],
        ));
        let body = text(&response.body);
        assert_eq!(response.status, 200);
        assert!(body.starts_with(DENIED_DOC), "{body}");
        // ASCII survives `ensure_ascii`, so both halves of the sentence -- where
        // you came from, and which address to use instead -- are assertable.
        assert!(body.contains("evil.test"), "{body}");
        assert!(body.contains("http://127.0.0.1:58993/"), "{body}");
        assert!(management.calls().is_empty());
    }

    #[test]
    fn the_browser_question_is_asked_once_and_before_the_table() {
        crate::pin_test_dir();
        // A cross-site request that also carries a field no route knows could
        // hear two different sentences. `post_api` asks the browser question
        // first and returns, so it hears the first -- and the fall-through
        // document, which reads its own field name back to it, never appears.
        let (face, management) = harness_management("post-once");
        let response = face.answer(&post_form(
            "/api",
            "clear-log=1&cast-uri=http://host/a.mp4",
            LAN,
            &[("Origin", "http://evil.test")],
        ));
        let body = text(&response.body);
        assert_eq!(response.status, 200);
        assert!(body.starts_with(DENIED_DOC), "{body}");
        assert!(body.contains("evil.test"), "{body}");
        assert_ne!(response.body, FALL_THROUGH_BODY.as_bytes());
        assert!(!body.contains("clear-log"), "{body}");
        assert!(management.calls().is_empty());
    }

    #[test]
    fn an_origin_port_is_not_compared() {
        crate::pin_test_dir();
        // The shipped code deliberately does not check the port: when 58880 is
        // taken, Macast itself binds a random one, so pinning it would lock the
        // real page out of its own management API -- and there is no cookie here
        // to ride anyway.
        let (face, management) = harness_management("post-origin-port");
        let response = face.answer(&post_form(
            "/api",
            "resolve-page=http://host/watch",
            LOCAL,
            &[("Origin", "http://127.0.0.1:51234")],
        ));
        assert_eq!(response.status, 200);
        assert!(text(&response.body).starts_with(OK_DOC), "{}", text(&response.body));
        assert_eq!(management.resolve_calls(), vec!["http://host/watch".to_string()]);
    }

    #[test]
    fn origin_wins_over_referer_in_both_directions() {
        crate::pin_test_dir();
        // A page chooses its own `Referer`, so `Referer` is only a fallback and
        // `Origin` is the word that settles the answer -- including when the two
        // disagree, which is exactly what an attacker would send.
        let (face, management) = harness_management("post-origin-wins");
        let refused = face.answer(&post_form(
            "/api",
            "cast-uri=http://host/a.mp4",
            LOCAL,
            &[("Origin", "http://evil.test"), ("Referer", "http://127.0.0.1:58880/")],
        ));
        assert!(text(&refused.body).starts_with(DENIED_DOC));
        assert!(management.calls().is_empty());

        let served = face.answer(&post_form(
            "/api",
            "cast-uri=http://host/a.mp4",
            LOCAL,
            &[("Origin", "http://127.0.0.1:58880/"), ("Referer", "http://evil.test/x")],
        ));
        assert!(text(&served.body).starts_with(OK_DOC), "{}", text(&served.body));
        assert_eq!(management.calls().len(), 1);
    }

    #[test]
    fn an_unreadable_origin_is_refused_rather_than_ignored() {
        crate::pin_test_dir();
        // Two shapes that do not parse: a port that is not a number, and a
        // bracketed IPv6 host with no closing bracket. Both must be *refused*
        // with the sentence that says so -- "could not read it" is not a pass,
        // and it is not the cross-site sentence either, because that one names a
        // host the caller never sent.
        for origin in ["http://127.0.0.1:abc", "http://[::1"] {
            let (face, management) = harness_management("post-bad-origin");
            let response = face.answer(&post_form(
                "/api",
                "cast-uri=http://host/a.mp4",
                LOCAL,
                &[("Origin", origin)],
            ));
            assert_eq!(response.status, 200, "{origin}");
            assert_eq!(
                response.body,
                json_body(403, "无法识别的来源站点"),
                "{origin}"
            );
            assert!(management.calls().is_empty(), "{origin}");
        }
    }

    #[test]
    fn no_origin_means_not_a_browser_so_the_gate_is_the_address_or_the_token() {
        crate::pin_test_dir();
        // curl, a phone Shortcut, Home Assistant: no `Origin`, no `Referer`. The
        // shipped code then falls through to the address-and-token gate rather
        // than treating the absence as a cross-site hit.
        let (face, management) = harness_management("post-no-origin-lan");
        store_token(&face.setting, Some(SECRET));
        let refused = face.answer(&post_form(
            "/api",
            "cast-uri=http://host/a.mp4",
            LAN,
            &[("Content-Type", "application/x-www-form-urlencoded")],
        ));
        assert_eq!(refused.status, 200);
        assert_eq!(refused.body, json_body(403, MANAGEMENT_REFUSAL));
        assert!(management.calls().is_empty());

        let served = face.answer(&post_query(
            "/api?cast-uri=http://host/a.mp4&cast-title=x",
            LAN,
            &[("X-Macast-Token", SECRET)],
        ));
        assert!(text(&served.body).starts_with(OK_DOC), "{}", text(&served.body));
        assert_eq!(management.calls().len(), 1);
    }

    #[test]
    fn a_form_body_reaches_the_cast_route_trimmed_and_named() {
        crate::pin_test_dir();
        // The face trims the address (a form field arrives with the padding the
        // caller typed) and passes the title through untouched. The answer the
        // seam builds is forwarded as an indented document with its keys in the
        // order the seam wrote them -- measured here, not asserted per field.
        let (face, management) = harness_management("post-cast-form");
        // A form body, not a query string: those spaces are legal inside a body
        // and illegal in a request line, and `parse_query` keeps the padding
        // verbatim either way, so the trim this test names is the face's work.
        let response = face.answer(&post_form(
            "/api",
            "cast-uri=  http://host/a.mp4  &cast-title=名称",
            LOCAL,
            &[],
        ));
        let calls = management.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http://host/a.mp4");
        assert_eq!(calls[0].1.as_deref(), Some("名称"));
        let expected = answer_bytes(
            &Answer::ok("success")
                .with("url", Value::String("http://host/a.mp4".to_string()))
                .with("title", Value::String("名称".to_string())),
        );
        assert_eq!(response.body, expected);
    }

    #[test]
    fn an_empty_cast_uri_selects_the_route_and_is_the_route_s_problem() {
        crate::pin_test_dir();
        // Route selection asks only whether the **name** is present
        // (`protocol.py:2535`), so `cast-uri=` runs the route and the route
        // discovers the missing address. The fall-through document would have
        // echoed the caller's own field back and said something different.
        let (face, management) = harness_management("post-empty-cast-uri");
        let response = face.answer(&post_query("/api?cast-uri=", LOCAL, &[]));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, json_body(1, MISSING_URL));
        assert!(management.calls().is_empty());
    }

    #[test]
    fn an_empty_query_title_beats_a_body_title() {
        crate::pin_test_dir();
        // cherrypy merges query first and *appends* the body, so `cast-title=`
        // over a body `cast-title=名称` arrives as `['', '名称']` and the first
        // element is the query's empty one. The face then filters the empty
        // string away, so the player is named with no title at all rather than
        // with the one the caller also sent.
        let (face, management) = harness_management("post-title-merge");
        let response = face.answer(&post_form(
            "/api?cast-uri=http://host/a.mp4&cast-title=",
            "cast-title=名称",
            LOCAL,
            &[],
        ));
        let calls = management.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, None);
        let expected = answer_bytes(
            &Answer::ok("success")
                .with("url", Value::String("http://host/a.mp4".to_string()))
                .with("title", Value::Null),
        );
        assert_eq!(response.body, expected);
    }

    #[test]
    fn the_resolve_route_hands_the_whole_text_to_the_seam() {
        crate::pin_test_dir();
        // The seam's contract is that it receives the pasted text **as typed** --
        // share copy, wrapping sentence and all -- and that it alone decides
        // whether that is an address. An empty field still selects the route
        // (same rule as `cast-uri=`), and the seam then gets the empty string.
        let (face, management) = harness_management("post-resolve-empty");
        let empty = face.answer(&post_query("/api?resolve-page=", LOCAL, &[]));
        assert_eq!(management.resolve_calls(), vec![String::new()]);
        assert_ne!(empty.body, FALL_THROUGH_BODY.as_bytes());

        let (face, management) = harness_management("post-resolve-text");
        let pasted = "看看这个 https://host/watch/7，复制打开App";
        let served = face.answer(&post_form("/api", &format!("resolve-page={pasted}"), LOCAL, &[]));
        assert_eq!(management.resolve_calls(), vec![pasted.to_string()]);
        let body = text(&served.body);
        assert!(body.starts_with(OK_DOC), "{body}");
        // Key order is the seam's insertion order, forwarded: job, url, note.
        let job = body.find("\"job\"").expect("job key");
        let url = body.find("\"url\"").expect("url key");
        let note = body.find("\"note\"").expect("note key");
        assert!(job < url && url < note, "{body}");
    }

    #[test]
    fn the_cast_resolved_route_passes_all_three_coordinates() {
        crate::pin_test_dir();
        let (face, management) = harness_management("post-cast-resolved");
        let response = face.answer(&post_query(
            "/api?cast-resolved=1&job=7&candidate=2&target=tv",
            LOCAL,
            &[],
        ));
        assert_eq!(
            management.cast_resolved_calls(),
            vec![("7".to_string(), "2".to_string(), "tv".to_string())]
        );
        let body = text(&response.body);
        assert!(body.starts_with(OK_DOC), "{body}");
        assert!(body.contains("\"relay\""), "{body}");
    }

    #[test]
    fn the_table_order_decides_when_a_request_names_several_actions() {
        crate::pin_test_dir();
        // Position is semantics: the first field present wins and the later rows
        // do not run at all. Here the request body lists `cast-resolved` before
        // `cast-uri`, so a table read in request order would start a relay cast
        // and this one casts -- and only one seam is touched either way, which is
        // the part a wrong precedence would silently double-run.
        let (face, management) = harness_management("post-precedence-first");
        let response = face.answer(&post_form(
            "/api",
            "cast-resolved=1&resolve-page=http://host/watch&cast-uri=http://host/a.mp4",
            LOCAL,
            &[],
        ));
        assert_eq!(management.calls().len(), 1);
        assert!(management.resolve_calls().is_empty());
        assert!(management.cast_resolved_calls().is_empty());
        assert!(text(&response.body).starts_with(OK_DOC));

        let (face, management) = harness_management("post-precedence-second");
        face.answer(&post_form(
            "/api",
            "resolve-page=http://host/watch&cast-resolved=1&job=7",
            LOCAL,
            &[],
        ));
        assert_eq!(management.resolve_calls().len(), 1);
        assert!(management.cast_resolved_calls().is_empty());
        assert!(management.calls().is_empty());
    }

    #[test]
    fn the_soap_path_never_pays_the_browser_question() {
        crate::pin_test_dir();
        // Measured, not inferred: the shipped DLNA handler's POST (`protocol.py:3154`)
        // dispatches the action before anything about the API face runs, so a
        // control point -- which is not a browser and does not send `Origin`, and
        // whose request would fail if it did -- is unaffected. A test that put the
        // same-site check in front of the SOAP branch would break every TV on the
        // LAN while passing every API test above.
        let setting = ported_setting("post-soap-exempt", TEST_PORT);
        let control = Arc::new(FakeControl::soap_ok());
        let management = Arc::new(FakeManagement::new());
        let face = face_built(
            setting,
            control.clone(),
            Arc::new(FakeSubscription::new()),
            management.clone(),
        );
        let response = face.answer(&request_with_body(
            "POST",
            "/AVTransport/action",
            &[
                ("Origin", "http://evil.test"),
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Content-Length", "11"),
            ],
            b"<envelope/>",
        ));
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, crate::soap::SOAP_CONTENT_TYPE);
        assert_eq!(control.calls(), 1);
        assert!(management.calls().is_empty());
    }

    // ---- `query=status` and `query=interfaces` ----------------------------------
    //
    // Both documents are read-only views of facts this file does not own, so every
    // test below sets the fact through its owner (`TestStore`, `FakeSubscription`,
    // the `Setting`) and then asks the route what it printed. Nothing is asserted
    // against a fixture's own kindness: an empty `clients` list and a `null` media
    // variable are the answers *because* nothing was written, and that is worth a
    // sentence precisely because a friendly default would make five of these tests
    // pass while asking nothing (§4.2's 「不提问的检查等于通过的检查」).

    /// The status document, parsed the way the page reads it.
    fn status_value(face: &Arc<Face>) -> serde_json::Value {
        let response = face.answer(&get_api("/api?query=status", LOCAL));
        assert_eq!(response.status, 200);
        serde_json::from_slice(&response.body).expect("status answers a JSON document")
    }

    fn row(sid: &str, host: &str, path: &str, timeout: i64) -> crate::eventing::SubscriberRow {
        crate::eventing::SubscriberRow {
            sid: sid.to_string(),
            service: "AVTransport".to_string(),
            url: format!("http://{host}{path}"),
            host: host.to_string(),
            path: path.to_string(),
            timeout,
            seq: 3,
            errors: 0,
            timed_out: false,
        }
    }

    #[test]
    fn status_answers_one_document_with_the_reference_s_five_sections() {
        crate::pin_test_dir();
        let (face, _, _) = harness_status("status-shape");
        let doc = status_value(&face);
        // **No `code`, no `message`** — the reference hands `get_status()` straight
        // to `json.dumps` (`protocol.py:2200-2201`), so a wrapper here would shift
        // every key the page reads. This is the one answer on this face that must
        // not go through `Answer`, and the assertion is the difference.
        assert!(doc.get("code").is_none(), "status wrapped itself: {doc}");
        assert!(doc.get("message").is_none());
        let keys: Vec<&str> = doc
            .as_object()
            .expect("an object")
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(keys, ["server", "media", "clients", "history", "network"]);
        assert!(doc["server"]["running"].as_bool().unwrap());
        assert!(
            text(&face.answer(&get_api("/api?query=status", LOCAL)).body).starts_with("{\n    \"server\""),
            "the writer's four-space indent and first key are what the page sees"
        );
    }

    #[test]
    fn status_server_block_writes_the_twelve_keys_in_order() {
        crate::pin_test_dir();
        let setting = fresh_setting("status-server");
        setting.set(Key::ApplicationPort, Value::from(58993i64));
        setting.set(Key::DlnaFriendlyName, Value::String("客厅的电视".into()));
        setting.set(Key::MacastRenderer, Value::String("MPV Renderer".into()));
        let store = TestStore::new();
        let face: Arc<Face> = Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            setting.clone(),
            store as SharedStore,
        )
        .expect("face builds")
        .into();
        let server = status_value(&face)["server"].as_object().expect("server").clone();
        let keys: Vec<&str> = server.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "running",
                "port",
                "version",
                "friendly_name",
                "renderer",
                "protocol",
                "platform",
                "system",
                "system_version",
                "ip",
                "https_enabled",
                "https_port"
            ]
        );
        assert_eq!(server["port"], Value::from(58993i64));
        assert_eq!(server["friendly_name"], Value::String("客厅的电视".into()));
        assert_eq!(server["renderer"], Value::String("MPV Renderer".into()));
        assert_eq!(server["version"], Value::String(paths::version()));
        // The CPython spelling, not Rust's — a page that prints
        // `{{ system }} ({{ platform }})` next to every older install would show
        // "Darwin (macos)" if this read `std::env::consts::OS`.
        assert_eq!(server["platform"], Value::String(paths::sys_platform()));
        let (system, release) = paths::system_version();
        assert_eq!(server["system"], Value::String(system));
        assert_eq!(server["system_version"], Value::String(release));
        // HTTPS is off, so the port prints as `null` rather than as a number the
        // page would then offer as a link that does not listen.
        assert_eq!(server["https_enabled"], Value::Bool(false));
        assert_eq!(server["https_port"], Value::Null);
    }

    #[test]
    fn status_https_port_appears_only_when_the_switch_is_on() {
        crate::pin_test_dir();
        let setting = fresh_setting("status-https");
        setting.set(Key::HttpsEnabled, Value::Bool(true));
        setting.set(Key::HttpsPort, Value::from(9443i64));
        let face: Arc<Face> = Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            setting,
            TestStore::new() as SharedStore,
        )
        .expect("face builds")
        .into();
        let server = &status_value(&face)["server"];
        assert_eq!(server["https_enabled"], Value::Bool(true));
        assert_eq!(server["https_port"], Value::from(9443i64));
    }

    #[test]
    fn status_media_reads_the_table_and_nulls_what_was_never_written() {
        crate::pin_test_dir();
        let (face, store, _) = harness_status("status-media");
        let doc = status_value(&face);
        let media = doc["media"].as_object().expect("media");
        // The list is the reference's superset, in its order — including the three
        // this port has no value for, which print `null` rather than disappearing.
        let keys: Vec<&str> = media.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, STATUS_MEDIA_NAMES.to_vec());
        for name in STATUS_MEDIA_NAMES {
            assert_eq!(
                media[name],
                Value::Null,
                "{name} was never written and must not be invented"
            );
        }
        store.put("TransportState", Value::String("PLAYING".into()));
        store.put("Volume", Value::from(37i64));
        let after = status_value(&face)["media"].clone();
        assert_eq!(after["TransportState"], Value::String("PLAYING".into()));
        assert_eq!(after["Volume"], Value::from(37i64));
        // Reading two names must not answer for the rest.
        assert_eq!(after["CurrentURI"], Value::Null);
    }

    #[test]
    fn status_protocol_reports_the_stored_value_not_the_resolved_one() {
        crate::pin_test_dir();
        // The page has to show which boxes the user ticked, so this prints what is
        // on disk: `["DLNA"]` stays `["DLNA"]` even though the run opened
        // `DLNA Protocol`. The gate built the same way *resolves*, and the two
        // documents answering differently is the point, not an accident.
        let cases: Vec<(Value, Value)> = vec![
            // Truthy stored list: echoed whole.
            (json!(["DLNA"]), json!(["DLNA"])),
            (json!(["DLNA Protocol", "Chromecast"]), json!(["DLNA Protocol", "Chromecast"])),
            // A **string** where a list belongs is echoed as the string — Python's
            // `or` sees a non-empty string as truthy and prints it whole, while the
            // gate reads the same value as one entry (AGENTS.md §4.2's coercion
            // rule lives on the gate side only).
            (json!("DLNA"), json!("DLNA")),
            // `[""]` is a non-empty list ⇒ truthy ⇒ echoed, legacy key never asked.
            (json!([""]), json!([""])),
            // Falsy forms fall through to the legacy key or to `[]`.
            (json!([]), json!([])),
            (json!(""), json!([])),
            (json!(0), json!([])),
            (Value::Null, json!([])),
        ];
        for (stored, want) in cases {
            let setting = fresh_setting("status-protocol");
            if stored.is_null() {
                // Leave the key absent — `null` written and `null` never stored
                // must answer the same way for this route.
            } else {
                setting.set(Key::MacastProtocols, stored.clone());
            }
            let face: Arc<Face> = Face::new(
                Arc::new(FakeControl::soap_ok()),
                Arc::new(FakeSubscription::new()),
                Arc::new(FakeManagement::new()) as Arc<dyn Management>,
                setting.clone(),
                TestStore::new() as SharedStore,
            )
            .expect("face builds")
            .into();
            assert_eq!(
                status_value(&face)["server"]["protocol"],
                want,
                "stored {stored} answered wrong"
            );
            assert!(
                setting.peek(Key::MacastProtocol).is_none(),
                "the fallback read created the legacy key in {stored:?}"
            );
        }
        // With the legacy key present and the list empty, Python wraps it in a
        // **list of one** — and keeps the nesting if the old key holds something
        // odd, because that is what it prints.
        let both = fresh_setting("status-legacy");
        both.set(Key::MacastProtocols, json!([]));
        both.set(Key::MacastProtocol, json!("DLNA"));
        let face: Arc<Face> = Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            both,
            TestStore::new() as SharedStore,
        )
        .expect("face builds")
        .into();
        assert_eq!(
            status_value(&face)["server"]["protocol"],
            json!(["DLNA"]),
            "the legacy branch answers a list of one"
        );
    }

    #[test]
    fn status_clients_lists_published_rows_under_the_six_reference_keys() {
        crate::pin_test_dir();
        let (face, _, subscription) = harness_status("status-clients");
        // Nobody subscribed: the answer is an empty list, and this route says so by
        // being empty — which is a **different** answer from `query=subscribers`,
        // where `pending_append` is how you tell "accepted but never published" from
        // "nobody at all". Keeping the two documents apart is deliberate.
        assert_eq!(status_value(&face)["clients"], json!([]));
        subscription.push_row(row("uuid:1", "192.168.1.9:8080", "/cb", 1800));
        subscription.push_row(row("uuid:2", "", "example.com/cb", 900));
        let clients = status_value(&face)["clients"].clone();
        let list = clients.as_array().expect("a list of clients");
        assert_eq!(list.len(), 2);
        let first = list[0].as_object().expect("a client row");
        let keys: Vec<&str> = first.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["host", "url", "service", "sid", "path", "timeout"]);
        assert_eq!(first["host"], json!("192.168.1.9:8080"));
        assert_eq!(first["path"], json!("/cb"));
        assert_eq!(first["url"], json!("http://192.168.1.9:8080/cb"));
        assert_eq!(first["timeout"], json!(1800));
        // The second row is the reference's odd case, not a mistake of ours: its
        // regex `//([0-9:.]*)` matches the **empty** string before a host with
        // letters, so `host` is `''` and everything else lands in `path`. Reproduced
        // because `eventing::split_callback` reads it that way and one owner decides.
        assert_eq!(list[1]["host"], json!(""));
        assert_eq!(list[1]["path"], json!("example.com/cb"));
        // A row's fields come from one snapshot: no seq/errors/timed_out leaks into
        // this document, because `get_status` never wrote them.
        assert!(first.get("seq").is_none() && first.get("errors").is_none());
    }

    #[test]
    fn status_history_prints_the_rows_the_table_remembers() {
        crate::pin_test_dir();
        // An empty history is printed as `[]` — the reference's `[] or []` answer
        // for a page that iterates the key without checking — and it is the *same*
        // `[]` the shell prints after a restart, which is the divergence this route
        // inherited rather than invented. See `store::StateStore::play_history` for
        // why the two causes cannot be told apart from here.
        let (face, store, _) = harness_status("status-history-empty");
        assert_eq!(status_value(&face)["history"], json!([]));
        store.push_history("Rick Astley", "http://h/a.mp4", 1_730_000_000);
        store.push_history("", "http://h/b.ts", 1_730_000_001);
        let rows = status_value(&face)["history"].clone();
        let list = rows.as_array().expect("a list");
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0].as_object().expect("a row").keys().collect::<Vec<_>>(),
            ["uri", "title", "time"],
            "the reference's three keys, in its order (`protocol.py:1211-1215`)"
        );
        assert_eq!(list[0]["uri"], json!("http://h/a.mp4"));
        assert_eq!(list[0]["title"], json!("Rick Astley"));
        assert_eq!(list[0]["time"], json!(1_730_000_000i64));
        // The route does not apply the title fallback: an empty title reaching here
        // would mean the *owner* forgot it, and printing the URI instead would hide
        // that in the document instead of in the log.
        assert_eq!(list[1]["title"], json!(""));
        // And the settings file's `Play_History` is **not** consulted, which is the
        // half a reader of this test would otherwise assume: a stored list that the
        // shell never wrote would print as rows, and a cast the shell made would
        // vanish if the file were the source. Pinning which one is read is what
        // keeps the next person from wiring both.
        let setting = fresh_setting("status-history-file");
        setting.set(Key::PlayHistory, json!([{"uri": "http://from/the/file"}]));
        let face: Arc<Face> = Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            setting.clone(),
            TestStore::new() as SharedStore,
        )
        .expect("face builds")
        .into();
        assert_eq!(
            status_value(&face)["history"],
            json!([]),
            "the document must not read a list this shell never writes"
        );
    }

    #[test]
    fn status_ip_is_the_joined_list_and_agrees_with_the_announcer_s_source() {
        crate::pin_test_dir();
        let (face, _, _) = harness_status("status-ip");
        let doc = status_value(&face);
        let advertised = doc["network"]["advertised"].as_array().expect("a list");
        let joined = doc["server"]["ip"].as_str().expect("a string");
        // Two shapes, one fact: `'/'.join(advertisable_addresses())` for the page's
        // cast-entry row, the list for the picker. A port that read a *different*
        // address helper for one of them would print an address the television
        // cannot open — AGENTS.md §4.1's first row, one layer over.
        let parts: Vec<&str> = joined.split('/').filter(|p| !p.is_empty()).collect();
        assert_eq!(
            parts,
            advertised.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>()
        );
        for address in advertised.iter() {
            let text = address.as_str().expect("an address string");
            assert!(
                text.parse::<std::net::Ipv4Addr>().is_ok(),
                "the picker offered something that is not an IPv4 address: {text}"
            );
        }
    }

    #[test]
    fn interfaces_uses_current_where_status_uses_interface() {
        crate::pin_test_dir();
        let setting = fresh_setting("status-vs-interfaces");
        setting.set(Key::NetworkInterface, Value::String("en0".into()));
        let build = |store: Arc<TestStore>| -> Arc<Face> {
            Face::new(
                Arc::new(FakeControl::soap_ok()),
                Arc::new(FakeSubscription::new()),
                Arc::new(FakeManagement::new()) as Arc<dyn Management>,
                setting.clone(),
                store as SharedStore,
            )
            .expect("face builds")
            .into()
        };
        let face = build(TestStore::new());
        let standalone = face.answer(&get_api("/api?query=interfaces", LOCAL));
        assert_eq!(standalone.status, 200);
        let doc: serde_json::Value =
            serde_json::from_slice(&standalone.body).expect("a document");
        let keys: Vec<&str> = doc.as_object().expect("object").keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["current", "advertised", "interfaces"]);
        assert_eq!(doc["current"], json!("en0"));
        let status = status_value(&face);
        assert_eq!(status["network"]["interface"], json!("en0"));
        // Same facts, first key spelled differently — the reference writes both this
        // way (`:2016-2020` against `:2146-2151`) and a page reads each. Making them
        // agree is the tidy change that breaks one of them.
        assert!(status["network"].get("current").is_none());
        assert!(doc.get("interface").is_none());
        assert_eq!(
            status["network"]["advertised"],
            doc["advertised"],
            "the two documents must not read different address sets"
        );
        assert_eq!(
            status["network"]["interfaces"],
            doc["interfaces"],
            "nor different picker tables"
        );
        // The picker's rows carry the seven keys the page sorts and colours by, in
        // the reference's order — asserted on whatever this machine contributes, so
        // the claim does not depend on how many interfaces the runner has.
        let rows = doc["interfaces"].as_array().expect("a table");
        for r in rows {
            let keys: Vec<&str> = r
                .as_object()
                .expect("a row")
                .keys()
                .map(|k| k.as_str())
                .collect();
            assert_eq!(
                keys,
                ["name", "ip", "netmask", "default", "usable", "reason", "selected"]
            );
        }
        assert_eq!(
            rows.len(),
            macast_core::net::network_interfaces(&setting).len(),
            "the route filtered the table it was handed"
        );
    }

    #[test]
    fn status_and_interfaces_refuse_a_lan_caller_that_brings_no_token() {
        crate::pin_test_dir();
        let (face, store, subscription) = harness_status("status-gate");
        // The token has to exist before a caller can bring it: `SECRET` is only a
        // credential once it is the one this machine compares against, and a test
        // that handed a token nobody had minted would be asserting that the gate
        // accepts *any* value.
        store_token(&face.setting, Some(SECRET));
        store.put("TransportState", Value::String("PLAYING".into()));
        subscription.push_row(row("uuid:leak", "192.168.1.9:8080", "/cb", 1800));
        for query in ["status", "interfaces"] {
            let refused = face.answer(&get_api(&format!("/api?query={query}"), LAN));
            assert_eq!(refused.status, 403, "{query} answered a LAN caller");
            let body: serde_json::Value =
                serde_json::from_slice(&refused.body).expect("a refusal is still JSON");
            assert_eq!(
                body["message"].as_str(),
                Some(MANAGEMENT_REFUSAL),
                // Compared as a **parsed field**, not as a substring: the writer is
                // `ensure_ascii`, so the bytes on the wire are `\u7ba1\u7406…` and a
                // `contains` over the raw sentence could never match. A first version
                // of this assertion grepped the body and went red on a correct refusal.
                "{query} refused in the wrong sentence: {body}"
            );
            assert_eq!(body["code"], json!(1));
            // The whole document stays shut, not just the flag: what a passer-by
            // must not read here is the interface table, what is playing, who is
            // subscribed, and every address in the play history.
            assert!(body.get("server").is_none() && body.get("media").is_none());
            assert!(
                !text(&refused.body).contains("PLAYING"),
                "the refusal leaked the state table"
            );
            assert!(
                !text(&refused.body).contains("192.168.1.9"),
                "the refusal leaked a subscriber address"
            );
            // Same caller, same address, with the token: served.
            let with_token = face.answer(&get_api(
                &format!("/api?query={query}&token={SECRET}"),
                LAN,
            ));
            assert_eq!(with_token.status, 200, "{query} refused a tokened caller");
        }
        let local = face.answer(&get_api("/api?query=status", LOCAL));
        assert_eq!(local.status, 200);
    }

    #[test]
    fn the_status_read_creates_no_key_that_was_not_there() {
        crate::pin_test_dir();
        // The reference builds this document with `Setting.get(key, default)` in five
        // places, and every one of those takes the default **into the table** for the
        // next `save` to persist (AGENTS.md §4.2's first trap). This route reads the
        // four keys it owns through `peek` instead, so a user who never opened the
        // 插件 or 网页地址投屏 card does not find `Macast_Renderer` in his file
        // because a page polled it. The boundary is not the whole table, and the
        // `Network_Interface` case below is where the boundary is.
        // The boundary is deliberate and narrow: `ApplicationPort`, `Https_enabled`
        // and `Https_Port` still go through the shared readers, because those three
        // are facts the *shell* owns and wrote at startup — pinning the five below
        // is the cost of not silently extending the rule to the whole table.
        let setting = fresh_setting("status-no-write");
        let face: Arc<Face> = Face::new(
            Arc::new(FakeControl::soap_ok()),
            Arc::new(FakeSubscription::new()),
            Arc::new(FakeManagement::new()) as Arc<dyn Management>,
            setting.clone(),
            TestStore::new() as SharedStore,
        )
        .expect("face builds")
        .into();
        for _ in 0..2 {
            status_value(&face);
            face.answer(&get_api("/api?query=interfaces", LOCAL));
        }
        for key in [
            Key::MacastProtocols,
            Key::MacastProtocol,
            Key::MacastRenderer,
            Key::PlayHistory,
        ] {
            assert!(
                setting.peek(key).is_none(),
                "the status read created {} in a settings file that had no such key",
                key.name()
            );
        }
        // `Network_Interface` is deliberately **not** in that list, and the reason is
        // worth keeping because it is the boundary of this claim rather than an
        // oversight: the picker's table comes from
        // `macast_core::net::network_interfaces`, which reads the pin through the
        // shared reader — so after one status read the key exists, exactly as it does
        // in Python (`Setting.get_network_interface()` is `get(..., '')`). The four
        // keys above are the ones this route reads for itself; extending the rule to
        // the whole table would be a change to SSDP, the announcer and the relay in
        // the middle of a status-port round, and it is #14's successor's decision, not
        // this test's.
        assert!(
            setting.peek(Key::NetworkInterface).is_some(),
            "the picker's own reader stopped creating its key — if that changed on \
             purpose, this route's claim above about who creates what needs re-reading"
        );
    }

    #[test]
    fn py_truthy_is_the_rule_behind_or_not_a_guess_about_it() {
        crate::pin_test_dir();
        for (value, want) in [
            (json!(null), false),
            (json!(false), false),
            (json!(0), false),
            (json!(0.0), false),
            (json!(""), false),
            (json!([]), false),
            (json!({}), false),
            (json!(true), true),
            (json!(1), true),
            (json!("-1"), true),
            (json!("0"), true),
            (json!([""]), true),
            (json!([0]), true),
            (json!({"a": 0}), true),
        ] {
            assert_eq!(py_truthy(&value), want, "{value} truthiness");
        }
    }
}
