//! The reader the page-scraping layer feeds HTML into: a port of CPython
//! 3.12.15 `html/parser.py` (`HTMLParser`), character for character.
//!
//! Why a port instead of `scraper`/`lol_html`: this layer's job is to report
//! *the same hrefs for the same broken markup*. A spec-conforming HTML5 parser
//! is a different reader — it would fix the tags `media_resolve.py` reads as
//! garbage, and then the candidate table would silently disagree with the
//! product that every prior sublayer of this crate is being proved against.
//! So the tolerance is the feature, and it is only a feature if it is exact:
//! every arm below is transcribed from the stdlib file, including the ones that
//! look like accidents (an end tag parsed from `</div <span>`, a bare attribute
//! value that backtracks into the whitespace after `=`, `</>` swallowing itself
//! in silence).
//!
//! Two deviations, both deliberate and both recorded here:
//!
//! * `convert_charrefs` is pinned `true`. The Python class defaults to `true`
//!   and `media_resolve.py` never passes it, so the `&#`/`&` arms of `goahead`
//!   — and `handle_charref`/`handle_entityref` — are dead code in this crate.
//!   The port keeps the arms as an early `break` with this note instead of a
//!   panic, because `assert 0, "interesting.search() lied"` is an assertion
//!   about a pattern the port never builds.
//! * The pending/threshold accumulator in `feed()` is dropped. It is a
//!   quadratic-rescan guard: with `_parse_threshold == 1` at reset, any
//!   non-empty feed parses immediately, and `close()` flushes what is left, so
//!   `(feed…; close)` and `(feed…)` both reach exactly the state this port
//!   reaches by appending to `rawdata` and running `goahead`.
//!
//! Indexing follows the Python: `rawdata` is a `Vec<char>`, so every offset
//! below is a *character* offset, not a byte offset. UTF-8 pages with Chinese
//! titles are the case that makes this load-bearing.

/// `HTMLParser.CDATA_CONTENT_ELEMENTS` — parsed in RAWTEXT mode: the closing
/// tag is the only thing special inside, and the bytes go to `handle_data`
/// without entity unescaping.
const CDATA_CONTENT_ELEMENTS: [&str; 6] =
    ["script", "style", "xmp", "iframe", "noembed", "noframes"];

/// `HTMLParser.RCDATA_CONTENT_ELEMENTS` — the same "only the closing tag is
/// special" rule, but character references *are* still resolved.
const RCDATA_CONTENT_ELEMENTS: [&str; 2] = ["textarea", "title"];

/// What the parser calls when it finds markup. The three required methods are
/// the three `media_resolve.py` overrides; the rest default to the stdlib's
/// `pass` bodies, and `handle_startendtag` defaults to the stdlib's own
/// definition — start, then end, and **no** CDATA mode either way.
pub trait HtmlSink {
    fn handle_starttag(&mut self, tag: &str, attrs: &[(String, Option<String>)]);
    fn handle_endtag(&mut self, tag: &str);
    fn handle_data(&mut self, data: &str);

    fn handle_comment(&mut self, _data: &str) {}
    fn handle_decl(&mut self, _decl: &str) {}
    fn handle_pi(&mut self, _data: &str) {}
    fn unknown_decl(&mut self, _data: &str) {}

    fn handle_startendtag(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        self.handle_starttag(tag, attrs);
        self.handle_endtag(tag);
    }
}

// --- character classes, straight from the regexes -------------------------

/// `[\t\n\r\f ]` — the *five*-character class the parser's regexes use. This is
/// NOT Python's `str.strip()`/`str.split()` set (29 code points), and the two
/// predicates live side by side inside `parse_starttag`: the attribute scanner
/// uses these five, the `end = rawdata[k:endpos].strip()` uses all 29. Nothing
/// may unify them.
///
/// (`\x0c` is form feed: Rust has no `\f` character escape, so every `[\t\n\r\f ]`
/// in the Python source reads back as this one class here.)
#[inline]
fn is_ws5(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\x0c' | ' ')
}

/// `[\t\n\r\f />]` as a membership test.
#[inline]
fn is_ws5_slash_gt(c: char) -> bool {
    is_ws5(c) || c == '/' || c == '>'
}

/// `[\t\n\r\f /]` as a membership test.
#[inline]
fn is_ws5_or_slash(c: char) -> bool {
    is_ws5(c) || c == '/'
}

