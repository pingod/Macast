//! The other half of the wire: this shell is the application that
//! `macast-mpv::player::Host` was cut to describe.
//!
//! `macast-mpv/src/lib.rs` names this file's job as the reason the seam exists:
//! the event bus and the settings live in `src-tauri`, and until now the only
//! implementations of that trait in the tree were the crate's own test doubles.
//! So everything here is a *reproduction*, and the reference for each line is
//! named in the comment above it — `macast/protocol.py` for the DLNA state table
//! and the push sequence, `macast_renderer/mpv.py` for what the renderer does to
//! that state, `macast/media_resolve.py` for the window title.
//!
//! Two honest deviations, each marked again at its own code:
//!
//! - **an unknown state name is logged, not raised.** Python's `set_state`
//!   deliberately leaves its `KeyError` in place (protocol.py:1025-1036 —
//!   "silently ignoring it would let a typo in a renderer look like 'the client
//!   just never heard about it'"). Here it is an ERROR log naming the key,
//!   because a panic inside a Tauri command is a crashed window instead of a red
//!   line in a log the user can open. The mitigation is structural rather than
//!   behavioural: [`StateTable`] is seeded with every name the three SCPDs
//!   declare, so a name this shell writes is never unknown.
//! - **stopping the service stops the receiver, not the player.** `stop_service`
//!   is the reference's "the app asked to stop answering DLNA"; here that means
//!   the HTTP face, the SSDP announcer (with its `byebye`) and the GENA event
//!   thread, reached through `crate::Runtime`. The mpv child keeps playing what it
//!   was handed, because that is what "the service stopped" reads like to a user:
//!   the phone no longer finds Macast, the window it opened stays open. Before the
//!   services existed this method only set a flag; the flag is still set, because
//!   `player_snapshot` reports it and a caller that cannot reach the app handle
//!   (a test, a thread started before `.setup()` finished) still gets a durable
//!   record that the request happened.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use macast_cast::push;
use macast_cast::relay;
use macast_cast::resolve;
use macast_cast::targets;
use macast_core::logging::{Level, Logger};
use macast_core::net;
use macast_core::paths;
use macast_core::setting::{Key, Setting};
use macast_core::token;
use macast_mpv::argv;
use macast_mpv::player::{Host, Player, StateCall};
use macast_upnp::http::Answer;
use macast_upnp::store::SharedBroadcast;

/// `DLNAProtocol.SERVICE_STATE_OBSERVED` for the two services this shell can
/// reach (`macast/protocol.py:33-44`). The connection manager's three are listed
/// for completeness even though nothing here writes them.
const OBSERVED: [&str; 8] = [
    "TransportState",
    "TransportStatus",
    "CurrentMediaDuration",
    "CurrentTrackDuration",
    "CurrentTrack",
    "NumberOfTracks",
    "Volume",
    "Mute",
];

/// `XMLPath.PROTOCOL_INFO` — the boot value of `SinkProtocolInfo`, read with
/// `load_xml(...).strip()` exactly once, at construction.
const PROTOCOL_INFO_RESOURCE: &str = "xml/SinkProtocolInfo.csv";

/// The five variables whose `allowedValueList` contains `NOT_IMPLEMENTED`.
/// `StateVariable.set_allowed_value_list` boots them to that string
/// (`macast/protocol.py:529-533`), which is why they are not empty strings.
const NOT_IMPLEMENTED: [&str; 5] = [
    "RecordStorageMedium",
    "CurrentRecordQualityMode",
    "PossibleRecordQualityModes",
    "RecordMediumWriteStatus",
    "PossibleRecordStorageMedia",
];

/// Every `stateVariable` name declared by the three SCPDs, with the datatype the
/// XML gives it. Read off `macast/xml/AVTransport.xml` (32),
/// `RenderingControl.xml` (8) and `ConnectionManager.xml` (10) — 50 declarations,
/// **48 unique names**: `LastChange` and `A_ARG_TYPE_InstanceID` are each declared
/// twice, and because `DLNAProtocol.__init__` keeps one merged
/// `self.state_list = {}` (`protocol.py:589`, written at `:708`) the second
/// declaration simply overwrites the first. Both duplicates agree on value and
/// type, so the collapse is harmless here; it is recorded because a Rust map that
/// *refused* a duplicate would be a divergence from the reference, not a fix.
///
/// The **row order is `Description.xml`'s own `<serviceList>` order** — AVTransport,
/// then RenderingControl, then ConnectionManager — because that is the order the
/// reference loader walks (`init_services` iterates the description, not the
/// filenames) and therefore the order the state page shows. `macast-upnp`'s
/// `scpd.rs` compares this table against the table it builds **as an ordered
/// list**, so grouping these rows by anything other than the description's order
/// — alphabetically, or by the order someone opened the files in — is a real drift
/// and turns that test red.
///
/// `s` is the datatype's first letter: `string` boots to `''`, everything else
/// (`boolean` / `i2` / `ui2` / `i4` / `ui4`) boots to the number `0`
/// (`StateVariable.__init__`, `protocol.py:513-525`).
const DECLARED: [(&str, char); 48] = [
    ("CurrentPlayMode", 's'),
    ("RecordStorageMedium", 's'),
    ("LastChange", 's'),
    ("RelativeTimePosition", 's'),
    ("CurrentTrackTitle", 's'),
    ("DisplayCurrentSubtitle", 'b'),
    ("CurrentTrackURI", 's'),
    ("CurrentTrackDuration", 's'),
    ("CurrentRecordQualityMode", 's'),
    ("CurrentMediaDuration", 's'),
    ("AbsoluteCounterPosition", 'i'),
    ("RelativeCounterPosition", 'i'),
    ("A_ARG_TYPE_InstanceID", 'u'),
    ("AVTransportURI", 's'),
    ("TransportState", 's'),
    ("CurrentTrackMetaData", 's'),
    ("NextAVTransportURI", 's'),
    ("PossibleRecordQualityModes", 's'),
    ("CurrentTrack", 'u'),
    ("AbsoluteTimePosition", 's'),
    ("NextAVTransportURIMetaData", 's'),
    ("PlaybackStorageMedium", 's'),
    ("CurrentTransportActions", 's'),
    ("RecordMediumWriteStatus", 's'),
    ("PossiblePlaybackStorageMedia", 's'),
    ("AVTransportURIMetaData", 's'),
    ("NumberOfTracks", 'u'),
    ("A_ARG_TYPE_SeekMode", 's'),
    ("A_ARG_TYPE_SeekTarget", 's'),
    ("PossibleRecordStorageMedia", 's'),
    ("TransportStatus", 's'),
    ("TransportPlaySpeed", 's'),
    ("A_ARG_TYPE_Channel", 's'),
    ("Volume", 'u'),
    ("Mute", 'b'),
    ("PresetNameList", 's'),
    ("A_ARG_TYPE_PresetName", 's'),
    ("VolumeDB", 'i'),
    ("A_ARG_TYPE_ProtocolInfo", 's'),
    ("A_ARG_TYPE_ConnectionStatus", 's'),
    ("A_ARG_TYPE_AVTransportID", 'i'),
    ("A_ARG_TYPE_RcsID", 'i'),
    ("A_ARG_TYPE_ConnectionID", 'i'),
    ("A_ARG_TYPE_ConnectionManager", 's'),
    ("SourceProtocolInfo", 's'),
    ("SinkProtocolInfo", 's'),
    ("A_ARG_TYPE_Direction", 's'),
    ("CurrentConnectionIDs", 's'),
];

/// Where this table's sentences go: `protocol.py:30` is
/// `logging.getLogger("Protocol")`, and `logsplit.claim_from_module` gives every
/// module-level logger in `macast/*.py` its own `logs/<Name>.log`. So the state
/// table's `setState: …` lines and the whole play-history story belong to
/// `logs/Protocol.log`, not to `macast.log` and not to the renderer's file.
pub const PROTOCOL_LOG: &str = "Protocol";

/// The state table one DLNA receiver owns — `DLNAProtocol.state_list`, minus the
/// XML machinery: values are what a client would read back, and the playlist is
/// the list `cast_uri` appends to.
///
/// `log` is **this table's own destination**, not the logger the app happens to
/// carry. Until 2026-10-18 `main.rs` handed it the `MPV` logger (the player's), so
/// a reader chasing "why did the 播放历史 card empty itself" was sent to the file
/// full of mpv events — the lines were there, in the wrong place, which is the
/// §4.3 shape of a log that points at the wrong suspect. No unit test can pin
/// `main.rs`'s choice — it is the caller that names the logger — so the witness is
/// `verify_rust_receiver.sh` §27, which greps the read-back sentence **in
/// `logs/Protocol.log` by name** instead of anywhere.
pub struct StateTable {
    values: Mutex<BTreeMap<String, Value>>,
    playlist: Mutex<Vec<String>>,
    log: Arc<Logger>,
    playlist_index: Mutex<i64>,
    /// `(title, uri, time)` rows, newest first — the reference's three fields
    /// (`protocol.py:1211-1215`), and since 2026-10-18 **also in the settings
    /// file**, because that is where the reference keeps them and a restart that
    /// empties the 播放历史 card is a divergence the user can see. See
    /// [`StateTable::with_setting`] for the read half and
    /// [`StateTable::persist_history`] for the write half.
    history: Mutex<Vec<(String, String, i64)>>,
    /// The settings file these rows are mirrored into, or `None` for a table
    /// built without an app around it (a test, or the boot order in `main.rs`
    /// before the setting exists). `None` is not "memory is authoritative": it
    /// is "nothing to persist to", and [`StateTable::with_setting`] says so in
    /// the log rather than letting the rows live only in RAM silently.
    history_store: Mutex<Option<Arc<Setting>>>,
    /// Who hears about a changed state variable. `None` until `main.rs` has
    /// built the eventing service; the boot writes never broadcast anyway.
    broadcaster: Mutex<Option<SharedBroadcast>>,
}

/// A poisoned lock still holds the table; the state a client reads is worth more
/// than the panic that dropped it. Same rule `macast-mpv::player::guard` uses.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl StateTable {
    /// Build the table the way a fresh `DLNAProtocol` is built: the SCPD loader
    /// first (`init_services` → `build_action`), then `init_state()`
    /// (`protocol.py:650-659`, called from `__init__`). Doing it in that order is
    /// what makes the boot values right rather than plausible:
    /// `TransportPlaySpeed` ends up the **number** `1`, not the empty string the
    /// XML would leave, and both counters end up `2147483647`.
    pub fn new(log: Arc<Logger>) -> Self {
        let mut values: BTreeMap<String, Value> = BTreeMap::new();
        for (name, datatype) in DECLARED {
            let value = match datatype {
                's' => json!(""),
                _ => json!(0),
            };
            values.insert(name.to_string(), value);
        }
        // `<defaultValue>` exists for exactly two variables tree-wide, and the
        // loader sets it as **raw text** — including for `DisplayCurrentSubtitle`,
        // whose dataType is boolean. So its boot value is the string "1", and
        // `bool('1')` is what the reference reads back as `True`.
        values.insert("CurrentPlayMode".to_string(), json!("NORMAL"));
        values.insert("DisplayCurrentSubtitle".to_string(), json!("1"));
        for name in NOT_IMPLEMENTED {
            values.insert(name.to_string(), json!("NOT_IMPLEMENTED"));
        }

        let table = Self {
            values: Mutex::new(values),
            playlist: Mutex::new(Vec::new()),
            log: log.clone(),
            // Nothing has been cast yet, so there is no track to point at —
            // `-1`, not `0`, which would read as "the first item is loaded".
            playlist_index: Mutex::new(-1),
            history: Mutex::new(Vec::new()),
            history_store: Mutex::new(None),
            broadcaster: Mutex::new(None),
        };
        // `init_state()`, in the reference's own order. The debug lines are the
        // reference's `setState: {} {}` — a log the user reads should look like
        // the one they have always read.
        table.write("CurrentPlayMode", json!("NORMAL"), log.as_ref(), false);
        table.write("TransportPlaySpeed", json!(1), log.as_ref(), false);
        table.write("TransportStatus", json!("OK"), log.as_ref(), false);
        table.write(
            "RelativeCounterPosition",
            json!(2147483647),
            log.as_ref(),
            false,
        );
        table.write(
            "AbsoluteCounterPosition",
            json!(2147483647),
            log.as_ref(),
            false,
        );
        table.write("A_ARG_TYPE_Direction", json!("Output"), log.as_ref(), false);
        table.write("CurrentConnectionIDs", json!("0"), log.as_ref(), false);
        table.write("PlaybackStorageMedium", json!("None"), log.as_ref(), false);
        table.write(
            "SinkProtocolInfo",
            json!(protocol_info(log.as_ref())),
            log.as_ref(),
            false,
        );
        table
    }

    /// `set_state(name, value)` (`protocol.py:1025-1036`), with the two rules
    /// that make it more than an assignment: `changed` is computed **before**
    /// anything is queued (92% of the reference's duration reports were the same
    /// value, and broadcasting those is what this rule ended), and an unknown
    /// name is an ERROR here instead of the reference's `KeyError` — see the
    /// module doc. Returns whether this write was a change.
    pub fn write(&self, name: &str, value: Value, log: &Logger, broadcast: bool) -> bool {
        let mut values = lock(&self.values);
        let Some(current) = values.get(name) else {
            // Deviation, by name, so a typo is still findable in the log.
            log.error(&format!("setState: unknown state variable '{}'", name));
            return false;
        };
        // Mixed types compare unequal exactly like Python's `!=`: `json!(1)` is
        // not `json!("1")`, and `json!("1")` is not `json!(true)`. That matters
        // because the reference mixes them too — `TransportPlaySpeed` is the
        // number 1 at boot and a string when a sender sets a speed.
        let changed = *current != value;
        log.debug(&format!("setState: {} {}", name, render(&value)));
        // The broadcast needs the value twice — once for the table, once for the
        // subscribers — so it pays for the clone and nothing else does.
        let announce = changed && broadcast && OBSERVED.contains(&name);
        let wired = if announce { Some(value.clone()) } else { None };
        if changed {
            values.insert(name.to_string(), value);
        }
        // The guard goes down before the broadcast. `enqueue` takes the eventing
        // service's own lock, and the reference's rule for that lock is the same
        // one AGENTS.md §4.2 records for the subscriber table: nobody may sit on
        // one ledger while touching another's I/O, or a dead client holds the
        // state table.
        drop(values);
        if let Some(value) = wired {
            match self.broadcaster() {
                Some(shared) => shared.enqueue(name, &value),
                // Nothing is listening yet, which is a fact worth one line: the
                // name *was* observed and *was* changed, and the window where
                // that can happen is only the boot before `main.rs` wires the
                // eventing service in.
                None => log.debug(&format!(
                    "setState: '{}' is observed but no eventing service is wired",
                    name
                )),
            }
        }
        changed
    }

    /// Hand the table its GENA broadcast target — `DLNAProtocol`'s
    /// `state_queue`/`_state_event` pair, as the object `macast-upnp` owns.
    pub fn set_broadcaster(&self, shared: SharedBroadcast) {
        *lock(&self.broadcaster) = Some(shared);
    }

    /// Bound once in `main.rs`'s `.setup()`, so any write that reaches `write`
    /// after the services are up is broadcast. Reading it is a short lock and a
    /// clone of an `Arc`, never a clone of the table.
    fn broadcaster(&self) -> Option<SharedBroadcast> {
        lock(&self.broadcaster).clone()
    }

    /// `get_state(name).value` for the strings the control surface reads back.
    /// A number or bool is rendered the way Python's `%s` would render it, so
    /// `TransportPlaySpeed` reads as `1` and not as a JSON literal.
    pub fn read_str(&self, name: &str) -> String {
        match lock(&self.values).get(name) {
            Some(value) => render(value),
            None => String::new(),
        }
    }

    pub fn read(&self, name: &str) -> Option<Value> {
        lock(&self.values).get(name).cloned()
    }

    /// Everything, sorted by name — what `query=states` would hand a client, and
    /// what the page shows under the player card.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        lock(&self.values)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// `if uri not in self.playlist: self.playlist.append(uri)` followed by
    /// `self.playlist.index(uri)` (`protocol.py:1180-1182`): a second push of the
    /// same address lands on the track it already occupies, and both the index
    /// and the length come out of the same list.
    pub fn playlist_push(&self, uri: &str) -> (i64, i64) {
        let mut playlist = lock(&self.playlist);
        if !playlist.iter().any(|item| item == uri) {
            playlist.push(uri.to_string());
        }
        let index = playlist.iter().position(|item| item == uri).unwrap_or(0);
        (index as i64, playlist.len() as i64)
    }

    pub fn playlist(&self) -> Vec<String> {
        lock(&self.playlist).clone()
    }

    /// `self.current_index` (`protocol.py:1120`, `1142`, `1193`): the playlist
    /// position of what the player is showing right now, or `-1` for "nothing
    /// has been cast this run".
    pub fn playlist_index(&self) -> i64 {
        *lock(&self.playlist_index)
    }

    pub fn set_playlist_index(&self, index: i64) {
        *lock(&self.playlist_index) = index;
    }

    /// `PlaybackGuard._add_history` (`protocol.py:1203-1220`): newest first,
    /// deduped by URI, capped at 50 so the settings file stays small. An empty
    /// title stores the URI, because the history's job is to be recognisable
    /// later and a blank row is not.
    ///
    /// **The list goes into `Play_History` in the user's settings file, as it
    /// does in the reference.** Until 2026-10-18 this shell kept the rows in
    /// memory only, and the comment here defended that with AGENTS.md §10
    /// ("don't write the user's configuration") — §10 is about *verification
    /// paths*, and an app persisting its own history is exactly what the user's
    /// file is for. The gap was closed with a restart-survival test on both
    /// layers (`host::tests::a_history_row_survives_a_second_setting_instance`
    /// and `verify_rust_receiver.sh` §27), not with a line in this comment.
    pub fn add_history(&self, title: &str, uri: &str) {
        {
            let mut history = lock(&self.history);
            history.retain(|(_, item, _)| item != uri);
            let name = if title.is_empty() { uri } else { title };
            history.insert(0, (name.to_string(), uri.to_string(), now_secs() as i64));
            if history.len() > 50 {
                history.truncate(50);
            }
        }
        self.persist_history();
    }

    pub fn play_history(&self) -> Vec<(String, String, i64)> {
        lock(&self.history).clone()
    }

    pub fn clear_history(&self) {
        lock(&self.history).clear();
        self.persist_history();
    }

    /// Attach the settings file the history is mirrored into, and read the rows
    /// that are already there. Consumed at boot by `main.rs`, once, before any
    /// face can cast.
    ///
    /// The read is [`Setting::peek`], never `get`: `get` takes its default into
    /// the table (§4.2's first trap, and this port reproduces the side effect),
    /// so asking "is there a history?" with a default of `[]` would **create the
    /// key on every clean install** and turn "a first run" into "a cleared list"
    /// in the user's own file.
    ///
    /// What is said depends on which of the three readings it is: rows restored
    /// (INFO — the page can offer yesterday's casts again, and that is news),
    /// unreadable entries dropped (WARNING — someone edited the file or an older
    /// build wrote it), and both kinds of empty (DEBUG — there is nothing to
    /// report, but the two are still told apart, because §23 delivered this
    /// table's first red as `[]` with two causes).
    pub fn with_setting(self, setting: Arc<Setting>) -> Self {
        let stored = setting.peek(Key::PlayHistory);
        let (rows, dropped) = match &stored {
            Some(value) => parse_history(value),
            None => (Vec::new(), 0),
        };
        if dropped > 0 {
            self.log.warning(&format!(
                "play history: {} of {} entr{} in {} could not be read as a row \
                 (an entry needs a non-empty `uri`); it is dropped here, and the \
                 next cast rewrites the list without it",
                dropped,
                stored.as_ref().map(Value::as_array).flatten().map_or(0, Vec::len),
                if dropped == 1 { "y" } else { "ies" },
                Key::PlayHistory.name()
            ));
        }
        match (&stored, rows.is_empty()) {
            (Some(_), false) => self.log.info(&format!(
                "play history: {} row(s) read back from {} — the page offers them again",
                rows.len(),
                Key::PlayHistory.name()
            )),
            (Some(_), true) => self.log.debug(
                "play history starts empty because the key is there and holds nothing a cast left behind",
            ),
            (None, _) => self.log.debug(
                "play history starts empty because this settings file has no Play_History key yet (a first run, not a cleared one)",
            ),
        }
        *lock(&self.history) = rows;
        *lock(&self.history_store) = Some(setting);
        self
    }

    /// Mirror [`StateTable::play_history`] into the settings file, the way
    /// `Setting.set(SettingProperty.Play_History, history)` does at
    /// `protocol.py:1216`. `Setting::save` already says what happened when a
    /// write fails, so nothing is logged here beyond the one case that is this
    /// table's own fact: no file attached, so the rows cannot outlive the run.
    fn persist_history(&self) {
        let rows = self.play_history();
        let setting = lock(&self.history_store).clone();
        let Some(setting) = setting else {
            self.log.debug(
                "play history changed, but this table was built without a settings file: \
                 these rows end with the process",
            );
            return;
        };
        setting.set(Key::PlayHistory, history_document(&rows));
    }
}

