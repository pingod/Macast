//! The two outgoing mDNS queues and the ledger that decides which one an answer
//! goes into — `zeroconf/_handlers/multicast_outgoing_queue.py` plus the cache
//! lookup in `query_handler.py:152-160`.
//!
//! Why this file exists at all: until now this port had **no** queues.
//! `announce::serve` put every answer on the wire the moment it was planned, which
//! is what RFC 6762 §14 calls flooding — a browser that opens a Cast menu asks the
//! same PTR question six times in two seconds and gets six separate datagrams back.
//! The reference spreads that over `U(20, 120)` ms of aggregation, and it does
//! something subtler for a record it has just shouted: it waits a **full second**.
//!
//! The second behaviour is the one that had to be measured before it could be
//! written (design doc §16's aggregation round). `_has_mcast_record_in_last_second`
//! reads the **cache**, and the cache is filled by `_listener.py:326` ingesting
//! every non-query datagram the socket receives — including the copy of our own
//! answer that the kernel loops back. So the reference's flood protection is not
//! "I remember sending this", it is "**I heard this on the wire**". A port that
//! fills the ledger from its own send path is *stronger* than the reference and
//! diverges exactly where the reference is weak (a socket that does not read). This
//! file therefore takes the ledger's words only from what the engine **received**,
//! and [`Ledger::learn`] has one caller.
//!
//! Three things here are deliberately not shared with the rest of the crate:
//!
//! * **The random delay is drawn by the caller** ([`OutgoingQueue::add`] takes the
//!   number). A queue that calls `rand` itself cannot be tested against the four
//!   recorded readings, and "which two questions merged" is the whole content of
//!   those readings.
//! * **Time is passed in**, in the same monotonic milliseconds everywhere in this
//!   module. The reference uses `current_time_millis()` (epoch); nothing here
//!   compares against a wall clock, so a jump in NTP cannot strand a queued answer
//!   or send one a second early.
//! * **Nothing here touches a socket.** [`OutgoingQueue::ready`] hands back the
//!   groups that may go out and `announce` renders and sends them. The scheduling
//!   rule is then testable without binding 5353 — which on this machine is shared
//!   with the OS responder, and is the reason [`crate::announce::run`] takes three
//!   closures for the same job.

use crate::mdns::{RData, Record};
use crate::respond::{Chosen, Kind};
use std::collections::{HashMap, VecDeque};

/// `answers.py:21` — `MULTICAST_DELAY_RANDOM_INTERVAL`. Inclusive on both ends,
/// because the reference uses `random.randint`, and `randint` is inclusive while
/// `random.random() * (max - min)` is not. A port that draws `20 + rand % 100`
/// gets the same *shape* and can never produce 120.
pub const RANDOM_MIN_MS: u64 = 20;
/// See [`RANDOM_MIN_MS`].
pub const RANDOM_MAX_MS: u64 = 120;

/// `_core.py:77` — how long the plain queue may hold an answer before it must go.
pub const AGGREGATION_DELAY_MS: u64 = 500;

/// `_core.py:88` — the same window for the protected queue, and it is **shorter**
/// than the plain one (200 ms) even though that queue exists to wait longer: the
/// one-second floor comes from `additional_delay`, not from the window. Reading
/// these two numbers as one "aggregation delay" is how a port ends up holding
/// already-protected answers for half a second *on top* of the second.
pub const PROTECTED_AGGREGATION_DELAY_MS: u64 = 200;

/// `_core.py:200` — `out_queue = MulticastOutgoingQueue(self, 0, _AGGREGATION_DELAY)`.
pub const PLAIN_ADDITIONAL_DELAY_MS: u64 = 0;

/// `_core.py:201` — `out_delay_queue = MulticastOutgoingQueue(self, _ONE_SECOND,
/// _PROTECTED_AGGREGATION_DELAY)`. One second added to the random draw.
pub const PROTECTED_ADDITIONAL_DELAY_MS: u64 = 1000;

