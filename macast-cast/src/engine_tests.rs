//! Differential truth for the engine-answer sublayer of `macast/media_resolve.py`.
//! The oracle is `/tmp/mr14_oracle.py` running under CPython 3.12.15; it wrote
//! `/tmp/mr14_truth.txt` (md5 `9ef5ee3ce791433b3005409857e2dd00`, **601 rows**).
//! Every expectation below is one of its rows re-rendered as a Rust literal -- nothing
//! here was typed from memory, nothing here reads `/tmp`, nothing here spawns
//! `yt-dlp`, and nothing here touches the network. The eight functions under test are
//! [`crate::resolve::cookie_arguments`], [`crate::resolve::ytdlp_command`],
//! [`crate::resolve::engine_note`], [`crate::resolve::needs_cookies`],
//! [`crate::resolve::engine_says_unsupported`],
//! [`crate::resolve::cookie_source_problem`], [`crate::resolve::cookie_jar_state`] and
//! `super::timeout_sentence`.
//!
//! Four shapes of row exist in the truth file and cannot become an assertion here,
//! each for a different reason:
//!
//! * **There is no sink family.** The oracle's `subprocess.run` stub answered a
//!   normal `CompletedProcess`, so the wrapper sentence `yt-dlp 没能运行（<异常名>: <exc>）`
//!   has no recorded answer to copy. `super::sink_note`, `super::python_error_name` and
//!   [`crate::resolve::ytdlp_candidates`] keep only the doc comment in `resolve.rs` as
//!   their witness -- the argv they hand to the process *is* asserted, below, by the
//!   `cmd` rows; the JSON half was already covered by `ytdlp_json_becomes_candidates_
//!   and_never_claims_a_probe` in the inline tests.
//! * **`arg` index 291** asks `cookie_arguments` about `file='/x', browser=None`. The
//!   `Cookies` seam is `&str`, so the row is replayed with `browser: ""`. Unlike the
//!   other thirteen null cells it has **no `('/x', '')` twin in the file** (294 rows
//!   answer 291 distinct `(file, browser)` pairs), so the collapse rests on Python
//!   truthiness -- `if cookies['browser']:` reads `None` and `""` as the same
//!   "no opinion" -- and not on a recorded second row.
//! * **`note` shape 0** is `None`, which *is* representable (`Option<&str>`) and is
//!   replayed as `None` below. The fact that row 1 exists to establish -- `None` and
//!   `''` answer identically at all five exit codes -- is asserted once, in
//!   `engine_note_treats_no_stderr_as_empty_stderr`, rather than being duplicated
//!   across the grid.
//! * **`toutx`'s two rows** cannot be reproduced by a `Duration` at all. CPython
//!   formats the *float it was given* (`120.0`, `1e-07`); our port has seconds and
//!   nanos and renders `120` and `0.0000001`. The divergence is asserted as a
//!   divergence: the framing around the number matches and the number itself does
//!   not. That is the honest shape -- a green equality here would mean the test
//!   stopped asking.
//!
//! And two things the header should say about the numbers, because both were
//! falsified once on the way here: the note corpus is **29 stderr shapes x 5 exit
//! codes = 145 rows** (not 31 shapes -- two of the oracle's `STDERRS` lines are
//! Python implicit string concatenations), and the `prob` family's `hits` column is
//! **counted from the 27 probe rows below**, not copied: the generator refuses to
//! emit a table whose own count disagrees with the recorded one.

use std::time::Duration;

#[allow(dead_code)]
fn strs(items: &[String]) -> Vec<&str> {
    items.iter().map(|s| s.as_str()).collect()
}

#[allow(dead_code)]
fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The jar answers are compared as text so a refusal and a count pair cannot be
/// confused for each other by a `{}` that happens to print the same shape.
#[allow(dead_code)]
fn jar_answer(got: &Result<(usize, usize), String>) -> (bool, String) {
    match got {
        Ok((cookies, bad)) => (true, format!("{},{}", cookies, bad)),
        Err(msg) => (false, msg.clone()),
    }
}

#[allow(dead_code)]
fn jar_expectation(want: &Result<(usize, usize), &str>) -> (bool, String) {
    match want {
        Ok((cookies, bad)) => (true, format!("{},{}", cookies, bad)),
        Err(msg) => (false, msg.to_string()),
    }
}

