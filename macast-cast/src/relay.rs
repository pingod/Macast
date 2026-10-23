//! Relay server that serves resolved page-address candidates to devices.
//!
//! Python counterpart: `macast/media_relay.py`. The references are a file that
//! grew every lesson in AGENTS.md §4.14 into it, so this port follows the same
//! structure: constants first, then `parse_range` / `serve_plan`, then the relay
//! bookkeeping (`Relay` / `RelayStore`), then the HTTP handler, then `open_relay`
//! and `advertise_host`.
//!
//! ## Why a second HTTP server
//!
//! A media fetch holds its connection for the length of the video. Serving it on
//! the same CherryPy / Axum tree that answers management API calls would block
//! every management request for that duration. The relay binds its own random
//! port (the port never appears anywhere except inside the URLs we hand out), and
//! the management API stays answerable.
//!
//! ## Three serving shapes
//!
//! `proxy`: progressive file, no ffmpeg. Range is translated if the origin
//! honours it, ignored otherwise.
//!
//! `remux`: HLS/DASH manifest. ffmpeg copies elementary streams into one
//! fragmented MP4 that grows in a temp file. Seeking is offered only after
//! ffmpeg exits 0 — before that the relay has no length.
//!
//! `merge`: DASH with picture and sound on two addresses. Same growing file,
//! same states as remux; `Relay.mode` stays `"remux"`. The second input carries
//! its own headers, because one blanket `-headers` in the ffmpeg command would
//! leave it unauthenticated.
//!
//! ## Red lines
//!
//! * `parse_range(None)` → `(Some(0), None)` is a whole-file answer, status
//!   200, **no** `Content-Range`. 206 is only for a request that actually
//!   carried a Range header. The suffix form `bytes=-N` is *not* junk: it is
//!   `(None, Some(N))` and it hands over the tail.
//! * A relay whose bytes are gone is refused (502/503), not answered with a
//!   lengthless 200 that leaves the player waiting forever.
//! * `serve_plan` decides status/bounds before the socket opens. The decision is
//!   callable from tests without opening a socket.
//! * `wait_for_bytes_within` blocks **before** the response line is written.
///   Once that line is out the only honest answer is a truncated one.

use macast_core::logging::Logger;
use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::resolve::{find_command, header_field, plan, py_round1, Candidate};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use crate::resolve::console_flags;

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

/// Bytes read from upstream per turn.
const COPY_CHUNK: usize = 128 * 1024;

/// The logger this module writes through, under the name the reference gives it
/// (`media_relay.py:72` — `logging.getLogger("MediaRelay")`), unclaimed so the
/// records join `macast.log` like Python's do.
///
/// Same story as `airplay.rs`: these two `warn!` calls went through the `log`
/// facade, which no code in this workspace ever gave a sink, so "a relay temp
/// file survived the sweep" was a sentence that could never be read.
fn relay_logger() -> &'static std::sync::Arc<Logger> {
    static SELF: std::sync::OnceLock<std::sync::Arc<Logger>> = std::sync::OnceLock::new();
    SELF.get_or_init(|| Logger::named("MediaRelay"))
}

/// Maximum seconds to wait for the remux to produce its first byte.
pub const GROW_WAIT_SECONDS: f64 = 20.0;

/// Poll interval while waiting for remux bytes.
pub const GROW_POLL: Duration = Duration::from_millis(100);

/// A relay lives until nobody reads it for this long (30 min).
pub const RELAY_IDLE_TTL: f64 = 30.0 * 60.0;

/// How often the idle-relay sweeper wakes — `media_relay.py`'s
/// `start_sweeper(interval=60)`.
pub const RELAY_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum simultaneous relays; oldest-LRU eviction.
pub const RELAY_MAX: usize = 8;

/// Bytes=0-0 probe of the origin's Range support.
const RANGE_PROBE: &str = "bytes=0-0";

/// The status codes that mean "the origin said no" outright: 401, 403, 404, 410.
///
/// This is the **second copy** of that set — the first is
/// [`crate::resolve::REFUSED_STATUS`], which writes it as a regex over a
/// subprocess's stderr. Python has the same pair (`media_relay.REFUSED_STATUS`
/// as a tuple, `media_resolve._REFUSED_STATUS` as the regex) and Part 59 pins
/// them together; `refused_codes_match_the_resolver_regex` does the same here,
/// so the two cannot drift apart silently.
///
/// What it does **not** decide is whether a relay is refused. In Python the
/// tuple's only reader is `UpstreamError.refused` (`media_relay.py:199`), and
/// nothing downstream consults that fact: `probe_proxy` catches the exception
/// and fails the relay whatever the status was. The earlier draft of this
/// function branched on the set instead — a non-refused status left the relay
/// `Open` and `open_relay_with` returned `Ok`, which registered a relay that
/// never got a byte from the origin. That is a port of a fact nobody in Python
/// ever claimed, so the set is kept only as the cross-check it actually is.
pub const REFUSED_CODES: &[u16] = &[401, 403, 404, 410];

/// `-protocol_whitelist` for ffmpeg when reading manifests.
const PROTOCOL_WHITELIST: &str = "file,http,https,tcp,tls,crypto,httpproxy,data";

/// AAC codec prefixes that need the ADTS-to-BISTREAM filter.
const AAC_CODECS: &[&str] = &["aac", "mp4a"];

/// ffmpeg's own words that trigger the ADTS retry.
const ADTS_TELLS: &[&str] = &["Malformed AAC bitstream", "aac_adtstoasc"];

// ---------------------------------------------------------------------------
// Content-Type tables
// ---------------------------------------------------------------------------

/// What "we do not know what this is" looks like on the wire (`UNKNOWN_TYPE` in
/// `media_relay.py:133`). Two readers depend on this exact spelling: the suffix
/// table's fallback below, and [`probe_proxy`], which treats it as "the suffix
/// table said nothing, so the origin's own `Content-Type` is worth reading".
/// Python writes the default into the dataclass (`content_type: str =
/// UNKNOWN_TYPE`, :235), so the constant is the single truth on both sides.
const UNKNOWN_TYPE: &str = "application/octet-stream";

fn content_type_for_suffix(suffix: &str) -> &'static str {
    match suffix {
        ".mp4" | ".m4v" => "video/mp4",
        ".mov"          => "video/quicktime",
        ".mkv"          => "video/x-matroska",
        ".webm"         => "video/webm",
        ".flv"          => "video/x-flv",
        ".ts"           => "video/mp2t",
        ".m3u8"         => "application/vnd.apple.mpegurl",
        ".mpd"          => "application/dash+xml",
        ".mp3"          => "audio/mpeg",
        ".m4a"          => "audio/mp4",
        ".aac"          => "audio/aac",
        ".ogg" | ".opus" => "audio/ogg",
        ".wav"          => "audio/wav",
        ".flac"         => "audio/flac",
        _               => UNKNOWN_TYPE,
    }
}

