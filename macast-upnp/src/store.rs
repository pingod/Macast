//! The four sides this crate is bolted to — the state table, the renderer, the
//! app hooks, and the GENA queue — plus the tables that say which state
//! variables anybody is listening to.
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
    /// `macast/protocol.py:1117-1122` computes them.
    fn playlist_push(&self, uri: &str) -> (i64, i64);

    /// The playlist, in order. `_playlist_step` reads it and indexes it.
    fn playlist(&self) -> Vec<String>;

    /// Which playlist entry was last handed to the player: a 0-based index, or
    /// `-1` when nothing has been cast yet (`self.current_index = -1`,
    /// `macast/protocol.py:607`).
    ///
    /// This is **not** `CurrentTrack - 1`, which is the obvious reading and
    /// wrong. Those two state variables have a second writer that does not know
    /// anything about our playlist: the player reports its own track list and
    /// the renderer answers with `CurrentTrack = 0 if tracks == 0 else 1` and
    /// `NumberOfTracks = tracks` (`macast_renderer/mpv.py:320-324`). Both names
    /// are in `SERVICE_STATE_OBSERVED['AVTransport']`, so both writers also
    /// broadcast GENA, and after a load that leaves mpv with one audio track the
    /// table reads `1`/`1` whether or not the playlist holds one URI or nine.
    ///
    /// The collision is **kept, not fixed**. It is upstream behaviour that real
    /// control points already live with, and the pair is only ever *displayed*
    /// back to them — nothing of ours reads `CurrentTrack` to decide what to
    /// play next. Deriving the index from the table would turn a cosmetic
    /// disagreement into a wrong cast, which is the one outcome this trait exists
    /// to prevent. It also would have looked safe at boot, where the default
    /// `CurrentTrack` of `0` and `current_index` of `-1` agree by accident.
    fn playlist_index(&self) -> i64;

    /// Record the index [`Self::playlist_index`] should report. Every site that
    /// swaps the played address sets it (`SetAVTransportURI`, `cast_uri`,
    /// `_playlist_step`); a site that forgets leaves the next `Next`/`Previous`
    /// stepping from the previous item's position.
    fn set_playlist_index(&self, index: i64);

    /// `PlaybackGuard._add_history`: dedupe by URI, keep the newest 50, never
    /// raise. A play-history write is a convenience and must not be able to
    /// fail a cast.
    fn add_history(&self, title: &str, uri: &str);

    /// The rows [`StateStore::add_history`] wrote, newest first — the read
    /// `query=status` prints as its `history` list.
    ///
    /// A tuple of `(title, uri, time)` rather than a type invented here, because
    /// the shape belongs to whoever keeps the rows; the three names are the
    /// reference's (`protocol.py:1211-1215` writes `uri`, `title`, `time`), and
    /// `time` is the epoch second the row was pushed, which the 「播放历史」 card
    /// sorts and prints.
    ///
    /// An empty list has **two causes** in this port and one in the reference: the
    /// rows live in memory here, so a restart empties them while the Python app's
    /// list survives in `Play_History`. `query=status` therefore cannot tell "nobody
    /// has cast anything" from "the casts were forgotten on relaunch" — the divergence
    /// is recorded in `docs/Rust-Port-Design.md` rather than papered over by reading
    /// a settings key this shell never writes.
    fn play_history(&self) -> Vec<(String, String, i64)>;

    /// `Protocol.clear_play_history` (`protocol.py:1220-1226`): the ledger becomes an
    /// **empty list**, and on disk as well as in memory — the reference writes
    /// `Setting[Play_History] = []` rather than deleting the key, which is the same
    /// rule 模块设置 follows for a scalar it clears (§16 第 22 条: a page that reads
    /// 「清空过」 and a page that reads 「从未投过东西」 need not be the same page, but
    /// the key's presence is what makes the boot log say the right one).
    ///
    /// The reference wraps the write in `try/except` and answers
    /// `{'code': 1, 'message': 'clear failed'}`. **That arm has no shape here** and the
    /// reason is recorded rather than hidden: `Setting::save` swallows the same I/O
    /// errors Python would raise and logs them where they happen, so a `clear failed`
    /// sentence would be a document for a state nothing can reach (§16 第 34 条's
    /// mirror-switch reasoning, same shape).
    ///
    /// This is the **one** ledger write the HTTP face performs. State variables stay
    /// SOAP's alone (AGENTS.md §5's ownership rule, and [`crate::http::Face`]'s `store`
    /// field says so); `Play_History` is not a state variable — it is the list the
    /// 「播放历史」 card prints, and in Python the same page button writes it through the
    /// same door.
    fn clear_history(&self);
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

    /// Hand the player the address, and where in it the sender asked to start.
    ///
    /// `start` is the sender's own clock text, verbatim, or `None` for "no
    /// opinion". The two callers of this method differ in exactly this
    /// parameter, which is why it lives on the trait rather than being tacked
    /// onto one of them:
    ///
    /// * DLNA's `SetAVTransportURI` carries **no** start position
    ///   (`macast/protocol.py:1180`-`1250`), so [`crate::soap`] passes `None`,
    ///   and a sender that wants a position sends `Seek` afterwards;
    /// * Cast's `LOAD` does — `renderer.set_media_url(url,
    ///   start=str(int(self._position)))` (`macast/protocol_cast.py:995`) — and
    ///   the position is already sitting in that message.
    ///
    /// `None` and `Some("0")` are the same request, not two behaviours: the
    /// reference's own default for `start` is `"0"` (`macast/renderer.py:76`),
    /// and the player drops a position that is empty or literally `0` so the
    /// mpv command keeps the bare `["loadfile", url, "replace"]` shape instead
    /// of growing a start option for a position that is already the beginning
    /// (`macast-mpv/src/player.rs:502`).
    fn set_media_url(&self, uri: &str, start: Option<&str>);
    fn set_media_title(&self, title: &str);

    /// `Seek`'s `Target`, handed on **as the sender's text**.
    ///
    /// This took a number before, which was a misreading of both sides of the
    /// seam. The reference passes `data['Target'].value` — the argument's text,
    /// possibly `None` (macast/protocol.py:1238-1244) — and the sibling crate's
    /// player already takes `&str` and writes `["seek", <text>, "absolute"]`
    /// (`macast-mpv/src/player.rs:533`). Measured on this machine against mpv
    /// v0.41.0 (private instance, `/tmp/seek_arg_probe.py`), where mpv's own
    /// time parser is the thing on the other end of that command:
    ///
    /// * `["seek", "00:01:23", "absolute"]` → `error running command` — the
    ///   argument was **accepted**, and it failed only because the player was
    ///   idle with no file loaded;
    /// * `["seek", "83", "absolute"]` and `["seek", 83, "absolute"]` → the same
    ///   `error running command`, so a bare number in text or as JSON is also
    ///   mpv's to interpret;
    /// * `["seek", "not-a-time", "absolute"]` and `["seek", null, "absolute"]`
    ///   → `invalid parameter`, which is mpv **rejecting the argument shape**.
    ///
    /// So `error running command` vs `invalid parameter` is the line between
    /// "mpv understood and had nothing to do it to" and "this is not a seek".
    /// A `f64` here would force this crate to parse the sender's clock string
    /// before mpv gets to, and the two parsers would then disagree in
    /// production while agreeing in every test on this machine — the same
    /// shape AGENTS.md §4.2 keeps meeting ("假命令行工具的输出，必须是真工具在这台机器上的
    /// 逐字输出"). The text goes through; what it means is mpv's call.
    fn set_media_position(&self, target: &str);
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

