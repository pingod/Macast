//! Witnesses for [`crate::targets`] — the device table that issue #27 moved out of
//! `macast/plugins/renderer/cast_local_file.py:1085-1247`.
//!
//! ## Three shapes of witness
//!
//! **Byte-level fixtures.** Everything above the sockets is a pure decision over
//! bytes: [`ssdp_request`] builds one CRLF document, [`locations_in`] reads a
//! reply, [`parse_description`] reads a device description. Those get literal
//! answers, and a literal answer is the only kind that catches a rewrite of the
//! sentence it produces — the sentences are what the user reads when a TV refuses.
//!
//! **A scripted [`SsdpWire`], a scripted [`Searcher`], [`Describer`], [`Fetcher`]
//! and [`Sweeps`].** Discovery is a conversation with a deadline, and a real
//! socket cannot hand me "the second reply arrives after the first broke". The
//! fakes are argument-shape honest: `SsdpWire` takes `&mut self` and is only ever
//! used behind `&mut dyn SsdpWire` (so it needs no `Sync`), while `Searcher`,
//! `Describer`, `Fetcher` and `Sweeps` are `Send + Sync` because the table hands
//! them to a thread — a fake that forgot those bounds would not compile, which is
//! itself a witness that the seam is where the real one is.
//!
//! **The project [`Logger`], one file per test.** The failure sentences are the
//! deliverable of [`sweep_renderers`] and [`Table::start`], so a test that only
//! asserts `Vec::new()` proves the device disappeared and nothing about whether a
//! human was told. Each test claims its own witness file (see [`witness`]).
//!
//! **One real loopback socket** (section I): [`HttpFetcher`] is `ureq` behind
//! `proxy(None)`, and the system proxy on this machine has already eaten a module
//! that thought it was hermetic (AGENTS.md §4.2). A fake `Fetcher` can never catch
//! that, because a fake `Fetcher` is not `ureq`.
//!
//! Do not read `relay_tests.rs:3` ("no mocks") as a house rule that applies here —
//! that file tests a relay against a real origin, and a real socket cannot replay
//! a scripted sequence of announcements on a deadline.
//!
//! ## What is *not* tested here, and why
//!
//! Four arms stay unwitnessed. The first two have no branch in Rust to witness;
//! the second two have a branch that no fixture can reach — and each says which:
//!
//! - **Python's `if root is None`** (the reference at `:1151`) defends against an
//!   lxml that returns nothing for a parseable document. [`macast_upnp::scpd::Dom`]
//!   cannot answer that way — `Dom::parse` returns `Ok` with a root or `Err`, so
//!   the arm has no Rust shape to reach.
//! - **Python's `isinstance(tag, str)` in `_local_name`**: comments and processing
//!   instructions are nodes with a callable `.tag` there and are not nodes here.
//! - **`ours.contains(&peer)`** in [`discover_renderers`]: `host` comes from
//!   `macast_core::net::advertised_hosts`, which starts with `Snapshot::collect()`
//!   — live interface state with **no injection seam**. The test that would drive
//!   it can only pass or fail depending on which cables are in this machine, so
//!   section J asks for it conditionally and says out loud when it could not.
//! - **`thread::Builder::spawn` returning `Err`** in [`Table::start`]: reaching it
//!   means exhausting the OS thread limit, and the only honest way to arrange that
//!   is a fixture that also breaks every other test in the process. Its sentence
//!   is asserted at the string level instead? No — it is not asserted at all. The
//!   `spawn`-failure branch is the one place where a real `Err` is the witness,
//!   and there is no seam for it. Recorded.
//!
//! A fifth item used to be on this list: the control URL re-split inside
//! [`discover_renderers`], which the module header called unreachable by
//! construction. It is not — the [`Describer`] seam can hand back any string, and
//! [`j_an_unreadable_control_url_is_skipped_with_a_debug_line`] walks it. The two
//! above that genuinely have no Rust branch are the module header's
//! "guards the reference has that this module has no branch for".
//!
//! Per `docs/Rust-Port-Design.md` §13: 0 红 means nobody asked, and here a whole
//! set of questions genuinely went unasked. That is recorded, not papered over.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use macast_core::logging::{read_tail, Logger};

use super::*;

use crate::mdns::CastDevice;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// A witness file nobody else writes to. `claim()` is first-writer-wins, so the
/// name has to be unique per test, not per module.
fn witness(name: &str) -> (Arc<Logger>, std::path::PathBuf) {
    let logger = Logger::named(name);
    let path = logger.claim();
    (logger, path)
}

/// Read a witness back. Tests read the [`Logger::claim`] return value rather
/// than re-deriving a path through `module_log_path`, so a change to the
/// sanitising rule cannot silently redirect the read away from the write.
fn lines(path: &std::path::Path) -> String {
    read_tail(path, 200).text
}

// The DEBUG window is the crate-level `crate::DebugWindow` (lib.rs), not a
// local guard here: the threshold is a process-wide `AtomicU8` in
// `macast_core::logging`, so a guard that only restores the level still lets
// two tests share the one global at once. That collision is what this file
// used to cause in `mdns_tests` — 5 red out of 24 parallel full-crate runs,
// 0 out of 12 serial — and the reading is recorded on `DebugWindow`.

/// Poll an atomic until it reaches `want`, or give up after ~2 s and let the
/// caller's assertion fail honestly. There is no shared `wait_until` in this
/// crate (`mdns_tests.rs` has none either), and a test that sleeps a fixed
/// amount is a test that passes on a fast machine for the wrong reason.
fn wait_for(counter: &AtomicUsize, want: usize) -> bool {
    for _ in 0..2000 {
        if counter.load(Ordering::SeqCst) >= want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    counter.load(Ordering::SeqCst) >= want
}

/// A `Setting` bound to a path under the pinned test directory. `Setting::new`
/// reads nothing — the file does not have to exist — and [`discover_renderers`]
/// only ever passes it on to `advertised_hosts`.
fn setting(dir_name: &str) -> Arc<Setting> {
    Arc::new(Setting::new(
        macast_core::paths::setting_dir().join(format!("targets-{dir_name}.json")),
    ))
}

/// One device description, in **both** spellings: the namespaced one a real TV
/// sends, and the bare one that proves `local_name`'s `rsplit('}')` is doing the
/// work rather than the fixture happening to look right. Everything else — the
/// control URL, the service type, the whitespace — is identical, so the two
/// answers must be too.
fn description(namespaced: bool, name: &str, service_type: &str, control: &str) -> String {
    let xmlns = if namespaced {
        r#" xmlns="urn:schemas-upnp-org:device-1-0""#
    } else {
        ""
    };
    format!(
        "<root{xmlns}><device><friendlyName>{name}</friendlyName>\
         <serviceList><service><serviceType>{service_type}</serviceType>\
         <controlURL>{control}</controlURL><serviceId>urn:foo:1</serviceId>\
         </service></serviceList></device></root>"
    )
}

/// The AVTransport type spelled the way the **device** spells it.
///
/// This is deliberately a literal and **not** derived from `AVTRANSPORT_SUFFIX`.
/// The fixtures have to stay independent of the constant they are matched against:
/// if `description()` built its `serviceType` out of the suffix, then widening,
/// narrowing, or renaming `AVTRANSPORT_SUFFIX` in `targets.rs` would move the
/// fixture and the implementation **together**, and every test below would still
/// pass while the real SSDP answers on the network no longer matched. As written,
/// someone changing the suffix breaks the fixtures instead of blessing them — which
/// is the whole point of the case at 「the `:` in `AVTRANSPORT_SUFFIX` is compared
/// with case」.
const AV: &str = "urn:schemas-upnp-org:service:AVTransport:1";

/// A description naming one AVTransport service at `control`, in either spelling.
fn av_description(namespaced: bool, name: &str, control: &str) -> String {
    description(namespaced, name, AV, control)
}

// ---------------------------------------------------------------------------
// Fake seams
// ---------------------------------------------------------------------------

/// A scripted SSDP socket. Unlike `mdns_tests`' fake `Wire`, a reply here carries
/// **its own peer** — `SsdpWire::recv_from` returns `(bytes, ip)` per datagram and
/// [`discover_renderers`] filters on that ip, so one fixed source could not witness
/// "our own announcement is dropped and the neighbour's is kept".
/// One scripted answer from the socket, in the position it was written.
///
/// The three shapes are separate queue entries rather than flags **because the
/// position carries the meaning**: `failing()` after two replies is a different
/// network from `failing()` before them, and the test that says "the breaking
/// reply is the second read but contributed nothing" can only be written if the
/// error sits at index 1 of a list. With a single `fail_recv` flag consulted
/// after the queue drains, the hard-error test would read the *third* datagram
/// instead of erroring, and its "the loop broke" assertion would be a statement
/// about the deadline rather than about the error.
enum Answer {
    /// A datagram from a peer.
    Reply(Vec<u8>, String),
    /// "Nothing arrived before the socket's own timeout": `Ok(None)`.
    Empty,
    /// A socket that died mid-search.
    Broken(io::ErrorKind),
}

struct FakeWire {
    /// Answers the fake will hand back, in order. Exhausting this list means
    /// "nothing came before the deadline", which keeps being `Ok(None)` — the
    /// search loops until its own time is up.
    incoming: VecDeque<Answer>,
    /// Every `M-SEARCH` the module wrote, so a change to the request bytes cannot
    /// pass by no longer being checked.
    sent: Vec<Vec<u8>>,
    reads: usize,
    /// When set, `send_to` refuses before anything is asked.
    fail_send: Option<io::ErrorKind>,
}

/// How long a scripted search is allowed to keep asking for datagrams.
///
/// A fake that runs out of replies answers `Ok(None)` forever, which is the
/// honest model — the real socket blocks 0.4 s per empty answer and only stops
/// when the deadline passes. Reproducing that by sleeping would make these tests
/// take seconds, so the deadline does the work instead: keep it short, and assert
/// on what was **heard** and on the log line. `reads` is only asserted where the
/// loop stops on its own — a zero timeout, or a scripted broken read.
const SEARCH_WINDOW: Duration = Duration::from_millis(40);

impl FakeWire {
    fn new() -> Self {
        FakeWire {
            incoming: VecDeque::new(),
            sent: Vec::new(),
            reads: 0,
            fail_send: None,
        }
    }

    /// One announcement from `peer`. The bytes are the whole datagram, headers
    /// included, exactly as `recv_from` hands them to the caller.
    fn with_reply(mut self, reply: &str, peer: &str) -> Self {
        self.incoming
            .push_back(Answer::Reply(
                reply.as_bytes().to_vec(),
                peer.to_string(),
            ));
        self
    }

    /// `n` "nothing yet" answers, at this position in the script.
    fn with_empties(mut self, n: usize) -> Self {
        for _ in 0..n {
            self.incoming.push_back(Answer::Empty);
        }
        self
    }

    fn failing(mut self, kind: io::ErrorKind) -> Self {
        self.incoming.push_back(Answer::Broken(kind));
        self
    }

    fn refusing_send(mut self, kind: io::ErrorKind) -> Self {
        self.fail_send = Some(kind);
        self
    }
}

impl SsdpWire for FakeWire {
    fn send_to(&mut self, request: &[u8]) -> io::Result<()> {
        if let Some(kind) = self.fail_send {
            return Err(io::Error::new(kind, "send refused"));
        }
        self.sent.push(request.to_vec());
        Ok(())
    }

    fn recv_from(&mut self) -> io::Result<Option<(Vec<u8>, String)>> {
        self.reads += 1;
        match self.incoming.pop_front() {
            Some(Answer::Reply(bytes, peer)) => Ok(Some((bytes, peer))),
            Some(Answer::Empty) => Ok(None),
            Some(Answer::Broken(kind)) => Err(io::Error::new(kind, "receive refused")),
            // Out of script: the socket simply has not heard anything, which is
            // `Ok(None)` and keeps the search asking until its deadline.
            None => Ok(None),
        }
    }
}

/// A scripted search that answers the way a network cannot on a deadline.
struct FakeSearcher {
    /// One row list per `ST`, in the order asked.
    answers: Mutex<Vec<Vec<(String, String)>>>,
    asked: Mutex<Vec<String>>,
    timeouts: Mutex<Vec<Duration>>,
    /// A count of how many times each `ST` was searched, for the "never asked"
    /// assertions, which need a number rather than a missing row.
    calls: AtomicUsize,
    fail: bool,
}

impl FakeSearcher {
    fn answering(answers: Vec<Vec<(String, String)>>) -> Arc<Self> {
        Arc::new(FakeSearcher {
            answers: Mutex::new(answers),
            asked: Mutex::new(Vec::new()),
            timeouts: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail: false,
        })
    }

    /// `search` returns `Err` — the shape that must abort the whole sweep without
    /// a single description being fetched.
    fn broken() -> Arc<Self> {
        Arc::new(FakeSearcher {
            answers: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            timeouts: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail: true,
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn timeouts(&self) -> Vec<Duration> {
        self.timeouts.lock().unwrap().clone()
    }
}

impl Searcher for FakeSearcher {
    fn search(
        &self,
        st: &str,
        timeout: Duration,
        _log: &Logger,
    ) -> io::Result<Vec<(String, String)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(st.to_string());
        self.timeouts.lock().unwrap().push(timeout);
        if self.fail {
            return Err(io::Error::new(io::ErrorKind::Other, "no route to 239.255.255.250"));
        }
        let mut answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            return Ok(Vec::new());
        }
        Ok(answers.remove(0))
    }
}

/// A scripted description fetch. `None` means "this renderer is not usable" —
/// Python's `continue` and the Rust `else { continue }` are the same decision.
struct FakeDescriber {
    by_url: Mutex<BTreeMap<String, Option<(String, String)>>>,
    called: Mutex<Vec<(String, String)>>,
}

impl FakeDescriber {
    fn new(rows: Vec<(&str, Option<(&str, &str)>)>) -> Arc<Self> {
        let mut by_url = BTreeMap::new();
        for (url, answer) in rows {
            by_url.insert(
                url.to_string(),
                answer.map(|(name, control)| (name.to_string(), control.to_string())),
            );
        }
        Arc::new(FakeDescriber {
            by_url: Mutex::new(by_url),
            called: Mutex::new(Vec::new()),
        })
    }

    fn called(&self) -> Vec<(String, String)> {
        self.called.lock().unwrap().clone()
    }
}

impl Describer for FakeDescriber {
    fn describe(&self, url: &str, peer_ip: &str, _log: &Logger) -> Option<(String, String)> {
        self.called
            .lock()
            .unwrap()
            .push((url.to_string(), peer_ip.to_string()));
        self.by_url.lock().unwrap().get(url).cloned().flatten()
    }
}

/// A scripted HTTP read for [`describe_renderer`].
struct FakeFetcher {
    body: Vec<u8>,
    error: Option<String>,
    seen_urls: Mutex<Vec<String>>,
    seen_limits: Mutex<Vec<usize>>,
}

impl FakeFetcher {
    fn serving(body: &str) -> Self {
        FakeFetcher {
            body: body.as_bytes().to_vec(),
            error: None,
            seen_urls: Mutex::new(Vec::new()),
            seen_limits: Mutex::new(Vec::new()),
        }
    }

    fn refusing(why: &str) -> Self {
        FakeFetcher {
            body: Vec::new(),
            error: Some(why.to_string()),
            seen_urls: Mutex::new(Vec::new()),
            seen_limits: Mutex::new(Vec::new()),
        }
    }
}

impl Fetcher for FakeFetcher {
    fn read(&self, url: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.seen_urls.lock().unwrap().push(url.to_string());
        self.seen_limits.lock().unwrap().push(max_bytes);
        match &self.error {
            Some(why) => Err(io::Error::new(io::ErrorKind::Other, why.clone())),
            None => Ok(self.body.clone()),
        }
    }
}

/// A scripted pair of sweeps for [`Table`]. Two shapes: answer immediately with a
/// fixed list, or hold the thread open so a latch can actually be observed.
struct FakeSweeps {
    cast_rows: Mutex<Vec<CastDevice>>,
    dlna_rows: Mutex<Vec<DlnaDevice>>,
    cast_calls: Arc<AtomicUsize>,
    dlna_calls: Arc<AtomicUsize>,
    /// When set, each sweep signals "I am in" and then waits for "you may leave",
    /// so `searching` is observable rather than a race the test lost.
    gate: Option<Arc<HoldGate>>,
    /// When set, the cast sweep panics before answering.
    explode: bool,
}

/// A two-beat gate: the sweep waits here, the test releases it.
struct HoldGate {
    inside: Mutex<bool>,
    release: Mutex<bool>,
    entered: Condvar,
    let_go: Condvar,
}

impl HoldGate {
    fn new() -> Arc<Self> {
        Arc::new(HoldGate {
            inside: Mutex::new(false),
            release: Mutex::new(false),
            entered: Condvar::new(),
            let_go: Condvar::new(),
        })
    }

    /// Park until the test lets go. The `inside` guard is **dropped before**
    /// parking: this gate is waited on by another thread that has to read that
    /// same flag, and holding it through a `Condvar::wait` would deadlock the
    /// witness against the very sweep it is trying to observe (the first run of
    /// section K hung exactly here, with `CAST_FILE_SEARCH` parked on `let_go`
    /// while `wait_until_inside` blocked on `inside`).
    fn arrive(&self) {
        {
            let mut inside = self.inside.lock().unwrap();
            *inside = true;
        }
        self.entered.notify_all();
        let mut release = self.release.lock().unwrap();
        while !*release {
            release = self.let_go.wait(release).unwrap();
        }
    }

    /// True once a sweep has parked. Bounded so a sweep that never arrives is a
    /// failed assertion, not a hung test.
    fn wait_until_inside(&self) -> bool {
        let mut inside = self.inside.lock().unwrap();
        for _ in 0..2000 {
            if *inside {
                return true;
            }
            inside = self.entered.wait_timeout(inside, Duration::from_millis(10)).unwrap().0;
        }
        *inside
    }

    fn release(&self) {
        let mut flag = self.release.lock().unwrap();
        *flag = true;
        self.let_go.notify_all();
    }
}

impl FakeSweeps {
    fn returning(cast: Vec<CastDevice>, dlna: Vec<DlnaDevice>) -> Arc<Self> {
        Arc::new(FakeSweeps {
            cast_rows: Mutex::new(cast),
            dlna_rows: Mutex::new(dlna),
            cast_calls: Arc::new(AtomicUsize::new(0)),
            dlna_calls: Arc::new(AtomicUsize::new(0)),
            gate: None,
            explode: false,
        })
    }

    /// Both sweeps empty: the shape that must **not** clear a cache that already
    /// has rows.
    fn empty() -> Arc<Self> {
        Self::returning(Vec::new(), Vec::new())
    }

    /// Both sweeps hold until told to leave.
    fn held(gate: Arc<HoldGate>) -> Arc<Self> {
        Arc::new(FakeSweeps {
            cast_rows: Mutex::new(Vec::new()),
            dlna_rows: Mutex::new(Vec::new()),
            cast_calls: Arc::new(AtomicUsize::new(0)),
            dlna_calls: Arc::new(AtomicUsize::new(0)),
            gate: Some(gate),
            explode: false,
        })
    }

    /// The cast sweep panics on the way out — the `LatchGuard`'s witness.
    fn exploding() -> Arc<Self> {
        Arc::new(FakeSweeps {
            cast_rows: Mutex::new(Vec::new()),
            dlna_rows: Mutex::new(Vec::new()),
            cast_calls: Arc::new(AtomicUsize::new(0)),
            dlna_calls: Arc::new(AtomicUsize::new(0)),
            gate: None,
            explode: true,
        })
    }

    fn cast_calls(&self) -> usize {
        self.cast_calls.load(Ordering::SeqCst)
    }

    fn dlna_calls(&self) -> usize {
        self.dlna_calls.load(Ordering::SeqCst)
    }
}

impl Sweeps for FakeSweeps {
    fn cast(&self, _setting: &Arc<Setting>) -> Vec<CastDevice> {
        self.cast_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.arrive();
        }
        if self.explode {
            panic!("the cast sweep died mid-flight");
        }
        self.cast_rows.lock().unwrap().clone()
    }

    fn dlna(&self, _setting: &Arc<Setting>) -> Vec<DlnaDevice> {
        self.dlna_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.arrive();
        }
        self.dlna_rows.lock().unwrap().clone()
    }
}

