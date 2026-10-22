//! NVA binary frames: the four shapes the sender and receiver exchange.
//!
//! Rust counterpart of `macast/plugins/protocol/nirvana.py`'s `send_res` / `send_ping` /
//! `send_cmd` (`:860-893`). NVA uses HTTP for the handshake and then **the same socket for a
//! length-prefixed binary channel** — which is why the request-line parser hands the socket
//! over instead of letting `cheroot` reuse it (`crate::nva`).
//!
//! ## The format, read off the `struct.pack` formats
//!
//! All integers are big-endian (`>`). The first byte is the frame kind:
//!
//! | kind | bytes | layout |
//! |---|---|---|
//! | `RET_CMD` `0xC0`, empty | 0 | `c B I` = kind, `0`, counter |
//! | `RET_CMD`, JSON | 1 | `c B 2I <n>s` = kind, `1`, counter, length, body |
//! | `PING` `0xE4` | 0 | `c B I` = kind, `0`, counter |
//! | `SEND_CMD`, no data | 2 | `c B I 2B 7s B <n>s` = …`1,7,"Command",len,cmd` |
//! | `SEND_CMD`, with data | 3 | the above, then `I <n>s` = length, JSON body |
//!
//! Two details that are easy to get wrong and impossible to notice afterwards:
//!
//! * **the JSON is compact** — `separators=(',', ':')`, so no spaces after `,` or `:`. A
//!   pretty-printed body is a different byte string and the length field is what the peer reads
//!   with, so the frame stays self-consistent while the *content* differs from what the
//!   reference sends. Nothing rejects it; it just never matches a captured exchange.
//! * **the counter is bumped before encoding**, under a lock, and it is the *frame* counter — not
//!   a request id chosen by the caller. Two frames must never share one, which is why the
//!   counter lives with the connection and not with this function.
//!
//! ## What this module deliberately does not do
//!
//! It does not increment the counter, hold a socket, or log. Those are the caller's, and the
//! counter especially: a function that bumped its own private counter would produce frames that
//! look right on their own and collide on the wire.

/// Frame kinds.
pub const RET_CMD: u8 = 0xC0;
pub const SEND_CMD: u8 = 0xE0;
pub const PING: u8 = 0xE4;

/// The field name that prefixes a command payload. Seven bytes, and the length in the frame is
/// its length, so a rename here changes every frame on the wire.
pub const COMMAND_FIELD: &[u8] = b"Command";

/// One encoded frame.
pub fn response(counter: u32, body: Option<&[u8]>) -> Vec<u8> {
    match body {
        None => {
            let mut out = Vec::with_capacity(6);
            out.push(RET_CMD);
            out.push(0);
            out.extend_from_slice(&counter.to_be_bytes());
            out
        }
        Some(payload) => {
            let mut out = Vec::with_capacity(10 + payload.len());
            out.push(RET_CMD);
            out.push(1);
            out.extend_from_slice(&counter.to_be_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
            out
        }
    }
}

/// A keepalive frame. Nothing to read back: the peer answers with a `PING` of its own.
pub fn ping(counter: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.push(PING);
    out.push(0);
    out.extend_from_slice(&counter.to_be_bytes());
    out
}

/// A command frame.
///
/// `command` is the verb (`"pause"`, `"seek"`, …) and `body` is its already-compact JSON. The
/// caller is expected to have produced `body` with [`compact_json`] so the bytes match the
/// reference.
pub fn command(counter: u32, command: &str, body: Option<&[u8]>) -> Vec<u8> {
    let verb = command.as_bytes();
    let mut out = Vec::with_capacity(20 + verb.len() + body.map_or(0, |b| b.len()));
    match body {
        None => {
            out.push(SEND_CMD);
            out.push(2);
            out.extend_from_slice(&counter.to_be_bytes());
            out.push(1);
            out.push(7);
            out.extend_from_slice(COMMAND_FIELD);
            out.push(verb.len() as u8);
            out.extend_from_slice(verb);
        }
        Some(payload) => {
            out.push(SEND_CMD);
            out.push(3);
            out.extend_from_slice(&counter.to_be_bytes());
            out.push(1);
            out.push(7);
            out.extend_from_slice(COMMAND_FIELD);
            out.push(verb.len() as u8);
            out.extend_from_slice(verb);
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
        }
    }
    out
}

/// Serialise JSON the way the reference does: **no spaces after `,` or `:`**.
///
/// `serde_json::to_string` already matches `separators=(',', ':')`, so this is a named seam
/// rather than a second serialiser — but it exists because "compact JSON" is a property of the
/// wire format here, and a future switch to a pretty printer would keep every length field
/// self-consistent while silently changing what a capture shows.
pub fn compact_json(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec())
}

