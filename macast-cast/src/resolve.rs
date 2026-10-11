//! Page-address resolution for Macast's 「网页地址投屏」.
//!
//! Python counterpart: `macast/media_resolve.py`. The references are files that
//! grew every lesson in AGENTS.md §4.14 into them, so this module is ported in
//! layers rather than all at once: constants and suffix tables first, then the
//! two engines (HTML scraping + yt-dlp JSON), then the admission/ranking tables
//! ported from castor (MIT) with two divergences marked at their rows, then
//! cookie jar handling and the classification tables.
//!
//! ## What this module owns, and what it deliberately does not
//!
//! It owns **page-address resolution**: taking a URL (or a share-text blob that
//! contains a URL) and producing a list of candidate media URLs with the headers
//! that make each URL readable. The relay that serves those candidates to devices
//! lives in [`crate::relay`], and the two are separate because a candidate that
//! is good for one request gets handed to a device which will fetch it minutes
//! later, from another IP, with no Referer — and "the TV says it won't play" is
//! the symptom.
//!
//! It does **not** own the HTTP server that serves candidates, the ffmpeg
//! transmux that grows a file, or the device discovery that picks which renderer
//! to send to. Those live in [`crate::relay`] and `macast_upnp`.
//!
//! ## Two engines, merged into one candidate list
//!
//! Because they fail in complementary ways (measured 2026-10 against the castor
//! fixtures):
//!
//! * **yt-dlp**: ~1800 sites, needs the binary, 12-19 s on a real video page,
//!   and it reports the *signed* address together with the headers the origin
//!   demanded. Answers `ERROR: Unsupported URL` for a small self-hosted site.
//! * **scrape**: stdlib only, microseconds, and structurally blind: it sees only
//!   what the HTML states, so a page that builds its `src` in JavaScript gives
//!   it nothing.
//!
//! A browser that observes network traffic (castor's third engine, the one that
//! caught the JS-only case in 2 s where `yt-dlp -g` failed outright) is
//! deliberately absent: that means shipping CDP. Until phase B exists, this
//! module's coverage hole is stated in the settings page instead of hidden.
//!
//! ## The one thing to know before editing
//!
//! Every candidate is served from the Macast side ([`crate::relay`]). That is a
//! decision, not a limitation: an address that is good for one request gets
//! handed to a device which will fetch it minutes later, from another IP, with
//! no Referer — and "the TV says it won't play" is the symptom.

use regex::Regex;
use serde_json::Value;
use std::sync::{Arc, LazyLock, Mutex};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// A page body is read to this ceiling and no further. Video sites inline a
/// stylesheet the size of a novel; the addresses we want are in the first blocks.
pub const PAGE_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The three waits. yt-dlp is the slow one on a real site (castor's browser path
/// needed 2 s where `yt-dlp -J` needed 12-19 s), which is why a resolve runs in a
/// thread and the page polls `query=resolve-status` rather than holding a request.
pub const SCRAPE_TIMEOUT_SECS: u64 = 15;
pub const YTDLP_TIMEOUT_SECS: u64 = 120;
/// Generous for one ffprobe because a manifest probe fetches the playlist *and* a
/// segment before it will report anything.
pub const PROBE_TIMEOUT_SECS: u64 = 25;

/// Measuring costs a fetch each, so it is capped: yt-dlp returns dozens of
/// renditions of the same title and the user is not learning anything after the
/// first few.
pub const MAX_MEASURED: usize = 8;
/// ... and no more than this many from one host, so one site's ladder cannot
/// crowd out the other engine's only candidate.
pub const MAX_MEASURED_PER_HOST: usize = 5;

/// How long a name we hand to a player or a DIDL `<dc:title>` may be. One owner
/// because a name is read from four places (the page's `<title>`, yt-dlp's
/// `title`, the address, and a Cast sender's `metadata.title`), and a limit that
/// lives in only one of them is a limit the other three do not have.
pub const TITLE_MAX_CHARS: usize = 120;

/// Containers that name a document listing segments, versus a file a reader can
/// walk with byte offsets.
pub const SEGMENTED_SUFFIXES: &[&str] = &[".m3u8", ".mpd"];
pub const PROGRESSIVE_SUFFIXES: &[&str] = &[
    ".mp4", ".m4v", ".mov", ".mkv", ".webm", ".flv", ".ts",
];
/// Sound on its own is still castable, so these are candidates too.
pub const AUDIO_SUFFIXES: &[&str] = &[
    ".mp3", ".m4a", ".aac", ".ogg", ".opus", ".wav", ".flac",
];

/// Progressive containers + audio-only files.
pub static PROGRESSIVE: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    PROGRESSIVE_SUFFIXES
        .iter()
        .chain(AUDIO_SUFFIXES.iter())
        .copied()
        .collect()
});

/// All media suffixes this module recognizes.
pub static MEDIA_SUFFIXES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    SEGMENTED_SUFFIXES
        .iter()
        .chain(PROGRESSIVE.iter())
        .copied()
        .collect()
});

/// A GUI app started from Finder/Dock does not inherit the shell's PATH, so a
/// brew-installed yt-dlp or ffprobe looks missing even though the user has it.
/// The same directories `macast_ytdlp.py` walks, for the same reason.
pub fn extra_bin_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/local/bin"),
        PathBuf::from(format!("{home}/.local/bin")),
    ]
}

/// Path of an external command, or `None` when this machine has nothing usable.
/// All four parameters are seams: the suite runs on macOS and needs to ask
/// "what would a Windows machine find" without being one.
/// Looked up per use, not at import: installing yt-dlp while Macast runs should
/// take effect without a restart.
pub fn find_command(
    name: &str,
    platform: Option<&str>,
    extra_dirs: Option<&[PathBuf]>,
    env_path: Option<&str>,
) -> Option<String> {
    let plat = platform.unwrap_or(if cfg!(windows) { "win32" } else { "other" });
    let fallback_path = std::env::var("PATH").unwrap_or_default();
    let search = env_path.unwrap_or(&fallback_path);
    let names: Vec<String> = if plat == "win32" {
        vec![name.to_string(), format!("{name}.exe")]
    } else {
        vec![name.to_string()]
    };
    // Python: `filter(None, search.split(os.pathsep))` — an empty PATH segment
    // ("foo::bar", or a trailing ':') yields '' and os.path.join('', name) is a
    // bare relative name. split_paths already drops nothing, so filter here.
    for entry in std::env::split_paths(search).filter(|entry| !entry.as_os_str().is_empty()) {
        for filename in &names {
            let candidate = entry.join(filename);
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    for entry in extra_dirs.unwrap_or(&extra_bin_dirs()) {
        for filename in &names {
            let candidate = entry.join(filename);
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// `creationflags` so a probe never flashes a terminal on Windows.
/// Every helper here is a console program and the packaged .exe is built
/// `--noconsole`.
#[cfg(windows)]
pub fn console_flags(platform: Option<&str>) -> u32 {
    let plat = platform.unwrap_or("win32");
    if plat == "win32" { 0x0800_0000 } else { 0 }
}

#[cfg(not(windows))]
pub fn console_flags(platform: Option<&str>) -> u32 {
    let plat = platform.unwrap_or("other");
    if plat == "win32" { 0x0800_0000 } else { 0 }
}

/// The User-Agent we present to pages and ffprobe. A wrong one here is a site
/// that returns a login wall instead of a video URL.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// castor's `Reach`: how far reading the origin got, as a fact about the origin
/// rather than about the media.
pub const REACH_UNPROVEN: &str = "unproven";
pub const REACH_OPENED: &str = "opened";
pub const REACH_REFUSED: &str = "refused";

/// ffprobe states a refusal in its own words, and only these four codes count as
/// one. Anything else (timeout, TLS, DNS, a 500, a 503) is *unproven*, which the
/// admission table keeps as a last resort — a slow or briefly unhappy origin must
/// cost the user a worse first choice, not no choice. The phrasings ffprobe uses
/// (`http error 404 not found`, `Server returned 403 Forbidden`) always carry the
/// digits, so the codes are the whole rule; matching on the words alone would make
/// a 500 a refusal and quietly delete the row above this one's rationale.
/// `media_relay.REFUSED_STATUS` is the same set for the same reason, and Part 59
/// pins the two against each other.
pub static REFUSED_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:40[134]|410)\b").unwrap()
});

/// Codecs ffprobe reports for a poster image embedded as a video track. A
/// candidate whose only "video" is one of these is a still image, not a video,
/// and the admission table drops it.
pub static STILL_IMAGE_CODECS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "png", "apng", "mjpeg", "jpeg", "jpegls", "bmp", "gif", "tiff", "webp", "ppm",
    ]
    .into_iter()
    .collect()
});

/// Headers that belong to *this* request, not the upstream one. Stripped before
/// forwarding so a `Host: 127.0.0.1:58880` from the relay's own fetch does not
/// arrive at the origin.
pub static UNFORWARDABLE_HEADERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "host",
        "content-length",
        "content-type",
        "connection",
        "proxy-connection",
        "upgrade",
        "te",
        "trailer",
        "transfer-encoding",
        "accept-encoding",
    ]
    .into_iter()
    .collect()
});

/// What ffprobe reported about one candidate: duration, codecs, whether it
/// refused with a status code we recognize, and how far we got.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub duration: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub bitrate: Option<f64>,
    pub reach: String,
    pub container: Option<String>,
    pub error: Option<String>,
    pub is_live: bool,
}

impl Probe {
    pub fn new() -> Self {
        Self {
            reach: REACH_UNPROVEN.to_string(),
            ..Default::default()
        }
    }

    /// True if ffprobe actually opened the URL and got far enough to report
    /// codecs or duration. A refusal or a timeout is not measured.
    pub fn measured(&self) -> bool {
        self.reach == REACH_OPENED
    }

    /// True if the probe found a video codec that is not a still image format.
    /// A row without a video codec (audio-only) is not moving picture.
    /// Python: `bool(self.video_codec) and self.video_codec not in STILL...` —
    /// an *empty* codec is falsy there, so it is not a moving picture either.
    pub fn moving_picture(&self) -> bool {
        self.video_codec
            .as_ref()
            .is_some_and(|c| !c.is_empty() && !STILL_IMAGE_CODECS.contains(c.as_str()))
    }
}

/// One candidate media URL with its headers, probe results, and the engine that
/// found it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub title: Option<String>,
    pub probe: Probe,
    /// Which engine produced this candidate: `"ytdlp"` or `"page"`.
    /// Maps to Python's `origin` field.
    pub engine: String,
    /// Human-readable quality label (e.g. "1080p · 2.4 Mbps").
    pub label: String,
    /// DASH shape: video and audio are separate URLs, and the relay merges them.
    pub audio_url: Option<String>,
    pub audio_headers: HashMap<String, String>,
    /// Audio codec name reported by the probe or the engine (e.g. "aac", "mp4a").
    pub audio_codec: String,
    /// Human-readable audio label (e.g. "aac 192 k").
    pub audio_label: String,
    /// True if this row survived admission only as a last resort (unproven or
    /// header-only). Last-resort rows appear below measured ones.
    pub last_resort: bool,
    /// Why this candidate was kept or demoted — a short phrase the card shows.
    pub reason: String,
}

impl Candidate {
    pub fn new(url: String, engine: String) -> Self {
        Self {
            url,
            headers: HashMap::new(),
            title: None,
            probe: Probe::new(),
            engine,
            label: String::new(),
            audio_url: None,
            audio_headers: HashMap::new(),
            audio_codec: String::new(),
            audio_label: String::new(),
            last_resort: false,
            reason: String::new(),
        }
    }

    /// True if the probe measured this candidate and found moving picture but
    /// no audio track and no separate audio URL. Only a *measured* row may
    /// answer this — an unmeasured row is not silent, it is unknown.
    /// Python: `not probe.audio_codec and not self.audio_url` — an empty string
    /// is "no sound" there, and `''` is exactly what "nobody has said" collapses
    /// to in this port, so the empty form must answer silent too.
    pub fn silent(&self) -> bool {
        self.probe.measured()
            && self.probe.moving_picture()
            && self
                .probe
                .audio_codec
                .as_deref()
                .map_or(true, |a| a.is_empty())
            && self.audio_url.as_deref().map_or(true, |u| u.is_empty())
    }

    /// Host (with port, if any) of the candidate URL, lowercased.
    /// Maps to Python's `host` property (`urlsplit().netloc.lower()`).
    ///
    /// Python leaves this unguarded — `urlsplit` can raise `ValueError` on a
    /// bracketed netloc and the caller never expects it. We answer `""` for a
    /// URL the Python split would refuse (this runs on untrusted URLs during
    /// ranking, where a panic is not an option).
    pub fn host(&self) -> String {
        match py_urlsplit(&self.url, "", true) {
            Ok(split) => split.netloc.to_lowercase(),
            Err(_) => String::new(),
        }
    }

    /// The media suffix of the URL path, or empty string if none matches.
    /// Maps to Python's `path_suffix` property.
    ///
    /// Same unguarded call as `host` on the Python side, same answer for a
    /// refused URL: no suffix matches.
    pub fn path_suffix(&self) -> &'static str {
        match py_urlsplit(&self.url, "", true) {
            Ok(split) => {
                let path = split.path.to_lowercase();
                for suffix in MEDIA_SUFFIXES.iter() {
                    if path.ends_with(*suffix) {
                        return suffix;
                    }
                }
                ""
            }
            Err(_) => "",
        }
    }

    /// True if the URL path ends in a segmented-container suffix (.m3u8, .mpd).
    pub fn segmented(&self) -> bool {
        SEGMENTED_SUFFIXES.contains(&self.path_suffix())
    }

    /// True if the URL path ends in an audio-only suffix.
    pub fn audio_only(&self) -> bool {
        AUDIO_SUFFIXES.contains(&self.path_suffix())
    }

    /// Video height from the probe, or 0 if unknown.
    pub fn height(&self) -> u32 {
        self.probe.video_height.unwrap_or(0)
    }

    /// Video width from the probe, or 0 if unknown.
    pub fn width(&self) -> u32 {
        self.probe.video_width.unwrap_or(0)
    }

    /// Duration from the probe, or 0.0 if unknown.
    pub fn duration(&self) -> f64 {
        self.probe.duration.unwrap_or(0.0)
    }

    /// True if this candidate should appear in the card. Silent candidates are
    /// filtered out after merging (so DASH pairs survive), but a candidate that
    /// is *still* silent after merging is a true "video without sound" and does
    /// not appear.
    pub fn visible(&self) -> bool {
        !self.silent()
    }
}

/// Unescape the three JSON escape sequences that appear in PHP-generated pages:
/// `\\`, `\/`, and `\uXXXX` (ASCII only). Other code points are left as-is
/// because they are someone's title, and rewriting it would make the scanned
/// text differ from the page.
pub fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() == 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(code) = u32::from_str_radix(&hex, 16) {
                            if code < 0x80 {
                                if let Some(ch) = char::from_u32(code) {
                                    out.push(ch);
                                    continue;
                                }
                            }
                        }
                    }
                    // Malformed or non-ASCII: keep the original escape.
                    out.push_str("\\u");
                    out.push_str(&hex);
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whichever of `\t` `\r` `\n` urlsplit removes wholesale from the raw string.
/// Python removes exactly these three before looking for a scheme, so they can
/// never appear in the parts (the WHATWG-trim set, not the whole C0 range).
fn strip_tab_cr_lf(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect()
}

/// The C0-control-or-space prefix urlsplit strips from the *left* only.
/// Python's comment: "Only lstrip url as some applications rely on preserving
/// trailing space." (`_WHATWG_C0_CONTROL_OR_SPACE = range(0, 0x21)`.)
fn is_c0_control_or_space(c: char) -> bool {
    c <= '\u{20}'
}

