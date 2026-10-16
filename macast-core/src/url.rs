//! The URL reader, ported from `urllib.parse` and measured against it.
//!
//! This module is why it lives in `macast-core` rather than next to the two
//! callers: `macast-cast` depends on `macast-upnp` and never the other way
//! round, so a reader that both the resolver (page links, share text, titles)
//! and the device discovery layer (SSDP `LOCATION` headers, `<URLBase>` joins,
//! control-URL ports) has to agree on cannot be private to either crate.
//! The alternative — a second copy — is the failure AGENTS.md §4.14 already
//! names for `advertise_host`: two readers of one rule, drifting apart while
//! both stay green.
//!
//! # What "measured" means here
//!
//! Every branch and every message in this file was copied from the reference
//! implementation's *behaviour*, not from its source: the oracle is
//! `.venv/bin/python` — **CPython 3.12.15** — and `.venv` resolves the stdlib to
//! `/opt/homebrew/Cellar/python@3.12/3.12.15/…/lib/python3.12/urllib/parse.py`,
//! whose line numbers are cited below so a future reader can re-ask the same
//! question. Where a wording was never reachable by the corpus we measured, the
//! constant says so (`UNMEASURED`).
//!
//! Two deliberate divergences, both documented at their own definition instead
//! of hidden:
//!
//! - [`py_is_nonprintable`] answers for Co/Cn code points by printing them
//!   verbatim (no Unicode category table for one message nobody has ever shown
//!   a user).
//! - Nothing here touches a socket or a process; `port()` returns Python's
//!   `ValueError` as [`PyUrlError`] instead of raising, which is the same choice
//!   the callers already make (see [`PyUrlError`]'s doc on `run()`).

/// Whichever of `\t` `\r` `\n` urlsplit removes wholesale from the raw string.
/// Python removes exactly these three before looking for a scheme, so they can
/// never appear in the parts (the WHATWG-trim set, not the whole C0 range).
fn strip_tab_cr_lf(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect()
}

/// The C0-control-or-space prefix urlsplit strips from the *left* only.
/// Python's comment: "Only lstrip url as some applications rely on preserving
/// trailing space." (`_WHATWG_C0_CONTROL_OR_SPACE = range(0, 0x21)`.)
fn is_c0_control_or_space(c: char) -> bool {
    c <= '\u{20}'
}

/// Python's `ValueError` from `urlsplit` / `urljoin` / the `ipaddress` module.
/// The port keeps the failure shape — callers that mirror Python's `try/except`
/// catch it, and callers that mirror its *unguarded* calls turn it into the
/// fallback value they use instead of a traceback.
///
/// It carries Python's own message because that message is what the job's card
/// reads: `run()` writes 「解析出错：ValueError: <message>」, and three truth
/// rows (`flow#19`, `err#6`, `err#7`) record the sentence
/// `ValueError: Invalid IPv6 URL` in that field. Every wording below was
/// measured against the oracle's interpreter (Python 3.12.15, `.venv/bin/python`)
/// on the shape named at its raise site; only `UNMEASURED` was not, because no
/// `ValueError` message exists for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyUrlError {
    pub message: String,
}

/// `ValueError(f'Invalid IPv6 URL')` — CPython writes this at three places in
/// `urlparse` (`urlsplit`'s bracket-parity check, and the two checks in
/// `_check_bracketed_netloc`), all byte-identical.
const INVALID_IPV6: &str = "Invalid IPv6 URL";

/// The one site left with a generic wording: `split_netloc` slicing mid
/// character. CPython raises `TypeError` there, not `ValueError`, and a `String`
/// netloc cannot reach it — so there is no measured message to copy, and no
/// truth row or probe drives it.
const UNMEASURED: &str = "invalid URL";

impl PyUrlError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn ipv6() -> Self {
        Self::new(INVALID_IPV6)
    }
}

impl std::fmt::Display for PyUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PyUrlError {}

/// Python's `urllib.parse.uses_relative` / `uses_netloc` / `uses_params`,
/// verbatim (the leading `""` classifies scheme-less URLs).
const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];

const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https",
    "shttp", "snews", "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp",
    "nfs", "git", "git+ssh", "ws", "wss", "itms-services",
];

