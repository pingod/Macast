//! Witnesses for [`crate::announce`] — the packet a receiver sends about itself.
//!
//! The reference for every expectation here is the **frozen output of the
//! reference implementation**, not this file's reading of the RFC:
//! `tests/golden/mdns_announce.hex` and `tests/golden/mdns_names.jsonl`, both
//! produced by `scripts/mdns_announce_probe.py` against the `zeroconf` in this
//! repository's `.venv` (0.151.3) with `async_send_with_transport` replaced by a
//! recorder. That is the same layering `docs/Rust-Port-Design.md` §13.2 writes for
//! the byte-level corpora: a hand-built fixture proves self-consistency, and only
//! a capture proves parity with the thing senders are built against.
//!
//! Two habits carried in from the other test files in this crate, both earned the
//! hard way:
//!
//! * every test opens with `crate::pin_test_dir()`, because a `cfg(test)` guard in
//!   `macast-core` does not protect a dependent crate's test binary;
//! * a silence assertion is phrased **by level** ([`loud`]), never by emptiness —
//!   the log threshold is one process-wide `AtomicU8` and a neighbour's DEBUG
//!   window decides whether a file is empty if the test lets it.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use macast_core::logging::{read_tail, Logger};
use macast_core::setting::Setting;

use super::*;
use crate::mdns::{self, RData, TYPE_A, TYPE_PTR, TYPE_SRV, TYPE_TXT};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ANNOUNCE_HEX: &str = include_str!("../tests/golden/mdns_announce.hex");
const NAMES_JSONL: &str = include_str!("../tests/golden/mdns_names.jsonl");

/// One shape from the corpus, as bytes.
fn corpus(name: &str) -> Vec<u8> {
    for line in ANNOUNCE_HEX.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.splitn(2, ' ');
        let key = fields.next().unwrap_or("");
        let hex = fields.next().unwrap_or("");
        if key == name {
            return (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit"))
                .collect();
        }
    }
    panic!("the corpus has no `{}` line — regenerate it with --dump-hex", name);
}

/// The fixture's inputs, read back out of the corpus header rather than
/// re-typed here. A test that hard-codes the inputs it is comparing against can
/// pass while the corpus it claims to check says something else entirely.
fn fixture_inputs() -> (String, String, u16, Ipv4Addr, Vec<(String, Vec<u8>)>) {
    let mut instance = String::new();
    let mut server = String::new();
    let mut port = 0u16;
    let mut address = String::new();
    let mut txt: Vec<(String, Vec<u8>)> = Vec::new();
    for line in ANNOUNCE_HEX.lines().filter(|l| l.starts_with("# inputs:") || l.starts_with("# txt")) {
        if line.starts_with("# inputs:") {
            for token in line.split_whitespace().skip(2) {
                if let Some(value) = token.strip_prefix("instance=") {
                    instance = value.to_string();
                } else if let Some(value) = token.strip_prefix("server=") {
                    server = value.to_string();
                } else if let Some(value) = token.strip_prefix("port=") {
                    port = value.parse().expect("a port number");
                } else if let Some(value) = token.strip_prefix("address=") {
                    address = value.to_string();
                }
            }
        } else {
            // `# txt (order as written on the wire): k=v;k=v;…` — split on the
            // semicolons the header uses, then on the **first** equals sign,
            // because a value may carry one (`fn=Macast(x=y)`).
            let table = line.split(": ").nth(1).unwrap_or("");
            for cell in table.split(';') {
                let mut halves = cell.splitn(2, '=');
                let key = halves.next().unwrap_or("").to_string();
                let value = halves.next().unwrap_or("").to_string();
                if !key.is_empty() {
                    txt.push((key, value.into_bytes()));
                }
            }
        }
    }
    assert!(!instance.is_empty() && !server.is_empty(), "corpus header unreadable");
    assert!(port > 0, "corpus header carries no port");
    let addr: Ipv4Addr = address.parse().expect("the corpus address is IPv4");
    (instance, server, port, addr, txt)
}

fn fixture_service() -> Service {
    let (instance, server, port, address, txt) = fixture_inputs();
    Service {
        instance,
        server,
        port,
        txt,
        addresses: vec![address],
        clipped: Vec::new(),
    }
}

fn setting(dir_name: &str) -> Arc<Setting> {
    Arc::new(Setting::new(
        macast_core::paths::setting_dir().join(format!("announce-{dir_name}.json")),
    ))
}

fn lines(path: &std::path::Path) -> String {
    read_tail(path, 200).text
}

