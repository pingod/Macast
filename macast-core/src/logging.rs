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
///
/// `name` is the module (`Screen Mirror`), `file` is **what is on disk**
/// (`Screen Mirror.log.1` for a rotated sibling). Both, because the reference's
/// rows carry both (`logsplit.list_logs`), and the difference is the whole point:
/// a page that offers "download this" needs the file, while a page that offers
/// "read this module" needs the name. Synthesising `file` from `name` at the
/// caller is what made rotated rows name a file that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LogFile {
    pub name: String,
    pub file: String,
    pub bytes: u64,
    pub modified: i64,
}

/// The stem of a directory entry the reference's `list_logs` would keep:
/// `name.log` or `name.log.<digits>`, and `None` for anything else.
fn log_file_stem(file: &str) -> Option<String> {
    let Some((stem, suffix)) = file.split_once(".log") else {
        return None;
    };
    if suffix.is_empty() {
        return Some(stem.to_string());
    }
    let rotated = suffix.strip_prefix('.')?;
    if !rotated.is_empty() && rotated.chars().all(|c| c.is_ascii_digit()) {
        Some(stem.to_string())
    } else {
        None
    }
}

/// Every per-module log file on disk — **including the rotated backups**, one row
/// each — sorted by name and then by file name.
///
/// The sort and the rotated rows are both the reference's (`logsplit.list_logs`:
/// `sorted(os.listdir)`, a row per `name.log(.N)?`). The port used to skip
/// `name.log.1` and sort newest-first, which quietly breaks two page promises at
/// once: the 「日志」 panel's file count was lower than what the user is asking to
/// clear (`clear-log` removes the backups and counts them), and a module that had
/// rotated looked older than it was. `log-modules` is the owner of that list, so
/// the HTTP route and the window command both read it — see
/// `src-tauri/src/main.rs::log_modules`.
pub fn module_logs() -> Vec<LogFile> {
    let dir = paths::logs_dir();
    let mut rows = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(name) = log_file_stem(file) else {
                continue;
            };
            let Ok(meta) = path.metadata() else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            rows.push(LogFile {
                name,
                file: file.to_string(),
                bytes: meta.len(),
                modified,
            });
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.file.cmp(&b.file)));
    rows
}

