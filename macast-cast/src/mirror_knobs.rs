//! Screen mirroring: the three knobs, and the arithmetic behind them.
//!
//! Rust counterpart of `screen_mirror.py`'s `dlna_prefill_seconds_setting` (`:687`),
//! `dlna_prefill_bytes` (`:703`), `dlna_prefill_seconds` (`:717`), `live_edge_seconds` (`:234`),
//! `max_duration_hours` (`:1525`), `bound_video_seconds` (`:1540`) and `duration_bound`
//! (`:1553`).
//!
//! ## Every one of these clamps rather than rejects, and that is the design
//!
//! The settings JSON is hand-editable. A rejected value is therefore not "the user is told they
//! typed something wrong"; it is a **running mirror whose knob silently does nothing**, or a
//! television that stops playing with no explanation. So a stored value out of range is pulled
//! into range, and the value that reaches the encoder is always one the code can act on.
//!
//! The one exception is `max_duration_hours`, which **writes back** — `set-max-duration` refuses
//! a bad value at the page (a picker is not the same as a JSON file), while a bad stored value
//! still clamps, because a setting page that will not open is worse than a clamped knob.
//!
//! ## Why some of these read the byte budget instead of the setting
//!
//! `dlna_prefill_seconds` is derived from [`dlna_prefill_bytes`], **not** read back from the
//! setting. The byte budget has a 2 MiB floor and an 8 MiB ceiling, so either can move the real
//! delay away from what was asked; reporting the setting instead would tell the user a number
//! that is not the delay they are getting. Two answers disagreeing is worse than either being
//! approximate.
//!
//! ## The maximum duration exists because the numbers must agree
//!
//! `bound_video_seconds()` is the single truth for how long one television session may encode.
//! Both halves read it: ffmpeg's `-t` (when the bytes stop) and the advertised file length (what
//! the television is told). Those two were computed separately exactly once, and what that
//! produced is a file shape whose advertised length is **further than the encoder ever writes** —
//! the Android-11-TV failure: the device reads to the end, goes looking for an index that is not
//! there, and never reaches `PLAYING`.

/// Seconds of prefill the DLNA target asks for, when the user has not chosen.
///
/// **The default is 4, not the 1-second floor**, and the reason is a measurement on somebody
/// else's hardware: MirrorCast pre-fills about 20 MiB (~20–25 s) on a real Philips 43PFS5301
/// (2016, non-Android) and says so. So firmware that wants *more* than our floor demonstrably
/// exists, and this machine has no such television to find **our** floor on. A number that
/// cannot be measured here is handed to whoever can measure it, with the cost written next to it.
pub const DLNA_PREFILL_SECONDS: u32 = 4;
pub const DLNA_PREFILL_MIN_SECONDS: u32 = 1;
pub const DLNA_PREFILL_MAX_SECONDS: u32 = 8;

/// The byte budget's own floor and ceiling.
pub const DLNA_PREFILL_MIN_BYTES: u64 = 2 << 20;
pub const DLNA_PREFILL_MAX_BYTES: u64 = 8 << 20;

/// The sentence that must sit next to the knob: this number **is** the visible delay.
pub const DLNA_PREFILL_HINT: &str =
    "这个数字就是看得见的延迟：调小一点，电视开始播放得更早，但更容易卡";

/// Where the browser viewer parks behind the live edge.
///
/// `0.5` is the floor, and the reason changed once. The old reason was "the fMP4 fragment cadence
/// is 12 frames = 0.5 s, so 1.0 s leaves two fragments of slack" — that reasoning died with the
/// v0.19 muxer change, which made fragments one frame (41.7 ms), so 0.5 s is now twelve
/// fragments. What holds today is a measurement: 0.5 and 1.0 differ by **9 ms** (829 against
/// 838) with zero seeks, so the knob stopped being about latency and became about **recovery
/// distance** — how far behind the viewer sits when a fragment has to be skipped.
pub const LIVE_EDGE_SECONDS: f64 = 1.0;
pub const LIVE_EDGE_MIN_SECONDS: f64 = 0.5;
pub const LIVE_EDGE_MAX_SECONDS: f64 = 5.0;

/// The allowed values, in the order the picker shows them.
pub const LIVE_EDGE_OPTIONS: &[f64] = &[0.5, 1.0, 2.0, 3.0, 5.0];

