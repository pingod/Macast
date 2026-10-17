//! The test face for [`super::mdns`] — mDNS/DNS-SD parsing and the discovery fold.
//!
//! Three shapes of witness, deliberately different for the three kinds of claim:
//!
//! * **Byte-level fixtures** for the parser. Every packet here is hand-built, so
//!   a green test means this reader and this byte sequence agree, not that two
//!   implementations of the same Python library agree.
//! * **A fake [`super::mdns::Wire`]** for [`super::mdns::browse`]. The module doc says the
//!   fold, the ledger, the retransmit policy and the timeout are all testable
//!   without a LAN; this is that claim being paid for. **Do not read
//!   `relay_tests.rs:3` ("no mocks") as a house rule that applies here** — that
//!   file tests a relay against a real origin, and a real socket cannot replay a
//!   scripted sequence of announcements on a deadline.
//! * **The project [`macast_core::logging::Logger`]** for the DEBUG/WARNING/ERROR
//!   lines, each test with a logger name of its own so one writer owns one file
//!   and a count cannot be polluted by a sibling test on another thread.
//!
//! ## What is *not* tested here, and why
//!
//! [`super::mdns::open`] and [`super::mdns::discover_cast`] bind real UDP `0.0.0.0:5353`.
//! On this machine that means either stealing the user's own mDNS traffic or
//! failing to bind at all, so neither is called: the five failure strings inside
//! `open`, the join WARNING, and the "joined no multicast interface" ERROR have
//! **no witness**. The pure helpers on that side (`multicast_interfaces`,
//! `describe_interfaces`, `set_reuse_for_mdns`, `log_reuse_failure`) are
//! drivable without a bind and are covered; the socket's *failure text* is not.
//! Per the design doc §13: 0 红 means nobody asked, and here a whole set of
//! questions genuinely went unasked. That is recorded, not papered over.

use std::collections::VecDeque;
use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

use macast_core::logging::{read_tail, Logger};
use socket2::{Domain, Protocol, Socket, Type};

use super::*;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

