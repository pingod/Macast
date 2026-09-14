# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Chromecast receiver protocol (Google Cast v2).
#
# This makes Macast show up as a Chromecast target on the local network so that
# a phone / Chrome / Android can "cast" to the computer. The media model is the
# same as DLNA: the sender provides a media URL and Macast plays it with mpv.
#
# Components
#   * mDNS   : _googlecast._tcp.local  (port 8009)  -- how senders discover us
#   * Cast v2: TLS on 8009             (protobuf envelope + JSON namespaces)
#   * setup  : plain HTTP on 8008      (/setup/eureka_info, /setup/icon.png)
#
# NOTE: This is an *uncertified* receiver. We answer the device-auth handshake
# with an empty signature, which Chrome/Android accept for media casting (with a
# one-time security prompt). Full Google-certified auth would require Google's
# private key and is out of scope.
#

import base64
import json
import logging
import os
import ssl
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .discovery import MDNSAdvertiser
from .protocol import Protocol
from .utils import Setting, SETTING_DIR

logger = logging.getLogger("Chromecast")

CAST_PORT = 8009
SETUP_PORT = 8008
CAST_SERVICE = "_googlecast._tcp.local."
# Fall back to a nearby port if the canonical one is taken; mDNS carries the
# port we actually bound, so senders follow automatically.
PORT_FALLBACK_RANGE = 20
DEFAULT_MEDIA_APP_ID = "CC1AD845"  # Google's default media receiver
APP_BACKDROP_ID = "E8C28D3C"       # backdrop / ambient screen

# Cast v2 namespaces (see thibauts/node-castv2 protocol reference).
NS_CONNECTION = "urn:x-cast:com.google.cast.tp.connection"
NS_HEARTBEAT = "urn:x-cast:com.google.cast.tp.heartbeat"
NS_RECEIVER = "urn:x-cast:com.google.cast.receiver"
NS_MEDIA = "urn:x-cast:com.google.cast.media"
NS_DEVICEAUTH = "urn:x-cast:com.google.cast.tp.deviceauth"
# Namespaces a media receiver app must expose so senders know what it speaks.
APP_NAMESPACES = [NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA,
                  "urn:x-cast:com.google.cast.player.message"]

# ---------------------------------------------------------------------------
# Minimal protobuf codec (only the messages Cast v2 needs)
# ---------------------------------------------------------------------------


def _encode_varint(n):
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


def _field_varint(field_num, value):
    return bytes([(field_num << 3) | 0]) + _encode_varint(value)


def _field_bytes(field_num, data):
    return bytes([(field_num << 3) | 2]) + _encode_varint(len(data)) + data


def encode_cast_message(source_id, dest_id, namespace, payload, binary=False):
    """Build a CastMessage protobuf blob (not yet length-prefixed)."""
    buf = bytearray()
    buf += _field_varint(1, 0)  # protocol_version = CASTV2_1_0
    buf += _field_bytes(2, source_id.encode("utf-8"))
    buf += _field_bytes(3, dest_id.encode("utf-8"))
    buf += _field_bytes(4, namespace.encode("utf-8"))
    if binary:
        buf += _field_varint(5, 1)  # payload_type = BINARY
        buf += _field_bytes(7, payload)
    else:
        buf += _field_varint(5, 0)  # payload_type = STRING
        buf += _field_bytes(6, payload.encode("utf-8"))
    return bytes(buf)


def encode_auth_response():
    """Respond to a device-auth challenge with an empty signature.

    A certified device would embed its signed certificate here; uncertified
    receivers return empty fields, which clients accept for media casting.
    """
    # AuthResponse { signature = b'', client_auth_certificate = b'' }
    return _field_bytes(1, b"") + _field_bytes(2, b"")


def _parse_fields(buf):
    """Yield (field_number, wire_type, value_bytes) for a protobuf message."""
    i = 0
    n = len(buf)
    while i < n:
        tag = buf[i]
        i += 1
        field_num = tag >> 3
        wire_type = tag & 0x07
        if wire_type == 0:  # varint
            shift = 0
            val = 0
            while i < n:
                b = buf[i]
                i += 1
                val |= (b & 0x7F) << shift
                shift += 7
                if not (b & 0x80):
                    break
            yield field_num, wire_type, val
        elif wire_type == 2:  # length-delimited
            length = 0
            shift = 0
            while i < n:
                b = buf[i]
                i += 1
                length |= (b & 0x7F) << shift
                shift += 7
                if not (b & 0x80):
                    break
            val = buf[i : i + length]
            i += length
            yield field_num, wire_type, val
        else:  # unsupported wire type, bail
            return


