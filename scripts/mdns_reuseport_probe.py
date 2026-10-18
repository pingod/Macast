#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# One question, and the Rust port's mDNS responder is shaped by the answer:
#
#   When several sockets are bound to UDP/5353 at the same time, how many of
#   them see **one** incoming datagram?
#
# This is not a style question. Two sentences in the port currently rest on it:
#
# * `announce.rs`'s socket is bound to 5353 and **deliberately does not join**
#   the group, because (its module header says) `SO_REUSEPORT` "distributes
#   each incoming datagram to one of the bound sockets", so a second joined
#   reader would "eat half of the device list" our browser is trying to hear.
# * `docs/Rust-Port-Design.md` §8 and §16 turn that into a plan: when the
#   responder lands, it must **merge** the two readers into one socket instead
#   of opening a third.
#
# Both sentences are about the kernel, and neither had ever been asked of this
# machine. That is the shape AGENTS.md §4.13 keeps calling a number nobody
# queried: a claim that reads like a measurement, copied forward from a design
# note. If the kernel actually **duplicates** multicast to every joined socket,
# then the responder may join on its own socket, and — worse — the *current*
# code's unjoined announcer would already be a receiver stealing datagrams from
# the browser, which is the one thing that comment claims it cannot do.
#
# So this probe measures four shapes and prints counts, without opinions:
#
#   multicast, 1 joined socket                      (the control)
#   multicast, 2 joined + 1 unjoined                (browser + responder + announcer)
#   multicast, 3 joined                             (does it scale, or is 2 special)
#   unicast  to 127.0.0.1:5353, 2 joined + 1 unjoined
#
# Nothing leaves this machine. Multicast is forced through `lo0` with
# `IP_MULTICAST_IF = 127.0.0.1` and TTL 1, and the unicast case is to loopback,
# so no datagram reaches the LAN and no real sender or neighbour can see any of
# it. A real mDNS announcement on this LAN would put a second Chromecast in the
# phone's menu for the length of the run — the cost `mdns_announce_probe.py`
# avoids by patching zeroconf's one send site, and the reason both probes are
# written to stay local.
#
# The payload is a **real mDNS PTR query** for `_googlecast._tcp.local.`,
# encoded here rather than imported from the Rust side so this file can run
# before the answer it measures exists. It is a valid datagram on purpose:
# the question is "how does the kernel dispatch", and an unparseable payload
# cannot change that answer, while a realistic one means a future run can be
# compared against a device's actual query.
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_reuseport_probe.py
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_reuseport_probe.py --json
#   env -u PYTHONPATH .venv/bin/python scripts/mdns_reuseport_probe.py --packets 40
#
# Exit code: 0 = every case produced a count (even a count of 0, which is an
# answer); 2 = environment problem (cannot bind 5353, no loopback multicast
# route), which must never be read as "the kernel delivers nothing".

import argparse
import json
import socket
import struct
import subprocess
import sys
import time

MDNS_GROUP = "224.0.0.251"
MDNS_PORT = 5353
SERVICE_TYPE = "_googlecast._tcp.local."

# IP_ADD_MEMBERSHIP / IP_MULTICAST_IF / IP_MULTICAST_TTL / IP_MULTICAST_LOOP
# are in the `socket` module on every platform we ship on; the four-option
# set-and-bind order below is deliberately the same one `macast-cast/src/mdns.rs`
# documents for the browser (reuse options before bind), because a probe that
# binds differently than the product measures a different question.
IP_ADD_MEMBERSHIP = socket.IP_ADD_MEMBERSHIP
try:
    SO_REUSEPORT = socket.SO_REUSEPORT
except AttributeError:  # pragma: no cover - macOS/Windows both have it
    print("SO_REUSEPORT is not exposed on this platform", file=sys.stderr)
    sys.exit(2)


