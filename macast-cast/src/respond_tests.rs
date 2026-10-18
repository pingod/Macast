//! The responder's answer table, diffed row by row against the reference.
//!
//! Every expectation here is read out of
//! `macast-cast/tests/golden/mdns_responder.jsonl`, which
//! `scripts/mdns_responder_probe.py --dump-corpus` produced by registering the
//! product's own service in a loopback-only `Zeroconf`, sending a query at it, and
//! recording what the reference meant to put on the wire. The corpus is the oracle;
//! this file's only invention is the mapping from a row to the inputs `respond()`
//! takes.
//!
//! The diff is a **sorted set of (section, type, ttl, owner, flush)**, not bytes:
//! the reference lays its additionals out in `set` order and the same question
//! produced `TXT/SRV/A/NSEC` in one run and `SRV/TXT/A/NSEC` in another
//! (design doc §14.6). A byte comparison here would be a test that goes red on a
//! hash seed and would then get deleted, which is the failure mode §4.13 keeps
//! meeting.
//!
//! Two floor questions this file answers rather than assumes:
//!
//! * **Every corpus row must have a mapping.** `the_corpus_rows_and_the_mapping_agree`
//!   asks in both directions, because a row nobody maps is a measured rule nobody
//!   enforces, and that is the exact state a golden corpus rots into.
//! * **`delays_ms` is never asserted.** Those numbers are this machine's timer
//!   (`U(20, 120)` ms aggregation), not a protocol fact; `Timing` exists so the
//!   choice is *representable*, and `the_unicast_leg_is_direct_and_the_group_leg_is_not`
//!   is as far as the assertions go.

use super::*;
use std::collections::BTreeSet;

/// One row of the frozen corpus, in the vocabulary `respond()` produces.
struct Row {
    case: String,
    /// `known` as the probe wrote it: (kind, ttl, alias-or-none).
    known: Vec<(String, u64, Option<String>)>,
    /// The records the reference answered with, already sorted for comparison.
    answers: BTreeSet<String>,
}