/// `name_to_wire`: uncompressed labels plus the terminating zero byte.
fn wire_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.').filter(|label| !label.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// One record's rdata for a TXT, length-prefixed the way the wire does it.
fn txt_rdata(pairs: &[(&str, Option<&str>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in pairs {
        let bytes = match value {
            Some(value) => format!("{}={}", key, value),
            None => key.to_string(),
        };
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes.as_bytes());
    }
    out
}

/// A record header the way it sits on the wire, class **not** masked.
fn rr(name: &str, rtype: u16, class: u16, ttl: u32, rdata: &[u8]) -> Vec<u8> {
    let mut out = wire_name(name);
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
    out
}

/// A datagram: header, then the three record sections merged the way
/// [`Parser::read_others`] reads them.
fn message(questions: &[(String, u16)], answers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // id
    out.extend_from_slice(&0u16.to_be_bytes()); // flags
    out.extend_from_slice(&(questions.len() as u16).to_be_bytes());
    out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // authority
    out.extend_from_slice(&0u16.to_be_bytes()); // additional
    for (name, rtype) in questions {
        out.extend_from_slice(&wire_name(name));
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
    }
    for record in answers {
        out.extend_from_slice(record);
    }
    out
}

/// A record built as the fold's own type, so a test can hand `ingest` a class
/// field the parser would never let through (the cache-flush bit included).
fn record(name: &str, rtype: u16, ttl: u32, rdata: RData) -> Record {
    Record {
        name: name.to_string(),
        class: CLASS_IN,
        flush: false,
        rtype,
        ttl,
        rdata,
    }
}

/// The same with the class field raw, for the red line about masking.
fn raw_record(name: &str, rtype: u16, class: u16, ttl: u32, rdata: RData) -> Record {
    Record {
        name: name.to_string(),
        class,
        flush: false,
        rtype,
        ttl,
        rdata,
    }
}

/// A witness file nobody else writes to. `claim()` is first-writer-wins, so the
/// name has to be unique per test, not per module.
fn witness(name: &str) -> (std::sync::Arc<Logger>, std::path::PathBuf) {
    let logger = Logger::named(name);
    let path = logger.claim();
    (logger, path)
}

/// Read a witness back. Tests read the [`Logger::claim`] return value rather
/// than re-deriving a path through `module_log_path`, so a change to the
/// sanitising rule cannot silently redirect the read away from the write.
fn lines(path: &std::path::Path) -> String {
    read_tail(path, 200).text
}

// ---------------------------------------------------------------------------
// A: the name walker — every refusal sentence, and the offset contract
// ---------------------------------------------------------------------------

#[test]
fn a_plain_name_ends_one_byte_past_the_terminator() {
    crate::pin_test_dir();
    // The `+1` versus the pointer's `+2` is the whole difference between a name
    // that ends and a name that continues, and they differ by one byte.
    let data = [3u8, b'f', b'o', b'o', 3, b'b', b'a', b'r', 0];
    let (name, end) = decode_name(&data, 0, "peer").expect("a plain name decodes");
    assert_eq!(name, "foo.bar."); // the trailing dot is not decoration
    assert_eq!(end, data.len());
    assert_eq!(end, 9); // 3+1 + 3+1 + 1
}

#[test]
fn a_root_name_is_a_single_dot() {
    crate::pin_test_dir();
    let (name, end) = decode_name(&[0u8], 0, "peer").expect("the root is a name");
    assert_eq!(name, ".");
    assert_eq!(end, 1);
}

#[test]
fn b_pointer_ends_the_name_and_resumes_two_bytes_past_itself() {
    crate::pin_test_dir();
    // The behaviour the first draft of `decode_labels` got wrong: a pointer
    // *ends* the name it appears in, so the caller resumes at `off + 2` no
    // matter how far the target's own labels run. A loop that "continues at the
    // target" instead consumes the target's labels and reports the offset past
    // *them*, which puts every record whose rdata follows a compressed name at
    // the wrong byte.
    let mut data = vec![3u8, b'f', b'o', b'o', 0]; // "foo." at 0
    let pointer_at = data.len(); // 5
    data.extend_from_slice(&[0xC0u8, 0x00]);
    let (name, end) = decode_name(&data, pointer_at, "peer").expect("a pointer decodes");
    assert_eq!(name, "foo.");
    assert_eq!(end, pointer_at + 2);
    assert_eq!(end, data.len());
}

#[test]
fn c_pointer_chain_composes_prefix_and_target() {
    crate::pin_test_dir();
    // `\x03dev\\x04cast\x00` at 0, then a name that points into its middle.
    let mut data = vec![3u8, b'd', b'e', b'v', 4, b'c', b'a', b's', b't', 0];
    let at = data.len();
    data.push(0xC0 | 0);
    data.push(4); // pointer to offset 4 == the "cast" label
    let (name, end) = decode_name(&data, at, "peer").expect("a chain composes");
    assert_eq!(name, "cast.");
    assert_eq!(end, at + 2);
}

#[test]
fn d_pointer_to_self_is_refused() {
    crate::pin_test_dir();
    let data = [0xC0u8, 0x00];
    let error = decode_name(&data, 0, "host:5353").unwrap_err();
    assert_eq!(error, "DNS compression pointer at 0 points to itself from host:5353");
}

#[test]
fn d_pointer_past_the_end_is_refused_and_names_the_target() {
    crate::pin_test_dir();
    // The second slot carries the *pointer value*, not the offset — a reader
    // chasing a bad packet wants to know where it was aimed.
    let data = [0xC0u8, 0xFF];
    let error = decode_name(&data, 0, "peer").unwrap_err();
    assert_eq!(error, "DNS compression pointer at 0 points beyond packet 255 from peer");
}

#[test]
fn d_pointer_on_the_last_byte_is_refused_as_truncated() {
    crate::pin_test_dir();
    let data = [3u8, b'a', b'b', b'c', 0xC0];
    let error = decode_name(&data, 4, "peer").unwrap_err();
    assert_eq!(error, "Truncated DNS compression pointer at 4 from peer");
}

#[test]
fn d_a_second_visit_to_the_same_pointer_is_a_cycle() {
    crate::pin_test_dir();
    // `seen` holds the **link targets**, not the offsets, and it is shared
    // downward, so the cycle is caught by the "was seen again" guard, *not* by
    // the depth guard. A fixture that tripped the depth message instead would be
    // testing the wrong machine.
    //
    // 0 -> 2, 2 -> 4, and 4 -> 2 again: the third pointer's target is already in
    // `seen`, so the refusal names offset 4. Writing `4 -> 0` instead looks like
    // the same story but is not — 0 was never a *target*, so nothing repeats.
    let data = vec![0xC0u8, 0x02, 0xC0, 0x04, 0xC0, 0x02];
    let error = decode_name(&data, 0, "peer").unwrap_err();
    assert_eq!(error, "DNS compression pointer at 4 was seen again from peer");
}

#[test]
fn e_unknown_compression_type_prints_the_raw_length_byte() {
    crate::pin_test_dir();
    // 0x41..0xBF is neither a plain label nor a pointer. The first byte here is
    // 65, and the message prints 65 — the raw byte, not a masked value.
    let data = [0x41u8, 0x00];
    let error = decode_name(&data, 0, "peer").unwrap_err();
    assert_eq!(error, "DNS compression type 65 is unknown at 0 from peer");
}

#[test]
fn e_sixty_three_is_a_legal_label_length_and_sixty_four_is_not() {
    crate::pin_test_dir();
    // The arm is `length < 0x40`, so 0x3F (63) is a plain label and 0x40 (64)
    // falls into the "unknown compression type" refusal. One byte of boundary.
    let label = [b'a'; 63];
    let mut data = vec![0x3Fu8];
    data.extend_from_slice(&label);
    data.push(0);
    let (name, _) = decode_name(&data, 0, "peer").expect("63 bytes is a label");
    assert_eq!(name.chars().count(), 64); // 63 a's plus the trailing dot

    let mut data = vec![0x40u8];
    data.extend_from_slice(&[b'a'; 64]);
    let error = decode_name(&data, 0, "peer").unwrap_err();
    assert_eq!(error, "DNS compression type 64 is unknown at 0 from peer");
}

#[test]
fn f_plain_label_running_past_the_end_is_a_corrupt_packet() {
    crate::pin_test_dir();
    // Python's slice truncates and the next iteration falls out of the `while`
    // into the same raise; only the message differs, and both refuse the name.
    let data = [0x08u8, b'a', b'b'];
    let error = decode_name(&data, 0, "192.0.2.1:5353").unwrap_err();
    assert_eq!(error, "Corrupt packet received while decoding name from 192.0.2.1:5353");
}

#[test]
fn g_a_run_of_labels_after_a_pointer_is_the_second_cap() {
    crate::pin_test_dir();
    // `MAX_DNS_LABELS` is checked twice, in two places, with two sentences:
    // `depth > MAX` walks the chain, `labels.len() > MAX` counts what a pointer
    // appended. This fixture trips the *second* one, so a long run of plain
    // labels behind one pointer is caught only there.
    let mut data: Vec<u8> = Vec::new();
    for _ in 0..129 {
        data.push(1);
        data.push(b'x');
    }
    data.push(0); // the target: 129 labels, terminated
    let at = data.len();
    data.push(0xC0);
    data.push(0);
    let error = decode_name(&data, at, "peer").unwrap_err();
    assert!(
        error.starts_with("Maximum dns labels reached while processing pointer at "),
        "expected the accumulated-count refusal, got {:?}",
        error
    );
    assert!(error.ends_with(" from peer"));
}

#[test]
fn g_the_depth_cap_names_the_chain_it_refuses() {
    crate::pin_test_dir();
    // 130 pointers, each aiming at the *next* pair, so the walk depth passes the
    // cap before any label count does (nothing here is a label). Both guards are
    // strictly greater than `MAX_DNS_LABELS`, so the refusal comes from the call
    // that enters with depth = MAX_DNS_LABELS + 1, i.e. the pointer at byte
    // 2 * (MAX_DNS_LABELS + 1).
    let pairs = MAX_DNS_LABELS + 2;
    let mut data: Vec<u8> = Vec::new();
    for i in 0..pairs {
        let target = 2 * (i + 1);
        data.push(0xC0u8 | (target >> 8) as u8);
        data.push((target & 0xFF) as u8);
    }
    let refused_at = 2 * (MAX_DNS_LABELS + 1);
    assert!(refused_at + 1 < data.len(), "the guard must fire inside the packet");
    // The target offset sits inside the packet, so the refusal is the chain
    // guard rather than the bounds guard.
    let error = decode_name(&data, 0, "peer").unwrap_err();
    assert_eq!(
        error,
        format!(
            "DNS compression pointer chain exceeds {} at {} from peer",
            MAX_DNS_LABELS, refused_at
        ),
        "the chain guard must name the offset it refused at"
    );
}

#[test]
fn h_the_name_cap_counts_characters_not_bytes() {
    crate::pin_test_dir();
    // `finish` measures `name.chars().count()` because Python measures `len()`
    // on a `str`. This name is 250 characters and 742 UTF-8 bytes: `name.len()`
    // here would reject a name Python accepts, which is the whole divergence.
    let label = "中".repeat(125); // 125 chars, 375 bytes
    let mut data: Vec<u8> = Vec::new();
    data.push(label.len() as u8); // a 375-byte label cannot be on the wire,
    // so build the name through the public entry the way the reader would:
    // hand `finish` the labels directly instead.
    let _ = data.len();
    let joined = finish(vec![label.clone(), "local".into(), "on".into()], "peer").expect("250 chars fits");
    assert_eq!(joined.chars().count(), 125 + 1 + 5 + 1 + 2 + 1);
    assert!(joined.chars().count() < MAX_NAME_LENGTH);
    assert!(joined.len() > MAX_NAME_LENGTH); // bytes would have said "too long"

    let long = "a".repeat(251); // + ".": exactly 252 characters
    finish(vec![long], "peer").expect("252 is under the cap");
    let error = finish(vec!["a".repeat(253)], "peer").unwrap_err();
    assert_eq!(
        error,
        format!("DNS name {} exceeds maximum length of 253 from peer", "a".repeat(253) + ".")
    );
}

#[test]
fn i_a_garbage_label_does_not_abort_the_record() {
    crate::pin_test_dir();
    // zeroconf decodes with ("utf-8", "replace"); a device name in Chinese
    // survives as itself and garbage survives as replacement characters.
    //
    // The length is **2**, not 3: with `0x03` the label swallows the terminating
    // zero byte, the walker falls off the end of the packet and both Python and
    // us raise "Corrupt packet" — which is the right answer for that packet but
    // not the claim being tested here.
    let data = [0x02u8, 0xFF, 0xFE, 0x00];
    let (name, end) = decode_name(&data, 0, "peer").expect("lossy decoding still finishes");
    assert_eq!(name, "\u{fffd}\u{fffd}.");
    assert_eq!(end, 4);
    assert_eq!(decode_label(&[0xC3u8, 0xA9]), "é");
}

// ---------------------------------------------------------------------------
// B: message parsing — the three recoveries, and the single masking site
// ---------------------------------------------------------------------------

#[test]
fn j_a_well_formed_datagram_yields_every_section() {
    crate::pin_test_dir();
    let (logger, _path) = witness("mdns_witness_parse_ok");
    let rdata = txt_rdata(&[("fn", Some("Living Room TV"))]);
    let packet = message(
        &[("_googlecast._tcp.local.".to_string(), TYPE_PTR)],
        &[
            rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 4500, &wire_name("Macast-abc._googlecast._tcp.local.")),
            rr(
                "Macast-abc._googlecast._tcp.local.",
                TYPE_TXT,
                CLASS_IN | CLASS_UNIQUE,
                4500,
                &rdata,
            ),
        ],
    );
    let parsed = parse_message(&packet, "192.0.2.1:5353", &logger).expect("this parses");
    assert_eq!(parsed.id, 0);
    assert_eq!(parsed.questions.len(), 1);
    assert_eq!(parsed.questions[0].name, "_googlecast._tcp.local.");
    assert_eq!(parsed.questions[0].rtype, TYPE_PTR);
    assert_eq!(parsed.records.len(), 2);
    // The class field on the wire is 0x8001. Red line: mask it before comparing
    // anything, or SRV/TXT/A match nothing and the device list is empty on a
    // LAN full of Chromecasts while every fixture stays green.
    assert_eq!(parsed.records[1].class, CLASS_IN, "the field never holds 0x8001");
    assert!(parsed.records[1].flush, "the cache-flush bit is parsed apart");
    assert_eq!(parsed.records[0].class, CLASS_IN);
    assert!(!parsed.records[0].flush, "a PTR here arrives without the bit");
    assert_eq!(parsed.records[1].key(), "macast-abc._googlecast._tcp.local.");
    assert_eq!(parsed.records[1].name, "Macast-abc._googlecast._tcp.local.");
    match &parsed.records[1].rdata {
        RData::Txt(text) => assert_eq!(text, &rdata),
        other => panic!("TXT rdata stayed raw bytes, got {:?}", other),
    }
}

#[test]
fn k_the_sections_merge_in_wire_order_not_by_kind() {
    crate::pin_test_dir();
    // A real DNS-SD answer puts the instance PTR in answers and its SRV/TXT/A
    // in additional. A reader that only walked `answers` would find names
    // without addresses, so this hands the A record over as the *third* row and
    // requires the fold to see it.
    let (logger, _path) = witness("mdns_witness_sections");
    let srv = {
        let mut rdata = Vec::new();
        rdata.extend_from_slice(&0u16.to_be_bytes());
        rdata.extend_from_slice(&0u16.to_be_bytes());
        rdata.extend_from_slice(&8009u16.to_be_bytes());
        rdata.extend_from_slice(&wire_name("Macast-abc.local."));
        rdata
    };
    let packet = message(
        &[],
        &[
            rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 120, &wire_name("a._googlecast._tcp.local.")),
            rr("a._googlecast._tcp.local.", TYPE_SRV, CLASS_IN, 120, &srv),
            rr("Macast-abc.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 9]),
        ],
    );
    let parsed = parse_message(&packet, "peer", &logger).expect("three rows");
    assert_eq!(parsed.records.len(), 3);
    assert_eq!(parsed.records[0].rtype, TYPE_PTR);
    assert_eq!(parsed.records[2].rtype, TYPE_A);
    match &parsed.records[2].rdata {
        RData::A(address) => assert_eq!(*address, Ipv4Addr::new(192, 0, 2, 9)),
        other => panic!("A decoded to {:?}", other),
    }
    match &parsed.records[1].rdata {
        RData::Srv { port, target, .. } => {
            assert_eq!(*port, 8009);
            assert_eq!(target, "Macast-abc.local.");
        }
        other => panic!("SRV decoded to {:?}", other),
    }
}

