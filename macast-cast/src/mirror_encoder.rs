//! Screen mirroring: which encoder, at what bitrate, with which level.
//!
//! Rust counterpart of `screen_mirror.py`'s `HARDWARE_ENCODERS` (`:3136`),
//! `encoder_pix_fmt` (`:3162`), `VT_BITRATE_MULT` / `rate_target` (`:3235-3245`),
//! `VT_LEVELS` / `vt_level` (`:3249-3300`) and the hardware/software branch of
//! `encoder_args` (`:3303-`).
//!
//! ## The bug this module's shape exists to prevent, and it was ours
//!
//! `-level 42` was **hardcoded** for `h264_videotoolbox`. Level 4.2 caps a frame at 8704
//! macroblocks (2208×1242), while this machine's desktop is 3456×2234 = 30384 of them. The
//! encoder is then handed a promise it cannot keep and dies: exit **187**, five lines of
//! stderr, **and not one output byte**. Measured on 2026-10-02, same capture and rate control in
//! every row:
//!
//! | picture | `-level` | result |
//! |---|---|---|
//! | 1080p | 42 | works, 3/3 |
//! | 2160p | 42 | exit 187, **0 bytes** |
//! | 2160p | 51 | works, 2 130 382 bytes |
//! | 2160p | *(none)* | works, VT picks its own |
//! | 3456×2234 | 42 | exit 187, **0 bytes**, 2/2 |
//! | 3456×2234 | 51 | works, 1 797 725 bytes |
//! | 3456×2234 | *(none)* | works, 1 799 372 bytes |
//!
//! Why that is worse than an ordinary crash: `auto` picks hardware whenever the probe finds it,
//! so on a Mac the shipped default for the 原画 preset was **"produce zero frames"** — and the
//! no-frame path sends the user to the **screen-recording permission** door, which is the wrong
//! door for a number we typed ourselves. `has_hardware_encoder` cannot catch it: it greps
//! `ffmpeg -encoders`, which happily lists an encoder that then refuses the picture.
//!
//! ## The rule that came out of it
//!
//! * pin a level where the pin is a **real promise** (a Cast device's decoder limit — what `42`
//!   was chosen for, and it only means anything up to 1080p);
//! * name the next one up for pictures that fit it;
//! * **omit the flag entirely when the size cannot be known** (`height == 0` is the 原画 preset:
//!   "do not scale", so the picture is whatever the display is — 3456×2234 here, 5120×2880 on a
//!   Studio Display). Omitting is not a shrug: VT computes a level matching the real picture at
//!   every size measured, and at 1080p it picks **4.0**, which is *more* conservative than the
//!   4.2 we were pinning. A table that stopped at 5.1 would just move this bug to the next
//!   display someone buys.
//!
//! ## The bitrate multiplier is one platform's quirk
//!
//! VideoToolbox produces **30–33% less** than `-b:v` asks for (4 Mbps requested → 2802 kbit
//! measured), so the target is scaled by 1.5 to make the wire rate equal the number the menu
//! promises. That is VT's, not hardware's: NVENC lands on the requested figure (menu 6000 →
//! 6052/6081/6087/6029 measured) and x264 does too, so scaling them would make them overshoot.

/// macOS's one real hardware tap.
pub const VT_ENCODER: &str = "h264_videotoolbox";
/// Windows' hardware encoder.
pub const NVENC: &str = "h264_nvenc";
/// The software fallback, on every platform.
pub const X264: &str = "libx264";

/// Which hardware encoder each platform has, if any.
///
/// **Linux is deliberately absent**: there is no tap that a probe can trust there, so `hardware`
/// falls through to x264 exactly as it did before this table existed.
pub fn hardware_encoder(platform: &str) -> Option<&'static str> {
    match platform {
        "darwin" => Some(VT_ENCODER),
        "win32" => Some(NVENC),
        _ => None,
    }
}

/// Which encoder to write into the argv.
///
/// `auto` means "hardware when the probe found it": the probe's answer is an **input**, because
/// spawning ffmpeg to ask is not a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Whatever the probe answered.
    Auto { hardware_available: bool },
    Hardware,
    Software,
}

