//! The commands the renderer sends, and where the player's socket lives.
//!
//! Everything here returns a `Vec<Value>` — the JSON array as mpv's IPC defines
//! it, one element per positional argument — rather than a string. That choice
//! is the whole module: the wire format is `json.dumps({"command": [...]})`, so
//! a command built as a string would have to be re-parsed to be tested, and the
//! two documented failures this crate carries are both about *element position*,
//! not about text.
//!
//! The reference is `macast_renderer/mpv.py`.

use serde_json::{json, Map, Value};

/// The three answers to "is the media currently in the player a live stream".
///
/// `Undeclared` is **not a shy `false`**. AGENTS.md §4.1 records why: the
/// declaration comes from the sender's own fields (`DLNA.ORG_OP` for DLNA,
/// `media.streamType` for Cast), and a sender we do not know says nothing.
/// "Says nothing" must keep mpv's default read-ahead, because an ordinary video
/// with a declared end is the case that buffering is *for*, and treating silence
/// as "it has an end" would take the read-ahead away from every app that never
/// heard of this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The sender declared this media live: no timeline to seek into.
    Live,
    /// The sender declared this media has an end.
    Finite,
    /// Nobody said. Keep today's behaviour.
    Undeclared,
}

impl Liveness {
    /// `MPVRenderer.note_media_live`: the three answers are stored as given.
    pub fn from_declaration(declared: Option<bool>) -> Self {
        match declared {
            Some(true) => Liveness::Live,
            Some(false) => Liveness::Finite,
            None => Liveness::Undeclared,
        }
    }

    /// The value the guard hands over: `Some(true)`/`Some(false)`/`None`.
    pub fn to_declaration(self) -> Option<bool> {
        match self {
            Liveness::Live => Some(true),
            Liveness::Finite => Some(false),
            Liveness::Undeclared => None,
        }
    }
}

/// mpv per-file options for a stream the *sender* declared live.
///
/// Copied key-for-key and **in the reference's order** from `LIVE_LOAD_OPTIONS`.
/// The order is not decoration: this is a Python dict, so its insertion order is
/// the order the keys reach mpv in, and `serde_json`'s `Map` only keeps it
/// because the workspace turns on `preserve_order`. A `BTreeMap`-backed map
/// would silently re-sort it and the argv/option dump in a bug report would stop
/// matching the reference build's.
///
/// The values are strings because that is the shape that was measured on the
/// machine that produced the latency numbers behind them (AGENTS.md §4.2, "the
/// `loadfile` fifth slot"). `demuxer-lavf-o-add` — not `demuxer-lavf-o` — is the
/// one that *appends* `fflags=+nobuffer` to the demuxer's own option list instead
/// of replacing the list.
pub fn live_load_options() -> Map<String, Value> {
    let mut options = Map::new();
    options.insert("audio-buffer".into(), json!("0"));
    options.insert("vd-lavc-threads".into(), json!("1"));
    options.insert("cache-pause".into(), json!("no"));
    options.insert("demuxer-lavf-o-add".into(), json!("fflags=+nobuffer"));
    options.insert("demuxer-lavf-probe-info".into(), json!("nostreams"));
    options.insert("demuxer-lavf-analyzeduration".into(), json!("0.1"));
    options.insert("video-sync".into(), json!("audio"));
    options.insert("interpolation".into(), json!("no"));
    options.insert("video-latency-hacks".into(), json!("yes"));
    options.insert("stream-buffer-size".into(), json!("4k"));
    options
}

