//! Deciding what to answer when somebody asks.
//!
//! The announcement side ([`crate::announce"]) shouts the service at the group
//! three times and then goes quiet for [`crate::announce::REANNOUNCE`]. This
//! module is the other half of discoverability: a phone that **asks** — the Cast
//! app, `dns-sd -B`, any browser with a cold cache — used to wait up to a minute
//! for the next burst, because nothing here answered.
//!
//! ## Where every rule in this file comes from
//!
//! Not from RFC 6762 and not from reading `zeroconf`. `scripts/mdns_responder_probe.py`
//! starts a real loopback-only `Zeroconf`, registers the service the product itself
//! would register, sends sixteen *shapes* of query at it, and records the datagrams
//! the reference meant to put on the wire. Those rows are frozen in
//! `macast-cast/tests/golden/mdns_responder.jsonl`, and `respond_tests.rs` diffs this
//! module against every one of them. Two consequences of that method are worth
//! stating before the code:
//!
//! * **The corpus is a decoded table, not bytes.** The reference lays its additionals
//!   out in set order, so the same question emitted `TXT/SRV/A/NSEC` in one run and
//!   `SRV/TXT/A/NSEC` in another, and `A/TXT/SRV/NSEC` for a different case in the
//!   first run (`docs/Rust-Port-Design.md` §14.6 carries the readings). Byte parity
//!   would therefore be a test that goes red on the machine's hash seed. So this
//!   module returns **records**, in a deterministic order of our own, and the writer
//!   layer that turns them into bytes is the next step.
//! * **A timing decision is in here because the reference makes one**, but nothing
//!   asserts it: `{SRV, A, AAAA, NSEC}` go out immediately, everything else through
//!   an aggregation queue with a `U(20, 120)` ms delay. That is scheduling, and a
//!   test that pinned a millisecond would be testing this machine's timer.
//!
//! ## The two rules that have no visible effect unless you look for them
//!
//! * Suppression compares TTLs with a **strict `>`**: `other.ttl > record.ttl / 2`.
//!   A querier claiming exactly half (`2250` of a `4500`-second PTR) must still be
//!   answered. The corpus row `ptr-known-half` is the only witness of that boundary
//!   anywhere in this workspace — with `>=`, a port would match every other reading
//!   of the rule and go green.
//! * A known PTR matches by **record equality, and a PTR's equality includes its
//!   alias**. `ptr-known-other-alias` feeds a record with our name and our TTL that
//!   points at a *different* instance, and the reference answers. A port that keyed
//!   suppression on `(name, type)` alone would go silent there.

use crate::announce::{
    FLAGS_ANNOUNCE, HOST_TTL, OTHER_TTL, SERVICE_TYPE, TYPE_A, TYPE_NSEC,
    TYPE_PTR, TYPE_SRV, TYPE_TXT, Writer,
};
use crate::announce::Service;

/// The enumeration question's owner name, spelled the way the reference answers.
///
/// `*._dns-sd._udp.local.` — RFC 6763's DQE form — is **not** answered: the corpus
/// row `enum-star` produced nothing at all. Keeping the unanswered spelling in a doc
/// comment rather than a constant is deliberate: the day someone adds it, that row
/// stops matching and the diff test says so.
pub const ENUM_TYPE: &str = "_services._dns-sd._udp.local.";

/// A record type, named by what it answers rather than by its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Ptr,
    Srv,
    Txt,
    A,
    Aaaa,
    Nsec,
    /// `TYPE_ANY` (255) over an instance name. It is its own variant rather than a
    /// fall-through in [`answers`] because the reference's answer to ANY is *not*
    /// the union of everything else: the corpus's `any` row has no PTR in it.
    Any,
}

impl Kind {
    /// `zeroconf`'s `_RESPOND_IMMEDIATE_TYPES` is `{NSEC, SRV, A, AAAA}`: a single
    /// question of one of those types is answered without waiting for the
    /// aggregation window. PTR and TXT are not in it, which is why the corpus's
    /// `ptr` row went out at +92 ms and its `a` row at +0.3 ms.
    pub fn is_immediate(&self) -> bool {
        matches!(self, Kind::Srv | Kind::A | Kind::Aaaa | Kind::Nsec)
    }

