#!/usr/bin/env python3
"""Who receives a **unicast** mDNS datagram addressed at this host: a wildcard bind, or a
socket bound to that address?

Why this probe exists (design doc §16 第 40 条). §16 第 13 条 left a conclusion on the table —
"a unicast datagram to port 5353 goes to exactly one socket in the reuse group, and which
one varies per flow" — and from it task #40 was written as *"merge the browser's socket into
the engine's, so the coin flip stops eating answers"*. This probe went to test that shape and
found something else, on the first run:

    wildcard bound, unicast -> 192.168.1.5:5353  : 0 of 8
    exact-address bound, same stream              : 6 of 6

A socket bound to `0.0.0.0:5353` — which is what **both** of this port's readers are
(`mdns::open` and `announce::bind_engine`) — receives **none** of it. The merge was going to
choose which one of two sockets that both receive nothing gets the traffic. The question was
never "ours or theirs"; it is "wildcard or per-address", and on this machine the answer is
the second one.

What that costs, precisely — no more and no less: today `mdns::query` sends its PTR
question with class `IN` and **no** `QU` bit, so devices answer our browser with
multicast and the device list is not what this breaks. What is broken is the other
direction: **a querier that asks us directly, unicast, is talking to a socket that cannot
hear it** — and the first branch we take toward politeness or robustness on the query side
(asking with `QU`, which is what a querier with a real unicast socket does) would open the
same hole under the answers.

That is also exactly the shape the reference uses, read here from the wheel in this
repository's `.venv` rather than from the RFC:

* `new_listen_socket` (`_utils/net.py:455-461`) — *one* shared socket, `bind_addr=("",)`,
  and its docstring says what it is for: "**used to receive multicast queries**".
* `new_respond_socket` (`_utils/net.py:415-455`) — **one per interface**, and the comment
  block says "For response sockets: **Bind explicitly to the interface address**", with
  `port=0 if unicast else _MDNS_PORT`.

So the reference holds a per-address socket in addition to the wildcard listener, and this
port holds only wildcard listeners. What that costs, in shipped sentences: a device that
answers our PTR query with a **unicast** reply (RFC 6762 §5.4 `QU`) has that reply dropped
before either of our sockets sees it — `query=cast-targets` then reports "no devices found"
on a LAN that is full of them; and a querier that asks us directly, unicast, gets no answer
from a responder that cannot hear the question.

Three controls, so a zero is never read as "the product is deaf":

1. an **exact-address** socket bound the same way must receive the whole stream. If *it*
   receives nothing, this machine is not delivering unicast to port 5353 at all and the
   comparison below says nothing about binding.
2. a **multicast** send must reach the wildcard socket (that is `mdns_reuseport_probe.py`'s
   reading, re-checked here because everything below depends on it still holding).
3. everything is sent to **127.0.0.1** or to this host's own address from this host, and the
   group send is pinned to loopback with `IP_MULTICAST_IF=127.0.0.1` + TTL 1: **no byte of
   this probe reaches the LAN**, because a real announcement here would make this machine
   look like a second Chromecast for the length of the run (AGENTS.md §10 — do not make the
   user's network observe the test).

Usage:
    .venv/bin/python scripts/mdns_unicast_bind_probe.py [rounds] [iface-address]
"""

from __future__ import annotations

import os
import socket
import struct
import sys
import time

MDNS_GROUP = "224.0.0.251"
MDNS_PORT = 5353
# A wire-shaped response: QR=1, one PTR answer pointing at its own name. The bytes only
# have to *look* like an answer — the probe measures delivery, not parsing — but a
# well-formed datagram is what a real responder would hand back, so a stack that filters
# by shape before queueing is not accidentally being tested with junk.
WIRE = (
    b"\x00\x00\x84\x00\x00\x00\x00\x01\x00\x00\x00\x00"
    b"\x0b_googlecast\x04_tcp\x05_local\x00"
    b"\x00\x0c\x00\x01\x00\x00\x00\x78\x00\x02\xc0\x0c"
)


def mdns_socket(bind_to: str, join_on: str | None = None) -> socket.socket:
    """One socket, bound the way one of the two shipped readers binds it.

    `bind_to` is `""` for the shape `mdns::open` / `announce::bind_engine` use and an
    address for the shape `new_respond_socket` uses. `REUSEADDR` + `REUSEPORT` before
    `bind`, as both of them do (`macast-cast/src/mdns.rs:1546-1570`) — without those, the
    system's own `mDNSResponder` would simply refuse the bind and every row below would
    read "cannot listen", not "someone else got the packet".
    """
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    sock.bind((bind_to, MDNS_PORT))
    if join_on:
        mreq = struct.pack("4s4s", socket.inet_aton(MDNS_GROUP), socket.inet_aton(join_on))
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
    sock.setblocking(False)
    return sock