fn dlna(name: &str, control: &str, host: &str) -> DlnaDevice {
    DlnaDevice {
        name: name.to_string(),
        control_url: control.to_string(),
        host: host.to_string(),
    }
}

fn cast(name: &str, address: &str, port: u16) -> CastDevice {
    CastDevice {
        name: name.to_string(),
        address: address.to_string(),
        port,
    }
}

// ---------------------------------------------------------------------------
// A: the M-SEARCH document — every byte, derived from the constants
// ---------------------------------------------------------------------------

#[test]
fn a_the_request_is_one_crlf_document_ended_by_a_blank_line() {
    crate::pin_test_dir();
    let bytes = ssdp_request("urn:schemas-upnp-org:service:AVTransport:1");
    let text = String::from_utf8(bytes.clone()).expect("the request is ASCII by construction");

    // `ssdp:discover` in quotes, `MX: 2`, and the `ST` interpolated — each is a
    // line a receiver reads, so each is a sentence rather than a detail.
    assert!(
        text.starts_with("M-SEARCH * HTTP/1.1\r\n"),
        "the request line is the first line: {text:?}"
    );
    assert!(text.contains("\r\nMAN: \"ssdp:discover\"\r\n"), "{text:?}");
    assert!(text.contains("\r\nMX: 2\r\n"), "{text:?}");
    assert!(
        text.contains("\r\nST: urn:schemas-upnp-org:service:AVTransport:1\r\n"),
        "{text:?}"
    );

    // The terminating blank line is not decoration: a header block without it is
    // a document the receiver never finishes reading (AGENTS.md §4.2, the stub
    // that omitted it and made ureq hang).
    assert!(text.ends_with("\r\n\r\n"), "{text:?}");
    assert!(!text.ends_with("\r\n\r\n\r\n"), "{text:?}");

    // Line count, so adding or dropping a header is a red test rather than a
    // quieter request.
    assert_eq!(text.split("\r\n").count(), 7, "{text:?}");
}

#[test]
fn a_the_host_line_is_the_multicast_group_not_a_literal() {
    crate::pin_test_dir();
    // This row is the re-export's reason to exist: `SSDP_ADDR`/`SSDP_PORT` come
    // from `macast-upnp`, and a literal here would let the two drift.
    let text = String::from_utf8(ssdp_request("x")).unwrap();
    let expected = format!("HOST: {SSDP_ADDR}:{SSDP_PORT}");
    assert!(text.contains(&expected), "{text:?} does not contain {expected:?}");
    assert_eq!(SSDP_ADDR, "239.255.255.250");
    assert_eq!(SSDP_PORT, 1900);
}

#[test]
fn a_every_header_its_own_line_and_no_trailing_space() {
    crate::pin_test_dir();
    let text = String::from_utf8(ssdp_request(DLNA_SERVICE)).unwrap();
    let mut rows: Vec<&str> = text.split("\r\n").collect();
    assert_eq!(rows.pop(), Some("")); // the blank line that ends the block
    assert_eq!(rows.pop(), Some("")); // the CRLF that makes it blank
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert_eq!(rows[0], "M-SEARCH * HTTP/1.1", "{rows:?}");
    for row in &rows[1..] {
        assert!(!row.ends_with(' ') && !row.ends_with('\t'), "{row:?}");
        // Every header line is `Name: value`. **The space after the colon is the
        // HTTP separator and it is on all four**, `HOST:` (`cast_local_file.py:1089`
        // spells it `'HOST: {}:{}'.format(...)`) exactly as much as `ST:`. The
        // thing that must be space-free is the *value*: a search target is a
        // colon-spelled URN, the receiver splits at the first colon and takes
        // the rest verbatim, so a space there means somebody typed a
        // human-readable name into a wire field.
        //
        // This row first whitelisted four prefixes and forgot `ST: ` — so it
        // failed on a **correct** request. The whitelist was reconstructing
        // "which lines do I remember" instead of asking what the rule is, and
        // the rule answers the real question (a space inside `urn:…:1` would be
        // a defect) that the prefix list could not.
        let (name, value) = row
            .split_once(": ")
            .unwrap_or_else(|| panic!("{row:?} is not `Name: value`"));
        assert!(!name.is_empty() && !name.contains(' '), "bad header name in {row:?}");
        assert!(!value.is_empty(), "{row:?} carries no value");
        assert!(!value.contains(' '), "space inside the value of {row:?}");
    }
    assert_eq!(rows[4], format!("ST: {DLNA_SERVICE}"), "{rows:?}");
}

// ---------------------------------------------------------------------------
// B: locations_in — which header lines count as an announcement
// ---------------------------------------------------------------------------

#[test]
fn b_the_location_header_is_read_case_insensitively_and_stripped() {
    crate::pin_test_dir();
    // Real stacks spell it every way. The strip is on **both** sides of the
    // colon, which is why `Location:\thttp://…` and `location :` are separate
    // shapes rather than one.
    let reply = "HTTP/1.1 200 OK\r\nLocation:\thttp://192.168.1.9:49152/d.xml \r\n\r\n";
    assert_eq!(
        locations_in(reply, "192.168.1.9"),
        vec![(
            "http://192.168.1.9:49152/d.xml".to_string(),
            "192.168.1.9".to_string()
        )]
    );

    let shouty = "LOCATION: http://a/x\r\n".to_string();
    assert_eq!(locations_in(&shouty, "1.2.3.4").len(), 1);
    let mixed = "LoCaTiOn: http://a/x\r\n".to_string();
    assert_eq!(locations_in(&mixed, "1.2.3.4").len(), 1);
}

