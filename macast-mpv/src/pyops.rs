//! The five places this port touches Python semantics rather than Rust's,
//! kept in one file so the divergences are countable.
//!
//! mpv's JSON IPC hands us `serde_json::Value`, and the Python renderer treats
//! those values with *Python's* rules. Three of those rules change behaviour on
//! real streams, so they are implemented here and tested here rather than being
//! accidentally written differently at each call site:
//!
//! - `if res['data']` — truthiness, not "is present".
//! - `int(res['data'])` — integer conversion, which truncates floats toward
//!   zero and **refuses** a decimal string.
//! - `player_size == 4` — equality, which is `False` for the string `"4"` and
//!   `True` for the number `4.0`. Rust's `PartialEq` on `Value` would answer
//!   `False` for both, so the fullscreen case would silently stop firing for
//!   every machine whose setting was written by a float-emitting serializer.
//!
//! The remaining two (`py_str`, `type_name`) are only about the words that reach
//! the log. Log lines are what the user greps when something is wrong
//! (AGENTS.md §5), and an abort is far easier to read when it names its type the
//! way Python's own `TypeError` did, so both are reproduced rather than
//! re-invented.

use serde_json::{Number, Value};

/// Python's truthiness for a JSON value.
///
/// The shape that makes this necessary: mpv reports `pause` as a real boolean,
/// but the Python branch is `if res['data'] and res['data'] is not None`, which
/// is a truthiness test wearing a null check. A `0`, an empty track list, or an
/// empty string all take the `else` arm — which for `pause` means "the player is
/// playing", so reading presence instead of truth would report the wrong
/// transport state to every control point.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => number_is_nonzero(n),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
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