    /// The two types whose "answer" is a statement about what does *not* exist.
    /// They still get a datagram — silence is not how mDNS says "no such type".
    pub fn is_address(&self) -> bool {
        matches!(self, Kind::A | Kind::Aaaa)
    }
}

/// Which header section a record belongs to. The reference puts the answer records
/// in AN and everything that rides along in AR; the corpus rows carry that split,
/// so a port that folded them together changes the byte counts a client reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Answer,
    Additional,
}

/// One record this responder would put in a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub kind: Kind,
    pub section: Section,
    /// The record's owner name, exactly as the corpus prints it (trailing dot included).
    pub owner: String,
    pub ttl: u32,
    /// The cache-flush bit. See [`Reply::unicast`]: the reference clears it on the
    /// unicast copy of the same answer.
    pub flush: bool,
    /// For a PTR: the instance or type it points at. It is part of *equality* for
    /// suppression, and it is why `Known` carries the field too.
    pub alias: Option<String>,
}

/// A record the querier claims to already hold, read from the query's answer section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub kind: Kind,
    pub owner: String,
    pub ttl: u32,
    pub alias: Option<String>,
}

impl Known {
    /// Suppression matches records, not names — and for a PTR that means the alias
    /// too (`_dns.py:609-615` looks the record up by equality).
    fn matches(&self, record: &Chosen) -> bool {
        self.kind == record.kind
            && self.owner == record.owner
            && match (self.alias.as_ref(), record.alias.as_ref()) {
                (Some(mine), Some(theirs)) => mine == theirs,
                (None, None) => true,
                _ => false,
            }
    }
}

/// A question as it arrives on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub kind: Kind,
    /// The cache-flush bit **in the question's class field** — which is where mDNS
    /// puts the unicast-request flag, and the reference reads it as exactly that
    /// (`is_unicast = question.unique`, `query_handler.py:236`).
    pub unicast_request: bool,
}

/// What the service looks like to an asker: the three names and whether we have an
/// address at all. A [`Shape`] with no v4 address answers nothing about `A`, which
/// is the state a machine with no advertisable interface is in — the same state
/// [`crate::announce::Announcer::start`] refuses to announce from.
#[derive(Clone, Debug)]
pub struct Shape<'a> {
    pub instance: &'a str,
    pub server: &'a str,
    pub service_type: &'a str,
    pub has_v4: bool,
}

impl<'a> Shape<'a> {
    pub fn new(instance: &'a str, server: &'a str, has_v4: bool) -> Shape<'a> {
        Shape {
            instance,
            server,
            service_type: SERVICE_TYPE,
            has_v4,
        }
    }
}

/// When the reference lets an **answer record** of the group leg go out.
///
/// One bucket per answer, not one per reply: `query_handler.py:106-125` loops over
/// the answers and `continue`s into whichever bucket each one falls in, so an `ANY`
/// reply whose TXT was multicast a moment ago and whose SRV was not is **split**
/// across two queues. A reply-level choice could not express that, and the record
/// that would have been protected goes out on the fast path instead.
///
/// `Direct` is deliberately absent: the unicast leg is [`Reply::unicast`], decided
/// by the source port before any bucketing happens, and the reference's fourth
/// bucket (`_ucast`) never mixes with these three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// Sent immediately to the group.
    Now,
    /// Queued behind `U(20, 120)` ms so concurrent answers aggregate.
    Aggregate,
    /// The protected queue: we already multicast this record in the last second, so
    /// RFC 6762 §14 says wait another one.
    AggregateProtected,
}

