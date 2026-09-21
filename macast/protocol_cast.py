# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.
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

import json
import logging
import os
import ssl
import socket
import time
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .discovery import MDNSAdvertiser
from .protocol import Protocol
from .utils import Setting, SETTING_DIR

logger = logging.getLogger("Chromecast")

CAST_PORT = 8009
SETUP_PORT = 8008
#: The HTTPS twin of the setup API. Not our choice: it is a client-side
#: constant. pychromecast's ``get_cast_type`` -- the function the mDNS discovery
#: path calls -- builds ``https://host:8443`` and has no plain-HTTP fallback
#: (dial.py:154-161, FORMAT_BASE_URL_HTTPS at dial.py:26), so a listener on any
#: other port would simply never be asked. Real devices expose the same
#: ``eureka_info`` JSON on 8008 (clear) and 8443 (TLS).
HTTPS_SETUP_PORT = 8443
CAST_SERVICE = "_googlecast._tcp.local."
# Fall back to a nearby port if the canonical one is taken; mDNS carries the
# port we actually bound, so senders follow automatically.
PORT_FALLBACK_RANGE = 20
DEFAULT_MEDIA_APP_ID = "CC1AD845"  # Google's default media receiver
# The name a real device reports for CC1AD845. Senders compare against it --
# mkchromecast's --hijack re-issues LOAD every 5s while it differs -- so this
# is protocol-visible, not cosmetics.
DEFAULT_MEDIA_APP_NAME = "Default Media Receiver"
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


def _app_namespaces():
    """`namespaces` as the wire format wants it.

    cast_channel.proto declares ``message Namespace { string name = 1; }``, so
    RECEIVER_STATUS carries a list of *objects*, not a list of strings. Sending
    bare strings makes senders that parse it strictly (pychromecast, VLC, the
    Cast SDK) fail with "namespace not supported by current app" and never
    issue the LOAD -- the device is discovered but cannot be cast to.
    """
    return [{"name": ns} for ns in APP_NAMESPACES]

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


def encode_auth_challenge():
    """DeviceAuthMessage carrying an empty AuthChallenge (field 1)."""
    return _field_bytes(1, b"")


def encode_auth_response(signature=b"", certificate=b"",
                         intermediates=(), crl=b""):
    """Answer a device-auth challenge.

    The bytes on the wire must be a **DeviceAuthMessage**, not a bare
    AuthResponse::

        message DeviceAuthMessage {
          optional AuthChallenge challenge = 1;
          optional AuthResponse  response  = 2;
          optional AuthError     error     = 3;
        }
        message AuthResponse {
          required bytes signature               = 1;
          required bytes client_auth_certificate = 2;
          repeated bytes intermediate_certificate = 3;
          ...
        }

    ``signature`` and ``client_auth_certificate`` are *required*, so they have
    to be present -- empty is fine for an uncertified receiver. Sending a bare
    AuthResponse (or an empty field 2) leaves those required fields unset and
    strict parsers reject the whole message: VLC hangs up immediately after the
    challenge and the device looks "found but not castable".
    """
    auth_response = _field_bytes(1, signature) + _field_bytes(2, certificate)
    for ca in intermediates:
        auth_response += _field_bytes(3, ca)
    if crl:
        auth_response += _field_bytes(7, crl)
    return _field_bytes(2, auth_response)


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


class _TlsHTTPServer(ThreadingHTTPServer):
    """ThreadingHTTPServer that wraps each accepted connection in TLS.

    Per *connection*, deliberately, not the listening socket: wrapping the
    listener runs the handshake inside ``accept()``, so a client that connects
    without TLS (a port scan, a stray HTTPS probe, a browser hitting the wrong
    port) raises SSLError there -- and ``SSLError`` is an ``OSError``, the exact
    mistake that once killed the Cast accept loop for good while mDNS kept
    advertising the device (AGENTS.md section 4.1). Here a bad handshake fails
    inside ``get_request``, which socketserver swallows, and the listener
    survives.
    """

    daemon_threads = True
    allow_reuse_address = True
    ssl_context = None

    def get_request(self):
        sock, addr = self.socket.accept()
        return self.ssl_context.wrap_socket(sock, server_side=True), addr

    def handle_error(self, request, client_address):
        # A dropped or non-TLS client is routine on an open port; the default
        # prints a full traceback to stderr for every one of them.
        logger.debug("Cast HTTPS setup connection from %s failed",
                     client_address)