#[test]
fn l_an_oversized_datagram_is_refused_before_a_byte_is_parsed() {
    crate::pin_test_dir();
    // `MAX_MSG_LEN` is an overflow precondition, not a memory guard: it keeps
    // every `offset + length` comparison far from wrapping. It also yields no
    // log line of its own — so the witness file must stay empty.
    let (logger, path) = witness("mdns_witness_max_len");
    let huge = vec![0u8; MAX_MSG_LEN + 1];
    assert!(parse_message(&huge, "peer", &logger).is_none());
    assert_eq!(lines(&path), "", "refusing here is silent by design");
}

#[test]
fn m_a_truncated_header_refuses_the_whole_message() {
    crate::pin_test_dir();
    let (logger, _path) = witness("mdns_witness_short_header");
    // Fewer than the twelve header bytes: nothing was read, so `None` is honest.
    assert!(parse_message(&[0u8, 1, 2, 3], "peer", &logger).is_none());
    assert!(parse_message(&[], "peer", &logger).is_none());
}

#[test]
fn n_a_truncated_question_refuses_the_whole_message() {
    crate::pin_test_dir();
    let (logger, path) = witness("mdns_witness_short_question");
    // Python's `_read_questions` reads the **name first** and only then asks
    // whether `offset + 4` still fits, so a name plus two trailing bytes is
    // exactly the case that raises. The header must genuinely announce the
    // question: `message()` freezes the section counts from the vecs it is
    // handed, so building the bytes by hand behind a `qdcount = 0` header would
    // make the parser read no questions at all and answer `Some` — a green test
    // that never asked the question.
    let mut packet = message(&[("_googlecast._tcp.local.".to_string(), TYPE_PTR)], &[]);
    packet.truncate(packet.len() - 2); // drop the question's class field
    assert_eq!(&packet[4..6], &[0, 1], "the header must still announce one question");
    assert!(parse_message(&packet, "peer", &logger).is_none());
    // `_initial_parse` raises and `__init__` swallows it: no line of ours, no
    // line of Python's, for the same reason as `l_...` above.
    assert_eq!(lines(&path), "", "a refused question is silent by design");
}

#[test]
fn o_recovery_one_keeps_the_records_read_so_far() {
    crate::pin_test_dir();
    // A bad name inside a record header must not empty the message: the prefix
    // is returned and the offset sits back at that record's start.
    let (logger, _path) = witness("mdns_witness_recovery_name");
    let mut packet = message(
        &[],
        &[rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 120, &wire_name("a._googlecast._tcp.local."))],
    );
    let start = packet.len();
    packet.push(0x41); // an unknown compression byte where the name begins
    packet.extend_from_slice(&[0, 0]);
    let parsed = parse_message(&packet, "peer", &logger).expect("the prefix survives");
    assert_eq!(parsed.records.len(), 1);
    let mut parser = Parser {
        data: &packet,
        offset: start,
        source: "peer",
        log: &logger,
    };
    assert!(parser.name().is_err(), "the row really is unreadable");
    assert_eq!(parser.offset, start, "recovery 1 does not advance");
}

#[test]
fn o_a_record_claiming_more_bytes_than_the_packet_holds_stops_the_section() {
    crate::pin_test_dir();
    // The declared length runs off the end. The reader cannot decode it, and it
    // has no way to find the next record either, so the section stops with the
    // prefix kept — and the cursor goes back to where the bad record started,
    // which is the only reason the guard is worth a test of its own: resyncing
    // to the declared `end` would put the cursor past the buffer, so this branch
    // never *gains* a record, it only ever loses the trailing ones.
    let (logger, _path) = witness("mdns_witness_recovery_overrun");
    let first = rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 120, &wire_name("a._googlecast._tcp.local."));
    let mut bad = wire_name("bad.local.");
    bad.extend_from_slice(&TYPE_A.to_be_bytes());
    bad.extend_from_slice(&CLASS_IN.to_be_bytes());
    bad.extend_from_slice(&120u32.to_be_bytes());
    bad.extend_from_slice(&9999u16.to_be_bytes()); // longer than what follows
    bad.extend_from_slice(&[192, 0, 2]);
    // A well-formed record *after* the overrun, to witness "trailing ones lost".
    let last = rr("host.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 9]);
    let packet = message(&[], &[first.clone(), bad.clone(), last.clone()]);
    let start = 12 /* the datagram header */ + first.len();
    let parsed = parse_message(&packet, "peer", &logger).expect("the prefix survives");
    assert_eq!(parsed.records.len(), 1, "the PTR before it; the A after it is gone");
    assert_eq!(parsed.records[0].rtype, TYPE_PTR);
    let mut parser = Parser {
        data: &packet,
        offset: start,
        source: "peer",
        log: &logger,
    };
    let read = parser.read_others(2);
    assert!(read.is_empty(), "the overrun record contributes nothing");
    assert_eq!(parser.offset, start, "and the cursor is where the record started");
}

#[test]
fn p_record_header_hands_back_the_class_unmasked() {
    crate::pin_test_dir();
    // The one place that masks is the push site. This is red line ①'s second
    // witness: if `record_header` masked, the push site's mask would be dead
    // code and the flush bit would have nowhere to come from.
    let (logger, _path) = witness("mdns_witness_header_class");
    let packet = rr("a.local.", TYPE_SRV, CLASS_IN | CLASS_UNIQUE, 120, &[]);
    // `record_header` reads the ten bytes **after** the owner name; the walker
    // that consumed the name is `read_others`. Starting at offset 0 here would
    // read the name's own bytes as a type/class pair (and did: 353, i.e.
    // `0x0161` — the label length and the `a` of `a.local.`).
    let mut parser = Parser {
        data: &packet,
        offset: wire_name("a.local.").len(),
        source: "peer",
        log: &logger,
    };
    let (rtype, class, ttl, length) = parser.record_header().expect("the header reads");
    assert_eq!(rtype, TYPE_SRV);
    assert_eq!(class, 0x8001, "unmasked by design");
    assert_eq!(ttl, 120);
    assert_eq!(length, 0, "the fourth field is the declared rdata length");
    assert_eq!(class & CLASS_MASK, CLASS_IN);
    assert_ne!(class, CLASS_IN, "a comparison against 1 here matches nothing");
}

#[test]
fn p_the_flush_bit_survives_only_at_the_push_site() {
    crate::pin_test_dir();
    // The third witness for the same red line, from the opposite side: a record
    // on the wire with 0x8001 lands in `Vec<Record>` as class 1 plus flush
    // true, and `Record::class` never holds the raw value.
    let (logger, _path) = witness("mdns_witness_flush_push");
    let srv = {
        let mut rdata = Vec::new();
        rdata.extend_from_slice(&1u16.to_be_bytes());
        rdata.extend_from_slice(&2u16.to_be_bytes());
        rdata.extend_from_slice(&8009u16.to_be_bytes());
        rdata.extend_from_slice(&wire_name("host.local."));
        rdata
    };
    let packet = message(&[], &[rr("i._googlecast._tcp.local.", TYPE_SRV, 0x8001, 120, &srv)]);
    let parsed = parse_message(&packet, "peer", &logger).expect("an SRV with the flush bit");
    assert_eq!(parsed.records[0].class, 1);
    assert!(parsed.records[0].flush);
}

#[test]
fn q_an_unknown_record_type_is_skipped_and_the_stream_resyncs() {
    crate::pin_test_dir();
    // HINFO is one of the three types zeroconf *does* decode and this fold does
    // not (the other two are AAAA and NSEC), so it lands on our own line rather
    // than Python's — deliberately worded so nobody reads "we never heard of
    // this type" as "we tried to read it and could not". The offset it names is
    // where the rdata began: that is the whole point of the line, and it is the
    // one number a capture is searched with.
    let (logger, path) = witness("mdns_witness_skip_unknown");
    let _debug = crate::DebugWindow::armed();
    let first = rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 120, &wire_name("a._googlecast._tcp.local."));
    let hinfo = rr("a.local.", 13 /* HINFO */, CLASS_IN, 120, &[0x01, 0x41]);
    let third = rr("host.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 7]);
    let packet = message(&[], &[first.clone(), hinfo.clone(), third.clone()]);
    let parsed = parse_message(&packet, "peer", &logger).expect("an unknown type is skipped");
    assert_eq!(parsed.records.len(), 2, "PTR then A, with HINFO gone");
    assert_eq!(parsed.records[1].rtype, TYPE_A);

    // Header, then the first record whole, then this record's name and its
    // 10-byte fixed tail — the byte the rdata starts on.
    let hinfo_start = 12 + first.len() + wire_name("a.local.").len() + 10;
    assert_eq!(
        hinfo_start,
        12 + first.len() + 9 + 10,
        "the offset is arithmetic on wire lengths, not a number copied from a run"
    );
    let text = lines(&path);
    assert!(
        text.contains(&format!(
            "Skipping unknown record type {} at offset {} from {}",
            13u16, hinfo_start, "peer"
        )),
        "witness was:\n{}",
        text
    );
    assert!(
        !text.contains("Unable to parse"),
        "a type we never claim to read must not borrow the wording of a type \
         we tried to read: \n{}",
        text
    );
}