/// The GENA broadcast queue, seen from the side that fills it.
///
/// Why this is a fourth seam instead of a method on [`StateStore`]: the trait
/// above is explicitly forbidden from enqueueing (`write`'s doc names
/// [`crate::eventing`] as the only place that knows the queue, the condvar and
/// the subscriber table), and that prohibition is not decoration. A store
/// implementation that also held a subscriber list would be two owners of one
/// fact — the same shape as the settings-page-vs-`<LastChange>` disagreement
/// this module's header is written around.
///
/// So the decision stays where Python put it: [`crate::soap`]'s `set_state`
/// helper asks the store "did this change?", and *that call site* — the only one
/// with both the boolean and the observed table in hand — decides whether to
/// call this. `enqueue` is therefore contractually "only for a value that
/// changed", and its argument is the caller's
/// [`is_observed`], not something an implementation may re-check by ignoring the
/// gate. An implementation that enqueued on every write would reintroduce the
/// 92%: one user's log carried 505 duration reports, 464 of them for a number
/// that had not moved (AGENTS.md §4.2, "状态变了才广播").
pub trait Broadcast: Send + Sync {
    /// A variable's value changed; the subscribers of the service that owns
    /// this name should hear about it.
    fn enqueue(&self, name: &str, value: &Value);
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

