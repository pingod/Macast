// Copyright (c) 2026 by pingod. All Rights Reserved.
//
//! The announcement half of mDNS: what this machine says about itself,
//! unprompted, so a sender that **only listens** can find us.
//!
//! Python counterpart: `macast/discovery.py` (`MDNSAdvertiser.advertise` →
//! zeroconf's `register_service`, plus the 60-second keepalive thread), driven by
//! `macast/protocol_cast.py`'s `_advertise()` — which owns the TXT table.
//!
//! ## Provenance — the same third layer as [`crate::mdns`], and it is not ours
//!
//! The record order, the TTL split, the cache-flush rule and the
//! name-compression algorithm below are transcribed from **zeroconf** 0.151.3
//! (`_core.py`'s `_add_broadcast_answer` / `_async_broadcast_service`,
//! `_protocol/outgoing.py:285-327`, `_dns.py:287-400`), which is
//! **LGPL-2.1-or-later** — the situation `mdns.rs:13-23` records for the reading
//! side and `macast/ssdp.py` records for Coherence: rewriting the code does not
//! remove the attribution (AGENTS.md §4.12).
//!
//! ## Every number here was measured, not inferred
//!
//! The instrument is `scripts/mdns_announce_probe.py`. It replaces
//! `zeroconf._core.async_send_with_transport` — the **only** `sendto` call in the
//! package (`_core.py:134`, grepped over the installed tree) — with a recorder,
//! so no byte of it leaves this machine, and it decodes every captured datagram
//! **twice** (once by hand, once through zeroconf's own `DNSIncoming`) and
//! refuses to report unless the two readers agree. Its outputs are the two
//! corpora under `tests/golden/` that `announce_tests.rs` compares against.
//! Readings taken 2026-10-18 on this build:
//!
//! * One announcement is **5 records**: PTR, SRV, TXT, then the address pair.
//!   TTLs are `4500` for PTR/TXT (`_DNS_OTHER_TTL`) and `120` for SRV/A/NSEC
//!   (`_DNS_HOST_TTL`); the cache-flush bit is on **all of them except the PTR**.
//!   That is not a style choice: `_write_record_class` ORs `0x8000` in only for
//!   `record.unique`, and a PTR is constructed non-unique because one service
//!   type has many instances. A reader that forgets the mask matches nothing
//!   (AGENTS.md §4.2, and `mdns.rs`'s first red line for the reading side).
//! * Register sends **3 bursts 225 ms apart**; `update_service` — what the
//!   keepalive calls every 60 s — sends the same three bursts; `unregister` sends
//!   3 bursts 125 ms apart with **every TTL written as 0**. A goodbye is the
//!   same records, not an empty packet.
//! * The tail pair's order is **not a protocol fact**.
//!   `get_address_and_nsec_records` returns a `set`, and on this build
//!   `PYTHONHASHSEED` 0/1/2 emit `NSEC, A` while `PYTHONHASHSEED 12345` emits
//!   `A, NSEC`. The corpus therefore freezes both; pinning one would be pinning
//!   a hash seed. PTR/SRV/TXT *are* a fixed order and are byte-identical in all
//!   four corpus lines — measured: the first difference sits at offset 266.
//! * Destination `224.0.0.251:5353`, `flags = 0x8400` (response + authoritative
//!   answer), transaction id 0, 296 bytes for the fixture's inputs. On this
//!   machine the reference opened **six** sender sockets (`127.0.0.1`, the LAN
//!   address, Tailscale, three VM bridges) and wrote the identical datagram to
//!   each one.
//! * An over-long friendly name does not degrade gracefully **in the reference**:
//!   `ServiceInfo(...)` packs each TXT property into a length-prefixed string,
//!   and measured on this machine a 300-byte value raises
//!   `ValueError: bytes must be in range(0, 256)` from `advertise()` — outside
//!   the two `try` blocks that function does have (`discovery.py:266-269` and
//!   `:296-300` wrap the *instance* and the *registration*, not the property
//!   packing) — so it propagates out of `_advertise()` and **out of
//!   `ChromecastProtocol.start()`** (`protocol_cast.py:414` has no guard). A
//!   user who types a long device name therefore loses the Chromecast receiver
//!   entirely. This port does not copy that: see the divergence below.
//!
//! ## Deliberate divergences (each is also a row in `docs/Rust-Port-Design.md` §8)
//!
//! * **One socket, not one per interface, and — today — not joined and not read.**
//!   The reference binds a sender per IPv4 address *and* joins each to the group,
//!   so every burst goes out six times and every one of those sockets is read by
//!   zeroconf's engine. Ours sends from a single socket that never reads.
//!
//!   **The reason this socket does not join was a guess, and it has now been
//!   measured** (`scripts/mdns_reuseport_probe.py`; 2026-10-18 on this Mac and on
//!   the CachyOS box; egress pinned to `lo0` with `IP_MULTICAST_IF=127.0.0.1` +
//!   TTL 1 so no byte reaches the LAN; twelve and then eight datagrams per case,
//!   each with a **distinct transaction id** so "every socket got every datagram"
//!   is a per-datagram reading rather than two equal counts, and each socket's
//!   queue is filtered to *our* ids because a live mDNS stack puts other
//!   people's traffic in there too — the first Linux run read `14 of 12`, which
//!   is noise, not a finding). What the two machines say, and what it costs this
//!   file:
//!
//!   - **Multicast is duplicated.** Two joined sockets on macOS: `[12, 12, 0]`.
//!     Three: `[12, 12, 12]`. On Linux the same three rows read `[8, 8, 8]` —
//!     including the **unjoined** socket, which on Linux is a multicast receiver
//!     whether or not it joined. So a second joined reader does **not** cost the
//!     browser anything a phone sends (a query and an announcement are both
//!     multicast to `224.0.0.251`), and the sentence this bullet used to carry —
//!     "`SO_REUSEPORT` distributes each incoming datagram to one of the bound
//!     sockets, so a second joined socket takes away part of what the device
//!     dropdown hears" — **is not what either kernel does**.
//!   - **Unicast is handed to exactly one socket, and which one is not stable in
//!     the way the old sentence guessed.** Linux, three passes in one process:
//!     `[8, 0, 0]` every time. Different process, different ephemeral source
//!     port: `[0, 0, 12]` and `[0, 8, 0]` in the two earlier runs — so the pick
//!     is per-flow, not per-bind-order, and a run cannot be quoted as the rule.
//!     macOS: a wildcard-bound socket gets **nothing** even with no competition
//!     in the probe (the OS responder is the likely holder; an unprivileged
//!     `lsof` cannot name it), while a socket bound to the exact destination
//!     address takes the whole stream away from a wildcard one (`[12, 0]`, stable
//!     across passes).
//!
//!   The plan changes shape accordingly, and the conclusion is **not** what the
//!   guess reached: the merge is real, but it is about **unicast**, not about
//!   multicast, and it has to happen on the **long-lived** socket rather than the
//!   transient browse. Today this socket is a competitor for unicast it never
//!   reads, and on Linux it is also a receiver of every multicast datagram whose
//!   queue nobody drains. When the responder lands (§15's remaining row) it joins
//!   and **reads** — answers the queries, and feeds what it hears to the device
//!   ledger — so the one socket that is always on the port is a socket somebody
//!   is actually reading. `mdns.rs::open`'s claim that unicast replies to our own
//!   queries "still reach us" is therefore conditional in a way its comment did
//!   not say: they reach **a** socket bound to 5353, and only count when the one
//!   that reads them is ours.
//! * **A too-long TXT property costs the property, not the receiver.** Ours
//!   truncates at the wire limit — on a character boundary, `fn` first, and the
//!   warning says the field name, the byte count and the limit — and announces.
//!   The reference's `start()` raises, which turns a naming preference into
//!   「Chromecast 起不来」. Choosing "lose 40 bytes of a decorative name" over
//!   "lose the receiver" is the direction this repository has always taken
//!   (§4.8's 「越界夹住而不是拒绝」, the same argument as `vt_level`'s "the safe
//!   side"). **The Python side is still as measured above — this document does
//!   not claim it was fixed.**
//! * **No announcement without a listener.** `_advertise()` runs after
//!   `_start_tls_server()` regardless of whether 8009 came up, and `cast_port`
//!   keeps its default, so the Python app advertises a port nothing holds. Ours
//!   refuses to announce unless the channel is actually listening
//!   ([`Announcer::start`]'s caller decides that), because a device that appears
//!   in a menu and then fails to cast is AGENTS.md §4.1's first symptom row, not
//!   a smaller version of it.
//! * **No uniqueness probe.** zeroconf sends a `QU` question for the PTR and
//!   waits (`generate_service_query`, `_core.py:263-283`), then raises
//!   `NonUniqueNameException` on a collision — which `advertise()` lets through
//!   as well. We announce anyway, so: two instances with the same friendly name
//!   on one LAN publish the same instance and SRV target, and a sender may
//!   resolve the name to the other machine's port. Not building the probe packet
//!   at all is deliberate — a probe without the listen-and-wait is a packet that
//!   decides nothing, and dead code that reads like coverage is exactly what
//!   AGENTS.md §4.13 exists to catch. The corpus keeps the reference's probe
//!   bytes so the divergence stays *checkable*: nothing this crate writes
//!   produces them (there is a test for that sentence).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use macast_core::logging::Logger;
use macast_core::setting::Setting;
use socket2::{Domain, Protocol, Socket, Type};

/// The service type we announce — the same constant the browser asks about, so
/// one string in this crate means "Chromecast receiver".
pub use crate::mdns::SERVICE_TYPE;

/// `set_respond_socket_multicast_options` (`_utils/net.py:206-221`), v4 arm:
/// TTL 255 **and** loop=1. The browser deliberately leaves loop alone
/// (`mdns.rs:77-84`), which is why it hears itself and why `drop_ours` exists;
/// the responder sets it because that is what the reference's sender sockets
/// carry.
pub const MULTICAST_TTL: u8 = 255;

/// `_DNS_OTHER_TTL` (`const.py:43`): PTR and TXT live 75 minutes.
pub const OTHER_TTL: u32 = 4500;
/// `_DNS_HOST_TTL` (`const.py:42`): SRV, A and the NSEC that accompanies them
/// live two minutes — the whole reason a keepalive has to exist.
pub const HOST_TTL: u32 = 120;

/// `_REGISTER_BROADCASTS` (`_core.py:90`).
pub const BROADCASTS: usize = 3;
/// `_REGISTER_TIME` (`const.py:16`): the gap between announcement bursts.
pub const REGISTER_GAP: Duration = Duration::from_millis(225);
/// `_UNREGISTER_TIME` (`const.py:17`): the gap between goodbye bursts.
pub const GOODBYE_GAP: Duration = Duration::from_millis(125);
/// `MDNSAdvertiser.REANNOUNCE_SECONDS` (`discovery.py:239`), whose own comment
/// is the reason: 「comfortably inside the 120s TTL」, because some senders only
/// listen — libvlc's microdns 「never sends a query of its own」 — so one
/// announcement per start is not enough to *stay* known.
pub const REANNOUNCE: Duration = Duration::from_secs(60);

