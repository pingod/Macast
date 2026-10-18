//! The Chromecast and AirPlay faces of the Rust receiver: what a sender says
//! on the wire, and what we say back.
//!
//! Python counterparts: `macast/protocol_cast.py` and
//! `macast/protocol_airplay.py`. The references are files that grew every
//! lesson in AGENTS.md §4.1 and §4.9 into them, so this crate is ported in
//! layers rather than all at once: [`proto`] is the Cast envelope and its
//! namespaces, [`cert`] is the pair the TLS face runs on, [`channel`] is the
//! Cast channel on 8009, [`airplay`] is the AirPlay RTSP receiver on 7000,
//! then the setup faces on 8008 and 8443, then the mDNS announcement.
//!
//! ## What this crate owns, and what it deliberately does not
//!
//! It owns the **Cast wire**. The protobuf envelope, the five
//! `urn:x-cast:com.google.cast.*` namespaces, the JSON bodies of
//! `RECEIVER_STATUS` and `MEDIA_STATUS`, and the eureka-info document the setup
//! port serves — all built here, because all of them are a contract with a
//! sender stack we do not control. pychromecast, VLC, the Cast SDK and the
//! Google Home app each parse a little differently, none of them tells us why
//! it hung up, and the failure reads as "this device was found but nothing
//! happens" (§4.1). A byte we get wrong is not a crash log; it is a silent
//! refusal, which is why every shape in [`proto`] is pinned to bytes measured
//! out of the Python implementation rather than to the spec's prose.
//!
//! It does **not** own playback state or the player. Media arrives through
//! `macast_upnp::store::Renderer` and the transport state we report back is
//! read through `macast_upnp::store::StateStore`, both implemented by
//! `src-tauri` over the same tables DLNA already writes. A Cast LOAD and a
//! `SetAVTransportURI` are two doors into one living room: if each kept its own
//! copy of "what is playing and where", the settings page and the sender would
//! eventually answer from different tables, and AGENTS.md §4.8's oldest lesson
//! says that disagreement always shows up in front of a user.
//!
//! ## Layering
//!
//! * [`proto`] — the `CastMessage` protobuf codec (`_encode_varint`,
//!   `_field_bytes`, `_parse_fields`, `parse_cast_message`), the five
//!   namespaces, `app_namespaces()`, the device-auth answers, and the
//!   big-endian four-byte frame header.
//! * [`cert`] — the self-signed pair the TLS face runs on, generated
//!   **in-process** (`Service._ensure_self_signed_cert()` plus
//!   `ChromecastProtocol._ensure_cert()`): the `macast_cast.*` slot is adopted
//!   when a pair already sits in it, the shared `macast.*` pair is what this
//!   module writes, and neither is ever overwritten — a broken foreign pair is
//!   reported by name and left in place.
//! * [`channel`] — TLS on 8009, the namespace-only dispatch, the sender
//!   and session ledgers, volume, `LAUNCH_ERROR`, and the playback watcher.
//! * [`airplay`] — the AirPlay RTSP receiver on 7000: the eleven methods,
//!   session management, `Content-Location` parsing (header and SDP body),
//!   port fallback, and the `RendererCallback` seam the player answers through.
//! * [`mdns`] — the wire codec and the **browser**: `decode_name` with RFC 1035
//!   compression pointers, `parse_message`, the PTR→SRV→A/TXT ledger with
//!   RFC 6762 §10.1 goodbye handling, and `discover_cast`, which finds the
//!   *senders'* receivers on 5353 so the settings page has devices to name.
//!   Reading a reply is not the same job as broadcasting our own receiver —
//!   that half is [`announce`], and it deliberately keeps its own socket so the
//!   two never fight over what an incoming datagram means.
//! * [`setup`] — HTTP/8008 and HTTPS/8443: `/setup/eureka_info` with a **bare**
//!   UUID in `ssdp_udn`, `params=` filtering that keeps the document's key order,
//!   `/setup/icon.png`, and the two listeners whose ports a sender hardcodes.
//!   The face is ungated by design, and the module header says what that means.
//! * [`announce`] — the `_googlecast._tcp` **announcement**: PTR, SRV, TXT, NSEC
//!   and A written byte-for-byte the way zeroconf writes them (two frozen
//!   corpora under `tests/golden/` say so), three bursts on start, a re-announce
//!   every 60 s, and a zero-TTL goodbye on stop. **Not** here yet: answering
//!   somebody else's query — see the module header's last divergence and §15.
//!
//! ## The one thing to know before editing
//!
//! `RECEIVER_STATUS` and `MEDIA_STATUS` are JSON objects whose **key order is
//! the answer a sender reads**. Python builds them as dicts, so insertion order
//! is the wire format, and the tests were written against that order
//! byte-for-byte. The same is true of `app_namespaces()`: the schema is
//! `message Namespace { string name = 1; }`, so what goes out is
//! `[{"name": …}]` — a list of objects. Handing a sender a list of bare
//! strings is the single mistake that makes a device discoverable and
//! uncastable (§4.1), and it produces no error on our side at all.