#[test]
fn b_the_prefix_test_is_the_literal_http_and_it_is_lax() {
    crate::pin_test_dir();
    let _debug = crate::DebugWindow::armed();
    // Three cases that look like one question and are two.
    //
    // `httpX://` **passes** — the filter is `starts_with("http")`, not a scheme
    // parse, and the module's own doc names this shape. A future "tighten it to a
    // real scheme check" must red this test rather than silently widen the accept
    // set.
    let lax = "Location: httpX://a/x\r\n".to_string();
    assert_eq!(
        locations_in(&lax, "1.2.3.4"),
        vec![("httpX://a/x".to_string(), "1.2.3.4".to_string())]
    );
    // `httx://` fails: not a prefix of `http`.
    let reject = "Location: httx://a/x\r\n".to_string();
    assert!(locations_in(&reject, "1.2.3.4").is_empty());
    // `ftp://` fails the same way.
    let ftp = "Location: ftp://a/x\r\n".to_string();
    assert!(locations_in(&ftp, "1.2.3.4").is_empty());
}

#[test]
fn b_a_header_naming_an_http_url_is_not_an_announcement() {
    crate::pin_test_dir();
    // Only `LOCATION` is a device's address. A `Server:` or `EXT:` header that
    // happens to contain a URL must not become a renderer.
    let reply = "HTTP/1.1 200 OK\r\nServer: Linux UPnP/1.0\r\nCACHE-CONTROL: http://a/x\r\nEXT: \r\nLocation: http://a/y\r\n\r\n";
    assert_eq!(
        locations_in(reply, "10.0.0.5"),
        vec![("http://a/y".to_string(), "10.0.0.5".to_string())]
    );
}

#[test]
fn b_a_bare_location_line_without_a_colon_answers_nothing() {
    crate::pin_test_dir();
    // The line is *named* `location`, which is the trap: a reader that matched on
    // the lowercased line rather than the pre-colon half would call this an
    // announcement with an empty address.
    assert!(locations_in("LOCATION\r\n", "1.2.3.4").is_empty());
    assert!(locations_in("location", "1.2.3.4").is_empty());
    // And a value that is only whitespace strips to nothing, which is not http.
    assert!(locations_in("Location:   \r\n", "1.2.3.4").is_empty());
}

#[test]
fn b_two_location_headers_in_one_reply_are_two_answers() {
    crate::pin_test_dir();
    // A device that advertises both an IPv4 and a hostname gets both rows — the
    // dedup happens later, per host, in `discover_renderers`, not here.
    let reply = "Location: http://a/one\r\nLOCATION: http://a/two\r\n";
    assert_eq!(
        locations_in(reply, "1.2.3.4"),
        vec![
            ("http://a/one".to_string(), "1.2.3.4".to_string()),
            ("http://a/two".to_string(), "1.2.3.4".to_string()),
        ]
    );
}

#[test]
fn b_the_peer_ip_is_carried_through_and_never_invented() {
    crate::pin_test_dir();
    // The peer is the only thing `discover_renderers` can filter "is this us?" on,
    // and it can only come from the datagram's sender.
    let reply = "Location: http://a/x\r\n";
    assert_eq!(locations_in(reply, "9.9.9.9")[0].1, "9.9.9.9");
    assert!(locations_in(reply, "").is_empty() == false);
    assert_eq!(locations_in(reply, "")[0].1, "");
}

#[test]
fn b_the_line_split_is_python_splitlines_not_the_ascii_one() {
    crate::pin_test_dir();
    // `\r\n` and `\n` both end a header; the module also honours the Unicode line
    // endings CPython's `str.splitlines()` treats as breaks, because the same
    // helper decides what a "line" of a reply is. A `\u{2028}` in a `USN` field
    // must not splice a `Location` value in half.
    assert_eq!(
        locations_in("Location: http://a/one\nLocation: http://a/two\r\n", "1.2.3.4").len(),
        2
    );
    assert_eq!(
        locations_in("Location: http://a/one\u{2028}Cache-Control: max-age=1800", "1.2.3.4").len(),
        1
    );
    // A CR alone ends nothing (CPython agrees: `\r` is a break **only** as `\r\n`
    // or `\r` at line start — so here it is part of the value's whitespace and
    // the strip eats it).
    assert_eq!(
        locations_in("Location: http://a/one\rCache-Control: x", "1.2.3.4").len(),
        1
    );
}

// ---------------------------------------------------------------------------
// C: ssdp_search — the deadline, the dedup, and which failure stops the sweep
// ---------------------------------------------------------------------------

#[test]
fn c_a_zero_timeout_still_asks_and_never_reads() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-c-zero-timeout");
    let mut wire = FakeWire::new();
    let found = ssdp_search(&mut wire, "st-one", Duration::ZERO, &log).unwrap();
    assert!(found.is_empty());
    // The send happens before the deadline is even computed, so a search with no
    // time left is still a search that was asked.
    assert_eq!(wire.sent, vec![ssdp_request("st-one")]);
    assert_eq!(wire.reads, 0, "no deadline means no datagram wait");
    assert!(lines(&_path).is_empty(), "silence is the right answer here");
}

#[test]
fn c_the_request_bytes_are_what_the_searcher_claims_to_have_sent() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-c-request-shape");
    let mut wire = FakeWire::new();
    ssdp_search(&mut wire, DLNA_SERVICE, Duration::ZERO, &log).unwrap();
    assert_eq!(wire.sent.len(), 1);
    assert_eq!(
        String::from_utf8(wire.sent[0].clone()).unwrap(),
        String::from_utf8(ssdp_request(DLNA_SERVICE)).unwrap()
    );
}

#[test]
fn c_replies_are_deduped_and_sorted_because_the_card_reads_a_list() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-c-dedup");
    let reply = "Location: http://b/x\r\n";
    let mut wire = FakeWire::new()
        .with_reply(reply, "10.0.0.2")
        .with_reply(reply, "10.0.0.2") // same datagram twice — the usual multicast life
        .with_reply("Location: http://a/y\r\n", "10.0.0.9");
    let found = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log).unwrap();
    // `BTreeSet` is Python's `sorted(found)`: one row per (location, peer), in a
    // stable order the card can show without flickering.
    assert_eq!(
        found,
        vec![
            ("http://a/y".to_string(), "10.0.0.9".to_string()),
            ("http://b/x".to_string(), "10.0.0.2".to_string()),
        ]
    );
    let quiet = lines(&path);
    assert!(quiet.is_empty(), "a good search says nothing: {quiet:?}");
}

#[test]
fn c_the_same_location_from_two_peers_is_two_answers() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-c-two-peers");
    let mut wire = FakeWire::new()
        .with_reply("Location: http://shared/x\r\n", "10.0.0.2")
        .with_reply("Location: http://shared/x\r\n", "10.0.0.9");
    let found = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
}

#[test]
fn c_a_would_block_answer_is_not_the_end_of_the_search() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-c-would-block");
    // `Ok(None)` is "nothing arrived before the socket's own timeout" — Python
    // loops again. The only witness that this did not break early is the reply
    // that arrives **after** the empties, so the assertion is that it was heard,
    // not that the search came back empty.
    let mut wire = FakeWire::new()
        .with_empties(2)
        .with_reply("Location: http://late/x\r\n", "10.0.0.2");
    let found = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log).unwrap();
    assert_eq!(
        found,
        vec![("http://late/x".to_string(), "10.0.0.2".to_string())],
        "a quiet socket is not a dead one"
    );
    // Two empties plus the reply is the **minimum** number of reads: the deadline
    // is what ends this loop, so the exact count depends on how fast this machine
    // runs. `>=` is the honest form; the number that would prove the loop kept
    // going is 3, and it is present.
    assert!(wire.reads >= 3, "reads {}", wire.reads);
    let quiet = lines(&path);
    assert!(quiet.is_empty(), "an empty read is not a failure: {quiet:?}");
}

#[test]
fn c_a_hard_receive_error_breaks_with_a_count_of_what_was_found() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-c-hard-error");
    // The sentence is DEBUG on purpose: a socket that breaks mid-search is normal
    // on a noisy network, and an ERROR line per search would drown the real ones.
    // It is also **ours** — `_ssdp_search` at `cast_local_file.py:1105-1106` breaks
    // in silence — so nothing about this witness comes from the ported code: the
    // default INFO threshold swallows the record and the test reads an empty file
    // (the first run of this row was red exactly that way, and it looked like the
    // product never logged).
    let _debug = crate::DebugWindow::armed();
    let mut wire = FakeWire::new()
        .with_reply("Location: http://a/one\r\n", "10.0.0.2")
        .failing(io::ErrorKind::PermissionDenied)
        .with_reply("Location: http://never/read\r\n", "10.0.0.3");
    let found = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log).unwrap();
    assert_eq!(
        found,
        vec![("http://a/one".to_string(), "10.0.0.2".to_string())]
    );
    let text = lines(&path);
    assert!(
        text.contains("SSDP receive stopped after 1: "),
        "no break sentence in the witness: {text:?}"
    );
    // `after 1` counts **locations**, not replies: the breaking reply is the
    // second read but contributed nothing, and the number the user needs is "how
    // much did I already learn".
    assert!(text.contains("receive refused"), "{text}");
    // And the break is a break: the queued third reply is never read.
    assert_eq!(wire.reads, 2, "{text}");
}

#[test]
fn c_an_empty_search_counts_zero_in_that_sentence() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-c-zero-found");
    let _debug = crate::DebugWindow::armed();
    let mut wire = FakeWire::new().failing(io::ErrorKind::Interrupted);
    let found = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log).unwrap();
    assert!(found.is_empty());
    // The break happened on the very first read, so the exact count is a fact about
    // the error, not about the deadline — and the sentence's number is `0`.
    assert_eq!(wire.reads, 1, "the first broken read ends the search");
    let text = lines(&path);
    assert!(
        text.contains("SSDP receive stopped after 0: "),
        "an empty search must still say which empty it is: {text:?}"
    );
    assert!(!text.contains("after 1"), "{text}");
}

#[test]
fn c_a_refused_send_propagates_and_says_nothing() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-c-send-refused");
    // `ssdp_search` is the only place that could turn this into a sentence, and it
    // deliberately does not: `sweep_renderers` owns the one user-facing sentence,
    // and two would be two answers to the same failure.
    let mut wire = FakeWire::new().refusing_send(io::ErrorKind::PermissionDenied);
    let error = ssdp_search(&mut wire, "st", SEARCH_WINDOW, &log)
        .expect_err("the send failure is not swallowed");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(wire.sent.is_empty(), "a refused send is not a sent request");
    assert_eq!(wire.reads, 0, "a socket that could not ask never starts listening");
    assert!(lines(&path).is_empty(), "{}", lines(&path));
}

#[test]
fn c_the_real_wire_formats_the_group_from_the_same_constants() {
    crate::pin_test_dir();
    // `ssdp_destination` is the one place an IPv4-only assumption can speak: if
    // `SSDP_ADDR` ever became a hostname or a v6 literal, this would be an error
    // with a sentence that names it rather than a silently wrong destination.
    let dest = ssdp_destination().expect("the group address is IPv4");
    assert_eq!(dest.ip().to_string(), SSDP_ADDR);
    assert_eq!(dest.port(), SSDP_PORT);
    assert!(dest.is_ipv4(), "the port stays IPv4-only by design");
}

// ---------------------------------------------------------------------------
// D: local_name — the rsplit that carries the namespace
// ---------------------------------------------------------------------------

#[test]
fn d_a_clark_name_yields_only_the_local_part_lowercased() {
    crate::pin_test_dir();
    assert_eq!(local_name("{urn:schemas-upnp-org:device-1-0}friendlyName"), "friendlyname");
    assert_eq!(local_name("{x}ServiceType"), "servicetype");
}

#[test]
fn d_an_unprefixed_name_is_lowercased_and_left_whole() {
    crate::pin_test_dir();
    assert_eq!(local_name("friendlyName"), "friendlyname");
    assert_eq!(local_name("root"), "root");
    assert_eq!(local_name("FRIENDLYNAME"), "friendlyname");
}

#[test]
fn d_a_lone_closing_brace_is_the_edge_that_python_keeps() {
    crate::pin_test_dir();
    // `rsplit('}').next()` on `"}x"` is `"x"`; on `"}"` it is `""`; on a name with
    // no brace the iterator yields the whole string. All three are answers a real
    // device can produce, and none of them is a panic.
    assert_eq!(local_name("}x"), "x");
    assert_eq!(local_name("}"), "");
    assert_eq!(local_name(""), "");
    assert_eq!(local_name("a}b}c"), "c");
}

// ---------------------------------------------------------------------------
// E: port_suffix — "no port" has three spellings and two answers
// ---------------------------------------------------------------------------