/// Records at INFO or above — see `targets_tests::loud` for why silence is
/// asserted by level in this crate.
fn loud(path: &std::path::Path) -> String {
    lines(path)
        .lines()
        .filter(|record| !record.contains("[DEBUG]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `Service` the product would have built, minus the parts of `Setting` that
/// read from disk. Used by the tests that are about the *packet*, so that a
/// friendly-name change cannot make them pass for the wrong reason.
fn service(friendly: &str, port: u16, addresses: Vec<Ipv4Addr>) -> Service {
    let instance = sanitize_instance_name(friendly, SERVICE_TYPE, FALLBACK_NAME);
    let server = normalize_server(instance.split('.').next().unwrap_or(FALLBACK_NAME));
    let txt = vec![
        ("id".to_string(), b"00000000-0000-4000-8000-000000000007".to_vec()),
        ("fn".to_string(), friendly.as_bytes().to_vec()),
        ("ca".to_string(), b"5".to_vec()),
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

// ---------------------------------------------------------------------------
// A. Byte parity with the reference
// ---------------------------------------------------------------------------

#[test]
fn the_announcement_is_the_reference_s_bytes_in_the_order_it_emits_them() {
    crate::pin_test_dir();
    let service = fixture_service();
    assert_eq!(
        service.announcement(),
        corpus("announce-nsec-first"),
        "our announcement is not the datagram zeroconf sent for the same inputs"
    );
    assert_eq!(
        service.announcement_a_first(),
        corpus("announce-a-first"),
        "the other order the reference emits (PYTHONHASHSEED 12345) is not what we write either"
    );
}

#[test]
fn the_goodbye_is_the_same_records_with_every_ttl_zeroed() {
    crate::pin_test_dir();
    let service = fixture_service();
    assert_eq!(service.goodbye(), corpus("goodbye-nsec-first"));
    assert_eq!(service.goodbye_a_first(), corpus("goodbye-a-first"));
    // …and it is **the same records**: same length, same names, types and
    // classes, every TTL zero. Length alone would pass for a packet that dropped
    // a record and padded itself back out, and "the bytes that differ are the TTL
    // fields" is only checkable through a reader once name compression puts the
    // records at offsets this file cannot count by hand.
    let announce = service.announcement();
    let goodbye = service.goodbye();
    assert_eq!(announce.len(), goodbye.len());
    assert_eq!(&announce[2..4], &goodbye[2..4], "flags differ — the reference sends the same header for both");
    let logger = Logger::named("LocalFileCaster");
    let read = |bytes: &[u8]| {
        mdns::parse_message(bytes, "192.0.2.7:5353", &logger).expect("parses")
    };
    let a = read(&announce).records;
    let g = read(&goodbye).records;
    assert_eq!(a.len(), g.len(), "the goodbye lost a record");
    for (one, two) in a.iter().zip(g.iter()) {
        assert_eq!(one.name, two.name);
        assert_eq!(one.rtype, two.rtype);
        assert_eq!(one.class, two.class);
        assert_eq!(one.flush, two.flush);
        assert_eq!(one.rdata, two.rdata, "the goodbye changed a payload, not just a TTL");
        assert_ne!(one.ttl, 0, "the announcement itself must carry a live TTL");
        assert_eq!(two.ttl, 0, "{} ttl={} in the goodbye, expected 0", one.name, two.ttl);
    }
}

#[test]
fn every_packet_we_write_is_a_response_and_never_a_query() {
    crate::pin_test_dir();
    let service = fixture_service();
    let probe = corpus("probe");
    for (label, bytes) in [
        ("announcement", service.announcement()),
        ("announcement-a-first", service.announcement_a_first()),
        ("goodbye", service.goodbye()),
        ("goodbye-a-first", service.goodbye_a_first()),
    ] {
        assert_eq!(
            &bytes[2..4],
            &FLAGS_ANNOUNCE.to_be_bytes(),
            "{label} does not carry the response+AA flags"
        );
        assert_ne!(bytes, probe, "{label} is the uniqueness probe, which we do not send");
    }
    // The probe is a **query** (`flags = 0x0400`) and carries one question, so a
    // reader can tell the two families apart by the QR bit alone. This asserts
    // that about the corpus, because the sentence above is what the divergence in
    // the module header rests on: we never emit it.
    assert_eq!(&probe[2..4], &0x0400u16.to_be_bytes());
    assert_eq!(&probe[4..6], &1u16.to_be_bytes(), "one question");
}

#[test]
fn the_two_corpus_orders_differ_only_in_the_address_pair() {
    crate::pin_test_dir();
    let a = corpus("announce-nsec-first");
    let b = corpus("announce-a-first");
    assert_eq!(a.len(), b.len());
    let first_difference = (0..a.len()).find(|i| a[*i] != b[*i]).unwrap_or(a.len());
    // 266 measured on this build, for this fixture. The point is not the number:
    // PTR, SRV and TXT are byte-identical, so the *only* disagreement between the
    // reference's two orders is where the tail pair sits. A corpus that differed
    // earlier would mean the set order is not the only nondeterminism, and then
    // byte parity would not be a reachable target at all.
    assert!(
        first_difference > 200,
        "the two reference orders disagree at offset {}, which is inside the fixed \
         prefix (PTR/SRV/TXT) — parity is then not a property this test can claim",
        first_difference
    );
    assert_eq!(first_difference, 266, "the measured boundary moved; re-read the corpus");
}

#[test]
fn the_packet_parses_with_our_own_browser() {
    crate::pin_test_dir();
    let service = fixture_service();
    let (logger, _path) = (Logger::named("LocalFileCaster"), ());
    let message = mdns::parse_message(&service.announcement(), "192.0.2.7:5353", &logger)
        .expect("our own announcement must parse with our own reader");
    let (_, _, port, address, txt) = fixture_inputs();

    // Five records go out; the reader keeps four, because NSEC is one of the
    // three types `mdns.rs` skips **by declared length**. That skip is the claim
    // this test makes: our writer's rdata lengths are right, because a reader
    // that resyncs after the NSEC still finds the A record after it. If the
    // lengths were wrong, the A would not decode and this would lose two records
    // rather than one.
    let names: Vec<(u16, String)> = message
        .records
        .iter()
        .map(|r| (r.rtype, r.name.clone()))
        .collect();
    assert_eq!(
        names
            .iter()
            .map(|(t, _)| *t)
            .collect::<Vec<_>>()
            .as_slice(),
        &[TYPE_PTR, TYPE_SRV, TYPE_TXT, TYPE_A],
        "the reader did not land on exactly the four types it decodes: {:?}",
        names
    );

    let ptr = message
        .records
        .iter()
        .find(|r| r.rtype == TYPE_PTR)
        .expect("a PTR");
    assert_eq!(ptr.name, SERVICE_TYPE);
    assert_eq!(
        ptr.rdata,
        RData::Name(service.instance.clone()),
        "the PTR alias is not the instance name we own"
    );
    assert_eq!(ptr.ttl, OTHER_TTL);
    assert!(!ptr.flush, "a PTR is never unique — one type, many instances");

    let srv = message
        .records
        .iter()
        .find(|r| r.rtype == TYPE_SRV)
        .expect("an SRV");
    match srv.rdata {
        RData::Srv {
            priority,
            weight,
            port: got,
            ref target,
        } => {
            assert_eq!((priority, weight), (0, 0));
            assert_eq!(got, port);
            assert_eq!(target, &service.server);
        }
        _ => panic!("the SRV did not decode"),
    }
    assert!(srv.flush);
    assert_eq!(srv.ttl, HOST_TTL);

    let txt_record = message
        .records
        .iter()
        .find(|r| r.rtype == TYPE_TXT)
        .expect("a TXT");
    let raw = match &txt_record.rdata {
        RData::Txt(bytes) => bytes.clone(),
        _ => panic!("the TXT did not decode"),
    };
    let pairs = mdns::txt_pairs(&raw);
    let keys: Vec<String> = pairs
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).to_string())
        .collect();
    let expected: Vec<String> = txt.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys, expected,
        "TXT order on the wire is not the order the reference writes it"
    );
    for (key, value) in &txt {
        let got = mdns::txt_value(&pairs, key).unwrap_or_default();
        assert_eq!(
            got,
            String::from_utf8_lossy(value).to_string(),
            "TXT property {} read back wrong",
            key
        );
    }

    let a = message.records.iter().find(|r| r.rtype == TYPE_A).expect("an A");
    assert_eq!(a.rdata, RData::A(address));
    assert_eq!(a.name, service.server, "the A must answer for the SRV target");
    assert_eq!(a.ttl, HOST_TTL);
}

#[test]
fn the_ledger_folds_our_announcement_into_a_device() {
    crate::pin_test_dir();
    // The end of the road for an announcement: not "it parses" but "a browser
    // that hears only this learns a name, a host and a port" — which is exactly
    // what `discover_cast()` returns and what the device dropdown shows. VLC's
    // microdns never sends a query, so this fold from unsolicited bytes is the
    // whole story for that sender.
    let service = fixture_service();
    let (instance, _, port, address, txt) = fixture_inputs();
    let fn_value = txt
        .iter()
        .find(|(k, _)| k == "fn")
        .map(|(_, v)| String::from_utf8_lossy(v).to_string())
        .expect("the fixture carries an fn");
    let logger = Logger::named("LocalFileCaster");
    let message =
        mdns::parse_message(&service.announcement(), &format!("{}:5353", address), &logger)
            .expect("parses");
    let mut ledger = mdns::Ledger::new(SERVICE_TYPE);
    ledger.ingest(&message.records);
    let devices = ledger.devices();
    assert_eq!(devices.len(), 1, "one announcement, one device");
    // The name is the TXT `fn`, not the instance label — `Ledger::device_name`
    // prefers `fn` and falls through to the SRV target, which is Python's
    // `properties.get('fn') or info.server or name`. Asserting the instance here
    // would pin the wrong rule.
    assert_eq!(devices[0].name, fn_value, "the dropdown shows the fn, not the sanitized label");
    assert_ne!(devices[0].name, instance, "…and that is a different string from the instance name");
    assert_eq!(devices[0].port, port);
    assert_eq!(devices[0].address, address.to_string());
}

// ---------------------------------------------------------------------------
// B. The name rules, frozen from the product
// ---------------------------------------------------------------------------

#[test]
fn the_name_rules_answer_the_frozen_table() {
    crate::pin_test_dir();
    let mut cases = 0usize;
    for line in NAMES_JSONL.lines().filter(|l| !l.starts_with('#')) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).expect("a JSON line");
        let input = value["input"].as_str().expect("input");
        let want_instance = value["instance"].as_str().expect("instance");
        let want_server = value["server"].as_str().expect("server");
        let got = sanitize_instance_name(input, SERVICE_TYPE, FALLBACK_NAME);
        assert_eq!(
            got, want_instance,
            "sanitize_instance_name({:?}) gave {}, the product gives {}",
            input, got, want_instance
        );
        let stem = want_instance.split('.').next().unwrap_or("");
        assert_eq!(
            normalize_server(stem),
            want_server,
            "normalize_server({:?}) gave {}, the product gives {}",
            stem,
            normalize_server(stem),
            want_server
        );
        cases += 1;
    }
    assert!(
        cases >= 20,
        "the name corpus only carried {} cases — a dropped table is not a pass",
        cases
    );
}

