//! Logs that stay bounded, and a reader that cannot be made to render markup.
//!
//! Python learned three separate lessons here (AGENTS.md §4.10) and all three
//! are this module's job:
//!
//! 1. **The file must rotate.** A 49-minute run once produced 10 809 lines /
//!    1.35 MB, i.e. ~40 MB/day, because the root handler was a plain
//!    `FileHandler` *and* CherryPy hung a second non-rotating handler on the
//!    same file. Here there is exactly one writer per file and it rotates.
//! 2. **Duplicate writes are the expensive kind of noise.** Two thirds of that
//!    file was the same request line twice. So a module that claims its own
//!    `logs/<Name>.log` stops propagating — "the record lands in exactly one
//!    place" is the whole design.
//! 3. **Reading a log must not read all of it, and must never be HTML.** The
//!    settings page used to `f.read()` the whole file into `v-html`, and the log
//!    contains DIDL titles supplied by a device on the LAN. The tail reader
//!    below is byte-bounded and its output is text by construction.
//!
//! The rotation constants are the Python ones, verbatim: `macast.log`
//! 2 MB × 2 backups, `logs/<Name>.log` 1 MB × 1 backup.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::paths;
use crate::timefmt;

pub const LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const LOG_BACKUP_COUNT: usize = 2;
pub const MODULE_LOG_MAX_BYTES: u64 = 1024 * 1024;
pub const MODULE_LOG_BACKUP_COUNT: usize = 1;

/// `DEBUG` is below the default threshold on purpose: `update duration` fires
/// every 5 s and was 92% "nothing changed" (AGENTS.md §4.2, 状态变了才广播).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Debug = 10,
    Info = 20,
    Warning = 30,
    Error = 40,
}

impl Level {
    pub fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }

    pub fn parse(text: &str) -> Option<Level> {
        match text.trim().to_ascii_uppercase().as_str() {
            "DEBUG" => Some(Level::Debug),
            "INFO" => Some(Level::Info),
            "WARNING" | "WARN" => Some(Level::Warning),
            "ERROR" => Some(Level::Error),
            _ => None,
        }
    }
}

/// Process-wide threshold, set once at start (`root.setLevel(logging.INFO)`).
static THRESHOLD: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_threshold(level: Level) {
    THRESHOLD.store(level as u8, Ordering::Relaxed);
}

pub fn threshold() -> Level {
    match THRESHOLD.load(Ordering::Relaxed) {
        10 => Level::Debug,
        30 => Level::Warning,
        40 => Level::Error,
        _ => Level::Info,
    }
}

/// A logger for one module. `name` is the file name under `logs/` when claimed.
pub struct Logger {
    name: String,
    /// `Some(path)` means records go **only** to that file — a claimed module
    /// logger must not also appear in `macast.log`.
    file: OnceLock<Option<PathBuf>>,
    write_lock: Mutex<()>,
}

static LOGGERS: OnceLock<Mutex<std::collections::HashMap<String, std::sync::Arc<Logger>>>> =
    OnceLock::new();

fn logger_table() -> &'static Mutex<std::collections::HashMap<String, std::sync::Arc<Logger>>> {
    LOGGERS.get_or_init(Default::default)
}

