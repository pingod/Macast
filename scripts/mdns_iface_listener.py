#!/usr/bin/env python3
"""Count the mDNS datagrams that arrive **through one interface**, for N seconds.

Why this exists as a file rather than a shell one-liner: §32 of
`scripts/verify_rust_receiver.sh` has to compare the *same* socket's count before and
after a 网卡 pin, on a card that has nothing attached to it (a VM bridge). That needs one
socket held open across the switch, and a socket that is closed and reopened between the
two readings measures whether the second bind joined in time — not whether the packets
moved.

Three facts this tool is built to keep honest, all of them learned the expensive way in
this repository (AGENTS.md §4.2, 「测试自己的实现也是实现」):

* **Bind 5353 with `SO_REUSEPORT`.** On macOS the system's own `mDNSResponder` holds that
  port, so a plain bind fails with `EADDRINUSE` and the listener would report zero — the
  same "the instrument is the outage" mistake as reading `0` and concluding 「没有答复」.
* **Join with `imr_interface` set to the address we are asking about.** Joining on the
  default route and then concluding "this card hears nothing" is the same reading error
  in the other direction.
* **Match on the wire form of a name, not on its dotted spelling.** mDNS names are
  length-prefixed labels, so `Macast-Rust-Iface` appears in the packet *without* the
  `.local.` dots — a filter written against the dotted text counts zero and looks like a
  silent responder. (§25 measured exactly this, twice: a filter on `_googlecast._tcp.local.`
  read a full group's queries as `queries=0`.) This tool therefore takes a **substring**
  and matches bytes, and prints the raw hex of every hit so a wrong filter is visible in
  the output instead of being inferred from a zero.

Output: one line per matching datagram —

    <epoch_ms> <source_ip> <length> <hex>

and a final `LISTENED <iface> hits <n>` line on stdout, which is what the shell greps.

Usage:
    .venv/bin/python scripts/mdns_iface_listener.py --iface 192.168.139.3 \\
        --seconds 12 --needle Macast-Rust-Iface --out /tmp/hits.txt
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import time

MDNS_GROUP = "224.0.0.251"
MDNS_PORT = 5353


def listen(iface: str, seconds: float, needle: bytes, out) -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    # Not wrapped in a try/except on purpose: a platform without SO_REUSEPORT cannot run
    # this probe at all, and that is a louder answer than a listener that silently hears
    # nothing (AGENTS.md §4.2's 「静默的跳过才是'不提问的检查等于通过的检查'」).
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    sock.bind(("", MDNS_PORT))
    # `ip_mreq` is two `in_addr`s, both 4 bytes. Packing it as `4sl` — which is what a
    # Linux-flavoured snippet tends to copy — asks the kernel to join a group named by
    # the interface address and fails with EINVAL. Measured here on macOS, where the
    # first version of this tool died on that line and every reading came back empty.
    mreq = struct.pack(
        "4s4s", socket.inet_aton(MDNS_GROUP), socket.inet_aton(iface)
    )
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
    sock.settimeout(1.0)

    hits = 0
    deadline = time.time() + seconds
    with open(out, "wb") as sink:
        while time.time() < deadline:
            try:
                data, peer = sock.recvfrom(65535)
            except socket.timeout:
                continue
            except OSError:
                break
            if needle and needle not in data:
                continue
            hits += 1
            stamp = ("%d" % (time.time() * 1000)).encode()
            sink.write(
                stamp
                + b" "
                + peer[0].encode()
                + b" "
                + str(len(data)).encode()
                + b" "
                + data.hex().encode()
                + b"\n"
            )
            sink.flush()
    return hits


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iface", required=True, help="local IPv4 address to join on")
    parser.add_argument("--seconds", type=float, default=10.0)
    parser.add_argument(
        "--needle",
        default="",
        help="byte substring a packet must contain (a name fragment, in its WIRE form)",
    )
    parser.add_argument("--out", required=True, help="file to append one line per hit")
    args = parser.parse_args()

    try:
        needle = args.needle.encode()
    except UnicodeError:
        print("needle is not representable as bytes", file=sys.stderr)
        return 2
    try:
        hits = listen(args.iface, args.seconds, needle, args.out)
    except OSError as e:
        # A listener that could not start is not a listener that heard nothing: say which
        # one it was, or the caller reads "hits 0" as a product verdict.
        print("LISTEN_FAILED %s %s" % (args.iface, e), file=sys.stderr)
        return 2
    print("LISTENED %s hits %d" % (args.iface, hits))
    return 0


if __name__ == "__main__":
    sys.exit(main())