#[test]
fn a_sanitized_label_never_exceeds_one_dns_label() {
    crate::pin_test_dir();
    // The corpus pins the outcomes; this pins the invariant the outcomes are
    // instances of, so a future case cannot put a 64-byte label on the wire —
    // which the reader would show as a name nothing else in the packet owns.
    for input in [
        "a".repeat(200),
        "Macast(".to_string() + &"b".repeat(200) + ")",
        "中文".repeat(80),
        "-".repeat(70),
        String::new(),
    ] {
        let got = sanitize_instance_name(&input, SERVICE_TYPE, FALLBACK_NAME);
        let label = got.split('.').next().unwrap_or("");
        assert!(
            label.len() <= MAX_LABEL && !label.is_empty(),
            "{:?} produced the label {:?}, which is not one DNS label",
            input,
            label
        );
        assert!(
            label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{:?} produced a label with illegal characters: {:?}",
            input,
            label
        );
        assert!(!label.starts_with('-') && !label.ends_with('-'));
    }
}

// ---------------------------------------------------------------------------
// C. The TXT clip: a long name costs the property, not the receiver
// ---------------------------------------------------------------------------

#[test]
fn an_over_long_property_is_shortened_and_reported() {
    crate::pin_test_dir();
    let long = "影".repeat(200); // 600 UTF-8 bytes, and every char is 3
    let (txt, clipped) = clip_txt(vec![
        ("fn".to_string(), long.clone().into_bytes()),
        ("ca".to_string(), b"5".to_vec()),
    ]);
    assert_eq!(
        clipped,
        vec![("fn".to_string(), format!("fn={}", long).len())],
        "the clip did not report the field and its original length"
    );
    let body = format!("fn={}", String::from_utf8_lossy(&txt[0].1));
    assert!(
        body.len() <= MAX_TXT_PROPERTY,
        "the shortened property is still {} bytes",
        body.len()
    );
    // The claim is "cut on a character boundary", and the way to ask it is
    // "does the result decode" — not "is the last byte a lead byte", which a
    // correct cut in fact fails (「影」 is E5 BD B1, and the B1 that ends a
    // complete code point is a continuation byte). First version of this line
    // asserted the wrong predicate and went red on correct code.
    assert!(
        std::str::from_utf8(&txt[0].1).is_ok(),
        "the shortened value is not valid UTF-8 — it was cut inside a code point"
    );
    assert_eq!(txt[1].1, b"5", "a short property must not be touched");
}

