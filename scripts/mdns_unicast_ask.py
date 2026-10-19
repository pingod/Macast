#!/usr/bin/env python3
"""Ask one mDNS question **to an address**, and report whether anyone answered.

Why this exists (design doc §16 第 40 条, measured 2026-10-20 by
`scripts/mdns_unicast_bind_probe.py`): a socket bound to `0.0.0.0:5353` receives
**none** of the traffic sent to this host's own address:5353. That is not the
`SO_REUSEPORT` coin flip §16 第 13 条 used to blame — it is a routing fact about
unicast versus group delivery, and it held for both wildcard sockets in that probe
(`0 of 6`) while a socket bound to the exact address took all six. RFC 6762 §5.1 lets a
querier ask us directly instead of asking the group, and this machine's own
`mDNSResponder` does exactly that once it has learned an address for a responder. So a
receiver with only a wildcard socket has been answering nobody for as long as it has
existed, and every test of it passed, because every test asked the group.

The unit witness for the fix is `announce_tests::e12`, inside one process. What only a
real machine can say is whether the **shipped app process** has that socket live and
reads it — which is what `verify_rust_receiver.sh` §33 asks through this script.

Three rules this instrument keeps:

* **The question is not ours.** It is written by `zeroconf`'s own `DNSOutgoing` and the
  answer is read back by `DNSIncoming`, so neither the shape on the wire nor the verdict
  about the reply is a claim this repository makes about itself. §22 used the same split
  in the other direction (the reference's parser reading our announcement); here it is
  the reference's *writer* asking and its *reader* judging our answer.
* **Nothing of this run reaches the LAN.** The socket is bound to `127.0.0.1`, and the
  destination is the machine's own address, so the kernel loops the datagram internally.
  Measured on 2026-10-20: an asker bound to `127.0.0.1:51703` sending to
  `192.168.1.5:5353` was received by a socket bound to `192.168.1.5:5353` **from**
  `('127.0.0.1', 51703)`, while a `0.0.0.0:5353` socket in the same test heard nothing.
* **An empty answer is told apart from a wrong one.** Exit code 2 means no datagram
  arrived — which on this Mac is §25's unnamed inbound-delivery gap, not a verdict about
  the responder — so the caller has to look at the app's own `heard query` line before
  it says anything. Exit 1 means someone answered and it was not us, which *is* a
  product reading.

Usage:

    .venv/bin/python scripts/mdns_unicast_ask.py --addr 192.168.1.5 \
        --needle Macast-Rust-Unicast --seconds 8

Prints one line per reply and a final `VERDICT:` line. Exit 0 answered-and-named-us,
1 answered-but-not-us, 2 nobody answered, 3 could not ask at all.
"""

import argparse
import socket
import sys

try:
    from zeroconf import const
    from zeroconf._dns import DNSQuestion
    from zeroconf._protocol.incoming import DNSIncoming
    from zeroconf._protocol.outgoing import DNSOutgoing
except ImportError as exc:  # pragma: no cover - instrumented by the caller's exit code
    print("ASK_FAILED zeroconf is not importable here: %r" % (exc,), file=sys.stderr)
    sys.exit(3)


def build_query(service, unicast_response):
    """One PTR question, written by the reference stack rather than by hand.

    The `unicast_response` bit lives in the **class** field (`_CLASS_UNIQUE` is
    0x8000), not in a flag — a hand-rolled query that forgot it would still be a legal
    question, and a responder that ignores it would still be wrong, so the caller gets to
    ask both shapes and the bit is exposed rather than fixed.
    """
    out = DNSOutgoing(const._FLAGS_QR_QUERY)
    cls = const._CLASS_IN | (const._CLASS_UNIQUE if unicast_response else 0)
    out.add_question(DNSQuestion(service, const._TYPE_PTR, cls))
    return out.packets()[0]


def type_name(rtype):
    """A readable name for the record types an answer of ours can carry.

    Deliberately a table rather than zeroconf's own helper: `DNSRecord._type_name` is a
    private method of a Cython build, and an instrument that reaches into a third party's
    underscore names breaks on their next release for a reason that has nothing to do
    with what it is measuring.
    """
    return {
        const._TYPE_A: "A",
        const._TYPE_AAAA: "AAAA",
        const._TYPE_NSEC: "NSEC",
        const._TYPE_PTR: "PTR",
        const._TYPE_SRV: "SRV",
        const._TYPE_TXT: "TXT",
    }.get(rtype, str(rtype))


