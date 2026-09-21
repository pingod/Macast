# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# AirPlay receiver protocol (video / URL-cast path).
#
# Makes Macast appear as an AirPlay target so an iPhone / iPad / macOS can "AirPlay"
# a video to the computer. AirPlay also supports screen mirroring and audio-only
# (RAOP), but those require real-time encode / ALAC decode and are OUT OF SCOPE
# here. We implement the *video URL-cast* flow, which is the same "sender gives a
# URL, receiver plays it" model as DLNA/Chromecast:
#
#   mDNS  : _airplay._tcp.local  (port 7000)   -- sender discovery
#   RTSP  : port 7000            (OPTIONS / ANNOUNCE / SETUP / PLAY / ...)
#           ANNOUNCE carries Content-Location (the media URL) -> mpv
#

import base64
import logging
import random
import socket
import threading
import time

from .discovery import MDNSAdvertiser
from .protocol import Protocol
from .utils import Setting

logger = logging.getLogger("AirPlay")

AIRPLAY_PORT = 7000
AIRPLAY_SERVICE = "_airplay._tcp.local."
# macOS ships its own AirPlay receiver that occupies 7000; try a few
# consecutive ports before giving up, and advertise whichever one we bound.
PORT_FALLBACK_RANGE = 20
# AirPlay "features" bitmask. Only advertise what we actually implement --
# claiming a bit makes iOS offer a feature that would then fail.
#   0x01  Video
#   0x02  Photo
#   0x08  VideoVolumeControl
#   0x10  VideoHTTPLiveStreams
#   0x20  Slideshow
# Deliberately NOT advertised: VideoFairPlay (0x04, no DRM decryption),
# screen mirroring, AirPlay audio/RAOP, and AirPlay 2 multi-room.
FEATURES = 0x01 | 0x02 | 0x08 | 0x10 | 0x20

#: The RTSP methods we actually implement. One tuple, used both to advertise
#: ``Public``/``Allow`` and to decide whether a request is answerable, so the
#: two can never drift apart.
SUPPORTED_METHODS = (
    "OPTIONS", "ANNOUNCE", "SETUP", "RECORD", "PLAY", "PAUSE", "FLUSH",
    "STOP", "TEARDOWN", "SET_PARAMETER", "GET_PARAMETER",
)

#: RTSP status codes -> reason phrase (RFC 2326 section 12). The old code only
#: ever produced ``200 OK`` or the literal word ``Error``, which is not a valid
#: status line for any other code.
REASON_PHRASES = {
    200: "OK",
    400: "Bad Request",
    404: "Not Found",
    405: "Method Not Allowed",
    454: "Session Not Found",
    455: "Method Not Valid In This State",
    500: "Internal Server Error",
    501: "Not Implemented",
}


class _BadRequest(Exception):
    """Raised while parsing, so the caller can answer 400 instead of hanging up."""


def get_header(headers, name, default=None):
    """Case-insensitive header lookup.

    RTSP header names are case-insensitive, but the parser used to build a
    plain dict verbatim from the wire. A client sending ``cseq:`` (or
    ``content-location:``) therefore missed every lookup and the reply carried
    a fabricated ``CSeq: 1``, leaving the client's sequence numbers permanently
    out of step. Plain dicts are supported too, so callers that build headers by
    hand keep working.
    """
    if not headers:
        return default
    try:
        if name in headers:
            return headers[name]
    except TypeError:  # not a mapping at all
        return default
    wanted = name.lower()
    for key, value in headers.items():
        if isinstance(key, str) and key.lower() == wanted:
            return value
    return default



def _random_mac():
    """Generate a stable, locally-administered MAC for the device-id."""
    mac = [0x02, 0x00, 0x00, random.randint(0, 255), random.randint(0, 255), random.randint(0, 255)]
    return ":".join("{:02X}".format(b) for b in mac)


