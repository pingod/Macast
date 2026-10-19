//! Where the settings page looks for installable plugins, and what "国内镜像" means.
//!
//! Port of `macast/plugin_repo.py`. The coordinates live here and nowhere else:
//! `/api?query=plugin-info` hands them to the page, which is what makes them
//! testable — and what makes a **second** copy in the page (or in this crate's HTTP
//! face) a drift waiting to happen (AGENTS.md §4.6).
//!
//! Two things this module is careful about, both of them measured in the reference:
//!
//! * **The switch is read, never written, by a plain getter.** `Setting.get` persists
//!   its default (AGENTS.md §4.2's oldest trap), so a page that polls `plugin-info`
//!   would otherwise grow the user's settings file one `Github_CN_Mirror: false` per
//!   poll. [`crate::setting::github_cn_mirror`] reads through `peek` and is the one
//!   owner; [`mirror_enabled`] is this module's name for it.
//! * **Off means the key is absent**, not `false` ([`set_mirror_enabled`], the
//!   reference's `set_mirror_enabled` / `Setting.unset`).
//!
//! The index itself is **empty** in this fork — all fifteen first-party plugins are
//! built in (AGENTS.md §4.6) — so "nothing installable" is the answer in both builds.
//! What is ported here is therefore the *coordinates and the rewriting rule*, not an
//! installer: this shell still has no plugin subsystem, and `plugins` in
//! `plugin-info` stays an empty list for that reason.

use crate::pyops::form_flag_is_true;
use crate::setting::{github_cn_mirror, Key, Setting};
use serde_json::{Value, json};

/// The fork's own repository — the index moved out of the upstream collection
/// (`plugin_repo.py:25-27`).
pub const REPO: &str = "pingod/Macast";
pub const BRANCH: &str = "main";
pub const INDEX_PATH: &str = "plugins/info.json";

/// Target of the 「打开插件仓库」 button (`plugin_repo.py:30`).
pub fn repo_url() -> String {
    format!("https://github.com/{}/tree/{}/plugins", REPO, BRANCH)
}

/// The three index candidates, **in freshness order** (`plugin_repo.py:41-46`).
///
/// The order is the whole design and it is not decoration: `raw.githubusercontent.com`
/// is canonical and always fresh but is routinely unreachable from the mainland; the
/// ghproxy entry proxies raw on demand, so it is fresh too; jsDelivr comes **last**
/// because it caches branch files for hours — fine as a last resort, wrong as a first
/// choice (AGENTS.md §4.6: 「改顺序前先想一遍：把会缓存的放前面 = 用户看到的插件列表可能落后半天」).
/// All three send CORS headers, because this fetch happens **in the browser**.
pub fn index_urls_plain() -> [String; 3] {
    [
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            REPO, BRANCH, INDEX_PATH
        ),
        format!(
            "https://ghproxy.net/https://raw.githubusercontent.com/{}/{}/{}",
            REPO, BRANCH, INDEX_PATH
        ),
        format!("https://cdn.jsdelivr.net/gh/{}@{}/{}", REPO, BRANCH, INDEX_PATH),
    ]
}

/// The prefix the domestic mirror adds (`plugin_repo.py:52`).
pub const MIRROR_PREFIX: &str = "https://ghproxy.net/";

/// The canonical GitHub hosts the app fetches at runtime (`plugin_repo.py:58-62`).
///
/// jsDelivr's `gh` URLs are **deliberately absent**: they already *are* a GitHub mirror
/// and are reachable from the mainland, so prefixing them would only add a hop.
pub const MIRROR_HOSTS: [&str; 3] = [
    "https://github.com/",
    "https://raw.githubusercontent.com/",
    "https://api.github.com/",
];

/// `url` with a GitHub host put behind the domestic prefix.
///
/// **Pure**: it does not consult the setting, and it leaves a non-GitHub URL — and one
/// that is already mirrored — untouched. That second half is what makes
/// [`index_urls`] collapse to two rows instead of producing a double prefix: the
/// already-mirrored middle entry starts with `https://ghproxy.net/`, which is not one of
/// [`MIRROR_HOSTS`], so it comes back unchanged and the de-duplication drops the
/// mirrored copy of the first.
pub fn to_mirror_url(url: &str) -> String {
    for host in MIRROR_HOSTS {
        if url.starts_with(host) {
            return format!("{MIRROR_PREFIX}{url}");
        }
    }
    url.to_string()
}