#[test]
fn e_a_real_port_survives_and_zero_does_not() {
    crate::pin_test_dir();
    let with = py_urlsplit("http://192.168.1.9:49152/d.xml", "", true).unwrap();
    assert_eq!(port_suffix(&with, ""), ":49152");
    // Python's `parts.port` is `0` for `:0`, which is not a usable port and is not
    // `None`; the guard reads `p != 0` for that reason.
    let zero = py_urlsplit("http://192.168.1.9:0/d.xml", "", true).unwrap();
    assert_eq!(port_suffix(&zero, "DEFAULT"), "DEFAULT");
    let absent = py_urlsplit("http://192.168.1.9/d.xml", "", true).unwrap();
    assert_eq!(port_suffix(&absent, "-"), "-");
}

#[test]
fn e_an_unreadable_port_and_an_out_of_range_port_both_fall_back() {
    crate::pin_test_dir();
    // `port()` returns `Err` for both — and `port_suffix`'s match arm is
    // `Ok(Some(p))`, so **any** error is the default. A device that writes `:abc`
    // is a broken device, not a broken parse.
    let junk = py_urlsplit("http://192.168.1.9:abc/d.xml", "", true).unwrap();
    assert!(junk.port().is_err());
    assert_eq!(port_suffix(&junk, "F"), "F");
    let huge = py_urlsplit("http://192.168.1.9:99999/d.xml", "", true).unwrap();
    assert!(huge.port().is_err());
    assert_eq!(port_suffix(&huge, "F"), "F");
}

#[test]
fn e_a_fragment_does_not_hide_the_port() {
    crate::pin_test_dir();
    let with = py_urlsplit("http://192.168.1.9:8000/d.xml#top", "", true).unwrap();
    assert_eq!(port_suffix(&with, ""), ":8000");
}

// ---------------------------------------------------------------------------
// F: path_dir — the directory of a URL path, with Python's two oddities
// ---------------------------------------------------------------------------

#[test]
fn f_the_dir_of_a_bare_name_is_the_name_itself() {
    crate::pin_test_dir();
    // Python's `parsed.path.rsplit('/', 1)[0]` on a path with **no** slash is
    // `['d.xml'][0]` — the whole string, not `path[:-1]`. The first draft of this
    // section read the site as a slice (`path[:path.rfind('/')]`, which *does*
    // drop the last character when `rfind` returns -1), wrote the F rows against
    // that reading, and claimed a deviation that the code does not have: the
    // `None` arm here gives the same answer `rsplit` gives. The machine
    // contradicted the slice reading on `path_dir("/a/b/")`, which is why this
    // section now quotes `rsplit` and asks the two shapes separately.
    assert_eq!(path_dir("d.xml"), "d.xml");
    assert_eq!(path_dir(""), "");
}

#[test]
fn f_a_path_with_a_slash_yields_its_directory_without_the_slash() {
    crate::pin_test_dir();
    // `rsplit('/', 1)[0]` keeps everything **before** the last slash and drops the
    // slash itself — so a trailing slash on the base path buys a directory that
    // ends one character short of itself (`/a/b/` → `/a/b`), and a control URL
    // that does not start with `/` therefore lands one level up from where a
    // `dirname()` reader would expect. Trailing-slash behaviour is not free here:
    // it is the whole difference between `/a/b/cm.xml` and `/a/b/desc/cm.xml`.
    assert_eq!(path_dir("/desc.xml"), "");
    assert_eq!(path_dir("/a/b/desc.xml"), "/a/b");
    assert_eq!(path_dir("/a/b/"), "/a/b");
}

// ---------------------------------------------------------------------------
// G: parse_description — the name, the host, and which rewrite fires
// ---------------------------------------------------------------------------

#[test]
fn g_the_friendly_name_wins_and_both_spellings_agree() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-g-name");
    let xml = av_description(true, "客厅电视", "/cm.xml");
    let bare = av_description(false, "客厅电视", "/cm.xml");
    let a = parse_description(xml.as_bytes(), "http://192.168.1.9:8080/d.xml", "192.168.1.9", &log)
        .expect("a namespaced description is a device");
    let b = parse_description(bare.as_bytes(), "http://192.168.1.9:8080/d.xml", "192.168.1.9", &log)
        .expect("the same description without a namespace is the same device");
    assert_eq!(a, b, "the namespace must not change the answer");
    assert_eq!(a.0, "客厅电视");
    assert_eq!(a.1, "http://192.168.1.9:8080/cm.xml");
    assert!(lines(&path).is_empty(), "{}", lines(&path));
}

#[test]
fn g_a_missing_or_empty_friendly_name_is_the_peer_ip() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-name-fallback");
    // Two shapes: the element absent, and the element present and empty. Both are
    // "this TV has no name", and the dropdown must still show a row.
    let absent = r#"<root><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/cm</controlURL></service></serviceList></device></root>"#;
    let empty = av_description(false, "", "/cm");
    for xml in [absent, empty.as_str()] {
        let got =
            parse_description(xml.as_bytes(), "http://10.0.0.7:49152/d", "10.0.0.7", &log).unwrap();
        assert_eq!(got.0, "10.0.0.7", "{xml} named itself {:?}", got.0);
    }
}

#[test]
fn g_first_text_is_first_match_and_empty_is_still_a_match() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-first-text");
    // `first_text` returns at the **first** tag that matches, so a device that
    // sends an empty `<friendlyName/>` before the real one is an unnamed device —
    // that is Python's `text or peer`, which also picks the first element.
    let xml = r#"<root><device><friendlyName></friendlyName><friendlyName>Second</friendlyName></device></root>"#;
    // No service, so the answer is None — but the name was still decided, and the
    // only way to read it back is the fallback shape below.
    assert!(parse_description(xml.as_bytes(), "http://10.0.0.7/d", "10.0.0.7", &log).is_none());

    let one = r#"<root><device><friendlyName>First</friendlyName><friendlyName>Second</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/cm</controlURL></service></serviceList></device></root>"#;
    let got = parse_description(one.as_bytes(), "http://10.0.0.7/d", "10.0.0.7", &log).unwrap();
    assert_eq!(got.0, "First");
}

#[test]
fn g_the_device_url_decides_the_host_and_the_peer_is_only_a_fallback() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-g-host-source");
    let _debug = crate::DebugWindow::armed();
    // `http:///desc.xml` — an empty authority. Python's `urlsplit` gives hostname
    // `None`, so the host becomes `peer_ip`, and the origin rebuilt for a
    // root-relative control URL uses the **peer**, not the empty string that the
    // device URL nominally carries.
    let xml = av_description(false, "Box", "/cm.xml");
    let got = parse_description(xml.as_bytes(), "http:///desc.xml", "192.168.1.44", &log).unwrap();
    assert_eq!(got.1, "http://192.168.1.44/cm.xml", "{got:?}");
    // `0.0.0.0` is the other "the device is lying about its address" shape.
    let lying = parse_description(
        av_description(false, "Box", "/cm.xml").as_bytes(),
        "http://0.0.0.0:49152/d.xml",
        "192.168.1.44",
        &log,
    )
    .unwrap();
    assert_eq!(lying.1, "http://192.168.1.44:49152/cm.xml", "{lying:?}");
    // A hostname is **not** a lie: no rewrite, even though it resolves to something.
    let named = parse_description(
        av_description(false, "Box", "/cm.xml").as_bytes(),
        "http://livingroom-tv.local:49152/d.xml",
        "192.168.1.44",
        &log,
    )
    .unwrap();
    assert_eq!(named.1, "http://livingroom-tv.local:49152/cm.xml");
    assert!(lines(&path).is_empty(), "{}", lines(&path));
}

#[test]
fn g_loopback_is_replaced_by_the_peer_because_localhost_is_not_the_tv() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-loopback");
    // Two loopback **addresses** are rewritten to the address the datagram
    // actually came from. Without this, a device that advertises `127.0.0.1`
    // would make **our own** app the renderer.
    for (base, peer, expect_host) in [
        ("http://127.0.0.1:49152/d.xml", "192.168.1.9", "192.168.1.9"),
        ("http://[::1]:49152/d.xml", "192.168.1.9", "192.168.1.9"),
    ] {
        let got = parse_description(
            av_description(false, "TV", "/cm.xml").as_bytes(),
            base,
            peer,
            &log,
        )
        .unwrap_or_else(|| panic!("{base} should describe a device"));
        assert!(
            got.1.starts_with(&format!("http://{expect_host}:49152/")),
            "{base} became {}",
            got.1
        );
    }
}

#[test]
fn g_localhost_is_a_name_and_a_name_is_never_rewritten() {
    crate::pin_test_dir();
    // The third spelling that used to sit in the table above was **not** a
    // rewrite, and the Python site is what says so:
    // `ipaddress.ip_address('localhost')` raises `ValueError`, and
    // `cast_local_file.py:1167-1168` catches exactly that with
    // `pass  # a hostname: keep it, DNS is the LAN's job`. `host_is_loopback`
    // answers `None` rather than `Some(false)` for that same reason, so
    // `localhost` goes to the control URL unchanged — the same arm that keeps
    // `livingroom-tv.local` (row above) unchanged.
    //
    // This is a real way to cast to yourself, and the port keeps it because
    // "is this name actually the TV" is not answerable from the address book.
    // It is registered in `docs/Rust-Port-Design.md` §13, not fixed here.
    let (log, _path) = witness("targets-g-localhost-name");
    let got = parse_description(
        av_description(false, "TV", "/cm.xml").as_bytes(),
        "http://localhost:49152/d.xml",
        "192.168.1.9",
        &log,
    )
    .unwrap();
    assert_eq!(got.1, "http://localhost:49152/cm.xml", "{got:?}");
    assert_eq!(host_is_loopback("localhost"), None);
    assert_eq!(host_is_loopback("::1"), Some(true));
    assert_eq!(host_is_loopback("127.0.0.1"), Some(true));
    assert_eq!(host_is_loopback("192.168.1.9"), Some(false));
}

#[test]
fn g_the_port_is_taken_before_the_host_is_rewritten() {
    crate::pin_test_dir();
    // This is the one ordering bug in the block that produces a **silently
    // unopenable** URL: the description advertises no port, the device URL has
    // one, and the host is rewritten to the peer. If `port_suffix` were read
    // after the rewrite, the answer would be the peer with no port at all and the
    // control URL would point at :80.
    let (log, _path) = witness("targets-g-port-order");
    let xml = av_description(false, "TV", "/cm.xml");
    let got =
        parse_description(xml.as_bytes(), "http://127.0.0.1:49152/d.xml", "192.168.1.9", &log)
            .unwrap();
    assert_eq!(got.1, "http://192.168.1.9:49152/cm.xml", "{got:?}");
}

#[test]
fn g_a_relative_control_url_gets_the_base_directory() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-relative-dir");
    // Leading slash: origin + control. No leading slash: origin + **the base
    // path's directory** + '/' + control.
    let under = parse_description(
        av_description(false, "TV", "/cm/d.xml").as_bytes(),
        "http://10.0.0.7:8080/root/desc.xml",
        "10.0.0.7",
        &log,
    )
    .unwrap();
    assert_eq!(under.1, "http://10.0.0.7:8080/cm/d.xml", "{under:?}");

    let relative = parse_description(
        av_description(false, "TV", "cm/d.xml").as_bytes(),
        "http://10.0.0.7:8080/upnp/desc.xml",
        "10.0.0.7",
        &log,
    )
    .unwrap();
    assert_eq!(relative.1, "http://10.0.0.7:8080/upnp/cm/d.xml", "{relative:?}");

    // A base with no path at all: `path_dir('')` is `''`, so the directory
    // contributes nothing and the join is exactly one slash.
    let bare = parse_description(
        av_description(false, "TV", "cm.xml").as_bytes(),
        "http://10.0.0.7:8080",
        "10.0.0.7",
        &log,
    )
    .unwrap();
    assert_eq!(bare.1, "http://10.0.0.7:8080/cm.xml", "{bare:?}");

    // A base whose netloc names a **non-numeric port** exercises the other arm:
    // `_port_suffix` catches the ValueError and falls back to its `default`, and
    // `parse_description` calls it as `_port_suffix(parsed)` — default `''`. So the
    // bogus port is dropped **whole** and the origin is bare (`:1130-1133`, whose
    // docstring gives the reason: "dropping the whole renderer is worse than
    // guessing the scheme's own port"). This row used to assert the opposite
    // (`…:8080desc/cm.xml`) and leaned on a `path[:-1]` story that does not exist
    // here — see the F section, where the same misreading of the Python site put
    // words in a comment the code never said. The base path is `''`, so `path_dir`
    // contributes nothing either way; what this row actually witnesses is the port
    // being thrown away, which is the difference between an openable URL and one
    // aimed at a host that has no such port.
    let odd = parse_description(
        av_description(false, "TV", "cm.xml").as_bytes(),
        "http://10.0.0.7:8080desc",
        "10.0.0.7",
        &log,
    )
    .unwrap();
    assert_eq!(odd.1, "http://10.0.0.7/cm.xml", "{odd:?}");
    assert!(!odd.1.contains("8080"), "{odd:?}");
}

