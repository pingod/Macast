//! The aggregation queues and the last-second ledger, tested against the four
//! answers the reference gave when it was asked (design doc §16's aggregation
//! round: `scripts/mdns_responder_probe.py`, cases `txt-twice`, `ptr-then-txt`,
//! `srv-then-txt`, `txt-twice-1s`).
//!
//! Those four readings are **counts and ordering**, not milliseconds: how many
//! datagrams a pair of questions cost, and whether the second question rode out with
//! the first. That is what makes them testable here at all, because the millisecond
//! figures belong to the machine that produced them — a `U(20, 120)` draw is not a
//! protocol fact, and the probe's own corpus deliberately keeps `delays_ms` out of
//! the judged columns.
//!
//! The draw is therefore injected ([`Outgoing::with_random`]) and the clock is a
//! number the test writes down. Without those two seams every assertion about
//! merging would be a coin flip that passes most of the time — which is the shape of
//! a flaky test, and a flaky test in this repo gets deleted rather than fixed.

use super::{
    Added, Ledger, Outgoing, OutgoingQueue, Queued, plan_identity, production_draw,
    wire_identity,
    RANDOM_MAX_MS,
    RANDOM_MIN_MS, AGGREGATION_DELAY_MS, PROTECTION_WINDOW_MS, PROTECTED_AGGREGATION_DELAY_MS,
    PROTECTED_ADDITIONAL_DELAY_MS};
use crate::announce::{self, Service, Served, MDNS_PORT};
use macast_core::logging::Logger;
use crate::mdns;
use crate::respond::{Chosen, Group, Kind, Section, Shape, Timing};
use std::net::SocketAddr;

fn service() -> Service {
    Service {
        instance: "Macast-Fixture-1._googlecast._tcp.local.".to_string(),
        server: "Macast-Fixture-1.local.".to_string(),
        port: 8009,
        txt: vec![("id".to_string(), b"00000000-0000-4000-8000-000000000007".to_vec())],
        addresses: vec!["192.0.2.1".parse().expect("a test address")],
        service_type: crate::announce::SERVICE_TYPE.to_string(),
        clipped: Vec::new(),
    }
}

fn answer(kind: Kind, owner: &str, alias: Option<&str>) -> Chosen {
    Chosen {
        kind,
        section: Section::Answer,
        owner: owner.to_string(),
        ttl: 4500,
        flush: kind != Kind::Ptr,
        alias: alias.map(|a| a.to_string()),
    }
}

/// One planned answer with no riders.
fn planned(kind: Kind, owner: &str, alias: Option<&str>) -> Group {
    Group {
        answer: answer(kind, owner, alias),
        additionals: Vec::new(),
            from_qu: false,
    }
}

fn queued(service: usize, kind: Kind, owner: &str, alias: Option<&str>) -> Queued {
    Queued::new(service, &planned(kind, owner, alias))
}

// ---------------------------------------------------------------------------
// The four recorded readings, as rules rather than as luck
// ---------------------------------------------------------------------------

#[test]
fn reading_one_a_second_question_inside_the_first_s_wait_costs_no_datagram() {
    crate::pin_test_dir();
    // Measured: the same TXT question asked twice, 10 ms apart, produced **one**
    // 212-byte datagram at +104 ms — the second question never touched the wire.
    // Drawn here at the window's ends so the merge is arithmetic rather than chance:
    // the first answer waits 120 ms, the second would have waited 20.
    let mut queue = OutgoingQueue::aggregating();
    let first = queue.add(0, RANDOM_MAX_MS, vec![queued(0, Kind::Txt, "a", None)]);
    assert_eq!(first, Added::Appended, "an empty queue cannot merge");
    let second = queue.add(10, RANDOM_MIN_MS, vec![queued(0, Kind::Txt, "a", None)]);
    assert_eq!(second, Added::Merged, "the second question opened a second group");
    assert_eq!(queue.group_count(), 1);
    let batch = queue.ready(200);
    assert_eq!(batch.len(), 1, "the merged pair went out twice");
    assert!(queue.is_empty());
}

#[test]
fn reading_two_two_groups_wait_for_the_first_one_s_deadline_not_its_own() {
    crate::pin_test_dir();
    // Measured: PTR asked, then TXT asked 10 ms later — **one** datagram, at
    // **+502 ms**, i.e. at the aggregation ceiling rather than at either answer's own
    // `send_after`. That offset is not the draw (which is capped at 120); it is
    // `async_ready` deferring while more than one group waits, so that a third
    // question arriving in the window can still be merged into the pair.
    let mut queue = OutgoingQueue::aggregating();
    queue.add(0, RANDOM_MIN_MS, vec![queued(0, Kind::Ptr, "svc", Some("a"))]);
    queue.add(10, RANDOM_MAX_MS, vec![queued(0, Kind::Txt, "a", None)]);
    assert_eq!(
        queue.group_count(),
        2,
        "the second draw merged; the pair has to be two groups to test the deferral"
    );
    let (first, second) = (&queue.groups()[0], &queue.groups()[1]);
    assert!(
        second.send_after < first.send_before,
        "the second group's own deadline ({}) is not inside the first's window ({})",
        second.send_after,
        first.send_before
    );
    assert_eq!(
        queue.due(20),
        Some(AGGREGATION_DELAY_MS),
        "the loop would have woken at the first group's `send_after` and sent early"
    );
    assert!(queue.ready(300).is_empty(), "the pair went out before the ceiling");
    let batch = queue.ready(AGGREGATION_DELAY_MS);
    assert_eq!(
        batch.len(),
        2,
        "the ceiling released only one of the two groups, so the pair cost two datagrams"
    );
}

