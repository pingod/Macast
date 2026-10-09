//! The macOS half of the Rust port: a Tauri shell that owns the settings file,
//! the DLNA state table, and one [`macast_mpv::player::Player`], and hands the
//! player to the page through [`host`].
//!
//! Read [`host`] first: it is the `Host` implementation that
//! `macast-mpv/src/lib.rs` names as the missing piece, and every rule this file
//! applies was verified there against `macast/protocol.py` and
//! `macast_renderer/mpv.py`. This file's job is narrower — put the handles
//! together, start the player, start the receiver, and expose enough of both to
//! click on that a real mpv child appears and disappears and a real `NOTIFY`
//! arrives at a subscribed control point.
//!
//! What lives here now: the three services that make this shell a **discovery
//! target** as well as a player — the HTTP face (`description.xml`, SCPD, control
//! and GENA), the SSDP announcer, and the event thread — wired to the four
//! `macast-upnp` seams [`host`] implements. A phone's `DESCRIBE` is answered, and
//! a `SetAVTransportURI` reaches the same mpv `cast_url` drives, so the page and
//! the control point are two readers of one state table rather than two players.
//!
//! `cast_url` stays for the same reason it was written before the services
//! existed: it is the call `Handler.cast_uri` makes internally
//! (`macast/protocol.py:1163`), so the page can drive the exact sequence a DLNA
//! `SetAVTransportURI` would without waiting for a phone to find us. What is
//! still **not** here is Chromecast and AirPlay — those are different protocols,
//! not the same one wearing a different hat.

mod host;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Manager, State};

use macast_core::logging::Logger;
use macast_core::paths;
use macast_core::setting::{Key, Setting};
use macast_mpv::argv;
use macast_mpv::player::Player;
use macast_upnp::device::SsdpPlugin;
use macast_upnp::eventing::Eventing;
use macast_upnp::http::{Control, Face, HttpServer, Subscription};
use macast_upnp::scpd::Scpd;
use macast_upnp::soap::Soap;
use macast_upnp::store::{SharedBroadcast, SharedHooks, SharedRenderer, SharedStore};

use host::{StateTable, TauriHost};

/// `media_resolve.EXTRA_BIN_DIRS` (`macast/media_resolve.py`), reused for mpv for
/// the reference's own reason: 「A GUI app started from Finder/Dock does not
/// inherit the shell's PATH」. On this machine mpv is at
/// `/opt/homebrew/bin/mpv`, and Homebrew's `bin` is **not** in the default
/// launchd PATH, so a shell-PATH-only lookup would find nothing for the very
/// user this port is written for.
const EXTRA_BIN_DIRS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "~/.local/bin",
];

/// The app's mutable middle, managed once in `.setup()`.
///
/// `player` and `host` are two handles on the same `Arc<Shared>`
/// (`Player: Clone` and `TauriHost: Clone` are both shallow), and `table` /
/// `service_stopped` are the host's own handles copied out so the commands can
/// read them without going through a trait they have no reason to reach.
///
/// The three service handles are the receiver: [`HttpServer`] is the HTTP face
/// that answers `DESCRIBE`/`GET`/`POST`, [`SsdpPlugin`] is the announcer on
/// UDP/1900, [`Eventing`] is the GENA thread. They are `Arc`s rather than values
/// because both `start` methods take `&Arc<Self>` — the reference's services can
/// be started and stopped from inside their own worker threads, and an `Arc`
/// cloned into the thread is how that stays legal in Rust.
struct Runtime {
    player: Player,
    host: TauriHost,
    table: Arc<StateTable>,
    setting: Arc<Setting>,
    service_stopped: Arc<AtomicBool>,
    http: Arc<HttpServer>,
    ssdp: Arc<SsdpPlugin>,
    eventing: Arc<Eventing>,
}

