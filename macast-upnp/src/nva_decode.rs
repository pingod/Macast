//! NVA frame *reading*: an incremental decoder for the binary channel.
//!
//! Rust counterpart of `nirvana.py`'s `parse_request` (`:905-978`). The encoder lives in
//! [`crate::nva_frames`]; this is the other direction, and it has one property the encoder does
//! not need: **it must survive arriving in pieces**.
//!
//! ## Three behaviours worth stating, because each looks like a bug
//!
//! 1. **A partial frame is not an error, it is a "come back later".** TCP hands us whatever
//!    happens to be in the buffer, so the loop stops at the first frame that does not fit and
//!    keeps the remainder in `cache`. Reporting that as malformed would drop real traffic on
//!    every large payload.
//! 2. **A byte that cannot start a frame advances by exactly one.** The reference does
//!    `index += 1; continue`, so a stream that begins mid-frame (or carries junk) *resynchronises*
//!    at the next plausible header instead of discarding everything that follows. A decoder that
//!    reset the buffer instead would lose the rest of that connection's traffic.
//! 3. **A body that is not JSON is dropped, but the frame still counts as consumed.** The
//!    reference's `try/except/finally` puts `index += …` in the `finally`: the callback is not
//!    invoked, the position still moves. Skipping the advance would loop on the same bytes
//!    forever.
//!
//! ## What is checked, and what is only asserted
//!
//! The reference `assert`s that the flag bytes are in `[2,3]` / `[0,1]`, that `n == 1`, that the
//! field length is 7 and the field name is `"Command"`. An `assert` is a **crash** in Python when
//! it fails; here those are refused frames ([`Decoded::Bad`]) which the loop skips one byte at a
//! time, exactly like an unknown header. That is a deliberate divergence and the safer one: a
//! peer sending a malformed frame should not end the connection.

use crate::nva_frames::{COMMAND_FIELD, RET_CMD, SEND_CMD};

/// One thing a frame said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// `RET_CMD` with no body.
    ResponseEmpty { counter: u32 },
    /// `RET_CMD` carrying JSON.
    Response {
        counter: u32,
        body: serde_json::Value,
    },
    /// `SEND_CMD` with a verb and no arguments.
    Command { counter: u32, method: String },
    /// `SEND_CMD` with a verb and JSON arguments.
    CommandWith {
        counter: u32,
        method: String,
        body: serde_json::Value,
    },
}

/// What one step of the decoder produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// A complete frame.
    Frame(Frame),
    /// The bytes so far cannot complete a frame; wait for more.
    NeedMore,
    /// These bytes are not a frame header. Skip **one** byte and try again.
    Bad,
    /// A complete frame whose body is not JSON. The bytes are consumed, but **no callback
    /// fires** — the reference puts `index += …` in a `finally`, so the frame is swallowed
    /// rather than reported. Kept as its own variant so the decoder can still advance by the
    /// right amount without inventing a `null` body nobody sent.
    Unreported,
}

/// The incremental decoder.
///
/// `cache` is the partial tail from the previous call — the reference keeps it on the connection
/// for the same reason.
#[derive(Debug, Default)]
pub struct Decoder {
    cache: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// The bytes held back from the last call.
    pub fn pending(&self) -> &[u8] {
        &self.cache
    }

    /// Feed bytes, collecting every complete frame.
    ///
    /// The caller renders each frame (`cmd_from_client` / `res_from_client` in the reference);
    /// this returns them in arrival order.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Frame> {
        let mut buffer = std::mem::take(&mut self.cache);
        buffer.extend_from_slice(data);
        let mut frames = Vec::new();
        let mut index = 0usize;
        let length = buffer.len();
        while index < length {
            match decode_at(&buffer, index) {
                Decoded::Frame(frame) => {
                    // How far to advance is a property of the frame's shape, so it is computed
                    // from the bytes again rather than returned twice.
                    index += frame_len(&buffer[index..]).unwrap_or(1);
                    frames.push(frame);
                }
                Decoded::Unreported => {
                    index += frame_len(&buffer[index..]).unwrap_or(1);
                }
                Decoded::NeedMore => break,
                Decoded::Bad => index += 1,
            }
        }
        self.cache = buffer[index..].to_vec();
        frames
    }
}

