//! The environment a child process inherits, and the windows it must not open.
//!
//! Two facts from the Python implementation, both of which were paid for with a
//! user-visible failure (AGENTS.md §4.1, §4.2, §4.3):
//!
//! 1. **A proxy must not reach the player.** Macast is a *receiver*: the media
//!    URL it plays is usually served by the sender on the local network, and an
//!    HTTP proxy has no route back to it. Inheriting `http_proxy` makes mpv ask
//!    the proxy for a LAN address, the proxy answers 502, and the cast connects
//!    and then plays nothing at all — a failure that reads like a protocol bug
//!    and is entirely environmental. The stripping applies to the *player* only;
//!    Macast's own HTTP calls (plugin downloads, update checks, the extras zip)
//!    still honour the user's proxy.
//! 2. **On Windows a console child of a windowed parent gets its own console
//!    window.** The packaged build is `--noconsole`, so every helper — ffmpeg,
//!    ffprobe, yt-dlp, taskkill, the user's own hooks — would flash a black box
//!    on the desktop. One window per call, and a mirror session is made of many.
//!
//! The pure halves here (`player_env_from`, `hidden_flags`) take their inputs as
//! arguments so they can be tested on any machine, which is the same reason
//! Python's `hidden_flags` is parameterised on `platform`.
//!
//! **One deliberate difference from Python**: `install_hidden_popen()` wraps the
//! one class that every `subprocess` entry point resolves through, so a single
//! assignment covers the whole program. Rust has no such shared object — a
//! [`std::process::Command`] carries its own flags — so coverage here is
//! [`hide_console`] / [`player_command`], and "a spawn that forgot the flag" is a
//! call-site audit (the shape AGENTS.md §4.2's Part 62 G-section uses), not a
//! thing this module can guarantee by construction. That gap is written down
//! rather than papered over.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::Command;

use crate::logging::Logger;

/// Proxy variables a child process must not inherit. Verbatim from
/// `Setting.PROXY_ENV_VARS`, **including its order**: the "we ignored these"
/// line is reported in this order, not in the order the environment happened to
/// list them, and a test that sorted either side would hide a real difference.
pub const PROXY_ENV_VARS: [&str; 10] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "ftp_proxy",
    "FTP_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// `subprocess.CREATE_NO_WINDOW` — the win32 bit that keeps a console child from
/// being handed a window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `LD_LIBRARY_PATH`, and the name PyInstaller stores the real value under.
/// A frozen build overwrites the first with its own bundle directory; a player
/// launched from that build then loads the wrong shared libraries unless the
/// original is handed back.
pub const LIBRARY_PATH_KEY: &str = "LD_LIBRARY_PATH";
pub const LIBRARY_PATH_ORIG_KEY: &str = "LD_LIBRARY_PATH_ORIG";

/// `subprocess.NO_WINDOW`: the bit, or 0 where no such thing exists.
pub const NO_WINDOW: u32 = if cfg!(windows) { CREATE_NO_WINDOW } else { 0 };

/// `sys.platform`, as far as this module cares: only `"win32"` has a bit to add.
pub fn sys_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// The no-window bit for a named platform. Separate from [`NO_WINDOW`] because
/// the suite runs on macOS and a constant cannot be asked about a machine it is
/// not on.
pub fn no_window_bit(platform: &str) -> u32 {
    if platform == "win32" {
        CREATE_NO_WINDOW
    } else {
        0
    }
}

/// `creationflags` with the no-console-window bit **added, never replaced**: a
/// caller that already asked for something keeps it.
pub fn hidden_flags(flags: u32, no_window: u32) -> u32 {
    flags | no_window
}

/// The same, resolving the bit from a platform string the way Python's default
/// argument does — including the `platform or sys.platform` fallback, so an
/// empty string means "ask this machine" rather than "there is no bit".
pub fn hidden_flags_for(flags: u32, platform: &str) -> u32 {
    let platform = if platform.is_empty() { sys_platform() } else { platform };
    hidden_flags(flags, no_window_bit(platform))
}

/// What the player's environment looks like: the variables to hand it, and the
/// proxy names that were taken out (in [`PROXY_ENV_VARS`] order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerEnv {
    /// Sorted by name — see [`player_env_from`] for why the order is not the
    /// input's.
    pub vars: BTreeMap<String, String>,
    pub ignored_proxies: Vec<String>,
}