/// The group every burst is written to.
pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

/// The port — **re-exported**, not repeated. This file used to declare its own
/// `pub const MDNS_PORT: u16 = 5353`, so a reader comparing the two modules' copies
/// found agreement and nothing more: two owners of a protocol number, and the only
/// thing that could ever disagree about it is a change to one of them. `crate::mdns`
/// owns it (its doc already says whose port it is — "the only port a browser binds"),
/// and `§16 第 13 条`'s merged-socket work is exactly the change that would have to
/// move it once and have both sides follow.
pub use crate::mdns::MDNS_PORT;

/// The DNS numbers. Reading lives in [`crate::mdns`]; these are the writing side
/// of the same table, and a mismatch between the two is a packet our own browser
/// would not recognise — `announce_tests` asks that directly.
pub(crate) const TYPE_A: u16 = 1;
pub(crate) const TYPE_PTR: u16 = 12;
pub(crate) const TYPE_TXT: u16 = 16;
pub(crate) const TYPE_SRV: u16 = 33;
pub(crate) const TYPE_NSEC: u16 = 47;
const CLASS_IN: u16 = 1;
/// The cache-flush bit as it sits **on the wire**, inside the class field.
const CLASS_UNIQUE: u16 = 0x8000;

/// response + authoritative answer — `DNSOutgoing(_FLAGS_QR_RESPONSE |
/// _FLAGS_AA)` (`_core.py:523`).
pub(crate) const FLAGS_ANNOUNCE: u16 = 0x8400;

/// The longest one TXT property may be on the wire. Not a guess: measured by
/// handing zeroconf values of 254 / 255 / 256 / 257 bytes and reading which
/// attempts raised (see the module header) — the reference packs each property
/// with a single length byte, so 255 is the last one that fits a datagram and
/// 256 is the first `ValueError`.
pub const MAX_TXT_PROPERTY: usize = 255;

/// Python's advertiser logs under `Discovery` (`discovery.py:26`); the Cast
/// protocol's own advertise line is under `Chromecast`. Everything in this
/// module is the advertiser, so it takes the first name — and the log family the
/// settings page already lists (`query=log-modules`) is therefore the one a
/// "why doesn't my phone see this" question has to read.
pub const LOGGER_NAME: &str = "Discovery";

/// `sanitize_instance_name(..., fallback="Macast")`: the label used when a
/// friendly name has nothing DNS-SD-legal left after normalising.
pub const FALLBACK_NAME: &str = "Macast";

/// One DNS label's ceiling, and the instance name's leading-label cap
/// (`_MAX_LABEL` in `discovery.py:34`).
pub const MAX_LABEL: usize = 63;

/// Which of the two orders the reference produces for the address/NSEC pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailOrder {
    NsecFirst,
    AFirst,
}

/// One announcement's worth of facts. Build it with [`Service::for_cast`] or
/// [`Service::from_setting`], not by hand outside tests: the instance name, the
/// SRV target and the A records must be **one** derivation, or the packet
/// answers a question about a name nothing in the packet owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// e.g. `Macast-Pavia-MacBookPro-1859._googlecast._tcp.local.`
    pub instance: String,
    /// The SRV target, and the owner of the A records: `Macast-…-1859.local.`
    pub server: String,
    pub port: u16,
    /// TXT properties **in the order the reference writes them**. A `Vec`, not a
    /// map: the wire order is the answer a sender's cache keeps (§9.2).
    pub txt: Vec<(String, Vec<u8>)>,
    pub addresses: Vec<Ipv4Addr>,
    /// Which service this record set announces: `_googlecast._tcp.local.` for a
    /// Chromecast receiver, `_airplay._tcp.local.` for the AirPlay one.
    ///
    /// A **field**, not a second module: measured on 2026-10-18 against
    /// `tests/golden/mdns_airplay.hex`, the two announcements are record-for-record
    /// identical — same five records in the same sections, same TTLs
    /// (4500/120/4500/120/120), same cache-flush rule (everything but the PTR),
    /// same `flags=0x8400`, same goodbye (the same five records with TTL 0, not an
    /// empty packet) — and the only difference is this name plus the TXT table.
    /// A second builder would have been a second place to be wrong about all of
    /// that, and §16's A/NSEC lesson says the corpus, not the reading, is what
    /// decides a shape.
    pub service_type: String,
    /// Set when [`clip_txt`] shortened something, so the caller can say so once
    /// instead of every burst.
    pub clipped: Vec<(String, usize)>,
}

impl Service {
    /// The Cast receiver's service from the two settings that name it and the
    /// addresses that reach it.
    ///
    /// The non-literal half of the TXT table — `id` and `fn` — comes from
    /// `Setting::usn()` and `Setting::friendly_name()`, which are also what
    /// SSDP's USN and the DLNA friendly name read. One owner, so the device a
    /// sender discovers over mDNS cannot be a different machine from the one it
    /// talks to over UPnP.
    ///
    /// `ca=5` is not decoration: VLC reads that field as a bitmask
    /// (`modules/services_discovery/microdns.c`), and without the video bit it
    /// adds `no-video` to its cast chain and never sends a video elementary
    /// stream at all — which looks exactly like a receiver bug
    /// (`protocol_cast.py:461-466`, AGENTS.md §4.1's 「有声音没画面」 row).
    pub fn for_cast(setting: &Setting, port: u16, addresses: Vec<Ipv4Addr>) -> Service {
        let friendly = setting.friendly_name();
        let instance = sanitize_instance_name(&friendly, SERVICE_TYPE, FALLBACK_NAME);
        let server = normalize_server(instance.split('.').next().unwrap_or(FALLBACK_NAME));
        let txt: Vec<(String, Vec<u8>)> = vec![
            ("id".to_string(), setting.usn().into_bytes()),
            ("fn".to_string(), friendly.clone().into_bytes()),
            ("ca".to_string(), b"5".to_vec()),
            ("st".to_string(), b"0".to_vec()),
            ("bs".to_string(), b"FA8B".to_vec()),
            ("ic".to_string(), b"/setup/icon.png".to_vec()),
            ("ve".to_string(), b"05".to_vec()),
            ("md".to_string(), b"Macast".to_vec()),
            ("rm".to_string(), b"Macast".to_vec()),
            ("rs".to_string(), b"Macast Receiver".to_vec()),
        ];
        let (txt, clipped) = clip_txt(txt);
        Service {
            instance,
            server,
            port,
            txt,
            addresses,
            service_type: SERVICE_TYPE.to_string(),
            clipped,
        }
    }

    /// The AirPlay receiver's service, from the same two settings plus the RTSP port
    /// this run **actually bound**.
    ///
    /// Two facts the reference owns and this copies rather than improves:
    ///
    /// * the nine TXT keys, in this order, with `features` rendered as
    ///   `"0x%X"` — asked of `AirPlayProtocol._advertise` with the advertiser
    ///   replaced by a recorder, so the table is what the product hands zeroconf
    ///   rather than what a text scan of `protocol_airplay.py` suggests
    ///   (`scripts/mdns_announce_probe.py --protocol airplay`; frozen in
    ///   `tests/golden/mdns_airplay.hex`).
    /// * `deviceid` is **drawn per process** (`protocol_airplay.py:99-103`, never
    ///   persisted), so a phone that sees this Mac before and after a restart is
    ///   told it is two different receivers. That is the reference's behaviour.
    ///   Persisting it would look like a fix and read like a divergence to every
    ///   sender that cached the old one, so it is not done here — §8 carries the
    ///   row if anyone wants to change both sides at once.
    ///
    /// `port` is the caller's, deliberately: `AirPlayProtocol.start()` binds the
    /// RTSP listener **before** advertising, because macOS ships its own receiver
    /// on 7000 and the fallback number is the one a sender must be told. A
    /// hardcoded 7000 here would announce a port that may not be ours.
    pub fn for_airplay(
        setting: &Setting,
        port: u16,
        addresses: Vec<Ipv4Addr>,
        device_id: &str,
    ) -> Service {
        let friendly = setting.friendly_name();
        let instance = sanitize_instance_name(&friendly, crate::airplay::AIRPLAY_SERVICE, FALLBACK_NAME);
        let server = normalize_server(instance.split('.').next().unwrap_or(FALLBACK_NAME));
        let txt: Vec<(String, Vec<u8>)> = vec![
            ("deviceid".to_string(), device_id.as_bytes().to_vec()),
            (
                "features".to_string(),
                format!("0x{:X}", crate::airplay::FEATURES).into_bytes(),
            ),
            ("model".to_string(), b"Macast".to_vec()),
            ("name".to_string(), friendly.clone().into_bytes()),
            ("srcvers".to_string(), b"320.20".to_vec()),
            ("vv".to_string(), b"2".to_vec()),
            ("pi".to_string(), b"2e3883fe".to_vec()),
            ("flags".to_string(), b"0x4".to_vec()),
            (
                "pk".to_string(),
                b64_macast_public_key().as_bytes().to_vec(),
            ),
        ];
        let (txt, clipped) = clip_txt(txt);
        Service {
            instance,
            server,
            port,
            txt,
            addresses,
            service_type: crate::airplay::AIRPLAY_SERVICE.to_string(),
            clipped,
        }
    }

    /// The addresses to publish: [`macast_core::net::advertised_addresses`] —
    /// the **narrowed** set, i.e. the interface carrying the default route plus
    /// whatever the user pinned. Not [`macast_core::net::advertised_hosts`],
    /// which is the superset [`crate::mdns`]'s browser *joins* on.
    ///
    /// This is AGENTS.md §4.1's first row, and §22 of the acceptance script
    /// caught it on this machine the first time it ran: the announcement carried
    /// `192.168.1.5, 192.168.148.0, 192.168.215.0, 192.168.139.3` — a LAN address
    /// plus a VMware host-only adapter, a VirtualBox one and a Docker one —
    /// because those three are what a join-able interface list contains. A sender
    /// picks one of them, gets no route, and the user's report is 「能搜到、
    /// 点投屏没反应」 with **nothing wrong on our side at all**. Same family as
    /// §4.14's 「`advertise_host()` 在核心和插件里各有一份」: the two functions
    /// answer different questions, and the packet has to be built from the one
    /// that means "reachable".
    ///
    /// A text that is not an IPv4 address is skipped with a warning rather than
    /// losing the whole announcement — one dead interface among several is not a
    /// reason for a sender to learn nothing.
    pub fn from_setting(setting: &Setting, port: u16, log: &Logger) -> Service {
        let service = Service::for_cast(setting, port, advertisable(setting, log));
        report_clipped(&service, log);
        service
    }

