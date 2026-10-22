//! Handing the media url to the user's own player (VLC / MPC / mpv.net).
//!
//! Rust counterpart of `macast/plugins/renderer/external_player.py`'s decision layer
//! (`:60-134`): which players exist, which one is configured, and what argv to launch.
//!
//! ## The one rule that matters
//!
//! **The player is launched with an argv list, never a command string.** The url comes off
//! the network, so a string command would let it append switches of its own — `; rm -rf`,
//! or simply `--fullscreen` on a player that takes trailing options. The table therefore
//! models each player as `executable + args(url)`, and there is no code path here that
//! concatenates anything into a shell.
//!
//! ## Why the lookup is two-step and platform-tagged
//!
//! `PATH` first, then the usual install locations, because a GUI app started from Finder
//! or the Dock does not inherit the shell's `PATH` — a Homebrew or `Program Files` install
//! would look missing otherwise. The locations are keyed by platform and the platform is an
//! **input**, not `std::env::consts::OS`: the same reason `macast_core::paths::sys_platform`
//! exists (a test on Linux asking about `win32` must get `win32`'s answer, AGENTS.md §4.8's
//! Part 40 row).
//!
//! ## What is deliberately not here
//!
//! No process spawning and no filesystem. Those are seams the caller supplies, so the tests
//! can be deterministic and a machine with VLC installed does not change any assertion.

/// One player: how to find it and how to call it.
#[derive(Clone, Debug)]
pub struct Player {
    /// The menu label, which is also the value stored in the settings file.
    pub name: &'static str,
    /// Executable names to look for on `PATH`.
    pub names: &'static [&'static str],
    /// Install locations per platform, checked when `PATH` has nothing.
    pub paths: &'static [(&'static str, &'static [&'static str])],
    /// How the url becomes arguments. Every player here takes it last.
    pub urls_as: UrlStyle,
}

/// The two argument shapes in the table. A closure would be more general, but these are the
/// only two, and naming them keeps the table a `const`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlStyle {
    /// The url and nothing else (VLC, mpv.net).
    LastArgument,
    /// A leading verb pair, then the url (MPC: `/play /close <url>`).
    PlayAndClose,
}

impl UrlStyle {
    pub fn args(self, url: &str) -> Vec<String> {
        match self {
            UrlStyle::LastArgument => vec![url.to_string()],
            UrlStyle::PlayAndClose => vec![
                "/play".to_string(),
                "/close".to_string(),
                url.to_string(),
            ],
        }
    }
}

/// The player table, in menu order.
pub const PLAYERS: &[Player] = &[
    Player {
        name: "VLC",
        names: &["vlc"],
        paths: &[
            (
                "darwin",
                &["/Applications/VLC.app/Contents/MacOS/VLC"],
            ),
            (
                "win32",
                &[
                    r"C:\Program Files\VideoLAN\VLC\vlc.exe",
                    r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
                ],
            ),
            ("linux", &["/usr/bin/vlc", "/snap/bin/vlc"]),
        ],
        urls_as: UrlStyle::LastArgument,
    },
    Player {
        name: "MPC-BE / MPC-HC",
        names: &["mpc-be64", "mpc-be", "mpc-hc64", "mpc-hc"],
        paths: &[(
            "win32",
            &[
                r"C:\Program Files\MPC-BE x64\mpc-be64.exe",
                r"C:\Program Files\MPC-BE\mpc-be.exe",
                r"C:\Program Files\MPC-HC\mpc-hc64.exe",
            ],
        )],
        urls_as: UrlStyle::PlayAndClose,
    },
    Player {
        name: "mpv.net",
        names: &["mpvnet"],
        paths: &[
            ("win32", &[r"C:\Program Files\mpv.net\mpvnet.exe"]),
            (
                "darwin",
                &["/Applications/mpv.net.app/Contents/MacOS/mpvnet"],
            ),
        ],
        urls_as: UrlStyle::LastArgument,
    },
];

/// Look a player up by its stored name.
pub fn player_named(name: &str) -> Option<&'static Player> {
    PLAYERS.iter().find(|player| player.name == name)
}

/// The settings keys this renderer reads.
pub const KEY_PLAYER: &str = "External_Player";
pub const KEY_PLAYER_PATH: &str = "External_Player_Path";

