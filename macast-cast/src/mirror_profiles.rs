//! Screen mirroring: the output targets and the DLNA compatibility profiles.
//!
//! Rust counterpart of `screen_mirror.py`'s three tables — `OUTPUTS` (`:427-449`),
//! `DLNA_PROFILES` (`:544-573`) and `DLNA_PROFILE_LATENCY` (`:1437-1443`) — plus the default
//! selection (`:1286-1310`).
//!
//! The plugin is 13,629 lines and most of it is a pipeline: capture, encode, broadcast, WebRTC
//! signalling. What is portable **as a decision** is the part a reader can get wrong in a way
//! nothing reports: which container each target gets, why two of the five muxer slots are `None`,
//! and which compatibility shape an untouched install should get.
//!
//! ## The three things these tables encode that are easy to lose
//!
//! 1. **Two targets are not containers.** `webrtc` and `caststream` hand raw H.264 NAL units to
//!    something that repacketises them (an RTP sender, a UDP socket). They therefore have **no
//!    muxer, no suffix and no Content-Type** — nothing is served over HTTP. A port that gave them
//!    `-f mpegts` would produce a stream the peer cannot parse, and the only symptom would be a
//!    black picture.
//! 2. **`dlna` is not a container either**, for the opposite reason: a television is told it is
//!    downloading a *finite file*, so container, codec **and the extension in the URL** all come
//!    from the compatibility profile. Its muxer slot is `None` because the profile supplies it.
//! 3. **The default profile depends on the shape**, and that is a measurement, not a taste:
//!    `ps-pal` costs ~3.4 s of start-up against `ts-mpeg2` for a byte-equivalent payload, so the
//!    live shape gets `ts-h264` while the file shape keeps the historical `ps-pal`.
//!
//! ## What the latency table is, and is not
//!
//! It is **this machine against another Macast on this LAN**, not a television. The ordering
//! (TS before PS by about 3 s) is a property of reading a container that identifies itself
//! weakly, which every receiver does; the sizes are one receiver's numbers. [`LATENCY_SCOPE`]
//! exists so that qualifier travels with the numbers instead of being dropped by whoever quotes
//! them.

/// One output target: the label a user picks, and how the encoder is told to emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    /// The target's id, as the settings file and the page use it.
    pub id: &'static str,
    pub label: &'static str,
    /// File suffix for the served URL. Empty for a target that serves nothing.
    pub suffix: &'static str,
    /// `Content-Type`. `None` when nothing is served over HTTP.
    pub content_type: Option<&'static str>,
    /// The muxer arguments. `None` when the container comes from elsewhere (DLNA profiles).
    pub muxer: Option<&'static [&'static str]>,
}

/// The muxer argument list for a live TS pipe.
///
/// `-muxdelay 0 -muxpreload 0` give back the TS muxer's default interleave delay, which a live
/// pipe cannot afford. The MPEG-PS profiles deliberately do **not** carry it (AGENTS.md §4.8: PS
/// needs that interleave margin, and removing it causes underruns and artefacts).
const TS_LIVE: &[&str] = &[
    "-f",
    "mpegts",
    "-muxdelay",
    "0",
    "-muxpreload",
    "0",
    "pipe:1",
];

/// Fragmented MP4, one fragment per frame.
///
/// `frag_every_frame` is what made browser lag drop 1305 → 838 ms at the same `-g 12`
/// (AGENTS.md §4.8, the v0.19 measurement). `empty_moov` is the other half: it puts the track
/// table at the **head** so a growing file is playable before it is finished.
const FMP4_LIVE: &[&str] = &[
    "-f",
    "mp4",
    "-movflags",
    "frag_every_frame+empty_moov+default_base_moof",
    "pipe:1",
];

/// Raw Annex-B H.264: the two targets that repacketise rather than demux.
const H264_RAW: &[&str] = &["-f", "h264", "pipe:1"];