    /// The AirPlay announcement's own entry point — same addresses, same clipping
    /// report, different table and port.
    ///
    /// `port` is a parameter and not `airplay::AIRPLAY_PORT`: the reference binds
    /// first and advertises the number it got (`protocol_airplay.py:126-131`),
    /// because macOS's own receiver sits on 7000 on this machine. Announcing the
    /// requested port instead of the bound one would publish an address that
    /// belongs to somebody else's AirPlay.
    pub fn airplay_from_setting(
        setting: &Setting,
        port: u16,
        device_id: &str,
        log: &Logger,
    ) -> Service {
        let service = Service::for_airplay(setting, port, advertisable(setting, log), device_id);
        report_clipped(&service, log);
        service
    }

    /// The announcement: each record with its own TTL, tail in the order this
    /// port chose (NSEC then A — one of the two the reference emits).
    pub fn announcement(&self) -> Vec<u8> {
        self.datagram(None, TailOrder::NsecFirst)
    }

    /// The same announcement with the address pair in the other order — what the
    /// reference *also* emits, depending on its hash seed. Public because the
    /// corpus pins both, and a test that claims byte parity has to say which one
    /// it is claiming.
    pub fn announcement_a_first(&self) -> Vec<u8> {
        self.datagram(None, TailOrder::AFirst)
    }

    /// The goodbye: **the same records with every TTL written as 0**. Not an
    /// empty packet, and not a missing one — a sender that keeps a stale PTR for
    /// 75 minutes is the failure mode this prevents.
    pub fn goodbye(&self) -> Vec<u8> {
        self.datagram(Some(0), TailOrder::NsecFirst)
    }

    pub fn goodbye_a_first(&self) -> Vec<u8> {
        self.datagram(Some(0), TailOrder::AFirst)
    }

    /// May the responder answer queries on this service's socket?
    ///
    /// Yes for `_googlecast._tcp`, no for anything else — and the reason is a
    /// measurement gap, not a policy preference. The 17 rows of
    /// `tests/golden/mdns_responder.jsonl` are answers the reference gave to
    /// queries about **our Chromecast service**: suppression edges, the PTR's
    /// alias equality, the NSEC it sends instead of silence for `AAAA`. Nothing has
    /// ever been asked of it about `_airplay._tcp`, and a responder that guesses a
    /// second service's rules is a responder that will answer with bytes nobody
    /// verified. So the AirPlay announcement sends and stays quiet.
    ///
    /// A method rather than a field, because the answer is a fact about the type —
    /// and the one place that reads it (`Announcer::start`) cannot then be told a
    /// different story by a caller that constructed the struct by hand.
    pub fn responder_answers(&self) -> bool {
        self.service_type == SERVICE_TYPE
    }

    /// The TXT record's rdata: every property length-prefixed, in table order.
    ///
    /// **One producer for these bytes**, because three readers need them to agree
    /// character for character: [`Service::datagram`] writes them into an
    /// announcement, `respond::datagram` writes them into an answer, and
    /// [`crate::aggregate::plan_identity`] hashes them into a record's identity. If
    /// the identity ever drifted from the bytes, the ledger would file a key under
    /// a name the wire never carried, and the flood protection would stop firing
    /// with no symptom short of a second Chromecast menu asking six times and
    /// hearing six answers back.
    pub(crate) fn txt_rdata(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value) in &self.txt {
            let property = property_bytes(key, value);
            body.push(property.len() as u8);
            body.extend_from_slice(&property);
        }
        body
    }

    /// `ttl = None` means "each record's own TTL", the way
    /// `_add_broadcast_answer` passes it; `Some(n)` overrides every one of them,
    /// which is how `unregister` gets zeros everywhere.
    fn datagram(&self, ttl: Option<u32>, tail: TailOrder) -> Vec<u8> {
        let other = ttl.unwrap_or(OTHER_TTL);
        let host = ttl.unwrap_or(HOST_TTL);
        // Borrowed out before the closures start mutating the writer, so the
        // borrow checker argues with the wire format and not with the design.
        let instance = self.instance.as_str();
        let server = self.server.as_str();
        let service_type = self.service_type.as_str();
        let addresses = self.addresses.as_slice();
        let port = self.port;

        let mut w = Writer::new(FLAGS_ANNOUNCE);
        // 1. PTR: the service type says this instance exists.
        w.record(service_type, TYPE_PTR, other, false, |w| w.name(instance));
        // 2. SRV: the instance has a host and a port.
        w.record(instance, TYPE_SRV, host, true, |w| {
            w.u16(0); // priority — the reference writes 0 for both
            w.u16(0); // weight
            w.u16(port);
            w.name(server);
        });
        // 3. TXT: the properties, in order, each one length-prefixed — the same
        // bytes [`txt_rdata`](Self::txt_rdata) names, so an announcement and an
        // answer cannot drift apart from the identity the ledger files them under.
        let txt_rdata = self.txt_rdata();
        w.record(instance, TYPE_TXT, other, true, |w| {
            w.octets(&txt_rdata);
        });
        // 4 and 5. The address pair, in whichever order the caller asked for.
        let write_nsec = |w: &mut Writer| {
            w.record(server, TYPE_NSEC, host, true, |w| {
                w.name(server);
                w.u8(0); // window 0 — mDNS never uses another
                w.u8(1); // one octet of bitmap follows
                w.u8(0x40); // bit for type 1 (A), counted from the high bit
            });
        };
        let write_addresses = |w: &mut Writer| {
            for address in addresses {
                w.record(server, TYPE_A, host, true, |w| {
                    w.octets(&address.octets());
                });
            }
        };
        match tail {
            TailOrder::NsecFirst => {
                write_nsec(&mut w);
                write_addresses(&mut w);
            }
            TailOrder::AFirst => {
                write_addresses(&mut w);
                write_nsec(&mut w);
            }
        }
        w.finish()
    }
}

/// A TXT property as it goes on the wire: `key=value`, or the bare key when
/// there is no value. This is the reference's rule
/// (`_services/info.py`'s `_set_properties`), and it matters for `fn` — an empty
/// friendly name would otherwise write `fn=` where the reference writes `fn`.
/// The reference's `pk` value: `base64.b64encode(b"macast-airplay-public-key")`.
///
/// It is a **fixed string**, not a key — the reference generates no pair material
/// and its AirPlay implementation does not use it for anything the receiver reads
/// back. Reproduced as bytes rather than by pulling in a base64 crate for one
/// constant; the corpus line is the witness that this string is what goes out.
fn b64_macast_public_key() -> &'static str {
    "bWFjYXN0LWFpcnBsYXktcHVibGljLWtleQ=="
}

pub(crate) fn property_bytes(key: &str, value: &[u8]) -> Vec<u8> {
    if value.is_empty() {
        return key.as_bytes().to_vec();
    }
    let mut body = key.as_bytes().to_vec();
    body.push(b'=');
    body.extend_from_slice(value);
    body
}

/// Shorten any property that cannot fit one length byte, and report which ones
/// were touched.
///
/// The unit is the **whole `key=value` string**, because that is what gets the
/// length prefix — so a long key costs the value room, exactly as it does in the
/// reference's packer. Cutting on a `char` boundary so a truncated CJK friendly
/// name cannot end in half a code point, which would reach a sender as a broken
/// string rather than a short one (`mdns.rs`'s name-length red line is the same
/// argument read from the other side: never let a byte boundary invent a
/// character).
pub fn clip_txt(txt: Vec<(String, Vec<u8>)>) -> (Vec<(String, Vec<u8>)>, Vec<(String, usize)>) {
    let mut clipped = Vec::new();
    let mut out = Vec::with_capacity(txt.len());
    for (key, value) in txt {
        let total = property_bytes(&key, &value).len();
        if total <= MAX_TXT_PROPERTY {
            out.push((key, value));
            continue;
        }
        let room = MAX_TXT_PROPERTY.saturating_sub(key.len() + 1);
        let mut keep = String::new();
        let source = String::from_utf8_lossy(&value).to_string();
        for ch in source.chars() {
            let candidate_len = keep.len() + ch_len(ch);
            if candidate_len > room {
                break;
            }
            keep.push(ch);
        }
        clipped.push((key.clone(), total));
        out.push((key, keep.into_bytes()));
    }
    (out, clipped)
}

/// UTF-8 width of one char, without pulling in a `unicode-segmentation` style
/// dependency for a comparison this file makes once per property.
fn ch_len(ch: char) -> usize {
    ch.len_utf8()
}

/// `sanitize_instance_name` (`discovery.py:36-58`).
///
/// The rule is not cosmetic. A DNS-SD instance name is **one label** of
/// `[A-Za-z0-9-]`, at most 63 bytes, and Macast's friendly name predates mDNS:
/// it is built for DLNA, where it is free text, and the default is
/// `Macast(MyHost-1754.local)` — parentheses and dots included. The reference's
/// own comment says what happens when the rule is missed:
///
/// > zeroconf accepts such a name without complaint and reports success, but the
/// > record never matches a browse query, so the device is silently invisible.
///
/// The order of the steps is load-bearing, and `tests/golden/mdns_names.jsonl`
/// pins each of them: strip a trailing service-type suffix, keep only the
/// **leading** label, replace illegal characters with `-` (per character, so
/// `Macast 1` becomes `Macast-1` and `Mäcast(local)` becomes `M-cast-local`),
/// strip leading and trailing `-`, then truncate to 63, and fall back to
/// `Macast` only when nothing is left. A name in CJK loses every character and
/// takes the fallback — so two Chinese-named instances *do* collide; that is the
/// reference's behaviour too, and it is on the record rather than hidden by
/// reordering these steps.
pub fn sanitize_instance_name(name: &str, service_type: &str, fallback: &str) -> String {
    let suffix = if service_type.ends_with('.') {
        service_type.to_string()
    } else {
        format!("{}.", service_type)
    };
    let without = suffix.trim_end_matches('.');
    let stem = if let Some(rest) = name.strip_suffix(suffix.as_str()) {
        rest.trim_end_matches('.')
    } else if let Some(rest) = name.strip_suffix(without) {
        rest.trim_end_matches('.')
    } else {
        name.trim_end_matches('.')
    };
    // Only the leading label survives: `Macast(host.local)` contributes
    // `Macast(host`, which becomes `Macast-host`.
    let first_label = stem.split('.').next().unwrap_or("");
    let replaced: String = first_label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = replaced.trim_matches('-');
    let capped: String = trimmed.chars().take(MAX_LABEL).collect();
    // `take(MAX_LABEL)` counts **chars**, while Python's slice counts code
    // points and the wire counts bytes. For this alphabet the three agree — the
    // replacement above leaves only ASCII — so the cap is a byte cap too, and
    // the audit test says so in the only way that can be checked: a name that
    // reaches the wire with a label over 63 bytes cannot exist.
    let label = if capped.is_empty() {
        fallback.to_string()
    } else {
        capped
    };
    format!("{}.{}", label, suffix)
}

/// `_normalize_server` (`discovery.py:113-120`): a FQDN under `.local.`, without
/// doubling the suffix, falling back to the machine hostname when handed
/// nothing.
pub fn normalize_server(host: &str) -> String {
    let trimmed = host.trim_end_matches('.');
    let base = if trimmed.is_empty() {
        machine_hostname()
    } else {
        trimmed.to_string()
    };
    if base.to_lowercase().ends_with(".local") {
        format!("{}.", base)
    } else {
        format!("{}.local.", base)
    }
}

