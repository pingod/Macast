//! Which settings key belongs to which module — and what to call it.
//!
//! Port of `macast/module_settings.py`. The 高级设置 tab has always exposed the raw
//! JSON, which answers "what is stored" but never "who wrote this"; this module turns
//! the persisted keys into per-module groups for the 模块设置 tab, and it is the **one**
//! owner of that map (AGENTS.md §4.11's first red line: 归属表只写在这一处, not
//! scattered through the modules that happen to read a key).
//!
//! Three things are deliberately **not** copies of the reference, and each is a
//! decision rather than an oversight:
//!
//! * **No plugin groups.** The reference walks the loaded plugin manager and gives each
//!   plugin with a `SettingProperty` its own card. This shell has no plugin subsystem
//!   yet, so `groups` answers core → player → 其他（未归类） — the same shape the
//!   reference produces when `plugins_from_manager()` returns `[]` (an unpublished
//!   manager, early boot, tests). When plugins are ported, the missing piece is a
//!   per-plugin label map and one more `groups` input; nothing here has to be re-derived.
//! * **`Timeout` is a core row here.** In Python it belongs to the `cast_local_file`
//!   *plugin* (`module_settings.py:113`), because that plugin is what reads it. In this
//!   port the device hand-off sender lives in `macast-cast::push` and reads the same
//!   JSON key (`setting.rs`'s `Key::Timeout` carries the whole argument), so the owner
//!   in this build really is the core. A label table that names a plugin this shell does
//!   not run would be the page pointing at a module that is not there — the same reason
//!   `airplay_mirror` refuses to guess which mDNS name won.
//! * **The hint text for `Timeout` says who reads it *here*.** Labels stay verbatim
//!   (they are the key's name in the user's language); a hint is a sentence about this
//!   machine, and copying the plugin's sentence would describe a different build.
//!
//! Values that are scalars (str/int/float/bool/None) are marked editable; lists and
//! dicts stay read-only here — they are structure, and 高级设置's JSON editor remains the
//! place to change them (`_item`'s rule, `module_settings.py:189-204`).

use crate::setting::Setting;
use serde_json::{Map, Value, json};

/// Group ids that are not plugins.
pub const CORE_GROUP: &str = "core";
pub const PLAYER_GROUP: &str = "player";
pub const OTHER_GROUP: &str = "other";

/// Group titles, verbatim from the reference (`build_groups`).
pub const CORE_TITLE: &str = "Macast 核心";
pub const PLAYER_TITLE: &str = "MPV 播放器";
pub const OTHER_TITLE: &str = "其他（未归类）";

/// The sentence an unclaimed key gets, verbatim from the reference. It has to give a
/// next step (AGENTS.md §4.8's 「每一条失败必须给出下一步」 applied to a label table):
/// a key nobody owns is not an error, it is a thing the user may delete.
pub const UNCLASSIFIED_HINT: &str = "未识别归属的键；确认无用后可直接删除。";

/// One row of the ownership map: the exact JSON key, what the page calls it, and the
/// sentence that says who reads it.
pub struct Entry {
    pub name: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
}

macro_rules! entry {
    ($name:expr, $label:expr, $hint:expr) => {
        Entry { name: $name, label: $label, hint: $hint }
    };
}