#[test]
fn q_c_a_known_type_that_fails_to_decode_names_the_domain_and_the_resynced_offset() {
    crate::pin_test_dir();
    // Recovery 2 for a type this fold *does* read. Python prints two record-level
    // DEBUG lines and this is the one keyed on "we tried": it names the owner
    // domain, the numeric type, and the offset **after** the resync to the
    // declared end — so the number is the next record's start, not the byte that
    // went wrong. A fixture with a self-referencing-looking pointer is the
    // cheapest read that fails inside `name()` without also refusing the packet.
    let (logger, path) = witness("mdns_witness_undecoded");
    let _debug = crate::DebugWindow::armed();
    let first = rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 120, &wire_name("a._googlecast._tcp.local."));
    // A PTR whose only rdata is a pointer to offset 0x3FF: past this packet, so
    // the name read refuses, while the declared rdlength still frames the record.
    let bad = rr("b._googlecast._tcp.local.", TYPE_PTR, CLASS_IN, 120, &[0xC0, 0xFF]);
    let third = rr("host.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 9]);
    let packet = message(&[], &[first.clone(), bad.clone(), third.clone()]);
    let parsed = parse_message(&packet, "peer", &logger).expect("the bad PTR is dropped");
    assert_eq!(parsed.records.len(), 2, "the first PTR and the A survive");
    assert_eq!(parsed.records[1].rtype, TYPE_A);

    // The offset Python prints is the declared end of the bad record, i.e. where
    // the *next* record begins.
    let end_of_bad = 12 + first.len() + bad.len();
    let text = lines(&path);
    assert!(
        text.contains(&format!(
            "Unable to parse; skipping record for {} with type {} at offset {}",
            "b._googlecast._tcp.local.",
            TYPE_PTR,
            end_of_bad
        )),
        "witness was:\n{}",
        text
    );
}

#[test]
fn r_a_record_that_overruns_its_rdlength_is_dropped_and_the_offset_jumps_to_end() {
    crate::pin_test_dir();
    // Recovery 3: the decode succeeded but consumed a different number of bytes
    // than the record claimed. The record is dropped (it was built from a slice
    // straddling its own rdata boundary) and the stream resyncs to the declared
    // end, so the *next* record header still lands aligned.
    //
    // The aligned case is the one where the decode stops **short**: a compressed
    // name is two bytes while the sender advertised six. The other direction is
    // physically impossible to recover from — the extra bytes were eaten from
    // the next record, so resyncing to `end` cannot put the cursor back — and
    // that asymmetry is why this test is built the way it is.
    let (logger, path) = witness("mdns_witness_imbalance");
    let _debug = crate::DebugWindow::armed();
    let mut record_bytes = wire_name("i.local.");
    record_bytes.extend_from_slice(&TYPE_PTR.to_be_bytes());
    record_bytes.extend_from_slice(&CLASS_IN.to_be_bytes());
    record_bytes.extend_from_slice(&120u32.to_be_bytes());
    let claimed = 6u16;
    record_bytes.extend_from_slice(&claimed.to_be_bytes());
    // Two bytes of compression pointer at the first record's owner name, then
    // the four bytes this decode never consumes.
    record_bytes.extend_from_slice(&[0xC0, 12]);
    record_bytes.extend_from_slice(&[0, 0, 0, 0]);
    let following = rr("host.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 8]);
    let packet = message(&[], &[record_bytes, following]);
    let parsed = parse_message(&packet, "peer", &logger).expect("the next record is read");
    assert_eq!(parsed.records.len(), 1, "the imbalanced record is dropped");
    assert_eq!(parsed.records[0].rtype, TYPE_A, "and the one after it is aligned");
    let text = lines(&path);
    assert!(
        text.contains(&format!(
            "Record for {} with type {} did not consume exactly rdlength={}; dropping",
            "i.local.",
            TYPE_PTR,
            claimed
        )),
        "witness was:\n{}",
        text
    );
    assert!(
        !text.contains(" from peer"),
        "Python has the source on its neighbour line and not on this one; the \
         asymmetry is copied, not fixed: \n{}",
        text
    );
}

#[test]
fn s_a_short_a_rdata_and_a_short_srv_rdata_land_in_the_same_none() {
    crate::pin_test_dir();
    // "the difference between 'we did not decode this' and 'the sender sent
    // nothing' never appears in the record list" — `Some(None)` is not a shape
    // here, so both short bodies are simply not in `Vec<Record>`.
    let (logger, _path) = witness("mdns_witness_short_rdata");
    let packet = message(
        &[],
        &[
            rr("a.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2]), // 3 of 4 bytes
            rr("b.local.", TYPE_SRV, CLASS_IN, 120, &[0, 0, 0, 0, 0x1f, 0x40]), // 6 of 7+
            rr("c.local.", TYPE_TXT, CLASS_IN, 120, &txt_rdata(&[("fn", Some("x"))])),
        ],
    );
    let parsed = parse_message(&packet, "peer", &logger).expect("the third record survives");
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(parsed.records[0].rtype, TYPE_TXT);
}

#[test]
fn t_cname_shares_the_ptr_decode_and_the_fold_ignores_it() {
    crate::pin_test_dir();
    // PTR and CNAME share one decode so the reader's offset stays exactly where
    // zeroconf's would be. The fold only ever reads PTRs, so a CNAME decodes
    // into `RData::Name` and then changes nothing.
    let (logger, _path) = witness("mdns_witness_cname");
    let packet = message(
        &[],
        &[
            rr("alias.local.", TYPE_CNAME, CLASS_IN, 120, &wire_name("real.local.")),
            rr("real.local.", TYPE_A, CLASS_IN, 120, &[192, 0, 2, 3]),
        ],
    );
    let parsed = parse_message(&packet, "peer", &logger).expect("a CNAME parses");
    match &parsed.records[0].rdata {
        RData::Name(target) => assert_eq!(target, "real.local."),
        other => panic!("CNAME shares the PTR decode, got {:?}", other),
    }
    let mut ledger = Ledger::new(SERVICE_TYPE);
    let questions = ledger.ingest(&parsed.records);
    assert!(questions.is_empty(), "a CNAME asks nothing");
    assert!(ledger.devices().is_empty());
    assert!(ledger.ptrs.is_empty(), "it is not folded into the PTR list");
}

// ---------------------------------------------------------------------------
// C: TXT — the three shapes the fold reads
// ---------------------------------------------------------------------------

#[test]
fn u_a_bare_key_is_none_and_an_empty_value_is_some_empty() {
    crate::pin_test_dir();
    // RFC 6763 §6.4 distinguishes the two by the separator, not the value, and
    // it matters here because the name is picked with
    // `properties.get('fn') or info.server or name` — a bare `fn` must fall
    // through, and `fn=` must fall through too, for different reasons.
    let bare = txt_pairs(&txt_rdata(&[("fn", None)]));
    assert_eq!(bare.len(), 1);
    assert_eq!(bare[0].0, b"fn".to_vec());
    assert_eq!(bare[0].1, None);
    assert!(txt_value(&bare, "fn").is_none(), "no separator, no value");

    let empty = txt_rdata(&[("fn", Some(""))]);
    let pairs = txt_pairs(&empty);
    assert_eq!(pairs[0].1, Some(b"".to_vec()));
    assert_eq!(
        txt_value(&pairs, "fn"),
        Some(String::new()),
        "the caller's `or` chain has to see the emptiness to choose past it"
    );
    assert_eq!(txt_value(&pairs, "cn"), None, "an absent key answers None");
}

#[test]
fn v_the_first_duplicate_key_wins() {
    crate::pin_test_dir();
    let text = txt_rdata(&[("fn", Some("first")), ("fn", Some("second"))]);
    let pairs = txt_pairs(&text);
    assert_eq!(pairs.len(), 1, "a repeated key is not a second property");
    assert_eq!(txt_value(&pairs, "fn"), Some("first".to_string()));
}

#[test]
fn w_a_length_byte_past_the_end_truncates_silently() {
    crate::pin_test_dir();
    // Python slices `text[index:index+length]` with no bounds check, so this is
    // a silent truncation, not a raise — and the loop still terminates because
    // `index += length` is unclamped while `index` runs past `text.len()`.
    let mut text = vec![0x10u8]; // claims 16 bytes
    text.extend_from_slice(b"fn=short");
    let pairs = txt_pairs(&text);
    assert_eq!(pairs.len(), 1);
    assert_eq!(txt_value(&pairs, "fn"), Some("short".to_string()));
    assert_eq!(txt_pairs(&[]).len(), 0, "an empty TXT is no pairs, not a refusal");
}

#[test]
fn x_a_non_utf8_value_becomes_replacement_characters() {
    crate::pin_test_dir();
    let mut text = Vec::new();
    text.push(0x04u8);
    text.extend_from_slice(b"fn=");
    text.push(0xFF);
    let pairs = txt_pairs(&text);
    let value = txt_value(&pairs, "fn").expect("a value is there even when undecodable");
    assert_eq!(value, "\u{fffd}");
}

// ---------------------------------------------------------------------------
// D: the fold — keying, goodbyes, and the address order
// ---------------------------------------------------------------------------

#[test]
fn y_ledgers_are_keyed_lowercase_and_output_keeps_the_spelling() {
    crate::pin_test_dir();
    // An SRV target of `Macast-abc.LOCAL.` with an A owner of
    // `macast-abc.local.` is one device in Python and two here if we forget.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("Macast-A._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "Macast-A._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "Macast-abc.LOCAL.".into(),
        },
    )]);
    ledger.ingest(&[record(
        "macast-abc.local.",
        TYPE_A,
        120,
        RData::A(Ipv4Addr::new(192, 0, 2, 1)),
    )]);
    assert!(ledger.ptrs.contains_key(SERVICE_TYPE.to_lowercase().as_str()));
    assert!(ledger.instances.contains_key("macast-a._googlecast._tcp.local."));
    assert!(ledger.addresses.contains_key("macast-abc.local."));
    let devices = ledger.devices();
    assert_eq!(devices.len(), 1, "two spellings, one device");
    assert_eq!(devices[0].address, Ipv4Addr::new(192, 0, 2, 1).to_string());
    assert_eq!(devices[0].port, 8009);
    // No TXT at all: `info.server` answers, which is Python's **second** slot —
    // the PTR's rdata only gets a vote when there is no usable server either
    // (`ak`). This is where the spelling question bites: the ledger keyed that
    // name lowercased, the display must still hand back `Macast-abc.LOCAL.`.
    assert_eq!(devices[0].name, "Macast-abc.LOCAL.");
}