/// The kind byte of a frame, for a reader that only wants to dispatch.
pub fn kind(frame: &[u8]) -> Option<u8> {
    frame.first().copied()
}

/// The counter of a fixed-shape frame (`RET_CMD` empty, `PING`).
///
/// Returns `None` for the variable-shape frames, where the counter sits at the same offset but
/// reading it without checking the flag byte would be guessing which shape arrived.
pub fn fixed_counter(frame: &[u8]) -> Option<u32> {
    if frame.len() != 6 || frame[1] != 0 {
        return None;
    }
    Some(u32::from_be_bytes([frame[2], frame[3], frame[4], frame[5]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_response_is_six_bytes_and_a_with_body_one_carries_its_length() {
        crate::pin_test_dir();
        assert_eq!(response(7, None), vec![RET_CMD, 0, 0, 0, 0, 7]);
        let framed = response(7, Some(b"{\"a\":1}"));
        assert_eq!(framed[0], RET_CMD);
        assert_eq!(framed[1], 1, "flag byte says a body follows");
        assert_eq!(&framed[2..6], &[0, 0, 0, 7]);
        assert_eq!(&framed[6..10], &[0, 0, 0, 7], "length of {{\"a\":1}} is 7");
        assert_eq!(&framed[10..], b"{\"a\":1}");
    }

    #[test]
    fn a_ping_is_the_same_six_bytes_with_a_different_kind() {
        crate::pin_test_dir();
        assert_eq!(ping(1), vec![PING, 0, 0, 0, 0, 1]);
        // The kind byte is the only difference from an empty response — which is exactly why a
        // reader must dispatch on it rather than on the length.
        let mut as_response = ping(1);
        as_response[0] = RET_CMD;
        assert_eq!(as_response, response(1, None));
    }

    #[test]
    fn a_command_without_data_carries_its_verb_and_the_seven_byte_field_name() {
        crate::pin_test_dir();
        let framed = command(3, "pause", None);
        assert_eq!(framed[0], SEND_CMD);
        assert_eq!(framed[1], 2);
        assert_eq!(&framed[2..6], &[0, 0, 0, 3]);
        assert_eq!(framed[6], 1);
        assert_eq!(framed[7], 7, "length of \"Command\"");
        assert_eq!(&framed[8..15], b"Command");
        assert_eq!(framed[15], 5, "length of \"pause\"");
        assert_eq!(&framed[16..], b"pause");
        assert_eq!(framed.len(), 21);
    }

    #[test]
    fn a_command_with_data_appends_the_length_then_the_json() {
        crate::pin_test_dir();
        let framed = command(4, "seek", Some(b"{\"p\":2}"));
        assert_eq!(framed[1], 3, "flag byte says data follows");
        // 1 kind + 1 flag + 4 counter + 1 + 1 + 7 ("Command") + 1 + 4 ("seek") = 20.
        let tail = &framed[20..];
        assert_eq!(&tail[..4], &[0, 0, 0, 7], "length of the 7-byte body");
        assert_eq!(&tail[4..], b"{\"p\":2}");
    }

    #[test]
    fn compact_json_has_no_spaces_so_the_bytes_match_a_capture() {
        crate::pin_test_dir();
        // A pretty printer would keep every length field self-consistent and still never match
        // the reference's frames — the kind of difference nothing rejects.
        let value: serde_json::Value =
            serde_json::from_str(r#"{"a": 1, "b": [1, 2]}"#).expect("valid json");
        let bytes = compact_json(&value);
        let text = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(text, r#"{"a":1,"b":[1,2]}"#);
        assert!(!text.contains(", "));
        assert!(!text.contains(": "));
    }

    #[test]
    fn the_counter_is_read_back_only_for_the_fixed_shapes() {
        crate::pin_test_dir();
        assert_eq!(fixed_counter(&ping(9)), Some(9));
        assert_eq!(fixed_counter(&response(9, None)), Some(9));
        // A body-carrying frame has the counter at the same offset, but reading it without the
        // flag byte would be guessing which shape arrived.
        assert_eq!(fixed_counter(&response(9, Some(b"{}"))), None);
        assert_eq!(fixed_counter(&command(9, "pause", None)), None);
        assert_eq!(fixed_counter(&[]), None);
        assert_eq!(kind(&ping(1)), Some(PING));
        assert_eq!(kind(&[]), None);
    }

    #[test]
    fn a_long_body_still_encodes_its_real_length() {
        crate::pin_test_dir();
        // The length field is 4 bytes, so a body above 64 KiB is representable; a u16 or a
        // length taken from a truncated string is the kind of bug that only shows on a large
        // danmaku payload.
        let body = vec![b'x'; 70_000];
        let framed = response(1, Some(&body));
        let length = u32::from_be_bytes([framed[6], framed[7], framed[8], framed[9]]);
        assert_eq!(length as usize, body.len());
        assert_eq!(framed.len(), 10 + body.len());
    }

    /// Every frame `struct.pack` produced, replayed byte for byte.
    ///
    /// 126 frames from executing the reference's three `struct.pack` formats over seven counter
    /// values (including `0xFFFFFFFF >> 1` and both byte boundaries), four command verbs and
    /// four bodies (including non-ASCII). This is the tightest check in this module: a wrong
    /// offset or a decimal instead of big-endian bytes produces a frame that still *looks*
    /// plausible and is decoded as garbage by a real client.
    #[test]
    fn every_reference_frame_replays_byte_for_byte() {
        crate::pin_test_dir();
        let corpus = include_str!("../tests/golden/nva_frames.jsonl");
        let mut rows = 0usize;
        let mut kinds_seen = std::collections::BTreeSet::new();
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let counter = row["counter"].as_u64().expect("counter") as u32;
            let expected = row["hex"].as_str().expect("hex");
            let kind = row["kind"].as_str().expect("kind");
            kinds_seen.insert(kind.to_string());
            let produced = match kind {
                "res_empty" => response(counter, None),
                "ping" => ping(counter),
                "res_body" => {
                    let body = row["body"].as_str().expect("body").as_bytes();
                    response(counter, Some(body))
                }
                "cmd" => command(counter, row["command"].as_str().expect("command"), None),
                "cmd_body" => {
                    let body = row["body"].as_str().expect("body").as_bytes();
                    command(counter, row["command"].as_str().expect("command"), Some(body))
                }
                other => panic!("unknown frame kind {other}"),
            };
            let produced_hex: String = produced.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(produced_hex, expected, "{kind} at counter {counter} differs");
            rows += 1;
        }
        assert_eq!(rows, 126, "the corpus lost rows");
        // All five frame shapes must actually appear, or one of them is untested by the corpus.
        assert_eq!(kinds_seen.len(), 5, "the corpus lost a frame shape: {kinds_seen:?}");
    }

    #[test]
    fn the_kind_bytes_are_the_three_the_protocol_names() {
        crate::pin_test_dir();
        // These three constants appear in the reference as `\xc0`, `\xe0`, `\xe4`; a typo in one
        // of them produces frames a real client silently ignores.
        assert_eq!(RET_CMD, 0xC0);
        assert_eq!(SEND_CMD, 0xE0);
        assert_eq!(PING, 0xE4);
        assert_eq!(COMMAND_FIELD, b"Command");
    }
}