class ChromecastProtocol(Protocol):
    """Act as a Chromecast receiver. Reuses the mpv renderer for playback."""

    uses_ssdp = False  # we advertise over mDNS instead

    def __init__(self):
        super().__init__()
        self._advertiser = None
        self._listen_socket = None
        self._ssl_context = None
        self._setup_server = None
        self._stop_event = threading.Event()
        self._connections = []
        self._senders = {}          # source_id -> socket (for broadcasts)
        self._session_id = None
        self._media = None          # current Cast media descriptor
        self._media_session_id = 1
        self._position = 0.0
        # Receiver-level volume, as reported back in RECEIVER_STATUS and
        # MEDIA_STATUS. Real state, not a constant: senders compute the next
        # volume from what we report (pychromecast does level +/- 0.1), so
        # answering 1.0 forever freezes their volume slider.
        self._volume = 1.0
        self._muted = False
        # Latest transport observation for the *current* media, filled in from
        # the player's set_state_* fan-out. Dialect: 'PLAYING' / 'PAUSED' / None
        # for unknown; ``_idle_reason`` is Cast's idleReason string.
        self._observed_transport = None
        self._idle_reason = None
        self._transport_id = "MacastTransport"
        self._lock = threading.Lock()
        # Bumped on every LOAD so a stale playback watcher stops reporting.
        self._watch_generation = 0
        self.cast_port = CAST_PORT   # actual bound port (may differ)
        self.setup_port = SETUP_PORT
        self._https_setup_server = None
        self.https_setup_port = None  # set only when 8443 could be bound
        self._cert_path = os.path.join(SETTING_DIR, "macast_cast.crt")
        self._key_path = os.path.join(SETTING_DIR, "macast_cast.key")

    # -- lifecycle ----------------------------------------------------------

    def start(self):
        self._stop_event.clear()
        self._ensure_cert()
        # Bind listeners first so mDNS can advertise the ports we really bound.
        self._start_tls_server()
        self._start_setup_server()
        self._start_https_setup_server()
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
        self._connections = []
        self._senders = {}
        # A restart must be a clean slate: without this the phantom session
        # from the previous cast survived the plugin being toggled off and on.
        with self._lock:
            self._session_id = None
            self._media = None
            self._watch_generation += 1
        if self._listen_socket is not None:
            # Closing the plain listener reliably wakes the blocked accept();
            # closing an SSL-wrapped listener did not, which left a thread
            # spinning up on a socket nobody could reach.
            try:
                self._listen_socket.close()
            except Exception:
                pass
            self._listen_socket = None
        if self._setup_server is not None:
            self._setup_server.shutdown()
            self._setup_server.server_close()
            self._setup_server = None
        if self._https_setup_server is not None:
            self._https_setup_server.shutdown()
            self._https_setup_server.server_close()
            self._https_setup_server = None
            self.https_setup_port = None
        logger.info("ChromecastProtocol stopped")

    def _advertise(self):
        self._advertiser = MDNSAdvertiser()
        name = "{}._googlecast._tcp.local.".format(Setting.get_friendly_name())
        props = {
            "id": Setting.get_usn(),
            "fn": Setting.get_friendly_name(),
            # VLC reads this as a bitmask: 0x01 = can play video,
            # 0x04 = can play audio (modules/services_discovery/microdns.c).
            # Without 0x01 VLC adds `no-video` to its cast chain and never
            # sends a video ES at all, which looks exactly like a receiver bug.
            # 5 = video | audio, the value a real Chromecast advertises.
            "ca": "5",        # cast capable: video + audio
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
            raw.listen(8)
            # Listen in the clear and wrap each *connection* instead of wrapping
            # the listener. Wrapping the listener made ``accept()`` run the TLS
            # handshake inline, so a single client that aborted mid-handshake
            # raised SSLError -- and SSLError is an OSError, which the accept
            # loop treated as "listener is gone" and exited. Macast then stayed
            # advertised on mDNS while nothing answered on 8009: still
            # discoverable, no longer castable.
            self._ssl_context = ctx
            self._listen_socket = raw
            self.cast_port = port
            if port != CAST_PORT:
                logger.warning("Cast port %d in use, using %d instead", CAST_PORT, port)
            t = threading.Thread(target=self._accept_loop, args=(raw,),
                                 name="CAST_TLS", daemon=True)
            t.start()
            return
        logger.error("Cast TLS server failed: no free port in range")

    def _accept_loop(self, server):
        """Accept connections until the listener is closed or stop is requested.

        ``server`` is passed in rather than read from ``self`` so a stale thread
        left over from a previous start() can never end up accepting on the
        current listener.
        """
        while not self._stop_event.is_set():
            try:
                conn, addr = server.accept()
            except OSError as e:
                if self._stop_event.is_set() or server.fileno() < 0:
                    break
                # Transient: a full backlog, an aborted connect, EINTR. Never
                # give up the listener for it.
                logger.warning("Cast accept failed, still listening: %s", e)
                time.sleep(0.05)
                continue
            try:
                # Bound the handshake only; the session itself must not time
                # out (senders only PING every ~5s and may idle for longer).
                conn.settimeout(10)
                tls = self._ssl_context.wrap_socket(conn, server_side=True)
                tls.settimeout(None)
            except Exception as e:
                # A bad handshake is the client's problem, not ours; the next
                # sender must still be able to connect.
                logger.debug("Cast handshake failed from %s: %s", addr, e)
                try:
                    conn.close()
                except Exception:
                    pass
                continue
            logger.info("Cast connection from %s", addr)
            self._connections.append(tls)
            t = threading.Thread(
                target=self._handle_client, args=(tls,), name="CAST_CLIENT", daemon=True
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
            # A sender that dies without a CLOSE (the phone sleeping, the app
            # being killed, Wi-Fi dropping -- what VLC actually does) must still
            # be forgotten, otherwise the app stays "running" for the next one.
            self._forget_sender_socket(sock)

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
        # Routing is by namespace only, so destination_id is not consulted:
        # senders CONNECT to our transportId but reply to whatever source_id we
        # used, which is why the reply source is not echoed from the request.
        src = msg["source_id"]
        if ns == NS_DEVICEAUTH:
            # DeviceAuthMessage{challenge} -> DeviceAuthMessage{response}
            # (uncertified receiver: empty signature / certificate).
            logger.info("Cast device-auth challenge from %s -> "
                        "uncertified AuthResponse", src)
            self._send(sock, "receiver-0", src, ns,
                       encode_auth_response(), binary=True)
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
        else:
            # Not answered, and deliberately so: this receiver advertises only
            # the namespaces it implements, so a message here belongs to a
            # feature we never claimed, and an unexpected reply is what wedges a
            # strict sender's state machine. Logging it is the cheap half -- it
            # is how we would ever learn that some client really does need an
            # INVALID_REQUEST instead of silence.
            logger.debug("Unhandled Cast namespace %r (type %r) from %s",
                         ns, msg_type, src)

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
            if not self._senders:
                self._clear_session("last sender closed the connection")

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
        elif msg_type in ("STOP", "QUIT_APP"):
            # Receiver-namespace STOP is what pychromecast's quit_app() sends;
            # QUIT_APP is what the Google Cast SDK sends. Both mean "close the
            # app", and the media ledger has to go with the session: answering
            # GET_STATUS afterwards used to report PLAYING for a cast the sender
            # had already closed, because only the ``_session_id`` was cleared.
            with self._lock:
                self._session_id = None
                self._media = None
                self._observed_transport = None
                self._idle_reason = None
                self._watch_generation += 1
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
            self._apply_volume(data.get("volume") or {})
            # A real receiver answers SET_VOLUME with the updated
            # RECEIVER_STATUS. pychromecast's set_volume()/set_volume_muted()
            # sit in WaitResponse(REQUEST_TIMEOUT=10.0) until that reply shows
            # up, matched by requestId -- staying silent made every volume
            # change stall for 10 seconds and then raise RequestTimeout on the
            # sender (mkchromecast's volume keys, Home Assistant's slider).
            self._send_receiver_status(sock, src, data.get("requestId"))
        else:
            # Ditto for the receiver namespace (SET_ACTIVE_INPUT_STATE and the
            # GET_APP_AVAILABILITY variants we do not answer): recorded rather
            # than answered, to avoid inventing a reply.
            logger.debug("Unhandled Cast receiver message %r from %s",
                         msg_type, src)

    def _apply_volume(self, vol):
        """Apply a Cast ``volume`` dict to our state and to the player.

        Everything here comes off the wire, so it is coerced defensively: an
        exception raised in a handler escapes to ``_handle_client``, which
        drops the whole sender connection.
        """
        if not isinstance(vol, dict):
            return
        if "level" in vol:
            try:
                level = min(max(float(vol["level"]), 0.0), 1.0)
            except (TypeError, ValueError):
                logger.debug("Ignoring malformed Cast volume level: %r",
                             vol.get("level"))
            else:
                with self._lock:
                    self._volume = level
                try:
                    self.renderer.set_media_volume(int(round(level * 100)))
                except Exception as e:
                    logger.debug("Cast volume apply failed: %s", e)
        if "muted" in vol:
            muted = bool(vol["muted"])
            with self._lock:
                self._muted = muted
            try:
                self.renderer.set_media_mute(muted)
            except Exception as e:
                logger.debug("Cast mute apply failed: %s", e)

    def _volume_state(self):
        """The ``status.volume`` payload, as real state."""
        with self._lock:
            return {"level": self._volume, "muted": self._muted}

    # -- playback observations ----------------------------------------------
    #
    # ``ProtocolGroup`` fans every ``set_state_*`` the player reports out to all
    # of its children, so these overrides are how a Chromecast-only setup (DLNA
    # switched off) learns what mpv is really doing. The DLNA ledger is *not*
    # usable for this: with no DLNA child in the group its base-class stub
    # answers ``STOPPED`` no matter what is on screen.
    #
    # They deliberately never touch the network. A GET_STATUS is answered from
    # this, but nothing is pushed unsolicited: an unexpected status message is
    # what wedges VLC's state machine (docs/Cast-AirPlay-Testing.md), so the
    # only safe way to tell a sender that playback ended is to answer honestly
    # the next time it asks.
    #
    # ``Protocol`` declares all of these as no-ops, so overriding is additive.

    def set_state_eof(self):
        self._note_stopped("FINISHED")

    def set_state_transport_error(self):
        self._note_stopped("ERROR")

    def set_state_stop(self):
        # mpv fires ``idle`` immediately after a normal end-of-file, so this
        # must not overwrite a more specific reason already recorded.
        self._note_stopped("CANCELLED")

    def set_state_pause(self):
        with self._lock:
            if self._media is not None:
                self._observed_transport = "PAUSED"

    def set_state_play(self):
        with self._lock:
            if self._media is not None:
                self._observed_transport = "PLAYING"
                self._idle_reason = None

    def _note_stopped(self, reason):
        """Record why playback stopped.

        A LOAD replaces the media, and mpv reports the end-file of the file it
        just dropped *after* the new one is in place. Accepting that as the new
        media's outcome labelled a video that then played to completion as
        CANCELLED -- found by driving the receiver with a real sender stack
        (scripts/cast_conformance.py), not by any stubbed test.

        So a terminal stop is only believed once the current media has reported
        playback at least once. An ERROR is accepted unconditionally: it is the
        signal worth surfacing, and a replaced file ends with reason=stop rather
        than error.
        """
        with self._lock:
            if self._media is None:
                return
            if reason != "ERROR" and self._observed_transport is None:
                logger.debug("Ignoring %s for a media that never started "
                             "playing (superseded LOAD)", reason)
                return
            if self._idle_reason is None:
                self._idle_reason = reason

    def _observed_state(self):
        """What a GET_STATUS should answer, from real observations only.

        ``None`` means "we have not observed anything since the LOAD", which
        the caller resolves to the optimistic ``PLAYING`` -- the previous
        behaviour, kept as the fallback rather than guessing IDLE and turning a
        working cast into a stopped one.
        """
        with self._lock:
            if self._media is None:
                return "IDLE", self._idle_reason
            if self._idle_reason:
                return "IDLE", self._idle_reason
            return (self._observed_transport or "PLAYING"), None

    def _send_receiver_status(self, sock, src, request_id):
        with self._lock:
            session = self._session_id
            volume = {"level": self._volume, "muted": self._muted}
        apps = []
        if session:
            apps.append(
                {
                    "appId": DEFAULT_MEDIA_APP_ID,
                    # A real device reports "Default Media Receiver" for
                    # CC1AD845, and senders key off it: mkchromecast's
                    # --hijack treats any other name as "someone stole my
                    # receiver" and re-issues LOAD every 5 seconds, which is an
                    # endless restart loop against us. See cast.py:410-447.
                    "displayName": DEFAULT_MEDIA_APP_NAME,
                    "sessionId": session,
                    "statusText": DEFAULT_MEDIA_APP_NAME,
                    "transportId": self._transport_id,
                    # Senders establish a virtual connection to transportId
                    # and only then speak the media namespace.
                    "namespaces": _app_namespaces(),
                }
            )
        status = {
            "requestId": request_id,
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": apps,
                "isActiveInput": True,
                # pychromecast's CastStatus.is_stand_by defaults to True for a
                # non-audio cast type when the key is missing (_parse_status),
                # which is what makes a working receiver show up as "standby".
                "isStandBy": False,
                "volume": volume,
            },
        }
        self._send(sock, "receiver-0", src, NS_RECEIVER, json.dumps(status))

    # -- session teardown ---------------------------------------------------

    def _forget_sender_socket(self, sock):
        """Drop every sender that was riding on ``sock`` and close the app.

        Called when a client thread ends. The Cast receiver namespace has no
        "sender went away" event of its own, so the last connection closing is
        what ends the app on a real Chromecast -- and senders rely on it:
        VLC's controller only leaves its Connecting state on the *next*
        RECEIVER_STATUS it receives (chromecast_ctrl.cpp, processReceiverMessage).
        """
        with self._lock:
            gone = [src for src, s in self._senders.items() if s is sock]
            for src in gone:
                self._senders.pop(src, None)
            last_one_out = not self._senders
        if gone:
            logger.info("Cast sender dropped: %s", ", ".join(gone))
        if last_one_out:
            self._clear_session("sender connection ended")

    def _clear_session(self, reason):
        """Forget the media app and tell the senders it closed.

        Regression: ``_session_id`` was only cleared by an explicit STOP, so
        after a sender disconnected the receiver kept answering GET_STATUS with
        ``applications: [{...}]`` and a sessionId nobody owned while nothing was
        playing. VLC reads that as "Media receiver application was already
        running", skips its LAUNCH, connects to the dead transportId and adopts
        the stale session; the phone then needed an explicit "stop casting" (or
        a plugin restart) before it would cast again. A receiver must report
        ``applications: []`` once the app is gone.
        """
        with self._lock:
            if self._session_id is None and self._media is None:
                return False
            self._session_id = None
            self._media = None
            self._observed_transport = None
            self._idle_reason = None
            # Stop a playback watcher from reporting on the session we just
            # dropped (it re-reads the generation before every status).
            self._watch_generation += 1
            senders = list(self._senders.items())
        logger.info("Cast session cleared (%s)", reason)
        for src, sock in senders:
            try:
                self._send_receiver_status(sock, src, None)
            except Exception as e:
                logger.debug("Cast status broadcast failed for %s: %s", src, e)
        return True

    def _on_media(self, sock, src, data, msg_type):
        """Media playback control (media namespace)."""
        with self._lock:
            self._media_session_id = data.get("mediaSessionId",
                                              self._media_session_id)
        if msg_type == "LOAD":
            media = data.get("media", {})
            url = media.get("contentId") or media.get("contentUrl")
            # Log what the sender says it is about to serve. This is what makes
            # a "sound but no picture" report diagnosable from the log alone:
            # VLC announces `audio/x-matroska` when it has decided to send no
            # video at all, which is indistinguishable from a receiver bug
            # unless you record it (see docs/Cast-AirPlay-Testing.md 10.6).
            logger.info(
                "Cast LOAD url=%s contentType=%s streamType=%s duration=%s tracks=%d",
                url, media.get("contentType"), media.get("streamType"),
                data.get("duration") or media.get("duration"),
                len(media.get("tracks") or []))
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
                # A new LOAD starts a new observation window: whatever the
                # previous media ended with must not leak into this one.
                self._observed_transport = None
                self._idle_reason = None
            self.renderer.set_media_url(url, start=str(int(self._position)))
            self._send_media_status(sock, src, "BUFFERING", data.get("requestId"))
            # PLAYING is reported once the player confirms it, not blindly.
            self._start_playback_watch(sock, src, data.get("requestId"))
        elif msg_type == "PLAY":
            self.renderer.set_media_resume()
            self._send_media_status(sock, src, "PLAYING", data.get("requestId"))
        elif msg_type == "PAUSE":
            self.renderer.set_media_pause()
            self._send_media_status(sock, src, "PAUSED", data.get("requestId"))
        elif msg_type == "STOP":
            self.renderer.set_media_stop()
            # Closes the app for every connected sender, not just this one.
            self._clear_session("sender stopped playback")
            with self._lock:
                self._idle_reason = "CANCELLED"
            self._send_media_status(sock, src, "IDLE", data.get("requestId"))
        elif msg_type == "SEEK":
            with self._lock:
                self._position = float(data.get("currentTime", 0) or 0)
                position = self._position
            self.renderer.set_media_position(str(position))
            self._send_media_status(sock, src, "PLAYING", data.get("requestId"))
        elif msg_type == "GET_STATUS":
            self._send_media_status(sock, src, None, data.get("requestId"))
        else:
            # QUEUE_*, EDIT_TRACKS_INFO, SET_PLAYBACK_RATE and friends land
            # here. We do not advertise them in supportedMediaCommands, so a
            # compliant sender will not send them; answering anyway would be a
            # reply to a request we never accepted. Recorded, not answered.
            logger.debug("Unhandled Cast media message %r from %s", msg_type, src)

    #: How long to wait for the player to confirm playback before falling back
    #: to the optimistic "PLAYING" reply. Long enough for a network stream to
    #: start, short enough that a sender does not consider the load stalled.
    LOAD_GRACE_SECONDS = 8.0

    def _renderer_transport(self):
        """(state, status) from the player, or ('', '') if it cannot say.

        The transport state is DLNA-flavoured, so with DLNA switched off the
        group may answer ``STOPPED``/``OK`` regardless of what mpv is doing.
        That is why a missing answer degrades to the old optimistic reply
        rather than to a false failure.
        """
        try:
            return (self.renderer.get_state_transport_state(),
                    self.renderer.get_state_transport_status())
        except Exception as e:
            logger.debug("Renderer transport state unavailable: %s", e)
            return '', ''

    def _start_playback_watch(self, sock, src, request_id):
        with self._lock:
            self._watch_generation += 1
            generation = self._watch_generation
        threading.Thread(target=self._watch_playback,
                         args=(sock, src, request_id, generation),
                         name="CAST_STATUS", daemon=True).start()

    def _watch_playback(self, sock, src, request_id, generation):
        """Push PLAYING once playback really starts -- or LOAD_FAILED.

        Replying "PLAYING" the moment LOAD arrives was a lie: mpv has not even
        opened the URL yet, so a stream that never plays still looked healthy
        and the sender had no reason to retry. It also ran off the client
        thread on purpose -- blocking that reader for seconds would stop us
        answering the sender's heartbeats, which is how a receiver gets
        declared dead mid-load.
        """
        deadline = time.time() + self.LOAD_GRACE_SECONDS
        reported = None
        while time.time() < deadline:
            with self._lock:
                if generation != self._watch_generation:
                    return  # superseded by a newer LOAD
            if self._stop_event.wait(0.2):
                return
            state, status = self._renderer_transport()
            if status == 'ERROR_OCCURRED':
                reported = 'LOAD_FAILED'
                break
            if state in ('PLAYING', 'PAUSED_PLAYBACK'):
                reported = 'PAUSED' if state == 'PAUSED_PLAYBACK' else 'PLAYING'
                break
        if reported is None:
            reported = 'PLAYING'
        try:
            if reported == 'LOAD_FAILED':
                logger.warning("Cast load failed: the player reported an error")
                self._send(sock, self._transport_id, src, NS_MEDIA, json.dumps({
                    "type": "LOAD_FAILED",
                    "requestId": request_id,
                }))
            else:
                self._send_media_status(sock, src, reported, request_id)
        except Exception as e:
            logger.debug("Cast status push failed: %s", e)

    def _send_media_status(self, sock, src, player_state, request_id):
        with self._lock:
            session = self._session_id
            media = self._media
            position = self._position
            media_session_id = self._media_session_id
            volume = {"level": self._volume, "muted": self._muted}
        if player_state is None:
            # A GET_STATUS must answer with what is really happening. It used to
            # answer PLAYING for as long as a media descriptor existed, so a
            # video that had already finished still looked healthy to the
            # sender -- and a sender that never learns otherwise never offers to
            # re-play it. See ``_observed_state``.
            state, idle_reason = self._observed_state()
        else:
            state = player_state
            idle_reason = None
            if state == "IDLE":
                with self._lock:
                    idle_reason = self._idle_reason
        entry = {
            "mediaSessionId": media_session_id,
            "playbackRate": 1,
            "playerState": state,
            "currentTime": position,
            "supportedMediaCommands": 15,
            "volume": volume,
            "sessionId": session or _random_session(),
            "media": media,
        }
        if idle_reason:
            # pychromecast clears its idle_reason whenever the key is absent, so
            # only the terminal states carry it (media.py, MediaStatus.update).
            entry["idleReason"] = idle_reason
        status = {
            "requestId": request_id,
            "type": "MEDIA_STATUS",
            "status": [entry],
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

    def _start_https_setup_server(self):
        """Serve ``eureka_info`` over TLS on 8443, if we can get the port.

        Why bother: pychromecast's ``get_cast_type`` (the mDNS discovery path)
        asks ``https://host:8443`` and has no plain-HTTP fallback, so without
        this listener every discovery logs `Failed to determine cast type` and
        the ``device_info`` branch -- ``manufacturer`` in particular -- never
        runs. The plain 8008 listener is what the host-scanning path uses and is
        unaffected either way.

        Only 8443 will do; see ``HTTPS_SETUP_PORT``. If something else holds it
        (Docker/OrbStack, dev servers, router UIs all like this port) we log and
        carry on: losing a cosmetic field is not worth fighting over a port, and
        Macast must never take a listener away from another program or fail to
        start because of it.
        """
        if not (self._cert_path and os.path.exists(self._cert_path)
                and os.path.exists(self._key_path)):
            logger.info("No TLS certificate available, skipping the Cast HTTPS "
                        "setup server on %d", HTTPS_SETUP_PORT)
            return
        try:
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(certfile=self._cert_path, keyfile=self._key_path)
        except Exception as e:
            logger.warning("Cast HTTPS setup server: unusable certificate (%s)", e)
            return
        try:
            server = _TlsHTTPServer(("0.0.0.0", HTTPS_SETUP_PORT),
                                    self._make_setup_handler())
        except OSError as e:
            # Expected on machines where another program owns 8443. Not a
            # fallback case: the client hardcodes the port, so a different
            # listener would never be contacted.
            logger.info("Cast HTTPS setup port %d unavailable (%s); discovery "
                        "still works over %d", HTTPS_SETUP_PORT, e, SETUP_PORT)
            return
        server.ssl_context = ctx
        self._https_setup_server = server
        self.https_setup_port = HTTPS_SETUP_PORT
        t = threading.Thread(target=server.serve_forever,
                             name="CAST_SETUP_TLS", daemon=True)
        t.start()
        logger.info("Cast HTTPS setup server on %d", HTTPS_SETUP_PORT)

    def _make_setup_handler(self):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            @staticmethod
            def _eureka_info(path):
                """Device description in the shape a real Chromecast returns.

                Senders probe this (over 8008, and some over 8443) both to
                label the device and to decide what it can play. Two details
                matter: ``device_info`` is *nested* rather than flattened, and
                ``?params=a,b`` filters the top-level keys -- a request for
                ``params=device_info,name`` must not be answered with the whole
                document or strict clients reject the shape.
                """
                from urllib.parse import parse_qs, urlparse

                # A bare UUID, not "uuid:<...>". pychromecast parses this with
                # ``UUID(udn.replace("-", ""))`` and catches the ValueError,
                # returning None for the whole device_info (dial.py). A prefixed
                # value therefore made Macast invisible to the host-scanning
                # fallback used when mDNS is unavailable, even though the mDNS
                # TXT ``id`` -- which is already bare -- was fine.
                udn = Setting.get_usn()
                name = Setting.get_friendly_name()
                device_info = {
                    "manufacturer": "Macast",
                    "model_name": "Macast",
                    "product_name": "macast",
                    "friendly_name": name,
                    "ssdp_udn": udn,
                    "mac_address": "00:00:00:00:00:00",
                    "capabilities": {
                        "display_supported": True,
                        "audio_supported": True,
                        "video_out": True,
                        "audio_out": True,
                        # Must be explicit. pychromecast reads
                        # ``capabilities.get("multizone_supported", True)``
                        # whenever ``device_info`` is present (dial.py), so
                        # omitting the key claims multi-room support and makes
                        # every discovery probe
                        # https://host:8443/...?params=multizone, which we do
                        # not serve -- one failed request per poll, forever.
                        "multizone_supported": False,
                    },
                }
                full = {
                    "name": name,
                    "device_info": device_info,
                    "ssdp_udn": udn,
                    "version": Setting.version,
                    "build_version": Setting.version,
                    "connected": True,
                    "settings": {"control_notifications": False},
                }
                params = parse_qs(urlparse(path).query).get("params")
                if params:
                    wanted = [p.strip() for p in params[0].split(",") if p.strip()]
                    if wanted:
                        return {k: v for k, v in full.items() if k in wanted}
                return full

            def do_GET(self):
                if self.path.startswith("/setup/eureka_info"):
                    info = self._eureka_info(self.path)
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
