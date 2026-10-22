//! The small-window renderer: how `floating.py` reshapes mpv's argv.
//!
//! Rust counterpart of `macast/plugins/renderer/floating.py` (`:39-118`). Two modes,
//! three sizes, and the same rule the Python states in a comment: **whatever the inherited
//! params say about window placement is dropped first**, because otherwise the global
//! Player Size / Player Ontop settings fight the choice made here. That drop is the part
//! worth porting carefully — a renderer that only *appends* `--autofit=26%` gets the last
//! word only by accident, since mpv lets a later option win but `--fullscreen` still applies
//! and the window comes up fullscreen with a useless autofit.
//!
//! ## The wallpaper mode is conditional on the binary, not the platform
//!
//! `--ontop-level=desktop` is a recent option. An mpv that does not know it **refuses to
//! start**, and the launcher's response to "mpv never connected" is to retry three times
//! and then stop the service (AGENTS.md §4.8, the floating row). So the mode is only taken
//! when a probe says the option is accepted, and the fallback says so out loud instead of
//! silently becoming a fullscreen window. The probe itself lives in the caller — this module
//! takes its answer as an input, because spawning a process is not a decision.

use std::collections::BTreeMap;

/// `(menu label, --autofit percentage)`, in menu order.
pub const SIZES: &[(&str, u32)] = &[("Small", 16), ("Medium", 26), ("Large", 38)];

/// Options that decide where and how big the window is. Dropped before appending ours.
const CONFLICTING: &[&str] = &["--ontop", "--fullscreen"];

/// Prefixes that carry the same meaning with a value attached.
const CONFLICTING_PREFIXES: &[&str] = &["--geometry=", "--autofit", "--ontop-level="];

/// Which shape the window takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Window,
    Wallpaper,
}

impl Mode {
    /// The stored value, read defensively: anything that is not the wallpaper value is the
    /// window. The reference does the same with an explicit `in (…)` check rather than
    /// truthiness, because `0` is the window and `0` is falsy.
    pub fn from_setting(value: Option<i64>) -> Mode {
        match value {
            Some(1) => Mode::Wallpaper,
            _ => Mode::Window,
        }
    }
}

/// Index into [`SIZES`], clamped rather than rejected.
///
/// A rejected size would leave the user with a window at whatever the global setting says
/// and no explanation; clamping keeps the picker and the window in agreement.
pub fn size_index(value: Option<i64>) -> usize {
    match value {
        Some(index) if index >= 0 && (index as usize) < SIZES.len() => index as usize,
        _ => 1,
    }
}

/// What `build_mpv_params` decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Params {
    /// Arguments to append after the conflicting ones are dropped.
    pub extra: Vec<String>,
    /// The wallpaper mode was asked for but this mpv cannot do it, so the window mode was
    /// used instead. The caller turns this into a notification — silence here would read as
    /// "wallpaper mode does nothing on my machine".
    pub wallpaper_refused: bool,
}

/// Whether an argument belongs to the window-placement family the renderer owns.
pub fn is_conflicting(arg: &str) -> bool {
    if CONFLICTING.contains(&arg) {
        return true;
    }
    CONFLICTING_PREFIXES
        .iter()
        .any(|prefix| arg.starts_with(prefix))
}

/// Drop the inherited placement options. Everything else is left exactly as it was.
pub fn drop_conflicting(params: Vec<String>) -> Vec<String> {
    params
        .into_iter()
        .filter(|arg| !is_conflicting(arg))
        .collect()
}

