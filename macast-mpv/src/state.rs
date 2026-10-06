//! An mpv reply translated into the state updates a control point is told about.
//!
//! The reference is `MPVRenderer.update_state` (`macast_renderer/mpv.py:273–369`).
//! It is one function, and it is the reason this crate exists: AGENTS.md §4.2
//! records that mpv's replies do not have the shape our code assumed, and every
//! one of those assumptions lives in this branch tree. There is no socket and no
//! player here — a line of JSON goes in, an ordered list of [`Signal`]s comes
//! out, and the two together are what the renderer would have done.
//!
//! Three shapes make the port worth doing carefully rather than approximately:
//!
//! - **One `except` wraps the whole batch.** `start_ipc` parses a chunk into
//!   lines and runs `for msg in msgs: self.update_state(msg)` inside a single
//!   `try`, so the first line that raises stops the second and third from being
//!   applied at all. That is why [`PlayerState::apply`] returns `Err` rather than
//!   skipping a field: a port that kept going would log a decode error at a
//!   different moment than the reference does, and the two logs would stop being
//!   comparable.
//! - **`is not None` and truthiness are different questions**, and the reference
//!   uses both in the same function — see [`crate::pyops::truthy`].
//! - **A missing key and a `null` key are different replies.** mpv sends
//!   `{"error": "property unavailable", "data": null}` for a property it cannot
//!   read, and the three guards here (`'data' in res`, `res['data']`,
//!   `res.get('data', None)`) answer three different questions about it. Two
//!   branches raise `KeyError` when mpv omits the field entirely — reproducing
//!   that is the point, not an oversight.

use serde_json::Value;

use crate::command::observed_name;
use crate::pyops::{as_int, clock, py_repr, py_str, truthy, type_name};

/// What the renderer would have done, in the order it would have done it.
///
/// Ordering is part of the contract: the volume branch logs *before* it updates
/// state, the duration branch publishes and logs *before* `set_state_duration`,
/// and `renderer_av_stop` precedes the transport change in `end-file`. A
/// consumer that flushed these as a set rather than a sequence would produce a
/// log whose lines are the right ones in the wrong order — which is exactly the
/// artifact anyone diffs when a cast goes silent.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// `logger.info(res)` — `str()` of the decoded reply, not the wire text.
    Info(String),
    /// `logger.debug(...)`, either the bare reply or one of the two sentences.
    Debug(String),
    /// `set_state_volume(int(res['data']))`.
    Volume(i64),
    /// `set_state_position(position)` — always sent, even as `'00:00:00'`.
    Position(String),
    /// `set_state_duration(duration)` — always sent, from either arm.
    Duration(String),
    /// `cherrypy.engine.publish('mpv_update_duration', duration)`.
    PublishDuration(String),
    /// `set_state_pause()` / `set_state_play()`.
    Pause,
    Play,
    /// `set_state_mute(res['data'])` — the raw value, unguarded and unconverted.
    Mute(Value),
    /// The two `set_state` calls track-list makes together.
    Tracks { current: i64, number: i64 },
    /// `set_state_speed` / `set_media_subtitle` with the raw value.
    Speed(Value),
    Subtitle(Value),
    /// `cherrypy.engine.publish('renderer_av_stop')`.
    AvStop,
    /// `set_state_stop()`.
    Stop,
    /// `set_state_eof()` — which the protocol turns into `NO_MEDIA_PRESENT`.
    Eof,
    /// `set_state_transport_error()` — `TransportState=STOPPED` plus
    /// `TransportStatus=ERROR_OCCURRED`, always as a pair.
    TransportError,
    /// `publish('app_notify', "File error", res['file_error'])`.
    FileError(Value),
    /// `publish('renderer_av_uri', self.protocol.get_state_url())`.
    AvUri(String),
}

/// The player state `update_state` reads and writes, plus the two protocol
/// answers it asks for.
///
/// `playing` and `pause` are the renderer's own booleans — the only mutable state
/// in the reference function, and the reason it cannot be a pure `&str -> Vec`
/// map. `transport_state` and `current_url` are read from the protocol on two
/// specific branches (`duration` and `start-file`); they are arguments rather
/// than callbacks because the caller is the transport layer, which is exactly
/// where those getters live.
#[derive(Debug, Clone)]
pub struct PlayerState {
    playing: bool,
    pause: bool,
    transport_state: String,
    current_url: String,
}

