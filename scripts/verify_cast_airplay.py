#!/usr/bin/env python3
# Copyright (c) 2021 by xfangfang. All Rights Reserved.
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
# The online plugins in plugins/ are written against the app's public surface:
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
_plugin_dir = os.path.join(REPO, "plugins")


def _load_plugin(name, filename=None):
    """Import plugins/<filename> as a standalone module (as Macast does)."""
    path = os.path.join(_plugin_dir, filename or (name + ".py"))
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
# plugins/macast_ytdlp.py is the first entry in the (previously empty) index:
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

    _plugin_file = os.path.join(REPO, "plugins", "macast_ytdlp.py")
    check("the downloader plugin ships in plugins/",
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
      '[avfoundation @ 0x1] The following devices were found:' \
      '[avfoundation @ 0x1] Video devices:' \
      '[avfoundation @ 0x1]    "FaceTime HD Camera"' \
      '[avfoundation @ 0x1]    "Capture screen 0"' \
      '[avfoundation @ 0x1] Audio devices:' \
      '[avfoundation @ 0x1]    "MacBook Pro Microphone"'
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
      '[avfoundation @ 0x1] The following devices were found:' \
      '[avfoundation @ 0x1] Video devices:' \
      '[avfoundation @ 0x1]    "FaceTime HD Camera"' \
      '[avfoundation @ 0x1]    "Capture screen 0"' \
      '[avfoundation @ 0x1] Audio devices:' \
      '[avfoundation @ 0x1]    "MacBook Pro Microphone"' \
      '[avfoundation @ 0x1]    "BlackHole 2ch"'
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
        check("windows dispatches to gdigrab, video only",
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

        # -- the menu -----------------------------------------------------------
        def _menu_texts21(items):
            texts = []
            for i in items:
                texts.append(i.text)
                texts.extend(_menu_texts21(i.children or []))
            return texts

        mirror._devices = [('Living Room TV', '192.0.2.7', 8009)]

        mirror._capture_cache.clear()
        _labels21 = _menu_texts21(mirror.ScreenMirrorSetting().build_menu())
        check("the menu offers a start entry and a target submenu",
              any('开始镜像' in t for t in _labels21)
              and any(t == '输出目标' for t in _labels21), str(_labels21))
        check("before the first probe the menu defers the audio state",
              any('系统声音：开始镜像后' in t for t in _labels21), str(_labels21))
        if sys.platform == 'darwin':
            check("before the first probe the menu already offers the one click",
                  any('一键设置' in t for t in _labels21), str(_labels21))
        mirror.probe_capture(fake_ffmpeg21)          # the video-only fake
        _labels21 = _menu_texts21(mirror.ScreenMirrorSetting().build_menu())
        check("a BlackHole-less probe names the fix instead of staying mute",
              any('blackhole' in t.lower() for t in _labels21), str(_labels21))
        mirror._capture_cache.clear()
        mirror.probe_capture(fake_bh21)              # the BlackHole fake
        _labels21 = _menu_texts21(mirror.ScreenMirrorSetting().build_menu())
        check("with system audio captured the menu says so",
              any('系统声音：已启用' in t for t in _labels21), str(_labels21))
        if sys.platform == 'darwin':
            check("once audio works the one-click entry steps aside",
                  not any('一键设置' in t for t in _labels21), str(_labels21))

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

        def _stub_audio21(**over):
            for _name, _fn in [('find_ffmpeg', lambda: 'ffmpeg'),
                               ('_has_blackhole', lambda f: False),
                               ('_download_blackhole',
                                lambda: _stub21.setdefault('pkg', '/tmp/bh.pkg')),
                               ('_wait_for_blackhole', lambda f, timeout=0: True),
                               ('_route_audio_through_blackhole', lambda f: True),
                               ('_open_audio_midi_setup', lambda rep: None),
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
            _stub_audio21()
            mirror._capture_cache[('ffmpeg', 'darwin', True)] = object()
            _msgs21 = []
            _ok21 = mirror.setup_system_audio(_msgs21.append)
            check("the assisted setup installs, waits, routes and reports",
                  _ok21 and 'downloading' not in _stub21
                  and mirror._capture_cache == {},
                  str(_msgs21))
            _stub21.clear()
            _msgs21 = []

            def _route_fail21(f):
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
      '[avfoundation @ 0x1] The following devices were found:' \
      '[avfoundation @ 0x1] Video devices:' \
      '[avfoundation @ 0x1]    "Capture screen 0"' \
      '[avfoundation @ 0x1]    "Capture screen 1"' \
      '[avfoundation @ 0x1]    "Capture screen 2"' \
      '[avfoundation @ 0x1] Audio devices:' \
      '[avfoundation @ 0x1]    "MacBook Pro Microphone"'
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
        check("one keyframe a second is what makes both of them joinable",
              _mp22[_mp22.index('-g') + 1] == str(mirror.FPS)
              and _ts22[_ts22.index('-g') + 1] == str(mirror.FPS), str(_mp22))

        # -- quality presets -------------------------------------------------
        check("the four presets name a height and a bitrate",
              mirror.QUALITIES['360'] == (360, 2500000)
              and mirror.QUALITIES['1080'] == (1080, 10000000)
              and mirror.QUALITIES['source'][0] == 0, str(mirror.QUALITIES))
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
        check("a late joiner is handed the header, then the tail",
              _late22.get_nowait() == b'ftypisomxx'
              and _late22.get_nowait() == b'moofAAAA'
              and _late22.get_nowait() == b'moofBBBB')
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
            check("a pushed url is refused loudly instead of killing the mirror",
                  any('无法播放' in str(n) for n in _notify22)
                  and mir22.is_mirroring()
                  and mir22.playing_url() == _url_before22, str(_notify22))
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

            def stop_mirror(self):
                self.stops += 1

            def start_mirror(self):
                self.starts += 1

            def viewer_url(self):
                return 'http://192.0.2.9:5555/browser?token=abc123'

            def stats(self):
                return {'kind': 'browser', 'clients': 2, 'chunks': 40,
                        'bytes': 500000, 'drops': 3, 'mbps': 4.2, 'seconds': 65}

        def _menu_texts22(items):
            texts = []
            for i in items:
                texts.append(i.text)
                texts.extend(_menu_texts22(i.children or []))
            return texts

        def _find22(items, text):
            for i in items:
                if i.text == text:
                    return i
                hit = _find22(i.children or [], text)
                if hit is not None:
                    return hit
            return None

        fake22 = _FakeMirror22()
        mirror._devices = [('Living Room TV', '192.0.2.7', 8009)]
        setting22 = mirror.ScreenMirrorSetting()
        _saved_renderer_fn22 = setting22._renderer
        setting22._renderer = lambda: fake22
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'browser')
        mirror.probe_capture(fake_ffmpeg22, 'darwin', True)
        _items22 = setting22.build_menu()
        _labels22 = _menu_texts22(_items22)
        check("both output targets are offered, and the running one is checked",
              _find22(_items22, 'Chromecast / Google TV').checked is False
              and _find22(_items22, '浏览器（打开网址即可看）').checked is True,
              str(_labels22))
        check("a browser mirror stops offering Chromecast devices",
              'Living Room TV · 192.0.2.7' not in _labels22, str(_labels22))
        check("and offers the viewing address to copy instead",
              '复制观看地址' in _labels22
              and 'http://192.0.2.9:5555/browser?token=abc123' in _labels22,
              str(_labels22))
        check("all four quality presets are on the menu",
              sum(1 for t in _menu_texts22(
                  _find22(_items22, '画质').children or []) if 'Mbps' in t) == 4,
              str(_labels22))
        check("the display picker is filled from the cached probe",
              _find22(_items22, '采集屏幕') is not None
              and any('Capture screen 1' in t for t in _labels22),
              str(_labels22))
        check("a running mirror reports what it is costing",
              any('已镜像 1:05' in t and '4.2 Mbps' in t and '丢块 3' in t
                  for t in _labels22), str(_labels22))

        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
        _items22 = setting22.build_menu()
        _labels22 = _menu_texts22(_items22)
        check("the Chromecast target puts the devices back on the menu",
              _find22(_items22, 'Chromecast / Google TV').checked is True
              and 'Living Room TV · 192.0.2.7' in _labels22
              and '重新搜索' in _labels22, str(_labels22))
        check("a Chromecast mirror has no address to copy",
              '复制观看地址' not in _labels22, str(_labels22))

        setting22.on_output_clicked(mirror.MenuItem('x', data='browser'))
        check("switching the output restarts the running pipeline",
              mirror.output_kind() == 'browser' and fake22.stops == 1
              and fake22.starts == 1, str(fake22.stops))
        fake22.stops = fake22.starts = 0
        setting22.on_output_clicked(mirror.MenuItem('x', data='browser'))
        check("clicking the output that is already chosen changes nothing",
              fake22.stops == 0 and fake22.starts == 0)

        mirror._capture_cache[('ffmpeg', 'darwin', True)] = object()
        setting22.on_cursor_clicked(mirror.MenuItem('显示鼠标指针'))
        check("the pointer toggle stores the wish and clears the probe cache",
              mirror.cursor_enabled() is False and mirror._capture_cache == {})
        mirror._capture_cache[('ffmpeg', 'darwin', False)] = object()
        setting22.on_cursor_clicked(mirror.MenuItem('显示鼠标指针'))
        check("clicking the pointer entry again puts it back, and empties the cache",
              mirror.cursor_enabled() is True and mirror._capture_cache == {})
        utils.Setting.unset(mirror.SettingProperty.Mirror_Cursor)

        setting22.on_encoder_clicked(mirror.MenuItem('硬件编码'))
        check("the encoder toggle is only honest about what it can do",
              mirror.encoder_kind() == ('hardware' if sys.platform == 'darwin'
                                        else 'software'),
              str(utils.Setting.get(mirror.SettingProperty.Mirror_Encoder, '')))
        utils.Setting.unset(mirror.SettingProperty.Mirror_Encoder)

        setting22.on_screen_clicked(mirror.MenuItem('2 · Capture screen 1',
                                                   data='2'))
        check("choosing a display stores the index the probe uses",
              str(utils.Setting.get(mirror.SettingProperty.Mirror_Screen, '')) == '2')
        setting22.on_screen_clicked(mirror.MenuItem('第一块屏幕（默认）', data=''))
        check("choosing the default takes the stored value away again",
              not utils.Setting.has(mirror.SettingProperty.Mirror_Screen))

        _clip22 = types.ModuleType('pyperclip')
        _copied22 = []
        _clip22.copy = lambda text: _copied22.append(text)
        _py22 = sys.modules.get('pyperclip')
        sys.modules['pyperclip'] = _clip22
        try:
            utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'browser')
            setting22.on_copy_url_clicked(mirror.MenuItem('复制观看地址'))
            check("the copy entry puts the session address on the clipboard",
                  _copied22 == ['http://192.0.2.9:5555/browser?token=abc123'],
                  str(_copied22))
        finally:
            if _py22 is not None:
                sys.modules['pyperclip'] = _py22
            else:
                sys.modules.pop('pyperclip', None)
            utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
            setting22._renderer = _saved_renderer_fn22
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
      '[avfoundation @ 0x1] The following devices were found:' \
      '[avfoundation @ 0x1] Video devices:' \
      '[avfoundation @ 0x1]    "Capture screen 0"' \
      '[avfoundation @ 0x1] Audio devices:' \
      '[avfoundation @ 0x1]    "MacBook Pro Microphone"'
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
              mirror.content_features(_pal23).startswith('DLNA.ORG_OP=01')
              and 'DLNA.ORG_PN' not in mirror.content_features(_pal23),
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
              and _tsh23[_tsh23.index('pipe:1') - 1] == 'mpegts'
              and _tsh23[_tsh23.index('-g') + 1] == '25'
              and 'mpeg2video' not in _tsh23, str(_tsh23))
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
        _saved_prefill23 = mirror.DLNA_PREFILL_BYTES
        mirror.DLNA_PREFILL_BYTES = 4096
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
            mirror.DLNA_PREFILL_BYTES = _saved_prefill23

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
        mirror.DLNA_PREFILL_BYTES = 24576
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
            mirror.DLNA_PREFILL_BYTES = _saved_prefill23
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

        def _menu_texts23(items):
            texts = []
            for i in items:
                texts.append(i.text)
                texts.extend(_menu_texts23(i.children or []))
            return texts

        def _find23(items, text):
            for i in items:
                if i.text == text:
                    return i
                hit = _find23(i.children or [], text)
                if hit is not None:
                    return hit
            return None

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
        _items23 = setting23.build_menu()
        _labels23 = _menu_texts23(_items23)
        check("the menu offers three targets and marks the running one",
              _find23(_items23, 'Chromecast / Google TV').checked is False
              and _find23(_items23, 'DLNA 电视（老电视，MPEG-PS）').checked is True
              and _find23(_items23, '浏览器（打开网址即可看）').checked is False,
              str(_labels23))
        check("a DLNA target lists the renderers discovery found",
              '厨房的小电视 · 192.0.2.40' in _labels23
              and '客厅 · 192.0.2.41' in _labels23
              and '重新搜索' in _labels23, str(_labels23))
        check("and the live line names the profile, the TV's state and the hoard",
              any('已镜像 1:15' in t and '档位 ps-pal' in t
                  and '电视 PLAYING' in t and '缓冲 20 MiB' in t
                  for t in _labels23), str(_labels23))
        check("all five compatibility profiles are on the menu",
              len(_menu_texts23(_find23(_items23, '兼容档位').children or []))
              == len(mirror.DLNA_PROFILES)
              and _find23(_items23, 'MPEG-PS · PAL 576p（老电视首选）').checked
              is True, '')
        check("a DLNA mirror has no address to copy",
              '复制观看地址' not in _labels23, str(_labels23))
        setting23.on_profile_clicked(mirror.MenuItem('x', data='ts-h264'))
        check("choosing a profile stores it and restarts the running stream",
              mirror.dlna_profile_id(mirror.dlna_profile()) == 'ts-h264'
              and fake23.stops == 1 and fake23.starts == 1, str(fake23.stops))
        fake23.stops = fake23.starts = 0
        setting23.on_profile_clicked(mirror.MenuItem('x', data='ts-h264'))
        check("clicking the profile that is already chosen changes nothing",
              fake23.stops == 0 and fake23.starts == 0)
        setting23.on_dlna_target_clicked(
            mirror.MenuItem('x', data=('客厅', 'http://192.0.2.41:49152/x')))
        check("choosing a TV stores its control URL and leaves the Chromecast "
              "target alone",
              mirror.dlna_target() == ('客厅', 'http://192.0.2.41:49152/x')
              and mirror.output_kind() == 'dlna', str(mirror.dlna_target()))
        setting23.on_refresh(mirror.MenuItem('重新搜索'))
        check("refresh on this target re-runs SSDP, not the Chromecast search",
              _searches23 == ['dlna'], str(_searches23))
        check("an empty renderer list says so instead of vanishing",
              (setattr(mirror, '_dlna_devices', []), True)[1]
              and any('没有发现' in t or '搜索中' in t
                      for t in _menu_texts23(setting23.build_menu())), '')
        utils.Setting.unset(mirror.SettingProperty.Mirror_Output)
        _searches23[:] = []
        setting23.build_menu()
        check("and a cast target still searches for Chromecasts",
              _searches23 == ['cast'], str(_searches23))
        utils.Setting.set(mirror.SettingProperty.Mirror_Output, 'dlna')
        _searches23[:] = []
        setting23.build_menu()
        check("opening the menu on an empty renderer list kicks a search",
              _searches23 == ['dlna'], str(_searches23))
        mirror._dlna_devices = [('厨房的小电视', _control23, '192.0.2.40')]
        _searches23[:] = []
        setting23.build_menu()
        check("a full renderer list does not search on every redraw",
              _searches23 == [], str(_searches23))
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
# Summary
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