/// The one `loadfile` command this renderer ever sends.
///
/// mpv's IPC signature is `loadfile <url> [<flags> [<index> [<options>]]]`.
/// **The fourth slot is the playlist index and the fifth is the option
/// dictionary.** Writing `start=00:00:12` into the fourth is an invalid
/// parameter: measured on this machine, mpv answers
/// `{"error": "invalid parameter"}` and loads nothing, which on a `reload()`
/// looks like "the player stopped responding" rather than like our own typo.
///
/// Three shapes, and the bare one is deliberate:
///
/// - no options at all → `["loadfile", url, "replace"]`, three elements exactly
///   as this file emitted them before per-file options existed. That is what
///   keeps an ordinary video playing with mpv's read-ahead, and it is the shape
///   a no-op case promises;
/// - declared live → five elements carrying [`live_load_options`];
/// - a start position on media nobody declared live → five elements carrying
///   `{"start": ...}`.
///
/// `start` is honoured **only when the media is not declared live**: a live
/// declaration means there is no timeline to jump into, and sending a position
/// into that is precisely the seek the sender asked us not to make.
///
/// `"0"` is not sent. mpv already starts a file at 0, so shipping it as an option
/// would take the load out of the bare shape for no change in behaviour — and the
/// bare shape is the one that carries mpv's defaults.
pub fn loadfile(url: &str, start: Option<&str>, live: Liveness) -> Vec<Value> {
    let mut options = Map::new();
    if live == Liveness::Live {
        options = live_load_options();
    } else {
        let position = start.unwrap_or("").trim();
        if !position.is_empty() && position != "0" {
            options.insert("start".into(), json!(position));
        }
    }
    if options.is_empty() {
        return vec![json!("loadfile"), json!(url), json!("replace")];
    }
    vec![
        json!("loadfile"),
        json!(url),
        json!("replace"),
        json!(0),
        Value::Object(options),
    ]
}

/// `set_property <name> <value>` with the value type the reference used.
///
/// The value is a `Value`, not a `&str`, because mpv's own option types differ
/// and the reference sends them differently on purpose: `pause` is a real JSON
/// boolean, while `mute` and `sub-visibility` are the **strings** `"yes"`/`"no"`.
/// Flattening those into one helper that always stringifies would still work
/// against mpv and would still break the log lines a user greps.
pub fn set_property(name: &str, value: Value) -> Vec<Value> {
    vec![json!("set_property"), json!(name), value]
}

/// The window-title pair. **Two commands, and the second one is the fix.**
///
/// `title` alone only reaches the OSC overlay; the window itself is named by
/// `${media-title}`, which falls back to `filename` — and for a
/// `/relay/<id>/media` URL the last path segment is literally `media`. So
/// `media - mpv` was never a bug in the title code: it is mpv reading the
/// filename because nobody forced the property. AGENTS.md §4.2 has the whole
/// entry ("change the address the player plays, and you must change its name").
///
/// `--title` stays the user's option; we only ever set the property.
pub fn title(text: &str) -> Vec<Vec<Value>> {
    vec![
        set_property("title", json!(text)),
        set_property("force-media-title", json!(text)),
    ]
}

/// `set_media_volume`: an integer 0–100, sent as a number.
pub fn volume(data: Value) -> Vec<Value> {
    set_property("volume", data)
}

/// `set_media_mute`: the *string* form, because that is what the reference sends.
pub fn mute(muted: bool) -> Vec<Value> {
    set_property("mute", json!(if muted { "yes" } else { "no" }))
}

/// `set_media_speed`.
pub fn speed(data: Value) -> Vec<Value> {
    set_property("speed", data)
}

/// `set_media_pause` / `set_media_resume` — booleans, not `"yes"`/`"no"`.
pub fn pause(p: bool) -> Vec<Value> {
    set_property("pause", json!(p))
}

/// Fullscreen is a `set_property`, **not** a per-file option on `loadfile`.
///
/// It is a player-wide setting the user chose, not a statement about this one
/// stream, and it must survive an item that arrives with no declaration at all —
/// per-file options reset on the next bare load, which is the point of them.
pub fn fullscreen() -> Vec<Value> {
    set_property("fullscreen", json!("yes"))
}

/// `set_media_position`: an absolute seek, `H:MM:SS` text.
pub fn seek(position: &str) -> Vec<Value> {
    vec![json!("seek"), json!(position), json!("absolute")]
}