impl Default for PlayerState {
    fn default() -> Self {
        // `MPVRenderer.__init__`: both booleans start False, and `pause` staying
        // False is what makes the first `playback-restart` report PLAYING.
        Self {
            playing: false,
            pause: false,
            transport_state: "STOPPED".to_string(),
            current_url: String::new(),
        }
    }
}

impl PlayerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tell the player what the protocol currently says, before feeding it a line.
    pub fn with_protocol(mut self, transport_state: &str, current_url: &str) -> Self {
        self.transport_state = transport_state.to_string();
        self.current_url = current_url.to_string();
        self
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn pause(&self) -> bool {
        self.pause
    }

    /// One line of mpv's output, applied through `update_state`.
    ///
    /// `Err(str(e))` is the exception Python would have raised, in its own words:
    /// a `JSONDecodeError` message, `'data'` for the two branches that index the
    /// missing key, `invalid literal for int() with base 10: '45.5'`, and the
    /// membership/index/comparison sentences for the replies that are not objects
    /// at all. The caller owns the batch — see [`crate::ipc::Feed::push`].
    pub fn apply(&mut self, line: &str) -> Result<Vec<Signal>, String> {
        let res = decode(line)?;
        let mut out = Vec::new();
        if has_key(&res, "id")? {
            self.property_update(&res, &mut out)?;
        } else if has_key(&res, "event")? {
            self.event(&res, &mut out)?;
        } else {
            out.push(Signal::Debug(py_str(&res)));
        }
        Ok(out)
    }

    /// The `if 'id' in res:` chain. An id we never observed falls through the
    /// whole chain and produces nothing — not even a debug line, because the
    /// reference's chain has no `else` inside that branch.
    fn property_update(
        &mut self,
        res: &Value,
        out: &mut Vec<Signal>,
    ) -> Result<(), String> {
        let id = index(&res, "id")?;
        // Python compares with `==`, so `true` and `1.0` are both `1` here, while
        // a string id is equal to nothing and reaches `None` below. The tree is
        // keyed by the *name* rather than the number so that it reads in the order
        // the reference wrote it.
        match observed_name(python_number_eq(id).unwrap_or(u64::MAX)) {
            Some("volume") => self.on_volume(res, out),
            Some("time-pos") => self.on_time_pos(res, out),
            Some("pause") => self.on_pause(res, out),
            Some("mute") => self.on_mute(res, out),
            Some("duration") => self.on_duration(res, out),
            Some("track-list") => self.on_track_list(res, out),
            Some("speed") => self.on_data_passthrough(res, out, true),
            Some("sub-visibility") => self.on_data_passthrough(res, out, false),
            // An id outside the eight we registered is not an unknown *property*;
            // it is a reply to something else, and the reference stays silent.
            _ => Ok(()),
        }
    }

    /// `id 1`. The log line comes first and is unconditional — it prints even a
    /// reply whose `data` is `null`, which is the only way anyone found out that
    /// mpv reports an unavailable property at all.
    fn on_volume(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        out.push(Signal::Info(py_str(res)));
        if present(res, "data") && !is_null(res, "data") {
            let value = as_int(data_of(res)).ok_or_else(|| int_error(data_of(res)))?;
            out.push(Signal::Volume(value));
        }
        Ok(())
    }

    /// `id 2`. No logging at all — this is the once-a-second line, and the
    /// reference deliberately keeps it out of the log (AGENTS.md §4.2: 92% of the
    /// duration lines in a three-hour log were restating a value nobody asked
    /// about; position is the same firehose).
    fn on_time_pos(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        let position = if present(res, "data") && !is_null(res, "data") {
            clock(as_int(data_of(res)).ok_or_else(|| int_error(data_of(res)))?)
        } else {
            "00:00:00".to_string()
        };
        // Always sent, even when nothing changed.
        out.push(Signal::Position(position));
        Ok(())
    }

    /// `id 3`. The only branch that can `return` early, and the only place
    /// `self.playing` gates a state update.
    fn on_pause(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        out.push(Signal::Info(py_str(res)));
        if !self.playing {
            // `if self.playing is False: return` — the log line above still
            // happened. A player that has not announced `start-file` cannot be
            // paused *into*, so this reply is dropped after being witnessed.
            return Ok(());
        }
        // Direct index: a reply without `data` raises here, `'data'`.
        let data = index(res, "data")?;
        if truthy(data) && !data.is_null() {
            self.pause = true;
            out.push(Signal::Pause);
        } else {
            self.pause = false;
            out.push(Signal::Play);
        }
        Ok(())
    }

    /// `id 4`. Unguarded in both directions: no log, no `None` check, and the raw
    /// value goes to the protocol, which is where `volume.muted` gets its truth.
    fn on_mute(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        let data = index(res, "data")?;
        out.push(Signal::Mute(data.clone()));
        Ok(())
    }

    /// `id 5`. The branch with the most order in it: publish, debug, the
    /// transport-state question, and only then the state update — and the state
    /// update happens on *both* sides of the `if`.
    fn on_duration(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        if present(res, "data") && !is_null(res, "data") {
            let duration =
                clock(as_int(data_of(res)).ok_or_else(|| int_error(data_of(res)))?);
            out.push(Signal::PublishDuration(duration.clone()));
            out.push(Signal::Debug(format!("update duration {duration}")));
            if self.transport_state == "PLAYING" {
                out.push(Signal::Debug("Living media".to_string()));
            }
            out.push(Signal::Duration(duration));
        } else {
            out.push(Signal::Duration("00:00:00".to_string()));
        }
        Ok(())
    }

    /// `id 6`. `CurrentTrack: 0` is **unreachable in the reference**: the guard is
    /// `if res['data'] and res['data'] is not None`, and an empty track list is
    /// falsy, so the `0 if tracks == 0 else 1` expression can only ever answer
    /// `1`. Reproduced rather than tidied up, because a control point that
    /// currently sees "one track" while nothing plays would notice the fix.
    fn on_track_list(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        let data = index(res, "data")?;
        if truthy(data) && !data.is_null() {
            let tracks = length_of(data)?;
            out.push(Signal::Tracks {
                current: if tracks == 0 { 0 } else { 1 },
                number: tracks,
            });
        }
        Ok(())
    }

    /// `id 7` and `id 8` — `res.get('data', None)`, skip only on a real `None`,
    /// and the value is passed through untouched (a speed of `"1.5"` stays a
    /// string; the protocol side does its own coercion).
    fn on_data_passthrough(
        &self,
        res: &Value,
        out: &mut Vec<Signal>,
        is_speed: bool,
    ) -> Result<(), String> {
        if let Some(data) = get_key(res, "data") {
            if !data.is_null() {
                out.push(if is_speed {
                    Signal::Speed(data.clone())
                } else {
                    Signal::Subtitle(data.clone())
                });
            }
        }
        Ok(())
    }

    /// The `elif 'event' in res:` chain. Every event is logged first, whatever
    /// follows.
    fn event(&mut self, res: &Value, out: &mut Vec<Signal>) -> Result<(), String> {
        out.push(Signal::Info(py_str(res)));
        let event = index(res, "event")?;
        match event.as_str() {
            Some("end-file") => {
                out.push(Signal::AvStop);
                self.playing = false;
                // A reply with no `reason` is `stop`, not an error: the reference
                // falls through the missing-key case into the same call the
                // unrecognised-reason case makes.
                match get_key(res, "reason") {
                    Some(value) if *value == Value::String("error".to_string()) => {
                        out.push(Signal::TransportError)
                    }
                    Some(value) if *value == Value::String("eof".to_string()) => {
                        out.push(Signal::Eof)
                    }
                    _ => out.push(Signal::Stop),
                }
                if truthy(get_key(res, "file_error").unwrap_or(&Value::Bool(false))) {
                    out.push(Signal::FileError(
                        get_key(res, "file_error").unwrap().clone(),
                    ));
                }
            }
            Some("start-file") => {
                self.playing = true;
                out.push(Signal::AvUri(self.current_url.clone()));
            }
            Some("seek") => {}
            Some("idle") => {
                self.playing = false;
                out.push(Signal::Stop);
            }
            Some("playback-restart") => {
                out.push(if self.pause { Signal::Pause } else { Signal::Play })
            }
            // An event we have never seen is logged and otherwise ignored — the
            // chain has no `else`, so it is not even a debug line beyond the one
            // every event gets above.
            _ => {}
        }
        Ok(())
    }
}