/// `const.py:29` — `_ONE_SECOND`, the window `_has_mcast_record_in_last_second`
/// measures against: an answer this socket already shouted waits a full second
/// before it goes out again.
pub const PROTECTION_WINDOW_MS: u64 = 1000;

/// `_dns.py:28` — `_RECENT_TIME_MS = 250`, used as `created + 250 * ttl > now` with
/// `ttl` in **seconds**, i.e. "less than a quarter of the record's own lifetime has
/// passed since we heard it". `_dns.py:208-210` (`is_recent`) is exactly what
/// `_has_mcast_within_one_quarter_ttl` asks, and RFC 6762 §5.4 is why the number
/// exists: a QU question should be answered by unicast, *unless* this responder has
/// not sent that record within a quarter of its TTL, in which case the group is
/// supposed to hear it so peer caches stay current.
///
/// A PTR at 4500 s is therefore "recent" for 1125 seconds and an A at 120 s for 30 —
/// one to two orders of magnitude more than [`PROTECTION_WINDOW_MS`]. That is why
/// [`Ledger::prune`] cannot age entries out on the one-second window: the QU question
/// needs the same entry seconds to minutes later, and a prune tuned to one question
/// silently makes the other answer "no" forever.
pub const RECENT_FRACTION_MS_PER_TTL: u64 = 250;

/// The reference's record identity, as one comparable string.
///
/// `DNSEntry._dns_entry_matches` (`_dns.py:113-114`) is `key == key and type ==
/// type and class_ == class_` where `key = name.lower()` and `class_` has the
/// cache-flush bit masked **off**, and every concrete record then adds its own
/// rdata (`DNSPointer._eq` adds `alias_key`, `DNSText._eq` adds `text`,
/// `DNSAddress` adds the bytes). **TTL and section are not in it** — a TXT whose
/// TTL changed is the same record, and an A riding in the additional section is
/// the same record as an A in the answer section. Both halves of that omission are
/// load-bearing here: the dedupe in [`crate::respond::datagram`] suppresses an
/// additional that is already an answer, which only works because the section is
/// ignored.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordKey(String);

impl RecordKey {
    /// `kind`/`class`/`owner` plus the rdata rendered as `detail`. `detail` is
    /// `None` for a record whose rdata is already fully named by the other three
    /// fields — which is never, in practice, and is `None` only so a caller that
    /// *cannot* work out the rdata says so rather than inventing an empty string
    /// that would then collide with a real record whose rdata is empty.
    pub fn new(kind: Kind, class: u16, owner: &str, detail: Option<&str>) -> RecordKey {
        // Both halves are lowercased, and that is the reference's rule rather than a
        // tidiness habit: `_dns.py:119` keys on `name.lower()`, `DNSPointer._eq`
        // compares `alias_key` (also lowercased), and `DNSText._eq` compares the
        // length-prefixed bytes of a record whose plan side renders them as hex — so
        // the only non-name detail left is digits. The first version of this function
        // lowered only the owner, and the round-trip test found a plan-side PTR alias
        // that no heard record could ever match: the ledger would have filed our own
        // announcements under a name the wire does not carry, which is the drift
        // `the_bytes_we_send_read_back_into_the_same_record_key` exists to catch.
        RecordKey(format!(
            "{:?}#{}#{}#{}",
            kind,
            class,
            owner.to_lowercase(),
            detail.map(|d| d.to_lowercase()).unwrap_or_default()
        ))
    }

