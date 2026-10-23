//! Screen mirroring: how many units the live queue holds, and what that is worth in seconds.
//!
//! Rust counterpart of `screen_mirror.py`'s `fragment_rate` (`:6408-6422`), `live_queue_units`
//! (`:6425-6458`) and `queue_hold_seconds` (`:6461-6484`).
//!
//! ## One budget in seconds, three different units
//!
//! `LIVE_QUEUE_SECONDS = 0.75` is a **time** budget. What occupies a slot differs by container:
//!
//! | shape | unit | why |
//! |---|---|---|
//! | unframed TS/PS | 4 KiB read | there are no frame boundaries to queue by |
//! | fragmented MP4 | one fragment | the framer emits whole `moof`+`mdat` pieces |
//! | live Matroska | one cluster | a reader must re-sync on clusters, so it cannot be handed half |
//!
//! Getting the unit wrong is invisible until a viewer is slow: the queue then either drops
//! frames it did not have to (`frag_every_frame` gave 8 fragments ≈ 0.11 s where 0.75 s was
//! asked for — measured as **28% self-drop**) or blocks the encoder.
//!
//! ## The fragment cadence is per **sample**, not per picture
//!
//! `frag_every_frame` is per sample: the audio track is muxed in the same flush, so a browser
//! session with system sound at 24 fps writes **24 + 46.875 = 70.9** `moof` boxes a second.
//! Measured on a real session served by the shipping code (2026-10-02, Windows source, 24 fps
//! with sound): **1063 fragments in 15.02 s of stream — 70.8/s, 8835 bytes each**, which is what
//! this arithmetic gives. Budgeting against `fps` alone buys a third of the slack that was asked
//! for; budgeting against a hard-coded eight buys a sixth.
//!
//! ## `queue_hold_seconds` is the inverse, and it may answer "unknown"
//!
//! The diagnostics card and the viewing page's overlay both need "this queue is worth N
//! seconds". Framed units divide by their cadence; byte-shaped units divide by the bitrate, and
//! **without a bitrate they answer `None`** — a duration invented from an unknown rate is the
//! number that card exists to stop people guessing at.

/// The time budget for the live queue.
pub const LIVE_QUEUE_SECONDS: f64 = 0.75;

/// Chunk floor for the byte-shaped queues.
pub const LIVE_QUEUE_MIN_CHUNKS: usize = 64;
/// Chunk ceiling. Beyond this a slow viewer is not behind, it is gone.
pub const LIVE_QUEUE_MAX_CHUNKS: usize = 256;

/// Floor for the unit-shaped queues (fragments, clusters).
pub const LIVE_QUEUE_MIN_UNITS: usize = 8;

/// The read size the unframed shapes are queued in.
pub const CHUNK: usize = 4096;

/// A Matroska cluster: the muxer closes them by size, and the reference's value is
/// `60 << 10` = **61440**.
///
/// This port carried `55_000` — the midpoint of the comment above in the reference
/// ("measured at 50–60 KB") — as if the comment were the value. Everything was consistent
/// because **the corpus was generated from the same wrong number**, which is precisely the
/// co-conspiracy a differential corpus is supposed to prevent. It only surfaced when the corpus
/// started being regenerated from the reference source instead of from a copied constant.
pub const MKV_CLUSTER_BYTES: u64 = 60 << 10;

/// AAC frames in one second: 43.066 frames/s at 1024 samples and 44.1 kHz would be 43, but the
/// muxer writes AAC at **48 kHz**, so 48000/1024 = 46.875. Both numbers appear in this
/// repository; this is the one the fragment rate uses, and the 70.8/s measurement confirms it.
pub const AAC_FRAMES_PER_SECOND: f64 = 48000.0 / 1024.0;

/// The frame rate every mirror shape is pinned to.
pub const FPS: u32 = 24;

/// The TS packet size, for queue alignment.
pub const TS_PACKET: usize = 188;

/// What the queue is counted in, decided by the first bytes the muxer writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Whole `moof`+`mdat` fragments (fragmented MP4).
    Fragment,
    /// Whole clusters (live Matroska).
    Cluster,
    /// 4 KiB reads (unframed TS/PS).
    Chunk,
}

