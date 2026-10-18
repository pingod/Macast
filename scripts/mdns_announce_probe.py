#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# What does the reference actually put on the wire when it announces a
# Chromecast receiver? This question used to be answered by reading
# `zeroconf`'s source, and the Rust port cannot be written from reading: the
# announcement is a **packet builder**, so the facts we need are the ones a
# byte-level witness can give — which records, in which section, with which
# TTL, with or without the cache-flush bit, to how many destinations, and how
# far apart in time.
#
# Nothing here touches the network. `zeroconf._core.async_send_with_transport`
# is the **only** place in the package that calls `transport.sendto`
# (`_core.py:134`, verified with a grep over the installed tree), so replacing
# it records every would-be datagram and lets none of them leave. A real
# announcement would put a second Chromecast-looking device on this LAN for
# the length of the run — the same cost `verify_rust_receiver.sh` declares for
# SSDP, and the reason it says *do not run this during a demo*. Here the cost
# is zero, so the run is safe at any time.
#
# Two more isolation facts, both load-bearing:
#
# * `MACAST_SETTING_DIR` is a fresh temp dir **before** importing anything from
#   `macast`, because `_local_addresses()` reaches `Setting.resolved_network_interface()`,
#   and `Setting.get(key, default)` writes the default into the dict and
#   persists it (AGENTS.md §4.2). A probe that reads the user's settings file
#   is not a read-only probe.
# * `register_service(..., cooperating_responders=True)` skips the
#   uniqueness-probing phase (`_core.py:250-262` returns early). With sends
#   patched, no probe would ever be answered, so a strict registration would
#   spend its timeout waiting for a reply to a packet we never sent — and the
#   announcements we came to measure would never be generated at all. The probe
#   **packet shape** is still measured, by calling `generate_service_query()`
#   directly instead of registering.
#
# The property table is not transcribed from this file's own opinion either:
# `_advertise` is **called** with `MDNSAdvertiser` replaced by a recorder, so the
# dict printed here is the one the app would have handed zeroconf — `id` and `fn`
# included, which are not literals in that source and could not be read out of a
# text scan. A fixture that asks the product instead of copying it cannot drift
# from it (§4.13's 「每个数字问两遍」, applied to an instrument).
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_announce_probe.py
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_announce_probe.py --seconds 3 --json
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_announce_probe.py --dump /tmp/announce.bin
#
# Exit code: 0 = captured at least one announcement and decoded it;
# 1 = nothing captured, or the two readers disagree about a packet;
# 2 = environment problem (no zeroconf, cannot bind).

import argparse
import json
import os
import socket
import struct
import sys
import tempfile
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# --- isolation, before any macast import ------------------------------------
# `macast.utils` freezes SETTING_DIR at import time, so this must happen first.
_TMP_SETTING_DIR = tempfile.mkdtemp(prefix="macast-mdns-probe.")
os.environ["MACAST_SETTING_DIR"] = _TMP_SETTING_DIR

sys.path.insert(0, REPO)

try:
    import zeroconf as _z
    import zeroconf._core as _zc_core
    from zeroconf._protocol.incoming import DNSIncoming
except Exception as e:  # pragma: no cover - environment
    print("zeroconf is not importable here: %r" % (e,), file=sys.stderr)
    raise SystemExit(2)

CAST_SERVICE = "_googlecast._tcp.local."
# The second protocol this probe can be pointed at. Not "the same file with a
# different string": the AirPlay table is a different question (nine TXT
# properties instead of ~20, a per-process random `deviceid` instead of a
# stored USN, an instance name that is already sanitized by the caller), so
# the fixture overrides and the honesty checks differ — see --protocol.
AIRPLAY_SERVICE = "_airplay._tcp.local."
DNS_TYPE = {1: "A", 6: "SOA", 12: "PTR", 16: "TXT", 33: "SRV", 47: "NSEC"}


