//! Which chunk of mpv's IPC output is worth parsing, and what the log says when
//! it isn't.
//!
//! The reference is the inner loop of `start_ipc` (`macast_renderer/mpv.py:420-443`).
//! Reading it as Rust would be a mistake, because three of its behaviours come
//! from *how Python scopes names*, not from its control flow:
//!
//! - `res` is reset at connect time and cleared in a `finally`, so a chunk that
//!   cannot be decoded is discarded whole — including the well-formed lines that
//!   arrived inside it.
//! - `msgs` is reset at connect time (`msgs = None`), so a decode failure on the
//!   first chunk of a connection reports the list as `None`.
//! - **`msg` is never reset.** It is the loop variable of `for msg in msgs`, so it
//!   outlives its own batch and outlives the connection: a failure on a later
//!   chunk prints *the last line of the previous batch*. That is not a Rust
//!   variable's behaviour, and the log line a user reads depends on it.
//!
//! The consequence of the third one is the sharpest: on the very first failure of
//! a thread's life `msg` has never been bound, so the `except` arm itself raises
//! `UnboundLocalError: cannot access local variable 'msg' where it is not
//! associated with a value`. It does so **after** the first `logger.error` has
//! already run and **before** the other two, so the log gets one line of a three-line
//! story and the IPC thread dies — leaving `Macast` with a player that is playing and
//! a control point that is no longer told about it. [`Failure::log_lines`]
//! reproduces the truncation and [`Failure::kills_the_reader`] says when it happens,
//! so the transport half has to deal with that branch explicitly instead of
//! discovering it on a user's machine (AGENTS.md §4.2, §5).
//!
//! All strings here are measured against the interpreter this app runs on
//! (`.venv`, CPython 3.12.15), not paraphrased: the `UnicodeDecodeError` texts in
//! [`decode_error`] and the shape of `f"…{msgs}"` in [`ListLocal::render`].

use crate::pyops;
use serde_json::Value;

/// `msg` — a `str` local that Python never re-initialises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrLocal {
    /// Never bound in this thread. Reading it raises, and the raise happens while
    /// the `except` arm is building its own arguments.
    NeverBound,
    Set(String),
}

/// `msgs` — a `list` local that is set to `None` at every new connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListLocal {
    Nothing,
    Set(Vec<String>),
}

impl StrLocal {
    /// What `f"decode error data: {msg}"` interpolates, or `None` when the f-string
    /// raises instead.
    pub fn render(&self) -> Option<String> {
        match self {
            StrLocal::NeverBound => None,
            StrLocal::Set(text) => Some(text.clone()),
        }
    }
}

/// A fresh socket has never run the `for` loop, so `msg` is exactly that.
impl Default for StrLocal {
    fn default() -> Self {
        StrLocal::NeverBound
    }
}

/// A fresh socket starts with `msgs = None`.
impl Default for ListLocal {
    fn default() -> Self {
        ListLocal::Nothing
    }
}

impl ListLocal {
    /// What `f"decode error data list: {msgs}"` interpolates. A list renders as
    /// *Python's* `repr` of it — quotes on the elements included — which is why
    /// this goes through [`pyops::py_str`] rather than Rust's `Debug`.
    pub fn render(&self) -> String {
        match self {
            ListLocal::Nothing => "None".to_string(),
            ListLocal::Set(lines) => {
                pyops::py_str(&Value::Array(
                    lines.iter().map(|line| Value::String(line.clone())).collect(),
                ))
            }
        }
    }
}

/// The three `logger.error` lines Python writes when the batch raises, and the
/// truncation of those lines when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `str(e)` — mpv's own words if a line decoded but was not JSON, a
    /// [`decode_error`] string if the bytes were not UTF-8, or whatever
    /// [`crate::state::PlayerState::apply`] reported for a line that decoded fine.
    pub message: String,
    pub line: StrLocal,
    pub batch: ListLocal,
    /// Lines of this batch that reached `update_state` before the raise. `0` for a
    /// decode failure, since nothing of the chunk was parsed at all.
    pub applied_before: usize,
}

impl Failure {
    /// The first chunk a thread ever fails to decode kills it: the unbound `msg`
    /// raises inside the `except` arm.
    pub fn kills_the_reader(&self) -> bool {
        matches!(self.line, StrLocal::NeverBound)
    }

