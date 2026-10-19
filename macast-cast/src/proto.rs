//! The `CastMessage` envelope, read the way the senders read it.
//!
//! Every byte array asserted in [`tests`] was measured out of
//! `macast/protocol_cast.py`'s own codec on this machine, not derived from
//! `cast_channel.proto`'s prose. That distinction is the point of this module:
//! the three senders that matter to us (pychromecast, VLC, the Cast SDK) each
//! run a different protobuf reader, and none of them reports *why* it hung up.
//! A wrong byte here is a silent refusal, so we pin to the reference
//! implementation's output byte-for-byte and let the tests do the arguing.
//!
//! Two deliberate asymmetries are documented where they live rather than here:
//! the varint reader keeps Python's low 64 bits when a counter runs past
//! `u64::MAX` ([`tests::a_varint_that_never_ends_keeps_the_low_sixty_four_bits`]),
//! and the namespace documents are compared by **shape** instead of by
//! whitespace ([`tests::namespaces_go_out_as_objects_not_strings`]).

use serde_json::{json, Value};

/// `NS_CONNECTION` in Python: the channel every sender opens first, and the one
/// whose `CONNECT`/`CLOSE` pair our sender ledger keys off.
pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
/// `NS_HEARTBEAT`: `PING` must be answered with `PONG` on the same socket.
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
/// `NS_RECEIVER`: `GET_STATUS`, `LAUNCH`, `STOP`, `SET_VOLUME`, and every
/// `RECEIVER_STATUS` we emit.
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
/// `NS_MEDIA`: `LOAD` and every `MEDIA_STATUS` we emit.
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
/// `NS_DEVICEAUTH`: the challenge VLC answers before it sends anything else.
pub const NS_DEVICEAUTH: &str = "urn:x-cast:com.google.cast.tp.deviceauth";
/// The fourth namespace a media receiver app must expose.
///
/// Python spells this one **inline** in `APP_NAMESPACES` rather than as a
/// `NS_*` constant, so nothing in the reference file names it. We do, because a
/// crate that hands out a bare string at the fourth position has to say which
/// string it means, and because `channel` will dispatch on it by name. Same
/// value, one owner.
pub const NS_PLAYER_MESSAGE: &str = "urn:x-cast:com.google.cast.player.message";

/// The namespaces this receiver advertises, in the order that goes on the wire.
///
/// Order is not decoration: `_app_namespaces()` builds a list, Python keeps
/// insertion order, and the senders read that list as the app's capability
/// document. `NS_RECEIVER` is deliberately absent — it is the *receiver's*
/// namespace, not the media app's, and Python never lists it either.
pub const APP_NAMESPACES: [&str; 4] =
    [NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_PLAYER_MESSAGE];

/// The length prefix every `CastMessage` travels behind, in bytes.
///
/// Python's `_struct_pack` is `struct.Struct(">I").pack` — big-endian,
/// unsigned, four bytes. Read the header, then that many bytes, then parse.
pub const FRAME_HEADER_LEN: usize = 4;

/// Which payload slot a [`CastMessage`] carries, mirroring protobuf's
/// `enum PayloadType { STRING = 0; BINARY = 1; }`.
///
/// Python passes a `binary: bool` down `encode_cast_message`. A bool two
/// positions away from the value it selects is how the two payload fields get
/// swapped without anything looking wrong at the call site, so the choice is
/// named here and the field number is derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Field 6, `payload_utf8`, with `payload_type = 0`.
    Text(String),
    /// Field 7, `payload_binary`, with `payload_type = 1`.
    Binary(Vec<u8>),
}

impl From<&str> for Payload {
    fn from(s: &str) -> Self {
        Payload::Text(s.to_owned())
    }
}

impl From<String> for Payload {
    fn from(s: String) -> Self {
        Payload::Text(s)
    }
}

impl From<&[u8]> for Payload {
    fn from(b: &[u8]) -> Self {
        Payload::Binary(b.to_vec())
    }
}

impl From<Vec<u8>> for Payload {
    fn from(b: Vec<u8>) -> Self {
        Payload::Binary(b)
    }
}

/// `protobuf` varint: 7 bits per byte, high bit means "more".
///
/// Python's loop is `while True` with `n >>= 7` until the remainder is zero,
/// which is why `0` still emits one byte.
fn encode_varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// One varint field: tag `(field_num << 3) | 0`, then the value.
///
/// The assert is not defensive padding. Python computes
/// `bytes([(field_num << 3) | 0])`, and `bytes()` refuses any value outside
/// `range(0, 256)` — `field_num = 32` raises `ValueError` there, measured. In
/// Rust `(32 << 3) as u8` would silently produce `0` and write a tag for field
/// 0, so the refusal has to be explicit to stay the same contract.
pub fn field_varint(field_num: u32, value: u64) -> Vec<u8> {
    assert!(field_num <= 31, "field {field_num} does not fit one protobuf tag byte");
    let mut out = vec![(field_num << 3) as u8];
    out.extend(encode_varint(value));
    out
}

/// One length-delimited field: tag `(field_num << 3) | 2`, then the length as a
/// varint, then the bytes themselves.
///
/// Same tag-byte refusal as [`field_varint`]: `field_num = 32` is a `ValueError`
/// in the reference, not a `0` tag.
pub fn field_bytes(field_num: u32, data: &[u8]) -> Vec<u8> {
    assert!(field_num <= 31, "field {field_num} does not fit one protobuf tag byte");
    let mut out = vec![((field_num << 3) | 2) as u8];
    out.extend(encode_varint(data.len() as u64));
    out.extend_from_slice(data);
    out
}