#[test]
fn a_long_friendly_name_still_announces_a_device() {
    crate::pin_test_dir();
    // The reference's equivalent raises out of `start()` and the receiver never
    // comes up. Ours must still produce a packet a browser folds into a device,
    // with an instance name that is still legal.
    let long = "Macast(".to_string() + &"x".repeat(300) + ")";
    let s = service(&long, 8009, vec![Ipv4Addr::new(192, 0, 2, 9)]);
    assert!(!s.clipped.is_empty(), "a 300-byte fn was not clipped");
    let bytes = s.announcement();
    let logger = Logger::named("LocalFileCaster");
    let message = mdns::parse_message(&bytes, "192.0.2.9:5353", &logger).expect("parses");
    let mut ledger = mdns::Ledger::new(SERVICE_TYPE);
    ledger.ingest(&message.records);
    let devices = ledger.devices();
    assert_eq!(devices.len(), 1, "the clip must not lose the device");
    assert_eq!(devices[0].port, 8009);
    // The label the packet owns is 63 bytes — one DNS label — while the `fn` the
    // dropdown shows is the clipped one. Two different limits, and the device
    // surviving is the point of the divergence.
    assert_eq!(s.instance.split('.').next().unwrap().len(), MAX_LABEL);
    assert_eq!(
        s.txt
            .iter()
            .find(|(k, _)| k == "fn")
            .map(|(_, v)| v.len())
            .expect("fn present")
            + 3,
        MAX_TXT_PROPERTY,
        "the clip fills the property rather than leaving it short: fn= plus the value"
    );
}

// ---------------------------------------------------------------------------
// D. The cadence
// ---------------------------------------------------------------------------

#[test]
fn a_burst_shouts_the_broadcasts_and_stops_on_the_flag() {
    crate::pin_test_dir();
    let running = AtomicBool::new(true);
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let outcome = burst(
        &mut |bytes| {
            seen.push(bytes.to_vec());
            Ok(())
        },
        b"packet",
        Duration::from_millis(0),
        Some(&running),
    );
    assert_eq!(outcome.attempted, BROADCASTS);
    assert!(outcome.clean());
    assert_eq!(seen.len(), BROADCASTS, "the sink saw a different count");
    assert!(seen.iter().all(|one| one == b"packet"), "a burst re-sends the same bytes — the reference's three bursts are byte-identical");

    // Now the same call with the flag already down: nothing goes out, and that is
    // the announcement path's contract (`stop()` must not keep shouting).
    running.store(false, Ordering::SeqCst);
    let mut seen2 = 0usize;
    let outcome = burst(
        &mut |_| {
            seen2 += 1;
            Ok(())
        },
        b"packet",
        Duration::from_millis(0),
        Some(&running),
    );
    assert_eq!(outcome.attempted, 0);
    assert_eq!(seen2, 0);
}

#[test]
fn the_goodbye_is_not_gated_by_the_stop_flag() {
    crate::pin_test_dir();
    // The asymmetry the module's `abort` argument exists for. `Announcer::stop`
    // clears `running` and *then* the thread reaches the goodbye: a goodbye that
    // consulted the same flag would send zero packets, and a sender that misses
    // it keeps our PTR for [`OTHER_TTL`] — 75 minutes of a dead device in a
    // phone's list. Written as a test because the bug is invisible in a run where
    // nobody looks at the wire after shutdown.
    let running = AtomicBool::new(false);
    // The precondition, asserted rather than assumed: this is exactly the state
    // `Announcer::stop()` leaves behind — flag down, thread still running.
    assert!(!running.load(Ordering::SeqCst));
    let mut seen = 0usize;
    let outcome = burst(
        &mut |_| {
            seen += 1;
            Ok(())
        },
        b"goodbye",
        Duration::from_millis(0),
        None,
    );
    assert_eq!(outcome.attempted, BROADCASTS, "the goodbye was gated by a flag that `stop()` already cleared");
    assert_eq!(seen, BROADCASTS);
}

