//! Automation hooks: a shell command of the user's own, run at each playback
//! transition.
//!
//! Port of `macast/plugins/renderer/hooks.py`. Four keys in the settings file, one
//! per event, each holding a command:
//!
//! | key | event |
//! |---|---|
//! | `Hook_On_Cast` | `cast` — a new URL was handed to the player |
//! | `Hook_On_Pause` | `pause` |
//! | `Hook_On_Resume` | `resume` |
//! | `Hook_On_Stop` | `stop`, **including the stop that precedes a new cast** |
//!
//! Every hook gets the same three environment variables — `MACAST_EVENT`,
//! `MACAST_URL`, `MACAST_TITLE` — and **the URL is never interpolated into the
//! command**. That is the whole of the safety argument and it has two halves:
//!
//! * the command comes from the user's **own settings file**, so it carries the same
//!   trust as their shell profile. Running it through a shell is the point of the
//!   feature (`notify-send … && osascript …`), not a shortcut;
//! * the *URL* comes from the **network** — a phone sends it. If it were pasted into
//!   the command string, a cast to `$(rm -rf ~)` would be remote code execution with
//!   the user's privileges. It travels as an environment variable instead, where the
//!   shell never parses it.
//!
//! This is why AGENTS.md §4.7b's `GATE_CODE` family includes `set-module-setting`:
//! writing one of these keys **is** installing a command that runs on the next cast.
//!
//! Two more rules, both from the reference's own comment:
//!
//! * **spawned, not waited on.** A hook that takes ten seconds must not stall the
//!   worker answering the phone. `spawn` returns as soon as the process exists.
//! * **stdin/stdout/stderr are all `/dev/null`.** A hook that writes to stdout would
//!   otherwise fill a pipe nobody reads and eventually block; and on Windows the
//!   windowed build has no stdout at all (AGENTS.md §4.2).
//!
//! ## What this module deliberately does *not* do
//!
//! The reference reads the command with `Setting.get(prop, '')`, which **creates the
//! key** on first read, and its comment says that is on purpose — a menu item cannot
//! ask for text input, so the keys appearing in the JSON editor is how the feature is
//! discovered. This port reads with [`Setting::peek`] instead and does **not** create
//! them: the 高级设置 editor here lists whatever the file holds, and inventing four
//! empty keys on every boot would be four rows of noise for a user who never asked
//! for hooks. Discovery is the settings page's job, not a side effect of firing an
//! event that has no command anyway.

use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::logging::Logger;
use crate::setting::{Key, Setting};

/// One event, and the key that holds its command.
///
/// The order is the reference's `HOOKS` tuple (`hooks.py:50-55`), and a table rather
/// than four constants because [`event_keys`] and the settings-page labels both walk
/// it — one owner for the mapping.
pub const EVENTS: [(&str, Key); 4] = [
    ("cast", Key::HookOnCast),
    ("pause", Key::HookOnPause),
    ("resume", Key::HookOnResume),
    ("stop", Key::HookOnStop),
];

/// The key for `event`, or `None` for a name no hook answers.
///
/// An unknown event is **not** an error worth a log line: `run` is called from the
/// player's own transitions, and the only way to reach this with a bad name is a
/// programming mistake in this crate.
pub fn key_for(event: &str) -> Option<Key> {
    EVENTS
        .iter()
        .find(|(name, _)| *name == event)
        .map(|(_, key)| *key)
}

/// The configured command for `event`, or `""`.
///
/// Reads with [`Setting::peek`] — see the module doc for why this port does not
/// create the key the way the reference does.
pub fn command_for(setting: &Setting, event: &str) -> String {
    let Some(key) = key_for(event) else {
        return String::new();
    };
    match setting.peek(key) {
        Some(serde_json::Value::String(text)) => text,
        // A number or a bool in that slot is somebody's hand-edit; running `1` as a
        // shell command would be a surprising answer, so it counts as "not configured".
        _ => String::new(),
    }
}