/// A `CastMessage` without its length prefix — the body of one Cast frame.
///
/// The five fields Python writes, in the order `_field_*` appends them:
/// `protocol_version`, `source_id`, `destination_id`, `namespace`,
/// `payload_type`, then exactly one payload slot.
pub fn encode_cast_message(
    source_id: &str,
    destination_id: &str,
    namespace: &str,
    payload: impl Into<Payload>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    // protocol_version = CASTV2_1_0. Required by the schema, and always 0, so
    // it is written rather than taken as a parameter: a caller cannot be asked
    // to supply a value that has one legal answer.
    buf.extend(field_varint(1, 0));
    buf.extend(field_bytes(2, source_id.as_bytes()));
    buf.extend(field_bytes(3, destination_id.as_bytes()));
    buf.extend(field_bytes(4, namespace.as_bytes()));
    match payload.into() {
        Payload::Text(text) => {
            buf.extend(field_varint(5, 0));
            buf.extend(field_bytes(6, text.as_bytes()));
        }
        Payload::Binary(bytes) => {
            buf.extend(field_varint(5, 1));
            buf.extend(field_bytes(7, &bytes));
        }
    }
    buf
}

/// `DeviceAuthMessage` carrying an empty `AuthChallenge` — field 1, zero bytes.
///
/// This is what we send first on `NS_DEVICEAUTH`, and what a sender's challenge
/// looks like back at us. It is *shorter than nothing looks wrong*, which is
/// exactly why it has its own function instead of a call site repeating
/// `field_bytes(1, b"")`.
pub fn encode_auth_challenge() -> Vec<u8> {
    field_bytes(1, b"")
}

/// `DeviceAuthMessage{ response: AuthResponse{ signature, client_auth_certificate } }`.
///
/// Takes **no parameters**. The reference defaults both inner fields to empty
/// bytes and our receiver is uncertified, so there is nothing a caller could
/// hand us that we could also test; a parameter nobody can supply is a promise.
///
/// The shape is the whole lesson (AGENTS.md §4.1): `signature` and
/// `client_auth_certificate` are `required`, so a bare `AuthResponse` — or an
/// empty field 2 — leaves them unset, VLC's `ParseFromString()` fails, it
/// `return`s, and its state machine never leaves `Authenticating`. The device
/// stays advertised and uncastable.
pub fn encode_auth_response() -> Vec<u8> {
    let auth_response = field_bytes(1, b"")
        .into_iter()
        .chain(field_bytes(2, b""))
        .collect::<Vec<u8>>();
    field_bytes(2, &auth_response)
}

/// One decoded protobuf field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub number: u32,
    /// 0 = varint, 2 = length-delimited. Anything else ends the read.
    pub wire_type: u8,
    pub value: FieldValue,
}

/// The two payload shapes `_parse_fields` can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Varint(u64),
    Bytes(Vec<u8>),
}

/// Read the fields of one message body.
///
/// Two behaviors are copied from Python exactly because a sender's framing is
/// not always polite:
///
/// * An **unsupported wire type ends the read** and yields what was decoded so
///   far — no error. 3 (64-bit) and 5 (32-bit) are fixed-size fields Cast v2
///   never uses, and the reference stops rather than guessing where the next
///   tag starts.
/// * A **truncated** varint or length prefix yields whatever was accumulated and
///   a clamped slice, again without an error. `buf[i : i + length]` in Python
///   silently returns the short tail; `&buf[start..end]` in Rust would panic, so
///   the clamp is what keeps us reading a sender's last half-frame the way the
///   reference does.
pub fn parse_fields(buf: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    let n = buf.len();
    let mut i = 0usize;
    while i < n {
        let tag = buf[i];
        i += 1;
        let number = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        if wire_type == 0 {
            let (value, next) = read_varint(buf, i);
            i = next;
            out.push(Field {
                number,
                wire_type,
                value: FieldValue::Varint(value),
            });
        } else if wire_type == 2 {
            let (length, start) = read_varint(buf, i);
            // `usize::try_from` only fails on a 128-bit-truncated length; clamping
            // to `usize::MAX` reproduces Python's "slice whatever is there".
            let len = usize::try_from(length).unwrap_or(usize::MAX);
            let end = start.saturating_add(len).min(n);
            let value = buf[start..end].to_vec();
            i = start.saturating_add(len);
            out.push(Field {
                number,
                wire_type,
                value: FieldValue::Bytes(value),
            });
        } else {
            return out;
        }
    }
    out
}

/// Read one varint from `buf` starting at `i`, returning the value and the next
/// index — which may be past the end, exactly like Python's `i` after a
/// truncated counter.
///
/// The `shift < 64` guard is the one intentional divergence from Python, and it
/// lands on the same number: Python accumulates into an arbitrary-precision int
/// and its bits past 64 simply never matter to a 64-bit reader, while Rust's
/// `<<` panics on a shift amount that large. Dropping those bits gives the same
/// low 64 bits — the assertion in
/// [`tests::a_varint_that_never_ends_keeps_the_low_sixty_four_bits`] is measured
/// from the reference, not argued.
fn read_varint(buf: &[u8], mut i: usize) -> (u64, usize) {
    let mut value = 0u64;
    let mut shift = 0u32;
    while i < buf.len() {
        let b = buf[i];
        i += 1;
        let part = (b & 0x7F) as u64;
        if shift < 64 {
            value |= part << shift;
        }
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }
    (value, i)
}

/// A decoded `CastMessage`.
///
/// The `Default` values are Python's dict defaults verbatim: the reference
/// `parse_cast_message` starts from `""`/`0`/`b""` and only overwrites a slot
/// when it sees that field number **with the wire type it expects**, so a
/// LEN-encoded field 5 leaves `payload_type` at `0` instead of throwing. That
/// "always answers, never raises" property is what keeps one malformed frame
/// from killing a client loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CastMessage {
    pub source_id: String,
    pub destination_id: String,
    pub namespace: String,
    pub payload_type: u64,
    pub payload_utf8: String,
    pub payload_binary: Vec<u8>,
}