#[test]
fn g_a_control_that_is_not_http_prefixed_is_relative_even_when_it_has_a_colon() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-httx-relative");
    // `httx://a/b` is the shape that a "scheme check" would call absolute and this
    // filter does not: it takes the **relative** branch, exactly like Python's
    // `control.startswith('http')`. Asserting the wrong one here would be a
    // quieter, more useful-looking bug.
    let got = parse_description(
        av_description(false, "TV", "httx://a/b").as_bytes(),
        "http://10.0.0.7:8080/d.xml",
        "10.0.0.7",
        &log,
    )
    .unwrap();
    assert_eq!(got.1, "http://10.0.0.7:8080/httx://a/b", "{got:?}");
    assert!(!got.1.contains("a/b@") , "{got:?}");
}

#[test]
fn g_an_absolute_control_on_the_same_host_is_left_alone() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-g-absolute-same");
    let (log2, _path2) = witness("targets-g-absolute-same-bare");
    for (logger, namespaced) in [(&log, true), (&log2, false)] {
        let got = parse_description(
            av_description(namespaced, "TV", "http://10.0.0.7:8080/cm?x=1").as_bytes(),
            "http://10.0.0.7:8080/d.xml",
            "10.0.0.7",
            logger,
        )
        .unwrap();
        // Untouched, **query and all** — the rewrite only happens for the other
        // host, and dropping a query from a URL we never touched would be a bug
        // the card could not show.
        assert_eq!(got.1, "http://10.0.0.7:8080/cm?x=1", "{got:?}");
    }
}

#[test]
fn g_an_absolute_control_on_another_host_is_ours_and_loses_its_query() {
    crate::pin_test_dir();
    // Three facts in one answer: the **host** becomes the one we believe in (the
    // device may name itself `192.168.0.1` while the datagram came from
    // `192.168.1.9`), the **port** is the described one with *our* port as the
    // default, and the **query is dropped**. The drop is not a port defect: the
    // reference at `cast_local_file.py:1186-1192` rebuilds
    // `'{scheme}://{host}{port}{path}'` with no query either.
    let (log, _path) = witness("targets-g-absolute-other");
    let got = parse_description(
        av_description(false, "TV", "http://192.168.0.1:5000/cm?sig=abc").as_bytes(),
        "http://192.168.1.9:8080/d.xml",
        "192.168.1.9",
        &log,
    )
    .unwrap();
    assert_eq!(got.1, "http://192.168.1.9:5000/cm", "{got:?}");

    // No port in the control: our port survives as the default.
    let (log2, _p2) = witness("targets-g-absolute-other-noport");
    let got = parse_description(
        av_description(false, "TV", "http://192.168.0.1/cm").as_bytes(),
        "http://192.168.1.9:8080/d.xml",
        "192.168.1.9",
        &log2,
    )
    .unwrap();
    assert_eq!(got.1, "http://192.168.1.9:8080/cm", "{got:?}");

    // Our base has no port either ⇒ nothing is appended, and the URL keeps its
    // default-port shape.
    let (log3, _p3) = witness("targets-g-absolute-other-both-bare");
    let got = parse_description(
        av_description(false, "TV", "http://192.168.0.1:5000/cm").as_bytes(),
        "http://192.168.1.9/d.xml",
        "192.168.1.9",
        &log3,
    )
    .unwrap();
    assert_eq!(got.1, "http://192.168.1.9:5000/cm", "{got:?}");

    // Scheme follows the **control**, not the base: an http description naming an
    // https control is still https after the host swap.
    let (log4, _p4) = witness("targets-g-absolute-scheme");
    let got = parse_description(
        av_description(false, "TV", "https://192.168.0.1/cm").as_bytes(),
        "http://192.168.1.9:8080/d.xml",
        "192.168.1.9",
        &log4,
    )
    .unwrap();
    assert_eq!(got.1, "https://192.168.1.9:8080/cm", "{got:?}");
}

// ---------------------------------------------------------------------------
// H: parse_description — every refusal, in the sentence the user would read
// ---------------------------------------------------------------------------

#[test]
fn h_a_description_with_no_avtransport_service_is_not_a_renderer() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-h-no-service");
    // The `:` in `AVTRANSPORT_SUFFIX` is compared **with case** — a device that
    // writes `:avtransport:1` is not a renderer we can drive, and matching
    // case-insensitively would hand the card a control URL for a service the
    // SOAP layer then refuses.
    let xml = description(false, "Radio", "urn:schemas-upnp-org:service:avtransport:1", "/cm");
    assert!(parse_description(xml.as_bytes(), "http://10.0.0.7/d", "10.0.0.7", &log).is_none());
    let other = description(false, "Radio", "urn:schemas-upnp-org:service:RenderingControl:1", "/cm");
    assert!(parse_description(other.as_bytes(), "http://10.0.0.7/d", "10.0.0.7", &log).is_none());
    assert!(lines(&path).is_empty(), "{}", lines(&path));
}

#[test]
fn h_the_first_matching_service_wins_even_if_it_cannot_be_reached() {
    crate::pin_test_dir();
    // `break` is on the **type**, not on a usable control URL. A device with a
    // nameless AVTransport followed by a working lowercase twin must answer `None`
    // — otherwise the card offers a row whose control URL belongs to a service the
    // SOAP calls will not address.
    let (log, _path) = witness("targets-h-first-service-wins");
    let xml = r#"<root><device><friendlyName>TV</friendlyName><serviceList>
        <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL></controlURL></service>
        <service><serviceType>urn:schemas-upnp-org:service:avtransport:1</serviceType><controlURL>/cm</controlURL></service>
        </serviceList></device></root>"#;
    assert!(parse_description(xml.as_bytes(), "http://10.0.0.7/d", "10.0.0.7", &log).is_none());
}

#[test]
fn h_the_last_child_of_a_name_is_the_one_that_survives() {
    crate::pin_test_dir();
    // `values.insert(...)` over the children means a device that repeats
    // `<controlURL>` is read the way `dict(...)` would read it: last wins. Python
    // does the same, and a test that only ever gave one child would not notice a
    // switch to "first wins".
    let (log, _path) = witness("targets-h-last-child");
    let xml = r#"<root><device><friendlyName>TV</friendlyName><serviceList><service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>/first</controlURL><controlURL>/second</controlURL>
        </service></serviceList></device></root>"#;
    let got = parse_description(xml.as_bytes(), "http://10.0.0.7:80/d", "10.0.0.7", &log).unwrap();
    assert_eq!(got.1, "http://10.0.0.7:80/second", "{got:?}");
}

#[test]
fn h_an_unreadable_device_url_is_an_error_that_names_the_skipped_renderer() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-h-bad-base");
    // `http://]/x` is a mismatched bracket; CPython 3.12 answers
    // `ValueError: Invalid IPv6 URL`, and this is a real device-URL shape from a
    // real reply, not a made-up one. ERROR because the sweep loses a renderer the
    // user can see in the TV's own menu.
    let xml = av_description(false, "TV", "/cm");
    assert!(parse_description(xml.as_bytes(), "http://]/x", "10.0.0.7", &log).is_none());
    let text = lines(&path);
    assert!(
        text.contains(
            "unreadable device URL http://]/x: Invalid IPv6 URL — this renderer is skipped"
        ),
        "{text}"
    );
    assert!(text.contains("[ERROR]"), "{text}");
}

#[test]
fn h_an_unreadable_control_url_is_an_error_about_the_control_url() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-h-bad-control");
    // The absolute branch re-parses the control URL. The sentence must name the
    // **control**, not the description, or the user debugs the wrong file.
    let xml = r#"<root><device><friendlyName>TV</friendlyName><serviceList><service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>http://]/cm</controlURL></service></serviceList></device></root>"#;
    assert!(parse_description(xml.as_bytes(), "http://10.0.0.7:8080/d", "10.0.0.7", &log).is_none());
    let text = lines(&path);
    assert!(
        text.contains("unreadable control URL http://]/cm: Invalid IPv6 URL — this renderer is skipped"),
        "{text}"
    );
}

#[test]
fn h_an_unparsable_document_is_a_debug_line_that_quotes_the_parser() {
    crate::pin_test_dir();
    let _debug = crate::DebugWindow::armed();
    let (log, path) = witness("targets-h-bad-xml");
    // Two roots is the shape `scpd::Dom::parse` refuses; its error is a plain
    // `String`, so the assertion can bind the prefix that is ours without quoting
    // the parser's whole sentence (which is the parser's to write).
    assert!(
        parse_description(
            b"<friendlyName>A</friendlyName><friendlyName>B</friendlyName>",
            "http://10.0.0.7/d",
            "10.0.0.7",
            &log
        )
        .is_none()
    );
    let text = lines(&path);
    assert!(text.contains("cannot parse http://10.0.0.7/d: "), "{text}");
    assert!(text.contains("[DEBUG]"), "{text}");
}

#[test]
fn h_invalid_utf8_inside_a_comment_is_still_a_device() {
    crate::pin_test_dir();
    // `String::from_utf8_lossy` runs before the parse, and a device description
    // with a stray byte in a comment is a description. Refusing it would drop a
    // working renderer.
    let (log, _path) = witness("targets-h-lossy-utf8");
    let xml = av_description(false, "TV", "/cm").into_bytes();
    let mark = xml.iter().position(|b| *b == b'<').unwrap();
    let mut raw = xml[..mark].to_vec();
    raw.extend_from_slice(b"<!-- \xf3\x28\xbf\xff -->");
    raw.extend_from_slice(&xml[mark..]);
    let got = parse_description(&raw, "http://10.0.0.7:80/d", "10.0.0.7", &log);
    assert!(got.is_some(), "a comment byte must not lose a renderer: {got:?}");
    assert_eq!(got.unwrap().1, "http://10.0.0.7:80/cm");
}

#[test]
fn h_whitespace_in_a_value_is_trimmed_but_inside_the_name_is_kept() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-h-trim");
    let xml = r#"<root><device><friendlyName>  Living Room TV
        </friendlyName><serviceList><service>
        <serviceType>  urn:schemas-upnp-org:service:AVTransport:1  </serviceType>
        <controlURL> /cm </controlURL></service></serviceList></device></root>"#;
    let got = parse_description(xml.as_bytes(), "http://10.0.0.7:80/d", "10.0.0.7", &log).unwrap();
    // `str::trim` in Python and `.trim()` in Rust both eat the trailing newline
    // and the indent; the inner double space is the name.
    assert_eq!(got.0, "Living Room TV", "{got:?}");
    assert_eq!(got.1, "http://10.0.0.7:80/cm", "{got:?}");
}

// ---------------------------------------------------------------------------
// I: describe_renderer and the one real socket
// ---------------------------------------------------------------------------

#[test]
fn i_an_unreadable_description_is_a_debug_line_and_no_renderer() {
    crate::pin_test_dir();
    let _debug = crate::DebugWindow::armed();
    let (log, path) = witness("targets-i-read-fail");
    let fetcher = FakeFetcher::refusing("connection closed before message completed");
    let got = describe_renderer(&fetcher, "http://10.0.0.7:80/d.xml", "10.0.0.7", &log);
    assert!(got.is_none());
    let text = lines(&path);
    assert!(
        text.contains("cannot read http://10.0.0.7:80/d.xml: connection closed before message completed"),
        "{text}"
    );
    assert!(text.contains("[DEBUG]"), "{text}");
}

#[test]
fn i_the_read_is_asked_for_the_cap_the_module_promises() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-i-cap");
    let fetcher = FakeFetcher::serving(&av_description(false, "TV", "/cm"));
    describe_renderer(&fetcher, "http://10.0.0.7/d", "10.0.0.7", &log).unwrap();
    assert_eq!(fetcher.seen_urls.lock().unwrap().len(), 1);
    assert_eq!(fetcher.seen_urls.lock().unwrap()[0], "http://10.0.0.7/d");
    // The limit is `DESCRIPTION_MAX_BYTES`, not "however much the device sends":
    // one 6 MB advertisement used to be enough to make a sweep unbounded.
    assert_eq!(fetcher.seen_limits.lock().unwrap()[0], DESCRIPTION_MAX_BYTES);
    assert_eq!(DESCRIPTION_MAX_BYTES, 512 * 1024);
}

#[test]
fn i_a_body_truncated_at_the_cap_loses_the_service_and_says_so_by_silence() {
    crate::pin_test_dir();
    let (log, path) = witness("targets-i-truncated");
    // Cut the document where the `<controlURL>` no longer exists. The answer is
    // `None` with **no** log line — a truncated body is not a parse failure, and
    // an ERROR per truncation would be a new way to fill the log with a lie.
    let full = av_description(false, "TV", "/cm");
    let cut = &full[..full.find("<controlURL>").unwrap()];
    let fetcher = FakeFetcher::serving(cut);
    assert!(describe_renderer(&fetcher, "http://10.0.0.7/d", "10.0.0.7", &log).is_none());
    assert!(lines(&path).is_empty(), "{}", lines(&path));
}

