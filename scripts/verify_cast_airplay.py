#!/usr/bin/env python3
# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Verification harness for the Chromecast / AirPlay receiver protocols.
#
# It loads the REAL protocol modules (utils / protocol / discovery /
# protocol_cast / protocol_airplay) without booting the full GUI app, stubs
# `netifaces` (which needs a real interface table), and exercises:
#   1. Chromecast protobuf codec + command routing (auth / status / LOAD / ...)
#   2. AirPlay RTSP command routing (ANNOUNCE + PLAY / PAUSE / STOP)
#   3. End-to-end over real TLS(8009) / RTSP(7000) sockets
#   4. That mDNS advertisement is triggered with the right service/port
#
# Run with the project's managed venv, e.g.:
#   .venv/bin/python scripts/verify_cast_airplay.py
#

import importlib.util
import http.client
import json
import os
import shutil as _shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile as _tempfile
import threading
import time
import types
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MACAST = os.path.join(REPO, "macast")

# --------------------------------------------------------------------------
# Stub netifaces so utils.py imports without a real interface table.
# --------------------------------------------------------------------------
_netifaces = types.ModuleType("netifaces")
_netifaces.AF_INET = 2
_netifaces.AF_LINK = 17
_netifaces.gateways = lambda: {}
_netifaces.ifaddresses = lambda i: {2: [{"addr": "127.0.0.1", "netmask": "255.0.0.0"}]}
sys.modules["netifaces"] = _netifaces

# Stub AppKit (pyobjc) -- only used by Setting.set_start_at_login, which the
# verification never invokes.
if sys.platform == "darwin":
    _appkit = types.ModuleType("AppKit")
    _appkit.NSBundle = object
    sys.modules["AppKit"] = _appkit

# --------------------------------------------------------------------------
# Load the real macast submodules into a fake package (no GUI / no macast_renderer).
# --------------------------------------------------------------------------
_pkg = types.ModuleType("macast")
_pkg.__path__ = [MACAST]
sys.modules["macast"] = _pkg


def _load(name, fname):
    path = os.path.join(MACAST, fname)
    spec = importlib.util.spec_from_file_location("macast." + name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["macast." + name] = mod
    spec.loader.exec_module(mod)
    return mod


utils = _load("utils", "utils.py")
protocol = _load("protocol", "protocol.py")
discovery = _load("discovery", "discovery.py")
cast = _load("protocol_cast", "protocol_cast.py")
airplay = _load("protocol_airplay", "protocol_airplay.py")

# --------------------------------------------------------------------------
# Test doubles
# --------------------------------------------------------------------------
class MockRenderer:
    def __init__(self):
        self.calls = []

    def _rec(self, name, *args):
        self.calls.append((name, args))

    # Transport state the Cast protocol polls to decide whether playback really
    # started. Class-level defaults keep the recorder tests unchanged; the
    # LOAD tests set them per instance.
    transport_state = "STOPPED"
    transport_status = "OK"

    def get_state_transport_state(self):
        return self.transport_state

    def get_state_transport_status(self):
        return self.transport_status

    def set_media_url(self, url, start="0"):
        self._rec("set_media_url", url, start)

    def set_media_pause(self):
        self._rec("set_media_pause")

    def set_media_resume(self):
        self._rec("set_media_resume")

    def set_media_stop(self):
        self._rec("set_media_stop")

    def set_media_volume(self, v):
        self._rec("set_media_volume", v)

    def set_media_mute(self, m):
        self._rec("set_media_mute", m)

    def set_media_position(self, p):
        self._rec("set_media_position", p)

    def called(self, name):
        return any(c[0] == name for c in self.calls)

    def last_arg(self, name):
        for c in reversed(self.calls):
            if c[0] == name:
                return c[1][0]
        return None


class FakeSock:
    def __init__(self):
        self.sent = b""

    def sendall(self, data):
        self.sent += data

    def close(self):
        pass


class MockAdvertiser:
    calls = []

    def __init__(self):
        self.advertised = []

    def advertise(self, service_type, name, port, properties, server=None):
        self.advertised.append((service_type, name, port, properties))
        MockAdvertiser.calls.append((service_type, name, port))

    def unadvertise(self, *a, **k):
        pass

    def close(self):
        pass


# Monkeypatch the advertiser into both protocol modules.
cast.MDNSAdvertiser = MockAdvertiser
airplay.MDNSAdvertiser = MockAdvertiser


class TestProtocol(cast.ChromecastProtocol):
    @property
    def renderer(self):
        return _CTX.renderer


class TestAirPlay(airplay.AirPlayProtocol):
    @property
    def renderer(self):
        return _CTX.renderer


# --------------------------------------------------------------------------
# Test runner
# --------------------------------------------------------------------------
RESULTS = []


def check(name, cond, detail=""):
    RESULTS.append((name, bool(cond), detail))
    mark = "PASS" if cond else "FAIL"
    print("[{}] {}".format(mark, name) + ("" if cond else "  -- " + detail))


def _rejects(fn):
    """True when `fn` raises ValueError -- used for the refusal paths."""
    try:
        fn()
    except ValueError:
        return True
    except Exception:
        return False
    return False


def _recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


# --------------------------------------------------------------------------
# Part 1: Chromecast codec + routing
# --------------------------------------------------------------------------
print("\n=== Part 1: Chromecast codec + routing ===")
_CTX = types.SimpleNamespace(renderer=MockRenderer())

# 1a protobuf roundtrip
msg = cast.encode_cast_message(
    "sender-0", "receiver-0", "urn:x-cast:com.google.cast.receiver",
    json.dumps({"type": "GET_STATUS"}),
)
parsed = cast.parse_cast_message(msg)
check("cast codec roundtrip source_id", parsed["source_id"] == "sender-0", parsed.get("source_id"))
check("cast codec roundtrip namespace", parsed["namespace"] == "urn:x-cast:com.google.cast.receiver")
check("cast codec roundtrip payload", "GET_STATUS" in parsed["payload_utf8"])

# 1b auth handshake
p = TestProtocol()
sock = FakeSock()
p._on_message(sock, {
    "source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.tp.deviceauth", "payload_type": 0,
    "payload_utf8": json.dumps({"type": "CHALLENGE"}), "payload_binary": b"",
})
auth = cast.parse_cast_message(sock.sent[4:])
check("cast auth response sent", auth["payload_type"] == 1 and len(auth["payload_binary"]) > 0)

# 1c receiver GET_STATUS
sock = FakeSock()
p._on_message(sock, {
    "source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.receiver", "payload_type": 0,
    "payload_utf8": json.dumps({"type": "GET_STATUS", "requestId": 1}), "payload_binary": b"",
})
status = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
check("cast receiver status type", status.get("type") == "RECEIVER_STATUS", str(status.get("type")))

# 1c2 virtual connection (tp.connection) + heartbeat (tp.heartbeat)
# Real senders open a virtual connection before anything else and PING ~5s.
sock = FakeSock()
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": cast.NS_CONNECTION, "payload_type": 0,
    "payload_utf8": json.dumps({"type": "CONNECT"}), "payload_binary": b""})
conn = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
check("cast CONNECT acknowledged", conn.get("type") == "CONNECT", str(conn))
check("cast CONNECT registers sender", "sender-0" in p._senders)

sock = FakeSock()
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": cast.NS_HEARTBEAT, "payload_type": 0,
    "payload_utf8": json.dumps({"type": "PING"}), "payload_binary": b""})
hb = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
check("cast PING -> PONG", hb.get("type") == "PONG", str(hb))

# LAUNCH must report the app namespaces senders rely on
sock = FakeSock()
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": cast.NS_RECEIVER, "payload_type": 0,
    "payload_utf8": json.dumps({"type": "LAUNCH", "appId": "CC1AD845"}),
    "payload_binary": b""})
st = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
apps = st["status"]["applications"]
# Regression: `namespaces` must be a list of {"name": ...} objects, not bare
# strings -- cast_channel.proto defines Namespace{string name = 1}. Senders
# that parse it strictly reject the media namespace otherwise and never LOAD.
_ns = apps[0].get("namespaces", []) if apps else []
_ns_names = [n.get("name") for n in _ns if isinstance(n, dict)]
check("cast LAUNCH reports media namespace",
      cast.NS_MEDIA in _ns_names, str(apps))
check("namespaces are objects with a 'name' field, not bare strings",
      bool(_ns) and all(isinstance(n, dict) and "name" in n for n in _ns),
      str(_ns))

sock = FakeSock()
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": cast.NS_RECEIVER, "payload_type": 0,
    "payload_utf8": json.dumps({"type": "GET_APP_AVAILABILITY",
                                "appId": ["CC1AD845"]}), "payload_binary": b""})
av = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
check("cast GET_APP_AVAILABILITY",
      av.get("availability", {}).get("CC1AD845") == "APP_AVAILABLE", str(av))

# A LOAD with no playable URL must report LOAD_FAILED, not fail silently.
sock = FakeSock()
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": cast.NS_MEDIA, "payload_type": 0,
    "payload_utf8": json.dumps({"type": "LOAD", "media": {}}), "payload_binary": b""})
lf = json.loads(cast.parse_cast_message(sock.sent[4:])["payload_utf8"])
check("cast LOAD without contentId -> LOAD_FAILED", lf.get("type") == "LOAD_FAILED", str(lf))

# 1d media LOAD -> set_media_url
_CTX.renderer = MockRenderer()
p = TestProtocol()
sock = FakeSock()
p._on_message(sock, {
    "source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.media", "payload_type": 0,
    "payload_utf8": json.dumps({
        "type": "LOAD", "currentTime": 0,
        "media": {"contentId": "http://example.com/video.mp4", "contentType": "video/mp4"},
    }), "payload_binary": b"",
})
check("cast LOAD -> set_media_url called", _CTX.renderer.called("set_media_url"))
check("cast LOAD -> correct url",
      _CTX.renderer.last_arg("set_media_url") == "http://example.com/video.mp4",
      str(_CTX.renderer.last_arg("set_media_url")))


# 1d-bis media LOAD must not claim PLAYING before the player confirms it.
#
# Regression: the receiver answered LOAD with BUFFERING then PLAYING
# immediately, before mpv had even opened the URL. A stream that never plays
# (wrong advertised address, a proxy in the way, an unsupported codec) then
# still looked healthy -- and the sender, told everything was fine, never
# retried. Two of the bugs in this file were only invisible because of that.
def _load_statuses(renderer, grace=0.5):
    _CTX.renderer = renderer
    proto = TestProtocol()
    proto.LOAD_GRACE_SECONDS = grace
    s = FakeSock()
    proto._on_message(s, {
        "source_id": "sender-0", "destination_id": "receiver-0",
        "namespace": "urn:x-cast:com.google.cast.media", "payload_type": 0,
        "payload_utf8": json.dumps({
            "type": "LOAD", "requestId": 7, "currentTime": 0,
            "media": {"contentId": "http://example.com/video.mp4",
                      "contentType": "video/mp4"},
        }), "payload_binary": b"",
    })
    time.sleep(grace + 0.7)
    out, off = [], 0
    while off + 4 <= len(s.sent):
        (n,) = struct.unpack(">I", s.sent[off:off + 4])
        msg = cast.parse_cast_message(s.sent[off + 4: off + 4 + n])
        off += 4 + n
        try:
            out.append(json.loads(msg["payload_utf8"]))
        except Exception:
            pass
    return out


def _player_states(msgs):
    states = []
    for m in msgs:
        if m.get("type") == "MEDIA_STATUS":
            states.append((m.get("status") or [{}])[0].get("playerState"))
    return states


msgs = _load_statuses(MockRenderer())
states = _player_states(msgs)
check("LOAD answers BUFFERING first, not PLAYING",
      states[:1] == ["BUFFERING"], str(states))
check("unconfirmed playback only falls back to PLAYING after the grace period",
      states[-1:] == ["PLAYING"] and "PLAYING" not in states[:-1], str(states))

_playing = MockRenderer()
_playing.transport_state = "PLAYING"
states = _player_states(_load_statuses(_playing))
check("confirmed playback is reported as PLAYING",
      states == ["BUFFERING", "PLAYING"], str(states))

_broken = MockRenderer()
_broken.transport_status = "ERROR_OCCURRED"
msgs = _load_statuses(_broken)
check("player error is reported as LOAD_FAILED",
      any(m.get("type") == "LOAD_FAILED" for m in msgs),
      str([m.get("type") for m in msgs]))
check("a failed load does not also claim PLAYING",
      "PLAYING" not in _player_states(msgs), str(_player_states(msgs)))

# 1e PLAY / PAUSE / STOP routing
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.media", "payload_type": 0,
    "payload_utf8": json.dumps({"type": "PLAY"}), "payload_binary": b""})
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.media", "payload_type": 0,
    "payload_utf8": json.dumps({"type": "PAUSE"}), "payload_binary": b""})
p._on_message(sock, {"source_id": "sender-0", "destination_id": "receiver-0",
    "namespace": "urn:x-cast:com.google.cast.media", "payload_type": 0,
    "payload_utf8": json.dumps({"type": "STOP"}), "payload_binary": b""})
check("cast PLAY -> resume", _CTX.renderer.called("set_media_resume"))
check("cast PAUSE -> pause", _CTX.renderer.called("set_media_pause"))
check("cast STOP -> stop", _CTX.renderer.called("set_media_stop"))

# --------------------------------------------------------------------------
# Part 2: AirPlay RTSP routing
# --------------------------------------------------------------------------
print("\n=== Part 2: AirPlay RTSP routing ===")
_CTX.renderer = MockRenderer()
ap = TestAirPlay()

code, headers, body = ap._handle_rtsp("OPTIONS", {"CSeq": "1"}, "")
check("airplay OPTIONS 200", code == 200 and "ANNOUNCE" in headers.get("Public", ""), str(headers))

code, headers, body = ap._handle_rtsp("ANNOUNCE", {
    "CSeq": "2", "Content-Location": "http://example.com/air.mp4"}, "")
check("airplay ANNOUNCE 200", code == 200)
code, headers, body = ap._handle_rtsp("SETUP", {"CSeq": "3"}, "")
check("airplay SETUP 200 + session", code == 200 and "Session" in headers)
code, headers, body = ap._handle_rtsp("PLAY", {"CSeq": "4", "Session": "1"}, "")
check("airplay PLAY -> set_media_url", _CTX.renderer.called("set_media_url"))
check("airplay PLAY -> correct url",
      _CTX.renderer.last_arg("set_media_url") == "http://example.com/air.mp4",
      str(_CTX.renderer.last_arg("set_media_url")))
code, headers, body = ap._handle_rtsp("PAUSE", {"CSeq": "5", "Session": "1"}, "")
check("airplay PAUSE -> pause", _CTX.renderer.called("set_media_pause"))
code, headers, body = ap._handle_rtsp("TEARDOWN", {"CSeq": "6", "Session": "1"}, "")
check("airplay TEARDOWN -> stop", _CTX.renderer.called("set_media_stop"))

# 2b SDP-body content-location
_CTX.renderer = MockRenderer()
ap = TestAirPlay()
ap._handle_rtsp("ANNOUNCE", {"CSeq": "2", "Content-Type": "application/sdp",
    "Content-Length": "60"},
    "a=content-location:http://example.com/sdp.mp4\r\n")
ap._handle_rtsp("PLAY", {"CSeq": "4", "Session": "1"}, "")
check("airplay SDP content-location parsed",
      _CTX.renderer.last_arg("set_media_url") == "http://example.com/sdp.mp4",
      str(_CTX.renderer.last_arg("set_media_url")))

# 2c honest capability advertising: never promise a signature we can't produce
code, headers, body = ap._handle_rtsp(
    "OPTIONS", {"CSeq": "1", "Apple-Challenge": "AAAA"}, "")
check("airplay OPTIONS does not fabricate Apple-Challenge/Response",
      code == 200 and "Apple-Challenge" not in headers and
      "Apple-Response" not in headers, str(headers))

# 2d /info capability endpoint
code, headers, body = ap._handle_rtsp("GET", {"CSeq": "2"}, "", uri="/info")
check("airplay GET /info returns plist",
      code == 200 and "plist" in body and "deviceID" in body, body[:80])

# 2e FLUSH
code, headers, body = ap._handle_rtsp("FLUSH", {"CSeq": "3", "Session": "1"}, "")
check("airplay FLUSH 200", code == 200)

# 2f features bitmask must not claim unsupported FairPlay
check("airplay features omit FairPlay bit (0x04)",
      airplay.FEATURES & 0x04 == 0, hex(airplay.FEATURES))

# 2g RTSP message hygiene. Every one of these was a real defect: the fallback
# answered 200 to requests it had never performed, the header dict was
# case-sensitive, and CSeq was fabricated when the client sent none.
code, headers, body = ap._handle_rtsp("OPTIONS", {"CSeq": "1"}, "")
# Read through getattr so that deleting SUPPORTED_METHODS fails this check
# instead of aborting the whole suite with an AttributeError.
_supported = list(getattr(airplay, "SUPPORTED_METHODS", ()))
check("airplay Public lists exactly the implemented methods",
      bool(_supported) and headers.get("Public", "").split(", ") == _supported,
      str(headers.get("Public")))

code, headers, body = ap._handle_rtsp("DESCRIBE", {"CSeq": "9"}, "")
check("an unimplemented RTSP method is refused, not answered 200",
      code == 501, "code={}".format(code))
check("the refusal tells the client what is allowed",
      "OPTIONS" in headers.get("Allow", "") and "PLAY" in headers.get("Allow", ""),
      str(headers.get("Allow")))

ap2 = TestAirPlay()
code, _h, _b = ap2._handle_rtsp("ANNOUNCE", {"CSeq": "2"}, "")
check("ANNOUNCE without Content-Location is refused",
      code == 400, "code={}".format(code))
_CTX.renderer = MockRenderer()
code, headers, body = ap2._handle_rtsp("PLAY", {"CSeq": "3"}, "")
check("PLAY with nothing announced is refused",
      code == 455, "code={}".format(code))
check("a refused PLAY does not touch the player",
      not _CTX.renderer.called("set_media_url"))

# A client that sends lowercase header names must still be understood, and the
# reply must carry *its* CSeq -- not an invented 1.
code, headers, _b = ap2._handle_rtsp(
    "ANNOUNCE", {"cseq": "42", "content-location": "http://example.com/lower.mp4"}, "")
check("lowercase header names are understood",
      code == 200 and headers.get("CSeq") == "42", "{} {}".format(code, headers))
ap2._handle_rtsp("SETUP", {"cseq": "43"}, "")
code, headers, _b = ap2._handle_rtsp("PLAY", {"cseq": "44", "session": "1"}, "")
check("a lowercase session header is accepted",
      code == 200 and headers.get("CSeq") == "44", "{} {}".format(code, headers))
check("case-insensitive Content-Location reached the player",
      _CTX.renderer.last_arg("set_media_url") == "http://example.com/lower.mp4",
      str(_CTX.renderer.last_arg("set_media_url")))

code, headers, _b = ap2._handle_rtsp("OPTIONS", {}, "")
check("no CSeq in the request means no CSeq in the reply",
      "CSeq" not in headers, str(headers))

# Session parameters are legal and are not part of the session id.
code, _h, _b = ap2._handle_rtsp("PLAY", {"CSeq": "45", "Session": "1;timeout=60"}, "")
check("a Session carrying parameters is not rejected",
      code == 200, "code={}".format(code))
code, _h, _b = ap2._handle_rtsp("PAUSE", {"CSeq": "46", "Session": "99"}, "")
check("a stale Session is answered 454",
      code == 454, "code={}".format(code))

# --------------------------------------------------------------------------
# Part 2b: RTSP framing over a real socket
# --------------------------------------------------------------------------
print("\n=== Part 2b: RTSP framing over a real socket ===")
_CTX.renderer = MockRenderer()
ap3 = TestAirPlay()
try:
    ap3.start()
    s = socket.create_connection(("127.0.0.1", ap3.rtsp_port))
    # Fail the check instead of wedging the whole suite if a reply never comes:
    # a framing regression means the second coalesced request is dropped, and
    # without a timeout the harness would block on readline() forever.
    s.settimeout(10.0)
    f = s.makefile("rwb")

    def read_status():
        line = f.readline().decode()
        while not line.strip():
            line = f.readline().decode()
        while f.readline().decode().strip():
            pass
        return line

    # Coalesced requests: SETUP and PLAY in a single write, the shape a sender
    # produces when it does not wait for the first reply. The old parser
    # consumed everything up to the body length and discarded the remainder,
    # so PLAY vanished and the client hung until it gave up.
    blob = (
        "ANNOUNCE rtsp://airplay RTSP/1.0\r\n"
        "CSeq: 1\r\nContent-Location: http://e2e/coalesced.mp4\r\n"
        "Content-Length: 0\r\n\r\n"
        "SETUP rtsp://airplay RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 0\r\n\r\n"
        "PLAY rtsp://airplay RTSP/1.0\r\nCSeq: 3\r\nSession: 1\r\nContent-Length: 0\r\n\r\n"
    )
    f.write(blob.encode())
    f.flush()
    codes = [read_status() for _ in range(3)]
    check("three coalesced RTSP requests are all answered",
          all(" 200 " in c for c in codes), str(codes))
    check("the coalesced PLAY still reached the player",
          _CTX.renderer.last_arg("set_media_url") == "http://e2e/coalesced.mp4",
          str(_CTX.renderer.last_arg("set_media_url")))

    # A non-numeric Content-Length used to raise inside the handler thread: the
    # connection died with no response and no log line.
    f.write(b"OPTIONS rtsp://airplay RTSP/1.0\r\nCSeq: 4\r\n"
            b"Content-Length: banana\r\n\r\n")
    f.flush()
    status = read_status()
    check("a malformed Content-Length is answered with 400",
          " 400 " in status, status)
    s.close()
except Exception as e:
    check("airplay RTSP framing over a real socket", False, repr(e))
finally:
    try:
        ap3.stop()
    except Exception:
        pass

# --------------------------------------------------------------------------
# Part 3: End-to-end over real TLS(8009) / RTSP(7000) sockets
# --------------------------------------------------------------------------
print("\n=== Part 3: end-to-end over real sockets ===")


def cast_send(sock, src, dst, ns, payload, binary=False):
    blob = cast.encode_cast_message(src, dst, ns, payload, binary)
    sock.sendall(struct.pack(">I", len(blob)) + blob)


def cast_recv(sock):
    hdr = _recv_exact(sock, 4)
    if not hdr:
        return None
    (n,) = struct.unpack(">I", hdr)
    body = _recv_exact(sock, n)
    return cast.parse_cast_message(body)


e2e_ok = True
# ---- Chromecast real TLS server ----
_CTX.renderer = MockRenderer()
p = TestProtocol()
MockAdvertiser.calls = []
try:
    p.start()
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with ctx.wrap_socket(socket.create_connection(("127.0.0.1", p.cast_port)),
                         server_hostname=None) as s:
        cast_send(s, "sender-0", "receiver-0",
                  "urn:x-cast:com.google.cast.tp.deviceauth",
                  json.dumps({"type": "CHALLENGE"}))
        cast_recv(s)  # auth response
        cast_send(s, "sender-0", "receiver-0",
                  "urn:x-cast:com.google.cast.media",
                  json.dumps({"type": "LOAD", "currentTime": 0,
                              "media": {"contentId": "http://e2e/v.mp4"}}))
        cast_recv(s)  # media status
    check("chromecast e2e LOAD reached renderer",
          _CTX.renderer.called("set_media_url") and
          _CTX.renderer.last_arg("set_media_url") == "http://e2e/v.mp4",
          str(_CTX.renderer.last_arg("set_media_url")))
except Exception as e:
    e2e_ok = False
    check("chromecast e2e (TLS server)", False, "start/connect failed: {}".format(e))
finally:
    try:
        p.stop()
    except Exception:
        pass

# ---- A stray/aborted connection must not take the receiver down ----
# Regression: the receiver used to wrap the *listening* socket in TLS, which
# made accept() run the handshake inline. A client that vanished mid-handshake
# raised SSLError there -- and SSLError is an OSError, which the accept loop
# treated as "the listener is gone" and exited. Macast then stayed advertised
# over mDNS while nothing answered on 8009: discoverable, not castable.
_CTX.renderer = MockRenderer()
p2 = TestProtocol()
try:
    p2.start()

    def _connect_cast(timeout=5):
        raw = socket.create_connection(("127.0.0.1", p2.cast_port), timeout=timeout)
        return ctx.wrap_socket(raw, server_hostname=None)

    for _ in range(3):
        socket.create_connection(("127.0.0.1", p2.cast_port), timeout=5).close()
    time.sleep(0.5)

    with _connect_cast() as s:
        cast_send(s, "sender-0", "receiver-0",
                  "urn:x-cast:com.google.cast.tp.deviceauth",
                  json.dumps({"type": "CHALLENGE"}))
        reply = cast_recv(s)
    check("aborted handshakes do not kill the receiver", reply is not None,
          "no reply after 3 stray connections")

    # An interface change cycles the protocol; the receiver must come back.
    p2.stop()
    p2.start()
    with _connect_cast() as s:
        cast_send(s, "sender-0", "receiver-0",
                  "urn:x-cast:com.google.cast.tp.deviceauth",
                  json.dumps({"type": "CHALLENGE"}))
        reply2 = cast_recv(s)
    check("receiver serves again after a stop/start cycle", reply2 is not None,
          "no reply after restart")
except Exception as e:
    check("receiver survives stray connections", False, str(e))
finally:
    try:
        p2.stop()
    except Exception:
        pass

# ---- The app must close when its last sender goes away ----
# Regression: `_session_id` was only ever cleared by an explicit STOP, so a
# sender that disconnected -- or simply vanished, which is what really happens
# when the phone sleeps or VLC is killed -- left the receiver answering
# GET_STATUS with `applications: [{Macast, sessionId: <dead>}]` while mpv sat
# idle. VLC's state machine keys on exactly that field (chromecast_ctrl.cpp,
# processReceiverMessage: "Media receiver application was already running"),
# skips its LAUNCH, connects to the dead transportId and adopts the stale
# session; casting again then appeared to need "stop casting" (or a plugin
# restart) first. A receiver reports `applications: []` once the app is gone,
# and a restart has to be a clean slate.
_CAST_CONN = "urn:x-cast:com.google.cast.tp.connection"
_CAST_RECV = "urn:x-cast:com.google.cast.receiver"


def _cast_msg(namespace, payload, src="sender-0", dst="receiver-0"):
    return {"source_id": src, "destination_id": dst, "namespace": namespace,
            "payload_type": 0, "payload_utf8": json.dumps(payload),
            "payload_binary": b""}


def _receiver_statuses(raw):
    out, off = [], 0
    while off + 4 <= len(raw):
        (n,) = struct.unpack(">I", raw[off:off + 4])
        msg = cast.parse_cast_message(raw[off + 4: off + 4 + n])
        off += 4 + n
        try:
            data = json.loads(msg["payload_utf8"])
        except Exception:
            continue
        if data.get("type") == "RECEIVER_STATUS":
            out.append(data)
    return out


def _apps(msg):
    return ((msg.get("status") or {}).get("applications")) or []


def _launched_proto():
    """A protocol with one sender that has already launched the app."""
    _CTX.renderer = MockRenderer()
    proto = TestProtocol()
    sock = FakeSock()
    proto._on_message(sock, _cast_msg(_CAST_CONN, {"type": "CONNECT"}))
    proto._on_message(sock, _cast_msg(_CAST_RECV, {
        "type": "LAUNCH", "requestId": 1, "appId": "CC1AD845"}))
    return proto, sock


proto, sock = _launched_proto()
check("LAUNCH reports a running app",
      _apps(_receiver_statuses(sock.sent)[-1]) != [],
      str(_receiver_statuses(sock.sent)[-1:]))

sock.sent = b""
proto._on_message(sock, _cast_msg(_CAST_CONN, {"type": "CLOSE"}))
check("CLOSE forgets the session", proto._session_id is None,
      str(proto._session_id))
# What the *next* sender is told is the part that broke casting: the last
# sender is gone by now, so there is nobody left to notify -- the fix shows up
# in the status a fresh sender gets.
proto._on_message(sock, _cast_msg(_CAST_CONN, {"type": "CONNECT"}))
proto._on_message(sock, _cast_msg(_CAST_RECV, {"type": "GET_STATUS",
                                               "requestId": 9}))
statuses = _receiver_statuses(sock.sent)
check("the next sender is told no app is running",
      bool(statuses) and _apps(statuses[-1]) == [], str(statuses[-1:]))

# A STOP must close the app for every sender, not only the one that asked.
proto, asker = _launched_proto()
watcher = FakeSock()
proto._on_message(watcher, _cast_msg(_CAST_CONN, {"type": "CONNECT"},
                                    src="sender-1"))
asker.sent = b""
watcher.sent = b""
proto._on_message(asker, _cast_msg("urn:x-cast:com.google.cast.media",
                                   {"type": "STOP", "requestId": 4,
                                    "mediaSessionId": 1}))
seen = _receiver_statuses(watcher.sent)
check("STOP closes the app for the other senders too",
      bool(seen) and _apps(seen[-1]) == [], str(seen[-1:]))

proto, _ = _launched_proto()
proto.stop()
check("stopping the service forgets the session", proto._session_id is None,
      str(proto._session_id))

# The path VLC actually takes: the socket dies with no CLOSE frame at all.
_CTX.renderer = MockRenderer()
p3 = TestProtocol()
try:
    p3.start()
    s1 = ctx.wrap_socket(socket.create_connection(
        ("127.0.0.1", p3.cast_port), timeout=5), server_hostname=None)
    cast_send(s1, "sender-0", "receiver-0", _CAST_RECV,
              json.dumps({"type": "LAUNCH", "requestId": 1,
                          "appId": "CC1AD845"}))
    apps_before = _apps(json.loads(cast_recv(s1)["payload_utf8"]))
    s1.close()
    deadline = time.time() + 5
    while p3._senders and time.time() < deadline:
        time.sleep(0.05)
    s2 = ctx.wrap_socket(socket.create_connection(
        ("127.0.0.1", p3.cast_port), timeout=5), server_hostname=None)
    cast_send(s2, "sender-0", "receiver-0", _CAST_RECV,
              json.dumps({"type": "GET_STATUS", "requestId": 2}))
    apps_after = _apps(json.loads(cast_recv(s2)["payload_utf8"]))
    s2.close()
    check("a sender that vanishes closes the app too",
          apps_before != [] and apps_after == [],
          "before={} after={}".format(apps_before, apps_after))
except Exception as e:
    check("a sender that vanishes closes the app too", False, str(e))
finally:
    try:
        p3.stop()
    except Exception:
        pass

# ---- AirPlay real RTSP server ----
_CTX.renderer = MockRenderer()
ap = TestAirPlay()
try:
    ap.start()
    s = socket.create_connection(("127.0.0.1", ap.rtsp_port))
    f = s.makefile("rwb")

    def rtsp_request(method, headers, body=""):
        out = "{} rtsp://airplay RTSP/1.0\r\n".format(method)
        for k, v in headers.items():
            out += "{}: {}\r\n".format(k, v)
        out += "Content-Length: {}\r\n\r\n".format(len(body))
        f.write(out.encode())
        if body:
            f.write(body.encode())
        f.flush()
        # read status line + headers until blank line
        line = f.readline().decode()
        while not line.strip():
            line = f.readline().decode()
        while f.readline().decode().strip():
            pass
        return line

    rtsp_request("ANNOUNCE", {"CSeq": "1", "Content-Location": "http://e2e/a.mp4"})
    rtsp_request("SETUP", {"CSeq": "2"})
    rtsp_request("PLAY", {"CSeq": "3", "Session": "1"})
    s.close()
    check("airplay e2e PLAY reached renderer",
          _CTX.renderer.called("set_media_url") and
          _CTX.renderer.last_arg("set_media_url") == "http://e2e/a.mp4",
          str(_CTX.renderer.last_arg("set_media_url")))
except Exception as e:
    e2e_ok = False
    check("airplay e2e (RTSP server)", False, "start/connect failed: {}".format(e))
finally:
    try:
        ap.stop()
    except Exception:
        pass

# 4: mDNS advertisement triggered with correct service/port (populated during
#    the real start() calls in Part 3)
check("chromecast mDNS service type",
      any(s == cast.CAST_SERVICE for s, _, _ in MockAdvertiser.calls), str(MockAdvertiser.calls))
check("chromecast mDNS advertises the port actually bound",
      any(port == p.cast_port for _, _, port in MockAdvertiser.calls),
      "bound={} advertised={}".format(p.cast_port, MockAdvertiser.calls))
check("airplay mDNS service type",
      any(s == airplay.AIRPLAY_SERVICE for s, _, _ in MockAdvertiser.calls))
check("airplay mDNS advertises the port actually bound",
      any(port == ap.rtsp_port for _, _, port in MockAdvertiser.calls),
      "bound={} advertised={}".format(ap.rtsp_port, MockAdvertiser.calls))

# 4b: the advertised instance name must be a legal DNS-SD label.
#
# Regression: Macast's default friendly name is "Macast(Hostname.local)" —
# parentheses and dots included. zeroconf accepts it and reports success, but
# the record never matches a `dns-sd -B _googlecast._tcp` query, so the device
# was advertised yet invisible. It is normalised in discovery.py now.
from macast.discovery import sanitize_instance_name  # noqa: E402

_RAW = "Macast(MyHost-1754.local)"
print("\n=== Part 4b: mDNS instance-name normalisation ===")
_clean = sanitize_instance_name(_RAW + "._googlecast._tcp.local.",
                                "_googlecast._tcp.local.")
check("illegal friendly name is normalised",
      _clean == "Macast-MyHost-1754._googlecast._tcp.local.", _clean)
check("normalised label has only [A-Za-z0-9-]",
      all(c.isalnum() or c == "-" for c in _clean.split(".")[0]),
      _clean.split(".")[0])
check("normalised label <= 63 bytes",
      len(_clean.split(".")[0].encode()) <= 63, str(len(_clean.split(".")[0])))
check("normalisation is idempotent",
      sanitize_instance_name(_clean, "_googlecast._tcp.local.") == _clean)
check("already-legal name is left alone",
      sanitize_instance_name("Macast._airplay._tcp.local.",
                             "_airplay._tcp.local.")
      == "Macast._airplay._tcp.local.")

# 4c: registered services are re-announced, because some senders only listen.
#
# Regression: Macast announced exactly once, at protocol start. VLC Android's
# renderer discovery is passive -- it logs "mDNS: listening to
# _googlecast._tcp.local renderer" and a sniffer on the LAN sees no query from
# the phone -- so once its cache aged out the device was missing from VLC's
# list until the user restarted the plugin (which re-announces). Our records
# carry a 120s TTL, so re-announce well inside it.
print("\n=== Part 4c: mDNS re-announcement ===")


class _FakeZc:
    def __init__(self):
        self.registered, self.unregistered, self.updated = [], [], []

    def register_service(self, info, **kwargs):
        self.registered.append(info)

    def unregister_service(self, info):
        self.unregistered.append(info)

    def update_service(self, info):
        self.updated.append(info)


_fake_zc = _FakeZc()
_real_acquire, _real_release = discovery._acquire_zc, discovery._release_zc
discovery._acquire_zc = lambda: _fake_zc
discovery._release_zc = lambda: None
try:
    _adv = discovery.MDNSAdvertiser()
    _adv.advertise("_googlecast._tcp.local.",
                   "Macast-Test._googlecast._tcp.local.", 8009,
                   {"md": "Macast", "fn": "Macast-Test"})
    check("advertiser registers the service", len(_fake_zc.registered) == 1)
    check("advertiser starts a keepalive thread",
          _adv._thread is not None and _adv._thread.is_alive())
    check("re-announce republishes the registered record",
          _adv.reannounce() == 1 and len(_fake_zc.updated) == 1
          and _fake_zc.updated[0] is _fake_zc.registered[0],
          "updated={}".format(len(_fake_zc.updated)))
    check("re-announce interval stays inside the record TTL",
          discovery.MDNSAdvertiser.REANNOUNCE_SECONDS < 120,
          str(discovery.MDNSAdvertiser.REANNOUNCE_SECONDS))
    _adv.close()
    check("close stops the keepalive thread and withdraws the record",
          len(_fake_zc.unregistered) == 1
          and (_adv._thread is None or not _adv._thread.is_alive()),
          "unregistered={} thread={}".format(
              len(_fake_zc.unregistered), _adv._thread))
finally:
    discovery._acquire_zc, discovery._release_zc = _real_acquire, _real_release

# 4d: the TXT `ca` field must declare *both* video and audio.
#
# VLC parses `ca` as a bitmask -- 0x01 video, 0x04 audio
# (modules/services_discovery/microdns.c) -- and a real Chromecast sends 5.
# Macast sent 1, i.e. "can play video but not audio". Measured on VLC Android
# 3.7.1 this alone does not decide the cast chain (see docs), but claiming the
# wrong capability is a bug in its own right, and other senders do read it.
_CTX.renderer = MockRenderer()
_pca = TestProtocol()
try:
    _pca.start()
    _props = {}
    if _pca._advertiser is not None and _pca._advertiser.advertised:
        _props = _pca._advertiser.advertised[0][3]
    _ca = int(_props.get("ca", "0"))
    check("chromecast TXT ca declares video and audio",
          (_ca & 0x01) != 0 and (_ca & 0x04) != 0, "ca={}".format(_props.get("ca")))
except Exception as e:
    check("chromecast TXT ca declares video and audio", False, str(e))
finally:
    try:
        _pca.stop()
    except Exception:
        pass
check("empty/garbage name falls back to Macast",
      sanitize_instance_name("()()._airplay._tcp.local.",
                             "_airplay._tcp.local.")
      == "Macast._airplay._tcp.local.")

# The SRV target must not end up as "My-Mac.local.local." — macOS hostnames
# already carry the suffix, so it must not be appended twice.
from macast.discovery import _normalize_server  # noqa: E402

check("hostname already ending in .local is not doubled",
      _normalize_server("MyHost-1754.local")
      == "MyHost-1754.local.", _normalize_server("MyHost-1754.local"))
check("bare hostname gets a .local suffix",
      _normalize_server("MyHost") == "MyHost.local.",
      _normalize_server("MyHost"))

# 4c: switching protocols at runtime must re-wire SSDP.
#
# Regression: SSDP was only ever configured for the protocol chosen at launch.
# Switching DLNA -> AirPlay from the menu kept announcing a DLNA renderer that
# no longer answered, and switching back announced nothing.
print("\n=== Part 4c: SSDP follows the active protocol ===")
try:
    server = _load("server", "server.py")

    class _FakePlugin(object):
        def __init__(self, *a, **k):
            self.started = False
            self.unsubscribed = False

        def subscribe(self):
            pass

        def start(self):
            self.started = True

        def stop(self):
            pass

        def unsubscribe(self):
            self.unsubscribed = True

    _real_ssdp_plugin, _real_monitor = server.SSDPPlugin, server.Monitor
    server.SSDPPlugin, server.Monitor = _FakePlugin, _FakePlugin

    class _BareService(server.Service):
        """Service without the port-binding __init__."""

        def __init__(self):
            self.ssdp_plugin = None
            self.ssdp_monitor = None
            self.ssdp_monitor_counter = 0

    svc = _BareService()

    # AirPlay does not use SSDP -> an existing plugin must be torn down.
    svc.ssdp_plugin = _FakePlugin()
    svc.ssdp_monitor = _FakePlugin()
    old_plugin = svc.ssdp_plugin
    svc._sync_ssdp(airplay.AirPlayProtocol())
    check("switching to AirPlay tears SSDP down",
          svc.ssdp_plugin is None and svc.ssdp_monitor is None and
          old_plugin.unsubscribed)

    # ...and switching back to DLNA must bring it up again.
    svc._sync_ssdp(protocol.DLNAProtocol())
    check("switching back to DLNA brings SSDP up",
          svc.ssdp_plugin is not None and svc.ssdp_monitor is not None)

    # Idempotence: re-selecting DLNA must not stack a second plugin.
    again = svc.ssdp_plugin
    svc._sync_ssdp(protocol.DLNAProtocol())
    check("_sync_ssdp is idempotent", svc.ssdp_plugin is again)
except Exception as e:
    check("SSDP re-wiring on protocol switch", False,
          "{}: {}".format(type(e).__name__, e))
finally:
    try:
        server.SSDPPlugin, server.Monitor = _real_ssdp_plugin, _real_monitor
    except Exception:
        pass

# --------------------------------------------------------------------------
# Part 5: the plugin manager must actually expose the new protocols in the menu
# --------------------------------------------------------------------------
print("\n=== Part 5: plugin registration (MacastPluginManager) ===")
try:
    # Stub macast_renderer (the mpv wrapper); only the symbol is needed here.
    _mcr = types.ModuleType("macast_renderer")
    _mcr_mpv = types.ModuleType("macast_renderer.mpv")

    class _DummyMPV(object):
        pass

    _mcr_mpv.MPVRenderer = _DummyMPV
    _mcr.mpv = _mcr_mpv
    sys.modules["macast_renderer"] = _mcr
    sys.modules["macast_renderer.mpv"] = _mcr_mpv

    # pyperclip (clipboard) is a git-fork dependency; stub it if absent.
    try:
        import pyperclip  # noqa: F401
    except ImportError:
        _pc = types.ModuleType("pyperclip")
        _pc.copy = lambda *a, **k: None
        _pc.paste = lambda *a, **k: ""
        sys.modules["pyperclip"] = _pc

    # GUI toolkits (rumps/pystray/PIL) are platform deps we don't need here.
    # A module that answers any attribute with a placeholder class keeps
    # `from rumps import App` style imports working.
    class _AnyModule(types.ModuleType):
        def __getattr__(self, name):
            # Dunder lookups (__path__, __spec__, ...) must behave normally or
            # importlib itself breaks when it touches the stub.
            if name.startswith("__"):
                raise AttributeError(name)
            return object

    for _name in ("rumps", "pystray"):
        try:
            __import__(_name)
        except ImportError:
            sys.modules[_name] = _AnyModule(_name)
    try:
        import PIL.Image  # noqa: F401
    except Exception:
        sys.modules.setdefault("PIL", _AnyModule("PIL"))
        sys.modules.setdefault("PIL.Image", _AnyModule("PIL.Image"))
        sys.modules["PIL"].Image = sys.modules["PIL.Image"]

    macast_mod = _load("macast", "macast.py")
    mgr = macast_mod.MacastPluginManager(
        macast_mod.MacastPlugin(None, "MPV", _DummyMPV(), "darwin,win32,linux"),
        macast_mod.MacastPlugin(None, "DLNA", protocol.DLNAProtocol(),
                                "darwin,win32,linux"),
    )
    titles = [p.title for p in mgr.protocol_list]
    check("plugin manager lists DLNA", "DLNA" in titles, str(titles))
    check("plugin manager lists Chromecast", "Chromecast" in titles, str(titles))
    check("plugin manager lists AirPlay", "AirPlay" in titles, str(titles))
except Exception as e:
    import traceback
    traceback.print_exc()
    check("plugin manager registers built-in protocols", False, str(e))

# --------------------------------------------------------------------------
# Part 5b: bundled plugins (xfangfang/Macast-plugins, shipped in-tree)
#
# Macast now ships the upstream plugin collection inside the package. Two
# things must hold, and both have already gone wrong once:
#
#   1. Every bundled plugin must declare a readable <macast.*> manifest. The
#      loader trusts the manifest to decide *whether to import the module*, so
#      a typo there is no longer survivable -- the upstream PotPlayer header
#      closed its <macast.renderer> and <macast.platform> tags with
#      "</macast.title>", and both values were silently lost.
#   2. A plugin declaring a platform we are not on must not be imported at
#      all. The bundled set includes `import win32api` (module level) and a
#      `sudo pi_fm_rds` launch, so importing them off-platform raises -- and
#      used to take the whole app down while it was just building its list.
# --------------------------------------------------------------------------
print("\n=== Part 5b: bundled plugins ===")
try:
    _renderer_dir = os.path.join(MACAST, "plugins", "renderer")
    _protocol_dir = os.path.join(MACAST, "plugins", "protocol")

    _expected = {
        "iina.py": ("IINA Renderer", "IINARenderer", "darwin"),
        "web.py": ("Web Renderer", "WebRenderer", "darwin,linux,win32"),
        "live.py": ("Live Renderer", "LiveRenderer", "win32,darwin,linux"),
        "potplayer.py": ("PotPlayer Renderer", "PotplayerRenderer", "win32"),
        "pi_fm.py": ("PIFMRDS Renderer", "PIFMRenderer", "linux"),
    }
    for _fname, (_title, _cls, _plat) in sorted(_expected.items()):
        _meta = macast_mod._read_plugin_metadata(os.path.join(_renderer_dir, _fname))
        check("manifest of {} parses".format(_fname), bool(_meta.get("title")),
              str(_meta))
        check("{} declares its title".format(_fname), _meta.get("title") == _title,
              repr(_meta.get("title")))
        check("{} declares its class".format(_fname),
              _meta.get("renderer") == _cls, repr(_meta.get("renderer")))
        check("{} declares its platform".format(_fname),
              _meta.get("platform") == _plat, repr(_meta.get("platform")))

    _nva = macast_mod._read_plugin_metadata(
        os.path.join(_protocol_dir, "nirvana.py"))
    check("nirvana manifest declares a protocol class",
          _nva.get("protocol") == "NVAProtocol", repr(_nva.get("protocol")))
    check("nirvana manifest declares its platform",
          _nva.get("platform") == "darwin,win32,linux", repr(_nva.get("platform")))

    # Hidden imports put bytecode in PyInstaller's PYZ archive, but the loader
    # discovers plugins by scanning real .py files before importing them. Linux
    # and Windows builds therefore need both the modules and the manifest tree.
    with open(os.path.join(REPO, ".github", "workflows", "build.yml"),
              encoding="utf-8") as _f:
        _build5b = _f.read()
    _bundled5b = []
    for _kind5b, _dir5b in (("renderer", _renderer_dir),
                            ("protocol", _protocol_dir)):
        for _entry5b in sorted(os.listdir(_dir5b)):
            if _entry5b.endswith(".py") and _entry5b != "__init__.py":
                _bundled5b.append("macast.plugins.{}.{}".format(
                    _kind5b, _entry5b[:-3]))
    _missing_hidden5b = [
        _module5b for _module5b in _bundled5b
        if _build5b.count("--hidden-import={}".format(_module5b)) != 3
    ]
    check("every PyInstaller target includes every bundled plugin module",
          not _missing_hidden5b, repr(_missing_hidden5b))
    check("every PyInstaller target ships the scannable plugin manifests",
          _build5b.count('--add-data "macast/plugins:macast/plugins"') == 2
          and _build5b.count('--add-data "macast/plugins;macast/plugins"') == 1,
          "unix={}, windows={}".format(
              _build5b.count('--add-data "macast/plugins:macast/plugins"'),
              _build5b.count('--add-data "macast/plugins;macast/plugins"')))

    # A bundled plugin for another OS must be *registered* (so the settings
    # page can grey it out) while its module is never imported. The fixtures
    # are injected where the loader looks for them, so this needs no real
    # plugin file on disk.
    class _FixtureRenderer(object):
        pass

    _valid_module = types.ModuleType("macast.plugins.renderer._valid_fixture")
    _valid_module.FixtureRenderer = _FixtureRenderer

    _fixture_sources = {
        "_valid_fixture.py": (
            "# <macast.title>Fixture Renderer</macast.title>\n"
            "# <macast.renderer>FixtureRenderer</macast.renderer>\n"
            "# <macast.platform>darwin,win32,linux</macast.platform>\n"
            "# <macast.version>7.7</macast.version>\n"
            "# <macast.desc>fixture</macast.desc>\n"),
        "_foreign_fixture.py": (
            "# <macast.title>Foreign Renderer</macast.title>\n"
            "# <macast.renderer>ForeignRenderer</macast.renderer>\n"
            "# <macast.platform>plan9</macast.platform>\n"),
    }
    _fixture_dir = _tempfile.mkdtemp(prefix="macast-bundled-")
    try:
        for _name, _body in _fixture_sources.items():
            with open(os.path.join(_fixture_dir, _name), "w", encoding="utf-8") as _f:
                _f.write(_body)

        # `_import_bundled_module` consults sys.modules first and only then
        # importlib, so a mock here sees exactly the modules the loader really
        # tries to execute -- and nothing else.
        _imported = []
        _real_import_module = macast_mod.importlib.import_module

        def _spy_import(dotted, *a, **k):
            _imported.append(dotted)
            if dotted.endswith("_valid_fixture"):
                return _valid_module
            if dotted.endswith("_foreign_fixture"):
                raise AssertionError(
                    "the loader tried to import a foreign-platform plugin: "
                    + dotted)
            return _real_import_module(dotted, *a, **k)

        _probe = macast_mod.MacastPluginManager.__new__(macast_mod.MacastPluginManager)
        macast_mod.importlib.import_module = _spy_import
        try:
            _loaded = _probe._load_bundled_plugins("renderer", directory=_fixture_dir)
        finally:
            macast_mod.importlib.import_module = _real_import_module

        check("foreign-platform plugin module is never imported",
              not any("_foreign_fixture" in d for d in _imported), str(_imported))
        check("matching-platform plugin module is imported exactly once",
              _imported.count("macast.plugins.renderer._valid_fixture") == 1,
              str(_imported))

        _titles = sorted(p.title for p in _loaded)
        check("bundled loader registers every fixture",
              _titles == ["Fixture Renderer", "Foreign Renderer"], str(_titles))

        _foreign = [p for p in _loaded if p.title == "Foreign Renderer"][0]
        check("foreign-platform plugin has no class to build",
              _foreign.plugin_factory is None, repr(_foreign.plugin_factory))
        check("foreign-platform plugin reports available=False",
              _foreign.get_info()["available"] is False)
        check("foreign-platform plugin is still listed as a bundled default",
              _foreign.kind() == "renderer" and _foreign.get_info()["default"] is True)

        _valid = [p for p in _loaded if p.title == "Fixture Renderer"][0]
        check("matching-platform plugin is instantiable",
              _valid.get_instance() is not None)
        check("bundled plugin is flagged default and not uninstallable",
              _valid.get_info()["default"] is True
              and _valid.get_info()["can_uninstall"] is False)
        check("bundled plugin reports its own version, not the app version",
              _valid.get_info()["version"] == "7.7",
              repr(_valid.get_info()["version"]))
    finally:
        _shutil.rmtree(_fixture_dir, ignore_errors=True)
        sys.modules.pop("macast.plugins.renderer._valid_fixture", None)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("bundled plugins behave", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 5c: the plugin index moved off the upstream collection repo
#
# Every plugin xfangfang/Macast-plugins publishes is bundled in-tree now, so
# the settings page reads this fork's own `plugins/info.json` (empty on
# purpose) instead. Three things are easy to break here and all three fail
# silently in the browser, hence the checks:
#
#   1. more than one index host, and in the right order -- raw.githubusercontent
#      is unreachable from mainland China far more often than the jsDelivr
#      mirror that backs it up;
#   2. setting.html must not hardcode a repository address again. That is
#      exactly how every bundled plugin got a duplicate card plus a bogus
#      "update available" badge pointing back upstream;
#   3. the index file must stay loadable and must not re-list anything the app
#      already ships.
# --------------------------------------------------------------------------
_git_available = bool(_shutil.which("git")) and os.path.isdir(
    os.path.join(REPO, ".git"))


def _git_show(sha, path):
    """File contents at `sha`, or None (git missing / unknown commit / no file)."""
    if not _git_available:
        return None
    try:
        result = subprocess.run(["git", "show", "{}:{}".format(sha, path)],
                                capture_output=True, text=True, timeout=15,
                                cwd=REPO)
    except Exception:
        return None
    return result.stdout if result.returncode == 0 else None


print("\n=== Part 5c: plugin index ===")
# The mirror switch lives in Setting; run this whole part against a temp
# config so neither the user's real settings nor leftover state from another
# part can change the expected index ordering (AGENTS §10).
_saved_setting5c = (utils.Setting.setting, utils.Setting.setting_path)
_tmp5c = _tempfile.mkdtemp(prefix="macast-index-")
utils.Setting.setting = {}
utils.Setting.setting_path = os.path.join(_tmp5c, "macast_setting.json")
try:
    repo_mod = _load("plugin_repo", "plugin_repo.py")
    _repo_info = repo_mod.describe()

    check("plugin index describes more than one host to try",
          isinstance(_repo_info.get("index_urls"), list)
          and len(_repo_info["index_urls"]) >= 2, str(_repo_info))
    check("plugin index points at this fork, not the upstream collection",
          all("xfangfang" not in _u for _u in _repo_info["index_urls"]),
          str(_repo_info["index_urls"]))
    check("plugin index URLs carry both the repo and the index path",
          all(repo_mod.REPO in _u and repo_mod.INDEX_PATH in _u
              for _u in _repo_info["index_urls"]), str(_repo_info["index_urls"]))
    check("raw.githubusercontent is tried before the mirror",
          _repo_info["index_urls"][0].startswith("https://raw.githubusercontent.com/"),
          _repo_info["index_urls"][0])
    # jsDelivr caches branch files for hours, so an index served from it can be
    # stale. It is the last resort, never the first choice, and something that
    # proxies raw on demand has to come before it.
    check("something fresher than the caching CDN is tried before it",
          "jsdelivr" not in str(_repo_info["index_urls"][:-1]),
          str(_repo_info["index_urls"]))
    check("the caching CDN is only the last resort",
          "jsdelivr" in _repo_info["index_urls"][-1],
          _repo_info["index_urls"][-1])
    check("the repository button has somewhere to go",
          _repo_info.get("repo_url", "").startswith("https://github.com/" + repo_mod.REPO),
          repr(_repo_info.get("repo_url")))

    # -- 「启用国内镜像地址」（Github_CN_Mirror）-----------------------------
    # One switch rewrites every GitHub URL the app fetches; the rewrite rules
    # are pure functions here, the wiring to the fetch sites is source-checked
    # below and exercised end-to-end in Parts 7 and 12.
    check("the mirror switch is off until the user says otherwise",
          repo_mod.mirror_enabled() is False and
          'Github_CN_Mirror' not in utils.Setting.setting,
          repr(utils.Setting.setting))
    check("github.com urls mirror onto the domestic prefix",
          repo_mod.to_mirror_url("https://github.com/a/b") ==
          "https://ghproxy.net/https://github.com/a/b")
    check("raw.githubusercontent urls mirror too",
          repo_mod.to_mirror_url("https://raw.githubusercontent.com/a/b") ==
          "https://ghproxy.net/https://raw.githubusercontent.com/a/b")
    check("api.github.com urls mirror too",
          repo_mod.to_mirror_url("https://api.github.com/repos/a/b") ==
          "https://ghproxy.net/https://api.github.com/repos/a/b")
    check("jsDelivr urls are already a reachable mirror and stay put",
          repo_mod.to_mirror_url("https://cdn.jsdelivr.net/gh/a@b/c.py") ==
          "https://cdn.jsdelivr.net/gh/a@b/c.py")
    check("non-GitHub urls are never rewritten",
          repo_mod.to_mirror_url("https://existential.audio/x.pkg") ==
          "https://existential.audio/x.pkg")
    check("mirroring is idempotent",
          repo_mod.to_mirror_url(repo_mod.to_mirror_url("https://github.com/a/b")) ==
          "https://ghproxy.net/https://github.com/a/b")

    repo_mod.set_mirror_enabled(True)
    check("enabling persists exactly one settings key",
          utils.Setting.setting.get('Github_CN_Mirror') is True and
          repo_mod.mirror_enabled() is True, repr(utils.Setting.setting))
    _mir_info = repo_mod.describe()
    check("mirror mode keeps the bare raw host out of the index chain",
          all(not _u.startswith("https://raw.githubusercontent.com/")
              for _u in _mir_info["index_urls"]), str(_mir_info["index_urls"]))
    check("mirror mode still tries more than one host",
          len(_mir_info["index_urls"]) >= 2, str(_mir_info["index_urls"]))
    check("mirror mode keeps the caching CDN last",
          "jsdelivr" in _mir_info["index_urls"][-1], _mir_info["index_urls"][-1])
    check("mirror mode mirrors the repository button as well",
          _mir_info["repo_url"].startswith("https://ghproxy.net/https://github.com/"),
          _mir_info["repo_url"])
    check("plugin-info tells the page the mirror is on",
          _mir_info["mirror_enabled"] is True, repr(_mir_info))
    check("with the mirror on, GitHub fetch urls are rewritten",
          repo_mod.mirror_url("https://github.com/x/y/raw/p.py") ==
          "https://ghproxy.net/https://github.com/x/y/raw/p.py")
    repo_mod.set_mirror_enabled(False)
    check("disabling removes the key instead of persisting False",
          'Github_CN_Mirror' not in utils.Setting.setting and
          repo_mod.mirror_enabled() is False, repr(utils.Setting.setting))
    check("the canonical fresh-first index chain returns with the mirror off",
          repo_mod.index_urls()[0].startswith("https://raw.githubusercontent.com/"),
          repo_mod.index_urls()[0])

    _index_file = os.path.join(REPO, repo_mod.INDEX_PATH)
    check("the plugin index lives in the repo", os.path.isfile(_index_file),
          _index_file)
    with open(_index_file, "r", encoding="utf-8") as _f:
        _index = json.load(_f)
    check("the plugin index has the fields the settings page reads",
          isinstance(_index.get("plugin_v1"), list)
          and _index.get("repo_url", "").startswith("https://github.com/"),
          str(sorted(_index.keys())))

    # Nothing offered for install may be a plugin the app already bundles --
    # that is the duplicate-card failure mode this whole part exists for.
    try:
        _read_manifest = macast_mod._read_plugin_metadata
    except NameError:
        _read_manifest = _load("macast", "macast.py")._read_plugin_metadata
    _bundled_titles = set()
    for _kind in ("renderer", "protocol"):
        _kind_dir = os.path.join(MACAST, "plugins", _kind)
        for _fname in sorted(os.listdir(_kind_dir)):
            if _fname.endswith(".py") and not _fname.startswith("__"):
                _bundled_titles.add(
                    _read_manifest(os.path.join(_kind_dir, _fname)).get("title"))
    _offered = [e.get("title") for e in _index["plugin_v1"]]
    check("the index never re-lists a bundled plugin",
          not (set(_offered) & _bundled_titles),
          "offered={} bundled={}".format(sorted(set(_offered)),
                                         sorted(_bundled_titles)))
    # A filled-in entry has to be installable by the page as-is.
    for _entry in _index["plugin_v1"]:
        check("index entry {} carries every field the page needs".format(
                  _entry.get("title")),
              # The query string is stripped first, exactly as install_url does
              # (`os.path.basename(url.split('?')[0])`): a cache-busting ?v= is
              # legitimate on a .py url.
              bool(_entry.get("title") and _entry.get("version")
                   and _entry.get("platform")
                   and _entry.get("url", "").split("?")[0].endswith(".py"))
              and _entry.get("type") in ("renderer", "protocol")
              and bool(_entry.get("renderer") or _entry.get("protocol")),
              str(_entry))

    # An entry and the file it installs must agree, or the page shows one
    # version and installs another -- the kind of drift that only shows up as
    # "the update did nothing".
    for _entry in _index["plugin_v1"]:
        _fname = _entry.get("url", "").split("?")[0].rsplit("/", 1)[-1]
        _fpath = os.path.join(REPO, "plugins", _fname)
        check("index entry {} points at a file in this repo".format(_entry.get("title")),
              os.path.isfile(_fpath), _fpath)
        if not os.path.isfile(_fpath):
            continue
        _bundled = _read_manifest(_fpath)
        for _key in ("title", "version", "platform"):
            check("{} manifest and index agree on {}".format(_fname, _key),
                  _bundled.get(_key) == _entry.get(_key),
                  "manifest={!r} index={!r}".format(_bundled.get(_key),
                                                    _entry.get(_key)))
        _class_key = "protocol" if _entry.get("type") == "protocol" else "renderer"
        check("{} manifest and index agree on the class name".format(_fname),
              _bundled.get(_class_key) == _entry.get(_class_key),
              "manifest={!r} index={!r}".format(_bundled.get(_class_key),
                                                _entry.get(_class_key)))
        # The install url is pinned to a commit SHA, never a branch. Every CDN
        # in front of this was measured caching branch files for far longer than
        # they admit (jsDelivr hours, and `?v=` does not bust it; a raw proxy
        # answered stale minutes after a push, past its own max-age=300). The
        # content behind a SHA never changes, so a cache of any age is correct.
        _url = _entry.get("url", "")
        _pin = ""
        if "@" in _url:
            _pin = _url.split("@", 1)[1].split("/", 1)[0]
        check("{} install url is pinned to a commit, not a branch".format(_fname),
              len(_pin) == 40 and all(c in "0123456789abcdef" for c in _pin),
              _url)
        if len(_pin) == 40 and _git_available:
            _pinned = _git_show(_pin, "plugins/" + _fname)
            check("{} commit {} really contains that plugin".format(_fname, _pin[:8]),
                  bool(_pinned), _url)
            if _pinned:
                _tmp_pin = os.path.join(_tempfile.mkdtemp(prefix="macast-pin-"), _fname)
                try:
                    with open(_tmp_pin, "w", encoding="utf-8") as _f:
                        _f.write(_pinned)
                    _pinned_meta = _read_manifest(_tmp_pin)
                finally:
                    _shutil.rmtree(os.path.dirname(_tmp_pin), ignore_errors=True)
                for _key in ("title", "version", "platform"):
                    check("{} pinned content matches the index ({})".format(_fname, _key),
                          _pinned_meta.get(_key) == _entry.get(_key),
                          "pinned={!r} index={!r}".format(_pinned_meta.get(_key),
                                                          _entry.get(_key)))

    _html_path = os.path.join(MACAST, "xml", "setting.html")
    with open(_html_path, "r", encoding="utf-8") as _f:
        _html = _f.read()
    _upstream = [ln.strip() for ln in _html.splitlines() if "Macast-plugins" in ln]
    check("the settings page no longer reads the upstream plugin repo",
          not _upstream, str(_upstream[:2]))
    check("the settings page takes the index URLs from the backend",
          "index_urls" in _html and "plugin_repo" in _html)
    check("the settings page hides the repository button without a URL",
          'v-if="repo_url"' in _html)
    check("the mirror prefix is a backend fact, not page folklore",
          "ghproxy" not in _html)
    check("the settings page busts its own HTTP cache when fetching the index",
          "Date.now()" in _html)
    with open(os.path.join(MACAST, "protocol.py"), "r", encoding="utf-8") as _f:
        _proto_src = _f.read()
    check("plugin-info hands the index coordinates to the page",
          "plugin_repo.describe()" in _proto_src)
    check("the mirror toggle is a gated management endpoint",
          "set-github-mirror" in _proto_src[
              _proto_src.index("_MANAGEMENT_PARAMS = ("):
              _proto_src.index(")", _proto_src.index("_MANAGEMENT_PARAMS = ("))])
    with open(os.path.join(MACAST, "macast.py"), "r", encoding="utf-8") as _f:
        _macast_src = _f.read()
    check("plugin downloads honour the mirror switch",
          "url = plugin_repo.mirror_url(url)" in _macast_src)
    check("the update check honours the mirror switch",
          "release_url = plugin_repo.mirror_url(" in _macast_src and
          "api_url = plugin_repo.mirror_url(" in _macast_src)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("plugin index behaves", False, "{}: {}".format(type(e).__name__, e))
finally:
    utils.Setting.setting, utils.Setting.setting_path = _saved_setting5c
    _shutil.rmtree(_tmp5c, ignore_errors=True)

# --------------------------------------------------------------------------
# Part 6: running several protocols at once (ProtocolGroup)
#
# Macast historically ran ONE protocol; the group lets DLNA + Chromecast +
# AirPlay advertise simultaneously while looking like a single Protocol to
# everything downstream.
# --------------------------------------------------------------------------
print("\n=== Part 6: concurrent protocols (ProtocolGroup) ===")
try:
    group_mod = _load("protocol_group", "protocol_group.py")
    ProtocolGroup = group_mod.ProtocolGroup

    class _Rec(object):
        """Minimal Protocol stand-in that records calls."""

        def __init__(self, name, uses_ssdp=False):
            self.name = name
            self.uses_ssdp = uses_ssdp
            self.started = False
            self.stopped = False
            self.state = []
            self.handler = "handler-of-" + name

        def start(self):
            self.started = True

        def stop(self):
            self.stopped = True

        def reload(self):
            self.stop()
            self.start()

        def methods(self):
            return ['set_state_play', 'set_state_stop']

        def set_state_play(self):
            self.state.append('play')
            return self.name

        def unique(self):
            return "unique-" + self.name

    dlna = _Rec("DLNA", uses_ssdp=True)
    cc = _Rec("Chromecast")
    ap2 = _Rec("AirPlay")
    g = ProtocolGroup([("AirPlay", ap2), ("Chromecast", cc), ("DLNA", dlna)])

    check("group holds every child", g.titles() == ["AirPlay", "Chromecast", "DLNA"],
          str(g.titles()))
    check("group is a Protocol", isinstance(g, protocol.Protocol))

    # start/stop must reach ALL children
    g.start()
    check("start() starts every child",
          dlna.started and cc.started and ap2.started)
    g.stop()
    check("stop() stops every child",
          dlna.stopped and cc.stopped and ap2.stopped)

    # uses_ssdp is true if ANY child wants it
    check("uses_ssdp true when DLNA present", g.uses_ssdp is True)
    g.remove("DLNA")
    check("uses_ssdp false without DLNA", g.uses_ssdp is False, str(g.uses_ssdp))
    # Insertion order was AirPlay, Chromecast, DLNA -> AirPlay is first left.
    check("primary falls back to the first child with no DLNA",
          g.primary is ap2, getattr(g.primary, "name", None))

    # ...and comes back when DLNA is re-added
    g.add("DLNA", dlna)
    check("primary prefers the SSDP protocol", g.primary is dlna)
    check("handler comes from the primary", g.handler == "handler-of-DLNA")

    # A protocol that extends another's handler must win the tree root even
    # when it was added later. The NVA protocol subclasses DLNAHandler to add
    # its own endpoints; picking by insertion order mounted DLNA's handler and
    # made every NVA route unreachable with no error anywhere.
    class _Extended(_Rec):
        handler_priority = 10

    nva = _Extended("NVA", uses_ssdp=True)
    g.add("NVA", nva)
    check("an extended handler wins over plain insertion order",
          g.primary is nva, getattr(g.primary, "name", None))
    check("the richest handler is the one mounted",
          g.handler == "handler-of-NVA", str(g.handler))
    g.remove("NVA")
    check("primary falls back when the extended handler is removed",
          g.primary is dlna, getattr(g.primary, "name", None))

    # set_state_* fan-out: Renderer resolves ONE protocol via the bus, so
    # without this only one protocol would ever learn about playback.
    dlna.state, cc.state, ap2.state = [], [], []
    g.set_state_play()
    check("set_state_* reaches every child",
          dlna.state == ['play'] and cc.state == ['play'] and ap2.state == ['play'],
          "dlna={} cc={} ap={}".format(dlna.state, cc.state, ap2.state))

    # unknown attributes delegate to the primary
    check("unknown attributes delegate to primary", g.unique() == "unique-DLNA")
    try:
        g.definitely_missing
        raised = False
    except AttributeError:
        raised = True
    check("truly missing attributes raise AttributeError", raised)

    # one broken child must not sink the others
    class _Boom(_Rec):
        def start(self):
            raise RuntimeError("boom")

    boom = _Boom("Boom")
    g2 = ProtocolGroup([("DLNA", dlna), ("Boom", boom)])
    try:
        g2.start()
        survived = True
    except RuntimeError:
        survived = False
    check("a failing child does not break the group", survived)

    # empty group: no children, no primary, still no crash
    empty = ProtocolGroup()
    check("empty group has no primary", empty.primary is None)
    check("empty group still exposes a handler", empty.handler is not None)
    check("empty group does not need SSDP", empty.uses_ssdp is False)
    empty.start()
    empty.stop()
    check("empty group start/stop is safe", True)

    # ----------------------------------------------------------------------
    # The state ledger must survive being wrapped in a group.
    #
    # Regression: `Protocol` declares get_state / get_state_* / set_state as
    # no-op stubs, so `group.get_state(...)` resolved on the base class and
    # never reached DLNAProtocol -- the only child that actually keeps a
    # ledger. /api?query=status therefore reported an empty title, no volume
    # and 0:00:00 forever, and the subtitle toggle was stuck reading `''`.
    # `_Rec` above cannot catch this: it does not inherit from Protocol, so
    # attribute lookup always falls through to __getattr__. That is precisely
    # why the bug shipped.
    # ----------------------------------------------------------------------
    ledger = protocol.DLNAProtocol()
    ledger.set_state('TransportState', 'PLAYING')
    ledger.set_state('CurrentTrackTitle', 'a real title')
    ledger.set_state('CurrentMediaDuration', '00:01:30')
    ledger.set_state('Volume', 0)              # 0 is an answer, not "unknown"
    ledger.set_state('DisplayCurrentSubtitle', False)

    gl = ProtocolGroup([("DLNA Protocol", ledger)])
    check("group forwards get_state to the protocol holding state",
          gl.get_state('TransportState') == 'PLAYING',
          repr(gl.get_state('TransportState')))
    check("group forwards get_state_transport_state",
          gl.get_state_transport_state() == 'PLAYING',
          repr(gl.get_state_transport_state()))
    check("group forwards get_state_title",
          gl.get_state_title() == 'a real title', repr(gl.get_state_title()))
    check("group forwards get_state_duration",
          gl.get_state_duration() == '00:01:30', repr(gl.get_state_duration()))
    check("a real 0 / False is not mistaken for 'unknown'",
          gl.get_state_volume() == 0 and gl.get_state_display_subtitle() is False,
          "{!r} / {!r}".format(gl.get_state_volume(),
                               gl.get_state_display_subtitle()))

    gl.set_state('CurrentTrackTitle', 'written through the group')
    check("Renderer.set_state reaches the ledger through the group",
          ledger.get_state('CurrentTrackTitle') == 'written through the group',
          repr(ledger.get_state('CurrentTrackTitle')))

    # With DLNA off there is no ledger at all; the group must still answer a
    # sender-facing read instead of the base-class stub's hardcoded 'STOPPED'.
    check("state is readable with DLNA not the primary child",
          ProtocolGroup([("Chromecast", ledger)]).get_state_transport_state()
          == 'PLAYING')

    # A subscriber registered on the child must show up on the group: this is
    # the settings page's "client information" table.
    class _Sub(object):
        host, url, service = '10.1.2.3', 'http://10.1.2.3/notify', 'AVTransport'
        sid, path, timeout = 'uuid:deadbeef', '/event', 1800

    ledger.event_subscribes['uuid:deadbeef'] = _Sub()
    check("group exposes every child's event subscribers",
          'uuid:deadbeef' in gl.event_subscribes, str(list(gl.event_subscribes)))
    check("subscriber fields reach the status page intact",
          gl.event_subscribes['uuid:deadbeef'].service == 'AVTransport')
    check("group reports no subscribers when nothing subscribed",
          ProtocolGroup().event_subscribes == {})

    # ----------------------------------------------------------------------
    # A subscriber must enter the client list without waiting for playback to
    # change something.
    #
    # Regression: `append_device_queue` was drained *only* from inside
    # send_states_to_clients, which the event loop reached only when
    # `state_queue` was non-empty. Playback position is deliberately not an
    # observed state (DLNA clients poll GetPositionInfo for it), so while a
    # stream sat there playing nothing was ever queued and the first
    # subscriber of every session stayed pending forever -- the settings page's
    # "client information" table showed "no clients".
    # ----------------------------------------------------------------------
    def _pending_subscriber_is_registered(drain):
        sub = protocol.DLNAProtocol()
        sub.add_subscribe('AVTransport', 'http://127.0.0.1:9/notify', 1800)
        if sub.append_device_queue.qsize() != 1:
            return False, "subscribe did not queue a client"
        # No observed state has changed: exactly what the event loop sees while
        # a stream plays.
        drain(sub)
        return len(sub.event_subscribes) == 1, \
            "event_subscribes={} pending={}".format(
                list(sub.event_subscribes), sub.append_device_queue.qsize())

    ok, detail = _pending_subscriber_is_registered(
        lambda s: s._sync_subscribe_list())
    check("a subscriber is registered without any state change", ok, detail)
    ok, detail = _pending_subscriber_is_registered(
        lambda s: s.send_states_to_clients({'TransportState': 'PLAYING'}))
    check("a subscriber is registered when a state does change", ok, detail)

    # The reverse has to hold too, or a disconnected client lingers until the
    # next broadcast -- which may never come.
    sub = protocol.DLNAProtocol()
    sub.add_subscribe('AVTransport', 'http://127.0.0.1:9/notify', 1800)
    sub._sync_subscribe_list()
    sid = list(sub.event_subscribes)[0]
    sub.remove_subscribe(sid)
    sub._sync_subscribe_list()
    check("a removed subscriber is gone after the drain",
          sid not in sub.event_subscribes and sub.removed_device_queue.qsize() == 0,
          "subs={} pending={}".format(list(sub.event_subscribes),
                                      sub.removed_device_queue.qsize()))

    # An expired subscriber must be reaped by the periodic pass, not only when
    # a broadcast happens.
    sub = protocol.DLNAProtocol()
    sub.add_subscribe('AVTransport', 'http://127.0.0.1:9/notify', 0)
    sub._sync_subscribe_list()
    check("a zero-timeout subscriber registers", len(sub.event_subscribes) == 1)
    time.sleep(1.1)
    sub._reap_timed_out_clients()
    sub._sync_subscribe_list()
    check("an expired subscriber is reaped by the periodic pass",
          sub.event_subscribes == {} and sub.removed_device_queue.qsize() == 0,
          "subs={} pending={}".format(list(sub.event_subscribes),
                                      sub.removed_device_queue.qsize()))
except Exception as e:
    import traceback
    traceback.print_exc()
    check("ProtocolGroup behaves as a single Protocol", False, str(e))

# --------------------------------------------------------------------------
# Part 7: one player, one owner (PlaybackGuard)
# --------------------------------------------------------------------------
print("\n=== Part 7: playback ownership across protocols ===")
try:
    class _Player(object):
        def __init__(self):
            self.urls = []

        def set_media_url(self, url, **kw):
            self.urls.append(url)
            return "ok"

        def set_media_pause(self):
            return "paused"

    class _Owner(_Rec):
        def __init__(self, name):
            super(_Owner, self).__init__(name)
            self.released = 0

        def release_playback(self):
            self.released += 1

    player = _Player()
    first, second = _Owner("First"), _Owner("Second")
    protocol.PlaybackGuard.owner = None

    guard = protocol.PlaybackGuard(first, player)
    ret = guard.set_media_url("http://a/1.mp4")
    check("guard forwards to the real renderer", player.urls == ["http://a/1.mp4"])
    check("guard passes through the return value", ret == "ok")
    check("guard forwards unrelated calls", guard.set_media_pause() == "paused")

    # First takes over nothing (no previous owner) -> no release
    check("no release on the first playback", first.released == 0)

    protocol.PlaybackGuard(first, player).set_media_url("http://a/2.mp4")
    check("same owner keeps ownership without self-release", first.released == 0)

    protocol.PlaybackGuard(second, player).set_media_url("http://b/1.mp4")
    check("previous owner is released on takeover", first.released == 1)
    check("new owner's URL reaches the player", player.urls[-1] == "http://b/1.mp4")

    # A broken release must never block playback.
    class _BadRelease(_Owner):
        def release_playback(self):
            raise RuntimeError("cannot release")

    bad = _BadRelease("Bad")
    protocol.PlaybackGuard(bad, player).set_media_url("http://c/1.mp4")
    protocol.PlaybackGuard(first, player).set_media_url("http://c/2.mp4")
    check("a failing release does not block playback",
          player.urls[-1] == "http://c/2.mp4", str(player.urls))
    protocol.PlaybackGuard.owner = None
except Exception as e:
    import traceback
    traceback.print_exc()
    check("PlaybackGuard arbitrates concurrent senders", False, str(e))

# --------------------------------------------------------------------------
# Part 8: persisted protocol names resolve to real plugins
#
# Regression: the default DLNA plugin is titled from its class name
# ("DLNA Protocol"), so a stored value of "DLNA" matched nothing and silently
# dropped DLNA -- taking SSDP discovery down with it, with no error shown.
# --------------------------------------------------------------------------
print("\n=== Part 8: protocol title resolution ===")
try:
    mgr_t = macast_mod.MacastPluginManager(
        macast_mod.MacastPlugin(None, "MPV", _DummyMPV(), "darwin,win32,linux"),
        macast_mod.MacastPlugin(
            None, utils.format_class_name(protocol.DLNAProtocol()),
            protocol.DLNAProtocol(), "darwin,win32,linux"),
    )
    real_titles = [p.title for p in mgr_t.protocol_list]
    check("default DLNA plugin keeps the class-derived title",
          "DLNA Protocol" in real_titles, str(real_titles))

    check("short 'DLNA' resolves despite the real title differing",
          mgr_t.resolve_title("DLNA") == "DLNA Protocol",
          str(mgr_t.resolve_title("DLNA")))
    check("exact title still resolves",
          mgr_t.resolve_title("DLNA Protocol") == "DLNA Protocol")
    check("case/space variants resolve",
          mgr_t.resolve_title("dlna  protocol") == "DLNA Protocol",
          str(mgr_t.resolve_title("dlna  protocol")))
    check("built-in protocols resolve",
          mgr_t.resolve_title("Chromecast") == "Chromecast" and
          mgr_t.resolve_title("AirPlay") == "AirPlay")
    check("unknown names resolve to None",
          mgr_t.resolve_title("Nonexistent") is None)

    instance = mgr_t.get_protocol_instance("DLNA")
    check("get_protocol_instance works from the short name",
          instance is not None and isinstance(instance, protocol.DLNAProtocol),
          type(instance).__name__)

    group_all = mgr_t.build_protocol_group(["DLNA", "Chromecast", "AirPlay"])
    check("group built from short names stores canonical titles",
          group_all.titles() == ["DLNA Protocol", "Chromecast", "AirPlay"],
          str(group_all.titles()))
    check("group containing DLNA needs SSDP", group_all.uses_ssdp is True)

    # Unknown entries must be skipped, not crash the app.
    group_partial = mgr_t.build_protocol_group(["DLNA", "Bogus"])
    check("unknown entries are skipped", group_partial.titles() == ["DLNA Protocol"],
          str(group_partial.titles()))

    # An empty/absent list must never leave nothing listening.
    group_empty = mgr_t.build_protocol_group([])
    check("empty selection falls back to DLNA", len(group_empty) == 1 and
          group_empty.uses_ssdp is True, str(group_empty.titles()))
except Exception as e:
    import traceback
    traceback.print_exc()
    check("protocol titles resolve robustly", False, str(e))

# --------------------------------------------------------------------------
# Part 6: mDNS must only publish addresses other devices can actually reach
#
# Regression: the device was discoverable but casting always failed. Cause was
# that every address on a "gateway-bearing" interface was published as an A
# record -- on a machine with VMs/Tailscale that is 5 addresses of which 4 are
# unreachable, so senders resolved the device and then connected to a dead end.
# --------------------------------------------------------------------------
print("\n--- Part 6: mDNS address selection ---")
try:
    # The filter moved to utils so the network picker and the advertiser share
    # one definition of "reachable"; discovery re-exports it.
    is_ok = discovery.is_advertisable_address
    check("loopback is not advertised", not is_ok("127.0.0.1", "255.0.0.0"))
    check("link-local is not advertised", not is_ok("169.254.1.2", "255.255.0.0"))
    check("point-to-point (/32) is not advertised",
          not is_ok("100.85.176.107", "255.255.255.255"))
    check("Tailscale CGNAT is not advertised",
          not is_ok("100.64.0.1", "255.255.255.0"))
    check("normal LAN address is advertised", is_ok("192.168.1.6", "255.255.255.0"))

    # Now the real selection, against a network that mimics this host:
    # en0 is the only interface with an IPv4 default route; the rest are VM
    # bridges and a Tailscale utun that Setting.get_ip() would also return.
    import netifaces as _real_ni  # the stub installed at import time
except Exception:
    _real_ni = None

_fake_ni = types.ModuleType("netifaces")
_fake_ni.AF_INET = 2
_fake_ni.AF_LINK = 17
_fake_ni.gateways = lambda: {
    2: [("192.168.1.1", "en0", True)],
    17: [("link#25", "utun4", False), ("link#27", "bridge100", False)],
}
_FAKE_TABLE = {
    "en0": {2: [{"addr": "192.168.1.6", "netmask": "255.255.255.0",
                 "broadcast": "192.168.1.255"}]},
    "bridge100": {2: [{"addr": "192.168.139.3", "netmask": "255.255.254.0"}]},
    "utun4": {2: [{"addr": "100.85.176.107", "netmask": "255.255.255.255"}]},
}


def _fake_ifaddresses(name):
    # Real netifaces raises ValueError for an unknown interface; matching that
    # keeps the caller's error handling under test.
    if name not in _FAKE_TABLE:
        raise ValueError("unknown interface %s" % name)
    return _FAKE_TABLE[name]


_fake_ni.ifaddresses = _fake_ifaddresses
_fake_ni.interfaces = lambda: ["en0", "bridge100", "utun4"]

_saved_ni = sys.modules.get("netifaces")
# utils.py / discovery.py both do `import netifaces as ni`, so `sys.modules`
# alone is not enough: utils binds the module at import time and needs the
# attribute patched too.
_saved_ni_attr = utils.ni
_saved_get_ip = utils.Setting.get_ip
try:
    sys.modules["netifaces"] = _fake_ni
    utils.ni = _fake_ni
    # Setting.get_ip() is what the old code used; make it return the noisy set
    # so the test proves the new code does better than "just use get_ip()".
    utils.Setting.get_ip = staticmethod(lambda: {
        ("192.168.1.6", "255.255.255.0"),
        ("192.168.139.3", "255.255.254.0"),
        ("192.168.215.0", "255.255.255.0"),
        ("100.85.176.107", "255.255.255.255"),
    })
    picked = sorted(socket.inet_ntoa(a) for a in discovery._local_addresses())
    check("only the default-route address is advertised", picked == ["192.168.1.6"],
          str(picked))

    # --- network picker (see also Part 10) ---------------------------------
    _saved_pin = utils.Setting.get_network_interface
    try:
        utils.Setting.get_network_interface = staticmethod(lambda: "en0")
        check("pinned interface restricts mDNS to that interface",
              discovery._advertisable_interfaces() == {"en0"},
              str(discovery._advertisable_interfaces()))
        pinned_addrs = sorted(socket.inet_ntoa(a)
                              for a in discovery._local_addresses())
        check("pinned interface still yields its own address",
              pinned_addrs == ["192.168.1.6"], str(pinned_addrs))

        # A tunnel with a /32 address cannot carry discovery; pinning it must
        # fall back to automatic rather than make Macast invisible.
        utils.Setting.get_network_interface = staticmethod(lambda: "utun4")
        fallback = sorted(socket.inet_ntoa(a)
                          for a in discovery._local_addresses())
        check("unusable pinned interface falls back to automatic",
              fallback == ["192.168.1.6"], str(fallback))

        # An interface that no longer exists must not break discovery either.
        utils.Setting.get_network_interface = staticmethod(lambda: "en99")
        stale = sorted(socket.inet_ntoa(a) for a in discovery._local_addresses())
        check("stale pinned interface falls back to automatic",
              stale == ["192.168.1.6"], str(stale))
    finally:
        utils.Setting.get_network_interface = _saved_pin
finally:
    utils.Setting.get_ip = _saved_get_ip
    utils.ni = _saved_ni_attr
    if _saved_ni is not None:
        sys.modules["netifaces"] = _saved_ni

# The SRV target must be a hostname we own, so its A records are ours too.
check("SRV target derives from the service instance",
      discovery._normalize_server("Macast-MyHost") == "Macast-MyHost.local.")
check("SRV target is not double-suffixed",
      discovery._normalize_server("MyHost.local") ==
      "MyHost.local.")

# --------------------------------------------------------------------------
# Part 9: device authentication must survive a *strict* protobuf parser
#
# Regression: VLC connected, sent its AuthChallenge, and then hung up with no
# further message -- "found but not castable". VLC's processAuthMessage() bails
# out of the whole handshake when ParseFromString() fails, and C++ protobuf
# validates `required` fields while parsing. The reply used to be a bare
# AuthResponse (0a001200): its `response` submessage existed but was empty, so
# `signature` / `client_auth_certificate` were missing and the parse failed.
# --------------------------------------------------------------------------
print("\n=== Part 9: DeviceAuthMessage wire format ===")
try:
    sys.path.insert(0, os.path.join(REPO, "scripts"))
    import importlib as _importlib
    vlc_sim = _importlib.import_module("vlc_sender_sim")

    ok_new, why_new = vlc_sim.parse_device_auth(cast.encode_auth_response())
    check("AuthResponse is wrapped in DeviceAuthMessage.response", ok_new, why_new)

    # A certified receiver would add a certificate and a CA chain; make sure
    # the encoder can carry them without breaking the required fields.
    rich = cast.encode_auth_response(signature=b"sig", certificate=b"cert",
                                     intermediates=(b"ca1", b"ca2"))
    ok_rich, why_rich = vlc_sim.parse_device_auth(rich)
    check("a signed reply with a CA chain still parses", ok_rich, why_rich)

    old_bytes = vlc_sim._f_bytes(1, b"") + vlc_sim._f_bytes(2, b"")
    ok_old, why_old = vlc_sim.parse_device_auth(old_bytes)
    check("bare AuthResponse is rejected (the original bug)", not ok_old, why_old)

    # Field numbers must match cast_channel.proto exactly.
    fields = {n for n, _w, _v in vlc_sim._fields(cast.encode_auth_response())}
    check("DeviceAuthMessage uses field 2 for the response", fields == {2},
          str(fields))
    resp = dict((n, v) for n, w, v in vlc_sim._fields(cast.encode_auth_response())
                if w == 2)
    inner = {n for n, _w, _v in vlc_sim._fields(resp[2])}
    check("AuthResponse sets signature(1) and certificate(2)",
          inner == {1, 2}, str(inner))
    check("challenge message still parses",
          vlc_sim.parse_device_auth(cast.encode_auth_challenge())[0] is False,
          "challenge is not a response, correctly rejected")
finally:
    sys.path.remove(os.path.join(REPO, "scripts"))

# --------------------------------------------------------------------------
# Part 10: network interface listing / pinning
# --------------------------------------------------------------------------
print("\n=== Part 10: network interface picker ===")
_saved_get_ip2 = utils.Setting.get_ip
_saved_ni_attr2 = utils.ni
_saved_pin2 = utils.Setting.get_network_interface
_saved_set_pin = utils.Setting.set_network_interface
_saved_setting2 = (utils.Setting.setting, utils.Setting.setting_path)
try:
    utils.ni = _fake_ni
    rows = utils.Setting.network_interfaces()
    by_name = {r["name"]: r for r in rows}
    check("every local IPv4 interface is listed",
          set(by_name) == {"en0", "bridge100", "utun4"}, str(sorted(by_name)))
    check("default-route interface is flagged and sorts first",
          rows[0]["name"] == "en0" and rows[0]["default"] is True,
          str([(r["name"], r["default"]) for r in rows]))
    check("LAN address is marked usable",
          by_name["en0"]["usable"] is True and by_name["en0"]["reason"] == "",
          str(by_name["en0"]))
    check("Tailscale /32 address is marked unusable with a reason",
          by_name["utun4"]["usable"] is False and bool(by_name["utun4"]["reason"]),
          str(by_name["utun4"]))
    check("netmask is reported for the UI",
          by_name["en0"]["netmask"] == "255.255.255.0")

    # Pinning must narrow get_ip(), which is what SSDP uses to decide which
    # subnet it answers on.
    utils.Setting.get_network_interface = staticmethod(lambda: "bridge100")
    pinned = utils.Setting.get_ip()
    check("get_ip() honours the pinned interface",
          pinned == {("192.168.139.3", "255.255.254.0")}, str(pinned))

    # get_ip() at its most permissive (VM bridge + /32 tunnel present), as it
    # is on this kind of host; the filter must drop the tunnel.
    utils.Setting.get_ip = staticmethod(lambda: {
        ("192.168.139.3", "255.255.254.0"),
        ("100.85.176.107", "255.255.255.255"),
    })
    utils.Setting.get_network_interface = staticmethod(lambda: "")
    check("get_advertisable_ip() filters unreachable addresses",
          utils.Setting.get_advertisable_ip() == ["192.168.139.3"],
          str(utils.Setting.get_advertisable_ip()))

    # Back to the real get_ip() so the pin actually drives address selection.
    utils.Setting.get_ip = _saved_get_ip2
    utils.Setting.get_network_interface = staticmethod(lambda: "bridge100")
    check("get_advertisable_ip() follows the pin",
          utils.Setting.get_advertisable_ip() == ["192.168.139.3"],
          str(utils.Setting.get_advertisable_ip()))
    utils.Setting.get_network_interface = staticmethod(lambda: "utun4")
    check("an unusable pin is resolved away and never advertised",
          utils.Setting.resolved_network_interface() == "" and
          "100.85.176.107" not in utils.Setting.get_advertisable_ip(),
          str(utils.Setting.get_advertisable_ip()))
    utils.Setting.get_network_interface = staticmethod(lambda: "en0")
    check("a usable pin resolves to itself",
          utils.Setting.resolved_network_interface() == "en0")

    # The player is handed an environment without proxy variables.
    #
    # Regression: mpv inherited http_proxy from Macast's environment and sent
    # LAN media requests to it. A proxy with no route back to the sender
    # answers 502, so casting connected and then played nothing --
    # "[ffmpeg] http: HTTP error 502 Bad Gateway" for a URL curl fetched fine.
    _saved_env = {k: os.environ.get(k) for k in utils.Setting.PROXY_ENV_VARS}
    try:
        for k in utils.Setting.PROXY_ENV_VARS:
            os.environ[k] = "http://127.0.0.1:9"
        env = utils.Setting.get_system_env()
        check("player environment drops proxy variables",
              not any(k in env for k in utils.Setting.PROXY_ENV_VARS),
              str([k for k in utils.Setting.PROXY_ENV_VARS if k in env]))
        check("player environment keeps everything else", "PATH" in env)
    finally:
        for k, v in _saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
finally:
    utils.Setting.get_ip = _saved_get_ip2
    utils.ni = _saved_ni_attr2
    utils.Setting.get_network_interface = _saved_pin2
    utils.Setting.set_network_interface = _saved_set_pin
    utils.Setting.setting, utils.Setting.setting_path = _saved_setting2

# --------------------------------------------------------------------------
# Part 11: plugin hot-plug (enable / disable / uninstall / install)
#
# Everything here runs against a throwaway config dir so the tests can freely
# create, uninstall and re-install plugins without touching the real install.
# --------------------------------------------------------------------------
print("\n=== Part 11: plugin hot-plug ===")

RENDERER_PLUGIN = """#<macast.title>Hotplug Player</macast.title>
#<macast.renderer>HotplugRenderer</macast.renderer>
#<macast.platform>darwin,win32,linux</macast.platform>
#<macast.version>1.0</macast.version>
#<macast.author>tester</macast.author>
#<macast.desc>test renderer</macast.desc>
class HotplugRenderer(object):
    def start(self):
        pass
    def stop(self):
        pass
"""

PROTOCOL_PLUGIN = """#<macast.title>Hotplug Protocol</macast.title>
#<macast.protocol>HotplugProtocol</macast.protocol>
#<macast.platform>darwin,win32,linux</macast.platform>
#<macast.version>1.0</macast.version>
#<macast.author>tester</macast.author>
#<macast.desc>test protocol</macast.desc>
from macast.protocol import Protocol
class HotplugProtocol(Protocol):
    pass
"""

BROKEN_PLUGIN = """#<macast.title>Broken Plugin</macast.title>
#<macast.renderer>NotDefinedAnywhere</macast.renderer>
#<macast.platform>darwin,win32,linux</macast.platform>
"""

_tmp_root = _tempfile.mkdtemp(prefix="macast-hotplug-")
_saved_dir_macast = macast_mod.SETTING_DIR
_saved_dir_utils = utils.SETTING_DIR
_saved_setting3 = (utils.Setting.setting, utils.Setting.setting_path)
_saved_renderer_sel = utils.Setting.get(utils.SettingProperty.Macast_Renderer, '')
_saved_syspath = list(sys.path)
try:
    utils.SETTING_DIR = _tmp_root
    macast_mod.SETTING_DIR = _tmp_root
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp_root, "macast_setting.json")

    # Earlier parts built managers against the real config dir, so it is on
    # sys.path ahead of the sandbox. `renderer` / `protocol` would then resolve
    # to the *real* packages and the sandbox plugins would never be found --
    # which looks exactly like "hot-plug is broken". Point the lookup at the
    # sandbox, front-most, and drop any package already bound.
    for _mod in ("renderer", "protocol", "renderer.hotplay", "protocol.hotproto"):
        sys.modules.pop(_mod, None)
    sys.path[:] = [p for p in sys.path if p != _saved_dir_macast]
    sys.path.insert(0, _tmp_root)

    def _write(name, body, kind="renderer"):
        d = os.path.join(_tmp_root, kind)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, name)
        with open(p, "w", encoding="utf-8") as f:
            f.write(body)
        return p

    _write("hotplay.py", RENDERER_PLUGIN)
    _write("hotproto.py", PROTOCOL_PLUGIN, "protocol")

    mgr2 = macast_mod.MacastPluginManager(
        macast_mod.MacastPlugin(None, "MPV", _DummyMPV(), "darwin,win32,linux"),
        macast_mod.MacastPlugin(
            None, utils.format_class_name(protocol.DLNAProtocol()),
            protocol.DLNAProtocol(), "darwin,win32,linux"),
    )
    keys = {p.key: p for p in mgr2.renderer_all + mgr2.protocol_list}
    check("file plugins are keyed by kind:basename",
          "renderer:hotplay" in keys and "protocol:hotproto" in keys,
          str(sorted(keys)))
    check("built-in plugins are keyed by kind:title",
          "protocol:DLNA Protocol" in keys and "renderer:MPV" in keys,
          str(sorted(keys)))
    check("installed flag distinguishes file plugins from built-ins",
          keys["renderer:hotplay"].installed is True and
          keys["renderer:MPV"].installed is False)

    info = {i["key"]: i for i in mgr2.get_info()}
    check("plugin info exposes key / installed / can_uninstall",
          info["renderer:hotplay"]["can_uninstall"] is True and
          info["renderer:MPV"]["can_uninstall"] is False and
          info["protocol:hotproto"]["enabled"] is False,
          str(sorted(info)))

    # --- enable / disable a protocol --------------------------------------
    check("protocol can be enabled",
          mgr2.set_protocol_enabled("Hotplug Protocol", True) is True and
          "Hotplug Protocol" in mgr2.enabled_protocol_titles(),
          str(mgr2.enabled_protocol_titles()))
    mgr2.set_protocol_enabled("DLNA Protocol", False)
    check("protocol can be disabled when others remain",
          "DLNA Protocol" not in mgr2.enabled_protocol_titles(),
          str(mgr2.enabled_protocol_titles()))
    last_one_refused = False
    try:
        mgr2.set_protocol_enabled("Hotplug Protocol", False)
    except ValueError:
        last_one_refused = True
    check("refuses to disable the last enabled protocol", last_one_refused,
          str(mgr2.enabled_protocol_titles()))
    check("no restart needed: settings already updated",
          utils.Setting.get(utils.SettingProperty.Macast_Protocols) ==
          ["Hotplug Protocol"],
          str(utils.Setting.get(utils.SettingProperty.Macast_Protocols)))

    # --- enable / disable a renderer --------------------------------------
    utils.Setting.set(utils.SettingProperty.Macast_Renderer, "MPV")
    active_refused = False
    try:
        mgr2.set_renderer_enabled("renderer:MPV", False)
    except ValueError:
        active_refused = True
    check("refuses to unload the renderer currently in use", active_refused)

    check("renderer can be unloaded",
          mgr2.set_renderer_enabled("renderer:hotplay", False) is True)
    check("unloaded renderer leaves renderer_list but stays listed",
          "renderer:hotplay" not in {p.key for p in mgr2.renderer_list} and
          "renderer:hotplay" in {p.key for p in mgr2.renderer_all},
          str([p.key for p in mgr2.renderer_list]))
    check("unloaded renderer can be loaded again",
          mgr2.set_renderer_enabled("renderer:hotplay", True) is True and
          "renderer:hotplay" in {p.key for p in mgr2.renderer_list})

    # --- uninstall ---------------------------------------------------------
    builtin_refused = False
    try:
        mgr2.uninstall("renderer:MPV")
    except ValueError:
        builtin_refused = True
    check("built-in plugins cannot be uninstalled", builtin_refused)

    hot_path = os.path.join(_tmp_root, "renderer", "hotplay.py")
    mgr2.uninstall("renderer:hotplay")
    check("uninstall removes the plugin file",
          not os.path.exists(hot_path), hot_path)
    check("uninstall keeps a recoverable copy in .trash",
          os.path.exists(os.path.join(_tmp_root, ".trash")) and
          any("hotplay.py" in files
              for _r, _d, files in os.walk(os.path.join(_tmp_root, ".trash"))))
    check("uninstall drops it from the in-memory lists",
          "renderer:hotplay" not in {p.key for p in mgr2.renderer_all})

    # Uninstalling an enabled protocol must not leave a phantom selection.
    mgr2.set_protocol_enabled("DLNA Protocol", True)
    mgr2.uninstall("protocol:hotproto")
    check("uninstalling a protocol removes it from the enabled list",
          "Hotplug Protocol" not in mgr2.enabled_protocol_titles(),
          str(mgr2.enabled_protocol_titles()))

    # --- install -----------------------------------------------------------
    broken = _write("broken.py", BROKEN_PLUGIN)
    rollback_ok = False
    try:
        mgr2.install_file(broken, "renderer")
    except ValueError:
        rollback_ok = True
    check("a plugin that cannot load is rolled back",
          rollback_ok and
          not os.path.exists(os.path.join(_tmp_root, "renderer", "broken.py")))

    src = os.path.join(_tmp_root, "incoming.py")
    with open(src, "w", encoding="utf-8") as f:
        f.write(RENDERER_PLUGIN)
    installed = mgr2.install_file(src, "renderer", "incoming.py")
    check("install copies the plugin into place and loads it",
          installed.title == "Hotplug Player" and
          os.path.exists(os.path.join(_tmp_root, "renderer", "incoming.py")),
          installed.key)
    check("install rejects a non-.py file", _rejects(lambda: mgr2.install_file(
        src, "renderer", "incoming.zip")))

    src_p = os.path.join(_tmp_root, "incoming_proto.py")
    with open(src_p, "w", encoding="utf-8") as f:
        f.write(PROTOCOL_PLUGIN)
    mgr2.install_file(src_p, "protocol", "incoming_proto.py")
    check("a newly installed protocol is enabled automatically",
          "Hotplug Protocol" in mgr2.enabled_protocol_titles(),
          str(mgr2.enabled_protocol_titles()))

    check("install_url refuses a non-.py url",
          _rejects(lambda: mgr2.install_url("https://example.com/p.zip",
                                            "renderer")))

    # --- install_url obeys the mirror switch --------------------------------
    # Only the URL handed to requests.get is under test: the download and the
    # install itself are covered elsewhere, so both get stubbed.
    _seen_urls7 = []
    _real_get7 = macast_mod.requests.get
    _real_install_file7 = mgr2.install_file

    class _Resp7(object):
        content = b''

        def raise_for_status(self):
            pass

    macast_mod.requests.get = lambda url, *a, **k: (
        _seen_urls7.append(url), _Resp7())[1]
    mgr2.install_file = lambda *a, **k: None
    try:
        macast_mod.plugin_repo.set_mirror_enabled(True)
        mgr2.install_url("https://github.com/x/y/raw/mirrorplug.py", "renderer")
        check("plugin downloads go through the mirror when it is on",
              _seen_urls7 == ["https://ghproxy.net/https://github.com/x/y/raw/mirrorplug.py"],
              str(_seen_urls7))
        _seen_urls7[:] = []
        macast_mod.plugin_repo.set_mirror_enabled(False)
        mgr2.install_url("https://github.com/x/y/raw/mirrorplug.py", "renderer")
        check("and straight to GitHub when it is off",
              _seen_urls7 == ["https://github.com/x/y/raw/mirrorplug.py"],
              str(_seen_urls7))
    finally:
        macast_mod.requests.get = _real_get7
        mgr2.install_file = _real_install_file7
        macast_mod.plugin_repo.set_mirror_enabled(False)

    check("unknown keys are reported, not silently ignored",
          mgr2.plugin_by_key("renderer:nope") is None)
finally:
    macast_mod.SETTING_DIR = _saved_dir_macast
    utils.SETTING_DIR = _saved_dir_utils
    utils.Setting.setting, utils.Setting.setting_path = _saved_setting3
    # Best-effort: the value was only read to be put back unchanged, so a
    # sandbox that forbids writing the user's real config dir (EEXIST from the
    # PYTHONPATH shim, EPERM from the DSH file sandbox) must not turn a fully
    # passing suite into a traceback on the very last line.
    try:
        utils.Setting.set(utils.SettingProperty.Macast_Renderer, _saved_renderer_sel)
    except OSError as e:
        print("[WARN] could not restore the real renderer setting "
              "({}); the test used its own temp config, so nothing was "
              "changed by the run itself.".format(e))
    for _mod in ("renderer", "protocol", "renderer.hotplay", "renderer.incoming",
                 "protocol.hotproto", "protocol.incoming_proto"):
        sys.modules.pop(_mod, None)
    sys.path[:] = _saved_syspath
    _shutil.rmtree(_tmp_root, ignore_errors=True)

# --------------------------------------------------------------------------
# Part 12: the web cast entry point (GET /api?query=cast)
#
# Anything that can only open a URL -- a phone Shortcut, a bookmarklet, curl --
# can now start playback. Two properties matter and neither is visible from the
# code alone:
#
#   1. the token has to survive a restart, otherwise every configured Shortcut
#      breaks the next time Macast is opened. It used to be per-process and was
#      never displayed anywhere, which made the whole management API
#      unreachable from another device;
#   2. a GET that starts playback is reachable by *any page the user visits*
#      (an <img src="http://127.0.0.1:58880/api?query=cast&...">), so unlike the
#      POST flavour it must demand the token even from the loopback interface.
#
# CherryPy hands out a dummy loopback request outside a served request, so the
# gate can be driven for real here instead of being asserted from source.
# --------------------------------------------------------------------------
print("\n=== Part 12: web cast entry ===")
try:
    import cherrypy as _cherrypy

    _saved_setting4 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir4 = utils.SETTING_DIR
    _tmp4 = _tempfile.mkdtemp(prefix="macast-webcast-")
    try:
        utils.SETTING_DIR = _tmp4
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp4, "macast_setting.json")

        _token = protocol.api_token()
        check("the management token is generated once and looks like a secret",
              _token == protocol.api_token() and len(_token) == 32, repr(_token))

        utils.Setting.setting = {}      # pretend the process was restarted
        check("the management token survives a restart",
              protocol.api_token() == _token, repr(protocol.api_token()))
        check("the token is persisted in the settings, not just in memory",
              utils.Setting.get(utils.SettingProperty.Api_Token, '') == _token,
              repr(utils.Setting.setting.get('Api_Token')))

        class _Target(object):
            """Stands in for the protocol a cast is handed to."""

            def __init__(self):
                self.calls = []
                self.fail = False

            def cast_uri(self, uri, title=''):
                if self.fail:
                    raise RuntimeError('renderer exploded')
                self.calls.append((uri, title))

        class _Handler(protocol.Handler):
            # Skips the real __init__ (it reads the settings page off disk and
            # creates the local-files directory) -- neither is needed here.
            def __init__(self, target):
                self._target = target

            @property
            def protocol(self):
                return self._target

        target = _Target()
        handler = _Handler(target)
        request = _cherrypy.serving.request
        _saved_params = request.params
        _saved_remote = getattr(request, 'remote', None)
        _saved_scheme = request.scheme
        _saved_headers = dict(request.headers)
        # GET answers 503 when the service is not up, and this suite never
        # starts one. Everything below is about the gate and the payload, so
        # the "is the engine running" check is the one thing that gets stubbed.
        _saved_running = utils.Setting.is_service_running
        utils.Setting.is_service_running = staticmethod(lambda: True)
        try:
            def _reset(ip='127.0.0.1', token=None, scheme='http'):
                request.headers.clear()
                for _k, _v in _saved_headers.items():
                    request.headers[_k] = _v
                request.params = {'token': token} if token else {}
                request.remote = types.SimpleNamespace(ip=ip)
                request.scheme = scheme
                return request

            def _get(**kw):
                return json.loads(handler.GET(param='api', **kw).decode())

            def _post(**kw):
                return json.loads(handler.POST(**kw).decode())

            # -- the drive-by guard ----------------------------------------
            _reset()
            check("loopback is still trusted for the management API",
                  handler._management_allowed() is True)
            check("but a GET cast is refused without the token, even locally",
                  _get(query='cast', url='http://x/y.mp4')['code'] == 403,
                  str(_get(query='cast', url='http://x/y.mp4')))
            check("a refused cast starts nothing", target.calls == [], str(target.calls))

            _reset(token=_token)
            res = _get(query='cast', url='http://x/y.mp4', title='movie')
            check("the token in the query string authorises a GET cast",
                  res.get('code') == 0 and target.calls == [('http://x/y.mp4', 'movie')],
                  "{} / {}".format(res, target.calls))

            _reset()
            request.headers['X-Macast-Token'] = _token
            target.calls = []
            res = _get(query='cast', url='http://x/y2.mp4')
            check("the token header authorises a GET cast too (what scripts use)",
                  res.get('code') == 0 and target.calls == [('http://x/y2.mp4', '')],
                  "{} / {}".format(res, target.calls))

            _reset(ip='192.168.1.9')
            check("a LAN caller without the token is not trusted",
                  handler._management_allowed() is False)
            _reset(ip='192.168.1.9', token=_token)
            check("a LAN caller with the token is trusted",
                  handler._management_allowed() is True)
            _reset(ip='192.168.1.9', scheme='https')
            check("the HTTPS admin channel is still trusted without a token",
                  handler._management_allowed() is True)

            # -- 「启用国内镜像地址」toggle ---------------------------------
            _reset(ip='192.168.1.9')
            res = _post(**{'set-github-mirror': '1'})
            check("a LAN caller without the token cannot flip the mirror switch",
                  res.get('code') == 403, str(res))
            check("and the refused flip persists nothing",
                  'Github_CN_Mirror' not in utils.Setting.setting,
                  repr(utils.Setting.setting))
            _reset()
            res = _post(**{'set-github-mirror': '1'})
            check("locally the mirror flips on and the page gets new coordinates",
                  res.get('code') == 0 and res.get('mirror_enabled') is True and
                  all(u.startswith('https://ghproxy.net/') or 'jsdelivr' in u
                      for u in res['plugin_repo']['index_urls']), str(res))
            check("the mirror state is now in the settings",
                  utils.Setting.setting.get('Github_CN_Mirror') is True,
                  repr(utils.Setting.setting))
            _reset()
            res = _post(**{'set-github-mirror': 'off'})
            check("the mirror flips back off and the key is removed",
                  res.get('code') == 0 and res.get('mirror_enabled') is False and
                  'Github_CN_Mirror' not in utils.Setting.setting, str(res))

            # -- validation -------------------------------------------------
            _reset(token=_token)
            target.calls = []
            check("a cast without a url is rejected",
                  _get(query='cast')['code'] == 1, str(_get(query='cast')))
            check("a relative url is rejected before it reaches the player",
                  _get(query='cast', url='/tmp/movie.mp4')['code'] == 1,
                  str(_get(query='cast', url='/tmp/movie.mp4')))
            check("nothing was cast by the rejected requests",
                  target.calls == [], str(target.calls))

            target.fail = True
            check("a renderer failure is reported as a failed cast, not a crash",
                  _get(query='cast', url='http://x/y.mp4')['code'] == 1)
            target.fail = False

            # -- cast-info (the token must not leak to the LAN) --------------
            _reset(token=_token)
            res = _get(query='cast-info')
            check("cast-info hands the page the token and the port",
                  res.get('token') == _token and res.get('port') == utils.Setting.get_port(),
                  str(res))
            _reset(ip='192.168.1.9')
            check("cast-info refuses to leak the token to the LAN",
                  _get(query='cast-info').get('code') == 403,
                  str(_get(query='cast-info')))

            # -- the POST flavour keeps working for the local page ----------
            _reset()
            target.calls = []
            res = _post(**{'cast-uri': 'http://x/z.mp4'})
            check("POST cast-uri still works from the page",
                  res.get('code') == 0 and target.calls == [('http://x/z.mp4', '')],
                  "{} / {}".format(res, target.calls))
            _reset(ip='192.168.1.9')
            check("POST cast-uri still refuses an untokened LAN caller",
                  _post(**{'cast-uri': 'http://x/z.mp4'}).get('code') == 403)
        finally:
            request.params = _saved_params
            request.remote = _saved_remote
            request.scheme = _saved_scheme
            request.headers.clear()
            for _k, _v in _saved_headers.items():
                request.headers[_k] = _v
            utils.Setting.is_service_running = staticmethod(_saved_running)
    finally:
        utils.SETTING_DIR = _saved_dir4
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting4
        _shutil.rmtree(_tmp4, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("web cast entry behaves", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Plugin harness (shared by Parts 13-17)
#
# The bundled plugins in macast/plugins/{renderer,protocol}/ are written against the app's public surface:
# `macast`, `macast.renderer`, `macast.gui` and `macast_renderer.mpv`. This
# suite loads macast's submodules by hand, so before any plugin is imported:
#
#   * REPO goes on sys.path, for `macast_renderer`;
#   * the GUI toolkits are stubbed if they are missing (same trick as Part 5 --
#     `macast.gui` imports rumps at module level);
#   * `macast_renderer.mpv` is imported for real, and so is `macast.gui`: the
#     number of ways a plugin can get the player's command line or a menu tree
#     wrong is exactly why these are tested against the real classes.
# --------------------------------------------------------------------------
print("\n=== plugin harness ===")
_plugin_dir = os.path.join(REPO, "plugins")  # legacy index dir; fallback only


def _load_plugin(name, filename=None):
    """Import a bundled plugin module by file name, as Macast does at startup.

    Plugin sources now live under ``macast/plugins/{renderer,protocol}/`` (they
    ship inside the package), so resolve the file from there instead of the
    flat ``plugins/`` index directory.
    """
    filename = filename or (name + ".py")
    path = None
    for _d in (os.path.join(MACAST, "plugins", "renderer"),
               os.path.join(MACAST, "plugins", "protocol")):
        _candidate = os.path.join(_d, filename)
        if os.path.isfile(_candidate):
            path = _candidate
            break
    if path is None:
        path = os.path.join(_plugin_dir, filename)  # last-resort fallback
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def _write_fake(bindir, name, body):
    """Drop an executable `name` into `bindir` (a stand-in for a real tool)."""
    os.makedirs(bindir, exist_ok=True)
    path = os.path.join(bindir, name)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(body)
    os.chmod(path, 0o755)
    return path


def _wait_until(predicate, timeout=10.0, interval=0.1):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


class _FakeMdns(object):
    """Be the network for a few seconds: `zeroconf` answered with these services.

    Both Chromecast searches reach into `zeroconf` inside the function, so the
    only way to test what a search *keeps* is to hand it an answer. Each service
    is `(instance name, friendly name, address, port)`.
    """

    def __init__(self, services):
        self.services = list(services)
        self.saved = sys.modules.get('zeroconf')

    def __enter__(self):
        outer = self

        class _Info(object):
            def __init__(self, friendly, address, port):
                self.friendly, self.address, self.port = friendly, address, port
                self.server = 'fake.local.'

            def parsed_addresses(self, api=4):
                return [self.address]

            @property
            def properties(self):
                return {b'fn': self.friendly.encode('utf-8')}

        class _Zeroconf(object):
            def get_service_info(self, type_, name, timeout=None):
                for instance, friendly, address, port in outer.services:
                    if instance == name:
                        return _Info(friendly, address, port)
                return None

            def close(self):
                pass

        class _ServiceBrowser(object):
            def __init__(self, zc, type_, listener):
                for instance, _f, _a, _p in outer.services:
                    listener.add_service(zc, type_, instance)

        module = types.ModuleType('zeroconf')
        module.Zeroconf = _Zeroconf
        module.ServiceBrowser = _ServiceBrowser
        sys.modules['zeroconf'] = module
        return self

    def __exit__(self, *exc):
        if self.saved is None:
            sys.modules.pop('zeroconf', None)
        else:
            sys.modules['zeroconf'] = self.saved
        return False


def _own_address(setting_cls, address='192.0.2.1'):
    """Pin "which addresses are ours" for a search test; returns the restore.

    Restored exactly as found: an earlier Part may already have replaced this
    with a plain lambda, and unwrapping that would put a `staticmethod` where a
    function was.
    """
    marker = object()
    saved = setting_cls.__dict__.get('get_advertisable_ip', marker)

    def restore():
        if saved is marker:
            try:
                delattr(setting_cls, 'get_advertisable_ip')
            except AttributeError:
                pass
        else:
            setting_cls.get_advertisable_ip = saved
    setting_cls.get_advertisable_ip = staticmethod(lambda: [address])
    return restore


def _console_texts(state):
    """Every string the console window would lay out, for the「says X」checks.

    `console_state()` replaced this suite's menu walks: the screen-mirror
    controls live in another process now, so the view model *is* the surface, and
    asserting on it needs no display.
    """
    texts = [state['audio']['line'], state['capture']['label'],
             state['quality']['note'], state['prompt'],
             state['viewer']['hint'],
             state['viewer']['url'], state['status_line']]
    for group in ('output', 'quality', 'profiles'):
        texts += [opt['label'] + opt.get('hint', '')
                  for opt in state[group]['options']]
    #: Channel rows carry what the old `search` card carried -- the verdict per
    #: protocol and the devices behind it -- because the window now asks the
    #: protocol question first.
    for row in state.get('channels') or []:
        texts += [row['label'], row['words'], row['choice'], row['hint']]
        texts += [d['label'] for d in row['devices']]
    texts += [r['label'] + r['detail'] + r['command']
              for r in state.get('requirements') or []]
    texts += [s['label'] for s in state['capture']['screens']]
    return [t for t in texts if t]


def _channel(state, key):
    """One protocol's row from `console_state()['channels']`.

    The suite used to read a single `state['search']` card; with the protocol
    question asked first, every verdict is per protocol, so a check has to say
    which protocol it means -- and that is the check's whole point.
    """
    for row in state.get('channels') or []:
        if row.get('key') == key:
            return row
    raise KeyError(key)


# The view rules are plain functions over the state dict: no display, no browser,
# no running app. That is what makes "which panels does this state warrant"
# testable here at all.
from macast import mirror_view as _mc  # noqa: E402


class _StateRec(object):
    """Stands in for the protocol a renderer reports to."""

    def __init__(self):
        self.rows = []

    def set_state_url(self, v):
        self.rows.append(('url', v))

    def set_state(self, k, v):
        self.rows.append((k, v))

    def set_state_position(self, v):
        self.rows.append(('position', v))

    def set_state_duration(self, v):
        self.rows.append(('duration', v))

    def set_state_transport(self, v):
        self.rows.append(('transport', v))

    def set_state_transport_error(self):
        self.rows.append(('error', True))

    def get_state_title(self):
        return 'Some Title'


try:
    if REPO not in sys.path:
        sys.path.insert(0, REPO)

    class _AnyModule(types.ModuleType):
        def __getattr__(self, name):
            if name.startswith("__"):
                raise AttributeError(name)
            return object

    for _name in ("rumps", "pystray"):
        try:
            __import__(_name)
        except ImportError:
            sys.modules.setdefault(_name, _AnyModule(_name))

    # Part 5 installed a *stub* for `macast_renderer.mpv` (macast.py only needed
    # the symbol). The plugins need the real one, so drop the stub first -- a
    # stub reports a truthy attribute for every name, which would make these
    # tests pass while testing nothing.
    for _name in ("macast_renderer.mpv", "macast_renderer"):
        _mod = sys.modules.get(_name)
        if _mod is not None and getattr(_mod, "__file__", None) is None:
            sys.modules.pop(_name, None)

    import macast.gui as gui_mod            # noqa: E402  (needs the stubs above)
    import macast_renderer.mpv as mpv_mod   # noqa: E402

    _pkg = sys.modules["macast"]
    # User plugins say `from macast import Setting, MenuItem, gui`; bind the real
    # objects behind those names rather than doubles, so a menu the plugin builds
    # is the menu Macast would build.
    _pkg.Setting = utils.Setting
    _pkg.MenuItem = gui_mod.MenuItem
    _pkg.App = gui_mod.App
    _pkg.gui = lambda *a, **k: None
    check("the real mpv renderer and gui are importable for plugin tests",
          mpv_mod.MPVRenderer is not None and gui_mod.MenuItem is not None)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("plugin harness is usable", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 13: the yt-dlp downloader shipped as an online plugin
#
# macast/plugins/renderer/macast_ytdlp.py is a bundled built-in plugin (it used to be offered through the online index):
# it turns "cast this Bilibili / YouTube / m3u8 url" into "download it", which
# is the one thing the built-in mpv renderer cannot do with a page URL.
#
# It is a single file that shells out to the yt-dlp *binary*, so the things
# worth testing are the ones a user would otherwise discover the hard way:
#
#   1. a fake yt-dlp on PATH is really executed, its progress lines become
#      playback state, and its Destination line becomes the title;
#   2. a missing binary (or a failed download) is *reported* -- a phone left
#      staring at a spinner is the failure mode this whole part guards;
#   3. stopping mid-download kills the child and does not then announce a
#      completion that never happened.
# --------------------------------------------------------------------------
print("\n=== Part 13: yt-dlp downloader plugin ===")
try:
    import cherrypy

    _plugin_file = os.path.join(MACAST, "plugins", "renderer", "macast_ytdlp.py")
    check("the downloader plugin ships in macast/plugins/renderer/",
          os.path.isfile(_plugin_file), _plugin_file)

    _saved_setting5 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir5 = utils.SETTING_DIR
    _saved_path5 = os.environ.get('PATH', '')
    _tmp5 = _tempfile.mkdtemp(prefix="macast-ytdlp-")
    _notify_rec = lambda *a, **k: _notifications.append(a)  # noqa: E731
    _notifications = []
    try:
        utils.SETTING_DIR = _tmp5
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp5, "macast_setting.json")
        # Never download into the user's real ~/Downloads from a test: point the
        # plugin's own setting at the sandbox instead.
        _downloads5 = os.path.join(_tmp5, "downloads")

        # The plugin imports its names off the `macast` package, the way every
        # real user plugin does; the harness above has already bound the real
        # Setting / MenuItem / gui behind those names.
        _spec = importlib.util.spec_from_file_location("macast_ytdlp_plugin",
                                                       _plugin_file)
        _plug = importlib.util.module_from_spec(_spec)
        sys.modules["macast_ytdlp_plugin"] = _plug
        _spec.loader.exec_module(_plug)

        try:
            _read_manifest5 = _read_manifest
        except NameError:
            _read_manifest5 = macast_mod._read_plugin_metadata
        _meta5 = _read_manifest5(_plugin_file)
        # Setting.set keys off the enum *name*, so the plugin's own enum works
        # here and the download lands in the sandbox.
        utils.Setting.set(_plug.SettingProperty.YTDLP_Dir, _downloads5)
        check("the manifest names the class the module defines",
              getattr(_plug, _meta5.get('renderer', ''), None) is _plug.YTDLPRenderer,
              repr(_meta5.get('renderer')))
        check("eta parsing handles mm:ss and h:mm:ss",
              (_plug.eta_seconds('00:12'), _plug.eta_seconds('1:02:03')) == (12, 3723),
              str((_plug.eta_seconds('00:12'), _plug.eta_seconds('1:02:03'))))
        check("a duration is formatted the way the status page expects",
              _plug.hms(3723) == '1:02:03', _plug.hms(3723))

        # -- a fake yt-dlp on PATH --------------------------------------
        _bin5 = os.path.join(_tmp5, "bin")
        os.makedirs(_bin5)
        _fake = os.path.join(_bin5, "yt-dlp")
        with open(_fake, "w", encoding="utf-8") as f:
            # `exec sleep` matters: replacing the shell with the sleeper means
            # SIGTERM reaches the process that holds the pipe, so the plugin's
            # reader loop really ends when the download is cancelled.
            f.write("#!/bin/sh\n"
                    "echo '[download] Destination: /tmp/video [abc].mp4'\n"
                    "if [ -n \"$YTDLP_SLOW\" ]; then exec sleep 30; fi\n"
                    "echo '[download]  50.0% of 10.00MiB at 1.00MiB/s ETA 00:05'\n"
                    "sleep 1\n"
                    "echo '[download] 100% of 10.00MiB in 00:10'\n"
                    "exit 0\n")
        os.chmod(_fake, 0o755)
        os.environ['PATH'] = _bin5 + os.pathsep + _saved_path5
        check("the binary is found on PATH",
              _plug.find_ytdlp() == _fake, str(_plug.find_ytdlp()))

        class _States(object):
            """Stands in for the protocol the renderer reports to."""

            def __init__(self):
                self.rows = []

            def set_state_url(self, v):
                self.rows.append(('url', v))

            def set_state(self, k, v):
                self.rows.append((k, v))

            def set_state_position(self, v):
                self.rows.append(('position', v))

            def set_state_duration(self, v):
                self.rows.append(('duration', v))

            def set_state_transport(self, v):
                self.rows.append(('transport', v))

            def set_state_transport_error(self):
                self.rows.append(('error', True))

        states = _States()

        class _Renderer(_plug.YTDLPRenderer):
            @property
            def protocol(self):
                return states

        cherrypy.engine.subscribe('app_notify', _notify_rec)
        renderer = _Renderer()

        def _wait_for(row, timeout=20):
            deadline = time.time() + timeout
            while time.time() < deadline:
                if row in states.rows:
                    return True
                time.sleep(0.2)
            return False

        renderer.set_media_url('https://example.com/watch?v=abc')
        check("the cast is reported as playing while the download runs",
              _wait_for(('transport', 'PLAYING'), timeout=5), str(states.rows[:3]))
        check("the download finishes", _wait_for(('transport', 'STOPPED')),
              str(states.rows))
        check("progress lines become position and duration",
              any(r[0] == 'position' for r in states.rows)
              and any(r[0] == 'duration' for r in states.rows), str(states.rows))
        check("the file name yt-dlp reported becomes the title",
              ('CurrentTrackTitle', 'video [abc].mp4') in states.rows, str(states.rows))
        check("a successful download reports no error",
              ('error', True) not in states.rows, str(states.rows))
        check("finishing the download notifies the user",
              any('下载完成' in str(n) for n in _notifications), str(_notifications))

        # -- cancel mid-download -----------------------------------------
        os.environ['YTDLP_SLOW'] = '1'
        states.rows = []
        _before = len(_notifications)
        renderer.set_media_url('https://example.com/slow')
        time.sleep(1.5)
        renderer.set_media_stop()
        check("stopping marks the download stopped",
              ('transport', 'STOPPED') in states.rows, str(states.rows))
        renderer._thread.join(timeout=10)
        check("the cancelled download's thread really ends",
              not renderer._thread.is_alive())
        time.sleep(0.5)
        check("a cancelled download is not announced as completed",
              len(_notifications) == _before, str(_notifications[_before:]))

        # -- no yt-dlp installed -----------------------------------------
        del os.environ['YTDLP_SLOW']
        _real_find = _plug.find_ytdlp
        _plug.find_ytdlp = lambda: None
        states.rows = []
        renderer.set_media_url('https://example.com/x')
        check("a missing yt-dlp is reported, not swallowed",
              _wait_for(('error', True), timeout=5) or ('error', True) in states.rows,
              str(states.rows))
        check("the report names the tool to install",
              any(r[0] == 'CurrentTrackTitle' and 'yt-dlp' in str(r[1])
                  for r in states.rows), str(states.rows))
        _plug.find_ytdlp = _real_find

        states.rows = []
        renderer.set_media_url('')
        time.sleep(0.5)
        check("an empty url starts nothing", states.rows == [], str(states.rows))
        check("the download directory setting is honoured",
              os.path.isdir(_downloads5), _downloads5)
        renderer.set_media_stop()
    finally:
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify_rec)
        except Exception:
            pass
        os.environ['PATH'] = _saved_path5
        os.environ.pop('YTDLP_SLOW', None)
        utils.SETTING_DIR = _saved_dir5
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting5
        _shutil.rmtree(_tmp5, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("yt-dlp downloader plugin behaves", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 14: the player adapters, the floating window and the yt-dlp modes
#
# These plugins live or die on the command line they build, and a wrong option
# is not a small mistake: mpv refuses to start on an unknown option, and the
# launcher's response to "mpv never connected" is to retry three times and then
# stop the service. So the assertions below are about argv, not about vibes.
# --------------------------------------------------------------------------
print("\n=== Part 14: player adapters / floating / yt-dlp modes ===")
try:
    _saved_setting6 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir6 = utils.SETTING_DIR
    _saved_path6 = os.environ.get('PATH', '')
    _tmp6 = _tempfile.mkdtemp(prefix="macast-plugins-")
    _notify6 = []
    _notify6_rec = lambda *a, **k: _notify6.append(a)      # noqa: E731
    try:
        utils.SETTING_DIR = _tmp6
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp6, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify6_rec)

        # -- external player -------------------------------------------
        _bin6 = os.path.join(_tmp6, "bin")
        _argv6 = os.path.join(_tmp6, "argv.txt")
        _fake_vlc = _write_fake(
            _bin6, "vlc",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nsleep 30\n".format(_argv6))
        _fake_ytdlp = _write_fake(_bin6, "yt-dlp", "#!/bin/sh\nexit 0\n")
        os.environ['PATH'] = _bin6 + os.pathsep + _saved_path6

        ext = _load_plugin("ext_player", "external_player.py")
        check("an installed player is found (with the url as one argv element)",
              ext.find_player(ext.PLAYERS['VLC']) == _fake_vlc,
              str(ext.find_player(ext.PLAYERS['VLC'])))
        check("a player that is not installed is reported missing",
              ext.find_player(ext.PLAYERS['MPC-BE / MPC-HC']) is None,
              str(ext.find_player(ext.PLAYERS['MPC-BE / MPC-HC'])))
        check("only the installed players are offered",
              [name for name, _ in ext.installed_players()] == ['VLC'],
              str(ext.installed_players()))

        _rec14 = _StateRec()

        class _ExtRenderer(ext.ExternalPlayerRenderer):
            @property
            def protocol(self):
                return _rec14

        external = _ExtRenderer()
        external.set_media_url('http://host/movie file.mp4')
        check("the cast url really reaches the player's argv",
              _wait_until(lambda: os.path.exists(_argv6))
              and open(_argv6, encoding='utf-8').read().split('\n')[0]
              == 'http://host/movie file.mp4',
              open(_argv6, encoding='utf-8').read() if os.path.exists(_argv6) else 'no argv')
        check("launching is reported as playing",
              ('transport', 'PLAYING') in _rec14.rows, str(_rec14.rows))
        _proc14 = external._proc
        external.set_media_stop()
        check("stop really kills the player process",
              _proc14 is not None and _proc14.poll() is not None,
              repr(_proc14.poll() if _proc14 else None))

        # A player the user does not have must be reported, not crash.
        _real_find14 = ext.find_player
        ext.find_player = lambda entry: None
        _rec14.rows = []
        external.set_media_url('http://host/other.mp4')
        check("a missing player is reported as an error",
              ('error', True) in _rec14.rows, str(_rec14.rows))
        check("the error is announced to the user",
              any('外部播放器' in str(n) for n in _notify6), str(_notify6))
        ext.find_player = _real_find14

        # -- floating window / wallpaper -------------------------------
        flo = _load_plugin("floating_player", "floating.py")
        utils.Setting.set(mpv_mod.SettingProperty.PlayerSize,
                          mpv_mod.SettingProperty.PlayerSize_FullScreen.value)
        floating = flo.FloatingRenderer(path='mpv')
        params = floating.build_mpv_params()
        check("the floating window owns its geometry, whatever the global size says",
              '--fullscreen' not in params
              and len([p for p in params if p.startswith('--geometry=')]) == 1,
              str([p for p in params if p.startswith(('--geometry', '--autofit', '--fullscreen'))]))
        check("the floating window is small and on top",
              any(p.startswith('--autofit=') for p in params) and '--ontop' in params,
              str([p for p in params if 'autofit' in p or 'ontop' in p]))

        utils.Setting.set(flo.SettingProperty.Floating_Mode, 1)
        flo.mpv_supports = lambda path, option: True
        params = floating.build_mpv_params()
        check("wallpaper mode asks for the desktop window level",
              '--ontop-level=desktop' in params, str(params))
        flo.mpv_supports = lambda path, option: False
        params = floating.build_mpv_params()
        check("an mpv without that level degrades to a normal window",
              not any(p.startswith('--ontop-level=') for p in params)
              and '--ontop' in params, str(params))
        utils.Setting.set(flo.SettingProperty.Floating_Mode, 0)
        utils.Setting.set(mpv_mod.SettingProperty.PlayerSize,
                          mpv_mod.SettingProperty.PlayerSize_Normal.value)

        # -- yt-dlp: download vs stream --------------------------------
        def _without_socket(params):
            # The ipc socket path carries a random suffix per instance, so it is
            # never part of "did the plugin change the command line".
            return [p for p in params if not p.startswith('--input-ipc-server=')]

        ytdlp = _load_plugin("ytdlp_plugin_2", "macast_ytdlp.py")
        ytdlp_renderer = ytdlp.YTDLPRenderer(path='mpv')
        check("yt-dlp defaults to downloading", ytdlp_renderer.stream_mode() is False)
        check("download mode leaves the player command line untouched",
              _without_socket(ytdlp_renderer.build_mpv_params())
              == _without_socket(mpv_mod.MPVRenderer(path='mpv').build_mpv_params()),
              str(ytdlp_renderer.build_mpv_params()))

        utils.Setting.set(ytdlp.SettingProperty.YTDLP_Mode, 1)
        check("stream mode is what the setting says", ytdlp_renderer.stream_mode() is True)
        params = ytdlp_renderer.build_mpv_params()
        hook = [p for p in params if 'ytdl_hook-ytdl_path' in p]
        check("stream mode tells mpv's ytdl hook where yt-dlp is",
              len(hook) == 1 and _fake_ytdlp in hook[0], str(hook))
        check("the ytdl path is merged into the existing script options",
              len([p for p in params if p.startswith('--script-opts=')]) == 1,
              str([p for p in params if p.startswith('--script-opts=')]))

        _commands14 = []
        ytdlp_renderer.send_command = lambda command: _commands14.append(command) or True
        ytdlp_renderer.set_media_url('https://example.com/watch?v=abc')
        check("stream mode hands the page url to mpv",
              _commands14[-1:] == [['loadfile', 'https://example.com/watch?v=abc',
                                    'replace']],
              str(_commands14))
        check("stream mode does not also start a download",
              ytdlp_renderer._thread is None)
        utils.Setting.set(ytdlp.SettingProperty.YTDLP_Mode, 0)
    finally:
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify6_rec)
        except Exception:
            pass
        os.environ['PATH'] = _saved_path6
        utils.SETTING_DIR = _saved_dir6
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting6
        _shutil.rmtree(_tmp6, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("player adapters and floating window behave", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 15: automation hooks
#
# The point of a hook is that it runs the *user's* command at the right moment
# with the right environment. Both halves are tested with a real shell command
# writing to a file: an event that fires with the wrong url is as broken as one
# that never fires.
# --------------------------------------------------------------------------
print("\n=== Part 15: automation hooks ===")
try:
    _saved_setting7 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir7 = utils.SETTING_DIR
    _tmp7 = _tempfile.mkdtemp(prefix="macast-hooks-")
    try:
        utils.SETTING_DIR = _tmp7
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp7, "macast_setting.json")
        _hooklog = os.path.join(_tmp7, "hooks.log")
        hooks = _load_plugin("hooks_plugin", "hooks.py")

        utils.Setting.set(
            hooks.SettingProperty.Hook_On_Cast,
            'echo "cast:$MACAST_EVENT:$MACAST_URL:$MACAST_TITLE" >> ' + _hooklog)
        utils.Setting.set(hooks.SettingProperty.Hook_On_Pause,
                          'echo pause >> ' + _hooklog)
        utils.Setting.set(hooks.SettingProperty.Hook_On_Stop,
                          'echo stop >> ' + _hooklog)

        _rec15 = _StateRec()

        class _HooksRenderer(hooks.HooksRenderer):
            @property
            def protocol(self):
                return _rec15

        hooks_renderer = _HooksRenderer(path='mpv')
        hooks_renderer.send_command = lambda command: True   # no mpv in this test
        hooks_renderer.set_media_url('http://host/film.mp4')
        check("the cast hook runs with the event, url and title in its environment",
              _wait_until(lambda: os.path.exists(_hooklog)
                          and 'cast:cast:http://host/film.mp4:Some Title'
                          in open(_hooklog, encoding='utf-8').read()),
              open(_hooklog, encoding='utf-8').read() if os.path.exists(_hooklog) else 'no log')

        hooks_renderer.set_media_pause()
        hooks_renderer.set_media_stop()
        _wait_until(lambda: os.path.exists(_hooklog)
                    and 'stop' in open(_hooklog, encoding='utf-8').read())
        body = open(_hooklog, encoding='utf-8').read()
        check("the pause hook runs on pause", 'pause' in body, body)
        check("the stop hook runs on stop", 'stop' in body, body)
        check("an unconfigured event runs nothing",
              hooks.run_hook('resume', 'http://host/film.mp4') is False)
        # Fire-and-forget is the design: the command is the user's, and a
        # failure in it must never surface as an exception inside the CherryPy
        # worker that is answering the phone.
        utils.Setting.set(hooks.SettingProperty.Hook_On_Resume,
                          '/nonexistent/macast-hook-xyz')
        try:
            hooks.run_hook('resume', 'http://host/film.mp4')
            raised = False
        except Exception as exc:
            raised = '{}: {}'.format(type(exc).__name__, exc)
        check("a hook command that cannot succeed does not raise into the worker",
              raised is False, str(raised))

        # Opening the menu is what creates the four keys in the settings file,
        # which is how the feature is discovered (a menu cannot ask for input).
        hooks.HooksRendererSetting().build_menu()
        check("the menu creates the hook keys in the settings file",
              all(utils.Setting.has(hooks.SettingProperty[key])
                  for key in ('Hook_On_Cast', 'Hook_On_Pause',
                              'Hook_On_Resume', 'Hook_On_Stop')),
              str(sorted(k for k in utils.Setting.setting if k.startswith('Hook_'))))
    finally:
        utils.SETTING_DIR = _saved_dir7
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting7
        _shutil.rmtree(_tmp7, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("automation hooks behave", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 16: the Chromecast bridge, driven against Macast's own receiver
#
# This is the sender half of a protocol Macast implements as a receiver -- which
# makes for an unusually good test: start a real ChromecastProtocol (the same
# class the app runs), point the bridge at 127.0.0.1:<its port> and check that
# a genuine device-auth / CONNECT / LAUNCH / CONNECT transport / LOAD sequence
# arrives intact. No real TV needed, and nothing here is simulated except the
# player behind the receiver.
# --------------------------------------------------------------------------
print("\n=== Part 16: Chromecast bridge ===")
try:
    _saved_setting8 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir8 = utils.SETTING_DIR
    _tmp8 = _tempfile.mkdtemp(prefix="macast-bridge-")
    _notify8 = []
    _notify8_rec = lambda *a, **k: _notify8.append(a)      # noqa: E731
    _receiver = None
    try:
        utils.SETTING_DIR = _tmp8
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp8, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify8_rec)

        bridge = _load_plugin("cast_bridge_plugin", "cast_bridge.py")
        _rec16 = _StateRec()

        class _Bridge(bridge.CastBridgeRenderer):
            @property
            def protocol(self):
                return _rec16

        bridged = _Bridge()
        utils.Setting.set(bridge.SettingProperty.Bridge_Timeout, 3)

        # -- no target chosen ------------------------------------------
        utils.Setting.set(bridge.SettingProperty.Bridge_Target, '')
        _rec16.rows = []
        bridged.set_media_url('http://bridge/nowhere.mp4')
        check("a bridge cast without a target is reported, not attempted",
              _wait_until(lambda: ('error', True) in _rec16.rows, timeout=5),
              str(_rec16.rows))
        check("the report says how to fix it",
              any('Target' in str(row[1]) for row in _rec16.rows
                  if row[0] == 'CurrentTrackTitle'), str(_rec16.rows))

        # -- a real receiver on the loopback ----------------------------
        _CTX.renderer = MockRenderer()
        _receiver = TestProtocol()
        _receiver.start()
        utils.Setting.set(bridge.SettingProperty.Bridge_Target,
                          '127.0.0.1:{}'.format(_receiver.cast_port))
        _rec16.rows = []
        bridged.set_media_url('http://bridge/movie.mp4')
        check("the bridge completes the Cast handshake and loads the url",
              _wait_until(lambda: _CTX.renderer.called('set_media_url'), timeout=20)
              and _CTX.renderer.last_arg('set_media_url') == 'http://bridge/movie.mp4',
              str(_CTX.renderer.calls))
        check("a confirmed load is reported as playing",
              ('transport', 'PLAYING') in _rec16.rows, str(_rec16.rows))

        bridged.set_media_pause()
        check("pause reaches the other device",
              _wait_until(lambda: _CTX.renderer.called('set_media_pause'), timeout=10),
              str(_CTX.renderer.calls))
        bridged.set_media_resume()
        check("resume reaches the other device",
              _wait_until(lambda: _CTX.renderer.called('set_media_resume'), timeout=10),
              str(_CTX.renderer.calls))
        bridged.set_media_stop()
        check("stop reaches the other device",
              _wait_until(lambda: _CTX.renderer.called('set_media_stop'), timeout=10),
              str(_CTX.renderer.calls))
        check("the bridge releases the connection on stop",
              bridged._sender is None)

        # -- an unreachable target --------------------------------------
        utils.Setting.set(bridge.SettingProperty.Bridge_Target, '127.0.0.1:1')
        _rec16.rows = []
        bridged.set_media_url('http://bridge/unreachable.mp4')
        check("an unreachable target is reported instead of hanging",
              _wait_until(lambda: ('error', True) in _rec16.rows, timeout=15),
              str(_rec16.rows))
        check("the failure is announced with the device name",
              any('投屏到' in str(n) or '失败' in str(n) for n in _notify8),
              str(_notify8))
    finally:
        if _receiver is not None:
            try:
                _receiver.stop()
            except Exception:
                pass
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify8_rec)
        except Exception:
            pass
        utils.SETTING_DIR = _saved_dir8
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting8
        _shutil.rmtree(_tmp8, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the Chromecast bridge behaves", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 17: AirPlay audio (RAOP) supervision
#
# The plugin does not implement RAOP; it keeps shairport-sync running with a
# config that carries the Macast name. So the tests are about the supervisor:
# it must start the binary with the right argv, notice a connection from the
# log, and never leave a process behind when it is switched off or when the
# binary is simply not installed.
# --------------------------------------------------------------------------
print("\n=== Part 17: AirPlay audio (RAOP) ===")
try:
    _saved_setting9 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir9 = utils.SETTING_DIR
    _saved_path9 = os.environ.get('PATH', '')
    _tmp9 = _tempfile.mkdtemp(prefix="macast-raop-")
    _notify9 = []
    _notify9_rec = lambda *a, **k: _notify9.append(a)      # noqa: E731
    try:
        utils.SETTING_DIR = _tmp9
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp9, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify9_rec)

        raop = _load_plugin("raop_plugin", "raop.py")
        check("RAOP does not force the SSDP server on",
              raop.AirPlayAudioProtocol.uses_ssdp is False)
        check("a quote in the device name cannot break the generated config",
              '\\"' in raop.config_body('My "Mac"'), raop.config_body('My "Mac"'))

        _bin9 = os.path.join(_tmp9, "bin")
        _argv9 = os.path.join(_tmp9, "argv.txt")
        _fake_shair = _write_fake(
            _bin9, "shairport-sync",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n"
            "echo 'Connection from 10.0.0.9:1234.'\n"
            "exec sleep 30\n".format(_argv9))
        os.environ['PATH'] = _bin9 + os.pathsep + _saved_path9
        check("shairport-sync is found on PATH",
              raop.find_shairport() == _fake_shair, str(raop.find_shairport()))

        protocol9 = raop.AirPlayAudioProtocol()
        protocol9.start()
        check("the supervisor starts the binary", protocol9.running())
        config9 = os.path.join(_tmp9, raop.CONFIG_NAME)
        check("a config carrying the Macast name is written",
              os.path.exists(config9) and 'name = ' in open(config9, encoding='utf-8').read(),
              open(config9, encoding='utf-8').read() if os.path.exists(config9) else 'missing')
        check("the config file is what the binary was pointed at",
              _wait_until(lambda: os.path.exists(_argv9))
              and config9 in open(_argv9, encoding='utf-8').read(),
              open(_argv9, encoding='utf-8').read() if os.path.exists(_argv9) else 'no argv')
        check("an AirPlay client connecting is surfaced to the user",
              _wait_until(lambda: any('AirPlay' in str(n) for n in _notify9), timeout=10),
              str(_notify9))
        protocol9.stop()
        check("stopping the plugin stops the binary", not protocol9.running())

        _real_find9 = raop.find_shairport
        raop.find_shairport = lambda: None
        _notify9[:] = []
        protocol9b = raop.AirPlayAudioProtocol()
        protocol9b.start()
        check("a missing shairport-sync is reported with the install hint",
              any('shairport-sync' in str(n) for n in _notify9), str(_notify9))
        check("a missing binary leaves no process behind", protocol9b._proc is None)
        raop.find_shairport = _real_find9

        # v0.2 gave RAOP its own persisted speaker name; the default must stay
        # exactly what v0.1 did (follow the friendly name), including when the
        # key exists but holds only whitespace.
        check("without an override the RAOP name follows the device name",
              raop.service_name() == utils.Setting.get_friendly_name(),
              repr(raop.service_name()))
        utils.Setting.setting['RAOP_Device_Name'] = '客厅音箱'
        check("RAOP_Device_Name overrides the advertised speaker name",
              raop.service_name() == '客厅音箱', repr(raop.service_name()))
        utils.Setting.setting['RAOP_Device_Name'] = '  '
        check("a whitespace-only override falls back to the device name",
              raop.service_name() == utils.Setting.get_friendly_name(),
              repr(raop.service_name()))
        del utils.Setting.setting['RAOP_Device_Name']
        check("the override lookup never creates the key as a side effect",
              'RAOP_Device_Name' not in utils.Setting.setting,
              str(sorted(utils.Setting.setting)))
    finally:
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify9_rec)
        except Exception:
            pass
        os.environ['PATH'] = _saved_path9
        utils.SETTING_DIR = _saved_dir9
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting9
        _shutil.rmtree(_tmp9, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the AirPlay audio supervisor behaves", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 18: what a real Cast sender stack requires of the receiver
# --------------------------------------------------------------------------
# Every check below corresponds to a concrete expectation of pychromecast (the
# client library mkchromecast and Home Assistant talk to receivers with), and
# each one used to fail. Line references are to pychromecast 14.0.10.
print("\n=== Part 18: Cast receiver conformance ===")


def _cast_msgs(sock):
    """Every CastMessage written to a FakeSock, oldest first."""
    out, buf = [], sock.sent
    while len(buf) >= 4:
        (n,) = struct.unpack(">I", buf[:4])
        if len(buf) < 4 + n:
            break
        out.append(cast.parse_cast_message(buf[4 : 4 + n]))
        buf = buf[4 + n :]
    return out


def _cast_to(proto, sock, ns, payload):
    proto._on_message(sock, {
        "source_id": "sender-0", "destination_id": "receiver-0",
        "namespace": ns, "payload_type": 0,
        "payload_utf8": json.dumps(payload), "payload_binary": b"",
    })


def _last_json(sock):
    msgs = _cast_msgs(sock)
    return json.loads(msgs[-1]["payload_utf8"]) if msgs else {}


_CTX.renderer = MockRenderer()
p18 = TestProtocol()

# The media app must be running before RECEIVER_STATUS carries applications[].
sock = FakeSock()
_cast_to(p18, sock, cast.NS_RECEIVER, {"type": "LAUNCH", "appId": "CC1AD845"})
launch = _last_json(sock)
app = launch["status"]["applications"][0]
# A real device answers "Default Media Receiver" for CC1AD845 and senders key
# off it: mkchromecast's --hijack re-issues LOAD every 5 seconds while the name
# differs, which is an endless restart loop against us (cast.py:410-447).
check("the media receiver reports the name real devices use",
      app.get("displayName") == "Default Media Receiver",
      str(app.get("displayName")))
check("a working receiver does not advertise itself as standby",
      launch["status"].get("isStandBy") is False,
      str(launch["status"].get("isStandBy")))

# SET_VOLUME must be acknowledged. pychromecast sends it and blocks in
# WaitResponse(REQUEST_TIMEOUT=10.0) until a RECEIVER_STATUS carrying the same
# requestId arrives (controllers/receiver.py set_volume, const.py:13); staying
# silent made every volume change raise RequestTimeout on the sender.
sock = FakeSock()
_cast_to(p18, sock, cast.NS_RECEIVER,
         {"type": "SET_VOLUME", "volume": {"level": 0.4}, "requestId": 7})
vol_status = _last_json(sock)
check("SET_VOLUME is acknowledged",
      vol_status.get("type") == "RECEIVER_STATUS", str(vol_status.get("type")))
check("the acknowledgement echoes the sender's requestId",
      vol_status.get("requestId") == 7, str(vol_status.get("requestId")))
check("the reported volume is the one just set",
      vol_status.get("status", {}).get("volume", {}).get("level") == 0.4,
      str(vol_status.get("status", {}).get("volume")))
check("the player was actually asked to change volume",
      _CTX.renderer.last_arg("set_media_volume") == 40,
      str(_CTX.renderer.last_arg("set_media_volume")))

sock = FakeSock()
_cast_to(p18, sock, cast.NS_RECEIVER,
         {"type": "SET_VOLUME", "volume": {"muted": True}, "requestId": 8})
sock = FakeSock()
_cast_to(p18, sock, cast.NS_RECEIVER, {"type": "GET_STATUS", "requestId": 9})
reported = _last_json(sock)["status"]["volume"]
# Senders compute the next volume from what we report (level +/- 0.1), so a
# hardcoded 1.0 froze their slider and lost the mute state entirely.
check("mute survives and the level is not reset to 1.0",
      reported.get("muted") is True and reported.get("level") == 0.4, str(reported))

# Volume arrives off the wire; a bad value must not kill the connection, which
# is what an exception escaping the handler would do.
sock = FakeSock()
_cast_to(p18, sock, cast.NS_RECEIVER,
         {"type": "SET_VOLUME", "volume": {"level": "loud"}, "requestId": 10})
check("a malformed volume level is ignored, not raised on",
      _last_json(sock).get("type") == "RECEIVER_STATUS",
      str(_last_json(sock)))


def _media_entry(proto, request_id=12):
    s = FakeSock()
    _cast_to(proto, s, cast.NS_MEDIA, {"type": "GET_STATUS", "requestId": request_id})
    return json.loads(_cast_msgs(s)[0]["payload_utf8"])["status"][0]


sock_load = FakeSock()
_cast_to(p18, sock_load, cast.NS_MEDIA,
         {"type": "LOAD", "requestId": 11,
          "media": {"contentId": "http://e2e/a.mp4", "contentType": "video/mp4",
                    "streamType": "BUFFERED"}})
check("LOAD is answered with BUFFERING",
      json.loads(_cast_msgs(sock_load)[0]["payload_utf8"])["status"][0]["playerState"]
      == "BUFFERING",
      str(_cast_msgs(sock_load)[0]))

entry = _media_entry(p18)
check("a stream with no news yet is still reported PLAYING",
      entry["playerState"] == "PLAYING" and "idleReason" not in entry, str(entry))

p18.set_state_pause()
entry = _media_entry(p18)
check("GET_STATUS follows the player into PAUSED",
      entry["playerState"] == "PAUSED", str(entry["playerState"]))

p18.set_state_eof()
entry = _media_entry(p18)
# This is the whole point of the observation plumbing: GET_STATUS used to keep
# answering PLAYING for as long as a media descriptor existed, so a video that
# had already ended still looked healthy to the sender.
check("a finished video is reported IDLE, not PLAYING",
      entry["playerState"] == "IDLE", str(entry["playerState"]))
check("a finished video carries idleReason FINISHED",
      entry.get("idleReason") == "FINISHED", str(entry))

# mpv fires `idle` immediately after end-of-file, which maps onto
# set_state_stop(); it must not relabel a finished video as cancelled.
p18.set_state_stop()
entry = _media_entry(p18)
check("the trailing stop event does not downgrade FINISHED",
      entry.get("idleReason") == "FINISHED", str(entry))

sock_load = FakeSock()
_cast_to(p18, sock_load, cast.NS_MEDIA,
         {"type": "LOAD", "requestId": 13,
          "media": {"contentId": "http://e2e/b.mp4", "contentType": "video/mp4",
                    "streamType": "BUFFERED"}})
entry = _media_entry(p18)
check("a new LOAD clears the previous idle reason",
      entry["playerState"] == "PLAYING" and "idleReason" not in entry, str(entry))

# mpv reports the end-file of the file a LOAD just replaced *after* the new one
# is in place. Taking that as the new media's outcome labelled a video that then
# played to completion as CANCELLED -- the live probe found this, no stubbed test
# could have.
p18.set_state_stop()
entry = _media_entry(p18)
check("a superseded media's end-file does not condemn the new one",
      entry["playerState"] == "PLAYING" and "idleReason" not in entry, str(entry))

p18.set_state_play()
p18.set_state_eof()
entry = _media_entry(p18)
check("the new media still reports FINISHED when it really ends",
      entry.get("idleReason") == "FINISHED", str(entry))

# A fresh media, to prove ERROR is reachable and that a terminal reason is
# cleared per LOAD rather than latching onto the next session.
sock_load = FakeSock()
_cast_to(p18, sock_load, cast.NS_MEDIA,
         {"type": "LOAD", "requestId": 14,
          "media": {"contentId": "http://e2e/c.mp4", "contentType": "video/mp4",
                    "streamType": "BUFFERED"}})
p18.set_state_play()
p18.set_state_transport_error()
entry = _media_entry(p18)
check("a playback error is reported as ERROR",
      entry.get("idleReason") == "ERROR", str(entry))
check("MEDIA_STATUS reports the real volume, not a constant",
      entry.get("volume") == {"level": 0.4, "muted": True}, str(entry.get("volume")))

# eureka_info is what pychromecast's host-scanning fallback reads when mDNS is
# unavailable; a value it cannot parse makes it give up on the device entirely.
info = TestProtocol()._make_setup_handler()._eureka_info(
    "/setup/eureka_info?params=device_info,name")
udn = info.get("device_info", {}).get("ssdp_udn")
try:
    uuid.UUID(str(udn).replace("-", ""))
    udn_parses = True
except Exception:
    udn_parses = False
check("ssdp_udn is a bare UUID a client can parse", udn_parses, repr(udn))
check("multi-room support is explicitly denied",
      info["device_info"]["capabilities"].get("multizone_supported") is False,
      str(info["device_info"]["capabilities"]))

# --------------------------------------------------------------------------
# Part 19: the HTTPS setup API on 8443
# --------------------------------------------------------------------------
# pychromecast's get_cast_type -- what the mDNS discovery path calls -- asks
# https://host:8443 and has NO plain-HTTP fallback, so without this listener the
# device_info branch never runs. This part deliberately passes on a machine
# where 8443 is taken (Docker, OrbStack, dev servers): in that case the correct
# behaviour is to degrade silently, and that is what gets checked.
print("\n=== Part 19: Cast HTTPS setup API (8443) ===")

_CTX.renderer = MockRenderer()
p19 = TestProtocol()
p19._ensure_cert()
extra_servers = []
try:
    p19._start_setup_server()
    try:
        p19._start_https_setup_server()
        # Not "did it raise": _start_https_setup_server swallows a taken port
        # on purpose, so the only honest test for "the listener is ours" is the
        # port it recorded.
        started = p19.https_setup_port is not None
    except Exception as e:
        started = False
        check("starting the HTTPS setup server never raises", False, repr(e))

    if started:
        # The whole point: same document, same ?params= filtering, over TLS.
        http_conn = http.client.HTTPConnection("127.0.0.1", p19.setup_port, timeout=5)
        http_conn.request("GET", "/setup/eureka_info?params=device_info,name")
        plain = json.loads(http_conn.getresponse().read().decode())
        http_conn.close()

        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        tls_conn = http.client.HTTPSConnection(
            "127.0.0.1", p19.https_setup_port, timeout=5, context=ctx)
        tls_conn.request("GET", "/setup/eureka_info?params=device_info,name")
        resp = tls_conn.getresponse()
        tls_status = resp.status
        secure = json.loads(resp.read().decode())
        tls_conn.close()

        check("8443 answers the eureka_info request",
              p19.https_setup_port == 8443 and tls_status == 200,
              "port={} status={}".format(p19.https_setup_port, tls_status))
        check("the HTTPS document matches the plain one",
              secure == plain, "{} vs {}".format(secure, plain))
        # Only the two requested top-level keys, and device_info nested.
        check("?params= filtering still applies over TLS",
              set(secure) == {"device_info", "name"}, str(list(secure)))
        check("device_info over TLS carries the bare UUID",
              len(secure["device_info"]["ssdp_udn"]) == 36,
              repr(secure["device_info"]["ssdp_udn"]))

        # A client that does not speak TLS must not take the listener down with
        # it: wrapping the *listener* would run the handshake inside accept(),
        # and one SSLError there is an OSError, which is exactly the mistake
        # that once killed the Cast accept loop (AGENTS.md section 4.1).
        for _ in range(3):
            try:
                raw = socket.create_connection(("127.0.0.1", 8443), timeout=5)
                raw.sendall(b"GET /setup/eureka_info HTTP/1.0\r\n\r\n")
                raw.close()
            except OSError:
                pass
        try:
            tls_conn = http.client.HTTPSConnection(
                "127.0.0.1", 8443, timeout=5, context=ctx)
            tls_conn.request("GET", "/setup/eureka_info")
            still_up = tls_conn.getresponse().status
            tls_conn.close()
        except Exception as e:
            still_up = repr(e)
        check("plain-TCP clients do not kill the HTTPS listener",
              still_up == 200, str(still_up))

        # A second instance must give way, not fight for the port.
        p19b = TestProtocol()
        p19b._ensure_cert()
        try:
            p19b._start_https_setup_server()
            check("a second instance degrades instead of stealing 8443",
                  p19b.https_setup_port is None,
                  "port={}".format(p19b.https_setup_port))
        except Exception as e:
            check("a second instance degrades instead of stealing 8443",
                  False, repr(e))
        if p19b._https_setup_server is not None:
            p19b._https_setup_server.shutdown()
            p19b._https_setup_server.server_close()
    else:
        check("8443 is already in use, so the listener degrades quietly",
              p19.https_setup_port is None,
              "port={}".format(p19.https_setup_port))
        http_conn = http.client.HTTPConnection("127.0.0.1", p19.setup_port, timeout=5)
        http_conn.request("GET", "/setup/eureka_info?params=device_info,name")
        still_serving = http_conn.getresponse().status
        http_conn.close()
        check("the plain 8008 setup API is unaffected when 8443 is taken",
              still_serving == 200, str(still_serving))
finally:
    for srv in extra_servers:
        try:
            srv.shutdown()
            srv.server_close()
        except Exception:
            pass
    for srv in (p19._https_setup_server, p19._setup_server):
        if srv is not None:
            try:
                srv.shutdown()
                srv.server_close()
            except Exception:
                pass

# --------------------------------------------------------------------------
# Part 20: the log file must stay bounded, and reading it must stay cheap
#
# Three separate problems used to line up here, all measured on a live instance
# that had been running for 49 minutes:
#
#   * macast.log is deleted once per start (Macast.py `clear_env`) but nothing
#     capped it *within* a run: 1.35 MB / 49 min = ~27 KB/min = ~40 MB/day.
#     CherryPy's own FileHandler never rotates; the root handler is now the only
#     writer and it rotates.
#   * every request line was written TWICE (2 x 2621 access lines = ~64% of the
#     file): cherrypy's non-rotating handler wrote a bare copy while the record
#     also propagated to the root handler. `log.access_file`/`log.error_file`
#     are now empty, which is only safe *because* those loggers propagate.
#   * /api?query=log returned the whole file and the page rendered it with
#     v-html, so the page got slower as the file grew. It now serves a tail.
# --------------------------------------------------------------------------
print("\n=== Part 20: log rotation, tail API, clear ===")
try:
    import logging
    import logging.handlers
    import cherrypy as _cherrypy

    _entry_path = os.path.join(REPO, "Macast.py")
    # `Macast.py` is the *script* entry point: `from macast import Setting,
    # SETTING_DIR` is what `macast/__init__.py` provides when it runs as the
    # package it normally is. This suite replaces `macast` with a stub package
    # (see the top of the file), so those two names have to be lent to it.
    _pkg_stub = sys.modules["macast"]
    _pkg_stub.Setting = utils.Setting
    _pkg_stub.SETTING_DIR = utils.SETTING_DIR
    _entry_spec = importlib.util.spec_from_file_location("macast_entry", _entry_path)
    macast_entry = importlib.util.module_from_spec(_entry_spec)
    _entry_spec.loader.exec_module(macast_entry)

    _tmp20 = _tempfile.mkdtemp(prefix="macast-log-")
    _saved_dir20_utils = utils.SETTING_DIR
    _saved_dir20_proto = protocol.SETTING_DIR
    _saved_setting20 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_running20 = utils.Setting.is_service_running
    _saved_entry_dir20 = macast_entry.SETTING_DIR
    _root_logger = logging.getLogger()
    _saved_root_handlers20 = list(_root_logger.handlers)
    _added20 = []
    log_path20 = os.path.join(_tmp20, utils.LOG_FILE_NAME)
    try:
        utils.SETTING_DIR = _tmp20
        protocol.SETTING_DIR = _tmp20
        macast_entry.SETTING_DIR = _tmp20
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp20, "macast_setting.json")

        # -- the handler itself ------------------------------------------
        macast_entry.setup_logging()
        _added20 = [h for h in _root_logger.handlers
                    if h not in _saved_root_handlers20]
        _rot20 = [h for h in _added20
                  if isinstance(h, logging.handlers.RotatingFileHandler)]
        check("the log handler rotates instead of appending forever",
              len(_rot20) == 1, str(_added20))
        check("rotation is bounded (<= 8 MB per file, at most 2 backups)",
              bool(_rot20)
              and _rot20[0].maxBytes == macast_entry.LOG_MAX_BYTES
              and _rot20[0].backupCount == macast_entry.LOG_BACKUP_COUNT
              and 0 < macast_entry.LOG_MAX_BYTES <= 8 * 1024 * 1024,
              str([(h.maxBytes, h.backupCount) for h in _rot20]))
        check("the handler writes the one log file name shared with protocol.py",
              bool(_rot20)
              and os.path.basename(_rot20[0].baseFilename) == utils.LOG_FILE_NAME,
              str([h.baseFilename for h in _rot20]))

        # Writing through it for real is the only way to prove the attributes
        # above are not decorative: RotatingFileHandler rolls over once the
        # stream would pass maxBytes, so a bit over 2 MB has to trigger it.
        _probe = logging.getLogger("macast.rotation-probe")
        _payload = "x" * 1024
        for _i in range(4000):
            _probe.warning(_payload)
            if os.path.exists(log_path20 + ".1"):
                break
        check("a log that exceeds the limit is actually rolled over",
              os.path.exists(log_path20 + ".1"),
              str(sorted(os.listdir(_tmp20))))
        check("the rolled-over file stays within the configured size",
              os.path.getsize(log_path20) <= macast_entry.LOG_MAX_BYTES,
              str(os.path.getsize(log_path20)))

        # -- clear_env() removes the log *and* its backups ---------------
        for _name in (utils.LOG_FILE_NAME, utils.LOG_FILE_NAME + ".1",
                      utils.LOG_FILE_NAME + ".2"):
            with open(os.path.join(_tmp20, _name), "w", encoding="utf-8") as _f:
                _f.write("stale\n")
        with open(os.path.join(_tmp20, "macast_setting.json"), "w",
                  encoding="utf-8") as _f:
            _f.write("{}")
        macast_entry.remove_log_files(_tmp20)
        check("a start wipes the log together with its rotated backups",
              not any(os.path.exists(os.path.join(_tmp20, _n)) for _n in
                      (utils.LOG_FILE_NAME, utils.LOG_FILE_NAME + ".1",
                       utils.LOG_FILE_NAME + ".2")),
              str(sorted(os.listdir(_tmp20))))
        check("but it leaves the settings file alone",
              os.path.exists(os.path.join(_tmp20, "macast_setting.json")))

        for _h in _added20:
            _root_logger.removeHandler(_h)
            try:
                _h.close()
            except Exception:
                pass
        _added20 = []

        # -- the tail reader --------------------------------------------
        _big20 = os.path.join(_tmp20, "big.log")
        with open(_big20, "w", encoding="utf-8") as _f:
            for _i in range(5000):
                _f.write("line {}\n".format(_i))
        _text, _truncated, _size = protocol.read_log_tail(_big20, 100)
        _lines = _text.split("\n")
        check("the log reader can return just the tail",
              len(_lines) == 100 and _lines[0] == "line 4900"
              and _lines[-1] == "line 4999", str(_lines[:1] + _lines[-1:]))
        check("...and it reports that it dropped the earlier part",
              _truncated is True and _size == os.path.getsize(_big20),
              "{} / {}".format(_truncated, _size))
        _text, _truncated, _ = protocol.read_log_tail(_big20, 100000)
        check("a log shorter than the requested tail comes back whole",
              _truncated is False and _text.count("\n") == 4999,
              "{} / {}".format(_truncated, _text.count("\n")))
        _text, _truncated, _ = protocol.read_log_tail(_big20, 0)
        check("a nonsense tail is clamped instead of returning nothing",
              _truncated is True and _text.split("\n")[-1] == "line 4999",
              repr(_text[-20:]))

        # The byte budget has to scale with the requested line count: the long
        # SOAP/URL lines in a real log would otherwise cut "10000 lines" down to
        # a couple of thousand without the caller asking for that.
        _long20 = os.path.join(_tmp20, "long.log")
        with open(_long20, "w", encoding="utf-8") as _f:
            for _i in range(6000):
                _f.write("{:05d} {}\n".format(_i, "y" * 200))
        _text, _truncated, _ = protocol.read_log_tail(_long20, 10000)
        check("asking for many lines is not silently cut short by the byte cap",
              _truncated is False and _text.count("\n") == 5999
              and _text.split("\n")[-1].startswith("05999"),
              "{} lines / truncated={}".format(_text.count("\n"), _truncated))

        # A byte-capped window almost always starts inside a multi-byte
        # character (Chinese media titles land in the log all the time); a
        # strict decode would raise UnicodeDecodeError there.
        _utf20 = os.path.join(_tmp20, "utf.log")
        with open(_utf20, "w", encoding="utf-8") as _f:
            for _i in range(4000):
                _f.write("中文标题-{}\n".format(_i))
        _text, _truncated, _ = protocol.read_log_tail(_utf20, 4000, max_bytes=4096)
        _lines = _text.split("\n")
        check("a byte-limited window never raises on a split UTF-8 character",
              _truncated is True and _lines[-1] == "中文标题-3999",
              repr(_lines[-1:]))
        check("and the partial first line is dropped, not half-rendered",
              all(ln.startswith("中文标题-") for ln in _lines),
              repr(_lines[0]))

        try:
            protocol.read_log_tail(os.path.join(_tmp20, "missing.log"))
            _missing20 = False
        except OSError:
            _missing20 = True
        check("a missing log file raises OSError for the API to swallow",
              _missing20 is True)

        # -- the HTTP surface -------------------------------------------
        utils.Setting.is_service_running = staticmethod(lambda: True)

        class _LogHandler(protocol.Handler):
            """Skips the real __init__ (it reads the settings page off disk)."""

            def __init__(self):
                pass

            @property
            def protocol(self):
                return types.SimpleNamespace()

        handler20 = _LogHandler()
        request20 = _cherrypy.serving.request
        _saved_params20 = request20.params
        _saved_remote20 = getattr(request20, "remote", None)
        _saved_scheme20 = request20.scheme
        _saved_headers20 = dict(request20.headers)
        try:
            def _reset20(ip="127.0.0.1", token=None, scheme="http"):
                request20.headers.clear()
                for _k, _v in _saved_headers20.items():
                    request20.headers[_k] = _v
                request20.params = {"token": token} if token else {}
                request20.remote = types.SimpleNamespace(ip=ip)
                request20.scheme = scheme

            def _get20(**kw):
                return json.loads(handler20.GET(param="api", **kw).decode())

            def _post20(**kw):
                return json.loads(handler20.POST(**kw).decode())

            # The page's file: one line per record, 5000 of them.
            with open(log_path20, "w", encoding="utf-8") as _f:
                for _i in range(5000):
                    _f.write("entry {}\n".format(_i))

            _reset20()
            _res = _get20(query="log")
            check("the log API defaults to the tail, not the whole file",
                  _res.get("lines") == protocol.LOG_TAIL_LINES
                  and _res.get("truncated") is True
                  and _res.get("logs", "").split("\n")[-1] == "entry 4999",
                  str({k: v for k, v in _res.items() if k != "logs"}))
            check("the payload still carries the key older pages expect",
                  "logs" in _res and _res.get("size") == os.path.getsize(log_path20),
                  str(_res.get("size")))

            _res = _get20(query="log", tail="100")
            check("?tail= is honoured",
                  _res.get("lines") == 100
                  and _res.get("logs", "").split("\n")[0] == "entry 4900",
                  str(_res.get("lines")))
            _res = _get20(query="log", all="1")
            check("?all=1 still asks for everything (bounded by the reader)",
                  _res.get("lines") == 5000 and _res.get("truncated") is False,
                  str(_res.get("lines")))

            _reset20(ip="192.168.1.9")
            check("the log is not readable from the LAN without the token",
                  _get20(query="log").get("code") == 403)
            check("the full-log download is gated the same way",
                  _get20(query="log-download").get("code") == 403)

            _reset20()
            _blob = handler20.GET(param="api", query="log-download")
            check("log-download hands back the raw file",
                  isinstance(_blob, bytes)
                  and _blob.startswith(b"entry 0\n")
                  and _blob.count(b"\n") == 5000,
                  "{} bytes".format(len(_blob) if isinstance(_blob, bytes) else -1))
            check("log-download is marked as an attachment, not a page",
                  "attachment" in
                  str(_cherrypy.serving.response.headers.get("Content-Disposition", "")),
                  str(dict(_cherrypy.serving.response.headers)))

            _reset20(ip="192.168.1.9")
            _res = _post20(**{"clear-log": "1"})
            check("clear-log refuses an untokened LAN caller",
                  _res.get("code") == 403
                  and os.path.getsize(log_path20) > 0, str(_res))
            _reset20()
            _res = _post20(**{"clear-log": "1"})
            check("clear-log truncates the file from the settings page",
                  _res.get("code") == 0 and os.path.getsize(log_path20) == 0,
                  "{} / {}".format(_res, os.path.getsize(log_path20)))
            _res = _get20(query="log")
            check("an empty (or missing) log is an empty box, not an error",
                  _res.get("logs") == "" and _res.get("lines") == 0, str(_res))
            os.remove(log_path20)
            check("a missing log file is still not an error",
                  _get20(query="log").get("logs") == "")
        finally:
            request20.params = _saved_params20
            request20.remote = _saved_remote20
            request20.scheme = _saved_scheme20
            request20.headers.clear()
            for _k, _v in _saved_headers20.items():
                request20.headers[_k] = _v

        # -- the two configuration decisions behind all of that ----------
        #
        # `log.access_file` was emptied on the strength of one assumption:
        # cherrypy's loggers propagate to the root logger, which is what writes
        # macast.log (with rotation). If that ever stops being true, access
        # lines would silently disappear instead of merely being duplicated.
        _root_seen = []
        _root_probe = logging.Handler()
        _root_probe.emit = lambda rec: _root_seen.append(rec.name)
        _access_logger = logging.getLogger("cherrypy.access")
        # CherryPy may already have a screen handler attached (that is the noise
        # `log.screen` turns off once the real app starts); park them so the
        # probe only exercises the root path.
        _access_handlers = list(_access_logger.handlers)
        for _h in _access_handlers:
            _access_logger.removeHandler(_h)
        _root_logger.addHandler(_root_probe)
        try:
            _access_logger.info("127.0.0.1 - - probe")
        finally:
            _root_logger.removeHandler(_root_probe)
            for _h in _access_handlers:
                _access_logger.addHandler(_h)
        check("cherrypy access records still reach the rotating root handler",
              "cherrypy.access" in _root_seen, str(_root_seen))

        with open(os.path.join(MACAST, "server.py"), "r", encoding="utf-8") as _f:
            _server_src = _f.read()
        check("cherrypy no longer owns a second, non-rotating writer on macast.log",
              "'log.access_file': ''" in _server_src
              and "'log.error_file': ''" in _server_src
              and "log.access_file': os.path.join" not in _server_src,
              "log.access_file still points at a file"
              if "log.access_file': os.path.join" in _server_src else "")

        with open(os.path.join(MACAST, "xml", "setting.html"), "r",
                  encoding="utf-8") as _f:
            _html20 = _f.read()
        check("the page renders the log as text, not through v-html",
              'v-html="macast_log"' not in _html20
              and "{{ macast_log }}" in _html20)
        check("the page asks for a tail and offers clear/download",
              "query=log&" in _html20 and "clear-log" in _html20
              and "log-download" in _html20)
        _mounted20 = _html20.split("mounted()", 1)[1].split("beforeDestroy", 1)[0]
        check("the page no longer pulls the log on every page load",
              "this.get_log();" not in _mounted20,
              "".join(ln for ln in _mounted20.splitlines() if "get_log" in ln))
    finally:
        for _h in _added20:
            _root_logger.removeHandler(_h)
            try:
                _h.close()
            except Exception:
                pass
        utils.SETTING_DIR = _saved_dir20_utils
        protocol.SETTING_DIR = _saved_dir20_proto
        macast_entry.SETTING_DIR = _saved_entry_dir20
        (utils.Setting.setting, utils.Setting.setting_path) = _saved_setting20
        utils.Setting.is_service_running = staticmethod(_saved_running20)
        _shutil.rmtree(_tmp20, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("log handling behaves", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 21: the screen mirror plugin (live capture -> HTTP -> Cast sender)
#
# Same gift as Part 16: the plugin's counterpart is a protocol Macast itself
# implements, so a real ChromecastProtocol on the loopback proves the whole
# handshake. The only stand-in is ffmpeg itself -- a shell script that prints
# a canned avfoundation device list and then emits TS-shaped bytes forever.
# Real screen capture is never touched (and the plugin's ffmpeg lookup is
# monkeypatched so a Homebrew ffmpeg on this machine cannot leak in).
# --------------------------------------------------------------------------
print("\n=== Part 21: screen mirror plugin ===")
try:
    _saved_setting21 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir21 = utils.SETTING_DIR
    _tmp21 = _tempfile.mkdtemp(prefix="macast-mirror-")
    _notify21 = []
    _notify21_rec = lambda *a, **k: _notify21.append(a)      # noqa: E731
    _receiver21 = None
    try:
        utils.SETTING_DIR = _tmp21
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp21, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify21_rec)

        mirror = _load_plugin("screen_mirror_plugin", "screen_mirror.py")
        fake_ffmpeg21 = _write_fake(os.path.join(_tmp21, "bin"), "ffmpeg", r"""#!/bin/sh
case "$*" in
  *list_devices*)
    printf '%s\n' \
      'AVFoundation input device list has 3 items:' \
      '[AVFoundation indev @ 0x1] AVFoundation video devices:' \
      '[AVFoundation indev @ 0x1] [0] FaceTime HD Camera' \
      '[AVFoundation indev @ 0x1] [1] Capture screen 0' \
      '[AVFoundation indev @ 0x1] AVFoundation audio devices:' \
      '[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.2
done
""")
        _saved_find21 = mirror.find_ffmpeg
        _saved_search21 = mirror.start_search
        _saved_devices21 = mirror._devices
        _saved_cache21 = dict(mirror._capture_cache)
        mirror.start_search = lambda: True    # the menu must not touch zeroconf

        _rec21 = _StateRec()

        class _Mirror21(mirror.ScreenMirrorRenderer):
            @property
            def protocol(self):
                return _rec21

        mir21 = _Mirror21()

        # -- capture probing --------------------------------------------------
        cap21 = mirror.probe_capture(fake_ffmpeg21)
        check("the screen is found by name and indexed within the video list",
              cap21 is not None and cap21.inputs[0][-1] == '1:none',
              str(cap21.inputs if cap21 else None))
        _cmd21 = mirror.build_ffmpeg_command(fake_ffmpeg21, cap21, 720, 5000000)
        check("without a BlackHole device the mirror stays video only",
              cap21.audio_map is None and '-an' in _cmd21, str(_cmd21))

        fake_bh21 = _write_fake(os.path.join(_tmp21, "binbh"), "ffmpeg", r"""#!/bin/sh
case "$*" in
  *list_devices*)
    printf '%s\n' \
      'AVFoundation input device list has 4 items:' \
      '[AVFoundation indev @ 0x1] AVFoundation video devices:' \
      '[AVFoundation indev @ 0x1] [0] FaceTime HD Camera' \
      '[AVFoundation indev @ 0x1] [1] Capture screen 0' \
      '[AVFoundation indev @ 0x1] AVFoundation audio devices:' \
      '[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone' \
      '[AVFoundation indev @ 0x1] [1] BlackHole 2ch' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.2
done
""")
        cap_bh21 = mirror.probe_capture(fake_bh21)
        _cmd_bh21 = mirror.build_ffmpeg_command(fake_bh21, cap_bh21, 720, 5000000)
        check("a BlackHole device turns system audio on",
              cap_bh21.audio_map == '0:a:0'
              and cap_bh21.inputs[0][-1] == '1:1'
              and '-c:a' in _cmd_bh21 and 'aac' in _cmd_bh21,
              str(_cmd_bh21))

        # -- the other two platforms' dispatch (pure builders; the platform
        #    argument is the seam, nothing about this process changes) ------
        cap_win21 = mirror.probe_capture(fake_ffmpeg21, 'win32')
        _cmd_win21 = mirror.build_ffmpeg_command(fake_ffmpeg21, cap_win21, 720, 5000000)
        # This fake answers the avfoundation shape, so the dshow device table
        # comes back empty and Windows is the video-only case -- the audio half
        # needs a device that carries the output, and Part 39 feeds the parser
        # a real dshow listing to prove it turns on when one is there.
        check("windows dispatches to gdigrab, video only with no loopback device",
              'gdigrab' in _cmd_win21 and '-an' in _cmd_win21, str(_cmd_win21))

        _saved_disp21 = os.environ.pop('DISPLAY', None)
        _saved_pulse21 = mirror._default_pulse_monitor
        try:
            mirror._default_pulse_monitor = lambda: None
            check("linux without DISPLAY refuses instead of guessing",
                  mirror.probe_capture(fake_ffmpeg21, 'linux') is None)
            os.environ['DISPLAY'] = ':0'
            mirror._capture_cache.pop((fake_ffmpeg21, 'linux', True), None)
            cap_lin21 = mirror.probe_capture(fake_ffmpeg21, 'linux')
            check("linux dispatches to x11grab on the session display",
                  'x11grab' in cap_lin21.inputs[0]
                  and ':0+0,0' in cap_lin21.inputs[0]
                  and cap_lin21.audio_map is None,
                  str(cap_lin21.inputs))
            mirror._default_pulse_monitor = lambda: 'alsa_out.monitor'
            mirror._capture_cache.pop((fake_ffmpeg21, 'linux', True), None)
            cap_lina21 = mirror.probe_capture(fake_ffmpeg21, 'linux')
            _cmd_lina21 = mirror.build_ffmpeg_command(fake_ffmpeg21, cap_lina21,
                                                      720, 5000000)
            check("a PulseAudio monitor sink rides along as a second input",
                  cap_lina21.audio_map == '1:a:0'
                  and 'alsa_out.monitor' in cap_lina21.inputs[1]
                  and '1:a:0' in _cmd_lina21 and 'aac' in _cmd_lina21,
                  str(_cmd_lina21))
        finally:
            mirror._default_pulse_monitor = _saved_pulse21
            if _saved_disp21 is not None:
                os.environ['DISPLAY'] = _saved_disp21
            else:
                os.environ.pop('DISPLAY', None)
        mirror._capture_cache.clear()

        # -- no ffmpeg ------------------------------------------------------
        mirror.find_ffmpeg = lambda: None
        utils.Setting.set(mirror.SettingProperty.Mirror_Target, '127.0.0.1:9')
        _rec21.rows = []
        mir21.start_mirror()
        check("a missing ffmpeg is reported, not attempted",
              _wait_until(lambda: ('error', True) in _rec21.rows, timeout=5),
              str(_rec21.rows))
        check("the message names the fix",
              'ffmpeg' in str(_notify21), str(_notify21))

        # -- a real receiver, a fake encoder --------------------------------
        mirror.find_ffmpeg = lambda: fake_ffmpeg21
        _CTX.renderer = MockRenderer()
        _receiver21 = TestProtocol()
        _receiver21.start()
        utils.Setting.set(mirror.SettingProperty.Mirror_Target,
                          '127.0.0.1:{}'.format(_receiver21.cast_port))
        _notify21.clear()
        mir21.start_mirror()
        check("the mirror completes the Cast handshake and loads a live url",
              _wait_until(lambda: _CTX.renderer.called('set_media_url'),
                          timeout=20)
              and '/stream/' in str(_CTX.renderer.last_arg('set_media_url'))
              and str(_CTX.renderer.last_arg('set_media_url')).endswith('.ts'),
              str(_CTX.renderer.calls))
        check("the mirror is reported as playing",
              ('transport', 'PLAYING') in _rec21.rows, str(_rec21.rows))
        check("the start is announced",
              any('镜像' in str(n) for n in _notify21), str(_notify21))

        _live_url21 = str(_CTX.renderer.last_arg('set_media_url'))
        _port21 = int(_live_url21.rsplit(':', 1)[1].split('/')[0])
        _path21 = '/' + _live_url21.split('/', 3)[3]
        _conn21 = http.client.HTTPConnection('127.0.0.1', _port21, timeout=10)
        _conn21.request('GET', _path21)
        _resp21 = _conn21.getresponse()
        _body21 = _resp21.read(4096)
        _conn21.close()
        check("the encoder output is served to the TV",
              _resp21.status == 200 and b'TTTT' in _body21,
              "{} / {} bytes".format(_resp21.status, len(_body21)))
        check("the stream is typed as live MPEG-TS and uncacheable",
              _resp21.getheader('Content-Type') == 'video/mp2t'
              and _resp21.getheader('Cache-Control') == 'no-store',
              "{} / {}".format(_resp21.getheader('Content-Type'),
                               _resp21.getheader('Cache-Control')))

        # -- a pushed url wins over the mirror -------------------------------
        mir21.set_media_url('http://mirror/movie.mp4')
        check("a DLNA push takes the device away from the live mirror",
              _wait_until(lambda: _CTX.renderer.last_arg('set_media_url')
                          == 'http://mirror/movie.mp4', timeout=20)
              and not mir21.is_mirroring(),
              str(_CTX.renderer.calls))
        check("the capture process is released when the mirror yields",
              _wait_until(lambda: mir21._proc is None, timeout=10),
              "ffmpeg still owned")

        mir21.set_media_stop()
        check("stop releases the sender",
              _wait_until(lambda: mir21._sender is None
                          and not mir21.is_mirroring(), timeout=10),
              str(_rec21.rows))

        # -- an unreachable target --------------------------------------------
        utils.Setting.set(mirror.SettingProperty.Mirror_Target, '127.0.0.1:1')
        _rec21.rows = []
        mir21.start_mirror()
        check("an unreachable target is reported instead of hanging",
              _wait_until(lambda: ('error', True) in _rec21.rows, timeout=25),
              str(_rec21.rows))
        check("a failed start leaves no capture behind",
              not mir21.is_mirroring()
              and _wait_until(lambda: mir21._proc is None, timeout=10),
              str(_rec21.rows))

        # -- our own receiver is not a device to mirror to --------------------
        #
        # Found on a real machine, not invented: Macast broadcasts
        # `_googlecast._tcp` exactly like a Chromecast does, so the Chromecast
        # row's first entry was *this instance* -- and「投给我自己」is a loop, not
        # a target. The DLNA search already dropped itself; this is the same
        # rule on the other half of the list.
        _restore21 = _own_address(mirror.Setting)
        try:
            with _FakeMdns([('Living-Room@abc._googlecast._tcp.local.',
                             '客厅的电视', '192.0.2.77', 8009),
                            ('Macast@abc._googlecast._tcp.local.',
                             'Macast 自己', '192.0.2.1', 8009)]):
                _found21 = mirror.discover(timeout=0.01)
        finally:
            _restore21()
        check("the Chromecast search does not offer this machine as a target",
              [host for _name, host, _port in _found21] == ['192.0.2.77'],
              str(_found21))

        # -- the console's view model (the menu kept only the door) ----------
        mirror._devices = [('Living Room TV', '192.0.2.7', 8009)]
        # console_state() kicks a stale search, which is the window's business
        # and not this check's: a live mDNS browse here would only be noise.
        _kick21, _kick_dlna21 = mirror.start_search, mirror.start_renderer_search
        mirror.start_search = mirror.start_renderer_search = lambda: None
        try:
            mirror._capture_cache.clear()
            _st21 = mirror.ScreenMirrorSetting().console_state()
            _texts21 = _console_texts(_st21)
            check("the console offers every output target with its trade-off",
                  len(_st21['output']['options']) == 4
                  and all(o['label'] and o['hint']
                          for o in _st21['output']['options']),
                  str(_st21['output']['options']))
            check("the console keeps a way out of a live capture",
                  'stop' in mirror.ScreenMirrorSetting.CONSOLE_ACTIONS
                  and _st21['console_version'] == mirror.CONSOLE_VERSION,
                  str(_st21['console_version']))
            check("before the first probe the console defers the audio state",
                  '系统声音：开始镜像后' in _st21['audio']['line']
                  and not _st21['capture']['probed'], _texts21)
            if sys.platform == 'darwin':
                check("before the first probe the console already offers the one click",
                      _st21['audio']['setup_available'], _st21['audio'])
            mirror.probe_capture(fake_ffmpeg21)          # the video-only fake
            _st21 = mirror.ScreenMirrorSetting().console_state()
            _texts21 = _console_texts(_st21)
            check("a BlackHole-less probe names the fix instead of staying mute",
                  any('blackhole' in t.lower() for t in _texts21), _texts21)
            mirror._capture_cache.clear()
            mirror.probe_capture(fake_bh21)              # the BlackHole fake
            _st21 = mirror.ScreenMirrorSetting().console_state()
            check("with system audio captured the console says so",
                  '系统声音：已启用' in _st21['audio']['line']
                  and _st21['audio']['capturable'], _st21['audio']['line'])
            if sys.platform == 'darwin':
                check("once audio works the one-click entry steps aside",
                      not _st21['audio']['setup_available'], _st21['audio'])
            _rows21 = {r['key']: r for r in _st21['channels']}
            _devs21 = [d for r in _st21['channels'] for d in r['devices']]
            check("the device list is carried as id plus label, not markup",
                  all(d['id'] and d['label'] for d in _devs21), str(_devs21))
            check("each protocol answers only for itself",
                  set(_rows21) == {'cast', 'caststream', 'dlna', 'browser'}
                  and _rows21['cast']['words'].startswith('发现 ')
                  and _rows21['dlna']['words'] != _rows21['cast']['words']
                  and _rows21['browser']['words'] == ''
                  and _rows21['browser']['needs_device'] is False,
                  str([(k, v['words']) for k, v in sorted(_rows21.items())]))
        finally:
            mirror.start_search, mirror.start_renderer_search = _kick21, _kick_dlna21

        # -- v0.3: the assisted BlackHole install --------------------------------
        _good21 = ('{"url":"https://existential.audio/BlackHole2ch-0.8.0.pkg",'
                   '"sha256":"__SHA__"}').replace('__SHA__', 'a' * 64).encode()
        _url21, _sha21 = mirror.blackhole_pkg_source(_good21)
        check("the cask API decides what gets downloaded",
              _url21.endswith('0.8.0.pkg') and _sha21 == 'a' * 64,
              "{} / {}".format(_url21, _sha21[:8]))
        for _junk21 in (b'not json', b'{}',
                        b'{"url":"https://x/tool.exe","sha256":"' + b'b' * 64 + b'"}',
                        b'{"url":"https://x/a.pkg","sha256":"short"}'):
            _u21, _s21 = mirror.blackhole_pkg_source(_junk21)
            if (_u21, _s21) != (mirror.BLACKHOLE_PKG_URL,
                                mirror.BLACKHOLE_PKG_SHA256):
                break
        else:
            _u21, _s21 = mirror.BLACKHOLE_PKG_URL, mirror.BLACKHOLE_PKG_SHA256
        check("an unusable cask answer falls back to the pinned pkg",
              (_u21, _s21) == (mirror.BLACKHOLE_PKG_URL,
                               mirror.BLACKHOLE_PKG_SHA256), str(_junk21))

        _stub21 = {}
        _audio_saved = {}
        _real_route21 = mirror._route_audio_through_blackhole
        import types as _types21

        class _Sub21(object):
            """Stands in for mirror.subprocess while the assisted install is
            stubbed: a test must never `open` a real installer, and never
            shell out to pkgutil/osascript either."""

            def __init__(self):
                self.calls = []

            def run(self, cmd, **kwargs):
                self.calls.append(list(cmd))
                return _types21.SimpleNamespace(returncode=0, stdout='',
                                                stderr='')

        _sub21 = _Sub21()

        def _fetch21(url, on_bytes=None):
            _stub21['fetched'] = url
            if on_bytes is not None:
                on_bytes(1024, 2048)
            return '/tmp/bh.pkg'

        def _verify21(path, expected):
            _stub21['verified'] = (path, expected)
            return True

        def _stub_audio21(**over):
            for _name, _fn in [('find_ffmpeg', lambda: 'ffmpeg'),
                               ('_has_blackhole', lambda f: False),
                               ('blackhole_pkg_pair',
                                lambda: ('https://x/BlackHole2ch-0.8.0.pkg',
                                         'a' * 64, 'stub-meta')),
                               ('_fetch_blackhole_pkg', _fetch21),
                               ('verify_blackhole_pkg', _verify21),
                               ('_wait_for_blackhole',
                                lambda f, timeout=0.0, progress=None,
                                       interval=5.0, **kw: 'capturable'),
                               ('_route_audio_through_blackhole',
                                lambda f, progress=None: True),
                               ('_open_audio_midi_setup', lambda rep: None),
                               ('_blackhole_driver_installed', lambda: False),
                               ('_blackhole_receipt_present', lambda: False),
                               ('_reload_coreaudiod', lambda: (False, 'stub')),
                               ('subprocess', _sub21),
                               ('_audio_devices', lambda: []),
                               ('_find_blackhole', lambda f: None),
                               ('_default_output', lambda: None),
                               ('_set_default_output', lambda d: True),
                               ('_create_aggregate', lambda uids: None)]:
                if _name not in _audio_saved:
                    _audio_saved[_name] = getattr(mirror, _name)
                setattr(mirror, _name, over.get(_name, _fn))

        def _unstub_audio21():
            for _name, _fn in _audio_saved.items():
                setattr(mirror, _name, _fn)
            _audio_saved.clear()

        try:
            _stub21.clear()
            del _sub21.calls[:]
            _stub_audio21()
            mirror._capture_cache[('ffmpeg', 'darwin', True)] = object()
            _msgs21 = []
            _ok21 = mirror.setup_system_audio(_msgs21.append)
            check("the assisted setup fetches, verifies, opens the installer, "
                  "routes and clears the capture cache",
                  _ok21
                  and _stub21.get('fetched', '').endswith('.pkg')
                  and _stub21.get('verified') == ('/tmp/bh.pkg', 'a' * 64)
                  and ['open', '/tmp/bh.pkg'] in _sub21.calls
                  and mirror._capture_cache == {},
                  "{} / {}".format(_stub21, _sub21.calls))
            _stub21.clear()
            _msgs21 = []

            def _route_fail21(f, progress=None):
                _msgs21.append('routing')
                return False

            _stub_audio21(_route_audio_through_blackhole=_route_fail21)
            _ok21 = mirror.setup_system_audio(_msgs21.append)
            check("a routing failure degrades to the manual pane",
                  not _ok21 and 'routing' in _msgs21, str(_msgs21))

            # CoreAudio stubs only -- nothing is touched on this machine
            _stub_audio21(
                _route_audio_through_blackhole=_real_route21,
                _has_blackhole=lambda f: True,
                _audio_devices=lambda: [(7, 'builtin-uid'),
                                        (8, 'BlackHole2ch-uid')],
                _find_blackhole=lambda f: (8, 'BlackHole2ch-uid'),
                _default_output=lambda: 7,
                _create_aggregate=lambda uids: _stub21.setdefault(
                    'created', uids) and 99)
            utils.Setting.unset(mirror.SettingProperty.Mirror_Audio_Aggregate)
            utils.Setting.unset(mirror.SettingProperty.Mirror_Audio_Original)
            _ok21 = mirror._route_audio_through_blackhole('ffmpeg')
            check("the aggregate mixes BlackHole with the real speakers",
                  _ok21 and _stub21.get('created') == ['BlackHole2ch-uid',
                                                       'builtin-uid'],
                  str(_stub21))
            check("the routing remembers the original output to restore later",
                  utils.Setting.get(mirror.SettingProperty.Mirror_Audio_Original,
                                    None) == 7
                  and utils.Setting.get(mirror.SettingProperty.Mirror_Audio_Aggregate,
                                        None) == 99,
                  str(utils.Setting.setting))
            _stub21.clear()
            _stub_audio21(
                _audio_devices=lambda: [(99, mirror.MACAST_AGGREGATE_UID),
                                        (8, 'BlackHole2ch-uid')],
                _create_aggregate=lambda uids: _stub21.setdefault(
                    'created', uids) and 123)
            _ok21 = mirror._route_audio_through_blackhole('ffmpeg')
            check("a second run reuses the aggregate instead of stacking one",
                  _ok21 and 'created' not in _stub21
                  and utils.Setting.get(mirror.SettingProperty.Mirror_Audio_Aggregate,
                                        None) == 99,
                  str(_stub21))
            _stub_audio21(_route_audio_through_blackhole=_real_route21,
                          _audio_devices=lambda: [])
            check("with no CoreAudio device visible the route fails softly",
                  mirror._route_audio_through_blackhole('ffmpeg') is False)

            utils.Setting.unset(mirror.SettingProperty.Mirror_Audio_Original)
            check("restore without a saved output does nothing",
                  mirror.restore_system_audio() is False)
            utils.Setting.set(mirror.SettingProperty.Mirror_Audio_Original, 7)
            _set21 = []
            _stub_audio21(_set_default_output=lambda d: _set21.append(d) or True)
            check("restore points the default output back at the speakers",
                  mirror.restore_system_audio() is True and _set21 == [7],
                  str(_set21))
        finally:
            _unstub_audio21()
            mirror._capture_cache.clear()
            utils.Setting.unset(mirror.SettingProperty.Mirror_Audio_Aggregate)
            utils.Setting.unset(mirror.SettingProperty.Mirror_Audio_Original)
    finally:
        mirror.find_ffmpeg = _saved_find21
        mirror.start_search = _saved_search21
        mirror._devices = _saved_devices21
        mirror._capture_cache.clear()
        mirror._capture_cache.update(_saved_cache21)
        if _receiver21 is not None:
            try:
                _receiver21.stop()
            except Exception:
                pass
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify21_rec)
        except Exception:
            pass
        utils.SETTING_DIR = _saved_dir21
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting21
        _shutil.rmtree(_tmp21, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the screen mirror plugin behaves", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 22: screen mirror v0.4 -- the browser target and the capture options
#
# Part 21 proved the capture -> encode -> broadcast -> Chromecast chain. This
# part covers what v0.4 added, where the interesting failures are: a second
# muxer behind the same pump, the replay asymmetry (a browser can attach
# mid-stream; a TV handed a backlog sits seconds behind forever), the two
# credentials the HTTP endpoint carries, and the capture choices that reach
# ffmpeg through a *cached* probe.
# --------------------------------------------------------------------------
print("\n=== Part 22: screen mirror v0.4 (browser target, capture options) ===")
try:
    _saved_setting22 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir22 = utils.SETTING_DIR
    _tmp22 = _tempfile.mkdtemp(prefix="macast-mirror22-")
    _notify22 = []
    _notify22_rec = lambda *a, **k: _notify22.append(a)      # noqa: E731
    _server22 = None
    _mir22 = None
    try:
        utils.SETTING_DIR = _tmp22
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp22, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify22_rec)

        mirror = _load_plugin("screen_mirror_plugin_v04", "screen_mirror.py")
        fake_ffmpeg22 = _write_fake(os.path.join(_tmp22, "bin"), "ffmpeg", r"""#!/bin/sh
case "$*" in
  *list_devices*)
    printf '%s\n' \
      'AVFoundation input device list has 4 items:' \
      '[AVFoundation indev @ 0x1] AVFoundation video devices:' \
      '[AVFoundation indev @ 0x1] [0] Capture screen 0' \
      '[AVFoundation indev @ 0x1] [1] Capture screen 1' \
      '[AVFoundation indev @ 0x1] [2] Capture screen 2' \
      '[AVFoundation indev @ 0x1] AVFoundation audio devices:' \
      '[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.2
done
""")
        _rec22 = _StateRec()

        class _Mirror22(mirror.ScreenMirrorRenderer):
            @property
            def protocol(self):
                return _rec22

        mir22 = _Mirror22()
        _mir22 = mir22
        mirror.find_ffmpeg = lambda: fake_ffmpeg22
        mirror.start_search = lambda: True
        mirror._devices = []

        # -- one muxer per output target ----------------------------------
        _cap22 = mirror._Capture('test', [['-f', 'test', '-i', 'x']])
        _ts22 = mirror.build_ffmpeg_command('ffmpeg', _cap22, 720, 5000000,
                                            kind='cast')
        _mp22 = mirror.build_ffmpeg_command('ffmpeg', _cap22, 720, 5000000,
                                           kind='browser')
        check("the browser target muxes fragmented MP4 into the pipe",
              'mp4' in _mp22
              and 'frag_keyframe+empty_moov+default_base_moof' in ''.join(_mp22)
              and 'pipe:1' in _mp22 and 'mpegts' not in _mp22, str(_mp22))
        check("the Chromecast target still muxes MPEG-TS",
              'mpegts' in _ts22 and 'mp4' not in _ts22, str(_ts22))
        check("the browser opens a keyframe twice a second, the TVs once",
              _mp22[_mp22.index('-g') + 1] == str(mirror.FPS // 2)
              and _ts22[_ts22.index('-g') + 1] == str(mirror.FPS), str(_mp22))
        check("the shorter GOP is bought only where a viewer joins live",
              mirror.gop_size('browser') == mirror.FPS // 2
              and all(mirror.gop_size(k) == mirror.FPS
                      for k in ('cast', 'dlna', 'caststream')),
              str([(k, mirror.gop_size(k)) for k in mirror.OUTPUTS]))

        # -- quality presets -------------------------------------------------
        check("the four presets name a height and a bitrate",
              mirror.QUALITIES['360'] == (360, 2000000)
              and mirror.QUALITIES['1080'] == (1080, 6000000)
              and mirror.QUALITIES['source'][0] == 0, str(mirror.QUALITIES))
        check("no preset outruns a Wi-Fi link, because an uncapped bitrate is "
              "the latency the user complains about and a blurred picture",
              max(rate for _h, rate in mirror.QUALITIES.values()) <= 8000000
              and all(mirror.rate_caps(rate) == [
                  '-maxrate', str(int(rate * 1.5)), '-bufsize', str(rate)]
                  for _h, rate in mirror.QUALITIES.values()),
              str(mirror.QUALITIES))
        check("the source preset asks for no scaling at all",
              '-vf' not in mirror.build_ffmpeg_command('ffmpeg', _cap22, 0,
                                                       12000000))
        check("a preset with a height scales to even edges",
              _ts22[_ts22.index('-vf') + 1] == 'scale=-2:720', str(_ts22))
        check("the encoder is chosen per target, not baked into the pipeline",
              'h264_videotoolbox' in mirror.build_ffmpeg_command(
                  'ffmpeg', _cap22, 720, 5000000, encoder='hardware')
              and 'h264_videotoolbox' not in mirror.build_ffmpeg_command(
                  'ffmpeg', _cap22, 720, 5000000, encoder='software'),
              str(_mp22))

        # -- what the encoder probe decides, and how often ------------------
        _count22 = os.path.join(_tmp22, "enc_calls")
        _vt22 = _write_fake(os.path.join(_tmp22, "binenc"), "ffmpeg-vt",
                            '#!/bin/sh\necho x >> "%s"\n'
                            'printf "h264_videotoolbox\\n"\n' % _count22)
        _no22 = _write_fake(os.path.join(_tmp22, "binenc"), "ffmpeg-no",
                            '#!/bin/sh\necho x >> "%s"\nprintf "libx264\\n"\n'
                            % _count22)
        check("VideoToolbox is offered on macOS and refused elsewhere",
              'h264_videotoolbox' in mirror.encoder_args('hardware', 'darwin')
              and 'libx264' in mirror.encoder_args('hardware', 'linux')
              and 'libx264' in mirror.encoder_args('hardware', 'win32'))
        check("the probe reads what this ffmpeg actually lists",
              mirror.has_hardware_encoder(_vt22, 'darwin') is True
              and mirror.has_hardware_encoder(_no22, 'darwin') is False,
              _vt22)
        _before22 = open(_count22).read()
        mirror.has_hardware_encoder(_vt22, 'darwin')
        check("and pays for that answer once, because the menu asks every redraw",
              open(_count22).read() == _before22,
              repr(open(_count22).read()))
        check("a machine without the tap never gets asked to encode on it",
              mirror.has_hardware_encoder(_vt22, 'linux') is False
              and open(_count22).read() == _before22)

        # -- the pointer, and the display to grab ---------------------------
        _win22 = os.path.join(_tmp22, "binwin")
        check("the pointer is drawn until the user says otherwise",
              mirror.cursor_enabled() is True)
        utils.Setting.set(mirror.SettingProperty.Mirror_Cursor, False)
        check("and not afterwards", mirror.cursor_enabled() is False)
        utils.Setting.unset(mirror.SettingProperty.Mirror_Cursor)

        _cur_on22 = mirror.probe_capture(_win22, 'win32', True)
        _cur_off22 = mirror.probe_capture(_win22, 'win32', False)
        check("the cursor choice reaches the capture command",
              _cur_on22.inputs[0][_cur_on22.inputs[0].index('-draw_mouse')
                                  + 1] == '1'
              and _cur_off22.inputs[0][_cur_off22.inputs[0].index('-draw_mouse')
                                       + 1] == '0', str(_cur_on22.inputs))
        check("so it is part of the probe cache key, not an afterthought",
              _cur_on22 is not _cur_off22
              and mirror.probe_capture(_win22, 'win32', True) is _cur_on22)
        mirror.invalidate_capture_cache()
        check("invalidate_capture_cache() really empties it",
              mirror._capture_cache == {})

        _screen22 = mirror.probe_capture(fake_ffmpeg22, 'darwin', True)
        check("the probe keeps the display list the picker needs",
              [i for i, _n in _screen22.screens] == [0, 1, 2],
              str(_screen22.screens))
        utils.Setting.set(mirror.SettingProperty.Mirror_Screen, '2')
        mirror.invalidate_capture_cache()
        check("a saved display is the one captured",
              mirror.probe_capture(fake_ffmpeg22, 'darwin', True).inputs[0][-1]
              == '2:none')
        utils.Setting.set(mirror.SettingProperty.Mirror_Screen, '9')
        mirror.invalidate_capture_cache()
        _gone22 = mirror.probe_capture(fake_ffmpeg22, 'darwin', True)
        check("a display that has been unplugged falls back instead of failing",
              _gone22 is not None and _gone22.inputs[0][-1] == '0:none')
        utils.Setting.unset(mirror.SettingProperty.Mirror_Screen)
        mirror.invalidate_capture_cache()

        # -- the two shapes of session ---------------------------------------
        _sess_b22 = mirror._Session('browser', has_audio=True, title='Test')
        _sess_c22 = mirror._Session('cast', has_audio=True)
        check("a browser session is replayed, a TV session never is",
              _sess_b22.replay is True and _sess_c22.replay is False)
        check("only the browser session knows where its header ends",
              _sess_b22.init_marker == b'moof' and _sess_c22.init_marker is None)
        check("each output names its own suffix and content type",
              _sess_b22.suffix == 'm4s' and _sess_b22.content_type == 'video/mp4'
              and _sess_c22.suffix == 'ts'
              and _sess_c22.content_type == 'video/mp2t'
              and _sess_b22.stream_path().endswith('.m4s'),
              "{} / {}".format(_sess_b22.stream_path(), _sess_c22.content_type))
        check("the codec string says whether there is audio to decode",
              'mp4a.40.2' in _sess_b22.codecs
              and 'mp4a' not in mirror._Session('browser').codecs)
        check("two sessions never share a stream id",
              mirror._Session('browser').stream_id
              != mirror._Session('browser').stream_id)

        # -- the broadcaster's replay bookkeeping ----------------------------
        bc22 = mirror._Broadcaster(init_marker=b'moof')
        bc22.feed(b'ftypisom')
        check("the header is held back from every subscriber until it is whole",
              bc22.init_segment == b'' and bc22.tail() == [])
        bc22.feed(b'xxmoofAAAA')
        check("everything before the first moof is the header",
              bc22.init_segment == b'ftypisomxx' and bc22.tail() == [b'moofAAAA'],
              "{} / {}".format(bc22.init_segment, bc22.tail()))
        bc22.feed(b'moofBBBB')
        _late22 = bc22.subscribe(replay=True)
        _live22 = bc22.subscribe(replay=False)
        check("a late joiner is handed the tail, and the header separately",
              _late22.get_nowait() == b'moofAAAA'
              and _late22.get_nowait() == b'moofBBBB'
              and bc22.await_init(timeout=1) == b'ftypisomxx',
              "{} / {}".format(bc22.init_segment, _late22.qsize()))
        _joined22 = False
        try:
            _live22.get_nowait()
        except Exception:
            _joined22 = True
        check("a TV that connects at the same moment gets only what comes next",
              _joined22)
        bc22.feed(b'moofCCCC')
        _small22 = mirror._Broadcaster(maxsize=2, ring_bytes=0)
        _slow22 = _small22.subscribe()
        for _ in range(6):
            _small22.feed(b'x' * 100)
        check("a slow viewer loses whole chunks instead of stalling the encoder",
              _small22.drops >= 4 and _slow22.qsize() == 2,
              "drops={} qsize={}".format(_small22.drops, _slow22.qsize()))
        _ring22 = mirror._Broadcaster(ring_bytes=250)
        for _ in range(10):
            _ring22.feed(b'y' * 100)
        check("the replay tail is bounded, so a long session cannot grow",
              sum(len(c) for c in _ring22.tail()) <= 350,
              str([len(c) for c in _ring22.tail()]))
        _noinit22 = mirror._Broadcaster()
        _noinit22.feed(b'raw')
        check("a container with no header keeps its bytes in the tail",
              _noinit22.tail() == [b'raw'] and _noinit22.init_segment == b'')

        # -- framing: a shed unit has to be a whole fragment -----------------
        #
        # stdout is read 4 KiB at a time, so a read boundary is essentially
        # never a box boundary. Both ways this server sheds load -- ring
        # eviction and a slow viewer's overflow -- remove units, and until the
        # framer a unit could be half an `mdat`. What a viewer then holds is a
        # byte stream whose box headers are wrong from that byte on, forever:
        # the measured shape of "black page, byte counter still climbing" was
        # 152 KiB of tail behind the header before the first whole `moof`.
        def _box22(kind, body=b''):
            return struct.pack('>I', 8 + len(body)) + kind + body

        def _fragment22(n):
            return (_box22(b'moof', b'.' * 24)
                    + _box22(b'mdat', bytes([48 + n % 10]) * 400))

        _head22 = _box22(b'ftyp', b'isom') + _box22(b'moov', b'm' * 60)
        _frag22 = 440
        _mp4_22 = _head22 + b''.join(_fragment22(i) for i in range(9))
        for _cut in (4096, 7, 1):
            # Every read size the pipe can produce: the bug is a property of
            # where the reads land, not of one particular size.
            _fr22 = mirror._Fragments()
            _units22 = []
            for _i in range(0, len(_mp4_22), _cut):
                _units22 += _fr22.feed(_mp4_22[_i:_i + _cut])
            _units22 += _fr22.flush()
            check("framing is exact at every read boundary ({} B)".format(_cut),
                  b''.join(u for _, u in _units22) == _mp4_22
                  and not _fr22.broken,
                  "{} of {} bytes handed out".format(
                      sum(len(u) for _, u in _units22), len(_mp4_22)))
        _fr22 = mirror._Fragments()
        _units22 = _fr22.feed(_mp4_22)
        check("one unit handed out is one whole fragment, and the header is "
              "the first thing out",
              _units22[0] == (False, _head22)
              and all(u[4:8] == b'moof' for m22, u in _units22[1:])
              and len(_units22) == 9,
              "{} / {}".format(len(_units22), [u[4:8] for _, u in _units22][:3]))
        _bc22 = mirror._Broadcaster(init_marker=b'moof', ring_bytes=900)
        for _i in range(9):
            _bc22.feed(_fragment22(_i))
        _q22 = _bc22.subscribe(replay=True)
        _tail22 = []
        while not _q22.empty():
            _tail22.append(_q22.get_nowait())
        check("the replay tail begins at a fragment edge, not mid-box",
              bool(_bc22.tail()) and all(u[4:8] == b'moof' for u in _tail22)
              and sum(len(u) for u in _tail22) <= 2 * _frag22,
              str([u[:8] for u in _tail22]))
        _bc22 = mirror._Broadcaster(init_marker=b'moof', maxsize=2)
        _bc22.feed(_head22 + _fragment22(0))
        _q22 = _bc22.subscribe(replay=True)
        for _i in range(1, 9):
            _bc22.feed(_fragment22(_i))
        _kept22 = []
        while not _q22.empty():
            _kept22.append(_q22.get_nowait())
        check("a slow viewer loses whole fragments and stays parseable",
              _bc22.drops > 0 and bool(_kept22)
              and all(u[4:8] == b'moof' for u in _kept22),
              "drops={} kept={}".format(_bc22.drops, len(_kept22)))
        check("the header is not queued up among the droppable fragments",
              all(not u.startswith(b'ftyp') for u in _kept22)
              and _bc22.init_segment == _head22,
              str([u[:12] for u in _kept22][:2]))
        _bc22 = mirror._Broadcaster(init_marker=b'moof')
        _bc22.feed(b'this is plainly not an mp4 at all')
        check("bytes that are not boxes fall back to the marker search",
              _bc22._framer is None, str(_bc22._framer))
        _bc22.feed(b'ftypisomxxmoofZZZZ')
        check("and the fallback still serves the header plus the tail, junk and "
              "all -- the marker search never promised the prefix was a header",
              _bc22.init_segment == b'this is plainly not an mp4 at allftypisomxx'
              and _bc22.tail() == [b'moofZZZZ'],
              "{} / {}".format(_bc22.init_segment, _bc22.tail()))
        check("a framed queue is bounded in fragments, not in 4 KiB reads",
              mirror._Broadcaster(init_marker=b'moof')._maxsize
              == mirror._Broadcaster.FRAMED_QUEUE
              and mirror._Broadcaster()._maxsize == 256)

        # -- the browser endpoint over a real socket -------------------------
        _server22 = mirror.start_stream_server(_sess_b22)
        _port22 = _server22.server_address[1]
        _server22.broadcaster.feed(b'ftypisomxxmoofAAAA')
        _path22 = _sess_b22.stream_path()
        _html_path22 = '{}?token={}'.format(mirror.BROWSER_PATH,
                                            _sess_b22.page_token)

        def _get22(path, read=0):
            conn = http.client.HTTPConnection('127.0.0.1', _port22, timeout=5)
            conn.request('GET', path)
            resp = conn.getresponse()
            body = resp.read(read) if read else b''
            conn.close()
            return resp, body

        _resp22, _body22 = _get22(_path22, 14)
        check("the browser pulls the live stream by its session id",
              _resp22.status == 200 and _body22.startswith(b'ftypisomxxmoof'),
              "{} / {!r}".format(_resp22.status, _body22[:16]))
        check("the stream is typed as MP4 and marked uncacheable",
              _resp22.getheader('Content-Type') == 'video/mp4'
              and _resp22.getheader('Cache-Control') == 'no-store'
              and _resp22.getheader('X-Content-Type-Options') == 'nosniff',
              str(dict(_resp22.getheaders())))
        _guessed22, _junk22 = _get22('/stream/0011223344556677.m4s')
        check("an invented stream id is a 404, not somebody else's screen",
              _guessed22.status == 404, str(_guessed22.status))
        _noauth22, _junk22 = _get22(mirror.BROWSER_PATH)
        check("the player page refuses a caller with no token",
              _noauth22.status == 403, str(_noauth22.status))
        _wrong22, _junk22 = _get22(mirror.BROWSER_PATH + '?token=deadbeef')
        check("and one with the wrong token", _wrong22.status == 403,
              str(_wrong22.status))
        _page22, _html22 = _get22(_html_path22, 1 << 16)
        check("with the token it is the player page",
              _page22.status == 200
              and b'<video' in _html22 and _path22.encode() in _html22,
              str(_page22.status))
        check("the page is ours end to end: no template hole, no innerHTML",
              b'@STREAM@' not in _html22 and b'innerHTML' not in _html22
              and b'document.write' not in _html22, str(_html22[:80]))
        # `addSourceBuffer` only accepts a MIME type: the bare codec list used to
        # throw NotSupportedError, so *every* viewer fell through to the
        # progressive path -- fine in Chrome, an empty black page in Safari, and
        # the failure was invisible because the fallback says nothing about it.
        check("the page hands MediaSource a MIME type, not a bare codec list",
              b'addSourceBuffer(CODECS)' in _html22
              and b'isTypeSupported(CODECS)' in _html22
              and _sess_b22.codecs.startswith('video/mp4; codecs="')
              and _sess_b22.codecs.endswith('"'),
              _sess_b22.codecs)
        check("and a fallback that reaches the reader says why",
              _html22.count(b'progressive(') >= 4
              and b"progressive('" in _html22, str(_html22.count(b'progressive(')))
        _head22 = http.client.HTTPConnection('127.0.0.1', _port22, timeout=5)
        _head22.request('HEAD', _path22)
        _hresp22 = _head22.getresponse()
        _head22.close()
        check("a HEAD probe of the stream costs nothing to the pump",
              _hresp22.status == 200 and _hresp22.read() == b'',
              str(_hresp22.status))
        _miss22, _junk22 = _get22('/nope')
        check("nothing else on this port is a page", _miss22.status == 404,
              str(_miss22.status))
        _server22.shutdown()
        _server22.server_close()
        _server22 = None

        # -- a mirror whose output is a browser, end to end -------------------
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'browser')
        _keep22 = []
        _saved_keep22 = mirror._keep_awake
        _saved_drop22 = mirror._stop_awake
        mirror._keep_awake = lambda: 'awake'
        mirror._stop_awake = lambda handle: _keep22.append(handle)
        try:
            _notify22.clear()
            mir22.start_mirror()
            check("a browser mirror needs no Chromecast at all",
                  _wait_until(mir22.is_mirroring, timeout=25)
                  and mir22._sender is None, str(_notify22))
            check("the announced address is the one the page is served on",
                  any('浏览器打开' in str(n) for n in _notify22)
                  and 'http' in str(_notify22)
                  and mirror.BROWSER_PATH in str(_notify22), str(_notify22))
            _viewer22 = mir22.viewer_url()
            check("viewer_url() is the tokened page while the mirror runs",
                  _viewer22.startswith('http://') and 'token=' in _viewer22,
                  _viewer22)
            _vport22 = int(_viewer22.split('//')[1].split('/')[0].rsplit(':', 1)[1])
            _vconn22 = http.client.HTTPConnection(
                '127.0.0.1', _vport22, timeout=5)
            _vconn22.request('GET', '/' + _viewer22.split('/', 3)[3])
            _vresp22 = _vconn22.getresponse()
            _vconn22.close()
            check("and it really answers from another connection",
                  _vresp22.status == 200, str(_vresp22.status))
            # The address handed out has to be one a phone can dial: this one
            # goes to the viewer (and, for the TV targets, into the stream URL).
            # `get_advertisable_ip()` is permissive on purpose, so its first
            # element is set order rather than reachability -- on a machine with
            # VM bridges it returned 192.168.215.0 / 192.168.97.0 / 192.168.139.3
            # while the LAN address was 192.168.1.5, i.e. every URL this plugin
            # printed was undialable. The core narrows it down in
            # discovery.advertisable_addresses(); the plugin has to agree.
            _reach22 = mirror._reachable_hosts()
            check("the address the plugin hands out is one the LAN can reach",
                  (not _reach22) or (mirror.advertise_host() in _reach22),
                  "advertise_host=%r core=%r" % (mirror.advertise_host(),
                                                 _reach22))
            # ... and the preference itself, independent of this machine's
            # interfaces: a VM bridge listed first must not win.
            _saved_hosts22 = mirror._reachable_hosts
            _saved_gai22 = utils.Setting.__dict__['get_advertisable_ip']
            try:
                utils.Setting.get_advertisable_ip = staticmethod(
                    lambda: ['192.168.97.0', '192.168.1.5'])
                mirror._reachable_hosts = lambda: ['192.168.1.5']
                check("a VM bridge listed first does not win the address",
                      mirror.advertise_host() == '192.168.1.5',
                      mirror.advertise_host())
                mirror._reachable_hosts = lambda: []
                check("with no core answer it keeps the old first-of-list",
                      mirror.advertise_host() == '192.168.97.0',
                      mirror.advertise_host())
            finally:
                setattr(utils.Setting, 'get_advertisable_ip', _saved_gai22)
                mirror._reachable_hosts = _saved_hosts22
            _stats22 = mir22.stats()
            check("the pump's numbers are readable without touching it",
                  _stats22.get('kind') == 'browser' and _stats22.get('chunks', 0)
                  > 0 and 'mbps' in _stats22 and 'drops' in _stats22,
                  str(_stats22))
            check("the sleep assertion is taken for as long as we mirror",
                  mir22._awake == 'awake', str(mir22._awake))

            # A push cannot be played here: bridge behaviour needs a device.
            _url_before22 = mir22.playing_url()
            _notify22.clear()
            mir22.set_media_url('http://elsewhere/movie.mp4')
            _refused22 = ' '.join(str(n) for n in _notify22)
            check("a pushed url is refused loudly instead of killing the mirror",
                  '无法播放' in _refused22
                  and mir22.is_mirroring()
                  and mir22.playing_url() == _url_before22, str(_notify22))
            check("and the refusal says a browser target has no device to pick",
                  '浏览器目标' in _refused22
                  and '还没有选择投屏目标' not in _refused22,
                  'telling someone to choose a device that does not exist is '
                  'exactly how this window reads as broken')
            _notify22.clear()
            mir22.set_media_url(_url_before22)
            check("the mirror's own stream address is not a push to bridge",
                  _notify22 == [] and mir22.is_mirroring(), str(_notify22))
        finally:
            mir22.stop_mirror()
            # Teardown is a thread, and it releases the sleep assertion only
            # after the broadcaster has shut down — wait for the whole thing,
            # not merely for the handles to be cleared.
            _wait_until(lambda: mir22._proc is None and mir22._server is None
                        and _keep22, timeout=20)
            mirror._keep_awake = _saved_keep22
            mirror._stop_awake = _saved_drop22
        check("stopping drops the sleep assertion and the viewer url",
              _keep22 == ['awake'] and mir22.viewer_url() == ''
              and mir22.stats() == {}, str(_keep22))
        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)

        # -- caffeinate for real ---------------------------------------------
        _caf_dir22 = os.path.join(_tmp22, 'bincaf')
        _write_fake(_caf_dir22, 'caffeinate', '#!/bin/sh\nsleep 30\n')
        _path_saved22 = os.environ['PATH']
        os.environ['PATH'] = _caf_dir22 + os.pathsep + _path_saved22
        try:
            _handle22 = mirror._keep_awake('darwin')
            check("caffeinate is what holds the Mac awake",
                  _handle22 is not None and _handle22.poll() is None)
            mirror._stop_awake(_handle22)
            check("and teardown lets go of it",
                  _wait_until(lambda: _handle22.poll() is not None, timeout=8),
                  str(_handle22.poll()))
        finally:
            os.environ['PATH'] = _path_saved22
        check("a platform without caffeinate asserts nothing",
              mirror._keep_awake('linux') is None
              and mirror._keep_awake('win32') is None)
        mirror._stop_awake(None)      # the no-handle case must be inert

        # -- ffmpeg's stderr cannot be allowed to fill up --------------------
        _noise22 = _write_fake(os.path.join(_tmp22, 'binerr'), 'ffmpeg-noise',
                               '#!/bin/sh\necho "avfoundation: denied" 1>&2\n'
                               'echo "incompatible pixel format" 1>&2\n')
        _proc22 = subprocess.Popen([_noise22], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE)
        _tail22 = []
        mirror._drain_stderr(_proc22, _tail22)
        check("ffmpeg is read until it closes, and the last lines are kept",
              _tail22 == ['avfoundation: denied', 'incompatible pixel format'],
              str(_tail22))
        _proc22.wait(timeout=5)

        # -- the menu ---------------------------------------------------------
        class _FakeMirror22(object):
            def __init__(self):
                self.stops = 0
                self.starts = 0

            def is_mirroring(self):
                return True

            def is_starting(self):
                return False

            def audio_dropped(self):
                return False

            def stop_mirror(self):
                self.stops += 1

            def start_mirror(self):
                self.starts += 1

            def viewer_url(self):
                return 'http://192.0.2.9:5555/browser?token=abc123'

            def stats(self):
                return {'kind': 'browser', 'clients': 2, 'chunks': 40,
                        'bytes': 500000, 'drops': 3, 'mbps': 4.2, 'seconds': 65}

        fake22 = _FakeMirror22()
        mirror._devices = [('Living Room TV', '192.0.2.7', 8009)]
        setting22 = mirror.ScreenMirrorSetting()
        _saved_renderer_fn22 = setting22._renderer
        setting22._renderer = lambda: fake22
        _kick22, _kick_dlna22 = mirror.start_search, mirror.start_renderer_search
        mirror.start_search = mirror.start_renderer_search = lambda: None
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'browser')
        mirror.probe_capture(fake_ffmpeg22, 'darwin', True)
        _st22 = setting22.console_state()
        _labels22 = _console_texts(_st22)
        check("both output targets are offered, and the running one is the state",
              _st22['output']['kind'] == 'browser'
              and [o['key'] for o in _st22['output']['options']].count('browser') == 1,
              str(_st22['output']))
        check("a browser mirror stops offering Chromecast devices",
              _mc.current_channel(_st22)['needs_device'] is False
              and _mc.device_rows(_mc.current_channel(_st22)) == []
              and 'devices' not in _mc.sections_for(_st22),
              str(_st22['channels']))
        check("and offers the viewing address to copy instead",
              _st22['viewer']['available']
              and _st22['viewer']['url'] == 'http://192.0.2.9:5555/browser?token=abc123'
              and 'viewer' in _mc.sections_for(_st22), str(_st22['viewer']))
        check("the address carries the warning a menu label never had room for",
              '令牌' in _st22['viewer']['hint'], _st22['viewer']['hint'])
        check("all four quality presets are offered",
              len([o for o in _st22['quality']['options'] if 'Mbps' in o['label']])
              == 4 and _st22['quality']['current'] in mirror.QUALITY_ORDER,
              str(_st22['quality']['options']))
        check("the display picker is filled from the cached probe",
              _st22['capture']['probed']
              and any('Capture screen 1' in s['label']
                      for s in _st22['capture']['screens']), str(_labels22))
        check("a running mirror reports what it is costing",
              '已镜像 1:05' in _st22['status_line']
              and '4.2 Mbps' in _st22['status_line']
              and '丢块 3' in _st22['status_line'], _st22['status_line'])
        # The menu bar used to carry a 电脑投屏 door and a stop row. It does not
        # any more -- the whole mirror surface is the settings page, and Part 36
        # is what proves nothing about mirroring is spliced into the menu. What
        # stays true here is that the page is the only readout: a second copy of
        # `status_line` in a menu is how the two came to disagree.
        check("the console keeps the only copy of the mirror readout",
              'status_line' not in dir(macast_mod.Macast)
              and not hasattr(macast_mod.Macast, '_mirror_menu_rows')
              and not hasattr(macast_mod.Macast, 'on_open_mirror_page_clicked'),
              'the app must not grow a second place that renders it')
        check("...and the page still renders it, because that is where it lives",
              _st22['status_line'] and '已镜像 1:05' in _st22['status_line'],
              _st22['status_line'])

        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
        _st22 = setting22.console_state()
        _labels22 = _console_texts(_st22)
        check("the Chromecast target puts the devices back in the window",
              _st22['output']['kind'] == 'cast'
              and 'Living Room TV · 192.0.2.7' in _labels22, str(_labels22))
        check("a Chromecast mirror gets no viewer panel, address or not",
              'viewer' not in _mc.sections_for(_st22)
              and _st22['viewer']['kind'] == 'cast', str(_st22['viewer']))
        check("and the device list is in the layout while the target is a cast",
              'devices' in _mc.sections_for(_st22), str(_labels22))

        setting22.console_action('set-output', {'value': 'browser'})
        check("switching the output restarts the running pipeline",
              mirror.output_kind() == 'browser' and fake22.stops == 1
              and fake22.starts == 1, str(fake22.stops))
        fake22.stops = fake22.starts = 0
        setting22.console_action('set-output', {'value': 'browser'})
        check("choosing the output that is already chosen changes nothing",
              fake22.stops == 0 and fake22.starts == 0)
        check("and an output that does not exist is refused, not stored",
              setting22.console_action('set-output', {'value': 'telnet'})['code'] == 1
              and mirror.output_kind() == 'browser', mirror.output_kind())

        mirror._capture_cache[('ffmpeg', 'darwin', True)] = object()
        setting22.console_action('set-cursor', {'value': False})
        check("the pointer toggle stores the wish and clears the probe cache",
              mirror.cursor_enabled() is False and mirror._capture_cache == {})
        mirror._capture_cache[('ffmpeg', 'darwin', False)] = object()
        setting22.console_action('set-cursor', {'value': True})
        check("turning the pointer back on puts it there, and empties the cache",
              mirror.cursor_enabled() is True and mirror._capture_cache == {})
        utils.Setting.unset(mirror.SettingProperty.Mirror_Cursor)

        _enc22 = setting22.console_action('set-encoder', {'value': 'hardware'})
        check("the encoder switch is only honest about what it can do",
              mirror.encoder_kind() == ('hardware' if sys.platform == 'darwin'
                                        else 'software')
              and _enc22['code'] == (0 if sys.platform == 'darwin' else 1),
              "{} / {}".format(utils.Setting.get(
                  mirror.SettingProperty.Mirror_Encoder, ''), _enc22['message']))
        check("and an encoder no build of ffmpeg here has is refused",
              setting22.console_action('set-encoder', {'value': 'nvenc'})['code'] == 1)
        utils.Setting.unset(mirror.SettingProperty.Mirror_Encoder)

        setting22.console_action('set-screen', {'value': '2'})
        check("choosing a display stores the index the probe uses",
              str(utils.Setting.get(mirror.SettingProperty.Mirror_Screen, '')) == '2')
        setting22.console_action('set-screen', {'value': ''})
        check("choosing the default takes the stored value away again",
              not utils.Setting.has(mirror.SettingProperty.Mirror_Screen))
        check("a screen that is not a number never reaches the command line",
              setting22.console_action('set-screen', {'value': '../x'})['code'] == 1
              and not utils.Setting.has(mirror.SettingProperty.Mirror_Screen), '')

        try:
            utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'browser')
            check("the window's only clipboard is the address in the state",
                  setting22.console_state()['viewer']['url']
                  == 'http://192.0.2.9:5555/browser?token=abc123', '')
            check("nothing the console may ask for is invented by the console",
                  all(a in mirror.ScreenMirrorSetting.CONSOLE_ACTIONS
                      for a in ('set-output', 'set-target', 'set-dlna-target',
                                'set-profile', 'set-quality', 'set-screen',
                                'set-cursor', 'set-encoder', 'audio-setup')), '')
            check("an action the plugin does not know is a refusal, not a crash",
                  setting22.console_action('delete-everything')['code'] == 1, '')
        finally:
            utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
            setting22._renderer = _saved_renderer_fn22
            mirror.start_search, mirror.start_renderer_search = _kick22, _kick_dlna22
    finally:
        if _server22 is not None:
            try:
                _server22.shutdown()
                _server22.server_close()
            except Exception:
                pass
        if _mir22 is not None:
            _mir22.stop_mirror()
        cherrypy.engine.unsubscribe('app_notify', _notify22_rec)
        utils.SETTING_DIR = _saved_dir22
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting22
        _shutil.rmtree(_tmp22, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the browser target behaves", False,
          "{}: {}".format(type(e).__name__, e))

print("\n=== Part 23: screen mirror v0.5 (DLNA TV target) ===")
try:
    import re as _re23
    import xml.etree.ElementTree as _ET23
    import http.server as _httpd23
    _saved_setting23 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir23 = utils.SETTING_DIR
    _tmp23 = _tempfile.mkdtemp(prefix="macast-mirror23-")
    _notify23 = []
    _notify23_rec = lambda *a, **k: _notify23.append(a)      # noqa: E731
    _server23 = None
    _tv23 = None
    _mir23 = None
    _taps23 = {}
    try:
        utils.SETTING_DIR = _tmp23
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp23, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify23_rec)

        mirror = _load_plugin("screen_mirror_plugin_v05", "screen_mirror.py")
        fake_ffmpeg23 = _write_fake(os.path.join(_tmp23, "bin"), "ffmpeg", r"""#!/bin/sh
case "$*" in
  *list_devices*)
    printf '%s\n' \
      'AVFoundation input device list has 3 items:' \
      '[AVFoundation indev @ 0x1] AVFoundation video devices:' \
      '[AVFoundation indev @ 0x1] [0] FaceTime高清相机' \
      '[AVFoundation indev @ 0x1] [1] Capture screen 0' \
      '[AVFoundation indev @ 0x1] AVFoundation audio devices:' \
      '[AVFoundation indev @ 0x1] [0] MacBook Pro麦克风' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.05
done
""")

        # -- the arithmetic of a file that has no end ------------------------
        check("the DLNA target is a third output, with its own container",
              'dlna' in mirror.OUTPUTS
              and mirror.OUTPUTS['dlna'][1] == 'mpg'
              and mirror.OUTPUTS['dlna'][2] == 'video/mpeg'
              and mirror.OUTPUTS['dlna'][3] is None, str(mirror.OUTPUTS))
        _sizes23 = {}
        for _pid23, _p23 in mirror.DLNA_PROFILES.items():
            _size23, _dur23 = mirror.advertised_file(_p23.bitrate)
            _sizes23[_pid23] = _size23
        check("every profile advertises a file a signed 32-bit client survives",
              all(0 < s <= mirror.DLNA_MAX_ADVERTISED_SIZE < 2 ** 31
                  for s in _sizes23.values()), str(_sizes23))
        _size23, _dur23 = mirror.advertised_file(4500000)
        _secs23 = sum(int(x) * y for x, y in zip(_dur23.split(':'),
                                                 (3600, 60, 1)))
        check("and the advertised duration agrees with the bitrate",
              abs(_secs23 - _size23 * 8 // 4500000) <= 1, _dur23)

        _pal23 = mirror.DLNA_PROFILES['ps-pal']
        _h26423 = mirror.DLNA_PROFILES['ts-h264']
        _pi23 = mirror.protocol_info(_pal23)
        check("protocolInfo names the container the TV is being lied to about",
              _pi23.startswith('http-get:*:video/mpeg:')
              and 'DLNA.ORG_PN=MPEG_PS_PAL' in _pi23
              and 'DLNA.ORG_OP=01' in _pi23
              and 'DLNA.ORG_FLAGS=' in _pi23, _pi23)
        check("an H.264 shape claims no profile name it cannot honour",
              'DLNA.ORG_PN' not in mirror.protocol_info(_h26423)
              and 'video/vnd.dlna.mpeg-tts' in mirror.protocol_info(_h26423),
              mirror.protocol_info(_h26423))
        check("contentFeatures travels on its own header too",
              # And it names the same profile the DIDL promised. It used to
              # carry only the flags, which is what a renderer that validates
              # the *response* against the profile name it was promised has
              # nothing to compare against: a TCL television downloaded 26 MB
              # through eight connections and never reached PLAYING (Part 42).
              'DLNA.ORG_PN={}'.format(_pal23.org_pn)
              in mirror.content_features(_pal23)
              and 'DLNA.ORG_OP=01' in mirror.content_features(_pal23),
              mirror.content_features(_pal23))

        _didl23 = mirror.build_didl('http://10.0.0.2:9/stream/aa.mpg',
                                    'TV & <b>host</b>', _pal23, _size23, _dur23)
        _root23 = _ET23.fromstring(_didl23.encode('utf-8'))
        _ns23 = 'urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/'
        _res23 = _root23.find('.//{{{}}}item/{{{}}}res'.format(_ns23, _ns23))
        check("the DIDL is well formed and carries size, duration and URL",
              _res23 is not None
              and _res23.text == 'http://10.0.0.2:9/stream/aa.mpg'
              and _res23.get('size') == str(_size23)
              and _res23.get('duration') == _dur23, _didl23[:160])
        check("a hostname with an ampersand cannot break the envelope",
              'TV & <b>' not in _didl23 and 'TV &amp; &lt;b&gt;' in _didl23,
              _didl23[:200])

        # -- what a renderer asks for, and what it must get back -------------
        check("a byte range is parsed the way a TV writes it",
              mirror.parse_range('bytes=0-393215') == (0, 393215)
              and mirror.parse_range('bytes=14680064-') == (14680064, None)
              and mirror.parse_range('BYTES=10-20') == (10, 20), '')
        check("a request we cannot answer honestly is refused, not guessed",
              mirror.parse_range('bytes=0-99,200-299') is None
              and mirror.parse_range('bytes=-1024') is None
              and mirror.parse_range('bytes=abc-') is None
              and mirror.parse_range('items=0-9') is None
              and mirror.parse_range(None) == (0, None), '')

        # -- the encoder shape an old TV swallows ----------------------------
        _cap23 = mirror._Capture('test', [['-f', 'test', '-i', 'x']])
        _ps23 = mirror.build_ffmpeg_command('ffmpeg', _cap23, 1080, 10000000,
                                           kind='dlna', profile=_pal23)
        check("the PS shape is MPEG-2 in a VOB, at the DVD frame size",
              _ps23[_ps23.index('pipe:1') - 1] == 'vob'
              and 'mpeg2video' in _ps23
              and _ps23[_ps23.index('-vf') + 1] == 'scale=720:576,setdar=16/9'
              and _ps23[-1] == 'pipe:1', str(_ps23))
        check("constant bitrate, because the TV models a fullness buffer",
              [_ps23[_ps23.index(k) + 1] for k in ('-b:v', '-minrate',
                                                   '-maxrate')]
              == ['4500000'] * 3
              and _ps23[_ps23.index('-g') + 1] == '15'
              and _ps23[_ps23.index('-r') + 1] == '25', str(_ps23))
        check("the profile, not the quality menu, decides the picture",
              '1080' not in ''.join(_ps23) and '10000000' not in ''.join(_ps23),
              str(_ps23))
        _psa23 = mirror.build_ffmpeg_command(
            'ffmpeg', mirror._Capture('test', [['-f', 'test', '-i', 'x']],
                                      audio_map='1:a:0'),
            576, 4500000, kind='dlna', profile=_pal23)
        check("and the audio is the one a DVD-era TV decodes",
              _psa23[_psa23.index('-c:a') + 1] == 'ac3'
              and _psa23[_psa23.index('-b:a') + 1] == '192k'
              and _psa23[_psa23.index('0:v:0') + 1:_psa23.index('0:v:0') + 3]
              == ['-map', '1:a:0'], str(_psa23))
        _ntsc23 = mirror.build_ffmpeg_command(
            'ffmpeg', _cap23, 720, 5000000, kind='dlna',
            profile=mirror.DLNA_PROFILES['ps-ntsc'])
        check("NTSC land gets 30 fps and 480 lines",
              _ntsc23[_ntsc23.index('-r') + 1] == '30'
              and 'scale=720:480,setdar=16/9' in _ntsc23, str(_ntsc23))
        _tsh23 = mirror.build_ffmpeg_command('ffmpeg', _cap23, 720, 5000000,
                                             kind='dlna', profile=_h26423,
                                             encoder='hardware')
        check("the TS/H.264 shape keeps the hardware encoder and a 1 s GOP",
              'h264_videotoolbox' in _tsh23
              and 'mpegts' in _tsh23
              and _tsh23[_tsh23.index('-muxdelay') - 1] == 'mpegts'
              and _tsh23[_tsh23.index('-g') + 1] == '25'
              and 'mpeg2video' not in _tsh23, str(_tsh23))
        check("the H.264 shapes are rate-capped, MPEG-2 keeps its CBR triplet",
              '-maxrate' in _tsh23 and '-bufsize' in _tsh23
              and _tsh23[_tsh23.index('-maxrate') + 1] == '9000000'
              and _tsh23[_tsh23.index('-bufsize') + 1] == '6000000'
              and '-minrate' not in _tsh23
              and _ps23[_ps23.index('-minrate') + 1]
              == _ps23[_ps23.index('-maxrate') + 1]
              == _ps23[_ps23.index('-b:v') + 1], str(_tsh23) + str(_ps23))
        check("only the DVD muxer keeps ffmpeg's interleaving delay, "
              "because a zero one underflows MPEG-PS's own VRV model",
              '-muxdelay' not in _ps23
              and '-muxdelay' in _tsh23
              and '-muxdelay' in mirror.build_ffmpeg_command(
                  'ffmpeg', _cap23, 720, 5000000, kind='dlna',
                  profile=mirror.DLNA_PROFILES['mkv-h264']),
              str(_ps23))
        check("the aspect-preserving shapes scale by height only",
              _tsh23[_tsh23.index('-vf') + 1] == 'scale=-2:720,setdar=16/9',
              str(_tsh23))
        check("the MKV shape is for the renderer that only speaks Matroska",
              'matroska' in mirror.build_ffmpeg_command(
                  'ffmpeg', _cap23, 720, 6000000, kind='dlna',
                  profile=mirror.DLNA_PROFILES['mkv-h264']))
        check("a machine with no audio tap sends video only",
              '-an' in _ps23 and '-c:a' not in _ps23, str(_ps23))

        check("an unknown stored profile falls back instead of failing",
              mirror.dlna_profile('no-such-shape') is _pal23
              and mirror.dlna_profile() is _pal23)
        check("the watchdog's suggestion order starts after the current one",
              mirror.profile_order()[:2] == ['ps-ntsc', 'ts-mpeg2']
              and len(mirror.profile_order()) == len(mirror.DLNA_PROFILES) - 1,
              str(mirror.profile_order()))
        utils.Setting.set(mirror.SettingProperty.Mirror_Dlna_Profile, 'ts-h264')
        check("so switching the profile moves the whole list forward",
              mirror.profile_order()[0] == 'mkv-h264'
              and mirror.profile_order()[-1] == 'ts-mpeg2',
              str(mirror.profile_order()))
        utils.Setting.unset(mirror.SettingProperty.Mirror_Dlna_Profile)

        # -- the byte log: a file you can seek inside ------------------------
        _log23 = mirror._ByteLog(limit=1 << 16)
        _stream23 = [bytes([i % 251]) * 4096 for i in range(1, 5)]
        for _c23 in _stream23:
            _log23.feed(_c23)
        _all23 = b''.join(_stream23)
        check("reads are addressed by absolute offset",
              _log23.read(0, 1024, deadline=1)[0] == _all23[:1024]
              and _log23.read(5000, 10, deadline=1)[0] == _all23[5000:5010], '')
        check("an offset behind the window clamps to the oldest byte "
              "instead of slicing backwards",
              _log23.read(-4096, 16, deadline=1)[0] == _all23[:16], '')
        for _n23 in range(20):
            _log23.feed(b'x' * 8192)
        check("the ring is bounded, and aging out is what `drops` counts",
              _log23.start > 0 and _log23.drops > 0
              and _log23.end - _log23.start <= (1 << 16) + 8192,
              '{} {} {}'.format(_log23.start, _log23.drops, _log23.end))
        _late23 = mirror._ByteLog(limit=4096)
        _full23 = bytes(range(256)) * 40
        for _c23 in (_full23[i:i + 2560] for i in range(0, len(_full23), 2560)):
            _late23.feed(_c23)
        check("a reader that lags the eviction resyncs to what we still hold",
              _late23.start > 0 and _late23.read(0, 32, deadline=1)[0]
              == _full23[_late23.start:_late23.start + 32], str(_late23.start))
        check("the anchor sits prefill bytes behind the live edge",
              _log23.anchor(8192) == _log23.end - 8192
              and _log23.anchor(1 << 30) == _log23.start, '')
        _soon23 = mirror._ByteLog()
        _got23 = []
        threading.Thread(target=lambda: _got23.append(
            _soon23.read(0, 4096, deadline=None)), daemon=True).start()
        _soon23.feed(b'z' * 4096)
        check("an unbounded read blocks until the encoder produces the bytes",
              _wait_until(lambda: _got23 == [(b'z' * 4096, True)], timeout=5),
              str(_got23))
        _stall23 = mirror._ByteLog()
        _data23, _complete23 = _stall23.read(0, 4096, deadline=0.3)
        check("a bounded read gives up in time instead of hanging the TV",
              _data23 == b'' and _complete23 is False, repr(_data23))
        check("padding is a valid MPEG-PS packet, sized to the byte",
              len(_stall23.pad(4096)) == 4096
              and _stall23.pad(4096)[:6] == mirror.PS_PADDING
              and _stall23.pad(0) == b''
              and len(_stall23.pad(7)) == 7, '')
        _wake23 = mirror._ByteLog()
        _woke23 = []
        threading.Thread(target=lambda: _woke23.append(
            _wake23.read(0, 4096, deadline=None)), daemon=True).start()
        _wait_until(lambda: _wake23._readers == 1)
        _wake23.close()
        check("teardown wakes a parked reader, so shutdown cannot hang",
              _wait_until(lambda: bool(_woke23), timeout=5)
              and _woke23[0] == (b'', False), str(_woke23))

        # -- serving that file over HTTP -------------------------------------
        _sess23 = mirror._Session('dlna', has_audio=True, title='测试机')
        _server23 = mirror.start_stream_server(_sess23)
        _port23 = _server23.server_address[1]
        _path23 = _sess23.stream_path()
        _log23b = _server23.broadcaster
        check("a DLNA session gets a byte log instead of the live queue",
              isinstance(_log23b, mirror._ByteLog)
              and _sess23.file_size == mirror.advertised_file(
                  _sess23.profile.bitrate)[0], str(type(_log23b)))

        def _http23(method, path, range_header=None, timeout=8):
            conn = http.client.HTTPConnection('127.0.0.1', _port23,
                                              timeout=timeout)
            conn.request(method, path, headers={} if range_header is None else
                         {'Range': range_header})
            resp = conn.getresponse()
            out = (resp.status, dict(resp.getheaders()), resp.read())
            conn.close()
            return out

        _head23 = _http23('HEAD', _path23)[1]
        check("a HEAD names the whole file and promises ranges",
              _head23['Content-Length'] == str(_sess23.file_size)
              and _head23['Accept-Ranges'] == 'bytes'
              and _head23['Content-Type'] == 'video/mpeg', str(_head23))
        check("the two DLNA headers the firmware sniffs are on every answer",
              _head23['transferMode.dlna.org'] == 'Streaming'
              and 'DLNA.ORG_OP=01' in _head23['contentFeatures.dlna.org'],
              str(_head23))
        check("and a plain 200 carries no Content-Range",
              'Content-Range' not in _head23, str(_head23))

        _sniff23 = 393216
        _saved_sniff23 = mirror.DLNA_SNIFF_TIMEOUT
        mirror.DLNA_SNIFF_TIMEOUT = 0.3
        _st23, _hd23, _body23 = _http23('GET', _path23,
                                        'bytes=0-{}'.format(_sniff23 - 1))
        check("an opening sniff gets EXACTLY the bytes it asked for",
              _st23 == 206 and len(_body23) == _sniff23
              and _hd23['Content-Length'] == str(_sniff23)
              and _hd23['Content-Range'] == 'bytes 0-{}/{}'.format(
                  _sniff23 - 1, _sess23.file_size),
              '{} {}'.format(_st23, len(_body23)))
        check("the gap the encoder has not filled is MPEG-PS padding",
              _body23.startswith(mirror.PS_PADDING * 2)
              and _body23[len(_body23) - 6:] == mirror.PS_PADDING[:6],
              repr(_body23[:12]))
        _again23 = _http23('GET', _path23, 'bytes=0-{}'.format(_sniff23 - 1))[2]
        check("the same range twice gives the same bytes: the sniff may retry",
              _again23 == _body23, str(len(_again23)))
        check("the file's offset 0 was pinned on the first answer",
              isinstance(_sess23.file_anchor, int), str(_sess23.file_anchor))
        mirror.DLNA_SNIFF_TIMEOUT = _saved_sniff23

        for _i23 in range(16):
            _log23b.feed(bytes([_i23]) * 8192)
        _held23 = b''.join([bytes([i]) * 8192 for i in range(16)])
        _st23, _hd23, _body23 = _http23('GET', _path23, 'bytes=0-4095')
        check("once the encoder has produced bytes, the sniff gets real data",
              _st23 == 206 and _body23 == _held23[:4096], repr(_body23[:16]))
        _st23, _hd23, _body23 = _http23('GET', _path23, 'bytes=8192-16383')
        check("and it is the data at THAT offset, not the live edge",
              _body23 == _held23[8192:16384]
              and _hd23['Content-Range'] == 'bytes 8192-16383/{}'.format(
                  _sess23.file_size), repr(_body23[:8]))
        check("past the promised end is a 416 that names the size",
              _http23('GET', _path23, 'bytes=2000000000-')[0] == 416, '')
        _st416, _hd416, _bd416 = _http23('GET', _path23,
                                         'bytes={}-{}'.format(
                                             _sess23.file_size,
                                             _sess23.file_size + 100))
        check("the 416 keeps the DLNA headers and an empty body",
              _hd416['Content-Range'] == 'bytes */{}'.format(
                  _sess23.file_size) and _bd416 == b''
              and _hd416['transferMode.dlna.org'] == 'Streaming', str(_hd416))
        check("a multi-range request is refused rather than half-served",
              _http23('GET', _path23, 'bytes=0-99,200-299')[0] == 404, '')
        check("so is a stranger's stream id",
              _http23('GET', '/stream/deadbeefdeadbeef.mpg')[0] == 404, '')

        # the endless read: a TV filling its buffer towards the "end"
        def _raw_get23(path, range_header=None, soak=1.0):
            conn = socket.create_connection(('127.0.0.1', _port23), timeout=5)
            request = 'GET {} HTTP/1.0\r\nHost: x\r\n'.format(path)
            if range_header:
                request += 'Range: {}\r\n'.format(range_header)
            conn.sendall((request + '\r\n').encode('ascii'))
            head = b''
            while b'\r\n\r\n' not in head:
                piece = conn.recv(4096)
                if not piece:
                    break
                head += piece
            first, _, rest = head.partition(b'\r\n\r\n')
            body = bytearray(rest)
            conn.settimeout(soak)
            try:
                while True:
                    piece = conn.recv(65536)
                    if not piece:
                        break
                    body += piece
            except OSError:
                pass
            conn.close()
            return first.decode('latin-1'), bytes(body)

        _endless23 = []

        def _read_endless23():
            _endless23.append(_raw_get23(_path23, 'bytes=32768-', soak=4.0))

        _tail_thread23 = threading.Thread(target=_read_endless23, daemon=True)
        _tail_thread23.start()
        _wait_until(lambda: _log23b.clients() >= 1, timeout=5)
        _more23 = bytes(range(256)) * 64            # 16 KiB
        _log23b.feed(_more23)
        check("an open-ended range keeps the connection while the show goes on",
              _endless23 == [], str(_endless23))
        _more23b = bytes(range(256)) * 64
        _log23b.feed(_more23b)
        _log23b.close()
        _tail_thread23.join(timeout=15)
        check("and closes when the session ends, from that offset on",
              _endless23 and _endless23[0][0].startswith('HTTP/1.0 206')
              and 'Content-Length: {}'.format(_sess23.file_size - 32768)
              in _endless23[0][0]
              and _endless23[0][1] == (_held23 + _more23 + _more23b)[32768:],
              str([(r.splitlines()[:1], len(b)) for r, b in _endless23]))
        _server23.shutdown()
        _server23.server_close()
        _server23 = None

        _sess23b = mirror._Session('cast', has_audio=True)
        _server23b = mirror.start_stream_server(_sess23b)
        _server23b.broadcaster.feed(b'chunk')
        _conn23b = http.client.HTTPConnection('127.0.0.1',
                                              _server23b.server_address[1],
                                              timeout=5)
        _conn23b.request('GET', _sess23b.stream_path())
        _resp23b = _conn23b.getresponse()
        _hd23b = dict(_resp23b.getheaders())
        _conn23b.close()
        _server23b.shutdown()
        _server23b.server_close()
        check("the Chromecast target keeps its live-edge semantics",
              _hd23b.get('Accept-Ranges') == 'none'
              and 'transferMode.dlna.org' not in _hd23b
              and 'Content-Range' not in _hd23b, str(_hd23b))

        # -- discovery: device descriptions ----------------------------------
        _desc23 = (
            '<?xml version="1.0"?>'
            '<root xmlns="urn:schemas-upnp-org:device-1-0">'
            '<specVersion><major>1</major><minor>0</minor></specVersion>'
            '<device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1'
            '</deviceType><friendlyName> 厨房的小电视 </friendlyName>'
            '<UDN>uuid:11111111-2222-3333-4444-555555555555</UDN>'
            '<serviceList><service>'
            '<serviceType>urn:schemas-upnp-org:service:AVTransport:1'
            '</serviceType>'
            '<serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>'
            '<controlURL>/dmr/AVTransport</controlURL>'
            '<eventSubURL>/dmr/evt</eventSubURL>'
            '<SCPDURL>/av.xml</SCPDURL></service></serviceList>'
            '</device></root>').encode('utf-8')
        check("a renderer with a relative controlURL is still addressable",
              mirror.parse_description(_desc23, 'http://192.0.2.40:5246/desc',
                                       '192.0.2.40')
              == ('厨房的小电视', 'http://192.0.2.40:5246/dmr/AVTransport'), '')
        check("a device that answers from a different address than it claims "
              "is believed by its answer",
              mirror.parse_description(_desc23, 'http://127.0.0.1:5246/desc',
                                       '192.0.2.40')
              == ('厨房的小电视', 'http://192.0.2.40:5246/dmr/AVTransport'), '')
        check("a description with no AVTransport service is not a target",
              mirror.parse_description(b'<root><device/></root>',
                                       'http://192.0.2.40/x', '192.0.2.40')
              is None
              and mirror.parse_description(b'not xml at all', 'http://x/y',
                                          '192.0.2.40') is None, '')

        def _fake_ssdp23(st, timeout=3.0, interface=None):
            _taps23['ssdp'] = _taps23.get('ssdp', 0) + 1
            if st.endswith('MediaRenderer:1'):
                return [('http://192.0.2.40:5246/desc', '192.0.2.40'),
                        ('http://192.0.2.41:5246/desc', '192.0.2.41')]
            return [('http://192.0.2.40:5246/desc', '192.0.2.40')]

        _saved_ssdp23 = mirror._ssdp_search
        _saved_adv23 = mirror.Setting.get_advertisable_ip
        _taps23['desc'] = mirror.describe_renderer
        mirror._ssdp_search = _fake_ssdp23
        mirror.Setting.get_advertisable_ip = lambda: ['192.0.2.41']
        mirror.describe_renderer = lambda url, peer, timeout=3.0: (
            mirror.parse_description(_desc23, url, peer)
            if peer == '192.0.2.40' else None)
        _found23 = mirror.discover_renderers()
        check("discovery dedupes, skips ourselves, and names the host",
              _found23 == [('厨房的小电视',
                            'http://192.0.2.40:5246/dmr/AVTransport',
                            '192.0.2.40')], str(_found23))
        check("nothing is chosen until the user picks it",
              mirror.dlna_target() == (None, ''), str(mirror.dlna_target()))
        utils.Setting.set(mirror.SettingProperty.Mirror_Dlna_Control,
                          'http://192.0.2.40:5246/dmr/AVTransport')
        utils.Setting.set(mirror.SettingProperty.Mirror_Target_Name, '小电视')
        check("the control URL lives in its own key, not in host:port",
              mirror.dlna_target()
              == ('小电视', 'http://192.0.2.40:5246/dmr/AVTransport')
              and mirror.output_kind() == 'cast', str(mirror.dlna_target()))
        check("the route probe answers with a local address without sending",
              mirror.source_address_for('127.0.0.1') == '127.0.0.1', '')
        mirror._ssdp_search = _saved_ssdp23
        mirror.Setting.get_advertisable_ip = _saved_adv23
        mirror.describe_renderer = _taps23['desc']

        # -- SOAP: what actually goes over the wire --------------------------
        class _TV23Handler(_httpd23.BaseHTTPRequestHandler):
            def log_message(self, fmt, *args):
                pass

            def _reply(self, action, fields):
                inner = ''.join('<{k}>{v}</{k}>'.format(k=k, v=v)
                                for k, v in [('InstanceID', '3')] + list(fields))
                body = (
                    '<?xml version="1.0"?>'
                    '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"'
                    ' s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
                    '<s:Body><u:{a}Response '
                    'xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">'
                    '{i}</u:{a}Response></s:Body></s:Envelope>'
                ).format(a=action, i=inner)
                raw = body.encode('utf-8')
                self.send_response(200)
                self.send_header('Content-Type', 'text/xml; charset="utf-8"')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_POST(self):
                n = int(self.headers.get('Content-Length') or 0)
                raw = self.rfile.read(n).decode('utf-8')
                action = (self.headers.get('SOAPAction') or '').strip('"')
                action = action.rsplit('#', 1)[-1]
                _taps23.setdefault('calls', []).append((action, raw))
                state = _taps23.setdefault('state', 'NO_MEDIA_PRESENT')
                if action == 'SetAVTransportURI':
                    _taps23['uri'] = raw
                    _taps23['state'] = 'STOPPED'
                    self._reply(action, [])
                elif action == 'Play':
                    _taps23['state'] = 'PLAYING'
                    self._reply(action, [])
                elif action == 'Stop':
                    _taps23['state'] = 'STOPPED'
                    self._reply(action, [])
                elif action == 'GetTransportInfo':
                    if _taps23.get('drop'):
                        self.send_error(500)
                        return
                    self._reply(action, [('CurrentTransportState', state),
                                         ('CurrentTransportStatus', 'OK'),
                                         ('CurrentSpeed', '1')])
                elif action == 'GetPositionInfo':
                    _taps23['rel'] = _taps23.get('rel', 0) + 1
                    self._reply(action, [('RelTime',
                                          '0:00:{:02d}'.format(_taps23['rel']
                                                               % 60))])
                else:
                    self.send_error(501)

        _tv23 = _httpd23.ThreadingHTTPServer(('127.0.0.1', 0), _TV23Handler)
        threading.Thread(target=_tv23.serve_forever, daemon=True,
                         name="FAKE_DLNA_TV").start()
        _control23 = 'http://127.0.0.1:{}/control'.format(
            _tv23.server_address[1])

        sender23 = mirror._DlnaSender(_control23, timeout=5.0)
        check("InstanceID starts at 0", sender23.instance_id == '0', '')
        sender23.set_uri('http://192.0.2.9:9/stream/aa.mpg', '屏幕镜像', _pal23)
        sender23.play()
        _calls23 = [c for c, _b in _taps23['calls']]
        check("the push is SetAVTransportURI then Play, in that order",
              _calls23 == ['SetAVTransportURI', 'Play'], str(_calls23))
        check("a renderer that moved off InstanceID 0 is answered in kind",
              sender23.instance_id == '3', sender23.instance_id)
        _uri_body23 = _taps23['uri']
        check("the envelope names the service and carries the DIDL we built",
              'urn:schemas-upnp-org:service:AVTransport:1' in _uri_body23
              and 'SetAVTransportURI' in _uri_body23
              and 'MPEG_PS_PAL' in _uri_body23
              and 'http-get:*:video/mpeg' in _uri_body23, _uri_body23[:200])
        check("the transport state we read back is the one the TV reported",
              sender23.transport_state() == 'PLAYING'
              and sender23.position().startswith('0:00:'), '')
        _taps23['drop'] = True
        try:
            sender23.transport_state()
            _raised23 = False
        except Exception:
            _raised23 = True
        _taps23['drop'] = False
        check("a TV that answers 500 raises instead of reporting PLAYING",
              _raised23 and sender23.last_state == 'PLAYING',
              sender23.last_state)
        sender23.stop()
        check("teardown tells the renderer to stop, not just hangs up",
              _taps23['calls'][-1][0] == 'Stop'
              and _taps23['state'] == 'STOPPED',
              str([c for c, _b in _taps23['calls']]))
        sender23.close()

        # Our own receiver is the strictest parser we can reach without a TV:
        # it has to accept the envelope byte for byte.
        _pushed23 = []

        class _Dlna23(protocol.DLNAProtocol):
            @property
            def renderer(self):
                return self

            def set_media_url(self, uri, start='0'):
                _pushed23.append(uri)

            def set_media_title(self, title):
                _pushed23.append(('title', title))

            def set_media_resume(self):
                _pushed23.append('resume')

            def set_media_stop(self):
                _pushed23.append('stop')

            def release_playback(self):
                pass

        _dp23 = _Dlna23()
        _accepted23 = []
        for _action23, _body23 in _taps23['calls']:
            try:
                _dp23.call(_body23)
                _accepted23.append(_action23)
            except Exception as e:
                _accepted23.append('{}:{}'.format(_action23, e))
        check("Macast's own SOAP parser accepts every envelope we send",
              _accepted23 == ['SetAVTransportURI', 'Play', 'Stop']
              or _accepted23[:2] == ['SetAVTransportURI', 'Play'],
              str(_accepted23))
        check("and it latches the URI and the DIDL title we advertised",
              'http://192.0.2.9:9/stream/aa.mpg' in _pushed23
              and _dp23.get_state('CurrentTrackURI')
              == 'http://192.0.2.9:9/stream/aa.mpg'
              and ('title', '屏幕镜像') in _pushed23, str(_pushed23))

        # -- the watchdog -----------------------------------------------------
        _saved_poll23 = mirror.DLNA_POLL_SECONDS
        _saved_rep23 = mirror.DLNA_MAX_REPUSHES
        mirror.DLNA_POLL_SECONDS = 0.05
        mirror.DLNA_MAX_REPUSHES = 20
        _rec23 = _StateRec()

        class _Mirror23(mirror.ScreenMirrorRenderer):
            @property
            def protocol(self):
                return _rec23

        mir23 = _Mirror23()
        _mir23 = mir23
        _failures23 = []
        mir23._fail = lambda message, generation: _failures23.append(message)
        _sess_w23 = mirror._Session('dlna', has_audio=True, title='T')
        _url_w23 = 'http://192.0.2.9:9/stream/wf.mpg'
        _pushes23 = lambda: sum(1 for c, _b in _taps23['calls']      # noqa: E731
                                if c == 'SetAVTransportURI')
        _watch23 = mirror._DlnaSender(_control23)
        _taps23['state'] = 'PAUSED_PLAYBACK'
        _before23 = _pushes23()
        threading.Thread(target=mir23._watch_dlna,
                         args=(_watch23, _url_w23, _sess_w23,
                               mir23._generation), daemon=True).start()
        check("a renderer that fell out of PLAYING gets the URL pushed again",
              _wait_until(lambda: _pushes23() > _before23, timeout=8)
              and mir23._dlna_state == 'PAUSED_PLAYBACK', str(mir23._dlna_state))
        _taps23['state'] = 'PLAYING'
        _taps23['rel'] = 0
        check("recovery is recognised, and no advice is shouted on the way",
              _wait_until(lambda: mir23._dlna_state == 'PLAYING', timeout=8)
              and _failures23 == [], str(mir23._dlna_state))
        _taps23['drop'] = True
        check("giving up is a message that names the next profile to try",
              _wait_until(lambda: bool(_failures23), timeout=15)
              and '兼容档位' in _failures23[0]
              and 'NTSC' in _failures23[0], str(_failures23))
        _taps23['drop'] = False
        _watch23.close()

        with mir23._lock:
            mir23._generation += 1
        _taps23['state'] = 'STOPPED'
        _before23 = _pushes23()
        _stale_thread23 = threading.Thread(
            target=mir23._watch_dlna,
            args=(mirror._DlnaSender(_control23), _url_w23, _sess_w23, -7),
            daemon=True)
        _stale_thread23.start()
        _stale_thread23.join(timeout=2)
        check("a stale generation stops the watchdog without another push",
              _stale_thread23.is_alive() is False and _pushes23() == _before23,
              str(_pushes23()))
        mirror.DLNA_POLL_SECONDS = _saved_poll23
        mirror.DLNA_MAX_REPUSHES = _saved_rep23

        # -- prefill, the honest reason this target lags ----------------------
        _saved_prefill23 = mirror.dlna_prefill_bytes
        mirror.dlna_prefill_bytes = lambda profile: 4096
        _server23c = mirror.start_stream_server(mirror._Session('dlna'))
        _server23d = mirror.start_stream_server(mirror._Session('dlna'))
        try:
            _server23c.broadcaster.feed(b'a' * 8192)
            mir23._generation += 1
            _t023 = time.time()
            mir23._prefill(_server23c, types.SimpleNamespace(poll=lambda: None),
                           mir23._generation)
            check("prefill returns as soon as the ring holds the promised hoard",
                  time.time() - _t023 < 1.5
                  and _server23c.broadcaster.bytes >= 4096, '')
            mir23._generation += 1
            _t023 = time.time()
            mir23._prefill(_server23d, types.SimpleNamespace(poll=lambda: 1),
                           mir23._generation)
            check("and does not wait for an encoder that already died",
                  time.time() - _t023 < 1.0, '')
        finally:
            for _s23c in (_server23c, _server23d):
                _s23c.broadcaster.close()
                _s23c.shutdown()
                _s23c.server_close()
            mirror.dlna_prefill_bytes = _saved_prefill23

        # -- the whole thing, end to end --------------------------------------
        mir23b = _Mirror23()
        mirror.find_ffmpeg = lambda: fake_ffmpeg23
        mirror.start_search = lambda: True
        mirror._devices = []
        _saved_keep23 = mirror._keep_awake
        _saved_drop23 = mirror._stop_awake
        mirror._keep_awake = lambda: 'awake'
        mirror._stop_awake = lambda handle: _taps23.setdefault(
            'sleep', []).append(handle)
        mirror.dlna_prefill_bytes = lambda profile: 24576
        mirror.DLNA_POLL_SECONDS = 0.1
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'dlna')
        utils.Setting.set(mirror.SettingProperty.Mirror_Dlna_Control, _control23)
        utils.Setting.set(mirror.SettingProperty.Mirror_Target_Name, '小电视')
        _taps23['calls'] = []
        _taps23['state'] = 'NO_MEDIA_PRESENT'
        _taps23['sleep'] = []
        try:
            _notify23.clear()
            mir23b.start_mirror()
            check("a DLNA mirror starts against a renderer with no Google stack",
                  _wait_until(mir23b.is_mirroring, timeout=30), str(_notify23))
            _m23 = _re23.search(r'<CurrentURI>([^<]+)</CurrentURI>',
                                _taps23.get('uri', ''))
            _pushed_url23 = _m23.group(1) if _m23 else ''
            check("the URL the TV was handed points back at this machine",
                  _pushed_url23.startswith('http://127.0.0.1:')
                  and '/stream/' in _pushed_url23
                  and _pushed_url23.endswith('.mpg'), _pushed_url23)
            check("the start message says how late this target is",
                  any('小电视' in str(n) and '秒延迟' in str(n) for n in _notify23),
                  str(_notify23))
            _where23 = _pushed_url23.split('//', 1)[1]
            _conn23 = http.client.HTTPConnection(
                '127.0.0.1', int(_where23.split('/')[0].rsplit(':', 1)[1]),
                timeout=10)
            _conn23.request('GET', '/' + _where23.split('/', 1)[1],
                            headers={'Range': 'bytes=0-8191'})
            _resp23 = _conn23.getresponse()
            _tv_st23, _tv_hd23 = _resp23.status, dict(_resp23.getheaders())
            _tv_body23 = _resp23.read()
            _conn23.close()
            check("what the TV fetches is a 206 of exactly the bytes it wants",
                  _tv_st23 == 206 and len(_tv_body23) == 8192
                  and _tv_hd23['Accept-Ranges'] == 'bytes'
                  and _tv_hd23['transferMode.dlna.org'] == 'Streaming'
                  and _tv_hd23['Content-Type'] == 'video/mpeg', str(_tv_st23))
            _stats23 = mir23b.stats()
            check("the session reports its profile and how much is hoarded",
                  _stats23.get('kind') == 'dlna'
                  and _stats23.get('profile') == mirror.dlna_profile_id(
                      mirror.dlna_profile())
                  and _stats23.get('buffered', 0) > 0 and _stats23['bytes'] > 0,
                  str(_stats23))
            check("the renderer's own state reaches the menu",
                  _wait_until(lambda: mir23b.stats().get('state') == 'PLAYING',
                              timeout=10), str(mir23b.stats()))
            check("a DLNA mirror keeps the machine awake like every other target",
                  mir23b._awake == 'awake', str(mir23b._awake))
            _bridged23 = 'http://elsewhere/movie.mp4'
            _before23 = _pushes23()
            mir23b.set_media_url(_bridged23)
            check("a push while mirroring is bridged to the TV, not refused",
                  _wait_until(lambda: _pushes23() > _before23
                              and any(_bridged23 in b for _c, b
                                      in _taps23['calls'][-4:]), timeout=10),
                  str([c for c, _b in _taps23['calls'][-3:]]))
            _before23 = len(_taps23['calls'])
            mir23b.set_media_url(_bridged23)
            check("the address we are already playing is not a push to bridge",
                  len(_taps23['calls']) == _before23, str(_before23))
            mir23b.stop_mirror()
            # Teardown runs on its own thread and releases the sleep assert
            # only after the Stop and the server shutdown, so poll for it --
            # sampling it once proves the thread was scheduled, not the order.
            check("stopping tells the TV to stop and releases the sleep assert",
                  _wait_until(lambda: _taps23['sleep'] == ['awake'], timeout=20)
                  and any(c == 'Stop' for c, _b in _taps23['calls'])
                  and mir23b.stats() == {},
                  '{} {} {}'.format([c for c, _b in _taps23['calls'][-3:]],
                                    mir23b.stats(), _taps23['sleep']))
        finally:
            mirror._keep_awake = _saved_keep23
            mirror._stop_awake = _saved_drop23
            mirror.dlna_prefill_bytes = _saved_prefill23
            mirror.DLNA_POLL_SECONDS = _saved_poll23
            utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
            utils.Setting.unset(mirror.SettingProperty.Mirror_Dlna_Control)
            if mir23b.is_mirroring():
                mir23b.stop_mirror()

        # -- the menu ----------------------------------------------------------
        class _FakeMirror23(object):
            def __init__(self):
                self.stops = 0
                self.starts = 0

            def is_mirroring(self):
                return True

            def is_starting(self):
                return False

            def audio_dropped(self):
                return False

            def stop_mirror(self):
                self.stops += 1

            def start_mirror(self):
                self.starts += 1

            def viewer_url(self):
                return ''

            def stats(self):
                return {'kind': 'dlna', 'clients': 1, 'chunks': 30,
                        'bytes': 900000, 'drops': 0, 'mbps': 1.2, 'seconds': 75,
                        'profile': 'ps-pal', 'state': 'PLAYING',
                        'buffered': 20971520}

        fake23 = _FakeMirror23()
        setting23 = mirror.ScreenMirrorSetting()
        _saved_renderer_fn23 = setting23._renderer
        setting23._renderer = lambda: fake23
        _saved_devices23 = list(mirror._dlna_devices)
        mirror._dlna_devices = [('厨房的小电视', _control23, '192.0.2.40'),
                                ('客厅', 'http://192.0.2.41:49152/x',
                                 '192.0.2.41')]
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'dlna')
        utils.Setting.set(mirror.SettingProperty.Mirror_Dlna_Control, _control23)
        _searches23 = []
        _saved_rr23 = mirror.start_renderer_search
        _saved_cr23 = mirror.start_search
        mirror.start_renderer_search = lambda: _searches23.append('dlna') or True
        mirror.start_search = lambda: _searches23.append('cast') or True
        _st23 = setting23.console_state()
        _labels23 = _console_texts(_st23)
        check("the console offers four targets and names the running one",
              _st23['output']['kind'] == 'dlna'
              and len(_st23['output']['options']) == 4
              and any(o['key'] == 'dlna' and 'MPEG-PS' in o['label']
                      for o in _st23['output']['options']), str(_st23['output']))
        check("a DLNA target lists the renderers discovery found",
              '厨房的小电视 · 192.0.2.40' in _labels23
              and '客厅 · 192.0.2.41' in _labels23
              and any(d['id'] == _control23 and d['selected']
                      for d in _mc.current_channel(_st23)['devices']),
              str(_labels23))
        check("and the live line names the profile, the TV's state and the hoard",
              '已镜像 1:15' in _st23['status_line']
              and '档位 ps-pal' in _st23['status_line']
              and '电视 PLAYING' in _st23['status_line']
              and '缓冲 20 MiB' in _st23['status_line'], _st23['status_line'])
        check("all five compatibility profiles are offered",
              len(_st23['profiles']['options']) == len(mirror.DLNA_PROFILES)
              and _st23['profiles']['current'] == 'ps-pal', str(_st23['profiles']))
        check("a DLNA mirror has no address to copy",
              not _st23['viewer']['available'], str(_st23['viewer']))
        _sections23 = _mc.sections_for(_st23)
        check("and a DLNA target is laid out with its compatibility profiles",
              [s for s in _sections23 if s != 'requirements'][:3]
              == ['channels', 'devices', 'profiles'], str(_sections23))
        # The board is the app's, not this plugin's: whatever an *other* plugin
        # said it needs is on it, and the window shows that instead of keeping
        # it to a five-second notification. Where it sits is the whole design.
        check("a requirement from another plugin sits under the devices, "
              "not above them",
              'requirements' not in _sections23
              or _sections23.index('requirements')
              == _sections23.index('devices') + 1, str(_sections23))
        setting23.console_action('set-profile', {'value': 'ts-h264'})
        check("choosing a profile stores it and restarts the running stream",
              mirror.dlna_profile_id(mirror.dlna_profile()) == 'ts-h264'
              and fake23.stops == 1 and fake23.starts == 1, str(fake23.stops))
        fake23.stops = fake23.starts = 0
        setting23.console_action('set-profile', {'value': 'ts-h264'})
        check("choosing the profile that is already chosen changes nothing",
              fake23.stops == 0 and fake23.starts == 0)
        check("and a profile that is not on the list is refused",
              setting23.console_action('set-profile', {'value': 'vp9'})['code'] == 1
              and mirror.dlna_profile_id(mirror.dlna_profile()) == 'ts-h264', '')
        setting23.console_action('set-dlna-target',
                                 {'name': '客厅',
                                  'control': 'http://192.0.2.41:49152/x'})
        check("choosing a TV stores its control URL and leaves the Chromecast "
              "target alone",
              mirror.dlna_target() == ('客厅', 'http://192.0.2.41:49152/x')
              and mirror.output_kind() == 'dlna', str(mirror.dlna_target()))
        _searches23[:] = []
        setting23.console_action('refresh')
        check("refresh reruns every protocol's search, not the chosen one's",
              _searches23 == ['cast', 'dlna'], str(_searches23))
        # A search that finished with nothing in it is a *result*: the row has
        # to say 没有发现 rather than keep the「还没有搜过」it had before anyone
        # looked, which is what made an empty LAN read as「still working」.
        mirror._dlna_searched_at = time.time()
        check("an empty renderer list says so instead of vanishing",
              (setattr(mirror, '_dlna_devices', []), True)[1]
              and '没有发现' in _channel(setting23.console_state(),
                                       'dlna')['words'], '')
        mirror._dlna_searched_at = 0.0
        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
        _searches23[:] = []
        setting23.console_state()
        check("and the cast protocol is searched whatever the target is",
              _searches23 == ['cast', 'dlna'], str(_searches23))
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'dlna')
        _searches23[:] = []
        setting23.console_state()
        check("opening the console on an empty renderer list kicks a search",
              _searches23 == ['cast', 'dlna'], str(_searches23))
        mirror._dlna_devices = [('厨房的小电视', _control23, '192.0.2.40')]
        mirror._devices = [('Living Room TV', '192.0.2.7', 8009)]
        # Both searches start out never-run, which is due by definition; this
        # case is about a *young* answer not being re-run every poll, so date
        # both of them -- including the Chromecast one, which the window now
        # kicks on every poll too.
        mirror._searched_at = mirror._dlna_searched_at = time.time()
        _searches23[:] = []
        setting23.console_state()
        check("a full device list does not search on every poll",
              _searches23 == [], str(_searches23))
        # The bug this guards: on a LAN where discovery finds nothing the list
        # stayed empty, so the menu re-kicked the search on every open and the
        # submenu could only ever read「搜索中…再展开一次菜单」. A completed
        # search has to turn that into a dated「没有发现」and stop re-searching,
        # while the very first (still-running) look is the only「搜索中」left.
        # The console polls its state once a second, so this guard matters more
        # than it ever did for a menu.
        mirror._dlna_devices = []
        #: …and nothing was dropped as "ours": the counter describes the same
        #: search as the list, so a fake search has to state both. Left over
        #: from Part 23's real search (which only ever met this machine), it
        #: would otherwise turn the verdict below into a claim about a search
        #: that never ran.
        mirror._self_alone['dlna'] = 0
        mirror._dlna_searched_at = time.time()
        _searches23[:] = []
        _empty23 = _channel(setting23.console_state(), 'dlna')
        check("a completed empty search reads as a dated「没有发现」and is not "
              "re-run on every poll",
              _searches23 == []
              and not _empty23['searching']
              and _empty23['words'].startswith('没有发现 DLNA 电视；')
              and '（搜于 ' in _empty23['words']
              and '重搜设备' in _empty23['words'],
              str(_empty23))
        check("and that verdict stays off the other protocol's row",
              _channel(setting23.console_state(), 'cast')['words']
              .startswith('发现 1 台'), str(_channel(
                  setting23.console_state(), 'cast')))
        mirror._dlna_searched_at = 0.0
        mirror._dlna_searching = True
        _first23 = _channel(setting23.console_state(), 'dlna')
        check("only the very first, still-running search says「搜索中」",
              _first23['searching'] and not _first23['searched_at']
              and _first23['words'] == '正在搜索局域网设备…', str(_first23))
        mirror._dlna_searching = False
        utils.Setting.unset(mirror.SettingProperty.Mirror_Dlna_Control)
        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
        utils.Setting.unset(mirror.SettingProperty.Mirror_Dlna_Profile)
        setting23._renderer = _saved_renderer_fn23
        mirror.start_renderer_search = _saved_rr23
        mirror.start_search = _saved_cr23
        mirror._dlna_devices = _saved_devices23
    finally:
        for _s23 in (_server23, _tv23):
            if _s23 is not None:
                try:
                    _s23.shutdown()
                    _s23.server_close()
                except Exception:
                    pass
        if _mir23 is not None:
            _mir23.stop_mirror()
        cherrypy.engine.unsubscribe('app_notify', _notify23_rec)
        utils.SETTING_DIR = _saved_dir23
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting23
        _shutil.rmtree(_tmp23, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the DLNA target behaves", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 24: screen mirror v0.6 -- Cast Streaming, the low-latency target
#
# Every claim here is about bytes a *television* is supposed to agree with, and
# the far end of that loop is a fake built from the same tables as the sender.
# So read these cases for what they are: the handshake is ordered the way the
# reference orders it, the framing and the cipher are self-consistent (the AES
# half is pinned against /usr/bin/openssl output), and the sender reacts to a
# checkpoint, a PLI and a window full of un-acked frames. None of it is evidence
# that a real Chromecast paints -- that is scripts/cast_streaming_probe.py, and
# AGENTS.md 4.9 is explicit that the two are different claims.
# --------------------------------------------------------------------------
print("\n=== Part 24: screen mirror v0.6 (Cast Streaming) ===")

_SC24 = b'\x00\x00\x00\x01'


def _slice24(kind, first, body=b''):
    """One Annex-B slice NAL. `first` is first_mb_in_slice == 0, i.e. ue(v)
    zero, i.e. a single '1' bit -- the top bit of the byte after the header."""
    return _SC24 + bytes([kind, 0x80 if first else 0x00]) + body


# Shared by the fake encoder below and by the assertions, so the two cannot
# drift apart: `exec` the same source the shell script runs.
_AU_SOURCE24 = '''
def au(index):
    """What the encoder puts on the wire for one picture.

    x264 under -tune zerolatency splits a picture into several slice NALs, so
    the fake does too: a key picture is AUD + SPS + PPS + two IDR slices, a
    delta picture is AUD + three P slices.
    """
    tag = bytes([index % 251])
    if index % 6:
        return (b'\\x00\\x00\\x00\\x01\\x09\\x50'
                + b'\\x00\\x00\\x00\\x01\\x01\\x80' + tag * 80
                + b'\\x00\\x00\\x00\\x01\\x01\\x00' + tag * 80
                + b'\\x00\\x00\\x00\\x01\\x01\\x00' + tag * 80)
    return (b'\\x00\\x00\\x00\\x01\\x09\\x50'
            + b'\\x00\\x00\\x00\\x01\\x67\\x80'
            + b'\\x00\\x00\\x00\\x01\\x68\\x80'
            + b'\\x00\\x00\\x00\\x01\\x05\\x80' + tag * 900
            + b'\\x00\\x00\\x00\\x01\\x05\\x00' + tag * 900)
'''


def _decode24(packets, key, iv_mask, mirror):
    """{frame id: (is_key, access unit)} from the packets a receiver got.

    Decrypting with the key from the OFFER is the point: it proves the sender
    used the material it announced, per frame, once per access unit rather than
    once per packet.
    """
    frames = {}
    for packet in packets:
        (_ver, ptype, _seq, _ts, _ssrc, flags, frame8, packet_id, _count,
         _ref) = struct.unpack('>BBHII' + 'BBHHB', packet[:19])
        if ptype & 0x7F != 96:
            continue
        if packet_id == 0:
            frames[frame8] = [flags, bytearray()]
        frames[frame8][1] += packet[19:]
    out = {}
    for frame8, (flags, cipher) in frames.items():
        plain = mirror.Aes128Ctr(key).crypt(mirror.frame_iv(iv_mask, frame8),
                                            bytes(cipher))
        out[frame8] = (bool(flags & 0x80), plain)
    return out


def _header24(packet):
    return struct.unpack('>BBHII' + 'BBHHB', packet[:19])


def _checkpoint24(frame8, media_ssrc, receiver_ssrc, delay=12, losses=b''):
    """A receiver's CAST acknowledgement, as openscreen writes it.

    The RTCP length field counts 32-bit words *including* the four-byte header,
    minus one -- so it is just the body's word count here.
    """
    body = (struct.pack('>II', receiver_ssrc, media_ssrc) + b'CAST'
            + bytes([frame8, len(losses) // 4]) + struct.pack('>H', delay)
            + losses)
    return struct.pack('>BBH', 0x80 | 15, 206, len(body) // 4) + body


def _pli24(media_ssrc, receiver_ssrc=0x1234):
    body = struct.pack('>II', receiver_ssrc, media_ssrc)
    return struct.pack('>BBH', 0x80 | 1, 206, len(body) // 4) + body


class _FakeCastDevice24(object):
    """A Chromecast that answers the mirroring app: TLS control, UDP media.

    Real sockets on both planes, because half of what the sender gets wrong is
    invisible without one -- an unconnected UDP socket, a per-frame nonce, a
    compound RTCP reply the receiver addresses by media SSRC.
    """

    def __init__(self, mirror, certfile, keyfile, refuse=False, stale=False):
        self.mirror = mirror
        self.refuse = refuse
        self.received = []          # [(namespace, type, payload)]
        self.rtp = []               # RTP packets the sender pushed
        self.rtcp = []              # sender reports
        self.peer = None            # the sender's media address
        self.offer = None
        self.loaded = []
        self.answer_ssrc = 0x5EED0001
        self.apps = []
        self.running = True
        if stale:
            self.apps = [{'appId': mirror.MIRROR_APP_ID,
                          'transportId': 'stale-0', 'sessionId': 'stale-9'}]
        self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.udp.bind(('127.0.0.1', 0))
        self.udp_port = self.udp.getsockname()[1]
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certfile=certfile, keyfile=keyfile)
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen(4)
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()
        threading.Thread(target=self._read_udp, daemon=True).start()

    # -- control plane -------------------------------------------------------

    def _accept(self):
        while self.running:
            try:
                raw, _addr = self.listener.accept()
            except OSError:
                return
            try:
                conn = self.context.wrap_socket(raw, server_side=True)
            except Exception:
                raw.close()
                continue
            threading.Thread(target=self._serve, args=(conn,),
                             daemon=True).start()

    @staticmethod
    def _read(conn, count):
        buf = b''
        while len(buf) < count:
            chunk = conn.recv(count - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def _serve(self, conn):
        with conn:
            while self.running:
                try:
                    head = self._read(conn, 4)
                    body = self._read(conn, struct.unpack('>I', head)[0])
                    message = cast.parse_cast_message(body)
                except Exception:
                    return
                if message is None:
                    return
                try:
                    self._dispatch(conn, message)
                except Exception:
                    return

    def _write(self, conn, destination, namespace, payload):
        blob = cast.encode_cast_message('receiver-0', destination, namespace,
                                        json.dumps(payload))
        conn.sendall(struct.pack('>I', len(blob)) + blob)

    def _status(self, conn, destination, request_id=None):
        self._write(conn, destination, self.mirror.NS_RECEIVER, {
            'type': 'RECEIVER_STATUS', 'requestId': request_id,
            'status': {'applications': list(self.apps),
                       'volume': {'level': 1.0, 'muted': False}}})

    def _dispatch(self, conn, message):
        destination = message['source_id']
        namespace = message['namespace']
        if message.get('payload_type') != 0:
            # The device-auth CHALLENGE: any answer at all walks the sender
            # past its `connect()`.
            self._write(conn, destination, self.mirror.NS_RECEIVER,
                        {'type': 'AUTH'})
            return
        data = json.loads(message.get('payload_utf8') or '{}')
        kind = data.get('type')
        self.received.append((namespace, kind, data))
        if kind == 'GET_STATUS':
            self._status(conn, destination, data.get('requestId'))
        elif kind == 'STOP':
            gone = data.get('sessionId')
            self.apps = [app for app in self.apps
                         if app.get('sessionId') != gone
                         and app.get('transportId') != gone]
            self._status(conn, destination, data.get('requestId'))
        elif kind == 'LAUNCH':
            self._launch(conn, destination, data)
        elif namespace == self.mirror.NS_WEBRTC and kind == 'OFFER':
            self.offer = data
            self._write(conn, destination, self.mirror.NS_WEBRTC, {
                'type': 'ANSWER', 'seqNum': data.get('seqNum'),
                'result': 'ok',
                'answer': {'udpPort': self.udp_port, 'sendIndexes': [0],
                           'ssrcs': [self.answer_ssrc]}})
        elif kind == 'LOAD':
            self.loaded.append(data)
            self._write(conn, destination, self.mirror.NS_MEDIA, {
                'type': 'MEDIA_STATUS', 'requestId': data.get('requestId'),
                'status': [{'mediaSessionId': 1, 'playerState': 'PLAYING'}]})
        elif kind == 'PING':
            self._write(conn, destination, namespace, {'type': 'PONG'})

    def _launch(self, conn, destination, data):
        app_id = data.get('appId')
        if app_id == self.mirror.MIRROR_APP_ID and self.refuse:
            self._write(conn, destination, self.mirror.NS_RECEIVER, {
                'type': 'LAUNCH_ERROR', 'requestId': data.get('requestId'),
                'reason': 'RECEIVER_ERROR_UNHANDLED'})
            return
        self.apps = [app for app in self.apps if app.get('appId') != app_id]
        self.apps.append({'appId': app_id, 'transportId': 'transport-1',
                          'sessionId': 'session-1'})
        self._status(conn, destination, data.get('requestId'))

    # -- media plane ---------------------------------------------------------

    def _read_udp(self):
        self.udp.settimeout(0.2)
        while self.running:
            try:
                data, peer = self.udp.recvfrom(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            self.peer = peer
            (self.rtcp if data[1] == 200 else self.rtp).append(data)

    def ack(self, frame8, delay=12):
        self.udp.sendto(_checkpoint24(frame8, self.offer_ssrc,
                                      self.answer_ssrc, delay), self.peer)

    def nack_picture(self):
        self.udp.sendto(_pli24(self.offer_ssrc), self.peer)

    @property
    def offer_ssrc(self):
        return self.offer['offer']['supportedStreams'][0]['ssrc']

    @property
    def keys(self):
        stream = self.offer['offer']['supportedStreams'][0]
        return (bytes.fromhex(stream['aesKey']),
                bytes.fromhex(stream['aesIvMask']))

    def close(self):
        self.running = False
        for sock in (self.listener, self.udp):
            try:
                sock.close()
            except OSError:
                pass


try:
    _saved_setting24 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir24 = utils.SETTING_DIR
    _tmp24 = _tempfile.mkdtemp(prefix="macast-mirror24-")
    _notify24 = []
    _notify24_rec = lambda *a, **k: _notify24.append(a)      # noqa: E731
    _mir24 = None
    _saved24 = {}
    _devices24 = []
    try:
        utils.SETTING_DIR = _tmp24
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp24, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify24_rec)

        mirror = _load_plugin("screen_mirror_plugin_v06", "screen_mirror.py")
        _ns24 = {'bytes': bytes}
        exec(_AU_SOURCE24, _ns24)
        _au24 = _ns24['au']
        _cap24 = mirror._Capture('test', [['-f', 'test', '-i', 'x']],
                                 screens=[(0, 'Capture screen 0')])

        # -- the OFFER: one video stream, sized before a frame exists --------
        offer = mirror.build_offer(0x11223344, b'A' * 16, b'B' * 16, 1280, 720,
                                   mirror.FPS, mirror.CAST_STREAM_MAX_BITRATE)
        stream = offer['offer']['supportedStreams'][0]
        check("the OFFER is mirroring mode with one video stream",
              offer['type'] == 'OFFER' and offer['offer']['castMode'] == 'mirroring'
              and len(offer['offer']['supportedStreams']) == 1
              and stream['type'] == 'video_source'
              and stream['codecName'] == 'h264', json.dumps(offer))
        check("a cast RTP profile with the mirroring payload type",
              stream['rtpProfile'] == 'cast'
              and stream['rtpPayloadType'] == 96
              and stream['timeBase'] == '1/90000', str(stream))
        check("aesKey and aesIvMask are exactly 32 hex digits",
              len(stream['aesKey']) == 32 and len(stream['aesIvMask']) == 32
              and set(stream['aesKey']) <= set('0123456789abcdef'),
              stream['aesKey'])
        check("the frame size and rate are claimed up front",
              stream['resolutions'] == [{'width': 1280, 'height': 720}]
              and stream['maxFrameRate'] == '24000/1000'
              and stream['maxBitRate'] == mirror.CAST_STREAM_MAX_BITRATE
              and stream['targetDelay'] == 200, str(stream))
        _key, _mask = mirror.aes_material()
        check("the sender invents both key and mask, sixteen bytes each",
              len(_key) == 16 and len(_mask) == 16 and _key != _mask)
        check("every frame gets its own counter block, big-endian at byte 8",
              mirror.frame_iv(b'\x00' * 16, 1) == b'\x00' * 8 + b'\x00\x00\x00\x01'
              + b'\x00' * 4
              and mirror.frame_iv(b'\x00' * 16, 256)[-4:] == b'\x00' * 4
              and mirror.frame_iv(b'\xff' * 16, 5)[8:12] == b'\xff\xff\xff\xfa',
              mirror.frame_iv(b'\x00' * 16, 1).hex())

        # -- the cipher, pinned against /usr/bin/openssl ---------------------
        _vector = bytes(range(256)) * 3
        _cipher = mirror.Aes128Ctr(bytes(range(16))).crypt(
            mirror.frame_iv(bytes([0x0f]) * 16, 7), _vector)
        check("AES-128-CTR is byte-identical to openssl's",
              len(_cipher) == len(_vector)
              and _cipher[:16].hex() == '54be03754f76e8a02f70d7bd5e6414fc'
              and __import__('hashlib').sha256(_cipher).hexdigest() ==
              '9b1ef02bb66d8d7d20e417fb65db902e2ddb7f6d5cebf44d470edd4dff2c4a8e',
              _cipher[:16].hex())
        check("the keystream counter carries over a block boundary",
              _cipher[16:20] == mirror.Aes128Ctr(bytes(range(16))).crypt(
                  mirror.frame_iv(bytes([0x0f]) * 16, 7), _vector)[16:20]
              and len(mirror.Aes128Ctr(bytes(range(16))).crypt(
                  mirror.frame_iv(bytes([0x0f]) * 16, 7), b'z' * 17)) == 17)

        # -- the ANSWER ------------------------------------------------------
        check("sendIndexes and ssrcs pair up positionally",
              mirror.parse_answer({'type': 'ANSWER', 'result': 'ok', 'answer': {
                  'udpPort': 50232, 'sendIndexes': [1, 0],
                  'ssrcs': [11, 22]}}) == (50232, 22))
        check("an ANSWER without a result field is still an answer",
              mirror.parse_answer({'type': 'ANSWER', 'answer': {
                  'udpPort': 1, 'sendIndexes': [0], 'ssrcs': [7]}}) == (1, 7))

        def _refused24(payload):
            try:
                mirror.parse_answer(payload)
            except RuntimeError:
                return True
            except Exception:
                return False
            return False

        check("an error ANSWER is a refusal, not an empty port",
              _refused24({'type': 'ANSWER', 'result': 'error',
                          'error': {'code': 10}}))
        check("a port that cannot be sent to is a refusal",
              _refused24({'type': 'ANSWER', 'answer': {'udpPort': 0,
                                                       'sendIndexes': [0],
                                                       'ssrcs': [7]}})
              and _refused24({'type': 'ANSWER', 'answer': {'udpPort': 70000,
                                                           'sendIndexes': [0],
                                                           'ssrcs': [7]}}))
        check("a receiver that took the audio stream but not the video is refused",
              _refused24({'type': 'ANSWER', 'answer': {'udpPort': 4000,
                                                       'sendIndexes': [1],
                                                       'ssrcs': [7]}})
              and _refused24({'type': 'ANSWER', 'answer': {'udpPort': 4000,
                                                           'sendIndexes': [0],
                                                           'ssrcs': []}}))

        # -- RTP + the 7-byte Cast header ------------------------------------
        packet = mirror.cast_packet(b'x' * 10, 0xABCDEF01, 7, 999, 5, True,
                                    1, 3, 5, marker=True)
        fixed = _header24(packet)
        check("the fixed RTP header says version 2 and our payload type",
              fixed[0] == 0x80 and fixed[1] == (0x80 | 96)
              and fixed[2] == 7 and fixed[3] == 999 and fixed[4] == 0xABCDEF01,
              str(fixed))
        check("the Cast header flags a key frame and the referenced id in use",
              fixed[5] == 0xC0 and fixed[6] == 5 and fixed[7] == 1
              and fixed[8] == 2 and fixed[9] == 5, str(fixed[5:]))
        check("a delta frame advertises the frame before it and no key flag",
              _header24(mirror.cast_packet(b'', 1, 0, 0, 9, False, 0, 1,
                                           8))[5] == 0x40
              and _header24(mirror.cast_packet(b'', 1, 0, 0, 9, False, 0, 1,
                                               8))[9] == 8)
        _big = b'y' * (mirror.MAX_PAYLOAD * 2 + 7)
        _parts, _next = mirror.cast_packets(_big, 3, False, 2, 0x11, 100, 555)
        check("a frame is encrypted once and then sliced whole",
              len(_parts) == 3
              and sum(len(p) - 19 for p in _parts) == len(_big)
              and len(_parts[-1]) - 19 == 7, str([len(p) for p in _parts]))
        check("only the last packet of a frame carries the marker",
              [(_header24(p)[1] & 0x80) != 0 for p in _parts] == [False, False,
                                                                  True])
        check("the sequence number advances on every packet",
              [_header24(p)[2] for p in _parts] == [100, 101, 102]
              and _next == 103, str([_header24(p)[2] for p in _parts]))
        _wrap, _after = mirror.cast_packets(_big, 3, True, 3, 0x11, 0xFFFF, 0)
        check("and wraps around at sixteen bits",
              [_header24(p)[2] for p in _wrap] == [0xFFFF, 0, 1]
              and _after == 2, str([_header24(p)[2] for p in _wrap]))
        check("an empty frame still says it is one packet long",
              _header24(mirror.cast_packets(b'', 0, True, 0, 1, 0, 0)[0][0]
                        )[8] == 0)

        # -- RTCP ------------------------------------------------------------
        _sr = mirror.rtcp_sender_report(0x11223344, mirror.ntp_timestamp(0.5),
                                        9000, 12, 3456)
        check("a sender report is 28 bytes of version 2, type 200, length 6",
              len(_sr) == 28 and _sr[0] == 0x80 and _sr[1] == 200
              and struct.unpack('>H', _sr[2:4])[0] == 6, _sr.hex())
        check("its fields are big-endian and the SSRC is ours",
              struct.unpack('>I', _sr[4:8])[0] == 0x11223344
              and struct.unpack('>I', _sr[16:20])[0] == 9000
              and struct.unpack('>II', _sr[20:28]) == (12, 3456), _sr.hex())
        check("the NTP stamp is the UNIX clock moved to the 1900 epoch",
              mirror.ntp_timestamp(0.0) >> 32 == mirror.NTP_UNIX_OFFSET
              and mirror.ntp_timestamp(0.0) & 0xFFFFFFFF == 0
              and mirror.ntp_timestamp(0.5) & 0xFFFFFFFF == 0x80000000,
              hex(mirror.ntp_timestamp(0.5)))
        check("octet and packet counts wrap at 32 bits",
              struct.unpack('>H', _sr[2:4])[0] == 6
              and len(mirror.rtcp_sender_report(1, 2, 3, 1 << 32,
                                                (1 << 32) + 5)) == 28
              and struct.unpack('>II', mirror.rtcp_sender_report(
                  1, 2, 3, 1 << 32, (1 << 32) + 5)[20:28]) == (0, 5))
        check("a picture loss asks for a redraw",
              mirror.parse_rtcp(_pli24(0x11), 0x11) == [('picture-loss',)])
        check("feedback about another stream is not ours to act on",
              mirror.parse_rtcp(_pli24(0x99), 0x11) == [])
        check("a checkpoint gives back the 8-bit frame id and the playout delay",
              mirror.parse_rtcp(_checkpoint24(200, 0x11, 0x22, 37), 0x11)
              == [('checkpoint', 200, 37)],
              str(mirror.parse_rtcp(_checkpoint24(200, 0x11, 0x22, 37), 0x11)))
        _lost = bytes([7, 0x01, 0x02, 0xFF]) + bytes([9, 0xFF, 0xFF, 0x01])
        check("per-packet loss is read and named, then ignored",
              mirror.parse_rtcp(_checkpoint24(4, 0x11, 0x22, 5, losses=_lost),
                                0x11) == [('checkpoint', 4, 5),
                                          ('nack', 7, 0x0102, 0xFF),
                                          ('nack', 9, 0xFFFF, 0x01)])
        _compound = _pli24(0x11) + _checkpoint24(6, 0x11, 0x22)
        check("compound RTCP is walked block by block",
              mirror.parse_rtcp(_compound, 0x11) == [('picture-loss',),
                                                     ('checkpoint', 6, 12)],
              str(mirror.parse_rtcp(_compound, 0x11)))
        check("an unknown RTCP type is skipped, not fatal",
              mirror.parse_rtcp(struct.pack('>BBH', 0x80, 201, 0)
                                + _checkpoint24(8, 0x11, 0x22), 0x11)
              == [('checkpoint', 8, 12)])
        check("a CST2 tail block after the losses is stepped over",
              mirror.parse_rtcp(_checkpoint24(6, 0x11, 0x22)
                                + struct.pack('>BBH', 0x80, 206, 1)
                                + b'CST2', 0x11) == [('checkpoint', 6, 12)])
        check("an 8-bit frame id widens to the one we actually sent",
              mirror.expand_frame_id(200, 204) == 200
              and mirror.expand_frame_id(3, 259) == 259
              and mirror.expand_frame_id(3, 258) == 3
              and mirror.expand_frame_id(0, -1) == 0)

        # -- Annex-B access units, with real encoder habits ------------------
        _units = mirror._AccessUnits()
        check("a sliced picture is ONE access unit, not three",
              _units.feed(_au24(0) + _au24(1)) == [_au24(0)]
              and _units.feed(_au24(2)) == [_au24(1)])
        # A NAL is only known to be over when the next start code arrives, so
        # the honest comparison is what it costs to close picture 0: with the
        # delimiter, its few bytes; without it, the next picture's first slice.
        check("the delimiter closes a frame before its next slice arrives",
              mirror._AccessUnits().feed(_au24(0) + _SC24 + b'\x09\x50'
                                         + _slice24(1, True)) == [_au24(0)]
              and mirror._AccessUnits().feed(_au24(0)
                                             + _slice24(1, True)) == [])
        _dribble = mirror._AccessUnits()
        _stream = _au24(0) + _au24(1) + _au24(2) + _au24(3)
        _got = []
        for _i in range(0, len(_stream), 13):
            _got += _dribble.feed(_stream[_i:_i + 13])
        check("chunk boundaries in the middle of a NAL change nothing",
              _got + _dribble.flush() == [_au24(i) for i in range(4)],
              '{} vs {}'.format(len(_got), 4))
        check("a key frame is recognised by its IDR slice, deltas are not",
              mirror._unit_is_key(_au24(0)) and not mirror._unit_is_key(_au24(1)))
        _second_key = mirror._AccessUnits()
        _keys24 = _second_key.feed(_au24(0) + _au24(6)) + _second_key.flush()
        check("in-band parameter sets are not duplicated in front of the IDR",
              _au24(0).count(_SC24 + b'\x67') == 1
              and _keys24[-1] == _au24(6)
              and _keys24[-1].count(_SC24 + b'\x67') == 1,
              str([len(u) for u in _keys24]))

        # -- the encoder has to be shaped for this target --------------------
        _cs24 = mirror.build_ffmpeg_command('ffmpeg', _cap24, 0, 12000000,
                                            kind='caststream')
        check("the low-latency target feeds a raw Annex-B pipe, not a muxer",
              _cs24[-3:] == ['-f', 'h264', 'pipe:1']
              and 'mpegts' not in _cs24, str(_cs24))
        check("the OFFER's frame size is pinned, letterboxed not stretched",
              _cs24[_cs24.index('-vf') + 1]
              == 'scale=1920:1080:force_original_aspect_ratio=decrease,'
                 'pad=1920:1080:(ow-iw)/2:(oh-ih)/2', str(_cs24))
        check("the pure-Python cipher sets the bitrate ceiling",
              _cs24[_cs24.index('-b:v') + 1] == str(
                  mirror.CAST_STREAM_MAX_BITRATE), str(_cs24))
        check("the delimiter and the fixed GOP are promised to x264",
              _cs24[_cs24.index('-aud') + 1] == '1'
              and 'scenecut=0' in _cs24[_cs24.index('-x264-params') + 1]
              and _cs24.index('-x264-params') > _cs24.index('libx264'),
              str(_cs24[-8:]))
        check("a hardware encoder is not handed x264-only options",
              '-x264-params' not in mirror.build_ffmpeg_command(
                  'ffmpeg', _cap24, 720, 5000000, kind='caststream',
                  encoder='hardware'))
        check("the LOAD targets are untouched by any of this",
              '-aud' not in mirror.build_ffmpeg_command('ffmpeg', _cap24, 720,
                                                        5000000, kind='cast'))
        check("this target has no HTTP server and no viewer URL",
              mirror.OUTPUTS['caststream'][2] is None
              and '无声音' in mirror.OUTPUTS['caststream'][0],
              str(mirror.OUTPUTS['caststream']))
    except Exception as e:
        import traceback
        traceback.print_exc()
        check("the Cast Streaming byte layer is self-consistent", False,
              "{}: {}".format(type(e).__name__, e))

    # -- the whole handshake, over real sockets ------------------------------
    try:
        from macast.server import Service as _Service24
        _cert24, _key24 = _Service24._ensure_self_signed_cert()
        check("the fake device can speak TLS with the app's own certificate",
              bool(_cert24) and os.path.exists(_cert24), str((_cert24, _key24)))

        _saved24 = {name: getattr(mirror, name) for name in (
            'find_ffmpeg', 'probe_capture', '_keep_awake', '_stop_awake',
            'start_search', '_devices')}
        fake24 = _write_fake(os.path.join(_tmp24, "bin24"), "ffmpeg",
                             "#!%s\nimport os, sys, time\n%s\n"
                             "out = sys.stdout.buffer\n"
                             "index = 0\n"
                             "limit = int(os.environ.get('FAKE_FRAMES', '100000'))\n"
                             "gap = float(os.environ.get('FAKE_GAP', '0.01'))\n"
                             "pause = os.environ.get('FAKE_PAUSE', '')\n"
                             "while index < limit:\n"
                             "    while pause and os.path.exists(pause):\n"
                             "        time.sleep(0.02)\n"
                             "    out.write(au(index))\n"
                             "    out.flush()\n"
                             "    index += 1\n"
                             "    time.sleep(gap)\n" % (sys.executable,
                                                        _AU_SOURCE24))
        mirror.find_ffmpeg = lambda: fake24
        # Platform-specific capture probing is Part 21's and 22's subject; this
        # part is about what happens to the encoded bytes.
        mirror.probe_capture = lambda ffmpeg, platform=None: _cap24
        mirror._keep_awake = lambda: 'awake'
        mirror._stop_awake = lambda handle: None
        mirror.start_search = lambda: True
        mirror._devices = []

        _rec24 = _StateRec()

        class _Mirror24(mirror.ScreenMirrorRenderer):
            @property
            def protocol(self):
                return _rec24

        mir24 = _Mirror24()
        _mir24 = mir24
        device24 = _FakeCastDevice24(mirror, _cert24, _key24, stale=True)
        _devices24.append(device24)
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'caststream')
        utils.Setting.set(mirror.SettingProperty.Mirror_Target,
                          '127.0.0.1:{}'.format(device24.port))
        utils.Setting.set(mirror.SettingProperty.Mirror_Target_Name, '客厅的电视')
        utils.Setting.set(mirror.SettingProperty.Mirror_Quality, '720')

        # The fake encoder honours this flag file; see its loop below.
        _pause24 = os.path.join(_tmp24, 'paused')
        os.environ['FAKE_PAUSE'] = _pause24
        mir24.start_mirror()
        check("the mirroring app is launched after the stale instance is stopped",
              _wait_until(lambda: device24.offer is not None, timeout=30),
              str([t for (_n, t, _d) in device24.received]))
        _types24 = [t for (_n, t, _d) in device24.received]
        _stop_at = _types24.index('STOP')
        check("STOP names the session it is clearing",
              device24.received[_stop_at][2].get('sessionId') == 'stale-9',
              str(device24.received[_stop_at]))
        check("and only then does LAUNCH ask for 0F5096E8",
              _stop_at < _types24.index('LAUNCH')
              and device24.received[_types24.index('LAUNCH')][2]['appId']
              == mirror.MIRROR_APP_ID
              and _types24.index('LAUNCH') < _types24.index('OFFER'),
              str(_types24))
        check("the OFFER the device saw offers one 720p stream",
              device24.offer['offer']['supportedStreams'][0]['resolutions']
              == [{'width': 1280, 'height': 720}], str(device24.offer))
        check("nothing is served over HTTP for this target",
              mir24._server is None and mir24._sink is not None
              and mir24.playing_url() == '')
        check("the LOAD path is not used: no media app, no LOAD",
              'LOAD' not in _types24 and mirror.DEFAULT_MEDIA_APP_ID
              not in [d.get('appId') for (_n, t, d) in device24.received
                      if t == 'LAUNCH'], str(_types24))

        check("the picture arrives as RTP packets on the port we were given",
              _wait_until(lambda: len(device24.rtp) >= 6, timeout=20),
              '{} packets'.format(len(device24.rtp)))
        _frames = _decode24(device24.rtp, device24.keys[0], device24.keys[1],
                            mirror)
        check("the first frame decrypts, with the offered key, to the encoder's "
              "whole access unit",
              _frames.get(0) == (True, _au24(0)),
              str((len(_frames), _frames.get(0, (False, b''))[1][:24])))
        check("a sliced picture arrives as several packets of one frame",
              len([p for p in device24.rtp
                   if _header24(p)[6] == 0]) == 2,
              str([_header24(p)[6:9] for p in device24.rtp[:6]]))
        check("deltas are not advertised as key frames, and reference the "
              "frame before them",
              _frames.get(1) == (False, _au24(1))
              and _header24([p for p in device24.rtp if _header24(p)[6] == 1]
                            [0])[9] == 0, str(_frames.get(1, (0, b''))[1][:16]))
        _seqs = [_header24(p)[2] for p in device24.rtp]
        check("the sequence number is monotonic across frames",
              all((b - a) % 0x10000 == 1 for a, b in zip(_seqs, _seqs[1:])),
              str(_seqs[:10]))
        check("the SSRC on every packet is the one the OFFER announced",
              {_header24(p)[4] for p in device24.rtp} == {device24.offer_ssrc},
              hex(device24.offer_ssrc))
        check("a sender report goes out with the first picture",
              _wait_until(lambda: len(device24.rtcp) >= 1, timeout=5)
              and len(device24.rtcp[0]) == 28
              and struct.unpack('>I', device24.rtcp[0][4:8])[0]
              == device24.offer_ssrc, str(device24.rtcp[:1]))
        _stats24 = mir24.stats()
        check("the low-latency session reports itself as caststream",
              _stats24.get('kind') == 'caststream' and _stats24['frames'] >= 1
              and _stats24['clients'] == 1, str(_stats24))

        check("twelve un-acknowledged frames is the window, then frames shed",
              _wait_until(lambda: mir24._sink.drops > 0
                          and mir24._sink.in_flight()
                          == mirror.MAX_IN_FLIGHT_FRAMES, timeout=20),
              'drops={} in_flight={}'.format(mir24._sink.drops,
                                             mir24._sink.in_flight()))
        # The encoder produces a picture every 10 ms and the window is twelve
        # deep, so a freed window refills long before a 0.1 s poll can notice.
        # Pausing the fake encoder is what turns this from a race into a claim.
        _before24 = mir24._sink.frames
        open(_pause24, 'w').close()
        device24.ack(mirror.MAX_IN_FLIGHT_FRAMES - 1)
        check("a checkpoint frees the whole window at once",
              _wait_until(lambda: mir24._sink.in_flight() == 0
                          and mir24._sink.acked
                          == mirror.MAX_IN_FLIGHT_FRAMES - 1, timeout=10),
              'in_flight={} acked={}'.format(mir24._sink.in_flight(),
                                             mir24._sink.acked))
        os.remove(_pause24)
        check("and the frames start flowing again",
              _wait_until(lambda: mir24._sink.frames > _before24, timeout=10),
              'frames {} -> {}'.format(_before24, mir24._sink.frames))
        check("and the playout delay the TV reports is kept for the menu",
              mir24.stats()['delay'] == 12, str(mir24.stats()))
        _dropped24 = mir24._sink.drops
        _keys24_seen = lambda: {
            fid for fid, (is_key, _unit) in
            _decode24(device24.rtp, device24.keys[0], device24.keys[1],
                      mirror).items() if is_key}
        _keys_before24 = _keys24_seen()
        device24.nack_picture()
        check("a picture loss stops deltas until the next redraw",
              _wait_until(lambda: mir24._sink.drops > _dropped24, timeout=10),
              'drops {} -> {}'.format(_dropped24, mir24._sink.drops))
        def _key_gets_through24():
            # This fake device only ever acknowledges when the test tells it to,
            # and a full window is allowed to hold back even a key frame -- so
            # watching for the redraw without reopening the credit would race
            # the 12-frame window rather than test the picture-loss path.
            device24.ack((mir24._sink._frame_id - 1) & 0xFF)
            return _keys24_seen() - _keys_before24 \
                and mir24._sink._awaiting_key is False
        check("the next key frame gets through regardless",
              _wait_until(_key_gets_through24, timeout=20),
              str(mir24._sink._awaiting_key))

        _line24 = mirror.ScreenMirrorSetting._status_line(mir24)
        check("the menu line counts frames and the window instead of viewers",
              'caststream' not in _line24 and '帧' in _line24
              and '在途' in _line24, _line24)
        # 'source' is a promise this channel cannot keep, and the console has to
        # say so out loud rather than quietly letterbox somebody's desktop.
        utils.Setting.set(mirror.SettingProperty.Mirror_Quality, 'source')
        _note24 = mirror.ScreenMirrorSetting._quality_note()
        check("the console admits what the low-latency channel caps",
              '4.5 Mbps' in _note24 and '1920x1080' in _note24, _note24)
        utils.Setting.set(mirror.SettingProperty.Mirror_Quality, '720')
        check("and a 720p request keeps its own size",
              '1280x720' in mirror.ScreenMirrorSetting._quality_note())

        mir24.stop_mirror()
        check("teardown leaves the app before it drops the transport",
              _wait_until(lambda: [t for (_n, t, _d) in device24.received][-3:]
                          == ['CLOSE', 'STOP', 'CLOSE'], timeout=10),
              str([t for (_n, t, _d) in device24.received][-4:]))
        check("the STOP carries the mirroring session id",
              [d for (_n, t, d) in device24.received
               if t == 'STOP'][-1].get('sessionId') == 'session-1',
              str([d for (_n, t, d) in device24.received if t == 'STOP']))
        check("a stopped session reports no state and the sink is gone",
              _wait_until(lambda: mir24._sink is None and not mir24.is_mirroring()
                          and mir24.stats() == {}, timeout=10))

        # -- why close() drains before it hangs up ---------------------------
        # A receiver answers every keepalive and every goodbye, so at teardown
        # the control socket always holds replies nobody read. Closing with a
        # non-empty receive buffer makes the kernel answer with RST instead of
        # FIN, and an RST drops whatever of ours is still in flight -- which is
        # how the final CLOSE went missing under load. A real receiver that
        # never saw it keeps the mirroring app running, and the next session
        # then pays for that with the stale-instance OFFER rejection.
        _l24 = socket.socket()
        _l24.bind(('127.0.0.1', 0))
        _l24.listen(1)
        _l24.settimeout(15)
        _sctx24 = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        _sctx24.load_cert_chain(certfile=_cert24, keyfile=_key24)
        _seen24, _reset24 = [], []

        def _peer24():
            try:
                raw, _addr = _l24.accept()
                conn = _sctx24.wrap_socket(raw, server_side=True)
            except OSError as e:
                _reset24.append('accept: %s' % e)
                return
            with conn:
                conn.settimeout(15)

                def _answer():
                    blob = cast.encode_cast_message(
                        'receiver-0', 'sender-0', mirror.NS_RECEIVER,
                        json.dumps({'type': 'RECEIVER_STATUS'}))
                    conn.sendall(struct.pack('>I', len(blob)) + blob)

                try:
                    while True:
                        head = _FakeCastDevice24._read(conn, 4)
                        if head is None:
                            return
                        body = _FakeCastDevice24._read(
                            conn, struct.unpack('>I', head)[0])
                        if body is None:
                            return
                        _seen24.append(json.loads(
                            cast.parse_cast_message(body)['payload_utf8']
                        )['type'])
                        # Every receiver answers what it was told, and nobody
                        # on our side reads those answers at teardown -- that
                        # unread pile is what turns the FIN into an RST.
                        _answer()
                except OSError as e:
                    _reset24.append(type(e).__name__)

        _peer_thread24 = threading.Thread(target=_peer24, daemon=True)
        _peer_thread24.start()
        _cctx24 = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        _cctx24.check_hostname = False
        _cctx24.verify_mode = ssl.CERT_NONE
        _ctl24 = mirror._CastSender('127.0.0.1', _l24.getsockname()[1])
        _ctl24.sock = _cctx24.wrap_socket(
            socket.create_connection(_l24.getsockname(), timeout=15))
        _ctl24.poll(0.0)                  # the mode the media loop leaves it in
        _ctl24.transport_id = 'v2-mirror'
        _ctl24.mirror_session_id = 'session-1'
        _ctl24.close_mirroring()
        _ctl24.close()
        _peer_thread24.join(20)
        _l24.close()
        check("the whole goodbye sequence reaches a receiver that has replies "
              "pending", _seen24 == ['CLOSE', 'STOP', 'CLOSE'],
              'seen={} err={}'.format(_seen24, _reset24))
        check("and it is a clean hangup, not a reset that eats the last write",
              not _reset24, str(_reset24))

        # -- the device that has no mirroring app -----------------------------
        device2b = _FakeCastDevice24(mirror, _cert24, _key24, refuse=True)
        _devices24.append(device2b)
        utils.Setting.set(mirror.SettingProperty.Mirror_Target,
                          '127.0.0.1:{}'.format(device2b.port))
        _notify24[:] = []
        mir24.start_mirror()
        check("a refused mirroring app falls back to the compatible LOAD path",
              _wait_until(lambda: bool(device2b.loaded), timeout=30),
              str([t for (_n, t, _d) in device2b.received]))
        check("the fallback launches the media receiver instead",
              [d.get('appId') for (_n, t, d) in device2b.received
               if t == 'LAUNCH'] == [mirror.MIRROR_APP_ID,
                                     mirror.DEFAULT_MEDIA_APP_ID],
              str([d for (_n, t, d) in device2b.received if t == 'LAUNCH']))
        check("and the session is a normal HTTP-served mirror again",
              mir24._sink is None and mir24._server is not None
              and mir24.playing_url().endswith('.ts'), str(mir24.playing_url()))
        check("the user is told it started, not that it failed",
              _wait_until(lambda: any('已开始镜像到' in str(n) for n in _notify24),
                          timeout=10), str(_notify24))
        check("the LOAD media type is still MPEG-TS",
              device2b.loaded[0]['media']['contentType'] == 'video/mp2t'
              and device2b.loaded[0]['media']['streamType'] == 'LIVE',
              str(device2b.loaded))
        mir24.stop_mirror()
        check("a fallback session tears down the same way",
              _wait_until(lambda: not mir24.is_mirroring(), timeout=10))

        # -- the hardware probe, wired to this same code ---------------------
        # Nothing here needs a television: the point is that the probe drives
        # the plugin under test rather than a second copy of the tables, which
        # is the only way a pass on hardware means anything for the menu bar.
        _scripts24 = os.path.join(REPO, 'scripts')
        _added24 = _scripts24 not in sys.path
        if _added24:
            sys.path.insert(0, _scripts24)
        import importlib as _il24
        probe24 = _il24.import_module('cast_streaming_probe')
        try:
            _psm = probe24.load_plugin()
            check("the probe loads the plugin under test, not its own tables",
                  _psm is not mirror
                  and _psm.MIRROR_APP_ID == mirror.MIRROR_APP_ID
                  and _psm.CAST_STREAM_MAX_BITRATE
                  == mirror.CAST_STREAM_MAX_BITRATE, str(_psm))
            _pcap, _label24 = probe24.make_capture(
                _psm, 'ffmpeg',
                types.SimpleNamespace(live=False, source=None, height=720))
            _pcmd = _psm.build_ffmpeg_command(
                'ffmpeg', _pcap, 720, _psm.CAST_STREAM_MAX_BITRATE,
                kind='caststream')
            check("the probe paces its own encoder and shares the builder",
                  _pcap.inputs[0][0] == '-re' and '-re' in _pcmd
                  and _pcmd[-3:] == ['-f', 'h264', 'pipe:1']
                  and 'testsrc2=size=1280x720' in ' '.join(_pcmd), str(_pcmd))
            probe24.instrument(_psm)
            _ev24 = _psm.parse_rtcp(_checkpoint24(3, 0x11, 0x22), 0x11)
            check("watching the feedback does not change the feedback",
                  _ev24 == mirror.parse_rtcp(_checkpoint24(3, 0x11, 0x22), 0x11)
                  and probe24.EVENT_TALLY.get('checkpoint') == 1, str(_ev24))
        finally:
            if _added24:
                sys.path.remove(_scripts24)
    except Exception as e:
        import traceback
        traceback.print_exc()
        check("the Cast Streaming session behaves end to end", False,
              "{}: {}".format(type(e).__name__, e))
    finally:
        os.environ.pop('FAKE_PAUSE', None)
        for _name, _value in _saved24.items():
            setattr(mirror, _name, _value)
        if _mir24 is not None:
            _mir24.stop_mirror()
        for _device in _devices24:
            _device.close()
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify24_rec)
        except Exception:
            pass
        utils.SETTING_DIR = _saved_dir24
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting24
        _shutil.rmtree(_tmp24, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("screen mirror v0.6 loads", False, "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 25: the local file caster
#
# macast/plugins/renderer/cast_local_file.py is the sender half of this plugin family: it serves
# the user's own files over HTTP and orders a Chromecast or a DLNA television to
# fetch them. Two things make this part worth its length.
#
#   * The decision table. Whether a file travels as it is, or has to be
#     re-encoded while it streams, *is* the product: a wrong "yes" is a black
#     screen the user blames us for, a wrong "no" is a few CPU seconds.
#   * Both faces are protocol code, so both are driven for real -- the HTTP
#     face over a socket, the Cast face against Macast's own Chromecast
#     receiver, the SOAP face against a fake TV that answers the way firmware
#     does, whose envelopes are then re-read by our own DLNA parser.
#
# The encoder is a fake ffmpeg on PATH that appends bytes to whatever file it is
# told to write, which is precisely what serving a growing file needs.
# --------------------------------------------------------------------------
print("\n=== Part 25: local file caster ===")
try:
    import http.client as _hc25
    import http.server as _httpd25
    import urllib.error as _ue25
    import urllib.parse as _up25
    import xml.etree.ElementTree as _ET25
    _saved_setting25 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir25 = utils.SETTING_DIR
    _saved_path25 = os.environ.get('PATH', '')
    _tmp25 = _tempfile.mkdtemp(prefix="macast-castfile-")
    _notify25 = []
    _notify25_rec = lambda *a, **k: _notify25.append(a)      # noqa: E731
    _receiver25 = None
    _tv25 = None
    _caster25 = None
    _caster25b = None
    _caster25g = None
    _caster25h = None
    _server25 = None
    _taps25 = {}
    _taps_back25 = {}
    try:
        utils.SETTING_DIR = _tmp25
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp25, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify25_rec)

        clf = _load_plugin("cast_local_file_plugin", "cast_local_file.py")
        # Every tap this part installs is snapshotted once, here, so the
        # restore below cannot be tricked into writing back a stub.
        for _name25 in ('probe_media', '_keep_awake', '_stop_awake',
                        'WATCHDOG_SECONDS', 'find_tool', 'free_space',
                        'system_audio_input', '_ssdp_search',
                        'describe_renderer', 'start_search',
                        'start_renderer_search', '_devices', '_dlna_devices'):
            _taps_back25[_name25] = getattr(clf, _name25)
        _taps_back25['advertisable'] = utils.Setting.__dict__.get(
            'get_advertisable_ip')
        _probe_snapshot25 = dict(clf._probe_cache)

        # -- the two tools this plugin lives by --------------------------------
        _bin25 = os.path.join(_tmp25, "bin")
        _write_fake(_bin25, "ffprobe", r"""#!/bin/sh
# One report per file name, so a test asks about a file by naming it.
mp4='{"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"%s","size":"%s"},"streams":[%s]}'
case "$*" in
  *clean.mp4*)
    printf '%s' '{"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"120.0","size":"24000000","bit_rate":"1600000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"},"disposition":{"default":1}}]}'
    ;;
  *twin.mp4*)
    printf '%s' '{"format":{"format_name":"mov","duration":"60.0","size":"12000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"},"disposition":{"default":1}},{"index":2,"codec_type":"audio","codec_name":"ac3","tags":{"language":"chi"}},{"index":3,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"chi"}},{"index":4,"codec_type":"subtitle","codec_name":"mov_text","tags":{"title":"Forced English"}}]}'
    ;;
  *silent.mp4*)
    printf '%s' '{"format":{"format_name":"mp4","duration":"30.0","size":"3000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'
    ;;
  *subs.mp4*)
    printf '%s' '{"format":{"format_name":"mp4","duration":"60.0","size":"12000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac"},{"index":2,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"chi"}}]}'
    ;;
  *movie.mkv*)
    printf '%s' '{"format":{"format_name":"matroska,webm","duration":"240.0","size":"96000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac"}]}'
    ;;
  *hevc.mp4*)
    printf '%s' '{"format":{"format_name":"mp4","duration":"240.0","size":"96000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"hevc"},{"index":1,"codec_type":"audio","codec_name":"aac"}]}'
    ;;
  *song.mp3*)
    printf '%s' '{"format":{"format_name":"mp3","duration":"180.0","size":"2160000"},"streams":[{"index":0,"codec_type":"audio","codec_name":"mp3"}]}'
    ;;
  *live.opus*)
    printf '%s' '{"format":{"format_name":"ogg","duration":"180.0","size":"2160000"},"streams":[{"index":0,"codec_type":"audio","codec_name":"opus"}]}'
    ;;
  *long.mkv*)
    printf '%s' '{"format":{"format_name":"matroska","duration":"5400.0","size":"4000000000"},"streams":[{"index":0,"codec_type":"video","codec_name":"hevc"},{"index":1,"codec_type":"audio","codec_name":"aac"}]}'
    ;;
  *broken.mp4*)
    printf '%s' 'not json at all'
    ;;
esac
exit 0
""")
        _write_fake(_bin25, "ffmpeg", r"""#!/bin/sh
case "$*" in
  *-encoders*|*-filters*)
    printf '%s\n' 'V..... libx264' 'A..... aac' 'A..... ac3' ' .. subtitles'
    if [ "$MACAST_FAKE_HW" = yes ]; then printf '%s\n' 'V..... h264_videotoolbox'; fi
    exit 0
    ;;
  *-list_devices*)
    printf '%s\n' 'AVFoundation input device list has 2 items:' \
      '[AVFoundation indev @ 0x1] AVFoundation video devices:' \
      '[AVFoundation indev @ 0x1] [0] FaceTime高清相机' \
      '[AVFoundation indev @ 0x1] [1] Capture screen 0' \
      '[AVFoundation indev @ 0x1] AVFoundation audio devices:' \
      '[AVFoundation indev @ 0x1] [0] MacBook Pro麦克风' \
      '[AVFoundation indev @ 0x1] [1] BlackHole 2ch'
    exit 0
    ;;
esac
out=''
for a in "$@"; do out="$a"; done
case "$out" in
  *.srt|*.vtt)
    printf '%s\n' 'WEBVTT' '' '00:00:01.000 --> 00:00:02.000' 'hi' > "$out"
    exit 0
    ;;
  *nope.ts)
    printf '%s\n' 'Impossible to open' >&2
    exit 1
    ;;
esac
i=0
while [ $i -lt 400 ]; do
  head -c 4096 /dev/zero | tr '\0' 'C' >> "$out"
  i=$((i+1))
  sleep 0.02
done
""")
        os.environ['PATH'] = _bin25 + os.pathsep + _saved_path25
        clf.invalidate_tool_cache()
        _fake_ffmpeg25 = clf.find_tool('ffmpeg')
        _fake_ffprobe25 = clf.find_tool('ffprobe')
        check("ffmpeg and ffprobe are found on PATH",
              _fake_ffmpeg25 and _fake_ffprobe25
              and os.path.dirname(_fake_ffmpeg25) == _bin25,
              '{} {}'.format(_fake_ffmpeg25, _fake_ffprobe25))

        _media_dir25 = os.path.join(_tmp25, "media")
        os.makedirs(_media_dir25, exist_ok=True)

        def _file25(name, blob=None, size=4096):
            path = os.path.join(_media_dir25, name)
            with open(path, 'wb') as handle:
                if blob is not None:
                    handle.write(blob)
                else:
                    handle.truncate(size)
            return path

        def _fixture25(name, size=4096):
            """A name the fake ffprobe answers about also has to be a real
            file, because the delivery path refuses what it cannot open."""
            path = os.path.join(_media_dir25, name)
            return path if os.path.isfile(path) else _file25(name, size=size)

        # --------------------------------------------------------------------
        # A. what a file contains, and what we may therefore do with it
        # --------------------------------------------------------------------
        _report25 = json.dumps({
            "format": {"format_name": "matroska,webm", "duration": "10.5",
                       "size": "1000000"},
            "streams": [{"index": 0, "codec_type": "video",
                         "codec_name": "h264"},
                        {"index": 1, "codec_type": "audio",
                         "codec_name": "vorbis", "tags": {"language": "jpn"},
                         "disposition": {"default": 1}},
                        {"index": 2, "codec_type": "data"},
                        {"index": "bad"}]}
        )
        _parsed25 = clf.parse_probe(_report25)
        check("an ffprobe report becomes tracks, container and duration",
              _parsed25.ok and _parsed25.container == 'matroska'
              and _parsed25.duration == 10.5
              and [t.index for t in _parsed25.tracks] == [0, 1]
              and _parsed25.audio[0].label() == '#1 vorbis · jpn · 默认',
              str(_parsed25.tracks))
        check("a stream with no codec_type and a stream with no index are both skipped",
              len(_parsed25.tracks) == 2, str(_parsed25.tracks))
        check("bit_rate is derived from size and duration when ffprobe has none",
              _parsed25.bitrate == int(1000000 * 8 / 10.5), str(_parsed25.bitrate))
        check("MP4 is reported as mov by ffprobe and sorted as mp4",
              clf._container('mov,mp4,m4a,3gp,3g2,mj2') == 'mp4'
              and clf._container('avi') == 'avi', '')
        check("garbage from ffprobe is not a report",
              clf.parse_probe('not json').ok is False
              and clf.parse_probe('[1,2]').ok is False
              and clf.parse_probe(None).ok is False, '')
        _clean25 = clf.probe_media(_file25('clean.mp4', size=24000000))
        check("the fake ffprobe really answers per file name",
              _clean25.ok and _clean25.container == 'mp4'
              and _clean25.duration == 120.0 and _clean25.bitrate == 1600000
              and _clean25.content_type() == 'video/mp4', str(_clean25.tracks))
        _twin25 = clf.probe_media(_file25('twin.mp4'))
        check("a second audio track is chosen by its position, not its index",
              _twin25.stream_position(2) == 1 and _twin25.stream_position(1) == 0
              and _twin25.subtitle_position(4) == 1
              and _twin25.subtitle_position(3) == 0, '')
        _probe_path25 = []
        _saved_probe25 = clf.probe_media

        def _counting_probe(path, timeout=25):
            _probe_path25.append(path)
            return _saved_probe25(path, timeout)

        clf.probe_media = _counting_probe
        check("an unprobeable file is converted, and says why",
              clf.plan(clf.probe_media(_file25('broken.mp4')), 'cast')
              == ('convert', '读不出文件信息，按转码处理'), '')

        _decide25 = lambda p, kind, **kw: clf.plan(   # noqa: E731
            clf.probe_media(_fixture25(p)), kind, **kw)
        check("h264 in MP4 with AAC goes out as it is, to either target",
              _decide25('clean.mp4', 'cast')[0] == 'copy'
              and _decide25('clean.mp4', 'dlna')[0] == 'copy', '')
        check("one AC-3 track converts the whole file",
              _decide25('twin.mp4', 'cast')[0] == 'convert'
              and '声音编码 aac/ac3' in _decide25('twin.mp4', 'cast')[1],
              _decide25('twin.mp4', 'cast')[1])
        check("Matroska is fine for a Chromecast and not for a television",
              _decide25('movie.mkv', 'cast')[0] == 'copy'
              and '容器 matroska' in _decide25('movie.mkv', 'dlna')[1], '')
        check("a codec no TV is trusted with converts for both",
              '画面编码 hevc' in _decide25('hevc.mp4', 'cast')[1]
              and _decide25('hevc.mp4', 'dlna')[0] == 'convert', '')
        check("a silent video is not converted for want of an audio track",
              _decide25('silent.mp4', 'cast')[0] == 'copy'
              and _decide25('silent.mp4', 'dlna')[0] == 'copy', '')
        check("music rides the tolerant audio path",
              _decide25('song.mp3', 'cast')[0] == 'copy'
              and _decide25('song.mp3', 'dlna')[0] == 'copy', '')
        check("and that path still has a floor: Opus on a DLNA TV converts",
              _decide25('live.opus', 'cast')[0] == 'copy'
              and _decide25('live.opus', 'dlna')[0] == 'convert'
              and clf.probe_media(_fixture25('live.opus')).upnp_class
              == 'object.item.audioItem.musicTrack', '')
        check("picking a different audio track means re-muxing",
              '选音轨' in _decide25('twin.mp4', 'cast', audio_index=2)[1], '')
        check("a subtitle is a sidecar on Chromecast, so it never costs a transcode",
              _decide25('twin.mp4', 'cast', subtitle_index=3)
              == _decide25('twin.mp4', 'cast')
              and '字幕' not in _decide25('twin.mp4', 'cast')[1],
              _decide25('twin.mp4', 'cast', subtitle_index=3)[1])
        check("…but it is burned into the picture on a DLNA TV",
              '字幕烧录' in _decide25('twin.mp4', 'dlna',
                                     subtitle_index=3, libass=False)[1]
              and _decide25('clean.mp4', 'dlna', audio_index=1,
                            mode='direct')[0] == 'copy', '')
        check("an A/V offset needs the muxer, so it needs a transcode",
              '音画同步' in _decide25('clean.mp4', 'cast', delay_ms=250)[1], '')
        check("the two manual switches beat the table",
              _decide25('clean.mp4', 'cast', mode='convert')[1] == '手动选择强制转码'
              and _decide25('hevc.mp4', 'cast', mode='direct')[0] == 'copy', '')

        _br25, _sz25 = clf.bitrate_for_convert(_clean25, 6000000)
        check("a two-hour file gets a length the device can hold in 31 bits",
              _br25 == 6000000 and _sz25 == int((6000000 + 192000) * 120 / 8),
              '{} {}'.format(_br25, _sz25))
        _long25 = clf.probe_media(_fixture25('long.mkv', size=24000000))
        _br25b, _sz25b = clf.bitrate_for_convert(_long25, 16000000)
        check("…so a 90-minute file asks for a lower bitrate instead",
              _sz25b < clf.MAX_ADVERTISED_SIZE < 2 ** 31 and _br25b < 16000000
              and _br25b > 600000, '{} {}'.format(_br25b, _sz25b))
        _br25c, _sz25c = clf.bitrate_for_convert(clf.Media(), 4000000)
        check("a file with no duration is still budgeted, not left open-ended",
              clf.MAX_ADVERTISED_SIZE * 0.8 < _sz25c < clf.MAX_ADVERTISED_SIZE
              and 600000 < _br25c < 4000000, '{} {}'.format(_br25c, _sz25c))

        # --------------------------------------------------------------------
        # B. the transcode command
        # --------------------------------------------------------------------
        _base25 = {'audio_index': None, 'subtitle_index': None, 'delay_ms': 0,
                   'bitrate': 4000000, 'audio_bitrate': 192000, 'height': 0,
                   'encoder': 'software', 'hardware_name': 'h264_videotoolbox'}

        def _args25(path, media=None, **over):
            options = dict(_base25)
            options.update(over)
            return clf.convert_args(path, media if media is not None
                                    else _clean25, options)

        _copy25 = _args25(_media_dir25 + '/clean.mp4')

        def _maps25(args):
            return [args[i + 1] for i, a in enumerate(args) if a == '-map']

        check("the transcode is MPEG-TS on stdout with the rate control asked for",
              _copy25[-2:] == ['-f', 'mpegts']
              and _copy25[_copy25.index('-b:v') + 1] == '4000000'
              and _copy25[_copy25.index('-maxrate') + 1] == '4000000'
              and _copy25[_copy25.index('-bufsize') + 1] == '8000000', '')
        check("audio is pinned to 48 kHz stereo AAC",
              _copy25[_copy25.index('-c:a') + 1] == 'aac'
              and _copy25[_copy25.index('-ar') + 1] == '48000'
              and _copy25[_copy25.index('-ac') + 1] == '2', '')
        _audio_map25 = _args25('/x/twin.mp4', _twin25, audio_index=2)
        check("a chosen track is mapped by its position among the audio streams",
              _maps25(_audio_map25) == ['0:v:0', '0:a:1'],
              str(_maps25(_audio_map25)))
        _offset25 = _args25('/x/twin.mp4', _twin25, audio_index=2,
                            delay_ms=-500)
        check("…and an A/V offset is a second input, with audio taken from it",
              _offset25[_offset25.index('-itsoffset') + 1] == '-0.500'
              and _maps25(_offset25) == ['0:v:0', '1:a:1'],
              str(_maps25(_offset25)))
        check("a height cap scales by height only, keeping the aspect",
              _args25('/x/clean.mp4', height=720)[
              _args25('/x/clean.mp4', height=720).index('-vf') + 1]
              == 'scale=-2:720', '')
        _silent25 = clf.probe_media(_fixture25('silent.mp4'))
        _song25 = clf.probe_media(_fixture25('song.mp3'))
        check("a silent video loses the audio half of the command, not the picture",
              '-vn' not in _args25('/x/silent.mp4', _silent25)
              and '-c:a' not in _args25('/x/silent.mp4', _silent25)
              and _maps25(_args25('/x/silent.mp4', _silent25)) == ['0:v:0'],
              str(_args25('/x/silent.mp4', _silent25)))
        check("and music loses the picture half",
              '-vn' in _args25('/x/song.mp3', _song25)
              and '-b:v' not in _args25('/x/song.mp3', _song25)
              and _maps25(_args25('/x/song.mp3', _song25)) == ['0:a:0'],
              str(_args25('/x/song.mp3', _song25)))
        # `options()` is where the build is asked what it has, so the menu can
        # tick 硬件编码器 all it likes: without VideoToolbox the command falls
        # back on libx264 rather than failing to start.
        utils.Setting.set(clf.SettingProperty.Hardware, True)
        _nohw25 = _args25('/x/clean.mp4', **clf.options())
        os.environ['MACAST_FAKE_HW'] = 'yes'
        clf.invalidate_tool_cache()
        _hw25 = _args25('/x/clean.mp4', **clf.options())
        os.environ.pop('MACAST_FAKE_HW', None)
        clf.invalidate_tool_cache()
        utils.Setting.unset(clf.SettingProperty.Hardware)
        check("the hardware encoder is only named when this build has it",
              clf.ffmpeg_has(_fake_ffmpeg25, 'h264_videotoolbox') is False
              and 'h264_videotoolbox' not in _nohw25
              and 'libx264' in _nohw25, str(_nohw25[4:8]))
        check("…and it is used the moment the build really has it",
              _hw25[_hw25.index('-c:v') + 1] == 'h264_videotoolbox'
              and 'libx264' not in _hw25, str(_hw25[4:8]))
        check("a subtitle path inside a filter survives ffmpeg's two parsers",
              clf.escape_filter_path("/tmp/a:b's.srt")
              == "/tmp/a\\:b\\'s.srt"
              and 'subtitles=/tmp/a\\:b'
              in ' '.join(_args25('/x/clean.mp4',
                                  burn_subtitle=True,
                                  subtitle_path='/tmp/a:b.srt')), '')

        # --------------------------------------------------------------------
        # C. the HTTP face: Range, 206, and a file that is still growing
        # --------------------------------------------------------------------
        _blob25 = bytes(bytearray([i % 251 for i in range(10240)]))
        _real25 = _file25('range.mp4', blob=_blob25)
        _store25 = clf.Store()
        _server25 = clf.start_media_server(_store25)
        _plain25 = _store25.add(_real25, 'video/mp4', 'mp4', title='range')
        _dlna25 = _store25.add(_real25, 'video/mp4', 'mp4', title='dlna',
                               dlna=True)
        _torn25 = _store25.add(os.path.join(_tmp25, 'gone.mp4'), 'video/mp4',
                               'mp4', title='gone')

        def _http25(method, entry_or_path, range_header=None, head=False,
                    port=None):
            path = (entry_or_path if isinstance(entry_or_path, str)
                    else clf.MEDIA_PREFIX + entry_or_path.filename)
            conn = _hc25.HTTPConnection('127.0.0.1', _server25.server_address[1]
                                         if port is None else port, timeout=10)
            conn.request(method, path,
                         headers={} if range_header is None
                         else {'Range': range_header})
            resp = conn.getresponse()
            out = (resp.status, dict(resp.getheaders()), resp.read())
            conn.close()
            return out

        _st25, _hd25, _bd25 = _http25('GET', _plain25)
        check("a whole-file read is a 200 with the exact bytes and no Content-Range",
              _st25 == 200 and _bd25 == _blob25
              and _hd25['Content-Length'] == str(len(_blob25))
              and 'Content-Range' not in _hd25, str(_hd25))
        check("every answer says no-cache and nosniff",
              _hd25['Cache-Control'] == 'no-store'
              and _hd25['X-Content-Type-Options'] == 'nosniff', '')
        _st25, _hd25, _bd25 = _http25('HEAD', _plain25)
        check("HEAD promises ranges and carries no body",
              _st25 == 200 and _hd25['Accept-Ranges'] == 'bytes'
              and _bd25 == b'' and _hd25['Content-Type'] == 'video/mp4', '')
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=100-199')
        check("a closed range is answered to the byte, which is what a seek bar needs",
              _st25 == 206 and _bd25 == _blob25[100:200]
              and _hd25['Content-Range'] == 'bytes 100-199/10240'
              and _hd25['Content-Length'] == '100', str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=10000-')
        check("an open-ended range runs to the end of the file",
              _st25 == 206 and _bd25 == _blob25[10000:]
              and _hd25['Content-Range'] == 'bytes 10000-10239/10240', '')
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=-512')
        check("a suffix range means the last 512 bytes",
              _st25 == 206 and _bd25 == _blob25[-512:]
              and _hd25['Content-Range'] == 'bytes 9728-10239/10240', '')
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=200-100')
        check("a range that ends before it starts is ignored, not answered 416",
              _st25 == 200 and _bd25 == _blob25 and 'Content-Range' not in _hd25,
              str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=abc-')
        check("so is one that is not a range at all: RFC 7233 says ignore it",
              _st25 == 200 and _bd25 == _blob25 and 'Content-Range' not in _hd25,
              str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=0-9,20-29')
        check("a multi-range request is served as one whole file, not half",
              _st25 == 200 and _bd25 == _blob25, str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', _plain25, 'bytes=10240-10999')
        check("an offset past the end is the only 416, and it names the size",
              _st25 == 416 and _hd25['Content-Range'] == 'bytes */10240'
              and _bd25 == b'', str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', '/elsewhere/file.mp4')
        check("nothing outside the one random-named prefix is readable",
              _st25 == 404 and _http25('GET', '/media/deadbeefdeadbeef.mp4')[0]
              == 404, str(_st25))
        _st25, _hd25, _bd25 = _http25('GET', _dlna25)
        check("a renderer gets the two DLNA headers, a browser-side entry does not",
              _hd25['transferMode.dlna.org'] == 'Streaming'
              and 'DLNA.ORG_OP=01' in _hd25['contentFeatures.dlna.org']
              and 'transferMode.dlna.org' not in
              dict(_http25('GET', _plain25)[1]), str(_hd25))
        _st25, _hd25, _bd25 = _http25('GET', _torn25)
        check("a file that vanished while registered answers 404-worth of nothing",
              _bd25 == b'' and _st25 == 404
              and _http25('HEAD', _torn25)[0] == 404, str(_hd25))

        _growing25 = clf.Job(_fake_ffmpeg25, ['-i', 'x'], 'ts', 'video/mp2t',
                             total=65536, label='growing')
        check("the fake encoder starts and begins writing its output file",
              _growing25.start()
              and _wait_until(lambda: _growing25.size_now() > 8192, timeout=10),
              str(_growing25.size_now()))
        _job_entry25 = _store25.add(_growing25.path, 'video/mp2t', 'ts',
                                     job=_growing25, title='growing')
        check("a growing file advertises the length the encoder will reach",
              _job_entry25.size() == 65536, str(_job_entry25.size()))
        _st25, _hd25, _bd25 = _http25('GET', _job_entry25, 'bytes=0-4095')
        check("the head of a transcode is served as soon as it exists",
              _st25 == 206 and _bd25 == b'C' * 4096, repr(_bd25[:16]))
        _st25, _hd25, _bd25 = _http25('GET', _job_entry25, 'bytes=0-4095')
        check("and the same range twice gives the same bytes, so a retry is safe",
              _bd25 == _blob25[:0] + b'C' * 4096, repr(_bd25[:8]))
        _ahead25 = []
        _slow_thread25 = threading.Thread(
            target=lambda: _ahead25.append(_http25('GET', _job_entry25,
                                                   'bytes=8192-16383')),
            daemon=True)
        _slow_thread25.start()
        check("a reader ahead of the encoder waits for it instead of lying",
              _wait_until(lambda: bool(_ahead25), timeout=20)
              and _ahead25[0][2] == b'C' * 8192, str(_ahead25 and len(_ahead25[0][2])))
        _growing25.stop()
        _t025 = time.time()
        _st25, _hd25, _bd25 = _http25('GET', _job_entry25, 'bytes=400000-')
        check("a dead encoder gets an honest short answer, not a hang",
              _bd25 == b'' and time.time() - _t025 < 6.0,
              '{} {:d}s'.format(_st25, int(time.time() - _t025)))
        _growing25.cleanup()
        _store25.drop(_job_entry25)
        _live25 = clf.Job(_fake_ffmpeg25, ['-i', 'x'], 'ts', 'video/mp2t',
                          live=True, label='live')
        _live25.start()
        _live_entry25 = _store25.add(_live25.path, 'video/mp2t', 'ts',
                                      job=_live25, title='live')
        _conn25 = _hc25.HTTPConnection('127.0.0.1', _server25.server_address[1],
                                       timeout=20)
        _conn25.request('GET', clf.MEDIA_PREFIX + _live_entry25.filename)
        _resp25 = _conn25.getresponse()
        _first25 = _resp25.read(4096)
        _second25 = _resp25.read(4096)
        _conn25.close()
        check("a live stream is a lengthless 200 that keeps producing",
              _resp25.status == 200 and 'Content-Length' not in dict(
                  _resp25.getheaders())
              and dict(_resp25.getheaders())['Accept-Ranges'] == 'none'
              and _first25 == b'C' * 4096 and _second25 == b'C' * 4096,
              repr(_first25[:8]))
        _live25.stop()
        _live25.cleanup()
        _store25.drop(_live_entry25)
        check("Range parsing itself is the forgiving kind",
              clf.parse_range('bytes=100-200') == (100, 200)
              and clf.parse_range('bytes=100-') == (100, None)
              and clf.parse_range('bytes=-1000') == (None, 1000)
              and clf.parse_range('bytes=abc-') is None
              and clf.parse_range('items=0-9') is None
              and clf.parse_range('bytes=0-1,4-5') is None
              and clf.parse_range(None) == (0, None), '')

        # --------------------------------------------------------------------
        # D. the Cast face, driven against Macast's own Chromecast receiver
        # --------------------------------------------------------------------
        _saved_keep25 = clf._keep_awake
        _saved_awake25 = clf._stop_awake
        _saved_watch25 = clf.WATCHDOG_SECONDS
        clf.WATCHDOG_SECONDS = 0.2

        def _hold_awake25(platform=None):
            _taps25['awake'] = _taps25.get('awake', 0) + 1
            return 'assertion'

        def _release_awake25(handle):
            # The real one ignores a None handle, so this must too or the two
            # counters stop meaning "one assertion held, one assertion released".
            if handle is not None:
                _taps25['asleep'] = _taps25.get('asleep', 0) + 1

        clf._keep_awake = _hold_awake25
        clf._stop_awake = _release_awake25

        class _Caster25(clf.LocalFileRenderer):
            """The renderer, with its state reports caught rather than sent."""

            def __init__(self):
                super(_Caster25, self).__init__()
                self.recorder = _StateRec()

            @property
            def protocol(self):
                return self.recorder

        _caster25 = _Caster25()
        _rec25 = _caster25.recorder
        _CTX.renderer = MockRenderer()
        _CTX.renderer.transport_state = 'PLAYING'
        _receiver25 = TestProtocol()
        _receiver25.start()
        utils.Setting.set(clf.SettingProperty.Target_Kind, 'cast')
        utils.Setting.set(clf.SettingProperty.Cast_Target,
                          '127.0.0.1:{}'.format(_receiver25.cast_port))
        utils.Setting.set(clf.SettingProperty.Cast_Target_Name, '测试用的电视')
        utils.Setting.set(clf.SettingProperty.Mode, 'auto')

        def _media_url25():
            return str(_CTX.renderer.last_arg('set_media_url') or '')

        _caster25.play_path(_media_dir25 + '/clean.mp4')
        check("a local file reaches the receiver as a url it can fetch",
              _wait_until(lambda: _CTX.renderer.called('set_media_url'),
                          timeout=25)
              and _media_url25().startswith('http://127.0.0.1:')
              and clf.MEDIA_PREFIX in _media_url25()
              and _media_url25().endswith('.mp4'), str(_CTX.renderer.calls))

        def _split_url25(url):
            return (int(url.split('/')[2].rsplit(':', 1)[1]),
                    '/' + url.split('/', 3)[3])

        _st25, _hd25, _bd25 = _http25('GET', _split_url25(_media_url25())[1],
                                       'bytes=0-15',
                                       port=_split_url25(_media_url25())[0])
        check("the receiver range-reads the file it was pointed at",
              _st25 == 206 and _hd25['Content-Range'] == 'bytes 0-15/24000000'
              and len(_bd25) == 16, str(_hd25))
        check("an h264/AAC mp4 travels untouched, and says so in one line",
              _caster25.mode == 'copy' and _caster25.duration == 120.0
              and 'clean.mp4 · 直通' in _caster25.status(), _caster25.status())
        check("the receiver agrees its app is open and playing our item",
              _wait_until(lambda: _caster25.output is not None
                          and _caster25.output.media_status()[0] == 'PLAYING',
                          timeout=20)
              and _caster25.output.applications() == [clf.DEFAULT_MEDIA_APP_ID],
              str(_caster25.output.media_status() if _caster25.output else None))
        _CTX.renderer.calls = []
        _caster25.set_media_pause()
        check("pause lands on the device and on the state page",
              _wait_until(lambda: _CTX.renderer.called('set_media_pause'),
                          timeout=15) and _caster25.state == 'PAUSED'
              and ('transport', 'PAUSED_PLAYBACK') in _rec25.rows,
              str(_CTX.renderer.calls))
        _caster25.set_media_resume()
        check("so does resume",
              _wait_until(lambda: _CTX.renderer.called('set_media_resume'),
                          timeout=15) and _caster25.state == 'PLAYING',
              str(_CTX.renderer.calls))
        _caster25.seek_to(30)
        check("a seek is a SEEK carrying the absolute time",
              _wait_until(lambda: _CTX.renderer.last_arg('set_media_position')
                          == '30.0', timeout=15) and _caster25.position == 30.0,
              str(_CTX.renderer.calls))
        _caster25.set_media_volume(40)
        check("volume is asked of the receiver, in percent",
              _wait_until(lambda: _CTX.renderer.last_arg('set_media_volume') == 40,
                          timeout=15), str(_CTX.renderer.calls))
        _out25 = _caster25.output
        check("QUIT_APP is answered, so a real stop really ends the session",
              _out25._receiver_command({'type': 'QUIT_APP',
                                        'appId': _out25.app_id}) is True, '')
        check("…and the receiver stops claiming a media it no longer holds",
              _wait_until(lambda: _out25.media_status()[0] == 'IDLE', timeout=15)
              and _out25.applications() == [], str(_out25.media_status()))

        utils.Setting.set(clf.SettingProperty.Mode, 'convert')
        _CTX.renderer.calls = []
        _caster25.play_path(_media_dir25 + '/hevc.mp4')
        check("a forced transcode goes out as a live mpeg-ts url",
              _wait_until(lambda: _CTX.renderer.called('set_media_url'),
                          timeout=25)
              and _media_url25().endswith('.ts') and _caster25.mode == 'convert',
              _caster25.status())
        _conv_port25, _conv_path25 = _split_url25(_media_url25())
        _conn25c = _hc25.HTTPConnection('127.0.0.1', _conv_port25, timeout=20)
        _conn25c.request('GET', _conv_path25,
                         headers={'Range': 'bytes=0-4095'})
        _resp25c = _conn25c.getresponse()
        _head25c = dict(_resp25c.getheaders())
        _first25c = _resp25c.read(4096)
        _conn25c.close()
        check("a live transcode promises no length and ignores the range",
              _resp25c.status == 200 and 'Content-Length' not in _head25c
              and _head25c['Accept-Ranges'] == 'none'
              and _head25c['Content-Type'] == 'video/mp2t'
              and _first25c == b'C' * 4096, str(_head25c))
        _conv_job25 = _caster25.job
        _conv_dir25 = _conv_job25.directory
        _caster25.set_media_stop()
        check("stopping takes the encoder down and its file out with it",
              _conv_job25.proc is None and not os.path.isdir(_conv_dir25)
              and _caster25.store.entries == {} and _caster25.output is None,
              _conv_dir25)
        check("one sleep assertion per session, released by its stop",
              _taps25.get('awake') == 2 and _taps25.get('asleep') == 2,
              str(_taps25))

        utils.Setting.set(clf.SettingProperty.Mode, 'auto')
        utils.Setting.set(clf.SettingProperty.Subtitle_Stream, '2')
        _CTX.renderer.calls = []
        _twin_path25 = _media_dir25 + '/twin.mp4'
        # twin.mp4 is the multi-track file but its second audio is AC-3, which
        # converts on its own; subs.mp4 is the same shape with all-copyable
        # audio, so a subtitle choice is the only thing moving here.
        _subs_path25 = _fixture25('subs.mp4')
        _caster25.play_path(_subs_path25)
        check("a chosen subtitle rides along as a sidecar, still no re-encode",
              _wait_until(lambda: _CTX.renderer.called('set_media_url'),
                          timeout=25) and _caster25.mode == 'copy'
              and len(_caster25.store.entries) == 2, _caster25.status())
        _led25 = dict(_receiver25._media or {})
        _vtt25 = str(((_led25.get('textTracks') or [{}])[0]).get('contentId'))
        check("the LOAD declares the track and points it at our own server",
              _vtt25.endswith('.vtt') and clf.MEDIA_PREFIX in _vtt25,
              str(_led25))
        _sidecar25 = _caster25.entries[-1].path
        _st25, _hd25, _bd25 = _http25('GET', _split_url25(_vtt25)[1],
                                       port=_split_url25(_vtt25)[0])
        check("the sidecar is served as WebVTT, the one kind a Chromecast reads",
              _st25 == 200 and _bd25.startswith(b'WEBVTT')
              and _hd25['Content-Type'] == 'text/vtt', str(_hd25))
        _caster25.set_media_stop()
        check("our sidecar is deleted afterwards; the user's file never is",
              not os.path.exists(_sidecar25) and os.path.isfile(_subs_path25),
              _sidecar25)
        utils.Setting.unset(clf.SettingProperty.Subtitle_Stream)

        # --------------------------------------------------------------------
        # E. what may be asked for, and what is refused
        # --------------------------------------------------------------------
        _caster25.clear_queue()
        _picked25 = [_media_dir25 + '/clean.mp4', _twin_path25]
        check("the queue takes real files once and drops the rest",
              _caster25.enqueue(_picked25 + _picked25
                                + [_media_dir25 + '/missing.mp4']) == 2
              and _caster25.queue == _picked25, str(_caster25.queue))
        check("an index off either end plays nothing and starts no delivery",
              _caster25.play_index(99) is False
              and _caster25.play_index(-1) is False and _caster25.index == -1,
              str(_caster25.index))
        _played25 = []

        def _listing_play25(index, start='0'):
            _played25.append((index, start))
            # The real one stores it, and the two direction checks below read it
            # back, so the stand-in has to keep the same promise.
            _caster25.index = index
            return True

        _caster25.play_index = _listing_play25
        _notify25.clear()
        _caster25.index = 0
        check("walking off the front says so instead of going silent",
              _caster25.play_previous() is False
              and _wait_until(lambda: any('已经是列表第一个' in str(n)
                                          for n in _notify25), timeout=5)
              and _played25 == [], str(_notify25))
        _notify25.clear()
        _caster25.index = 1
        check("…and so does walking off the back",
              _caster25.play_next() is False
              and _wait_until(lambda: any('已经是列表最后一个' in str(n)
                                          for n in _notify25), timeout=5),
              str(_notify25))
        _played25 = []
        _caster25.index = 0
        check("from the middle of the list both directions move",
              _caster25.play_next() is True and _played25 == [(1, '0')]
              and _caster25.play_previous() is True
              and _played25 == [(1, '0'), (0, '0')], str(_played25))
        del _caster25.play_index
        _notify25.clear()
        check("an empty push is ignored rather than announced",
              _caster25.set_media_url('') is None and _notify25 == []
              and _caster25.queue == [_media_dir25 + '/clean.mp4',
                                      _twin_path25],
              str(_caster25.queue))
        _notify25.clear()
        _caster25.clear_queue()
        _caster25.set_media_url('/nope/nothing.mp4')
        check("a path that does not exist is refused with the rule quoted",
              _wait_until(lambda: any('只认本机文件路径' in str(n)
                                      for n in _notify25), timeout=5)
              and _caster25.queue == [], str(_notify25))
        _caster25.set_media_url('file://' + _up25.quote(_twin_path25))
        check("a file:// url is decoded into the same entry as the plain path",
              _caster25.queue == [_twin_path25] and _caster25.index == 0,
              str(_caster25.queue))
        _CTX.renderer.calls = []
        _caster25.set_media_stop()
        _CTX.renderer.calls = []
        _caster25.set_media_url('http://remote.example/movie.mp4')
        check("a remote url is handed to the device untouched, not proxied",
              _wait_until(lambda: _CTX.renderer.last_arg('set_media_url')
                          == 'http://remote.example/movie.mp4', timeout=25)
              and _caster25.mode == 'remote'
              and _caster25.store.entries == {}, _caster25.status())
        _caster25.set_media_stop()
        utils.Setting.set(clf.SettingProperty.System_Audio, True)
        _notify25.clear()
        _CTX.renderer.calls = []
        _caster25.set_media_url(_twin_path25)
        check("a pushed file is refused while system sound is on the air",
              _wait_until(lambda: any('只投系统声音' in str(n)
                                      for n in _notify25), timeout=5)
              and not _CTX.renderer.called('set_media_url'), str(_notify25))
        utils.Setting.set(clf.SettingProperty.System_Audio, False)
        _saved_input25 = clf.system_audio_input
        clf.system_audio_input = lambda: None
        _notify25.clear()
        check("audio-only mode refuses when this machine has no loopback device",
              _caster25.set_system_audio(True) is False
              and any('BlackHole' in str(n) for n in _notify25)
              and clf.system_audio_wanted() is False, str(_notify25))
        clf.system_audio_input = lambda: ('BlackHole 2ch',
                                          ['-f', 'avfoundation', '-i', ':0'])
        utils.Setting.set(clf.SettingProperty.Target_Kind, 'dlna')
        utils.Setting.set(clf.SettingProperty.Dlna_Control,
                          'http://127.0.0.1:9/AVTransport')
        _notify25.clear()
        check("…and only on a Chromecast, because a DLNA TV takes no live audio",
              _caster25.set_system_audio(True) is False
              and any('只支持 Chromecast' in str(n) for n in _notify25)
              and clf.system_audio_wanted() is False, str(_notify25))
        utils.Setting.set(clf.SettingProperty.Target_Kind, 'cast')
        _CTX.renderer.calls = []
        check("with a device to speak of, audio-only mode starts streaming",
              _caster25.set_system_audio(True) is True
              and _wait_until(lambda: _CTX.renderer.called('set_media_url'),
                              timeout=25)
              and _caster25.mode == 'audio'
              and '系统声音' in _caster25.status(), _caster25.status())
        _caster25.set_media_stop()
        clf.system_audio_input = _saved_input25
        check("seconds are clock times here, wherever they come from",
              clf.hms(3725) == '1:02:05' and clf.hms(0) == '0:00:00'
              and clf.parse_time('1:02:03.5') == 3723.5
              and clf.parse_time('90') == 90.0
              and clf.parse_time('nonsense') == 0.0, '')
        check("a start is mpv's grammar, because the DLNA side sends that",
              clf.parse_start('0:01:30') == 90.0
              and clf.parse_start('50%', 200) == 100.0
              and clf.parse_start('-30') == -30.0
              and clf.parse_start('') == 0.0, '')
        check("content types are guessed by extension and the name follows",
              clf.guess_content('/x/a.mkv') == 'video/x-matroska'
              and clf.guess_content('/x/a.bin') == 'video/mp4'
              and clf.suffix_for('video/mp2t') == 'ts'
              and clf.suffix_for('application/octet-stream', '/x/y.avi') == 'avi',
              '')
        # Verbatim shape from `ffmpeg -f avfoundation -list_devices true -i ""`
        # on the machine running these tests -- the log prefix is part of it.
        _devices25 = clf._avfoundation_audio_devices(
            '[AVFoundation indev @ 0x7f] AVFoundation video devices:\n'
            '[AVFoundation indev @ 0x7f] [0] OBS Virtual Camera\n'
            '[AVFoundation indev @ 0x7f] [1] Capture screen 0\n'
            '[AVFoundation indev @ 0x7f] AVFoundation audio devices:\n'
            '[AVFoundation indev @ 0x7f] [0] MacBook Pro麦克风\n'
            '[AVFoundation indev @ 0x7f] [1] "BlackHole 2ch"\n'
            '[in#0 @ 0x7f] Error opening input: Input/output error\n')
        check("avfoundation's audio block is read without its video block",
              _devices25 == [('0', 'MacBook Pro麦克风'),
                             ('1', 'BlackHole 2ch')]
              and clf._avfoundation_audio_devices(
                  'List of Audio devices:\n  0) BlackHole 2ch\n')
              == [('0', 'BlackHole 2ch')]
              and clf._avfoundation_audio_devices('nothing at all') == [],
              str(_devices25))
        _listing25 = clf._ask(_fake_ffmpeg25, ['-hide_banner', '-f',
                                               'avfoundation', '-list_devices',
                                               'true', '-i', ''])
        check("…and the fake ffmpeg on PATH says what the real one says",
              clf._avfoundation_audio_devices(_listing25)
              == [('0', 'MacBook Pro麦克风'), ('1', 'BlackHole 2ch')],
              _listing25)
        _file25('notes.txt', size=16)
        _names25, _total25 = clf.list_media(_media_dir25)
        check("the folder listing takes media files only, and is capped for the menu",
              'notes.txt' not in _names25 and 'clean.mp4' in _names25
              and _total25 == len(_names25)
              and clf.list_media(_media_dir25, limit=3)[0] == _names25[:3]
              and clf.list_media(_media_dir25, limit=3)[1] == _total25
              and clf.list_media(_tmp25 + '/nowhere')[0] == [], str(_names25))
        check("and a file whose name is not decodable stays off the menu",
              clf.list_media('') == ([], 0) and clf.list_media(None) == ([], 0), '')

        # --------------------------------------------------------------------
        # F. the DLNA face: a fake TV that answers the way firmware does
        # --------------------------------------------------------------------
        class _TV25Handler(_httpd25.BaseHTTPRequestHandler):
            def log_message(self, fmt, *args):
                pass

            def _reply(self, action, fields):
                inner = ''.join('<{k}>{v}</{k}>'.format(k=k, v=v)
                                for k, v in [('InstanceID', '3')] + list(fields))
                body = (
                    '<?xml version="1.0"?>'
                    '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"'
                    ' s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
                    '<s:Body><u:{a}Response '
                    'xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">'
                    '{i}</u:{a}Response></s:Body></s:Envelope>'
                ).format(a=action, i=inner)
                raw = body.encode('utf-8')
                self.send_response(200)
                self.send_header('Content-Type', 'text/xml; charset="utf-8"')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_POST(self):
                n = int(self.headers.get('Content-Length') or 0)
                raw = self.rfile.read(n).decode('utf-8')
                action = (self.headers.get('SOAPAction') or '').strip('"')
                action = action.rsplit('#', 1)[-1]
                _taps25.setdefault('calls', []).append(
                    (action, raw, self.headers.get('Content-Length')))
                state = _taps25.setdefault('state', 'NO_MEDIA_PRESENT')
                if action == 'SetAVTransportURI':
                    _taps25['uri'] = raw
                    _taps25['state'] = 'STOPPED'
                    self._reply(action, [])
                elif action in ('Play', 'Pause', 'Seek', 'Stop'):
                    if action == 'Play':
                        _taps25['state'] = 'PLAYING'
                    elif action == 'Pause':
                        _taps25['state'] = 'PAUSED_PLAYBACK'
                    self._reply(action, [])
                elif action == 'GetTransportInfo':
                    if _taps25.get('status'):
                        self.send_error(_taps25['status'])
                        return
                    self._reply(action, [
                        ('CurrentTransportState',
                         _taps25.get('reported', state)),
                        ('CurrentTransportStatus', 'OK'),
                        ('CurrentSpeed', '1')])
                elif action == 'GetPositionInfo':
                    self._reply(action, [('RelTime', _taps25.get('rel',
                                                                 '0:00:42')),
                                         ('TrackDuration',
                                          _taps25.get('dur', '0:02:30'))])
                else:
                    self.send_error(501)

        _tv25 = _httpd25.ThreadingHTTPServer(('127.0.0.1', 0), _TV25Handler)
        threading.Thread(target=_tv25.serve_forever, daemon=True,
                         name="FAKE_DLNA_TV").start()
        _control25 = 'http://127.0.0.1:{}/control'.format(
            _tv25.server_address[1])

        def _verbs25():
            return [a for a, _b, _c in _taps25['calls']
                    if a in ('SetAVTransportURI', 'Play', 'Seek', 'Pause',
                             'Stop')]

        _taps25['calls'] = []
        _taps25['state'] = 'NO_MEDIA_PRESENT'
        _taps25['reported'] = 'PLAYING'
        _sender25 = clf.DlnaSender(_control25, timeout=5.0)
        _sender25.set_uri('http://192.0.2.9:8/media/x.mp4', clf.build_didl(
            'http://192.0.2.9:8/media/x.mp4', '深夜食堂', 'video/mp4', 120.0,
            24000000))
        _sender25.play()
        _sender25.seek(42)
        _sender25.pause()
        check("one DLNA hand-off is URI, play, the resume seek, then pause",
              _verbs25() == ['SetAVTransportURI', 'Play', 'Seek', 'Pause'],
              str(_verbs25()))
        check("a renderer that moved off InstanceID 0 is answered in kind",
              _sender25.instance_id == '3', _sender25.instance_id)
        check("the seek names the unit and carries the absolute time",
              '<Unit>REL_TIME</Unit><Target>0:00:42</Target>' in
              [b for a, b, _c in _taps25['calls'] if a == 'Seek'][0], '')
        check("the transport state we read is the one the TV reported",
              _sender25.transport_state() == 'PLAYING'
              and _sender25.last_state == 'PLAYING', _sender25.last_state)
        check("a position is (reltime, trackduration) in seconds",
              _sender25.position() == (42.0, 150.0), str(_sender25.position()))
        _taps25['status'] = 500
        try:
            _sender25.transport_state()
            _raised25 = None
        except Exception as e:
            _raised25 = e
        del _taps25['status']
        check("a TV that answers 500 raises, and the last good state survives",
              isinstance(_raised25, _ue25.HTTPError)
              and _sender25.last_state == 'PLAYING', repr(_raised25))
        _uri25 = [c for c in _taps25['calls'] if c[0] == 'SetAVTransportURI'][0]
        check("Content-Length counts UTF-8 bytes, so a Chinese title is not cut off",
              int(_uri25[2]) == len(_uri25[1].encode('utf-8'))
              and int(_uri25[2]) > len(_uri25[1]),
              '{} vs {} characters'.format(_uri25[2], len(_uri25[1])))
        _sender25.close()

        # Our own receiver is the strictest parser we can reach without a TV.
        _pushed25 = []

        class _Dlna25(protocol.DLNAProtocol):
            @property
            def renderer(self):
                return self

            def set_media_url(self, uri, start='0'):
                _pushed25.append(uri)

            def set_media_title(self, title):
                _pushed25.append(('title', title))

            def set_media_position(self, position):
                _pushed25.append(('position', position))

            def set_media_resume(self):
                _pushed25.append('resume')

            def set_media_pause(self):
                _pushed25.append('pause')

            def set_media_stop(self):
                _pushed25.append('stop')

            def release_playback(self):
                pass

        _dp25 = _Dlna25()
        _accepted25 = []
        for _action25, _body25, _cl25 in _taps25['calls']:
            try:
                _dp25.call(_body25)
                _accepted25.append(_action25)
            except Exception as e:
                _accepted25.append('{}:{}'.format(_action25, e))
        check("Macast's own SOAP parser accepts every envelope we send",
              _accepted25 == [a for a, _b, _c in _taps25['calls']],
              str(_accepted25))
        check("and it latches the URI, the DIDL title and the resume position",
              _pushed25[0] == 'http://192.0.2.9:8/media/x.mp4'
              and ('title', '深夜食堂') in _pushed25
              and ('position', '0:00:42') in _pushed25, str(_pushed25))

        # -- the whole hand-off, watchdog included --------------------------
        utils.Setting.set(clf.SettingProperty.Target_Kind, 'dlna')
        utils.Setting.set(clf.SettingProperty.Dlna_Control, _control25)
        utils.Setting.set(clf.SettingProperty.Dlna_Target_Name, '假的电视')
        utils.Setting.set(clf.SettingProperty.Mode, 'auto')
        utils.Setting.set(clf.SettingProperty.Audio_Stream, '')
        utils.Setting.set(clf.SettingProperty.Subtitle_Stream, '')
        utils.Setting.set(clf.SettingProperty.System_Audio, False)
        _taps25['calls'] = []
        _taps25['state'] = 'NO_MEDIA_PRESENT'
        _caster25b = _Caster25()
        _caster25b.start()                 # this one keeps its watchdog awake
        _caster25b.play_path(_media_dir25 + '/clean.mp4', '0:00:42')
        check("a television is handed the URL, a play and the resume seek",
              _wait_until(lambda: _verbs25() == ['SetAVTransportURI', 'Play',
                                                 'Seek'], timeout=25),
              str(_verbs25()))

        def _didl25(envelope):
            root = _ET25.fromstring(envelope)
            for node in root.iter():
                if node.tag.rpartition('}')[2] == 'CurrentURIMetaData':
                    return _ET25.fromstring(node.text or '<x/>')
            return None

        _resnode25 = None
        for _node25 in _didl25([b for a, b, _c in _taps25['calls']
                                if a == 'SetAVTransportURI'][-1]).iter():
            if _node25.tag.rpartition('}')[2] == 'res':
                _resnode25 = _node25
        check("the DIDL advertises a length and duration the file can honour",
              _resnode25 is not None
              and _resnode25.get('duration') == '0:02:00'
              and _resnode25.get('size') == '24000000'
              and 'DLNA.ORG_OP=01' in (_resnode25.get('protocolInfo') or ''),
              str(_resnode25.attrib if _resnode25 is not None else None))
        _dport25, _dpath25 = _split_url25(_resnode25.text)
        _st25, _hd25, _bd25 = _http25('GET', _dpath25, 'bytes=0-99',
                                      port=_dport25)
        check("the television range-reads the file we advertised to it",
              _st25 == 206 and _hd25['Content-Range'] == 'bytes 0-99/24000000'
              and _hd25['transferMode.dlna.org'] == 'Streaming'
              and len(_bd25) == 100, str(_hd25))
        check("the state page learns the length the TV claims, not ours",
              _wait_until(lambda: _caster25b.duration == 150.0
                          and _caster25b.position == 42.0, timeout=15)
              and _caster25b.mode == 'copy', _caster25b.status())
        _before25v = len(_taps25['calls'])
        _caster25b.set_media_volume(30)
        check("volume is not asked of a DLNA TV: its remote is nearer the amp",
              len(_taps25['calls']) == _before25v, str(_verbs25()))
        _caster25b.set_media_pause()
        _caster25b.set_media_resume()
        check("pause and resume are SOAP verbs, never a reload",
              _wait_until(lambda: _verbs25().count('Pause') == 1
                          and _verbs25().count('Play') == 2, timeout=15)
              and _verbs25().count('SetAVTransportURI') == 1, str(_verbs25()))
        _notify25.clear()
        _taps25['reported'] = 'NO_MEDIA_PRESENT'
        check("a takeover is re-pushed, and the re-push keeps the position",
              _wait_until(lambda: _verbs25().count('SetAVTransportURI') == 2,
                          timeout=20)
              and all('<Target>0:00:42</Target>' in b
                      for a, b, _c in _taps25['calls'] if a == 'Seek')
              and _caster25b.position == 42.0, str(_verbs25()))
        check("after MAX_REPUSH takeovers we stop fighting and say so",
              _wait_until(lambda: any('电视被别的设备占用' in str(n)
                                      for n in _notify25), timeout=30)
              and _caster25b.output is None
              and _verbs25().count('SetAVTransportURI') == clf.MAX_REPUSH + 1,
              '{} {}'.format(str(_notify25)[:160], _verbs25()))
        check("giving up hands the panel back instead of polling forever",
              _verbs25()[-1] == 'Stop'
              and ('transport', 'STOPPED') in _caster25b.recorder.rows
              and _caster25b.store.entries == {} and _caster25b.job is None,
              str(_verbs25()))
        _caster25b.stop()

        # -- who else is on the network -------------------------------------
        _desc25 = ('<root xmlns="urn:schemas-upnp-org:device-1-0"><device>'
                   '<friendlyName>客厅电视</friendlyName><serviceList><service>'
                   '<serviceType>urn:schemas-upnp-org:service:'
                   'AVTransport:1</serviceType><controlURL>%s</controlURL>'
                   '</service></serviceList></device></root>')
        check("a relative control URL is made absolute against the description",
              clf.parse_description(_desc25 % 'dmr/control/2',
                                    'http://192.0.2.7:49152/d.xml',
                                    '192.0.2.7')
              == ('客厅电视', 'http://192.0.2.7:49152/dmr/control/2'),
              str(clf.parse_description(_desc25 % 'dmr/control/2',
                                        'http://192.0.2.7:49152/d.xml',
                                        '192.0.2.7')))
        check("a device that describes itself on loopback is reached anyway",
              clf.parse_description(_desc25 % '/c',
                                    'http://127.0.0.1:49152/d.xml',
                                    '192.0.2.7')[1] == 'http://192.0.2.7:49152/c',
              str(clf.parse_description(_desc25 % '/c',
                                        'http://127.0.0.1:49152/d.xml',
                                        '192.0.2.7')))
        check("so is one that names another machine, or a port that is not a port",
              clf.parse_description(_desc25 % 'http://10.0.0.5:77/x',
                                    'http://192.0.2.7:49152/d.xml',
                                    '192.0.2.7')[1]
              == 'http://192.0.2.7:77/x'
              and clf.parse_description(_desc25 % 'http://10.0.0.5:foo/x',
                                        'http://192.0.2.7:49152/d.xml',
                                        '192.0.2.7')[1]
              == 'http://192.0.2.7:49152/x', '')
        check("no AVTransport service, or no XML at all, means no renderer",
              clf.parse_description(_desc25.replace('AVTransport:1', 'X:1')
                                    % '/c', 'http://192.0.2.7:49152/d',
                                    '192.0.2.7') is None
              and clf.parse_description('<root>', 'http://h/d', '1.2.3.4') is None
              and clf.parse_description('', 'http://h/d', '1.2.3.4') is None, '')
        _saved_adv25 = utils.Setting.__dict__['get_advertisable_ip']
        clf.Setting.get_advertisable_ip = lambda: ['192.0.2.41']
        _saved_ssdp25 = clf._ssdp_search
        _saved_rd25 = clf.describe_renderer
        _asks25 = []
        _answers25 = [('http://192.0.2.9:49152/d.xml', '192.0.2.9'),
                      ('http://192.0.2.41:49152/d.xml', '192.0.2.41')]

        def _search25(st, timeout=3.0):
            _asks25.append(st)
            return list(_answers25)

        clf._ssdp_search = _search25
        clf.describe_renderer = lambda url, peer, timeout=3.0: (
            None if peer == '192.0.2.41'
            else ('楼上的电视', 'http://{}:49152/c'.format(peer)))
        _found25 = clf.discover_renderers(timeout=0.1)
        check("the search asks for a MediaRenderer and drops this machine",
              _found25 == [('楼上的电视', 'http://192.0.2.9:49152/c',
                            '192.0.2.9')]
              and len(_asks25) == len(clf.DLNA_SEARCH_TARGETS),
              '{} {}'.format(_found25, _asks25))
        clf._ssdp_search = lambda st, timeout=3.0: list(_answers25[:1])
        _again25 = clf.discover_renderers(timeout=0.1)
        check("a renderer that answers both search targets is listed once",
              _again25 == [('楼上的电视', 'http://192.0.2.9:49152/c',
                            '192.0.2.9')], str(_again25))
        # …and the Chromecast half of the list drops itself the same way: this
        # app broadcasts `_googlecast._tcp`, so without it the first device the
        # user can pick is the machine holding the file.
        _restore25 = _own_address(clf.Setting)
        try:
            with _FakeMdns([('upstairs@abc._googlecast._tcp.local.',
                              '楼上的 Chromecast', '192.0.2.77', 8009),
                             ('Macast@abc._googlecast._tcp.local.',
                              '本机', '192.0.2.1', 8009)]):
                _cast25 = clf.discover_cast(timeout=0.01)
        finally:
            _restore25()
        check("the file caster's Chromecast search drops this machine",
              [host for _name, host, _port in _cast25] == ['192.0.2.77'],
              str(_cast25))
        _searching25 = []
        _saved_rsearch_fn25 = clf.start_renderer_search
        clf.start_renderer_search = lambda: _searching25.append('dlna') or True
        clf._dlna_devices = []
        _probe_setting25 = clf.LocalFileSetting()
        _probe_setting25.build_menu()
        check("an empty renderer list is refilled by a background search",
              _searching25 == ['dlna'], str(_searching25))
        clf.start_renderer_search = _saved_rsearch_fn25
        clf.Setting.get_advertisable_ip = _saved_adv25
        clf.describe_renderer = _saved_rd25
        clf._ssdp_search = _saved_ssdp25

        # --------------------------------------------------------------------
        # G. the menu bar: what it shows, and what a click costs
        # --------------------------------------------------------------------
        _searches25 = []
        clf._devices = [('客厅的电视', '192.0.2.9', 8009),
                        ('床头的', '192.0.2.10', 8009)]
        clf._dlna_devices = [('老电视', 'http://192.0.2.11:49152/x',
                              '192.0.2.11')]
        clf.start_search = lambda: _searches25.append('cast') or True

        def _texts25(items, out=None):
            out = [] if out is None else out
            for item in items:
                out.append(item.text)
                if item.children:
                    _texts25(item.children, out)
            return out

        def _item25(items, needle):
            for item in items:
                if needle in (item.text or ''):
                    return item
                found = _item25(item.children or [], needle)
                if found is not None:
                    return found
            return None

        def _exact25(items, text):
            """'-750 ms' contains '500 ms', so some labels need an exact match."""
            for item in items:
                if (item.text or '') == text:
                    return item
                found = _exact25(item.children or [], text)
                if found is not None:
                    return found
            return None

        utils.Setting.set(clf.SettingProperty.Target_Kind, 'cast')
        utils.Setting.set(clf.SettingProperty.Mode, 'auto')
        utils.Setting.set(clf.SettingProperty.Audio_Delay, 0)
        utils.Setting.set(clf.SettingProperty.Bitrate, clf.DEFAULT_BITRATE)
        utils.Setting.set(clf.SettingProperty.Hardware, False)
        utils.Setting.set(clf.SettingProperty.Folder, _media_dir25)
        clf._probe_cache.clear()
        clf.remember_probe(_twin_path25, _twin25)
        _caster25g = _Caster25()
        _caster25g.current = _twin_path25
        _caster25g.mode = 'copy'
        _caster25g.note = '电视能直接解码'
        _caster25g.duration = 150.0
        _caster25g.position = 65.0
        _caster25g.state = 'PAUSED'
        _caster25g.queue = [_twin_path25, _media_dir25 + '/clean.mp4']
        _caster25g.index = 0
        _restarts25 = []
        _caster25g.play_path = lambda path, start='0': _restarts25.append(
            (path, start))
        setting25 = _caster25g.renderer_setting
        setting25._renderer = lambda: _caster25g
        _probes_before25 = len(_probe_path25)
        _menu25 = setting25.build_menu()
        _flat25 = _texts25(_menu25)
        check("the menu must never probe a file: ffprobe on the UI thread hangs it",
              len(_probe_path25) == _probes_before25, str(_probe_path25[-2:]))
        check("the running item, its place in the list and its clock are one line",
              _caster25g.status().startswith('twin.mp4 · 直通 · 1/2')
              and '0:01:05/0:02:30' in _caster25g.status()
              and any('twin.mp4 · 直通 · 1/2' in t for t in _flat25),
              _caster25g.status())
        check("both kinds of target are offered and the current one is ticked",
              _item25(_menu25, clf.TARGETS['cast']).checked is True
              and _item25(_menu25, clf.TARGETS['dlna']).checked is False, '')
        check("discovered devices are listed under the one in use",
              '客厅的电视 · 192.0.2.9' in _flat25
              and '床头的 · 192.0.2.10' in _flat25
              and any(' · 127.0.0.1:' in t and '测试用的电视' in t
                      for t in _flat25), str(_flat25[-8:]))
        check("a paused item is offered 继续, not a second 暂停",
              _item25(_menu25, '继续') is not None
              and _item25(_menu25, '暂停') is None
              and _item25(_menu25, '停止（让电视回主页）') is not None, '')
        check("the tracks of the playing file are listed by index, codec and language",
              '#1 aac · eng · 默认' in _flat25 and '#2 ac3 · chi' in _flat25
              and '#3 subrip · chi' in _flat25
              and '音频：跟随文件' in _flat25 and '字幕：不投' in _flat25,
              str([t for t in _flat25 if t.startswith('#')]))
        check("the folder listing reaches the menu, capped and counted",
              'clean.mp4' in _flat25 and '全部按顺序播' in _flat25,
              str(_flat25[:6]))
        clf._devices = []
        _searches25 = []
        setting25.build_menu()
        check("an empty device list is refilled in the background",
              _searches25 == ['cast'] and clf._devices == [],
              str(_searches25))
        clf._devices = [('客厅的电视', '192.0.2.9', 8009),
                        ('床头的', '192.0.2.10', 8009)]

        setting25.on_audio_stream_clicked(_item25(_menu25, '#2 ac3 · chi'))
        check("picking a second audio track stores it and re-casts from here",
              utils.Setting.get(clf.SettingProperty.Audio_Stream) == '2'
              and _restarts25 == [(_twin_path25, '0:01:05')], str(_restarts25))
        _restarts25 = []
        setting25.on_subtitle_clicked(_item25(_menu25, '#3 subrip · chi'))
        check("so does picking a subtitle, because the choice is made at the LOAD",
              utils.Setting.get(clf.SettingProperty.Subtitle_Stream) == '3'
              and _restarts25 == [(_twin_path25, '0:01:05')], str(_restarts25))
        _restarts25 = []
        setting25.on_delay_clicked(_exact25(_menu25, '500 ms'))
        check("an A/V offset re-casts too: it is baked into the stream",
              utils.Setting.get(clf.SettingProperty.Audio_Delay) == 500
              and _restarts25 == [(_twin_path25, '0:01:05')], str(_restarts25))
        _restarts25 = []
        setting25.on_bitrate_clicked(_item25(_menu25, '10 Mbps'))
        setting25.on_hardware_clicked(_item25(_menu25, '硬件编码器'))
        check("quality choices wait for the next transcode instead of restarting",
              utils.Setting.get(clf.SettingProperty.Bitrate) == 10000000
              and utils.Setting.get(clf.SettingProperty.Hardware) is True
              and _restarts25 == [], str(_restarts25))
        _restarts25 = []
        setting25.on_mode_clicked(_item25(_menu25, '强制转码'))
        check("changing how the file is handled re-casts it that way",
              utils.Setting.get(clf.SettingProperty.Mode) == 'convert'
              and _restarts25 == [(_twin_path25, '0:01:05')], str(_restarts25))
        _restarts25 = []
        setting25.on_cast_clicked(_item25(_menu25, '床头的 · 192.0.2.10'))
        check("choosing another Chromecast moves the same file to it",
              utils.Setting.get(clf.SettingProperty.Cast_Target)
              == '192.0.2.10:8009'
              and utils.Setting.get(clf.SettingProperty.Cast_Target_Name)
              == '床头的' and _restarts25 == [(_twin_path25, '0:01:05')],
              str(_restarts25))
        utils.Setting.set(clf.SettingProperty.Cast_Target, '127.0.0.1:1')
        _notify25.clear()
        _restarts25 = []
        setting25.on_kind_clicked(_item25(_menu25, clf.TARGETS['dlna']))
        check("switching kind is a setting and a note, not a surprise reload",
              utils.Setting.get(clf.SettingProperty.Target_Kind) == 'dlna'
              and _restarts25 == []
              and any('输出目标类型' in str(n) for n in _notify25),
              str(_notify25))
        _menu25d = setting25.build_menu()
        check("the DLNA branch lists the renderers the search found",
              '老电视 · 192.0.2.11' in _texts25(_menu25d)
              and '重新搜索 DLNA 电视' in _texts25(_menu25d),
              str(_texts25(_menu25d)[-6:]))
        utils.Setting.set(clf.SettingProperty.Target_Kind, 'cast')
        _footer25 = [t for t in _texts25(setting25.build_menu())
                     if t.startswith('转码临时目录 ')]
        check("a forced transcode says in the footer where its file goes",
              len(_footer25) == 1
              and clf.temp_root() in _footer25[0]
              and '{} MB'.format(int(clf.MAX_ADVERTISED_SIZE / 1048576))
              in _footer25[0], str(_footer25))

        # --------------------------------------------------------------------
        # H. who owns the process, the socket and the temporary file
        # --------------------------------------------------------------------
        class _Out25(clf.CastSender):
            def __init__(self):
                self.calls = []
                self.app_id = clf.DEFAULT_MEDIA_APP_ID

            def stop(self):
                self.calls.append('stop')

            def quit_app(self):
                self.calls.append('quit')

            def close(self):
                self.calls.append('close')

            def connect(self):
                raise AssertionError('a superseded hand-off must not connect')

            def media_status(self):
                return _taps25.get('status3', ('IDLE', '', 12.0))

            def applications(self):
                return ['Other App']

        _caster25h = _Caster25()
        _rec25h = _caster25h.recorder
        _caster25h._ensure_server()
        _out25h = _Out25()
        _job25h = clf.Job(_fake_ffmpeg25, ['-i', 'x'], 'ts', 'video/mp2t',
                          total=65536, label='owned')
        _job25h.start()
        _entry25h = _caster25h.store.add(_job25h.path, 'video/mp2t', 'ts',
                                         job=_job25h, title='owned')
        _caster25h.output = _out25h
        _caster25h.job = _job25h
        _caster25h.entries = [_entry25h]
        _caster25h.repush = 2
        _dir25h = _job25h.directory
        _caster25h._begin('/x/next.mp4')
        check("the next hand-off owns the last one's encoder and connection",
              _out25h.calls == ['stop', 'close'] and _job25h.proc is None
              and not os.path.isdir(_dir25h)
              and _caster25h.store.entries == {} and _caster25h.entries == [],
              '{} {}'.format(_out25h.calls, _caster25h.store.entries))
        check("…and a re-push does not buy itself a fresh retry budget",
              _caster25h.current == '/x/next.mp4' and _caster25h.repush == 0,
              _caster25h.repush)
        _caster25h.job = _job25h
        _caster25h.entries = [_entry25h]
        check("a superseded worker retires its work instead of casting over it",
              _caster25h._push(_caster25h.generation - 1, 'cast', 'x',
                               '127.0.0.1:1', '127.0.0.1', 1, 'u', 'video/mp2t',
                               False, 't', clf.Media(), 0.0, [], 'convert')
              is None and _caster25h.job is None
              and _caster25h.store.entries == {}
              and _caster25h.recorder.rows == [], str(_rec25h.rows))
        _out25b = _Out25()
        _caster25h.output = _out25b
        _caster25h.set_media_stop(quit_app=False)
        check("a hand-off stops the app; a real stop quits it",
              _out25b.calls == ['stop', 'close'], str(_out25b.calls))
        _out25c = _Out25()
        _caster25h.output = _out25c
        _caster25h.set_media_stop()
        check("…which is why the panel only comes back on a real stop",
              _out25c.calls == ['stop', 'quit', 'close'], str(_out25c.calls))
        _out25d = _Out25()
        _advanced25 = []
        _caster25h.output = _out25d
        _caster25h.queue = ['/a.mp4', '/b.mp4']
        _caster25h.index = 0
        _caster25h.play_index = lambda i, start='0': _advanced25.append(i) or True
        utils.Setting.set(clf.SettingProperty.Auto_Next, True)
        _caster25h._ended(_caster25h.generation)
        check("finishing an item advances the list without quitting the app",
              _advanced25 == [1] and _out25d.calls == ['stop', 'close'],
              '{} {}'.format(_advanced25, _out25d.calls))
        _out25e = _Out25()
        _advanced25 = []
        _caster25h.output = _out25e
        _caster25h.index = 1
        _caster25h._ended(_caster25h.generation)
        check("the last item of the list gives the television back",
              _advanced25 == [] and _out25e.calls == ['stop', 'quit', 'close'],
              str(_out25e.calls))
        _out25f = _Out25()
        _caster25h.output = _out25f
        _caster25h.play_index = lambda i, start='0': None
        _caster25h.generation += 1
        _caster25h.queue = ['/a.mp4']
        _caster25h.index = 0
        utils.Setting.set(clf.SettingProperty.Auto_Next, False)
        _caster25h._ended(_caster25h.generation)
        check("with auto-next off, an end is an end",
              _out25f.calls == ['stop', 'quit', 'close'], str(_out25f.calls))
        _caster25h.generation += 1
        _caster25h.output = _Out25()
        _caster25h.current = '/x/clean.mp4'
        _caster25h.position = 42.0
        _caster25h.mode = 'copy'
        _caster25h.repush = 0
        _delivers25 = []
        _caster25h._deliver = lambda path, start, retry=False: _delivers25.append(
            (path, start, retry))
        _caster25h._take_back(_caster25h.generation)
        check("a takeover re-pushes from where the device stopped, as a retry",
              _delivers25 == [('/x/clean.mp4', '0:00:42', True)]
              and _caster25h.repush == 1, str(_delivers25))
        _notify25.clear()
        _caster25h.repush = clf.MAX_REPUSH
        _caster25h.generation += 1
        _out25g = _Out25()
        _caster25h.output = _out25g
        _caster25h._take_back(_caster25h.generation)
        check("the fourth attempt gives up, and says why in one line",
              _delivers25 == [('/x/clean.mp4', '0:00:42', True)]
              and _out25g.calls == ['stop', 'quit', 'close']
              and any('电视被别的设备占用' in str(n) for n in _notify25)
              and ('error', True) in _rec25h.rows,
              '{} {}'.format(_delivers25, str(_notify25)[:120]))
        _notify25.clear()
        _caster25h.mode = 'audio'
        _caster25h.repush = 0
        utils.Setting.set(clf.SettingProperty.System_Audio, True)
        _caster25h.generation += 1
        _caster25h._take_back(_caster25h.generation)
        check("a dropped system-sound stream is reported and switched off",
          any('系统声音投屏已断开' in str(n) for n in _notify25)
          and clf.system_audio_wanted() is False
          and _delivers25 == [('/x/clean.mp4', '0:00:42', True)],
          str(_notify25)[:160])
        check("the watchdog reads a squatting app as a takeover, not as idle",
              _caster25h._poll(_Out25()) == ('SQUATTED', '', 12.0), '')
        # Back to the real delivery path: the stubs above only existed to keep
        # the takeover checks from launching an encoder.
        del _caster25h.play_index
        del _caster25h._deliver
        _saved_free25 = clf.free_space
        clf.free_space = lambda path: 1
        utils.Setting.set(clf.SettingProperty.Mode, 'convert')
        _notify25.clear()
        _rec25h.rows = []
        _caster25h.play_path(_media_dir25 + '/clean.mp4')
        check("a transcode that would fill the disk is refused before it starts",
              _wait_until(lambda: any('临时目录放不下' in str(n)
                                      for n in _notify25), timeout=15)
              and ('error', True) in _rec25h.rows
              and _caster25h.job is None, str(_notify25)[:200])
        clf.free_space = lambda path: 1 << 40
        _saved_tool25 = clf.find_tool
        clf.find_tool = lambda name: None
        _notify25.clear()
        _caster25h.play_path(_media_dir25 + '/clean.mp4')
        check("needing a transcode with no ffmpeg on the machine is its own message",
              _wait_until(lambda: any('没有 ffmpeg' in str(n)
                                      for n in _notify25), timeout=15),
              str(_notify25)[:200])
        clf.find_tool = lambda name: '/nonexistent/ffmpeg-nope'
        _notify25.clear()
        _caster25h.play_path(_media_dir25 + '/clean.mp4')
        check("an ffmpeg that will not spawn is reported, not left black",
              _wait_until(lambda: any('没能开始转码' in str(n)
                                      for n in _notify25), timeout=15)
              and _caster25h.store.entries == {}, str(_notify25)[:200])
        clf.find_tool = _saved_tool25
        clf.free_space = _saved_free25
        utils.Setting.set(clf.SettingProperty.Mode, 'auto')
        _caster25h.output = _Out25()
        _caster25h.stop()
        check("closing the plugin takes the media server and the store down",
              _caster25h.server is None and _caster25h.store.entries == {}
              and _caster25h.running is False, '')
        _caster25.stop()
        _caster25g.stop()

    except Exception as e:
        import traceback
        traceback.print_exc()
        check("the local file caster behaves", False,
              "{}: {}".format(type(e).__name__, e))
    finally:
        for _c25 in (_caster25, _caster25b, _caster25g, _caster25h):
            try:
                if _c25 is not None:
                    _c25.running = False
                    _c25.stop()
            except Exception:
                pass
        try:
            if _receiver25 is not None:
                _receiver25.stop()
        except Exception:
            pass
        for _http25 in (_tv25, _server25):
            try:
                if _http25 is not None:
                    _http25.serving = False
                    _http25.shutdown()
                    _http25.server_close()
            except Exception:
                pass
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify25_rec)
        except Exception:
            pass
        os.environ['PATH'] = _saved_path25
        utils.SETTING_DIR = _saved_dir25
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting25
        if _taps_back25:
            for _name25 in ('probe_media', '_keep_awake', '_stop_awake',
                            'WATCHDOG_SECONDS', 'find_tool', 'free_space',
                            'system_audio_input', '_ssdp_search',
                            'describe_renderer', 'start_search',
                            'start_renderer_search', '_devices',
                            '_dlna_devices'):
                setattr(clf, _name25, _taps_back25[_name25])
            setattr(utils.Setting, 'get_advertisable_ip',
                    _taps_back25['advertisable'])
            clf._probe_cache.clear()
            clf._probe_cache.update(_probe_snapshot25)
            clf.invalidate_tool_cache()
        _shutil.rmtree(_tmp25, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the local file caster loads", False,
          "{}: {}".format(type(e).__name__, e))


# --------------------------------------------------------------------------
# Part 26: the AirPlay screen mirror supervisor
#
# macast/plugins/protocol/airplay_mirror.py does not implement AirPlay mirroring; it keeps
# uxplay running with an option file that carries the Macast name. Everything
# worth testing is therefore either the option file (uxplay's config format has
# no escaping rules that a friendly name with a space satisfies by accident) or
# the supervision (which fd the log comes out of, what happens when the binary
# is missing, and what must *not* be reported).
#
# The log lines below are copied from uxplay's own source strings, including
# the two spellings of the disconnection message -- the parser matches on
# substrings precisely because those prefixes are not part of any contract.
# --------------------------------------------------------------------------
print("\n=== Part 26: AirPlay screen mirror supervisor ===")
try:
    _saved_setting26 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir26 = utils.SETTING_DIR
    _saved_path26 = os.environ.get('PATH', '')
    _tmp26 = _tempfile.mkdtemp(prefix="macast-airplay-mirror-")
    _notify26 = []
    _notify26_rec = lambda *a, **k: _notify26.append(a)      # noqa: E731
    try:
        utils.SETTING_DIR = _tmp26
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp26, "macast_setting.json")
        cherrypy.engine.subscribe('app_notify', _notify26_rec)

        am = _load_plugin("airplay_mirror_plugin", "airplay_mirror.py")

        check("uxplay does not force the SSDP server on",
              am.AirPlayMirrorProtocol.uses_ssdp is False)
        # The built-in AirPlay receiver owns the transport state; a mirror
        # session has no media url and no position to report.
        _proto26 = am.AirPlayMirrorProtocol()
        check("no DLNA transport state is claimed by the mirror supervisor",
              sorted(_proto26.methods()) == sorted(protocol.Protocol().methods()),
              str(_proto26.methods()))

        # -- the option file ------------------------------------------------
        _body26 = am.rc_body('无常的 Mac "Pro" -x')
        check("a friendly name with a space stays a single option",
              'n "无常的 Mac Pro -x"' in _body26, _body26)
        check("quotes and backslashes cannot escape out of the name",
              all(c not in am.safe_name('a"b\\c\rd\ne') for c in '"\\\r\n'),
              repr(am.safe_name('a"b\\c\rd\ne')))
        check("a name that is empty after cleaning still names the server",
              am.safe_name('  ""  ') == 'Macast', repr(am.safe_name('  ""  ')))
        # uxplay reads "-n <value>" as "no argument" when the value starts with
        # a dash, and then refuses to start at all.
        check("a name beginning with a dash cannot look like another option",
              not am.safe_name('-weird').startswith('-'), am.safe_name('-weird'))
        check("no option line carries the dash the file format forbids",
              all(not line.startswith('-') for line in _body26.splitlines()
                  if line and not line.startswith('#')), _body26)
        check("mirroring uses the timestamp-free sync uxplay recommends for it",
              '\nvsync no\n' in _body26, _body26)
        check("macOS picks the videosink the official GStreamer build has",
              ('\nvs osxvideosink\n' in _body26) == (sys.platform == 'darwin'),
              _body26)
        check("user options are appended after the defaults so they win",
              am.rc_body('X', ['vs glimagesink']).index('vsync no')
              < am.rc_body('X', ['vs glimagesink']).index('vs glimagesink'))
        check("options typed with a leading dash still reach uxplay",
              am.extra_rc_lines('-pin 1234\n\n  \nrestrict yes\n')
              == ['pin 1234', 'restrict yes'], str(am.extra_rc_lines('-pin 1234\n\n  \nrestrict yes\n')))

        # -- what the log means --------------------------------------------
        _ev26 = am.log_event('connection request from Anna (iPhone14,3)'
                             ' with deviceID = a1:b2:c3:d4:e5:f6')
        check("a client connecting names the device it came from",
              _ev26[0] == 'connected' and 'Anna' in _ev26[1]
              and 'iPhone14,3' in _ev26[1], str(_ev26))
        check("a client with no model string still reports",
              am.log_event('connection request from MacBook () with deviceID = x')
              [0] == 'connected',
              str(am.log_event('connection request from MacBook () with deviceID = x')))
        check("the disconnection is reported in both of uxplay's spellings",
              am.log_event('*** ERROR lost connection with client (network problem?)')[0]
              == 'disconnected'
              and am.log_event('***ERROR lost connection with client (network problem?)')[0]
              == 'disconnected')
        check("a rejected client is not silently dropped",
              am.log_event('*** attempt to connect by blocked client (clientID x): DENIED')
              [0] == 'blocked')
        # Without this the symptom is "my Mac is not in the list" and the cause
        # is another receiver holding the name.
        check("a failed mDNS registration is told to the user",
              am.log_event('*** ERROR: dnssd_register_airplay failed with error code -65537')
              [0] == 'mdns-failed')
        check("the server coming up is not a user-facing event",
              am.log_event('Initialized server socket(s)')[0] == 'ready')
        check("uxplay's chatter is not mistaken for an event",
              all(am.log_event(line) == (None, None) for line in (
                  'UxPlay 1.73.7: An Open-Source AirPlay mirroring and audio-streaming server.',
                  'using network ports UDP 52000 52001 52002 TCP 53000 53001 53002',
                  'UxPlay on macOS is using -nc option as workaround for GStreamer problem')))

        # uxplay prints with printf(); against a pipe that is block-buffered,
        # so the events above would arrive whenever 4 KiB happened to pile up
        # behind them. A pty keeps stdout line-buffered.
        _child26, _read26 = am._open_stdout()
        try:
            if sys.platform == 'win32':
                check("the log reaches us as it happens (pty, or Windows pipe)",
                      not os.isatty(_read26), 'windows fallback')
            else:
                check("the log reaches us as it happens",
                      os.isatty(_read26), 'not a tty: stdout would be block-buffered')
        finally:
            os.close(_child26)
            os.close(_read26)

        # -- supervising a real process ------------------------------------
        _bin26 = os.path.join(_tmp26, "bin")
        _argv26 = os.path.join(_tmp26, "argv.txt")
        _env26 = os.path.join(_tmp26, "env.txt")
        _fake_uxplay = _write_fake(
            _bin26, "uxplay",
            "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > {}\n"
            "printf 'UXPLAYRC=[%s]\\n' \"${{UXPLAYRC-}}\" > {}\n"
            "echo 'UxPlay 1.73.7: An Open-Source AirPlay mirroring and audio-streaming server.'\n"
            "echo 'Initialized server socket(s)'\n"
            "echo 'connection request from Anna (iPhone14,3) with deviceID = a1:b2:c3:d4:e5:f6'\n"
            "echo 'audio progress (min:sec):  1:23; remaining:  0:10; track length 1:33'\n"
            "echo 'lost connection with client (network problem?)'\n"
            "exec sleep 30\n".format(_argv26, _env26))

        def _argv26_raw():
            try:
                return open(_argv26, encoding='utf-8').read()
            except OSError:
                return 'no argv'

        def _argv26_list():
            return [a for a in _argv26_raw().splitlines() if a]

        os.environ['PATH'] = _bin26 + os.pathsep + _saved_path26
        check("uxplay is found on PATH", am.find_uxplay() == _fake_uxplay,
              str(am.find_uxplay()))

        _notify26[:] = []
        _user_rc26 = os.path.expanduser('~/.uxplayrc')
        _had_user_rc26 = os.path.exists(_user_rc26)
        proto26 = am.AirPlayMirrorProtocol()
        proto26.start()
        check("the supervisor starts uxplay", proto26.running())
        _rc26 = os.path.join(_tmp26, am.RC_NAME)
        # Macast's own config directory, never ~/.uxplayrc: that file may hold
        # options the user wrote by hand (AGENTS.md §10).
        check("the option file is written inside Macast's own directory",
              os.path.exists(_rc26) and os.path.dirname(
                  os.path.realpath(_rc26)) == os.path.realpath(_tmp26), _rc26)
        _body26 = open(_rc26, encoding='utf-8').read()
        check("the name uxplay advertises is Macast's friendly name",
              'n "{}"'.format(utils.Setting.get_friendly_name()) in _body26, _body26)
        _wait_until(lambda: len(_argv26_list()) == 3)
        _argv_list26 = _argv26_list()
        check("uxplay was pointed at the file we wrote",
              _argv_list26 == [_fake_uxplay, '-rc', _rc26], _argv26_raw())
        # `-p` is the legacy port set, and its TCP 7000 is what Macast's own
        # AirPlay receiver binds (and macOS's built-in one holds). uxplay
        # advertises whatever it picked over mDNS, so dynamic ports cost
        # nothing and a collision costs everything.
        check("no fixed port set is requested, so nothing collides on 7000",
              '-p' not in _argv_list26, str(_argv_list26))
        # $UXPLAYRC pointing at a file that is not there makes uxplay read the
        # user's ~/.uxplayrc instead -- a silent betrayal of the settings page.
        check("the user's own uxplay configuration is neither read nor written",
              _wait_until(lambda: os.path.exists(_env26))
              and 'UXPLAYRC=[]' in open(_env26, encoding='utf-8').read()
              and os.path.exists(_user_rc26) == _had_user_rc26,
              open(_env26, encoding='utf-8').read() if os.path.exists(_env26) else 'no env')
        check("an AirPlay client connecting is surfaced to the user",
              _wait_until(lambda: any('Anna' in str(n) for n in _notify26),
                          timeout=10), str(_notify26))
        check("the session ending is surfaced to the user",
              _wait_until(lambda: any('已断开' in str(n) for n in _notify26),
                          timeout=10), str(_notify26))
        check("uxplay's chatter stays out of the notifications",
              not any('UxPlay' in str(n) or 'network ports' in str(n)
                      or 'audio progress' in str(n) for n in _notify26),
              str(_notify26))

        _notify26[:] = []
        _pid26 = proto26._proc.pid
        proto26.stop()
        check("stopping the plugin stops uxplay", not proto26.running())
        check("an intentional stop is not reported as a crash",
              not any('已退出' in str(n) for n in _notify26), str(_notify26))

        proto26.reload()
        check("reload replaces the uxplay that was running",
              proto26.running() and proto26._proc.pid != _pid26,
              '{} vs {}'.format(_pid26, proto26._proc.pid))
        check("the reloaded instance is not reported as a crash",
              not any('已退出' in str(n) for n in _notify26), str(_notify26))
        proto26.stop()

        _real_find26 = am.find_uxplay
        am.find_uxplay = lambda: None
        _notify26[:] = []
        proto26b = am.AirPlayMirrorProtocol()
        proto26b.start()
        check("a missing uxplay is reported with what to do about it",
              any('uxplay' in str(n) and '编译' in str(n) for n in _notify26),
              str(_notify26))
        check("the build recipe names the dependencies, not just the project",
              all(word in am.INSTALL_GUIDE for word in
                  ('cmake', 'libplist', 'openssl', 'GStreamer', 'make install')))
        check("a missing binary leaves no process behind", proto26b._proc is None)
        am.find_uxplay = _real_find26

        # -- dying on its own ----------------------------------------------
        class _DeadProc26(object):
            def __init__(self, code):
                self.returncode = code

            def poll(self):
                return self.returncode

            def wait(self, timeout=None):
                return self.returncode

        import io as _io26
        _dead26 = _DeadProc26(1)
        proto26._proc = _dead26
        _notify26[:] = []
        proto26._read_output(_dead26, _io26.StringIO(
            'using network ports UDP 1 2 3 TCP 4 5 6\n'
            '*** ERROR: Could not find videosink "glimagesink"\n'))
        check("a uxplay that dies by itself says so",
              any('已退出' in str(n) for n in _notify26), str(_notify26))
        check("the last error line is what the user sees",
              any('videosink' in str(n) for n in _notify26), str(_notify26))
        check("a dead process is not left in the running state",
              proto26._proc is None and not proto26.running())
        _notify26[:] = []
        proto26._read_output(_DeadProc26(0), _io26.StringIO('whatever\n'))
        check("the reader of a replaced instance stays quiet",
              not _notify26, str(_notify26))

        # -- two receivers, one phone ---------------------------------------
        _notify26[:] = []
        utils.Setting.setting[utils.SettingProperty.Macast_Protocols.name] = \
            ['DLNA', 'AirPlay', 'Chromecast']
        am.AirPlayMirrorProtocol()._warn_duplicate_receiver()
        check("the built-in AirPlay receiver being on is mentioned once",
              len([n for n in _notify26 if '接收端' in str(n)]) == 1, str(_notify26))
        _notify26[:] = []
        # This plugin's own title, and the RAOP supervisor's, sit in the same
        # list; neither is a second video-URL receiver.
        utils.Setting.setting[utils.SettingProperty.Macast_Protocols.name] = \
            ['DLNA', 'AirPlay Screen Mirror', 'AirPlay Audio (RAOP)']
        am.AirPlayMirrorProtocol()._warn_duplicate_receiver()
        check("this plugin is not itself a competing receiver",
              not _notify26, str(_notify26))
        _notify26[:] = []
        utils.Setting.setting.pop(
            utils.SettingProperty.Macast_Protocols.name, None)
        am.AirPlayMirrorProtocol()._warn_duplicate_receiver()
        check("a settings file that predates the protocol list warns nobody",
              not _notify26, str(_notify26))
    finally:
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify26_rec)
        except Exception:
            pass
        for _p26 in ('proto26', 'proto26b'):
            _obj26 = locals().get(_p26)
            if _obj26 is not None:
                _obj26.stop()
        os.environ['PATH'] = _saved_path26
        utils.SETTING_DIR = _saved_dir26
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting26
        _shutil.rmtree(_tmp26, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the AirPlay screen mirror supervisor behaves", False,
          "{}: {}".format(type(e).__name__, e))


# --------------------------------------------------------------------------
# Part 27: per-module log files (macast/logsplit.py)
#
# A plugin that logs at frame rate would bury the core protocol lines in
# macast.log. `claim()` gives a logger its own file under logs/ and turns
# propagation off, so the record lands in exactly one place -- the module
# file -- and the settings page can show either one on its own.
# --------------------------------------------------------------------------
print("\n=== Part 27: module log splitting ===")
try:
    import logging as _lg27
    logsplit = sys.modules.get("macast.logsplit") or _load("logsplit", "logsplit.py")

    _tmp27 = _tempfile.mkdtemp(prefix="macast-logsplit-")
    _saved_dir27_utils = utils.SETTING_DIR
    _saved_dir27_proto = protocol.SETTING_DIR
    _saved_setting27 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_running27 = utils.Setting.is_service_running
    _root27 = _lg27.getLogger()
    _saved_root_level27 = _root27.level
    _saved_root_handlers27 = list(_root27.handlers)
    _probe_seen27 = []
    _probe27 = _lg27.Handler()
    _probe27.emit = lambda rec: _probe_seen27.append(rec.name)
    _file27 = _lg27.FileHandler(
        os.path.join(_tmp27, utils.LOG_FILE_NAME), encoding='utf-8')
    _added27 = [_probe27, _file27]
    _logs_dir27 = os.path.join(_tmp27, 'logs')
    _log_path27 = os.path.join(_tmp27, utils.LOG_FILE_NAME)
    try:
        utils.SETTING_DIR = _tmp27
        protocol.SETTING_DIR = _tmp27
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp27, "macast_setting.json")
        _root27.addHandler(_probe27)
        _root27.addHandler(_file27)
        _root27.setLevel(_lg27.INFO)

        # -- claim() routing ---------------------------------------------
        _mod27 = _lg27.getLogger("MirrorProbe")
        _p27 = logsplit.claim("MirrorProbe")
        check("a claimed logger owns a file under logs/",
              _p27.endswith(os.path.join("logs", "MirrorProbe.log"))
              and os.path.isfile(_p27), _p27)
        check("claiming turns propagation off (that is the whole point)",
              _mod27.propagate is False)
        check("claim is idempotent: same path, one handler",
              logsplit.claim("MirrorProbe") == _p27
              and sum(1 for h in _mod27.handlers
                      if getattr(h, "_macast_module_log", False)) == 1,
              str(_mod27.handlers))
        _probe_seen27[:] = []
        _mod27.info("frame 1")
        _lg27.getLogger("SomeCore").info("core side line")
        _file27.flush()
        check("module records never reach the root handler",
              "MirrorProbe" not in _probe_seen27, str(_probe_seen27))
        check("module records never reach macast.log",
              "frame 1" in open(_p27, encoding="utf-8").read()
              and "frame 1" not in open(_log_path27, encoding="utf-8").read(),
              open(_log_path27, encoding="utf-8").read())
        check("unclaimed loggers keep flowing into macast.log",
              "core side line" in open(_log_path27, encoding="utf-8").read(),
              open(_log_path27, encoding="utf-8").read()[-120:])

        _with_logger = types.SimpleNamespace(logger=_lg27.getLogger("HookProbe"))
        _without = types.SimpleNamespace()
        check("claim_from_module picks up a plugin module's logger",
              logsplit.claim_from_module(_with_logger)
              == os.path.join(_logs_dir27, "HookProbe.log")
              and logsplit.claim_from_module(_without) == '',
              str(logsplit.claimed_names()))

        # -- file names, listing, clearing --------------------------------
        _evil = logsplit.file_name("../evil name/中文")
        check("a logger name can only ever become one plain file name",
              "/" not in _evil and "\\" not in _evil and ".." not in _evil
              and _evil.endswith(".log"), _evil)
        with open(os.path.join(_logs_dir27, "Other.log.1"), "w",
                  encoding="utf-8") as _f:
            _f.write("rotated\n")
        with open(os.path.join(_logs_dir27, "notes.txt"), "w",
                  encoding="utf-8") as _f:
            _f.write("not a log\n")
        _listing = logsplit.list_logs()
        _names = [m["name"] for m in _listing]
        check("list_logs shows each module once, rotated backups excluded",
              _names.count("MirrorProbe") == 1 and "Other" in _names
              and "notes" not in _names, str(_names))
        check("list_logs carries what the picker needs",
              all({"name", "file", "size", "mtime"} <= set(m) for m in _listing),
              str(_listing[:1]))
        with open(_p27 + ".1", "w", encoding="utf-8") as _f:
            _f.write("stale backup\n")
        _removed = logsplit.clear("MirrorProbe")
        check("clear truncates the module log and drops its backups",
              _removed >= 2 and os.path.getsize(_p27) == 0
              and not os.path.exists(_p27 + ".1"), str(_removed))

        # -- the HTTP surface the log tab uses ----------------------------
        utils.Setting.is_service_running = staticmethod(lambda: True)

        class _LogHandler27(protocol.Handler):
            """Skips the real __init__ (it reads the settings page off disk)."""

            def __init__(self):
                pass

            @property
            def protocol(self):
                return types.SimpleNamespace()

        handler27 = _LogHandler27()
        from cherrypy import serving as _serving27
        request27 = _serving27.request
        _saved_params27 = request27.params
        _saved_remote27 = getattr(request27, "remote", None)
        _saved_scheme27 = request27.scheme
        _saved_headers27 = dict(request27.headers)
        try:
            def _reset27(ip="127.0.0.1"):
                request27.headers.clear()
                for _k, _v in _saved_headers27.items():
                    request27.headers[_k] = _v
                request27.params = {}
                request27.remote = types.SimpleNamespace(ip=ip)
                request27.scheme = "http"

            def _get27(**kw):
                return json.loads(handler27.GET(param="api", **kw).decode())

            # The module has to have something to show after the clear above.
            _mod27.info("frame 2")
            for _h in _mod27.handlers:
                _h.flush()

            _reset27()
            _mods = _get27(query="log-modules")
            check("log-modules lists the global file and one row per module",
                  _mods["main"]["size"] > 0
                  and "MirrorProbe" in [m["name"] for m in _mods["modules"]],
                  str(_mods))
            _res = _get27(query="log", module="MirrorProbe")
            check("query=log?module= reads that module's own file",
                  _res.get("code") == 0 and _res.get("module") == "MirrorProbe"
                  and "frame 2" in _res.get("logs", "")
                  and "core side line" not in _res.get("logs", ""),
                  str(_res).replace("\n", " ")[:160])
            _res = _get27(query="log")
            check("the global view keeps showing macast.log only",
                  _res.get("code") == 0 and _res.get("module") == ""
                  and "core side line" in _res.get("logs", "")
                  and "frame 2" not in _res.get("logs", ""),
                  str(_res).replace("\n", " ")[:160])
            _res = _get27(query="log", module="整体")
            check("「整体」 is an accepted alias for the global file",
                  _res.get("code") == 0 and _res.get("module") == "", str(_res))
            _res = _get27(query="log", module="NoSuchModule")
            check("an unknown module is an explicit error, never a fallback",
                  _res.get("code") == 1 and _res.get("logs") == "", str(_res))

            _reset27(ip="192.168.1.9")
            check("log-modules is gated with the other management queries",
                  _get27(query="log-modules").get("code") == 403)
            check("a module log is not readable from the LAN without the token",
                  _get27(query="log", module="MirrorProbe").get("code") == 403)
            _reset27()
            _blob = handler27.GET(param="api", query="log-download",
                                  module="MirrorProbe")
            check("log-download follows the module too",
                  isinstance(_blob, bytes) and b"frame 2" in _blob
                  and "MirrorProbe.log" in str(
                      _serving27.response.headers.get("Content-Disposition", "")),
                  str(dict(_serving27.response.headers)))
            _res = json.loads(handler27.POST(**{"clear-log": "1",
                                                "module": "MirrorProbe"}).decode())
            check("clear-log?module= clears just that module",
                  _res.get("code") == 0 and os.path.getsize(_p27) == 0
                  and os.path.getsize(_log_path27) > 0,
                  "{} / {}".format(_res, os.path.getsize(_log_path27)))
            _mod27.info("frame 3")
            for _h in _mod27.handlers:
                _h.flush()
            _res = json.loads(handler27.POST(**{"clear-log": "1"}).decode())
            check("clearing the global log leaves module files alone",
                  _res.get("code") == 0 and os.path.getsize(_log_path27) == 0
                  and "frame 3" in open(_p27, encoding="utf-8").read(),
                  str(_res))
            _res = json.loads(handler27.POST(**{"clear-log": "1",
                                                "module": "NoSuchModule"}).decode())
            check("clear-log refuses an unknown module",
                  _res.get("code") == 1, str(_res))
        finally:
            request27.params = _saved_params27
            request27.remote = _saved_remote27
            request27.scheme = _saved_scheme27
            request27.headers.clear()
            for _k, _v in _saved_headers27.items():
                request27.headers[_k] = _v

        # -- startup wipe: module logs are cleared with macast.log ---------
        check("remove_all clears the logs directory of every log file",
              logsplit.remove_all() >= 1
              and not [f for f in os.listdir(_logs_dir27) if ".log" in f]
              and os.path.exists(os.path.join(_logs_dir27, "notes.txt")),
              str(sorted(os.listdir(_logs_dir27))))

        # -- the wiring this part cannot exercise end-to-end ---------------
        with open(os.path.join(MACAST, "macast.py"), "r", encoding="utf-8") as _f:
            _macast_src27 = _f.read()
        check("both plugin load paths claim the module's logger",
              _macast_src27.count("logsplit.claim_from_module(module)") == 2,
              str(_macast_src27.count("logsplit.claim_from_module(module)")))
        with open(os.path.join(REPO, "Macast.py"), "r", encoding="utf-8") as _f:
            _entry_src27 = _f.read()
        check("clear_env wipes module logs with the main one",
              "logsplit.remove_all()" in _entry_src27)
        with open(os.path.join(MACAST, "xml", "setting.html"), "r",
                  encoding="utf-8") as _f:
            _html27 = _f.read()
        check("the log tab offers the module picker and threads it through",
              "query=log-modules" in _html27
              and _html27.count("&module=") >= 2
              and "fd.append('module', this.log_module)" in _html27)
    finally:
        for _n27 in logsplit.claimed_names():
            _lgone = _lg27.getLogger(_n27)
            for _h in list(_lgone.handlers):
                if getattr(_h, "_macast_module_log", False):
                    _lgone.removeHandler(_h)
                    try:
                        _h.close()
                    except Exception:
                        pass
            _lgone.propagate = True
        logsplit._claimed.clear()
        for _h in _added27:
            _root27.removeHandler(_h)
            try:
                _h.close()
            except Exception:
                pass
        _root27.setLevel(_saved_root_level27)
        utils.SETTING_DIR = _saved_dir27_utils
        protocol.SETTING_DIR = _saved_dir27_proto
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting27
        utils.Setting.is_service_running = staticmethod(_saved_running27)
        _shutil.rmtree(_tmp27, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("module log splitting behaves", False,
          "{}: {}".format(type(e).__name__, e))

# --------------------------------------------------------------------------
# Part 28: settings grouped by owning module (macast/module_settings.py)
#
# The 高级设置 tab answers "what is stored"; this answers "who owns it". Two
# failure modes matter: a key silently missing from every group (the user
# cannot find where a setting lives), and the label map drifting from what
# the modules actually persist -- which is why the map is checked against
# every SettingProperty in the repo, not just against fixtures.
# --------------------------------------------------------------------------
print("\n=== Part 28: module settings ===")
try:
    import re as _re28
    from enum import Enum as _Enum28
    module_settings = (sys.modules.get("macast.module_settings")
                       or _load("module_settings", "module_settings.py"))

    _tmp28 = _tempfile.mkdtemp(prefix="macast-modsettings-")
    _saved_setting28 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_running28 = utils.Setting.is_service_running
    try:
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp28, "macast_setting.json")
        utils.Setting.is_service_running = staticmethod(lambda: True)

        # -- build_groups --------------------------------------------------
        _sm_mod = types.ModuleType("alias_x")
        _sm_mod.__file__ = os.path.join(MACAST, "plugins", "renderer", "screen_mirror.py")
        _sm_mod.SettingProperty = _Enum28(
            "SettingProperty", {"Mirror_Quality": 1, "Mirror_Target": 2})
        _raop_old = types.ModuleType("alias_y")
        _raop_old.__file__ = os.path.join(MACAST, "plugins", "protocol", "raop.py")  # v0.1: no enum
        _weird = types.ModuleType("alias_z")
        _weird.__file__ = "/somewhere/weird_thing.py"
        _weird.SettingProperty = _Enum28("SettingProperty", {"Third_Party_Key": 1})
        _plug_sm = types.SimpleNamespace(title="Screen Mirror", version="0.6",
                                         module=_sm_mod)
        _plug_raop = types.SimpleNamespace(title="AirPlay Audio (RAOP)",
                                           version="0.1", module=_raop_old)
        _plug_wd = types.SimpleNamespace(title="Weird", version="1", module=_weird)
        _stored28 = {"Mirror_Quality": "1080p", "Blocked_Interfaces": ["en9"],
                     "Made_Up_Key": "x"}
        _groups = module_settings.build_groups(
            [_plug_sm, _plug_raop, _plug_wd], _stored28)
        _ids = [g["id"] for g in _groups]
        check("core and player lead the groups; leftovers close them",
              _ids[0] == "core" and _ids[1] == "player"
              and _ids[-1] == "other", str(_ids))
        _g_sm = next(g for g in _groups if g["id"] == "plugin:screen_mirror")
        _k_q = next(i for i in _g_sm["keys"] if i["key"] == "Mirror_Quality")
        check("a known plugin's keys get their Chinese labels and hints",
              _k_q["label"] == module_settings.PLUGIN_LABELS[
                  "screen_mirror"]["Mirror_Quality"][0]
              and _k_q["hint"] and _k_q["present"] is True
              and _k_q["value"] == "1080p" and _k_q["editable"] is True,
              str(_k_q))
        _k_t = next(i for i in _g_sm["keys"] if i["key"] == "Mirror_Target")
        check("a declared key that was never persisted shows as 未设置, still writable",
              _k_t["present"] is False and _k_t["editable"] is True)
        _g_raop = next(g for g in _groups if g["id"] == "plugin:raop")
        check("a plugin without persisted settings still gets a card (empty, not hidden)",
              _g_raop["keys"] == [], str(_g_raop))
        _g_wd = next(g for g in _groups if g["id"] == "plugin:weird_thing")
        check("an unknown plugin's own enum is listed verbatim",
              [i["key"] for i in _g_wd["keys"]] == ["Third_Party_Key"]
              and _g_wd["keys"][0]["label"] == "Third_Party_Key", str(_g_wd))
        _g_other = next(g for g in _groups if g["id"] == "other")
        check("a key nobody owns lands in 其他（未归类）",
              [i["key"] for i in _g_other["keys"]] == ["Made_Up_Key"],
              str(_g_other))
        _g_core = _groups[0]
        _k_bi = next(i for i in _g_core["keys"] if i["key"] == "Blocked_Interfaces")
        check("list/dict values are marked not editable here (JSON tab owns them)",
              _k_bi["editable"] is False and _k_bi["present"] is True)
        _all_keys = [i["key"] for g in _groups for i in g["keys"]]
        check("no key is claimed by two groups",
              len(_all_keys) == len(set(_all_keys)),
              str([k for k in set(_all_keys) if _all_keys.count(k) > 1]))

        # -- set_value ------------------------------------------------------
        _r = module_settings.set_value("Not_Owned_At_All", "1")
        check("writing an unowned key is refused", _r["code"] == 1, str(_r))
        _r = module_settings.set_value("Blocked_Interfaces", '["en0"]')
        check("a JSON container is refused (structure belongs to the JSON tab)",
              _r["code"] == 1, str(_r))
        _r = module_settings.set_value("PlayerSize", "2")
        check("a scalar saves, with JSON types honoured, and persists to disk",
              _r["code"] == 0 and utils.Setting.setting.get("PlayerSize") == 2
              and json.load(open(utils.Setting.setting_path,
                                 encoding="utf-8"))["PlayerSize"] == 2, str(_r))
        _r = module_settings.set_value("DLNA_FriendlyName", "客厅的小屏")
        check("unquoted text stays a plain string",
              _r["code"] == 0
              and utils.Setting.setting.get("DLNA_FriendlyName") == "客厅的小屏",
              str(_r))
        _r = module_settings.set_value("PlayerSize", "", remove=True)
        _r2 = module_settings.set_value("PlayerSize", "", remove=True)
        check("remove deletes the key and tolerates an absent one",
              _r["code"] == 0 and _r["removed"] is True
              and "PlayerSize" not in utils.Setting.setting
              and _r2["code"] == 0 and _r2["removed"] is False,
              "{} {}".format(_r, _r2))
        # A plugin key is only owned while a manager actually has that plugin
        # loaded -- prove the bus path, not just the static maps.
        _mgr28 = types.SimpleNamespace(renderer_all=[_plug_sm], protocol_list=[])
        _get_mgr28 = lambda: _mgr28                                  # noqa: E731
        cherrypy.engine.subscribe("get_plugin_manager", _get_mgr28)
        try:
            _r = module_settings.set_value("Mirror_Target", '"192.168.1.40:8009"')
            check("a loaded plugin's key is writable through the manager",
                  _r["code"] == 0
                  and utils.Setting.setting.get("Mirror_Target")
                  == "192.168.1.40:8009", str(_r))
        finally:
            try:
                cherrypy.engine.unsubscribe("get_plugin_manager", _get_mgr28)
            except Exception:
                pass

        # -- the HTTP surface ----------------------------------------------
        class _MsHandler28(protocol.Handler):
            def __init__(self):
                pass

            @property
            def protocol(self):
                return types.SimpleNamespace()

        handler28 = _MsHandler28()
        from cherrypy import serving as _serving28
        request28 = _serving28.request
        _saved28 = (request28.params, getattr(request28, "remote", None),
                    request28.scheme, dict(request28.headers))
        try:
            def _reset28(ip="127.0.0.1"):
                request28.headers.clear()
                for _k, _v in _saved28[3].items():
                    request28.headers[_k] = _v
                request28.params = {}
                request28.remote = types.SimpleNamespace(ip=ip)
                request28.scheme = "http"

            def _get28(**kw):
                return json.loads(handler28.GET(param="api", **kw).decode())

            def _post28(**kw):
                return json.loads(handler28.POST(**kw).decode())

            _reset28(ip="192.168.1.9")
            check("module-settings is a management query: loopback or token",
                  _get28(query="module-settings").get("code") == 403
                  and _post28(**{"set-module-setting": "1",
                                 "key": "PlayerSize", "value": "1"}).get("code") == 403)
            _reset28()
            _res = _get28(query="module-settings")
            check("the page gets groups with labels from one call",
                  _res.get("code") == 0
                  and {g["id"] for g in _res.get("groups", [])} >= {"core", "player"},
                  str(_res)[:140])
            _res = _post28(**{"set-module-setting": "1",
                              "key": "PlayerOntop", "value": "0"})
            check("set-module-setting persists through the API",
                  _res.get("code") == 0
                  and utils.Setting.setting.get("PlayerOntop") == 0, str(_res))
            _res = _post28(**{"set-module-setting": "1",
                              "key": "Nope", "value": "1"})
            check("the API refuses unowned keys too", _res.get("code") == 1,
                  str(_res))
        finally:
            (request28.params, request28.remote, request28.scheme, _) = _saved28
            request28.headers.clear()
            for _k, _v in _saved28[3].items():
                request28.headers[_k] = _v

        # -- the label map against the repo's real modules -------------------
        def _enum_block_names(src):
            marker = "class SettingProperty(Enum):"
            if marker not in src:
                return None
            names = []
            for ln in src.split(marker, 1)[1].splitlines():
                if ln.strip() == "":
                    continue
                if not (ln.startswith(" ") or ln.startswith("\t")):
                    break
                if ln.strip().startswith("#"):
                    continue
                m = _re28.match(r"([A-Za-z_]\w*)\s*=\s*", ln.strip())
                if m:
                    names.append(m.group(1))
            return names

        def _setting_keys_used(src):
            # `.name` is the persisted key, and the only thing that persists a
            # key is the first argument of Setting.get/set/has/unset. Bare
            # references elsewhere are value comparisons (mpv.py compares
            # `setting_player_hw != SettingProperty.PlayerHW_Disable`), not
            # keys; and constants always appear with `.value` appended.
            return {m.group(1) for m in _re28.finditer(
                r"Setting\.(?:get|set|has|unset)\(\s*"
                r"SettingProperty\.([A-Za-z_]\w*)(\s*\.value)?", src)
                if not m.group(2)}

        _allowed28 = set(module_settings.CORE_LABELS) | set(
            module_settings.PLAYER_LABELS)
        for _m28 in module_settings.PLUGIN_LABELS.values():
            _allowed28 |= set(_m28)
        _scan28 = ([("macast/utils.py", "core"), ("macast_renderer/mpv.py", "player")]
                   + [("plugins/" + _f, _f[:-3]) for _f in sorted(
                      os.listdir(os.path.join(REPO, "plugins")))
                      if _f.endswith(".py") and _f != "__init__.py"]
                   + [("macast/plugins/" + _k + "/" + _f, _f[:-3])
                      for _k in ("renderer", "protocol")
                      for _f in sorted(os.listdir(
                          os.path.join(MACAST, "plugins", _k)))
                      if _f.endswith(".py") and not _f.startswith("__")])
        _drift28 = []
        for _rel, _base in _scan28:
            with open(os.path.join(REPO, _rel), "r", encoding="utf-8") as _f:
                _src28 = _f.read()
            if "SettingProperty." not in _src28:
                continue
            _missing = _setting_keys_used(_src28) - _allowed28
            if _missing:
                _drift28.append("{}: {}".format(_rel, sorted(_missing)))
        check("every persisted setting key in the repo has a label",
              not _drift28, "; ".join(_drift28))
        _ghost28 = []
        for _base, _labels in module_settings.PLUGIN_LABELS.items():
            _files = [os.path.join(REPO, "plugins", _base + ".py")]
            for _k in ("renderer", "protocol"):
                _files.append(os.path.join(MACAST, "plugins", _k, _base + ".py"))
            _hit = [p for p in _files if os.path.isfile(p)]
            if not _hit:
                _ghost28.append("{}: no plugin file".format(_base))
                continue
            for _p in _hit:
                with open(_p, "r", encoding="utf-8") as _f:
                    _names28 = set(_enum_block_names(_f.read()) or [])
                _bad = set(_labels) - _names28
                if _bad:
                    _ghost28.append("{}: {}".format(_base, sorted(_bad)))
        check("no label points at a key its plugin does not declare",
              not _ghost28, "; ".join(_ghost28))
        with open(os.path.join(MACAST, "utils.py"), "r", encoding="utf-8") as _f:
            _core_names28 = set(_enum_block_names(_f.read()) or [])
        check("the core label map covers exactly the core enum",
              _core_names28 == set(module_settings.CORE_LABELS),
              str(sorted(_core_names28 ^ set(module_settings.CORE_LABELS))))
        with open(os.path.join(REPO, "macast_renderer", "mpv.py"), "r",
                  encoding="utf-8") as _f:
            _mpv_names28 = set(_enum_block_names(_f.read()) or [])
        check("the player label map is a subset of mpv's enum (constants excluded)",
              set(module_settings.PLAYER_LABELS) <= _mpv_names28,
              str(set(module_settings.PLAYER_LABELS) - _mpv_names28))

        # -- wiring -----------------------------------------------------------
        with open(os.path.join(MACAST, "protocol.py"), "r", encoding="utf-8") as _f:
            _proto_src28 = _f.read()
        _gate_at28 = _proto_src28.index("Sensitive management queries")
        _gate_txt28 = _proto_src28[_gate_at28:_proto_src28.index(
            "_management_allowed()", _gate_at28)]
        check("module-settings sits in the gated query list",
              "'module-settings'" in _gate_txt28, _gate_txt28)
        _mp28 = _proto_src28[_proto_src28.index("_MANAGEMENT_PARAMS = ("):]
        _mp28 = _mp28[:_mp28.index(")")]
        check("set-module-setting sits in the gated POST params",
              "set-module-setting" in _mp28, _mp28)
        with open(os.path.join(MACAST, "xml", "setting.html"), "r",
                  encoding="utf-8") as _f:
            _html28 = _f.read()
        check("the page carries the 模块设置 tab wired to both endpoints",
              'label="模块设置"' in _html28
              and "query=module-settings" in _html28
              and "set-module-setting" in _html28)
        check("module values render as text, never v-html",
              'v-html="' not in _html28)
        # The fake plugins above all carry `.module`, so only a source check
        # catches the real wiring gap: load_from_file must store the imported
        # module on the MacastPlugin, or every file-loaded plugin's card is
        # empty and its keys fall into 其他（未归类）.
        with open(os.path.join(MACAST, "macast.py"), "r", encoding="utf-8") as _f:
            _mgr_src28 = _f.read()
        check("load_from_file hands the imported module to the plugin",
              _mgr_src28.count("self.module = module") >= 3,
              str(_mgr_src28.count("self.module = module")))
        with open(os.path.join(MACAST, "module_settings.py"), "r",
                  encoding="utf-8") as _f:
            _ms_src28 = _f.read()
        check("the mpv plugin defers to the 播放器 group instead of doubling it",
              "'mpv'" in _ms_src28 and "plugin_module_name(module) == 'mpv'"
              in _ms_src28)
    finally:
        (utils.Setting.setting, utils.Setting.setting_path) = _saved_setting28
        utils.Setting.is_service_running = staticmethod(_saved_running28)
        _shutil.rmtree(_tmp28, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("module settings behave", False,
          "{}: {}".format(type(e).__name__, e))


# --------------------------------------------------------------------------
# Part 29: the assisted BlackHole install tells the truth
#
# The user-visible failure this version exists for: 一键设置 says "安装被取消了
# 吗？" when the real story is a stale pkgutil receipt (files gone, receipt
# kept, reinstall silently skipped by the old flow) or a pkg whose postinstall
# never restarts coreaudiod (driver on disk, device invisible forever). The
# step machine is the only place those two branches become *named* outcomes,
# so the tests here walk every branch with stubs -- and the loopback progress
# page gets real HTTP checks, because its credential rules are §4.8's.
#
# v0.9 adds the third root cause, which is the one the user actually hit
# ("重启后再点一键安装又要安装一遍，这样安装流程永远装不完"): the probe branch
# *printed* "安装会被跳过" and then fell through into meta/download/verify/
# install anyway. A skipped step must be skipped in the machine, not in a
# sentence -- and the two witnesses (avfoundation vs CoreAudio) have to be
# reported separately, because "CoreAudio has it, capture does not" is a
# microphone permission, which no amount of downloading fixes.
# --------------------------------------------------------------------------
print("\n=== Part 29: screen mirror v0.9 (one-click repair + progress page) ===")
try:
    import json as _json29
    import urllib.error as _urlerr29
    import urllib.request as _urlreq29

    _saved_setting29 = (utils.Setting.setting, utils.Setting.setting_path)
    _tmp29 = _tempfile.mkdtemp(prefix="macast-mirror29-")
    _opener29 = _urlreq29.build_opener(_urlreq29.ProxyHandler({}))
    mirror29 = _load_plugin("screen_mirror_plugin_v07", "screen_mirror.py")
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp29, "macast_setting.json")
    try:
        # -- the step machine itself ------------------------------------------
        _ids29 = [s[0] for s in mirror29.AUDIO_STEPS]
        check("the run plan names every branch of the repair, reload included",
              len(set(_ids29)) == len(_ids29) and
              set(('probe', 'download', 'verify', 'install', 'wait',
                   'reload', 'aggregate', 'output')) <= set(_ids29),
              str(_ids29))
        p29 = mirror29._SetupProgress()
        p29.enter('download')
        p29.sub('download', 0.5, '1 / 2 MB')
        p29.leave('download')
        p29.enter('download')  # a done step refuses re-entry
        _d29 = {s['id']: s for s in p29.snapshot()['steps']}
        check("a finished step never runs backwards",
              _d29['download']['state'] == 'done'
              and _d29['download']['pct'] == 1.0, str(_d29['download']))
        p29.skip('meta')
        p29.enter('verify')
        _snap29 = p29.snapshot()
        _w = (1.0 + 0.15) / 9  # 10 steps, 1 skipped out of the denominator
        check("skipped steps leave the denominator",
              _snap29['steps'][_ids29.index('meta')]['state'] == 'skipped'
              and abs(_snap29['pct'] - _w) < 1e-9, str(_snap29['pct']))
        p29.fail('verify', 'bad sha')
        p29.leave('verify', 'too late')
        _d29 = {s['id']: s for s in p29.snapshot()['steps']}
        check("fail wins over a late leave",
              _d29['verify']['state'] == 'fail', str(_d29['verify']))
        _null29 = mirror29._NullProgress()
        _null29.enter('x'); _null29.sub('x', 0.5); _null29.leave('x')
        _null29.skip('x'); _null29.fail('x'); _null29.finish(True)
        check("the no-op twin answers the whole surface, snapshot included",
              _null29.snapshot()['steps'] == [])

        # -- stubbed plumbing shared by the setup walk-throughs ---------------
        import types as _types29
        _types29_ns = _types29.SimpleNamespace

        class _Sub29(object):
            DEVNULL = -3
            PIPE = -1

            def __init__(self, results=None):
                self.calls = []
                self.results = results or []

            def run(self, cmd, **kwargs):
                self.calls.append(list(cmd))
                r = self.results.pop(0) if self.results else (0, '', '')
                return _types29_ns(returncode=r[0], stdout=r[1], stderr=r[2])

        def _rec_progress29():
            return mirror29._SetupProgress()

        _saved29 = {}

        def _stub29(overrides=None, **over):
            merged = dict(overrides or {})
            merged.update(over)
            for _name, _fn in merged.items():
                if _name not in _saved29:
                    _saved29[_name] = getattr(mirror29, _name)
                setattr(mirror29, _name, _fn)

        def _unstub29():
            for _name, _fn in _saved29.items():
                setattr(mirror29, _name, _fn)
            _saved29.clear()

        def _route_ok29(f, progress=None):
            # The real route marks these two steps itself; the stub has to,
            # or the "everything finished" percentage checks test the stub.
            progress.leave('aggregate', 'stub')
            progress.leave('output', 'stub')
            return True

        _base_stubs = dict(
            find_ffmpeg=lambda: 'ffmpeg',
            _has_blackhole=lambda f: False,
            _find_blackhole=lambda f: None,
            blackhole_pkg_pair=lambda: ('https://x/BH.pkg', 'a' * 64, 'stub'),
            _fetch_blackhole_pkg=lambda url, on_bytes=None: '/tmp/bh29.pkg',
            verify_blackhole_pkg=lambda path, sha: True,
            _wait_for_blackhole=lambda f, timeout=300.0, progress=None,
                                  interval=5.0, **kw: 'capturable',
            _route_audio_through_blackhole=_route_ok29,
            _open_audio_midi_setup=lambda rep: None,
            _blackhole_driver_installed=lambda: False,
            _blackhole_receipt_present=lambda: False,
            _reload_coreaudiod=lambda: (False, 'stub'),
            subprocess=_Sub29())

        # -- branch: device already there -> install steps skipped ------------
        _stub29(dict(_base_stubs, _has_blackhole=lambda f: True))
        try:
            p29 = _rec_progress29()
            _ok29 = mirror29.setup_system_audio(lambda m: None, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("a healthy machine skips straight to routing",
                  _ok29 and _d29['download']['state'] == 'skipped'
                  and _d29['reload']['state'] == 'skipped'
                  and _d29['output']['state'] == 'done'
                  and p29.snapshot()['pct'] == 1.0,
                  str(_d29))
        finally:
            _unstub29()

        # -- branch: stale pkgutil receipt -> names the real cause ------------
        _fetched29 = []
        _sub29 = _Sub29()
        _stub29(dict(_base_stubs,
                     _blackhole_receipt_present=lambda: True,
                     _fetch_blackhole_pkg=(
                         lambda url, on_bytes=None:
                         _fetched29.append(url) or '/tmp/bh29.pkg'),
                     subprocess=_sub29))
        try:
            p29 = _rec_progress29()
            _ok29 = mirror29.setup_system_audio(lambda m: None, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("the stale-receipt machine is told it will reinstall",
                  _ok29 and '残留' in _d29['probe']['note']
                  and _fetched29 == ['https://x/BH.pkg']
                  and ['open', '/tmp/bh29.pkg'] in _sub29.calls,
                  "{} / {}".format(_d29['probe']['note'], _sub29.calls))
        finally:
            _unstub29()

        # -- branch: files on disk, coreaudiod never loaded them -> reload ----
        #
        # THIS is the case the user's "永远装不完" report is about, and the old
        # flow passed the old version of this test while doing the opposite of
        # what its own probe note promised: it announced "安装会被跳过" and then
        # fell straight through into meta/download/verify/install. So the
        # assertions here are about what does *not* happen -- no fetch, no
        # `open`, and the wait step stays skipped.
        _waits29 = []
        _fetched_on_disk29 = []
        _sub29 = _Sub29()

        def _wait_after_reload29(f, timeout=300.0, progress=None, interval=5.0,
                                 **kw):
            _waits29.append((timeout, kw.get('step')))
            return 'capturable'

        _stub29(dict(_base_stubs,
                     _blackhole_driver_installed=lambda: True,
                     _wait_for_blackhole=_wait_after_reload29,
                     _fetch_blackhole_pkg=(
                         lambda url, on_bytes=None:
                         _fetched_on_disk29.append(url) or '/tmp/bh29.pkg'),
                     _reload_coreaudiod=lambda: (True, '音频服务已重载'),
                     subprocess=_sub29))
        try:
            p29 = _rec_progress29()
            _msgs29 = []
            _ok29 = mirror29.setup_system_audio(_msgs29.append, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("a driver that is on disk is never downloaded or opened again",
                  _ok29 and _fetched_on_disk29 == []
                  and not [c for c in _sub29.calls if c[0] == 'open']
                  and all(_d29[s]['state'] == 'skipped'
                          for s in ('meta', 'download', 'verify', 'install',
                                    'wait'))
                  and _d29['reload']['state'] == 'done'
                  and _d29['output']['state'] == 'done',
                  str(_d29) + ' / ' + str(_sub29.calls))
            check("the skipped install is explained by the daemon, not by a reboot",
                  'postinstall' in _d29['probe']['note']
                  and '跳过下载与安装' in _d29['probe']['note']
                  and '重启一次' not in _d29['probe']['note'],
                  _d29['probe']['note'])
            check("the one reload wait is the 45 s one and it ticks on 'reload'",
                  _waits29 == [(45.0, 'reload')], str(_waits29))
        finally:
            _unstub29()

        # -- branch: reload refused (password cancelled) -> honest stop -------
        _fetched_refused29 = []
        _sub29 = _Sub29()
        _stub29(dict(_base_stubs,
                     _blackhole_driver_installed=lambda: True,
                     _wait_for_blackhole=lambda f, timeout=300.0,
                                           progress=None, interval=5.0,
                                           **kw: '',
                     _fetch_blackhole_pkg=(
                         lambda url, on_bytes=None:
                         _fetched_refused29.append(url) or '/tmp/bh29.pkg'),
                     _reload_coreaudiod=lambda: (False, '你取消了密码框'),
                     subprocess=_sub29))
        try:
            p29 = _rec_progress29()
            _msgs29 = []
            _ok29 = mirror29.setup_system_audio(_msgs29.append, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("a cancelled reload fails reload, not a bogus timeout",
                  not _ok29 and _d29['reload']['state'] == 'fail'
                  and '密码框' in _d29['reload']['note']
                  and _d29['wait']['state'] == 'skipped'
                  and _d29['aggregate']['state'] == 'pending',
                  str(_msgs29))
            check("a cancelled reload promises no second download and never "
                  "opens the installer",
                  _fetched_refused29 == []
                  and any('不会重新下载' in m for m in _msgs29)
                  and not [c for c in _sub29.calls if c[0] == 'open'],
                  str(_msgs29))
        finally:
            _unstub29()

        # -- branch: CoreAudio sees it, the capture side does not -> no install
        _sub29 = _Sub29()
        _reloads29 = []
        _stub29(dict(_base_stubs,
                     _find_blackhole=lambda f: (8, 'BlackHole2ch-uid'),
                     _blackhole_driver_installed=lambda: True,
                     _fetch_blackhole_pkg=(
                         lambda url, on_bytes=None: '/tmp/never29.pkg'),
                     _reload_coreaudiod=lambda: _reloads29.append('x') or
                                                (True, '不该被调用'),
                     subprocess=_sub29))
        try:
            p29 = _rec_progress29()
            _msgs29 = []
            _ok29 = mirror29.setup_system_audio(_msgs29.append, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("a loaded-but-uncapturable device installs nothing and "
                  "reloads nothing",
                  not _ok29
                  and all(_d29[s]['state'] == 'skipped'
                          for s in ('meta', 'download', 'verify', 'install',
                                    'wait', 'reload'))
                  and _reloads29 == []
                  and not [c for c in _sub29.calls if c[0] in ('open',
                                                               'osascript')],
                  str(_d29))
            check("that run still builds the aggregate and blames the right "
                  "thing",
                  _d29['aggregate']['state'] == 'done'
                  and _d29['output']['state'] == 'done'
                  and _d29['probe']['state'] == 'fail'
                  and any('麦克风' in m for m in _msgs29)
                  and any('不会重新下载安装包' in m for m in _msgs29),
                  str(_msgs29))
        finally:
            _unstub29()

        # -- _blackhole_state: the precedence between the two witnesses -------
        _stub29(_has_blackhole=lambda f: True,
                _find_blackhole=lambda f: (8, 'BlackHole2ch-uid'),
                _blackhole_driver_installed=lambda: True)
        try:
            check("capture beats every other witness: it is the only one that "
                  "means audio will flow",
                  mirror29._blackhole_state('ffmpeg') == 'capturable')
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: False,
                _find_blackhole=lambda f: (8, 'BlackHole2ch-uid'),
                _blackhole_driver_installed=lambda: True,
                _blackhole_receipt_present=lambda: True)
        try:
            check("a device CoreAudio knows about outranks the files on disk",
                  mirror29._blackhole_state('ffmpeg') == 'loaded')
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: False,
                _find_blackhole=lambda f: None,
                _blackhole_driver_installed=lambda: True,
                _blackhole_receipt_present=lambda: True)
        try:
            check("files on disk outrank the receipt that lied about them",
                  mirror29._blackhole_state('ffmpeg') == 'on-disk')
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: False,
                _find_blackhole=lambda f: None,
                _blackhole_driver_installed=lambda: False,
                _blackhole_receipt_present=lambda: True)
        try:
            check("receipt without files is its own answer, not 'absent'",
                  mirror29._blackhole_state('ffmpeg') == 'stale-receipt')
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: False,
                _find_blackhole=lambda f: None,
                _blackhole_driver_installed=lambda: False,
                _blackhole_receipt_present=lambda: False)
        try:
            check("nothing anywhere is 'absent', and the five answers are the "
                  "five the flow branches on",
                  mirror29._blackhole_state('ffmpeg') == 'absent'
                  and set(mirror29.BH_STATES) == set(
                      ('capturable', 'loaded', 'on-disk', 'stale-receipt',
                       'absent')))
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: (_ for _ in ()).throw(OSError('tcc')),
                _find_blackhole=lambda f: (_ for _ in ()).throw(OSError('nope')),
                _blackhole_driver_installed=lambda: False,
                _blackhole_receipt_present=lambda: False)
        try:
            check("a witness that raises is survived, not propagated",
                  mirror29._blackhole_state('ffmpeg') == 'absent')
        finally:
            _unstub29()

        # -- branch: sha mismatch -> the pkg never reaches `open` -------------
        _sub29 = _Sub29()
        _stub29(dict(_base_stubs, verify_blackhole_pkg=lambda path, sha: False,
                     subprocess=_sub29))
        try:
            p29 = _rec_progress29()
            _ok29 = mirror29.setup_system_audio(lambda m: None, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("an unverifiable pkg aborts before the installer",
                  not _ok29 and _d29['verify']['state'] == 'fail'
                  and not [c for c in _sub29.calls if c[0] == 'open'],
                  str(_sub29.calls))
        finally:
            _unstub29()

        # -- branch: a crash fails exactly the step that was running ----------
        def _boom29(url, on_bytes=None):
            raise OSError('disk on fire')

        _stub29(dict(_base_stubs, _fetch_blackhole_pkg=_boom29))
        try:
            p29 = _rec_progress29()
            _msgs29 = []
            _ok29 = mirror29.setup_system_audio(_msgs29.append, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("an unexpected error lands on the running step, not all of them",
                  not _ok29 and _d29['download']['state'] == 'fail'
                  and _d29['probe']['state'] == 'done'
                  and any('disk on fire' in m for m in _msgs29),
                  str(_msgs29))
        finally:
            _unstub29()

        # -- the download tick reaches the page -------------------------------
        def _fetch_tick29(url, on_bytes=None):
            if on_bytes:
                on_bytes(1024 * 1024, 4 * 1024 * 1024)
            return '/tmp/bh29.pkg'

        _stub29(dict(_base_stubs, _fetch_blackhole_pkg=_fetch_tick29))
        try:
            p29 = _rec_progress29()
            _ok29 = mirror29.setup_system_audio(lambda m: None, progress=p29)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("byte callbacks land while the download runs, then it closes",
                  _ok29 and _d29['download']['state'] == 'done', str(_d29))
        finally:
            _unstub29()

        # -- _pkg_tmpdir survives a vanished CLI-sandbox TMPDIR ----------------
        _real_mkdtemp29 = _tempfile.mkdtemp
        _mkcalls29 = []

        def _mkdtemp29(*a, **k):
            _mkcalls29.append(k.get('dir'))
            if k.get('dir') is None:
                raise OSError(2, 'No such file or directory')
            return _real_mkdtemp29(*a, **k)

        _saved_home29 = os.environ.get('HOME')
        _tempfile.mkdtemp = _mkdtemp29
        os.environ['HOME'] = _tmp29
        try:
            _dir29 = mirror29._pkg_tmpdir()
            check("a vanished sandbox TMPDIR falls back to the user cache dir",
                  _mkcalls29[0] is None and len(_mkcalls29) == 2
                  and _dir29.startswith(os.path.join(_tmp29, 'Library'))
                  and os.path.isdir(_dir29), str(_mkcalls29))
        finally:
            _tempfile.mkdtemp = _real_mkdtemp29
            os.environ['HOME'] = _saved_home29

        # -- _wait_for_blackhole ticks its own step ----------------------------
        _stub29(_has_blackhole=lambda f: False,
                _find_blackhole=lambda f: None,
                _blackhole_driver_installed=lambda: False,
                _blackhole_receipt_present=lambda: False)
        try:
            p29 = _rec_progress29()
            p29.enter('wait')
            _hit29 = mirror29._wait_for_blackhole('ffmpeg', timeout=0.05,
                                                  progress=p29, interval=0.01)
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("the wait step says how long it has been waiting",
                  not _hit29 and '已等' in _d29['wait']['note']
                  and 0.0 < (_d29['wait']['pct'] or 0) <= 0.99,
                  str(_d29['wait']))
            # The same waiter drives the reload poll now, so the tick has to
            # follow whichever step the caller named -- not 'wait' forever.
            p29 = _rec_progress29()
            p29.enter('reload')
            _hit29 = mirror29._wait_for_blackhole('ffmpeg', timeout=0.05,
                                                  progress=p29, interval=0.01,
                                                  step='reload')
            _d29 = {s['id']: s for s in p29.snapshot()['steps']}
            check("a poll can be pointed at another step, and the timeout "
                  "answer is empty rather than False-with-a-lie",
                  _hit29 == '' and '已等' in _d29['reload']['note']
                  and _d29['wait']['note'] == '', str(_d29))
        finally:
            _unstub29()

        # -- the two witnesses stay apart through the poll ---------------------
        _stub29(_has_blackhole=lambda f: False,
                _blackhole_driver_installed=lambda: False,
                _blackhole_receipt_present=lambda: False,
                _find_blackhole=lambda f: (8, 'BlackHole2ch-uid'))
        try:
            check("a device only CoreAudio sees is reported as 'loaded', "
                  "never as a successful wait",
                  mirror29._wait_for_blackhole('ffmpeg', timeout=0.05,
                                               interval=0.01) == 'loaded')
        finally:
            _unstub29()
        _stub29(_has_blackhole=lambda f: True)
        try:
            check("the capture witness is the one that answers 'capturable'",
                  mirror29._wait_for_blackhole('ffmpeg', timeout=0.05,
                                               interval=0.01) == 'capturable')
        finally:
            _unstub29()

        # -- blackhole_pkg_pair: the cask API stays the authority --------------
        import requests as _req29
        _real_get29 = _req29.get
        try:
            def _raise29(*a, **k):
                raise RuntimeError('unreachable')

            _req29.get = _raise29
            _u29, _s29, _src29 = mirror29.blackhole_pkg_pair()
            check("an unreachable cask API falls back to the pinned pair, labelled",
                  (_u29, _s29) == (mirror29.BLACKHOLE_PKG_URL,
                                   mirror29.BLACKHOLE_PKG_SHA256)
                  and '不可达' in _src29, _src29)

            _req29.get = lambda *a, **k: _types29.SimpleNamespace(
                content=b'{"url":"https://x/BH-0.9.0.pkg",'
                        b'"sha256":"' + b'c' * 64 + b'"}')
            _u29, _s29, _src29 = mirror29.blackhole_pkg_pair()
            check("a usable cask API wins, and the page says so",
                  _u29.endswith('0.9.0.pkg') and _s29 == 'c' * 64
                  and 'Homebrew' in _src29, _src29)
        finally:
            _req29.get = _real_get29

        # -- existential.audio answers 406 to the bare python-requests UA -----
        class _Resp29(object):
            headers = {'Content-Length': '3'}

            def raise_for_status(self):
                pass

            def iter_content(self, n):
                yield b'pkg'

            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        _dl_hdrs29 = {}
        try:
            _req29.get = lambda url, **k: (_dl_hdrs29.update(k.get('headers')
                                                             or {}),
                                           _Resp29())[1]
            _pkgpath29 = mirror29._fetch_blackhole_pkg('https://x/BH.pkg')
            check("the pkg download presents a browser UA (existential.audio 406s the default)",
                  _dl_hdrs29.get('User-Agent', '').startswith('Mozilla/5.0')
                  and os.path.getsize(_pkgpath29) == 3, str(_dl_hdrs29))
            mirror29._remove_quietly(_pkgpath29)
        finally:
            _req29.get = _real_get29

        # -- driver-file and receipt probes read reality, not assumptions ------
        _probe_dir29 = _tempfile.mkdtemp(prefix="macast-hal29-", dir=_tmp29)
        _real_globs29 = mirror29.HAL_DRIVER_GLOBS
        try:
            mirror29.HAL_DRIVER_GLOBS = (
                os.path.join(_probe_dir29, 'BlackHole*.driver'),)
            check("no driver directory means not installed",
                  mirror29._blackhole_driver_installed() is False)
            os.makedirs(os.path.join(_probe_dir29, 'BlackHole2ch.driver'))
            check("the driver directory is what 'files on disk' means",
                  mirror29._blackhole_driver_installed() is True)
        finally:
            mirror29.HAL_DRIVER_GLOBS = _real_globs29
        _sub29 = _Sub29(results=[(0, '\n'.join(
            ['com.apple.coreaudio', 'audio.existential.BlackHole2ch']), '')])
        _stub29(subprocess=_sub29)
        try:
            check("pkgutil's list is read by exact package id",
                  mirror29._blackhole_receipt_present() is True)
        finally:
            _unstub29()
        _sub29 = _Sub29(results=[(127, '', 'User canceled (-128)')])
        _stub29(subprocess=_sub29)
        try:
            _ok29, _why29 = mirror29._reload_coreaudiod()
            check("a dismissed admin prompt is reported as a cancellation",
                  _ok29 is False and '取消' in _why29, _why29)
        finally:
            _unstub29()
        _sub29 = _Sub29(results=[(0, '', '')])
        _stub29(subprocess=_sub29)
        try:
            _ok29, _why29 = mirror29._reload_coreaudiod()
            check("the reload asks for authorization instead of faking silence",
                  _ok29 is True and _sub29.calls
                  and _sub29.calls[0][0] == 'osascript'
                  and 'with administrator privileges' in _sub29.calls[0][2]
                  and 'coreaudiod' in _sub29.calls[0][2],
                  str(_sub29.calls))
        finally:
            _unstub29()

        # -- route failures name the step that actually broke ------------------
        _stub29(_audio_devices=lambda: [],
                _find_blackhole=lambda f: None,
                _default_output=lambda: None,
                _set_default_output=lambda d: True,
                _create_aggregate=lambda uids: None)
        try:
            p29 = _rec_progress29()
            check("no CoreAudio devices fails the aggregate step itself",
                  mirror29._route_audio_through_blackhole('ffmpeg',
                                                          progress=p29) is False
                  and {s['id']: s['state']
                       for s in p29.snapshot()['steps']}['aggregate'] == 'fail')
        finally:
            _unstub29()
        utils.Setting.set(mirror29.SettingProperty.Mirror_Audio_Aggregate, 99)
        _stub29(_audio_devices=lambda: [(99, 'macast-agg'), (8, 'bh-uid')],
                _find_blackhole=lambda f: (8, 'bh-uid'),
                _default_output=lambda: 7,
                _set_default_output=lambda d: False,
                _create_aggregate=lambda uids: None)
        try:
            p29 = _rec_progress29()
            _ok29 = mirror29._route_audio_through_blackhole('ffmpeg',
                                                            progress=p29)
            _d29 = {s['id']: s['state'] for s in p29.snapshot()['steps']}
            check("a failed default-output switch is blamed on output, not aggregate",
                  not _ok29 and _d29['aggregate'] == 'done'
                  and _d29['output'] == 'fail', str(_d29))
        finally:
            _unstub29()
            utils.Setting.unset(mirror29.SettingProperty.Mirror_Audio_Aggregate)
            utils.Setting.unset(mirror29.SettingProperty.Mirror_Audio_Original)

        # -- the progress page: real HTTP, real credential rules ---------------
        _sub29 = _Sub29()
        _stub29(subprocess=_sub29)
        p29 = _rec_progress29()
        p29.enter('download')
        p29.sub('download', 0.25, '1 / 4 MB')
        _url29, _srv29 = None, None
        try:
            _url29, _srv29 = mirror29._open_audio_progress(p29)
            mirror29._audio_server = _srv29  # _close_audio_server works off the global
            _token29 = _url29.split('token=')[1]

            def _get29(path):
                try:
                    with _opener29.open(_url29.rsplit('/', 1)[0] + path,
                                        timeout=5) as _r29:
                        return _r29.getcode(), dict(_r29.headers), _r29.read()
                except _urlerr29.HTTPError as _e29:
                    return _e29.code, dict(_e29.headers), _e29.read()

            _code29, _hdr29, _body29 = _get29('/?token=' + _token29)
            _page29 = _body29.decode('utf-8')
            check("the page opens only with the run token",
                  _code29 == 200 and '一键设置进度' in _page29
                  and _hdr29.get('X-Content-Type-Options') == 'nosniff'
                  and _hdr29.get('Cache-Control') == 'no-store', str(_code29))
            _code29, _hdr29, _body29 = _get29('/state?token=' + _token29)
            _st29 = _json29.loads(_body29.decode('utf-8'))
            check("/state mirrors the live step machine",
                  _code29 == 200 and len(_st29['steps']) == len(mirror29.AUDIO_STEPS)
                  and abs(_st29['pct'] - p29.snapshot()['pct']) < 1e-9,
                  str(_st29['pct']))
            _code29b, _, _ = _get29('/state?token=deadbeef')
            _code29c, _, _ = _get29('/state')
            check("wrong or missing tokens get 403 on both endpoints",
                  _code29b == 403 and _code29c == 403,
                  "{} / {}".format(_code29b, _code29c))
            check("the page is self-contained and never carries the management token",
                  '<script src' not in _page29
                  and 'Api_Token' not in _page29
                  and _token29 != utils.Setting.setting.get('Api_Token', ''),
                  '')
            mirror29._close_audio_server()
            try:
                _get29('/state?token=' + _token29)
                _closed29 = False
            except (_urlerr29.HTTPError, OSError):
                _closed29 = True
            check("the page stops being served once retired", _closed29)
        finally:
            mirror29._close_audio_server()
            _unstub29()

        # -- the worker: page first, then the run, then retirement ------------
        _setup_calls29 = []

        def _fake_setup29(report, progress=None):
            _setup_calls29.append(progress)
            report('stubbed setup ran')
            return True

        mirror29._audio_setup_busy.set()
        _stub29(subprocess=_Sub29(), setup_system_audio=_fake_setup29)
        try:
            mirror29._audio_setup_worker()
            _wp29 = mirror29._audio_progress
            check("the worker finishes the machine, clears busy and keeps the "
                  "page readable for a while",
                  len(_setup_calls29) == 1 and _setup_calls29[0] is _wp29
                  and _wp29.snapshot()['done'] and _wp29.snapshot()['ok'] is True
                  and not mirror29._audio_setup_busy.is_set()
                  and mirror29._audio_server is not None
                  and mirror29._audio_server_timer is not None,
                  str(_wp29.snapshot()))
        finally:
            mirror29._close_audio_server()
            mirror29._audio_setup_busy.clear()
            _unstub29()

        # -- wiring: what the menu says must be what the manifest says ----------
        # Written against one release number at first, which meant every bump
        # had to remember to edit this file -- and the interesting failure is a
        # *stale* label, so the check now reads the manifest and demands that no
        # other version appears in the plugin at all.
        with open(os.path.join(MACAST, "plugins", "renderer", "screen_mirror.py"), "r",
                  encoding="utf-8") as _f:
            _src29 = _f.read()
        import re as _re29
        _manifest29 = _re29.search(r'<macast\.version>([^<]*)</macast\.version>',
                                   _src29).group(1)
        # The state the page receives carries `'version': PLUGIN_VERSION`, and so
        # does the plugin card, so the number the user reads comes from one place.
        # A literal
        # `Screen Mirror v0.x` anywhere in the file would be a second, staler one.
        _declared29 = _re29.search(r"^PLUGIN_VERSION = '([^']*)'", _src29,
                                  _re29.M)
        _stray29 = set(_re29.findall(r"'Screen Mirror v([0-9][.\w]*)'", _src29))
        check("the plugin announces its manifest version everywhere the user reads it",
              _declared29 is not None and _declared29.group(1) == _manifest29
              and not _stray29,
              "manifest=%s, PLUGIN_VERSION=%s, literals=%s" % (
                  _manifest29, _declared29 and _declared29.group(1), sorted(_stray29)))
        check("the setup hands its progress object down the whole chain",
              "_route_audio_through_blackhole(ffmpeg, progress=progress)"
              in _src29 and "_wait_for_blackhole(ffmpeg, progress=progress)"
              in _src29 and "setup_system_audio(_report, progress=progress)"
              in _src29)
        # The other half of "装了却没有设备": a bundle that never declares the
        # microphone can be denied without ever prompting, and the plugin's
        # honest answer then reads like a broken driver. Packaging holds the
        # string, the plugin holds the wording -- so the guard reads both.
        with open(os.path.join(REPO, "scripts", "setup_py2app.py"), "r",
                  encoding="utf-8") as _f:
            _py2app29 = _f.read()
        check("the .app declares the microphone the system-audio tap needs",
              "'NSMicrophoneUsageDescription'" in _py2app29
              and 'NSMicrophoneUsageDescription' in _src29,
              'plist=%s plugin=%s' % ("'NSMicrophoneUsageDescription'" in _py2app29,
                                      'NSMicrophoneUsageDescription' in _src29))
    finally:
        mirror29._capture_cache.clear()
        (utils.Setting.setting, utils.Setting.setting_path) = _saved_setting29
        _shutil.rmtree(_tmp29, ignore_errors=True)
except Exception as e:
    import traceback
    traceback.print_exc()
    check("the assisted BlackHole install behaves", False,
          "{}: {}".format(type(e).__name__, e))


# --------------------------------------------------------------------------
# Part 30: what a bundled plugin is allowed to import
#
# `macast/plugins/*.py` are bundled built-in plugins loaded at startup, so a
# third-party import in one of them is a promise nobody is holding: the built
# artefacts carry exactly what `requirements/` declares (AGENTS.md §4.3/§4.4),
# and a user who installs a bare .py gets whatever their machine happens to
# already have. Until now the rule was prose -- and `screen_mirror`'s use of
# pyobjc's `Foundation` turned that prose into an argument about whether a
# transitive dependency counts as "Macast 自带的库". It was settled by naming
# pyobjc in requirements/darwin.txt instead of inheriting it from rumps, which is
# the first thing these checks hold the two files to.
# --------------------------------------------------------------------------
print("\n=== Part 30: bundled plugin imports ===")
try:
    import ast as _ast30
    import re as _re30

    def _norm30(name):
        return str(name).lower().replace('_', '-')

    def _dist_from_line(raw):
        """Distribution name behind one requirements line, or ''.

        Handles the two forms these files actually use: a plain requirement with
        a specifier, and a `git+https://...` URL (pystray and pyperclip), whose
        distribution name is the repository's.
        """
        line = raw.split('#', 1)[0].strip()
        if not line or line.startswith('-'):
            return ''
        if line.startswith('git+') or '://' in line:
            tail = line.rstrip('/').rsplit('/', 1)[-1].split('@')[-1]
            if tail.endswith('.git'):
                tail = tail[:-4]
            return _norm30(tail)
        return _norm30(_re30.split(r'[<>=!;\s]', line, 1)[0])

    def _requirements_dists():
        """Distribution names requirements/*.txt asks for, normalised."""
        names = set()
        req_dir = os.path.join(REPO, "requirements")
        for fname in sorted(os.listdir(req_dir)):
            if not fname.endswith(".txt"):
                continue
            with open(os.path.join(req_dir, fname), encoding="utf-8") as fh:
                for line in fh:
                    name = _dist_from_line(line)
                    if name:
                        names.add(name)
        return names

    def _declared_in(fname):
        """Distribution names one requirements file asks for."""
        names = set()
        with open(os.path.join(REPO, "requirements", fname), encoding="utf-8") as fh:
            for line in fh:
                name = _dist_from_line(line)
                if name:
                    names.add(name)
        return names

    def _import_roots(source, top_level_only=False):
        """Module names an import statement reaches for.

        `top_level_only` keeps out of function and class bodies, so it answers a
        different question: not "what may be imported here" but "what is
        imported the moment the file is loaded" -- which is what decides whether
        the plugin can load at all on another platform.
        """
        tree = _ast30.parse(source)
        out = set()

        def _take(node):
            if isinstance(node, _ast30.Import):
                out.update(a.name.split('.')[0] for a in node.names)
            elif isinstance(node, _ast30.ImportFrom) and node.level == 0 and node.module:
                out.add(node.module.split('.')[0])

        if not top_level_only:
            for node in _ast30.walk(tree):
                _take(node)
            return out
        stack = list(_ast30.iter_child_nodes(tree))
        while stack:
            node = stack.pop()
            _take(node)
            if isinstance(node, (_ast30.FunctionDef, _ast30.AsyncFunctionDef,
                                 _ast30.ClassDef)):
                continue
            stack.extend(_ast30.iter_child_nodes(node))
        return out

    _declared30 = _requirements_dists()
    # Modules that exist only on one platform, with the distribution that
    # provides them and the requirement file that must name it. A table rather
    # than a guess at what this machine has installed: on Linux, pyobjc is not
    # importable and `packages_distributions()` therefore cannot report it, so
    # the platform-conditional names have to be allowed by rule -- and the rule
    # is only honest because the file it points at really declares them.
    _PLATFORM_OPTIONAL = {
        'Foundation': ('pyobjc-framework-Cocoa', 'darwin.txt'),
        'objc': ('pyobjc-framework-Cocoa', 'darwin.txt'),
    }
    for _mod, (_dist, _rfile) in _PLATFORM_OPTIONAL.items():
        check("{} is allowed in plugins only because {} declares {}"
              .format(_mod, _rfile, _dist),
              _norm30(_dist) in _declared_in(_rfile),
              str(sorted(_declared_in(_rfile))))

    # The other kind of platform-conditional name: one that no build ships at
    # all, allowed because the plugin that reaches for it still works without
    # it. `potplayer` uses win32api/win32con only to read PotPlayer's install
    # path out of the Windows registry -- and it says so itself: "Absent key,
    # missing pywin32, or a path that no longer exists: all mean 'ask the next
    # source', not 'fail'", falling back to the user-configured path and then
    # the two default install locations. No build installs pywin32 (the Windows
    # job's pip line does not name it, and there is no windows requirements
    # file), so listing it in `_PLATFORM_OPTIONAL` above would mean pointing at
    # a requirements file no job ever installs -- §4.3's mistake with the sign
    # flipped.
    #
    # This table is a claim about the code, so the code is held to it: an entry
    # only counts if the import really is reached through a `try/except
    # ImportError` that binds the name to None. Without that, the table would
    # be a way to import anything by declaring nothing.
    _OPTIONAL_BY_FALLBACK = {
        'win32api': ('pywin32', 'renderer/potplayer.py'),
        'win32con': ('pywin32', 'renderer/potplayer.py'),
    }

    def _degrades_without(source, mod):
        """True iff every `import mod` here is guarded by an ImportError.

        Answers "can this file still load without the module": the import has to
        sit inside a `try` whose handlers catch ImportError and bind the name to
        None. potplayer's module-level try is the shape this encodes.
        """
        for _try in _ast30.walk(_ast30.parse(source)):
            if not isinstance(_try, _ast30.Try):
                continue
            if not any(isinstance(h.type, _ast30.Name)
                       and h.type.id == 'ImportError' for h in _try.handlers):
                continue
            _inside = set()
            for _child in _ast30.walk(_try):
                if isinstance(_child, _ast30.Import):
                    _inside.update(a.name.split('.')[0] for a in _child.names)
                elif (isinstance(_child, _ast30.ImportFrom)
                      and _child.level == 0 and _child.module):
                    _inside.add(_child.module.split('.')[0])
            if mod not in _inside:
                continue
            if any(isinstance(_a, _ast30.Assign)
                   and any(isinstance(t, _ast30.Name) and t.id == mod
                           for t in _a.targets)
                   and isinstance(_a.value, _ast30.Constant)
                   and _a.value.value is None
                   for _h in _try.handlers for _a in _ast30.walk(_h)):
                return True
        return False

    _hard30 = []
    for _mod, (_dist, _rel) in _OPTIONAL_BY_FALLBACK.items():
        with open(os.path.join(MACAST, "plugins", _rel), encoding="utf-8") as fh:
            _src30 = fh.read()
        if not _degrades_without(_src30, _mod):
            _hard30.append('{} ({})'.format(_mod, _rel))
    check("a module no build ships is allowed in a plugin only if the plugin "
          "degrades without it",
          not _hard30, str(_hard30))

    # ... and prove _degrades_without separates the two cases, so the check
    # above cannot pass by being unable to see an unguarded import.
    check("an unguarded import of such a module is what the rule above catches",
          _degrades_without("try:\n    import win32api\n"
                            "except ImportError:\n    win32api = None\n",
                            'win32api')
          and not _degrades_without("import win32api\n", 'win32api'),
          "guarded=%s bare=%s" % (
              _degrades_without("try:\n    import win32api\n"
                                "except ImportError:\n    win32api = None\n",
                                'win32api'),
              _degrades_without("import win32api\n", 'win32api')))

    try:
        from importlib.metadata import packages_distributions as _pdd30
        _mapped = {m for m, ds in _pdd30().items()
                   if any(_norm30(d) in _declared30 for d in ds)}
    except ImportError:  # pragma: no cover - Python < 3.8
        _mapped = set()
    _allowed30 = (set(sys.stdlib_module_names) | _mapped | set(_PLATFORM_OPTIONAL)
                  | set(_OPTIONAL_BY_FALLBACK)
                  | {'macast', 'macast_renderer'})

    def _illegal30(source):
        return sorted(r for r in _import_roots(source) if r not in _allowed30)

    _plugin_files30 = []
    for _k in ("renderer", "protocol"):
        _pd30 = os.path.join(MACAST, "plugins", _k)
        for _fname in sorted(os.listdir(_pd30)):
            if _fname.endswith(".py") and not _fname.startswith("__"):
                _plugin_files30.append(os.path.join(_pd30, _fname))
    _bad30 = {}
    for _fpath in _plugin_files30:
        _fname = os.path.basename(_fpath)
        with open(_fpath, encoding="utf-8") as fh:
            _offenders = _illegal30(fh.read())
        if _offenders:
            _bad30[_fname] = _offenders
    check("no bundled plugin imports a third-party module Macast does not declare",
          not _bad30, str(_bad30))
    # The other way round: prove the rule above is not vacuous, and that it
    # allows what it should allow. A plugin adding `import aiortc` is exactly
    # the case AGENTS.md §4.6 sends to the bundled-plugin route, and `cherrypy`
    # has to stay legal or every protocol plugin here would be reported.
    check("an undeclared pip import is what this catches, and a declared one is not",
          _illegal30("import aiortc\nimport pychromecast\n") == ['aiortc', 'pychromecast']
          and _illegal30("import cherrypy\nimport Foundation\nimport os\n") == [],
          str(_illegal30("import aiortc\nimport cherrypy\nimport Foundation\n")))

    # pyobjc has to stay a declared dependency, not one inherited from rumps:
    # macast/utils.py imports AppKit at module level, and the two names above are
    # the audio bridge's. §4.3 is the record of what silent inheritance does.
    _darwin30 = _declared_in('darwin.txt')
    check("darwin declares the framework behind Foundation itself",
          'pyobjc-framework-cocoa' in _darwin30, str(sorted(_darwin30)))

    # §4.3's lesson, made automatic rather than remembered: the macOS CI job
    # installs from its own inline list (`build_macos_arm.sh` at least reads
    # requirements/darwin.txt), and the last time the two disagreed the shipped
    # .app died at launch with a ModuleNotFoundError while source ran fine.
    _pip_words = []
    _gathering = False
    with open(os.path.join(REPO, ".github", "workflows", "build.yml"),
              encoding="utf-8") as fh:
        for _line in fh:
            if _gathering:
                _pip_words.append(_line)
                _gathering = _line.rstrip().endswith('\\')
            elif 'pip install' in _line:
                _pip_words.append(_line.split('pip install', 1)[1])
                _gathering = _line.rstrip().endswith('\\')
    _ci30 = set()
    for _tok in _re30.findall(r"['\"]([^'\"]+)['\"]", ''.join(_pip_words)):
        _ci30.add(_norm30(_re30.split(r'[<>=!~\s]', _tok, 1)[0].split('[', 1)[0]))
    _absent = sorted(_darwin30 - _ci30)
    check("the macOS CI job installs everything requirements/darwin.txt names",
          not _absent, "missing from build.yml: %s" % _absent)

    with open(os.path.join(MACAST, "utils.py"), encoding="utf-8") as fh:
        _utils_src = fh.read()
    _tree30 = _ast30.parse(_utils_src)
    _anchored = []
    for _node in _ast30.walk(_tree30):
        if isinstance(_node, _ast30.If) and 'darwin' in _ast30.dump(_node.test).lower():
            _inner = set()
            for _child in _ast30.walk(_node):
                if isinstance(_child, _ast30.Import):
                    _inner.update(a.name.split('.')[0] for a in _child.names)
                elif (isinstance(_child, _ast30.ImportFrom) and _child.level == 0
                      and _child.module):
                    _inner.add(_child.module.split('.')[0])
            if 'AppKit' in _inner:
                _anchored.append('AppKit')
    check("the core is what anchors pyobjc on macOS (utils.py imports AppKit "
          "under a darwin guard)", bool(_anchored),
          'module-level imports: %s' % sorted(_import_roots(_utils_src,
                                                            top_level_only=True)))

    # ... and the plugin keeps those names out of module scope, so a Windows or
    # Linux user can still load the file: the import is reached only from the
    # CoreAudio paths, and `from Foundation import ...` sits in a try/except.
    with open(os.path.join(MACAST, "plugins", "renderer", "screen_mirror.py"), encoding="utf-8") as fh:
        _mirror_src30 = fh.read()
    _hoisted = sorted(r for r in _import_roots(_mirror_src30, top_level_only=True)
                      if r in _PLATFORM_OPTIONAL)
    check("screen_mirror loads on every platform: no pyobjc import at module level",
          not _hoisted, str(_hoisted))
except Exception as _e30:
    import traceback
    traceback.print_exc()
    check("plugin import rules are checkable", False,
          "{}: {}".format(type(_e30).__name__, _e30))


# --------------------------------------------------------------------------
# Part 31: what `ffmpeg -list_devices` actually prints
#
# `screen_mirror` shipped four releases of a darwin capture probe that could
# never succeed on a real Mac. The parser split the device listing on
# 'Video devices:' -- capital V, while ffmpeg prints 'AVFoundation video
# devices:' -- and then kept only text sitting between double quotes, which
# ffmpeg never emits. On a real machine both lists came back empty,
# `_probe_avfoundation` returned None and mirroring refused to start with
# "no capturable screen". Parts 21/22/23 each fed that parser a fixture written
# in exactly that invented shape, so 1053 checks were green around a bug that
# made the feature unreachable on its primary platform. The first check below
# is this machine's ffmpeg, verbatim; the last one is the old parser, kept
# executable so the shape of the mistake stays on record; the one before it
# reads this file and refuses fixtures that repeat it.
# --------------------------------------------------------------------------
print("\n=== Part 31: the avfoundation device listing ===")
try:
    import re as _re31
    _saved_setting31 = (utils.Setting.setting, utils.Setting.setting_path)
    _saved_dir31 = utils.SETTING_DIR
    _tmp31 = _tempfile.mkdtemp(prefix="macast-mirror31-")
    _bin31 = os.path.join(_tmp31, "bin")
    _mirror31 = None
    os.makedirs(_bin31)
    try:
        utils.SETTING_DIR = _tmp31
        utils.Setting.setting = {}
        utils.Setting.setting_path = os.path.join(_tmp31, "macast_setting.json")

        _mirror31 = _load_plugin("screen_mirror_plugin_v08", "screen_mirror.py")
        _mirror31.invalidate_capture_cache()

        # Captured with
        #   ffmpeg -hide_banner -loglevel info -f avfoundation \
        #          -list_devices true -i ""
        # on the machine these tests run on. Every line but the last three is
        # what the parser has to survive: the log prefix, the lower-case block
        # names, indices in square brackets, names in Chinese, and ffmpeg
        # failing to open the (empty) input afterwards.
        _real31 = (
            '[AVFoundation indev @ 0x7ac1400140] AVFoundation video devices:\n'
            '[AVFoundation indev @ 0x7ac1400140] [0] OBS Virtual Camera\n'
            '[AVFoundation indev @ 0x7ac1400140] [1] FaceTime高清相机\n'
            '[AVFoundation indev @ 0x7ac1400140] [2] Capture screen 0\n'
            '[AVFoundation indev @ 0x7ac1400140] AVFoundation audio devices:\n'
            '[AVFoundation indev @ 0x7ac1400140] [0] MacBook Pro麦克风\n'
            '[AVFoundation indev @ 0x7ac1400140] [1] JustStream Audio Driver\n'
            '[in#0 @ 0x7ac1400000] Error opening input: Input/output error\n'
            "Error opening input file .\n"
            "Error opening input files: Input/output error\n")
        _vid31 = ['OBS Virtual Camera', 'FaceTime高清相机', 'Capture screen 0']
        _aud31 = ['MacBook Pro麦克风', 'JustStream Audio Driver']
        check("the listing this machine's ffmpeg really prints yields devices",
              _mirror31._parse_avfoundation_lists(_real31) == (_vid31, _aud31),
              str(_mirror31._parse_avfoundation_lists(_real31)))

        # ffmpeg 4.x spelled the same report differently, and the plugin still
        # has to read it: an older build on an older Mac is the common case for
        # anyone whose Homebrew is years stale.
        check("the older 'List of Video devices:' / '0) name' spelling parses too",
              _mirror31._parse_avfoundation_lists(
                  'List of AVFoundation Video devices:\n'
                  '  0) FaceTime HD Camera\n'
                  '  1) Capture screen 0\n'
                  'List of AVFoundation Audio devices:\n'
                  '  0) MacBook Pro Microphone\n')
              == (['FaceTime HD Camera', 'Capture screen 0'],
                  ['MacBook Pro Microphone']),
              str(_mirror31._parse_avfoundation_lists(
                  'List of AVFoundation Video devices:\n  0) FaceTime HD Camera\n')))

        # Names are printed as `[N] name`, so a line without an index is not a
        # device the plugin can address: `-i N:none` needs that number. This is
        # deliberately what the invented quoted form parses to -- an honest
        # empty result rather than a guessed index.
        check("a device line with no index is not a device",
              _mirror31._parse_avfoundation_lists(
                  'AVFoundation video devices:\n'
                  '   "Capture screen 0"\n') == ([], []),
              str(_mirror31._parse_avfoundation_lists(
                  'AVFoundation video devices:\n   "Capture screen 0"\n')))

        # -- the same reading, through a real subprocess ---------------------
        _listing31 = os.path.join(_tmp31, "listing.txt")
        with open(_listing31, "w", encoding="utf-8") as _fh31:
            _fh31.write(_real31)
        _argv31 = os.path.join(_tmp31, "argv.txt")
        _fake31 = _write_fake(_bin31, "ffmpeg", """#!/bin/sh
printf '%s\\n' "$@" >> '{argv}'
case "$*" in
  *list_devices*)
    cat '{listing}'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\\0' 'T'
  sleep 0.05
done
""".format(argv=_argv31, listing=_listing31))
        _asked_list31 = _mirror31._avfoundation_lists(_fake31)
        check("…and the plugin asks ffmpeg for it with the exact command line",
              _asked_list31 == (_vid31, _aud31), str(_asked_list31))
        with open(_argv31, encoding="utf-8") as _fh31:
            _asked31 = [l.rstrip('\n') for l in _fh31]
        check("…asking for the device table only, never opening a device",
              _asked31 == ['-hide_banner', '-loglevel', 'info',
                           '-f', 'avfoundation', '-list_devices', 'true',
                           '-i', ''], str(_asked31))

        # Device names on a Chinese-locale Mac are UTF-8, and a webcam name can
        # be anything the firmware put there. The read is `errors='replace'`,
        # so the worst case is one mangled name -- not an exception that loses
        # every device after it.
        _raw31 = os.path.join(_tmp31, "raw.txt")
        with open(_raw31, "wb") as _fh31:
            _fh31.write(
                b'[AVFoundation indev @ 0x1] AVFoundation video devices:\n'
                b'[AVFoundation indev @ 0x1] [0] Clear Name\n'
                b'[AVFoundation indev @ 0x1] [1] Bad\xff\xc3name\n'
                b'[AVFoundation indev @ 0x1] AVFoundation audio devices:\n'
                b'[AVFoundation indev @ 0x1] [0] Fine Mic\n')
        _rawfake31 = _write_fake(_bin31, "ffmpeg-broken", """#!/bin/sh
case "$*" in
  *list_devices*) cat '{raw}'; exit 0;;
esac
exit 1
""".format(raw=_raw31))
        _vidbroken31, _audbroken31 = _mirror31._avfoundation_lists(_rawfake31)
        check("one undecodable device name costs that name's bytes, not the list",
              len(_vidbroken31) == 2 and _vidbroken31[0] == 'Clear Name'
              and '\ufffd' in _vidbroken31[1] and _audbroken31 == ['Fine Mic'],
              str((_vidbroken31, _audbroken31)))

        # -- what the probe does with a correct list --------------------------
        # This is the level the user felt the bug at: not "the parse returned
        # []" but "the menu says this Mac cannot mirror anything".
        _cap31 = _mirror31.probe_capture(_fake31, 'darwin', True)
        check("so the darwin probe finds the screen at its real avfoundation index",
              _cap31 is not None and _cap31.screens == [(2, 'Capture screen 0')]
              and _cap31.inputs[0][-1] == '2:none'
              and _cap31.audio_map is None,
              str(_cap31 and (_cap31.screens, _cap31.inputs, _cap31.audio_map)))

        _bhlisting31 = os.path.join(_tmp31, "bh.txt")
        with open(_bhlisting31, "w", encoding="utf-8") as _fh31:
            _fh31.write(_real31.replace(
                '[1] JustStream Audio Driver',
                '[1] JustStream Audio Driver\n'
                '[AVFoundation indev @ 0x7ac1400140] [2] BlackHole 2ch'))
        _bhfakes1 = _write_fake(_bin31, "ffmpeg-bh", """#!/bin/sh
case "$*" in
  *list_devices*) cat '{listing}'; exit 0;;
esac
exit 1
""".format(listing=_bhlisting31))
        _capbh31 = _mirror31.probe_capture(_bhfakes1, 'darwin', True)
        check("a BlackHole two indices away from the mic is still the third input",
              _capbh31 is not None and _capbh31.inputs[0][-1] == '2:2'
              and _capbh31.audio_map == '0:a:0',
              str(_capbh31 and (_capbh31.inputs, _capbh31.audio_map)))

        _none31 = os.path.join(_tmp31, "none.txt")
        with open(_none31, "w", encoding="utf-8") as _fh31:
            _fh31.write('[AVFoundation indev @ 0x1] AVFoundation video devices:\n'
                        '[AVFoundation indev @ 0x1] AVFoundation audio devices:\n'
                        '[in#0 @ 0x1] Error opening input: Input/output error\n')
        _nofake31 = _write_fake(_bin31, "ffmpeg-none", """#!/bin/sh
case "$*" in
  *list_devices*) cat '{listing}'; exit 0;;
esac
exit 1
""".format(listing=_none31))
        check("and an honest empty listing still means 'nothing to mirror'",
              _mirror31.probe_capture(_nofake31, 'darwin', True) is None,
              str(_mirror31.probe_capture(_nofake31, 'darwin', True)))

        # -- the regression, kept executable --------------------------------
        # The parser v0.1..v0.7 shipped, reproduced here line for line. On the
        # output above it returns nothing, which is how a four-release-old bug
        # looked like a working feature in every test.
        def _old31(text):
            def _section(marker):
                tail = text.split(marker)
                if len(tail) < 2:
                    return []
                body = tail[1]
                for other in ('Video devices:', 'Audio devices:'):
                    body = body.split(other)[0]
                return _re31.findall(r'"([^"]*)"', body)
            return _section('Video devices:'), _section('Audio devices:')
        check("the parser this replaces read real ffmpeg output as an empty machine",
              _old31(_real31) == ([], [])
              and _mirror31._parse_avfoundation_lists(_real31) == (_vid31, _aud31),
              str(_old31(_real31)))

        # -- and the fixtures that hid it -----------------------------------
        # A parser bug that its own tests agree with is a fixture bug. The
        # fictional form had one unmistakable mark: a device name in quotes with
        # no avfoundation index in front of it. Real ffmpeg does quote some
        # names, always alongside `[N]`, so the index is what decides.
        with open(os.path.abspath(__file__), encoding="utf-8") as _fh31:
            _self31 = _fh31.read()
        _fictional31 = []
        for _block31 in _self31.split('*list_devices*)')[1:]:
            for _line in _block31.split('exit 0')[0].splitlines():
                if '"' not in _line:
                    continue
                if _re31.search(r'\[\s*\d+\s*\]|\d+\s*\)', _line):
                    continue
                _fictional31.append(_line.strip())
        check("no fake ffmpeg in this suite answers -list_devices in the invented shape",
              not _fictional31, str(_fictional31))
    finally:
        utils.SETTING_DIR = _saved_dir31
        utils.Setting.setting, utils.Setting.setting_path = _saved_setting31
        if _mirror31 is not None:
            _mirror31.invalidate_capture_cache()
        _shutil.rmtree(_tmp31, ignore_errors=True)
except Exception as _e31:
    import traceback
    traceback.print_exc()
    check("the device listing is checkable", False,
          "{}: {}".format(type(_e31).__name__, _e31))


# --------------------------------------------------------------------------
# Part 32: what the preflight says about the sender plugins
#
# `selfcheck.py` is the thing a user runs when a plugin "does nothing", so its
# answers have to match what the plugins actually ask the machine. Both sides
# drift: a plugin gains an encoder or an install directory, the preflight keeps
# its older list, and the report becomes a confident lie ("ffmpeg can encode
# everything" / "no Chromecast here") that sends the user somewhere else. These
# checks read the two plugin files and the preflight and require them to agree;
# they never run the preflight, which probes the network and spawns ffmpeg.
# --------------------------------------------------------------------------
print("\n=== Part 32: the preflight's sender-plugin claims ===")
try:
    import re as _re32
    with open(os.path.join(REPO, "scripts", "selfcheck.py"),
              encoding="utf-8") as _fh32:
        _pre32 = _fh32.read()
    _plug32 = ""
    for _name32 in ("screen_mirror.py", "cast_local_file.py"):
        with open(os.path.join(MACAST, "plugins", "renderer", _name32),
                  encoding="utf-8") as _fh32:
            _plug32 += _fh32.read()

    # -- 1. every codec the plugins hand to ffmpeg gets asked about ----------
    _used32 = set(_re32.findall(r"'-c:[va]',\s*'([^']+)'", _plug32))
    # `aac` ships in every ffmpeg build, so probing for it would only produce
    # noise; anything else has to be named in the preflight. `ppm` joins it for
    # the same reason: the preview frame is raw pixels, an encoder no build of
    # ffmpeg is without, and a missing one would take the picture with it -- the
    # mirror's real encoders (x264 / VideoToolbox / mpeg2 / ac3) each still cost
    # a link, so they stay on the list.
    _always32 = {'aac', 'ppm'}
    _probe_src32 = (_pre32.split('for flag, why in (')[1]
                    .split('if flag in out:')[0])
    _probed32 = set(_re32.findall(r'\(\s*"([a-z0-9_]+)"', _probe_src32))
    check("the preflight asks about every codec the plugins encode to",
          _used32 - _always32 <= _probed32,
          "codecs in plugins=%s, probed=%s" % (sorted(_used32), sorted(_probed32)))
    check("and it does not still ask about a codec no sender plugin uses",
          not (_probed32 - _used32),
          "stale probes: %s" % sorted(_probed32 - _used32))

    # -- 2. the same places to look for ffmpeg -------------------------------
    def _listed_dirs(source, marker):
        """Directories listed by one tool-lookup block, binaries stripped.

        The plugins list `<dir>/ffmpeg` while the preflight lists directories,
        so both are compared as directories or the check would be red for a
        formatting difference.
        """
        body = source.split(marker, 1)[1].split('\n\n\n')[0]
        out = set()
        for path in _re32.findall(r'"(/[^"]+)"', body):
            base = os.path.basename(path)
            out.add(os.path.dirname(path)
                    if base in ('ffmpeg', 'ffprobe', 'mpv')
                    or base.endswith('.exe') else path)
        return out

    _plugin_bin32 = set()
    for _name32, _marker32 in (("screen_mirror.py", "def find_ffmpeg("),
                               ("cast_local_file.py", "def find_tool(")):
        with open(os.path.join(MACAST, "plugins", "renderer", _name32),
                  encoding="utf-8") as _fh32:
            _plugin_bin32 |= _listed_dirs(_fh32.read(), _marker32)
    _pre_bin32 = _listed_dirs(_pre32, "COMMON_BIN_DIRS = ")
    check("the preflight looks for tools where the plugins look for them",
          _plugin_bin32 and _plugin_bin32 <= _pre_bin32,
          "plugins search %s, preflight searches %s" % (
              sorted(_plugin_bin32), sorted(_pre_bin32)))

    # -- 3. the setting keys it reads are real ------------------------------
    _read32 = set(_re32.findall(r'\.get\("([A-Z][A-Za-z_]*)"\)', _pre32))
    check("the preflight reads settings only through keys a plugin writes",
          all(_re32.search(r'\b%s\s*=' % _key32, _plug32) for _key32 in _read32),
          "read=%s" % sorted(_read32))

    # -- 4. discovery targets -----------------------------------------------
    check("the preflight browses the mDNS service type the plugins browse",
          '"_googlecast._tcp.local."' in _pre32
          and '"_googlecast._tcp.local."' in _plug32,
          "the service type string has to be identical on both sides")
    check("and it probes for the same UPnP device type the DLNA targets use",
          'urn:schemas-upnp-org:device:MediaRenderer:1' in _pre32
          and 'urn:schemas-upnp-org:device:MediaRenderer:1' in _plug32,
          "otherwise the preflight can find a TV no plugin would offer")

    # -- 5. it stays a reader ------------------------------------------------
    check("the preflight never writes to the user's settings",
          'Setting.set(' not in _pre32 and 'Setting.save(' not in _pre32
          and 'Setting.setting[' not in _pre32,
          "AGENTS.md 10: verification must not change real configuration")
except Exception as _e32:
    import traceback
    traceback.print_exc()
    check("the preflight's sender claims are checkable", False,
          "{}: {}".format(type(_e32).__name__, _e32))


# --------------------------------------------------------------------------
# Part 33: the end-to-end smoke test stays isolated and stays truthful
#
# `scripts/e2e_smoke.py` is the only check that runs the *real* app, which
# makes it the only check that can hurt the user: it starts a second Macast on
# this machine. Everything that keeps it harmless is a string in that file --
# the settings keys that put it on another port, the appdirs patch that keeps
# its files out of the real config dir, the protocol list that keeps it off
# 8009. A rename on the app side (SettingProperty, a proxy variable, an `/api`
# query name) turns one of those into a no-op **silently**, and the smoke test
# still prints all green because the app happily falls back to defaults. So
# each of those couplings gets asserted here, against the app's own source.
# The script is never imported -- these are text checks, exactly like Part 32.
# --------------------------------------------------------------------------
print("\n=== Part 33: the end-to-end smoke test's couplings to the app ===")
try:
    import re as _re33

    def _read33(*parts):
        with open(os.path.join(REPO, *parts), encoding="utf-8") as _fh33:
            return _fh33.read()

    _smoke33 = _read33("scripts", "e2e_smoke.py")
    _utils33 = _read33("macast", "utils.py")
    _proto33 = _read33("macast", "protocol.py")
    _mgr33 = _read33("macast", "macast.py")

    def _between(source, start, end):
        """The text after `start` up to the next `end`; '' when `start` is gone.

        Deliberately returns '' rather than the tail when `end` is missing: a
        moved function should make the check below fail loudly, not read the
        rest of the file as its body.
        """
        at = source.find(start)
        if at < 0:
            return ''
        rest = source[at + len(start):]
        stop = rest.find(end)
        return rest if stop < 0 else rest[:stop]

    _seed33 = _between(_smoke33, 'def seed_settings(', '\ndef ')
    _boot33 = _between(_smoke33, "BOOTSTRAP = '''", "'''")

    # -- 1. the settings it seeds are settings the app still reads ----------
    _keys33 = set(_re33.findall(r"'([A-Za-z_][A-Za-z0-9_]*)':", _seed33))
    _props33 = set(_re33.findall(
        r"^    ([A-Za-z_][A-Za-z0-9_]*) = \d+$",
        _between(_utils33, 'class SettingProperty(Enum):', '\n\n\n'),
        _re33.M))
    check("the smoke test seeds keys that really are settings",
          bool(_keys33) and _keys33 <= _props33,
          "seeded=%s unknown=%s" % (sorted(_keys33), sorted(_keys33 - _props33)))
    # ApplicationPort is the one that matters most: if the app stopped reading
    # it, the child falls back to 58880 and fights the user's own instance --
    # and the smoke test still passes, because whatever is on 58880 answers.
    check("and it pins the port through the key the app actually reads",
          "'ApplicationPort': PORT" in _seed33
          and _re33.search(r"ApplicationPort = \d+", _utils33) is not None
          and _re33.search(r"ApplicationPort, DEFAULT_PORT", _utils33) is not None,
          "a renamed or unread ApplicationPort puts the smoke instance on the "
          "user's port")
    check("the smoke port is not a port the app owns",
          'PORT = 58999' in _smoke33
          and '58999' not in _utils33 and '58999' not in _proto33,
          "app DEFAULT_PORT=%s" % _re33.findall(r"DEFAULT_PORT = (\d+)", _utils33))
    _enabled33 = _between(_seed33, "'Macast_Protocols': [", "]")
    check("it enables DLNA only, so it cannot take 8009 or the HTTPS channel",
          'DLNA Protocol' in _enabled33
          and 'Chromecast' not in _enabled33 and 'AirPlay' not in _enabled33
          and 'Https_Enabled' not in _keys33,
          "Macast_Protocols=[%s]" % _enabled33.strip())
    check("the friendly name it asserts on is the one it seeds",
          "'DLNA_FriendlyName': FRIENDLY" in _seed33
          and "FRIENDLY = 'Macast E2E Smoke'" in _smoke33
          and _smoke33.count('FRIENDLY') >= 4,
          "a literal in one place and a constant in the other would prove nothing")

    # -- 2. the isolation patch has to run before the app imports -----------
    check("the child patches appdirs before it imports macast",
          0 < _boot33.find('appdirs.user_config_dir') < _boot33.find('runpy')
          and "run_name='__main__'" in _boot33,
          "SETTING_DIR is computed at import time (AGENTS.md 4.9); patching "
          "later would write into the user's real config dir")
    check("the smoke test itself never imports macast",
          _re33.search(r"^\s*(?:import macast\b|from macast)", _smoke33, _re33.M)
          is None,
          "an in-process import resolves SETTING_DIR to the real one before the "
          "child is even started")
    check("nothing in it writes settings",
          'Setting.set(' not in _smoke33 and 'Setting.save(' not in _smoke33,
          "AGENTS.md 10: verification must not change real configuration")

    # -- 3. the proxy list has to keep up with the app ----------------------
    _strip33 = set(_re33.findall(r"'([A-Za-z_]+)'",
                                 _between(_smoke33, 'PROXY_VARS = (', ')')))
    _app_proxy33 = set(_re33.findall(
        r"'([A-Za-z_]+)'",
        _between(_utils33, 'PROXY_ENV_VARS = (', ')')))
    check("it strips every proxy variable the app strips from the player",
          bool(_app_proxy33) and _app_proxy33 <= _strip33,
          "app=%s smoke=%s" % (sorted(_app_proxy33), sorted(_strip33)))

    # -- 4. the /api surface it depends on still exists ---------------------
    _asked33 = set(_re33.findall(r"api\('([a-z-]+)'", _smoke33))
    _asked33 |= set(_re33.findall(r"[?&]query=([a-z-]+)", _smoke33))
    _served33 = set(_re33.findall(r"query == '([a-z-]+)'", _proto33))
    check("every /api query it asks is one the server answers",
          bool(_asked33) and _asked33 <= _served33,
          "asks=%s unanswered=%s" % (sorted(_asked33),
                                     sorted(_asked33 - _served33)))
    _status_body33 = _between(_proto33, 'def get_status(self):',
                              "return {'server'")
    _read33 = set(_re33.findall(r"server\.get\('([a-z_]+)'\)", _smoke33))
    _built33 = set(_re33.findall(r"'([a-z_]+)':", _status_body33))
    check("the status fields it reads are the ones get_status builds",
          bool(_read33) and bool(_built33) and _read33 <= _built33,
          "reads=%s missing=%s" % (sorted(_read33), sorted(_read33 - _built33)))
    check("the GET cast entry point it exercises still demands the token",
          "if not self._token_present()" in _proto33
          and "token=%s" in _smoke33,
          "AGENTS.md 4.7: a drive-by page must not be able to start playback, "
          "even from loopback")

    # -- 5. how it installs plugins mirrors how the app does ----------------
    # Matched inside the copy function, not anywhere in the file: the word
    # __init__.py also appears in the comment that explains why it is needed,
    # and a mutant that deletes the call must not keep passing on that.
    _place33 = _between(_smoke33, 'def install_online_plugins(', '\ndef ')
    check("it recreates the plugin directories the loader expects",
          _re33.search(r"open\(os\.path\.join\(\w+, '__init__\.py'\)", _place33)
          is not None
          and "os.path.join(temp_dir, kind)" in _place33
          and "'__init__.py'" in _mgr33,
          "MacastPluginManager.create_plugin_dir writes an __init__.py, so a "
          "hand-copied plugin without one is not importable")

    # -- 6. it reports honestly ---------------------------------------------
    check("it keeps the conformance script's PASS/FAIL/SKIP/INFO convention",
          all(word in _smoke33 for word in ("'PASS'", "'FAIL'", "'SKIP'",
                                            "'INFO'")),
          "a check this machine cannot do is SKIPPED, never silently passed "
          "(scripts/cast_conformance.py)")
    check("its playback check is opt-in and skipped otherwise",
          "def playback_round(" in _smoke33 and "--play" in _smoke33
          and "skip('playback" in _smoke33,
          "it opens a window and needs ffmpeg, so the default run must say so "
          "rather than pass")
    check("and its exit code is driven by failures, not by having finished",
          _re33.search(r"return 1 if failed else 0", _smoke33) is not None,
          "otherwise cron/CI could never tell")
except Exception as _e33:
    import traceback
    traceback.print_exc()
    check("the smoke test's couplings are checkable", False,
          "{}: {}".format(type(_e33).__name__, _e33))


# --------------------------------------------------------------------------
# Part 34: who wrote what, and does the tree say so
#
# `scripts/provenance.py` counts, per file, how many lines `git blame`
# attributes to commits before the fork point. That number is what the two
# copyright statements in a file's header have to line up with: upstream's
# notice may not go while its lines are still there, and ours may not appear
# where we never wrote a line. Both directions matter now that the project
# sells an unlocked tier -- at that point a provenance claim is a claim made to
# a paying customer.
#
# So this Part checks the ledger three times over: it runs the tool, then
# re-derives the same rules from the files themselves (a bug in `problems()`
# must not be able to certify its own absence), then compares the numbers
# recorded in `docs/Provenance.md` against the ones history actually gives.
# Finally it checks the tool's own promise -- that stamping only ever *inserts*
# -- because "we never delete attribution" is the sentence this whole exercise
# rests on.
#
# Unlike the rest of the suite this needs real history, so a shallow clone is
# reported as a failure with its cause, never as a pass.
# --------------------------------------------------------------------------
print("\n=== Part 34: the provenance ledger and the notices that track it ===")
try:
    import re as _re34
    import subprocess as _sub34

    def _read34(*parts):
        with open(os.path.join(REPO, *parts), encoding="utf-8") as _fh34:
            return _fh34.read()

    _env34 = dict(os.environ)
    _env34.pop('PYTHONPATH', None)
    _run34 = _sub34.run([sys.executable, os.path.join(REPO, 'scripts', 'provenance.py'),
                         '--json', '--check'], cwd=REPO, env=_env34,
                       stdout=_sub34.PIPE, stderr=_sub34.PIPE, text=True)

    _shallow34 = 'cannot read history' in _run34.stderr
    check("the ledger is computable here (a full clone, not a shallow one)",
          not _shallow34,
          "git history is missing, so every line count below would be a guess: "
          "%s" % _run34.stderr.strip().splitlines()[:1])

    _led34 = json.loads(_run34.stdout) if _run34.stdout.strip() else {}
    _files34 = _led34.get('files', [])
    _by_path34 = {row['path']: row for row in _files34}

    # -- 1. the tool's own verdict, and its exit code ------------------------
    check("no file's notices contradict its line counts",
          _run34.returncode == 0 and not _led34.get('problems'),
          "; ".join(_led34.get('problems', []))[:400])
    check("the fork point the ledger measures against is in this history",
          _led34.get('fork_point') and _sub34.run(
              ['git', 'cat-file', '-e', _led34['fork_point'] + '^{commit}'],
              cwd=REPO, stdout=_sub34.DEVNULL,
              stderr=_sub34.DEVNULL).returncode == 0,
          "a wrong or absent FORK_POINT would classify the whole repo as ours")
    check("and upstream really has history behind it",
          _led34.get('upstream_commits', 0) >= 100,
          "upstream_commits=%s" % _led34.get('upstream_commits'))

    # -- 2. the same rules, re-derived from the files ------------------------
    # Independent of problems(): if that function loses a branch, the check
    # above goes green on its own report and proves nothing.
    _UPSTREAM_MARKS34 = ('by xfangfang', 'Derived from xfangfang/Macast',
                         'Copied from xfangfang/Macast-plugins')

    def _head34(path):
        text = _read34(path)
        return "\n".join(text.splitlines()[:40])

    _unattributed34 = [r['path'] for r in _files34
                       if r['upstream_lines']
                       and not any(m in _head34(r['path']) for m in _UPSTREAM_MARKS34)]
    check("every file holding upstream lines attributes them to upstream",
          not _unattributed34, "missing notice: %s" % _unattributed34)

    _unclaimed34 = [r['path'] for r in _files34
                    if r['our_lines'] and r['state'] in ('ours', 'mixed')
                    and 'by pingod' not in _head34(r['path'])]
    check("every file this fork wrote a line of carries our notice",
          not _unclaimed34, "missing notice: %s" % _unclaimed34)

    _overclaim34 = [r['path'] for r in _files34
                    if r['state'] in ('upstream', 'vendored')
                    and 'by pingod' in _head34(r['path'])]
    check("and no notice of ours sits on lines we never wrote",
          not _overclaim34, "over-claimed: %s" % _overclaim34)

    check("the MIT layer inside macast/ssdp.py is still stated",
          'Licensed under the MIT license' in _head34('macast/ssdp.py')
          and 'Tim Potter' in _read34('macast', 'ssdp.py'),
          "that block belongs to neither us nor upstream -- nobody may remove it")

    # The bundle (`macast/plugins/`) now holds both vendored upstream plugins
    # and this fork's own built-in plugins, so it is no longer exclusively
    # vendored. The invariant that still holds: the `VENDORED` tuple must
    # exactly match the set of bundled files provenance actually classifies as
    # vendored -- every declared-vendored file really is there and really is
    # vendored, and none the tool flags as vendored was left out of the
    # declaration.
    _vendored_in_bundle34 = sorted(
        r['path'] for r in _files34
        if r.get('state') == 'vendored'
        and r['path'].startswith('macast/plugins/')
        and not r['path'].endswith('__init__.py'))
    check("every declared-vendored bundled plugin is genuinely vendored, "
          "and none is missed",
          sorted(_led34.get('vendored', [])) == _vendored_in_bundle34,
          "declared=%s vendored-in-bundle=%s"
          % (sorted(_led34.get('vendored', [])), _vendored_in_bundle34))

    # -- 3. the numbers written down in the docs ----------------------------
    _doc34 = _read34('docs', 'Provenance.md')
    _mark34 = _re34.search(r"<!--\s*provenance-ledger:(.*?)-->", _doc34)
    _said34 = dict(kv.split('=', 1) for kv in
                   _mark34.group(1).split()) if _mark34 else {}
    _counted34 = {state: sum(1 for r in _files34 if r['state'] == state)
                  for state in ('upstream', 'vendored', 'mixed', 'ours')}
    _tally34 = {
        'fork': _led34.get('fork_point', ''),
        'files': str(len(_files34)),
        'upstream_lines': str(sum(r['upstream_lines'] for r in _files34)),
        'our_lines': str(sum(r['our_lines'] for r in _files34)),
    }
    _tally34.update({k: str(v) for k, v in _counted34.items()})
    check("docs/Provenance.md's ledger line matches the history",
          _said34 == _tally34,
          "doc says %s, history says %s" % (_said34, _tally34))

    _rows34 = _re34.findall(r"^\| (?!域|\*\*|文件)[^|]+\| *([\d,]+) \| *([\d,]+) "
                            r"\| *(\d+) \|", _doc34, _re34.M)

    def _num34(text):
        return int(text.replace(',', ''))

    check("its per-area table adds up to the ledger",
          len(_rows34) >= 6
          and sum(_num34(r[0]) for r in _rows34) == _num34(_said34.get('upstream_lines', '-1'))
          and sum(_num34(r[1]) for r in _rows34) == _num34(_said34.get('our_lines', '-1'))
          and sum(_num34(r[2]) for r in _rows34) == _num34(_said34.get('files', '-1')),
          "table rows=%d sums=%s/%s/%s vs doc %s/%s/%s"
          % (len(_rows34),
             sum(_num34(r[0]) for r in _rows34),
             sum(_num34(r[1]) for r in _rows34),
             sum(_num34(r[2]) for r in _rows34),
             _said34.get('upstream_lines'), _said34.get('our_lines'),
             _said34.get('files')))

    # The per-file table is the one a reader trusts, so it has to be the ledger
    # too -- same numbers, one row per non-obvious file.
    _file34 = {m.group(1): (int(m.group(2)), int(m.group(3)))
               for m in _re34.finditer(r"^\| `([^`]+\.py)` \| (?:mixed|upstream|vendored) "
                                       r"\| *(\d+) \| *(\d+) \|", _doc34, _re34.M)}
    check("and its per-file rows are the ledger's own line counts",
          _file34 and all(_by_path34.get(p, {}).get('upstream_lines') == u
                          and _by_path34.get(p, {}).get('our_lines') == o
                          for p, (u, o) in _file34.items()),
          "drifted: %s" % [p for p, (u, o) in _file34.items()
                            if (_by_path34.get(p, {}).get('upstream_lines'),
                                _by_path34.get(p, {}).get('our_lines')) != (u, o)])

    # -- 4. the promise the whole exercise rests on --------------------------
    _tool34 = _read34('scripts', 'provenance.py')
    _stamp34 = _tool34[_tool34.find('def stamp('):_tool34.find('def report(')]
    check("stamping can only ever insert a notice",
          '.insert(' in _stamp34 and 'by pingod' in _tool34
          and not any(word in _stamp34 for word in
                      ('.pop(', 'del lines', 'lines.remove', 'os.remove',
                       'shutil', 're.sub', 'truncate')),
          "if this grows a delete path, docs/Provenance.md 6 is lying")

    # Import the tool as a module -- it is stdlib-only, so this is cheap and it
    # lets the rules be poked directly instead of trusting their current output.
    import importlib.util as _ilu34
    _spec34 = _ilu34.spec_from_file_location('macast_provenance',
                                             os.path.join(REPO, 'scripts', 'provenance.py'))
    _prov34 = _ilu34.module_from_spec(_spec34)
    _spec34.loader.exec_module(_prov34)

    def _row34(**over):
        row = {'path': 'x/y.py', 'state': 'mixed', 'upstream_lines': 100,
               'our_lines': 50, 'upstream_attributed': True, 'our_attributed': True}
        row.update(over)
        return row

    # `problems()` also sweeps the real third-party table, so each synthetic
    # case filters to its own row instead of asserting on the whole report --
    # otherwise an unrelated drift in the tree would look like a rules bug.
    def _drift34(row):
        return [text for text in _prov34.problems([row]) if text.startswith('x/y.py')]

    check("the tool reports a stripped upstream notice as drift",
          len(_drift34(_row34(upstream_attributed=False))) == 1
          and 'upstream' in _drift34(_row34(upstream_attributed=False))[0],
          "this is the direction the fork is tempted to get wrong: "
          "%s" % _drift34(_row34(upstream_attributed=False)))
    check("and a notice we never earned as drift too",
          any('claims our copyright' in text for text in
              _drift34(_row34(state='vendored', our_lines=0, upstream_lines=500))),
          "vendored files came from Macast-plugins; stamping them would be a lie")
    check("a file whose last upstream line has been rewritten needs no upstream notice",
          not _drift34(_row34(state='ours', upstream_lines=0,
                              upstream_attributed=False)),
          "this is the rewrite queue's finish line -- it has to be machine-checkable")
    check("the insert point stays inside a plugin's metadata block",
          _prov34.notice_index(['# Screen Mirror for Macast\n', '#\n',
                                '# <macast.title>Screen Mirror</macast.title>\n',
                                'import os\n']) == 0
          and _prov34.notice_index(['#!/usr/bin/env python3\n',
                                    '# -*- coding: utf-8 -*-\n', 'import os\n']) == 2
          and _prov34.notice_index([
              '# Copyright (c) 2021 by xfangfang. All Rights Reserved.\n',
              '\n', 'import os\n']) == 1,
          "AGENTS.md 4.5: the manifest is parsed only from the *contiguous* comment "
          "block, so a notice pushed past it silently uninstalls the plugin")

    # That last claim is about the app's own parser, so ask the parser.
    _stamped34 = "\n".join([
        _prov34.OUR_NOTICE,
        '# Screen Mirror for Macast',
        '#',
        '# Macast Metadata',
        '# <macast.title>Stamped Mirror</macast.title>',
        '# <macast.renderer>StampedRenderer</macast.renderer>',
        '# <macast.platform>darwin</macast.platform>',
        '# <macast.version>0.1</macast.version>',
        '"""Still a docstring, still the first statement."""',
        'import os'])
    _tmp34 = _tempfile.mkdtemp(prefix="macast-provenance-")
    _probe34 = os.path.join(_tmp34, "stamped_plugin.py")
    with open(_probe34, "w", encoding="utf-8") as _fh34:
        _fh34.write(_stamped34)
    try:
        _read_manifest34 = macast_mod._read_plugin_metadata
    except NameError:
        _read_manifest34 = _load("macast", "macast.py")._read_plugin_metadata
    _meta34 = _read_manifest34(_probe34)
    check("and a stamped plugin is still recognised by the loader",
          _meta34.get('title') == 'Stamped Mirror'
          and _meta34.get('renderer') == 'StampedRenderer',
          "metadata=%s" % _meta34)
    check("the tool reads only stdlib, so CI can run it without the app's deps",
          all('import %s' % m in _tool34 or 'import %s,' % m in _tool34
              for m in ('json', 'os', 'subprocess', 'sys'))
          and not _re34.search(r"^\s*(?:import|from) (cherrypy|requests|zeroconf|macast)\b",
                               _tool34, _re34.M),
          "it is a history tool, not part of the shipped app")
    check("it never touches the working tree without --apply",
          '--apply' in _stamp34 and 'continue' in _stamp34,
          "the dry-run guard lives inside stamp(); move it and a bulk edit "
          "becomes an accident")
except Exception as _e34:
    import traceback
    traceback.print_exc()
    check("the provenance ledger is checkable", False,
          "{}: {}".format(type(_e34).__name__, _e34))


# --------------------------------------------------------------------------
# Part 35: the 电脑投屏 console, in the settings page.
#
# The mirror controls left the menu bar and became a tab of the page Macast
# serves in the browser, so three things that used to be "a row in a menu" are
# now contracts: the HTTP endpoints the page reads, the view rules that decide
# what it shows (`macast/mirror_view.py`), and the spellings the page and the
# server share. Each is tested where it is decided -- and because the page holds
# no Python anyone could call, its half of those contracts is tested as text.
# --------------------------------------------------------------------------
print("\n=== Part 35: the mirror console in the settings page ===")
import re as _re35

_saved_setting35 = (utils.Setting.setting, utils.Setting.setting_path)
_saved_dir35 = utils.SETTING_DIR
_tmp35 = _tempfile.mkdtemp(prefix="macast-console-")
_notify35 = []
_notify35_rec = lambda *a, **k: _notify35.append(a)          # noqa: E731
mirror35 = None
_patches35 = {}


def _patch35(**kw):
    """Patch names on the plugin module only -- never on a shared stdlib module."""
    for _name, _value in kw.items():
        _patches35.setdefault(_name, getattr(mirror35, _name))
        setattr(mirror35, _name, _value)


def _unpatch35():
    for _name, _value in _patches35.items():
        setattr(mirror35, _name, _value)
    _patches35.clear()


try:
    utils.SETTING_DIR = _tmp35
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp35, "macast_setting.json")
    cherrypy.engine.subscribe('app_notify', _notify35_rec)
    mirror35 = _load_plugin("screen_mirror_plugin_v35", "screen_mirror.py")

    # -- the view rules: what a state dict turns into -----------------------
    _base35 = {'platform': 'darwin', 'available': True,
               'console_version': _mc.VIEW_VERSION,
               'capture': {'probed': True}, 'audio': {'line': ''},
               'output': {'kind': 'cast'}}

    def _state35(**kw):
        merged = dict(_base35)
        merged.update(kw)
        return merged

    check("the page and the plugin agree on the contract version",
          _mc.VIEW_VERSION == mirror35.CONSOLE_VERSION,
          "an old cached page against a new app has to say so; silently losing "
          "buttons is the failure this guards")
    check("a Chromecast target lays out devices but no viewer panel",
          _mc.sections_for(_base35) == ['channels', 'devices', 'quality',
                                        'capture', 'audio', 'preview',
                                        'activity'],
          str(_mc.sections_for(_base35)))
    check("a DLNA电视 adds its compatibility profiles beside the devices",
          _mc.sections_for(_state35(output={'kind': 'dlna'}))[:4]
          == ['channels', 'devices', 'profiles', 'quality'],
          str(_mc.sections_for(_state35(output={'kind': 'dlna'}))))
    check("a browser target drops the device list and gains the viewer",
          'devices' not in _mc.sections_for(_state35(output={'kind': 'browser'}))
          and 'viewer' in _mc.sections_for(_state35(output={'kind': 'browser'})))
    check("Windows with nothing to report gets no audio panel",
          'audio' not in _mc.sections_for(_state35(platform='win32')))
    check("...and gets one the moment the plugin has something to say",
          'audio' in _mc.sections_for(_state35(platform='win32',
                                              audio={'line': '系统声音：无'})))
    check("an unknown output kind still produces a page",
          _mc.sections_for(_state35(output={'kind': 'telnet'}))[:1]
          == ['channels'],
          '「投屏方式」is the one card that always has an honest answer')

    # 「需要安装」is the difference between a notification the user missed and a
    # card that stays until the binary turns up, so its absence is as much a
    # bug as its presence: an empty table must not hold a panel open.
    check("the requirements card appears only while something is outstanding",
          'requirements' not in _mc.sections_for(_base35)
          and 'requirements' in _mc.sections_for(
              _state35(requirements=[{'key': 'ffmpeg', 'label': 'ffmpeg',
                                      'detail': '', 'command': ''}])),
          str(_mc.sections_for(_base35)))

    # The card being in the layout is worth nothing if what a plugin records
    # never reaches it -- that was the whole report: a「brew install …」that
    # lived only in Notification Center. The board is the app's shared one, so
    # this writes an entry of its own and takes it back afterwards.
    import macast.notice as _board35
    _saved_kick35 = mirror35.start_search, mirror35.start_renderer_search
    mirror35.start_search = mirror35.start_renderer_search = lambda: None
    try:
        _board35.requirement('zz-suite-35', label='测试用需求',
                             detail='这条是用例写进去的，不是真的缺东西',
                             command='brew install something')
        _req35 = mirror35.ScreenMirrorSetting().console_state()['requirements']
        _board35.satisfied('zz-suite-35')
        _after35 = mirror35.ScreenMirrorSetting().console_state()['requirements']
        _prompt35 = mirror35.ScreenMirrorSetting().console_state()['prompt']
        _verdict35 = mirror35.target_prompt('cast', with_verdict=True)
    finally:
        _board35.satisfied('zz-suite-35')
        mirror35.start_search, mirror35.start_renderer_search = _saved_kick35
    check("what a plugin records as missing reaches the page",
          any(r['key'] == 'zz-suite-35'
              and r['command'] == 'brew install something'
              and '测试用需求' in r['label'] for r in _req35), str(_req35))
    check("and the card row disappears once the thing turns up",
          not any(r['key'] == 'zz-suite-35' for r in _after35), str(_after35))

    # The feed is the same board: 「刚才通知了个要 brew 安装个啥东西」 was the
    # report, and a notification centre is not somewhere you can scroll back to.
    _board35.record('ZZ 套件：这条通知要能在页面里读到')
    _recent35 = mirror35.ScreenMirrorSetting().console_state()['recent']
    check("a sentence said to the user is in the page's feed",
          any('ZZ 套件' in m['text'] for m in _recent35), str(_recent35[-3:]))

    # The prompt used to carry the search verdict inside it, so one card read
    # 「没有发现 Chromecast（搜于 19:53:42）」twice -- once on its own line and
    # once in the sentence under it. The page already has the verdict; only a
    # refused start, which leaves for a notification with no card beside it,
    # has to say why as well as what is missing.
    check("the card's prompt says what is missing, and leaves the verdict out",
          '还没有选择' in _prompt35 and '（' not in _prompt35, _prompt35)
    check("a refused start says the verdict too",
          _verdict35.startswith(_prompt35) and len(_verdict35) > len(_prompt35),
          _verdict35)

    # -- 「正在探测…」 has to be on its way to an answer ------------------------
    #
    # Both the capture probe and the encoder probe spawn ffmpeg, so neither may
    # run inside the request that asks for the value. That leaves a page which
    # only ever reads caches showing a spinner forever on a machine nobody has
    # pressed「重新探测」on -- and the preview's own grab warms the capture cache
    # but not the encoder one, which is how a Mac got「正在探测这台机器有没有硬件
    # 编码…」as a permanent answer. So opening the page asks, once, and the guard
    # is the cache itself.
    _saved_kickpr35 = mirror35.request_capture_probe
    _saved_cap35 = dict(mirror35._capture_cache)
    _saved_hw35 = dict(mirror35._hw_encoder_cache)
    _saved_kick35b = mirror35.start_search, mirror35.start_renderer_search
    mirror35.start_search = mirror35.start_renderer_search = lambda: None
    _asks35 = []
    mirror35.request_capture_probe = lambda: _asks35.append(1) or True
    mirror35._capture_cache.clear()
    mirror35._hw_encoder_cache.clear()
    _plugin35src = open(mirror35.__file__, encoding='utf-8').read()
    _protocol35src = open(protocol.__file__, encoding='utf-8').read()
    try:
        mirror35.ScreenMirrorSetting().request_probes()
        check("an unprobed page gets a probe asked for, not a spinner",
              len(_asks35) == 1, str(_asks35))
        _cap35 = mirror35.ScreenMirrorSetting()._capture_state()
        check("and the two rows say they are waiting rather than answering",
              _cap35['hardware_probed'] is False and _cap35['probed'] is False,
              'hardware_probed False is the row that reads「正在探测…」')
        mirror35._capture_cache[('ffmpeg', 'darwin', True)] = object()
        mirror35._hw_encoder_cache[('ffmpeg', 'darwin')] = True
        mirror35.ScreenMirrorSetting().request_probes()
        check("...and a poll of an already-probed machine asks for nothing",
              len(_asks35) == 1,
              'the page polls once a second; a kick without the cache guard '
              'would spawn ffmpeg every second')
        check("so it is the endpoint that asks, not the state builder",
              'request_probes' in _protocol35src
              and 'request_probes' not in _plugin35src.split(
                  'def console_state')[1].split('def snapshot_frame')[0],
              'console_state() is what the suite drives directly, and a spawn '
              'in there is a probe thread racing every later assertion')
    finally:
        mirror35.request_capture_probe = _saved_kickpr35
        mirror35.start_search, mirror35.start_renderer_search = _saved_kick35b
        mirror35._capture_cache.clear()
        mirror35._capture_cache.update(_saved_cap35)
        mirror35._hw_encoder_cache.clear()
        mirror35._hw_encoder_cache.update(_saved_hw35)

    # -- which renderer owns the console, whichever one is playing -----------
    #
    # v2 moved 电脑投屏 out of the renderer's own `build_menu()`: the app only
    # asks *that* of the selected renderer, so opening the console used to mean
    # selecting Screen Mirror, clicking the door, then selecting the real player
    # back. So the console is found from the plugin catalog instead, and finding
    # it never starts anything.
    class _NoConsole35(object):
        """A renderer that owns no console must contribute no menu row."""

    class _Owner35(mirror35.ScreenMirrorRenderer):
        #: Building the door must not put a capture of this machine on the LAN.
        def start(self):
            raise AssertionError('the console door must not start the renderer')

    def _plugin35(title, cls):
        return macast_mod.MacastPlugin(None, title, plugin_factory=cls,
                                       platform='darwin,win32,linux')

    _mgr35b = macast_mod.MacastPluginManager.__new__(
        macast_mod.MacastPluginManager)
    _plain35 = _plugin35('MPV', _NoConsole35)
    _owner35 = _plugin35('Screen Mirror', _Owner35)
    _mgr35b.renderer_all = [_plain35, _owner35]
    check("the console's owner is found by a class flag, not by its title",
          _mgr35b.console_plugin() is _owner35
          and mirror35.ScreenMirrorRenderer.MIRROR_CONSOLE is True,
          'a title is user-editable; a class attribute is not')
    _mgr35b.renderer_all = [_owner35, _plain35]
    check("and the order of the renderer list does not decide it",
          _mgr35b.console_plugin() is _owner35, 'no selected renderer is '
          'involved, so nothing about the list can shadow it')
    _setting35 = _mgr35b.mirror_setting()
    check("asking for the surface instantiates the plugin without starting it",
          isinstance(_setting35, mirror35.ScreenMirrorSetting)
          and _owner35.plugin_instance is not None
          and _owner35.plugin_instance._proc is None,
          'instantiating is not selecting: no bus topic is taken, no media routed')
    # There is no door in the menu bar to test any more, and that is the point:
    # the settings page is the only console, and a plugin that cannot answer for
    # itself is now the page's problem (checked below, where it says so on the
    # card) rather than something the UI thread has to survive. Part 36 is what
    # keeps 电脑投屏 out of the menu.

    # The cards fall back to these names when the app answers without a catalog
    # -- which is what it does when Screen Mirror is not the current renderer.
    # Showing the raw key ('cast') is a key name, not a label, so the fallback
    # has to keep saying what the plugin says.
    check("the page's card names match the plugin's own output labels",
          set(_mc.OUTPUT_FALLBACK) == set(mirror35.OUTPUTS)
          and all(_mc.OUTPUT_FALLBACK[key] == mirror35.OUTPUTS[key][0]
                  for key in _mc.OUTPUT_FALLBACK),
          str(_mc.OUTPUT_FALLBACK))

    check("the header line is the plugin's own status, so they cannot disagree",
          _mc.format_stats({'status_line': '正在镜像 1920x1080 @ 24 fps',
                            'available': True}) == '正在镜像 1920x1080 @ 24 fps')
    check("a start that has not finished reads as starting, not as a dead button",
          _mc.format_stats({'available': True, 'starting': True})
          == '正在开始镜像…',
          'the capture probe costs a second; without this the pill says 未镜像 '
          'at a mirror that is on its way')
    check("an unavailable plugin is not the header's story to tell",
          _mc.format_stats({'available': False}) == '未镜像'
          and '启用 Screen Mirror'
          in _mc.banner_for(_state35(available=False))['text'],
          'one sentence per channel: the pill says state, the strip says fault')
    check("and an idle mirror reads 未镜像",
          _mc.format_stats({'available': True}) == '未镜像')

    check("device rows carry the key the page posts back, not the label",
          _mc.device_rows({'devices': [{'id': '192.168.1.9:8009',
                                        'label': '电视 · 192.168.1.9',
                                        'selected': True, 'name': '电视'}]})
          == [['192.168.1.9:8009', '电视 · 192.168.1.9', True, '电视']],
          "the label has a ' · ' in it; a key rebuilt from it never matches")
    check("and no devices is an empty list rather than None",
          _mc.device_rows({}) == [] and _mc.device_rows({'devices': None}) == [])
    check("one protocol's devices are never read out of another's row",
          _mc.device_rows(_mc.current_channel(
              _state35(output={'kind': 'dlna'},
                       channels=[{'key': 'cast', 'devices': [{'id': 'c'}]},
                                 {'key': 'dlna', 'devices': [{'id': 'd'}]}])))
          == [['d', '', False, '']],
          'the list panel follows the protocol, which is the whole redesign')

    check("the note under a device list is the plugin's own verdict",
          _mc.search_note({'words': '没有发现 Chromecast（搜于 12:00:00）'})
          == '没有发现 Chromecast（搜于 12:00:00）'
          and _mc.search_note({}) == '',
          'written once, in the plugin, so a refused start and the card under '
          'the list cannot drift apart')
    check("the first search is the only thing allowed to say 正在搜索局域网设备",
          mirror35._search_words([], True, 0.0, '')
          == '正在搜索局域网设备…'
          and mirror35._search_words([], True, 1.0, '') == '正在重新搜索…',
          'a second look has an earlier verdict behind it, and says so')
    check("a completed empty search stays dated",
          mirror35._search_words([], False, 1.0, '', '没有发现 Chromecast')
          == '没有发现 Chromecast；如果设备就在这台 Mac 的同一个网络里，'
             '检查它是否开机、再点「重搜设备」'
             + mirror._searched_suffix(1.0),
          "otherwise a list that has always been empty looks alive -- and an "
          "answer with no next step in it is a dead end, not a diagnosis")
    check("an empty search that only saw this machine says who it did see",
          '这台 Mac 自己' in mirror35._search_words(
              [], False, 1.0, '', '没有发现 DLNA 电视', alone=1)
          and mirror35._search_words(['x'], False, 1.0, '', '没有发现',
                                     alone=2) == '发现 1 台',
          'a TV that is switched off and a search that is broken read the same '
          'until the page names the answers it dropped')
    check("an answer that spoke SSDP and then served no description is not 没有发现",
          '读不到描述' in mirror35._search_words([], False, 1.0, '',
                                                 '没有发现 DLNA 电视',
                                                 unreadable=1)
          and '读不到描述' in mirror35._search_words(
              ['x'], False, 1.0, '', '没有发现', unreadable=2)
          and '发现 1 台' in mirror35._search_words(
              ['x'], False, 1.0, '', '没有发现', unreadable=2),
          'the TV answered; sending the user off to check the cable is wrong')

    # …and the counter behind that phrase really is filled by the filter, or the
    # wording above is dead code that no LAN can ever trigger.
    class _SelfOnly35:
        @staticmethod
        def get_advertisable_ip():
            return ['10.0.0.5']

    _keep35 = (mirror35.Setting, mirror35._ask_renderers,
               mirror35.describe_renderer)
    mirror35.Setting = _SelfOnly35
    mirror35._ask_renderers = lambda targets, timeout=None, interface=None: [
        ('http://10.0.0.5:58880/d.xml', '10.0.0.5'),
        ('http://10.0.0.5:58998/d.xml', '10.0.0.5'),
        ('http://10.0.0.7:58880/d.xml', '10.0.0.7'),
        ('http://10.0.0.9:58880/d.xml', '10.0.0.9')]
    mirror35.describe_renderer = lambda location, peer: (
        None if peer == '10.0.0.9'
        else ('TV', 'http://{}/AVTransport/action'.format(peer)))
    try:
        _found35 = mirror35.discover_renderers()
        check("the DLNA search counts the answers it dropped as its own",
              [host for _, _, host in _found35] == ['10.0.0.7']
              and mirror35._self_alone['dlna'] == 1,
              '%s / alone=%s' % (_found35, mirror35._self_alone['dlna']))
        check("and counts the ones that answered discovery but no description",
              mirror35._dlna_unreadable == 1,
              '10.0.0.9 answered SSDP and then failed the GET: that is the '
              'standby-TV case, and it has to be sayable')
    finally:
        (mirror35.Setting, mirror35._ask_renderers,
         mirror35.describe_renderer) = _keep35
        mirror35._self_alone['dlna'] = 0
        mirror35._dlna_unreadable = 0
    check("a search that could not run says that instead of 没有发现",
          mirror35._search_words([], False, 1.0, '组播被防火墙挡了')
          == '组播被防火墙挡了',
          'the user goes looking for a TV that is fine when this machine is the '
          'problem')
    check("and it does not leave last round's verdict standing",
          mirror35._dlna_unreadable == 0 and mirror35._self_alone['dlna'] == 0,
          'a round that failed early must not be described by the round before')
    check("and a protocol with nothing to search has no verdict to give",
          [r for r in mirror35.channels_state('browser')
           if r['key'] == 'browser'][0]['words'] == '',
          '「没有发现」about the browser target would be a lie')

    # Four presets share one row, so a pill can only carry「1080p」-- but the
    # bandwidth cost is the entire reason to pick one preset over another, so
    # dropping it with the label would remove the decision's only evidence.
    _q35 = {'current': '1080',
            'options': [{'key': k, 'label': v}
                        for k, v in sorted(mirror35.QUALITY_LABELS.items())]}
    _short35, _note35 = _mc.quality_text(_q35)
    check("each pill keeps its resolution and loses nothing else",
          _short35 == {'360': '360p', '720': '720p', '1080': '1080p',
                       'source': '原始分辨率'},
          str(_short35))
    check("with nothing overriding them, the line says what is chosen costs",
          _note35 == mirror35.QUALITY_LABELS['1080'], _note35)
    check("a preset with no list around it still reports its own cost",
          _mc.quality_text({'current': '360',
                            'options': [{'key': '360',
                                         'label': mirror35.QUALITY_LABELS['360']}]})
          [1] == mirror35.QUALITY_LABELS['360'])
    # The low-latency channel's note is the case that decides the precedence: it
    # contradicts the preset's number on purpose, so quoting both would read as
    # a bug rather than as a ceiling.
    check("a situational note wins over the preset's own number",
          _mc.quality_text(dict(_q35, note='低延迟通道上限 4.5 Mbps'))[1]
          == '低延迟通道上限 4.5 Mbps',
          "「10 Mbps · 上限 4.5 Mbps」 is a contradiction, not information")
    check("an empty quality block renders as nothing rather than as 'None'",
          _mc.quality_text({}) == ({}, ''))

    check("the audio steps' glyphs cover every state the progress machine emits",
          [_mc.audio_step_mark({'state': s})[0] for s in
           ('running', 'done', 'fail', 'skipped', 'pending')]
          == ['▶', '✓', '✗', '—', '○'])
    check("and a state it has never seen is marked rather than swallowed",
          _mc.audio_step_mark({'state': 'stalled'})[0] == '?'
          and _mc.audio_step_mark({})[0] == '?')

    check("a healthy machine produces no banner",
          _mc.banner_for(_base35) == {'text': '', 'level': None})
    check("a page written against another contract version outranks everything "
          "it could be blamed for",
          _mc.banner_for(_state35(available=False, console_version=99,
                                  capture={'probed': False}))['level']
          == _mc.WARN,
          'a stale page is the reason the other two read wrong; saying them '
          'first sends the user to enable a plugin that is already enabled')
    check("and it names both versions, not just the fact of a mismatch",
          'v99' in _mc.banner_for(_state35(console_version=99))['text']
          and 'v%d' % _mc.VIEW_VERSION
          in _mc.banner_for(_state35(console_version=99))['text'],
          str(_mc.banner_for(_state35(console_version=99))))
    check("a missing plugin is named as the problem, not a dead button",
          '没有可用的电脑投屏插件'
          in _mc.banner_for(_state35(available=False))['text'],
          _mc.banner_for(_state35(available=False))['text'])
    check("and a machine still being probed is explained while it is probed",
          '正在探测'
          in _mc.banner_for(_state35(capture={'probed': False}))['text'])

    # -- the HTTP surface: these endpoints start a capture of the desktop ----
    _token35 = protocol.api_token()

    class _Surface35(object):
        """The plugin's console face, as the page sees it over HTTP."""

        def __init__(self):
            self.state_calls = 0
            self.snaps = []
            self.actions = []
            self.frame = b'\x89PNG\r\n\x1a\nfake-frame'
            self.reason = ''

        def console_state(self):
            self.state_calls += 1
            return {'available': True, 'mirroring': False,
                    'output': {'kind': 'cast'}, 'recent': []}

        def snapshot_frame(self):
            # One entry per grab, tagged with the only raster there is: the
            # checks below ask *whether* a frame was taken, never which format.
            self.snaps.append('png')
            return self.frame, self.reason

        def console_action(self, name, args):
            self.actions.append((name, args))
            return {'code': 0, 'message': 'ok'}

    class _MirrorHandler35(protocol.Handler):
        # Not the real __init__: it loads the settings page off disk and makes a
        # local-files directory, and neither is what is under test here.
        def __init__(self, surface):
            self._surface = surface

        def _mirror_setting(self):
            # The real one resolves this off the plugin manager -- see
            # `MacastPluginManager.mirror_setting` -- because the console is not
            # a mode of whichever renderer is playing.
            return self._surface

    surface35 = _Surface35()
    handler35 = _MirrorHandler35(surface35)
    bare35 = _MirrorHandler35(None)   # the app is up, but no plugin owns the console
    request35 = cherrypy.serving.request
    response35 = cherrypy.serving.response
    _saved_req35 = (request35.params, getattr(request35, 'remote', None),
                    request35.scheme, dict(response35.headers))
    _saved_running35 = utils.Setting.is_service_running
    utils.Setting.is_service_running = staticmethod(lambda: True)

    def _reset35(ip='127.0.0.1', token=None, scheme='http'):
        request35.headers.clear()
        request35.params = {'token': token} if token else {}
        request35.remote = types.SimpleNamespace(ip=ip)
        request35.scheme = scheme
        return request35

    def _getraw35(handler=None, **kw):
        # The snapshot endpoint answers with an image, which is not JSON: the
        # checks that only look at bytes and headers ask for the raw body.
        return (handler or handler35).GET(param='api', **kw)

    def _get35(handler=None, **kw):
        raw = _getraw35(handler, **kw)
        return json.loads(raw.decode()), raw

    def _post35(**kw):
        return json.loads(handler35.POST(**kw).decode())

    try:
        _reset35()
        res, _ = _get35(query='mirror-state')
        check("mirror-state is refused without the token even from this machine",
              res['code'] == 403 and surface35.state_calls == 0, str(res))
        res, _ = _get35(query='mirror-snapshot')
        check("mirror-snapshot too, so a drive-by cannot read the desktop",
              res['code'] == 403 and surface35.snaps == [], str(res))
        res = _post35(**{'mirror-action': 'start'})
        check("and mirror-action cannot be fired by a form on some page either",
              res['code'] == 403 and surface35.actions == [], str(res))

        _reset35(ip='192.168.1.42')
        res, _ = _get35(query='mirror-state')
        check("a LAN caller without the token gets the same answer",
              res['code'] == 403 and surface35.state_calls == 0, str(res))

        _reset35(token=_token35)
        res, _ = _get35(query='mirror-state')
        check("the token in the query opens the state read",
              res.get('code') == 0 and res['state']['available'] is True
              and surface35.state_calls == 1, str(res))

        _reset35()
        request35.params = {'token': [_token35, _token35]}
        res = _post35(**{'mirror-action': 'set-quality',
                         'mirror-args': '{"value": "720"}'})
        check("the same token in the query *and* the form body is one token, not a "
              "wrong one",
              res.get('code') == 0 and surface35.actions[-1][0] == 'set-quality',
              "%s -- CherryPy hands a repeated key back as a list, and comparing "
              "that list to a string 403s a caller who is right" % res)
        _reset35()
        request35.params = {'token': ['wrong', _token35]}
        res = _post35(**{'mirror-action': 'start'})
        check("and only the value the caller led with counts",
              res['code'] == 403 and surface35.actions[-1][0] == 'set-quality',
              str(res))

        _reset35()
        request35.headers['X-Macast-Token'] = _token35
        raw = _getraw35(query='mirror-snapshot')
        check("the token header works too, and the frame is the raw image",
              raw == surface35.frame
              and response35.headers.get('Content-Type') == 'image/png',
              "%r / %s" % (raw, response35.headers.get('Content-Type')))
        check("with its length in bytes and the caches told to stay out",
              response35.headers.get('Content-Length') == str(len(raw))
              and response35.headers.get('Cache-Control') == 'no-store'
              and response35.headers.get('X-Content-Type-Options') == 'nosniff',
              "the frame is the user's desktop, seconds ago")

        _reset35(token=_token35)
        _getraw35(query='mirror-snapshot')
        check("the frame is served as the PNG the plugin grabs",
              surface35.snaps[-1] == 'png'
              and response35.headers.get('Content-Type') == 'image/png',
              str(response35.headers.get('Content-Type')))
        surface35.snaps[:] = []
        _getraw35(query='mirror-snapshot', fmt='ppm')
        check("a stale page that still asks for a format is answered, not refused",
              len(surface35.snaps) == 1
              and response35.headers.get('Content-Type') == 'image/png',
              "the caller is a one-second poll loop; a leftover query string "
              "must not cost it the panel")

        surface35.frame, surface35.reason = b'', '找不到 ffmpeg，无法生成预览'
        _, raw = _get35(query='mirror-snapshot')
        check("no frame answers with the reason, and not as an image",
              json.loads(raw.decode())['message'] == '找不到 ffmpeg，无法生成预览'
              and response35.headers.get('Content-Type').startswith('application/json'),
              "%r" % raw)
        surface35.frame, surface35.reason = b'\x89PNG\r\n\x1a\nfake-frame', ''

        _, raw = _get35(bare35, query='mirror-state')
        check("while the app is still booting the window is told that",
              json.loads(raw.decode())['message'] == '应用还在启动，请一秒后再试',
              "%s -- 'go enable a plugin' is the wrong advice for a two-second "
              "state" % raw)
        # A manager that answers means the app is up: the same `None` surface is
        # then the *other* problem, and the sentence has to change.
        _mgr35 = types.SimpleNamespace(mirror_setting=lambda: None)
        _get_mgr35 = lambda: _mgr35                                     # noqa: E731
        cherrypy.engine.subscribe('get_plugin_manager', _get_mgr35)
        try:
            _, raw = _get35(bare35, query='mirror-state')
            check("with the plugin switched off the window is told which problem",
                  json.loads(raw.decode())['message']
                  .startswith('没有可用的电脑投屏插件'), str(raw))
            _, raw = _get35(bare35, query='mirror-snapshot')
            check("and the preview answers the same way, not as a broken image",
                  response35.headers.get('Content-Type')
                  .startswith('application/json')
                  and '没有可用的电脑投屏插件'
                  in json.loads(raw.decode())['message'], str(raw))
            res = json.loads(bare35.POST(**{'mirror-action': 'start'}).decode())
            check("an action is refused with the same sentence",
                  res['code'] == 1 and '没有可用的电脑投屏插件' in res['message'],
                  str(res))
        finally:
            try:
                cherrypy.engine.unsubscribe('get_plugin_manager', _get_mgr35)
            except Exception:
                pass

        _reset35()                      # the token is gone again
        _seen35 = len(surface35.actions)
        res = _post35(**{'mirror-action': 'start'})
        check("a refusal on the gate never reaches the plugin, checked again "
              "after the authorised calls",
              res['code'] == 403 and len(surface35.actions) == _seen35, str(res))
        _reset35(token=_token35)
        res = _post35(**{'mirror-action': 'set-quality',
                         'mirror-args': '{"value": "1080p"}'})
        check("a console action reaches the plugin with its arguments parsed",
              surface35.actions[-1] == ('set-quality', {'value': '1080p'})
              and res['code'] == 0, str(surface35.actions))
        res = _post35(**{'mirror-action': 'start', 'mirror-args': 'not json'})
        check("malformed arguments are refused before the plugin is asked",
              res['code'] == 1 and '不是合法的 JSON' in res['message'], str(res))
        res = _post35(**{'mirror-action': 'start', 'mirror-args': '["start"]'})
        check("so is a value that is not an object",
              res['code'] == 1 and '必须是 JSON 对象' in res['message'], str(res))
        res = _post35(**{'mirror-action': 'set-target', 'mirror-args': '{"id": "x"}'})
        check("arguments travel as the dict the plugin validates",
              surface35.actions[-1] == ('set-target', {'id': 'x'}), str(res))
        check("and the only state read that ever happened was the authorised one",
              surface35.state_calls == 1, str(surface35.state_calls))
    finally:
        request35.params, request35.remote, request35.scheme = _saved_req35[:3]
        response35.headers.clear()
        for _k35, _v35 in _saved_req35[3].items():
            response35.headers[_k35] = _v35
        utils.Setting.is_service_running = _saved_running35

    # -- the preview: throttling, retention, and what a frame really is ------
    def _reset35snap():
        mirror35._snapshot.update({'frame': b'', 'at': 0.0,
                                   'busy': False, 'failed_at': 0.0,
                                   'reason': ''})

    _grabs35 = []
    _saved_grab35 = mirror35._grab_snapshot

    def _fake_grab35():
        _grabs35.append('png')
        return b'\x89PNG\x00\x01', ''

    _reset35snap()
    mirror35._grab_snapshot = _fake_grab35
    try:
        frame, reason = mirror35.snapshot_frame(min_interval=60)
        check("the first preview request really grabs a frame",
              frame.startswith(b'\x89PNG') and reason == '' and _grabs35 == ['png'],
              str(_grabs35))
        frame, reason = mirror35.snapshot_frame(min_interval=60)
        check("the next one within the interval gets the cached frame",
              _grabs35 == ['png'] and frame.startswith(b'\x89PNG'), str(_grabs35))
        _grabs35[:] = []
        for _ in range(3):
            mirror35.snapshot_frame(min_interval=0)
        check("once the interval has passed every poll gets a fresh frame",
              len(_grabs35) == 3, str(_grabs35))

        _reset35snap()
        mirror35.snapshot_frame(min_interval=0)
        _grabs35[:] = []
        _refuses35 = [True]

        def _flaky_grab35():
            _grabs35.append('png')
            if _refuses35[0]:
                return b'', '屏幕录制权限被拒'
            return b'\x89PNG\x00\x02', ''

        mirror35._grab_snapshot = _flaky_grab35
        frame, reason = mirror35.snapshot_frame(min_interval=0)
        check("a failed grab keeps showing the last good picture",
              frame.startswith(b'\x89PNG') and reason == '屏幕录制权限被拒',
              "a black rectangle where the desktop was is not an answer")
        _grabs35[:] = []
        frame, reason = mirror35.snapshot_frame(min_interval=0)
        check("and the next poll backs off instead of spawning ffmpeg every second",
              _grabs35 == [] and reason == '屏幕录制权限被拒', str(_grabs35))
        mirror35._snapshot['failed_at'] = 0.0     # time passes
        _refuses35[0] = False
        frame, reason = mirror35.snapshot_frame(min_interval=0)
        check("once the backoff window passes it tries again, and a good frame "
              "replaces the reason",
              _grabs35 == ['png'] and reason == ''
              and mirror35._snapshot['failed_at'] == 0.0, str(_grabs35))
        _grabs35[:] = []
        mirror35._snapshot['busy'] = True
        frame, reason = mirror35.snapshot_frame(min_interval=0)
        check("a second concurrent reader waits for the frame in flight",
              _grabs35 == [] and reason == '正在采集上一帧'
              and frame.startswith(b'\x89PNG'), str(_grabs35))
        mirror35._snapshot['busy'] = False
        _snap35 = mirror35.snapshot_state()
        check("the state describes the preview without asking for a frame",
              _snap35['has_frame'] and _snap35['busy'] is False
              and _grabs35 == [], str(_snap35))
        check("and never leaks the bytes themselves into the state",
              'frame' not in _snap35, str(sorted(_snap35)))
        _grabs35[:] = []
        mirror35.snapshot_frame(min_interval=0)
        check("after a success the retry window is forgotten",
              mirror35._snapshot['failed_at'] == 0.0 and _grabs35 == ['png'])

        # The real grab now, with only ffmpeg faked: this is where the argv
        # contract lives.
        _cap35 = types.SimpleNamespace(
            inputs=[['-f', 'avfoundation', '-capture_cursor', '1', '-i', '1:none']],
            audio_map=None, label='内置显示器', screens=[(1, '内置显示器')])
        _written35 = []
        _payload35 = [b'\x89PNG\r\n\x1a\nIHDR', 0]
        _saved_run35 = mirror35.subprocess.run

        def _fake_run35(cmd, **kw):
            _written35.append(list(cmd))
            with open(cmd[-1], 'wb') as _fh:
                _fh.write(_payload35[0])
            return types.SimpleNamespace(returncode=_payload35[1], stderr=b'')

        mirror35._grab_snapshot = _saved_grab35
        _patch35(find_ffmpeg=lambda: '/usr/bin/ffmpeg',
                 probe_capture=lambda *a, **k: _cap35)
        mirror35.subprocess.run = _fake_run35
        try:
            frame, reason = mirror35._grab_snapshot()
            check("a PNG frame comes back with its magic bytes intact",
                  frame.startswith(b'\x89PNG') and reason == '', str(reason))
            _argv35 = _written35[-1]
            check("the grab is one frame of the same source the mirror uses",
                  _argv35[:4] == ['/usr/bin/ffmpeg', '-hide_banner',
                                  '-loglevel', 'error']
                  and '1:none' in _argv35 and '0:v:0' in _argv35
                  and '-frames:v' in _argv35, str(_argv35))
            check("and it is scaled to the width the console was told about",
                  _argv35[_argv35.index('-vf') + 1]
                  == 'scale={}:-2'.format(mirror35.SNAPSHOT_WIDTH), str(_argv35))
            check("PNG names no encoder: the extension is enough",
                  '-c:v' not in _argv35 and '-f image2' not in _argv35,
                  str(_argv35))
            check("the frame file is named .png, because that is the magic the "
                  "reader checks for",
                  _written35[-1][-1].endswith('.png'), str(_written35[-1][-1]))
            check("and it is gone once the bytes have been read",
                  not os.path.exists(_written35[-1][-1]),
                  "a stray frame is a screenshot of the user's desktop left in /tmp")
            _payload35[0], _payload35[1] = b'garbage', 0
            frame, reason = mirror35._grab_snapshot()
            check("bytes that are not the promised image are refused",
                  frame == b'' and 'PNG' in reason, str((frame, reason)))
            _payload35[0], _payload35[1] = b'', 1
            frame, reason = mirror35._grab_snapshot()
            check("a failing ffmpeg says which exit code it used",
                  frame == b'' and '退出码 1' in reason, str(reason))
            _patch35(find_ffmpeg=lambda: None)
            frame, reason = mirror35._grab_snapshot()
            check("no ffmpeg at all is named as the reason, not a black box",
                  'ffmpeg' in reason and frame == b'', str(reason))
            _patch35(find_ffmpeg=lambda: '/usr/bin/ffmpeg',
                     probe_capture=lambda *a, **k: None,
                     capture_unavailable_hint=lambda: '这台电脑还不能采集屏幕')
            frame, reason = mirror35._grab_snapshot()
            check("an unprobeable machine gets the plugin's own hint",
                  reason == '这台电脑还不能采集屏幕', str(reason))
        finally:
            mirror35.subprocess.run = _saved_run35
            _unpatch35()
        _reset35snap()
    finally:
        mirror35._grab_snapshot = _saved_grab35

    # -- 「正在抓取第一帧…」has to be a phase, not an answer ------------------
    #
    # The preview's <img> only exists in the DOM once a frame does, so nothing in
    # the page ever asked for the *first* one: a freshly started app showed the
    # spinner forever. The state endpoint now sponsors that grab -- off its own
    # thread, because a grab is an ffmpeg -- and stands down while the mirror
    # holds the display, where a second capture of the same screen costs the full
    # timeout and returns nothing.
    _grabs35p = []

    def _preview_grab35():
        _grabs35p.append('png')
        return b'\x89PNG\x00\x09', ''

    def _setting35p(renderer):
        _s35 = mirror35.ScreenMirrorSetting()
        _s35._renderer = lambda: renderer
        return _s35

    _live35 = types.SimpleNamespace(is_mirroring=lambda: True,
                                    is_starting=lambda: False,
                                    stats=lambda: {},
                                    viewer_url=lambda: '')
    _protocol_file35 = open(protocol.__file__, encoding='utf-8').read()
    _patch35(_grab_snapshot=_preview_grab35,
             start_search=lambda: None,
             start_renderer_search=lambda: None)
    try:
        _reset35snap()
        check("an empty preview is a grab somebody has yet to make",
              mirror35.snapshot_wanted() is True, str(mirror35.snapshot_state()))
        check("the page's first read sponsors it",
              _setting35p(None).request_preview() is True)
        check("and the frame really arrives, with nobody having asked "
              "mirror-snapshot",
              _wait_until(lambda: bool(mirror35._snapshot['frame'])
                          and not mirror35._snapshot['busy'], timeout=5)
              and mirror35.snapshot_state()['has_frame'] is True, str(_grabs35p))
        _grabs35p[:] = []
        check("a page that already has a frame asks for nothing",
              _setting35p(None).request_preview() is False and _grabs35p == [],
              "the page polls once a second; an unconditional kick spawns "
              "ffmpeg every second")
        _grabs35p[:] = []
        mirror35._snapshot['busy'] = True
        check("and so does a grab already in flight",
              _setting35p(None).request_preview() is False and _grabs35p == [])
        mirror35._snapshot['busy'] = False
        _reset35snap()
        mirror35._snapshot.update({'failed_at': time.time(),
                                   'reason': '屏幕录制权限被拒'})
        check("a failed grab backs off rather than re-prompting every second",
              _setting35p(None).request_preview() is False and _grabs35p == [],
              str(_grabs35p))
        _reset35snap()
        check("while the mirror owns the display the preview takes no frame",
              _setting35p(_live35).request_preview() is False
              and _grabs35p == [],
              "measured on macOS: the second capture produces nothing after "
              "the full timeout, so the card would be spinning at the user")
        _reset35snap()
        mirror35._snapshot.update({'frame': b'\x89PNG\x00\x0a',
                                   'at': time.time(), 'reason': ''})
        _pv35 = _setting35p(_live35).console_state()['preview']
        check("so a running mirror shows its own reason, not a stale picture "
              "presented as live",
              _pv35['has_frame'] is False
              and _pv35['reason'] == mirror35.MIRRORING_PREVIEW_NOTE, str(_pv35))
        _idle35 = _setting35p(None).console_state()['preview']
        check("and the same cache is the preview again the moment it stops",
              _idle35['has_frame'] is True and _idle35['reason'] == '',
              str(_idle35))
        _grabs35p[:] = []
        _frame35, _why35 = _setting35p(_live35).snapshot_frame()
        check("the snapshot endpoint refuses on the same verdict, not in "
              "different words",
              _frame35 == b'' and _why35 == mirror35.MIRRORING_PREVIEW_NOTE
              and _grabs35p == [], str(_why35))
        _frame35, _why35 = _setting35p(None).snapshot_frame()
        check("idle, it still hands over the picture",
              _frame35.startswith(b'\x89PNG') and _why35 == '', str(_why35))
        check("...and it is the endpoint that asks, not the state builder",
              'request_preview' in _protocol_file35
              and 'request_preview' not in open(mirror35.__file__,
                                                encoding='utf-8').read()
              .split('def console_state')[1].split('def snapshot_frame')[0],
              'console_state() is what the suite drives directly; a thread '
              'started in there races every later assertion')
    finally:
        _reset35snap()
        _unpatch35()

    # -- what the page says to the app, in the app's own words ----------------
    #
    # The page and the server are two languages with no import between them, so a
    # name that drifts between them is not a TypeError. It is a button that does
    # nothing, or a panel nobody ever sees. So every spelling the page sends is
    # looked for in the Python that reads it, and every value the core can emit is
    # looked for in the page that has to render it.
    _page_src35 = open(os.path.join(MACAST, 'xml', 'setting.html'),
                       encoding='utf-8').read()
    _plugin_src35 = open(mirror35.__file__, encoding='utf-8').read()
    _protocol_src35 = open(protocol.__file__, encoding='utf-8').read()
    _view_src35 = open(_mc.__file__, encoding='utf-8').read()
    _flat35 = ' '.join(_page_src35.split())          # template literals span lines
    # `'/a' + '/b'` is one URL split across lines: glue adjacent string literals so
    # a guard can ask about the whole query string without caring where the page
    # ran out of line width.
    _joined35 = _re35.sub(r"['\"`]\s*\+\s*['\"`]", '', _flat35)
    _tab35 = _page_src35.split('label="电脑投屏"', 1)[1].split(
        'label="高级设置"', 1)[0]

    _queries35 = set(_re35.findall(r"query=(mirror-[\w-]+)", _page_src35))
    check("every mirror endpoint the page calls is one the server answers",
          _queries35 == {'mirror-state', 'mirror-snapshot'}
          and all(q in _protocol_src35 for q in _queries35),
          str(sorted(_queries35)))
    check("and it asks for them with the token the gating requires",
          'query=mirror-state&token=' in _joined35
          and 'query=mirror-snapshot&token=' in _joined35
          and "fd.append('token', this.cast_info.token)" in _flat35,
          "all three mirror endpoints answer 403 to a loopback caller without a "
          "token, which is the point of them")
    for _field35 in ('mirror-action', 'mirror-args'):
        check("the POST field %s is spelled the same on both sides" % _field35,
              _field35 in _page_src35 and _field35 in _protocol_src35)

    _actions35 = set(_re35.findall(r"mirror_run\('([\w-]+)'", _page_src35))
    _offered35 = set(mirror35.ScreenMirrorSetting.CONSOLE_ACTIONS)
    check("every action the page can name is one the plugin offers",
          _actions35 and _actions35 <= _offered35,
          'a renamed action is a dead button: %s'
          % sorted(_actions35 - _offered35))
    check("and the plugin offers nothing the page has no way to reach",
          _offered35 - _actions35 == {'toggle'},
          "toggle belongs to the menu bar, which says 开始/停止 in one row")

    # The layout decision lives in the core on purpose, which makes this the one
    # drift that cannot be noticed from the page: a section nobody renders.
    _sections35 = set(_re35.findall(r"sec === '([\w-]+)'", _tab35))
    check("the page can render every panel the core may ask for",
          _sections35 == set(_mc.SECTION_ORDER),
          'SECTION_ORDER entries with no branch in the tab are invisible panels: '
          '%s' % sorted(set(_mc.SECTION_ORDER) - _sections35))
    _levels35 = (_mc.GOOD, _mc.BAD, _mc.WARN, _mc.MUTED, _mc.ACCENT, _mc.DIM)
    check("and every severity those rules can emit has a colour in the page",
          all('.lv-%s' % lv in _page_src35 for lv in _levels35),
          str([lv for lv in _levels35 if '.lv-%s' % lv not in _page_src35]))

    check("「连不上 Macast」is the page's to say, and it does say it",
          '连不上 Macast' in _tab35 and 'mirror_offline' in _tab35,
          "banner_for() gave that sentence up to the page; if the page never had "
          "it either, a stopped app would read as a broken panel")
    check("the preview fields the page reads are ones the plugin reports",
          {'has_frame', 'reason', 'at', 'busy'} <= set(mirror35.snapshot_state())
          and 'frame' not in mirror35.snapshot_state(),
          str(sorted(mirror35.snapshot_state())))
    check("the view layer holds no Tk anywhere, so a headless server can import it",
          'tkinter' not in _view_src35 and 'Tk' not in _view_src35,
          'the console window is gone; this module is what replaced it')

    # -- the deleted desktop window stays deleted ------------------------------
    # §4.3 and §4.4 are one story told twice: a packaging list copied per build
    # job drifts, and the artefact that drops a piece is the piece nobody notices.
    # The reverse holds too -- a line left behind for a file nobody ships is a
    # build that fails on the day the file is finally removed.
    with open(os.path.join(REPO, 'scripts', 'setup_py2app.py'),
              encoding='utf-8') as fh:
        _p2a35 = fh.read()
    with open(os.path.join(REPO, '.github', 'workflows', 'build.yml'),
              encoding='utf-8') as fh:
        _wf35 = fh.read()
    _stale35 = [n for n, src in (('setup_py2app.py', _p2a35),
                                 ('build.yml', _wf35),
                                 ('screen_mirror.py', _plugin_src35),
                                 ('protocol.py', _protocol_src35))
                if 'mirror_console' in src or 'config_window' in src]
    check("no build file or app module still names the desktop console",
          not _stale35 and not os.path.exists(os.path.join(MACAST,
                                                           'mirror_console.py')),
          str(_stale35))
    check("and the .app still ships no Tk of its own",
          "'tkinter'" in _p2a35.split("'excludes': [", 1)[-1].split('],', 1)[0],
          'nothing in the app opens a window; the control surface is the page')
except Exception as _e35:
    import traceback
    traceback.print_exc()
    check("the mirror console window behaves", False,
          "{}: {}".format(type(_e35).__name__, _e35))
finally:
    _unpatch35()
    utils.SETTING_DIR = _saved_dir35
    utils.Setting.setting, utils.Setting.setting_path = _saved_setting35
    try:
        cherrypy.engine.unsubscribe('app_notify', _notify35_rec)
    except Exception:
        pass


# --------------------------------------------------------------------------
# Part 36: the menu bar has a menu again.
#
# Deleting the Tk console (224d6d6) also replaced `build_app_menu()` with `[]`
# in the call that constructs the tray app, so the status-bar icon stopped
# having any menu at all -- and nothing caught it, because until this Part no
# check ever built one. So: build the real top level from the real methods and
# say what belongs in it. The service switch, the playback rows, the door to
# the settings page, the replay history. And no 电脑投屏 row anywhere -- that
# surface lives in the page now, which is the user's own ruling, not an
# oversight: 「打开设置页」is one click and the page stops a live capture.
# --------------------------------------------------------------------------
print("\n=== Part 36: the menu bar's own contents ===")

_saved_setting36 = (utils.Setting.setting, utils.Setting.setting_path)
_saved_getip36 = utils.Setting.__dict__['get_ip']
_saved_clip36 = getattr(macast_mod, 'pyperclip', None)
_tmp36 = _tempfile.mkdtemp(prefix="macast-menu-")
_notify36 = []
_notify36_rec = lambda *a, **k: _notify36.append(a)          # noqa: E731


class _PlayerSetting36(object):
    """The renderer's own submenu -- the app splices it in, contents aside."""

    def build_menu(self):
        return [macast_mod.MenuItem('Player Size')]


class _Renderer36(object):
    def __init__(self):
        self.stops = 0
        self.broken = False
        self.renderer_setting = _PlayerSetting36()

    def set_media_stop(self):
        if self.broken:
            raise RuntimeError('ipc socket gone')
        self.stops += 1


class _Protocol36(object):
    """Enough of a protocol for the menu: a title to show, a uri to re-cast."""

    def __init__(self, title=u'示例影片'):
        self.title = title
        self.cast = []

    def get_state_title(self):
        return self.title

    def cast_uri(self, uri, title=''):
        self.cast.append((uri, title))


class _App36(object):
    """A `Macast` without a tray backend, a player or a CherryPy tree.

    Every menu method on it *is* the real one, lifted off `Macast` by name, so
    this exercises the app's menu code rather than a paraphrase of it. What is
    stood in is rumps (there is no Cocoa run loop to build a native menu on)
    and mpv.
    """

    def __init__(self, **over):
        self.playing_uri = ''
        self.mode = 'tray'
        self.service = types.SimpleNamespace(renderer=_Renderer36(),
                                             protocol=_Protocol36())
        self.plugin_manager = types.SimpleNamespace(
            renderer_list=[types.SimpleNamespace(title=t)
                           for t in ('MPV', 'IINA')],
            protocol_list=[types.SimpleNamespace(title=t)
                           for t in ('DLNA', 'Chromecast')])
        self.enabled_protocols = ['DLNA']
        self.setting_renderer = 'MPV'
        self.setting_menubar_icon = 0
        self.setting_check = 1
        self.setting_start_at_login = 0
        self.menu = []
        self.rebuilds = 0
        self.opened = []
        self.deferred = []
        self.toggles = 0
        self.quits = 0
        for _k, _v in over.items():
            setattr(self, _k, _v)

    # -- the tray backend, as far as these methods use it --------------------
    def set_menu(self, menu):
        self.menu = menu
        self.rebuilds += 1

    def update_menu(self):
        pass

    def call_on_main_thread(self, fn):
        self.deferred.append(fn)
        fn()

    def open_browser(self, url):
        self.opened.append(url)

    def notification(self, title, msg, sound=True):
        cherrypy.engine.publish('app_notify', title, msg)

    # -- what the App base class would supply --------------------------------
    def quit(self, item=None):
        self.quits += 1

    # -- callbacks the menu only needs to exist to be wired ------------------
    def on_toggle_service_click(self, item):
        self.toggles += 1

    def on_renderer_change_click(self, item):
        pass

    def on_protocol_toggle_click(self, item):
        pass

    def on_menubar_icon_change_click(self, item):
        pass

    def on_auto_check_update_click(self, item):
        pass

    def on_start_at_login_click(self, item):
        pass

    def on_open_config_click(self, item):
        pass

    def on_check_click(self, item):
        pass

    def on_about_click(self, item):
        pass


for _n36 in ('build_app_menu', 'build_setting_menu', '_playback_menu_rows',
             'build_history_menu', '_refresh_app_menu', '_apply_app_menu',
             'renderer_av_uri', 'renderer_av_stop', 'on_open_page_click',
             'on_stop_playback_click', 'on_copy_uri_click', 'on_history_click',
             'on_clear_history_click', 'update_service_status'):
    setattr(_App36, _n36, getattr(macast_mod.Macast, _n36))
#: A staticmethod object, not the bare function: assigning the function to a
#: class would turn `self` into its first argument.
_App36._short_label = macast_mod.Macast.__dict__['_short_label']


def _rows36(items, depth=0):
    """(depth, text, enabled) for every row in a built menu, recursively."""
    out = []
    for item in items:
        if item is None:
            continue
        out.append((depth, item.text, item.enabled))
        if getattr(item, 'children', None):
            out.extend(_rows36(item.children, depth + 1))
    return out


def _top36(app):
    return [t for d, t, _e in _rows36(app.menu) if d == 0]


def _all36(app):
    return [t for _d, t, _e in _rows36(app.menu)]


def _history_rows36(app):
    """The live children of the 投屏历史 submenu, separators dropped."""
    for item in app.menu:
        if item is not None and item.text == 'Play History':
            return [i for i in item.children if i is not None]
    return []


class _Clip36(object):
    def __init__(self):
        self.copied = []

    def copy(self, text):
        self.copied.append(text)


try:
    utils.SETTING_DIR = _tmp36
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp36, "macast_setting.json")
    utils.Setting.get_ip = staticmethod(lambda: [('192.0.2.7', 'en0')])
    cherrypy.engine.subscribe('app_notify', _notify36_rec)

    _app36 = _App36()
    _app36._apply_app_menu()

    check("the status-bar icon has a menu at all",
          len(_app36.menu) > 0 and _app36.rebuilds == 1, str(_top36(_app36)))
    check("the service switch still leads and 退出 still follows the settings",
          _top36(_app36)[0] == ('Stop Cast' if utils.Setting.is_service_running()
                                else 'Start Cast')
          and _top36(_app36)[-1] == 'Quit'
          and 'Setting' in _top36(_app36), str(_top36(_app36)))
    check("the door to the settings page is on the top level, not buried",
          'Open Settings Page' in _top36(_app36), str(_top36(_app36)))
    _app36.on_open_page_click(None)
    check("...and it opens this app's own port on the loopback interface",
          _app36.opened == ['http://127.0.0.1:{}'.format(
              macast_mod.Setting.get_port())],
          str(_app36.opened))

    # -- the empty-menu regression itself -----------------------------------
    with open(os.path.join(REPO, "macast", "macast.py"), encoding="utf-8") as _f36:
        _src36 = _f36.read()
    _init36 = _re35.search(r"super\(Macast, self\)\.__init__\(.*?\)", _src36,
                           _re35.S)
    check("the menu is built when the app is constructed, not after a toggle",
          _init36 is not None and 'build_app_menu()' in _init36.group(0),
          _init36.group(0) if _init36 else 'no super().__init__ call found')
    check("and no call hands the tray an empty list again",
          'icon_path, [], template' not in _src36
          and ', [], template)' not in _src36,
          'that is how the menu bar lost its menu in 224d6d6')

    # -- the playback rows ---------------------------------------------------
    check("while nothing is playing there is nothing to stop or copy",
          'Stop Playback' not in _all36(_app36)
          and 'Copy Video URI' not in _all36(_app36)
          and not any(t.startswith('Now playing') for t in _all36(_app36)),
          str(_all36(_app36)))
    _before36 = len(_app36.deferred)
    _app36.renderer_av_uri('http://192.0.2.7:58880/movie.mp4')
    check("media arriving puts the title, a stop and a copy on the menu",
          'Stop Playback' in _all36(_app36)
          and 'Copy Video URI' in _all36(_app36)
          and 'Now playing: 示例影片' in _all36(_app36), str(_all36(_app36)))
    check("the rebuild is marshalled to the thread that owns the menu",
          len(_app36.deferred) == _before36 + 1
          and getattr(_app36.deferred[-1], '__name__', '') == '_apply_app_menu',
          'the player reports media on its own IPC thread; AppKit must not be '
          'touched from there (AGENTS.md 4.2)')
    _before36 = len(_app36.deferred)
    _app36.renderer_av_uri('http://192.0.2.7:58880/movie.mp4')
    check("the same media twice does not rebuild the menu again",
          len(_app36.deferred) == _before36, str(len(_app36.deferred)))
    _app36.renderer_av_uri('http://192.0.2.7:58880/next.mkv')
    check("a track change rebuilds it, because the label carries the media",
          len(_app36.deferred) == _before36 + 1
          and _app36.playing_uri.endswith('next.mkv'), str(len(_app36.deferred)))
    _app36.on_stop_playback_click(None)
    check("「停止播放」tells the player to stop",
          _app36.service.renderer.stops == 1, str(_app36.service.renderer.stops))
    _notify36[:] = []
    _app36.service.renderer.broken = True
    _app36.on_stop_playback_click(None)
    check("and when the player cannot hear it, that is said out loud",
          len(_notify36) == 1 and _notify36[0][0] == 'Error'
          and _notify36[0][1] == 'Stop playback failed', str(_notify36))
    _app36.service.renderer.broken = False
    _clip36 = _Clip36()
    macast_mod.pyperclip = _clip36
    _app36.on_copy_uri_click(None)
    check("「复制视频链接」copies what is actually playing",
          _clip36.copied == ['http://192.0.2.7:58880/next.mkv'],
          str(_clip36.copied))
    _app36.renderer_av_stop()
    check("when the media ends the rows go away with it",
          _app36.playing_uri == ''
          and 'Stop Playback' not in _all36(_app36)
          and 'Copy Video URI' not in _all36(_app36), str(_all36(_app36)))

    # -- the replay history --------------------------------------------------
    check("with no history the submenu is absent, not empty",
          'Play History' not in _top36(_app36), str(_top36(_app36)))
    check("...because reading the key must not create it",
          'Play_History' not in utils.Setting.setting,
          'Setting.get() writes its default into the settings -- AGENTS.md 4.2')
    utils.Setting.setting['Play_History'] = [
        {'uri': 'http://a/one.mp4', 'title': u'第一部', 'time': 3},
        {'uri': 'http://a/two.mp4', 'title': u'第二部', 'time': 2},
        {'uri': 'http://a/three.mp4', 'time': 1},
    ]
    _app36._apply_app_menu()
    _hist36 = _history_rows36(_app36)
    check("the last few casts come back as rows, newest first",
          [i.text for i in _hist36][:3] == [u'第一部', u'第二部',
                                            'http://a/three.mp4'],
          str([i.text for i in _hist36]))
    check("an entry without a title falls back to its url, not to blank",
          _hist36[2].data == 'http://a/three.mp4', str(_hist36[2].text))
    _app36.on_history_click(_hist36[0])
    check("clicking one casts it again, through the protocol that owns casting",
          _app36.service.protocol.cast == [('http://a/one.mp4', '')],
          str(_app36.service.protocol.cast))

    _long36 = u'影片' * 40
    utils.Setting.setting['Play_History'] = [
        {'uri': 'http://a/long.mp4', 'title': _long36, 'time': 9}]
    _app36._apply_app_menu()
    _hist36 = _history_rows36(_app36)
    _label36 = _hist36[0].text
    check("a sender-provided title is bounded to one line",
          len(_label36) <= 57 and '\n' not in _label36 and _label36.endswith('…'),
          '{} chars: {!r}'.format(len(_label36), _label36[:70]))
    check("...but the row still casts the real uri",
          _hist36[0].data == 'http://a/long.mp4', _label36)

    utils.Setting.setting['Play_History'] = [
        {'uri': 'http://a/{}.mp4'.format(n), 'title': u'同名影片', 'time': n}
        for n in range(20)]
    _app36._apply_app_menu()
    _many36 = _history_rows36(_app36)
    check("the menu offers a window onto the history, not all of it",
          len([j for j in _many36 if j.data]) == macast_mod.HISTORY_MENU_LIMIT,
          str(len([j for j in _many36 if j.data])))
    check("two entries with the same title stay two rows",
          len(set(j.text for j in _many36 if j.data))
          == len([j for j in _many36 if j.data])
          and len(set(j.data for j in _many36 if j.data))
          == len([j for j in _many36 if j.data]),
          'rumps keys a menu by title, so equal labels would silently merge -- '
          + str([j.text for j in _many36]))

    _clear36 = [j for j in _many36 if j.text == 'Clear Play History'][0]
    check("the submenu ends with a way to empty it",
          _clear36 is not None, str([j.text for j in _many36]))
    _app36.on_clear_history_click(_clear36)
    check("and it really empties the stored history, then re-makes the menu",
          utils.Setting.setting['Play_History'] == []
          and 'Play History' not in _top36(_app36), str(_top36(_app36)))

    # -- what must NOT be in the menu ---------------------------------------
    utils.Setting.setting['Play_History'] = []
    _app36.playing_uri = 'http://a/one.mp4'
    _app36._apply_app_menu()
    # The console's own vocabulary. Any of these on a menu row means the mirror
    # surface crept back out of the settings page.
    _console36 = (u'镜像', u'投屏方式', u'画质', u'低延迟', u'观看地址', u'采集')
    check("电脑投屏 is not in the menu bar at all, playing or not",
          not any(w in t for t in _all36(_app36) for w in _console36),
          str(_all36(_app36)))
    check("the only 投屏 rows left are the replay history's",
          not any(u'投屏' in t for t in _all36(_app36)
                  if t not in ('Play History', 'Clear Play History')),
          str(_all36(_app36)))
    check("the app keeps no mirror-menu code to drift back from",
          not hasattr(macast_mod.Macast, '_mirror_menu_rows')
          and not hasattr(macast_mod.Macast, 'on_open_mirror_page_clicked')
          and 'mirror_setting' not in _src36.split('def build_setting_menu')[1]
                                                .split('    def ')[0],
          'the Setting submenu must not reach for the console again')
    check("removing those rows did not eat the switches beside them",
          'Renderers' in _all36(_app36) and 'Protocols' in _all36(_app36)
          and 'Menubar Icon' in _all36(_app36)
          and 'Advanced Setting' in _all36(_app36), str(_all36(_app36)))

    # -- the translations the menu shows ------------------------------------
    _used36 = set(_re35.findall(r"_\(\s*'((?:[^'\\]|\\.)*)'\s*\)", _src36))
    _used36 |= set(_re35.findall(r'_\(\s*"((?:[^"\\]|\\.)*)"\s*\)', _src36))
    with open(os.path.join(REPO, "i18n", "zh_CN", "LC_MESSAGES", "macast.po"),
              encoding="utf-8") as _f36b:
        _po36 = _f36b.read()
    _have36 = set(_re35.findall(r'^msgid "(.*)"', _po36, _re35.M))
    _lettered36 = {u for u in _used36 if any(c.isalpha() for c in u)}
    check("every string the menu can show has a Chinese translation",
          not (_lettered36 - _have36),
          'missing: {}'.format(sorted(_lettered36 - _have36)))
    check("and the po file still compiles",
          not any(l.startswith('msgstr') and l.endswith('" "')
                  for l in _po36.splitlines()),
          'a msgid/msgstr pair that msgfmt would reject also breaks the build')
except Exception as _e36:
    import traceback
    traceback.print_exc()
    check("the menu bar builds", False, "{}: {}".format(type(_e36).__name__, _e36))
finally:
    try:
        cherrypy.engine.unsubscribe('app_notify', _notify36_rec)
    except Exception:
        pass
    utils.Setting.get_ip = _saved_getip36
    if _saved_clip36 is not None:
        macast_mod.pyperclip = _saved_clip36
    utils.Setting.setting, utils.Setting.setting_path = _saved_setting36
    _shutil.rmtree(_tmp36, ignore_errors=True)


# --------------------------------------------------------------------------
# Part 37: the live pipe's own arithmetic.
#
# The 2026-09-23 reports were「延迟大」「有杂音」「不开硬件编码几乎看不见」. All three
# are numbers or placements inside this one plugin -- how much of the stream a
# queue may hold, where a drop is allowed to cut, how long a TV is fed nothing
# before it gets the URL, and which encoder an untouched install picks -- so
# they are measured here instead of argued about.
print("\n=== Part 37: the live pipe's arithmetic ===")
import traceback as _traceback37

_tmp37 = _tempfile.mkdtemp(prefix="macast-pipe-")
mirror37 = None
_saved37 = {}
try:
    utils.SETTING_DIR = _tmp37
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp37, "macast_setting.json")
    mirror37 = _load_plugin("screen_mirror_plugin_v37", "screen_mirror.py")
    m37 = mirror37

    # -- the sender's own slice of the latency ------------------------------
    _held = lambda b: m37.live_queue_chunks(b) * m37.CHUNK * 8.0 / b
    check("a viewer's queue is budgeted in seconds of picture, not in bytes",
          all(0.45 <= _held(b) <= 1.1 for b in
              [rate for _h, rate in m37.QUALITIES.values()]),
          str([(b, _held(b)) for b in [r for _h, r in m37.QUALITIES.values()]]))
    _rates37 = sorted(r for _h, r in m37.QUALITIES.values())
    check("a bigger preset holds more bytes, so the queue is a byte budget "
          "only after it was derived from seconds",
          [m37.live_queue_chunks(b) for b in _rates37]
          == sorted(m37.live_queue_chunks(b) for b in _rates37)
          and m37.live_queue_chunks(_rates37[-1])
          > m37.live_queue_chunks(_rates37[0]),
          str([(b, m37.live_queue_chunks(b)) for b in _rates37]))
    check("and an unknown bitrate keeps the old ceiling rather than dividing by 0",
          m37.live_queue_chunks(None) == m37.LIVE_QUEUE_MAX_CHUNKS
          and m37.live_queue_chunks(0) == m37.LIVE_QUEUE_MAX_CHUNKS, '')

    # -- where a drop is allowed to cut -------------------------------------
    _align_q = m37._Broadcaster(maxsize=256, packet_align=m37.TS_PACKET)
    _subs37 = _align_q.subscribe()
    for _i37 in range(3):
        _align_q.feed(b'z' * 4096)
    _units37 = []
    while not _subs37.empty():
        _units37.append(_subs37.get_nowait())
    check("a drop on an MPEG-TS pipe never ends inside a 188-byte packet",
          all(len(u) % m37.TS_PACKET == 0 for u in _units37)
          and sum(len(u) for u in _units37)
          == 3 * 4096 - (3 * 4096) % m37.TS_PACKET,
          str([len(u) for u in _units37]))
    _before_eof37 = _align_q.bytes
    _align_q.flush()
    _tail37 = []
    while not _subs37.empty():
        _tail37.append(_subs37.get_nowait())
    check("...and the encoder's last partial packet is handed over at EOF "
          "instead of stranded in the aligner",
          sum(len(u) for u in _tail37) == (3 * 4096) % m37.TS_PACKET
          and _before_eof37 < 3 * 4096 and _align_q.bytes == 3 * 4096,
          str([len(u) for u in _tail37]))
    _align_q.unsubscribe(_subs37)

    _b37 = m37.start_stream_server(m37._Session('cast', bitrate=6000000))
    _f37 = m37.start_stream_server(m37._Session('browser', bitrate=6000000))
    try:
        check("the cast target is the one that gets packet-aligned, at its "
              "preset's queue depth",
              _b37.broadcaster._align == m37.TS_PACKET
              and _b37.broadcaster._maxsize == m37.live_queue_chunks(6000000),
              '%s / %s' % (_b37.broadcaster._align, _b37.broadcaster._maxsize))
        check("the browser target is framed by fragment instead, so alignment "
              "would fight the framer",
              _f37.broadcaster._align is None
              and _f37.broadcaster._framer is not None, '')
    finally:
        for _s37 in (_b37, _f37):
            _s37.shutdown()
            _s37.server_close()

    # -- how long a TV waits for the URL ------------------------------------
    _clamped37 = lambda rate: m37.dlna_prefill_bytes(
        types.SimpleNamespace(total_bitrate=rate))
    check("the DLNA prefill is seconds of picture, clamped at both ends",
          all(m37.DLNA_PREFILL_MIN_BYTES
              <= m37.dlna_prefill_bytes(p) <= m37.DLNA_PREFILL_MAX_BYTES
              for p in m37.DLNA_PROFILES.values())
          and _clamped37(1000000) == m37.DLNA_PREFILL_MIN_BYTES
          and _clamped37(100000000) == m37.DLNA_PREFILL_MAX_BYTES
          and m37.dlna_prefill_bytes(m37.DLNA_PROFILES['ps-pal'])
          < m37.dlna_prefill_bytes(m37.DLNA_PROFILES['mkv-h264'])
          and 1 <= m37.dlna_prefill_seconds(
              m37.DLNA_PROFILES['ts-h264']) <= 10,
          str([(k, m37.dlna_prefill_bytes(p))
               for k, p in m37.DLNA_PROFILES.items()]))
    check("it is derived from the bitrate the profile actually runs at, "
          "audio included",
          m37.dlna_prefill_bytes(m37.DLNA_PROFILES['ps-pal'])
          == (m37.DLNA_PROFILES['ps-pal'].total_bitrate
              * m37.DLNA_PREFILL_SECONDS // 8)
          and m37.DLNA_PROFILES['ps-pal'].total_bitrate == 4500000 + 192000,
          str(m37.DLNA_PROFILES['ps-pal'].total_bitrate))

    # -- which encoder an untouched install picks ---------------------------
    _SP37 = m37.SettingProperty
    m37.Setting.unset(_SP37.Mirror_Encoder)
    _hw37 = dict(m37._hw_encoder_cache)
    try:
        m37._hw_encoder_cache.clear()
        check("with nothing probed yet, auto means software: the probe spawns "
              "ffmpeg and this runs on the UI thread",
              m37.encoder_kind() == 'software', m37.encoder_kind())
        m37._hw_encoder_cache[(m37.find_ffmpeg(), 'darwin')] = True
        check("once a probe has answered, an untouched install takes "
              "VideoToolbox -- x264 cannot keep a Retina desktop watchable",
              m37.encoder_kind() == ('hardware' if sys.platform == 'darwin'
                                     else 'software'), m37.encoder_kind())
        m37._hw_encoder_cache[(m37.find_ffmpeg(), 'darwin')] = False
        m37.Setting.set(_SP37.Mirror_Encoder, 'hardware')
        check("an explicit wish is still honoured, and a machine that turns out "
              "not to have it falls back at start, not here",
              m37.encoder_kind() == ('hardware' if sys.platform == 'darwin'
                                     else 'software'), m37.encoder_kind())
        m37.Setting.set(_SP37.Mirror_Encoder, 'nvidia')
        check("a stored value this plugin does not know is software, not a crash",
              m37.encoder_kind() == 'software', m37.encoder_kind())
    finally:
        m37._hw_encoder_cache.clear()
        m37._hw_encoder_cache.update(_hw37)
        m37.Setting.unset(_SP37.Mirror_Encoder)

    # -- the audio tap and the sound that is actually routed into it --------
    if sys.platform == 'darwin':
        _keep37 = (m37._default_output, m37._device_uid)
        try:
            m37._default_output = lambda: 77
            for _uid, _want in ((m37.MACAST_AGGREGATE_UID, True),
                                ('BlackHole2ch_uid', True),
                                ('BuiltInSpeakerDevice', False),
                                ('', None)):
                m37._device_uid = lambda device_id, _u=_uid: _u
                check("the console can tell「已启用」from「有声音进来」 "
                      "(%s)" % (_uid or 'unknown'),
                      m37.system_audio_routed() is _want,
                      str(m37.system_audio_routed()))
            m37._device_uid = lambda device_id: 'BuiltInSpeakerDevice'
            _cap37 = m37._capture_cache
            _keep_cap37 = dict(_cap37)
            _cap37.clear()
            _cap37['x'] = m37._Capture('BlackHole 2ch', [['-f', 'avfoundation']],
                                        audio_map='1:a:0')
            try:
                check("and says so on the 系统声音 line, because a silent "
                      "mirror otherwise reads as an encoder bug",
                      '静音' in m37.ScreenMirrorSetting._audio_line(),
                      m37.ScreenMirrorSetting._audio_line())
            finally:
                _cap37.clear()
                _cap37.update(_keep_cap37)
        finally:
            m37._default_output, m37._device_uid = _keep37

    # -- which interface the search leaves on -------------------------------
    _keep_iface37 = (m37.Setting.resolved_network_interface,
                     m37.Setting.get_advertisable_ip)
    try:
        m37.Setting.resolved_network_interface = staticmethod(lambda: '')
        m37.Setting.get_advertisable_ip = staticmethod(
            lambda: ['192.168.99.1', '192.168.1.20'])
        check("without a pinned interface the kernel routes the search -- "
              "guessing here is how a VM bridge captures it",
              m37._search_source_ip() is None, str(m37._search_source_ip()))
        m37.Setting.resolved_network_interface = staticmethod(lambda: 'en0')
        check("with one, the search leaves from that interface's address",
              m37._search_source_ip() == '192.168.1.20',
              str(m37._search_source_ip()))
    finally:
        (m37.Setting.resolved_network_interface,
         m37.Setting.get_advertisable_ip) = _keep_iface37

    class _Sock37(object):
        def __init__(self, log):
            self.log = log

        def settimeout(self, _t):
            pass

        def bind(self, _a):
            self.log.append(('bind', _a))

        def setsockopt(self, *a):
            self.log.append(('sockopt', a[1]))

        def sendto(self, data, addr):
            self.log.append(('send', addr))

        def recvfrom(self, _n):
            raise socket.timeout

        def close(self):
            self.log.append(('close',))

    _log37 = []
    _mod37 = m37.socket

    class _FakeSocketMod(object):
        """Just the surface `_ssdp_search` touches, so the search sequence --
        not the network -- is what this measures."""

        AF_INET = socket.AF_INET
        SOCK_DGRAM = socket.SOCK_DGRAM
        timeout = socket.timeout
        IPPROTO_IP = 0
        IP_MULTICAST_IF = 12

        @staticmethod
        def inet_aton(a):
            return socket.inet_aton(a)

        def socket(self, *_a):
            return _Sock37(_log37)

    try:
        m37.socket = _FakeSocketMod()
        m37._ssdp_search('urn:schemas-upnp-org:device:MediaRenderer:1',
                         timeout=0.6)
        _sends37 = [e for e in _log37 if e[0] == 'send']
        check("one M-SEARCH is not one search: a renderer that just woke has "
              "not joined the multicast group yet",
              len(_sends37) == 2 and not any(e[0] == 'bind' for e in _log37),
              str(_log37))
        _log37[:] = []
        m37._ssdp_search('urn:schemas-upnp-org:device:MediaRenderer:1',
                         timeout=0.4, interface='192.168.1.20')
        check("and a pinned interface is set as the multicast egress, not only "
              "as a source address",
              ('sockopt', _FakeSocketMod.IP_MULTICAST_IF) in _log37
              and ('bind', ('192.168.1.20', 0)) in _log37, str(_log37))
    finally:
        m37.socket = _mod37

    # -- the low-latency channel says what it is ----------------------------
    _src37 = open(os.path.join(MACAST, "plugins", "renderer",
                               "screen_mirror.py"), encoding='utf-8').read()
    check("the menu names the low-latency channel's silence, and the hint "
          "names the fallback that has audio",
          '此通道无声音' in m37.OUTPUTS['caststream'][0]
          and '回落' in m37.OUTPUT_HINTS['caststream'],
          '%s / %s' % (m37.OUTPUTS['caststream'][0],
                       m37.OUTPUT_HINTS['caststream']))
    check("a refused handshake is reported as the fallback it is, in the "
          "message the user sees",
          "设备拒绝了低延迟通道" in _src37 and "这一路有声音" in _src37,
          'the report that the channel is silent was the complaint; the '
          'session that starts instead has audio')
    check("the DLNA and cast hints quote the sender's own numbers, not the "
          "ones this file no longer implements",
          '20 MiB' not in m37.OUTPUT_HINTS['dlna']
          and '2–4 秒' not in m37.OUTPUT_HINTS['cast'], str(m37.OUTPUT_HINTS))
finally:
    print('Part 37 setup error: %s' % _traceback37.format_exc()
          if mirror37 is None else '')
    utils.Setting.setting = {}
    _shutil.rmtree(_tmp37, ignore_errors=True)


# --------------------------------------------------------------------------
# Part 38: the capture that opens and never returns a frame.
#
# 2026-09-23, on the installed .app: 「镜像到 浏览器 启动失败：屏幕采集在 3 秒内
# 没有返回画面：objc[…]: class `NSKVONotifying_AVCaptureScreenInput' not linked
# into application；…Configuration of video device failed…」. Both of those
# lines come out of a capture that *works* -- measured on this machine: the same
# ffmpeg wrote 3.9 MB of H.264 in six seconds -- so they explained nothing.
# What did explain it: drop the system-audio input and the frames come back. A
# tap this process may not read (avfoundation treats every audio input as a
# microphone, so that is the grant it wants) starves the whole session without
# saying so, which leaves the code three jobs: keep the noise out of the
# message, name the door in the message, and bring the mirror up without the
# sound rather than not at all.
print("\n=== Part 38: a capture that never returns a frame ===")
import traceback as _traceback38

_tmp38 = _tempfile.mkdtemp(prefix="macast-noframe-")
mirror38 = None
try:
    _saved38 = (utils.Setting.setting, utils.Setting.setting_path,
                utils.SETTING_DIR)
    utils.SETTING_DIR = _tmp38
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp38, "macast_setting.json")
    mirror38 = _load_plugin("screen_mirror_plugin_v38", "screen_mirror.py")
    m38 = mirror38

    # -- what a *healthy* avfoundation capture prints on stderr --------------
    _noise38 = [
        "objc[68946]: class `NSKVONotifying_AVCaptureScreenInput' not linked "
        "into application",
        "[AVFoundation indev @ 0x79b100c140] Configuration of video device "
        "failed, falling back to default.",
        "[in#0/avfoundation @ 0x7baa804000] Stream #0: not enough frames to "
        "estimate rate; consider increasing probesize",
    ]
    _real38 = '[avfoundation @ 0x1] Input/output error'
    _noise_script38 = _write_fake(
        os.path.join(_tmp38, 'binnoise'), 'stderr-noise',
        '#!/bin/sh\ncat >&2 <<\'NOISE\'\n'
        + '\n'.join(_noise38 + [_real38]) + '\nNOISE\n')
    _proc38 = subprocess.Popen([_noise_script38], stdout=subprocess.DEVNULL,
                               stderr=subprocess.PIPE)
    _tail38 = []
    m38._drain_stderr(_proc38, _tail38)
    check("lines a working capture prints too never become the reason given "
          "for a failure",
          _tail38 == [_real38], str(_tail38))
    _proc38.wait(timeout=5)

    # -- the sentence has to end at a door -----------------------------------
    _words38 = m38.no_frame_words('whatever ffmpeg said', 'darwin')
    check("「没有返回画面」names the permission and the restart, and keeps "
          "ffmpeg's own words as an appendix",
          'whatever ffmpeg said' in _words38
          and m38.PERMISSION_DOOR in _words38 and '重启' in _words38, _words38)
    check("a platform with no such door does not invent one",
          '隐私与安全性' not in m38.no_frame_words('', 'linux'),
          m38.no_frame_words('', 'linux'))

    # -- giving up the sound, not the mirror ---------------------------------
    _av38 = m38._Capture('屏幕 + 系统声音 (BlackHole)',
                         [['-f', 'avfoundation', '-i', '1:3']], '0:a:0',
                         screens=[(1, 'Capture screen 0')])
    _drop38 = m38.video_only_capture(_av38)
    check("avfoundation carries both in one -i, so the retry asks for "
          "screen:none and the cached probe it came from stays intact",
          _drop38 is not None and _drop38.inputs ==
          [['-f', 'avfoundation', '-i', '1:none']]
          and _drop38.audio_map is None
          and _av38.inputs[0][-1] == '1:3' and _av38.audio_map == '0:a:0',
          '%s / %s' % (_drop38.inputs if _drop38 else None, _av38.inputs))
    _pulse38 = m38._Capture(
        '屏幕 (X11) + 系统声音 (PulseAudio)',
        [['-f', 'x11grab', '-i', ':0+0,0'], ['-f', 'pulse', '-i', 'o.monitor']],
        '1:a:0')
    check("a second input per device is dropped whole, and the display spec is "
          "left alone (rewriting `:0+0,0` would break the video, not the sound)",
          m38.video_only_capture(_pulse38).inputs ==
          [['-f', 'x11grab', '-i', ':0+0,0']],
          str(m38.video_only_capture(_pulse38).inputs))
    check("with no audio in the graph there is nothing to give up",
          m38.video_only_capture(m38._Capture('x', [['-i', '1:none']])) is None)

    # -- end to end: the stall, the retry, and what the user hears ------------
    # The fake answers -list_devices with a BlackHole, streams bytes only for a
    # video-only -i, and otherwise prints the runtime chatter a real stalled
    # capture prints and goes quiet -- the failure this part exists for.
    _fake38 = _write_fake(os.path.join(_tmp38, 'bin38'), 'ffmpeg', r"""#!/bin/sh
case "$*" in
  *list_devices*)
    cat <<'DEVICES'
AVFoundation input device list has 4 items:
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] [1] Capture screen 0
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone
[AVFoundation indev @ 0x1] [1] BlackHole 2ch
DEVICES
    exit 0
    ;;
esac
case "$*" in
  *":none"*)
    while true; do
      head -c 4096 /dev/zero | tr '\0' 'F'
      sleep 0.05
    done
    ;;
  *)
    cat >&2 <<'NOISE'
objc[42]: class `NSKVONotifying_AVCaptureScreenInput' not linked into application
[AVFoundation indev @ 0x1] Configuration of video device failed, falling back to default.
NOISE
    sleep 60
    ;;
esac
""")
    # `probe_capture` dispatches on the host platform and this machine may be
    # Linux; the capture shape under test is avfoundation's, so hand it over
    # directly. Everything after this point spawns the fake for real.
    _cap38 = m38._Capture(
        '屏幕 + 系统声音 (BlackHole)',
        [['-f', 'avfoundation', '-framerate', '24', '-pixel_format', 'uyvy422',
          '-capture_cursor', '1', '-i', '1:1']], '0:a:0')
    _notify38 = []

    def _notify38_rec(*args, **kwargs):
        # publish('app_notify', title, message) -- the sentence the user reads
        # is the second half, and a check on the first would pass forever.
        _notify38.append(' '.join(str(one) for one in args))

    cherrypy.engine.subscribe('app_notify', _notify38_rec)
    _saved38_probe = m38.probe_capture
    _saved38_find = m38.find_ffmpeg
    _saved38_awake = m38._keep_awake
    _saved38_budget = m38.NO_FRAME_SECONDS
    m38.probe_capture = lambda ffmpeg, *a, **k: _cap38
    m38.find_ffmpeg = lambda: _fake38
    m38._keep_awake = lambda *a, **k: None      # no caffeinate in a test
    m38.NO_FRAME_SECONDS = 0.5                  # the real budget is 3 seconds

    _rec38 = _StateRec()

    class _Mirror38(m38.ScreenMirrorRenderer):
        @property
        def protocol(self):
            return _rec38

    try:
        utils.Setting.set(m38.SettingProperty.Mirror_Output, 'browser')
        mir38 = _Mirror38()
        mir38.start_mirror()
        check("an unreadable audio tap no longer costs the mirror: the session "
              "comes up video-only",
              _wait_until(lambda: mir38.is_mirroring()
                          or ('error', True) in _rec38.rows, timeout=20),
              str(_rec38.rows))
        check("and the killed attempt is not reported as an interruption "
              "(the generation moves on before we kill it)",
              ('error', True) not in _rec38.rows, str(_rec38.rows))
        _said38 = [str(s) for s in _notify38]
        check("the success line owns up to the sound it dropped, and says "
              "which door and which restart",
              any('没有系统声音' in s and '麦克风' in s and '重启' in s
                  for s in _said38), str(_said38))
        check("which is the whole of it: no failure, no Apple runtime noise",
              not any('启动失败' in s or 'objc' in s for s in _said38),
              str(_said38))
        _cmd38 = m38.build_ffmpeg_command(_fake38, m38.video_only_capture(_cap38),
                                          720, 5000000)
        check("the encoder was asked for video alone: the audio map is gone "
              "and -an took its place",
              '-an' in _cmd38 and '0:a:0' not in _cmd38
              and '-i' in _cmd38 and _cmd38[_cmd38.index('-i') + 1] == '1:none',
              str(_cmd38))
        # A notification flashes once and is gone; the page is what the user can
        # still read ten minutes later, and its「系统声音」row otherwise answers
        # only for the machine's configuration --「已启用」over a silent session.
        check("the session keeps its own answer, separate from the probe's",
              mir38.audio_dropped() is True, str(mir38.audio_dropped()))
        m38._capture_cache[('fake38', 'darwin', '1')] = _cap38
        _row38 = m38.ScreenMirrorSetting._audio_line(mir38)
        check("so the row stops saying 已启用 over a mirror with no sound in it",
              '没有声音' in _row38 and '麦克风' in _row38 and '重启' in _row38
              and '已启用' not in _row38, _row38)

        class _Live38:
            def __init__(self, dropped, kind='browser'):
                self._dropped = dropped
                self.s = {'kind': kind, 'seconds': 65, 'mbps': 3.2, 'frames': 40,
                          'in_flight': 2, 'clients': 1, 'drops': 0,
                          'profile': 'ps-pal', 'state': 'PLAYING',
                          'buffered': 1048576}

            def audio_dropped(self):
                return self._dropped

            def stats(self):
                return self.s

        _quiet38 = m38.ScreenMirrorSetting._audio_line(_Live38(False))
        check("a session that kept its sound is not accused of losing it, "
              "and the routing question still gets asked",
              _quiet38.startswith('系统声音：已启用'), _quiet38)
        check("the status line carries the fact in the few words it has, "
              "on both of the two shapes that report throughput",
              m38.AUDIO_DROPPED_MARK in m38.ScreenMirrorSetting._status_line(
                  _Live38(True)) and m38.AUDIO_DROPPED_MARK in
              m38.ScreenMirrorSetting._status_line(_Live38(True, 'dlna')),
              '%s / %s' % (m38.ScreenMirrorSetting._status_line(_Live38(True)),
                           m38.ScreenMirrorSetting._status_line(
                               _Live38(True, 'dlna'))))
        check("and a healthy session's line is only the numbers it always was",
              m38.AUDIO_DROPPED_MARK not in m38.ScreenMirrorSetting._status_line(
                  _Live38(False)),
              m38.ScreenMirrorSetting._status_line(_Live38(False)))
        mir38.stop_mirror()

        # Nothing yields a frame at all: the last word must be a next step.
        _rec38.rows = []
        del _notify38[:]
        _dead38 = _write_fake(os.path.join(_tmp38, 'bin38dead'), 'ffmpeg-dead',
                              '#!/bin/sh\ncat >&2 <<\'NOISE\'\n'
                              + _noise38[0] + '\nNOISE\nsleep 60\n')
        m38.find_ffmpeg = lambda: _dead38
        mir38b = _Mirror38()
        mir38b.start_mirror()
        check("a capture with no audio left to blame still reports a failure",
              _wait_until(lambda: ('error', True) in _rec38.rows, timeout=20),
              str(_rec38.rows))
        _title38 = [v for k, v in _rec38.rows if k == 'CurrentTrackTitle']
        check("and that failure names the permission door and the restart, "
              "with the runtime noise left out",
              _title38 and m38.PERMISSION_DOOR in _title38[-1]
              and '重启' in _title38[-1] and 'objc' not in _title38[-1],
              str(_title38))
        mir38b.stop_mirror()
    finally:
        m38.probe_capture = _saved38_probe
        m38.find_ffmpeg = _saved38_find
        m38._keep_awake = _saved38_awake
        m38.NO_FRAME_SECONDS = _saved38_budget
        m38._capture_cache.clear()
        try:
            cherrypy.engine.unsubscribe('app_notify', _notify38_rec)
        except Exception:
            pass
finally:
    if mirror38 is None:
        print('Part 38 setup error: %s' % _traceback38.format_exc())
    utils.Setting.setting = {}
    _shutil.rmtree(_tmp38, ignore_errors=True)


# --------------------------------------------------------------------------
# Part 39: Windows system audio, DLNA discovery that leaves the machine, and
# the numbers behind「延迟有点大」
#
# Three reports, one theme: each of them is a fact that was invisible from the
# screen. Windows mirrored picture only and said nothing about why; a DLNA
# search that found nothing looked identical whether the television was off or
# the M-SEARCH never left the right adapter; and "the delay is noticeable" had
# no number attached to it anywhere in the app. So this part measures the
# device table ffmpeg really prints, the interfaces a search really tries, and
# the budgets a session really holds -- and it proves the Nagle fix on a real
# TCP socket rather than by reading the source.
# --------------------------------------------------------------------------
print("\n=== Part 39: windows audio, discovery reach, latency budgets ===")
import traceback as _traceback39

_tmp39 = _tempfile.mkdtemp(prefix="macast-win39-")
mirror39 = None
try:
    utils.SETTING_DIR = _tmp39
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp39, "macast_setting.json")
    mirror39 = _load_plugin("screen_mirror_plugin_v39", "screen_mirror.py")
    m39 = mirror39
    _bin39 = os.path.join(_tmp39, "bin")

    # -- what `ffmpeg -f dshow -list_devices true -i dummy` really prints ----
    # Not invented: this is the verbatim table ffmpeg 8.1.2 printed on a real
    # Windows 11 box (AMD-YES), which is the machine that reported "no sound".
    # The avfoundation parser in this plugin was once written against made-up
    # output and returned two empty lists on every real Mac (AGENTS.md 4.2), so
    # the input here is captured rather than imagined. Three details only real
    # output has: the prefix is `[in#0 @ ...]` not `[dshow @ ...]`, a `(none)`
    # marker exists for a device whose pin category cannot be resolved, and the
    # `Alternative name` lines carry a quoted string with no marker at all.
    _dshow_real = (
        '[in#0 @ 00000000007c1300] "Astra Pro HD Camera" (video)\n'
        '[in#0 @ 00000000007c1300]   Alternative name "@device_pnp_\\\\?\\usb#vi'
        'd_2bc5&pid_0501&mi_00#a&2387c1ff&0&0000#{65e8773d-8f56-11d0-a3b9-00a0'
        'c9223196}\\global"\n'
        '[in#0 @ 00000000007c1300] "Smart Connect Camera" (video)\n'
        '[in#0 @ 00000000007c1300]   Alternative name "@device_pnp_\\\\?\\root#'
        'camera#0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\\global"\n'
        '[in#0 @ 00000000007c1300] "OBS Virtual Camera" (none)\n'
        '[in#0 @ 00000000007c1300]   Alternative name "@device_sw_{860BB310-5D0'
        '1-11D0-BD3B-00A0C911CE86}\\{A3FCE0F5-3493-419F-958A-ABA1250EC20B}"\n'
        '[in#0 @ 00000000007c1300] "耳机式麦克风 (HUAWEI Sound Joy-06342)" (audio)\n'
        '[in#0 @ 00000000007c1300]   Alternative name "@device_cm_{33D9A762-90C'
        '8-11D0-BD43-00A0C911CE86}\\wave_{1EE13EA6-CD75-4818-AFFD-310B0FA9A5AC}"\n'
        '[in#0 @ 00000000007c1300] "Virtual Mic (Virtual Mic for AudioRelay)" '
        '(audio)\n'
        '[in#0 @ 00000000007c1300] "麦克风 (Steam Streaming Microphone)" (audio)\n'
        '[in#0 @ 00000000007c1300] "麦克风 (ORBBEC Audio Device)" (audio)\n'
        '[in#0 @ 00000000007c1300] "麦克风 (Realtek(R) Audio)" (audio)\n'
        '[in#0 @ 00000000007c1300] "立体声混音 (Realtek(R) Audio)" (audio)\n'
        'Error opening input file dummy.\n')
    _v39, _a39 = m39._parse_dshow_devices(_dshow_real)
    check("the real dshow device table yields both lists",
          _v39 == ['Astra Pro HD Camera', 'Smart Connect Camera']
          and _a39[-1] == '立体声混音 (Realtek(R) Audio)'
          and '耳机式麦克风 (HUAWEI Sound Joy-06342)' in _a39
          and len(_a39) == 6,
          '%s / %s' % (_v39, _a39))
    check("an 'Alternative name' line is not mistaken for a device",
          not any('device_pnp' in n or 'device_cm' in n or 'device_sw' in n
                  for n in _v39 + _a39), str(_v39 + _a39))
    check("this machine's table really does offer a loopback tap to pick",
          m39.windows_loopback_device(_a39) == '立体声混音 (Realtek(R) Audio)',
          'the machine that reported no sound has Stereo Mix; it was the -an '
          'that silenced it, not the hardware: %s' % _a39)
    # The `(none)` marker: OBS Virtual Camera printed it. It is neither list --
    # deliberately, because guessing "video" is one bad guess away from being
    # handed to `audio=<name>` and taking the capture down with it.
    check("a device whose type ffmpeg cannot resolve is neither list",
          'OBS Virtual Camera' not in _v39
          and 'OBS Virtual Camera' not in _a39
          and m39.windows_loopback_device(['OBS Virtual Camera']) is None,
          '%s / %s' % (_v39, _a39))

    _quoted39 = m39._parse_dshow_devices(
        '[dshow @ 0] "Odd \\"quoted\\" device" (audio)\n'
        '[dshow @ 0] "Fine" (audio)\n')[1]
    check("a name that could break out of audio=<name> never reaches the argv",
          'Fine' in _quoted39
          and not any('"' in name for name in _quoted39),
          str(_quoted39))

    check("only a device that carries the output counts as a loopback tap",
          m39.windows_loopback_device(['麦克风阵列', 'Stereo Mix (Realtek(R))'])
          == 'Stereo Mix (Realtek(R))'
          and m39.windows_loopback_device(['立体声混音 (Realtek)'])
          == '立体声混音 (Realtek)'
          and m39.windows_loopback_device(['CABLE Output (VB-Audio)'])
          == 'CABLE Output (VB-Audio)'
          and m39.windows_loopback_device(['麦克风阵列', 'HD WebCam']) is None)

    _win_audio39 = _write_fake(_bin39, "ffmpeg-win-audio", r"""#!/bin/sh
case "$*" in
  *dshow*)
    printf '%s\n' \
      '[dshow @ 0x1] "HD WebCam" (video)' \
      '[dshow @ 0x1]   Alternative name "@device_pnp_usb#vid_1bcf"' \
      '[dshow @ 0x1] "麦克风阵列 (Realtek(R) Audio)" (audio)' \
      '[dshow @ 0x1] "Stereo Mix (Realtek(R) Audio)" (audio)' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.2
done
""")
    _win_mic39 = _write_fake(_bin39, "ffmpeg-win-mic", r"""#!/bin/sh
case "$*" in
  *dshow*)
    printf '%s\n' \
      '[dshow @ 0x1] "HD WebCam" (video)' \
      '[dshow @ 0x1] "麦克风阵列 (Realtek(R) Audio)" (audio)' \
      '[in#0 @ 0x1] Error opening input: Input/output error'
    exit 0
    ;;
esac
while true; do
  head -c 8192 /dev/zero | tr '\0' 'T'
  sleep 0.2
done
""")

    m39._capture_cache.clear()
    _cap39 = m39.probe_capture(_win_audio39, 'win32')
    _cmd39 = m39.build_ffmpeg_command(_win_audio39, _cap39, 720, 5000000)
    check("a Windows loopback device turns system audio on",
          _cap39.audio_map == '1:a:0'
          and 'Stereo Mix (Realtek(R) Audio)' in _cap39.label
          and '-c:a' in _cmd39 and 'aac' in _cmd39 and '-an' not in _cmd39,
          '%s / %s' % (_cap39.label, _cmd39))
    check("the sound is a second input, and the picture stays gdigrab",
          len(_cap39.inputs) == 2
          and _cap39.inputs[0][:2] == ['-f', 'gdigrab']
          and _cap39.inputs[1][:3] == ['-f', 'dshow', '-i']
          and _cap39.inputs[1][3].startswith('audio='),
          str(_cap39.inputs))
    check("and the audio map points at that second input, not input 0",
          _cap39.audio_map.startswith('1:'), _cap39.audio_map)

    _vo39 = m39.video_only_capture(_cap39)
    check("giving up the sound on Windows drops the dshow input whole",
          len(_vo39.inputs) == 1 and _vo39.inputs[0][:2] == ['-f', 'gdigrab']
          and _vo39.audio_map is None
          and '-an' in m39.build_ffmpeg_command(_win_audio39, _vo39, 720, 5000000),
          str(_vo39.inputs))

    m39._capture_cache.clear()
    _capmic39 = m39.probe_capture(_win_mic39, 'win32')
    check("no loopback device on Windows stays video only, as before",
          _capmic39.audio_map is None
          and '-an' in m39.build_ffmpeg_command(_win_mic39, _capmic39, 720, 5000000),
          str(_capmic39.inputs))

    # The sentence the user actually reads has to name the device to switch on
    # and the button to press -- "仅画面" was the whole of the original answer.
    _saved_platform39 = m39.sys.platform
    try:
        m39.sys.platform = 'win32'
        m39._capture_cache.clear()
        m39.probe_capture(_win_mic39, 'win32')
        _line39 = m39.ScreenMirrorSetting._audio_line()
    finally:
        m39.sys.platform = _saved_platform39
        m39._capture_cache.clear()
    check("the Windows audio line names the device to enable and the retry",
          '立体声混音' in _line39 and 'Stereo' in _line39
          and '重新探测采集' in _line39,
          _line39)

    # -- a DLNA search that does not leave from the right adapter -----------
    # The routed attempt first (what a single-homed machine needs and what the
    # rest of this suite has always exercised), then one per local address.
    _saved_ips39 = m39.Setting.get_advertisable_ip
    _saved_resolved39 = m39.Setting.resolved_network_interface
    m39.Setting.resolved_network_interface = staticmethod(lambda: '')
    m39.Setting.get_advertisable_ip = staticmethod(
        lambda: ['10.0.0.5', '192.168.99.1', '192.168.1.5'])
    _saved_ask39 = m39._ask_renderers
    _calls39 = []

    def _ask39(targets, timeout=None, interface=None):
        _calls39.append(interface)
        if interface == '192.168.1.5':
            return [('http://192.168.1.40:58880/d.xml', '192.168.1.40')]
        return []

    m39._ask_renderers = _ask39
    try:
        _ans39 = m39._ask_renderers_everywhere(['urn:x'], 0.1)
        _trace39 = m39.dlna_trace()
        # Sorted, so the sweep is deterministic run to run; which address wins
        # only decides how long an *empty* search takes, and the answering
        # attempt ends it either way.
        check("an empty search is retried from every local address",
              _calls39 == [None, '10.0.0.5', '192.168.1.5'],
              'the routed attempt first, then each advertisable address: %s'
              % _calls39)
        check("the sweep stops at the interface that answered",
              _ans39 == [('http://192.168.1.40:58880/d.xml', '192.168.1.40')]
              and len(_trace39) == 3
              and [r['answers'] for r in _trace39] == [0, 0, 1],
              '%s / %s' % (_ans39, _trace39))
        check("every attempt is recorded for the diagnostics card",
              _trace39[0]['interface'] == '自动（内核路由）'
              and _trace39[1]['interface'] == '10.0.0.5'
              and _trace39[2]['interface'] == '192.168.1.5',
              str(_trace39))

        _calls39[:] = []
        m39._ask_renderers = lambda t, timeout=None, interface=None: [
            ('http://10.0.0.9:1/d.xml', '10.0.0.9')]
        _one39 = m39._ask_renderers_everywhere(['urn:x'], 0.1)
        check("a search that already answered is not retried anywhere",
              _calls39 == [] and len(_one39) == 1
              and len(m39.dlna_trace()) == 1,
              'a busy LAN pays nothing for the fallback: %s' % _calls39)

        def _dead39(targets, timeout=None, interface=None):
            raise m39.DiscoveryError('packet could not be sent')
        m39._ask_renderers = _dead39
        _raised39 = False
        try:
            m39._ask_renderers_everywhere(['urn:x'], 0.1)
        except m39.DiscoveryError:
            _raised39 = True
        check("a search that never sent anything is still a discovery failure",
              _raised39 and all(r['error'] for r in m39.dlna_trace()),
              '「没有发现」and「发不出去」must not become the same sentence')
    finally:
        m39._ask_renderers = _saved_ask39
        m39.Setting.get_advertisable_ip = _saved_ips39
        m39.Setting.resolved_network_interface = _saved_resolved39
        m39._dlna_trace = []

    _trace39_words = [{'interface': '自动（内核路由）', 'answers': 0, 'error': ''},
                      {'interface': '192.168.1.5', 'answers': 0, 'error': ''}]
    check("an empty search says which interfaces it tried",
          '2 个网卡' in m39._search_words([], False, time.time(), '', '没有发现',
                                           trace=_trace39_words),
          m39._search_words([], False, time.time(), '', '没有发现',
                            trace=_trace39_words))
    check("...and a single attempt adds no such clause",
          '网卡' not in m39._search_words([], False, time.time(), '', '没有发现',
                                          trace=_trace39_words[:1]),
          'a single-homed machine must not be lectured about adapters')

    # -- the sender's own slice of the latency -------------------------------
    _pf39 = m39.DLNA_PROFILES['ps-pal']
    check("the DLNA prefill no longer sits behind its own floor",
          m39.dlna_prefill_bytes(_pf39) != m39.DLNA_PREFILL_MIN_BYTES
          and 3 <= m39.dlna_prefill_seconds(_pf39) <= 4,
          '%s bytes / %s s (the floor used to dominate: 5.4 s)'
          % (m39.dlna_prefill_bytes(_pf39), m39.dlna_prefill_seconds(_pf39)))
    check("the browser replay tail is a few GOPs, not ten seconds of picture",
          m39.REPLAY_BYTES <= (2 << 20)
          and m39.REPLAY_BYTES >= (m39.QUALITIES['source'][1] // 8) // 4,
          '%s bytes' % m39.REPLAY_BYTES)

    # A late joiner must still land on a keyframe, which means the ring keeps
    # at least one *whole* fragment however small the budget is.
    _big39 = m39._Broadcaster(maxsize=256, ring_bytes=1024,
                              init_marker=b'moof')
    _frag39 = b'moof' + b'\x00' * 8192 + b'mdat' + b'\x00' * 8192
    _big39.feed(_frag39)
    check("a replay budget smaller than one fragment still keeps that fragment",
          len(_big39.tail()) == 1 and len(_big39.tail()[0]) >= len(_frag39) - 1,
          'a replay that starts mid-fragment is a green smear, not a picture')

    # -- Nagle, on a real socket --------------------------------------------
    _srv39 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    _srv39.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    _srv39.bind(('127.0.0.1', 0))
    _srv39.listen(1)
    _client39 = socket.create_connection(_srv39.getsockname(), timeout=5)
    _conn39, _addr39 = _srv39.accept()

    class _Handler39(object):
        connection = _conn39

    try:
        _before39 = _conn39.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY)
        m39._StreamHandler._no_delay(_Handler39())
        _after39 = _conn39.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY)
        _conn39.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 0)
        _off39 = _conn39.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY)
    finally:
        for _s39 in (_conn39, _client39, _srv39):
            try:
                _s39.close()
            except OSError:
                pass
    # Darwin's getsockopt answers with the internal flag (TF_NODELAY = 4), not
    # the value that was stored, so the honest assertion is "was off, is on,
    # and 0 still turns it back off" -- measured on this machine rather than
    # assumed to be 1 the way Linux reports it.
    check("the live stream turns Nagle off on its own connection",
          _before39 == 0 and _after39 != 0 and _off39 == 0,
          'before=%s after=%s off-again=%s: Nagle batches in time, and time is '
          'what is being streamed' % (_before39, _after39, _off39))

    with open(os.path.join(MACAST, "plugins", "renderer", "screen_mirror.py"),
              encoding="utf-8") as _fh39:
        _src39 = _fh39.read()
    _serve39 = _src39.split('def _serve_stream')[1].split('def _serve_infinite')[0]
    _file39 = _src39.split('def _serve_infinite_file')[1].split('def _dlna_headers')[0]
    check("both streaming responses do it, not just the live-edge one",
          '_no_delay()' in _serve39 and '_no_delay()' in _file39,
          'a DLNA renderer reads in big chunks but reconnects constantly')

    # -- the statistics card ------------------------------------------------
    _mc39 = _load("mirror_view39", "mirror_view.py")
    _idle39 = {'mirroring': False, 'platform': 'darwin', 'available': True,
               'console_version': _mc39.VIEW_VERSION, 'capture': {'probed': True},
               'audio': {'line': ''}, 'output': {'kind': 'cast'}, 'stats': {}}
    check("no session means no statistics card, not an empty one",
          _mc39.diagnostics_rows(_idle39) == []
          and _mc39.diagnostics_text(_idle39) == ''
          and 'diagnostics' not in _mc39.sections_for(_idle39))

    _diag39 = m39._session_diagnostics(
        kind='browser', capture=_cap39, command=['ffmpeg', '-f', 'x', 'pipe:1'],
        encoder='software', height=720, bitrate=4000000,
        session=type('S39', (), {'profile': None})())
    _live39 = {'mirroring': True, 'version': '0.13', 'platform': 'darwin',
               'available': True, 'console_version': _mc39.VIEW_VERSION,
               'capture': {'probed': True}, 'audio': {'line': ''},
               'output': {'kind': 'browser'},
               'channels': [{'key': 'browser', 'label': '浏览器',
                             'words': '不需要设备'}],
               'search_trace': {'dlna': _trace39_words},
               'stats': dict({'diag': _diag39}, mbps=3.9, clients=1,
                             bytes=2 << 20, chunks=512, drops=0, seconds=42)}
    _rows39 = _mc39.diagnostics_rows(_live39)
    _flat39 = dict(_rows39)
    check("the statistics card carries every term the delay is made of",
          any('延迟来源' in k for k in _flat39)
          and '关键帧间隔' in _flat39 and '编码器' in _flat39
          and '实测码率' in _flat39 and '丢块' in _flat39,
          str(sorted(_flat39)))
    check("it says what the sender holds in front of a viewer, in seconds",
          any('发送队列' in k and '秒' in v for k, v in _rows39),
          str([r for r in _rows39 if '队列' in r[0]]))
    check("and it shows the actual ffmpeg argv",
          'ffmpeg' in _flat39.get('ffmpeg 命令', ''),
          _flat39.get('ffmpeg 命令', ''))
    check("the card says how many interfaces the DLNA search tried",
          any(k.startswith('发现尝试') for k in _flat39), str(sorted(_flat39)))
    check("the card appears exactly when there is a session to debug",
          'diagnostics' in _mc39.sections_for(_live39)
          and _mc39.sections_for(_live39)[-1] == 'activity',
          str(_mc39.sections_for(_live39)))
    _text39 = _mc39.diagnostics_text(_live39)
    check("the copied block repeats the card row for row",
          all('{}: {}'.format(k, v) in _text39 for k, v in _rows39)
          and '设备发现: 不需要设备' in _text39,
          _text39[:200])
    check("nothing secret is in the copied block",
          'token' not in _text39.lower() and all(
              'token' not in str(k).lower() and 'token' not in str(v).lower()
              for k, v in _rows39),
          'the mirror stream id and the page token are credentials (AGENTS 4.7)')

    # The DLNA shape reports its own budget instead of a queue it does not have.
    _dlna_diag39 = m39._session_diagnostics(
        kind='dlna', capture=_cap39, command=['ffmpeg', 'pipe:1'],
        encoder='software', height=720, bitrate=4000000,
        session=type('S39b', (), {'profile': _pf39})())
    _dlna_rows39 = dict(_mc39.diagnostics_rows(dict(
        _live39, output={'kind': 'dlna'},
        stats=dict({'diag': _dlna_diag39}, mbps=4.5, clients=1, bytes=1 << 20,
                   chunks=256, drops=0, seconds=12, state='PLAYING',
                   buffered=4 << 20))))
    check("the DLNA shape shows its prefill, not a live queue",
          '预填缓冲（延迟来源）' in _dlna_rows39
          and '发送队列（延迟来源）' not in _dlna_rows39
          and _dlna_rows39.get('DLNA 档位') == 'ps-pal',
          str(sorted(_dlna_rows39)))
    check("and the console exposes the discovery trace it is drawn from",
          'search_trace' in _src39
          and "'search_trace': {'dlna': dlna_trace()}" in _src39,
          'the card reads it, so it has to be in the state the page fetches')

    check("the plugin header version matches the constant it advertises",
          '<macast.version>{}'.format(m39.PLUGIN_VERSION) in _src39,
          'PLUGIN_VERSION=%s' % m39.PLUGIN_VERSION)
finally:
    if mirror39 is None:
        print('Part 39 setup error: %s' % _traceback39.format_exc())
    utils.Setting.setting = {}
    _shutil.rmtree(_tmp39, ignore_errors=True)


# --------------------------------------------------------------------------
# Part 40: three things only the *packaged* Windows build could teach us
#
# v0.8.4 shipped to a real Windows 11 box (AMD-YES) and was driven there -- the
# running app, not the source -- which turned up a number, a sentence and a
# build flag that were each right on exactly one platform:
#
#   1. our own 3-second first-frame budget killed a capture whose sound device
#      had already opened (`Guessed Channel Layout: stereo`) and whose picture
#      was 1.2 s behind; the video-only retry then came up, so a machine with a
#      working Stereo Mix reported「没有系统声音」,
#   2. the sentence explaining that sent a Windows user to macOS's microphone
#      pane -- a door Windows does not have,
#   3. the packaged build logged `Failed to start HTTPS channel: No module
#      named 'cheroot.ssl'` at startup: AGENTS 4.3's class of bug, sitting on
#      the very packaging path that had just been repaired for the plugins.
#
# So this part anchors the budget to the real stderr that justified it, keeps
# the two wordings apart, and makes the three PyInstaller jobs prove they carry
# the module that failed.
# --------------------------------------------------------------------------
print("\n=== Part 40: windows first-frame budget, wording, ssl module ===")
import traceback as _traceback40

_tmp40 = _tempfile.mkdtemp(prefix="macast-win40-")
mirror40 = None
try:
    utils.SETTING_DIR = _tmp40
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp40, "macast_setting.json")
    mirror40 = _load_plugin("screen_mirror_plugin_v40", "screen_mirror.py")
    m40 = mirror40
    _root40 = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    _src40 = open(os.path.join(_root40, 'macast', 'plugins', 'renderer',
                               'screen_mirror.py'), encoding='utf-8').read()

    # -- the budget --------------------------------------------------------
    check("the first-frame budget is platform-aware, not one number",
          m40.no_frame_budget('darwin') == m40.NO_FRAME_SECONDS
          and m40.no_frame_budget('linux') == m40.NO_FRAME_SECONDS
          and m40.no_frame_budget('win32') == m40.NO_FRAME_SECONDS_WIN32
          and m40.NO_FRAME_SECONDS_WIN32 > m40.NO_FRAME_SECONDS * 2,
          'darwin/linux=%g win32=%g' % (m40.NO_FRAME_SECONDS,
                                        m40.NO_FRAME_SECONDS_WIN32))

    # Verbatim from the box, in the same stderr chunk the old budget killed:
    # a gdigrab still starting up *and* a sound device that had opened.
    check("the Windows budget cites the real output that justified it",
          'not enough frames to estimate rate' in _src40
          and 'Guessed Channel Layout: stereo' in _src40,
          'a number without the evidence next to it is just a bigger guess')

    check("the sentence a timed-out Windows capture gets quotes that budget",
          '8 秒' in m40.no_frame_words('x', platform='win32')
          and '3 秒' in m40.no_frame_words('x', platform='darwin'),
          m40.no_frame_words('', platform='win32')[:120])

    # -- the two wordings --------------------------------------------------
    _win40 = m40.audio_dropped_suffix('win32') + m40.audio_dropped_mark('win32')
    check("a Windows session that drops sound names the Windows cause",
          '立体声混音' in _win40 and '重新探测采集' in _win40
          and '独占' in _win40,
          _win40)
    check("and never sends the user to a door Windows does not have",
          '麦克风' not in _win40 and '隐私与安全性' not in _win40,
          'the macOS door is what the first Windows run actually printed')
    check("the macOS wording is untouched by the Windows branch",
          m40.audio_dropped_suffix('darwin') == m40.AUDIO_DROPPED_SUFFIX
          and m40.audio_dropped_mark('darwin') == m40.AUDIO_DROPPED_MARK
          and '麦克风' in m40.AUDIO_DROPPED_MARK,
          m40.audio_dropped_suffix('darwin'))

    # The「系统声音」row and the status line have to agree: green「已启用」above
    # a mark that says otherwise is the lie Part 38 exists to prevent.
    class _Dropped40(object):
        @staticmethod
        def audio_dropped():
            return True
    _cap40 = type('C40', (), {
        'audio_map': ['0:a'],
        'label': '屏幕 (GDI) + 系统声音 (立体声混音 (Realtek(R) Audio))'})()
    m40._capture_cache['part40'] = _cap40
    _holder40 = next(obj for obj in vars(m40).values()
                     if isinstance(obj, type) and hasattr(obj, '_audio_line'))
    _win_line40 = _holder40._audio_line(_Dropped40(), platform='win32')
    _mac_line40 = _holder40._audio_line(_Dropped40(), platform='darwin')
    check("the「系统声音」row speaks the language of the platform it ran on",
          '立体声混音' in _win_line40 and '麦克风' not in _win_line40
          and '麦克风' in _mac_line40,
          _win_line40[:120])
    check("and it does not say「已启用」in the same breath",
          not _win_line40.startswith('系统声音：已启用'),
          _win_line40[:60])

    # -- the build flag ----------------------------------------------------
    _yml40 = open(os.path.join(_root40, '.github', 'workflows', 'build.yml'),
                  encoding='utf-8').read()
    _py2app40 = open(os.path.join(_root40, 'scripts', 'setup_py2app.py'),
                     encoding='utf-8').read()
    _count40 = _yml40.count('--hidden-import=cheroot.ssl.builtin')
    check("every PyInstaller job ships the ssl module cherrypy names at runtime",
          _count40 == 3,
          'found %d of 3 (macos-arm64, linux-x86_64, linux-arm64, windows)'
          % _count40)
    check("py2app keeps its include, so the two packaging paths cannot drift",
          'cheroot.ssl.builtin' in _py2app40
          and 'cheroot.ssl.builtin' in _yml40,
          'the packaged Windows build logged this exact module as missing')
    check("the Windows job is one of the three that carries it",
          _yml40.count('--hidden-import=cheroot.ssl.builtin') >= 1
          and _yml40.index('--hidden-import=cheroot.ssl.builtin')
              < _yml40.index('windows-x86_64'),
          'the module has to be in the list *before* the job that builds it')
finally:
    if mirror40 is None:
        print('Part 40 setup error: %s' % _traceback40.format_exc())
    utils.Setting.setting = {}
    _shutil.rmtree(_tmp40, ignore_errors=True)


# --------------------------------------------------------------------------
# Part 41: the Windows .exe must not open a console window
#
# The packaged Windows build was a console program, so launching it put a black
# window on the desktop with nobody's name on it. Two separate things fix that,
# and neither works alone:
#
#   1. `--noconsole`, so Macast itself has no console -- but a windowed process
#      that starts a *console* helper gives that helper a console window of its
#      own, and every helper we run is a console program (ffmpeg, ffprobe,
#      yt-dlp, taskkill, the user's hooks). Ship only this and the single black
#      window becomes one black box per probe, per mirror and per download.
#   2. a no-console-window flag on every child, installed once in
#      `macast/utils.py` instead of at ~30 call sites spread over a dozen
#      single-file plugins written by different hands.
#
# A windowed process also has no `stdout`, so the entry point has to hand
# `print()` somewhere to go before it imports anything that prints.
# --------------------------------------------------------------------------
print("\n=== Part 41: no console window on Windows ===")
import subprocess as _sp41

try:
    check("the no-window bit exists only where it means something",
          utils.NO_WINDOW == (getattr(_sp41, 'CREATE_NO_WINDOW', 0)
                              if sys.platform == 'win32' else 0),
          'NO_WINDOW=%r on %s' % (utils.NO_WINDOW, sys.platform))

    check("hidden_flags sets the bit on Windows and nothing elsewhere",
          utils.hidden_flags(0, platform='win32', no_window=0x08000000)
          == 0x08000000
          and utils.hidden_flags(0, platform='darwin', no_window=0) == 0
          and utils.hidden_flags(0, platform='linux', no_window=0) == 0,
          'win32 / darwin / linux')

    check("and it merges a caller's own flags instead of replacing them",
          utils.hidden_flags(0x00000200, platform='win32',
                             no_window=0x08000000) == 0x08000200,
          'potplayer.py passes CREATE_NO_WINDOW of its own; others may pass '
          'CREATE_NEW_PROCESS_GROUP')

    # All four entry points end up in the object patched here: `run` and `call`
    # build a `Popen` themselves, `check_output` goes through `run`.
    check("run and call reach the global we patch, check_output via run",
          'Popen' in _sp41.run.__code__.co_names
          and 'Popen' in _sp41.call.__code__.co_names
          and 'run' in _sp41.check_output.__code__.co_names,
          'measured, not assumed: check_output names run, not Popen')

    # `creationflags` is a Windows keyword: POSIX raises ValueError for any
    # nonzero value. Two things follow, and both are asserted here -- the bit
    # must be 0 off Windows, and the installer must therefore not wrap anything
    # on a platform whose subprocess would reject the flag it injects.
    _legal41 = None
    try:
        _sp41.run(['/bin/echo', 'probe'], stdout=_sp41.PIPE, timeout=20,
                  creationflags=0x08000000)
        _legal41 = True
    except ValueError:
        _legal41 = False
    check("the no-window bit is legal exactly where NO_WINDOW sets it",
          _legal41 == (sys.platform == 'win32'),
          'POSIX rejects any nonzero creationflags outright')

    class _Recorder41(object):
        """Stands in for Popen: records what it was called with, spawns nothing."""

        def __init__(self, *args, **kwargs):
            self.args = args
            self.kwargs = kwargs

    utils.install_hidden_popen(no_window=0x08000000, popen=_Recorder41)
    _child41 = _Recorder41(['ffmpeg'])
    check("installing the wrapper reaches a child spawned afterwards",
          getattr(_Recorder41, '_macast_hidden_console', False) is True
          and _child41.kwargs.get('creationflags') == 0x08000000,
          str(_child41.kwargs))
    _child41b = _Recorder41(['ffmpeg'], creationflags=0x00000200)
    check("a child spawned with its own flags keeps both",
          _child41b.kwargs.get('creationflags') == 0x08000200,
          str(_child41b.kwargs))
    _after41 = _Recorder41.__init__
    utils.install_hidden_popen(no_window=0x08000000, popen=_Recorder41)
    check("installing twice does not nest a second wrapper",
          _Recorder41.__init__ is _after41,
          'the marker is what makes a re-install a no-op')

    # The real installer, on this platform, with the real class -- and a real
    # child through it: the inert path has to be genuinely inert.
    utils.install_hidden_popen()
    if sys.platform != 'win32':
        check("off Windows the installer does not touch subprocess at all",
              getattr(_sp41.Popen, '_macast_hidden_console', False) is False,
              'injecting the flag here would raise ValueError in every spawn')
    _ran41 = _sp41.run(['/bin/echo', 'no-console'], stdout=_sp41.PIPE,
                       text=True, timeout=20)
    check("a real child still runs after the installer ran",
          _ran41.stdout.strip() == 'no-console',
          'proves the wrapper cannot break subprocess outright')

    # -- the entry point: a windowed process has no stdout -----------------
    _macastpy41 = open(os.path.join(_root40, 'Macast.py'), encoding='utf-8').read()
    _guard41 = _macastpy41.find('sys.stdout is None')
    _import41 = _macastpy41.find('from macast import')
    check("the entry point gives print() somewhere to go before it imports",
          _guard41 != -1 and _import41 != -1 and _guard41 < _import41,
          'gui.py, protocol.py and nirvana.py all print; a windowed build has '
          'sys.stdout = None and the first one would raise AttributeError')
    check("and both streams are covered, not just stdout",
          'sys.stderr is None' in _macastpy41
          and 'os.devnull' in _macastpy41,
          'logging still goes to the rotating file in SETTING_DIR')
    check("the entry point imports the module that installs the wrapper",
          'from macast.utils import' in _macastpy41,
          'or the first ffmpeg spawn would flash a box before it is installed')

    # -- the build flag ----------------------------------------------------
    _winjob41 = _yml40[_yml40.index('build-windows-x86_64'):]
    _winjob41 = _winjob41[:_winjob41.find('\n  # --') if '\n  # --' in _winjob41
                          else len(_winjob41)]
    check("the Windows job builds a windowed executable",
          '--noconsole' in _winjob41 or '--windowed' in _winjob41,
          'without this the console window is the app itself, not a child')
    check("and the macOS/Linux jobs are left as they were",
          _yml40[:_yml40.index('build-windows-x86_64')].count('--noconsole') == 0,
          'a console is only a defect where one pops up on its own')
finally:
    utils.Setting.setting = {}


# --------------------------------------------------------------------------
# Part 42: a DLNA renderer that downloads and still refuses
#
# Shipped as v0.8.6, the mirror to a real TCL 85T8G measured this: the
# television opened eight connections to our stream port, read 26 MB across
# them (~5 Mbps, so it was genuinely pulling), closed them again, and its player
# stayed in TRANSITIONING for eight watchdog cycles -- it never reached PLAYING.
# Nothing in the log said why, because the exchange is recorded at DEBUG (which
# the module log does not keep) and a renderer that fails looks exactly like one
# that played. So this part pins the three things that failure turned on:
#
#   1. the response must name the same DLNA.ORG_PN the DIDL promised -- ours
#      sent only the flags, so a renderer checking the *response* for its
#      profile name found none,
#   2. an exchange has to be readable in the log a user actually keeps, bounded
#      so a retrying renderer cannot flood it,
#   3. our half of a connection the renderer closed has to be noticed, or the
#      sockets pile up in CLOSE_WAIT (seven of them, measured).
# --------------------------------------------------------------------------
print("\n=== Part 42: DLNA exchange evidence ===")
import traceback as _traceback42

_tmp42 = _tempfile.mkdtemp(prefix="macast-dlna42-")
mirror42 = None
try:
    utils.SETTING_DIR = _tmp42
    utils.Setting.setting = {}
    utils.Setting.setting_path = os.path.join(_tmp42, "macast_setting.json")
    mirror42 = _load_plugin("screen_mirror_plugin_v42", "screen_mirror.py")
    m42 = mirror42

    # -- 1. the two places that name the profile must agree ----------------
    _ps42 = m42.dlna_profile('ps-pal')
    _features42 = m42.content_features(_ps42)
    check("the answer names the profile the DIDL promised",
          'DLNA.ORG_PN={}'.format(_ps42.org_pn) in _features42
          and 'DLNA.ORG_PN={}'.format(_ps42.org_pn)
          in m42.protocol_info(_ps42),
          _features42)
    check("and a profile with no PN does not emit a dangling one",
          all('DLNA.ORG_PN=' not in m42.content_features(prof)
              for prof in m42.DLNA_PROFILES.values() if not prof.org_pn),
          'the MKV shape has no standard PN; inventing one is worse')

    # -- 2. a real request through the real handler, watched in the log ----
    import http.client as _http42
    import logging as _logging42
    import threading as _threading42
    from http.server import ThreadingHTTPServer as _Server42

    class _Grab42(_logging42.Handler):
        def __init__(self):
            _logging42.Handler.__init__(self)
            self.lines = []

        def emit(self, record):
            self.lines.append(record.getMessage())

    _grab42 = _Grab42()
    _plugin_logger42 = _logging42.getLogger(m42.logger.name)
    _plugin_logger42.addHandler(_grab42)
    _old_level42 = _plugin_logger42.level
    _plugin_logger42.setLevel(_logging42.DEBUG)

    _session42 = m42._Session('browser')
    _server42 = _Server42(('127.0.0.1', 0), m42._StreamHandler)
    _server42.session = _session42
    _server42.daemon_threads = True
    _port42 = _server42.server_address[1]
    _threading42.Thread(target=_server42.serve_forever, daemon=True).start()
    try:
        _conn42 = _http42.HTTPConnection('127.0.0.1', _port42, timeout=20)
        _conn42.request('GET', '/browser?token={}'.format(
            _session42.page_token))
        _answer42 = _conn42.getresponse()
        _answer42.read()
        check("a served request is counted as an exchange",
              _answer42.status == 200 and _session42.exchanges == 1,
              'status={} exchanges={}'.format(_answer42.status,
                                              _session42.exchanges))
        check("and the exchange is readable in the normal log",
              any('exchange 1' in line and '-> 200' in line
                  for line in _grab42.lines),
              _grab42.lines[-1] if _grab42.lines else '(nothing logged)')

        # A request for something that is not the stream or the page is not an
        # exchange we care about -- and a rejected token is still an answer.
        _conn42.request('GET', '/nope')
        _conn42.getresponse().read()
        _conn42.request('GET', '/browser?token=wrong')
        _bad42 = _conn42.getresponse()
        _bad42.read()
        check("a rejected token is recorded too, and junk paths are not",
              _bad42.status == 403 and _session42.exchanges == 2,
              'status={} exchanges={} (the /nope miss must not count)'.format(
                  _bad42.status, _session42.exchanges))

        # The bound: the counter keeps climbing, the log stops at the limit.
        # Counted on 'exchange ' lines only -- the handler's own DEBUG request
        # lines land in the same handler, and counting those measured nothing.
        _limit42 = m42._StreamHandler.EXCHANGE_LOG_LIMIT

        def _ex_lines42():
            return [line for line in _grab42.lines
                    if line.startswith('exchange ')]

        _logged42 = len(_ex_lines42())
        for _ in range(_limit42 + 2):
            _conn42.request('GET', '/browser?token=wrong')
            _conn42.getresponse().read()
        _conn42.close()
        check("every answer is counted but only the first few are described",
              _session42.exchanges == 2 + _limit42 + 2
              and _logged42 == 2 and len(_ex_lines42()) == _limit42,
              'exchanges={} logged={} (limit {}, needs a renderer that retries)'
              .format(_session42.exchanges, len(_ex_lines42()), _limit42))
    finally:
        _server42.shutdown()
        _server42.server_close()
        _plugin_logger42.removeHandler(_grab42)
        _plugin_logger42.setLevel(_old_level42)

    # -- 3. noticing that the renderer closed its half ---------------------
    import socket as _socket42
    _left42, _right42 = _socket42.socketpair()
    _fake42 = type('H42', (), {'connection': _left42})()
    check("an open connection is not mistaken for a closed one",
          m42._StreamHandler._peer_gone(_fake42) is False,
          'a non-blocking peek on a live socket says nothing at all')
    _right42.close()
    time.sleep(0.2)
    check("a closed one is noticed without consuming a byte",
          m42._StreamHandler._peer_gone(_fake42) is True,
          'this is what left seven sockets in CLOSE_WAIT')
    _left42.close()
finally:
    if mirror42 is None:
        print('Part 42 setup error: %s' % _traceback42.format_exc())
    utils.Setting.setting = {}
    _shutil.rmtree(_tmp42, ignore_errors=True)


# --------------------------------------------------------------------------

passed = sum(1 for _, ok, _ in RESULTS if ok)
failed = len(RESULTS) - passed
print("\n=== SUMMARY: {}/{} passed ===".format(passed, len(RESULTS)))
if failed:
    print("FAILED cases:")
    for name, ok, detail in RESULTS:
        if not ok:
            print("  - {}  ({})".format(name, detail))
    sys.exit(1)
print("All checks passed.")