/// The lookbehind `(?<=['"\t\n\r\f /])` of the attribute-name patterns.
#[inline]
fn is_attr_lookbehind(c: char) -> bool {
    matches!(c, '\'' | '"' | '\t' | '\n' | '\r' | '\x0c' | ' ' | '/')
}

#[inline]
fn is_ascii_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// `(?i)` under `re.ASCII`: ASCII letters fold, everything else must be equal.
#[inline]
fn ascii_fold_eq(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    a.is_ascii_alphabetic() && b.is_ascii_alphabetic() && a.to_ascii_lowercase() == b.to_ascii_lowercase()
}

fn starts_with(raw: &[char], i: usize, lit: &str) -> bool {
    let l = lit.chars().collect::<Vec<char>>();
    i + l.len() <= raw.len() && raw[i..i + l.len()] == l[..]
}

/// `rawdata[i:j]` as an owned `String`.
fn slice(raw: &[char], i: usize, j: usize) -> String {
    let hi = j.min(raw.len());
    if i >= hi {
        return String::new();
    }
    raw[i..hi].iter().collect()
}

fn find_char(raw: &[char], c: char, from: usize) -> Option<usize> {
    (from..raw.len()).find(|&p| raw[p] == c)
}

/// `str.rfind(needle, start)` — the rightmost hit at or after `start`.
fn rfind_char(raw: &[char], c: char, from: usize) -> Option<usize> {
    (from..raw.len()).rev().find(|&p| raw[p] == c)
}

/// `re.compile(r'[\t\n\r\f ;]').search(rawdata, pos)`: **unanchored**, so this
/// asks "does such a character exist at or after `pos`", not "is the character
/// at `pos` one of them". Getting this backwards is what makes a page hang
/// waiting for text that will never arrive.
fn has_ws_or_semi(raw: &[char], from: usize) -> bool {
    (from..raw.len()).any(|p| is_ws5(raw[p]) || raw[p] == ';')
}

/// `(?:[\t\n\r\f ]|/(?!>))*` — the trailing group of `tagfind_tolerant` and
/// `attrfind_tolerant`. A slash is consumed only when it does not begin `/>`;
/// at the very end of the buffer the lookahead succeeds, so a trailing `/` is
/// consumed.
fn skip_ws_or_fork(raw: &[char], from: usize) -> usize {
    let n = raw.len();
    let mut p = from;
    while p < n {
        if is_ws5(raw[p]) {
            p += 1;
        } else if raw[p] == '/' && !(p + 1 < n && raw[p + 1] == '>') {
            p += 1;
        } else {
            break;
        }
    }
    p
}

/// `[\t\n\r\f /]*` — the trailing group of `locatetagend`, which unlike the two
/// above eats `/` unconditionally.
fn skip_ws5_slash(raw: &[char], from: usize) -> usize {
    let mut p = from;
    while p < raw.len() && is_ws5_or_slash(raw[p]) {
        p += 1;
    }
    p
}

fn skip_ws5(raw: &[char], from: usize) -> usize {
    let mut p = from;
    while p < raw.len() && is_ws5(raw[p]) {
        p += 1;
    }
    p
}

// --- the regexes, as deterministic procedures ------------------------------

/// `tagfind_tolerant.match(raw, p)` → `(group 1, match.end())`.
fn tagfind(raw: &[char], p: usize) -> Option<(String, usize)> {
    let n = raw.len();
    if p >= n || !is_ascii_letter(raw[p]) {
        return None;
    }
    let mut q = p + 1;
    while q < n && !is_ws5_slash_gt(raw[q]) {
        q += 1;
    }
    Some((slice(raw, p, q), skip_ws_or_fork(raw, q)))
}

/// `(?<=…)[^\t\n\r\f />][^\t\n\r\f /=>]*` — `attrfind_tolerant`'s group 1. The
/// first character may not be `>`; the rest may not be `>` **or** `=`.
fn attr_name(raw: &[char], p: usize) -> Option<usize> {
    if p == 0 || !is_attr_lookbehind(raw[p - 1]) {
        return None;
    }
    let n = raw.len();
    if p >= n || is_ws5_slash_gt(raw[p]) {
        return None;
    }
    let mut q = p + 1;
    while q < n && !(is_ws5(raw[q]) || raw[q] == '/' || raw[q] == '=' || raw[q] == '>') {
        q += 1;
    }
    Some(q)
}

