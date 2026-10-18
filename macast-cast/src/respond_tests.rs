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
