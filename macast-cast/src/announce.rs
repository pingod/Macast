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
//! * **One socket, not one per interface, and it does not join the group.** The
//!   reference binds a sender per IPv4 address *and* joins each to the group, so
//!   every burst goes out six times and every one of those sockets is read by
//!   zeroconf's engine. Our browser ([`crate::mdns`]) already holds a joined
//!   socket while it browses, and on macOS/Linux `SO_REUSEPORT` **distributes**
//!   each incoming datagram to one of the bound sockets — so a second joined
//!   socket here would silently take away part of what the device dropdown hears.
//!   A sender that never reads must therefore never join: we send to the group
//!   from an unjoined socket, which reaches the LAN exactly as well and steals
//!   nothing. Consequence, stated where it can be read: answering other people's
//!   queries (the responder) needs a joined socket, and when that lands these two
//!   sockets have to become one reader. Registered as the remaining half of §15's
//!   row.
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

/// The group and port every burst is written to.
pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const MDNS_PORT: u16 = 5353;

/// The DNS numbers. Reading lives in [`crate::mdns`]; these are the writing side
/// of the same table, and a mismatch between the two is a packet our own browser
/// would not recognise — `announce_tests` asks that directly.
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const TYPE_NSEC: u16 = 47;
const CLASS_IN: u16 = 1;
/// The cache-flush bit as it sits **on the wire**, inside the class field.
const CLASS_UNIQUE: u16 = 0x8000;