/// The same shape inside `locatetagend`. It is written with the **same** first
/// character class as `attrfind_tolerant`'s group 1 (`[^\t\n\r\f />]`, which
/// excludes `>`), so a start tag always ends at the *first* `>` and nothing
/// after it is ever swallowed as an attribute name: in `<video foo > >` the tag
/// ends at the first `>` and the trailing ` >` is handed to `handle_data`.
fn locatetag_name(raw: &[char], p: usize) -> Option<usize> {
    if p == 0 || !is_attr_lookbehind(raw[p - 1]) {
        return None;
    }
    let n = raw.len();
    if p >= n || is_ws5_or_slash(raw[p]) || raw[p] == '>' {
        return None;
    }
    let mut q = p + 1;
    while q < n && !(is_ws5(raw[q]) || raw[q] == '/' || raw[q] == '=' || raw[q] == '>') {
        q += 1;
    }
    Some(q)
}

/// `([\t\n\r\f ]*=[\t\n\r\f ]*(LITA|LIT|bare))?` — the value indicator.
///
/// Returns `None` when the group is absent, otherwise `(group 3, end)` where
/// `end` is the position just after group 3 (the caller then applies the
/// trailing whitespace group). The subtlety that earns this its own function:
/// when the value starts with a quote that is never closed, the two quoted
/// alternatives fail and the bare one is blocked by its `(?!['"])` lookahead —
/// so the engine backtracks the greedy `[\t\n\r\f ]*` *after* the `=`, one
/// character at a time, until the bare alternative matches **the empty string**
/// at a whitespace position. With at least one space after the `=` the group
/// therefore succeeds with `value == ""` and lands on the quote; with none it
/// fails outright and the attribute has no value at all.
fn indicator(raw: &[char], p: usize) -> Option<(String, usize)> {
    let n = raw.len();
    let eq = skip_ws5(raw, p);
    if eq >= n || raw[eq] != '=' {
        return None;
    }
    let greedy = skip_ws5(raw, eq + 1);
    // Backtrack the run of spaces after `=` from longest to shortest.
    let mut v = greedy;
    loop {
        if v == n {
            // End of buffer: the bare alternative matches the empty string.
            return Some((String::new(), n));
        }
        let c = raw[v];
        if c == '\'' || c == '"' {
            if let Some(t) = find_char(raw, c, v + 1) {
                return Some((slice(raw, v, t + 1), t + 1));
            }
            // No closing quote anywhere: give back one space and try the bare
            // alternative at that whitespace character (an empty match).
        } else {
            let mut q = v;
            while q < n && !(is_ws5(raw[q]) || raw[q] == '>') {
                q += 1;
            }
            return Some((slice(raw, v, q), q));
        }
        if v == eq + 1 {
            return None;
        }
        v -= 1;
        // `v` only ever points at whitespace here, which is what the bare
        // alternative matches zero of, so this loop terminates.
    }
}

/// One `attrfind_tolerant.match`: name, optional value, and where the match
/// ends.
pub struct Attribute {
    pub name: String,
    pub value: Option<String>,
    end: usize,
}

fn attrfind(raw: &[char], p: usize) -> Option<Attribute> {
    let name_end = attr_name(raw, p)?;
    let name = slice(raw, p, name_end);
    let mut value: Option<String> = None;
    let mut end = name_end;
    if let Some((group3, e)) = indicator(raw, name_end) {
        // Python:
        //   if not rest: attrvalue = None
        //   elif attrvalue[:1] == "'" == attrvalue[-1:] or same for '"': strip
        // `rest` is group 2, which is present exactly when group 3 is, so a
        // present group 3 always means "strip if it is quote-wrapped".
        let mut v: Vec<char> = group3.chars().collect();
        let first = v.first().copied();
        let last = v.last().copied();
        if (first == Some('\'') && last == Some('\'')) || (first == Some('"') && last == Some('"')) {
            v.remove(0);
            if !v.is_empty() {
                v.pop();
            }
        }
        let joined: String = v.iter().collect();
        // `if attrvalue:` — an empty value is not unescaped, and stays empty.
        value = Some(if joined.is_empty() {
            joined
        } else {
            crate::entities::unescape(&joined)
        });
        end = e;
    }
    Some(Attribute {
        name,
        value,
        end: skip_ws_or_fork(raw, end),
    })
}