class AirPlayProtocol(Protocol):
    """Act as an AirPlay video receiver. Reuses the mpv renderer for playback."""

    uses_ssdp = False

    def __init__(self):
        super().__init__()
        self._advertiser = None
        self._rtsp_server = None
        self._stop_event = threading.Event()
        self._lock = threading.Lock()
        self._current_url = None
        self._device_id = _random_mac()
        self._session = 0
        self.rtsp_port = AIRPLAY_PORT  # actual bound port (may differ)

    # -- lifecycle ----------------------------------------------------------

    def start(self):
        self._stop_event.clear()
        # Bind the RTSP listener *before* advertising: macOS ships its own
        # AirPlay receiver which already owns port 7000, so we may have to
        # fall back to another port and advertise that one over mDNS.
        self._start_rtsp_server()
        self._advertise()
        logger.info("AirPlayProtocol started on port %s", self.rtsp_port)

    def stop(self):
        self._stop_event.set()
        if self._advertiser is not None:
            self._advertiser.close()
            self._advertiser = None
        if self._rtsp_server is not None:
            try:
                self._rtsp_server.shutdown()
                self._rtsp_server.server_close()
            except Exception:
                pass
            self._rtsp_server = None
        logger.info("AirPlayProtocol stopped")

    def _advertise(self):
        self._advertiser = MDNSAdvertiser()
        name = "{}._airplay._tcp.local.".format(Setting.get_friendly_name())
        props = {
            "deviceid": self._device_id,
            "features": "0x{:X}".format(FEATURES),
            "model": "Macast",
            "name": Setting.get_friendly_name(),
            "srcvers": "320.20",
            "vv": "2",
            "pi": "2e3883fe",
            "flags": "0x4",
            "pk": base64.b64encode(b"macast-airplay-public-key").decode("ascii"),
        }
        self._advertiser.advertise(AIRPLAY_SERVICE, name, self.rtsp_port, props)

    # -- RTSP control channel ----------------------------------------------

    def _start_rtsp_server(self):
        handler = self._make_rtsp_handler()
        for port in range(AIRPLAY_PORT, AIRPLAY_PORT + PORT_FALLBACK_RANGE):
            try:
                server = _RtspServer(("0.0.0.0", port), handler)
                server.protocol = self
                self._rtsp_server = server
                self.rtsp_port = port
                t = threading.Thread(target=server.serve_forever,
                                     name="AIRPLAY_RSTP", daemon=True)
                t.start()
                if port != AIRPLAY_PORT:
                    logger.warning(
                        "AirPlay port %d already in use (macOS AirPlay Receiver?), "
                        "using %d instead", AIRPLAY_PORT, port)
                return
            except OSError as e:
                logger.debug("AirPlay bind port %d failed: %s", port, e)
                continue
        logger.error("AirPlay RTSP server failed: no free port in range")

    def _make_rtsp_handler(self):
        protocol = self

        class Handler(_RtspHandlerBase):
            def handle_request(self, method, uri, headers, body):
                return protocol._handle_rtsp(method, headers, body, uri=uri)

        return Handler

    def _handle_rtsp(self, method, headers, body, uri="/"):
        base = {"Server": Setting.get_server_info()}
        # Echo the client's CSeq back if it sent one -- and only if. Inventing
        # ``CSeq: 1`` for a request that arrived without one desynchronises the
        # client's sequence bookkeeping for the rest of the connection.
        cseq = get_header(headers, "CSeq")
        if cseq is not None:
            base["CSeq"] = cseq
        # Some clients GET /info over the same socket to read device capability.
        if method == "GET" and uri.startswith("/info"):
            base["Content-Type"] = "text/x-apple-plist+xml"
            return 200, base, self._info_plist()
        if method == "OPTIONS":
            base["Public"] = ", ".join(SUPPORTED_METHODS)
            # In AirPlay the *client* sends Apple-Challenge and the receiver
            # answers with a signed Apple-Response. We have no signing key, so
            # we deliberately do not promise one (see module docstring).
            if get_header(headers, "Apple-Challenge"):
                logger.info(
                    "Client sent Apple-Challenge; signature auth is not "
                    "implemented, continuing without Apple-Response")
            return 200, base, ""
        if method == "FLUSH":
            base["Session"] = "{}".format(max(self._session, 1))
            return 200, base, ""
        if method == "ANNOUNCE":
            url = self._parse_content_location(headers, body)
            if not url:
                # Answering 200 here promised a play that could never happen,
                # and the PLAY that followed reported Range/RTP-Info for a
                # stream that did not exist.
                logger.warning("AirPlay ANNOUNCE carried no Content-Location; "
                               "refusing rather than pretending to load")
                return 400, base, ""
            with self._lock:
                self._current_url = url
            logger.info("AirPlay ANNOUNCE url=%s", url)
            return 200, base, ""
        if method == "SETUP":
            self._session += 1
            base["Session"] = "{}".format(self._session)
            base["Transport"] = "RTP/AVP/TCP;unicast;interleaved=0-1"
            return 200, base, ""
        if method == "RECORD":
            base["Session"] = "{}".format(max(self._session, 1))
            return 200, base, ""
        if method == "PLAY":
            stale = self._stale_session(headers)
            if stale is not None:
                return stale, base, ""
            with self._lock:
                url = self._current_url
            if not url:
                # Nothing was announced: 455 rather than a 200 that claims
                # playback started.
                logger.warning("AirPlay PLAY without a prior ANNOUNCE url")
                return 455, base, ""
            self.renderer.set_media_url(url)
            base["Session"] = "{}".format(max(self._session, 1))
            base["Range"] = "npt=now-"
            base["RTP-Info"] = "seq=0;rtptime=0"
            return 200, base, ""
        if method in ("PAUSE", "SET_PARAMETER"):
            stale = self._stale_session(headers)
            if stale is not None:
                return stale, base, ""
            if method == "PAUSE":
                self.renderer.set_media_pause()
            elif "volume" in body.lower():
                # SET_PARAMETER with 'volume: <0..100>' -> renderer volume
                try:
                    vol = float(body.split("volume:")[-1].strip()) * 100
                    self.renderer.set_media_volume(int(vol))
                except Exception:
                    pass
            base["Session"] = "{}".format(max(self._session, 1))
            return 200, base, ""
        if method in ("STOP", "TEARDOWN"):
            self.renderer.set_media_stop()
            with self._lock:
                self._current_url = None
            return 200, base, ""
        if method == "GET_PARAMETER":
            return 200, base, "volume: 100\r\n"
        # Anything else: say so instead of answering 200 to a request we never
        # performed. ``Allow`` tells the client what it may ask for.
        logger.info("AirPlay unsupported RTSP method %r from a client", method)
        base["Allow"] = ", ".join(SUPPORTED_METHODS)
        return 501, base, ""

    def _stale_session(self, headers):
        """454 if the client names a session we do not own, else None.

        A ``Session`` value may carry parameters (``1;timeout=60``); only the id
        before the semicolon identifies the session, and comparing the raw
        string rejected legal requests. A request that names no session at all
        is accepted: before SETUP there is nothing to compare against, and being
        strict there would break clients that PLAY without one.
        """
        if self._session <= 0:
            return None
        sent = get_header(headers, "Session")
        if sent is None:
            return None
        session_id = sent.split(";", 1)[0].strip()
        if not session_id:
            return None
        if session_id != str(self._session):
            logger.info("AirPlay stale session %r (ours is %d)",
                        session_id, self._session)
            return 454
        return None

    def _info_plist(self):
        """Device capability plist served on GET /info."""
        return (
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
            '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
            '<plist version="1.0"><dict>'
            '<key>deviceID</key><string>{}</string>'
            '<key>features</key><integer>{}</integer>'
            '<key>model</key><string>Macast</string>'
            '<key>name</key><string>{}</string>'
            '<key>protocolVersion</key><string>1.0</string>'
            '<key>sourceVersion</key><string>320.20</string>'
            '</dict></plist>'
        ).format(self._device_id, FEATURES, Setting.get_friendly_name())

    @staticmethod
    def _parse_content_location(headers, body):
        # Header form:  Content-Location: <url>
        cl = get_header(headers, "Content-Location")
        if cl:
            return cl.strip()
        # SDP body form: a=content-location:<url>
        for line in body.splitlines():
            line = line.strip()
            if line.lower().startswith("a=content-location:"):
                return line.split(":", 1)[1].strip()
        return None