    /// The rendered text, for a log line or a test's diff.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The identity of a record this responder is **about to send**, from the plan plus
/// the service it belongs to.
///
/// A `Vec`, not one key, because one [`Chosen`] can be several records: an `A`
/// answer expands to one record per advertisable address at write time, and the
/// reference treats those as separate records with separate protection. Callers
/// that must answer for the whole plan (see `announce::serve`) take the protective
/// reading — a record we may be about to shout again is protected if **any** of
/// the records it can turn into was just heard.
pub fn plan_identity(record: &Chosen, service: &crate::announce::Service) -> Vec<RecordKey> {
    let owner = record.owner.as_str();
    match record.kind {
        Kind::Ptr => vec![RecordKey::new(
            Kind::Ptr,
            crate::mdns::CLASS_IN,
            owner,
            Some(record.alias.as_deref().unwrap_or(owner)),
        )],
        Kind::Txt => vec![RecordKey::new(
            Kind::Txt,
            crate::mdns::CLASS_IN,
            owner,
            Some(&hex(&service.txt_rdata())),
        )],
        Kind::Srv => vec![RecordKey::new(
            Kind::Srv,
            crate::mdns::CLASS_IN,
            owner,
            // `datagram` writes priority 0, weight 0, this port, this server.
            Some(&format!("0:0:{}:{}", service.port, service.server.to_lowercase())),
        )],
        Kind::Nsec => vec![RecordKey::new(
            Kind::Nsec,
            crate::mdns::CLASS_IN,
            owner,
            Some(&format!("{}:000140", service.server.to_lowercase())),
        )],
        Kind::A => service
            .addresses
            .iter()
            .map(|address| {
                RecordKey::new(
                    Kind::A,
                    crate::mdns::CLASS_IN,
                    owner,
                    Some(&address.to_string()),
                )
            })
            .collect(),
        // Question-only kinds: nothing plans an answer record with these, and a
        // caller that asks has leaked a question into the answer layer.
        Kind::Any | Kind::Aaaa => Vec::new(),
    }
}

/// The identity of a record read **off the wire**.
///
/// `None` for a record whose rdata this parser does not decode — today that is
/// `NSEC` (`mdns.rs` skips it, and the announcement corpus pins that skip). The
/// reference *does* parse NSEC into its cache, so a port that never learns an NSEC
/// key keeps answering an `AAAA` question on the fast path where the reference
/// would wait a second. That is a recorded divergence (design doc §8), not a
/// silence: an unparseable record must be dropped, never given a key invented from
/// its name, because a wrong key would protect a *different* record.
pub fn wire_identity(record: &Record) -> Option<RecordKey> {
    // Only answers and additionals are identities a queue can use; an authority
    // record is a *probe*'s claim and the reference's `answers()` includes it, but
    // nothing this port answers with can equal one.
    let detail = match &record.rdata {
        RData::A(address) => address.to_string(),
        RData::Name(alias) => alias.to_lowercase(),
        RData::Txt(bytes) => hex(bytes),
        RData::Srv {
            priority,
            weight,
            port,
            target,
        } => format!("{priority}:{weight}:{port}:{}", target.to_lowercase()),
    };
    let kind = crate::respond::kind_of(record.rtype)?;
    Some(RecordKey::new(kind, record.class, &record.name, Some(&detail)))
}

/// What the engine heard, and the only two questions it is ever asked about it.
///
/// The reference answers both from one structure, its `DNSCache`: the one-second
/// flood question (`_has_mcast_record_in_last_second`) and the QU question
/// (`_has_mcast_within_one_quarter_ttl` → `is_recent`). This type is **not** a general
/// cache — `ServiceBrowser` listeners, expiry heaps and goodbye bookkeeping live
/// elsewhere (`targets.rs` holds the device ledger, `mdns::browse` its own fold) — but
/// it now has to hold what *both* questions need, which is `created` **and `ttl`**:
/// a 4500-second PTR is "recent" for 1125 s and a 120-second A for 30 s, so an entry
/// keyed on "heard at" alone cannot answer the QU question at all, and a prune tuned to
/// the one-second window would silently make the other question answer "no" forever.
/// That is why [`Ledger::prune`] ages on the record's own quarter-TTL.
#[derive(Debug, Default)]
pub struct Ledger {
    entries: HashMap<RecordKey, Heard>,
}

/// One heard record: when it arrived on this socket, and the lifetime the wire claimed
/// for it. `ttl` is kept in seconds exactly as `mdns::Record` reads it, because the
/// quarter-TTL arithmetic (`_dns.py:210`) multiplies it by 250 rather than by 1000.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Heard {
    created: u64,
    ttl: u32,
}