#[test]
fn reading_three_an_aggregated_answer_following_a_due_one_costs_a_second_datagram() {
    crate::pin_test_dir();
    // Measured: SRV (an immediate question type, answered on the spot) then TXT
    // (aggregated) → **two** datagrams, +0.7 ms and +87.5 ms. The two buckets never
    // merge, and this test pins the queue half of that sentence: a queue must be able
    // to fire, then take a second answer and fire again. If a flushed queue stayed
    // "warm" and swallowed the next answer into the batch that already left, the
    // measured count of two becomes one.
    let mut queue = OutgoingQueue::aggregating();
    queue.add(100, RANDOM_MIN_MS, vec![queued(0, Kind::Txt, "a", None)]);
    assert_eq!(queue.due(100), Some(120), "the draw did not reach 20 ms");
    let first = queue.ready(120);
    assert_eq!(first.len(), 1, "the due answer stayed queued");
    assert!(queue.is_empty(), "a flushed queue still holds the batch it sent");
    queue.add(130, RANDOM_MIN_MS, vec![queued(0, Kind::Txt, "b", None)]);
    let second = queue.ready(150);
    assert_eq!(second.len(), 1, "the second answer merged into a batch already sent");
    assert_eq!(second[0].answer.owner, "b", "the batches came out of order");
}

#[test]
fn reading_four_a_protected_answer_adds_a_second_on_top_of_the_draw() {
    crate::pin_test_dir();
    // Measured, and the prize of that round: the same TXT asked twice, 1.25 s apart,
    // produced two datagrams — the second at **+1339.6 ms**, which is
    // `out_delay_queue` (`additional_delay = 1000`, window 200).
    let mut protected = OutgoingQueue::protected();
    protected.add(1250, RANDOM_MIN_MS, vec![queued(0, Kind::Txt, "a", None)]);
    let group = &protected.groups()[0];
    assert_eq!(
        group.send_after,
        1250 + PROTECTED_ADDITIONAL_DELAY_MS + RANDOM_MIN_MS,
        "the protected queue did not add a second on top of the draw"
    );
    assert_eq!(
        group.send_before,
        1250 + PROTECTED_ADDITIONAL_DELAY_MS + PROTECTED_AGGREGATION_DELAY_MS,
        "the protected window is 200 ms, not the ordinary 500"
    );
    assert!(
        protected.ready(1400).is_empty(),
        "the protected answer went out before its floor"
    );
    assert_eq!(protected.ready(2_270).len(), 1);
}

// ---------------------------------------------------------------------------
// The rules behind the readings
// ---------------------------------------------------------------------------

#[test]
fn the_merge_boundary_is_inclusive_because_the_reference_compares_with_less_or_equal() {
    crate::pin_test_dir();
    // Two draws landing on exactly the same deadline must merge (`send_after <=
    // last.send_after`). With `<` that pair costs two datagrams, and the coincidence
    // is reachable: two questions can draw the same number.
    let mut queue = OutgoingQueue::aggregating();
    queue.add(0, 60, vec![queued(0, Kind::Txt, "a", None)]);
    assert_eq!(queue.groups()[0].send_after, 60);
    assert_eq!(
        queue.add(0, 60, vec![queued(0, Kind::Txt, "b", None)]),
        Added::Merged,
        "an equal deadline opened a second group"
    );
    assert_eq!(queue.group_count(), 1);
    // One millisecond past it is a new group, because there is no longer a wait to join.
    assert_eq!(
        queue.add(0, 61, vec![queued(0, Kind::Txt, "c", None)]),
        Added::Appended
    );
    assert_eq!(queue.group_count(), 2);
}

#[test]
fn a_merged_batch_replaces_the_riding_records_rather_than_unioning_them() {
    crate::pin_test_dir();
    // `dict.update`, not `setdefault`: the later group's answer wins with **its own**
    // additionals. A union would send a rider the reference dropped, and the two
    // differ whenever a second question earns a different set of riders.
    let mut queue = OutgoingQueue::aggregating();
    let with_rider = Group {
        answer: answer(Kind::Txt, "a", None),
        additionals: vec![answer(Kind::A, "host", None)],
            from_qu: false,
    };
    let without = Group {
        answer: answer(Kind::Txt, "a", None),
        additionals: Vec::new(),
            from_qu: false,
    };
    queue.add(0, RANDOM_MAX_MS, vec![Queued::new(0, &with_rider)]);
    queue.add(10, RANDOM_MIN_MS, vec![Queued::new(0, &without)]);
    let batch = queue.ready(500);
    assert_eq!(batch.len(), 1, "the same answer was queued twice");
    assert!(
        batch[0].additionals.is_empty(),
        "the merge unioned the additionals; the reference overwrote them"
    );
}