/// Python's `str.isspace` set: exactly these 29 code points. Rust's
/// `char::is_whitespace` has 25 — it misses U+001C..U+001F — so every place the
/// port asks "is this Python whitespace" goes through here.
fn py_is_space(c: char) -> bool {
    matches!(
        c,
        '\u{09}'..='\u{0d}'
            | '\u{1c}'..='\u{20}'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Python `str.strip()` with no argument. `pub(crate)` because `html.rs` needs
/// the *same* predicate as `parse_starttag`'s `rawdata[k:endpos].strip()`: the
/// parser's regular expressions use the five-character class `[\t\n\r\f /]`,
/// while this is the 29-code-point set — nothing may unify the two.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// Python `len(s.split())` — runs of non-whitespace separated by its 29-code-point
/// space set.
fn py_token_count(s: &str) -> usize {
    s.split(py_is_space)
        .filter(|t| !t.is_empty())
        .count()
}

/// `os.path.basename` with `posixpath` semantics — `path.rpartition('/')[2]`.
/// The oracle runs on macOS and a URL path is slash-separated by construction
/// (castor, whose reading this layer follows, also uses `path.Base`). A Windows
/// *Python* build would use `ntpath` and also split on `\`; the port pins the
/// measured behaviour, so a backslash stays part of the name everywhere.
fn py_basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Python `urllib.parse.unquote` (UTF-8, `errors='replace'`). Percent escapes
/// are only read inside ASCII runs: a non-ASCII character passes through
/// untouched and also breaks any escape sequence that would span it — `é%E7`
/// is one literal `é` plus one replacement character, never a two-byte
/// sequence. Decoding is per run, so replacement characters are counted per
/// run too, exactly as `unquote_to_bytes(...).decode(...)` counts them.
fn py_unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    for c in s.chars() {
        if c.is_ascii() {
            run.push(c);
        } else {
            out.push_str(&unquote_ascii_run(&run));
            run.clear();
            out.push(c);
        }
    }
    out.push_str(&unquote_ascii_run(&run));
    out
}

/// One ASCII run of `unquote`: `unquote_to_bytes`'s split-on-`b'%'` walk, then
/// the run's bytes decoded as UTF-8 with replacement. An item contributes a
/// byte only when its first two characters are hex digits (`_hextobyte`), so
/// `%zz` and a trailing `%` stay literal; the remainder after a decoded byte is
/// never re-scanned because the split already removed every `%`.
fn unquote_ascii_run(run: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(run.len());
    let mut parts = run.split('%');
    bytes.extend_from_slice(parts.next().unwrap_or("").as_bytes());
    for item in parts {
        let b = item.as_bytes();
        let decoded = if b.len() >= 2 && b[0].is_ascii_hexdigit() && b[1].is_ascii_hexdigit() {
            let hi = (b[0] as char).to_digit(16).unwrap();
            let lo = (b[1] as char).to_digit(16).unwrap();
            Some(((hi << 4) | lo) as u8)
        } else {
            None
        };
        match decoded {
            Some(byte) => {
                bytes.push(byte);
                bytes.extend_from_slice(&b[2..]);
            }
            None => {
                bytes.push(b'%');
                bytes.extend_from_slice(b);
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Python's `ValueError` from `urlsplit` / `urljoin` / the `ipaddress` module.
/// The port keeps the failure shape — callers that mirror Python's `try/except`
/// catch it, and callers that mirror its *unguarded* calls turn it into the
/// fallback value they use instead of a traceback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyUrlError;

impl std::fmt::Display for PyUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid URL")
    }
}

impl std::error::Error for PyUrlError {}

/// Python's `urllib.parse.uses_relative` / `uses_netloc` / `uses_params`,
/// verbatim (the leading `""` classifies scheme-less URLs).
const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];

const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https",
    "shttp", "snews", "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp",
    "nfs", "git", "git+ssh", "ws", "wss", "itms-services",
];

const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// Result of urlsplit's parsing step: the scheme is already lowercased and the
/// netloc has passed the bracket checks; the path/query/fragment are the raw
/// substrings Python would have split out.
struct PySplit {
    scheme: String,
    netloc: String,
    path: String,
    query: String,
    fragment: String,
}

/// `urllib.parse.urlsplit(url)`, ported whole — including the cache-free shape
/// (the memo in Python is unobservable) and the `allow_fragments=True` default.
fn py_urlsplit(url: &str, scheme: &str, allow_fragments: bool) -> Result<PySplit, PyUrlError> {
    // Only lstrip url as some applications rely on preserving trailing space.
    let url: String = url
        .trim_start_matches(is_c0_control_or_space)
        .to_string();
    // The scheme argument is the *default* scheme, not a candidate: when the
    // url carries none, this value is what the result reports.
    let scheme = scheme.trim_matches(is_c0_control_or_space).to_string();
    let url = strip_tab_cr_lf(&url);
    let scheme = strip_tab_cr_lf(&scheme);

    let mut split = PySplit {
        scheme,
        netloc: String::new(),
        path: String::new(),
        query: String::new(),
        fragment: String::new(),
    };

    // Scheme detection: i > 0, first byte is ASCII (Python checks
    // `url[0].isascii()` then `url[0].isalpha()`), and the head is all
    // scheme chars.
    if let Some(i) = url.find(':') {
        if i > 0 {
            let head = &url[..i];
            let first = head.as_bytes()[0];
            if first.is_ascii() && first.is_ascii_alphabetic()
                && head.chars().all(scheme_chars_contains)
            {
                split.scheme = head.to_ascii_lowercase();
                let rest = url[i + 1..].to_string();
                return PySplitResult::finish(rest, split, allow_fragments);
            }
        }
    }
    PySplitResult::finish(url, split, allow_fragments)
}

fn scheme_chars_contains(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// The tail of urlsplit after the scheme (or the whole string when there is no
/// scheme): the `//` netloc branch, then `#`, then `?`.
struct PySplitResult;

impl PySplitResult {
    fn finish(
        mut rest: String,
        mut split: PySplit,
        allow_fragments: bool,
    ) -> Result<PySplit, PyUrlError> {
        if rest.starts_with("//") {
            let (netloc, after) = split_netloc(&rest, 2).map_err(|_| PyUrlError)?;
            rest = after;
            // A url with bracketed IPv6 literals in netloc must have matching
            // brackets; otherwise the ValueError("Invalid IPv6 URL") only
            // fires when a present bracket is also used.
            let has_open = netloc.contains('[');
            let has_close = netloc.contains(']');
            if has_open != has_close {
                // ValueError(f'Invalid IPv6 URL')
                return Err(PyUrlError);
            }
            if has_open {
                check_bracketed_netloc(&netloc)?;
            }
            split.netloc = netloc;
        }
        if allow_fragments && rest.contains('#') {
            if let Some(i) = rest.find('#') {
                split.fragment = rest[i + 1..].to_string();
                rest.truncate(i);
            }
        }
        if rest.contains('?') {
            if let Some(i) = rest.find('?') {
                split.query = rest[i + 1..].to_string();
                rest.truncate(i);
            }
        }
        split.path = rest;
        check_netloc(&split.netloc)?;
        Ok(split)
    }
}

/// Python `_splitnetloc`: netloc ends at the first `/`, `?` or `#` after
/// `start`; when none is found the rest of the string is the netloc.
/// Returns Err only when `start` slices mid-character (Python would raise
/// `TypeError`; ours is a `String`, so this cannot practically happen).
fn split_netloc(url: &str, start: usize) -> Result<(String, String), ()> {
    if !url.is_char_boundary(start) {
        return Err(());
    }
    let mut delim = url.len();
    for c in ['/', '?', '#'] {
        if let Some(i) = url[start..].find(c) {
            let i = start + i;
            if i < delim {
                delim = i;
            }
        }
    }
    Ok((url[start..delim].to_string(), url[delim..].to_string()))
}

/// `urllib.parse.urlparse` — urlsplit plus parameter splitting for schemes in
/// `uses_params` (http and https are). Returns `(url_without_params, params)`.
fn py_urlparse(
    url: &str,
    scheme: &str,
    allow_fragments: bool,
) -> Result<(PySplit, String), PyUrlError> {
    let split = py_urlsplit(url, scheme, allow_fragments)?;
    if USES_PARAMS.contains(&split.scheme.as_str()) && split.path.contains(';') {
        let (path, params) = split_params(&split.path);
        let mut split = split;
        split.path = path;
        Ok((split, params))
    } else {
        Ok((split, String::new()))
    }
}

/// Python `_splitparams`: with a `/` present the separator is the first `;`
/// after the last `/`; with no `/` at all it is the first `;` anywhere.
fn split_params(url: &str) -> (String, String) {
    match url.rfind('/') {
        Some(last_slash) => match url[last_slash..].find(';') {
            Some(rel) => {
                let i = last_slash + rel;
                (url[..i].to_string(), url[i + 1..].to_string())
            }
            None => (url.to_string(), String::new()),
        },
        None => match url.find(';') {
            Some(i) => (url[..i].to_string(), url[i + 1..].to_string()),
            None => (url.to_string(), String::new()),
        },
    }
}

/// `urllib.parse.urlunparse` (params non-empty re-attaches a `;` segment).
fn py_urlunparse(
    scheme: &str,
    netloc: &str,
    path: &str,
    params: &str,
    query: &str,
    fragment: &str,
) -> String {
    let url = if params.is_empty() {
        path.to_string()
    } else {
        format!("{};{}", path, params)
    };
    py_urlunsplit(scheme, netloc, &url, query, fragment)
}

/// `urllib.parse.urlunsplit` — the three netloc branches decide when a `//`
/// prefix is added, and the scheme prefix is glued on *after* them (so a
/// schemeless `//x` becomes `////x`, which is the shape Python produces).
fn py_urlunsplit(scheme: &str, netloc: &str, url: &str, query: &str, fragment: &str) -> String {
    let mut url = url.to_string();
    if !netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{}", url);
        }
        url = format!("//{}{}", netloc, url);
    } else if url.starts_with("//") {
        url = format!("//{}", url);
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (url.is_empty() || url.starts_with('/'))
    {
        url = format!("//{}", url);
    }
    if !scheme.is_empty() {
        url = format!("{}:{}", scheme, url);
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

/// `urllib.parse.urljoin(base, url)`. Ported whole, including the two early
/// returns that hand back a string *without re-serialising it* — Python returns
/// the `url` argument object as-is when the schemes differ, and the base string
/// as-is when `url` is empty.
fn py_urljoin(base: &str, url: &str) -> Result<String, PyUrlError> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let (b, bparams) = py_urlparse(base, "", true)?;
    // The url side is parsed with the base's scheme as its default.
    let (u, params) = py_urlparse(url, &b.scheme, true)?;
    let scheme = u.scheme.as_str();
    let bscheme = b.scheme.as_str();
    if scheme != bscheme || !USES_RELATIVE.contains(&scheme) {
        return Ok(url.to_string());
    }
    let mut netloc = u.netloc.clone();
    if USES_NETLOC.contains(&scheme) {
        if !netloc.is_empty() {
            return Ok(py_urlunparse(
                scheme,
                &netloc,
                &u.path,
                &params,
                &u.query,
                &u.fragment,
            ));
        }
        netloc = b.netloc.clone();
    }
    if u.path.is_empty() && params.is_empty() {
        let query = if u.query.is_empty() {
            b.query.clone()
        } else {
            u.query.clone()
        };
        return Ok(py_urlunparse(
            scheme,
            &netloc,
            &b.path,
            &bparams,
            &query,
            &u.fragment,
        ));
    }
    // Merge paths for relative references.
    let segments: Vec<String>;
    if u.path.starts_with('/') {
        segments = u.path.split('/').map(|s| s.to_string()).collect();
    } else {
        let mut base_parts: Vec<String> = b.path.split('/').map(|s| s.to_string()).collect();
        if base_parts.last().map(|s| !s.is_empty()).unwrap_or(false) {
            base_parts.pop();
        }
        let mut segs = base_parts;
        segs.extend(u.path.split('/').map(|s| s.to_string()));
        // Python: `segments[1:-1] = filter(None, segments[1:-1])` — only on
        // this branch (the `path.startswith('/')` branch above never empties).
        if segs.len() > 2 {
            let filtered: Vec<String> = segs[1..segs.len() - 1]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect();
            segs.splice(1..segs.len() - 1, filtered);
        }
        segments = segs;
    }
    let mut resolved_path: Vec<String> = Vec::new();
    for seg in &segments {
        if seg == ".." {
            resolved_path.pop();
        } else if seg == "." {
            continue;
        } else {
            resolved_path.push(seg.clone());
        }
    }
    if matches!(segments.last().map(|s| s.as_str()), Some(".") | Some("..")) {
        resolved_path.push(String::new());
    }
    let joined = resolved_path.join("/");
    Ok(py_urlunparse(
        scheme,
        &netloc,
        if joined.is_empty() { "/" } else { &joined },
        &params,
        &u.query,
        &u.fragment,
    ))
}

/// The 19 code points `_checknetloc` searches for after NFKC normalisation.
/// NFKC-expanded labels are the cheap version of Python's
/// `unicodedata.normalize('NFKC', netloc)`: if a dangerous character is not
/// literally present, Python only rejects when its NFKC expansion would
/// introduce one, and this table holds exactly the expansions that matter.
const NETLOC_DANGER: &[(char, &str)] = &[
    ('\u{2047}', "??"),
    ('\u{2048}', "?!"),
    ('\u{2049}', "!?"),
    ('\u{2100}', "a/c"),
    ('\u{2101}', "a/s"),
    ('\u{2105}', "c/o"),
    ('\u{2106}', "c/u"),
    ('\u{2a74}', "::="),
    ('\u{fe13}', ":"),
    ('\u{fe16}', "?"),
    ('\u{fe55}', ":"),
    ('\u{fe56}', "?"),
    ('\u{fe5f}', "#"),
    ('\u{fe6b}', "@"),
    ('\u{ff03}', "#"),
    ('\u{ff0f}', "/"),
    ('\u{ff1a}', ":"),
    ('\u{ff1f}', "?"),
    ('\u{ff20}', "@"),
];

/// Python `_checknetloc`: an all-ASCII netloc is always fine; otherwise, any
/// code point whose NFKC expansion contains one of `/:?#@` is a ValueError.
fn check_netloc(netloc: &str) -> Result<(), PyUrlError> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    for (c, expansion) in NETLOC_DANGER {
        if netloc.contains(*c)
            && expansion
                .chars()
                .any(|e| matches!(e, '/' | '?' | '#' | '@' | ':'))
        {
            return Err(PyUrlError);
        }
    }
    Ok(())
}

/// Python `_check_bracketed_netloc`: inside the `[...]` the hostname is what
/// sits before `:port`, and the hostname still has to pass
/// `_check_bracketed_host`.
fn check_bracketed_netloc(netloc: &str) -> Result<(), PyUrlError> {
    let hostname_and_port = netloc.rsplit_once('@').map(|(_, p)| p).unwrap_or(netloc);
    match hostname_and_port.split_once('[') {
        Some((before_bracket, bracketed)) => {
            if !before_bracket.is_empty() {
                return Err(PyUrlError);
            }
            let hostname = match bracketed.split_once(']') {
                Some((h, port)) => {
                    if !port.is_empty() && !port.starts_with(':') {
                        return Err(PyUrlError);
                    }
                    h
                }
                None => bracketed,
            };
            check_bracketed_host(hostname)
        }
        None => {
            let hostname = hostname_and_port
                .split_once(':')
                .map(|(h, _)| h)
                .unwrap_or(hostname_and_port);
            check_bracketed_host(hostname)
        }
    }
}

/// Python `_check_bracketed_host` with the `ipaddress` module's rules inlined:
/// IPvFuture (`v[hex]+.` + tail) is one check; otherwise an IPv4 inside
/// brackets is refused (Python refuses it *after* `ip_address` parses it), and
/// otherwise it must parse as IPv6 under `IPv6Address.__init__`'s extras —
/// `/` refused before any parsing, scope id (after `%`) non-empty and
/// `%`-free, address parseable by Rust's std (verified equivalent against the
/// Python 3.12 oracle over the corpus, 45/45).
fn check_bracketed_host(hostname: &str) -> Result<(), PyUrlError> {
    if hostname.starts_with('v') {
        // IPvFuture: at least one hex digit, then '.', then a non-empty tail
        // with no newline. `re.match(r"\Av[a-fA-F0-9]+\..+\Z")` — `.+` cannot
        // cross a newline without `re.S`.
        let b = hostname.as_bytes();
        let mut pos = 1;
        while pos < b.len() && b[pos].is_ascii_hexdigit() {
            pos += 1;
        }
        let tail_ok = pos > 1
            && pos < b.len()
            && b[pos] == b'.'
            && hostname.len() > pos + 1
            && !hostname[pos + 1..].contains('\n');
        return if tail_ok { Ok(()) } else { Err(PyUrlError) };
    }
    // IPv4-in-brackets deliberately fails (checked before IPv6 so the error
    // shape matches Python's two separate raises).
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(PyUrlError);
    }
    if py_ipv6_parses(hostname) {
        Ok(())
    } else {
        Err(PyUrlError)
    }
}

/// `ipaddress.IPv6Address` for the cases `_check_bracketed_host` can present:
/// `/` anywhere is rejected before scope splitting (`IPv6Address.__init__`
/// checks `'/' in addr_str` first), the scope id after `%` must be non-empty
/// and contain no second `%`, and the address half must parse as IPv6.
fn py_ipv6_parses(s: &str) -> bool {
    if s.contains('/') {
        return false;
    }
    let addr = match s.split_once('%') {
        Some((a, scope)) => {
            if scope.is_empty() || scope.contains('%') {
                return false;
            }
            a
        }
        None => s,
    };
    addr.parse::<std::net::Ipv6Addr>().is_ok()
}

/// Resolve a possibly-relative URL against a page's base URL. Returns None for
/// anything a reader cannot fetch — `blob:`/`data:`/`javascript:` are dropped
/// here rather than at the admission table: without a browser there is nothing
/// to resolve a `blob:` against.
pub fn absolute_url(base: &str, href: &str) -> Result<Option<String>, PyUrlError> {
    // Python tests `if not href` — before any strip — so an all-whitespace
    // href passes that gate, then strips to ''. urljoin's `if not url: return
    // base` hands the base back.
    if href.is_empty() {
        return Ok(None);
    }
    let href_trimmed = py_strip(href);
    // Also catches `//host/x`: its first character is `/`, so the
    // protocol-relative arm below only ever runs with a non-empty base.
    if base.is_empty() && href_trimmed.starts_with('/') {
        return Ok(None);
    }
    let effective_href = if href_trimmed.starts_with("//") {
        // Python: `urlsplit(page_url).scheme + ':' + href` — that urlsplit is
        // not wrapped in a try, so its ValueError propagates.
        format!("{}:{}", py_urlsplit(base, "", true)?.scheme, href_trimmed)
    } else {
        href_trimmed.to_string()
    };
    // Python wraps urljoin in `try/except ValueError: return None`.
    let resolved = match py_urljoin(base, &effective_href) {
        Ok(u) => u,
        Err(_) => return Ok(None),
    };
    // The final scheme check is not wrapped in Python either — it propagates.
    let split = py_urlsplit(&resolved, "", true)?;
    match split.scheme.as_str() {
        "http" | "https" => Ok(Some(resolved)),
        _ => Ok(None),
    }
}

/// True if the whole paste *is* an address, and not a sentence holding one. A
/// bare host (`example.com/watch/7`) is not accepted: feeding it to yt-dlp
/// would turn it into a search, and the result would be "no video found" when
/// the page actually has one.
///
/// `urlsplit` itself is *not* wrapped in a try in Python, so a malformed paste
/// (odd bracket count, bad IPv6) comes back as an Err, not a silent `false`.
pub fn looks_like_url(text: &str) -> Result<bool, PyUrlError> {
    let text = py_strip(text);
    // One token: any whitespace means this is a sentence about an address, not one.
    if text.is_empty() || py_token_count(text) != 1 {
        return Ok(false);
    }
    // `.lower()` on the scheme is a no-op: urlsplit already lowercased it.
    let split = py_urlsplit(text, "", true)?;
    Ok(matches!(split.scheme.as_str(), "http" | "https"))
}

/// Extract URLs from a share-text blob. Returns all URLs found, or an empty
/// vector if none. The caller decides whether to use the first one or report
/// the count.
pub fn extract_share_urls(text: &str) -> Vec<String> {
    static SHARE_URL: LazyLock<Regex> = LazyLock::new(|| {
        // The Python class char for char, `re.IGNORECASE` included: stop at
        // quotes, angle brackets and the full-width punctuation Chinese share
        // text wraps an address in. `(?i)` affects matching only — the match
        // keeps the case the page wrote, and simple case folding agrees with
        // Python on the dotted-I family: both fold U+017F into `s` and match
        // `httpſ://x` without rewriting it. `\x{1C}-\x{1F}` restores what
        // Rust's `\s` (Unicode White_Space, 25 code points) is missing from
        // Python's 29-code-point set.
        Regex::new(r#"(?i)https?://[^\s<>"'\x{1C}-\x{1F}\u{3000}，。、；：！？（）【】《》“”‘’]+"#).unwrap()
    });
    SHARE_URL
        .find_iter(text)
        .map(|m| {
            // The 9 ASCII characters Python's `rstrip('.,;:!?)\'"')` strips.
            m.as_str()
                .trim_end_matches(|c: char| {
                    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '\'' | '"')
                })
                .to_string()
        })
        .fold(Vec::new(), |mut found, url| {
            // Ordered dedup, same as Python's `if url not in found`: first one wins.
            if !found.contains(&url) {
                found.push(url);
            }
            found
        })
}

/// `(address, how many were found)` for whatever the user pasted.
///
/// The share blurb is the whole reason this exists: what a phone hands over is
/// a sentence — `7.12 复制打开抖音，看看【…】https://v.douyin.com/iAbc/ …` — and
/// asking people to cut the URL out of it is a UI failure we can fix here. The
/// first address is the one we parse; the count is what the page says back, so
/// a multi-part post is understood as "this part" rather than silently
/// dropping the rest. `("", 0)` means the paste carried no address at all —
/// the caller's refusal to phrase, in `looks_like_url`'s own words.
pub fn resolve_target(text: &str) -> Result<(String, usize), PyUrlError> {
    let text = py_strip(text);
    if text.is_empty() {
        return Ok((String::new(), 0));
    }
    if looks_like_url(text)? {
        return Ok((text.to_string(), 1));
    }
    let found = extract_share_urls(text);
    Ok(match found.first() {
        Some(url) => (url.clone(), found.len()),
        None => (String::new(), 0),
    })
}

/// The nine browsers yt-dlp recognizes for `--cookies-from-browser`, spelled
/// the way the engine spells them (`SUPPORTED_BROWSERS` in `yt_dlp/cookies.py`
/// is this exact set), in the oracle's order. The list lives next to the
/// function that turns it into an argv element: a whitelist one file away from
/// the argument it guards can drift from what that argument will accept.
pub const COOKIE_BROWSERS: &[&str] = &[
    "brave",
    "chrome",
    "chromium",
    "edge",
    "firefox",
    "opera",
    "safari",
    "vivaldi",
    "whale",
];

/// Validate a cookie browser spec. The grammar is
/// `BROWSER[+KEYRING][:PROFILE][::CONTAINER]`, where the profile can be a
/// Windows path (so `:` inside it is legal). Returns None if the spec is
/// malformed or uses a container/keyring we do not support.
pub fn cookie_browser_spec(spec: &str) -> Option<String> {
    const SPEC_FORBIDDEN: [char; 3] = ['\r', '\n', '\t'];
    let text = py_strip(spec);
    if text.is_empty() || text.chars().any(|c| SPEC_FORBIDDEN.contains(&c)) {
        return None;
    }
    // The engine accepts BROWSER[+KEYRING][:PROFILE][::CONTAINER]; we take
    // BROWSER[:PROFILE] only. A keyring lands in the name slot and dies on the
    // whitelist below; a container or a leading '-' dies on the profile checks.
    //
    // The first colon only: on Windows the profile *is* a path with a drive
    // letter in it, so splitting on every colon would cut the path.
    let (name, tail) = match text.find(':') {
        Some(idx) => (&text[..idx], &text[idx + 1..]),
        None => (text, ""),
    };
    // The name is case-insensitive; the profile is not (it is a path).
    let name = py_strip(name).to_lowercase();
    if !COOKIE_BROWSERS.contains(&name.as_str()) {
        return None;
    }
    let profile = py_strip(tail);
    if profile.is_empty() {
        return Some(name);
    }
    // A stored value must not smuggle an option into the second half of the
    // argument, and '::' opens a container we do not support.
    if profile.starts_with(':') || profile.starts_with('-') || profile.contains("::") {
        return None;
    }
    Some(format!("{}:{}", name, profile))
}

/// The cookie sources one engine run may use. An empty half means "not given":
/// Python reads the same pair out of a dict, where a missing key and an empty
/// string both answer `''`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cookies<'a> {
    pub file: &'a str,
    pub browser: &'a str,
}

/// `cookie_arguments(cookies)`: the argv slice that points the engine at a
/// cookie source. The pasted jar wins because it is what the user *gave* us,
/// while the browser name is only what we inferred; with neither, the engine
/// resolves anonymously and adds nothing to the command line.
pub fn cookie_arguments(cookies: &Cookies<'_>) -> Vec<String> {
    let path = py_strip(cookies.file);
    if !path.is_empty() {
        return vec!["--cookies".into(), path.to_string()];
    }
    match cookie_browser_spec(cookies.browser) {
        Some(spec) => vec!["--cookies-from-browser".into(), spec],
        None => Vec::new(),
    }
}

/// The whole command line `ytdlp_candidates` would hand to the operating
/// system. A seam, not a second copy: the argv is the part of that function a
/// test can pin without spawning an engine that is not on this machine.
pub fn ytdlp_command(binary: &str, page_url: &str, cookies: &Cookies<'_>) -> Vec<String> {
    let mut command: Vec<String> = vec![
        binary.into(),
        "-J".into(),
        "--no-playlist".into(),
        "--no-warnings".into(),
        "--quiet".into(),
        "--socket-timeout".into(),
        "20".into(),
    ];
    command.extend(cookie_arguments(cookies));
    command.push(page_url.into());
    command
}

/// `engine_note(completed)`: the sentence the card shows when the engine said
/// no. `--no-warnings` does **not** silence everything the engine writes to
/// stderr: `cookies.py` sends 「skipping cookie file entry due to invalid
/// length」 straight to `write_string`, which bypasses the logging machinery,
/// so that warning can be the first line while the real refusal is below it.
/// Reading the first line would call *our* junk jar a site that wants a
/// session. Hence: an `ERROR:` line wins, and only then the first line with any
/// content. Cut to 200 characters because a real stderr carries a traceback
/// after the sentence, and a traceback on a settings page is not an explanation.
pub fn engine_note(returncode: i32, stderr: Option<&str>) -> String {
    const LIMIT: usize = ENGINE_NOTE_LIMIT;
    let text = py_strip(stderr.unwrap_or("")).to_string();
    let mut first = String::new();
    for line in py_splitlines(&text) {
        let line = py_strip(line);
        if line.is_empty() {
            continue;
        }
        if first.is_empty() {
            first = line.to_string();
        }
        if line.starts_with("ERROR:") {
            return line.chars().take(LIMIT).collect();
        }
    }
    if !first.is_empty() {
        return first.chars().take(LIMIT).collect();
    }
    if returncode != 0 {
        return format!("yt-dlp 退出码 {}", returncode);
    }
    "yt-dlp 没有返回任何地址".to_string()
}

/// `engine_note`'s `limit` argument, kept as a constant so the corpus and the
/// page agree on one number.
pub const ENGINE_NOTE_LIMIT: usize = 200;

/// `_COOKIE_WORDS`: what an engine says when the page will not hand anything
/// over without a session. Every extractor phrases it differently, and a return
/// code never distinguishes them from "this site has no video", so this is a
/// guess at somebody else's sentence and lives where one owner can be pinned.
const COOKIE_WORDS: &[&str] = &[
    "cookie",
    "log in",
    "login",
    "logged in",
    "sign in",
    "authentication",
    "unauthorized",
    "需要登录",
    "授权",
];

/// `needs_cookies(note)`: whether an engine's own refusal is asking for a
/// session.
pub fn needs_cookies(note: &str) -> bool {
    let text = note.to_lowercase();
    COOKIE_WORDS.iter().any(|word| text.contains(word))
}

/// The four ways the engine can fail at **our** cookie source. This table
/// exists because `needs_cookies` cannot do its job without it: the word
/// `cookie` appears just as confidently in a site's 「cookies to access this
/// webpage」 as in the engine telling us it could not open the store we pointed
/// it at. So ask this first, and only call a sentence a login request when no
/// row here answered.
///
/// The rows are verbatim fragments from `yt_dlp/cookies.py`, lower-cased to
/// match, and the order is load-bearing — **first match wins**, because
/// `could not copy chrome cookie database` also contains a `not_found`
/// fragment.
pub const COOKIE_PROBLEMS: &[&str] = &["locked", "not_found", "decrypt", "bad_profile"];

const COOKIE_SOURCE_TABLE: &[(&str, &[&str])] = &[
    // The browser is running and holds its own database open (Windows: the
    // engine reaches this from a sharing violation).
    ("locked", &["could not copy chrome cookie database"]),
    // No file named `Cookies` under the root it looked at. Three spellings of
    // this sentence, all ERROR level so all reachable: the Chrome family quotes
    // the root it searched, Firefox prints it bare, and Safari prints neither a
    // root nor an `in` at all. The singular form is here because it also sits
    // inside `locked`'s sentence above — which is what makes that row have to
    // be checked first.
    ("not_found", &["cookies database", "cookie database"]),
    // The store was found and opened, but the key that decrypts it was not
    // reachable: DPAPI, the login keyring, kwallet, or Chrome's `Local State`.
    (
        "decrypt",
        &[
            "failed to decrypt with dpapi",
            "failed to read from keyring",
            "kwallet-query",
            "could not find local state file",
            "no encrypted key in local state",
        ],
    ),
    // Opera keeps one profile and does not take this argument at all: it prints
    // the refusal at ERROR level — so that sentence is the line `engine_note`
    // brings back — and then goes on to look for a database under a path it
    // built out of the profile name. Saying "that browser has no profiles"
    // beats reporting the not-found sentence that follows it.
    ("bad_profile", &["does not support profiles"]),
];

/// `cookie_source_problem(note)`: which of our cookie sources the engine's
/// sentence says failed. `None` where Python returns the empty string.
pub fn cookie_source_problem(note: &str) -> Option<&'static str> {
    let text = note.to_lowercase();
    for (problem, words) in COOKIE_SOURCE_TABLE {
        if words.iter().any(|word| text.contains(word)) {
            return Some(problem);
        }
    }
    None
}

/// `_UNSUPPORTED_WORDS`: what an engine says when the page is not from a site
/// it knows how to read at all. Deliberately *not* including `unable to
/// extract`, which means an extractor it does have ran and came back empty:
/// that is a real answer about this page, and re-labelling it as our limitation
/// would send the user hunting for a different site when the honest sentence is
/// the engine's own.
const UNSUPPORTED_WORDS: &[&str] = &["unsupported url", "no supported", "not supported"];

/// `engine_says_unsupported(note)`.
pub fn engine_says_unsupported(note: &str) -> bool {
    let text = note.to_lowercase();
    UNSUPPORTED_WORDS.iter().any(|word| text.contains(word))
}

/// `JS_HOLE`: the hole this feature admits, written as the sentence a user
/// reads at the moment it costs them something. The Python module keeps one
/// copy that the card, the help, and the empty-result sentence all read, so a
/// limit never lives only in a design document.
pub const JS_HOLE: &str = "需要 JavaScript 才能算出地址的站点本机解不出来：\
                            地址不在 HTML 里，要页面自己的脚本再算一次";