/// The rows as the reference writes them: an array of objects carrying exactly
/// `uri`, `title` and `time` (`protocol.py:1211-1215`).
///
/// Key **order inside each object** follows this port's settings table, which is
/// a `BTreeMap` — alphabetical (`time`, `title`, `uri`) rather than the
/// reference's insertion order. Same recorded deviation as the top-level table,
/// and the only readers are the page and the 高级设置 JSON editor, both of which
/// address the keys by name.
fn history_document(rows: &[(String, String, i64)]) -> Value {
    Value::Array(
        rows.iter()
            .map(|(title, uri, at)| json!({ "uri": uri, "title": title, "time": at }))
            .collect(),
    )
}

/// The `Play_History` list read back: the rows this table can use, and how many
/// entries it could not.
///
/// Pure over the JSON value on purpose — the three shapes of "empty" (no key, an
/// empty list, a list of things that are not rows) have to be testable without a
/// settings file, and §4.14's 「空结果要说清是哪一种空」 is the reason the caller
/// gets a count rather than one `Vec`.
///
/// Dropping an unreadable entry **is a divergence**: the reference filters junk
/// only on its next write (`protocol.py:1208-1210`) and `query=status` echoes
/// whatever the file holds, so a hand-edited entry survives there and not here.
/// The count is what makes that drop an announced fact instead of a silent one,
/// and §8 carries the row.
fn parse_history(value: &Value) -> (Vec<(String, String, i64)>, usize) {
    let mut rows = Vec::new();
    let mut dropped = 0usize;
    for entry in value.as_array().map(|items| items.as_slice()).unwrap_or(&[]) {
        let uri = entry.get("uri").and_then(Value::as_str).unwrap_or("");
        if uri.is_empty() {
            dropped += 1;
            continue;
        }
        let title = entry.get("title").and_then(Value::as_str).unwrap_or(uri);
        let at = entry.get("time").and_then(Value::as_i64).unwrap_or(0);
        rows.push((title.to_string(), uri.to_string(), at));
    }
    (rows, dropped)
}

/// The `StateStore` seam `macast-upnp` reads and writes through.
///
/// Two things differ from the inherent methods, and both are the trait's
/// wording rather than an accident: `write` has no logger parameter (this table
/// owns its `Arc<Logger>` — the reference's `set_state` reaches for the module
/// logger the same way), and `playlist_push` hands back the pair of **state
/// variables** (`CurrentTrack`, `NumberOfTracks`), so the 0-based position the
/// list reports becomes the 1-based one a control point reads
/// (`protocol.py:1122`: `self.current_index + 1`).
impl macast_upnp::StateStore for StateTable {
    fn read(&self, name: &str) -> Option<Value> {
        self.read(name)
    }

    fn write(&self, name: &str, value: Value) -> bool {
        // `broadcast: false` here is not "never broadcast": it is "this store is
        // not the one that decides who hears about it" — that is `crate::eventing`
        // in macast-upnp, which owns the subscriber table and the queue.
        self.write(name, value, self.log.as_ref(), false)
    }

    fn playlist_push(&self, uri: &str) -> (i64, i64) {
        let (index, total) = self.playlist_push(uri);
        (index + 1, total)
    }

    fn playlist(&self) -> Vec<String> {
        self.playlist()
    }

    fn playlist_index(&self) -> i64 {
        self.playlist_index()
    }

    fn set_playlist_index(&self, index: i64) {
        self.set_playlist_index(index);
    }

    fn add_history(&self, title: &str, uri: &str) {
        self.add_history(title, uri);
    }

    fn play_history(&self) -> Vec<(String, String, i64)> {
        // Delegates to the inherent read of the same name; `self.x()` resolves to
        // the inherent method, so this is a forward, not the trait calling itself.
        StateTable::play_history(self)
    }

    /// Same shape as [`StateStore::play_history`]: the trait method forwards to the
    /// inherent one, which is the only place that knows how to mirror the rows into the
    /// settings file. A second `persist_history` call written here would be the second
    /// owner of "what cleared history costs on disk".
    fn clear_history(&self) {
        StateTable::clear_history(self)
    }
}

/// The `Renderer` seam, which is the player wearing a second face.
///
/// A newtype rather than an `impl` on [`Player`] itself because the start
/// position means different things on each side. [`Player`] speaks mpv: it takes
/// an optional start because the reference's `set_media_url` has one (`macast/renderer.py:76`, default `"0"`). This trait
/// speaks the *sender's* protocol, and which of its callers has a position to
/// hand over is a fact about the wire, not about the player — DLNA's
/// `SetAVTransportURI` carries none, so a sender that wants a position sends
/// `Seek` afterwards (`macast/protocol.py:1180-1250`), while Cast's `LOAD`
/// puts one in `currentTime` (`macast/protocol_cast.py:995`). So this type's
/// whole job is to be the place where that difference is stated, and to pass
/// whatever the wire said straight through.
///
/// `Player` is already a handle (`Clone` shares one `Arc<Shared>`), so this is a
/// handle of a handle: `main.rs` builds one per `Soap`, and both point at the one
/// mpv thread.
pub struct ShellRenderer {
    pub player: Player,
}

impl macast_upnp::store::Renderer for ShellRenderer {
    /// `note_media_live` and not `set_media_live`: the reference enumerates its
    /// plugin bus by the `set_media_` prefix, so a name spelled that way would
    /// subscribe the whole application to a message nobody sends
    /// (AGENTS.md §4.2, "这一条故意不叫 `set_media_live`"). The pass-through is
    /// exact — including `None`, which is "keep today's behaviour" and not
    /// "this is a file".
    fn note_media_live(&self, live: Option<bool>) {
        self.player.note_media_live(live);
    }

    /// Straight through: `None` is the reference's default `"0"`, and the
    /// loadfile builder drops a position that is empty or `0` so the command
    /// keeps the bare shape. Who had a position and who did not is the wire's
    /// answer, not this type's.
    fn set_media_url(&self, uri: &str, start: Option<&str>) {
        self.player.set_media_url(uri, start);
    }

    fn set_media_title(&self, title: &str) {
        self.player.set_media_title(title);
    }

    /// The sender's clock string, verbatim, to mpv's own parser — see the trait
    /// doc for why this crate is not allowed to read it first.
    fn set_media_position(&self, target: &str) {
        self.player.set_media_position(target);
    }

    fn set_media_volume(&self, value: Value) {
        self.player.set_media_volume(&value);
    }

    fn set_media_mute(&self, value: Value) {
        self.player.set_media_mute(&value);
    }

    fn set_media_pause(&self) {
        self.player.set_media_pause();
    }

    fn set_media_resume(&self) {
        self.player.set_media_resume();
    }

    fn set_media_stop(&self) {
        self.player.set_media_stop();
    }

    /// The window title for an address that arrived with no title. This is the
    /// same function `cast_uri` uses, so the name a sender sees in the DLNA
    /// state table and the name on the mpv window cannot disagree (AGENTS.md
    /// §4.2, "改播放器的地址，就必须改播放器的名字").
    fn title_for_uri(&self, uri: &str) -> String {
        title_for_uri(uri)
    }
}
/// The same player, wearing the AirPlay face.
///
/// Five methods, each a straight pass to [`Player`], and that plainness is the
/// point: `protocol_airplay.py:251-280` calls `self.renderer.…`, where the
/// reference's `renderer` is **the renderer instance**, not the `PlaybackGuard`.
/// So an AirPlay cast writes no DLNA state, no playlist and no play history —
/// the 设置页 keeps showing whatever the last DLNA or Cast handover said. That is
/// the reference's behaviour and this port copies it rather than improving it; the
/// consequence is written into `docs/Rust-Port-Design.md` §8 so nobody reads the
/// stale card as an AirPlay bug.
///
/// What is **not** a straight pass: nothing. The naming rule lives in one place
/// (`airplay.rs` asks `resolve::title_for_uri`, the same owner the DLNA state table
/// and the Cast channel ask), so the four ways a stream can arrive here cannot give
/// one address four window titles.
impl macast_cast::airplay::RendererCallback for ShellRenderer {
    /// AirPlay's RTSP carries no start position, so `None` — the bare
    /// `loadfile <url> replace` shape, not a seek to zero.
    fn set_media_url(&self, url: &str) {
        self.player.set_media_url(url, None);
    }

    fn set_media_title(&self, title: &str) {
        self.player.set_media_title(title);
    }

    fn set_media_pause(&self) {
        self.player.set_media_pause();
    }

    fn set_media_stop(&self) {
        self.player.set_media_stop();
    }

    /// The RTSP layer has already turned the sender's `volume:` body into the
    /// 0–100 integer the reference hands its renderer (`protocol_airplay.py:274`
    /// multiplies by 100 before calling); this is where that number meets the
    /// JSON value the player's own setter takes.
    fn set_media_volume(&self, volume: i32) {
        self.player.set_media_volume(&json!(volume));
    }
}

/// `load_xml(XMLPath.PROTOCOL_INFO.value).strip()`.
///
/// If the resource is not on disk there is no faithful answer to give — Python
/// would raise here and the receiver would not start — so this logs the path it
/// looked for and boots the variable empty rather than inventing a MIME list.
/// The verification run sets `MACAST_RESOURCE_DIR` to the repo's `macast/`
/// directory, which is where the file actually is.
fn protocol_info(log: &Logger) -> String {
    let path = paths::base_path(PROTOCOL_INFO_RESOURCE);
    match std::fs::read_to_string(&path) {
        Ok(text) => text.trim().to_string(),
        Err(e) => {
            log.error(&format!(
                "cannot read {} ({}): SinkProtocolInfo boots empty",
                path.display(),
                e
            ));
            String::new()
        }
    }
}

/// `%s` for a state value: strings verbatim, numbers and bools as JSON renders
/// them (`true` / `false` differ from Python's `True` / `False`, so they are
/// spelled the way the DLNA event does, which is the only place a client sees
/// them).
fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".to_string(),
        Value::Bool(false) => "0".to_string(),
        other => other.to_string(),
    }
}

/// The `Host` implementation: one window's worth of app handle, the settings
/// file, the state table, and a log that belongs to the renderer.
///
/// Every field is a handle rather than a value because `Player::new` takes the
/// host **by value** and `Host: 'static` forbids borrowing anything it did not
/// own — so `main.rs` clones these `Arc`s out and moves the host in, and keeps
/// its own copies for the commands.
pub struct TauriHost {
    pub app: AppHandle,
    pub setting: Arc<Setting>,
    pub table: Arc<StateTable>,
    pub log: Arc<Logger>,
    pub service_stopped: Arc<AtomicBool>,
    /// The Chromecast receiver, once `main.rs` has built one.
    ///
    /// A slot rather than a field on `Player` because the direction is
    /// **inbound**: mpv's events arrive here as [`StateCall`]s, and Python's
    /// `ProtocolGroup` fans each one out to every protocol in the group — which
    /// is how `protocol_cast.py:774-795`'s five `set_state_*` methods have any
    /// content at all. Without that fan-out the Cast wire answers
    /// `MEDIA_STATUS` from a ledger nothing ever fed: a phone that casts, then
    /// presses play, reads back whatever the LOAD happened to say.
    ///
    /// `Arc<Mutex<..>>` and not a plain field because this struct is cloned into
    /// `Player` and into every `SharedHooks`; a fresh `Mutex` per clone would
    /// mean the clone that receives mpv's events is not the clone that was given
    /// the receiver, and the fan-out would silently never fire.
    pub cast: Arc<Mutex<Option<Arc<macast_cast::channel::CastChannel>>>>,
}

impl TauriHost {
    /// Hand the host its Cast receiver. Called once, from `main.rs`, before the
    /// player starts — a `StateCall` that arrives first would simply not find a
    /// receiver to tell, which is the same nothing that happens today.
    pub fn set_cast(&self, channel: Arc<macast_cast::channel::CastChannel>) {
        *lock(&self.cast) = Some(channel);
    }

    /// Empty the slot on the way out, so a stopped receiver cannot be fanned
    /// into any longer. `stop_services` does it because the alternative is a
    /// `StateCall` updating the ledger of a channel whose sockets are gone: quiet,
    /// harmless today (this shell has no restart path), and exactly the kind of
    /// stale answer that costs someone an afternoon when the path appears.
    pub fn clear_cast(&self) {
        *lock(&self.cast) = None;
    }

    /// The receiver, if one is wired. Cloned out of the lock so no `CastChannel`
    /// method is ever called while this table's mutex is held.
    fn cast(&self) -> Option<Arc<macast_cast::channel::CastChannel>> {
        lock(&self.cast).clone()
    }
}

impl Clone for TauriHost {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            setting: self.setting.clone(),
            table: self.table.clone(),
            log: self.log.clone(),
            service_stopped: self.service_stopped.clone(),
            cast: Arc::clone(&self.cast),
        }
    }
}

impl Host for TauriHost {
    /// The old `cherrypy.engine.publish(name, *args)`. Here it is a Tauri event,
    /// and the payload is the argument list — the page does
    /// `listen('mpv_start', ({payload}) => …)` and reads `payload[0]`.
    ///
    /// The `Result` is logged, never unwrapped: `Emitter::emit` validates the
    /// event name and can fail, and a failed notification should not take the
    /// player thread down with it.
    fn publish(&self, event: &str, args: &[Value]) {
        if let Err(e) = self.app.emit(event, args) {
            self.log
                .error(&format!("cannot publish {} to the page: {}", event, e));
        }
    }

    fn stop_service(&self) {
        // One implementation, reached from two seams. The work lives in the
        // `Hooks` impl below because that is the trait `macast-upnp` is written
        // against; this arm exists so the renderer's bus call lands on it too.
        macast_upnp::store::Hooks::stop_service(self);
    }

    fn transport_state(&self) -> String {
        self.table.read_str("TransportState")
    }

    fn current_url(&self) -> String {
        self.table.read_str("CurrentTrackURI")
    }

    /// `Setting.get(PlayerDefaultVolume, 100)` — a [`Value`], not an `i64`, for
    /// the reason in the trait doc: a hand-typed string in the advanced settings
    /// JSON reaches mpv and the log as that string, and coercing it here would be
    /// a silent fix nobody asked for. Note the reference's documented side
    /// effect: reading a missing key **writes** the default into memory and
    /// persists it (`macast/utils.py`, §4.2), and `Setting::get` reproduces that.
    fn default_volume(&self) -> Value {
        self.setting.get(Key::PlayerDefaultVolume, json!(100))
    }

    fn player_size(&self) -> Value {
        // Read on **every** hand-off, exactly as `set_media_url` does at
        // `mpv.py:204-206`; `Config::size` is the command line's frozen value.
        self.setting.get(Key::PlayerSize, json!(argv::DEFAULT_SIZE))
    }

    fn friendly_name(&self) -> String {
        self.setting.friendly_name()
    }

    /// `publish('get_protocol')` then `.pop()`, so the last protocol answers.
    /// This shell owns exactly one state table, so the empty-list case that the
    /// trait doc carves out (`None`, not `Some("")`) cannot arise here; the table
    /// always has `RelativeTimePosition`, and at boot it is the empty string —
    /// which is what makes the loadfile builder return the bare
    /// `['loadfile', url, 'replace']` shape, as the reference does.
    fn current_position(&self) -> Option<String> {
        Some(self.table.read_str("RelativeTimePosition"))
    }

    fn log(&self, level: Level, text: &str) {
        self.log.log(level, text);
    }

    /// The paired writes, one `set_state*` per arm, in the reference's order —
    /// `macast/protocol.py:1258-1322`. Each arm is a method there; the names are
    /// kept in the comments so the next reader can diff this against the source.
    fn set_state(&self, call: StateCall) {
        let table = self.table.as_ref();
        let log = self.log.as_ref();
        // The `ProtocolGroup` fan-out, held until **after** the table writes
        // below: in the group the DLNA child is primary and its write is what a
        // GENA subscriber hears, so fanning second means a Cast listener that is
        // asked `MEDIA_STATUS` right after the event reads a ledger that agrees
        // with the table instead of racing it. `StateCall` is `Clone`, and the
        // clone is cheap next to a socket write — the alternative, matching twice
        // on one owned value, cannot compile at all.
        let cast = self.cast();
        let fan = call.clone();
        match call {
            // set_state_position: RelativeTimePosition + AbsoluteTimePosition
            StateCall::Position(text) => {
                table.write("RelativeTimePosition", json!(text.clone()), log, true);
                table.write("AbsoluteTimePosition", json!(text), log, true);
            }
            // set_state_duration: CurrentTrackDuration + CurrentMediaDuration
            StateCall::Duration(text) => {
                table.write("CurrentTrackDuration", json!(text.clone()), log, true);
                table.write("CurrentMediaDuration", json!(text), log, true);
            }
            // set_state_volume: ('Volume', data), an int
            StateCall::Volume(value) => {
                table.write("Volume", json!(value), log, true);
            }
            // set_state_mute: ('Mute', data), raw and unguarded
            StateCall::Mute(value) => {
                table.write("Mute", value, log, true);
            }
            // set_state_speed: ('TransportPlaySpeed', data)
            StateCall::Speed(value) => {
                table.write("TransportPlaySpeed", value, log, true);
            }
            // set_state_display_subtitle: ('DisplayCurrentSubtitle', data)
            StateCall::Subtitle(value) => {
                table.write("DisplayCurrentSubtitle", value, log, true);
            }
            StateCall::CurrentTrack(value) => {
                table.write("CurrentTrack", json!(value), log, true);
            }
            StateCall::NumberOfTracks(value) => {
                table.write("NumberOfTracks", json!(value), log, true);
            }
            // set_state_transport('PAUSED_PLAYBACK') — which is also
            // ('TransportStatus', 'OK'), in that order
            StateCall::Pause => transport(table, log, "PAUSED_PLAYBACK"),
            StateCall::Play => transport(table, log, "PLAYING"),
            StateCall::Stop => transport(table, log, "STOPPED"),
            // set_state_eof
            StateCall::Eof => transport(table, log, "NO_MEDIA_PRESENT"),
            // set_state_transport_error
            StateCall::TransportError => {
                table.write("TransportState", json!("STOPPED"), log, true);
                table.write("TransportStatus", json!("ERROR_OCCURRED"), log, true);
            }
        }
        // Only the five transport-ish calls go out. The rest (`Volume`, `Mute`,
        // positions, track counters) have no counterpart in the reference:
        // `grep -n 'def set_state' macast/protocol_cast.py` finds exactly
        // `eof/transport_error/stop/pause/play` (`:774-795`) and nothing else, so
        // a sixth call here would be this port inventing a fan-out target the
        // Python receiver does not have.
        if let Some(channel) = &cast {
            match fan {
                StateCall::Pause => channel.set_state_pause(),
                StateCall::Play => channel.set_state_play(),
                StateCall::Stop => channel.set_state_stop(),
                StateCall::Eof => channel.set_state_eof(),
                StateCall::TransportError => channel.set_state_transport_error(),
                _ => {}
            }
        }
    }
}