#[test]
fn an_answer_that_goes_out_is_taken_out_of_the_groups_still_waiting() {
    crate::pin_test_dir();
    // `_remove_answers_from_queue`. Two groups, the second holding the same PTR plus
    // a TXT of its own: when the first fires, the PTR must not be shouted again by the
    // group behind it — but the TXT has its own deadline and stays.
    let mut queue = OutgoingQueue::aggregating();
    queue.add(0, RANDOM_MIN_MS, vec![queued(0, Kind::Ptr, "svc", Some("a"))]);
    queue.add(
        10,
        900,
        vec![
            queued(0, Kind::Ptr, "svc", Some("a")),
            queued(0, Kind::Txt, "a", None),
        ],
    );
    // Asked **at the ceiling**, not at the first group's own deadline: with two groups
    // waiting the deferral rule holds the first until 500 ms, and a `ready(100)` here
    // would be testing the deferral rather than the dedupe.
    let first = queue.ready(500);
    assert_eq!(first.len(), 1, "the first batch did not go out at the ceiling");
    assert_eq!(queue.group_count(), 1, "the whole second group vanished");
    let held = &queue.groups()[0];
    assert_eq!(
        held.answers.len(),
        1,
        "the answer that already went out is still queued behind it"
    );
    assert_eq!(held.answers[0].answer.kind, Kind::Txt);
}

#[test]
fn a_group_emptied_by_the_dedupe_keeps_its_place_because_the_reference_keeps_it() {
    crate::pin_test_dir();
    // The reference pops *keys* out of pending groups and leaves the group in the
    // deque. An empty group still defers what is behind it (`len(self.queue) > 1` in
    // `async_ready`), so a port that garbage-collected it would send the next batch
    // early — the flooding the whole module exists to prevent.
    let mut queue = OutgoingQueue::aggregating();
    queue.add(0, RANDOM_MIN_MS, vec![queued(0, Kind::Txt, "a", None)]);
    queue.add(10, 900, vec![queued(0, Kind::Txt, "a", None)]);
    queue.ready(500);
    assert_eq!(queue.group_count(), 1);
    assert!(queue.groups()[0].answers.is_empty());
    assert_eq!(
        queue.due(100),
        Some(910),
        "the emptied group stopped deferring the queue behind it"
    );
}

#[test]
fn the_two_queues_are_two_batches_and_not_one_merged_packet() {
    crate::pin_test_dir();
    // `construct_outgoing_multicast_answers` is called once per queue's merged dict.
    // Flattening both into one packet would put an answer that was told to wait a
    // second on the wire beside one that was not — the exact divergence the four
    // readings were taken to rule out. It also means the *same* record can legitimately
    // be queued twice (once per queue), because the two queues never talk.
    let mut out = Outgoing::with_random(Box::new(|| RANDOM_MIN_MS));
    let groups = vec![planned(Kind::Txt, "a", None)];
    assert_eq!(out.enqueue(Timing::Aggregate, 0, &groups, 0), 1);
    assert_eq!(out.enqueue(Timing::AggregateProtected, 0, &groups, 0), 1);
    assert_eq!(out.waiting(), 2);
    let due = out.due(100);
    assert_eq!(due.len(), 1, "both queues fired together at +100");
    assert_eq!(due[0].len(), 1);
    let due = out.due(1_100);
    assert_eq!(
        due.len(),
        1,
        "the protected answer did not come out on its own a second later"
    );
    assert_eq!(due[0][0].answer.owner, "a");
}

#[test]
fn the_deadline_is_the_sooner_of_the_two_queues() {
    crate::pin_test_dir();
    let mut out = Outgoing::with_random(Box::new(|| RANDOM_MAX_MS));
    let groups = vec![planned(Kind::Txt, "a", None)];
    out.enqueue(Timing::AggregateProtected, 0, &groups, 0);
    out.enqueue(Timing::Aggregate, 0, &groups, 0);
    assert_eq!(
        out.deadline(0),
        Some(RANDOM_MAX_MS),
        "the loop would have slept past the ordinary queue waiting for the protected one"
    );
    assert_eq!(out.due(RANDOM_MAX_MS).len(), 1);
    assert_eq!(out.deadline(200), Some(1_120), "the protected queue was forgotten");
}

#[test]
fn an_empty_outgoing_promises_no_deadline() {
    crate::pin_test_dir();
    // The loop turns `None` into "wait the whole poll interval". A queue that
    // reported a deadline it did not have would spin the socket for no reason, and
    // one that reported none while holding an answer would strand it — which is the
    // `Ok(0)`-versus-`WouldBlock` lesson from `channel.rs`'s fixture, one layer up.
    let mut out = Outgoing::with_random(Box::new(|| RANDOM_MIN_MS));
    assert_eq!(out.deadline(0), None);
    assert!(out.due(10_000).is_empty());
}

#[test]
fn the_production_draw_stays_inside_the_inclusive_window() {
    crate::pin_test_dir();
    // Read through the queue rather than by calling `rand` in here: an expression
    // copied into a test proves the test's own arithmetic. With 400 samples both
    // ends have to show up, which is what makes `gen_range(20..120)` — the shape that
    // reads like the same thing — a red test rather than a rarer tail.
    let mut low = 0usize;
    let mut high = 0usize;
    for _ in 0..5_000 {
        let draw = production_draw();
        assert!(
            (RANDOM_MIN_MS..=RANDOM_MAX_MS).contains(&draw),
            "a draw left the window: {draw}"
        );
        if draw == RANDOM_MIN_MS {
            low += 1;
        }
        if draw == RANDOM_MAX_MS {
            high += 1;
        }
    }
    assert!(low > 0 && high > 0, "tails never reached: low={low} high={high}");
    // …and the production queue really is the function that was just sampled: a
    // second draw expression inside `Outgoing::new` would make everything above a
    // statement about a function nobody uses.
    let src = include_str!("aggregate.rs");
    let at = src
        .find("pub fn new() -> Outgoing {")
        .expect("the production constructor exists");
    assert!(
        src[at..at + 200].contains("Box::new(production_draw)"),
        "Outgoing::new no longer routes through production_draw"
    );
}