/// Decode one message **body** (the bytes after the length prefix has been
/// consumed — the reference's docstring says "length-prefixed", but `:572` calls
/// it on the body it just read, and that is the contract we keep).
///
/// Decoding is lossy on purpose: Python uses `.decode("utf-8", "replace")`, so
/// invalid bytes become U+FFFD and the message still parses. See
/// [`tests::a_broken_utf8_source_id_replaces_instead_of_refusing`] for the eight
/// shapes a sender's id can arrive as.
pub fn parse_cast_message(buf: &[u8]) -> CastMessage {
    let mut msg = CastMessage::default();
    for field in parse_fields(buf) {
        match (field.number, field.wire_type) {
            (2, 2) => msg.source_id = lossy(&field),
            (3, 2) => msg.destination_id = lossy(&field),
            (4, 2) => msg.namespace = lossy(&field),
            (5, 0) => {
                if let FieldValue::Varint(v) = field.value {
                    msg.payload_type = v;
                }
            }
            (6, 2) => msg.payload_utf8 = lossy(&field),
            (7, 2) => {
                if let FieldValue::Bytes(bytes) = field.value {
                    msg.payload_binary = bytes;
                }
            }
            _ => {}
        }
    }
    msg
}

/// The bytes of a LEN field, decoded the way Python's `errors="replace"` does.
fn lossy(field: &Field) -> String {
    match &field.value {
        FieldValue::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        // A varint in a text slot is not text. Python would raise
        // AttributeError on `.decode`; we keep the default and let the sender's
        // next frame say what it meant.
        FieldValue::Varint(_) => String::new(),
    }
}

