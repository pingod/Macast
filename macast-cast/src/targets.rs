//! Which renderers answer an SSDP `M-SEARCH`, and what we keep about them.
//!
//! Python counterpart: `macast/plugins/renderer/cast_local_file.py` — the
//! discovery block (`:1085-1247`), the two caches (`:1001-1005`), the card
//! document (`:1622-1665`), and the two entry points the settings page polls
//! (`:2895-2914`). Unlike [`crate::mdns`], this module has **no third-party layer**
//! inside it: SSDP is a spec, `M-SEARCH` is six header lines, and the device
//! description is XML we already know how to read ([`macast_upnp::scpd`]). The
//! LGPL obligations recorded in `mdns.rs`'s header belong to zeroconf, not here.
//!
//! ## Why the browser is not enough
//!
//! [`crate::mdns::discover_cast`] answers "which Chromecasts are on 5353". This module
//! answers the other half of the dropdown: "which TVs speak DLNA", which no
//! amount of mDNS will tell you, because a 2016 Philips does not announce
//! itself over DNS-SD. Both answers land in one card, and the card is what the
//! user reads when the first answer is empty — which is why [`crate::mdns`] hands its
//! "nothing found" wording to this module (`mdns.rs:1535`) rather than
//! inventing one two layers away from the sentence.
//!
//! ## Layering, inside the module
//!
//! * the **request and the sweep** — [`ssdp_request`], [`locations_in`],
//!   [`ssdp_search`], behind the [`SsdpWire`] / [`Searcher`] seams;
//! * the **reading of one device** — [`parse_description`] (pure, no syscall),
//!   behind [`Fetcher`] and [`Describer`];
//! * [`discover_renderers`] — the sweep plus the descriptions, deduplicated by
//!   host;
//! * [`Table`] — the two caches and their two latches, i.e. the Python module
//!   globals' only legal Rust home;
//! * [`Table::cast_target_state`] and friends — the document the page polls.
//!
//! Every seam follows the same house shape as [`crate::resolve`]'s
//! `fetch_page(page_url, opener)` and [`crate::relay`]'s ffmpeg-injection
//! seams: the pure decision lives in the function, the syscall arrives as an
//! argument. That is not decoration. `parse_description` is where every quirk
//! below is decided, and none of them can be asked about through a real socket.
//!
//! ## Quirks transcribed on purpose
//!
//! Each of these reads like a bug and is kept because the Python behaves this
//! way and a device in the field depends on it:
//!
//! * **port `0` is "no opinion".** `_port_suffix` (`:1123`) asks
//!   `if parts.port`, and `0` is falsy — so a description that names port zero
//!   gets the scheme's own default like a description that names no port at
//!   all. A literal `:0` in a control URL would send the SOAP to nothing.
//! * **`startswith('http')`, not `startswith('http://')`.** Both in
//!   `_ssdp_search`'s Location filter (`:1112`) and in `parse_description`'s
//!   branch (`:1181`). `httx://` would be believed; the far more common case is
//!   a Location header that simply *starts with* `http` and is not a scheme, and
//!   the answer there is "we try to read it and fail quietly".
//! * **the AVTransport match is case-sensitive.** `_local_name` lowercases the
//!   *tag*, never the text, so `:avtransport:1` in a `<serviceType>` is a device
//!   we do not add. That is upstream's rule, and the fix for it is not ours.
//! * **`port` is computed *before* the host rewrite.** `:1161-1168`: a device
//!   that describes itself as `http://127.0.0.1:8080/upnp/...` keeps `:8080`
//!   when the host becomes the answering IP. Rewriting first would put its SOAP
//!   on port 80.
//! * **the first `<service>` wins even when it has nothing to say.** `:1170-1180`
//!   `break`s on the first service whose `serviceType` ends `:AVTransport:1`, and
//!   `if not control: return None` then rejects the whole device — so a renderer
//!   that lists a second, complete AVTransport service is still rejected. Trying
//!   the later one would be a behaviour change wearing a bug-fix's clothes.
//!
//! ## Deliberate deviations (and why each one is worth the divergence)
//!
//! * **A bad `base_url` drops one renderer instead of aborting the sweep.**
//!   Python's `describe_renderer` (`:1196`) calls `parse_description` in its
//!   `return` (`:1204`), *outside* its `try/except Exception` (`:1197-1203`). A
//!   `ValueError` from `urlsplit(base_url)` therefore escapes into
//!   `discover_renderers`, whose loop (`:1214-1215`) has no `try` either — one
//!   malformed LOCATION kills every
//!   device found in that sweep, prints a traceback into `LocalFileCaster.log`,
//!   and leaves the cache untouched. Here it costs one row and logs which URL.
//!   The comment in `parse_description`'s error arm says so at the point of use.
//! * **The same unguarded split on the *answer* also stops costing the sweep.**
//!   `:1222` reads `host = urllib.parse.urlsplit(control).hostname or peer` — the
//!   control URL `parse_description` just returned, re-split outside a `try`:
//!   `parse_description`'s (`:1147`) wraps only `ET.fromstring`, `describe_renderer`'s
//!   (`:1197-1203`) only the fetch, and `discover_renderers`' loop only the `peer in
//!   ours` question. So one control URL `urlsplit` chokes on leaves the loop and
//!   every reply still unread in both `ST` passes is lost. Here the loop continues
//!   and one DEBUG line names the URL. Reached through the `Describer` seam, not
//!   by a real description — see `targets_tests::j_an_unreadable_control_url_is_skipped_with_a_debug_line`.
//! * **The sweep failure is a sentence, not a traceback.** Python lets the
//!   exception leave the daemon thread. Here it is one `log.error` that names
//!   the error *and* the next step (AGENTS.md「每一条失败文案必须给出下一步」).
//! * **A thread that will not spawn does not leave the latch down.** Python's
//!   `finally` (`:1080-1082`) always clears it because the thread body ran. If
//!   `thread::Builder::spawn` itself fails there is no body and no `finally`, so
//!   `Table::start` resets the latch by hand — a stuck `searching` flag is the
//!   spinner that never stops (the shell's `cast_targets` makes the same argument
//!   about a *missing* key, and `scripts/verify_rust_receiver.sh` §19 asks for
//!   both: the keys present, and consecutive refreshes sharing the latch).
//! * **XML bytes are read lossily.** `Dom::parse` takes `&str`, `ET.fromstring`
//!   takes bytes and trusts the `<?xml encoding?>` declaration. A description
//!   whose declared encoding lies about its bytes therefore parses here and is
//!   refused there — refusal is `None` in both languages, so the visible
//!   difference is only which devices survive a lying header.
//! * **`start_target_search` starts *both* sweeps.** Python's
//!   `bool(start_search() or start_renderer_search())` (`:1652`) short-circuits:
//!   when the mDNS sweep starts, the SSDP sweep never does. With a populated
//!   Cast cache — i.e. every home with one Chromecast and one TV — 「重新搜索」
//!   can therefore never refresh the DLNA list, while both docstrings say
//!   "ask for both kinds of device". This is a defect in the reference, not in
//!   its description, and it is reported rather than copied.
//! * **No `IP_MULTICAST_IF`.** `macast/screen_mirror.py`'s search pins the
//!   outgoing interface and AGENTS.md §4.2 explains why; `_ssdp_search` does
//!   not, so this module does not either. It is a known gap shared with the
//!   reference, not an oversight of the port.
//! * **`target_by_id` hands back the whole row.** Python returns
//!   `(kind, name, address)` (`:1664`); the Rust [`TargetItem`] carries `id`
//!   too, because its caller — `src-tauri/src/host.rs`'s device arm of
//!   `cast_resolved` — echoes the id back into the answer it sends the page, and
//!   rebuilding it from a `kind` and an `address` there would be a second owner
//!   of the id format.
//!
//! ## Two guards the reference has that this module has no branch for
//!
//! Python's `if root is None` (`:1151`) is defensive against a parser that hands
//! back nothing for a well-formed document; [`Dom`] either gives a root or an
//! error, so there is no arm here that could take that branch. The same is true of
//! the `isinstance(tag, str)` guard in `_local_name` (`:1120`) — comments and
//! processing instructions are nodes there, never nodes in [`macast_upnp::scpd`].
//! "Nothing can fail" is not "covered": per `docs/Rust-Port-Design.md` §13 both
//! are recorded as absent rather than papered over with an assertion that cannot
//! fail.
//!
//! A third item used to sit in this list — `discover_renderers`' re-split of the
//! control URL, called unreachable by construction because `parse_description`
//! only ever emits an absolute URL with a host it chose. **That was false**, twice
//! over: the branch is reachable through the `Describer` trait (which exists so
//! that other answers can be handed back), and Python's answer to the same bytes
//! is not "skip it" but "lose the sweep". It is now in the deviation list above
//! and has a witness in `targets_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use macast_core::logging::Logger;
use macast_core::setting::Setting;
use macast_core::url::{host_is_loopback, py_urlsplit, PySplit};
use macast_upnp::scpd::Dom;
use serde_json::{json, Value};