/// Keys persisted by Macast itself. Order is the reference's dict order
/// (`CORE_LABELS`, `module_settings.py:30-55`) and the page renders it in this order,
/// so a reorder is a visible change, not a refactor.
pub const CORE: [Entry; 22] = [
    entry!("USN", "SSDP USN", "设备标识序号，换网卡/重置发现时会变，一般不用手动改。"),
    entry!("CheckUpdate", "自动检查更新", "bool：启动时是否向 GitHub 检查新版本。"),
    entry!("StartAtLogin", "开机自启", "bool：登录后自动运行 Macast。"),
    entry!("MenubarIcon", "菜单栏图标", "控制菜单栏图标的显示/样式。"),
    entry!("ApplicationPort", "应用端口", "设置页与网页投屏入口的 HTTP 端口（默认 58880）。"),
    entry!("DLNA_FriendlyName", "DLNA 设备名", "投屏发送端看到的本机设备名称。"),
    entry!("Macast_Renderer", "当前渲染器", "正在使用的渲染器插件标题；渲染器一次只能选一种。"),
    entry!("Macast_Protocol", "（旧）单一协议", "Macast_Protocols 出现前的旧键，仅作迁移来源保留。"),
    entry!("Blocked_Interfaces", "禁用的网卡", "列表：不参与发现与广播的网卡名。"),
    entry!(
        "Additional_Interfaces",
        "附加的网卡",
        "列表：默认路由之外额外允许广播的网卡名。"
    ),
    entry!("Play_History", "投屏历史", "列表：设置页「历史」面板的数据，可整表清空。"),
    entry!("Https_Enabled", "启用 HTTPS 通道", "bool：为管理 API 启用 HTTPS。"),
    entry!("Https_Port", "HTTPS 端口", ""),
    entry!("Https_Cert", "HTTPS 证书路径", ""),
    entry!("Https_Key", "HTTPS 私钥路径", ""),
    entry!(
        "Macast_Protocols",
        "启用的协议",
        "列表：同时开启的协议标题（DLNA / Chromecast / AirPlay…）。"
    ),
    entry!(
        "Network_Interface",
        "固定网卡",
        "网卡名；空串表示自动选择承载 IPv4 默认路由的网卡。"
    ),
    entry!("Disabled_Plugins", "已停用的插件", "列表：被关掉（不加载）的插件标题。"),
    entry!(
        "Api_Token",
        "管理令牌",
        "网页投屏入口与远程管理 API 的凭据，状态页可见可复制。"
    ),
    entry!("Github_CN_Mirror", "启用国内镜像地址", "bool：所有 GitHub 地址改走国内可达的镜像。"),
    entry!(
        "Resolve_Cookie_Browser",
        "网页投屏读取 cookie 的浏览器",
        "「网页地址投屏」在没有本机 cookie 文件时从哪个浏览器读会话 cookie；空＝不读取。\
         cookie 内容不在这里，只存在配置目录的 resolve_cookies.txt。"
    ),
    // See the module header: in Python this row is a plugin's; here the reader is the
    // port's own device hand-off sender, so the group is core and the hint names *this*
    // build's reader instead of a plugin that is not loaded.
    entry!(
        "Timeout",
        "网络超时",
        "秒，socket 超时；本机投给 Chromecast / DLNA 电视的那一步读的就是这一格。"
    ),
];

/// Persisted keys of the built-in mpv renderer (`PLAYER_LABELS`,
/// `module_settings.py:57-65`).
pub const PLAYER: [Entry; 5] = [
    entry!("PlayerHW", "硬件解码", "0 关 / 1 开 / 2 强制，透传给 mpv 的 hwdec。"),
    entry!("PlayerSize", "播放器尺寸", "0 小 / 1 正常 / 2 大 / 3 自适应 / 4 全屏。"),
    entry!("PlayerPosition", "播放器位置", "0..4：左上 / 左下 / 右上 / 右下 / 居中。"),
    entry!("PlayerOntop", "播放器置顶", "0 否 / 1 是。"),
    entry!("PlayerDefaultVolume", "默认音量", "每次开始播放时设置的音量（0-100）。"),
];

/// `key in owned_keys(...)` for the write path (`module_settings.py:269-278`).
///
/// The reference's `owned_keys` builds a set — the plugin tables plus everything in the
/// file. There is no such list here on purpose: an `owned_keys()` returning borrowed
/// strings would have to leak the stored names it does not own, and a page polling the
/// read side would leak one string per poll. A key the table does not own is owned by
/// whoever stored it, which is exactly the answer the write path needs:
/// a table row, **or** a key already in the file.
pub fn is_owned(key: &str, stored: &Map<String, Value>) -> bool {
    CORE.iter().any(|e| e.name == key)
        || PLAYER.iter().any(|e| e.name == key)
        || stored.contains_key(key)
}

/// Whether one of the two tables claims this name.
fn is_claimed(name: &str) -> bool {
    CORE.iter().any(|e| e.name == name) || PLAYER.iter().any(|e| e.name == name)
}

/// Build an object with its fields in the order written. `serde_json::Map` preserves
/// insertion order here (the workspace turns on `preserve_order`), and these documents'
/// field order is what the page renders, so the order is a wire property rather than
/// cosmetics — the same reason `groups()` builds its cells in the reference's order.
fn object(fields: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (name, value) in fields {
        map.insert((*name).to_string(), value.clone());
    }
    Value::Object(map)
}

/// The five answers `set_value` can give (`module_settings.py:312-343`). They are
/// written here in the reference's dict order, and the branch wording is what a user
/// reads, so it is copied rather than paraphrased.
pub fn set_value(setting: &Setting, key: Option<&str>, raw_value: &str, remove: bool) -> Value {
    // `str(key or '').strip()` — a missing key and an empty one are the same answer.
    let name = key.unwrap_or("").trim().to_string();
    if name.is_empty() {
        return object(&[("code", json!(1)), ("message", json!("缺少设置项名称"))]);
    }
    let stored = setting.snapshot();
    if !is_owned(&name, &stored) {
        // The name is read back to the caller: a page that posts a typo needs to see
        // which word the machine did not know, and 「未知设置项」 without the name is the
        // kind of sentence that costs an hour (§4.2's 「空结果要说清是哪一种空」 again).
        return object(&[
            ("code", json!(1)),
            ("message", json!(format!("未知设置项：{name}"))),
        ]);
    }
    if remove {
        let was_present = setting.remove_name(&name);
        return object(&[
            ("code", json!(0)),
            (
                "message",
                json!(if was_present { "已删除" } else { "本就未设置" }),
            ),
            ("removed", json!(was_present)),
        ]);
    }
    // `json.loads(raw_value)` falling back to the raw text: the page posts JSON scalars
    // (`58880`, `true`, `"en0"`), and anything that is not JSON is a **string** — which
    // is how a plain-text setting stays writable without the page having to quote it.
    let value = serde_json::from_str::<Value>(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_string()));
    if value.is_array() || value.is_object() {
        return object(&[
            ("code", json!(1)),
            ("message", json!("列表/对象值请在「高级设置」JSON 里编辑")),
        ]);
    }
    setting.set_name(&name, value.clone());
    object(&[
        ("code", json!(0)),
        ("message", json!("已保存")),
        ("value", value),
    ])
}