fn corpus() -> Vec<Row> {
    let text = include_str!("../tests/golden/mdns_responder.jsonl");
    let mut rows = Vec::new();
    for line in text.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("corpus line is not JSON: {}", e));
        let known = value["known"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                let entry = entry.as_array().cloned().unwrap_or_default();
                (
                    entry[0].as_str().unwrap_or("").to_string(),
                    entry[1].as_u64().unwrap_or(0),
                    entry[2].as_str().map(|s| s.to_string()),
                )
            })
            .collect();
        let answers = value["answers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|record| {
                format!(
                    "{}/{}/{}/{}/flush={}",
                    record["section"].as_str().unwrap_or(""),
                    record["type"].as_str().unwrap_or(""),
                    record["ttl"].as_u64().unwrap_or(0),
                    record["name"].as_str().unwrap_or(""),
                    record["cache_flush"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        rows.push(Row {
            case: value["case"].as_str().unwrap_or("").to_string(),
            known,
            answers,
        });
    }
    rows
}

/// The instance and server names the corpus fixture uses — `192.0.2.7`, a
/// TEST-NET-1 address, is the probe's, and only `has_v4` reaches this module.
const INSTANCE: &str = "Macast-Fixture-1._googlecast._tcp.local.";
const SERVER: &str = "Macast-Fixture-1.local.";

fn question(name: &str, kind: Kind, unicast_request: bool) -> Question {
    Question {
        name: name.to_string(),
        kind,
        unicast_request,
    }
}

/// The row → inputs table. `ucast_source` is the source port, which the probe varied
/// in exactly one case (`ptr-ephemeral-src`), so only that row is `true`.
fn inputs_for(row: &Row, shape: &Shape) -> (Vec<Question>, Vec<Known>, bool) {
    let known = row
        .known
        .iter()
        .map(|(kind, ttl, alias)| Known {
            kind: match kind.as_str() {
                "ptr" => Kind::Ptr,
                "srv" => Kind::Srv,
                "txt" => Kind::Txt,
                other => panic!("the corpus names a known-answer kind nobody maps: {}", other),
            },
            owner: if kind == "ptr" {
                SERVICE_TYPE.to_string()
            } else {
                shape.instance.to_string()
            },
            ttl: *ttl as u32,
            alias: alias.clone().or(if kind == "ptr" {
                Some(shape.instance.to_string())
            } else {
                None
            }),
        })
        .collect();
    let (name, kind) = match row.case.as_str() {
        "ptr" | "ptr-known-live" | "ptr-known-half" | "ptr-known-low" | "ptr-known-other-alias"
        | "ptr-ephemeral-src" => (SERVICE_TYPE, Kind::Ptr),
        "ptr-qu" => (SERVICE_TYPE, Kind::Ptr),
        "srv" | "srv-known-ptr" => (INSTANCE, Kind::Srv),
        "ptr-known-srv" => (SERVICE_TYPE, Kind::Ptr),
        "a" => (SERVER, Kind::A),
        "txt" => (INSTANCE, Kind::Txt),
        "any" => (INSTANCE, Kind::Any),
        "aaaa" => (SERVER, Kind::Aaaa),
        "enum" => (ENUM_TYPE, Kind::Ptr),
        "enum-star" => ("*._dns-sd._udp.local.", Kind::Ptr),
        "other-service" => ("_airplay._tcp.local.", Kind::Ptr),
        other => panic!("corpus case {} has no inputs — a measured rule nobody enforces", other),
    };
    let unicast_request = row.case == "ptr-qu";
    let ucast_source = row.case == "ptr-ephemeral-src";
    (
        vec![question(name, kind, unicast_request)],
        known,
        ucast_source,
    )
}

/// Every reply's records, flattened into the corpus's string form.
fn flatten(replies: &[Reply]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for reply in replies {
        for record in &reply.records {
            out.insert(format!(
                "{}/{}/{}/{}/flush={}",
                match record.section {
                    Section::Answer => "an",
                    Section::Additional => "ar",
                },
                match record.kind {
                    Kind::Ptr => "PTR",
                    Kind::Srv => "SRV",
                    Kind::Txt => "TXT",
                    Kind::A => "A",
                    Kind::Aaaa => "AAAA",
                    Kind::Nsec => "NSEC",
                    Kind::Any => "ANY",
                },
                record.ttl,
                record.owner,
                record.flush,
            ));
        }
    }
    out
}

fn run(case: &str) -> (BTreeSet<String>, Vec<Reply>) {
    crate::pin_test_dir();
    let shape = Shape::new(INSTANCE, SERVER, true);
    let row = corpus()
        .into_iter()
        .find(|row| row.case == case)
        .unwrap_or_else(|| panic!("no corpus row named {}", case));
    let (questions, known, ucast_source) = inputs_for(&row, &shape);
    let replies = respond(&questions, &known, ucast_source, &shape, None, 0);
    (row.answers, replies)
}

#[test]
fn every_corpus_row_is_answered_exactly_as_the_reference_answered_it() {
    crate::pin_test_dir();
    let rows = corpus();
    assert!(!rows.is_empty(), "the responder corpus read as empty");
    let mut checked = 0usize;
    for row in &rows {
        let shape = Shape::new(INSTANCE, SERVER, true);
        let (questions, known, ucast_source) = inputs_for(row, &shape);
        let replies = respond(&questions, &known, ucast_source, &shape, None, 0);
        assert_eq!(
            flatten(&replies),
            row.answers,
            "case {} does not match what the reference sent",
            row.case
        );
        checked += 1;
    }
    assert_eq!(checked, 17, "the corpus has {} rows, expected 17", checked);
}

#[test]
fn the_corpus_rows_and_the_mapping_agree_in_both_directions() {
    crate::pin_test_dir();
    // A row nobody maps is a measured rule nobody enforces, and a mapping nobody
    // uses is a case the reference was never asked. `inputs_for` panics on an
    // unmapped row, so the first half is proved by running every row once.
    let cases = corpus().into_iter().map(|row| row.case).collect::<Vec<_>>();
    for case in [
        "ptr",
        "ptr-qu",
        "ptr-ephemeral-src",
        "srv",
        "a",
        "txt",
        "any",
        "aaaa",
        "enum",
        "enum-star",
        "other-service",
        "ptr-known-live",
        "ptr-known-half",
        "ptr-known-low",
        "ptr-known-other-alias",
        "ptr-known-srv",
        "srv-known-ptr",
    ] {
        assert!(
            cases.iter().any(|held| held == case),
            "{} is asserted here but is not in the frozen corpus",
            case
        );
    }
}

#[test]
fn a_known_ptr_with_exactly_half_the_ttl_left_still_gets_an_answer() {
    crate::pin_test_dir();
    // The boundary is the whole point of `ptr-known-half`: the rule is
    // `other.ttl > record.ttl / 2`, 2250 is exactly half of 4500, so it must NOT
    // suppress. Written `>=` this row and the full-TTL row would look identical.
    let (expected, replies) = run("ptr-known-half");
    assert_eq!(flatten(&replies), expected);
    assert!(!replies.is_empty(), "the half-TTL row went silent");
}

#[test]
fn a_known_ptr_with_the_full_ttl_left_silences_the_whole_reply() {
    crate::pin_test_dir();
    // Not "the PTR is dropped": the additionals ride on the answer, so suppressing
    // the only answer record removes the datagram. `ptr-known-live` captured zero
    // bytes, and a port that kept sending SRV/TXT/A to a client that already asked
    // for them is the flooding §14 says this rule exists to stop.
    let (expected, replies) = run("ptr-known-live");
    assert!(replies.is_empty(), "the reference sent nothing, we sent {:?}", replies);
    assert!(expected.is_empty());
}

#[test]
fn suppression_matches_the_alias_so_a_different_instance_cannot_silence_us() {
    crate::pin_test_dir();
    // The querier holds a PTR with our name and our TTL pointing at `Other-Box`.
    // Keying suppression on (name, type) alone would go silent here and lose a real
    // device from a real phone's menu.
    let (expected, replies) = run("ptr-known-other-alias");
    assert!(!replies.is_empty(), "a foreign alias suppressed our answer");
    assert_eq!(flatten(&replies), expected);
}

#[test]
fn a_known_additional_still_rides_out_with_the_answer_it_belongs_to() {
    crate::pin_test_dir();
    // The hole this row closes: a mutant that extended suppression from the answer
    // records to the riding ones went **0-red** for a whole round, because no
    // capture had ever asked "you know the SRV — do you still want it handed to
    // you?". `mdns_responder_probe.py`'s `ptr-known-srv` case asked, and the
    // reference sent all five records anyway. So suppression is about the
    // **answer** a question earns, not about every record in the packet, and this
    // test is what makes that statement enforceable rather than decorative.
    let (expected, replies) = run("ptr-known-srv");
    assert_eq!(flatten(&replies), expected);
    let seen = flatten(&replies);
    assert_eq!(seen.len(), 5, "the riding SRV was suppressed: {:?}", seen);
    assert!(seen.iter().any(|row| row.starts_with("ar/SRV/")));
}

#[test]
fn knowing_the_ptr_does_not_suppress_an_answer_about_the_srv() {
    crate::pin_test_dir();
    // Suppression is per record, not per question: the asker knows who is a
    // Chromecast and asked for one instance's host, and that is a different record.
    let (expected, replies) = run("srv-known-ptr");
    assert_eq!(flatten(&replies), expected);
    assert!(flatten(&replies).iter().any(|row| row.starts_with("an/SRV/")));
}

#[test]
fn a_txt_question_answers_with_txt_and_nothing_else() {
    crate::pin_test_dir();
    let (expected, replies) = run("txt");
    let seen = flatten(&replies);
    assert_eq!(seen, expected);
    assert_eq!(seen.len(), 1, "TXT dragged additionals along: {:?}", seen);
}

#[test]
fn an_any_question_answers_txt_and_srv_but_never_the_ptr() {
    crate::pin_test_dir();
    let (expected, replies) = run("any");
    let seen = flatten(&replies);
    assert_eq!(seen, expected);
    assert!(
        !seen.iter().any(|row| row.contains("/PTR/")),
        "ANY answered a PTR the reference did not: {:?}",
        seen
    );
}

#[test]
fn an_aaaa_question_is_answered_with_nsec_rather_than_silence() {
    crate::pin_test_dir();
    // "This name exists and has no v6" is a record, and the difference matters to
    // the asker: silence reads as "nobody is listening", NSEC reads as "not this".
    let (expected, replies) = run("aaaa");
    assert_eq!(flatten(&replies), expected);
    assert!(flatten(&replies).iter().any(|row| row.starts_with("an/NSEC/")));
}

#[test]
fn enumeration_answers_with_the_type_and_the_star_form_answers_with_nothing() {
    crate::pin_test_dir();
    // Two rows because the pair is the finding: `_services._dns-sd._udp.local.` is
    // answered (owner is the enumeration name, alias is our type), and RFC 6763's
    // `*._dns-sd._udp.local.` is not answered at all by this reference.
    let (expected, replies) = run("enum");
    assert_eq!(flatten(&replies), expected);
    let (star_expected, star_replies) = run("enum-star");
    assert!(star_replies.is_empty(), "the star form answered: {:?}", star_replies);
    assert!(star_expected.is_empty());
}

#[test]
fn a_service_we_do_not_run_is_silent_and_that_is_not_an_error() {
    crate::pin_test_dir();
    let (expected, replies) = run("other-service");
    assert!(replies.is_empty());
    assert!(expected.is_empty());
}

#[test]
fn the_unicast_leg_is_direct_and_the_group_leg_is_not() {
    crate::pin_test_dir();
    // A query from a port other than 5353 gets **two** copies, which the corpus row
    // holds in one capture: five records with flush set, five without. The
    // flush-stripped one is the direct reply. `delays_ms` in that row says which
    // went first; asserting the milliseconds would be asserting this machine's
    // timer, so only the shape is checked.
    let (expected, replies) = run("ptr-ephemeral-src");
    assert_eq!(flatten(&replies), expected);
    assert_eq!(replies.len(), 2, "expected a group copy and a direct copy");
    let direct = replies.iter().find(|reply| reply.unicast).expect("no unicast reply");
    let group = replies.iter().find(|reply| !reply.unicast).expect("no group reply");
    assert_eq!(direct.timing, Timing::Direct);
    assert_ne!(group.timing, Timing::Direct);
    assert!(
        direct.records.iter().all(|record| !record.flush),
        "the unicast copy still carries cache-flush bits, which the reference clears"
    );
    assert!(group.records.iter().any(|record| record.flush));
}

#[test]
fn a_qu_question_from_port_5353_still_multicasts() {
    crate::pin_test_dir();
    // RFC 6762 §5.4's "usually unicast" is not what this reference does when it has
    // not multicast the record recently: it multicasts, so every peer's cache learns.
    // The corpus's `ptr-qu` row has one destination, and it is the group.
    let (expected, replies) = run("ptr-qu");
    assert_eq!(flatten(&replies), expected);
    assert_eq!(replies.len(), 1);
    assert!(!replies[0].unicast);
}

#[test]
fn without_an_address_no_host_record_is_promised() {
    crate::pin_test_dir();
    // `has_v4 == false` is the state `Announcer::start` refuses to announce from,
    // and an answer that names a server with no A record under it is a sender
    // hanging on a connection that cannot be made.
    let shape = Shape::new(INSTANCE, SERVER, false);
    let questions = vec![question(SERVICE_TYPE, Kind::Ptr, false)];
    let replies = respond(&questions, &[], false, &shape, None, 0);
    let seen = flatten(&replies);
    assert!(!seen.iter().any(|row| row.contains("/A/")), "{:?}", seen);
    assert!(!seen.iter().any(|row| row.contains("/NSEC/")), "{:?}", seen);
    assert!(seen.iter().any(|row| row.starts_with("an/PTR/")));
    let a_row = vec![question(SERVER, Kind::A, false)];
    assert!(
        respond(&a_row, &[], false, &shape, None, 0).is_empty(),
        "we answered an A question with no address to give"
    );
}

#[test]
fn the_ptr_answer_carries_no_flush_bit_while_every_riding_record_carries_one() {
    crate::pin_test_dir();
    // The PTR is shared between our service and every other Chromecast on the LAN,
    // so it is not "ours alone" and cannot claim cache-flush; SRV/TXT/A/NSEC are.
    // The corpus prints this per record and this is the one reading a hand-written
    // response builder gets backwards.
    let (expected, replies) = run("ptr");
    assert_eq!(flatten(&replies), expected);
    for record in &replies[0].records {
        if record.kind == Kind::Ptr {
            assert!(!record.flush, "the shared PTR claimed cache-flush");
        } else {
            assert!(record.flush, "{:?} lost its flush bit", record);
        }
    }
}

// ---------------------------------------------------------------------------
// The assembly layer: our bytes, read back by our own parser
// ---------------------------------------------------------------------------

/// The service the corpus fixture describes. `192.0.2.7` is TEST-NET-1, and the
/// TXT table only has to be *something* — the corpus diff compares record types,
/// TTLs, owners and flush bits, not the property bytes, because the property
/// payload is already pinned byte-for-byte by the announcement corpus.
fn fixture_service() -> crate::announce::Service {
    crate::announce::Service {
        instance: INSTANCE.to_string(),
        server: SERVER.to_string(),
        port: 8009,
        txt: vec![
            ("id".to_string(), b"00000000-0000-4000-8000-000000000007".to_vec()),
            ("fn".to_string(), b"Macast(Fixture-1.local)".to_vec()),
            ("ca".to_string(), b"5".to_vec()),
        ],
        addresses: vec!["192.0.2.7".parse().unwrap()],
        service_type: crate::announce::SERVICE_TYPE.to_string(),
        clipped: Vec::new(),
    }
}

/// An **independent** walk of a datagram's record skeleton.
///
/// Deliberately not `crate::mdns::parse_message`: that parser decodes the rdata of
/// five types and skips every other record (`mdns.rs:566-569`), so a round trip
/// through it cannot tell which *section* a skipped NSEC sat in, and the very
/// thing this test is for is the AN/AR split. Two readers, different code, same
/// bytes — the shape `announce_tests` uses for the announcement corpus, and the
/// reason `read_back` below stays: it proves the records we hand a sender are
/// records a sender using our own parser can read.
fn walk(bytes: &[u8]) -> Vec<(String, u16, u32, String, bool, Vec<u8>)> {
    crate::pin_test_dir();
    assert!(bytes.len() > 12, "a datagram with no header is not a datagram");
    let count = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
    let (qd, an, ns, ar) = (count(4), count(6), count(8), count(10));
    let mut offset = 12;
    /// Skip or read a name, following the compression pointer that ends it.
    fn name_at(bytes: &[u8], at: usize) -> (String, usize) {
        let mut labels = Vec::new();
        let mut cursor = at;
        let mut resume = None;
        loop {
            let length = bytes[cursor] as usize;
            if length == 0 {
                cursor += 1;
                break;
            }
            if length & 0xC0 == 0xC0 {
                let target = ((length & 0x3F) << 8) | bytes[cursor + 1] as usize;
                resume = Some(cursor + 2);
                let (decoded, _) = name_at(bytes, target);
                for label in decoded.split('.').filter(|label| !label.is_empty()) {
                    labels.push(label.to_string());
                }
                break;
            }
            cursor += 1;
            labels.push(String::from_utf8_lossy(&bytes[cursor..cursor + length]).to_string());
            cursor += length;
        }
        (labels.join(".") + ".", resume.unwrap_or(cursor))
    }
    for _ in 0..qd {
        let (_, next) = name_at(bytes, offset);
        offset = next + 4; // qtype, qclass
    }
    let mut out = Vec::new();
    for section in std::iter::repeat("an").take(an)
        .chain(std::iter::repeat("ns").take(ns))
        .chain(std::iter::repeat("ar").take(ar))
    {
        let (owner, next) = name_at(bytes, offset);
        offset = next;
        let rtype = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        let class = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]);
        let ttl = u32::from_be_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]);
        let length = u16::from_be_bytes([bytes[offset + 8], bytes[offset + 9]]) as usize;
        // Read the rdata **before** advancing: the slice is inside the record, and
        // the cursor has to end up after it. First version advanced first and then
        // sliced, which ran off the end of the packet.
        let rdata = bytes[offset + 10..offset + 10 + length].to_vec();
        offset += 10 + length;
        out.push((
            section.to_string(),
            rtype,
            ttl,
            owner,
            class & 0x8000 != 0,
            rdata,
        ));
    }
    assert_eq!(offset, bytes.len(), "the walk lost track of the packet's length");
    out
}