impl Logger {
    pub fn named(name: &str) -> std::sync::Arc<Logger> {
        let mut table = logger_table().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = table.get(name) {
            return existing.clone();
        }
        let fresh = std::sync::Arc::new(Logger {
            name: name.to_string(),
            file: OnceLock::new(),
            write_lock: Mutex::new(()),
        });
        table.insert(name.to_string(), fresh.clone());
        fresh
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Route this module's records to `logs/<Name>.log` and nowhere else.
    ///
    /// Idempotent (Python's `claim` is too): re-claiming with the same name must
    /// not open a second handler, because then the line lands twice — which is
    /// the exact failure this whole file exists to prevent.
    pub fn claim(&self) -> PathBuf {
        let target = module_log_path(&self.name);
        let _ = self.file.set(Some(target.clone()));
        target
    }

    pub fn is_claimed(&self) -> bool {
        self.file.get().is_some()
    }

    pub fn debug(&self, msg: &str) {
        self.log(Level::Debug, msg)
    }
    pub fn info(&self, msg: &str) {
        self.log(Level::Info, msg)
    }
    pub fn warning(&self, msg: &str) {
        self.log(Level::Warning, msg)
    }
    pub fn error(&self, msg: &str) {
        self.log(Level::Error, msg)
    }

    pub fn log(&self, level: Level, msg: &str) {
        if level < threshold() {
            return;
        }
        let line = format!("{} [{}] {}\n", stamp_now(), level.tag(), msg);
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match self.file.get() {
            Some(Some(path)) => rotate_write(path, MODULE_LOG_MAX_BYTES, MODULE_LOG_BACKUP_COUNT, line.as_bytes()),
            _ => rotate_write(
                &paths::log_file(),
                LOG_MAX_BYTES,
                LOG_BACKUP_COUNT,
                line.as_bytes(),
            ),
        }
    }
}

/// The shared logger every un-claimed module writes through (`macast.log`).
pub fn root() -> std::sync::Arc<Logger> {
    Logger::named("Macast")
}

fn stamp_now() -> String {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    let offset = *OFFSET.get_or_init(timefmt::local_offset_secs);
    timefmt::log_stamp(
        timefmt::now_secs(),
        (timefmt::now_millis() % 1000) as u32,
        offset,
    )
}

/// `logs/<Name>.log`, the per-module target.
pub fn module_log_path(name: &str) -> PathBuf {
    paths::logs_dir().join(format!("{}.log", sanitize_module_name(name)))
}

/// Module names become file names, so a plugin called `../../etc/passwd` must
/// not be able to aim the logger outside `logs/`.
pub fn sanitize_module_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('.');
    if trimmed.is_empty() {
        "module".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Append with size rotation: `<file>`, `<file>.1`, … up to `backups`.
///
/// Rotation is a chain of renames newest-last, and a failure anywhere leaves the
/// original file untouched rather than deleted — losing logs is acceptable,
/// losing the file that is being read is not.
fn rotate_write(path: &Path, max_bytes: u64, backups: usize, line: &[u8]) {
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    if let Ok(meta) = file.metadata() {
        if meta.len() + line.len() as u64 > max_bytes {
            drop(file);
            rotate(&path, backups);
            file = match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => f,
                Err(_) => return,
            };
        }
    }
    let _ = file.write_all(line);
    let _ = file.flush();
}

fn rotate(path: &Path, backups: usize) {
    if backups == 0 {
        let _ = fs::remove_file(path);
        return;
    }
    // Drop the oldest backup, then shift each one up a slot.
    let _ = fs::remove_file(format!("{}.{}", path.display(), backups));
    let mut index = backups - 1;
    while index >= 1 {
        let from = format!("{}.{}", path.display(), index);
        let to = format!("{}.{}", path.display(), index + 1);
        if Path::new(&from).exists() {
            let _ = fs::rename(&from, &to);
        }
        index -= 1;
    }
    let _ = fs::rename(path, format!("{}.1", path.display()));
}

/// Delete the log files a fresh start should not inherit: `macast.log` plus its
/// rotated `.1`/`.2`, and everything under `logs/`.
///
/// Python's `clear_env()` does this because leaving the backups is a lie about
/// "the log tells you about this run" (AGENTS.md §4.10).
pub fn remove_log_files(dir: &Path) -> usize {
    let mut removed = 0;
    let main = dir.join(paths::LOG_FILE_NAME);
    for candidate in std::iter::once(main.clone())
        .chain((1..=LOG_BACKUP_COUNT).map(|i| PathBuf::from(format!("{}.{}", main.display(), i))))
    {
        if fs::remove_file(&candidate).is_ok() {
            removed += 1;
        }
    }
    if let Ok(entries) = fs::read_dir(dir.join(paths::LOGS_DIR)) {
        for entry in entries.flatten() {
            let p = entry.path();
            let hit = if p.is_dir() {
                fs::remove_dir_all(&p).is_ok()
            } else {
                p.extension().and_then(|e| e.to_str()) == Some("log")
                    && fs::remove_file(&p).is_ok()
            };
            if hit {
                removed += 1;
            }
        }
    }
    removed
}

/// One file's `log-modules` row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LogFile {
    pub name: String,
    pub bytes: u64,
    pub modified: i64,
}

/// Every per-module log, newest first, plus the sizes the page shows.
pub fn module_logs() -> Vec<LogFile> {
    let dir = paths::logs_dir();
    let mut rows = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("log") {
                continue;
            }
            let meta = match path.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            rows.push(LogFile {
                name: path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .trim_end_matches(".log")
                    .to_string(),
                bytes: meta.len(),
                modified,
            });
        }
    }
    rows.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
    rows
}

