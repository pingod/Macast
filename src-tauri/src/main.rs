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
//!
//! The web-address entry adds two things this file **owns** rather than
//! implements: the `SETTING_DIR/relay` directory with its store and accept
//! listener (`macast-cast/src/relay.rs`), and the resolve-job table
//! ([`host::ResolveJobs`]). They are built here because `Management` is the only
//! shape between the crates — `macast-cast` depends on `macast-upnp`, never the
//! reverse, so the store a viewer's `/relay/<id>/media` reads and the table a
//! `resolve-page` poll writes can only be held by the one crate that sees both
//! sides. The reference puts them in the same place for the same reason
//! (`macast/protocol.py:27` imports `media_relay`, not the other way round), and
//! it tears the relay down with the service, **first** and before the status
//! relabel (`macast/macast.py:1432-1439`) — because a remux of a live playlist is
//! an ffmpeg that never finishes writing to the user's disk.

mod host;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Manager, State};

use macast_cast::relay;
use macast_cast::setup;
use macast_cast::targets;
use macast_core::env;
use macast_core::logging::{self, Logger};
use macast_core::net;
use macast_core::paths;
use macast_core::setting::{application_port, https_enabled, https_port, Key, Setting};
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
///
/// `relay` is the fourth handle and it is not a service in the same sense: the
/// accept listener is already reachable by the time `Runtime` is built, and the
/// handle here exists for the one thing that has to happen on the way out. It is
/// the **store**, not the server, because `RelayServer` owns no stop handle
/// (`macast-cast/src/relay.rs` starts its accept loop and sweeper as detached
/// threads) while every `Relay::shutdown` the teardown needs lives behind
/// `RelayStore::clear`.
struct Runtime {
    player: Player,
    host: TauriHost,
    table: Arc<StateTable>,
    setting: Arc<Setting>,
    service_stopped: Arc<AtomicBool>,
    http: Arc<HttpServer>,
    ssdp: Arc<SsdpPlugin>,
    eventing: Arc<Eventing>,
    relay: Arc<relay::RelayStore>,
    /// The Chromecast setup face (8008, and 8443 when a certificate came up).
    /// `Option` because the plain listener may be refused (someone else holds
    /// 8008) and `SetupServer` itself already carries "is 8443 up" in
    /// `https_port`; the outer `Option` is only "did this run get a face at all".
    setup: Mutex<Option<setup::SetupServer>>,
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
    ///
    /// The relay goes **first**, ahead of even the `byebye`, and that is the
    /// reference's own order (`macast/macast.py:1432-1439` puts
    /// `media_relay.stop_server()` at the top of `service_stop`, before the status
    /// relabel, with the sentence 「a remux of a live HLS playlist is an ffmpeg
    /// that never finishes and a temp file that never stops growing」 under it).
    /// `RelayStore::clear` is the half of `stop_server` that carries that weight:
    /// it calls `Relay::shutdown` for every row, which kills the ffmpeg child and
    /// deletes its temp file.
    ///
    /// **The rest of `stop_server` has no Rust counterpart, and this is the place
    /// that says so.** Python then does `_server.shutdown(); _server.server_close()`
    /// and `store.stop_sweeper()` (`macast/media_relay.py:1120`); `RelayServer` here
    /// exposes no stop handle and `RelayStore` no `stop_sweeper`, so the accept loop
    /// and the sweep loop stay detached for the life of the process. That is safe
    /// for the reason Python's comment is not: both loops only ever touch the store
    /// this call has just emptied, and the process is exiting on the other side of
    /// the one caller below. What would **not** be safe is a service restart —
    /// `stop_services` would leave a listener on a port the next `RelayServer::start`
    /// would not know about — and this shell has exactly one call site, the window
    /// closing, so no path here restarts it.
    fn stop_services(&self) {
        self.relay.clear();
        self.ssdp.stop();
        self.http.stop();
        // The setup face closes with the other two HTTP faces rather than first:
        // a sender that already read `eureka_info` has our USN and 8009 for the
        // length of its cache, and there is no goodbye to send on this face — it
        // is a document, not an announcement. The SSDP `byebye` above is the one
        // that actually tells the LAN we are gone.
        if let Some(mut server) = self
            .setup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let port = server.port;
            server.shutdown();
            self.host
                .log
                .info(&format!("setup face on {port} closed; 8443 goes with it"));
        }
        self.eventing.stop();
        self.host.log.info(
            "receiver stopped (relay emptied, SSDP byebye, HTTP face closed, GENA thread \
             released); the player is untouched",
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

/// `query=status` (`macast/protocol.py:1998-2029`): the service's own identity,
/// which address the phone should reach, and which NIC is in play.
///
/// One deliberate difference from the reference. Python reports a single
/// `server.running`, because in that app CherryPy's engine is the only service
/// and "running" means "the engine started". Here the receiver is **three**
/// services with three independent lifecycles (the HTTP face, SSDP, GENA), and
/// the failure this page has to be able to show is exactly one of them having
/// died while the others answer — the reference's own history has two of those
/// (a GENA thread that stops draining the subscribe queue, §4.2 「订阅者必须
/// "没有状态变更也能入表"」, and an SSDP listener that is healthy-looking while
/// its receive thread is gone, §4.2 「SSDP 的定时重announce 要先问它还活着没」).
/// So `running` keeps the reference's meaning for the HTTP face, and each
/// service gets its own `running` / `healthy` pair **without** repeating the
/// port: `bound_port` is only reported once, on the face that owns it.
#[tauri::command]
fn service_snapshot(runtime: State<'_, Runtime>) -> Value {
    let setting = &runtime.setting;

    // `Setting.get(Macast_Protocols, []) or [Setting.get(Macast_Protocol, '')] if
    // Setting.has(Macast_Protocol) else []` — the legacy single value is only
    // consulted when the list is empty, and `has` gates the read so a legacy
    // install that never wrote the key is reported as "nothing configured"
    // rather than as a stored empty string. Note `get_str_list` reads through
    // `peek`, so unlike Python's `get` this does not insert `[]` into his table
    // just because the page asked what is in it.
    let mut protocols = setting.get_str_list(Key::MacastProtocols);
    if protocols.is_empty() && setting.has(Key::MacastProtocol) {
        protocols = vec![setting.get_str(Key::MacastProtocol, "")];
    }

    // Only the addresses a phone can actually reach. `Setting::get_ip()` also
    // lists VM bridges and Tailscale tunnels, which is noise here and actively
    // misleading when diagnosing "I can see it but casting fails" — the sender
    // may have picked one of the dead addresses.
    let advertised = net::advertised_addresses(setting);
    let (system, system_version) = paths::system_version();

    json!({
        "server": {
            "running": runtime.http.is_running(),
            "port": application_port(setting),
            "bound_port": runtime.http.bound_port(),
            "version": paths::version(),
            "friendly_name": setting.friendly_name(),
            "renderer": setting.get_str(Key::MacastRenderer, ""),
            "protocol": protocols,
            "platform": env::sys_platform(),
            "system": system,
            "system_version": system_version,
            "ip": advertised.join("/"),
            "https_enabled": https_enabled(setting),
            "https_port": if https_enabled(setting) { Some(https_port(setting)) } else { None },
            "service_stopped": runtime.service_stopped.load(Ordering::SeqCst),
        },
        "http": {
            "running": runtime.http.is_running(),
            "healthy": runtime.http.is_healthy(),
        },
        "ssdp": {
            "running": runtime.ssdp.ssdp().is_running(),
            "healthy": runtime.ssdp.ssdp().is_healthy(),
        },
        "eventing": {
            "running": runtime.eventing.is_running(),
            "thread_alive": runtime.eventing.event_thread_alive(),
        },
        "network": {
            // '' means automatic (the default-route interface).
            "interface": setting.get_str(Key::NetworkInterface, ""),
            "advertised": advertised,
            "interfaces": net::network_interfaces(setting),
        },
    })
}

/// Store the name phones and TVs show for this receiver.
///
/// The reference has **no** POST for this — 高级设置 writes the whole JSON blob —
/// so this is a widening, and it is deliberately narrow: one key, and it is the
/// key whose value the page also *reads back* (`service_snapshot`).
///
/// The name is stored exactly as typed. Python trims the interface name
/// ([`set_network_interface`]) but never the friendly name, and the difference
/// is a feature: a trailing space in a device name is what the user typed.
///
/// One thing does go stale: `Face::new` bakes the device description into a
/// `Vec<u8>` once at construction (`macast-upnp/src/description.rs` reads
/// `setting.friendly_name()` there), so the phone's `GET /description.xml` keeps
/// the old name until restart. `SsdpPlugin::update_ip()` is **not** the fix —
/// SSDP payloads carry the USN and the LOCATION, not the friend name. The reply
/// says so instead of pretending the change is fully live.
#[tauri::command]
fn set_friendly_name(runtime: State<'_, Runtime>, name: String) -> Value {
    runtime.setting.set(Key::DlnaFriendlyName, Value::String(name.clone()));
    json!({
        "code": 0,
        "friendly_name": runtime.setting.friendly_name(),
        "saved": name,
        "restart_pending": true,
    })
}

/// Pin (or clear, with `""`) the interface used for discovery, and re-advertise.
///
/// Ported from `_set_network_interface` (`macast/protocol.py:1977-1995`) with
/// its refusal intact: an unknown interface name is refused rather than stored,
/// because a typo would otherwise silently drop Macast to the loopback fallback
/// and make it invisible while looking like it was configured. The known set is
/// every row of the picker's table, usable or not — matching Python, which does
/// not filter, so that a down-but-real interface is not "not found".
///
/// The reference then publishes `network_interface_changed`, whose handler
/// (`macast/macast.py:1324-1330`) rebinds SSDP **and** cycles mDNS. Only the
/// SSDP half is portable here: mDNS belongs to the Chromecast and AirPlay
/// broadcasters, which are tasks #19 and #20. Until they exist, a phone looking
/// for `_googlecast._tcp` has nothing to see anyway.
#[tauri::command]
fn set_network_interface(runtime: State<'_, Runtime>, name: String) -> Value {
    let name = name.trim().to_string();
    if !name.is_empty() {
        let known: Vec<String> = {
            let mut names: Vec<String> = net::network_interfaces(&runtime.setting)
                .into_iter()
                .map(|row| row.name)
                .collect();
            names.sort();
            names
        };
        if let Some(refusal) = net::pin_refusal(&name, &known) {
            return json!({"code": 1, "message": refusal});
        }
    }

    runtime.setting.set(Key::NetworkInterface, Value::String(name.clone()));

    // The reference restarts the announcer unconditionally; here the restart is
    // asked for only when the announcer is up, because `update_ip()` stops and
    // starts it, and bringing SSDP up for a user who stopped the service would
    // be this command announcing something nobody asked it to announce.
    let rebound = runtime.ssdp.ssdp().is_running();
    if rebound {
        runtime.ssdp.update_ip();
    }

    json!({
        "code": 0,
        "message": format!("已切换到 {}", if name.is_empty() { "自动选择" } else { &name }),
        "interface": name,
        "ssdp_reannounced": rebound,
    })
}

/// Read one log the way the settings page asks for it: `query=log` with an
/// optional `module=`, `tail=` and `all=`.
///
/// No `State` parameter on this command and the two below it: the log surface
/// reaches the filesystem through `macast-core`, not through the running
/// services, and a `State<'_, Runtime>` we never read would be a parameter the
/// compiler has to be argued into tolerating.
///
/// An unknown module is the reference's zeroed answer with
/// `message: 'unknown log module'` (`protocol.py:2416-2431`), not an error: a
/// fresh install, a name typed into the wrong box, and a module that has since
/// been unloaded all arrive here, and the page renders `logs` either way.
#[tauri::command]
fn log_view(module: Option<String>, tail: Option<usize>, all: Option<bool>) -> Value {
    let want_all = all.unwrap_or(false);
    let lines = tail.unwrap_or(logging::DEFAULT_TAIL_LINES);
    match logging::read_log(module.as_deref(), lines, want_all) {
        Ok(payload) => json!({
            "code": 0,
            "logs": payload.logs,
            "truncated": payload.truncated,
            "lines": payload.lines,
            "size": payload.size,
            "module": payload.module,
        }),
        Err(name) => json!({
            "code": 1,
            "message": "unknown log module",
            "logs": "",
            "truncated": false,
            "lines": 0,
            "size": 0,
            "module": name,
        }),
    }
}

/// Empty one log, module-aware, exactly like `POST clear-log`
/// (`('clear-log', GATE_MANAGEMENT, '_post_clear_log')`, `protocol.py:2329` and
/// `3091-3098`): resolve the target, refuse an unknown name, then truncate the
/// file and its rotated siblings.
///
/// The failure line goes to `logging::root()` — the main log — rather than the
/// logger this shell uses for player traffic, because `host.log` claims the
/// per-module file and 「clear log error」 written there would hide the one fact
/// the log surface exists to report.
#[tauri::command]
fn clear_log(module: Option<String>) -> Value {
    let (path, _) = logging::log_target(module.as_deref());
    let Some(path) = path else {
        return json!({"code": 1, "message": "unknown log module"});
    };
    match logging::clear_log_file(&path) {
        Ok(removed) => json!({"code": 0, "message": "success", "removed": removed}),
        Err(e) => {
            logging::root().error(&format!("clear log error: {e}"));
            json!({"code": 1, "message": "clear failed"})
        }
    }
}

/// `query=log-modules` (`protocol.py:2116-2128`): the main log plus one row per
/// module file, so the page's log selector can show what is there before the
/// user clicks it.
///
/// Two documented differences from `logsplit.list_logs()`, both inherited from
/// `logging::module_logs()` rather than invented here: rotated `name.log.N`
/// backups are **not** listed (the page offers "the current MPV log", not three
/// generations of it), and rows come newest-first instead of sorted by name —
/// the picker wants the module that is doing something at the top.
///
/// `main` is `(0, 0)` when the file cannot be stat'ed, which is the reference's
/// `except OSError` branch: 「No log yet (fresh install, or it was just cleared)
/// is not an error」.
#[tauri::command]
fn log_modules() -> Value {
    let main = match fs::metadata(paths::log_file()) {
        Ok(meta) => json!({"size": meta.len(), "mtime": mtime_secs(&meta)}),
        Err(_) => json!({"size": 0, "mtime": 0}),
    };
    let modules: Vec<Value> = logging::module_logs()
        .into_iter()
        .map(|row| {
            json!({
                "name": row.name,
                "file": format!("{}.log", row.name),
                "size": row.bytes,
                "mtime": row.modified,
            })
        })
        .collect();
    json!({
        "main": main,
        "modules": modules,
        "claimed": logging::claimed_names(),
        "known": logging::known_log_modules(),
    })
}

/// `query=subscribers` (`protocol.py:2202-2238`): who is subscribed to our
/// events, and whether the machinery that lists them is still draining.
///
/// There is **one** child here. The reference loops its protocol group's
/// children, and in this port the only member of that group is the DLNA
/// receiver until tasks #19 and #20 add the others; the row is shaped exactly
/// like theirs so the second child costs nothing to add.
///
/// The two counters that matter are the reference's own, and both comments are
/// carried over because they are the reason the keys exist: a subscription that
/// was accepted but never published is a different failure from nobody
/// subscribing at all, and a non-zero pending queue means the event thread is
/// not draining — either it is gone, or it never ran.
#[tauri::command]
fn subscriber_snapshot(runtime: State<'_, Runtime>) -> Value {
    let rows = runtime.eventing.subscribers();
    let mut sids: Vec<String> = rows.iter().map(|r| r.sid.clone()).collect();
    sids.sort();

    json!({
        "aggregated": sids,
        "children": [{
            "title": "DLNA Protocol",
            "type": "DLNAProtocol",
            "running": runtime.eventing.is_running(),
            // Read together with `running`, never instead of it: the useful
            // state is the disagreement between the two.
            "event_thread_alive": runtime.eventing.event_thread_alive(),
            "subscribed": sids,
            "pending_append": runtime.eventing.pending_append(),
            "pending_state": runtime.eventing.pending_state(),
            // Not in the reference. A stuck reaper is the same shape of failure
            // as a stuck appender, and without this number "subscribers that
            // never go away" has no witness.
            "pending_remove": runtime.eventing.pending_remove(),
            "rows": rows
                .into_iter()
                .map(|row| {
                    json!({
                        "sid": row.sid,
                        "service": row.service,
                        "url": row.url,
                        "timeout": row.timeout,
                        "seq": row.seq,
                        "errors": row.errors,
                        "timed_out": row.timed_out,
                    })
                })
                .collect::<Vec<_>>(),
        }],
    })
}

/// A file's mtime as whole seconds, `0` when the platform will not say. The
/// reference's `os.stat(...).st_mtime` is a float; the page only ever shows it
/// as a date, so the fraction is not worth a drift-prone `as` on the field.
fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
            //
            // First, the two layers task #25 gives this file rather than a crate:
            // the relay store with its listener, and the resolve-job table. Both
            // are owned here because `Management` is the only shape that reaches
            // them — `macast-cast` depends on `macast-upnp`, never the reverse, so
            // the store a viewer's `/relay/<id>/media` reads cannot be named from
            // the crate that serves it.
            //
            // `setting_path("relay", true)` creates the **config directory** and
            // joins the name; the `relay/` subdirectory is not created by it, and
            // nothing in `relay.rs` creates it either (measured: no `create_dir` in
            // that file at all — `spawn_remux` writes straight into `temp_dir`).
            // Hence the explicit second call, which is AGENTS.md §4.2
            // 「写文件的代码要自己把目录建出来」 applied before it can hurt: the
            // Python side creates it in `relay_dir()` (`macast/media_relay.py:741`),
            // and on a first run here the symptom of skipping it would be every
            // transcode-refused candidate arriving as a spawn error.
            let relay_temp = paths::setting_path("relay", true)?;
            fs::create_dir_all(&relay_temp)?;
            // The two numbers are the reference's module constants, and they come
            // from `relay.rs` rather than being re-typed here: `RELAY_MAX` is the
            // eight concurrent relays, `RELAY_IDLE_TTL` the thirty idle minutes.
            let relay_store =
                Arc::new(relay::RelayStore::new(relay::RELAY_MAX, relay::RELAY_IDLE_TTL));
            // **Eagerly**, where the reference is lazy (`ensure_server` is called
            // from `media_url`): the bind is `0.0.0.0:0`, so an ephemeral port
            // cannot be "already taken" and a refusal here means this process
            // cannot make listening sockets at all — in which case `HttpServer` and
            // SSDP fail too, and refusing the window costs nothing a real cast would
            // have answered differently. Python goes lazy because its server is
            // restartable with the service; `stop_services` has exactly one call
            // site in this shell (the window closing), so that difference is
            // unobservable here. `start` also runs `sweep_orphans` **before** the
            // accept loop takes a connection, which is `ensure_server`'s order.
            let relay_server =
                relay::RelayServer::start(relay_store.clone(), relay_temp.clone())?;
            // The port the listener **actually** bound, and it is not the
            // application port: `cast_resolved` builds `/relay/<id>/media` with
            // this number, because the relay does not live on the HTTP face.
            let relay_port = relay_server.port;
            // The table `resolve_page` writes and `resolve-status` polls. Empty at
            // startup — the reference has no persistence either, which is why a
            // poll after a restart answers 「没有这个解析任务（超时或已重启）」.
            let resolve_jobs: host::ResolveJobs = Arc::new(Mutex::new(Vec::new()));
            // The device caches behind the settings page's dropdown, and the table a
            // device-named cast looks its id up in. Built once here because "what is
            // out there" is one fact with two readers (AGENTS.md §4.8's rule): a
            // second `Table` would let the card list a television that the push path
            // does not know about, which reads to the user as 「投了但什么都没播」.
            // `Table::new` searches for real; nothing is probed at construction —
            // the cold-start kick happens on the first `target_state()` poll, so a
            // shell that never opens the page never sends a multicast.
            let device_table = Arc::new(targets::Table::new(setting.clone()));
            let face = Face::new(
                Arc::new(soap) as Arc<dyn Control>,
                eventing.clone() as Arc<dyn Subscription>,
                // The GET web-cast entry's hand on the player: another face on the
                // same host + player pair, for the same reason `ShellRenderer` is —
                // `cast_uri` has one implementation and two callers (the page's
                // command and a `curl` on `/api?query=cast`). Since #25 it is also
                // the hand on the relay and the job table, because `resolve_page`
                // and `cast_resolved` are the same trait's other methods and the
                // page polls them through this one object.
                Arc::new(host::ShellManagement {
                    host: host.clone(),
                    player: player.clone(),
                    store: relay_store.clone(),
                    temp_dir: relay_temp,
                    port: relay_port,
                    jobs: Arc::clone(&resolve_jobs),
                    targets: device_table,
                }) as Arc<dyn macast_upnp::http::Management>,
                setting.clone(),
            )?;
            let http = HttpServer::new(Arc::new(face));
            // `host.clone()` rather than `host`: the last one moves into `Runtime`
            // below, and SSDP needs a `SharedHooks` of its own — the `notify` half is
            // how a bind failure reaches the page, and the `stop_service` half is how
            // it reaches *this* struct's `stop_services`.
            let ssdp = SsdpPlugin::new(Arc::new(host.clone()) as SharedHooks, setting.clone());

            // The Chromecast setup face (8008, plus 8443 when a certificate
            // exists). Its lines belong to the module's own log file: Python's
            // `protocol_cast.py:35` writes through `logging.getLogger("Chromecast")`,
            // which `logsplit` claims, so claiming here is what keeps §4.10b's
            // "one record, one file" true instead of dropping these rows into
            // `macast.log` beside the DLNA ones.
            let cast_log = Logger::named(setup::LOGGER_NAME);
            let _cast_log_path = cast_log.claim();
            // The certificate is the shared `macast.*` pair when one is already
            // there and our own otherwise (`ensure_in_setting_dir` owns that rule,
            // including "a broken foreign pair is reported by name and left
            // alone"). A pair we cannot load is not a reason to lose the plain
            // face, so the failure path hands `None` down and the module logs the
            // reference's own sentence about 8443.
            let setup_config = match macast_cast::cert::ensure_in_setting_dir(&cast_log) {
                Ok(pair) => match pair.server_config() {
                    Ok(config) => Some(config),
                    Err(e) => {
                        cast_log.warning(&format!(
                            "Cast HTTPS setup: the certificate will not load ({}); answering over 8008 only",
                            e.message()
                        ));
                        None
                    }
                },
                Err(e) => {
                    cast_log.warning(&format!(
                        "Cast HTTPS setup: no certificate available ({}); answering over 8008 only",
                        e.message()
                    ));
                    None
                }
            };
            // **Started unconditionally**, which is a gap this shell has already
            // and not one this face introduces: the DLNA receiver answers
            // regardless of `Macast_Protocols`, because the protocol enable list
            // is read for the status document only. When that list becomes a gate,
            // this line and the HTTP/SSDP starts above are the places that have to
            // read it — and until then a user who "disabled Chromecast" in the
            // page still gets a listener on 8008 answering what the device is.
            // What it answers is deliberately nothing secret: name, USN, version.
            let setup_server = match setup::SetupServer::start(
                setting.clone(),
                Arc::clone(&cast_log),
                setup_config,
            ) {
                Ok(server) => Some(server),
                Err(e) => {
                    cast_log.warning(&format!(
                        "Cast setup face did not start: {e} — senders that probe 8008 will get nothing"
                    ));
                    None
                }
            };
            // One summary line's worth of facts, taken **before** the handle moves
            // into the managed `Runtime`: the answer has to be the bound numbers,
            // and after the move there is nothing left here to ask.
            let setup_report = setup_server.as_ref().map(|face| {
                format!(
                    "HTTP on {}, HTTPS {}",
                    face.port,
                    match face.https_port {
                        Some(port) => port.to_string(),
                        None => "not started".to_string(),
                    }
                )
            });
            let setup = Mutex::new(setup_server);

            // Managed before anything is started, and this ordering is load-bearing
            // rather than tidy: `ssdp`'s bind failure spawns a thread that calls
            // `Hooks::stop_service`, which resolves the services through
            // `try_state::<Runtime>()`. Managed first, that call finds them and stops
            // the receiver; managed last, it finds nothing and the flag becomes the
            // only record of a request the user can see as "the phone found it and it
            // never answered".
            // The port is read off `relay_server` above and the value is then
            // dropped: `RelayServer` is a handle to nothing but its own fields, and
            // `std::thread::spawn` detaches the accept loop, so the listener outlives
            // it by design rather than by leak. The `Arc` the loop holds is
            // `relay_store` below.
            app.manage(Runtime {
                player: player.clone(),
                host,
                table,
                setting,
                service_stopped,
                http: http.clone(),
                ssdp: ssdp.clone(),
                eventing: eventing.clone(),
                relay: relay_store,
                setup,
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
            // Same rule for the setup face: report the bound number, not the
            // requested one, and say plainly when 8443 is not there — a user
            // comparing this line with a sender's "Failed to determine cast type"
            // warning is reading the two halves of one fact.
            match &setup_report {
                Some(text) => log.info(&format!("setup face: {text}")),
                None => log.info("setup face: not listening on 8008"),
            }
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
            service_snapshot,
            set_friendly_name,
            set_network_interface,
            log_view,
            clear_log,
            log_modules,
            subscriber_snapshot,
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