/// The five targets, in the order the page lists them.
pub const OUTPUTS: &[Output] = &[
    Output {
        id: "cast",
        label: "Chromecast / Google TV",
        suffix: "ts",
        content_type: Some("video/mp2t"),
        muxer: Some(TS_LIVE),
    },
    Output {
        id: "browser",
        label: "浏览器（打开网址即可看）",
        suffix: "m4s",
        content_type: Some("video/mp4"),
        muxer: Some(FMP4_LIVE),
    },
    Output {
        // No muxer, no suffix, no Content-Type: these bytes go to a WebRTC peer, which
        // repacketises them into RTP without decoding. Nothing is served over HTTP.
        id: "webrtc",
        label: "浏览器 · WebRTC（低延迟 · 此通道无声音）",
        // The table carries `h264` here even though nothing is served: the field is a stream
        // *kind*, and the corpus caught this port reading the comment ("no suffix, no
        // Content-Type") as "empty suffix". `content_type: None` is what actually says the bytes
        // never travel over HTTP.
        suffix: "h264",
        content_type: None,
        muxer: Some(H264_RAW),
    },
    Output {
        // Not a different device — the same Chromecast, driven by its mirroring app. The bytes
        // are pushed to a UDP port, so again nothing is served.
        id: "caststream",
        label: "Chromecast 低延迟（实验 · 此通道无声音）",
        suffix: "h264",
        content_type: None,
        muxer: Some(H264_RAW),
    },
    Output {
        // A television is told it is downloading a finite file, so container, codec and even the
        // extension in the URL come from the compatibility profile. `None` is the whole point.
        id: "dlna",
        label: "DLNA 电视（老电视，无需在电视上装东西）",
        suffix: "mpg",
        content_type: Some("video/mpeg"),
        muxer: None,
    },
];

/// What an install that never chose one gets.
pub const DEFAULT_OUTPUT: &str = "cast";

/// The viewing page's path, and the two endpoints that are deliberately **not** under the stream
/// prefix: they are not media, so counting them as stream exchanges would bury the requests the
/// log exists to record.
pub const BROWSER_PATH: &str = "/browser";
pub const BROWSER_STATS_PATH: &str = "/browser/stats";

/// Look a target up by id.
pub fn output(id: &str) -> Option<&'static Output> {
    OUTPUTS.iter().find(|output| output.id == id)
}

/// One DLNA compatibility shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlnaProfile {
    pub id: &'static str,
    pub label: &'static str,
    /// Muxer arguments; appended to the encoder's output list.
    pub muxer: &'static [&'static str],
    /// The extension in the served URL.
    pub suffix: &'static str,
    pub content_type: &'static str,
    /// `DLNA.ORG_PN`, when the shape makes a claim a device can look up. `None` for H.264 shapes,
    /// where no profile name would be honest.
    pub profile_name: Option<&'static str>,
    /// Video codec arguments, or `None` for the H.264 path (the encoder is chosen elsewhere).
    pub video: Option<&'static [&'static str]>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u32,
    pub audio_bitrate: u32,
}

/// MPEG-2 at the given frame rate and bitrate.
const fn mpeg2_args(fps: u32, bitrate: u32) -> &'static [&'static str] {
    // Kept as one array per rate because a `const fn` cannot format; the two rates are the only
    // ones the profiles use.
    match (fps, bitrate) {
        (25, 4_500_000) => &["-c:v", "mpeg2video", "-b:v", "4500k", "-r", "25"],
        (30, 4_500_000) => &["-c:v", "mpeg2video", "-b:v", "4500k", "-r", "30"],
        (25, 5_000_000) => &["-c:v", "mpeg2video", "-b:v", "5000k", "-r", "25"],
        _ => &[],
    }
}

const PS_MUXER: &[&str] = &["-f", "vob"];
const TS_MUXER: &[&str] = &["-f", "mpegts", "-muxdelay", "0", "-muxpreload", "0"];
const MKV_MUXER: &[&str] = &["-f", "matroska", "-muxdelay", "0", "-muxpreload", "0"];

