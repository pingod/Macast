//! `json.loads(line)` as CPython 3.12 actually behaves — the decoder, not a
//! stand-in for it.
//!
//! Why this module exists instead of one call to `serde_json::from_str`: the
//! renderer this crate is a port of wraps its whole read loop in one `except
//! Exception` and *logs the decode error verbatim*. The sentence a user reads in
//! `macast.log` after a truncated socket is therefore part of the product, and
//! `serde_json`'s sentence ("expected value at line 1 column 3") is not CPython's
//! ("Expecting value: line 1 column 1 (char 0)") in wording, in case, or in where
//! it counts from. Reproducing the wording from a `serde_json` failure is worse
//! than the wording gap: `serde_json` and CPython disagree about *validity* on
//! several shapes (`[1,]`, `{"a":1,}`, `NaN`, a bare `1e`), and whichever one is
//! the gate decides whether the thread lives.
//!
//! So the gate here is ours and it is CPython's: [`loads`] replays the `_json.c`
//! scanner step for step over *characters*, and the error it returns is the
//! message that scanner would raise, with the position that scanner would report.
//! Every rule in this file was measured against `.venv/bin/python` — 3.12.15, the
//! interpreter the product runs — rather than read out of `json/decoder.py`,
//! because the pure-Python fallback and the C scanner disagree about several
//! positions (`[`, a control character, `\escape`) and the C one is what runs.
//!
//! The positions follow the rules `JSONDecodeError.__init__` derives them with:
//!
//! ```python
//! lineno = doc.count('\n', 0, pos) + 1
//! colno  = pos - doc.rfind('\n', 0, pos)
//! ```
//!
//! and `pos` is a **character** offset, so `中文` reports `char 0` where a byte
//! scanner would report `char 2`.
//!
//! Accepted, documented divergences — all of them about *representation* rather
//! than about validity, and all of them outside anything mpv writes:
//!
//! - `NaN`, `Infinity`, `-Infinity` parse (as they do in Python) and become
//!   [`Value::Null`], because JSON has no non-finite number and `serde_json`
//!   refuses one. Same for a float that overflows, e.g. `1e999`.
//! - A lone surrogate in `\uXXXX` becomes U+FFFD; Python keeps it and Rust's
//!   `String` cannot hold it. A surrogate *pair* written as two escapes combines
//!   into the real codepoint, exactly as Python does.
//! - An integer too wide for `i64`/`u64` becomes a float; Python has bignums.
//! - Duplicate keys: last value wins at the first key's position — which is
//!   already Python's answer (`dict` assignment) and `IndexMap`'s, so this is a
//!   note rather than a divergence.

use serde_json::{Map, Number, Value};

/// A refusal: the label CPython's scanner chose and the character offset it
/// would point at.
struct Refusal {
    label: &'static str,
    pos: usize,
}

impl Refusal {
    fn at(label: &'static str, pos: usize) -> Self {
        Refusal { label, pos }
    }

    /// `{label}: line {l} column {c} (char {pos})` — verbatim CPython.
    fn render(&self, doc: &str) -> String {
        format!(
            "{}: line {} column {} (char {})",
            self.label,
            line_of(doc, self.pos),
            column_of(doc, self.pos),
            self.pos,
        )
    }
}

/// `json.loads(line)`. Ok holds the document's value; Err holds the exact
/// sentence CPython would have raised, as the caller is expected to log it.
pub fn loads(doc: &str) -> Result<Value, String> {
    let chars: Vec<char> = doc.chars().collect();
    let mut scanner = Scanner { chars, idx: 0 };
    scanner.top_level().map_err(|err| err.render(doc))
}

struct Scanner {
    chars: Vec<char>,
    idx: usize,
}