/// One datagram's worth of records, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub records: Vec<Chosen>,
    /// The same answers as [`Reply::records`], but keeping **which additionals ride
    /// with which answer**. The flat list is what the wire needs and what the corpus
    /// pins; the grouping is what the aggregation queue needs, because its merge and
    /// dedupe rules are keyed on the answer record and carry that answer's
    /// additionals with it (`_remove_answers_from_queue` pops *keys*).
    ///
    /// `records` is always `flatten(groups)`, asserted by
    /// `the_flat_records_are_exactly_the_flattened_groups` — two fields, one builder,
    /// so neither can drift into being a second answer about the same query.
    pub groups: Vec<Group>,
    /// The unicast copy. The reference writes it with **every cache-flush bit
    /// cleared** — the corpus's `ptr-ephemeral-src` row holds both copies of the
    /// same five records in one capture, one set `flush=True` (the group) and one
    /// set `flush=False` (the reply to `127.0.0.1:55759`). A port that reused the
    /// multicast records would send flush bits the reference never sends.
    pub unicast: bool,
}

/// One answer record and the records that ride out with it.
///
/// The grouping is not bookkeeping: in the reference, additionals are attached
/// **to an answer record**, so suppressing that answer takes its additionals with it.
/// The corpus's `ptr-known-live` row is the witness — the asker held our PTR with a
/// full TTL, and the capture is **zero bytes**, not "a packet with four additionals
/// and no answer". A flat list here would have sent exactly that, which is the
/// flooding the suppression rule exists to prevent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub answer: Chosen,
    pub additionals: Vec<Chosen>,
    /// Did the **question that earned this answer** ask for a unicast response
    /// (`_QU_BIT_MASK` on the class field, `question.unique`)?
    ///
    /// Carried on the group rather than decided here because the reference carries it
    /// on the question and branches on it in `async_response`
    /// (`if not ucast_source and is_unicast: add_qu_question_response(...)`): a QU
    /// question that reached us over the **group** is answered by unicast back to the
    /// asker, *unless* we have not sent that record within a quarter of its TTL, in
    /// which case the whole group is supposed to hear it (RFC 6762 §5.4). That is a
    /// different bucket set from the multicast one — not a re-timing of it — so it has
    /// to be knowable per answer.
    ///
    /// Stamped by [`respond`] rather than by the answer table, because `answers_any`
    /// builds groups for a question the reader of it should not have to name: the flag
    /// describes why an answer exists, and one writer is enough.
    pub from_qu: bool,
}