fn content_type_for_container(name: &str) -> Option<&'static str> {
    Some(match name {
        "mp3"                    => "audio/mpeg",
        "mp4" | "mov,mp4,m4a,3gp,3g2,mj2" => "video/mp4",
        "matroska,webm" | "matroska" | "webm" => "video/x-matroska",
        "mpegts"                 => "video/mp2t",
        "flv"                    => "video/x-flv",
        "hls"                    => "application/vnd.apple.mpegurl",
        "dash"                   => "application/dash+xml",
        _ if name.contains("matroska") || name.contains("webm") => "video/x-matroska",
        _ if name.contains("mp4") || name.contains("mov") => "video/mp4",
        _ if name.contains("mpegts")  => "video/mp2t",
        _ => return None,
    })
}

/// The `Content-Type` to advertise for a candidate.
pub fn content_type_for(candidate: &Candidate) -> &'static str {
    let probe_container = candidate.probe.container.as_deref().unwrap_or("");
    if !probe_container.is_empty() {
        if let Some(t) = content_type_for_container(&probe_container.to_lowercase()) {
            return t;
        }
    }
    content_type_for_suffix(candidate.path_suffix())
}

// ---------------------------------------------------------------------------
// parse_range / serve_plan
// ---------------------------------------------------------------------------

/// `parse_range(header)` — mirrors Python's tuple exactly, including which half
/// of it is `None`.
///
/// `None` (no header) is "from the beginning". `(None, n)` is the suffix form —
/// the last `n` bytes — and it is **not** the same as junk: a player asking for
/// the tail must get the tail. `Some(None)` as the whole result is junk, which
/// RFC 7233 says to ignore rather than answer 416 with.
///
/// Collapsing "suffix" into "junk" is the mistake this signature exists to
/// prevent: both read as a missing start, and the two answers differ by the
/// whole file.
pub fn parse_range(header: Option<&str>) -> Option<(Option<u64>, Option<u64>)> {
    let h = match header {
        None | Some("") => return Some((Some(0), None)),
        Some(h) => h,
    };
    let (unit, spec) = h.split_once('=')?;
    if unit.trim().to_lowercase() != "bytes" || spec.contains(',') || spec.is_empty() {
        return None;
    }
    let (first, last) = spec.split_once('-').unwrap_or((spec, ""));
    // **Fields are parsed the way Python's `int()` parses**, which tolerates surrounding
    // whitespace (including `\n`) and a leading `+`. Rust's `str::parse::<u64>` accepts `+` but
    // not whitespace, so `bytes=0-9 ` and `bytes=0 - 9` were refused here and accepted by the
    // reference. A trailing space is exactly what a hand-written or buggy sender produces, and the
    // consequence of refusing it is not "an error": it is a **416**, which a DLNA renderer reads as
    // a dead file (AGENTS.md §4.8, the `parse_range` row). Found by differential probing.
    fn number(field: &str) -> Option<u64> {
        field.trim().parse::<u64>().ok()
    }
    if first.is_empty() {
        // `bytes=-` is `(None, None)` in the reference — an empty suffix still *is* a Range, and
        // the caller resolves it against the real length (which yields "the last 0 bytes").
        if last.is_empty() {
            return Some((None, None));
        }
        return Some((None, Some(number(last)?)));
    }
    let start: u64 = number(first)?;
    if last.is_empty() {
        return Some((Some(start), None));
    }
    let stop: u64 = number(last)?;
    if stop < start {
        return None;
    }
    Some((Some(start), Some(stop)))
}

/// Whether the codec name indicates ADTS framing.
pub fn needs_adtstoasc(codec: &str) -> bool {
    let name = codec.trim().to_lowercase();
    AAC_CODECS.iter().any(|prefix| name.starts_with(prefix))
}

/// Whether ffmpeg's stderr contains the ADTS complaint.
pub fn tells_adts(stderr: &str) -> bool {
    ADTS_TELLS.iter().any(|tell| stderr.contains(tell))
}

/// The outcome of `serve_plan`, returned as data so callers can test it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServePlan {
    pub status: u16,
    pub start:  u64,
    pub stop:   Option<u64>,  // None = to end
    pub total:  Option<u64>,  // None = unknown
    pub partial: bool,
    pub content_length: Option<u64>,
}

impl ServePlan {
    /// Bytes to write, `None` = until the source ends.
    pub fn remaining(&self) -> Option<u64> {
        match (self.stop, self.start) {
            (Some(stop), start) => Some(stop.saturating_sub(start) + 1),
            _ => None,
        }
    }
}

/// Decide the HTTP response for one request against a relay's known facts.
///
/// Mirrors Python `serve_plan` including every branching comment.
pub fn serve_plan(ranges: bool, length: Option<u64>, range_header: Option<&str>) -> ServePlan {
    let asked = range_header.map_or(false, |h| !h.is_empty());
    // Junk and "no Range at all" are the same answer: serve the whole file.
    let (req_start, req_stop) = match parse_range(range_header) {
        None            => (None, None),
        Some((start, stop)) => (start, stop),
    };

    // The total the viewer can seek over — only meaningful when the origin
    // honours Range. `length` without `ranges` is still useful as a
    // `Content-Length`, but not as the upper bound for a 206.
    let total = if ranges { length } else { None };

    if total.is_none() {
        if ranges {
            // The origin honours Range but never told us how long it is (a 206
            // with no parseable `Content-Range`): stream from the beginning with
            // no length. An estimate here would be a lie the player seeks by.
            return ServePlan { status: 200, start: 0, stop: None, total: None, partial: false, content_length: None };
        }
        // No Range support at all: the declared length is still worth telling
        // the renderer, because that is what lets one show a duration. The stop
        // is filled in so the write is bounded by the length just advertised
        // rather than by however long the origin keeps talking.
        return match length {
            None    => ServePlan { status: 200, start: 0, stop: None, total: None, partial: false, content_length: None },
            Some(n) => ServePlan { status: 200, start: 0, stop: Some(n - 1), total: Some(n), partial: false, content_length: Some(n) },
        };
    }

    let total = total.unwrap();
    if req_start.is_none() && req_stop.is_none() {
        // Junk Range: ignored, whole file (RFC 7233, and Part 25's table).
        return ServePlan { status: 200, start: 0, stop: Some(total - 1), total: Some(total), partial: false, content_length: Some(total) };
    }

    let (start, stop) = match (req_start, req_stop) {
        (None, tail) => (total.saturating_sub(tail.unwrap_or(0)), None),
        (Some(from), Some(to)) => (from, Some(to.min(total - 1))),
        (Some(from), None) => (from, None),
    };

    if start >= total {
        return ServePlan { status: 416, start, stop: None, total: Some(total), partial: false, content_length: None };
    }
    if !asked {
        return ServePlan { status: 200, start: 0, stop: Some(total - 1), total: Some(total), partial: false, content_length: Some(total) };
    }
    let end = stop.unwrap_or(total - 1);
    ServePlan {
        status: 206,
        start,
        stop: Some(end),
        total: Some(total),
        partial: true,
        content_length: Some(end - start + 1),
    }
}

// ---------------------------------------------------------------------------
// Relay bookkeeping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayState { Open, Running, Complete, Failed }

impl RelayState {
    pub fn is_alive(&self) -> bool {
        matches!(self, RelayState::Open | RelayState::Running | RelayState::Complete)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            RelayState::Open     => "open",
            RelayState::Running  => "running",
            RelayState::Complete => "complete",
            RelayState::Failed   => "failed",
        }
    }
}

