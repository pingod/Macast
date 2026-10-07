//! The two sides this crate is bolted to, plus the tables that say which
//! state variables anybody is listening to.
//!
//! Rust counterpart of `macast/protocol.py:29-45` (the `SERVICE_STATE_OBSERVED`
//! literal), `macast/protocol.py:998-1043` (`set_state`), and the renderer
//! seam that `PlaybackGuard` (`macast/protocol.py:164-260`) stands on.
//!
//! Why traits and not concrete types: the DLNA face must not own the playback
//! state. In this workspace [`crate::scpd::Service`] is only the schema — the
//! values a control point reads back are driven from `src-tauri`, whose
//! `StateTable` is the single source for what the phone reads back, and whose
//! `Player` is the only thing that talks to mpv. If the crate kept its own copy
//! of either, the settings page and the GENA events would answer from different
//! tables — and AGENTS.md §4.8 has already learned that "two places giving the
//! same number a different way" is worse than no number.
//!
//! What the crate *does* own is the wire format. Everything a control point
//! parses comes out of [`wire`], and that function is deliberately not the same
//! as `src-tauri`'s page renderer: Python puts `str(value)` into SOAP and GENA
//! bodies, so a stored boolean reaches the wire as `True`/`False`, while the
//! settings page's own table maps booleans to `1`/`0` (`src-tauri/src/host.rs`,
//! `render`). Both are correct on their own side of the seam. Change one and
//! the other does not follow — that is the point, not an oversight.

use serde_json::Value;
use std::sync::Arc;

/// AVTransport state variables a control point may SUBSCRIBE to.
/// `macast/protocol.py:33-45`, verbatim, in order.
pub const AVTRANSPORT_OBSERVED: [&str; 6] = [
    "TransportState",
    "TransportStatus",
    "CurrentMediaDuration",
    "CurrentTrackDuration",
    "CurrentTrack",
    "NumberOfTracks",
];

/// RenderingControl observed variables. Same source.
pub const RENDERING_CONTROL_OBSERVED: [&str; 2] = ["Volume", "Mute"];

/// ConnectionManager observed variables. Same source.
///
/// These are handed to a subscriber's *initial* event (so a new control point
/// learns the protocol info and connection ids the moment it subscribes), yet
/// `set_state`'s queue gate never lists this service — a later change to one of
/// them is announced to nobody. That asymmetry is upstream's, and it is
/// preserved rather than "fixed", because a control point that suddenly starts
/// receiving `ConnectionID` property changes mid-session is a behaviour change
/// no one has a TV to test.
pub const CONNECTION_MANAGER_OBSERVED: [&str; 3] = [
    "A_ARG_TYPE_Direction",
    "SinkProtocolInfo",
    "CurrentConnectionIDs",
];

/// The union that `set_state` will queue for: AVTransport ∪ RenderingControl.
/// `macast/protocol.py:1027-1028` names those two services and only those two.
pub const OBSERVED: [&str; 8] = [
    "TransportState",
    "TransportStatus",
    "CurrentMediaDuration",
    "CurrentTrackDuration",
    "CurrentTrack",
    "NumberOfTracks",
    "Volume",
    "Mute",
];

/// Is anyone told when this variable changes? (`set_state`'s gate.)
pub fn is_observed(name: &str) -> bool {
    OBSERVED.contains(&name)
}

/// The variables whose *current value* goes into a new subscriber's initial
/// event for this service (`send_init_event`, `macast/protocol.py:770`).
pub fn observed_for(service: &str) -> &'static [&'static str] {
    match service {
        "AVTransport" => &AVTRANSPORT_OBSERVED,
        "RenderingControl" => &RENDERING_CONTROL_OBSERVED,
        "ConnectionManager" => &CONNECTION_MANAGER_OBSERVED,
        _ => &[],
    }
}

/// Where DLNA reads and writes playback state. Implemented over
/// `src-tauri`'s `StateTable`.
///
/// `write` returns whether the value actually changed, because `set_state`
/// computes `changed` *before* it touches anything else and uses that one
/// boolean for both the debug log line and the broadcast queue
/// (`macast/protocol.py:998-1043`). Broadcasting an unchanged value was 92% of
/// one user's log volume (AGENTS.md §4.2, "状态变了才广播"), so the answer is
/// load-bearing rather than informative.
///
/// `write` must **not** enqueue a GENA event itself: deciding who hears about a
/// change belongs to [`crate::eventing`], which is the only place that knows the
/// queue, the condvar, and the subscriber table.
pub trait StateStore: Send + Sync {
    /// Current value of a state variable, or `None` when this name is not in
    /// the table at all. `None` is how an unknown variable is reported;
    /// `Some(Value::Null)` means the table genuinely holds a null.
    fn read(&self, name: &str) -> Option<Value>;

    /// Store a value; `true` if it differs from what was there.
    fn write(&self, name: &str, value: Value) -> bool;

    /// Append a URI to the playlist if it is not already there, and return the
    /// pair the two state variables need: `(CurrentTrack, NumberOfTracks)` —
    /// i.e. 1-based index and total, exactly as
    /// `macast/protocol.py:1130-1150` computes them.
    fn playlist_push(&self, uri: &str) -> (i64, i64);

    /// The playlist, in order. `_playlist_step` reads it and indexes it.
    fn playlist(&self) -> Vec<String>;

    /// `PlaybackGuard._add_history`: dedupe by URI, keep the newest 50, never
    /// raise. A play-history write is a convenience and must not be able to
    /// fail a cast.
    fn add_history(&self, title: &str, uri: &str);
}