impl Runtime {
    /// Stop the receiver and nothing else — the decision is
    /// [`host`]'s module doc, deviation number two: "the phone no longer finds
    /// Macast, the window it opened stays open."
    ///
    /// The order is the reference's teardown order for a service stop, and each
    /// step is a no-op when that service never came up, so calling this twice (a
    /// bind failure on `SSDP_STOP_THREAD` followed by the window closing) is
    /// harmless. `ssdp` goes **first** among the networked ones because its stop
    /// sends `byebye` — a control point that hears nothing at all keeps
    /// re-announcing a device that is not there for the length of its cache
    /// timeout, which is how a stopped Macast used to stay clickable on the TV
    /// for half a minute.
    fn stop_services(&self) {
        self.ssdp.stop();
        self.http.stop();
        self.eventing.stop();
        self.host.log.info(
            "receiver stopped (SSDP byebye, HTTP face closed, GENA thread released); \
             the player is untouched",
        );
    }
}

#[derive(Default)]
struct AppState {
    status: Mutex<String>,
}

#[derive(Serialize)]
struct RuntimeStatus {
    version: &'static str,
    backend: &'static str,
    migration: &'static str,
    status: String,
}

#[tauri::command]
fn runtime_status(state: State<'_, AppState>) -> RuntimeStatus {
    let status = state
        .status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "unavailable".to_string());

    RuntimeStatus {
        version: env!("CARGO_PKG_VERSION"),
        backend: "rust-tauri",
        migration: "parallel",
        status,
    }
}

#[tauri::command]
fn set_status(value: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut status = state
        .status
        .lock()
        .map_err(|_| "state lock poisoned".to_string())?;
    *status = value;
    Ok(())
}

/// `Handler.cast_uri` for the page: the full push sequence, in the reference's
/// order, ending with mpv holding the address **and the name**.
///
/// `title` is optional exactly where the reference's is: `cast_uri(uri, title)`
/// falls back to `media_resolve.title_for_uri(uri)` when the caller said nothing
/// (`macast/protocol.py:1183`). That fallback is not cosmetics — §4.2 records the
/// symptom (`media - mpv` in the window) and the rule 「改播放器的地址，就必须改
/// 播放器的名字」.
#[tauri::command]
fn cast_url(
    runtime: State<'_, Runtime>,
    uri: String,
    title: Option<String>,
) -> Result<Value, String> {
    host::cast_uri(
        &runtime.host,
        &runtime.player,
        &uri,
        title.as_deref().unwrap_or(""),
    )
}

/// Everything a reader can ask the player for without changing it: the transport
/// flags, the pid we own, the socket, the DLNA state table, the playlist and the
/// play history. The page polls this after each action rather than assuming its
/// call worked.
#[tauri::command]
fn player_snapshot(runtime: State<'_, Runtime>) -> Value {
    let player = &runtime.player;
    let mut states = serde_json::Map::new();
    for (name, value) in runtime.table.snapshot() {
        states.insert(name, value);
    }
    json!({
        "running": player.is_running(),
        "ipc_running": player.ipc_running(),
        "ipc_once_connected": player.ipc_once_connected(),
        "playing": player.is_playing(),
        "paused": player.is_pause(),
        "title": player.title(),
        "media_live": player.media_live().to_declaration(),
        "socket_path": player.socket_path(),
        "mpv_path": player.config().mpv_path,
        "pid": player.player_pid(),
        "service_stopped": runtime.service_stopped.load(Ordering::SeqCst),
        "friendly_name": runtime.setting.friendly_name(),
        "transport_state": runtime.table.read_str("TransportState"),
        "current_url": runtime.table.read_str("CurrentTrackURI"),
        "position": runtime.table.read_str("RelativeTimePosition"),
        "duration": runtime.table.read_str("CurrentTrackDuration"),
        "playlist": runtime.table.playlist(),
        // Newest first, deduped by address, capped at 50 — the table's own rule,
        // which `cast_uri` applies on every push, whether the push came from the
        // page or from a `SetAVTransportURI`. Each row is what a re-cast sends:
        // the name that was handed over with it (or the one the fallback derived),
        // so a history click re-plays under the same window title.
        "history": runtime
            .table
            .play_history()
            .iter()
            .map(|(title, uri)| json!({ "title": title, "url": uri }))
            .collect::<Vec<_>>(),
        "states": states,
    })
}

/// Empty the play history. The page's 「播放历史」 card has a clear button, and
/// this is the only way its row count can go down — nothing truncates the list
/// while playing except the 50-entry cap.
#[tauri::command]
fn clear_play_history(runtime: State<'_, Runtime>) -> Value {
    runtime.table.clear_history();
    json!({ "history": [] })
}

