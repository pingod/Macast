//! The yt-dlp renderer: download to disk, or stream through mpv via `ytdl_hook`.
//!
//! Rust counterpart of `macast/plugins/renderer/macast_ytdlp.py`'s decision layer
//! (`:88-190`). Two modes, one argv merge, and three small parsers.
//!
//! ## The argv trap this module exists to get right
//!
//! In stream mode the plugin tells mpv where `yt-dlp` lives by setting
//! `ytdl_hook-ytdl_path` **inside the existing `--script-opts` value**. Appending a second
//! `--script-opts=` would be the obvious thing and it is wrong: mpv's key/value-list options
//! **replace** rather than merge, so the base class's OSC options would silently disappear —
//! the video plays but the on-screen controls are gone, and nothing anywhere says why.
//!
//! ## Three parsers, each with a job
//!
//! * [`eta_seconds`] reads yt-dlp's own ETA text (`00:12`, `1:02:03`, `42`) for the progress
//!   card. Anything unparseable is `0`, not an error: an ETA we cannot read is a cosmetic
//!   loss, and a download that fails over its own progress string would be absurd.
//! * [`hms`] formats a position for the DLNA state table.
//! * [`stream_mode`] reads the mode **numerically**, because the constants are `0` (download)
//!   and `1` (stream) and `0` is falsy — the same trap as the floating renderer's mode.
//!
//! ## What is not here
//!
//! No spawning, no `PATH` search, no filesystem: the caller supplies `ytdlp_path` as an input.
//! The download itself is a child process with a progress reader, and that belongs with the
//! other process plumbing.

/// `0` downloads to disk, `1` streams through mpv.
pub const MODE_DOWNLOAD: i64 = 0;
pub const MODE_STREAM: i64 = 1;

/// The settings keys this renderer reads.
pub const KEY_DIR: &str = "YTDLP_Dir";
pub const KEY_MODE: &str = "YTDLP_Mode";

/// Whether streaming was asked for.
///
/// Read **numerically**: `0` is download and `0` is falsy, so a truthiness test would turn
/// every download installation into a stream.
pub fn stream_mode(value: Option<i64>) -> bool {
    value == Some(MODE_STREAM)
}

/// `00:12` / `1:02:03` / `42` → seconds. `0` when unparseable.
///
/// Accepts the two colon-separated shapes yt-dlp prints and a bare integer/float, which is
/// what the reference's `ETA_RE` plus its `int(float(...))` fallback accept. Deliberately
/// permissive about a missing hours field, because `00:12` is what a short download prints.
pub fn eta_seconds(text: &str) -> i64 {
    let text = text.trim();
    if text.is_empty() {
        return 0;
    }
    if text.contains(':') {
        // Split into at most three fields, right-aligned: `12:34` is minutes:seconds,
        // `1:02:03` is hours:minutes:seconds.
        //
        // **Fields are parsed untrimmed**, because the reference's `ETA_RE`
        // (`^(?:(\d+):)?(\d+):(\d+)$`) has no `\s` and runs on the already-stripped text: an
        // inner space like `"1 :02:03"` simply does not match, so Python falls through to
        // `int(float(...))` and raises `ValueError` → 0. Trimming each field made this port
        // accept a string the reference refuses, which the corpus caught as a divergence.
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() > 3 || parts.iter().any(|part| part.is_empty()) {
            return 0;
        }
        let mut seconds = 0i64;
        for part in &parts {
            match part.parse::<i64>() {
                Ok(value) if value >= 0 => seconds = seconds.saturating_mul(60).saturating_add(value),
                _ => return 0,
            }
        }
        return seconds;
    }
    // A bare number, possibly with a fractional part (`42.7`).
    //
    // Three divergences from Python's `int(float(text))` used to live here, and the third is a
    // real defect rather than a tidy-up:
    //
    // 1. **Underscores.** `float("1_000")` is 1000 in Python; `str::parse::<f64>` rejects it. A
    //    real yt-dlp never prints one, but the progress regex captures `<\S+>` and this function
    //    is the only thing standing between that and a state write.
    // 2. **Unicode digits.** `float("١٢")` is 12 and `int(float("𝟜𝟚"))` is 42 — Python accepts
    //    any `Nd` decimal digits. Rather than port that, the ASCII-only answer is kept and named
    //    here: an ETA label in Arabic-Indic digits would read as "no estimate", which is the
    //    cosmetic loss this function already tolerates.
    // 3. **Overflow.** `float("1e30")` gives 1e30 and Python's `int()` of it is a 31-digit
    //    integer, so `elapsed + eta` stays exact there. A cast to `i64` **saturates** at
    //    `i64::MAX` (that is Rust's defined behaviour for `as`, not something this code arranges),
    //    which keeps the number large and honest. The residual risk is on the *caller*: an
    //    `elapsed + eta` in `i64` would wrap negative and `hms` would clamp that to `0:00:00` —
    //    a progress bar reading "just started" on a nearly-finished download. No caller exists
    //    yet; whoever writes one owes it a saturating add, and this note is the reason.
    let parsed = match text.parse::<f64>() {
        Ok(value) if value.is_finite() => value as i64,
        // `inf`/`-inf` raise `OverflowError` in Python — *not* `ValueError` — so the reference's
        // `except ValueError` does not catch them and the progress thread dies. Answering 0 is
        // the one behaviour that keeps a download going, and it is a deliberate divergence.
        Ok(_) => 0,
        Err(_) => {
            // The underscore form, which Python's float parser accepts and Rust's does not.
            let cleaned: String = text.chars().filter(|c| *c != '_').collect();
            match cleaned.parse::<f64>() {
                Ok(value) if value.is_finite() => value as i64,
                _ => 0,
            }
        }
    };
    parsed
}

