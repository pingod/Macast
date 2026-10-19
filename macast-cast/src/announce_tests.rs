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
        service_type: SERVICE_TYPE.to_string(),
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
        service_type: SERVICE_TYPE.to_string(),
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
            std::slice::from_ref(&service),
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
    let a = Service::airplay_from_setting(&dir, 7000, "11:22:33:44:55:66", &logger);
    let reachable = macast_core::net::advertised_addresses(&dir);
    let joinable = macast_core::net::advertised_hosts(&dir);
    // Both builders, because they are the two announcements this port makes and the
    // narrow list is a property of the owner they share — checking one of them and
    // refactoring the other into the same helper is exactly the shape of edit that
    // leaves a stale assertion behind.
    for (which, published) in [
        (
            "Chromecast",
            s.addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<String>>(),
        ),
        (
            "AirPlay",
            a.addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<String>>(),
        ),
    ] {
        for one in &published {
            assert!(
                reachable.iter().any(|held| held == one),
                "the {which} announcement carries {}, which is not in the set a \
                 sender can reach ({:?})",
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
                "this machine has {} reachable of {} joinable interfaces and the {which} \
                 announcement carried {} — the wide list is the §4.1 bug",
                reachable.len(),
                joinable.len(),
                published.len()
            );
        }
    }
    // …and the audit, because "the lists differ here" is a fact about the machine,
    // not about the code: an edit that swaps back to the join list would go green
    // on a laptop with one NIC and red here only by luck.
    //
    // The audit walks **to the owner**, not to the caller. Both announcement
    // builders — the Chromecast one and the AirPlay one — now read the list through
    // one private helper, so the rule is two facts joined: every builder routes
    // through that helper, and the helper is the place the narrow call lives. A
    // test that only looked inside `from_setting` would go red on a clean refactor
    // (which is how it read when the helper was introduced) and a test that only
    // looked at the helper would miss a builder that quietly went back to the wide
    // list beside it.
    let src = include_str!("announce.rs");
    for (builder, needle) in [
        ("pub fn from_setting", "the Chromecast announcement"),
        ("pub fn airplay_from_setting", "the AirPlay announcement"),
    ] {
        let at = src.find(builder).unwrap_or_else(|| panic!("{builder} exists"));
        let body = &src[at..at + 1200];
        assert!(
            body.contains("advertisable(setting, log)"),
            "{builder} does not read the addresses through the one owner, so it is \
             free to pick a list — and {} is the builder a reader would least expect \
             to have drifted",
            needle
        );
        assert!(
            !body.contains("net::advertised_hosts("),
            "{builder} reads the join list again — that is the four-address bug §22 \
             caught on the wire on 2026-10-18"
        );
    }
    let at = src.find("fn advertisable(").expect("the owner exists");
    let helper = &src[at..at + 900];
    assert!(
        helper.contains("net::advertised_addresses("),
        "the address owner no longer reads the reachable list"
    );
    assert!(
        !helper.contains("net::advertised_hosts("),
        "the address owner reads the join list: every announcement is now the §4.1 bug"
    );
}

#[test]
fn no_advertisable_address_says_which_half_failed() {
    crate::pin_test_dir();
    let logger = Logger::named(LOGGER_NAME);
    let path = logger.claim();
    let announcer = Announcer::new(
        vec![service("Macast", 8009, Vec::new())],
        Arc::clone(&logger),
    );
    let outcome = announcer.start();
    assert!(outcome.is_err(), "an empty address list must not pretend to announce");
    assert!(!announcer.is_running());
    let text = loud(&path);
    assert!(
        text.contains("will not be discoverable") && text.contains("no advertisable address"),
        "the refusal has to name the half that failed, got: {}",
        text
    );
    // …and name it **by the service type**, which is what the reference's `%s` is
    // (`discovery.py:268-269` formats `service_type`): a reader who arrives at this
    // line has to know which of the two announcements died, and today there are two.
    assert!(
        text.contains(SERVICE_TYPE),
        "the refusal does not name the service type it refused ({SERVICE_TYPE})"
    );
    // And it must not be a second socket's failure: nothing was bound, so no
    // 「cannot bind」 line either.
    assert!(
        !text.contains("cannot bind"),
        "the empty-address path reached the socket: {}",
        text
    );
}

