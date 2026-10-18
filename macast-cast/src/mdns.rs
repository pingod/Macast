//! The mDNS/DNS-SD browser that answers "which Chromecasts answer on this LAN".
//!
//! Python counterpart: `discover_cast()` in
//! `macast/plugins/renderer/cast_local_file.py`, which does that work by
//! handing the packet to **zeroconf** (`_core.py` → `AsyncZeroconf` →
//! `_protocol.py`). This module is not a port of zeroconf — it is a port of
//! the one question `discover_cast()` asks and the twenty-some seconds of
//! machinery zeroconf spends to answer it. Everything below is written from
//! two references read side by side: the plugin's own code (what the answer
//! must contain) and zeroconf's `_protocol/incoming.py`, `_dns.py`, `_cache.py`
//! and `_services/info.py` (what a correct reading of the wire looks like).
//!
//! ## Provenance — a third layer, and it is not ours
//!
//! The DNS constants, the label-compression guard set, and the record-class
//! masking rule transcribed here come from **zeroconf**
//! (`_protocol/incoming.py:43-49`, `_dns.py:116-122`, `const.py:38-41`,
//! `:109-137`), which is **LGPL-2.1-or-later**. That is the same shape as
//! `macast/ssdp.py`'s Coherence author group (AGENTS.md §4.12): a layer whose
//! obligations belong neither to us nor to `xfangfang/Macast`, so rewriting
//! the code does not make it disappear. Every divergence from zeroconf's
//! reading is written down in this file rather than left as a silent difference
//! (AGENTS.md §4.14: 「和参考实现不一样」必须是文档).
//!
//! ## Why this is not just `UdpSocket` plus a parser
//!
//! Binding 5353 needs two decisions made **before** the bind, and `std::net`
//! cannot make them: it hands you a socket only after it has bound it.
//! `macast-cast/Cargo.toml:67-78` records the reason `socket2` is a dependency
//! of this crate at all, and this is that reason in use — `SO_REUSEPORT`
//! (`sock.rs:269-296` explains why the raw `setsockopt` instead of socket2's
//! setter, which lives behind `features = ["all"]` this workspace declines) and
//! the group join. So: socket2 for create/options/bind/join, then
//! `std::net::UdpSocket::from(sock)` for the IO.
//!
//! ⚠ **That is the opposite of [`macast_upnp::ssdp`]'s shape, on purpose.**
//! `ssdp.rs:597-609` reads datagrams off the socket2 `Socket` itself, through a
//! `[MaybeUninit<u8>; 1024]` buffer. This module deliberately does not copy it:
//! socket2's `recv_from` wants that `MaybeUninit` slice (`socket.rs:528`), and
//! this file's reason to exist is a **parser** — pushing `unsafe` into the code
//! that walks a stranger's packet is the worse trade. `impl From<Socket> for
//! net::UdpSocket` exists (`socket.rs:2479`), so the std conversion buys a
//! parser that holds no unsafe at the cost of one move.
//!
//! ## What the fold needs, and what it throws away
//!
//! `discover_cast()` wants `[(friendly name, host, port)]`. On the wire that is
//! three record types and one chain: a **PTR** saying an instance exists under
//! `_googlecast._tcp.local.`, an **SRV** giving that instance a target host and
//! port, and an **A** making that host an address we can open. TXT is read for
//! the *name* only (`fn` → `server` → instance name), never for completeness.
//! Everything else on the packet is skipped by its declared length. Note that
//! zeroconf decodes **eight** types, not five: HINFO, AAAA and NSEC are read
//! there and skipped here. The skip is still what the fold needs — none of those
//! three reach `discover_cast()`, and when Python's decode lands exactly on the
//! declared `rdlength` its cursor ends on the same byte our resync picks, so the
//! stream stays aligned either way. Where Python overshoots it drops the record
//! and prints a line (see [`log_skip_undecoded`] and [`log_skip_unknown`] for
//! how our wording diverges, and why it is written down rather than silently
//! different); where it succeeds we keep the difference in the record list only,
//! and that list is read by code that never asks for those types.
//!
//! ## Red lines
//!
//! * **Mask the record class before comparing anything.** `_dns.py:121-122`
//!   stores `class_ & 0x7FFF` and takes the flush bit out separately; real mDNS
//!   answers carry the cache-flush bit, so SRV/TXT/A arrive on the wire as
//!   `0x8001`, and a comparison against `1` that forgot the mask would match
//!   nothing. The failure is silent and total: zero devices, on a LAN full of
//!   them, while every hand-made fixture stays green (AGENTS.md §4.2
//!   「不提问的检查」). There is a test that sets `0x8000` for exactly this.
//! * **Key the ledgers on the lowercased name.** Python compares through
//!   `key = name.lower()` (`_dns.py:118`) and looks A records up by
//!   `server_key`. An SRV target of `Macast-abc.LOCAL.` with an A owner of
//!   `macast-abc.local.` is one device there and two here if we forget. Output
//!   keeps the original spelling.
//! * **Do not set `IP_MULTICAST_LOOP`.** zeroconf's listener never does — those
//!   options live only in the responder's send path
//!   (`_utils/net.py:208-234`, `set_respond_socket_multicast_options`), so the
//!   kernel default (loop on) applies and **our own** `_googlecast._tcp`
//!   announcement comes back to us. That is why the answer is filtered at the
//!   end by `drop_ours`, and why `discover_cast()`'s own docstring says
//!   「otherwise the first entry is this machine, and "投给我自己" is not a
//!   target」.
//! * **Never turn the poll loop into a query storm.** Every unanswered question
//!   is a re-transmitted multicast. `asked` is keyed case-insensitively and
//!   holds one entry per (name, type) for the whole browse: the PTR for the
//!   service type once, then SRV and TXT per newly seen instance, then A per
//!   newly learned server.
//! * **Send id 0 and match nothing.** `discover_cast()` runs a full browse, not
//!   a targeted query, so the answers it wants include **unsolicited**
//!   announcements (a device's periodic alive multicast). Matching responses to
//!   questions by id would drop exactly those, and RFC 6762 §10 permits
//!   ingesting them.

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use macast_core::logging::Logger;
use macast_core::setting::Setting;
use socket2::{Domain, Protocol, Socket, Type};

// ---------------------------------------------------------------------------
// Wire constants — transcribed from zeroconf's `const.py` and
// `_protocol/incoming.py:43-49`. The names carry the numbers they mask.
// ---------------------------------------------------------------------------

/// The mDNS port every responder answers on, and the only port a browser binds.
pub const MDNS_PORT: u16 = 5353;
/// The IPv4 multicast group, kept as text because Python keeps it as text
/// (`const.py:38`) and because an error message must print `224.0.0.251`.
pub const MDNS_GROUP: &str = "224.0.0.251";

/// The service type `_googlecast._tcp.local.` — `cast_local_file.py:98`.
pub const SERVICE_TYPE: &str = "_googlecast._tcp.local.";

/// RFC 1035 type numbers, only the five this fold reads.
pub const TYPE_A: u16 = 1;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_SRV: u16 = 33;

/// RFC 1035 class IN.
pub const CLASS_IN: u16 = 1;
/// The mask that gets the *class* out of the class field (`const.py:115`).
pub const CLASS_MASK: u16 = 0x7FFF;
/// The cache-flush bit sharing that field (`const.py:116`). Parsed, never
/// compared: `_dns.py:122` uses it only to set `unique`, and `unique` decides
/// whether a *responder* suppresses its own retransmission — not our problem.
pub const CLASS_UNIQUE: u16 = 0x8000;

/// The two high bits of a length byte mark a compressed label (`incoming.py:43`).
const POINTER_TAG: u8 = 0xC0;
const DNS_COMPRESSION_HEADER_LEN: usize = 1;
const DNS_COMPRESSION_POINTER_LEN: usize = 2;
/// `incoming.py:45` — the cap on the *chain*, checked twice: once while walking
/// it, once on the accumulated label count after a pointer resolves.
const MAX_DNS_LABELS: usize = 128;
/// `incoming.py:46` — checked on the joined name, in `_read_name`, not in the
/// label walker. A name longer than this is a corrupt packet.
const MAX_NAME_LENGTH: usize = 253;
/// `incoming.py:47` — a UDP datagram claiming to be longer than this is refused
/// before a single byte is parsed.
const MAX_MSG_LEN: usize = 0xFFFF;
/// RFC 1035 §3.3: a length byte carries six bits, so one label is at most 63
/// bytes. The reader accepts what the wire can express; the writer refuses to
/// emit what it cannot (`write_label`), which is the direction that matters.
const MAX_LABEL_LENGTH: usize = 63;