/// Run the hook for `event` **without blocking the caller**.
///
/// Returns whether a process was started. Three ways that can be `false`, and only
/// one of them is worth a warning:
///
/// * no command configured — the normal case, silent;
/// * the spawn itself failed (no `/bin/sh`, a fork limit) — one `error` line naming
///   the event and the reason;
/// * unknown event name — silent, see [`key_for`].
///
/// The `url` and `title` go into the environment, never into the command string.
pub fn run(log: &Arc<Logger>, setting: &Setting, event: &str, url: &str, title: &str) -> bool {
    let command = command_for(setting, event);
    if command.is_empty() {
        return false;
    }
    let spawned = Command::new(shell_program())
        .arg(shell_flag())
        .arg(&command)
        .env("MACAST_EVENT", event)
        .env("MACAST_URL", url)
        .env("MACAST_TITLE", title)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => {
            // The command is logged; the url is **not**, because on a signed site link
            // it is a short-lived credential and `macast.log` is what a user pastes into
            // a bug report (AGENTS.md §4.14's cookie rule, applied to the same class of
            // secret). The event name is what makes the row useful.
            log.info(&format!(
                "{event} hook started (pid {}): {command}",
                child.id()
            ));
            true
        }
        Err(err) => {
            log.error(&format!("cannot run the {event} hook: {err}"));
            false
        }
    }
}

/// The shell to hand the command to.
///
/// `bash` on unix, not `sh`: the reference passes `shell=True`, which uses `/bin/sh`,
/// and on macOS `/bin/sh` is bash in POSIX mode — so `&&`, `||` and `$(…)` behave the
/// same. Naming `bash` here would change `[[ ]]` handling for a user whose hook uses
/// it, and this port's job is to run what the reference would have run.
#[cfg(not(windows))]
fn shell_program() -> &'static str {
    "/bin/sh"
}

/// `cmd.exe` on Windows, which is what Python's `shell=True` uses there.
#[cfg(windows)]
fn shell_program() -> &'static str {
    "cmd.exe"
}

/// `-c` for a POSIX shell, `/C` for `cmd.exe`.
#[cfg(not(windows))]
fn shell_flag() -> &'static str {
    "-c"
}