/// How long a control command waits for mpv to answer for itself before its ack
/// admits it could not confirm anything, and how often it asks in that window.
///
/// The wait exists because of **who writes the state table**. In the reference
/// `TransportState` and `Volume` are written only by the mpv IPC event thread:
/// `MPVRenderer.update_state()` (`macast_renderer/mpv.py:273-300`) turns a
/// `volume` property reply into `set_state_volume(int(res['data']))` and a
/// `pause` reply into `set_state_pause()` / `set_state_play()`, while the
/// control calls themselves (`macast_renderer/mpv.py:123-149`) only do
/// `send_command(...)` plus `set_media_text(...)` and never touch the table. The
/// port is the same on both sides of that seam (`macast-mpv/src/state.rs`
/// produces `Signal::Volume` / `Signal::Pause` / `Signal::Play`, and
/// `player.rs::deliver()` hands them to `Host::set_state`). So a read made in
/// the *same call* as the command is a read of the **pre-command** value, and an
/// ack that prints it as if it were the result is a witness that proves nothing —
/// which is exactly what the live page showed on 2026-10-07: `pause →
/// {"transport_state":"PLAYING"}` and `set_volume → {"Volume":100}` while mpv was
/// in fact paused at volume 80.
///
/// `settled:false` is therefore the honest answer when the change never arrives,
/// and it is not automatically a failure. The most common reason is the
/// reference's own guard — `if self.playing is False: return`
/// (`macast_renderer/mpv.py:291-292`, ported at `state.rs:220-225`) — i.e. a
/// player that has not announced `start-file` cannot be paused *into*, so
/// nothing should land in the table. The ack reports **whether it waited**, not
/// whether mpv obeyed; `player_snapshot` is still the place that says what the
/// player is doing.
const ACK_SETTLE: Duration = Duration::from_millis(400);
const ACK_POLL: Duration = Duration::from_millis(20);

/// Read `variable` until it satisfies `answered`, or the window runs out. Hands
/// back the value the table holds **last** plus whether the condition was ever
/// met, so a timeout still tells the reader what the state is now rather than
/// what it was when the command arrived.
fn settle(
    table: &StateTable,
    variable: &str,
    answered: impl Fn(&Value) -> bool,
) -> (Value, bool) {
    let deadline = Instant::now() + ACK_SETTLE;
    let current = loop {
        let current = table.read(variable).unwrap_or(Value::Null);
        if answered(&current) {
            break current;
        }
        if Instant::now() >= deadline {
            break current;
        }
        thread::sleep(ACK_POLL);
    };
    let settled = !matches!(current, Value::Null) && answered(&current);
    (current, settled)
}

/// The three transport commands share one ack shape: the state as a string
/// (matching `player_snapshot.transport_state`, which renders it the same way)
/// and whether the wait ended on the expected value.
fn transport_ack(table: &StateTable, expected: &str) -> Value {
    let want = json!(expected);
    let (state, settled) = settle(table, "TransportState", |value| *value == want);
    json!({
        "transport_state": state.as_str().unwrap_or_default(),
        "settled": settled,
    })
}

/// `MPVRenderer.set_media_stop`, then wait for mpv's `end-file`/`idle` reply to
/// say so (`state.rs` maps both to `Signal::Stop` → `TransportState = STOPPED`).
///
/// Both this command and the three below are `async` for one reason: Tauri runs a
/// non-`async` command on the **main thread**, so a 400 ms wait there would stall
/// the window behind every click. They take `tauri::AppHandle` rather than
/// `State<'_, Runtime>` for the same reason — an async command's future must be
/// `Send + 'static` (`tauri-2.12.1/src/ipc/command.rs:298`), so a state handle
/// borrowed from the IPC message cannot be a parameter; reaching through the
/// owned handle inside the body is the shape that resolves.
#[tauri::command]
async fn stop_playback(app: tauri::AppHandle) -> Value {
    let runtime = app.state::<Runtime>();
    runtime.player.set_media_stop();
    transport_ack(&runtime.table, "STOPPED")
}

/// `MPVRenderer.set_media_pause`: hand mpv `set_property pause yes` and then wait
/// for the observed `pause` reply to come back through the event thread.
#[tauri::command]
async fn pause(app: tauri::AppHandle) -> Value {
    let runtime = app.state::<Runtime>();
    runtime.player.set_media_pause();
    transport_ack(&runtime.table, "PAUSED_PLAYBACK")
}

