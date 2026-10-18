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
    property_bytes, FLAGS_ANNOUNCE, HOST_TTL, OTHER_TTL, SERVICE_TYPE, TYPE_A, TYPE_NSEC,
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

/// When the reference lets this answer go out. Carried because the choice is part of
/// the protocol (`mcast_now` vs the aggregation queue), asserted by no test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// Sent immediately to the group.
    Now,
    /// Queued behind `U(20, 120)` ms so concurrent answers aggregate.
    Aggregate,
    /// The protected queue: we already multicast this record in the last second, so
    /// RFC 6762 §14 says wait another one.
    AggregateProtected,
    /// Unicast straight back to the asker.
    Direct,
}

/// One datagram's worth of records, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub records: Vec<Chosen>,
    /// The unicast copy. The reference writes it with **every cache-flush bit
    /// cleared** — the corpus's `ptr-ephemeral-src` row holds both copies of the
    /// same five records in one capture, one set `flush=True` (the group) and one
    /// set `flush=False` (the reply to `127.0.0.1:55759`). A port that reused the
    /// multicast records would send flush bits the reference never sends.
    pub unicast: bool,
    pub timing: Timing,
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
struct Group {
    answer: Chosen,
    additionals: Vec<Chosen>,
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
            vec![Group { answer, additionals }]
        }
        // The enumeration answer is one PTR naming the *type*, with no additionals —
        // the corpus's `enum` row carries exactly one record, owner
        // `_services._dns-sd._udp.local.`, alias our service type.
        Kind::Ptr if question.name == ENUM_TYPE => vec![Group {
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
                answer: record(Kind::Srv, Section::Answer, &instance, HOST_TTL, true),
                additionals,
            }]
        }
        // A TXT question answers with TXT and nothing else: no A, no NSEC. The
        // corpus's `txt` row is a single record — an "obvious" addition of the host
        // pair here would be a divergence nobody measured.
        Kind::Txt if named_instance => vec![Group {
            answer: record(Kind::Txt, Section::Answer, &instance, OTHER_TTL, true),
            additionals: Vec::new(),
        }],
        Kind::A if question.name == server && shape.has_v4 => vec![Group {
            answer: record(Kind::A, Section::Answer, &server, HOST_TTL, true),
            additionals: vec![record(Kind::Nsec, Section::Additional, &server, HOST_TTL, true)],
        }],
        // "You asked for a v6 address of a name that exists and has none" is a
        // **positive** answer: the NSEC says so. The corpus's `aaaa` row proves the
        // reference does not go silent here, which is the difference between
        // "no such record" and "I am not listening".
        Kind::Aaaa if question.name == server => vec![Group {
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
fn flatten(groups: &[Group]) -> Vec<Chosen> {
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

/// Which timing the reference would pick for the **group** leg of an answer. The
/// direct leg is `Timing::Direct` by definition and never comes through here.
///
/// Carried because the choice is part of the protocol, asserted only by shape
/// (`the_unicast_leg_is_direct_and_the_group_leg_is_not`): the millisecond figures
/// belong to this machine's timer, not to mDNS.
fn timing(
    questions: &[Question],
    records: &[Chosen],
    last_mcast_ms: Option<u64>,
    now_ms: u64,
) -> Timing {
    // A QU question from port 5353 is not answered directly unless we already
    // multicast the record within a quarter of its TTL: `ptr-qu` went to the group
    // at +0.3 ms, which is `add_qu_question_response`'s "not recent ⇒ multicast so
    // every peer's cache learns" branch.
    if let Some(created) = last_mcast_ms {
        if now_ms.saturating_sub(created) < 1000 {
            return Timing::AggregateProtected;
        }
    }
    let single = questions.len() == 1;
    if single
        && !records.is_empty()
        && records.iter().all(|record| record.kind.is_immediate())
    {
        return Timing::Now;
    }
    Timing::Aggregate
}

/// Everything the responder sends for one packet's questions.
///
/// `ucast_source` is `source port != 5353` — a property of the datagram, not of the
/// question, which is why it is a parameter and not something derived here. When it
/// is true the reference answers **both** ways: the direct copy with the cache-flush
/// bits cleared, and the usual multicast copy. `last_mcast_ms` / `now_ms` feed the
/// "did we just shout this" checks; `None` means "not recently", which is what a
/// caller with no send history should pass.
pub fn respond(
    questions: &[Question],
    known: &[Known],
    ucast_source: bool,
    shape: &Shape,
    last_mcast_ms: Option<u64>,
    now_ms: u64,
) -> Vec<Reply> {
    let mut groups: Vec<Group> = Vec::new();
    for question in questions {
        for group in suppress(answers(question, shape), known) {
            if !groups.iter().any(|held| *held == group) {
                groups.push(group);
            }
        }
    }
    if groups.is_empty() {
        return Vec::new();
    }
    let records = flatten(&groups);
    // The multicast leg of a unicast-sourced query is bucketed exactly as any other
    // answer would be — the corpus's `ptr-ephemeral-src` pair is a direct reply at
    // +0.3 ms and the group copy at +110.4 ms, i.e. it went through the aggregation
    // queue like a QM query did.
    let group_reply = timing(questions, &records, last_mcast_ms, now_ms);
    let mut replies = Vec::new();
    if ucast_source {
        let mut direct = records.clone();
        for record in direct.iter_mut() {
            // The reference's unicast leg clears every cache-flush bit: the corpus
            // holds both copies of the same five records in one capture, one set
            // `flush=True`, one `flush=False`.
            record.flush = false;
        }
        replies.push(Reply {
            records: direct,
            unicast: true,
            timing: Timing::Direct,
        });
    }
    replies.push(Reply {
        records,
        unicast: false,
        timing: group_reply,
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
    let mut w = Writer::new(FLAGS_ANNOUNCE);
    let txt = service.txt.as_slice();
    let addresses = service.addresses.as_slice();
    let server = service.server.as_str();
    let port = service.port;
    for record in records {
        let unique = record.flush;
        match record.kind {
            Kind::Ptr => {
                let alias = record
                    .alias
                    .as_deref()
                    .unwrap_or(record.owner.as_str());
                emit(&mut w, record.section, &record.owner, TYPE_PTR, record.ttl, unique, |w| {
                    w.name(alias)
                });
            }
            Kind::Srv => emit(&mut w, record.section, &record.owner, TYPE_SRV, record.ttl, unique, |w| {
                w.u16(0);
                w.u16(0);
                w.u16(port);
                w.name(server);
            }),
            Kind::Txt => emit(&mut w, record.section, &record.owner, TYPE_TXT, record.ttl, unique, |w| {
                for (key, value) in txt {
                    let body = property_bytes(key, value);
                    w.u8(body.len() as u8);
                    w.octets(&body);
                }
            }),
            Kind::A => {
                for address in addresses {
                    emit(&mut w, record.section, &record.owner, TYPE_A, record.ttl, unique, |w| {
                        w.octets(&address.octets());
                    });
                }
            }
            Kind::Nsec => emit(&mut w, record.section, &record.owner, TYPE_NSEC, record.ttl, unique, |w| {
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
    w.finish()
}

/// Write one record into the section the plan chose.
fn emit<F>(w: &mut Writer, section: Section, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
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