def ptr_query(txn_id: int = 0, name: str = SERVICE_TYPE) -> bytes:
    """A one-question mDNS PTR query with a caller-chosen transaction id.

    The id varies across the datagrams one case sends — that is what makes
    "duplicated" a byte-level reading instead of an inference from two equal
    counts. With an identical payload twelve times over, a kernel that split the
    traffic between two sockets could still hand each of them twelve datagrams
    (six real ones plus six it did not), and the counts alone would not show it.
    Header: flags 0x0000 (query), one question, no records; the `unicast`
    response bit is **not** set, so a responder answering this would answer it
    to the group, which is the traffic we are counting.
    """
    labels = b"".join(
        bytes([len(part)]) + part.encode("ascii") for part in name.split(".") if part
    ) + b"\x00"
    question = labels + struct.pack("!HH", 12, 1)  # TYPE_PTR, CLASS_IN
    header = struct.pack("!HHHHHH", txn_id & 0xFFFF, 0x0001, 0, 0, 0, 0)
    return header + question


def receiver(joined: bool, log: list, bind: str = "") -> socket.socket:
    """One socket bound to `bind`:5353, optionally joined on loopback.

    `bind` is a parameter because the unicast question has two shapes and they
    are not the same kernel path: a wildcard bind competes with every other
    socket on the port (including the OS responder), while a bind to
    `127.0.0.1` is a *more specific* match and wins the route lookup outright.
    Measuring only the first would let "0 received" be read as "unicast is
    distributed" when it can also mean "someone else on this machine is a better
    match for port 5353".
    """
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.setsockopt(socket.SOL_SOCKET, SO_REUSEPORT, 1)
    except OSError as exc:
        log.append("SO_REUSEPORT refused: %s" % exc)
    sock.bind((bind, MDNS_PORT))
    if joined:
        # Join on 127.0.0.1 so the membership lives on lo0: the group is real,
        # the interface is loopback, nothing reaches the LAN.
        mreq = socket.inet_aton(MDNS_GROUP) + socket.inet_aton("127.0.0.1")
        sock.setsockopt(socket.IPPROTO_IP, IP_ADD_MEMBERSHIP, mreq)
    sock.setblocking(False)
    return sock