const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// Result of urlsplit's parsing step: the scheme is already lowercased and the
/// netloc has passed the bracket checks; the path/query/fragment are the raw
/// substrings Python would have split out.
pub struct PySplit {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// `urllib.parse.urlsplit(url)`, ported whole — including the cache-free shape
/// (the memo in Python is unobservable) and the `allow_fragments=True` default.
pub fn py_urlsplit(url: &str, scheme: &str, allow_fragments: bool) -> Result<PySplit, PyUrlError> {
    // Only lstrip url as some applications rely on preserving trailing space.
    let url: String = url
        .trim_start_matches(is_c0_control_or_space)
        .to_string();
    // The scheme argument is the *default* scheme, not a candidate: when the
    // url carries none, this value is what the result reports.
    let scheme = scheme.trim_matches(is_c0_control_or_space).to_string();
    let url = strip_tab_cr_lf(&url);
    let scheme = strip_tab_cr_lf(&scheme);

    let mut split = PySplit {
        scheme,
        netloc: String::new(),
        path: String::new(),
        query: String::new(),
        fragment: String::new(),
    };

    // Scheme detection: i > 0, first byte is ASCII (Python checks
    // `url[0].isascii()` then `url[0].isalpha()`), and the head is all
    // scheme chars.
    if let Some(i) = url.find(':') {
        if i > 0 {
            let head = &url[..i];
            let first = head.as_bytes()[0];
            if first.is_ascii() && first.is_ascii_alphabetic()
                && head.chars().all(scheme_chars_contains)
            {
                split.scheme = head.to_ascii_lowercase();
                let rest = url[i + 1..].to_string();
                return PySplitResult::finish(rest, split, allow_fragments);
            }
        }
    }
    PySplitResult::finish(url, split, allow_fragments)
}

fn scheme_chars_contains(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// The tail of urlsplit after the scheme (or the whole string when there is no
/// scheme): the `//` netloc branch, then `#`, then `?`.
struct PySplitResult;

impl PySplitResult {
    fn finish(
        mut rest: String,
        mut split: PySplit,
        allow_fragments: bool,
    ) -> Result<PySplit, PyUrlError> {
        if rest.starts_with("//") {
            let (netloc, after) = split_netloc(&rest, 2).map_err(|_| PyUrlError::new(UNMEASURED))?;
            rest = after;
            // A url with bracketed IPv6 literals in netloc must have matching
            // brackets; otherwise the ValueError("Invalid IPv6 URL") only
            // fires when a present bracket is also used.
            let has_open = netloc.contains('[');
            let has_close = netloc.contains(']');
            if has_open != has_close {
                // ValueError(f'Invalid IPv6 URL')
                return Err(PyUrlError::ipv6());
            }
            if has_open {
                check_bracketed_netloc(&netloc)?;
            }
            split.netloc = netloc;
        }
        if allow_fragments && rest.contains('#') {
            if let Some(i) = rest.find('#') {
                split.fragment = rest[i + 1..].to_string();
                rest.truncate(i);
            }
        }
        if rest.contains('?') {
            if let Some(i) = rest.find('?') {
                split.query = rest[i + 1..].to_string();
                rest.truncate(i);
            }
        }
        split.path = rest;
        check_netloc(&split.netloc)?;
        Ok(split)
    }
}

/// Python `_splitnetloc`: netloc ends at the first `/`, `?` or `#` after
/// `start`; when none is found the rest of the string is the netloc.
/// Returns Err only when `start` slices mid-character (Python would raise
/// `TypeError`; ours is a `String`, so this cannot practically happen).
fn split_netloc(url: &str, start: usize) -> Result<(String, String), ()> {
    if !url.is_char_boundary(start) {
        return Err(());
    }
    let mut delim = url.len();
    for c in ['/', '?', '#'] {
        if let Some(i) = url[start..].find(c) {
            let i = start + i;
            if i < delim {
                delim = i;
            }
        }
    }
    Ok((url[start..delim].to_string(), url[delim..].to_string()))
}

/// `urllib.parse.urlparse` — urlsplit plus parameter splitting for schemes in
/// `uses_params` (http and https are). Returns `(url_without_params, params)`.
pub fn py_urlparse(
    url: &str,
    scheme: &str,
    allow_fragments: bool,
) -> Result<(PySplit, String), PyUrlError> {
    let split = py_urlsplit(url, scheme, allow_fragments)?;
    if USES_PARAMS.contains(&split.scheme.as_str()) && split.path.contains(';') {
        let (path, params) = split_params(&split.path);
        let mut split = split;
        split.path = path;
        Ok((split, params))
    } else {
        Ok((split, String::new()))
    }
}

/// Python `_splitparams`: with a `/` present the separator is the first `;`
/// after the last `/`; with no `/` at all it is the first `;` anywhere.
fn split_params(url: &str) -> (String, String) {
    match url.rfind('/') {
        Some(last_slash) => match url[last_slash..].find(';') {
            Some(rel) => {
                let i = last_slash + rel;
                (url[..i].to_string(), url[i + 1..].to_string())
            }
            None => (url.to_string(), String::new()),
        },
        None => match url.find(';') {
            Some(i) => (url[..i].to_string(), url[i + 1..].to_string()),
            None => (url.to_string(), String::new()),
        },
    }
}

/// `urllib.parse.urlunparse` (params non-empty re-attaches a `;` segment).
pub fn py_urlunparse(
    scheme: &str,
    netloc: &str,
    path: &str,
    params: &str,
    query: &str,
    fragment: &str,
) -> String {
    let url = if params.is_empty() {
        path.to_string()
    } else {
        format!("{};{}", path, params)
    };
    py_urlunsplit(scheme, netloc, &url, query, fragment)
}

/// `urllib.parse.urlunsplit` — the three netloc branches decide when a `//`
/// prefix is added, and the scheme prefix is glued on *after* them (so a
/// schemeless `//x` becomes `////x`, which is the shape Python produces).
pub fn py_urlunsplit(scheme: &str, netloc: &str, url: &str, query: &str, fragment: &str) -> String {
    let mut url = url.to_string();
    if !netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{}", url);
        }
        url = format!("//{}{}", netloc, url);
    } else if url.starts_with("//") {
        url = format!("//{}", url);
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (url.is_empty() || url.starts_with('/'))
    {
        url = format!("//{}", url);
    }
    if !scheme.is_empty() {
        url = format!("{}:{}", scheme, url);
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

/// `urllib.parse.urljoin(base, url)`. Ported whole, including the two early
/// returns that hand back a string *without re-serialising it* — Python returns
/// the `url` argument object as-is when the schemes differ, and the base string
/// as-is when `url` is empty.
pub fn py_urljoin(base: &str, url: &str) -> Result<String, PyUrlError> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let (b, bparams) = py_urlparse(base, "", true)?;
    // The url side is parsed with the base's scheme as its default.
    let (u, params) = py_urlparse(url, &b.scheme, true)?;
    let scheme = u.scheme.as_str();
    let bscheme = b.scheme.as_str();
    if scheme != bscheme || !USES_RELATIVE.contains(&scheme) {
        return Ok(url.to_string());
    }
    let mut netloc = u.netloc.clone();
    if USES_NETLOC.contains(&scheme) {
        if !netloc.is_empty() {
            return Ok(py_urlunparse(
                scheme,
                &netloc,
                &u.path,
                &params,
                &u.query,
                &u.fragment,
            ));
        }
        netloc = b.netloc.clone();
    }
    if u.path.is_empty() && params.is_empty() {
        let query = if u.query.is_empty() {
            b.query.clone()
        } else {
            u.query.clone()
        };
        return Ok(py_urlunparse(
            scheme,
            &netloc,
            &b.path,
            &bparams,
            &query,
            &u.fragment,
        ));
    }
    // Merge paths for relative references.
    let segments: Vec<String>;
    if u.path.starts_with('/') {
        segments = u.path.split('/').map(|s| s.to_string()).collect();
    } else {
        let mut base_parts: Vec<String> = b.path.split('/').map(|s| s.to_string()).collect();
        if base_parts.last().map(|s| !s.is_empty()).unwrap_or(false) {
            base_parts.pop();
        }
        let mut segs = base_parts;
        segs.extend(u.path.split('/').map(|s| s.to_string()));
        // Python: `segments[1:-1] = filter(None, segments[1:-1])` — only on
        // this branch (the `path.startswith('/')` branch above never empties).
        if segs.len() > 2 {
            let filtered: Vec<String> = segs[1..segs.len() - 1]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect();
            segs.splice(1..segs.len() - 1, filtered);
        }
        segments = segs;
    }
    let mut resolved_path: Vec<String> = Vec::new();
    for seg in &segments {
        if seg == ".." {
            resolved_path.pop();
        } else if seg == "." {
            continue;
        } else {
            resolved_path.push(seg.clone());
        }
    }
    if matches!(segments.last().map(|s| s.as_str()), Some(".") | Some("..")) {
        resolved_path.push(String::new());
    }
    let joined = resolved_path.join("/");
    Ok(py_urlunparse(
        scheme,
        &netloc,
        if joined.is_empty() { "/" } else { &joined },
        &params,
        &u.query,
        &u.fragment,
    ))
}

/// The 19 code points `_checknetloc` searches for after NFKC normalisation.
/// NFKC-expanded labels are the cheap version of Python's
/// `unicodedata.normalize('NFKC', netloc)`: if a dangerous character is not
/// literally present, Python only rejects when its NFKC expansion would
/// introduce one, and this table holds exactly the expansions that matter.
const NETLOC_DANGER: &[(char, &str)] = &[
    ('\u{2047}', "??"),
    ('\u{2048}', "?!"),
    ('\u{2049}', "!?"),
    ('\u{2100}', "a/c"),
    ('\u{2101}', "a/s"),
    ('\u{2105}', "c/o"),
    ('\u{2106}', "c/u"),
    ('\u{2a74}', "::="),
    ('\u{fe13}', ":"),
    ('\u{fe16}', "?"),
    ('\u{fe55}', ":"),
    ('\u{fe56}', "?"),
    ('\u{fe5f}', "#"),
    ('\u{fe6b}', "@"),
    ('\u{ff03}', "#"),
    ('\u{ff0f}', "/"),
    ('\u{ff1a}', ":"),
    ('\u{ff1f}', "?"),
    ('\u{ff20}', "@"),
];

/// Python `_checknetloc`: an all-ASCII netloc is always fine; otherwise, any
/// code point whose NFKC expansion contains one of `/:?#@` is a ValueError.
fn check_netloc(netloc: &str) -> Result<(), PyUrlError> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    for (c, expansion) in NETLOC_DANGER {
        if netloc.contains(*c)
            && expansion
                .chars()
                .any(|e| matches!(e, '/' | '?' | '#' | '@' | ':'))
        {
            // ValueError(f'netloc {netloc!r} contains invalid characters '
            //            'under NFKC normalization') -- CPython quotes the
            // netloc with `'%s'`, not with `repr()`. Measured on 3.12.15.
            return Err(PyUrlError::new(format!(
                "netloc '{}' contains invalid characters under NFKC normalization",
                netloc
            )));
        }
    }
    Ok(())
}

/// Python `_check_bracketed_netloc`: inside the `[...]` the hostname is what
/// sits before `:port`, and the hostname still has to pass
/// `_check_bracketed_host`.
fn check_bracketed_netloc(netloc: &str) -> Result<(), PyUrlError> {
    let hostname_and_port = netloc.rsplit_once('@').map(|(_, p)| p).unwrap_or(netloc);
    match hostname_and_port.split_once('[') {
        Some((before_bracket, bracketed)) => {
            if !before_bracket.is_empty() {
                return Err(PyUrlError::ipv6());
            }
            let hostname = match bracketed.split_once(']') {
                Some((h, port)) => {
                    if !port.is_empty() && !port.starts_with(':') {
                        return Err(PyUrlError::ipv6());
                    }
                    h
                }
                None => bracketed,
            };
            check_bracketed_host(hostname)
        }
        None => {
            let hostname = hostname_and_port
                .split_once(':')
                .map(|(h, _)| h)
                .unwrap_or(hostname_and_port);
            check_bracketed_host(hostname)
        }
    }
}

/// Python `_check_bracketed_host` with the `ipaddress` module's rules inlined:
/// IPvFuture (`v[hex]+.` + tail) is one check; otherwise an IPv4 inside
/// brackets is refused (Python refuses it *after* `ip_address` parses it), and
/// otherwise it must parse as IPv6 under `IPv6Address.__init__`'s extras —
/// `/` refused before any parsing, scope id (after `%`) non-empty and
/// `%`-free, address parseable by Rust's std (verified equivalent against the
/// Python 3.12 oracle over the corpus, 45/45).
fn check_bracketed_host(hostname: &str) -> Result<(), PyUrlError> {
    if hostname.starts_with('v') {
        // IPvFuture: at least one hex digit, then '.', then a non-empty tail
        // with no newline. `re.match(r"\Av[a-fA-F0-9]+\..+\Z")` — `.+` cannot
        // cross a newline without `re.S`.
        let b = hostname.as_bytes();
        let mut pos = 1;
        while pos < b.len() && b[pos].is_ascii_hexdigit() {
            pos += 1;
        }
        let tail_ok = pos > 1
            && pos < b.len()
            && b[pos] == b'.'
            && hostname.len() > pos + 1
            && !hostname[pos + 1..].contains('\n');
        return if tail_ok {
            Ok(())
        } else {
            // ValueError('IPvFuture address is invalid')
            Err(PyUrlError::new("IPvFuture address is invalid"))
        };
    }
    // IPv4-in-brackets deliberately fails (checked before IPv6 so the error
    // shape matches Python's two separate raises).
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        // ValueError('An IPv4 address cannot be in brackets')
        return Err(PyUrlError::new("An IPv4 address cannot be in brackets"));
    }
    if py_ipv6_parses(hostname) {
        Ok(())
    } else {
        // ipaddress raises `ValueError(f'{addr!r} does not appear to be an
        // IPv4 or IPv6 address')` -- `repr()` of the hostname, measured on
        // 3.12.15 (`"no'pe" does not appear ...`).
        Err(PyUrlError::new(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            py_repr_str(hostname)
        )))
    }
}