/// One record in the corpus's string form, so `walk` and the JSON compare directly.
/// (`record_row`, not `row`: the corpus loop binds `row` for its own
/// row, and a helper with the same name would be shadowed rather than called —
/// the `_restore59` shape AGENTS.md §4.14 already carries, arriving here on the
/// Rust side.)
fn record_row(section: &str, rtype: u16, ttl: u32, name: &str, flush: bool) -> String {
    format!("{}/{}/{}/{}/flush={}", section, kind_name(rtype), ttl, name, flush)
}

/// Decode our own datagram with **our browser's** parser, for the types it reads.
///
/// `_read_others` decodes A, PTR, CNAME, TXT and SRV and contributes nothing for
/// any other type (NSEC is 47), so this is a statement about our reader, not the
/// wire: the answers we send contain records our own discovery code cannot read.
/// That is worth saying out loud — and it is why `walk`, not this, carries the
/// section assertions above.
fn read_back(bytes: &[u8]) -> BTreeSet<String> {
    crate::pin_test_dir();
    let log = macast_core::logging::Logger::named("respond-selftest");
    let message = crate::mdns::parse_message(bytes, "127.0.0.1:5353", &log)
        .expect("our own answer did not parse — the writer produced bytes our reader refuses");
    message
        .records
        .iter()
        .map(|record| record_row("?", record.rtype, record.ttl, &record.name, record.flush))
        .collect()
}