// ---------------------------------------------------------------------------
// The ledger: what the wire carried, heard rather than remembered
// ---------------------------------------------------------------------------

#[test]
fn the_bytes_we_send_read_back_into_the_same_record_key() {
    crate::pin_test_dir();
    // The one witness that the two identity producers are one rule.
    // `plan_identity` is built from a `Chosen` plus the service; `wire_identity`
    // from parsed bytes. If they drift, the ledger files a key under a name the wire
    // never carried and the flood protection stops firing with no symptom short of a
    // menu that asks six times and hears six answers. So the test asks the bytes.
    let service = service();
    let logger = Logger::named("Aggregate-Identity");
    let message = mdns::parse_message(&service.announcement(), "test", &logger)
        .expect("our own announcement parses");
    let heard: Vec<String> = message
        .records
        .iter()
        .filter_map(wire_identity)
        .map(|key| key.as_str().to_string())
        .collect();
    let shape = Shape::new(&service.instance, &service.server, true);
    let mut planned_keys: Vec<String> = Vec::new();
    for (kind, name) in [
        (Kind::Ptr, service.service_type.clone()),
        (Kind::Srv, service.instance.clone()),
        (Kind::Txt, service.instance.clone()),
        (Kind::A, service.server.clone()),
        (Kind::Any, service.instance.clone()),
    ] {
        let question = crate::respond::Question {
            name,
            kind,
            unicast_request: false,
        };
        for reply in crate::respond::respond(&[question], &[], false, &shape) {
            for group in &reply.groups {
                let mut records = vec![group.answer.clone()];
                records.extend(group.additionals.iter().cloned());
                for record in records {
                    for key in plan_identity(&record, &service) {
                        planned_keys.push(key.as_str().to_string());
                    }
                }
            }
        }
    }
    for key in &heard {
        assert!(
            planned_keys.contains(key),
            "the wire carries a record no plan names: {key}\nplanned: {planned_keys:?}"
        );
    }
    // Four of the five records in an announcement are hearable; the fifth is NSEC,
    // which this parser does not decode (see the next test).
    assert_eq!(heard.len(), 4, "a five-record announcement read back as: {heard:?}");
    assert!(
        planned_keys.iter().any(|k| k.starts_with("Nsec")),
        "the plan lost its NSEC key: {planned_keys:?}"
    );
}

#[test]
fn the_ledger_cannot_hear_an_nsec_and_says_so() {
    crate::pin_test_dir();
    // A recorded divergence, not a silent one. `mdns.rs` skips NSEC (the announcement
    // corpus pins that skip), so an `AAAA` answer's NSEC never enters the ledger and
    // the reference's one-second protection for *that* record is unreachable here —
    // the reference parses NSEC into its cache and would put a repeated `AAAA`
    // question on the protected queue. Every other record of the same packet is
    // protected, which the second half of this test pins from the other side, so the
    // row above is about one record type rather than about the mechanism.
    let service = service();
    let logger = Logger::named("Aggregate-Nsec");
    let message = mdns::parse_message(&service.announcement(), "test", &logger)
        .expect("it parses");
    assert!(
        !message.records.iter().any(|r| r.rtype == 47),
        "the parser started reading NSEC; §8's divergence row has to be re-measured, not kept"
    );
    let mut ledger = Ledger::default();
    ledger.learn(&message.records, 0);
    let nsec = answer(Kind::Nsec, &service.server, None);
    let nsec_key = plan_identity(&nsec, &service)
        .into_iter()
        .next()
        .expect("a plan key");
    assert!(
        ledger.seen(&nsec_key, 100).is_none(),
        "an NSEC was heard after all; this test's premise is stale"
    );
    let txt = answer(Kind::Txt, &service.instance, None);
    let txt_key = plan_identity(&txt, &service)
        .into_iter()
        .next()
        .expect("a plan key");
    assert_eq!(ledger.seen(&txt_key, 0), Some(0), "the TXT is not protected at all");
    assert_eq!(
        ledger.seen(&txt_key, PROTECTION_WINDOW_MS - 1),
        Some(0),
        "the window ended before a second"
    );
    assert_eq!(
        ledger.seen(&txt_key, PROTECTION_WINDOW_MS),
        None,
        "the window is `<`, and the reference's `_now - created < _ONE_SECOND` is strict"
    );
}

#[test]
fn a_goodbye_forgets_what_it_named_instead_of_protecting_it() {
    crate::pin_test_dir();
    // `record_manager.py:130-135`: an expired record is a removal, not an add. The
    // announcer's own goodbye burst (five records at TTL 0, looped back a poll later)
    // must not leave those keys "just shouted" — the receiver has just said this
    // service is gone.
    let service = service();
    let logger = Logger::named("Aggregate-Goodbye");
    let hello = mdns::parse_message(&service.announcement(), "t", &logger).expect("parses");
    let bye = mdns::parse_message(&service.goodbye(), "t", &logger).expect("parses");
    let mut ledger = Ledger::default();
    assert_eq!(ledger.learn(&hello.records, 0), 4, "the hello learned nothing");
    assert_eq!(ledger.len(), 4);
    assert_eq!(ledger.learn(&bye.records, 10), 4, "the goodbye did not act");
    assert_eq!(ledger.len(), 0, "a goodbye left its records protected");
    assert_eq!(
        ledger.learn(&bye.records, 20),
        0,
        "a second goodbye of an empty ledger reported work it did not do"
    );
}