/// One registered stream: what to fetch, how to serve it, and for how long.
///
/// Mirrors Python `Relay` dataclass. The per-relay `Mutex<RelayInner>` carries
/// the mutable state so the store's own lock is never held across network or
/// file I/O.
pub struct Relay {
    pub relay_id:     String,
    pub url:          String,
    pub headers:      HashMap<String, String>,
    pub mode:         String,    // "proxy" or "remux"
    pub title:        String,
    /// Locks its own value: [`probe_proxy`] takes `&Relay` and learns the
    /// declared type from the origin's answer, so the field can change after
    /// registration while a viewer may be reading the response headers.
    /// Python's dataclass field is plain (`content_type: str`, :235) because the
    /// GIL makes the write atomic; here a `String` behind `&Relay` is not.
    pub content_type: Mutex<String>,
    pub created:      f64,
    pub last_seen:    Arc<Mutex<f64>>,
    pub inner:        Arc<Mutex<RelayInner>>,
}

pub struct RelayInner {
    /// Whether the origin honours Range requests.
    pub ranges:      bool,
    /// Content length, if known.
    pub length:      Option<u64>,
    /// Error text from probe or watcher.
    pub error:       String,
    /// Probe error (proxy only).
    pub probe_error: String,

    // remux only
    pub path:        String,
    pub state:       RelayState,
    pub audio_url:   String,
    pub audio_headers: HashMap<String, String>,
    pub audio_codec: String,
    pub adtstoasc:   bool,
    pub ffmpeg:      String,
    pub proc:        Option<Child>,
}