/// What one question is worth, as answer records plus the records that ride with
/// each one.
///
/// Order is ours (answers in the order the reference lists them, additionals after),
/// because the reference's order is a `set`. The `ptr` case's additionals in the
/// corpus are SRV, TXT, A, NSEC; `respond_tests` compares sorted sets, so nothing
/// here claims a sequence.
fn answers(question: &Question, shape: &Shape) -> Vec<Group> {
    let instance = shape.instance.to_string();
    let server = shape.server.to_string();
    let record = |kind: Kind, section: Section, owner: &str, ttl: u32, flush: bool| Chosen {
        kind,
        section,
        owner: owner.to_string(),
        ttl,
        flush,
        alias: None,
    };
    // The host pair: an A and the NSEC that says "this name has no other type".
    // Nothing may name a server we have no address for, so `has_v4` gates it.
    let host_pair = |extra: &mut Vec<Chosen>| {
        if shape.has_v4 {
            extra.push(record(Kind::A, Section::Additional, &server, HOST_TTL, true));
            extra.push(record(Kind::Nsec, Section::Additional, &server, HOST_TTL, true));
        }
    };
    let named_instance = question.name == instance;
    match question.kind {
        Kind::Ptr if question.name == shape.service_type => {
            let answer = Chosen {
                kind: Kind::Ptr,
                section: Section::Answer,
                owner: shape.service_type.to_string(),
                ttl: OTHER_TTL,
                // The PTR is shared with every other Chromecast on the LAN, so it
                // is not "ours alone" and cannot claim the cache-flush bit; the
                // corpus prints `flush=False` for it and `True` for all four riders.
                flush: false,
                alias: Some(instance.clone()),
            };
            let mut additionals = vec![
                record(Kind::Srv, Section::Additional, &instance, HOST_TTL, true),
                record(Kind::Txt, Section::Additional, &instance, OTHER_TTL, true),
            ];
            host_pair(&mut additionals);
            vec![Group {
                answer,
                additionals,
                from_qu: false,
            }]
        }
        // The enumeration answer is one PTR naming the *type*, with no additionals —
        // the corpus's `enum` row carries exactly one record, owner
        // `_services._dns-sd._udp.local.`, alias our service type.
        Kind::Ptr if question.name == ENUM_TYPE => vec![Group {
            from_qu: false,
            answer: Chosen {
                kind: Kind::Ptr,
                section: Section::Answer,
                owner: ENUM_TYPE.to_string(),
                ttl: OTHER_TTL,
                flush: false,
                alias: Some(shape.service_type.to_string()),
            },
            additionals: Vec::new(),
        }],
        Kind::Srv if named_instance => {
            let mut additionals = Vec::new();
            host_pair(&mut additionals);
            vec![Group {
                from_qu: false,
                answer: record(Kind::Srv, Section::Answer, &instance, HOST_TTL, true),
                additionals,
            }]
        }
        // A TXT question answers with TXT and nothing else: no A, no NSEC. The
        // corpus's `txt` row is a single record — an "obvious" addition of the host
        // pair here would be a divergence nobody measured.
        Kind::Txt if named_instance => vec![Group {
            from_qu: false,
            answer: record(Kind::Txt, Section::Answer, &instance, OTHER_TTL, true),
            additionals: Vec::new(),
        }],
        Kind::A if question.name == server && shape.has_v4 => vec![Group {
            from_qu: false,
            answer: record(Kind::A, Section::Answer, &server, HOST_TTL, true),
            additionals: vec![record(Kind::Nsec, Section::Additional, &server, HOST_TTL, true)],
        }],
        // "You asked for a v6 address of a name that exists and has none" is a
        // **positive** answer: the NSEC says so. The corpus's `aaaa` row proves the
        // reference does not go silent here, which is the difference between
        // "no such record" and "I am not listening".
        Kind::Aaaa if question.name == server => vec![Group {
            from_qu: false,
            answer: record(Kind::Nsec, Section::Answer, &server, HOST_TTL, true),
            additionals: Vec::new(),
        }],
        Kind::Any => answers_any(&question.name, shape),
        // Anything else — another service type, the `*._dns-sd._udp.local.` star
        // form the reference does not answer, a name we do not own — is silence, and
        // silence with no error: `other-service` and `enum-star` both produced
        // exactly zero datagrams and no log line.
        Kind::Ptr | Kind::Srv | Kind::Txt | Kind::A | Kind::Aaaa | Kind::Nsec => Vec::new(),
    }
}

/// Whether any of these questions asks about records **this** shape holds.
///
/// The announcer's log line needs exactly this question, and it is answered here —
/// by calling the same matcher that produces the answers — rather than by a second
/// copy of the name rules. A copy would be a divergence waiting to happen: the log
/// would claim a query was "about us" on a datagram the decision layer then ignores.
///
/// A question that is ours but fully suppressed by the asker's known answers counts
/// **here**, which is the point: "we were asked and declined" and "we were never
/// asked" are two different facts, and `verify_rust_receiver.sh` §25 has to tell
/// them apart from one line. Measured on this Mac 2026-10-18: a cold python browser
/// and this process were both joined to the group, the LAN also carries queries for
/// `_http._tcp.local.` (36 bytes, not ours, answered by nobody), and a cap that
/// counts *any* query gets spent on that chatter before the browser has asked
/// anything — which is how "the receiving sentence never fired" came to be written
/// down as "this binary receives no inbound multicast". It does.
pub fn concerns(questions: &[Question], shape: &Shape) -> bool {
    questions.iter().any(|question| !answers(question, shape).is_empty())
}