impl Ledger {
    /// The one caller is the engine's receive path (`announce::run`): a datagram
    /// whose QR bit says *response*. Learning from our own send path instead would
    /// make this port stronger than the reference and would hide the case the
    /// reference is actually in when its socket cannot hear the group.
    ///
    /// **Every section** is ingested, because `incoming.py:407` counts
    /// `answers + authorities + additionals` into the one list `answers()` returns:
    /// a real DNS-SD announcement puts its PTR in AN and its SRV/TXT/A in AR, so a
    /// reader that walked only AN would never hear its own TXT come back and would
    /// never protect it. That is measured, not inferred — the four recorded answers
    /// in design doc §16 turn on a TXT that arrived in an additional section.
    ///
    /// A record whose TTL is zero is a **goodbye** and is removed rather than
    /// learned: `record_manager.py:130-135` puts an expired record into `removes`
    /// when the cache already holds it, and the announcer's goodbye burst (five
    /// records with TTL 0, looped back one poll later) must not leave the keys it
    /// names protected — that would be a stale "we just shouted this" for a record
    /// we have just announced as gone.
    ///
    /// Returns how many records it **acted on** (learned or forgot), because a
    /// caller that only wants to log "I heard myself" must not count the ones this
    /// parser could not read — an unparseable record contributes nothing, and a
    /// return value that counted it would claim a ledger entry that was never made.
    pub fn learn(&mut self, records: &[Record], now_ms: u64) -> usize {
        let mut touched = 0usize;
        for record in records {
            let Some(key) = wire_identity(record) else {
                continue;
            };
            if record.ttl == 0 {
                // The reference's asymmetry, kept: an expired record is a removal
                // **when the cache holds it**, and otherwise it is nothing at all
                // (`record_manager.py:130-135`). A counter that counted both would
                // report a goodbye that forgot nothing as work done.
                if self.entries.remove(&key).is_some() {
                    touched += 1;
                }
            } else {
                self.entries.insert(
                    key,
                    Heard {
                        created: now_ms,
                        ttl: record.ttl,
                    },
                );
                touched += 1;
            }
        }
        touched
    }

    /// When did we hear this record, if that was inside the one-second window? The
    /// caller passes the result straight to [`crate::respond::bucket`].
    pub fn seen(&self, key: &RecordKey, now_ms: u64) -> Option<u64> {
        let entry = *self.entries.get(key)?;
        (now_ms.saturating_sub(entry.created) < PROTECTION_WINDOW_MS).then_some(entry.created)
    }

    /// `_dns.py:208-210` — `created + 250 * ttl > now`: less than a quarter of the
    /// record's own lifetime has passed since it was heard. This is the QU question,
    /// and it is asked by [`crate::announce::serve`] per answer record, the way
    /// `add_qu_question_response` asks it per record.
    ///
    /// Note which way this is read: `is_recent` true means "**do not** multicast it
    /// again, send the asker a unicast instead". A port that treats recency as "go
    /// ahead and shout" inverts RFC 6762 §5.4 and turns every QU query from every
    /// browser on the LAN into a group-addressed flood — the exact traffic the rule is
    /// there to prevent.
    pub fn is_recent(&self, key: &RecordKey, now_ms: u64) -> bool {
        match self.entries.get(key) {
            Some(entry) => {
                entry.created
                    + RECENT_FRACTION_MS_PER_TTL * entry.ttl as u64
                    > now_ms
            }
            None => false,
        }
    }