    /// The lines the log actually gets. Three, unless this failure is the one that
    /// kills the reader — then one, because the second f-string is what raises.
    pub fn log_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("decode error: {}", self.message)];
        let Some(text) = self.line.render() else {
            return lines;
        };
        lines.push(format!("decode error data: {}", text));
        lines.push(format!("decode error data list: {}", self.batch.render()));
        lines
    }
}

/// Why the inner `while self.ipc_running` loop ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Close {
    /// `recv` returned `b''` — mpv closed the socket. Silent in the reference; the
    /// `logger.error("mpv ipc stopped")` that follows belongs to the caller.
    Eof,
    /// `recv` raised. Python logs `logger.debug(e)` and then breaks, so the
    /// exception text is a DEBUG line and nothing more.
    Error(String),
}

impl Close {
    /// The line the reference writes on this path, if any.
    pub fn log_line(&self) -> Option<String> {
        match self {
            Close::Eof => None,
            Close::Error(text) => Some(text.clone()),
        }
    }
}

/// What one chunk was worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// The chunk does not end on a newline, so nothing was parsed and the bytes are
    /// still in the buffer.
    Buffered,
    /// The buffer was split and every line was handed to `apply`.
    Applied(Vec<String>),
    /// The inner loop is over. Whatever is buffered is abandoned unparsed, and the
    /// caller must [`Feed::reconnect`] before feeding again.
    Closed(Close),
    /// Python's `except` arm ran. The buffer was cleared either way — including the
    /// lines that would have parsed.
    Failed(Failure),
}

/// `res`, plus the two names that outlive it.
///
/// `Default` is a freshly connected socket: empty buffer, `msgs = None`, `msg`
/// unbound.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    buf: Vec<u8>,
    line: StrLocal,
    batch: ListLocal,
}

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    /// `res = b''; msgs = None` — the two resets `start_ipc` does after a
    /// successful `connect`. **`msg` deliberately survives**: it is not reset here,
    /// and it is not reset on the next iteration of the outer loop either, so the
    /// new connection's first failure still prints the previous session's last line.
    pub fn reconnect(&mut self) {
        self.buf.clear();
        self.batch = ListLocal::Nothing;
    }

    /// Bytes still held from a chunk that never completed a line. Read-only, for
    /// diagnostics; a `Closed` does not clear it, because neither does the
    /// reference — the next `reconnect` does.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// `logger.debug(e); break` — the socket raised.
    pub fn recv_failed(&mut self, error: &str) -> Received {
        Received::Closed(Close::Error(error.to_string()))
    }

    /// One `recv` result, applied through `apply`.
    ///
    /// `apply` is `update_state`: it gets each line in order and returns `Err(str(e))`
    /// for the exception Python would have raised. Its first `Err` ends the whole
    /// batch — the remaining lines of the chunk are never applied, which is the
    /// difference between this port and a loop that keeps going.
    pub fn push<F>(&mut self, chunk: &[u8], mut apply: F) -> Received
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // `if data == b'': break` comes *before* `res += data`, so an EOF contributes
        // nothing to the buffer and abandons what is already in it.
        if chunk.is_empty() {
            return Received::Closed(Close::Eof);
        }
        self.buf.extend_from_slice(chunk);
        // The *new chunk's* last byte decides. Not the buffer's, and not "does the
        // buffer contain a newline": mpv can hand us a line's tail and the head of
        // the next one in the same read, and we still wait.
        if chunk.last() != Some(&b'\n') {
            return Received::Buffered;
        }

        let text = match std::str::from_utf8(&self.buf) {
            Ok(text) => text.to_string(),
            Err(err) => {
                let failure = Failure {
                    message: decode_error(&self.buf, err.valid_up_to(), err.error_len()),
                    line: self.line.clone(),
                    batch: self.batch.clone(),
                    applied_before: 0,
                };
                self.buf.clear();
                return Received::Failed(failure);
            }
        };

        let lines: Vec<String> = text.trim().split('\n').map(str::to_string).collect();
        self.batch = ListLocal::Set(lines.clone());
        for (index, line) in lines.iter().enumerate() {
            self.line = StrLocal::Set(line.clone());
            if let Err(message) = apply(line) {
                self.buf.clear();
                return Received::Failed(Failure {
                    message,
                    line: self.line.clone(),
                    batch: self.batch.clone(),
                    applied_before: index,
                });
            }
        }
        self.buf.clear();
        Received::Applied(lines)
    }
}