use crate::mdns::{discover_cast, CastDevice};

// ---------------------------------------------------------------------------
// Constants — the values the request, the timeout and the card are built from
// ---------------------------------------------------------------------------

/// `SSDP_ADDR` / `SSDP_PORT` in the plugin (`:103-104`), which are themselves the
/// ones in `macast/ssdp.py`. Read from [`macast_upnp`] rather than re-literalised
/// so that the announcement side and the client side can never disagree about
/// the multicast group (AGENTS.md §4.8's "两处答案不一致比没有数字更糟").
pub use macast_upnp::{SSDP_ADDR, SSDP_PORT};

/// `DLNA_SERVICE` (`:105`) — the plugin's own name for the AVTransport type.
pub const DLNA_SERVICE: &str = macast_upnp::AVTRANSPORT_URN;

/// `DLNA_SEARCH_TARGETS` (`:106`):
/// `(UPNP_DEVICE, DLNA_SERVICE)`. Two searches, in this order, and the order is
/// load-bearing for the *dedup* result: a renderer that answers both is written
/// into `seen` twice with the same host key, so the second write wins, and both
/// writes carry the same name and control URL. It is load-bearing for timing
/// too — the sweep is serial, so `timeout` is per search, not for the whole
/// discovery (Python: `for st in DLNA_SEARCH_TARGETS: … _ssdp_search(st, timeout)`).
pub const DLNA_SEARCH_TARGETS: [&str; 2] = [macast_upnp::UPNP_DEVICE, DLNA_SERVICE];

/// The suffix `parse_description` matches `<serviceType>` text against (`:1176`).
/// Case-sensitive, as the reference is — see the module header.
pub const AVTRANSPORT_SUFFIX: &str = ":AVTransport:1";

/// `sock.recvfrom(4096)` (`:1102`).
pub const SSDP_READ_MAX: usize = 4096;

/// `sock.settimeout(0.4)` (`:1096`) — the per-receive budget that turns "nobody
/// answered this instant" into a `continue` rather than an error.
pub const SSDP_RECV_TIMEOUT: Duration = Duration::from_millis(400);

/// `response.read(512 * 1024)` (`:1200`). A device description is a few
/// kilobytes; the cap is for the ones that are not.
pub const DESCRIPTION_MAX_BYTES: usize = 512 * 1024;

/// `discover_renderers(timeout=3.0)` (`:1207`) and `describe_renderer(…, timeout=3.0)`
/// (`:1196`): the same number in two places, so it is one constant.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);

/// The logger both sweeps write into: `logsplit.claim()` routes this name to
/// `logs/LocalFileCaster.log`, and `mdns.rs:170` uses the identical string for
/// the identical reason — one file per plugin, so a chatty discovery cannot push
/// the protocol's own lines out of the core log (§4.10b).
pub const LOGGER_NAME: &str = "LocalFileCaster";

/// `threading.Thread(name=…)` for the two sweeps (`:1068`, `:1245`).
pub const CAST_SEARCH_THREAD: &str = "CAST_FILE_SEARCH";
pub const DLNA_SEARCH_THREAD: &str = "CAST_FILE_DLNA_SEARCH";

// ---------------------------------------------------------------------------
// Layer: the M-SEARCH, and reading its replies
// ---------------------------------------------------------------------------