/// The subset of a corpus row our own parser can see, so the two comparisons use
/// the same filter on both sides instead of one side quietly holding NSEC.
/// The wire number for one of our kinds. Written out rather than matched on the
/// constant names so a renamed arm in [`Kind`] shows up here as a compile error.
fn rtype_of(kind: Kind) -> u16 {
    match kind {
        Kind::A => 1,
        Kind::Ptr => 12,
        Kind::Txt => 16,
        Kind::Aaaa => 28,
        Kind::Srv => 33,
        Kind::Nsec => 47,
        Kind::Any => 255,
    }
}

fn readable_rows(rows: &BTreeSet<String>) -> BTreeSet<String> {
    rows.iter()
        .filter(|row| !row.contains("/NSEC/"))
        .cloned()
        .collect()
}

#[test]
fn every_corpus_answer_survives_our_own_writer_and_our_own_parser() {
    crate::pin_test_dir();
    let service = fixture_service();
    let shape = Shape::new(INSTANCE, SERVER, true);
    let rows = corpus();
    // Which rows are silent is **read off the corpus**, not remembered: three
    // measured nothing (`ptr-known-live` by suppression, `other-service` and
    // `enum-star` because the reference has no strategy for them). The count is
    // derived; the names are asserted, so a fourth silence cannot join unnoticed and
    // a vanished one cannot leave a rule unenforced.
    let silent: Vec<String> = rows
        .iter()
        .filter(|row| row.answers.is_empty())
        .map(|row| row.case.clone())
        .collect();
    assert_eq!(
        silent,
        vec![
            "ptr-known-live".to_string(),
            "other-service".to_string(),
            "enum-star".to_string()
        ],
        "the set of questions the reference does not answer changed"
    );
    let mut rows_checked = 0usize;
    for row in rows {
        let (questions, known, ucast_source) = inputs_for(&row, &shape);
        let replies = respond(&questions, &known, ucast_source, &shape, None, 0);
        if replies.is_empty() {
            assert!(
                row.answers.is_empty(),
                "{} answered nothing but the reference sent {:?}",
                row.case,
                row.answers
            );
            continue;
        }
        // The corpus row holds **every** datagram the reference sent for that
        // question (the unicast-sourced case holds two), so the comparison is over
        // the union of our legs too.
        let mut seen = BTreeSet::new();
        for reply in &replies {
            let bytes = datagram(&reply.records, &service);
            // (1) The independent walk: sections, types, TTLs, owners, flush bits.
            let walked: BTreeSet<String> = walk(&bytes)
                .into_iter()
                .map(|(section, rtype, ttl, name, flush, _)| {
                    record_row(&section, rtype, ttl, &name, flush)
                })
                .collect();
            let planned: BTreeSet<String> = reply
                .records
                .iter()
                .map(|record| {
                    record_row(
                        match record.section {
                            Section::Answer => "an",
                            Section::Additional => "ar",
                        },
                        rtype_of(record.kind),
                        record.ttl,
                        &record.owner,
                        record.flush,
                    )
                })
                // An `A` choice is **one per address**: the plan says the host
                // rides, the writer decides how many records that is.
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect();
            assert!(
                walked.len() >= planned.len(),
                "case {}: the bytes hold fewer records than the plan",
                row.case
            );
            for entry in &planned {
                assert!(
                    walked.contains(entry) || entry.contains("/A/"),
                    "case {}: the plan asked for {} and the bytes do not hold it",
                    row.case,
                    entry
                );
            }
            seen.extend(walked);
            // (2) Our own browser parser must be able to read what we hand out, for
            // every type it claims to support. NSEC is not one of them (see
            // `read_back`), so the comparison runs through the same filter on both
            // sides rather than pretending the header and the record list disagree.
            let parsed = read_back(&bytes);
            // `read_back` has no section labels to give — `mdns::parse_message`
            // merges the three sections into one list by design — so this half of
            // the comparison drops the prefix on both sides. The sections are not
            // unchecked: `walk` above compares them, and it is a different reader.
            let readable_plan: BTreeSet<String> = planned
                .into_iter()
                .filter(|entry| !entry.contains("/NSEC/"))
                .map(|entry| entry.splitn(2, '/').last().unwrap_or(&entry).to_string())
                .map(|entry| format!("?/{}", entry))
                .collect();
            assert_eq!(
                readable_rows(&parsed),
                readable_plan,
                "case {}: our parser reads a different answer than we wrote",
                row.case
            );
        }
        assert_eq!(
            seen, row.answers,
            "case {} does not match what the reference sent, after the round trip through our bytes",
            row.case
        );
        rows_checked += 1;
    }
    assert_eq!(
        rows_checked + silent.len(),
        17,
        "17 rows, {} answered, {} silent — the numbers do not add up to the corpus",
        rows_checked,
        silent.len()
    );
    assert_eq!(rows_checked, 14, "the corpus answers 14 of its 17 rows");
}