/// `locatetagend.match(raw, p).end()` — the position after the whole (possibly
/// unterminated) start/end tag. `None` when the tag name is not there, which is
/// the stdlib's `assert match` and therefore not reachable from `goahead`.
fn locatetag_end(raw: &[char], p: usize) -> Option<usize> {
    let n = raw.len();
    if p >= n || !is_ascii_letter(raw[p]) {
        return None;
    }
    let mut q = p + 1;
    while q < n && !is_ws5_slash_gt(raw[q]) {
        q += 1;
    }
    q = skip_ws5_slash(raw, q);
    loop {
        let e2 = match locatetag_name(raw, q) {
            Some(e) => e,
            None => break,
        };
        let mut e = e2;
        if let Some((_g3, ie)) = indicator(raw, e2) {
            e = ie;
        }
        e = skip_ws5_slash(raw, e);
        if e <= q {
            break;
        }
        q = e;
    }
    if q < n && raw[q] == '>' {
        q += 1;
    }
    Some(q)
}

/// The stateful half of `HTMLParser`. `sink` is public so a caller can take the
/// answers back out after `close()`, the way `media_resolve.py` reads
/// `parser.found` and `parser.page_title`.
pub struct HtmlParser<S: HtmlSink> {
    pub sink: S,
    rawdata: Vec<char>,
    lasttag: String,
    cdata_elem: Option<String>,
    /// `self._escapable`: whether `handle_data` resolves character references.
    escapable: bool,
    /// `self._support_cdata` — `<![CDATA[` is a section while this holds, and a
    /// bogus comment when it does not. The stdlib only ever leaves it `True`.
    support_cdata: bool,
    scripting: bool,
}

impl<S: HtmlSink> HtmlParser<S> {
    pub fn new(sink: S) -> Self {
        Self::with_scripting(sink, false)
    }

    /// `HTMLParser(*, scripting=…)`. `convert_charrefs` is not a knob here —
    /// see the module comment.
    pub fn with_scripting(sink: S, scripting: bool) -> Self {
        HtmlParser {
            sink,
            rawdata: Vec::new(),
            lasttag: String::from("???"),
            cdata_elem: None,
            escapable: true,
            support_cdata: true,
            scripting,
        }
    }

    /// `HTMLParser.reset()`.
    pub fn reset(&mut self) {
        self.rawdata.clear();
        self.lasttag = String::from("???");
        self.cdata_elem = None;
        self.escapable = true;
        self.support_cdata = true;
    }

    pub fn lasttag(&self) -> &str {
        &self.lasttag
    }

    /// `feed(data)` — append, then parse what can be parsed. Whatever the
    /// buffer cannot decide yet (an unterminated tag) stays in it for the next
    /// call, exactly like the Python.
    pub fn feed(&mut self, data: &str) {
        self.rawdata.extend(data.chars());
        self.goahead(false);
    }

    /// `close()` — parse the rest as if an EOF marker followed.
    pub fn close(&mut self) {
        self.goahead(true);
    }

    fn set_cdata_mode(&mut self, elem: &str, escapable: bool) {
        self.cdata_elem = Some(elem.to_lowercase());
        self.escapable = escapable;
    }

    fn clear_cdata_mode(&mut self) {
        self.cdata_elem = None;
        self.escapable = true;
    }

    /// `self.interesting.search(rawdata, i)` while a CDATA element is open.
    ///
    /// The stdlib compiles `</elem(?=[\t\n\r\f />])` with `IGNORECASE|ASCII`,
    /// i.e. the closing tag **is** searched for. Only `plaintext` gets `\Z`,
    /// which as a `search` is a zero-width hit at the end of the buffer — so it
    /// "never matches anything interesting" by always matching the end. That
    /// default (`escapable=False`) is the one `set_cdata_mode` documents, and
    /// the `&|</elem…` variant needs `convert_charrefs=False`, which this port
    /// pins off; transcribing it would be code no input can reach.
    fn cdata_search(&self, raw: &[char], from: usize) -> Option<usize> {
        let elem = self.cdata_elem.as_ref()?;
        if elem == "plaintext" {
            return Some(raw.len());
        }
        let e: Vec<char> = elem.chars().collect();
        let n = raw.len();
        let mut p = from;
        while p + 2 + e.len() <= n {
            if raw[p] == '<' && raw[p + 1] == '/' && raw[p + 2..p + 2 + e.len()]
                .iter()
                .zip(e.iter())
                .all(|(a, b)| ascii_fold_eq(*a, *b))
            {
                let after = p + 2 + e.len();
                // The `(?=[\t\n\r\f />])` lookahead needs a *character* to look
                // at: at the end of the buffer it cannot be satisfied, so an
                // `</title` cut off at the last byte is **not** a match and the
                // content stays in the buffer for the next feed.
                if after >= n || !is_ws5_slash_gt(raw[after]) {
                    p += 1;
                    continue;
                }
                return Some(p);
            }
            p += 1;
        }
        None
    }