/// Python's `int(value)`, for the values that arrive over mpv's IPC.
///
/// `None` means "Python would have raised here" — and the caller must treat that
/// as *the whole batch of pending lines aborting*, not as "skip this field".
/// `start_ipc` wraps `for msg in msgs: self.update_state(msg)` in one
/// `try/except`, so a bad value in the second line of a chunk swallows the third
/// and fourth as well. Porting that faithfully is the difference between our log
/// saying "decode error" at the same moment Python's does and saying it never.
///
/// The accepted shapes, each measured against CPython:
///
/// - integers pass through;
/// - floats **truncate toward zero** (`int(-0.9) == 0`, `int(1.9) == 1`);
/// - `True`/`False` are `1`/`0`;
/// - strings must be an optional sign followed by digits only — `int("45")` is
///   45, `int("45.5")` raises `ValueError`, and `int("")` raises too.
///
/// Deliberate divergence, one case: a float outside `i64`'s range returns `None`
/// here while Python's arbitrary-precision int would keep going. mpv reports
/// positions and durations in seconds as doubles; nothing that reaches this
/// function is anywhere near 9.2e18.
pub fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Some(i);
            }
            if let Some(u) = n.as_u64() {
                return i64::try_from(u).ok();
            }
            n.as_f64().map(|f| f.trunc() as i64)
        }
        Value::String(s) => {
            let text = s.trim();
            let (sign, digits) = match text.strip_prefix('-') {
                Some(rest) => (-1i64, rest),
                None => (1, text.strip_prefix('+').unwrap_or(text)),
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse::<i64>().ok().map(|v| sign * v)
        }
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// `type(value).__name__` for the values that arrive over mpv's IPC.
///
/// This exists so that a message about a failed conversion can name the type the
/// way Python's own `TypeError` did: `int() argument must be a string or a
/// number, not 'NoneType'`, `list indices must be integers or slices, not str`.
/// A reader who greps the old log finds the new one by matching that sentence,
/// so the name has to be Python's spelling — `NoneType` rather than `Null`,
/// `bool` rather than `Bool`, and an integral JSON number reporting `int` where
/// Rust's type would say `Number`.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        // Python's json decoder gives `int` for `5` and `float` for `5.0`; the
        // two shapes are distinguishable here because serde_json keeps them
        // distinguishable.
        Value::Number(n) => {
            if n.as_i64().is_some() || n.as_u64().is_some() {
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

/// The clock string mpv's numbers become on the wire: `'%d:%02d:%02d'`.
///
/// Reproduced exactly, including the part that looks like a bug. Python's `//`
/// floors and `%` stays non-negative, so a negative position — which mpv does
/// report for a live stream behind the wall clock — formats as `-1:59:55`, not
/// `-1:-5:-5`. A whole `-3661` is `-2:58:59`: the hours floor *away from zero*
/// while the minutes and seconds come from the non-negative remainder, so the
/// three columns do not describe the same sign.
/// [`macast_core::timefmt::dlna_clock`] is a *different* function
/// (it clamps negatives to zero) and belongs to the DIDL side; the two must not
/// be collapsed into one helper, because the DLNA description and the renderer's
/// own `RelativeTimePosition` genuinely do not agree about negative input.
pub fn clock(secs: i64) -> String {
    format!(
        "{}:{:02}:{:02}",
        secs.div_euclid(3600),
        secs.rem_euclid(3600).div_euclid(60),
        secs.rem_euclid(60)
    )
}

/// Python's `==` on two decoded JSON values.
///
/// This exists because one comparison in the renderer decides whether the player
/// goes fullscreen, and it is written with `==` against a value that came from a
/// settings file a human may have edited by hand
/// (`macast_renderer/mpv.py`: `player_size == SettingProperty.PlayerSize_FullScreen.value`).
/// Rust's `PartialEq` on `Value` is *not* Python's `==`, and the difference goes
/// both ways: `json!(4.0) == json!(4)` is `false` here but `true` in Python, so a
/// naive port would drop fullscreen for a machine whose `macast_setting.json` was
/// written by a serializer that emits whole floats; and `Value` compares object
/// keys with no notion of "a `bool` is a `1`", where `True == 1` is `true` in
/// Python. Measured on CPython 3.12.15, all twelve of these on the same
/// interpreter that runs the product:
///
/// ```text
/// 4 == 4          True        '4' == 4          False       None == 4         False
/// True == 1       True        1 == True         True        4.0 == 4          True
/// [4] == 4        False       {'a': 4} == 4     False       0 == False      True
/// 1.0 == True     True        '4.0' == 4        False       4 == 5          False
/// ```
///
/// Two shapes are worth more than the table. A **string never equals a number**
/// (`'4' == 4` is `False`), which is the whole point: a hand-typed
/// `{"PlayerSize": "4"}` in the advanced-settings JSON must not put the player
/// into fullscreen, and the verdict has to come from the comparison itself
/// rather than from whoever happened to write the value. And container equality
/// is **element-wise with the same rules** — `[1, {'a': 2.0}] == [1.0, {'a': 2}]`
/// is `True`, with dicts matched by key rather than by order.
///
/// **The remaining divergence lives in one arm, and it is the decoder's, not the
/// comparison's.** An integer against a float is compared exactly at *every*
/// magnitude: a float carrying a fraction equals no integer, and a whole float
/// outside `i64` is by definition past one end of every integer an `i64` can be,
/// so `9007199254740993 == 9007199254740992.0` and
/// `9223372036854775807 == 9223372036854775808.0` both read `False` here, as they
/// do in Python. What is left is two numbers that *both* fail to hold an `i64` —
/// a `u64` above `i64::MAX`, or a wire integer so long that `serde_json` already
/// stored it as a `f64` (CPython's decoder keeps arbitrary precision). Those are
/// compared as `f64`, so two distinct integers above `2**53` can read as equal
/// where Python says they are not. Nothing in this protocol reaches that range;
/// it is written down so the next reader does not mistake the boundary for the
/// rule.
pub fn py_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        // A bool is a number to Python's `==`, and only the two values `0` and
        // `1` — so comparing it numerically is the whole rule.
        (Value::Bool(a), other) | (other, Value::Bool(a)) => {
            compare_number(&Value::Number((*a as i64).into()), other)
        }
        (Value::Number(a), Value::Number(b)) => compare_numbers(a, b),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            // Order does not matter, and it must not: `preserve_order` is what
            // makes the *wire* readable, and a setting dict read in a different
            // order is still the same setting.
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|o| py_eq(v, o)))
        }
        // Everything else is a different type: Python's `==` between `str` and a
        // number, or between a container and anything that is not its own kind,
        // is simply `False` with no coercion attempted.
        _ => false,
    }
}