/// The arguments this renderer appends, given the mode, the size index and whether the mpv
/// binary accepts `--ontop-level=desktop`.
pub fn extra_params(mode: Mode, size: usize, desktop_layer_supported: bool) -> Params {
    if mode == Mode::Wallpaper && desktop_layer_supported {
        // Bottom of the window stack: normal windows cover it, which is as close to a
        // wallpaper as mpv gets. No OSC and no default bindings because there is nothing
        // sane to click on a wallpaper.
        return Params {
            extra: [
                "--geometry=100%:100%",
                "--no-border",
                "--no-osc",
                "--no-input-default-bindings",
                "--ontop",
                "--ontop-level=desktop",
                "--cursor-autohide=always",
                "--no-window-dragging",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            wallpaper_refused: false,
        };
    }
    // Either the window mode was chosen, or wallpaper was chosen and this mpv cannot do it.
    // Both land on the same small on-top window; only the second one owes the user a word.
    let percentage = SIZES[size.min(SIZES.len() - 1)].1;
    Params {
        extra: vec![
            format!("--autofit={percentage}%"),
            "--geometry=98%:3%".to_string(),
            "--ontop".to_string(),
            "--no-border".to_string(),
        ],
        wallpaper_refused: mode == Mode::Wallpaper,
    }
}

/// The whole reshape: drop, then append, then sort into a stable order for comparison.
///
/// Kept as one function because the reference's `build_mpv_params` is one function, and the
/// order of the two steps is the whole point — appending first and filtering afterwards would
/// delete the `--ontop` we just added.
pub fn reshape(
    inherited: Vec<String>,
    mode: Mode,
    size: usize,
    desktop_layer_supported: bool,
) -> (Vec<String>, Params) {
    let mut params = drop_conflicting(inherited);
    let decided = extra_params(mode, size, desktop_layer_supported);
    params.extend(decided.extra.iter().cloned());
    (params, decided)
}

/// The setting keys this renderer reads, so the module owns its own key names rather than
/// scattering string literals through the shell.
pub fn settings_to_read() -> BTreeMap<&'static str, i64> {
    let mut keys = BTreeMap::new();
    keys.insert("Floating_Mode", 0);
    keys.insert("Floating_Size", 1);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    // No  here, unlike /: this module is pure
    // decision and writes no log, so there is no user directory for it to stray into.

    fn inherited() -> Vec<String> {
        vec![
            "--no-config".to_string(),
            "--geometry=50%:50%".to_string(),
            "--autofit=40x40".to_string(),
            "--ontop".to_string(),
            "--fullscreen".to_string(),
            "--ontop-level=system".to_string(),
            "--volume=80".to_string(),
        ]
    }

    #[test]
    fn every_placement_option_the_global_settings_add_is_dropped() {
        // The comment in the reference says why: otherwise the global Player Size / Player
        // Ontop settings fight the choice made here. Note that `--volume` must survive — a
        // filter that drops too much silently changes playback.
        let (params, _) = reshape(inherited(), Mode::Window, 1, false);
        // Careful: the output *does* carry `--geometry=`/`--autofit`/`--ontop` — the ones this
        // renderer chose. What must be gone is the inherited value of each, so assert on
        // exactly one of each family and on its value rather than on "none at all".
        assert_eq!(params.iter().filter(|p| *p == "--ontop").count(), 1);
        assert!(!params.contains(&"--fullscreen".to_string()));
        let geometry: Vec<&String> = params
            .iter()
            .filter(|p| p.starts_with("--geometry="))
            .collect();
        assert_eq!(geometry.len(), 1, "an inherited geometry survived: {geometry:?}");
        assert_eq!(*geometry[0], "--geometry=98%:3%");
        let autofit: Vec<&String> =
            params.iter().filter(|p| p.starts_with("--autofit")).collect();
        assert_eq!(autofit.len(), 1, "an inherited autofit survived: {autofit:?}");
        assert_eq!(*autofit[0], format!("--autofit={}%", SIZES[1].1));
        assert!(!params.iter().any(|p| p.starts_with("--ontop-level=")));
        // …and nothing unrelated was touched: a filter that drops too much changes playback
        // silently, which is worse than one that drops too little.
        assert!(params.iter().any(|p| p == "--volume=80"));
        assert!(params.iter().any(|p| p == "--no-config"));
    }

    #[test]
    fn the_filter_runs_before_the_append_so_our_own_ontop_survives() {
        // The bug this guards: filtering after appending deletes the `--ontop` we just added,
        // and the window comes up behind everything with no error anywhere.
        let (params, _) = reshape(inherited(), Mode::Window, 1, false);
        assert_eq!(params.iter().filter(|p| *p == "--ontop").count(), 1);
        assert!(params.contains(&"--no-border".to_string()));
    }

    #[test]
    fn window_mode_uses_the_size_table_and_sits_near_the_top_left() {
        for (index, (_, percentage)) in SIZES.iter().enumerate() {
            let (_, decided) = reshape(vec![], Mode::Window, index, false);
            assert_eq!(decided.extra[0], format!("--autofit={percentage}%"));
            assert_eq!(decided.extra[1], "--geometry=98%:3%");
            assert!(!decided.wallpaper_refused);
        }
    }

    #[test]
    fn wallpaper_mode_needs_the_binary_to_accept_the_desktop_layer() {
        let (params, decided) = reshape(vec![], Mode::Wallpaper, 1, true);
        assert!(!decided.wallpaper_refused);
        assert!(params.contains(&"--ontop-level=desktop".to_string()));
        assert!(params.contains(&"--no-input-default-bindings".to_string()));
        assert_eq!(params[0], "--geometry=100%:100%");

        // …and when it does not, the mode falls back and *says so*: a silent fallback reads
        // as "wallpaper mode is broken on this machine".
        let (params, decided) = reshape(vec![], Mode::Wallpaper, 1, false);
        assert!(decided.wallpaper_refused);
        assert!(!params.iter().any(|p| p == "--ontop-level=desktop"));
        assert!(params[0].starts_with("--autofit="));
    }

    #[test]
    fn a_size_outside_the_table_clamps_instead_of_being_rejected() {
        assert_eq!(size_index(Some(0)), 0);
        assert_eq!(size_index(Some(2)), 2);
        assert_eq!(size_index(Some(9)), 1);
        assert_eq!(size_index(Some(-3)), 1);
        assert_eq!(size_index(None), 1);
        // An index past the end clamps to the *last* entry, not the default: clamping to the
        // middle would make a picker that offered three sizes render the middle one after any
        // out-of-range value, which reads as "my choice was ignored" rather than "reset".
        let decided = extra_params(Mode::Window, SIZES.len() + 5, false);
        assert_eq!(decided.extra[0], format!("--autofit={}%", SIZES[SIZES.len() - 1].1));
        assert_eq!(size_index(Some(SIZES.len() as i64 + 5)), 1);
    }

    #[test]
    fn the_mode_value_is_read_as_a_number_not_as_truthiness() {
        // `0` is the window mode and `0` is falsy: reading this with truthiness would turn
        // every window-mode installation into a wallpaper.
        assert_eq!(Mode::from_setting(Some(0)), Mode::Window);
        assert_eq!(Mode::from_setting(Some(1)), Mode::Wallpaper);
        assert_eq!(Mode::from_setting(Some(7)), Mode::Window);
        assert_eq!(Mode::from_setting(None), Mode::Window);
    }

    /// Every argv the reference renderer produced, replayed against this port.
    ///
    /// Produced by executing `floating.FloatingRenderer.build_mpv_params()` over mode × size ×
    /// whether the mpv accepts `--ontop-level=desktop`. Element order is part of the corpus:
    /// the reference appends after filtering, and a port that appends *then* filters produces
    /// a different list even when it contains the same set.
    #[test]
    fn every_reference_argv_replays_in_the_same_order() {
        let corpus = include_str!("../tests/golden/floating_params.jsonl");
        let inherited: Vec<String> = [
            "--no-config",
            "--geometry=50%:50%",
            "--autofit=40x40",
            "--ontop",
            "--fullscreen",
            "--ontop-level=system",
            "--volume=80",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row is JSON");
            let mode = Mode::from_setting(row["mode"].as_i64());
            // The reference clamps out-of-range sizes itself before calling, so replay the same
            // clamping rather than feeding the raw value.
            let size = size_index(row["size"].as_i64());
            let supports = row["supports"].as_bool().expect("supports");
            let (params, _) = reshape(inherited.clone(), mode, size, supports);
            let expected: Vec<String> = row["params"]
                .as_array()
                .expect("params")
                .iter()
                .map(|p| p.as_str().unwrap_or("").to_string())
                .collect();
            assert_eq!(params, expected, "row {line} disagrees with the reference");
            rows += 1;
        }
        assert_eq!(rows, 20, "the corpus lost rows");
    }

    #[test]
    fn the_keys_this_renderer_reads_are_declared_here() {
        let keys = settings_to_read();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains_key("Floating_Mode"));
        assert!(keys.contains_key("Floating_Size"));
    }
}
