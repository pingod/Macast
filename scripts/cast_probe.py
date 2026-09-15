#!/usr/bin/env python3
"""Minimal Chromecast *sender* used to test Macast's Cast receiver.

Mimics the handshake a real sender performs, printing every response so we can
see exactly which step fails:

    TLS connect -> CONNECT -> GET_STATUS -> LAUNCH
                -> CONNECT(transportId) -> LOAD

Usage:
    .venv/bin/python scripts/cast_probe.py [host] [port] [media_url]
"""

import json
import socket
import ssl
import struct
import sys
import time

NS_CONNECTION = "urn:x-cast:com.google.cast.tp.connection"
NS_HEARTBEAT = "urn:x-cast:com.google.cast.tp.heartbeat"
NS_RECEIVER = "urn:x-cast:com.google.cast.receiver"
NS_MEDIA = "urn:x-cast:com.google.cast.media"

SENDER = "sender-0"
RECEIVER = "receiver-0"


# -- protobuf helpers (mirror macast/protocol_cast.py) -----------------------

def _varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            break
    return bytes(out)


def _f_varint(num, value):
    return bytes([(num << 3) | 0]) + _varint(value)


def _f_bytes(num, data):
    return bytes([(num << 3) | 2]) + _varint(len(data)) + data


def encode(source, dest, ns, payload, binary=False):
    buf = bytearray()
    buf += _f_varint(1, 0)
    buf += _f_bytes(2, source.encode())
    buf += _f_bytes(3, dest.encode())
    buf += _f_bytes(4, ns.encode())
    if binary:
        buf += _f_varint(5, 1)
        buf += _f_bytes(7, payload)
    else:
        buf += _f_varint(5, 0)
        buf += _f_bytes(6, payload.encode())
    return bytes(buf)


def _fields(buf):
    i, n = 0, len(buf)
    while i < n:
        tag = buf[i]
        i += 1
        fnum, wtype = tag >> 3, tag & 0x07
        if wtype == 0:
            val = 0
            shift = 0
            while i < n:
                b = buf[i]
                i += 1
                val |= (b & 0x7F) << shift
                shift += 7
                if not (b & 0x80):
                    break
            yield fnum, wtype, val
        elif wtype == 2:
            length = 0
            shift = 0
            while i < n:
                b = buf[i]
                i += 1
                length |= (b & 0x7F) << shift
                shift += 7
                if not (b & 0x80):
                    break
            yield fnum, wtype, buf[i:i + length]
            i += length
        else:
            return


def decode(buf):
    msg = {"source_id": "", "destination_id": "", "namespace": "",
           "payload_type": 0, "payload_utf8": "", "payload_binary": b""}
    for fnum, wtype, val in _fields(buf):
        if fnum == 2 and wtype == 2:
            msg["source_id"] = val.decode("utf-8", "replace")
        elif fnum == 3 and wtype == 2:
            msg["destination_id"] = val.decode("utf-8", "replace")
        elif fnum == 4 and wtype == 2:
            msg["namespace"] = val.decode("utf-8", "replace")
        elif fnum == 5 and wtype == 0:
            msg["payload_type"] = val
        elif fnum == 6 and wtype == 2:
            msg["payload_utf8"] = val.decode("utf-8", "replace")
        elif fnum == 7 and wtype == 2:
            msg["payload_binary"] = bytes(val)
    return msg


class CastClient(object):
    def __init__(self, host, port):
        self.sock = None
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((host, port), timeout=10)
        self.sock = ctx.wrap_socket(raw, server_hostname=host)
        print("  TLS OK   cipher=%s" % (self.sock.cipher()[0],))

    def send(self, dest, ns, payload, binary=False):
        blob = encode(SENDER, dest, ns, payload, binary)
        self.sock.sendall(struct.pack(">I", len(blob)) + blob)

    def recv(self, timeout=6):
        self.sock.settimeout(timeout)
        try:
            hdr = self.sock.recv(4)
            if len(hdr) < 4:
                return None
            (length,) = struct.unpack(">I", hdr)
            body = b""
            while len(body) < length:
                chunk = self.sock.recv(length - len(body))
                if not chunk:
                    return None
                body += chunk
        except socket.timeout:
            return None
        return decode(body)

    def drain(self, label, seconds=2.0):
        """Print everything the receiver says for a while."""
        end = time.time() + seconds
        got = []
        while time.time() < end:
            msg = self.recv(1.0)
            if msg is None:
                continue
            got.append(msg)
            ns = msg["namespace"].rsplit(".", 1)[-1]
            body = msg["payload_utf8"] or repr(msg["payload_binary"][:24])
            print("    <- [%s] %s" % (ns, body[:400]))
        if not got:
            print("    <- (%s) 无响应" % label)
        return got


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8009
    url = (sys.argv[3] if len(sys.argv) > 3
           else "https://commondatastorage.googleapis.com/gtv-videos-bucket/"
                "sample/BigBuckBunny.mp4")

    print("== 1. 连接 %s:%d ==" % (host, port))
    c = CastClient(host, port)

    print("== 2. CONNECT (receiver-0) ==")
    c.send(RECEIVER, NS_CONNECTION, json.dumps({"type": "CONNECT"}))
    c.drain("CONNECT")

    print("== 3. GET_STATUS ==")
    c.send(RECEIVER, NS_RECEIVER, json.dumps({"type": "GET_STATUS", "requestId": 1}))
    status = c.drain("GET_STATUS")

    print("== 4. LAUNCH (CC1AD845 默认媒体接收器) ==")
    c.send(RECEIVER, NS_RECEIVER,
           json.dumps({"type": "LAUNCH", "appId": "CC1AD845", "requestId": 2}))
    launched = c.drain("LAUNCH")

    transport = None
    for m in launched:
        if m["namespace"] == NS_RECEIVER and m["payload_utf8"]:
            try:
                apps = json.loads(m["payload_utf8"])["status"]["applications"]
                if apps:
                    transport = apps[0].get("transportId")
            except Exception:
                pass
    if not transport:
        print("  !! 没有拿到 transportId，无法继续")
        return 1
    print("  transportId = %s" % transport)

    print("== 5. CONNECT (%s) ==" % transport)
    c.send(transport, NS_CONNECTION, json.dumps({"type": "CONNECT"}))
    c.drain("CONNECT app")

    print("== 6. LOAD ==")
    c.send(transport, NS_MEDIA, json.dumps({
        "type": "LOAD",
        "requestId": 3,
        "autoplay": True,
        "currentTime": 0,
        "media": {
            "contentId": url,
            "contentType": "video/mp4",
            "streamType": "BUFFERED",
            "metadata": {"type": 0, "metadataType": 0, "title": "probe"},
        },
    }))
    c.drain("LOAD", seconds=4.0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