def cast_properties():
    """Ask the product itself for the table, by swapping its advertiser out.

    Not a parse of `protocol_cast.py`: `_advertise` is called with a stub `self`
    and a recorder in place of `MDNSAdvertiser`, so the dict below is the one the
    app would have handed zeroconf — `id` and `fn` included, which are not
    literals in that source and could not be read out of a text scan anyway. If
    the product's table changes, this fixture changes with it, in the same
    second, with nothing to remember.
    """
    from macast import protocol_cast

    calls = []

    class _Recorder:
        def __init__(self, *a, **kw):
            pass

        def advertise(self, service_type, name, port, properties, server=None):
            calls.append((service_type, name, port, dict(properties), server))
            return True

        def close(self):
            pass

        def unadvertise(self, *a, **kw):
            pass

    original = protocol_cast.MDNSAdvertiser
    protocol_cast.MDNSAdvertiser = _Recorder
    try:
        class _Stub:
            # `_advertise` only reads `self.cast_port`; `__init__` sets it from
            # this constant before any fallback kicks in.
            cast_port = protocol_cast.CAST_PORT

        stub = _Stub()
        protocol_cast.ChromecastProtocol._advertise(stub)
    finally:
        protocol_cast.MDNSAdvertiser = original
    if not calls:
        raise SystemExit("_advertise did not reach MDNSAdvertiser.advertise")
    service_type, name, port, props, server = calls[0]
    return service_type, name, port, props


def airplay_properties():
    """Ask `AirPlayProtocol._advertise` what it hands the advertiser.

    Same discipline as `cast_properties`: the table is **produced** by the product
    with `MDNSAdvertiser` replaced by a recorder, not copied out of
    `protocol_airplay.py` by eye. Two reasons this matters more here than for
    Chromecast: `deviceid` is not a literal anywhere (it is `_random_mac()`, drawn
    per process — see the note in the fixture block), and `features` is
    `"0x%X" % FEATURES`, i.e. the wire text is a formatting of an integer the
    module builds itself.

    The stub carries the two attributes `_advertise` reads: `rtsp_port` (the port
    actually bound, which is what the reference advertises after its fallback loop)
    and `_device_id`.
    """
    from macast import protocol_airplay

    calls = []

    class _Recorder:
        def __init__(self, *a, **kw):
            pass

        def advertise(self, service_type, name, port, properties, server=None):
            calls.append((service_type, name, port, dict(properties), server))
            return True

        def close(self):
            pass

        def unadvertise(self, *a, **kw):
            pass

    original = protocol_airplay.MDNSAdvertiser
    protocol_airplay.MDNSAdvertiser = _Recorder
    try:
        class _Stub:
            rtsp_port = protocol_airplay.AIRPLAY_PORT
            _device_id = protocol_airplay._random_mac()

        protocol_airplay.AirPlayProtocol._advertise(_Stub())
    finally:
        protocol_airplay.MDNSAdvertiser = original
    if not calls:
        raise SystemExit("AirPlayProtocol._advertise never reached MDNSAdvertiser.advertise")
    service_type, name, port, props, server = calls[0]
    return service_type, name, port, props


# Deterministic inputs for the frozen corpus. Every one of these is a choice the
# *live* product would not make the same way twice — the USN is random per
# install, the friendly name is this machine's, and the address list is this
# machine's interfaces — so `--fixture` replaces all three with fixed values.
# 192.0.2.7 is TEST-NET-1 (RFC 5737): an address no interface on this planet will
# ever actually have, so the corpus cannot be a screenshot of the machine that
# generated it and mistake that for a protocol fact.
FIXTURE = {
    "name": "Macast-Fixture-1",
    "id": "00000000-0000-4000-8000-000000000007",
    "fn": "Macast(Fixture-1.local)",
    "address": "192.0.2.7",
    "port": 8009,
    "airplay_port": 7000,
    "deviceid": "02:00:00:00:00:07",
}


def build_info(port, name, props, fixture=False, service_type=CAST_SERVICE):
    from macast import discovery

    if fixture:
        addresses = [socket.inet_aton(FIXTURE["address"])]
        # Two different "what would drift" lists. Chromecast pins `id` (a USN
        # random per install) and `fn`; AirPlay pins `deviceid`, which the
        # reference draws from `random` **every process start** — so a phone that
        # sees this Mac twice in two launches is talking to two "different"
        # receivers. That is the reference's behaviour, and the corpus has to say
        # so with a fixed value rather than inherit one machine's draw.
        if service_type == AIRPLAY_SERVICE:
            # `name` is the same friendly name the instance is built from, so it
            # has to be pinned with the fixture name or the corpus would carry this
            # machine's identity in one record and the fixture's in another — and a
            # corpus that cannot be rebuilt from its own header is a screenshot.
            props = dict(props, deviceid=FIXTURE["deviceid"], name=FIXTURE["fn"])
            port = FIXTURE["airplay_port"]
        else:
            props = dict(props, id=FIXTURE["id"], fn=FIXTURE["fn"])
            port = FIXTURE["port"]
        name = FIXTURE["name"]
    else:
        addresses = discovery._local_addresses()
    safe = discovery.sanitize_instance_name(name, service_type)
    server = discovery._normalize_server(safe.split(".")[0])
    info = _z.ServiceInfo(
        service_type,
        safe,
        addresses=addresses,
        port=port,
        properties={k: v.encode("utf-8") for k, v in props.items()},
        server=server,
    )
    # The *effective* properties, not the ones the product handed us: in fixture
    # mode `id` and `fn` above were replaced, and a corpus header that names the
    # machine's real USN while its bytes carry the fixture's would make the file
    # lie about how to rebuild it. (The first version of this did exactly that.)
    return info, safe, server, addresses, props


