//! The mpv command line, built the way the renderer builds it: from the setting
//! file's raw values, with Python's rules for what those values mean.
//!
//! The reference is `MPVRenderer.build_mpv_params` in `macast_renderer/mpv.py`.
//! Two things about that function are the reason this module exists as a pure
//! one, and both are the kind AGENTS.md §4.2 keeps recording:
//!
//! - **it is called again on every start attempt**, and it reads the setting
//!   file each time. So the argv a user's mpv ran with is a function of whatever
//!   was in `macast_setting.json` at that moment, including a value a person
//!   hand-edited into the "advanced settings" JSON on the settings page. That is
//!   where the Python-semantics cases below come from — they are not hypothetical
//!   input shapes, they are the shapes a hand-edit produces.
//! - **a wrong value here does not fail loudly, and it does not fail the way a
//!   caller would guess.** `params = self.build_mpv_params()` sits at
//!   `mpv.py:527`, which is **outside** the `try:` opened at `:532` — measured,
//!   not inferred, by replaying that exact control flow on CPython 3.12.15: a
//!   `PlayerSize` that is not a number lets the exception escape `start_mpv`
//!   altogether, so the thread dies with `Exception in thread MPV_THREAD` on
//!   stderr and **nothing** the loop guards ever runs. No 「mpv starting」, no
//!   `publish('mpv_start')`, no retry, no three-attempt budget spent, no
//!   「mpv cannot start」, no notify, no `stop_service()`. The log the user reads
//!   is one traceback, not a log full of them — and `mpv_thread.is_alive()` is
//!   then False, which is what ends the ipc loop too. The port returns that case
//!   as [`BuildError`] so the caller can name the key, and `crate::player` logs it
//!   and abandons the attempt, keeping the silence faithful.
//!
//! Everything the reference reaches for outside its own arguments — the setting
//! file, `os.path.exists`, `os.listdir`, `sys.platform`, the socket path — is a
//! field of [`Config`] here, so the builder can be asked "what argv does this
//! machine produce" without being a machine.

use serde_json::Value;
use std::fmt;

use crate::pyops;

/// `PlayerOntop_True.value` — the reference's default for the always-on-top
/// knob, and the value a setting file with no `PlayerOntop` key yields.
pub const DEFAULT_ONTOP: i64 = 1;
/// `PlayerPosition_RightTop.value`.
pub const DEFAULT_POSITION: i64 = 2;
/// `PlayerSize_Normal.value`.
pub const DEFAULT_SIZE: i64 = 1;
/// `PlayerHW_Enable.value`.
pub const DEFAULT_HW: i64 = 1;

/// `player_position_data` in the reference: `[x, y]` percentages indexed by
/// `PlayerPosition`, in that enum's order — LeftTop, LeftBottom, RightTop,
/// RightBottom, Center.
pub const POSITION_TABLE: [[i64; 2]; 5] = [[2, 5], [2, 98], [98, 5], [98, 98], [50, 50]];

/// Everything `build_mpv_params` reads from the outside world.
///
/// `Default` is the state of a fresh install: mpv on `PATH`, the four knobs at
/// the reference's defaults, no scripts directory, and — deliberately — a
/// non-darwin `platform`, so that a test written without a `Config` override
/// cannot accidentally become a macOS-only test.
#[derive(Debug, Clone)]
pub struct Config {
    /// `MPVRenderer.__init__`'s `path`, which defaults to the bare word `mpv`.
    pub mpv_path: String,
    /// What `--input-ipc-server` is told to listen on. See
    /// [`crate::command::ipc_socket_path`] for how the reference makes it.
    pub ipc_socket: String,
    /// `sys.platform`: `darwin` turns on the AppKit block, `windows` changes how
    /// script paths are joined.
    pub platform: String,
    /// `PlayerOntop`, read with **truthiness** because that is how the reference
    /// reads it: `if ontop:`.
    pub ontop: Value,
    /// `PlayerPosition`, an index into [`POSITION_TABLE`].
    pub position: Value,
    /// `PlayerSize`, compared with `<=` and `==`.
    pub size: Value,
    /// `PlayerHW`, compared with `==` twice.
    pub hw: Value,
    /// `Setting.get_base_path('scripts')`.
    pub scripts_dir: String,
    /// `os.listdir` of that directory when it exists, and empty when it does
    /// not. **Unsorted on purpose** — see [`build`].
    pub scripts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mpv_path: "mpv".to_string(),
            ipc_socket: String::new(),
            platform: "linux".to_string(),
            ontop: Value::from(DEFAULT_ONTOP),
            position: Value::from(DEFAULT_POSITION),
            size: Value::from(DEFAULT_SIZE),
            hw: Value::from(DEFAULT_HW),
            scripts_dir: String::new(),
            scripts: Vec::new(),
        }
    }
}