def send_many(target: str, count: int, group: bool = False, via: str | None = None) -> None:
    peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    peer.bind(("", 0))
    if group:
        # Loopback-pinned, TTL 1: the same argument as control 3 in the module docstring.
        peer.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, struct.pack("b", 1))
        peer.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(via or "127.0.0.1"))
    for _ in range(count):
        peer.sendto(WIRE, (target, MDNS_PORT))
        time.sleep(0.02)
    peer.close()
    time.sleep(0.4)


def drain(sock: socket.socket) -> int:
    got = 0
    while True:
        try:
            if sock.recv(65535):
                got += 1
        except (BlockingIOError, OSError):
            return got


def main() -> int:
    rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 8
    iface = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("MDNS_PROBE_IFACE", "127.0.0.1")
    print(f"{rounds} datagrams per round; this host's address for the unicast target: {iface}")

    failures = 0

    # ---- control 1: an exact-address bind must receive the stream. ----
    exact = mdns_socket(iface)
    send_many(iface, rounds)
    got_exact = drain(exact)
    exact.close()
    print(f"control, bound to {iface!r}:      {got_exact} of {rounds}")
    if got_exact == 0:
        print("  CONTROL FAILED — this machine delivers no unicast to port 5353 at all;")
        print("  the wildcard row below would then read as deafness of the instrument, not of the bind.")
        failures += 1

    # ---- control 2: a group send must reach the wildcard listener. ----
    wildcard = mdns_socket("", join_on="127.0.0.1")
    send_many(MDNS_GROUP, rounds, group=True)
    got_group = drain(wildcard)
    print(f"control, wildcard bind, group:   {got_group} of {rounds}")
    if got_group == 0:
        print("  CONTROL FAILED — a joined wildcard socket hears no multicast here, so")
        print("  §16 第 13 条's whole reading has changed on this machine and nothing below is comparable.")
        failures += 1

    # ---- the shipped shape: two wildcard binds, one unicast stream. ----
    second = mdns_socket("")
    send_many(iface, rounds)
    got_first, got_second = drain(wildcard), drain(second)
    second.close()
    print(f"shipped shape, two wildcard binds on {iface}:5353:")
    print(f"  socket A: {got_first}   socket B: {got_second}   of {rounds}")
    if got_first + got_second == 0:
        print("  ⇒ neither receives it. This is the finding, not a broken instrument:")
        print("    control 1 shows the same stream arriving in full the moment a socket is")
        print("    bound to the address it is addressed at.")
        print("    → 绑定地址的那只 socket 才是这条流的收件人；本端口两只都绑通配，所以任何")
        print("      送到我们地址上的单播 mDNS 报文都在进我们任何一只 socket 之前被判给了别人。")
        print("      今天咬到的是「别人单播问我们 ⇒ 我们听不见，也就不会答」；而 `mdns::query` 现在")
        print("      不设 QU（class 里没有 0x8000），设备的答复是多播回来的，所以浏览器那一侧")
        print("      暂时不受影响 —— 哪天为了礼貌把查询设成单播请求，答复会掉在同一个洞里。")
    elif got_first == 0 or got_second == 0:
        print("  ⇒ one socket of the reuse group took the whole stream — which is §16 第 13 条")
        print("    的形状；但注意今天两只都绑通配，所以合并它们并不改变谁能收到这一条。")
    else:
        print("  ⇒ split across both, which neither measured kernel was reported to do for unicast.")
        failures += 1

    wildcard.close()

    # ---- the row that decides the fix's shape: is this a swap or an addition? ----
    # A socket bound to the address hears the unicast and **not the group** — measured, not
    # inferred — so the port cannot simply move its bind from `0.0.0.0` to the interface
    # address and keep announcing. Two roles, two sockets, which is exactly why the
    # reference has two constructors.
    both = mdns_socket(iface, join_on=iface)
    time.sleep(0.2)
    drain(both)
    send_many(MDNS_GROUP, rounds, group=True, via="127.0.0.1" if iface == "127.0.0.1" else iface)
    hear_group = drain(both)
    send_many(iface, rounds)
    hear_unicast = drain(both)
    both.close()
    print(
        f"one socket bound AND joined on {iface}: group {hear_group} of {rounds}, "
        f"unicast {hear_unicast} of {rounds}"
    )
    if hear_group == 0 and hear_unicast == rounds:
        print("  ⇒ a per-address bind is not a substitute for the wildcard listener: the fix is an")
        print("    **addition** (keep the group listener, add one socket per address), not a swap.")

    print(
        "SHAPE the reference keeps both kinds — one wildcard listener for the group and one\n"
        "      socket **bound to each interface address** (`net.py:455-461` / `415-455`).\n"
        "      The port has only the first."
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
