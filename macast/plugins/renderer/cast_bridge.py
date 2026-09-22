# Copyright (c) 2026 by pingod. All Rights Reserved.
# Chromecast Bridge for Macast
#
# Macast Metadata
# <macast.title>Chromecast Bridge</macast.title>
# <macast.renderer>CastBridgeRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.role>addon</macast.role>
# <macast.desc>Re-cast what you received to another Chromecast on the network -- a TV, a speaker group. Pick the target from the menu bar; Macast stays the receiver, the other device does the playing.</macast.desc>
#
# Why: a phone app that only speaks DLNA (or a private dialect of it) cannot
# reach a TV that only speaks Chromecast. Macast already receives the first half;
# this renderer sends the second half, which turns the Mac into a protocol
# translator instead of the player.
#
# Implementation notes, because this is the *sender* side of a protocol Macast
# otherwise implements as a receiver:
#   * it reuses macast.protocol_cast's framing (`encode_cast_message` /
#     `parse_cast_message`) rather than adding pychromecast as a dependency. A
#     single-file plugin cannot install a pip package, and bundling one would
#     change the packaged app;
#   * the sequence is the one every Cast sender performs:
#     deviceauth CHALLENGE -> CONNECT receiver-0 -> LAUNCH(CC1AD845) ->
#     CONNECT <transportId> -> LOAD. The reply to LAUNCH is what names the
#     transport, so it is never hardcoded;
#   * discovery is mDNS (`_googlecast._tcp`). It runs in the background because
#     `build_menu` is called on the UI thread and must never block;
#   * the connection work happens in a worker thread as well: the sender is
#     waiting for the phone, which must not be held for a LAN round trip.

import json
import logging
import socket
import ssl
import struct
import threading
import time
from enum import Enum

import cherrypy

from macast import Setting, MenuItem, gui
from macast.renderer import Renderer, RendererSetting
from macast.protocol_cast import (encode_cast_message, parse_cast_message,
                                  DEFAULT_MEDIA_APP_ID, NS_CONNECTION,
                                  NS_MEDIA, NS_RECEIVER)

logger = logging.getLogger("CastBridge")
logger.setLevel(logging.INFO)

CAST_PORT = 8009
SERVICE_TYPE = "_googlecast._tcp.local."
#: `DeviceAuthMessage{challenge: Challenge{}}` -- the empty challenge every
#: sender opens with. The response (a signature) is not verified: Macast is an
#: uncertified sender, and the receiving device only requires that we asked.
DEVICE_AUTH_CHALLENGE = b"\x0a\x00"

#: Last discovery result and whether a search is in flight. Module level because
#: the menu is rebuilt from scratch on every open.
_devices = []
_searching = False
#: Wall clock of the last completed search; 0.0 = never. The menu reads it to
#: tell「still looking」from「looked, found nothing」-- without it a LAN with no
#: Chromecast to find made the menu read「搜索中」on every single open.
_searched_at = 0.0
#: Opening the menu again inside this window reuses the cached answer instead
#: of kicking another multicast search (see `_search_due`).
SEARCH_REFRESH_SECONDS = 15.0
_search_lock = threading.Lock()


def discover(timeout=3.0):
    """[(friendly name, host, port)] for Chromecasts answering on the LAN."""
    try:
        from zeroconf import Zeroconf, ServiceBrowser
    except ImportError:
        logger.error("zeroconf is unavailable: cannot look for Chromecasts")
        return []
    found = {}

    class _Listener(object):
        def add_service(self, zc, type_, name):
            info = zc.get_service_info(type_, name, timeout=2000)
            if info is None:
                return
            addresses = info.parsed_addresses()
            if not addresses:
                return
            properties = {}
            for key, value in (info.properties or {}).items():
                if isinstance(key, bytes):
                    key = key.decode('utf-8', 'replace')
                if isinstance(value, bytes):
                    value = value.decode('utf-8', 'replace')
                properties[key] = value
            found[name] = (properties.get('fn') or info.server or name,
                           addresses[0], info.port)

        def update_service(self, *args):
            pass

        def remove_service(self, zc, type_, name):
            found.pop(name, None)

    zeroconf = Zeroconf()
    try:
        ServiceBrowser(zeroconf, SERVICE_TYPE, _Listener())
        time.sleep(timeout)
    except Exception as e:
        logger.error("Chromecast discovery failed: %s", e)
    finally:
        zeroconf.close()
    return sorted(found.values())


