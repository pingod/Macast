#!/usr/bin/env python3
# Cross-process half of `mdns_reuseport_probe.py`.
#
# That probe measured multicast duplication among sockets **in one process** and
# concluded a second joined reader costs the first nothing. The app said otherwise:
# `macast-tauri` holds 5353, joined, reading, answering on loopback — and receives
# nothing from a remote querier while a python listener on the same Mac receives the
# same queries. The difference between those two observations is not the protocol, it
# is the **process boundary**, and nothing had ever measured it.
#
# Re-run 2026-10-18 with a query built by this file (labels length-prefixed, 40
# bytes): the listener half still holds — a python process here receives a remote
# host's `PTR _googlecast._tcp`. The app half still does not answer, and the two
# earlier "the app is deaf to inbound LAN multicast" readings were partly my
# instruments' fault (a hand-built 39-byte query missing the first label's length
# byte, and an answer filter that searched a *dotted* name, which never appears on
# an mDNS wire). So this probe's honest scope is: **what one process of mine
# receives**, per transaction id. Whether a *specific* app binary is subscribed to
# the same flow needs its own log sentence — see `announce.rs`'s two INFO lines and
# `docs/Rust-Port-Design.md` §14.6.
#
# Usage:
#   listener:  python3 scripts/mdns_xproc_probe.py listen <lan-ip> <seconds> <id>
#   sender:    python3 scripts/mdns_xproc_probe.py send <lan-ip> <packets>
import socket
import struct
import sys
import time

GROUP = "224.0.0.251"
PORT = 5353
TYPE = "_googlecast._tcp.local."


def wire_name(name):
    out = b""
    for label in name.strip(".").split("."):
        out += bytes([len(label)]) + label.encode()
    return out + b"\x00"


def query(txn):
    return (struct.pack("!HHHHHH", txn, 0, 1, 0, 0, 0)
            + wire_name(TYPE) + struct.pack("!HH", 12, 1))


def bound_socket(lan, join=True):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    s.bind(("", PORT))
    if join:
        s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP,
                     socket.inet_aton(GROUP) + socket.inet_aton(lan))
    s.settimeout(0.3)
    return s


def listen(lan, seconds, tag):
    s = bound_socket(lan)
    ours = set()
    other = 0
    start = time.time()
    while time.time() - start < seconds:
        try:
            data, frm = s.recvfrom(4096)
        except socket.timeout:
            continue
        if len(data) < 12:
            continue
        txn = struct.unpack(">H", data[:2])[0]
        flags = struct.unpack(">H", data[2:4])[0]
        # Ours are the queries (QR clear) whose id is in the band `send` picks.
        # Everything else on this group is somebody's traffic and is only counted,
        # because "ambient > 0" is the proof the listener is wired at all.
        if (flags & 0x8000) == 0 and 0xB0 <= txn <= 0xFF:
            ours.add(txn)
        else:
            other += 1
    print("listen %s: our-queries=%d ambient=%d ids=%s"
          % (tag, len(ours), other, sorted(ours)), flush=True)


def send(lan, packets):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.bind(("", 0))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(lan))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 255)
    sent = []
    for index in range(packets):
        txn = 0xB0 + index
        s.sendto(query(txn), (GROUP, PORT))
        sent.append(txn)
        time.sleep(0.05)
    print("send: %d datagrams ids=%s" % (len(sent), sent[:3] + ["..."] + sent[-1:]),
          flush=True)


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "listen":
        listen(sys.argv[2], float(sys.argv[3]), sys.argv[4])
    else:
        send(sys.argv[2], int(sys.argv[3]))
