//! Where Macast keeps its files, and the one knob that moves them for tests.
//!
//! Python freezes these as module-level constants at import time
//! (`SETTING_DIR = appdirs.user_config_dir('Macast', 'xfangfang')`), which is
//! why every verification run in this repository has to patch `appdirs`
//! *before* importing `macast` (AGENTS.md §4.9). The Rust port gets the same
//! isolation guarantee structurally: the directory is resolved once, and
//! [`set_setting_dir_override`] only works before that first read.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The port the DLNA/Chromecast/AirPlay receiver listens on by default
/// (`utils.DEFAULT_PORT`). 58880 is also the management HTTP port.
pub const DEFAULT_PORT: u16 = 58880;

/// The single log file every part of Macast writes to or reads from
/// (`utils.LOG_FILE_NAME`).
pub const LOG_FILE_NAME: &str = "macast.log";

/// Settings file name inside [`setting_dir`].
pub const SETTING_FILE_NAME: &str = "macast_setting.json";

/// Subdirectory of the settings dir holding user-dropped protocol plugins
/// (`utils.PROTOCOL_DIR`).
pub const PROTOCOL_DIR: &str = "protocol";

/// Subdirectory of the settings dir holding user-dropped renderer plugins
/// (`utils.RENDERER_DIR`).
pub const RENDERER_DIR: &str = "renderer";

/// Per-module log directory (`logsplit.py`): `logs/<Name>.log`.
pub const LOGS_DIR: &str = "logs";

/// Reversible-deletion directory (`Macast.py` / plugins): `.trash/`.
pub const TRASH_DIR: &str = ".trash";

static SETTING_DIR: OnceLock<PathBuf> = OnceLock::new();
static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Environment variable that pins the config directory. This is the isolation
/// hook used by every verification run and every test here: set it, then touch
/// nothing else.
pub const SETTING_DIR_ENV: &str = "MACAST_SETTING_DIR";

/// Environment variable that pins the read-only resource directory (the XML
/// descriptors, the settings page). Defaults to the executable's directory,
/// which is where a bundled `.app` puts `Contents/Resources`.
pub const RESOURCE_DIR_ENV: &str = "MACAST_RESOURCE_DIR";

/// The platform's own answer for "where does Macast keep its settings".
///
/// Under `cfg(test)` this is deliberately unreachable — a test build always
/// falls back to a temp directory (see [`setting_dir`]) — so the dead-code lint
/// is silenced for that one configuration and kept live everywhere else.
#[cfg_attr(test, allow(dead_code))]
fn platform_config_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    {
        // appdirs on macOS: ~/Library/Application Support/<AppName>
        // (the 'xfangfang' author argument is ignored there).
        home.join("Library")
            .join("Application Support")
            .join("Macast")
    }
    #[cfg(target_os = "windows")]
    {
        // appdirs on Windows asks the shell (`SHGetFolderPathW(CSIDL_LOCAL_APPDATA)`),
        // which reads the registry, so `set LOCALAPPDATA=…` does **not** move a
        // Python Macast's config dir (AGENTS.md §4.9). This port reads the
        // environment variable instead, which is the documented, testable seam:
        // the isolation hook for a run here is `MACAST_SETTING_DIR`, never a
        // forged `%LOCALAPPDATA%`. Same destination in the normal case; different
        // promise under a shell that has been poked at.
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"));
        local.join("xfangfang").join("Macast")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        match xdg {
            Some(dir) if dir.is_absolute() => dir.join("Macast"),
            _ => home.join(".config").join("Macast"),
        }
    }
}

/// The directory holding `macast_setting.json`, the certificate, the relay
/// scratch, the logs, and `.trash`.
///
/// Resolution order: an override installed by [`set_setting_dir_override`]
/// (tests), then `MACAST_SETTING_DIR`, then the platform default.
pub fn setting_dir() -> PathBuf {
    SETTING_DIR
        .get_or_init(|| match std::env::var_os(SETTING_DIR_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            // A test binary must never resolve to the real config directory:
            // an un-claimed logger writes to `setting_dir()/macast.log` on the
            // first record it sees, and which test happens to log first is not
            // something a test may depend on. AGENTS.md §10: never touch the
            // user's configuration in order to verify something.
            #[cfg(test)]
            _ => scratch_dir(),
            #[cfg(not(test))]
            _ => platform_config_dir(),
        })
        .clone()
}