/// ANY over one instance name: TXT and SRV in the answer section, the host pair as
/// additionals on the SRV, and **no PTR** — the corpus's `any` row is four records
/// and a PTR is not among them.
fn answers_any(name: &str, shape: &Shape) -> Vec<Group> {
    if name != shape.instance {
        return Vec::new();
    }
    let instance = shape.instance.to_string();
    let server = shape.server.to_string();
    let mut additionals = Vec::new();
    if shape.has_v4 {
        additionals.push(Chosen {
            kind: Kind::A,
            section: Section::Additional,
            owner: server.clone(),
            ttl: HOST_TTL,
            flush: true,
            alias: None,
        });
        additionals.push(Chosen {
            kind: Kind::Nsec,
            section: Section::Additional,
            owner: server,
            ttl: HOST_TTL,
            flush: true,
            alias: None,
        });
    }
    vec![
        Group {
            from_qu: false,
            answer: Chosen {
                kind: Kind::Txt,
                section: Section::Answer,
                owner: instance.clone(),
                ttl: OTHER_TTL,
                flush: true,
                alias: None,
            },
            additionals: Vec::new(),
        },
        Group {
            from_qu: false,
            answer: Chosen {
                kind: Kind::Srv,
                section: Section::Answer,
                owner: instance,
                ttl: HOST_TTL,
                flush: true,
                alias: None,
            },
            additionals,
        },
    ]
}

/// Does the asker already hold this record well enough that we should stay quiet?
///
/// `other.ttl > record.ttl / 2`, **strictly** — `ptr-known-half` in the corpus is
/// that boundary and nothing else in this workspace is.
fn suppressed_by(held: &Known, record: &Chosen) -> bool {
    held.matches(record) && (held.ttl as f64) > (record.ttl as f64) / 2.0
}

/// Drop the groups whose answer the asker already holds. Suppression is per record
/// (`srv-known-ptr` answers the SRV although the PTR is known), and a dropped group
/// takes its additionals with it (`ptr-known-live` sent nothing at all).
///
/// What is **not** measured, and therefore not implemented: whether a known
/// *additional* suppresses just itself while its answer still goes out. The corpus
/// has no row for it — no case asked a PTR question while holding the SRV — so
/// adding that rule here would be inventing a protocol fact (§10's "先拿证据再动手").
fn suppress(groups: Vec<Group>, known: &[Known]) -> Vec<Group> {
    groups
        .into_iter()
        .filter(|group| !known.iter().any(|held| suppressed_by(held, &group.answer)))
        .collect()
}

/// Answers first, then the surviving groups' additionals, deduplicated: two answers
/// in one packet (the ANY case) must not write the same A record twice.
pub fn flatten(groups: &[Group]) -> Vec<Chosen> {
    let mut out: Vec<Chosen> = groups.iter().map(|group| group.answer.clone()).collect();
    for group in groups {
        for record in &group.additionals {
            if !out.iter().any(|held| held == record) {
                out.push(record.clone());
            }
        }
    }
    out
}

/// Which bucket the reference would put an answer in — `query_handler.py:106-125`
/// (`add_mcast_question_response`), branch for branch, in the source's order. The
/// order is the rule: a probe check that ran after the last-second check would send
/// a probe's just-shouted record on the protected queue instead of now, and the two
/// are 1000 ms apart.
///
/// Asked **once per answer record**, because that is how the reference loops: an
/// `ANY` reply whose TXT was multicast a moment ago and whose SRV was not is split
/// across two queues, and a reply-level answer could only be wrong about one of
/// them. The record-specific input is `last_seen_ms`; the rest of the inputs belong
/// to the packet.
///
/// `probe` is the incoming packet's own property (`_num_authorities > 0`), not a
/// property of this record, which is why it is an argument. `last_seen_ms` is when
/// this responder **heard** this exact record — the caller reads it out of the
/// ledger [`crate::aggregate`] keeps from what the engine's socket receives, and
/// `None` means "never, or not within the window". `None` for a caller with no
/// send history is also the reference's answer for a process that cannot hear
/// itself: the protection is learned from the wire, not remembered from the send
/// (design doc §16's aggregation round — `_has_mcast_record_in_last_second` reads
/// the **cache**, and the cache is filled by the looped-back copy of our own
/// announcement).
pub fn bucket(
    questions: &[Question],
    probe: bool,
    last_seen_ms: Option<u64>,
    now_ms: u64,
) -> Timing {
    if probe {
        return Timing::Now;
    }
    if let Some(created) = last_seen_ms {
        if now_ms.saturating_sub(created) < crate::aggregate::PROTECTION_WINDOW_MS {
            return Timing::AggregateProtected;
        }
    }
    // The branch is on the **question's** type, exactly as `query_handler.py:119`
    // writes it, not on what we answer with. Honest about its own teeth: on every
    // row the frozen corpus holds, the two readings agree — a single `AAAA` question
    // answers with an NSEC and both kinds are immediate, a single `ANY` expands to a
    // TXT and neither rule is satisfied. So this line is not a bug fix and no
    // mutation battery here can make it red; it is written the source's way because
    // a plan layer that grows another answer kind (a second instance's PTR, say)
    // would make the two disagree, and the side that wins should be the one the
    // reference measured. Recorded as **unmeasured** rather than pretending a test
    // asks it.
    if questions.len() == 1 && questions[0].kind.is_immediate() {
        return Timing::Now;
    }
    Timing::Aggregate
}