#[test]
fn the_run_function_goodbyes_even_when_the_flag_is_already_down() {
    crate::pin_test_dir();
    // The same claim as `the_goodbye_is_not_gated_by_the_stop_flag`, one level up
    // — and this is the one that would have caught the bug, because the bug is
    // not in `burst`'s parameter handling, it is in `run` handing it the flag.
    // A test that only exercises `burst` with `None` proves nothing about which
    // call sites pass `None`.
    let logger = Logger::named(LOGGER_NAME);
    let path = logger.claim();
    let service = fixture_service();
    let goodbye = service.goodbye();
    let running = AtomicBool::new(false); // exactly the state `stop()` leaves
    let mut seen: Vec<Vec<u8>> = Vec::new();
    {
        let mut send = |bytes: &[u8]| {
            seen.push(bytes.to_vec());
            Ok(())
        };
        // The two seams this test never reaches: the flag is already down, so the
        // loop's first check ends it before a read or an answer is asked for. They
        // are here because the signature says so, not to simulate anything.
        let mut no_unicast = |_bytes: &[u8], _peer: std::net::SocketAddr| Ok(());
        let mut no_recv = || Ok(None);
        run(
            &service,
            &mut send,
            &mut no_unicast,
            &mut no_recv,
            &running,
            &logger,
        );
    }
    let announced = seen.iter().filter(|bytes| *bytes != &goodbye).count();
    let goodbyed = seen.iter().filter(|bytes| *bytes == &goodbye).count();
    assert_eq!(
        announced, 0,
        "a stopped announcer still shouted {} announcement packets",
        announced
    );
    assert_eq!(
        goodbyed, BROADCASTS,
        "the goodbye was gated by the flag `stop()` already cleared — a sender that \
         missed it keeps this device in its list for {:?} (saw {} of {})",
        Duration::from_secs(OTHER_TTL as u64),
        goodbyed,
        BROADCASTS
    );
    assert!(
        loud(&path).contains("mDNS announcement stopped"),
        "the thread that sent the goodbye said nothing about it: {}",
        lines(&path)
    );
}

#[test]
fn a_failed_send_is_reported_and_not_retried_inside_the_burst() {
    crate::pin_test_dir();
    let running = AtomicBool::new(true);
    let outcome = burst(
        &mut |_| Err(io_error()),
        b"packet",
        Duration::from_millis(0),
        Some(&running),
    );
    assert_eq!(outcome.attempted, BROADCASTS);
    assert_eq!(outcome.failures.len(), BROADCASTS);
    assert!(outcome.failures[0].contains("packet 1 of 3"));
    assert!(!outcome.clean());
}

/// An `io::Error` that is not an OS failure, so the message the test asserts on
/// is ours. `io::Error::other` is the stable constructor for that.
fn io_error() -> std::io::Error {
    std::io::Error::other("ENOBUFS on this test's fake socket")
}

#[test]
fn the_keepalive_lands_inside_the_host_ttl() {
    crate::pin_test_dir();
    // Not a tautology about two constants: this is the sentence in
    // `discovery.py:237` —「comfortably inside the 120s TTL」— and the failure
    // mode is the one that used to be worked around by restarting the plugin.
    // Raising `REANNOUNCE` past `HOST_TTL` keeps every test above green and makes
    // the device vanish between announcements.
    assert!(
        REANNOUNCE < Duration::from_secs(HOST_TTL as u64),
        "re-announcing every {:?} cannot refresh a {:?} TTL",
        REANNOUNCE,
        Duration::from_secs(HOST_TTL as u64)
    );
    assert_eq!(BROADCASTS, 3);
    assert_eq!(REGISTER_GAP, Duration::from_millis(225));
    assert_eq!(GOODBYE_GAP, Duration::from_millis(125));
}

// ---------------------------------------------------------------------------
// E. The product's own table
// ---------------------------------------------------------------------------

#[test]
fn the_table_names_the_device_the_rest_of_the_app_names() {
    crate::pin_test_dir();
    let dir = setting("identity");
    dir.set(
        macast_core::setting::Key::DlnaFriendlyName,
        serde_json::json!("Macast(TestHost-1)"),
    );
    dir.set(
        macast_core::setting::Key::Usn,
        serde_json::json!("767dc6b4-d20f-41ed-9e44-e293c03c2e1b"),
    );
    let s = Service::for_cast(
        &dir,
        8009,
        vec![Ipv4Addr::new(192, 0, 2, 11)],
    );
    // The instance is the sanitized name, the `fn` is the **raw** one, and the
    // `id` is the USN SSDP already publishes. Three readers, one answer — the
    // shape AGENTS.md §4.14's 「advertise_host 有两份副本」 exists to forbid.
    assert_eq!(s.instance, "Macast-TestHost-1._googlecast._tcp.local.");
    assert_eq!(s.server, "Macast-TestHost-1.local.");
    let fn_value = s
        .txt
        .iter()
        .find(|(k, _)| k == "fn")
        .map(|(_, v)| String::from_utf8_lossy(v).to_string())
        .expect("fn present");
    assert_eq!(fn_value, "Macast(TestHost-1)", "the TXT must carry the friendly name the user set, not the sanitized label");
    let id_value = s
        .txt
        .iter()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| String::from_utf8_lossy(v).to_string())
        .expect("id present");
    assert_eq!(id_value, dir.usn());
    assert_eq!(
        s.txt
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec!["id", "fn", "ca", "st", "bs", "ic", "ve", "md", "rm", "rs"],
        "the TXT key order is the order _advertise writes them in"
    );
    // ca=5 is the bitmask VLC reads; without the video bit it never sends a
    // video stream, which arrives as a user report about our receiver.
    assert_eq!(
        s.txt.iter().find(|(k, _)| k == "ca").map(|(_, v)| v.clone()),
        Some(b"5".to_vec())
    );
    assert!(s.clipped.is_empty());
}