/// The `Hooks` seam — the two things outside `macast-upnp` that a protocol answer
/// depends on, which the crate deliberately refuses to reach into: tell the human
/// something, and stop answering the phone.
///
/// `TauriHost` is the implementor rather than a second struct because it already
/// owns both handles each method needs (`app` for the page, `table` for nothing
/// here) and because `Arc::new(host.clone()) as SharedHooks` is then the whole
/// wiring cost in `main.rs`. The two `stop_service` methods that now exist on this
/// type are one behaviour, not two: [`Host::stop_service`] delegates here.
impl macast_upnp::store::Hooks for TauriHost {
    /// `cherrypy.engine.publish('app_notify', title, text)`. The payload is the
    /// two-argument list, same shape as [`Host::publish`]'s, so the page's single
    /// `listen('app_notify', …)` handler reads `payload[0]` and `payload[1]` for
    /// both routes — the renderer's notification and the receiver's.
    ///
    /// Logged, never unwrapped, for the reason recorded in AGENTS.md §4.2: the
    /// caller here is SSDP's bind-failure path running on `SSDP_STOP_THREAD`, and
    /// a failed notification on that thread must still let the stop itself happen.
    fn notify(&self, title: &str, text: &str) {
        if let Err(e) = self.app.emit("app_notify", [title, text]) {
            self.log
                .error(&format!("cannot notify the page ({}): {}", title, e));
        }
    }

    /// The reference's "stop the service and tell the user", where *the service*
    /// is the receiver: HTTP face, SSDP announcer, GENA event thread. The player
    /// is not in that list — see the module doc's second deviation.
    ///
    /// `try_state` rather than `state`, and the order is load-bearing: the flag
    /// goes first because it is the only answer a caller can read on a machine
    /// where `Runtime` was never managed (a setup that failed, a test host that
    /// builds a `TauriHost` directly). Panicking there would turn "nothing to
    /// stop" into a crashed window, which is the same judgement call as deviation
    /// number one above.
    fn stop_service(&self) {
        self.service_stopped.store(true, Ordering::SeqCst);
        match self.app.try_state::<crate::Runtime>() {
            Some(runtime) => runtime.stop_services(),
            None => self.log.warning(
                "stop_service: no managed runtime yet, so the flag is the only record \
                 of this request",
            ),
        }
    }
}

/// `set_state_transport(data)`: the state first, then `TransportStatus` back to
/// `OK` (`protocol.py:1303-1306`). Two writes, never one.
fn transport(table: &StateTable, log: &Logger, state: &str) {
    table.write("TransportState", json!(state), log, true);
    table.write("TransportStatus", json!("OK"), log, true);
}

/// `DLNAHandler.cast_uri(uri, title)` — `macast/protocol.py:1163-1201` —
/// reproduced as one ordered sequence. The order is the contract, so it is
/// written in the reference's order and every step is named:
///
/// 1. an empty address returns without touching anything (`if not uri: return`);
/// 2. the title is **stripped** before it is used for anything;
/// 3. `self._media_live = None` — whoever is starting this media has not
///    declared whether it is live, and a stale answer from the previous media
///    would give this one read-ahead it never asked for (§4.1, §4.2);
/// 4. `set_state_url` → `CurrentTrackURI`;
/// 5. the renderer gets the url, then the **name** (§4.2: change the player's
///    address and you must change the player's name);
/// 6. `CurrentTrackTitle` gets the same string the player was named with;
/// 7. the playlist, then `NumberOfTracks` / `CurrentTrack` derived from it;
/// 8. positions back to `00:00:00`, `TransportState` to `PLAYING`,
///    `TransportStatus` to `OK`;
/// 9. `_add_history` is **not** implemented here — see the comment at the call.
///
/// The one thing that differs from Python is deliberate: a refusal says so.
/// Returning "success" for a request that did nothing is the bug §4.7c records,
/// and an empty address is exactly that shape.
///
/// Step 8's `PLAYING` is **not** witnessed by mpv, and that is the reference's
/// behavior rather than a porting gap: `protocol.py:1199` writes it from the
/// requesting thread the moment it hands the address over. Whatever correction
/// comes, comes later from mpv's own `playback-restart` (`mpv.py:362-367`, ported
/// at `macast-mpv/src/state.rs:340`), which answers from the renderer's `pause`
/// bookkeeping. That bookkeeping only updates while `playing` is true
/// (`mpv.py:291-292` / `state.rs:218-232`), so a 暂停 clicked against an *idle*
/// player leaves it `false` and the next cast reads `PLAYING` while mpv really
/// sits at `pause=yes`, `time-pos` 0.0 — measured on this machine (2026-10-07)
/// through this shell's page, and identical in the Python app. Left as-is on
/// purpose: this port's contract is the reference's contract, not a better one.
pub fn cast_uri(host: &TauriHost, player: &Player, uri: &str, title: &str) -> Result<Value, String> {
    let given = title.trim().to_string();
    let uri = uri.trim();
    if uri.is_empty() {
        return Err("没有要投屏的地址".to_string());
    }

    // 3. `set_media_live(None)` for the displaced protocol.
    player.note_media_live(None);
    // The same moment tells the *other* door to give up its opinion —
    // `ProtocolGroup` fans `release_playback()` to every child that is not the
    // new owner (`macast/protocol.py`'s guard hands the player over), and on the
    // Cast side that child is `protocol_cast.py:946`, whose `_media` stays while
    // `_media_live` clears. Skipping this line is not cosmetic: the next LOAD on
    // the Cast wire would inherit "this is live" from a clip that stopped playing
    // minutes ago, and AGENTS.md §4.1's whole three-state rule is about not
    // letting a stale answer drive a fresh handover.
    if let Some(channel) = host.cast() {
        channel.release_playback();
    }

    // 4. `set_state_url(uri)`.
    host.table
        .write("CurrentTrackURI", json!(uri), host.log.as_ref(), true);

    // 5. `renderer.set_media_url(uri)` then `set_media_title(...)`.
    player.set_media_url(uri, None);
    let name = if given.is_empty() {
        title_for_uri(uri)
    } else {
        given.clone()
    };
    player.set_media_title(&name);

    // 6. `set_state('CurrentTrackTitle', given or media_resolve.title_for_uri(uri))`
    //    — the same expression, evaluated the same way, for the same reason: the
    //    player and the protocol must not disagree about what is playing.
    host.table
        .write("CurrentTrackTitle", json!(name.clone()), host.log.as_ref(), true);

    // 7. playlist + track counters, and the position `_playlist_step` steps from.
    let (index, number) = host.table.playlist_push(uri);
    host.table.set_playlist_index(index);
    host.table
        .write("NumberOfTracks", json!(number), host.log.as_ref(), true);
    host.table
        .write("CurrentTrack", json!(index + 1), host.log.as_ref(), true);
    host.table
        .write("CurrentTrackURI", json!(uri), host.log.as_ref(), true);

    // 8. positions, then the transport pair in `set_state_transport`'s order.
    host.table
        .write("RelativeTimePosition", json!("00:00:00"), host.log.as_ref(), true);
    host.table
        .write("AbsoluteTimePosition", json!("00:00:00"), host.log.as_ref(), true);
    transport(&host.table, host.log.as_ref(), "PLAYING");

    // 9. `_add_history(uri, title)` — in the table only. The reference writes this
    //    list into `Play_History` in the user's settings file; this shell keeps the
    //    rows in memory, because §10 forbids a verification path from writing his
    //    configuration, and a row that is read back by nothing still costs him a
    //    write on every cast.
    host.table.add_history(&given, uri);

    Ok(json!({
        "uri": uri,
        "title": name,
        "track": index + 1,
        "total": number,
    }))
}

/// `Handler.RESOLVE_JOBS_MAX` and `Handler.RESOLVE_JOB_TTL`
/// (`macast/protocol.py:2726-2727`). They live here rather than in
/// `macast-cast` because the trait says the implementer owns the slot accounting
/// (`macast-upnp/src/http.rs:1074-1079`): the layer that decides *which* finished
/// job is replaceable is the layer that knows a page is polling for a running one.
const RESOLVE_JOBS_MAX: usize = 4;
const RESOLVE_JOB_TTL: f64 = 30.0 * 60.0;

/// The shell's clock, as a `f64` of seconds since the epoch, because
/// [`resolve::ResolveJob::run`] and
/// [`macast_cast::resolve::ResolveJob::status`] take `&dyn Fn() -> f64` while
/// [`relay::Relay::status_json`] reads `SystemTime` itself. One seam so the two
/// readings of "now" cannot drift apart inside one status document.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `Handler._resolve_jobs` (`macast/protocol.py:2725`): id → job, in the order
/// they were inserted. A `Vec` rather than a map because Python's dict is
/// insertion-ordered and [`prune_locked`] has to evict the **first** finished row
/// — which is only a well-defined answer in a sequence.
///
/// What a row holds is a **published snapshot**, not the live job: a
/// [`resolve::ResolveJob`] is `Send` but **not** `Sync`, so the running job cannot
/// be shared between the worker thread and a polling HTTP thread at all. The
/// snapshots come from [`PublishingOps`] (at every step the reference publishes a
/// state change under) and from `on_finish`. The cost, stated plainly: a poll that
/// arrives in the middle of the measure loop sees the row as of `enter("measure",
/// …)`, whereas Python's reader touches the live object and sees partial counts.
/// Nothing in the four sentences `status()` renders changes meaning — the counts
/// it reports are the ones that were true at the last step boundary.
pub type ResolveJobs = Arc<Mutex<Vec<(String, resolve::ResolveJob)>>>;

/// `_publish`-shaped helper: replace the row for `id`, **only if it is still
/// there**. A worker whose job was evicted (TTL, or the first-finished slot) keeps
/// running in Python and keeps writing into an object nobody can reach; pushing on
/// a miss would bring a thirty-minute-old job back into the table and hand the page
/// an answer for a parse nobody asked for any more.
fn publish(jobs: &ResolveJobs, id: &str, snapshot: resolve::ResolveJob) {
    let mut table = lock(&jobs);
    if let Some(row) = table.iter_mut().find(|(row_id, _)| row_id == id) {
        row.1 = snapshot;
    }
}

/// `Handler._prune_resolve_jobs` (`macast/protocol.py:2869-2884`, 「Lock held.」).
/// First free anything older than the TTL, then take the slot if that left room.
/// Only over the line **and** all still working counts as full: 「a finished job is
/// replaceable, a running one is the answer some page is polling for」.
fn prune_locked(table: &mut Vec<(String, resolve::ResolveJob)>) -> bool {
    let deadline = now_secs() - RESOLVE_JOB_TTL;
    table.retain(|(_, job)| job.started >= deadline);
    if table.len() < RESOLVE_JOBS_MAX {
        return true;
    }
    match table.iter().position(|(_, job)| job.done) {
        Some(index) => {
            table.remove(index);
            true
        }
        None => false,
    }
}

/// `Handler.COOKIE_JAR_NAME` (`macast/protocol.py:2729-2740`). Not in the settings
/// JSON: a jar is 「a pile of session secrets」 while the settings file is 「the
/// thing people export, attach to bug reports and paste into chats」, and yt-dlp
/// **writes that file back** after every run.
const COOKIE_JAR_NAME: &str = "resolve_cookies.txt";

/// `Handler._cookie_jar_path` (`macast/protocol.py:2742-2743`):
/// `os.path.join(SETTING_DIR, COOKIE_JAR_NAME)`. `ensure = false` — reading a jar
/// must not create a directory, let alone the jar.
fn cookie_jar_path() -> Option<PathBuf> {
    paths::setting_path(COOKIE_JAR_NAME, false).ok()
}