    /// `handle_data` with the stdlib's unescape gate.
    fn emit_data(&mut self, raw: &[char], i: usize, j: usize) {
        let text = slice(raw, i, j);
        if self.escapable {
            self.sink.handle_data(&crate::entities::unescape(&text));
        } else {
            self.sink.handle_data(&text);
        }
    }

    fn goahead(&mut self, end: bool) {
        let raw = std::mem::take(&mut self.rawdata);
        let n = raw.len();
        let mut i = 0usize;
        let mut stop = false;
        while i < n && !stop {
            // `if self.convert_charrefs and not self.cdata_elem:` — the first
            // term is constant here, so the fast path is "no CDATA element".
            let j: usize = if self.cdata_elem.is_none() {
                match find_char(&raw, '<', i) {
                    Some(p) => p,
                    None => {
                        let start = i.max(n.saturating_sub(34));
                        match rfind_char(&raw, '&', start) {
                            Some(amppos) if !has_ws_or_semi(&raw, amppos) => {
                                // A character reference may still be arriving;
                                // hold the text back for the next feed.
                                stop = true;
                                n
                            }
                            _ => n,
                        }
                    }
                }
            } else {
                match self.cdata_search(&raw, i) {
                    Some(p) => p,
                    None => {
                        // `if self.cdata_elem: break` — unconditional, even
                        // when `end` is true. An unclosed `<script>` holds its
                        // content forever; that is the stdlib's answer.
                        stop = true;
                        i
                    }
                }
            };
            if stop {
                break;
            }
            if i < j {
                self.emit_data(&raw, i, j);
                i = j;
            }
            if i == n {
                break;
            }
            if raw[i] == '<' {
                // The stdlib's dispatch, including its last arm: `else: break`.
                // That break leaves `i` where it is, so the lone `<` at the end
                // of a feed stays in the buffer — and it is only reachable when
                // `end` is false, because the arm above takes the lone `<` as
                // data when `end` is true.
                let mut k: Option<usize> = None;
                let mut held = false;
                if i + 1 < n && is_ascii_letter(raw[i + 1]) {
                    k = self.parse_starttag(&raw, i);
                } else if starts_with(&raw, i, "</") {
                    k = self.parse_endtag(&raw, i);
                } else if starts_with(&raw, i, "<!--") {
                    k = self.parse_comment(&raw, i);
                } else if starts_with(&raw, i, "<?") {
                    k = self.parse_pi(&raw, i);
                } else if starts_with(&raw, i, "<!") {
                    k = self.parse_html_declaration(&raw, i);
                } else if i + 1 < n || end {
                    self.sink.handle_data("<");
                    k = Some(i + 1);
                } else {
                    held = true;
                }
                if held {
                    break;
                }
                if k.is_none() {
                    if !end {
                        break;
                    }
                    // EOF ladder: whatever cannot be decided gets consumed as
                    // the one thing that says so most honestly.
                    if i + 1 < n && is_ascii_letter(raw[i + 1]) {
                        // `<x` with no end: stdlib `pass` — consumed, silent.
                    } else if starts_with(&raw, i, "</") {
                        if i + 2 == n {
                            self.sink.handle_data("</");
                        } else if i + 2 < n && is_ascii_letter(raw[i + 2]) {
                            // `</x` — likewise silent.
                        } else {
                            let t = slice(&raw, i + 2, n);
                            self.sink.handle_comment(&t);
                        }
                    } else if starts_with(&raw, i, "<!--") {
                        let mut j = n;
                        for suffix in ["--!", "--", "-"] {
                            let l = suffix.chars().count();
                            if n >= i + 4 + l && slice(&raw, n - l, n) == suffix.to_string() {
                                j -= l;
                                break;
                            }
                        }
                        let t = slice(&raw, i + 4, j);
                        self.sink.handle_comment(&t);
                    } else if starts_with(&raw, i, "<![CDATA[") && self.support_cdata {
                        let t = slice(&raw, i + 3, n);
                        self.sink.unknown_decl(&t);
                    } else if lower9(&raw, i) == "<!doctype" {
                        let t = slice(&raw, i + 2, n);
                        self.sink.handle_decl(&t);
                    } else if starts_with(&raw, i, "<!") {
                        let t = slice(&raw, i + 2, n);
                        self.sink.handle_comment(&t);
                    } else if starts_with(&raw, i, "<?") {
                        let t = slice(&raw, i + 2, n);
                        self.sink.handle_pi(&t);
                    }
                    k = Some(n);
                }
                i = k.unwrap();
            } else {
                // The `&#` / `&` / `assert 0` arms. With `convert_charrefs`
                // pinned true the fast path only ever stops on `<`, and the
                // CDATA pattern is anchored on `</`, so no `&` reaches here.
                // The stdlib's own `assert 0` says the same thing.
                break;
            }
            continue;
        }
        if end && i < n {
            self.emit_data(&raw, i, n);
            i = n;
        }
        self.rawdata = if i >= n { Vec::new() } else { raw[i..].to_vec() };
    }