/// `COOKIE_JAR_MAX_BYTES`: a jar is written into the config directory and then
/// handed to a subprocess, so the cap is not politeness — a 200 MB paste is a
/// disk problem. Compared against the UTF-8 byte length, like Python's
/// `len(text.encode('utf-8'))`.
pub const COOKIE_JAR_MAX_BYTES: usize = 256 * 1024;
/// `COOKIE_JAR_FIELDS`: the Netscape format puts seven tab-separated fields on
/// a cookie line.
pub const COOKIE_JAR_FIELDS: usize = 7;
/// `HTTPOONLY_PREFIX`: an HttpOnly cookie is a *cookie* whose line starts with
/// this, not a comment. A jar exported from a browser marks its session cookies
/// this way, so a reader that skips every `#` line reads a pile of real login
/// cookies as "nothing here".
pub const HTTPOONLY_PREFIX: &str = "#HttpOnly_";

/// `cookie_jar_state(text)`: `(cookies, bad_lines)` for a pasted Netscape jar —
/// it reads, it does not run. An empty or malformed jar produces *no* error at
/// all from the engine (it simply resolves the page anonymously and comes back
/// with fewer addresses), so saying "3 条 cookie，2 行看不懂" at paste time is
/// the only moment anyone can act on it. The `Err` half is Python's `ValueError`.
pub fn cookie_jar_state(text: &str) -> Result<(usize, usize), String> {
    if text.len() > COOKIE_JAR_MAX_BYTES {
        return Err(format!(
            "cookie 文件太大了（上限 {} KB）",
            COOKIE_JAR_MAX_BYTES / 1024
        ));
    }
    let mut cookies = 0usize;
    let mut bad = 0usize;
    for raw in py_splitlines(text) {
        // The prefix binds at index 0 of the *raw* line, so it is stripped
        // before the comment test; then the engine's own skip applies to what
        // is left. An indented `  #HttpOnly_…` stays a comment, and so does
        // `#HttpOnly_# …`. Counting either as a cookie would promise a login
        // state the engine never loads.
        let stripped = raw.strip_prefix(HTTPOONLY_PREFIX).unwrap_or(raw);
        if py_strip(stripped).is_empty() || stripped.trim_start_matches(py_is_space).starts_with('#')
        {
            continue;
        }
        // `rstrip('\r\n')` never bites: `py_splitlines` leaves neither on a row.
        let fields: Vec<&str> = stripped.trim_end_matches(['\r', '\n']).split('\t').collect();
        if fields.len() >= COOKIE_JAR_FIELDS && !py_strip(fields[0]).is_empty() {
            cookies += 1;
        } else {
            bad += 1;
        }
    }
    Ok((cookies, bad))
}

/// The characters CPython's `repr` would escape: everything in category Cc,
/// every separator (Zs minus the ASCII space, Zl, Zp), and the format characters
/// (Cf) it counts as invisible. Rust's `is_control` is exactly Cc and
/// `is_whitespace` covers the separators plus the control codes `repr` already
/// names, so the hand-built list below is the Cf set.
///
/// **Documented divergence**: Cc/Cf/Zs/Zl/Zp are enumerated here rather than
/// looked up in a general-category table, so Co and Cn code points — private-use
/// and unassigned, which CPython also escapes — print verbatim in this port. The
/// corpus has no row in those categories, and adding a unicode-category
/// dependency to a crate that does not otherwise need one is not worth a code
/// point no argv can contain.
fn py_is_nonprintable(c: char) -> bool {
    if c == ' ' {
        return false;
    }
    if c.is_control() || c.is_whitespace() {
        return true;
    }
    let cp = c as u32;
    matches!(cp,
        0x00ad
            | 0x0600..=0x0605
            | 0x061c
            | 0x06dd
            | 0x070f
            | 0x08e2
            | 0x110bd
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x16fe3
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f)
}

/// `repr()` of a Python `str`, which is what `str(list)` puts between the
/// brackets of an argv — and `subprocess` builds its timeout sentence out of
/// exactly that. Quote choice: `'` unless the text has a `'` and no `"`, in
/// which case `"` (so a shell-ish argument keeps its apostrophe unescaped).
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other == quote => {
                out.push('\\');
                out.push(other);
            }
            other if py_is_nonprintable(other) => match other as u32 {
                cp if cp < 0x100 => out.push_str(&format!("\\x{:02x}", cp)),
                cp if cp < 0x10000 => out.push_str(&format!("\\u{:04x}", cp)),
                cp => out.push_str(&format!("\\U{:08x}", cp)),
            },
            other => out.push(other),
        }
    }
    out.push(quote);
    out
}

/// `str(list)` of a list of strings: each element's `repr`, comma and space
/// apart, inside square brackets.
fn py_repr_str_list(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|s| py_repr_str(s))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{}]", inner)
}

/// How `str()` would print the `timeout` that `subprocess.run` was handed.
/// There is no whole-seconds branch: Rust's `Display` for `f64` already prints
/// a whole number without a `.0`, and deleting that branch redd nothing across
/// the 41 recorded sentences. **Documented divergence**: Python prints `120.0`
/// for a *float* 120 and switches to exponent notation below 1e-4 (`1e-07`),
/// where Rust prints `120` and `0.0000001`. The port only ever passes whole
/// seconds (`YTDLP_TIMEOUT` is the integer 120), so no product path reaches
/// either gap; both shapes stay in the corpus as rows that must *not* match.
fn timeout_seconds_text(timeout: Duration) -> String {
    format!("{}", timeout.as_secs_f64())
}

/// `str(subprocess.TimeoutExpired(cmd, timeout))`, measured on the pinned
/// interpreter: `Command '<list repr>' timed out after <timeout> seconds`.
/// The `Command '…'` wrapper is part of the sentence, so the card that shows
/// 「yt-dlp 没能运行（TimeoutExpired: …）」 names the argv that timed out.
fn timeout_sentence(argv: &[String], timeout: Duration) -> String {
    format!(
        "Command '{}' timed out after {} seconds",
        py_repr_str_list(argv),
        timeout_seconds_text(timeout)
    )
}

/// The `type(exc).__name__` half of the Python message for an `OSError` that
/// comes out of `Popen`. Only the name is ported faithfully: the message half
/// is Rust's `io::Error` `Display`, which for the same failure reads
/// `No such file or directory (os error 2)` where CPython writes
/// `[Errno 2] No such file or directory: '/nope'`. Same shape as `probe_url_with`,
/// and no corpus row can pin it without spawning a binary that is not here.
fn python_error_name(err: &std::io::Error) -> &'static str {
    match err.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    }
}

/// `completed.returncode`: Python reports a signal-killed child as `-N`, which
/// a bare `ExitStatus::code()` would drop. On Windows there is no signal half.
#[cfg(unix)]
fn returncode_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(c) => c,
        None => -status.signal().unwrap_or(0),
    }
}

#[cfg(not(unix))]
fn returncode_of(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// `_sink(sink, note)`: a note only lands when somebody is collecting **and**
/// there is something to say. An empty sentence on the card is worse than none.
fn sink_note(sink: &mut Option<&mut Vec<String>>, note: &str) {
    if note.is_empty() {
        return;
    }
    if let Some(rows) = sink {
        rows.push(note.to_string());
    }
}

/// `ytdlp_candidates(page_url, binary=None, cookies=None, sink=None)`: run the
/// engine and read its JSON. Empty on every failure — the caller's answer to
/// "no addresses" is the four-way sentence in `ResolveJob`, not an exception —
/// and the reason, if anybody is listening, goes into `sink`.
pub fn ytdlp_candidates(
    page_url: &str,
    binary: Option<&str>,
    cookies: &Cookies<'_>,
    sink: Option<&mut Vec<String>>,
) -> Vec<Candidate> {
    ytdlp_candidates_with(page_url, binary, cookies, sink, Duration::from_secs(YTDLP_TIMEOUT_SECS))
}

/// The same with the timeout as a parameter, so a case that wants to watch the
/// deadline fire does not wait two minutes for it.
pub fn ytdlp_candidates_with(
    page_url: &str,
    binary: Option<&str>,
    cookies: &Cookies<'_>,
    mut sink: Option<&mut Vec<String>>,
    timeout: Duration,
) -> Vec<Candidate> {
    let bin = binary
        .filter(|b| !b.is_empty())
        .map(|b| b.to_string())
        .or_else(|| find_command("yt-dlp", None, None, None));
    let bin = match bin {
        // `if binary is None: return []` — no note, because a machine without
        // the engine is a normal state, not a failure of this resolve.
        None => return Vec::new(),
        Some(b) => b,
    };
    let argv = ytdlp_command(&bin, page_url, cookies);

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(console_flags(None));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            sink_note(
                &mut sink,
                &format!("yt-dlp 没能运行（{}: {}）", python_error_name(&e), e),
            );
            return Vec::new();
        }
    };

    let out_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let err_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sh = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ob = Arc::clone(&out_bytes);
    let eb = Arc::clone(&err_bytes);
    let out_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = sh.read_to_end(&mut buf);
        *ob.lock().unwrap() = buf;
    });
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = se.read_to_end(&mut buf);
        *eb.lock().unwrap() = buf;
    });

    let deadline = Instant::now() + timeout;
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let status = match status {
        Some(s) => s,
        None => {
            // TimeoutExpired is a SubprocessError, so it takes the same sentence
            // as a failed spawn — with the engine's own text for the argv.
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_thread.join();
            let _ = err_thread.join();
            sink_note(
                &mut sink,
                &format!(
                    "yt-dlp 没能运行（TimeoutExpired: {}）",
                    timeout_sentence(&argv, timeout)
                ),
            );
            return Vec::new();
        }
    };
    let _ = out_thread.join();
    let _ = err_thread.join();
    let out_bytes = std::mem::take(&mut *out_bytes.lock().unwrap());
    let err_bytes = std::mem::take(&mut *err_bytes.lock().unwrap());
    // Python read the streams with encoding='utf-8', errors='replace'.
    let stdout = String::from_utf8_lossy(&out_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&err_bytes).into_owned();
    let returncode = returncode_of(&status);
    if returncode != 0 || py_strip(&stdout).is_empty() {
        sink_note(&mut sink, &engine_note(returncode, Some(&stderr)));
        return Vec::new();
    }
    parse_ytdlp_json(&stdout)
}

/// Python `title_for_uri`: what to call a stream when nobody has told us its
/// name.
///
/// The container suffix is stripped because `movie.mp4` is the file that was
/// chosen while `movie` is what was meant; what remains is the address itself —
/// not a title, but a title that names a *different* video is worse than one
/// that says nothing. `urlsplit` is called unguarded in the oracle and raises on
/// malformed input (`http://[::1`), so the failure is a `Result`, not a panic.
pub fn title_for_uri(uri: &str, fallback: &str) -> Result<String, PyUrlError> {
    let split = py_urlsplit(uri, "", true)?;
    // The basename runs on the raw path, *before* percent-decoding: `a%2Fb.mp4`
    // has no separator in it yet, so the whole segment is one name.
    let stem = py_basename(&split.path);
    let stem = py_unquote(stem);
    let mut stem = py_strip(&stem);
    for suffix in MEDIA_SUFFIXES.iter() {
        if stem.to_lowercase().ends_with(*suffix) {
            // Python's `stem[:-len(suffix)]` drops *characters*. KELVIN
            // (U+212A) lowercases to the single ASCII 'k', so `.mKv` matches
            // `.mkv` from four characters but five bytes — a byte-count cut
            // would leave a stray `.m`, which for a bare `.mKv` is even the
            // difference between falling back and naming the stream `.m`.
            let drop = suffix.chars().count();
            let cut = stem
                .char_indices()
                .rev()
                .nth(drop - 1)
                .map_or(0, |(i, _)| i);
            stem = py_strip(&stem[..cut]);
            break;
        }
    }
    if !stem.is_empty() {
        return Ok(stem.chars().take(TITLE_MAX_CHARS).collect());
    }
    Ok(if fallback.is_empty() {
        uri.to_string()
    } else {
        fallback.to_string()
    })
}

/// What to call this on the television.
///
/// `label` is provenance — `video:src`, or yt-dlp's `mp4 720p` — not a title,
/// and handing it to `SetAVTransportURI` is how a DLNA renderer ends up printing
/// "mp4" where the user expected the name of a film. So: the page's own title,
/// then the file's own name without its container suffix, then the address we
/// were pointed at. Nothing here invents a title.
pub fn display_title(candidate: &Candidate, fallback: &str) -> Result<String, PyUrlError> {
    let title = py_strip(candidate.title.as_deref().unwrap_or(""));
    if !title.is_empty() {
        // Python slices by characters; a byte cut would split a multibyte
        // character sitting exactly on the 120-char boundary.
        return Ok(title.chars().take(TITLE_MAX_CHARS).collect());
    }
    title_for_uri(&candidate.url, fallback)
}

/// The one word to put on the card for "what container is this".
///
/// `probe.container` is ffprobe's honest answer, which for an MP4 is the whole
/// family list — `mov,mp4,m4a,3gp,3g2,mj2` — correct in a machine and noise in
/// a badge. So the address's own suffix comes first, then the first name
/// ffprobe offered. The raw list stays on the `Probe`, because the admission
/// table asks "is mp4 anywhere in it", not "is it the first one".
pub fn container_label(candidate: &Candidate) -> String {
    let suffix = candidate.path_suffix();
    if !suffix.is_empty() {
        return suffix.trim_start_matches('.').to_uppercase();
    }
    // `probe=None` and `container=''` both answer the empty string in the
    // oracle, so one `unwrap_or` covers both.
    let listed = candidate.probe.container.as_deref().unwrap_or("");
    // split(',')[0] always exists; the strip runs on the *first* element, so
    // `' matroska,webm '` keeps the space after the comma out of the badge.
    py_strip(listed.split(',').next().unwrap_or("")).to_uppercase()
}

/// Why this address is served from Macast rather than handed to the device.
///
/// The rule castor states in `compose`, and that reading the headers yt-dlp
/// returns confirms: one request header disqualifies the hand-off. The device
/// will fetch the URL minutes from now, from its own IP, with no Referer at all.
pub fn relay_reason(candidate: &Candidate) -> String {
    if candidate
        .audio_url
        .as_deref()
        .map_or(false, |u| !u.is_empty())
    {
        return "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出".to_string();
    }
    if candidate.segmented() {
        return "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流".to_string();
    }
    if !candidate.headers.is_empty() {
        // `sorted(dict)` in the oracle: code-point order. Rust's `&str` Ord is
        // bytewise, which coincides with code-point order for UTF-8.
        let mut keys: Vec<&str> = candidate.headers.keys().map(|k| k.as_str()).collect();
        keys.sort();
        return format!(
            "源站要求请求头（{}）：电视不会带上这些，由 Macast 代取",
            keys.join("、")
        );
    }
    "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403".to_string()
}

/// `"proxy"`, `"remux"`, or `"merge"` — which of the three shapes serves this one.
///
/// `merge` is a remux with a second input: the picture and the sound arrive as
/// two addresses and ffmpeg copies both into one growing MP4. It therefore
/// inherits every property the relay gives a remuxed file, which is why the
/// relay keeps one serving shape and two ways of filling it.
pub fn plan(candidate: &Candidate) -> &'static str {
    if candidate
        .audio_url
        .as_deref()
        .map_or(false, |u| !u.is_empty())
    {
        return "merge";
    }
    if candidate.segmented() {
        "remux"
    } else {
        "proxy"
    }
}

// ---------------------------------------------------------------------------
// Fifth sublayer: ffprobe probing, yt-dlp JSON parsing, admission and ranking.
// Every predicate here is the Python body in `macast/media_resolve.py`
// (:820–1247) read byte for byte; the differential corpus `/tmp/mr5_corpus.json`
// is the witness. Where the two languages genuinely differ (float repr,
// Unicode digit classes) the divergence is noted at the site.
// ---------------------------------------------------------------------------

/// Shared falsy `Value` so helpers returning `&Value` never make a copy.
static JSON_NULL: LazyLock<Value> = LazyLock::new(|| serde_json::Value::Null);

/// Field of a JSON object, or the shared `Null` when this is not an object
/// or the key is absent — `info.get(key)` for any dict-shaped value.
fn obj_get<'a>(v: &'a Value, key: &str) -> &'a Value {
    match v {
        Value::Object(map) => map.get(key).unwrap_or(&*JSON_NULL),
        _ => &*JSON_NULL,
    }
}

/// Python truthiness over a JSON value: `Null`/`false`/`0`/`0.0`/`""`/`[]`/`{}`
/// are falsy, everything else is truthy. A string counts by length, never by
/// `strip()` — `'  '` is truthy in Python and here.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `str(v)` for the shapes a JSON tree can carry. Numbers keep their
/// compact serde rendering; bools become `'True'`/`'False'` like `str()` does.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Python `int(v)` for the shapes a JSON tree can carry. A non-numeric string
/// becomes 0 (`int()` would raise; no corpus row reaches that, so the
/// divergence is only documented).
fn py_int(v: &Value) -> i64 {
    match v {
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::String(s) => py_strip(s).parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// Python `float(s)` for a string: leading/trailing whitespace is stripped
/// first (Python's own rule), underscores count only when flanked by ASCII
/// digits on both sides (CPython 3.12), and anything else that does not parse
/// is a `ValueError` the caller turns into 0.0. `inf`/`infinity`/`nan` are
/// case-insensitive in both languages (`f64::from_str` accepts "Infinity" and
/// "NaN").
fn py_float(s: &str) -> Option<f64> {
    let t = py_strip(s);
    let chars: Vec<char> = t.chars().collect();
    for (k, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = k > 0 && chars[k - 1].is_ascii_digit();
            let after = k + 1 < chars.len() && chars[k + 1].is_ascii_digit();
            if !(before && after) {
                return None;
            }
        }
    }
    let cleaned: String = chars.into_iter().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// `_number(value)`: `float(value)` except `(TypeError, ValueError) → 0.0`.
/// A bool becomes 1.0/0.0 exactly as Python's `float(True)` does.
fn number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as u8 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => py_float(s).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Python's 0.0-or-falsy collapse for a probe float: `duration` and `bitrate`
/// are `float` fields, and only a *falsy* one renders as `''`-equivalent None.
/// `-0.0` is falsy in Python (`not -0.0` is True) but keeps its sign for the
/// byte-exact `f:%016x` render, so this returns `Some(-0.0)` and callers that
/// need the Python *decision* compare the raw value with `== 0.0` instead.
fn number_collapse(v: f64) -> Option<f64> {
    if v == 0.0 && !v.is_sign_negative() {
        None
    } else {
        Some(v)
    }
}

/// `int(_number(v))` with the two shapes the corpus never reaches (NaN,
/// infinity, negatives) mapped onto `None` rather than a panic. Python would
/// raise or store the value; a probe that reports them is not in front of us.
fn collapse_int(v: f64) -> Option<u32> {
    if v > 0.0 && v <= u32::MAX as f64 {
        Some(v.trunc() as u32)
    } else {
        None
    }
}

/// Python `str.splitlines()` boundaries — the Unicode set, including `\v`,
/// `\f`, `\x1c`–`\x1e`, NEL and the two paragraph separators, but NOT `\x1f`.
/// A trailing boundary yields no empty final line, and `\r\n` is one boundary.
fn is_line_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
            | '\u{2028}' | '\u{2029}'
    )
}

fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if is_line_boundary(c) {
            let eat_lf = c == '\r' && iter.peek().map(|&(j, d)| d == '\n' && j == i + 1) == Some(true);
            out.push(&s[start..i]);
            if eat_lf {
                if let Some((_, '\n')) = iter.next() {
                    start = i + 2;
                }
            } else {
                start = i + c.len_utf8();
            }
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `first_line(text)`: the first line with content after `strip()`, itself
/// `strip()`ed and cut to 200 *characters* (Python slicing is by code point).
pub fn first_line(text: Option<&str>) -> String {
    for line in py_splitlines(text.unwrap_or("")) {
        let t = py_strip(line);
        if !t.is_empty() {
            return t.chars().take(200).collect();
        }
    }
    String::new()
}

/// `normalize_headers(raw)`: lower-case the names, keep Python's `str(value)`
/// for non-string values, and drop the headers that must not be forwarded.
/// A non-object answers `{}` like the `isinstance` guard in Python does.
pub fn normalize_headers(raw: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Value::Object(map) = raw {
        for (k, v) in map {
            if v.is_null() {
                continue;
            }
            let name = k.to_lowercase();
            if UNFORWARDABLE_HEADERS.contains(name.as_str()) {
                continue;
            }
            out.insert(name, py_str(v));
        }
    }
    out
}

/// `header_field(headers)`: ffprobe wants one `-headers` blob with CRLF between
/// the lines, and the lines must be in sorted-name order. `None` behaves like
/// Python's `headers or {}`. UTF-8 byte order coincides with Python's
/// code-point order, so an ordinary `sort` is the same answer (same precedent
/// as `relay_reason`).
pub fn header_field(headers: Option<&HashMap<String, String>>) -> String {
    let mut names: Vec<&String> = match headers {
        Some(m) => m.keys().collect(),
        None => Vec::new(),
    };
    names.sort();
    names
        .iter()
        .map(|n| format!("{}: {}", n, headers.unwrap().get(*n).unwrap()))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// `parse_probe_json(text)`: ffprobe's JSON answer turned into a `Probe`.
/// `None` when it is not JSON or not an object — the caller says
/// "unreadable probe output". Every `or ''` / truthiness fold below mirrors
/// the Python line-by-line, including the two decisions taken on the RAW
/// floats: the bitrate fallback (`if not probe.bitrate`) and `is_live`
/// (`not probe.duration`, which is True for `-0.0`).
pub fn parse_probe_json(text: &str) -> Option<Probe> {
    let info: Value = serde_json::from_str(text).ok()?;
    if !info.is_object() {
        return None;
    }
    let fmt_value = obj_get(&info, "format");
    let fmt: &Value = if truthy(fmt_value) { fmt_value } else { &*JSON_NULL };
    let streams: Vec<&Value> = match obj_get(&info, "streams") {
        Value::Array(items) => items.iter().filter(|s| s.is_object()).collect(),
        _ => Vec::new(),
    };
    let video = streams
        .iter()
        .copied()
        .find(|s| obj_get(s, "codec_type") == &Value::String("video".to_string()))
        .filter(|s| truthy(s));
    let audio = streams
        .iter()
        .copied()
        .find(|s| obj_get(s, "codec_type") == &Value::String("audio".to_string()))
        .filter(|s| truthy(s));

    let container = match truthy(obj_get(fmt, "format_name")) {
        true => py_str(obj_get(fmt, "format_name")),
        false => String::new(),
    };
    let duration_raw = number(obj_get(fmt, "duration"));
    let mut bitrate_raw = number(obj_get(fmt, "bit_rate"));

    let mut probe = Probe {
        container: (!container.is_empty()).then_some(container),
        duration: number_collapse(duration_raw),
        bitrate: number_collapse(bitrate_raw),
        ..Probe::new()
    };
    if let Some(v) = video {
        let codec = match truthy(obj_get(v, "codec_name")) {
            true => py_str(obj_get(v, "codec_name")),
            false => String::new(),
        };
        probe.video_codec = (!codec.is_empty()).then_some(codec);
        probe.video_width = collapse_int(number(obj_get(v, "width")));
        probe.video_height = collapse_int(number(obj_get(v, "height")));
    }
    if let Some(a) = audio {
        let codec = match truthy(obj_get(a, "codec_name")) {
            true => py_str(obj_get(a, "codec_name")),
            false => String::new(),
        };
        probe.audio_codec = (!codec.is_empty()).then_some(codec);
    }
    // `if not probe.bitrate and video:` — falsiness of the raw float, so -0.0
    // takes the fallback branch exactly as Python does.
    if bitrate_raw == 0.0 && video.is_some() {
        bitrate_raw = number(obj_get(video.unwrap(), "bit_rate"));
        probe.bitrate = number_collapse(bitrate_raw);
    }
    // `probe.is_live = not probe.duration` — again the raw float.
    probe.is_live = duration_raw == 0.0;
    Some(probe)
}

/// `probe_url(url, headers, binary=None)` with the timeout as a parameter, so
/// a corpus case that patches `PROBE_TIMEOUT` can drive the same code. The
/// Python body runs `subprocess.run(..., timeout=PROBE_TIMEOUT)`; here the
/// deadline is polled by hand because a Rust kill must reap the child first,
/// and the two output pipes are drained by reader threads so a chatty ffprobe
/// cannot fill the pipe buffer and block before its own deadline.
pub fn probe_url_with(
    url: &str,
    headers: Option<&HashMap<String, String>>,
    binary: Option<&str>,
    timeout: Duration,
) -> Probe {
    let bin = binary
        .filter(|b| !b.is_empty())
        .map(|b| b.to_string())
        .or_else(|| find_command("ffprobe", None, None, None));
    let bin = match bin {
        Some(b) => b,
        None => {
            return Probe {
                error: Some("no ffprobe on this machine".to_string()),
                ..Probe::new()
            }
        }
    };
    let mut argv: Vec<String> = vec![
        bin,
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-print_format".into(),
        "json".into(),
        "-show_format".into(),
        "-show_streams".into(),
        "-analyzeduration".into(),
        "4000000".into(),
        "-probesize".into(),
        "4000000".into(),
        "-protocol_whitelist".into(),
        "file,http,https,tcp,tls,crypto,httpproxy,data".into(),
    ];
    // `if headers:` — an empty mapping adds nothing, like the Python body.
    let has_headers = headers.map_or(false, |m| !m.is_empty());
    if has_headers {
        argv.push("-headers".into());
        argv.push(header_field(headers));
    }
    argv.push(url.into());

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(console_flags(None));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Python: `Probe(reach=UNPROVEN, error=str(exc))` — an OSError's
            // str(). Rust's io::Error Display is close but not identical; no
            // corpus row spawns a missing binary by path, so this is a
            // documented divergence.
            return Probe {
                error: Some(e.to_string()),
                ..Probe::new()
            };
        }
    };
    let out_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let err_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut sh = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ob = Arc::clone(&out_bytes);
    let eb = Arc::clone(&err_bytes);
    let out_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = sh.read_to_end(&mut buf);
        *ob.lock().unwrap() = buf;
    });
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = se.read_to_end(&mut buf);
        *eb.lock().unwrap() = buf;
    });

    let deadline = Instant::now() + timeout;
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let status = match status {
        Some(s) => s,
        None => {
            // TimeoutExpired: kill, reap, and let the readers drain to EOF.
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Probe {
                error: Some("probe timed out".to_string()),
                ..Probe::new()
            };
        }
    };
    let _ = out_thread.join();
    let _ = err_thread.join();
    let out_bytes = std::mem::take(&mut *out_bytes.lock().unwrap());
    let err_bytes = std::mem::take(&mut *err_bytes.lock().unwrap());
    // Python read the streams with encoding='utf-8', errors='replace';
    // from_utf8_lossy produces the same U+FFFD substitutions.
    let stdout = String::from_utf8_lossy(&out_bytes).into_owned();
    let stderr_raw = String::from_utf8_lossy(&err_bytes).into_owned();
    let stderr = py_strip(&stderr_raw).to_string();
    // `returncode != 0` — a signal-killed child has no code and is a failure.
    if status.code().map_or(true, |c| c != 0) {
        let reach = if REFUSED_STATUS.is_match(&stderr) {
            REACH_REFUSED
        } else {
            REACH_UNPROVEN
        };
        return Probe {
            reach: reach.to_string(),
            error: Some(first_line(Some(&stderr))),
            ..Probe::new()
        };
    }
    match parse_probe_json(&stdout) {
        None => Probe {
            error: Some("unreadable probe output".to_string()),
            ..Probe::new()
        },
        Some(mut probe) => {
            probe.reach = REACH_OPENED.to_string();
            probe
        }
    }
}