/// Whether the user asked for domestic mirrors (default: off).
///
/// The one owner of the read is [`crate::setting::github_cn_mirror`]; this name exists
/// because it is the reference's, and because the two doors below want to say the
/// switch's purpose rather than a key's name.
pub fn mirror_enabled(setting: &Setting) -> bool {
    github_cn_mirror(setting)
}

/// The URL as this build should actually fetch it right now.
pub fn mirror_url(setting: &Setting, url: &str) -> String {
    if mirror_enabled(setting) {
        to_mirror_url(url)
    } else {
        url.to_string()
    }
}

/// Persist the switch. Off means the key is **absent**, not `false` — the file never
/// grows a key whose only meaning is "the default" (AGENTS.md §4.6).
pub fn set_mirror_enabled(setting: &Setting, on: bool) {
    if on {
        setting.set(Key::GithubCnMirror, Value::Bool(true));
    } else {
        setting.unset(Key::GithubCnMirror);
    }
}

/// The ordered index candidates for the current mirror setting
/// (`plugin_repo.py:98-110`.
///
/// In mirror mode the canonical raw URL collapses onto the ghproxy entry already in the
/// list, so the de-duplicated chain **stays fresh-first**: ghproxy (proxies raw on
/// demand), then jsDelivr as the caching last resort. Three rows become two, and which
/// two survive is the claim.
pub fn index_urls(setting: &Setting) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for url in index_urls_plain() {
        let candidate = if mirror_enabled(setting) {
            to_mirror_url(&url)
        } else {
            url
        };
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The two declared packages whose import gates the WebRTC mirror target
/// (`plugin_repo.py:129`).
///
/// The extras asset carries these plus what they pull in (`cryptography`,
/// `pylibsrtp`, `cffi`, `_cffi_backend`), and nothing here enumerates those: this is
/// what `requirements/*.txt` declares and what the build step installs.
pub const EXTRAS_PACKAGES: [&str; 2] = ["aiortc", "av"];

/// The asset name template (`plugin_repo.py:134`.
///
/// os / arch / cpython tag / version are **not decoration** — the wheels inside differ
/// on all three, and the installer refuses a bundle whose manifest disagrees with the
/// machine reading it. Four holes, and the order of the four is a promise: a swapped
/// pair produces a well-formed name that exists on no release.
pub const EXTRAS_ASSET: &str = "Macast-WebRTC-extras-{}-{}-{}-v{}.zip";

/// The one asset name for one ABI on one released version (`plugin_repo.py:137-139`).
pub fn extras_asset_name(os_key: &str, arch: &str, python_tag: &str, version: &str) -> String {
    // The four holes are positional and in the reference's order, spelled out here
    // rather than driven off `EXTRAS_ASSET`, so a reordering is a visible edit and the
    // template stays the string the page is handed.
    format!("Macast-WebRTC-extras-{os_key}-{arch}-{python_tag}-v{version}.zip")
}

/// Ordered candidates for one extras asset, honouring the switch
/// (`plugin_repo.py:142-159`.
///
/// Release assets download from `github.com`, so the domestic mirror is the same prefix
/// the other GitHub hosts get. Mirror mode puts the mirror **first but keeps the direct
/// URL as a fallback** — a dead proxy should not be fatal — and, like [`index_urls`],
/// the list is de-duplicated. AGENTS.md §4.6 measured this path rather than inferring
/// it: on that machine the direct link returned 0 bytes / curl 000 while the prefixed
/// one ran the full 26,884,399 bytes with a matching sha256, at about 0.7 MB/min —
/// reachable, **not** fast.
pub fn extras_urls(setting: &Setting, name: &str, version: &str) -> Vec<String> {
    let direct = format!("https://github.com/{REPO}/releases/download/v{version}/{name}");
    let candidates: Vec<String> = if mirror_enabled(setting) {
        vec![to_mirror_url(&direct), direct]
    } else {
        vec![direct]
    };
    let mut out: Vec<String> = Vec::new();
    for candidate in candidates {
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// What the settings page needs to fetch an extras asset itself
/// (`plugin_repo.py:162-169`.
///
/// The page does not build these strings: it **asks**, so the naming rule has exactly
/// one owner and can be tested against it (the same reason [`describe`] hands over the
/// index URLs instead of letting the page paste them).
pub fn extras_describe() -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "packages".into(),
        Value::Array(EXTRAS_PACKAGES.iter().map(|p| json!(p)).collect()),
    );
    out.insert("asset".into(), json!(EXTRAS_ASSET));
    Value::Object(out)
}

/// The plugin-repository half of the `plugin-info` answer (`plugin_repo.py:113-117`).
///
/// **Key order is the reference's** (`repo_url`, `index_urls`, `mirror_enabled`,
/// `extras`) and the workspace manifest turns on `serde_json`'s `preserve_order`, so
/// this is the document the page was written against rather than an alphabetical
/// paraphrase of it.
pub fn describe(setting: &Setting) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("repo_url".into(), json!(mirror_url(setting, &repo_url())));
    out.insert("index_urls".into(), json!(index_urls(setting)));
    out.insert("mirror_enabled".into(), json!(mirror_enabled(setting)));
    out.insert("extras".into(), extras_describe());
    Value::Object(out)
}