#[test]
fn the_prune_ages_on_a_quarter_of_the_ttl_because_that_is_the_longer_question() {
    crate::pin_test_dir();
    // The one-second window and the quarter-TTL window live in the same entry, so
    // `prune` has to age on the *longer* one. The first version of this function pruned
    // at `PROTECTION_WINDOW_MS` and passed every assertion that existed — which is
    // exactly what a silent loss looks like: `seen` kept answering correctly, and
    // `is_recent` quietly became a function that had never met an entry old enough to
    // answer "yes". The TXT this fixture announces carries TTL 4500, so it must still be
    // recent at +1124 s and not at +1126 s.
    let service = service();
    let logger = Logger::named("Aggregate-Prune");
    let hello = mdns::parse_message(&service.announcement(), "t", &logger).expect("parses");
    let mut ledger = Ledger::default();
    ledger.learn(&hello.records, 0);
    assert_eq!(ledger.len(), 4);
    let txt = plan_identity(&answer(Kind::Txt, &service.instance, None), &service)
        .into_iter()
        .next()
        .expect("a key for the TXT");
    // At one minute the 120-second SRV and A are correctly gone (`_RECENT_TIME_MS * 120`
    // is 30 s), and the two 4500-second records — PTR and TXT — are not. Under the old
    // one-second prune all four vanished, which is the loss this row exists to catch:
    // the flood question would still have answered "no", as it always does by then, and
    // the QU question would have answered "no" forever.
    ledger.prune(60_000);
    assert_eq!(
        ledger.len(),
        2,
        "the long-TTL records did not outlive the short ones: {:?}",
        ledger
    );
    assert!(ledger.is_recent(&txt, 60_000), "the entry is gone from the map");
    // The boundary is `created + 250 * ttl > now` — strict, the reference's own `>`.
    assert!(
        ledger.is_recent(&txt, 4500 * 250 - 1),
        "the quarter window ended early"
    );
    assert!(
        !ledger.is_recent(&txt, 4500 * 250),
        "the window is `>=`; `_dns.py:210` is `>`"
    );
    ledger.prune(4500 * 250 + 1);
    assert_eq!(
        ledger.len(),
        0,
        "an entry that answers neither question lived on — the map would grow with the LAN"
    );
    // …and the one-second question still says "no" long after its own window, rather
    // than panicking or resurrecting: two questions, one entry, no contradiction.
    assert_eq!(ledger.seen(&txt, 4500 * 250 - 1), None);
}

#[test]
fn a_qu_question_from_the_group_is_multicast_now_when_the_record_is_not_recent() {
    crate::pin_test_dir();
    // This is the corpus's `ptr-qu` row, and the reason the port had to grow a fourth
    // route: `async_response` sends a QU answer that arrived on the **group** through
    // `add_qu_question_response`, which puts it in `_mcast_now` — immediately, not
    // behind the aggregation delay — when the record has not gone out within a quarter
    // of its TTL. Measured: that row answered at +0.3 ms while the plain `ptr` row of
    // the same corpus went out at +92 ms. Until this round the port queued it like any
    // other PTR answer, so the row's own delay was a witness nobody was reading.
    let service = service();
    let query = qu_query(&service.service_type, mdns::TYPE_PTR);
    let (served, group, direct) = served_from(&service, &query, 500, 600, MDNS_PORT, |_| {});
    assert_eq!(served.queries, 1);
    assert_eq!(
        served.group, 1,
        "a QU question over the group was queued instead of answered now: {:?}",
        served
    );
    assert_eq!(served.queued, 0, "the QU answer reached a queue");
    assert_eq!(
        served.direct, 0,
        "a QU answer with nothing recent also went back to the asker"
    );
    assert_eq!(group.len(), 1);
    assert!(direct.is_empty());
}

#[test]
fn a_qu_question_from_the_group_is_unicast_only_once_the_record_is_recent() {
    crate::pin_test_dir();
    // RFC 6762 §5.4's other half, and the reason `_mcast_now` is not the only answer:
    // once this responder has sent the record within a quarter of its TTL, a QU
    // question is answered **back to the asker alone** — the group already has it, and
    // re-shouting it is the flood. Six browsers opening a Cast menu in one second is
    // the shape this exists for. Note what the route is *not*: it is not the direct copy
    // an ephemeral-port query gets, which is also sent to the group;
    // `query_handler.py:245-248`'s `continue` skips the multicast branch entirely.
    let service = service();
    let announcement = service.announcement();
    let query = qu_query(&service.service_type, mdns::TYPE_PTR);
    let heard = |out: &mut Outgoing| {
        let logger = Logger::named("Aggregate-QuRecent");
        let message = mdns::parse_message(&announcement, "t", &logger).expect("parses");
        out.heard.learn(&message.records, 0);
    };
    let (served, group, direct) = served_from(&service, &query, 500, 600, MDNS_PORT, heard);
    assert_eq!(served.direct, 1, "the QU answer did not go back to the asker");
    assert_eq!(
        served.group + served.queued,
        0,
        "the group was told again about a record it already has: {:?}",
        served
    );
    assert!(group.is_empty(), "bytes reached the group sink: {group:?}");
    assert_eq!(direct.len(), 1);
    // And the reply the asker gets is the flush-stripped copy — the same rule the
    // ephemeral-port leg uses, which is why both go through `respond::unicast_of`.
    let logger = Logger::named("Aggregate-QuFlush");
    let message = mdns::parse_message(&direct[0].0, "t", &logger).expect("the unicast parses");
    assert!(
        message.records.iter().all(|r| !r.flush),
        "the QU unicast leg kept cache-flush bits: {:?}",
        message.records.iter().map(|r| r.flush).collect::<Vec<_>>()
    );
    assert_eq!(
        direct[0].1.port(),
        MDNS_PORT,
        "the QU reply went back somewhere other than the asker"
    );
}