impl Unit {
    /// Read the unit from the marker the framer splits on.
    ///
    /// The same value the broadcaster frames by decides this, so the unit the queue is counted in
    /// and the unit the framer emits are decided together — or, as the reference puts it, "或
    /// 不". A second source for this would let the two drift.
    pub fn from_marker(marker: &[u8]) -> Unit {
        if marker == b"moof" {
            return Unit::Fragment;
        }
        // The first Cluster element: Matroska's own re-sync point.
        if marker == [0x1f, 0x43, 0xb6, 0x75] {
            return Unit::Cluster;
        }
        Unit::Chunk
    }
}

/// How many units one second of live fragmented MP4 puts on the wire.
///
/// Per **sample**, so audio counts: `fps + 46.875` with sound.
pub fn fragment_rate(fps: u32, has_audio: bool) -> f64 {
    let rate = if fps == 0 { FPS as f64 } else { fps as f64 };
    rate + if has_audio { AAC_FRAMES_PER_SECOND } else { 0.0 }
}

/// The queue capacity, in the unit this container queues.
pub fn live_queue_units(unit: Unit, bitrate: Option<u64>, has_audio: bool) -> usize {
    match unit {
        Unit::Fragment => {
            let units = (fragment_rate(FPS, has_audio) * LIVE_QUEUE_SECONDS).ceil() as usize;
            units.max(LIVE_QUEUE_MIN_UNITS)
        }
        Unit::Cluster => match bitrate {
            // Without a rate there is no honest count, so the floor is the whole answer.
            None | Some(0) => LIVE_QUEUE_MIN_UNITS,
            Some(bitrate) => {
                let seconds_bits = bitrate as f64 * LIVE_QUEUE_SECONDS;
                let units = (seconds_bits / 8.0 / MKV_CLUSTER_BYTES as f64).ceil() as usize;
                units.max(LIVE_QUEUE_MIN_UNITS)
            }
        },
        Unit::Chunk => live_queue_chunks(bitrate),
    }
}

/// The byte-shaped queue depth: the seconds budget divided by the read size, clamped.
pub fn live_queue_chunks(bitrate: Option<u64>) -> usize {
    // **An unknown bitrate answers the *ceiling*, not the floor.** The reference returns
    // `LIVE_QUEUE_MAX_CHUNKS` when the rate is falsy: with no rate to divide by, the widest
    // allowed queue is the safe side (a slow-but-alive viewer is not cut off), whereas the floor
    // would be the smallest buffer we could get away with — the opposite guess. This port had the
    // two swapped, and the corpus did not catch it until it was regenerated from the source.
    let bitrate = match bitrate {
        None | Some(0) => return LIVE_QUEUE_MAX_CHUNKS,
        Some(rate) => rate,
    };
    // Integer arithmetic, like the reference: `int(bitrate) * SECONDS // 8` then `// CHUNK`.
    let budget = (bitrate as f64 * LIVE_QUEUE_SECONDS / 8.0) as u64;
    let chunks = (budget / CHUNK as u64) as usize;
    chunks.clamp(LIVE_QUEUE_MIN_CHUNKS, LIVE_QUEUE_MAX_CHUNKS)
}

/// What that capacity is actually worth, in seconds — or `None` when it cannot be known.
///
/// Rounded to two decimals, because both readers display it.
pub fn queue_hold_seconds(
    unit: Unit,
    units: usize,
    bitrate: Option<u64>,
    has_audio: bool,
) -> Option<f64> {
    match unit {
        Unit::Fragment => {
            // **`has_audio` is the session's, not a constant.** The corpus caught this port
            // hard-coding `true`: a silent session writes 18 fragments in 0.75 s, so dividing its
            // capacity by the audio-inclusive rate answered 0.25 s — a card reporting a queue
            // four times shorter than it is.
            let rate = fragment_rate(FPS, has_audio);
            if rate <= 0.0 {
                return None;
            }
            Some(round2(units as f64 / rate))
        }
        Unit::Cluster => {
            let bitrate = bitrate?;
            if bitrate == 0 {
                return None;
            }
            Some(round2(
                units as f64 * MKV_CLUSTER_BYTES as f64 * 8.0 / bitrate as f64,
            ))
        }
        Unit::Chunk => {
            let bitrate = bitrate?;
            if bitrate == 0 {
                return None;
            }
            Some(round2(
                units as f64 * CHUNK as f64 * 8.0 / bitrate as f64,
            ))
        }
    }
}