/// `type(value).__name__` for the seven shapes a JSON file can hold, and `''` for a
/// key that is absent. The page branches on these strings, so they are a wire format:
/// Python writes `str`/`int`/`float`/`bool`/`NoneType`/`list`/`dict`
/// (`module_settings.py:198`).
///
/// The rule itself lives in [`crate::pyops::type_name`] — it has a second writer
/// (`macast-mpv/src/pyops.rs` puts the same words into an abort message), and two
/// spellings of "which type is this" is how the 模块设置 card and a player abort end up
/// calling the same value by two names.
pub fn py_type_name(value: &Value) -> &'static str {
    crate::pyops::type_name(value)
}

/// `_SCALARS = (str, int, float, bool)` plus the explicit `or value is None` in
/// `editable` (`module_settings.py:154, 202`).
fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_))
}

/// One cell: the seven fields, in the reference's order.
fn item(name: &str, label: &str, hint: &str, stored: &Map<String, Value>) -> Value {
    let present = stored.get(name);
    let mut cell = Map::new();
    cell.insert("key".into(), json!(name));
    // `label or key` — an empty label falls back to the key itself, which is what makes
    // a third-party plugin's verbatim row work. Our tables never have an empty label.
    cell.insert("label".into(), json!(if label.is_empty() { name } else { label }));
    cell.insert("hint".into(), json!(hint));
    cell.insert("present".into(), json!(present.is_some()));
    match present {
        Some(value) => {
            // Absent keys answer `value: null` in Python too (`stored.get(key)`), and
            // `type` is `''` — the page distinguishes "unset" from "set to null" by
            // `present`, not by the value.
            cell.insert("value".into(), value.clone());
            cell.insert("type".into(), json!(py_type_name(value)));
            cell.insert("editable".into(), json!(is_scalar(value) || value.is_null()));
        }
        None => {
            cell.insert("value".into(), Value::Null);
            cell.insert("type".into(), json!(""));
            // A key that is not there yet is editable: writing it creates it.
            cell.insert("editable".into(), json!(true));
        }
    }
    Value::Object(cell)
}

fn group(id: &str, title: &str, keys: Vec<Value>) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("title".into(), json!(title));
    out.insert("version".into(), json!(""));
    out.insert("keys".into(), Value::Array(keys));
    Value::Object(out)
}

/// The ordered groups: 核心 → MPV 播放器 → 其他（未归类）.
///
/// **The read has no side effect.** This takes a snapshot the caller already holds; it
/// never asks `Setting` for a key, because `Setting::get` writes the default into the
/// table on a miss (AGENTS.md §4.2's oldest trap) and a page that polls this query would
/// otherwise create all 27 keys on a clean install. `the_payload_never_creates_a_key`
/// asks that question of the caller's table.
pub fn groups(stored: &Map<String, Value>) -> Vec<Value> {
    let mut out = vec![
        group(
            CORE_GROUP,
            CORE_TITLE,
            CORE.iter().map(|e| item(e.name, e.label, e.hint, stored)).collect(),
        ),
        group(
            PLAYER_GROUP,
            PLAYER_TITLE,
            PLAYER.iter().map(|e| item(e.name, e.label, e.hint, stored)).collect(),
        ),
    ];
    let mut leftovers: Vec<&String> = stored.keys().filter(|k| !is_claimed(k)).collect();
    // `sorted(...)` — the reference sorts because a settings file's own order is the
    // user's typing order, and a page that shows "other" would otherwise shuffle between
    // polls.
    leftovers.sort();
    if !leftovers.is_empty() {
        out.push(group(
            OTHER_GROUP,
            OTHER_TITLE,
            leftovers
                .into_iter()
                .map(|k| item(k, k, UNCLASSIFIED_HINT, stored))
                .collect(),
        ));
    }
    out
}

