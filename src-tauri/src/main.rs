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
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Manager, State};

use macast_cast::airplay;
use macast_cast::announce;
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
    /// The two discovery faces a 网卡 switch re-touches (SSDP and the announcer), plus
    /// the receivers the announcement is built from. Held as one handle because the
    /// management HTTP face holds the **same** `Arc` and must not have a second copy of
    /// "which interface is pinned, what is listening": see [`Discovery`].
    discovery: Arc<Discovery>,
    eventing: Arc<Eventing>,
    relay: Arc<relay::RelayStore>,
    /// The **same** management object the HTTP face holds: `resolve-page`,
    /// `resolve-status`, `cast-resolved` and `cast-targets` are one implementation with
    /// two doors (the page's four commands below, and `/api?query=…`). It also carries
    /// the one job table and the one device cache, so a parse started from the window is
    /// pollable by `curl` and vice versa — the reason this is a field rather than a
    /// second `Arc::new(ShellManagement{…})` next to the commands.
    management: Arc<dyn macast_upnp::http::Management>,
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
        self.discovery.stop_ssdp();
        self.http.stop();
        // The Cast receiver stops with the other listeners, and its slot in the
        // host is emptied on the way: `stop()` wipes the ledger, so a `StateCall`
        // fanned into a stopped channel would be updating a table that can never
        // be read again — and the day a restart path exists, the last thing that
        // should happen is a new session reading the previous one's `PLAYING`.
        // The announcement goes **before** the receiver's socket closes, which is
        // the reference's order (`protocol_cast.py:417-421` closes the advertiser
        // — and with it the goodbye — ahead of the connections) and the same
        // reasoning as the `byebye` above: a sender that never hears we are gone
        // keeps the PTR for [`announce::OTHER_TTL`], i.e. 75 minutes of a dead
        // device in a phone's list.
        // `take`, not `as_ref`: the same handle may be stopped by a re-advertisement
        // on another thread, and joining it twice would panic on the second caller.
        if let Some(announcer) = self.discovery.take_announcer() {
            announcer.stop();
        }
        // One owner for closing them: `Discovery::stop_cast` closes 8009 **and** the
        // setup faces (they are one protocol), and `stop_airplay` joins the serving
        // thread. The host's own slot is cleared here because a status read that kept
        // pointing at a stopped channel would report a receiver that cannot answer.
        self.discovery.stop_cast();
        self.discovery.stop_airplay();
        self.eventing.stop();
        self.host.log.info(
            "receiver stopped (relay emptied, SSDP byebye, HTTP face closed, AirPlay RTSP \
             released, GENA thread released); the player is untouched",
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
        //
        // The keys are the **reference's** (`uri`, `title`, `time`, from
        // `protocol.py:1211-1215`), which is also what `query=status` prints for
        // the same rows. The command used to say `url` while the HTTP face said
        // `uri` — two spellings for one fact, with the only reader still a stub
        // (`frontend/index.html`), so the cost of aligning them today is nothing
        // and the cost of not aligning them is a frontend written against one of
        // them. `time` was missing entirely, which the 「播放历史」 card sorts by.
        "history": runtime
            .table
            .play_history()
            .iter()
            .map(|(title, uri, at)| json!({ "uri": uri, "title": title, "time": at }))
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

/// `POST resolve-page=<page url or share text>` — start a background parse
/// (`_post_resolve_page` → `_resolve_page`, `macast/protocol.py:2828-2867`).
///
/// The answer is the seam's whole document: `code`, `message`, `job`, `url`, `note`.
/// The **`job` id is the page's only handle** on the parse, so the card keeps it and
/// polls [`resolve_status`]; nothing else identifies the work.
///
/// A refusal here is not a crashed parse — every "this is not an address" sentence comes
/// back with `code: 1` and the reason in `message` (a bare host would otherwise be handed
/// to the engine as a *search*; AGENTS.md §4.14's second red line).
#[tauri::command]
fn resolve_page(runtime: State<'_, Runtime>, text: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.resolve_page(&text))
}

/// The poll half: `GET ?query=resolve-status&job=<id>`
/// (`_resolve_status`, `macast/protocol.py:2890-2899`).
///
/// Two shapes are the contract, and the card must tell them apart: an unknown job answers
/// `code: 1` with `job: ""` and `status: null` （超时或已重启）, while a live job answers
/// `code: 0` with the **seventeen-key** status object. Rendering `null` as "still
/// parsing" would leave a dead job spinning for the whole TTL.
#[tauri::command]
fn resolve_status(runtime: State<'_, Runtime>, job: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.resolve_status(&job))
}

/// `POST cast-resolved=1&job=<id>&candidate=<index>[&target=<id>]` — cast one candidate
/// through the relay (`_post_cast_resolved`, `macast/protocol.py:2904-2948`).
///
/// `candidate` travels as the **raw string** the page happened to send, not as a number:
/// the seam owns the 「没有这个候选项」 refusal for anything that is not an index into the
/// finished job's list, and a page that parsed it first would answer that sentence itself —
/// two owners for one refusal.
///
/// `target` empty or `local` means this machine's own renderer; anything else is a device
/// row from [`cast_targets`], and **that half does not touch our playback state** (the
/// reference's own rule: a card may push to the TV while another player is playing here,
/// and two owners of "what is playing" would make the status page answer for both).
#[tauri::command]
fn cast_resolved(
    runtime: State<'_, Runtime>,
    job: String,
    candidate: String,
    target: Option<String>,
) -> Value {
    macast_upnp::http::answer_value(&runtime.management.cast_resolved(
        &job,
        &candidate,
        target.as_deref().unwrap_or(""),
    ))
}

/// `GET ?query=cast-targets[&refresh=1]` — the device dropdown's one read.
///
/// The first row is **always 本机** and it is inserted by the seam, not by the page: the
/// reference appends `_local_target()` itself, and a page that prepended its own would show
/// two entries for one renderer. `refresh=1` *starts* a discovery and answers from the
/// caches — a synchronous SSDP+mDNS round trip would hold a worker for three seconds to say
/// what the next poll says anyway.
#[tauri::command]
fn cast_targets(runtime: State<'_, Runtime>, refresh: Option<bool>) -> Value {
    macast_upnp::http::answer_value(&runtime.management.cast_targets(refresh.unwrap_or(false)))
}

/// `GET ?query=resolve-cookies` — the cookie card's one read
/// (`_cookie_state`, `macast/protocol.py:2794-2818`).
///
/// The answer carries **counts, never values** (`jar`, `cookies`, `bad`, `path`,
/// `browser`, `browsers`, `max_bytes`, `yt_dlp`) and that is a contract, not a habit:
/// `query=launch-param` and `query=module-settings` already hand out `Api_Token`, so a
/// second reader being management-gated is not a licence to add a second secret to the
/// same window. The `path` **is** named, because the panel's whole job is to tell a user
/// which file to look at.
#[tauri::command]
fn resolve_cookies(runtime: State<'_, Runtime>) -> Value {
    macast_upnp::http::answer_value(&runtime.management.cookie_state())
}

/// `POST resolve-cookies=<jar text>` — validate the whole Netscape jar, then save it
/// (`_post_resolve_cookies`, `macast/protocol.py:2986-3030`).
///
/// Validation happens **before** anything is opened, and that ordering is the feature: a
/// malformed jar is not an error to yt-dlp — it resolves anonymously and returns *fewer*
/// addresses, which the user reads as "this page has no video". The success message
/// carries both counts so a mixed jar (good lines + unreadable ones) says so instead of
/// reporting a smaller number as if it were the whole story.
#[tauri::command]
fn save_cookies(runtime: State<'_, Runtime>, text: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.save_cookies(&text))
}

/// `POST resolve-cookies-clear=1` — drop the saved jar
/// (`_post_resolve_cookies_clear`, `macast/protocol.py:3032-3043`).
///
/// A machine that never had a jar answers **success**: the caller's goal is "no jar",
/// and it has already reached it. That is the reference's own reading, and turning it into
/// a failure would send a user looking for a file that was never written.
#[tauri::command]
fn clear_cookies(runtime: State<'_, Runtime>) -> Value {
    macast_upnp::http::answer_value(&runtime.management.clear_cookies())
}

/// `POST resolve-cookie-browser=<名字[:资料目录]>` — which browser to read cookies from
/// (`_post_resolve_cookie_browser`, `macast/protocol.py:3045-3081`).
///
/// "Off" is one of `"" / 0 / off / none / 关闭 / 不用`, and it **unsets the key** rather
/// than storing `false` (§4.2: a disabled setting is absent, and `Setting::get` would
/// otherwise take the literal into the table). The grammar of `名字:资料目录` is owned by
/// `macast-cast::resolve::cookie_browser_spec` — splitting it here would make this file a
/// second write gate that can disagree with the read gate.
#[tauri::command]
fn set_cookie_browser(runtime: State<'_, Runtime>, text: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.set_cookie_browser(&text))
}

/// `POST set-subtitle-show=<flag>` — show or hide subtitles
/// (`_post_set_subtitle_show`, `macast/protocol.py:2699-2703`).
///
/// The field's **value** goes through the four-word rule with no `strip()`, which is the
/// reference's own asymmetry (`macast_core::pyops::form_flag_is_true`): `" on "` means
/// *hide* here, while `?refresh=` on the GET side does strip. The answer is
/// `{code, message}` and nothing else — Python publishes to the renderer and answers
/// success whether or not a player is running, and `DisplayCurrentSubtitle` changes
/// because **mpv reports the property back**, not because this command wrote it.
#[tauri::command]
fn set_subtitle_show(runtime: State<'_, Runtime>, value: String) -> Value {
    let show = macast_core::pyops::form_flag_is_true(&value);
    runtime.management.set_sub_show(show);
    json!({ "code": 0, "message": "success" })
}

/// `POST set-renderer=<title>` — switch the player driver
/// (`_set_renderer`, `macast/protocol.py:2385-2392`, row **6**).
///
/// This build ships **one** renderer, so the only accepted value is the name already in
/// force (`host::renderer_title`, which is also the name the dropdown shows) and
/// everything else gets the reference's `未找到播放器：{title}`. Accepting it still does
/// something real: a fresh install's `''` is persisted as `MPV` and the player reloads.
#[tauri::command]
fn set_renderer(runtime: State<'_, Runtime>, title: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.set_renderer(&title))
}

/// `GET ?query=launch-param` — the raw settings table, which is what the 高级设置 tab
/// edits (`launch_param_entry`, `macast-upnp/src/http.rs:2246`; `protocol.py:2149-2150`).
///
/// **This document is the whole file**, so it carries `Api_Token` — the credential that
/// opens `query=cast` and every `GATE_CODE` POST. On the HTTP face that is survivable for
/// one measured reason: no response on that port ever carries a CORS header, so a
/// cross-origin page cannot read it back even when DNS rebinding gets the request in.
/// The window is this process's own webview and has no such reader to defend against;
/// what it must not do is *render* the table list somewhere it can leak.
///
/// The answer has **no `code`/`message`** — it is the table itself, which is why this goes
/// through `Setting::snapshot()` rather than `answer_value`.
#[tauri::command]
fn launch_param(runtime: State<'_, Runtime>) -> Value {
    Value::Object(runtime.setting.snapshot())
}

/// `POST save-launch-param=<json>` — replace the **whole** settings table and restart
/// (`_post_save_launch_param`, `macast/protocol.py:2640-2648`, row **10**).
///
/// The answer is written **before** the restart (the seam spawns `MACAST_RESTART` with a
/// 250 ms delay), or the reply would die with the process it is describing. `json format
/// error` is the reference's own sentence for both a parse failure and a non-object —
/// the reference accepts an array and breaks on the next read; this port refuses it.
///
/// This is the window's door onto the same code-execution-gated row the HTTP face has
/// (`GATE_CODE` there): it replaces every key, including the ones that later reach
/// `subprocess(shell=True)`. The window is allowed to call it because it *is* this
/// process — the same boundary the mirror/gate rules in AGENTS.md §4.7b protect on the
/// wire, not a weaker one.
#[tauri::command]
fn save_launch_param(runtime: State<'_, Runtime>, value: String) -> Value {
    macast_upnp::http::answer_value(&runtime.management.save_launch_param(&value))
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
            "running": runtime.discovery.ssdp_running(),
            "healthy": runtime.discovery.ssdp_healthy(),
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
/// `query=interfaces` for the page's picker: every row the HTTP face reports, from
/// the same table (`net::network_interfaces`), so the two doors cannot disagree about
/// which cards exist.
#[tauri::command]
fn interfaces_snapshot(runtime: State<'_, Runtime>) -> Value {
    let rows: Vec<Value> = macast_core::net::network_interfaces(&runtime.setting)
        .into_iter()
        .map(|row| json!({ "name": row.name, "ip": row.ip, "usable": row.usable }))
        .collect();
    json!({ "code": 0, "interfaces": rows })
}

/// `query=module-settings`, the same builder the HTTP face calls.
#[tauri::command]
fn module_settings_snapshot(runtime: State<'_, Runtime>) -> Value {
    macast_core::module_settings::payload(&runtime.setting.snapshot())
}

/// `set-module-setting` — one owner (`module_settings::set_value`), two doors. The
/// window's door is **not** gated the way the HTTP one is: it is a command only this
/// process can invoke through its own webview, which is the boundary the GATE_CODE
/// rule protects on the wire (`AGENTS.md` §4.7b).
#[tauri::command]
fn set_module_setting(
    runtime: State<'_, Runtime>,
    key: String,
    value: String,
    remove: bool,
) -> Value {
    macast_core::module_settings::set_value(&runtime.setting, Some(&key), &value, remove)
}

/// The protocol switch, for the page's own checkboxes. The HTTP face has the same
/// implementation (`Discovery::toggle_protocol`); this is the window's door on it.
#[tauri::command]
fn toggle_protocol(runtime: State<'_, Runtime>, title: String) -> Value {
    let answer = runtime.discovery.toggle_protocol(&title);
    json!({ "code": answer.code, "message": answer.message })
}

/// `quit` and `check-update` from the page. Same implementation as the HTTP row.
#[tauri::command]
fn app_action(runtime: State<'_, Runtime>, action: String) -> Value {
    let answer = runtime.discovery.app_action(&action);
    json!({ "code": answer.code, "message": answer.message })
}

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

/// What a re-advertisement did, in the three states only this pair can tell apart.
///
/// `stopping` is "there was an announcer to stop" and `up` is "there is one now". The
/// middle case is the one a reader needs: **goodbye sent, nothing came back up** — a
/// phone has just been told the device is gone and no log line except this one says
/// whether it came back. Collapsing it into "not announcing" (which is also what the
/// no-receiver case says) would send that reader to restart the app instead of to the
/// bind failure that happened.
fn announcement_change(stopping: bool, up: bool) -> &'static str {
    match (stopping, up) {
        (true, true) => "re-announced",
        (false, true) => "now announcing",
        (true, false) => "stopped announcing (see this file for why it could not start again)",
        (false, false) => "has nothing to announce (no receiver is listening)",
    }
}

/// The faces an interface switch has to re-touch, behind one handle with **two doors**.
///
/// Both doors ask the same question — "which ports are listening, and is the
/// announcement back up after the pin changed" — so they get one implementation. Before
/// this, the window's picker was the only thing that could re-advertise, and a
/// `set-interface` POST would have answered 「已切换到 en0」 while the phone went on being
/// told the addresses of the interface the user just replaced: the same `code: 0` that
/// says nothing happened, reached from the other side of the house.
///
/// `cast` / `airplay` / `announce` are slots rather than fields because this handle is
/// built **before** boot binds any of them (the HTTP face needs it to answer a POST, and
/// a POST cannot arrive before the receivers exist). An empty slot is therefore a
/// statement about construction order, not about a face being down — which is why the
/// three readers below take/clone through methods instead of exposing `Option` fields a
/// caller would have to guess the meaning of. `ssdp` is a plain field: `SsdpPlugin::new`
/// registers devices without touching a socket, so it exists before this handle does.
pub struct Discovery {
    setting: Arc<Setting>,
    ssdp: Arc<SsdpPlugin>,
    cast: Arc<Mutex<Option<Arc<macast_cast::CastChannel>>>>,
    airplay: Arc<Mutex<Option<(Arc<airplay::AirPlayReceiver>, thread::JoinHandle<()>)>>>,
    announce: Arc<Mutex<Option<Arc<announce::Announcer>>>>,
    /// The 8008/8443 face. It lives here rather than in `Runtime` since
    /// `toggle-protocol` has to close it with 8009: the two are one protocol, and a
    /// switch that stopped the receiver while leaving the identity face answering
    /// is the shape `host::Faces`'s doc refuses.
    setup: Arc<Mutex<Option<setup::SetupServer>>>,
    app_log: Arc<Logger>,
    /// The five handles a *starting* face needs, attached once by boot. A slot rather
    /// than fields for the same reason `cast`/`airplay` are: `Discovery` is built
    /// before the player, the table and the host exist, and a unit test has none of
    /// them. Empty means "this discovery cannot open faces" — every starter says so
    /// instead of panicking.
    faces: Arc<Mutex<Option<FaceParts>>>,
}

/// See [`Discovery::faces`]. One struct rather than five slots so "is this wired" has
/// exactly one answer.
#[derive(Clone)]
pub struct FaceParts {
    pub table: Arc<StateTable>,
    pub renderer: SharedRenderer,
    pub shell_renderer: Arc<host::ShellRenderer>,
    pub cast_log: Arc<Logger>,
    /// The host's own receiver slot is published/cleared with the channel. Two slots
    /// (`Discovery::cast` and the host's) exist because they mean different things:
    /// the first is "may the announcement read a port", the second is "an inbound
    /// `StateCall` may fan out to it" — and the handover path (`host.rs:1065`) asks
    /// the second before releasing the previous protocol.
    pub host: host::TauriHost,
}

fn slot<T>() -> Arc<Mutex<Option<T>>> {
    Arc::new(Mutex::new(None))
}

impl Discovery {
    pub fn new(setting: Arc<Setting>, ssdp: Arc<SsdpPlugin>, app_log: Arc<Logger>) -> Arc<Discovery> {
        Arc::new(Discovery {
            setting,
            ssdp,
            cast: slot(),
            airplay: slot(),
            announce: slot(),
            setup: slot(),
            app_log,
            faces: slot(),
        })
    }

    /// Attach the plumbing boot owns. Called once, from `.setup()`, before any face
    /// starts.
    pub fn wire(&self, parts: FaceParts) {
        *self.faces.lock().unwrap() = Some(parts);
    }

    /// Register the 8009 receiver once it has bound, so a later re-advertisement can
    /// read the port it **actually** got (and not announce a port nobody serves —
    /// `discovery_services`'s own doc carries that rule).
    pub fn set_cast(&self, channel: Arc<macast_cast::CastChannel>) {
        *self.cast.lock().unwrap() = Some(channel);
    }

    /// Register the RTSP receiver and its serving thread once both exist.
    pub fn set_airplay(&self, receiver: Arc<airplay::AirPlayReceiver>, handle: thread::JoinHandle<()>) {
        *self.airplay.lock().unwrap() = Some((receiver, handle));
    }

    pub fn ssdp_running(&self) -> bool {
        self.ssdp.ssdp().is_running()
    }

    pub fn ssdp_healthy(&self) -> bool {
        self.ssdp.ssdp().is_healthy()
    }

    /// The receiver as it is registered right now — for the shell's own status reads,
    /// which must answer "is 8009 up" from the same slot the announcement reads.
    pub fn cast(&self) -> Option<Arc<macast_cast::CastChannel>> {
        self.cast.lock().unwrap().clone()
    }

    /// The setup face's bound numbers, for the boot summary and for a reader who wants
    /// to know whether 8443 is there. A string rather than the numbers because the two
    /// callers print the same sentence and only one of them has the `SetupServer`.
    pub fn setup_report(&self) -> Option<String> {
        self.setup.lock().unwrap().as_ref().map(|face| {
            format!(
                "HTTP on {}, HTTPS {}",
                face.port,
                match face.https_port {
                    Some(port) => port.to_string(),
                    None => "not started".to_string(),
                }
            )
        })
    }

    /// Take the announcer, leaving nothing behind. `take`, not `as_ref`: stopping it
    /// joins its thread, and joining twice panics the second caller — a re-advertisement
    /// on one thread and the window closing on another can both ask for this.
    pub fn take_announcer(&self) -> Option<Arc<announce::Announcer>> {
        self.announce.lock().unwrap().take()
    }

    /// Take the RTSP receiver with its thread handle, for the same reason.
    pub fn take_airplay(&self) -> Option<(Arc<airplay::AirPlayReceiver>, thread::JoinHandle<()>)> {
        self.airplay.lock().unwrap().take()
    }

    pub fn stop_ssdp(&self) {
        self.ssdp.stop();
    }

    /// Rebind the SSDP listener onto the interface that was just written.
    ///
    /// Asked for only by a run that had something to rebind: `update_ip` stops and
    /// starts the listener, so calling it when SSDP never came up would have this
    /// command announcing something nobody asked it to announce.
    pub fn rebind_ssdp(&self) {
        self.ssdp.update_ip();
    }

    /// Which services are worth publishing **right now**, from the slots as filled.
    fn services(&self, discovery_log: &Arc<Logger>) -> Vec<announce::Service> {
        let cast = self.cast.lock().unwrap();
        let airplay = self.airplay.lock().unwrap();
        discovery_services(
            &self.setting,
            &cast,
            airplay.as_ref().map(|(receiver, _)| receiver),
            discovery_log,
        )
    }

    /// Say goodbye through whatever announcer is registered, build the services again,
    /// and start a new one. Returns `(was_announcing, is_announcing)` — the pair
    /// [`announcement_change`] turns into a sentence.
    ///
    /// The reference's `on_network_interface_changed` (`macast/macast.py:1324-1346`)
    /// rebinds SSDP **and cycles every running protocol**, because in Python each
    /// protocol owns its own advertiser; the sentence under it — 「Cycling does not touch
    /// mpv, so anything playing keeps playing」 — is why a re-advertisement is allowed to
    /// be this narrow. Here the mDNS half is **one** object (`Announcer`, one socket,
    /// every service type), so cycling means exactly these three moves. The Cast and
    /// AirPlay receivers keep their sockets — the ports did not change, and a sender
    /// already connected to 8009 would have been dropped for nothing.
    /// Open 8009 and the two setup faces, or say why not. The **one** constructor of
    /// the Chromecast protocol's sockets: boot calls it, and so does the protocol
    /// switch — a second copy is how a toggled-on receiver ends up with a different
    /// order than a booted one (8009 before 8008 is load-bearing; see `setup.rs`'s
    /// header), or with a certificate that was never asked for.
    ///
    /// Idempotent: a call when the receiver is already registered answers `true`
    /// without touching a socket, which is what lets `apply_protocols` run on every
    /// switch instead of tracking what changed.
    pub fn start_cast(&self) -> bool {
        if self.cast.lock().unwrap().is_some() {
            return true;
        }
        // The certificate is asked for **here**, behind the protocol's door: generating
        // one writes two files into the settings directory, and a protocol the user
        // switched off should not leave key material behind (§24's rule).
        let parts = match self.faces.lock().unwrap().clone() {
            Some(parts) => parts,
            None => {
                self.app_log.error(
                    "start_cast: no faces are wired into this discovery (boot attaches \
                     them; a test does not), so 8009 cannot start",
                );
                return false;
            }
        };
        let pair = match macast_cast::cert::ensure_in_setting_dir(&parts.cast_log) {
            Ok(pair) => Some(pair),
            Err(e) => {
                parts.cast_log.warning(&format!(
                    "Cast certificate unavailable ({}) — neither 8009 nor 8443 can start; 8008 still answers",
                    e.message()
                ));
                None
            }
        };
        let channel = macast_cast::CastChannel::new(
            Arc::clone(&parts.cast_log),
            parts.table.clone() as SharedStore,
            Arc::clone(&parts.renderer),
        );
        // Registered **before** it starts: a `StateCall` that arrives in between should
        // fan out to a receiver that has not bound yet rather than be dropped.
        self.set_cast(Arc::clone(&channel));
        parts.host.set_cast(Arc::clone(&channel));
        let started = match &pair {
            Some(pair) => match channel.start(pair) {
                Ok(()) => true,
                Err(e) => {
                    parts.cast_log.warning(&format!(
                        "Cast channel could not start: {e} — senders will find no 8009 here"
                    ));
                    false
                }
            },
            // `start` needs a certificate to wrap connections with; the refusal above
            // already said which ports that costs.
            None => false,
        };
        if !started {
            self.stop_cast();
            return false;
        }
        let config = match &pair {
            Some(pair) => match pair.server_config() {
                Ok(config) => Some(config),
                Err(e) => {
                    parts.cast_log.warning(&format!(
                        "Cast HTTPS setup: the certificate will not load ({}); answering over 8008 only",
                        e.message()
                    ));
                    None
                }
            },
            None => None,
        };
        match setup::SetupServer::start(
            Arc::clone(&self.setting),
            Arc::clone(&parts.cast_log),
            config,
        ) {
            Ok(server) => *self.setup.lock().unwrap() = Some(server),
            Err(e) => parts.cast_log.warning(&format!(
                "Cast setup face did not start: {e} — senders that probe 8008 will get nothing"
            )),
        }
        true
    }

    /// Close 8009 and the setup faces, in the reference's order (`channel::stop` logs
    /// its own sentence). The announcer is **not** touched here: it is rebuilt from the
    /// slots by [`Discovery::restart`], and doing it twice would send a goodbye twice.
    pub fn stop_cast(&self) {
        if let Some(parts) = self.faces.lock().unwrap().as_ref() {
            parts.host.clear_cast();
        }
        if let Some(channel) = self.cast.lock().unwrap().take() {
            channel.stop();
            self.app_log
                .info(&format!("Cast receiver on {} stopped", channel.port()));
        }
        if let Some(mut server) = self.setup.lock().unwrap().take() {
            let port = server.port;
            server.shutdown();
            self.app_log
                .info(&format!("setup face on {port} closed; 8443 goes with it"));
        }
    }

    /// The RTSP receiver and its serving thread, or a sentence about why neither.
    /// Same idempotence contract as [`Discovery::start_cast`].
    pub fn start_airplay(&self) -> bool {
        if self.airplay.lock().unwrap().is_some() {
            return true;
        }
        let parts = match self.faces.lock().unwrap().clone() {
            Some(parts) => parts,
            None => {
                self.app_log.error(
                    "start_airplay: no faces are wired into this discovery (boot attaches \
                     them; a test does not), so 7000 cannot start",
                );
                return false;
            }
        };
        let mut receiver = airplay::AirPlayReceiver::new(self.setting.friendly_name());
        match receiver.start() {
            Ok(port) => {
                let receiver = Arc::new(receiver);
                let served = {
                    let receiver = Arc::clone(&receiver);
                    let serve_log = Arc::clone(&self.app_log);
                    let sink =
                        Arc::clone(&parts.shell_renderer) as Arc<dyn airplay::RendererCallback>;
                    // The reference's own thread name, typo included (`AIRPLAY_RSTP`).
                    thread::Builder::new()
                        .name("AIRPLAY_RSTP".to_string())
                        .spawn(move || {
                            if let Err(e) = receiver.serve(sink) {
                                serve_log.warning(&format!(
                                    "AirPlay RTSP serve loop ended: {e} — 7000 answers no more"
                                ));
                            }
                        })
                };
                match served {
                    Ok(handle) => {
                        self.app_log.info(&format!(
                            "AirPlay RTSP: listening on {port} as \"{}\" — the number this run \
                             actually bound, which is not necessarily 7000",
                            self.setting.friendly_name()
                        ));
                        self.set_airplay(receiver, handle);
                        true
                    }
                    Err(e) => {
                        self.app_log.warning(&format!(
                            "AirPlay RTSP could not start its serving thread: {e} — {port} is \
                             bound but nobody reads it"
                        ));
                        false
                    }
                }
            }
            // `start` has already said which ports it tried; this sentence is for the
            // reader of the startup summary.
            Err(e) => {
                self.app_log.warning(&format!(
                    "AirPlay RTSP: not listening — no free port in the range after 7000 ({e}); \
                     macOS's own AirPlay Receiver is the usual reason, and turning it off in \
                     系统设置 is the way to give this to Macast instead"
                ));
                false
            }
        }
    }

    /// Stop the RTSP receiver and **join** its thread: "switched off" has to mean the
    /// port is free, and the accept loop polls, so this is milliseconds.
    pub fn stop_airplay(&self) {
        if let Some((receiver, handle)) = self.take_airplay() {
            let port = receiver.port();
            receiver.stop();
            match handle.join() {
                Ok(()) => self
                    .app_log
                    .info(&format!("AirPlay RTSP on {port} stopped and the port is free")),
                Err(_) => self.app_log.warning(&format!(
                    "AirPlay RTSP on {port}: its serving thread panicked; the port may still be held"
                )),
            }
        }
    }

    /// Cycle every face the enabled list names, then re-advertise. The reference's
    /// `plugins_changed` handler does the same thing (`macast.py:1048-1051`): read the
    /// list again, apply it to the running service, rebuild the menu.
    ///
    /// The **DLNA** face is deliberately not cycled: it carries the settings page and
    /// the management API, so closing it would take away the place the switch lives in
    /// (`docs/Rust-Port-Design.md` §16 第 12 条, and §8's row about it).
    pub fn apply_protocols(&self) -> host::Faces {
        let faces = host::faces_enabled(&self.setting, &self.app_log);
        if faces.cast {
            self.start_cast();
        } else {
            self.stop_cast();
        }
        if faces.airplay {
            self.start_airplay();
        } else {
            self.stop_airplay();
        }
        let discovery_log = Logger::named(announce::LOGGER_NAME);
        let discovery_log_path = discovery_log.claim();
        let (stopping, up) = self.restart(&discovery_log, &discovery_log_path);
        discovery_log.info(&format!(
            "protocol switch applied; mDNS {}",
            announcement_change(stopping, up)
        ));
        faces
    }

    /// `_app_action` (`protocol.py:2404-2414`). `quit` ends **this process**: the
    /// answer is written first and the exit happens on its own thread, which is the
    /// reference's shape too (`threading.Thread(... 'MACAST_QUIT').start()` at
    /// `:2407-2409`) — a synchronous exit would drop the very reply that tells the
    /// page what happened. The exit hook installed in `run()` does the rest: bye-bye on
    /// SSDP, goodbye on mDNS, the HTTP face closed, the player stopped.
    ///
    /// `check-update` is refused rather than mimed: this build has no version check and
    /// no updater, and answering 「正在检查更新」 would send the user to watch a
    /// progress bar that does not exist. Registered as a divergence in
    /// `docs/Rust-Port-Design.md` §8.
    pub fn app_action(&self, raw: &str) -> macast_upnp::http::Answer {
        let action = raw.trim();
        match action {
            "quit" => {
                let parts = match self.faces.lock().unwrap().clone() {
                    Some(parts) => parts,
                    None => {
                        return macast_upnp::http::Answer::fail(
                            "退出失败：这个 discovery 没有接到应用句柄（boot 负责接上）",
                        )
                    }
                };
                let log = Arc::clone(&self.app_log);
                let app = parts.host.app.clone();
                let spawned = thread::Builder::new()
                    .name("MACAST_QUIT".to_string())
                    .spawn(move || {
                        // Long enough for the reply to leave, short enough that a person
                        // who pressed the button sees the app close at once.
                        thread::sleep(std::time::Duration::from_millis(250));
                        log.info(
                            "quit requested from the settings page; stopping the receiver \
                             and the player",
                        );
                        app.exit(0);
                    });
                if spawned.is_err() {
                    return macast_upnp::http::Answer::fail(
                        "退出失败：起不了退出线程，应用还在运行",
                    );
                }
                macast_upnp::http::Answer::ok("Macast 正在退出")
            }
            "check-update" => macast_upnp::http::Answer::fail(
                "这台设备的这个构建没有更新检查：它不联网查版本，也不会自己升级",
            ),
            other => macast_upnp::http::Answer::fail(format!("未知应用操作：{}", other)),
        }
    }

    /// `_toggle_protocol` (`protocol.py:2394-2402`) with the write in
    /// [`host::set_protocol_enabled`]. The direction is the reference's: a title that
    /// is **not** in the enabled list is the one being switched on, compared raw —
    /// which is why `chromecast` (lowercase) reports 「已启用」 while changing nothing
    /// but its own spelling on the way out.
    ///
    /// One divergence, and it is the page's lifeline: switching **DLNA** off is
    /// refused here. The reference would stop that protocol's handler with the rest of
    /// the service — and that handler is where this settings page and every management
    /// route live, so the switch would take away the only place that can switch it back
    /// (`docs/Rust-Port-Design.md` §8 records the row).
    pub fn toggle_protocol(&self, raw: &str) -> macast_upnp::http::Answer {
        let titles = host::enabled_protocols(&self.setting, &self.app_log);
        let enabling = !titles.iter().any(|held| held == raw);
        if let Some(real) = host::resolve_title(raw, &host::available_titles()) {
            if real == host::DLNA_TITLE && !enabling {
                return macast_upnp::http::Answer::fail(
                    "不能停用 DLNA：这台设备的设置页与管理接口都住在它的 HTTP 面上，\
                     关掉它就没有页面能把它开回来了",
                );
            }
        }
        match host::set_protocol_enabled(&self.setting, &self.app_log, raw, enabling) {
            Ok(_) => {
                self.apply_protocols();
                macast_upnp::http::Answer::ok(format!(
                    "{}{}",
                    raw,
                    if enabling { "已启用" } else { "已停用" }
                ))
            }
            Err(refusal) => macast_upnp::http::Answer::fail(refusal),
        }
    }

    fn restart(&self, discovery_log: &Arc<Logger>, discovery_log_path: &std::path::Path) -> (bool, bool) {
        let old = self.take_announcer();
        let stopping = old.is_some();
        if let Some(announcer) = old {
            announcer.stop();
        }
        let started = start_announcement(
            self.services(discovery_log),
            discovery_log,
            discovery_log_path,
        );
        let up = started.is_some();
        *self.announce.lock().unwrap() = started;
        (stopping, up)
    }

    /// Pin (or clear, with `""`) the interface used for discovery, re-advertise, and
    /// answer. One implementation for the window's picker and the management POST row.
    ///
    /// Ported from `_set_network_interface` (`macast/protocol.py:1977-1995`) with its
    /// refusal intact: an unknown interface name is refused rather than stored, because
    /// a typo would otherwise silently drop Macast to the loopback fallback and make it
    /// invisible while looking like it was configured. The known set is every row of the
    /// picker's table, usable or not — matching Python, which does not filter, so that a
    /// down-but-real interface is not "not found".
    ///
    /// The reference then publishes `network_interface_changed`, whose handler rebinds
    /// SSDP **and** cycles mDNS; both halves are here, and the answer says whether each
    /// one moved. **That is a widening**: the reference's dict is `{code, message,
    /// interface}` because its publisher cannot tell the caller what the engine bus did,
    /// while this call is direct. The two extra keys are additive — a page that reads the
    /// three reference keys is unaffected — and they are the difference between 「切换成功」
    /// and a control that looks like it works. Recorded as a divergence in
    /// `docs/Rust-Port-Design.md` §8.
    pub fn set_interface(&self, raw: &str) -> macast_upnp::http::Answer {
        let name = raw.trim().to_string();
        if !name.is_empty() {
            let known: Vec<String> = {
                let mut names: Vec<String> = net::network_interfaces(&self.setting)
                    .into_iter()
                    .map(|row| row.name)
                    .collect();
                names.sort();
                names
            };
            if let Some(refusal) = net::pin_refusal(&name, &known) {
                return macast_upnp::http::Answer::fail(refusal);
            }
        }

        self.setting.set(Key::NetworkInterface, Value::String(name.clone()));

        // The reference restarts both halves unconditionally; here each is asked for only
        // when it exists, because `update_ip` stops and starts the SSDP listener and the
        // announcer can only be rebuilt from receivers that are actually listening.
        let rebound = self.ssdp_running();
        if rebound {
            self.rebind_ssdp();
        }
        // The cached browse socket joined the old card (see `discover_cast`'s cache),
        // so it is dropped here with the rest of the mDNS state — the reference's
        // `network_interface_changed` cycles the protocols for the same reason.
        macast_cast::mdns::forget_browser();
        // The mDNS half, which is the half a phone actually looks for. The reference
        // cycles the protocols for exactly this reason.
        let discovery_log = Logger::named(announce::LOGGER_NAME);
        let discovery_log_path = discovery_log.claim();
        let (stopping, up) = self.restart(&discovery_log, &discovery_log_path);
        // The one sentence that tells a reader whether the switch did anything at all
        // to mDNS: `up` without `stopping` is "started", `!up` with it is "said goodbye
        // and could not start again", and `!up` without it is "there was nothing to say".
        // Three states, one line, because the answer is the same `code: 0` in all three.
        discovery_log.info(&format!(
            "network interface changed -> {pin}; mDNS {state}",
            pin = if name.is_empty() {
                "auto".to_string()
            } else {
                name.clone()
            },
            state = announcement_change(stopping, up)
        ));

        macast_upnp::http::Answer::ok(format!(
            "已切换到 {}",
            if name.is_empty() { "自动选择" } else { &name }
        ))
        // The reference's three keys in the reference's order, then the two this port
        // adds because its call is direct (see the doc above).
        .with("interface", json!(name))
        .with("ssdp_reannounced", json!(rebound))
        .with("mdns_reannounced", json!(up))
    }
}

/// The service list this machine should announce **right now**.
///
/// One owner for "which receivers get advertised, and what we say about the ones that
/// do not", because there are now two callers: boot (which builds it once, after the
/// receivers it depends on have bound) and `set_network_interface` (which has to rebuild
/// it after the pin changes, when the address list inside every `Service` is stale).
/// A second copy of this in the switch path is how a re-advertisement ends up publishing
/// the interface the user just replaced.
///
/// Both arguments are "what is **actually listening**": `cast_channel` is `None` when
/// 8009 never came up, and the AirPlay receiver is `None` when the port range was taken
/// (macOS ships its own receiver on 7000). The reference advertises `cast_port` even
/// after a failed bind, which puts a device in a menu that cannot be cast to — recorded
/// in `announce`'s module header as a deliberate divergence, and the reason the
/// refusals are logged here rather than answered with an empty packet.
fn discovery_services(
    setting: &Setting,
    cast: &Option<Arc<macast_cast::CastChannel>>,
    airplay: Option<&Arc<airplay::AirPlayReceiver>>,
    discovery_log: &Arc<Logger>,
) -> Vec<announce::Service> {
    let mut services = Vec::new();
    match cast.as_ref() {
        Some(channel) => services.push(announce::Service::from_setting(
            setting,
            channel.port(),
            discovery_log,
        )),
        None => discovery_log.info(
            "mDNS: not announcing — 8009 is not listening, and a device that cannot be \
             cast to is worse than no device",
        ),
    }
    // Two things decide the AirPlay service's shape and both are measurements: the
    // **port** is the one `start()` bound (so a sender is never pointed at 7000, which
    // is often Apple's), and the **deviceid** is the receiver's own, so the TXT record
    // and `GET /info` name the same device.
    if let Some(receiver) = airplay {
        services.push(announce::Service::airplay_from_setting(
            setting,
            receiver.port(),
            receiver.device_id(),
            discovery_log,
        ));
    }
    services
}

/// Start the one announcer for `services`, and say out loud what it published.
///
/// **One socket announces every type** this machine serves — the reference's
/// `discover()` is a singleton and both protocols register into it, so this is the
/// shipped shape rather than an optimisation: with one socket there is no competing
/// reader for 5353's unicast traffic (`scripts/mdns_reuseport_probe.py` measured what
/// two give), and the answering half is per service type (`responder_answers()`), not
/// per socket.
///
/// `None` is a real answer with two different meanings, and the log says which: no
/// service worth publishing (both receivers down), or a start that was refused —
/// whose reason `Announcer::start` already wrote to `Discovery.log`, which is what the
/// per-instance warning here points at.
fn start_announcement(
    services: Vec<announce::Service>,
    discovery_log: &Arc<Logger>,
    discovery_log_path: &std::path::Path,
) -> Option<Arc<announce::Announcer>> {
    if services.is_empty() {
        return None;
    }
    let summaries: Vec<(String, u16, String, &'static str)> = services
        .iter()
        .map(|service| {
            (
                service.instance.clone(),
                service.port,
                // The list, not just the count: §22 compares what the announcer
                // decided to publish against what a second stack reads off the wire,
                // and "3 interfaces" cannot be compared to a set of addresses.
                service
                    .addresses
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                if service.responder_answers() {
                    "it answers queries for this type"
                } else {
                    "it announces this type and does not answer it — the responder table was measured for _googlecast._tcp"
                },
            )
        })
        .collect();
    let announcer = announce::Announcer::new(services, Arc::clone(discovery_log));
    if announcer.start().is_err() {
        for (instance, _, _, _) in &summaries {
            discovery_log.warning(&format!(
                "mDNS: not announcing {instance} — see {}",
                discovery_log_path.display()
            ));
        }
        None
    } else {
        for (instance, port, published, mode) in &summaries {
            discovery_log.info(&format!(
                "mDNS: announcing {instance} on port {port} over [{published}]; \
                 {mode}; re-announcing every {} s",
                announce::REANNOUNCE.as_secs()
            ));
        }
        Some(announcer)
    }
}

/// Pin (or clear, with `""`) the interface used for discovery, and re-advertise.
///
/// The window's door on [`Discovery::set_interface`], which owns every part of the
/// answer — the known-interface table and its refusal, `Setting[NetworkInterface]`,
/// the SSDP rebind, the goodbye and the fresh announcement, and the four keys of the
/// document. The management POST row (`set-interface`, `protocol.py:2288`) opens the
/// same function: a picker that only worked from the window would answer a page's
/// 「已切换到 en0」 with the phone still being told the previous interface's addresses.
#[tauri::command]
fn set_network_interface(runtime: State<'_, Runtime>, name: String) -> Value {
    macast_upnp::http::answer_value(&runtime.discovery.set_interface(&name))
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
    // The same builder `GET /api?query=log` calls (`macast_core::logging`). The key
    // order, the missing `message` on the success branch and the `unknown log
    // module` payload are all decided there, with the reasons; this is the window's
    // door onto it, not a second copy of the document.
    logging::log_view_document(
        module.as_deref(),
        tail.unwrap_or(logging::DEFAULT_TAIL_LINES),
        all.unwrap_or(false),
    )
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
    // Same single owner as `POST clear-log` on the DLNA port.
    logging::clear_log_document(module.as_deref())
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
    logging::log_modules_document()
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
    // Built by the same function that answers `GET /api?query=subscribers`
    // (`macast_upnp::http::subscribers_document`), so the window and a `curl` cannot
    // be shown two shapes of one table — AGENTS.md §4.8's
    // 「两处答案不一致比没有数字更糟」, refused structurally rather than by memory.
    // The keys, the two `not in the reference` additions, and why `aggregated` and
    // `subscribed` are the same list while there is one child, are all documented at
    // that function.
    let child = macast_upnp::http::ChildDiagnostics {
        title: host::DLNA_TITLE,
        type_name: macast_upnp::http::DLNA_PROTOCOL_TYPE,
        // One snapshot for the five numbers, not five reads of a live queue.
        stats: runtime.eventing.diagnostics(),
    };
    macast_upnp::http::subscribers_document(&child, &runtime.eventing.subscribers())
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
            // The shell's own startup summary speaks through the **root** logger, i.e.
            // `macast.log` — which is where the reference puts it: `server.py`'s
            // 「receiver: …」, `protocol_cast.py`'s receiver line and
            // `protocol_airplay.py`'s listening line all propagate to the root handler,
            // because `logsplit.claim` is only ever called for **plugins**
            // (`macast.py:336` / `:905`). This port claims four core names as a
            // deliberate divergence (§8), and the cost of getting the routing wrong is
            // that a reader following AGENTS.md §5's troubleshooting recipe greps a file
            // that does not hold the sentence — measured once already, when these lines
            // were riding the player's logger and §28's instrument looked in the wrong
            // file (§16 第 29 条 ④).
            let app_log = macast_core::logging::root();
            // The state table speaks through its **own** module log, not the
            // player's: `protocol.py:30` is `logging.getLogger("Protocol")` and
            // `logsplit` gives it `logs/Protocol.log`. Handing the table the `MPV`
            // logger — which is what this line used to do — put `setState: …` and
            // the whole play-history story next to the renderer's events, where a
            // reader chasing "the card emptied itself on restart" would never look.
            let protocol_log = Logger::named(host::PROTOCOL_LOG);
            let _protocol_log_path = protocol_log.claim();
            let setting = Arc::new(Setting::load_default());
            // The table reads the persisted 播放历史 **before** any face exists, so a
            // sender that casts within the first milliseconds finds yesterday's rows
            // already on the card. Order is the whole point: built the other way round
            // the page would show an empty list until the next cast rewrote the file,
            // and the empty would be indistinguishable from "this machine has never
            // been used" (§23's first delivery was exactly that `[]` with two causes).
            let table = Arc::new(
                StateTable::new(protocol_log).with_setting(Arc::clone(&setting)),
            );
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

            // One logger for both Chromecast faces — the 8009 receiver and the
            // 8008/8443 setup face share `protocol_cast.py`'s module, so their
            // rows land in the same claimed file (§4.10b's "one record, one file"
            // read from the other side: one record type, one file).
            let cast_log = Logger::named(setup::LOGGER_NAME);
            let _cast_log_path = cast_log.claim();
            // A third file, because the advertiser is a different module in the
            // reference too: `discovery.py:26` logs under `Discovery`, and the
            // question that reads this file is 「why doesn't my phone see this
            // Macast」 — which is an announcement question, not a Cast-session one
            // (§4.10b: one module, one file).
            let discovery_log = Logger::named(announce::LOGGER_NAME);
            let discovery_log_path = discovery_log.claim();

            let host = TauriHost {
                app: handle,
                setting: setting.clone(),
                table: table.clone(),
                log: log.clone(),
                service_stopped: service_stopped.clone(),
                cast: Arc::new(Mutex::new(None)),
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
            // One renderer object, two doors. `ShellRenderer` is stateless over
            // the `Player`, so two instances would *answer* the same today — and
            // the day one of them gains a field (the last-cast title, say) the
            // DLNA door and the Cast door would start disagreeing about what the
            // player was handed, which is exactly the class of bug §4.8 keeps
            // naming. Hoisted above `Soap::new` for that reason, not for tidiness.
            let shell_renderer = Arc::new(host::ShellRenderer {
                player: player.clone(),
            });
            // The concrete handle is kept because there are now **three** doors
            // (SOAP, the Cast channel, AirPlay's RTSP) and the last one speaks a
            // different trait; the object behind all three stays the one value.
            let renderer = Arc::clone(&shell_renderer) as SharedRenderer;
            let scpd = Arc::new(Scpd::embedded()?);
            let soap = Soap::new(
                scpd,
                table.clone() as SharedStore,
                // The renderer seam is the player plus nothing else: this shell has one
                // renderer, so there is no `ProtocolGroup` fan-out to reproduce here
                // (`macast/protocol_group.py`), and `ShellRenderer` is the single
                // delegate `Renderer::methods()` would have enumerated.
                Arc::clone(&renderer),
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
            // `host.clone()` rather than `host`: the last one moves into `Runtime`
            // below, and SSDP needs a `SharedHooks` of its own — the `notify` half is
            // how a bind failure reaches the page, and the `stop_service` half is how
            // it reaches *this* struct's `stop_services`.
            //
            // Built before the management face because the face holds the same
            // `Arc<SsdpPlugin>` inside [`Discovery`]: `set-interface` has to rebind
            // SSDP, and two copies of "which listener is running" is how a switch
            // ends up reported but not performed. `SsdpPlugin::new` only registers
            // devices and logs; it binds nothing until `start()` below.
            let ssdp = SsdpPlugin::new(Arc::new(host.clone()) as SharedHooks, setting.clone());
            // One handle for the two doors that can switch the interface. Its receiver
            // slots are filled as boot binds each face, and it is read — never cloned
            // into a second copy — by `ShellManagement` and `Runtime`.
            let discovery = Discovery::new(
                setting.clone(),
                Arc::clone(&ssdp),
                app_log.clone(),
            );
            discovery.wire(FaceParts {
                table: table.clone(),
                renderer: Arc::clone(&renderer),
                shell_renderer: Arc::clone(&shell_renderer),
                cast_log: Arc::clone(&cast_log),
                host: host.clone(),
            });
            // Built here rather than next to the other listeners because the
            // management face below takes the same `Arc` inside [`Discovery`]:
            // `set-interface` has to rebind SSDP and re-announce, and two copies of
            // "which listener is running" is how a switch ends up reported but not
            // performed. `SsdpPlugin::new` only registers devices and logs one line;
            // it binds nothing until `start()` near the bottom of this closure.
            // One handle, two doors: the HTTP face answers `/api?query=resolve-page`
            // through it, and the window's four commands (resolve/status/cast/targets)
            // go through **the same object** — AGENTS.md §4.8's 「两处答案不一致比没有
            // 数字更糟」 refused structurally rather than by memory. A second
            // `ShellManagement` here would also mean a second job table, so a parse
            // started from the window would be invisible to a `curl` poll.
            let management: Arc<dyn macast_upnp::http::Management> =
                Arc::new(host::ShellManagement {
                    host: host.clone(),
                    player: player.clone(),
                    store: relay_store.clone(),
                    temp_dir: relay_temp,
                    port: relay_port,
                    jobs: Arc::clone(&resolve_jobs),
                    targets: device_table,
                    discovery: Arc::clone(&discovery),
                });
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
                Arc::clone(&management),
                setting.clone(),
                // The same table SOAP writes and GENA reads — `query=status`'s
                // `media` block reads it through this face, so the page and a control
                // point asking `GetTransportInfo` at the same second see one moment,
                // not two. A second `StateTable` here would be the §4.2
                // 「两处答案不一致比没有数字更糟」 shape, one layer over.
                table.clone() as SharedStore,
            )?;
            let http = HttpServer::new(Arc::new(face));

            // The Chromecast receiver and its setup face are one protocol, so they
            // share a certificate, a log file, and — load-bearing — an **order**:
            // 8009 binds before the setup face looks for 8008, because a setup
            // listener that walks its fallback range would step onto 8009 and take
            // the socket from under the receiver. The reference is in the same
            // order for the same reason (`protocol_cast.py:411-413` starts the TLS
            // server first), and `setup.rs`'s header says why this port does not
            // copy the range at all.
            //
            // One read of the enable list, **before** any Chromecast socket is
            // touched: the receiver, the setup face and the announcement are one
            // protocol, and a run that gated 8009 but left 8008 answering would
            // hand a sender an identity for a device it cannot connect to. The
            // certificate is behind the same door because generating it writes two
            // files into the settings directory — a protocol the user switched off
            // should not leave key material behind.
            // One read of the enable list for **both** doors. The list is also what
            // says which 「unknown protocol」 warnings get written, so reading it per
            // face would repeat that sentence once per protocol and let 8009 and 7000
            // be opened by two different snapshots of a file the settings tab can
            // rewrite (`host::faces_enabled`'s doc, and the audit row in `host.rs`).
            let faces = host::faces_enabled(&setting, &log);
            let cast_on = faces.cast;
            let airplay_on = faces.airplay;
            if !cast_on {
                app_log.info(
                    "Chromecast is switched off (Macast_Protocols): not starting 8009, 8008/8443, \
                     or the mDNS announcement",
                );
            }
            // The certificate is the shared `macast.*` pair when one is already
            // there and our own otherwise (`ensure_in_setting_dir` owns that rule,
            // including "a broken foreign pair is reported by name and left
            // alone"). No pair costs 8009 and 8443; the plain 8008 face needs no
            // certificate and still answers.
            // Both faces, one constructor: [`Discovery::start_cast`] owns the order
            // (8009 before 8008), the certificate, and the sentences for each refusal.
            // **Behind the gate**, like every other face: the switch path asks
            // `apply_protocols` for its own read, so a boot that skipped this `if`
            // would open 8009 on a run whose list says Chromecast is off — which is
            // exactly what §24 caught when this refactor first landed.
            if cast_on {
                discovery.start_cast();
            }
            // The answer, not the ask: a receiver whose bind failed is `None` in the slot,
            // and the summary lines below say the bound numbers.
            let cast_report = discovery
                .cast()
                .map(|channel| channel.port().to_string());
            let setup_report = discovery.setup_report();
            // The **announcement** is started below, after both receivers have bound,
            // by `Discovery::restart` — the same call `set-interface` makes when the
            // user moves to another 网卡. 8009 answers a sender that already knows our
            // address, and the announcement is the half that lets a sender find that
            // address without being told; it publishes only ports that are really
            // listening, because the reference advertises `cast_port` even after a
            // failed TLS bind, which puts a device in a menu that cannot be cast to
            // (AGENTS.md §4.1's first symptom row, recorded in `announce`'s module
            // header as a deliberate divergence rather than a silent one).
            // The **AirPlay receiver**: a second, independent protocol on its own
            // port, with its own door in the same one read of the enable list. The
            // reference binds RTSP *before* it advertises, because macOS ships an
            // AirPlay Receiver that already owns 7000 — so the number a sender must
            // reach is only known after the bind, and `start()` answers with the
            // port it got rather than the one it was asked for (`protocol_airplay.py`
            // says the same thing in its own comment).
            if airplay_on {
                // The receiver, its serving thread, the "listening on <bound port>" line
                // and the slot registration are one function — the same one the protocol
                // switch calls. An empty slot afterwards means the port range was taken
                // (or the thread refused), and `Discovery` reads that as "do not announce
                // _airplay._tcp".
                discovery.start_airplay();
            } else {
                app_log.info(
                    "AirPlay is switched off (Macast_Protocols): not listening on 7000, \
                     and an iPhone will not be pointed at a port nobody serves",
                );
            }

            // **One announcer, one socket, every service**, started once for both
            // types — see [`start_announcement`] for why that is the shipped shape
            // rather than an optimisation.
            // Boot's first start is the same call the interface switch makes, minus the
            // sentence: `stopping` is `false` here by construction (nothing was
            // announced yet), and the answer a reader of `Discovery.log` wants at boot
            // is the per-service line `start_announcement` writes, not a change report.
            let _ = discovery.restart(&discovery_log, &discovery_log_path);

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
                discovery: Arc::clone(&discovery),
                eventing: eventing.clone(),
                relay: relay_store,
                management: Arc::clone(&management),
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
            app_log.info(&format!(
                "receiver: HTTP {} on port {}, SSDP {}",
                if http.is_running() { "listening" } else { "did not start" },
                http.bound_port(),
                if ssdp.ssdp().is_running() { "announcing" } else { "did not start" },
            ));
            // Same rule for the setup face: report the bound number, not the
            // requested one, and say plainly when 8443 is not there — a user
            // comparing this line with a sender's "Failed to determine cast type"
            // warning is reading the two halves of one fact.
            match &cast_report {
                // The channel logs its own "ChromecastProtocol started on port"
                // line inside `start()`; this is the shell's answer to "did it",
                // phrased the way the HTTP and SSDP lines above are — the bound
                // number, or the reason there is none.
                Some(port) => app_log.info(&format!("Cast receiver: 8009 listening on {port}")),
                None => log.info(if cast_on {
                    "Cast receiver: not listening — no certificate to wrap 8009 with \
                     (the setup face says the same about 8443)"
                } else {
                    // The certificate sentence above is **false** on this path: no
                    // pair was ever asked for, because the protocol is switched
                    // off. Naming the missing key when the user pressed the switch
                    // is the §4.2/§4.3 failure this repository has been bitten by
                    // twice — a log line that points at the wrong suspect costs
                    // more than no log line, because it sends whoever reads it to
                    // fix something that is not broken.
                    "Cast receiver: not starting — Chromecast is switched off in the \
                     settings page (nothing was bound, and no certificate was asked for)"
                }),
            }
            match &setup_report {
                Some(text) => app_log.info(&format!("setup face: {text}")),
                None => log.info(if cast_on {
                    "setup face: not listening on 8008"
                } else {
                    "setup face: not starting — Chromecast is switched off"
                }),
            }
            app_log.info(&format!("renderer log: {}", log_path.display()));
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
            toggle_protocol,
            app_action,
            interfaces_snapshot,
            module_settings_snapshot,
            set_module_setting,
            log_view,
            clear_log,
            log_modules,
            subscriber_snapshot,
            resolve_page,
            resolve_status,
            cast_resolved,
            cast_targets,
            resolve_cookies,
            save_cookies,
            clear_cookies,
            set_cookie_browser,
            set_subtitle_show,
            set_renderer,
            save_launch_param,
            launch_param,
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