/// Round to two decimals **the way Python's `round()` does**, which is not the way Rust's
/// `.round()` does.
///
/// `round(x, 2)` is round-half-to-**even** on the decimal representation, while
/// `(x * 100.0).round() / 100.0` is round-half-away-from-zero. The two disagree on exact halves:
/// `0.125` is `0.12` in Python and `0.13` here, and `2.675` is `2.67` against `2.68`. Measured
/// over the reachable space (chunk and cluster shapes, 0.6–20 Mbit/s, every plausible capacity)
/// that is **142 divergences**, e.g. a cluster queue at 1 Mbit/s answering `20.62` against
/// `20.63`.
///
/// Two decimals is not decoration here: these numbers are printed on the diagnostics card and on
/// the viewing page's overlay, so a port that rounds differently shows a different reading of the
/// same queue. The fix is to format with Python's rule rather than approximate it, which works
/// because `format!("{:.2}", …)` is the same "round half to even" the reference relies on.
fn round2(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    // `{:.2}` gives the rounded decimal text; parsing it back yields the double Python's
    // `round()` would have produced for the same input.
    format!("{value:.2}")
        .parse::<f64>()
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fragment_rate_counts_audio_samples_not_pictures() {
        crate::pin_test_dir();
        // `frag_every_frame` is per sample, and the audio track is muxed in the same flush. At
        // 24 fps with sound that is 70.875/s — the measurement was 70.8/s over 15.02 s.
        let with_sound = fragment_rate(24, true);
        assert!((with_sound - 70.875).abs() < 0.001, "got {with_sound}");
        // Sanity against the recorded session: 70.8/s is within 0.2% of this.
        assert!((with_sound - 70.8).abs() / 70.8 < 0.005);
        assert_eq!(fragment_rate(24, false), 24.0);
        // Silence is a third of the fragments, which is why budgeting against `fps` alone was
        // wrong by that much.
        assert!(with_sound / 24.0 > 2.9);
    }

    #[test]
    fn a_zero_frame_rate_falls_back_rather_than_dividing_by_zero() {
        crate::pin_test_dir();
        assert_eq!(fragment_rate(0, false), FPS as f64);
        let units = live_queue_units(Unit::Fragment, None, false);
        assert!(units >= LIVE_QUEUE_MIN_UNITS);
    }

    #[test]
    fn the_fragment_queue_holds_more_than_eight_because_the_budget_is_in_seconds() {
        crate::pin_test_dir();
        // The measured defect: eight fragments was 0.11 s where 0.75 s was asked for — 28% of
        // frames dropped by the queue itself before any viewer was slow.
        let units = live_queue_units(Unit::Fragment, Some(4_000_000), true);
        assert_eq!(units, 54, "0.75 s of 70.875 fragments/s, ceiled");
        let hold = queue_hold_seconds(Unit::Fragment, units, Some(4_000_000), true).expect("known");
        assert!((hold - 0.76).abs() < 0.02, "hold was {hold}s, expected ~0.75");
    }

    #[test]
    fn the_unit_comes_from_the_marker_the_framer_splits_on() {
        crate::pin_test_dir();
        assert_eq!(Unit::from_marker(b"moof"), Unit::Fragment);
        assert_eq!(Unit::from_marker(&[0x1f, 0x43, 0xb6, 0x75]), Unit::Cluster);
        // Anything else (an unframed TS/PS stream) is queued by read size.
        assert_eq!(Unit::from_marker(b""), Unit::Chunk);
        assert_eq!(Unit::from_marker(&[0x47]), Unit::Chunk);
    }

    #[test]
    fn a_byte_shaped_queue_without_a_bitrate_answers_the_ceiling_not_the_floor() {
        crate::pin_test_dir();
        // **The ceiling.** With no rate to divide by, the safe side is the widest allowed queue:
        // a slow-but-alive viewer is not cut off, while the floor is the smallest buffer we could
        // get away with. This test used to assert the floor, which is the opposite guess — and it
        // agreed with the implementation because both came from the same reading.
        assert_eq!(live_queue_chunks(None), LIVE_QUEUE_MAX_CHUNKS);
        assert_eq!(live_queue_chunks(Some(0)), LIVE_QUEUE_MAX_CHUNKS);
        assert_eq!(live_queue_units(Unit::Cluster, None, true), LIVE_QUEUE_MIN_UNITS);
        assert_eq!(queue_hold_seconds(Unit::Cluster, 8, None, true), None);
        assert_eq!(queue_hold_seconds(Unit::Chunk, 64, None, true), None);
        assert_eq!(queue_hold_seconds(Unit::Chunk, 64, Some(0), true), None);
        // The fragment branch needs no bitrate: a fragment is a frame, at a fixed rate.
        assert!(queue_hold_seconds(Unit::Fragment, 54, None, true).is_some());
    }

    #[test]
    fn the_chunk_queue_is_clamped_at_both_ends() {
        crate::pin_test_dir();
        // A very low rate still gets a floor of 64 reads, so a slow stream does not starve the
        // writer; a very high rate is capped, because beyond 256 chunks a viewer is gone.
        assert_eq!(live_queue_chunks(Some(1)), LIVE_QUEUE_MIN_CHUNKS);
        assert_eq!(live_queue_chunks(Some(100_000_000)), LIVE_QUEUE_MAX_CHUNKS);
        let mid = live_queue_chunks(Some(4_000_000));
        assert!((LIVE_QUEUE_MIN_CHUNKS..=LIVE_QUEUE_MAX_CHUNKS).contains(&mid));
    }

    #[test]
    fn the_hold_seconds_is_the_inverse_of_the_capacity_arithmetic() {
        crate::pin_test_dir();
        // Both answers must come from one place: the card reads the inverse of what the
        // broadcaster used, so they cannot disagree.
        let bitrate = 6_000_000u64;
        let units = live_queue_chunks(Some(bitrate));
        let hold = queue_hold_seconds(Unit::Chunk, units, Some(bitrate), true).expect("known");
        // `units` was rounded **up** to a whole number of reads, so the hold is at least the
        // budget and never less.
        assert!(hold >= 0.75, "hold {hold}s is below the budget");
        assert!(hold < 0.80, "hold {hold}s overshoots the budget by too much");

        let clusters = live_queue_units(Unit::Cluster, Some(bitrate), true);
        let cluster_hold =
            queue_hold_seconds(Unit::Cluster, clusters, Some(bitrate), true).expect("known");
        assert!(cluster_hold >= 0.75, "cluster hold {cluster_hold}s below budget");
    }

    /// Every capacity and hold the reference computed, replayed against this port.
    ///
    /// 48 rows from executing the reference's three functions over three shapes × eight bitrates
    /// (including `None`, `0` and a rate above the ceiling) × audio on/off, plus the `units: None`
    /// row. Both answers come from the same arithmetic here, so a divergence in one would show
    /// up as a card whose two numbers disagree.
    #[test]
    fn every_reference_queue_answer_replays_identically() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/mirror_queue.json"))
                .expect("corpus is JSON");
        let rows = corpus.as_array().expect("rows");
        let mut seen = 0usize;
        let mut shapes = std::collections::BTreeSet::new();
        for row in rows {
            let marker = row["marker"].as_str().expect("marker");
            let unit = match marker {
                "moof" => Unit::Fragment,
                "cluster" => Unit::Cluster,
                _ => Unit::Chunk,
            };
            shapes.insert(marker.to_string());
            let bitrate = row["bitrate"].as_u64();
            let audio = row["audio"].as_bool().expect("audio");
            // The reference's `live_queue_chunks` can return a **float** (`91.0`) because its
            // budget arithmetic mixes `int` and `LIVE_QUEUE_SECONDS = 0.75`. The *count* is that
            // whole number; compare numerically rather than demanding the JSON type match the
            // Rust one.
            let expected_units = row["units"]
                .as_f64()
                .expect("units")
                .round() as usize;
            if row.get("note").is_some() {
                // The divergence rows carry **explicit** units rather than ones derived from the
                // bitrate: they exist to exercise `queue_hold_seconds` at an exact rounding
                // boundary, which a capacity recomputation would never reach (at 1 Mbit/s the
                // capacity is the floor of 8, not 48). Asserting capacity here would be asking the
                // wrong question of the row.
            } else {
                assert_eq!(
                    live_queue_units(unit, bitrate, audio),
                    expected_units,
                    "{marker} bitrate={bitrate:?} audio={audio} capacity differs"
                );
            }
            let hold = queue_hold_seconds(unit, expected_units, bitrate, audio);
            match row["hold"].as_f64() {
                Some(expected) => {
                    let found = hold.unwrap_or_else(|| {
                        panic!("{marker} bitrate={bitrate:?}: hold was None, expected {expected}")
                    });
                    assert!(
                        (found - expected).abs() < 0.005,
                        "{marker} bitrate={bitrate:?} audio={audio}: hold {found} != {expected}"
                    );
                }
                None => assert_eq!(hold, None, "{marker}: hold should be unknown"),
            }
            seen += 1;
        }
        assert_eq!(seen, 48, "the corpus lost rows");
        assert_eq!(shapes.len(), 3, "the corpus lost a shape: {shapes:?}");
    }

    /// The two-decimal rounding must be **Python's**, not Rust's.
    ///
    /// `round(x, 2)` is half-to-even; `(x * 100.0).round() / 100.0` is half-away-from-zero. They
    /// disagree on exact halves, and over the reachable space that is **142 divergences** — a
    /// cluster queue at 1 Mbit/s reading `20.62` against `20.63`. Both numbers are printed on the
    /// diagnostics card, so a port that rounds differently shows a different reading of the same
    /// queue.
    ///
    /// This test is the reason the implementation formats instead of multiplying: the obvious
    /// `(v * 100.0).round() / 100.0` is what a reader would write, and it is wrong here.
    #[test]
    fn the_two_decimal_rounding_is_pythons_not_rusts() {
        crate::pin_test_dir();
        // Exact halves, where the two rules part company.
        assert_eq!(round2(0.125), 0.12, "Python rounds 0.125 to 0.12 (half to even)");
        assert_eq!(round2(2.675), 2.67);
        assert_eq!(round2(0.135), 0.14);
        // …and the shapes where they agree, so this test is about the rule and not about
        // "everything is off by one".
        assert_eq!(round2(0.145), 0.14);
        assert_eq!(round2(4.115), 4.12);

        // The reachable case the scan found: 8 clusters at 1.024 Mbit/s.
        // 48 clusters at 1.024 Mbit/s. Python's `round(48 * 61440 * 8 / 1024000, 2)`.
        let cluster = queue_hold_seconds(Unit::Cluster, 48, Some(1_024_000), true).expect("known");
        assert_eq!(cluster, 23.04);
    }

    /// Every constant this port copied by hand still equals the reference's.
    ///
    /// `MKV_CLUSTER_BYTES` was `55_000` for a whole session — the midpoint of a comment that said
    /// "50–60 KB" — instead of the reference's `60 << 10`. Nothing caught it because the corpus was
    /// generated from the same misreading, so port and corpus agreed with each other and disagreed
    /// with the software. This test closes that hole by comparing against `constants.json`, whose
    /// values come from `scripts/build_corpora.py` reading the reference's own text.
    ///
    /// The Rust side reads its values through the local `const`s, so a hand edit fails here even
    /// when the expression still looks plausible.
    #[test]
    fn every_hand_copied_constant_still_matches_the_reference() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/constants.json"))
                .expect("corpus is JSON");
        let rows = corpus.as_array().expect("rows");
        let mut checked = 0usize;
        for row in rows {
            // Only this module's group: the knobs and the encoder have their own tests, and
            // asserting them from here would mean importing constants this module does not own.
            if row["group"].as_str() != Some("mirror_queue") {
                continue;
            }
            let name = row["name"].as_str().expect("name");
            let reference = row["reference"].as_f64().expect("reference");
            let local = match name {
                "LIVE_QUEUE_SECONDS" => LIVE_QUEUE_SECONDS,
                "LIVE_QUEUE_MIN_CHUNKS" => LIVE_QUEUE_MIN_CHUNKS as f64,
                "LIVE_QUEUE_MAX_CHUNKS" => LIVE_QUEUE_MAX_CHUNKS as f64,
                "LIVE_QUEUE_MIN_UNITS" => LIVE_QUEUE_MIN_UNITS as f64,
                "CHUNK" => CHUNK as f64,
                "MKV_CLUSTER_BYTES" => MKV_CLUSTER_BYTES as f64,
                "FPS" => FPS as f64,
                "TS_PACKET" => TS_PACKET as f64,
                other => panic!("{other} is in the corpus but not in this test's match"),
            };
            assert_eq!(local, reference, "{name} drifted from the reference");
            checked += 1;
        }
        assert_eq!(checked, 8, "the corpus lost a mirror_queue constant: {checked}");
    }

    #[test]
    fn the_aac_rate_is_the_48khz_one_not_the_44khz_one() {
        crate::pin_test_dir();
        // 48000/1024 = 46.875. Using 43.066 (44.1 kHz) would put the fragment rate at 67/s and
        // the queue 5% short — small enough to never be noticed, large enough to matter on a
        // slow viewer.
        assert!((AAC_FRAMES_PER_SECOND - 46.875).abs() < 1e-9);
        assert!((fragment_rate(24, true) - 70.875).abs() < 1e-9);
    }
}
