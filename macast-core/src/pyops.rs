//! Python's rules for reading a value — the ones this port needs in **more than one crate**.
//!
//! Port of nothing in particular: this is the small island where the reference's
//! *semantics* live rather than Rust's, so the divergences from `Value`'s own behaviour
//! are countable (AGENTS.md §4.14 / `macast-mpv/src/pyops.rs` say the same thing about
//! the renderer).
//!
//! Three rules, and the reason each is here rather than at a call site:
//!
//! * [`truthy`] — `x or y` and `if x:` in the reference are truthiness tests, not
//!   presence tests. This rule has four readers now: mpv's state branches
//!   (`if res['data']`), the management face's legacy-key fallbacks
//!   (`Setting.get(Macast_Protocols) or …`), the page resolver's `or` chains
//!   (`macast-cast::resolve`), and the 「启用国内镜像地址」 switch
//!   ([`crate::setting::github_cn_mirror`]). Four readers, one rule: a fifth crate that
//!   reimplements it differently would produce two documents about the same setting.
//! * [`type_name`] — the word an abort message prints. Python names the type
//!   (`argument of type 'NoneType' is not iterable`), Rust's `Debug` does not
//!   (`Null`), and AGENTS.md §5's whole premise is that the user greps our log lines
//!   when something is wrong. Two crates write those messages.
//! * [`form_flag_is_true`] — the form field that means "yes": the reference reads both
//!   of its switches the same way (`str(value).lower() in ('1','true','yes','on')`,
//!   `protocol.py:3103` for `remove` and `protocol.py:2493` for `set-github-mirror`).
//!   Two doors, one spelling of "on".
//!
//! **A third spelling exists and is deliberately not this function.** `?refresh=` is
//! read with a `strip()` in front of the `lower()` (`protocol.py`'s `_normalize_refresh`,
//! ported as `macast-upnp`'s `query_flag`), so `%20true%20` means yes there and means no
//! here. Two spellings, two owners, and the difference is written on both sides — a
//! "cleanup" that merged them would silently change which of the three switches a
//! padded value can flip.
//!
//! **Deliberately narrow**: the arithmetic and formatting rules (`as_int`, `clock`,
//! `py_str`, `py_repr`, `py_eq`) stayed in `macast-mpv/src/pyops.rs`, because those are
//! about what mpv's JSON IPC hands us, not about how Python reads a value.

use serde_json::{Number, Value};

/// Python's truthiness for a JSON value: `Null`/`false`/`0`/`0.0`/`""`/`[]`/`{}` are
/// falsy and everything else is truthy.
///
/// Three shapes make this necessary rather than decorative:
///
/// * A string holding a zero — `"0"` — is **truthy** in Python (`bool("0")` is `True`
///   because the string is non-empty). That is not a typo we get to fix: a user who
///   hand-edits 「高级设置」 and writes `"Github_CN_Mirror": "0"` gets the mirror *on*
///   in the shipped app, so this port must answer on too, or the two implementations
///   disagree about which URLs the settings page shows (§29 asks both sides the same
///   file).
/// * Whitespace is not stripped anywhere: `"  "` is a non-empty string, so truthy.
/// * A number is judged by value, not by text. `-1` is truthy and `0.0` is not; the
///   `i64`/`u64` arms come first so a huge integer is never decided by whether it
///   survives a trip through `f64`.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number_is_nonzero(number),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

fn number_is_nonzero(n: &Number) -> bool {
    if let Some(i) = n.as_i64() {
        return i != 0;
    }
    if let Some(u) = n.as_u64() {
        return u != 0;
    }
    match n.as_f64() {
        // Zero, and both infinities, are representable in JSON only through
        // serde_json's non-standard path; `0.0` is the case that matters.
        Some(f) => f != 0.0,
        None => true,
    }
}