/// The two interface lists are **not** the same question, and this is the sentence
/// that keeps them from being used interchangeably: `advertised_hosts` is what a
/// browser joins on (a superset — every interface that could carry a membership),
/// while `advertised_addresses` is what a sender must be able to **reach**.
/// On this machine the first has four entries: the LAN address plus a VMware, a
/// VirtualBox and a Docker adapter. Publishing those three as our A records is
/// AGENTS.md §4.1's first symptom row — the sender picks one, has no route, and
/// nothing on our side is wrong. `verify_rust_receiver.sh` §22 measured exactly
/// that on the wire on 2026-10-18, which is why this test exists.
#[test]
fn the_published_addresses_come_from_the_reachable_owner() {
    crate::pin_test_dir();
    let dir = setting("addresses");
    let logger = Logger::named(LOGGER_NAME);
    let s = Service::from_setting(&dir, 8009, &logger);
    let reachable = macast_core::net::advertised_addresses(&dir);
    let joinable = macast_core::net::advertised_hosts(&dir);
    let published: Vec<String> = s.addresses.iter().map(|a| a.to_string()).collect();
    for one in &published {
        assert!(
            reachable.iter().any(|held| held == one),
            "announced {}, which is not in the set a sender can reach ({:?})",
            one,
            reachable
        );
    }
    // The count comparison only says something where the two lists differ, so the
    // reading is printed rather than assumed.
    if reachable.len() != joinable.len() {
        assert_eq!(
            published.len(),
            reachable.len(),
            "this machine has {} reachable of {} joinable interfaces and the announcement \
             carried {} — the wide list is the §4.1 bug",
            reachable.len(),
            joinable.len(),
            published.len()
        );
    }
    // …and the audit, because "the lists differ here" is a fact about the machine,
    // not about the code: an edit that swaps back to the join list would go green
    // on a laptop with one NIC and red here only by luck.
    let src = include_str!("announce.rs");
    let at = src.find("pub fn from_setting").expect("from_setting exists");
    let body = &src[at..at + 1200];
    assert!(
        body.contains("net::advertised_addresses("),
        "from_setting no longer reads the reachable list"
    );
    assert!(
        !body.contains("net::advertised_hosts("),
        "from_setting reads the join list again — that is the four-address bug §22 \
         caught on the wire on 2026-10-18"
    );
}

#[test]
fn no_advertisable_address_says_which_half_failed() {
    crate::pin_test_dir();
    let logger = Logger::named(LOGGER_NAME);
    let path = logger.claim();
    let announcer = Announcer::new(service("Macast", 8009, Vec::new()), Arc::clone(&logger));
    let outcome = announcer.start();
    assert!(outcome.is_err(), "an empty address list must not pretend to announce");
    assert!(!announcer.is_running());
    let text = loud(&path);
    assert!(
        text.contains("will not be discoverable") && text.contains("no advertisable address"),
        "the refusal has to name the half that failed, got: {}",
        text
    );
    // And it must not be a second socket's failure: nothing was bound, so no
    // 「cannot bind」 line either.
    assert!(
        !text.contains("cannot bind"),
        "the empty-address path reached the socket: {}",
        text
    );
}

#[test]
fn the_announcer_never_writes_to_stdout() {
    crate::pin_test_dir();
    // Part 41's fact: a packaged build on Windows has no console, so a `println!`
    // here is not noise but a silently lost line — and §4.2's rule is that a
    // worker thread must not write where nobody reads. The announcer's witness is
    // `logs/Discovery.log`, and this is the sentence that keeps it that way.
    let src = include_str!("announce.rs");
    for needle in ["println!", "print!(", "eprintln!"] {
        assert!(
            !src.contains(needle),
            "announce.rs writes to stdout with {} — that thread has no window",
            needle
        );
    }
}
// ---------------------------------------------------------------------------
// The engine: answering a query that arrives on the socket
// ---------------------------------------------------------------------------
//
// Everything above this line is about the packet a receiver *sends*. These are about
// the one it answers, and they run through `serve` and `run`'s seams — except the
// last, which binds the real socket on loopback because a seam cannot prove a
// joined socket hears anything.

use std::cell::RefCell;
use std::net::{IpAddr, Ipv4Addr as V4, SocketAddr, UdpSocket};
use std::rc::Rc;

/// Where the engine's bytes went, as a pair of sinks a test can read afterwards.
#[derive(Clone, Debug, Default)]
struct Sent {
    group: Vec<Vec<u8>>,
    direct: Vec<(Vec<u8>, SocketAddr)>,
}

fn peer(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(V4::LOCALHOST), port)
}

/// A PTR query for our own type, built by this crate's own query writer.
fn ptr_query(service_type: &str) -> Vec<u8> {
    mdns::query(service_type, TYPE_PTR).expect("the query encodes")
}

fn wire_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// A PTR query that also carries **one record of ours it claims to hold**, placed in
/// the answer section (`in_answers`) or the additional section (not).
///
/// The two placements are the point: this is the only difference the `section` field
/// on `mdns::Record` exists to make, and before it the two packets were the same to
/// us. Written by hand onto the end of a real `mdns::query` packet — same names, same
/// TTL, only the header count and the section differ, so no test can accuse the
/// fixture of being what it is comparing.
fn query_with_known(ttl: u32, in_answers: bool) -> Vec<u8> {
    let (instance, _server, _port, _address, _txt) = fixture_inputs();
    let mut packet = ptr_query(SERVICE_TYPE);
    let mut record = Vec::new();
    record.extend_from_slice(&wire_name(SERVICE_TYPE));
    record.extend_from_slice(&TYPE_PTR.to_be_bytes());
    record.extend_from_slice(&1u16.to_be_bytes()); // CLASS_IN, no cache-flush bit
    record.extend_from_slice(&ttl.to_be_bytes());
    let alias = wire_name(&instance);
    record.extend_from_slice(&(alias.len() as u16).to_be_bytes());
    record.extend_from_slice(&alias);
    packet.extend_from_slice(&record);
    let (an, ar): (u16, u16) = if in_answers { (1, 0) } else { (0, 1) };
    packet[6..8].copy_from_slice(&an.to_be_bytes());
    packet[10..12].copy_from_slice(&ar.to_be_bytes());
    packet
}