/// `socket.gethostname()`, reached only when [`normalize_server`] is handed an
/// empty name. `HOSTNAME` first, then the macOS/BSD `scutil --get HostName`
/// alternative is deliberately *not* tried: this is a fallback behind a fallback,
/// and `localhost` keeps the packet well-formed (one owner name, matching A
/// records) while the announcement that produced it is logged. Spawning a
/// helper binary from the advertiser would be a second way for the advertiser to
/// fail.
pub fn machine_hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string())
}

// ---------------------------------------------------------------------------
// The packet writer
// ---------------------------------------------------------------------------

/// A packet under construction, with the name-compression table that makes the
/// bytes match the reference.
///
/// The table is why this is a struct and not a `Vec<u8>`: `write_name`
/// (`outgoing.py:285-327`) records the offset of **every suffix** of every name
/// it writes, in the order they first appear, and later names point into it.
/// Skipping the recording step still produces a packet that parses — it is just
/// not the packet the reference sends, and the difference is most of the 296
/// bytes the corpus pins.
pub(crate) struct Writer {
    buf: Vec<u8>,
    /// (name without its trailing dot, byte offset of its first label)
    names: Vec<(String, usize)>,
    answers: u16,
    additionals: u16,
}

impl Writer {
    pub(crate) fn new(flags: u16) -> Writer {
        let mut buf = Vec::with_capacity(320);
        buf.extend_from_slice(&0u16.to_be_bytes()); // transaction id
        buf.extend_from_slice(&flags.to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]); // qd / an / ns / ar, written last
        Writer {
            buf,
            names: Vec::new(),
            answers: 0,
            additionals: 0,
        }
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn octets(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// `write_name`: the whole name if it has never been written; otherwise a
    /// two-byte pointer; and if only a *suffix* is known, write the labels up to
    /// where the known part begins and point there.
    pub(crate) fn name(&mut self, name: &str) {
        let trimmed = name.strip_suffix('.').unwrap_or(name);
        if let Some(&(_, offset)) = self.names.iter().find(|(held, _)| held == trimmed) {
            self.pointer(offset);
            return;
        }
        let start = self.buf.len();
        let labels: Vec<&str> = trimmed
            .split('.')
            .filter(|label| !label.is_empty())
            .collect();
        if labels.is_empty() {
            self.buf.push(0);
            return;
        }
        self.names.push((trimmed.to_string(), start));
        self.label(labels[0]);
        // Suffix offsets come from the byte length of the whole name: a suffix
        // begins `name_length - len(suffix)` bytes after the first label. The
        // reference computes that lazily on the second label; the value is the
        // same, and a one-label name never pays for it.
        let name_length = trimmed.as_bytes().len();
        for count in 1..labels.len() {
            let partial = labels[count..].join(".");
            if let Some(&(_, offset)) = self.names.iter().find(|(held, _)| held == &partial) {
                self.pointer(offset);
                return;
            }
            let offset = start + name_length - partial.as_bytes().len();
            self.names.push((partial, offset));
            self.label(labels[count]);
        }
        self.buf.push(0);
    }

    fn label(&mut self, label: &str) {
        let bytes = label.as_bytes();
        // A label longer than 63 bytes means a caller built a [`Service`] past
        // [`sanitize_instance_name`]. Truncating quietly would write a packet
        // that owns a name nothing else answers for, so this refuses the byte
        // rather than the name: debug builds assert, release builds write the
        // length the string actually has and let the record fail loudly — the
        // reference raises `NamePartTooLongException` here, and its own limit is
        // 64 bytes (`outgoing.py:475-481`), which is one past the DNS rule.
        debug_assert!(
            bytes.len() <= MAX_LABEL,
            "mDNS label over {} bytes: {}",
            MAX_LABEL,
            label
        );
        self.u8(bytes.len() as u8);
        self.buf.extend_from_slice(bytes);
    }

    fn pointer(&mut self, offset: usize) {
        self.u8((0xC0 | (offset >> 8)) as u8);
        self.u8((offset & 0xFF) as u8);
    }

    /// One resource record: owner name, type, class (with the flush bit when the
    /// record is unique), TTL, then rdata whose length is patched in afterwards
    /// — nothing knows it until the closure has written it.
    pub(crate) fn record<F>(&mut self, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
    where
        F: FnOnce(&mut Writer),
    {
        self.write_record(name, rtype, ttl, unique, rdata);
        self.answers += 1;
    }

    /// The same record, counted into the **additional** section instead.
    ///
    /// A response is not an announcement: the reference puts the record the
    /// question earned in AN and everything that rides along in AR, and the
    /// corpus rows carry that split. `finish()` used to write the AN count and
    /// leave AR at zero, which was right for the five-record announcement and
    /// would be wrong for every answer — a client that walks only AN would read
    /// an answer with no host under it.
    pub(crate) fn record_additional<F>(&mut self, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
    where
        F: FnOnce(&mut Writer),
    {
        self.write_record(name, rtype, ttl, unique, rdata);
        self.additionals += 1;
    }

    fn write_record<F>(&mut self, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
    where
        F: FnOnce(&mut Writer),
    {
        self.name(name);
        self.u16(rtype);
        let class = if unique { CLASS_IN | CLASS_UNIQUE } else { CLASS_IN };
        self.u16(class);
        self.u32(ttl);
        let length_at = self.buf.len();
        self.u16(0);
        let rdata_at = self.buf.len();
        rdata(self);
        let written = (self.buf.len() - rdata_at) as u16;
        self.buf[length_at..length_at + 2].copy_from_slice(&written.to_be_bytes());
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        let mut buf = self.buf;
        buf[4..6].copy_from_slice(&0u16.to_be_bytes()); // questions
        buf[6..8].copy_from_slice(&self.answers.to_be_bytes());
        buf[8..10].copy_from_slice(&0u16.to_be_bytes()); // authority
        buf[10..12].copy_from_slice(&self.additionals.to_be_bytes());
        buf
    }
}

// ---------------------------------------------------------------------------
// The sender
// ---------------------------------------------------------------------------

/// What one burst did: how many datagrams it attempted, and the text of each
/// failure.
///
/// The failures come back as **strings** rather than `io::Error`s because the
/// only thing a reader of `logs/Discovery.log` can act on is the sentence, and
/// carrying a type that cannot be cloned through the announcer's loop would be a
/// second owner of the same information.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Burst {
    pub attempted: usize,
    pub failures: Vec<String>,
}

impl Burst {
    /// True when every attempt went out. Not "the burst is interesting"; a burst
    /// with a failure is the state where a sender may not have heard us, and the
    /// announcer says so once per failure rather than once per run.
    pub fn clean(&self) -> bool {
        self.failures.is_empty()
    }
}

/// One burst: [`BROADCASTS`] datagrams, `gap` apart.
///
/// `abort` is `Some(flag)` for the **announcement** bursts — where a stop means
/// "we are going away, do not keep shouting" — and `None` for the **goodbye**.
/// That asymmetry is the whole point of the parameter and not a style choice:
/// [`Announcer::stop`] clears `running` *before* the thread reaches the goodbye,
/// so a goodbye that consulted the same flag would send **zero** packets at the
/// one moment its existence matters. A sender that never hears the goodbye keeps
/// the PTR for [`OTHER_TTL`] — 75 minutes of a dead device in a phone's Cast
/// menu, which is the same class of complaint as a menu full of machines that
/// are not there.
///
/// `send` is a closure rather than a socket or a trait so the policy can be
/// asked without binding 5353 — which on this machine is shared with the OS
/// responder and with every other receiver that happens to be running. The gap
/// is a parameter for the same reason `relay`'s wait budget was synthesised
/// (§8's 「等待预算是接缝」 row): the policy is one thing ("say it three times,
/// with this gap") and a test that wants the policy without waiting 675 ms must
/// be able to ask for it.
pub fn burst(
    send: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    bytes: &[u8],
    gap: Duration,
    abort: Option<&AtomicBool>,
) -> Burst {
    let mut out = Burst::default();
    for index in 0..BROADCASTS {
        if abort.map(|flag| !flag.load(Ordering::SeqCst)) == Some(true) {
            break;
        }
        out.attempted += 1;
        if let Err(e) = send(bytes) {
            // A failure is counted and *not* retried inside the burst: the
            // reference's `async_send` drops the packet and carries on
            // (`_core.py:556-566` logs an oversized drop and returns), and the
            // next set is `REANNOUNCE` away. Retrying here would be a second
            // behaviour nobody asked the reference about.
            out.failures.push(format!("burst packet {} of {}: {}", index + 1, BROADCASTS, e));
        }
        if index + 1 < BROADCASTS {
            interrupted_sleep(gap, abort);
        }
    }
    out
}

/// Sleep in slices the stop flag can interrupt, so a shutdown never sits on the
/// [`REANNOUNCE`] timer. `None` means "nothing may cut this short", which is what
/// the goodbye's gap is: three packets 125 ms apart, all of them, always.
fn interrupted_sleep(for_: Duration, abort: Option<&AtomicBool>) {
    let until = Instant::now() + for_;
    while Instant::now() < until {
        if abort.map(|flag| !flag.load(Ordering::SeqCst)) == Some(true) {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The announcement loop, on its own socket.
///
/// Owns nothing about playback and nothing about a Cast session: it is a
/// broadcaster, started and stopped with the receiver.
pub struct Announcer {
    /// **All** the services this machine announces, on **one** socket.
    ///
    /// This used to be one `Service` per `Announcer`, i.e. one bound, joined socket on
    /// 5353 per service type. The reference does not work that way: `discovery.py`'s
    /// `discover()` is a singleton and both protocols register into it, so the Python
    /// app holds one announcing socket no matter how many services it advertises. The
    /// cost of the per-service shape was measured, not guessed
    /// (`scripts/mdns_reuseport_probe.py`, two kernels): multicast is duplicated to
    /// every joined socket, but **a unicast datagram goes to exactly one of them, and
    /// which one varies with the flow** — so a third competing socket made "the socket
    /// that answers" a coin flip for unicast queriers. One socket, N services removes
    /// the competition and is the reference's own shape.
    services: Vec<Service>,
    log: Arc<Logger>,
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Announcer {
    pub fn new(services: Vec<Service>, log: Arc<Logger>) -> Arc<Announcer> {
        Arc::new(Announcer {
            services,
            log,
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        })
    }

    pub fn services(&self) -> &[Service] {
        &self.services
    }

    /// The service of one type, for the shell's summary lines and for tests that ask
    /// "did *this* one get announced".
    pub fn service_for(&self, service_type: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.service_type == service_type)
    }

    /// The interfaces to join: every address any service publishes, deduplicated.
    ///
    /// All of them, because the socket must hear a query about *any* service it
    /// answers; the narrow set is already applied per service by [`advertisable`], so
    /// this union cannot widen what a sender is told.
    fn join_addresses(&self) -> Vec<Ipv4Addr> {
        let mut out: Vec<Ipv4Addr> = Vec::new();
        for service in &self.services {
            for address in &service.addresses {
                if !out.contains(address) {
                    out.push(*address);
                }
            }
        }
        out
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Bind and announce. The `Err` path has already logged its reason, because a
    /// machine whose mDNS is blocked must still run the rest of the receiver —
    /// the reference swallows the equivalent into `advertise() -> False` with
    /// 「mDNS unavailable, %s will not be discoverable: %s」.
    ///
    /// The `%s` there is **the service type the caller passed** (`discovery.py:268-269`
    /// formats `service_type`), and this port names it the same way. It used to say
    /// "Chromecast" in both sentences, which was a divergence from the reference
    /// *and* became a lie the day the same module started announcing `_airplay._tcp`:
    /// an iPhone user whose AirPlay advertisement failed would be told his Chromecast
    /// is undiscoverable, and would go looking in the Cast settings. AGENTS.md §4.2's
    /// 「日志指错嫌疑人比不指更贵」, one layer down: the sentence has to name the thing
    /// that actually failed.
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        let addresses = self.join_addresses();
        if addresses.is_empty() {
            // Not a comment about the network: this is the case where the receivers are
            // up on their own ports and **no sender can learn any of them**, so the
            // sentence names every type that lost its announcement.
            let types: Vec<&str> = self
                .services
                .iter()
                .map(|s| s.service_type.as_str())
                .collect();
            self.log.warning(&format!(
                "mDNS unavailable, {} will not be discoverable: \
                 no advertisable address to publish",
                types.join(", "),
            ));
            return Err("no advertisable address to publish".to_string());
        }
        let engine = match bind_engine(&addresses, &self.log) {
            Ok(sock) => sock,
            Err(e) => {
                let types: Vec<&str> = self
                    .services
                    .iter()
                    .map(|s| s.service_type.as_str())
                    .collect();
                self.log.error(&format!(
                    "mDNS unavailable, {} will not be discoverable: {}",
                    types.join(", "),
                    e
                ));
                return Err(e.to_string());
            }
        };
        self.running.store(true, Ordering::SeqCst);
        let running = Arc::clone(&self.running);
        let log = Arc::clone(&self.log);
        let services = self.services.clone();
        let started = thread::Builder::new()
            .name("MDNS_KEEPALIVE".to_string())
            .spawn(move || {
                let sock = engine.sock;
                let egress = engine.egress;
                let group = SocketAddr::from((MDNS_GROUP, MDNS_PORT));
                // Every group packet this thread writes leaves through every interface the
                // announcement publishes — see [`send_group`] for the measurement that made
                // this load-bearing. The warning is **once per run**, because a card that
                // refuses membership is a permanent condition and §4.10's log is full of
                // sentences that repeated a true thing on every packet.
                let mut complained = false;
                let mut send = |bytes: &[u8]| {
                    let (copies, refused) = send_group(&sock, group, bytes, &egress);
                    if !refused.is_empty() && !complained {
                        complained = true;
                        log.warning(&format!(
                            "mDNS cannot send through {} — the announcement leaves the other \
                             interface(s) only, so a sender on that segment will not find this \
                             Macast; 设置 → 网络, or 自动选择 in the picker, is the way to give \
                             it back [{}]",
                            refused.join(", "),
                            refused.len()
                        ));
                    }
                    if copies == 0 {
                        Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            "the mDNS group accepted no copy of this packet",
                        ))
                    } else {
                        Ok(())
                    }
                };
                let mut unicast =
                    |bytes: &[u8], peer: SocketAddr| sock.send_to(bytes, peer).map(|_| ());
                let mut buf = vec![0u8; crate::mdns::READ_MAX];
                // The loop says how long it can wait, and the answer becomes this
                // socket's receive timeout: an aggregated answer whose random interval
                // has already run must not also wait for a poll boundary, while the
                // 60-second re-announcement still gets asked for on schedule. Setting
                // the option per call costs one syscall at a few hertz.
                let mut recv = |wait: Duration| {
                    let _ = sock.set_read_timeout(Some(wait));
                    match sock.recv_from(&mut buf) {
                    Ok((length, peer)) => {
                        buf.truncate(length);
                        Ok(Some((buf.clone(), peer)))
                    }
                    // The poll elapsing is not an event: nothing arrived, so do the
                    // rest of the loop's work and ask again.
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        Ok(None)
                    }
                    Err(e) => Err(e),
                    }
                };
                run(
                    &services,
                    &mut send,
                    &mut unicast,
                    &mut recv,
                    &running,
                    &log,
                )
            });
        match started {
            Ok(handle) => {
                *self.handle.lock().unwrap() = Some(handle);
                // `discovery.py:294`: 「mDNS advertised %s on port %d」 — and
                // `protocol_cast.py:415`'s 「ChromecastProtocol started on port
                // %s」 is the channel's own line, not this thread's. One line per
                // service: the shell's summary already names them, and a reader asking
                // "which port did AirPlay get" must not have to parse a list.
                for service in &self.services {
                    self.log.info(&format!(
                        "mDNS advertised {} on port {}",
                        service.instance, service.port
                    ));
                }
                Ok(())
            }
            Err(e) => {
                // `self.running`, not the moved clone: the closure took its Arc
                // with it when `spawn` failed, and the flag the rest of the
                // object reads is still ours to clear.
                self.running.store(false, Ordering::SeqCst);
                self.log
                    .error(&format!("mDNS announce thread could not start: {}", e));
                Err(e.to_string())
            }
        }
    }

    /// Stop the loop, then let it send its goodbye. The thread sends it on its
    /// own way out (it still holds the socket), so `stop` joins rather than
    /// writing a second packet from a second thread.
    pub fn stop(&self) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

/// One socket, bound to 5353 with the reuse options, **joined**, with a short read
/// timeout so the announcer can poll it.
///
/// It used to be a sender only — unjoined, unread, and defended by a claim about
/// `SO_REUSEPORT` that turned out not to be what either kernel does
/// (`scripts/mdns_reuseport_probe.py`; the module header's first divergence carries
/// the readings). Joining is what makes *answering* possible at all, and the
/// measurement is what makes it safe: multicast is **duplicated** to every joined
/// socket, so this one does not take datagrams away from [`crate::mdns`]'s browser.
///
/// `interfaces` is a parameter rather than a read of `service.addresses` for the
/// same reason `burst`'s `send` is a closure: the join list is the fact under test,
/// and a test that wants to join on `127.0.0.1` (the only place a loopback witness
/// can hear anything) must not have to fake an interface the machine does not have.
/// The product passes the service's own addresses — the narrow set an announcement
/// is allowed to name (`advertised_addresses`, not the browser's superset, which is
/// §22's product bug). A querier that can reach us is on one of these; joining
/// elsewhere would only hear traffic we cannot answer on.
fn bind_engine(interfaces: &[Ipv4Addr], log: &Logger) -> io::Result<Engine> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    crate::mdns::set_reuse_for_mdns(&sock, log);
    sock.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into())?;
    // The responder's two options, from `set_respond_socket_multicast_options`.
    sock.set_multicast_ttl_v4(MULTICAST_TTL as u32)?;
    sock.set_multicast_loop_v4(true)?;
    let mut joined = 0usize;
    let mut took: Vec<String> = Vec::new();
    let mut egress: Vec<Ipv4Addr> = Vec::new();
    for address in interfaces {
        match sock.join_multicast_v4(&MDNS_GROUP, address) {
            Ok(()) => {
                joined += 1;
                took.push(address.to_string());
                egress.push(*address);
            }
            Err(e) => log.warning(&format!(
                "mDNS cannot join {} on {}: {}",
                MDNS_GROUP, address, e
            )),
        }
    }
    if joined == 0 {
        // Announce anyway: the unsolicited bursts leave from a bound socket whether
        // or not a membership took, and a listener-only failure must not cost the
        // receiver its discovery. But say which half broke — "searchable but
        // unreachable" is otherwise indistinguishable from a network problem
        // (§4.1's first row).
        log.warning(&format!(
            "mDNS joined {} on no interface ({} addresses offered) — this receiver \
             will announce but will not answer queries",
            MDNS_GROUP,
            interfaces.len()
        ));
    }
    let sock: UdpSocket = sock.into();
    // The poll, not an infinite block: see [`run`]'s `recv`.
    sock.set_read_timeout(Some(READ_POLL))?;
    // And say out loud what this socket can now do, with the join count in the
    // sentence. Before this line existed, "the receiver never answered a query" had
    // three indistinguishable readings in the log — nothing bound, bound but not
    // joined, joined but deaf — and telling them apart took two machines and an hour
    // of probes. §4.2's 「别把端口在听当证据」 cuts both ways: the answer is the
    // membership, not the bind, so the membership is what gets reported.
    log.info(&format!(
        "mDNS engine on {} joined {} of {} interface(s) [{}]; {}",
        MDNS_PORT,
        joined,
        interfaces.len(),
        took.join(", "),
        "it answers queries for every service type registered on it"
    ));
    Ok(Engine {
        sock,
        joined,
        offered: interfaces.len(),
        egress,
    })
}

/// The socket plus what it managed to join.
///
/// `joined == 0` is the state where announcements still leave and queries never
/// arrive — the shape a user reads as "搜到但投不上" and a log without this number
/// cannot name.
pub(crate) struct Engine {
    sock: UdpSocket,
    /// The interfaces whose membership **took**, in the order offered — the list every
    /// group send fans out over ([`send_group`]).
    ///
    /// This is not a cosmetic duplicate of `joined`: the count is what the log sentence
    /// prints, while the list is what the socket sends through. They are the same
    /// information in two shapes because the two readers ask different questions — "did
    /// any membership take" and "how many copies should this burst leave as".
    pub egress: Vec<Ipv4Addr>,
    /// Read only by `announce_tests::e8`, which asserts the join count on the
    /// struct instead of parsing the log sentence that prints it — so the dead-code
    /// warning here is the product build, not a field nobody asked for. Deleting
    /// these two would move the witness onto wording, which is the coupling the
    /// module's other log-name assertions exist to avoid.
    #[allow(dead_code)]
    pub joined: usize,
    #[allow(dead_code)]
    pub offered: usize,
}

/// One packet, out **every** interface this announcement publishes.
///
/// This is what makes 网卡 pinning mean something on the wire. Until it existed the
/// engine set no outgoing interface at all, so the kernel chose one by route and the
/// pin only changed the **A records**: measured here on 2026-10-20, after
/// `set-interface=bridge100` the announcer logged `joined 1 of 1 interface(s)
/// [192.168.139.3]` and `over [192.168.139.3]`, while a listener joined on 192.168.139.3
/// counted **0** datagrams and the three-packet burst still arrived on 192.168.1.5. The
/// page said 「已切换到 bridge100」 and a sender on that segment heard nothing — a control
/// whose only effect was on a document.
///
/// The reference gets this for free from its shape: `discovery.py:222` hands zeroconf the
/// narrowed address list, zeroconf makes **one socket per interface**, and each of them
/// sends the same bytes (§22 measured six sockets, each string sent once per socket). This
/// port has one socket because the merged engine is what makes unicast queries reliable
/// (§16 第 32 条), so it reproduces the fan-out by rotating `IP_MULTICAST_IF` and sending
/// once per interface. Same on-wire shape, one socket.
///
/// Answers are fanned out the same way as bursts, and that is also the reference's
/// behaviour rather than a choice: its per-interface sockets each *hear* a multicast query
/// (multicast is duplicated to every joined socket — measured on two kernels) and each
/// answers, so a querier there receives one copy per interface too.
///
/// Returns `(copies, refused)`. `refused` names the addresses the kernel would not accept
/// as the outgoing interface are **skipped, not fatal, and not sent anyway**: one dead card
/// out of three must not cost the announcement the other two, and a copy that leaves
/// through *some other* card while naming the dead one's address is exactly the
/// 「搜到但投不上」 shape §4.1's first row is about. This is also what the reference's
/// UDP sender does with an adapter that refuses the option (`sock.rs:16-23`:
/// `WSAEADDRNOTAVAIL` on a hotspot/ICS adapter ⇒ the caller skips it).
///
/// An empty `egress` — every join failed — sends one copy and lets the route decide,
/// which is the "announce anyway" rule [`bind_engine`] already logs: no membership is a
/// listener problem, not a reason to stop publishing.
pub fn send_group(
    sock: &UdpSocket,
    group: SocketAddr,
    bytes: &[u8],
    egress: &[Ipv4Addr],
) -> (usize, Vec<String>) {
    let mut copies = 0usize;
    let mut refused = Vec::new();
    if egress.is_empty() {
        return (usize::from(sock.send_to(bytes, group).is_ok()), refused);
    }
    for address in egress {
        if let Err(e) = pin_multicast_if(sock, *address) {
            refused.push(format!("{}: {}", address, e));
            continue;
        }
        if sock.send_to(bytes, group).is_ok() {
            copies += 1;
        }
    }
    (copies, refused)
}

/// Pin the outgoing multicast interface for one send.
///
/// `std::net::UdpSocket` has no setter for `IP_MULTICAST_IF`. It does not need a raw
/// `setsockopt` either: socket2's [`socket2::SockRef`] borrows the option off any
/// `AsFd`, which is how [`crate::announce_tests`]’s loopback querier already pins its
/// egress — so this file borrows the crate's existing dependency instead of adding a
/// second platform shape to maintain (contrast `macast-upnp/src/sock.rs:269-276`, which
/// goes raw because *that* option, `SO_REUSEPORT`, is behind socket2's `all` feature).
///
/// **The address form is what moves the egress here, measured.** `sock.rs:28-36` says the
/// BSDs want the interface *index* — the shape Python's `ssdp.py` packs for its SSDP
/// sender — and this port's SSDP sender follows that. For a group send on this Mac the
/// address form works: a datagram sent to `224.0.0.251` with `IP_MULTICAST_IF` set to
/// `192.168.139.3` arrived at a listener joined on that card and **not** at one joined on
/// the default route's `192.168.1.5` (probe run 2026-10-20, the measurement behind
/// [`send_group`]). `sock.rs:31-33` also records that Windows accepts **only** the address
/// form, so this one call is the shape both of the other platforms want as well; the
/// Windows leg is not measured here and §16 第 39 条 says so.
fn pin_multicast_if(sock: &UdpSocket, address: Ipv4Addr) -> io::Result<()> {
    socket2::SockRef::from(sock).set_multicast_if_v4(&address)
}

/// The interfaces an announcement may name — the **narrowed** set, in one place.
///
/// Two service types publishing two different lists is AGENTS.md §4.1's first row
/// ("搜到、投不上") with an extra step: the sender picks one address for the
/// service it likes, so a Chromecast on the LAN address and an AirPlay on a VM
/// bridge is one unreachable record set and one lucky one. It is also what
/// §22 of the acceptance script compares against what a second stack reads off
/// the wire, so a count is not enough and a list is.
fn advertisable(setting: &Setting, log: &Logger) -> Vec<Ipv4Addr> {
    let mut addresses = Vec::new();
    for text in macast_core::net::advertised_addresses(setting) {
        match text.parse::<Ipv4Addr>() {
            Ok(addr) => addresses.push(addr),
            Err(_) => log.warning(&format!(
                "mDNS announce skipping non-IPv4 interface {}",
                text
            )),
        }
    }
    addresses
}

/// The clipping warnings, said once per construction rather than per burst.
fn report_clipped(service: &Service, log: &Logger) {
    for (key, bytes) in &service.clipped {
        // The reference's equivalent here is `start()` raising; ours names
        // the field, what was lost, and that the receiver still came up.
        log.warning(&format!(
            "mDNS TXT property {} is {} bytes, past the {}; shortened for the wire",
            key, bytes, MAX_TXT_PROPERTY
        ));
    }
}

/// How long a read may hold the announcer's thread before it is asked again.
///
/// The same budget `channel.rs` settled on for the Cast socket: long enough that
/// polling is not a busy loop, short enough that neither the [`REANNOUNCE`]
/// schedule nor [`Announcer::stop`] can sit behind it.
const READ_POLL: Duration = Duration::from_millis(200);

/// How many received queries this process names out loud (and, one level down, how
/// many queries that are not about it it still admits to having received).
///
/// Eight for each, and the number is a measurement rather than a taste: one run of
/// `verify_rust_receiver.sh` has §22's browser, §24's browse and §25's browser all
/// asking, and the cap that used to be 3 was spent before §25 got there. The INFO
/// half is gated on the query being about this service, because this machine's own
/// stack asks `Ptr _irohv1._udp.local.` about every 0.8 seconds — no cap that has to
/// outlast that chatter is a witness. One sentence each is not enough to tell a
/// reader that is alive from one that died after its first packet, and an unbounded
/// sentence is §4.10's measured 49%-of-one-file shape. The cap is a fact about
/// diagnosis rather than about traffic, because `verify_rust_receiver.sh` §25 reads
/// this pair to separate "nothing arrived" from "it arrived and we declined".
const HEARD_SENTENCES: usize = 8;

/// What one received datagram turned into.
///
/// The counters exist because **a drop is a fact, not an absence**. This socket now
/// receives (see the module header's first divergence, measured by
/// `scripts/mdns_reuseport_probe.py`): multicast is duplicated to every joined
/// socket, so everything else on this group arrives here too, and a **unicast**
/// datagram to port 5353 is handed to exactly one socket in the reuse group — which
/// may be this one.
///
/// `ingested` and `queued` were added when the aggregation queue landed, and both
/// are the reason the old `absorbed` sentence had to shrink. A response packet used
/// to be something this loop threw away; now it is the only source of the
/// last-second ledger, and an answer can be *taken* by a queue without having
/// reached the wire yet. A counter that means "on the wire" and a counter that means
/// "promised" must not share a name, or the summary line lies about packets nobody
/// has sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    /// Queries this datagram carried (of any type, answered or not).
    pub queries: usize,
    /// Replies written to the multicast group **on this call**.
    pub group: usize,
    /// Replies written back to the asker's address.
    pub direct: usize,
    /// Answers handed to a queue instead of to the wire. Not a send.
    pub queued: usize,
    /// Records a **response** packet contributed to the last-second ledger
    /// ([`crate::aggregate::Ledger`]), including the looped-back copy of our own
    /// answer — the only witness that ever makes the protected bucket reachable.
    pub ingested: usize,
    /// Packets the parser could not read at all. This used to be "a non-query
    /// datagram, dropped"; responses are read now, so the name covers only what
    /// genuinely goes nowhere, and the doc above is why that distinction is worth
    /// a field.
    pub absorbed: usize,
    /// Whether one of those queries asked about records this service holds — the
    /// decision layer's own answer, not a name comparison made here.
    pub for_us: bool,
    /// What was asked, in the words the parser read off the wire (`PTR
    /// _googlecast._tcp.local.`), first three at most. A byte count cannot tell a
    /// query for us from a query for someone else's printer, and the announcer's
    /// log line is the only place a reader learns which one arrived.
    pub asking: String,
}

impl Served {
    /// Did this query turn into anything at all — sent now or scheduled?
    ///
    /// The first-answer sentence asks this rather than `group + direct > 0`
    /// because an answer that went into the aggregation queue has not reached the
    /// wire yet, and a reader of §25 needs to learn "we replied" at the moment the
    /// query produced it, not a poll later. The group total stays bytes-only.
    pub fn answered_any(&self) -> bool {
        self.group + self.direct + self.queued > 0
    }
}

/// The questions a datagram asked, in the words a reader of a log wants:
/// `Ptr _googlecast._tcp.local. + A pavia-macbookpro.local.`, three at most.
///
/// The type is spelled the way [`crate::respond::Kind`] prints it, from
/// [`crate::respond::kind_of`] — the one table — and an rtype this port does not
/// model keeps its number (`type48 …`) instead of vanishing: a line that quietly
/// dropped the question it could not name would read as "nothing was asked", which
/// is the exact ambiguity §25 exists to remove.
fn describe_questions(questions: &[crate::mdns::Question]) -> String {
    const NAMED: usize = 3;
    let mut words: Vec<String> = questions
        .iter()
        .take(NAMED)
        .map(|question| {
            let kind = crate::respond::kind_of(question.rtype)
                .map_or_else(|| format!("type{}", question.rtype), |kind| format!("{kind:?}"));
            format!("{kind} {}", question.name)
        })
        .collect();
    if questions.len() > NAMED {
        words.push(format!("+{} more", questions.len() - NAMED));
    }
    words.join(" + ")
}

/// When did this responder last **hear** the record this answer would put on the
/// wire, if ever?
///
/// The protective half of a divergence, stated rather than hidden: one planned `A`
/// record expands to one record per advertisable address, and the reference buckets
/// *those* separately, so a machine with two interfaces could split one answer
/// across two queues. `Chosen` is one record and one bucket, so this takes the
/// newest sighting of any expansion — the reading that never shouts a record the
/// wire has just carried. The cost is that a fresh second address can hold a stale
/// first one for a second; the alternative reading would flood a record we did just
/// shout, which is the thing RFC 6762 §14 exists to stop.
fn last_heard(
    record: &crate::respond::Chosen,
    service: &Service,
    queues: &crate::aggregate::Outgoing,
    now_ms: u64,
) -> Option<u64> {
    crate::aggregate::plan_identity(record, service)
        .into_iter()
        .filter_map(|key| queues.heard.seen(&key, now_ms))
        .max()
}

/// Where one answer group goes: into a bucket of the aggregation scheduler, or
/// straight back to the asker as a unicast packet.
///
/// `Direct` is not a fourth bucket — the reference's `_ucast` set is written by the
/// caller and never touches a queue — but it has to be one of the answers
/// [`split_by_bucket`] can give, because `query_handler.py:245-248` routes an entire
/// QU-over-multicast answer there whenever the record is recent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Bucket(crate::respond::Timing),
    Direct,
}