/// The largest datagram we read. Nothing useful arrives under the 1400-byte
/// link MTA that RFC 6762 §17 asks for, but a Chromecast announcing fifteen
/// records rides right up to the IP fragmentation limit, and an oversized
/// buffer costs one allocation per poll — so 8 KiB, bounded rather than tight.
const READ_MAX: usize = 8 * 1024;

/// How long one read blocks before the browse re-checks its deadline. Python
/// has no equivalent: zeroconf keeps its own async timers and
/// `cast_local_file.py` just spends `time.sleep(timeout)` once. This value is
/// purely the granularity of our poll — the `asked` ledger in [`browse`] is
/// what keeps a tick from re-multicasting a question it has already sent.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// The name of the face, so a log line says who is looking for devices.
/// Python's plugin logger is `logging.getLogger("LocalFileCaster")`
/// (`cast_local_file.py:95`); AGENTS.md §4.10b gives that name its own file.
const LOGGER_NAME: &str = "LocalFileCaster";

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// The rdata of the four shapes this fold decodes.
///
/// There is deliberately **no** "everything else" variant: a record whose type
/// we do not decode is dropped at the reader (`record_rdata`'s last arm returns
/// `None`, and [`read_others`] resyncs to the declared length), so it never
/// reaches this enum. A variant nothing can construct would promise a third
/// state — "we read it and chose not to decode it" — that the record list
/// cannot express, and `Ledger::ingest` would have to answer it with an empty
/// arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RData {
    A(Ipv4Addr),
    /// PTR and CNAME share one decode so the reader's offset stays exactly
    /// where zeroconf's would be. The fold only ever reads PTRs.
    Name(String),
    /// Raw, length-prefixed. Unpacking is the fold's job (`txt_pairs`), which
    /// mirrors `DNSText.text` staying bytes until `_properties` is asked for.
    Txt(Vec<u8>),
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
}

/// One resource record, as the fold needs it. `ttl` is kept because
/// `ttl == 0` is the *shape* of a goodbye (see [`Ledger::ingest`]), not because
/// this cache expires anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    /// Class **already masked** — this field never holds `0x8001`.
    pub class: u16,
    pub flush: bool,
    pub rtype: u16,
    pub ttl: u32,
    pub rdata: RData,
}

impl Record {
    /// The lowercase key every ledger in this file is indexed by, matching
    /// `_dns.py:118` (`self.key = name.lower()`).
    pub fn key(&self) -> String {
        self.name.to_lowercase()
    }
}

/// A question this browser asks, and the unit of the `asked` set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub rtype: u16,
}

/// A parsed datagram: its id (kept for the tests and for a future responder,
/// unused by the fold — see the red line about matching) and every record.
///
/// The three sections are **merged into one list in order** (answers, then
/// authority, then additional), because that is what `_read_others` does
/// (`incoming.py:407`: `self._num_answers + self._num_authorities +
/// self._num_additionals`), and because a real DNS-SD answer puts the instance
/// PTR in answers, its SRV/TXT in additional, and its A in additional too. A
/// reader that only walked `answers` would find names without addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub questions: Vec<Question>,
    pub records: Vec<Record>,
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Decode a DNS name at `offset`, following compression pointers.
///
/// `incoming.py:205-267` (`_decode_labels_at_offset`) + `:368-400`
/// (`_read_name`). This mirrors the reference **recursively**, and that is a
/// behaviour, not a style choice: a pointer *ends* the name it appears in, so
/// the offset the caller resumes at is `off + 2` regardless of how far the
/// target's own labels run. A loop that "continues at the target" instead — the
/// shape this function had until it was tested — consumes the target's labels
/// and reports the offset past *them*, which makes every record whose rdata
/// follows a compressed name start reading at the wrong byte.
///
/// `MAX_DNS_LABELS` is therefore two checks, in two places, exactly as Python
/// has them: the `depth > MAX_DNS_LABELS` refusal *before* the walk starts, and
/// the post-extend `len(labels) > MAX_DNS_LABELS` refusal after a pointer's
/// labels have been appended. The first bounds the chain length, the second the
/// accumulated label count; a long run of plain labels after a pointer is caught
/// only by the second.
///
/// **Both of zeroconf's caches are omitted** (`_name_cache` for partial label
/// runs, `name_str_cache` for pure pointers). They are pure optimizations:
/// `_read_name`'s own comment says 「A cache hit needs no re-validation because
/// entries are only written after a decode passed every check」, and an entry
/// is written only for a fully successful decode, so nothing that a cycle could
/// hide is ever cached. Dropping them costs time, not correctness, and a
/// per-browse map keyed by byte offset would be state this parser has no use
/// for. Python allocates `seen_pointers` lazily (only at the first pointer);
/// passing the parent's `Vec` down is at least as strict, because a pointer's
/// caller returns immediately after the recursive call, so no sibling branch
/// ever shares it.
///
/// Returns the name with its trailing dot and the offset just past the encoded
/// name. The trailing dot is not decoration: `_read_name` builds
/// `".".join(labels) + "."`, so `info.server` reads `Macast-abc.local.` — and
/// `discover_cast()` puts that string on the device list, so we keep it verbatim.
pub fn decode_name(data: &[u8], offset: usize, source: &str) -> Result<(String, usize), String> {
    let mut labels: Vec<String> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let end = decode_labels(data, offset, &mut labels, &mut seen, 0, source)?;
    Ok((finish(labels, source)?, end))
}

/// `_decode_labels_at_offset`, field for field.
///
/// `Ok(end)` where `end` is `off + DNS_COMPRESSION_HEADER_LEN` for a terminating
/// zero-length label and `off + DNS_COMPRESSION_POINTER_LEN` for a pointer —
/// those two constants are the whole difference between a name that ends and a
/// name that continues, and they differ by one byte.
fn decode_labels(
    data: &[u8],
    mut off: usize,
    labels: &mut Vec<String>,
    seen: &mut Vec<usize>,
    depth: usize,
    source: &str,
) -> Result<usize, String> {
    if depth > MAX_DNS_LABELS {
        return Err(format!(
            "DNS compression pointer chain exceeds {} at {} from {}",
            MAX_DNS_LABELS, off, source
        ));
    }
    while off < data.len() {
        let length = data[off];
        if length == 0 {
            return Ok(off + DNS_COMPRESSION_HEADER_LEN);
        }
        if length < 0x40 {
            // A plain label. Python slices `data[idx:idx+length]` without a
            // bounds check, so a label running past the end truncates and the
            // *next* loop iteration falls out of the `while` into the
            // corrupt-packet raise below. Refusing here instead is the same
            // answer for the same packet: either way this name is not decoded
            // and the record is dropped; only the message differs.
            let start = off + DNS_COMPRESSION_HEADER_LEN;
            let end = start + length as usize;
            if end > data.len() {
                return Err(corrupt_packet(source));
            }
            labels.push(decode_label(&data[start..end]));
            off = end;
            continue;
        }
        if length < POINTER_TAG {
            return Err(format!(
                "DNS compression type {} is unknown at {} from {}",
                length, off, source
            ));
        }
        // Pointer. Four guards, in Python's order, each with Python's sentence.
        if off + 1 >= data.len() {
            return Err(format!(
                "Truncated DNS compression pointer at {} from {}",
                off, source
            ));
        }
        let link = ((data[off] as usize & 0x3F) << 8) | data[off + 1] as usize;
        if link >= data.len() {
            return Err(format!(
                "DNS compression pointer at {} points beyond packet {} from {}",
                off, link, source
            ));
        }
        if link == off {
            return Err(format!(
                "DNS compression pointer at {} points to itself from {}",
                off, source
            ));
        }
        if seen.contains(&link) {
            return Err(format!(
                "DNS compression pointer at {} was seen again from {}",
                off, source
            ));
        }
        seen.push(link);
        let mut linked: Vec<String> = Vec::new();
        decode_labels(data, link, &mut linked, seen, depth + 1, source)?;
        labels.append(&mut linked);
        if labels.len() > MAX_DNS_LABELS {
            return Err(format!(
                "Maximum dns labels reached while processing pointer at {} from {}",
                off, source
            ));
        }
        return Ok(off + DNS_COMPRESSION_POINTER_LEN);
    }
    Err(corrupt_packet(source))
}