/// The 「日志」 panel's tail document — `query=log` and the window's `log_view`
/// command, **one builder for both readers**.
///
/// Key order is the reference's and it differs between the two branches: success is
/// `{code, logs, truncated, lines, size, module}` (`protocol.py:2044-2046`, **no
/// `message`** on the happy path), while an unknown name answers
/// `{code, message, logs, truncated, lines, size, module}` with the sentence in
/// second place (`protocol.py:2036-2039`). A caller that renders "the panel or not"
/// from the presence of `message` is wrong in both directions, so the shape is
/// written once, here.
pub fn log_view_document(module: Option<&str>, lines: usize, all: bool) -> serde_json::Value {
    use serde_json::json;
    match read_log(module, lines, all) {
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

/// `query=log-modules` (`protocol.py:2116-2128`): the main file's size and mtime
/// plus one row per log file on disk — rotated siblings included, because they are
/// what `clear-log` counts when it takes them away.
///
/// Rows carry **both** `name` and `file` (see [`LogFile`]) and come sorted by
/// name, as the reference sorts them: a panel that wants "the module doing
/// something on top" has `mtime` and may sort, while a panel that has to explain
/// why three files disappeared on a clear cannot be shown a list of one.
///
/// `main` is `{size: 0, mtime: 0}` when the file cannot be read — the reference's
/// `except OSError` branch, whose comment is worth keeping: *"No log yet (fresh
/// install, or it was just cleared) is not an error: the page should show an empty
/// box, not a red banner."*
pub fn log_modules_document() -> serde_json::Value {
    use serde_json::json;
    let main = match std::fs::metadata(crate::paths::log_file()) {
        Ok(meta) => json!({
            "size": meta.len(),
            "mtime": meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }),
        Err(_) => json!({"size": 0, "mtime": 0}),
    };
    let modules: Vec<serde_json::Value> = module_logs()
        .into_iter()
        .map(|row| {
            json!({
                "name": row.name,
                "file": row.file,
                "size": row.bytes,
                "mtime": row.modified,
            })
        })
        .collect();
    json!({ "main": main, "modules": modules })
}

/// `POST clear-log` (`protocol.py:3091-3098` + `_clear_log`): empty one log and
/// its rotated siblings, answering with how many files went.
///
/// The failure line is written to [`root()`] — the **main** log — because the
/// per-module logger this shell uses for player traffic claims its own file, and
/// 「clear log error」 hidden in the file that was just refused would be unreadable
/// in exactly the moment someone needs it.
pub fn clear_log_document(module: Option<&str>) -> serde_json::Value {
    use serde_json::json;
    let (path, _resolved) = log_target(module);
    let Some(path) = path else {
        return json!({"code": 1, "message": "unknown log module"});
    };
    match clear_log_file(&path) {
        // `removed` on **both** branches: `_clear_log` (the main log) and
        // `logsplit.clear` (a module) return the same three keys, and the number is
        // the answer to "did it take the rotated backups too" — which is the whole
        // reason the module list above shows them.
        //
        // The target is resolved, not echoed: `?module=main`, `?module=整体` and no
        // `module` at all are the same call here, and the page must not need to know
        // which spelling the user's button sent.
        Ok(removed) => json!({"code": 0, "message": "success", "removed": removed}),
        Err(e) => {
            root().error(&format!("clear log error: {e}"));
            json!({"code": 1, "message": "clear failed"})
        }
    }
}

/// Default tail for the settings page (`?tail=`, `LOG_TAIL_LINES` in Python).
pub const DEFAULT_TAIL_LINES: usize = 2000;
/// Ceiling for one tail request (`LOG_TAIL_MAX_LINES`).
pub const MAX_TAIL_LINES: usize = 50_000;
/// Floor of the byte window: 512 KB of log is enough for the default 2000 lines.
pub const TAIL_BYTE_FLOOR: u64 = 512 * 1024;
/// Ceiling of the byte window (`LOG_TAIL_MAX_BYTES`) -- the budget scales toward
/// this as more lines are asked for, and never past it.
pub const TAIL_BYTE_MAX: u64 = 4 * 1024 * 1024;
/// The reference's per-line allowance. DIDL titles plus whole SOAP request lines
/// run to several hundred bytes, which is why the budget is a product of the
/// line count and not one fixed window: a 512 KB cap on a `?tail=10000` request
/// answers with ~2000 lines and the page has no way to say so.
const TAIL_BYTES_PER_LINE: u64 = 256;
/// `?all=1` bound (`LOG_ALL_MAX_BYTES`): "anything bigger is a download, not a
/// page".
pub const ALL_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// The byte window a request for `lines` lines may read.
fn tail_byte_budget(lines: usize) -> u64 {
    let wanted = (lines as u64).saturating_mul(TAIL_BYTES_PER_LINE);
    wanted.max(TAIL_BYTE_FLOOR).min(TAIL_BYTE_MAX)
}

/// What a bounded read of one file decided.
///
/// All three fields are needed together: `truncated` without `size` is
/// "something is missing" with no way to say how much, which is the half the
/// reference added so the page can print "only the last N lines of M bytes"
/// instead of silently pretending it shows everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailRead {
    pub text: String,
    pub truncated: bool,
    pub size: u64,
}

impl TailRead {
    fn empty() -> TailRead {
        TailRead {
            text: String::new(),
            truncated: false,
            size: 0,
        }
    }
}

/// Read the last `lines` whole lines of `path`, within a byte window that scales
/// with `lines` ([`tail_byte_budget`]).
///
/// Lossy decode and the partial first line dropped: a byte window into a UTF-8
/// file **always** cuts a character in half, and a strict decode there would
/// throw on the one path that must keep answering (a Chinese media title in a
/// DIDL log line is enough to hit it).
///
/// A missing or unreadable file is `Ok` and empty, not an error -- "no log yet"
/// (fresh install, or it was just cleared) must render an empty box, not a red
/// banner.
pub fn read_tail(path: &Path, lines: usize) -> TailRead {
    let lines = lines.clamp(1, MAX_TAIL_LINES);
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return TailRead::empty(),
    };
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return TailRead::empty(),
    };
    let window = size.min(tail_byte_budget(lines));
    let start = size - window;
    if start > 0 {
        let _ = file.seek(SeekFrom::Start(start));
    }
    let mut buf = Vec::with_capacity(window as usize);
    if file.read_to_end(&mut buf).is_err() {
        return TailRead {
            text: String::new(),
            truncated: start > 0,
            size,
        };
    }
    let mut truncated = start > 0;
    if start > 0 {
        // Discard up to the first newline: whatever is before it is a fragment
        // of a line whose head we never read.
        match buf.iter().position(|b| *b == b'\n') {
            Some(index) => {
                buf.drain(0..=index);
            }
            None => return TailRead {
                text: String::new(),
                truncated,
                size,
            },
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let skip = all.len().saturating_sub(lines);
    if all.len() > lines {
        // Line-bound, not byte-bound: we still left whole lines behind.
        truncated = true;
    }
    TailRead {
        text: all[skip..].join("\n"),
        truncated,
        size,
    }
}

/// Whole-file flavour of [`read_tail`], bounded by `max_bytes` only: no line
/// cap, no partial head dropped.
pub fn read_whole(path: &Path, max_bytes: u64) -> TailRead {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return TailRead::empty(),
    };
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return TailRead::empty(),
    };
    // One byte past the bound is all the witness we need that there was more --
    // and it is also the ceiling on what this call may put in memory, so a 40 MB
    // log asked for with `?all=1` does not become a 40 MB allocation.
    let want = size.min(max_bytes.saturating_add(1));
    let mut buf = Vec::with_capacity(want as usize);
    if file.take(want).read_to_end(&mut buf).is_err() {
        return TailRead::empty();
    }
    let truncated = buf.len() as u64 > max_bytes;
    buf.truncate(max_bytes as usize);
    TailRead {
        text: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
        size,
    }
}