    /// Drop what can no longer answer either question — which is "older than a
    /// quarter of its own TTL", the longer of the two windows for every TTL this
    /// protocol uses (a record would have to arrive with ttl < 4 s for the one-second
    /// window to outlive it, and the reference's own cache would have expired it by
    /// then anyway). Pruning on the shorter window is the trap this function used to
    /// be in: the flood answer stayed right, and `is_recent` quietly became a
    /// function that had never seen an entry old enough to answer "yes".
    ///
    /// Called once per engine tick, not per packet.
    pub fn prune(&mut self, now_ms: u64) {
        self.entries.retain(|_, entry| {
            entry.created + RECENT_FRACTION_MS_PER_TTL * entry.ttl as u64 > now_ms
        });
    }

    /// How many keys are held. A test reads this to prove a goodbye removed
    /// something rather than the prune window having happened to pass.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// See [`Ledger::len`].
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One answer waiting to go out, and **which service it belongs to**.
///
/// The service index is not decoration. The reference's queue holds record objects,
/// which carry their own rdata; our plan layer's [`Chosen`] deliberately does not
/// (it decides *whether* the host rides along, not which of several addresses it
/// has — see `respond::datagram`), so the batch that comes out of the queue can only
/// be rendered if it remembers whose TXT and whose port each record means. One
/// engine answers for every registered service, so a merged batch really can mix
/// them, and the index is resolved against the same list that
/// [`crate::announce::run`] built its answering services from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub service: usize,
    pub answer: Chosen,
    pub additionals: Vec<Chosen>,
}

impl Queued {
    /// Lift one planned reply group into the queue's element.
    pub fn new(service: usize, group: &crate::respond::Group) -> Queued {
        Queued {
            service,
            answer: group.answer.clone(),
            additionals: group.additionals.clone(),
        }
    }

    /// Every record this answer brings with it, answer first — the order the
    /// reference's `_add_answers_additionals` writes them in.
    pub fn records(&self) -> Vec<Chosen> {
        let mut records = vec![self.answer.clone()];
        records.extend(self.additionals.iter().cloned());
        records
    }
}

/// One batch of answers waiting for the same moment, with the two deadlines
/// `AnswerGroup` carries in the reference (`answers.py`: `send_after`,
/// `send_before`).
///
/// `answers` keeps the reference's **dict** semantics: keyed by the answer record,
/// value = its additionals, and `update` *replaces* the additionals of a key that
/// is already there rather than unioning them (`dict.update` overwrites). A merge
/// that unioned would send a record the reference dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerGroup {
    pub send_after: u64,
    pub send_before: u64,
    pub answers: Vec<Queued>,
}

/// What [`OutgoingQueue::add`] did with the answers it was given.
///
/// An enum because the reference's two branches differ in *scheduling*, and a
/// boolean called `merged` would be read as "the queue changed" by the next person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// The last group was still waiting and its deadline was later than ours, so
    /// these answers rode out with it. No new deadline exists.
    Merged,
    /// A new group was appended. The reference only schedules `async_ready` when
    /// the queue was **empty** before this add, and the merged branch returns early
    /// — see the `if len(self.queue): ... else: loop.call_at(...)` pair.
    Appended,
}

/// A queue of aggregated multicast answers — `MulticastOutgoingQueue`.
///
/// Two of these exist on the reference's engine, and which one an answer goes into
/// is decided by [`crate::respond::bucket`]: [`OutgoingQueue::aggregating`] is the
/// ordinary one, [`OutgoingQueue::protected`] is the one that has already shouted
/// this record within a second.
#[derive(Clone, Debug)]
pub struct OutgoingQueue {
    groups: VecDeque<AnswerGroup>,
    additional_delay_ms: u64,
    aggregation_delay_ms: u64,
}

impl OutgoingQueue {
    /// `out_queue`: no extra floor, 500 ms window.
    pub fn aggregating() -> OutgoingQueue {
        OutgoingQueue::with_delays(PLAIN_ADDITIONAL_DELAY_MS, AGGREGATION_DELAY_MS)
    }