/// The message Python's `_read_name` raises when the label loop ends without
/// returning — the fallthrough after `while off < self._data_len`.
fn corrupt_packet(source: &str) -> String {
    format!(
        "Corrupt packet received while decoding name from {}",
        source
    )
}

/// The 253-**character** cap, applied where `_read_name` applies it: after
/// joining, before returning. Python compares `len(name)` on a `str`, so a
/// Chinese device name is measured in characters, not the UTF-8 bytes we hold in
/// the packet; `name.len()` here would reject a name Python accepts.
fn finish(labels: Vec<String>, source: &str) -> Result<String, String> {
    let name = format!("{}.", labels.join("."));
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(format!(
            "DNS name {} exceeds maximum length of {} from {}",
            name, MAX_NAME_LENGTH, source
        ));
    }
    Ok(name)
}

/// A label's bytes as text. zeroconf decodes with `("utf-8", "replace")`
/// (`_protocol/incoming.py:223`) — a device name in Chinese survives as itself,
/// garbage survives as replacement characters, and neither aborts the record.
/// [`String::from_utf8_lossy`] is that codec with one documented difference:
/// Python replaces each invalid **byte**, Rust each invalid maximal **sequence**,
/// so a run of malformed bytes can cost fewer U+FFFDs here than there. Nothing
/// reads a name for equality across that boundary, so the difference stays here.
fn decode_label(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Message parsing
// ---------------------------------------------------------------------------

/// Parse one mDNS datagram into the records the fold can use.
///
/// `source` is the peer text Python puts in its error messages
/// (`from {self.source}`), so a refusal names the machine that sent it.
///
/// **`None` means "this is not a message we can use at all"** and corresponds
/// to zeroconf's `DNSIncoming` coming back `valid == False` — the faults that
/// happen *before* `_read_others` is reached (`_initial_parse`: the length
/// guard, `_read_header`, `_read_questions`). `__init__` swallows them, the
/// object stays empty, and `answers()` on an empty object returns nothing.
///
/// A fault *inside* the record loop never empties a message. `_read_others`
/// (:402-454) runs three distinct recoveries, and they are three because the
/// parser knows different amounts at each point:
///
/// 1. the **owner name** or the 10-byte record header overruns the packet ⇒
///    there is no declared length to recover to, so the loop stops and the
///    records parsed *before* it are kept;
/// 2. `_read_record` raises on rdata ⇒ resync to the declared `end` and skip
///    this record;
/// 3. a record that decoded but did **not** consume exactly `rdlength` ⇒
///    resync to `end` and **drop** it — `end` is authoritative, and a record
///    whose internal framing disagrees with its outer length is a record whose
///    fields we cannot trust.
///
/// All three keep the prefix, which is why this returns the records rather
/// than a `Result`.
///
/// One deliberate **timing** divergence, not an outcome one: Python's
/// `_initial_parse` (`incoming.py:269-280`) only calls `_read_others()` when the
/// question section is empty, and otherwise leaves the reading to the `answers`
/// **property** (`incoming.py:145-153`), which lazily runs `_read_others()` the
/// first time anything asks for the records. A browser asks for the records, so
/// the records always get read — here unconditionally, at parse time. The two
/// shapes differ only in *when*, and only for a message that is never looked at.
pub fn parse_message(data: &[u8], source: &str, log: &Logger) -> Option<Message> {
    // `MAX_MSG_LEN` is not a memory guard, it is an **overflow precondition**:
    // `incoming.py:47-49` says rejecting anything larger 「keeps every unsigned
    // `offset + length` comparison far from wrapping」. Every `end = offset +
    // length` below relies on it, so the comparison stays in `usize` and never
    // saturates. Remove this guard and the packet-bound checks gain a second
    // job they cannot do at `usize::MAX`.
    if data.len() > MAX_MSG_LEN {
        return None;
    }
    let mut p = Parser {
        data,
        offset: 0,
        source,
        log,
    };
    let (id, qd, an, ns, ar) = p.read_header()?;
    let questions = p.read_questions(qd)?;
    // Widen each count before adding: three `u16`s sum to 196,605 at most,
    // which fits `u32`, but the arithmetic here is deliberately done in the
    // index type so `read_others` never has to re-widen or re-check.
    let records = p.read_others(an as usize + ns as usize + ar as usize);
    Some(Message {
        id,
        questions,
        records,
    })
}

struct Parser<'a> {
    data: &'a [u8],
    offset: usize,
    source: &'a str,
    log: &'a Logger,
}

impl<'a> Parser<'a> {
    /// The 12-byte header. Everything after the flags is a count this parser
    /// obeys, and nothing here looks at the QR/AA bits: a browser ingests
    /// answers whether or not the sender claims authority.
    fn read_header(&mut self) -> Option<(u16, u16, u16, u16, u16)> {
        let id = self.u16()?;
        let _flags = self.u16()?;
        let qd = self.u16()?;
        let an = self.u16()?;
        let ns = self.u16()?;
        let ar = self.u16()?;
        Some((id, qd, an, ns, ar))
    }

    /// Questions are read and kept (a responder will want them) but never
    /// matched against: see the red line about id 0. `_read_questions`
    /// (:456-476) raises on an overrun, which here means the whole message is
    /// refused — the same `valid == False` outcome as a bad header, because a
    /// truncated question section leaves no trustworthy offset for the records.
    ///
    /// There is no offset restore on this path. Every failure here discards the
    /// message, so rewinding the cursor would be arithmetic nobody reads; the
    /// one place where a rewind *is* read is [`read_others`], where recovery 1
    /// has to leave the stream where the record started.
    fn read_questions(&mut self, count: u16) -> Option<Vec<Question>> {
        let mut out = Vec::new();
        for _ in 0..count {
            let name = self.name().ok()?;
            let rtype = self.u16()?;
            let _class = self.u16()?;
            out.push(Question { name, rtype });
        }
        Some(out)
    }

    /// The record sections, merged, with the three recoveries described on
    /// [`parse_message`].
    fn read_others(&mut self, count: usize) -> Vec<Record> {
        let mut out = Vec::new();
        for _ in 0..count {
            let start = self.offset;
            let name = match self.name() {
                Ok(name) => name,
                Err(_) => {
                    // Recovery 1: no declared length exists yet, so stop with
                    // what we have — exactly Python's `break` out of its loop.
                    self.offset = start;
                    return out;
                }
            };
            let (rtype, class_field, ttl, length) = match self.record_header() {
                Some(header) => header,
                None => {
                    self.offset = start;
                    return out;
                }
            };
            let end = self.offset + length as usize;
            if end > self.data.len() {
                // A declared length that reaches past the packet is treated as
                // recovery 1, not recovery 2: the loop stops with the prefix.
                // This guard is **our** shape rather than a transcription, and
                // the difference it buys is exactly one thing — the records
                // *after* the overrun. Resyncing to `end` would move the cursor
                // off the end of the buffer, and every later record would then
                // be parsed from bytes that do not exist. The bad record
                // contributes nothing either way, so no message ever gains a
                // record from this branch; it only ever loses trailing ones.
                self.offset = start;
                return out;
            }
            // The five types `record_rdata` can read. The two skip lines below
            // key on **this** boundary rather than on Python's (which also
            // decodes HINFO, AAAA and NSEC), because the sentence "we tried to
            // read this record and could not" is only honest for a type we do
            // read; everything else gets our own wording.
            let decoded = matches!(
                rtype,
                TYPE_A | TYPE_PTR | TYPE_CNAME | TYPE_TXT | TYPE_SRV
            );
            let rdata_start = self.offset;
            let rdata = match self.record_rdata(rtype, length as usize) {
                // Recovery 2: this record contributes nothing and the stream
                // resyncs to the declared end.
                None => {
                    if decoded {
                        log_skip_undecoded(self.log, &name, rtype, end);
                    } else {
                        log_skip_unknown(self.log, rtype, rdata_start, self.source);
                    }
                    self.offset = end;
                    continue;
                }
                Some(rdata) => rdata,
            };
            if self.offset != end {
                // Recovery 3: exact-consumption rule. Drop the record, keep the
                // stream aligned.
                log_skip_imbalance(self.log, &name, rtype, length);
                self.offset = end;
                continue;
            }
            out.push(Record {
                name,
                class: class_field & CLASS_MASK,
                flush: class_field & CLASS_UNIQUE != 0,
                rtype,
                ttl,
                rdata,
            });
        }
        out
    }