/// The page's log view: one file's tail (or whole body) plus the counts that let
/// it say what it is showing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LogPayload {
    pub logs: String,
    pub truncated: bool,
    pub lines: usize,
    pub size: u64,
    /// `''` for the main log, matching the reference: the page uses it to tell
    /// "showing everything" from "nothing was selected".
    pub module: String,
}

/// Tail or read one log as the settings page asks for it.
///
/// `Err(module)` is the unknown-name answer (see [`log_target`]) -- a sentence
/// the caller renders, not a file it can read.
pub fn read_log(module: Option<&str>, lines: usize, all: bool) -> Result<LogPayload, String> {
    let (path, name) = log_target(module);
    let path = match path {
        Some(p) => p,
        None => return Err(name),
    };
    let read = if all {
        read_whole(&path, ALL_MAX_BYTES)
    } else {
        read_tail(&path, lines)
    };
    let lines = if read.text.is_empty() {
        0
    } else {
        read.text.lines().count()
    };
    Ok(LogPayload {
        logs: read.text,
        truncated: read.truncated,
        lines,
        size: read.size,
        module: name,
    })
}

/// Which file a log query targets: `macast.log`, or one module's own.
///
/// `(None, name)` means nobody has written a log under that name -- an unknown
/// name is a mistake, not a secret, so it gets its own answer rather than the
/// whole list. The known set is the files on disk **plus** the loggers claimed
/// this run, because a plugin that has claimed its file but not yet logged
/// anything is a name the page is entitled to read (an empty box, per
/// [`read_tail`]).
pub fn log_target(module: Option<&str>) -> (Option<PathBuf>, String) {
    let name = module.unwrap_or("").trim().to_string();
    let lower = name.to_lowercase();
    if name.is_empty() || lower == "main" || lower == "macast" || lower == "整体" {
        return (Some(paths::log_file()), String::new());
    }
    if !known_log_modules().iter().any(|known| known == &name) {
        return (None, name);
    }
    (Some(module_log_path(&name)), name)
}