def parse_cast_message(buf):
    """Parse a length-prefixed CastMessage into a dict."""
    msg = {
        "source_id": "",
        "destination_id": "",
        "namespace": "",
        "payload_type": 0,
        "payload_utf8": "",
        "payload_binary": b"",
    }
    for fnum, wtype, val in _parse_fields(buf):
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


# ---------------------------------------------------------------------------
# Chromecast protocol
# ---------------------------------------------------------------------------


class ChromecastProtocol(Protocol):
    """Act as a Chromecast receiver. Reuses the mpv renderer for playback."""

    uses_ssdp = False  # we advertise over mDNS instead

    def __init__(self):
        super().__init__()
        self._advertiser = None
        self._tls_server = None
        self._setup_server = None
        self._stop_event = threading.Event()
        self._connections = []
        self._senders = {}          # source_id -> socket (for broadcasts)
        self._session_id = None
        self._media = None          # current Cast media descriptor
        self._media_session_id = 1
        self._position = 0.0
        self._transport_id = "MacastTransport"
        self._lock = threading.Lock()
        self.cast_port = CAST_PORT   # actual bound port (may differ)
        self.setup_port = SETUP_PORT
        self._cert_path = os.path.join(SETTING_DIR, "macast_cast.crt")
        self._key_path = os.path.join(SETTING_DIR, "macast_cast.key")

    # -- lifecycle ----------------------------------------------------------

    def start(self):
        self._stop_event.clear()
        self._ensure_cert()
        # Bind listeners first so mDNS can advertise the ports we really bound.
        self._start_tls_server()
        self._start_setup_server()
        self._advertise()
        logger.info("ChromecastProtocol started on port %s", self.cast_port)

    def stop(self):
        self._stop_event.set()
        if self._advertiser is not None:
            self._advertiser.close()
            self._advertiser = None
        for sock in self._connections:
            try:
                sock.close()
            except Exception:
                pass
        if self._tls_server is not None:
            self._tls_server.close()
            self._tls_server = None
        if self._setup_server is not None:
            self._setup_server.shutdown()
            self._setup_server.server_close()
            self._setup_server = None
        logger.info("ChromecastProtocol stopped")

    def _advertise(self):
        self._advertiser = MDNSAdvertiser()
        name = "{}._googlecast._tcp.local.".format(Setting.get_friendly_name())
        props = {
            "id": Setting.get_usn(),
            "fn": Setting.get_friendly_name(),
            "ca": "1",        # cast capable
            "st": "0",        # standard
            "bs": "FA8B",
            "ic": "/setup/icon.png",
            "ve": "05",
            "md": "Macast",
            "rm": "Macast",
            "rs": "Macast Receiver",
        }
        self._advertiser.advertise(CAST_SERVICE, name, self.cast_port, props)

    def _ensure_cert(self):
        if os.path.exists(self._cert_path) and os.path.exists(self._key_path):
            return
        from .server import Service

        cert, key = Service._ensure_self_signed_cert()
        # Reuse the HTTPS cert/key if present, otherwise we already generated one.
        if cert and key:
            self._cert_path, self._key_path = cert, key

    # -- TLS Cast v2 channel ------------------------------------------------

    def _start_tls_server(self):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(certfile=self._cert_path, keyfile=self._key_path)
        for port in range(CAST_PORT, CAST_PORT + PORT_FALLBACK_RANGE):
            raw = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            raw.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                raw.bind(("0.0.0.0", port))
            except OSError as e:
                logger.debug("Cast bind port %d failed: %s", port, e)
                raw.close()
                continue
            raw.listen(5)
            self._tls_server = ctx.wrap_socket(raw, server_side=True)
            self.cast_port = port
            if port != CAST_PORT:
                logger.warning("Cast port %d in use, using %d instead", CAST_PORT, port)
            t = threading.Thread(target=self._accept_loop, name="CAST_TLS", daemon=True)
            t.start()
            return
        logger.error("Cast TLS server failed: no free port in range")

    def _accept_loop(self):
        while not self._stop_event.is_set():
            try:
                conn, _addr = self._tls_server.accept()
            except OSError:
                break
            self._connections.append(conn)
            t = threading.Thread(
                target=self._handle_client, args=(conn,), name="CAST_CLIENT", daemon=True
            )
            t.start()

    def _handle_client(self, sock):
        try:
            while not self._stop_event.is_set():
                # 4-byte big-endian length prefix + protobuf CastMessage
                hdr = self._recv_exact(sock, 4)
                if hdr is None:
                    break
                (length,) = _struct_unpack(hdr)
                body = self._recv_exact(sock, length)
                if body is None:
                    break
                self._on_message(sock, parse_cast_message(body))
        except Exception as e:
            logger.debug("Cast client disconnected: %s", e)
        finally:
            try:
                sock.close()
            except Exception:
                pass

    @staticmethod
    def _recv_exact(sock, n):
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def _send(self, sock, source_id, dest_id, namespace, payload, binary=False):
        blob = encode_cast_message(source_id, dest_id, namespace, payload, binary)
        sock.sendall(_struct_pack(len(blob)) + blob)

    def _on_message(self, sock, msg):
        ns = msg["namespace"]
        dest = msg["destination_id"]
        src = msg["source_id"]
        if ns == NS_DEVICEAUTH:
            # auth challenge -> empty signature response (uncertified receiver)
            logger.info("Cast device-auth challenge from %s "
                        "(uncertified: empty signature)", src)
            self._send(sock, "receiver-0", src, ns, encode_auth_response(), binary=True)
            return
        if msg["payload_type"] != 0:
            return
        try:
            data = json.loads(msg["payload_utf8"])
        except Exception:
            return
        msg_type = data.get("type")
        if ns == NS_CONNECTION:
            self._on_connection(sock, src, data, msg_type)
        elif ns == NS_HEARTBEAT:
            self._on_heartbeat(sock, src, msg_type)
        elif ns == NS_RECEIVER:
            self._on_receiver(sock, src, data, msg_type)
        elif ns == NS_MEDIA:
            self._on_media(sock, src, data, msg_type)

    # -- Cast v2 namespaces -------------------------------------------------

    def _on_connection(self, sock, src, data, msg_type):
        """Virtual connection management (tp.connection)."""
        if msg_type == "CONNECT":
            self._senders[src] = sock
            logger.info("Cast sender connected: %s", src)
            # Acknowledge; also lets the sender start receiving broadcasts.
            self._send(sock, "receiver-0", src, NS_CONNECTION,
                       json.dumps({"type": "CONNECT"}))
        elif msg_type == "CLOSE":
            self._senders.pop(src, None)
            logger.info("Cast sender disconnected: %s", src)

    def _on_heartbeat(self, sock, src, msg_type):
        """Keep-alive (tp.heartbeat) -- senders PING every ~5s."""
        if msg_type == "PING":
            self._send(sock, "receiver-0", src, NS_HEARTBEAT,
                       json.dumps({"type": "PONG"}))

    def _on_receiver(self, sock, src, data, msg_type):
        if msg_type in ("GET_STATUS", "PING"):
            self._send_receiver_status(sock, src, data.get("requestId"))
        elif msg_type == "LAUNCH":
            with self._lock:
                self._session_id = _random_session()
            self._send_receiver_status(sock, src, data.get("requestId"))
        elif msg_type == "STOP":
            with self._lock:
                self._session_id = None
            self.renderer.set_media_stop()
            self._send_receiver_status(sock, src, data.get("requestId"))
        elif msg_type == "GET_APP_AVAILABILITY":
            # Report the apps we can serve: the default media receiver only.
            self._send(sock, "receiver-0", src, NS_RECEIVER, json.dumps({
                "type": "GET_APP_AVAILABILITY",
                "requestId": data.get("requestId"),
                "availability": {DEFAULT_MEDIA_APP_ID: "APP_AVAILABLE"},
            }))
        elif msg_type == "SET_VOLUME":
            vol = data.get("volume", {})
            if "level" in vol:
                self.renderer.set_media_volume(int(vol["level"] * 100))
            if "muted" in vol:
                self.renderer.set_media_mute(bool(vol["muted"]))

    def _send_receiver_status(self, sock, src, request_id):
        with self._lock:
            session = self._session_id
        apps = []
        if session:
            apps.append(
                {
                    "appId": DEFAULT_MEDIA_APP_ID,
                    "displayName": "Macast",
                    "sessionId": session,
                    "statusText": "Macast",
                    "transportId": self._transport_id,
                    # Senders establish a virtual connection to transportId
                    # and only then speak the media namespace.
                    "namespaces": APP_NAMESPACES,
                }
            )
        status = {
            "requestId": request_id,
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": apps,
                "isActiveInput": True,
                "volume": {"level": 1.0, "muted": False},
            },
        }
        self._send(sock, "receiver-0", src, NS_RECEIVER, json.dumps(status))

    def _on_media(self, sock, src, data, msg_type):
        """Media playback control (media namespace)."""
        with self._lock:
            self._media_session_id = data.get("mediaSessionId",
                                              self._media_session_id)
        if msg_type == "LOAD":
            media = data.get("media", {})
            url = media.get("contentId") or media.get("contentUrl")
            if not url:
                # Nothing playable: tell the sender instead of failing silently.
                self._send(sock, self._transport_id, src, NS_MEDIA, json.dumps({
                    "type": "LOAD_FAILED",
                    "requestId": data.get("requestId"),
                }))
                return
            with self._lock:
                self._session_id = _random_session()
                self._media = media
                self._position = float(data.get("currentTime", 0) or 0)
            logger.info("Cast LOAD url=%s", url)
            self.renderer.set_media_url(url, start=str(int(self._position)))
            self._send_media_status(sock, src, "BUFFERING", data.get("requestId"))
            self._send_media_status(sock, src, "PLAYING", data.get("requestId"))
        elif msg_type == "PLAY":
            self.renderer.set_media_resume()
            self._send_media_status(sock, src, "PLAYING", data.get("requestId"))
        elif msg_type == "PAUSE":
            self.renderer.set_media_pause()
            self._send_media_status(sock, src, "PAUSED", data.get("requestId"))
        elif msg_type == "STOP":
            self.renderer.set_media_stop()
            with self._lock:
                self._session_id = None
                self._media = None
            self._send_media_status(sock, src, "IDLE", data.get("requestId"))
        elif msg_type == "SEEK":
            with self._lock:
                self._position = float(data.get("currentTime", 0) or 0)
                position = self._position
            self.renderer.set_media_position(str(position))
            self._send_media_status(sock, src, "PLAYING", data.get("requestId"))
        elif msg_type == "GET_STATUS":
            self._send_media_status(sock, src, None, data.get("requestId"))

    def _send_media_status(self, sock, src, player_state, request_id):
        with self._lock:
            session = self._session_id
            media = self._media
            position = self._position
            media_session_id = self._media_session_id
            state = player_state or ("PLAYING" if media else "IDLE")
        status = {
            "requestId": request_id,
            "type": "MEDIA_STATUS",
            "status": [
                {
                    "mediaSessionId": media_session_id,
                    "playbackRate": 1,
                    "playerState": state,
                    "currentTime": position,
                    "supportedMediaCommands": 15,
                    "volume": {"level": 1.0, "muted": False},
                    "sessionId": session or _random_session(),
                    "media": media,
                }
            ],
        }
        self._send(sock, self._transport_id, src, NS_MEDIA,
                   json.dumps(status, default=str))

    # -- setup HTTP server (8008) ------------------------------------------

    def _start_setup_server(self):
        handler = self._make_setup_handler()
        for port in range(SETUP_PORT, SETUP_PORT + PORT_FALLBACK_RANGE):
            try:
                server = ThreadingHTTPServer(("0.0.0.0", port), handler)
                self._setup_server = server
                self.setup_port = port
                t = threading.Thread(target=server.serve_forever,
                                     name="CAST_SETUP", daemon=True)
                t.start()
                return
            except OSError as e:
                logger.debug("Cast setup bind port %d failed: %s", port, e)
                continue
        logger.warning("Cast setup HTTP server failed: no free port in range")

    def _make_setup_handler(self):
        protocol = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                if self.path.startswith("/setup/eureka_info"):
                    info = {
                        "name": Setting.get_friendly_name(),
                        "model_name": "Macast",
                        "manufacturer": "Macast",
                        "upnp_device_type": "Macast",
                        "capabilities": ["video_out", "audio_out"],
                    }
                    body = json.dumps(info).encode("utf-8")
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                elif self.path.startswith("/setup/icon.png"):
                    self.send_response(200)
                    self.send_header("Content-Type", "image/png")
                    self.end_headers()
                else:
                    self.send_response(404)
                    self.end_headers()

        return Handler


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

import struct  # noqa: E402  (kept at bottom to avoid clutter above)

_struct_pack = struct.Struct(">I").pack
_struct_unpack = struct.Struct(">I").unpack


def _random_session():
    import uuid

    return uuid.uuid4().hex
