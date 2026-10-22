//! Local-file casting: whether a file can go to a device **as-is**, and if not, why.
//!
//! Rust counterpart of `macast/plugins/renderer/cast_local_file.py`'s decision layer — the
//! tables and `plan()` at `:151-156` and `:423-462`. The serving half (Range/206, the growing
//! transcode file, the watchdog) already lives in [`crate::relay`], because a relay and a local
//! file are the same shape on the wire: a byte range out of a file that may still be growing.
//!
//! ## Why this is a decision table and not "just try it"
//!
//! The copy path hands the device **the original bytes**. That means it cannot reorder a track
//! list, cannot burn in a subtitle, cannot shift audio against video — and it must be certain
//! the device decodes the container and both codecs, because there is no fallback once the
//! bytes are out. So the rule is inverted from the usual one:
//!
//! > everything we cannot **prove** the device will decode converts.
//!
//! The reason string is part of the feature: it reaches the picker verbatim, and it is how a
//! user learns *why* a file is about to be re-encoded instead of streamed untouched.
//!
//! ## Two targets, three tables
//!
//! Chromecast and a DLNA television do not accept the same things, and the tables say so per
//! target rather than by flags on a shared list. Two asymmetries are deliberate and come from
//! the reference:
//!
//! * **audio-only files get their own list** (`COPY_AUDIO_ONLY`). A device's music path is far
//!   more tolerant than its video path — a Chromecast plays `flac` and `vorbis` as audio and
//!   refuses them inside a video container.
//! * **DLNA does not take `opus` at all**, in either list.
//!
//! ## The 1.9 GB ceiling
//!
//! A transcode's advertised length is what the device draws on its progress bar, so it has to
//! be the length the encoder will actually produce. When that would cross 1.9 GB the **bitrate
//! comes down instead of the length lying** — old firmwares store the length in a signed 32-bit
//! value (AGENTS.md §4.8), and a length of `-1` is a file that never plays.

/// What the device is: the two targets this port serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Cast,
    Dlna,
}

impl Target {
    /// Read a target from its wire name. `None` for anything else, so a caller cannot invent a
    /// third target by spelling it differently.
    pub fn parse(name: &str) -> Option<Target> {
        match name {
            "cast" => Some(Target::Cast),
            "dlna" => Some(Target::Dlna),
            _ => None,
        }
    }

    fn copy_containers(self) -> &'static [&'static str] {
        match self {
            Target::Cast => &["mp4", "mov", "m4v", "matroska", "webm"],
            Target::Dlna => &["mp4", "mov", "m4v"],
        }
    }

    fn copy_audio_codecs(self) -> &'static [&'static str] {
        match self {
            Target::Cast => &["aac", "mp3", "flac", "vorbis", "opus"],
            Target::Dlna => &["aac", "mp3"],
        }
    }

    fn copy_audio_only(self) -> &'static [&'static str] {
        match self {
            Target::Cast => &["mp3", "aac", "flac", "wav", "vorbis", "opus"],
            Target::Dlna => &["mp3", "aac", "flac", "wav"],
        }
    }
}

/// The only video codec both devices are trusted with. Everything else converts.
pub const COPY_VIDEO_CODECS: &[&str] = &["h264"];

/// A transcode's advertised length may not cross this, and the bitrate gives way first.
pub const MAX_ADVERTISED_SIZE: u64 = 1_900_000_000;

/// Budget when the file has no duration to divide by.
pub const FALLBACK_DURATION: f64 = 3600.0;

/// What `ffprobe` said about the file, as far as the decision needs it.
#[derive(Clone, Debug, Default)]
pub struct Media {
    /// The probe succeeded. `false` means "unknown", which converts.
    pub ok: bool,
    pub container: String,
    /// Audio-only files take a different path through the tables.
    pub audio_only: bool,
    /// Codec of the first video stream; empty when there is none.
    pub video_codec: String,
    pub has_video: bool,
    /// Codec of every audio stream, in the file's order.
    pub audio_codecs: Vec<String>,
    pub duration: f64,
}

/// What the user asked for, which can override the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    ForceCopy,
    ForceConvert,
}