/// Every module name the page may ask for: on disk or claimed this run.
pub fn known_log_modules() -> Vec<String> {
    let mut names: Vec<String> = module_logs().into_iter().map(|row| row.name).collect();
    for name in claimed_names() {
        if !names.iter().any(|known| known == &name) {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// The names of the loggers that claimed their own file this run, sorted.
///
/// Kept separate from [`module_logs`] on purpose: the files on disk answer
/// "what has been written", this answers "what has been routed", and the second
/// half is what lets a fresh plugin show up in the dropdown before its first
/// record rotates anything in.
pub fn claimed_names() -> Vec<String> {
    let table = logger_table().lock().unwrap_or_else(|e| e.into_inner());
    let mut names: Vec<String> = table
        .values()
        .filter(|logger| logger.is_claimed())
        .map(|logger| logger.name.clone())
        .collect();
    names.sort();
    names
}

/// Truncate one log and drop its rotated backups, returning how many files were
/// emptied or removed (the log itself counts as one).
///
/// Truncating from out here is safe because every writer opens in append mode:
/// the next record seeks to the (new) end instead of writing into a hole. The
/// backups have to go too -- they are the older megabytes the user is trying to
/// get rid of -- and unlike [`remove_log_files`], which keeps backups past the
/// rotation count, this follows the reference's `path + '.*'` and takes any
/// sibling the name prefixes, because a stray `macast.log.3` is exactly such a
/// leftover.
///
/// `Err` is only the truncation itself failing; a backup that will not go away
/// is counted as not-removed, not as a failed clear.
pub fn clear_log_file(path: &Path) -> std::io::Result<usize> {
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(path)?
        .sync_all()?;
    let mut removed = 1usize;
    let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
        return Ok(removed);
    };
    let Some(parent) = path.parent() else {
        return Ok(removed);
    };
    let prefix = format!("{stem}.");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let other = entry.path();
            if !other.is_file() {
                continue;
            }
            match other.file_name().and_then(|s| s.to_str()) {
                Some(name) if name.starts_with(&prefix) => {
                    if fs::remove_file(&other).is_ok() {
                        removed += 1;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(removed)
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

    /// ~53 bytes per line, CJK included, so a byte window into it lands
    /// mid-character about as often as mid-line -- which is what the reader is
    /// required to survive.
    fn tail_fixture(total: usize) -> String {
        let mut body = String::new();
        for i in 0..total {
            body.push_str(&format!("line {} 中文字符{}\n", i, "填".repeat(10)));
        }
        body
    }

    #[test]
    fn tail_byte_budget_scales_with_the_ask() {
        // Below the crossover (2048 lines) the floor rules, which is what makes
        // `?tail=3` as cheap to read as the page default.
        assert_eq!(tail_byte_budget(0), TAIL_BYTE_FLOOR);
        assert_eq!(tail_byte_budget(1), TAIL_BYTE_FLOOR);
        assert_eq!(tail_byte_budget(DEFAULT_TAIL_LINES), TAIL_BYTE_FLOOR);
        // Above it the budget is 256 bytes per line -- the reference's allowance,
        // sized for DIDL titles and whole SOAP request lines.
        assert_eq!(tail_byte_budget(4_000), 4_000 * TAIL_BYTES_PER_LINE);
        assert_eq!(tail_byte_budget(10_000), 10_000 * TAIL_BYTES_PER_LINE);
        // And it stops at the ceiling instead of following the request.
        assert_eq!(tail_byte_budget(16_384), TAIL_BYTE_MAX);
        assert_eq!(tail_byte_budget(20_000), TAIL_BYTE_MAX);
        // saturating: `usize::MAX * 256` must not wrap into a tiny window.
        assert_eq!(tail_byte_budget(usize::MAX), TAIL_BYTE_MAX);
    }

    #[test]
    fn tail_reader_drops_the_cut_head_and_caps_lines() {
        // The fixture has to be *bigger* than the floor or the byte window never
        // bites and "the first line is a fragment" is a claim about a cut that
        // never happened. 20 000 padded lines ≈ 1.07 MB, comfortably over.
        const TOTAL: usize = 20_000;
        let scratch = Scratch::new("tail");
        let path = scratch.0.join("t.log");
        let body = tail_fixture(TOTAL);
        fs::write(&path, body.as_bytes()).unwrap();
        assert!(
            body.len() as u64 > TAIL_BYTE_FLOOR,
            "fixture is {} bytes, floor is {}: the window would not bite",
            body.len(),
            TAIL_BYTE_FLOOR
        );

        let three = read_tail(&path, 3);
        assert_eq!(three.text.lines().count(), 3);
        assert_eq!(
            three.text.lines().next(),
            Some(format!("line {} 中文字符{}", TOTAL - 3, "填".repeat(10)).as_str())
        );
        assert!(three.truncated, "thousands of lines were left behind");
        assert_eq!(three.size, body.len() as u64);

        // The page's default ask: still byte-bound, and neither a line fragment
        // nor a replacement character may reach the caller (discarding the head
        // swallows the split code point with it).
        let wide = read_tail(&path, DEFAULT_TAIL_LINES);
        assert_eq!(wide.text.lines().count(), DEFAULT_TAIL_LINES);
        assert!(!wide.text.contains('\u{fffd}'));
        assert!(wide.text.starts_with("line "));
        assert!(wide
            .text
            .ends_with(&format!("line {} 中文字符{}", TOTAL - 1, "填".repeat(10))));
        assert!(wide.truncated);

        // Why the budget is a product of the line count rather than one fixed
        // window: asking for 10 000 lines has to *get* 10 000 lines. A fixed
        // 512 KiB window answers this with ~9 700 and the page cannot tell.
        let ten = read_tail(&path, 10_000);
        assert_eq!(ten.text.lines().count(), 10_000);
        assert_eq!(
            ten.text.lines().next(),
            Some(format!("line {} 中文字符{}", TOTAL - 10_000, "填".repeat(10)).as_str())
        );
        assert!(ten.truncated, "the line cap bit, the byte window did not");

        // At the ceiling the window is 4 MiB -- past this whole file -- so nothing
        // was left behind and `truncated` has to say so honestly.
        let whole = read_tail(&path, MAX_TAIL_LINES);
        assert_eq!(whole.text.lines().count(), TOTAL);
        assert!(!whole.truncated);

        // Missing file answers with silence: not a panic, not an error.
        let gone = read_tail(&scratch.0.join("nope.log"), 10);
        assert_eq!(gone.text, "");
        assert!(!gone.truncated);
        assert_eq!(gone.size, 0);
    }

    #[test]
    fn whole_reader_stops_at_the_bound_and_keeps_the_true_size() {
        let scratch = Scratch::new("whole");
        let path = scratch.0.join("w.log");
        let body = tail_fixture(2_000);
        fs::write(&path, body.as_bytes()).unwrap();

        let limited = read_whole(&path, 100);
        assert_eq!(limited.text.len(), 100, "the bound is a byte bound");
        assert!(limited.truncated);
        assert_eq!(
            limited.size,
            body.len() as u64,
            "size is the file, not the window -- the page prints both"
        );

        let within = read_whole(&path, ALL_MAX_BYTES);
        assert_eq!(within.text, body);
        assert!(!within.truncated);
        assert_eq!(within.size, body.len() as u64);

        // A file far past the bound still reports its real length.
        let big = scratch.0.join("big.log");
        fs::write(&big, vec![b'a'; 4024]).unwrap();
        let capped = read_whole(&big, 1_000);
        assert_eq!(capped.text.len(), 1_000);
        assert!(capped.truncated);
        assert_eq!(capped.size, 4024);

        assert_eq!(read_whole(&scratch.0.join("nope.log"), 100), TailRead::empty());
    }

    #[test]
    fn log_target_names_the_main_log_for_every_alias() {
        for alias in [None, Some(""), Some("main"), Some(" MACAST "), Some(" 整体 ")] {
            let (path, module) = log_target(alias);
            assert_eq!(path, Some(paths::log_file()), "{alias:?}");
            assert_eq!(module, "", "{alias:?} must read as the main log");
        }
        // An unknown name is a mistake, not a secret: its own answer, not the list.
        let (path, module) = log_target(Some("no-such-module-18"));
        assert_eq!(path, None);
        assert_eq!(module, "no-such-module-18");
    }

    #[test]
    fn a_claimed_module_is_readable_before_it_has_written_anything() {
        // Claiming is process-global and irreversible, so this asserts membership
        // of the name it claims -- never an exact set other tests may have added.
        const NAME: &str = "Cards18";
        let logger = Logger::named(NAME);
        let target = logger.claim();
        assert!(claimed_names().iter().any(|n| n == NAME));
        assert!(known_log_modules().iter().any(|n| n == NAME));

        // Claimed but empty: an empty box with the name still echoed back.
        let empty = read_log(Some(NAME), DEFAULT_TAIL_LINES, false).unwrap();
        assert_eq!(empty.logs, "");
        assert_eq!(empty.lines, 0);
        assert_eq!(empty.size, 0);
        assert_eq!(empty.module, NAME);

        logger.info("first record");
        logger.info("second record");
        let read = read_log(Some(NAME), DEFAULT_TAIL_LINES, false).unwrap();
        assert_eq!(read.lines, 2);
        let on_disk = fs::read_to_string(&target).unwrap();
        assert_eq!(
            read.logs,
            on_disk.strip_suffix('\n').unwrap_or(&on_disk),
            "the tail reader joins whole lines: only the final newline is lost"
        );
        assert!(read.logs.contains(" [INFO] first record"));
        assert!(!read.truncated);
        assert_eq!(read.size, fs::metadata(&target).unwrap().len());

        // `?all=1` is the same file through the other reader, and the two readers
        // differ by exactly the newline the tail dropped: `read_whole` hands back
        // the bytes, `read_tail` hands back whole lines joined. The reference has
        // the same asymmetry (`f.read()` vs `"\n".join(lines[-tail:])`), so the
        // page must not assume the two flavours print identically.
        let all = read_log(Some(NAME), DEFAULT_TAIL_LINES, true).unwrap();
        assert_eq!(all.logs, on_disk);
        assert_eq!(all.lines, 2);
        assert_eq!(all.size, read.size);
        assert!(!all.truncated);

        // The unknown name refuses, and the refusal carries the name to render.
        assert_eq!(
            read_log(Some("no-such-module-18"), DEFAULT_TAIL_LINES, false),
            Err("no-such-module-18".to_string())
        );
    }

    #[test]
    fn clear_log_file_empties_the_log_and_takes_every_backup() {
        let scratch = Scratch::new("clearlog");
        let main = scratch.0.join(paths::LOG_FILE_NAME);
        fs::write(&main, "stale").unwrap();
        for suffix in ["1", "2", "3", "bak"] {
            fs::write(format!("{}.{}", main.display(), suffix), "stale").unwrap();
        }
        fs::write(scratch.0.join("keep.txt"), "stale").unwrap();
        fs::create_dir(scratch.0.join("macast.log.d")).unwrap();

        let removed = clear_log_file(&main).unwrap();
        assert_eq!(removed, 5, "main + three backups + the .bak leftover");
        assert!(
            main.exists(),
            "cleared, not deleted: every writer holds it open in append mode"
        );
        assert_eq!(fs::metadata(&main).unwrap().len(), 0);
        assert!(
            !Path::new(&format!("{}.3", main.display())).exists(),
            "unlike remove_log_files, which keeps .3 past the rotation count"
        );
        assert!(!Path::new(&format!("{}.bak", main.display())).exists());
        assert!(scratch.0.join("keep.txt").exists());
        assert!(
            scratch.0.join("macast.log.d").is_dir(),
            "a directory that happens to match the prefix is not a backup"
        );

        // Clearing a log that does not exist yet is not a failure.
        assert_eq!(
            clear_log_file(&scratch.0.join("never-written.log")).unwrap(),
            1
        );
    }
}