/// The body's default timeout: `PROBE_TIMEOUT` as the module defines it.
pub fn probe_url(
    url: &str,
    headers: Option<&HashMap<String, String>>,
    binary: Option<&str>,
) -> Probe {
    probe_url_with(url, headers, binary, Duration::from_secs(PROBE_TIMEOUT_SECS))
}

/// `measure(candidate, binary=None)`: probe in place and hand the probe back.
pub fn measure(candidate: &mut Candidate, binary: Option<&str>) -> Probe {
    let probe = probe_url(&candidate.url.clone(), Some(&candidate.headers.clone()), binary);
    candidate.probe = probe.clone();
    probe
}

/// `track_kind(form)`: which half of a DASH pair this format carries —
/// `'video'`, `'audio'`, or `''` when the answer is neither or unreadable.
pub fn track_kind(form: &Value) -> &'static str {
    if !form.is_object() {
        return "";
    }
    let video = obj_get(form, "vcodec");
    let audio = obj_get(form, "acodec");
    let (vs, as_) = match (video.as_str(), audio.as_str()) {
        (Some(v), Some(a)) => (v, a),
        _ => return "",
    };
    let has_video = vs != "none";
    let has_audio = as_ != "none";
    if has_video && !has_audio {
        "video"
    } else {
        if has_audio && !has_video {
            return "audio";
        }
        ""
    }
}

/// `engine_audio_codec(form)`: the audio codec the engine names, normalised
/// the way the engine's own table is read — stripped, lower-cased, and `'none'`
/// answered as "no audio" (`''`).
pub fn engine_audio_codec(form: &Value) -> String {
    if !form.is_object() {
        return String::new();
    }
    let audio = match obj_get(form, "acodec").as_str() {
        Some(a) => a,
        None => return String::new(),
    };
    let lower = py_strip(audio).to_lowercase();
    if lower == "none" {
        String::new()
    } else {
        lower
    }
}

/// `_track_bitrate(form)`: the first of abr/tbr/br that reports a positive
/// number. Python's `float(value)` with the coercion rules `_number` pins;
/// `ValueError`/`TypeError` continue to the next key, so a non-numeric value
/// reads as 0.0 and is not `> 0` — the same answer.
fn track_bitrate(form: &Value) -> f64 {
    for key in ["abr", "tbr", "br"] {
        let value = obj_get(form, key);
        if value.is_null() {
            continue;
        }
        let f = number(value);
        if f > 0.0 {
            return f;
        }
    }
    0.0
}

/// `pair_tracks(videos, audios)`: one ledger, many copies — the best audio
/// (bitrate first, url as the tie-break) is written into every video row.
/// Python's `max()` keeps the FIRST of equal maxima, so the scan replaces the
/// best only on a strictly greater bitrate, or an equal bitrate with a
/// strictly greater url.
fn pair_tracks(videos: &mut [(Candidate, Value)], audios: &[(Candidate, Value)]) {
    if audios.is_empty() {
        return;
    }
    let mut best_idx = 0usize;
    let mut best_bitrate = track_bitrate(&audios[0].1);
    let mut best_url = audios[0].0.url.clone();
    for (i, (cand, form)) in audios.iter().enumerate().skip(1) {
        let bt = track_bitrate(form);
        if bt > best_bitrate || (bt == best_bitrate && cand.url > best_url) {
            best_idx = i;
            best_bitrate = bt;
            best_url = cand.url.clone();
        }
    }
    let a = &audios[best_idx].0;
    for (cand, _) in videos.iter_mut() {
        cand.audio_url = Some(a.url.clone());
        cand.audio_headers = a.headers.clone();
        cand.audio_label = a.label.clone();
        cand.audio_codec = a.audio_codec.clone();
    }
}

/// `format_label(form)`: the human string on the card — ext, height, codec,
/// note, bitrate. `str()` (not `int()`) is what Python prints for ext/height/
/// note, so `{"height": "720.5"}` becomes "720.5p"; an empty codec token still
/// contributes an empty part and `int(tbr)` truncates toward zero.
pub fn format_label(form: &Value) -> String {
    if !form.is_object() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if truthy(obj_get(form, "ext")) {
        parts.push(py_str(obj_get(form, "ext")));
    }
    if truthy(obj_get(form, "height")) {
        parts.push(format!("{}p", py_str(obj_get(form, "height"))));
    }
    let vcodec = obj_get(form, "vcodec");
    let vs = vcodec.as_str();
    if let Some(v) = vs {
        if !v.is_empty() && v != "none" {
            let head = v.split('.').next().unwrap_or("");
            let base = head.split('-').next().unwrap_or("").to_lowercase();
            let label = match base.as_str() {
                "avc1" | "avc" | "h264" => "H.264".to_string(),
                "hev1" | "hvc1" | "hevc" => "H.265".to_string(),
                "av01" => "AV1".to_string(),
                "vp9" | "vp09" => "VP9".to_string(),
                _ => base.to_uppercase(),
            };
            parts.push(label);
        }
    }
    if truthy(obj_get(form, "format_note")) {
        parts.push(py_str(obj_get(form, "format_note")));
    }
    if truthy(obj_get(form, "tbr")) {
        parts.push(format!("{} k", py_int(obj_get(form, "tbr"))));
    }
    parts.join(" ")
}

/// `page_wording(title, page_title)`: the episode suffix bilibili puts on the
/// player title (" (12)") is stripped so the card reads the show's name, but
/// only when the two otherwise match exactly.
static PAGE_ENTRY_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ \(\d{1,2}\)$").unwrap());

pub fn page_wording(title: &str, page_title: &str) -> String {
    if title.is_empty() {
        return page_title.to_string();
    }
    if page_title.is_empty() || title == page_title {
        return title.to_string();
    }
    if title.starts_with(page_title) {
        let suffix = &title[page_title.len()..];
        if PAGE_ENTRY_SUFFIX_RE.is_match(suffix) {
            return page_title.to_string();
        }
    }
    title.to_string()
}

/// `dedupe(candidates)`: first sighting of a URL owns its slot; a ytdlp row
/// replacing a page row inherits the page wording decision for the title.
/// Python keeps insertion order of the dict, so the Vec holds the slots and
/// the map only points at them.
pub fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut slots: Vec<Candidate> = Vec::new();
    let mut by_url: HashMap<String, usize> = HashMap::new();
    for cand in candidates {
        match by_url.get(&cand.url) {
            None => {
                by_url.insert(cand.url.clone(), slots.len());
                slots.push(cand);
            }
            Some(&i) => {
                let current = &slots[i];
                if current.engine == "page" && cand.engine == "ytdlp" {
                    let title = page_wording(
                        cand.title.as_deref().unwrap_or(""),
                        current.title.as_deref().unwrap_or(""),
                    );
                    let mut replacement = cand;
                    replacement.title = if title.is_empty() {
                        None
                    } else {
                        Some(title)
                    };
                    slots[i] = replacement;
                }
            }
        }
    }
    slots
}

/// `len(entry['formats'])` without a panic: Python counts a string by code
/// points, a number is not a container so `len()` would raise — the corpus
/// only carries nulls, arrays, and objects here, and non-sizes answer 0.
fn formats_len(v: &Value) -> usize {
    if !truthy(v) {
        return 0;
    }
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        _ => 0,
    }
}