/// The DNS type number → the kind the decision layer speaks.
///
/// Unknown numbers come back as `None` rather than a guess: a question we cannot
/// name is a question we cannot answer, and inventing a `Kind` for it would make
/// the reply table claim things about types this port does not model.
///
/// This is the one owner. `announce.rs` used to keep a private copy for its log
/// sentence, and a second table of type numbers is how a responder starts
/// describing a question in words the decision layer would not answer.
pub fn kind_of(rtype: u16) -> Option<Kind> {
    match rtype {
        crate::mdns::TYPE_A => Some(Kind::A),
        crate::mdns::TYPE_PTR => Some(Kind::Ptr),
        crate::mdns::TYPE_TXT => Some(Kind::Txt),
        crate::mdns::TYPE_SRV => Some(Kind::Srv),
        28 => Some(Kind::Aaaa),
        47 => Some(Kind::Nsec),
        255 => Some(Kind::Any),
        _ => None,
    }
}

/// The same answers as a **unicast** leg: every cache-flush bit cleared.
///
/// The one owner of that rule, because two branches need it. `ucast_source` (the
/// asker's port was not 5353) builds a direct copy of the whole reply, and
/// `query_handler.py:245-248`'s QU-over-multicast branch sends **one answer record**
/// back to the asker when that record was multicast within a quarter of its TTL — a
/// packet `respond` never sees as a reply, because the question arrived on the group.
/// Both write flush bits the reference does not send: the corpus's `ptr-ephemeral-src`
/// row holds the same five records twice in one capture, once with `flush=True` (the
/// group copy) and once with `flush=False` (the reply to `127.0.0.1:55759`).
pub fn unicast_of(groups: &[Group]) -> Vec<Group> {
    let mut direct: Vec<Group> = groups.to_vec();
    for group in &mut direct {
        group.answer.flush = false;
        for record in &mut group.additionals {
            record.flush = false;
        }
    }
    direct
}

/// Everything the responder sends for one packet's questions.
///
/// `ucast_source` is `source port != 5353` — a property of the datagram, not of the
/// question, which is why it is a parameter and not something derived here. When it
/// is true the reference answers **both** ways: the direct copy with the cache-flush
/// bits cleared, and the usual multicast copy.
///
/// No timing decision is made here. Bucketing is per answer record and needs the
/// ledger (when did we last *hear* this record), which is a fact about the running
/// engine rather than about this packet — so [`bucket`] is asked by
/// `announce::serve`, and this function answers only "which records, in which
/// grouping, on which leg". Keeping it out of here is also what stops the decision
/// from being made once for a whole reply, which is the split the reference makes
/// and a reply-level answer cannot express.
pub fn respond(questions: &[Question], known: &[Known], ucast_source: bool, shape: &Shape) -> Vec<Reply> {
    let mut groups: Vec<Group> = Vec::new();
    for question in questions {
        for mut group in suppress(answers(question, shape), known) {
            // The answer table builds groups without an opinion about who asked; this is
            // the one place that knows the question, so the QU flag is stamped here.
            group.from_qu = question.unicast_request;
            if !groups.iter().any(|held| *held == group) {
                groups.push(group);
            }
        }
    }
    if groups.is_empty() {
        return Vec::new();
    }
    let mut replies = Vec::new();
    if ucast_source {
        let direct = unicast_of(&groups);
        let records = flatten(&direct);
        replies.push(Reply {
            records,
            groups: direct,
            unicast: true,
        });
    }
    // The multicast leg of a unicast-sourced query is bucketed exactly as any other
    // answer would be — the corpus's `ptr-ephemeral-src` pair is a direct reply at
    // +0.3 ms and the group copy at +110.4 ms, i.e. it went through the aggregation
    // queue like a QM query did.
    let records = flatten(&groups);
    replies.push(Reply {
        records,
        groups,
        unicast: false,
    });
    replies
}

