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
pub trait Host: Send + Sync + 'static {
    fn publish(&self, event: &str, args: &[Value]);
    fn stop_service(&self);
    fn transport_state(&self) -> String;
    fn current_url(&self) -> String;
    fn default_volume(&self) -> Value;
    fn set_state(&self, call: StateCall);
    fn log(&self, level: Level, text: &str);
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
    pub fn with_timings(host: impl Host, mut config: Config, timings: Timings) -> Self {
        if config.ipc_socket.is_empty() {
            config.ipc_socket =
                command::ipc_socket_path(&config.platform, command::socket_random());
        }
        let socket_path = config.ipc_socket.clone();
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
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    pub fn socket_path(&self) -> &str {
        &self.shared.socket_path
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
    shared.host.publish("renderer_start", &[]);
    shared.ipc_once_connected.store(true, Ordering::SeqCst);
    set_observe(shared);
    feed.reconnect();
    Ok(())
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
fn set_observe(shared: &Arc<Shared>) {
    for observed in command::observe_all() {
        send_command(shared, &observed);
    }
    let volume = shared.host.default_volume();
    let text = format!("Volume: {}", pyops::py_str(&volume));
    send_command(shared, &command::volume(volume));
    send_command(shared, &command::show_text(&text, 1000));
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

    fn wait_until(label: &str, mut pred: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
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
    }

    impl TestHost {
        fn new() -> TestHost {
            TestHost {
                volume: json!(100),
                transport: "STOPPED".to_string(),
                url: String::new(),
                ..Default::default()
            }
        }

        fn with_volume(volume: Value) -> TestHost {
            TestHost {
                volume,
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

            let (l, conns2, readers2, commands2, stop2) = (
                listener.try_clone().expect("clone listener"),
                conns.clone(),
                readers.clone(),
                commands.clone(),
                stop.clone(),
            );
            let accept_thread = Builder::new()
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
                .expect("spawn fake accept");

            FakeMpv {
                path,
                listener: Some(listener),
                accept_thread: Some(accept_thread),
                conns,
                readers,
                commands,
                stop,
            }
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
    }

    impl Harness {
        fn start(&mut self) {
            self.player.start();
            wait_until("mpv connected", || self.player.ipc_once_connected());
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
        wait_until("ten commands", || h.fake.commands().len() >= 10);
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
        wait_until("ten commands", || h.fake.commands().len() >= 10);
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

    fn shell_quote(path: &str) -> String {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}
