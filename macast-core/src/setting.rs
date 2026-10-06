//! The settings table: one JSON file, read defensively, never overwritten blind.
//!
//! This is the Rust `macast/utils.py::Setting`. Two of this repository's most
//! expensive lessons live in here, and both are why this module is written the
//! way it is:
//!
//! 1. **A file we cannot parse is moved aside intact before defaults are
//!    considered** (AGENTS.md §4.2). A PowerShell-written BOM once made the
//!    reader fail, the code logged the error and started from defaults, and the
//!    next save replaced 36 user keys with 11 — including `Api_Token`, which
//!    silently invalidated every configured phone Shortcut. So: read with
//!    BOM-tolerance, reject a non-object, and *rename* the unreadable file out
//!    of the way. A rename failure says so rather than claiming a copy exists.
//! 2. **`get` has a side effect** (AGENTS.md §4.2 first bullet): reading a
//!    missing key stores the default and the next save persists it. Callers that
//!    only want to know whether a key exists use [`Setting::has`] /
//!    [`Setting::peek`], and a migration that wants the key gone uses
//!    [`Setting::unset`] — `set(key, null)` would persist a JSON `null`, which
//!    reads back as present-but-empty.
//!
//! The keys are spelled exactly as the Python `SettingProperty` enum names
//! because this file is shared data: an installed 0.25 user who runs the Rust
//! build must keep their token, their pinned interface, and their history.
//!
//! No sockets and no processes in here, same rule as the rest of the crate, so
//! every one of those properties is checkable by a unit test rather than by a
//! reproduction on a real machine.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::{logging, paths, timefmt, token};

/// A persisted settings key, named for the Python enum member it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Usn,
    CheckUpdate,
    StartAtLogin,
    MenubarIcon,
    ApplicationPort,
    DlnaFriendlyName,
    MacastRenderer,
    MacastProtocol,
    BlockedInterfaces,
    AdditionalInterfaces,
    PlayHistory,
    HttpsEnabled,
    HttpsPort,
    HttpsCert,
    HttpsKey,
    MacastProtocols,
    NetworkInterface,
    DisabledPlugins,
    ApiToken,
    GithubCnMirror,
    ResolveCookieBrowser,
    // The five keys `macast_renderer/mpv.py` owns. They are here rather than in
    // that crate because this file is one table with one writer: a player key
    // declared on the far side of the crate boundary would be a second source
    // for its own JSON spelling, and the whole point of `name()` is that the
    // spelling is not up for redecision.
    PlayerHw,
    PlayerSize,
    PlayerPosition,
    PlayerOntop,
    PlayerDefaultVolume,
}