/// `ipaddress.IPv6Address` for the cases `_check_bracketed_host` can present:
/// `/` anywhere is rejected before scope splitting (`IPv6Address.__init__`
/// checks `'/' in addr_str` first), the scope id after `%` must be non-empty
/// and contain no second `%`, and the address half must parse as IPv6.
fn py_ipv6_parses(s: &str) -> bool {
    if s.contains('/') {
        return false;
    }
    let addr = match s.split_once('%') {
        Some((a, scope)) => {
            if scope.is_empty() || scope.contains('%') {
                return false;
            }
            a
        }
        None => s,
    };
    addr.parse::<std::net::Ipv6Addr>().is_ok()

}
/// The characters CPython's `repr` would escape: everything in category Cc,
/// every separator (Zs minus the ASCII space, Zl, Zp), and the format characters
/// (Cf) it counts as invisible. Rust's `is_control` is exactly Cc and
/// `is_whitespace` covers the separators plus the control codes `repr` already
/// names, so the hand-built list below is the Cf set.
///
/// **Documented divergence**: Cc/Cf/Zs/Zl/Zp are enumerated here rather than
/// looked up in a general-category table, so Co and Cn code points — private-use
/// and unassigned, which CPython also escapes — print verbatim in this port. The
/// corpus has no row in those categories, and adding a unicode-category
/// dependency to a crate that does not otherwise need one is not worth a code
/// point no argv can contain.
fn py_is_nonprintable(c: char) -> bool {
    if c == ' ' {
        return false;
    }
    if c.is_control() || c.is_whitespace() {
        return true;
    }
    let cp = c as u32;
    matches!(cp,
        0x00ad
            | 0x0600..=0x0605
            | 0x061c
            | 0x06dd
            | 0x070f
            | 0x08e2
            | 0x110bd
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x16fe3
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f)
}