/// Seconds → `H:MM:SS`, clamped at zero.
///
/// The clamp is not cosmetic: the DLNA state table feeds this straight to a device, and a
/// negative clock is a protocol error rather than a small inaccuracy.
pub fn hms(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Merge `ytdl_hook-ytdl_path=<binary>` into the `--script-opts` value.
///
/// Returns the params unchanged when `binary` is `None` (yt-dlp is not installed): stream
/// mode without the hook would hand mpv a URL it cannot resolve, so the caller reports the
/// missing binary separately and this function stays a pure merge.
///
/// The first `--script-opts=` wins and is extended in place. If there is none, one is
/// appended. Both halves matter: extending keeps the OSC options that are already there, and
/// appending covers a base class that sets none.
pub fn merge_ytdl_hook(params: Vec<String>, binary: Option<&str>) -> Vec<String> {
    let binary = match binary {
        Some(path) if !path.is_empty() => path,
        _ => return params,
    };
    let option = format!("ytdl_hook-ytdl_path={binary}");
    let mut params = params;
    for param in params.iter_mut() {
        if let Some(existing) = param.strip_prefix("--script-opts=") {
            *param = format!("--script-opts={existing},{option}");
            return params;
        }
    }
    params.push(format!("--script-opts={option}"));
    params
}

/// The whole decision for `build_mpv_params`: only stream mode with a real binary changes
/// anything, and it changes the params **in place**.
pub fn params_for(base: Vec<String>, stream: bool, binary: Option<&str>) -> Vec<String> {
    if !stream {
        return base;
    }
    merge_ytdl_hook(base, binary)
}

/// The message when stream mode needs a binary the machine does not have.
///
/// Names both installers, because the plugin's own text does and a bare "yt-dlp not found"
/// leaves a Homebrew user and a pipx user equally stuck.
pub const MISSING_YTDLP_MESSAGE: &str =
    "未找到 yt-dlp，请先安装：brew install yt-dlp / pipx install yt-dlp";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_is_read_as_a_number_because_zero_is_the_download_mode() {
        // Truthiness here would make every default installation try to stream.
        assert!(!stream_mode(Some(MODE_DOWNLOAD)));
        assert!(stream_mode(Some(MODE_STREAM)));
        assert!(!stream_mode(None));
        assert!(!stream_mode(Some(7)));
    }

    #[test]
    fn eta_reads_the_three_shapes_ytdlp_prints() {
        assert_eq!(eta_seconds("00:12"), 12);
        assert_eq!(eta_seconds("1:02:03"), 3723);
        assert_eq!(eta_seconds("42"), 42);
        // A fractional bare number is what a very short download prints.
        assert_eq!(eta_seconds("42.7"), 42);
        // Whitespace is the caller's to trim, and this does it too.
        assert_eq!(eta_seconds(" 0:07 "), 7);
    }

    #[test]
    fn an_unreadable_eta_is_zero_and_never_an_error() {
        // A download failing over its own progress string would be absurd, so every one of
        // these degrades to "no estimate". Measured against the reference: all of these
        // answer 0 there too.
        for text in ["", "   ", "unknown", "1:2:3:4", "1::3", ":", "1:-2", "-1:02:03"] {
            assert_eq!(eta_seconds(text), 0, "{text:?} should read as no estimate");
        }
    }

    #[test]
    fn a_negative_bare_number_survives_because_the_reference_lets_it() {
        // This is a deliberate non-divergence, not an oversight. The reference's fallback is
        // `int(float(text))`, which happily returns -5 for "-5" — the colon shapes are
        // regex-guarded but the bare-number path is not. Rejecting it here would be a silent
        // behaviour change in the direction of "more correct, differently answering", and the
        // only consumer is a cosmetic progress label; a caller who needs a clock clamps it
        // through `hms`, which does clamp.
        assert_eq!(eta_seconds("-5"), -5);
        assert_eq!(hms(eta_seconds("-5")), "0:00:00");
    }

    #[test]
    fn hms_formats_and_clamps() {
        assert_eq!(hms(0), "0:00:00");
        assert_eq!(hms(12), "0:00:12");
        assert_eq!(hms(3723), "1:02:03");
        // A negative clock is a protocol error on the wire, not a small inaccuracy.
        assert_eq!(hms(-5), "0:00:00");
    }

    #[test]
    fn the_hook_path_is_merged_into_the_existing_script_opts() {
        // The whole reason this module exists: a second `--script-opts=` would replace the
        // first, and mpv's OSC controls would silently vanish.
        let base = vec![
            "--no-config".to_string(),
            "--script-opts=osc-visibility=always,osc-idlescreen=no".to_string(),
            "--volume=80".to_string(),
        ];
        let merged = merge_ytdl_hook(base, Some("/opt/homebrew/bin/yt-dlp"));
        let script_opts: Vec<&String> = merged
            .iter()
            .filter(|p| p.starts_with("--script-opts="))
            .collect();
        assert_eq!(
            script_opts.len(),
            1,
            "a second --script-opts= appeared, which replaces the first"
        );
        assert_eq!(
            script_opts[0],
            "--script-opts=osc-visibility=always,osc-idlescreen=no,ytdl_hook-ytdl_path=/opt/homebrew/bin/yt-dlp"
        );
        // …and nothing else was reordered or dropped.
        assert_eq!(merged[0], "--no-config");
        assert_eq!(merged[2], "--volume=80");
    }

    #[test]
    fn with_no_script_opts_to_extend_one_is_appended() {
        let base = vec!["--no-config".to_string()];
        let merged = merge_ytdl_hook(base, Some("/usr/bin/yt-dlp"));
        assert_eq!(
            merged,
            vec![
                "--no-config".to_string(),
                "--script-opts=ytdl_hook-ytdl_path=/usr/bin/yt-dlp".to_string(),
            ]
        );
    }

    #[test]
    fn a_missing_binary_leaves_the_params_untouched() {
        let base = vec!["--script-opts=osc-visibility=always".to_string()];
        assert_eq!(merge_ytdl_hook(base.clone(), None), base);
        assert_eq!(merge_ytdl_hook(base.clone(), Some("")), base);
    }

    #[test]
    fn download_mode_changes_nothing_and_stream_mode_needs_a_binary() {
        let base = vec!["--no-config".to_string()];
        assert_eq!(params_for(base.clone(), false, Some("/usr/bin/yt-dlp")), base);
        // Stream mode without a binary must also change nothing here — the caller answers
        // with MISSING_YTDLP_MESSAGE instead of handing mpv an unresolvable url.
        assert_eq!(params_for(base.clone(), true, None), base);
        let streamed = params_for(base, true, Some("/usr/bin/yt-dlp"));
        assert!(streamed.iter().any(|p| p.contains("ytdl_hook-ytdl_path=")));
    }

    /// Every ETA string the reference answered **or raised on**, replayed against this port.
    ///
    /// 39 rows: the ordinary shapes, the malformed colon forms, and the eleven inputs a
    /// differential probe turned up (`1_000`, `1e30`, `nan`, `0x10`, `1e400`, `inf`, …).
    ///
    /// Two classes of row, and the second is a **declared divergence**:
    ///
    /// * a `ValueError` means the reference's own `except` caught it and answered 0 — the port
    ///   must answer 0 too;
    /// * an `OverflowError` (`inf`, `-inf`, `1e400`) is **not** caught by `except ValueError`, so
    ///   the reference's progress thread dies. Answering 0 keeps a download going, and that is a
    ///   deliberate difference rather than an alignment failure.
    #[test]
    fn every_reference_eta_answer_replays_identically_except_the_declared_ones() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/ytdlp_eta.json"))
                .expect("corpus is JSON");
        let rows = corpus.as_array().expect("rows");
        let mut answered = 0usize;
        let mut raised = 0usize;
        let mut overflows = 0usize;
        for row in rows {
            let text = row["eta_text"].as_str().expect("eta_text");
            let raises = row["raises"].as_str();
            let got = eta_seconds(text);
            match raises {
                None if row.get("exceeds_i64").is_some() => {
                    // Python's `int` is unbounded and this port's is not: `1e30` becomes a
                    // 31-digit integer there and `i64::MAX` here. What must hold is that the
                    // port stays at the **large** end rather than wrapping to something small
                    // (a wrapped value would make `hms` clamp to `0:00:00` and report a
                    // nearly-finished download as "just started").
                    assert_eq!(got, i64::MAX, "eta_seconds({text:?}) must saturate high");
                    assert!(row["seconds"].as_i64().is_none(), "the corpus is self-consistent");
                    answered += 1;
                }
                None => {
                    assert_eq!(
                        got,
                        row["seconds"].as_i64().expect("seconds"),
                        "eta_seconds({text:?}) disagrees"
                    );
                    assert_eq!(
                        hms(got),
                        row["hms"].as_str().unwrap_or(""),
                        "hms for {text:?} disagrees"
                    );
                    answered += 1;
                }
                Some("ValueError") => {
                    // The reference's own `except` answered 0, so this is an alignment. The
                    // regenerated corpus has **no** rows here: `eta_seconds` swallows ValueError
                    // itself, so a `raises: ValueError` row was an artifact of the first
                    // generator calling `int(float(...))` directly instead of the function.
                    assert_eq!(got, 0, "eta_seconds({text:?}) should answer 0");
                    raised += 1;
                }
                Some("OverflowError") => {
                    // **Declared divergence**: the reference dies here, this port answers 0.
                    assert_eq!(got, 0, "eta_seconds({text:?}) answers 0 on purpose");
                    overflows += 1;
                }
                Some(other) => panic!("unexpected exception {other} for {text:?}"),
            }
        }
        assert_eq!(rows.len(), 39, "the corpus lost rows");
        assert_eq!(answered, 36, "answerable rows");
        assert_eq!(raised, 0, "ValueError rows: the reference swallows them itself");
        assert_eq!(overflows, 3, "OverflowError rows — the declared divergence");
        // Every row either answered or was the declared OverflowError class; a corpus that was
        // mostly refusals would pass every refusal branch and prove nothing.
        assert_eq!(answered + raised + overflows, rows.len());
    }

    #[test]
    fn the_missing_binary_message_names_both_installers() {
        // A bare "not found" leaves a Homebrew user and a pipx user equally stuck.
        assert!(MISSING_YTDLP_MESSAGE.contains("brew install"));
        assert!(MISSING_YTDLP_MESSAGE.contains("pipx install"));
        assert_eq!(KEY_DIR, "YTDLP_Dir");
        assert_eq!(KEY_MODE, "YTDLP_Mode");
    }
}