# ---------------------------------------------------------------------------
# Minimal RTSP server (text protocol, like HTTP but with RTSP/1.0)
# ---------------------------------------------------------------------------


class _RtspHandlerBase:
    """A tiny RTSP request handler (one connection per instance)."""

    #: How long an idle control connection may sit before we reclaim it. RTSP
    #: clients keep the socket open between commands, so this must be generous;
    #: its purpose is only to stop a sender that vanished without a FIN (Wi-Fi
    #: drop, app killed) from parking a thread and an fd forever.
    IDLE_TIMEOUT_SECONDS = 120.0

    def __init__(self, server, conn, addr):
        self.server = server
        self.conn = conn
        self.addr = addr
        self.protocol = getattr(server, "protocol", None)
        # Bytes read but not yet consumed. Keeping them is the whole point: a
        # client that coalesces SETUP+PLAY into one segment used to lose the
        # second request, because the parser read the rest of the buffer as part
        # of the first request's body and then dropped it.
        self._buf = b""
        try:
            conn.settimeout(self.IDLE_TIMEOUT_SECONDS)
        except OSError:  # pragma: no cover - defensive
            pass

    def handle(self):
        try:
            while True:
                try:
                    request_line, headers, body = self._read_request()
                except _BadRequest as e:
                    logger.info("RTSP bad request from %s: %s", self.addr, e)
                    self._send_response(400, {}, "")
                    break
                if request_line is None:
                    break
                parts = request_line.split()
                if len(parts) < 3:
                    logger.info("RTSP malformed request line from %s: %r",
                                self.addr, request_line)
                    self._send_response(400, {}, "")
                    break
                method = parts[0].upper()
                uri = parts[1] if len(parts) > 1 else "/"
                resp = self.handle_request(method, uri, headers, body)
                if resp is None:
                    break
                code, resp_headers, resp_body = resp
                self._send_response(code, resp_headers, resp_body)
                if method == "TEARDOWN":
                    break
        finally:
            try:
                self.conn.close()
            except Exception:
                pass

    def _fill(self):
        """Pull one more chunk into the buffer; False on EOF or idle timeout."""
        try:
            chunk = self.conn.recv(4096)
        except socket.timeout:
            logger.info("RTSP idle timeout from %s, closing connection", self.addr)
            return False
        except OSError:
            return False
        if not chunk:
            return False
        self._buf += chunk
        return True

    def _read_request(self):
        # Read headers (terminated by blank line).
        while b"\r\n\r\n" not in self._buf:
            if not self._fill():
                return None, {}, ""
        header_blob, _, rest = self._buf.partition(b"\r\n\r\n")
        self._buf = rest
        lines = header_blob.decode("utf-8", "replace").split("\r\n")
        request_line = lines[0]
        headers = {}
        for line in lines[1:]:
            if ":" in line:
                k, v = line.split(":", 1)
                headers[k.strip()] = v.strip()
        # Read body if Content-Length present (SDP for ANNOUNCE).
        raw_length = get_header(headers, "Content-Length") or "0"
        try:
            length = int(raw_length)
        except (TypeError, ValueError):
            # Used to raise straight out of the handler thread: the connection
            # died with no response at all and nothing in the log.
            raise _BadRequest("non-numeric Content-Length: {!r}".format(raw_length))
        if length < 0:
            raise _BadRequest("negative Content-Length: {}".format(length))
        while len(self._buf) < length:
            if not self._fill():
                break
        body = self._buf[:length].decode("utf-8", "replace") if length else ""
        self._buf = self._buf[length:]
        return request_line, headers, body

    def _send_response(self, code, headers, body):
        reason = REASON_PHRASES.get(code, "Unknown")
        out = "RTSP/1.0 {} {}\r\n".format(code, reason)
        for k, v in headers.items():
            out += "{}: {}\r\n".format(k, v)
        out += "Content-Length: {}\r\n\r\n".format(len(body))
        try:
            self.conn.sendall(out.encode("utf-8"))
            if body:
                self.conn.sendall(body.encode("utf-8"))
        except OSError:
            pass

    def handle_request(self, method, headers, body):  # pragma: no cover
        raise NotImplementedError


class _RtspServer:
    """Barebones TCP server for RTSP (no BaseServer inheritance to keep it simple)."""

    def __init__(self, addr, handler_class):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(addr)
        self.sock.listen(16)
        self.handler_class = handler_class
        self.protocol = None
        self._serving = True

    def serve_forever(self):
        while self._serving:
            try:
                conn, addr = self.sock.accept()
            except OSError as e:
                if not self._serving:
                    break
                # A *single* failed accept (ECONNABORTED, EMFILE) must not take
                # the listener down for good: mDNS would keep advertising this
                # port, leaving the device visible and silently uncastable. This
                # is the same trap as the Chromecast accept loop (AGENTS.md
                # section 4.1), which killed TLS with one flaky handshake.
                logger.warning("RTSP accept failed, listener kept alive: %s", e)
                time.sleep(0.2)
                continue
            if not self._serving:
                break
            h = self.handler_class(self, conn, addr)
            t = threading.Thread(target=h.handle, name="AIRPLAY_CLIENT", daemon=True)
            t.start()

    def shutdown(self):
        self._serving = False
        try:
            self.sock.close()
        except Exception:
            pass

    def server_close(self):
        try:
            self.sock.close()
        except Exception:
            pass