/// Pure half of [`player_env`]: take a snapshot of the environment, hand
/// `LD_LIBRARY_PATH` back to whoever owned it before the bundle, and drop every
/// proxy variable.
///
/// Two subtleties that are the whole point of this function:
///
/// - **Present-but-empty still counts.** `http_proxy=""` is not `None` in
///   Python, so it is reported as ignored and removed. An empty proxy is still
///   a variable the child inherits, and the reason for reporting is that the
///   user needs to know a proxy was in play at all.
/// - **`LD_LIBRARY_PATH_ORIG` itself stays.** It is the user's variable, not
///   ours to clean up.
///
/// Input order is not preserved in `vars`: a map keyed by name is what
/// [`apply`] needs, and any duplicate name in the input resolves to its last
/// value (also what `dict(os.environ)` gives). The *report* of what was dropped
/// is order-independent by construction: it walks the table, not the input.
pub fn player_env_from<I, K, V>(raw: I) -> PlayerEnv
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in raw {
        vars.insert(k.into(), v.into());
    }

    match vars.remove(LIBRARY_PATH_ORIG_KEY) {
        Some(orig) => {
            vars.insert(LIBRARY_PATH_KEY.to_string(), orig.clone());
            // Put the marker back: it is the user's variable and it documents
            // where the value we just restored came from.
            vars.insert(LIBRARY_PATH_ORIG_KEY.to_string(), orig);
        }
        None => {
            // No original to restore: the bundled value goes — that is literally
            // what `env.pop(lp_key, None)` does. Leaving it would hand mpv /
            // ffmpeg / yt-dlp a `LD_LIBRARY_PATH` pointing into our own bundle,
            // which is the one thing this branch exists to prevent.
            vars.remove(LIBRARY_PATH_KEY);
        }
    }

    let mut ignored_proxies = Vec::new();
    for name in PROXY_ENV_VARS {
        if vars.remove(name).is_some() {
            ignored_proxies.push(name.to_string());
        }
    }
    PlayerEnv { vars, ignored_proxies }
}

/// Read this process's environment and turn it into the player's, logging the
/// proxy stripping the way `get_system_env()` does.
///
/// Variables whose name or value is not valid UTF-8 are dropped rather than
/// mangled: they can never be one of the ASCII proxy names, and `Command::envs`
/// would take them anyway while a lossy copy could silently invent a value.
pub fn player_env() -> PlayerEnv {
    let mut kept = 0usize;
    let mut skipped = 0usize;
    let mut raw: Vec<(String, String)> = Vec::new();
    for (k, v) in std::env::vars_os() {
        match (k.into_string(), v.into_string()) {
            (Ok(k), Ok(v)) => {
                kept += 1;
                raw.push((k, v));
            }
            _ => skipped += 1,
        }
    }
    let logger = Logger::named("Utils");
    if skipped > 0 {
        logger.debug(&format!(
            "Player env: {} entries not valid text were left out ({} kept)",
            skipped, kept
        ));
    }
    let result = player_env_from(raw);
    if !result.ignored_proxies.is_empty() {
        // Verbatim sentence from `Setting.get_system_env()`; the settings page
        // and the log-reading habits built on it quote it.
        logger.info(&format!(
            "Player will ignore proxy environment: {}",
            result.ignored_proxies.join(", ")
        ));
    }
    result
}

/// Point a command at a prepared environment: clear the inherited one, then set
/// exactly these variables.
///
/// `env_clear` first is what makes this a statement rather than a suggestion —
/// without it a stripped name could still be there from the parent, and the
/// whole reason for the list is that those names must not be reachable.
pub fn apply(command: &mut Command, env: &PlayerEnv) {
    command.env_clear();
    for (k, v) in &env.vars {
        command.env(k, v);
    }
}