impl RelayInner {
    /// Python's `self.ranges and bool(self.length)`: a known zero-length stream
    /// is not scrubable. `Option::is_some` would answer the other way.
    pub fn seekable(&self) -> bool {
        self.ranges && self.length.unwrap_or(0) != 0
    }
    pub fn size(&self) -> u64 {
        if self.path.is_empty() { return self.length.unwrap_or(0); }
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

impl Relay {
    /// Whether the stream can be promised a byte range: Python's `relay.seekable`
    /// property, which is `self.ranges and bool(self.length)` (`RelayInner::seekable`).
    ///
    /// An accessor rather than a field read because the answer lives behind a lock,
    /// and because two callers ask from different sides of a wire: the byte-serving
    /// path writes it into `Accept-Ranges`, and the page card's device push turns
    /// "not seekable" into `live = true` for the device's `streamType`. Reading it
    /// back out of `status_json()` would put a document format in the middle of a
    /// decision.
    pub fn seekable(&self) -> bool {
        self.inner.lock().unwrap().seekable()
    }

    /// The media type this relay is currently serving, which is **not** fixed at
    /// registration: a proxy relay learns the real one from the origin's answer
    /// (`probe_proxy` writes `content_type` under its own lock, `:1222`), so the
    /// reader takes the lock instead of copying the field at open time.
    pub fn content_type(&self) -> String {
        self.content_type.lock().unwrap().clone()
    }

    pub fn touch(&self) {
        *self.last_seen.lock().unwrap() = now_secs();
    }

    pub fn last_seen_secs(&self) -> f64 {
        *self.last_seen.lock().unwrap()
    }

    /// The card's row for this relay: Python's `status()` dict, eleven keys, in
    /// Python's order, with Python's types.
    ///
    /// Two shapes here are the whole reason this is not a `HashMap<String,
    /// String>`: `length` and `written` are numbers that may be `null` — a proxy
    /// relay that never got a `Content-Length` reports "nobody has said", which
    /// the page renders differently from "zero bytes" — and `seekable`/`ranges`
    /// are booleans. `serde_json`'s `Map` keeps insertion order (the workspace
    /// turns on `preserve_order`) because the order *is* the document the page
    /// reads.
    ///
    /// `written` follows Python's `size()`, which decides by `mode` rather than
    /// by whether a path exists: a remux whose encoder has not spawned yet asks
    /// `getsize('')` and gets 0, while a proxy asks `length` and may get nothing
    /// to say. `seconds` and `idle` reuse `py_round1` — the same one-decimal,
    /// half-to-even reading the resolve job's own seconds use, so one truth
    /// point formats both.
    pub fn status_json(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        let now = now_secs();
        let written = if self.mode == "remux" {
            serde_json::Value::from(
                std::fs::metadata(&inner.path).map(|m| m.len()).unwrap_or(0),
            )
        } else {
            match inner.length {
                Some(n) => serde_json::Value::from(n),
                None => serde_json::Value::Null,
            }
        };
        let mut row = serde_json::Map::new();
        row.insert("id".to_string(), serde_json::Value::String(self.relay_id.clone()));
        row.insert("title".to_string(), serde_json::Value::String(self.title.clone()));
        row.insert("mode".to_string(), serde_json::Value::String(self.mode.clone()));
        row.insert("state".to_string(), serde_json::Value::String(inner.state.as_str().to_string()));
        row.insert("seekable".to_string(), serde_json::Value::Bool(inner.seekable()));
        row.insert("ranges".to_string(), serde_json::Value::Bool(inner.ranges));
        row.insert("length".to_string(), match inner.length {
            Some(n) => serde_json::Value::from(n),
            None => serde_json::Value::Null,
        });
        row.insert("written".to_string(), written);
        row.insert(
            "error".to_string(),
            serde_json::Value::String(
                if !inner.error.is_empty() { inner.error.clone() } else { inner.probe_error.clone() },
            ),
        );
        row.insert("seconds".to_string(), serde_json::Value::from(py_round1(now - self.created)));
        row.insert("idle".to_string(), serde_json::Value::from(py_round1(now - self.last_seen_secs())));
        serde_json::Value::Object(row)
    }

    /// Stop the encoder and delete the temp file. Called **after** removing
    /// from the store — never under the store's lock.
    pub fn shutdown(&self) {
        let (proc, path) = {
            let mut inner = self.inner.lock().unwrap();
            let proc = inner.proc.take();
            let path = std::mem::take(&mut inner.path);
            inner.state = RelayState::Failed;
            (proc, path)
        };
        if let Some(mut proc) = proc {
            let _ = proc.kill();
            let _ = proc.wait();
        }
        if !path.is_empty() {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != io::ErrorKind::NotFound {
                    relay_logger().warning(&format!("relay {}: temp file left behind at {path}: {e}", self.relay_id));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RelayStore
// ---------------------------------------------------------------------------

/// Thread-safe map of active relays. The store's Mutex is never held across
/// network I/O or `ffmpeg.wait()` — entries are popped under the lock, then
/// shut down outside it.
pub struct RelayStore {
    entries: Mutex<HashMap<String, Arc<Relay>>>,
    max:     usize,
    ttl:     f64,
    /// Latched by `start_sweeper`: one sweep loop per store, for the life of
    /// that store.
    sweeper_run: AtomicBool,
}

impl RelayStore {
    pub fn new(max: usize, ttl: f64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max,
            ttl,
            sweeper_run: AtomicBool::new(false),
        }
    }

    pub fn add(&self, relay: Arc<Relay>) -> Arc<Relay> {
        let dropped = {
            let mut map = self.entries.lock().unwrap();
            let mut dropped = self.expire_locked(&mut map, Some(&relay.relay_id));
            while map.len() >= self.max {
                // Evict the entry with the oldest `last_seen`.
                let oldest = map.values()
                    .min_by(|a, b| a.last_seen_secs().partial_cmp(&b.last_seen_secs()).unwrap())
                    .map(|r| r.relay_id.clone());
                if let Some(id) = oldest {
                    if let Some(r) = map.remove(&id) { dropped.push(r); }
                }
            }
            map.insert(relay.relay_id.clone(), Arc::clone(&relay));
            dropped
        };
        for gone in dropped { gone.shutdown(); }
        relay
    }

    /// Get a relay, extending its TTL. Returns `None` when unknown or expired.
    pub fn get(&self, relay_id: &str) -> Option<Arc<Relay>> {
        let (relay, expired) = {
            let mut map = self.entries.lock().unwrap();
            let relay = self.find_locked(&map, relay_id)?.clone();
            let idle = now_secs() - relay.last_seen_secs();
            let expired = idle > self.ttl;
            if expired { map.remove(&relay.relay_id); }
            else { relay.touch(); }
            (relay, expired)
        };
        if expired { relay.shutdown(); None } else { Some(relay) }
    }

    /// Like `get` but does not extend the TTL (for the status endpoint).
    pub fn peek(&self, relay_id: &str) -> Option<Arc<Relay>> {
        let map = self.entries.lock().unwrap();
        self.find_locked(&map, relay_id).cloned()
    }

    /// Is this id one of the live relays? The orphan sweep needs it to tell an
    /// abandoned temp file from one somebody is still watching. The
    /// device-facing lookup (`find_locked`) is the constant-time one because
    /// there the id arrives from a URL; this asks about a name that came out of
    /// our own temp directory, so it goes straight to the map.
    pub fn contains(&self, relay_id: &str) -> bool {
        self.entries.lock().unwrap().contains_key(relay_id)
    }

    pub fn drop_relay(&self, relay_id: &str) -> Option<Arc<Relay>> {
        let relay = self.entries.lock().unwrap().remove(relay_id)?;
        relay.shutdown();
        Some(relay)
    }

    pub fn clear(&self) {
        let relays: Vec<_> = self.entries.lock().unwrap().drain().map(|(_, v)| v).collect();
        for r in relays { r.shutdown(); }
    }

    pub fn active(&self) -> Vec<serde_json::Value> {
        let map = self.entries.lock().unwrap();
        let mut rs: Vec<_> = map.values().cloned().collect();
        rs.sort_by(|a, b| a.created.partial_cmp(&b.created).unwrap());
        rs.iter().map(|r| r.status_json()).collect()
    }

    fn find_locked<'a>(&self, map: &'a HashMap<String, Arc<Relay>>, relay_id: &str) -> Option<&'a Arc<Relay>> {
        let wanted = relay_id.as_bytes();
        let mut found = None;
        for (stored, relay) in map {
            if hmac_eq(stored.as_bytes(), wanted) { found = Some(relay); }
        }
        found
    }

    fn expire_locked(&self, map: &mut HashMap<String, Arc<Relay>>, keep: Option<&str>) -> Vec<Arc<Relay>> {
        let deadline = now_secs() - self.ttl;
        let mut dropped = Vec::new();
        let ids: Vec<_> = map.keys().cloned().collect();
        for id in ids {
            if keep == Some(&id) { continue; }
            let last = map[&id].last_seen_secs();
            if last < deadline {
                if let Some(r) = map.remove(&id) { dropped.push(r); }
            }
        }
        dropped
    }

    /// Drop remux temp files nobody is watching any more — Python's
    /// `sweep_orphans`, called once when the listener starts.
    ///
    /// A clean shutdown empties the directory, but AGENTS.md §4.2 records that a
    /// signal-killed Macast skips its cleanup — and a live HLS remux is an
    /// ffmpeg that never ends, so its temp file would grow across every later
    /// session unnoticed. Only files older than the idle TTL go: a *second*
    /// running instance keeps its own file freshly written, so this cannot reach
    /// into a stream somebody is watching (multi-instance is normal on this
    /// LAN). Returns the paths removed, so the caller can log it and a test can
    /// assert the age rule rather than the accident.
    pub fn sweep_orphans(&self, dir: &Path, max_age: f64, now: f64) -> Vec<PathBuf> {
        let mut removed = Vec::new();
        // Python's `except OSError: return []` — a directory we cannot list is
        // not a reason to say we deleted something.
        let Ok(entries) = std::fs::read_dir(dir) else { return removed };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if !name.ends_with(".mp4") {
                continue;
            }
            if self.contains(&name[..name.len() - 4]) {
                continue;
            }
            let age = match std::fs::metadata(&path).and_then(|m| {
                m.modified().map(|t| now - t.duration_since(UNIX_EPOCH)
                    .unwrap_or_default().as_secs_f64())
            }) {
                Ok(age) => age,
                Err(_) => continue,
            };
            if age < max_age {
                continue;
            }
            if std::fs::remove_file(&path).is_err() {
                continue;
            }
            removed.push(path);
        }
        if !removed.is_empty() {
            // The names, not just the count. "swept 3 abandoned temp file(s)" says
            // something happened; a reader deciding whether that is a bug needs to
            // know **which** files, because an abandoned `…/<id>.mp4` whose id is
            // still on the page is a different story from an old one left by a
            // killed ffmpeg (AGENTS.md §4.2: 每一条失败要给得出下一步).
            const NAMED: usize = 5;
            let mut names = removed
                .iter()
                .take(NAMED)
                .filter_map(|path| path.file_name().and_then(|n| n.to_str()))
                .collect::<Vec<_>>()
                .join(", ");
            if removed.len() > NAMED {
                names.push_str(&format!(" … (+{} more)", removed.len() - NAMED));
            }
            relay_logger().warning(&format!(
                "relay: swept {} abandoned temp file(s): {}",
                removed.len(),
                names
            ));
        }
        removed
    }

    /// Expire idle relays even while nobody reads the page; `true` if this call
    /// started the loop — Python's `start_sweeper`, whose "already have one"
    /// guard matters because `ensure_server` is idempotent.
    ///
    /// The thread holds only a `Weak`, so dropping the store ends the loop
    /// instead of leaking a strong reference for the life of the process. A
    /// poisoned entries lock is skipped rather than fatal: silencing the sweep
    /// would strand an ffmpeg and the temp file it keeps growing.
    ///
    /// An associated function, not a method: the loop needs an `Arc`, and
    /// `self: &Arc<Self>` is not a legal receiver in stable Rust.
    pub fn start_sweeper(store: &Arc<RelayStore>, interval: Duration) -> bool {
        if store
            .sweeper_run
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let weak: Weak<RelayStore> = Arc::downgrade(store);
        thread::Builder::new().name("RELAY_SWEEP".into()).spawn(move || {
            loop {
                // Python's `while not self._stop.wait(interval)`: sleep first,
                // then work — a relay registered a second ago is not idle.
                thread::sleep(interval);
                let Some(store) = weak.upgrade() else { return };
                let dropped = match store.entries.lock() {
                    Ok(mut map) => store.expire_locked(&mut map, None),
                    Err(_) => continue,
                };
                for relay in dropped {
                    relay.shutdown();
                }
            }
        }).expect("spawn sweeper");
        true
    }
}

/// Constant-time byte comparison (same as Python's `hmac.compare_digest`).
fn hmac_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// remux helpers
// ---------------------------------------------------------------------------

fn input_arguments(url: &str, headers: &HashMap<String, String>) -> Vec<String> {
    let mut args = Vec::new();
    if !headers.is_empty() {
        args.push("-headers".to_string());
        args.push(header_field(Some(headers)));
    }
    args.extend([
        "-protocol_whitelist".to_string(), PROTOCOL_WHITELIST.to_string(),
        "-i".to_string(), url.to_string(),
    ]);
    args
}

fn remux_arguments(with_adtstoasc: bool) -> Vec<String> {
    let mut args = vec!["-c".to_string(), "copy".to_string()];
    if with_adtstoasc {
        args.extend(["-bsf:a".to_string(), "aac_adtstoasc".to_string()]);
    }
    args.extend([
        "-movflags".to_string(), "+frag_keyframe+empty_moov+default_base_moof".to_string(),
        "-flush_packets".to_string(), "1".to_string(),
        "-f".to_string(), "mp4".to_string(),
    ]);
    args
}

fn drain_stderr(child: &mut Child) -> String {
    let mut text = String::new();
    if let Some(stderr) = child.stderr.take() {
        let _ = BufReader::new(stderr).read_to_string(&mut text);
    }
    text
}

fn last_line(text: &str) -> &str {
    text.lines().rev().find(|l| !l.trim().is_empty()).map(|l| {
        let l = l.trim();
        if l.len() > 200 { &l[..200] } else { l }
    }).unwrap_or("")
}

fn spawn_remux(relay: &Relay) -> bool {
    let inner = relay.inner.lock().unwrap();
    let mut cmd = Command::new(&inner.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    let url = relay.url.clone();
    let hdrs = relay.headers.clone();
    let audio_url = inner.audio_url.clone();
    let audio_hdrs = inner.audio_headers.clone();
    let with_adts = inner.adtstoasc;
    let path = inner.path.clone();
    drop(inner);

    for arg in input_arguments(&url, &hdrs)    { cmd.arg(arg); }
    if !audio_url.is_empty() {
        for arg in input_arguments(&audio_url, &audio_hdrs) { cmd.arg(arg); }
    }
    for arg in remux_arguments(with_adts) { cmd.arg(arg); }
    cmd.arg(&path);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    // `CREATE_NO_WINDOW` on Windows: an ffmpeg that flashes a console for the
    // length of a film is the packaged `.exe` behaving like a batch tool
    // (AGENTS.md §4.8's `console_flags` exists for exactly this).
    #[cfg(windows)]
    cmd.creation_flags(console_flags(None));

    match cmd.spawn() {
        Ok(child) => {
            let mut inner = relay.inner.lock().unwrap();
            inner.proc = Some(child);
            inner.state = RelayState::Running;
            inner.length = None;
            inner.ranges = false;
            inner.error = String::new();
            // The output is always fragmented MP4 regardless of the source.
            true
        }
        Err(e) => {
            let mut inner = relay.inner.lock().unwrap();
            inner.state = RelayState::Failed;
            inner.error = format!("无法启动 ffmpeg：{e}");
            false
        }
    }
}

fn watch_remux(relay: Arc<Relay>) {
    thread::Builder::new().name("RELAY_REMUX".into()).spawn(move || {
        loop {
            // Take the child so `wait()` never runs under the Mutex.
            let Some(mut child) = relay.inner.lock().unwrap().proc.take() else {
                return;
            };
            let stderr = drain_stderr(&mut child);
            let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

            let verdict = {
                let mut inner = relay.inner.lock().unwrap();
                if inner.state == RelayState::Failed {
                    // Killed on purpose; the entry is already gone.
                    return;
                }
                let size = std::fs::metadata(&inner.path).map(|m| m.len()).unwrap_or(0);
                if code == 0 && size > 0 {
                    inner.state  = RelayState::Complete;
                    inner.length = Some(size);
                    inner.ranges = true;
                    None
                } else if code != 0 && !inner.adtstoasc && tells_adts(&stderr) {
                    // Flip the latch, delete the junk file, and let the loop spawn
                    // once more. The latch is what makes "once" structural.
                    // The path is kept: `_spawn_remux` recomputes nothing.
                    inner.adtstoasc = true;
                    Some(inner.path.clone())
                } else {
                    inner.state = RelayState::Failed;
                    inner.error = remux_failure(code, &stderr);
                    None
                }
            };
            match verdict {
                // The retry owns the outcome from here.
                Some(path) => {
                    let _ = std::fs::remove_file(&path);
                    if !spawn_remux(&relay) { return; }
                }
                None => return,
            }
        }
    }).unwrap();
}

fn remux_failure(code: i32, stderr: &str) -> String {
    let tail = if stderr.is_empty() {
        String::new()
    } else {
        format!("：{}", last_line(stderr))
    };
    format!("转封装没有产出可用的文件（ffmpeg 退出码 {code}{tail}）")
}

/// Block until the remux has written at least one byte, or until `timeout` elapses.
///
/// The two budgets are parameters because they are the seam the reference has
/// (`remux_has_bytes(relay, timeout, poll)`), and without it the row that
/// matters most — **"no bytes yet" must not be a 200** — cannot be reached by a
/// test at all: every fixture with a file present returns true on the first
/// look, so a gate that was simply deleted would leave the suite green.
///
/// Measured on a relay whose ffmpeg had not created its output yet:
/// file absent → 0.0013 s and 0 bytes; file present but empty → held for the
/// whole budget. The first line is the bug: `FileNotFoundError` is an `OSError`,
/// so the "the viewer left" clause swallowed it *after* the status line and
/// headers had gone out — a 200 every player reads as an immediate EOF.
pub fn wait_for_bytes_within(relay: &Relay, timeout: Duration, poll: Duration) -> bool {
    let start = Instant::now();
    loop {
        {
            let inner = relay.inner.lock().unwrap();
            if inner.state == RelayState::Failed {
                return false;
            }
            if !inner.path.is_empty() {
                if let Ok(meta) = std::fs::metadata(&inner.path) {
                    if meta.len() > 0 {
                        return true;
                    }
                }
            }
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(poll);
    }
}

/// The production budget.
pub fn wait_for_bytes(relay: &Relay) -> bool {
    wait_for_bytes_within(relay, Duration::from_secs_f64(GROW_WAIT_SECONDS), GROW_POLL)
}

// ---------------------------------------------------------------------------
// HTTP handler (one connection at a time, plain std TcpStream)
// ---------------------------------------------------------------------------

/// A running relay HTTP server.
pub struct RelayServer {
    pub store: Arc<RelayStore>,
    pub port:  u16,
    pub temp_dir: PathBuf,
    /// How long a reader may wait for a growing file before the answer becomes
    /// "nothing yet" instead of a short read that looks like an EOF.
    ///
    /// A field rather than a constant so the row that matters — *no bytes yet
    /// must not be answered 200* — is reachable in a test without spending the
    /// production twenty seconds. Production uses [`GROW_WAIT_SECONDS`].
    pub grow_wait: Duration,
}

impl RelayServer {
    /// Bind a random port and start accepting connections.
    pub fn start(store: Arc<RelayStore>, temp_dir: PathBuf) -> io::Result<Self> {
        Self::start_with_budget(store, temp_dir, Duration::from_secs_f64(GROW_WAIT_SECONDS))
    }

    pub fn start_with_budget(
        store: Arc<RelayStore>,
        temp_dir: PathBuf,
        grow_wait: Duration,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("0.0.0.0:0")?;
        let port = listener.local_addr()?.port();
        // `ensure_server`'s order, and it is load-bearing: the sweep runs before
        // any viewer can reach the store, so a file it deletes is never one a
        // request is about to open.
        store.sweep_orphans(&temp_dir, RELAY_IDLE_TTL, now_secs());
        let store2 = Arc::clone(&store);
        let temp_dir2 = temp_dir.clone();
        thread::Builder::new().name("RELAY_HTTP".into()).spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let store3 = Arc::clone(&store2);
                        let dir = temp_dir2.clone();
                        thread::Builder::new().name("RELAY_CONN".into()).spawn(move || {
                            handle_connection(s, &store3, &dir, grow_wait);
                        }).ok();
                    }
                    Err(_) => break,
                }
            }
        })?;
        // And the loop that expires idle relays, last — Python puts it after the
        // listener too, so the first thing the store sees is a sweep, not a race.
        RelayStore::start_sweeper(&store, RELAY_SWEEP_INTERVAL);
        Ok(Self { store, port, temp_dir, grow_wait })
    }
}