/// The same `start()` now serves **two** service types, and the refusal sentence used
/// to name one of them by its marketing name — so an AirPlay advertisement that failed
/// reported "Chromecast will not be discoverable" and sent the reader to the Cast
/// settings. The reference never had that shape: `discovery.py:268-269` formats the
/// **service type the caller passed**, which is the one name that stays true for both.
/// Separate logger name because this row asserts an absence and `claim()` is
/// first-wins — two tests reading one file would make the absence about scheduling.
#[test]
fn an_airplay_refusal_names_airplay_and_never_blames_the_chromecast() {
    crate::pin_test_dir();
    let logger = Logger::named("Discovery-AirPlayRefusal");
    let path = logger.claim();
    let airplay = Service::for_airplay(
        &setting("airplay-refusal"),
        7001,
        Vec::new(),
        "11:22:33:44:55:66",
    );
    assert_eq!(airplay.service_type, crate::airplay::AIRPLAY_SERVICE);
    let announcer = Announcer::new(vec![airplay], Arc::clone(&logger));
    let outcome = announcer.start();
    assert!(
        outcome.is_err(),
        "an AirPlay service with no addresses must not pretend to announce"
    );
    assert!(!announcer.is_running());
    let text = loud(&path);
    assert!(
        text.contains(crate::airplay::AIRPLAY_SERVICE)
            && text.contains("will not be discoverable"),
        "the AirPlay refusal has to name the AirPlay type, got: {text}"
    );
    assert!(
        !text.contains("Chromecast") && !text.contains(SERVICE_TYPE),
        "the AirPlay path still blames the Chromecast: {text}"
    );
}