/// The thing that actually plays. Implemented over `src-tauri`'s `Player`.
///
/// Method names mirror `macast/renderer.py`'s `Renderer` surface, and two of
/// them are load-bearing in ways that look arbitrary:
///
/// * [`Renderer::note_media_live`] is deliberately **not** called
///   `set_media_live`. `Renderer.methods()` in Python enumerates everything
///   prefixed `set_media_` and `MacastPluginManager` subscribes each of those
///   names to the CherryPy bus, so naming this one `set_media_live` would hang
///   a new message on the bus that nobody publishes and make every built-in
///   renderer look like it should implement it (AGENTS.md §4.2, the
///   `media_is_live` trap, and Part 64's L section).
/// * [`Renderer::set_media_url`] and [`Renderer::set_media_title`] are separate
///   calls because "the address changed" and "the name changed" are two
///   different facts, and every path that swaps the first must also swap the
///   second (AGENTS.md §4.2, "改播放器的地址，就必须改播放器的名字"). A port that
///   folds them into one call loses the third-party renderers that legitimately
///   have no title to set.
pub trait Renderer: Send + Sync {
    /// Hand the player the sender's own "is this a live stream" answer
    /// immediately before the URL. `None` is **not** `false`: it means "keep
    /// today's behaviour", which is the safe side for anything we do not
    /// understand (AGENTS.md §4.1, last row).
    fn note_media_live(&self, live: Option<bool>);

    fn set_media_url(&self, uri: &str);
    fn set_media_title(&self, title: &str);
    fn set_media_position(&self, seconds: f64);
    fn set_media_volume(&self, value: Value);
    fn set_media_mute(&self, value: Value);
    fn set_media_pause(&self);
    fn set_media_resume(&self);
    fn set_media_stop(&self);

    /// Best-effort name for a bare URI — used when the sender sent an address
    /// and no title (`cast_uri`, `_playlist_step`).
    fn title_for_uri(&self, uri: &str) -> String;
}

/// The few things outside this crate that a protocol answer depends on.
pub trait Hooks: Send + Sync {
    /// `cherrypy.engine.publish('app_notify', title, text)` — a tray
    /// notification.
    fn notify(&self, title: &str, text: &str);

    /// `Setting.stop_service()`. SSDP calls this when it cannot bind UDP/1900:
    /// a receiver that announces nothing while the settings page says
    /// "running" is the worse failure.
    fn stop_service(&self);
}

/// `str(value)` as CPython would produce it, for the bytes that go on the wire.
///
/// Every SOAP response body, every `<LastChange val="...">`, and every
/// ConnectionManager propertyset is built by calling `str()` on a stored value
/// in Python, so the stored types matter as much as the stored strings:
/// `set_state_mute(bool)` stores a real Python bool, and the wire answer for
/// muted is `True` / `False` — not `1` / `0`. That has been measured in real
/// SOAP text and in real GENA `val=` attributes on this machine.
///
/// See the module header for why this is not `src-tauri`'s page renderer.
pub fn wire(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                py_float(f)
            } else {
                n.to_string()
            }
        }
        // A list or dict is not something our own writers produce; the closest
        // honest answer is what Python prints for it, and JSON is that for the
        // shapes we could ever receive (a control point sending a JSON array in
        // an argument string would be stored as a *string* anyway).
        other => other.to_string(),
    }
}

/// `str(float)` for the narrow range of floats this table can hold.
///
/// Python's `str()` keeps trailing zeros (`1.0`, not `1`) and uses
/// shortest-repr for everything else. `serde_json` would have stored an
/// integral value as an integer, so reaching here with a whole number means the
/// input literally had a decimal point.
fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    let text = format!("{}", f);
    if text.contains('.') || text.contains('e') || text.contains("inf") || text.contains("NaN") {
        return text;
    }
    format!("{}.0", text)
}

/// The eight names are one table, and `src-tauri` used to keep a copy.
/// A unit test here is the cheapest way to make a drift between the crate's
/// gate and the crate's docs impossible.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_is_the_union_of_the_two_queued_services() {
        crate::pin_test_dir();
        let mut union: Vec<&str> = Vec::new();
        for name in AVTRANSPORT_OBSERVED {
            union.push(name);
        }
        for name in RENDERING_CONTROL_OBSERVED {
            union.push(name);
        }
        assert_eq!(union, OBSERVED.to_vec());
        // ConnectionManager's names are in the *initial event* only.
        for name in CONNECTION_MANAGER_OBSERVED {
            assert!(!is_observed(name), "{} must not be queued", name);
            assert!(observed_for("ConnectionManager").contains(&name));
        }
    }

    #[test]
    fn wire_renders_like_python_str_not_like_json() {
        crate::pin_test_dir();
        assert_eq!(wire(&Value::Bool(true)), "True");
        assert_eq!(wire(&Value::Bool(false)), "False");
        assert_eq!(wire(&Value::Null), "None");
        assert_eq!(wire(&serde_json::json!(0)), "0");
        assert_eq!(wire(&serde_json::json!(1.0)), "1.0");
        assert_eq!(wire(&serde_json::json!("00:01:02")), "00:01:02");
        // The page renderer's answer is the other one, on purpose.
        assert_ne!(wire(&Value::Bool(true)), "1");
    }

    #[test]
    fn an_unknown_service_has_no_observed_names() {
        crate::pin_test_dir();
        assert!(observed_for("").is_empty());
        assert!(observed_for("AVTransport ").is_empty());
    }
}

/// Keep the trait objects' `Arc` in the same place so callers cannot hand a
/// `Service` a store that is dropped while a thread still reads it.
pub type SharedStore = Arc<dyn StateStore>;
pub type SharedRenderer = Arc<dyn Renderer>;
pub type SharedHooks = Arc<dyn Hooks>;