/// `json.loads(line)`, and nothing else — the framing is [`crate::ipc`]'s job.
///
/// The gate is [`crate::pyjson`], CPython's own scanner replayed, rather than
/// `serde_json::from_str` with the sentence patched afterwards. Two reasons, and
/// the reference is what makes them load-bearing:
///
/// - **Validity decides whether the thread lives.** `MPVRenderer` wraps its whole
///   read loop in one `except Exception`, so a document Python would have parsed
///   but a stricter decoder refuses turns into a decode error — and in this port a
///   decode error ends the batch. `serde_json` rejects `NaN`, `Infinity`,
///   `-Infinity` and `[1,]`/`{"a":1,}` outright; CPython accepts the first three
///   and refuses the trailing comma for its own reasons. Whichever one is the gate
///   answers a different question than "what does the log say".
/// - **The sentence is part of the product.** A truncated socket is a real user
///   state, and what they read in `macast.log` is CPython's wording at CPython's
///   character offset — `Expecting value: line 1 column 1 (char 0)`, not
///   `expected value at line 1 column 1`. Reconstructing one from the other's
///   position is guesswork: the two disagree about *where* they stopped as well
///   (`serde_json` counts bytes).
fn decode(line: &str) -> Result<Value, String> {
    crate::pyjson::loads(line)
}