/// One round trip of the wire, as two questions.
///
/// Split this way because `_ssdp_search` (`:1085-1116`) interleaves them: it
/// sends once, then receives until *its own* deadline, and the only thing that
/// makes a fake adequate is answering those two questions in that order.
pub trait SsdpWire {
    /// `sock.sendto(request.encode('ascii'), (SSDP_ADDR, SSDP_PORT))`.
    ///
    /// **No `try`.** In Python a send failure leaves `_ssdp_search` and the
    /// traceback lands in the daemon thread; this returns the error for
    /// [`ssdp_search`] to propagate, which is the same claim.
    fn send_to(&mut self, request: &[u8]) -> io::Result<()>;

    /// `sock.recvfrom(4096)`, with the 0.4-second timeout folded in:
    /// `Ok(None)` means "timed out, keep waiting" (`except socket.timeout:
    /// continue`), `Err(e)` means "the socket is done" (`except OSError: break`).
    /// The `String` is `peer[0]` — the answering *address*, which is the whole
    /// point of this trait: `Location:` is a hint, the peer is a fact.
    fn recv_from(&mut self) -> io::Result<Option<(Vec<u8>, String)>>;
}

/// The request bytes: `'\r\n'.join([...])` from `:1087-1093`, verbatim.
///
/// Seven items, so the last two are the empty lines that terminate the header
/// block; dropping one gives a request that a TV parses as `ST: urn:…\r\n\r\n`
/// glued to nothing and answers with silence. Python encodes `.encode('ascii')`
/// — unreachable here, because both `ST` values ([`DLNA_SEARCH_TARGETS`]) are
/// ASCII constants, so there is no `str` that could fail to encode.
pub fn ssdp_request(st: &str) -> Vec<u8> {
    [
        "M-SEARCH * HTTP/1.1".to_string(),
        format!("HOST: {SSDP_ADDR}:{SSDP_PORT}"),
        "MAN: \"ssdp:discover\"".to_string(),
        "MX: 2".to_string(),
        format!("ST: {st}"),
        String::new(),
        String::new(),
    ]
    .join("\r\n")
    .into_bytes()
}

/// Every `Location:` in one reply, paired with the address that sent it.
///
/// `:1107-1113`. Three things here are not interchangeable with the obvious Rust:
///
/// * the split is `str.splitlines()`, so `crate::resolve::py_splitlines` and
///   **not** [`str::lines`] — that helper's own doc carries the boundary set;
/// * `line.partition(':')` splits at the **first** colon and yields
///   `('', '', line)` when there is none, so a bare `LOCATION` line with no
///   colon is skipped on the value test, not the name test;
/// * the filter is `startswith('http')`, so `httpX://` passes and `ftp://` does
///   not. Kept, with the reason in the module header.
pub fn locations_in(reply: &str, peer: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in crate::resolve::py_splitlines(reply) {
        // `name, _, value = line.partition(':')`
        let (name, value) = match line.find(':') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, ""),
        };
        if crate::resolve::py_strip(name).to_lowercase().as_str() != "location" {
            continue;
        }
        let value = crate::resolve::py_strip(value).to_string();
        if value.starts_with("http") {
            out.push((value, peer.to_string()));
        }
    }
    out
}