def sender(log: list):
    """A socket whose multicast egress is pinned to loopback."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.bind(("127.0.0.1", 0))
    # IP_MULTICAST_IF takes the interface address: pinning it to 127.0.0.1 puts
    # the egress on lo0, so the group is reached without ever reaching the LAN.
    if hasattr(socket, "IP_MULTICAST_IF"):
        try:
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton("127.0.0.1"))
        except OSError as exc:
            log.append("IP_MULTICAST_IF refused: %s" % exc)
    else:  # pragma: no cover
        log.append("IP_MULTICAST_IF is not exposed; cannot pin egress to lo0")
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
    return sock


def drain(socks, deadline):
    """Count datagrams per socket until `deadline`, byte-identical payloads counted once."""
    counts = [0 for _ in socks]
    digests = [set() for _ in socks]
    while time.monotonic() < deadline:
        for index, sock in enumerate(socks):
            while True:
                try:
                    data, peer = sock.recvfrom(65535)
                except BlockingIOError:
                    break
                except OSError as exc:
                    if exc.errno in (11, 35):  # EAGAIN / EWOULDBLOCK on Linux / macOS
                        break
                    raise
                counts[index] += 1
                digests[index].add((bytes(data), peer[0]))
        time.sleep(0.002)
    return counts, digests


def run_case(title, receivers, dst, packets, gap, log):
    """Bind `receivers`, fire `packets` datagrams at `dst`, count what each saw.

    `receivers` is a list of `(joined, bind)` so one case can mix a wildcard
    socket with a loopback-pinned one — the shape the field notes below need.
    """
    socks = [receiver(joined, log, bind) for joined, bind in receivers]
    src = sender(log)
    peers = {"mcast": (MDNS_GROUP, MDNS_PORT), "ucast": ("127.0.0.1", MDNS_PORT)}[dst]
    sent_ids = set()
    sent = 0
    try:
        for index in range(packets):
            # One id per datagram: see `ptr_query`.
            payload = ptr_query(index + 1)
            sent_ids.add(payload[1:3])
            try:
                src.sendto(payload, peers)
                sent += 1
            except OSError as exc:
                log.append("send refused: %s" % exc)
                break
            time.sleep(gap)
        # Give the queue a moment to be walked: the count is about dispatch, not
        # about our own scheduling, so an under-read would look like "not
        # delivered" and that is the exact confusion this probe exists to avoid.
        counts, digests = drain(socks, time.monotonic() + 0.5)
    finally:
        for sock in socks:
            sock.close()
        src.close()
    # Each socket's datagrams split into **ours** and **not ours**. This is the
    # whole measurement: on a machine with a live mDNS stack the raw queue holds
    # other people's traffic too, and counting it reads as "received more than we
    # sent" — which is exactly what the first Linux run did (14 of 12), and which
    # a reader could otherwise file as "the sockets share a queue". The verdict
    # below is computed from `matched` only; `foreign` is printed so a future run
    # can see how noisy the machine was rather than being quietly fooled by it.
    ids = [{data[1:3] for data, _ in d} for d in digests]
    matched = [len(group & sent_ids) for group in ids]
    heard = [group for group, n in zip(ids, matched) if n]
    return {
        "case": title,
        "sockets": [
            "%s/%s" % ("joined" if joined else "unjoined", bind or "wildcard")
            for joined, bind in receivers
        ],
        "sent": sent,
        "received": counts,
        "matched_ours": matched,
        "foreign": [len(group - sent_ids) for group in ids],
        "identical_payloads": bool(heard) and len({frozenset(g & sent_ids) for g in heard}) == 1,
        "every_socket_got_every_datagram": bool(matched) and all(n == sent for n in matched),
        "note": None,
    }


def neighbours(log):
    """Name the other processes already bound to UDP/5353 on this machine.

    Not decoration: "0 received" has two very different causes — the kernel
    split the traffic, or somebody else's socket is a better match for the port.
    macOS runs `mDNSResponder` on 5353 permanently, so without this line the
    unicast numbers below are unreadable.
    """
    try:
        out = subprocess.run(
            ["lsof", "-nP", "-iUDP:5353"], capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError) as exc:
        log.append("cannot list the other 5353 sockets: %s" % exc)
        return []
    rows = []
    for line in out.stdout.splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 9:
            rows.append("%s pid %s on %s" % (fields[0], fields[1], fields[8]))
    return sorted(set(rows))


CASES = [
    ("multicast, 1 joined (control)", [(True, "")], "mcast"),
    ("multicast, 2 joined + 1 unjoined", [(True, ""), (True, ""), (False, "")], "mcast"),
    ("multicast, 3 joined", [(True, ""), (True, ""), (True, "")], "mcast"),
    ("unicast, 1 joined (control)", [(True, "")], "ucast"),
    ("unicast, 2 joined + 1 unjoined", [(True, ""), (True, ""), (False, "")], "ucast"),
    ("unicast, 1 on 127.0.0.1 + 1 wildcard", [(True, "127.0.0.1"), (True, "")], "ucast"),
]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="how many sockets on UDP/5353 see one incoming datagram"
    )
    parser.add_argument("--packets", type=int, default=20, help="datagrams per case")
    parser.add_argument("--gap", type=float, default=0.02, help="seconds between datagrams")
    parser.add_argument(
        "--repeat",
        type=int,
        default=3,
        help="passes over the whole case table; unicast's winner moves between passes, "
        "so one pass is not the rule",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args()

    log = []
    others = neighbours(log)

    # Each case is (title, [(joined, bind), ...], destination). A control exists
    # on **both** delivery shapes, because a case whose single-socket reading is
    # 0 cannot tell "the kernel does not deliver this shape to us" apart from
    # "the second socket stole it" — and only the second one is a design input.
    passes = []
    for _ in range(max(1, args.repeat)):
        results = []
        for title, receivers, dst in CASES:
            try:
                results.append(run_case(title, receivers, dst, args.packets, args.gap, log))
            except OSError as exc:
                results.append({"case": title, "error": str(exc), "note": "environment"})
        verdicts(results)
        passes.append(results)

    if args.json:
        print(
            json.dumps(
                {
                    "sys": sys.platform,
                    "other_5353_sockets": others,
                    "passes": passes,
                    "log": log,
                },
                indent=2,
            )
        )
        return 0

    print(
        "platform %s, %d datagrams per case, %d ms apart, %d pass(es)"
        % (sys.platform, args.packets, int(args.gap * 1000), len(passes))
    )
    print("egress pinned to lo0 (IP_MULTICAST_IF=127.0.0.1, TTL 1) — nothing on the LAN")
    print("who else holds UDP/5353 here: %s" % (", ".join(others) if others else "nobody named"))
    width = max(len(title) for title, _, _ in CASES)
    for pass_number, results in enumerate(passes, start=1):
        print("\npass %d — of our datagrams per socket" % pass_number)
        for result in results:
            if result.get("error"):
                print("  %-*s ERROR %s" % (width, result["case"], result["error"]))
                continue
            print(
                "  %-*s matched=%-18s foreign=%-12s of %-3s [%s]"
                % (
                    width,
                    result["case"],
                    result["matched_ours"],
                    result["foreign"],
                    result["sent"],
                    result["note"],
                )
            )
    if log:
        print("\nsocket-layer notes:")
        for line in log:
            print("  " + line)
    # A cross-pass reading, because the point of repeating is exactly this: if the
    # same case hands the stream to a different socket next time, no single pass
    # may be quoted as "the kernel's rule".
    moved = set()
    for pass_number, results in enumerate(passes, start=1):
        for result in results:
            if result.get("error") or "one socket" not in (result["note"] or ""):
                continue
            winner = result["note"].split("route went to ")[1].split(" (")[0]
            moved.add((result["case"], winner))
    if len({case for case, _ in moved}) and len(moved) > len({case for case, _ in moved}):
        print("\nthe unicast winner MOVED between passes on these cases:")
        for case, winner in sorted(moved):
            print("  %-*s -> %s" % (width, case, winner))
    print("\nreading: 'duplicated' means a second joined reader costs the first nothing;")
    print("'one socket got all and another none' means the kernel handed the stream to a")
    print("single reader — which one is not a rule if it moved between passes above.")
    print("Which of the two the kernel does is what announce.rs's header and")
    print("Rust-Port-Design.md §8/§16 claimed without having asked, and the two")
    print("answers differ for the responder's shape: duplicated says a responder may")
    print("hold its own joined socket, single-reader says some datagrams we expect")
    print("are already being taken by somebody. Read the multicast rows and the")
    print("unicast rows **separately** — a query from a phone is multicast, a reply")
    print("to a query we sent is often unicast — and never infer one shape from the")
    print("other.")
    return 0


def verdicts(results) -> None:
    """Attach the per-socket-kind reading to one pass.

    The two questions a reader of this table has are different: "does a second
    joined reader cost the first anything" (the responder's shape) and "does an
    unjoined socket receive at all" (whether the announcer we already ship
    competes with the browser).
    """
    for result in results:
        if result.get("error"):
            continue
        kinds = result["sockets"]
        sent = result["sent"]
        joined = [n for kind, n in zip(kinds, result["matched_ours"]) if kind.startswith("joined")]
        unjoined = [
            n for kind, n in zip(kinds, result["matched_ours"]) if kind.startswith("unjoined")
        ]
        says = []
        if not joined or max(joined) == 0:
            says.append("no joined socket received one of OURS: this case asks nothing (see the control)")
        elif all(n == sent for n in joined):
            says.append("every joined socket got every one of our %d (duplicated)" % sent)
        elif any(n == 0 for n in joined):
            winners = [
                kind
                for kind, n in zip(kinds, result["matched_ours"])
                if kind.startswith("joined") and n == sent
            ]
            says.append(
                "one socket got all %d and another got none — the route went to %s "
                "(a single reader takes the whole stream; it does not share it)"
                % (sent, ", ".join(winners) if winners else "a socket outside this probe")
            )
        else:
            says.append(
                "joined sockets hold different subsets of ours (%s of %s): the traffic was divided"
                % (joined, sent)
            )
        if unjoined:
            if all(n == 0 for n in unjoined):
                says.append("and the unjoined socket(s) got none of ours")
            else:
                says.append("BUT an unjoined socket received %s of ours — it does compete" % unjoined)
        result["note"] = "; ".join(says)


if __name__ == "__main__":
    sys.exit(main())
