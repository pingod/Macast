#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# The reference answers a query. Our port does not — that is §15's remaining
# row, and the cost is a concrete sentence: a sender that only listens (libvlc's
# microdns) sees us in the first announcement burst, while a sender that **asks**
# (the Cast app, `dns-sd -B`, a cold browser) waits for the next re-announcement,
# which is up to 60 seconds.
#
# So the responder cannot be written from RFC prose, and it cannot be written
# from reading `zeroconf`'s source either — the same reason `announce.rs` exists:
# the package ships Cython builds of these modules, the answer is in the **bytes**,
# and the interesting rules (which records ride along as additionals, which TTL,
# whether the cache-flush bit doubles as the unicast request, what suppression
# does to an answer we already know) are exactly the ones a text read gets wrong.
#
# What this does: starts a real `Zeroconf` restricted to **loopback**, registers
# the Chromecast service the app itself would register (the property table comes
# from `_advertise` with the advertiser swapped for a recorder — see
# `mdns_announce_probe.py`, whose `cast_properties()` and `build_info()` this file
# imports rather than copies: one builder for both probes), then **sends queries
# at it on 224.0.0.251:5353 pinned to lo0** and records what it would have put on
# the wire.
#
# Two isolation facts, both load-bearing:
#
# * `zeroconf._core.async_send_with_transport` is replaced by a recorder, and it
#   is the only `transport.sendto` in the package (grep-verified once, in
#   `mdns_announce_probe.py`'s header). Nothing this probe answers actually
#   leaves: a real answer to a real query would put a Chromecast-looking response
#   on the LAN for every case below.
# * The query datagrams themselves go out `lo0` (`IP_MULTICAST_IF=127.0.0.1`,
#   TTL 1), so they reach our own listener and no neighbour. The *responder that
#   is not ours* — macOS's mDNSResponder — also hears them, and its reply is not
#   ours to suppress; a PTR query for `_googlecast._tcp` is what every Apple
#   device on this LAN sends routinely, and nothing here answers it on his behalf.
#
# Feeding the query through the socket rather than calling
# `QueryHandler.handle_assembled_query()` directly is deliberate: the handler
# decides `ucast_source` from the **source port** (`port != 5353`), and that is a
# property of the packet, not of the handler. Two cases below vary it, which they
# could not do against a direct call.
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_responder_probe.py
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_responder_probe.py --json
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_responder_probe.py --case ptr
#
# Exit code: 0 = every case ran and was decoded (a case that correctly produced
# **no** answer is a result, not a failure); 1 = a packet could not be decoded, or
# the listener never heard our query at all (then every row is empty and the
# table would read like "the reference is silent", which is a lie);
# 2 = environment problem (no zeroconf, cannot bind).

import argparse
import copy
import json
import os
import socket
import sys
import tempfile
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, "scripts"))

# The announce probe sets its own temp setting dir at import; ours goes **after**
# so `macast.utils` (imported lazily inside its functions) freezes the one this
# file's run owns. Same rule as everywhere else in this repo: the isolation
# happens before anything from `macast` is imported (AGENTS.md §4.9), because
# `Setting.get(key, default)` writes the default into the user's file.
_TMP_SETTING_DIR = tempfile.mkdtemp(prefix="macast-mdns-responder.")
import mdns_announce_probe as _announce  # noqa: E402  (its imports are side-effect-free)

os.environ["MACAST_SETTING_DIR"] = _TMP_SETTING_DIR

try:
    import zeroconf as _z
    import zeroconf._core as _zc_core
    from zeroconf._protocol.incoming import DNSIncoming
    from zeroconf._protocol.outgoing import DNSOutgoing
    from zeroconf import const
    from zeroconf._dns import DNSQuestion
except Exception as e:  # pragma: no cover - environment
    print("zeroconf is not importable here: %r" % (e,), file=sys.stderr)
    raise SystemExit(2)