impl Scanner {
    /// `_w(s, 0).end()` then `scan_once` then the `Extra data` check — a
    /// document is only a document when nothing but whitespace follows its value.
    fn top_level(&mut self) -> Result<Value, Refusal> {
        self.skip_ws();
        let value = self.value()?;
        self.skip_ws();
        if self.idx != self.chars.len() {
            return Err(Refusal::at("Extra data", self.idx));
        }
        Ok(value)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.idx).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.idx + offset).copied()
    }

    fn skip_ws(&mut self) {
        // Python's `WHITESPACE` is `[ \t\n\r]*` — not every Unicode space.
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.idx += 1;
        }
    }

    /// `scanner.call`'s switch on the first character.
    fn value(&mut self) -> Result<Value, Refusal> {
        match self.peek() {
            Some('"') => self.string().map(Value::String),
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('t') => self.literal("true", Value::Bool(true)),
            Some('f') => self.literal("false", Value::Bool(false)),
            Some('n') => self.literal("null", Value::Null),
            // Python's three non-standard constants. They parse; they cannot be
            // represented, and the module doc says so.
            Some('N') => self.literal("NaN", Value::Null),
            Some('I') => self.literal("Infinity", Value::Null),
            Some('-') if self.at(1) == Some('I') => {
                self.literal("-Infinity", Value::Null)
            }
            // A `-` goes to the number grammar whatever follows it, exactly as
            // `_json.c` does: `case '-': if next is 'I' ... else match_number`.
            // Routing only digits here would make every negative number in the
            // protocol unreadable, and `-x` still reports "Expecting value" at
            // the minus because `number` refuses at its own start.
            Some(c) if c.is_ascii_digit() || c == '-' => self.number(),
            // Includes EOF: an empty or whitespace-only document fails here with
            // the offset the whitespace skip left behind.
            _ => Err(Refusal::at("Expecting value", self.idx)),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, Refusal> {
        let start = self.idx;
        let chars: Vec<char> = word.chars().collect();
        if self.chars.len() - start >= chars.len() && self.chars[start..].starts_with(&chars) {
            self.idx = start + chars.len();
            Ok(value)
        } else {
            // A half-typed `tru` reports where the token *started*, not where it
            // ran out — measured, and not what a character-by-character reader
            // would guess.
            Err(Refusal::at("Expecting value", start))
        }
    }

    /// The number grammar, matched as the *longest valid prefix*. This is why
    /// `1e` yields the number `1` and the caller then reports `Extra data` at
    /// the `e` rather than a malformed-number error: CPython's regex makes the
    /// fraction and exponent groups optional as whole units, so a `.` that is
    /// not followed by a digit, or an `e` that is not followed by digits, simply
    /// does not belong to the number.
    fn number(&mut self) -> Result<Value, Refusal> {
        let start = self.idx;
        let mut cursor = start;
        let negative = self.chars.get(cursor) == Some(&'-');
        if negative {
            cursor += 1;
        }
        match self.chars.get(cursor).copied() {
            Some('0') => cursor += 1,
            Some(c) if c.is_ascii_digit() => {
                cursor += 1;
                while matches!(self.chars.get(cursor), Some(c) if c.is_ascii_digit()) {
                    cursor += 1;
                }
            }
            // `-` alone, `- x`, or nothing at all: no number starts here.
            _ => return Err(Refusal::at("Expecting value", start)),
        }
        let mut fractional = false;
        if self.chars.get(cursor) == Some(&'.')
            && matches!(self.chars.get(cursor + 1), Some(c) if c.is_ascii_digit())
        {
            fractional = true;
            cursor += 2;
            while matches!(self.chars.get(cursor), Some(c) if c.is_ascii_digit()) {
                cursor += 1;
            }
        }
        if matches!(self.chars.get(cursor), Some('e') | Some('E')) {
            let mut exponent = cursor + 1;
            if matches!(self.chars.get(exponent), Some('+') | Some('-')) {
                exponent += 1;
            }
            if matches!(self.chars.get(exponent), Some(c) if c.is_ascii_digit()) {
                fractional = true;
                cursor = exponent;
                while matches!(self.chars.get(cursor), Some(c) if c.is_ascii_digit()) {
                    cursor += 1;
                }
            }
        }
        self.idx = cursor;
        let text: String = self.chars[start..cursor].iter().collect();
        Ok(if fractional {
            float_value(&text)
        } else {
            integer_value(&text)
        })
    }

    /// `scanstring`, and the four ways it can refuse.
    fn string(&mut self) -> Result<String, Refusal> {
        let open = self.idx;
        self.idx += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                // The opening quote is the position this one reports — the rest
                // of the document is a guess about where the writer meant to
                // stop, and CPython does not guess.
                None => return Err(Refusal::at("Unterminated string starting at", open)),
                Some('"') => {
                    self.idx += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    // A backslash with nothing after it is an unterminated
                    // string, not an invalid escape — the escape never arrived.
                    if self.at(1).is_none() {
                        return Err(Refusal::at("Unterminated string starting at", open));
                    }
                    match self.at(1) {
                        Some('"') => out.push('"'),
                        Some('\\') => out.push('\\'),
                        Some('/') => out.push('/'),
                        Some('b') => out.push('\u{8}'),
                        Some('f') => out.push('\u{c}'),
                        Some('n') => out.push('\n'),
                        Some('r') => out.push('\r'),
                        Some('t') => out.push('\t'),
                        Some('u') => {
                            let code = self.hex4()?;
                            self.idx += 6;
                            out.push(self.surrogate(code)?);
                            continue;
                        }
                        // Reports the backslash, and does not name the character
                        // that follows it.
                        _ => return Err(Refusal::at("Invalid \\escape", self.idx)),
                    }
                    self.idx += 2;
                }
                // 0x7f is legal in a string; only `< 0x20` is a control
                // character to this scanner.
                Some(c) if (c as u32) < 0x20 => {
                    return Err(Refusal::at("Invalid control character at", self.idx));
                }
                Some(c) => {
                    out.push(c);
                    self.idx += 1;
                }
            }
        }
    }

    /// The four hex digits of a `\uXXXX`, refusing at the `u`.
    fn hex4(&self) -> Result<u32, Refusal> {
        let mut code: u32 = 0;
        for offset in 2..6 {
            match self.at(offset).and_then(|c| c.to_digit(16)) {
                Some(digit) => code = code * 16 + digit,
                None => return Err(Refusal::at("Invalid \\uXXXX escape", self.idx + 1)),
            }
        }
        Ok(code)
    }

    /// Turn one escape's code into a character, joining a surrogate pair if the
    /// next six characters are a low surrogate's escape. `self.idx` is already
    /// past the escape that produced `code` — the caller advanced it first — so
    /// every offset here is relative to the character *after* it.
    fn surrogate(&mut self, code: u32) -> Result<char, Refusal> {
        if !(0xd800..0xdc00).contains(&code) {
            // A plain codepoint, or an unpaired *low* surrogate. Python writes
            // the latter as-is; Rust's `String` cannot hold it, and the module
            // doc says so.
            return Ok(char::from_u32(code).unwrap_or('\u{fffd}'));
        }
        if self.at(0) != Some('\\') || self.at(1) != Some('u') {
            // `\ud800x`, `\ud800"`, `\ud800` at end of input: the high surrogate
            // is alone and the scanner carries on with whatever comes next — so
            // a `\g` after it is refused as an *escape* one character later,
            // which is exactly what the interpreter does.
            return Ok('\u{fffd}');
        }
        let mut low = 0_u32;
        for offset in 2..6 {
            match self.at(offset).and_then(|c| c.to_digit(16)) {
                Some(digit) => low = low * 16 + digit,
                // `\ud800\uZZZZ` and `\ud800\udd`: refused at the second
                // escape's `u`, the same place `hex4` points for a first escape
                // it cannot finish.
                None => return Err(Refusal::at("Invalid \\uXXXX escape", self.idx + 1)),
            }
        }
        if !(0xdc00..0xe000).contains(&low) {
            // `\ud800\udbff`: two highs. Python keeps both surrogates; we get
            // two replacement characters, one here and one when the second
            // escape is parsed on its own.
            return Ok('\u{fffd}');
        }
        self.idx += 6;
        let combined = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
        Ok(char::from_u32(combined).unwrap_or('\u{fffd}'))
    }

    fn object(&mut self) -> Result<Value, Refusal> {
        self.idx += 1;
        let mut map = Map::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.idx += 1;
            return Ok(Value::Object(map));
        }
        loop {
            // The key position admits no closing brace: `{"a":1,}` is refused
            // here rather than read as a trailing comma, and the character it
            // names is the `}` — "Expecting property name enclosed in double
            // quotes".
            if self.peek() != Some('"') {
                return Err(Refusal::at(
                    "Expecting property name enclosed in double quotes",
                    self.idx,
                ));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(Refusal::at("Expecting ':' delimiter", self.idx));
            }
            self.idx += 1;
            self.skip_ws();
            let value = self.value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.idx += 1;
                    return Ok(Value::Object(map));
                }
                Some(',') => {
                    self.idx += 1;
                    self.skip_ws();
                }
                // Even at end of input this is the delimiter complaint — a
                // container that runs out after a value wants a closer, not a
                // value, so it does not report "Expecting value".
                _ => return Err(Refusal::at("Expecting ',' delimiter", self.idx)),
            }
        }
    }

    fn array(&mut self) -> Result<Value, Refusal> {
        self.idx += 1;
        let mut items: Vec<Value> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.idx += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.idx += 1;
                    return Ok(Value::Array(items));
                }
                Some(',') => {
                    self.idx += 1;
                    self.skip_ws();
                }
                _ => return Err(Refusal::at("Expecting ',' delimiter", self.idx)),
            }
        }
    }
}