def start_search():
    """Kick off a discovery in the background; safe to call repeatedly."""
    global _searching
    with _search_lock:
        if _searching:
            return False
        _searching = True
    threading.Thread(target=_search, daemon=True,
                     name="CAST_BRIDGE_SEARCH").start()
    return True


def _search():
    global _devices, _searching, _searched_at
    try:
        found = discover()
        # Keep the previous list when a search comes back empty: a device that
        # is asleep right now is still the device the user was casting to.
        if found:
            _devices = found
    finally:
        with _search_lock:
            _searching = False
            _searched_at = time.time()


def _search_due(searched_at, now):
    """Whether an empty device list warrants a fresh multicast search.

    Never searched counts as due, so the first open always looks. A completed
    search younger than SEARCH_REFRESH_SECONDS does not -- without that a LAN
    with no device to find re-ran the search on every open, and the menu could
    only ever read「搜索中…再展开一次菜单」.
    """
    return searched_at == 0.0 or (now - searched_at) >= SEARCH_REFRESH_SECONDS


def _searched_suffix(searched_at):
    """「（搜于 08:47:31）」 once a search has completed, '' before the first."""
    if not searched_at:
        return ''
    return '（搜于 {}）'.format(
        time.strftime('%H:%M:%S', time.localtime(searched_at)))


class SettingProperty(Enum):
    #: "host:port" of the selected device.
    Bridge_Target = 1
    #: Friendly name, for messages only.
    Bridge_Target_Name = 2
    #: Socket timeout in seconds.
    Bridge_Timeout = 3