/// `start()` must **derive** the answering half from [`Service::responder_answers`]
/// rather than write a literal at the call site. The predicate is the one line that
/// says which service type the 17-row responder corpus covers, so a `true` hardcoded
/// here would give `_airplay._tcp` a responder nobody measured and a `false` would
/// switch the Chromecast one off.
///
/// Why this is an audit and not a behaviour test: the behavioural witness for the
/// `false` direction is a real sender asking a real socket, which is §25 — and §25's
/// LAN row is a **skip on this machine** (the query never reaches this binary, §16 第
/// 24 条). A row that is skipped cannot be described as coverage, and 「不提问的检查等
/// 于通过的检查」 is the same failure one layer over: the honest object here is the
/// wiring, so the wiring is what gets asked. When §25 lands a second machine's
/// reading, that becomes the stronger witness and this stays as the structural one.
#[test]
fn the_answering_half_is_derived_not_written() {
    crate::pin_test_dir();
    let src = include_str!("announce.rs");
    // **The derivation moved with the merge**, and this row moved with it — a check
    // that keeps looking for a line that no longer exists is a green that agrees with
    // anything. The predicate is now asked **per service inside the loop**
    // (`answering`), because one socket carries every type; `start()` no longer
    // computes a single flag. So the audit asks the two places that would have to be
    // wrong together for a type to start answering without being measured:
    let at = src
        .find("let answering: Vec<&Service> = services.iter()")
        .expect("the answering set is derived somewhere");
    let body = &src[at..at + 300];
    assert!(
        body.contains(".filter(|s| s.responder_answers())"),
        "the answering set is no longer filtered by the predicate"
    );
    // …and no literal `responder_answers()` replacement: `serve` must be reached only
    // through that set. Asked as "the loop's call site names the set", not as "no
    // `true` appears in the file".
    let loop_at = src.find("for service in &answering {").expect("the loop exists");
    let loop_body = &src[loop_at..loop_at + 600];
    assert!(
        loop_body.contains("serve(&data, peer, service, send, unicast, log)"),
        "the loop no longer serves through the answering set"
    );
    assert!(
        !src.contains("bind_engine(&self.service.addresses, &self.log, true)"),
        "a hardcoded answering flag came back at bind time"
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
            std::slice::from_ref(&service),
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
        Ok(engine) => engine,
        // Not a skip. Binding 5353 with the reuse options *is* the product's path,
        // and "we could not try it" must never read as "it works" (§4.2's
        // 「不提问的检查」 with the exit code as the mask).
        Err(e) => panic!("cannot bind the engine socket the product binds: {}", e),
    };
    // The two numbers behind the app's 「joined N of M interface(s)」 sentence, asked
    // of the struct rather than of its own log text: §25's reading starts from that
    // pair, and a sentence nothing reads is a decoration.
    assert_eq!(
        engine.joined, 1,
        "the engine says it joined, but the kernel did not take a join on lo"
    );
    assert_eq!(
        engine.offered, 1,
        "one interface was handed in and the engine should report exactly that offer"
    );
    let sock = engine.sock;
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
        match sock.recv_from(&mut buf) {
            Ok((length, from)) => {
                let data = buf[..length].to_vec();
                let mut send = |bytes: &[u8]| sock.send_to(bytes, group).map(|_| ());
                let mut unicast = |bytes: &[u8], peer: SocketAddr| {
                    sock.send_to(bytes, peer).map(|_| ())
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

/// Drives [`run`] over a scripted queue of received datagrams, then stops it.
///
/// The queue is the only clock: when it runs dry the loop's flag goes down, so a
/// test never waits on `REANNOUNCE` to reach its assertions.
fn drive(service: &Service, logger: &Logger, packets: Vec<(Vec<u8>, SocketAddr)>) {
    drive_services(&[service.clone()], logger, packets);
}

/// The same drive over **several** services, which is the shape the product now uses:
/// one socket announces every type, and whether a type answers is asked of the service
/// itself (`responder_answers()`), not passed in as a mode. There is no `answers`
/// parameter any more on purpose — the first version of this helper had one, and a
/// caller could then ask the loop to answer for a service whose predicate says no,
/// which is a state the product can no longer be in.
fn drive_services(
    services: &[Service],
    logger: &Logger,
    packets: Vec<(Vec<u8>, SocketAddr)>,
) {
    let running = AtomicBool::new(true);
    let mut queue: std::collections::VecDeque<(Vec<u8>, SocketAddr)> = packets.into();
    let mut send = |_bytes: &[u8]| Ok(());
    let mut unicast = |_bytes: &[u8], _peer: SocketAddr| Ok(());
    let mut recv = || match queue.pop_front() {
        Some((bytes, peer)) => Ok(Some((bytes, peer))),
        None => {
            running.store(false, Ordering::SeqCst);
            Ok(None)
        }
    };
    run(
        services,
        &mut send,
        &mut unicast,
        &mut recv,
        &running,
        logger,
    );
}

/// **One socket, two service types, one of which answers.**
///
/// This is the merged engine's whole point, and the shape the reference has: one
/// `Zeroconf` registers both services, so a querier's datagram reaches **one** socket
/// and the answer comes from the service whose type the question names. Before the
/// merge the two types lived on two sockets, and the measured unicast rule (one socket
/// gets the whole stream, which one varies) made "will the answering socket see this
/// query" a coin flip.
///
/// Both halves are asserted on the same run: a `_googlecast._tcp` PTR query is answered
/// (the 17-row corpus covers that type), and a `_airplay._tcp` PTR query is **read and
/// not answered** — read, because the log line proves the datagram arrived; not
/// answered, because the predicate says the decision table was never measured for it.
/// **Every** service announces, not just the first. This row exists because a mutant
/// that kept only the first service's burst went **0 red** (measured 2026-10-19): the
/// merged witness above looks at answers and at the AirPlay *question*, and neither
/// notices that the AirPlay *announcement* was never sent — which is the one thing an
/// iPhone list is built from. Asked by decoding, not by byte-identity, so the row says
/// **which** type went missing.
#[test]
fn every_service_in_the_engine_gets_its_own_burst() {
    crate::pin_test_dir();
    let logger = Logger::named("Discovery-BurstAll");
    let cast = fixture_service();
    let (_instance, _server, port, address, txt) = airplay_inputs();
    let airplay = airplay_service(&txt, port, address);
    // `true` until the first read: a flag that is **already down** truncates the opening
    // burst (its sleeps check the flag), which is what the goodbye test relies on and
    // what made this row see an empty list in its first version. Here the burst has to
    // run to completion, so the flag goes down from `recv` — the same clock `drive` uses.
    let running = AtomicBool::new(true);
    let sent: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&sent);
    let mut send = move |bytes: &[u8]| {
        sink.lock().unwrap().push(bytes.to_vec());
        Ok(())
    };
    let mut unicast = |_b: &[u8], _p: SocketAddr| Ok(());
    let mut recv = || {
        running.store(false, Ordering::SeqCst);
        Ok(None)
    };
    run(&[cast, airplay], &mut send, &mut unicast, &mut recv, &running, &logger);
    let wrote = sent.lock().unwrap();
    // **Announcements only**: `ttl > 0` excludes the goodbye, which the stop path sends
    // for every service and which satisfied this assertion in its first version — the
    // mutant that announced only the first service still passed, because the AirPlay
    // name appeared in a packet with TTL 0. A check that cannot tell "announced" from
    // "said goodbye" is the shape this branch keeps finding (§16 第 30 条的 same family).
    let types: Vec<String> = wrote
        .iter()
        .filter_map(|bytes| crate::mdns::parse_message(bytes, "test", &logger))
        .flat_map(|doc| {
            doc.records
                .into_iter()
                .filter(|r| r.ttl > 0)
                .map(|r| r.name)
        })
        .filter(|name| name.contains("._tcp.local."))
        .collect();
    assert!(
        types.iter().any(|n| n.contains("_googlecast")),
        "no Chromecast burst went out: {types:?}"
    );
    assert!(
        types.iter().any(|n| n.contains("_airplay")),
        "the AirPlay service is in the engine and never announced: {types:?}"
    );
}

/// The join list is the **union** of what every service publishes, and this row exists
/// because a mutant that took only the first service's addresses went **0 red**
/// (measured 2026-10-19): on this machine both services publish the same one address, so
/// the behavior cannot tell the two implementations apart. That is the same shape as
/// §22's "the two lists differ here, so the count comparison says something" — the
/// assertion has to be given two **different** sets to be able to answer.
#[test]
fn the_join_list_is_the_union_of_every_service_and_not_the_first_one() {
    crate::pin_test_dir();
    let logger = Logger::named("Discovery-JoinUnion");
    let first = service("Macast", 8009, vec![V4::new(192, 0, 2, 1)]);
    let second = service("Macast", 7001, vec![V4::new(192, 0, 2, 2), V4::new(192, 0, 2, 1)]);
    let announcer = Announcer::new(vec![first, second], Arc::clone(&logger));
    let joined = announcer.join_addresses();
    assert_eq!(
        joined,
        vec![V4::new(192, 0, 2, 1), V4::new(192, 0, 2, 2)],
        "the join list is not the union: {joined:?}"
    );
    // …and the union is deduplicated, because `join_multicast_v4` twice on the same
    // interface is at best a wasted syscall and at worst an error that reads like a
    // broken interface.
    assert_eq!(joined.len(), 2);
}

#[test]
fn one_socket_answers_the_measured_type_and_stays_silent_for_the_other() {
    crate::pin_test_dir();
    let logger = Logger::named("Discovery-Merged");
    let path = logger.claim();
    let (_instance, _server, port, address, txt) = airplay_inputs();
    let cast = fixture_service();
    let airplay = airplay_service(&txt, port, address);
    assert!(cast.responder_answers(), "the fixture's Cast service must be the answering one");
    assert!(!airplay.responder_answers(), "the AirPlay service must not be the answering one");

    let running = AtomicBool::new(true);
    let sent: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&sent);
    let mut send = move |bytes: &[u8]| {
        sink.lock().unwrap().push(bytes.to_vec());
        Ok(())
    };
    let mut unicast = |_bytes: &[u8], _peer: SocketAddr| Ok(());
    let mut queue = std::collections::VecDeque::from(vec![
        (ptr_query(SERVICE_TYPE), peer(MDNS_PORT)),
        (ptr_query(crate::airplay::AIRPLAY_SERVICE), peer(MDNS_PORT)),
    ]);
    let mut recv = || match queue.pop_front() {
        Some((bytes, peer)) => Ok(Some((bytes, peer))),
        None => {
            running.store(false, Ordering::SeqCst);
            Ok(None)
        }
    };
    {
        let _debug = crate::DebugWindow::armed();
        run(&[cast.clone(), airplay.clone()], &mut send, &mut unicast, &mut recv, &running, &logger);
    }
    let wrote = sent.lock().unwrap();
    // Every packet this run put on the wire: two announcement bursts (3 each), then the
    // answers, then two goodbyes (3 each). The answers are the packets that are not one
    // of those six fixed datagrams.
    let cast_announce = cast.announcement();
    let airplay_announce = airplay.announcement();
    let cast_goodbye = cast.goodbye();
    let airplay_goodbye = airplay.goodbye();
    let answers: Vec<&Vec<u8>> = wrote
        .iter()
        .filter(|b| {
            **b != cast_announce
                && **b != airplay_announce
                && **b != cast_goodbye
                && **b != airplay_goodbye
        })
        .collect();
    assert!(!answers.is_empty(), "the measured type's query went unanswered: {wrote:?}");
    // The answer must be a **Cast** answer: decoding it is the only way to say whose it
    // is, and a byte-identity check would pass for the AirPlay reply of a run that
    // answered the wrong question.
    let parsed = crate::mdns::parse_message(answers[0], "test", &logger).expect("an answer");
    assert!(
        parsed.records.iter().any(|r| r.name.contains("_googlecast")),
        "the answer does not name the Cast type: {:?}",
        parsed.records.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    assert!(
        !answers.iter().any(|b| crate::mdns::parse_message(b, "test", &logger)
            .map(|doc| doc.records.iter().any(|r| r.name.contains("_airplay")))
            .unwrap_or(false)),
        "something answered the AirPlay query, whose table was never measured"
    );
    // …and the AirPlay query was **delivered**: the receiving sentence fires for a query
    // that is ours, and "ours" is asked per service inside the merged loop.
    let text = read_tail(&path, 400).text;
    assert!(
        text.contains("asking Ptr _airplay._tcp.local."),
        "the merged socket did not even report hearing the AirPlay question: {text}"
    );
    assert!(
        text.contains("mDNS answered its first query"),
        "the answering half never fired for the measured type: {text}"
    );
}

#[test]
fn e9_the_receiving_sentence_fires_for_a_query_and_never_for_our_own_echo() {
    crate::pin_test_dir();
    // "The responder never answered" has two causes, and this process is the only
    // one that can tell them apart: nothing was ever handed to this socket, or it
    // was handed over and declined. `verify_rust_receiver.sh` section 25 reads that
    // pair, so the receiving question needs its own line — and the line has to mean
    // *network*, not *thread*. Multicast loop is left at the kernel default (the
    // module's own first divergence, and the reason `drop_ours` exists), so our
    // announcement comes back to us on the same socket within one poll of start:
    // a line fired by "first datagram" would be true on a machine that hears nobody.
    //
    // Two logger names, not one file: `run` logs into `logs/<name>.log` and the
    // tests in this crate run in one process, so an absence assertion that shares a
    // file with its own positive half is a collision waiting for the scheduler.
    let service = fixture_service();
    let echo = Logger::named("Discovery-Echo");
    let echo_path = echo.claim();
    drive(
        &service,
        &echo,
        vec![
            (service.announcement(), peer(MDNS_PORT)),
            (service.goodbye(), peer(MDNS_PORT)),
        ],
    );
    assert!(
        // The needle is the sentence as `run` writes it **today**. It was first
        // written against the older wording, and after the wording changed this
        // assertion kept passing while asking nothing at all — the mutant that
        // fires on every datagram came back **0 red** until the needle was
        // re-matched. A stale needle is not a conservative test; it is no test.
        !loud(&echo_path).contains("mDNS engine heard query"),
        "the engine called its own looped-back announcement a query: {}",
        lines(&echo_path)
    );

    let heard = Logger::named("Discovery-Heard");
    let heard_path = heard.claim();
    let query = ptr_query(SERVICE_TYPE);
    drive(
        &service,
        &heard,
        vec![
            (query.clone(), peer(MDNS_PORT)),
            (query.clone(), peer(MDNS_PORT)),
        ],
    );
    let said = |path: &std::path::Path| {
        lines(path)
            .lines()
            .filter(|record| record.contains("mDNS engine heard query"))
            .count()
    };
    assert_eq!(
        said(&heard_path),
        2,
        "two queries should each be named, and the loop named {}: {}",
        said(&heard_path),
        lines(&heard_path)
    );
    assert!(
        loud(&heard_path).contains("answered its first query"),
        "the pair is what section 25 reads; a hearing with no answering is the \
         product half of the diagnosis: {}",
        lines(&heard_path)
    );
    // **What was asked** is in the line, not just a byte count. Measured on this
    // Mac 2026-10-18: a fresh instance's log filled with three `Ptr
    // _http._tcp.local.` probes from the machine's own stack — the same shape the
    // old anonymous line printed as "36 bytes", indistinguishable from a
    // Chromecast's query — and §25's cold browser arrived to a spent budget. A
    // reader of the log has to be able to tell which query this process is talking
    // about, or the sentence cannot separate the empties it exists to separate.
    let subject = format!("{:?} {}", crate::respond::Kind::Ptr, SERVICE_TYPE);
    let asked = format!("asking {subject}");
    assert!(
        loud(&heard_path).contains(&asked),
        "the hearing line names no subject ({asked}): {}",
        lines(&heard_path)
    );
    // The same subject has to be in the **answering** sentence too: §25 pairs the
    // two lines and reads them as one story, and an answer whose question is not
    // named cannot be matched to the query the harness sent.
    let answered = loud(&heard_path)
        .lines()
        .find(|record| record.contains("answered its first query"))
        .unwrap_or_default()
        .to_string();
    assert!(
        answered.contains(&subject),
        "the answering sentence does not say what it answered ({subject} would have \
         to be inferred from the line above it, which is a different datagram): {answered}"
    );

    // And a query about **somebody else** is still admitted to — at DEBUG, and under
    // its own cap. The INFO line is reserved for traffic a reader of this log cares
    // about (a cap that the machine's own chatter can outrun is not a witness), but
    // "not ours" and "never arrived" must stay two different answers, so the
    // receiving fact is written down either way.
    let other = Logger::named("Discovery-OtherType");
    let other_path = other.claim();
    {
        let _debug = crate::DebugWindow::armed();
        drive(
            &service,
            &other,
            vec![
                (foreign_query("_http._tcp.local.", 0, false), peer(MDNS_PORT)),
                (foreign_query("_printer._tcp.local.", 7, false), peer(MDNS_PORT)),
            ],
        );
    }
    assert_eq!(
        said(&other_path),
        0,
        "the INFO budget was spent on a query this service cannot answer, which is \
         how §25's own query ends up unsaid: {}",
        lines(&other_path)
    );
    assert_eq!(
        lines(&other_path)
            .lines()
            .filter(|record| record.contains("a query that is not ours"))
            .count(),
        2,
        "two foreign queries arrived and neither was recorded at all, so the log \
         cannot tell \"not for us\" from \"nothing came\": {}",
        lines(&other_path)
    );
    assert!(
        lines(&other_path)
            .lines()
            .filter(|record| record.contains("a query that is not ours"))
            .all(|record| record.contains("[DEBUG]")),
        "the foreign line is meant to be DEBUG — at INFO it is the chatter that \
         outran the old cap: {}",
        lines(&other_path)
    );
    assert!(
        loud(&heard_path).contains(&asked),
        "the hearing line names no subject ({asked}): {}",
        lines(&heard_path)
    );

    // And the sentence is **capped**: eight, and the number is a *literal* here on
    // purpose. The first version asserted `said == crate::announce::HEARD_SENTENCES`
    // — a same-source equality that is true for every value of the constant, and a
    // mutant moving the cap 3 → 4 came back **0 red** because of it (§13.4's
    // 「对任何上限值都成立」 again, this time in a test I wrote this hour). Ten
    // queries now, eight lines expected: a chatty scanner cannot turn this into a
    // line per probe, which is §4.10's measured flood. Eight replaced three on
    // 2026-10-18 because one script run has three separate browsers asking, and a
    // cap they all fit inside before §25 arrives is not a witness.
    let capped = Logger::named("Discovery-Capped");
    let capped_path = capped.claim();
    drive(
        &service,
        &capped,
        (0..10).map(|_| (query.clone(), peer(MDNS_PORT))).collect(),
    );
    let heard = lines(&capped_path)
        .lines()
        .filter(|record| record.contains("mDNS engine heard query"))
        .map(|record| record.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        heard.len(),
        8,
        "the hearing sentence is capped at eight lines; the loop wrote {}: {}",
        heard.len(),
        lines(&capped_path)
    );
    // The number each line prints is the constant that decides when to stop. That
    // is a real question — it goes red when the sentence and the cap drift apart —
    // and it is asked *after* the count, so a wrong count cannot hide behind it.
    for line in &heard {
        assert!(
            line.contains(&format!("of {} on", crate::announce::HEARD_SENTENCES)),
            "the sentence prints a cap that is not the constant: {line}"
        );
    }
}


/// A query built by a **foreign stack**, in the exact shape one went across the LAN
/// from 192.168.1.68 on 2026-10-18 (a python sender on that machine, its bytes
/// printed before sending), and the answer that same host saw back: 296 bytes,
/// an=1 ar=4, from 192.168.1.5:5353.
///
/// The transaction id is non-zero because the sender was not written to match us.
/// Everything the responder layer had been checked against up to now was a query
/// this crate's own [`crate::mdns::query`] wrote — self-consistency, and nothing
/// else. A responder that reads only its own writer's shape (a zero id, one class
/// spelling, its own label order) is exactly what a phone's Cast browser would meet
/// as "searchable, never reachable".
fn foreign_query(name: &str, id: u16, qu: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // flags: standard query
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // ANCOUNT / NSCOUNT / ARCOUNT
    for label in name.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&12u16.to_be_bytes()); // PTR
    out.extend_from_slice(&if qu { [0x80, 0x01] } else { [0x00, 0x01] });
    out
}

#[test]
fn e10_a_query_written_by_a_foreign_stack_is_answered() {
    crate::pin_test_dir();
    let service = fixture_service();
    for (label, bytes) in [
        ("id 0, class IN", foreign_query(SERVICE_TYPE, 0, false)),
        ("id 0x1234, class IN", foreign_query(SERVICE_TYPE, 0x1234, false)),
        ("id 0, QU bit set", foreign_query(SERVICE_TYPE, 0, true)),
    ] {
        assert_eq!(bytes.len(), 40, "{label}: the frozen wire shape drifted");
        let (served, _sent) = served_by(&service, &bytes, peer(5353));
        assert_eq!(served.queries, 1, "{label}: not read as a question");
        assert_eq!(
            served.group, 1,
            "{label}: a question we run the answer to was declined"
        );
    }
    // Byte parity with our own writer on the plain shape: that is what makes the
    // three cases above a statement about senders rather than about two encoders.
    assert_eq!(
        foreign_query(SERVICE_TYPE, 0, false),
        ptr_query(SERVICE_TYPE),
        "our query writer and the foreign sender disagree — the parity claim is gone"
    );
    // And the answering is not the *shape* doing the work: a foreign-built query
    // for a type this product does not run still costs nothing but silence (e4).
    let other = foreign_query("_airplay._tcp.local.", 0x1234, false);
    let (served, sent) = served_by(&service, &other, peer(5353));
    assert_eq!(served.queries, 1, "still a question");
    assert_eq!(
        served.group + served.direct,
        0,
        "a foreign type was answered: {}",
        served.group + served.direct
    );
    assert!(sent.group.is_empty() && sent.direct.is_empty());
}

// ---------------------------------------------------------------------------
// G. The second service type: announce, and stay quiet
// ---------------------------------------------------------------------------

/// The AirPlay corpus, frozen by the same probe with `--protocol airplay`.
const AIRPLAY_HEX: &str = include_str!("../tests/golden/mdns_airplay.hex");

/// One shape from the AirPlay corpus. Deliberately a second copy of the reader
/// rather than a parameterised first one: the two files are the same format, and
/// sharing the parser would let a header change in one corpus quietly redefine
/// what the other one is being compared against.
fn airplay_corpus(name: &str) -> Vec<u8> {
    for line in AIRPLAY_HEX.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.splitn(2, ' ');
        if fields.next().unwrap_or("") == name {
            let hex = fields.next().unwrap_or("");
            return (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit"))
                .collect();
        }
    }
    panic!("the AirPlay corpus has no `{}` line", name);
}

/// The inputs behind that file, read **out of its header**. The friendly name and
/// the MAC are not this test's to invent: the first version of this row hardcoded
/// `Macast-Fixture-1` for both, and the corpus's TXT `name=` is the *unsanitized*
/// friendly name — so a hand-typed fixture would have compared our bytes against a
/// table this file made up (§4.13's 「每个数字问两遍」, here on an instrument).
fn airplay_inputs() -> (String, String, u16, Ipv4Addr, Vec<(String, String)>) {
    let mut instance = String::new();
    let mut server = String::new();
    let mut port = 0u16;
    let mut address = String::new();
    let mut txt: Vec<(String, String)> = Vec::new();
    for line in AIRPLAY_HEX.lines() {
        if line.starts_with("# inputs:") {
            for token in line.split_whitespace().skip(2) {
                if let Some(v) = token.strip_prefix("instance=") {
                    instance = v.to_string();
                } else if let Some(v) = token.strip_prefix("server=") {
                    server = v.to_string();
                } else if let Some(v) = token.strip_prefix("port=") {
                    port = v.parse().expect("a port number");
                } else if let Some(v) = token.strip_prefix("address=") {
                    address = v.to_string();
                }
            }
        } else if line.starts_with("# txt") {
            let table = line.split(": ").nth(1).unwrap_or("");
            for cell in table.split(';') {
                let mut halves = cell.splitn(2, '=');
                let key = halves.next().unwrap_or("").to_string();
                let value = halves.next().unwrap_or("").to_string();
                if !key.is_empty() {
                    txt.push((key, value));
                }
            }
        }
    }
    assert!(!instance.is_empty() && !server.is_empty() && port > 0, "header unreadable");
    (instance, server, port, address.parse().expect("IPv4"), txt)
}

fn airplay_service(txt: &[(String, String)], port: u16, address: Ipv4Addr) -> Service {
    let friendly = txt
        .iter()
        .find(|(k, _)| k == "name")
        .map(|(_, v)| v.clone())
        .expect("the corpus's TXT table carries `name`");
    let device_id = txt
        .iter()
        .find(|(k, _)| k == "deviceid")
        .map(|(_, v)| v.clone())
        .expect("…and `deviceid`");
    let dir = setting("airplay-bytes");
    dir.set(macast_core::setting::Key::DlnaFriendlyName, serde_json::json!(friendly));
    Service::for_airplay(&dir, port, vec![address], &device_id)
}

#[test]
fn the_airplay_announcement_is_the_reference_s_bytes() {
    crate::pin_test_dir();
    let (instance, server, port, address, txt) = airplay_inputs();
    let service = airplay_service(&txt, port, address);
    // The two things a builder can get wrong about a *second* service type: whose
    // name is on the wire, and which table rides along.
    assert_eq!(service.instance, instance, "the instance name is not the reference's");
    assert_eq!(service.server, server, "the SRV target is not the reference's");
    assert_eq!(
        service.service_type,
        crate::airplay::AIRPLAY_SERVICE,
        "announcing the Chromecast type under an AirPlay name would be worse than not announcing"
    );
    // The reference's `A`/`NSEC` pair has no order (the corpus carries both lines
    // for that reason), so either match is faithful — and **both** are asserted,
    // because "one of these two" alone would also be true of a writer that emitted
    // neither.
    let announced = vec![
        airplay_corpus("announce-nsec-first"),
        airplay_corpus("announce-a-first"),
    ];
    assert!(
        announced.contains(&service.announcement()) || announced.contains(&service.announcement_a_first()),
        "neither of our two orders is one of the reference's two"
    );
    assert_eq!(service.announcement(), announced[0], "our nsec-first order is not the reference's");
    assert_eq!(service.announcement_a_first(), announced[1], "our a-first order is not the reference's");
    assert_eq!(service.goodbye(), airplay_corpus("goodbye-nsec-first"));
    assert_eq!(service.goodbye_a_first(), airplay_corpus("goodbye-a-first"));
    // The TXT order is on the wire, so byte parity above already proves the table
    // is in the reference's order — this row says *which* claim that is, because a
    // reader comparing key sets would accept any order and the senders that cache
    // this record do not.
    assert_eq!(
        service.txt.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        txt.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        "the nine keys are not in the order the reference writes them"
    );
}

#[test]
fn the_airplay_service_is_announced_and_never_answered() {
    crate::pin_test_dir();
    let (_instance, _server, port, address, txt) = airplay_inputs();
    let airplay = airplay_service(&txt, port, address);
    let cast = fixture_service();
    assert!(
        cast.responder_answers(),
        "the Chromecast service is the one the 17-row corpus measured"
    );
    assert!(
        !airplay.responder_answers(),
        "a second service type answers nothing until the reference is asked about it"
    );

    // And the flag is obeyed, not just computed: feed the send-only loop a query
    // this same service **would** answer if it were allowed to, and check what left
    // the socket. Every packet must be one of our own announcements or the goodbye —
    // a reply is the only other shape, and it is the one with a question section.
    let logger = Logger::named("Discovery-SendOnly");
    let path = logger.claim();
    let running = AtomicBool::new(true);
    let sent: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&sent);
    let mut send = move |bytes: &[u8]| {
        sink.lock().unwrap().push(bytes.to_vec());
        Ok(())
    };
    let mut unicast = |_bytes: &[u8], _peer: SocketAddr| Ok(());
    let announced = cast.announcement();
    let question = ptr_query(SERVICE_TYPE);
    let mut queue = std::collections::VecDeque::from(vec![question.clone()]);
    let mut recv = || match queue.pop_front() {
        Some(bytes) => Ok(Some((bytes, peer(MDNS_PORT)))),
        None => {
            running.store(false, Ordering::SeqCst);
            Ok(None)
        }
    };
    {
        // The one-time "I am not answering" line is DEBUG, so without the window
        // this assertion would be about the scheduler rather than the product.
        let _debug = crate::DebugWindow::armed();
        // The service here is the **AirPlay** one: what this test asks is that a
        // service whose predicate says "not measured" is announced and never answered.
        // Before the merge it fed the Cast service with an explicit `false`, which
        // tested the flag but not the decision — and after the merge that state is
        // unreachable from the product, so the test now walks the state that is.
        run(
            std::slice::from_ref(&airplay),
            &mut send,
            &mut unicast,
            &mut recv,
            &running,
            &logger,
        );
    }
    let wrote = sent.lock().unwrap();
    assert!(!wrote.is_empty(), "the announcer sent nothing at all — the fixture is broken");
    let replies: Vec<&Vec<u8>> = wrote
        .iter()
        .filter(|bytes| *bytes != &announced && mdns::parse_message(bytes, "test", &logger)
            .map(|doc| !doc.questions.is_empty())
            .unwrap_or(false))
        .collect();
    assert!(
        replies.is_empty(),
        "a send-only socket answered {} packet(s) with a question section in them",
        replies.len()
    );
    let text = read_tail(&path, 200).text;
    assert!(
        !text.contains("mDNS engine heard query"),
        "the answering sentences fired on a socket that is not allowed to answer: {text}"
    );
    assert!(
        !text.contains("answered its first query"),
        "same, for the answering half: {text}"
    );
    assert!(
        text.contains("did not answer it"),
        "it stayed quiet without saying why — that is the reading that costs an hour: {text}"
    );
}
