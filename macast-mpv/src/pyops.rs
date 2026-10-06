//! The four places this port touches Python semantics rather than Rust's,
//! kept in one file so the divergences are countable.
//!
//! mpv's JSON IPC hands us `serde_json::Value`, and the Python renderer treats
//! those values with *Python's* rules. Two of those rules change behaviour on
//! real streams, so they are implemented here and tested here rather than being
//! accidentally written differently at each call site:
//!
//! - `if res['data']` — truthiness, not "is present".
//! - `int(res['data'])` — integer conversion, which truncates floats toward
//!   zero and **refuses** a decimal string.
//!
//! The third function here (`py_str`) is only about log lines, but log lines are
//! what the user greps when something is wrong (AGENTS.md §5), so they are worth
//! reproducing rather than re-inventing. The fourth (`type_name`) is the same
//! argument in a narrower place: it exists so that an aborted batch says what
//! Python would have said about the value that caused it.

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
}