/// Drive `serve` with recording sinks.
fn served_by(service: &Service, data: &[u8], from: SocketAddr) -> (Served, Sent) {
    let logger = Logger::named(LOGGER_NAME);
    let sent = Rc::<RefCell<Sent>>::default();
    let group_sink = {
        let sent = Rc::clone(&sent);
        move |bytes: &[u8]| {
            sent.borrow_mut().group.push(bytes.to_vec());
            Ok(())
        }
    };
    let direct_sink = {
        let sent = Rc::clone(&sent);
        move |bytes: &[u8], peer: SocketAddr| {
            sent.borrow_mut().direct.push((bytes.to_vec(), peer));
            Ok(())
        }
    };
    let mut group_sink = group_sink;
    let mut direct_sink = direct_sink;
    let served = serve(data, from, service, &mut group_sink, &mut direct_sink, &logger);
    // Borrowed rather than `Rc::try_unwrap`: this function's own handle is one of
    // the strong counts, so a unwrap that expected to be the last owner failed for
    // the wrong reason and made four answering tests look like four wrong answers.
    let captured = sent.borrow().clone();
    (served, captured)
}

#[test]
fn e1_a_query_from_port_5353_is_answered_on_the_group_and_nowhere_else() {
    crate::pin_test_dir();
    let service = fixture_service();
    let (served, sent) = served_by(&service, &ptr_query(SERVICE_TYPE), peer(5353));
    assert_eq!(served.queries, 1);
    assert_eq!(served.group, 1, "one datagram to the group: {:?}", served);
    assert_eq!(
        served.direct, 0,
        "a multicast querier must not get a unicast leg"
    );
    let logger = Logger::named(LOGGER_NAME);
    let message = mdns::parse_message(&sent.group[0], "peer", &logger).expect("parses");
    assert!(
        message.records.iter().any(|record| {
            record.rtype == TYPE_PTR && record.rdata == RData::Name(service.instance.clone())
        }),
        "the answer did not name this instance: {:?}",
        message.records
    );
}

#[test]
fn e2_a_query_from_an_ephemeral_port_is_answered_twice_the_reference_way() {
    crate::pin_test_dir();
    // `ucast_source = port != 5353`, and the reference answers such a querier **both**
    // ways. Witnessed in the corpus by `ptr-ephemeral-src`, whose capture holds a
    // datagram to `127.0.0.1:55759` and one to the group.
    let service = fixture_service();
    let asker = peer(55759);
    let (served, sent) = served_by(&service, &ptr_query(SERVICE_TYPE), asker);
    assert_eq!(served.group, 1);
    assert_eq!(served.direct, 1);
    assert_eq!(sent.direct[0].1, asker, "the direct leg went elsewhere");
}

#[test]
fn e3_a_packet_that_is_not_a_query_is_counted_rather_than_answered() {
    crate::pin_test_dir();
    // The honest half of the merge. A socket bound to 5353 can be handed a **unicast**
    // datagram meant for another bound socket (measured on both kernels by
    // `mdns_reuseport_probe.py`). Nothing here can use it, and the loss has to show up
    // in a counter rather than be inferred from an answer that is not there.
    let service = fixture_service();
    let announcement = service.announcement();
    let (served, sent) = served_by(&service, &announcement, peer(5353));
    assert_eq!(served.queries, 0);
    assert_eq!(served.absorbed, 1, "a non-query must be counted");
    assert_eq!(served.group + served.direct, 0);
    assert!(sent.group.is_empty() && sent.direct.is_empty());
}

#[test]
fn e4_a_query_for_a_service_we_do_not_run_costs_no_bytes_and_no_error_line() {
    crate::pin_test_dir();
    // `other-service` in the corpus: silence, and silence that is not an error. A
    // responder that complained about every unanswered question would let a chatty
    // scanner fill the log — §4.10's measured 49%-of-one-file is the warning.
    let logger = Logger::named(LOGGER_NAME);
    let path = logger.claim();
    let service = fixture_service();
    let (served, _sent) = served_by(&service, &ptr_query("_airplay._tcp.local."), peer(5353));
    assert_eq!(served.queries, 1, "the question was understood");
    assert_eq!(served.group + served.direct, 0, "and declined");
    assert_eq!(served.absorbed, 0, "a query is not an absorbed packet");
    assert!(
        !lines(&path).contains("ERROR"),
        "declining a foreign query logged an error: {}",
        lines(&path)
    );
}

#[test]
fn e5_a_known_answer_in_the_answer_section_silences_the_reply_as_the_reference_does() {
    crate::pin_test_dir();
    let service = fixture_service();
    let (served, _sent) = served_by(
        &service,
        &query_with_known(OTHER_TTL, true),
        peer(5353),
    );
    assert_eq!(served.queries, 1);
    assert_eq!(
        served.group + served.direct,
        0,
        "the asker said it holds this PTR with a full TTL; answering is the flooding \
         the suppression rule exists to stop"
    );
}

#[test]
fn e6_a_known_answer_in_the_additional_section_does_not_silence_anything() {
    crate::pin_test_dir();
    // The reason `mdns::Record` carries a section at all: a querier's *additional*
    // section is records it is telling us about, not records it claims to hold.
    // Before the field these two packets were indistinguishable, so a responder could
    // have stopped answering while every corpus test stayed green.
    let service = fixture_service();
    let (served, _sent) = served_by(
        &service,
        &query_with_known(OTHER_TTL, false),
        peer(5353),
    );
    assert_eq!(
        served.group,
        1,
        "an additional-section record was read as a known answer: {:?}",
        served
    );
}

