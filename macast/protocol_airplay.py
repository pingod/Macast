# Copyright (c) 2021 by xfangfang. All Rights Reserved.
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
import os
import random
import socket
import threading

from .discovery import MDNSAdvertiser
from .protocol import Protocol
from .utils import Setting, SETTING_DIR

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
        cseq = headers.get("CSeq", "1")
        base = {"CSeq": cseq, "Server": Setting.get_server_info()}
        # Some clients GET /info over the same socket to read device capability.
        if method == "GET" and uri.startswith("/info"):
            base["Content-Type"] = "text/x-apple-plist+xml"
            return 200, base, self._info_plist()
        if method == "OPTIONS":
            base["Public"] = (
                "ANNOUNCE, SETUP, RECORD, PLAY, PAUSE, FLUSH, STOP, "
                "SET_PARAMETER, GET_PARAMETER, TEARDOWN"
            )
            # In AirPlay the *client* sends Apple-Challenge and the receiver
            # answers with a signed Apple-Response. We have no signing key, so
            # we deliberately do not promise one (see module docstring).
            if headers.get("Apple-Challenge"):
                logger.info(
                    "Client sent Apple-Challenge; signature auth is not "
                    "implemented, continuing without Apple-Response")
            return 200, base, ""
        if method == "FLUSH":
            base["Session"] = "{}".format(max(self._session, 1))
            return 200, base, ""
        if method == "ANNOUNCE":
            url = self._parse_content_location(headers, body)
            if url:
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
            with self._lock:
                url = self._current_url
            if url:
                self.renderer.set_media_url(url)
            base["Session"] = "{}".format(max(self._session, 1))
            base["Range"] = "npt=now-"
            base["RTP-Info"] = "seq=0;rtptime=0"
            return 200, base, ""
        if method in ("PAUSE", "SET_PARAMETER"):
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
        # fallback
        return 200, base, ""

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
        cl = headers.get("Content-Location")
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

    def __init__(self, server, conn, addr):
        self.server = server
        self.conn = conn
        self.addr = addr
        self.protocol = getattr(server, "protocol", None)

    def handle(self):
        try:
            while True:
                request_line, headers, body = self._read_request()
                if request_line is None:
                    break
                parts = request_line.split()
                if len(parts) < 3:
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

    def _read_request(self):
        # Read headers (terminated by blank line).
        raw = b""
        while b"\r\n\r\n" not in raw:
            chunk = self.conn.recv(4096)
            if not chunk:
                return None, {}, ""
            raw += chunk
        header_blob, _, rest = raw.partition(b"\r\n\r\n")
        lines = header_blob.decode("utf-8", "replace").split("\r\n")
        request_line = lines[0]
        headers = {}
        for line in lines[1:]:
            if ":" in line:
                k, v = line.split(":", 1)
                headers[k.strip()] = v.strip()
        # Read body if Content-Length present (SDP for ANNOUNCE).
        length = int(headers.get("Content-Length", "0"))
        body = rest.decode("utf-8", "replace")
        while len(rest) < length:
            chunk = self.conn.recv(4096)
            if not chunk:
                break
            rest += chunk
        if length:
            body = rest[:length].decode("utf-8", "replace")
        return request_line, headers, body

    def _send_response(self, code, headers, body):
        reason = "OK" if code == 200 else "Error"
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
            except OSError:
                break
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