    /// `out_delay_queue`: a full second of floor, 200 ms window.
    pub fn protected() -> OutgoingQueue {
        OutgoingQueue::with_delays(
            PROTECTED_ADDITIONAL_DELAY_MS,
            PROTECTED_AGGREGATION_DELAY_MS,
        )
    }

    /// Both numbers are arguments because the pair *is* the design: the reference
    /// constructs the two queues with different ones, and a queue that hardcodes
    /// them cannot be the same code twice.
    pub fn with_delays(additional_delay_ms: u64, aggregation_delay_ms: u64) -> OutgoingQueue {
        OutgoingQueue {
            groups: VecDeque::new(),
            additional_delay_ms,
            aggregation_delay_ms,
        }
    }

    /// `async_add`, with the random draw taken from the caller.
    ///
    /// `random_ms` is the reference's `randint(20, 120)` and nothing else: the two
    /// delays are added here so that the one place they are computed is this
    /// function, and a test can hand in `RANDOM_MIN_MS` and `RANDOM_MAX_MS` to
    /// reach either end of a comparison that the reference decides by chance.
    pub fn add(&mut self, now_ms: u64, random_ms: u64, answers: Vec<Queued>) -> Added {
        let delay = random_ms + self.additional_delay_ms;
        let send_after = now_ms + delay;
        let send_before = now_ms + self.aggregation_delay_ms + self.additional_delay_ms;
        if let Some(last) = self.groups.back_mut() {
            // "less than the last group scheduled to go out" — `<=`, not `<`. The
            // boundary is what makes reading ① of the four measured answers come out
            // as one datagram when the two draws are equal, and a `<` here answers
            // that pair in two.
            if send_after <= last.send_after {
                merge_into(&mut last.answers, answers);
                return Added::Merged;
            }
        }
        self.groups.push_back(AnswerGroup {
            send_after,
            send_before,
            answers,
        });
        Added::Appended
    }

    /// The earliest moment [`OutgoingQueue::ready`] can return anything, or `None`
    /// when the queue is empty.
    ///
    /// This is the poll interval the engine should use. It exists because the
    /// reference does not poll — it calls `loop.call_at` — and a port that replaced
    /// that with a fixed tick would add up to a whole tick of latency to every
    /// answer, while a port that blocked on the socket instead would starve the
    /// 60-second re-announcement (`channel.rs`'s 「读不得跨阻塞持锁」 one layer up).
    ///
    /// The `send_before` half is `async_ready`'s deferral: **with more than one
    /// group waiting**, the first one is held until its aggregation deadline even if
    /// its own `send_after` has passed, so that later questions can still be merged
    /// into it. With one group there is nothing left to merge with, so it goes out
    /// at its own `send_after`. Measured: PTR asked, then TXT asked 10 ms later,
    /// produced **one** 296-byte datagram at **+502 ms** — the second group's
    /// `send_after` was already past, and the deferral is the only reason the pair
    /// did not go out as two datagrams at +50 and +60 ms.
    pub fn due(&self, now_ms: u64) -> Option<u64> {
        let first = self.groups.front()?;
        if self.groups.len() > 1 && first.send_before > now_ms {
            return Some(first.send_before);
        }
        Some(first.send_after.max(now_ms))
    }

    /// `async_ready`: hand back every group that may go now, and `Vec::new` when
    /// nothing may (including the deferral case, which the caller re-asks on its
    /// next tick — [`OutgoingQueue::due`] already told it when).
    ///
    /// Groups are popped while `send_after <= now`, merged in queue order with the
    /// reference's `dict.update` semantics (a later group's answer **replaces** the
    /// earlier one's, additionals and all), and then every answer that is going out
    /// is removed from the groups that remain — `_remove_answers_from_queue`, the
    /// reason the same record cannot be shouted twice when two questions about it
    /// land on either side of a deadline.
    pub fn ready(&mut self, now_ms: u64) -> Vec<Queued> {
        if self.groups.len() > 1 && self.groups[0].send_before > now_ms {
            return Vec::new();
        }
        let mut sending: Vec<Queued> = Vec::new();
        while let Some(front) = self.groups.front() {
            if front.send_after > now_ms {
                break;
            }
            let group = self.groups.pop_front().expect("checked above");
            merge_into(&mut sending, group.answers);
        }
        if !sending.is_empty() {
            self.remove_from_pending(&sending);
        }
        sending
    }