#[test]
fn e7_the_loop_answers_between_bursts_and_survives_a_failed_read() {
    crate::pin_test_dir();
    // `run` now owns two clocks: the 60-second re-announcement and the read it polls.
    // A blocking read lets a quiet network starve the announcement (a receiver that
    // stops advertising is the complaint this module exists to prevent), and a read
    // error that ends the thread leaves 8009 listening to nobody. Both are asked
    // through the seams instead of by waiting on a timer.
    let logger = Logger::named(LOGGER_NAME);
    let service = fixture_service();
    let announcement = service.announcement();
    let goodbye = service.goodbye();
    let query = ptr_query(SERVICE_TYPE);
    let running = AtomicBool::new(true);
    let sends = Rc::<RefCell<Vec<Vec<u8>>>>::default();
    let mut calls = 0usize;
    {
        let mut send = {
            let sends = Rc::clone(&sends);
            move |bytes: &[u8]| {
                sends.borrow_mut().push(bytes.to_vec());
                Ok(())
            }
        };
        let mut unicast = {
            let sends = Rc::clone(&sends);
            move |bytes: &[u8], _peer: SocketAddr| {
                sends.borrow_mut().push(bytes.to_vec());
                Ok(())
            }
        };
        let mut recv = || {
            calls += 1;
            match calls {
                // A query first: the loop must answer it without waiting for the
                // next minute's burst.
                1 => Ok(Some((query.clone(), peer(5353)))),
                // Then a failed read: logged, not fatal.
                2 => Err(std::io::Error::other("transient read failure")),
                // Then nothing, and the announcer is asked to stop.
                _ => {
                    running.store(false, Ordering::SeqCst);
                    Ok(None)
                }
            }
        };
        run(
            &service,
            &mut send,
            &mut unicast,
            &mut recv,
            &running,
            &logger,
        );
    }
    let held = sends.borrow();
    let answered = held
        .iter()
        .filter(|bytes| *bytes != &announcement && *bytes != &goodbye)
        .count();
    assert_eq!(
        answered, 1,
        "the query the loop received was not answered (sends: {})",
        held.len()
    );
    assert_eq!(
        held.iter().filter(|bytes| *bytes == &goodbye).count(),
        BROADCASTS,
        "a read error or a stop must not cut the goodbye"
    );
    assert!(
        calls >= 3,
        "the loop stopped polling after the error (calls: {})",
        calls
    );
}

#[test]
fn e8_a_query_on_loopback_gets_an_answer_back_from_the_real_socket() {
    crate::pin_test_dir();
    // Every case above goes through an injected sink, which proves the decision and
    // nothing about the socket. This one binds what the product binds, joined on lo
    // (the join list is a parameter for exactly this reason), hears a real datagram,
    // and answers it to a real querier.
    let logger = Logger::named(LOGGER_NAME);
    let service = fixture_service();
    let engine = match bind_engine(&[V4::LOCALHOST], &logger) {
        Ok(sock) => sock,
        // Not a skip. Binding 5353 with the reuse options *is* the product's path,
        // and "we could not try it" must never read as "it works" (§4.2's
        // 「不提问的检查」 with the exit code as the mask).
        Err(e) => panic!("cannot bind the engine socket the product binds: {}", e),
    };
    let group = SocketAddr::new(IpAddr::V4(V4::new(224, 0, 0, 251)), MDNS_PORT);
    let asker = UdpSocket::bind("127.0.0.1:0").expect("an ephemeral asker binds");
    // Pin its egress to loopback with TTL 1 so no byte of this test reaches the LAN.
    let sock2 = socket2::SockRef::from(&asker);
    sock2
        .set_multicast_if_v4(&V4::LOCALHOST)
        .expect("lo0 can be the multicast interface");
    sock2.set_multicast_ttl_v4(1).expect("TTL 1 stays on this host");
    let query = ptr_query(SERVICE_TYPE);
    asker.send_to(&query, group).expect("the query leaves on lo");

    let mut buf = vec![0u8; crate::mdns::READ_MAX];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut answered = 0usize;
    let mut ask_again = std::time::Instant::now();
    while std::time::Instant::now() < deadline && answered == 0 {
        match engine.recv_from(&mut buf) {
            Ok((length, from)) => {
                let data = buf[..length].to_vec();
                let mut send = |bytes: &[u8]| engine.send_to(bytes, group).map(|_| ());
                let mut unicast = |bytes: &[u8], peer: SocketAddr| {
                    engine.send_to(bytes, peer).map(|_| ())
                };
                let served = serve(
                    &data,
                    from,
                    &service,
                    &mut send,
                    &mut unicast,
                    &logger,
                );
                answered = served.group + served.direct;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                // Re-ask every second: the querier's own stack repeats a query it gets
                // no answer to, and so does a test that wants a witness rather than a
                // timeout.
                if std::time::Instant::now() >= ask_again {
                    asker.send_to(&query, group).expect("the retry leaves");
                    ask_again = std::time::Instant::now() + Duration::from_secs(1);
                }
            }
            Err(e) => panic!("the engine socket's read failed: {}", e),
        }
    }
    assert!(
        answered > 0,
        "the engine never answered a query on its own joined socket"
    );
    // And the bytes have to reach the asker: an answer written by a socket that
    // cannot route back to the querier is the same class of bug the dispatch
    // measurement was about.
    let mut got = vec![0u8; crate::mdns::READ_MAX];
    asker
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout on the asker");
    let (length, from) = asker.recv_from(&mut got).expect("the querier got an answer");
    got.truncate(length);
    let message = mdns::parse_message(&got, "engine", &logger).expect("the answer parses");
    assert!(
        message.records.iter().any(|record| record.rtype == TYPE_PTR
            && record.rdata == RData::Name(service.instance.clone())),
        "the querier got a packet that does not name us: {:?}",
        message.records
    );
    assert_eq!(
        from.port(),
        MDNS_PORT,
        "an mDNS answer must come from 5353 — a querier that filters on the source \
         port would drop a reply written from an ephemeral one"
    );
}