pub mod airplay;
pub mod announce;
pub mod cert;
pub mod channel;
pub mod charmap;
pub mod cjk;
pub mod entities;
pub mod gbk;
pub mod html;
pub mod mdns;
pub mod proto;
pub mod push;
pub mod resolve;
pub mod relay;
pub mod respond;
pub mod setup;
pub mod targets;

pub use cert::{
    ensure_in_setting_dir, ensure_pair, CertError, Pair, Source, CAST_CERT_FILE, CAST_KEY_FILE,
    COMMON_NAME, SHARED_CERT_FILE, SHARED_KEY_FILE, SUBJECT_ALTERNATIVE_NAMES,
};
pub use channel::{
    launch_refusal, live_from_stream_type, random_session, sender_media_title, CastChannel,
    APP_BACKDROP_ID, CAST_PORT, DEFAULT_MEDIA_APP_ID, DEFAULT_MEDIA_APP_NAME,
    LOAD_GRACE_SECONDS, MAX_CONNECTIONS, PORT_FALLBACK_RANGE, RECEIVER_SOURCE,
    SERVED_APP_IDS, TITLE_MAX_CHARS, TRANSPORT_ID,
};
pub use proto::{
    app_namespaces, encode_auth_challenge, encode_auth_response, encode_cast_message, field_bytes,
    field_varint, frame_length, parse_cast_message, parse_fields, wrap_frame, CastMessage, Field,
    FieldValue, Payload, APP_NAMESPACES, FRAME_HEADER_LEN, NS_CONNECTION, NS_DEVICEAUTH,
    NS_HEARTBEAT, NS_MEDIA, NS_PLAYER_MESSAGE, NS_RECEIVER,
};

pub use airplay::{
    get_header, info_plist, parse_content_location, random_mac, reason_phrase, AirPlayReceiver,
    AirPlayState, RendererCallback, RtspRequest, AIRPLAY_PORT, AIRPLAY_SERVICE, FEATURES,
    IDLE_TIMEOUT_SECONDS, REASON_PHRASES, SUPPORTED_METHODS,
};

pub use targets::{
    describe_renderer, discover_renderers, local_name, locations_in, parse_description,
    port_suffix, ssdp_request, ssdp_search, sweep_renderers, Describer, DlnaDevice, Fetcher,
    HttpDescriber, HttpFetcher, RealSearcher, RealSweeps, Searcher, SsdpWire, Table, TargetItem,
    AVTRANSPORT_SUFFIX, CAST_SEARCH_THREAD, DESCRIPTION_MAX_BYTES, DLNA_SEARCH_TARGETS,
    DLNA_SEARCH_THREAD, DLNA_SERVICE, LOGGER_NAME, SEARCH_TIMEOUT, SSDP_ADDR, SSDP_PORT,
    SSDP_READ_MAX, SSDP_RECV_TIMEOUT,
};

pub use setup::{
    answer, eureka_info, Response, SetupServer, EUREKA_PATH, HTTPS_SETUP_PORT, ICON_PATH,
    SETUP_CONN_THREAD, SETUP_PORT, SETUP_THREAD, SETUP_TLS_THREAD,
};

/// The config directory **this crate's tests** log into, pinned per process.
///
/// The reason is a fact that is easy to assert and impossible to feel:
/// `macast_core::paths::setting_dir()` guards its "a test binary must never
/// resolve to the real config directory" fallback with `#[cfg(test)]`, and
/// `cfg(test)` is set for the crate **being tested**, not for the crates it
/// depends on. Build `cargo test -p macast-cast` and `macast-core` is compiled
/// as a normal dependency with that arm removed, so `paths::log_file()` resolves
/// to `~/Library/Application Support/Macast/macast.log` — the user's real log.
///
/// Nothing in [`proto`] writes a log record yet. That is not why this exists.
/// It exists because the next layer does (`Logger::named("Chromecast"`), because
/// AGENTS.md §10 forbids verifying a receiver by writing into his configuration,
/// and because a test whose witnesses read a file that outlives the run is not a
/// test — the append-mode log would already contain the previous run's lines, and
/// every "exactly one line says X" claim would fail on correct code. The pin is
/// therefore installed *before* the first layer that needs it and checked by
/// [`proto::tests`]'s own witness, so it is a working mechanism today rather than
/// a comment about tomorrow.
///
/// Every `#[test]` in this crate calls it as its **first statement**, which is
/// what makes the ordering hold whichever thread reaches a log record first.
/// `every_test_pins_the_log_directory_first` in `proto::tests` checks that no
/// test was added without the call.
#[cfg(test)]
pub(crate) fn pin_test_dir() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        let dir = std::env::temp_dir().join(format!("macast-cast-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        // The same thing `clear_env()` does when the app starts (AGENTS.md
        // §4.10): a reused pid must not inherit the previous run's log, or the
        // counts are the previous run's again.
        macast_core::logging::remove_log_files(&dir);
        macast_core::paths::set_setting_dir_override(&dir);
    });
}