/// Sweep for one `ST` until `timeout` runs out. `:1085-1116`.
///
/// The deadline is computed **after** the send (`:1098-1099`), so a slow send eats
/// the receive window rather than the whole search; `Instant` reproduces
/// `time.time()` for that purpose without pretending to measure wall clock.
/// Replies are deduped and sorted because Python returns `sorted(found)` over a
/// set of `(location, ip)` tuples (`:1113`, returned at `:1116`) — a renderer that
/// answers both M-SEARCHes is asked twice, and it is this `set` that makes the
/// second answer free.
pub fn ssdp_search(
    wire: &mut dyn SsdpWire,
    st: &str,
    timeout: Duration,
    log: &Logger,
) -> io::Result<Vec<(String, String)>> {
    wire.send_to(&ssdp_request(st))?;
    let mut found: BTreeSet<(String, String)> = BTreeSet::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match wire.recv_from() {
            Ok(Some((bytes, peer))) => {
                // `data.decode('utf-8', 'replace')` — lossy on both sides, and
                // deliberately: a GBK `<friendlyName>` is someone else's problem
                // (this read is of the *header block*, not the description).
                let reply = String::from_utf8_lossy(&bytes);
                for (location, peer) in locations_in(&reply, &peer) {
                    found.insert((location, peer));
                }
            }
            Ok(None) => continue,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                continue
            }
            Err(e) => {
                // `except OSError: break` — keep what we heard, stop listening.
                // Not silent: this is the one row that distinguishes "nobody is
                // home" from "our socket died", and §4.2 says an empty answer
                // must say *which* empty it is.
                log.debug(&format!("SSDP receive stopped after {}: {e}", found.len()));
                break;
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// The real socket behind [`RealSearcher`]: a plain bound UDP socket.
///
/// **`std::net`, not `socket2`** — unlike [`crate::mdns`], which needs socket2
/// to set `SO_REUSEPORT` *before* binding 5353. This socket binds port `0`
/// ephemerally and never joins a group: it sends to `239.255.255.250:1900` as
/// ordinary unicast-shaped UDP, exactly as `socket.socket(AF_INET, SOCK_DGRAM)`
/// + `sendto` does in Python. No `SO_REUSE*`, no `IP_MULTICAST_IF`
/// (see the deviation in the module header).
pub struct UdpSsdpWire {
    sock: UdpSocket,
    group: SocketAddr,
}

impl UdpSsdpWire {
    /// Create the socket, set the 0.4-second read timeout, resolve the group.
    ///
    /// `sock.settimeout(0.4)` sits **outside** Python's `try` (the `try:` is
    /// `:1097`, the settimeout `:1096`), so a socket that cannot be built is an
    /// error out of the thread — the same
    /// shape: `open()` returns `io::Result`, and the first thing the caller does
    /// with it is propagate.
    pub fn open() -> io::Result<Self> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.set_read_timeout(Some(SSDP_RECV_TIMEOUT))?;
        Ok(Self {
            sock,
            group: ssdp_destination()?,
        })
    }
}

/// `239.255.255.250:1900` as a `SocketAddr`.
///
/// Parsing it here rather than `unwrap()`ing at the call site, because the
/// failure mode is worth a sentence: if [`SSDP_ADDR`] ever stops being an IPv4
/// literal, the bug is in a constant three crates away from this one, and a
/// panic with the address in its message says so while `unwrap()` says nothing.
fn ssdp_destination() -> io::Result<SocketAddr> {
    format!("{SSDP_ADDR}:{SSDP_PORT}")
        .parse::<SocketAddr>()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{SSDP_ADDR} is not IPv4: {e}")))
}

impl SsdpWire for UdpSsdpWire {
    fn send_to(&mut self, request: &[u8]) -> io::Result<()> {
        self.sock.send_to(request, self.group).map(|_| ())
    }

    fn recv_from(&mut self) -> io::Result<Option<(Vec<u8>, String)>> {
        let mut buf = vec![0u8; SSDP_READ_MAX];
        match self.sock.recv_from(&mut buf) {
            // `peer[0]` — the IP alone, never `ip:port`. The port belongs to the
            // ephemeral reply, and a control URL built from it would die with it.
            Ok((length, peer)) => {
                buf.truncate(length);
                Ok(Some((buf, peer.ip().to_string())))
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

/// The sweep as one call, so [`discover_renderers`] can be driven without a
/// network. One socket per `ST`, because that is what Python does: the socket is
/// created inside `_ssdp_search`, so each search gets a fresh ephemeral port and
/// a fresh 0.4-second receive timeout.
pub trait Searcher: Send + Sync {
    fn search(&self, st: &str, timeout: Duration, log: &Logger) -> io::Result<Vec<(String, String)>>;
}

/// The searcher that speaks on the wire.
pub struct RealSearcher;

impl Searcher for RealSearcher {
    fn search(&self, st: &str, timeout: Duration, log: &Logger) -> io::Result<Vec<(String, String)>> {
        let mut wire = UdpSsdpWire::open()?;
        ssdp_search(&mut wire, st, timeout, log)
    }
}

// ---------------------------------------------------------------------------
// Layer: reading one device's description
// ---------------------------------------------------------------------------

/// `_local_name` (`:1119-1120`): `tag.rpartition('}')[2].lower()` for a `str` tag.
///
/// The `isinstance(tag, str)` half has no counterpart — see the module header's
/// guards this module has no branch for: [`Dom`] never stores a comment or PI
/// node, so there is no non-string tag to skip.
pub fn local_name(tag: &str) -> String {
    tag.rsplit('}').next().unwrap_or("").to_lowercase()
}

/// `_port_suffix` (`:1123-1133`), including its docstring's reason: a device
/// whose `<ControlURL>` names no port is common, one whose port we cannot read
/// is rarer, and **dropping the whole renderer is worse than guessing the
/// scheme's own port**.
///
/// `default` is `""` for the base URL and the base's suffix for the control URL
/// — that is the second half of `:1187-1192`, where an absolute control URL on a
/// different host keeps *our* port unless it names its own.
///
/// Swallows the error the way `except ValueError: return default` does. This is
/// not defensive coding: `PySplit::port()` errors on `:abc` and on `:99999`, and
/// both of those are a device we should still try to talk to.
pub fn port_suffix(parts: &PySplit, default: &str) -> String {
    match parts.port() {
        Ok(Some(p)) if p != 0 => format!(":{p}"),
        _ => default.to_string(),
    }
}

/// `parsed.path.rsplit('/', 1)[0]` (`:1184`) — the directory of a path, with
/// Python's two edges: no slash yields the whole path, and a trailing slash
/// drops that slash (so `/a/b/` prefixes as `/a/b` + `/x`).
fn path_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => path,
    }
}

/// `first_text(tag)` (`:1154-1158`): the first node whose local name matches,
/// trimmed — **returning at the first match even when it is empty**.
///
/// `or peer_ip` in the caller is what makes an empty `<friendlyName/>` fall back
/// to the IP; a *second* `<friendlyName>` with a name in it never gets read,
/// because this loop returns. Kept: lxml's `.iter()` is document order and so is
/// [`Dom::iter_all`], so the two agree on *which* one is first.
fn first_text(dom: &Dom, root: usize, tag: &str) -> String {
    for node in dom.iter_all(root) {
        if local_name(dom.tag(node)) == tag {
            return dom.text(node).unwrap_or("").trim().to_string();
        }
    }
    String::new()
}

/// One device description, read into `(friendly name, absolute AVTransport
/// control URL)` — or `None`, which means "this device is not on the list".
///
/// `parse_description` (`:1136-1193`). The docstring's own framing is the design:
/// *namespaces are matched by local name* (a Samsung and a Philips disagree about
/// prefixes in ways that a Clark-notation match would treat as absence), and
/// *the control URL is rewritten to the address that actually answered*, because
/// a device that describes itself on loopback, or forgets its port, is normal.
///
/// `raw` is the bytes as fetched; `base_url` is where they came from; `peer_ip`
/// is the address that answered the M-SEARCH, not a name.
pub fn parse_description(
    raw: &[u8],
    base_url: &str,
    peer_ip: &str,
    log: &Logger,
) -> Option<(String, String)> {
    // `ET.fromstring(raw)` on bytes vs. `Dom::parse` on `&str`: see the header.
    let text = String::from_utf8_lossy(raw);
    let dom = match Dom::parse(&text) {
        Ok(dom) => dom,
        Err(e) => {
            // Python returns `None` here without a word. One debug line is added
            // because "the TV is in the list but has no name" is otherwise
            // indistinguishable from "the TV never answered".
            log.debug(&format!("cannot parse {base_url}: {e}"));
            return None;
        }
    };
    let root = dom.root();

    // `first_text('friendlyname') or peer_ip` — an empty name is the IP.
    let name = {
        let found = first_text(&dom, root, "friendlyname");
        if found.is_empty() {
            peer_ip.to_string()
        } else {
            found
        }
    };

    let parsed = match py_urlsplit(base_url, "", true) {
        Ok(parsed) => parsed,
        Err(e) => {
            // Deviation, on purpose: Python lets this leave `describe_renderer`
            // and abort the entire sweep (header). One bad LOCATION costs one row.
            log.error(&format!("unreadable device URL {base_url}: {e} — this renderer is skipped"));
            return None;
        }
    };
    // `parsed.hostname or peer_ip`: an empty netloc is falsy, and `hostname()`
    // gives `None` for it and `Some("")` for `http:///x`. Both mean "no host".
    let mut host = match parsed.hostname() {
        Some(h) if !h.is_empty() => h,
        _ => peer_ip.to_string(),
    };
    // **Before** the rewrite — see the header.
    let port = port_suffix(&parsed, "");

    // `ipaddress.ip_address(host).is_loopback or host == '0.0.0.0'`, with
    // `except ValueError: pass` for the case where host is a *name*.
    // `host_is_loopback` returns `None` for exactly that: "a hostname — keep it,
    // DNS is the LAN's job".
    if host_is_loopback(&host) == Some(true) || host == "0.0.0.0" {
        host = peer_ip.to_string();
    }

    // The service scan. `values` is a dict built per `<service>`, so the **last**
    // same-named child wins — that is Python, and `BTreeMap::insert` agrees.
    let mut control = String::new();
    for node in dom.iter_all(root) {
        if local_name(dom.tag(node)) != "service" {
            continue;
        }
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        for child in dom.children(node) {
            values.insert(
                local_name(dom.tag(*child)),
                dom.text(*child).unwrap_or("").trim().to_string(),
            );
        }
        if values
            .get("servicetype")
            .map(|t| t.ends_with(AVTRANSPORT_SUFFIX))
            .unwrap_or(false)
        {
            control = values.get("controlurl").cloned().unwrap_or_default();
            break;
        }
    }
    if control.is_empty() {
        return None;
    }

    if !control.starts_with("http") {
        // Relative to *us*, not to the description's own URL: same origin, then
        // either root or the base path's directory (`:1181-1186`).
        let same_origin = format!("{}://{}{}", parsed.scheme, host, port);
        let prefix = if control.starts_with('/') {
            same_origin
        } else {
            format!("{same_origin}{}", path_dir(&parsed.path))
        };
        control = if control.starts_with('/') {
            format!("{prefix}{control}")
        } else {
            format!("{prefix}/{control}")
        };
    } else {
        // Absolute, but possibly naming a host we cannot reach — a device that
        // says `http://mediarenderer.local:49152/…` from an address we just
        // talked to. Trust the address, keep its scheme and path, and let the
        // base's port act as the default (`:1187-1192`).
        let described = match py_urlsplit(&control, "", true) {
            Ok(described) => described,
            Err(e) => {
                log.error(&format!("unreadable control URL {control}: {e} — this renderer is skipped"));
                return None;
            }
        };
        // `described.hostname != host`: `None != Some` is a rewrite in Python too,
        // because `urlsplit('http:///x').hostname` is `''`/`None` and `'' != ip`.
        if described.hostname().as_deref() != Some(host.as_str()) {
            control = format!(
                "{}://{}{}{}",
                described.scheme,
                host,
                port_suffix(&described, &port),
                described.path
            );
        }
    }

    Some((name, control))
}

/// The one syscall `parse_description` does not make: fetch the description.
///
/// `Fetcher` exists so that the *decision* (what a bad XML body, a 404, and a
/// 512 KiB truncation each answer) is testable without a listener; the
/// `discover_renderers` rows below do use a real one, because "the socket the
/// device answered on is not the socket its description lives on" is only
/// visible across a real port.
pub trait Fetcher: Send + Sync {
    fn read(&self, url: &str, max_bytes: usize) -> io::Result<Vec<u8>>;
}

/// The agent the description is fetched with: **no proxy**, once, instead of
/// inheriting whatever this machine exports.
///
/// Same decision as `relay.rs:1165` and Python's `build_opener(ProxyHandler({}))`
/// at `:1198`, and it has one owner (`resolve::no_proxy_agent`); what differs here
/// is only the budget — 3 seconds, because `describe_renderer`'s default is 3.0.
fn description_agent() -> ureq::Agent {
    crate::resolve::no_proxy_agent(SEARCH_TIMEOUT)
}

/// The fetcher that speaks HTTP.
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn read(&self, url: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        let response = description_agent()
            .get(url)
            .call()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut body = response.into_body();
        let mut raw = Vec::new();
        // `response.read(512 * 1024)` truncates without erroring, so a
        // over-long description is a *short* one here — and a short description
        // whose `<service>` block got cut is a device that is not on the list.
        Read::take(body.as_reader(), max_bytes as u64)
            .read_to_end(&mut raw)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        Ok(raw)
    }
}

/// `describe_renderer` (`:1196-1204`): read, then decide.
///
/// The debug line is verbatim Python (`cannot read %s: %s`) because it is the
/// only trace a device that 404s its own description leaves, and the log file is
/// what AGENTS.md §5 says to read first. `parse_description` is *outside* the
/// `try` in Python; here the difference is recorded in that call's error arm.
pub fn describe_renderer(
    fetcher: &dyn Fetcher,
    url: &str,
    peer_ip: &str,
    log: &Logger,
) -> Option<(String, String)> {
    let raw = match fetcher.read(url, DESCRIPTION_MAX_BYTES) {
        Ok(raw) => raw,
        Err(e) => {
            log.debug(&format!("cannot read {url}: {e}"));
            return None;
        }
    };
    parse_description(&raw, url, peer_ip, log)
}

/// [`describe_renderer`] behind the seam [`discover_renderers`] asks through.
pub trait Describer: Send + Sync {
    fn describe(&self, url: &str, peer_ip: &str, log: &Logger) -> Option<(String, String)>;
}

/// The describer that fetches over HTTP.
pub struct HttpDescriber;

impl Describer for HttpDescriber {
    fn describe(&self, url: &str, peer_ip: &str, log: &Logger) -> Option<(String, String)> {
        describe_renderer(&HttpFetcher, url, peer_ip, log)
    }
}

/// One DLNA renderer as the card knows it: `(name, control URL, host)`, in
/// Python's tuple order (`:1223`) so that the derived [`Ord`] *is* `sorted()` —
/// name first, then control URL, then host, exactly as Python compares tuples.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DlnaDevice {
    pub name: String,
    pub control_url: String,
    pub host: String,
}

/// Search twice, describe what answered, dedupe by host, sort.
/// `discover_renderers` (`:1207-1224`).
///
/// Three rules that are not decoration:
///
/// * **`ours` is [`macast_core::net::advertised_hosts`]** — `Setting.get_advertisable_ip()`,
///   the same set [`crate::mdns::drop_ours`] filters Cast devices with. Answering our own
///   SSDP would put *this Mac* in the dropdown as a TV, and the user would then
///   cast to the renderer they are already using (§4.2's "两处答案不一致").
/// * **The dedup key is the control URL's host**, not the name and not the peer:
///   a device that answers both `ST`s lands once, and a device whose description
///   lies about its host lands under the address we rewrote it to (`:1216-1223`).
/// * **No `try` around the loop** in Python, so this loop's two unguarded splits
///   are deviations with a name in the module header: the request side (a bad
///   `base_url`) and the answer side (a control URL [`py_urlsplit`] chokes on).
///   Each costs one row here and the whole sweep in Python.
pub fn discover_renderers(
    searcher: &dyn Searcher,
    describer: &dyn Describer,
    setting: &Setting,
    timeout: Duration,
    log: &Logger,
) -> io::Result<Vec<DlnaDevice>> {
    let ours = macast_core::net::advertised_hosts(setting);
    let mut seen: BTreeMap<String, DlnaDevice> = BTreeMap::new();
    for st in DLNA_SEARCH_TARGETS {
        for (location, peer) in searcher.search(st, timeout, log)? {
            if ours.contains(&peer) {
                continue;
            }
            let Some((name, control)) = describer.describe(&location, &peer, log) else {
                continue;
            };
            // The deviation the module header registers: Python re-splits this
            // same URL unguarded (`:1222`), so one control URL `urlsplit` chokes on
            // leaves the loop and every remaining SSDP reply in both `ST` passes is
            // lost. Here it costs one row and one DEBUG line naming the URL.
            // `parse_description` on its own would never hand back such a URL — it
            // builds absolute ones — but the `Describer` is a trait precisely so
            // that other answers can arrive, and that is the shape a cached ledger
            // row or a second describer would take.
            let host = match py_urlsplit(&control, "", true) {
                Ok(parts) => match parts.hostname() {
                    Some(h) if !h.is_empty() => h,
                    _ => peer.clone(),
                },
                Err(e) => {
                    log.debug(&format!("control URL cannot be re-read: {control}: {e}"));
                    continue;
                }
            };
            seen.insert(host.clone(), DlnaDevice { name, control_url: control, host });
        }
    }
    // Python returns `sorted(seen.values())`; the `BTreeMap` is keyed by host, so
    // its order is *not* the card's order. Sort explicitly.
    let mut out: Vec<DlnaDevice> = seen.into_values().collect();
    out.sort();
    Ok(out)
}

/// The whole DLNA sweep with the real seams, as one call from [`Table`].
///
/// A failed sweep is an empty list **and one error line that names the next
/// step**. Python's version raises out of the daemon thread, which prints a
/// traceback nobody reads and leaves the card saying "no devices" — so the
/// failure exists in both languages; only the sentence is ours
/// (AGENTS.md「每一条失败文案必须给出下一步」).
pub fn sweep_renderers(setting: &Arc<Setting>, timeout: Duration) -> Vec<DlnaDevice> {
    let log = Logger::named(LOGGER_NAME);
    match discover_renderers(&RealSearcher, &HttpDescriber, setting, timeout, &log) {
        Ok(devices) => devices,
        Err(e) => {
            log.error(&format!(
                "SSDP discovery failed: {e} — check that the TV is on and in this network, then press 重新搜索 to ask again"
            ));
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Layer: the two caches
// ---------------------------------------------------------------------------

/// Which of the two sweeps a latch belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Cast,
    Dlna,
}

impl Which {
    fn thread_name(self) -> &'static str {
        match self {
            Which::Cast => CAST_SEARCH_THREAD,
            Which::Dlna => DLNA_SEARCH_THREAD,
        }
    }
    fn latch(self, inner: &Inner) -> bool {
        match self {
            Which::Cast => inner.cast_searching,
            Which::Dlna => inner.dlna_searching,
        }
    }
    fn set_latch(self, inner: &mut Inner, value: bool) {
        match self {
            Which::Cast => inner.cast_searching = value,
            Which::Dlna => inner.dlna_searching = value,
        }
    }
}

/// `_devices` / `_dlna_devices` / `_searching` / `_dlna_searching`
/// (`cast_local_file.py:1001-1004`), which are module globals because the plugin
/// *is* a module. Here they are one struct behind one mutex, and the mutex
/// replaces `_search_lock` (`:1005`) rather than sitting next to it: the Python
/// pattern is "take the lock, read the latch, set it, release, start the thread",
/// and that is exactly one critical section.
#[derive(Default)]
struct Inner {
    cast: Vec<CastDevice>,
    dlna: Vec<DlnaDevice>,
    cast_searching: bool,
    dlna_searching: bool,
}

/// The two sweeps as two calls, so [`Table`] can be driven with rows that never
/// touch an interface.
pub trait Sweeps: Send + Sync {
    fn cast(&self, setting: &Arc<Setting>) -> Vec<CastDevice>;
    fn dlna(&self, setting: &Arc<Setting>) -> Vec<DlnaDevice>;
}

/// The sweeps that speak on the wire.
pub struct RealSweeps;

impl Sweeps for RealSweeps {
    fn cast(&self, setting: &Arc<Setting>) -> Vec<CastDevice> {
        discover_cast(setting, SEARCH_TIMEOUT)
    }
    fn dlna(&self, setting: &Arc<Setting>) -> Vec<DlnaDevice> {
        sweep_renderers(setting, SEARCH_TIMEOUT)
    }
}

/// Clears a sweep's latch when the search thread's body ends, by whatever route.
///
/// The guard is constructed **inside the thread closure**, not before `spawn`.
/// `thread::Builder::spawn` *discards* the closure when it returns `Err`, so a
/// guard made outside would have been moved into a thread that never runs and
/// never drops — leaving `searching` true forever, which the page reads as a
/// spinner that never stops. Inside the closure, a panic on the way out still
/// unwinds through this `Drop`, which is Python's `finally:` (`:1080`, `:1241`)
/// in the only form Rust has for it.
struct LatchGuard {
    inner: Arc<Mutex<Inner>>,
    which: Which,
}

impl Drop for LatchGuard {
    fn drop(&mut self) {
        let mut inner = lock_recovering(&self.inner);
        self.which.set_latch(&mut inner, false);
    }
}

/// Lock, recovering from poisoning, deliberately.
///
/// A poison here can only come from a panic inside one of this module's own
/// critical sections, and all four are plain assignments — no parse, no I/O, no
/// index that can fail. Recovering is therefore not sweeping a corrupt state
/// under the rug: the alternative is that one panic anywhere in the process makes
/// every later `target_state()` poll panic, i.e. the settings page stops working
/// because a search thread died weeks ago.
fn lock_recovering<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The two caches and the two latches: the object the settings page's
/// `target_state()` / `target_refresh()` read, and the object the shell's device
/// arm looks an id up in before it pushes (`src-tauri/src/host.rs`'s
/// `cast_resolved` → [`crate::push::push_url_to`]).
///
/// `Clone` is a *handle*, not a copy of the ledger: every clone shares one
/// `Inner`, so the page and the push path cannot hold different answers about
/// "what is out there" (§4.8's oldest lesson, one copy of a fact per owner).
#[derive(Clone)]
pub struct Table {
    inner: Arc<Mutex<Inner>>,
    setting: Arc<Setting>,
    sweeps: Arc<dyn Sweeps>,
    log: Arc<Logger>,
}

impl Table {
    /// A table that will really search.
    pub fn new(setting: Arc<Setting>) -> Self {
        Self::with_sweeps(setting, Arc::new(RealSweeps))
    }

    /// A table whose rows come from `sweeps`: the hermetic seam, and the reason
    /// every cache and card rule below has a witness that does not enumerate this
    /// machine's interfaces (`net.rs`'s three `advertised_*` calls all take a live
    /// snapshot).
    pub fn with_sweeps(setting: Arc<Setting>, sweeps: Arc<dyn Sweeps>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            setting,
            sweeps,
            log: Logger::named(LOGGER_NAME),
        }
    }

    /// `start_search` / `start_renderer_search` (`:1061-1070`, `:1227-1247`):
    /// take the latch or say "already running" by returning `false`.
    ///
    /// `true` means "a search started now", not "devices were found" — which is
    /// what `state['started']` says on the card, and why the distinction survives
    /// into [`Table::target_refresh`].
    fn start(&self, which: Which) -> bool {
        {
            let mut inner = lock_recovering(&self.inner);
            if which.latch(&inner) {
                return false;
            }
            which.set_latch(&mut inner, true);
        }
        let inner = Arc::clone(&self.inner);
        let setting = Arc::clone(&self.setting);
        let sweeps = Arc::clone(&self.sweeps);
        let spawned = thread::Builder::new()
            .name(which.thread_name().to_string())
            .spawn(move || {
                let _guard = LatchGuard {
                    inner: Arc::clone(&inner),
                    which,
                };
                // `if found: _devices = found` (`:1077-1079`). A sweep that found
                // nothing **does not overwrite** what we already knew:
                // 「A device asleep right now is still the one being cast to」
                // — dropping the cache on a missed multicast would empty the
                // dropdown while the TV is still in the living room.
                //
                // Copy-on-write in the Python sense: the whole vector is replaced
                // in one assignment under the lock, and every reader takes a
                // snapshot. Nothing here ever mutates a Vec in place, so a reader
                // on another thread cannot see a half-updated list (§4.2's
                // `RuntimeError: dictionary changed size during iteration` is the
                // same failure with a dict).
                //
                // The store lives **inside** each arm rather than behind one
                // `let found = match …`: the two rows are different types
                // (`Vec<CastDevice>` / `Vec<DlnaDevice>`), so binding them to one
                // name is a type error — and the natural "simplify" of hoisting
                // the `if` out would need a common type that does not exist.
                match which {
                    Which::Cast => {
                        let found = sweeps.cast(&setting);
                        if !found.is_empty() {
                            lock_recovering(&inner).cast = found;
                        }
                    }
                    Which::Dlna => {
                        let found = sweeps.dlna(&setting);
                        if !found.is_empty() {
                            lock_recovering(&inner).dlna = found;
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            // The deviation this exists for: no closure ran, so no `Drop` ran.
            self.log.error(&format!(
                "could not start the device search: {e} — press 重新搜索 to try again"
            ));
            let mut inner = lock_recovering(&self.inner);
            which.set_latch(&mut inner, false);
            return false;
        }
        true
    }

    /// `start_search()` (`:1061`): the mDNS/Chromecast sweep.
    pub fn start_search(&self) -> bool {
        self.start(Which::Cast)
    }

    /// `start_renderer_search()` (`:1227`): the SSDP/DLNA sweep.
    pub fn start_renderer_search(&self) -> bool {
        self.start(Which::Dlna)
    }

    /// `start_target_search()` (`:1650`) — **and a deliberate fix**: Python's
    /// `bool(start_search() or start_renderer_search())` short-circuits, so the
    /// DLNA sweep never runs when the Cast sweep starts. Both are asked here; the
    /// answer is still "True when either one actually started". See the header.
    pub fn start_target_search(&self) -> bool {
        let cast = self.start_search();
        let dlna = self.start_renderer_search();
        cast || dlna
    }

    /// Both caches as a snapshot, with the lock already released.
    ///
    /// Python's `list(_devices)` (`:1631`, `:1634`) is the same idiom: bind a copy
    /// and iterate the copy, so nothing downstream needs the lock — and so a sweep
    /// that finishes mid-card cannot lengthen the list under the reader.
    fn snapshot(&self) -> (Vec<CastDevice>, Vec<DlnaDevice>) {
        let inner = lock_recovering(&self.inner);
        (inner.cast.clone(), inner.dlna.clone())
    }

    /// `cast_target_items()` (`:1622-1636`).
    ///
    /// **Never searches here**: 「a page that polls once a second must not spawn
    /// an mDNS or SSDP round trip per poll」. The search lives in
    /// [`Table::target_state`] and [`Table::target_refresh`], which are the two
    /// routes the page is allowed to poke.
    ///
    /// Ids are `cast:{host}:{port}` and `dlna:{control}`, Cast rows first — the id
    /// is a *lookup key*, and the `:` inside a DLNA URL is why the `dlna:` branch
    /// uses the whole control URL instead of splitting it again.
    pub fn cast_target_items(&self) -> Vec<TargetItem> {
        let (cast, dlna) = self.snapshot();
        let mut items = Vec::with_capacity(cast.len() + dlna.len());
        for device in cast {
            items.push(TargetItem {
                id: format!("cast:{}:{}", device.address, device.port),
                kind: "cast",
                name: device.name,
                address: format!("{}:{}", device.address, device.port),
            });
        }
        for device in dlna {
            items.push(TargetItem {
                id: format!("dlna:{}", device.control_url),
                kind: "dlna",
                name: device.name,
                address: device.control_url,
            });
        }
        items
    }

    /// `cast_target_state()` (`:1639-1647`): the document the card renders.
    ///
    /// **Key order is the answer the page reads.** `serde_json`'s
    /// `preserve_order` feature is on for this crate, so `json!({...})` keeps the
    /// order written below — `items`, `searching`, `known` — which is Python's
    /// dict literal order at `:1641-1647`. **The witness for that order is the
    /// acceptance script**: `scripts/verify_rust_receiver.sh` §19 compares
    /// `json_keys targets` against exactly `items,searching,known`, and asks again
    /// for `items,searching,known,started` after `?refresh=1` — so a reordering,
    /// or a `started` that lands in the middle instead of last
    /// (`cast_local_file.py:2913` assigns it after the document is built), is a
    /// red line and not a style opinion. Nothing in this crate checks it, and the
    /// shell does not either: `cast_targets` prepends 「本机」 to whatever this
    /// returns, so the order has to be right **here** for it to be right there.
    /// The two booleans must be **present** whatever they say — the card renders
    /// them as booleans, and a missing `searching` is a spinner that never ends.
    /// (On a LAN with nothing discoverable `searching` reads `true` on every poll,
    /// because the cold-start kick re-arms a sweep each time the caches are found
    /// empty; that is the reference's behaviour, and §19 asserts the latch rather
    /// than a settle. See `docs/Rust-Port-Design.md` §16.)
    pub fn cast_target_state(&self) -> Value {
        let inner = lock_recovering(&self.inner);
        let items: Vec<Value> = items_from(&inner.cast, &inner.dlna);
        json!({
            "items": items,
            "searching": inner.cast_searching || inner.dlna_searching,
            "known": !inner.cast.is_empty() || !inner.dlna.is_empty(),
        })
    }

    /// `target_state()` (`:2897-2907`): the card, **and the cold-start kick**.
    ///
    /// `if not _devices and not _dlna_devices: start_target_search()` — the first
    /// poll of a fresh run is the only thing that asks for devices, which is why
    /// 「刚打开设置页时下拉是空的」 used to be normal. The guard is dropped before
    /// `start_target_search()` so the cold path cannot take the lock twice (it
    /// takes it again inside `start`, and this is the one place where a re-entrant
    /// `std::sync::Mutex` would deadlock instead of merely looking ugly).
    pub fn target_state(&self) -> Value {
        let cold = {
            let inner = lock_recovering(&self.inner);
            inner.cast.is_empty() && inner.dlna.is_empty()
        };
        if cold {
            self.start_target_search();
        }
        self.cast_target_state()
    }

    /// `target_refresh()` (`:2909-2914`): ask again, then answer.
    ///
    /// `started` is **appended last** — `state['started'] = started` (`:2913`, after
    /// `state = cast_target_state()` at `:2912`) rather than a fourth key in the
    /// `json!`. A card that says `"started": true`
    /// is a card that promises the next poll will look different; `"started":
    /// false` means a sweep is already in flight and the user should keep polling.
    pub fn target_refresh(&self) -> Value {
        let started = self.start_target_search();
        let mut state = self.cast_target_state();
        state["started"] = json!(started);
        state
    }

    /// `target_by_id()` (`:1655-1665`): look an id up **in the live caches**, not
    /// in whatever the caller asserts.
    ///
    /// That is the whole point of the Python comment ("the row the card showed may
    /// be a device that has since left"): a stale id must not become a push to an
    /// address we no longer believe in. Returns the full [`TargetItem`] instead of
    /// Python's `(kind, name, address)`; see the header's deviation list.
    pub fn target_by_id(&self, id: &str) -> Option<TargetItem> {
        self.cast_target_items().into_iter().find(|item| item.id == id)
    }
}

/// One row of the dropdown. `kind` is `&'static str` because the only two values
/// are `"cast"` and `"dlna"` (`:1632`, `:1635`), and a row that carries the kind
/// as a `String` invites a third.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetItem {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    pub address: String,
}

/// The rows, from the two vectors, without going through the mutex. Split out so
/// [`Table::cast_target_state`] and [`Table::cast_target_items`] cannot drift
/// apart into two answers about the same ledger.
fn items_from(cast: &[CastDevice], dlna: &[DlnaDevice]) -> Vec<Value> {
    let mut rows = Vec::with_capacity(cast.len() + dlna.len());
    for device in cast {
        rows.push(json!({
            "id": format!("cast:{}:{}", device.address, device.port),
            "kind": "cast",
            "name": device.name,
            "address": format!("{}:{}", device.address, device.port),
        }));
    }
    for device in dlna {
        rows.push(json!({
            "id": format!("dlna:{}", device.control_url),
            "kind": "dlna",
            "name": device.name,
            "address": device.control_url,
        }));
    }
    rows
}

// ---------------------------------------------------------------------------
// targets_tests module wiring
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "targets_tests.rs"]
mod targets_tests;