#[test]
fn the_nsec_record_is_written_even_though_our_parser_skips_it() {
    crate::pin_test_dir();
    // `read_back` cannot see NSEC, so the filter above must never become "we
    // stopped writing it". The walk counts it, and the answer the phone reads
    // depends on it being there.
    let service = fixture_service();
    let shape = Shape::new(INSTANCE, SERVER, true);
    let questions = vec![question(SERVICE_TYPE, Kind::Ptr, false)];
    let replies = respond(&questions, &[], false, &shape, None, 0);
    let bytes = datagram(&replies[0].records, &service);
    let nsec = walk(&bytes)
        .into_iter()
        .filter(|(_, rtype, ..)| *rtype == 47)
        .count();
    assert_eq!(nsec, 1, "the NSEC vanished from the answer: {:?}", walk(&bytes));
    assert!(
        read_back(&bytes).iter().all(|entry| !entry.contains("/NSEC/")),
        "the claim above is that our parser skips NSEC"
    );
}

#[test]
fn two_addresses_answer_with_two_a_records_not_one() {
    crate::pin_test_dir();
    // The plan layer says "the host rides along"; the assembly layer decides how
    // many records that is. A second interface means a second A — and a sender that
    // picks the wrong one is the §4.1 first-row complaint, so the count is the fact.
    let mut service = fixture_service();
    service.addresses.push("192.0.2.8".parse().unwrap());
    let shape = Shape::new(INSTANCE, SERVER, true);
    let questions = vec![question(SERVICE_TYPE, Kind::Ptr, false)];
    let replies = respond(&questions, &[], false, &shape, None, 0);
    let bytes = datagram(&replies[0].records, &service);
    let a_rows = walk(&bytes)
        .into_iter()
        .filter(|(_, rtype, ..)| *rtype == 1)
        .count();
    assert_eq!(a_rows, 2, "two addresses produced {} A records", a_rows);
}