/// `repr()` of a Python `str`, which is what `str(list)` puts between the
/// brackets of an argv — and `subprocess` builds its timeout sentence out of
/// exactly that. Quote choice: `'` unless the text has a `'` and no `"`, in
/// which case `"` (so a shell-ish argument keeps its apostrophe unescaped).
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other == quote => {
                out.push('\\');
                out.push(other);
            }
            other if py_is_nonprintable(other) => match other as u32 {
                cp if cp < 0x100 => out.push_str(&format!("\\x{:02x}", cp)),
                cp if cp < 0x10000 => out.push_str(&format!("\\u{:04x}", cp)),
                cp => out.push_str(&format!("\\U{:08x}", cp)),
            },
            other => out.push(other),
        }
    }
    out.push(quote);
    out
}

/// `parse.py:206-217` — the two accessors below are the only readers of this.
///
/// Userinfo is cut off at the **last** `@` (`rpartition`), brackets are stripped
/// by a `partition('[')` / `partition(']')` pair, and the port is *everything
/// after the first* `:` — not the next colon-separated field. That distinction is
/// measured: `http://a.example:80:90/x` answers `'80:90'` and then refuses to
/// cast it, so a `split(':').nth(1)` would quietly turn a refusal into a port.
/// An empty port means `None`, which is what makes `http://a.example:/x` answer
/// "no opinion" rather than zero.
fn hostinfo(netloc: &str) -> (String, Option<String>) {
    let tail = match netloc.rfind('@') {
        Some(i) => &netloc[i + 1..],
        None => netloc,
    };
    let (hostname, port): (&str, &str) = match tail.find('[') {
        Some(i) => {
            let bracketed = &tail[i + 1..];
            match bracketed.find(']') {
                Some(j) => {
                    let after = &bracketed[j + 1..];
                    let port = match after.find(':') {
                        Some(k) => &after[k + 1..],
                        None => "",
                    };
                    (&bracketed[..j], port)
                }
                // `finish` rejects unbalanced brackets, so this arm is unreachable
                // from `py_urlsplit`. It stays faithful to `partition(']')`, whose
                // answer is (whole tail, "") — not a colon split, which would
                // invent a port on a shape Python reports as none.
                None => (bracketed, ""),
            }
        }
        None => match tail.find(':') {
            Some(i) => (&tail[..i], &tail[i + 1..]),
            None => (tail, ""),
        },
    };
    (
        hostname.to_string(),
        if port.is_empty() { None } else { Some(port.to_string()) },
    )
}