#[test]
fn z_a_ptr_goodbye_before_the_dedup_check_tears_the_instance_down() {
    crate::pin_test_dir();
    // The first draft tested dedup before the goodbye, so a device's own
    // goodbye — arriving under a spelling the ledger already holds — was
    // swallowed by "already listed" and the instance was never removed. Both
    // failure shapes are pinned here.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 2)))]);
    assert_eq!(ledger.devices().len(), 1);

    // Same spelling, ttl 0: the dedup branch would have skipped it.
    let questions = ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        0,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert!(questions.is_empty());
    assert!(ledger.devices().is_empty(), "the goodbye has to bite");
    assert!(ledger.instances.is_empty());
    assert_eq!(ledger.ptrs.get(SERVICE_TYPE), None, "an empty list removes its key");
    assert_eq!(
        ledger.addresses.get("a.local."),
        None,
        "an unreferenced server's addresses go with it"
    );
}

#[test]
fn aa_a_ptr_goodbye_under_a_different_type_leaves_the_instance_alone() {
    crate::pin_test_dir();
    // A `_airplay._tcp` PTR naming the same host says nothing about whether the
    // GoogleCast instance still answers. The name always leaves *that* type's
    // list; the teardown is only for our own type.
    let other = "_airplay._tcp.local.";
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 3)))]);
    ledger.ingest(&[record(
        other,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert!(ledger.ptrs.contains_key(other));

    ledger.ingest(&[record(
        other,
        TYPE_PTR,
        0,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert_eq!(ledger.ptrs.get(other).map(|list| list.len()), None);
    assert_eq!(ledger.devices().len(), 1, "the GoogleCast instance survives");
    assert!(ledger.addresses.contains_key("a.local."));
}

#[test]
fn ab_a_txt_goodbye_removes_the_instance_but_never_the_address_list() {
    crate::pin_test_dir();
    // The asymmetry is deliberate: an SRV/TXT goodbye says "this instance stops
    // announcing", not "this host is gone" — another instance may still point
    // at the same server.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", Some("TV"))])),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 4)))]);
    assert_eq!(ledger.devices().len(), 1);

    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        0,
        RData::Txt(Vec::new()),
    )]);
    assert!(ledger.instances.is_empty());
    assert!(
        ledger.addresses.contains_key("a.local."),
        "an A list is not this goodbye's to remove"
    );
    assert!(ledger.devices().is_empty(), "no instance, so no device");
}

#[test]
fn ac_an_a_goodbye_empties_the_list_without_removing_the_key() {
    crate::pin_test_dir();
    // `retain` on the list, and the map key stays. Which is why `devices()`
    // asks for a *non-empty* list rather than merely a present one — an empty
    // `Vec` under a live key must drop the device.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 5)))]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 6)))]);
    assert_eq!(ledger.devices().len(), 1);

    ledger.ingest(&[record("a.local.", TYPE_A, 0, RData::A(Ipv4Addr::new(192, 0, 2, 5)))]);
    assert_eq!(ledger.addresses["a.local."].len(), 1);
    assert_eq!(
        ledger.devices().len(),
        1,
        "one goodbye is not the instance leaving: the other address still answers"
    );
    // ... two of them are: with the list empty the key still exists.
    ledger.ingest(&[record("a.local.", TYPE_A, 0, RData::A(Ipv4Addr::new(192, 0, 2, 6)))]);
    assert!(ledger.addresses.contains_key("a.local."), "the key is never removed");
    assert!(ledger.addresses["a.local."].is_empty());
    assert!(ledger.devices().is_empty());
}

#[test]
fn ad_the_fold_opens_the_last_distinct_address_not_the_highest() {
    crate::pin_test_dir();
    // Python reverses a dict in insertion order, and `DNSAddress.__eq__`
    // ignores the TTL, so re-observation never moves an entry. A set would
    // order by address bytes and report the *highest* address of a dual-homed
    // device — a defensible guess and the wrong answer.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 50)))]);
    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(10, 0, 0, 9)))]);
    assert_eq!(
        ledger.addresses["a.local."],
        vec![Ipv4Addr::new(192, 0, 2, 50), Ipv4Addr::new(10, 0, 0, 9)],
        "insertion order, not sorted"
    );
    let devices = ledger.devices();
    assert_eq!(devices[0].address, Ipv4Addr::new(10, 0, 0, 9).to_string(), "the last one");

    // Re-observing the *first* address with a fresh TTL does not move it.
    ledger.ingest(&[record("a.local.", TYPE_A, 30, RData::A(Ipv4Addr::new(192, 0, 2, 50)))]);
    assert_eq!(
        ledger.addresses["a.local."].len(),
        2,
        "the pop-then-append branch can never fire"
    );
    assert_eq!(
        ledger.addresses["a.local."],
        vec![Ipv4Addr::new(192, 0, 2, 50), Ipv4Addr::new(10, 0, 0, 9)]
    );
}

#[test]
fn ae_a_record_the_class_check_rejects_is_not_folded_at_all() {
    crate::pin_test_dir();
    // `_dns_entry_matches` compares class before it compares rdata, so a CH/A
    // record of a type we *can* decode still changes nothing. The compare also
    // happens before the rdata match, which is why a class-CH TXT never reaches
    // the instance map.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    let questions = ledger.ingest(&[
        raw_record(SERVICE_TYPE, TYPE_PTR, 0x8003 /* CH | flush */, 4500, RData::Name("a.x.local.".into())),
        raw_record("a.x.local.", TYPE_TXT, 3 /* CH */, 4500, RData::Txt(txt_rdata(&[("fn", Some("no"))]))),
        raw_record("a.x.local.", TYPE_A, 0x7FFF, 4500, RData::A(Ipv4Addr::new(192, 0, 2, 1))),
    ]);
    assert!(questions.is_empty());
    assert!(ledger.ptrs.is_empty());
    assert!(ledger.instances.is_empty());
    assert!(ledger.addresses.is_empty());
    assert!(ledger.devices().is_empty());
}

#[test]
fn af_ingest_asks_for_srv_and_txt_once_per_new_instance() {
    crate::pin_test_dir();
    // The PTR for the service type once, then SRV and TXT per newly seen
    // instance, then A per newly learned server. A second identical PTR must
    // not open a second pair of questions.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    let first = ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].rtype, TYPE_SRV);
    assert_eq!(first[0].name, "a._googlecast._tcp.local.");
    assert_eq!(first[1].rtype, TYPE_TXT);

    let second = ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert!(second.is_empty(), "already listed, nothing to ask");

    // A different spelling of the same name is also already listed.
    let third = ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("A._GOOGLECAST._TCP.LOCAL.".into()),
    )]);
    assert!(third.is_empty());
    assert_eq!(
        ledger.ptrs[SERVICE_TYPE].len(),
        1,
        "the first target's spelling is the one kept"
    );
    assert_eq!(ledger.ptrs[SERVICE_TYPE][0], "a._googlecast._tcp.local.");

    // A gains its own question once the SRV names a server the ledger lacks.
    let srv = ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    assert_eq!(srv.len(), 1);
    assert_eq!(srv[0].rtype, TYPE_A);
    assert_eq!(srv[0].name, "a.local.");
    assert_eq!(srv[0].name, "a.local.", "the target's own spelling rides along");
    let again = ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "A.LOCAL.".into(),
        },
    )]);
    assert!(again.is_empty(), "fresh_server compares lowercased");
}

#[test]
fn ag_a_ptr_under_another_service_type_is_recorded_but_asks_nothing() {
    crate::pin_test_dir();
    // `if key != self.service_type { continue; }` sits *after* the push, so the
    // name is still remembered (a goodbye for it has to find the list) while no
    // SRV/TXT question is generated for a service we are not browsing.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    let questions = ledger.ingest(&[record(
        "_airplay._tcp.local.",
        TYPE_PTR,
        4500,
        RData::Name("a._airplay._tcp.local.".into()),
    )]);
    assert!(questions.is_empty());
    assert_eq!(ledger.ptrs["_airplay._tcp.local."].len(), 1);
    assert!(ledger.instances.is_empty());
    assert_eq!(ledger.service_type, SERVICE_TYPE);
}

// ---------------------------------------------------------------------------
// E: devices() — the requirement, the divergences, and the sort
// ---------------------------------------------------------------------------