#[test]
fn a_qu_question_from_an_ephemeral_port_is_not_routed_by_the_qu_rule() {
    crate::pin_test_dir();
    // `if not ucast_source and is_unicast` — the QU branch is guarded by the source
    // port first. A QU flag on a packet that already arrived by unicast is answered the
    // ordinary way (direct copy **plus** the bucketed multicast copy), because the asker
    // is one host and the group still needs the record. Dropping that guard would take
    // the multicast copy away from every legacy unicast querier — the corpus's
    // `ptr-ephemeral-src` row, which the aggregation round kept its pair for.
    let service = service();
    let announcement = service.announcement();
    let query = qu_query(&service.service_type, mdns::TYPE_PTR);
    let heard = |out: &mut Outgoing| {
        let logger = Logger::named("Aggregate-QuEphemeral");
        let message = mdns::parse_message(&announcement, "t", &logger).expect("parses");
        out.heard.learn(&message.records, 0);
    };
    let (served, group, direct) = served_from(&service, &query, 500, 600, 55_759, heard);
    assert_eq!(served.direct, 1, "the ephemeral querier got no direct copy");
    assert_eq!(direct[0].1.port(), 55_759, "the direct reply was sent elsewhere");
    // **The bucket path, with the ledger's answer.** The group copy of the same reply is
    // queued — and queued into the *protected* queue, because the record was heard 500 ms
    // ago — so it is not on the wire at +100 ms. Asking the QU route instead would have
    // produced `direct = 1, queued = 0`: exactly one copy, and a group that never learns.
    assert_eq!(
        served.queued, 1,
        "the group copy vanished: {:?}",
        served
    );
    assert!(
        group.is_empty(),
        "a protected answer reached the wire at +100 ms: {group:?}"
    );
    let (_again, group, _direct) = served_from(&service, &query, 500, 1_600, 55_759, heard);
    assert_eq!(
        group.len(),
        1,
        "the group copy never arrived, so this is a missing packet, not a delay"
    );
}

#[test]
fn one_planned_a_record_is_one_record_per_advertisable_address() {
    crate::pin_test_dir();
    // The expansion behind [`plan_identity`]'s `Vec` return: `Chosen` carries no
    // address (`respond::datagram`'s own comment says the plan decides *whether* the
    // host rides along, not which of several), so one planned `A` answer is N records
    // on the wire and N identities in the ledger. A laptop with one interface cannot
    // tell this from a port that truncates the list to the first address — which is
    // why the service here has **two**, and why the two keys have to differ.
    //
    // The same asymmetry the reference cannot express in this port: it buckets each A
    // record on its own, while a single `Chosen` gets a single bucket. `last_heard`
    // takes the newest sighting of the expansions (the protective side), and
    // `a_heard_answer_protects_every_address_it_named` asks what that means on the
    // wire.
    let one = service();
    let mut service = one.clone();
    service.addresses = vec![
        "192.0.2.1".parse().expect("first"),
        "192.0.2.2".parse().expect("second"),
    ];
    let record = answer(Kind::A, &service.server, None);
    let keys: Vec<String> = plan_identity(&record, &service)
        .iter()
        .map(|key| key.as_str().to_string())
        .collect();
    assert_eq!(
        keys.len(),
        2,
        "one planned A record became {} ledger entries for a two-address service",
        keys.len()
    );
    assert_ne!(keys[0], keys[1], "the two addresses share one identity");
    // And a single-address service still costs one entry — the `Vec` is not standing
    // in for a bug that fires whenever there is exactly one interface.
    assert_eq!(plan_identity(&record, &one).len(), 1);
}

#[test]
fn a_heard_answer_protects_every_address_it_named() {
    crate::pin_test_dir();
    // Reading the other half of the expansion: the looped-back copy carries both A
    // records, so both keys enter the ledger, and the TXT riding in the same packet is
    // what a later TXT question is measured against. Asserted through the packet's
    // record count rather than through a bucket, because the bucket rule with one
    // answer per packet is already `a_query_the_wire_just_answered_waits_a_second…`.
    let mut service = service();
    service.addresses = vec![
        "192.0.2.1".parse().expect("first"),
        "192.0.2.2".parse().expect("second"),
    ];
    let logger = Logger::named("Aggregate-TwoAddresses");
    let message = mdns::parse_message(&service.announcement(), "t", &logger)
        .expect("a two-address announcement parses");
    let mut ledger = Ledger::default();
    let touched = ledger.learn(&message.records, 0);
    assert_eq!(
        touched,
        5,
        "the ledger learned {} records from a two-address packet, not the five it carries",
        touched
    );
    for address in ["192.0.2.1", "192.0.2.2"] {
        let record = answer(Kind::A, &service.server, None);
        let key = crate::aggregate::RecordKey::new(
            Kind::A,
            crate::mdns::CLASS_IN,
            &service.server,
            Some(address),
        );
        assert!(
            plan_identity(&record, &service)
                .iter()
                .any(|held| held == &key),
            "the plan does not name {address}"
        );
        assert_eq!(ledger.seen(&key, 500), Some(0), "{address} was not heard");
    }
}