impl PySplit {
    /// `parse.py:165-173`. Empty means `None`. Everything before the first `%`
    /// is lowercased and everything from the `%` on is returned **verbatim**,
    /// because a scoped IPv6 zone id is an interface name on this machine, not
    /// part of a case-insensitive host: `http://[fe80::1%tESt]:1234/keys` keeps
    /// `tESt`. Lowercasing is Python's `str.lower()` — full Unicode, including
    /// SpecialCasing — so `İ` becomes `i` + U+0307 and `K` (U+212A) becomes `k`;
    /// `to_ascii_lowercase()` would answer both of those wrong.
    pub fn hostname(&self) -> Option<String> {
        let (raw, _) = hostinfo(&self.netloc);
        if raw.is_empty() {
            return None;
        }
        match raw.find('%') {
            Some(i) => Some(format!("{}{}", raw[..i].to_lowercase(), &raw[i..])),
            None => Some(raw.to_lowercase()),
        }
    }

    /// `parse.py:176-185`. Python raises `ValueError`; this returns it, which is
    /// the same choice every other reader in this file makes, so a caller cannot
    /// tell "no port" from "port nobody can cast" by accident.
    ///
    /// The cast predicate is `isdigit() and isascii()`. `isdigit()` is true for
    /// Arabic-Indic `٣` and superscript `²`, so `isascii()` is doing real work —
    /// Rust's `is_ascii_digit()` answers both rows the way Python's *pair* does.
    /// An empty port never reaches the predicate at all: `_hostinfo` maps it to
    /// `None` (`http://a.example:/x` answers "no opinion"), which is also what
    /// makes Python's `isdigit()`-on-`''` question unaskable here.
    ///
    /// Overflow reads as out-of-range, not as a parse failure: Python has
    /// bignums, so `:99999999999999999999` passes the cast and dies on the range
    /// check. Measured.
    pub fn port(&self) -> Result<Option<u16>, PyUrlError> {
        const PORT_OUT_OF_RANGE: &str = "Port out of range 0-65535";
        let (_, raw) = hostinfo(&self.netloc);
        let Some(raw) = raw else { return Ok(None) };
        if !raw.chars().all(|c| c.is_ascii_digit()) {
            return Err(PyUrlError::new(format!(
                "Port could not be cast to integer value as {}",
                py_repr_str(&raw)
            )));
        }
        match raw.parse::<u64>() {
            Ok(value) if value <= 65535 => Ok(Some(value as u16)),
            _ => Err(PyUrlError::new(PORT_OUT_OF_RANGE)),
        }
    }
}