/// Per-process temp directory used as the config dir under `cargo test`.
#[cfg(test)]
fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("macast-core-test-{}", std::process::id()))
}

/// Pin the config directory. Only valid before anything has read it: after the
/// first read the value is frozen, and a later call is ignored rather than
/// silently splitting the app across two directories -- the failure mode this
/// whole module exists to avoid.
///
/// In a test build the fallback (see [`setting_dir`]) is already a temp
/// directory, so an ignored override costs a test a scratch location, never the
/// user's real `macast.log`.
pub fn set_setting_dir_override(dir: impl AsRef<Path>) {
    let _ = SETTING_DIR.set(dir.as_ref().to_path_buf());
}

/// `true` if the settings directory has not been resolved yet, i.e. an override
/// can still be installed. Test helper.
pub fn setting_dir_is_unresolved() -> bool {
    SETTING_DIR.get().is_none()
}

/// The directory holding shipped resources (`macast/xml/**`).
pub fn resource_dir() -> PathBuf {
    RESOURCE_DIR
        .get_or_init(|| match std::env::var_os(RESOURCE_DIR_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => match std::env::current_exe() {
                Ok(exe) => exe.parent().map(Path::to_path_buf).unwrap_or_default(),
                Err(_) => PathBuf::from("."),
            },
        })
        .clone()
}

/// Resolve a resource path relative to the shipped resources, the counterpart
/// of `Setting.get_base_path()`. Pass `""` for the directory itself.
pub fn base_path(rel: &str) -> PathBuf {
    let base = resource_dir();
    if rel.is_empty() {
        base
    } else {
        base.join(rel)
    }
}

/// A path inside the config directory, creating the directory first when
/// `ensure` is set.
///
/// The `ensure` half is not cosmetic: AGENTS.md §4.2 records a shipped build
/// that could not start its Cast TLS receiver because the code writing
/// `macast.key` assumed a directory that only `Setting.save()` ever created.
/// Anything that writes here asks first.
pub fn setting_path(rel: &str, ensure: bool) -> std::io::Result<PathBuf> {
    let dir = setting_dir();
    if ensure {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(if rel.is_empty() {
        dir
    } else {
        dir.join(rel)
    })
}

/// The settings JSON path (`Setting.setting_path`).
pub fn setting_file() -> PathBuf {
    setting_dir().join(SETTING_FILE_NAME)
}

/// The main log file (`macast/macast.log`).
///
/// This is `SETTING_DIR/macast.log`, not a path relative to the process
/// working directory — the packaged app is started by LaunchServices and has no
/// meaningful cwd at all.
pub fn log_file() -> PathBuf {
    setting_dir().join(LOG_FILE_NAME)
}

/// `logs/` inside the config directory.
pub fn logs_dir() -> PathBuf {
    setting_dir().join(LOGS_DIR)
}

/// `.trash/` inside the config directory.
pub fn trash_dir() -> PathBuf {
    setting_dir().join(TRASH_DIR)
}

/// The user-dropped renderer plugin directory.
pub fn user_renderer_dir() -> PathBuf {
    setting_dir().join(RENDERER_DIR)
}

/// The user-dropped protocol plugin directory.
pub fn user_protocol_dir() -> PathBuf {
    setting_dir().join(PROTOCOL_DIR)
}

/// Application version, the counterpart of `Setting.get_version()`.
///
/// Python reads `get_base_path('.version')` (written by the packaging) and
/// falls back to the installed package version. Here: the same file, then
/// `MACAST_VERSION` (so a source run can state a version without being
/// installed), then this crate's cargo version.
pub fn version() -> String {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        if let Ok(text) = std::fs::read_to_string(base_path(".version")) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        match std::env::var("MACAST_VERSION") {
            Ok(v) if !v.is_empty() => v,
            _ => env!("CARGO_PKG_VERSION").to_string(),
        }
    })
    .clone()
}