/// The name the corpus uses for a wire type number. Unknown numbers print as
/// themselves: a summary that drops an unexpected record type is how a new answer
/// becomes invisible (`mdns.rs`'s own parser holds the same line).
pub fn kind_name(rtype: u16) -> String {
    match rtype {
        1 => "A".to_string(),
        12 => "PTR".to_string(),
        16 => "TXT".to_string(),
        28 => "AAAA".to_string(),
        33 => "SRV".to_string(),
        47 => "NSEC".to_string(),
        other => other.to_string(),
    }
}

/// Assemble one [`Reply`] into a datagram.
///
/// The bytes are built by [`crate::announce`]'s `Writer`, so the name-compression
/// table is the same one the announcement corpus pinned — a second writer here
/// would be a second thing to get wrong, and the reference's answers compress the
/// same way its announcements do.
///
/// Two expansions worth naming, because neither is visible in the record *list*:
///
/// * An `A` choice expands to **one record per address** the service publishes.
///   `Chosen` carries no address because the plan layer decides *whether* the host
///   rides along, not which of several it has; a machine with two advertisable
///   interfaces answers with two A records, which is what the reference does.
/// * The flags are `0x8400` — response plus authoritative — and `id` is 0. Both
///   are read from the captures, not chosen: every answer the reference sent in
///   `mdns_responder.jsonl` came back `id=0 aa=True`.
pub fn datagram(records: &[Chosen], service: &Service) -> Vec<u8> {
    datagram_multi(&[(records, service)])
}

/// Several planned replies assembled into **one** datagram.
///
/// This is what the aggregation queue needs: when two questions' answers were
/// merged into one batch — possibly from two different services on one engine,
/// which is the reference's own shape, since `Zeroconf` has a single pair of
/// outgoing queues for every registration — the reference writes the whole batch
/// with one [`Writer`], and one name-compression table. Two calls to
/// [`datagram`] would be two packets *and* two compression tables, i.e. twice the
/// bytes for the same news.
///
/// The dedupe is not an optimization. `_add_answers_additionals`
/// (`answers.py:98-110`) starts with `sending = set(answers)` and then writes an
/// additional **only if it is not already in `sending`** — so a PTR answer and an
/// SRV answer in one batch do not each bring their own copy of the host pair, and
/// an additional that happens to also be an answer in this batch is dropped
/// wherever it appears in the order. Identity for that test is
/// [`crate::aggregate::RecordKey`]: name/type/class/rdata, **not** the section and
/// **not** the TTL, which is exactly why an `A` that answered an `A` question is
/// suppressed from a PTR's additionals even though one is in AN and the other in AR.
pub fn datagram_multi(parts: &[(&[Chosen], &Service)]) -> Vec<u8> {
    use std::collections::HashSet;
    let mut w = Writer::new(FLAGS_ANNOUNCE);
    let mut sending: HashSet<crate::aggregate::RecordKey> = HashSet::new();
    // Pass one: every answer record claims its identity first, in the reference's
    // `sending = set(answers)`. Doing this before any writing is what makes the
    // suppression independent of the order the two sections happen to be planned in.
    for (records, service) in parts {
        for record in *records {
            if record.section == Section::Answer {
                for key in crate::aggregate::plan_identity(record, service) {
                    sending.insert(key);
                }
            }
        }
    }
    // Pass two: write, dropping any additional whose identity is already going out.
    // One set, not two. `answers.py:100` keeps a single `sending` that starts as the
    // answers and grows with every additional written, and mirroring that is not
    // tidiness: with a second "already written in this part" set, the answer-seeded
    // suppression and the within-batch suppression became two rules that could be
    // broken one at a time — and the battery proved it, because deleting the answer
    // half left the other half doing the same work on every case we had written.
    for (records, service) in parts {
        for record in *records {
            let keys = crate::aggregate::plan_identity(record, service);
            if record.section == Section::Additional {
                // `keys` holds more than one entry only for `A`, where the plan is
                // one record and the wire is one per address: each address is then
                // its own record and is suppressed or written on its own identity.
                let keep: Vec<usize> = keys
                    .iter()
                    .enumerate()
                    .filter(|(_, key)| !sending.contains(*key))
                    .map(|(index, _)| index)
                    .collect();
                if keep.is_empty() {
                    continue;
                }
                emit(&mut w, record, service, &keep);
                for index in keep {
                    sending.insert(keys[index].clone());
                }
            } else {
                emit(&mut w, record, service, &all_indexes(&keys));
                sending.extend(keys);
            }
        }
    }
    w.finish()
}