/// `==` between two numbers, reached only from paths that already know the
/// values are numeric (or bool, which is numeric here).
fn compare_number(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => compare_numbers(a, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq(x, y))
        }
        _ => false,
    }
}

fn compare_numbers(a: &Number, b: &Number) -> bool {
    match (int_of(a), int_of(b)) {
        (Some(x), Some(y)) => x == y,
        (Some(x), None) => float_equals_int(b, x),
        (None, Some(y)) => float_equals_int(a, y),
        // Neither holds an `i64`, so both are floats: `f64` comparison *is* the
        // exact comparison here, because a decoded float already is a `f64`.
        (None, None) => a.as_f64() == b.as_f64(),
    }
}

/// The integer a `serde_json` number holds, if it holds one.
fn int_of(n: &Number) -> Option<i64> {
    n.as_i64().or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))
}

/// Does the number `n` — which holds no `i64`, so it is a float, possibly with a
/// fraction, possibly whole but outside `i64` — equal the integer `i`?
fn float_equals_int(n: &Number, i: i64) -> bool {
    let Some(f) = n.as_f64() else { return false };
    // `whole_int` is exact at every magnitude, so its `None` is a real answer
    // rather than a gap: a float that carries a fraction equals no integer, and
    // a whole float outside `i64` is by definition past one end of every integer
    // `i` can be. Falling back to `i as f64` would be the lossy path — it is what
    // makes `9223372036854775807` against `9223372036854775808.0` read as equal,
    // where Python says they are not.
    whole_int(f).is_some_and(|j| j == i)
}

/// The integer a float is *exactly*, if it is a whole number an `i64` can hold.
/// The saturating cast alone is not the range check — `1e30 as i64` hands back
/// `i64::MIN` — so the round trip is what rejects a float that does not fit, and
/// `fract` is what rejects one that isn't whole. This is also why
/// `9007199254740993 == 9007199254740992.0` stays `False`: the float on the
/// right *is* exactly `…992`, so it never becomes `…993`.
fn whole_int(f: f64) -> Option<i64> {
    if !f.is_finite() || f.fract() != 0.0 {
        return None;
    }
    // The upper bound is strict because `i64::MAX as f64` *is* 2^63 — that cast
    // rounds up. Without it, the saturating cast below would read
    // `9223372036854775808.0` as `i64::MAX`, round-trip it back to the same
    // float, and answer `True` to a comparison Python answers `False`.
    if !(f >= (i64::MIN as f64) && f < (i64::MAX as f64)) {
        return None;
    }
    let i = f as i64;
    (i as f64 == f).then_some(i)
}