impl Mode {
    pub fn parse(name: &str) -> Mode {
        match name {
            "direct" => Mode::ForceCopy,
            "convert" => Mode::ForceConvert,
            _ => Mode::Auto,
        }
    }
}

/// The other inputs to the decision: what the user picked, and what this ffmpeg can do.
#[derive(Clone, Debug, Default)]
pub struct Preferences {
    pub mode: Option<Mode>,
    /// A chosen audio track; more than one track means the copy path cannot express it.
    pub audio_index: Option<usize>,
    pub subtitle_index: Option<usize>,
    /// Audio/video offset in milliseconds. Non-zero needs a re-mux.
    pub delay_ms: i64,
    /// This ffmpeg has `libass`, so subtitles can be burned in on the convert path.
    pub libass: bool,
}

/// The verdict: which path, and the sentence the picker shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub copy: bool,
    /// Reaches the user verbatim, so it is built from the same words the reference used.
    pub reason: String,
}

impl Planned {
    fn copy(reason: impl Into<String>) -> Planned {
        Planned {
            copy: true,
            reason: reason.into(),
        }
    }

    fn convert(reason: impl Into<String>) -> Planned {
        Planned {
            copy: false,
            reason: reason.into(),
        }
    }
}

/// Decide between streaming the original bytes and re-encoding.
///
/// The order of the checks is the reference's, and it is load-bearing in two places: a forced
/// mode wins over the tables (that is what "forced" means), and an unreadable file converts
/// *before* any table is consulted — otherwise a probe failure would be read as "the container
/// is fine because we cannot see one".
pub fn plan(media: &Media, target: Target, prefs: &Preferences) -> Planned {
    match prefs.mode {
        Some(Mode::ForceConvert) => return Planned::convert("手动选择强制转码"),
        Some(Mode::ForceCopy) => return Planned::copy("手动选择强制直通"),
        _ => {}
    }
    if !media.ok {
        return Planned::convert("读不出文件信息，按转码处理");
    }

    let mut reasons: Vec<String> = Vec::new();

    if media.audio_only {
        let codec = media.audio_codecs.first().map(String::as_str).unwrap_or("");
        if !target.copy_audio_only().contains(&codec) {
            reasons.push(format!("音乐编码 {codec}"));
        }
    } else {
        if !media.has_video || media.video_codec.is_empty() {
            // Not "the container is wrong" but "there is nothing to show": the device would get
            // an audio track it did not ask for.
            return Planned::convert("文件里没有视频轨");
        }
        if !target.copy_containers().contains(&media.container.as_str()) {
            reasons.push(format!("容器 {}", media.container));
        }
        if !COPY_VIDEO_CODECS.contains(&media.video_codec.as_str()) {
            reasons.push(format!("画面编码 {}", media.video_codec));
        }
        // Any track outside the allowed set converts the whole file: the copy path hands over
        // every stream as it is, so one bad track is one undecodable file.
        let allowed = target.copy_audio_codecs();
        let offending: Vec<&str> = media
            .audio_codecs
            .iter()
            .map(String::as_str)
            .filter(|codec| !allowed.contains(codec))
            .collect();
        if !offending.is_empty() {
            let mut sorted: Vec<&str> = media.audio_codecs.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            reasons.push(format!("声音编码 {}", sorted.join("/")));
        }
    }

    // The three things the copy path structurally cannot do. Each is a *change* the user asked
    // for, not a decode risk, which is why they are checked after the tables.
    if prefs.audio_index.is_some() && media.audio_codecs.len() > 1 {
        reasons.push("选音轨要重新混流".to_string());
    }
    if prefs.subtitle_index.is_some() && target != Target::Cast && !prefs.libass {
        reasons.push("这个 ffmpeg 没有字幕烧录能力".to_string());
    }
    if prefs.delay_ms != 0 {
        reasons.push("音画同步偏移要重新混流".to_string());
    }

    if reasons.is_empty() {
        Planned::copy("电视能直接解码")
    } else {
        Planned::convert(format!("{}，电视吃不下", reasons.join("、")))
    }
}