/// Decode one frame starting at `index`.
pub fn decode_at(buffer: &[u8], index: usize) -> Decoded {
    let rest = match buffer.get(index..) {
        Some(rest) => rest,
        None => return Decoded::NeedMore,
    };
    // The smallest frame is 6 bytes; anything shorter might still grow into one.
    if rest.len() < 6 {
        return Decoded::NeedMore;
    }
    let header = match rest[0] {
        SEND_CMD | RET_CMD => rest[0],
        // Not a header at all: skip a byte and resynchronise.
        _ => return Decoded::Bad,
    };
    let flags = rest[1];
    let counter = u32::from_be_bytes([rest[2], rest[3], rest[4], rest[5]]);

    if header == SEND_CMD {
        if flags != 2 && flags != 3 {
            return Decoded::Bad;
        }
        if rest.len() < 16 {
            return Decoded::NeedMore;
        }
        // `n`, field length, field name, verb length.
        let n = rest[6];
        let field_len = rest[7];
        let field = &rest[8..15];
        if n != 1 || field_len != 7 || field != COMMAND_FIELD {
            return Decoded::Bad;
        }
        let verb_len = rest[15] as usize;
        if rest.len() < 16 + verb_len {
            return Decoded::NeedMore;
        }
        let method = String::from_utf8_lossy(&rest[16..16 + verb_len]).to_string();
        if flags == 2 {
            return Decoded::Frame(Frame::Command { counter, method });
        }
        if rest.len() < 16 + verb_len + 4 {
            return Decoded::NeedMore;
        }
        let body_len = u32::from_be_bytes([
            rest[16 + verb_len],
            rest[17 + verb_len],
            rest[18 + verb_len],
            rest[19 + verb_len],
        ]) as usize;
        if rest.len() < 16 + verb_len + 4 + body_len {
            return Decoded::NeedMore;
        }
        let body = &rest[16 + verb_len + 4..16 + verb_len + 4 + body_len];
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(value) => Decoded::Frame(Frame::CommandWith {
                counter,
                method,
                body: value,
            }),
            // Consumed but not reported. The reference's `finally` advances the index; the
            // callback simply never fires. Returning `Bad` here would resynchronise *inside*
            // the body and produce garbage frames from JSON bytes.
            Err(_) => Decoded::Unreported,
        }
    } else {
        if flags != 0 && flags != 1 {
            return Decoded::Bad;
        }
        if flags == 0 {
            return Decoded::Frame(Frame::ResponseEmpty { counter });
        }
        if rest.len() < 10 {
            return Decoded::NeedMore;
        }
        let body_len = u32::from_be_bytes([rest[6], rest[7], rest[8], rest[9]]) as usize;
        if rest.len() < 10 + body_len {
            return Decoded::NeedMore;
        }
        let body = &rest[10..10 + body_len];
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(value) => Decoded::Frame(Frame::Response {
                counter,
                body: value,
            }),
            Err(_) => Decoded::Unreported,
        }
    }
}