def reference_datagram(info, override_ttl, order):
    """One datagram, built by the reference's own writers.

    Same `DNSOutgoing`, same record objects, same `add_answer_at_time` calls as
    `_add_broadcast_answer` (`_core.py:716-729`) — only the order of the
    address/NSEC pair is chosen here instead of falling out of a `set`. Sorting
    each side by `record.key` keeps a multi-address machine reproducible; the
    reference's own set order is not (see the corpus header).
    """
    from zeroconf._protocol.outgoing import DNSOutgoing
    from zeroconf import const

    head = [info.dns_pointer(override_ttl=override_ttl),
            info.dns_service(override_ttl=override_ttl),
            info.dns_text(override_ttl=override_ttl)]
    rest = list(info.get_address_and_nsec_records(override_ttl=override_ttl))
    nsec = sorted((r for r in rest if r.type == const._TYPE_NSEC), key=lambda r: r.key)
    addr = sorted((r for r in rest if r.type != const._TYPE_NSEC), key=lambda r: r.key)
    tail = (nsec + addr) if order == "nsec-first" else (addr + nsec)
    out = DNSOutgoing(const._FLAGS_QR_RESPONSE | const._FLAGS_AA)
    for record in head + tail:
        out.add_answer_at_time(record, 0)
    return out.packets()[0]


def reference_probe(info):
    """The uniqueness probe, transcribed from `generate_service_query`."""
    from zeroconf._protocol.outgoing import DNSOutgoing
    from zeroconf import const
    from zeroconf._dns import DNSQuestion

    out = DNSOutgoing(const._FLAGS_QR_QUERY | const._FLAGS_AA)
    out.add_question(DNSQuestion(info.type, const._TYPE_PTR,
                                 const._CLASS_IN | const._CLASS_UNIQUE))
    out.add_authorative_answer(info.dns_pointer())
    return out.packets()[0]


# --- the recorder -----------------------------------------------------------
CAPTURED = []
_LOCK = threading.Lock()
_T0 = time.monotonic()
# Which call produced this datagram. Without it the count "42 packets" cannot be
# read as "3 bursts x N sockets per burst", which is the cadence the port has to
# reproduce — the phase is the measurement, not a label on it.
_PHASE = ["register"]


def _record(log_debug, transport, packet, packet_num, out, addr, port,
            v6_flow_scope=()):
    """Stand in for `async_send_with_transport`: record, never send."""
    ipv6 = getattr(transport, "is_ipv6", False)
    if addr is None:
        real = "::1" if ipv6 else "224.0.0.251"
    else:
        real = addr
    try:
        sock_name = transport.sock_name
        fileno = transport.fileno
    except Exception:
        sock_name, fileno = None, None
    with _LOCK:
        CAPTURED.append({
            "t_ms": round((time.monotonic() - _T0) * 1000, 1),
            "dst": real,
            "port": port or 5353,
            "ipv6": ipv6,
            "sock": repr(sock_name),
            "fileno": fileno,
            "packet_num": packet_num,
            "phase": _PHASE[0],
            "bytes": bytes(packet),
        })


