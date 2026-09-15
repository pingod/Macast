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
import json
import os
import shutil as _shutil
import socket
import ssl
import struct
import sys
import tempfile as _tempfile
import threading
import time
import types

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