#[test]
fn ah_devices_require_srv_and_at_least_one_a_and_not_txt() {
    crate::pin_test_dir();
    // Declared divergence #1: TXT is not required, where zeroconf's
    // `_is_complete` asks for PTR+SRV+TXT+A. This goes in the direction that
    // keeps devices. #2: IPv4 only, where Python passes `IPVersion.All` —
    // "listing a device we cannot route to is the worse failure".
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    assert!(ledger.devices().is_empty(), "a name with no SRV is not a target");

    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", Some("TV"))])),
    )]);
    assert!(ledger.devices().is_empty(), "TXT without SRV still has no port");

    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    assert!(ledger.devices().is_empty(), "no address yet");
    assert_eq!(ledger.devices().len(), 0);

    ledger.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 9)))]);
    let devices = ledger.devices();
    assert_eq!(devices.len(), 1, "SRV + A is enough; no TXT was ever needed");
    assert_eq!(devices[0].port, 8009);
}

#[test]
fn ai_no_service_ptr_means_an_empty_list_not_an_error() {
    crate::pin_test_dir();
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        "a.local.",
        TYPE_A,
        120,
        RData::A(Ipv4Addr::new(192, 0, 2, 9)),
    )]);
    assert!(ledger.devices().is_empty());
}

#[test]
fn aj_the_display_name_walks_fn_then_server_then_the_ptr_target() {
    crate::pin_test_dir();
    // `cast_local_file.py:1037`: `properties.get('fn') or info.server or name`.
    // The first two slots are here; the third only answers when the server is
    // unusable, which is `ak`'s single-dot fixture.
    let mut with_fn = Ledger::new(SERVICE_TYPE);
    with_fn.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    with_fn.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: "a.local.".into(),
        },
    )]);
    with_fn.ingest(&[record("a.local.", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 1)))]);
    assert_eq!(
        with_fn.devices()[0].name,
        "a.local.",
        "no TXT at all: `info.server` answers, which is the second slot, not the third"
    );

    // A bare `fn` (no separator) is `None`, so it also falls through.
    with_fn.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", None), ("cn", Some("TV"))])),
    )]);
    assert_eq!(
        with_fn.devices()[0].name,
        "a.local.",
        "the SRV target answers when TXT says nothing"
    );

    // `fn=` is `Some("")` — present, empty, and still chosen past by the `or`.
    with_fn.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", Some(""))])),
    )]);
    assert_eq!(with_fn.devices()[0].name, "a.local.");

    // A real `fn` wins, and a Chinese name survives as itself.
    with_fn.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", Some("客厅电视"))])),
    )]);
    assert_eq!(with_fn.devices()[0].name, "客厅电视");
}

#[test]
fn ak_a_server_of_a_single_dot_falls_through_to_the_instance_name() {
    crate::pin_test_dir();
    // `device_name` keeps `server != "."`: a root target is "no target", and
    // `.` is exactly what an unreadable SRV rdata would leave behind.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    ledger.ingest(&[record(
        SERVICE_TYPE,
        TYPE_PTR,
        4500,
        RData::Name("a._googlecast._tcp.local.".into()),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_TXT,
        4500,
        RData::Txt(txt_rdata(&[("fn", None)])),
    )]);
    ledger.ingest(&[record(
        "a._googlecast._tcp.local.",
        TYPE_SRV,
        4500,
        RData::Srv {
            priority: 0,
            weight: 0,
            port: 8009,
            target: ".".into(),
        },
    )]);
    ledger.ingest(&[record(".", TYPE_A, 120, RData::A(Ipv4Addr::new(192, 0, 2, 8)))]);
    let devices = ledger.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "a._googlecast._tcp.local.");
    assert_eq!(devices[0].address, Ipv4Addr::new(192, 0, 2, 8).to_string());
}

#[test]
fn al_the_device_list_sorts_like_python_s_tuple() {
    crate::pin_test_dir();
    // Python builds `[(friendly name, host, port)]` and `sorted()`s it, so the
    // order is name, then address, then port — which is exactly the field order
    // `CastDevice` derives `Ord` over.
    let mut ledger = Ledger::new(SERVICE_TYPE);
    for (index, (name, address)) in [
        ("Zeta", Ipv4Addr::new(192, 0, 2, 1)),
        ("alpha", Ipv4Addr::new(192, 0, 2, 2)),
        ("alpha", Ipv4Addr::new(10, 0, 0, 1)),
    ]
    .iter()
    .enumerate()
    {
        let instance = format!("i{}._googlecast._tcp.local.", index);
        let server = format!("s{}.local.", index);
        ledger.ingest(&[record(
            SERVICE_TYPE,
            TYPE_PTR,
            4500,
            RData::Name(instance.clone()),
        )]);
        ledger.ingest(&[record(
            &instance,
            TYPE_TXT,
            4500,
            RData::Txt(txt_rdata(&[("fn", Some(name))])),
        )]);
        ledger.ingest(&[record(
            &instance,
            TYPE_SRV,
            4500,
            RData::Srv {
                priority: 0,
                weight: 0,
                port: 8009,
                target: server.clone(),
            },
        )]);
        ledger.ingest(&[record(&server, TYPE_A, 120, RData::A(*address))]);
    }
    let devices = ledger.devices();
    assert_eq!(devices.len(), 3);
    assert_eq!(devices[0].name, "Zeta", "capital Z sorts before lowercase a");
    assert_eq!(devices[1].address, Ipv4Addr::new(10, 0, 0, 1).to_string(), "then by address");
    assert_eq!(devices[2].address, Ipv4Addr::new(192, 0, 2, 2).to_string());
    assert_eq!(devices[1].name, "alpha");
    assert_eq!(devices[2].name, "alpha");
}

// ---------------------------------------------------------------------------
// F: drop_ours — the filter that degrades to no filter
// ---------------------------------------------------------------------------

#[test]
fn m_a_b_drop_ours_reads_the_address_only() {
    crate::pin_test_dir();
    let devices = vec![
        CastDevice {
            name: "TV".into(),
            address: Ipv4Addr::new(192, 0, 2, 1).to_string(),
            port: 8009,
        },
        CastDevice {
            name: "This Mac".into(),
            address: Ipv4Addr::new(192, 0, 2, 2).to_string(),
            port: 8009,
        },
    ];
    let ours = vec!["192.0.2.2".to_string()];
    let kept = drop_ours(&devices, &ours);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name, "TV", "the name is never consulted");
    assert_eq!(kept[0].address, Ipv4Addr::new(192, 0, 2, 1).to_string());

    // A name matching `ours` is not enough — the compare is on the address.
    let by_name = vec!["TV".to_string()];
    assert_eq!(drop_ours(&devices, &by_name).len(), 2);

    // Port plays no part either: our own cast on another port is still ours.
    let other_port = vec![CastDevice {
        name: "This Mac".into(),
        address: Ipv4Addr::new(192, 0, 2, 2).to_string(),
        port: 8008,
    }];
    assert!(drop_ours(&other_port, &ours).is_empty());
}

#[test]
fn n_b_empty_ours_means_no_filter_at_all() {
    crate::pin_test_dir();
    // The demanded test. `ours = set()` is the `except Exception` branch in
    // Python, and there "a device list with one useless extra entry beats a
    // device list that silently explains nothing".
    let devices = vec![CastDevice {
        name: "TV".into(),
        address: Ipv4Addr::new(192, 0, 2, 1).to_string(),
        port: 8009,
    }];
    assert_eq!(drop_ours(&devices, &[]).len(), 1);
    assert_eq!(drop_ours(&devices, &["".to_string()]).len(), 1);
    assert_eq!(drop_ours(&devices, &["192.0.2.99".to_string()]).len(), 1);
}

// ---------------------------------------------------------------------------
// G: query encoding — refusal, not truncation
// ---------------------------------------------------------------------------

#[test]
fn o_b_a_query_is_id_zero_with_one_question_and_no_answers() {
    crate::pin_test_dir();
    // Send id 0 and match nothing: the fold's first pass is a full browse, not
    // a targeted query, so the answers it wants include unsolicited
    // announcements. Matching responses to questions by id would drop exactly
    // those, and RFC 6762 §10 permits ingesting them.
    let bytes = query(SERVICE_TYPE, TYPE_PTR).expect("a legal name encodes");
    let mut expect = Vec::new();
    expect.extend_from_slice(&0u16.to_be_bytes()); // id
    expect.extend_from_slice(&0u16.to_be_bytes()); // flags (QR=0)
    expect.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    expect.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    expect.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    expect.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    expect.extend_from_slice(&wire_name(SERVICE_TYPE));
    expect.extend_from_slice(&TYPE_PTR.to_be_bytes());
    expect.extend_from_slice(&CLASS_IN.to_be_bytes());
    assert_eq!(bytes, expect);
    assert!(
        !bytes.contains(&0xC0),
        "labels are never compressed on output"
    );
}

#[test]
fn p_b_the_writer_refuses_a_label_the_wire_cannot_carry() {
    crate::pin_test_dir();
    // The first draft clamped with `bytes.len().min(63)`. That comes back as the
    // one thing worse than an error: an answer about a name nobody asked for,
    // and the instance never resolves. The reader accepts what the wire can
    // express; the writer refuses to emit what it cannot.
    let sixty_three = "a".repeat(63);
    query(&format!("{}.local.", sixty_three), TYPE_A).expect("63 is expressible");

    let sixty_four = "a".repeat(64);
    let error = query(&format!("{}.local.", sixty_four), TYPE_A).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "mDNS label {:?} is {} bytes, over the {} a wire name allows",
            sixty_four,
            64,
            MAX_LABEL_LENGTH
        )
    );

    // The unit is bytes, not characters: 64 Chinese characters are 192 bytes.
    let wide = "中".repeat(64);
    let error = query(&wide, TYPE_A).unwrap_err();
    assert!(
        error.to_string().contains(" is 192 bytes,"),
        "the refusal counts bytes, got {}",
        error
    );
}