/// `_set_github_mirror`'s read of the posted field (`protocol.py:2493`).
///
/// `str(value).lower() in ('1','true','yes','on')` — the same four words as the
/// 模块设置 panel's `remove` flag, so the two switches in this app agree about what
/// "on" is spelled like. [`crate::pyops::form_flag_is_true`] is the owner; this name
/// exists so the route reads as its own door.
pub fn mirror_flag_is_true(raw: &str) -> bool {
    form_flag_is_true(raw)
}

/// `_set_github_mirror`'s answer (`protocol.py:2486-2501`).
///
/// Returning the fresh [`describe`] saves a second `plugin-info` round trip: the page
/// re-fetches the index straight away with the mirrored URLs.
///
/// The reference wraps `set_mirror_enabled` in `try/except` and answers
/// `{'code': 1, 'message': 'set failed'}`. **That arm does not exist here, and it is
/// not silently dropped**: [`Setting::set`] / [`Setting::unset`] cannot report failure
/// in this port — they swallow the same I/O errors their Python counterparts raise as
/// `OSError` — so a `set failed` sentence would be a document for a state nothing can
/// reach. The failure is *logged* where it happens (`setting.rs`'s `save`), which is
/// the honest place for it; if `save` ever starts returning `Result`, this is the row
/// that has to grow the third branch, and AGENTS.md §4.14's rule applies: an error arm
/// nobody can reach is worse than no arm.
pub fn set_mirror_answer(setting: &Setting, raw: &str) -> Value {
    let on = mirror_flag_is_true(raw);
    set_mirror_enabled(setting, on);
    let mut out = serde_json::Map::new();
    out.insert("code".into(), json!(0));
    out.insert("message".into(), json!("success"));
    out.insert("mirror_enabled".into(), json!(mirror_enabled(setting)));
    out.insert("plugin_repo".into(), describe(setting));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use std::fs;

    struct Scratch(std::path::PathBuf);

    /// Every test owns its settings file: the switch writes, and a shared path would
    /// make one test's `on` the next one's precondition (AGENTS.md §4.2's "并行跑的用例
    /// 共享夹具路径，单跑绿、整组红").
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "macast-core-plugin-repo-{}-{}-{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_micros())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn path(&self) -> std::path::PathBuf {
            self.0.join(crate::paths::SETTING_FILE_NAME)
        }
        fn setting(&self) -> Setting {
            let setting = Setting::new(self.path());
            setting.load();
            setting
        }
        fn bytes(&self) -> Option<String> {
            std::fs::read_to_string(self.path()).ok()
        }
        fn text(&self) -> String {
            self.bytes().unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The reference's own three rows, in its order. Written against the shipped file
    /// rather than this function's output: a test that compares both sides of one
    /// `format!` proves nothing about which host goes first, and the first choice is
    /// the one that decides whether a user sees a stale index.
    #[test]
    fn the_index_chain_is_fresh_first_and_caching_last() {
        let rows = index_urls_plain();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            "https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json"
        );
        assert_eq!(
            rows[1],
            "https://ghproxy.net/https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json"
        );
        assert_eq!(
            rows[2],
            "https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json"
        );
    }

    /// jsDelivr is a mirror already. Prefixing it would be a second hop on the only row
    /// the mainland can always reach — and the rule is a prefix list, so this row is
    /// what stops the list from being "every URL we hand out".
    #[test]
    fn to_mirror_url_touches_only_the_three_github_hosts() {
        assert_eq!(
            to_mirror_url("https://github.com/pingod/Macast/releases/download/v1/x.zip"),
            "https://ghproxy.net/https://github.com/pingod/Macast/releases/download/v1/x.zip"
        );
        assert_eq!(
            to_mirror_url("https://api.github.com/repos/pingod/Macast"),
            "https://ghproxy.net/https://api.github.com/repos/pingod/Macast"
        );
        let jsdelivr = "https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json";
        assert_eq!(to_mirror_url(jsdelivr), jsdelivr, "a mirror of a mirror");
        let already = "https://ghproxy.net/https://raw.githubusercontent.com/a/b";
        assert_eq!(
            to_mirror_url(already),
            already,
            "no double prefix — this is the half that makes index_urls dedupe"
        );
        assert_eq!(to_mirror_url(""), "", "an empty URL is not a GitHub URL");
    }

    /// Three rows off, **two** rows on, and the surviving pair is (ghproxy, jsDelivr).
    /// The collapse is the interesting direction: an implementation that prefixed each
    /// row independently would hand the browser a `ghproxy.net/https://ghproxy.net/…`
    /// second row and still be three rows long.
    #[test]
    fn the_switch_collapses_three_rows_into_two_in_the_right_order() {
        let scratch = Scratch::new("dedupe");
        let setting = scratch.setting();
        assert_eq!(index_urls(&setting), index_urls_plain().to_vec());
        set_mirror_enabled(&setting, true);
        let rows = index_urls(&setting);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(
            rows[0],
            "https://ghproxy.net/https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json"
        );
        assert_eq!(
            rows[1],
            "https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json"
        );
    }

    /// AGENTS.md §4.2's oldest trap, in this module's own shape: a page polls
    /// `plugin-info` once a second, and every poll would otherwise write
    /// `Github_CN_Mirror: false` into the user's file.
    #[test]
    fn reading_the_switch_never_creates_it() {
        let scratch = Scratch::new("peek");
        let setting = scratch.setting();
        assert!(!mirror_enabled(&setting));
        let _ = describe(&setting);
        let _ = index_urls(&setting);
        assert!(
            !setting.has(Key::GithubCnMirror),
            "a read must not grow the key it reports"
        );
        assert!(
            scratch.bytes().is_none(),
            "and no file at all: {}",
            scratch.bytes().unwrap_or_default()
        );
    }

    /// 「关掉走 `Setting.unset` 而不是存 `False`」 (AGENTS.md §4.6): the off state is the
    /// key being **gone**, so a switch this app never enabled stays out of the file.
    #[test]
    fn off_is_absent_not_false() {
        let scratch = Scratch::new("absent");
        let setting = scratch.setting();
        set_mirror_enabled(&setting, true);
        assert!(scratch.text().contains("\"Github_CN_Mirror\": true"));
        set_mirror_enabled(&setting, false);
        assert!(
            !scratch.text().contains("Github_CN_Mirror"),
            "the key survived its own off switch: {}",
            scratch.text()
        );
        assert!(!mirror_enabled(&setting));
    }

    /// The switch's effect on the document the page reads, asked through `describe`
    /// rather than `mirror_url`: `repo_url` is 「打开插件仓库」's target, so a mirrored
    /// answer and a non-mirrored `index_urls` would be two stories about one setting.
    #[test]
    fn describe_moves_every_github_url_the_same_way() {
        let scratch = Scratch::new("both");
        let setting = scratch.setting();
        let off = describe(&setting);
        assert_eq!(off["mirror_enabled"], json!(false));
        assert_eq!(off["repo_url"], json!(repo_url()));
        assert_eq!(off["index_urls"].as_array().expect("list").len(), 3);
        set_mirror_enabled(&setting, true);
        let on = describe(&setting);
        assert_eq!(
            on["repo_url"],
            json!("https://ghproxy.net/https://github.com/pingod/Macast/tree/main/plugins")
        );
        assert_eq!(on["mirror_enabled"], json!(true));
        assert_eq!(on["index_urls"].as_array().expect("list").len(), 2);
    }

    /// Key order is the reference's dict literal, and the workspace turned on
    /// `preserve_order` for a different promise (`LIVE_LOAD_OPTIONS`). Pinned as a list
    /// because "the page reads the first field" is exactly the kind of claim that rots
    /// into a coin flip.
    #[test]
    fn describe_has_the_reference_s_four_keys_in_its_order() {
        let scratch = Scratch::new("order");
        let doc = describe(&scratch.setting());
        let keys: Vec<&String> = doc.as_object().expect("object").keys().collect();
        assert_eq!(
            keys,
            vec!["repo_url", "index_urls", "mirror_enabled", "extras"]
        );
        let extras: Vec<&String> = doc["extras"]
            .as_object()
            .expect("object")
            .keys()
            .collect();
        assert_eq!(extras, vec!["packages", "asset"]);
    }

    /// The two packages, as a list of exactly two. `Part 30`'s lesson applies: the
    /// subtraction that keeps `aiortc`/`av` out of the packaging comparison is keyed on
    /// this name, so "a third entry" must not be reachable without a deliberate edit.
    #[test]
    fn extras_names_exactly_the_two_declared_packages() {
        let doc = extras_describe();
        let packages = doc["packages"].as_array().expect("list");
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0], json!("aiortc"));
        assert_eq!(packages[1], json!("av"));
        assert_eq!(
            doc["asset"],
            json!("Macast-WebRTC-extras-{}-{}-{}-v{}.zip")
        );
    }

    /// Four holes, one order. Each argument is a distinct word so a swapped pair cannot
    /// pass — `darwin-arm64-cp312` versus `arm64-darwin-cp312` is a name that exists on
    /// no release, and the caller would read the failure as "there is no bundle for my
    /// machine" (AGENTS.md §4.15's `EXTRAS_ASSET` reasoning).
    #[test]
    fn the_asset_name_fills_four_holes_in_the_reference_s_order() {
        assert_eq!(
            extras_asset_name("macos", "arm64", "cp312", "0.22.0"),
            "Macast-WebRTC-extras-macos-arm64-cp312-v0.22.0.zip"
        );
        assert_eq!(
            extras_asset_name("windows", "amd64", "cp313", "1.0.0"),
            "Macast-WebRTC-extras-windows-amd64-cp313-v1.0.0.zip"
        );
    }

    /// Off: **one** URL. On: the mirror first and the direct link kept as a fallback —
    /// the reference's own comment says a dead proxy must not be fatal, and §4.6
    /// measured exactly that asymmetry (direct = 0 bytes, prefixed = the whole 26.9 MB).
    #[test]
    fn extras_urls_put_the_mirror_first_and_keep_the_direct_link() {
        let scratch = Scratch::new("urls");
        let setting = scratch.setting();
        let name = extras_asset_name("macos", "arm64", "cp312", "0.22.0");
        let off = extras_urls(&setting, &name, "0.22.0");
        assert_eq!(off.len(), 1, "{off:?}");
        assert_eq!(
            off[0],
            "https://github.com/pingod/Macast/releases/download/v0.22.0/Macast-WebRTC-extras-macos-arm64-cp312-v0.22.0.zip"
        );
        set_mirror_enabled(&setting, true);
        let on = extras_urls(&setting, &name, "0.22.0");
        assert_eq!(on.len(), 2, "{on:?}");
        assert!(on[0].starts_with(MIRROR_PREFIX));
        assert_eq!(on[1], off[0], "the direct link survives as the fallback");
    }

    /// The door's own truth table plus its answer document. Two claims: the flag reads
    /// the reference's four words (`" on "` is **not** one of them — no `strip()` in
    /// `protocol.py:2493`), and the fresh `describe()` is in the same reply, because the
    /// page re-fetches the index without a second round trip.
    #[test]
    fn the_mirror_door_answers_with_the_new_coordinates() {
        let scratch = Scratch::new("door");
        let setting = scratch.setting();
        assert_eq!(
            set_mirror_answer(&setting, "1")["mirror_enabled"],
            json!(true)
        );
        let on = set_mirror_answer(&setting, "on");
        assert_eq!(on["code"], json!(0));
        assert_eq!(on["message"], json!("success"));
        assert_eq!(on["mirror_enabled"], json!(true));
        assert_eq!(on["plugin_repo"]["index_urls"].as_array().expect("l").len(), 2);
        let keys: Vec<&String> = on.as_object().expect("object").keys().collect();
        assert_eq!(keys, vec!["code", "message", "mirror_enabled", "plugin_repo"]);

        set_mirror_answer(&setting, "0");
        assert_eq!(
            set_mirror_answer(&setting, "off")["mirror_enabled"],
            json!(false)
        );
        assert!(
            !scratch.text().contains("Github_CN_Mirror"),
            "the door's off answer left a `false` behind: {}",
            scratch.text()
        );
        for off in ["", "0", "false", "no", "y", "2", " on", "on ", "  on  "] {
            assert!(!mirror_flag_is_true(off), "{off:?} is not one of the four words");
        }
        for on in ["1", "true", "TRUE", "Yes", "on"] {
            assert!(mirror_flag_is_true(on), "{on:?} must mean on");
        }
    }

    /// The reference stores `True` through `Setting.set`, whose value is a **bool**.
    /// Reading it back is what separates "the door wrote something" from "the door wrote
    /// the thing the next poll can read": a string `"true"` would be truthy too, and the
    /// 模块设置 card would then call it a str with a different `type` than the file's
    /// only other bool rows.
    #[test]
    fn the_switch_is_stored_as_a_json_bool() {
        let scratch = Scratch::new("bool");
        let setting = scratch.setting();
        set_mirror_answer(&setting, "1");
        assert!(scratch.text().contains("\"Github_CN_Mirror\": true"));
        let second = Setting::new(scratch.path());
        second.load();
        assert!(mirror_enabled(&second), "a second reader sees the same switch");
    }

    /// `plugin-info` is a read the page polls; it must not be the thing that writes the
    /// file it reports — asserted **with a file that already exists**, because the
    /// absent-key test above cannot tell "no write" from "write of the same bytes".
    #[test]
    fn a_describe_on_a_real_file_leaves_the_bytes_alone() {
        let scratch = Scratch::new("stable");
        fs::write(
            &scratch.path(),
            "{\"ApplicationPort\": 58880, \"DLNA_FriendlyName\": \"x\"}",
        )
        .expect("seed");
        let setting = scratch.setting();
        setting.load();
        let before = scratch.bytes().expect("seeded");
        let _ = describe(&setting);
        assert_eq!(scratch.bytes().expect("still there"), before);
        assert!(!mirror_enabled(&setting));
    }

    /// The version this module has nothing to say about, asserted so the *shape* stays
    /// honest: `plugin-info`'s top-level `version` comes from `paths`, not from here.
    #[test]
    fn this_module_never_invents_a_version() {
        let scratch = Scratch::new("version");
        let doc = describe(&scratch.setting());
        assert!(
            doc.get("version").is_none(),
            "describe() is the repo half, not the whole answer"
        );
        assert!(!paths::version().is_empty(), "the other half has a value");
    }
}