impl Key {
    /// The exact JSON key this is stored under — `SettingProperty.<name>`.
    pub fn name(self) -> &'static str {
        match self {
            Key::Usn => "USN",
            Key::CheckUpdate => "CheckUpdate",
            Key::StartAtLogin => "StartAtLogin",
            Key::MenubarIcon => "MenubarIcon",
            Key::ApplicationPort => "ApplicationPort",
            Key::DlnaFriendlyName => "DLNA_FriendlyName",
            Key::MacastRenderer => "Macast_Renderer",
            Key::MacastProtocol => "Macast_Protocol",
            Key::BlockedInterfaces => "Blocked_Interfaces",
            Key::AdditionalInterfaces => "Additional_Interfaces",
            Key::PlayHistory => "Play_History",
            Key::HttpsEnabled => "Https_Enabled",
            Key::HttpsPort => "Https_Port",
            Key::HttpsCert => "Https_Cert",
            Key::HttpsKey => "Https_Key",
            Key::MacastProtocols => "Macast_Protocols",
            Key::NetworkInterface => "Network_Interface",
            Key::DisabledPlugins => "Disabled_Plugins",
            Key::ApiToken => "Api_Token",
            Key::GithubCnMirror => "Github_CN_Mirror",
            Key::ResolveCookieBrowser => "Resolve_Cookie_Browser",
            Key::PlayerHw => "PlayerHW",
            Key::PlayerSize => "PlayerSize",
            Key::PlayerPosition => "PlayerPosition",
            Key::PlayerOntop => "PlayerOntop",
            Key::PlayerDefaultVolume => "PlayerDefaultVolume",
        }
    }

    /// Reverse of [`Key::name`], for reading a file written by another build.
    pub fn from_name(name: &str) -> Option<Key> {
        (Key::all()).iter().find(|k| k.name() == name).copied()
    }

    /// Every key, in declaration order.
    pub fn all() -> [Key; 26] {
        [
            Key::Usn,
            Key::CheckUpdate,
            Key::StartAtLogin,
            Key::MenubarIcon,
            Key::ApplicationPort,
            Key::DlnaFriendlyName,
            Key::MacastRenderer,
            Key::MacastProtocol,
            Key::BlockedInterfaces,
            Key::AdditionalInterfaces,
            Key::PlayHistory,
            Key::HttpsEnabled,
            Key::HttpsPort,
            Key::HttpsCert,
            Key::HttpsKey,
            Key::MacastProtocols,
            Key::NetworkInterface,
            Key::DisabledPlugins,
            Key::ApiToken,
            Key::GithubCnMirror,
            Key::ResolveCookieBrowser,
            Key::PlayerHw,
            Key::PlayerSize,
            Key::PlayerPosition,
            Key::PlayerOntop,
            Key::PlayerDefaultVolume,
        ]
    }
}

/// The whole user-visible port choice: the HTTP port, and HTTPS at +1.
///
/// `+1` is not a convention we invented — the settings page states it and the
/// Python server computes it the same way, so it lives beside the key that can
/// override it rather than in both readers.
pub fn application_port(setting: &Setting) -> u16 {
    setting
        .get(Key::ApplicationPort, Value::from(paths::DEFAULT_PORT as i64))
        .as_i64()
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(paths::DEFAULT_PORT)
}

pub fn https_port(setting: &Setting) -> u16 {
    setting
        .get(Key::HttpsPort, Value::from(0i64))
        .as_i64()
        .filter(|v| *v > 0)
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(application_port(setting) + 1)
}

pub fn https_enabled(setting: &Setting) -> bool {
    setting
        .get(Key::HttpsEnabled, Value::Bool(false))
        .as_bool()
        .unwrap_or(false)
}

/// "启用国内镜像地址": every GitHub URL goes through a domestic mirror.
///
/// Absent key means off; a *false* value is not stored, the Python side unsets
/// the key instead (AGENTS.md §4.6), so the file never grows a key whose only
/// meaning is "the default".
pub fn github_cn_mirror(setting: &Setting) -> bool {
    match setting.peek(Key::GithubCnMirror) {
        Some(v) => matches!(v.as_str(), Some("1") | Some("true") | Some("True"))
            || v.as_bool() == Some(true),
        None => false,
    }
}

/// One read of the settings table.
///
/// [`Load::Quarantined`] is not "the user has no settings" — it is "the file is
/// there and we could not read it", and the caller must not save over it. The
/// `None` payload is the worst case on purpose: the rename itself failed, so the
/// original bytes are still under the real path.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// No file at all: start from defaults, nothing to protect.
    Missing,
    Read(serde_json::Map<String, Value>),
    /// Moved aside intact; the path it went to, or `None` when even that moved.
    Quarantined(Option<PathBuf>),
}

/// The settings table plus the file it came from.
pub struct Setting {
    path: PathBuf,
    values: Mutex<BTreeMap<String, Value>>,
    /// `Setting.temp_friendly_name` — a process-lifetime override that is never
    /// written to disk (the e2e smoke run uses it so the LAN sees a distinct
    /// device without editing the user's config).
    temp_friendly_name: Mutex<Option<String>>,
}

