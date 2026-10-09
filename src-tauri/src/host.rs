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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use macast_core::logging::{Level, Logger};
use macast_core::paths;
use macast_core::setting::{Key, Setting};
use macast_mpv::argv;
use macast_mpv::player::{Host, Player, StateCall};
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

/// The state table one DLNA receiver owns — `DLNAProtocol.state_list`, minus the
/// XML machinery: values are what a client would read back, and the playlist is
/// the list `cast_uri` appends to.
pub struct StateTable {
    values: Mutex<BTreeMap<String, Value>>,
    playlist: Mutex<Vec<String>>,
    log: Arc<Logger>,
    playlist_index: Mutex<i64>,
    history: Mutex<Vec<(String, String)>>,
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
    pub fn add_history(&self, title: &str, uri: &str) {
        let mut history = lock(&self.history);
        history.retain(|(_, item)| item != uri);
        let name = if title.is_empty() { uri } else { title };
        history.insert(0, (name.to_string(), uri.to_string()));
        if history.len() > 50 {
            history.truncate(50);
        }
    }

    pub fn play_history(&self) -> Vec<(String, String)> {
        lock(&self.history).clone()
    }

    pub fn clear_history(&self) {
        lock(&self.history).clear();
    }
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
}

impl Clone for TauriHost {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
            setting: self.setting.clone(),
            table: self.table.clone(),
            log: self.log.clone(),
            service_stopped: self.service_stopped.clone(),
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

/// The [`macast_upnp::http::Management`] seam: the GET web-cast entry's hand on
/// the player.
///
/// A newtype for the same reason [`ShellRenderer`] is one — the wording belongs
/// to the HTTP face, not to [`Player`]. The trait answers with **sentences** and
/// [`cast_uri`] answers with a JSON object, so this is the one place that turns
/// "what happened" into the words a `curl` caller reads.
///
/// It runs **on an HTTP worker thread**, which is not a new situation for this
/// shell: every [`ShellRenderer`] method the SOAP layer calls arrives on a worker
/// thread too, and `cast_uri` touches only the state table and the player — no
/// AppKit, no `AppHandle`. The reference's GET entry is the same shape (it is a
/// CherryPy handler calling the same sequence), so no marshalling is added here.
///
/// `title` is handed over verbatim, including `None`: [`cast_uri`] falls back to
/// [`title_for_uri`] when the caller said nothing, which is §4.2's
/// 「改播放器的地址，就必须改播放器的名字」 rather than a default this struct gets
/// to invent.
pub struct ShellManagement {
    pub host: TauriHost,
    pub player: Player,
}

impl macast_upnp::http::Management for ShellManagement {
    fn cast_url(&self, url: &str, title: Option<&str>) -> Result<String, String> {
        let sent = cast_uri(
            &self.host,
            &self.player,
            url,
            title.unwrap_or(""),
        )?;
        let read = |key: &str| -> &str { sent.get(key).and_then(Value::as_str).unwrap_or("") };
        Ok(format!(
            "已投屏：{}（名字：{}，第 {}/{} 首）",
            read("uri"),
            read("title"),
            sent["track"],
            sent["total"],
        ))
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