/// `MPVRenderer.set_media_resume`, waiting for the `PLAYING` that only mpv can
/// report. An idle player answers `settled:false` for the same reason the
/// reference silently drops its pause event.
#[tauri::command]
async fn resume(app: tauri::AppHandle) -> Value {
    let runtime = app.state::<Runtime>();
    runtime.player.set_media_resume();
    transport_ack(&runtime.table, "PLAYING")
}

/// `MPVRenderer.set_media_volume`, with the reference's `set_state('Volume', int)`
/// ride-along. The value is handed over unguarded, like Python's, and the
/// state table keeps whatever arrives — clamping here would make this shell
/// disagree with what a real DLNA client reads back.
#[tauri::command]
async fn set_volume(app: tauri::AppHandle, value: i64) -> Value {
    let runtime = app.state::<Runtime>();
    runtime.player.set_media_volume(&json!(value));
    // Compare on the number, not on `json!(value) == json!(value)`: mpv reports
    // volume as a float and `as_int` (`state.rs:195`) is what turns it into the
    // integer the table holds, so the ack must not care which shape it arrives in.
    let (volume, settled) = settle(&runtime.table, "Volume", |v| {
        v.as_f64() == Some(value as f64)
    });
    json!({ "Volume": volume, "settled": settled })
}