/// Split one reply's answer groups by **where each one goes**, preserving the order in
/// which the routes first appear.
///
/// The reference loops over the answers of a reply and `continue`s into whichever
/// bucket each one lands in (`query_handler.py:106-125`), so a reply can occupy two
/// buckets at once and the two go out as two datagrams at two different times. A
/// port that answered the whole reply with one bucket would be right on every
/// corpus row (each row asks one question) and wrong on the first `ANY` that
/// arrives half a second after we shouted its TXT.
fn split_by_bucket(
    groups: &[crate::respond::Group],
    service: &Service,
    questions: &[crate::respond::Question],
    probe: bool,
    ucast_source: bool,
    queues: &crate::aggregate::Outgoing,
    now_ms: u64,
) -> Vec<(Route, Vec<crate::respond::Group>)> {
    use crate::respond::Timing;
    let mut buckets: Vec<(Route, Vec<crate::respond::Group>)> = Vec::new();
    let mut push = |route: Route, group: &crate::respond::Group| {
        match buckets
            .iter_mut()
            .find(|(held, _)| *held == route)
        {
            Some((_, held)) => held.push(group.clone()),
            None => buckets.push((route, vec![group.clone()])),
        }
    };
    for group in groups {
        if !ucast_source && group.from_qu {
            // `_has_mcast_within_one_quarter_ttl`, asked of the same ledger the
            // one-second question uses and with the same protective reading of a plan
            // that expands to several records (`last_heard` carries why). **Recent
            // means do not shout it again**: answer the asker alone. Inverting that
            // sentence is how a port turns every browser's Cast menu into six
            // group-addressed copies of a record the group already has.
            let recent = crate::aggregate::plan_identity(&group.answer, service)
                .into_iter()
                .any(|key| queues.heard.is_recent(&key, now_ms));
            push(
                if recent {
                    Route::Direct
                } else {
                    Route::Bucket(Timing::Now)
                },
                group,
            );
            continue;
        }
        let seen = last_heard(&group.answer, service, queues, now_ms);
        push(
            Route::Bucket(crate::respond::bucket(questions, probe, seen, now_ms)),
            group,
        );
    }
    buckets
}