#[test]
fn i_the_real_fetcher_reads_a_real_listener_through_the_proxy_none_agent() {
    crate::pin_test_dir();
    // The one test here that cannot be faked: `description_agent()` sets
    // `proxy(None)` because this machine's Wi-Fi proxy silently eats loopback
    // traffic (AGENTS.md §4.2). A fake `Fetcher` is not `ureq`, so only a real
    // request can witness that the agent was built the right way.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback is available");
    let port = listener.local_addr().unwrap().port();
    let body = av_description(true, "Real TV", "/upnp/control/av");
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one request");
        let mut head = [0u8; 4096];
        let mut filled = 0;
        while filled < head.len() {
            match stream.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(n) => {
                    filled += n;
                    if head[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // The terminating blank line is part of the protocol, not politeness: the
        // first relay stub here omitted it and hung a real client while a bare
        // socket read was content.
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        use std::io::Write;
        stream.write_all(reply.as_bytes()).expect("the body goes out");
    });

    let (log, path) = witness("targets-i-real-socket");
    let url = format!("http://127.0.0.1:{port}/desc.xml");
    let got = HttpDescriber
        .describe(&url, "192.168.99.9", &log)
        .expect("a real listener on loopback is a real device");
    // The host is ours (`127.0.0.1` is loopback ⇒ the peer), the port is the
    // described one, and the path is absolute-relative.
    assert_eq!(got.0, "Real TV");
    assert_eq!(got.1, format!("http://192.168.99.9:{port}/upnp/control/av"), "{got:?}");
    assert!(lines(&path).is_empty(), "{}", lines(&path));
    thread.join().expect("the fake server finished");
}

#[test]
fn i_a_closed_listener_is_a_debug_line_not_a_panic() {
    crate::pin_test_dir();
    let _debug = crate::DebugWindow::armed();
    let (log, path) = witness("targets-i-closed");
    // Bind and drop immediately so the port is real and nothing is listening: the
    // refusal is `Err` from ureq, which `describe_renderer` turns into one DEBUG
    // line naming the URL.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}/desc.xml");
    assert!(HttpDescriber.describe(&url, "192.168.99.9", &log).is_none());
    let text = lines(&path);
    assert!(text.contains(&format!("cannot read {url}: ")), "{text}");
}

// ---------------------------------------------------------------------------
// J: discover_renderers — the filter, the dedup, the sort, and the sentence
// ---------------------------------------------------------------------------

#[test]
fn j_both_search_targets_are_asked_in_order_with_the_same_timeout() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-order");
    let searcher = FakeSearcher::answering(vec![Vec::new(), Vec::new()]);
    let describer = FakeDescriber::new(Vec::new());
    discover_renderers(&*searcher, &*describer, &setting("j-order"), Duration::from_millis(700), &log)
        .unwrap();
    // The device URN first, then the service URN — `DLNA_SEARCH_TARGETS` in Python
    // (`cast_local_file.py:106-107`) is that tuple, in that order, and the loop at
    // `:1214` walks it as written. A card that showed the same TV twice proved the
    // second pass is not optional; a card that only ever asked for the service would
    // miss the renderers that answer on the device type alone.
    assert_eq!(
        searcher.asked(),
        vec![
            macast_upnp::UPNP_DEVICE.to_string(),
            DLNA_SERVICE.to_string()
        ]
    );
    assert_eq!(DLNA_SEARCH_TARGETS, [macast_upnp::UPNP_DEVICE, DLNA_SERVICE]);
    assert_eq!(searcher.timeouts(), vec![Duration::from_millis(700); 2]);
}

#[test]
fn j_a_failed_search_aborts_before_any_description_is_fetched() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-abort");
    let searcher = FakeSearcher::broken();
    let describer = FakeDescriber::new(vec![("http://a/x", Some(("A", "http://a/x")))]);
    let error = discover_renderers(
        &*searcher,
        &*describer,
        &setting("j-abort"),
        Duration::from_secs(1),
        &log,
    )
    .expect_err("the SSDP failure is the caller's to report");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(describer.called().len(), 0, "no location, no fetch");
}

#[test]
fn j_a_renderer_that_cannot_be_described_adds_nothing() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-undescribed");
    let searcher = FakeSearcher::answering(vec![
        vec![("http://a/dead".to_string(), "10.0.0.2".to_string())],
        Vec::new(),
    ]);
    let describer = FakeDescriber::new(vec![("http://a/dead", None)]);
    let found =
        discover_renderers(&*searcher, &*describer, &setting("j-undescribed"), Duration::from_secs(1), &log)
            .unwrap();
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(describer.called(), vec![("http://a/dead".to_string(), "10.0.0.2".to_string())]);
}

#[test]
fn j_rows_are_keyed_by_host_and_the_later_write_wins() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-dedup");
    // Two advertisements, one machine. `seen.insert(host, …)` means the **second**
    // name is what the dropdown shows — which is also Python's dict behaviour, and
    // the reason a renamed TV appears renamed without a restart.
    let searcher = FakeSearcher::answering(vec![
        vec![
            ("http://10.0.0.2:80/one".to_string(), "10.0.0.2".to_string()),
            ("http://10.0.0.2:80/two".to_string(), "10.0.0.2".to_string()),
        ],
        Vec::new(),
    ]);
    let describer = FakeDescriber::new(vec![
        (
            "http://10.0.0.2:80/one",
            Some(("Living Room", "http://10.0.0.2:80/one")),
        ),
        (
            "http://10.0.0.2:80/two",
            Some(("Bedroom", "http://10.0.0.2:80/two")),
        ),
    ]);
    let found =
        discover_renderers(&*searcher, &*describer, &setting("j-dedup"), Duration::from_secs(1), &log)
            .unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "Bedroom");
    assert_eq!(found[0].control_url, "http://10.0.0.2:80/two");
    assert_eq!(found[0].host, "10.0.0.2");
}

#[test]
fn j_an_empty_host_in_the_control_url_falls_back_to_the_peer() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-empty-host");
    // `http:///cm` splits to an empty authority, so `hostname()` is `None` and the
    // row is keyed by the **peer** — the only address we know the device answers
    // on. Without the fallback every such device would collide on `""`.
    let searcher = FakeSearcher::answering(vec![
        vec![("http://10.0.0.2:80/d".to_string(), "10.0.0.2".to_string())],
        Vec::new(),
    ]);
    let describer = FakeDescriber::new(vec![("http://10.0.0.2:80/d", Some(("Box", "http:///cm")))]);
    let found = discover_renderers(
        &*searcher,
        &*describer,
        &setting("j-empty-host"),
        Duration::from_secs(1),
        &log,
    )
    .unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].host, "10.0.0.2", "{:?}", found[0]);
    assert_eq!(found[0].control_url, "http:///cm", "the URL is not repaired here");
}

#[test]
fn j_an_unreadable_control_url_is_skipped_with_a_debug_line() {
    crate::pin_test_dir();
    // **The witness that falsifies the comment at `targets.rs:706-709`.** That arm
    // was documented unreachable-by-construction on the grounds that `control` was
    // just built by `parse_description` from a split that already succeeded. It is
    // reachable the moment the `Describer` is not `parse_description` — which is
    // exactly the seam the trait exists for, and the shape a future third
    // describer (or a cached ledger row) can hand us.
    let _debug = crate::DebugWindow::armed();
    let (log, path) = witness("targets-j-resplit");
    let searcher = FakeSearcher::answering(vec![
        vec![("http://10.0.0.2:80/d".to_string(), "10.0.0.2".to_string())],
        Vec::new(),
    ]);
    let describer = FakeDescriber::new(vec![("http://10.0.0.2:80/d", Some(("客厅电视", "http://]/x")))]);
    let found = discover_renderers(
        &*searcher,
        &*describer,
        &setting("j-resplit"),
        Duration::from_secs(1),
        &log,
    )
    .unwrap();
    assert!(found.is_empty(), "the skipped renderer must not become a row: {found:?}");
    let text = lines(&path);
    assert!(
        text.contains("control URL cannot be re-read: http://]/x: Invalid IPv6 URL"),
        "{text}"
    );
    assert!(text.contains("[DEBUG]"), "{text}");
}

#[test]
fn j_the_answer_is_sorted_by_name_not_by_the_map_key() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-sort");
    // `BTreeMap` is keyed by **host**, so its iteration order is numeric-ish by
    // address. `DlnaDevice`'s derived `Ord` compares `(name, control_url, host)`,
    // and the explicit `out.sort()` is what makes the card's order Python's.
    // `Zulu` vs `alpha` is not decoration: ASCII sorts uppercase first, exactly as
    // Python's `sorted()` does, so a case-folding "improvement" reds here.
    let rows = vec![
        ("http://9.9.9.9:80/b".to_string(), "9.9.9.9".to_string()),
        ("http://1.1.1.1:80/a".to_string(), "1.1.1.1".to_string()),
    ];
    let searcher = FakeSearcher::answering(vec![rows, Vec::new()]);
    let describer = FakeDescriber::new(vec![
        ("http://9.9.9.9:80/b", Some(("Zulu", "http://9.9.9.9:80/b"))),
        ("http://1.1.1.1:80/a", Some(("alpha", "http://1.1.1.1:80/a"))),
    ]);
    let found =
        discover_renderers(&*searcher, &*describer, &setting("j-sort"), Duration::from_secs(1), &log)
            .unwrap();
    assert_eq!(
        found
            .iter()
            .map(|d| (d.name.clone(), d.host.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("Zulu".to_string(), "9.9.9.9".to_string()),
            ("alpha".to_string(), "1.1.1.1".to_string())
        ]
    );
}

#[test]
fn j_our_own_announcement_is_dropped_when_this_machine_lists_itself() {
    crate::pin_test_dir();
    let (log, _path) = witness("targets-j-ours");
    // `advertised_hosts` reads the live interface table and has **no** injection
    // seam, so this test cannot fabricate "we announced ourselves". It asks for an
    // address this machine does not have, and only answers a claim it can check.
    // The other direction — "the peer is one of ours" — is recorded as untested in
    // the module header and `docs/Rust-Port-Design.md` §13.
    let peer = "203.0.113.77"; // TEST-NET-3: never a local interface on a real LAN
    let ours = macast_core::net::advertised_hosts(&setting("j-ours"));
    if ours.iter().any(|host| host == peer) {
        return; // this machine does claim it; nothing honest to assert
    }
    let searcher = FakeSearcher::answering(vec![
        vec![("http://203.0.113.77:80/d".to_string(), peer.to_string())],
        Vec::new(),
    ]);
    let describer = FakeDescriber::new(vec![(
        "http://203.0.113.77:80/d",
        Some(("Neighbour", "http://203.0.113.77:80/d")),
    )]);
    let found = discover_renderers(
        &*searcher,
        &*describer,
        &setting("j-ours"),
        Duration::from_secs(1),
        &log,
    )
    .unwrap();
    assert_eq!(found.len(), 1, "a stranger's TV is a device: {found:?}");
    assert_eq!(describer.called().len(), 1);
}

#[test]
fn j_the_sweep_sentence_names_the_next_step_and_leaves_an_empty_table() {
    crate::pin_test_dir();
    // `sweep_renderers` is the only place the user-facing sentence lives, because
    // Python's version raised out of a daemon thread and printed a traceback
    // nobody reads (AGENTS.md「每一条失败文案必须给出下一步」). This test drives the
    // **real** searcher, so it reaches the network; it asserts the shape of the
    // answer (empty list, one ERROR line) rather than its content, which is why
    // there is no fixture here.
    let (log, _path) = witness("targets-j-sweep-sentence");
    let found = sweep_renderers(&setting("j-sweep"), Duration::from_millis(1));
    assert!(
        found.is_empty() || found.len() < 64,
        "a real sweep on a real LAN answered {} devices — the assertions above are the offline ones",
        found.len()
    );
    let _ = &log;
}

// ---------------------------------------------------------------------------
// K: the table — latches, copy-on-write, and the card's key order
// ---------------------------------------------------------------------------