/// Where mpv actually is, in the reference's priority order: an explicit
/// `MACAST_MPV`, then `PATH`, then the four directories a GUI launch cannot see.
///
/// The bare word `mpv` is still the answer when nothing is found, because that is
/// what `MPVRenderer.__init__`'s `path` default is — but it is a **warned**
/// fallback, so the log says why a spawn then fails with "No such file or
/// directory" instead of leaving the reader to guess.
fn resolve_mpv(log: &Logger) -> String {
    if let Some(given) = std::env::var("MACAST_MPV").ok().filter(|s| !s.is_empty()) {
        return given;
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join("mpv");
            if is_program(&candidate) {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    for dir in EXTRA_BIN_DIRS {
        // `~` is expanded here rather than in the table because that is what the
        // reference does: `media_resolve.py:100` builds the last entry with
        // `os.path.join(os.path.expanduser('~'), '.local', 'bin')` at import, so
        // the table it walks is four absolute directories.
        let dir = match dir.strip_prefix('~') {
            Some(rest) => match std::env::var("HOME") {
                Ok(home) => format!("{home}{rest}"),
                Err(_) => continue,
            },
            None => dir.to_string(),
        };
        let candidate = PathBuf::from(dir).join("mpv");
        if is_program(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    log.warning("no mpv found on PATH or in the usual install directories; passing the bare word mpv to spawn");
    "mpv".to_string()
}

/// `os.path.isfile(candidate) and os.access(candidate, os.X_OK)`
/// (`media_resolve.py:161`) — the two questions the reference asks of every
/// candidate, so a directory named `mpv` or a non-executable leftover cannot be
/// handed to spawn and produce an error that reads like a broken mpv.
fn is_program(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && executable(&meta)
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    true
}

/// The four knobs `build_mpv_params` reads **each time it is called**, taken from
/// the settings file here rather than baked into `Config::default()`.
///
/// The defaults are `argv::DEFAULT_*` so that a machine with no
/// `macast_setting.json` builds the same command line the reference would build
/// for a fresh install — and each read still goes through `Setting::get`, which
/// inserts the default on a miss. That side effect is `Setting`'s documented
/// behaviour (AGENTS.md §4.2) and reproducing it is the point, not an accident
/// to work around; the file it writes is this run's temp setting dir.
fn config_from_settings(setting: &Setting, log: &Logger) -> argv::Config {
    let scripts_dir = paths::base_path("scripts");
    argv::Config {
        mpv_path: resolve_mpv(log),
        // Empty means "ask `command::ipc_socket_path`", which for darwin makes a
        // path in /tmp — the namespace the user's running instance already owns.
        // A verification run must set MACAST_MPV_SOCKET; `main` warns when it is
        // absent instead of quietly picking a name out of that namespace.
        ipc_socket: std::env::var("MACAST_MPV_SOCKET").unwrap_or_default(),
        // Not `env::sys_platform()`'s choice to make: this shell only ever runs
        // the player on the machine it was built for, and `argv::Config::default`
        // is a deliberately non-darwin value so tests cannot become macOS-only.
        platform: "darwin".to_string(),
        ontop: setting.get(Key::PlayerOntop, json!(argv::DEFAULT_ONTOP)),
        position: setting.get(Key::PlayerPosition, json!(argv::DEFAULT_POSITION)),
        size: setting.get(Key::PlayerSize, json!(argv::DEFAULT_SIZE)),
        hw: setting.get(Key::PlayerHw, json!(argv::DEFAULT_HW)),
        scripts_dir: scripts_dir.to_string_lossy().into_owned(),
        scripts: host::lua_scripts(&scripts_dir),
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .manage(AppState {
            status: Mutex::new("scaffold".to_string()),
        })
        .setup(|app| {
            // A menu-bar receiver does not own a Dock tile, and this is not a
            // stylistic choice: the Python app's bundle says `LSUIElement = true`
            // (scripts/setup_py2app.py) and every mpv child it spawns runs with
            // `--macos-app-activation-policy=accessory`. `tauri.conf.json` declares
            // a normal window, so without this line every `macast-tauri` run is a
            // *foreground* app — and a bare Mach-O under `target/debug/` has no
            // bundle of its own, so LaunchServices attributes it to its responsible
            // process and paints the Dock with *that* app's icon. Verification runs
            // that start and kill this binary several times an hour therefore
            // accumulate tiles under somebody else's badge. Accessory is the same
            // state the shipped product is in; `set_activation_policy` (not
            // `set_dock_visibility`) because the latter goes through tao's
            // DockHide/DockShow path, which is the one that leaves stale tiles
            // behind when it cycles quickly (tao 0.37.1 dock.rs).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let handle = app.handle().clone();
            let log = Logger::named("MPV");
            // logs/MPV.log inside whatever MACAST_SETTING_DIR points at. Claiming
            // here, before the player's first line, is what keeps the renderer's
            // rows out of the shared macast.log — the same split the Python app
            // makes (macast/logsplit.py), and the reason `Host::log` exists.
            let log_path = log.claim();
            let setting = Arc::new(Setting::load_default());
            let table = Arc::new(StateTable::new(log.clone()));
            let service_stopped = Arc::new(AtomicBool::new(false));

            let config = config_from_settings(&setting, &log);
            if config.ipc_socket.is_empty() {
                log.warning(
                    "MACAST_MPV_SOCKET is unset: this run will make a /tmp/macast_mpvsocket* \
                     name, which is the namespace the Python app's children already live in",
                );
            }
            log.info(&format!(
                "mpv candidate: {} (socket {}, {} scripts dir {})",
                config.mpv_path,
                if config.ipc_socket.is_empty() {
                    "(auto)"
                } else {
                    config.ipc_socket.as_str()
                },
                config.scripts.len(),
                config.scripts_dir,
            ));

            let host = TauriHost {
                app: handle,
                setting: setting.clone(),
                table: table.clone(),
                log: log.clone(),
                service_stopped: service_stopped.clone(),
            };
            let player = Player::new(host.clone(), config);

            // The receiver, wired to the four seams [`host`] implements. All of it is
            // built **before** `player.start()` on purpose: every construction step
            // below can refuse (`Scpd::embedded`, `Face::new`), and a `?` that fires
            // after the mpv child exists would leave it orphaned — the exact shape
            // AGENTS.md §4.2 counted (「被反复重启的 Macast 会攒播放器」). Refusing
            // here costs a window, not a process.
            //
            // `Eventing` goes first because the other two take it as an argument: it
            // is the `StateStore`'s broadcast target (so a state write from the mpv
            // event thread reaches subscribed control points) and the `Subscription`
            // behind the HTTP face's SUBSCRIBE/UNSUBSCRIBE at the same time. One
            // object, both roles, because in the reference they are one thread.
            let eventing = Eventing::new(table.clone() as SharedStore);
            table.set_broadcaster(eventing.clone() as SharedBroadcast);
            let scpd = Arc::new(Scpd::embedded()?);
            let soap = Soap::new(
                scpd,
                table.clone() as SharedStore,
                // The renderer seam is the player plus nothing else: this shell has one
                // renderer, so there is no `ProtocolGroup` fan-out to reproduce here
                // (`macast/protocol_group.py`), and `ShellRenderer` is the single
                // delegate `Renderer::methods()` would have enumerated.
                Arc::new(host::ShellRenderer {
                    player: player.clone(),
                }) as SharedRenderer,
                eventing.clone() as SharedBroadcast,
                setting.clone(),
            );
            // `Face::new` reads and validates `description.xml` (it is what a phone
            // `DESCRIBE`s), so an `Err` here means "we would answer discovery with
            // garbage" and the face must not start. That is a build-time fact about
            // our own bundled XML, not a runtime condition.
            let face = Face::new(
                Arc::new(soap) as Arc<dyn Control>,
                eventing.clone() as Arc<dyn Subscription>,
                setting.clone(),
            )?;
            let http = HttpServer::new(Arc::new(face));
            // `host.clone()` rather than `host`: the last one moves into `Runtime`
            // below, and SSDP needs a `SharedHooks` of its own — the `notify` half is
            // how a bind failure reaches the page, and the `stop_service` half is how
            // it reaches *this* struct's `stop_services`.
            let ssdp = SsdpPlugin::new(Arc::new(host.clone()) as SharedHooks, setting.clone());

            // Managed before anything is started, and this ordering is load-bearing
            // rather than tidy: `ssdp`'s bind failure spawns a thread that calls
            // `Hooks::stop_service`, which resolves the services through
            // `try_state::<Runtime>()`. Managed first, that call finds them and stops
            // the receiver; managed last, it finds nothing and the flag becomes the
            // only record of a request the user can see as "the phone found it and it
            // never answered".
            app.manage(Runtime {
                player: player.clone(),
                host,
                table,
                setting,
                service_stopped,
                http: http.clone(),
                ssdp: ssdp.clone(),
                eventing: eventing.clone(),
            });

            player.start();
            http.start();
            ssdp.start();
            eventing.start();
            // The answer, not the ask: `HttpServer::start` can refuse (the port is
            // taken) and return, so "started" is only true once `is_running()` says
            // so — and a control point's whole experience of this app is that port.
            // SSDP's state is read through `ssdp()` because the plugin delegates to
            // the server it owns. The GENA thread is not reported: it has no query to
            // ask, and a line saying "started" for a thread that could have failed to
            // spawn is the kind of unasked claim this port keeps refusing.
            log.info(&format!(
                "receiver: HTTP {} on port {}, SSDP {}",
                if http.is_running() { "listening" } else { "did not start" },
                http.bound_port(),
                if ssdp.ssdp().is_running() { "announcing" } else { "did not start" },
            ));
            log.info(&format!("renderer log: {}", log_path.display()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            set_status,
            cast_url,
            player_snapshot,
            clear_play_history,
            stop_playback,
            pause,
            resume,
            set_volume,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Macast Tauri application");

    // `build` + `run` instead of `run` for one reason: the exit hook. An mpv we
    // spawned has to die with this window, because AGENTS.md §4.2 counted the
    // orphaned players this exact shape leaves behind (「被反复重启的 Macast 会攒
    // 播放器」). `RunEvent::Exit` is the only place that fires on every way out —
    // `on_window_event`/Destroyed misses a signal, and `Host::stop_service` is
    // not called by a quit.
    //
    // The receiver goes with it for the same reason in the other direction: a
    // process that quits while UDP/1900 still has our `ALIVE` in flight looks to
    // every control point on the network like a device that is there and silent,
    // so it stays in their caches for the length of the timeout. `stop_services`
    // sends the `byebye` that tells them otherwise, and it is a no-op for a
    // service that never came up — which is the state after a refused bind.
    app.run(move |app, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(runtime) = app.try_state::<Runtime>() {
                runtime.stop_services();
                runtime.player.stop();
                // `Logger`'s inherent method, not `Host::log`: the trait is not in
                // scope here, and the host's `log` field is the same logger the
                // player writes through, so this row lands in the same file.
                runtime.host.log.info("player stopped on exit");
            }
        }
    });
}

fn main() {
    run();
}
