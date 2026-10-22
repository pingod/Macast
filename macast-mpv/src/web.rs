//! The browser renderer: hand the url to the user's browser.
//!
//! Rust counterpart of `macast/plugins/renderer/web.py`'s decision layer (`:87-127`).
//!
//! ## The failure that made this plugin worth writing down
//!
//! The comment at the top of the reference lists three defects it fixed, and the third is the
//! important one: **`xdg-open` was spawned without checking it exists**. The consequence was
//! not a crash — it was that the sender was told `PLAYING` for a browser that never opened,
//! and the failure was invisible everywhere. So "how do we open a browser" has to be a
//! decision that can *answer no*, with the reason, before anything reports success.
//!
//! ## Two small policies worth keeping exactly
//!
//! * **The env scrub drops variables whose value contains `tmp`** — a PyInstaller-era
//!   workaround (its issue #3668) for temp dirs that make a child browser fail to start.
//!   `PATH` is exempt, and so is any non-string value: the reference guards both, and the
//!   guard is not cosmetic — iterating and mutating the same map raised mid-loop before.
//! * **The clipboard is best-effort.** A headless box has no clipboard, and refusing to play
//!   over that would turn a convenience into a blocker. So the copy failure is reported and
//!   swallowed, while the *open* failure sets the transport state to error.
//!
//! ## Not here
//!
//! No spawning and no clipboard: both are seams the caller supplies. That keeps the test
//! deterministic and lets the shell decide how to run a child process.

/// Which command opens a url on this platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opener {
    /// macOS: `open <url>`.
    Open,
    /// Windows: the shell's default-browser API.
    ShellOpen,
    /// Everything else: `xdg-open <url>`, which may not be installed.
    XdgOpen(String),
}

/// Why a browser could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenFailure {
    /// `xdg-open` is not on this machine. Its own sentence, because the fix is "install
    /// `xdg-utils`", not "check your browser".
    NoXdgOpen,
    /// Anything else the spawner reported, verbatim — the reference's `str(e)`.
    Spawn(String),
}

impl OpenFailure {
    /// The sentence published to the user.
    ///
    /// Matches the reference's wording so the two apps say the same thing, and it carries the
    /// underlying reason because "could not open a browser" alone cannot be acted on.
    pub fn message(&self) -> String {
        match self {
            OpenFailure::NoXdgOpen => {
                "Could not open a browser: xdg-open is not installed".to_string()
            }
            OpenFailure::Spawn(reason) => format!("Could not open a browser: {reason}"),
        }
    }
}

/// Decide how to open a url, or why we cannot.
///
/// `platform` is an **input**, not `std::env::consts::OS` — the same seam rule as
/// `macast_core::paths::sys_platform` (AGENTS.md §4.8 Part 40): a test on Linux asking about
/// `darwin` must get `darwin`'s answer.
///
/// `which` is the caller's `PATH` lookup. It is only consulted on the Linux branch, and only
/// because that is the branch whose success depends on a binary being installed.
pub fn opener_for(platform: &str, which: impl Fn(&str) -> Option<String>) -> Result<Opener, OpenFailure> {
    match platform {
        "darwin" => Ok(Opener::Open),
        "win32" => Ok(Opener::ShellOpen),
        _ => match which("xdg-open") {
            Some(path) => Ok(Opener::XdgOpen(path)),
            // Not `Spawn("No such file")`: the user needs to know which program is missing.
            None => Err(OpenFailure::NoXdgOpen),
        },
    }
}

/// The argv for an opener that takes one.
///
/// `ShellOpen` has no argv — it goes through the platform API rather than a process, which is
/// why it is not a string here. Returning `None` for it is the honest answer.
pub fn opener_argv(opener: &Opener, url: &str) -> Option<Vec<String>> {
    match opener {
        Opener::Open => Some(vec!["open".to_string(), url.to_string()]),
        Opener::XdgOpen(path) => Some(vec![path.clone(), url.to_string()]),
        Opener::ShellOpen => None,
    }
}

/// Drop the environment variables that carry a temporary path.
///
/// Two exemptions, both load-bearing: `PATH` (dropping it makes every child fail to start)
/// and any non-string value (the reference iterates and mutates the same map, so a non-str
/// value used to raise mid-loop). The `tmp` test is on the **value**, and it mirrors the
/// reference's reason: PyInstaller-era temp dirs made a child browser refuse to start.
///
/// Returns the keys to remove rather than a new map, so the caller keeps its own environment
/// type (this crate does not own the process environment).
pub fn env_keys_to_scrub(env: &[(String, EnvValue)]) -> Vec<String> {
    env.iter()
        .filter(|(key, value)| key != "PATH" && value.is_tmp_path())
        .map(|(key, _)| key.clone())
        .collect()
}