/// The five compatibility shapes.
///
/// `ps-pal`/`ps-ntsc` carry AC-3 (DVD-era televisions do not accept AAC), which is why the audio
/// encoder differs by shape rather than being one setting.
pub const DLNA_PROFILES: &[DlnaProfile] = &[
    DlnaProfile {
        id: "ps-pal",
        label: "MPEG-PS · PAL 576p（老电视首选）",
        muxer: PS_MUXER,
        suffix: "mpg",
        content_type: "video/mpeg",
        profile_name: Some("MPEG_PS_PAL"),
        video: Some(mpeg2_args(25, 4_500_000)),
        width: 720,
        height: 576,
        fps: 25,
        video_bitrate: 4_500_000,
        audio_bitrate: 192_000,
    },
    DlnaProfile {
        id: "ps-ntsc",
        label: "MPEG-PS · NTSC 480p（北美/日本老电视）",
        muxer: PS_MUXER,
        suffix: "mpg",
        content_type: "video/mpeg",
        profile_name: Some("MPEG_PS_NTSC"),
        video: Some(mpeg2_args(30, 4_500_000)),
        width: 720,
        height: 480,
        fps: 30,
        video_bitrate: 4_500_000,
        audio_bitrate: 192_000,
    },
    DlnaProfile {
        id: "ts-mpeg2",
        label: "MPEG-TS · MPEG-2 576p（认 TS 不认 PS 的电视）",
        muxer: TS_MUXER,
        suffix: "ts",
        content_type: "video/vnd.dlna.mpeg-tts",
        profile_name: Some("MPEG_TS_SD_EU"),
        video: Some(mpeg2_args(25, 5_000_000)),
        width: 720,
        height: 576,
        fps: 25,
        video_bitrate: 5_000_000,
        audio_bitrate: 192_000,
    },
    DlnaProfile {
        id: "ts-h264",
        label: "MPEG-TS · H.264 720p（较新的电视，清晰度更高）",
        muxer: TS_MUXER,
        suffix: "ts",
        content_type: "video/vnd.dlna.mpeg-tts",
        // No `PN`: claiming a profile name for H.264 here would be a claim no device looks up.
        profile_name: None,
        video: None,
        width: 0,
        height: 720,
        fps: 25,
        video_bitrate: 6_000_000,
        audio_bitrate: 128_000,
    },
    DlnaProfile {
        id: "mkv-h264",
        label: "Matroska · H.264 720p（只认 MKV 的电视/Kodi）",
        muxer: MKV_MUXER,
        suffix: "mkv",
        content_type: "video/x-matroska",
        profile_name: None,
        video: None,
        width: 0,
        height: 720,
        fps: 25,
        video_bitrate: 6_000_000,
        audio_bitrate: 128_000,
    },
];

/// The historical default, and the one the **file** shape keeps.
pub const DEFAULT_DLNA_PROFILE: &str = "ps-pal";

/// What the **live** shape gets instead.
pub const LIVE_DEFAULT_DLNA_PROFILE: &str = "ts-h264";

/// Which shape this is: the live one or the file one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlnaShape {
    /// Frames arrive as they are produced.
    Live,
    /// A finite file, served with a length.
    File,
}

/// The profile an install that never chose one gets.
///
/// The live/file split is a measurement, not a preference: on the same machine, receiver and
/// encoder, `ps-pal` started 5.86 s and settled at 5.08 s while `ts-mpeg2` started 2.47 s and
/// settled at 2.15 s — for a **byte-for-byte equivalent** payload (same codec, CBR, frame size
/// and rate). So ~3.4 s of that is the container (`ps` needs `-preload`, which stamps
/// `start_time 0.534667`, and a PS demuxer also asks for more probe data: `probe_score` 26
/// against TS's 100 on the same stream).
///
/// The file shape keeps `ps-pal` because that is the historical answer and the one DVD-era
/// firmware was tested with; there is no measurement saying otherwise for a finite file.
pub fn default_dlna_profile_id(shape: DlnaShape) -> &'static str {
    match shape {
        DlnaShape::Live => LIVE_DEFAULT_DLNA_PROFILE,
        DlnaShape::File => DEFAULT_DLNA_PROFILE,
    }
}

/// Look a profile up by id.
pub fn dlna_profile(id: &str) -> Option<&'static DlnaProfile> {
    DLNA_PROFILES.iter().find(|profile| profile.id == id)
}