/// `payload`: the GET `query=module-settings` body.
///
/// **Success carries no `message` key** — the same two-shape rule `launch-param` learned
/// from the log panel (the reference's success dict is `{code, groups}` and its refusal
/// is `{code, message}`), so a page that decides by "is there a message" is wrong in both
/// directions and the port must not be the one that blurs it.
pub fn payload(stored: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("code".into(), json!(0));
    out.insert("groups".into(), Value::Array(groups(stored)));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Setting` arrives through `super::*` (the module owns it for `set_value`); importing it
    // again here would be a second name for one type, which rustc refuses.
    use crate::setting::Key;
    use serde_json::json;

    fn stored(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
        let mut map = Map::new();
        for (k, v) in pairs {
            map.insert(k.into(), v);
        }
        map
    }

    fn cell<'a>(groups: &'a [Value], group_id: &str, key: &str) -> &'a Value {
        groups
            .iter()
            .find(|g| g["id"] == json!(group_id))
            .unwrap_or_else(|| panic!("no group {group_id}"))
            .get("keys")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter().find(|r| r["key"] == json!(key))
            })
            .unwrap_or_else(|| panic!("no row {key} in {group_id}"))
    }

    /// The drift check AGENTS.md §4.11 asks for, in the shape this build can actually
    /// answer: the reference scans the repo for keys that get persisted and requires a
    /// label for each; here the set is closed (`Key::all()`), so the question is exact —
    /// **every key this shell can write must be owned by exactly one table**. A key that
    /// is in no table lands in 其他（未归类） with a verbatim label, which reads to the
    /// user as "this machine has a setting nobody wrote" — and that is precisely the
    /// drift Part 28 exists to catch.
    #[test]
    fn every_key_in_the_table_is_owned_by_exactly_one_group() {
        let mut missing = Vec::new();
        let mut double = Vec::new();
        for key in Key::all() {
            let name = key.name();
            let in_core = CORE.iter().any(|e| e.name == name);
            let in_player = PLAYER.iter().any(|e| e.name == name);
            match (in_core, in_player) {
                (false, false) => missing.push(name),
                (true, true) => double.push(name),
                _ => {}
            }
        }
        assert!(missing.is_empty(), "keys with no owner in the label table: {missing:?}");
        assert!(double.is_empty(), "keys owned by two tables: {double:?}");
        // And the reverse direction: no table row is a key this shell does not know.
        // A label for a name nobody can write is how a table starts drifting toward a
        // plugin that is not loaded.
        let unknown = CORE
            .iter()
            .chain(PLAYER.iter())
            .filter(|e| Key::from_name(e.name).is_none())
            .map(|e| e.name)
            .collect::<Vec<_>>();
        assert!(unknown.is_empty(), "label rows for keys that are not in `Key`: {unknown:?}");
        assert_eq!(
            CORE.len() + PLAYER.len(),
            Key::all().len(),
            "the two tables must cover the key table exactly once"
        );
    }

    /// Labels are user-facing text in a Chinese UI, and AGENTS.md §4.5b's rule for plugin
    /// descriptions is the same rule one layer over: a row that leaks an English
    /// "Application port" onto the page is a fallback nobody noticed.
    ///
    /// The exception list is **the reference's own**, not a tolerance invented here:
    /// `CORE_LABELS['USN']` is `'SSDP USN'` in `module_settings.py:31`, and a table that
    /// "improves" it to Chinese is a divergence nobody asked for. So the rule is
    /// "Chinese, unless the reference writes it this way" — and the list must be empty
    /// the day someone checks the reference and translates the row, which is what the
    /// second half of this test asks.
    #[test]
    fn labels_are_not_empty_and_are_written_in_chinese() {
        const TECHNICAL_LABELS_VERBATIM: [&str; 1] = ["SSDP USN"];
        for e in CORE.iter().chain(PLAYER.iter()) {
            assert!(!e.label.is_empty(), "{} has no label", e.name);
            let chinese = e.label.chars().any(|c| c > '\u{2E80}');
            let verbatim = TECHNICAL_LABELS_VERBATIM.contains(&e.label);
            assert!(chinese || verbatim, "label for {} is not Chinese: {}", e.name, e.label);
            if !chinese {
                assert_eq!(
                    e.name,
                    "USN",
                    "a non-Chinese label appeared outside the one the reference writes                      that way: {} => {}",
                    e.name,
                    e.label
                );
            }
            // Hints may legitimately be empty (the reference leaves `Https_Port` bare),
            // but a non-empty one must be Chinese too.
            if !e.hint.is_empty() {
                assert!(
                    e.hint.chars().any(|c| c > '\u{2E80}'),
                    "hint for {} is not Chinese: {}",
                    e.name,
                    e.hint
                );
            }
        }
    }

    #[test]
    fn the_groups_are_core_then_player_then_other_in_that_order() {
        let groups = groups(&stored(vec![("Zebra_Count".into(), json!(3))]));
        let ids: Vec<&str> = groups.iter().map(|g| g["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["core", "player", "other"], "group order: {ids:?}");
        // **Against the literals, not against the constants.** The first version of these
        // two lines compared the document with `CORE_TITLE` / `PLAYER_TITLE` /
        // `OTHER_TITLE` — the same constants that produced it — so they were true for any
        // title, which is the 「同源等式对任何常量都成立」 shape §16 keeps hitting. The page
        // renders these strings to a user, so the user-visible value is what gets pinned.
        assert_eq!(groups[0]["title"], json!("Macast 核心"));
        assert_eq!(groups[1]["title"], json!("MPV 播放器"));
        assert_eq!(groups[2]["title"], json!("其他（未归类）"));
        // The version slot exists and is empty for the three non-plugin groups — the
        // page renders `title + version` as one string, so a missing key is a `undefined`
        // in the middle of a title.
        for g in &groups {
            assert_eq!(g["version"], json!(""));
        }
        // And the cell carries the table's **label**, not the key it was built from.
        // Asked here because `item()` is decided here: when this row was only asserted at
        // the HTTP layer, the mutant that replaced every label with the raw key name went
        // red in `macast-upnp` and green in the crate that owns the cell (measured
        // 2026-10-19) — a property of a builder needs a reader that reads the builder.
        // `SSDP USN` is the one label written without Chinese characters, so this line is
        // also the witness that the exception list is real and in use.
        let first = groups[0]["keys"].as_array().expect("core rows")[0].clone();
        assert_eq!(first["key"], json!("USN"));
        assert_eq!(first["label"], json!("SSDP USN"));
    }

    #[test]
    fn there_is_no_other_group_when_everything_is_claimed() {
        let groups = groups(&stored(vec![("USN".into(), json!("uuid-1"))]));
        assert_eq!(groups.len(), 2, "an unclaimed bucket appeared for a claimed key");
    }

    /// `sorted(...)` is load-bearing **in the reference**, and this row says how far it
    /// is load-bearing here: `Setting::snapshot()` comes out of a `BTreeMap`, so the
    /// bucket is already in name order and this assertion cannot fail today. It is kept
    /// because the day the table becomes order-preserving (`serde_json`'s `preserve_order`
    /// feature, or a `Vec`-backed settings table — both plausible) this stops being
    /// automatic and the page's stable ordering needs a guard. A row that pins a property
    /// the current data structure gives for free is honest **only** when it says so;
    /// pretending it has teeth is how the next reader learns the ordering nobody promised.
    #[test]
    fn unclaimed_keys_are_sorted_and_named_verbatim_with_a_next_step() {
        let groups = groups(&stored(vec![
            ("zulu".into(), json!(1)),
            ("Alpha".into(), json!(2)),
            ("Mirror_Target".into(), json!("192.168.1.13:8009")),
        ]));
        let rows = groups[2]["keys"].as_array().unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r["key"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Alpha", "Mirror_Target", "zulu"], "leftover order: {names:?}");
        for row in rows {
            // label falls back to the key itself — the page must not invent a name.
            assert_eq!(row["label"], row["key"]);
            // The literal sentence, not `UNCLASSIFIED_HINT` — comparing a document with
            // the constant that wrote it is a check that agrees with anything (the same
            // trap §16 keeps naming; this file now avoids it in all three of its
            // string-valued rows).
            assert_eq!(row["hint"], json!("未识别归属的键；确认无用后可直接删除。"));
        }
    }

    /// The seven shapes Python's `type().__name__` reports, plus `''` for absent. This is
    /// a wire format: the page's editor widget is chosen from it.
    #[test]
    fn type_names_are_the_strings_python_writes() {
        let cases = [
            (json!("text"), "str"),
            (json!(5), "int"),
            // `5.0` in a JSON file is a Python `float`, and serde agrees via `is_i64`
            // being false — the trap is reading `as_i64` off `5.0` and calling it int.
            (json!(5.0), "float"),
            (json!(true), "bool"),
            (json!(null), "NoneType"),
            (json!(["a"]), "list"),
            (json!({"a": 1}), "dict"),
        ];
        for (value, want) in cases {
            assert_eq!(py_type_name(&value), want, "wrong type name for {value}");
        }
        let groups = groups(&stored(vec![]));
        assert_eq!(cell(&groups, "core", "USN")["type"], json!(""));
    }

    /// `present` / `value` / `editable`, in both directions: a scalar that is there is
    /// editable, a structure that is there is **not**, a null that is there **is**
    /// (`or value is None`), and a key that is absent is editable because writing it
    /// creates it.
    #[test]
    fn editable_is_the_reference_s_rule_not_a_guess() {
        let groups = groups(&stored(vec![
            ("ApplicationPort".into(), json!(58880)),
            ("Play_History".into(), json!([{"uri": "x"}])),
            ("Https_Cert".into(), json!(null)),
            ("Blocked_Interfaces".into(), json!(["vmnet"])),
        ]));
        let port = cell(&groups, "core", "ApplicationPort");
        assert_eq!(port["present"], json!(true));
        assert_eq!(port["value"], json!(58880));
        assert_eq!(port["editable"], json!(true));

        let history = cell(&groups, "core", "Play_History");
        assert_eq!(history["type"], json!("list"));
        assert_eq!(
            history["editable"],
            json!(false),
            "a list is structure; the JSON tab is the only place that may half-edit it"
        );

        let null = cell(&groups, "core", "Https_Cert");
        assert_eq!(null["present"], json!(true), "a stored null is present, not absent");
        assert_eq!(null["editable"], json!(true));

        let absent = cell(&groups, "core", "USN");
        assert_eq!(absent["present"], json!(false));
        assert_eq!(absent["value"], Value::Null);
        assert_eq!(absent["type"], json!(""));
        assert_eq!(absent["editable"], json!(true));
    }

    /// The success document is `{code, groups}` — **no `message` key** — and the field
    /// order is the reference's. A page that decides success by "is there a message" is
    /// wrong in both directions (the log panel learned this the same way).
    #[test]
    fn the_payload_has_two_keys_in_the_reference_s_order() {
        let out = payload(&stored(vec![]));
        let map = out.as_object().expect("an object");
        let names: Vec<&String> = map.keys().collect();
        assert_eq!(names, vec!["code", "groups"], "keys: {names:?}");
        assert_eq!(map["code"], json!(0));
        assert_eq!(map.len(), 2, "a third key appeared: {names:?}");
        assert!(!map.contains_key("message"));
        // And one cell's own field order, which is what the page's row template reads.
        // The workspace manifest turns on `serde_json`'s `preserve_order` (root
        // `Cargo.toml`, there for `LIVE_LOAD_OPTIONS`), so this is a **property of the
        // build**, not a freebie: drop that feature and every cell silently re-orders,
        // and this row is what says so. The acceptance script reads the same order off the
        // wire (§28). §8's recorded alphabetical deviation is about the **settings table's**
        // keys (`Setting`'s `BTreeMap`), not about cells — the first version of that check
        // conflated the two and reported a false finding about correct code.
        let groups = out["groups"].as_array().expect("groups");
        let cell = groups[0]["keys"].as_array().expect("rows")[0]
            .as_object()
            .expect("a cell");
        let order: Vec<&String> = cell.keys().collect();
        assert_eq!(
            order,
            vec!["key", "label", "hint", "present", "value", "type", "editable"],
            "the cell's fields came out in a different order: {order:?}"
        );
    }

    /// AGENTS.md §4.2's oldest trap, asked of this read: the page polls
    /// `query=module-settings`, so the read must not be the thing that creates the keys
    /// it is reporting. `Setting::get` would write every default into the table; this
    /// asks a real `Setting` to build a payload and then checks the file is **still
    /// without those keys**.
    #[test]
    fn the_payload_never_creates_a_key_in_the_setting_it_reads() {
        let dir = std::env::temp_dir().join(format!(
            "macast-core-module-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_micros())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(crate::paths::SETTING_FILE_NAME);
        let setting = Setting::new(&path);
        setting.load();
        let before = setting.snapshot();
        assert!(before.is_empty(), "a fresh scratch dir should hold no keys, got {before:?}");
        let out = payload(&before);
        // 22 core + 5 player rows, all `present: false`, and none of them in the table.
        let rows = out["groups"].as_array().unwrap()[0]["keys"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(rows, 22);
        let after = setting.snapshot();
        assert!(after.is_empty(), "building the payload wrote {} key(s): {after:?}", after.len());
        assert!(!setting.has(Key::Usn), "the read created USN");
        assert!(!setting.has(Key::PlayerDefaultVolume), "the read created PlayerDefaultVolume");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The write path's ownership question, in all three directions: a table key is
    /// writable even when absent (that is what "writing creates it" means), a stored
    /// unowned key is writable (otherwise 其他（未归类） would be a display with no way
    /// to clean it up), and a name nobody ever wrote is refused.
    #[test]
    fn ownership_covers_created_and_stored_but_not_invented_names() {
        let map = stored(vec![("zulu".into(), json!(1))]);
        assert!(is_owned("ApplicationPort", &map), "a table key must be writable when absent");
        assert!(is_owned("PlayerHW", &map), "the player table must own its keys too");
        assert!(is_owned("zulu", &map), "a stored key must stay writable from the page");
        assert!(is_owned("Timeout", &map), "the device hand-off timeout is a core row here");
        assert!(!is_owned("Never_Heard_Of_It", &map));
        // The same answer, asked of an empty file: an unowned key that is not stored is
        // still not writable, so this is not `stored` doing all the work.
        assert!(!is_owned("zulu", &Map::new()));
    }

    /// The two tables' row counts are quoted by the acceptance script and by §16; a row
    /// added to one table without adding the key to `Key` is caught above, and a row
    /// added *with* the key must move these numbers on purpose, not silently.
    #[test]
    fn the_two_tables_have_the_counts_the_docs_quote() {
        assert_eq!(CORE.len(), 22);
        assert_eq!(PLAYER.len(), 5);
    }

    // ---------------------------------------------------------------- write path

    /// A scratch settings file, with a Drop that removes it. Mirrors `setting.rs`'s own
    /// test helper: a name unique to the test and the process, and **no** user directory
    /// anywhere near it (AGENTS.md §10's first rule).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "macast-core-module-write-{}-{}-{}",
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
        fn bytes(&self) -> Option<Vec<u8>> {
            std::fs::read(self.path()).ok()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_scalar_write_lands_in_the_file_and_in_the_next_grouped_read() {
        let scratch = Scratch::new("scalar");
        let setting = scratch.setting();
        let answer = set_value(&setting, Some("ApplicationPort"), "59001", false);
        assert_eq!(answer["code"], json!(0));
        assert_eq!(answer["message"], json!("已保存"));
        assert_eq!(answer["value"], json!(59001));
        // The bytes are the proof, not the in-memory table: 模块设置's whole promise is
        // that what the page writes is what the next process reads.
        let on_disk = String::from_utf8(scratch.bytes().expect("the file was written")).expect("utf-8");
        assert!(on_disk.contains("\"ApplicationPort\": 59001"), "file: {on_disk}");
        // …and the grouped read sees it, because both go through one table.
        let doc = payload(&setting.snapshot());
        let cell = doc["groups"][0]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == json!("ApplicationPort"))
            .expect("the row exists");
        assert_eq!(cell["present"], json!(true));
        assert_eq!(cell["value"], json!(59001));
        assert_eq!(cell["type"], json!("int"));
    }

    /// The two refusals that must **not** touch the file: an unowned name, and a
    /// container value. Asserted against the bytes rather than the in-memory table,
    /// because "the answer was no" and "nothing was written" are two different claims and
    /// the settings file has been destroyed by a code path that only meant to answer.
    #[test]
    fn a_refused_write_refuses_the_disk_too() {
        let scratch = Scratch::new("refused");
        let setting = scratch.setting();
        setting.set(Key::ApplicationPort, json!(58880));
        let before = scratch.bytes().expect("the file exists after a real write");

        let unknown = set_value(&setting, Some("no_such_key"), "1", false);
        assert_eq!(unknown["code"], json!(1));
        assert_eq!(unknown["message"], json!("未知设置项：no_such_key"));

        let padded = set_value(&setting, Some("   "), "1", false);
        assert_eq!(padded["message"], json!("缺少设置项名称"));
        let missing = set_value(&setting, None, "1", false);
        assert_eq!(missing["message"], json!("缺少设置项名称"));

        let container = set_value(&setting, Some("Play_History"), "[{\"uri\": \"x\"}]", false);
        assert_eq!(container["code"], json!(1));
        assert_eq!(
            container["message"],
            json!("列表/对象值请在「高级设置」JSON 里编辑"),
            "a structure has to be named as structure, or the user reads it as a save failure"
        );
        let object_value = set_value(&setting, Some("Macast_Protocols"), "{\"a\":1}", false);
        assert_eq!(object_value["code"], json!(1));

        assert_eq!(
            scratch.bytes().expect("the file is still there"),
            before,
            "a refused write rewrote the settings file"
        );
        assert!(!setting.has(Key::PlayHistory), "the refused container still landed in the table");
    }

    /// `json.loads` falling back to the raw text: the page posts JSON scalars, and
    /// anything that is not JSON is **a string** — which is how `en0` is written without
    /// the caller having to quote it. The four shapes are pinned because the value's
    /// *type* is what the next read reports as `type` and `editable`.
    #[test]
    fn a_value_that_is_not_json_is_stored_as_a_string() {
        let scratch = Scratch::new("shapes");
        let setting = scratch.setting();
        for (raw, want) in [
            ("en0", json!("en0")),
            ("58880", json!(58880)),
            ("true", json!(true)),
            ("null", Value::Null),
            ("42.5", json!(42.5)),
            ("\"en0\"", json!("en0")),
        ] {
            let answer = set_value(&setting, Some("Network_Interface"), raw, false);
            assert_eq!(answer["code"], json!(0), "{raw} was refused: {answer}");
            assert_eq!(answer["value"], want, "raw {raw} stored as something else");
            assert_eq!(setting.peek(Key::NetworkInterface).unwrap_or(Value::Null), want, "raw {raw}");
        }
        // `NaN` and `Infinity` are JSON extensions Python's parser accepts **and** serde
        // accepts, so they land as numbers here too — but they are not something the page
        // posts, and a NaN in a settings file is a number no reader agrees about. Pinned
        // so nobody "fixes" one side of this without noticing the other moved.
        let nan = set_value(&setting, Some("Https_Port"), "NaN", false);
        assert!(nan["value"].is_number() || nan["value"].is_string(), "NaN: {nan}");
    }

    /// Remove answers two different sentences, and the second one must not rewrite the
    /// file. `was_present` is in the document because the page shows "已删除" only when
    /// something was deleted — 「本就未设置」 with a `removed: true` would be the same kind
    /// of lie the log panel's key-count bug was.
    #[test]
    fn remove_names_which_of_two_things_happened() {
        let scratch = Scratch::new("remove");
        let setting = scratch.setting();
        set_value(&setting, Some("Network_Interface"), "en0", false);
        let before = scratch.bytes().expect("written");

        let gone = set_value(&setting, Some("Network_Interface"), "", true);
        assert_eq!(gone["code"], json!(0));
        assert_eq!(gone["message"], json!("已删除"));
        assert_eq!(gone["removed"], json!(true));
        assert!(!setting.has(Key::NetworkInterface), "the key is still in the table");
        let after_delete = scratch.bytes().expect("still a file");
        assert_ne!(after_delete, before, "the delete was memory-only");

        // An owned key that is simply absent: refused? no — answered, and the file is
        // left exactly as it was.
        let never = set_value(&setting, Some("Github_CN_Mirror"), "", true);
        assert_eq!(never["message"], json!("本就未设置"));
        assert_eq!(never["removed"], json!(false));
        assert_eq!(scratch.bytes().expect("still a file"), after_delete);

        // An unowned name is refused *before* the remove branch, so no delete can wipe a
        // key the read side would then have to explain.
        let foreign = set_value(&setting, Some("no_such_key"), "", true);
        assert_eq!(foreign["code"], json!(1));
        assert_eq!(scratch.bytes().expect("still a file"), after_delete);
    }

    /// `remove` is read with the reference's truthiness list, and the field arrives as
    /// text: `('1','true','yes','on')` lower-cased (`protocol.py:3103-3104`). `0` and the
    /// empty string are **not** removals — a page that posts `remove=0` next to a value
    /// means to write it.
    #[test]
    fn the_remove_flag_is_the_reference_s_truthiness() {
        let truthy = ["1", "true", "TRUE", "Yes", "on"];
        for text in truthy {
            assert!(
                remove_flag_is_true(text),
                "{text:?} must mean remove — the reference lower-cases and tests membership"
            );
        }
        for text in ["", "0", "false", "no", "off", "1x", "remove", "  ON ", "\tON"] {
            assert!(
                !remove_flag_is_true(text),
                "{text:?} must not mean remove — the reference never strips, and the \
                 wrong answer here deletes a settings key"
            );
        }
    }

    /// The branch **field order**, pinned as a list: `code,message,value` on a save and
    /// `code,message,removed` on a delete are the reference's dict literals, and the two
    /// shapes differ in their third key — so a page that reads "the third field" is
    /// already wrong in one direction, and this row is the record of what is really there.
    #[test]
    fn each_branch_has_the_reference_s_field_order_and_key_set() {
        let scratch = Scratch::new("order");
        let setting = scratch.setting();
        let saved = set_value(&setting, Some("Network_Interface"), "en0", false);
        let keys: Vec<&String> = saved.as_object().expect("object").keys().collect();
        assert_eq!(keys, vec!["code", "message", "value"]);
        let removed = set_value(&setting, Some("Network_Interface"), "", true);
        let keys: Vec<&String> = removed.as_object().expect("object").keys().collect();
        assert_eq!(keys, vec!["code", "message", "removed"]);
        let refused = set_value(&setting, Some("nope"), "1", false);
        let keys: Vec<&String> = refused.as_object().expect("object").keys().collect();
        assert_eq!(keys, vec!["code", "message"]);
    }
}

/// `str(remove).lower() in ('1','true','yes','on')` — the reference's read of the field
/// the page posts, kept as a function so the truthiness has one owner and one test.
///
/// **No `.trim()`, and that is not an oversight.** `protocol.py:3103` lower-cases and
/// tests membership, nothing else, so `" on "` means *do not delete*. This function used
/// to trim, and its own test asserted `"  ON "` was a removal — a rule the reference does
/// not have, frozen in place by the one test that would have caught it (AGENTS.md §4.2's
/// "桩自己也会撒谎"). §29 now asks the same shape through the other door, against the
/// reference's expression.
pub fn remove_flag_is_true(raw: &str) -> bool {
    crate::pyops::form_flag_is_true(raw)
}