/// Everything that touches the machine, supplied by the caller.
///
/// Kept as a trait object rather than a closure triple so a test reads as "this machine has
/// vlc at /usr/bin/vlc", instead of three opaque closures.
pub trait Host {
    /// Equivalent of `shutil.which`: absolute path if the name is executable on `PATH`.
    fn which(&self, name: &str) -> Option<String>;
    /// Is this an executable file?
    fn is_executable(&self, path: &str) -> bool;
    /// The platform whose install locations to consult: `darwin`, `win32`, `linux`.
    fn platform(&self) -> &str;
}

/// Find an installed player's absolute path.
///
/// `PATH` first, then install locations. The order is the reference's and it matters: an
/// explicit `PATH` entry is the user's own choice of which VLC, while the fallback list is
/// our guess about where a GUI install lives.
pub fn find_player(player: &Player, host: &dyn Host) -> Option<String> {
    for name in player.names {
        if let Some(found) = host.which(name) {
            return Some(found);
        }
    }
    let platform = host.platform();
    for (name, candidates) in player.paths {
        if *name != platform {
            continue;
        }
        for candidate in *candidates {
            if host.is_executable(candidate) {
                return Some((*candidate).to_string());
            }
        }
    }
    None
}

/// `[(name, path)]` for everything launchable, in table order.
pub fn installed_players(host: &dyn Host) -> Vec<(&'static str, String)> {
    PLAYERS
        .iter()
        .filter_map(|player| find_player(player, host).map(|path| (player.name, path)))
        .collect()
}

/// What resolution decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// The player's menu name — always one from the table, so the caller can render it.
    pub name: &'static str,
    /// `None` when the player is not installed; the caller turns that into a message.
    pub path: Option<String>,
}

/// Resolve the configured player, falling back instead of raising.
///
/// The name comes from a settings file the user can edit by hand, so an unknown value must
/// not raise inside an HTTP worker: it falls back to the first *installed* player, then to
/// the first entry in the table (so the error message can still name something real).
pub fn resolve_player(
    configured: &str,
    override_path: &str,
    host: &dyn Host,
) -> Chosen {
    let name = match player_named(configured) {
        Some(player) => player.name,
        None => {
            let available = installed_players(host);
            match available.first() {
                Some((name, _)) => name,
                None => PLAYERS[0].name,
            }
        }
    };
    // An explicit path wins over detection, but only when it is really executable — a stale
    // path in the settings file must not shadow a working install.
    if !override_path.is_empty() && host.is_executable(override_path) {
        return Chosen {
            name,
            path: Some(override_path.to_string()),
        };
    }
    let player = player_named(name).expect("the name came from the table");
    Chosen {
        name,
        path: find_player(player, host),
    }
}

/// The argv to launch, or `None` when the player is missing.
///
/// No shell, no string joining: the executable is element zero and the url is one element of
/// its own. That is the whole safety property of this module.
pub fn launch_argv(chosen: &Chosen, url: &str) -> Option<Vec<String>> {
    let path = chosen.path.as_ref()?;
    let player = player_named(chosen.name)?;
    let mut argv = vec![path.clone()];
    argv.extend(player.urls_as.args(url));
    Some(argv)
}