    /// The 10 bytes after an owner name: type, class, ttl, rdlength. The class
    /// field comes back **unmasked** so [`Record`] can split it the way
    /// `_fast_init_entry` does; masking happens at the one place that builds a
    /// `Record`, which is what keeps the "mask before comparing" red line from
    /// depending on every caller remembering it.
    fn record_header(&mut self) -> Option<(u16, u16, u32, u16)> {
        let rtype = self.u16()?;
        let class = self.u16()?;
        let ttl = self.u32()?;
        let length = self.u16()?;
        Some((rtype, class, ttl, length))
    }

    /// `_read_record` (`incoming.py:478-558`) for the five types this fold reads.
    /// Anything else — and any fault inside one of the five — returns `None` and
    /// leaves the cursor **wherever the read stopped**: the resync and the
    /// complaint both belong to [`read_others`] (recovery 2), because only the
    /// caller knows whether the type is one we claim to read, and a type we do
    /// not read must not be reported as "we tried and could not".
    ///
    /// It never sees the declared end. Every arm here consumes from the rdata
    /// that is actually present; whether that amount *equals* the advertised
    /// `rdlength` is the caller's question (recovery 3), and answering it from
    /// inside the decoder would let one arm quietly disagree with the other
    /// four about what "consumed the record" means.
    fn record_rdata(&mut self, rtype: u16, length: usize) -> Option<RData> {
        match rtype {
            TYPE_A => {
                let bytes = self.take(4)?;
                let a = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
                Some(RData::A(a))
            }
            TYPE_PTR | TYPE_CNAME => {
                let name = self.name().ok()?;
                Some(RData::Name(name))
            }
            TYPE_TXT => {
                let bytes = self.take(length)?;
                Some(RData::Txt(bytes.to_vec()))
            }
            TYPE_SRV => {
                // Python bounds-checks *before* unpacking the fixed part with
                // its own sentence ("SRV record at offset … overruns packet").
                // A declared rdlength that reaches past the packet is already
                // caught by the guard in [`read_others`], so the only case left
                // for these reads is the one where the buffer simply runs out
                // mid-field — the same `None` a short A gets.
                let priority = self.u16()?;
                let weight = self.u16()?;
                let port = self.u16()?;
                let target = self.name().ok()?;
                Some(RData::Srv {
                    priority,
                    weight,
                    port,
                    target,
                })
            }
            _ => None,
        }
    }

    /// Read and decompress a name, advancing past it.
    fn name(&mut self) -> Result<String, String> {
        let (name, next) = decode_name(self.data, self.offset, self.source)?;
        self.offset = next;
        Ok(name)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.offset + n > self.data.len() {
            return None;
        }
        let slice = &self.data[self.offset..self.offset + n];
        self.offset += n;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// Python's `_read_others` debug line for a **known** type whose rdata would not
/// decode (`incoming.py:432-438`), with three documented divergences, all of
/// them deliberate:
///
/// * Python appends `while unpacking %r` — a full `repr` of the datagram — and
///   logs with `exc_info=True`, so a traceback follows. On a LAN one malformed
///   record per advertisement is normal traffic, and paying a packet dump plus a
///   stack trace for each of them is how a debug log becomes unreadable.
/// * Python prints `_TYPES.get(type_, type_)`, which is a **record class**
///   (`dnsrecord.DNSA` and friends), while we print the number. A number is what
///   you grep a capture with; a class name from a library the reader may not
///   have open is not.
/// * The offset is `end`, i.e. **after** the resync, exactly as Python prints it
///   (`self.offset = end` sits above the call). It names the byte the next record
///   starts at, not the rdata that failed — which is the more useful half of the
///   pair, and the one a reader can act on.
///
/// It goes through the project [`Logger`], **not** the `log` facade: this
/// workspace installs no `impl log::Log` anywhere, so a `log::debug!` here is a
/// write into a channel with no listener, while `Logger` is what routes records
/// into `logs/<Name>.log` (AGENTS.md §4.10b).
fn log_skip_undecoded(log: &Logger, name: &str, rtype: u16, end: usize) {
    log.debug(&format!(
        "Unable to parse; skipping record for {} with type {} at offset {}",
        name, rtype, end
    ));
}

/// Ours, not a transcription — the wording is deliberately unlike every line
/// above, because the situation is different. `_read_record`
/// (`incoming.py:478-548`) decodes **eight** types (A, CNAME, PTR, TXT, SRV,
/// HINFO, AAAA, NSEC) and only returns `None` **without logging** for anything
/// outside that table. This fold reads five, so HINFO/AAAA/NSEC land here too,
/// and two things follow: those records never reach `discover_cast()` either way
/// (so the device list is unchanged), and the cursor lands on the same `end`
/// Python's own resync would pick — which is why the *skip* is safe even where
/// Python decoded. An unasked line is still worth one debug line here, because
/// "why is this record missing" is a question a reader of a log will ask.
/// Naming the offset answers it: "record type 33 at offset 41" is findable in a
/// capture, "record type 33" is not.
fn log_skip_unknown(log: &Logger, rtype: u16, offset: usize, source: &str) {
    log.debug(&format!(
        "Skipping unknown record type {} at offset {} from {}",
        rtype, offset, source
    ));
}

/// Python's exact-consumption debug line (`incoming.py:446-450`), with the
/// `rdlength` it disagreed with. Verbatim including the absence of a `from
/// {source}` tail — Python has one here and not on its neighbour, and copying
/// that asymmetry is cheaper than explaining it later.
fn log_skip_imbalance(log: &Logger, name: &str, rtype: u16, length: u16) {
    log.debug(&format!(
        "Record for {} with type {} did not consume exactly rdlength={}; dropping",
        name, rtype, length
    ));
}

// ---------------------------------------------------------------------------
// TXT
// ---------------------------------------------------------------------------

/// Unpack a TXT rdata into `(key, value)` pairs in wire order.
///
/// `info.py:1044-1068` (`_unpack_text_into_properties`). Three behaviours are
/// load-bearing and each is a divergence from "split on commas" in a way a real
/// device will find:
///
/// * **A bare key is `None`, not `""`.** The comment there reads 「RFC 6763 §6.4
///   distinguishes a key with no '=' (a boolean attribute: present, no value)
///   from `key=` (present with an empty value), so test the separator, not the
///   value」. It matters here because `discover_cast()` picks a name with
///   `properties.get('fn') or info.server or name` — a `fn` that is present but
///   empty *and* a `fn` that is absent must both fall through, and only one of
///   them is a real device's choice.
/// * **First key wins.** `if key not in properties` — a duplicated key keeps the
///   first occurrence. `Vec` keeps the order and the fold applies the rule.
/// * **A length byte past the end truncates silently.** Python slices
///   `text[index:index+length]` with no bounds check, so a malformed TXT yields
///   the bytes that exist and stops, rather than raising.
pub fn txt_pairs(text: &[u8]) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    let mut out: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let length = text[index] as usize;
        index += 1;
        let end = (index + length).min(text.len());
        let key_value = &text[index..end];
        index += length;
        // `partition(b"=")` on bytes: the separator test is the whole RFC point.
        let (key, value) = match key_value.iter().position(|byte| *byte == b'=') {
            Some(at) => (
                key_value[..at].to_vec(),
                Some(key_value[at + 1..].to_vec()),
            ),
            None => (key_value.to_vec(), None),
        };
        if out.iter().any(|(held, _)| *held == key) {
            continue;
        }
        out.push((key, value));
    }
    out
}

