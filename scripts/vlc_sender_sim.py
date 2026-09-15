#!/usr/bin/env python3
"""Simulate VLC's Cast sender, including its *strict* device-auth step.

VLC is the sender that exposes bugs pychromecast cannot, because
``intf_sys_t::processAuthMessage()`` (modules/stream_out/chromecast/
chromecast_ctrl.cpp) bails out of the whole handshake when the auth reply does
not parse::

    if ( authMessage.ParseFromString(msg.payload_binary()) == false ) {
        msg_Warn( m_module, "Failed to parse the payload" );
        return;                       // stays in Authenticating forever
    }
    if (authMessage.has_error()) { ... }
    else if (!authMessage.has_response()) {
        msg_Err( m_module, "Authentication message has no response field");
    }
    else {
        setState( Connecting );
        m_communication->msgConnect( DEFAULT_CHOMECAST_RECEIVER );
        m_communication->msgReceiverGetStatus();
    }

``AuthResponse.signature`` and ``AuthResponse.client_auth_certificate`` are
``required`` in cast_channel.proto, and C++ protobuf validates required fields
while parsing. An auth reply whose ``response`` submessage exists but is empty
therefore makes ``ParseFromString`` return false -> VLC hangs up with no further
message. That is exactly the "discoverable but not castable" symptom.

This script replays VLC's state machine and fails loudly if the reply would not
survive that parse:

    Authenticating --auth--> Connecting --RECEIVER_STATUS(no app)--> Connected
                   --LAUNCH--> Launching --RECEIVER_STATUS(app)--> Ready
                   --LOAD--> Loading/Buffering/Playing

Usage:
    .venv/bin/python scripts/vlc_sender_sim.py [host] [port] [media_url]
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
NS_DEVICEAUTH = "urn:x-cast:com.google.cast.tp.deviceauth"

SENDER = "sender-vlc"          # VLC's literal source_id
RECEIVER = "receiver-0"        # DEFAULT_CHOMECAST_RECEIVER
MEDIA_RECEIVER_APP = "CC1AD845"

# AuthResponse fields declared `required` in cast_channel.proto.
AUTH_RESPONSE_REQUIRED = (1, 2)   # signature, client_auth_certificate


# -- protobuf (exactly the subset cast_channel.proto needs) ------------------

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
    """CastMessage envelope."""
    buf = bytearray()
    buf += _f_varint(1, 0)                       # protocol_version = CASTV2_1_0
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
    """Yield (field_number, wire_type, value). Length-delimited -> bytes."""
    i, n = 0, len(buf)
    while i < n:
        tag = buf[i]
        i += 1
        fnum, wtype = tag >> 3, tag & 0x07
        if wtype == 0:
            val, shift = 0, 0
            while i < n:
                b = buf[i]
                i += 1
                val |= (b & 0x7F) << shift
                shift += 7
                if not (b & 0x80):
                    break
            yield fnum, wtype, val
        elif wtype == 2:
            length, shift = 0, 0
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


def parse_device_auth(blob):
    """Parse a DeviceAuthMessage the way C++ protobuf does.

    Returns (ok, summary). ``ok`` is False when a *required* field is missing
    from a present submessage -- protobuf's ParseFromString() rejects that, and
    VLC treats a rejection as fatal for the whole handshake.
    """
    present = {}
    for fnum, wtype, val in _fields(blob):
        present.setdefault(fnum, val)

    if 3 in present:
        return False, "AuthError set (field 3)"
    if 2 not in present:
        return False, "no response field (has_response() == false)"
    if not isinstance(present[2], bytes):
        return False, "response field is not length-delimited"

    resp_fields = {f for f, _w, _v in _fields(present[2])}
    missing = [f for f in AUTH_RESPONSE_REQUIRED if f not in resp_fields]
    if missing:
        return False, ("AuthResponse missing required field(s) %s "
                       "-> ParseFromString() returns false" % missing)
    return True, "response{signature, client_auth_certificate} present"


class VlcSender(object):
    """Minimal re-implementation of intf_sys_t, including its states."""

    def __init__(self, host, port, verbose=True):
        self.verbose = verbose
        self.state = "Authenticating"
        self.app = None
        self.transport = None
        self.request_id = 1
        self.log = []
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((host, port), timeout=10)
        self.sock = ctx.wrap_socket(raw, server_hostname=host)
        self._note("TLS connected, cipher=%s" % (self.sock.cipher()[0],))

    def _note(self, text):
        self.log.append(text)
        if self.verbose:
            print("    " + text)

    def _set_state(self, state):
        if self.state != state:
            self.state = state
            self._note("state -> %s" % state)

    def send(self, dest, ns, payload, binary=False):
        blob = encode(SENDER, dest, ns, payload, binary)
        self.sock.sendall(struct.pack(">I", len(blob)) + blob)

    def send_json(self, dest, ns, obj):
        self.send(dest, ns, json.dumps(obj))

    def recv(self, timeout=6.0):
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
        except (socket.timeout, TimeoutError):
            return None
        return decode(body)

    # -- VLC state machine -------------------------------------------------

    def step_auth(self):
        """MainLoop(): msgAuth() then processAuthMessage() on the reply."""
        self.send(RECEIVER, NS_DEVICEAUTH, _f_bytes(1, b""), binary=True)
        self._note("-> msgAuth() DeviceAuthMessage{challenge}")

        deadline = time.time() + 8
        while time.time() < deadline:
            msg = self.recv(2.0)
            if msg is None:
                continue
            if msg["namespace"] != NS_DEVICEAUTH:
                self._note("<- unexpected ns=%s" % msg["namespace"])
                continue
            ok, why = parse_device_auth(msg["payload_binary"])
            self._note("<- auth reply: %s" % why)
            if not ok:
                return False
            self._set_state("Connecting")
            # processAuthMessage(): these two go out in the same breath.
            self.send_json(RECEIVER, NS_CONNECTION, {"type": "CONNECT"})
            self._note("-> msgConnect(receiver-0)")
            self.send_json(RECEIVER, NS_RECEIVER,
                           {"type": "GET_STATUS", "requestId": self.request_id})
            self._note("-> msgReceiverGetStatus()")
            self.request_id += 1
            return True
        self._note("!! no auth reply -> still Authenticating (VLC would give up)")
        return False

    def _receiver_app(self, msg):
        """Mirror processReceiverMessage()'s app extraction."""
        if msg["namespace"] != NS_RECEIVER or not msg["payload_utf8"]:
            return None
        try:
            data = json.loads(msg["payload_utf8"])
        except ValueError:
            return None
        if data.get("type") != "RECEIVER_STATUS":
            return None
        apps = (data.get("status") or {}).get("applications") or []
        return apps[0] if apps else None

    def pump(self, seconds, want_app=None):
        """Read for a while, driving the state machine. Returns messages."""
        end = time.time() + seconds
        got = []
        while time.time() < end:
            msg = self.recv(min(1.5, max(0.2, end - time.time())))
            if msg is None:
                continue
            got.append(msg)
            ns = msg["namespace"].rsplit(".", 1)[-1]
            body = msg["payload_utf8"] or ("<%d bytes>" % len(msg["payload_binary"]))
            self._note("<- [%s] %s" % (ns, body[:300]))
            if msg["namespace"] == NS_HEARTBEAT and msg["payload_utf8"]:
                if json.loads(msg["payload_utf8"]).get("type") == "PING":
                    self.send_json(RECEIVER, NS_HEARTBEAT, {"type": "PONG"})

            app = self._receiver_app(msg)
            if app is not None:
                self.app = app
                self.transport = app.get("transportId") or self.transport
            if (self.state == "Connecting" and msg["namespace"] == NS_RECEIVER
                    and msg["payload_utf8"]):
                # VLC: Connecting -> Ready when an app is already running,
                # otherwise -> Connected (which triggers tryLoad -> LAUNCH).
                self._set_state("Ready" if self.app else "Connected")
                break
            if (self.state == "Launching" and msg["namespace"] == NS_RECEIVER
                    and self.app):
                self._set_state("Ready")
                break
            if want_app and self.app and self.state == "Ready":
                break
        return got

    def launch(self):
        """tryLoad() while Connected: msgReceiverLaunchApp()."""
        self._set_state("Launching")
        self.send_json(RECEIVER, NS_RECEIVER, {
            "type": "LAUNCH",
            "appId": MEDIA_RECEIVER_APP,
            "requestId": self.request_id,
        })
        self._note("-> msgReceiverLaunchApp(CC1AD845)")
        self.request_id += 1

    def load(self, url, timeout=20.0, content_type=None):
        """Ready: connect to transportId, then msgPlayerLoad().

        Waits for a *terminal* answer. Accepting BUFFERING as success would
        make this harness pass on a receiver that never actually plays
        anything -- which is exactly the class of bug it exists to catch.
        """
        if not self.transport:
            self._note("!! no transportId")
            return False
        self.send_json(self.transport, NS_CONNECTION, {"type": "CONNECT"})
        self._note("-> msgConnect(%s)" % self.transport)
        self.pump(1.0)
        self._set_state("Loading")
        self.send_json(self.transport, NS_MEDIA, {
            "type": "LOAD",
            "requestId": self.request_id,
            "autoplay": True,
            "currentTime": 0,
            "media": {
                "contentId": url,
                "contentType": content_type or "video/mp4",
                "streamType": "BUFFERED",
                "metadata": {"type": 0, "metadataType": 0, "title": "vlc-sim"},
            },
        })
        self._note("-> msgPlayerLoad(%s)" % url)
        self.request_id += 1

        deadline = time.time() + timeout
        while time.time() < deadline:
            for msg in self.pump(max(0.2, min(2.0, deadline - time.time()))):
                if msg["namespace"] != NS_MEDIA or not msg["payload_utf8"]:
                    continue
                try:
                    payload = json.loads(msg["payload_utf8"])
                except ValueError:
                    continue
                if payload.get("type") == "LOAD_FAILED":
                    self._note("<- LOAD_FAILED")
                    self._set_state("LoadFailed")
                    return False
                status = payload.get("status") or [{}]
                state = (status[0] or {}).get("playerState")
                if state:
                    self._set_state(state)
            if self.state == "LoadFailed":
                return False
            if self.state == "PLAYING":
                return True
        self._note("!! no PLAYING within %.0fs (state=%s)" % (timeout, self.state))
        return False


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 8009
    url = (sys.argv[3] if len(sys.argv) > 3
           else "https://commondatastorage.googleapis.com/gtv-videos-bucket/"
                "sample/BigBuckBunny.mp4")

    print("== 1. TLS -> %s:%d (sender-vlc) ==" % (host, port))
    s = VlcSender(host, port)

    print("== 2. Authenticating --msgAuth--> ==")
    if not s.step_auth():
        print("\n[FAIL] VLC 会卡在 Authenticating，握手无法继续。")
        return 1
    print("   ok: auth reply accepted -> Connecting")

    print("== 3. Connecting --RECEIVER_STATUS--> ==")
    s.pump(3.0)
    if s.state == "Connecting":
        print("\n[FAIL] 收不到 RECEIVER_STATUS")
        return 1
    print("   状态: %s" % s.state)

    if s.state == "Connected":
        print("== 4. Connected --LAUNCH--> ==")
        s.launch()
        s.pump(4.0)
        print("   状态: %s, transportId=%s" % (s.state, s.transport))

    if s.state != "Ready":
        print("\n[FAIL] 未能进入 Ready（LAUNCH 后拿不到 app/transportId）")
        return 1

    print("== 5. Ready --LOAD--> ==")
    ok = s.load(url)
    print("   状态: %s" % s.state)

    print("\n=== VLC 模拟结果: %s ===" % ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