/// `ipaddress.ip_address(text).is_loopback`, with `None` standing in for the
/// `ValueError` Python raises for anything that is not an IP literal.
///
/// It lives here because two faces ask this question and must not answer it
/// differently. `cast_local_file.parse_description` uses it to decide whether a
/// device description's `<URLBase>` names an address a control point can
/// actually reach (`127.0.0.1` in a description means "talk to the box that
/// answered", so the host gets replaced by the answering peer); this crate's
/// HTTP face uses the same predicate to decide whether a request came from
/// loopback and may therefore skip the management token. AGENTS.md §4.14's
/// `advertise_host` is what happens when that is two functions.
///
/// `None` means "this is a hostname" — Python's `except ValueError: pass`, with
/// the comment 「a hostname: keep it, DNS is the LAN's job」. It is *not* the
/// same answer as `Some(false)`, so callers that want a boolean have to say
/// which way they collapse the third state and why. This crate's management
/// gate collapses it to `false` (fail closed); the description reader collapses
/// it to "leave the host alone".
///
/// # Why this is not `IpAddr::is_loopback`
///
/// Rust's answer disagrees with CPython's on two rows, and the second one
/// changes what `parse_description` *does*, not just what it prints:
///
/// - `Ipv6Addr::is_loopback()` is `*self == LOCALHOST`, so `::ffff:127.0.0.1`
///   answers `false`. Python routes an IPv4-mapped address through
///   `ipv4_mapped.is_loopback`, so it answers `True`.
/// - `Ipv6Addr::from_str` rejects `%scope`, so `::1%lo` would answer `None` —
///   "a hostname" — and the description reader would **keep** `::1` instead of
///   replacing it with the peer that answered. Python parses the scope and
///   reports `True`.
///
/// So the scope grammar is stripped first (the same rules
/// [`py_ipv6_parses`] documents and tests: `/` rejected before anything else,
/// scope non-empty and `%`-free), then mapped IPv4 is unwrapped, then the
/// value is compared. `::0.0.0.1` is loopback too — Python's first arm is
/// `int(self) == 1`, which Rust reaches by the same route because
/// `Ipv6Addr` equality is by address value, not by spelling. Both of those
/// were measured here rather than assumed (see the row table in `tests`).
pub fn host_is_loopback(text: &str) -> Option<bool> {
    // IPv4 is tried first, exactly like `ip_address()`: it rejects leading
    // zeros, trailing spaces, and five dotted quads, and Rust's parser
    // rejects the same set (verified against the oracle over this corpus).
    if let Ok(v4) = text.parse::<std::net::Ipv4Addr>() {
        return Some(v4.is_loopback());
    }
    if text.contains('/') {
        return None;
    }
    let addr = match text.split_once('%') {
        Some((a, scope)) => {
            if scope.is_empty() || scope.contains('%') {
                return None;
            }
            a
        }
        None => text,
    };
    let v6 = addr.parse::<std::net::Ipv6Addr>().ok()?;
    Some(match v6.to_ipv4_mapped() {
        Some(mapped) => mapped.is_loopback(),
        None => v6 == std::net::Ipv6Addr::LOCALHOST,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the oracle answered for `.port`. The two refusals are separate
    /// variants because Python writes two different sentences: a cast refusal
    /// quotes the port's `repr` (`'abc'`), a range refusal quotes nothing.
    /// `CAST_PREFIX` + the repr, and the range sentence in full, are each
    /// asserted as a complete message — a row that only said "refused" would
    /// pass on the wrong wording.
    enum Port {
        Value(u16),
        Absent,
        Cast(&'static str),
        OutOfRange,
    }

    struct Row {
        url: &'static str,
        /// `Some(msg)` means `urlsplit` itself raised, so there is no split to
        /// read accessors off — the refusal has to be the split's own.
        split_err: Option<&'static str>,
        host: Option<&'static str>,
        port: Port,
    }

    const CAST_PREFIX: &str = "Port could not be cast to integer value as ";

    fn rows() -> Vec<Row> {
        vec![
            Row {
                url: "http://a.example/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Absent,
            },
            Row {
                url: "http://a.example:8080/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Value(8080),
            },
            // Empty port is "no opinion", not zero.
            Row {
                url: "http://a.example:/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Absent,
            },
            Row {
                url: "http://a.example:abc/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'abc'"),
            },
            Row {
                url: "http://a.example:65536/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::OutOfRange,
            },
            Row {
                url: "http://a.example:0/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Value(0),
            },
            Row {
                url: "http://a.example:-1/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'-1'"),
            },
            // isdigit() is true for both of these; isascii() is what refuses them.
            Row {
                url: "http://a.example:٣/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'٣'"),
            },
            Row {
                url: "http://a.example:²/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'²'"),
            },
            Row {
                url: "http://USER@Host.Example:8080/x",
                split_err: None,
                host: Some("host.example"),
                port: Port::Value(8080),
            },
            Row {
                url: "http://host@255.255.255.255/x",
                split_err: None,
                host: Some("255.255.255.255"),
                port: Port::Absent,
            },
            // The zone half keeps its case; the address half does not.
            Row {
                url: "http://[fe80::1%tESt]:1234/keys",
                split_err: None,
                host: Some("fe80::1%tESt"),
                port: Port::Value(1234),
            },
            Row {
                url: "http://[::1]/x",
                split_err: None,
                host: Some("::1"),
                port: Port::Absent,
            },
            Row {
                url: "http://[::1]:80/x",
                split_err: None,
                host: Some("::1"),
                port: Port::Value(80),
            },
            Row {
                url: "http://[fe80::1%]/x",
                split_err: Some("'fe80::1%' does not appear to be an IPv4 or IPv6 address"),
                host: None,
                port: Port::Absent,
            },
            Row {
                url: "http://x",
                split_err: None,
                host: Some("x"),
                port: Port::Absent,
            },
            Row {
                url: "",
                split_err: None,
                host: None,
                port: Port::Absent,
            },
            Row {
                url: "http:///x",
                split_err: None,
                host: None,
                port: Port::Absent,
            },
            // No host but a real port: the two accessors are independent.
            Row {
                url: "http://:80/x",
                split_err: None,
                host: None,
                port: Port::Value(80),
            },
            // Port = everything after the first colon, so this is a refusal.
            Row {
                url: "http://a.example:80:90/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'80:90'"),
            },
            Row {
                url: "http://[fe80::1]:x/x",
                split_err: None,
                host: Some("fe80::1"),
                port: Port::Cast("'x'"),
            },
            Row {
                url: "http://a.example:1_0/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'1_0'"),
            },
            Row {
                url: "http://a.example:065536/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::OutOfRange,
            },
            Row {
                url: "http://a.example:+8/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("'+8'"),
            },
            Row {
                url: "http://a.example: 8/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::Cast("' 8'"),
            },
            Row {
                url: "http://EXAMPLE.COM./x",
                split_err: None,
                host: Some("example.com."),
                port: Port::Absent,
            },
            Row {
                url: "http://[vX.y]/x",
                split_err: Some("IPvFuture address is invalid"),
                host: None,
                port: Port::Absent,
            },
            Row {
                url: "http://[v1a.z]/x",
                split_err: None,
                host: Some("v1a.z"),
                port: Port::Absent,
            },
            Row {
                url: "http://a:b@c/x",
                split_err: None,
                host: Some("c"),
                port: Port::Absent,
            },
            Row {
                url: "HTTP://A.B/x",
                split_err: None,
                host: Some("a.b"),
                port: Port::Absent,
            },
            Row {
                url: "ftp://a.b/x",
                split_err: None,
                host: Some("a.b"),
                port: Port::Absent,
            },
            // No netloc at all: "a.b" is read as a path, so neither accessor
            // has anything to say.
            Row {
                url: "a.b:80",
                split_err: None,
                host: None,
                port: Port::Absent,
            },
            Row {
                url: "//h:1/",
                split_err: None,
                host: Some("h"),
                port: Port::Value(1),
            },
            Row {
                url: "http://[::1]%25x/x",
                split_err: Some("Invalid IPv6 URL"),
                host: None,
                port: Port::Absent,
            },
            Row {
                url: "http://ho st:80/x",
                split_err: None,
                host: Some("ho st"),
                port: Port::Value(80),
            },
            Row {
                url: "http://h%20st:80/x",
                split_err: None,
                host: Some("h%20st"),
                port: Port::Value(80),
            },
            // Bignum: passes the cast, fails the range. A u64 parse failure must
            // therefore answer out-of-range, not "could not be cast".
            Row {
                url: "http://a.example:99999999999999999999/x",
                split_err: None,
                host: Some("a.example"),
                port: Port::OutOfRange,
            },
            Row {
                url: "http://u@h:443/p",
                split_err: None,
                host: Some("h"),
                port: Port::Value(443),
            },
            Row {
                url: "http://[fe80::1%eth0]/x",
                split_err: None,
                host: Some("fe80::1%eth0"),
                port: Port::Absent,
            },
            Row {
                url: "http://[fe80::1%25eth0]/x",
                split_err: None,
                host: Some("fe80::1%25eth0"),
                port: Port::Absent,
            },
            // str.lower() is Unicode lowercasing, SpecialCasing included. Each
            // of these would answer differently under to_ascii_lowercase().
            Row {
                url: "http://ÄÖ.example/x",
                split_err: None,
                host: Some("äö.example"),
                port: Port::Absent,
            },
            Row {
                url: "http://İ.x/y",
                split_err: None,
                host: Some("i̇.x"),
                port: Port::Absent,
            },
            Row {
                url: "http://ẞ.de/x",
                split_err: None,
                host: Some("ß.de"),
                port: Port::Absent,
            },
            Row {
                url: "http://Σΐ.gr/x",
                split_err: None,
                host: Some("σΐ.gr"),
                port: Port::Absent,
            },
            Row {
                url: "http://ǅ.rs/x",
                split_err: None,
                host: Some("ǆ.rs"),
                port: Port::Absent,
            },
            Row {
                url: "http://ﬀ.be/x",
                split_err: None,
                host: Some("ﬀ.be"),
                port: Port::Absent,
            },
            Row {
                url: "http://꞊A.ng/x",
                split_err: None,
                host: Some("꞊a.ng"),
                port: Port::Absent,
            },
            Row {
                url: "http://K.k/z",
                split_err: None,
                host: Some("k.k"),
                port: Port::Absent,
            },
        ]
    }

    #[test]
    fn hostname_and_port_answer_every_oracle_row() {
        for row in rows() {
            let split = match py_urlsplit(row.url, "", true) {
                Ok(split) => {
                    assert!(
                        row.split_err.is_none(),
                        "{}: the oracle's split raised {:?} but this port accepted it",
                        row.url,
                        row.split_err.unwrap()
                    );
                    split
                }
                Err(error) => {
                    let want = row.split_err.unwrap_or_else(|| {
                        panic!("{}: this port raised {:?} where the oracle answered", row.url, error)
                    });
                    assert_eq!(want, error.message, "{}: split refusal", row.url);
                    continue;
                }
            };
            assert_eq!(
                row.host.map(str::to_string),
                split.hostname(),
                "{}: hostname",
                row.url
            );
            match row.port {
                Port::Value(want) => {
                    assert_eq!(
                        Ok(Some(want)),
                        split.port(),
                        "{}: port must be {want}",
                        row.url
                    )
                }
                Port::Absent => assert_eq!(
                    Ok(None),
                    split.port(),
                    "{}: port must be absent",
                    row.url
                ),
                Port::Cast(repr) => {
                    let error = split
                        .port()
                        .expect_err(&format!("{}: port must refuse", row.url));
                    assert_eq!(
                        format!("{CAST_PREFIX}{repr}"),
                        error.message,
                        "{}: cast refusal",
                        row.url
                    );
                }
                Port::OutOfRange => {
                    let error = split
                        .port()
                        .expect_err(&format!("{}: port must refuse", row.url));
                    assert_eq!(
                        "Port out of range 0-65535",
                        error.message,
                        "{}: range refusal",
                        row.url
                    );
                }
            }
        }
    }

    #[test]
    fn hostinfo_keeps_the_two_shapes_apart() {
        // The bracketed and unbracketed branches take the port from different
        // places; a single `rsplit(':')` would pass one row and fail the other.
        let bracketed = py_urlsplit("http://[::1]:8080/x", "", true).expect("bracketed");
        assert_eq!(Some("::1".to_string()), bracketed.hostname());
        assert_eq!(Ok(Some(8080)), bracketed.port());
        let plain = py_urlsplit("http://a.example:8080/x", "", true).expect("plain");
        assert_eq!(Some("a.example".to_string()), plain.hostname());
        assert_eq!(Ok(Some(8080)), plain.port());
        // Last `@` wins, so a password containing `@` cannot fake a host.
        let at = py_urlsplit("http://user:pa@ss@[::1]:9/x", "", true).expect("at");
        assert_eq!(Some("::1".to_string()), at.hostname());
        assert_eq!(Ok(Some(9)), at.port());
    }

    #[test]
    fn py_urlsplit_matches_python_bracket_parity() {
        // One extra `]`: both brackets are present, so Python's parity check
        // passes and the netloc keeps the stray bracket.
        let split = py_urlsplit("http://[::1]:80]/x", "", true).unwrap();
        assert_eq!(split.netloc, "[::1]:80]");
        assert_eq!(split.path, "/x");
        // An unclosed `[` is a ValueError.
        assert!(py_urlsplit("http://[::1", "", true).is_err());
        assert_eq!(py_urlsplit("//x", "http", true).unwrap().scheme, "http");
    }

    #[test]
    fn py_url_unparse_shapes_match_python() {
        assert_eq!(py_urlunsplit("http", "a", "", "", ""), "http://a");
        assert_eq!(py_urlunsplit("ws", "", "/c", "", ""), "ws:///c");
        // Two `//`s of its own plus the empty-netloc branch's doubling.
        assert_eq!(py_urlunsplit("", "", "//x", "", ""), "////x");
        assert_eq!(py_urlunparse("http", "a", "", "", "q", ""), "http://a?q");
        assert_eq!(py_urlunparse("http", "a", "/x", "p", "", ""), "http://a/x;p");
        // `_splitparams` with no `/` in the path: the first `;` anywhere is
        // the separator.
        let (split, params) = py_urlparse("x;y", "", true).unwrap();
        assert_eq!(split.path, "x");
        assert_eq!(params, "y");
        let (split, params) = py_urlparse("http://a/x;y", "", true).unwrap();
        assert_eq!(split.path, "/x");
        assert_eq!(params, "y");
    }

    struct LoopbackRow {
        text: &'static str,
        /// What `ipaddress.ip_address(text).is_loopback` answered. A Python
        /// `ValueError` is `None` here — that is the third state, "this is not
        /// an IP literal", and it must never be read as `Some(false)`.
        expected: Option<bool>,
    }

    /// Both oracle passes, copied verbatim from `.venv/bin/python` (3.12.15)
    /// output rather than written from memory. Four of these rows — `'1'`,
    /// `'::ffff:127.1'`, `'::0.0.0.1'`, `'::ffff:0:127.0.0.1'` — had no Rust
    /// measurement when [`host_is_loopback`] was written, which is exactly why
    /// they are in the table: a row nobody asks is a row nobody answers.
    fn loopback_rows() -> Vec<LoopbackRow> {
        let rows: Vec<(&str, Option<bool>)> = vec![
            // The whole /8, not just `.1`: Python's v4 loopback is a network test.
            ("127.0.0.1", Some(true)),
            ("127.1.2.3", Some(true)),
            ("127.0.0.0", Some(true)),
            ("127.255.255.255", Some(true)),
            // The two neighbours, so an off-by-one network mask shows up, and
            // one refusal per `IP_MIN`/`IP_MAX`-shaped input: Python's v4
            // parser checks the octet bound before it ever considers v6.
            ("128.0.0.1", Some(false)),
            ("126.0.0.1", Some(false)),
            ("256.0.0.1", None),
            ("10.0.0.1", Some(false)),
            ("192.168.1.13", Some(false)),
            ("169.254.1.1", Some(false)),
            // Load-bearing: `parse_description` has a *separate* `or host ==
            // '0.0.0.0'` clause, and it only makes sense because
            // `is_loopback` says False for the unspecified address.
            ("0.0.0.0", Some(false)),
            ("255.255.255.255", Some(false)),
            // v6 literals.
            ("::1", Some(true)),
            ("::", Some(false)),
            ("fe80::1", Some(false)),
            ("2001:db8::1", Some(false)),
            // Rust's `Ipv6Addr::is_loopback` says false for this one; the
            // mapped-IPv4 arm in the implementation is what makes it agree.
            ("::ffff:127.0.0.1", Some(true)),
            ("::ffff:127.1.2.3", Some(true)),
            ("::ffff:126.0.0.1", Some(false)),
            ("::ffff:128.0.0.1", Some(false)),
            ("::ffff:0.0.0.0", Some(false)),
            ("::ffff:10.0.0.1", Some(false)),
            // v4-compatible spelling: not mapped, and loopback by value
            // (`int(self) == 1` in Python).
            ("::0.0.0.1", Some(true)),
            // Mapped-prefix addresses that only *look* like embedded IPv4.
            ("::127.0.0.1", Some(false)),
            ("64:ff9b::127.0.0.1", Some(false)),
            ("::ffff:0:127.0.0.1", Some(false)),
            // Scope ids. `Ipv6Addr::from_str` rejects `%`, so without the
            // split every one of these would answer `None` — "a hostname" —
            // and the description reader would keep `::1` instead of swapping
            // in the peer that answered.
            ("::1%lo", Some(true)),
            ("::1%eth0", Some(true)),
            ("::ffff:127.0.0.1%eth0", Some(true)),
            ("::ffff:126.0.0.1%eth0", Some(false)),
            ("fe80::1%eth0", Some(false)),
            ("2001:db8::1%wlan0", Some(false)),
            // v4 with a scope, and a doubled/empty scope, are ValueError there.
            ("127.0.0.1%eth0", None),
            ("fe80::1%%eth0", None),
            ("::1%", None),
            ("::1%lo%lo", None),
            // Names and the empty string: Python's `except ValueError: pass`.
            ("localhost", None),
            ("Macast.local", None),
            ("a.example", None),
            ("", None),
            (" ", None),
            // Whitespace either side is refused, not stripped.
            ("127.0.0.1 ", None),
            (" 127.0.0.1", None),
            // A port belongs in the netloc, not in the address.
            ("127.0.0.1:8080", None),
            // Leading zeros: Python refuses them (they used to mean octal), and
            // so does Rust's v4 parser.
            ("010.1.1.1", None),
            ("127.0.0.01", None),
            ("0x7f.0.0.1", None),
            ("127.0.0.1.", None),
            // Length, both directions.
            ("1", None),
            ("1.2", None),
            ("1.2.3", None),
            ("1.2.3.4.5", None),
            ("999.1.1.1", None),
            ("4294967295", None),
            ("-1", None),
            // Separators without operands, and a non-ASCII colon.
            ("1..2", None),
            ("1.2.3.-4", None),
            ("[:：]", None),
            // Space inside the string.
            ("ab cd", None),
            // An embedded v4 that is not a dotted quad.
            ("::ffff:127.1", None),
            ("::ffff:127.0.0.1.2", None),
        ];
        rows.into_iter()
            .map(|(text, expected)| LoopbackRow { text, expected })
            .collect()
    }

    #[test]
    fn host_is_loopback_matches_ipaddress() {
        for row in loopback_rows() {
            assert_eq!(
                row.expected,
                host_is_loopback(row.text),
                "ipaddress.ip_address({:?}).is_loopback said {:?}",
                row.text,
                row.expected
            );
        }
    }

    #[test]
    fn host_is_loopback_table_asks_both_third_state_questions() {
        // A table can pass while every `None` row is `None` for the wrong
        // reason, so this asks for the two answers that have to stay apart:
        // a hostname, and a real address that is not loopback.
        assert_eq!(None, host_is_loopback("Macast.local"));
        assert_eq!(Some(false), host_is_loopback("192.168.1.13"));
        // And the `0.0.0.0` row that Python's second clause exists to catch.
        assert_eq!(Some(false), host_is_loopback("0.0.0.0"));
        // The row the mapped arm buys: removing it does not merely print a
        // different word, it keeps a host `parse_description` must replace.
        assert_eq!(Some(true), host_is_loopback("::ffff:127.0.0.1"));
        assert_eq!(Some(true), host_is_loopback("::1%lo"));
    }
}