/// The encoder name for a choice, or `None` when `auto` found no hardware and the platform has
/// none to fall back to.
pub fn encoder_for(choice: Choice, platform: &str) -> &'static str {
    match choice {
        Choice::Software => X264,
        Choice::Hardware => hardware_encoder(platform).unwrap_or(X264),
        Choice::Auto {
            hardware_available: true,
        } => hardware_encoder(platform).unwrap_or(X264),
        // `auto` with no probe hit is the software path — including on Linux, where there is
        // nothing to probe for.
        Choice::Auto {
            hardware_available: false,
        } => X264,
    }
}

/// Pixel format: NVENC wants `nv12`, everything else `yuv420p`.
pub fn encoder_pix_fmt(encoder: &str) -> &'static str {
    if encoder == NVENC {
        "nv12"
    } else {
        "yuv420p"
    }
}

/// The multiplier applied to `-b:v` so the wire rate matches the promise.
pub const VT_BITRATE_MULT: f64 = 1.5;

/// The `-b:v` value that actually produces `bitrate` on the wire.
///
/// Only VideoToolbox is scaled. NVENC and x264 are asked for the menu's number and measured to
/// land on it, so scaling them would overshoot by half.
pub fn rate_target(bitrate: u64, encoder: &str) -> u64 {
    if encoder == VT_ENCODER {
        (bitrate as f64 * VT_BITRATE_MULT) as u64
    } else {
        bitrate
    }
}

/// `(max height, level)` pairs for VideoToolbox.
///
/// Level 4.2 is the Cast device's decoder limit (what `42` was chosen for) and stops being
/// meaningful above 1080p; 5.1 covers 4K.
pub const VT_LEVELS: &[(u32, &str)] = &[(1080, "42"), (2160, "51")];

/// The `-level` for a picture of `height` lines, or `None` for "let VideoToolbox work it out".
///
/// `None` for `height == 0` too, which is the 原画 preset: "do not scale" means the picture is
/// whatever the display is, and pinning a level for a size we do not know is the bug this
/// function exists to prevent.
pub fn vt_level(height: u32) -> Option<&'static str> {
    if height == 0 {
        return None;
    }
    for (limit, level) in VT_LEVELS {
        if height <= *limit {
            return Some(level);
        }
    }
    None
}