// --- Python's containers, one helper per operator ------------------------------

/// `key in res` for a decoded JSON value.
///
/// Three answers and a raise: a dict reports membership, a list and a str report
/// their own kind of membership (`'id' in ['id']` is `True`, and `'id' in "a id b"`
/// is `True` by substring), and `int`, `float`, `bool` and `None` raise
/// `argument of type '…' is not iterable`. The list and str cases matter because
/// mpv's socket is a *byte stream*: a chunk that ends mid-line gives the next
/// line's parser a document that is a plain number or a string, and the reference
/// reaches `'id' in res` before it ever looks at the shape.
fn has_key(res: &Value, key: &str) -> Result<bool, String> {
    match res {
        Value::Object(map) => Ok(map.contains_key(key)),
        Value::Array(items) => Ok(items
            .iter()
            .any(|item| item.as_str() == Some(key))),
        Value::String(text) => Ok(text.contains(key)),
        other => Err(format!("argument of type '{}' is not iterable", type_name(other))),
    }
}

/// `res[key]`.
fn index<'a>(res: &'a Value, key: &str) -> Result<&'a Value, String> {
    match res {
        Value::Object(map) => map
            .get(key)
            .ok_or_else(|| format!("{}", py_repr(&Value::String(key.to_string())))),
        Value::Array(_) => Err("list indices must be integers or slices, not str".to_string()),
        Value::String(_) => Err("string indices must be integers, not 'str'".to_string()),
        other => Err(not_subscriptable(other)),
    }
}

/// `key in res` *without* raising — for the callers that only reach a value after
/// a guard has already established the shape.
fn get_key<'a>(res: &'a Value, key: &str) -> Option<&'a Value> {
    res.as_object().and_then(|map| map.get(key))
}

/// `'data' in res` — presence, and it raises exactly where [`has_key`] does. The
/// volume, position and duration branches ask this question before the value
/// question, so `{"id": 5}` and `{"id": 5, "data": null}` take the same arm while
/// `{"id": 4}` raises.
fn present(res: &Value, key: &str) -> bool {
    matches!(res, Value::Object(map) if map.contains_key(key))
}

fn is_null(res: &Value, key: &str) -> bool {
    get_key(res, key).map(Value::is_null).unwrap_or(false)
}

/// The value the guarded branches pass to `int()`. Safe only behind `present`.
fn data_of(res: &Value) -> &Value {
    get_key(res, "data").unwrap_or(&Value::Null)
}

/// `len(value)`, for the shapes `track-list` can hold.
fn length_of(value: &Value) -> Result<i64, String> {
    match value {
        Value::Array(items) => Ok(items.len() as i64),
        Value::Object(map) => Ok(map.len() as i64),
        Value::String(text) => Ok(text.chars().count() as i64),
        other => Err(format!("object of type '{}' has no len()", type_name(other))),
    }
}

/// The message `int(value)` writes when [`as_int`] refuses.
fn int_error(value: &Value) -> String {
    match value {
        Value::String(text) => format!(
            "invalid literal for int() with base 10: {}",
            py_repr(&Value::String(text.clone()))
        ),
        other => format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ),
    }
}

