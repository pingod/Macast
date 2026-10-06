//! The transport half: the mpv process and the socket that feeds it.
//!
//! This is what `macast_renderer/mpv.py` does when it is not doing arithmetic:
//! [`run_player`] is `start_mpv`, [`run_ipc`] is `start_ipc`, [`send_command`] is
//! `send_command`, and [`stop`] is `MPVRenderer.stop`. The pure halves — argv,
//! command shapes, byte framing, reply translation — stayed in their own modules
//! and are called from here, so every ordering decision the reference made is
//! visible in one file:
//!
//! - `set_state_speed('1')` is the **first** thing `start_mpv` does, before the
//!   command line is even built, so it happens on every retry attempt and on the
//!   attempt that never reaches a spawn.
//! - `argv::build` is called **outside** the `try`, so a knob whose stored value
//!   makes Python raise escapes the thread as a traceback: no 「mpv starting」, no
//!   `publish('mpv_start')`, no retry, no three-attempt budget spent, no
//!   「mpv cannot start」, no notify, no `stop_service()`. The port reproduces that
//!   by breaking out of the loop with `error_time` still above zero.
//! - `ipc_sock` is assigned **before** `connect`, so a failed connect leaves an
//!   unconnected socket published in the slot and the next `send_command` hears
//!   `[Errno 57] Socket is not connected` rather than the `AttributeError`.
//! - the connect phase publishes `mpvipc_start` and `renderer_start`, then sets
//!   `ipc_once_connected`, then calls `set_observe`, then resets the buffer —
//!   `res = b''; msgs = None` is [`crate::ipc::Feed::reconnect`], and `msg`
//!   deliberately survives, which is why one [`crate::ipc::Feed`] belongs to a
//!   *thread* rather than to a connection.
//! - `stop()` runs in the reference's exact order: publish `renderer_av_stop`
//!   (that is `Renderer.stop()`, and it comes before our own log line) →
//!   「stoping mpv and mpv ipc」 → `['quit']` → `terminate` → `wait(5)` → (`kill`
//!   **only** inside the `except`) → join the player thread → `ipc_running =
//!   False` → join the ipc thread → remove the socket file. It therefore does
//!   **not** end the reader by closing the socket — see [`crate::link`]'s note on
//!   `Link::close` — the session ends because mpv exits and `recv` returns `b''`.
//!
//! Two things this module decides for itself, with no reference counterpart. Both
//! are on purpose, and both are said out loud here rather than left as a diff:
//!
//! 1. **`Level::Warning` for [`crate::argv::Build::warnings`].** `grep
//!    'logger.warn' macast_renderer/mpv.py` returns nothing: the reference has no
//!    warning-level logging at all, so the level itself is ours. The alternative
//!    was to drop the sentences, which is the same silence as not having them.
//! 2. **The `Host` seam has no production implementation here.** The workspace has
//!    no event bus yet — there is no CherryPy to `publish` to — so inventing a
//!    `CherryPyHost` would mean claiming a behaviour for the dropped `publish`
//!    calls that nothing has defined. `src-tauri` implements this trait when the
//!    bus exists; the tests in this module are the only implementations for now.
//!
//! Honest limitations, all of them inherited from the reference rather than fixed:
//!
//! - A *foreign* process bound to the socket path that never `accept`s makes the
//!   reader hang in `recv`, and `stop()` then blocks in the player-thread `join()`
//!   exactly as the Python does. `Link::close` would fix it; using it here would
//!   be a divergence, so the tests that need the reader gone hang up the fake mpv
//!   instead (`stop_and_hang_up`), and the fake listener is **not** the spawned
//!   child — which is why that helper exists at all.
//! - 「mpv stopped」 can land up to one poll interval later than the child's exit,
//!   because the port polls `try_wait` where Python blocks in `communicate()`.
//! - mpv's stderr is drained to a sink and discarded. That is what
//!   `stdout=DEVNULL` plus an unread `stderr` pipe effectively is, and it is why a
//!   bad mpv argument in the field reads as "it started and stopped" — keeping the
//!   pipe unread would instead fill 64 KB and block mpv itself.
//! - `stop()` without `start()` no-ops where Python raises `AttributeError` on
//!   `self.mpv_thread.join()`. A port that crashed the caller on a double-click in
//!   the menu bar would be worse than one that quietly did nothing.

use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{Builder, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use macast_core::logging::Level;

use crate::argv::{self, Config};
use crate::command;
use crate::ipc::{Feed, Received};
use crate::link::{errno_text, Link, NO_SOCKET};
use crate::pyops;
use crate::state::{PlayerState, Signal};

/// `self.ipc_sock.recv(1048576)`.
const RECV_SIZE: usize = 1048576;

/// Lock, tolerating a poisoned mutex.
///
/// Poisoning means a panic happened while a thread held shared state. Python has
/// no such concept, and unwinding one thread must not make every other thread
/// crash on its next lock, so the recovery is to take the data anyway.
fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How long each of the reference's waits is.
///
/// A struct rather than constants because the tests in this module need the same
/// machine with the same ordering and 1/25th of the wall clock; a port whose
/// "does it retry three times" test takes 45 seconds never gets run.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// `time.sleep(0.5)` before every connect attempt.
    pub connect: Duration,
    /// `time.sleep(1)` in the retry branch.
    pub retry: Duration,
    /// `self.proc.wait(timeout=5)`.
    pub stop_wait: Duration,
    /// How often the ported `communicate()` asks the child for its status.
    pub poll: Duration,
}

impl Timings {
    /// The numbers read off `macast_renderer/mpv.py`.
    pub const REFERENCE: Timings = Timings {
        connect: Duration::from_millis(500),
        retry: Duration::from_secs(1),
        stop_wait: Duration::from_secs(5),
        poll: Duration::from_millis(50),
    };
}

/// One `set_state*` call the protocol is told about.
///
/// `CurrentTrack` and `NumberOfTracks` are the reference's generic
/// `set_state('CurrentTrack', …)` / `set_state('NumberOfTracks', …)` pair
/// (`macast_renderer/mpv.py:323-324`); the renderer state setters are named here
/// rather than as free functions because the seam is "who hears about a state
/// change", and the subtitle one is **`set_state_subtitle`**
/// (`macast_renderer/mpv.py:332`, `macast/renderer.py:214`).
#[derive(Debug, Clone, PartialEq)]
pub enum StateCall {
    /// `set_state_volume(int(res['data']))`.
    Volume(i64),
    Position(String),
    Duration(String),
    Pause,
    Play,
    /// `set_state_mute(res['data'])` — raw, unguarded.
    Mute(Value),
    CurrentTrack(i64),
    NumberOfTracks(i64),
    Speed(Value),
    Subtitle(Value),
    Stop,
    Eof,
    TransportError,
}

/// Everything the player needs from the application around it.
///
/// `publish` and `set_state` are the two directions of the old CherryPy bus;
/// `transport_state` and `current_url` are the two protocol reads `update_state`
/// makes (`duration` asks for the transport state, `start-file` for the url);
/// `default_volume` is `Setting.get(PlayerDefaultVolume, 100)`.
///
/// `default_volume` returns a [`Value`] and not an `i64` because the reference
/// reads that key and puts it **straight** into the command and into the
/// `f'Volume: {data}'` text: a value somebody hand-typed into the advanced
/// settings JSON as a string therefore reaches mpv as a string, and the log says
/// the string. Coercing here would be one more silent fix nobody asked for.
///
/// The three reads below serve the media control surface, and each is a
/// deliberate shape rather than an accident:
///
/// - `friendly_name` is read **once**, when the player is built, and it seeds the
///   title the window shows until the sender names the media (`mpv.py:113`).
/// - `player_size` is read on **every** hand-off, not from [`Config`]. The
///   reference calls `Setting.get` inside `set_media_url`, which is why changing
///   that setting on the settings page takes effect on the next cast without a
///   restart. `Config::size` is a different thing: it is the one value the command
///   line was built from, and the command line only exists at spawn.
/// - `current_position` is `publish('get_protocol')` followed by `.pop()` —
///   **the last** protocol answers, and an empty list answers `None` rather than
///   `Some("")`, because the reference binds `position = None` and passes that
///   `None` to the loadfile builder.
pub trait Host: Send + Sync + 'static {
    fn publish(&self, event: &str, args: &[Value]);
    fn stop_service(&self);
    fn transport_state(&self) -> String;
    fn current_url(&self) -> String;
    fn default_volume(&self) -> Value;
    fn set_state(&self, call: StateCall);
    fn log(&self, level: Level, text: &str);
    fn friendly_name(&self) -> String;
    fn player_size(&self) -> Value;
    fn current_position(&self) -> Option<String>;
}

/// Hand one translated reply to the application, in the order the reference does.
pub fn deliver(host: &dyn Host, signal: &Signal) {
    match signal {
        Signal::Info(text) => host.log(Level::Info, text),
        Signal::Debug(text) => host.log(Level::Debug, text),
        Signal::Volume(value) => host.set_state(StateCall::Volume(*value)),
        Signal::Position(text) => host.set_state(StateCall::Position(text.clone())),
        Signal::Duration(text) => host.set_state(StateCall::Duration(text.clone())),
        Signal::PublishDuration(text) => {
            host.publish("mpv_update_duration", &[json!(text)])
        }
        Signal::Pause => host.set_state(StateCall::Pause),
        Signal::Play => host.set_state(StateCall::Play),
        Signal::Mute(value) => host.set_state(StateCall::Mute(value.clone())),
        Signal::Tracks { current, number } => {
            host.set_state(StateCall::CurrentTrack(*current));
            host.set_state(StateCall::NumberOfTracks(*number));
        }
        Signal::Speed(value) => host.set_state(StateCall::Speed(value.clone())),
        Signal::Subtitle(value) => host.set_state(StateCall::Subtitle(value.clone())),
        Signal::AvStop => host.publish("renderer_av_stop", &[]),
        Signal::Stop => host.set_state(StateCall::Stop),
        Signal::Eof => host.set_state(StateCall::Eof),
        Signal::TransportError => host.set_state(StateCall::TransportError),
        Signal::FileError(value) => {
            host.publish("app_notify", &[json!("File error"), value.clone()])
        }
        Signal::AvUri(url) => host.publish("renderer_av_uri", &[json!(url)]),
    }
}

/// The state one player owns, shared by its two threads.
struct Shared {
    host: Arc<dyn Host>,
    /// Built once. The socket path lives here so that `--input-ipc-server` and the
    /// connect call can never be told about two different files.
    config: Config,
    timings: Timings,
    socket_path: String,
    running: AtomicBool,
    ipc_running: AtomicBool,
    ipc_once_connected: AtomicBool,
    link: Mutex<Option<Link>>,
    command_lock: Mutex<()>,
    child: Mutex<Option<Child>>,
    mpv_thread: Mutex<Option<JoinHandle<()>>>,
    ipc_thread: Mutex<Option<JoinHandle<()>>>,
    state: Mutex<PlayerState>,
    /// The name the player window carries. Seeded from
    /// [`Host::friendly_name`] and replaced by every [`Player::set_media_title`],
    /// which is also the value a `reload()` hands back after the restart — mpv
    /// comes up with its default `--title` template, so both properties have to be
    /// re-sent from the one place that names the player.
    title: Mutex<String>,
    /// `_media_live`: what the sender declared about the media that is in the
    /// player. Three states, and `Undeclared` must never collapse into `Finite`
    /// (AGENTS.md §4.1: `None` means "keep today's behaviour", which is the safe
    /// side for anything we do not understand).
    live: Mutex<command::Liveness>,
    /// The `subscribe('mpvipc_start', loadfile)` of the reference, in the shape a
    /// port can actually make one-shot: the url `reload()` read while the player
    /// was still playing, waiting for the next connection to spend it.
    ///
    /// [`Shared::pending_reload`] is **taken**, not inspected, so the second
    /// reconnect of the same session loads nothing — that take is the
    /// `unsubscribe`. `Vec::pop` on the position and the live verdict happen later,
    /// inside the reconnect, exactly as the closure deferred them.
    pending_reload: Mutex<Option<String>>,
}

/// A renderer that owns an mpv process and its IPC socket.
#[derive(Clone)]
pub struct Player {
    shared: Arc<Shared>,
}

impl Player {
    /// [`Timings::REFERENCE`].
    pub fn new(host: impl Host, config: Config) -> Self {
        Self::with_timings(host, config, Timings::REFERENCE)
    }