/// Python's `str()` on a decoded JSON value — what `logger.info(res)` and
/// `logger.debug("send command: " + str(command))` put in the log.
///
/// Not a Rust `Debug` impl: those differ in ways a reader notices. `str(list)`
/// quotes its *elements* with `'` but `str(str)` adds no quotes at all, `True`
/// is not `true`, `None` is not `null`, and a whole-number float keeps its `.0`
/// (`str(12.0)` is `'12.0'`, while Rust prints `12` — the one case here that has
/// to be patched by hand, because serde_json gives us a float and Rust's
/// formatter would silently rewrite the log line).
pub fn py_str(value: &Value) -> String {
    match value {
        // `str("x")` is `x` — the quotes belong to `repr`, and `str()` of a
        // container calls `repr()` on what it holds.
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr()` on a decoded JSON value.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => repr_number(n),
        Value::String(s) => repr_string(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_string(k), py_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn repr_number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) => {
            if f == f.trunc() && f.is_finite() {
                // Python's float repr keeps one decimal on integral values.
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        None => n.to_string(),
    }
}

/// `repr()` of a string: single quotes, unless it contains one and not a double.
///
/// Escapes are Python's, not Rust's: `\t`, `\n`, `\r` are spelled out, and
/// other control characters use `\xNN`. Python's own rule for the quote choice
/// is "if `'` in s and `"` not in s, use double quotes".
fn repr_string(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness_is_not_presence() {
        // The `pause` branch's whole point: these all take the `else` arm.
        for falsy in [json!(false), json!(0), json!(0.0), json!(""), json!([]), json!({}), json!(null)] {
            assert!(!truthy(&falsy), "{falsy} must be falsy");
        }
        for truthy_value in [json!(true), json!(1), json!(-1), json!("no"), json!([0]), json!({"a": 0})] {
            assert!(truthy(&truthy_value), "{truthy_value} must be truthy");
        }
    }

    #[test]
    fn int_truncates_toward_zero_and_refuses_decimals() {
        assert_eq!(as_int(&json!(1.9)), Some(1));
        assert_eq!(as_int(&json!(-0.9)), Some(0));
        assert_eq!(as_int(&json!(-1.9)), Some(-1));
        assert_eq!(as_int(&json!(true)), Some(1));
        assert_eq!(as_int(&json!(false)), Some(0));
        assert_eq!(as_int(&json!(" 45 ")), Some(45));
        assert_eq!(as_int(&json!("-7")), Some(-7));
        assert_eq!(as_int(&json!("+7")), Some(7));
        // CPython raises for each of these; `None` is the honest answer.
        for nope in [json!("45.5"), json!(""), json!("  "), json!("0x10"), json!("abc"), json!(null), json!([1]), json!([]), json!({})] {
            assert_eq!(as_int(&nope), None, "{nope} must be rejected");
        }
    }

    #[test]
    fn clock_floors_like_python_not_like_dlna_clock() {
        assert_eq!(clock(0), "0:00:00");
        assert_eq!(clock(12), "0:00:12");
        assert_eq!(clock(3725), "1:02:05");
        assert_eq!(clock(86_399), "23:59:59");
        // A negative position is what mpv reports for a lagging live stream, and
        // Python's floor-division shape is the one the reference logs show.
        // Measured against `.venv/bin/python` (3.12.15) with the reference's own
        // expression, `'%d:%02d:%02d' % (sec // 3600, (sec % 3600) // 60, sec % 60)`.
        assert_eq!(clock(-5), "-1:59:55");
        assert_eq!(clock(-3600), "-1:00:00");
        // The hours and the minutes do not floor the same way; this is the case
        // that shows they cannot both be read off one division.
        assert_eq!(clock(-3661), "-2:58:59");
        // And it is deliberately *not* what the DLNA formatter does.
        assert_eq!(macast_core::timefmt::dlna_clock(-5.0), "0:00:00");
    }

    #[test]
    fn str_and_repr_follow_pythons_quote_and_number_rules() {
        assert_eq!(py_str(&json!("media")), "media");
        assert_eq!(py_repr(&json!("media")), "'media'");
        assert_eq!(
            py_str(&json!(["loadfile", "http://x/y", "replace"])),
            "['loadfile', 'http://x/y', 'replace']"
        );
        assert_eq!(py_str(&json!({"id": 5, "data": null})), "{'id': 5, 'data': None}");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(12)), "12");
        // The hand-patched case: Rust would print `12` for this float.
        assert_eq!(py_str(&json!(12.0)), "12.0");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        // A string holding an apostrophe switches to double quotes, like repr().
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!("a\nb")), "'a\\nb'");
    }

    #[test]
    fn type_names_are_pythons_not_rusts() {
        // The spellings a reader greps for when an abort names its type.
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

    #[test]
    fn object_repr_keeps_the_order_the_wire_had() {
        // `preserve_order` is what makes this true; without it the two inner
        // entries would come back alphabetically and the log line would be a
        // paraphrase of Python's rather than Python's.
        let value = json!({"z": 1, "a": 2});
        assert_eq!(py_str(&value), "{'z': 1, 'a': 2}");
    }

    #[test]
    fn equality_is_pythons_not_rusts() {
        // The twelve rows from the doc, measured on CPython 3.12.15 and written
        // here in the same order, so a reader can put the two side by side.
        assert!(py_eq(&json!(4), &json!(4)));
        assert!(!py_eq(&json!("4"), &json!(4)));
        assert!(!py_eq(&json!(null), &json!(4)));
        assert!(py_eq(&json!(true), &json!(1)));
        assert!(py_eq(&json!(1), &json!(true)));
        assert!(py_eq(&json!(4.0), &json!(4)));
        assert!(!py_eq(&json!([4]), &json!(4)));
        assert!(!py_eq(&json!({"a": 4}), &json!(4)));
        assert!(py_eq(&json!(0), &json!(false)));
        assert!(py_eq(&json!(1.0), &json!(true)));
        assert!(!py_eq(&json!("4.0"), &json!(4)));
        assert!(!py_eq(&json!(4), &json!(5)));
        // Rust's own `PartialEq` on `Value` gets the fifth row wrong, and that is
        // the row the fullscreen case turns on: a settings file written by a
        // serializer that emits whole floats would stop fullscreening.
        assert_ne!(json!(4.0), json!(4));
        assert!(py_eq(&json!(4.0), &json!(4)));
    }

    #[test]
    fn a_string_never_equals_a_number() {
        // The other half of the same verdict, stated on its own because it is the
        // one that protects a user: `{"PlayerSize": "4"}` typed by hand into the
        // advanced-settings JSON must not put the player into fullscreen, and the
        // answer has to come from the comparison rather than from whoever wrote
        // the value.
        for text in ["4", " 4", "4 ", "04", "4.0", "true"] {
            assert!(
                !py_eq(&json!(text), &json!(4)),
                "{text:?} must not equal the number 4"
            );
        }
        // And it is not merely "strings are never equal to anything": two strings
        // do compare, which is why this is a type rule and not a refusal.
        assert!(py_eq(&json!("4"), &json!("4")));
    }

    #[test]
    fn containers_compare_elementwise_and_dicts_by_key() {
        // `[1, {'a': 2.0}] == [1.0, {'a': 2}]` is `True` in Python: the same
        // number rules apply all the way down, and a dict is matched by key.
        assert!(py_eq(&json!([1, { "a": 2.0 }]), &json!([1.0, { "a": 2 }])));
        // Order of dict entries does not matter — `preserve_order` is for the
        // wire, not for identity.
        assert!(py_eq(&json!({"z": 1, "a": 2}), &json!({"a": 2, "z": 1})));
        // Order in a list does.
        assert!(!py_eq(&json!([1, 2]), &json!([2, 1])));
        // A container only equals its own kind, at any depth.
        assert!(!py_eq(&json!([[4]]), &json!([4])));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        // A bool inside a container keeps the bool-is-a-number rule.
        assert!(py_eq(&json!([true]), &json!([1])));
    }

    #[test]
    fn int_and_float_compare_exactly_inside_i64() {
        // The boundary the doc points at: a whole float inside `i64` is compared
        // as *that integer*, so the first value that `f64` cannot represent is
        // also the first one Python calls unequal. Reading both through `f64` —
        // what a naive port does — turns this into `True`.
        assert!(!py_eq(
            &json!(9007199254740993i64),
            &json!(9007199254740992.0)
        ));
        assert!(py_eq(
            &json!(9007199254740992i64),
            &json!(9007199254740992.0)
        ));
        // A float with a fraction equals no integer, in either direction, and
        // that must not be reached by rounding something large down first.
        assert!(!py_eq(&json!(2), &json!(1.5)));
        assert!(!py_eq(&json!(-3), &json!(-2.5)));
        // The two ends of the range, where the saturating cast would otherwise
        // claim a float that does not fit *is* `i64::MAX`.
        assert!(py_eq(&json!(i64::MIN), &json!(-9223372036854775808.0)));
        assert!(!py_eq(&json!(i64::MAX), &json!(9223372036854775808.0)));
        assert!(!py_eq(&json!(0), &json!(1e30)));
    }
}