/// Measured latency per profile: `(first picture, steady lag)` in seconds.
///
/// The numbers are **strings**, as in the reference: they reach a `title` attribute, and turning
/// them into floats here would invite arithmetic on a measurement.
pub const DLNA_PROFILE_LATENCY: &[(&str, &str, &str)] = &[
    ("ts-h264", "2.0", "1.9"),
    ("mkv-h264", "2.4", "2.1"),
    ("ts-mpeg2", "2.5", "2.2"),
    ("ps-pal", "5.7", "5.1"),
    ("ps-ntsc", "5.7", "5.0"),
];

/// The qualifier that must travel with [`DLNA_PROFILE_LATENCY`].
///
/// Without it, five numbers read as a claim about televisions. They are one Mac against another
/// Macast on this LAN.
pub const LATENCY_SCOPE: &str = "实测：同一台 Mac、直播形状、硬件编码，接收端是本机另一台 mpv/Macast（不是真老电视）";

/// The latency row for a profile.
pub fn latency_of(id: &str) -> Option<(&'static str, &'static str)> {
    DLNA_PROFILE_LATENCY
        .iter()
        .find(|(profile, _, _)| *profile == id)
        .map(|(_, first, steady)| (*first, *steady))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_repacketising_targets_serve_nothing_over_http() {
        crate::pin_test_dir();
        // A port that gave these `-f mpegts` would produce bytes the peer cannot parse, and the
        // only symptom would be a black picture. No suffix and no Content-Type is the assertion.
        for id in ["webrtc", "caststream"] {
            let target = output(id).expect("target exists");
            // `h264` is a **stream kind**, not a served file extension: `content_type: None` is
            // what says these bytes never travel over HTTP. The corpus caught this port reading
            // the reference's comment ("no suffix") as an empty field.
            assert_eq!(target.suffix, "h264", "{id}'s kind");
            assert_eq!(target.content_type, None, "{id} must not claim a Content-Type");
            // …but it *does* have a muxer: raw Annex-B H.264, which the peer repacketises.
            assert_eq!(target.muxer, Some(H264_RAW), "{id} must emit raw H.264");
            assert!(
                target.label.contains("无声音"),
                "{id}'s label must say it has no sound: {}",
                target.label
            );
        }
    }

    #[test]
    fn the_dlna_target_takes_its_container_from_the_profile() {
        crate::pin_test_dir();
        // `None` is the point: container, codec and the URL's extension all come from the chosen
        // compatibility shape. Filling this in would pin every television to one container.
        let dlna = output("dlna").expect("target exists");
        assert_eq!(dlna.muxer, None);
        assert_eq!(dlna.suffix, "mpg");
        assert_eq!(dlna.content_type, Some("video/mpeg"));
    }

    #[test]
    fn the_ts_and_mp4_shapes_carry_the_flags_they_were_measured_with() {
        crate::pin_test_dir();
        let cast = output("cast").expect("target exists");
        let muxer = cast.muxer.expect("cast has a muxer");
        assert!(muxer.contains(&"-muxdelay"));
        assert!(muxer.contains(&"0"));
        assert!(muxer.contains(&"-muxpreload"));
        assert_eq!(cast.content_type, Some("video/mp2t"));

        let browser = output("browser").expect("target exists");
        let fmp4 = browser.muxer.expect("browser has a muxer");
        let flags = fmp4
            .iter()
            .position(|arg| *arg == "-movflags")
            .map(|at| fmp4[at + 1])
            .expect("a -movflags value");
        // `frag_every_frame` is the 1305 -> 838 ms change; `empty_moov` is what makes a growing
        // file playable before it is finished.
        assert!(flags.contains("frag_every_frame"), "{flags}");
        assert!(flags.contains("empty_moov"), "{flags}");
        assert!(flags.contains("default_base_moof"), "{flags}");
    }

    #[test]
    fn the_ps_profiles_do_not_carry_the_ts_interleave_flags() {
        crate::pin_test_dir();
        // AGENTS.md §4.8: PS needs its interleave margin, and removing it causes underruns and
        // artefacts. The TS shapes give it back; the PS ones must not.
        for id in ["ps-pal", "ps-ntsc"] {
            let profile = dlna_profile(id).expect("profile exists");
            assert!(
                !profile.muxer.contains(&"-muxdelay"),
                "{id} must keep the PS interleave margin"
            );
            assert_eq!(profile.muxer, PS_MUXER);
        }
        for id in ["ts-mpeg2", "ts-h264", "mkv-h264"] {
            let profile = dlna_profile(id).expect("profile exists");
            assert!(profile.muxer.contains(&"-muxdelay"), "{id} should give it back");
        }
    }

    #[test]
    fn only_the_mpeg2_shapes_claim_a_dlna_profile_name() {
        crate::pin_test_dir();
        // A `PN` is a claim a device looks up. The H.264 shapes have none to make.
        for profile in DLNA_PROFILES {
            match profile.id {
                "ps-pal" | "ps-ntsc" | "ts-mpeg2" => {
                    assert!(profile.profile_name.is_some(), "{} needs a PN", profile.id)
                }
                _ => assert_eq!(
                    profile.profile_name, None,
                    "{} must not claim a PN",
                    profile.id
                ),
            }
        }
    }

    #[test]
    fn the_live_shape_defaults_to_ts_and_the_file_shape_keeps_ps() {
        crate::pin_test_dir();
        // ~3.4 s of start-up is the container, measured on a byte-equivalent payload. The file
        // shape has no such measurement against it, so it keeps the historical answer.
        assert_eq!(default_dlna_profile_id(DlnaShape::Live), "ts-h264");
        assert_eq!(default_dlna_profile_id(DlnaShape::File), "ps-pal");
        // …and both must exist in the table, or the default is a dangling id.
        assert!(dlna_profile(default_dlna_profile_id(DlnaShape::Live)).is_some());
        assert!(dlna_profile(default_dlna_profile_id(DlnaShape::File)).is_some());
    }

    #[test]
    fn the_latency_numbers_never_travel_without_their_scope() {
        crate::pin_test_dir();
        // Five numbers read as a claim about televisions; they are one Mac against another Mac.
        assert!(LATENCY_SCOPE.contains("不是真老电视"), "{LATENCY_SCOPE}");
        assert_eq!(DLNA_PROFILE_LATENCY.len(), DLNA_PROFILES.len());
        // Every profile has a row, and every row names a real profile.
        for profile in DLNA_PROFILES {
            assert!(latency_of(profile.id).is_some(), "{} has no latency row", profile.id);
        }
        for (id, _, _) in DLNA_PROFILE_LATENCY {
            assert!(dlna_profile(id).is_some(), "latency row for unknown profile {id}");
        }
        // The ordering is the whole message: TS starts about 3 s sooner than PS.
        let (ts_first, _) = latency_of("ts-mpeg2").expect("row");
        let (ps_first, _) = latency_of("ps-pal").expect("row");
        let ts: f64 = ts_first.parse().expect("a number");
        let ps: f64 = ps_first.parse().expect("a number");
        assert!(ps - ts > 2.5, "the PS penalty should be ~3 s, got {}", ps - ts);
    }

    #[test]
    fn the_page_endpoints_are_not_under_a_stream_prefix() {
        crate::pin_test_dir();
        // Counting a once-a-second stats poll as a stream exchange would bury the requests the
        // log exists to record (the reference says so in a comment at `:452`).
        assert_eq!(BROWSER_STATS_PATH, "/browser/stats");
        assert!(BROWSER_STATS_PATH.starts_with(BROWSER_PATH));
        assert!(!BROWSER_STATS_PATH.contains("stream"));
    }

    /// Every field of every table the reference defines, replayed against this port.
    ///
    /// Produced by importing the reference's `OUTPUTS`, `DLNA_PROFILES` and
    /// `DLNA_PROFILE_LATENCY` dicts and dumping them. Field-by-field rather than
    /// whole-struct, so a divergence names the field.
    ///
    /// This corpus changed the port once: `webrtc`/`caststream` carry `h264` in the suffix slot,
    /// and I had read the reference's comment ("No suffix and no Content-Type") as an empty
    /// field. `content_type: None` is the part that actually means "never served over HTTP".
    #[test]
    fn every_reference_table_field_replays_identically() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/mirror_profiles.json"))
                .expect("corpus is JSON");

        let outputs = corpus["outputs"].as_object().expect("outputs");
        assert_eq!(outputs.len(), OUTPUTS.len());
        for (id, expected) in outputs {
            let found = output(id).unwrap_or_else(|| panic!("target {id} is missing"));
            assert_eq!(found.label, expected["label"].as_str().expect("label"), "{id} label");
            assert_eq!(found.suffix, expected["suffix"].as_str().expect("suffix"), "{id} suffix");
            assert_eq!(
                found.content_type,
                expected["content_type"].as_str(),
                "{id} content_type"
            );
            let muxer: Option<Vec<String>> = expected["muxer"].as_array().map(|args| {
                args.iter().map(|a| a.as_str().unwrap_or("").to_string()).collect()
            });
            let mine: Option<Vec<String>> =
                found.muxer.map(|args| args.iter().map(|a| a.to_string()).collect());
            assert_eq!(mine, muxer, "{id} muxer");
        }

        let profiles = corpus["profiles"].as_object().expect("profiles");
        assert_eq!(profiles.len(), DLNA_PROFILES.len());
        for (id, expected) in profiles {
            let found = dlna_profile(id).unwrap_or_else(|| panic!("profile {id} is missing"));
            assert_eq!(found.label, expected["label"].as_str().expect("label"), "{id} label");
            assert_eq!(found.suffix, expected["suffix"].as_str().expect("suffix"), "{id} suffix");
            assert_eq!(
                found.content_type,
                expected["content_type"].as_str().expect("type"),
                "{id} content_type"
            );
            assert_eq!(
                found.profile_name,
                expected["pn"].as_str(),
                "{id} DLNA.ORG_PN"
            );
            assert_eq!(
                found.width as u64,
                expected["width"].as_u64().expect("width"),
                "{id} width"
            );
            assert_eq!(
                found.height as u64,
                expected["height"].as_u64().expect("height"),
                "{id} height"
            );
            assert_eq!(found.fps as u64, expected["fps"].as_u64().expect("fps"), "{id} fps");
            assert_eq!(
                found.video_bitrate as u64,
                expected["vbr"].as_u64().expect("vbr"),
                "{id} video bitrate"
            );
            assert_eq!(
                found.audio_bitrate as u64,
                expected["abr"].as_u64().expect("abr"),
                "{id} audio bitrate"
            );
            let muxer: Vec<String> = expected["muxer"]
                .as_array()
                .expect("muxer")
                .iter()
                .map(|a| a.as_str().unwrap_or("").to_string())
                .collect();
            let mine: Vec<String> = found.muxer.iter().map(|a| a.to_string()).collect();
            assert_eq!(mine, muxer, "{id} muxer");
        }

        let latency = corpus["latency"].as_object().expect("latency");
        assert_eq!(latency.len(), DLNA_PROFILE_LATENCY.len());
        for (id, pair) in latency {
            let expected: Vec<&str> = pair
                .as_array()
                .expect("pair")
                .iter()
                .map(|v| v.as_str().unwrap_or(""))
                .collect();
            let (first, steady) =
                latency_of(id).unwrap_or_else(|| panic!("no latency row for {id}"));
            assert_eq!((first, steady), (expected[0], expected[1]), "{id} latency");
        }
    }

    #[test]
    fn the_target_ids_are_unique_and_the_default_exists() {
        crate::pin_test_dir();
        let mut ids: Vec<&str> = OUTPUTS.iter().map(|output| output.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "a target id is duplicated");
        assert_eq!(OUTPUTS.len(), 5);
        assert!(output(DEFAULT_OUTPUT).is_some());
        // Every profile's dimensions are sane, so a typo cannot smuggle a 0 into a stream.
        for profile in DLNA_PROFILES {
            assert!(profile.fps > 0, "{} has no frame rate", profile.id);
            assert!(profile.height > 0, "{} has no height", profile.id);
            assert!(profile.video_bitrate > 0);
            assert!(profile.audio_bitrate > 0);
            assert!(!profile.suffix.is_empty());
            assert!(!profile.muxer.is_empty());
        }
    }
}