/// Prefix a body with its big-endian length: the frame as it goes on the socket.
///
/// `struct.pack(">I", len(body))` raises `struct.error` past 4 GiB. A Cast frame
/// that large means our own ledger has run away, not that a sender is creative,
/// so this panics with a sentence instead of silently wrapping to a small prefix
/// and desynchronizing every read on the socket that follows.
pub fn wrap_frame(body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(body.len())
        .expect("a Cast frame longer than 4 GiB cannot be length-prefixed");
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + body.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// The body length a four-byte header promises.
pub fn frame_length(header: &[u8; FRAME_HEADER_LEN]) -> usize {
    u32::from_be_bytes(*header) as usize
}

/// The `namespaces` array as the wire format wants it: a list of **objects**.
///
/// `message Namespace { string name = 1; }`, so each entry is `{"name": …}`.
/// Handing a sender `["urn:…"]` — bare strings — makes pychromecast, VLC and the
/// Cast SDK answer "namespace not supported by current app" and never issue the
/// `LOAD`. The device is then advertised, findable, connectable, and impossible
/// to cast to, with no error on our side at all (AGENTS.md §4.1).
///
/// Returns `Value`, not `Vec<&str>`, for exactly that reason: the tidy type
/// cannot compile into the wire format. `serde_json` here is built with
/// `preserve_order`, so the object keeps its single key in the position the
/// schema gives it.
pub fn app_namespaces() -> Vec<Value> {
    APP_NAMESPACES
        .iter()
        .map(|ns| json!({ "name": *ns }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `#[test]` in this crate must call `crate::pin_test_dir()` as its
    /// **first statement** — see the doc there for why `cfg(test)` in
    /// `macast-core` cannot protect a dependent crate's tests. The floor below
    /// is this crate's own measured `#[test]` count — read from
    /// `cargo test -p macast-cast --lib -- --list`, not from arithmetic on a
    /// diff; a test added without the pin makes the audit ask for more than it
    /// finds, and a test deleted silently makes this floor stale, which is the
    /// one thing the number is here to catch. **A new module with tests moves
    /// this number.**
    ///
    /// 695 is what both instruments say right now: `cargo test -p macast-cast
    /// --lib -- --list` prints 695, and the scan over `src/*.rs` sums to **the
    /// same 695**. The per-file rows are the **scanner's own line counts**:
    /// `job_tests` 162, `targets_tests` 75, `mdns_tests` 66, `airplay_tests` 66,
    /// `relay_tests` 64, `push_tests` 46, `channel_tests` 40, `resolve` 39,
    /// `setup_tests` 30, `announce_tests` 29, `respond_tests` 22, this file 19,
    /// `engine_tests` 17, `cert` 12, `scrape_tests` 6, `charmap_tests` 2.
    ///
    /// The two instruments agreeing is a **measured** fact, not a law: the same
    /// scan run against `macast-mpv` reads **161** while its `--list` reads
    /// **149** — twelve of that crate's attributes sit in `#[cfg(windows)]`
    /// modules that this Mac does not compile, so a scan counts source that
    /// cannot run. `macast-cast` has no such module today, which is *why* the
    /// two numbers coincide here; a round that adds a platform-gated test file
    /// should expect them to part, and `--list` is the one that counts tests.
    ///
    /// Neither instrument is a plain `grep` for the attribute, which on this
    /// crate reads **702**: seven of those lines *mention* the attribute — the scanner's own
    /// needle and this doc comment among them — instead of carrying one. Same
    /// lesson as `push_tests` carrying 610 against 603: an instrument that counts
    /// strings is not a census. The sentence this one replaces was itself the
    /// stale half of that lesson (it claimed the scanner said 671 while `--list`
    /// said 686 and called the two "in agreement"), which is why the per-file
    /// rows are re-measured rather than remembered.
    ///
    /// The scan is deliberately non-recursive, so the `#[path]`-included
    /// `*_tests.rs` files still land in it because they sit in `src/` itself.
    /// The previous floors — 603, 633, 637, 655, 656, 671, 672, 677, 678, 686,
    /// 687, 688, 690, 695, 699, 702, 723 — were carried by the
    /// same two instruments
    /// until `setup.rs` landed with its own module, then until the Cast
    /// connection itself was reached by a real socket (2026-10-18, +4 in
    /// `channel_tests`: the starvation witness plus the frame cap, the split
    /// frame and the peer that closes), then until `announce.rs` landed with the
    /// two frozen corpora, then until §22 of the acceptance script caught that
    /// module reading the wrong interface list (+1: the owner audit), then
    /// until the responder was wired to a socket that both joins and reads
    /// (+10 in `announce_tests`: the eight `e1`-`e8` answer-shape witnesses,
    /// `e9` — the one that asks whether anything ever *arrived* — and `e10`,
    /// which feeds in the 40-byte query a foreign python stack sent across the
    /// LAN and asserts our own writer produces the same 40 bytes), then 690
    /// → 695 on 2026-10-18 when the AirPlay RTSP face was wired into the shell:
    /// five rows in `airplay_tests` (the accept loop the shell actually runs and
    /// the port it lets go of, a connection that arrives after the first poll,
    /// the name the page would show, and the two `SET_PARAMETER` sentences — the
    /// handover said with both numbers, and the refusal said when the number
    /// cannot be read), and 695 → 699 the same day when `_airplay._tcp` got its
    /// announcement: the byte-for-byte row against the third frozen corpus, the row
    /// that says a service of that type is announced and **never answered** — the
    /// second is a decision with a witness, the first is a corpus with a reader —
    /// plus two that exist because the shared module started telling lies about a
    /// second protocol: the refusal sentence naming the service type it actually
    /// refused (it used to say "Chromecast" for both), and the audit that `start()`
    /// **derives** the answering half from the predicate instead of hardcoding it —
    /// whose behavioural witness is §25, which is a skip on this machine, so the
    /// structural question is the one being asked here.
    /// Two more rows in
    /// this crate are not about the responder at all: `airplay_tests`'s
    /// refusal-lines row and `relay_tests`'s sweep-names-its-files row, both of
    /// which exist because those two modules wrote through the `log` facade while
    /// nothing in the workspace had installed a sink for it — see
    /// `macast-core`'s `nothing_in_the_workspace_borrows_the_log_facade`.
    /// `601 passed; 1 failed` in an older run is a *602-test* run with one
    /// failure, not a third floor — the sum of passed and failed is what has to
    /// reach this number.
    ///
    /// And a name filter is not a census: `--lib setup` reports **32** tests,
    /// because two older `airplay::tests` rows have "setup" in their names while
    /// `setup_tests.rs` itself holds 30. The floor is the crate number, and the
    /// only readings that may be compared with it are `--list` and the scan.
    ///
    /// **The stale half of this cell belongs to the record**: the value two floors
    /// ago was 65 — "35 channel + 18 proto + 12 cert" — and it was never brought
    /// back here while seven later modules were added. That is not a harmless under-floor: `scanned >= floor` with a floor that far below the truth (65 against the
    /// 637 measured above) means the audit cannot see a deleted module at all, and
    /// the one thing the
    /// number exists to catch stops being caught. So a round that adds tests must
    /// move this constant in the same pass, and a round that removes a whole file
    /// of tests must move it down — either way, from the measurement, never from a
    /// diff.
    /// 702 → **723** on 2026-10-20, when the aggregation queue landed: nineteen
    /// rows in the new `aggregate_tests` (the four measured answers as rules, the
    /// merge boundary, `dict.update`'s overwrite-not-union, the deferral ceiling,
    /// `_remove_answers_from_queue`, the emptied group that still defers, two queues
    /// as two batches, the deadline being the sooner of the two, the production draw
    /// read *through* a queue rather than by copying its expression, and six rows for
    /// the ledger (19 rows) — the round trip from our own bytes to our own key, the NSEC this
    /// parser cannot hear, a goodbye that forgets instead of protecting, the prune,
    /// the QR dispatch, and the hearing that moves an answer from 520 ms to 1520 ms),
    /// plus two in `respond_tests` that the queue made visible: the suppression of an
    /// additional that is already an answer in the same batch — a rule the frozen
    /// corpus structurally cannot see, because it compares **sets** and every one of
    /// its rows asks one question — and the identity between `Reply::records` and
    /// `flatten(Reply::groups)` that keeps the two fields from becoming two answers
    /// about the same query.
    /// 723 → **727** in the same pass, from the four rows that only appeared once
    /// the battery was written: one planned `A` answer becoming one ledger entry **per
    /// advertisable address** (a single-interface laptop cannot tell that list from a
    /// truncation, so the fixture has two), the heard packet protecting every address it
    /// named, a **response that also carries a question** — the one packet shape that
    /// separates the QR bit from `questions.is_empty()`, which is what §31's dispatch
    /// reads — and `run` being asked for a read *shorter* than the poll ceiling while an
    /// answer is queued, which is the only row that can see a queue that is drained on
    /// time only by accident.
    /// 728 → **730** on 2026-10-20 with the QU branch (three new routes above, and one
    /// test replaced rather than added), and **731** at the end of the same day with
    /// `e11` — the fan-out witness, added because 网卡 pinning had become a document.
    /// The number went *down* to 725 first, and the only
    /// reason that was noticed is this floor: an index-based splice of the test file had
    /// quietly eaten five rows on the way to rewriting a helper. `>=` cannot see a drift
    /// upward, which is the complaint recorded above; what it does see, and saw here, is a
    /// **deletion** — which is the failure this floor exists for, and the reason it is a
    /// floor of the measured number rather than a round one.
    /// 731 → **732** on 2026-10-20 with `e12`: the rotation's second socket kind, added
    /// because `scripts/mdns_unicast_bind_probe.py` measured that a wildcard bind receives
    /// **no** unicast query addressed at this host, so a sender that asked us directly was
    /// talking to a port this process could not hear. **733** from
    /// `the_reading_thread_rotates_every_socket_the_engine_bound` — binding the second kind
    /// is half the claim, and `start()` handing it to the read loop is the other half, which
    /// no behavioural row in this crate can reach without announcing on a real card. **734**
    /// from `e13`, which is the only row that can see the engine's summary echoing the
    /// addresses it was *handed* instead of the sockets it *got*.
    const TESTS_IN_THIS_CRATE: usize = 734;

    /// The frames below are `list(...)` output from the reference codec running
    /// in this repository's `.venv`, measured rather than transcribed from the
    /// spec. `receiver-0` / `sender-1` / `…tp.connection` / `{"type": "CONNECT"}`
    /// is the first real message exchange on a Cast socket, in Python's own
    /// bytes: 89 of them.
    const TEXT_FRAME: [u8; 89] = [
        8, 0, 18, 10, 114, 101, 99, 101, 105, 118, 101, 114, 45, 48, 26, 8, 115, 101, 110, 100,
        101, 114, 45, 49, 34, 40, 117, 114, 110, 58, 120, 45, 99, 97, 115, 116, 58, 99, 111, 109,
        46, 103, 111, 111, 103, 108, 101, 46, 99, 97, 115, 116, 46, 116, 112, 46, 99, 111, 110,
        110, 101, 99, 116, 105, 111, 110, 40, 0, 50, 19, 123, 34, 116, 121, 112, 101, 34, 58, 32,
        34, 67, 79, 78, 78, 69, 67, 84, 34, 125,
    ];

    /// The same call with `binary=True` and the device-auth payload: 76 bytes,
    /// and the first place the two encodings part is index 56 — the payload
    /// field number, 6 versus 7.
    const BINARY_FRAME: [u8; 76] = [
        8, 0, 18, 10, 114, 101, 99, 101, 105, 118, 101, 114, 45, 48, 26, 8, 115, 101, 110, 100,
        101, 114, 45, 49, 34, 40, 117, 114, 110, 58, 120, 45, 99, 97, 115, 116, 58, 99, 111, 109,
        46, 103, 111, 111, 103, 108, 101, 46, 99, 97, 115, 116, 46, 116, 112, 46, 100, 101, 118,
        105, 99, 101, 97, 117, 116, 104, 40, 1, 58, 6, 18, 4, 10, 0, 18, 0,
    ];

    #[test]
    fn a_text_frame_matches_python_byte_for_byte() {
        crate::pin_test_dir();
        let body = encode_cast_message(
            "receiver-0",
            "sender-1",
            NS_CONNECTION,
            r#"{"type": "CONNECT"}"#,
        );
        assert_eq!(body, TEXT_FRAME.to_vec(), "the TEXT envelope drifted");
        assert_eq!(body.len(), 89, "measured against the reference codec");
    }

    #[test]
    fn a_binary_frame_matches_python_byte_for_byte() {
        crate::pin_test_dir();
        let body = encode_cast_message(
            "receiver-0",
            "sender-1",
            NS_DEVICEAUTH,
            encode_auth_response().as_slice(),
        );
        assert_eq!(body, BINARY_FRAME.to_vec(), "the BINARY envelope drifted");
        assert_eq!(body.len(), 76, "measured against the reference codec");
    }

    /// The two fixtures differ only in the namespace and in the payload slot, so
    /// the index where they first disagree is a fact about the codec: 56 is the
    /// `payload_binary` tag. Pinning the shared prefix and the divergent tail
    /// separately means a swap of fields 6/7 cannot pass both frame tests by
    /// coincidence.
    #[test]
    fn the_two_encodings_part_at_the_payload_field_number() {
        crate::pin_test_dir();
        let shared = 56usize;
        assert_eq!(
            TEXT_FRAME[..shared],
            BINARY_FRAME[..shared],
            "the frames share everything before the payload"
        );
        assert_eq!(
            BINARY_FRAME[shared..],
            [100, 101, 118, 105, 99, 101, 97, 117, 116, 104, 40, 1, 58, 6, 18, 4, 10, 0, 18, 0],
            "the BINARY tail, measured"
        );
        // TEXT carries its payload in field 6 (tag 50), BINARY in field 7 (58).
        // Both tags sit at the *same* index, because both namespaces are 40 bytes
        // long: the namespace body runs 26..=65, field 5 is the tag+value pair at
        // 66 and 67, so the payload field's tag is byte 68 -- which is 56+12.
        assert_eq!(TEXT_FRAME[56 + 12], 50);
        assert_eq!(BINARY_FRAME[56 + 12], 58);
    }

    /// The whole §4.1 lesson in six bytes: the outer wrapper is field 2 (40 =
    /// tag for `DeviceAuthMessage.response`) around an inner message that carries
    /// **both** required fields, each present even when empty.
    #[test]
    fn the_auth_response_keeps_both_required_inner_fields() {
        crate::pin_test_dir();
        let body = encode_auth_response();
        assert_eq!(body, vec![18, 4, 10, 0, 18, 0], "measured");
        // field 1 = signature, field 2 = client_auth_certificate, both on the wire.
        let inner = &body[2..];
        assert_eq!(&inner[..2], &[10, 0], "signature is present and empty");
        assert_eq!(&inner[2..], &[18, 0], "certificate is present and empty");
        // And the wrapper is a DeviceAuthMessage, not a bare AuthResponse:
        // re-parsing the outer layer must find field 2, not field 1.
        let fields = parse_fields(&body);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 2);
    }

    #[test]
    fn the_auth_challenge_is_an_empty_field_one() {
        crate::pin_test_dir();
        assert_eq!(encode_auth_challenge(), vec![10, 0], "measured");
    }

    /// A length that itself needs a varint: 300 bytes in field 7 begin
    /// `[58, 172, 2]` and total 303, measured. This is the row that catches a
    /// codec writing `len as u8` — a 300-byte `LOAD` payload would then declare
    /// a 44-byte body and the sender would read garbage for the next frame.
    #[test]
    fn a_long_field_writes_its_length_as_a_varint() {
        crate::pin_test_dir();
        let body = field_bytes(7, &[0u8; 300]);
        assert_eq!(&body[..3], &[58, 172, 2], "measured head");
        assert_eq!(body.len(), 303, "measured length");
        assert!(body[3..].iter().all(|&b| b == 0));
    }

    /// Varint boundary rows, each measured from Python's `_field_varint(1, v)`.
    /// `0 → [8, 0]`, `127 → [8, 127]`, `128 → [8, 128, 1]`, `300 → [8, 172, 2]`,
    /// `u64::MAX →` nine `0xFF` then `0x01` (11 bytes), `i64::MAX →` eight
    /// `0xFF` then `0x7F` (10 bytes).
    #[test]
    fn varint_boundaries_are_the_measured_bytes() {
        crate::pin_test_dir();
        assert_eq!(field_varint(1, 0), vec![8, 0]);
        assert_eq!(field_varint(1, 127), vec![8, 127]);
        assert_eq!(field_varint(1, 128), vec![8, 128, 1]);
        assert_eq!(field_varint(1, 300), vec![8, 172, 2]);
        let max = field_varint(1, u64::MAX);
        assert_eq!(max.len(), 11, "measured");
        assert_eq!(&max[1..9], &[0xFF; 8]);
        assert_eq!(max[9], 0xFF);
        assert_eq!(max[10], 0x01);
        let signed_max = field_varint(1, i64::MAX as u64);
        assert_eq!(signed_max.len(), 10, "measured");
        assert_eq!(&signed_max[1..9], &[0xFF; 8]);
        assert_eq!(signed_max[9], 0x7F);
        // The payload type lives in field 5 in both of its legal values.
        assert_eq!(field_varint(5, 0), vec![40, 0]);
        assert_eq!(field_varint(5, 1), vec![40, 1]);
    }

    /// `bytes([(32 << 3) | 0])` raises `ValueError: bytes must be in range(0,
    /// 256)` in the reference — measured for both `_field_bytes(32, b'')` and
    /// `_field_varint(32, 0)`. Rust's `as u8` would answer `0` instead, so the
    /// refusal is the contract and a panic is its witness.
    #[test]
    #[should_panic(expected = "does not fit one protobuf tag byte")]
    fn field_thirty_two_is_refused_not_wrapped() {
        crate::pin_test_dir();
        let _ = field_bytes(32, b"");
    }

    /// Ten continuation bytes and a terminator: Python accumulates
    /// `2361183241434822606847`, whose low 64 bits are `u64::MAX`. That is the
    /// number asserted, because it is the number measured — this is the one
    /// place where the port and the reference differ in mechanism (Rust drops
    /// the bits past 64 instead of carrying them) and agree in outcome.
    ///
    /// Harmless by construction: the only varint we ever read is
    /// `payload_type ∈ {0, 1}`, and a sender that emits eleven bytes for it is
    /// not sending us a message we could act on anyway.
    #[test]
    fn a_varint_that_never_ends_keeps_the_low_sixty_four_bits() {
        crate::pin_test_dir();
        let buf: Vec<u8> = std::iter::once(0x08u8)
            .chain(std::iter::repeat(0xFF).take(10))
            .chain(std::iter::once(0x01))
            .collect();
        let fields = parse_fields(&buf);
        assert_eq!(fields.len(), 1, "one field, then the buffer is spent");
        assert_eq!(fields[0].number, 1);
        assert_eq!(
            fields[0].value,
            FieldValue::Varint(u64::MAX),
            "measured: Python's value mod 2^64"
        );
    }

    /// Five rows the reference produces on inputs no correct sender sends, each
    /// measured. A check that cannot run is a check that never passes, so these
    /// are here to prove the clamps are real rather than decorative.
    #[test]
    fn truncated_and_clipped_fields_answer_like_the_reference() {
        crate::pin_test_dir();
        // A tag with nothing after it: varint accumulated over an empty run = 0.
        assert_eq!(parse_fields(b"\x10"), vec![varint_field(2, 0)]);
        // A varint counter whose every byte says "more" runs to the end of the
        // buffer, contributing no bits: field 5, wire type 0, value 0. (0x28 is
        // (5<<3)|0, so this is *not* a length-prefixed field at all.)
        assert_eq!(parse_fields(b"\x28\x80\x80"), vec![varint_field(5, 0)]);
        // A length that overruns the body is clamped, not an error.
        assert_eq!(parse_fields(b"\x12\x09abc"), vec![bytes_field(2, b"abc")]);
        // An unsupported wire type (0x19 = field 3, wire type 1) stops the read
        // *before* the field that follows it — field 2 is never parsed.
        assert_eq!(
            parse_fields(&[0x08, 0x01, 0x19, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x12, 0x01, b'z']),
            vec![varint_field(1, 1)]
        );
        // The empty body yields no fields rather than a default one.
        assert_eq!(parse_fields(b""), vec![]);
    }

    fn varint_field(number: u32, value: u64) -> Field {
        Field {
            number,
            wire_type: 0,
            value: FieldValue::Varint(value),
        }
    }

    fn bytes_field(number: u32, value: &[u8]) -> Field {
        Field {
            number,
            wire_type: 2,
            value: FieldValue::Bytes(value.to_vec()),
        }
    }

    /// The same bail-out, told from the message side: a sender that puts a
    /// fixed64 field where we expect a varint gets a `CastMessage` with Python's
    /// dict defaults, not an exception. `_handle_client` in the reference wraps
    /// the whole read in one `except Exception` that logs at debug and closes;
    /// this is why that handler almost never has anything to say.
    #[test]
    fn an_unsupported_wire_type_ends_the_read_without_an_error() {
        crate::pin_test_dir();
        let body = encode_cast_message("receiver-0", "sender-1", NS_CONNECTION, "hi");
        // Replace the varint tag of field 1 with `field 1, wire type 1`.
        let mut broken = body;
        broken[0] = 0x09;
        let msg = parse_cast_message(&broken);
        assert_eq!(msg, CastMessage::default());
        assert_eq!(msg.payload_type, 0);
        assert_eq!(msg.payload_utf8, "");
    }

    #[test]
    fn a_text_body_round_trips_into_its_own_slots() {
        crate::pin_test_dir();
        let body = encode_cast_message(
            "receiver-0",
            "sender-1",
            NS_CONNECTION,
            r#"{"type": "CONNECT"}"#,
        );
        let msg = parse_cast_message(&body);
        assert_eq!(msg.source_id, "receiver-0");
        assert_eq!(msg.destination_id, "sender-1");
        assert_eq!(msg.namespace, NS_CONNECTION);
        assert_eq!(msg.payload_type, 0);
        assert_eq!(msg.payload_utf8, r#"{"type": "CONNECT"}"#);
        assert_eq!(msg.payload_binary, Vec::<u8>::new(), "the other slot stays empty");
    }

    /// The BINARY round trip must not touch `payload_utf8`. Python's elif chain
    /// keeps them in separate slots; a codec that decoded both into one string
    /// would turn an auth payload into replacement characters and still look
    /// like it parsed.
    #[test]
    fn a_binary_body_round_trips_without_touching_the_text_slot() {
        crate::pin_test_dir();
        let body = encode_cast_message(
            "receiver-0",
            "sender-1",
            NS_DEVICEAUTH,
            encode_auth_response().as_slice(),
        );
        let msg = parse_cast_message(&body);
        assert_eq!(msg.namespace, NS_DEVICEAUTH);
        assert_eq!(msg.payload_type, 1);
        assert_eq!(msg.payload_utf8, "");
        assert_eq!(msg.payload_binary, encode_auth_response());
    }

    /// `.decode("utf-8", "replace")` per row, measured on these eight inputs:
    /// `[255,255] → [65533,65533]`, `[226,130] → [65533]`,
    /// `[226,130,172] → [8364]` (a real €, which must survive),
    /// `[192,128] → [65533,65533]` (overlong encoding, two replacements),
    /// `[237,160,128] → [65533,65533,65533]` (a surrogate, three),
    /// `[97,226] → [97,65533]`, `[240,159,152] → [65533]` (a truncated emoji),
    /// `[240,159,152,128,98] → [128512,98]` (a whole emoji plus an ASCII).
    ///
    /// `String::from_utf8_lossy` agrees on all eight; the row that would have
    /// made this test decorative is `[226,130,172]`, because a codec that
    /// replaced everything would still pass the other seven.
    #[test]
    fn a_broken_utf8_source_id_replaces_instead_of_refusing() {
        crate::pin_test_dir();
        let rows: [(Vec<u8>, Vec<u32>); 8] = [
            (vec![255, 255], vec![65533, 65533]),
            (vec![226, 130], vec![65533]),
            (vec![226, 130, 172], vec![8364]),
            (vec![192, 128], vec![65533, 65533]),
            (vec![237, 160, 128], vec![65533, 65533, 65533]),
            (vec![97, 226], vec![97, 65533]),
            (vec![240, 159, 152], vec![65533]),
            (vec![240, 159, 152, 128, 98], vec![128512, 98]),
        ];
        for (input, expected) in rows {
            let body = field_bytes(2, &input);
            let msg = parse_cast_message(&body);
            let got: Vec<u32> = msg.source_id.chars().map(|c| c as u32).collect();
            assert_eq!(got, expected, "row {:?} decoded differently", input);
        }
    }

    /// The mistake that makes a device discoverable and uncastable (§4.1),
    /// asserted as a shape rather than as bytes.
    ///
    /// Deliberate: the assertion does **not** compare against Python's
    /// `json.dumps` text. That output spaces after every `:` and `,`, and
    /// `serde_json` emits compact JSON; pinning the whitespace would redden this
    /// test on perfectly good code. Whitespace is not part of the contract — the
    /// object-versus-string shape is, so that is what is asserted here, in both
    /// directions.
    #[test]
    fn namespaces_go_out_as_objects_not_strings() {
        crate::pin_test_dir();
        let list = app_namespaces();
        assert_eq!(list.len(), 4);
        assert_eq!(
            list.iter()
                .map(|v| v.as_object().map(|o| o.len()))
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default(),
            vec![1, 1, 1, 1],
            "every entry is an object with exactly one key"
        );
        let names: Vec<&str> = list
            .iter()
            .map(|v| v["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names, APP_NAMESPACES.to_vec(), "and in this order");
        assert_eq!(names[3], NS_PLAYER_MESSAGE);

        let text = serde_json::to_string(&list).unwrap();
        assert!(text.starts_with(r#"[{"name":"#), "{text}");
        assert!(
            !text.starts_with(r#"["urn:"#),
            "a bare string array is the §4.1 failure: {text}"
        );
        // NS_RECEIVER is the receiver's own namespace, never the app's.
        assert!(!text.contains(NS_RECEIVER), "{text}");
    }

    #[test]
    fn the_frame_header_is_a_big_endian_length() {
        crate::pin_test_dir();
        let frame = wrap_frame(b"abc");
        assert_eq!(frame, vec![0, 0, 0, 3, b'a', b'b', b'c']);
        assert_eq!(frame_length(&[0, 0, 1, 0]), 256);
        assert_eq!(frame_length(&[0x80, 0, 0, 0]), 2_147_483_648);
        let body = encode_cast_message("receiver-0", "sender-1", NS_CONNECTION, "{}");
        let frame = wrap_frame(&body);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + body.len());
        assert_eq!(&frame[FRAME_HEADER_LEN..], body.as_slice());
    }

    /// `pin_test_dir` is only a mechanism if it actually moved the log. The
    /// neighbour this asserts against is the path the fallback resolves to when
    /// the pin is absent — the same construction `macast-upnp`'s ssdp audit uses.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_suite_never_logs_into_the_user_directory() {
        crate::pin_test_dir();
        let log = macast_core::paths::log_file();
        let real = std::path::PathBuf::from(
            std::env::var("HOME").unwrap_or_default(),
        )
        .join("Library/Application Support/Macast/macast.log");
        assert_ne!(log, real, "the suite is writing the user's real macast.log");
        assert!(
            log.starts_with(std::env::temp_dir()),
            "the pin did not take: {}",
            log.display()
        );
    }

    /// Scan this crate's own sources and require every `#[test]` to open with the
    /// pin. The rules that make the scan itself honest:
    ///
    /// * the signature line is asserted to exist, so a test the scanner cannot
    ///   read is a failure rather than a silence;
    /// * the first **non-empty, non-comment** line of the body must be exactly
    ///   `crate::pin_test_dir();`;
    /// * the count is compared against a measured floor, so an empty scan can
    ///   never report success.
    #[test]
    fn every_test_pins_the_log_directory_first() {
        crate::pin_test_dir();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scanned = 0usize;
        let mut files = std::fs::read_dir(&root)
            .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect::<Vec<_>>();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if line.trim() != "#[test]" {
                    continue;
                }
                // A `#[should_panic]` sits between the attribute and the
                // signature; skip attributes, then demand the signature.
                let mut j = index + 1;
                while j < lines.len() && lines[j].trim().starts_with("#[") {
                    j += 1;
                }
                let signature = lines.get(j).unwrap_or(&"");
                assert!(
                    signature.trim_start().starts_with("fn ") && signature.contains('{'),
                    "{}:{}: no test signature follows the attribute",
                    path.display(),
                    index + 1
                );
                scanned += 1;
                let mut k = j + 1;
                while k < lines.len() {
                    let body = lines[k].trim();
                    if !body.is_empty() && !body.starts_with("//") && !body.starts_with("///") {
                        break;
                    }
                    k += 1;
                }
                let first = lines.get(k).unwrap_or(&"").trim();
                assert_eq!(
                    first, "crate::pin_test_dir();",
                    "{}:{}: this test does not pin its log directory first",
                    path.display(),
                    k + 1
                );
            }
        }
        assert!(
            scanned >= TESTS_IN_THIS_CRATE,
            "only {scanned} tests were scanned; the floor is {TESTS_IN_THIS_CRATE} \
             (this crate's measured count) — an empty scan is not a pass"
        );
    }

    /// The process-wide log threshold has exactly one owner in this crate.
    ///
    /// The setter in `macast_core::logging` writes a `static AtomicU8` shared by
    /// every test thread, so a test that raises the level to DEBUG and lowers it
    /// back on a line of its own is not "polite" — it is a window that another
    /// thread can close mid-turn. That is how this crate lost **5 runs out of 24**
    /// parallel ones (**0 out of 12** serial) before [`crate::DebugWindow`]
    /// existed: always an `mdns_tests` DEBUG witness reading an empty file,
    /// because `Logger::log` returns on `level < threshold()` **before** writing,
    /// and a neighbouring test had just put INFO back.
    ///
    /// A raw pair anywhere else re-creates the flake, and — this is why the rule
    /// is a scan rather than a behavioural test — the flake it recreates is
    /// **intermittent**, so no assertion in this crate can be trusted to catch
    /// it. What is catchable is the text: only `lib.rs` may name that setter at
    /// all, comments included, so prose cannot smuggle the pattern back in as
    /// "an example". The two needles are built by concatenation for the same
    /// reason: a scanner that contains its own offender would report a clean
    /// crate by matching itself out of existence — and, with the rule as written,
    /// `proto.rs` *is* one of the scanned files.
    #[test]
    fn only_lib_rs_touches_the_log_threshold() {
        crate::pin_test_dir();
        let setter = format!("set_{}", "threshold");
        let armed = format!("DebugWindow::{}()", "armed");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = std::fs::read_dir(&root)
            .unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect::<Vec<_>>();
        files.sort();
        let mut offenders: Vec<String> = Vec::new();
        let mut windows = 0usize;
        let mut owner_calls = 0usize;
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let is_owner = path.file_name().map(|n| n == "lib.rs").unwrap_or(false);
            for (index, line) in text.lines().enumerate() {
                if line.contains(&setter) && !is_owner {
                    offenders.push(format!("{}:{}", path.display(), index + 1));
                }
                // Counted only in the owner, and only as a *call*, so the sentence
                // in `lib.rs`'s own doc that names the setter as the thing to avoid
                // elsewhere is not mistaken for a second window.
                if is_owner && line.contains(&format!("{setter}(")) {
                    owner_calls += 1;
                }
                if line.contains(&armed) && !line.contains(&format!("format!(")) {
                    windows += 1;
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the log threshold is process-wide; open a DEBUG window with the crate \
             guard instead — {offenders:?}"
        );
        // The two floors are this crate's measured state, and they are what stops
        // the scan from reporting success on an empty reading: the owner really
        // calls the setter twice (raise in `armed`, restore in `Drop`), and there
        // really are twelve DEBUG windows — 4 in `mdns_tests`, 8 in
        // `targets_tests`.
        assert_eq!(
            owner_calls, 2,
            "lib.rs was expected to own exactly the raise and the restore, got {owner_calls}"
        );
        assert!(
            windows >= 12,
            "only {windows} DEBUG windows were found; the floor is 12 — a scan that \
             finds nothing is not a pass"
        );
    }
}