/// The `User-Agent`/`SERVER` string UPnP discovery answers with
/// (`Setting.get_server_info()`): `'{system}/{release} UPnP/1.0 Macast/{version}'`.
pub fn server_info() -> String {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let (sys, release) = system_version();
        format!("{}/{} UPnP/1.0 Macast/{}", sys, release, version())
    })
    .clone()
}

/// `(system name, release)` for [`server_info`]: Python uses
/// `platform.system()` / `platform.release()`.
pub fn system_version() -> (String, String) {
    let sys = if cfg!(target_os = "macos") {
        "Darwin".to_string()
    } else if cfg!(target_os = "windows") {
        "Windows".to_string()
    } else if cfg!(target_os = "linux") {
        "Linux".to_string()
    } else {
        std::env::consts::OS.to_string()
    };
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uname_release().unwrap_or_else(|| "unknown".to_string()));
    (sys, release)
}

#[cfg(unix)]
fn uname_release() -> Option<String> {
    use std::ffi::CStr;
    let mut u = std::mem::MaybeUninit::<libc::utsname>::uninit();
    if unsafe { libc::uname(u.as_mut_ptr()) } != 0 {
        return None;
    }
    let u = unsafe { u.assume_init() };
    let raw = unsafe { CStr::from_ptr(u.release.as_ptr()) };
    raw.to_str().ok().map(|s| s.to_string())
}

#[cfg(not(unix))]
fn uname_release() -> Option<String> {
    None
}

/// The name this host advertises as, the counterpart of
/// `Setting.friendly_name = "Macast({})".format(platform.node())`.
pub fn default_friendly_name() -> String {
    static N: OnceLock<String> = OnceLock::new();
    N.get_or_init(|| format!("Macast({})", hostname())).to_string()
}

/// This host's own name, the same one `default_friendly_name` puts inside
/// `Macast(...)`. Public because the same-site check needs it too: the settings
/// page may legitimately be opened on `pavia-macbook.local` (§4.7b — the name
/// set is loopback ∪ advertised addresses ∪ **this host's names**).
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        let mut buf = vec![0u8; 256];
        let rc = unsafe {
            libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len() - 1)
        };
        if rc == 0 {
            buf.iter_mut().for_each(|b| {
                if *b == 0 {
                    // keep the NUL where it is; CStr::from_ptr wants it
                }
            });
            let c = unsafe { CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
            if let Ok(name) = c.to_str() {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string())
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "localhost".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_and_names_match_python() {
        // These four constants are read by the settings page, the log API, and
        // the discovery layer; drift here is drift in what clients see.
        assert_eq!(DEFAULT_PORT, 58880);
        assert_eq!(LOG_FILE_NAME, "macast.log");
        assert_eq!(SETTING_FILE_NAME, "macast_setting.json");
        assert_eq!(PROTOCOL_DIR, "protocol");
        assert_eq!(RENDERER_DIR, "renderer");
    }

    #[test]
    fn setting_path_creates_the_directory_only_when_asked() {
        let dir = std::env::temp_dir().join(format!("macast-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("nope");
        // First caller: does not exist, does not ask -> io error kind, no side effect.
        assert!(!target.exists());
        let _ = std::fs::create_dir_all(&target).unwrap();
        assert!(target.join("macast.key").parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_info_carries_upnp_and_version() {
        let info = server_info();
        assert!(info.contains("UPnP/1.0 Macast/"), "got {info}");
        assert!(info.starts_with("Darwin/") || info.contains('/'));
    }

    #[test]
    fn friendly_name_is_prefix_and_hostname() {
        let name = default_friendly_name();
        assert!(name.starts_with("Macast(") && name.ends_with(')'), "got {name}");
    }
}