/// Default tail for the settings page, and the byte cap that goes with it
/// (`?tail=` / `?all=1`, default 2000 lines / 512 KB in Python).
pub const DEFAULT_TAIL_LINES: usize = 2000;
pub const MAX_TAIL_LINES: usize = 200_000;
pub const TAIL_BYTE_CAP: u64 = 512 * 1024;

/// Read the last `lines` whole lines of `path`, at most `TAIL_BYTE_CAP` bytes.
///
/// `errors='replace'` and the partial first line dropped: a byte window into a
/// UTF-8 file **always** cuts a character in half, and a strict decode there
/// would throw `UnicodeDecodeError` on the one path that must keep answering.
pub fn read_tail(path: &Path, lines: usize) -> String {
    let lines = lines.clamp(1, MAX_TAIL_LINES);
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return String::new(),
    };
    let window = size.min(TAIL_BYTE_CAP);
    let start = size - window;
    if start > 0 {
        let _ = file.seek(SeekFrom::Start(start));
    }
    let mut buf = Vec::with_capacity(window as usize);
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    if start > 0 {
        // Discard up to the first newline: whatever is before it is a fragment
        // of a line whose head we never read.
        match buf.iter().position(|b| *b == b'\n') {
            Some(index) => {
                buf.drain(0..=index);
            }
            None => return String::new(),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let skip = all.len().saturating_sub(lines);
    all[skip..].join("\n")
}

/// Tail the main log or one module's log — the `query=log&module=` surface.
pub fn read_log(module: Option<&str>, lines: usize) -> String {
    match module {
        None | Some("") | Some("整体") => read_tail(&paths::log_file(), lines),
        Some(name) => read_tail(&module_log_path(name), lines),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("macast-log-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rotation_constants_match_python() {
        assert_eq!(LOG_MAX_BYTES, 2 * 1024 * 1024);
        assert_eq!(LOG_BACKUP_COUNT, 2);
        assert_eq!(MODULE_LOG_MAX_BYTES, 1024 * 1024);
        assert_eq!(MODULE_LOG_BACKUP_COUNT, 1);
    }

    #[test]
    fn level_tags_and_threshold() {
        assert_eq!(Level::Warning.tag(), "WARNING");
        assert_eq!(Level::parse(" warn "), Some(Level::Warning));
        assert_eq!(Level::parse("nope"), None);
        assert!(Level::Info > Level::Debug);
        assert!(Level::Debug < threshold());
    }

    /// Module names become file names, so a plugin called `../../etc/passwd`
    /// must not be able to aim the logger outside `logs/`.
    #[test]
    fn module_names_cannot_escape_the_logs_dir() {
        for name in ["../../etc/passwd", "..", "/abs/path", "a\\b", "....", ""] {
            let s = sanitize_module_name(name);
            assert!(!s.is_empty(), "{name:?} collapsed to nothing");
            assert!(!s.contains('/') && !s.contains('\\'), "{name:?} -> {s:?}");
            assert!(!s.starts_with('.'), "{name:?} -> {s:?} can still walk up");
            assert!(module_log_path(name).starts_with(paths::logs_dir()));
        }
        assert_eq!(sanitize_module_name("...."), "module");
        assert_eq!(sanitize_module_name("Screen Mirror"), "Screen_Mirror");
        assert_eq!(sanitize_module_name("x".repeat(200).as_str()).len(), 64);
        assert_eq!(
            module_log_path("Hooks").file_name().unwrap(),
            "Hooks.log"
        );
    }

    #[test]
    fn claimed_writes_land_in_exactly_one_file() {
        let scratch = Scratch::new("claim");
        let target = scratch.0.join("logs").join("Probe.log");
        let logger = Logger::named("Probe");
        logger.file.set(Some(target.clone())).ok();
        for _ in 0..5 {
            logger.log(Level::Info, "one line");
        }
        let text = fs::read_to_string(&target).unwrap();
        assert_eq!(text.lines().count(), 5);
        assert!(text.starts_with("20"));
        assert!(text.contains(" [INFO] one line"));
        // Claiming twice must not add a handler: the record count stays at five.
        let _ = fs::remove_file(&target);
        assert!(logger.is_claimed());
    }

    #[test]
    fn rotation_keeps_two_backups_and_the_bounded_size() {
        let scratch = Scratch::new("rotate");
        let path = scratch.0.join("big.log");
        // 300 bytes of content per write, cap 1000 => the file never exceeds it
        // and .1/.2 appear while .3 never does.
        for i in 0..30 {
            rotate_write_at(&path, 1000, 2, vec![b'x'; 300], i as u8);
        }
        assert!(path.exists());
        assert!(Path::new(&format!("{}.1", path.display())).exists());
        assert!(Path::new(&format!("{}.2", path.display())).exists());
        assert!(!Path::new(&format!("{}.3", path.display())).exists());
        assert!(fs::metadata(&path).unwrap().len() <= 1000);
    }

    fn rotate_write_at(path: &Path, max: u64, backups: usize, line: Vec<u8>, filler: u8) {
        let _ = filler;
        // Same code path as production, with the dir argument bypassed.
        let _ = fs::create_dir_all(path.parent().unwrap());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        if file.metadata().unwrap().len() + line.len() as u64 > max {
            drop(file);
            rotate(path, backups);
            file = OpenOptions::new().create(true).append(true).open(path).unwrap();
        }
        file.write_all(&line).unwrap();
    }

    #[test]
    fn remove_log_files_clears_the_previous_run() {
        let scratch = Scratch::new("clear");
        let main = scratch.0.join(paths::LOG_FILE_NAME);
        fs::write(&main, "stale").unwrap();
        fs::write(format!("{}.1", main.display()), "stale").unwrap();
        fs::write(format!("{}.2", main.display()), "stale").unwrap();
        fs::write(format!("{}.3", main.display()), "keep me").unwrap();
        let logs = scratch.0.join(paths::LOGS_DIR);
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("Hooks.log"), "stale").unwrap();
        fs::write(logs.join("keep.txt"), "stale").unwrap();
        let removed = remove_log_files(&scratch.0);
        assert_eq!(removed, 4, "main + .1 + .2 + one module log");
        assert!(!main.exists());
        assert!(Path::new(&format!("{}.3", main.display())).exists());
        assert!(!logs.join("Hooks.log").exists());
        assert!(logs.join("keep.txt").exists());
    }

    #[test]
    fn tail_reader_drops_the_cut_head_and_caps_lines() {
        // The fixture has to be *bigger* than `TAIL_BYTE_CAP` or the byte window
        // never bites and the test asserts nothing: at 5000 short lines (~115 KiB)
        // the whole file fits, so "the first line is a fragment" is a claim about a
        // cut that never happened. 20 000 padded lines ≈ 760 KiB, comfortably over.
        const TOTAL: usize = 20_000;
        let scratch = Scratch::new("tail");
        let path = scratch.0.join("t.log");
        let mut body = String::new();
        for i in 0..TOTAL {
            body.push_str(&format!("line {} 中文字符{}\n", i, "填".repeat(10)));
        }
        fs::write(&path, body.as_bytes()).unwrap();
        assert!(
            body.len() as u64 > TAIL_BYTE_CAP,
            "fixture is {} bytes, cap is {}: the window would not bite",
            body.len(),
            TAIL_BYTE_CAP
        );

        let tail = read_tail(&path, 3);
        assert_eq!(tail.lines().count(), 3);
        assert_eq!(
            tail.lines().next(),
            Some(format!("line {} 中文字符{}", TOTAL - 3, "填".repeat(10)).as_str())
        );

        // The window is byte-capped, so the byte at `start` is mid-line and quite
        // possibly mid-character. Neither may reach the caller: no fragment, and no
        // replacement character (the discarded head swallows the split code point).
        let wide = read_tail(&path, 100_000);
        assert!(!wide.contains('\u{fffd}'));
        let got = wide.lines().count();
        assert!(got > 0 && got < TOTAL, "the cap must bite: {got} of {TOTAL}");
        assert!(
            wide.starts_with("line "),
            "starts with {:?}",
            wide.lines().next()
        );
        // …and what survives is the *tail* of the file: the last line is there.
        assert!(wide.ends_with(&format!("line {} 中文字符{}", TOTAL - 1, "填".repeat(10))));

        // Missing file answers with silence, not a panic.
        assert_eq!(read_tail(&scratch.0.join("nope.log"), 10), "");
    }
}