/// `type(x).__name__` for the values a JSON tree can carry.
///
/// The spellings a reader greps for when an abort names its type. `json.load` gives
/// `int` for `5` and `float` for `5.0`, and serde keeps that distinction through
/// `as_i64` / `as_u64`, so the two arms have to be tried before the float one.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The reference's reading of a form field that means "yes".
///
/// `str(value).lower() in ('1', 'true', 'yes', 'on')` — verbatim, **including the part
/// that looks like an oversight**: there is no `.strip()`. `" on "` is a *different*
/// string to Python and therefore means **off**, which is the safe direction on a
/// field whose "on" answer deletes a settings key (`set-module-setting`'s `remove`) and
/// on one that rewrites every GitHub URL the app fetches. The first version of this
/// function had a `.trim()` in it and a test asserting `"  ON "` meant remove — a
/// permissiveness the reference does not have, discovered by reading
/// `protocol.py:3103` and `protocol.py:2493` rather than by a red test, so §29 now asks
/// the rule through the door against the reference's own expression.
pub fn form_flag_is_true(raw: &str) -> bool {
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The table Python answers, in both directions, for all three crates that read it.
    #[test]
    fn truthiness_is_python_s_not_presence_nor_text() {
        for falsy in [
            json!(false),
            json!(0),
            json!(0.0),
            json!(-0.0),
            json!(""),
            json!([]),
            json!({}),
            json!(null),
        ] {
            assert!(!truthy(&falsy), "{falsy} must be falsy");
        }
        for truth in [
            json!(true),
            json!(1),
            json!(-1),
            json!(0.5),
            json!("no"),
            json!("  "),
            json!([""]),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(&truth), "{truth} must be truthy");
        }
    }

    /// The one row on this list that a "sensible" implementation gets wrong: `"0"` is a
    /// non-empty string. A read that "fixes" this decides a switch differently from the
    /// shipped app for the same bytes on disk.
    #[test]
    fn a_string_zero_is_truthy_and_that_is_not_a_bug_we_get_to_fix() {
        assert!(truthy(&json!("0")), "Python: bool('0') is True");
        assert!(truthy(&json!("false")), "Python does not parse the text");
        assert!(!truthy(&json!("")), "only the empty string is falsy");
    }

    /// The float arm's own two directions. Written as a pair because `0.0` and `-0.0`
    /// are the same value to Python and a "falsy means the text is empty" implementation
    /// would answer both from the string rather than the number.
    #[test]
    fn a_float_is_judged_by_value_not_by_its_text() {
        assert!(!truthy(&json!(0.0)), "0.0 is falsy");
        assert!(!truthy(&json!(-0.0)), "-0.0 == 0.0 in Python");
        assert!(truthy(&json!(-0.5)), "a negative number is truthy");
        assert!(truthy(&json!(1e-300)), "small is not zero");
    }

    #[test]
    fn type_names_are_pythons_not_rusts() {
        assert_eq!(type_name(&json!(null)), "NoneType");
        assert_eq!(type_name(&json!(true)), "bool");
        assert_eq!(type_name(&json!("media")), "str");
        assert_eq!(type_name(&json!([1])), "list");
        assert_eq!(type_name(&json!({"a": 1})), "dict");
        // json keeps 5 and 5.0 apart, and so does Python's decoder.
        assert_eq!(type_name(&json!(5)), "int");
        assert_eq!(type_name(&json!(-5)), "int");
        assert_eq!(type_name(&json!(5.0)), "float");
        assert_eq!(type_name(&json!(-0.0)), "float");
    }

    /// The switch's own truth table, written the way the reference writes it. Every one
    /// of these rows reaches the door as **text** (a form field), and the two refusals
    /// are the ones that would be silently permissive: a stray space, and a word that
    /// merely contains one.
    /// The structural half, and the reason this file exists: **one body per rule**.
    ///
    /// Before 2026-10-20 the truthiness rule had three implementations in this
    /// workspace (here, `macast-upnp`'s `py_truthy`, `macast-cast`'s `resolve::truthy`)
    /// and the type-name rule had two (`macast-mpv/src/pyops.rs` and
    /// `module_settings::py_type_name`). None of them disagreed **today** — which is
    /// exactly why none of them was red: a duplicate that has not drifted yet is silent,
    /// and the drift arrives as a change to one copy. This scan is the question the
    /// battery could not ask, because a mutation battery kills one body at a time and
    /// the other copy answers for it.
    ///
    /// The flag rule is deliberately **not** collapsed to one: the reference has two
    /// spellings — `remove` / `set-github-mirror` (no `strip`) and `?refresh=` (a
    /// `strip()`), and they are different rules. So the claim is not "one occurrence of
    /// the four words" but "one per spelling, and the padded one is the one that pads".
    #[test]
    fn each_value_rule_has_one_body_in_the_workspace() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace layout")
            .to_path_buf();
        // Built by concatenation: this file contains the needles, and a scanner that
        // matches its own source reports a false third body (§4.14's needle lesson).
        let truthy_body = ["Value::Null => ", "false"].concat();
        let four_words = format!(
            "{} | {} | {} | {}",
            "\"1\"", "\"true\"", "\"yes\"", "\"on\""
        );
        let mut truthy_hits: Vec<String> = Vec::new();
        let mut flag_hits: Vec<(String, bool)> = Vec::new();
        let mut files = 0usize;
        for crate_name in ["macast-core", "macast-mpv", "macast-upnp", "macast-cast", "src-tauri"] {
            let dir = root.join(crate_name).join("src");
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                files += 1;
                let name = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                let lines: Vec<&str> = text.lines().collect();
                for (index, line) in lines.iter().enumerate() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("//") {
                        continue;
                    }
                    if trimmed.contains(&truthy_body) {
                        truthy_hits.push(name.clone());
                    }
                    if trimmed.contains(&four_words) {
                        // The strip is asked of the **enclosing statement**, not of this
                        // one line: `query_flag` computes `let text = value.trim()…` on
                        // the line above and matches on the line below. Reading only the
                        // matching line called both spellings "no-strip" and made this
                        // witness report a false third owner.
                        let window_start = index.saturating_sub(3);
                        let padded = lines[window_start..=index]
                            .iter()
                            .any(|near| near.contains("trim"));
                        flag_hits.push((name.clone(), padded));
                    }
                }
            }
        }
        assert!(
            files >= 40,
            "only {files} sources were read — a scan of nothing is not a pass"
        );
        truthy_hits.sort();
        truthy_hits.dedup();
        assert_eq!(
            truthy_hits,
            vec!["macast-core/src/pyops.rs".to_string()],
            "Python truthiness has more than one body in this workspace: {truthy_hits:?}"
        );
        // **Per file**, not per line. `query_flag` has two matching lines — the plain
        // `matches!` inside the function and its doc-quote of the reference at the top of
        // the file — so a per-line list reported the same owner twice under two verdicts
        // and this witness went red on a tree that had exactly the two owners it claims.
        // The question is "does this file strip", and one `trim` anywhere in it is a yes.
        flag_hits.sort();
        let mut owners: Vec<(String, bool)> = Vec::new();
        for (name, padded) in &flag_hits {
            match owners.iter_mut().find(|(seen, _)| seen == name) {
                Some(entry) => entry.1 |= *padded,
                None => owners.push((name.clone(), *padded)),
            }
        }
        let spelled: Vec<String> = owners
            .iter()
            .map(|(name, padded)| format!("{name}:{}", if *padded { "strips" } else { "no-strip" }))
            .collect();
        assert_eq!(
            spelled,
            vec![
                "macast-core/src/pyops.rs:no-strip".to_string(),
                "macast-upnp/src/http.rs:strips".to_string(),
            ],
            "the two spellings of a form flag are not one rule with two owners: {spelled:?}"
        );
    }

    #[test]
    fn the_form_flag_is_the_reference_s_four_words_lower_cased() {
        for on in ["1", "true", "TRUE", "Yes", "on", "On"] {
            assert!(form_flag_is_true(on), "{on:?} must mean on");
        }
        for off in [
            "", "0", "false", "no", "off", "1x", "remove", "y", "2", "True ", " on",
            // The row the reference does not accept and this function used to: no
            // `.strip()` exists in either of its two call sites.
            "  on ", "\ton",
        ] {
            assert!(!form_flag_is_true(off), "{off:?} must not mean on");
        }
    }
}