/// The command line plus the things the builder had to decide about it.
///
/// A second return channel rather than a silent clamp: the reference would have
/// raised, or would have silently taken neither branch, and a port that quietly
/// fixes a user's bad value owes them a line saying it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub argv: Vec<String>,
    pub warnings: Vec<String>,
}

/// A knob whose stored value makes Python raise, which is only two of the three.
///
/// `PlayerHW` is deliberately **not** in this enum. `value == PlayerHW_Disable`
/// and `value == PlayerHW_Force` are both simply `False` for a value Python
/// cannot compare to an int, so the reference takes neither branch, leaves the
/// `--hwdec=yes` it already built in place, and starts mpv. Reporting that as an
/// error would give a person who hand-typed `null` into the advanced-settings
/// JSON a player that never starts, where today they have a working one — so it
/// is a [`Build::warnings`] entry instead, and the argument was had in the open
/// because "the port refuses" is exactly the kind of improvement that reads as a
/// regression at 3 a.m.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// `player_position_data[value]` — Python's "list indices must be integers".
    Position(Value),
    /// `value <= PlayerSize_Large` — Python's "'<=' not supported between
    /// instances of 'str' and 'int'".
    Size(Value),
}

impl BuildError {
    /// The key as the setting file spells it, so the message is greppable.
    pub fn key(&self) -> &'static str {
        match self {
            BuildError::Position(_) => "PlayerPosition",
            BuildError::Size(_) => "PlayerSize",
        }
    }

    /// What the reference would have printed, next to what this port does.
    ///
    /// The reference text is in here because a user who searches the log for the
    /// symptom of the old build should find this entry, and because the
    /// difference between the two is the point of the type.
    pub fn message(&self) -> String {
        match self {
            BuildError::Position(v) => format!(
                "PlayerPosition is not an integer: {} \u{28}Python: list indices must be integers \
                 or slices, not {}\u{29}",
                pyops::py_repr(v),
                pyops::type_name(v)
            ),
            // Python names no type for the left operand here — it asks for one and
            // fails, which is why the message quotes its error instead of naming a
            // type the way the `Position` arm can.
            BuildError::Size(v) => format!(
                "PlayerSize is not a number: {} \u{28}Python: '<=' not supported between instances \
                 of '{}' and 'int'\u{29}",
                pyops::py_repr(v),
                pyops::type_name(v)
            ),
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for BuildError {}

/// `build_mpv_params`, in the reference's order, with the reference's quirks.
pub fn build(config: &Config) -> Result<Build, BuildError> {
    let mut warnings = Vec::new();
    let mut params: Vec<String> = vec![
        config.mpv_path.clone(),
        format!("--input-ipc-server={}", config.ipc_socket),
        "--image-display-duration=inf".to_string(),
        "--idle=yes".to_string(),
        "--no-terminal".to_string(),
        "--on-all-workspaces".to_string(),
        "--hwdec=yes".to_string(),
        "--save-position-on-quit=yes".to_string(),
        // One argument, not three: this is one Python string concatenation in the
        // reference, and mpv's `--script-opts` is a comma list. Splitting it is
        // the kind of mistake that reads as "the OSC layout changed".
        "--script-opts=osc-timetotal=yes,osc-layout=bottombar,osc-title=${title},\
         osc-showwindowed=yes,osc-seekbarstyle=bar,osc-visibility=auto"
            .to_string(),
    ];

    // `if ontop:` — truthiness, so `""` and `0` mean "no" while the string "no"
    // means "yes". The reference's own shape, kept because the setting page has
    // always written a number here and a person editing the JSON writes a word.
    if pyops::truthy(&config.ontop) {
        params.push("--ontop".to_string());
    }

    let (x, y) = position_anchor(config, &mut warnings)?;
    params.push(format!("--geometry={x}%:{y}%"));

    params.extend(script_args(config));

    push_size_args(config, &mut params)?;

    if config.platform == "darwin" {
        // `--on-all-workspaces` appears twice in the reference's output: once in
        // the base list, once here. mpv takes the last value of a repeated
        // option, and both spellings are the same, so this is a no-op — but it is
        // the reference's no-op, and an argv diff against a real build is worth
        // more than a tidy list.
        params.push("--ontop-level=system".to_string());
        params.push("--on-all-workspaces".to_string());
        params.push("--macos-app-activation-policy=accessory".to_string());
    }

    push_hw_args(config, &mut params, &mut warnings);

    Ok(Build {
        argv: params,
        warnings,
    })
}

/// `player_position_data[player_position]`, with Python's negative indexing and
/// one documented divergence at the edges.
fn position_anchor(config: &Config, warnings: &mut Vec<String>) -> Result<(i64, i64), BuildError> {
    let raw = pyops::as_int(&config.position).ok_or(BuildError::Position(config.position.clone()))?;
    let len = POSITION_TABLE.len() as i64;
    // Python's `list[-1]` is the last element, and `list[-6]` is an IndexError.
    let index = if raw < 0 { raw + len } else { raw };
    let index = if index < 0 {
        warnings.push(format!(
            "PlayerPosition {} is before the start of the position table; clamped to 0 \u{28}Python: \
             IndexError, and the thread dies\u{29}",
            raw
        ));
        0
    } else if index >= len {
        warnings.push(format!(
            "PlayerPosition {} is past the end of the position table; clamped to {}\u{28}Python: \
             IndexError, and the thread dies\u{29}",
            raw,
            len - 1
        ));
        len - 1
    } else {
        index
    };
    Ok((
        POSITION_TABLE[index as usize][0],
        POSITION_TABLE[index as usize][1],
    ))
}

/// `--script=<dir>/<name>` for every `.lua` in the user's scripts directory.
///
/// `os.listdir` has no order guarantee, and the reference does not sort, so this
/// does not either: the filter is what is being ported, and a test that wants to
/// see "which files became options" has to be able to see the order it was given.
fn script_args(config: &Config) -> Vec<String> {
    config
        .scripts
        .iter()
        .filter(|name| name.ends_with(".lua"))
        .map(|name| format!("--script={}", join(&config.scripts_dir, name, &config.platform)))
        .collect()
}

/// `os.path.join`, for the two platforms the reference runs on.
fn join(base: &str, name: &str, platform: &str) -> String {
    if platform == "windows" {
        if base.is_empty() {
            return name.to_string();
        }
        return format!("{}\\{}", base.trim_end_matches('\\'), name);
    }
    if base.is_empty() {
        return name.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), name)
}

/// The size knob: a quadratic autofit, an autofit ceiling, or fullscreen.
fn push_size_args(config: &Config, params: &mut Vec<String>) -> Result<(), BuildError> {
    let size = pyops::as_int(&config.size).ok_or(BuildError::Size(config.size.clone()))?;
    if size <= 2 {
        // `int(15 - 2.5 * s + 7.5 * s ** 2)`: the float arithmetic and the
        // truncation toward zero both matter. `int()` of `19.999999999999996`
        // (which is what s=1 gives in IEEE) is 19 in a language that rounds and
        // 19 here, but s=-1 gives 25.0 exactly and s=2 gives 40.0 exactly — and
        // `as_int`-then-`trunc` is the order the reference has it in.
        let percent = (15.0 - 2.5 * size as f64 + 7.5 * (size as f64 * size as f64)).trunc() as i64;
        params.push(format!("--autofit={percent}%"));
    } else if size == 3 {
        params.push("--autofit-larger=90%".to_string());
    } else if size == 4 {
        params.push("--fullscreen".to_string());
    }
    // `size == 5` appends nothing, exactly as the reference's three-arm chain
    // appends nothing: there is no `else`.
    Ok(())
}

/// The hardware knob, which **removes** an option rather than setting one.
///
/// A value `as_int` cannot read is a warning here rather than a [`BuildError`],
/// because Python's `==` never raises: `null == 0` and `null == 2` are both
/// `False`, so the reference simply starts mpv with the `--hwdec=yes` the base
/// list already gave it. The port has to keep that player startable.
fn push_hw_args(config: &Config, params: &mut Vec<String>, warnings: &mut Vec<String>) {
    let Some(hw) = pyops::as_int(&config.hw) else {
        warnings.push(format!(
            "PlayerHW {} is not a number; neither hardware branch runs, so --hwdec=yes stays \
             \u{28}Python: both comparisons are False, and it says nothing\u{29}",
            pyops::py_repr(&config.hw)
        ));
        return;
    };
    if hw == 0 {
        // `params.remove('--hwdec=yes')` — and `list.remove` raises if the value
        // is not in the list. It always is here: the base list is built above and
        // nothing between the two can drop it. Staying faithful means the
        // removal is by value, so an argv with two `--hwdec=yes` (which the
        // darwin block does *not* add, but a future base list might) loses one.
        if let Some(at) = params.iter().position(|p| p == "--hwdec=yes") {
            params.remove(at);
        }
    } else if hw == 2 {
        params.push("--macos-force-dedicated-gpu=yes".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Config {
        Config {
            ipc_socket: "/tmp/macast_mpvsocket7".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn the_default_config_is_the_reference_base_line() {
        let argv = build(&config()).unwrap().argv;
        assert_eq!(
            argv,
            vec![
                "mpv",
                "--input-ipc-server=/tmp/macast_mpvsocket7",
                "--image-display-duration=inf",
                "--idle=yes",
                "--no-terminal",
                "--on-all-workspaces",
                "--hwdec=yes",
                "--save-position-on-quit=yes",
                "--script-opts=osc-timetotal=yes,osc-layout=bottombar,osc-title=${title},\
                 osc-showwindowed=yes,osc-seekbarstyle=bar,osc-visibility=auto",
                "--ontop",
                "--geometry=98%:5%",
                "--autofit=20%",
            ]
        );
        // The OSC string is one argument: splitting it would silently change the
        // player's on-screen controls rather than fail.
        assert_eq!(argv[8].matches(',').count(), 5);
        assert!(build(&config()).unwrap().warnings.is_empty());
    }

    #[test]
    fn ontop_is_truthiness_not_presence() {
        let has_ontop = |value: Value| {
            build(&Config { ontop: value, ..config() })
                .unwrap()
                .argv
                .contains(&"--ontop".to_string())
        };
        // `if ontop:` — so the string "no" turns it *on* and the empty string
        // turns it off. Ugly, and it is what the settings JSON allows a person to
        // type into the advanced-settings editor.
        assert!(has_ontop(json!("no")));
        assert!(has_ontop(json!(1)));
        assert!(has_ontop(json!(true)));
        assert!(!has_ontop(json!("")));
        assert!(!has_ontop(json!(0)));
        assert!(!has_ontop(json!(false)));
    }

    #[test]
    fn negative_position_indexes_from_the_end_like_a_python_list() {
        let geometry = |position: Value| {
            build(&Config { position, ..config() })
                .unwrap()
                .argv
                .iter()
                .find(|a| a.starts_with("--geometry="))
                .unwrap()
                .clone()
        };
        assert_eq!(geometry(json!(-1)), "--geometry=50%:50%");
        assert_eq!(geometry(json!(-5)), "--geometry=2%:5%");
        assert_eq!(geometry(json!(0)), "--geometry=2%:5%");
        assert_eq!(geometry(json!(4)), "--geometry=50%:50%");
    }

    #[test]
    fn a_position_off_the_end_clamps_and_says_so() {
        // Documented divergence: `player_position_data[6]` is an IndexError in
        // Python, which `start_mpv` swallows and which costs the user a player
        // that never starts. Here the run continues and the warning is the record
        // that it should not have.
        let off_the_end = build(&Config { position: json!(6), ..config() }).unwrap();
        assert!(off_the_end.argv.contains(&"--geometry=50%:50%".to_string()));
        assert_eq!(off_the_end.warnings.len(), 1);
        assert!(
            off_the_end.warnings[0].starts_with("PlayerPosition 6 is past the end"),
            "{}",
            off_the_end.warnings[0]
        );

        let before_the_start = build(&Config { position: json!(-6), ..config() }).unwrap();
        assert!(before_the_start.argv.contains(&"--geometry=2%:5%".to_string()));
        assert_eq!(before_the_start.warnings.len(), 1);
        assert!(
            before_the_start.warnings[0].starts_with("PlayerPosition -6 is before the start"),
            "{}",
            before_the_start.warnings[0]
        );
    }

    #[test]
    fn the_size_knob_is_a_quadratic_then_two_exact_matches_then_nothing() {
        let size_arg = |size: Value| {
            build(&Config { size, ..config() })
                .unwrap()
                .argv
                .iter()
                .filter(|a| a.starts_with("--autofit") || **a == "--fullscreen")
                .cloned()
                .collect::<Vec<_>>()
        };
        // Each value CPython gives for `int(15 - 2.5*s + 7.5*s**2)`, including
        // the one below the enum's range: `-1` is not a size anyone can pick from
        // the menu, but it is a number a person can type.
        assert_eq!(size_arg(json!(-1)), ["--autofit=25%"]);
        assert_eq!(size_arg(json!(0)), ["--autofit=15%"]);
        assert_eq!(size_arg(json!(1)), ["--autofit=20%"]);
        assert_eq!(size_arg(json!(2)), ["--autofit=40%"]);
        assert_eq!(size_arg(json!(3)), ["--autofit-larger=90%"]);
        assert_eq!(size_arg(json!(4)), ["--fullscreen"]);
        // `5` matches nothing and is not `<= 2`: the chain has no `else`.
        assert!(size_arg(json!(5)).is_empty());
    }

    #[test]
    fn only_lua_files_become_scripts_and_the_given_order_is_kept() {
        let build = build(&Config {
            scripts_dir: "/Users/x/Macast/scripts".to_string(),
            scripts: vec![
                "z_last.lua".to_string(),
                "notes.txt".to_string(),
                "a_first.lua".to_string(),
                "reload.LUA".to_string(),
            ],
            ..config()
        })
        .unwrap();
        // Unsorted, because `os.listdir` is unsorted and the reference does not
        // sort: an argv comparison against a real build has to match a real
        // build's order, not a tidied one. And the suffix test is case-sensitive
        // — `s.endswith('.lua')` — so `.LUA` is not a script.
        assert_eq!(
            build
                .argv
                .iter()
                .filter(|a| a.starts_with("--script="))
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "--script=/Users/x/Macast/scripts/z_last.lua",
                "--script=/Users/x/Macast/scripts/a_first.lua",
            ]
        );
        // Scripts land after `--geometry` and before the size block, which is
        // where the reference builds them; mpv does not care and a plugin
        // overriding `build_mpv_params` does.
        let at = |needle: &str| {
            build
                .argv
                .iter()
                .position(|a| a.starts_with(needle))
                .unwrap()
        };
        assert!(at("--geometry=") < at("--script="));
        assert!(at("--script=") < at("--autofit="));
    }

    #[test]
    fn windows_joins_script_paths_with_a_backslash() {
        let build = build(&Config {
            platform: "windows".to_string(),
            scripts_dir: r"C:\Users\x\AppData\Local\Macast\scripts".to_string(),
            scripts: vec!["one.lua".to_string()],
            ..config()
        })
        .unwrap();
        assert!(
            build
                .argv
                .contains(&r"--script=C:\Users\x\AppData\Local\Macast\scripts\one.lua".to_string()),
            "{:?}",
            build.argv
        );
        // And the darwin block is *not* there, because `sys.platform` is not
        // this machine's — it is the field, so a Linux runner tests it too.
        assert!(!build.argv.contains(&"--ontop-level=system".to_string()));
    }

    #[test]
    fn darwin_adds_three_options_including_a_second_on_all_workspaces() {
        let build = build(&Config {
            platform: "darwin".to_string(),
            ..config()
        })
        .unwrap();
        let occurrences = build.argv.iter().filter(|a| **a == "--on-all-workspaces").count();
        assert_eq!(occurrences, 2, "the reference's duplicate is part of its argv: {:?}", build.argv);
        let at = |needle: &str| build.argv.iter().position(|a| a.starts_with(needle)).unwrap();
        assert!(at("--autofit=") < at("--ontop-level=system"));
        assert!(at("--ontop-level=system") < at("--macos-app-activation-policy="));
    }

    #[test]
    fn hardware_takes_the_flag_away_or_asks_for_the_dedicated_gpu() {
        let argv = |hw: Value| build(&Config { hw, ..config() }).unwrap().argv;
        assert!(!argv(json!(0)).contains(&"--hwdec=yes".to_string()));
        assert!(argv(json!(1)).contains(&"--hwdec=yes".to_string()));
        assert!(argv(json!(2)).contains(&"--macos-force-dedicated-gpu=yes".to_string()));
        // `3` is neither branch: `--hwdec=yes` stays and nothing is appended.
        let extra = argv(json!(3));
        assert!(extra.contains(&"--hwdec=yes".to_string()));
        assert!(!extra.contains(&"--macos-force-dedicated-gpu=yes".to_string()));
    }

    #[test]
    fn a_knob_that_is_not_a_number_names_the_key_and_says_what_python_did() {
        for (position, expect) in [
            (json!("Center"), "PlayerPosition is not an integer: 'Center'"),
            (json!([2]), "PlayerPosition is not an integer: [2]"),
        ] {
            let err = build(&Config { position, ..config() }).unwrap_err();
            assert_eq!(err.key(), "PlayerPosition");
            assert!(err.message().starts_with(expect), "{}", err.message());
            assert!(err.message().contains("list indices must be integers"), "{}", err.message());
        }
        let err = build(&Config { size: json!("Large"), ..config() }).unwrap_err();
        assert_eq!(err.key(), "PlayerSize");
        assert!(err.message().contains("not supported between instances of 'str' and 'int'"), "{}", err.message());
        // Hardware is the third knob, and it is **not** in this list: `null == 0`
        // and `null == 2` are both False in Python, so the reference starts mpv
        // with the `--hwdec=yes` the base list already gave it. The port owes that
        // case a line, not a dead player.
        let built = build(&Config { hw: json!(null), ..config() }).unwrap();
        assert!(built.argv.contains(&"--hwdec=yes".to_string()));
        assert_eq!(built.warnings.len(), 1, "{:?}", built.warnings);
        assert!(
            built.warnings[0].starts_with("PlayerHW None is not a number"),
            "{}",
            built.warnings[0]
        );
        assert!(
            built.warnings[0].contains("both comparisons are False"),
            "{}",
            built.warnings[0]
        );
        // A string a person typed for a number is the documented divergence, and
        // the direction matters: Python's `"0" == 0` is False, so the **reference
        // keeps** `--hwdec=yes`; `int("0")` is 0, so this port drops the flag —
        // which is what the person who typed `"0"` meant. See `pyops::as_int`.
        let built = build(&Config { hw: json!("0"), ..config() }).unwrap();
        assert!(!built.argv.contains(&"--hwdec=yes".to_string()));
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
    }
}