    /// `_remove_answers_from_queue`: for each group still waiting, drop every
    /// answer we are sending now (and with it, the additionals that rode with it).
    fn remove_from_pending(&mut self, sending: &[Queued]) {
        for pending in &mut self.groups {
            pending
                .answers
                .retain(|group| !sending.iter().any(|held| same_answer(&held.answer, &group.answer)));
            // The reference pops by *key* only; a group left with no answers stays
            // in the queue with an empty dict and its deadline still defers the
            // ones behind it. Reproduced, because "empty group" is a state the
            // reference can be in and a test may put us in.
        }
    }

    /// How many groups are waiting. The tests read the queue through this and
    /// [`OutgoingQueue::groups`] rather than through a send, because a send is the
    /// thing being scheduled.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// The waiting groups, oldest first. Read-only: a caller that could mutate
    /// these deadlines would be a second owner of the schedule.
    pub fn groups(&self) -> &VecDeque<AnswerGroup> {
        &self.groups
    }

    /// See [`OutgoingQueue::group_count`].
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// `dict.update`: existing keys keep their position and get their value replaced;
/// new keys are appended. Order matters here because the merged batch is rendered
/// into one datagram in this order, and a reader comparing two implementations
/// should see the same *first* answer.
fn merge_into(into: &mut Vec<Queued>, from: Vec<Queued>) {
    for group in from {
        if let Some(held) = into
            .iter_mut()
            .find(|held| same_answer(&held.answer, &group.answer))
        {
            held.additionals = group.additionals;
            held.service = group.service;
        } else {
            into.push(group);
        }
    }
}

/// The production draw: `random.randint(20, 120)` **inclusive**, from the `rand` crate
/// this crate already depends on for its session tokens.
///
/// A named function rather than a closure written inside [`Outgoing::new`] because the
/// inclusivity is a rule with a witness, and a witness has to reach the product's own
/// expression. Sampling `gen_range` inline in a test would prove the test wrote the
/// same thing; sampling *this* function 5,000 times proves both ends are reachable,
/// which is what separates `20..=120` from the `20..120` that reads like it (the first
/// version of the test drew through `Outgoing::new()` 400 times, which made the tail
/// claim a coin flip — P(neither end shows up) is about 4% per run, and a test that
/// fails one run in twenty five gets deleted, not fixed).
pub fn production_draw() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(RANDOM_MIN_MS..=RANDOM_MAX_MS)
}

/// The engine's whole sending state: the two queues, the ledger, and the draw.
///
/// `Zeroconf` holds exactly these three things for the same purpose (`out_queue`,
/// `out_delay_queue`, `cache`), and bundling them here is what lets
/// [`crate::announce::run`] own one value instead of threading three through every
/// call. The randomness is a boxed seam because the two queues' behaviour is the
/// one thing in this module that a test must be able to *order*: the four recorded
/// answers in design doc §16 are about which two questions merged, and a queue that
/// draws from the clock inside itself can only be asked probabilistically.
pub struct Outgoing {
    pub aggregating: OutgoingQueue,
    pub protected: OutgoingQueue,
    pub heard: Ledger,
    random: Box<dyn FnMut() -> u64>,
}

impl Default for Outgoing {
    fn default() -> Self {
        Outgoing::new()
    }
}

impl Outgoing {
    /// The production queue's draw is [`production_draw`], and nothing else.
    pub fn new() -> Outgoing {
        Outgoing::with_random(Box::new(production_draw))
    }