MDNS_GROUP = "224.0.0.251"
MDNS_PORT = 5353
DNS_TYPE = {1: "A", 12: "PTR", 16: "TXT", 28: "AAAA", 33: "SRV", 47: "NSEC", 255: "ANY"}
ENUM_TYPE = "_services._dns-sd._udp.local."
# RFC 6763's "DQE" star form. It is a *valid* question a modern client may send,
# and the row below asks rather than assumes whether the reference knows it.
ENUM_STAR = "*._dns-sd._udp.local."

CAPTURED = []
_LOCK = threading.Lock()
_T0 = [0.0]
_CASE = ["(none)"]


def _record(log_debug, transport, packet, packet_num, out, addr, port, v6_flow_scope=()):
    """Stand in for `async_send_with_transport`: record, never send."""
    with _LOCK:
        CAPTURED.append({
            "t_ms": round((time.monotonic() - _T0[0]) * 1000, 1),
            "case": _CASE[0],
            "dst": addr if addr is not None else ("::1" if getattr(transport, "is_ipv6", False) else MDNS_GROUP),
            "port": port or MDNS_PORT,
            "packet_num": packet_num,
            "bytes": bytes(packet),
        })


def _as_list(value):
    """Read `DNSIncoming.answers` / `.questions` whichever shape this build has.

    The wheel ships a Cython build where `answers` is a **method** and `questions`
    is a plain list; the pure-Python module the wheel also carries has both as
    attributes. Writing this once, tolerating both, is what keeps the probe from
    "working" only on whichever build this machine happened to install — §4.2's
    「桩要按协议写完整」, applied to the reader instead of the writer.
    """
    return value() if callable(value) else list(value)


def query_bytes(name, rtype, unique=False, known=None):
    """One query packet, built by the reference's own writer.

    `unique` sets the cache-flush bit **in the question's class**, which is where
    mDNS puts the unicast-request bit — and which `zeroconf` reads as exactly
    that (`is_unicast = question.unique`, `query_handler.py:236`). A hand-rolled
    header would let this file's own opinion about where QU lives decide the
    case, so the writer is `DNSOutgoing` and the only choice made here is the bit.

    `known` records go in the answer section: the "I already hold this with this
    much TTL left" form the suppression rule is about. They are written with
    `add_answer_at_time(record, current_time_millis())`, whose second argument is
    **the clock to compute remaining TTL against**, not the moment the asker got
    the record — this file's first version read it as the latter, passed a past
    timestamp, and got `ttl=9000` on a 4500-second record: the "expired" case then
    measured a *longer* TTL and every suppression row came out empty for a reason
    that had nothing to do with the rule. So the age a querier claims is expressed
    by the record's own `ttl` (`known_record` below), which is the only way that
    survives a wrong idea about the parameter.
    """
    out = DNSOutgoing(const._FLAGS_QR_QUERY)
    cls = const._CLASS_IN | (const._CLASS_UNIQUE if unique else 0)
    out.add_question(DNSQuestion(name, rtype, cls))
    now = _z.current_time_millis()
    for record in known or []:
        out.add_answer_at_time(record, now)
    return out.packets()[0]


def known_record(info, kind, ttl, alias=None):
    """One of our records with a caller-chosen TTL, for the known-answer section.

    **A copy, always.** `ServiceInfo.dns_pointer()` returns a *cached* object —
    the same instance the responder will later answer with — so writing `ttl` on
    it writes it on the registered service. This file's first version did exactly
    that, and the damage was invisible in just the way this probe exists to catch:
    after the "TTL 100" row ran, the responder's own PTR had 100 seconds left, so
    the suppression test `other.ttl > record.ttl / 2` became `4500 > 50`,
    `2250 > 50`, `100 > 50` — all true, all three rows "suppressed", and the
    following row's answer came out carrying `an PTR ttl=100`. Three cases and a
    wrong TTL in a fourth, all of it produced by the instrument, and all of it
    readable as "the reference suppresses more aggressively than the source says".

    So: deepcopy, then set the TTL on the copy, and `assert_service_untouched`
    below asks the question after every case rather than trusting this paragraph.
    """
    record = {
        "ptr": info.dns_pointer,
        "srv": info.dns_service,
        "txt": info.dns_text,
    }[kind]()
    if alias is not None:
        from zeroconf import DNSPointer

        # A PTR of ours in name, pointing elsewhere: `suppresses()` looks records
        # up by equality and a PTR's equality includes the alias, so this is the
        # shape that must NOT suppress — and building it from the reference's own
        # field names is what makes "the same record" the reference's question.
        record = DNSPointer(record.name, record.type, record.class_, ttl, alias)
    else:
        record = copy.deepcopy(record)
        record.ttl = ttl
    return record