fn all_indexes(keys: &[crate::aggregate::RecordKey]) -> Vec<usize> {
    (0..keys.len()).collect()
}

/// Write one planned record, restricted to the expanded rdata at `keep`.
fn emit(w: &mut Writer, record: &Chosen, service: &Service, keep: &[usize]) {
    let unique = record.flush;
    let owner = record.owner.as_str();
    let server = service.server.as_str();
    let port = service.port;
    match record.kind {
        Kind::Ptr => {
            let alias = record.alias.as_deref().unwrap_or(owner);
            route(w, record.section, owner, TYPE_PTR, record.ttl, unique, |w| {
                w.name(alias)
            });
        }
        Kind::Srv => route(w, record.section, owner, TYPE_SRV, record.ttl, unique, |w| {
            w.u16(0);
            w.u16(0);
            w.u16(port);
            w.name(server);
        }),
        Kind::Txt => route(w, record.section, owner, TYPE_TXT, record.ttl, unique, |w| {
            // The service's own producer, so the bytes here and the bytes
            // [`crate::aggregate::plan_identity`] names are one computation.
            let body = service.txt_rdata();
            w.octets(&body);
        }),
        Kind::A => {
            // One planned record, N on the wire: `keep` says which addresses this
            // batch may still carry. An index that is not an address means the plan
            // and the service disagree, so it is dropped by `get` rather than
            // panicking on a LAN.
            for index in keep.iter().filter_map(|i| service.addresses.get(*i)) {
                let address = *index;
                route(w, record.section, owner, TYPE_A, record.ttl, unique, |w| {
                    w.octets(&address.octets())
                });
            }
        }
        Kind::Nsec => route(w, record.section, owner, TYPE_NSEC, record.ttl, unique, |w| {
            w.name(server);
            w.u8(0);
            w.u8(1);
            w.u8(0x40);
        }),
        Kind::Any | Kind::Aaaa => {
            // BOTH are question-only types: `AAAA` is answered with an NSEC (see
            // the `aaaa` corpus row), and `ANY` is expanded into TXT and SRV.
            // Nothing should ever choose one as an answer record, and a reply
            // that carries one means the plan layer leaked a question through.
            // Loud, not silent.
            debug_assert!(false, "respond::datagram was handed a question-only kind");
        }
    }
}

/// Write one record into the section the plan chose.
fn route<F>(w: &mut Writer, section: Section, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
where
    F: FnOnce(&mut Writer),
{
    match section {
        Section::Answer => w.record(name, rtype, ttl, unique, rdata),
        Section::Additional => w.record_additional(name, rtype, ttl, unique, rdata),
    }
}

#[cfg(test)]
#[path = "respond_tests.rs"]
mod respond_tests;