/// Answer one received datagram, sending through the two closures and scheduling
/// through `queues`.
///
/// `peer` is the source address, and its **port** is the input the reference uses
/// to decide whether the querier wants a unicast reply (`ucast_source = port !=
/// 5353`, `query_handler.py:272`). That is why this function takes a `SocketAddr`
/// rather than an IP: the port is the question, and a caller that flattened it
/// would silently stop answering legacy unicast queriers.
///
/// Known answers are read from the **answer section only**, because that is what
/// the querier declared it holds; the additional section is records the querier is
/// *sending to us*.
///
/// `index` is this service's position in the answering list `run` built, and it
/// travels into the queue because a queued answer has to remember whose addresses
/// and whose TXT it means — the reference's queue holds record objects, which carry
/// that with them, and our plan layer deliberately does not.
pub fn serve(
    bytes: &[u8],
    peer: SocketAddr,
    service: &Service,
    index: usize,
    now_ms: u64,
    multicast: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    unicast: &mut dyn FnMut(&[u8], SocketAddr) -> io::Result<()>,
    queues: &mut crate::aggregate::Outgoing,
    log: &Logger,
) -> Served {
    use crate::respond::{self, Known, Question, Shape, Timing};
    let mut out = Served::default();
    let message = match crate::mdns::parse_message(bytes, &peer.to_string(), log) {
        Some(message) => message,
        None => {
            out.absorbed = 1;
            return out;
        }
    };
    if !message.is_query() {
        // `_listener.py:326`: a response is fed to the cache and nothing else. This
        // is where the flood protection learns anything — the datagram is very
        // often the kernel's looped-back copy of what this socket sent one poll
        // ago, which is the mechanism design doc §16 measured before writing.
        out.ingested = queues.heard.learn(&message.records, now_ms);
        return out;
    }
    if message.questions.is_empty() {
        // A query that asks nothing: `async_response` returns without an answer set.
        return out;
    }
    out.queries = message.questions.len();
    out.asking = describe_questions(&message.questions);
    let shape = Shape::new(&service.instance, &service.server, !service.addresses.is_empty());
    // The judgment the log line needs, taken from the decision layer rather than
    // from a name comparison here: if this says "not about us" the responder is
    // silent, so the two cannot disagree about whether a query was ours.
    let questions: Vec<Question> = message
        .questions
        .iter()
        .filter_map(|question| {
            respond::kind_of(question.rtype).map(|kind| Question {
                name: question.name.clone(),
                kind,
                unicast_request: question.unicast_request,
            })
        })
        .collect();
    out.for_us = respond::concerns(&questions, &shape);
    let known: Vec<Known> = message
        .records
        .iter()
        .filter(|record| record.section == crate::mdns::Section::Answer)
        .filter_map(|record| {
            let alias = match &record.rdata {
                crate::mdns::RData::Name(name) => Some(name.clone()),
                _ => None,
            };
            respond::kind_of(record.rtype).map(|kind| Known {
                kind,
                owner: record.name.clone(),
                ttl: record.ttl,
                alias,
            })
        })
        .collect();
    let ucast_source = peer.port() != MDNS_PORT;
    let probe = message.is_probe();
    for reply in respond::respond(&questions, &known, ucast_source, &shape) {
        if reply.unicast {
            let datagram = respond::datagram(&reply.records, service);
            match unicast(&datagram, peer) {
                Ok(()) => out.direct += 1,
                Err(e) => log.warning(&format!(
                    "mDNS answer could not be sent ({} bytes to {}): {}",
                    datagram.len(),
                    peer,
                    e
                )),
            }
            continue;
        }
        for (route, groups) in split_by_bucket(
            &reply.groups,
            service,
            &questions,
            probe,
            ucast_source,
            queues,
            now_ms,
        ) {
            match route {
                Route::Bucket(Timing::Now) => {
                    let records = respond::flatten(&groups);
                    let datagram = respond::datagram(&records, service);
                    match multicast(&datagram) {
                        Ok(()) => out.group += 1,
                        Err(e) => log.warning(&format!(
                            "mDNS answer could not be sent ({} bytes to {}): {}",
                            datagram.len(),
                            MDNS_GROUP,
                            e
                        )),
                    }
                }
                Route::Bucket(bucket) => {
                    out.queued += queues.enqueue(bucket, index, &groups, now_ms)
                }
                Route::Direct => {
                    // Unicast back to the asker, flush bits cleared by the same owner
                    // the ephemeral-port reply uses, and **nothing to the group** —
                    // that omission is the content of RFC 6762 §5.4's "respond with a
                    // unicast packet directed back to the querier". A port that also
                    // multicast it is the flood the rule exists to prevent when six
                    // browsers open a Cast menu in the same second.
                    let leg = respond::unicast_of(&groups);
                    let records = respond::flatten(&leg);
                    let datagram = respond::datagram(&records, service);
                    match unicast(&datagram, peer) {
                        Ok(()) => out.direct += 1,
                        Err(e) => log.warning(&format!(
                            "mDNS unicast answer could not be sent ({} bytes to {}): {}",
                            datagram.len(),
                            peer,
                            e
                        )),
                    }
                }
            }
        }
    }
    out
}