def assert_service_untouched(info, baseline, label):
    """Fail loudly if the probe has edited the responder's own records."""
    now = [(record.name, record.type, record.ttl) for record in (
        info.dns_pointer(), info.dns_service(), info.dns_text(),
    )]
    if now != baseline:
        raise AssertionError(
            "the %s case changed the registered service's records: %r (was %r)"
            % (label, now, baseline)
        )


def decode(data):
    """Read one captured datagram with the reference's own parser, then by name.

    `DNSIncoming` is the parser the reference uses on the wire, so a field this
    table prints is a field that parser agrees exists. It exposes the records as
    **one flat list** (`answers`) plus three counts, so the section a record is
    in comes from slicing by those counts — and the slice is asserted rather than
    trusted: if `num_answers + num_authorities + num_additionals != len(answers)`
    this file's idea of the layout and the reference's parser disagree, and that
    is reported instead of quietly printing a plausible table (§4.9's
    「两个见证者必须分开报」).

    Record types outside `DNS_TYPE` print as their number rather than being
    dropped: an unexpected type in an answer is precisely what a summary swallows.
    """
    msg = DNSIncoming(data)
    records = _as_list(msg.answers)
    counts = (msg.num_answers, msg.num_authorities, msg.num_additionals)
    if sum(counts) != len(records):
        raise ValueError("sections %r do not cover %d record(s)" % (counts, len(records)))
    split = ([("an", r) for r in records[: counts[0]]]
             + [("ns", r) for r in records[counts[0]: counts[0] + counts[1]]]
             + [("ar", r) for r in records[counts[0] + counts[1]:]])
    rows = []
    for section, record in split:
        flush = bool(getattr(record, "unique", False))
        rows.append({
            "section": section,
            "name": record.name,
            "type": DNS_TYPE.get(record.type, str(record.type)),
            "cache_flush": flush,
            "ttl": record.ttl,
        })
    return {
        "id": msg.id,
        "qr": msg.is_query() is False,
        "authoritative": bool(msg.flags & const._FLAGS_AA),
        "questions": [
            {"name": q.name, "type": DNS_TYPE.get(q.type, str(q.type)), "unicast_request": q.unique}
            for q in _as_list(msg.questions)
        ],
        "records": rows,
    }


def send_query(payload, src_port):
    """Put one query on lo0's mDNS group, from the source port the case asks for.

    `src_port=5353` is what a real mDNS listener sends from; any other value is
    what a *legacy unicast client* or a querier on an ephemeral port looks like to
    the responder (`ucast_source = port != 5353`). Binding to port 5353 competes
    with the listener's own socket, which the dispatch probe measured as harmless
    for multicast (`scripts/mdns_reuseport_probe.py`): multicast is duplicated to
    every socket, so pinning the source port here does not blind the responder.
    """
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    # Port 0 is the ephemeral case; 5353 is what a real mDNS listener sends from.
    sock.bind(("127.0.0.1", src_port))
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton("127.0.0.1"))
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    written = sock.sendto(payload, (MDNS_GROUP, MDNS_PORT))
    sock.close()
    return written