/// `file`, `browser`, the answer. 294 rows: 9 files x 32 browsers plus the 6
/// dictionary shapes the reference reads with `cookies.get`.
const ARG_CASES: &[(&str, &str, &[&str])] = &[
    ("", "", &[]),
    ("", "  ", &[]),
    ("", "chrome", &["--cookies-from-browser", "chrome"]),
    ("", "CHROME", &["--cookies-from-browser", "chrome"]),
    ("", " chrome ", &["--cookies-from-browser", "chrome"]),
    ("", "firefox", &["--cookies-from-browser", "firefox"]),
    ("", "safari", &["--cookies-from-browser", "safari"]),
    ("", "vivaldi", &["--cookies-from-browser", "vivaldi"]),
    ("", "whale", &["--cookies-from-browser", "whale"]),
    ("", "brave", &["--cookies-from-browser", "brave"]),
    ("", "edge", &["--cookies-from-browser", "edge"]),
    ("", "chromium", &["--cookies-from-browser", "chromium"]),
    ("", "opera", &["--cookies-from-browser", "opera"]),
    ("", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data"]),
    ("", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data"]),
    ("", "chrome:Default", &["--cookies-from-browser", "chrome:Default"]),
    ("", "chrome: Default ", &["--cookies-from-browser", "chrome:Default"]),
    ("", "chrome:", &["--cookies-from-browser", "chrome"]),
    ("", "chrome::", &[]),
    ("", "chrome+GNOMEKEYRING", &[]),
    ("", "chrome::2", &[]),
    ("", "-framing", &[]),
    ("", "-x", &[]),
    ("", "notabrowser", &[]),
    ("", "chrome\tDefault", &[]),
    ("", "chrome\nDefault", &[]),
    ("", "chrome\rDefault", &[]),
    ("", "中文名", &[]),
    ("", "firefox:中文档案", &["--cookies-from-browser", "firefox:中文档案"]),
    ("", "firefox:-nope", &[]),
    ("", "firefox:--nope", &[]),
    ("", "", &[]),
    ("   ", "", &[]),
    ("   ", "  ", &[]),
    ("   ", "chrome", &["--cookies-from-browser", "chrome"]),
    ("   ", "CHROME", &["--cookies-from-browser", "chrome"]),
    ("   ", " chrome ", &["--cookies-from-browser", "chrome"]),
    ("   ", "firefox", &["--cookies-from-browser", "firefox"]),
    ("   ", "safari", &["--cookies-from-browser", "safari"]),
    ("   ", "vivaldi", &["--cookies-from-browser", "vivaldi"]),
    ("   ", "whale", &["--cookies-from-browser", "whale"]),
    ("   ", "brave", &["--cookies-from-browser", "brave"]),
    ("   ", "edge", &["--cookies-from-browser", "edge"]),
    ("   ", "chromium", &["--cookies-from-browser", "chromium"]),
    ("   ", "opera", &["--cookies-from-browser", "opera"]),
    ("   ", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data"]),
    ("   ", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data"]),
    ("   ", "chrome:Default", &["--cookies-from-browser", "chrome:Default"]),
    ("   ", "chrome: Default ", &["--cookies-from-browser", "chrome:Default"]),
    ("   ", "chrome:", &["--cookies-from-browser", "chrome"]),
    ("   ", "chrome::", &[]),
    ("   ", "chrome+GNOMEKEYRING", &[]),
    ("   ", "chrome::2", &[]),
    ("   ", "-framing", &[]),
    ("   ", "-x", &[]),
    ("   ", "notabrowser", &[]),
    ("   ", "chrome\tDefault", &[]),
    ("   ", "chrome\nDefault", &[]),
    ("   ", "chrome\rDefault", &[]),
    ("   ", "中文名", &[]),
    ("   ", "firefox:中文档案", &["--cookies-from-browser", "firefox:中文档案"]),
    ("   ", "firefox:-nope", &[]),
    ("   ", "firefox:--nope", &[]),
    ("   ", "", &[]),
    ("\t", "", &[]),
    ("\t", "  ", &[]),
    ("\t", "chrome", &["--cookies-from-browser", "chrome"]),
    ("\t", "CHROME", &["--cookies-from-browser", "chrome"]),
    ("\t", " chrome ", &["--cookies-from-browser", "chrome"]),
    ("\t", "firefox", &["--cookies-from-browser", "firefox"]),
    ("\t", "safari", &["--cookies-from-browser", "safari"]),
    ("\t", "vivaldi", &["--cookies-from-browser", "vivaldi"]),
    ("\t", "whale", &["--cookies-from-browser", "whale"]),
    ("\t", "brave", &["--cookies-from-browser", "brave"]),
    ("\t", "edge", &["--cookies-from-browser", "edge"]),
    ("\t", "chromium", &["--cookies-from-browser", "chromium"]),
    ("\t", "opera", &["--cookies-from-browser", "opera"]),
    ("\t", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data"]),
    ("\t", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data"]),
    ("\t", "chrome:Default", &["--cookies-from-browser", "chrome:Default"]),
    ("\t", "chrome: Default ", &["--cookies-from-browser", "chrome:Default"]),
    ("\t", "chrome:", &["--cookies-from-browser", "chrome"]),
    ("\t", "chrome::", &[]),
    ("\t", "chrome+GNOMEKEYRING", &[]),
    ("\t", "chrome::2", &[]),
    ("\t", "-framing", &[]),
    ("\t", "-x", &[]),
    ("\t", "notabrowser", &[]),
    ("\t", "chrome\tDefault", &[]),
    ("\t", "chrome\nDefault", &[]),
    ("\t", "chrome\rDefault", &[]),
    ("\t", "中文名", &[]),
    ("\t", "firefox:中文档案", &["--cookies-from-browser", "firefox:中文档案"]),
    ("\t", "firefox:-nope", &[]),
    ("\t", "firefox:--nope", &[]),
    ("\t", "", &[]),
    ("\r\n", "", &[]),
    ("\r\n", "  ", &[]),
    ("\r\n", "chrome", &["--cookies-from-browser", "chrome"]),
    ("\r\n", "CHROME", &["--cookies-from-browser", "chrome"]),
    ("\r\n", " chrome ", &["--cookies-from-browser", "chrome"]),
    ("\r\n", "firefox", &["--cookies-from-browser", "firefox"]),
    ("\r\n", "safari", &["--cookies-from-browser", "safari"]),
    ("\r\n", "vivaldi", &["--cookies-from-browser", "vivaldi"]),
    ("\r\n", "whale", &["--cookies-from-browser", "whale"]),
    ("\r\n", "brave", &["--cookies-from-browser", "brave"]),
    ("\r\n", "edge", &["--cookies-from-browser", "edge"]),
    ("\r\n", "chromium", &["--cookies-from-browser", "chromium"]),
    ("\r\n", "opera", &["--cookies-from-browser", "opera"]),
    ("\r\n", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data"]),
    ("\r\n", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies-from-browser", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data"]),
    ("\r\n", "chrome:Default", &["--cookies-from-browser", "chrome:Default"]),
    ("\r\n", "chrome: Default ", &["--cookies-from-browser", "chrome:Default"]),
    ("\r\n", "chrome:", &["--cookies-from-browser", "chrome"]),
    ("\r\n", "chrome::", &[]),
    ("\r\n", "chrome+GNOMEKEYRING", &[]),
    ("\r\n", "chrome::2", &[]),
    ("\r\n", "-framing", &[]),
    ("\r\n", "-x", &[]),
    ("\r\n", "notabrowser", &[]),
    ("\r\n", "chrome\tDefault", &[]),
    ("\r\n", "chrome\nDefault", &[]),
    ("\r\n", "chrome\rDefault", &[]),
    ("\r\n", "中文名", &[]),
    ("\r\n", "firefox:中文档案", &["--cookies-from-browser", "firefox:中文档案"]),
    ("\r\n", "firefox:-nope", &[]),
    ("\r\n", "firefox:--nope", &[]),
    ("\r\n", "", &[]),
    ("/tmp/cookies.txt", "", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "  ", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "CHROME", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", " chrome ", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "firefox", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "safari", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "vivaldi", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "whale", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "brave", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "edge", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chromium", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "opera", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome:Default", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome: Default ", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome:", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome::", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome+GNOMEKEYRING", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome::2", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "-framing", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "-x", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "notabrowser", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome\tDefault", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome\nDefault", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "chrome\rDefault", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "中文名", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "firefox:中文档案", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "firefox:-nope", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "firefox:--nope", &["--cookies", "/tmp/cookies.txt"]),
    ("/tmp/cookies.txt", "", &["--cookies", "/tmp/cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "  ", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "CHROME", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", " chrome ", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "firefox", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "safari", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "vivaldi", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "whale", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "brave", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "edge", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chromium", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "opera", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome:Default", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome: Default ", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome:", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome::", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome+GNOMEKEYRING", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome::2", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "-framing", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "-x", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "notabrowser", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome\tDefault", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome\nDefault", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "chrome\rDefault", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "中文名", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "firefox:中文档案", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "firefox:-nope", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "firefox:--nope", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("  /tmp/空格 cookies.txt  ", "", &["--cookies", "/tmp/空格 cookies.txt"]),
    ("a\nb", "", &["--cookies", "a\nb"]),
    ("a\nb", "  ", &["--cookies", "a\nb"]),
    ("a\nb", "chrome", &["--cookies", "a\nb"]),
    ("a\nb", "CHROME", &["--cookies", "a\nb"]),
    ("a\nb", " chrome ", &["--cookies", "a\nb"]),
    ("a\nb", "firefox", &["--cookies", "a\nb"]),
    ("a\nb", "safari", &["--cookies", "a\nb"]),
    ("a\nb", "vivaldi", &["--cookies", "a\nb"]),
    ("a\nb", "whale", &["--cookies", "a\nb"]),
    ("a\nb", "brave", &["--cookies", "a\nb"]),
    ("a\nb", "edge", &["--cookies", "a\nb"]),
    ("a\nb", "chromium", &["--cookies", "a\nb"]),
    ("a\nb", "opera", &["--cookies", "a\nb"]),
    ("a\nb", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "a\nb"]),
    ("a\nb", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "a\nb"]),
    ("a\nb", "chrome:Default", &["--cookies", "a\nb"]),
    ("a\nb", "chrome: Default ", &["--cookies", "a\nb"]),
    ("a\nb", "chrome:", &["--cookies", "a\nb"]),
    ("a\nb", "chrome::", &["--cookies", "a\nb"]),
    ("a\nb", "chrome+GNOMEKEYRING", &["--cookies", "a\nb"]),
    ("a\nb", "chrome::2", &["--cookies", "a\nb"]),
    ("a\nb", "-framing", &["--cookies", "a\nb"]),
    ("a\nb", "-x", &["--cookies", "a\nb"]),
    ("a\nb", "notabrowser", &["--cookies", "a\nb"]),
    ("a\nb", "chrome\tDefault", &["--cookies", "a\nb"]),
    ("a\nb", "chrome\nDefault", &["--cookies", "a\nb"]),
    ("a\nb", "chrome\rDefault", &["--cookies", "a\nb"]),
    ("a\nb", "中文名", &["--cookies", "a\nb"]),
    ("a\nb", "firefox:中文档案", &["--cookies", "a\nb"]),
    ("a\nb", "firefox:-nope", &["--cookies", "a\nb"]),
    ("a\nb", "firefox:--nope", &["--cookies", "a\nb"]),
    ("a\nb", "", &["--cookies", "a\nb"]),
    ("a\\b", "", &["--cookies", "a\\b"]),
    ("a\\b", "  ", &["--cookies", "a\\b"]),
    ("a\\b", "chrome", &["--cookies", "a\\b"]),
    ("a\\b", "CHROME", &["--cookies", "a\\b"]),
    ("a\\b", " chrome ", &["--cookies", "a\\b"]),
    ("a\\b", "firefox", &["--cookies", "a\\b"]),
    ("a\\b", "safari", &["--cookies", "a\\b"]),
    ("a\\b", "vivaldi", &["--cookies", "a\\b"]),
    ("a\\b", "whale", &["--cookies", "a\\b"]),
    ("a\\b", "brave", &["--cookies", "a\\b"]),
    ("a\\b", "edge", &["--cookies", "a\\b"]),
    ("a\\b", "chromium", &["--cookies", "a\\b"]),
    ("a\\b", "opera", &["--cookies", "a\\b"]),
    ("a\\b", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "a\\b"]),
    ("a\\b", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "a\\b"]),
    ("a\\b", "chrome:Default", &["--cookies", "a\\b"]),
    ("a\\b", "chrome: Default ", &["--cookies", "a\\b"]),
    ("a\\b", "chrome:", &["--cookies", "a\\b"]),
    ("a\\b", "chrome::", &["--cookies", "a\\b"]),
    ("a\\b", "chrome+GNOMEKEYRING", &["--cookies", "a\\b"]),
    ("a\\b", "chrome::2", &["--cookies", "a\\b"]),
    ("a\\b", "-framing", &["--cookies", "a\\b"]),
    ("a\\b", "-x", &["--cookies", "a\\b"]),
    ("a\\b", "notabrowser", &["--cookies", "a\\b"]),
    ("a\\b", "chrome\tDefault", &["--cookies", "a\\b"]),
    ("a\\b", "chrome\nDefault", &["--cookies", "a\\b"]),
    ("a\\b", "chrome\rDefault", &["--cookies", "a\\b"]),
    ("a\\b", "中文名", &["--cookies", "a\\b"]),
    ("a\\b", "firefox:中文档案", &["--cookies", "a\\b"]),
    ("a\\b", "firefox:-nope", &["--cookies", "a\\b"]),
    ("a\\b", "firefox:--nope", &["--cookies", "a\\b"]),
    ("a\\b", "", &["--cookies", "a\\b"]),
    ("中文.jar", "", &["--cookies", "中文.jar"]),
    ("中文.jar", "  ", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome", &["--cookies", "中文.jar"]),
    ("中文.jar", "CHROME", &["--cookies", "中文.jar"]),
    ("中文.jar", " chrome ", &["--cookies", "中文.jar"]),
    ("中文.jar", "firefox", &["--cookies", "中文.jar"]),
    ("中文.jar", "safari", &["--cookies", "中文.jar"]),
    ("中文.jar", "vivaldi", &["--cookies", "中文.jar"]),
    ("中文.jar", "whale", &["--cookies", "中文.jar"]),
    ("中文.jar", "brave", &["--cookies", "中文.jar"]),
    ("中文.jar", "edge", &["--cookies", "中文.jar"]),
    ("中文.jar", "chromium", &["--cookies", "中文.jar"]),
    ("中文.jar", "opera", &["--cookies", "中文.jar"]),
    ("中文.jar", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "中文.jar"]),
    ("中文.jar", "vivaldi:C:\\Users\\无常\\AppData\\Local\\Vivaldi\\User Data", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome:Default", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome: Default ", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome:", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome::", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome+GNOMEKEYRING", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome::2", &["--cookies", "中文.jar"]),
    ("中文.jar", "-framing", &["--cookies", "中文.jar"]),
    ("中文.jar", "-x", &["--cookies", "中文.jar"]),
    ("中文.jar", "notabrowser", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome\tDefault", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome\nDefault", &["--cookies", "中文.jar"]),
    ("中文.jar", "chrome\rDefault", &["--cookies", "中文.jar"]),
    ("中文.jar", "中文名", &["--cookies", "中文.jar"]),
    ("中文.jar", "firefox:中文档案", &["--cookies", "中文.jar"]),
    ("中文.jar", "firefox:-nope", &["--cookies", "中文.jar"]),
    ("中文.jar", "firefox:--nope", &["--cookies", "中文.jar"]),
    ("中文.jar", "", &["--cookies", "中文.jar"]),
    ("", "", &[]),
    ("", "", &[]),
    ("", "", &[]),
    ("/x", "", &["--cookies", "/x"]),
    ("", "chrome", &["--cookies-from-browser", "chrome"]),
    ("", "", &[]),
];

/// `binary`, `page_url`, `file`, `browser`, the whole argv. Every row comes from
/// the oracle's captured `subprocess.run` call, so this is the command that
/// actually reaches the process -- not a hand-written one.
const CMD_CASES: &[(&str, &str, &str, &str, &[&str])] = &[
    ("yt-dlp", "https://example.com/v/1", "", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "", "", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://e/1"]),
    ("yt-dlp", "https://example.com/v/1", "/tmp/jar.txt", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "/tmp/jar.txt", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "/tmp/jar.txt", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "/tmp/jar.txt", "", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "/tmp/jar.txt", "", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://e/1"]),
    ("yt-dlp", "https://example.com/v/1", "", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "chrome", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "chrome", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "chrome", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "chrome", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "", "chrome", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "chrome", "https://e/1"]),
    ("yt-dlp", "https://example.com/v/1", "/tmp/jar.txt", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "/tmp/jar.txt", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "/tmp/jar.txt", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "/tmp/jar.txt", "chrome", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "/tmp/jar.txt", "chrome", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies", "/tmp/jar.txt", "https://e/1"]),
    ("yt-dlp", "https://example.com/v/1", "  ", "vivaldi:C:\\Users\\A\\D", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "vivaldi:C:\\Users\\A\\D", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "  ", "vivaldi:C:\\Users\\A\\D", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "vivaldi:C:\\Users\\A\\D", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "  ", "vivaldi:C:\\Users\\A\\D", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "vivaldi:C:\\Users\\A\\D", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "  ", "vivaldi:C:\\Users\\A\\D", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "vivaldi:C:\\Users\\A\\D", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "  ", "vivaldi:C:\\Users\\A\\D", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "--cookies-from-browser", "vivaldi:C:\\Users\\A\\D", "https://e/1"]),
    ("yt-dlp", "https://example.com/v/1", "", "chrome+GNOMEKEYRING", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://example.com/v/1"]),
    ("yt-dlp", "https://example.com/a?b=1&c=中", "", "chrome+GNOMEKEYRING", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://example.com/a?b=1&c=中"]),
    ("yt-dlp", "http://[::1]:8080/x", "", "chrome+GNOMEKEYRING", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "http://[::1]:8080/x"]),
    ("yt-dlp", "", "", "chrome+GNOMEKEYRING", &["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", ""]),
    ("/opt/homebrew/bin/yt-dlp", "https://e/1", "", "chrome+GNOMEKEYRING", &["/opt/homebrew/bin/yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://e/1"]),
];

/// The stderr the engine wrote, in the oracle's order. Index 0 is `None`: the
/// reference is called with `proc.stderr` and a killed or text-less run hands
/// back `None`, so the shape is real rather than defensive.
const NOTE_SHAPES: &[Option<&str>] = &[
    None,
    Some(""),
    Some("   \n\t \n"),
    Some("\n\n\n"),
    Some("ERROR: [BiliBili] xxx: Requested format is not available"),
    Some("ERROR: cookies to access this webpage"),
    Some("ERROR: 请试用 --cookies-from-browser"),
    Some("WARNING: skipping cookie file entry due to invalid length 1: b'\\x'\nERROR: [BiliBili] : Failed to retrieve cookies"),
    Some("Traceback (most recent call last):\n  File \"<frozen runpy>\", line 19\nERROR: Unsupported URL: https://x/y"),
    Some("ERROR: first marker\nERROR: second marker"),
    Some("a plain sentence\nanother plain line\n"),
    Some("  ERROR: indented marker  \n"),
    Some("error: lowercase is not the marker"),
    Some("prefix ERROR: not at line start"),
    Some("ERROR:"),
    Some("ERROR: "),
    Some("line one\nERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长"),
    Some("ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬"),
    Some("首行就是中文原话，没有任何标记"),
    Some("\u{b}vertical\u{b}tab\u{b}"),
    Some("\u{85}next line\u{85}"),
    Some("\u{2028}line separator\u{2029}paragraph\u{2028}ERROR: after separators"),
    Some("ERROR: crlf\r\nsecond\r\n"),
    Some("ERROR: tab\tinside"),
    Some("ERROR: \0nul\0inside"),
    Some("  \n  \t \nERROR: after blanks\n"),
    Some("ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
    Some("ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
    Some("ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
];

const NOTE_RCS: &[i32] = &[0, 1, 2, -15, 255];

/// One answer per (shape, exit code), shape-major and exit-code minor -- the
/// order `145 == 29 x 5` the grid is built in.
const NOTE_WANTS: &[&str] = &[
    "yt-dlp 没有返回任何地址",
    "yt-dlp 退出码 1",
    "yt-dlp 退出码 2",
    "yt-dlp 退出码 -15",
    "yt-dlp 退出码 255",
    "yt-dlp 没有返回任何地址",
    "yt-dlp 退出码 1",
    "yt-dlp 退出码 2",
    "yt-dlp 退出码 -15",
    "yt-dlp 退出码 255",
    "yt-dlp 没有返回任何地址",
    "yt-dlp 退出码 1",
    "yt-dlp 退出码 2",
    "yt-dlp 退出码 -15",
    "yt-dlp 退出码 255",
    "yt-dlp 没有返回任何地址",
    "yt-dlp 退出码 1",
    "yt-dlp 退出码 2",
    "yt-dlp 退出码 -15",
    "yt-dlp 退出码 255",
    "ERROR: [BiliBili] xxx: Requested format is not available",
    "ERROR: [BiliBili] xxx: Requested format is not available",
    "ERROR: [BiliBili] xxx: Requested format is not available",
    "ERROR: [BiliBili] xxx: Requested format is not available",
    "ERROR: [BiliBili] xxx: Requested format is not available",
    "ERROR: cookies to access this webpage",
    "ERROR: cookies to access this webpage",
    "ERROR: cookies to access this webpage",
    "ERROR: cookies to access this webpage",
    "ERROR: cookies to access this webpage",
    "ERROR: 请试用 --cookies-from-browser",
    "ERROR: 请试用 --cookies-from-browser",
    "ERROR: 请试用 --cookies-from-browser",
    "ERROR: 请试用 --cookies-from-browser",
    "ERROR: 请试用 --cookies-from-browser",
    "ERROR: [BiliBili] : Failed to retrieve cookies",
    "ERROR: [BiliBili] : Failed to retrieve cookies",
    "ERROR: [BiliBili] : Failed to retrieve cookies",
    "ERROR: [BiliBili] : Failed to retrieve cookies",
    "ERROR: [BiliBili] : Failed to retrieve cookies",
    "ERROR: Unsupported URL: https://x/y",
    "ERROR: Unsupported URL: https://x/y",
    "ERROR: Unsupported URL: https://x/y",
    "ERROR: Unsupported URL: https://x/y",
    "ERROR: Unsupported URL: https://x/y",
    "ERROR: first marker",
    "ERROR: first marker",
    "ERROR: first marker",
    "ERROR: first marker",
    "ERROR: first marker",
    "a plain sentence",
    "a plain sentence",
    "a plain sentence",
    "a plain sentence",
    "a plain sentence",
    "ERROR: indented marker",
    "ERROR: indented marker",
    "ERROR: indented marker",
    "ERROR: indented marker",
    "ERROR: indented marker",
    "error: lowercase is not the marker",
    "error: lowercase is not the marker",
    "error: lowercase is not the marker",
    "error: lowercase is not the marker",
    "error: lowercase is not the marker",
    "prefix ERROR: not at line start",
    "prefix ERROR: not at line start",
    "prefix ERROR: not at line start",
    "prefix ERROR: not at line start",
    "prefix ERROR: not at line start",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR:",
    "ERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长",
    "ERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长",
    "ERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长",
    "ERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长",
    "ERROR: 长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长长",
    "ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬",
    "ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬",
    "ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬",
    "ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬",
    "ERROR: 🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬🎬",
    "首行就是中文原话，没有任何标记",
    "首行就是中文原话，没有任何标记",
    "首行就是中文原话，没有任何标记",
    "首行就是中文原话，没有任何标记",
    "首行就是中文原话，没有任何标记",
    "vertical",
    "vertical",
    "vertical",
    "vertical",
    "vertical",
    "next line",
    "next line",
    "next line",
    "next line",
    "next line",
    "ERROR: after separators",
    "ERROR: after separators",
    "ERROR: after separators",
    "ERROR: after separators",
    "ERROR: after separators",
    "ERROR: crlf",
    "ERROR: crlf",
    "ERROR: crlf",
    "ERROR: crlf",
    "ERROR: crlf",
    "ERROR: tab\tinside",
    "ERROR: tab\tinside",
    "ERROR: tab\tinside",
    "ERROR: tab\tinside",
    "ERROR: tab\tinside",
    "ERROR: \0nul\0inside",
    "ERROR: \0nul\0inside",
    "ERROR: \0nul\0inside",
    "ERROR: \0nul\0inside",
    "ERROR: \0nul\0inside",
    "ERROR: after blanks",
    "ERROR: after blanks",
    "ERROR: after blanks",
    "ERROR: after blanks",
    "ERROR: after blanks",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "ERROR: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
];

const NEEDS_CASES: &[(&str, bool)] = &[
    ("", false),
    ("", false),
    ("ERROR: [BiliBili] xxx: cookies to access this webpage", true),
    ("ERROR: Fresh cookies (not necessarily logged in) are needed", true),
    ("ERROR: this video requires login", true),
    ("ERROR: Log in to watch this", true),
    ("ERROR: please sign in", true),
    ("ERROR: authentication required", true),
    ("ERROR: unauthorized", true),
    ("ERROR: 需要登录后才能观看", true),
    ("ERROR: 请先授权", true),
    ("ERROR: COOKIE DATABASE IS LOCKED", true),
    ("ERROR: could not find chrome cookies database in /nope", true),
    ("ERROR: Unsupported URL: https://x/y", false),
    ("ERROR: Unable to extract video data", false),
    ("yt-dlp 退出码 1", false),
    ("yt-dlp 没有返回任何地址", false),
    ("cookie", true),
    ("cooke", false),
    ("登录", false),
];

const UNSP_CASES: &[(&str, bool)] = &[
    ("", false),
    ("", false),
    ("ERROR: Unsupported URL: https://x/y", true),
    ("unsupported url", true),
    ("there is no supported extractor", true),
    ("ERROR: this site is not supported", true),
    ("NOT SUPPORTED", true),
    ("ERROR: Unable to extract video data", false),
    ("ERROR: cookies to access this webpage", false),
    ("ERROR: could not find chrome cookies database in /nope", false),
    ("yt-dlp 退出码 1", false),
    ("no support", false),
    ("unsupported", false),
];

/// The `__words__` row: the word list the reference actually reads, in order.
const UNSP_WORDS: &[&str] = &["unsupported url", "no supported", "not supported"];

/// The note, and the machine-side failure it names -- `None` where Python
/// answers `''`, because this port says "no opinion" with a type instead of
/// with the empty string.
const PROB_CASES: &[(&str, Option<&str>)] = &[
    ("", None),
    ("", None),
    ("ERROR: could not copy chrome cookie database", Some("locked")),
    ("ERROR: Could not copy Chrome cookie database", Some("locked")),
    ("could not copy chrome cookie database and it also says cookies database", Some("locked")),
    ("ERROR: could not find chrome cookies database in /nope", Some("not_found")),
    ("ERROR: could not find vivaldi cookies database in", Some("not_found")),
    ("ERROR: could not find safari cookie database", Some("not_found")),
    ("ERROR: could not find firefox cookies database in '~/Library'", Some("not_found")),
    ("ERROR: failed to decrypt with DPAPI", Some("decrypt")),
    ("ERROR: Failed to read from keyring", Some("decrypt")),
    ("ERROR: kwallet-query is not available", Some("decrypt")),
    ("ERROR: could not find Local State file", Some("decrypt")),
    ("ERROR: no encrypted key in Local State", Some("decrypt")),
    ("ERROR: Opera/GX does not support profiles", Some("bad_profile")),
    ("does not support profiles", Some("bad_profile")),
    ("ERROR: Unsupported URL: https://x/y", None),
    ("ERROR: cookies to access this webpage", None),
    ("WARNING: skipping cookie file entry due to invalid length 1: b'x'", None),
    ("cannot decrypt v10 cookies", None),
    ("unknown cookie version", None),
    ("unknown browser: nope", None),
    ("unsupported keyring: GNOMEKEYRING", None),
    ("error: could not copy chrome cookie database", Some("locked")),
    ("cookies DATABASE", Some("not_found")),
    ("Cookie Database", Some("not_found")),
    ("no rows match at all: just a word like cookie", None),
];

const PROB_TUPLE: &[&str] = &["locked", "not_found", "decrypt", "bad_profile"];

/// `cookie_source_problem`'s own table, one entry per declared state, and how
/// many of the 27 probes above land on it. The recorded numbers are compared
/// against a count taken from `PROB_CASES` at run time, so a probe row that
/// stops answering makes the pair that cites it go red instead of silently
/// agreeing with a stale literal.
const PROB_TABLE_HITS: &[(&str, usize)] = &[
    ("locked", 4),
    ("not_found", 6),
    ("decrypt", 5),
    ("bad_profile", 2),
];

const JAR_CASES: &[(&str, (usize, usize))] = &[
    ("", (0, 0)),
    ("\n\n\n", (0, 0)),
    ("# nothing here\n", (0, 0)),
    ("# Netscape HTTP Cookie File\n.example.com\tSESS\tTRUE\tTRUE\t0\t1\n", (0, 1)),
    ("#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\n#HttpOnly_# a comment wearing a prefix\n  #HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\n", (0, 1)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\n#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\n#HttpOnly_# a comment wearing a prefix\n  #HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\n# comment line\n#HttpOnly_ is the jar header people paste\n\n   \n\t\t\t\t\t\t\t\n.example.com\tA\tTRUE\tTRUE\t0\t\n.example.com\tA\tTRUE\tTRUE\t0\tx\ty\n.example.com\tA\tTRUE\tTRUE\t0\tx\ty\tz\n.example.com\n\tA\tTRUE\tTRUE\t0\t1\n   \tA\tTRUE\tTRUE\t0\t1\n.example.com\tA\tFALSE\tFALSE\tnot-a-number\t0\tval\n.example.com\tA\tFALSE\tTRUE\t0\tval\n.example.com\tA\tFALSE\tTRUE\t0\n中文域名\t中文键\tTRUE\tTRUE\t0\t中文值\na\u{b}b\u{c}ck\u{1c} Cookies\tBAD\tTRUE\tTRUE\t0\t1", (3, 14)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\r\n#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\r\n#HttpOnly_# a comment wearing a prefix\r\n  #HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1\r\n# comment line\r\n#HttpOnly_ is the jar header people paste\r\n\r\n   \r\n\t\t\t\t\t\t\t\r\n.example.com\tA\tTRUE\tTRUE\t0\t\r\n.example.com\tA\tTRUE\tTRUE\t0\tx\ty\r\n.example.com\tA\tTRUE\tTRUE\t0\tx\ty\tz\r\n.example.com\r\n\tA\tTRUE\tTRUE\t0\t1\r\n   \tA\tTRUE\tTRUE\t0\t1\r\n.example.com\tA\tFALSE\tFALSE\tnot-a-number\t0\tval\r\n.example.com\tA\tFALSE\tTRUE\t0\tval\r\n.example.com\tA\tFALSE\tTRUE\t0\r\n中文域名\t中文键\tTRUE\tTRUE\t0\t中文值\r\na\u{b}b\u{c}ck\u{1c} Cookies\tBAD\tTRUE\tTRUE\t0\t1", (3, 14)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\r\n#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 2)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\u{b}#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 2)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\u{85}#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 2)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\u{2028}#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 2)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\nno tabs at all\n.example.com\tA\tTRUE\tTRUE\t0\t", (0, 3)),
    ("#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 1)),
    ("\t#HttpOnly_.example.com\tSESS\tTRUE\tTRUE\t0\t1", (0, 0)),
    (".example.com\tSESS\tTRUE\tTRUE\t0\t1\n\n\t\n", (0, 1)),
];

/// The byte cap, measured on pure repeats: `character`, `count`, the UTF-8
/// length the oracle recorded for that text, and the answer. `中` crosses the
/// 256 KB ceiling at 87_382 characters and `A` at 262_145, which is the whole
/// reason the cap asks for bytes and not for `len()`.
const JAR_CAP_CASES: &[(&str, usize, usize, Result<(usize, usize), &str>)] = &[
    ("A", 262131, 262131, Ok((0, 1))),
    ("A", 262132, 262132, Ok((0, 1))),
    ("A", 262143, 262143, Ok((0, 1))),
    ("A", 262144, 262144, Ok((0, 1))),
    ("A", 262145, 262145, Err("cookie 文件太大了（上限 256 KB）")),
    ("中", 87381, 262143, Ok((0, 1))),
    ("中", 87382, 262146, Err("cookie 文件太大了（上限 256 KB）")),
];

/// The `__limit__` row: the three constants the reference measures a jar
/// against. `262144` is `COOKIE_JAR_MAX_BYTES`, `7` is `COOKIE_JAR_FIELDS`,
/// and `#HttpOnly_` is the prefix that marks a cookie line rather than a
/// comment -- the row that used to read a browser's export as "no cookies".
const JAR_LIMIT_TRIPLE: (usize, usize, &str) = (262144, 7, "#HttpOnly_");

/// `argv`, the timeout as (secs, nanos), and CPython's own `TimeoutExpired`
/// sentence. The repr of the argv list is part of the answer, so `super::py_repr_str_list`
/// is under test here as much as the framing is: `'`, `"`, NUL, `\x7f`, C1
/// controls, `\u2028`, Cf characters, astral code points and a 40-character CJK
/// element all sit in these rows.
const TOUT_CASES: &[(&[&str], u64, u32, &str)] = &[
    (&["yt-dlp", "-J", "--no-playlist", "--no-warnings", "--quiet", "--socket-timeout", "20", "https://example.com/v/1"], 120, 0, "Command '['yt-dlp', '-J', '--no-playlist', '--no-warnings', '--quiet', '--socket-timeout', '20', 'https://example.com/v/1']' timed out after 120 seconds"),
    (&["yt-dlp", "-J", "--cookies", "/tmp/jar.txt", "https://example.com/v/1"], 120, 0, "Command '['yt-dlp', '-J', '--cookies', '/tmp/jar.txt', 'https://example.com/v/1']' timed out after 120 seconds"),
    (&["yt-dlp", "-J", "--cookies-from-browser", "vivaldi:C:\\Users\\Administrator\\AppData\\Local\\Vivaldi\\User Data", "https://example.com/v/1"], 120, 0, "Command '['yt-dlp', '-J', '--cookies-from-browser', 'vivaldi:C:\\\\Users\\\\Administrator\\\\AppData\\\\Local\\\\Vivaldi\\\\User Data', 'https://example.com/v/1']' timed out after 120 seconds"),
    (&[], 120, 0, "Command '[]' timed out after 120 seconds"),
    (&[""], 120, 0, "Command '['']' timed out after 120 seconds"),
    (&["a"], 120, 0, "Command '['a']' timed out after 120 seconds"),
    (&["it's"], 120, 0, "Command '[\"it's\"]' timed out after 120 seconds"),
    (&["a\"b"], 120, 0, "Command '['a\"b']' timed out after 120 seconds"),
    (&["it's a\"b"], 120, 0, "Command '['it\\'s a\"b']' timed out after 120 seconds"),
    (&["\\c"], 120, 0, "Command '['\\\\c']' timed out after 120 seconds"),
    (&["\n\r\t\u{1}"], 120, 0, "Command '['\\n\\r\\t\\x01']' timed out after 120 seconds"),
    (&["中文"], 120, 0, "Command '['中文']' timed out after 120 seconds"),
    (&["🎬"], 120, 0, "Command '['🎬']' timed out after 120 seconds"),
    (&["\0"], 120, 0, "Command '['\\x00']' timed out after 120 seconds"),
    (&["\u{7f}\u{1f}中"], 120, 0, "Command '['\\x7f\\x1f中']' timed out after 120 seconds"),
    (&["\u{a0}nbsp"], 120, 0, "Command '['\\xa0nbsp']' timed out after 120 seconds"),
    (&["\u{2028}sep\u{2029}para"], 120, 0, "Command '['\\u2028sep\\u2029para']' timed out after 120 seconds"),
    (&["\u{ad}soft"], 120, 0, "Command '['\\xadsoft']' timed out after 120 seconds"),
    (&["\u{200b}zwsp\u{200c}\u{200d}"], 120, 0, "Command '['\\u200bzwsp\\u200c\\u200d']' timed out after 120 seconds"),
    (&["\u{feff}bom"], 120, 0, "Command '['\\ufeffbom']' timed out after 120 seconds"),
    (&["\u{2060}word"], 120, 0, "Command '['\\u2060word']' timed out after 120 seconds"),
    (&["\u{1bca0}shetty"], 120, 0, "Command '['\\U0001bca0shetty']' timed out after 120 seconds"),
    (&["\u{600}arabic-indic"], 120, 0, "Command '['\\u0600arabic-indic']' timed out after 120 seconds"),
    (&["\u{202a}embed"], 120, 0, "Command '['\\u202aembed']' timed out after 120 seconds"),
    (&["\u{fff9}interlinear"], 120, 0, "Command '['\\ufff9interlinear']' timed out after 120 seconds"),
    (&["\u{e0001}tag"], 120, 0, "Command '['\\U000e0001tag']' timed out after 120 seconds"),
    (&["\u{e0041}tagchar"], 120, 0, "Command '['\\U000e0041tagchar']' timed out after 120 seconds"),
    (&["'"], 120, 0, "Command '[\"'\"]' timed out after 120 seconds"),
    (&["\""], 120, 0, "Command '['\"']' timed out after 120 seconds"),
    (&["\"'"], 120, 0, "Command '['\"\\'']' timed out after 120 seconds"),
    (&["\\'"], 120, 0, "Command '[\"\\\\'\"]' timed out after 120 seconds"),
    (&["'\\'"], 120, 0, "Command '[\"'\\\\'\"]' timed out after 120 seconds"),
    (&["%s"], 120, 0, "Command '['%s']' timed out after 120 seconds"),
    (&["{}"], 120, 0, "Command '['{}']' timed out after 120 seconds"),
    (&["中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中"], 120, 0, "Command '['中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中中']' timed out after 120 seconds"),
    (&["yt-dlp", "-J"], 20, 0, "Command '['yt-dlp', '-J']' timed out after 20 seconds"),
    (&["yt-dlp", "-J"], 0, 500000000, "Command '['yt-dlp', '-J']' timed out after 0.5 seconds"),
    (&["yt-dlp", "-J"], 0, 100000000, "Command '['yt-dlp', '-J']' timed out after 0.1 seconds"),
    (&["yt-dlp", "-J"], 1, 500000000, "Command '['yt-dlp', '-J']' timed out after 1.5 seconds"),
    (&["yt-dlp", "-J"], 3600, 0, "Command '['yt-dlp', '-J']' timed out after 3600 seconds"),
    (&["yt-dlp", "-J"], 120, 0, "Command '['yt-dlp', '-J']' timed out after 120 seconds"),
];

/// The two rows a `Duration` cannot answer: Python printed the float it was
/// handed. `prefix` is everything up to and including `"' timed out after "`,
/// taken from the recorded answer rather than typed here.
const TOUTX_CASES: &[(&[&str], u64, u32, &str, &str)] = &[
    (&["yt-dlp", "-J"], 120, 0, "Command '['yt-dlp', '-J']' timed out after 120.0 seconds", "Command '['yt-dlp', '-J']' timed out after "),
    (&["yt-dlp", "-J"], 0, 100, "Command '['yt-dlp', '-J']' timed out after 1e-07 seconds", "Command '['yt-dlp', '-J']' timed out after "),
];

#[test]
fn cookie_arguments_replays_the_recorded_grid() {
    crate::pin_test_dir();
    assert_eq!(ARG_CASES.len(), 294, "the cookie grid moved");
    for (index, (file, browser, want)) in ARG_CASES.iter().enumerate() {
        let cookies = crate::resolve::Cookies {
            file: *file,
            browser: *browser,
        };
        let got = crate::resolve::cookie_arguments(&cookies);
        assert_eq!(
            strs(&got),
            *want,
            "cookie-argument row {} (file {:?}, browser {:?})",
            index, file, browser
        );
    }
}

#[test]
fn ytdlp_command_is_the_argv_the_process_is_handed() {
    crate::pin_test_dir();
    assert_eq!(CMD_CASES.len(), 30, "the argv corpus moved");
    for (index, (binary, url, file, browser, want)) in CMD_CASES
        .iter()
        .enumerate()
    {
        let cookies = crate::resolve::Cookies {
            file: *file,
            browser: *browser,
        };
        let got = crate::resolve::ytdlp_command(binary, url, &cookies);
        assert_eq!(
            strs(&got),
            *want,
            "argv row {} (binary {:?}, url {:?})",
            index, binary, url
        );
        assert!(
            got.len() == 8 || got.len() == 10,
            "argv row {} is {} long, and the file only ever answers 8 or 10",
            index,
            got.len()
        );
        let switches = got
            .iter()
            .filter(|row| **row == "--cookies" || **row == "--cookies-from-browser")
            .count();
        assert!(
            switches <= 1,
            "argv row {} carries both cookie switches; the pasted jar wins, so one run never gets both",
            index
        );
        if !super::py_strip(file).is_empty() {
            assert_eq!(
                switches,
                1,
                "argv row {} pastes a jar at {:?} and the engine must be pointed at it",
                index, file
            );
            assert_eq!(
                got[7], "--cookies",
                "argv row {} also names a browser ({:?}), but the file wins",
                index, browser
            );
            assert_eq!(
                got[8],
                super::py_strip(file),
                "argv row {} hands the engine a path the caller did not paste",
                index
            );
        } else if switches == 1 {
            assert_eq!(
                got[7],
                "--cookies-from-browser",
                "argv row {} gives no jar but carries a switch that is not the browser one",
                index
            );
            assert_eq!(
                crate::resolve::cookie_browser_spec(browser).as_deref(),
                Some(got[8].as_str()),
                "argv row {} hands the engine a browser spec the whitelist read does not admit",
                index
            );
        } else {
            assert_eq!(
                crate::resolve::cookie_browser_spec(browser),
                None,
                "argv row {} resolves anonymously, so browser {:?} must be a spec this read refuses",
                index, browser
            );
        }
    }
}

#[test]
fn engine_note_answers_every_shape_at_every_exit_code() {
    crate::pin_test_dir();
    assert_eq!(NOTE_SHAPES.len(), 29, "the stderr shapes moved");
    assert_eq!(
        NOTE_RCS,
        &[0, 1, 2, -15, 255][..],
        "the exit codes are the order the oracle wrote its rows in"
    );
    assert_eq!(NOTE_WANTS.len(), 145, "the note corpus moved");
    assert_eq!(
        NOTE_WANTS.len(),
        NOTE_SHAPES.len() * NOTE_RCS.len(),
        "the grid is shapes x exit codes, and both counts are the corpus's own"
    );
    for (shape, stderr) in NOTE_SHAPES.iter().enumerate() {
        for (code, rc) in NOTE_RCS.iter().enumerate() {
            let got = crate::resolve::engine_note(*rc, *stderr);
            assert_eq!(
                got,
                NOTE_WANTS[shape * NOTE_RCS.len() + code],
                "stderr shape {} at exit code {} answered {:?}",
                shape, rc, got
            );
        }
    }
}

#[test]
fn engine_note_treats_no_stderr_as_empty_stderr() {
    crate::pin_test_dir();
    assert!(
        NOTE_SHAPES[0].is_none(),
        "shape 0 is the row where the reference got no stderr at all"
    );
    assert_eq!(
        NOTE_SHAPES[1],
        Some(""),
        "shape 1 is empty stderr, and it is not shape 0"
    );
    for rc in NOTE_RCS {
        assert_eq!(
            crate::resolve::engine_note(*rc, None),
            crate::resolve::engine_note(*rc, Some("")),
            "exit code {} answers differently to None than to empty stderr",
            rc
        );
    }
}

#[test]
fn engine_note_clamps_at_the_limit_the_corpus_measured() {
    crate::pin_test_dir();
    assert_eq!(crate::resolve::ENGINE_NOTE_LIMIT, 200, "the recorded clamp");
    let longest = NOTE_WANTS.iter().map(|row| row.chars().count()).max().unwrap();
    assert_eq!(
        longest,
        crate::resolve::ENGINE_NOTE_LIMIT,
        "the corpus reaches the clamp, so the number is not one nothing touches"
    );
    let clamped = NOTE_WANTS
        .iter()
        .filter(|row| row.chars().count() == crate::resolve::ENGINE_NOTE_LIMIT)
        .count();
    assert_eq!(clamped, 20, "the corpus holds exactly this many clamped answers");
}

#[test]
fn needs_cookies_answers_the_word_table() {
    crate::pin_test_dir();
    assert_eq!(NEEDS_CASES.len(), 20, "the cookie-word corpus moved");
    for (index, (text, want)) in NEEDS_CASES.iter().enumerate() {
        assert_eq!(
            crate::resolve::needs_cookies(text),
            *want,
            "needs_cookies row {}: {:?}",
            index, text
        );
    }
}

#[test]
fn engine_says_unsupported_answers_the_extractor_phrases() {
    crate::pin_test_dir();
    assert_eq!(UNSP_CASES.len(), 13, "the unsupported-url probes moved");
    for (index, (text, want)) in UNSP_CASES.iter().enumerate() {
        assert_eq!(
            crate::resolve::engine_says_unsupported(text),
            *want,
            "engine_says_unsupported row {}: {:?}",
            index, text
        );
    }
}

#[test]
fn engine_says_unsupported_reads_the_recorded_words() {
    crate::pin_test_dir();
    assert_eq!(UNSP_WORDS.len(), 3, "the words row the oracle recorded has three phrases");
    assert_eq!(
        super::UNSUPPORTED_WORDS,
        UNSP_WORDS,
        "the word list is the answer the oracle recorded for _UNSUPPORTED_WORDS"
    );
    for word in UNSP_WORDS {
        assert!(
            crate::resolve::engine_says_unsupported(word),
            "{:?} is in the table but does not answer",
            word
        );
    }
}

#[test]
fn cookie_source_problem_answers_the_recorded_probes() {
    crate::pin_test_dir();
    assert_eq!(PROB_CASES.len(), 27, "the machine-failure probes moved");
    for (index, (text, want)) in PROB_CASES.iter().enumerate() {
        assert_eq!(
            crate::resolve::cookie_source_problem(text),
            *want,
            "cookie_source_problem row {}: {:?}",
            index, text
        );
    }
}

#[test]
fn cookie_problems_stay_in_the_order_that_makes_a_sentence_win() {
    crate::pin_test_dir();
    assert_eq!(
        crate::resolve::COOKIE_PROBLEMS,
        PROB_TUPLE,
        "the tuple order is the answer when two states' fragments sit in one sentence"
    );
    let table: Vec<&str> = super::COOKIE_SOURCE_TABLE
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        table,
        crate::resolve::COOKIE_PROBLEMS.to_vec(),
        "the table this read walks is in the order the tuple declares"
    );
    let not_found = super::COOKIE_SOURCE_TABLE
        .iter()
        .find(|(name, _)| *name == "not_found")
        .map(|(_, words)| words.to_vec())
        .expect("the table still carries a not_found row");
    assert!(
        PROB_CASES
            .iter()
            .filter(|probe| probe.1 == Some("locked"))
            .any(|probe| not_found.iter().any(|word| probe.0.contains(*word))),
        "the locked row must be asked first because a locked sentence also says not_found: {:?} appear in no recorded locked probe",
        not_found
    );
}

#[test]
fn every_declared_problem_is_reachable_by_its_own_probes() {
    crate::pin_test_dir();
    assert_eq!(
        PROB_TABLE_HITS.len(),
        crate::resolve::COOKIE_PROBLEMS.len(),
        "one hits row per declared problem state"
    );
    for (name, recorded) in PROB_TABLE_HITS {
        let counted = PROB_CASES
            .iter()
            .filter(|(_, want)| want.map(|word| word == *name).unwrap_or(false))
            .count();
        assert_eq!(
            counted,
            *recorded,
            "{} is named by {} of the recorded probes, not {}",
            name, recorded, counted
        );
        assert!(counted > 0, "{} has no probe that reaches it", name);
    }
}

#[test]
fn cookie_jar_state_counts_the_lines_the_reference_counts() {
    crate::pin_test_dir();
    assert_eq!(JAR_CASES.len(), 15, "the jar corpus moved");
    for (index, (text, want)) in JAR_CASES.iter().enumerate() {
        let got = crate::resolve::cookie_jar_state(text);
        assert_eq!(
            jar_answer(&got),
            jar_expectation(&Ok(*want)),
            "jar row {}: {:?}",
            index, text
        );
    }
}

#[test]
fn cookie_jar_state_measures_the_cap_in_bytes_not_characters() {
    crate::pin_test_dir();
    assert_eq!(JAR_CAP_CASES.len(), 7, "the byte-cap corpus moved");
    for (index, (ch, count, bytes, want)) in JAR_CAP_CASES.iter().enumerate() {
        let text = ch.repeat(*count);
        assert_eq!(
            text.len(),
            *bytes,
            "cap row {}: the repeat the oracle measured is not this long here",
            index
        );
        let got = crate::resolve::cookie_jar_state(&text);
        assert_eq!(
            jar_answer(&got),
            jar_expectation(want),
            "cap row {} ({} x {}) answered wrong",
            index, ch, count
        );
        assert_eq!(
            got.is_err(),
            want.is_err(),
            "cap row {}: a refusal and a count are not the same answer",
            index
        );
    }
}

#[test]
fn the_cookie_jar_constants_are_the_recorded_three() {
    crate::pin_test_dir();
    assert_eq!(crate::resolve::COOKIE_JAR_MAX_BYTES, 262144, "the recorded byte cap");
    assert_eq!(crate::resolve::COOKIE_JAR_FIELDS, 7, "the recorded field count");
    assert_eq!(
        crate::resolve::HTTPOONLY_PREFIX,
        "#HttpOnly_",
        "the prefix that marks a cookie line rather than a comment"
    );
    assert_eq!(JAR_LIMIT_TRIPLE.0, 256 * 1024, "the recorded cap is the documented 256 KB");
}

#[test]
fn timeout_sentence_reproduces_python_command_repr() {
    crate::pin_test_dir();
    assert_eq!(TOUT_CASES.len(), 41, "the timeout corpus moved");
    for (index, (argv, secs, nanos, want)) in TOUT_CASES.iter().enumerate() {
        let got = super::timeout_sentence(&owned(argv), Duration::new(*secs, *nanos));
        assert_eq!(
            got,
            *want,
            "timeout row {} (argv {:?}, {}.{} s)",
            index, argv, secs, nanos
        );
    }
}

#[test]
fn timeout_seconds_is_the_one_repr_a_duration_cannot_answer() {
    crate::pin_test_dir();
    assert_eq!(TOUTX_CASES.len(), 2, "the repr-divergence rows moved");
    for (index, (argv, secs, nanos, want, prefix)) in TOUTX_CASES
        .iter()
        .enumerate()
    {
        let got = super::timeout_sentence(&owned(argv), Duration::new(*secs, *nanos));
        assert_ne!(
            got,
            *want,
            "timeoutx row {}: this is the recorded divergence, it must still diverge",
            index
        );
        assert!(
            got.starts_with(*prefix),
            "timeoutx row {}: only the number may differ, not the framing: {:?} against {:?}",
            index, got, want
        );
        assert!(
            want.starts_with(*prefix),
            "timeoutx row {}: the prefix came from this row",
            index
        );
    }
}

#[test]
fn the_engine_corpus_is_the_recorded_six_hundred_and_one_rows() {
    crate::pin_test_dir();
    let rows = ARG_CASES.len()
        + CMD_CASES.len()
        + NOTE_WANTS.len()
        + NEEDS_CASES.len()
        + UNSP_CASES.len()
        + 1
        + PROB_CASES.len()
        + 1
        + PROB_TABLE_HITS.len()
        + JAR_CASES.len()
        + JAR_CAP_CASES.len()
        + 1
        + TOUT_CASES.len()
        + TOUTX_CASES.len();
    assert_eq!(
        rows,
        601,
        "the engine corpus is the 601 rows the oracle wrote, counted the same way here"
    );
}