impl Setting {
    /// An empty table bound to `path`; nothing is read until [`Setting::load`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Setting {
            path: path.into(),
            values: Mutex::new(BTreeMap::new()),
            temp_friendly_name: Mutex::new(None),
        }
    }

    /// `Setting.load_default()`: the file at [`paths::setting_file`], loaded now.
    pub fn load_default() -> Self {
        let setting = Setting::new(paths::setting_file());
        setting.load();
        setting
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn table(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Value>> {
        self.values.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read the file, once, and report what happened.
    ///
    /// Empty-table semantics match Python: an absent file and a file we had to
    /// move aside both leave the table empty, and every accessor re-attempts the
    /// load when it finds the table empty (Python's `if not bool(setting)`).
    /// That retry-on-empty is why a quarantine that leaves nothing behind does
    /// not loop: [`Setting::quarantine`] renames, so the next read sees no file
    /// at all and answers [`Load::Missing`].
    pub fn load(&self) -> Load {
        if !self.table().is_empty() {
            return Load::Read(self.snapshot());
        }
        logging::root().info("Load Setting");
        let text = match read_lenient(&self.path) {
            Some(t) => t,
            None => {
                self.replace(BTreeMap::new());
                return Load::Missing;
            }
        };
        let parsed: Result<Value, _> = serde_json::from_str(&text);
        match parsed {
            Ok(Value::Object(map)) if !map.is_empty() || self.looks_like_object(&text) => {
                let mut table = BTreeMap::new();
                for (k, v) in map {
                    table.insert(k, v);
                }
                self.replace(table);
                logging::root().info(&format!("Loaded settings from {:?}", self.path));
                Load::Read(self.snapshot())
            }
            Ok(other) => {
                let why = format!(
                    "顶层不是一个对象（读到的是 {}）",
                    match other {
                        Value::Null => "null",
                        Value::Bool(_) => "布尔值",
                        Value::Number(_) => "数字",
                        Value::String(_) => "字符串",
                        Value::Array(_) => "数组",
                        Value::Object(_) => "对象",
                    }
                );
                self.quarantine(&why)
            }
            Err(e) => self.quarantine(&format!("{}", e)),
        }
    }

    /// serde's `Value::Object` arm above cannot tell `{}` from `[]`-like text at
    /// a glance; this asks the question the Python `isinstance(loaded, dict)`
    /// asks, directly, so an empty *array* is quarantined rather than accepted.
    fn looks_like_object(&self, text: &str) -> bool {
        matches!(serde_json::from_str::<Value>(text), Ok(Value::Object(_)))
    }

    fn replace(&self, table: BTreeMap<String, Value>) {
        *self.table() = table;
    }

    /// Move an unreadable file aside, intact, and start from defaults.
    ///
    /// The suffix loop is load-bearing: the stamp has second granularity, a
    /// single start can reject three different broken files, and `rename`
    /// replaces — without the loop the second rejection erases the copy the
    /// first one saved, which is the only surviving record of someone's
    /// configuration.
    pub fn quarantine(&self, reason: &str) -> Load {
        let aside = unique_corrupt_path(&self.path, timefmt::now_secs());
        match fs::rename(&self.path, &aside) {
            Ok(()) => {
                self.replace(BTreeMap::new());
                logging::root().error(&format!(
                    "设置文件读不懂（{}），本次从默认设置开始。原文件已完整保留在 {}，可以把自己的配置从那里搬回来。",
                    reason,
                    aside.display()
                ));
                Load::Quarantined(Some(aside))
            }
            Err(e) => {
                self.replace(BTreeMap::new());
                logging::root().error(&format!(
                    "设置文件读不懂（{}），而且移不开（{}）：在修好它之前不要保存设置，否则默认值会覆盖 {}",
                    reason,
                    e,
                    self.path.display()
                ));
                Load::Quarantined(None)
            }
        }
    }

    /// The whole table as a JSON object, for the settings page's 高级设置 view.
    pub fn snapshot(&self) -> serde_json::Map<String, Value> {
        self.table()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// `Setting.get(key, default)` — **and its side effect**: a missing key
    /// takes the default into the table, where the next [`Setting::save`]
    /// persists it. Use [`Setting::peek`] when only presence matters.
    pub fn get(&self, key: Key, default: Value) -> Value {
        if self.table().is_empty() {
            self.load();
        }
        let mut table = self.table();
        if let Some(v) = table.get(key.name()) {
            return v.clone();
        }
        table.insert(key.name().to_string(), default.clone());
        default
    }

    /// Presence without the insert: `Setting.has`.
    pub fn has(&self, key: Key) -> bool {
        if self.table().is_empty() {
            self.load();
        }
        self.table().contains_key(key.name())
    }

    /// Read without writing anything into the table, as a string form.
    ///
    /// Returns the value rendered for text use (a bool becomes `"true"`/`"false"`,
    /// a list becomes its JSON text) so callers like the token generator can ask
    /// "is there already a token?" without deciding about JSON at all.
    pub fn peek(&self, key: Key) -> Option<Value> {
        if self.table().is_empty() {
            self.load();
        }
        self.table().get(key.name()).cloned()
    }

    pub fn get_str(&self, key: Key, default: &str) -> String {
        match self.get(key, Value::String(default.to_string())) {
            Value::String(s) => s,
            Value::Null => default.to_string(),
            other => other.to_string(),
        }
    }

    pub fn get_bool(&self, key: Key, default: bool) -> bool {
        match self.get(key, Value::Bool(default)) {
            Value::Bool(b) => b,
            Value::Null => default,
            other => other.as_str().map(|s| s == "true" || s == "1").unwrap_or(default),
        }
    }

    pub fn get_i64(&self, key: Key, default: i64) -> i64 {
        match self.get(key, Value::from(default)) {
            Value::Number(n) => n.as_i64().unwrap_or(default),
            Value::String(s) => s.trim().parse().unwrap_or(default),
            _ => default,
        }
    }

    /// A list of strings (`Blocked_Interfaces`, `Macast_Protocols`,
    /// `Disabled_Plugins`). Anything else reads as empty — the same answer a
    /// hand-edited JSON of the wrong shape gets in Python after the `or []`.
    pub fn get_str_list(&self, key: Key) -> Vec<String> {
        match self.peek(key) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `Setting.set(key, value)`: store **and** write the file.
    pub fn set(&self, key: Key, value: Value) {
        self.put(key, value);
        self.save();
    }

    /// Store in memory only. The public seam [`crate::token`] uses so that
    /// generating a token can persist exactly once rather than twice.
    pub fn put(&self, key: Key, value: Value) {
        self.table().insert(key.name().to_string(), value);
    }

    /// `Setting.unset(key)`: the key must be *gone*, not null. Saving happens
    /// whenever the key existed, even if its value was falsy.
    pub fn unset(&self, key: Key) {
        if self.table().is_empty() {
            self.load();
        }
        if self.table().remove(key.name()).is_some() {
            self.save();
        }
    }

    /// Replace the whole table from a parsed object (the 高级设置 save path),
    /// then write it.
    pub fn replace_all(&self, map: serde_json::Map<String, Value>) {
        let mut table = BTreeMap::new();
        for (k, v) in map {
            table.insert(k, v);
        }
        self.replace(table);
        self.save();
    }

    /// Write the file: UTF-8 (no BOM), keys sorted, four-space indent.
    ///
    /// The parent directory is created here because this is the one writer that
    /// has always done it — and because §4.2's certificate bug is exactly what
    /// happens when a writer assumes it.
    pub fn save(&self) {
        let text = self.serialise();
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        // Written through a temporary name and renamed, so a reader that opens
        // this file mid-save never sees a truncated one. Python writes in place;
        // the extra cost is one rename per settings change, and the failure it
        // prevents (a half-written 36-key file that then gets quarantined) is
        // precisely the one this module exists for.
        let tmp = self.path.with_extension("json.tmp");
        match write_then_rename(&tmp, &self.path, text.as_bytes()) {
            Ok(()) => {}
            Err(e) => logging::root().error(&format!("无法保存设置：{}（{}）", self.path.display(), e)),
        }
    }

    /// The exact bytes [`Setting::save`] would write.
    pub fn serialise(&self) -> String {
        let map: serde_json::Map<String, Value> = self.snapshot();
        match serde_json::to_string_pretty(&Value::Object(map)) {
            Ok(two_spaces) => reindent(&two_spaces),
            Err(_) => "{}".to_string(),
        }
    }

    /// Drop the in-memory table so the next read goes back to the file
    /// (`Setting.reload()`).
    pub fn reload(&self) {
        self.replace(BTreeMap::new());
        self.load();
    }

    /// `Setting.get_friendly_name()`: the DLNA name clients see, and the default
    /// mpv window title. The temporary override wins and is never persisted.
    pub fn friendly_name(&self) -> String {
        if let Some(temp) = self.temp_friendly_name.lock().unwrap_or_else(|e| e.into_inner()).clone()
        {
            if !temp.is_empty() {
                return temp;
            }
        }
        self.get(
            Key::DlnaFriendlyName,
            Value::String(paths::default_friendly_name()),
        )
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(paths::default_friendly_name)
    }

    pub fn set_temp_friendly_name(&self, name: Option<&str>) {
        *self.temp_friendly_name.lock().unwrap_or_else(|e| e.into_inner()) =
            name.map(|s| s.to_string());
    }

    /// `Setting.get_usn()`: the persisted device UUID. Generating a fresh one on
    /// every read would make every SSDP re-announce look like a new device, so
    /// insert-on-read is the mechanism that keeps it stable.
    pub fn usn(&self) -> String {
        self.get(Key::Usn, Value::String(token::uuid4()))
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    /// `Setting.get_usn(refresh=True)` — called when the advertised address
    /// changed, because UPnP peers cache a device by USN.
    pub fn refresh_usn(&self) -> String {
        let fresh = token::uuid4();
        self.set(Key::Usn, Value::String(fresh.clone()));
        fresh
    }
}

/// `<base>.corrupt-%Y%m%d-%H%M%S`, then `-1`, `-2`, … while the name is taken.
pub fn unique_corrupt_path(path: &Path, secs: i64) -> PathBuf {
    let stamp = timefmt::filename_stamp(secs, timefmt::local_offset_secs());
    let mut aside = path_display_with(path, &format!(".corrupt-{}", stamp));
    let mut suffix = 1;
    while aside.exists() {
        aside = path_display_with(path, &format!(".corrupt-{}-{}", stamp, suffix));
        suffix += 1;
    }
    aside
}

/// `path + suffix` without letting `push` interpret a `/` in the suffix.
fn path_display_with(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_string_lossy().into_owned();
    name.push_str(suffix);
    PathBuf::from(name)
}

/// Read a settings file tolerating the BOM some editors insist on adding.
fn read_lenient(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let body = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
        .unwrap_or(&bytes[..]);
    Some(String::from_utf8_lossy(body).into_owned())
}

fn write_then_rename(tmp: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::File::create(tmp)?;
    f.write_all(bytes)?;
    f.flush()?;
    drop(f);
    fs::rename(tmp, dest)
}

/// `json.dump(indent=4)` against serde's two-space pretty printer.
fn reindent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    for line in text.lines() {
        let trimmed = line.trim_start();
        let depth = (line.len() - trimmed.len()) / 2;
        out.push_str(&"    ".repeat(depth));
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "macast-core-setting-{}-{}-{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_micros())
                    .unwrap_or(0)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join(paths::SETTING_FILE_NAME)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn key_names_match_the_python_enum_exactly() {
        // Compatibility with a file a 0.25 Python install wrote: these strings
        // are the wire format, so one renamed member silently loses a user's
        // token or their pinned interface.
        let names: Vec<&str> = Key::all().iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            vec![
                "USN",
                "CheckUpdate",
                "StartAtLogin",
                "MenubarIcon",
                "ApplicationPort",
                "DLNA_FriendlyName",
                "Macast_Renderer",
                "Macast_Protocol",
                "Blocked_Interfaces",
                "Additional_Interfaces",
                "Play_History",
                "Https_Enabled",
                "Https_Port",
                "Https_Cert",
                "Https_Key",
                "Macast_Protocols",
                "Network_Interface",
                "Disabled_Plugins",
                "Api_Token",
                "Github_CN_Mirror",
                "Resolve_Cookie_Browser",
                "PlayerHW",
                "PlayerSize",
                "PlayerPosition",
                "PlayerOntop",
                "PlayerDefaultVolume",
            ]
        );
        for name in names {
            assert!(Key::from_name(name).is_some(), "{name} has no member");
        }
    }

    #[test]
    fn every_member_round_trips_through_its_json_name() {
        // The hand-written list above can only prove the names it knows about.
        // This one walks the enum itself, so it catches the two failures a
        // copy-of-a-list cannot: a member whose `name()` is spelled twice (the
        // reader would then resolve one JSON key to the wrong member), and a
        // member added to `all()` without its mapping being thought about.
        for key in Key::all() {
            assert_eq!(Key::from_name(key.name()), Some(key), "{}", key.name());
        }
        // The player table is the five keys mpv.py writes, no more: a sixth
        // here would be a name nothing persists, and one fewer would be a user
        // setting the Rust build reads as absent (so it re-stores the default).
        let player: Vec<&str> = Key::all()
            .iter()
            .map(|k| k.name())
            .filter(|n| n.starts_with("Player"))
            .collect();
        assert_eq!(
            player,
            vec![
                "PlayerHW",
                "PlayerSize",
                "PlayerPosition",
                "PlayerOntop",
                "PlayerDefaultVolume",
            ]
        );
    }

    #[test]
    fn get_inserts_the_default_and_save_persists_it() {
        let scratch = Scratch::new("insert-on-read");
        let setting = Setting::new(scratch.file());
        assert!(!scratch.file().exists());
        assert!(!setting.has(Key::CheckUpdate));
        assert_eq!(setting.get_bool(Key::CheckUpdate, true), true);
        // Still not on disk until something saves.
        assert!(!scratch.file().exists());
        setting.save();
        let text = fs::read_to_string(scratch.file()).unwrap();
        assert!(text.contains("\"CheckUpdate\": true"), "{text}");
    }

    #[test]
    fn save_is_sorted_four_space_and_bom_free() {
        let scratch = Scratch::new("shape");
        let setting = Setting::new(scratch.file());
        setting.set(Key::Usn, Value::String("u".into()));
        setting.set(Key::ApiToken, Value::String("t".into()));
        setting.set(Key::ApplicationPort, Value::from(58880i64));
        setting.save();
        let bytes = fs::read(scratch.file()).unwrap();
        assert!(
            !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
            "we must not be the tool that adds a BOM"
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\n    \"Api_Token\": \"t\","), "{text}");
        let at = text.find("\"Api_Token\"").unwrap();
        let po = text.find("\"ApplicationPort\"").unwrap();
        let us = text.find("\"USN\"").unwrap();
        assert!(at < po && po < us, "keys must be sorted: {text}");
    }

    #[test]
    fn a_bom_is_survivable() {
        let scratch = Scratch::new("bom");
        fs::write(
            scratch.file(),
            [
                &[0xEF, 0xBB, 0xBF][..],
                r#"{"Api_Token": "keepme", "ApplicationPort": 58880}"#.as_bytes(),
            ]
            .concat(),
        )
        .unwrap();
        let setting = Setting::new(scratch.file());
        assert_eq!(
            setting.load(),
            Load::Read(
                serde_json::from_str::<serde_json::Map<String, Value>>(
                    r#"{"Api_Token": "keepme", "ApplicationPort": 58880}"#
                )
                .unwrap()
            )
        );
        assert_eq!(setting.peek(Key::ApiToken).unwrap(), Value::String("keepme".into()));
    }

    /// The bug this module exists for, in both directions: the unreadable file
    /// must still be there afterwards, and the defaults must not have replaced
    /// it.
    #[test]
    fn unreadable_file_is_moved_aside_not_overwritten() {
        let scratch = Scratch::new("quarantine");
        fs::write(scratch.file(), "\u{feff}{not json at all").unwrap();
        let setting = Setting::new(scratch.file());
        let outcome = setting.load();
        let aside = match outcome {
            Load::Quarantined(Some(p)) => p,
            other => panic!("expected a quarantine, got {:?}", other),
        };
        assert!(!scratch.file().exists(), "the live path must be free");
        assert!(fs::read_to_string(&aside).unwrap().contains("not json"));
        assert_eq!(setting.peek(Key::ApiToken), None);

        // And a save now cannot touch the user's bytes: they are out of the way.
        setting.set(Key::CheckUpdate, Value::Bool(false));
        assert!(fs::read_to_string(&aside).unwrap().contains("not json"));
    }

    #[test]
    fn a_json_array_is_not_a_settings_file() {
        let scratch = Scratch::new("array");
        fs::write(scratch.file(), "[1, 2, 3]").unwrap();
        let setting = Setting::new(scratch.file());
        assert!(matches!(setting.load(), Load::Quarantined(Some(_))));
        assert!(!scratch.file().exists());
    }

    #[test]
    fn an_empty_object_is_a_legitimate_file() {
        let scratch = Scratch::new("empty-object");
        fs::write(scratch.file(), "{}").unwrap();
        let setting = Setting::new(scratch.file());
        assert!(matches!(setting.load(), Load::Read(_)));
        assert!(scratch.file().exists(), "nothing should have moved");
    }

    /// Two rejections inside the same stamp second must produce two copies: the
    /// first is the only surviving record of that user's configuration.
    #[test]
    fn same_second_quarantines_do_not_collide() {
        let scratch = Scratch::new("suffix");
        let dest = scratch.file();
        let a = unique_corrupt_path(&dest, 1_700_000_000);
        // `unique_corrupt_path` only *names* a free slot, so the test has to play
        // the part of the first rejection and actually put something there.
        fs::write(&a, br#"{"Api_Token": "theirs"}"#).unwrap();
        let b = unique_corrupt_path(&dest, 1_700_000_000);
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains(".corrupt-"));
        assert!(b.to_string_lossy().ends_with("-1"), "{a:?} then {b:?}");
        // The point of the suffix loop: the first copy is still readable.
        assert_eq!(
            fs::read_to_string(&a).unwrap(),
            r#"{"Api_Token": "theirs"}"#
        );
        assert!(!b.exists(), "the second name must be free to rename onto");
    }

    #[test]
    fn unset_removes_the_key_rather_than_nulling_it() {
        let scratch = Scratch::new("unset");
        let setting = Setting::new(scratch.file());
        setting.set(Key::ApiToken, Value::String("abc".into()));
        assert!(setting.has(Key::ApiToken));
        setting.unset(Key::ApiToken);
        assert!(!setting.has(Key::ApiToken));
        let text = fs::read_to_string(scratch.file()).unwrap();
        assert!(!text.contains("Api_Token"), "{text}");
        assert!(!text.contains("null"), "{text}");
    }

    #[test]
    fn reload_picks_up_another_writer() {
        let scratch = Scratch::new("reload");
        let setting = Setting::new(scratch.file());
        setting.set(Key::MenubarIcon, Value::from(1i64));
        fs::write(scratch.file(), r#"{"MenubarIcon": 7}"#).unwrap();
        assert_eq!(setting.get_i64(Key::MenubarIcon, 0), 1);
        setting.reload();
        assert_eq!(setting.get_i64(Key::MenubarIcon, 0), 7);
    }

    #[test]
    fn usn_is_sticky_across_reads_and_refresh_changes_it() {
        let scratch = Scratch::new("usn");
        let setting = Setting::new(scratch.file());
        let first = setting.usn();
        assert_eq!(first.len(), 36);
        assert_eq!(setting.usn(), first, "a new USN per read = a new device per announce");
        let second = setting.refresh_usn();
        assert_ne!(second, first);
        assert_eq!(setting.usn(), second);
    }

    #[test]
    fn friendly_name_falls_back_to_the_host_then_the_override() {
        let scratch = Scratch::new("friendly");
        let setting = Setting::new(scratch.file());
        assert!(setting.friendly_name().starts_with("Macast("));
        setting.set(Key::DlnaFriendlyName, Value::String("客厅的 Macast".into()));
        assert_eq!(setting.friendly_name(), "客厅的 Macast");
        setting.set_temp_friendly_name(Some("Macast E2E Smoke"));
        assert_eq!(setting.friendly_name(), "Macast E2E Smoke");
        assert_eq!(setting.peek(Key::DlnaFriendlyName).unwrap(), Value::String("客厅的 Macast".into()));
        setting.set_temp_friendly_name(None);
        assert_eq!(setting.friendly_name(), "客厅的 Macast");
    }

    #[test]
    fn ports_follow_the_documented_rule() {
        let scratch = Scratch::new("ports");
        let setting = Setting::new(scratch.file());
        assert_eq!(application_port(&setting), paths::DEFAULT_PORT);
        assert_eq!(https_port(&setting), paths::DEFAULT_PORT + 1);
        assert!(!https_enabled(&setting));
        setting.set(Key::ApplicationPort, Value::from(59000i64));
        assert_eq!(https_port(&setting), 59001);
        setting.set(Key::HttpsPort, Value::from(9443i64));
        assert_eq!(https_port(&setting), 9443);
    }

    #[test]
    fn mirror_switch_is_absent_not_false() {
        let scratch = Scratch::new("mirror");
        let setting = Setting::new(scratch.file());
        assert!(!github_cn_mirror(&setting));
        assert!(
            setting.peek(Key::GithubCnMirror).is_none(),
            "reading the switch must not create a key"
        );
        setting.set(Key::GithubCnMirror, Value::Bool(true));
        assert!(github_cn_mirror(&setting));
        setting.unset(Key::GithubCnMirror);
        assert!(!github_cn_mirror(&setting));
    }

    #[test]
    fn interface_lists_survive_wrong_shapes() {
        let scratch = Scratch::new("lists");
        let setting = Setting::new(scratch.file());
        assert!(setting.get_str_list(Key::BlockedInterfaces).is_empty());
        fs::write(scratch.file(), r#"{"Blocked_Interfaces": "en0"}"#).unwrap();
        setting.reload();
        assert!(
            setting.get_str_list(Key::BlockedInterfaces).is_empty(),
            "a bare string is not a list; treat it as no exclusions"
        );
        fs::write(scratch.file(), r#"{"Blocked_Interfaces": ["en0", "utun3"]}"#).unwrap();
        setting.reload();
        assert_eq!(
            setting.get_str_list(Key::BlockedInterfaces),
            vec!["en0".to_string(), "utun3".to_string()]
        );
    }

    #[test]
    fn saving_creates_the_directory_it_needs() {
        let scratch = Scratch::new("makedirs");
        let deep = scratch.0.join("nope").join("deeper").join("macast_setting.json");
        let setting = Setting::new(deep.clone());
        setting.set(Key::CheckUpdate, Value::Bool(true));
        assert!(deep.exists(), "a writer must build its own parent");
    }
}