    /// The fourth seam is reachable through the same `Arc` alias as the other
    /// three, and it hands the queue exactly what the caller passed.
    ///
    /// The *gate* — "only a value that changed, and only an observed name" — is
    /// deliberately not asserted here. It lives at [`crate::soap`]'s `set_state`
    /// call site, so re-implementing the condition in this file would make a test
    /// that passes whatever the shipped code does; that is the shape of check AGENTS.md
    /// §4.13 calls "不提问的检查". What this test can hold honestly is the two
    /// halves that would break silently: `dyn Broadcast` must stay object-safe
    /// behind [`SharedBroadcast`], and an implementation that dropped or rewrote
    /// the value on the way to the queue would make every GENA event report a
    /// number the control point never asked about.
    #[test]
    fn broadcast_carries_the_callers_value_verbatim() {
        crate::pin_test_dir();

        #[derive(Default)]
        struct Recorder {
            seen: std::sync::Mutex<Vec<(String, Value)>>,
        }
        impl Broadcast for Recorder {
            fn enqueue(&self, name: &str, value: &Value) {
                self.seen
                    .lock()
                    .unwrap()
                    .push((name.to_string(), value.clone()));
            }
        }

        let recorder = Arc::new(Recorder::default());
        // The unsized coercion is the point: `src-tauri` hands the queue over as
        // a trait object, exactly as it hands over the store and the renderer.
        let bus: SharedBroadcast = recorder.clone();

        for name in OBSERVED {
            bus.enqueue(name, &Value::String(format!("v-{}", name)));
        }
        // A name this table does not observe is still carried, because the trait
        // is the caller's mouth and not the caller's judge.
        bus.enqueue("RelativeTimePosition", &serde_json::json!(1));

        let seen = recorder.seen.lock().unwrap();
        assert_eq!(seen.len(), OBSERVED.len() + 1);
        for (index, name) in OBSERVED.iter().enumerate() {
            assert_eq!(seen[index].0, *name);
            assert_eq!(seen[index].1, Value::String(format!("v-{}", name)));
        }
        assert_eq!(seen[OBSERVED.len()].0, "RelativeTimePosition");
        assert_eq!(seen[OBSERVED.len()].1, serde_json::json!(1));
        assert!(!is_observed("RelativeTimePosition"));
    }
}

/// Keep the trait objects' `Arc` in the same place so callers cannot hand a
/// `Service` a store that is dropped while a thread still reads it.
pub type SharedStore = Arc<dyn StateStore>;
pub type SharedRenderer = Arc<dyn Renderer>;
pub type SharedHooks = Arc<dyn Hooks>;
pub type SharedBroadcast = Arc<dyn Broadcast>;