#[test]
fn kind_names_are_the_wire_names_not_our_rust_names() {
    crate::pin_test_dir();
    // The corpus spells PTR/SRV/TXT/A/AAAA/NSEC; the diff test compares strings, so
    // a renamed arm in `kind_name` silently makes every row compare against a name
    // nothing on the wire uses. Asked directly, because nothing else asks it.
    for (rtype, name) in [
        (1u16, "A"),
        (12, "PTR"),
        (16, "TXT"),
        (33, "SRV"),
        (28, "AAAA"),
        (47, "NSEC"),
    ] {
        assert_eq!(crate::respond::kind_name(rtype), name, "type {}", rtype);
    }
    assert_eq!(crate::respond::kind_name(99), "99", "an unknown type must print its number");
}

#[test]
fn the_answer_names_the_host_and_the_port_a_sender_would_connect_to() {
    crate::pin_test_dir();
    // The corpus diff compares (section, type, ttl, owner, flush) and **cannot see
    // rdata** — which is the part an answering service can get wrong in the way that
    // matters: a PTR pointing at the wrong instance, an SRV naming a host nothing
    // owns, an A carrying an address that is not ours. So the bytes are decoded with
    // our own reader and their payloads are checked against the service that was
    // asked about.
    let service = fixture_service();
    let shape = Shape::new(INSTANCE, SERVER, true);
    let questions = vec![question(SERVICE_TYPE, Kind::Ptr, false)];
    let replies = respond(&questions, &[], false, &shape, None, 0);
    let bytes = datagram(&replies[0].records, &service);
    let log = macast_core::logging::Logger::named("respond-selftest");
    let message = crate::mdns::parse_message(&bytes, "127.0.0.1:5353", &log).unwrap();
    let find = |rtype: u16| -> &crate::mdns::RData {
        &message
            .records
            .iter()
            .find(|record| record.rtype == rtype)
            .unwrap_or_else(|| panic!("no record of type {} in {:?}", rtype, message.records))
            .rdata
    };
    assert_eq!(
        find(12),
        &crate::mdns::RData::Name(INSTANCE.to_string()),
        "the PTR pointed somewhere other than this instance"
    );
    let srv = find(33);
    match srv {
        crate::mdns::RData::Srv { priority, weight, port, target } => {
            assert_eq!((*priority, *weight), (0, 0), "the reference writes zeros for both");
            assert_eq!(*port, 8009, "the answer promised a port the service does not run on");
            assert_eq!(target, SERVER, "the SRV names a host nothing in the packet owns");
        }
        other => panic!("SRV decoded as {:?}", other),
    }
    assert_eq!(find(1), &crate::mdns::RData::A("192.0.2.7".parse().unwrap()));
    let txt = find(16);
    match txt {
        crate::mdns::RData::Txt(bytes) => {
            let pairs = crate::mdns::txt_pairs(bytes);
            let id = crate::mdns::txt_value(&pairs, "id");
            assert_eq!(id.as_deref(), Some("00000000-0000-4000-8000-000000000007"));
        }
        other => panic!("TXT decoded as {:?}", other),
    }
}