/// `str(UnicodeDecodeError)` for the bytes Python could not decode.
///
/// CPython names **one byte** for a one-byte mistake, a **range** when the run of bad
/// bytes is longer, and a range ending at the last byte when the sequence runs off the
/// end of the buffer. Rust's `Utf8Error` reports the same facts as `valid_up_to` and
/// `error_len`. Every expectation in the table below was produced by feeding the *same
/// chunk the protocol produces* — the bytes plus their terminating newline — through
/// the interpreter this app ships (3.12.15): `b'\xff\n'`, `b'a\xff\n'`, `b'\x80\n'`,
/// `b'\xc0\xaf\n'`, `b'\xf5\x80\x80\x80\n'`, `b'\xc3(\n'`, `b'\xed\xa0\x80\n'`,
/// `b'\xc3\n'`, `b'\xe2\x82\n'`, `b'\xf0\x90\n'`. The byte that gets named is the one
/// at `valid_up_to`, with the reason decided by what kind of byte it is.
///
/// Note what that list implies: `push` only ever decodes a buffer whose last byte is
/// `\n`, so a truncated multi-byte sequence always has a following invalid
/// continuation byte and CPython reports *that* instead of the end of the buffer.
/// 「unexpected end of data」 is therefore **unreachable through the protocol**; the
/// arm below that prints it is kept because `decode_error` is a general reading of
/// `UnicodeDecodeError`, and a port that silently dropped a real CPython sentence
/// would be worse than one that keeps a defensive branch.
fn decode_error(bytes: &[u8], valid_up_to: usize, error_len: Option<usize>) -> String {
    let first = bytes[valid_up_to];
    // `error_len() == None` is Rust's "unexpected end of input", which is also
    // CPython's `unexpected end of data`.
    let range = match error_len {
        None => (bytes.len() - 1, "unexpected end of data"),
        Some(length) => {
            // A byte that could legally start a sequence means whatever followed it was
            // wrong; anything else means the start byte itself was never legal. `0xed` in
            // a surrogate pair counts as the former here, and CPython agrees: it blames
            // the continuation while naming `0xed`.
            let reason = if (0xC2..=0xF4).contains(&first) {
                "invalid continuation byte"
            } else {
                "invalid start byte"
            };
            // `error_len` counts the bytes CPython gives up on, so the last one it
            // blames sits at `valid_up_to + length - 1`.
            (valid_up_to + length - 1, reason)
        }
    };
    let (last, reason) = range;
    if last == valid_up_to {
        return format!(
            "'utf-8' codec can't decode byte {byte:#04x} in position {valid_up_to}: {reason}",
            byte = first
        );
    }
    format!(
        "'utf-8' codec can't decode bytes in position {valid_up_to}-{last}: {reason}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that accepts everything, for the framing questions.
    fn always_ok(_line: &str) -> Result<(), String> {
        Ok(())
    }

    /// The reference's own three log lines, in the order they are written.
    const HEAD: &str = "decode error: boom";

    /// `str(UnicodeDecodeError)` for the chunk `b'\xff\n'`, measured in 3.12.15. The
    /// two tests that print it differ only in what the *previous* batch left bound.
    const BYTE_ERROR: &str =
        "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte";

    #[test]
    fn a_chunk_that_does_not_end_on_a_newline_is_not_parsed() {
        let mut feed = Feed::new();
        assert_eq!(feed.push(b"{\"a\": 1}", always_ok), Received::Buffered);
        assert_eq!(feed.buffered(), 8);
        // A newline *inside* the chunk does not release it either: the decision is
        // the last byte of the new chunk, not the content of the buffer.
        assert_eq!(feed.push(b"\n{\"b\": 2}", always_ok), Received::Buffered);
        assert_eq!(feed.buffered(), 17);
        // Until a chunk ends on one — and then everything buffered goes at once. The
        // two deferred pushes each *begin* with a newline, so the buffer at this point
        // is three complete lines, not one line and a ragged tail.
        let received = feed.push(b"\n{\"c\": 3}\n", always_ok);
        assert_eq!(
            received,
            Received::Applied(vec![
                "{\"a\": 1}".to_string(),
                "{\"b\": 2}".to_string(),
                "{\"c\": 3}".to_string()
            ])
        );
        assert_eq!(feed.buffered(), 0);
    }

    #[test]
    fn strip_happens_before_the_split_so_the_terminating_newline_is_not_a_line() {
        let mut feed = Feed::new();
        let received = feed.push(b"{\"a\": 1}\n{\"b\": 2}\n", always_ok);
        assert_eq!(
            received,
            Received::Applied(vec!["{\"a\": 1}".to_string(), "{\"b\": 2}".to_string()])
        );
    }

    #[test]
    fn a_newline_only_chunk_is_one_empty_line_not_no_line() {
        // `''.strip().split('\n')` is `['']`, so the reference does call
        // `update_state('')`, and `json.loads('')` is what raises there. A port that
        // treated a blank chunk as nothing would silently drop the failure.
        let mut feed = Feed::new();
        assert_eq!(feed.push(b"\n", always_ok), Received::Applied(vec!["".to_string()]));
    }

    #[test]
    fn an_eof_ends_the_inner_loop_and_abandons_what_is_buffered() {
        let mut feed = Feed::new();
        assert_eq!(feed.push(b"{\"a\": 1}", always_ok), Received::Buffered);
        assert_eq!(feed.push(b"", always_ok), Received::Closed(Close::Eof));
        // Silent in the reference: only the caller's "mpv ipc stopped" follows.
        assert_eq!(Close::Eof.log_line(), None);
        // Python leaves `res` alone on the break; the next connect clears it.
        assert_eq!(feed.buffered(), 8);
        feed.reconnect();
        assert_eq!(feed.buffered(), 0);
    }

    #[test]
    fn a_raising_recv_ends_the_loop_with_a_debug_line() {
        let mut feed = Feed::new();
        let received = feed.recv_failed("[Errno 54] Connection reset by peer");
        assert_eq!(
            received,
            Received::Closed(Close::Error(
                "[Errno 54] Connection reset by peer".to_string()
            ))
        );
        assert_eq!(
            Close::Error("x".to_string()).log_line().as_deref(),
            Some("x")
        );
    }

    #[test]
    fn the_buffer_is_cleared_after_a_failed_parse_attempt_too() {
        // `finally: res = b''` runs on the exception path, so a poison line does not
        // poison every line after it forever.
        let mut feed = Feed::new();
        let received = feed.push(b"{\"a\": 1}\n", |_| Err("boom".to_string()));
        assert!(matches!(received, Received::Failed(_)));
        assert_eq!(feed.buffered(), 0);
        assert_eq!(
            feed.push(b"{\"b\": 2}\n", always_ok),
            Received::Applied(vec!["{\"b\": 2}".to_string()])
        );
    }

    #[test]
    fn the_first_failure_of_a_thread_prints_one_line_and_kills_the_reader() {
        // Measured by replaying `macast_renderer/mpv.py`'s loop in the interpreter that
        // runs it: the handler raises `UnboundLocalError: cannot access local variable
        // 'msg' where it is not associated with a value` on the *second* `logger.error`,
        // so the user reads one line and the IPC thread is gone. A line that raises
        // inside `update_state` is the other shape (see
        // `a_line_that_raises_names_itself_and_lives`): by then `msg` is bound.
        let mut feed = Feed::new();
        let received = feed.push(b"\xff\n", always_ok);
        let Received::Failed(failure) = received else {
            panic!("0xff must not decode");
        };
        assert!(failure.kills_the_reader());
        assert_eq!(failure.applied_before, 0);
        // One line, not three.
        assert_eq!(failure.log_lines(), vec![format!("decode error: {BYTE_ERROR}")]);
    }

    #[test]
    fn a_decode_failure_names_the_previous_batch_not_the_current_one() {
        // The shape that makes this module's docstring true: the raise happens before
        // either name is rebound, so `msg` and `msgs` are whatever the last *decoded*
        // batch ended on and the log blames lines that were fine.
        let mut feed = Feed::new();
        assert!(feed
            .push(b"{\"a\": 1}\n{\"b\": 2}\n", always_ok)
            .is_applied());
        let received = feed.push(b"\xff\n", always_ok);
        let Received::Failed(failure) = received else {
            panic!("0xff must not decode");
        };
        assert!(!failure.kills_the_reader());
        assert_eq!(
            failure.log_lines(),
            vec![
                format!("decode error: {BYTE_ERROR}"),
                "decode error data: {\"b\": 2}".to_string(),
                "decode error data list: ['{\"a\": 1}', '{\"b\": 2}']".to_string(),
            ]
        );
    }

    #[test]
    fn a_line_that_raises_names_itself_and_the_reader_lives() {
        // The other failure. `res.decode().strip().split('\n')` succeeded here, so both
        // names got rebound *before* `update_state` raised: measured, the log reports the
        // current line and the current one-element list, and `msg` is bound so nothing
        // dies. A test that claimed staleness on this path would be describing the test
        // above while pinning something the reference never does.
        let mut feed = Feed::new();
        assert!(feed
            .push(b"{\"a\": 1}\n{\"b\": 2}\n", always_ok)
            .is_applied());
        let received = feed.push(b"not json\n", |_| Err("boom".to_string()));
        let Received::Failed(failure) = received else {
            panic!("the line was meant to raise");
        };
        assert!(!failure.kills_the_reader());
        assert_eq!(failure.line, StrLocal::Set("not json".to_string()));
        assert_eq!(
            failure.log_lines(),
            vec![
                HEAD.to_string(),
                "decode error data: not json".to_string(),
                "decode error data list: ['not json']".to_string(),
            ]
        );
    }

    #[test]
    fn a_reconnect_resets_the_list_but_not_the_line() {
        // `msgs = None` is per connection; `msg` is per thread. A test that reset
        // both would report a stale line as `None` and read as a cleaner log than the
        // reference ever produces. Only the decode path can show this — on the other
        // path the new batch rebinds the list as well.
        let mut feed = Feed::new();
        assert!(feed.push(b"{\"a\": 1}\n", always_ok).is_applied());
        feed.reconnect();
        let received = feed.push(b"\xff\n", always_ok);
        let Received::Failed(failure) = received else {
            panic!("0xff must not decode");
        };
        assert_eq!(failure.batch, ListLocal::Nothing);
        assert_eq!(failure.line, StrLocal::Set("{\"a\": 1}".to_string()));
        assert_eq!(failure.log_lines()[1], "decode error data: {\"a\": 1}".to_string());
        assert_eq!(
            failure.log_lines()[2],
            "decode error data list: None".to_string()
        );
    }

    #[test]
    fn a_bad_line_swallows_the_rest_of_its_batch() {
        // `for msg in msgs` lives inside the `try`, so the raise ends the whole
        // chunk. This is why `state::apply` aborting is a *batch* event, not a
        // per-line one: AGENTS.md's "a bad value in the second line of a chunk
        // swallows the third and fourth as well".
        let mut seen: Vec<String> = Vec::new();
        let mut feed = Feed::new();
        let received = feed.push(b"one\ntwo\nthree\n", |line| {
            seen.push(line.to_string());
            if line == "two" {
                Err("boom".to_string())
            } else {
                Ok(())
            }
        });
        let Received::Failed(failure) = received else {
            panic!("the second line was meant to raise");
        };
        assert_eq!(seen, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(failure.applied_before, 1);
        assert_eq!(failure.line, StrLocal::Set("two".to_string()));
        assert_eq!(
            failure.batch,
            ListLocal::Set(vec![
                "one".to_string(),
                "two".to_string(),
                "three".to_string()
            ])
        );
    }

    #[test]
    fn undecodable_bytes_report_cpythons_own_sentence() {
        // Each expectation below is the `str()` of the exception CPython 3.12.15 raises
        // for the same chunk `push` would hand to `res.decode()` — the listed bytes plus
        // their terminating newline — measured in `.venv`.
        for (bytes, expect) in [
            (
                b"\xff".to_vec(),
                "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte",
            ),
            (
                b"a\xff".to_vec(),
                "'utf-8' codec can't decode byte 0xff in position 1: invalid start byte",
            ),
            (
                b"\x80".to_vec(),
                "'utf-8' codec can't decode byte 0x80 in position 0: invalid start byte",
            ),
            (
                b"\xc0\xaf".to_vec(),
                "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte",
            ),
            (
                b"\xf5\x80\x80\x80".to_vec(),
                "'utf-8' codec can't decode byte 0xf5 in position 0: invalid start byte",
            ),
            (
                b"\xc3(".to_vec(),
                "'utf-8' codec can't decode byte 0xc3 in position 0: invalid continuation byte",
            ),
            (
                b"\xed\xa0\x80".to_vec(),
                "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte",
            ),
            (
                b"\xc3".to_vec(),
                "'utf-8' codec can't decode byte 0xc3 in position 0: invalid continuation byte",
            ),
            (
                b"\xe2\x82".to_vec(),
                "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte",
            ),
            (
                b"\xf0\x90".to_vec(),
                "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte",
            ),
        ] {
            let mut chunk = bytes.clone();
            chunk.push(b'\n');
            let mut feed = Feed::new();
            let received = feed.push(&chunk, always_ok);
            let Received::Failed(failure) = received else {
                panic!("{bytes:?} must not decode");
            };
            assert_eq!(failure.message, expect, "{bytes:?}");
            // Nothing of the chunk was parsed, so there is no current line: the
            // batch reported is the previous one, and on a first chunk that is the
            // unbound `msg` — the reader-killing shape, even though the bytes are
            // nothing to do with mpv's protocol.
            assert_eq!(failure.applied_before, 0);
            assert!(failure.kills_the_reader());
        }
    }

    #[test]
    fn a_protocol_chunk_never_reports_the_end_of_the_data() {
        // `push` decodes only when the buffer's last byte is `\n`, so a truncated
        // multi-byte sequence always has an invalid continuation byte after it and
        // CPython blames *that*. Measured: `.venv` 3.12.15 says 「invalid continuation
        // byte」 for `b'\xc3\n'` and `b'\xe2\x82\n'`, while Rust's `Utf8Error` on the
        // same fed chunks reports `Some(1)` and `Some(2)` — never `None`. So the
        // end-of-data arm above is defensive, not reachable: pin that, because a reader
        // of the table could otherwise conclude a half-written character produces that
        // sentence and go looking for a message the app can never log.
        let mut prefixes: Vec<Vec<u8>> = (0u16..256).map(|b| vec![b as u8]).collect();
        for lead in 0xC0u8..=0xFF {
            for tail in [0x41u8, 0x80, 0xBF, 0xF0] {
                prefixes.push(vec![lead, tail]);
                prefixes.push(vec![0x41, lead, tail]);
                prefixes.push(vec![lead]);
            }
        }
        let mut asked = 0;
        for raw in prefixes {
            let mut chunk = raw.clone();
            chunk.push(b'\n');
            let error = match std::str::from_utf8(&chunk) {
                Ok(_) => continue, // ASCII plus a newline: nothing to pin here.
                Err(error) => error,
            };
            asked += 1;
            assert!(
                error.error_len().is_some(),
                "{raw:?} fed as the protocol would: Rust says {:?}, so CPython never \
                 reaches the end of the buffer",
                error.error_len()
            );
            let mut feed = Feed::new();
            let Received::Failed(failure) = feed.push(&chunk, always_ok) else {
                panic!("{raw:?} + newline must not decode");
            };
            assert!(
                !failure.message.contains("unexpected end of data"),
                "{raw:?}: {}",
                failure.message
            );
        }
        // The scan has to ask, or the loop above is a green that proves nothing.
        assert!(asked > 200, "only {asked} chunks were undecodable");
    }

    #[test]
    fn the_defensive_end_of_data_arm_still_quotes_cpython() {
        // Called directly, because the branch it pins cannot be reached through `push`
        // (see the test above). These two sentences are `str()` of the exception
        // CPython 3.12.15 raises for the *raw* bytes, measured in `.venv`; Rust reports
        // the same facts as `error_len() == None`, which is why both forms exist.
        assert_eq!(
            decode_error(b"\xc3", 0, None),
            "'utf-8' codec can't decode byte 0xc3 in position 0: unexpected end of data"
        );
        assert_eq!(
            decode_error(b"\xe2\x82", 0, None),
            "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"
        );
        // A one-byte `error_len` keeps the singular form, a longer run the range.
        assert_eq!(
            decode_error(b"\xc3\n", 0, Some(1)),
            "'utf-8' codec can't decode byte 0xc3 in position 0: invalid continuation byte"
        );
        assert_eq!(
            decode_error(b"\xe2\x82\n", 0, Some(2)),
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
    }

    #[test]
    fn a_stale_batch_is_rendered_as_a_python_list_of_strings() {
        // `f"{msgs}"` calls `repr()` on the elements, so the log shows the quotes.
        let batch = ListLocal::Set(vec!["x".to_string(), "it's".to_string()]);
        assert_eq!(batch.render(), "['x', \"it's\"]");
        assert_eq!(ListLocal::Nothing.render(), "None");
    }

    impl Received {
        fn is_applied(&self) -> bool {
            matches!(self, Received::Applied(_))
        }
    }
}