/// One test's turn at the **process-wide** DEBUG threshold, with the gate that
/// makes the turn actually belong to it.
///
/// `macast_core::logging::THRESHOLD` is a `static AtomicU8` shared by every
/// thread in the test binary (logging.rs:69), and [`macast_core::logging::Logger::log`]
/// early-returns on `level < threshold()` **before** touching the file. So a
/// DEBUG witness file that is empty is not "the write has not landed yet" — the
/// writes are synchronous (`rotate_write` appends per record) — it is "some other
/// thread put the threshold back to INFO while I was reading it".
///
/// That is exactly what this crate measured before this type existed:
/// **5 red out of 24 parallel full-crate runs (~21%)**, always `601 passed;
/// 1 failed`, always one of the two `mdns_tests` DEBUG witnesses
/// (`mdns_tests.rs:677` / `:721`), every panic body ending `witness was:` and
/// then nothing. The same 602 tests run **0 red out of 12 serial
/// (`--test-threads=1`) runs**. The colliding writer was `targets_tests`'s
/// threshold reset firing inside an `mdns` window.
///
/// This is AGENTS.md §4.2 「并行跑的用例共享夹具路径，单跑绿、整组红」 with the
/// shared fixture being a **global in another crate** rather than a path, and the
/// "single test passes" half of it showing up as *module-level* green: running
/// `mdns_tests` alone is 20/20, because the only thing it collides with lives in
/// `targets_tests`. A per-module run is therefore not evidence here, and the
/// reading that convicted it had to come from repeating the whole crate.
///
/// The gate is held for the whole window, so at most one test is in DEBUG at a
/// time, and `Drop` restores INFO **before** releasing the lock — otherwise the
/// next test's `armed()` could set DEBUG while this one's restore was still
/// pending on another thread and the two would swap again, one level apart.
///
/// `set_threshold` is therefore only allowed to be called here;
/// `proto::tests::only_lib_rs_touches_the_log_threshold` audits that, because a
/// raw pair added anywhere else re-creates the flake and no test in this crate
/// can be trusted to catch it — by construction, it is intermittent.
/// The same guard seen from the other side: a test that asserts its witness file
/// is **empty** is decided by whichever neighbour holds a window. `targets_tests`
/// measured it on 2026-10-18 — **12 red out of 14** `--lib targets` runs before its
/// silence assertions were rephrased by level (`targets_tests::loud`, which ignores
/// DEBUG records and fails on anything INFO or above), and **0 red out of 6** whole
/// crate runs the same day. This type cannot help those tests, because they must not
/// wait for a window to close — they must not care whether one is open — and the 0
/// red is not "fixed": it is the crate run not scheduling the collision, which is
/// why a round that only ever runs the whole crate would have closed this as done.
#[cfg(test)]
pub(crate) struct DebugWindow {
    // Dropped *after* the `Drop` body, which is the ordering the doc above
    // depends on: restore INFO, then let the next waiter in.
    _gate: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl DebugWindow {
    /// Wait for the gate, then raise the threshold to DEBUG for the turn of the
    /// caller. The returned value must be bound to a **variable** (not `_`), or
    /// the drop happens on this line and the window is closed before the log
    /// record is written.
    pub(crate) fn armed() -> DebugWindow {
        // Poisoned is fine and expected: a test that panicked inside a window
        // left the threshold at DEBUG, which is the one thing the next holder
        // must not inherit — so take the lock anyway and set the level.
        let gate = DEBUG_WINDOW_GATE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        macast_core::logging::set_threshold(macast_core::logging::Level::Debug);
        DebugWindow { _gate: gate }
    }
}

#[cfg(test)]
impl Drop for DebugWindow {
    fn drop(&mut self) {
        macast_core::logging::set_threshold(macast_core::logging::Level::Info);
    }
}

#[cfg(test)]
static DEBUG_WINDOW_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