/// The bitrate and advertised length for a transcode.
///
/// Returns `(bitrate, advertised_bytes)`. The length is what the device draws on its progress
/// bar, so when the wanted bitrate would cross [`MAX_ADVERTISED_SIZE`] the **bitrate drops**
/// rather than the length being reported wrong — a length the encoder does not produce makes a
/// device seek into bytes that do not exist, and old firmware cannot even hold the number
/// (AGENTS.md §4.8's signed-32-bit row).
pub fn bitrate_for_convert(media: &Media, wanted: u64) -> (u64, u64) {
    const DEFAULT_BITRATE: u64 = 6_000_000;
    const DEFAULT_AUDIO_BITRATE: u64 = 192_000;
    const FLOOR_BITRATE: u64 = 600_000;

    let duration = if media.duration > 0.0 {
        media.duration
    } else {
        FALLBACK_DURATION
    };
    let mut bitrate = if wanted == 0 {
        DEFAULT_BITRATE
    } else {
        wanted
    }
    .max(FLOOR_BITRATE);

    // `limit * 8 / duration * 0.9 - audio`: the ceiling on video bitrate that keeps the
    // advertised length under the cap, with 10% headroom because container overhead is not free.
    let ceiling = ((MAX_ADVERTISED_SIZE as f64 * 8.0 / duration) * 0.9) as u64;
    let ceiling = ceiling.saturating_sub(DEFAULT_AUDIO_BITRATE);
    if ceiling < bitrate {
        bitrate = ceiling.max(FLOOR_BITRATE);
    }

    let advertised = ((bitrate + DEFAULT_AUDIO_BITRATE) as f64 * duration / 8.0) as u64;
    (bitrate, advertised)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h264_mp4() -> Media {
        Media {
            ok: true,
            container: "mp4".into(),
            audio_only: false,
            video_codec: "h264".into(),
            has_video: true,
            audio_codecs: vec!["aac".into()],
            duration: 60.0,
        }
    }

    fn prefs() -> Preferences {
        Preferences::default()
    }

    #[test]
    fn a_file_both_devices_are_trusted_with_streams_untouched() {
        crate::pin_test_dir();
        // The whole point of the table: h264/mp4/aac is the one shape that costs no CPU.
        for target in [Target::Cast, Target::Dlna] {
            let verdict = plan(&h264_mp4(), target, &prefs());
            assert!(verdict.copy, "{target:?} converted a file it should copy");
            assert_eq!(verdict.reason, "电视能直接解码");
        }
    }

    #[test]
    fn everything_we_cannot_prove_converts_and_says_which_part() {
        crate::pin_test_dir();
        // Each row is one table entry, and the reason names the field rather than the verdict —
        // a user who reads "转码" alone cannot tell whether to remux or re-encode.
        let cases: Vec<(Media, Target, &str)> = vec![
            (
                Media {
                    container: "avi".into(),
                    ..h264_mp4()
                },
                Target::Dlna,
                "容器 avi",
            ),
            (
                Media {
                    video_codec: "hevc".into(),
                    ..h264_mp4()
                },
                Target::Cast,
                "画面编码 hevc",
            ),
            (
                Media {
                    audio_codecs: vec!["ac3".into()],
                    ..h264_mp4()
                },
                Target::Dlna,
                "声音编码 ac3",
            ),
        ];
        for (media, target, expected) in cases {
            let verdict = plan(&media, target, &prefs());
            assert!(!verdict.copy, "{expected} was copied: {verdict:?}");
            assert!(
                verdict.reason.contains(expected),
                "the reason must name the field that failed: expected {expected:?} inside {:?}",
                verdict.reason
            );
            assert!(
                verdict.reason.ends_with("，电视吃不下"),
                "the reference's sentence ends with this clause: {:?}",
                verdict.reason
            );
        }
    }

    #[test]
    fn an_unreadable_file_converts_before_any_table_is_consulted() {
        crate::pin_test_dir();
        // `ok: false` means the probe failed, so the tables have nothing to say — reading the
        // empty container as "not in the list" would still convert, but for a misleading reason.
        let media = Media {
            ok: false,
            ..h264_mp4()
        };
        let verdict = plan(&media, Target::Cast, &prefs());
        assert!(!verdict.copy);
        assert_eq!(verdict.reason, "读不出文件信息，按转码处理");
    }

    #[test]
    fn a_forced_mode_wins_over_the_tables() {
        crate::pin_test_dir();
        let bad = Media {
            container: "avi".into(),
            video_codec: "hevc".into(),
            ..h264_mp4()
        };
        let forced_copy = Preferences {
            mode: Some(Mode::ForceCopy),
            ..prefs()
        };
        assert!(plan(&bad, Target::Dlna, &forced_copy).copy);

        let forced_convert = Preferences {
            mode: Some(Mode::ForceConvert),
            ..prefs()
        };
        assert!(!plan(&h264_mp4(), Target::Cast, &forced_convert).copy);
        assert_eq!(
            plan(&h264_mp4(), Target::Cast, &forced_convert).reason,
            "手动选择强制转码"
        );
    }

    #[test]
    fn a_file_with_no_video_track_says_so_instead_of_blaming_the_container() {
        crate::pin_test_dir();
        let media = Media {
            has_video: false,
            video_codec: String::new(),
            ..h264_mp4()
        };
        let verdict = plan(&media, Target::Cast, &prefs());
        assert!(!verdict.copy);
        assert_eq!(verdict.reason, "文件里没有视频轨");
    }

    #[test]
    fn an_audio_only_file_uses_the_music_table_not_the_video_one() {
        crate::pin_test_dir();
        // The asymmetry that matters: a Chromecast plays flac as *music* and refuses it inside
        // a video container, so one table for both would convert a file the device handles.
        let flac_audio = Media {
            ok: true,
            container: "flac".into(),
            audio_only: true,
            has_video: false,
            video_codec: String::new(),
            audio_codecs: vec!["flac".into()],
            duration: 300.0,
        };
        assert!(plan(&flac_audio, Target::Cast, &prefs()).copy);
        // …and DLNA takes flac as music too, but would not take it as a video track.
        assert!(plan(&flac_audio, Target::Dlna, &prefs()).copy);

        let opus_audio = Media {
            audio_codecs: vec!["opus".into()],
            ..flac_audio
        };
        // Chromecast's music list has opus; DLNA's does not. This is the pair that proves the
        // two audio tables are actually consulted separately.
        assert!(plan(&opus_audio, Target::Cast, &prefs()).copy);
        let verdict = plan(&opus_audio, Target::Dlna, &prefs());
        assert!(!verdict.copy);
        assert!(verdict.reason.contains("音乐编码 opus"), "{:?}", verdict.reason);
    }

    #[test]
    fn three_changes_the_copy_path_cannot_express_all_convert() {
        crate::pin_test_dir();
        let two_tracks = Media {
            audio_codecs: vec!["aac".into(), "aac".into()],
            ..h264_mp4()
        };
        let picking_a_track = Preferences {
            audio_index: Some(1),
            ..prefs()
        };
        assert!(!plan(&two_tracks, Target::Cast, &picking_a_track).copy);

        // …but picking the *only* track changes nothing, so it must not force a re-encode.
        let one_track = Preferences {
            audio_index: Some(0),
            ..prefs()
        };
        assert!(plan(&h264_mp4(), Target::Cast, &one_track).copy);

        let subtitle_dlna = Preferences {
            subtitle_index: Some(0),
            libass: false,
            ..prefs()
        };
        let verdict = plan(&h264_mp4(), Target::Dlna, &subtitle_dlna);
        assert!(!verdict.copy);
        assert!(verdict.reason.contains("没有字幕烧录能力"), "{:?}", verdict.reason);
        // …while the same request on Cast converts WebVTT, so it needs no re-encode at all.
        assert!(plan(&h264_mp4(), Target::Cast, &subtitle_dlna).copy);

        let delayed = Preferences {
            delay_ms: -250,
            ..prefs()
        };
        assert!(!plan(&h264_mp4(), Target::Cast, &delayed).copy);
    }

    #[test]
    fn the_advertised_length_never_crosses_the_signed_32_bit_ceiling() {
        crate::pin_test_dir();
        // A long file at a high bitrate: the ceiling has to bend the bitrate, not the length.
        let long = Media {
            duration: 4.0 * 3600.0,
            ..h264_mp4()
        };
        let (bitrate, advertised) = bitrate_for_convert(&long, 20_000_000);
        assert!(
            advertised < MAX_ADVERTISED_SIZE,
            "advertised {advertised} crosses the 1.9 GB ceiling"
        );
        assert!(bitrate < 20_000_000, "the bitrate did not give way: {bitrate}");

        // A short file at a modest bitrate keeps what was asked for.
        let short = Media {
            duration: 300.0,
            ..h264_mp4()
        };
        let (bitrate, advertised) = bitrate_for_convert(&short, 4_000_000);
        assert_eq!(bitrate, 4_000_000);
        assert!(advertised < MAX_ADVERTISED_SIZE);
    }

    #[test]
    fn a_file_with_no_duration_is_budgeted_against_the_fallback() {
        crate::pin_test_dir();
        let unknown = Media {
            duration: 0.0,
            ..h264_mp4()
        };
        let (bitrate, advertised) = bitrate_for_convert(&unknown, 6_000_000);
        // 6 Mbps + 192 kbps over an hour is ~2.7 GB, so the ceiling must have bent the bitrate.
        assert!(bitrate < 6_000_000, "bitrate {bitrate} ignored the fallback duration");
        assert!(advertised < MAX_ADVERTISED_SIZE);
    }

    /// Every verdict the reference implementation gives, replayed against this port.
    ///
    /// The corpus was produced by executing `cast_local_file.plan()` itself over the cross
    /// product of containers × video codecs × audio codec sets × targets × modes, with
    /// `audio_only` as its own dimension (1,566 rows),
    /// so this is not "what I think the tables say" — it is what the shipped Python said.
    /// A row that disagrees is a behaviour change, and it has to be either a bug here or a
    /// deliberate divergence written down as one.
    #[test]
    fn every_reference_verdict_replays_identically() {
        crate::pin_test_dir();
        let corpus = include_str!("../tests/golden/local_file_plan.jsonl");
        let mut rows = 0usize;
        let mut copies = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row is JSON");
            let target = Target::parse(row["target"].as_str().expect("target"))
                .expect("corpus only names targets this port serves");
            let media = Media {
                ok: true,
                container: row["container"].as_str().unwrap_or("").to_string(),
                // `audio_only` is its own ffprobe fact, not something the port may infer from
                // the container: `flac`/`mp3`/`wav` as *containers* holding a video track are
                // real files, and guessing here made this port disagree with the reference on
                // the very first corpus run.
                audio_only: row["audio_only"].as_bool().unwrap_or(false),
                video_codec: row["video"].as_str().unwrap_or("").to_string(),
                has_video: !row["video"].as_str().unwrap_or("").is_empty(),
                audio_codecs: row["audio"]
                    .as_array()
                    .expect("audio list")
                    .iter()
                    .map(|c| c.as_str().unwrap_or("").to_string())
                    .collect(),
                duration: 60.0,
            };
            let prefs = Preferences {
                mode: Some(Mode::parse(row["mode"].as_str().unwrap_or("auto"))),
                ..Preferences::default()
            };
            let verdict = plan(&media, target, &prefs);
            let expected_copy = row["copy"].as_bool().expect("copy flag");
            let expected_reason = row["reason"].as_str().expect("reason");
            assert_eq!(
                (verdict.copy, verdict.reason.as_str()),
                (expected_copy, expected_reason),
                "row {line} disagrees with the reference"
            );
            rows += 1;
            if verdict.copy {
                copies += 1;
            }
        }
        // A corpus that failed to load would otherwise pass with zero assertions, and a corpus
        // where everything converts would not exercise the copy path at all.
        assert_eq!(rows, 1566, "the corpus lost rows");
        assert_eq!(copies, 611, "the corpus lost copy-path rows: {copies}");
    }

    #[test]
    fn target_names_are_a_closed_set() {
        crate::pin_test_dir();
        assert_eq!(Target::parse("cast"), Some(Target::Cast));
        assert_eq!(Target::parse("dlna"), Some(Target::Dlna));
        assert_eq!(Target::parse("chromecast"), None);
        assert_eq!(Target::parse(""), None);
        assert_eq!(Mode::parse("direct"), Mode::ForceCopy);
        assert_eq!(Mode::parse("convert"), Mode::ForceConvert);
        assert_eq!(Mode::parse("whatever"), Mode::Auto);
    }
}