/// Render one queue batch into the single datagram it goes out as.
///
/// A public function because the flush has **two** readers with the same job:
/// [`run`] on the real socket, and `announce_tests`'s `serve` harness, which has to
/// see the bytes an aggregated answer eventually becomes. A test that reimplemented
/// this rendering would be a second owner of the batch-to-packet rule and would
/// drift the way every duplicate here has drifted — silently, and only on the
/// machine that reads it.
///
/// `answering[queued.service]` is why [`crate::aggregate::Queued`] carries an index:
/// one engine answers for every registered service, so a merged batch can name two
/// of them and each record must be rendered against the TXT table and addresses it
/// was planned for.
pub fn datagram_for(
    batch: &[crate::aggregate::Queued],
    answering: &[&Service],
) -> Option<Vec<u8>> {
    use crate::respond::Chosen;
    let mut parts: Vec<(Vec<Chosen>, &Service)> = Vec::new();
    for queued in batch {
        let Some(service) = answering.get(queued.service) else {
            // The index came from this same list, so reaching here means the list
            // changed under the queue. It cannot — `run` builds it once — and a batch
            // that lost its service is dropped rather than rendered against a guess
            // about whose port and whose TXT these records mean.
            continue;
        };
        parts.push((queued.records(), *service));
    }
    if parts.is_empty() {
        // `None`, not an empty `Vec`: a header-only packet is twelve bytes that
        // parse as "a response with nothing in it", which is a different answer to
        // give a reader than "this batch had nothing left to say".
        return None;
    }
    let refs: Vec<(&[Chosen], &Service)> = parts
        .iter()
        .map(|(records, service)| (records.as_slice(), *service))
        .collect();
    Some(crate::respond::datagram_multi(&refs))
}