def cases_for(info, server):
    """The case table, as dicts.

    Every `why` is a sentence the Rust side has to reproduce or explain, so the
    list is the specification and the printed table is the evidence.
    """
    return [
        {"label": "ptr", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "why": "the phone's question: who is a Chromecast — does the answer carry PTR plus SRV/TXT/A additionals?"},
        {"label": "srv", "name": info.name, "type": const._TYPE_SRV, "src": 5353,
         "why": "one instance's target: does the A record ride along as an additional, and with which TTL"},
        {"label": "a", "name": server, "type": const._TYPE_A, "src": 5353,
         "why": "the host address on its own — cache-flush expected on our own record"},
        {"label": "txt", "name": info.name, "type": const._TYPE_TXT, "src": 5353,
         "why": "the TXT blob alone"},
        {"label": "ptr-qu", "name": info.type, "type": const._TYPE_PTR, "unique": True, "src": 5353,
         "why": "QU bit in the question class: the reference reads it as `is_unicast = question.unique`"},
        {"label": "ptr-ephemeral-src", "name": info.type, "type": const._TYPE_PTR, "src": 0,
         "why": "source port != 5353 ⇒ ucast_source ⇒ the reply goes back unicast to the asker"},
        {"label": "ptr-known-live", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "known": [("ptr", 4500, None)],
         "why": "known-answer suppression, full TTL left: the asker holds our PTR, so it must NOT be re-sent"},
        {"label": "ptr-known-half", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "known": [("ptr", 2250, None)],
         "why": "the boundary: the rule is `other.ttl > record.ttl / 2`, and 2250 is exactly half of 4500 — "
                "so this one must still be answered. Without a row on the boundary, a port that used `>=` "
                "would match every other reading of the rule"},
        {"label": "ptr-known-low", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "known": [("ptr", 100, None)],
         "why": "the asker holds the PTR with 100 s left: must be answered — this is the row a broken "
                "TTL clock cannot imitate, because a wrong clock makes the held TTL bigger, never smaller"},
        {"label": "ptr-known-other-alias", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "known": [("ptr", 4500, "Other-Box._googlecast._tcp.local.")],
         "why": "a known PTR of ours in name but pointing at a DIFFERENT instance: suppression matches by "
                "record equality, and a PTR's equality includes its alias, so this must be answered"},
        {"label": "srv-known-ptr", "name": info.name, "type": const._TYPE_SRV, "src": 5353,
         "known": [("ptr", 4500, None)],
         "why": "is suppression per record or per question: the asker knows the PTR but asked for the SRV"},
        {"label": "ptr-known-srv", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "known": [("srv", 120, None)],
         "why": "does suppression reach the RIDING records? the asker knows the SRV but not the PTR, "
                "and the SRV would normally come along as an additional. Nobody asked this until a "
                "mutant that suppresses additionals too went 0-red — this row is that hole closed"},
        {"label": "other-service", "name": "_airplay._tcp.local.", "type": const._TYPE_PTR, "src": 5353,
         "why": "a type we do not run: silence, and no error"},
        {"label": "any", "name": info.name, "type": const._TYPE_ANY, "src": 5353,
         "why": "does the reference answer ANY at all, or is that a question it has no strategy for"},
        {"label": "enum", "name": ENUM_TYPE, "type": const._TYPE_PTR, "src": 5353,
         "why": "service-type enumeration: the answer should be a PTR naming our type, not our instance"},
        {"label": "enum-star", "name": ENUM_STAR, "type": const._TYPE_PTR, "src": 5353,
         "why": "RFC 6763's DQE star form — does the reference know it at all, or only the long name"},
        {"label": "aaaa", "name": server, "type": const._TYPE_AAAA, "src": 5353,
         "why": "v6 address while the listener is V4Only — the honest answer here is 'nothing'"},
        # ---- the aggregation queue (`respond::timing`'s other half) ------------------
        # These four rows exist because the port answers everything immediately: it
        # reproduces *which* records are "immediate" (frozen in the 17-row corpus) but
        # has no queue, so it cannot coalesce. Whether a queue is worth its complexity is
        # not readable from `_handlers/multicast_outgoing_queue.py` — the merge test
        # compares a `send_after` built from a fresh `randint(20, 120)`, so the answer is
        # probabilistic, and the wheel ships Cython builds of these modules. So: ask the
        # running engine how many **datagrams** come out and at which offsets.
        {"label": "txt-twice", "name": info.name, "type": const._TYPE_TXT, "src": 5353,
         "also": [(info.name, const._TYPE_TXT, False, 10)],
         "queue": True,
         "why": "the same aggregated question twice inside one window: one datagram (coalesced) "
                "or two (each question answered on its own)? This is the thundering-herd case",
         "answer": "measured 2026-10-20: ONE datagram, 212 bytes, at +104.1 ms — the second question "
                   "added nothing to the wire. Two questions inside one window are one answer."},
        {"label": "ptr-then-txt", "name": info.type, "type": const._TYPE_PTR, "src": 5353,
         "also": [(info.name, const._TYPE_TXT, False, 10)],
         "queue": True,
         "why": "two different aggregated questions 10 ms apart: does the second answer ride in the "
                "first group's packet, and does the PTR's riding SRV/A survive the merge?",
         "answer": "measured 2026-10-20: ONE datagram at +502.3 ms carrying TXT + PTR + A + SRV + "
                   "NSEC (296 bytes). The merge keeps the riding records, and the offset sitting at the "
                   "500 ms aggregation ceiling rather than in the 20-120 ms band is the queue's own "
                   "send_before deferral: a second group exists, so the first waits for its deadline "
                   "instead of going out early (multicast_outgoing_queue.py:79-86)."},
        {"label": "srv-then-txt", "name": info.name, "type": const._TYPE_SRV, "src": 5353,
         "also": [(info.name, const._TYPE_TXT, False, 10)],
         "queue": True,
         "why": "an immediate question then an aggregated one: the split has to show as TWO datagrams "
                "at different offsets — a port that queues both, or sends both now, is caught here",
         "answer": "measured 2026-10-20: TWO datagrams — SRV+A+NSEC at +0.7 ms, TXT at +87.5 ms. The "
                   "four buckets are real, and the 86.8 ms between them is randint(20, 120) doing its job."},
        {"label": "txt-twice-1s", "name": info.name, "type": const._TYPE_TXT, "src": 5353,
         "also": [(info.name, const._TYPE_TXT, False, 1250)],
         "queue": True,
         "why": "the same question again after the one-second window closes: the second answer must "
                "come out again (this is what keeps a retrier from being starved), and the row "
                "separates 'coalesce forever' from 'coalesce within one window'",
         "answer": "measured 2026-10-20: TWO datagrams, +45.9 ms and +1339.6 ms. The second landing past "
                   "a second is out_delay_queue (additional_delay=1000, query_handler.py:293) — and it "
                   "reached that queue because _has_mcast_record_in_last_second consulted the CACHE, "
                   "which had learned the TXT from the responder's OWN looped-back answer. So the "
                   "reference's flood control is fed by hearing itself: a port that never reads its own "
                   "answers cannot reach this row except by keeping its own ledger of what it just "
                   "multicast — which is the same self-traffic §16 第 13 条 is about."},
    ]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="what the reference answers to an mDNS query, in bytes"
    )
    parser.add_argument("--case", help="run only this label (see the table)")
    parser.add_argument("--settle", type=float, default=1.2,
                        help="seconds to wait per case, to catch the aggregated replies")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument("--dump-corpus", metavar="PATH",
                        help="write the answers as JSONL, one line per case: the form the Rust "
                             "port diffs against (records + TTLs + section, NOT raw bytes — the "
                             "reference puts the additionals in set order, so its bytes are not one "
                             "canonical stream; the announcement corpus has the same problem and "
                             "freezes two rows for it, which a per-case table does not need)")
    args = parser.parse_args()

    service_type, name, port, props = _announce.cast_properties()
    # The same fixture the frozen announcement corpus uses: a TEST-NET-1 address
    # and a fixed USN, so the output describes a protocol and not this machine.
    info, instance, server, addresses, _props = _announce.build_info(port, name, props, fixture=True)
    if service_type != info.type:
        print("the advertiser's service type drifted from the fixture: %r vs %r"
              % (service_type, info.type), file=sys.stderr)
        return 2

    zc = _z.Zeroconf(interfaces=["127.0.0.1"], ip_version=_z.IPVersion.V4Only)
    _zc_core.async_send_with_transport = _record
    try:
        zc.register_service(info, cooperating_responders=True)
    except Exception as e:  # pragma: no cover - environment
        print("register failed: %r" % (e,), file=sys.stderr)
        return 2

    results = []
    try:
        # Let the registration bursts finish before clearing, so a captured
        # datagram belongs to the case whose label it carries and not to the
        # announcement that happened to still be in flight.
        time.sleep(1.0)
        with _LOCK:
            CAPTURED.clear()
        # What the responder believes about itself, before any case can change it.
        baseline = [(r.name, r.type, r.ttl) for r in (
            info.dns_pointer(), info.dns_service(), info.dns_text(),
        )]

        # The `ptr` row at the end of the table is this probe's positive control;
        # see the check after the loop, which refuses to let a blind listener
        # print eleven rows of "the reference did not answer".
        for case in cases_for(info, server):
            label = case["label"]
            if args.case and args.case != label:
                continue
            # `known` entries are (kind, ttl, alias) triples built through
            # `known_record`, i.e. the reference's own records with only the TTL
            # replaced — see its docstring for why the age is expressed that way.
            known = [known_record(info, kind, ttl, alias)
                     for kind, ttl, alias in case.get("known", [])]
            # After building the known section, not before: this is the point where
            # a shallow copy would already have done the damage.
            assert_service_untouched(info, baseline, label)
            _CASE[0] = label
            _T0[0] = time.monotonic()
            payload = query_bytes(case["name"], case["type"], case.get("unique", False), known)
            if send_query(payload, case["src"]) != len(payload):
                # A short write would make "no answer" mean "we never sent the
                # whole query" — a third kind of empty, and the one the receiving
                # side cannot distinguish from the table above.
                print("the %s query was not fully sent" % label, file=sys.stderr)
                return 1
            # The aggregation cases: follow-up questions, same label, each after its
            # own gap. Sent through the same `send_query` (so the same source-port
            # semantics) and recorded under the same label, because the question these
            # rows answer is **how many datagrams come out for the pair**.
            for name_, type_, unique_, gap_ms in case.get("also", []):
                time.sleep(gap_ms / 1000.0)
                extra = query_bytes(name_, type_, unique_)
                if send_query(extra, case["src"]) != len(extra):
                    print("the %s follow-up query was not fully sent" % label, file=sys.stderr)
                    return 1
            deadline = time.monotonic() + args.settle
            while time.monotonic() < deadline:
                time.sleep(0.02)
            with _LOCK:
                captured = [row for row in CAPTURED if row["case"] == label]
                CAPTURED.clear()
            decoded = []
            for row in captured:
                try:
                    parsed = decode(row["bytes"])
                except ValueError as exc:
                    print("a captured datagram for %s does not lay out the way the "
                          "reference's parser counts it: %s" % (label, exc), file=sys.stderr)
                    return 1
                parsed.update({"t_ms": row["t_ms"], "dst": row["dst"], "port": row["port"],
                               "bytes": len(row["bytes"])})
                decoded.append(parsed)
            answers = [row for row in decoded if row["qr"]]
            results.append({
                "case": label, "question": case["name"],
                "queue": bool(case.get("queue")),
                "answer": case.get("answer", ""),
                "type": DNS_TYPE.get(case["type"], str(case["type"])),
                "unique": case.get("unique", False), "src_port": case["src"],
                "known": case.get("known"), "why": case["why"],
                "datagrams": len(captured), "responses": answers,
            })
    finally:
        zc.unregister_service(info)
        zc.close()

    # Positive control, stated as a case rather than a counter: the plain
    # PTR query from port 5353 is the one question the reference cannot decline,
    # because our service is registered and no known-answer was supplied. If that
    # row is empty, every other empty row is about this probe — the query never
    # reached the responder's socket — and printing them as "the reference does
    # not answer" would be a lie with eleven rows (§4.14's 「空结果要说清是哪一种空」).
    control = [row for row in results if row["case"] == "ptr"]
    if not args.case and (not control or not control[0]["responses"]):
        print("\nThe PTR control produced no answer: our own query never reached the\n"
              "responder's socket. Every empty row above is about this probe, not\n"
              "about the reference — the table is not evidence of silence.", file=sys.stderr)
        return 1

    if args.dump_corpus:
        with open(args.dump_corpus, "w", encoding="utf-8") as handle:
            # **The corpus is the stateless decision table, and only that.** The four
            # `queue` rows ask "how many datagrams, at which offsets" — a question no call
            # to `respond::respond` can answer, because it holds nothing between calls.
            # Dumping them into the same file would hand the Rust diff a row whose honest
            # answer is "not implemented yet", and the way that fails is a silent skip. So
            # they are excluded **by name and counted**: move the count and this line goes
            # red instead of quietly shrinking the corpus (§28's counted Timeout exclusion is
            # the same rule — a comparison that allows an unbounded difference has stopped
            # being a comparison).
            queue_rows = [r for r in results if r.get("queue")]
            if len(queue_rows) != 4:
                print("expected 4 queue rows to be excluded from the decision corpus, found %d: %s"
                      % (len(queue_rows), [r["case"] for r in queue_rows]), file=sys.stderr)
                return 1
            print("excluded %d queue row(s) from the corpus: %s"
                  % (len(queue_rows), ", ".join(r["case"] for r in queue_rows)), file=sys.stderr)
            for result in [r for r in results if not r.get("queue")]:
                row = dict(result)
                # The two keys added for the queue rows are prose and a filter, not part
                # of the decision document: leaving them in would change all 17 frozen
                # rows' key sets and the Rust diff would go red about bookkeeping.
                row.pop("queue", None)
                row.pop("answer", None)
                # `t_ms` is a *scheduling* observation (which of the four answer
                # buckets the reference chose: ~0-1 ms for mcast_now, 50-120 ms for
                # the aggregated queue, and a unicast reply when the source port was
                # not 5353). It is kept because it is measured, and marked as not a
                # diff target: a Rust test that asserted a millisecond figure would
                # be asserting this machine's timer, not a protocol fact.
                row["dsts"] = sorted({"%s:%s" % (r["dst"], r["port"]) for r in row["responses"]})
                row["delays_ms"] = sorted({r["t_ms"] for r in row["responses"]})
                row["answers"] = [
                    {"section": record["section"], "type": record["type"],
                     "ttl": record["ttl"], "name": record["name"],
                     "cache_flush": record["cache_flush"]}
                    for record in sorted(
                        (r for row_ in row["responses"] for r in row_["records"]),
                        key=lambda item: (item["section"], item["type"], item["name"]),
                    )
                ]
                row.pop("responses")
                handle.write(json.dumps(row, sort_keys=True) + "\n")
        # The count is of what was **written**, not of what was collected: this line is
        # the only place a reader learns the corpus is 17 rows while the probe ran 21
        # cases, and a number that quietly includes the excluded rows is how a future
        # reader decides the filter is broken when it is not.
        print("wrote %d corpus row(s) of %d case(s) to %s"
              % (len(results) - len(queue_rows), len(results), args.dump_corpus), file=sys.stderr)

    if args.json:
        print(json.dumps({"fixture": {"instance": instance, "server": server,
                                      "address": _announce.FIXTURE["address"]},
                          "results": results}, indent=2, default=str))
    else:
        print("fixture instance %s (server %s, address %s)\n"
              % (instance, server, _announce.FIXTURE["address"]))
        for result in results:
            print("── %s  (%s %s%s, from port %s)"
                  % (result["case"], result["type"], result["question"],
                     " QU" if result["unique"] else "", result["src_port"]))
            print("   asks: %s" % result["why"])
            if result.get("answer"):
                # Printed under the question it answers, and carried into --json, so the
                # reading travels with the case instead of living only in this file's prose.
                print("   answers: %s" % result["answer"])
            if result["known"]:
                print("   known-answer section: %s" % result["known"])
            if not result["responses"]:
                print("   ** no answer **  (%d datagram(s) captured)" % result["datagrams"])
            for row in result["responses"]:
                print("   +%sms → %s:%s  id=%s aa=%s  %d bytes"
                      % (row["t_ms"], row["dst"], row["port"], row["id"],
                         row["authoritative"], row["bytes"]))
                for record in row["records"]:
                    print("      %s %-4s ttl=%-5d flush=%-5s  %s"
                          % (record["section"], record["type"], record["ttl"],
                             record["cache_flush"], record["name"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