#[test]
fn a_response_that_also_carries_questions_is_ingested_and_not_answered() {
    crate::pin_test_dir();
    // The dispatch is the **QR bit**, not "were there questions" — and this row is the
    // only thing that can tell the two apart, because an ordinary announcement has no
    // questions in it and both readings agree there. `incoming.py` counts questions and
    // records independently, and `_listener.py:326` tests only the flag, so a packet
    // that does both is a response. A responder keyed on the question list would answer
    // its own announcements as soon as another receiver's reply happened to carry a
    // question — which is the flood this whole module is about.
    let service = service();
    let logger = Logger::named("Aggregate-QR");
    let mut response = mdns::query(&service.instance, mdns::TYPE_TXT).expect("encodes");
    // One question, one record, and the QR bit set: a packet that asks *and* claims to
    // answer. The record is what makes the two readings differ in the other direction
    // too — without it "ingested" would be zero for a reason that has nothing to do
    // with the dispatch.
    let parsed = mdns::parse_message(&service.announcement(), "t", &logger).expect("parses");
    let txt = parsed
        .records
        .iter()
        .find(|r| r.rtype == mdns::TYPE_TXT)
        .expect("the announcement carries a TXT");
    let mut extra = Vec::new();
    write_record(&mut extra, txt);
    response.extend_from_slice(&extra);
    response[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT = 1
    response[2] |= 0x80; // QR: this is a response
    let (served, sent) = served_with_ledger(&service, &response, 0, 5_000, |_| {});
    assert_eq!(served.queries, 0, "a response with a question in it was answered");
    assert_eq!(served.group + served.direct + served.queued, 0);
    assert!(sent.is_empty(), "bytes went out for a packet that was not a query");
    assert_eq!(
        served.ingested, 1,
        "the QR dispatch is right but the ledger still missed it: {:?}",
        served
    );
}

#[test]
fn a_query_the_wire_just_answered_waits_a_second_while_a_fresh_one_does_not() {
    crate::pin_test_dir();
    // The whole point of the round, end to end through this port's own code path:
    // hear our own TXT, then answer a TXT question with it — and the answer must
    // arrive **a second later** than the same answer does with an empty ledger. Both
    // halves are asserted with the same flush moment, so the only variable is the
    // hearing. This is reading ④ reproduced rather than quoted.
    let service = service();
    let announcement = service.announcement();
    let query = mdns::query(&service.instance, mdns::TYPE_TXT).expect("the query encodes");
    let fill = |out: &mut Outgoing| {
        let logger = Logger::named("Aggregate-ProtectedQuery");
        let message = mdns::parse_message(&announcement, "t", &logger).expect("parses");
        out.heard.learn(&message.records, 0);
    };
    // t = 500, inside the window; flush at 600, which is enough for the ordinary
    // queue (520) and nowhere near the protected one (1520).
    let (heard, heard_sent) = served_with_ledger(&service, &query, 500, 600, fill);
    assert_eq!(heard.queued, 1, "the answer cost nothing at all");
    assert_eq!(heard.group, 0, "a protected answer was written on the spot");
    assert!(
        heard_sent.is_empty(),
        "the protected answer came out before its one-second floor"
    );
    let (fresh, fresh_sent) = served_with_ledger(&service, &query, 500, 600, |_| {});
    assert_eq!(fresh.queued, 1);
    assert_eq!(
        fresh_sent.len(),
        1,
        "the same query with nothing heard is queued too, and nothing came out"
    );
    // And the protected one does arrive, one second later: a floor is not a veto.
    let (later, later_sent) = served_with_ledger(&service, &query, 500, 1_600, fill);
    assert_eq!(later.queued, 1);
    assert_eq!(later_sent.len(), 1, "the protected answer never reached the wire");
}

#[test]
fn a_probe_is_answered_now_even_for_a_record_the_wire_just_carried() {
    crate::pin_test_dir();
    // `query_handler.py:110`: the probe branch comes **first**, ahead of the
    // last-second check. A probe is an address conflict, and waiting a second to say
    // "I have this name too" is how two machines end up silently sharing one host
    // record. Reordering the two branches is invisible on every single-question corpus
    // row (no row carries an authority section), so this is the only thing that can
    // ask it — and it is read from the source rather than measured, which is said
    // plainly in `bucket`'s comment and here.
    let service = service();
    let announcement = service.announcement();
    let mut query = mdns::query(&service.instance, mdns::TYPE_TXT).expect("encodes");
    // Turn the query into a probe: one authority record, the TXT the conflict names.
    let mut packet = query.clone();
    let authority = {
        let logger = Logger::named("Aggregate-Probe");
        let parsed = mdns::parse_message(&announcement, "t", &logger).expect("parses");
        let record = &parsed.records[1];
        record.clone()
    };
    let mut extra = Vec::new();
    write_record(&mut extra, &authority);
    packet.extend_from_slice(&extra);
    packet[6..8].copy_from_slice(&0u16.to_be_bytes()); // ANCOUNT stays 0
    packet[8..10].copy_from_slice(&1u16.to_be_bytes()); // NSCOUNT = 1 → a probe
    query = packet;
    let fill = |out: &mut Outgoing| {
        let logger = Logger::named("Aggregate-Probe2");
        let message = mdns::parse_message(&announcement, "t", &logger).expect("parses");
        out.heard.learn(&message.records, 0);
    };
    let (served, sent) = served_with_ledger(&service, &query, 500, 501, fill);
    assert_eq!(served.queries, 1, "the probe was not read as a query");
    assert_eq!(served.group, 1, "a probe's answer was queued instead of sent");
    assert_eq!(served.queued, 0, "a probe reached the protected queue");
    assert_eq!(sent.len(), 1);
}

/// Re-emit one parsed record in the **authority** section, uncompressed.
///
/// Small and explicit rather than reusing the announcement's bytes: the only thing
/// this test needs from the record is "an authority section exists with something in
/// it", and a helper that rebuilt the whole packet would hide the NSCOUNT edit that
/// is the actual point.
fn write_record(out: &mut Vec<u8>, record: &mdns::Record) {
    for label in record.name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&record.rtype.to_be_bytes());
    let class: u16 = if record.flush { 0x8001 } else { 1 };
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&record.ttl.to_be_bytes());
    let rdata = match &record.rdata {
        mdns::RData::A(address) => address.octets().to_vec(),
        mdns::RData::Txt(bytes) => bytes.clone(),
        mdns::RData::Name(name) => {
            let mut bytes = Vec::new();
            for label in name.trim_end_matches('.').split('.') {
                if label.is_empty() {
                    continue;
                }
                bytes.push(label.len() as u8);
                bytes.extend_from_slice(label.as_bytes());
            }
            bytes.push(0);
            bytes
        }
        mdns::RData::Srv {
            priority,
            weight,
            port,
            target,
        } => {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&priority.to_be_bytes());
            bytes.extend_from_slice(&weight.to_be_bytes());
            bytes.extend_from_slice(&port.to_be_bytes());
            for label in target.trim_end_matches('.').split('.') {
                if label.is_empty() {
                    continue;
                }
                bytes.push(label.len() as u8);
                bytes.extend_from_slice(label.as_bytes());
            }
            bytes.push(0);
            bytes
        }
    };
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(&rdata);
}