class _CastSender(object):
    """The smallest Cast v2 sender a receiver will accept."""

    def __init__(self, host, port=CAST_PORT, timeout=5.0, source_id="sender-0"):
        self.host = host
        self.port = port
        self.timeout = max(1.0, float(timeout))
        self.source_id = source_id
        self.sock = None
        self.transport_id = None
        self.media_session_id = 1
        self.player_state = ''

    # -- framing -----------------------------------------------------------

    def _send(self, destination, namespace, payload, binary=False):
        body = payload if binary else json.dumps(payload)
        blob = encode_cast_message(self.source_id, destination, namespace, body,
                                  binary)
        self.sock.sendall(struct.pack('>I', len(blob)) + blob)

    def _recv_exactly(self, count):
        buf = b""
        while len(buf) < count:
            chunk = self.sock.recv(count - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def _recv(self):
        header = self._recv_exactly(4)
        if header is None:
            return None
        (length,) = struct.unpack('>I', header)
        body = self._recv_exactly(length)
        return parse_cast_message(body) if body else None

    def _await(self, wanted, timeout=None):
        """Read until a message of type `wanted` arrives, or give up."""
        deadline = time.time() + (timeout or self.timeout)
        while time.time() < deadline:
            self.sock.settimeout(max(0.2, deadline - time.time()))
            try:
                message = self._recv()
            except (socket.timeout, ssl.SSLError, OSError):
                return None
            if message is None:
                return None
            if message.get("payload_type") != 0:
                continue
            try:
                data = json.loads(message.get("payload_utf8") or "{}")
            except ValueError:
                continue
            if data.get("type") == wanted:
                return data
        return None

    # -- protocol ----------------------------------------------------------

    def connect(self):
        raw = socket.create_connection((self.host, self.port), timeout=self.timeout)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.sock = context.wrap_socket(raw, server_hostname=None)
        self.sock.settimeout(self.timeout)
        self._send("receiver-0", "urn:x-cast:com.google.cast.tp.deviceauth",
                   DEVICE_AUTH_CHALLENGE, binary=True)
        self._recv()                      # device-auth response, not verified
        self._send("receiver-0", NS_CONNECTION, {"type": "CONNECT"})
        return self

    def launch(self):
        """Launch the default media receiver; returns its transport id."""
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "LAUNCH", "appId": DEFAULT_MEDIA_APP_ID,
                    "requestId": 1})
        status = self._await("RECEIVER_STATUS") or {}
        applications = (status.get("status") or {}).get("applications") or []
        if not applications:
            raise RuntimeError("目标设备没有启动媒体接收器（LAUNCH 无响应）")
        self.transport_id = applications[0].get("transportId")
        if not self.transport_id:
            raise RuntimeError("目标设备没有返回 transportId")
        self._send(self.transport_id, NS_CONNECTION, {"type": "CONNECT"})
        return self.transport_id

    def load(self, url, content_type=''):
        media = {"contentId": url, "streamType": "BUFFERED"}
        if content_type:
            media["contentType"] = content_type
        self._send(self.transport_id, NS_MEDIA,
                   {"type": "LOAD", "requestId": 2, "autoplay": True,
                    "media": media})
        status = self._await("MEDIA_STATUS") or {}
        entries = status.get("status") or [{}]
        if isinstance(entries, dict):
            entries = [entries]
        entry = entries[0] or {}
        self.media_session_id = entry.get("mediaSessionId", 1)
        self.player_state = (entry.get("playerState") or "").upper()
        return status

    def _media_action(self, action):
        if self.transport_id is None:
            return False
        self._send(self.transport_id, NS_MEDIA,
                   {"type": action, "requestId": 3,
                    "mediaSessionId": self.media_session_id})
        return True

    def pause(self):
        return self._media_action("PAUSE")

    def play(self):
        return self._media_action("PLAY")

    def stop(self):
        if self.transport_id is None:
            return False
        try:
            self._send(self.transport_id, NS_MEDIA,
                       {"type": "STOP", "requestId": 4,
                        "mediaSessionId": self.media_session_id})
            # Also close the receiver app so the TV goes back to its idle screen.
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "sessionId": None, "requestId": 5})
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info("could not send STOP to the device: %s", e)
            return False
        return True

    def set_volume(self, level):
        """`level` in 0..100, as the DLNA rendering control uses."""
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "SET_VOLUME", "requestId": 6,
                    "volume": {"level": max(0.0, min(1.0, level / 100.0))}})

    def close(self):
        if self.sock is not None:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None