/// `parse_ytdlp_json(text)`: the engine's JSON turned into candidates. A
/// playlist sorts its entries by format count (descending, stable) and reads
/// the top three; every form becomes a `Candidate` and the DASH halves are
/// paired before the list is deduped. The sort is Python's
/// `list.sort(reverse=True)` on a stable key — Rust's stable sort with the
/// comparator reversed gives the same order.
pub fn parse_ytdlp_json(text: &str) -> Vec<Candidate> {
    let info: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if !info.is_object() {
        return Vec::new();
    }
    let mut entries: Vec<Value> = if obj_get(&info, "_type").as_str() == Some("playlist") {
        match obj_get(&info, "entries") {
            Value::Array(items) => items
                .iter()
                .filter(|e| e.is_object())
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    } else {
        vec![info.clone()]
    };
    entries.sort_by(|a, b| formats_len(obj_get(b, "formats")).cmp(&formats_len(obj_get(a, "formats"))));
    let mut found: Vec<Candidate> = Vec::new();
    for entry in entries.iter().take(3) {
        let shared = normalize_headers(obj_get(entry, "http_headers"));
        let entry_title = obj_get(entry, "title");
        let mut video_rows: Vec<(Candidate, Value)> = Vec::new();
        let mut audio_rows: Vec<(Candidate, Value)> = Vec::new();
        let formats = match obj_get(entry, "formats") {
            Value::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        for form in formats {
            if !form.is_object() {
                continue;
            }
            let url = match obj_get(&form, "url").as_str() {
                Some(u) => u,
                None => continue,
            };
            let lower = url.to_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                continue;
            }
            // The entry title wins. yt-dlp labels each format
            // "<video title> (<index>)", and that index is a row in its own
            // format list — handing "第三集 · 夏日回响 (2)" to the television
            // is a claim about the page that the page never made. Python:
            // `title = entry_title if isinstance(entry_title, str) and
            // entry_title.strip() else form.get('title')`, then
            // `title if isinstance(title, str) else ''` at the ctor.
            let title: Option<String> = if entry_title.is_string()
                && !py_strip(entry_title.as_str().unwrap()).is_empty()
            {
                Some(entry_title.as_str().unwrap().to_string())
            } else {
                match obj_get(&form, "title").as_str() {
                    Some(t) => Some(t.to_string()),
                    None => None,
                }
            };
            // `normalize_headers(form.get('http_headers') or shared)` — a
            // falsy value (absent, {}, [], "", 0) falls through to `shared`,
            // and normalize_headers answers {} for any non-dict shape, so
            // both arms land on the same call. `shared` is itself already a
            // normalized map, so Python's `dict(...)` pass-through is exactly
            // a clone here.
            let raw_form_headers = obj_get(&form, "http_headers");
            let headers = if truthy(raw_form_headers) {
                normalize_headers(raw_form_headers)
            } else {
                shared.clone()
            };
            let cand = Candidate {
                url: url.to_string(),
                headers,
                title,
                probe: Probe::new(),
                engine: "ytdlp".to_string(),
                label: format_label(&form),
                audio_url: None,
                audio_headers: HashMap::new(),
                audio_codec: engine_audio_codec(&form),
                audio_label: String::new(),
                last_resort: false,
                reason: String::new(),
            };
            match track_kind(&form) {
                "video" => video_rows.push((cand, form)),
                "audio" => audio_rows.push((cand, form)),
                _ => found.push(cand),
            }
        }
        pair_tracks(&mut video_rows, &audio_rows);
        for (c, _) in video_rows {
            found.push(c);
        }
        for (c, _) in audio_rows {
            found.push(c);
        }
    }
    dedupe(found)
}

pub const REASON_CASTABLE: &str = "carried a castable program";
pub const REASON_UNPROVEN: &str = "unmeasurable, offered as a last resort";
pub const REASON_REFUSED: &str = "refused by the origin";
pub const REASON_AUDIO_ONLY: &str = "carried sound but no picture, kept for an audio cast";
pub const REASON_NO_PROGRAM: &str = "carried no moving picture";
pub const REASON_HEADER_ONLY: &str = "an MP4 header with no timeline of its own, offered as a last resort";

/// The admission table, in order: first match decides. Python's row order is
/// `(reason, when, keep, last_resort)`; the predicate is stored last here
/// because a `fn` pointer cannot sit first in a const tuple with the same
/// readability. Rows (2) UNPROVEN and (6) CASTABLE are total — the table is
/// exhaustive, so `admit` never falls through.
pub const ADMISSION_ROWS: [(&'static str, bool, bool, fn(&Candidate) -> bool); 6] = [
    (REASON_REFUSED, false, false, |c: &Candidate| {
        c.probe.reach == REACH_REFUSED
    }),
    (REASON_UNPROVEN, true, true, |c: &Candidate| !c.probe.measured()),
    (REASON_AUDIO_ONLY, true, false, |c: &Candidate| {
        !c.probe.moving_picture() && c.probe.audio_codec.is_some()
    }),
    (REASON_NO_PROGRAM, false, false, |c: &Candidate| !c.probe.moving_picture()),
    (REASON_HEADER_ONLY, true, true, |c: &Candidate| {
        !c.segmented()
            && c.duration() == 0.0
            && c.probe
                .container
                .as_deref()
                .unwrap_or("")
                .contains("mp4")
    }),
    (REASON_CASTABLE, true, false, |_c: &Candidate| true),
];

/// `admit(candidate)`: the first-match judgment — keep it, mark it last-resort,
/// and say why.
pub fn admit(candidate: &Candidate) -> (bool, bool, &'static str) {
    for (reason, keep, last, pred) in ADMISSION_ROWS.iter() {
        if pred(candidate) {
            return (*keep, *last, reason);
        }
    }
    (true, false, REASON_CASTABLE)
}

/// `exceeds_cap(candidate, ceiling)`: a last-resort or segmented row is never
/// capped (a ceiling is for progressive video; the other two shapes have no
/// height answer worth comparing), and a ceiling of 0 disables the rule.
pub fn exceeds_cap(candidate: &Candidate, ceiling: f64) -> bool {
    if ceiling <= 0.0 || candidate.last_resort || candidate.segmented() {
        return false;
    }
    candidate.height() as f64 > ceiling
}

/// `preference(a, b, ceiling)`: one comparison, Python's tuple-cascade shape.
/// A nonzero from the first unequal term decides; equal terms fall through to
/// the url as the final tie-break so the order is total.
pub fn preference(a: &Candidate, b: &Candidate, ceiling: f64) -> i32 {
    if a.last_resort != b.last_resort {
        return if a.last_resort { 1 } else { -1 };
    }
    let ae = exceeds_cap(a, ceiling);
    let be = exceeds_cap(b, ceiling);
    if ae != be {
        return if ae { 1 } else { -1 };
    }
    if a.segmented() != b.segmented() {
        return if !a.segmented() { -1 } else { 1 };
    }
    let ah = a.height() > 0;
    let bh = b.height() > 0;
    if ah != bh {
        return if bh { 1 } else { -1 };
    }
    if a.height() != b.height() {
        return if a.height() > b.height() { -1 } else { 1 };
    }
    let abr = match a.probe.bitrate {
        Some(x) => x,
        None => 0.0,
    };
    let bbr = match b.probe.bitrate {
        Some(x) => x,
        None => 0.0,
    };
    if abr != bbr {
        return if abr > bbr { -1 } else { 1 };
    }
    if a.url > b.url {
        1
    } else if a.url < b.url {
        -1
    } else {
        0
    }
}

/// `rank(candidates, ceiling)`: admit, drop the rejects, stamp the judgment on
/// what survives, and order it. `sorted(key=cmp_to_key(preference))` is stable
/// in Python; a stable sort with the same comparator answers identically.
pub fn rank(candidates: Vec<Candidate>, ceiling: f64) -> Vec<Candidate> {
    let mut kept: Vec<Candidate> = Vec::new();
    for mut c in candidates {
        let (keep, last, reason) = admit(&c);
        if !keep {
            continue;
        }
        c.last_resort = last;
        c.reason = reason.to_string();
        kept.push(c);
    }
    kept.sort_by(|a, b| preference(a, b, ceiling).cmp(&0));
    kept
}

/// `visible(candidates)`: the split the card draws — a still-silent row after
/// pairing does not appear, and the hidden count is what the page prints.
pub fn visible_rows(candidates: &[Candidate]) -> (Vec<Candidate>, Vec<Candidate>) {
    let kept: Vec<Candidate> = candidates.iter().filter(|c| !c.silent()).cloned().collect();
    let hidden: Vec<Candidate> = candidates.iter().filter(|c| c.silent()).cloned().collect();
    (kept, hidden)
}

// ---------------------------------------------------------------------------
// engine 1 -- scrape the page with the standard library
// ---------------------------------------------------------------------------

/// `raw.decode("utf-8", "replace")`: the last resort in `decode_body`, and what
/// a page becomes when the charset the server announced is a lie.
///
/// CPython's UTF-8 "replace" handler writes one U+FFFD per *maximal subpart*
/// and then resumes; Rust's lossy decoder follows the same recommended
/// practice, so the two agree byte for byte on the cases in the differential
/// table rather than merely "both produce replacement characters".
pub fn utf8_replace(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// `decode_body(raw, charset='')` — try the server's label, then utf-8, then
/// gbk, then utf-8 with replacement.
///
/// [`crate::charmap::decode_by_label`]'s three-way answer maps onto Python's
/// `try/except (UnicodeDecodeError, LookupError)` exactly: `Some(Ok)` is a
/// decoded page, `Some(Err)` is a mislabeled page (ask the next name), and
/// `None` is a label this build has no table for — also ask the next name. So
/// a declined label cannot fail a page; it falls through to utf-8/replace,
/// which is the documented divergence in `charmap.rs` and is why that `None`
/// had to stay distinguishable from `Some(Err(()))`.
///
/// `charset` is `Option<&str>` because both spellings of "the server said
/// nothing" appear in the truth table: `''` (no `charset=` token) and `None`.
///
/// One Python branch is unrepresentable here and is *skipped, not faked*:
/// `isinstance(raw, str) → return raw`. This port's only producer of `raw` is
/// a socket read, so the branch has no caller.
pub fn decode_body(raw: &[u8], charset: Option<&str>) -> String {
    for name in [charset.unwrap_or(""), "utf-8", "gbk"] {
        if name.is_empty() {
            continue;
        }
        if let Some(Ok(text)) = crate::charmap::decode_by_label(raw, name) {
            return text;
        }
    }
    utf8_replace(raw)
}

/// `_PageParser`: turns the parser's callbacks into `(href, "where:attribute")`
/// rows, plus the page title the card shows.
///
/// Row order is the order the page is read in and the table is scanned in, so
/// a tag carrying both `src` and `data-src` emits **two** rows. That is
/// observable in the differential table (A10) and it is the reason `take` has
/// no `break`.
#[derive(Debug, Default)]
pub struct PageParser {
    pub found: Vec<(String, String)>,
    pub page_title: String,
    in_title: bool,
}

impl PageParser {
    /// `_PageParser.SRC_ATTRS`. `data-src` and its cousins are here because
    /// lazy-load is the normal shape of a self-hosted video page: the `src` is
    /// a placeholder and the real file sits in an attribute a script copies
    /// across.
    pub const SRC_ATTRS: [&'static str; 7] = [
        "src",
        "data-src",
        "data-orig-file",
        "data-video-src",
        "data-hls-url",
        "data-mp4",
        "data-file",
    ];

    pub fn new() -> Self {
        Self::default()
    }

    /// `_take(values, where)` — every name in the table, then `href`.
    fn take(&mut self, values: &HashMap<String, String>, where_: &str) {
        for name in Self::SRC_ATTRS.iter().copied().chain(std::iter::once("href")) {
            // `if href:` — a value only reaches this map when Python would
            // also have accepted it, so presence here is the whole test.
            if let Some(href) = values.get(name) {
                self.found
                    .push((href.clone(), format!("{}:{}", where_, name)));
            }
        }
    }
}

/// `' '.join(s.split())`: Python's whitespace runs collapsed to one space,
/// using its 29-code-point set — not the 25 Rust's `\s` knows.
fn normalize_spaces(s: &str) -> String {
    s.split(py_is_space)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl crate::html::HtmlSink for PageParser {
    fn handle_starttag(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        if tag == "title" {
            self.in_title = true;
            return;
        }
        // `for key, value in attrs: if value: values.setdefault(key.lower(), value)`
        // — a bare attribute (`src`, no `=`) is `None` in CPython and an empty
        // one is `''`; neither is a value. Names arrive from `html.rs` already
        // lowercased (`to_lowercase` on both tag and attribute), so `key.lower()`
        // is not re-applied here.
        let mut values: HashMap<String, String> = HashMap::new();
        for (key, value) in attrs {
            if let Some(value) = value {
                if !value.is_empty() {
                    values.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
        match tag {
            "video" | "audio" | "source" | "embed" => self.take(&values, tag),
            "link" => {
                let rel = normalize_spaces(
                    &values
                        .get("rel")
                        .map(|value| value.to_lowercase())
                        .unwrap_or_default(),
                );
                // `'preload' in rel` is a *substring* test, and `as` is compared
                // case-sensitively against the raw attribute value — the
                // `lower()` above belongs to `rel` alone.
                if rel.contains("preload")
                    && matches!(
                        values.get("as").map(String::as_str),
                        Some("video") | Some("fetch")
                    )
                {
                    let mut one = HashMap::new();
                    if let Some(href) = values.get("href") {
                        one.insert("href".to_string(), href.clone());
                    }
                    self.take(&one, "preload");
                }
            }
            _ => {}
        }
    }

    fn handle_endtag(&mut self, tag: &str) {
        if tag == "title" {
            self.in_title = false;
        }
    }

    fn handle_data(&mut self, data: &str) {
        // The first non-empty `<title>` text is the page's; a site that
        // animates the tab title with a script would only be editing the same
        // node. Truncation is by *character*, and `html.rs` counts characters
        // too — a Chinese title is the case that makes that load-bearing.
        if !self.in_title || !self.page_title.is_empty() {
            return;
        }
        let stripped = py_strip(data);
        if stripped.is_empty() {
            return;
        }
        self.page_title = stripped.chars().take(TITLE_MAX_CHARS).collect();
    }
}

/// `_OG_VIDEO_NAMES` — `secure_url` first so a page offering both gives the
/// https one a stable place in the list.
const OG_VIDEO_NAMES: [&str; 3] = ["og:video:secure_url", "og:video:url", "og:video"];

/// `re.compile(r'<meta\b[^>]*>')` with `IGNORECASE`, which `(?i)` carries.
static META_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<meta\b[^>]*>").expect("meta tag regex"));

/// `_META_ATTR`, **without** `IGNORECASE`: the page's own casing survives the
/// scan and only gets lowered when it is looked up, which is what makes a
/// `PROPERTY=` tag behave like a `property=` one while an `HTTP-EQUIV` value
/// stays exactly as written.
///
/// Rust's `\s` misses U+001C..U+001F, which Python's str `\s` includes, so the
/// class is spelled out. `\w` is the documented divergence in the other
/// direction: Rust includes Unicode marks, Python's `isalnum` does not, and no
/// attribute name in the table exercises it.
static META_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    let around_eq = r#"[\s\x{1C}-\x{1F}]*"#;
    Regex::new(&format!(
        r#"([\w:-]+){around_eq}={around_eq}["']([^"']*)["']"#
    ))
    .expect("meta attribute regex")
});

/// `_meta_og_video(body)` — read with a regex rather than the parser:
/// attribute order is not stable across generators, `<meta>` may or may not be
/// self-closed, and the tag can appear before `<body>`, where a one-shot read
/// of the head is all we need.
pub fn meta_og_video(body: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for tag in META_TAG.find_iter(body) {
        let mut attrs: HashMap<String, String> = HashMap::new();
        for cap in META_ATTR.captures_iter(tag.as_str()) {
            let key = cap[1].to_lowercase();
            let value = cap[2].to_string();
            attrs.entry(key).or_insert(value);
        }
        // `attrs.get('property') or attrs.get('name') or ''` — an *empty*
        // property falls through to name, which is not the same as `None`.
        let name = attrs
            .get("property")
            .filter(|value| !value.is_empty())
            .or_else(|| attrs.get("name").filter(|value| !value.is_empty()))
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        if OG_VIDEO_NAMES.contains(&name.as_str()) {
            if let Some(content) = attrs.get("content") {
                if !content.is_empty() {
                    found.push((content.clone(), name));
                }
            }
        }
    }
    found
}

/// `_LITERAL_URL` — one capture group, mandatory, with the query tail inside
/// it. The character class ends the match at a quote, an angle bracket, a
/// backslash, or whitespace, which is how a URL embedded in prose stops at the
/// prose. Python's str `\s` set is spelled out for the same reason as above.
static LITERAL_URL: LazyLock<Regex> = LazyLock::new(|| {
    let not = r#"[^\s\x{1C}-\x{1F}"'<>\\]"#;
    let suffixes = MEDIA_SUFFIXES
        .iter()
        .map(|suffix| suffix.trim_start_matches('.'))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(
        r"(?i)((?:https?://|//|/){not}*?\.(?:{suffixes})(?:\?{not}*)?)"
    ))
    .expect("literal URL regex")
});

/// `_literal_urls(body)` — the third raw source, and the one that caught the
/// PHP-escaped m3u8s (`\/`) that `json_unescape` exists for. Scanning happens
/// *after* unescaping, on the whole body, tags included.
pub fn literal_urls(body: &str) -> Vec<(String, String)> {
    let text = json_unescape(body);
    LITERAL_URL
        .captures_iter(&text)
        // Group 1 is the address; the full match is the same thing here, but
        // Python reads `.group(1)` and the group is what makes the class
        // boundaries testable.
        .map(|cap| (cap[1].to_string(), "literal".to_string()))
        .collect()
}

/// `_fetch_page(page_url)` — the only place this crate speaks HTTP as a
/// *client*. Every failure shape (DNS, timeout, 4xx, 5xx, a body that cannot
/// be read) is Python's `(URLError, OSError, ValueError) → ''`: a page we
/// cannot read is an empty page, and the candidate list then says so.
pub fn fetch_page(page_url: &str, opener: Option<&dyn Fn(&str) -> String>) -> String {
    if let Some(opener) = opener {
        // `opener(page_url) or ''` — an empty answer stays empty.
        return opener(page_url);
    }
    let response = ureq::get(page_url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
        .config()
        .timeout_connect(Some(Duration::from_secs(SCRAPE_TIMEOUT_SECS)))
        .timeout_recv_response(Some(Duration::from_secs(SCRAPE_TIMEOUT_SECS)))
        .build()
        .call();
    let Ok(response) = response else {
        return String::new();
    };
    let mut body = response.into_body();
    // `get_content_charset()` is `None` when the header carries no charset
    // token; `''` and `None` must both mean "ask utf-8 next".
    let charset = body.charset().unwrap_or_default().to_string();
    let mut raw = Vec::new();
    // `response.read(PAGE_MAX_BYTES)` truncates silently — it never errors, so
    // a long page is a short page here, not a failed one.
    if Read::take(body.as_reader(), PAGE_MAX_BYTES as u64)
        .read_to_end(&mut raw)
        .is_err()
    {
        return String::new();
    }
    decode_body(
        &raw,
        if charset.is_empty() {
            None
        } else {
            Some(charset.as_str())
        },
    )
}

/// `scrape_page(page_url, body=None, opener=None)`.
///
/// The engine's whole claim: only what the HTML *states outright*. Three raw
/// sources, merged in this order — parser attributes, `og:video`, literal
/// suffixes — then made absolute against the **stem** (the page URL with its
/// fragment removed), with the page itself and duplicates removed.
///
/// It feeds and **never closes**, which is the Python and is not an oversight.
/// The reason was measured, because the obvious one is wrong: CPython's
/// `check_for_whole_start_tag` refuses a partial start tag **even at `close()`**,
/// so a page cut inside `<video src="…` leaves that attribute unsampled either
/// way — closing here does *not* invent candidates. What `close()` does flush is
/// buffered character data, and `<title>` is the only data this function reads,
/// so the shape that is load-bearing is a page that ends inside its title. Both
/// shapes are in the corpus (`scrape_tests.rs` `sp_10`–`sp_17`: six gain a title
/// the reference never had, two answer identically either way). (The differential
/// test for the parser itself does close, because that is what the oracle does.)
///
/// `absolute_url` can fail for one reason this port keeps loud rather than
/// swallowing: a base URL whose *scheme* cannot be parsed is Python's
/// `ValueError`, and it escapes `scrape_page` un-caught there, so it propagates
/// here too.
pub fn scrape_page(
    page_url: Option<&str>,
    body: Option<&str>,
    opener: Option<&dyn Fn(&str) -> String>,
) -> Result<Vec<Candidate>, PyUrlError> {
    let text = match body {
        Some(text) => text.to_string(),
        None => match page_url {
            Some(url) if !url.is_empty() => fetch_page(url, opener),
            _ => String::new(),
        },
    };
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut parser = crate::html::HtmlParser::new(PageParser::new());
    parser.feed(&text);
    let parser = parser.sink;
    let mut raw = parser.found.clone();
    raw.extend(meta_og_video(&text));
    raw.extend(literal_urls(&text));

    let stem = page_url.map(|url| url.split('#').next().unwrap_or("")).unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut found: Vec<Candidate> = Vec::new();
    for (href, where_) in raw {
        let Some(url) = absolute_url(stem, &href)? else {
            continue;
        };
        // A relative `src` that resolves back to the page is not a media
        // address, it is the page being pointed at itself.
        if url.split('#').next().unwrap_or("") == stem || !seen.insert(url.clone()) {
            continue;
        }
        found.push(Candidate {
            label: where_,
            title: if parser.page_title.is_empty() {
                None
            } else {
                Some(parser.page_title.clone())
            },
            ..Candidate::new(url, "page".to_string())
        });
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_unescape_handles_the_three_shapes() {
        crate::pin_test_dir();
        assert_eq!(json_unescape(r"\/"), "/");
        assert_eq!(json_unescape(r"\\"), "\\");
        assert_eq!(json_unescape(r"\u0041"), "A");
        // Non-ASCII is left as-is.
        assert_eq!(json_unescape(r"\u4e2d"), r"\u4e2d");
        // Malformed is left as-is.
        assert_eq!(json_unescape(r"\uXYZ"), r"\uXYZ");
    }

    #[test]
    fn absolute_url_rejects_unfetchable_schemes() {
        crate::pin_test_dir();
        assert!(absolute_url("https://example.com", "data:text/plain,x")
            .unwrap()
            .is_none());
        assert!(
            absolute_url("https://example.com", "blob:https://example.com/x")
                .unwrap()
                .is_none()
        );
        assert!(absolute_url("https://example.com", "javascript:void(0)")
            .unwrap()
            .is_none());
    }

    #[test]
    fn absolute_url_resolves_relative_urls() {
        crate::pin_test_dir();
        assert_eq!(
            absolute_url("https://example.com/a/b", "/c/d").unwrap(),
            Some("https://example.com/c/d".to_string())
        );
        assert_eq!(
            absolute_url("https://example.com/a/b", "c").unwrap(),
            Some("https://example.com/a/c".to_string())
        );
    }

    #[test]
    fn looks_like_url_requires_scheme() {
        crate::pin_test_dir();
        assert!(looks_like_url("https://example.com").unwrap());
        assert!(!looks_like_url("example.com/watch/7").unwrap());
        assert!(!looks_like_url("").unwrap());
    }

    #[test]
    fn extract_share_urls_stops_before_fullwidth_punctuation() {
        crate::pin_test_dir();
        let text = "https://v.douyin.com/iAbc/，复制打开抖音";
        let urls = extract_share_urls(text);
        assert_eq!(urls, vec!["https://v.douyin.com/iAbc/"]);
    }

    #[test]
    fn cookie_browser_spec_rejects_container_and_keyring() {
        crate::pin_test_dir();
        assert!(cookie_browser_spec("firefox::container").is_none());
        assert!(cookie_browser_spec("chrome+gnome").is_none());
    }

    #[test]
    fn cookie_browser_spec_accepts_profile_path() {
        crate::pin_test_dir();
        // Windows path with colon is legal.
        assert_eq!(
            cookie_browser_spec("vivaldi:C:\\Users\\test"),
            Some("vivaldi:C:\\Users\\test".to_string())
        );
    }

    #[test]
    fn looks_like_url_reproduces_urlsplit() {
        crate::pin_test_dir();
        assert!(looks_like_url("https://example.com").unwrap());
        assert!(looks_like_url("HTTP://EXAMPLE.com").unwrap());
        // Python's urlsplit accepts the empty host; `url::Url::parse` did not.
        assert!(looks_like_url("https://").unwrap());
        // `http:8080` is a scheme to urlsplit, not a host:port (battery-verified).
        assert!(looks_like_url("http:8080").unwrap());
        // Two tokens is prose about an address, not one.
        assert!(!looks_like_url("https://example.com 复制打开抖音").unwrap());
        assert!(!looks_like_url("ftp://example.com").unwrap());
        // A syntactically valid scheme that is not one of ours.
        assert!(!looks_like_url("example.com:8080/x").unwrap());
        // A scheme must start with an ASCII letter.
        assert!(!looks_like_url("1http://x").unwrap());
        // U+017F folds into `s` for `_SHARE_URL` (re.IGNORECASE), but it is not
        // an ASCII letter to urlsplit: no scheme, so not a whole paste that is
        // an address.
        assert!(!looks_like_url("http\u{17f}://x").unwrap());
        // A bad bracket count is a ValueError in Python, not a silent false.
        assert!(looks_like_url("http://[::1").is_err());
    }

    #[test]
    fn extract_share_urls_matches_python_share_regex() {
        crate::pin_test_dir();
        // ASCII ')' is stripped by the rstrip set...
        assert_eq!(
            extract_share_urls("看这个(https://example.com/a)"),
            vec!["https://example.com/a"]
        );
        // ...curly quotes end the match in the regex itself...
        assert_eq!(
            extract_share_urls("看\u{201c}https://example.com/a\u{201d}这里"),
            vec!["https://example.com/a"]
        );
        assert_eq!(
            extract_share_urls("<https://example.com/a>"),
            vec!["https://example.com/a"]
        );
        // `(?i)` matches case-insensitively but never rewrites the match.
        assert_eq!(
            extract_share_urls("HTTPS://Example.com/A"),
            vec!["HTTPS://Example.com/A"]
        );
        // The dotted-I family: Python's re.IGNORECASE folds U+017F into `s`
        // and matches; Rust's simple case folding does the same.
        assert_eq!(
            extract_share_urls("http\u{17f}://x"),
            vec!["http\u{17f}://x"]
        );
        // U+001C is Python whitespace (`str.isspace()`) but not in Rust's
        // Unicode White_Space set, so the class must spell it out.
        assert_eq!(extract_share_urls("http://a\u{1c}b"), vec!["http://a"]);
        // A trailing backslash is NOT stripped: Python's rstrip set is
        // '.,;:!?)\'"' and does not contain it.
        assert_eq!(
            extract_share_urls("https://example.com/a\\ 后面"),
            vec!["https://example.com/a\\"]
        );
        // U+3005 is NOT in Python's class, so it stays inside the address --
        // the witness that the class is the explicit list, not a char range.
        assert_eq!(
            extract_share_urls("https://a.com/x々尾"),
            vec!["https://a.com/x々尾"]
        );
        // Ordered dedup keeps the first of two copies.
        assert_eq!(
            extract_share_urls("https://a.com/x 和 https://a.com/x"),
            vec!["https://a.com/x"]
        );
        assert_eq!(
            extract_share_urls("https://a.com/x 和 https://b.com/y"),
            vec!["https://a.com/x", "https://b.com/y"]
        );
    }

    #[test]
    fn resolve_target_reads_the_share_blurb() {
        crate::pin_test_dir();
        assert_eq!(
            resolve_target("  https://example.com/a  ").unwrap(),
            ("https://example.com/a".to_string(), 1)
        );
        assert_eq!(
            resolve_target("7.12 复制打开抖音 https://v.douyin.com/iAbc/ 看看").unwrap(),
            ("https://v.douyin.com/iAbc/".to_string(), 1)
        );
        assert_eq!(
            resolve_target("看 https://a.com/x 和 https://b.com/y").unwrap(),
            ("https://a.com/x".to_string(), 2)
        );
        // The bare-host rule still holds: this is a caller-side refusal.
        assert_eq!(
            resolve_target("example.com/watch/7").unwrap(),
            (String::new(), 0)
        );
        assert_eq!(resolve_target("   ").unwrap(), (String::new(), 0));
    }

    #[test]
    fn absolute_url_matches_python_urljoin() {
        crate::pin_test_dir();
        assert_eq!(
            absolute_url("http://a/b", "//host/x").unwrap(),
            Some("http://host/x".to_string())
        );
        // With no base, a leading `/` (protocol-relative included) is refused
        // before urljoin ever runs.
        assert_eq!(absolute_url("", "//host/x").unwrap(), None);
        assert_eq!(absolute_url("", "/x").unwrap(), None);
        // A whitespace-only href passes `if not href`, strips to '', and
        // urljoin hands the base back.
        assert_eq!(
            absolute_url("http://a/", "   ").unwrap(),
            Some("http://a/".to_string())
        );
        assert_eq!(
            absolute_url("http://a/b/c/d;p;q", "g").unwrap(),
            Some("http://a/b/c/g".to_string())
        );
        assert_eq!(
            absolute_url("http://a/b?x", "?q").unwrap(),
            Some("http://a/b?q".to_string())
        );
    }

    #[test]
    fn py_urlsplit_matches_python_bracket_parity() {
        crate::pin_test_dir();
        // One extra `]`: both brackets are present, so Python's parity check
        // passes and the netloc keeps the stray bracket.
        let split = py_urlsplit("http://[::1]:80]/x", "", true).unwrap();
        assert_eq!(split.netloc, "[::1]:80]");
        assert_eq!(split.path, "/x");
        // An unclosed `[` is a ValueError.
        assert!(py_urlsplit("http://[::1", "", true).is_err());
        assert_eq!(py_urlsplit("//x", "http", true).unwrap().scheme, "http");
    }

    #[test]
    fn py_url_unparse_shapes_match_python() {
        crate::pin_test_dir();
        assert_eq!(py_urlunsplit("http", "a", "", "", ""), "http://a");
        assert_eq!(py_urlunsplit("ws", "", "/c", "", ""), "ws:///c");
        // Two `//`s of its own plus the empty-netloc branch's doubling.
        assert_eq!(py_urlunsplit("", "", "//x", "", ""), "////x");
        assert_eq!(py_urlunparse("http", "a", "", "", "q", ""), "http://a?q");
        assert_eq!(py_urlunparse("http", "a", "/x", "p", "", ""), "http://a/x;p");
        // `_splitparams` with no `/` in the path: the first `;` anywhere is
        // the separator.
        let (split, params) = py_urlparse("x;y", "", true).unwrap();
        assert_eq!(split.path, "x");
        assert_eq!(params, "y");
        let (split, params) = py_urlparse("http://a/x;y", "", true).unwrap();
        assert_eq!(split.path, "/x");
        assert_eq!(params, "y");
    }

    #[test]
    fn cookie_browser_spec_normalises_like_python() {
        crate::pin_test_dir();
        assert_eq!(cookie_browser_spec("Chrome:"), Some("chrome".to_string()));
        assert_eq!(
            cookie_browser_spec(" chrome : C:\\Users\\Test "),
            Some("chrome:C:\\Users\\Test".to_string())
        );
        assert_eq!(
            cookie_browser_spec("chrome:Default+1"),
            Some("chrome:Default+1".to_string())
        );
        // '+' in the profile half is fine; the engine only knows +KEYRING
        // in the name slot.
        assert_eq!(
            cookie_browser_spec("chrome:+gnome"),
            Some("chrome:+gnome".to_string())
        );
        // Python's str.strip() treats U+001C..U+001F as whitespace (29 code
        // points) where Rust's trim() does not (25). Each half is stripped
        // separately, so an inner separator is eaten from the profile half.
        assert_eq!(cookie_browser_spec("\x1cchrome"), Some("chrome".to_string()));
        assert_eq!(
            cookie_browser_spec("chrome:\x1cprof"),
            Some("chrome:prof".to_string())
        );
        assert_eq!(
            cookie_browser_spec("\x1cchrome:prof\x1c"),
            Some("chrome:prof".to_string())
        );
        assert_eq!(cookie_browser_spec("chro\rme"), None);
        assert_eq!(cookie_browser_spec("chrome:pr\tofile"), None);
        assert_eq!(cookie_browser_spec("chrome:-profile"), None);
        assert_eq!(cookie_browser_spec("chrome:Default::container"), None);
    }

    #[test]
    fn title_for_uri_matches_python() {
        crate::pin_test_dir();
        let t = |uri: &str, fb: &str| title_for_uri(uri, fb).unwrap();

        // Stem comes from the basename, query and fragment are not part of it.
        assert_eq!(
            t("https://example.com/videos/movie.mp4?x=1#t=10", ""),
            "movie"
        );
        // The match lowercases the comparison but never the result.
        assert_eq!(t("http://a/MOVIE.MP4", ""), "MOVIE");
        assert_eq!(t("http://a/movie.MP4", ""), "movie");

        // Empty stem: the fallback wins when it is truthy, else the raw uri.
        assert_eq!(t("http://a/", ""), "http://a/");
        assert_eq!(t("http://a/", "X"), "X");
        assert_eq!(t("http://a/", " "), " ");

        // Python strips str, slices by characters, and lowercases the compare:
        // U+212A lowercases to 'k', so ".m\u{212A}v" matches ".mkv" as 4 chars.
        assert_eq!(t("http://a/x.m\u{212A}v", ""), "x");
        assert_eq!(t("http://a/MOVIE.M\u{212A}V", ""), "MOVIE");
        // Four chars from a four-char stem leave the empty string -> fallback.
        assert_eq!(t("http://a/.m\u{212A}v", ""), "http://a/.m\u{212A}v");
        assert_eq!(t("http://a/.m\u{212A}v", "FB"), "FB");

        // A stray '%' never decodes; the ascii run passes through verbatim.
        assert_eq!(t("http://a/a%zzb.mp4", ""), "a%zzb");
        assert_eq!(t("http://a/a%.mp4", ""), "a%");
        assert_eq!(t("http://a/movie%20.mp4", ""), "movie");
        // Decoding happens after basename, before the suffix check: the
        // decoded '/' stays inside the stem, and a decoded '.' joins the dot.
        assert_eq!(t("http://a/a%2Fb.mp4", ""), "a/b");
        assert_eq!(t("http://a/movie%2Emp4", ""), "movie");
        // strip() runs before the suffix check: " .mp4" collapses to ".mp4",
        // gets stripped, and the stem is empty again.
        assert_eq!(t("http://a/%20.mp4", ""), "http://a/%20.mp4");

        // errors="replace" counts per broken sequence: an overlong 3-byte
        // sequence is three replacement characters, not one.
        assert_eq!(t("http://a/%E0%80%80.mp4", ""), "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(t("http://a/%C3%A9.mp4", ""), "é");
        assert_eq!(t("http://a/%E6%B1%89%E7%94%B5.mp4", ""), "汉电");
        // '%25' decodes to a literal '%' first; the bytes after it are then
        // read one ascii run at a time, so "%E6" stays text and the two raw
        // bytes each decode separately.
        assert_eq!(t("http://a/%25E6%B1%89.mp4", ""), "%E6\u{FFFD}\u{FFFD}");

        // Only a full suffix is removed, once.
        assert_eq!(t("http://a/x.mp4y", ""), "x.mp4y");
        assert_eq!(t("http://a/xmkv", ""), "xmkv");
        assert_eq!(t("http://a/movie.mp4;x", ""), "movie.mp4;x");
        assert_eq!(t("http://a/movie.mp4.mp4", ""), "movie.mp4");
        assert_eq!(t("http://a/movie.mp4.", ""), "movie.mp4.");

        // All-dot names lose exactly the four suffix characters.
        assert_eq!(t("http://a/..mp4", ""), ".");
        assert_eq!(t("http://a/%2E%2E.mp4", ""), "..");

        // TITLE_MAX_CHARS counts characters, not bytes.
        assert_eq!(t(&("a".repeat(121) + ".mp4"), ""), "a".repeat(120));
        assert_eq!(t(&("汉".repeat(121) + ".mp4"), ""), "汉".repeat(120));

        // A broken IPv6 netloc raises, exactly where Python's urlsplit does;
        // without a netloc the bracket is ordinary path text.
        assert_eq!(title_for_uri("http://[::1", ""), Err(PyUrlError));
        assert_eq!(t("[::1/movie.mp4", ""), "movie");
    }

    #[test]
    fn display_title_strips_cuts_by_character_and_falls_back_to_the_url() {
        crate::pin_test_dir();
        let t = |title: Option<&str>, url: &str, fallback: &str| {
            let mut c = Candidate::new(url.to_string(), "page".to_string());
            c.title = title.map(|s| s.to_string());
            display_title(&c, fallback).unwrap()
        };
        assert_eq!(t(Some("  Hello  "), "http://a/", ""), "Hello");
        // \x1c and U+202F are Python whitespace; strip() takes both ends.
        assert_eq!(t(Some("\u{1c}\u{202f} x \u{202f}\u{1c}"), "http://a/", ""), "x");
        // KELVIN SIGN is a letter, not whitespace: strip() leaves it standing.
        assert_eq!(t(Some("\u{212a}"), "http://a/", ""), "\u{212a}");
        // An empty title falls back to title_for_uri; an empty fallback leaves
        // the whole address as the name.
        assert_eq!(t(None, "http://a/movie.mp4", ""), "movie");
        assert_eq!(t(None, "http://a/", ""), "http://a/");
        assert_eq!(t(None, "http://a/", "FB"), "FB");
        // TITLE_MAX_CHARS counts characters, not bytes.
        assert_eq!(t(Some(&("汉".repeat(119) + "extra")), "http://a/", ""), "汉".repeat(119) + "e");
        assert_eq!(t(Some(&("汉".repeat(120) + "more")), "http://a/", ""), "汉".repeat(120));
        assert_eq!(t(Some(&"x".repeat(121)), "http://a/", ""), "x".repeat(120));
    }

    #[test]
    fn container_label_takes_the_path_suffix_before_the_probed_name() {
        crate::pin_test_dir();
        let c = |url: &str, container: Option<&str>| {
            let mut cand = Candidate::new(url.to_string(), "page".to_string());
            cand.probe.container = container.map(|s| s.to_string());
            container_label(&cand)
        };
        // The suffix wins even when ffprobe listed a whole family.
        assert_eq!(c("http://a/v.MP4", Some("mov,mp4,m4a,3gp,3g2,mj2")), "MP4");
        // The path is lowercased before matching, so KELVIN folds into 'k'.
        assert_eq!(c("http://a/v.m\u{212a}v", None), "MKV");
        assert_eq!(c("http://a/v.m3u8", Some("mov,x")), "M3U8");
        // No suffix: the first listed name, stripped, upper-cased.
        assert_eq!(c("http://a/v", Some("mov,mp4,m4a,3gp,3g2,mj2")), "MOV");
        assert_eq!(c("http://a/v", Some(" matroska,webm ")), "MATROSKA");
        assert_eq!(c("http://a/v", Some(",first")), "");
        // Full Unicode case mapping: ß grows into SS, ﬀ into FF.
        assert_eq!(c("http://a/v", Some("ß, x")), "SS");
        assert_eq!(c("http://a/v", Some("ﬀx")), "FFX");
        assert_eq!(c("http://a/v", Some("")), "");
        assert_eq!(c("http://a/v", None), "");
        assert_eq!(c("http://a/v", Some("video/mp2t")), "VIDEO/MP2T");
    }

    #[test]
    fn relay_reason_and_plan_answer_the_four_shapes_in_order() {
        crate::pin_test_dir();
        const MERGE: &str = "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出";
        const REMUX: &str = "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流";
        const FALLBACK: &str = "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403";
        let c = |url: &str, audio: Option<&str>, keys: &[&str]| {
            let mut cand = Candidate::new(url.to_string(), "page".to_string());
            cand.audio_url = audio.map(|s| s.to_string());
            for k in keys {
                cand.headers.insert(k.to_string(), "v".to_string());
            }
            cand
        };
        // merge short-circuits: it answers before any URL parsing, so even the
        // broken netloc that raises in the oracle still gets merge here.
        let m = c("http://a/v.mp4", Some("http://a/a.m4a"), &[]);
        assert_eq!(relay_reason(&m), MERGE);
        assert_eq!(plan(&m), "merge");
        let m = c("http://a/v.m3u8", Some("http://a/a"), &["Z"]);
        assert_eq!(relay_reason(&m), MERGE);
        assert_eq!(plan(&m), "merge");
        let m = c("http://[::1", Some("http://a/a"), &[]);
        assert_eq!(relay_reason(&m), MERGE);
        assert_eq!(plan(&m), "merge");
        // segmented beats headers.
        let r = c("http://a/v.m3u8", None, &["Z"]);
        assert_eq!(relay_reason(&r), REMUX);
        assert_eq!(plan(&r), "remux");
        let r = c("http://a/v.mpd", None, &[]);
        assert_eq!(relay_reason(&r), REMUX);
        assert_eq!(plan(&r), "remux");
        // Headers sort by code point and join with the ideographic comma.
        let p = c("http://a/v.mp4", None, &["Z", "a", "b", "中"]);
        assert_eq!(
            relay_reason(&p),
            "源站要求请求头（Z、a、b、中）：电视不会带上这些，由 Macast 代取"
        );
        assert_eq!(plan(&p), "proxy");
        let p = c("http://a/v.mp4", None, &["中"]);
        assert_eq!(relay_reason(&p), "源站要求请求头（中）：电视不会带上这些，由 Macast 代取");
        // The query string is not the path: "?next=.m3u8" is not segmented,
        // and neither is a probed Matroska name next to "?x=.m3u8".
        let p = c("http://a/v.mp4?next=.m3u8", None, &[]);
        assert_eq!(relay_reason(&p), FALLBACK);
        assert_eq!(plan(&p), "proxy");
        let mut p = c("http://a/v?x=.m3u8", None, &[]);
        p.probe.container = Some("matroska,webm".to_string());
        assert_eq!(relay_reason(&p), FALLBACK);
        assert_eq!(plan(&p), "proxy");
    }

    #[test]
    fn the_four_functions_stay_total_on_a_broken_netloc() {
        crate::pin_test_dir();
        // The oracle raises ValueError from urlsplit for all four (every path
        // runs through Candidate.urlsplit); the port keeps them total on
        // purpose, the same agreement host()/path_suffix() already keep:
        // no suffix matches, nothing is segmented, no header sentence.
        let mut cand = Candidate::new("http://[::1".to_string(), "page".to_string());
        cand.probe.container = Some("mov,mp4,m4a,3gp,3g2,mj2".to_string());
        assert_eq!(container_label(&cand), "MOV");
        assert_eq!(relay_reason(&cand), "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403");
        assert_eq!(plan(&cand), "proxy");
        // display_title only touches the URL when the title is empty -- and
        // then the fallback is not consulted either, the parse raises first.
        assert_eq!(display_title(&cand, ""), Err(PyUrlError));
        assert_eq!(display_title(&cand, "fb"), Err(PyUrlError));
        cand.title = Some("T".to_string());
        assert_eq!(display_title(&cand, ""), Ok("T".to_string()));
        // 'http://]' is broken at the same place.
        let bracket = Candidate::new("http://]".to_string(), "page".to_string());
        assert_eq!(display_title(&bracket, "fb"), Err(PyUrlError));
        assert_eq!(relay_reason(&bracket), "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403");
        assert_eq!(plan(&bracket), "proxy");
    }

    // ---- mr5 differential helpers -------------------------------------
    // These tests are the permanent half of the media_resolve porting gate:
    // every expectation below was rendered by the Python oracle over the same
    // inputs, byte for byte. Nothing here reads /tmp/mr5_bin; the fake
    // ffprobe scripts are re-created under the process temp dir instead.

    fn esc5(s: &str) -> String {
        let mut out = String::from("\"");
        for ch in s.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    fn json5(raw: &str) -> Value {
        serde_json::from_str(raw).expect("corpus json parses")
    }

    fn strmap5(v: &Value) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if let Value::Object(map) = v {
            for (k, val) in map {
                m.insert(
                    k.clone(),
                    val.as_str()
                        .unwrap_or_else(|| panic!("map value must be a string: {val}"))
                        .to_string(),
                );
            }
        }
        m
    }

    fn rend_map5(m: &HashMap<String, String>) -> String {
        if m.is_empty() {
            return "{}".to_string();
        }
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort();
        let body = keys
            .iter()
            .map(|k| format!("{}={}", esc5(k), esc5(&m[*k])))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{{}}}", body)
    }

    fn rend_probe5(p: &Probe) -> String {
        format!(
            "[reach:{} container:{} video_codec:{} audio_codec:{} width:i:{} height:i:{} duration:f:{:016x} bitrate:f:{:016x} is_live:{} error:{} measured:{} moving_picture:{}]",
            esc5(&p.reach),
            esc5(p.container.as_deref().unwrap_or("")),
            esc5(p.video_codec.as_deref().unwrap_or("")),
            esc5(p.audio_codec.as_deref().unwrap_or("")),
            p.video_width.unwrap_or(0),
            p.video_height.unwrap_or(0),
            p.duration.unwrap_or(0.0).to_bits(),
            p.bitrate.unwrap_or(0.0).to_bits(),
            p.is_live,
            esc5(p.error.as_deref().unwrap_or("")),
            p.measured(),
            p.moving_picture(),
        )
    }

    fn rend_row5(c: &Candidate, probe_null: bool) -> String {
        format!(
            "[url:{} origin:{} headers:{} label:{} title:{} probe:{} probe_null:{} last_resort:{} reason:{} audio_codec:{} audio_url:{} audio_headers:{} audio_label:{} silent:{} height:i:{} segmented:{}]",
            esc5(&c.url),
            esc5(&c.engine),
            rend_map5(&c.headers),
            esc5(&c.label),
            esc5(c.title.as_deref().unwrap_or("")),
            if probe_null {
                "null".to_string()
            } else {
                rend_probe5(&c.probe)
            },
            probe_null,
            c.last_resort,
            esc5(&c.reason),
            esc5(&c.audio_codec),
            esc5(c.audio_url.as_deref().unwrap_or("")),
            rend_map5(&c.audio_headers),
            esc5(&c.audio_label),
            c.silent(),
            c.height(),
            c.segmented(),
        )
    }

    fn rend_rows5(rows: &[String]) -> String {
        format!("[{}]", rows.iter().map(|r| esc5(r)).collect::<Vec<_>>().join(", "))
    }

    fn rend_tuple5(a: &[String], b: &[String]) -> String {
        format!("({}, {})", rend_rows5(a), rend_rows5(b))
    }

    fn opt_str5(v: &Value, key: &str) -> Option<String> {
        match v.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => panic!("{key} is not a string: {other}"),
        }
    }

    fn opt_u32_5(v: &Value, key: &str) -> Option<u32> {
        match v.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => Some(
                n.as_u64()
                    .unwrap_or_else(|| panic!("{key} must be an integer")) as u32,
            ),
            Some(other) => panic!("{key} is not a number: {other}"),
        }
    }

    fn opt_f64_5(v: &Value, key: &str) -> Option<f64> {
        match v.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => Some(
                n.as_f64()
                    .unwrap_or_else(|| panic!("{key} must be a number")),
            ),
            Some(Value::String(s)) => Some(
                s.parse()
                    .unwrap_or_else(|_| panic!("{key} string must parse as a float")),
            ),
            Some(other) => panic!("{key} is not a float: {other}"),
        }
    }

    fn probe5(v: &Value) -> Probe {
        let mut p = Probe::new();
        if let Some(Value::String(r)) = v.get("reach") {
            p.reach = r.clone();
        }
        p.container = opt_str5(v, "container");
        p.video_codec = opt_str5(v, "video_codec");
        p.audio_codec = opt_str5(v, "audio_codec");
        p.video_width = opt_u32_5(v, "width");
        p.video_height = opt_u32_5(v, "height");
        p.duration = opt_f64_5(v, "duration");
        p.bitrate = opt_f64_5(v, "bitrate");
        if let Some(Value::Bool(b)) = v.get("is_live") {
            p.is_live = *b;
        }
        p.error = opt_str5(v, "error");
        p
    }

    /// One corpus candidate -> (Candidate, probe_null). probe_null mirrors the
    /// oracle's "no probe was attached" flag: a missing or null probe key
    /// leaves Probe::new() in place and marks the row as never probed.
    fn cand5(v: &Value) -> (Candidate, bool) {
        let url = v
            .get("url")
            .and_then(Value::as_str)
            .expect("candidate needs a url")
            .to_string();
        let origin = v
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("page")
            .to_string();
        let mut c = Candidate::new(url, origin);
        if let Some(h) = v.get("headers") {
            c.headers = strmap5(h);
        }
        if let Some(h) = v.get("audio_headers") {
            c.audio_headers = strmap5(h);
        }
        if let Some(Value::String(s)) = v.get("title") {
            c.title = Some(s.clone());
        }
        if let Some(Value::String(s)) = v.get("label") {
            c.label = s.clone();
        }
        if let Some(Value::String(s)) = v.get("reason") {
            c.reason = s.clone();
        }
        if let Some(Value::String(s)) = v.get("audio_codec") {
            c.audio_codec = s.clone();
        }
        if let Some(Value::String(s)) = v.get("audio_label") {
            c.audio_label = s.clone();
        }
        if let Some(Value::String(s)) = v.get("audio_url") {
            c.audio_url = Some(s.clone());
        }
        if let Some(Value::Bool(b)) = v.get("last_resort") {
            c.last_resort = *b;
        }
        let probe_null = match v.get("probe") {
            None | Some(Value::Null) => {
                c.probe = Probe::new();
                true
            }
            Some(p) => {
                c.probe = probe5(p);
                false
            }
        };
        (c, probe_null)
    }

    fn cand_half(v: &Value) -> Candidate {
        cand5(v).0
    }

    /// The permanent half of the oracle's fake ffprobe family. Same payloads,
    /// minus the one-time receipt echo the differential gate used.
    fn fakes5() -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        static INIT: std::sync::Once = std::sync::Once::new();
        let dir = std::env::temp_dir().join(format!("macast-mr5-{}", std::process::id()));
        INIT.call_once(|| {
            let ok = "{\"format\": {\"format_name\": \"mov,mp4,m4a,3gp,3g2,mj2\", \"duration\": \"212.288000\", \"bit_rate\": \"2833943\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 1920, \"height\": 1080, \"bit_rate\": \"2600000\"}, {\"codec_type\": \"audio\", \"codec_name\": \"aac\"}]}";
            let live = "{\"format\": {\"format_name\": \"mpegts\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 1280, \"height\": 720}]}";
            let scripts: [(&str, String); 14] = [
                ("ok.sh", format!("#!/bin/sh\nprintf '%s\\n' '{ok}'\nexit 0\n")),
                ("live.sh", format!("#!/bin/sh\nprintf '%s\\n' '{live}'\nexit 0\n")),
                ("refused403.sh", "#!/bin/sh\nprintf '%s\\n' 'Server returned 403 Forbidden (access denied)' >&2\nexit 1\n".to_string()),
                ("refused_mid.sh", "#!/bin/sh\nprintf '%s\\n' \"\" \"   \" \"正在连接…\" \"http error 404 not found\" >&2\nexit 1\n".to_string()),
                ("unproven_err.sh", "#!/bin/sh\nprintf '%s\\n' 'Connection refused' >&2\nexit 1\n".to_string()),
                ("code1403.sh", "#!/bin/sh\nprintf '%s\\n' 'internal server error code 1403' >&2\nexit 1\n".to_string()),
                ("unicode_adjacent.sh", "#!/bin/sh\nprintf '%s\\n' '误403无法打开' >&2\nexit 1\n".to_string()),
                ("brackets403.sh", "#!/bin/sh\nprintf '%s\\n' '[403] open failed' >&2\nexit 1\n".to_string()),
                ("junk_stdout.sh", "#!/bin/sh\nprintf '%s\\n' 'not json at all'\nexit 0\n".to_string()),
                ("empty_stderr.sh", "#!/bin/sh\nexit 1\n".to_string()),
                ("blank_first.sh", "#!/bin/sh\nprintf '%s\\n' \"\" \"   \" \"Real message\" >&2\nexit 1\n".to_string()),
                ("cut_long.sh", format!("#!/bin/sh\nprintf '%s\\n' '{}' >&2\nexit 1\n", "中".repeat(201))),
                ("sleep.sh", "#!/bin/sh\nexec sleep 30\n".to_string()),
                ("vert_tab.sh", "#!/bin/sh\nprintf 'first\\vsecond\\n' >&2\nexit 1\n".to_string()),
            ];
            std::fs::create_dir_all(&dir).expect("fake ffprobe dir");
            for (name, body) in scripts {
                let path = dir.join(name);
                std::fs::write(&path, body).expect("write fake ffprobe");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod fake ffprobe");
            }
        });
        dir
    }


    #[test]
    fn header_normalization_drops_unforwardable_and_stringifies_scalars() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("nh_01", r##"null"##, "{}"),
            ("nh_02", r##"[]"##, "{}"),
            ("nh_03", r##""x""##, "{}"),
            ("nh_04", r##"42"##, "{}"),
            ("nh_05", r##"{}"##, "{}"),
            ("nh_06", r##"{"HOST": "h", "content-length": "10", "Content-Type": "video/mp4", "Connection": "close", "Proxy-Connection": "x", "Upgrade": "websocket", "TE": "trailers", "Trailer": "X", "Transfer-Encoding": "chunked", "Accept-Encoding": "gzip"}"##, "{}"),
            ("nh_07", r##"{"User-Agent": "UA"}"##, "{\"user-agent\"=\"UA\"}"),
            ("nh_08", r##"{"X-Num": 5}"##, "{\"x-num\"=\"5\"}"),
            ("nh_09", r##"{"A": true, "B": false}"##, "{\"a\"=\"True\", \"b\"=\"False\"}"),
            ("nh_10", r##"{"F": 1.5}"##, "{\"f\"=\"1.5\"}"),
            ("nh_11", r##"{"Keep": null, "Yes": "y"}"##, "{\"yes\"=\"y\"}"),
            ("nh_12", r##"{"a": "1", "A": "2", "HOST": "x"}"##, "{\"a\"=\"2\"}"),
            ("nh_13", r##"{"Referer": "http://ref", "Cookie": "c=1"}"##, "{\"cookie\"=\"c=1\", \"referer\"=\"http://ref\"}"),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(rend_map5(&normalize_headers(&json5(raw))), want, "{}", id);
        }
    }


    #[test]
    fn header_field_sorts_and_joins_with_carriage_returns() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("hf_01", r##"{}"##, "\"\""),
            ("hf_02", r##"{"B": "2", "A": "1"}"##, "\"A: 1\\r\\nB: 2\""),
            ("hf_03", r##"{"A": "2", "a": "1"}"##, "\"A: 2\\r\\na: 1\""),
            ("hf_04", r##"{"X-Key": "a b c"}"##, "\"X-Key: a b c\""),
            ("hf_05", r##"{"U": "中"}"##, "\"U: 中\""),
            ("hf_06", r##"null"##, "\"\""),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            let v = json5(raw);
            let got = match &v {
                Value::Null => header_field(None),
                other => header_field(Some(&strmap5(other))),
            };
            assert_eq!(esc5(&got), want, "{}", id);
        }
    }


    #[test]
    fn first_line_splits_on_every_python_boundary_and_clamps_at_two_hundred() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("fl_01", "", "\"\""),
            ("fl_02", "\n\n", "\"\""),
            ("fl_03", "   ", "\"\""),
            ("fl_04", "  \n\t\n  \nReal message", "\"Real message\""),
            ("fl_05", "a\r\nb", "\"a\""),
            ("fl_06", "\rb", "\"b\""),
            ("fl_07", "a\nb", "\"a\""),
            ("fl_08", "\u{000b}b", "\"b\""),
            ("fl_09", "\u{000c}b", "\"b\""),
            ("fl_10", "\u{001c}b", "\"b\""),
            ("fl_11", "\u{001f}only", "\"only\""),
            ("fl_12", "a\u{001f}b", "\"a\\u001fb\""),
            ("fl_13", "b", "\"b\""),
            ("fl_14", " b", "\"b\""),
            ("fl_15", "中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中", "\"中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中\""),
            ("fl_16", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\""),
            ("fl_17", " a b ", "\"a b\""),
            ("fl_18", "  \t\nc", "\"c\""),
            ("fl_19", "　x　", "\"x\""),
        ];
        for c in CASES {
            let (id, text, want) = *c;
            assert_eq!(esc5(&first_line(Some(text))), want, "{}", id);
        }
    }


    #[test]
    fn number_reads_python_float_semantics() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("nm_01", r##""5""##, "f:4014000000000000"),
            ("nm_02", r##"" 5 ""##, "f:4014000000000000"),
            ("nm_03", r##""5\t""##, "f:4014000000000000"),
            ("nm_04", r##""-7""##, "f:c01c000000000000"),
            ("nm_05", r##""+5""##, "f:4014000000000000"),
            ("nm_06", r##""1e3""##, "f:408f400000000000"),
            ("nm_07", r##""1E-3""##, "f:3f50624dd2f1a9fc"),
            ("nm_08", r##""0x10""##, "f:0000000000000000"),
            ("nm_09", r##""inf""##, "f:7ff0000000000000"),
            ("nm_10", r##""-Infinity""##, "f:fff0000000000000"),
            ("nm_11", r##""nan""##, "f:7ff8000000000000"),
            ("nm_12", r##""NaN""##, "f:7ff8000000000000"),
            ("nm_13", r##""infinity""##, "f:7ff0000000000000"),
            ("nm_14", r##""abc""##, "f:0000000000000000"),
            ("nm_15", r##""""##, "f:0000000000000000"),
            ("nm_16", r##"" ""##, "f:0000000000000000"),
            ("nm_17", r##""4.""##, "f:4010000000000000"),
            ("nm_18", r##"".5""##, "f:3fe0000000000000"),
            ("nm_19", r##""- .5""##, "f:0000000000000000"),
            ("nm_20", r##""1e400""##, "f:7ff0000000000000"),
            ("nm_21", r##"null"##, "f:0000000000000000"),
            ("nm_22", r##"[]"##, "f:0000000000000000"),
            ("nm_23", r##"{}"##, "f:0000000000000000"),
            ("nm_24", r##"true"##, "f:3ff0000000000000"),
            ("nm_25", r##"false"##, "f:0000000000000000"),
            ("nm_26", r##"10"##, "f:4024000000000000"),
            ("nm_27", r##"2.5"##, "f:4004000000000000"),
            ("nm_28", r##""3 4""##, "f:0000000000000000"),
            ("nm_29", r##""-""##, "f:0000000000000000"),
            ("nm_30", r##""e5""##, "f:0000000000000000"),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(format!("f:{:016x}", number(&json5(raw)).to_bits()), want, "{}", id);
        }
    }


    #[test]
    fn track_kind_reads_the_codec_triplet() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("tk_01", r##"{"vcodec": "h264", "acodec": "aac"}"##, "\"\""),
            ("tk_02", r##"{"vcodec": "avc1", "acodec": "none"}"##, "\"video\""),
            ("tk_03", r##"{"vcodec": "none", "acodec": "aac"}"##, "\"audio\""),
            ("tk_04", r##"{"vcodec": "none", "acodec": "none"}"##, "\"\""),
            ("tk_05", r##"{}"##, "\"\""),
            ("tk_06", r##"{"vcodec": "", "acodec": ""}"##, "\"\""),
            ("tk_07", r##"{"vcodec": "", "acodec": "none"}"##, "\"video\""),
            ("tk_08", r##"{"vcodec": "none", "acodec": ""}"##, "\"audio\""),
            ("tk_09", r##"{"vcodec": null, "acodec": "aac"}"##, "\"\""),
            ("tk_10", r##"{"vcodec": "h264"}"##, "\"\""),
            ("tk_11", r##"42"##, "\"\""),
            ("tk_12", r##"null"##, "\"\""),
            ("tk_13", r##"{"vcodec": "H264", "acodec": "NONE"}"##, "\"\""),
            ("tk_14", r##"{"vcodec": "none ", "acodec": "aac"}"##, "\"\""),
            ("tk_15", r##"{"vcodec": "NONE", "acodec": "none"}"##, "\"video\""),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(esc5(track_kind(&json5(raw))), want, "{}", id);
        }
    }


    #[test]
    fn engine_audio_codec_trims_and_lowercases() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("ea_01", r##"{"acodec": "aac"}"##, "\"aac\""),
            ("ea_02", r##"{"acodec": "AAC"}"##, "\"aac\""),
            ("ea_03", r##"{"acodec": " aac "}"##, "\"aac\""),
            ("ea_04", r##"{"acodec": "none"}"##, "\"\""),
            ("ea_05", r##"{"acodec": "NONE"}"##, "\"\""),
            ("ea_06", r##"{"acodec": " none "}"##, "\"\""),
            ("ea_07", r##"{"acodec": "\u001cnone"}"##, "\"\""),
            ("ea_08", r##"{"acodec": "\u001fnone"}"##, "\"\""),
            ("ea_09", r##"{}"##, "\"\""),
            ("ea_10", r##"42"##, "\"\""),
            ("ea_11", r##"{"acodec": "mp4a.40.2"}"##, "\"mp4a.40.2\""),
            ("ea_12", r##"{"acodec": "  "}"##, "\"\""),
            ("ea_13", r##"{"acodec": null}"##, "\"\""),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(esc5(&engine_audio_codec(&json5(raw))), want, "{}", id);
        }
    }


    #[test]
    fn track_bitrate_prefers_abr_then_tbr_then_br() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("tb_01", r##"{"abr": 128}"##, "f:4060000000000000"),
            ("tb_02", r##"{"tbr": 128.5}"##, "f:4060100000000000"),
            ("tb_03", r##"{"br": 64}"##, "f:4050000000000000"),
            ("tb_04", r##"{"abr": "0", "tbr": "500"}"##, "f:407f400000000000"),
            ("tb_05", r##"{"abr": "0"}"##, "f:0000000000000000"),
            ("tb_06", r##"{"abr": null}"##, "f:0000000000000000"),
            ("tb_07", r##"{"abr": "abc"}"##, "f:0000000000000000"),
            ("tb_08", r##"{"abr": "-5"}"##, "f:0000000000000000"),
            ("tb_09", r##"{"abr": true}"##, "f:3ff0000000000000"),
            ("tb_10", r##"{"abr": " 128 "}"##, "f:4060000000000000"),
            ("tb_11", r##"{"abr": "1e3"}"##, "f:408f400000000000"),
            ("tb_12", r##"{}"##, "f:0000000000000000"),
            ("tb_13", r##"{"br": "7"}"##, "f:401c000000000000"),
            ("tb_14", r##"{"tbr": {}}"##, "f:0000000000000000"),
            ("tb_15", r##"{"abr": "0", "tbr": {}}"##, "f:0000000000000000"),
            ("tb_16", r##"{"abr": "0", "tbr": "0", "br": "0"}"##, "f:0000000000000000"),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(format!("f:{:016x}", track_bitrate(&json5(raw)).to_bits()), want, "{}", id);
        }
    }


    #[test]
    fn format_label_stacks_the_pieces_the_way_the_page_shows_them() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("fmtl_01", r##"{"ext": "mp4"}"##, "\"mp4\""),
            ("fmtl_02", r##"{"height": 720}"##, "\"720p\""),
            ("fmtl_03", r##"{"height": "720.5"}"##, "\"720.5p\""),
            ("fmtl_04", r##"{"vcodec": "avc1.640028"}"##, "\"H.264\""),
            ("fmtl_05", r##"{"vcodec": "avc"}"##, "\"H.264\""),
            ("fmtl_06", r##"{"vcodec": "h264"}"##, "\"H.264\""),
            ("fmtl_07", r##"{"vcodec": "hev1.1.6"}"##, "\"H.265\""),
            ("fmtl_08", r##"{"vcodec": "hvc1.1.6"}"##, "\"H.265\""),
            ("fmtl_09", r##"{"vcodec": "hevc"}"##, "\"H.265\""),
            ("fmtl_10", r##"{"vcodec": "av01.0.01M.08"}"##, "\"AV1\""),
            ("fmtl_11", r##"{"vcodec": "vp9"}"##, "\"VP9\""),
            ("fmtl_12", r##"{"vcodec": "vp09.00.10.08"}"##, "\"VP9\""),
            ("fmtl_13", r##"{"vcodec": "vp8"}"##, "\"VP8\""),
            ("fmtl_14", r##"{"vcodec": " none "}"##, "\" NONE \""),
            ("fmtl_15", r##"{"vcodec": "None"}"##, "\"NONE\""),
            ("fmtl_16", r##"{"vcodec": "nonexistent"}"##, "\"NONEXISTENT\""),
            ("fmtl_17", r##"{"vcodec": ".H264"}"##, "\"\""),
            ("fmtl_18", r##"{}"##, "\"\""),
            ("fmtl_19", r##"{"format_note": "720p60"}"##, "\"720p60\""),
            ("fmtl_20", r##"{"tbr": 240.7}"##, "\"240 k\""),
            ("fmtl_21", r##"{"tbr": 0}"##, "\"\""),
            ("fmtl_22", r##"{"tbr": -5}"##, "\"-5 k\""),
            ("fmtl_23", r##"{"ext": "mp4", "height": 1080, "vcodec": "avc1.640028", "format_note": "1080p", "tbr": 4500.3}"##, "\"mp4 1080p H.264 1080p 4500 k\""),
            ("fmtl_24", r##"{"height": "none"}"##, "\"nonep\""),
            ("fmtl_25", r##"{"vcodec": "AVC1"}"##, "\"H.264\""),
            ("fmtl_26", r##"{"vcodec": "-hevc"}"##, "\"\""),
            ("fmtl_27", r##"{"vcodec": "hev1.90"}"##, "\"H.265\""),
            ("fmtl_28", r##"{"vcodec": "H264"}"##, "\"H.264\""),
            ("fmtl_29", r##"{"ext": "webm", "vcodec": "none"}"##, "\"webm\""),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            assert_eq!(esc5(&format_label(&json5(raw))), want, "{}", id);
        }
    }


    #[test]
    fn page_wording_strips_the_entry_suffix_only_when_the_page_agrees() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str, &str)] = &[
            ("pw_01", "x", "", "\"x\""),
            ("pw_02", "", "p", "\"p\""),
            ("pw_03", "", "", "\"\""),
            ("pw_04", "t", "t", "\"t\""),
            ("pw_05", "Watch (1)", "Watch", "\"Watch\""),
            ("pw_06", "Watch (12)", "Watch", "\"Watch\""),
            ("pw_07", "Watch (123)", "Watch", "\"Watch (123)\""),
            ("pw_08", "Watch (02)", "Watch", "\"Watch\""),
            ("pw_09", "Watch (٣)", "Watch", "\"Watch\""),
            ("pw_10", "Watch (1", "Watch", "\"Watch (1\""),
            ("pw_11", "Watchx (1)", "Watch", "\"Watchx (1)\""),
            ("pw_12", "Watch (1) extra", "Watch", "\"Watch (1) extra\""),
            ("pw_13", "lower", "LOWER", "\"lower\""),
            ("pw_14", "Watch", "Watch extra", "\"Watch\""),
            ("pw_15", "Watch (999)", "Watch", "\"Watch (999)\""),
        ];
        for c in CASES {
            let (id, title, page_title, want) = *c;
            assert_eq!(esc5(&page_wording(title, page_title)), want, "{}", id);
        }
    }


    #[test]
    fn probe_json_parsing_hands_back_exactly_what_ffprobe_meant() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("pj_01", "{\"format\": {\"format_name\": \"mov,mp4\", \"duration\": \"212.288000\", \"bit_rate\": \"2833943\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 1920, \"height\": 1080, \"bit_rate\": \"2600000\"}, {\"codec_type\": \"audio\", \"codec_name\": \"aac\"}]}", "[reach:\"unproven\" container:\"mov,mp4\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:false moving_picture:true]"),
            ("pj_02", "{\"format\": {\"format_name\": \"mpegts\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 1280, \"height\": 720}]}", "[reach:\"unproven\" container:\"mpegts\" video_codec:\"h264\" audio_codec:\"\" width:i:1280 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_03", "{\"format\": {\"format_name\": \"image2\", \"duration\": \"0.000000\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"mjpeg\", \"width\": 800, \"height\": 600}]}", "[reach:\"unproven\" container:\"image2\" video_codec:\"mjpeg\" audio_codec:\"\" width:i:800 height:i:600 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_04", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"-1.5\"}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:bff8000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pj_05", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"-0\"}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:8000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_06", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"0.0\"}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_07", "{\"format\": {\"format_name\": \"mp4\", \"duration\": null}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_08", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 1920, \"height\": 1080}, {\"codec_type\": \"video\", \"codec_name\": \"hevc\", \"width\": 640, \"height\": 360}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:1920 height:i:1080 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_09", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [42, \"x\", {\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": 320}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:320 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_10", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"5\"}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:4014000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pj_11", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"height\": 480}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:480 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_12", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"width\": \"1920.9\", \"height\": \"1080.9\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:1920 height:i:1080 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_13", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"video\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_14", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"Video\", \"codec_name\": \"h264\", \"width\": 1920}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_15", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"100\"}, \"streams\": [{\"codec_type\": \"audio\", \"codec_name\": \"aac\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"aac\" width:i:0 height:i:0 duration:f:4059000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pj_16", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"audio\", \"codec_name\": \"opus\"}, {\"codec_type\": \"video\", \"codec_name\": \"av1\", \"width\": 1280, \"height\": 720}, {\"codec_type\": \"audio\", \"codec_name\": \"aac\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"av1\" audio_codec:\"opus\" width:i:1280 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:true]"),
            ("pj_17", "{\"format\": {\"format_name\": 5}}", "[reach:\"unproven\" container:\"5\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_18", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"10\", \"bit_rate\": \"0\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"bit_rate\": \"5000\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:4024000000000000 bitrate:f:40b3880000000000 is_live:false error:\"\" measured:false moving_picture:true]"),
            ("pj_19", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"10\", \"bit_rate\": \"1000\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"bit_rate\": \"5000\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:4024000000000000 bitrate:f:408f400000000000 is_live:false error:\"\" measured:false moving_picture:true]"),
            ("pj_20", "{\"format\": {\"format_name\": \"mp4\", \"duration\": \"10\", \"bit_rate\": \"\"}, \"streams\": [{\"codec_type\": \"video\", \"codec_name\": \"h264\", \"bit_rate\": \"5000\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:4024000000000000 bitrate:f:40b3880000000000 is_live:false error:\"\" measured:false moving_picture:true]"),
            ("pj_21", "not json", "null"),
            ("pj_22", "[1, 2]", "null"),
            ("pj_23", "null", "null"),
            ("pj_24", "42", "null"),
            ("pj_25", "{}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_26", "{\"format\": {\"format_name\": \"mp4\"}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_27", "{\"format\": null}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_28", "{\"format\": 0}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_29", "{\"format\": {\"format_name\": \"mp4\", \"duration\": 212.288}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:406a89374bc6a7f0 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pj_30", "{\"format\": {\"format_name\": \"mp4\", \"bit_rate\": 2833943}}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:41459f0b80000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_31", "{\"format\": {\"format_name\": \"mp4\"}, \"streams\": [{\"codec_type\": \"audio\", \"codec_name\": \"aac\"}]}", "[reach:\"unproven\" container:\"mp4\" video_codec:\"\" audio_codec:\"aac\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_32", "{\"format\": {\"duration\": \"5\"}}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:4014000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pj_33", "{\"format\": {\"format_name\": \"\"}}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
            ("pj_34", "{\"format\": {\"format_name\": 0}}", "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:false moving_picture:false]"),
        ];
        for c in CASES {
            let (id, text, want) = *c;
            let got = match parse_probe_json(text) {
                Some(p) => rend_probe5(&p),
                None => "null".to_string(),
            };
            assert_eq!(got, want, "{}", id);
        }
    }


    #[test]
    fn ytdlp_json_becomes_candidates_and_never_claims_a_probe() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("py_01", "not json", "[]"),
            ("py_02", "[]", "[]"),
            ("py_03", "null", "[]"),
            ("py_04", "42", "[]"),
            ("py_05", "{\"title\": \"T\", \"http_headers\": {\"User-Agent\": \"UA\", \"Referer\": \"shared\"}, \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"ext\": \"mp4\", \"height\": 720, \"vcodec\": \"avc1.640028\", \"acodec\": \"aac\", \"tbr\": 2500, \"http_headers\": {\"Referer\": \"http://ref\"}}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{\\\"referer\\\"=\\\"http://ref\\\"} label:\\\"mp4 720p H.264 2500 k\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"aac\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_06", "{\"_type\": \"playlist\", \"entries\": [{\"title\": \"A\", \"formats\": [{\"url\": \"https://p/a1\"}]}, {\"title\": \"B\", \"formats\": [{\"url\": \"https://p/b1\"}, {\"url\": \"https://p/b2\"}]}, {\"title\": \"C\", \"formats\": [{\"url\": \"https://p/c1\"}]}, {\"title\": \"D\", \"formats\": [{\"url\": \"https://p/d1\"}, {\"url\": \"https://p/d2\"}, {\"url\": \"https://p/d3\"}]}]}", "[\"[url:\\\"https://p/d1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"D\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/d2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"D\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/d3\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"D\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/b1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"B\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/b2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"B\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/a1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"A\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_07", "{\"title\": \"第三集 · 夏日回响\", \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"title\": \"第三集 · 夏日回响 (2)\"}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"第三集 · 夏日回响\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_08", "{\"formats\": [{\"url\": \"https://cdn/1.mp4\", \"title\": \"FromForm\"}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"FromForm\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_09", "{\"title\": \"  \", \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"title\": \"FromForm\"}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"FromForm\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_10", "{\"title\": \"\", \"formats\": [{\"url\": \"https://cdn/1.mp4\"}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_11", "{\"title\": 42, \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"title\": 43}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_12", "{\"title\": \"T\", \"formats\": [{\"url\": \"ftp://x/1\"}, {\"url\": \"HTTP://X/2\"}, {\"url\": \"https://y/3\"}]}", "[\"[url:\\\"HTTP://X/2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://y/3\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_13", "{\"title\": \"T\", \"formats\": [{\"url\": 42}, {\"url\": \"https://y/3\"}]}", "[\"[url:\\\"https://y/3\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_14", "{\"title\": \"T\", \"http_headers\": {\"User-Agent\": \"UA\", \"Cookie\": \"c=1\", \"Host\": \"h\"}, \"formats\": [{\"url\": \"https://cdn/1.mp4\"}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{\\\"cookie\\\"=\\\"c=1\\\", \\\"user-agent\\\"=\\\"UA\\\"} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_15", "{\"title\": \"T\", \"http_headers\": {\"User-Agent\": \"shared-ua\"}, \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"http_headers\": {\"Referer\": \"r\"}}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{\\\"referer\\\"=\\\"r\\\"} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_16", "{\"title\": \"T\", \"http_headers\": {\"User-Agent\": \"shared-ua\"}, \"formats\": [{\"url\": \"https://cdn/1.mp4\", \"http_headers\": {}}]}", "[\"[url:\\\"https://cdn/1.mp4\\\" origin:\\\"ytdlp\\\" headers:{\\\"user-agent\\\"=\\\"shared-ua\\\"} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_17", "{\"title\": \"T\", \"formats\": [{\"url\": \"https://cdn/both.mp4\", \"ext\": \"mp4\", \"height\": 720, \"vcodec\": \"h264\", \"acodec\": \"aac\", \"tbr\": 2500}, {\"url\": \"https://cdn/v.mp4\", \"ext\": \"mp4\", \"height\": 1080, \"vcodec\": \"avc1\", \"acodec\": \"none\", \"tbr\": 6000}, {\"url\": \"https://cdn/a.m4a\", \"ext\": \"m4a\", \"vcodec\": \"none\", \"acodec\": \"mp4a.40.2\", \"abr\": 192}]}", "[\"[url:\\\"https://cdn/both.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264 2500 k\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"aac\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://cdn/v.mp4\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 1080p H.264 6000 k\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"https://cdn/a.m4a\\\" audio_headers:{} audio_label:\\\"m4a\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://cdn/a.m4a\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"m4a\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_18", "{\"_type\": \"playlist\", \"entries\": [42, \"x\", {\"title\": \"T\", \"formats\": [{\"url\": \"https://p/only\"}]}]}", "[\"[url:\\\"https://p/only\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_19", "{\"title\": \"T\", \"formats\": [\"xy\", 42, {\"url\": \"https://p/keep\"}]}", "[\"[url:\\\"https://p/keep\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_20", "{\"_type\": \"playlist\", \"entries\": [{\"title\": \"One\", \"formats\": [{\"url\": \"https://z/1\", \"title\": \"one\"}]}, {\"title\": \"Two\", \"formats\": [{\"url\": \"https://z/1\", \"title\": \"two\"}]}]}", "[\"[url:\\\"https://z/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"One\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_21", "{\"title\": \"T\", \"formats\": [{\"url\": \"https://a/1.m4a\", \"ext\": \"m4a\", \"vcodec\": \"none\", \"acodec\": \"aac\", \"abr\": 128}]}", "[\"[url:\\\"https://a/1.m4a\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"m4a\\\" title:\\\"T\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"aac\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("py_22", "{\"_type\": \"playlist\", \"entries\": []}", "[]"),
            ("py_23", "{\"_type\": \"playlist\"}", "[]"),
            ("py_24", "{\"_type\": \"playlist\", \"entries\": [{\"title\": \"A\", \"formats\": [{\"url\": \"https://p/A\"}]}, {\"title\": \"B\", \"formats\": [{\"url\": \"https://p/B\"}]}]}", "[\"[url:\\\"https://p/A\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"A\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"https://p/B\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"B\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
        ];
        for c in CASES {
            let (id, text, want) = *c;
            let rows: Vec<String> = parse_ytdlp_json(text).into_iter().map(|c| rend_row5(&c, true)).collect();
            assert_eq!(rend_rows5(&rows), want, "{}", id);
        }
    }


    #[test]
    fn candidate_rendering_keeps_every_field_a_viewer_would_read() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("cd_01", r##"{"url": "http://x/1", "origin": "page"}"##, "[url:\"http://x/1\" origin:\"page\" headers:{} label:\"\" title:\"\" probe:null probe_null:true last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_02", r##"{"url": "http://x/1", "origin": "ytdlp", "headers": {"B": "2", "A": "1", "a": "3"}}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{\"A\"=\"1\", \"B\"=\"2\", \"a\"=\"3\"} label:\"\" title:\"\" probe:null probe_null:true last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_03", r##"{"url": "http://x/1", "origin": "ytdlp", "audio_url": "http://a/1", "audio_headers": {"Referer": "r"}, "audio_codec": "mp4a.40.2", "audio_label": "m4a 192 k"}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:null probe_null:true last_resort:false reason:\"\" audio_codec:\"mp4a.40.2\" audio_url:\"http://a/1\" audio_headers:{\"Referer\"=\"r\"} audio_label:\"m4a 192 k\" silent:false height:i:0 segmented:false]"),
            ("cd_04", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"mov,mp4\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:1080 segmented:false]"),
            ("cd_05", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "video_codec": "h264"}}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:true height:i:0 segmented:false]"),
            ("cd_06", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "video_codec": "h264"}, "audio_url": "http://a/1"}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"http://a/1\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_07", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "video_codec": "h264"}, "audio_url": ""}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:true height:i:0 segmented:false]"),
            ("cd_08", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "video_codec": "", "audio_codec": "aac"}}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"\" video_codec:\"\" audio_codec:\"aac\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:true moving_picture:false] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_09", r##"{"url": "http://x/1", "origin": "ytdlp", "probe": {"reach": "opened", "video_codec": "h264", "audio_codec": ""}}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"\" probe:[reach:\"opened\" container:\"\" video_codec:\"h264\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:true height:i:0 segmented:false]"),
            ("cd_10", r##"{"url": "http://x/1", "origin": "page", "last_resort": true, "reason": "unmeasurable, offered as a last resort"}"##, "[url:\"http://x/1\" origin:\"page\" headers:{} label:\"\" title:\"\" probe:null probe_null:true last_resort:true reason:\"unmeasurable, offered as a last resort\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_11", r##"{"url": "http://x/1", "origin": "ytdlp", "title": "夏日回响"}"##, "[url:\"http://x/1\" origin:\"ytdlp\" headers:{} label:\"\" title:\"夏日回响\" probe:null probe_null:true last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
            ("cd_12", r##"{"url": "http://x/live.m3u8", "origin": "page"}"##, "[url:\"http://x/live.m3u8\" origin:\"page\" headers:{} label:\"\" title:\"\" probe:null probe_null:true last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:true]"),
            ("cd_13", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "unproven", "error": ""}}"##, "[url:\"http://x/1\" origin:\"page\" headers:{} label:\"\" title:\"\" probe:[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false] probe_null:false last_resort:false reason:\"\" audio_codec:\"\" audio_url:\"\" audio_headers:{} audio_label:\"\" silent:false height:i:0 segmented:false]"),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            let (c, probe_null) = cand5(&json5(raw));
            assert_eq!(rend_row5(&c, probe_null), want, "{}", id);
        }
    }


    #[test]
    fn admission_decides_in_the_first_matching_row() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str)] = &[
            ("ad_01", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "refused", "error": "Server returned 403 Forbidden"}}"##, "(false,false,\"refused by the origin\")"),
            ("ad_02", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "refused"}}"##, "(false,false,\"refused by the origin\")"),
            ("ad_03", r##"{"url": "http://x/1", "origin": "page"}"##, "(true,true,\"unmeasurable, offered as a last resort\")"),
            ("ad_04", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "unproven", "error": "probe timed out"}}"##, "(true,true,\"unmeasurable, offered as a last resort\")"),
            ("ad_05", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "image2", "video_codec": "mjpeg", "audio_codec": "aac"}}"##, "(true,false,\"carried sound but no picture, kept for an audio cast\")"),
            ("ad_06", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "audio_codec": "aac"}}"##, "(true,false,\"carried sound but no picture, kept for an audio cast\")"),
            ("ad_07", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "image2", "video_codec": "mjpeg"}}"##, "(false,false,\"carried no moving picture\")"),
            ("ad_08", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened"}}"##, "(false,false,\"carried no moving picture\")"),
            ("ad_09", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp,3g2,mj2", "video_codec": "h264"}}"##, "(true,true,\"an MP4 header with no timeline of its own, offered as a last resort\")"),
            ("ad_10", r##"{"url": "http://x/1.m3u8", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp,3g2,mj2", "video_codec": "h264"}}"##, "(true,false,\"carried a castable program\")"),
            ("ad_11", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "duration": 10.0}}"##, "(true,false,\"carried a castable program\")"),
            ("ad_12", r##"{"url": "http://x/1.mkv", "origin": "page", "probe": {"reach": "opened", "container": "matroska,webm", "video_codec": "h264", "duration": 0.0}}"##, "(true,false,\"carried a castable program\")"),
            ("ad_13", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mp4", "video_codec": "h264", "duration": -0.0}}"##, "(true,true,\"an MP4 header with no timeline of its own, offered as a last resort\")"),
            ("ad_14", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##, "(true,false,\"carried a castable program\")"),
            ("ad_15", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "unproven", "video_codec": "h264", "audio_codec": "aac"}}"##, "(true,true,\"unmeasurable, offered as a last resort\")"),
            ("ad_16", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "MP4", "video_codec": "h264", "duration": 0.0}}"##, "(true,false,\"carried a castable program\")"),
        ];
        for c in CASES {
            let (id, raw, want) = *c;
            let c = cand_half(&json5(raw));
            let (keep, last, reason) = admit(&c);
            assert_eq!(format!("({},{},{})", keep, last, esc5(reason)), want, "{}", id);
        }
    }


    #[test]
    fn admission_rows_keep_their_order_and_predicates() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, usize, &str)] = &[
            ("ar_00", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "refused", "error": "Server returned 403 Forbidden"}}"##, 0, "(true,false,\"refused by the origin\")"),
            ("ar_01", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##, 0, "(false,false,\"refused by the origin\")"),
            ("ar_02", r##"{"url": "http://x/1", "origin": "page"}"##, 1, "(true,true,\"unmeasurable, offered as a last resort\")"),
            ("ar_03", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##, 1, "(false,true,\"unmeasurable, offered as a last resort\")"),
            ("ar_04", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "image2", "video_codec": "mjpeg", "audio_codec": "aac"}}"##, 2, "(true,true,\"carried sound but no picture, kept for an audio cast\")"),
            ("ar_05", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "image2", "video_codec": "mjpeg"}}"##, 3, "(true,false,\"carried no moving picture\")"),
            ("ar_06", r##"{"url": "http://x/1.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp,3g2,mj2", "video_codec": "h264"}}"##, 4, "(true,true,\"an MP4 header with no timeline of its own, offered as a last resort\")"),
            ("ar_07", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##, 5, "(true,true,\"carried a castable program\")"),
        ];
        for c in CASES {
            let (id, raw, row, want) = *c;
            let c = cand_half(&json5(raw));
            let (reason, keep, _last, pred) = ADMISSION_ROWS[row];
            assert_eq!(format!("({},{},{})", pred(&c), keep, esc5(reason)), want, "{}", id);
        }
    }


    #[test]
    fn exceeds_cap_measures_the_bitrate_ceiling() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, f64, &str)] = &[
            ("ec_01", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, 0.0, "false"),
            ("ec_02", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, -5.0, "false"),
            ("ec_03", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}, "last_resort": true}"##, 720.0, "false"),
            ("ec_04", r##"{"url": "http://x/live.m3u8", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}}"##, 720.0, "false"),
            ("ec_05", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, 720.0, "true"),
            ("ec_06", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 720.0, "false"),
            ("ec_07", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 480}}"##, 720.0, "false"),
            ("ec_08", r##"{"url": "http://x/1", "origin": "page"}"##, 720.0, "false"),
        ];
        for c in CASES {
            let (id, raw, ceiling, want) = *c;
            let c = cand_half(&json5(raw));
            assert_eq!(format!("{}", exceeds_cap(&c, ceiling)), want, "{}", id);
        }
    }


    #[test]
    fn preference_ranks_direct_links_before_playlists() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str, f64, &str)] = &[
            ("pf_01", r##"{"url": "http://x/1", "origin": "page", "last_resort": true}"##, r##"{"url": "http://x/2", "origin": "page"}"##, 0.0, "i:1"),
            ("pf_02", r##"{"url": "http://x/1", "origin": "page"}"##, r##"{"url": "http://x/2", "origin": "page", "last_resort": true}"##, 0.0, "i:-1"),
            ("pf_03", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 720.0, "i:1"),
            ("pf_04", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}}"##, 720.0, "i:-1"),
            ("pf_05", r##"{"url": "http://x/live.m3u8", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, r##"{"url": "http://x/2.mp4", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 480}}"##, 0.0, "i:1"),
            ("pf_06", r##"{"url": "http://x/2.mp4", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 480}}"##, r##"{"url": "http://x/live.m3u8", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, 0.0, "i:-1"),
            ("pf_07", r##"{"url": "http://x/1", "origin": "page"}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 0.0, "i:1"),
            ("pf_08", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, r##"{"url": "http://x/2", "origin": "page"}"##, 0.0, "i:-1"),
            ("pf_09", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 0.0, "i:-1"),
            ("pf_10", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 480}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, 0.0, "i:1"),
            ("pf_11", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720, "bitrate": 5000.0}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720, "bitrate": 1000.0}}"##, 0.0, "i:-1"),
            ("pf_12", r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720, "bitrate": 1000.0}}"##, r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720, "bitrate": 5000.0}}"##, 0.0, "i:1"),
            ("pf_13", r##"{"url": "http://a/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, r##"{"url": "http://b/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 0.0, "i:-1"),
            ("pf_14", r##"{"url": "http://a/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, r##"{"url": "http://a/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##, 0.0, "i:0"),
            ("pf_15", r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}}"##, r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##, 0.0, "i:-1"),
        ];
        for c in CASES {
            let (id, a_raw, b_raw, ceiling, want) = *c;
            let a = cand_half(&json5(a_raw));
            let b = cand_half(&json5(b_raw));
            assert_eq!(format!("i:{}", preference(&a, &b, ceiling)), want, "{}", id);
        }
    }


    #[test]
    fn rank_orders_by_preference_under_the_ceiling() {
        crate::pin_test_dir();
        const CASES: &[(&str, f64, &[&str], &str)] = &[
            ("rk_01", 0.0, &[

            ], "[]"),
            ("rk_02", 0.0, &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "refused", "error": "Server returned 403 Forbidden"}}"##,
                r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##,
            ], "[\"[url:\\\"http://x/2\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"carried a castable program\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:720 segmented:false]\"]"),
            ("rk_03", 720.0, &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}"##,
                r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}"##,
                r##"{"url": "http://x/3", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "height": 2160}}"##,
            ], "[\"[url:\\\"http://x/2\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"carried a castable program\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:720 segmented:false]\", \"[url:\\\"http://x/3\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:2160 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"carried a castable program\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:2160 segmented:false]\", \"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:1080 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"carried a castable program\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:1080 segmented:false]\"]"),
            ("rk_04", 0.0, &[
                r##"{"url": "http://x/hdr.mp4", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp,3g2,mj2", "video_codec": "h264"}}"##,
                r##"{"url": "http://x/ok.mkv", "origin": "page", "probe": {"reach": "opened", "container": "matroska,webm", "video_codec": "h264", "height": 720, "duration": 200.0}}"##,
            ], "[\"[url:\\\"http://x/ok.mkv\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"matroska,webm\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:720 duration:f:4069000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"carried a castable program\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:720 segmented:false]\", \"[url:\\\"http://x/hdr.mp4\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"mov,mp4,m4a,3gp,3g2,mj2\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:true reason:\\\"an MP4 header with no timeline of its own, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:0 segmented:false]\"]"),
            ("rk_05", 0.0, &[
                r##"{"url": "http://x/L1", "origin": "page"}"##,
                r##"{"url": "http://x/L2", "origin": "page"}"##,
            ], "[\"[url:\\\"http://x/L1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:true reason:\\\"unmeasurable, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/L2\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:true reason:\\\"unmeasurable, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("rk_06", 0.0, &[
                r##"{"url": "http://x/live.m3u8", "origin": "page"}"##,
                r##"{"url": "http://x/plain.mp4", "origin": "page"}"##,
            ], "[\"[url:\\\"http://x/plain.mp4\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:true reason:\\\"unmeasurable, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/live.m3u8\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:true reason:\\\"unmeasurable, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:true]\"]"),
            ("rk_07", 0.0, &[
                r##"{"url": "http://x/u", "origin": "page", "probe": {"reach": "unproven", "error": "probe timed out"}}"##,
                r##"{"url": "http://x/a", "origin": "page", "probe": {"reach": "opened", "audio_codec": "aac"}}"##,
            ], "[\"[url:\\\"http://x/a\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"\\\" audio_codec:\\\"aac\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:false] probe_null:false last_resort:false reason:\\\"carried sound but no picture, kept for an audio cast\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/u\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"unproven\\\" container:\\\"\\\" video_codec:\\\"\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"probe timed out\\\" measured:false moving_picture:false] probe_null:false last_resort:true reason:\\\"unmeasurable, offered as a last resort\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
        ];
        for c in CASES {
            let (id, ceiling, inputs, want) = *c;
            let mut pn: HashMap<String, bool> = HashMap::new();
            let mut cands = Vec::new();
            for raw in inputs {
                let (cand, probe_null) = cand5(&json5(raw));
                pn.insert(cand.url.clone(), probe_null);
                cands.push(cand);
            }
            let rows: Vec<String> = rank(cands, ceiling)
                .into_iter()
                .map(|c| rend_row5(&c, *pn.get(&c.url).expect("ranked row keeps its url")))
                .collect();
            assert_eq!(rend_rows5(&rows), want, "{}", id);
        }
    }


    #[test]
    fn visible_rows_separates_the_silent_half() {
        crate::pin_test_dir();
        const CASES: &[(&str, &[&str], &str)] = &[
            ("vs_01", &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264"}}"##,
                r##"{"url": "http://x/2", "origin": "page", "probe": {"reach": "opened", "container": "mov,mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.288, "bitrate": 2833943.0}}"##,
            ], "([\"[url:\\\"http://x/2\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"mov,mp4\\\" video_codec:\\\"h264\\\" audio_codec:\\\"aac\\\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:1080 segmented:false]\"], [\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:0 segmented:false]\"])"),
            ("vs_02", &[
                r##"{"url": "http://x/1", "origin": "page"}"##,
            ], "([\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"], [])"),
            ("vs_03", &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264"}, "audio_url": "http://a/1"}"##,
            ], "([\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"http://a/1\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"], [])"),
            ("vs_04", &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "h264", "audio_codec": ""}}"##,
            ], "([], [\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:true height:i:0 segmented:false]\"])"),
            ("vs_05", &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened", "video_codec": "", "audio_codec": "aac"}}"##,
            ], "([\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"\\\" audio_codec:\\\"aac\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:false] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"], [])"),
            ("vs_06", &[
                r##"{"url": "http://x/1", "origin": "page", "probe": {"reach": "opened"}}"##,
            ], "([\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"\\\" audio_codec:\\\"\\\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:false] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"], [])"),
        ];
        for c in CASES {
            let (id, inputs, want) = *c;
            let mut pn: HashMap<String, bool> = HashMap::new();
            let mut cands = Vec::new();
            for raw in inputs {
                let (cand, probe_null) = cand5(&json5(raw));
                pn.insert(cand.url.clone(), probe_null);
                cands.push(cand);
            }
            let (kept, hidden) = visible_rows(&cands);
            let kept_rows: Vec<String> = kept.iter().map(|c| rend_row5(c, *pn.get(&c.url).expect("kept row keeps its url"))).collect();
            let hidden_rows: Vec<String> = hidden.iter().map(|c| rend_row5(c, *pn.get(&c.url).expect("hidden row keeps its url"))).collect();
            assert_eq!(rend_tuple5(&kept_rows, &hidden_rows), want, "{}", id);
        }
    }


    #[test]
    fn dedupe_keeps_the_first_shape_of_each_url() {
        crate::pin_test_dir();
        const CASES: &[(&str, &[&str], &str)] = &[
            ("dd_01", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "Only"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"Only\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_02", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "P"}"##,
                r##"{"url": "http://x/1", "origin": "ytdlp", "title": "Watch (1)"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"Watch (1)\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_03", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "P"}"##,
                r##"{"url": "http://x/2", "origin": "ytdlp", "title": "Watch"}"##,
                r##"{"url": "http://x/1", "origin": "page", "title": "Last"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"P\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"Watch\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_04", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "First"}"##,
                r##"{"url": "http://x/1", "origin": "page", "title": "Second"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"First\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_05", &[
                r##"{"url": "http://x/1", "origin": "ytdlp", "title": "Engine"}"##,
                r##"{"url": "http://x/1", "origin": "page", "title": "Page"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"Engine\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_06", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "P1"}"##,
                r##"{"url": "http://x/2", "origin": "page", "title": "other"}"##,
                r##"{"url": "http://x/1", "origin": "ytdlp", "title": "Watch (1)"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"Watch (1)\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/2\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"other\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_07", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "A"}"##,
                r##"{"url": "http://x/2", "origin": "ytdlp", "title": "B"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"page\\\" headers:{} label:\\\"\\\" title:\\\"A\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://x/2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"B\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("dd_08", &[

            ], "[]"),
            ("dd_09", &[
                r##"{"url": "http://x/1", "origin": "page", "title": "Watch"}"##,
                r##"{"url": "http://x/1", "origin": "ytdlp", "title": "Watch (1)"}"##,
            ], "[\"[url:\\\"http://x/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"\\\" title:\\\"Watch\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
        ];
        for c in CASES {
            let (id, inputs, want) = *c;
            let mut pn: HashMap<String, bool> = HashMap::new();
            let mut cands = Vec::new();
            for raw in inputs {
                let (cand, probe_null) = cand5(&json5(raw));
                pn.insert(cand.url.clone(), probe_null);
                cands.push(cand);
            }
            let rows: Vec<String> = dedupe(cands).into_iter().map(|c| rend_row5(&c, *pn.get(&c.url).expect("deduped row keeps its url"))).collect();
            assert_eq!(rend_rows5(&rows), want, "{}", id);
        }
    }


    #[test]
    fn pair_tracks_gives_every_video_the_best_audio() {
        crate::pin_test_dir();
        const CASES: &[(&str, &[&str], &[&str], &str)] = &[
            ("pt_01", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[

            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"\\\" audio_url:\\\"\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_02", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/9", "origin": "ytdlp", "headers": {"X-A": "1"}, "audio_codec": "aac", "label": "m4a 128 k"}, "form": {"abr": 128}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"aac\\\" audio_url:\\\"http://a/9\\\" audio_headers:{\\\"X-A\\\"=\\\"1\\\"} audio_label:\\\"m4a 128 k\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_03", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/low", "origin": "ytdlp", "audio_codec": "mp4a.40.2", "label": "m4a 64 k"}, "form": {"abr": 64}}"##,
                r##"{"candidate": {"url": "http://a/high", "origin": "ytdlp", "audio_codec": "mp4a.40.2", "label": "m4a 192 k", "audio_headers": {"X-A": "1"}}, "form": {"abr": 192}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"http://a/high\\\" audio_headers:{} audio_label:\\\"m4a 192 k\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_04", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/b", "origin": "ytdlp", "audio_codec": "aac"}, "form": {"abr": 128}}"##,
                r##"{"candidate": {"url": "http://a/c", "origin": "ytdlp", "audio_codec": "opus"}, "form": {"abr": 128}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"opus\\\" audio_url:\\\"http://a/c\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_05", &[
                r##"{"candidate": {"url": "http://v/1080", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1080"}}"##,
                r##"{"candidate": {"url": "http://v/480", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/480"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/high", "origin": "ytdlp", "audio_codec": "mp4a.40.2", "label": "m4a 192 k", "audio_headers": {"X-A": "1"}}, "form": {"abr": 192}}"##,
            ], "[\"[url:\\\"http://v/1080\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"http://a/high\\\" audio_headers:{} audio_label:\\\"m4a 192 k\\\" silent:false height:i:0 segmented:false]\", \"[url:\\\"http://v/480\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"http://a/high\\\" audio_headers:{} audio_label:\\\"m4a 192 k\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_06", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264", "audio_url": "http://old/1", "audio_codec": "flac", "audio_label": "old label", "audio_headers": {"Old": "x"}}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/8", "origin": "ytdlp", "audio_codec": "aac", "label": "m4a 128 k"}, "form": {"abr": 128}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"aac\\\" audio_url:\\\"http://a/8\\\" audio_headers:{} audio_label:\\\"m4a 128 k\\\" silent:false height:i:0 segmented:false]\"]"),
            ("pt_07", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264", "probe": {"reach": "opened", "video_codec": "h264", "height": 720}}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
                r##"{"candidate": {"url": "http://v/2", "origin": "ytdlp", "label": "mp4 720p H.264", "probe": {"reach": "opened", "video_codec": "h264", "height": 1080}}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/2"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/8", "origin": "ytdlp", "audio_codec": "aac", "label": "m4a 128 k"}, "form": {"abr": 128}}"##,
                r##"{"candidate": {"url": "http://a/high", "origin": "ytdlp", "audio_codec": "mp4a.40.2", "label": "m4a 192 k", "audio_headers": {"X-A": "1"}}, "form": {"abr": 192}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"http://a/high\\\" audio_headers:{} audio_label:\\\"m4a 192 k\\\" silent:false height:i:720 segmented:false]\", \"[url:\\\"http://v/2\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:[reach:\\\"opened\\\" container:\\\"\\\" video_codec:\\\"h264\\\" audio_codec:\\\"\\\" width:i:0 height:i:1080 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\\\"\\\" measured:true moving_picture:true] probe_null:false last_resort:false reason:\\\"\\\" audio_codec:\\\"mp4a.40.2\\\" audio_url:\\\"http://a/high\\\" audio_headers:{} audio_label:\\\"m4a 192 k\\\" silent:false height:i:1080 segmented:false]\"]"),
            ("pt_08", &[
                r##"{"candidate": {"url": "http://v/1", "origin": "ytdlp", "label": "mp4 720p H.264"}, "form": {"vcodec": "h264", "acodec": "none", "url": "http://v/1"}}"##,
            ], &[
                r##"{"candidate": {"url": "http://a/x", "origin": "ytdlp", "audio_codec": "aac"}, "form": {"abr": "0"}}"##,
                r##"{"candidate": {"url": "http://a/y", "origin": "ytdlp", "audio_codec": "opus"}, "form": {}}"##,
            ], "[\"[url:\\\"http://v/1\\\" origin:\\\"ytdlp\\\" headers:{} label:\\\"mp4 720p H.264\\\" title:\\\"\\\" probe:null probe_null:true last_resort:false reason:\\\"\\\" audio_codec:\\\"opus\\\" audio_url:\\\"http://a/y\\\" audio_headers:{} audio_label:\\\"\\\" silent:false height:i:0 segmented:false]\"]"),
        ];
        for c in CASES {
            let (id, vraws, araws, want) = *c;
            let mut vids: Vec<(Candidate, Value)> = Vec::new();
            let mut pns = Vec::new();
            for raw in vraws {
                let j = json5(raw);
                let (cand, probe_null) = cand5(j.get("candidate").expect("pt video candidate"));
                vids.push((cand, j.get("form").cloned().unwrap_or(Value::Null)));
                pns.push(probe_null);
            }
            let auds: Vec<(Candidate, Value)> = araws
                .iter()
                .map(|raw| {
                    let j = json5(raw);
                    let (cand, _pn) = cand5(j.get("candidate").expect("pt audio candidate"));
                    (cand, j.get("form").cloned().unwrap_or(Value::Null))
                })
                .collect();
            pair_tracks(&mut vids, &auds);
            let rows: Vec<String> = vids.iter().zip(pns.iter()).map(|((c, _f), pn)| rend_row5(c, *pn)).collect();
            assert_eq!(rend_rows5(&rows), want, "{}", id);
        }
    }


    #[test]
    fn probing_a_url_reads_ffprobe_and_answers_in_three_states() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str, Option<&str>, u64, &str)] = &[
            ("pu_01", "ok.sh", "http://x/1.mp4", None, 0, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
            ("pu_02", "live.sh", "http://x/live.m3u8", None, 0, "[reach:\"opened\" container:\"mpegts\" video_codec:\"h264\" audio_codec:\"\" width:i:1280 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:true moving_picture:true]"),
            ("pu_03", "refused403.sh", "http://x/1", None, 0, "[reach:\"refused\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"Server returned 403 Forbidden (access denied)\" measured:false moving_picture:false]"),
            ("pu_04", "refused_mid.sh", "http://x/1", None, 0, "[reach:\"refused\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"正在连接…\" measured:false moving_picture:false]"),
            ("pu_05", "unproven_err.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"Connection refused\" measured:false moving_picture:false]"),
            ("pu_06", "code1403.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"internal server error code 1403\" measured:false moving_picture:false]"),
            ("pu_07", "unicode_adjacent.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"误403无法打开\" measured:false moving_picture:false]"),
            ("pu_08", "brackets403.sh", "http://x/1", None, 0, "[reach:\"refused\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"[403] open failed\" measured:false moving_picture:false]"),
            ("pu_09", "junk_stdout.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"unreadable probe output\" measured:false moving_picture:false]"),
            ("pu_10", "empty_stderr.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"\" measured:false moving_picture:false]"),
            ("pu_11", "blank_first.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"Real message\" measured:false moving_picture:false]"),
            ("pu_12", "cut_long.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中\" measured:false moving_picture:false]"),
            ("pu_13", "sleep.sh", "http://x/1", None, 500, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"probe timed out\" measured:false moving_picture:false]"),
            ("pu_14", "vert_tab.sh", "http://x/1", None, 0, "[reach:\"unproven\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"first\" measured:false moving_picture:false]"),
            ("pu_15", "ok.sh", "http://x/1", Some(r##"{"X-B": "2", "X-A": "1"}"##), 0, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
            ("pu_16", "ok.sh", "http://x/1", Some(r##"{}"##), 0, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
            ("pu_17", "ok.sh", "http://x/1", Some(r##"{"U": "中"}"##), 0, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
            ("pu_18", "ok.sh", "http://x/1", Some(r##"{"Authorization": "Bearer t"}"##), 0, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
        ];
        for c in CASES {
            let (id, script, url, headers_raw, timeout_ms, want) = *c;
            let bin = fakes5().join(script);
            let bin_str = bin.to_str().expect("utf-8 temp dir");
            let headers = headers_raw.map(|h| strmap5(&json5(h)));
            let got = if timeout_ms > 0 {
                probe_url_with(url, headers.as_ref(), Some(bin_str), std::time::Duration::from_millis(timeout_ms))
            } else {
                probe_url(url, headers.as_ref(), Some(bin_str))
            };
            assert_eq!(rend_probe5(&got), want, "{}", id);
        }
    }


    #[test]
    fn measuring_a_candidate_probes_the_url_it_carries() {
        crate::pin_test_dir();
        const CASES: &[(&str, &str, &str, &str)] = &[
            ("me_01", "ok.sh", r##"{"url": "http://x/1", "origin": "ytdlp", "headers": {"X-B": "2", "X-A": "1"}}"##, "[reach:\"opened\" container:\"mov,mp4,m4a,3gp,3g2,mj2\" video_codec:\"h264\" audio_codec:\"aac\" width:i:1920 height:i:1080 duration:f:406a89374bc6a7f0 bitrate:f:41459f0b80000000 is_live:false error:\"\" measured:true moving_picture:true]"),
            ("me_02", "refused403.sh", r##"{"url": "http://x/2", "origin": "ytdlp"}"##, "[reach:\"refused\" container:\"\" video_codec:\"\" audio_codec:\"\" width:i:0 height:i:0 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:false error:\"Server returned 403 Forbidden (access denied)\" measured:false moving_picture:false]"),
            ("me_03", "live.sh", r##"{"url": "http://x/3", "origin": "page"}"##, "[reach:\"opened\" container:\"mpegts\" video_codec:\"h264\" audio_codec:\"\" width:i:1280 height:i:720 duration:f:0000000000000000 bitrate:f:0000000000000000 is_live:true error:\"\" measured:true moving_picture:true]"),
        ];
        for c in CASES {
            let (id, script, raw, want) = *c;
            let bin = fakes5().join(script);
            let bin_str = bin.to_str().expect("utf-8 temp dir");
            let (mut cand, _probe_null) = cand5(&json5(raw));
            let got = measure(&mut cand, Some(bin_str));
            assert_eq!(rend_probe5(&got), want, "{}", id);
            assert_eq!(rend_probe5(&cand.probe), want, "{}: the probe stays on the candidate", id);
        }
    }
}

#[cfg(test)]
#[path = "scrape_tests.rs"]
mod scrape_tests;

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