/// A question with the **QU** bit set on its class field (`_QU_BIT_MASK`).
fn qu_query(name: &str, rtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // flags: query
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&(crate::mdns::CLASS_IN | 0x8000).to_be_bytes());
    out
}

// ---------------------------------------------------------------------------
// Helpers that reach `serve` with a ledger that may already be filled
// ---------------------------------------------------------------------------

/// Drive `serve` at `now_ms` from a peer on `peer_port`, then flush the queues at
/// `flush_ms`, and hand back what reached each sink.
///
/// `fill` exists so a test can put the ledger in the state it wants **before** the
/// query is served — the same order the engine does it in, since a datagram is ingested
/// on the tick it arrives and an answer is bucketed on the tick it is asked.
fn served_from(
    service: &Service,
    data: &[u8],
    now_ms: u64,
    flush_ms: u64,
    peer_port: u16,
    fill: impl FnOnce(&mut Outgoing),
) -> (
    Served,
    Vec<Vec<u8>>,
    Vec<(Vec<u8>, SocketAddr)>,
) {
    let logger = Logger::named("Aggregate-Serve");
    let group: std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let direct: std::rc::Rc<std::cell::RefCell<Vec<(Vec<u8>, SocketAddr)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut queues = Outgoing::with_random(Box::new(|| RANDOM_MIN_MS));
    fill(&mut queues);
    let mut multicast = {
        let group = std::rc::Rc::clone(&group);
        move |bytes: &[u8]| {
            group.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
    };
    let mut unicast = {
        let direct = std::rc::Rc::clone(&direct);
        move |bytes: &[u8], peer: SocketAddr| {
            direct.borrow_mut().push((bytes.to_vec(), peer));
            Ok(())
        }
    };
    let from: SocketAddr = format!("192.0.2.9:{peer_port}").parse().expect("a peer");
    let served = announce::serve(
        data,
        from,
        service,
        0,
        now_ms,
        &mut multicast,
        &mut unicast,
        &mut queues,
        &logger,
    );
    // The flush `run` would do on a later tick, rendered through the product's own
    // `datagram_for` — a batch only this helper could render is a batch the engine
    // would drop.
    for batch in queues.due(flush_ms) {
        if let Some(bytes) = announce::datagram_for(&batch, &[service]) {
            group.borrow_mut().push(bytes);
        }
    }
    let group_out = {
        let mut held = group.borrow_mut();
        std::mem::take(&mut *held)
    };
    let direct_out = {
        let mut held = direct.borrow_mut();
        std::mem::take(&mut *held)
    };
    (served, group_out, direct_out)
}

/// [`served_from`] with the ordinary group-port peer and the direct sink dropped, which
/// is what most of the rows above only want to ask about.
fn served_with_ledger(
    service: &Service,
    data: &[u8],
    now_ms: u64,
    flush_ms: u64,
    fill: impl FnOnce(&mut Outgoing),
) -> (Served, Vec<Vec<u8>>) {
    let (served, group, _direct) =
        served_from(service, data, now_ms, flush_ms, MDNS_PORT, fill);
    (served, group)
}