/// An environment value, kept as the two shapes a real process environment can have.
///
/// This exists because the reference's guard is `isinstance(v, str)`, and collapsing the type
/// to `String` would make that guard **untestable** — the port would agree with the reference
/// on every input a test could construct, while disagreeing on the one input the guard was
/// written for (`screen_mirror`'s original crash: a non-string value in the environment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvValue {
    Text(String),
    /// Anything that is not a string. Never scrubbed, and never inspected.
    NotText,
}

impl EnvValue {
    fn is_tmp_path(&self) -> bool {
        match self {
            EnvValue::Text(text) => text.contains("tmp"),
            EnvValue::NotText => false,
        }
    }
}

/// What opening a url decided, so the caller knows whether to report success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The transport state must be set to error when this is false.
    pub ok: bool,
    /// A word for the user, or nothing when it worked.
    pub message: Option<String>,
    /// The clipboard copy failed, which is **not** a reason to refuse playing.
    pub clipboard_failed: Option<String>,
}

/// The whole policy: copy first (best effort), then open (and report failure).
///
/// Split from the effectful parts so the ordering is testable: the reference copies *before*
/// opening, which means that when the open fails the user still has the url on their
/// clipboard — a deliberate small mercy.
pub fn open_report(
    autocopy: bool,
    copy: impl FnOnce(&str) -> Result<(), String>,
    open: impl FnOnce(&str) -> Result<(), OpenFailure>,
    url: &str,
) -> Opened {
    let clipboard_failed = if autocopy {
        copy(url).err().map(|why| format!("Could not copy the url: {why}"))
    } else {
        None
    };
    match open(url) {
        Ok(()) => Opened {
            ok: true,
            message: None,
            clipboard_failed,
        },
        Err(failure) => Opened {
            ok: false,
            message: Some(failure.message()),
            clipboard_failed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, EnvValue)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), EnvValue::Text((*v).to_string())))
            .collect()
    }

    #[test]
    fn each_platform_gets_its_own_opener() {
        // The platform is an argument, so a Linux runner can ask about darwin and win32.
        assert_eq!(opener_for("darwin", |_| None), Ok(Opener::Open));
        assert_eq!(opener_for("win32", |_| None), Ok(Opener::ShellOpen));
        assert_eq!(
            opener_for("linux", |name| {
                assert_eq!(name, "xdg-open");
                Some("/usr/bin/xdg-open".to_string())
            }),
            Ok(Opener::XdgOpen("/usr/bin/xdg-open".to_string()))
        );
    }

    #[test]
    fn a_missing_xdg_open_is_its_own_failure_because_the_fix_is_its_own_package() {
        // "No such file" would send a user to look at their browser settings; the real fix is
        // `xdg-utils`. This is the reference's third listed defect.
        let failure = opener_for("linux", |_| None).expect_err("no xdg-open");
        assert_eq!(failure, OpenFailure::NoXdgOpen);
        assert!(failure.message().contains("xdg-open"));
        assert!(failure.message().contains("not installed"));
    }

    #[test]
    fn the_opener_argv_keeps_the_url_as_one_argument() {
        // Same safety property as the external player: the url arrives over the network.
        let hostile = "http://x/a.mp4; touch /tmp/pwned";
        let argv = opener_argv(&Opener::Open, hostile).expect("open takes an argv");
        assert_eq!(argv, vec!["open".to_string(), hostile.to_string()]);
        let argv = opener_argv(&Opener::XdgOpen("/usr/bin/xdg-open".into()), hostile)
            .expect("xdg-open takes an argv");
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], hostile);
        // The Windows path is a platform API, not a process, so there is no argv to get wrong.
        assert_eq!(opener_argv(&Opener::ShellOpen, hostile), None);
    }

    #[test]
    fn the_env_scrub_drops_tmp_paths_but_never_path_itself() {
        let scrubbed = env_keys_to_scrub(&env(&[
            ("PATH", "/usr/bin:/tmp/shim"),
            ("TMPDIR", "/var/folders/tmp/x"),
            ("HOME", "/Users/someone"),
            ("PYTHONPATH", "/tmp/x"),
        ]));
        // PATH contains `tmp` and must still survive: dropping it breaks every child.
        assert!(!scrubbed.contains(&"PATH".to_string()));
        assert!(scrubbed.contains(&"TMPDIR".to_string()));
        assert!(scrubbed.contains(&"PYTHONPATH".to_string()));
        assert!(!scrubbed.contains(&"HOME".to_string()));
    }

    #[test]
    fn a_non_string_value_is_never_inspected() {
        // The reference's crash: iterating and assuming every value is a string raised
        // mid-loop. A non-text value is therefore skipped, not coerced — and this test is why
        // `EnvValue` has two variants instead of being a `String`.
        let scrubbed = env_keys_to_scrub(&[
            ("PATH".to_string(), EnvValue::Text("/usr/bin".into())),
            ("NUMBER".to_string(), EnvValue::NotText),
            ("HOME".to_string(), EnvValue::Text("/tmp/a".into())),
        ]);
        assert!(!scrubbed.contains(&"NUMBER".to_string()));
        assert!(scrubbed.contains(&"HOME".to_string()));
    }

    #[test]
    fn a_clean_environment_scrubs_nothing() {
        let scrubbed = env_keys_to_scrub(&env(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/someone"),
        ]));
        assert!(scrubbed.is_empty());
    }

    #[test]
    fn a_successful_open_reports_nothing_and_a_failure_sets_the_error_state() {
        let opened = open_report(false, |_| Ok(()), |_| Ok(()), "http://x");
        assert_eq!(
            opened,
            Opened {
                ok: true,
                message: None,
                clipboard_failed: None
            }
        );

        let opened = open_report(
            false,
            |_| Ok(()),
            |_| Err(OpenFailure::NoXdgOpen),
            "http://x",
        );
        assert!(!opened.ok, "a browser that never opened must not read as success");
        assert!(opened.message.unwrap().contains("xdg-open"));
    }

    #[test]
    fn a_clipboard_failure_is_reported_but_does_not_stop_playback() {
        // The reference's exact asymmetry: no clipboard on a headless box is not a reason to
        // refuse to play, but the user still gets told.
        let opened = open_report(
            true,
            |_| Err("no display".to_string()),
            |_| Ok(()),
            "http://x",
        );
        assert!(opened.ok, "a clipboard failure must not fail the cast");
        assert_eq!(opened.clipboard_failed.as_deref(), Some("Could not copy the url: no display"));
        assert_eq!(opened.message, None);
    }

    #[test]
    fn the_copy_happens_before_the_open_so_a_failure_still_leaves_the_url_behind() {
        // Ordering is the small mercy: when the open fails, the url is already on the
        // clipboard. A test with two closures pins the order rather than trusting it.
        use std::cell::RefCell;
        let order = RefCell::new(Vec::new());
        let opened = open_report(
            true,
            |_| {
                order.borrow_mut().push("copy");
                Ok(())
            },
            |_| {
                order.borrow_mut().push("open");
                Err(OpenFailure::Spawn("boom".into()))
            },
            "http://x",
        );
        assert_eq!(*order.borrow(), vec!["copy", "open"]);
        assert!(!opened.ok);
        assert_eq!(
            opened.message.as_deref(),
            Some("Could not open a browser: boom")
        );
    }

    /// Every environment the reference scrubbed, replayed against this port.
    ///
    /// The interesting rows are the ones that must NOT be dropped: `PATH` carrying `tmp`
    /// (dropping it breaks every child) and a `tmp` value under an exempt key. Produced by
    /// executing the reference's `get_env` over six synthetic environments.
    #[test]
    fn every_reference_env_scrub_replays_identically() {
        let corpus = include_str!("../tests/golden/web_env.jsonl");
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            // Keep the reference's distinction: a value is either text or it is not, and
            // only text is ever inspected. A JSON number is exactly the "not a string" case.
            let mut env: Vec<(String, EnvValue)> = Vec::new();
            for (key, value) in row["input"].as_object().expect("input env") {
                let typed = match value.as_str() {
                    Some(text) => EnvValue::Text(text.to_string()),
                    None => EnvValue::NotText,
                };
                env.push((key.clone(), typed));
            }
            let mut dropped = env_keys_to_scrub(&env);
            dropped.sort();
            let expected: Vec<String> = row["keys_dropped"]
                .as_array()
                .expect("keys_dropped")
                .iter()
                .map(|k| k.as_str().unwrap_or("").to_string())
                .collect();
            assert_eq!(dropped, expected, "row {} scrubbed differently", row["env"]);
            rows += 1;
        }
        assert_eq!(rows, 6, "the corpus lost rows");
    }

    #[test]
    fn no_autocopy_means_no_clipboard_call_at_all() {
        // Calling `copy` when the setting is off would put every cast on the user's clipboard
        // uninvited, so the closure must not run.
        let called = std::cell::Cell::new(false);
        open_report(
            false,
            |_| {
                called.set(true);
                Ok(())
            },
            |_| Ok(()),
            "http://x",
        );
        assert!(!called.get());
    }
}