def decode(raw):
    """A second reader on purpose: our own walk of the wire, unmasked.

    `DNSIncoming` masks the cache-flush bit out of the class (`class_ & 0x7FFF`)
    — the very thing AGENTS.md §4.2 and `mdns.rs`'s red lines are about — so a
    dump through it alone cannot show *which* bit the reference put there.
    """
    if len(raw) < 12:
        return None
    tid, flags, qd, an, ns, ar = struct.unpack(">HHHHHH", raw[:12])
    rows = {"id": tid, "flags": flags, "counts": {"qd": qd, "an": an, "ns": ns, "ar": ar},
            "questions": [], "answers": [], "authority": [], "additional": []}
    cursor = 12

    def read_name(pos):
        """Walk a DNS name, following compression pointers the way RFC 1035 says.

        The rule that matters here, and that the first version of this function
        got wrong: a pointer advances the *record's* cursor by exactly two
        bytes, while the name itself continues at the target. Reading on from
        the target — which is what "keep the position where you landed" does —
        makes every following record start in the middle of the packet, and the
        walk then reports five PTRs for a packet that holds PTR/SRV/TXT/NSEC/A.
        A probe that misparses its own evidence is worse than no probe, so
        `cross_check` compares this reader against zeroconf's record by record.
        """
        parts = []
        end = None
        jumps = 0
        while pos < len(raw):
            length = raw[pos]
            if length == 0:
                pos += 1
                break
            if length & 0xC0 == 0xC0:
                if pos + 2 > len(raw):
                    break
                pointer = struct.unpack(">H", raw[pos:pos + 2])[0] & 0x3FFF
                if end is None:
                    end = pos + 2
                jumps += 1
                if jumps > 12:
                    break
                pos = pointer
                continue
            pos += 1
            if pos + length > len(raw):
                break
            parts.append(raw[pos:pos + length].decode("utf-8", "replace"))
            pos += length
        name = ".".join(parts)
        return (name + "." if name else "."), (end if end is not None else pos)

    def read_rdata(pos, n):
        return raw[pos:pos + n]

    def walk(count, bucket):
        nonlocal cursor
        for _ in range(count):
            name, cursor = read_name(cursor)
            if cursor + 10 > len(raw):
                bucket.append({"name": name, "truncated": True})
                return
            rtype, rclass, ttl, rdlen = struct.unpack(">HHIH", raw[cursor:cursor + 10])
            cursor += 10
            rdata = read_rdata(cursor, rdlen)
            cursor += rdlen
            row = {"name": name, "type": DNS_TYPE.get(rtype, str(rtype)),
                   "type_num": rtype, "class_hex": "0x%04X" % rclass,
                   "flush": bool(rclass & 0x8000), "class_num": rclass & 0x7FFF,
                   "ttl": ttl, "rdlength": rdlen}
            if rtype == 1 and rdlen == 4:
                row["rdata"] = socket.inet_ntoa(rdata)
            elif rtype == 33 and rdlen >= 8:
                pri, sec, port = struct.unpack(">HHH", rdata[:6])
                target, _ = read_name(cursor - rdlen + 6)
                row["rdata"] = "prio=%d weight=%d port=%d target=%s" % (pri, sec, port, target)
            elif rtype == 16:
                texts, pos = [], 0
                while pos < rdlen:
                    n = rdata[pos]
                    texts.append(rdata[pos + 1:pos + 1 + n].decode("utf-8", "replace"))
                    pos += 1 + n
                row["rdata"] = " | ".join(texts)
            elif rtype == 12:
                alias, _ = read_name(cursor - rdlen)
                row["rdata"] = alias
            elif rtype == 47:
                # NSEC = next owner name + type bit maps (RFC 4034 §4.1.1, read
                # the way zeroconf writes it). The port has to either reproduce
                # this record or write down why it does not, and neither is
                # possible while these bytes read back as nothing.
                base = cursor - rdlen
                next_name, after = read_name(base)
                pos = after
                types = []
                while pos + 2 <= cursor:
                    window = raw[pos]
                    span = raw[pos + 1]
                    bits = raw[pos + 2:pos + 2 + span]
                    for i in range(span * 8):
                        if bits[i // 8] & (0x80 >> (i % 8)):
                            types.append(window * 256 + i)
                    pos += 2 + span
                row["rdata"] = "next=%s types=%s" % (next_name, sorted(types))
            bucket.append(row)

    for _ in range(qd):
        name, cursor = read_name(cursor)
        if cursor + 4 > len(raw):
            rows["questions"].append({"name": name, "truncated": True})
            break
        qtype, qclass = struct.unpack(">HH", raw[cursor:cursor + 4])
        cursor += 4
        rows["questions"].append({"name": name, "type": DNS_TYPE.get(qtype, str(qtype)),
                                  "unicast_response": bool(qclass & 0x8000),
                                  "class_hex": "0x%04X" % qclass})
    walk(an, rows["answers"])
    walk(ns, rows["authority"])
    walk(ar, rows["additional"])
    return rows


def cross_check(raw, rows):
    """Two readers on the same bytes, compared record by record.

    `DNSIncoming` is the reference's own parser, and it masks the cache-flush
    bit out of `class_` (`_dns.py:121-122`) while keeping it in `.unique`. Our
    walk reads the wire field, so the comparison has to rebuild the wire class
    from those two halves — otherwise every record in this probe would say
    `class=0x0001` and the flush bit that `mdns.rs`'s red lines are about would
    be invisible in the very instrument built to look for it.
    """
    msg = DNSIncoming(raw)
    problems = []
    for label, theirs in (("qd", msg.num_questions), ("an", msg.num_answers),
                          ("ns", msg.num_authorities), ("ar", msg.num_additionals)):
        ours = rows["counts"][label]
        if theirs != ours:
            problems.append("%s: zeroconf %d, ours %d" % (label, theirs, ours))
    if len(msg.questions) != rows["counts"]["qd"]:
        problems.append("questions parsed: zeroconf %d, header %d"
                        % (len(msg.questions), rows["counts"]["qd"]))
    for q, ours in zip(msg.questions, rows["questions"]):
        wire = q.class_ | (0x8000 if q.unique else 0)
        if q.name != ours["name"] or "0x%04X" % wire != ours["class_hex"]:
            problems.append("question %s: zeroconf (%s, %s) ours (%s, %s)"
                            % (q.name, q.name, "0x%04X" % wire,
                               ours["name"], ours["class_hex"]))
    theirs = list(msg.answers())
    # `DNSIncoming.answers()` is every record it parsed, **not** the answer
    # section only: a probe packet keeps its suppression record in the authority
    # section (`_core.py:536` `add_authorative_answer`) and zeroconf still hands
    # it back here. So the record-by-record comparison runs over all three of our
    # sections in order — that is the same stream, and it is what the port has to
    # reproduce.
    ours_all = rows["answers"] + rows["authority"] + rows["additional"]
    if len(theirs) == len(ours_all):
        for r, ours in zip(theirs, ours_all):
            wire = r.class_ | (0x8000 if r.unique else 0)
            if (r.name, r.type, r.ttl, wire) != (ours["name"], ours["type_num"],
                                                 ours["ttl"], int(ours["class_hex"], 16)):
                problems.append("record %s: zeroconf (%s, type %s, ttl %s, class 0x%04X) "
                                "ours (%s, type %s, ttl %s, class %s)"
                                % (ours["name"], r.name, r.type, r.ttl, wire,
                                   ours["name"], ours["type_num"], ours["ttl"],
                                   ours["class_hex"]))
    else:
        problems.append("records: zeroconf parsed %d, ours %d"
                        % (len(theirs), len(ours_all)))
    return problems, {"zeroconf_records": len(theirs),
                      "zeroconf_answers": msg.num_answers,
                      "zeroconf_authority": msg.num_authorities,
                      "zeroconf_additional": msg.num_additionals}


# The name rules are the other half of what the port has to reproduce, and they
# are not in any packet header: `sanitize_instance_name` decides the instance
# name a sender browses for, and `_normalize_server` decides the SRV target the
# A record has to answer for. The table below is deliberately full of the shapes
# real friendly names take in this app — the DLNA friendly name is free text
# (`Macast(MyHost-1754.local)`, and `Macast(0123)` after a port fallback), and
# §4.2's rule that a name a sender cannot parse is a *silently invisible device*
# is exactly why these are frozen rather than reasoned about.
NAME_CASES = [
    "Macast",
    "Macast(Pavia-MacBookPro-1859.local)",
    "Macast(0123)",
    "Macast E2E Smoke",
    "Macast-MyHost-1754",
    "Macast(MyHost).local",
    "Macast._googlecast._tcp.local.",
    "Macast._googlecast._tcp.local",
    "Macast.子域._googlecast._tcp.local.",
    "中文名字",
    "",
    ".",
    "...",
    "-Macast-",
    "Macast 1",
    "Macast:8009",
    "Macast/host",
    "a" * 80,
    "a" * 63,
    "a" * 64,
    "Macast(" + "b" * 80 + ")",
    "Mäcast(local)",
    "Macast\t(tab)",
    "Macast(parens)(more)",
    "UPPERCASE-Macast",
    "Macast._GOOGLECAST._TCP.local.",
]


def name_corpus():
    from macast import discovery

    rows = []
    for raw in NAME_CASES:
        instance = discovery.sanitize_instance_name(raw, CAST_SERVICE)
        stem = instance.split(".")[0]
        rows.append((raw, instance, discovery._normalize_server(stem)))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, default=3.0,
                    help="how long to sit inside the event loop after registering "
                         "(the announcement burst is 3 x 225 ms, so 3 s covers it)")
    ap.add_argument("--port", type=int, default=None,
                    help="override the port the product asked for (default: its own)")
    ap.add_argument("--name", default=None,
                    help="instance name; default is a unique probe name so this "
                         "run cannot collide with a real Macast on the LAN")
    ap.add_argument("--dump", default=None, help="write the captured datagrams here")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--brief", action="store_true",
                    help="skip the per-packet listing; print only the derived shapes "
                         "(42 packets x 6 lines is not a reading, it is noise)")
    ap.add_argument("--no-register", action="store_true",
                    help="only build the packets (no event loop, no sockets at all)")
    ap.add_argument("--fixture", action="store_true",
                    help="deterministic inputs (implies --no-register) for the frozen corpus")
    ap.add_argument("--dump-hex", default=None,
                    help="write the fixture's announce/goodbye/probe bytes as one hex line "
                         "per shape, with the inputs in the header — the Rust writer is "
                         "compared against this file byte for byte")
    ap.add_argument("--protocol", choices=("cast", "airplay"), default="cast",
                    help="which receiver's announcement to measure; it changes the "
                         "service type, the property table the fixture pins, and the "
                         "port the corpus is written for")
    ap.add_argument("--dump-names", default=None,
                    help="write the friendly-name -> instance-name -> SRV-target table "
                         "the product's own two helpers produce, one JSON line per case")
    args = ap.parse_args()

    if args.dump_names:
        # JSON lines, not TSV: one of the cases carries a literal TAB, and a
        # corpus whose delimiter is also one of its inputs is a parser bug
        # waiting to be read as a product bug.
        with open(args.dump_names, "w", encoding="utf-8") as fh:
            fh.write("# frozen from macast/discovery.py: sanitize_instance_name + "
                     "_normalize_server\n")
            fh.write("# regenerate: env -u PYTHONPATH .venv/bin/python \\\n")
            fh.write("#   scripts/mdns_announce_probe.py --dump-names <this file>\n")
            for raw, instance, server in name_corpus():
                fh.write(json.dumps({"input": raw, "instance": instance,
                                     "server": server}, ensure_ascii=False) + "\n")
        print("wrote %d name cases to %s" % (len(NAME_CASES), args.dump_names))
        return 0

    if args.protocol == "airplay":
        want = AIRPLAY_SERVICE
        service_type, product_name, product_port, props = airplay_properties()
    else:
        want = CAST_SERVICE
        service_type, product_name, product_port, props = cast_properties()
    if service_type != want:
        # The product's own answer, not this file's constant, is what decides the
        # shape being measured: if `protocol_airplay.py` renames its service type,
        # a probe that kept going would freeze a corpus under a header that lies.
        print("the product advertises %r, this probe was asked about %r" % (service_type, want),
              file=sys.stderr)
        return 1

    name = args.name or product_name
    port = args.port or product_port
    if args.fixture:
        args.no_register = True
    info, safe, server, addresses, effective = build_info(
        port, name, props, fixture=args.fixture, service_type=service_type)

    out = {"service": service_type, "instance": safe, "srv_target": server,
           "addresses": [socket.inet_ntoa(a) for a in addresses],
           "txt": effective, "packets": []}

    # --- shapes without any socket at all (also what --no-register gives) ----
    # Same construction as `_new_zeroconf`: `IPVersion.V4Only` with the default
    # interface choice. Restricting `interfaces=` to loopback here would answer
    # "how many destinations does one announcement produce" with 1 instead of
    # the number of IPv4 interfaces this machine has, and that number is one of
    # the things we came to measure. Receive-only in this mode: sends are
    # patched before anything is registered.
    zc = None if args.no_register else _z.Zeroconf(ip_version=_z.IPVersion.V4Only)
    if args.no_register:
        # `generate_service_broadcast` is a method, but it only touches `self`
        # to read nothing at all — call it through the class with a bare stub so
        # this mode really opens no socket.
        class _Stub:
            generate_service_broadcast = _zc_core.Zeroconf.generate_service_broadcast
            generate_service_query = _zc_core.Zeroconf.generate_service_query
            # `no-self-use` in the real class: it only reads its arguments, so
            # borrowing it costs nothing and keeps this mode socket-free.
            _add_broadcast_answer = _zc_core.Zeroconf._add_broadcast_answer
        for label, produced in (
            ("broadcast", _Stub().generate_service_broadcast(info, None, True)),
            ("probe", _Stub().generate_service_query(info)),
            ("goodbye", _Stub().generate_service_broadcast(info, 0, True)),
        ):
            for i, pkt in enumerate(produced.packets()):
                out["packets"].append({"t_ms": None, "mode": label, "packet_num": i,
                                       "dst": "(direct generator)", "port": 5353,
                                       "bytes": pkt})
    else:
        _zc_core.async_send_with_transport = _record
        _PHASE[0] = "register"
        try:
            zc.register_service(info, cooperating_responders=True)
        except Exception as e:
            print("register failed: %r" % (e,), file=sys.stderr)
            return 1
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            time.sleep(0.05)
        _PHASE[0] = "update"
        try:
            zc.update_service(info)          # the re-announce the keepalive thread drives
        except Exception as e:
            print("update_service failed: %r" % (e,), file=sys.stderr)
        deadline = time.monotonic() + 0.8
        while time.monotonic() < deadline:
            time.sleep(0.05)
        _PHASE[0] = "goodbye"
        zc.unregister_service(info)          # goodbye
        # Do not close on top of the goodbye: the sync wrapper schedules the
        # burst and returns, so closing here would cut it off and this file
        # would report "one burst" as though that were the reference's cadence.
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline:
            time.sleep(0.05)
        _PHASE[0] = "closed"
        zc.close()
        with _LOCK:
            for row in CAPTURED:
                out["packets"].append(dict(row, mode="live:" + row["phase"]))

    # --- report ------------------------------------------------------------
    problems_all = []
    for row in out["packets"]:
        raw = row["bytes"]
        row["len"] = len(raw)
        rows = decode(raw)
        row["decoded"] = rows
        if rows is None:
            problems_all.append("undecodable packet")
            continue
        problems, counts = cross_check(raw, rows)
        row["cross"] = counts
        problems_all += ["packet @%s: %s" % (row.get("t_ms"), p) for p in problems]

    if args.dump_hex:
        if not args.fixture:
            print("--dump-hex is only honest with --fixture: a corpus of this "
                  "machine's USN, name and addresses is a screenshot, not a format",
                  file=sys.stderr)
            return 1
        with open(args.dump_hex, "w", encoding="utf-8") as fh:
            fh.write("# frozen from the reference: zeroconf %s, CPython %s\n"
                     % (_z.__version__, sys.version.split()[0]))
            fh.write("# regenerate: env -u PYTHONPATH .venv/bin/python \\\n")
            fh.write("#   scripts/mdns_announce_probe.py --protocol %s --fixture --dump-hex <this file>\n"
                     % args.protocol)
            fh.write("# inputs: service=%s instance=%s server=%s port=%d address=%s\n"
                     % (service_type, safe, server, info.port, FIXTURE["address"]))
            fh.write("# txt (order as written on the wire): %s\n"
                     % ";".join("%s=%s" % kv for kv in out["txt"].items()))
            fh.write("#\n")
            fh.write("# TWO announce lines, not one, because the reference does not have an\n")
            fh.write("# order for that pair. `_get_address_and_nsec_records` returns a `set`, and\n")
            fh.write("# measured on this build: PYTHONHASHSEED 0/1/2 emit `NSEC, A` while\n")
            fh.write("# PYTHONHASHSEED 12345 emits `A, NSEC`. So a writer that matches either one\n")
            fh.write("# is faithful, and a corpus that pinned one would be pinning a hash seed.\n")
            fh.write("# PTR, SRV and TXT *are* a fixed order (added explicitly in\n")
            fh.write("# `_add_broadcast_answer`), and they are byte-identical in all four lines.\n")
            for ttl_label, ttl in (("announce", None), ("goodbye", 0)):
                for order in ("nsec-first", "a-first"):
                    fh.write("%s-%s %s\n" % (ttl_label, order,
                                             reference_datagram(info, ttl, order).hex()))
            probe = reference_probe(info)
            fh.write("probe %s\n" % probe.hex())
        out["dumped"] = args.dump_hex

    if args.dump:
        with open(args.dump, "wb") as fh:
            for row in out["packets"]:
                fh.write(struct.pack(">IH", len(row["bytes"]), row.get("packet_num", 0)))
                fh.write(row["bytes"])
        out["dumped"] = args.dump

    if args.json:
        print(json.dumps({k: v for k, v in out.items() if k != "packets"}, indent=2))
        for row in out["packets"]:
            print(json.dumps({**{k: v for k, v in row.items() if k != "bytes"},
                              "hex": row["bytes"].hex()}))
    else:
        for k in ("service", "instance", "srv_target", "addresses", "dumped"):
            if k in out:
                print("%-11s %s" % (k, out[k]))
        print("txt         %s" % ", ".join("%s=%s" % kv for kv in out["txt"].items()))
        print()
        if not args.brief:
            print("%-8s %-7s %-16s %-6s %s" % ("t_ms", "mode", "dst", "len", "sections"))
        for row in [] if args.brief else out["packets"]:
            dec = row["decoded"] or {}
            counts = dec.get("counts", {})
            print("%-8s %-7s %-16s %-6d flags=0x%04X qd=%d an=%d ns=%d ar=%d" % (
                row.get("t_ms"), row["mode"], "%s:%s" % (row["dst"], row["port"]),
                row["len"], dec.get("flags", 0), counts.get("qd", 0), counts.get("an", 0),
                counts.get("ns", 0), counts.get("ar", 0)))
            for section in ("questions", "answers", "authority", "additional"):
                for rec in dec.get(section, []):
                    extra = " ttl=%s" % rec["ttl"] if "ttl" in rec else ""
                    cls = " class=%s%s" % (rec.get("class_hex"),
                                           " FLUSH" if rec.get("flush") else "") \
                        if "class_hex" in rec else ""
                    print("           %-10s %-4s %-46s%s%s  %s" % (
                        section[:-4], rec.get("type", "?"), rec.get("name", "")[:46],
                        cls, extra, str(rec.get("rdata", rec.get("unicast_response", "")))[:70]))
        print()
        deltas = [b["t_ms"] - a["t_ms"] for a, b in zip(out["packets"], out["packets"][1:])
                  if a.get("t_ms") is not None and b.get("t_ms") is not None]
        if deltas:
            print("inter-packet gaps (ms): %s" % ", ".join("%.0f" % d for d in deltas))

        # The part the port actually consumes, derived from the captured bytes
        # rather than written as this file's prose: one line per shape, with the
        # record order, the TTL of each record, and which ones carry the
        # cache-flush bit.
        print()
        print("== the shapes, derived off the bytes ==")
        by_mode = {}
        for row in out["packets"]:
            by_mode.setdefault(row["mode"], []).append(row)
        for mode, rows in by_mode.items():
            dec = rows[0]["decoded"] or {}
            answers = dec.get("answers", [])
            print("%-9s x%-3d size=%-5d flags=0x%04X an=%d ns=%d ar=%d qd=%d" % (
                mode, len(rows), rows[0]["len"], dec.get("flags", 0),
                len(answers), len(dec.get("authority", [])),
                len(dec.get("additional", [])), len(dec.get("questions", []))))
            if answers:
                print("            order   %s" % [r["type"] for r in answers])
                print("            ttl     %s" % [r["ttl"] for r in answers])
                print("            flush   %s" % [r["flush"] for r in answers])
                print("            classes %s" % [r["class_hex"] for r in answers])
            for q in dec.get("questions", []):
                print("            question %s type=%s class=%s QU=%s" % (
                    q["name"], q["type"], q["class_hex"], q.get("unicast_response")))
            for r in dec.get("authority", []) + dec.get("additional", []):
                print("            other    %s type=%s ttl=%s class=%s" % (
                    r["name"], r["type"], r["ttl"], r["class_hex"]))
            srv = next((r for r in answers if r["type"] == "SRV"), None)
            if srv:
                print("            srv      %s" % srv["rdata"])
            txt = next((r for r in answers if r["type"] == "TXT"), None)
            if txt:
                print("            txt      %s" % txt["rdata"])
            if mode.startswith("live"):
                stamps = sorted({r["t_ms"] for r in rows})
                bursts = 0
                last = None
                for t in stamps:
                    if last is None or t - last > 30:
                        bursts += 1
                    last = t
                dests = sorted({"%s:%s%s" % (r["dst"], r["port"], " (v6)" if r["ipv6"] else "")
                                for r in rows})
                print("            bursts=%d  sockets=%d  packets/burst=%s" % (
                    bursts, len({r["sock"] for r in rows}),
                    sorted({len([x for x in rows if x["t_ms"] == t]) for t in stamps})))
                gaps = [round(b - a, 0) for a, b in zip(stamps, stamps[1:])]
                print("            burst t_ms=%s gaps=%s" % (
                    [round(t - stamps[0]) for t in stamps], gaps))
                print("            destinations: %s" % ", ".join(dests))
                print("            source sockets: %s" % sorted({r["sock"] for r in rows}))
                # One burst, many sockets: are the datagrams the same bytes?
                first_t = stamps[0]
                group = [r["bytes"] for r in rows if r["t_ms"] == first_t]
                print("            first burst byte-identical across sockets: %s"
                      % all(g == group[0] for g in group))

        if problems_all:
            print("DECODE DISAGREEMENTS:")
            for p in problems_all:
                print("  %s" % p)
        else:
            print("both readers agree on every packet (%d captured)" % len(out["packets"]))
    return 0 if out["packets"] and not problems_all else 1


if __name__ == "__main__":
    sys.exit(main())