/// `Handler._cookie_jar_text` (`macast/protocol.py:2745-2760`): read
/// `COOKIE_JAR_MAX_BYTES + 1` bytes, swallow every OS error as the empty string,
/// decode with replacement. The `+ 1` is the whole point — a jar on disk that is
/// too big has to fail `cookie_jar_state` the same way an oversized **paste** does,
/// instead of being silently truncated into something that parses.
///
/// Three readers, and none of them echoes the text: `query=resolve-cookies`'s
/// `jar` key and its counts, [`cookie_settings`]'s "is there a file at all", and
/// `set_cookie_browser`'s "will this switch actually take effect" — which asks only
/// whether a jar exists. The values themselves never go near a page or a log
/// (AGENTS.md §4.14).
fn cookie_jar_text() -> String {
    let Some(path) = cookie_jar_path() else {
        return String::new();
    };
    let Ok(file) = File::open(&path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(resolve::COOKIE_JAR_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return String::new();
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `Handler._cookie_browser_spec` (`macast/protocol.py:2762-2779`). Two rules,
/// both load-bearing:
///
/// * **Presence is asked before the value is read.** [`Setting::get_str`] delegates
///   to [`Setting::get`], which takes a missing key's default **into the table**
///   for the next `save()` (AGENTS.md §4.2; `macast-core/src/setting.rs:335-440`).
///   Python's guard is `if not Setting.has(...): return ''`, so the `has` here is
///   that guard and not a stylistic choice.
/// * **The whitelist is re-checked on every read**, not only on the way in. The
///   value sits in the hand-editable advanced-settings JSON, so a stored string
///   nobody validated is still possible; anything the engine does not recognise
///   reads as "no browser", which is a real answer rather than a switch a
///   subprocess would interpret on its own.
fn cookie_browser_spec(setting: &Setting) -> String {
    if !setting.has(Key::ResolveCookieBrowser) {
        return String::new();
    }
    let stored = setting.get_str(Key::ResolveCookieBrowser, "");
    resolve::cookie_browser_spec(&stored).unwrap_or_default()
}

/// `Handler._cookie_settings` (`macast/protocol.py:2781-2800`): both halves when
/// both are set, and the file wins — that rule lives in
/// [`resolve::cookie_arguments`], not here.
///
/// Note what `file` carries: the **path**, not the jar's text. Python builds
/// `cookies['file'] = self._cookie_jar_path()`; the jar is only read to find out
/// whether there is one. Handing the contents over would put session secrets in
/// `--cookies`' position on a command line, where every process on the machine can
/// read it.
fn cookie_settings(setting: &Setting) -> resolve::JobCookies {
    let jar_present = !cookie_jar_text().is_empty();
    let path = cookie_jar_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let browser = cookie_browser_spec(setting);
    let file = if jar_present { path.as_str() } else { "" };
    resolve::JobCookies::from(&resolve::Cookies { file, browser: &browser })
}

/// The answer to "which device?" when the id the card sent is no longer a device.
///
/// Byte-for-byte the reference's sentence (`cast_local_file.py:2926-2927`), and it
/// is worth saying why a *stale id* is a different answer from a *failed push*:
/// the dropdown is a cache of a multicast that happened seconds ago, so the row
/// the user pointed at may have gone to sleep or left the network. Silently
/// falling back to this machine would play the clip somewhere the user did not
/// ask for; `push_url_to`'s own failure sentence («投给「…」失败：…») is for a
/// device we still believe in that said no.
///
/// This replaced `DEVICE_PUSH_UNAVAILABLE`, a refusal this shell used while the
/// send face (`crate::push`) existed but nothing called it. The honesty rule is
/// unchanged — a card that cannot reach a device must say so — but the fact it
/// reports is now the user's stale row rather than our missing wiring, so
/// `scripts/verify_rust_receiver.sh` §16 asserts the new shape instead of the old
/// sentence.
const DEVICE_ROW_GONE: &str = "设备列表里没有这一台（它可能刚关机，或者搜索结果已过期），请点「重新搜索」再投";

/// `_local_target` (`macast/protocol.py:1796-1807`): the card's first option.
///
/// The name is the **selected renderer's title**, not the word 「本机」, because
/// "this machine" is not one thing: with MPV it is a window on this Mac, with a
/// caster selected it is that caster's own chosen screen. A row that said 本机
/// would let the card promise one target and reach another.
///
/// The read is the same key `query=status` reports (`macast/protocol.py:2007`,
/// ported at `main.rs`'s status face) — with one difference that is Python's, not
/// ours: the status face ships the raw value (`''` when nothing was ever written),
/// while this row substitutes `'MPV'`, because a dropdown whose first row has a
/// blank label is not a label. So the two can legitimately differ on a fresh
/// install, and neither is a bug: one answers "what is stored", the other answers
/// "what pressing 投屏 will reach".
///
/// `address` is empty because there is no address to give: `local` is a route
/// through our own player, not a socket, and `push_url_to` never sees this row.
fn local_target(setting: &Setting) -> Value {
    let renderer = setting.get_str(Key::MacastRenderer, "");
    json!({
        "id": "local",
        "kind": "local",
        "name": if renderer.is_empty() { "MPV" } else { &renderer },
        "address": "",
    })
}

/// The name Python prints in front of an OS error: `type(exc).__name__`
/// (`macast/protocol.py:3005`, `:3041`). Both sentences a user can read quote it
/// — 「保存失败：PermissionError: …」 — so this is a contract with a **string**, not a
/// style choice: the four names are what a cookie file can actually hit (it is a
/// write into the config directory, so `FileNotFoundError` from a vanished parent,
/// `PermissionError` from a foreign-owned jar, and the two directory-shaped ones
/// from someone replacing the file with a directory). Everything else answers
/// `OSError`, which is also Python's answer for the remaining kinds.
///
/// One honest deviation, and it lives in the message rather than the name: Rust's
/// `io::Error` Display is `Permission denied (os error 13)` where Python renders
/// `[Errno 13] Permission denied: '/path'`. Both name the errno; the path is not
/// repeated here because the write failure is logged at ERROR with the path (the
/// reference's `logger.error('cookie jar write failed for %s: %s', path, exc)`), so
/// the sentence that reaches the page stays short and the log stays complete.
fn oserror_name(err: &std::io::Error) -> &'static str {
    match err.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        std::io::ErrorKind::NotADirectory => "NotADirectoryError",
        _ => "OSError",
    }
}

/// `os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)`
/// (`macast/protocol.py:3003-3004`). The mode is the red line: a jar is session
/// secrets, and a 0644 file is readable by every account on the machine
/// (AGENTS.md §4.14). Two facts about `mode()` that match `open(2)` and therefore
/// match Python: it only applies when the file is **created** (a stale `.tmp` from
/// an earlier crash keeps its own bits — same in Python), and the umask filters it.
#[cfg(unix)]
fn open_jar_temp(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// The same shape without the mode argument, because **Windows has no POSIX mode
/// bits**: the file inherits the parent directory's ACL — which is also exactly
/// what Python's `os.open(…, 0o600)` does there, where the mode is documented as
/// only meaningful on Unix. Keeping both halves the same function keeps the half
/// that *does* matter (the atomic replace below) identical code on both platforms.
#[cfg(not(unix))]
fn open_jar_temp(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// The write half of `Handler._post_resolve_cookies` (`macast/protocol.py:3000-3012`),
/// in the reference's order and with its two deliberate properties:
///
/// 1. **The directory is created before anything is opened in it** —
///    `os.makedirs(SETTING_DIR, exist_ok=True)`. `paths::setting_path("", true)` is
///    that line: `rel = ""` yields the directory itself, `ensure = true` runs
///    `create_dir_all` (AGENTS.md §4.2 「写文件的代码要自己把目录建出来」 — a shipped
///    build once failed to start its Cast receiver for exactly this omission).
/// 2. **No backup of the previous jar.** AGENTS.md §4.14: a feature that will
///    overwrite the file on purpose should not also keep a copy of it around,
///    because the copy is the same pile of secrets in a second place.
///
/// Write to `resolve_cookies.txt.tmp`, then `std::fs::rename` over the destination.
/// `rename` is `os.replace`: atomic within one filesystem, so a parse running
/// concurrently reads either the whole previous jar or the whole new one, never
/// the first kilobyte of a half-written file. On Windows `std::fs::rename` is
/// `MoveFileExW(…, MOVEFILE_REPLACE_EXISTING)`, so the replace-over-existing
/// behaviour is the same and no fallback branch is needed.
///
/// The trailing-newline rule is Python's (`text if text.endswith('\n') else text +
/// '\n'`): a paste that ends mid-line without a newline would leave that last line
/// unterminated, and the next `--cookies` reader would see a truncated field.
fn write_cookie_jar(text: &str) -> std::io::Result<()> {
    let dir = paths::setting_path("", true)?;
    let path = paths::setting_path(COOKIE_JAR_NAME, false)?;
    let temporary = dir.join(format!("{COOKIE_JAR_NAME}.tmp"));
    {
        let mut file = open_jar_temp(&temporary)?;
        file.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            file.write_all(b"\n")?;
        }
    }
    std::fs::rename(&temporary, &path)
}

/// The worker's `ResolveOps`: the real engine, with a snapshot published into
/// [`ResolveJobs`] **before** every call that follows an `_enter(...)`. That
/// ordering is the trait doc's own sentence (`macast-cast/src/resolve.rs:3330-3336`)
/// read from the other side — `_enter` runs before the call, so by the time the
/// engine is being asked, the step name and `step_started` and the facts the
/// caller passed have already moved; publishing first is what lets a poll see the
/// step the work is actually in. `measure` is the case that matters: publishing
/// before it is the only way a page gets `step = "measure"` plus the `FromYtdlp`
/// and `EngineNote` counts, and with `MAX_MEASURED = 8` the extra clones are
/// nothing.
///
/// `has_ytdlp` / `max_measured` / `max_per_host` delegate plain: no state moved
/// (`has_ytdlp` runs inside the private `why_empty`, before the final `on_finish`
/// publish, and the two caps are constants on this side).
struct PublishingOps {
    jobs: ResolveJobs,
    id: String,
}

impl PublishingOps {
    fn publish(&self, job: &resolve::ResolveJob) {
        publish(&self.jobs, &self.id, job.snapshot());
    }
}

impl resolve::ResolveOps for PublishingOps {
    fn scrape_page(
        &self,
        job: &resolve::ResolveJob,
        page_url: &str,
    ) -> Result<Vec<resolve::Candidate>, resolve::ResolveError> {
        self.publish(job);
        resolve::RealResolveOps.scrape_page(job, page_url)
    }

    fn ytdlp_candidates(
        &self,
        job: &resolve::ResolveJob,
        page_url: &str,
        cookies: &resolve::Cookies<'_>,
        sink: &mut Vec<String>,
    ) -> Result<Vec<resolve::Candidate>, resolve::ResolveError> {
        self.publish(job);
        resolve::RealResolveOps.ytdlp_candidates(job, page_url, cookies, sink)
    }

    fn measure(
        &self,
        job: &resolve::ResolveJob,
        candidate: &mut resolve::Candidate,
    ) -> Result<resolve::Probe, resolve::ResolveError> {
        self.publish(job);
        resolve::RealResolveOps.measure(job, candidate)
    }

    fn rank(
        &self,
        job: &resolve::ResolveJob,
        candidates: Vec<resolve::Candidate>,
        ceiling: f64,
    ) -> Result<Vec<resolve::Candidate>, resolve::ResolveError> {
        self.publish(job);
        resolve::RealResolveOps.rank(job, candidates, ceiling)
    }

    fn has_ytdlp(&self) -> bool {
        resolve::RealResolveOps.has_ytdlp()
    }

    fn max_measured(&self) -> usize {
        resolve::RealResolveOps.max_measured()
    }

    fn max_per_host(&self) -> usize {
        resolve::RealResolveOps.max_per_host()
    }
}

/// The [`macast_upnp::http::Management`] seam: the shell's hand on the player,
/// the resolve jobs, and the relay store.
///
/// A newtype for the same reason [`ShellRenderer`] is one — the wording belongs to
/// the HTTP face, not to [`Player`]. The trait answers with **whole documents**
/// now, mirroring the four reference routes one for one: `_cast_url`'s success dict
/// with its two extras, and `_resolve_page` / `_resolve_status` /
/// `_post_cast_resolved` with their measured key orders
/// (`macast-upnp/src/http.rs:1030-1061`). [`cast_uri`] still answers with a JSON
/// object of its own, so this is the place that turns "what happened to the player"
/// into the document a `curl` caller reads.
///
/// It runs **on an HTTP worker thread**, which is not a new situation for this
/// shell: every [`ShellRenderer`] method the SOAP layer calls arrives on a worker
/// thread too, and `cast_uri` touches only the state table and the player — no
/// AppKit, no `AppHandle`. The reference's GET entry is the same shape (it is a
/// CherryPy handler calling the same sequence), so no marshalling is added here.
/// The two routes that *do* reach outside that set are `resolve-page`, which hands
/// the work to a thread of its own, and `cast-resolved`, which asks the network —
/// both also synchronous handlers in the reference.
///
/// `title` is handed over verbatim, including `None`: [`cast_uri`] falls back to
/// [`title_for_uri`] when the caller said nothing, which is §4.2's
/// 「改播放器的地址，就必须改播放器的名字」 rather than a default this struct gets
/// to invent.
pub struct ShellManagement {
    pub host: TauriHost,
    pub player: Player,
    /// The store `/relay/<id>/media` is served from, and the same `Arc` the
    /// accept loop holds.
    pub store: Arc<relay::RelayStore>,
    /// `SETTING_DIR/relay`, created by `main.rs` before the server started
    /// (AGENTS.md §4.2「写文件的代码要自己把目录建出来」 — `spawn_remux` hands the
    /// path straight to ffmpeg and the sweep that runs before the accept loop
    /// reads the directory).
    pub temp_dir: PathBuf,
    /// The port the relay's accept loop actually bound. Not the application port:
    /// the reference serves `/relay` off the same CherryPy tree, while this shell
    /// has one listener per concern, so the address handed to the player is built
    /// from **this** number.
    pub port: u16,
    pub jobs: ResolveJobs,
    /// The device caches behind the dropdown and the device arm of a cast.
    ///
    /// One table for the whole shell, read by both faces that answer about
    /// "what is out there": [`ShellManagement::cast_targets`] polls it (and is the
    /// only thing that kicks a cold search), and [`ShellManagement::cast_resolved`]
    /// looks the card's id up in it before pushing. A second `Table` would give
    /// those two questions different answers — AGENTS.md §4.8's oldest lesson, one
    /// owner per fact.
    ///
    /// `Arc` rather than a bare `Table`: the type is already a `Clone` handle onto
    /// one shared `Inner`, but the shell hands it to a trait object that is itself
    /// shared, and `Clone`-ing a handle at each use site would let a future reader
    /// believe they copied a ledger.
    pub targets: Arc<targets::Table>,
    /// The handle `set-interface` needs: the pinned 网卡, the SSDP listener, and the
    /// announcer that has to say goodbye and come back on the new addresses.
    ///
    /// `Arc`, and the **same** `Arc` `Runtime` holds, because the window's picker and
    /// this face are two doors on one switch. A copy of the setting alone would let the
    /// page answer 「已切换到 en0」 while the announcement kept the previous interface's
    /// addresses for the rest of the session — which is the exact failure this row used
    /// to have when only the window could switch at all.
    pub discovery: Arc<crate::Discovery>,
}

impl macast_upnp::http::Management for ShellManagement {
    /// `_set_network_interface` (`macast/protocol.py:1977-1995`), through the one handle
    /// the window's picker also uses — see [`ShellManagement::discovery`].
    fn set_interface(&self, name: &str) -> macast_upnp::http::Answer {
        self.discovery.set_interface(name)
    }

    /// The protocol switch is **the shell's**, not this face's: `Discovery` owns the
    /// receivers, the announcement and the one read of the enabled list, so the write
    /// and the cycling it asks for have to happen where those live. `macast-upnp` can
    /// bind neither 8009 nor 7000 and must not pretend it knows what "switched off"
    /// costs (see the trait doc).
    fn toggle_protocol(&self, title: &str) -> macast_upnp::http::Answer {
        self.discovery.toggle_protocol(title)
    }

    /// `quit` needs an `AppHandle`, so this lives in the shell for the same reason the
    /// switch does: `macast-upnp` cannot end a process (`protocol.py:2404-2414`).
    fn app_action(&self, action: &str) -> macast_upnp::http::Answer {
        self.discovery.app_action(action)
    }

    /// `_set_renderer` (`protocol.py:2385-2392`): match the raw trimmed title against
    /// the renderer list and refuse with the reference's sentence when nothing matches.
    ///
    /// The list here has one entry ([`renderer_title`]) because this shell ships one
    /// renderer — so the **only** accepted request is the name already in force, and
    /// accepting it still does something real: it persists the canonical title (a fresh
    /// install's `''` becomes `MPV`, which is what the dropdown shows) and reloads the
    /// player, which is the reference's stop/start of the selected renderer
    /// (`macast.py`'s `set_renderer` handler). Refusing everything else is the honest
    /// half: a switch that pretends to find a plugin this build does not have is the
    /// document-only control this port keeps refusing.
    /// `_post_save_launch_param` (`protocol.py:2640-2648`): the whole table, then a
    /// restart — **answer first**, the §35 shape, or the reply dies with the process it
    /// is telling you about. `json format error` is the reference's own sentence, and a
    /// non-object is refused with it (the reference accepts one and breaks on the next
    /// read; §8 records that divergence).
    fn save_launch_param(&self, raw: &str) -> macast_upnp::http::Answer {
        let parsed: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) => return macast_upnp::http::Answer::fail("json format error"),
        };
        let map = match parsed {
            serde_json::Value::Object(map) => map,
            _ => return macast_upnp::http::Answer::fail("json format error"),
        };
        self.host.setting.replace_all(map);
        let log = self.host.log.clone();
        let app = self.host.app.clone();
        let spawned = std::thread::Builder::new()
            .name("MACAST_RESTART".to_string())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(250));
                log.info("launch param saved; restarting the app");
                app.restart();
            });
        if spawned.is_err() {
            return macast_upnp::http::Answer::fail(
                "设置已保存，但重启线程起不来；重启 Macast 之后新设置才会全面生效",
            );
        }
        macast_upnp::http::Answer::ok("success")
    }

    fn set_renderer(&self, title: &str) -> macast_upnp::http::Answer {
        let current = renderer_title(&self.host.setting);
        if title.trim() != current {
            return macast_upnp::http::Answer::fail(format!("未找到播放器：{}", title));
        }
        self.host
            .setting
            .set(Key::MacastRenderer, json!(current));
        self.player.reload();
        macast_upnp::http::Answer::ok(format!("播放器已切换为 {}", current))
    }

    /// `_cast_url` (`macast/protocol.py:1674-1695`).
    ///
    /// The `Err` half is deliberately not the reason: Python catches every
    /// exception out of the sequence, logs `cast url error: %s` at ERROR, and
    /// answers `{'code': 1, 'message': 'cast failed'}`. A raw string in the HTTP
    /// document would be this shell leaking an internal sentence the reference
    /// never shows — the reason belongs in the log, which is where §4.10 sent every
    /// other one of them.
    ///
    /// The success document's `'url'` is the **trimmed** address (the value
    /// [`cast_uri`] actually handed the player, so the page reports what plays),
    /// while `'title'` is the caller's argument as passed — `''` when the caller
    /// said nothing. Not the name we derived for the player: that would make this
    /// route's answer depend on a fallback the caller never wrote.
    fn cast_url(&self, url: &str, title: Option<&str>) -> Result<Answer, String> {
        match cast_uri(&self.host, &self.player, url, title.unwrap_or("")) {
            Ok(sent) => {
                // `logger.info('cast url from web endpoint: %s', url)`, and only on
                // this arm — the reference logs the success, not the attempt.
                let uri = sent.get("uri").and_then(Value::as_str).unwrap_or("");
                self.host
                    .log
                    .info(&format!("cast url from web endpoint: {uri}"));
                Ok(
                    Answer::ok("success")
                        .with("url", json!(uri))
                        .with("title", json!(title.unwrap_or(""))),
                )
            }
            Err(why) => {
                self.host
                    .log
                    .error(&format!("cast url error: {why}"));
                Err("cast failed".to_string())
            }
        }
    }

    /// `_resolve_page` (`macast/protocol.py:2828-2867`), in that method's order:
    /// validate → resolve the address → build the job → take an id → **then** the
    /// lock, which covers only the prune and the insert → start the work → log.
    ///
    /// `max_height` is `0.0` because it is a constructor default in the reference
    /// (`media_resolve.py:1397`) and this route never passes one
    /// (`protocol.py:2855-2856`); `rank(pool, 0.0)` means "no ceiling".
    fn resolve_page(&self, text: &str) -> Answer {
        let stripped = text.trim();
        if stripped.is_empty() {
            return Answer::fail("请先填写网页地址");
        }
        let (url, found) = match resolve::resolve_target(stripped) {
            Ok(pair) => pair,
            // `PyUrlError`'s `Display` is Python's `ValueError` message, and the
            // module doc's deviation (host.rs:1-31) is that an unknown shape is
            // logged rather than raised. This one has no unknown shape: the two
            // raises it can make are the ones Python renders into the page too.
            Err(why) => {
                self.host
                    .log
                    .error(&format!("resolve target failed for {stripped}: {why}"));
                return Answer::fail(&why.to_string());
            }
        };
        if url.is_empty() {
            if stripped.split_whitespace().count() == 1 {
                return Answer::fail("地址要以 http:// 或 https:// 开头");
            }
            return Answer::fail(
                "这段文字里没有找到网址；请连分享原文一起贴进来，但要包含以 http:// 或 https:// 开头的地址",
            );
        }
        let note =
            if url == stripped { String::new() } else {
                format!("从分享原文里读到 {found} 个网址，解析第一个：{url}")
            };

        let job_id = token::token_urlsafe(9);
        if job_id.is_empty() {
            // `token_urlsafe` swallows an RNG failure by returning the empty
            // string. An empty id would be a row every poll answers
            // 「没有这个解析任务」 for, so say what failed instead.
            self.host
                .log
                .error("could not generate a resolve job id (no system randomness)");
            return Answer::fail("本机没能生成解析任务编号，请重试");
        }

        let jobs = Arc::clone(&self.jobs);
        let spawn_id = job_id.clone();
        let spawn_url = url.clone();
        let cookies = cookie_settings(&self.host.setting);
        let note_for_job = note.clone();
        let started = now_secs();
        let finish_jobs = Arc::clone(&jobs);
        let finish_id = job_id.clone();
        let log = self.host.log.clone();

        let mut job = resolve::ResolveJob::new(
            &url,
            0.0,
            Some(Box::new(move |running| {
                publish(&finish_jobs, &finish_id, running.snapshot());
            })),
            &cookies.borrowed(),
            &note_for_job,
            started,
        );

        {
            let mut table = lock(&jobs);
            if !prune_locked(&mut table) {
                return Answer::fail(&format!(
                    "已经有 {RESOLVE_JOBS_MAX} 个解析在跑，请等其中一个结束"
                ));
            }
            table.push((job_id.clone(), job.snapshot()));
        }

        let spawn = thread::Builder::new().name("MacastResolve".to_string()).spawn(
            move || {
                let ops = PublishingOps {
                    jobs: Arc::clone(&jobs),
                    id: spawn_id,
                };
                job.run(&ops, &now_secs);
                // `on_finish` published the last state; this line is the log the
                // reference writes after `job.start()`, kept here because a worker
                // that died mid-parse should be visible in the file too.
                log.info(&format!("resolve worker finished: {spawn_url}"));
            },
        );

        match spawn {
            Ok(_) => {
                self.host
                    .log
                    .info(&format!("resolving page: {url}"));
                Answer::ok("success")
                    .with("job", json!(job_id))
                    .with("url", json!(url))
                    .with("note", json!(note))
            }
            Err(why) => {
                // The row was inserted above and the worker never ran, so the
                // snapshot would sit at `step = "queued"` for the whole TTL and
                // every poll would report a parse that is not happening. The lock
                // is on `self.jobs`, not the local `jobs`: that clone already moved
                // into the worker closure with the job, and it is the same table.
                let mut table = lock(&self.jobs);
                table.retain(|(id, _)| *id != job_id);
                drop(table);
                self.host
                    .log
                    .error(&format!("could not start the resolve worker for {url}: {why}"));
                Answer::fail("解析没能开始：本机起不了后台线程，请稍后重试")
            }
        }
    }

    /// `_resolve_status` (`macast/protocol.py:2890-2899`). The two shapes are the
    /// contract: 「没有这个解析任务（超时或已重启）」 carries `job: ""` and
    /// `status: null`, while a found row carries the **id that was asked for** and
    /// a seventeen-key object.
    ///
    /// An unanswered poll cannot consume a slot — the reference's GET route only
    /// looks the id up (`_resolve_job`, `protocol.py:2886-2888`) and
    /// `RESOLVE_JOBS_MAX` is spent at `resolve-page`.
    fn resolve_status(&self, job_id: &str) -> Answer {
        let table = lock(&self.jobs);
        match table.iter().find(|(id, _)| id == job_id) {
            None => Answer::fail("没有这个解析任务（超时或已重启）")
                .with("job", json!(""))
                .with("status", Value::Null),
            Some((_, job)) => {
                // A [`resolve::ResolveJob`] is not `Clone` (its `on_finish` is a
                // `Box<dyn Fn>`), so the row cannot be moved out and rendered
                // outside the lock the way Python's reader can. Holding it for the
                // duration of `status()` is the deviation: a poll can briefly delay
                // a publish, and nothing else — the job itself is only ever mutated
                // by its own worker.
                match job.status(&now_secs) {
                    Ok(status) => Answer::ok("success")
                        .with("job", json!(job_id))
                        .with("status", status),
                    Err(why) => {
                        self.host
                            .log
                            .error(&format!("resolve status failed for {job_id}: {why}"));
                        Answer::fail(&why.to_string())
                    }
                }
            }
        }
    }

    /// `_post_cast_resolved` (`macast/protocol.py:2904-2948`).
    ///
    /// `candidate` is a **raw string** because the reference converts it in the
    /// handler and answers 「没有这个候选项」 for anything that is not an index into
    /// the finished job's list. One divergence worth naming: Python's `int()`
    /// accepts `" 3 "`, while [`str::parse`] does not, so a padded index that would
    /// cast here answers 「没有这个候选项」. A malformed index that survives
    /// (`"3_0"`) lands on the same final answer in both.
    fn cast_resolved(&self, job: &str, candidate: &str, target: &str) -> Answer {
        let table = lock(&self.jobs);
        let Some((_, row)) = table.iter().find(|(id, _)| id == job) else {
            return Answer::fail("解析任务已经不在了，请重新解析");
        };
        if !row.done {
            return Answer::fail(&format!(
                "还在解析中（{}），请等它完成",
                resolve::step_label(&row.step)
            ));
        }
        let Ok(index) = candidate.parse::<i64>() else {
            return Answer::fail("没有这个候选项");
        };
        if index < 0 || index as usize >= row.candidates.len() {
            return Answer::fail("没有这个候选项");
        }
        // `Candidate` is `Clone` (`resolve.rs:297`), so the row can be read here
        // and released before the network work below. `candidates` is a fresh Vec
        // in Python (`list(job.candidates)`) for the same reason.
        let pick = row.candidates[index as usize].clone();
        let page_url = row.page_url.clone();
        drop(table);

        let title = match resolve::display_title(&pick, &page_url) {
            Ok(name) => name,
            Err(why) => {
                self.host
                    .log
                    .error(&format!("display title failed for {}: {why}", pick.url));
                return Answer::fail(&why.to_string());
            }
        };

        let relay = match relay::open_relay(&pick, &title, &self.store, &self.temp_dir) {
            Ok(relay) => relay,
            Err(why) => {
                // Both of the reference's two failure arms arrive here as one
                // sentence (`relay.rs:1359-1368`), and both already dropped the
                // relay from the store. 「中转启动失败：」 belongs to the arm that
                // catches an *unexpected* raise out of `open_relay`, which a Rust
                // caller does not have — prefixing it would put the reference's
                // diagnosis of itself on the origin's words.
                self.host
                    .log
                    .error(&format!("relay start failed for {}: {why}", pick.url));
                return Answer::fail(why);
            }
        };

        let host = relay::choose_advertise_host(
            &net::advertised_hosts(&self.host.setting),
            &net::advertised_addresses(&self.host.setting),
        );
        let url = relay::media_url(&relay, &host, self.port);

        let mut answer = if target.is_empty() || target == "local" {
            // `_cast_relayed`'s first arm calls `_cast_url(url, title)`, so this
            // route's local answer comes from the same method as the GET entry's —
            // one owner for that document, including its log lines.
            match self.cast_url(&url, Some(&title)) {
                Ok(answer) => answer,
                Err(why) => Answer::fail(why),
            }
        } else {
            // `_cast_relayed`'s device arm (`macast/protocol.py:2970-2983`), through
            // `target_push`'s two steps: look the id up **in the live caches**, then
            // push. The lookup is first and it is not a formality — the row the card
            // showed may name a television that has since slept or left the network,
            // and pushing to an address we no longer believe in would be a cast that
            // "succeeded" onto nothing.
            //
            // The device gets the **relay** address, never the page's URL; that is
            // the entire reason the relay exists, and it is why this arm is written
            // with `url` (built above from `relay` + the advertised host) and not
            // with `pick.url`.
            //
            // `live` is `not relay.seekable`: a stream whose byte range nobody has
            // promised cannot be scrubbed, so the sender must not promise a
            // seekable timeline. §4.1's three-state reading is *not* what lives here:
            // `push_url_to` gets a plain bool because this is our own page card
            // naming its own media, and by the time a clip reached the relay the
            // "we do not know" answer was already collapsed by `open_relay` into
            // whatever the probe could say about ranges and length.
            //
            // Both reference arms that could raise are unreachable in Rust and are
            // recorded rather than kept as dead code: `setting is None` (this shell
            // owns the table), and `except Exception` around `target_push` — whose
            // Rust counterpart returns `(bool, String)` and has already written the
            // ERROR line the reference's `logger.exception` would have produced.
            match self.targets.target_by_id(target) {
                None => Answer::fail(DEVICE_ROW_GONE).with("target", json!(target)),
                Some(row) => {
                    let content_type = relay.content_type();
                    let media = push::PushMedia {
                        title: &title,
                        content_type: &content_type,
                        duration: pick.duration(),
                        live: !relay.seekable(),
                    };
                    let (ok, message) = push::push_url_to(
                        &row,
                        &url,
                        &media,
                        &self.host.setting,
                        &self.host.log,
                    );
                    // `code` follows the plugin's `ok`, and the message is the
                    // plugin's sentence — the same words the caster's own menu
                    // prints, which is why they are not re-derived here. A failed
                    // hand-off still carries `target`, because Python builds the
                    // wrapper before it knows the outcome (`:2982-2983`), and the
                    // caller below drops the relay for `code != 0` either way.
                    let answer = if ok {
                        Answer::ok(message)
                    } else {
                        Answer::fail(message)
                    };
                    answer.with("target", json!(target))
                }
            }
        };

        if answer.code != 0 {
            // Python: `if result.get('code') != 0: media_relay.store.drop(...);
            // return result` (`protocol.py:2942-2944`). A cast the renderer refused
            // must not leave a stream nobody is playing squatting one of
            // `RELAY_MAX` slots for half an hour.
            self.store.drop_relay(&relay.relay_id);
            return answer;
        }

        // Then the two extras and the overwritten message, in the reference's
        // order: `message` keeps its original slot because `_post_cast_resolved`
        // assigns `relay` and `note` and rewrites `message` **in place**.
        answer = answer
            .with("relay", relay.status_json())
            .with("note", json!(resolve::relay_reason(&pick)));
        answer.message = "success".to_string();
        answer
    }

    /// `_cookie_state` (`macast/protocol.py:2794-2818`) — the panel read:
    /// **counts and names, never values**.
    ///
    /// The eight keys are in the reference's order, and the document is this
    /// shell's to build because every fact in it lives where the jar lives.
    /// `path` is the one string here that came off the filesystem: it is allowed
    /// because it is where the user would look to check our count, not what they
    /// would send us (§4.14 — the settings JSON gets exported into bug reports,
    /// and a jar is someone's live session credential).
    ///
    /// `cookie_jar_state`'s `Err` is not a refusal on this path. Reading a jar
    /// that is too big reports zero counts with `jar` still true, so the card can
    /// say "oversized, and the next parse will not use it" — the size cap is only
    /// a refusal when a human is *pasting* it (`save_cookies` below), where the
    /// caller can still fix it.
    fn cookie_state(&self) -> Answer {
        let text = cookie_jar_text();
        let (cookies, bad) = match resolve::cookie_jar_state(&text) {
            Ok(counts) => counts,
            Err(_) => (0, 0),
        };
        Answer::ok("success").with(
            "state",
            json!({
                "jar": !text.is_empty(),
                "cookies": cookies,
                "bad": bad,
                "path": cookie_jar_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                "browser": cookie_browser_spec(&self.host.setting),
                "browsers": resolve::COOKIE_BROWSERS,
                "max_bytes": resolve::COOKIE_JAR_MAX_BYTES,
                "yt_dlp": resolve::ResolveOps::has_ytdlp(&resolve::RealResolveOps),
            }),
        )
    }

    /// `_cast_targets` (`macast/protocol.py:1765-1794`): the card's device list,
    /// **never a live probe on this thread**.
    ///
    /// Both reference arms that used to be reachable here are now unreachable, and
    /// the honest way to say that is to say it rather than keep the code:
    ///
    /// - `setting is None` (the plugin that owns the caches is not loaded). This
    ///   shell *is* the owner — [`ShellManagement::targets`] is built at startup —
    ///   so there is no "no plugin" state to report. `scripts/verify_rust_receiver.sh`
    ///   §19 used to assert the refusal document that stood in for it; it now
    ///   asserts this one.
    /// - `except Exception` → 「读取设备列表失败：…」. The table's only failure mode
    ///   is a refused `thread::spawn` on the search path, and `Table::start` already
    ///   answers that itself (it logs, clears its own latch and returns `false`, so
    ///   the card gets `started: false` and can press 重新搜索). A lock here cannot
    ///   poison: [`lock_recovering`](macast_cast::targets) takes a panicked guard
    ///   back rather than propagating, because one dead search thread must not make
    ///   every later poll of the settings page panic.
    ///
    /// The local row is prepended **here**, not inside `Table`: Python's plugin
    /// answers the device rows and the core adds 「本机」, and the same split is what
    /// keeps `query=cast-targets` from claiming a device it does not own. The key
    /// order of the wrapper (`code,message,targets`) and of the inner document
    /// (`items,searching,known`, plus `started` last on a refresh) is asserted by
    /// that script — `serde_json`'s `preserve_order` is what makes re-assigning
    /// `items` an overwrite in place rather than a move to the end, which is
    /// exactly what Python's dict does and what a plain `BTreeMap` would get wrong.
    fn cast_targets(&self, refresh: bool) -> Answer {
        let mut state = if refresh {
            self.targets.target_refresh()
        } else {
            self.targets.target_state()
        };
        // `state.get('items') or []` (`:1793`): a document with no rows is still a
        // document, and `Table` always writes the key. The fallback is for the day
        // it does not, and it must not turn into 「本机 is the only device」 being
        // reported as a fact about the network.
        let mut rows = state
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rows.insert(0, local_target(&self.host.setting));
        state["items"] = Value::Array(rows);
        Answer::ok("success").with("targets", state)
    }

    /// `_post_resolve_cookies` (`macast/protocol.py:2986-3030`): validate the
    /// whole jar, then make it the saved one.
    ///
    /// The order is the product. `cookie_jar_state` runs **before** anything is
    /// opened, because a malformed jar is not an error to yt-dlp — it resolves
    /// anonymously and returns *fewer* addresses, which reads to the user as "this
    /// page has no video". And no copy of the previous jar is kept, deliberately:
    /// a feature that overwrites a credential cache must not leave a copy of every
    /// session it ever held.
    ///
    /// Every failure document is `{'code': 1, 'message': …}` with **no extras** —
    /// the counts only ride along with the success, whose message is built from
    /// them.
    fn save_cookies(&self, text: &str) -> Answer {
        let (cookies, bad) = match resolve::cookie_jar_state(text) {
            Ok(counts) => counts,
            Err(why) => return Answer::fail(why),
        };
        if cookies == 0 {
            return Answer::fail(format!(
                "这份文件里没有一条能读懂的 cookie（{bad} 行看不懂）。要 Netscape 格式：\
                 每行 7 个 TAB 分隔的字段"
            ));
        }
        if let Err(err) = write_cookie_jar(text) {
            // `logger.error('cookie jar write failed for %s: %s', path, exc)` —
            // the path goes to the log, not the page, because Rust's `io::Error`
            // Display has no room for it (see [`oserror_name`]).
            let path = cookie_jar_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.host
                .log
                .error(&format!("cookie jar write failed for {path}: {err}"));
            return Answer::fail(format!("保存失败：{}: {err}", oserror_name(&err)));
        }
        self.host
            .log
            .info(&format!("saved {cookies} cookies ({bad} unreadable lines) for page resolve"));
        let suffix = if bad > 0 {
            format!("，另有 {bad} 行看不懂（一并存了）")
        } else {
            String::new()
        };
        Answer::ok(format!("已保存 {cookies} 条 cookie{suffix}"))
            .with("cookies", json!(cookies))
            .with("bad", json!(bad))
    }

    /// `_post_resolve_cookies_clear` (`macast/protocol.py:3032-3043`). Three
    /// answers, and the first one is a **success**: the caller's goal is "no jar",
    /// so a machine that never had one has already reached it. Nothing is logged —
    /// the reference logs nothing here either, and a page-driven remove is not an
    /// event worth a line.
    fn clear_cookies(&self) -> Answer {
        let Some(path) = cookie_jar_path() else {
            return Answer::ok("本机没有保存过 cookie 文件");
        };
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Answer::ok("本机没有保存过 cookie 文件");
            }
            return Answer::fail(format!("删除失败：{}: {err}", oserror_name(&err)));
        }
        let browser = cookie_browser_spec(&self.host.setting);
        let suffix = if browser.is_empty() {
            String::new()
        } else {
            format!("，下次解析改用 {browser}")
        };
        Answer::ok(format!("已清除 cookie 文件{suffix}"))
    }

    /// `_post_resolve_cookie_browser` (`macast/protocol.py:3045-3081`): one raw
    /// field, `名字[:资料目录]`.
    ///
    /// "Off" **unsets** the key instead of storing `false` (§4.2 — a disabled
    /// setting is absent, and `Setting::get` would otherwise take the literal into
    /// the table). The name half is lowercased and whitelisted; the profile half is
    /// the user's path and travels byte-for-byte to
    /// [`resolve::cookie_browser_spec`], which is the only owner of that grammar —
    /// splitting it here would make this file a second write gate disagreeing with
    /// the read gate.
    ///
    /// The two refusals are ordered as the reference orders them, and the second
    /// one says which half failed: a user who typed `vivaldi:C:\Users\…` and hears
    /// "I don't know that browser" goes looking for a browser by that name.
    /// `set_media_sub_show` on the engine bus (`protocol.py:2702`), which in the shipped
    /// app lands on `MPVRenderer.set_media_sub_show` (`macast_renderer/mpv.py:235-236`)
    /// and becomes `set_property sub-visibility yes|no`. The value arrives here already
    /// reduced to a bool, exactly as Python reduces it in the handler — the four-word
    /// form rule lives in `macast-core::pyops`, not at this call site.
    ///
    /// Nothing is written to the state table from this method, and that is the point:
    /// `DisplayCurrentSubtitle` changes because **mpv reports the property back**
    /// (`macast-mpv/src/state.rs:182` → `Signal::Subtitle` → [`StateCall::Subtitle`]
    /// below), which is how Python's DLNA state ends up matching the player rather than
    /// matching the request. A player that is not running means a property that never
    /// echoes, and a status page that keeps the last value it was told — which is what
    /// the reference does too, and §30 measures the whole chain on a real mpv.
    fn set_sub_show(&self, show: bool) {
        self.player.set_media_sub_show(&json!(show));
    }

    fn set_cookie_browser(&self, text: &str) -> Answer {
        let text = text.trim();
        if ["", "0", "off", "none", "关闭", "不用"].contains(&text.to_lowercase().as_str()) {
            self.host.setting.unset(Key::ResolveCookieBrowser);
            return Answer::ok("不再从浏览器读取 cookie");
        }
        let name = text
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !resolve::COOKIE_BROWSERS.contains(&name.as_str()) {
            return Answer::fail(format!(
                "不认识「{name}」这个浏览器，可选：{}",
                resolve::COOKIE_BROWSERS.join("、")
            ));
        }
        let spec = resolve::cookie_browser_spec(text).unwrap_or_default();
        if spec.is_empty() {
            return Answer::fail(format!(
                "「{text}」我们只认到浏览器名字为止：后面可以跟一段「:资料目录」（那个浏览器\
                 存放 cookie 的目录），但不能是 +钥匙串、::容器，也不能以 - 开头"
            ));
        }
        self.host.setting.set(Key::ResolveCookieBrowser, json!(spec));
        if !cookie_jar_text().is_empty() {
            // Said out loud because the file outranks the browser in argv: without
            // this the switch reads as broken rather than as overridden.
            return Answer::ok(format!(
                "已设为 {spec}，但本机存过 cookie 文件，解析仍以文件为准（清除文件后才会用浏览器）"
            ));
        }
        Answer::ok(format!("下次解析会从 {spec} 读取 cookie"))
    }
}