/// Clamp the prefill setting into range.
///
/// A stored value outside the range is pulled in rather than rejected: the alternative is a
/// television that stops playing because somebody typed 40 into the settings JSON, with no
/// explanation.
pub fn dlna_prefill_seconds_setting(stored: Option<i64>) -> u32 {
    match stored {
        None => DLNA_PREFILL_SECONDS,
        Some(value) => value
            .clamp(DLNA_PREFILL_MIN_SECONDS as i64, DLNA_PREFILL_MAX_SECONDS as i64)
            as u32,
    }
}

/// The prefill budget for one DLNA shape, in bytes.
///
/// One argument on purpose. The Python suite replaces this with a stub of exactly that shape to
/// take the buffer out of a test; a second parameter would turn every such stub into a `TypeError`
/// somewhere unrelated to what the stub is testing.
pub fn dlna_prefill_bytes(total_bitrate: u64, stored_seconds: Option<i64>) -> u64 {
    let wanted = total_bitrate * dlna_prefill_seconds_setting(stored_seconds) as u64 / 8;
    wanted.clamp(DLNA_PREFILL_MIN_BYTES, DLNA_PREFILL_MAX_BYTES)
}

/// What that budget costs the user, in seconds — what the start message reports.
///
/// Derived from the **byte budget**, not read back from the setting, so a budget the clamp moved
/// (2 MiB floor, 8 MiB ceiling) still reports the delay the user is actually getting.
pub fn dlna_prefill_seconds(total_bitrate: u64, stored_seconds: Option<i64>) -> u64 {
    if total_bitrate == 0 {
        return 1;
    }
    let bytes = dlna_prefill_bytes(total_bitrate, stored_seconds);
    (bytes * 8 / total_bitrate).max(1)
}

/// Clamp the live-edge setting into range.
pub fn live_edge_seconds(stored: Option<f64>) -> f64 {
    match stored {
        None => LIVE_EDGE_SECONDS,
        Some(value) if !value.is_finite() => LIVE_EDGE_SECONDS,
        Some(value) => value.clamp(LIVE_EDGE_MIN_SECONDS, LIVE_EDGE_MAX_SECONDS),
    }
}

/// The three choices for how long one television session may run.
///
/// **Deliberately no "unlimited"**: the default on this page was never touched by anybody, and
/// "nobody touched it" is exactly the state this knob exists to end.
pub const MAX_DURATION_HOURS: &[u32] = &[12, 24, 48];

/// The default ceiling.
pub const DEFAULT_MAX_DURATION_HOURS: u32 = 12;

/// The shapes that obey the ceiling.
///
/// Only the two television targets: `cast` LOADs a live stream into a device that will sit on it
/// indefinitely, and `dlna` hands a renderer a file with a fabricated length it will read to the
/// end. A browser viewer that ran too long reloads its page; the two low-latency channels are
/// live-edge consumers whose receivers hold no backlog worth protecting a television from, so
/// putting `-t` on them would truncate a presentation to save nothing.
pub const MAX_DURATION_TARGETS: &[&str] = &["cast", "dlna"];

/// The stored ceiling, clamped the way the prefill setting is.
pub fn max_duration_hours(stored: Option<i64>) -> u32 {
    match stored {
        Some(value) if MAX_DURATION_HOURS.contains(&(value as u32)) => value as u32,
        // An unknown value is the default rather than a refusal: the alternative is a settings
        // JSON edit that ends a presentation mid-sentence, with no explanation to whoever is
        // watching.
        _ => DEFAULT_MAX_DURATION_HOURS,
    }
}

/// The single truth about how long one television session may encode.
///
/// Both halves of the promise read this: ffmpeg's `-t` and the advertised file length.
pub fn bound_video_seconds(stored: Option<i64>) -> u64 {
    max_duration_hours(stored) as u64 * 3600
}