#[cfg(windows)]
fn shell_flag() -> &'static str {
    "/C"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{set_setting_dir_override, setting_dir};
    use serde_json::json;

    /// Every test pins the settings directory first (AGENTS.md §10: nothing here may
    /// write into the user's real configuration, and a `Logger` would).
    fn pin(tag: &str) -> (Arc<Setting>, Arc<Logger>) {
        let dir = std::env::temp_dir().join(format!(
            "macast-hooks-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        set_setting_dir_override(&dir);
        crate::logging::remove_log_files(&dir);
        let _ = setting_dir();
        // `Logger::named` already answers an `Arc` (one logger per name, shared).
        let log = Logger::named("Hooks");
        log.claim();
        (
            Arc::new(Setting::new(dir.join(format!("hooks-{tag}.json")))),
            log,
        )
    }

    #[test]
    fn the_four_events_are_the_reference_s_and_each_has_its_own_key() {
        pin("events");
        // The mapping is a table because two readers walk it; a mismatch here would show
        // up as "the pause hook ran on resume", which is silent and maddening.
        assert_eq!(EVENTS.len(), 4);
        let names: Vec<&str> = EVENTS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["cast", "pause", "resume", "stop"]);
        assert_eq!(key_for("cast"), Some(Key::HookOnCast));
        assert_eq!(key_for("pause"), Some(Key::HookOnPause));
        assert_eq!(key_for("resume"), Some(Key::HookOnResume));
        assert_eq!(key_for("stop"), Some(Key::HookOnStop));
        assert_eq!(key_for("nope"), None);
    }

    /// **A key nobody configured is not created by asking.** The reference deliberately
    /// creates it (a menu cannot prompt); this port does not, because the 高级设置
    /// editor would then show four rows of noise for a feature the user never touched.
    #[test]
    fn reading_a_hook_does_not_write_the_key() {
        let (setting, _log) = pin("no-create");
        assert_eq!(command_for(&setting, "cast"), "");
        assert!(
            setting.peek(Key::HookOnCast).is_none(),
            "asking for a hook command created the key — that is the reference's behaviour \
             and this port's deliberate divergence"
        );
    }

    #[test]
    fn a_hand_edited_number_is_not_a_command() {
        let (setting, _log) = pin("odd-type");
        setting.set(Key::HookOnCast, json!(1));
        assert_eq!(command_for(&setting, "cast"), "");
        setting.set(Key::HookOnCast, json!("echo hi"));
        assert_eq!(command_for(&setting, "cast"), "echo hi");
    }

    #[test]
    fn an_unconfigured_event_spawns_nothing() {
        let (setting, log) = pin("no-command");
        assert!(!run(&log, &setting, "cast", "http://host/a", "t"));
    }

    /// The one behavioural test: a real process, a real environment, a real file.
    ///
    /// The URL is the interesting half — it is a **network** string, so this asserts it
    /// arrives as an environment value and never as part of the command (a shell would
    /// have expanded `$(…)`).
    #[cfg(not(windows))]
    #[test]
    fn a_hook_gets_the_three_variables_and_the_url_is_not_expanded() {
        let (setting, log) = pin("runs");
        let dir = std::env::temp_dir().join(format!("macast-hooks-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create out dir");
        let out = dir.join("hook.txt");
        let _ = std::fs::remove_file(&out);
        // `$(id)` and backticks in the URL must survive as literal text.
        let hostile = "http://host/$(touch /tmp/macast-hook-pwned)`id`";
        setting.set(
            Key::HookOnCast,
            json!(format!(
                "printf '%s|%s|%s' \"$MACAST_EVENT\" \"$MACAST_URL\" \"$MACAST_TITLE\" > {}",
                out.display()
            )),
        );
        assert!(run(&log, &setting, "cast", hostile, "Some Title"));
        // Spawned, not waited on: poll instead of sleeping a fixed time.
        let mut text = String::new();
        for _ in 0..400 {
            if let Ok(read) = std::fs::read_to_string(&out) {
                if !read.is_empty() {
                    text = read;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(text, format!("cast|{hostile}|Some Title"));
        assert!(
            !std::path::Path::new("/tmp/macast-hook-pwned").exists(),
            "the URL was expanded by the shell — it must only ever be an environment value"
        );
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir(&dir);
    }

    /// A hook that outlives its caller must not block: `run` returns as soon as the
    /// process exists. Measured rather than reasoned — the reference's comment says the
    /// same ("must not stall the CherryPy worker") and this is what pins it here.
    #[cfg(not(windows))]
    #[test]
    fn a_slow_hook_does_not_delay_the_caller() {
        let (setting, log) = pin("slow");
        setting.set(Key::HookOnStop, json!("sleep 5"));
        let started = std::time::Instant::now();
        assert!(run(&log, &setting, "stop", "", ""));
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_millis(500),
            "run() waited for the hook ({took:?}) — it must spawn and return"
        );
    }

    /// The start line names the event and the pid, and **not the url**.
    ///
    /// Two halves, both deliberate: the pid is what makes the row useful when a user asks
    /// "did it run?", and the url is left out because on a signed site link it is a
    /// short-lived credential and `macast.log` is what gets pasted into a bug report
    /// (AGENTS.md §4.14's rule for cookies, applied to the same class of secret).
    #[cfg(not(windows))]
    #[test]
    fn the_start_line_names_the_event_and_the_pid_but_not_the_url() {
        let (setting, log) = pin("start-line");
        let path = log.claim();
        setting.set(Key::HookOnCast, json!("true"));
        let secret_url = "http://cdn.example/secret-token-abc123";
        assert!(run(&log, &setting, "cast", secret_url, ""));
        let mut text = String::new();
        for _ in 0..400 {
            text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("hook started") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(text.contains("cast hook started"), "no start line: {text}");
        assert!(text.contains("pid"), "the start line must name the process: {text}");
        assert!(
            !text.contains("secret-token-abc123"),
            "the url reached the log — it can be a credential: {text}"
        );
    }
}