    /// Every scheduling rule in this file is testable through this constructor, and
    /// only through it: a fixed draw makes "did these two questions merge" a
    /// statement rather than a coin flip.
    pub fn with_random(random: Box<dyn FnMut() -> u64>) -> Outgoing {
        Outgoing {
            aggregating: OutgoingQueue::aggregating(),
            protected: OutgoingQueue::protected(),
            heard: Ledger::default(),
            random,
        }
    }

    /// Which queue this bucket is, and the one place that asks.
    ///
    /// Returns how many answers were handed over, so the caller can count what is
    /// waiting to reach the wire without pretending it has reached it.
    pub fn enqueue(
        &mut self,
        bucket: crate::respond::Timing,
        service: usize,
        groups: &[crate::respond::Group],
        now_ms: u64,
    ) -> usize {
        let queued: Vec<Queued> = groups
            .iter()
            .map(|group| Queued::new(service, group))
            .collect();
        if queued.is_empty() {
            return 0;
        }
        let queue = match bucket {
            crate::respond::Timing::Aggregate => &mut self.aggregating,
            crate::respond::Timing::AggregateProtected => &mut self.protected,
            // `Now` never comes through here: it is written by the caller the moment
            // it is decided. Reaching this arm means a caller routed the fast bucket
            // into a queue, which would delay an answer the reference sends at once.
            crate::respond::Timing::Now => {
                debug_assert!(false, "Timing::Now was queued instead of sent");
                return 0;
            }
        };
        let count = queued.len();
        let random = (self.random)();
        queue.add(now_ms, random, queued);
        count
    }

    /// The soonest moment anything may go out, or `None` when both queues are
    /// empty. `announce::run` turns this into the socket's read timeout, which is
    /// what keeps a 200 ms poll from adding 200 ms to every answer.
    pub fn deadline(&self, now_ms: u64) -> Option<u64> {
        match (self.aggregating.due(now_ms), self.protected.due(now_ms)) {
            (Some(a), Some(p)) => Some(a.min(p)),
            (Some(a), None) => Some(a),
            (None, Some(p)) => Some(p),
            (None, None) => None,
        }
    }

    /// Empty whatever has come due. One `Vec` per **batch**, oldest batch first, and
    /// an empty queue contributes nothing: the two queues are independent in the
    /// reference, and each merged dict becomes its own datagram
    /// (`_add_answers_additionals` is called once per queue), so flattening the two
    /// into one list would put an answer that was supposed to wait a full second on
    /// the wire beside the one that was not.
    pub fn due(&mut self, now_ms: u64) -> Vec<Vec<Queued>> {
        let mut batches = Vec::new();
        for queue in [&mut self.aggregating, &mut self.protected] {
            let batch = queue.ready(now_ms);
            if !batch.is_empty() {
                batches.push(batch);
            }
        }
        batches
    }

    /// How many answers are waiting in either queue.
    pub fn waiting(&self) -> usize {
        self.aggregating
            .groups()
            .iter()
            .chain(self.protected.groups().iter())
            .map(|group| group.answers.len())
            .sum()
    }
}

/// Do two planned answers name the same record?
///
/// [`RecordKey`] is the identity that includes rdata, and building it needs the
/// [`crate::announce::Service`] — which a queue does not have (the reference's
/// queue holds the record objects themselves, which carry their own rdata). Inside
/// one reply every candidate already came from the same service, so the two fields
/// that can distinguish two answers there are the type and the name — and the name
/// comparison is **case-insensitive** because `key = name.lower()`
/// (`_dns.py:119`). `alias` is compared because a PTR's rdata *is* its alias and
/// two Chromecasts share the PTR's name. TTL is not compared: it is not in
/// `_dns_entry_matches`, and a queue that treated "same record, different TTL" as
/// two records would answer the same question twice.
fn same_answer(a: &Chosen, b: &Chosen) -> bool {
    a.kind == b.kind
        && a.alias == b.alias
        && a.owner.to_lowercase() == b.owner.to_lowercase()
}

#[cfg(test)]
#[path = "aggregate_tests.rs"]
mod aggregate_tests;