/// `media_resolve.title_for_uri` (`macast/media_resolve.py:1299-1317`), ported to
/// the extent this shell needs it: strip a `#` fragment and a `?` query, drop the
/// scheme and netloc, take the last path segment, percent-decode it, trim, cut one
/// media suffix, cap it at [`TITLE_MAX_CHARS`], and fall back to the address
/// itself when the segment is empty.
///
/// This is **not** a full `urlsplit` port and does not pretend to be one: the
/// reference's own doc says the reason a name is needed is that mpv's `filename`
/// for a URL is only its last path segment (§4.2 — a `/relay/<id>/media` address
/// reads to the window as the word `media`), and the same arithmetic is what a
/// bare address needs here. `SEGMENTED_SUFFIXES` + `PROGRESSIVE_SUFFIXES` are
/// copied as one list because the reference concatenates them into
/// `MEDIA_SUFFIXES` before cutting.
const TITLE_MAX_CHARS: usize = 120;
const MEDIA_SUFFIXES: [&str; 9] = [
    ".m3u8", ".mpd", ".mp4", ".m4v", ".mov", ".mkv", ".webm", ".flv", ".ts",
];

pub fn title_for_uri(uri: &str) -> String {
    let mut path = uri.split('#').next().unwrap_or("");
    path = path.split('?').next().unwrap_or("");
    if let Some((_, rest)) = path.split_once("://") {
        // Drop the netloc; the reference does this with `urlsplit().path`, so a
        // scheme-less address keeps its whole string here as it would there.
        path = match rest.find('/') {
            Some(i) => &rest[i + 1..],
            None => "",
        };
    }
    let stem = path.rsplit('/').next().unwrap_or("");
    let mut stem = percent_decode(stem).trim().to_string();
    let lower = stem.to_lowercase();
    for suffix in MEDIA_SUFFIXES {
        if lower.ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
            break;
        }
    }
    if stem.is_empty() {
        return uri.to_string();
    }
    stem.chars().take(TITLE_MAX_CHARS).collect()
}

/// `urllib.parse.unquote` for the one job it has here: a percent-escaped segment
/// reads as its own name. An escape that is not a byte (`%zz`, a truncated `%`) is
/// left exactly as written, which is what `unquote` does too.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = &text[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // The reference decodes UTF-8 with `errors='replace'`; invalid sequences
    // become U+FFFD rather than dropping the rest of the name.
    String::from_utf8_lossy(&out).into_owned()
}