/// [`bound_video_seconds`] for a shape that obeys it, else `None`.
///
/// `None` means "no `-t` on this argv", which is the honest answer for the three live shapes and
/// what the diagnostics card leaves off rather than printing a blank row.
pub fn duration_bound(target: &str, stored: Option<i64>) -> Option<u64> {
    if MAX_DURATION_TARGETS.contains(&target) {
        Some(bound_video_seconds(stored))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefill_setting_clamps_instead_of_refusing() {
        crate::pin_test_dir();
        // A rejected value is a television that stops playing because somebody typed 40 into a
        // JSON file — with no explanation. Clamping always yields something the code can act on.
        assert_eq!(dlna_prefill_seconds_setting(None), DLNA_PREFILL_SECONDS);
        assert_eq!(dlna_prefill_seconds_setting(Some(40)), DLNA_PREFILL_MAX_SECONDS);
        assert_eq!(dlna_prefill_seconds_setting(Some(0)), DLNA_PREFILL_MIN_SECONDS);
        assert_eq!(dlna_prefill_seconds_setting(Some(-5)), DLNA_PREFILL_MIN_SECONDS);
        assert_eq!(dlna_prefill_seconds_setting(Some(2)), 2);
    }

    #[test]
    fn the_default_is_not_the_floor_because_a_television_needing_more_demonstrably_exists() {
        crate::pin_test_dir();
        // MirrorCast pre-fills ~20 MiB on a real Philips 43PFS5301; firmware wanting more than
        // our floor exists, and this machine has no such television to find our floor on.
        assert_eq!(DLNA_PREFILL_SECONDS, 4);
        assert!(DLNA_PREFILL_SECONDS > DLNA_PREFILL_MIN_SECONDS);
        // …and the cost is printed next to the knob, so nobody has to discover it.
        assert!(DLNA_PREFILL_HINT.contains("延迟"));
    }

    #[test]
    fn the_byte_budget_is_clamped_at_both_ends() {
        crate::pin_test_dir();
        // A tiny bitrate cannot ask for less than the floor, and a huge one cannot exceed the
        // ceiling: the former is unplayable, the latter is a longer wait than anyone asked for.
        let floor = dlna_prefill_bytes(1_000, Some(1));
        assert_eq!(floor, DLNA_PREFILL_MIN_BYTES);
        let ceiling = dlna_prefill_bytes(100_000_000, Some(8));
        assert_eq!(ceiling, DLNA_PREFILL_MAX_BYTES);
    }

    #[test]
    fn the_reported_seconds_come_from_the_budget_not_from_the_setting() {
        crate::pin_test_dir();
        // A budget the clamp moved must report the delay the user is *getting*. Reading the
        // setting instead would say "1 second" while the floor holds 2 MiB.
        let tiny = dlna_prefill_seconds(1_000, Some(1));
        assert!(tiny >= 1, "the floor still costs real time");
        // For a normal bitrate the reported figure tracks the setting closely.
        let normal = dlna_prefill_seconds(8_000_000, Some(4));
        assert!((3..=5).contains(&normal), "reported {normal}s for a 4 s setting");
        // …and never answers zero, which would be a start message claiming no delay.
        assert!(dlna_prefill_seconds(1, Some(1)) >= 1);
        assert!(dlna_prefill_seconds(0, Some(1)) >= 1);
    }

    #[test]
    fn the_live_edge_clamps_and_rejects_nonsense() {
        crate::pin_test_dir();
        assert_eq!(live_edge_seconds(None), LIVE_EDGE_SECONDS);
        assert_eq!(live_edge_seconds(Some(0.1)), LIVE_EDGE_MIN_SECONDS);
        assert_eq!(live_edge_seconds(Some(99.0)), LIVE_EDGE_MAX_SECONDS);
        assert_eq!(live_edge_seconds(Some(2.0)), 2.0);
        // A NaN would propagate into the injected page value; the default is the safe answer.
        assert_eq!(live_edge_seconds(Some(f64::NAN)), LIVE_EDGE_SECONDS);
        // The floor is 0.5, not 0: a viewer parked at zero has no recovery distance at all.
        assert_eq!(LIVE_EDGE_MIN_SECONDS, 0.5);
        assert!(LIVE_EDGE_OPTIONS.contains(&LIVE_EDGE_MIN_SECONDS));
        assert!(LIVE_EDGE_OPTIONS.contains(&LIVE_EDGE_SECONDS));
    }

    #[test]
    fn the_duration_ceiling_has_three_values_and_no_unlimited() {
        crate::pin_test_dir();
        // "Nobody touched the default" is the state this knob exists to end.
        assert_eq!(MAX_DURATION_HOURS.len(), 3);
        assert!(!MAX_DURATION_HOURS.contains(&0));
        assert_eq!(DEFAULT_MAX_DURATION_HOURS, 12);
        assert!(MAX_DURATION_HOURS.contains(&DEFAULT_MAX_DURATION_HOURS));
    }

    #[test]
    fn an_unknown_duration_clamps_to_the_default_rather_than_refusing() {
        crate::pin_test_dir();
        // A refusal here ends a presentation mid-sentence with no explanation.
        assert_eq!(max_duration_hours(None), DEFAULT_MAX_DURATION_HOURS);
        assert_eq!(max_duration_hours(Some(40)), DEFAULT_MAX_DURATION_HOURS);
        assert_eq!(max_duration_hours(Some(0)), DEFAULT_MAX_DURATION_HOURS);
        assert_eq!(max_duration_hours(Some(-1)), DEFAULT_MAX_DURATION_HOURS);
        assert_eq!(max_duration_hours(Some(24)), 24);
        assert_eq!(max_duration_hours(Some(48)), 48);
    }

    #[test]
    fn only_the_two_television_targets_obey_the_ceiling() {
        crate::pin_test_dir();
        // The live-edge shapes hold no backlog worth protecting a television from, so `-t` there
        // would truncate a presentation to save nothing.
        assert_eq!(duration_bound("cast", None), Some(43_200));
        assert_eq!(duration_bound("dlna", Some(24)), Some(86_400));
        for live in ["browser", "webrtc", "caststream"] {
            assert_eq!(duration_bound(live, None), None, "{live} must not be truncated");
        }
        assert_eq!(MAX_DURATION_TARGETS, &["cast", "dlna"]);
    }

    /// Every knob answer the reference computed, replayed against this port.
    ///
    /// 50 rows from executing the reference's clamp/derive functions: ten stored prefill values
    /// (including `None`, `0`, `40` and a negative), six bitrates × four settings for the byte
    /// budget and the reported seconds, nine live-edge values, seven duration values.
    ///
    /// The extreme row — `rate=1000` reporting **16777 s** — is the floor asserting itself: 2 MiB
    /// at 1 kbit/s genuinely is most of five hours, and the reference reports that rather than a
    /// prettier number. It is in the corpus so nobody "fixes" it into a lie.
    #[test]
    fn every_reference_knob_answer_replays_identically() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/mirror_knobs.json"))
                .expect("corpus is JSON");
        let rows = corpus.as_array().expect("rows");
        let mut kinds = std::collections::BTreeSet::new();
        for row in rows {
            let kind = row["kind"].as_str().expect("kind");
            kinds.insert(kind.to_string());
            match kind {
                "prefill_setting" => {
                    assert_eq!(
                        dlna_prefill_seconds_setting(row["stored"].as_i64()) as u64,
                        row["out"].as_u64().expect("out"),
                        "stored={:?}",
                        row["stored"]
                    );
                }
                "prefill" => {
                    let rate = row["rate"].as_u64().expect("rate");
                    let stored = row["stored"].as_i64();
                    assert_eq!(
                        dlna_prefill_bytes(rate, stored),
                        row["bytes"].as_u64().expect("bytes"),
                        "bytes for rate={rate} stored={stored:?}"
                    );
                    assert_eq!(
                        dlna_prefill_seconds(rate, stored),
                        row["seconds"].as_u64().expect("seconds"),
                        "seconds for rate={rate} stored={stored:?}"
                    );
                }
                "live_edge" => {
                    let stored = row["stored"].as_f64();
                    let expected = row["out"].as_f64().expect("out");
                    let found = live_edge_seconds(stored);
                    assert!(
                        (found - expected).abs() < 1e-9,
                        "live_edge({stored:?}) = {found}, expected {expected}"
                    );
                }
                "max_hours" => {
                    assert_eq!(
                        max_duration_hours(row["stored"].as_i64()) as u64,
                        row["out"].as_u64().expect("out"),
                        "stored={:?}",
                        row["stored"]
                    );
                }
                other => panic!("unknown corpus kind {other}"),
            }
        }
        assert_eq!(kinds.len(), 4, "the corpus lost a knob: {kinds:?}");
        assert_eq!(rows.len(), 50, "the corpus lost rows");
    }

    #[test]
    fn the_bound_and_the_advertised_length_read_one_function() {
        crate::pin_test_dir();
        // The failure this prevents: a file shape whose advertised length is further than the
        // encoder ever writes — the device reads to the end, looks for an index that is not
        // there, and never reaches PLAYING.
        for hours in MAX_DURATION_HOURS {
            assert_eq!(
                bound_video_seconds(Some(*hours as i64)),
                *hours as u64 * 3600
            );
        }
        // The default is 12 h.
        assert_eq!(bound_video_seconds(None), 43_200);
    }
}