/// The video encoder flags for a shape.
///
/// `height` is the picture's height, or `0` for "do not scale".
pub fn encoder_args(encoder: &str, height: u32, output_is_caststream: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    match encoder {
        X264 => {
            // `-tune zerolatency` already implies sliced threads; naming `-thread_type slice`
            // is cheap and explicit.
            args.extend(
                [
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-thread_type",
                    "slice",
                    "-profile:v",
                    "baseline",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        }
        VT_ENCODER => {
            // VideoToolbox reports *no* threading capability at all, so `-thread_type` is an
            // option it does not have and asking for it is an error, not a no-op.
            args.extend(
                ["-c:v", VT_ENCODER, "-realtime", "1", "-profile:v"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            args.push("baseline".to_string());
            if let Some(level) = vt_level(height) {
                args.push("-level".to_string());
                args.push(level.to_string());
            }
        }
        NVENC => {
            // `-preset p1 -tune ll -rc vbr`. No `-level` (NVENC picks its own), and no
            // `-realtime` — that is a VT AVOption, and writing it here is the "option this
            // encoder does not have" failure the other two branches avoid.
            args.extend(
                ["-c:v", NVENC, "-preset", "p1", "-tune", "ll", "-rc", "vbr", "-profile:v"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            args.push("baseline".to_string());
        }
        _ => {}
    }
    // Both low-latency branches carry `-flags +low_delay`: it is the first flag Google's own
    // reference sender sets, and upstream `videotoolboxenc.c` keeps adding support for it.
    args.push("-flags".to_string());
    args.push("+low_delay".to_string());
    // `caststream` pins the picture itself (the OFFER has already promised 1920x1080), so there
    // the level is a **promise** rather than a guess — which is why 42 is correct there and
    // wrong everywhere else.
    if output_is_caststream && encoder == VT_ENCODER {
        args.extend(["-level".to_string(), "42".to_string()]);
    }
    args
}

/// The trade-off the page must state, because the default is not obviously right.
///
/// The switch reads as "hardware = good", and that is not what the measurements say: hardware
/// costs about **200 ms** of first-frame latency and saves about **0.18 of a core**
/// (0.41 against 0.59 on a real 24 fps desktop at 1080p). Whether that is a good trade is the
/// user's, not ours — so the sentence travels with the switch.
pub const ENCODER_TRADEOFF: &str = "关掉硬件编码大约省下 200 毫秒的首帧延迟（实测同一台机器、\
同一份 24 fps 桌面采集：硬件首帧约 200 毫秒换 0.41 核，软件是 0.59 核），代价是 CPU";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_is_omitted_when_the_picture_size_is_unknown() {
        crate::pin_test_dir();
        // The 原画 preset is "do not scale", so the picture is whatever the display is. Pinning a
        // level for a size we do not know is exactly the shipped bug: exit 187 and zero bytes.
        assert_eq!(vt_level(0), None);
        // …and VT's own choice at 1080p is 4.0, which is *more* conservative than the 4.2 we
        // were pinning, so omitting is safe rather than a shrug.
    }

    #[test]
    fn the_level_table_stops_meaning_something_where_it_stops_covering() {
        crate::pin_test_dir();
        // 4.2 is the Cast device's limit and only means anything up to 1080p.
        assert_eq!(vt_level(1080), Some("42"));
        assert_eq!(vt_level(720), Some("42"));
        assert_eq!(vt_level(1081), Some("51"));
        assert_eq!(vt_level(2160), Some("51"));
        // Above the table: omit rather than guess. A table that stopped at 5.1 would move this
        // bug to the next display someone buys (5120x2880 is a real Mac).
        assert_eq!(vt_level(2234), None);
        assert_eq!(vt_level(2880), None);
        // The machine this bug was found on: 3456x2234 died at level 42 and worked without it.
        assert_eq!(vt_level(2234), None);
    }

    #[test]
    fn only_videotoolbox_gets_its_bitrate_scaled() {
        crate::pin_test_dir();
        // VT produces 30–33% less than asked (4 Mbps → 2802 kbit), so 1.5 makes the wire rate
        // match the menu. NVENC and x264 land on the number and must not be scaled.
        assert_eq!(rate_target(4_000_000, VT_ENCODER), 6_000_000);
        assert_eq!(rate_target(4_000_000, NVENC), 4_000_000);
        assert_eq!(rate_target(4_000_000, X264), 4_000_000);
        // Scaling NVENC would overshoot by half; the measurement (menu 6000 → 6052/6081/6087/6029)
        // is what says so.
        assert_eq!(rate_target(6_000_000, NVENC), 6_000_000);
    }

    #[test]
    fn the_platform_table_has_no_linux_entry_because_there_is_nothing_trustworthy_to_probe() {
        crate::pin_test_dir();
        assert_eq!(hardware_encoder("darwin"), Some(VT_ENCODER));
        assert_eq!(hardware_encoder("win32"), Some(NVENC));
        assert_eq!(hardware_encoder("linux"), None);
        // So `auto` on Linux is x264, and asking for `hardware` there also lands on x264 rather
        // than writing an encoder name the platform does not have.
        assert_eq!(
            encoder_for(Choice::Auto { hardware_available: true }, "linux"),
            X264
        );
        assert_eq!(encoder_for(Choice::Hardware, "linux"), X264);
    }

    #[test]
    fn the_pixel_format_follows_the_encoder_not_the_platform() {
        crate::pin_test_dir();
        assert_eq!(encoder_pix_fmt(NVENC), "nv12");
        assert_eq!(encoder_pix_fmt(VT_ENCODER), "yuv420p");
        assert_eq!(encoder_pix_fmt(X264), "yuv420p");
    }

    #[test]
    fn the_x264_branch_gets_zerolatency_and_sliced_threads() {
        crate::pin_test_dir();
        let args = encoder_args(X264, 1080, false);
        let joined = args.join(" ");
        assert!(joined.contains("-tune zerolatency"));
        assert!(joined.contains("-thread_type slice"));
        assert!(joined.contains("-preset ultrafast"));
        // No `-level`: level pinning is VT's problem, and x264 has no such promise to keep.
        assert!(!joined.contains("-level"), "{joined}");
    }

    #[test]
    fn the_videotoolbox_branch_never_gets_a_thread_type_and_carries_a_level_only_when_known() {
        crate::pin_test_dir();
        // `videotoolboxenc` reports no threading capability, so `-thread_type` is an option it
        // does not have — an error, not a no-op.
        let known = encoder_args(VT_ENCODER, 1080, false).join(" ");
        assert!(!known.contains("-thread_type"), "{known}");
        assert!(known.contains("-level 42"), "{known}");

        let unknown = encoder_args(VT_ENCODER, 0, false).join(" ");
        assert!(!unknown.contains("-level"), "an unknown size must not be pinned: {unknown}");
    }

    #[test]
    fn the_caststream_shape_pins_level_42_because_there_the_offer_already_promised_it() {
        crate::pin_test_dir();
        // Its OFFER pins the picture at 1920x1080, so a level is a promise rather than a guess —
        // which is why 42 is right here and wrong everywhere else.
        let pinned = encoder_args(VT_ENCODER, 0, true).join(" ");
        assert!(pinned.contains("-level 42"), "{pinned}");
        // …and it is the only place that happens without a known height.
        let plain = encoder_args(VT_ENCODER, 0, false).join(" ");
        assert!(!plain.contains("-level"));
    }

    #[test]
    fn the_nvenc_branch_has_neither_a_level_nor_a_videotoolbox_option() {
        crate::pin_test_dir();
        let args = encoder_args(NVENC, 2160, false).join(" ");
        // `-realtime` is a VT AVOption; writing it for NVENC is the "option this encoder does
        // not have" failure.
        assert!(!args.contains("-realtime"), "{args}");
        assert!(!args.contains("-level"), "{args}");
        assert!(args.contains("-preset p1"), "{args}");
        assert!(args.contains("-tune ll"), "{args}");
    }

    #[test]
    fn both_low_latency_branches_carry_low_delay() {
        crate::pin_test_dir();
        // Google's own reference sender sets it first; upstream videotoolboxenc.c keeps adding
        // support.
        for encoder in [X264, VT_ENCODER, NVENC] {
            let args = encoder_args(encoder, 1080, false).join(" ");
            assert!(args.contains("-flags +low_delay"), "{encoder}: {args}");
        }
    }

    /// Every level and bitrate the reference computed, replayed against this port.
    ///
    /// 36 rows from executing the reference's `vt_level` over twelve heights (including the
    /// 3456x2234 that exposed the bug) and `rate_target` over two kinds × three platforms × four
    /// bitrates. The 3456x2234 row is the one that matters: it answers `None`, which is what
    /// makes the picture work at all.
    #[test]
    fn every_reference_encoder_answer_replays_identically() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/mirror_encoder.json"))
                .expect("corpus is JSON");
        let rows = corpus.as_array().expect("rows");
        let mut levels = 0usize;
        let mut rates = 0usize;
        for row in rows {
            match row["kind"].as_str().expect("kind") {
                "vt_level" => {
                    let height = row["height"].as_u64().expect("height") as u32;
                    assert_eq!(
                        vt_level(height),
                        row["out"].as_str(),
                        "vt_level({height}) differs"
                    );
                    levels += 1;
                }
                "rate_target" => {
                    let encoder_kind = row["encoder"].as_str().expect("encoder");
                    let platform = row["platform"].as_str().expect("platform");
                    let bitrate = row["bitrate"].as_u64().expect("bitrate");
                    // The port's `rate_target` takes an encoder *name*, so resolve the reference's
                    // kind+platform the same way `encoder_for` does.
                    let name = encoder_for(
                        if encoder_kind == "hardware" {
                            Choice::Hardware
                        } else {
                            Choice::Software
                        },
                        platform,
                    );
                    assert_eq!(
                        rate_target(bitrate, name),
                        row["out"].as_u64().expect("out"),
                        "rate_target({bitrate}, {encoder_kind} on {platform}) differs"
                    );
                    rates += 1;
                }
                other => panic!("unknown corpus kind {other}"),
            }
        }
        assert_eq!(levels, 12, "the corpus lost level rows");
        assert_eq!(rates, 24, "the corpus lost rate rows");
    }

    #[test]
    fn the_tradeoff_sentence_carries_both_numbers() {
        crate::pin_test_dir();
        // "Hardware = good" is what the switch looks like; the measurement says 200 ms for
        // 0.18 of a core. Both halves must be in the sentence the page shows.
        assert!(ENCODER_TRADEOFF.contains("200"), "{ENCODER_TRADEOFF}");
        assert!(ENCODER_TRADEOFF.contains("0.41"));
        assert!(ENCODER_TRADEOFF.contains("0.59"));
    }
}