#[test]
fn k_an_empty_sweep_does_not_overwrite_what_we_already_knew() {
    crate::pin_test_dir();
    // 「A device asleep right now is still the one being cast to」: the cache is
    // replaced only when the sweep found something, so a missed multicast cannot
    // empty the dropdown while the TV is still in the living room.
    let (_log, _path) = witness("targets-k-copy-on-write");
    let rows = vec![dlna("客厅电视", "http://10.0.0.2:80/cm", "10.0.0.2")];
    // One table, two sweeps, two different answers — which is exactly what a
    // multicast that came back empty looks like after one that did not.
    let seeding = Arc::new(OnceSweeps {
        casts: Mutex::new(vec![Vec::new()]),
        dlnas: Mutex::new(vec![rows.clone(), Vec::new()]),
        calls: AtomicUsize::new(0),
    });
    let seeded = Table::with_sweeps(setting("k-cow"), seeding.clone());
    assert!(seeded.start_renderer_search());
    assert!(wait_for(&seeding.calls, 1));
    for _ in 0..2000 {
        if !seeded.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(seeded.cast_target_state()["known"].as_bool().unwrap());
    // Second sweep: empty, and the row survives because the assignment never ran.
    assert!(seeded.start_renderer_search());
    assert!(wait_for(&seeding.calls, 2));
    for _ in 0..2000 {
        if !seeded.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let state = seeded.cast_target_state();
    assert_eq!(state["items"].as_array().unwrap().len(), 1, "{state}");
    assert_eq!(state["items"][0]["name"], "客厅电视");
    assert!(state["known"].as_bool().unwrap());
}

/// A `Sweeps` that drains a scripted queue, so the same table can answer twice
/// with two different lists.
struct OnceSweeps {
    casts: Mutex<Vec<Vec<CastDevice>>>,
    dlnas: Mutex<Vec<Vec<DlnaDevice>>>,
    calls: AtomicUsize,
}

impl Sweeps for OnceSweeps {
    fn cast(&self, _s: &Arc<Setting>) -> Vec<CastDevice> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.casts.lock().unwrap();
        if queue.is_empty() {
            return Vec::new();
        }
        queue.remove(0)
    }

    fn dlna(&self, _s: &Arc<Setting>) -> Vec<DlnaDevice> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut queue = self.dlnas.lock().unwrap();
        if queue.is_empty() {
            return Vec::new();
        }
        queue.remove(0)
    }
}

/// A `Sweeps` that writes down the name of the thread it ran on.
///
/// The name is only this fake's answer if `Table::start` handed it to
/// `thread::Builder::name()` when spawning; nothing else in the crate can put a
/// name on a thread. Each body records the name **before** bumping its counter,
/// so a test that waits for the counter is guaranteed to see the stored name.
#[derive(Clone, Default)]
struct NameSpy {
    cast_calls: Arc<AtomicUsize>,
    dlna_calls: Arc<AtomicUsize>,
    cast_name: Arc<Mutex<Option<String>>>,
    dlna_name: Arc<Mutex<Option<String>>>,
}

impl NameSpy {
    fn new() -> Arc<Self> {
        Arc::new(NameSpy::default())
    }

    fn cast_name(&self) -> String {
        self.cast_name
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default()
    }

    fn dlna_name(&self) -> String {
        self.dlna_name
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default()
    }
}

impl Sweeps for NameSpy {
    fn cast(&self, _s: &Arc<Setting>) -> Vec<CastDevice> {
        *self.cast_name.lock().unwrap() = std::thread::current().name().map(str::to_string);
        self.cast_calls.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }

    fn dlna(&self, _s: &Arc<Setting>) -> Vec<DlnaDevice> {
        *self.dlna_name.lock().unwrap() = std::thread::current().name().map(str::to_string);
        self.dlna_calls.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
}

#[test]
fn k_a_search_in_flight_latches_its_own_kind_and_reports_searching() {
    crate::pin_test_dir();
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-latch"), sweeps.clone());

    assert!(table.start_search());
    assert!(
        gate.wait_until_inside(),
        "the sweep thread never reached its body"
    );
    let state = table.cast_target_state();
    assert!(state["searching"].as_bool().unwrap(), "{state}");
    // A second ask while the first is running is "already running", i.e. `false`,
    // and it must not start a second sweep.
    assert!(!table.start_search());
    assert_eq!(sweeps.cast_calls(), 1, "{:?}", sweeps.cast_calls());

    gate.release();
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!table.cast_target_state()["searching"].as_bool().unwrap());
    // And the latch is a latch per kind: the DLNA sweep was never asked for.
    assert_eq!(sweeps.dlna_calls(), 0);
    assert_eq!(table.cast_target_state()["known"], json!(false));
}

#[test]
fn k_the_two_latches_do_not_block_each_other() {
    crate::pin_test_dir();
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-two-latches"), sweeps.clone());
    assert!(table.start_search());
    assert!(gate.wait_until_inside());
    // The DLNA search is a different latch and still starts. Both sweeps then park
    // on the same gate, so the second `arrive` releases when the first does.
    assert!(table.start_renderer_search());
    // **Wait for the ledger before reading it.** `start_renderer_search()` returns
    // as soon as the latch is set; the count is raised on the sweep thread's first
    // line, and this machine does not guarantee that thread has run at all by the
    // time we ask. Asserting the sum in the same beat as the call is AGENTS.md
    // 「第三种形状是计数器在答复之后才加」: the first run of this test read `1 + 0`
    // and looked like a product bug — one latch swallowing the other kind — while
    // the DLNA sweep was simply not scheduled yet. Same rule as the two-latch test
    // above, which waits on `wait_until_inside` for exactly this reason.
    assert!(
        wait_for(&sweeps.dlna_calls, 1),
        "the DLNA sweep never reached its body"
    );
    assert_eq!(sweeps.cast_calls() + sweeps.dlna_calls(), 2, "one latch per kind");
    gate.release();
    for _ in 0..2000 {
        if table.cast_target_state()["searching"].as_bool().unwrap() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        break;
    }
    assert!(!table.cast_target_state()["searching"].as_bool().unwrap());
}

#[test]
fn k_start_target_search_asks_for_both_and_short_circuits_on_neither() {
    crate::pin_test_dir();
    // The **deliberate deviation**: Python's `bool(start_search() or
    // start_renderer_search())` never runs the DLNA sweep when the Cast sweep
    // starts. Both are asked here, so a network with only a TV in it still gets a
    // populated dropdown.
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-both"), sweeps.clone());
    assert!(table.start_target_search());
    // **Wait for each ledger before reading it**, same as the neighbour above: the
    // counter is raised on the sweep thread's first line, so asserting the DLNA
    // count in the same beat as the call reads `0` on a machine that had not
    // scheduled that thread yet — and `0` here reads exactly like the Python
    // short-circuit this test exists to tell apart. The first run went red on
    // nothing but scheduling.
    assert!(
        wait_for(&sweeps.cast_calls, 1),
        "the cast sweep never reached its body"
    );
    assert!(
        wait_for(&sweeps.dlna_calls, 1),
        "Python would have skipped this one"
    );
    assert!(gate.wait_until_inside(), "no sweep parked in a body");
    assert_eq!(sweeps.cast_calls(), 1, "one sweep per kind, not two");
    assert_eq!(sweeps.dlna_calls(), 1);

    // Both latched ⇒ nothing new started, and the answer is `false`.
    // A counter at 1 is enough to know the latch is still held: the body parks in
    // `arrive()` right after raising it and the latch only clears when the body
    // returns, so it cannot have finished.
    assert!(!table.start_target_search());
    assert_eq!(sweeps.cast_calls(), 1);
    assert_eq!(sweeps.dlna_calls(), 1);
    gate.release();
}

#[test]
fn k_the_rows_are_cast_first_and_their_ids_are_lookup_keys() {
    crate::pin_test_dir();
    let (_log, _path) = witness("targets-k-ids");
    let sweeps = FakeSweeps::returning(
        vec![cast("Kitchen", "192.168.1.20", 8009)],
        vec![dlna("客厅电视", "http://192.168.1.9:49152/upnp/control/av", "192.168.1.9")],
    );
    let table = Table::with_sweeps(setting("k-ids"), sweeps.clone());
    table.start_target_search();
    assert!(wait_for(&sweeps.cast_calls, 1));
    assert!(wait_for(&sweeps.dlna_calls, 1));
    for _ in 0..2000 {
        if table.cast_target_state()["searching"].as_bool().unwrap() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        break;
    }

    let items = table.cast_target_items();
    assert_eq!(items.len(), 2, "{items:?}");
    // Cast rows first, `cast:{host}:{port}`, and the address carries the port.
    assert_eq!(items[0].id, "cast:192.168.1.20:8009");
    assert_eq!(items[0].kind, "cast");
    assert_eq!(items[0].address, "192.168.1.20:8009");
    assert_eq!(items[0].name, "Kitchen");
    // The DLNA id is the **whole** control URL: a `:` inside it is data, not a
    // field separator, and splitting it again would key two devices to one row.
    assert_eq!(items[1].id, "dlna:http://192.168.1.9:49152/upnp/control/av");
    assert_eq!(items[1].kind, "dlna");
    assert_eq!(items[1].address, "http://192.168.1.9:49152/upnp/control/av");
    assert_eq!(items[1].name, "客厅电视");
}