/// Collect the `.lua` names in `Setting.get_base_path('scripts')` the way the
/// reference does — `os.listdir` then `filter(endswith('.lua'))` — which means
/// **the order the directory hands back is kept**. Sorting here would change the
/// `--script=` order, and `argv.rs` has a test whose whole point is that a given
/// order survives.
pub fn lua_scripts(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Some(text) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if text.ends_with(".lua") {
            names.push(text);
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Which faces this run is allowed to open
// ---------------------------------------------------------------------------

/// The three protocol titles the reference's plugin list can hold, spelled as
/// `format_class_name()` spells them — `DLNA Protocol`, `Chromecast Protocol`,
/// `AirPlay Protocol`. They live here rather than inline at each gate because
/// AGENTS.md §4.2's rule about plugin titles is exactly this: the stored value
/// is a *title*, and `resolve_title()` is what tolerates the user's older or
/// shorter spelling. Hard-coding one spelling in one place is how 「DLNA」 ends up
/// silently dropping SSDP (the docstring at `macast.py:706-713` says as much).
/// The one owner of this string is the crate that **is** that class:
/// `macast_upnp::http::DLNA_PROTOCOL_TITLE`, which `query=subscribers` prints as the
/// child's `title` (`format_class_name(DLNAProtocol())` in the reference). Re-typing
/// it here would let the gate's vocabulary and the diagnostic document disagree about
/// the same protocol's name — the §4.2 rule about never comparing hand-typed titles
/// is exactly this alias's reason to exist.
pub const DLNA_TITLE: &str = macast_upnp::http::DLNA_PROTOCOL_TITLE;
pub const CAST_TITLE: &str = "Chromecast Protocol";
pub const AIRPLAY_TITLE: &str = "AirPlay Protocol";

/// `Macast.canonical_title` (`macast/macast.py:702-704`): trim, fold case,
/// collapse internal runs of whitespace, drop a trailing word `protocol`.
///
/// Which part of this fold actually carries weight was asked, not assumed (the
/// shapes below are in the spelling table of this file's tests, and every answer
/// was read off CPython's own function before being frozen here):
///
/// * the **collapse** is load-bearing. It is the step that lets the `" protocol"`
///   replace fire at all on a tab- or newline-separated spelling: `"Chromecast
///   \tProtocol"` without it stays `"chromecast\tprotocol"`, and there is no
///   literal `" protocol"` inside that string to remove. CPython's no-argument
///   `str.split()` is Unicode-aware and folds `\t`, `\n`, `\r\n` and `\u{a0}` to
///   one space; Rust's `split_whitespace()` is too, so the two agree — and a value
///   pasted out of a JSON editor can carry any of them.
/// * **folding case** is load-bearing, and for the mirrored reason: the replace
///   tests a lowercase `" protocol"`, so `"Chromecast  PROTOCOL "` needs the
///   lowercase step to reach it at all.
/// * the two **strips** are guards, not teeth, and that is a measurement rather
///   than a guess: deleting the leading one, or the trailing one, leaves every
///   the trailing one, leaves every case in the table answering the same —
///   `split_whitespace()` yields no empty tokens, so a leading or trailing run is
///   gone before the replace looks. The trailing strip would earn its place the
///   moment the collapse left: a double-spaced `"Chromecast  Protocol"` folded
///   without the collapse reaches `"chromecast "` (the replace eats one of the two
///   spaces), and only that strip turns it into `"chromecast"`. They stay because
///   the reference writes them (`macast.py:703` is `strip().lower().split()`
///   joined, then `strip()`) and this function's job is to agree with the reference
///   even where agreeing is redundant.
///
/// So the fold has two teeth and two guards, and which is which was asked of both
/// machines: deleting the collapse goes red on 1 case, deleting the case fold on
/// 4, deleting either strip on none. That last half belongs here rather than in a
/// comment claiming "every step matters", because a reader who trusts the latter
/// will delete a guard and hear nothing — or, worse, keep a tooth on the strength
/// of the guards.
pub fn canonical_title(title: &str) -> String {
    let folded = title.trim().to_lowercase();
    let collapsed = folded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
        .replace(" protocol", "")
        .trim()
        .to_string()
}

/// What the run should treat as the set of *available* protocol titles: the
/// faces this shell can actually open.
///
/// The reference derives this from the loaded plugin list, so a protocol whose
/// plugin is missing is not available and its title resolves to nothing. This
/// shell has no plugin manager yet (that subsystem is unported), so the list is
/// the faces `main.rs` knows how to start. AirPlay is named here even though its
/// receiver is not wired to the shell yet: the *title* is part of the stored
/// vocabulary a user may already have in `Macast_Protocols`, and treating it as
/// unknown would make a three-protocol settings file lose an entry.
pub fn available_titles() -> Vec<&'static str> {
    vec![DLNA_TITLE, CAST_TITLE, AIRPLAY_TITLE]
}

/// `Macast.enabled_protocol_titles` (`macast/macast.py:763-784`) plus the two
/// rules that live around it: the fresh-install default at `:1268-1271` and the
/// never-listen-with-nothing fallback at `build_protocol_group` (`:757-760`).
///
/// The reading is the reference's, line for line, and the three places it could
/// plausibly be gotten wrong are the three this function's tests ask about:
///
/// * a stored **string** where a list belongs is coerced to one entry. The
///   shared [`Setting::get_str_list`] returns an empty list for that shape —
///   correct for the status document, which only reports — but here an empty
///   list means "fresh install, enable everything", so the coercion is the
///   difference between honouring a user's narrowing and silently opening every
///   face they just closed.
/// * the legacy single-protocol key is appended, not preferred: an install that
///   has both keeps the list and gains the legacy entry if it is not already in
///   it. `has()` gates the read so a missing key stays "nothing configured";
///   [`Setting::get_str`] would insert an empty string on a miss, which is
///   §4.2's oldest trap in this repository.
/// * unknown titles are **skipped with a warning** rather than fatal, and a
///   stored list whose every entry is unknown falls back to DLNA alone. That is
///   the reference's rule — a removed plugin must not be able to leave the app
///   listening on nothing.
pub fn enabled_protocols(setting: &Setting, log: &Logger) -> Vec<String> {
    let available = available_titles();
    let mut stored: Vec<String> = match setting.peek(Key::MacastProtocols) {
        Some(Value::String(text)) => vec![text],
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };
    if setting.has(Key::MacastProtocol) {
        stored.push(setting.get_str(Key::MacastProtocol, ""));
    }
    if stored.iter().all(|entry| entry.trim().is_empty()) {
        // Nothing usable yet: a fresh install gets every protocol at once.
        return available.iter().map(|title| (*title).to_string()).collect();
    }
    let mut titles: Vec<String> = Vec::new();
    for entry in &stored {
        if entry.trim().is_empty() {
            continue;
        }
        match resolve_title(entry, &available) {
            Some(title) => {
                if !titles.contains(&title) {
                    titles.push(title);
                }
            }
            None => log.warning(&format!("Skipping unknown protocol: {}", entry)),
        }
    }
    if titles.is_empty() {
        log.warning("no listed protocol is known to this build; running DLNA only");
        titles.push(DLNA_TITLE.to_string());
    }
    titles
}

/// `Macast.resolve_title` (`macast/macast.py:706-722`): exact title first, then
/// the canonical comparison, and `None` when nothing matches. Always returns the
/// **real** title, so a stored `"chromecast"` cannot become the string that the
/// next read fails to match.
pub fn resolve_title(title: &str, available: &[&str]) -> Option<String> {
    if let Some(exact) = available.iter().find(|held| **held == title) {
        return Some((*exact).to_string());
    }
    let canonical = canonical_title(title);
    available
        .iter()
        .find(|held| canonical_title(held) == canonical)
        .map(|held| (*held).to_string())
}

/// Which protocol faces this run opens, from **one** read of `Macast_Protocols`.
///
/// One read rather than one per face, for two reasons that are the same reason:
/// [`enabled_protocols`] is where the 「Skipping unknown protocol」 sentence lives,
/// so calling it per face says that sentence once per door on every run that carries
/// a stale entry (AGENTS.md §4.10's "the same thing said twice is the log's whole
/// growth"); and a settings tab can rewrite the file between two reads, which would
/// let 8009 and 7000 be opened by **different answers to the same question**.
pub struct Faces {
    /// 8009, the 8008/8443 setup face and the `_googlecast._tcp` announcement: one
    /// protocol, so a run that gated 8009 but not 8008 would answer `eureka_info`
    /// about a device that cannot be cast to, and one that gated both but not the
    /// announcement would put the device in every phone's list while refusing the
    /// connection.
    pub cast: bool,
    /// The AirPlay RTSP receiver on 7000. Separate key in the user's list, so a
    /// "Chromecast off" run must not keep answering iPhones on the old port.
    pub airplay: bool,
}

/// The **one** title this shell's renderer answers to: the stored
/// `Macast_Renderer`, or `MPV` when the key is empty (a fresh install).
///
/// One owner, because three readers ask the same question — the settings page's device
/// dropdown (`_local_target`'s row name), `set-renderer`'s match, and anything that
/// reports "what is driving the player". The reference derives it from its plugin list;
/// this shell has one built-in renderer, so the list is this name and the switch below
/// accepts exactly it.
pub fn renderer_title(setting: &Setting) -> String {
    let stored = setting.get_str(Key::MacastRenderer, "");
    let trimmed = stored.trim();
    if trimmed.is_empty() {
        "MPV".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn faces_enabled(setting: &Setting, log: &Logger) -> Faces {
    let titles = enabled_protocols(setting, log);
    Faces {
        cast: titles.iter().any(|title| title == CAST_TITLE),
        airplay: titles.iter().any(|title| title == AIRPLAY_TITLE),
    }
}

/// `MacastPluginManager.set_protocol_enabled` (`macast/macast.py:429-453`) — the
/// **write** side of [`enabled_protocols`], and the only place `Macast_Protocols`
/// is ever rewritten.
///
/// Four rules, each one a line of the reference rather than a preference:
///
/// * an **unknown** title is dropped from the list rather than refused, and
///   `Ok(true)` says "the list is what it should be now". A plugin the user
///   removed must not stay persisted forever; the reference's comment says the
///   same and its answer is still a success. The one difference is the warning:
///   this port says which name it dropped, because the caller's message is going
///   to claim the protocol was enabled and a reader of the log is the only one
///   who can tell them otherwise.
/// * enabling something already present is a no-op that still **succeeds**
///   (`Ok(true)`, list unchanged). The reference has the same shape, and the
///   caller's toggle (`title not in enabled` first) is what normally prevents it —
///   this function must not treat "already on" as an error, or a case-insensitive
///   toggle (`chromecast`) would answer code 1 for a request that changed nothing
///   because nothing needed changing.
/// * disabling something absent is `Ok(false)`: the caller can tell "I did
///   nothing" from "I removed it" without comparing lists, which is exactly the
///   distinction the reference's bare `return False` encodes.
/// * disabling the **last** enabled protocol is refused with the reference's own
///   sentence (`至少要保留一个启用的协议`). With every protocol off the player
///   stays up and nothing can reach it, which a user reads as "Macast is broken".
///
/// The stored order is [`available_titles`]'s order, because the reference sorts by
/// its plugin list (`self.protocol_list`): a list built by clicking switches must
/// not make the next read see a different order than a list that was never touched.
pub fn set_protocol_enabled(
    setting: &Setting,
    log: &Logger,
    title: &str,
    enabled: bool,
) -> Result<bool, String> {
    let available = available_titles();
    let mut titles = enabled_protocols(setting, log);
    match resolve_title(title, &available) {
        None => {
            titles.retain(|held| held != title);
            log.warning(&format!(
                "Skipping unknown protocol: {} (nothing by that name is stored now)",
                title
            ));
        }
        Some(real) => {
            if enabled {
                if !titles.contains(&real) {
                    titles.push(real);
                }
            } else {
                if !titles.contains(&real) {
                    return Ok(false);
                }
                if titles.len() <= 1 {
                    return Err("至少要保留一个启用的协议".to_string());
                }
                titles.retain(|held| *held != real);
            }
        }
    }
    titles.sort_by_key(|held| {
        available
            .iter()
            .position(|known| known == held)
            .unwrap_or(available.len())
    });
    setting.set(Key::MacastProtocols, json!(titles));
    Ok(true)
}

#[cfg(test)]
mod tests {
    //! The shell's own tests, the first ones in this crate.
    //!
    //! Two rules are carried over from the library crates rather than invented
    //! here: every test pins the settings/log directory **as its first statement**
    //! (`AGENTS.md` §10 — nothing in a test may write into the user's real
    //! configuration, and a `Logger::named` from this crate would do exactly that),
    //! and the gate below is tested through the *reference's* rules, not through
    //! the spelling this file happens to use.

    use super::*;
    use macast_core::logging::Logger;
    // `super` is this module, so the crate root — where `main.rs` keeps the shell's own
    // boot helpers — is one level further up. These two are the first items from there
    // that a test of this crate reaches for: the switch's handle and the four-word
    // sentence it logs. `SharedHooks` comes from the crate `macast-upnp` re-exports it
    // from, because `main.rs`'s own import is not visible down here.
    use super::super::{announcement_change, Discovery};
    use macast_upnp::store::SharedHooks;

    /// The same mechanism as `macast-cast`'s `pin_test_dir`, with this crate's
    /// directory name: `macast-core` compiles without the `cfg(test)` of a
    /// dependent crate, so each crate that reaches for a `Logger` pins for
    /// itself.
    #[cfg(test)]
    fn pin_test_dir() {
        static PIN: std::sync::Once = std::sync::Once::new();
        PIN.call_once(|| {
            let dir = std::env::temp_dir().join(format!("macast-tauri-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
            macast_core::logging::remove_log_files(&dir);
            macast_core::paths::set_setting_dir_override(&dir);
        });
    }

    /// The one-read answer, for the rows that only care about a single door.
    fn faces_of(setting: &Setting) -> Faces {
        faces_enabled(setting, &logger())
    }

    fn setting(tag: &str) -> Arc<Setting> {
        Arc::new(Setting::new(
            macast_core::paths::setting_dir().join(format!("gate-{tag}.json")),
        ))
    }

    fn logger() -> Arc<Logger> {
        Logger::named("Macast")
    }

    /// The table's **own** destination, claimed, with the file it writes to: the
    /// history sentences belong to `logs/Protocol.log` (see [`PROTOCOL_LOG`]), and a
    /// test that read `macast.log` instead would be asserting against a file the
    /// product never writes them to — the exact mistake `main.rs` made until this
    /// round, when it handed the table the player's logger.
    fn claimed_protocol_logger() -> (Arc<Logger>, std::path::PathBuf) {
        let log = Logger::named(PROTOCOL_LOG);
        let path = log.claim();
        (log, path)
    }

    static DEBUG_GATE: Mutex<()> = Mutex::new(());

    /// The same guard `macast-cast`'s `DebugWindow` is, copied rather than shared
    /// because that one is a `pub(crate)` item of a different crate. The threshold
    /// is one process-wide atomic: a test that raises it must put it back, and the
    /// restore must not happen while another window is still open.
    struct DebugWindow {
        // Dropped *after* the `Drop` body below, which is the ordering the restore
        // depends on: set INFO back, then let the next waiter in.
        _gate: std::sync::MutexGuard<'static, ()>,
    }

    impl DebugWindow {
        fn armed() -> DebugWindow {
            let gate = DEBUG_GATE.lock().unwrap_or_else(|e| e.into_inner());
            macast_core::logging::set_threshold(macast_core::logging::Level::Debug);
            DebugWindow { _gate: gate }
        }
    }

    impl Drop for DebugWindow {
        fn drop(&mut self) {
            macast_core::logging::set_threshold(macast_core::logging::Level::Info);
        }
    }

    #[test]
    fn a_fresh_install_gets_every_face() {
        pin_test_dir();
        // `_load_enabled_protocols` ends with 「Nothing usable yet: a fresh install
        // gets every protocol at once」. A brand-new settings file must not be read
        // as "the user switched Chromecast off" — that shape would ship with no
        // Cast receiver at all and the sentence in the log would name a choice
        // nobody made.
        let enabled = enabled_protocols(&setting("fresh"), &logger());
        assert_eq!(enabled, vec![DLNA_TITLE, CAST_TITLE, AIRPLAY_TITLE]);
        let faces = faces_enabled(&setting("fresh"), &logger());
        assert!(faces.cast, "a fresh install opens the Cast faces");
        assert!(faces.airplay, "and the AirPlay one");
    }

    #[test]
    fn a_list_of_one_closes_the_other_two() {
        pin_test_dir();
        let dir = setting("dlna-only");
        dir.set(Key::MacastProtocols, json!(["DLNA Protocol"]));
        assert_eq!(enabled_protocols(&dir, &logger()), vec![DLNA_TITLE]);
        let faces = faces_of(&dir);
        assert!(!faces.cast, "8009, 8008/8443 and the announcement stay shut");
        assert!(
            !faces.airplay,
            "and so does 7000 — an iPhone must not be pointed at a port this run              refused to listen on"
        );
    }

    #[test]
    fn airplay_alone_opens_its_own_door_and_no_other() {
        pin_test_dir();
        // The two protocols are separate entries in the user's list, so the two
        // doors have to be separable: a run that keeps AirPlay and drops Chromecast
        // must still answer RTSP on 7000, and one that keeps Chromecast and drops
        // AirPlay must not be an iPhone target. A single `cast_enabled` shape could
        // not express that at all, which is why the gate answers with a struct.
        let dir = setting("airplay-only");
        dir.set(Key::MacastProtocols, json!(["DLNA Protocol", "AirPlay Protocol"]));
        let faces = faces_of(&dir);
        assert!(faces.airplay, "RTSP is asked for by name");
        assert!(!faces.cast, "and nothing else is opened with it");
        assert_eq!(
            enabled_protocols(&dir, &logger()),
            vec![DLNA_TITLE, AIRPLAY_TITLE]
        );
    }

    #[test]
    fn a_stored_string_is_one_entry_not_nothing() {
        pin_test_dir();
        // The reference coerces `isinstance(stored, str)` to a single entry. The
        // shared reader returns an empty list for that shape, which here would
        // mean "fresh install" — so a user who hand-edited the JSON to narrow the
        // run to DLNA would get every face opened instead. This is the one place
        // the gate may not use the convenient reader.
        let dir = setting("string");
        dir.set(Key::MacastProtocols, json!("DLNA Protocol"));
        assert_eq!(
            dir.get_str_list(Key::MacastProtocols),
            Vec::<String>::new(),
            "the shared reader's behaviour is the trap this test names"
        );
        assert_eq!(enabled_protocols(&dir, &logger()), vec![DLNA_TITLE]);
        assert!(!faces_of(&dir).cast);
    }

    #[test]
    fn the_legacy_key_is_appended_not_preferred() {
        pin_test_dir();
        let both = setting("legacy-both");
        both.set(Key::MacastProtocols, json!(["DLNA Protocol"]));
        both.set(Key::MacastProtocol, json!("Chromecast"));
        // Both are honoured, in the stored order, with the legacy entry last.
        assert_eq!(
            enabled_protocols(&both, &logger()),
            vec![DLNA_TITLE.to_string(), CAST_TITLE.to_string()]
        );
        // And a missing legacy key stays "nothing configured" rather than
        // becoming an empty stored entry: `has()` before `get()`, §4.2's rule.
        let one = setting("legacy-only");
        one.set(Key::MacastProtocol, json!("DLNA"));
        assert_eq!(enabled_protocols(&one, &logger()), vec![DLNA_TITLE]);
        assert!(!one.has(Key::MacastProtocols));
        // The other half of that rule, and the one a `get_str` refactor would
        // lose silently: **the read must not create the key it is gating on.**
        // `Setting::get` takes its default into the table and the next `save`
        // persists it, so an ungated `get_str(Macast_Protocol, "")` would write
        // `Macast_Protocol = ""` into the settings file of every install that
        // never had the legacy key — the side effect this repository is oldest
        // about, landing in a user's configuration on a plain app start.
        let untouched = setting("no-legacy");
        assert!(!untouched.has(Key::MacastProtocol));
        enabled_protocols(&untouched, &logger());
        assert!(
            !untouched.has(Key::MacastProtocol),
            "the gate read created Macast_Protocol in a settings file that had no such key"
        );
    }

    #[test]
    fn spellings_resolve_to_the_title_that_is_stored() {
        pin_test_dir();
        let available = available_titles();
        for (spelling, want) in [
            ("DLNA Protocol", DLNA_TITLE),
            ("DLNA", DLNA_TITLE),
            ("dlna protocol", DLNA_TITLE),
            ("Chromecast", CAST_TITLE),
            ("chromecast  PROTOCOL ", CAST_TITLE),
            ("AirPlay", AIRPLAY_TITLE),
            // Measured against CPython's own `canonical_title` before being
            // frozen: every one of these answers only because the whitespace
            // collapse runs before the `" protocol"` replace. Delete the collapse
            // and the first three stop resolving — that is the load-bearing half
            // of the three-step fold, and it is what a value pasted out of a JSON
            // editor or a log line can carry.
            ("Chromecast\tProtocol", CAST_TITLE),
            ("AirPlay\nProtocol", AIRPLAY_TITLE),
            ("Chromecast\r\nProtocol", CAST_TITLE),
            // U+00A0 is whitespace to both `str.split()` and `split_whitespace()`,
            // so a non-breaking space inside the word — the one a browser puts in
            // a copied label — behaves like any other run.
            ("DLNA\u{a0}Protocol", DLNA_TITLE),
            ("\u{a0}Chromecast\u{a0}Protocol", CAST_TITLE),
            ("\tDLNA ", DLNA_TITLE),
        ] {
            assert_eq!(
                resolve_title(spelling, &available).as_deref(),
                Some(want),
                "{:?} did not resolve to {:?}",
                spelling,
                want
            );
        }
        assert_eq!(resolve_title("Nirvana", &available), None);
        // The one spelling the reference does **not** tolerate, asserted rather
        // than fixed: `canonical_title` strips " protocol" with a space
        // (`macast.py:704`), so a class name written tight — what a log line or a
        // pasted Python repr looks like — resolves to nothing here and there.
        // "Fixing" it on one side only would make the two disagree about which
        // stored values mean something, which is worse than a shared, logged gap:
        // an unresolvable entry is skipped with a warning, and a list whose every
        // entry is unknown falls back to DLNA rather than listening on nothing.
        assert_eq!(resolve_title("ChromecastProtocol", &available), None);
        // The canonical fold itself, since every one of those matches rides on it.
        assert_eq!(canonical_title("Chromecast  Protocol "), "chromecast");
        assert_eq!(canonical_title("  DLNA   protocol  "), "dlna");
        assert_eq!(canonical_title("Nirvana Protocol"), "nirvana");
        assert_eq!(canonical_title("Chromecast\tProtocol"), "chromecast");
        assert_eq!(canonical_title("\u{a0}AirPlay\u{a0}protocol"), "airplay");
        // `replace` is replace-**all** in both languages, so a doubled word loses
        // both copies and still resolves. Frozen because the two sides could
        // plausibly disagree here (a first-occurrence-only fold would leave
        // "chromecastprotocol") and only a measurement says which is the rule.
        assert_eq!(canonical_title("Chromecast Protocol Protocol"), "chromecast");
        assert_eq!(resolve_title("Chromecast Protocol Protocol", &available).as_deref(),
                   Some(CAST_TITLE));
    }

    #[test]
    fn an_unknown_title_is_skipped_and_said_out_loud() {
        pin_test_dir();
        let log = Logger::named("Macast");
        let path = log.claim();
        let dir = setting("unknown");
        dir.set(
            Key::MacastProtocols,
            json!(["Nirvana Protocol", "Chromecast"]),
        );
        assert_eq!(enabled_protocols(&dir, &log), vec![CAST_TITLE]);
        let text = macast_core::logging::read_tail(&path, 200).text;
        assert!(
            text.contains("Skipping unknown protocol: Nirvana Protocol"),
            "the skip has to be readable, not silent: {}",
            text
        );
        // …and a list whose every entry is unknown leaves DLNA, because the
        // reference would rather listen on one protocol than on none.
        let all_unknown = setting("all-unknown");
        all_unknown.set(Key::MacastProtocols, json!(["Nirvana Protocol"]));
        assert_eq!(enabled_protocols(&all_unknown, &log), vec![DLNA_TITLE]);
        assert!(!faces_enabled(&all_unknown, &log).cast);
    }

    #[test]
    fn duplicates_collapse_and_the_stored_order_wins() {
        pin_test_dir();
        let dir = setting("dupe");
        dir.set(
            Key::MacastProtocols,
            json!(["Chromecast", "chromecast protocol", "DLNA"]),
        );
        // One entry per protocol, in first-seen order, each spelled canonically —
        // `resolve_title`'s promise that what gets persisted stays canonical.
        assert_eq!(
            enabled_protocols(&dir, &logger()),
            vec![CAST_TITLE.to_string(), DLNA_TITLE.to_string()]
        );
    }

    /// The gate is a door only if every face walks through it. `main.rs` has no
    /// compiler help for this: a fourth `SetupServer::start` added after the
    /// existing `if` would compile and would open a Cast face on a run that
    /// switched Chromecast off, which is the exact state `docs/Rust-Port-Design.md`
    /// §16 item 12 was written to close. So the audit is three questions about
    /// `main.rs` as text: the one read of the gate has to come **before** any of the
    /// three faces is opened, there has to be **exactly one** read (so no face can
    /// consult the rule at its own door at its own moment), and each face has to be
    /// **constructed exactly once per face** (so a second `SetupServer::start(` cannot
    /// hide behind the first occurrence that `find` looks at), where "face" means one
    /// announced service or one listening socket — and the announcer type, which
    /// serves two of those, is pinned at exactly two call sites.
    ///
    /// What each half is witnessed by, since they are not the same:
    ///
    /// * **one read, and one construction per face** each have their own mutants:
    ///   writing the predicate a second time at the setup face's door (red here, on
    ///   the count, and nowhere else — the run behaves identically either way, which
    ///   is exactly why the assertion is about the read count and not the outcome),
    ///   and constructing the announcer a second time in a closure that is never
    ///   called (red here, on the per-face count). Both witnesses were measured
    ///   2026-10-18 at 1 red each; the first attempt at the second one came back a
    ///   **compile error** instead (the setup-face version moved `setup_config`),
    ///   which is not a red and would have read as "no teeth" from an exit code.
    /// * **position** has no compilable same-type mutant. Moving the read below
    ///   `CastChannel::new` means deleting the two users that come before it (the
    ///   refusal sentence and the certificate's `if`), and that is a rewrite, not a
    ///   substitution — its red would say as much about the rewrite as about the
    ///   rule. What really witnesses position is §24 of
    ///   `scripts/verify_rust_receiver.sh`: delete the certificate's gate and the
    ///   socket, the announcement on the wire, and the key material all go red
    ///   together — 6 checks, measured 2026-10-18.
    ///
    /// Recorded as a limitation rather than written as coverage: the two assertions
    /// in this test are not equally armed.
    /// The shell's own startup summary belongs in **`macast.log`**, and this row is the
    /// witness: the reference's `server.py` / `protocol_cast.py` / `protocol_airplay.py`
    /// lines all propagate to the root handler (only **plugins** are claimed —
    /// `macast.py:336` / `:905`), while this port claims four core names as a recorded
    /// divergence (§8). Getting the routing wrong is not cosmetic: a reader following
    /// AGENTS.md §5 greps `macast.log`, and these lines used to ride the **player's**
    /// logger — measured, when a probe of mine looked in `macast.log` and found nothing
    /// while the sentence sat in `logs/MPV.log` (§16 第 29 条 ④).
    ///
    /// Two questions, because either alone is weak: the **text** says which logger each
    /// sentence names, and the **logger table** says where the root logger actually
    /// writes. A sentence routed to `root()` that the table had claimed somewhere else
    /// would pass the first and fail the meaning.
    #[test]
    fn the_shells_own_summary_speaks_through_the_root_log() {
        // First statement, by the crate's own audit (`every_test_in_this_crate_pins_the_
        // log_directory_first`): every test here writes through a logger, so the pin has
        // to happen before the first line that could touch a path.
        pin_test_dir();
        let main = include_str!("main.rs");
        assert_eq!(
            main.matches("let app_log = macast_core::logging::root();").count(),
            1,
            "the shell's summary logger is not defined exactly once"
        );
        for sentence in [
            "\"receiver: HTTP {} on port {}, SSDP {}\"",
            "\"Cast receiver: 8009 listening on {port}\"",
            "\"setup face: {text}\"",
            "\"renderer log: {}\"",
            "\"AirPlay RTSP: listening on {port} as",
            "\"AirPlay RTSP: not listening",
            "\"AirPlay is switched off (Macast_Protocols)",
        ] {
            let at = main.find(sentence).unwrap_or_else(|| panic!("sentence gone: {sentence}"));
            // Look **backwards** for the logger call that owns it: the call opens on the
            // line (or two) before the format string.
            let window = &main[at.saturating_sub(220)..at];
            assert!(
                window.contains("app_log."),
                "this sentence still rides another logger: {sentence}"
            );
        }
        // The player's own sentences stay in the player's file — the routing is a split,
        // not a migration.
        for sentence in ["\"mpv candidate: {} (socket {}, {} scripts dir {})\"", "\"no mpv found on PATH"] {
            let at = main.find(sentence).unwrap_or_else(|| panic!("sentence gone: {sentence}"));
            let window = &main[at.saturating_sub(220)..at];
            assert!(
                !window.contains("app_log."),
                "a player sentence moved into the shell's log: {sentence}"
            );
        }
        // …and **nothing in the product claims that name**. This used to assert
        // `!root.is_claimed()` and went red only when the whole suite ran: `Logger::named`
        // remembers by name, and another test in this file claims `"Macast"` for its own
        // purposes — so the assertion was reading the test scheduler, not the product
        // (§4.2's shared-fixture family, in its logger form). The product question is
        // whether `main.rs` claims the root name, and that is answerable from the source:
        assert!(
            !main.contains("logging::root().claim()")
                && !main.contains("app_log.claim()"),
            "the shell claims the root logger, which moves every summary sentence out of macast.log"
        );
        // The name itself, so a rename of `root()` cannot silently point at a different
        // file than the one AGENTS.md §5 tells a user to read.
        assert_eq!(macast_core::logging::root().name(), "Macast");
    }

    /// "Chromecast is switched off" has to mean *nothing on 8009, nothing on 8008,
    /// nothing on the wire*. Three structural facts keep that true, and each has its
    /// own mutation body: the gate is read **before** every face `run()` opens, it is
    /// read **exactly once** so two faces cannot disagree, and each face is
    /// **constructed at exactly one place** so a fourth cannot appear behind a needle.
    ///
    /// The positional question is asked of `run()`'s body, not of the file. Since the
    /// interface picker re-announces, the service table is built by a helper
    /// (`discovery_services`) that both boot and `Runtime::readvertise` call, so a
    /// needle's first occurrence in the file can now sit in a body that only ever runs
    /// after the gate was read — `find` over the whole file would then compare a
    /// helper's text against a call site and report an order the process never
    /// executes. What has to be true is the order `run()` actually runs in.
    #[test]
    fn every_cast_face_is_behind_the_gate() {
        pin_test_dir();
        let main = include_str!("main.rs");
        let body = &main[main.find("pub fn run()").expect("main.rs has run()")
            ..main.find("\nfn main()").expect("main.rs has main()")];
        let gate = body
            .find("host::faces_enabled(")
            .expect("run() reads the gate");
        assert_eq!(
            body.matches("host::faces_enabled(").count(),
            1,
            "run() reads the enabled list more than once, so two faces can be opened \
             by two different snapshots of a file the page can rewrite"
        );
        // Since the protocol switch landed, boot no longer constructs the receivers
        // itself: `run()` opens them through `Discovery`, which is the **one**
        // constructor the switch also calls. So the positional rule follows the calls
        // the process makes — the gate must be read before each of them — and the
        // construction points are counted separately below.
        for (needle, what) in [
            ("discovery.start_cast(", "the 8009 receiver and its setup faces"),
            ("discovery.start_airplay(", "the AirPlay receiver"),
            // The announcement, audited as a *call*: run() must not build a service
            // table or start a socket before it knows which doors are open.
            ("discovery.restart(", "the mDNS announcement"),
        ] {
            let at = body.find(needle).unwrap_or_else(|| panic!("no {} in run()", what));
            assert!(
                gate < at,
                "{} is opened before the gate is read",
                what
            );
        }
        // The switch path has the same rule: `apply_protocols` must ask the table
        // **before** it opens or closes anything, or a toggle would apply a list the
        // gate is about to disagree with.
        let apply_at = main
            .find("pub fn apply_protocols(")
            .expect("Discovery::apply_protocols exists");
        let apply_body = &main[apply_at..apply_at + 1200];
        let apply_gate = apply_body
            .find("host::faces_enabled(")
            .expect("apply_protocols reads the gate");
        for (needle, what) in [
            ("self.start_cast(", "the 8009 receiver"),
            ("self.stop_cast(", "closing 8009"),
            ("self.start_airplay(", "the AirPlay receiver"),
            ("self.stop_airplay(", "closing AirPlay"),
        ] {
            let at = apply_body
                .find(needle)
                .unwrap_or_else(|| panic!("no {} in apply_protocols", what));
            assert!(
                apply_gate < at,
                "{} happens before the switch reads the list",
                what
            );
        }
        // **Exactly one call site per face.** `find` above only ever looks at
        // the first, so a second `SetupServer::start(` written somewhere else
        // in `main.rs` — the fourth face this audit exists to catch — would sit
        // behind the needle and never be looked at. The `(` is part of the
        // needle so a doc comment naming the type cannot count as a call.
        for (needle, what) in [
            ("CastChannel::new", "the 8009 receiver"),
            ("setup::SetupServer::start", "the setup face"),
            ("airplay::AirPlayReceiver::new", "the AirPlay receiver"),
            // Each door's service is built in one place, so a face cannot be
            // announced by an inline table someone assembled at an ungated call
            // site. Two announcers used to be one per door (2026-10-18); the
            // socket merge made that wrong on two kernels, so the count that
            // matters is now the *services* — one per door — plus one engine.
            ("announce::Service::from_setting", "the Chromecast announcement"),
            (
                "announce::Service::airplay_from_setting",
                "the AirPlay announcement",
            ),
        ] {
            let calls = main.matches(&format!("{needle}(")).count();
            assert_eq!(
                calls, 1,
                "{what} is constructed {calls} times in main.rs; a second call site \
                 would be behind the gate only by accident"
            );
        }
        // **One** announcer, because one socket owns every service type (§16 第 32 条):
        // the per-service shape cost unicast queriers a coin flip on which socket
        // received their query, measured on two kernels. And one owner of each half of
        // the announcement, so the two paths that may re-advertise (boot and a pin
        // change) cannot drift into two different tables or two different lifecycles:
        assert_eq!(
            main.matches("announce::Announcer::new(").count(),
            1,
            "the announcer is constructed at more than one site; one socket owns every service"
        );
        assert_eq!(
            main.matches("fn discovery_services(").count(),
            1,
            "the service table has more than one builder, so boot and the picker can disagree"
        );
        assert_eq!(
            main.matches("fn start_announcement(").count(),
            1,
            "the announcement has more than one starter, so only one of them can be gated"
        );
        // Three callers of one restart, and only three: boot's first start, the
        // interface switch, and the protocol switch. A fourth is a face being announced
        // by a path that never went through the enabled list.
        assert_eq!(
            main.matches(".restart(").count(),
            3,
            "the announcer is restarted from {} sites; boot, the interface switch and the \
             protocol switch are the three, and a fourth would be gated only by accident",
            main.matches(".restart(").count()
        );
        // **One read per path**, which the two bodies above already pin: `run()` reads it
        // once and `apply_protocols` reads it once. The file-wide count is therefore two,
        // and a third would be a path that decided for itself what is enabled — the
        // failure `host::Faces`'s doc describes as two faces answering one question
        // differently.
        assert_eq!(
            main.matches("host::faces_enabled(").count(),
            2,
            "the gate is read from a third place, so some path can disagree with the others"
        );
        // The interface switch's two halves have one home each, for the same reason. The
        // validation is the half a second copy would silently widen: a call site that
        // stored a card without asking `net::pin_refusal` first is the typo-as-a-pin
        // failure §32 measures from the other side, and it would not make any single test
        // red — only the count would.
        assert_eq!(
            main.matches("net::pin_refusal(").count(),
            1,
            "the picker validates a card name in more than one place"
        );
        assert_eq!(
            main.matches("set(Key::NetworkInterface").count(),
            1,
            "the pin is written from more than one place, so a refusal can be bypassed"
        );
    }

    /// Which half of a re-advertisement moved is a four-state fact, and only this pair
    /// can tell the states apart — the picker answers `code: 0` in all four.
    ///
    /// Asserted against **literals**: comparing the arms with each other (or with
    /// `announcement_change`'s own constants) is a same-source equality that holds for
    /// any wording, so a rewrite that made two states say the same thing would not move
    /// a line (§13.4's rule, met again in my own test). The middle one is the row that
    /// has to exist: **goodbye sent, nothing came back up** is otherwise read exactly
    /// like "there was nothing to announce", and the first sends the reader to the bind
    /// failure while the second sends them to 系统设置.
    #[test]
    fn announcement_change_names_which_half_of_the_switch_moved() {
        pin_test_dir();
        assert_eq!(announcement_change(true, true), "re-announced");
        assert_eq!(announcement_change(false, true), "now announcing");
        assert_eq!(
            announcement_change(true, false),
            "stopped announcing (see this file for why it could not start again)"
        );
        assert_eq!(
            announcement_change(false, false),
            "has nothing to announce (no receiver is listening)"
        );
    }

    /// A `Hooks` that answers nothing, so a test can build an `SsdpPlugin` without a
    /// running app. It exists for the same reason `macast-upnp`'s own null one does: an
    /// accidental `stop_service()` from a bind failure should read as silence here, not
    /// as a real service being torn down.
    struct SilentHooks;

    impl macast_upnp::store::Hooks for SilentHooks {
        fn notify(&self, _title: &str, _text: &str) {}
        fn stop_service(&self) {}
    }

    /// A plugin that never started: `SsdpPlugin::new` registers devices and logs, and
    /// `ssdp().is_running()` stays `false` until `start()` — which is exactly the state
    /// the switch's answer has to describe.
    fn ssdp_never_started(setting: Arc<Setting>) -> Arc<macast_upnp::device::SsdpPlugin> {
        macast_upnp::device::SsdpPlugin::new(Arc::new(SilentHooks) as SharedHooks, setting)
    }

    /// The reference's refusal (`protocol.py:1986-1990`), and the one thing that makes it
    /// a refusal rather than a warning: **nothing is written**. A typo stored as a pin is
    /// worse than no pin, because `resolved_network_interface` then falls back to
    /// automatic selection *silently* — the settings page would say the card is pinned
    /// while the announcer publishes whatever the default route picks. So the test asks
    /// for the key to be absent afterwards, not merely for a `code: 1`.
    #[test]
    fn the_interface_switch_refuses_a_card_this_machine_does_not_have() {
        pin_test_dir();
        let setting = setting("iface-refusal");
        let discovery = Discovery::new(setting.clone(), ssdp_never_started(setting.clone()), logger());

        let answer = discovery.set_interface("  nope9x  ");
        assert_eq!(answer.code, 1, "an unknown card must not be stored");
        assert!(
            answer.message.contains("nope9x"),
            "the refusal has to name the typo the user typed: {}",
            answer.message
        );
        assert!(
            answer.message.contains("当前可用"),
            "…and say what was available, or the reader cannot fix it: {}",
            answer.message
        );
        assert!(
            answer.fields.is_empty(),
            "a refusal is the reference's two-key dict, not a partial success document: {:?}",
            answer.fields
        );
        // **Nothing reaches the disk.** The in-memory table is a different story and has
        // to be told apart from it: reading the known-interface table above goes through
        // `Setting::get_str(NetworkInterface, "")`, and `get` with a default *remembers*
        // that default in the table — AGENTS.md §4.2's first trap, and the reference has
        // it identically (`Setting.get(SettingProperty.Network_Interface, '')`). So the
        // claim a refusal has to satisfy is that no `save()` ran: the file a user would
        // copy into a bug report still has whatever it had before, which for a fresh
        // settings path means "does not exist at all". Asserting `!has()` here would be
        // asserting something the reference also fails, and would push the next person to
        // route the read through `peek` — a divergence from Python made to please a test.
        let path = macast_core::paths::setting_dir().join("iface-refusal.json");
        assert!(
            !path.exists(),
            "the refusal wrote the settings file: {}",
            std::fs::read_to_string(&path).unwrap_or_default()
        );
    }

    /// The switch's own document, in the reference's key order, plus the two keys this
    /// port adds because its call is direct. Asked on a machine where **nothing is
    /// listening**: SSDP never started, and no receiver slot is filled, so both halves
    /// answer `false` — and the sentence in `logs/Discovery.log` has to be the one that
    /// means "there was nothing to say" rather than the one that means "a bind failed".
    ///
    /// The two `false`s are not decoration. A port that reported `mdns_reannounced: true`
    /// because it called `start_announcement` and ignored the answer would be printing
    /// the ask instead of the result — the shape AGENTS.md §4.2 keeps meeting ("别把端口在听
    /// 当证据" is the same rule read from the other side).
    #[test]
    fn the_interface_switch_answers_which_halves_moved_when_nothing_is_listening() {
        pin_test_dir();
        let setting = setting("iface-answer");
        let discovery = Discovery::new(setting.clone(), ssdp_never_started(setting.clone()), logger());
        let log = Logger::named(macast_cast::announce::LOGGER_NAME);
        let path = log.claim();

        // A name taken from **this machine's** table, not typed in: the row above proves
        // the refusal path, and hard-coding "en0" here would make this test a red herring
        // on every machine whose Wi-Fi card is numbered differently — this one answers
        // `en7,bridge100,bridge101,bridge102,lo0,utun4`. Same rule as §4.8's Part 40: a
        // judgment may not be decided by the machine that happens to run it.
        let rows = net::network_interfaces(&setting);
        assert!(
            !rows.is_empty(),
            "this machine reports no interfaces at all, so the switch has nothing to pin"
        );
        let card = rows[0].name.clone();

        let pinned = discovery.set_interface(&card);
        assert_eq!(pinned.code, 0, "a card off the machine's own table: {pinned:?}");
        assert_eq!(
            pinned.message,
            format!("已切换到 {card}"),
            "the reference's own sentence"
        );
        assert_eq!(
            pinned
                .fields
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["interface", "ssdp_reannounced", "mdns_reannounced"],
            "the reference's three keys first, then the two this port adds — in that order"
        );
        assert_eq!(pinned.fields[0].1, json!(card));
        assert_eq!(
            pinned.fields[1].1,
            json!(false),
            "SSDP never started, so the switch did not rebind it"
        );
        assert_eq!(
            pinned.fields[2].1,
            json!(false),
            "no receiver is listening, so there was nothing to announce"
        );
        assert_eq!(
            setting.get_str(Key::NetworkInterface, ""),
            card,
            "a refused pin must not be stored — and an accepted one has to be"
        );

        // **Trimmed before the known-set lookup**, because that is the reference's order
        // (`protocol.py:1985`, `name = (name or '').strip()` *then* the membership test):
        // `" en0 "` is a pin there, and a port that trimmed only after validating would
        // refuse a card the user can see in the picker. The stored value is the trimmed
        // one, so the next reader does not have to strip to compare anything.
        let padded = discovery.set_interface(&format!("  {card}  "));
        assert_eq!(
            padded.code,
            0,
            "a padded card name was refused; the reference strips before it looks up: {padded:?}"
        );
        assert_eq!(padded.fields[0].1, json!(card), "the stored pin kept the spaces");

        // Clearing the pin is a request, not a missing argument: the reference stores
        // the **empty string** (`utils.py:331-334`), it does not unset the key, and
        // `get_network_interface` reads `''` as automatic.
        let cleared = discovery.set_interface("");
        assert_eq!(cleared.code, 0);
        assert_eq!(cleared.message, "已切换到 自动选择");
        assert_eq!(cleared.fields[0].1, json!(""));
        assert_eq!(setting.get_str(Key::NetworkInterface, "?"), "");

        // The two sentences, in the file a reader of 「为什么手机找不到这台 Mac」 opens.
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            text.contains(&format!(
                "network interface changed -> {card}; mDNS has nothing to announce"
            )),
            "the pin change did not say which half moved: {text}"
        );
        assert!(
            text.contains("network interface changed -> auto; mDNS has nothing to announce"),
            "clearing the pin is not reported as a pin: {text}"
        );
        assert!(
            !text.contains("stopped announcing"),
            "nothing was announced, so a goodbye that never happened is being claimed: {text}"
        );
    }

    /// The play history is what `query=status` prints and what the 播放历史 card
    /// offers back, so the four rules the reference applies at write time live
    /// here — the document's reader is tested in `macast-upnp`, and this is the
    /// owner of the rows.
    #[test]
    fn the_play_history_follows_the_reference_s_write_rules() {
        pin_test_dir();
        let table = StateTable::new(logger());
        // Newest first.
        table.add_history("第一部", "http://h/a.mp4");
        table.add_history("第二部", "http://h/b.mp4");
        assert_eq!(
            table
                .play_history()
                .iter()
                .map(|(title, _, _)| title.as_str())
                .collect::<Vec<_>>(),
            ["第二部", "第一部"]
        );
        // Re-casting something already in the list **moves** it to the front rather
        // than growing a second row — `protocol.py:1207-1208` filters by `uri` first.
        table.add_history("第一部again", "http://h/a.mp4");
        let rows = table.play_history();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "http://h/a.mp4");
        assert_eq!(rows[0].0, "第一部again");
        // An empty title stores the URI, because a blank row is not recognisable
        // later (`'title': title or uri`).
        table.add_history("", "http://h/nameless.mkv");
        assert_eq!(table.play_history()[0].0, "http://h/nameless.mkv");
        // The third field is a wall-clock second, not a counter: the page prints it.
        let stamped = table.play_history()[0].2;
        assert!(
            stamped > 1_700_000_000,
            "the row carries no real time: {stamped}"
        );
        // Capped at 50 so the list the page renders cannot grow without bound.
        for n in 0..60 {
            table.add_history("t", &format!("http://h/{n}.mp4"));
        }
        assert_eq!(table.play_history().len(), 50);
        assert_eq!(table.play_history()[0].1, "http://h/59.mp4");
        // And clearing is the reference's `clear_play_history`: the rows go, the
        // list stays a list.
        table.clear_history();
        assert!(table.play_history().is_empty());
    }

    /// Same discipline as the library crates: a test added without the pin would
    /// write into the user's configuration directory, so the count is checked
    /// rather than trusted.
    #[test]
    fn every_test_in_this_crate_pins_the_log_directory_first() {
        pin_test_dir();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scanned = 0usize;
        let mut files = std::fs::read_dir(&root)
            .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect::<Vec<_>>();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if line.trim() != "#[test]" {
                    continue;
                }
                let mut j = index + 1;
                while j < lines.len() && lines[j].trim().starts_with("#[") {
                    j += 1;
                }
                let signature = lines.get(j).unwrap_or(&"");
                assert!(
                    signature.trim_start().starts_with("fn ") && signature.contains('{'),
                    "{}:{}: no test signature follows the attribute",
                    path.display(),
                    index + 1
                );
                scanned += 1;
                let mut k = j + 1;
                while k < lines.len() {
                    let body = lines[k].trim();
                    if !body.is_empty() && !body.starts_with("//") && !body.starts_with("///") {
                        break;
                    }
                    k += 1;
                }
                let first = lines.get(k).unwrap_or(&"").trim();
                assert_eq!(
                    first,
                    "pin_test_dir();",
                    "{}:{}: this test does not pin its log directory first",
                    path.display(),
                    k + 1
                );
            }
        }
        assert!(
            scanned >= 10,
            "only {} tests scanned in this crate — an empty scan is not a pass",
            scanned
        );
    }

    // ── the play history, persisted ──────────────────────────────────

    /// The rows, read from the settings file the way a restarted app reads it:
    /// a **second** `Setting` instance over the same path. Nothing in this file
    /// can fake that with one instance, which is the whole point — the divergence
    /// this closes was "the table remembers, the file does not", and a test that
    /// asked the same in-memory table twice would be true of the old code too.
    #[test]
    fn a_history_row_survives_a_second_setting_instance() {
        pin_test_dir();
        let (log, path) = claimed_protocol_logger();
        let file = macast_core::paths::setting_dir().join("hist-restart.json");
        let first = StateTable::new(log.clone()).with_setting(Arc::new(Setting::new(file.clone())));
        first.add_history("Rick Astley", "http://192.168.1.5:58999/relay/abc/media");
        assert_eq!(first.play_history().len(), 1, "and the live table shows it");
        drop(first);

        let again = StateTable::new(log).with_setting(Arc::new(Setting::new(file)));
        let rows = again.play_history();
        assert_eq!(
            rows.len(),
            1,
            "a restart emptied the card, which is the divergence this row exists to catch"
        );
        assert_eq!(rows[0].0, "Rick Astley", "the title comes back with the row");
        assert_eq!(rows[0].1, "http://192.168.1.5:58999/relay/abc/media");
        // The line the acceptance script greps for (§27), and the only sentence in
        // this feature that says which kind of empty it was *not*.
        let text = macast_core::logging::read_tail(&path, 200).text;
        assert!(
            text.contains("play history: 1 row(s) read back"),
            "the restart restored a row and said nothing about it: {text}"
        );
    }

    #[test]
    fn the_persisted_rows_carry_the_reference_s_three_keys() {
        pin_test_dir();
        let path = macast_core::paths::setting_dir().join("hist-keys.json");
        let table = StateTable::new(logger()).with_setting(Arc::new(Setting::new(path.clone())));
        table.add_history("clip", "file:///tmp/clip.mp4");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| format!("no file: {e}"));
        let doc: Value = serde_json::from_str(&text).expect("the settings file is JSON");
        let rows = doc
            .get(Key::PlayHistory.name())
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("no {} list in the file: {text}", Key::PlayHistory.name()))
            .clone();
        assert_eq!(rows.len(), 1);
        let row = rows[0].as_object().expect("a history row is an object");
        let mut keys: Vec<&str> = row.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["time", "title", "uri"],
            "the reference writes exactly these three (`protocol.py:1211-1215`); a fourth \
             name or a missing one is a second spelling for one fact"
        );
        assert_eq!(row["uri"], json!("file:///tmp/clip.mp4"));
        assert_eq!(row["title"], json!("clip"));
        assert!(
            !text.contains("\"url\":"),
            "the window command used to say `url`; nothing may write that key into the \
             user's file again: {text}"
        );
    }

    /// An unreadable entry is dropped **and counted**. The reference filters junk
    /// only on its next write and `query=status` echoes whatever is in the file,
    /// so this is a recorded divergence — the count is what keeps it from being a
    /// silent one (§4.14's 「空结果要说清是哪一种空」).
    #[test]
    fn junk_in_the_history_list_is_dropped_and_said() {
        pin_test_dir();
        let (log, path) = claimed_protocol_logger();
        let file = macast_core::paths::setting_dir().join("hist-junk.json");
        std::fs::write(
            &file,
            r#"{"Play_History": [{"title": "no uri at all"}, 7, {"uri": "http://ok/a.mp4", "title": "ok"}]}"#,
        )
        .expect("write the fixture");
        let table = StateTable::new(log).with_setting(Arc::new(Setting::new(file)));
        let rows = table.play_history();
        assert_eq!(rows.len(), 1, "only the entry with a URI is a row: {rows:?}");
        assert_eq!(rows[0].2, 0, "a row with no `time` reads as 0, not as a dropped row");
        let text = macast_core::logging::read_tail(&path, 200).text;
        assert!(
            text.contains("could not be read as a row"),
            "two entries were thrown away and the log says nothing about it: {text}"
        );
    }

    /// The read must not **create** the key. `Setting::get` takes its default into
    /// the table — §4.2's oldest trap, and this is the same shape the protocol gate
    /// was caught on: a cold start would otherwise write `Play_History: []` into a
    /// settings file that never had one.
    #[test]
    fn reading_the_history_leaves_the_key_absent_on_a_first_run() {
        pin_test_dir();
        let setting = Arc::new(Setting::new(
            macast_core::paths::setting_dir().join("hist-first-run.json"),
        ));
        assert!(!setting.has(Key::PlayHistory), "the fixture starts without it");
        let table = StateTable::new(logger()).with_setting(Arc::clone(&setting));
        assert_eq!(table.play_history(), Vec::<(String, String, i64)>::new());
        assert!(
            !setting.has(Key::PlayHistory),
            "a boot read fabricated the key — the user's file now says \"cleared\" about \
             a machine that has never cast anything"
        );
    }

    /// 「清空」 has to reach the file too, and it has to land as an **empty list**,
    /// not as a missing key: the two are the same to the page and different to the
    /// next boot's log line.
    #[test]
    fn a_cleared_history_writes_an_empty_list_rather_than_erasing_the_key() {
        pin_test_dir();
        let path = macast_core::paths::setting_dir().join("hist-clear.json");
        let setting = Arc::new(Setting::new(path.clone()));
        let table = StateTable::new(logger()).with_setting(Arc::clone(&setting));
        table.add_history("one", "http://a/1.mp4");
        table.clear_history();
        assert!(setting.has(Key::PlayHistory), "the key stays");
        assert_eq!(
            setting.peek(Key::PlayHistory),
            Some(Value::Array(Vec::new())),
            "and it says \"cleared\", which is not the same fact as \"absent\""
        );
        let again = StateTable::new(logger()).with_setting(Arc::new(Setting::new(path)));
        assert!(again.play_history().is_empty(), "the restart agrees it is empty");
    }

    /// The 50-row cap is a promise **about the file** (`protocol.py:1215` says the
    /// cap exists so the settings file stays small), so the count is taken off the
    /// disk rather than off the table that just wrote it.
    #[test]
    fn the_cap_is_honoured_in_the_file_not_only_in_memory() {
        pin_test_dir();
        let path = macast_core::paths::setting_dir().join("hist-cap.json");
        let table = StateTable::new(logger()).with_setting(Arc::new(Setting::new(path.clone())));
        for n in 0..55 {
            table.add_history("", &format!("http://a/{n}.mp4"));
        }
        let text = std::fs::read_to_string(&path).expect("the file exists");
        let doc: Value = serde_json::from_str(&text).unwrap();
        let rows = doc[Key::PlayHistory.name()].as_array().unwrap();
        assert_eq!(rows.len(), 50, "the file grew past the cap the reference caps it at");
        assert_eq!(
            table.play_history().len(),
            50,
            "the table and the file must not disagree about how many rows there are"
        );
    }

    /// A table built without a settings file — the shape every pre-`with_setting`
    /// caller in this crate uses — says so instead of letting the rows quietly
    /// stop outliving the process.
    #[test]
    fn a_table_without_a_settings_file_says_the_rows_end_with_it() {
        pin_test_dir();
        let (log, path) = claimed_protocol_logger();
        {
            // The window is what makes this a question at all: at the default
            // threshold an empty file proves only that INFO filtered the line, and
            // reading that as "the sentence was never written" would put the change
            // in the wrong place (§14's 「一个空的见证文件证明的是被过滤了」).
            let _debug = DebugWindow::armed();
            let table = StateTable::new(log);
            table.add_history("lost", "http://a/lost.mp4");
            assert_eq!(table.play_history().len(), 1, "memory still works");
        }
        let text = macast_core::logging::read_tail(&path, 200).text;
        assert!(
            text.contains("without a settings file"),
            "the degradation is real, so it must be readable: {text}"
        );
    }

    /// The renderer's name has **one** owner and it is the one the dropdown shows: a
    /// fresh install answers `MPV`, a stored name is honoured, and whitespace is not a
    /// different renderer.
    #[test]
    fn the_renderer_title_is_the_one_the_dropdown_shows() {
        pin_test_dir();
        let dir = setting("renderer-title");
        assert_eq!(renderer_title(&dir), "MPV", "a fresh install's default");
        dir.set(Key::MacastRenderer, json!("  IINA  "));
        assert_eq!(renderer_title(&dir), "IINA", "the stored name, trimmed");
        dir.set(Key::MacastRenderer, json!(""));
        assert_eq!(renderer_title(&dir), "MPV", "empty falls back, as the dropdown does");
    }

    // ---- the write side of the protocol table (toggle-protocol) ----------------

    /// The four rules of `set_protocol_enabled`, each asked as its own question:
    /// disable removes, enable adds back **in the canonical order**, disabling
    /// something absent answers "nothing happened", and the last one is refused
    /// with the reference's sentence. Asked on the **file**, not on `enabled_protocols`
    /// alone: `Setting::set` is where a missing persist would hide (the key would
    /// read right in memory and be gone after a restart).
    #[test]
    fn toggling_a_protocol_rewrites_the_list_and_keeps_the_canonical_order() {
        pin_test_dir();
        let dir = setting("toggle");
        let log = logger();
        let all: Vec<String> = available_titles().iter().map(|t| (*t).to_string()).collect();
        assert_eq!(enabled_protocols(&dir, &log), all, "a fresh file is all three");

        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, false),
            Ok(true),
            "disabling a protocol that is on must report a change"
        );
        assert_eq!(
            enabled_protocols(&dir, &log),
            vec![DLNA_TITLE.to_string(), AIRPLAY_TITLE.to_string()],
            "the canonical order survives a removal"
        );
        let on_disk = dir.get_str_list(Key::MacastProtocols);
        assert_eq!(
            on_disk,
            vec![DLNA_TITLE.to_string(), AIRPLAY_TITLE.to_string()],
            "the removal reached the file, not just the table"
        );

        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, true),
            Ok(true),
            "enabling it again is a change"
        );
        assert_eq!(
            enabled_protocols(&dir, &log),
            all,
            "an added protocol is sorted back into the canonical position, not appended"
        );

        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, true),
            Ok(true),
            "already on is a success that changes nothing — the reference's toggle relies on it"
        );
        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, false),
            Ok(true),
            "and off still works afterwards"
        );
        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, false),
            Ok(false),
            "disabling something absent says so instead of inventing a change"
        );
    }

    /// The last protocol may not be switched off, and the refusal is the
    /// reference's own sentence — the page renders it verbatim.
    #[test]
    fn the_last_enabled_protocol_cannot_be_switched_off() {
        pin_test_dir();
        let dir = setting("last");
        let log = logger();
        dir.set(Key::MacastProtocols, json!([CAST_TITLE]));
        assert_eq!(
            set_protocol_enabled(&dir, &log, CAST_TITLE, false),
            Err("至少要保留一个启用的协议".to_string())
        );
        assert_eq!(
            dir.get_str_list(Key::MacastProtocols),
            vec![CAST_TITLE.to_string()],
            "a refused toggle must not write the file"
        );
    }

    /// An unknown title is **dropped**, and the answer is the reference's success:
    /// a removed plugin must not stay persisted, and the caller's message says
    /// "enabled" — which is why the warning has to name what did not happen.
    #[test]
    fn an_unknown_protocol_is_dropped_with_a_warning_and_still_succeeds() {
        pin_test_dir();
        let dir = setting("unknown");
        // The warning goes wherever the caller's logger points; this test gives it a
        // file of its own so the assertion reads the sentence instead of a neighbour's.
        let log = Logger::named(PROTOCOL_LOG);
        let path = log.claim();
        dir.set(
            Key::MacastProtocols,
            json!([DLNA_TITLE, "Ghost Protocol", CAST_TITLE]),
        );
        assert_eq!(set_protocol_enabled(&dir, &log, "Ghost Protocol", true), Ok(true));
        assert_eq!(
            enabled_protocols(&dir, &log),
            vec![DLNA_TITLE.to_string(), CAST_TITLE.to_string()],
            "the ghost is gone from the file"
        );
        let text = macast_core::logging::read_tail(&path, 200).text;
        assert!(
            text.contains("Ghost Protocol"),
            "the drop has to be readable, because the page is about to say 'enabled': {text}"
        );
    }
}