/// The first value for `key`, decoded the way `discover_cast()` decodes it:
/// both the key and the value go through `bytes.decode('utf-8','replace')`
/// under an `isinstance(..., bytes)` guard, so a non-UTF-8 Google name arrives
/// as replacement characters rather than as an exception and a lost device.
///
/// Python's `if isinstance(value, str) and not value: value = None` step, and
/// the reason this can hand back `Some(String::new())` rather than folding it
/// away: the caller's `or` chain has to see the emptiness to choose past it, so
/// the choice lives here, once.
pub fn txt_value(pairs: &[(Vec<u8>, Option<Vec<u8>>)], key: &str) -> Option<String> {
    let (_, value) = pairs.iter().find(|(held, _)| held == key.as_bytes())?;
    let bytes = value.as_ref()?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// One Chromecast the browser is willing to name a target for.
///
/// `Ord` is derived over the fields **in this order**, which is what makes
/// `sorted()` here and `sorted(found.values())` in Python hand back the same
/// list: Python sorts the tuple `(friendly name, host, port)`, so the name
/// breaks ties, then the address, then the port.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CastDevice {
    pub name: String,
    pub address: String,
    pub port: u16,
}

/// What a browser accumulates over a browse: the records, in the three shapes
/// the fold needs.
///
/// This is a hand-written stand-in for zeroconf's cache
/// (`_cache.py:233-278 _async_add`), reduced to the questions the fold can
/// answer, and it keeps **two** behaviours that look like details and are not:
///
/// * **Address order is observation order, and the fold reads the last one.**
///   Python's chain is `parsed_addresses()` → `_set_ipv4_addresses_from_cache`
///   → `_get_ip_addresses_from_cache_lifo` (`info.py:775-801`), which walks
///   `cache.get_all_by_details(server_key, TYPE_A, CLASS_IN)` and reverses.
///   `get_all_by_details` (`_cache.py:200-206`) returns
///   `list(records.values())` — **dict insertion order** — and the sibling
///   `get_by_details` documents the same fact as 「the last one that was added
///   to the cache」. So the address we open is the *last distinct* A record
///   seen for that server.
///   The "distinct" half comes from `_async_add`'s `store[record] = record`
///   combined with `DNSAddress.__eq__` (`_dns.py:290-295`), which compares
///   address, scope id, name, type and class but **not TTL**: re-observing an
///   address already in the list does **not** move it, so the pop-then-append
///   branch at `info.py:787-797` can never fire. That branch is the reason this
///   ledger holds a `Vec` and a `contains` check rather than a set — a set would
///   order by address bytes and report the *highest* address of a dual-homed
///   device, which is a defensible guess and the wrong answer.
/// * **Goodbye is `ttl == 0`, and it removes rather than expires.**
///   `_cache.py` has a whole expiry machine (heap, `_MAX_CACHE_RECORDS`,
///   `_async_evict_oldest`) because a *resident* zeroconf holds a cache for
///   hours. A 3-second browse never reaches any of it, and RFC 6762 §10.1's
///   goodbye is the only removal that can arrive inside the window. Declared
///   divergence: no TTL arithmetic, no eviction, no expiry clock.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// PTR rdata keyed by the lowercased PTR name: which instances are claimed
    /// to exist under which service type.
    ptrs: BTreeMap<String, Vec<String>>,
    /// Per instance: the TXT pairs and the SRV's target/port, both optional
    /// until they arrive.
    instances: BTreeMap<String, PartialInstance>,
    /// Per **server** (the SRV target, not the instance): A addresses in
    /// observation order, first occurrence keeping its slot.
    addresses: BTreeMap<String, Vec<Ipv4Addr>>,
    service_type: String,
}

#[derive(Clone, Debug, Default)]
struct PartialInstance {
    txt: Option<Vec<u8>>,
    server: Option<String>,
    port: Option<u16>,
}

impl Ledger {
    /// A browser for one service type.
    pub fn new(service_type: &str) -> Ledger {
        Ledger {
            service_type: service_type.to_lowercase(),
            ..Default::default()
        }
    }

    /// Ingest every record in a parsed datagram. Returns the questions a
    /// responder would need answered next, so the caller can ask them once.
    ///
    /// Ingesting **all** records (not just the ones for `service_type`) is
    /// deliberate: an A record for a server we learned about from a PTR we did
    /// not ask for still belongs in the ledger, and dropping it is how a
    /// device ends up named but address-less.
    pub fn ingest(&mut self, records: &[Record]) -> Vec<Question> {
        let mut asked = Vec::new();
        for record in records {
            let key = record.key();
            if record.class != CLASS_IN {
                // A record for class CH or HS is not an address of anything:
                // `_dns_entry_matches` compares class_ before it compares
                // rdata, so honoring that here costs one branch and buys the
                // same "no cross-class confusion" that zeroconf has.
                continue;
            }
            match &record.rdata {
                RData::Name(target) => {
                    if record.rtype != TYPE_PTR {
                        continue;
                    }
                    // **Goodbye is tested before dedup, and it never adds.** The
                    // first draft of this arm pushed the name first and asked
                    // `ttl == 0` afterwards, so a brand-new instance's goodbye
                    // left its name in the PTR list forever — and because the
                    // list is exactly what [`Ledger::devices`] walks, the row
                    // stayed reachable to a re-ask while being unresolvable to
                    // the caller. Worse, the `contains` guard ran *before* the
                    // ttl test, so the **second** goodbye for a name we already
                    // knew did nothing at all: the instance was torn down once
                    // and its PTR entry survived. RFC 6762 §10.1 says a zero
                    // TTL means "this record is going away", so that question
                    // has to be asked first, of every goodbye, known or not.
                    if record.ttl == 0 {
                        self.forget_ptr(&key, target);
                        continue;
                    }
                    let list = self.ptrs.entry(key.clone()).or_default();
                    if list
                        .iter()
                        .any(|held| held.eq_ignore_ascii_case(target))
                    {
                        continue;
                    }
                    list.push(target.clone());
                    if key != self.service_type {
                        continue;
                    }
                    let entry = self.instance_entry(target);
                    if entry.server.is_none() {
                        asked.push(Question {
                            name: target.clone(),
                            rtype: TYPE_SRV,
                        });
                    }
                    if entry.txt.is_none() {
                        asked.push(Question {
                            name: target.clone(),
                            rtype: TYPE_TXT,
                        });
                    }
                }
                RData::Txt(text) => {
                    if record.rtype != TYPE_TXT {
                        continue;
                    }
                    if record.ttl == 0 {
                        self.instances.remove(&key);
                        continue;
                    }
                    self.instance_entry(&record.name).txt = Some(text.clone());
                }
                RData::Srv { target, port, .. } => {
                    if record.rtype != TYPE_SRV {
                        continue;
                    }
                    if record.ttl == 0 {
                        self.instances.remove(&key);
                        continue;
                    }
                    let server = target.clone();
                    let server_key = server.to_lowercase();
                    let entry = self.instance_entry(&record.name);
                    // Compared case-insensitively, because the address ledger is
                    // keyed on the lowercased server and [`Ledger::devices`]
                    // looks it up that way: two spellings of one target are one
                    // device there, and this arm must not read them as two.
                    let fresh_server = entry
                        .server
                        .as_deref()
                        .map(|held| held.to_lowercase())
                        .as_deref()
                        != Some(server_key.as_str());
                    entry.server = Some(server.clone());
                    entry.port = Some(*port);
                    if fresh_server {
                        asked.push(Question {
                            name: server,
                            rtype: TYPE_A,
                        });
                    }
                }
                RData::A(address) => {
                    if record.rtype != TYPE_A {
                        continue;
                    }
                    if record.ttl == 0 {
                        if let Some(list) = self.addresses.get_mut(&key) {
                            list.retain(|held| held != address);
                        }
                        continue;
                    }
                    let list = self.addresses.entry(key.clone()).or_default();
                    // The slot-preserving rule from the type's doc.
                    if !list.contains(address) {
                        list.push(*address);
                    }
                }
            }
        }
        asked
    }

    /// The instance's slot, created empty on first touch.
    ///
    /// There is no stored instance name: the display fallback on
    /// [`Ledger::devices`] is the **PTR rdata** it is walking, which is exactly
    /// the `name` that `cast_local_file.py:1037` reads
    /// (`properties.get('fn') or info.server or name`) — zeroconf hands that
    /// string to `add_service`, so the ledger would only be keeping a second
    /// copy of the same bytes under a different spelling.
    fn instance_entry(&mut self, name: &str) -> &mut PartialInstance {
        self.instances.entry(name.to_lowercase()).or_default()
    }