// ---------------------------------------------------------------------------
// HTTP request / response helpers
// ---------------------------------------------------------------------------

fn send_response(stream: &mut TcpStream, status: u16, reason: &str,
                 extra_headers: &[(&str, String)], body: &[u8]) {
    let _ = write!(stream, "HTTP/1.0 {} {}\r\n", status, reason);
    for (k, v) in extra_headers {
        let _ = write!(stream, "{}: {}\r\n", k, v);
    }
    let _ = write!(stream, "Content-Length: {}\r\n", body.len());
    let _ = stream.write_all(b"\r\n");
    let _ = stream.write_all(body);
}

fn refuse(stream: &mut TcpStream, status: u16) {
    send_response(stream, status, reason_phrase(status), &[
        ("Content-Type", "text/plain;charset=utf-8".to_string()),
        ("Cache-Control", "no-store".to_string()),
        ("X-Content-Type-Options", "nosniff".to_string()),
    ], b"not here");
}

fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK", 206 => "Partial Content", 404 => "Not Found",
        405 => "Method Not Allowed", 410 => "Gone", 416 => "Range Not Satisfiable",
        500 => "Internal Server Error", 502 => "Bad Gateway", 503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn relay_from_path(path: &str, store: &RelayStore) -> Option<(Arc<Relay>, &'static str)> {
    let clean = path.split('?').next().unwrap_or("");
    let suffix = clean.strip_prefix("/relay/")?;
    let parts: Vec<&str> = suffix.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() != 2 { return None; }
    let (relay_id, tail) = (parts[0], parts[1]);
    if tail == "status" {
        return Some((store.peek(relay_id)?, "status"));
    }
    if tail == "media" {
        return Some((store.get(relay_id)?, "media"));
    }
    None
}