/// Milliseconds since `start`, the one clock every scheduling decision here reads.
///
/// Monotonic rather than epoch like the reference's `current_time_millis()`, and
/// nothing in this loop compares it to anything outside itself: a wall clock that
/// steps (NTP correcting a lagging laptop) would strand a queued answer behind a
/// `send_after` it had already passed, or fire it a second early.
fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// The announcer's whole loop: shout, then answer questions until the next shout,
/// then goodbye.
///
/// `send` writes to the group, `unicast` writes back to one asker, `recv` asks the
/// socket for the next datagram and says how long it may hold the thread. All three
/// are closures for the reason `burst`'s `send` always was — the cadence and the
/// dispatch can then be driven without binding 5353, which on this machine is
/// shared with the OS responder and every other receiver running. The read is a
/// **poll**, not a blocking read, because the same thread owns the 60-second
/// re-announcement: a read that blocks forever would make the announcer silent
/// forever, and that is the §4.2 shape where "the receiver is up and nobody finds
/// it" reads like a network problem (`channel.rs`'s 「读不得跨阻塞持锁」 is the same
/// thing one layer down, where the lock was the casualty rather than the schedule).
///
/// The timeout is a parameter rather than a constant because of the queues: a fixed
/// 200 ms ceiling would add up to 200 ms to every aggregated answer on top of the
/// delay the reference itself chose, and an answer that has *already* been held for
/// its random interval has no reason to wait for a poll boundary too.
fn run(
    services: &[Service],
    send: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    unicast: &mut dyn FnMut(&[u8], SocketAddr) -> io::Result<()>,
    recv: &mut dyn FnMut(Duration) -> io::Result<Option<(Vec<u8>, SocketAddr)>>,
    running: &AtomicBool,
    log: &Logger,
) {
    use crate::aggregate::Outgoing;
    // One burst per service, the reference's own registration order (Cast first, then
    // AirPlay — the order the shell builds them in). Each burst is three packets, and
    // the gaps are the measured ones; sending them back to back is what the Python
    // engine does when two services are registered in the same `Zeroconf`.
    let mut bytes_per_service: Vec<Vec<u8>> = Vec::with_capacity(services.len());
    for service in services {
        let bytes = service.announcement();
        report(
            log,
            &burst(send, &bytes, REGISTER_GAP, Some(running)),
            "announcement",
        );
        bytes_per_service.push(bytes);
    }
    let start = Instant::now();
    let mut next = start + REANNOUNCE;
    let mut served = Served::default();
    let mut heard = 0usize;
    let mut other_heard = 0usize;
    // The service types this socket is allowed to answer. It is **not** "all of them":
    // `responder_answers()` is the one line that says which types the decision table was
    // measured for, and the merged socket keeps that distinction instead of quietly
    // widening it. A type that is not in this list still appears in every burst above —
    // announced, never answered, which is the shape §26 pins on the wire.
    let answering: Vec<&Service> = services.iter().filter(|s| s.responder_answers()).collect();
    let mut silent_notice = answering.is_empty();
    let mut queues = Outgoing::new();
    let mut first_answer_said = false;
    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        let now_ms = elapsed_ms(start);
        // Shortest of: the poll ceiling, the earliest queued deadline, and the next
        // re-announcement. The last one is what keeps a quiet network from parking
        // this thread past `REANNOUNCE` while a queue is empty.
        let reannounce_in = next.saturating_duration_since(Instant::now());
        let mut wait = READ_POLL.min(reannounce_in);
        if let Some(deadline) = queues.deadline(now_ms) {
            wait = wait.min(Duration::from_millis(deadline.saturating_sub(now_ms)));
        }
        match recv(wait) {
            Ok(Some((data, peer))) => {
                // The one place this loop can put a packet on the wire **in reply to
                // somebody else**. Every answering service gets the same datagram and
                // filters it by its own shape, so a query about `_airplay._tcp` finds
                // the AirPlay service and a query about `_googlecast._tcp` finds the
                // Cast one; `respond::concerns` is the shared matcher, not a second
                // name rule. With **no** answering service the datagram is still read
                // (a joined socket that is never read is the queue nobody drains that
                // `mdns_reuseport_probe.py` measured) and simply not answered.
                let mut one = Served::default();
                for (index, service) in answering.iter().enumerate() {
                    let part = serve(
                        &data,
                        peer,
                        service,
                        index,
                        now_ms,
                        send,
                        unicast,
                        &mut queues,
                        log,
                    );
                    one.queries += part.queries;
                    one.group += part.group;
                    one.direct += part.direct;
                    one.queued += part.queued;
                    one.ingested += part.ingested;
                    one.absorbed += part.absorbed;
                    one.for_us |= part.for_us;
                    if part.asking.len() > one.asking.len() {
                        // The sentence a human reads: the **widest** question set seen,
                        // because two services parse the same bytes and the longer
                        // description is the one that names every question asked.
                        one.asking = part.asking.clone();
                    }
                }
                if answering.is_empty() && one.ingested == 0 && one.absorbed == 0 {
                    if silent_notice {
                        silent_notice = false;
                        log.debug(
                            "mDNS engine received a query and did not answer it: this socket \
                             announces only, and no service type it carries has a measured \
                             responder table",
                        );
                    }
                }
                if one.queries > 0 && one.for_us && heard < HEARD_SENTENCES {
                    // The **receiving** question, asked separately from the answering
                    // one. "It never answered" has two causes and only this process can
                    // tell them apart: nothing was ever handed to this socket, or it was
                    // handed over and we declined.
                    //
                    // Counted only for queries about **us**, and the subject travels in
                    // the text. Both halves were measured on 2026-10-18, in two failed
                    // shapes each. Without the subject, `36 bytes` could be a Chromecast
                    // or this machine's own stack, which asks `Ptr
                    // _irohv1._udp.local.` about every 0.8 seconds — a cap of 3 spent
                    // itself inside four seconds and §25's cold browser then asked into a
                    // silent log, which got written down as "this binary receives no
                    // inbound LAN multicast". Without the subject gate the cap has to be
                    // larger than the chatter, and on a machine producing more than eight
                    // queries in the time it takes a script to reach §25 there is no such
                    // number: an instrument that can be drowned is an instrument. So the
                    // INFO line is reserved for the traffic a reader of this log cares
                    // about, and everything else is said at DEBUG — same sentence, same
                    // cap, one level down, so nothing that arrived goes unrecordable.
                    //
                    // Fire on a **query**, never on the first datagram: the announcer's
                    // own bursts loop back into this very socket (multicast loop is at
                    // the kernel default, which is why `drop_ours` exists), so "first
                    // datagram" would be true one poll after start on a machine that
                    // hears nobody. Cap at [`HEARD_SENTENCES`] for the flood reason.
                    heard += 1;
                    log.info(&format!(
                        "mDNS engine heard query {} of {} on {} ({} bytes, asking {}) from {}",
                        heard, HEARD_SENTENCES, MDNS_PORT, data.len(), one.asking, peer
                    ));
                } else if one.queries > 0 && other_heard < HEARD_SENTENCES {
                    other_heard += 1;
                    // The other half of the pair above: this process received a query
                    // that no answer of ours can be for. DEBUG because the machine's own
                    // name-service chatter is continuous and this line is only ever for
                    // a human with the threshold raised, asking "what is out there".
                    log.debug(&format!(
                        "mDNS engine heard a query that is not ours {} of {} on {} ({} bytes, asking {}) from {}",
                        other_heard, HEARD_SENTENCES, MDNS_PORT, data.len(), one.asking, peer
                    ));
                }
                if !first_answer_said && one.answered_any() {
                    // Once, at INFO: the first query this process ever produced an
                    // answer for is the moment "listening" stops being an assumption.
                    // `answered_any` includes an answer the queue took, because the
                    // reader of §25 is asking "did the query land", and a promise held
                    // for 100 ms is not the same fact as a byte — which is why the
                    // group total below still counts only writes.
                    first_answer_said = true;
                    log.info(&format!(
                        "mDNS answered its first query ({} to the group, {} direct, {} queued), of {} from {}",
                        one.group, one.direct, one.queued, one.asking, peer
                    ));
                }
                if one.absorbed == 1 && served.absorbed == 0 {
                    // Said once, not per packet: a packet this parser cannot read is
                    // news the first time and noise every time after. The reason the
                    // socket sees traffic at all is measured (`mdns_reuseport_probe.py`):
                    // multicast is duplicated to every joined socket, and a **unicast**
                    // reply to port 5353 is handed to exactly one socket in the reuse
                    // group, which may be this one even when the question was the OS
                    // responder's or our own browser's.
                    log.debug(
                        "mDNS engine holds 5353 and received a packet it could not parse: a \
                         unicast reply aimed at another socket's query can arrive here, and \
                         unreadable bytes are counted rather than smoothed over (announce.rs's \
                         first divergence, and §16's shared-reader note, carry why)",
                    );
                }
                if one.ingested > 0 && served.ingested == 0 {
                    // Once, at INFO, with the count in it. This is the row that says
                    // whether the protected bucket can ever fire **in this process**:
                    // `respond::bucket`'s one-second branch reads the ledger, and the
                    // ledger is fed only by what this socket *hears* — in practice the
                    // kernel's looped-back copy of our own announcement. A receiver
                    // that never hears itself is not broken; it answers every query as
                    // fast as before and simply has no flood protection. That is a fact
                    // a reader can only learn from a line like this one, which is why
                    // §31 of the acceptance script reads it. Said once because every
                    // other receiver on a LAN produces this traffic continuously.
                    log.info(&format!(
                        "mDNS engine heard a response on {} and learned {} record(s): the \
                         last-second ledger is live, so an answer this socket already sent \
                         waits a second before it goes out again",
                        MDNS_PORT, one.ingested
                    ));
                }
                served.queries += one.queries;
                served.group += one.group;
                served.direct += one.direct;
                served.queued += one.queued;
                served.ingested += one.ingested;
                served.absorbed += one.absorbed;
            }
            Ok(None) => {}
            Err(e) => {
                // One failed read is not the thread's last. An announcer that quits
                // on a transient error leaves a receiver that is running, listening
                // on 8009, and unfindable — which is exactly the class of complaint
                // this module exists to prevent.
                log.warning(&format!("mDNS receive failed: {}", e));
            }
        }
        // Whatever has come due goes out now, one datagram per queue's merged batch.
        // This is the flush the whole queue exists for, and it is also where a
        // queued answer becomes a **write**: `served.group` counts what left, so it
        // can never read as more than the wire carried.
        for batch in queues.due(now_ms) {
            let Some(datagram) = datagram_for(&batch, &answering) else {
                continue;
            };
            match send(&datagram) {
                Ok(()) => served.group += 1,
                Err(e) => log.warning(&format!(
                    "mDNS aggregated answer could not be sent ({} bytes to {}): {}",
                    datagram.len(),
                    MDNS_GROUP,
                    e
                )),
            }
        }
        // Keys older than the window answer no question this loop asks.
        queues.heard.prune(now_ms);
        if Instant::now() >= next {
            // Every service re-announces on the same tick: one timer for the socket,
            // which is what "one engine, N registrations" means. The reference's
            // `Zeroconf` re-sends each registered service on its own schedule, and the
            // 60-second cadence is ours to choose (measured in §22: the initial burst is
            // what a cold browser needs, the keepalive is for listeners that never ask).
            for bytes in &bytes_per_service {
                report(
                    log,
                    &burst(send, bytes, REGISTER_GAP, Some(running)),
                    "re-announcement",
                );
            }
            next = Instant::now() + REANNOUNCE;
        }
    }
    if served.queries > 0 {
        log.debug(&format!(
            "mDNS engine served {} queries ({} to the group, {} direct, {} queued, {} response records learned, {} unreadable)",
            served.queries,
            served.group,
            served.direct,
            served.queued,
            served.ingested,
            served.absorbed
        ));
    }
    for service in services {
        let goodbye = service.goodbye();
        report(log, &burst(send, &goodbye, GOODBYE_GAP, None), "goodbye");
    }
    log.info("mDNS announcement stopped");
}


/// One sentence per failure, and silence when the burst was clean.
///
/// A clean re-announcement is not worth a log line — `REANNOUNCE` is a minute,
/// and §4.10's measurement of this product's own log is that 49% of one file was
/// a healthy periodic event saying nothing had changed. A burst that stopped
/// early *is* worth one, because "the receiver is running and nothing finds it"
/// is otherwise indistinguishable from a network problem.
fn report(log: &Logger, burst: &Burst, which: &str) {
    for failure in &burst.failures {
        log.warning(&format!("mDNS {} send failed: {}", which, failure));
    }
    if burst.attempted < BROADCASTS {
        log.warning(&format!(
            "mDNS {} stopped after {} of {} packets — a sender that missed this will not see us \
             until the next one",
            which, burst.attempted, BROADCASTS
        ));
    }
}

#[cfg(test)]
#[path = "announce_tests.rs"]
mod announce_tests;
