//! The Chromecast face of the Rust receiver: what a sender says on the wire,
//! and what we say back.
//!
//! Python counterpart: `macast/protocol_cast.py`. The reference is one file of
//! 1298 lines that grew every lesson in AGENTS.md §4.1 and §4.9 into it, so this
//! crate is ported in layers rather than all at once: [`proto`] is the envelope
//! and the namespaces, [`cert`] is the pair the TLS face runs on, then the
//! channel on 8009, then the setup faces on 8008 and 8443,
//! then the mDNS announcement.
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
//! * *(next)* `channel` — TLS on 8009, the namespace-only dispatch, the sender
//!   and session ledgers, volume, `LAUNCH_ERROR`, and the playback watcher.
//! * *(next)* `setup` — HTTP/8008 and HTTPS/8443: `/setup/eureka_info` with a
//!   **bare** UUID in `ssdp_udn`, `params=` filtering, `/setup/icon.png`.
//! * *(next)* `mdns` — the `_googlecast._tcp` announcement and its TXT records.
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

pub mod cert;
pub mod proto;

pub use cert::{
    ensure_in_setting_dir, ensure_pair, CertError, Pair, Source, CAST_CERT_FILE, CAST_KEY_FILE,
    COMMON_NAME, SHARED_CERT_FILE, SHARED_KEY_FILE, SUBJECT_ALTERNATIVE_NAMES,
};
pub use proto::{
    app_namespaces, encode_auth_challenge, encode_auth_response, encode_cast_message, field_bytes,
    field_varint, frame_length, parse_cast_message, parse_fields, wrap_frame, CastMessage, Field,
    FieldValue, Payload, APP_NAMESPACES, FRAME_HEADER_LEN, NS_CONNECTION, NS_DEVICEAUTH,
    NS_HEARTBEAT, NS_MEDIA, NS_PLAYER_MESSAGE, NS_RECEIVER,
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