    /// An idle player: nothing spawned, nothing connected, `running` false.
    ///
    /// An empty `config.ipc_socket` is resolved here (the reference makes
    /// `/tmp/macast_mpvsocket{rand}` in `__init__`) and **written back into the
    /// stored config**, so the command line and the connect path are the same
    /// string by construction rather than by two calls agreeing.
    ///
    /// The title is read here too, before `host` moves into the `Arc`: it is the
    /// `self.title = Setting.get_friendly_name()` of `__init__`, so a player that
    /// has never been named by a sender still has a name to hand back on a
    /// `reload()`. Nothing else in this constructor talks to the host — the
    /// `PlayerSize` a hand-off needs is read per hand-off, not here.
    pub fn with_timings(host: impl Host, mut config: Config, timings: Timings) -> Self {
        if config.ipc_socket.is_empty() {
            config.ipc_socket =
                command::ipc_socket_path(&config.platform, command::socket_random());
        }
        let socket_path = config.ipc_socket.clone();
        let title = host.friendly_name();
        Player {
            shared: Arc::new(Shared {
                host: Arc::new(host),
                config,
                timings,
                socket_path,
                running: AtomicBool::new(false),
                ipc_running: AtomicBool::new(false),
                ipc_once_connected: AtomicBool::new(false),
                link: Mutex::new(None),
                command_lock: Mutex::new(()),
                child: Mutex::new(None),
                mpv_thread: Mutex::new(None),
                ipc_thread: Mutex::new(None),
                state: Mutex::new(PlayerState::new()),
                title: Mutex::new(title),
                live: Mutex::new(command::Liveness::Undeclared),
                pending_reload: Mutex::new(None),
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    pub fn socket_path(&self) -> &str {
        &self.shared.socket_path
    }

    /// The name currently on the player, which is what a `reload()` re-sends.
    pub fn title(&self) -> String {
        guard(&self.shared.title).clone()
    }

    /// The live verdict currently held for the media in the player.
    pub fn media_live(&self) -> command::Liveness {
        *guard(&self.shared.live)
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// `self.ipc_running` — the flag that makes the second `start_ipc` say
    /// 「mpv ipc is already runing」, and that a killed reader leaves `true`.
    pub fn ipc_running(&self) -> bool {
        self.shared.ipc_running.load(Ordering::SeqCst)
    }

    pub fn ipc_once_connected(&self) -> bool {
        self.shared.ipc_once_connected.load(Ordering::SeqCst)
    }

    pub fn is_playing(&self) -> bool {
        guard(&self.shared.state).playing()
    }

    pub fn is_pause(&self) -> bool {
        guard(&self.shared.state).pause()
    }

    /// The pid of the player we last spawned, or `None`.
    ///
    /// `0` is filtered out: std says a reaped `Child` may report `id() == 0`, and
    /// `kill(0, sig)` would signal the **process group** — the last thing a port
    /// of a media player should ever do to the process that hosts it.
    pub fn player_pid(&self) -> Option<u32> {
        let slot = guard(&self.shared.child);
        slot.as_ref().map(|c| c.id()).filter(|p| *p != 0)
    }

    /// `MPVRenderer.start()`: `running = True`, one INFO line, then the two
    /// threads in the reference's order.
    pub fn start(&self) {
        let shared = self.shared.clone();
        shared.running.store(true, Ordering::SeqCst);
        shared.host.log(Level::Info, "starting mpv and mpv ipc");
        let for_player = shared.clone();
        match Builder::new()
            .name("MPV_THREAD".to_string())
            .spawn(move || run_player(for_player))
        {
            Ok(handle) => *guard(&shared.mpv_thread) = Some(handle),
            Err(err) => shared
                .host
                .log(Level::Error, &format!("MPV_THREAD: {}", err)),
        }
        let for_ipc = shared.clone();
        match Builder::new()
            .name("MPV_IPC_THREAD".to_string())
            .spawn(move || run_ipc(for_ipc))
        {
            // Overwriting a previous handle is what the reference does when
            // `self.ipc_thread` is reassigned; the `ipc_running` guard above is
            // the thing that stops a second thread from doing any work.
            Ok(handle) => *guard(&shared.ipc_thread) = Some(handle),
            Err(err) => shared
                .host
                .log(Level::Error, &format!("MPV_IPC_THREAD: {}", err)),
        }
    }

    /// `MPVRenderer.stop()`. See the module doc for the order and for what this
    /// deliberately does **not** do.
    pub fn stop(&self) {
        stop(&self.shared)
    }

    /// `send_command(command)` — the public face of the one thing both threads and
    /// every menu action write through.
    pub fn send_command(&self, command: &[Value]) -> bool {
        send_command(&self.shared, command)
    }

    // --- the media control surface ------------------------------------------------
    //
    // `mpv.py:116-249`. Every one of these composes through [`Player::send_command`]
    // rather than touching the link, so the DEBUG-outside-the-lock contract, the
    // `sendCommand: {e}` contract and the `NO_SOCKET` contract are each owned once
    // and cannot be re-broken by a new action.

    /// `note_media_live(declared)` — remember what the sender said about the media
    /// about to be loaded.
    ///
    /// The three answers are stored as given, and only `Some(true)` changes the
    /// command that goes out later. `None` must not be collapsed into
    /// `Some(false)`: AGENTS.md §4.1 — "keep today's behaviour" and "this is a file
    /// you may seek into" are different statements, and the second one hands mpv a
    /// read-ahead for a stream that has no addressable prefix.
    ///
    /// The name is the reference's, and it is deliberately **not**
    /// `set_media_live`: `Renderer.methods()` enumerates the `set_media_` prefix and
    /// the plugin manager subscribes every name it enumerates to the bus, so a name
    /// spelled that way would put a message nobody sends onto the bus and imply to
    /// every built-in renderer that it should implement one (§4.2, Part 64's L arm).
    pub fn note_media_live(&self, declared: Option<bool>) {
        *guard(&self.shared.live) = command::Liveness::from_declaration(declared);
    }

    /// `set_media_stop`: mpv's bare `stop`, and the live verdict dropped first.
    ///
    /// The verdict is a statement about *the media that was in the player*. Clearing
    /// it is not hygiene: a next hand-off that arrives with no declaration at all
    /// would otherwise inherit this one's live options and quietly play an ordinary
    /// file with no read-ahead.
    pub fn set_media_stop(&self) {
        *guard(&self.shared.live) = command::Liveness::Undeclared;
        self.send_command(&command::stop());
    }

    /// `set_media_pause` — the property is a bool, the toast says `Pause`.
    pub fn set_media_pause(&self) {
        self.send_command(&command::pause(true));
        self.set_media_message("Pause");
    }

    /// `set_media_resume`
    pub fn set_media_resume(&self) {
        self.send_command(&command::pause(false));
        self.set_media_message("Resume");
    }

    /// `set_media_volume(data)` — `data` goes into the command **as it came in**.
    ///
    /// A `Value`, not an `i64`, for the same reason as [`Host::default_volume`]: a
    /// string somebody hand-typed into the advanced settings JSON reaches mpv as a
    /// string and the toast says the string. `f'Volume: {data}'` is Python's `str()`,
    /// so the toast of the number `50` says `Volume: 50` and of `"50"`, `Volume: 50`
    /// too — but of `50.0` it says `Volume: 50.0`, which is why this goes through
    /// [`pyops::py_str`] and not `to_string`.
    pub fn set_media_volume(&self, data: &Value) {
        self.send_command(&command::volume(data.clone()));
        self.set_media_message(&format!("Volume: {}", pyops::py_str(data)));
    }

    /// `set_media_mute(data)` — `"yes"`/`"no"` chosen by Python **truthiness**, and
    /// the toast printing the value as it arrived.
    ///
    /// Both halves read the same incoming [`Value`] and neither normalises it to the
    /// other: `"yes" if data else "no"` is truthiness (so `0`, `""`, `[]`, `null` and
    /// `false` all mean unmuted), while `f'Mute: {data}'` is `str(data)` (so `1`
    /// mutes and the toast says `Mute: 1`, not `Mute: True`). A port that took a
    /// `bool` here would be inventing a coercion the reference never applies, and
    /// would lose the only witness to what the caller actually passed.
    pub fn set_media_mute(&self, data: &Value) {
        self.send_command(&command::mute(pyops::truthy(data)));
        self.set_media_message(&format!("Mute: {}", pyops::py_str(data)));
    }

    /// `set_media_speed(data)` — the property, and **no toast**.
    ///
    /// The reference sends `set_property speed` and stops; mpv's own OSC already
    /// reports it. Inventing a `show-text` here would put a line on the user's screen
    /// that the build they are comparing against never put there.
    pub fn set_media_speed(&self, data: &Value) {
        self.send_command(&command::speed(data.clone()));
    }

    /// `set_media_url(url, start)` — the hand-off, and the one place that reads
    /// [`Host::player_size`].
    ///
    /// `start` is `None` for the reference's default `"0"`; both mean "no `start`
    /// option", because the builder drops a position that is empty or literally `0`.
    ///
    /// Fullscreen is a player-wide choice the user made, so it stays a
    /// `set_property` and does **not** join the per-file options on `loadfile` — a
    /// per-file option would be forgotten by the next item that arrives with no
    /// declaration, and the window would stop going full-screen on the second video.
    /// The comparison is [`pyops::py_eq`] and not `==` on `Value`: the reference
    /// compares against the enum's `4`, so the number `4.0` means fullscreen and the
    /// string `"4"` does not.
    pub fn set_media_url(&self, url: &str, start: Option<&str>) {
        if pyops::py_eq(&self.shared.host.player_size(), &json!(4)) {
            self.send_command(&command::fullscreen());
        }
        let live = self.media_live();
        self.send_command(&command::loadfile(url, start, live));
    }

    /// `set_media_title(data)` — the name, remembered and then written twice.
    ///
    /// Both `title` and `force-media-title` are needed and the reason is in
    /// AGENTS.md §4.2: mpv's default `--title` template falls back to `filename`, and
    /// for a `/relay/<id>/media` address the last path segment is literally `media`.
    /// `--title` belongs to the user so this port never writes it; forcing the
    /// property is the seam that reaches the window.
    ///
    /// Storing the text is what makes [`Player::reload`] able to say it again — after
    /// a restart both properties are gone, because mpv came back with the default
    /// template.
    pub fn set_media_title(&self, data: &str) {
        *guard(&self.shared.title) = data.to_string();
        for shaped in command::title(data) {
            self.send_command(&shaped);
        }
    }

    /// `set_media_position(data)` — `data` is a `00:00:00` clock string, and the
    /// third argument is mpv's own `absolute`.
    pub fn set_media_position(&self, data: &str) {
        self.send_command(&command::seek(data));
    }

    /// `set_media_sub_file(data)` — `{'url': …, 'title': …}`, `select` in the middle.
    ///
    /// The reference reads `data['url']` and `data['title']` and lets a `KeyError`
    /// escape to the bus that logged it. This logs instead: the exception would have
    /// no bus to travel up to yet, and silence would be worse than either — the user
    /// asked for a subtitle track. Nor does it send a `sub-add` with a `null` in it;
    /// mpv rejects that, and the reader would then report mpv's rejection instead of
    /// our own missing key.
    pub fn set_media_sub_file(&self, data: &Value) {
        let mut unusable: Vec<String> = Vec::new();
        for key in ["url", "title"] {
            match data.get(key) {
                Some(Value::String(_)) => {}
                Some(other) => unusable.push(format!("{} is {}", key, pyops::type_name(other))),
                None => unusable.push(format!("{} is missing", key)),
            }
        }
        if !unusable.is_empty() {
            self.shared.host.log(
                Level::Error,
                &format!("set_media_sub_file: {}", unusable.join(", ")),
            );
            return;
        }
        // Both keys are known to be strings by the loop above.
        let url = data["url"].as_str().unwrap_or_default();
        let title = data["title"].as_str().unwrap_or_default();
        self.send_command(&command::sub_add(url, title));
    }

    /// `set_media_sub_show(data)` — again Python truthiness, again `"yes"`/`"no"`, and
    /// again no toast (the reference sends the property alone).
    pub fn set_media_sub_show(&self, data: &Value) {
        self.send_command(&command::sub_show(pyops::truthy(data)));
    }

    /// `set_media_text(data, duration)` — mpv's `show-text`, `duration` in ms.
    pub fn set_media_text(&self, text: &str, duration_ms: i64) {
        self.send_command(&command::show_text(text, duration_ms));
    }

    /// `set_media_text(data)` at the reference's default duration (1000 ms).
    pub fn set_media_message(&self, text: &str) {
        self.set_media_text(text, 1000);
    }

    /// `reload()`: stop, start, and if it was playing, play that again.
    ///
    /// Three orderings are the whole method, and none of them is decorative:
    ///
    /// 1. The url is read **now**, while the old player is still up, but the
    ///    position and the live verdict are read **later**, on the reconnect — the
    ///    reference's `uri = self.protocol.get_state_url()` is a plain statement at
    ///    the top while `get_state_position()` lives inside the closure. Read the url
    ///    late and it is whatever `stop()` left behind, which is not the media.
    /// 2. The reload is armed **before** the restart thread starts, in this thread.
    ///    The reference's `subscribe` is likewise in the caller, and the connect can
    ///    happen fast: arm after the spawn and a quick reconnect misses it, which is
    ///    a silent "reload did nothing".
    /// 3. Only a player whose transport says `PLAYING` gets armed. Anything else
    ///    reloads to an idle window, which is the reference's `else` branch.
    ///
    /// The thread is load-bearing, not tidiness: [`Player::stop`] joins the two
    /// player threads, so a caller that *is* one of them would be joining itself.
    pub fn reload(&self) {
        let shared = self.shared.clone();
        let url = shared.host.current_url();
        if shared.host.transport_state() == "PLAYING" {
            *guard(&shared.pending_reload) = Some(url);
        }
        let reloader = Player { shared };
        if let Err(err) = Builder::new()
            .name("MPV_RELOAD".to_string())
            .spawn(move || {
                reloader.stop();
                reloader.start();
            })
        {
            // The reference lets `Thread.start()` raise. There is no exception to
            // hand a caller that is a plugin callback, so the failure says which
            // thread could not start, in the same shape as `MPV_THREAD: {e}`, and
            // leaves the player exactly as it was — which is at least a state a
            // reader can act on, unlike a half reload.
            self.shared
                .host
                .log(Level::Error, &format!("MPV_RELOAD: {}", err));
        }
    }
}

/// `mpv_thread.is_alive()`, ported as its inverse.
///
/// Asked **through** the mutex rather than by taking the handle out of it:
/// `JoinHandle` is not `Clone`, and `is_finished` only needs `&self`.
fn player_alive(shared: &Arc<Shared>) -> bool {
    match guard(&shared.mpv_thread).as_ref() {
        Some(handle) => !handle.is_finished(),
        // Python raises `AttributeError: 'NoneType' object has no attribute
        // 'is_alive'` here. Reaching this in the port means the thread could not
        // be spawned at all, and that failure is already logged; "the player is
        // not running" is the safe reading of it.
        None => false,
    }
}

/// `start_mpv`, line for line.
fn run_player(shared: Arc<Shared>) {
    let mut error_time: i32 = 3;
    while shared.running.load(Ordering::SeqCst) && error_time > 0 {
        shared.host.set_state(StateCall::Speed(json!("1")));
        // Outside the try: a raising knob never reaches a spawn, never spends the
        // retry budget, and never tells anybody through the notify path.
        let build = match argv::build(&shared.config) {
            Ok(build) => build,
            Err(err) => {
                shared.host.log(Level::Error, &err.message());
                break;
            }
        };
        for warning in &build.warnings {
            shared.host.log(Level::Warning, warning);
        }
        shared.host.log(Level::Info, "mpv starting");
        shared.host.publish("mpv_start", &[]);
        // `except Exception as e: logger.error(e)` — and `communicate()` does not
        // raise on a non-zero exit, so the only text here comes from the spawn or
        // from `waitpid`.
        if let Err(text) = spawn_and_wait(&shared, &build.argv) {
            shared.host.log(Level::Error, &text);
        }
        shared.host.log(Level::Info, "mpv stopped");
        if shared.running.load(Ordering::SeqCst) && !shared.ipc_once_connected.load(Ordering::SeqCst)
        {
            std::thread::sleep(shared.timings.retry);
            error_time -= 1;
            shared.host.log(Level::Error, "mpv restarting");
        }
    }
    if error_time <= 0 {
        shared
            .host
            .publish("app_notify", &[json!("Macast"), json!("MPV Can't start")]);
        shared.host.log(Level::Error, "mpv cannot start");
        let host = shared.host.clone();
        let _ = Builder::new()
            .name("MPV_STOP_SERVICE".to_string())
            .spawn(move || host.stop_service());
    }
}

/// The three answers the poll loop needs, so that "still running" and "the player
/// is gone from the slot" cannot be confused by a `bool`.
enum Step {
    Poll,
    Done,
    Failed(String),
}

/// `Popen(...)` then `proc.communicate()`, returning `str(e)` for the exception
/// Python would have raised.
fn spawn_and_wait(shared: &Arc<Shared>, argv: &[String]) -> Result<(), String> {
    // `argv[0]` is always `config.mpv_path` for a build that got here, but an empty
    // list has to answer with something, and `''` is what reproduces Python's
    // `[Errno 2] No such file or directory: ''`.
    let (program, rest): (String, &[String]) = match argv.split_first() {
        Some((first, more)) => (first.clone(), more),
        None => (String::new(), &[]),
    };
    let mut cmd = macast_core::env::player_command(program.clone());
    cmd.args(rest)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return Err(spawn_failure(&err, &program)),
    };
    // `communicate()` with no input: our stdin closes immediately, and mpv's stderr
    // has to be drained or the pipe fills and mpv blocks (see the module doc).
    if let Some(pipe) = child.stderr.take() {
        let _ = Builder::new().name("MPV_STDERR".to_string()).spawn(move || {
            let mut pipe = pipe;
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        });
    }
    drop(child.stdin.take());
    *guard(&shared.child) = Some(child);

    loop {
        let step = {
            let mut slot = guard(&shared.child);
            match slot.as_mut() {
                // `stop()` took it; the process is no longer ours to wait for.
                None => Step::Done,
                Some(c) => match c.try_wait() {
                    // std caches the status, so a second reader (this thread and
                    // `stop()`) sees the same exit code instead of failing.
                    Ok(Some(_)) => Step::Done,
                    Ok(None) => Step::Poll,
                    Err(err) => Step::Failed(io_text(&err)),
                },
            }
        };
        match step {
            Step::Done => return Ok(()),
            Step::Failed(text) => return Err(text),
            Step::Poll => std::thread::sleep(shared.timings.poll),
        }
    }
}

/// `str(e)` for an `io::Error` about a socket or a wait.
fn io_text(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => errno_text(code),
        None => err.to_string(),
    }
}

/// `str(e)` for `Popen`. This one names the program and socket errors never do:
/// `[Errno 2] No such file or directory: 'no-such-program-xyz'`.
fn spawn_failure(err: &std::io::Error, program: &str) -> String {
    match err.raw_os_error() {
        Some(code) => format!("{}: '{}'", errno_text(code), program),
        None => err.to_string(),
    }
}

/// `start_ipc`, line for line.
fn run_ipc(shared: Arc<Shared>) {
    if shared.ipc_running.swap(true, Ordering::SeqCst) {
        shared
            .host
            .log(Level::Error, "mpv ipc is already runing");
        return;
    }
    // One feed and one read buffer for the whole thread: the reference's `res` and
    // `msgs` are reset only after a *successful* connect, and `msg` is never reset
    // at all, so the first chunk a thread fails to decode is judged with the
    // history of every connection it has had. See `crate::ipc::Feed::reconnect`.
    let mut feed = Feed::new();
    let mut buf = vec![0u8; RECV_SIZE];
    while shared.ipc_running.load(Ordering::SeqCst)
        && shared.running.load(Ordering::SeqCst)
        && player_alive(&shared)
    {
        if let Err(text) = connect_once(&shared, &mut feed) {
            shared.host.log(
                Level::Error,
                &format!("mpv ipc socket reconnecting: {}", text),
            );
            continue;
        }
        let Some(link) = guard(&shared.link).clone() else {
            return; // unreachable: connect_once publishes one before it returns Ok
        };
        match read_loop(&shared, &link, &mut feed, &mut buf) {
            Session::Ended => {
                // Close **through the slot**, so the slot's own handle carries the
                // closed flag and a later `send_command` hears `[Errno 9]` rather
                // than a success it cannot have. `Link`'s flag is per-instance;
                // only the descriptor is shared.
                if let Some(slot) = guard(&shared.link).as_ref() {
                    slot.close();
                }
                shared.host.log(Level::Error, "mpv ipc stopped");
            }
            // The `except` arm raised inside the `except` arm. Python's thread dies
            // here without reaching `close()` or 「mpv ipc stopped」, and
            // `ipc_running` stays `true`.
            Session::Abandoned => return,
        }
    }
}

/// One turn of the connect phase: the sleep, the socket, the two publishes, the
/// observations. `Err` is `str(e)` for the reconnect line.
fn connect_once(shared: &Arc<Shared>, feed: &mut Feed) -> Result<(), String> {
    std::thread::sleep(shared.timings.connect);
    shared
        .host
        .log(Level::Error, "mpv ipc socket start connect");
    let link = Link::created()?;
    // Assigned before the connect, exactly as `self.ipc_sock = socket.socket(...)`
    // is, so a failed connect leaves an unconnected socket in the slot.
    *guard(&shared.link) = Some(link.clone());
    link.connect(&shared.socket_path)?;
    shared.host.publish("mpvipc_start", &[]);
    load_pending(shared);
    shared.host.publish("renderer_start", &[]);
    shared.ipc_once_connected.store(true, Ordering::SeqCst);
    set_observe(shared);
    feed.reconnect();
    Ok(())
}

/// The `subscribe('mpvipc_start', loadfile)` of the reference, run on the reconnect
/// that [`Player::reload`] armed.
///
/// In the closure that subscription fires **inside** `publish('mpvipc_start')`, so
/// the work happens after the socket exists and before `renderer_start` is
/// announced; doing it here, between the two publishes, is observably the same
/// because those two calls are the only events on either side of it, and a subscriber
/// could not have depended on anything else in between.
///
/// The url leaves the slot by `take()` rather than by inspection: that single removal
/// **is** the reference's `unsubscribe('mpvipc_start', loadfile)`. Read it twice and
/// the second reconnect of the same session would replay a media the user has since
/// stopped. Everything else the closure reads — the position and the live verdict —
/// is read *here*, at reconnect time, not when the reload was requested; that is the
/// half of `reload`'s ordering that [`Player::reload`] documents.
fn load_pending(shared: &Arc<Shared>) {
    let Some(url) = guard(&shared.pending_reload).take() else {
        return;
    };
    shared.host.log(Level::Debug, "mpv loadfile");
    let position = shared.host.current_position();
    let live = *guard(&shared.live);
    let player = Player {
        shared: shared.clone(),
    };
    player.send_command(&command::loadfile(&url, position.as_deref(), live));
    // The name has to be said again: mpv came back with its default `--title`
    // template, so the window of the media we are restoring would otherwise read
    // `media - mpv` — AGENTS.md §4.2's 「改播放器的地址，就必须改播放器的名字」.
    let title = player.title();
    player.set_media_title(&title);
}

/// How the inner `while self.ipc_running` loop ended.
enum Session {
    /// EOF, or a `recv` that raised, or `ipc_running` going false: the bottom of
    /// the outer loop body runs — close and 「mpv ipc stopped」.
    Ended,
    /// The decode handler itself raised. Nothing below it runs.
    Abandoned,
}

fn read_loop(
    shared: &Arc<Shared>,
    link: &Link,
    feed: &mut Feed,
    buf: &mut [u8],
) -> Session {
    while shared.ipc_running.load(Ordering::SeqCst) {
        match link.recv_into(buf) {
            // `if data == b'': break` — before anything is added to the buffer.
            Ok(0) => return Session::Ended,
            Ok(n) => match feed.push(&buf[..n], |line| apply_line(shared, line)) {
                Received::Buffered | Received::Applied(_) => {}
                Received::Closed(close) => {
                    if let Some(text) = close.log_line() {
                        shared.host.log(Level::Debug, &text);
                    }
                    return Session::Ended;
                }
                Received::Failed(failure) => {
                    for text in failure.log_lines() {
                        shared.host.log(Level::Error, &text);
                    }
                    if failure.kills_the_reader() {
                        return Session::Abandoned;
                    }
                }
            },
            Err(text) => {
                // `logger.debug(e)` and then `break`.
                if let Received::Closed(close) = feed.recv_failed(&text) {
                    if let Some(line) = close.log_line() {
                        shared.host.log(Level::Debug, &line);
                    }
                }
                return Session::Ended;
            }
        }
    }
    Session::Ended
}

/// `update_state(msg)`, then the state calls it asked for.
///
/// The two protocol reads happen **per line**, and the state lock is dropped
/// before anything is delivered: holding it across `host.set_state` would let one
/// reply call back into the application while the reader owns the player's state.
fn apply_line(shared: &Arc<Shared>, line: &str) -> Result<(), String> {
    let transport = shared.host.transport_state();
    let url = shared.host.current_url();
    let signals = {
        let mut state = guard(&shared.state);
        state.set_protocol(&transport, &url);
        state.apply(line)?
    };
    for signal in &signals {
        deliver(&*shared.host, signal);
    }
    Ok(())
}

/// `set_observe`: **eight** separate `send_command` calls, then the default volume,
/// then its `show-text`. Each one logs its own DEBUG line, as in the reference.
///
/// The last two go through [`Player::set_media_volume`] rather than being written out
/// again here, so the string `Volume: ` has one owner. Both places announce the
/// volume — this one on connect, the other when the user moves it — and a port that
/// spells the sentence twice is a port where one of them will drift.
fn set_observe(shared: &Arc<Shared>) {
    for observed in command::observe_all() {
        send_command(shared, &observed);
    }
    let volume = shared.host.default_volume();
    Player {
        shared: shared.clone(),
    }
    .set_media_volume(&volume);
}

/// `send_command(command)`.
///
/// The DEBUG line is written **outside** the lock (that is where Python writes it),
/// and the lock is held across reading `ipc_sock` *and* the send, because that is
/// what `with self.command_lock:` is.
fn send_command(shared: &Arc<Shared>, command: &[Value]) -> bool {
    shared.host.log(
        Level::Debug,
        &format!("send command: {}", command::logged_command(command)),
    );
    let _locked = guard(&shared.command_lock);
    let slot = guard(&shared.link).clone();
    let Some(link) = slot else {
        shared
            .host
            .log(Level::Error, &format!("sendCommand: {}", NO_SOCKET));
        return false;
    };
    match link.send_all(command::encode(command).as_bytes()) {
        Ok(()) => true,
        Err(text) => {
            shared.host.log(Level::Error, &format!("sendCommand: {}", text));
            false
        }
    }
}

/// `MPVRenderer.stop()`, as a free function so a test can run it on its own thread
/// while it hangs up the fake mpv that the real one is not.
fn stop(shared: &Arc<Shared>) {
    // `Renderer.stop()` first: `running = False`, then `renderer_av_stop`. Both
    // happen before this function's own log line, so a second `stop()` republishes
    // and re-logs exactly as a second `MPVRenderer.stop()` would.
    shared.running.store(false, Ordering::SeqCst);
    shared.host.publish("renderer_av_stop", &[]);
    shared.host.log(Level::Info, "stoping mpv and mpv ipc");
    send_command(shared, &[json!("quit")]);

    if let Some(pid) = child_pid(shared) {
        if !already_reaped(shared) {
            // `proc.terminate()` — CPython's is `kill(pid, SIGTERM)`, and it
            // returns silently once the status is cached, hence the guard above.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        let deadline = Instant::now() + shared.timings.stop_wait;
        let mut timed_out = true;
        while Instant::now() < deadline {
            match guard(&shared.child)
                .as_mut()
                .map(|c| c.try_wait())
                .unwrap_or(Ok(Some(Default::default())))
            {
                Ok(Some(_)) => {
                    timed_out = false;
                    break;
                }
                Ok(None) => std::thread::sleep(shared.timings.poll),
                Err(_) => {
                    timed_out = false;
                    break;
                }
            }
        }
        if timed_out {
            // `except Exception: proc.kill()` — only on the timeout, and std's
            // `kill()` would reap (and fail on a child we already reaped), so the
            // raw signal is the faithful call. No second wait: the player thread
            // collects the status.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }

    // The reference's order: join the player *before* clearing `ipc_running`, and
    // do not close the socket here — see [`crate::link`].
    // Bound the handle *outside* the guard: `if let Some(h) = lock().take()` keeps
    // the mutex held across the join, and the player thread is on its way out
    // through code that takes other locks.
    let mpv_thread = guard(&shared.mpv_thread).take();
    if let Some(handle) = mpv_thread {
        let _ = handle.join();
    }
    shared.ipc_running.store(false, Ordering::SeqCst);
    let ipc_thread = guard(&shared.ipc_thread).take();
    if let Some(handle) = ipc_thread {
        let _ = handle.join();
    }
    // `try: os.remove(self.mpv_sock) except OSError: pass` — mpv never removes its
    // own socket file, so this is the only cleanup there is, and a path that was
    // never created is not an error.
    let _ = std::fs::remove_file(&shared.socket_path);
}

/// The pid, or `None` if there is no process we spawned.
fn child_pid(shared: &Arc<Shared>) -> Option<u32> {
    let slot = guard(&shared.child);
    slot.as_ref().map(|c| c.id()).filter(|p| *p != 0)
}

/// `self.proc.returncode is not None` — the fact that makes CPython's
/// `send_signal` return without touching the kernel. An `Err` from `try_wait`
/// answers `false`: a probe that could not run must not swallow a signal.
fn already_reaped(shared: &Arc<Shared>) -> bool {
    let mut slot = guard(&shared.child);
    match slot.as_mut() {
        Some(c) => matches!(c.try_wait(), Ok(Some(_))),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU32;

    // --- test timings -----------------------------------------------------------

    fn quick() -> Timings {
        Timings {
            connect: Duration::from_millis(20),
            retry: Duration::from_millis(20),
            stop_wait: Duration::from_millis(300),
            poll: Duration::from_millis(10),
        }
    }

    fn wait_until(label: &str, pred: impl FnMut() -> bool) {
        wait_until_within(label, Duration::from_secs(5), pred)
    }

    /// The same poll, with the budget given by the caller.
    ///
    /// The five seconds above is right for a shell script that answers the moment it
    /// is asked. It is not right for a real player, which probes the media it was
    /// just handed before it says anything about it — so every wait that sits behind
    /// a genuine mpv reads the longer number instead.
    fn wait_until_within(label: &str, limit: Duration, mut pred: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !pred() {
            if Instant::now() > deadline {
                panic!("timed out waiting for: {}", label);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // --- temporary paths --------------------------------------------------------

    /// A private directory under `/tmp`, named after this test process and a
    /// counter, removed whole on drop. Nothing here ever touches
    /// `~/Library/Application Support/Macast`.
    struct Temp {
        dir: PathBuf,
    }

    static TEMP_SEQ: AtomicU32 = AtomicU32::new(0);

    impl Temp {
        fn new() -> Temp {
            let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(format!("/tmp/rm{}-{}", std::process::id(), n));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Temp { dir }
        }

        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_string_lossy().to_string()
        }

        /// A `/bin/sh` script standing in for mpv. It is passed the real mpv
        /// argument list and ignores it, so only `argv[0]` matters.
        fn script(&self, body: &str) -> String {
            let path = self.path("mpv.sh");
            std::fs::write(&path, format!("#!/bin/sh\n{}\n", body)).expect("write script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("chmod script");
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    // --- the application side ---------------------------------------------------

    #[derive(Default)]
    struct TestHost {
        logs: Mutex<Vec<(Level, String)>>,
        events: Mutex<Vec<(String, Vec<Value>)>>,
        states: Mutex<Vec<StateCall>>,
        stopped: AtomicBool,
        volume: Value,
        transport: String,
        url: String,
        friendly: String,
        size: Value,
        position: Option<String>,
    }

    impl TestHost {
        fn new() -> TestHost {
            TestHost {
                volume: json!(100),
                transport: "STOPPED".to_string(),
                url: String::new(),
                // The name a real install puts on the window before any media
                // arrives, and the size the reference's default `PlayerSize` maps to
                // — `Normal`, which is deliberately *not* the fullscreen answer.
                friendly: "Test Player".to_string(),
                size: json!(1),
                position: None,
                ..Default::default()
            }
        }

        fn with_volume(volume: Value) -> TestHost {
            TestHost {
                volume,
                ..TestHost::new()
            }
        }

        /// A player that is mid-media: the transport says `PLAYING`, the url and the
        /// reported position are the ones `reload()` is supposed to put back.
        fn playing(url: &str, position: &str) -> TestHost {
            TestHost {
                transport: "PLAYING".to_string(),
                url: url.to_string(),
                position: Some(position.to_string()),
                ..TestHost::new()
            }
        }

        /// Playing, and asked for a reload while the position was not readable yet.
        /// A real player answers `None` for `time-pos` during the window between a
        /// hand-off and the first decoded frame, so this is the shape a `reload()`
        /// sees most often on a stream that has just been pushed.
        fn playing_without_position(url: &str) -> TestHost {
            TestHost {
                transport: "PLAYING".to_string(),
                url: url.to_string(),
                ..TestHost::new()
            }
        }

        /// The value `PlayerSize` answers with, which is what decides whether a
        /// hand-off turns the window full-screen.
        fn at_size(size: Value) -> TestHost {
            TestHost {
                size,
                ..TestHost::new()
            }
        }

        fn named(friendly: &str) -> TestHost {
            TestHost {
                friendly: friendly.to_string(),
                ..TestHost::new()
            }
        }

        fn lines(&self) -> Vec<String> {
            self.logs.lock().unwrap().iter().map(|(_, t)| t.clone()).collect()
        }

        fn at(&self, level: Level) -> Vec<String> {
            self.logs
                .lock()
                .unwrap()
                .iter()
                .filter(|(l, _)| *l == level)
                .map(|(_, t)| t.clone())
                .collect()
        }

        fn count_containing(&self, needle: &str) -> usize {
            self.lines().iter().filter(|t| t.contains(needle)).count()
        }

        fn published(&self) -> Vec<String> {
            self.events.lock().unwrap().iter().map(|(e, _)| e.clone()).collect()
        }

        fn states(&self) -> Vec<StateCall> {
            self.states.lock().unwrap().clone()
        }
    }

    impl Host for TestHost {
        fn publish(&self, event: &str, args: &[Value]) {
            self.events
                .lock()
                .unwrap()
                .push((event.to_string(), args.to_vec()));
        }

        fn stop_service(&self) {
            self.stopped.store(true, Ordering::SeqCst);
        }

        fn transport_state(&self) -> String {
            self.transport.clone()
        }

        fn current_url(&self) -> String {
            self.url.clone()
        }

        fn default_volume(&self) -> Value {
            self.volume.clone()
        }

        fn set_state(&self, call: StateCall) {
            self.states.lock().unwrap().push(call);
        }

        fn log(&self, level: Level, text: &str) {
            self.logs
                .lock()
                .unwrap()
                .push((level, text.to_string()));
        }

        fn friendly_name(&self) -> String {
            self.friendly.clone()
        }

        fn player_size(&self) -> Value {
            self.size.clone()
        }

        fn current_position(&self) -> Option<String> {
            self.position.clone()
        }
    }

    /// Is `want` a subsequence of `seen`? Lifecycle tests need this rather than an
    /// exact list because a reader thread that is still reconnecting can add lines
    /// around the ones under test — see `stop_and_hang_up`.
    fn is_subsequence<T: PartialEq>(want: &[T], seen: &[T]) -> bool {
        let mut index = 0;
        for item in seen {
            if index < want.len() && *item == want[index] {
                index += 1;
            }
        }
        index == want.len()
    }

    // --- the fake mpv -----------------------------------------------------------

    /// A Unix listener that plays mpv: it accepts one connection, records every
    /// command line it receives, and can push reply lines.
    ///
    /// **It is not the spawned child.** The reference's reader ends because the
    /// process it is talking to exits and its half closes; here nothing does that,
    /// so a test that wants `stop()` to finish calls [`FakeMpv::hang_up`].
    struct FakeMpv {
        path: String,
        listener: Option<UnixListener>,
        accept_thread: Option<JoinHandle<()>>,
        conns: Arc<Mutex<Vec<Arc<UnixStream>>>>,
        readers: Arc<Mutex<Vec<JoinHandle<()>>>>,
        commands: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
    }

    impl FakeMpv {
        fn bind(temp: &Temp) -> FakeMpv {
            let path = temp.path("s");
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("bind fake mpv");
            // std's `bind` already calls `listen(SOMAXCONN)`, which is why this is a
            // valid stand-in: a connect into a backlog succeeds just as it does
            // against real mpv.
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let conns: Arc<Mutex<Vec<Arc<UnixStream>>>> = Arc::new(Mutex::new(Vec::new()));
            let readers = Arc::new(Mutex::new(Vec::new()));
            let commands = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));

            let mut fake = FakeMpv {
                path,
                listener: Some(listener),
                accept_thread: None,
                conns,
                readers,
                commands,
                stop,
            };
            // Bound first: `FakeMpv` has a `Drop`, so neither functional record
            // update nor a direct `field = self.method()` assignment is available.
            let accept_thread = fake.accept_loop();
            fake.accept_thread = Some(accept_thread);
            fake
        }

        /// The accept half, spun out of `bind` so `rebind` can point a **fresh**
        /// listener at the same recorded command log.
        ///
        /// It clones the listener because the struct keeps the original: `hang_up`
        /// drops that handle to produce the EOF, and a loop holding the last `Arc`
        /// of a socket would keep the peer open (see `drop_connections`).
        fn accept_loop(&self) -> JoinHandle<()> {
            let l = self
                .listener
                .as_ref()
                .expect("the accept loop needs a listener")
                .try_clone()
                .expect("clone listener");
            let conns2 = self.conns.clone();
            let readers2 = self.readers.clone();
            let commands2 = self.commands.clone();
            let stop2 = self.stop.clone();
            Builder::new()
                .name("FAKE_MPV_ACCEPT".to_string())
                .spawn(move || {
                    while !stop2.load(Ordering::SeqCst) {
                        match l.accept() {
                            Ok((stream, _)) => {
                                let stream = Arc::new(stream);
                                conns2.lock().unwrap().push(stream.clone());
                                let readers3 = readers2.clone();
                                let commands3 = commands2.clone();
                                let stop3 = stop2.clone();
                                let handle = Builder::new()
                                    .name("FAKE_MPV_READER".to_string())
                                    .spawn(move || {
                                        // std propagates the listener's nonblocking
                                        // flag onto accepted streams, and
                                        // `SO_RCVTIMEO` is inert on a descriptor with
                                        // `O_NONBLOCK` — so without this the loop
                                        // below returns `WouldBlock` in nanoseconds,
                                        // forever, and every parallel test burns a
                                        // whole core spinning.
                                        stream
                                            .set_nonblocking(false)
                                            .expect("blocking accepted stream");
                                        stream
                                            .set_read_timeout(Some(Duration::from_millis(30)))
                                            .expect("read timeout on accepted stream");
                                        let mut buf = [0u8; 4096];
                                        let mut text = String::new();
                                        let mut pending: Vec<u8> = Vec::new();
                                        while !stop3.load(Ordering::SeqCst) {
                                            // `Arc<UnixStream>` cannot give `&mut UnixStream`,
                                            // but `&UnixStream` itself implements `Read` —
                                            // which is the whole reason one connection can be
                                            // read by this thread and written by the player.
                                            let mut reader = stream.as_ref();
                                            match reader.read(&mut buf) {
                                                Ok(0) => break,
                                                Ok(n) => {
                                                    pending.extend_from_slice(&buf[..n]);
                                                    while let Some(pos) =
                                                        pending.iter().position(|b| *b == b'\n')
                                                    {
                                                        let line: Vec<u8> =
                                                            pending.drain(..=pos).collect();
                                                        let line =
                                                            String::from_utf8_lossy(&line[..pos]);
                                                        text.push_str(&line);
                                                        if let Ok(all) =
                                                            serde_json::from_str::<Value>(&text)
                                                        {
                                                            text.clear();
                                                            if let Some(command) =
                                                                all.get("command")
                                                            {
                                                                commands3
                                                                    .lock()
                                                                    .unwrap()
                                                                    .push(command.clone());
                                                            }
                                                        }
                                                        let _ = line;
                                                    }
                                                }
                                                Err(ref e)
                                                    if e.kind()
                                                        == std::io::ErrorKind::WouldBlock => {}
                                                Err(_) => break,
                                            }
                                        }
                                    })
                                    .expect("spawn fake reader");
                                readers3.lock().unwrap().push(handle);
                            }
                            Err(ref e)
                                if e.kind() == std::io::ErrorKind::WouldBlock
                                    || e.kind() == std::io::ErrorKind::NotConnected =>
                            {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("spawn fake accept")
        }

        fn path(&self) -> &str {
            &self.path
        }

        fn commands(&self) -> Vec<Value> {
            self.commands.lock().unwrap().clone()
        }

        /// Block until the fake actually holds a connection.
        ///
        /// `connect()` succeeds as soon as the listener has the socket sitting in
        /// its backlog — which is *before* the accept thread has picked it up. So
        /// a test that writes the moment the player reports
        /// `ipc_once_connected` can write to nobody, and it loses the line in
        /// silence: the reader then waits five seconds for a reply that was never
        /// sent. Real mpv has accepted long before it answers anything, so this is
        /// a fixture fact and not a protocol one — and the fix belongs on the
        /// writer, because every test that pushes a line is affected by it.
        fn await_connection(&self) {
            wait_until("the fake holds the connection", || {
                !self.conns.lock().unwrap().is_empty()
            });
        }

        /// One mpv-side line, to every connection it holds.
        fn send_line(&self, line: &str) {
            self.await_connection();
            let conns = self.conns.lock().unwrap().clone();
            for stream in conns {
                let mut stream = &*stream;
                let _ = stream.write_all(line.as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }

        fn send_bytes(&self, bytes: &[u8]) {
            self.await_connection();
            let conns = self.conns.lock().unwrap().clone();
            for stream in conns {
                let mut stream = &*stream;
                let _ = stream.write_all(bytes);
            }
        }

        /// End the connections it holds **and keep listening**.
        ///
        /// This is what a real mpv restart looks like from the reader's side: the
        /// socket dies, `recv` returns `b''`, and the next turn of the outer loop
        /// connects to a live listener again. `hang_up` cannot serve that test — it
        /// stops the accept loop and drops the listener too, so the next `connect`
        /// fails with `[Errno 2] No such file or directory` and the reader never
        /// reaches the two resets that follow a *successful* connect
        /// (`res = b''; msgs = None`, `macast_renderer/mpv.py:418-419`).
        ///
        /// The end is a `shutdown`, not a drop: this fake's own reader thread holds an
        /// `Arc` of the same stream, so releasing our handle would leave the peer open
        /// and that thread parked in `read` forever.
        fn drop_connections(&self) {
            let conns = std::mem::take(&mut *self.conns.lock().unwrap());
            for stream in &conns {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            let readers: Vec<JoinHandle<()>> =
                std::mem::take(&mut *self.readers.lock().unwrap());
            for handle in readers {
                let _ = handle.join();
            }
        }

        /// End every connection and stop listening, so the player's `recv` sees the
        /// EOF that a real mpv exit would have produced.
        fn hang_up(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let readers: Vec<JoinHandle<()>> =
                std::mem::take(&mut *self.readers.lock().unwrap());
            for handle in readers {
                let _ = handle.join();
            }
            // Dropping our last handles is what produces the EOF.
            self.conns.lock().unwrap().clear();
            if let Some(handle) = self.accept_thread.take() {
                let _ = handle.join();
            }
            self.listener.take();
        }

        /// Hang up, then come back to life at the **same path** with the same
        /// recorded command log.
        ///
        /// This exists because `Player::stop` is faithful about cleanup: its last
        /// act is `remove_file(socket_path)` (`macast_renderer/mpv.py`'s
        /// `os.remove(self.mpv_sock)` — see
        /// `stop_runs_in_the_references_order_and_removes_the_socket`), so after a
        /// `reload()` the fake's socket **file** is gone while its old
        /// `UnixListener` still holds the unlinked inode. A restarted mpv would have
        /// created that file again, and this is the half of restarting mpv the
        /// fixture owes us.
        ///
        /// The player never gives up waiting for it: `error_time` lives in the
        /// *player* thread and is only spent when the child exits (our harness child
        /// is `exec sleep 30`, which does not), and the ipc loop retries `connect`
        /// on every `timings.connect` with no budget at all. So a test that wants a
        /// second session needs no retry gymnastics — one `rebind()` and the next
        /// connect turn lands on a live listener.
        fn rebind(&mut self) {
            self.hang_up();
            let _ = std::fs::remove_file(&self.path);
            let listener = UnixListener::bind(&self.path).expect("rebind fake mpv");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            self.listener = Some(listener);
            self.stop.store(false, Ordering::SeqCst);
            // Bound first: assigning straight into the field would borrow `self`
            // mutably for the place and immutably for the call.
            let handle = self.accept_loop();
            self.accept_thread = Some(handle);
        }
    }

    impl Drop for FakeMpv {
        fn drop(&mut self) {
            self.hang_up();
        }
    }

    // --- wiring -----------------------------------------------------------------

    struct Harness {
        /// Held for the struct's life so the directory (and the fake mpv's socket
        /// file inside it) outliving the player is a fact, not a hope. Named with a
        /// leading underscore because nothing reads it: everything reads the paths
        /// that were derived from it.
        /// Declared first, so it reaps the child before anything else in this
        /// struct is torn down. Rust drops fields in declaration order, and the
        /// reaping does not need the directory — but "the guard runs before the
        /// fake mpv hangs up and before the temp dir is removed" is the only order
        /// worth promising a reader, because it is the order a live session needs.
        _guard: Guard,
        _temp: Temp,
        host: Arc<TestHost>,
        fake: FakeMpv,
        player: Player,
        /// The pid, read before `stop()` can reap it and let `id()` fall to 0.
        pid: Option<u32>,
    }

    /// A player whose mpv is a sleeping shell script and whose socket is a fake.
    fn harness(config: impl FnOnce(&mut Config, &Temp)) -> Harness {
        harness_with(config, Arc::new(TestHost::new()))
    }

    fn harness_with(
        config: impl FnOnce(&mut Config, &Temp),
        host: Arc<TestHost>,
    ) -> Harness {
        let temp = Temp::new();
        let fake = FakeMpv::bind(&temp);
        let mut cfg = Config::default();
        cfg.ipc_socket = fake.path().to_string();
        cfg.mpv_path = temp.script("exec sleep 30");
        config(&mut cfg, &temp);
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        Harness {
            _guard: Guard(player.shared.clone()),
            _temp: temp,
            host,
            fake,
            player,
            pid: None,
        }
    }

    /// `Player` takes a `Host` **by value**, and the test wants the same host behind
    /// an `Arc` it can read afterwards. So the value it takes is that `Arc`, wrapped
    /// in a unit struct that forwards the trait — the recorder stays shared with the
    /// assertions instead of being cloned out from under them. (An `Arc<TestHost>`
    /// cannot be passed bare: `Host: 'static` rules out any wrapper that borrows one.)
    struct SharedHost(Arc<TestHost>);

    impl Host for SharedHost {
        fn publish(&self, event: &str, args: &[Value]) {
            self.0.publish(event, args)
        }
        fn stop_service(&self) {
            self.0.stop_service()
        }
        fn transport_state(&self) -> String {
            self.0.transport_state()
        }
        fn current_url(&self) -> String {
            self.0.current_url()
        }
        fn default_volume(&self) -> Value {
            self.0.default_volume()
        }
        fn set_state(&self, call: StateCall) {
            self.0.set_state(call)
        }
        fn log(&self, level: Level, text: &str) {
            self.0.log(level, text)
        }
        fn friendly_name(&self) -> String {
            self.0.friendly_name()
        }
        fn player_size(&self) -> Value {
            self.0.player_size()
        }
        fn current_position(&self) -> Option<String> {
            self.0.current_position()
        }
    }

    impl Harness {
        /// Wait for the session's **ten setup commands**, not just for the connection.
        ///
        /// `ipc_once_connected` is stored before `set_observe` runs (see
        /// `connect_once`), so the earlier, shorter wait let a test read the command
        /// log while the ten observations were still on their way. That is fine for a
        /// test that pushes an mpv line (`send_line` awaits the connection itself) and
        /// fatal for one that asserts an exact tail — the first of those would race
        /// the setup and report whichever subset had landed.
        fn start(&mut self) {
            self.player.start();
            wait_until("mpv connected", || self.player.ipc_once_connected());
            wait_until("the session's ten observations", || {
                self.fake.commands().len() >= 10
            });
            self.pid = self.player.player_pid();
        }

        /// `stop()` on its own thread, the fake mpv hung up once the player thread
        /// is finished, then the join. See the module doc: the fake listener is not
        /// the child, so nothing else would ever close the reader's socket.
        fn stop(&mut self) {
            let shared = self.player.shared.clone();
            let watched = shared.clone();
            let stopper = Builder::new()
                .name("TEST_STOP".to_string())
                .spawn(move || stop(&shared))
                .expect("spawn stop");
            wait_until("the player thread stopped", || {
                self.host.lines().iter().any(|text| text == "mpv stopped")
                    || guard(&watched.mpv_thread).is_none()
            });
            self.fake.hang_up();
            let _ = stopper.join();
        }

        /// Run one media-control call and hand back exactly the commands it wrote.
        ///
        /// The count is snapshotted *before* the call because the fake's log is
        /// append-only **across connections**: the ten observations from the session
        /// that connected first are still in there, and a `reload()` will add ten
        /// more. What every test on this surface wants is "what did that one call
        /// put on the socket", and slicing the tail is how it gets that without
        /// re-deriving the session's whole command list.
        fn sent(&self, expect: usize, action: impl FnOnce(&Player)) -> Vec<Value> {
            let before = self.fake.commands().len();
            action(&self.player);
            wait_until("the commands reach the socket", || {
                self.fake.commands().len() >= before + expect
            });
            self.fake.commands()[before..].to_vec()
        }

        /// Snapshot, reload, wait for the shutdown line, hang up the fake, wait for
        /// the unlink, rebind, wait for the restarted session's commands, tail back.
        ///
        /// The unlink cannot be waited for *before* the hang-up, and the reason is a
        /// ported fact rather than a fixture quirk: `Link::recv_into` blocks in `read`
        /// with **no timeout**, because the reference never calls `settimeout` on the
        /// ipc socket (`macast_renderer/mpv.py` reads it bare). So the reader thread
        /// only returns when the peer closes — and [`stop`] joins that thread before its
        /// last act, `remove_file(socket_path)`. A predicate that demanded the file be
        /// gone while the fake still held the connection open could never be satisfied:
        /// the first draft of this helper did exactly that, and all seven reload tests
        /// timed out at the same 5-second deadline.
        ///
        /// Both halves are still load-bearing, just in the honest order. The `["quit"]`
        /// line is what proves the reload's `stop()` reached the socket *before* we cut
        /// it; the file disappearing is what proves `stop()` finished, because a
        /// rebind before then would hand the fresh listener's file to the old session's
        /// unlink.
        fn reloaded(&mut self, expect: usize) -> Vec<Value> {
            let before = self.fake.commands().len();
            self.player.reload();
            wait_until("the reload's quit line", || {
                self.fake.commands().len() >= before + 1
            });
            // The EOF is what lets the reader return so `stop()` can run its unlink.
            self.fake.hang_up();
            wait_until("the old session's unlink", || {
                !Path::new(self.fake.path()).exists()
            });
            self.fake.rebind();
            wait_until("the restarted player writes its commands", || {
                self.fake.commands().len() >= before + expect
            });
            self.fake.commands()[before..].to_vec()
        }
    }

    /// The index of the first command that begins with this verb.
    ///
    /// Panics with the whole tail in the message: a reload that loses a row is a
    /// question about *which* row, and "position returned None" answers nothing.
    fn first_index_of(verb: &str, seen: &[Value]) -> usize {
        seen.iter()
            .position(|row| {
                row.as_array()
                    .map_or(false, |cmd| cmd.first() == Some(&json!(verb)))
            })
            .unwrap_or_else(|| panic!("no `{}` command in {:?}", verb, seen))
    }

    /// The ten commands a session writes as soon as it connects: eight observations,
    /// then the volume handoff, then the toast that announces it.
    ///
    /// Both of the last two rows are spelled as **text**, not recomputed from the
    /// product's own helpers: this function is the witness that a reload put the
    /// session back the way it started, and if it called `pyops::py_str` to build the
    /// toast it would agree with the implementation no matter what the implementation
    /// did. The default host reports volume `100` (see `TestHost::new`); the case where
    /// the default is a *string*, and the toast has to follow it, belongs to
    /// `a_string_default_volume_stays_a_string_everywhere`.
    fn session_setup() -> Vec<Value> {
        command::observe_all()
            .iter()
            .map(|cmd| json!(cmd))
            .chain(std::iter::once(json!([
                "set_property", "volume", 100
            ])))
            .chain(std::iter::once(json!([
                "show-text", "Volume: 100", 1000
            ])))
            .collect()
    }

    /// Reaps the child a test spawned if that test ends before `stop()` — because
    /// it went red partway through, or because it only inspects state. Most such
    /// children are `exec sleep 30`, so the leak is bounded and invisible for a
    /// minute; but `a_real_mpv_serves_the_socket_we_ask_for` spawns **real mpv**,
    /// and a red run of it is exactly AGENTS.md's 「被反复重启的 Macast 会攒播放器」
    /// — except the pile would belong to the test suite, not the app, and nothing
    /// writes a line explaining it.
    ///
    /// `running = false` comes first because the player loop respawns while it is
    /// set: reaping the child without it would just watch a new one appear. This
    /// deliberately does *not* run `stop()` — a panic already in flight must not be
    /// followed by a second, unbounded one from inside a destructor.
    struct Guard(Arc<Shared>);

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.running.store(false, Ordering::SeqCst);
            if let Some(mut child) = guard(&self.0.child).take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    // --- tests ------------------------------------------------------------------

    #[test]
    fn reference_timings_are_the_pythons_numbers() {
        assert_eq!(Timings::REFERENCE.connect, Duration::from_millis(500));
        assert_eq!(Timings::REFERENCE.retry, Duration::from_secs(1));
        assert_eq!(Timings::REFERENCE.stop_wait, Duration::from_secs(5));
    }

    #[test]
    fn the_socket_path_is_the_one_the_command_line_advertises() {
        // `__init__` makes the path once and both `--input-ipc-server` and
        // `socket.connect` read that one value.
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.ipc_socket = String::new();
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        let build = argv::build(player.config()).expect("default config builds");
        let advertised = build
            .argv
            .iter()
            .find(|a| a.starts_with("--input-ipc-server="))
            .expect("the ipc server option is on the command line")
            .trim_start_matches("--input-ipc-server=")
            .to_string();
        assert_eq!(advertised, player.socket_path());
        assert!(player.socket_path().starts_with("/tmp/macast_mpvsocket"));
        drop(temp);
    }

    #[test]
    fn connect_publishes_in_the_reference_order() {
        let mut h = harness(|_, _| {});
        h.start();
        let events = h.host.published();
        assert!(
            is_subsequence(
                &["mpv_start".to_string(), "mpvipc_start".to_string(), "renderer_start".to_string()],
                &events
            ),
            "events: {:?}",
            events
        );
        assert!(h.player.is_playing() == false);
        assert_eq!(h.host.count_containing("mpv ipc socket start connect"), 1);
        // `running = True` then the INFO line then the threads — the INFO line is
        // between the first state call and the first publish.
        assert_eq!(h.host.at(Level::Info).first().map(String::as_str), Some("starting mpv and mpv ipc"));
    }

    #[test]
    fn set_observe_is_ten_separate_commands_in_order() {
        let mut h = harness(|_, _| {});
        h.start();
        let seen = h.fake.commands();
        let expected: Vec<Value> = command::observe_all()
            .iter()
            .map(|c| json!(c))
            .chain(std::iter::once(json!(["set_property", "volume", 100])))
            .chain(std::iter::once(json!([
                "show-text",
                "Volume: 100",
                1000
            ])))
            .collect();
        assert_eq!(&seen[..10], &expected[..], "seen: {:?}", seen);
        // Each one logged its own DEBUG line, outside the lock.
        assert!(h.host.count_containing("send command: ") >= 10);
    }

    #[test]
    fn a_blocked_send_announces_itself_before_the_socket_is_free() {
        // `logger.debug('send command: ' + str(command))` sits **outside**
        // `with self.command_lock:` (`mpv.py:371-386`); the read of `ipc_sock` and
        // the `sendall` sit inside it. So that line is a statement of intent, not a
        // report that bytes went out — and it is still heard while another writer
        // owns the socket, which is the whole reason the log reads as a timeline of
        // what the user asked for rather than a timeline of what mpv received
        // (§4.10: this log is what gets pasted into a bug report).
        //
        // Neither half of that is free, and both are the kind of thing that stays
        // true by accident: moving the log inside the lock leaves every line in the
        // log identical and only changes *when* it appears, while deleting the lock
        // makes the announcement honest for the wrong reason. So the test owns the
        // lock itself and asks the two questions in order — the DEBUG line appears
        // while the socket is still busy, and the bytes are still not there.
        let mut h = harness(|_, _| {});
        h.start();
        wait_until("the ten setup commands", || h.fake.commands().len() >= 10);
        let before = h.fake.commands().len();

        let held = h.player.shared.clone();
        let blocker = guard(&held.command_lock);
        let shared = h.player.shared.clone();
        let sender = std::thread::spawn(move || {
            send_command(&shared, &[json!("get_property"), json!("volume")])
        });

        wait_until("the announcement", || {
            h.host
                .at(Level::Debug)
                .iter()
                .any(|l| l.starts_with("send command: ") && l.contains("get_property"))
        });
        // A loopback write lands in well under a millisecond, so a window this long
        // is a real chance to lose the lock's effect, not an excuse for it; 120 ms
        // is what the other settle checks in this module use.
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            h.fake.commands().len(),
            before,
            "the send went out while another writer held the lock"
        );

        drop(blocker);
        wait_until("the command reaches mpv", || {
            h.fake.commands().len() > before
        });
        assert!(sender.join().expect("the sender panicked"));
        assert_eq!(
            h.fake.commands()[before],
            json!(["get_property", "volume"]),
            "seen: {:?}",
            h.fake.commands()
        );
        h.stop();
    }

    #[test]
    fn a_string_default_volume_stays_a_string_everywhere() {
        // `Setting.get(PlayerDefaultVolume, 100)` hands the stored value straight to
        // the command *and* into `f'Volume: {data}'`.
        let host = Arc::new(TestHost::with_volume(json!("70")));
        let mut h = harness_with(|_, _| {}, host);
        h.start();
        let seen = h.fake.commands();
        assert_eq!(seen[8], json!(["set_property", "volume", "70"]));
        assert_eq!(
            seen[9],
            json!(["show-text", "Volume: 70", 1000]),
            "seen: {:?}",
            seen
        );
    }

    #[test]
    fn a_volume_reply_becomes_state_and_an_info_line() {
        let mut h = harness(|_, _| {});
        h.start();
        h.fake.send_line(r#"{"id":1,"error":"success","data":55}"#);
        wait_until("the volume state call", || {
            h.host.states().contains(&StateCall::Volume(55))
        });
        // `logger.info(res)` is `str()` of the decoded dict, in wire order.
        let info = h.host.at(Level::Info);
        assert!(
            info.iter()
                .any(|line| line.contains("'id': 1") && line.contains("'data': 55")),
            "info: {:?}",
            info
        );
    }

    #[test]
    fn an_end_file_error_reaches_the_notify_and_the_transport_state() {
        let mut h = harness(|_, _| {});
        h.start();
        h.fake
            .send_line(r#"{"event":"end-file","reason":"error","file_error":"no audio or video data played"}"#);
        wait_until("the transport error", || {
            h.host.states().contains(&StateCall::TransportError)
        });
        let events = h.host.events.lock().unwrap().clone();
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert!(
            is_subsequence(&["renderer_av_stop", "app_notify"], &names),
            "events: {:?}",
            names
        );
        let notify = events
            .iter()
            .find(|(e, _)| e == "app_notify")
            .map(|(_, a)| a.clone())
            .expect("the notify was published");
        assert_eq!(
            notify,
            vec![json!("File error"), json!("no audio or video data played")]
        );
    }

    #[test]
    fn a_track_list_reply_makes_both_state_calls() {
        let mut h = harness(|_, _| {});
        h.start();
        h.fake
            .send_line(r#"{"id":6,"error":"success","data":[{"type":"video","selected":true}]}"#);
        wait_until("both track calls", || {
            let states = h.host.states();
            states.contains(&StateCall::CurrentTrack(1))
                && states.contains(&StateCall::NumberOfTracks(1))
        });
        let states = h.host.states();
        let at = states
            .iter()
            .position(|s| *s == StateCall::CurrentTrack(1))
            .expect("CurrentTrack");
        assert_eq!(states[at + 1], StateCall::NumberOfTracks(1));
    }

    #[test]
    fn send_command_before_any_socket_names_the_none() {
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.ipc_socket = temp.path("nothing");
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        assert!(!player.send_command(&[json!("quit")]));
        // The reference logs `'sendCommand: ' + str(e)`, so the line is the prefix
        // *and* the exception text — the bare `NO_SOCKET` is only half of it, and
        // asserting half a line is how the prefix would quietly disappear.
        assert_eq!(
            host.at(Level::Error).first().map(String::as_str),
            Some(&*format!("sendCommand: {}", NO_SOCKET))
        );
        // And the *reason* there is a `sendall` in that sentence at all: the slot is
        // `None`, so this is the never-created socket, not an unconnected one.
        assert!(host.at(Level::Error)[0].contains("NoneType"));
    }

    #[test]
    fn a_failed_connect_leaves_an_unconnected_socket() {
        // `self.ipc_sock = socket.socket(...)` is assigned **before** `.connect()`,
        // so the follow-up sentence is `[Errno 57]`, not the `AttributeError`.
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.ipc_socket = temp.path("nobody-listens-here");
        cfg.mpv_path = temp.script("exec sleep 30");
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        player.start();
        wait_until("a reconnect line", || {
            host.count_containing("mpv ipc socket reconnecting") >= 1
        });
        assert!(!player.send_command(&[json!("get_property"), json!("volume")]));
        let errors = host.at(Level::Error);
        assert!(
            errors
                .iter()
                .any(|line| line.ends_with("[Errno 57] Socket is not connected")),
            "errors: {:?}",
            errors
        );
        let pid = player.player_pid();
        player.stop();
        if let Some(pid) = pid {
            wait_until("the child is gone", || unsafe {
                libc::kill(pid as i32, 0) != 0
            });
        }
    }

    #[test]
    fn a_closed_socket_says_bad_file_descriptor() {
        let mut h = harness(|_, _| {});
        h.start();
        // Exactly the call the reader makes when a session ends.
        if let Some(slot) = guard(&h.player.shared.link).as_ref() {
            slot.close();
        }
        assert!(!h.player.send_command(&[json!("quit")]));
        assert!(h
            .host
            .at(Level::Error)
            .iter()
            .any(|line| line.ends_with("[Errno 9] Bad file descriptor")));
    }

    #[test]
    fn the_session_that_ends_on_eof_closes_the_socket_it_left_behind() {
        // The test above closes the slot **by hand**, so it proves what a closed
        // socket says — not that the reader ever closes one. `mpv.py:436` does:
        // `self.ipc_sock.close()` sits between the inner loop's exit and the
        // 「mpv ipc stopped」 line. Without it the slot keeps a descriptor whose
        // peer is gone, and the control plane that arrives next hears either a
        // `[Errno 32] Broken pipe` (open socket, dead peer) or a write success it
        // cannot have.
        //
        // The shape has to be deterministic, and the reference supplies the
        // determinism: `Renderer.stop()` sets `running = False` **first**
        // (`macast/renderer.py`), so by the time mpv's death gives the reader its
        // EOF the outer loop's own condition is already false. That is why a real
        // stop never reconnects — and writing that state here reproduces it rather
        // than racing it. The alternative was a long `connect` timing to park the
        // next turn, which only widens the race instead of closing it.
        let mut h = harness(|_, _| {});
        h.start();
        h.player.shared.running.store(false, Ordering::SeqCst);
        h.fake.hang_up();
        wait_until("the session ended", || {
            h.host.count_containing("mpv ipc stopped") >= 1
        });

        assert!(!h.player.send_command(&[json!("quit")]));
        // Both halves of the line: the prefix the reference adds (`'sendCommand: ' +
        // str(e)`), and EBADF — the answer for a socket *we* closed.
        let errors = h.host.at(Level::Error);
        assert!(
            errors
                .iter()
                .any(|line| line.starts_with("sendCommand: ")
                    && line.ends_with("[Errno 9] Bad file descriptor")),
            "errors: {:?}",
            errors
        );
        // The reader left the loop instead of beginning another connect turn, so the
        // socket just read is the one the session ended on. (`ipc_running` is still
        // true: the reference clears it only after joining the player thread.)
        assert!(h.player.ipc_running());
        let connects = h.host.count_containing("mpv ipc socket start connect");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            h.host.count_containing("mpv ipc socket start connect"),
            connects,
            "the reader reconnected after the session it just ended"
        );
        h.stop();
    }

    #[test]
    fn the_retry_budget_is_three_and_then_the_application_is_told() {
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.ipc_socket = temp.path("nothing");
        cfg.mpv_path = "no-such-program-xyz-9182".to_string();
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        player.start();
        wait_until("stop_service", || {
            host.stopped.load(Ordering::SeqCst)
        });
        // The spawn error names the program, which a socket error never does.
        assert!(host
            .at(Level::Error)
            .iter()
            .any(|line| line
                == "[Errno 2] No such file or directory: 'no-such-program-xyz-9182'"));
        let restarting = host.count_containing("mpv restarting");
        assert_eq!(restarting, 3, "lines: {:?}", host.lines());
        assert!(host.at(Level::Error).iter().any(|l| l == "mpv cannot start"));
        let events = host.events.lock().unwrap().clone();
        assert!(events
            .iter()
            .any(|(e, a)| e == "app_notify" && a == &vec![json!("Macast"), json!("MPV Can't start")]));
        // The reader ends with the player thread and never reaches 「mpv ipc
        // stopped」: that line sits at the bottom of the outer loop body, and the
        // loop condition sent the thread out of it instead.
        assert_eq!(host.count_containing("mpv ipc stopped"), 0);
        // The reader may finish the connect turn it is already inside, but it
        // never begins another one once the player thread is gone. A single
        // before/after comparison cannot say that: the reference checks
        // `mpv_thread.is_alive()` at the top of a turn, so one in-flight turn
        // legitimately lands after `stop_service()`. What is claimable is that
        // the count *settles* — two consecutive samples that agree — and that
        // the reader did connect at least once, so the settle is not vacuous.
        let connects = host.count_containing("mpv ipc socket start connect");
        assert!(connects >= 1, "the reader never tried: {:?}", host.lines());
        let mut last = connects;
        let mut stable = 0usize;
        wait_until("the reader stops reconnecting", || {
            std::thread::sleep(Duration::from_millis(120));
            let now = host.count_containing("mpv ipc socket start connect");
            stable = if now == last { stable + 1 } else { 0 };
            last = now;
            stable >= 2
        });
        player.stop();
    }

    #[test]
    fn a_child_that_died_after_a_connection_does_not_spend_the_budget() {
        // `if self.running and not self.ipc_once_connected:` (`mpv.py:541`) is the
        // only reason a player that got as far as an IPC socket never hears the
        // three-attempt story. Once a connection has been made, a dead child is
        // respawned with **no sleep and no 「mpv restarting」 line**, and the budget
        // is left where it was: `exec true` is not a way to end this thread, the
        // loop just goes around again, and 「MPV Can't start」 can only ever describe
        // a machine where mpv was never reachable at all.
        //
        // Nothing else in this module reaches that branch. Every other test either
        // never lets the child die (it is `exec sleep 30`) or never lets the socket
        // connect first (the budget test above), so deleting the `not` clause used
        // to be a change no test asked about.
        let mut h = harness(|_, _| {});
        h.start();
        // Two deaths, not three: with the clause gone the budget is exactly spent
        // after the third, so a three-round test fails in the *wait* for the next
        // respawn and the reader never sees the sentence about 「mpv restarting」.
        // Two rounds keep the loop alive under both readings, so the first thing
        // that goes red is the claim itself.
        for round in 1..=2 {
            let before = h
                .player
                .player_pid()
                .unwrap_or_else(|| panic!("no child before round {}", round));
            // SIGKILL because a crashing mpv does not get to choose how it goes.
            unsafe { libc::kill(before as i32, libc::SIGKILL) };
            // Wait for the respawn rather than for the log line, so the two
            // deaths cannot be collapsed into one by a fast poll loop.
            wait_until(&format!("the child respawned after round {}", round), || {
                h.player.player_pid().is_some_and(|pid| pid != before)
            });
        }
        // Two exits really happened, or the assertions below are vacuous: that
        // INFO line is the reference's own, written once per loop turn.
        assert_eq!(
            h.host.count_containing("mpv stopped"),
            2,
            "lines: {:?}",
            h.host.lines()
        );
        assert_eq!(
            h.host.count_containing("mpv restarting"),
            0,
            "the retry budget was charged for a player that had already connected"
        );
        // And the budget is still unspent, so the application hears nothing and a
        // third child is sitting in the slot: both of those are the tail of the
        // loop, not the branch under test.
        assert!(
            !h.host.stopped.load(Ordering::SeqCst),
            "the service was told to stop by a player that is still playing"
        );
        assert!(
            !h.host.at(Level::Error).iter().any(|l| l == "mpv cannot start"),
            "errors: {:?}",
            h.host.at(Level::Error)
        );
        assert!(h.player.player_pid().is_some(), "the loop gave up");
        h.stop();
    }

    #[test]
    fn a_knob_that_raises_never_reaches_a_spawn() {
        // `build_mpv_params()` is outside `start_mpv`'s try: the thread dies with
        // one traceback and the rest of the function is simply not executed.
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.ipc_socket = temp.path("nothing");
        cfg.position = json!("x");
        let player = Player::with_timings(SharedHost(host.clone()), cfg, quick());
        player.start();
        // Wait for the fact under test, not for "any error": the reader thread's
        // connect chatter is ERROR level too, so an empty-check would stop the
        // poll one turn early and the count below would depend on which thread
        // logged first.
        wait_until("the player thread's own line", || {
            host.at(Level::Error)
                .iter()
                .any(|line| line.starts_with("PlayerPosition"))
        });
        // ...then let the reader settle so its line count is not mid-flight.
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(host.states(), vec![StateCall::Speed(json!("1"))]);
        // The player thread answers with exactly one line, and that is the whole
        // claim. The reader thread is the second loop `start()` spawns, and the
        // reference starts it unconditionally: it gets its own
        // 「mpv ipc socket start connect」 plus a 「reconnecting: [Errno 2] …」
        // in before it notices at the top of a turn that the player thread is
        // gone. Filtering to the thread under test is the honest narrowing —
        // the reference's topology is not a second failure to explain.
        let player_errors: Vec<String> = host
            .at(Level::Error)
            .into_iter()
            .filter(|line| !line.starts_with("mpv ipc socket"))
            .collect();
        assert_eq!(player_errors.len(), 1, "lines: {:?}", host.lines());
        assert!(
            player_errors[0].starts_with("PlayerPosition is not an integer"),
            "{}",
            player_errors[0]
        );
        assert!(!host.lines().iter().any(|l| l == "mpv starting"));
        assert!(host.published().is_empty());
        assert!(!host.stopped.load(Ordering::SeqCst));
        assert_eq!(host.count_containing("mpv restarting"), 0);
        drop(player);
    }

    #[test]
    fn a_hw_knob_python_cannot_compare_is_a_warning_not_a_refusal() {
        // The reference takes neither branch, keeps `--hwdec=yes` and starts mpv.
        let mut h = harness(|cfg, _| cfg.hw = json!(null));
        h.start();
        assert!(!h.host.at(Level::Warning).is_empty());
        assert!(h
            .host
            .lines()
            .iter()
            .any(|l| l == "mpv starting"));
        assert!(h.host.published().contains(&"mpv_start".to_string()));
        h.stop();
    }

    #[test]
    fn stop_runs_in_the_references_order_and_removes_the_socket() {
        let mut h = harness(|_, _| {});
        h.start();
        let socket = PathBuf::from(h.player.socket_path().to_string());
        assert!(socket.exists(), "the fake mpv created it");
        let events_before = h.host.events.lock().unwrap().len();
        let logs_before = h.host.lines().len();
        h.stop();

        let events = h.host.events.lock().unwrap().clone();
        let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            names[events_before],
            "renderer_av_stop",
            "Renderer.stop() publishes before our own log line: {:?}",
            names
        );
        let after: Vec<String> = h.host.lines()[logs_before..].to_vec();
        let announcing = after.iter().position(|t| t == "stoping mpv and mpv ipc");
        assert_eq!(
            announcing,
            after.iter().position(|t| {
                t == "stoping mpv and mpv ipc" || t == "mpv stopped" || t == "mpv starting"
            }),
            "stop()'s own line is the first of the stop phase: {:?}",
            after
        );
        // And the player thread's own line comes after it, never before: `stop()`
        // writes its log line, then sends `['quit']`, then signals.
        let stopped = after.iter().position(|t| t == "mpv stopped");
        assert!(
            stopped.map_or(false, |i| Some(i) > announcing),
            "「mpv stopped」 must land after 「stoping mpv and mpv ipc」: {:?}",
            after
        );
        // The `['quit']` went out before anything was killed.
        assert!(h
            .fake
            .commands()
            .iter()
            .any(|c| c == &json!(["quit"])));
        assert!(
            !socket.exists(),
            "mpv never removes its own socket file; stop() does"
        );
        assert!(!h.player.is_running());
        assert!(!h.player.ipc_running());

        // A second stop() republishes and re-logs, because Python's does.
        let events_before = events.len();
        h.player.stop();
        let events = h.host.events.lock().unwrap().clone();
        assert_eq!(
            events[events_before].0,
            "renderer_av_stop",
            "the second stop() must not be swallowed"
        );
    }

    #[test]
    fn terminate_reaches_the_child() {
        let mut h = harness(|cfg, temp| {
            let witness = temp.path("term");
            cfg.mpv_path = temp.script(&format!(
                "trap 'echo TERM >> {}; exit 0' TERM\nwhile :; do sleep 0.2; done",
                shell_quote(&witness)
            ));
        });
        h.start();
        let socket = h.player.socket_path().to_string();
        h.stop();
        let witness = Path::new(&socket).parent().unwrap().join("term");
        wait_until("the trap ran", || {
            std::fs::read_to_string(&witness)
                .map(|text| text.contains("TERM"))
                .unwrap_or(false)
        });
    }

    #[test]
    fn kill_only_after_the_stop_wait() {
        let mut h = harness(|cfg, temp| {
            cfg.mpv_path = temp.script("trap '' TERM\nwhile :; do sleep 0.2; done");
            let _ = temp;
        });
        h.start();
        let pid = h.player.player_pid().expect("a child");
        let started = Instant::now();
        h.stop();
        // SIGTERM was ignored, so the wait timed out (300 ms in these timings) and
        // only then did the port fall through to SIGKILL.
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "stop() returned before the wait timed out: {:?}",
            started.elapsed()
        );
        wait_until("the child is gone", || {
            (unsafe { libc::kill(pid as i32, 0) }) != 0
        });
    }

    #[test]
    fn the_first_decode_failure_kills_the_reader_and_leaves_ipc_running() {
        // `msg` was never bound, so the second f-string in the `except` arm raises
        // inside the handler: one ERROR line, no close, no 「mpv ipc stopped」, and
        // `ipc_running` stays true.
        let mut h = harness(|_, _| {});
        h.start();
        h.fake.send_bytes(b"\xff\n");
        wait_until("one decode line", || {
            h.host
                .at(Level::Error)
                .iter()
                .any(|l| l.starts_with("decode error: "))
        });
        std::thread::sleep(Duration::from_millis(120));
        let decode_lines = h
            .host
            .at(Level::Error)
            .iter()
            .filter(|l| l.starts_with("decode error"))
            .count();
        assert_eq!(decode_lines, 1, "lines: {:?}", h.host.at(Level::Error));
        assert_eq!(h.host.count_containing("mpv ipc stopped"), 0);
        assert!(h.player.ipc_running());
        // And because it stayed running, a later start() hears the guard.
        let extra = h.player.shared.clone();
        run_ipc(extra);
        assert!(h
            .host
            .at(Level::Error)
            .iter()
            .any(|l| l == "mpv ipc is already runing"));
    }

    #[test]
    fn a_later_decode_failure_says_all_three_lines_and_keeps_reading() {
        let mut h = harness(|_, _| {});
        h.start();
        h.fake
            .send_line(r#"{"id":1,"error":"success","data":55}"#);
        wait_until("the first reply", || {
            h.host.states().contains(&StateCall::Volume(55))
        });
        h.fake.send_bytes(b"\xff\n");
        wait_until("three decode lines", || {
            h.host
                .at(Level::Error)
                .iter()
                .filter(|l| l.starts_with("decode error"))
                .count()
                >= 3
        });
        let lines = h.host.at(Level::Error);
        assert!(lines.iter().any(|l| l.starts_with("decode error data: ")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("decode error data list: ")));
        assert!(h.player.ipc_running());
        // The thread is still reading.
        h.fake
            .send_line(r#"{"id":1,"error":"success","data":42}"#);
        wait_until("the next reply", || {
            h.host.states().contains(&StateCall::Volume(42))
        });
    }

    #[test]
    fn a_chunk_without_a_newline_is_not_parsed() {
        let mut h = harness(|_, _| {});
        h.start();
        h.fake.send_bytes(br#"{"id":1,"error":"success","da"#);
        std::thread::sleep(Duration::from_millis(80));
        assert!(h.host.states().iter().all(|s| *s != StateCall::Volume(55)));
        h.fake.send_bytes(b"ta\":55}\n");
        wait_until("the joined line", || {
            h.host.states().contains(&StateCall::Volume(55))
        });
    }

    #[test]
    fn a_reconnect_drops_the_bytes_the_previous_session_left_half_read() {
        // `res = b''` (`macast_renderer/mpv.py:418`) runs after a *successful*
        // connect, so a fragment that arrived on the dead session never reaches the
        // next one. Without that reset the two halves join into one non-JSON line and
        // the reader reports a decode error instead of playing the reply.
        let mut h = harness(|_, _| {});
        h.start();
        h.fake
            .send_bytes(br#"{"id":1,"error":"success","da"#);
        std::thread::sleep(Duration::from_millis(80));
        h.fake.drop_connections();
        wait_until("the reader connects a second time", || {
            h.host.count_containing("mpv ipc socket start connect") >= 2
        });
        h.fake
            .send_line(r#"{"id":1,"error":"success","data":55}"#);
        wait_until("the reply on the new connection", || {
            h.host.states().contains(&StateCall::Volume(55))
        });
        std::thread::sleep(Duration::from_millis(120));
        let lines = h.host.at(Level::Error);
        assert!(
            !lines.iter().any(|l| l.starts_with("decode error")),
            "the old fragment was pasted in front of the new line: {:?}",
            lines
        );
        // And the EOF break did reach the close and its log line, inside the loop body.
        assert!(h.host.count_containing("mpv ipc stopped") >= 1);
    }

    #[test]
    fn a_reconnect_clears_the_batch_but_not_the_last_message() {
        // The reset on `macast_renderer/mpv.py:419` and the name that never gets one:
        // `msgs = None` at every connect, while `msg` outlives its own batch and
        // outlives the connection. So the first failure of the *new* session still
        // says the old session's last line — and it is a three-line story, not the
        // truncated one, because `msg` is bound.
        let mut h = harness(|_, _| {});
        h.start();
        let first = r#"{"id":1,"error":"success","data":55}"#;
        h.fake.send_line(first);
        wait_until("the first reply", || {
            h.host.states().contains(&StateCall::Volume(55))
        });
        h.fake.drop_connections();
        wait_until("the reader connects a second time", || {
            h.host.count_containing("mpv ipc socket start connect") >= 2
        });
        h.fake.send_bytes(b"\xff\n");
        wait_until("three decode lines", || {
            h.host
                .at(Level::Error)
                .iter()
                .filter(|l| l.starts_with("decode error"))
                .count()
                >= 3
        });
        let lines = h.host.at(Level::Error);
        assert!(
            lines
                .iter()
                .any(|l| l == &format!("decode error data: {}", first)),
            "the second session's failure must name the previous session's line: {:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l == "decode error data list: None"),
            "`msgs` is reset at connect, so this session has no batch to show: {:?}",
            lines
        );
        // The reader survived, and the session it reads is the new one.
        assert!(h.player.ipc_running());
        h.fake
            .send_line(r#"{"id":1,"error":"success","data":42}"#);
        wait_until("the next reply", || {
            h.host.states().contains(&StateCall::Volume(42))
        });
    }

    #[test]
    fn a_real_mpv_serves_the_socket_we_ask_for() {
        // Off by default: this spawns the player the users actually run.
        if std::env::var("MACAST_RUST_MPV_E2E").unwrap_or_default() != "1" {
            eprintln!("SKIPPED: set MACAST_RUST_MPV_E2E=1 to run this against real mpv");
            return;
        }
        let mpv = Path::new("/opt/homebrew/bin/mpv");
        if !mpv.exists() {
            eprintln!("SKIPPED: no mpv at {}", mpv.display());
            return;
        }
        let temp = Temp::new();
        let host = Arc::new(TestHost::new());
        let mut cfg = Config::default();
        cfg.mpv_path = mpv.to_string_lossy().to_string();
        cfg.ipc_socket = temp.path("s");
        let player = Player::with_timings(
            SharedHost(host.clone()),
            cfg,
            Timings {
                connect: Duration::from_millis(100),
                retry: Duration::from_millis(200),
                stop_wait: Duration::from_secs(2),
                poll: Duration::from_millis(20),
            },
        );
        // Real mpv, so a red run must not leave it playing: see `Guard`.
        let _guard = Guard(player.shared.clone());
        player.start();
        wait_until("real mpv accepted the ipc connection", || {
            player.ipc_once_connected()
        });
        wait_until("the observations were written", || {
            host.count_containing("send command: ") >= 10
        });
        let pid = player.player_pid().expect("mpv is our child");
        player.stop();
        assert!(
            !Path::new(player.socket_path()).exists(),
            "stop() removed the socket file mpv left behind"
        );
        wait_until("mpv is gone", || (unsafe {
            libc::kill(pid as i32, 0)
        }) != 0);
        assert_eq!(
            host.count_containing("mpv cannot start"),
            0,
            "lines: {:?}",
            host.lines()
        );
    }

    // --- real-player differential witnesses --------------------------------------
    //
    // Everything above is witnessed against a `/bin/sh` script that answers with the
    // bytes the test wrote into it. Five facts this port reproduces are not of that
    // kind: AGENTS.md §4.2's 「只有真机才教得了的 mpv 读取事实」 and §5's 「一次批量读取要按
    // `request_id` 是否凑齐来终止」 describe what a genuine player *says*. These ask the
    // real binary through the product's own seams and assert only what was measured on
    // this machine (mpv v0.41.0 at `/opt/homebrew/bin/mpv`). Gated on
    // `MACAST_RUST_MPV_E2E=1`; each prints `SKIPPED:` with its reason instead of
    // pretending when mpv or ffmpeg is not here.
    //
    // **Teeth, measured one mutation at a time** (each run was the whole crate, and each
    // file was restored and md5-verified before the count was read):
    //
    // | mutant | red |
    // |---|---|
    // | the settle-wait never waits | 1 — `ask_real`'s one-row-per-command count |
    // | one pinned live option wrong (`audio-buffer 0.0`→`0.1`) | 1 |
    // | the no-`data`-key assertion inverted | 1 |
    // | the expected `property unavailable` text changed | 1 |
    // | the fixture built under its requested extension-less name | 1 — and it prints ffmpeg's own refusal, which is the vacuous-green shape this block used to have |
    // | `set_media_url` stops passing the resume position | 4 — 3 unit + 「the player really sits at twelve seconds」 |
    // | expect a volume the machine never reported | 1 — times out at 30 s |
    // | **delete** `ask_real`'s count assertion | **0** |
    //
    // That last row is the honest one, and it is kept rather than removed: see
    // [`ask_real`].

    const REAL_MPV_TIMEOUT: Duration = Duration::from_secs(30);

    /// The reply rows the reader logged, in arrival order.
    ///
    /// `state.rs::apply`'s silent third branch is the only witness channel for
    /// anything read back from a real player: a reply we asked for is logged at
    /// **Debug** as the Python repr of a dict that always has an `error` key, an event
    /// is logged at **Info**, and a command we wrote is logged at Debug as
    /// `send command: […]`. The `"'error'"` needle is chosen against those Debug lines
    /// and not loosely: `send command: ['get_property', 'duration']` never has a quoted
    /// `error` in it, `update duration 0:00:20` has no quotes at all, and an event row's
    /// `'file_error'` has an underscore before `error` *and* is an Info line.
    fn reply_lines(host: &TestHost) -> Vec<String> {
        host.at(Level::Debug)
            .into_iter()
            .filter(|line| line.contains("'error'"))
            .collect()
    }

    /// Wait until the player has nothing more on the wire, and say how many replies
    /// that is.
    ///
    /// "Nothing more" is two 60 ms samples with the same count: real mpv flushes an ack
    /// asynchronously, and round 1's instrument read the *previous* entry's option
    /// values exactly this way (AGENTS.md §4.2 「等那个事实，而不是抓一瞬间」). A run that
    /// never settles is not a slow player but a broken witness, so it panics rather
    /// than handing back a count nobody can trust.
    fn wait_for_replies_to_settle(host: &TestHost) -> usize {
        let deadline = Instant::now() + REAL_MPV_TIMEOUT;
        let mut previous = reply_lines(host).len();
        let mut settled = 0;
        while settled < 2 {
            std::thread::sleep(Duration::from_millis(60));
            let now = reply_lines(host).len();
            if now == previous {
                settled += 1;
            } else {
                settled = 0;
                previous = now;
            }
            if Instant::now() > deadline {
                panic!(
                    "the player is still answering after {} seconds",
                    REAL_MPV_TIMEOUT.as_secs()
                );
            }
        }
        previous
    }

    /// Send one command to a running real player and hand back its reply row.
    ///
    /// `command::encode` never writes a `request_id`, so one connection carries one
    /// in-order stream: exactly one new row per send — asserted, not assumed. Round 1's
    /// instrument read one row too many because it never drained the ack for the
    /// command before it, and that is the failure this shape forbids.
    ///
    /// **The count and the settle-wait are load-bearing in one direction only, and that
    /// is measured, not assumed.** Deleting this `assert_eq!` is **0 red** on this
    /// machine: with the settle-wait in place nothing ever interleaves a second row, so
    /// no test can reach it. Deleting the *settle-wait* is 1 red and it is red **here**.
    /// So this line's job is not to catch today's timing — it is to make weakening that
    /// timing a red instead of a silently misread row, and an interleaved event
    /// (`file-loaded` landing between the settle and the reply) trips it too. Removing it
    /// would turn the first mutant green, which is why it stays despite the zero.
    fn ask_real(player: &Player, host: &TestHost, command: &Value) -> String {
        wait_for_replies_to_settle(host);
        let cmds = command.as_array().expect("a command array").clone();
        let before = reply_lines(host).len();
        assert!(
            player.send_command(&cmds),
            "mpv is not reading the socket: {:?}",
            command
        );
        let label = format!("a reply to {}", command::logged_command(&cmds));
        wait_until_within(&label, REAL_MPV_TIMEOUT, || {
            reply_lines(host).len() > before
        });
        let after = reply_lines(host);
        assert_eq!(
            after.len(),
            before + 1,
            "one reply per command, in order: {:?}",
            &after[before..]
        );
        after[before].clone()
    }

    /// One field out of a reply row, as Python prints it — quotes and all.
    ///
    /// **Scalar rows only**: a `loadfile` ack is
    /// `{'data': {'playlist_entry_id': 1}, 'error': 'success', …}` and that nested value
    /// contains `', '`, which is this function's terminator. Acks are counted, never
    /// parsed.
    fn reply_field(line: &str, key: &str) -> Option<String> {
        let marker = format!("'{}': ", key);
        let at = line.find(&marker)? + marker.len();
        let rest = &line[at..];
        let stop = match (rest.find(", '"), rest.find('}')) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => rest.len(),
        };
        Some(rest[..stop].to_string())
    }

    /// The `data` of a scalar reply row, as a number.
    fn reply_number(line: &str) -> f64 {
        let raw = reply_field(line, "data").unwrap_or_else(|| panic!("no data in {}", line));
        raw.trim_matches('\'')
            .parse::<f64>()
            .unwrap_or_else(|_| panic!("data is not a number: {}", line))
    }

    /// A real mpv, pointed at a private `MPV_HOME`, driven by the product's own
    /// [`Player`].
    ///
    /// - `MPV_HOME` is **inside the temp directory**: our argv carries
    ///   `--save-position-on-quit=yes` (`argv.rs`), so an unpinned home would write
    ///   `watch_later/` into the user's real `~/.config/mpv` — AGENTS.md §10's first
    ///   rule, and the one thing a differential test must not do to a sleeping machine.
    /// - It is set by `exec env MPV_HOME=…` inside a wrapper script rather than by
    ///   `std::env::set_var`, because these tests run concurrently in one process; and
    ///   `exec` makes the pid we reap *be* mpv instead of a shell.
    /// - `mpv.conf` asks for `vo=null` / `ao=null` / `force-window=no`: no window
    ///   appears while the user sleeps. Measured here, that home is paced in real time —
    ///   20.49 s of wall clock for a 20 s clip — so these witnesses do not race an
    ///   unthrottled player.
    /// - `platform` stays at [`Config`]'s default `linux` so `--ontop-level=system`
    ///   never enters the argv; the darwin shapes are already measured in `argv.rs`.
    ///
    /// Field order is the reaping order: `_guard` stops the child before `temp` removes
    /// the directory it was started from.
    struct RealMpv {
        _guard: Guard,
        host: Arc<TestHost>,
        player: Player,
        temp: Temp,
    }

    impl RealMpv {
        /// `Some(name)` builds a 20-second clip with that file name inside the temp
        /// directory (and loads nothing yet, so these witnesses each decide when the
        /// player starts); `None` leaves the player genuinely idle, which is what the
        /// batch-read witness needs.
        ///
        /// **Which absence is a skip and which is red.** `None` — a printed `SKIPPED:`
        /// and an `ok` — means only *this machine has no such tool*: the gate is off, or
        /// there is no mpv, or there is no ffmpeg to make a clip with. A tool that is
        /// **present and fails** panics. The difference is the whole point: a skip that
        /// comes from our own fixture being wrong is AGENTS.md §4.2's 「不提问的检查」
        /// wearing a green tick, and the first run of this harness produced exactly one
        /// of those (see the `clip.mp4` detour below).
        fn open(clip: Option<&str>) -> Option<RealMpv> {
            if std::env::var("MACAST_RUST_MPV_E2E").unwrap_or_default() != "1" {
                eprintln!("SKIPPED: set MACAST_RUST_MPV_E2E=1 to run this against real mpv");
                return None;
            }
            let mpv = Path::new("/opt/homebrew/bin/mpv");
            if !mpv.exists() {
                eprintln!("SKIPPED: no mpv at {}", mpv.display());
                return None;
            }
            let temp = Temp::new();
            if let Some(name) = clip {
                let ffmpeg = "/opt/homebrew/opt/ffmpeg/bin/ffmpeg";
                if !Path::new(ffmpeg).exists() {
                    eprintln!("SKIPPED: no ffmpeg at {} to make the clip", ffmpeg);
                    return None;
                }
                // ffmpeg picks its muxer from the output **extension**, and the whole point
                // of one witness below is a file with no extension at all — so everything is
                // built as `.mp4` and moved to the requested name afterwards. Asking ffmpeg
                // to write `/tmp/…/media` directly failed exactly the way it would on any
                // other machine ("use a standard extension for the filename"), and because
                // this `open` used to answer that with a skip, that witness printed `ok`
                // while asking nothing. See `open`'s own doc comment for that rule.
                let built = temp.path("clip.mp4");
                let made = std::process::Command::new(ffmpeg)
                    .args([
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=320x240:rate=25",
                        "-f",
                        "lavfi",
                        "-i",
                        "sine=frequency=440:sample_rate=48000",
                        "-t",
                        "20",
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-pix_fmt",
                        "yuv420p",
                        "-c:a",
                        "aac",
                        "-shortest",
                    ])
                    .arg(&built)
                    .output()
                    .expect("run ffmpeg");
                if !made.status.success() {
                    let tail = String::from_utf8_lossy(&made.stderr);
                    panic!(
                        "the fixture clip could not be made (this is the instrument, not \
                         mpv): {}",
                        &tail[tail.len().saturating_sub(300)..]
                    );
                }
                if name != "clip.mp4" {
                    std::fs::rename(&built, temp.path(name))
                        .expect("move the clip to the name the witness asks for");
                }
            }
            let home = temp.path("home");
            std::fs::create_dir_all(&home).expect("make the private MPV_HOME");
            std::fs::write(
                format!("{}/mpv.conf", home),
                "vo=null\nao=null\nforce-window=no\n",
            )
            .expect("write the private mpv.conf");
            let wrapper = temp.script(&format!("exec env MPV_HOME={} {} \"$@\"", home, mpv.display()));
            let host = Arc::new(TestHost::new());
            let player = Player::with_timings(
                SharedHost(host.clone()),
                Config {
                    mpv_path: wrapper,
                    ipc_socket: temp.path("s"),
                    ..Config::default()
                },
                Timings {
                    connect: Duration::from_millis(200),
                    retry: Duration::from_millis(400),
                    stop_wait: Duration::from_secs(2),
                    poll: Duration::from_millis(20),
                },
            );
            // A red run must not leave the user's player running: see `Guard`.
            let _guard = Guard(player.shared.clone());
            player.start();
            wait_until_within(
                "real mpv accepted the ipc connection",
                REAL_MPV_TIMEOUT,
                || player.ipc_once_connected(),
            );
            wait_until_within(
                "the ten setup commands were written",
                REAL_MPV_TIMEOUT,
                || host.count_containing("send command: ") >= 10,
            );
            wait_for_replies_to_settle(&host);
            Some(RealMpv {
                _guard,
                host,
                player,
                temp,
            })
        }

        fn file(&self, name: &str) -> String {
            self.temp.path(name)
        }

        /// How many times the player has said it loaded something. Every witness that
        /// reads a media-dependent property waits on this and not on a clock: the load
        /// is asynchronous, and a property read before it belongs to whatever was in
        /// the player before.
        fn loads(&self) -> usize {
            self.host.count_containing("'file-loaded'")
        }

        /// Stop through the product's own `stop()`, prove the socket file is gone and
        /// **our** child pid is reaped. The only pid this knows is the one `start()`
        /// recorded, so it can never touch the mpv children of the user's running app.
        fn shutdown(&self) {
            let pid = self.player.player_pid().expect("mpv is our child");
            self.player.stop();
            assert!(
                !Path::new(self.player.socket_path()).exists(),
                "stop() removed the socket file mpv left behind"
            );
            wait_until_within("mpv is gone", REAL_MPV_TIMEOUT, || (unsafe {
                libc::kill(pid as i32, 0)
            }) != 0);
            assert_eq!(
                self.host.count_containing("mpv cannot start"),
                0,
                "lines: {:?}",
                self.host.lines()
            );
        }
    }

    #[test]
    fn an_idle_player_answers_a_batch_read_the_way_the_reference_reads_it() {
        // AGENTS.md §5's 「一次批量读取要按 request_id 是否凑齐来终止」 is a rule about a
        // real shape: on a player with nothing loaded, four of these five properties
        // come back with **no `data` key at all** — which is precisely what
        // `state.rs::present(res, 'data')` reads — while the one the port can set
        // itself does answer. A reader that filtered on `error == 'success'` would wait
        // forever for the other four, and that is how round 1's instrument timed out.
        //
        // Ask for the **fields**, not a prefix. The first version of this witness pinned
        // `"{'error': 'property unavailable'…"` and real mpv v0.41.0 answered, verbatim,
        //
        //     {'request_id': 0, 'error': 'property unavailable'}
        //     {'data': '', 'request_id': 0, 'error': 'success'}
        //
        // — and note that those two shapes do not even share an order: `request_id` leads
        // the row that has no data, while `data` leads the one that does. `py_str` prints a
        // decoded reply in exactly the order the wire had (the workspace enables serde_json's
        // `preserve_order`), so no prefix could have been right for both, and nothing about
        // the reference's reading depends on the order anyway. The other measured fact in
        // those two rows is that `request_id` is there **even though we sent a bare array**
        // with no request id of our own — mpv supplies `0`.
        let Some(real) = RealMpv::open(None) else {
            return;
        };
        for name in ["media-title", "filename", "duration", "time-pos"] {
            let row = ask_real(&real.player, &real.host, &json!(["get_property", name]));
            assert_eq!(
                reply_field(&row, "error").as_deref(),
                Some("'property unavailable'"),
                "{} is the four that do not answer: {}",
                name,
                row
            );
            assert!(
                !row.contains("'data'"),
                "an unavailable property carries no data key, so `present(res, 'data')` \
                 must not be reached: {}",
                row
            );
        }
        let named = ask_real(
            &real.player,
            &real.host,
            &json!(["get_property", "force-media-title"]),
        );
        assert_eq!(
            reply_field(&named, "error").as_deref(),
            Some("'success'"),
            "force-media-title is the one that answers: {}",
            named
        );
        assert_eq!(
            reply_field(&named, "data").as_deref(),
            Some("''"),
            "and it answers empty, not missing: {}",
            named
        );
        real.shutdown();
    }

    #[test]
    fn a_position_in_the_fifth_slot_resumes_the_real_player() {
        // §4.2's 「`loadfile` 的第五格是选项字典，不是第三条字符串」, both halves: the
        // dictionary really resumes the clip, and the positional that tries to say the
        // same thing is mpv's playlist index instead.
        let Some(real) = RealMpv::open(Some("clip.mp4")) else {
            return;
        };
        let clip = real.file("clip.mp4");
        real.player.note_media_live(None);
        assert_eq!(
            real.player.media_live(),
            command::Liveness::Undeclared,
            "no declaration is a third thing, not `false`"
        );
        let before = real.loads();
        real.player.set_media_url(&clip, Some("12"));
        wait_until_within("the clip has loaded", REAL_MPV_TIMEOUT, || {
            real.loads() > before
        });
        let position = reply_number(&ask_real(
            &real.player,
            &real.host,
            &json!(["get_property", "time-pos"]),
        ));
        assert!(
            (11.5..20.0).contains(&position),
            "start=12 means the player sits at twelve seconds, not at zero: {}",
            position
        );
        let duration = reply_number(&ask_real(
            &real.player,
            &real.host,
            &json!(["get_property", "duration"]),
        ));
        assert!(
            (19.0..21.0).contains(&duration),
            "the clip is twenty seconds long: {}",
            duration
        );
        assert!(
            real.host.states().iter().any(|state| matches!(
                state,
                StateCall::Position(p) if p != "0:00:00"
            )),
            "the position update reached the application: {:?}",
            real.host.states()
        );
        let rejected = ask_real(
            &real.player,
            &real.host,
            &json!(["loadfile", clip.as_str(), "replace", "start=12"]),
        );
        assert!(
            rejected.contains("invalid parameter"),
            "a fourth positional is the playlist index, so it cannot hold a start time: {}",
            rejected
        );
        real.shutdown();
    }

    #[test]
    fn the_live_options_are_replaced_by_the_next_bare_load() {
        // §4.1's 「直播流被当成本地文件喂给播放器」 ends here: the ten options ride on the
        // `loadfile` that declares the media live, and the next bare load must not
        // inherit them. Nine of the ten are asked by name because that is how a reader
        // sees them; `demuxer-lavf-o-add` is deliberately **not** — it is a list of
        // appended strings whose printed shape I have not measured on this machine, and
        // asserting a guess would be a witness that lies.
        let Some(real) = RealMpv::open(Some("clip.mp4")) else {
            return;
        };
        let clip = real.file("clip.mp4");
        let read = |name: &str| -> String {
            let row = ask_real(&real.player, &real.host, &json!(["get_property", name]));
            reply_field(&row, "data").unwrap_or_else(|| {
                panic!("{} answered with no data key: {}", name, row)
            })
        };
        real.player.note_media_live(Some(true));
        assert_eq!(real.player.media_live(), command::Liveness::Live);
        let before = real.loads();
        real.player.set_media_url(&clip, None);
        wait_until_within("the live load has loaded", REAL_MPV_TIMEOUT, || {
            real.loads() > before
        });
        for (name, want) in [
            ("audio-buffer", "0.0"),
            ("vd-lavc-threads", "1"),
            ("cache-pause", "False"),
            ("demuxer-lavf-probe-info", "'nostreams'"),
            ("demuxer-lavf-analyzeduration", "0.1"),
            ("video-sync", "'audio'"),
            ("interpolation", "False"),
            ("video-latency-hacks", "True"),
            ("stream-buffer-size", "4096"),
        ] {
            assert_eq!(
                read(name),
                want,
                "{} while the media is declared live",
                name
            );
        }
        real.player.set_media_stop();
        assert_eq!(
            real.player.media_live(),
            command::Liveness::Undeclared,
            "the verdict is a statement about the media that was in the player"
        );
        let before = real.loads();
        real.player.set_media_url(&clip, None);
        wait_until_within("the second load has loaded", REAL_MPV_TIMEOUT, || {
            real.loads() > before
        });
        for (name, want) in [
            ("audio-buffer", "0.2"),
            ("cache-pause", "True"),
            ("stream-buffer-size", "131072"),
            ("vd-lavc-threads", "0"),
            ("demuxer-lavf-analyzeduration", "0.0"),
        ] {
            assert_eq!(
                read(name),
                want,
                "{} after a load that declared nothing",
                name
            );
        }
        real.shutdown();
    }

    #[test]
    fn a_real_property_change_carries_the_id_the_dispatcher_keys_on() {
        // `state.rs::apply` dispatches on `id` **before** it looks at `name`, and a stub
        // hands back whichever key the test wrote. So the eight observed ids
        // round-tripping through a genuine player is a fact only the player answers
        // for. Four of them are silent in the log (time-pos, mute, track-list, and the
        // two passthroughs write no line), which is why this is witnessed through the
        // state calls they must produce rather than through rows.
        let Some(real) = RealMpv::open(Some("clip.mp4")) else {
            return;
        };
        let seen = |want: StateCall| {
            let host = real.host.clone();
            let label = format!("the player reports {:?}", want);
            wait_until_within(&label, REAL_MPV_TIMEOUT, move || {
                host.states().contains(&want)
            });
        };
        // A fresh player's volume is already 100 and sub-visibility is already true, so
        // nothing would be announced unless the test *changes* those two; the reference's
        // defaults are the reason those values are not pinned at the top of the file.
        real.player.set_media_volume(&json!(63));
        seen(StateCall::Volume(63));
        real.player.set_media_mute(&json!(true));
        seen(StateCall::Mute(json!(true)));
        real.player.set_media_speed(&json!(1.5));
        seen(StateCall::Speed(json!(1.5)));
        real.player.set_media_sub_show(&json!(false));
        seen(StateCall::Subtitle(json!(false)));

        let clip = real.file("clip.mp4");
        let before = real.loads();
        real.player.set_media_url(&clip, None);
        wait_until_within("the clip has loaded", REAL_MPV_TIMEOUT, || {
            real.loads() > before
        });
        // duration (id 5) publishes unprompted; track-list (id 6) answers with a
        // current index and a count; time-pos (id 2) says the player is somewhere
        // other than the start.
        wait_until_within("a duration reached the bus", REAL_MPV_TIMEOUT, || {
            real.host
                .published()
                .contains(&"mpv_update_duration".to_string())
        });
        let states = real.host.states();
        assert!(
            states.contains(&StateCall::CurrentTrack(1)),
            "a loaded clip is track one of a list: {:?}",
            states
        );
        assert!(
            states
                .iter()
                .any(|state| matches!(state, StateCall::NumberOfTracks(_))),
            "the same row carried a count: {:?}",
            states
        );
        assert!(
            states.iter().any(|state| matches!(
                state,
                StateCall::Position(p) if p != "0:00:00"
            )),
            "time-pos reached the application: {:?}",
            states
        );
        // pause (id 3) answers only while the player is playing, which is `start-file`
        // that set it — so both halves of that arm need a real load to be reachable.
        real.player.set_media_pause();
        seen(StateCall::Pause);
        real.player.set_media_resume();
        seen(StateCall::Play);
        // 「Living media」 is **not** asserted here: [`TestHost`] pins
        // `transport_state` to `"STOPPED"`, and that arm is witnessed by state.rs's own
        // unit test, where the transport state is a fixture the test gets to choose.
        real.shutdown();
    }

    #[test]
    fn the_window_says_media_until_the_card_names_it() {
        // AGENTS.md §4.2's 「改播放器的地址，就必须改播放器的名字」 in the player's own
        // words: `filename` for an address whose last segment is `media` **is** `media`,
        // and `media-title` falls back to it, so the window reads `media - mpv` until
        // `force-media-title` says otherwise. The file here is named exactly that, for
        // the same reason `MEDIA_URL` is a `/relay/<id>/media` shape.
        let Some(real) = RealMpv::open(Some("media")) else {
            return;
        };
        let clip = real.file("media");
        let ask = |name: &str| -> String {
            let row = ask_real(&real.player, &real.host, &json!(["get_property", name]));
            reply_field(&row, "data").unwrap_or_else(|| {
                panic!("{} answered with no data key: {}", name, row)
            })
        };
        let before = real.loads();
        real.player.set_media_url(&clip, None);
        wait_until_within("the clip has loaded", REAL_MPV_TIMEOUT, || {
            real.loads() > before
        });
        assert_eq!(ask("filename"), "'media'");
        assert_eq!(ask("path"), format!("'{}'", clip));
        assert_eq!(ask("media-title"), "'media'");
        assert_eq!(ask("force-media-title"), "''");
        // Both properties, in that order — and the second one is the seam that reaches
        // the window, which is why `--title` is never written by this port.
        real.player.set_media_title("Never Gonna Give You Up - Rick Astley");
        wait_for_replies_to_settle(&real.host);
        assert_eq!(ask("filename"), "'media'", "the address did not move");
        assert_eq!(
            ask("path"),
            format!("'{}'", clip),
            "and neither did the stream the player holds"
        );
        assert_eq!(
            ask("media-title"),
            "'Never Gonna Give You Up - Rick Astley'"
        );
        assert_eq!(
            ask("force-media-title"),
            "'Never Gonna Give You Up - Rick Astley'"
        );
        real.shutdown();
    }

    // --- the media control surface ----------------------------------------------

    /// The address every hand-off test pushes.
    ///
    /// A `/relay/<id>/media` shape on purpose: that is the address whose last path
    /// segment is literally `media`, and therefore the one that made AGENTS.md §4.2's
    /// 「改播放器的地址，就必须改播放器的名字」 a bug report instead of a footnote.
    const MEDIA_URL: &str = "http://127.0.0.1:58880/relay/abc/media";

    /// The options dictionary `loadfile` shipped in its fifth slot, and the shape
    /// question that has to be asked **before** anything indexes it.
    ///
    /// `command::loadfile` has two shapes: the bare three-slot one when the player has
    /// no opinion, and the five-slot one when it does. A test that reaches for `[4]`
    /// before asking which shape arrived reports a panic that eats the rest of the
    /// assertion and, in this binary, every test after it — the exact failure AGENTS.md
    /// §4.2 records for the verify suite's Part 64, I arm. So the length question comes
    /// first, and it comes with the verb and url checked, because an options
    /// dictionary on some *other* command is not what this helper is for.
    fn loadfile_options(row: &Value, url: &str) -> serde_json::Map<String, Value> {
        let cmd = row
            .as_array()
            .unwrap_or_else(|| panic!("not a command array: {:?}", row));
        assert!(
            cmd.len() == 3 || cmd.len() == 5,
            "loadfile ships 3 slots bare or 5 with options: {:?}",
            cmd
        );
        assert_eq!(
            &cmd[..3],
            &[json!("loadfile"), json!(url), json!("replace")],
            "{:?}",
            cmd
        );
        assert_eq!(
            cmd.len(),
            5,
            "loadfile must carry an options dictionary as its fifth slot: {:?}",
            cmd
        );
        cmd[4]
            .as_object()
            .unwrap_or_else(|| panic!("the fifth slot is not an object: {:?}", cmd))
            .clone()
    }

    #[test]
    fn a_size_that_python_would_call_four_turns_the_window_fullscreen() {
        // `if self.setting.PlayerSize.value == 4:` compares against an enum member, so
        // `4.0` is four and the line goes out **first** — before the media, because a
        // window that is full-screen after the file loads still flashes a bordered
        // frame while it loads.
        let cases: Vec<(Value, Option<&str>, Vec<Value>)> = vec![
            (
                json!(4),
                None,
                vec![
                    json!(["set_property", "fullscreen", "yes"]),
                    json!(["loadfile", MEDIA_URL, "replace"]),
                ],
            ),
            (
                json!(4),
                Some("00:01:02"),
                vec![
                    json!(["set_property", "fullscreen", "yes"]),
                    json!(["loadfile", MEDIA_URL, "replace", 0, {"start": "00:01:02"}]),
                ],
            ),
            (
                json!(4.0),
                None,
                vec![
                    json!(["set_property", "fullscreen", "yes"]),
                    json!(["loadfile", MEDIA_URL, "replace"]),
                ],
            ),
        ];
        for (size, start, want) in cases {
            let mut h = harness_with(|_, _| {}, Arc::new(TestHost::at_size(size.clone())));
            h.start();
            let rows = h.sent(want.len(), |p| p.set_media_url(MEDIA_URL, start));
            assert_eq!(rows, want, "fullscreen has to be the first line out");
        }
    }

    #[test]
    fn a_size_of_four_written_as_text_is_not_four() {
        // `'4' == 4` is False in Python, and the port must not "helpfully" coerce the
        // advanced-settings JSON into a number: a user who typed `"4"` there did not
        // ask for a full-screen window on every cast.
        let mut h = harness_with(|_, _| {}, Arc::new(TestHost::at_size(json!("4"))));
        h.start();
        assert_eq!(
            h.sent(1, |p| p.set_media_url(MEDIA_URL, None)),
            vec![json!(["loadfile", MEDIA_URL, "replace"])],
            "a string size is not the enum member"
        );
    }

    #[test]
    fn the_handoff_reads_the_size_from_the_application_not_the_command_line() {
        // Two halves of one asymmetry, and it is deliberate: `mpv.py` reads
        // `self.setting.PlayerSize.value`, the live setting, so the menu changes the
        // next cast. `--player-size` on the argv is what the player process started
        // with, and a window size the user picks after Macast started would otherwise
        // take effect only after a restart.
        let mut h = harness(|cfg, _| cfg.size = json!(4));
        h.start();
        assert_eq!(
            h.sent(1, |p| p.set_media_url(MEDIA_URL, None)),
            vec![json!(["loadfile", MEDIA_URL, "replace"])],
            "the argv is not where the hand-off looks"
        );

        let mut h = harness_with(|cfg, _| cfg.size = json!(1), Arc::new(TestHost::at_size(json!(4))));
        h.start();
        assert_eq!(
            h.sent(2, |p| p.set_media_url(MEDIA_URL, None)),
            vec![
                json!(["set_property", "fullscreen", "yes"]),
                json!(["loadfile", MEDIA_URL, "replace"]),
            ],
            "and the setting is not decoration either"
        );
    }

    #[test]
    fn volume_goes_out_as_it_arrived_and_comes_back_as_python_would_str_it() {
        // The property carries the value unconverted; the toast is `f'Volume: {data}'`,
        // which is `str()`. `str(50)` is `50`, `str(50.0)` is `50.0`, and `str("50")`
        // is `50` — so `to_string()` on a `Value` gets the second one wrong, and the
        // wrong answer is on the user's screen for a second after every volume key.
        let rows = [
            (json!(50), "Volume: 50"),
            (json!(50.0), "Volume: 50.0"),
            (json!("50"), "Volume: 50"),
        ];
        for (given, toast) in rows {
            let mut h = harness(|_, _| {});
            h.start();
            let want = vec![
                json!(["set_property", "volume", given.clone()]),
                json!(["show-text", toast, 1000]),
            ];
            assert_eq!(
                h.sent(2, |p| p.set_media_volume(&given)),
                want,
                "{:?}",
                given
            );
        }
    }

    #[test]
    fn mute_is_a_yes_or_no_string_and_the_toast_names_the_value_it_was_given() {
        // Two independent readings of the same incoming value: the property is decided
        // by truthiness, the toast by `str()`. `1` is truthy and says `Mute: 1`; the
        // string `"false"` is truthy (Python: non-empty) and mutes.
        let cases = [(json!(1), "yes", "Mute: 1"), (json!("false"), "yes", "Mute: false")];
        for (given, want, toast) in cases {
            let mut h = harness(|_, _| {});
            h.start();
            assert_eq!(
                h.sent(2, |p| p.set_media_mute(&given)),
                vec![
                    json!(["set_property", "mute", want]),
                    json!(["show-text", toast, 1000]),
                ],
                "{:?}",
                given
            );
        }
    }

    #[test]
    fn the_values_python_calls_falsy_all_unmute() {
        // `false` too, but it is already covered by the type above; these five are the
        // shapes a hand-edited settings JSON or a caller with no schema actually
        // produces. A port that took a `bool` here could not answer for any of them.
        let cases = [
            (json!(0), "Mute: 0"),
            (json!(""), "Mute: "),
            (json!([]), "Mute: []"),
            (json!(null), "Mute: None"),
            (json!({}), "Mute: {}"),
        ];
        for (given, toast) in cases {
            let mut h = harness(|_, _| {});
            h.start();
            assert_eq!(
                h.sent(2, |p| p.set_media_mute(&given)),
                vec![
                    json!(["set_property", "mute", "no"]),
                    json!(["show-text", toast, 1000]),
                ],
                "{:?}",
                given
            );
        }
    }

    #[test]
    fn speed_and_subtitle_visibility_send_one_command_each() {
        // Neither has a toast. The reference sends the property and stops: mpv's own
        // OSC reports a speed change, and the subtitle toggle is visible on screen by
        // definition. Inventing a `show-text` here puts a line on the user's screen
        // that the build they are comparing against never put there.
        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(1, |p| p.set_media_speed(&json!(1.5))),
            vec![json!(["set_property", "speed", 1.5])]
        );

        for (given, want) in [(true, "yes"), (false, "no")] {
            let mut h = harness(|_, _| {});
            h.start();
            assert_eq!(
                h.sent(1, |p| p.set_media_sub_show(&json!(given))),
                vec![json!(["set_property", "sub-visibility", want])]
            );
        }
    }

    #[test]
    fn pause_and_resume_are_real_booleans_with_their_own_words() {
        // `pause` is the one property on this surface that mpv wants as a JSON bool
        // rather than the `"yes"`/`"no"` strings — and the two toasts are different
        // words, so a port that reused one message would say "Pause" while resuming.
        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(2, |p| p.set_media_pause()),
            vec![
                json!(["set_property", "pause", true]),
                json!(["show-text", "Pause", 1000]),
            ]
        );

        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(2, |p| p.set_media_resume()),
            vec![
                json!(["set_property", "pause", false]),
                json!(["show-text", "Resume", 1000]),
            ]
        );
    }

    #[test]
    fn a_message_keeps_the_duration_it_was_handed() {
        // The two entry points differ only in the third argument: `set_media_text`
        // passes the caller's duration, `set_media_message` uses the reference's
        // default of 1000 ms. A port that let one call the other with a fixed number
        // would silently shorten every "3.5 秒后继续" style countdown.
        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(1, |p| p.set_media_text("3.5 秒后继续", 2500)),
            vec![json!(["show-text", "3.5 秒后继续", 2500])]
        );
        assert_eq!(
            h.sent(1, |p| p.set_media_message("3.5 秒后继续")),
            vec![json!(["show-text", "3.5 秒后继续", 1000])]
        );
    }

    #[test]
    fn a_position_is_seeked_absolute_and_as_text() {
        // `absolute` is mpv's own word for "this is a timestamp, not an offset", and it
        // has to be present: the default is relative, so a control point that sends
        // `H:MM:SS` twice would walk forward instead of returning to the spot.
        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(1, |p| p.set_media_position("00:01:23")),
            vec![json!(["seek", "00:01:23", "absolute"])]
        );
    }

    #[test]
    fn a_title_is_written_to_both_properties_and_remembered() {
        // Three separate responsibilities in one call, and AGENTS.md §4.2 exists
        // because the middle one was once nobody's job: `title` alone only reaches the
        // OSC overlay, while the window's caption comes from `media-title`, which falls
        // back to `filename` — and for `/relay/<id>/media` that is the literal word
        // `media`. `--title` stays the user's option and this port never writes it.
        // The store is what lets `reload()` give the name back after mpv comes up fresh.
        let mut h = harness_with(|_, _| {}, Arc::new(TestHost::named("客厅的电视")));
        h.start();
        assert_eq!(
            h.player.title(),
            "客厅的电视",
            "before anyone names the media, the window carries the friendly name"
        );
        let rows = h.sent(2, |p| p.set_media_title("某部片子"));
        assert_eq!(
            rows,
            vec![
                json!(["set_property", "title", "某部片子"]),
                json!(["set_property", "force-media-title", "某部片子"]),
            ],
            "`title` alone only reaches the OSC overlay — AGENTS.md §4.2"
        );
        assert_eq!(h.player.title(), "某部片子");
        assert_eq!(
            h.host.count_containing("send command: "),
            12,
            "lines: {:?}",
            h.host.lines()
        );
    }

    #[test]
    fn a_subtitle_request_without_a_url_is_refused_without_a_word_on_the_socket() {
        // The reference reads `data['url']` and lets a `KeyError` escape to the bus that
        // logged it. This logs instead, and sends nothing — not even a `sub-add` with a
        // `null` in it, because then the user would read mpv's rejection instead of our
        // own missing key. A refusal is not a command: neither the wire nor the DEBUG
        // line may carry one, and the ten setup lines have to still be all there is.
        let refusals = [
            (
                json!({"title": "中文"}),
                "set_media_sub_file: url is missing",
            ),
            (
                json!({}),
                "set_media_sub_file: url is missing, title is missing",
            ),
        ];
        let mut h = harness(|_, _| {});
        h.start();
        for (given, message) in refusals {
            let before = h.fake.commands().len();
            h.player.set_media_sub_file(&given);
            std::thread::sleep(Duration::from_millis(120));
            assert_eq!(
                h.fake.commands().len(),
                before,
                "a refusal writes nothing: {:?}",
                given
            );
            assert!(
                h.host
                    .at(Level::Error)
                    .iter()
                    .any(|line| line.contains(message)),
                "refusal {:?} — lines: {:?}",
                message,
                h.host.lines()
            );
        }
        assert_eq!(
            h.host.count_containing("send command: "),
            10,
            "a refusal is not a command: {:?}",
            h.host.lines()
        );
    }

    #[test]
    fn a_subtitle_url_that_is_not_text_is_named_the_way_python_names_it() {
        // `url is int, title is list`, not "invalid type" and not a `TypeError` from
        // somewhere downstream: the reader of this line is a user who just pasted a
        // subtitle path into a field that took a number.
        let mut h = harness(|_, _| {});
        h.start();
        let given = json!({"url": 5, "title": []});
        let before = h.fake.commands().len();
        h.player.set_media_sub_file(&given);
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(h.fake.commands().len(), before, "{:?}", given);
        assert!(
            h.host
                .at(Level::Error)
                .iter()
                .any(|line| line.contains("set_media_sub_file: url is int, title is list")),
            "lines: {:?}",
            h.host.lines()
        );
    }

    #[test]
    fn a_subtitle_request_that_has_both_fields_selects_the_track() {
        // `select` in the middle is mpv's own track-selection argument, and the title
        // rides with it: two sidecar subtitles then land on the language the caller
        // named rather than whichever one the player picked.
        let mut h = harness(|_, _| {});
        h.start();
        assert_eq!(
            h.sent(1, |p| p
                .set_media_sub_file(&json!({"url": "/tmp/s.vtt", "title": "中文"}))),
            vec![json!(["sub-add", "/tmp/s.vtt", "select", "中文"])]
        );
    }

    #[test]
    fn a_declared_live_handoff_carries_the_ten_options_and_no_start() {
        // The one place the sender's own declaration reaches the player. The verdict is
        // stored, not sent — so nothing may leave the socket when only the verdict
        // changed — and then it lands in `loadfile`'s fifth slot as the ten live-shape
        // options, with **no** `start`: seeking a stream that has no addressable prefix
        // is how a live cast breaks.
        //
        // The assertions below are spot checks against literals, deliberately not a copy
        // of `command::live_load_options()`: `Value::Object` equality is order-insensitive,
        // so a wholesale compare would pass while the dictionary arrived with the wrong
        // insertion order (AGENTS.md §4.2 — mpv's option names are not a set). `command.rs`
        // owns the list; this test owns the route it takes to get here.
        let mut h = harness(|_, _| {});
        h.start();
        let before = h.fake.commands().len();
        h.player.note_media_live(Some(true));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            h.fake.commands().len(),
            before,
            "the verdict is a store, not a command"
        );
        assert_eq!(h.player.media_live(), command::Liveness::Live);
        let rows = h.sent(1, |p| p.set_media_url(MEDIA_URL, Some("00:00:12")));
        let options = loadfile_options(&rows[0], MEDIA_URL);
        assert_eq!(options.len(), 10, "{:?}", options);
        assert!(
            !options.contains_key("start"),
            "seeking a stream with no addressable prefix is how a live cast breaks: {:?}",
            options
        );
        assert_eq!(options["audio-buffer"], json!("0"));
        assert_eq!(options["cache-pause"], json!("no"));
        assert_eq!(options["stream-buffer-size"], json!("4k"));
    }

    #[test]
    fn a_handoff_nobody_declared_keeps_the_shapes_the_reference_shipped() {
        // Five rows, and the point is the split between `None` and `Some(false)` — three
        // slots with no dictionary at all when the player has no opinion, and a
        // one-entry dictionary only when a position actually arrived.
        //
        // `"0"` and `"  "` both mean "no position" because the builder trims and then
        // asks whether the text is literally `0`; `None` is the reference's own default
        // argument `"0"`, so the two spellings must agree. And `start` is suppressed
        // **only** for `Liveness::Live` — not for `Finite`, which is exactly the case
        // where resuming at a reported offset is the whole point.
        let rows: Vec<(Option<bool>, Option<&str>, Option<&str>)> = vec![
            (None, None, None),
            (None, Some("0"), None),
            (None, Some("  "), None),
            (None, Some("00:00:12"), Some("00:00:12")),
            (Some(false), Some("00:00:12"), Some("00:00:12")),
        ];
        let mut h = harness(|_, _| {});
        h.start();
        for (decl, start, expected) in rows {
            let sent = h.sent(1, |p| {
                p.note_media_live(decl);
                p.set_media_url(MEDIA_URL, start);
            });
            match expected {
                None => assert_eq!(
                    sent[0],
                    json!(["loadfile", MEDIA_URL, "replace"]),
                    "decl={:?} start={:?}",
                    decl,
                    start
                ),
                Some(position) => {
                    let options = loadfile_options(&sent[0], MEDIA_URL);
                    assert_eq!(options.len(), 1, "{:?}", options);
                    assert_eq!(options["start"], json!(position));
                }
            }
        }
    }

    #[test]
    fn the_three_verdicts_are_stored_as_the_three_verdicts() {
        // AGENTS.md §4.1: "keep today's behaviour" and "this is a file you may seek into"
        // are different statements, and only `Some(true)` changes the command that goes
        // out later. `loadfile` cannot witness that difference — `Undeclared` and `Finite`
        // produce byte-identical rows there — so the verdict as *stored* is the only
        // observable the split has. A `note_media_live(None)` that falls through to
        // `Some(false)` would pass every socket-shape test in this file.
        let h = harness(|_, _| {});
        for (declared, want) in [
            (Some(true), command::Liveness::Live),
            (Some(false), command::Liveness::Finite),
            (None, command::Liveness::Undeclared),
        ] {
            h.player.note_media_live(declared);
            assert_eq!(
                h.player.media_live(),
                want,
                "the sender said {:?} and the player must remember exactly that",
                declared
            );
        }
        // The fresh player reads `Undeclared` before anyone has said anything at all.
        assert_eq!(
            harness(|_, _| {}).player.media_live(),
            command::Liveness::Undeclared
        );
    }

    #[test]
    fn stopping_the_media_forgets_the_live_verdict() {
        // The verdict is a statement about *the media that was in the player*, so it
        // goes when the media does. Leave it and the next hand-off — arriving with no
        // declaration at all — inherits this one's live options and quietly plays an
        // ordinary file with no read-ahead, which is the buffering complaint the
        // declaration was introduced to answer.
        let mut h = harness(|_, _| {});
        h.start();
        h.player.note_media_live(Some(true));
        assert_eq!(h.player.media_live(), command::Liveness::Live);
        assert_eq!(
            h.sent(1, |p| p.set_media_stop()),
            vec![json!(["stop"])],
            "the one command on this surface that is not a `set_property`"
        );
        assert_eq!(h.player.media_live(), command::Liveness::Undeclared);
        let rows = h.sent(1, |p| p.set_media_url(MEDIA_URL, Some("00:00:12")));
        let options = loadfile_options(&rows[0], MEDIA_URL);
        assert_eq!(options.len(), 1, "{:?}", options);
        assert_eq!(options["start"], json!("00:00:12"));
    }

    // --- reload() ----------------------------------------------------------------

    #[test]
    fn a_reload_replays_the_media_that_was_playing() {
        // The tail is fourteen rows, and their order is the reference's: `quit`, the
        // media with its position, the name **twice**, then the ten setup commands.
        // The name comes back before the observations because `load_pending` runs
        // before `set_observe` on the new connection — and a window that got its media
        // but not its name looks exactly like the bug AGENTS.md §4.2 was written for.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing(MEDIA_URL, "00:00:12")),
        );
        h.start();
        let mut want = vec![
            json!(["quit"]),
            json!(["loadfile", MEDIA_URL, "replace", 0, {"start": "00:00:12"}]),
            json!(["set_property", "title", "Test Player"]),
            json!(["set_property", "force-media-title", "Test Player"]),
        ];
        want.extend(session_setup());
        assert_eq!(h.reloaded(14), want);
        h.stop();
    }

    #[test]
    fn a_reload_on_an_idle_player_loads_nothing() {
        // The reference's `else` branch: restart the window, do not resurrect media.
        // Eleven rows — `quit` and the ten setup commands — and no `loadfile` anywhere
        // in them, nor the DEBUG line that would announce one.
        let mut h = harness(|_, _| {});
        h.start();
        let tail = h.reloaded(11);
        assert_eq!(tail[0], json!(["quit"]));
        assert_eq!(&tail[1..], &session_setup()[..], "{:?}", tail);
        assert!(
            !tail
                .iter()
                .any(|row| row.as_array().map_or(false, |c| c.first()
                    == Some(&json!("loadfile")))),
            "{:?}",
            tail
        );
        assert_eq!(h.host.count_containing("mpv loadfile"), 0);
        h.stop();
    }

    #[test]
    fn the_replay_is_spent_once_the_session_has_read_it() {
        // A one-shot, proved the only way a one-shot can be proved: let a *later*
        // session read the pending slot and show that it has nothing left to say.
        //
        // The later session here is a third one, forced by hanging up on the second —
        // `TestHost` is immutable, so a second `reload()` would re-arm the same url and
        // prove nothing. The connection that follows the hang-up is a fresh
        // `connect_once`, so if the take were missing it would load the media again and
        // this assertion would see a `loadfile` inside those ten rows.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing(MEDIA_URL, "00:00:12")),
        );
        h.start();
        let tail = h.reloaded(14);
        assert_eq!(tail[1].as_array().unwrap()[0], json!("loadfile"));

        let before = h.fake.commands().len();
        h.fake.drop_connections();
        wait_until("a third session writes its own setup", || {
            h.fake.commands().len() >= before + 10
        });
        let added = h.fake.commands()[before..].to_vec();
        assert_eq!(&added[..10], &session_setup()[..], "{:?}", added);
        assert!(
            !added
                .iter()
                .any(|row| row.as_array().map_or(false, |c| c.first()
                    == Some(&json!("loadfile")))),
            "the replay is spent: {:?}",
            added
        );
        h.stop();
    }

    #[test]
    fn a_reload_without_a_readable_position_replays_the_start_of_the_media() {
        // `get_state_position()` can answer nothing at all — the control point has not
        // polled yet, or the media has no timeline. Then the hand-off is the bare
        // three-slot shape and mpv starts from zero, which is the reference's `else`.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing_without_position(MEDIA_URL)),
        );
        h.start();
        let tail = h.reloaded(14);
        assert_eq!(
            tail[1],
            json!(["loadfile", MEDIA_URL, "replace"]),
            "{:?}",
            tail
        );
        h.stop();
    }

    #[test]
    fn a_reload_does_not_ask_a_live_stream_for_an_offset() {
        // The verdict survives the restart, because `Player::stop()` deliberately does
        // **not** clear it — only `set_media_stop()` does, and a reload is not the media
        // ending. So the replay of a live stream carries the ten live options and no
        // `start`, exactly as the original hand-off did: the same rule that keeps a live
        // cast from seeking on the way in has to keep it from seeking on the way back.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing(MEDIA_URL, "00:00:12")),
        );
        h.start();
        h.player.note_media_live(Some(true));
        let tail = h.reloaded(14);
        let options = loadfile_options(&tail[1], MEDIA_URL);
        assert_eq!(options.len(), 10, "{:?}", options);
        assert!(
            !options.contains_key("start"),
            "a reload must not seek a live stream: {:?}",
            options
        );
        h.stop();
    }

    #[test]
    fn a_reload_gives_the_window_its_name_back() {
        // After the restart both properties are gone because mpv came back with its
        // default `--title` template, so the replay has to say the name again — and
        // say it after the media, because a title set before the load is what the load
        // overwrites. That ordering is the reason `title()` stores the text at all.
        //
        // The host has to be *playing* for this to have anything to check: `reload()`
        // arms the pending replay only when the transport says `PLAYING`, so a bare
        // harness would hand back the eleven-row idle tail and every index below
        // would read the session setup as if it were the replay.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing(MEDIA_URL, "00:00:12")),
        );
        h.start();
        h.sent(2, |p| p.set_media_title("某部片子"));
        let tail = h.reloaded(14);
        let load_at = first_index_of("loadfile", &tail);
        let title_at = first_index_of("set_property", &tail);
        assert!(
            load_at < title_at,
            "the name goes back after the media, not before: {:?}",
            tail
        );
        assert_eq!(tail[2], json!(["set_property", "title", "某部片子"]));
        assert_eq!(
            tail[3],
            json!(["set_property", "force-media-title", "某部片子"]),
            "{:?}",
            tail
        );
        assert_eq!(h.player.title(), "某部片子");
        h.stop();
    }

    #[test]
    fn a_reload_is_the_reference_two_publish_pairs_and_nothing_else() {
        // The bus traffic of one reload, in order: `renderer_av_stop` from `stop()`,
        // then `mpv_start` when the loop comes back, then `mpvipc_start` and
        // `renderer_start` from the new connection.
        //
        // This asserts a subsequence plus two exact counts rather than `len == 4`
        // on purpose: a state update that happens to be in flight is honest traffic and
        // must not make the test flaky, while a *third* lifecycle event — the service
        // being stopped, or the old session announced twice — is exactly the wrong
        // shape and is caught by the name filter and the counts.
        let mut h = harness_with(
            |_, _| {},
            Arc::new(TestHost::playing(MEDIA_URL, "00:00:12")),
        );
        h.start();
        let before = h.host.published().len();
        h.reloaded(14);
        std::thread::sleep(Duration::from_millis(150));
        let all = h.host.published();
        let added = &all[before..];
        assert!(
            added.iter().all(|name| {
                matches!(
                    name.as_str(),
                    "renderer_av_stop" | "mpv_start" | "mpvipc_start" | "renderer_start"
                )
            }),
            "a reload publishes lifecycle only: {:?}",
            added
        );
        assert!(
            is_subsequence(
                &[
                    "renderer_av_stop".to_string(),
                    "mpv_start".to_string(),
                    "mpvipc_start".to_string(),
                    "renderer_start".to_string()
                ],
                added
            ),
            "{:?}",
            added
        );
        assert_eq!(
            added
                .iter()
                .filter(|n| n.as_str() == "renderer_av_stop")
                .count(),
            1,
            "{:?}",
            added
        );
        assert_eq!(
            added.iter().filter(|n| n.as_str() == "mpv_start").count(),
            1,
            "the loop publishes mpv_start once per start(): {:?}",
            added
        );
        h.stop();
    }

    fn shell_quote(path: &str) -> String {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}