    /// A PTR goodbye, from either service type.
    ///
    /// The name always leaves the type's list — that list is the walk
    /// [`Ledger::devices`] takes, so leaving it behind would keep asking
    /// questions about an instance this browse has been told is gone. The
    /// instance itself is only torn down when the goodbye came under **our**
    /// type: a `_airplay._tcp` PTR naming the same host says nothing about
    /// whether the GoogleCast instance still answers, and zeroconf's cache
    /// agrees, since its PTR records are keyed by name and type.
    fn forget_ptr(&mut self, service_key: &str, target: &str) {
        if let Some(list) = self.ptrs.get_mut(service_key) {
            list.retain(|held| !held.eq_ignore_ascii_case(target));
            if list.is_empty() {
                self.ptrs.remove(service_key);
            }
        }
        if service_key == self.service_type {
            self.forget_instance(target);
        }
    }

    /// A PTR goodbye names an instance that is gone. Drop the instance and, if
    /// nothing else claims it, the address list we built under its server.
    fn forget_instance(&mut self, target: &str) {
        let key = target.to_lowercase();
        if let Some(removed) = self.instances.remove(&key) {
            if let Some(server) = removed.server {
                let server_key = server.to_lowercase();
                let still_referenced = self.instances.values().any(|held| {
                    held.server
                        .as_deref()
                        .map(|other| other.to_lowercase())
                        .as_deref()
                        == Some(server_key.as_str())
                });
                if !still_referenced {
                    self.addresses.remove(&server_key);
                }
            }
        }
    }

    /// Every instance claimed under our service type, folded into devices.
    ///
    /// Requirement: **SRV and at least one A**. TXT is *not* required, which is
    /// a declared divergence from `info.py:816-825` (`_is_complete` is
    /// `bool(self._txt_seen and (self._ipv4_addresses or
    /// self._ipv6_addresses))`, RFC 6763 §6). The divergence goes in the
    /// direction that keeps devices: `discover_cast()` only needs TXT for the
    /// name, and its chain already falls back to `info.server` then the
    /// instance name, so an instance whose TXT has not arrived yet is still a
    /// target we can open — while one without an address is not a target at all.
    ///
    /// Second declared divergence: **IPv4 only.** Python defaults to
    /// `IPVersion.All`, so an AAAA-only device yields `addresses[0]` and gets
    /// listed. This fold needs an A record and drops such a device. The
    /// announcement side of this crate (the next task) binds IPv4 too, and the
    /// send path that would need IPv6 has no socket yet, so listing a device we
    /// cannot route to is the worse failure.
    ///
    /// Third: the service-type match is `eq_ignore_ascii_case` on the lowercased
    /// key, where Python compares strings built by the same lowercase path. The
    /// two agree; the Rust one says so out loud instead of relying on both
    /// producers having normalized.
    pub fn devices(&self) -> Vec<CastDevice> {
        let instances = match self.ptrs.get(&self.service_type) {
            Some(instances) => instances,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for target in instances {
            let entry = match self.instances.get(&target.to_lowercase()) {
                Some(entry) => entry,
                None => continue,
            };
            let (server, port) = match (entry.server.as_ref(), entry.port) {
                (Some(server), Some(port)) => (server, port),
                _ => continue,
            };
            let addresses = match self.addresses.get(&server.to_lowercase()) {
                Some(addresses) if !addresses.is_empty() => addresses,
                _ => continue,
            };
            // The LAST distinct observation, for the reasons on `Ledger`.
            let address = addresses[addresses.len() - 1];
            out.push(CastDevice {
                name: self.device_name(entry, target),
                address: address.to_string(),
                port,
            });
        }
        out.sort();
        out
    }

    /// `properties.get('fn') or info.server or name`, with Python's falsiness:
    /// an absent, `None`, or empty `fn` all fall through to the SRV target, and
    /// a target that is only a dot falls through to the instance name.
    fn device_name(&self, entry: &PartialInstance, fallback: &str) -> String {
        if let Some(text) = &entry.txt {
            let pairs = txt_pairs(text);
            if let Some(fn_value) = txt_value(&pairs, "fn") {
                if !fn_value.is_empty() {
                    return fn_value;
                }
            }
        }
        if let Some(server) = &entry.server {
            if server != "." {
                return server.clone();
            }
        }
        fallback.to_string()
    }
}

// ---------------------------------------------------------------------------
// Own-address filter
// ---------------------------------------------------------------------------

/// Drop devices whose address is one of **our** interfaces.
///
/// `cast_local_file.py:1046-1049`:
/// ```python
/// ours = set(Setting.get_advertisable_ip())   # except Exception: ours = set()
/// return sorted(hit for hit in found.values() if hit[1] not in ours)
/// ```
/// The docstring above it gives the reason: Macast broadcasts
/// `_googlecast._tcp` like any Chromecast does, so without this the first entry
/// is this machine and "投给我自己" is not a target.
///
/// The degrade direction is the interesting half: if the interface list cannot
/// be read, Python falls back to the **empty set** — no filter at all, so our
/// own receiver is listed — rather than to a filter that removes everything.
/// A device list with one useless extra entry beats a device list that silently
/// explains nothing. [`discover_cast`] gets this for free by construction
/// ([`macast_core::net::advertised_hosts`] cannot fail), so this function is the pure
/// half and is exported for the test that checks the semantics.
pub fn drop_ours(devices: &[CastDevice], ours: &[String]) -> Vec<CastDevice> {
    devices
        .iter()
        .filter(|device| !ours.iter().any(|held| held == &device.address))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// A standard query for one name and type: 12-byte header, QDCOUNT=1, no
/// answers, recursion-desired clear.
///
/// The id is **0** and the flags are **0** — `cast_local_file.py` never
/// constructs a query because zeroconf does it, and zeroconf's browser queries
/// are answered by anyone who has the record, matched by name rather than by id.
/// Keeping id 0 and never matching on it is what lets the browse also ingest
/// the periodic unsolicited announcements (RFC 6762 §10), which is how a device
/// that was asleep when we started can still appear before the timeout ends.
///
/// A name that cannot go on the wire is a **refusal**, not a truncation. The
/// first draft clamped each label with `bytes.len().min(63)`, which writes a
/// length byte that disagrees with the bytes that follow it only when the label
/// is 64 bytes or longer — and in that case the datagram is well-formed enough
/// to send, so the failure comes back as the one thing worse than an error: an
/// answer about a name nobody asked for, and the instance never resolves.
/// Both illegal shapes are refused here (over-long label, and a name with no
/// labels at all, which otherwise encodes as the root) and [`browse`] logs
/// which one it gave up on.
pub fn query(name: &str, rtype: u16) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(32 + name.len());
    out.extend_from_slice(&0u16.to_be_bytes()); // id
    out.extend_from_slice(&0u16.to_be_bytes()); // flags: standard query
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    write_name(&mut out, name)?;
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(out)
}

/// Write a name as length-prefixed labels, uncompressed. A query has no reason
/// to compress: nothing to point back at.
fn write_name(out: &mut Vec<u8>, name: &str) -> io::Result<()> {
    let mut labels = name.split('.').filter(|label| !label.is_empty());
    let first = labels.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("mDNS query name {:?} has no labels", name),
        )
    })?;
    write_label(out, first)?;
    for label in labels {
        write_label(out, label)?;
    }
    out.push(0);
    Ok(())
}

fn write_label(out: &mut Vec<u8>, label: &str) -> io::Result<()> {
    let bytes = label.as_bytes();
    if bytes.len() > MAX_LABEL_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "mDNS label {:?} is {} bytes, over the {} a wire name allows",
                label,
                bytes.len(),
                MAX_LABEL_LENGTH
            ),
        ));
    }
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
    Ok(())
}

// ---------------------------------------------------------------------------
// The socket seam
// ---------------------------------------------------------------------------