/// `set_media_sub_file`: mpv's `sub-add <url> select <title>`.
pub fn sub_add(url: &str, title: &str) -> Vec<Value> {
    vec![json!("sub-add"), json!(url), json!("select"), json!(title)]
}

/// `set_media_sub_show`: subtitle visibility, again the `"yes"`/`"no"` strings.
pub fn sub_show(visible: bool) -> Vec<Value> {
    set_property("sub-visibility", json!(if visible { "yes" } else { "no" }))
}

/// `set_media_text`: the OSC overlay, with the reference's 1000 ms default.
pub fn show_text(text: &str, duration_ms: i64) -> Vec<Value> {
    vec![json!("show-text"), json!(text), json!(duration_ms)]
}

/// The properties mpv is asked to report, as `(id, name)` in the reference's
/// order.
///
/// The ids are the `ObserveProperty` enum values and they are **load-bearing
/// numbers, not labels**: they are echoed back in every reply's `id` field, so a
/// renumbering silently reassigns every state update in [`crate::state`] to the
/// wrong property. The order is the order `set_observe` sends them.
pub const OBSERVED: [(&str, u8); 8] = [
    ("volume", 1),
    ("time-pos", 2),
    ("pause", 3),
    ("mute", 4),
    ("duration", 5),
    ("track-list", 6),
    ("speed", 7),
    ("sub-visibility", 8),
];

/// `observe_property <id> <name>` for all eight, in order.
pub fn observe_all() -> Vec<Vec<Value>> {
    OBSERVED
        .iter()
        .map(|(name, id)| vec![json!("observe_property"), json!(*id), json!(name)])
        .collect()
}

/// The id a reply's `id` field refers to, or `None` for an id we never registered.
///
/// Python reaches this the other way round — a chain of `elif res['id'] == …`
/// that silently does nothing for an unknown id — so the behaviour for an
/// unrecognised id is "no state update at all", which is what `None` means
/// upstream in [`crate::state`].
pub fn observed_name(id: u64) -> Option<&'static str> {
    OBSERVED.iter().find(|(_, n)| *n as u64 == id).map(|(name, _)| *name)
}

/// The full path mpv is told to listen on (`--input-ipc-server=<path>`).
///
/// The random suffix is in the reference (`random.randint(0, 9999)`) and it is
/// not cosmetic: a machine that has accumulated orphaned mpv processes — which
/// AGENTS.md §4.2 says this one has, because a SIGINT-killed menu bar app skips
/// `MPVRenderer.stop()`'s cleanup — would otherwise have a second renderer
/// connecting to the *first* player's socket and both of them sending commands to
/// whichever mpv got there first. So the number is a caller-supplied parameter
/// rather than something read from a global: one instance, one socket, and a test
/// can ask what the shape is without rolling a die.
///
/// On Windows the "path" is a named pipe, and it must go through
/// [`macast_core::paths::base_path`]-style handling to be useful there; the
/// string form is reproduced exactly because that is what the argv carries.
pub fn ipc_socket_path(platform: &str, random: u32) -> String {
    if platform == "windows" {
        format!(r"\\.\pipe\macast_mpvsocket{random}")
    } else {
        format!("/tmp/macast_mpvsocket{random}")
    }
}

/// A random suffix in the reference's range, `0..=9999`.
///
/// `random.randint` is inclusive of both ends, so this is `% 10000` and not
/// `% 9999`. Uses the CSPRNG from `macast-core` rather than a `Random` trait:
/// nothing here is secret, but the crate already has one randomness seam and a
/// second one is a second thing to audit.
pub fn socket_random() -> u32 {
    let bytes = macast_core::token::random_bytes(4)
        .map(|b| [b[0], b[1], b[2], b[3]])
        .unwrap_or([0, 0, 0, 7]);
    (u32::from_be_bytes(bytes) % 10_000) as u32
}