    /// `parse_starttag`: return the end of the whole tag, or `None` (-1) when
    /// the tag is not complete yet.
    fn parse_starttag(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let endpos = check_for_whole_start_tag(raw, i)?;
        let mut attrs: Vec<(String, Option<String>)> = Vec::new();
        let (tag, start_k) = match tagfind(raw, i + 1) {
            Some(v) => v,
            None => return None,
        };
        let tag = tag.to_lowercase();
        self.lasttag = tag.clone();
        let mut k = start_k;
        while k < endpos {
            let m = match attrfind(raw, k) {
                Some(m) => m,
                None => break,
            };
            if m.end <= k {
                break;
            }
            attrs.push((m.name.to_lowercase(), m.value));
            k = m.end;
        }
        // `rawdata[k:endpos].strip()` — the 29-code-point set, not the 5.
        let tail = slice(raw, k, endpos);
        let end = crate::resolve::py_strip(&tail);
        if end != ">" && end != "/>" {
            // Unparsable tail: hand the whole tag to `handle_data`, raw. This
            // is the arm that makes `<video src="a.mp4>` produce no attribute
            // at all and one data event that contains the markup.
            let t = slice(raw, i, endpos);
            self.sink.handle_data(&t);
            return Some(endpos);
        }
        if end.ends_with("/>") {
            self.sink.handle_startendtag(&tag, &attrs);
        } else {
            self.sink.handle_starttag(&tag, &attrs);
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str())
                || (self.scripting && tag == "noscript")
                || tag == "plaintext"
            {
                self.set_cdata_mode(&tag, false);
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.set_cdata_mode(&tag, true);
            }
        }
        Some(endpos)
    }

    /// `parse_endtag`. Note the two checks the stdlib runs *before* it looks at
    /// the name: "is there any `>` at all", then "`</` + letter?". The name is
    /// then located with `locatetagend` and the end tag is reported for
    /// whatever `tagfind` names first — so `</div <span>` is an end tag named
    /// `div`, and it clears CDATA mode.
    fn parse_endtag(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let n = raw.len();
        if find_char(raw, '>', i + 2).is_none() {
            return None;
        }
        if i + 2 >= n || !is_ascii_letter(raw[i + 2]) {
            if i + 2 < n && raw[i + 2] == '>' {
                return Some(i + 3); // `</>` is ignored, in silence
            }
            return self.parse_bogus_comment(raw, i);
        }
        let j = match locatetag_end(raw, i + 2) {
            Some(j) => j,
            None => return None,
        };
        if j == 0 || raw[j - 1] != '>' {
            return None;
        }
        let tag = match tagfind(raw, i + 2) {
            Some((t, _)) => t.to_lowercase(),
            None => return None,
        };
        self.sink.handle_endtag(&tag);
        self.clear_cdata_mode();
        Some(j)
    }

    /// `parse_comment`: `--!?>` searched from `i+4`, else `-?>` **anchored** at
    /// `i+4`. The anchoring is why `<!--x>` is not a comment (`-?>` must start
    /// at the very first content character) while `<!-->` is an empty one.
    fn parse_comment(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let n = raw.len();
        let mut p = i + 4;
        let found: Option<(usize, usize)> = loop {
            if p >= n {
                break None;
            }
            if starts_with(raw, p, "--!") && p + 3 < n && raw[p + 3] == '>' {
                break Some((p, p + 4));
            }
            if starts_with(raw, p, "-->") {
                break Some((p, p + 3));
            }
            p += 1;
        };
        let (start, stop) = match found {
            Some(v) => v,
            None => {
                if i + 4 < n && raw[i + 4] == '>' {
                    (i + 4, i + 5)
                } else if starts_with(raw, i + 4, "->") {
                    (i + 4, i + 6)
                } else {
                    return None;
                }
            }
        };
        let t = slice(raw, i + 4, start);
        self.sink.handle_comment(&t);
        Some(stop)
    }

    /// `parse_bogus_comment`: the first `>` from `i+2` ends it; the content
    /// starts **after** the two characters `<!` or `</`.
    fn parse_bogus_comment(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let pos = find_char(raw, '>', i + 2)?;
        let t = slice(raw, i + 2, pos);
        self.sink.handle_comment(&t);
        Some(pos + 1)
    }

    fn parse_pi(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let j = find_char(raw, '>', i + 2)?;
        let t = slice(raw, i + 2, j);
        self.sink.handle_pi(&t);
        Some(j + 1)
    }

    /// `parse_html_declaration`. `<[CDATA[` is compared **case-exactly** (the
    /// stdlib writes `rawdata[i:i+9] == '<![CDATA['`), so `<![cdata[x]]>` is not
    /// a CDATA section — it falls through to the `<![` arm below.
    fn parse_html_declaration(&mut self, raw: &[char], i: usize) -> Option<usize> {
        let n = raw.len();
        if starts_with(raw, i, "<!--") {
            return self.parse_comment(raw, i);
        }
        if starts_with(raw, i, "<![CDATA[") && self.support_cdata {
            let j = find_char(raw, ']', i + 9);
            // `rawdata.find(']]>', i+9)`: two consecutive `]` then a `>`.
            let mut p = match j {
                Some(v) => v,
                None => return None,
            };
            let mut hit: Option<usize> = None;
            while p < n {
                if p + 2 < n && raw[p + 1] == ']' && raw[p + 2] == '>' {
                    hit = Some(p);
                    break;
                }
                p += 1;
            }
            let j = match hit {
                Some(v) => v,
                None => return None,
            };
            let t = slice(raw, i + 3, j);
            self.sink.unknown_decl(&t);
            return Some(j + 3);
        }
        if lower9(raw, i) == "<!doctype" {
            let gtpos = match find_char(raw, '>', i + 9) {
                Some(v) => v,
                None => return None,
            };
            let t = slice(raw, i + 2, gtpos);
            self.sink.handle_decl(&t);
            return Some(gtpos + 1);
        }
        if starts_with(raw, i, "<![") {
            let j = find_char(raw, '>', i + 3)?;
            if j >= 1 && raw[j - 1] == ']' {
                let t = slice(raw, i + 3, j - 1);
                self.sink.unknown_decl(&t);
            } else {
                let t = slice(raw, i + 2, j);
                self.sink.handle_comment(&t);
            }
            return Some(j + 1);
        }
        self.parse_bogus_comment(raw, i)
    }
}

/// `locatetagend.match(rawdata, i+1).end()`, then "did it actually land on a
/// `>`". A match that stops short of `>` means the tag is incomplete, so the
/// buffer keeps whatever it has until the next `feed`.
fn check_for_whole_start_tag(raw: &[char], i: usize) -> Option<usize> {
    let j = locatetag_end(raw, i + 1)?;
    if j == 0 || raw[j - 1] != '>' {
        return None;
    }
    Some(j)
}

/// `rawdata[i:i+9].lower()` — the left half of the `== "<!doctype"` test that
/// both `parse_html_declaration` and the end-of-file ladder make. Python's
/// slice is satisfied by fewer than nine characters, so a short tail lowers to
/// itself and answers `False`; `str.lower()` is Unicode-aware, which is why this
/// is `to_lowercase` and not ASCII folding.
fn lower9(raw: &[char], i: usize) -> String {
    slice(raw, i, i + 9).to_lowercase()
}