/// Everything [`browse`] can do to the network, so the fold, the ledger, the
/// retransmit policy and the timeout are all testable without a LAN.
///
/// Same house shape as [`crate::resolve`]'s `fetch_page(page_url, opener)`
/// (`resolve.rs:3026`) and [`crate::relay`]'s ffmpeg-injection seams: the pure
/// decision lives in the function, the syscall arrives as an argument.
pub trait Wire {
    /// Send one already-encoded query.
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Read one datagram, or `None` if the poll interval expired with nothing
    /// arriving. `None` is a **timeout**, not a failure: the browse keeps
    /// going until its own deadline.
    fn recv(&mut self) -> io::Result<Option<Vec<u8>>>;
    /// `"ip:port"` of the peer for the parse messages, mirroring Python's
    /// `self.source`.
    fn source(&self) -> String;
}

/// The browse, over any [`Wire`].
///
/// The loop is Python's `time.sleep(timeout)` spent as a poll: send what has
/// not been asked, read what arrives, ask what the answers just made legal to
/// ask (SRV/TXT for a new instance, A for a new server), until the deadline.
///
/// `asked` is the anti-storm guard: without it every tick re-multicasts every
/// outstanding question, which on a quiet LAN means a device that is not there
/// gets asked for it dozens of times, and a network with several browsers
/// amplifies. The ledger is keyed `(lowercased name, type)`, so a PTR arriving
/// in mixed spelling does not open a second question for the same instance.
///
/// **Two choices this fold owes to `cast_local_file.py:1008-1058`, and one it
/// deliberately does not:**
///
/// - A question is entered in the ledger **before** the send, so a datagram
///   that fails to leave is *not* retried on the next tick. Python has no
///   `asked` ledger at all — `ServiceBrowser` dedups by record inside zeroconf,
///   and a `send` that raises there is logged and dropped, not queued again.
///   Retrying-on-failure would be the one behaviour the reference never had,
///   and it would convert a single `ENOBUFS` into a question storm for the rest
///   of the budget.
/// - The deadline is checked at the **top** of the loop. Python's shape is one
///   `time.sleep(timeout)` followed by a cache read, so the exact position of
///   our check is not a parity requirement; what it buys is that the browse
///   never blocks another [`POLL_INTERVAL`] past the time the caller asked it
///   to stop.
/// - The error message on a failed read is Python's verbatim
///   `'Chromecast discovery failed: %s'` (its blanket `except Exception`),
///   including the fact that it stops browsing and returns what it has already
///   heard rather than raising.
pub fn browse(
    wire: &mut dyn Wire,
    service_type: &str,
    timeout: Duration,
    log: &Logger,
) -> Vec<CastDevice> {
    let mut ledger = Ledger::new(service_type);
    let mut asked: Vec<(String, u16)> = Vec::new();

    ask(
        wire,
        &mut asked,
        &Question {
            name: service_type.to_string(),
            rtype: TYPE_PTR,
        },
        log,
    );

    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            break;
        }
        match wire.recv() {
            Ok(Some(data)) => {
                let source = wire.source();
                if let Some(message) = parse_message(&data, &source, log) {
                    // Collected before asking: `ledger.ingest` returns the
                    // questions the new records made legal, and `ask` needs
                    // `wire`, which the borrow of `ledger` would otherwise
                    // have to share with the iterator it came from.
                    let questions: Vec<Question> = ledger.ingest(&message.records);
                    for question in questions {
                        ask(wire, &mut asked, &question, log);
                    }
                } else {
                    log.warning(&format!(
                        "ignored an mDNS datagram that did not parse: {} bytes from {}",
                        data.len(),
                        source
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => {
                // A read that fails for a reason other than timeout. Python's
                // equivalent is the blanket `except Exception` around the
                // browse — 'Chromecast discovery failed: %s' — and its answer
                // is to stop and report what has already been heard.
                log.error(&format!("Chromecast discovery failed: {}", e));
                break;
            }
        }
    }
    ledger.devices()
}

/// Send one question unless this browse has already sent it.
///
/// A free function rather than a closure because the closure form needs
/// `wire` and `asked` in one scope while the caller holds `&mut ledger`: the
/// two borrows cannot both be live across `ledger.ingest()`, and a `let _ =
/// ledger;` on a parameter that does nothing is the smell that says the shape
/// is wrong.
fn ask(
    wire: &mut dyn Wire,
    asked: &mut Vec<(String, u16)>,
    question: &Question,
    log: &Logger,
) {
    let key = (question.name.to_lowercase(), question.rtype);
    if asked.iter().any(|held| *held == key) {
        return;
    }
    asked.push(key);
    let bytes = match query(&question.name, question.rtype) {
        Ok(bytes) => bytes,
        Err(e) => {
            // The name came off a wire record, so this is a device announcing
            // a name the wire cannot carry. Logged and left in the ledger: the
            // instance stays unresolved, which is the honest answer, and the
            // alternative — asking every tick — is the storm this fn exists to
            // prevent.
            log.error(&format!("mDNS query could not be encoded: {}", e));
            return;
        }
    };
    if let Err(e) = wire.send(&bytes) {
        // One question failing is not the browse failing: the same device
        // may well announce itself, and the remaining questions still run.
        log.error(&format!("mDNS query send failed: {}", e));
    }
}

// ---------------------------------------------------------------------------
// The real socket
// ---------------------------------------------------------------------------

/// A [`Wire`] over one bound, joined multicast socket.
struct Multicast {
    sock: UdpSocket,
    group: SocketAddr,
    peer: SocketAddr,
}

impl Wire for Multicast {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.sock.send_to(bytes, self.group)?;
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; READ_MAX];
        match self.sock.recv_from(&mut buf) {
            Ok((length, peer)) => {
                self.peer = peer;
                buf.truncate(length);
                Ok(Some(buf))
            }
            // std reports a read timeout as `WouldBlock` / `TimedOut`, and
            // `kind()` is the stable way to name it. This is the poll
            // interval elapsing, not an error: the caller keeps browsing.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn source(&self) -> String {
        self.peer.to_string()
    }
}

/// Browse for Chromecasts on the LAN: `[(name, address, port)]`, sorted.
///
/// Python counterpart is `discover_cast(timeout=3.0)`
/// (`cast_local_file.py:1006-1049`), whose socket work is zeroconf's. Two of
/// its sentences have no Rust counterpart and that is worth saying:
/// `'zeroconf is unavailable: cannot look for Chromecasts'` exists because the
/// plugin imports `zeroconf` inside the function and a missing pip package is
/// a real state there; here the browser is compiled in, so there is no
/// ImportError path. `'Chromecast discovery failed: %s'` **is** kept, in
/// [`browse`], for the read that fails for another reason.
///
/// Every socket failure logs one error and returns the empty list. That is
/// deliberately *not* [`macast_upnp::sock::set_reuse_for_ssdp`]'s contract:
/// `sock.rs:253-256` records that on Darwin and Windows a failing `setsockopt`
/// propagates and kills the SSDP thread while `running` stays true, because
/// there is a server ledger and an `is_healthy()` witness to notice. A browse
/// has neither — nobody is watching a thread, the caller just gets an answer —
/// so the failure has to be reported and swallowed in the same place.
pub fn discover_cast(setting: &Arc<Setting>, timeout: Duration) -> Vec<CastDevice> {
    let log = Logger::named(LOGGER_NAME);
    let wire = match open(setting, &log) {
        Some(wire) => wire,
        None => return Vec::new(),
    };
    let mut wire = wire;
    let devices = browse(&mut wire, SERVICE_TYPE, timeout, &log);
    // `Setting.get_advertisable_ip()` for the Rust core is
    // `net::advertised_hosts` — the same superset, before discovery narrows it
    // to the interface carrying the default route (macast-core's `net.rs:610`).
    let ours = macast_core::net::advertised_hosts(setting);
    drop_ours(&devices, &ours)
}

/// Build the listening socket: options, bind, join, then hand it to std.
///
/// The option order follows zeroconf's `new_socket`
/// (`_utils/net.py:237-296`), and the fact that **both** `SO_REUSEADDR` and
/// `SO_REUSEPORT` are set **before** `bind` is the whole reason socket2 is in
/// this crate's dependency list (`Cargo.toml:67-78`: std binds first and only
/// then hands you the socket).
///
/// Multicast *loop* and *TTL* are **not** set. Those two options live only in
/// zeroconf's responder send path (`net.py:208-234`), so a browser leaves the
/// kernel default — loop on — and hears its own announcement. The consequence
/// is [`drop_ours`], not a bug to fix here.
fn open(setting: &Arc<Setting>, log: &Logger) -> Option<Multicast> {
    let sock = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
        Ok(sock) => sock,
        Err(e) => {
            log.error(&format!("mDNS cannot open a socket: {}", e));
            return None;
        }
    };
    set_reuse_for_mdns(&sock, log);
    let bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into();
    if let Err(e) = sock.bind(&bind) {
        // zeroconf treats `EADDRNOTAVAIL` as a warning and returns None
        // ("expected to happen on some systems"), and `EADDRINUSE` as fatal
        // with the comment 「This is still a fatal error as its not going to
        // work if we can't hear the traffic coming in」. std gives us one
        // `io::Error` and no errno, so this says the honest weaker thing: we
        // cannot hear the traffic, the answer is an empty list, and the caller
        // tells the user to search again.
        log.error(&format!("mDNS cannot bind {}: {}", MDNS_PORT, e));
        return None;
    }
    // Join the group on every interface `advertised_hosts` names, the way
    // zeroconf joins on every address the responder knows. A failure on one
    // interface is a warning, never a stop: `net.py:327-376` swallows six
    // errnos for exactly this reason (EADDRINUSE, ENOBUFS with the Linux
    // `net.ipv4.igmp_max_memberships` hint, EADDRNOTAVAIL, EINVAL on WSL,
    // ENOPROTOOPT for a disconnected adapter, ENODEV on v6) and raises only on
    // what is genuinely unexpected.
    let group: Ipv4Addr = MDNS_GROUP
        .parse()
        .expect("the mDNS group constant is a valid IPv4 address");
    let interfaces = multicast_interfaces(&macast_core::net::advertised_hosts(setting), log);
    let mut joined = 0usize;
    for interface in &interfaces {
        match sock.join_multicast_v4(&group, interface) {
            Ok(()) => joined += 1,
            Err(e) => log.warning(&format!(
                "mDNS cannot join {} on {}: {}",
                MDNS_GROUP, interface, e
            )),
        }
    }
    if joined == 0 {
        // Without a membership we hear only traffic routed to our unicast
        // socket, which on mDNS is nearly nothing. Say which interfaces were
        // offered rather than leaving "no devices" as the only clue.
        log.error(&format!(
            "mDNS joined no multicast interface (offered: {})",
            describe_interfaces(&interfaces)
        ));
    }
    // And still bind, still browse: `joined == 0` is not a refusal. The socket
    // is bound to 0.0.0.0:5353, so **replies to the queries we send** still
    // reach us as unicast to our source address, and on a machine whose only
    // interfaces refused the join that is strictly more than the empty list
    // this function could return instead. The browse is therefore run, and an
    // empty answer is reported by [`super::targets`] as "no devices found",
    // which is the honest shape of "we could listen but not on the group".
    let sock: UdpSocket = sock.into();
    if let Err(e) = sock.set_read_timeout(Some(POLL_INTERVAL)) {
        log.error(&format!("mDNS cannot set a read timeout: {}", e));
        return None;
    }
    Some(Multicast {
        sock,
        group: SocketAddr::from((group, MDNS_PORT)),
        peer: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
    })
}