def describe(message, source):
    """One line about a decoded message: who is answered, and with what."""
    records = list(message.answers())
    kinds = ",".join(type_name(record.type) for record in records)
    names = ",".join(sorted({name for record in records
                             for name in (record.name,
                                          getattr(record, "alias", None),
                                          getattr(record, "server", None))
                             if name}))
    return "answer from {src}: qd={qd} an={an} flags=0x{flags:04x} types=[{types}] names=[{names}]".format(
        src=source,
        qd=len(message.questions),
        an=message.num_answers,
        flags=message.flags,
        types=kinds,
        names=names,
    )


def carries_needle(data, needle):
    """Does this datagram name the instance we asked about?

    Matched on the **wire bytes**, because that is where the instance actually lives: a
    PTR answer record's `.name` is the *service type* and the instance sits in the rdata
    as length-prefixed labels, so a name-based match silently never fires — and dotted
    names do not appear on the wire at all (§25 measured a dotted filter counting a full
    group of queries as `queries=0`). The needle is therefore an instance's first label,
    dots omitted; `verify_rust_receiver.sh` §33 takes it out of the announcement line for
    exactly that reason.
    """
    return needle.encode() in data

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--addr", required=True,
                        help="the destination address to ask (this machine's own address)")
    parser.add_argument("--port", type=int, default=5353)
    parser.add_argument("--service", default="_googlecast._tcp.local.")
    parser.add_argument("--needle", required=True,
                        help="the instance name an answer must carry to count as ours")
    parser.add_argument("--seconds", type=float, default=8.0)
    parser.add_argument("--interval", type=float, default=1.0,
                        help="how often to repeat the question (a real querier retries too)")
    parser.add_argument("--src", default="127.0.0.1",
                        help="where to bind the asker; loopback keeps this off the wire")
    parser.add_argument("--qu", action="store_true",
                        help="set the unicast-response bit in the question's class")
    args = parser.parse_args()

    query = build_query(args.service, args.qu)

    # Self-check, and the reason the exit codes mean what they claim: if the reference's
    # own reader cannot read our question, then "nobody answered" would be about this
    # script rather than about any responder. Decoding the bytes we are about to put on
    # the wire is the cheapest control available, and it is a *second* implementation
    # doing the reading.
    echoed = DNSIncoming(query)
    if not echoed.is_query() or not echoed.questions:
        print("ASK_FAILED the query this script built does not read back as a query",
              file=sys.stderr)
        return 3
    question = echoed.questions[0]
    print("asking %s type=%s class=0x%04x (%d bytes, unicast-response=%s)" % (
        question.name, question.type, question.class_, len(query), bool(args.qu)))

    asker = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        asker.bind((args.src, 0))
    except OSError as exc:
        print("ASK_FAILED cannot bind the asker on %s: %s" % (args.src, exc),
              file=sys.stderr)
        return 3
    destination = (args.addr, args.port)
    asker.settimeout(args.interval)

    deadline = args.seconds
    waited = 0.0
    replies = 0
    named = 0
    try:
        while waited < deadline:
            try:
                asker.sendto(query, destination)
            except OSError as exc:
                # One honest reading of "no route to host" here is the macOS local-network
                # gate (§ env note: TCC refuses LAN I/O for an unsigned identity), so the
                # reason is printed rather than folded into "nobody answered".
                print("ASK_FAILED sendto %s: %s" % (destination, exc), file=sys.stderr)
                return 3
            waited += args.interval
            try:
                data, peer = asker.recvfrom(4096)
            except socket.timeout:
                continue
            except OSError as exc:
                print("ASK_FAILED recvfrom: %s" % (exc,), file=sys.stderr)
                return 3
            replies += 1
            message = DNSIncoming(data)
            print(describe(message, peer))
            if carries_needle(data, args.needle):
                named += 1
    finally:
        asker.close()

    if replies == 0:
        print("VERDICT: nobody answered %s within %.1fs — this is a delivery question, "
              "not an answering one; read the app's own 'heard query' line before "
              "concluding anything" % (destination, args.seconds))
        return 2
    if named == 0:
        print("VERDICT: %d reply(s) arrived and none carries '%s'" % (replies, args.needle))
        return 1
    print("VERDICT: %d of %d reply/replies name '%s'" % (named, replies, args.needle))
    return 0


if __name__ == "__main__":
    sys.exit(main())