/// `value == <int>`, with Python's cross-type equality.
///
/// `True == 1` and `1.0 == 1` are both true in Python, so a reply of
/// `{"id": true}` *is* the volume branch. A string is never equal to a number —
/// `{"id": "5"}` matches nothing and falls through silently, which is what makes
/// this match arm worth having a comment instead of an `as_u64`.
fn python_number_eq(value: &Value) -> Option<u64> {
    match value {
        Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                return Some(u);
            }
            if let Some(i) = n.as_i64() {
                return u64::try_from(i).ok();
            }
            let f = n.as_f64()?;
            if f.fract() == 0.0 && f >= 0.0 {
                Some(f as u64)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `str(e)` for `res['id']` / `res['data']` when `res` is a number, `true` or
/// `null`. Measured in `.venv` (3.12.15): `5` ⇒ 「'int' object is not
/// subscriptable」, `None` ⇒ 「'NoneType' …」, `True` ⇒ 「'bool' …」,
/// `3.5` ⇒ 「'float' …」.
///
/// [`index`] is never reached with one of those through [`PlayerState::apply`]:
/// [`has_key`] runs first and raises the membership sentence
/// ("argument of type 'int' is not iterable") for exactly the same values. The
/// two messages are different, so the arm is kept with the sentence Python
/// actually writes, and the direct test in the test module is what makes that
/// sentence a measured fact rather than a guess.
fn not_subscriptable(value: &Value) -> String {
    format!("'{}' object is not subscriptable", type_name(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn applied(line: &str) -> Vec<Signal> {
        PlayerState::new().apply(line).expect("must parse")
    }

    #[test]
    fn an_unrecognised_id_is_silent_not_debugged() {
        // The reference's `if 'id' in res:` chain has no `else`, so this is the
        // one branch shape that produces neither a state update nor a log line.
        assert_eq!(applied(r#"{"id": 99}"#), Vec::<Signal>::new());
        assert_eq!(applied(r#"{"id": "5"}"#), Vec::<Signal>::new());
    }

    #[test]
    fn a_bool_or_whole_float_id_is_the_number_python_compares_it_to() {
        // The reference is a chain of `elif res['id'] == 1:`, and `==` in Python
        // answers True for `True` and for `1.0`. Measured in `.venv` 3.12.15:
        // both `{'id': True, 'data': 42}` and `{'id': 1.0, 'data': 42}` take the
        // volume branch, and the log line is Python's own `str(res)` — `True`, not
        // `true`. A half or a negative id matches nothing.
        for (doc, id_repr) in [
            (r#"{"id": true, "data": 42}"#, "{'id': True, 'data': 42}"),
            (r#"{"id": 1.0, "data": 42}"#, "{'id': 1.0, 'data': 42}"),
        ] {
            assert_eq!(
                applied(doc),
                vec![Signal::Info(id_repr.to_string()), Signal::Volume(42)],
                "{doc}"
            );
        }
        assert_eq!(applied(r#"{"id": 1.5}"#), Vec::<Signal>::new());
        assert_eq!(applied(r#"{"id": -1}"#), Vec::<Signal>::new());
    }

    #[test]
    fn a_reply_with_neither_id_nor_event_is_debugged_as_python_str() {
        assert_eq!(
            applied(r#"{"foo": 1, "bar": "x"}"#),
            vec![Signal::Debug("{'foo': 1, 'bar': 'x'}".to_string())]
        );
    }

    #[test]
    fn volume_logs_before_it_updates_and_skips_a_null_data() {
        let out = applied(r#"{"id": 1, "data": 42}"#);
        assert_eq!(
            out,
            vec![
                Signal::Info("{'id': 1, 'data': 42}".to_string()),
                Signal::Volume(42)
            ]
        );
        // `data: null` is logged and then dropped by the `is not None` guard.
        assert_eq!(
            applied(r#"{"id": 1, "data": null}"#),
            vec![Signal::Info("{'id': 1, 'data': None}".to_string())]
        );
        // And a reply with no `data` at all is *also* only a log line — this
        // branch asks presence first.
        assert_eq!(
            applied(r#"{"id": 1}"#),
            vec![Signal::Info("{'id': 1}".to_string())]
        );
    }

    #[test]
    fn position_is_truncated_not_rounded_and_always_sent() {
        assert_eq!(applied(r#"{"id": 2, "data": 12.9}"#), vec![Signal::Position("0:00:12".into())]);
        assert_eq!(applied(r#"{"id": 2, "data": 3725}"#), vec![Signal::Position("1:02:05".into())]);
        assert_eq!(applied(r#"{"id": 2}"#), vec![Signal::Position("00:00:00".into())]);
        assert_eq!(applied(r#"{"id": 2, "data": null}"#), vec![Signal::Position("00:00:00".into())]);
        // The floor shape, and the reason `pyops::clock` is not
        // `macast_core::timefmt::dlna_clock`.
        assert_eq!(applied(r#"{"id": 2, "data": -5}"#), vec![Signal::Position("-1:59:55".into())]);
    }

    #[test]
    fn a_decimal_string_raises_the_way_python_does_and_kills_the_batch() {
        let err = PlayerState::new().apply(r#"{"id": 2, "data": "45.5"}"#).unwrap_err();
        assert_eq!(err, "invalid literal for int() with base 10: '45.5'");
        // `null` is guarded out; a list is not, and `int([])` is a TypeError.
        assert_eq!(
            PlayerState::new().apply(r#"{"id": 2, "data": []}"#).unwrap_err(),
            "int() argument must be a string, a bytes-like object or a real number, not 'list'"
        );
    }

    #[test]
    fn pause_is_dropped_while_the_player_has_not_started_and_logs_anyway() {
        // `playing` is False in a fresh state, so the log line happens and the
        // state update does not.
        assert_eq!(
            applied(r#"{"id": 3, "data": true}"#),
            vec![Signal::Info("{'id': 3, 'data': True}".to_string())]
        );
        let mut state = PlayerState::new();
        state.apply(r#"{"event": "start-file"}"#).unwrap();
        assert_eq!(
            state.apply(r#"{"id": 3, "data": true}"#).unwrap(),
            vec![
                Signal::Info("{'id': 3, 'data': True}".to_string()),
                Signal::Pause
            ]
        );
        assert!(state.pause());
        // Truthiness, not presence: `0`, `""` and `false` all mean "playing".
        for falsy in [json!(false), json!(0), json!("")] {
            let mut s = PlayerState::new();
            s.apply(r#"{"event": "start-file"}"#).unwrap();
            assert_eq!(
                s.apply(&format!(r#"{{"id": 3, "data": {falsy}}}"#)).unwrap().last(),
                Some(&Signal::Play),
                "{falsy} must be falsy for pause"
            );
            assert!(!s.pause());
        }
    }

    #[test]
    fn pause_and_mute_raise_when_data_is_absent_entirely() {
        // Pause only reaches `res['data']` once `playing` is True; a fresh state
        // returns at the guard and the missing key never burns (that shape is the
        // test above). Mute has no guard at all, so it raises in either state.
        let mut state = PlayerState::new();
        state.apply(r#"{"event": "start-file"}"#).unwrap();
        assert_eq!(state.apply(r#"{"id": 3}"#).unwrap_err(), "'data'");
        assert_eq!(PlayerState::new().apply(r#"{"id": 4}"#).unwrap_err(), "'data'");
        // and `null` passes for mute, because that branch has no guard at all.
        assert_eq!(
            applied(r#"{"id": 4, "data": null}"#),
            vec![Signal::Mute(Value::Null)]
        );
    }

    #[test]
    fn duration_publishes_and_logs_only_for_a_real_value_but_always_updates() {
        // The `null` arm: one signal, in the order Python emits them.
        assert_eq!(applied(r#"{"id": 5}"#), vec![Signal::Duration("00:00:00".into())]);
        // The real-value arm with the transport not playing: publish + debug + set.
        let out = applied(r#"{"id": 5, "data": 120}"#);
        assert_eq!(
            out,
            vec![
                Signal::PublishDuration("0:02:00".into()),
                Signal::Debug("update duration 0:02:00".into()),
                Signal::Duration("0:02:00".into()),
            ]
        );
        // "Living media" only joins when the protocol says PLAYING — and it joins
        // *before* the state update, because that is where the line sits.
        let out = PlayerState::new()
            .with_protocol("PLAYING", "http://a/b")
            .apply(r#"{"id": 5, "data": 120}"#)
            .unwrap();
        assert_eq!(
            out,
            vec![
                Signal::PublishDuration("0:02:00".into()),
                Signal::Debug("update duration 0:02:00".into()),
                Signal::Debug("Living media".into()),
                Signal::Duration("0:02:00".into()),
            ]
        );
    }

    #[test]
    fn an_empty_track_list_reports_nothing_and_current_track_zero_is_unreachable() {
        // `if res['data']` is falsy for `[]`, so the reference never sends
        // `CurrentTrack: 0` — the shape of the guard, not of the expression.
        assert_eq!(applied(r#"{"id": 6, "data": []}"#), Vec::<Signal>::new());
        assert_eq!(
            applied(r#"{"id": 6, "data": [{"type": "video"}]}"#),
            vec![Signal::Tracks { current: 1, number: 1 }]
        );
        // A non-list that is truthy still reaches `len()`, and a number raises.
        assert_eq!(
            PlayerState::new().apply(r#"{"id": 6, "data": 3}"#).unwrap_err(),
            "object of type 'int' has no len()"
        );
        assert_eq!(
            applied(r#"{"id": 6, "data": "abc"}"#),
            vec![Signal::Tracks { current: 1, number: 3 }]
        );
    }

    #[test]
    fn speed_and_subtitle_skip_only_a_real_none_and_pass_the_value_through() {
        assert_eq!(applied(r#"{"id": 7, "data": "1.5"}"#), vec![Signal::Speed(json!("1.5"))]);
        assert_eq!(applied(r#"{"id": 7, "data": 1.5}"#), vec![Signal::Speed(json!(1.5))]);
        assert_eq!(applied(r#"{"id": 7}"#), Vec::<Signal>::new());
        assert_eq!(applied(r#"{"id": 7, "data": null}"#), Vec::<Signal>::new());
        assert_eq!(
            applied(r#"{"id": 8, "data": false}"#),
            vec![Signal::Subtitle(json!(false))]
        );
    }

    #[test]
    fn end_file_orders_stop_before_the_transport_and_the_notify_last() {
        let out = applied(r#"{"event": "end-file"}"#);
        assert_eq!(out, vec![
            Signal::Info("{'event': 'end-file'}".to_string()),
            Signal::AvStop,
            Signal::Stop,
        ]);
        assert_eq!(
            applied(r#"{"event": "end-file", "reason": "error"}"#)[2],
            Signal::TransportError
        );
        assert_eq!(
            applied(r#"{"event": "end-file", "reason": "eof"}"#)[2],
            Signal::Eof
        );
        // An unknown reason is `stop`, not an error.
        assert_eq!(
            applied(r#"{"event": "end-file", "reason": "quit"}"#)[2],
            Signal::Stop
        );
        // `file_error` is a truthiness test with a default of False.
        let out = applied(r#"{"event": "end-file", "file_error": "no audio or video data played"}"#);
        assert_eq!(
            out.last(),
            Some(&Signal::FileError(json!("no audio or video data played")))
        );
        assert!(!applied(r#"{"event": "end-file", "file_error": ""}"#)
            .iter()
            .any(|s| matches!(s, Signal::FileError(_))));
    }

    #[test]
    fn starting_a_file_makes_pause_mean_something_and_carries_the_protocols_url() {
        let mut state = PlayerState::new().with_protocol("STOPPED", "http://a/b.mp4");
        assert_eq!(
            state.apply(r#"{"event": "start-file"}"#).unwrap(),
            vec![
                Signal::Info("{'event': 'start-file'}".to_string()),
                Signal::AvUri("http://a/b.mp4".to_string()),
            ]
        );
        assert!(state.playing());
        // `playback-restart` answers with whatever `pause` currently says.
        assert_eq!(
            state.apply(r#"{"event": "playback-restart"}"#).unwrap().last(),
            Some(&Signal::Play)
        );
        state.apply(r#"{"event": "idle"}"#).unwrap();
        assert!(!state.playing());
        // `seek` is logged and does nothing else.
        assert_eq!(
            applied(r#"{"event": "seek"}"#),
            vec![Signal::Info("{'event': 'seek'}".to_string())]
        );
    }

    #[test]
    fn a_line_that_is_not_json_reports_cpythons_own_sentence() {
        // Each of these is `str(json.JSONDecodeError(...))` measured on CPython
        // 3.12; the wording is what a reader of the old log recognises.
        let cases = [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            (" ", "Expecting value: line 1 column 2 (char 1)"),
            ("n", "Expecting value: line 1 column 1 (char 0)"),
            ("{", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("{\"a\"", "Expecting ':' delimiter: line 1 column 5 (char 4)"),
            ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
            ("[1,2", "Expecting ',' delimiter: line 1 column 5 (char 4)"),
            ("{\"a\": 1}x", "Extra data: line 1 column 9 (char 8)"),
            ("{\"a\": \"x", "Unterminated string starting at: line 1 column 7 (char 6)"),
        ];
        for (doc, expected) in cases {
            let err = PlayerState::new().apply(doc).unwrap_err();
            assert_eq!(err, expected, "for {doc:?}");
        }
        // Positions count characters, not bytes.
        assert_eq!(
            PlayerState::new().apply("{\"éé\": }").unwrap_err(),
            "Expecting value: line 1 column 8 (char 7)"
        );
        // And a document that wraps reports the line it wrapped on.
        assert_eq!(
            PlayerState::new().apply("{\"a\": 1}\n{\"b\": 2}").unwrap_err(),
            "Extra data: line 2 column 1 (char 9)"
        );
    }

    #[test]
    fn a_reply_that_is_not_an_object_reaches_the_membership_operator() {
        // mpv's socket is a byte stream; a chunk that splits mid-line leaves the
        // next parse looking at a bare number or string.
        assert_eq!(
            PlayerState::new().apply("5").unwrap_err(),
            "argument of type 'int' is not iterable"
        );
        assert_eq!(
            PlayerState::new().apply("null").unwrap_err(),
            "argument of type 'NoneType' is not iterable"
        );
        assert_eq!(
            PlayerState::new().apply("true").unwrap_err(),
            "argument of type 'bool' is not iterable"
        );
        // A list *can* answer the membership question, and then raises on the
        // subscript instead — a different sentence, and both are the reference's.
        assert_eq!(
            PlayerState::new().apply("[\"id\", 1]").unwrap_err(),
            "list indices must be integers or slices, not str"
        );
        // And a list that answers "no" to both membership questions falls through
        // to the `else` arm, which logs `str(res)` — Python's list repr, with the
        // quotes on the element and no quotes around the whole thing.
        assert_eq!(
            applied("[\"other\", 1]"),
            vec![Signal::Debug("['other', 1]".to_string())]
        );
        // A string answers by substring, then raises its own index error.
        assert_eq!(
            PlayerState::new().apply("\"a id b\"").unwrap_err(),
            "string indices must be integers, not 'str'"
        );
        assert_eq!(
            applied("\"nothing here\""),
            vec![Signal::Debug("nothing here".to_string())]
        );
    }

    #[test]
    fn the_subscript_arm_apply_cannot_reach_still_quotes_python() {
        // Every one of these values is stopped by `has_key` one call earlier (the
        // test above), so calling `index` directly is the only way this TypeError
        // is ever asked. Measured in `.venv` 3.12.15: `5['id']`, `None['id']`,
        // `True['id']`, `3.5['id']`.
        for (value, want) in [
            (json!(5), "'int' object is not subscriptable"),
            (Value::Null, "'NoneType' object is not subscriptable"),
            (json!(true), "'bool' object is not subscriptable"),
            (json!(3.5), "'float' object is not subscriptable"),
        ] {
            assert_eq!(index(&value, "id").unwrap_err(), want, "{value}");
        }
    }

    #[test]
    fn the_batch_stops_at_the_first_line_that_raises() {
        // This is the whole reason `apply` returns `Result`: `start_ipc` wraps
        // every line of a chunk in one `try`, so a bad second line swallows the
        // third, and a port that kept going would not be comparable to the
        // reference log.
        let mut state = PlayerState::new();
        let first = state.apply("{\"id\": 2, \"data\": 10}").unwrap();
        assert_eq!(first, vec![Signal::Position("0:00:10".into())]);
        assert!(state.apply("{\"id\": 2, \"data\": \"45.5\"}").is_err());
        // The third line was never applied, and the state it would have written
        // did not move: `playing` is still False from here.
        state.apply("{\"id\": 2, \"data\": 99}").unwrap();
        assert!(!state.playing());
    }

    #[test]
    fn feed_drives_apply_and_reports_the_line_it_died_on() {
        // The seam this crate's `ipc` module promises: `apply` is the function the
        // framing hands each line, and its error text is what `Failure::message`
        // carries.
        let mut state = PlayerState::new();
        let mut feed = crate::ipc::Feed::new();
        let received = feed.push(
            b"{\"id\": 4, \"data\": true}\n{\"id\": 4}\n{\"id\": 4, \"data\": false}\n",
            |line| state.apply(line).map(|_| ()),
        );
        let crate::ipc::Received::Failed(failure) = received else {
            panic!("the second line has no `data`: {received:?}");
        };
        assert_eq!(failure.message, "'data'");
        assert_eq!(failure.applied_before, 1);
        assert!(!failure.kills_the_reader());
        assert_eq!(failure.log_lines().len(), 3);
    }
}