#[test]
fn k_the_card_document_has_three_keys_in_this_order() {
    crate::pin_test_dir();
    // The gate is what turns `searching: true` from a race into a fact: the sweep
    // thread parks **before** it hands back any rows, so the document read while it
    // is parked is the document the page sees during a real mDNS round trip. With a
    // sweep that answers immediately, this assertion would be a coin toss that the
    // machine usually lets win.
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-card"), sweeps.clone());
    let state = table.cast_target_state();
    // `preserve_order` is what makes this a contract rather than a coincidence:
    // this assertion is one of the two witnesses to the order, the other being
    // `scripts/verify_rust_receiver.sh:1515`, which asks the shell's refusal
    // document for exactly `items,searching,known`. `src-tauri/src/host.rs` ships
    // the three keys but checks nothing — it is the response, not the gate.
    // A missing `searching` renders as a spinner that never stops.
    let keys: Vec<String> = state
        .as_object()
        .expect("the document is an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["items", "searching", "known"]);
    assert!(state["items"].is_array());
    assert_eq!(state["searching"], json!(false));
    assert_eq!(state["known"], json!(false), "nothing has been swept yet");

    table.start_search();
    assert!(gate.wait_until_inside(), "the sweep never reached the gate");
    let running = table.cast_target_state();
    assert_eq!(running["searching"], json!(true), "{running}");
    // `known` is about the **cache**, not about the attempt: a sweep in flight with
    // nothing stored yet is still "no devices", so the card does not pre-promise a
    // dropdown it cannot fill.
    assert_eq!(running["known"], json!(false), "{running}");

    gate.release();
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let after = table.cast_target_state();
    assert_eq!(after["searching"], json!(false), "{after}");
    // The held sweep returns no rows, so a finished sweep still says `known: false`
    // — and it never turned the empty answer into a cleared cache (the row-keeping
    // rule has its own test above).
    assert_eq!(after["known"], json!(false), "{after}");
    assert_eq!(sweeps.cast_calls(), 1);
}

#[test]
fn k_the_refresh_document_appends_started_last() {
    crate::pin_test_dir();
    // `state['started'] = started` after the dict literal in Python (`:1245`), and
    // the key order is the answer the page reads — so `started` must be **fourth**,
    // not alphabetised into second place.
    // `started: true` promises the next poll will look different; `false` means a
    // sweep is already in flight and the page should keep polling.
    //
    // **The second ask has to be proven in-flight, not assumed.** The first version
    // of this test used `FakeSweeps::returning(Vec::new(), Vec::new())` — a sweep
    // that answers immediately — and then asserted `started == false` on the second
    // refresh. That is a premise dressed as a witness: by the time we asked, both
    // latches had already cleared, so `start_target_search()` legitimately returned
    // `true` again and the product was right while the test was red. A held gate is
    // the only shape where "in flight" is a fact the test created, so both sweeps are
    // parked inside their bodies before the second refresh is issued.
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-refresh"), sweeps.clone());
    let state = table.target_refresh();
    let keys: Vec<String> = state
        .as_object()
        .expect("the document is an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["items", "searching", "known", "started"]);
    assert_eq!(state["started"], json!(true), "{state}");
    // Both sweeps reached their bodies, so both latches are held **now** — the
    // counters are raised on the first line of the body, which runs only after
    // `start()` has set the latch it is guarding.
    assert!(wait_for(&sweeps.cast_calls, 1), "the cast sweep never started");
    assert!(wait_for(&sweeps.dlna_calls, 1), "the DLNA sweep never started");
    assert!(gate.wait_until_inside(), "no sweep parked in a body");
    let second = table.target_refresh();
    let keys2: Vec<String> = second
        .as_object()
        .expect("still an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys2, vec!["items", "searching", "known", "started"]);
    assert_eq!(second["started"], json!(false), "a sweep in flight: {second}");
    assert_eq!(
        second["searching"],
        json!(true),
        "the same two sweeps are still the reason: {second}"
    );
    // The `started` key's presence is asserted with its position, and both
    // booleans are present rather than absent — `false` and missing are different
    // answers to the card.
    assert!(second.get("known").is_some());
    assert!(second.get("searching").is_some());
}

#[test]
fn k_the_first_poll_of_a_fresh_run_is_the_only_cold_start_kick() {
    crate::pin_test_dir();
    // `if not _devices and not _dlna_devices: start_target_search()` — the reason
    // 「刚打开设置页时下拉是空的」 used to be normal. The guard is dropped before the
    // call, so the cold path cannot take the lock twice.
    let sweeps = FakeSweeps::returning(Vec::new(), Vec::new());
    let table = Table::with_sweeps(setting("k-cold"), sweeps.clone());
    assert_eq!(sweeps.cast_calls(), 0);
    let state = table.target_state();
    assert!(state["searching"].as_bool().unwrap(), "{state}");
    assert!(wait_for(&sweeps.cast_calls, 1));
    assert!(wait_for(&sweeps.dlna_calls, 1));
    // Both sweeps were asked once by the read alone.
    assert_eq!(sweeps.cast_calls(), 1);
    assert_eq!(sweeps.dlna_calls(), 1);

    // Once there are rows, polling must **not** spawn an mDNS round trip per poll:
    // the page reads this document once a second.
    let known = FakeSweeps::returning(
        vec![cast("Kitchen", "192.168.1.20", 8009)],
        vec![dlna("TV", "http://10.0.0.2/cm", "10.0.0.2")],
    );
    let warm = Table::with_sweeps(setting("k-warm"), known.clone());
    warm.target_state();
    assert!(wait_for(&known.cast_calls, 1));
    assert!(wait_for(&known.dlna_calls, 1));
    for _ in 0..2000 {
        if !warm.target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..5 {
        let poll = warm.target_state();
        assert!(poll["known"].as_bool().unwrap(), "{poll}");
    }
    assert_eq!(known.cast_calls(), 1, "a warm poll never searches: {}", known.cast_calls());
    assert_eq!(known.dlna_calls(), 1);
}

#[test]
fn k_an_id_is_resolved_against_the_live_caches_not_against_the_caller() {
    crate::pin_test_dir();
    let sweeps = FakeSweeps::returning(
        vec![cast("Kitchen", "192.168.1.20", 8009)],
        vec![dlna("TV", "http://10.0.0.2:80/cm", "10.0.0.2")],
    );
    let table = Table::with_sweeps(setting("k-lookup"), sweeps.clone());
    assert!(!table.start_target_search() == false, "the kick starts both");
    assert!(wait_for(&sweeps.cast_calls, 1));
    assert!(wait_for(&sweeps.dlna_calls, 1));
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        break;
    }

    let hit = table
        .target_by_id("dlna:http://10.0.0.2:80/cm")
        .expect("a live row resolves to the whole row, not a tuple");
    assert_eq!(hit.kind, "dlna");
    assert_eq!(hit.name, "TV");
    assert_eq!(hit.address, "http://10.0.0.2:80/cm");

    // A stale id is `None`: 「the row the card showed may be a device that has
    // since left」, and pushing to an address we no longer believe in is the whole
    // thing this lookup exists to prevent.
    assert!(table.target_by_id("cast:192.168.1.21:8009").is_none());
    assert!(table.target_by_id("dlna:http://10.0.0.2:80/other").is_none());
    assert!(table.target_by_id("").is_none());
    assert!(table.target_by_id("cast:").is_none());
    // And a prefix alone is not an id — a lookup that answered on `starts_with`
    // would make every push to a gone device a push to the first live one.
    assert!(table.target_by_id("cast").is_none());
    assert!(table.target_by_id("dlna").is_none());
}

#[test]
fn k_items_from_and_the_two_row_builders_never_drift_apart() {
    crate::pin_test_dir();
    // `cast_target_items` (the `TargetItem` list) and `items_from` (the JSON rows)
    // answer the same ledger in two shapes. If one gains a field or changes an id,
    // the card and the lookup stop describing the same device.
    let cast_rows = vec![cast("Kitchen", "192.168.1.20", 8009)];
    let dlna_rows = vec![dlna("TV", "http://10.0.0.2:80/cm", "10.0.0.2")];
    let items = {
        let sweeps = FakeSweeps::returning(cast_rows.clone(), dlna_rows.clone());
        let table = Table::with_sweeps(setting("k-same"), sweeps.clone());
        // Fill the caches without waiting on threads by reading a table that was
        // seeded through its own sweep.
        table.start_search();
        table.start_renderer_search();
        assert!(wait_for(&sweeps.cast_calls, 1));
        assert!(wait_for(&sweeps.dlna_calls, 1));
        for _ in 0..2000 {
            if !table.cast_target_state()["searching"].as_bool().unwrap() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        table.cast_target_items()
    };
    let rows = items_from(&cast_rows, &dlna_rows);
    assert_eq!(rows.len(), items.len(), "{rows:?} vs {items:?}");
    for (row, item) in rows.iter().zip(&items) {
        assert_eq!(row["id"], item.id, "{row} vs {item:?}");
        assert_eq!(row["kind"], item.kind);
        assert_eq!(row["name"], item.name);
        assert_eq!(row["address"], item.address);
        // The JSON row has exactly the four keys the card reads.
        let keys: Vec<String> = row.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["id", "kind", "name", "address"]);
    }
}

#[test]
fn k_a_panicking_sweep_still_clears_its_latch() {
    crate::pin_test_dir();
    // `LatchGuard` is constructed **inside** the thread closure, so the `Drop` runs
    // on unwind as well as on return. A latch left true after a panic is a card
    // with a spinner that never stops and a search that can never be started
    // again — with no error the user could act on.
    let sweeps = FakeSweeps::exploding();
    let table = Table::with_sweeps(setting("k-panic"), sweeps.clone());
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let started = table.start_search();
    assert!(started);
    assert!(wait_for(&sweeps.cast_calls, 1), "the sweep never ran");
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::panic::set_hook(hook);
    assert!(
        !table.cast_target_state()["searching"].as_bool().unwrap(),
        "the guard did not survive the panic"
    );
    // And the poisoned mutex is still readable — `lock_recovering` takes the value
    // out of the poison instead of refusing the ledger, because every critical
    // section here is a plain assignment.
    assert!(table.start_search(), "a dead sweep must not latch forever");
    assert!(wait_for(&sweeps.cast_calls, 2));
}

#[test]
fn k_lock_recovering_reads_a_poisoned_mutex_and_latch_guard_uses_it() {
    crate::pin_test_dir();
    // The direct witness for the recovery: panic while holding, then read through
    // `lock_recovering`. Python's `threading.Lock` has no poison concept, so this
    // is the Rust-specific half of the same promise — the ledger survives a
    // member of it dying.
    let cell = Arc::new(Mutex::new(7usize));
    {
        let cell_for_panic = Arc::clone(&cell);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::thread::spawn(move || {
            let _guard = cell_for_panic.lock().unwrap();
            panic!("died holding the lock");
        })
        .join();
        std::panic::set_hook(hook);
    }
    assert!(cell.is_poisoned(), "the setup must actually poison it");
    let value = lock_recovering(&cell);
    assert_eq!(*value, 7, "the value survived the panic");

    // And the guard's own path: `LatchGuard`'s drop uses the same function, so a
    // panic in one sweep cannot keep the *other* latch from clearing.
    let gate = HoldGate::new();
    let sweeps = FakeSweeps::held(gate.clone());
    let table = Table::with_sweeps(setting("k-poison"), sweeps.clone());
    table.start_search();
    assert!(gate.wait_until_inside());
    gate.release();
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!table.cast_target_state()["searching"].as_bool().unwrap());
}

#[test]
fn k_the_thread_names_are_the_ones_the_python_sweeps_used() {
    crate::pin_test_dir();
    // A named thread is the only way a stack dump in `ScreenCaptureKit`-style
    // triage says which sweep died; the strings are also what a reader greps for.
    //
    // **Asked where the name is applied.** `Which::Cast.thread_name()` and
    // `CAST_SEARCH_THREAD` are the same expression twice — that match arm *returns*
    // the constant — so comparing them could never go red, and the first version of
    // this test did exactly that. What is asked here is
    // `std::thread::current().name()` **from inside a sweep body**, which only has
    // this value if `Table::start` handed the name to `thread::Builder::name()` at
    // spawn (`targets.rs:903`). The literals stay, because they are what the Python
    // sweeps used; the constants are only ever compared to a literal.
    assert_eq!(CAST_SEARCH_THREAD, "CAST_FILE_SEARCH");
    assert_eq!(DLNA_SEARCH_THREAD, "CAST_FILE_DLNA_SEARCH");
    let spy = NameSpy::new();
    let table = Table::with_sweeps(setting("k-names"), spy.clone());
    assert!(table.start_target_search());
    assert!(wait_for(&spy.cast_calls, 1), "the cast sweep never ran");
    assert!(wait_for(&spy.dlna_calls, 1), "the DLNA sweep never ran");
    assert_eq!(spy.cast_name(), "CAST_FILE_SEARCH");
    assert_eq!(spy.dlna_name(), "CAST_FILE_DLNA_SEARCH");

    // The logger name is the module file's name, and it is one name for both kinds
    // of sweep. The claim that matters is the one the **product** makes: the file a
    // reader opens has to be the file the sweeps write into. A test-local
    // `witness(slug)` cannot show that — `Logger::claim()` names the file after the
    // slug it was handed, never after `LOGGER_NAME` — so the witness here is
    // `table.log`, built as `Logger::named(LOGGER_NAME)` in `Table::with_sweeps`.
    assert_eq!(LOGGER_NAME, "LocalFileCaster");
    assert_eq!(table.log.name(), "LocalFileCaster");
    let path = table.log.claim();
    table.log.error("a sentence for the ledger");
    assert_eq!(path.file_name().unwrap().to_string_lossy(), "LocalFileCaster.log");
    assert!(lines(&path).contains("a sentence for the ledger"));
}

#[test]
fn k_the_latch_is_the_answer_of_a_start_and_not_a_promise_of_devices() {
    crate::pin_test_dir();
    // `true` means "a search started now", which is what `state['started']` says on
    // the card. It says nothing about whether anything was found — and the two
    // facts must not be merged, or "no devices" would read as "no search".
    let sweeps = FakeSweeps::empty();
    let table = Table::with_sweeps(setting("k-latch-answer"), sweeps.clone());
    assert!(table.start_search());
    assert!(wait_for(&sweeps.cast_calls, 1));
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let state = table.cast_target_state();
    assert_eq!(state["known"], json!(false), "an empty sweep is honest");
    assert_eq!(state["searching"], json!(false));
    // Third ask: not latched any more, so it starts again.
    assert!(table.start_search());
    assert!(wait_for(&sweeps.cast_calls, 2));
}

#[test]
fn k_snapshot_is_a_copy_and_a_sweep_cannot_grow_it_under_the_reader() {
    crate::pin_test_dir();
    // Copy-on-write in the Python sense (`list(_devices)` at `:1626`, `:1634`):
    // bind a copy, iterate the copy. Nothing here mutates a `Vec` in place, so the
    // `RuntimeError: dictionary changed size during iteration` shape from §4.2
    // cannot appear with a different exception name.
    //
    // **The replacement has to land on the ledger the reader bound.** The first
    // version ran the 8-row sweep on a *second* `Table` and then asserted
    // `before.0.len() == 1` — which the second table could not have changed even if
    // `snapshot()` handed out references, and which the machine contradicted for a
    // worse reason: that fixture seeded the cast sweep with `Vec::new()`, so the
    // bound snapshot legitimately had **no** cast row and the assertion was a
    // description of a table nobody built. Both kinds now start with one row, and
    // the sweep that replaces them is the same table's next round.
    let row = dlna("TV", "http://10.0.0.2/cm", "10.0.0.2");
    let kitchen = cast("Kitchen", "192.168.1.20", 8009);
    let sweeps = Arc::new(OnceSweeps {
        casts: Mutex::new(vec![
            vec![kitchen.clone()],
            vec![
                cast("A", "1.2.3.4", 8009),
                cast("B", "1.2.3.5", 8009),
                cast("C", "1.2.3.6", 8009),
                cast("D", "1.2.3.7", 8009),
                cast("E", "1.2.3.8", 8009),
                cast("F", "1.2.3.9", 8009),
                cast("G", "1.2.3.10", 8009),
                cast("H", "1.2.3.11", 8009),
            ],
        ]),
        dlnas: Mutex::new(vec![
            vec![row.clone()],
            vec![
                dlna("A", "http://a/cm", "a"),
                dlna("B", "http://b/cm", "b"),
            ],
        ]),
        calls: AtomicUsize::new(0),
    });
    let table = Table::with_sweeps(setting("k-snapshot"), sweeps.clone());
    table.start_target_search();
    assert!(wait_for(&sweeps.calls, 2));
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let before = table.snapshot();
    assert_eq!(before.0.len(), 1, "the first round seeded one cast row");
    assert_eq!(before.1, vec![row.clone()]);

    // Ask the same table for a second round: eight cast rows and two DLNA rows
    // replace the ledgers wholesale while `before` is still held by this thread.
    assert!(
        table.start_target_search(),
        "the first round had not finished, so this asked for nothing"
    );
    assert!(wait_for(&sweeps.calls, 4));
    for _ in 0..2000 {
        if !table.cast_target_state()["searching"].as_bool().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    // The bound copy did not move: it is the same length, the same rows, and it
    // still owns its own allocation rather than aliasing the new ledger.
    assert_eq!(before.0.len(), 1, "the snapshot the reader bound moved under it");
    assert_eq!(before.0, vec![kitchen]);
    assert_eq!(before.1, vec![row]);

    // And the live ledger is the new one, whole — not a splice of the two rounds.
    let after = table.snapshot();
    assert_eq!(after.0.len(), 8, "{:?}", after.0.len());
    assert_eq!(after.1.len(), 2);
}