#[test]
fn q_b_a_name_with_no_labels_is_refused_not_the_root() {
    crate::pin_test_dir();
    // Empty labels are dropped, so `""` and `"."` both encode as the root — and
    // a query for the root is a question about nothing.
    let error = query("", TYPE_PTR).unwrap_err();
    assert_eq!(error.to_string(), "mDNS query name \"\" has no labels");
    let error = query(".", TYPE_PTR).unwrap_err();
    assert_eq!(error.to_string(), "mDNS query name \".\" has no labels");
    let bytes = query("..a.local..", TYPE_PTR).expect("empty labels are dropped");
    assert_eq!(
        bytes[12..].to_vec(),
        vec![
            1,
            b'a',
            5,
            b'l',
            b'o',
            b'c',
            b'a',
            b'l',
            0,
            (TYPE_PTR >> 8) as u8,
            (TYPE_PTR & 0xFF) as u8,
            (CLASS_IN >> 8) as u8,
            (CLASS_IN & 0xFF) as u8,
        ],
        "one label, then the terminator, then qtype/qclass"
    );
}

// ---------------------------------------------------------------------------
// H: the browse loop over a fake wire
// ---------------------------------------------------------------------------

/// A wire that records every datagram it is asked to send and replays a script.
/// `browse` is tested against this because the fold, the ledger, the retransmit
/// policy and the timeout are all testable without a LAN — which is the module's
/// own claim, and a fake is the only shape that can hand back a scripted
/// sequence on a deadline.
struct FakeWire {
    sent: Vec<Vec<u8>>,
    incoming: VecDeque<Option<Vec<u8>>>,
    fail_send: Option<String>,
    fail_recv: Option<String>,
    source: String,
    reads: usize,
}

impl FakeWire {
    fn new(incoming: Vec<Option<Vec<u8>>>) -> Self {
        FakeWire {
            sent: Vec::new(),
            incoming: VecDeque::from(incoming),
            fail_send: None,
            fail_recv: None,
            source: "192.0.2.53:5353".to_string(),
            reads: 0,
        }
    }

    fn with_source(mut self, source: &str) -> Self {
        self.source = source.to_string();
        self
    }

    fn failing(mut self, why: &str) -> Self {
        self.fail_send = Some(why.to_string());
        self
    }

    /// Once the script is exhausted the next read fails instead of timing out.
    fn failing_read(mut self, why: &str) -> Self {
        self.fail_recv = Some(why.to_string());
        self
    }
}

impl Wire for FakeWire {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &self.fail_send {
            Some(why) => Err(io::Error::new(io::ErrorKind::Other, why.clone())),
            None => {
                self.sent.push(bytes.to_vec());
                Ok(())
            }
        }
    }

    fn recv(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.reads += 1;
        match self.incoming.pop_front() {
            Some(Some(data)) => Ok(Some(data)),
            Some(None) => Ok(None),
            None => match &self.fail_recv {
                Some(why) => Err(io::Error::new(io::ErrorKind::Other, why.clone())),
                None => Ok(None),
            },
        }
    }

    fn source(&self) -> String {
        self.source.clone()
    }
}

/// A complete, resolvable announcement for one device, as three records.
fn announcement(instance: &str, server: &str, address: [u8; 4]) -> Vec<Vec<u8>> {
    let srv = {
        let mut rdata = Vec::new();
        rdata.extend_from_slice(&0u16.to_be_bytes());
        rdata.extend_from_slice(&0u16.to_be_bytes());
        rdata.extend_from_slice(&8009u16.to_be_bytes());
        rdata.extend_from_slice(&wire_name(server));
        rdata
    };
    vec![
        rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 4500, &wire_name(instance)),
        rr(
            instance,
            TYPE_TXT,
            CLASS_IN | CLASS_UNIQUE,
            4500,
            &txt_rdata(&[("fn", Some("Kitchen TV"))]),
        ),
        rr(instance, TYPE_SRV, CLASS_IN | CLASS_UNIQUE, 4500, &srv),
        rr(server, TYPE_A, CLASS_IN | CLASS_UNIQUE, 120, &address.to_vec()),
    ]
}

#[test]
fn r_b_browse_returns_a_device_from_one_unsolicited_announcement() {
    crate::pin_test_dir();
    let (logger, _path) = witness("mdns_witness_browse_ok");
    let packet = message(&[], &announcement("k._googlecast._tcp.local.", "k.local.", [192, 0, 2, 40]));
    let mut wire = FakeWire::new(vec![Some(packet)]);
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Kitchen TV");
    assert_eq!(devices[0].address, Ipv4Addr::new(192, 0, 2, 40).to_string());
    assert_eq!(devices[0].port, 8009);
    // The very first thing out of the door is the PTR question for the type.
    assert_eq!(wire.sent[0], query(SERVICE_TYPE, TYPE_PTR).expect("the type encodes"));
}

#[test]
fn s_b_the_asked_ledger_is_the_anti_storm_guard() {
    crate::pin_test_dir();
    // Every unanswered question is a re-transmitted multicast, so `asked` holds
    // one entry per (name, type) keyed case-insensitively for the whole browse.
    let (logger, _path) = witness("mdns_witness_browse_ledger");
    let packet = message(&[], &announcement("k._googlecast._tcp.local.", "k.local.", [192, 0, 2, 41]));
    // The same announcement three times, in three spellings of the instance.
    let mut upper = message(
        &[],
        &announcement("K._GOOGLECAST._TCP.LOCAL.", "k.local.", [192, 0, 2, 41]),
    );
    let mut again = packet.clone();
    std::mem::swap(&mut upper, &mut again); // keep both fixtures live
    let mut wire = FakeWire::new(vec![Some(packet), Some(upper), Some(again)]);
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert_eq!(devices.len(), 1, "three datagrams, one device");
    // One PTR for the type, then SRV+TXT for the instance, then A for the
    // server — and never a second of any of them.
    let ptrs = wire
        .sent
        .iter()
        .filter(|bytes| {
            bytes.len() > 12 && bytes[12] == 11 && bytes[13] == b'_'
        })
        .count();
    assert_eq!(ptrs, 1, "the service PTR is asked once per browse");
    assert!(wire.sent.len() <= 4, "no query storm: {:?}", wire.sent.len());
    let _ = again;
}

#[test]
fn t_b_a_failed_send_is_never_retried() {
    crate::pin_test_dir();
    // `asked.push(key)` happens *before* the send. Retrying-on-failure would be
    // the one behaviour the reference never had, and it would convert a single
    // ENOBUFS into a question storm for the rest of the budget.
    let (logger, path) = witness("mdns_witness_browse_send_fail");
    let mut wire = FakeWire::new(vec![Some(message(&[], &[]))]).failing("No buffer space available (os error 105)");
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert!(devices.is_empty());
    assert!(wire.sent.is_empty(), "nothing ever left the socket");
    let text = lines(&path);
    assert!(
        text.contains("mDNS query send failed: No buffer space available (os error 105)"),
        "witness was:\n{}",
        text
    );
    // The loop keeps going after a failed send: one datagram, one read of it.
    assert!(wire.reads >= 2, "a send failure does not end the browse");
}

#[test]
fn u_b_an_unencodable_question_stays_asked() {
    crate::pin_test_dir();
    // The key goes into `asked` before the encode, so a name we cannot write is
    // asked about exactly once — the ledger, not a retry, is the policy.
    let (logger, path) = witness("mdns_witness_browse_encode_fail");
    let bad = "a".repeat(64);
    let mut wire = FakeWire::new(vec![Some(message(&[], &[]))]);
    // Drive `ask` directly: browse only ever asks for names the fold produced,
    // and this is the branch that needs a witness.
    let mut asked: Vec<(String, u16)> = Vec::new();
    let question = Question {
        name: format!("{}.local.", bad),
        rtype: TYPE_A,
    };
    ask(&mut wire, &mut asked, &question, &logger);
    assert_eq!(asked.len(), 1, "the key stays in the ledger");
    assert!(wire.sent.is_empty());
    let text = lines(&path);
    assert!(
        text.contains("mDNS query could not be encoded: mDNS label "),
        "witness was:\n{}",
        text
    );

    // A second ask for the same name changes nothing at all.
    ask(&mut wire, &mut asked, &question, &logger);
    assert_eq!(asked.len(), 1);
    assert_eq!(wire.reads, 0);
}

#[test]
fn v_b_an_unparsable_datagram_is_reported_with_its_size_and_peer() {
    crate::pin_test_dir();
    let (logger, path) = witness("mdns_witness_browse_unparsed");
    let mut wire = FakeWire::new(vec![Some(vec![0u8; 7])]).with_source("198.51.100.9:5353");
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert!(devices.is_empty());
    let text = lines(&path);
    assert!(
        text.contains("ignored an mDNS datagram that did not parse: 7 bytes from 198.51.100.9:5353"),
        "witness was:\n{}",
        text
    );
}