fn handle_connection(stream: TcpStream, store: &RelayStore, _temp_dir: &Path, grow_wait: Duration) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    let mut stream = stream;

    // A minimal HTTP/1.0 parser: read until the blank line after headers.
    use io::BufRead;
    let mut reader = io::BufReader::new(stream.try_clone().expect("clone"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() { return; }
    let parts: Vec<&str> = first.trim().splitn(3, ' ').collect();
    if parts.len() < 2 { return; }
    let method = parts[0].to_uppercase();
    let path   = parts[1].to_string();
    let mut req_headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() { break; }
        let t = line.trim();
        if t.is_empty() { break; }
        if let Some((k, v)) = t.split_once(':') {
            req_headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    drop(reader);

    if method == "POST" { refuse(&mut stream, 405); return; }
    if method != "GET" && method != "HEAD" { refuse(&mut stream, 405); return; }

    let Some((relay, tail)) = relay_from_path(&path, store) else {
        refuse(&mut stream, 404);
        return;
    };

    if tail == "status" {
        if method == "HEAD" { refuse(&mut stream, 405); return; }
        let status = relay.status_json();
        let body = serde_json::to_vec(&status).unwrap_or_default();
        send_response(&mut stream, 200, "OK", &[
            ("Content-Type", "application/json;charset=utf-8".to_string()),
            ("Cache-Control", "no-store".to_string()),
            ("X-Content-Type-Options", "nosniff".to_string()),
        ], &body);
        return;
    }

    // media endpoint
    {
        let inner = relay.inner.lock().unwrap();
        if inner.state == RelayState::Failed { drop(inner); refuse(&mut stream, 502); return; }
        if relay.mode == "remux" && inner.path.is_empty() { drop(inner); refuse(&mut stream, 410); return; }
    }
    if relay.mode == "remux" && !wait_for_bytes_within(&relay, grow_wait, GROW_POLL) {
        let state = relay.inner.lock().unwrap().state.clone();
        refuse(&mut stream, if state == RelayState::Failed { 502 } else { 503 });
        return;
    }

    let range_str = req_headers.get("range").cloned();
    let range_ref = range_str.as_deref();

    let (rl_ranges, rl_length) = {
        let inner = relay.inner.lock().unwrap();
        (inner.ranges, inner.length)
    };
    let sp = serve_plan(rl_ranges, rl_length, range_ref);

    if sp.status == 416 {
        let total_str = sp.total.map(|t| t.to_string()).unwrap_or_default();
        let _ = write!(stream, "HTTP/1.0 416 Range Not Satisfiable\r\n");
        let _ = write!(stream, "Content-Range: bytes */{}\r\n", total_str);
        let _ = write!(stream, "Accept-Ranges: bytes\r\n\r\n");
        return;
    }

    // Build response headers
    let seekable = relay.seekable();
    let content_type = relay.content_type();
    let _ = write!(stream, "HTTP/1.0 {} {}\r\n", sp.status, reason_phrase(sp.status));
    let _ = write!(stream, "Content-Type: {}\r\n", content_type);
    let _ = write!(stream, "Cache-Control: no-store\r\n");
    let _ = write!(stream, "X-Content-Type-Options: nosniff\r\n");
    let _ = write!(stream, "Accept-Ranges: {}\r\n", if seekable { "bytes" } else { "none" });
    if let Some(cl) = sp.content_length {
        let _ = write!(stream, "Content-Length: {}\r\n", cl);
    }
    if sp.partial {
        let end = sp.stop.unwrap_or(0);
        let total = sp.total.unwrap_or(0);
        let _ = write!(stream, "Content-Range: bytes {}-{}/{}\r\n", sp.start, end, total);
    }
    let _ = stream.write_all(b"\r\n");

    if method == "HEAD" { return; }

    if relay.mode == "remux" {
        pipe_file(&relay, sp.start, sp.remaining(), &mut stream, grow_wait);
    } else {
        let _ = pipe_upstream(&relay, sp.start, sp.remaining(), &mut stream);
    }
}

// ---------------------------------------------------------------------------
// piping
// ---------------------------------------------------------------------------

fn pipe_upstream(relay: &Relay, start: u64, remaining: Option<u64>, out: &mut TcpStream) -> io::Result<()> {
    let (rl_ranges, headers) = {
        let inner = relay.inner.lock().unwrap();
        (inner.ranges, relay.headers.clone())
    };
    let range_header: Option<String> = if rl_ranges && (start > 0 || remaining.is_some()) {
        let end = remaining.map(|r| (start + r - 1).to_string()).unwrap_or_default();
        Some(format!("bytes={}-{}", start, end))
    } else {
        None
    };

    let mut req = origin_agent().get(&relay.url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    if let Some(rh) = &range_header {
        req = req.header("Range", rh);
    }
    let response = req.call().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let mut body = response.into_body();
    let mut sent: u64 = 0;
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let limit = remaining.map(|r| r - sent).unwrap_or(u64::MAX).min(COPY_CHUNK as u64) as usize;
        if limit == 0 { break; }
        let n = body.as_reader().read(&mut buf[..limit])?;
        if n == 0 { break; }
        out.write_all(&buf[..n])?;
        sent += n as u64;
    }
    Ok(())
}

/// Read the growing file, waiting for more rather than answering short.
///
/// `grow_wait` is the **same** budget the pre-header wait uses, and it has to
/// be: both express one policy — how long a reader may sit still before "there
/// is nothing yet" is the honest answer. Two constants that could drift would
/// let the header wait say 20 s while the pipe gives up after 2, which is the
/// truncated-body answer the whole module exists to avoid.
fn pipe_file(
    relay: &Relay,
    start: u64,
    remaining: Option<u64>,
    out: &mut TcpStream,
    grow_wait: Duration,
) {
    let path = relay.inner.lock().unwrap().path.clone();
    let mut handle = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if start > 0 { let _ = handle.seek(SeekFrom::Start(start)); }
    let mut sent: u64 = 0;
    let mut waited = 0.0f64;
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let limit = remaining.map(|r| r - sent).unwrap_or(u64::MAX).min(COPY_CHUNK as u64) as usize;
        if limit == 0 { break; }
        match handle.read(&mut buf[..limit]) {
            Ok(0) => {
                let state = relay.inner.lock().unwrap().state.clone();
                if state == RelayState::Complete { break; }
                // Ahead of the encoder: *wait*, rather than answer short. A
                // short read is an EOF to every player that reads it.
                if waited >= grow_wait.as_secs_f64() { break; }
                thread::sleep(GROW_POLL);
                waited += GROW_POLL.as_secs_f64();
            }
            Ok(n) => {
                if out.write_all(&buf[..n]).is_err() { break; }
                sent += n as u64;
                waited = 0.0;
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// probe_proxy
// ---------------------------------------------------------------------------

/// The client this relay fetches origins with.
///
/// **No proxy, deliberately, and the reason is AGENTS.md §4.1's oldest row:** the
/// same machine that made mpv answer `HTTP error 502` for a LAN address because
/// mpv inherited `http_proxy`. An origin that only accepts a signed URL with a
/// Referer is exactly the case where a detour through a local proxy breaks, and
/// the symptom would again be "the TV says it won't play" rather than "we used
/// a proxy". The decision itself has one owner — `resolve::no_proxy_agent` —
/// and this only supplies the 30-second budget an origin probe can afford.
fn origin_agent() -> ureq::Agent {
    crate::resolve::no_proxy_agent(Duration::from_secs(30))
}

/// Ask the origin whether it honours Range, and how long the file is.
///
/// Updates `relay.inner` (and possibly `relay.content_type`) in place. The rule
/// for what counts as a refusal is Python's, and it is broader than the
/// refused-status set: **any** upstream error — any HTTP status, or not reaching
/// the origin at all — fails the relay and returns `false`
/// (`media_relay.py:480-483`). Treating a non-refused status as "open" would
/// register a relay that never got a byte from the origin, and the viewer would
/// then be served a 200 with nothing behind it.
pub fn probe_proxy(relay: &Relay) -> bool {
    let (url, headers) = (relay.url.clone(), relay.headers.clone());
    let mut req = origin_agent().get(&url).header("Range", RANGE_PROBE);
    for (k, v) in &headers { req = req.header(k, v); }
    let response = match req.call() {
        Ok(r)  => r,
        Err(ureq::Error::StatusCode(code)) => {
            let mut inner = relay.inner.lock().unwrap();
            inner.probe_error = format!("源站返回 {code}");
            inner.state = RelayState::Failed;
            return false;
        }
        Err(e) => {
            let mut inner = relay.inner.lock().unwrap();
            inner.probe_error = format!("无法连接源站：{e}");
            inner.state = RelayState::Failed;
            return false;
        }
    };
    let status = response.status();
    let ct = response.headers().get("Content-Type")
        .map(|v| v.to_str().unwrap_or("").split(';').next().unwrap_or("").trim().to_string());
    let cr = response.headers().get("Content-Range")
        .and_then(|v| v.to_str().ok().map(|s| s.to_string()));
    let cl = response.headers().get("Content-Length")
        .and_then(|v| v.to_str().ok().and_then(|s| s.parse::<u64>().ok()));

    // Python learns the declared type only when the suffix table gave it
    // nothing (`media_relay.py:494-497`): a `/relay/<id>/media` URL has no
    // suffix, so the origin's own `Content-Type` is the only statement about
    // what this is — and a sender decides whether to play it by reading exactly
    // that header (§4.1). The two questions are one lock-and-assign, not a read
    // then a second write, so a viewer that arrives between them never sees an
    // half-updated type.
    let declared = ct.filter(|t| !t.is_empty() && t != UNKNOWN_TYPE);
    if let Some(declared) = declared {
        let mut type_slot = relay.content_type.lock().unwrap();
        if type_slot.is_empty() || *type_slot == UNKNOWN_TYPE {
            *type_slot = declared;
        }
    }

    let mut inner = relay.inner.lock().unwrap();
    if status == 206 {
        inner.ranges = true;
        inner.length = cr.as_deref().and_then(total_from_content_range);
    } else {
        inner.ranges = false;
        inner.length = cl;
    }
    inner.state = RelayState::Open;
    true
}

fn total_from_content_range(value: &str) -> Option<u64> {
    let pos = value.rfind('/')?;
    value[pos+1..].trim().parse().ok()
}

// ---------------------------------------------------------------------------
// start_remux
// ---------------------------------------------------------------------------

/// Where ffmpeg lives on this machine, or `None` when it does not.
fn find_ffmpeg() -> Option<String> {
    find_command("ffmpeg", None, None, None)
}

pub fn start_remux(relay: &Arc<Relay>, temp_dir: &Path) -> bool {
    start_remux_with(relay, temp_dir, &find_ffmpeg)
}

/// `start_remux` with the binary lookup handed in.
///
/// A closure rather than a PATH edit for the reason `relay_tests::start_with`
/// already gives for taking the binary directly: these tests run in parallel
/// threads, and PATH is shared state a sibling clears out from under a case that
/// just set it. And a case that wants "this machine has no ffmpeg" cannot buy it
/// by emptying PATH at all — `find_command` also walks `extra_bin_dirs()` (brew,
/// `/usr/local/bin`, …), so the answer would depend on what happens to be
/// installed on the machine running the test. §4.2: the runner's accidental
/// contents are not a tested fact.
pub fn start_remux_with(
    relay: &Arc<Relay>,
    temp_dir: &Path,
    ffmpeg: &dyn Fn() -> Option<String>,
) -> bool {
    let Some(ffmpeg) = ffmpeg() else {
        let mut inner = relay.inner.lock().unwrap();
        inner.state = RelayState::Failed;
        inner.error = "这台机器没有 ffmpeg，无法把清单流转封装".to_string();
        return false;
    };
    let relay_id = &relay.relay_id;
    let path = temp_dir.join(format!("{}.mp4", relay_id))
        .to_string_lossy().into_owned();
    {
        let mut inner = relay.inner.lock().unwrap();
        inner.ffmpeg = ffmpeg;
        inner.path   = path;
        inner.adtstoasc = needs_adtstoasc(&inner.audio_codec);
    }
    if !spawn_remux(relay) { return false; }
    // Re-take child for watcher.
    let child = relay.inner.lock().unwrap().proc.take();
    if let Some(child) = child {
        // Put it back so watch_remux can take it.
        relay.inner.lock().unwrap().proc = Some(child);
        watch_remux(Arc::clone(relay));
    }
    true
}

// ---------------------------------------------------------------------------
// open_relay
// ---------------------------------------------------------------------------

fn new_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64url_encode(&bytes)
}

fn base64url_encode(bytes: &[u8]) -> String {
    let b64 = base64_encode(bytes);
    b64.trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u32;
        let b1 = if i+1 < bytes.len() { bytes[i+1] as u32 } else { 0 };
        let b2 = if i+2 < bytes.len() { bytes[i+2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if i+1 < bytes.len() { ALPHABET[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if i+2 < bytes.len() { ALPHABET[(n & 63) as usize] as char } else { '=' });
        i += 3;
    }
    out
}

/// Register one resolved candidate and return its relay (or the refusal sentence).
///
/// Mirrors Python `open_relay`, which has no ffmpeg seam at all: it just asks the
/// machine. See [`open_relay_with`] for what the refusal sentences are and why one
/// of them has no prefix.
pub fn open_relay(
    candidate: &Candidate,
    title: &str,
    store: &Arc<RelayStore>,
    temp_dir: &Path,
) -> Result<Arc<Relay>, String> {
    open_relay_with(candidate, title, store, temp_dir, &find_ffmpeg)
}

/// `open_relay` with the ffmpeg lookup handed in, so a case can say "this machine
/// has no ffmpeg" instead of hoping the runner has nothing installed. The remux
/// arm is the only one that reaches for it, so a proxy case never asks.
///
/// Probe/spawn happens synchronously so the management POST can say "origin
/// refused" in the same response.
///
/// The refused arms hand back **Python's caller's composition** —
/// `relay.error or relay.probe_error or '源站拒绝了这条地址'` (`protocol.py:2937`)
/// — with no 「中转启动失败：」 prefix. That prefix lives one line earlier in the
/// reference (`protocol.py:2933`), in the arm that catches an *unexpected* raise
/// out of `open_relay` itself; a Rust caller has no such arm, so prefixing it here
/// would move the sentence off the path that produces it and the page would read a
/// diagnosis of our own failure where the reference reports the origin's words.
/// Both branches share the chain because Python composes it once, after the mode
/// test: the fallback is reachable from either (a remux that fails without
/// recording a reason is as unanswered as a proxy that does).
pub fn open_relay_with(
    candidate: &Candidate,
    title: &str,
    store: &Arc<RelayStore>,
    temp_dir: &Path,
    ffmpeg: &dyn Fn() -> Option<String>,
) -> Result<Arc<Relay>, String> {
    let p = plan(candidate);
    let mode = if p == "merge" { "remux" } else { p }.to_string();
    let audio_codec = if !candidate.audio_codec.is_empty() {
        candidate.audio_codec.clone()
    } else {
        candidate.probe.audio_codec.clone().unwrap_or_default()
    };
    let relay = Arc::new(Relay {
        relay_id:     new_id(),
        url:          candidate.url.clone(),
        headers:      candidate.headers.clone(),
        mode:         mode.clone(),
        title:        if !title.is_empty() { title.to_string() }
                      else if !candidate.label.is_empty() { candidate.label.clone() }
                      else { candidate.host() },
        content_type: Mutex::new(content_type_for(candidate).to_string()),
        created:      now_secs(),
        last_seen:    Arc::new(Mutex::new(now_secs())),
        inner: Arc::new(Mutex::new(RelayInner {
            ranges:        false,
            length:        None,
            error:         String::new(),
            probe_error:   String::new(),
            path:          String::new(),
            state:         RelayState::Open,
            audio_url:     candidate.audio_url.clone().unwrap_or_default(),
            audio_headers: candidate.audio_headers.clone(),
            audio_codec,
            adtstoasc:     false,
            ffmpeg:        String::new(),
            proc:          None,
        })),
    });
    store.add(Arc::clone(&relay));
    if mode == "remux" {
        if !start_remux_with(&relay, temp_dir, ffmpeg) {
            store.drop_relay(&relay.relay_id);
            return Err(refusal(&relay.inner.lock().unwrap()));
        }
    } else {
        probe_proxy(&relay);
        let inner = relay.inner.lock().unwrap();
        if inner.state == RelayState::Failed {
            let why = refusal(&inner);
            drop(inner);
            store.drop_relay(&relay.relay_id);
            return Err(why);
        }
    }
    Ok(relay)
}

/// Python's `relay.error or relay.probe_error or '源站拒绝了这条地址'`: the first
/// non-empty word wins, and the last one is what the page reads when a relay died
/// without recording why. An empty message would render as 「」 plus a sentence
/// fragment, which is worse than saying the honest generic thing.
fn refusal(inner: &RelayInner) -> String {
    if !inner.error.is_empty() {
        return inner.error.clone();
    }
    if !inner.probe_error.is_empty() {
        return inner.probe_error.clone();
    }
    "源站拒绝了这条地址".to_string()
}

// ---------------------------------------------------------------------------
// advertise_host
// ---------------------------------------------------------------------------

/// The LAN address to put in the relay URL.
///
/// Two one-line seams (`advertisable_hosts` / `reachable_hosts`) allow tests to
/// supply a four-row machine table without pretending the test runner has a
/// second NIC.
pub fn choose_advertise_host(advertisable: &[String], reachable: &[String]) -> String {
    if advertisable.is_empty() {
        return "127.0.0.1".to_string();
    }
    let routed: HashSet<&str> = reachable.iter().map(|s| s.as_str()).collect();
    advertisable
        .iter()
        .find(|addr| routed.contains(addr.as_str()))
        .or_else(|| advertisable.first())
        .cloned()
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// Build the `/relay/<id>/media` URL for a relay.
pub fn media_url(relay: &Relay, host: &str, port: u16) -> String {
    format!("http://{}:{}/relay/{}/media", host, port, relay.relay_id)
}

// ---------------------------------------------------------------------------
// relay_tests module wiring
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "relay_tests.rs"]
mod relay_tests;