/// The wire bytes for one command: `json.dumps({"command": [...]}) + "\n"`.
///
/// **Documented divergence, one byte-level detail**: Python's `json.dumps` puts
/// a space after every `,` and `:`; `serde_json` does not. mpv parses both — it
/// takes a JSON object, not a prefix — so the difference is invisible to the
/// player. The *log* line is a different matter, and there we do reproduce
/// Python's spacing verbatim, because it is what the reference build's logs look
/// like and what anyone diffing two machines is comparing: see
/// [`crate::pyops::py_str`] and [`logged_command`].
pub fn encode(command: &[Value]) -> String {
    let mut body = Map::new();
    body.insert("command".into(), Value::Array(command.to_vec()));
    let mut line = serde_json::to_string(&Value::Object(body))
        .unwrap_or_else(|_| r#"{"command":[]}"#.to_string());
    line.push('\n');
    line
}

/// What `logger.debug("send command: " + str(command))` writes for this command.
///
/// Not the JSON we send — the Python `str()` of the list, quotes and `True` and
/// all, which is the text a user greps for when a cast goes wrong.
pub fn logged_command(command: &[Value]) -> String {
    crate::pyops::py_str(&Value::Array(command.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_declaration_sends_the_bare_three_element_load() {
        // The shape that predates per-file options, and the one that keeps mpv's
        // read-ahead for an ordinary video.
        assert_eq!(
            loadfile("http://a/b.mp4", None, Liveness::Undeclared),
            vec![json!("loadfile"), json!("http://a/b.mp4"), json!("replace")]
        );
        // And the reference's own default argument, `start="0"`, is not an option.
        assert_eq!(
            loadfile("http://a/b.mp4", Some("0"), Liveness::Undeclared),
            vec![json!("loadfile"), json!("http://a/b.mp4"), json!("replace")]
        );
        assert_eq!(
            loadfile("http://a/b.mp4", Some("  "), Liveness::Undeclared),
            vec![json!("loadfile"), json!("http://a/b.mp4"), json!("replace")]
        );
    }

    #[test]
    fn a_position_is_an_option_in_the_fifth_slot_not_an_argument_in_the_fourth() {
        let cmd = loadfile("http://a/b.mkv", Some("00:00:12"), Liveness::Finite);
        assert_eq!(cmd.len(), 5, "{cmd:?}");
        assert_eq!(cmd[3], json!(0), "slot four is the playlist index");
        assert_eq!(cmd[4], json!({"start": "00:00:12"}));
        // `start` is a *string*, exactly as the reference passes it through: mpv
        // parses relative, absolute and percentage forms out of the text.
        assert_eq!(
            loadfile("http://a/b.mkv", Some("50%"), Liveness::Finite)[4],
            json!({"start": "50%"})
        );
    }

    #[test]
    fn a_live_declaration_wins_over_a_requested_position() {
        // The seek is the thing the declaration forbids: there is no addressable
        // prefix on a live stream, so asking for one is how you get a frozen
        // player rather than a jump.
        let cmd = loadfile("http://a/live", Some("00:01:00"), Liveness::Live);
        let Value::Object(options) = &cmd[4] else {
            panic!("live load must carry an option dictionary: {cmd:?}")
        };
        assert!(!options.contains_key("start"), "{options:?}");
        assert_eq!(options.len(), 10, "{options:?}");
    }

    #[test]
    fn live_options_are_the_measured_ten_keys_in_the_dicts_order() {
        let options = live_load_options();
        let keys: Vec<&str> = options.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "audio-buffer",
                "vd-lavc-threads",
                "cache-pause",
                "demuxer-lavf-o-add",
                "demuxer-lavf-probe-info",
                "demuxer-lavf-analyzeduration",
                "video-sync",
                "interpolation",
                "video-latency-hacks",
                "stream-buffer-size",
            ]
        );
        // All strings: that is the shape that was measured. In particular
        // `cache-pause` is "no", not false — mpv's own bool spellings differ from
        // JSON's, and the reference passed it as text.
        for (k, v) in live_load_options() {
            assert!(v.is_string(), "{k} must be a string, got {v}");
        }
        assert_eq!(live_load_options()["demuxer-lavf-o-add"], json!("fflags=+nobuffer"));
    }

    #[test]
    fn the_three_declarations_are_not_two() {
        assert_eq!(Liveness::from_declaration(None), Liveness::Undeclared);
        assert_eq!(Liveness::from_declaration(Some(false)), Liveness::Finite);
        assert_eq!(Liveness::from_declaration(Some(true)), Liveness::Live);
        // Round-trips, because the guard stores the declaration it was handed and
        // the player reads it back.
        for declared in [Some(true), Some(false), None] {
            assert_eq!(Liveness::from_declaration(declared).to_declaration(), declared);
        }
        // And `Undeclared` behaves like "not live" for the *position*, while
        // `Finite` does too: only `Live` suppresses it.
        assert_eq!(
            loadfile("u", Some("12"), Liveness::Undeclared)[4],
            loadfile("u", Some("12"), Liveness::Finite)[4]
        );
    }

    #[test]
    fn title_is_two_commands_and_the_second_is_the_window() {
        let cmds = title("Never Gonna Give You Up");
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], vec![json!("set_property"), json!("title"), json!("Never Gonna Give You Up")]);
        assert_eq!(cmds[1][1], json!("force-media-title"));
    }

    #[test]
    fn booleans_are_bools_for_pause_and_strings_for_mute_and_subs() {
        assert_eq!(pause(true), vec![json!("set_property"), json!("pause"), json!(true)]);
        assert_eq!(mute(true), vec![json!("set_property"), json!("mute"), json!("yes")]);
        assert_eq!(mute(false)[2], json!("no"));
        assert_eq!(sub_show(false)[2], json!("no"));
        // Fullscreen is a player-wide property, sent before the load.
        assert_eq!(fullscreen()[1], json!("fullscreen"));
        assert_eq!(seek("0:01:00"), vec![json!("seek"), json!("0:01:00"), json!("absolute")]);
        assert_eq!(
            sub_add("http://s/x.srt", "中文"),
            vec![json!("sub-add"), json!("http://s/x.srt"), json!("select"), json!("中文")]
        );
        assert_eq!(show_text("Pause", 1000)[2], json!(1000));
    }

    #[test]
    fn observe_ids_are_the_numbers_the_replies_echo_back() {
        assert_eq!(
            observe_all()[1],
            vec![json!("observe_property"), json!(2), json!("time-pos")]
        );
        for (name, id) in OBSERVED {
            assert_eq!(observed_name(id as u64), Some(name));
        }
        // A number we never registered is not a property, and must not be
        // silently read as one.
        assert_eq!(observed_name(9), None);
        assert_eq!(observed_name(0), None);
    }

    #[test]
    fn the_socket_path_carries_the_random_suffix_both_platforms_need() {
        assert_eq!(ipc_socket_path("darwin", 42), "/tmp/macast_mpvsocket42");
        assert_eq!(ipc_socket_path("linux", 42), "/tmp/macast_mpvsocket42");
        // Windows: the verbatim named-pipe form, backslashes and all — the
        // reference gets this from `Setting.get_base_path`, which does no
        // joining to it.
        assert_eq!(ipc_socket_path("windows", 42), r"\\.\pipe\macast_mpvsocket42");
        // `randint(0, 9999)` is inclusive at both ends.
        for _ in 0..200 {
            assert!(socket_random() <= 9999);
        }
    }

    #[test]
    fn the_wire_is_one_json_object_per_line() {
        let line = encode(&loadfile("http://a/b", Some("5"), Liveness::Finite));
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["command"].as_array().unwrap().len(), 5);
        // And the log text is Python's, not serde's: quotes, no `": "`.
        assert_eq!(
            logged_command(&vec![json!("loadfile"), json!("x"), json!("replace")]),
            "['loadfile', 'x', 'replace']"
        );
    }
}