#[test]
fn the_answer_carries_the_same_nsec_bytes_as_the_announcement() {
    crate::pin_test_dir();
    // Neither reader in this file can see inside an NSEC: `mdns::parse_message`
    // skips the type by design, and `walk` only labels records. That left the
    // responder's NSEC rdata with **no witness at all** until a mutant that wrote an
    // empty type bitmap passed every test — so this compares its bytes with the one
    // record the announcement corpus already pins byte-for-byte. Same name, same
    // bitmap, one derivation: a reply that contradicts the burst that preceded it is
    // a sender caching two different answers about the same host.
    let service = fixture_service();
    let shape = Shape::new(INSTANCE, SERVER, true);
    let questions = vec![question(SERVICE_TYPE, Kind::Ptr, false)];
    let replies = respond(&questions, &[], false, &shape, None, 0);
    let answered = walk(&datagram(&replies[0].records, &service))
        .into_iter()
        .find(|(_, rtype, ..)| *rtype == 47)
        .expect("the answer carried no NSEC")
        .5;
    let announced = walk(&service.announcement())
        .into_iter()
        .find(|(_, rtype, ..)| *rtype == 47)
        .expect("the announcement carried no NSEC")
        .5;
    assert_eq!(
        answered, announced,
        "the reply's NSEC is {:?} and the announcement's is {:?}",
        answered, announced
    );
    // The pointer at the front is **not** invariant — it is an offset into whichever
    // packet this record happens to sit in, and the announcement and the answer
    // order their records differently. What must match is the bitmap the two paths
    // derive from the same three lines: window 0, one octet, the A bit.
    assert_eq!(&answered[answered.len() - 3..], &[0x00, 0x01, 0x40]);
}