#[test]
fn w_b_a_read_error_ends_the_browse_and_keeps_what_was_heard() {
    crate::pin_test_dir();
    // `Err` is not `Ok(None)`: a real read failure stops the poll, and the
    // devices learned before it are still the answer.
    let (logger, path) = witness("mdns_witness_browse_read_error");
    let good = message(&[], &announcement("g._googlecast._tcp.local.", "g.local.", [192, 0, 2, 42]));
    let mut wire = FakeWire::new(vec![Some(good)])
        .with_source("192.0.2.42:5353")
        .failing_read("no such device");
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert_eq!(devices.len(), 1, "what was already heard is not lost");
    assert_eq!(devices[0].address, Ipv4Addr::new(192, 0, 2, 42).to_string());
    // The poll stopped rather than running out its 40 ms: one read for the
    // announcement, one for the failure, and nothing after that. Without the
    // break arm the fake keeps answering `Ok(None)` until the deadline, so this
    // is the half of the contract that says the browse *ended*.
    assert_eq!(wire.reads, 2, "the failure ends the loop, not the clock");
    let text = lines(&path);
    assert!(
        text.contains("Chromecast discovery failed: no such device"),
        "witness was:\n{}",
        text
    );
}

#[test]
fn w_c_a_clean_poll_never_logs_a_discovery_failure() {
    crate::pin_test_dir();
    // The other direction of the same arm: `Ok(None)` is a timeout, so a browse
    // that only ever times out must stay silent. Without this pairing the
    // assertion above could be satisfied by an ERROR line that is written on
    // every poll.
    let (logger, path) = witness("mdns_witness_browse_clean");
    let good = message(&[], &announcement("g._googlecast._tcp.local.", "g.local.", [192, 0, 2, 42]));
    let mut wire = FakeWire::new(vec![Some(good)]).with_source("192.0.2.42:5353");
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert_eq!(devices.len(), 1);
    let text = lines(&path);
    assert!(
        !text.contains("Chromecast discovery failed"),
        "a clean poll must not log a failure: {}",
        text
    );
}

#[test]
fn x_b_a_zero_timeout_still_asks_the_first_question() {
    crate::pin_test_dir();
    // `ask` runs before the deadline is computed, so a zero-length browse sends
    // exactly one PTR query and then breaks at the top of the loop.
    let (logger, _path) = witness("mdns_witness_browse_zero");
    let mut wire = FakeWire::new(Vec::new());
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::ZERO, &logger);
    assert!(devices.is_empty());
    assert_eq!(wire.sent.len(), 1);
    assert_eq!(wire.sent[0], query(SERVICE_TYPE, TYPE_PTR).expect("encodes"));
    assert_eq!(wire.reads, 0, "the deadline is checked before the first read");
}

#[test]
fn y_b_a_goodbye_in_the_stream_removes_the_device() {
    crate::pin_test_dir();
    let (logger, _path) = witness("mdns_witness_browse_goodbye");
    let hello = message(&[], &announcement("h._googlecast._tcp.local.", "h.local.", [192, 0, 2, 43]));
    let bye = message(
        &[],
        &[rr(SERVICE_TYPE, TYPE_PTR, CLASS_IN, 0, &wire_name("h._googlecast._tcp.local."))],
    );
    let mut wire = FakeWire::new(vec![Some(hello), Some(bye)]);
    let devices = browse(&mut wire, SERVICE_TYPE, Duration::from_millis(40), &logger);
    assert!(devices.is_empty(), "the fold answers with what still answers");
}

// ---------------------------------------------------------------------------
// I: the socket helpers that need no bind
// ---------------------------------------------------------------------------

#[test]
fn z_c_a_non_ipv4_interface_is_skipped_and_named() {
    crate::pin_test_dir();
    // The list comes in as text so the parse is a seam a test can drive. The
    // message has no colon and no `{:?}` — it quotes the value the way the log
    // line reads, nothing more.
    let (logger, path) = witness("mdns_witness_interfaces");
    let texts = vec![
        "192.168.1.5".to_string(),
        "fe80::1".to_string(),
        "".to_string(),
        "10.8.0.2".to_string(),
    ];
    let found = multicast_interfaces(&texts, &logger);
    assert_eq!(
        found,
        vec![Ipv4Addr::new(192, 168, 1, 5), Ipv4Addr::new(10, 8, 0, 2)],
        "the good entries are kept"
    );
    let text = lines(&path);
    // Match whole lines, not `"…\n"`: the tail reader hands back whole lines
    // joined, so the final record has no newline to find (`read_tail` →
    // `text.lines().join("\n")`).
    let warns: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("mDNS skipping non-IPv4 interface"))
        .collect();
    assert_eq!(warns.len(), 2, "one line per bad entry; witness was:\n{}", text);
    assert!(
        warns.iter().any(|line| line.ends_with("interface fe80::1")),
        "the bad entry is named; witness was:\n{}",
        text
    );
    assert!(
        warns.iter().any(|line| line.ends_with("interface ")),
        "an empty string is also skipped, and the line stays bare: {}",
        text
    );
    assert_eq!(describe_interfaces(&found), "192.168.1.5, 10.8.0.2");
    assert_eq!(describe_interfaces(&[]), "none");
}

#[test]
fn z_d_the_reuse_options_are_set_and_the_debug_lines_say_mdns() {
    crate::pin_test_dir();
    // `bind` is not required for `setsockopt`, so an unbound socket2 socket
    // exercises this exactly. The two success lines are static `&str`s passed to
    // `log.debug`, and the red line is that they say `mDNS`, never `SSDP` —
    // "SSDP cannot set SO_REUSEPORT" would send whoever reads the log to the
    // SSDP server.
    let (logger, path) = witness("mdns_witness_reuse");
    let _debug = crate::DebugWindow::armed();
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).expect("a UDP socket opens");
    set_reuse_for_mdns(&sock, &logger);
    let text = lines(&path);
    assert!(
        text.contains("mDNS set SO_REUSEADDR"),
        "witness was:\n{}",
        text
    );
    assert!(
        !text.contains("SSDP"),
        "an mDNS log line must never name the other server: {}",
        text
    );
    #[cfg(unix)]
    assert!(
        text.contains("mDNS set SO_REUSEPORT"),
        "on unix both options are set: {}",
        text
    );
}

#[test]
fn z_e_a_reuse_failure_names_the_option_then_the_os_text_on_its_own_line() {
    crate::pin_test_dir();
    // The failure shape is copied from the SSDP server on purpose: two ERROR
    // lines, with the OS text alone on the second. Driving the helper directly
    // with a constructed error is the only way to see this branch on a machine
    // where the setsockopt calls succeed.
    let (logger, path) = witness("mdns_witness_reuse_fail");
    let error = io::Error::from_raw_os_error(92); // ENOPROTOOPT
    log_reuse_failure("SO_REUSEPORT", &error, &logger);
    let text = lines(&path);
    assert!(
        text.contains("mDNS cannot set SO_REUSEPORT"),
        "witness was:\n{}",
        text
    );
    assert!(
        !text.contains("mDNS cannot set SO_REUSEPORT: "),
        "the option line must not carry the OS text itself: {}",
        text
    );
    assert!(
        text.contains(&error.to_string()),
        "the OS text is logged, on its own line: {}",
        text
    );
    // The two lines are separate records: no single line holds both.
    let both = text
        .lines()
        .filter(|line| line.contains("SO_REUSEPORT") && line.contains(&error.to_string()))
        .count();
    assert_eq!(both, 0, "the split is the contract");
    assert!(!text.contains("SSDP"));
}

#[test]
fn z_f_the_non_unix_arm_is_a_no_op_and_the_unix_arm_exists() {
    crate::pin_test_dir();
    // Not a behaviour test — a shape test. The three `#[cfg]` arms this module
    // replaced were copied from `set_reuse_for_ssdp`, "whose table describes
    // macast's own ssdp.py, not zeroconf". Zeroconf's own rule is not
    // platform-dependent at all: every platform that can has both options.
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).expect("a UDP socket opens");
    let (logger, _path) = witness("mdns_witness_reuse_shape");
    #[cfg(unix)]
    {
        set_reuse_port(&sock).expect("SO_REUSEPORT is settable on unix");
        set_reuse_port_if_available(&sock, &logger);
    }
    #[cfg(not(unix))]
    set_reuse_port_if_available(&sock, &logger);
}

// ---------------------------------------------------------------------------
// J: the poll interval's own contract
// ---------------------------------------------------------------------------

#[test]
fn z_g_the_read_timeout_is_the_poll_granularity_not_a_budget() {
    crate::pin_test_dir();
    // `POLL_INTERVAL` has no Python equivalent — it is purely the granularity of
    // our poll, and the `asked` ledger is what keeps a tick from re-multicasting
    // a question it already sent. Pinning the value here is the honest way to
    // say "the budget lives in `browse`'s caller, not in the socket".
    assert_eq!(POLL_INTERVAL, Duration::from_millis(400));
    assert_eq!(READ_MAX, 8 * 1024);
    assert_eq!(MDNS_PORT, 5353);
    assert_eq!(MDNS_GROUP, "224.0.0.251");
    assert_eq!(SERVICE_TYPE, "_googlecast._tcp.local.");
    assert_eq!(LOGGER_NAME, "LocalFileCaster");
    // A read timeout is reported as WouldBlock *or* TimedOut; the module doc
    // says so, and `Multicast::recv` matches on both. There is no witness for
    // it here because `Multicast` is private and `open` binds port 5353.
    assert_eq!(MAX_DNS_LABELS, 128);
    assert_eq!(MAX_NAME_LENGTH, 253);
    assert_eq!(MAX_LABEL_LENGTH, 63);
    assert_eq!(MAX_MSG_LEN, 0xFFFF);
    assert_eq!(POINTER_TAG, 0xC0);
    assert_eq!(CLASS_MASK, 0x7FFF);
    assert_eq!(CLASS_UNIQUE, 0x8000);
}