/// The interfaces to join, as the four-octet addresses the kernel wants.
///
/// Python's browser joins on `InterfaceChoice.All`, which for v4 resolves to the
/// same superset `Setting.get_advertisable_ip()` returns; the mDNS
/// *announcement* side of this crate narrows it to the default-route carrier
/// (AGENTS.md §4.1, first row) and that narrowing is a property of the
/// responder, not of a listener — a browser joins everywhere it can and lets the
/// filter at the end decide what to show.
///
/// The list comes in as text so the parse is a seam a test can drive: an entry
/// that is not an IPv4 address is skipped with a warning rather than ending the
/// browse, because it is one interface among several.
fn multicast_interfaces(texts: &[String], log: &Logger) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for text in texts {
        match text.parse::<Ipv4Addr>() {
            Ok(addr) => out.push(addr),
            Err(_) => log.warning(&format!("mDNS skipping non-IPv4 interface {}", text)),
        }
    }
    out
}

fn describe_interfaces(interfaces: &[Ipv4Addr]) -> String {
    if interfaces.is_empty() {
        return "none".to_string();
    }
    interfaces
        .iter()
        .map(Ipv4Addr::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `SO_REUSE*` choice for a **5353 listener**, kept next to the browser
/// because this is where its cost is measurable.
///
/// The order is zeroconf's, read from `new_socket` (`_utils/net.py:237-262`):
/// **`SO_REUSEADDR` first and unconditionally**, then `SO_REUSEPORT` through
/// `set_so_reuseport_if_available` (`net.py:195-205`), which returns early when
/// the platform's `socket` module has no such option — which is Windows. So
/// every platform that can has both, and the ones it has them in is not a
/// platform-dependent choice at all. The three `#[cfg]` arms this function
/// replaces were copied from [`macast_upnp::sock::set_reuse_for_ssdp`], whose
/// table describes **macast's own `ssdp.py`**, not zeroconf; that file has no
/// `SO_REUSEADDR` on macOS and gives up after a successful `SO_REUSEPORT`, and
/// neither of those is what a 5353 browser does.
///
/// The one deliberate deviation: Python sets `SO_REUSEADDR` **outside any
/// `try`** and re-raises anything but `errno.ENOPROTOOPT` from the `SO_REUSEPORT`
/// call, because there the socket backs a responder thread with a health witness
/// that can act on a death. Here both failures **warn and continue** — see the
/// note on [`discover_cast`] about why a browse has nobody to propagate to.
///
/// ⚠ The message strings say `mDNS`, never `SSDP`, and this helper is a
/// deliberate duplicate of [`macast_upnp::sock::set_reuse_for_ssdp`] rather
/// than a call into it. A failing option on a 5353 bind logged as
/// 「SSDP cannot set SO_REUSEPORT」 would send whoever reads the log to the
/// SSDP server, which is a different socket on a different port with a
/// different failure mode. The failure *shape* is copied from that function's
/// Linux arm (`sock.rs:310-330`): `debug` on success, and on failure two
/// `error` lines with the OS text on its **own** line — the split AGENTS.md
/// §5 keeps so `grep -aE "ERROR|Traceback"` finds a reason next to the label.
pub(crate) fn set_reuse_for_mdns(sock: &Socket, log: &Logger) {
    match sock.set_reuse_address(true) {
        Ok(()) => log.debug("mDNS set SO_REUSEADDR"),
        Err(e) => log_reuse_failure("SO_REUSEADDR", &e, log),
    }
    set_reuse_port_if_available(sock, log);
}

/// Unix: `SO_REUSEPORT`, after `SO_REUSEADDR` and never instead of it.
#[cfg(unix)]
fn set_reuse_port_if_available(sock: &Socket, log: &Logger) {
    match set_reuse_port(sock) {
        // `ENOPROTOOPT` is the one failure Python tolerates silently here; the
        // rest are reported, and reported and swallowed rather than raised.
        Ok(()) => log.debug("mDNS set SO_REUSEPORT"),
        Err(e) => log_reuse_failure("SO_REUSEPORT", &e, log),
    }
}

/// Windows: no second option exists to set, which is the `hasattr` early return
/// at `net.py:198` rather than a failure of ours to handle.
#[cfg(not(unix))]
fn set_reuse_port_if_available(_sock: &Socket, _log: &Logger) {}

/// `setsockopt(SOL_SOCKET, SO_REUSEPORT, 1)` — the raw call, for the reason
/// `sock.rs:269-276` states once for the workspace: socket2's setter is behind
/// `features = ["all"]`, and turning that on recompiles a copy already in the
/// graph to buy one option.
#[cfg(unix)]
fn set_reuse_port(sock: &Socket) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let on: libc::c_int = 1;
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &on as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The two-line failure, in the shape `sock.rs:310-330` uses.
fn log_reuse_failure(option: &str, error: &io::Error, log: &Logger) {
    log.error(&format!("mDNS cannot set {}", option));
    log.error(&error.to_string());
}

#[cfg(test)]
#[path = "mdns_tests.rs"]
mod mdns_tests;