/// How many bytes the frame at the start of `slice` occupies, if it is complete.
fn frame_len(slice: &[u8]) -> Option<usize> {
    if slice.len() < 6 {
        return None;
    }
    let flags = slice[1];
    if slice[0] == RET_CMD {
        return match flags {
            0 => Some(6),
            1 if slice.len() >= 10 => {
                let len = u32::from_be_bytes([slice[6], slice[7], slice[8], slice[9]]) as usize;
                (slice.len() >= 10 + len).then_some(10 + len)
            }
            _ => None,
        };
    }
    if slice.len() < 16 {
        return None;
    }
    let verb_len = slice[15] as usize;
    match flags {
        2 => (slice.len() >= 16 + verb_len).then_some(16 + verb_len),
        3 if slice.len() >= 16 + verb_len + 4 => {
            let at = 16 + verb_len;
            let len = u32::from_be_bytes([slice[at], slice[at + 1], slice[at + 2], slice[at + 3]])
                as usize;
            (slice.len() >= at + 4 + len).then_some(at + 4 + len)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nva_frames::{command, ping, response};

    #[test]
    fn a_frame_arriving_whole_is_decoded_and_leaves_nothing_pending() {
        crate::pin_test_dir();
        let mut decoder = Decoder::new();
        let frames = decoder.feed(&response(5, Some(b"{\"volume\":0}")));
        assert_eq!(
            frames,
            vec![Frame::Response {
                counter: 5,
                body: serde_json::json!({"volume": 0})
            }]
        );
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn a_frame_arriving_in_pieces_waits_instead_of_failing() {
        crate::pin_test_dir();
        // The property this decoder exists for: TCP hands over whatever is in the buffer.
        let framed = command(3, "OnProgress", Some(b"{\"position\":64}"));
        let mut decoder = Decoder::new();
        let mut produced = Vec::new();
        for byte in &framed {
            produced.extend(decoder.feed(&[*byte]));
        }
        assert_eq!(produced.len(), 1, "one frame, not an error per byte");
        match &produced[0] {
            Frame::CommandWith { counter, method, body } => {
                assert_eq!(*counter, 3);
                assert_eq!(method, "OnProgress");
                assert_eq!(body, &serde_json::json!({"position": 64}));
            }
            other => panic!("expected a command with a body, got {other:?}"),
        }
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn junk_before_a_frame_resynchronises_at_the_header_instead_of_dropping_the_stream() {
        crate::pin_test_dir();
        // `index += 1; continue` in the reference. A decoder that reset instead would lose every
        // later frame on that connection.
        let mut stream = vec![0xFF, 0x00, 0x13, 0x99];
        stream.extend_from_slice(&response(1, None));
        stream.extend_from_slice(&response(2, None));
        let mut decoder = Decoder::new();
        let frames = decoder.feed(&stream);
        assert_eq!(
            frames,
            vec![
                Frame::ResponseEmpty { counter: 1 },
                Frame::ResponseEmpty { counter: 2 }
            ]
        );
    }

    #[test]
    fn a_ping_is_not_a_frame_this_decoder_reports() {
        crate::pin_test_dir();
        // `PING` (0xE4) is not in the reference's `[SEND_CMD, RET_CMD]` header set, so it is
        // skipped byte by byte like any other non-header. Recording that here means the encoder
        // and decoder visibly disagree about one constant *by design*.
        let mut decoder = Decoder::new();
        assert!(decoder.feed(&ping(1)).is_empty());
        // …and a real frame behind it still decodes.
        let mut stream = ping(1);
        stream.extend_from_slice(&response(9, None));
        assert_eq!(
            decoder.feed(&stream),
            vec![Frame::ResponseEmpty { counter: 9 }]
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_consumed_without_reporting_garbage() {
        crate::pin_test_dir();
        // The reference's `try/except/finally`: no callback, but the index still advances.
        // Returning `Bad` would resynchronise inside the body.
        let framed = response(4, Some(b"not json"));
        let mut decoder = Decoder::new();
        let frames = decoder.feed(&framed);
        assert!(
            frames.is_empty(),
            "a non-JSON body must not be reported at all: {frames:?}"
        );
        assert!(
            decoder.pending().is_empty(),
            "but the whole frame must still be consumed, or the loop runs on the same bytes \
             forever"
        );
    }

    #[test]
    fn two_frames_in_one_read_are_both_decoded_in_order() {
        crate::pin_test_dir();
        let mut stream = command(1, "GetVolume", None);
        stream.extend_from_slice(&response(2, Some(b"{\"volume\":50}")));
        let frames = Decoder::new().feed(&stream);
        assert_eq!(frames.len(), 2);
        assert!(matches!(&frames[0], Frame::Command { counter: 1, method } if method == "GetVolume"));
        assert!(matches!(&frames[1], Frame::Response { counter: 2, .. }));
    }

    #[test]
    fn a_bad_flag_byte_is_skipped_rather_than_asserted_into_a_crash() {
        crate::pin_test_dir();
        // The reference `assert`s flags are in [2,3]; a failed assert raises and its `except`
        // advances one byte. Here that is the same path as any other bad byte.
        let mut stream = vec![SEND_CMD, 9, 0, 0, 0, 1];
        stream.extend_from_slice(&response(7, None));
        let frames = Decoder::new().feed(&stream);
        assert_eq!(frames, vec![Frame::ResponseEmpty { counter: 7 }]);
    }

    #[test]
    fn a_wrong_command_field_is_refused_not_decoded_as_a_command() {
        crate::pin_test_dir();
        // `assert cmd_str == COMMAND`: a peer using a different field name is malformed, and
        // accepting it would mean inventing a verb from misaligned bytes.
        let mut framed = command(1, "pause", None);
        framed[8] = b'X';
        let mut decoder = Decoder::new();
        assert!(decoder.feed(&framed).is_empty());
    }

    /// Every event the reference's decoder fired, replayed against this port.
    ///
    /// Eight streams produced by executing the reference's `parse_request` with recording
    /// callbacks: whole frames, byte-at-a-time arrival, leading junk, a `PING` in the middle, a
    /// non-JSON body, a bad flag byte, and a frame split across two reads.
    ///
    /// The comparison is on **events**, not bytes: the reference reports through
    /// `cmd_from_client` / `res_from_client`, so a port that decoded the same bytes into
    /// different callbacks would still be wrong. `bad_json` is the row that pins the
    /// `try/except/finally` shape — no callback at all.
    #[test]
    fn every_reference_decode_sequence_replays_identically() {
        crate::pin_test_dir();
        let corpus = include_str!("../tests/golden/nva_decode.jsonl");
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let case = row["case"].as_str().expect("case");
            let mut decoder = Decoder::new();
            let mut events: Vec<serde_json::Value> = Vec::new();
            for chunk in row["chunks"].as_array().expect("chunks") {
                let bytes = hex_to_bytes(chunk.as_str().expect("hex"));
                for frame in decoder.feed(&bytes) {
                    events.push(match frame {
                        Frame::ResponseEmpty { counter } => {
                            serde_json::json!(["res", counter, null])
                        }
                        Frame::Response { counter, body } => {
                            serde_json::json!(["res", counter, body])
                        }
                        Frame::Command { counter, method } => {
                            serde_json::json!(["cmd", method, counter, null])
                        }
                        Frame::CommandWith { counter, method, body } => {
                            serde_json::json!(["cmd", method, counter, body])
                        }
                    });
                }
            }
            let expected: Vec<serde_json::Value> =
                row["events"].as_array().expect("events").clone();
            assert_eq!(events, expected, "{case} decoded differently");
            rows += 1;
        }
        assert_eq!(rows, 8, "the corpus lost rows");
    }

    /// Decode a hex string, for corpora that carry bytes as text.
    fn hex_to_bytes(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex pair"))
            .collect()
    }

    #[test]
    fn a_huge_declared_length_waits_instead_of_allocating() {
        crate::pin_test_dir();
        // A peer can claim 4 GiB. Waiting for the rest is the only safe answer: allocating it
        // first is a one-packet memory exhaustion.
        let mut framed = response(1, Some(b"{}"));
        framed[6..10].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        let mut decoder = Decoder::new();
        assert!(decoder.feed(&framed).is_empty());
        assert!(!decoder.pending().is_empty(), "it must be held, not dropped");
        assert!(
            decoder.pending().len() < 1024,
            "nothing was allocated for the claimed length"
        );
    }
}
