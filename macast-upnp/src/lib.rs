//! The DLNA face of the Rust receiver: who we announce to the LAN, and what we
//! answer when someone asks.
//!
//! Python counterparts: `macast/ssdp.py` (the announcement socket),
//! `macast/plugin.py::SSDPPlugin` (the six USNs a MediaRenderer advertises),
//! `macast/protocol.py` (description, SCPD, and the SOAP dispatcher in
//! `DLNAHandler.call`). GENA eventing is the one face still unwritten —
//! [`store::Broadcast`] is the seam that is waiting for it.
//!
//! ## What this crate owns, and what it deliberately does not
//!
//! It owns the **wire**. Every byte in an SSDP response, and later every SOAP
//! envelope and GENA body, is built here, because those bytes are a contract
//! with control points we do not control and cannot debug from this machine.
//!
//! It does **not** own playback state or the player. Those arrive through the
//! four traits in [`store`] — [`store::StateStore`], [`store::Renderer`],
//! [`store::Hooks`], [`store::Broadcast`] — implemented by `src-tauri` over its
//! own `StateTable` and `Player`. The reason is AGENTS.md §4.8's oldest lesson: two places that each
//! keep a copy of the same number will eventually disagree in front of a user,
//! and a receiver whose settings page and whose `<LastChange>` answer from
//! different tables is a bug report that reads as "the TV is lying".
//!
//! ## Layering
//!
//! * [`sock`] — one UDP *sender* per interface, and the two platform shapes of
//!   `IP_MULTICAST_IF`.
//! * [`ssdp`] — the receiver on UDP/1900, the registry of announced devices,
//!   `ssdp:alive`, `ssdp:byebye`, and the M-SEARCH answer.
//! * [`device`] — the six USNs, the `LOCATION` that still contains a `{}` for
//!   the sender to fill in, and the restart-under-a-lock that an IP change
//!   needs.
//! * [`scpd`] — the service descriptors: the flat state table and the action
//!   lists that `macast/xml/*.xml` declare, read with lxml's navigation rules
//!   because a control point's `GetStateVariables` answer is those rules.
//! * [`description`] — the one document that is *filled in* rather than built:
//!   `macast/xml/Description.xml` with eleven names substituted, byte for byte
//!   the same file the shipped receiver serves.
//! * [`soap`] — the action dispatcher: read the envelope the way lxml reads it,
//!   write the state variables the SCPD's `<argumentList>` says to write, answer
//!   with `<u:{Action}Response>`, and refuse with a real SOAP fault. It hands
//!   the media to [`store::Renderer`] and the changed values to
//!   [`store::Broadcast`]; it never keeps a copy of either.
//!
//! ## The one thing to know before editing
//!
//! Python dict **insertion order** is a wire format here. `register()` writes
//! `USN, LOCATION, ST, EXT, SERVER, CACHE-CONTROL`, and the NT-rewrite in
//! `do_notify`/`do_byebye` (`copy`, `NT = ST`, `del ST`) yields
//! `USN, LOCATION, EXT, SERVER, CACHE-CONTROL, NT` — a different order, on
//! purpose, and the one real control points were tested against. The registry
//! in [`ssdp`] is therefore an ordered `Vec`, not a `HashMap`, and a "tidier"
//! map would shuffle headers in a way no test on this machine can see.

pub mod description;
pub mod device;
pub mod scpd;
pub mod soap;
pub mod sock;
pub mod ssdp;
pub mod store;

/// The config directory **this crate's tests** log into, pinned per process.
///
/// This exists because of a fact that is easy to assert and impossible to feel:
/// `macast_core::paths::setting_dir()` guards its "a test binary must never
/// resolve to the real config directory" fallback with `#[cfg(test)]`, and
/// `cfg(test)` is set for the crate **being tested**, not for the crates it
/// depends on. So when `cargo test -p macast-upnp` builds `macast-core` as a
/// normal dependency, that arm is compiled out and `paths::log_file()` here
/// resolves to `~/Library/Application Support/Macast/macast.log` — the user's
/// real log.
///
/// Two separate damages follow from that, and only the first is obvious:
///
/// 1. AGENTS.md §10 — verifying this crate would write to his configuration.
/// 2. **The witnesses would be reading a file that outlives the run.** Every
///    `lines.len() == 1` / "these two lines are adjacent" claim in [`ssdp`]'s
///    tests keys on a needle that is unique within one run and byte-identical
///    across runs, while that log is opened in append mode by whoever runs next.
///    The second `cargo test` on a machine would find four of them and fail — on
///    a correct product, for a reason that has nothing to do with the code. A
///    test whose truth depends on nobody having run it before is not a test.
///
/// Every `#[test]` in this crate calls it as its **first statement**, which is
/// what makes the ordering hold: whichever thread reaches a log record first has
/// already installed this pin, so the process-global directory is decided before
/// the first byte is written.
/// `assert_the_suite_never_logs_into_the_user_directory` in `ssdp`::tests checks
/// that it stayed that way, and `every_test_pins_the_log_directory_first` checks
/// that no test was added without the call.
#[cfg(test)]
pub(crate) fn pin_test_dir() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        let dir = std::env::temp_dir().join(format!("macast-upnp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        // The same thing `clear_env()` does when the app starts (AGENTS.md
        // §4.10): a reused pid must not inherit the previous run's log, or the
        // counts are the previous run's again.
        macast_core::logging::remove_log_files(&dir);
        macast_core::paths::set_setting_dir_override(&dir);
    });
}

pub use device::{
    SsdpPlugin, AVTRANSPORT_URN, CONNECTION_MANAGER_URN, RENDERING_CONTROL_URN, ROOTDEVICE,
    UPNP_DEVICE,
};
pub use ssdp::{SsdpServer, SSDP_ADDR, SSDP_PORT};
pub use store::{
    is_observed, observed_for, wire, Broadcast, SharedBroadcast, SharedHooks, SharedRenderer,
    SharedStore, StateStore, Renderer, Hooks, AVTRANSPORT_OBSERVED, CONNECTION_MANAGER_OBSERVED,
    OBSERVED, RENDERING_CONTROL_OBSERVED,
};