class CastBridgeRenderer(Renderer):

    def __init__(self):
        super(CastBridgeRenderer, self).__init__()
        self._lock = threading.Lock()
        self._sender = None
        #: Bumped on every cast/stop so a slow connection cannot overwrite the
        #: state of the cast that replaced it.
        self._generation = 0
        self._url = ''
        self.renderer_setting = CastBridgeSetting()

    # -- target ------------------------------------------------------------

    def target(self):
        """(host, port, name) of the selected device; host is None if unset."""
        raw = Setting.get(SettingProperty.Bridge_Target, '') or ''
        name = Setting.get(SettingProperty.Bridge_Target_Name, '') or ''
        if not raw:
            return None, None, name
        host, _, port = raw.partition(':')
        try:
            port = int(port or CAST_PORT)
        except ValueError:
            port = CAST_PORT
        return host, port, name or host

    def playing_url(self):
        return self._url

    # -- Renderer API ------------------------------------------------------

    def set_media_url(self, url, start="0"):
        if not url:
            return
        self._url = url
        with self._lock:
            self._generation += 1
            generation = self._generation
            old, self._sender = self._sender, None
        _close_quietly(old)
        threading.Thread(target=self._cast, args=(url, generation),
                         daemon=True, name="CAST_BRIDGE").start()
        # The phone is answered now and corrected if the hand-off fails; the
        # alternative is holding a DLNA SOAP response for a LAN round trip.
        self.set_state_transport('PLAYING')
        cherrypy.engine.publish('renderer_av_uri', url)

    def set_media_stop(self):
        with self._lock:
            self._generation += 1
            sender, self._sender = self._sender, None
        if sender is not None:
            sender.stop()
            _close_quietly(sender)
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        with self._lock:
            sender = self._sender
        if sender is not None and sender.pause():
            self.set_state_transport('PAUSED_PLAYBACK')
        else:
            logger.info('pause ignored: no bridged device is connected')

    def set_media_resume(self):
        with self._lock:
            sender = self._sender
        if sender is not None and sender.play():
            self.set_state_transport('PLAYING')
        else:
            logger.info('resume ignored: no bridged device is connected')

    def set_media_volume(self, data):
        with self._lock:
            sender = self._sender
        if sender is None:
            return
        try:
            sender.set_volume(int(data))
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.error('cannot set the device volume: %s', e)

    def stop(self):
        self.set_media_stop()
        super(CastBridgeRenderer, self).stop()

    # -- internals ---------------------------------------------------------

    def _cast(self, url, generation):
        host, port, name = self.target()
        if host is None:
            self._fail('还没有选择投屏目标：在菜单栏的「Target」里选一台 Chromecast',
                       generation)
            return
        try:
            sender = _CastSender(host, port,
                                 timeout=Setting.get(SettingProperty.Bridge_Timeout, 5))
            sender.connect()
            sender.launch()
            sender.load(url)
        except Exception as e:
            self._fail('投屏到 {}（{}:{}）失败：{}'.format(name, host, port, e),
                       generation)
            return
        with self._lock:
            if generation != self._generation:
                _close_quietly(sender)
                return
            self._sender = sender
        if sender.player_state == 'PAUSED':
            self.set_state_transport('PAUSED_PLAYBACK')
        else:
            self.set_state_transport('PLAYING')
        logger.info('bridged %s to %s (%s:%s)', url, name, host, port)

    def _fail(self, message, generation):
        with self._lock:
            if generation != self._generation:
                return
        logger.error(message)
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        cherrypy.engine.publish('app_notify', 'Macast', message)


def _close_quietly(sender):
    if sender is not None:
        try:
            sender.close()
        except Exception:
            pass


class CastBridgeSetting(RendererSetting):

    def build_menu(self):
        # Only look when there is nothing to show AND the last answer has gone
        # stale, or a menu opened a second apart re-ran the search each time.
        if (not _devices and not _searching
                and _search_due(_searched_at, time.time())):
            start_search()
        current = Setting.get(SettingProperty.Bridge_Target, '') or ''
        children = []
        for name, host, port in list(_devices):
            target = '{}:{}'.format(host, port)
            children.append(MenuItem('{} · {}'.format(name, host),
                                     self.on_target_clicked,
                                     checked=(target == current),
                                     data=(name, target)))
        if not children:
            #「搜索中」is only honest while the very first look is in flight;
            # once a search has completed, an empty list is a fact to state.
            if _searching and not _searched_at:
                children.append(MenuItem('搜索中…再展开一次菜单', enabled=False))
            else:
                children.append(MenuItem(
                    '没有发现 Chromecast' + _searched_suffix(_searched_at),
                    enabled=False))
        children.append(MenuItem('重新搜索', self.on_refresh))
        return [
            MenuItem('Chromecast Bridge v0.1', enabled=False),
            MenuItem('Target', children=children),
        ]

    def on_target_clicked(self, item):
        name, target = item.data
        Setting.set(SettingProperty.Bridge_Target, target)
        Setting.set(SettingProperty.Bridge_Target_Name, name)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '投屏目标：{}'.format(name), sound=False)
        # Move what is playing right now, instead of waiting for the next cast.
        renderers = cherrypy.engine.publish('get_renderer')
        renderer = renderers.pop() if renderers else None
        if isinstance(renderer, CastBridgeRenderer) and renderer.playing_url():
            renderer.set_media_url(renderer.playing_url())

    def on_refresh(self, item):
        start_search()
        cherrypy.engine.publish('app_notify', 'Macast', '正在搜索 Chromecast…',
                                sound=False)


if __name__ == '__main__':
    gui(CastBridgeRenderer())