/// A float, or `Value::Null` for the ones JSON cannot hold.
fn float_value(text: &str) -> Value {
    match text.parse::<f64>() {
        Ok(number) => Number::from_f64(number).map(Value::Number).unwrap_or(Value::Null),
        // Every digit sequence this module hands here does parse; refusing with
        // the same shape as an unrepresentable value keeps the caller's contract
        // ("a document that decodes to a value") total.
        Err(_) => Value::Null,
    }
}

/// An integer, widened to a float when it exceeds every integer width — the
/// one place where Python's bignums and JSON's numbers genuinely part.
fn integer_value(text: &str) -> Value {
    if let Some(number) = text.parse::<i64>().ok().map(Number::from) {
        return Value::Number(number);
    }
    if let Some(number) = text.parse::<u64>().ok().map(Number::from) {
        return Value::Number(number);
    }
    float_value(text)
}

/// `doc.count('\n', 0, pos) + 1`.
fn line_of(doc: &str, pos: usize) -> usize {
    doc.chars().take(pos).filter(|c| *c == '\n').count() + 1
}

/// `pos - doc.rfind('\n', 0, pos)` when there is one, else `pos + 1`.
///
/// Both operands are **character** offsets: `pos` is the one the scanner kept,
/// and Python's `str.rfind` reports where in the *character* sequence the
/// newline sits. Rust's `str::rfind` reports a **byte** offset, so the two must
/// not be mixed. `["中文"]\nx` is the measured case: CPython refuses it at
/// `line 2 column 1 (char 7)`, and the newline that decides that column is
/// character 6 of the document but **byte** 10 — subtract 10 from the character
/// index 7 and this function does not return a wrong column, it underflows.
fn column_of(doc: &str, pos: usize) -> usize {
    let mut newline = None;
    for (index, c) in doc.chars().take(pos).enumerate() {
        if c == '\n' {
            newline = Some(index);
        }
    }
    match newline {
        Some(at) => pos - at,
        None => pos + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expectation in these two tables was printed by
    /// `.venv/bin/python -c 'json.loads(...)'` on 2026-10-06 — 3.12.15, the
    /// interpreter the product runs. They are transcriptions, not deductions:
    /// the pure-Python decoder in `json/decoder.py` disagrees with the C scanner
    /// about several of these positions, and the C scanner is what runs.
    fn reject(doc: &str) -> String {
        loads(doc).expect_err("this document must be refused")
    }

    #[test]
    fn the_five_wordings_and_where_each_points() {
        for (doc, expected) in [
            ("x", "Expecting value: line 1 column 1 (char 0)"),
            ("", "Expecting value: line 1 column 1 (char 0)"),
            (" ", "Expecting value: line 1 column 2 (char 1)"),
            ("+1", "Expecting value: line 1 column 1 (char 0)"),
            (".5", "Expecting value: line 1 column 1 (char 0)"),
            ("-", "Expecting value: line 1 column 1 (char 0)"),
            ("- 1", "Expecting value: line 1 column 1 (char 0)"),
            ("tru", "Expecting value: line 1 column 1 (char 0)"),
            ("tRue", "Expecting value: line 1 column 1 (char 0)"),
            // `null` parses and the comma is extra data; `nul` never parses, so
            // the complaint is about the value and not what follows it.
            ("null,", "Extra data: line 1 column 5 (char 4)"),
            ("nul,", "Expecting value: line 1 column 1 (char 0)"),
            ("1 2", "Extra data: line 1 column 3 (char 2)"),
            ("1  2", "Extra data: line 1 column 4 (char 3)"),
            ("1x", "Extra data: line 1 column 2 (char 1)"),
            // The number grammar backtracks to the longest valid prefix, so what
            // follows is extra data rather than a malformed number.
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("1.", "Extra data: line 1 column 2 (char 1)"),
            ("1e", "Extra data: line 1 column 2 (char 1)"),
            ("1e+", "Extra data: line 1 column 2 (char 1)"),
            ("1.2.3", "Extra data: line 1 column 4 (char 3)"),
            ("{\"a\": 1}x", "Extra data: line 1 column 9 (char 8)"),
            ("{\"a\": 1} x", "Extra data: line 1 column 10 (char 9)"),
            ("truefalse", "Extra data: line 1 column 5 (char 4)"),
            ("{\"a\":1}}", "Extra data: line 1 column 8 (char 7)"),
        ] {
            assert_eq!(reject(doc), expected, "for {doc:?}");
        }
    }

    #[test]
    fn a_container_refuses_at_the_character_that_was_not_the_closer() {
        for (doc, expected) in [
            // After a value: the delimiter complaint, at the offending character
            // — including when that character is the *other* container's closer.
            ("[1,2}]", "Expecting ',' delimiter: line 1 column 5 (char 4)"),
            (
                r#"{"a": [1,2}]x"#,
                "Expecting ',' delimiter: line 1 column 11 (char 10)",
            ),
            (
                r#"{"a": [1,2]x}"#,
                "Expecting ',' delimiter: line 1 column 12 (char 11)",
            ),
            ("{\"a\":1]x", "Expecting ',' delimiter: line 1 column 7 (char 6)"),
            ("{\"a\":\"b\"]}x", "Expecting ',' delimiter: line 1 column 9 (char 8)"),
            ("[1", "Expecting ',' delimiter: line 1 column 3 (char 2)"),
            ("[[1]", "Expecting ',' delimiter: line 1 column 5 (char 4)"),
            // After a comma there is no closer to find: the element or key
            // position demands a value, and a `}` or `]` sitting there is simply
            // not one. This is why neither container admits a trailing comma.
            ("[1e5,]", "Expecting value: line 1 column 6 (char 5)"),
            ("[1,", "Expecting value: line 1 column 4 (char 3)"),
            ("[,]", "Expecting value: line 1 column 2 (char 1)"),
            ("[1 ,, 2]", "Expecting value: line 1 column 5 (char 4)"),
            ("{\"a\":}", "Expecting value: line 1 column 6 (char 5)"),
            ("{\"a\": }", "Expecting value: line 1 column 7 (char 6)"),
            ("{\"a\":1, }", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("{\"a\":1 ,}", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("{\"a\":1,2", "Expecting property name enclosed in double quotes: line 1 column 8 (char 7)"),
            ("{\"a\":1,", "Expecting property name enclosed in double quotes: line 1 column 8 (char 7)"),
            ("{\"a\":1, ", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("{,}", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("{1:2}", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("[\"a\"", "Expecting ',' delimiter: line 1 column 5 (char 4)"),
        ] {
            assert_eq!(reject(doc), expected, "for {doc:?}");
        }
    }

    #[test]
    fn the_colon_and_the_string_refusals() {
        for (doc, expected) in [
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("{\"a\"", "Expecting ':' delimiter: line 1 column 5 (char 4)"),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            // A lone backslash at the end is an unfinished string, not an
            // unfinished escape.
            (
                "\"abc\\",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            (
                "\"abc\\\"",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            (r#""abc\d""#, "Invalid \\escape: line 1 column 5 (char 4)"),
            (r#""\uZZZZ""#, "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            (r#""\u12""#, "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            (
                "\"a\tb\"",
                "Invalid control character at: line 1 column 3 (char 2)",
            ),
            (
                "\"a\nb\"",
                "Invalid control character at: line 1 column 3 (char 2)",
            ),
        ] {
            assert_eq!(reject(doc), expected, "for {doc:?}");
        }
    }

    #[test]
    fn positions_are_characters_and_lines_are_counted_from_the_newlines() {
        // A byte scanner would say `char 2` for the first of these; the decoder
        // that produced the message the user reads says `char 0`.
        assert_eq!(
            reject("中文"),
            "Expecting value: line 1 column 1 (char 0)"
        );
        assert_eq!(
            reject("{\n  \"a\": 1,\n  x}"),
            "Expecting property name enclosed in double quotes: line 3 column 3 (char 14)",
        );
        assert_eq!(
            reject("[1,\n]"),
            "Expecting value: line 2 column 1 (char 4)",
        );
        // The column after a newline is a *character* distance too. These two
        // are the ones a byte scanner gets wrong: the newline that decides each
        // of them sits ten and five **bytes** into its prefix but six and four
        // **characters** in, so the byte answer is a column of 0 for the second
        // document and an underflow for the first.
        assert_eq!(
            reject("[\"中文\"]\nx"),
            "Extra data: line 2 column 1 (char 7)",
        );
        assert_eq!(
            reject("[\"é\"\nx]"),
            "Expecting ',' delimiter: line 2 column 1 (char 5)",
        );
        assert_eq!(reject("{\"中文\": }"), "Expecting value: line 1 column 8 (char 7)");
        assert_eq!(column_of("[\"中文\"]\nx", 7), 1);
        assert_eq!(line_of("[\"中文\"]\nx", 7), 2);
        assert_eq!(line_of("ab\ncd", 4), 2);
        assert_eq!(column_of("ab\ncd", 4), 2);
        assert_eq!(column_of("ab\ncd", 0), 1);
        assert_eq!(line_of("", 0), 1);
    }

    #[test]
    fn a_surrogate_pair_reaches_the_second_escape_before_it_is_judged() {
        // The high half was already consumed, so `idx` stands on the second
        // backslash, and CPython names the `u` one character later.
        assert_eq!(
            reject("{\"a\": \"\\ud800\\uZZZZ\"}"),
            "Invalid \\uXXXX escape: line 1 column 15 (char 14)",
        );
    }

    #[test]
    fn the_documents_that_do_parse_are_the_documents_python_agrees_with() {
        for doc in [
            "{}",
            "{ }",
            "[]",
            " [ ] ",
            "true",
            "false",
            "null",
            "0",
            "-0",
            "0.0",
            "-0.1e2",
            "1E5",
            "1e-02",
            "{\"a\": [1, 2] }",
            "[[]]",
            "[1, [2]]",
            "{\"b\":1,\"a\":[true,null,{\"c\":2}]}",
            "[\"a\\tb\", \"\\\\\", \"\\/\", \"\\u00e9\"]",
            "\"\\ud800\\udc00\"",
            "  {\"id\": 1, \"error\": null}  ",
        ] {
            assert!(loads(doc).is_ok(), "{doc:?} must decode");
        }
    }

    #[test]
    fn python_three_non_standard_numbers_parse_and_cannot_be_held() {
        // Validity first: Python accepts all of these, so the gate must too — a
        // gate that refuses them turns a document the reference would have
        // logged into a decode error. Representation second: JSON has nowhere to
        // put a non-finite number, so they arrive as null.
        for doc in ["NaN", "Infinity", "-Infinity", "[NaN]", "1e999", "-1e999"] {
            assert!(loads(doc).is_ok(), "{doc:?} must decode");
        }
        assert_eq!(loads("NaN").unwrap(), Value::Null);
        assert_eq!(loads("Infinity").unwrap(), Value::Null);
        assert_eq!(loads("-Infinity").unwrap(), Value::Null);
        assert_eq!(loads("1e999").unwrap(), Value::Null);
        assert_eq!(loads("[1e999]").unwrap(), json_array([Value::Null]));
    }

    #[test]
    fn a_lone_surrogate_becomes_the_replacement_character_and_a_pair_does_not() {
        assert_eq!(loads(r#""\ud800""#).unwrap(), Value::String("\u{fffd}".into()));
        assert_eq!(
            loads(r#""\ud800x""#).unwrap(),
            Value::String("\u{fffd}x".into()),
        );
        assert_eq!(
            loads(r#""\ud83d\ude00""#).unwrap(),
            Value::String("😀".into()),
        );
        assert_eq!(
            loads(r#""\udc00""#).unwrap(),
            Value::String("\u{fffd}".into()),
        );
    }

    #[test]
    fn duplicate_keys_take_the_last_value_at_the_first_place() {
        let value = loads(r#"{"b": 1, "a": 1, "b": 2}"#).unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(map["b"], Value::from(2));
    }

    #[test]
    fn integers_widen_to_float_only_past_every_integer_width() {
        assert_eq!(loads("42").unwrap(), Value::from(42_i64));
        assert_eq!(loads("-42").unwrap(), Value::from(-42_i64));
        assert_eq!(loads("18446744073709551615").unwrap(), Value::from(u64::MAX));
        let wide = loads("99999999999999999999999").unwrap();
        assert!(wide.as_f64().is_some(), "beyond i64 and u64 it is a float");
        // Python keeps it exactly (`int` is a bignum); JSON has nowhere to put it.
        assert_eq!(wide.as_f64(), Some(1e23));
    }

    fn json_array(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(items.into_iter().collect())
    }

    #[test]
    fn the_escapes_that_exist_reach_the_characters_they_name() {
        assert_eq!(
            loads(r#""\"\\\/\b\f\n\r\t""#).unwrap(),
            Value::String("\"\\/\u{8}\u{c}\n\r\t".into()),
        );
        assert_eq!(loads("\"é\"").unwrap(), Value::String("é".into()));
    }
}