/// response + authoritative answer — `DNSOutgoing(_FLAGS_QR_RESPONSE |
/// _FLAGS_AA)` (`_core.py:523`).
const FLAGS_ANNOUNCE: u16 = 0x8400;

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
        let service = Service::for_cast(setting, port, addresses);
        for (key, bytes) in &service.clipped {
            // The reference's equivalent here is `start()` raising; ours names
            // the field, what was lost, and that the receiver still came up.
            log.warning(&format!(
                "mDNS TXT property {} is {} bytes, past the {}; shortened for the wire",
                key, bytes, MAX_TXT_PROPERTY
            ));
        }
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
        let service_type = SERVICE_TYPE;
        let txt = self.txt.as_slice();
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
        // 3. TXT: the properties, in order, each one length-prefixed.
        w.record(instance, TYPE_TXT, other, true, |w| {
            for (key, value) in txt {
                let body = property_bytes(key, value);
                w.u8(body.len() as u8);
                w.octets(&body);
            }
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
fn property_bytes(key: &str, value: &[u8]) -> Vec<u8> {
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
struct Writer {
    buf: Vec<u8>,
    /// (name without its trailing dot, byte offset of its first label)
    names: Vec<(String, usize)>,
    answers: u16,
}

impl Writer {
    fn new(flags: u16) -> Writer {
        let mut buf = Vec::with_capacity(320);
        buf.extend_from_slice(&0u16.to_be_bytes()); // transaction id
        buf.extend_from_slice(&flags.to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]); // qd / an / ns / ar, written last
        Writer {
            buf,
            names: Vec::new(),
            answers: 0,
        }
    }

    fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn octets(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// `write_name`: the whole name if it has never been written; otherwise a
    /// two-byte pointer; and if only a *suffix* is known, write the labels up to
    /// where the known part begins and point there.
    fn name(&mut self, name: &str) {
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
    fn record<F>(&mut self, name: &str, rtype: u16, ttl: u32, unique: bool, rdata: F)
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
        self.answers += 1;
    }

    fn finish(self) -> Vec<u8> {
        let mut buf = self.buf;
        buf[4..6].copy_from_slice(&0u16.to_be_bytes()); // questions
        buf[6..8].copy_from_slice(&self.answers.to_be_bytes());
        buf[8..10].copy_from_slice(&0u16.to_be_bytes()); // authority
        buf[10..12].copy_from_slice(&0u16.to_be_bytes()); // additional
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
    service: Service,
    log: Arc<Logger>,
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Announcer {
    pub fn new(service: Service, log: Arc<Logger>) -> Arc<Announcer> {
        Arc::new(Announcer {
            service,
            log,
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        })
    }

    pub fn service(&self) -> &Service {
        &self.service
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Bind and announce. The `Err` path has already logged its reason, because a
    /// machine whose mDNS is blocked must still run the rest of the receiver —
    /// the reference swallows the equivalent into `advertise() -> False` with
    /// 「mDNS unavailable, %s will not be discoverable: %s」.
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        if self.service.addresses.is_empty() {
            // Not a comment about the network: this is the case where the
            // receiver is up and reachable on 8009 and **no sender can learn it**,
            // so the sentence says which half failed.
            self.log.warning(
                "mDNS unavailable, Chromecast will not be discoverable: \
                 no advertisable address to publish",
            );
            return Err("no advertisable address to publish".to_string());
        }
        let sock = match bind_sender(&self.log) {
            Ok(sock) => sock,
            Err(e) => {
                self.log.error(&format!(
                    "mDNS unavailable, Chromecast will not be discoverable: {}",
                    e
                ));
                return Err(e.to_string());
            }
        };
        self.running.store(true, Ordering::SeqCst);
        let running = Arc::clone(&self.running);
        let log = Arc::clone(&self.log);
        let service = self.service.clone();
        let started = thread::Builder::new()
            .name("MDNS_KEEPALIVE".to_string())
            .spawn(move || {
                let group = SocketAddr::from((MDNS_GROUP, MDNS_PORT));
                let mut send = |bytes: &[u8]| sock.send_to(bytes, group).map(|_| ());
                run(&service, &mut send, &running, &log)
            });
        match started {
            Ok(handle) => {
                *self.handle.lock().unwrap() = Some(handle);
                // `discovery.py:294`: 「mDNS advertised %s on port %d」 — and
                // `protocol_cast.py:415`'s 「ChromecastProtocol started on port
                // %s」 is the channel's own line, not this thread's.
                self.log.info(&format!(
                    "mDNS advertised {} on port {}",
                    self.service.instance, self.service.port
                ));
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

/// One socket, bound to 5353 with the reuse options, **not joined** to the
/// group. See the module header's first divergence for why joining it here would
/// take datagrams away from [`crate::mdns`]'s browser.
fn bind_sender(log: &Logger) -> io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    crate::mdns::set_reuse_for_mdns(&sock, log);
    sock.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into())?;
    // The responder's two options, from `set_respond_socket_multicast_options`.
    sock.set_multicast_ttl_v4(MULTICAST_TTL as u32)?;
    sock.set_multicast_loop_v4(true)?;
    Ok(sock.into())
}

/// Three bursts now, then three more every [`REANNOUNCE`], until asked to stop —
/// and then the goodbye, which the stop flag does **not** gate. See [`burst`]'s
/// `abort` argument for why that asymmetry is load-bearing, and the test named
/// `the_run_function_goodbyes_even_when_the_flag_is_already_down` for the
/// statement about *this* function rather than about `burst`.
///
/// `send` is a closure so this whole order of operations can be driven by a
/// `Vec` in a test instead of by 5353. The thread above wraps the socket into
/// exactly this shape, so nothing here is a test-only path.
fn run(
    service: &Service,
    send: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    running: &AtomicBool,
    log: &Logger,
) {
    let bytes = service.announcement();
    report(
        log,
        &burst(send, &bytes, REGISTER_GAP, Some(running)),
        "announcement",
    );
    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        interrupted_sleep(REANNOUNCE, Some(running));
        if !running.load(Ordering::SeqCst) {
            break;
        }
        report(
            log,
            &burst(send, &bytes, REGISTER_GAP, Some(running)),
            "re-announcement",
        );
    }
    let goodbye = service.goodbye();
    report(log, &burst(send, &goodbye, GOODBYE_GAP, None), "goodbye");
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