/// Set a command's creation flags to `flags` plus the no-console-window bit.
///
/// **This assigns, it does not merge with flags already on the command** — and
/// that is a std limitation, not a choice: `Command` has no getter for creation
/// flags, so a caller that already set some cannot be read back. Hence the rule
/// for callers: the flags are decided once, by whoever builds the command, by
/// passing the full set here. Within this module that holds — [`player_command`]
/// builds from scratch — and the merge arithmetic itself lives in
/// [`hidden_flags`], where it is testable.
///
/// Compiles to nothing on non-Windows (no bit to set, no window to prevent), so
/// callers do not need their own `cfg`.
pub fn hide_console_with(command: &mut Command, flags: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(hidden_flags_for(flags, sys_platform()));
    }
    #[cfg(not(windows))]
    {
        let _ = (command, flags);
    }
}

/// [`hide_console_with`] with no flags of the caller's own.
pub fn hide_console(command: &mut Command) {
    hide_console_with(command, 0);
}

/// A command with the player's environment and no window of its own.
///
/// This is the shape every player / ffmpeg / ffprobe / yt-dlp spawn should use.
pub fn player_command(program: impl Into<OsString>) -> Command {
    let env = player_env();
    let mut command = Command::new(program.into());
    apply(&mut command, &env);
    hide_console(&mut command);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn proxy_table_is_the_ten_names_in_pythons_order() {
        assert_eq!(
            PROXY_ENV_VARS,
            [
                "http_proxy",
                "HTTP_PROXY",
                "https_proxy",
                "HTTPS_PROXY",
                "all_proxy",
                "ALL_PROXY",
                "ftp_proxy",
                "FTP_PROXY",
                "no_proxy",
                "NO_PROXY"
            ]
        );
    }

    #[test]
    fn every_proxy_shape_is_stripped() {
        let raw = env_pairs(&[
            ("PATH", "/usr/bin"),
            ("http_proxy", "http://127.0.0.1:7890"),
            ("HTTPS_PROXY", "http://127.0.0.1:7890"),
            ("all_proxy", "socks5://127.0.0.1:7890"),
            ("no_proxy", "localhost"),
            ("SOMEONES_OWN", "keep me"),
        ]);
        let result = player_env_from(raw);
        for name in PROXY_ENV_VARS {
            assert!(!result.vars.contains_key(name), "{} survived", name);
        }
        assert_eq!(result.vars.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(
            result.vars.get("SOMEONES_OWN").map(String::as_str),
            Some("keep me")
        );
    }

    /// The reported list is the *table's* order, and it is the only thing telling
    /// a reader which proxies were in play — so the order is a contract, not
    /// decoration.
    #[test]
    fn the_ignored_list_follows_the_table_not_the_environment() {
        let raw = env_pairs(&[
            ("NO_PROXY", "x"),
            ("HTTP_PROXY", "x"),
            ("http_proxy", "x"),
            ("ALL_PROXY", "x"),
        ]);
        let result = player_env_from(raw);
        assert_eq!(
            result.ignored_proxies,
            vec!["http_proxy", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY"]
        );
    }

    #[test]
    fn an_empty_proxy_is_still_a_proxy() {
        // `env.pop(k, None) is not None` — an empty string is not `None`, so it
        // counts and it is reported. Unsetting a proxy by setting it to "" is a
        // thing people do, and "nothing was in play" would be a wrong answer.
        let result = player_env_from(env_pairs(&[("http_proxy", "")]));
        assert_eq!(result.ignored_proxies, vec!["http_proxy"]);
        assert!(!result.vars.contains_key("http_proxy"));
    }

    #[test]
    fn nothing_to_strip_says_nothing() {
        let result = player_env_from(env_pairs(&[("PATH", "/bin")]));
        assert!(result.ignored_proxies.is_empty());
    }

    #[test]
    fn a_frozen_builds_library_path_is_handed_back() {
        let result = player_env_from(env_pairs(&[
            ("LD_LIBRARY_PATH", "/tmp/_MEI1234"),
            ("LD_LIBRARY_PATH_ORIG", "/opt/homebrew/lib"),
        ]));
        assert_eq!(
            result.vars.get(LIBRARY_PATH_KEY).map(String::as_str),
            Some("/opt/homebrew/lib")
        );
        // The marker is the user's variable: restoring with it gone would erase
        // the evidence of where the value came from.
        assert_eq!(
            result.vars.get(LIBRARY_PATH_ORIG_KEY).map(String::as_str),
            Some("/opt/homebrew/lib")
        );
    }

    #[test]
    fn without_an_original_the_bundled_path_goes() {
        let result = player_env_from(env_pairs(&[
            ("LD_LIBRARY_PATH", "/tmp/_MEI1234"),
            ("PATH", "/bin"),
        ]));
        assert!(!result.vars.contains_key(LIBRARY_PATH_KEY));
        assert!(result.vars.contains_key("PATH"));
    }

    #[test]
    fn an_empty_original_is_a_real_answer() {
        // `LD_LIBRARY_PATH_ORIG=""` means "it was unset", which is different
        // from "there is no original" only in that the child now sees an empty
        // value — and an empty search path is how you say "system defaults".
        let result = player_env_from(env_pairs(&[
            ("LD_LIBRARY_PATH", "/tmp/_MEI1234"),
            ("LD_LIBRARY_PATH_ORIG", ""),
        ]));
        assert_eq!(result.vars.get(LIBRARY_PATH_KEY).map(String::as_str), Some(""));
    }

    #[test]
    fn the_no_window_bit_exists_on_exactly_one_platform() {
        assert_eq!(no_window_bit("win32"), CREATE_NO_WINDOW);
        assert_eq!(no_window_bit("darwin"), 0);
        assert_eq!(no_window_bit("linux"), 0);
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        // And the constant agrees with the machine the tests run on, which on
        // this suite is never win32.
        assert_eq!(NO_WINDOW, no_window_bit(sys_platform()));
    }

    #[test]
    fn hidden_flags_merges_and_never_replaces() {
        assert_eq!(hidden_flags(0x0, CREATE_NO_WINDOW), CREATE_NO_WINDOW);
        assert_eq!(
            hidden_flags(0x0100, CREATE_NO_WINDOW),
            0x0100 | CREATE_NO_WINDOW
        );
        // A caller asking for the bit twice is not an error, and the bit must
        // still be there once.
        assert_eq!(
            hidden_flags(CREATE_NO_WINDOW, CREATE_NO_WINDOW),
            CREATE_NO_WINDOW
        );
        assert_eq!(hidden_flags_for(0x0100, "win32"), 0x0100 | CREATE_NO_WINDOW);
        assert_eq!(hidden_flags_for(0x0100, "darwin"), 0x0100);
        // Empty platform means "ask this machine", matching `platform or
        // sys.platform`; on this runner that answer is "no bit".
        assert_eq!(hidden_flags_for(0x0100, ""), 0x0100);
    }

    #[test]
    fn applying_the_env_replaces_the_inherited_one() {
        let env = player_env_from(env_pairs(&[
            ("PATH", "/bin"),
            ("MACAST_MARKER", "yes"),
            ("http_proxy", "http://127.0.0.1:7890"),
        ]));
        let mut command = Command::new("/usr/bin/true");
        // Poison the command first: `apply` must leave no trace of this.
        command.env("http_proxy", "http://poisoned.invalid");
        apply(&mut command, &env);

        let seen: Vec<(String, String)> = command
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
            })
            .collect();
        assert!(
            !seen.iter().any(|(k, _)| k == "http_proxy"),
            "a proxy name survived: {:?}",
            seen
        );
        assert!(seen.iter().any(|(k, v)| k == "PATH" && v == "/bin"));
        assert!(seen.iter().any(|(k, v)| k == "MACAST_MARKER" && v == "yes"));
        // And the inherited environment really was cleared: whatever this test
        // binary has in its own environment is not in the command.
        if std::env::var_os("HOME").is_some() {
            assert!(!seen.iter().any(|(k, _)| k == "HOME"));
        }
    }

    #[test]
    fn player_env_reads_the_real_table_and_reports_what_it_dropped() {
        // Only the self-consistency is asserted, because what is in this
        // process's environment belongs to whoever ran the tests (AGENTS.md
        // §4.2: a judgement may not depend on the machine running it).
        std::env::set_var("macast_env_test_proxy", "http://127.0.0.1:1");
        let env = player_env();
        std::env::remove_var("macast_env_test_proxy");
        for name in PROXY_ENV_VARS {
            assert!(!env.vars.contains_key(name));
        }
        for name in &env.ignored_proxies {
            assert!(PROXY_ENV_VARS.contains(&name.as_str()));
            assert!(!env.vars.contains_key(name));
        }
    }
}