/// The message when the configured player is not installed.
///
/// Names both ways out, because "not found" alone leaves the user with a working app that
/// silently does nothing.
pub fn missing_message(name: &str) -> String {
    format!(
        "未找到外部播放器「{name}」：请在菜单里换一个，或在设置里填 {KEY_PLAYER_PATH}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A machine described in one line, so no assertion depends on what is installed here.
    struct Machine {
        on_path: BTreeMap<String, String>,
        executables: Vec<String>,
        platform: String,
    }

    impl Machine {
        fn new(platform: impl Into<String>) -> Machine {
            Machine {
                on_path: BTreeMap::new(),
                executables: Vec::new(),
                platform: platform.into(),
            }
        }

        fn with_path(mut self, name: impl Into<String>, path: impl Into<String>) -> Machine {
            self.on_path.insert(name.into(), path.into());
            self
        }

        fn with_executable(mut self, path: impl Into<String>) -> Machine {
            self.executables.push(path.into());
            self
        }
    }

    impl Host for Machine {
        fn which(&self, name: &str) -> Option<String> {
            self.on_path.get(name).cloned()
        }

        fn is_executable(&self, path: &str) -> bool {
            self.executables.iter().any(|p| p == path)
        }

        fn platform(&self) -> &str {
            &self.platform
        }
    }

    #[test]
    fn the_url_is_one_argument_and_never_reaches_a_shell() {
        // The safety property: an url full of shell metacharacters is inert because it is a
        // single element of an argv vector.
        let machine = Machine::new("linux").with_path("vlc", "/usr/bin/vlc");
        let chosen = resolve_player("VLC", "", &machine);
        let hostile = "http://x/a.mp4; rm -rf / --no-quit ' \" $(id)";
        let argv = launch_argv(&chosen, hostile).expect("vlc is installed");
        assert_eq!(argv, vec!["/usr/bin/vlc".to_string(), hostile.to_string()]);
        // …and no element contains a separator joining it to something else.
        assert_eq!(argv.len(), 2);
    }

    #[test]
    fn mpc_gets_its_verb_pair_before_the_url() {
        let machine = Machine::new("win32").with_path(
            "mpc-be64",
            r"C:\Program Files\MPC-BE x64\mpc-be64.exe",
        );
        let chosen = resolve_player("MPC-BE / MPC-HC", "", &machine);
        let argv = launch_argv(&chosen, "http://host/f.mkv").expect("mpc is installed");
        assert_eq!(
            argv,
            vec![
                r"C:\Program Files\MPC-BE x64\mpc-be64.exe".to_string(),
                "/play".to_string(),
                "/close".to_string(),
                "http://host/f.mkv".to_string(),
            ]
        );
    }

    #[test]
    fn path_is_searched_before_the_install_locations() {
        // A user with a `PATH` vlc chose that one; the fallback list is our guess.
        let machine = Machine::new("darwin")
            .with_path("vlc", "/opt/homebrew/bin/vlc")
            .with_executable("/Applications/VLC.app/Contents/MacOS/VLC");
        let vlc = player_named("VLC").expect("table has VLC");
        assert_eq!(
            find_player(vlc, &machine).as_deref(),
            Some("/opt/homebrew/bin/vlc")
        );
    }

    #[test]
    fn a_gui_install_is_found_when_path_is_empty() {
        // The reason the fallback list exists at all: a Dock-launched app has no shell PATH.
        let machine =
            Machine::new("darwin").with_executable("/Applications/VLC.app/Contents/MacOS/VLC");
        let vlc = player_named("VLC").expect("table has VLC");
        assert_eq!(
            find_player(vlc, &machine).as_deref(),
            Some("/Applications/VLC.app/Contents/MacOS/VLC")
        );
    }

    #[test]
    fn the_platform_is_an_input_so_a_linux_runner_can_ask_about_windows() {
        // AGENTS.md §4.8 Part 40: a decision that reads `sys.platform` answers about the
        // machine running the tests. Here the answer must follow the argument.
        let windows_only =
            Machine::new("win32").with_executable(r"C:\Program Files\mpv.net\mpvnet.exe");
        let mpvnet = player_named("mpv.net").expect("table has mpv.net");
        assert!(find_player(mpvnet, &windows_only).is_some());

        let same_paths_on_linux = Machine::new("linux")
            .with_executable(r"C:\Program Files\mpv.net\mpvnet.exe");
        assert!(
            find_player(mpvnet, &same_paths_on_linux).is_none(),
            "a win32 install location must not be consulted when the platform is linux"
        );
    }

    #[test]
    fn an_unknown_configured_name_falls_back_to_an_installed_player() {
        let machine = Machine::new("linux").with_path("mpvnet", "/usr/bin/mpvnet");
        let chosen = resolve_player("VLC (typo)", "", &machine);
        // VLC is first in the table but not installed, so the fallback is the one that is.
        assert_eq!(chosen.name, "mpv.net");
        assert_eq!(chosen.path.as_deref(), Some("/usr/bin/mpvnet"));
    }

    #[test]
    fn with_nothing_installed_the_name_still_comes_from_the_table() {
        // So the error message can say which player to install, rather than "none".
        let machine = Machine::new("linux");
        let chosen = resolve_player("", "", &machine);
        assert_eq!(chosen.name, "VLC");
        assert_eq!(chosen.path, None);
        assert!(missing_message(chosen.name).contains("VLC"));
    }

    #[test]
    fn an_explicit_path_wins_only_when_it_is_really_executable() {
        let machine = Machine::new("linux")
            .with_path("vlc", "/usr/bin/vlc")
            .with_executable("/opt/mine/vlc");
        let chosen = resolve_player("VLC", "/opt/mine/vlc", &machine);
        assert_eq!(chosen.path.as_deref(), Some("/opt/mine/vlc"));

        // A stale path in the settings file must not shadow a working install.
        let chosen = resolve_player("VLC", "/gone/vlc", &machine);
        assert_eq!(chosen.path.as_deref(), Some("/usr/bin/vlc"));
    }

    #[test]
    fn a_missing_player_yields_no_argv_and_a_message_naming_the_key() {
        let machine = Machine::new("linux");
        let chosen = resolve_player("VLC", "", &machine);
        assert!(launch_argv(&chosen, "http://x").is_none());
        let message = missing_message(chosen.name);
        assert!(message.contains("VLC"));
        assert!(
            message.contains(KEY_PLAYER_PATH),
            "the message must name the setting to fill in: {message}"
        );
    }

    /// Every resolution and argv the reference produced, replayed against this port.
    ///
    /// Produced by executing `external_player.installed_players()` / `resolve_player()` /
    /// the per-player `args` lambda over seven synthetic machines × seven configurations.
    /// The machines are synthetic on purpose: a real VLC install must not change an assertion.
    #[test]
    fn every_reference_resolution_replays_identically() {
        let corpus = include_str!("../tests/golden/external_player.jsonl");
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            // Rebuild the machine the row was produced on: `installed` is what
            // `installed_players()` saw, so it is the exact fingerprint of that machine.
            // The corpus carries the platform, because deriving it from the machine's *name*
            // made `win_mpvnet_on_linux` impossible to express (it is a linux machine whose
            // only install location is a Windows path).
            let platform = row["platform"].as_str().unwrap_or("linux");
            let installed: Vec<(String, String)> = row["installed"]
                .as_array()
                .expect("installed")
                .iter()
                .map(|pair| {
                    let pair = pair.as_array().expect("pair");
                    (
                        pair[0].as_str().unwrap_or("").to_string(),
                        pair[1].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            // Feed those back in as PATH hits for the *name* each player answers to, which is
            // enough to reproduce the decision without modelling `shutil.which`.
            let mut machine = Machine::new(platform);
            for (name, path) in &installed {
                if let Some(player) = player_named(name) {
                    machine = machine.with_path(player.names[0], path);
                }
            }
            // The override's executability is a fact about the machine, so the corpus carries
            // it rather than this replay assuming it — assuming made the `/opt/mine/vlc` row
            // disagree, because on that synthetic machine the override was *not* executable
            // and the reference correctly fell back to the PATH hit.
            let override_path = row["override"].as_str().unwrap_or("");
            if row["override_executable"].as_bool().unwrap_or(false) {
                machine = machine.with_executable(override_path);
            }

            let chosen = resolve_player(
                row["config"].as_str().unwrap_or(""),
                override_path,
                &machine,
            );
            assert_eq!(
                chosen.name,
                row["name"].as_str().unwrap_or(""),
                "row {line} resolved to a different player"
            );
            assert_eq!(
                chosen.path.as_deref(),
                row["path"].as_str(),
                "row {line} resolved to a different path"
            );

            let argv = launch_argv(&chosen, "http://x/a.mp4");
            let expected: Option<Vec<String>> = row["argv"].as_array().map(|args| {
                args.iter()
                    .map(|a| a.as_str().unwrap_or("").to_string())
                    .collect()
            });
            assert_eq!(argv, expected, "row {line} produced a different argv");
            rows += 1;
        }
        assert_eq!(rows, 49, "the corpus lost rows");
    }

    #[test]
    fn the_table_order_is_the_menu_order() {
        // The fallback picks the first *installed* entry, so table order is user-visible.
        let names: Vec<&str> = PLAYERS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["VLC", "MPC-BE / MPC-HC", "mpv.net"]);
        // Names must be unique, or `player_named` silently answers with the first.
        for (index, name) in names.iter().enumerate() {
            assert!(
                !names[index + 1..].contains(name),
                "duplicate player name {name}"
            );
        }
    }
}
