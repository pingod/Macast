# Copyright (c) 2026 by pingod. All Rights Reserved.
# Screen Mirror for Macast
#
# Macast Metadata
# <macast.title>Screen Mirror</macast.title>
# <macast.renderer>ScreenMirrorRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.9</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Mirror this Mac/PC/desktop screen to a Chromecast on the LAN (two channels: a compatible MPEG-TS LOAD, or an experimental low-latency Cast Streaming path that speaks Chrome's own mirroring protocol and falls back to LOAD if the device refuses it), to an old DLNA TV (five compatibility profiles, nothing to install on the TV), or to any browser on the LAN (open a URL -- no app needed). ffmpeg captures (avfoundation / gdigrab / x11grab), encodes, and a live stream is served from this machine: MPEG-TS LOADed on the TV for Chromecast, fragmented MP4 played in a bundled web page for browsers, or a deliberately endless MPEG-PS / MPEG-TS / MKV "file" that a UPnP MediaRenderer is pushed to fetch over SOAP. System audio rides along where a tap exists: macOS gets a one-click assisted install (official BlackHole pkg, sha256-verified, plus an auto-created multi-output device), Linux uses the PulseAudio monitor; Windows is video only. Also selectable: which display, cursor or no cursor, four quality presets, VideoToolbox hardware encoding, and a DLNA watchdog that re-pushes when the TV falls out of PLAYING and tells you which profile to try next.</macast.desc>
#
# Why: Macast is a receiver -- everything it plays was pushed to it. This
# plugin turns it around for one case: cast what is on this Mac's display,
# the way JustStream does, without leaving the menu bar.
#
# Implementation notes, because this is a *live* sender (see also
# cast_bridge.py, which forwards a finished URL):
#   * the pipeline is  ffmpeg screen capture -> H.264 (libx264 or
#     VideoToolbox) -> a muxer chosen by the output target -> a tiny HTTP
#     server that broadcasts every chunk to every connected client. The
#     Chromecast LOADs http://<this machine>:<port>/stream/<id>.ts; a browser
#     opens /browser?token=<page token> and feeds the same bytes from
#     the matching .m4s URL into MSE, falling back to a progressive <video>;
#   * both public addresses carry a per-session secret: the stream URL is
#     built from a random id, and /browser requires the matching token --
#     a live mirror is not something to hand to "any host that can reach
#     this port" (see the note on _StreamHandler and AGENTS.md 4.7);
#   * capture is platform dispatched: avfoundation (macOS), gdigrab
#     (Windows), x11grab (Linux/X11). System audio rides along where a tap
#     exists: macOS needs a BlackHole device (no released FFmpeg can see
#     system audio natively -- the screencapturekit demuxer never shipped),
#     Linux uses the PulseAudio `<sink>.monitor`. Windows stays video-only.
#     The probe is cached because the menu must never spawn ffmpeg;
#   * slow consumers drop whole chunks rather than blocking the reader --
#     for a live stream a stale frame is worse than a missing one, and a
#     blocked stdout pipe would stall the encoder;
#   * for the browser target a late joiner is replayed the fMP4 init segment
#     plus a rolling tail, so it can attach mid-stream; the MPEG-TS target is
#     deliberately *not* replayed, because a TV would then have a backlog to
#     drain and would sit seconds behind for the rest of the session;
#   * the DLNA target is the awkward one, because a ten-year-old MediaRenderer
#     has no notion of "live": it is handed a URL that pretends to be a finite
#     file. So the stream is served as one -- Content-Length under 2 GiB (some
#     firmware does signed 32-bit arithmetic there), Accept-Ranges plus
#     transferMode/contentFeatures.dlna.org on every answer, a bounded sniff
#     answered with *exactly* the bytes asked for (MPEG-PS padding, never a
#     short read), and reconnects served by absolute byte offset out of a ring
#     that is 48 MiB deep instead of the drop-oldest queue the other targets
#     use. The URL is only pushed once the ring holds ~20 MiB, which is where
#     this target's 20-ish seconds of latency comes from;
#   * the Cast sequence is the sender-side one: deviceauth CHALLENGE ->
#     CONNECT receiver-0 -> LAUNCH(CC1AD845) -> CONNECT <transportId> ->
#     LOAD with streamType LIVE. Framing is reused from
#     macast.protocol_cast, so no pychromecast dependency (a single-file
#     plugin cannot install pip packages);
#   * the 低延迟 target skips LOAD entirely and speaks Cast Streaming, which
#     Chrome's own "Cast desktop" uses: LAUNCH(0F5096E8) -> an OFFER on
#     urn:x-cast:com.google.cast.webrtc -> an ANSWER that names a UDP port ->
#     RTP packets with a 7-byte Cast header, each access unit encrypted once
#     with AES-128-CTR (pure Python -- a single-file plugin has no crypto
#     library) and keyed per frame. Nothing is served over HTTP for this one,
#     so it has no viewer URL and no audio, and because the field layouts are
#     transcribed rather than observed on a television it is opt-in and falls
#     back to LOAD when the device refuses the mirroring app;
#   * ffmpeg is located via PATH *and* the usual install directories, because
#     a menu-bar app launched from Finder does not inherit the shell PATH;
#   * capture fails closed: without Screen Recording permission (macOS)
#     ffmpeg exits within a second, and the plugin turns that into a
#     message naming the System Settings pane instead of a silent nothing.
#   * the BlackHole assisted install decides by *witness*, not by "is the
#     device missing", because five different machine states answer to that one
#     question and only two of them want an installer: see _blackhole_state.
#     Running the pkg again on a machine whose driver is merely unloaded is the
#     loop the user reported ("装不完"), and CoreAudio-yes/ffmpeg-no is a
#     microphone permission, which no download fixes either.

import json
import logging
import os
import re
import secrets
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from collections import deque
from enum import Enum
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from queue import Queue, Empty

import cherrypy

from macast import Setting, MenuItem, gui
from macast.renderer import Renderer, RendererSetting
from macast.protocol_cast import (encode_cast_message, parse_cast_message,
                                  DEFAULT_MEDIA_APP_ID, NS_CONNECTION,
                                  NS_MEDIA, NS_RECEIVER)

logger = logging.getLogger("ScreenMirror")
logger.setLevel(logging.INFO)

CAST_PORT = 8009
SERVICE_TYPE = "_googlecast._tcp.local."
DEVICE_AUTH_CHALLENGE = b"\x0a\x00"
#: The receiver app that speaks Cast Streaming. Not the Default Media
#: Receiver: mirroring lives on its own app id, its own namespace, and it never
#: accepts a LOAD -- the media plane leaves TLS for UDP entirely.
MIRROR_APP_ID = '0F5096E8'
NS_WEBRTC = 'urn:x-cast:com.google.cast.webrtc'
STREAM_PREFIX = "/stream/"
CHUNK = 4096
#: How long ffmpeg may survive before its death is blamed on the
#: Screen Recording permission instead of a genuine mid-stream failure.
EARLY_DEATH_SECONDS = 5.0
#: Rolling tail replayed to a late-joining browser viewer. Enough for a
#: second of 1080p plus the next keyframe, small enough to not matter.
REPLAY_BYTES = 8 << 20

#: height -> (label, video bitrate). height 0 means "do not scale".
QUALITIES = {'360': (360, 2500000),
             '720': (720, 5000000),
             '1080': (1080, 10000000),
             'source': (0, 12000000)}


def quality_preset(key=None):
    """(height, bitrate) for a menu key; anything unknown is 720p.

    Two callers need this -- the renderer picks the encoder settings, the menu
    has to say what the low-latency channel will actually do with them.
    """
    if key is None:
        key = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
    return QUALITIES.get(key, QUALITIES['720'])
#: keyframe / fragment cadence in seconds. One per second keeps the browser's
#: MSE join latency and the TV's seek behaviour both honest.
FPS = 24

#: output kind -> (menu label, HTTP suffix, Content-Type, muxer args)
OUTPUTS = {
    'cast': ('Chromecast / Google TV', 'ts', 'video/mp2t',
             ['-f', 'mpegts', 'pipe:1']),
    'browser': ('浏览器（打开网址即可看）', 'm4s', 'video/mp4',
                ['-f', 'mp4', '-movflags',
                 'frag_keyframe+empty_moov+default_base_moof', 'pipe:1']),
    #: Not a different device: the same Chromecast, driven by its mirroring app
    #: instead of by LOAD. No HTTP suffix and no Content-Type because nothing is
    #: served -- these bytes are pushed to a UDP port.
    'caststream': ('Chromecast 低延迟（实验 · 无声音）', 'h264', None,
                   ['-f', 'h264', 'pipe:1']),
    #: A DLNA TV is told it is downloading a finite file, so everything about
    #: this target -- container, codec, even the file extension in the URL --
    #: comes from the compatibility profile rather than from here. The muxer
    #: slot is None for exactly that reason.
    'dlna': ('DLNA 电视（老电视，MPEG-PS）', 'mpg', 'video/mpeg', None),
}
DEFAULT_OUTPUT = 'cast'
BROWSER_PATH = '/browser'


class _DlnaProfile(object):
    """One 'shape' of the infinite file a DLNA TV is asked to download.

    Old renderers are a compatibility matrix, not a spec: which one of these
    works is a property of the TV, which is why the menu offers all five and
    the watchdog tells you to switch. Facts (container, CBR rates, GOP, the
    advertised size/duration pair) come from the MirrorCast reference study in
    docs/Casting-Suite-Plan.md 2.1; **no real old TV has been tested** -- see
    the same section for what that means for these claims.
    """

    def __init__(self, label, muxer, suffix, content_type, org_pn,
                 video_args, audio_args, width, height, fps, bitrate):
        self.label = label
        self.muxer = muxer                      # ffmpeg args, minus pipe:1
        self.suffix = suffix
        self.content_type = content_type
        self.org_pn = org_pn
        self.video_args = video_args
        self.audio_args = audio_args
        #: width 0 means "keep the desktop's aspect and scale by height". Only
        #: the MPEG-PS shapes pin an exact frame: the DVD-derived muxers refuse
        #: anything else, and a 3400-pixel-wide Retina grab is not 720.
        self.width = width
        self.height = height
        self.fps = fps
        self.bitrate = bitrate

    def video_filter(self):
        """Anamorphic on purpose: the PS shapes squeeze a 16:9 desktop into a
        4:3 frame and then tell the TV to stretch it back, which is exactly what
        a DVD does -- and what the reference implementation is measured on."""
        if self.width:
            return 'scale={}:{}'.format(self.width, self.height)
        return 'scale=-2:{}'.format(self.height)


def _mpeg2(fps, bitrate):
    """CBR MPEG-2: the rate control matters as much as the codec, because the
    TV models a fullness buffer and a variable bitrate reads as starvation."""
    return ['-c:v', 'mpeg2video', '-b:v', str(bitrate),
            '-minrate', str(bitrate), '-maxrate', str(bitrate),
            '-bufsize', '2304k', '-g', str(fps * 3 // 5), '-r', str(fps),
            '-pix_fmt', 'yuv420p']


#: profile id -> shape. Ordered from "most likely to work on an old TV" to
#: "modern renderer that only speaks TS/MKV"; the watchdog walks this list
#: forward when the TV keeps refusing.
DLNA_PROFILES = {
    'ps-pal': _DlnaProfile(
        'MPEG-PS · PAL 576p（老电视首选）', ['-f', 'vob'], 'mpg', 'video/mpeg',
        'MPEG_PS_PAL', _mpeg2(25, 4500000),
        ['-c:a', 'ac3', '-b:a', '192k', '-ar', '48000', '-ac', '2'],
        720, 576, 25, 4500000),
    'ps-ntsc': _DlnaProfile(
        'MPEG-PS · NTSC 480p（北美/日本老电视）', ['-f', 'vob'], 'mpg',
        'video/mpeg', 'MPEG_PS_NTSC', _mpeg2(30, 4500000),
        ['-c:a', 'ac3', '-b:a', '192k', '-ar', '48000', '-ac', '2'],
        720, 480, 30, 4500000),
    'ts-mpeg2': _DlnaProfile(
        'MPEG-TS · MPEG-2 576p（认 TS 不认 PS 的电视）', ['-f', 'mpegts'],
        'ts', 'video/vnd.dlna.mpeg-tts', 'MPEG_TS_SD_EU',
        _mpeg2(25, 5000000),
        ['-c:a', 'ac3', '-b:a', '192k', '-ar', '48000', '-ac', '2'],
        720, 576, 25, 5000000),
    'ts-h264': _DlnaProfile(
        'MPEG-TS · H.264 720p（较新的电视，清晰度更高）', ['-f', 'mpegts'],
        'ts', 'video/vnd.dlna.mpeg-tts', None, None,
        ['-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2'],
        0, 720, 25, 6000000),
    'mkv-h264': _DlnaProfile(
        'Matroska · H.264 720p（只认 MKV 的电视/Kodi）', ['-f', 'matroska'],
        'mkv', 'video/x-matroska', None, None,
        ['-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2'],
        0, 720, 25, 6000000),
}
DEFAULT_DLNA_PROFILE = 'ps-pal'
#: How much of the ring the TV is allowed to be behind before we push the URL.
#: The advertised file is "already 20 MiB long", so the renderer's first big
#: probe is served from memory instead of stalling on the encoder. This is
#: where the ~20-25 s of latency on this target comes from -- the price of an
#: old TV being willing to play a live stream at all.
DLNA_PREFILL_BYTES = 20 << 20
#: Ring size: 48 MiB of produced bytes stay addressable by absolute offset.
DLNA_RING_BYTES = 48 << 20
#: The advertised file must stay under 2 GiB. Some firmware does signed 32-bit
#: arithmetic on Content-Length, which turns 3.9 GB into
#: `Range: bytes=0-18446744072566584319` and "this file is unsupported".
DLNA_MAX_ADVERTISED_SIZE = 1900000000
DLNA_SECONDARY_HEADER = 'Streaming'
DLNA_ORG_FLAGS = '01500000000000000000000000000000'
#: An MPEG-PS padding packet (private_stream_1, zero length). A renderer that
#: asks for exactly n bytes gets exactly n bytes -- it is sniffing a file, and
#: a short answer is what makes it give up.
PS_PADDING = b'\x00\x00\x01\xbe\x00\x00'
DLNA_POLL_SECONDS = 5.0
#: How long a bounded sniff waits for the encoder before we pad the rest.
#: Longer than a keyframe interval, shorter than most TVs' own read timeout.
DLNA_SNIFF_TIMEOUT = 10.0
#: Ceiling on the prefill wait: a slow encoder must not hold the session open
#: forever, and the TV is better off starting laggy than never starting.
DLNA_PREFILL_TIMEOUT = 45.0
DLNA_MAX_REPUSHES = 8
DLNA_SERVICE = 'urn:schemas-upnp-org:service:AVTransport:1'
DLNA_SEARCH_TARGETS = ('urn:schemas-upnp-org:device:MediaRenderer:1',
                       DLNA_SERVICE)
SSDP_ADDR = '239.255.255.250'
SSDP_PORT = 1900

_devices = []
_searching = False
#: wall clock of the last completed Chromecast search; 0.0 = never. The menu
#: reads it to tell「still looking」from「looked, found nothing」-- without it
#: a LAN with no device to find made the menu read「搜索中」on every open.
_searched_at = 0.0
#: Opening the menu again inside this window reuses the cached answer instead
#: of kicking another multicast search (see `_search_due`).
SEARCH_REFRESH_SECONDS = 15.0
_search_lock = threading.Lock()
#: cached result of `ffmpeg -encoders`, because the menu must never spawn it
_hw_encoder_cache = {}


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
    global _searching
    with _search_lock:
        if _searching:
            return False
        _searching = True
    threading.Thread(target=_search, daemon=True,
                     name="SCREEN_MIRROR_SEARCH").start()
    return True


def _search():
    global _devices, _searching, _searched_at
    try:
        found = discover()
        if found:
            _devices = found
    finally:
        with _search_lock:
            _searching = False
            _searched_at = time.time()


def _search_due(searched_at, now):
    """Whether an empty device list warrants a fresh multicast search.

    Never searched counts as due, so the first open always looks. A completed
    search younger than SEARCH_REFRESH_SECONDS does not -- and that is what
    stops `build_menu` from restarting the search on every single open, which
    is how the menu came to read「搜索中…再展开一次菜单」forever on a LAN
    that has no device to find.
    """
    return searched_at == 0.0 or (now - searched_at) >= SEARCH_REFRESH_SECONDS


def _searched_suffix(searched_at):
    """「（搜于 08:47:31）」 once a search has completed, '' before the first."""
    if not searched_at:
        return ''
    return '（搜于 {}）'.format(
        time.strftime('%H:%M:%S', time.localtime(searched_at)))


class SettingProperty(Enum):
    Mirror_Target = 1
    Mirror_Target_Name = 2
    Mirror_Quality = 3
    #: CoreAudio id of the aggregate we created, kept so a second run reuses
    #: it instead of stacking another one.
    Mirror_Audio_Aggregate = 4
    #: CoreAudio id of the user's real speakers, so「恢复原声音输出」can go back.
    Mirror_Audio_Original = 5
    #: 'cast' | 'caststream' | 'browser' | 'dlna' -- see OUTPUTS
    Mirror_Output = 6
    #: avfoundation video device index to capture; '' means "first screen".
    Mirror_Screen = 7
    #: draw the pointer. On by default; JustStream calls it「显示鼠标指针」.
    Mirror_Cursor = 8
    #: 'software' | 'hardware' (h264_videotoolbox, macOS only)
    Mirror_Encoder = 9
    #: key into DLNA_PROFILES -- which container/codec an old TV swallows is a
    #: property of the TV, so the user (and the watchdog) picks it.
    Mirror_Dlna_Profile = 10
    #: AVTransport control URL of the chosen DLNA renderer. Its own key
    #: because Mirror_Target holds `host:port` for a Chromecast.
    Mirror_Dlna_Control = 11


# -- ffmpeg ----------------------------------------------------------------

def find_ffmpeg():
    """Path to an ffmpeg binary, or None.

    PATH first, then the directories a Finder-launched app never sees.
    """
    found = shutil.which("ffmpeg")
    if found:
        return found
    for candidate in ("/opt/homebrew/opt/ffmpeg/bin/ffmpeg",
                      "/opt/homebrew/bin/ffmpeg",
                      "/usr/local/opt/ffmpeg/bin/ffmpeg",
                      "/usr/local/bin/ffmpeg",
                      "/opt/local/bin/ffmpeg"):
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


class _Capture(object):
    """What one probe decided: how to grab this machine's screen, and system
    audio too if a sink for it exists."""

    def __init__(self, label, inputs, audio_map=None, screens=None):
        self.label = label        # for the menu / logs
        self.inputs = inputs      # one list of input args per ffmpeg -i
        self.audio_map = audio_map  # '0:a:0' / '1:a:0' / None
        #: [(avfoundation index, device name)] for the display picker, empty
        #: where the platform has no list to offer (Windows/Linux).
        self.screens = screens or []


#: probe results are cached per (ffmpeg, platform): probing spawns ffmpeg, and
#: the menu must never block on it. The cursor and screen choices live in
#: `Setting`, so changing one has to clear this cache -- see
#: invalidate_capture_cache().
_capture_cache = {}


def invalidate_capture_cache():
    _capture_cache.clear()


def cursor_enabled():
    """Whether to draw the pointer. Default on -- and because Setting.get has
    side effects, "off" is the only value ever stored for this key."""
    return Setting.get(SettingProperty.Mirror_Cursor, True) is not False


def output_kind():
    """The chosen target, validated against OUTPUTS."""
    kind = str(Setting.get(SettingProperty.Mirror_Output, DEFAULT_OUTPUT)
               or DEFAULT_OUTPUT)
    return kind if kind in OUTPUTS else DEFAULT_OUTPUT


def encoder_kind():
    """'software' | 'hardware'; hardware only means anything on macOS."""
    kind = str(Setting.get(SettingProperty.Mirror_Encoder, 'software')
               or 'software')
    if kind != 'hardware' or sys.platform != 'darwin':
        return 'software'
    return 'hardware'


def dlna_profile(profile_id=None):
    """The compatibility shape in use for the DLNA target.

    Unknown or missing values fall back to the default rather than failing:
    the menu writes these, and a stale setting from a rolled-back plugin must
    not make the mirror refuse to start.
    """
    if profile_id is None:
        profile_id = str(Setting.get(SettingProperty.Mirror_Dlna_Profile,
                                     DEFAULT_DLNA_PROFILE)
                         or DEFAULT_DLNA_PROFILE)
    return DLNA_PROFILES.get(profile_id, DLNA_PROFILES[DEFAULT_DLNA_PROFILE])


def dlna_profile_id(profile):
    """The stored key for a profile object (the menu and the watchdog both
    need to name the current one to the user)."""
    for key, value in DLNA_PROFILES.items():
        if value is profile:
            return key
    return DEFAULT_DLNA_PROFILE


def profile_order():
    """Profiles in "try this next" order, starting after the current one."""
    keys = list(DLNA_PROFILES)
    current = dlna_profile_id(dlna_profile())
    return keys[keys.index(current) + 1:] + keys[:keys.index(current)]


def advertised_file(bitrate):
    """(size, 'H:MM:SS') claimed for the endless stream.

    The two numbers have to agree with each other and with the bitrate, or a
    renderer that cross-checks them sees a broken file. The size is capped
    below 2 GiB because some firmware treats Content-Length as a signed 32-bit
    integer and asks for `bytes=0-18446744072566584319`.
    """
    size = min(bitrate * 3600 // 8, DLNA_MAX_ADVERTISED_SIZE)
    seconds = max(1, size * 8 // bitrate)
    return size, '{}:{:02d}:{:02d}'.format(seconds // 3600,
                                           seconds % 3600 // 60, seconds % 60)


def protocol_info(profile):
    """The DIDL `protocolInfo` attribute for a DLNA profile."""
    parts = ['DLNA.ORG_OP=01', 'DLNA.ORG_CI=0',
             'DLNA.ORG_FLAGS={}'.format(DLNA_ORG_FLAGS)]
    if profile.org_pn:
        parts.insert(0, 'DLNA.ORG_PN={}'.format(profile.org_pn))
    return 'http-get:*:{}:{}'.format(profile.content_type, ';'.join(parts))


def content_features(profile):
    """The `contentFeatures.dlna.org` response header. Renderers that sniff it
    refuse the stream when it is missing, so it travels with every answer."""
    return ('DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS={}'.format(
        DLNA_ORG_FLAGS))


def build_didl(url, title, profile, size, duration):
    """DIDL-Lite for SetAVTransportURI.

    Both interpolations are escaped: `title` is this machine's hostname and
    `url` is ours while mirroring, but the same builder is used when a phone
    pushes a third-party URL through us -- an unescaped & or < there is a
    malformed envelope the TV blames on us.
    """
    from xml.sax.saxutils import escape
    return (
        '<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/" '
        'xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">'
        '<item id="0" parentID="-1" restricted="1">'
        '<dc:title>{title}</dc:title>'
        '<upnp:class>object.item.videoItem</upnp:class>'
        '<res protocolInfo="{pn}" size="{size}" duration="{duration}">'
        '{url}</res></item></DIDL-Lite>'
    ).format(title=escape(title), pn=escape(protocol_info(profile)),
             size=size, duration=escape(duration), url=escape(url))


def probe_capture(ffmpeg, platform=None, cursor=None):
    """Figure out the capture pipeline for this machine; None if impossible.

    `platform` and `cursor` are test seams; production always means
    sys.platform and the stored cursor preference.
    """
    platform = platform or sys.platform
    if cursor is None:
        cursor = cursor_enabled()
    key = (ffmpeg, platform, cursor)
    if key in _capture_cache:
        return _capture_cache[key]
    if platform == 'win32':
        capture = _Capture(
            'Desktop (GDI)',
            [['-f', 'gdigrab', '-framerate', str(FPS),
              '-draw_mouse', '1' if cursor else '0', '-i', 'desktop']])
    elif platform == 'darwin':
        capture = _probe_avfoundation(ffmpeg, cursor=cursor)
    else:
        capture = _probe_linux(ffmpeg, cursor=cursor)
    if capture is not None:
        _capture_cache[key] = capture
    return capture


#: What `ffmpeg -f avfoundation -list_devices true -i ""` really prints, on the
#: ffmpeg this plugin runs against (verified against 7.x on macOS 26):
#:
#:   [AVFoundation indev @ 0x775701c140] AVFoundation video devices:
#:   [AVFoundation indev @ 0x775701c140] [0] OBS Virtual Camera
#:   [AVFoundation indev @ 0x775701c140] AVFoundation audio devices:
#:   [AVFoundation indev @ 0x775701c140] [0] MacBook Pro麦克风
#:
#: Lower-case block names, and no quotes anywhere. Both details were wrong in
#: the first parser -- it split on 'Video devices:' and then kept only what
#: appeared between double quotes, so on a real Mac it returned two empty lists
#: and the whole darwin probe gave up. An older ffmpeg spelled the headers
#: `List of Video devices:` with `0) name` lines, which is still accepted.
_AVFOUNDATION_BLOCK = re.compile(r'(?:list of\s+)?(video|audio)\s+devices',
                                 re.I)
_AVFOUNDATION_DEVICE = re.compile(r'(?:\[\s*(\d+)\s*\]|(\d+)\s*\))\s*"?(.*?)"?\s*$')


def _avfoundation_lists(ffmpeg):
    """(video device names, audio device names) from -list_devices.

    The avfoundation input index is the position inside the respective list:
    ffmpeg numbers each block from zero with no holes, so the printed number and
    the position are the same thing -- the printed one is what is kept, so a
    future gap would show up as a wrong device rather than a wrong assumption.
    """
    try:
        proc = subprocess.run([ffmpeg, '-hide_banner', '-loglevel', 'info',
                               '-f', 'avfoundation', '-list_devices', 'true',
                               '-i', ''],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              timeout=10)
        text = proc.stdout.decode('utf-8', 'replace')
    except Exception as e:
        logger.error("cannot list avfoundation devices: %s", e)
        return [], []
    return _parse_avfoundation_lists(text)


def _parse_avfoundation_lists(text):
    """The two device lists, read out of ffmpeg's own log lines."""
    blocks = {'video': {}, 'audio': {}}
    current = None
    for line in str(text or '').splitlines():
        header = _AVFOUNDATION_BLOCK.search(line)
        if header:
            current = header.group(1).lower()
            continue
        if current is None:
            continue
        match = _AVFOUNDATION_DEVICE.search(line.strip())
        if not match:
            continue
        index = int(match.group(1) if match.group(1) is not None
                    else match.group(2))
        name = match.group(3).strip()
        if name:
            blocks[current][index] = name
    return ([blocks['video'][i] for i in sorted(blocks['video'])],
            [blocks['audio'][i] for i in sorted(blocks['audio'])])


def _probe_avfoundation(ffmpeg, cursor=True):
    videos, audios = _avfoundation_lists(ffmpeg)
    screens = [(index, name) for index, name in enumerate(videos)
               if 'capture screen' in name.lower()]
    if not screens and videos:
        screens = [(0, videos[0])]
    if not screens:
        return None
    wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
    screen = screens[0][0]
    if wanted.isdigit():
        for index, _name in screens:
            if str(index) == wanted:
                screen = index
                break
        else:
            logger.warning("screen %s is gone, falling back to %s",
                           wanted, screen)
    # macOS exposes no system-audio sink to avfoundation (FFmpeg's proposed
    # screencapturekit demuxer was never released); BlackHole is the open
    # source way to make one appear. Absent it, we mirror video only.
    blackhole = None
    for index, name in enumerate(audios):
        if 'blackhole' in name.lower():
            blackhole = index
            break
    base = ['-f', 'avfoundation', '-framerate', str(FPS),
            '-capture_cursor', '1' if cursor else '0']
    if blackhole is None:
        return _Capture('屏幕 (avfoundation)',
                        [base + ['-i', '{}:none'.format(screen)]],
                        screens=screens)
    return _Capture('屏幕 + 系统声音 (BlackHole)',
                    [base + ['-i', '{}:{}'.format(screen, blackhole)]],
                    audio_map='0:a:0', screens=screens)


def _default_pulse_monitor():
    """`<default sink>.monitor`, the PipeWire/PulseAudio system-audio tap."""
    try:
        proc = subprocess.run(['pactl', 'get-default-sink'],
                              stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, timeout=3)
        sink = proc.stdout.decode('utf-8', 'replace').strip()
    except Exception:
        return None
    return sink + '.monitor' if sink else None


def _probe_linux(ffmpeg, cursor=True):
    display = os.environ.get('DISPLAY')
    if not display:
        # x11grab cannot see a Wayland session; there is no ffmpeg-native
        # Wayland capture to fall back to.
        return None
    inputs = [['-f', 'x11grab', '-framerate', str(FPS),
               '-draw_mouse', '1' if cursor else '0', '-i',
               '{}+0,0'.format(display)]]
    monitor = _default_pulse_monitor()
    if monitor:
        inputs.append(['-f', 'pulse', '-i', monitor])
        return _Capture('屏幕 (X11) + 系统声音 (PulseAudio)',
                        inputs, audio_map='1:a:0')
    return _Capture('屏幕 (X11)', inputs)


def encoder_args(kind, platform=None):
    """Video encoder flags. Hardware encoding is opt-in and macOS-only:
    ffmpeg's h264_videotoolbox is the one tap Apple actually ships, and unlike
    the Castify reference (which never probes for it and always lands on CPU
    x264 on a Mac) we ask first -- see has_hardware_encoder()."""
    if kind == 'hardware' and (platform or sys.platform) == 'darwin':
        return ['-c:v', 'h264_videotoolbox', '-profile:v', 'high',
                '-level', '42', '-realtime', '1']
    return ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
            '-profile:v', 'high']


def has_hardware_encoder(ffmpeg, platform=None):
    """Whether this ffmpeg really offers h264_videotoolbox.

    Cached because it spawns ffmpeg, and the answer is only ever consulted
    from a background thread or the menu's status line. The platform is part
    of the key: the same binary answers differently under a different OS, and
    the caller may name one explicitly (tests, and any future cross-check).
    """
    key = (ffmpeg, platform or sys.platform)
    if key in _hw_encoder_cache:
        return _hw_encoder_cache[key]
    verdict = False
    if key[1] == 'darwin':
        try:
            proc = subprocess.run([ffmpeg, '-hide_banner', '-encoders'],
                                  stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, timeout=10)
            verdict = b'h264_videotoolbox' in proc.stdout
        except Exception as e:
            logger.info("cannot probe the encoders: %s", e)
    _hw_encoder_cache[key] = verdict
    return verdict


def build_ffmpeg_command(ffmpeg, capture, height, bitrate, kind=DEFAULT_OUTPUT,
                         encoder='software', profile=None):
    if kind == 'dlna':
        # A renderer of ten years ago is being handed this file, so the
        # profile -- not the user's quality menu -- decides the shape: the
        # height, the codec, the rate control and the container all have to be
        # ones that TV's demuxer knows.
        return build_dlna_command(ffmpeg, capture,
                                  profile or dlna_profile(), encoder)
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map and kind != 'caststream':
        cmd += ['-map', capture.audio_map,
                '-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2']
    else:
        # The mirroring app takes video only until its audio stream is wired
        # up, which is why the menu label for this target says 无声音 out loud.
        cmd += ['-an']
    extra = []
    if kind == 'caststream':
        # The OFFER promised a frame of exactly this size, so pin it and
        # letterbox rather than scaling by height and hoping.
        _width, height, video_filter = cast_stream_shape(height)
        bitrate = min(bitrate, CAST_STREAM_MAX_BITRATE)
        cmd += ['-vf', video_filter]
        if encoder == 'software':
            # `-aud` makes every picture start with an access unit delimiter,
            # which is what lets the splitter close a frame at its boundary
            # instead of one frame late -- at 24 fps that is 42 ms of the
            # latency this target exists to avoid. `scenecut=0` because x264
            # deciding on its own when to redraw is not a promise the OFFER's
            # one-second GOP can survive, and the PLI path assumes it.
            # `-aud` is an AVOption, not an ffmpeg flag: it takes its value as
            # a separate argument, so omitting the 1 eats the next option.
            extra = ['-aud', '1', '-x264-params',
                     'keyint={}:min_keyint={}:scenecut=0'.format(FPS, FPS)]
    elif height:
        # -2 keeps the aspect ratio and still satisfies yuv420p's even edges.
        cmd += ['-vf', 'scale=-2:{}'.format(height)]
    cmd += encoder_args(encoder)
    cmd += ['-pix_fmt', 'yuv420p', '-g', str(FPS), '-b:v', str(bitrate)]
    # Encoder private options only resolve after -c:v, so they ride at the end.
    cmd += extra
    cmd += OUTPUTS[kind][3]
    return cmd


def build_dlna_command(ffmpeg, capture, profile, encoder='software'):
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map:
        cmd += ['-map', capture.audio_map] + profile.audio_args
    else:
        cmd += ['-an']
    # setdar because a scaled desktop is not 4:3 just because the frame is.
    cmd += ['-vf', profile.video_filter() + ',setdar=16/9']
    if profile.video_args is not None:
        cmd += profile.video_args
    else:
        # The H.264 shapes: same encoder path as the other targets, with the
        # frame rate the profile advertises and a one-second GOP so a TV that
        # joins mid-file finds an IDR quickly.
        cmd += encoder_args(encoder)
        cmd += ['-pix_fmt', 'yuv420p', '-g', str(profile.fps),
                '-r', str(profile.fps), '-b:v', str(profile.bitrate)]
    cmd += profile.muxer + ['pipe:1']
    return cmd


def capture_unavailable_hint():
    if sys.platform == 'darwin':
        return 'ffmpeg 没有列出任何屏幕采集设备（avfoundation）'
    if sys.platform == 'win32':
        return '这个 ffmpeg 构建不支持 gdigrab'
    return ('没有 DISPLAY：x11grab 只认 X11 会话（Wayland 下 ffmpeg 无法截屏，'
            '本插件的三种目标都收不到画面；请切到 XWayland/X11 会话）')


# -- macOS 系统声音辅助：一键安装 BlackHole + 多输出设备 -----------------------
#
# A .pkg with a kernel-adjacent audio driver cannot be installed silently from
# an app -- installer.app always asks for the password. The most a "one click"
# can honestly do is: download the official pkg (verified against the sha256
# Homebrew's cask API publishes), open the GUI installer, poll until the
# device shows up in ffmpeg's list, then create the multi-output aggregate
# device and switch default output to it -- both via CoreAudio APIs, no
# clicking in Audio MIDI Setup. Any step that fails degrades to opening that
# pane with instructions.

BLACKHOLE_CASK_API = 'https://formulae.brew.sh/api/cask/blackhole-2ch.json'
#: Fallback if the cask API is unreachable (the GFW eats formulae.brew.sh
#: sometimes). Redownload only makes sense after bumping this pin.
BLACKHOLE_PKG_URL = 'https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg'
BLACKHOLE_PKG_SHA256 = ('57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef'
                        '9dccf851400dcf')
MACAST_AGGREGATE_UID = 'com.macast.screenmirror.output'
MACAST_AGGREGATE_NAME = 'Macast Screen Mirror'
#: kAudioObjectSystemObject == 1. (0x1000 is the hardware *model* object;
#: asking it for the device list answers 'nope'/kAudioHardwareUnknownPropertyError.)
SYSTEM_OBJECT = 1

#: Set while the assisted install runs; an Event because a plain bool would
#: need `global` in two places and menu clicks race on the UI thread.
_audio_setup_busy = threading.Event()


def blackhole_pkg_source(meta_json):
    """(url, sha256) from cask API bytes; the pinned pair if unusable."""
    try:
        data = json.loads(meta_json.decode('utf-8', 'replace'))
        url = data['url']
        sha = data['sha256']
        if (url.lower().endswith('.pkg') and isinstance(sha, str)
                and re.fullmatch(r'[0-9a-fA-F]{64}', sha)):
            return url, sha.lower()
    except Exception:
        pass
    return BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256


def _coreaudio():
    import ctypes
    ca = ctypes.CDLL('/System/Library/Frameworks/CoreAudio.framework'
                     '/CoreAudio')

    class Addr(ctypes.Structure):
        _fields_ = [('mSelector', ctypes.c_uint32),
                    ('mScope', ctypes.c_uint32),
                    ('mElement', ctypes.c_uint32)]

    # Declare every signature: an un-prototyped call in this build was enough
    # to make the framework reject correct addresses ('nope'/'!dat').
    ca.AudioObjectGetPropertyData.restype = ctypes.c_int               # OSStatus
    ca.AudioObjectGetPropertyData.argtypes = [
        ctypes.c_uint32, ctypes.POINTER(Addr), ctypes.c_uint32,
        ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32), ctypes.c_void_p]
    ca.AudioObjectSetPropertyData.restype = ctypes.c_int
    ca.AudioObjectSetPropertyData.argtypes = [
        ctypes.c_uint32, ctypes.POINTER(Addr), ctypes.c_uint32,
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    ca.AudioHardwareCreateAggregateDevice.restype = ctypes.c_int
    ca.AudioHardwareCreateAggregateDevice.argtypes = [ctypes.c_void_p,
                                                      ctypes.POINTER(ctypes.c_uint32)]
    ca._macast_addr_type = Addr
    return ca


def _fourcc(chars):
    """FourCC as the big-endian OSType value CoreAudio expects (no Carbon
    helper is worth pulling in for this)."""
    return int.from_bytes(chars, 'big')


def _address(ca, selector_chars, scope=b'glob', element=0):
    """An AudioObjectPropertyAddress; scope defaults to global."""
    Addr = ca._macast_addr_type
    return Addr(_fourcc(selector_chars), _fourcc(scope), element)


def _cf_to_str(pointer):
    """A CFStringRef returned by CoreAudio -> Python str (via pyobjc bridging).

    The +1 retain from the Get rule is deliberately leaked: the bridged
    NSString has no owner otherwise.
    """
    if not pointer:
        return None
    from objc import objc_object
    return str(objc_object(c_void_p=int(pointer)))


def _audio_devices():
    """[(device id, uid)] from kAudioHardwarePropertyDevices (all devices).

    Asks for the size with a real buffer: this framework build rejects the
    documented NULL-data size probe with '!dat'.
    """
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'devs')
    max_devices = 256
    buffer = (ctypes.c_uint32 * max_devices)()
    size = ctypes.c_uint32(max_devices * 4)
    if ca.AudioObjectGetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr), 0,
                                     None, ctypes.byref(size), buffer) != 0:
        return []
    uid_addr = _address(ca, b'deui')
    result = []
    for raw_id in buffer[:size.value // 4]:
        device_id = raw_id.value
        try:
            value = ctypes.c_void_p(0)
            uid_size = ctypes.c_uint32(8)
            if ca.AudioObjectGetPropertyData(device_id, ctypes.byref(uid_addr),
                                             0, None, ctypes.byref(uid_size),
                                             ctypes.byref(value)) != 0:
                continue
            uid = _cf_to_str(value.value)
            if uid:
                result.append((device_id, uid))
        except Exception:
            continue
    return result


def _default_output():
    """CoreAudio id of the current default output device, or None."""
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'dOut')
    size = ctypes.c_uint32(4)
    out = ctypes.c_uint32(0)
    if ca.AudioObjectGetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr), 0,
                                     None, ctypes.byref(size),
                                     ctypes.byref(out)) != 0:
        return None
    return out.value or None


def _set_default_output(device_id):
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'dOut')
    value = ctypes.c_uint32(int(device_id))
    return ca.AudioObjectSetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr),
                                         0, None, ctypes.sizeof(value),
                                         ctypes.byref(value)) == 0


def _create_aggregate(uids):
    """AudioHardwareCreateAggregateDevice; returns the new device id or None."""
    import ctypes
    try:
        from Foundation import NSMutableDictionary
    except ImportError:
        logger.error('pyobjc is unavailable: cannot create the aggregate device')
        return None
    ca = _coreaudio()
    sub_devices = []
    for uid in uids:
        one = NSMutableDictionary.dictionary()
        one['uid'] = uid
        one['isPrivate'] = False
        sub_devices.append(one)
    props = NSMutableDictionary.dictionary()
    props['name'] = MACAST_AGGREGATE_NAME
    props['uid'] = MACAST_AGGREGATE_UID
    props['deviceList'] = sub_devices
    # stacked=1 is the「多输出设备」checkbox: this device mixes to both sinks.
    props['stacked'] = True
    out = ctypes.c_uint32(0)
    status = ca.AudioHardwareCreateAggregateDevice(
        ctypes.c_void_p(props.pyobjc_id()), ctypes.byref(out))
    if status != 0:
        logger.error('AudioHardwareCreateAggregateDevice failed: %d', status)
        return None
    return out.value


def _device_uids(ffmpeg):
    """avfoundation names for both kinds; a freshly installed BlackHole shows
    up in the video *and* audio lists, so screen detection must not trust only
    one of them."""
    videos, audios = _avfoundation_lists(ffmpeg)
    return [n for n in videos if 'capture screen' not in n.lower()] + audios


def _find_blackhole(ffmpeg):
    for device_id, uid in _audio_devices():
        if 'blackhole' in uid.lower():
            return device_id, uid
    return None


def _has_blackhole(ffmpeg):
    return any('blackhole' in name.lower() for name in _device_uids(ffmpeg))


#: The five answers the assisted install can get, strongest first. Each one
#: takes a different branch, so the whole flow keys off this one value.
BH_STATES = ('capturable', 'loaded', 'on-disk', 'stale-receipt', 'absent')


def _blackhole_state(ffmpeg):
    """Where BlackHole is visible right now, as one of ``BH_STATES``.

    Three of these branches exist because the two witnesses disagree, and every
    one of them used to take the same action (download the pkg again):
    ``capturable`` -- ffmpeg's own device list, the only one that proves audio
        will actually be captured;
    ``loaded`` -- CoreAudio has the device but avfoundation does not, so the
        driver is alive and the blocker is the microphone permission;
    ``on-disk`` -- HAL driver files landed but the daemon never loaded them,
        which is what a reinstall can never fix;
    ``stale-receipt`` / ``absent`` -- the two cases an installer is for.
    """
    try:
        if _has_blackhole(ffmpeg):
            return 'capturable'
    except Exception as e:
        logger.info('avfoundation blackhole probe failed: %s', e)
    try:
        if _find_blackhole(ffmpeg) is not None:
            return 'loaded'
    except Exception as e:
        logger.info('CoreAudio blackhole probe failed: %s', e)
    if _blackhole_driver_installed():
        return 'on-disk'
    if _blackhole_receipt_present():
        return 'stale-receipt'
    return 'absent'


def blackhole_pkg_pair():
    """(url, sha256, source) — the cask API first, the pinned pair as fallback.

    The source string belongs on the progress page: "为什么下的是这个版本"
    deserves an answer when the answer is "拉不到 Homebrew 的公告，用了内置的".
    """
    import requests
    try:
        meta = requests.get(BLACKHOLE_CASK_API, timeout=10).content
    except Exception as e:
        logger.info('cask API unavailable (%s), using the pinned pkg', e)
        return BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256, '内置钉住版本（cask API 不可达）'
    url, sha = blackhole_pkg_source(meta)
    if (url, sha) == (BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256):
        return url, sha, 'Homebrew cask API（与钉住版本一致）'
    return url, sha, 'Homebrew cask API'


def _pkg_tmpdir():
    """A writable temp dir. The plain mkdtemp fails with ENOENT when the app
    inherited a TMPDIR pointing at a since-deleted CLI-sandbox directory
    (launching the .app binary from a tool shell does exactly that)."""
    import tempfile
    try:
        return tempfile.mkdtemp(prefix='macast-blackhole-')
    except OSError:
        fallback = os.path.join(os.path.expanduser('~/Library/Caches/Macast'),
                                'blackhole-tmp')
        os.makedirs(fallback, exist_ok=True)
        return tempfile.mkdtemp(prefix='macast-blackhole-', dir=fallback)


#: existential.audio answers 406 Not Acceptable to the default python-requests
#: User-Agent (verified: same URL, browser UA -> 200). The pkg itself is a
#: public download; presenting as a browser is the only way in.
PKG_USER_AGENT = ('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
                  'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 '
                  'Safari/537.36')


def _fetch_blackhole_pkg(url, on_bytes=None):
    """Stream the official pkg to a temp file; return its path.

    `on_bytes(done, total)` exists for the progress page -- a several-MB
    download over a slow link looked identical to a hang without it.
    """
    import requests
    path = os.path.join(_pkg_tmpdir(), 'BlackHole2ch.pkg')
    try:
        with requests.get(url, stream=True, timeout=60,
                          headers={'User-Agent': PKG_USER_AGENT}) as response:
            response.raise_for_status()
            total = int(response.headers.get('Content-Length') or 0)
            done = 0
            with open(path, 'wb') as handle:
                for chunk in response.iter_content(65536):
                    handle.write(chunk)
                    done += len(chunk)
                    if on_bytes is not None:
                        on_bytes(done, total)
    except Exception:
        _remove_quietly(path)
        raise
    return path


def verify_blackhole_pkg(path, expected_sha):
    """True when the file hashes to what Homebrew publishes for it.

    A mismatch deletes the file (the caller treats False as fatal): an
    unverified audio driver must never reach `open`.
    """
    import hashlib
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(65536), b''):
            digest.update(block)
    if digest.hexdigest() != expected_sha:
        _remove_quietly(path)
        return False
    return True


HAL_DRIVER_GLOBS = ('/Library/Audio/Plug-Ins/HAL/BlackHole*.driver',)
BLACKHOLE_PKG_IDS = ('audio.existential.BlackHole2ch',
                     'audio.existential.BlackHole16ch')


def _blackhole_driver_installed():
    """Driver *files* on disk -- weaker than "the device exists", and that
    gap is the whole story of one real failure mode: pkgutil keeps a receipt
    after the files are gone (so a reinstall is needed), and the official pkg
    does not restart coreaudiod after installing (so files can sit there
    forever without the daemon ever loading them)."""
    import glob
    return any(glob.glob(pattern) for pattern in HAL_DRIVER_GLOBS)


def _blackhole_receipt_present():
    try:
        res = subprocess.run(['pkgutil', '--pkgs'], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, timeout=15)
    except Exception:
        return False
    installed = set(res.stdout.split())
    return any(pkg in installed for pkg in BLACKHOLE_PKG_IDS)


def _reload_coreaudiod():
    """(ok, why). Restart the audio daemon with an admin prompt so an
    installed-but-unloaded HAL driver gets picked up.

    Deliberately its own password round-trip rather than silent sabotage:
    killing coreaudiod drops everyone's audio for a second, and macOS gives
    no way to do it without authorization. The pkg installer already asked
    for a password minutes earlier, so this prompt is expected, not a surprise.
    """
    script = ('do shell script "/bin/kill -TERM $(/usr/bin/pgrep -x coreaudiod)" '
              'with administrator privileges '
              'with prompt "Macast 需要重载音频服务，让刚安装的 BlackHole 驱动生效"')
    try:
        res = subprocess.run(['osascript', '-e', script],
                             stdout=subprocess.DEVNULL,
                             stderr=subprocess.PIPE, text=True, timeout=180)
    except Exception as e:
        return False, '无法执行重载：{}'.format(e)
    if res.returncode == 0:
        return True, '音频服务已重载'
    err = (res.stderr or '').strip()
    if '-128' in err:
        return False, '你取消了密码框'
    return False, err or '重载失败'


def _remove_quietly(path):
    try:
        os.remove(path)
    except OSError:
        pass


def _wait_for_blackhole(ffmpeg, timeout=300.0, progress=None, interval=5.0,
                        step='wait', note='请在安装器里点「安装」并输入密码'):
    """Poll until a witness sees the device, and report *which* one did.

    'capturable' / 'loaded' / '' (timeout). Keeping the two apart is the whole
    point: 'loaded' means the daemon picked the driver up but capture still
    cannot open it, and calling that a success would be a lie of exactly the
    kind this flow used to tell.

    The ticking message matters: this is the step where the user is supposed
    to be clicking through installer.app, and a silent 5 minutes reads exactly
    like a hang.
    """
    def _seen():
        state = _blackhole_state(ffmpeg)
        return state if state in ('capturable', 'loaded') else ''

    deadline = time.time() + timeout
    while time.time() < deadline:
        state = _seen()
        if state:
            return state
        time.sleep(min(interval, max(deadline - time.time(), 0.1)))
        if progress is not None:
            waited = timeout - max(deadline - time.time(), 0.0)
            progress.sub(step, min(waited / timeout, 0.99),
                         '已等 {:.0f} 秒：{}'.format(waited, note))
    return _seen()


def _reload_until_visible(ffmpeg, progress):
    """One admin password, then wait for the daemon to load the driver.

    Returns ``(state, why)`` with state in ('capturable', 'loaded', ''). The
    caller only needs the state to decide whether to keep going, but the user
    needs `why`: "you cancelled the password box" and "it reloaded and the
    device still isn't there" are different problems with different fixes, and
    conflating them is how this flow ended up blaming the user's reboot.
    """
    progress.enter('reload', '需要一次管理员密码')
    reloaded, why = _reload_coreaudiod()
    if not reloaded:
        progress.fail('reload', why)
        return '', why
    state = _wait_for_blackhole(ffmpeg, timeout=45.0, progress=progress,
                                interval=3.0, step='reload',
                                note='音频服务正在重启，等它把驱动读进来')
    if state:
        progress.leave('reload', '{}，设备已出现'.format(why))
    else:
        progress.fail('reload', '重载后仍然看不到 BlackHole 设备')
        state = ''
        why = '重载音频服务后仍然看不到 BlackHole 设备'
    return state, why


def _route_audio_through_blackhole(ffmpeg, progress=None):
    """Create/reuse the multi-output aggregate and make it the default output.

    Without it, installing BlackHole and selecting it as the output means the
    user hears nothing; the aggregate feeds BlackHole *and* the speakers.
    Returns True on success, False if the caller should show manual steps.
    Each False path fails the exact step that gave up, so the page never says
    "创建设备失败" when the device was created and the switch is what broke.
    """
    if progress is None:
        progress = _NullProgress()
    progress.enter('aggregate')
    try:
        devices = _audio_devices()
    except Exception as e:
        logger.error('CoreAudio query failed: %s', e)
        progress.fail('aggregate', 'CoreAudio 查询失败：{}'.format(e))
        return False
    if not devices:
        progress.fail('aggregate', 'CoreAudio 没有返回任何设备')
        return False
    blackhole = _find_blackhole(ffmpeg)
    aggregate_id = Setting.get(SettingProperty.Mirror_Audio_Aggregate, '') or ''
    for device_id, uid in devices:
        if uid == MACAST_AGGREGATE_UID:
            aggregate_id = device_id
            break
    if aggregate_id:
        for device_id, _uid in devices:
            if device_id == int(aggregate_id):
                progress.leave('aggregate', '复用已有聚合设备')
                progress.enter('output')
                if not _set_default_output_and_remember(device_id):
                    progress.fail('output', '切换到默认输出失败')
                    return False
                return True
    if blackhole is None:
        # Installed but CoreAudio has not surfaced it by UID yet.
        progress.fail('aggregate', 'BlackHole 尚未在 CoreAudio 设备表中出现')
        return False
    speakers = _default_output()
    if speakers is None:
        speakers = next((device_id for device_id, uid in devices
                         if 'blackhole' not in uid.lower()), None)
    if speakers is None or speakers == blackhole[0]:
        progress.fail('aggregate', '找不到可配对的扬声器输出')
        return False
    created = _create_aggregate([blackhole[1], dict(devices)[speakers]])
    if created is None:
        progress.fail('aggregate', '系统拒绝创建多输出聚合设备')
        return False
    progress.leave('aggregate', '已创建多输出设备')
    progress.enter('output')
    if not _set_default_output_and_remember(created):
        progress.fail('output', '切换到默认输出失败')
        return False
    return True


def _set_default_output_and_remember(device_id):
    if not Setting.has(SettingProperty.Mirror_Audio_Original):
        current = _default_output()
        if current is not None:
            Setting.set(SettingProperty.Mirror_Audio_Original, current)
    if not _set_default_output(device_id):
        return False
    Setting.set(SettingProperty.Mirror_Audio_Aggregate, device_id)
    return True


def _open_audio_midi_setup(report):
    report('降级为手动：已打开「音频 MIDI 设置」，请创建多输出设备并勾选 BlackHole + 你的扬声器')
    try:
        subprocess.run(['open', '-a', 'Audio MIDI Setup'],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=10)
    except Exception:
        pass


#: The assisted-install run plan, in order. 'reload' only runs when the driver
#: files landed but coreaudiod never picked them up -- the pkg's own
#: postinstall only chmods, and a driver installed into a running session can
#: stay invisible to every enumeration until the daemon restarts.
AUDIO_STEPS = (
    ('env', '环境自检'),
    ('probe', '检测 BlackHole 状态'),
    ('meta', '获取官方安装包信息'),
    ('download', '下载安装包'),
    ('verify', '校验 sha256'),
    ('install', '打开安装器'),
    ('wait', '等待设备安装完成'),
    ('reload', '重载音频服务'),
    ('aggregate', '创建/复用多输出设备'),
    ('output', '切换默认输出'),
)


class _NullProgress(object):
    """The no-op twin of _SetupProgress, for callers without a page.

    Same surface, so setup_system_audio never has to ask which one it got --
    including the except-path sweep over snapshot()['steps']."""

    def enter(self, step_id, note=''):
        pass

    def sub(self, step_id, frac, note=None):
        pass

    def leave(self, step_id, note=''):
        pass

    def skip(self, step_id, note=''):
        pass

    def fail(self, step_id, note=''):
        pass

    def finish(self, ok):
        pass

    def snapshot(self):
        return {'pct': 0.0, 'message': '', 'done': False, 'ok': None,
                'steps': []}


class _SetupProgress(object):
    """Step states + overall percent for the assisted install.

    One object, read from the HTTP handler thread and written from the worker;
    a plain lock around the dict copy keeps snapshots coherent without dragging
    in a framework. Steps already finished refuse re-entry -- a flow that
    re-opened a done step would make the bar run backwards.
    """

    def __init__(self, steps=AUDIO_STEPS):
        self._lock = threading.Lock()
        self._steps = [{'id': sid, 'label': label, 'state': 'pending',
                        'pct': None, 'note': ''} for sid, label in steps]
        self.message = ''
        self.done = False
        self.ok = None

    def _get(self, step_id):
        for st in self._steps:
            if st['id'] == step_id:
                return st
        return None

    def enter(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] in ('done', 'fail', 'skipped'):
                return
            st['state'] = 'running'
            st['note'] = note
            self.message = '{}：{}'.format(st['label'], note) if note else st['label']

    def sub(self, step_id, frac, note=None):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] != 'running':
                return
            st['pct'] = frac
            if note is not None:
                st['note'] = note
                self.message = '{}：{}'.format(st['label'], note)

    def leave(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] == 'fail':
                return
            st['state'] = 'done'
            st['pct'] = 1.0
            if note:
                st['note'] = note

    def skip(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] in ('done', 'fail'):
                return
            st['state'] = 'skipped'
            st['pct'] = 1.0
            if note:
                st['note'] = note

    def fail(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None:
                return
            st['state'] = 'fail'
            if note:
                st['note'] = note
                self.message = '{}失败：{}'.format(st['label'], note)

    def overall(self):
        weighted = {'done': 1.0, 'skipped': 1.0, 'fail': 1.0,
                    'running': None, 'pending': 0.0}
        total = 0.0
        count = 0
        for st in self._steps:
            if st['state'] == 'skipped':
                continue
            count += 1
            frac = weighted[st['state']]
            if frac is None:
                frac = st['pct'] if st['pct'] is not None else 0.15
            total += frac
        return total / count if count else 0.0

    def finish(self, ok):
        with self._lock:
            self.done = True
            self.ok = bool(ok)

    def snapshot(self):
        with self._lock:
            return {
                'pct': self.overall(),
                'message': self.message,
                'done': self.done,
                'ok': self.ok,
                'steps': [dict(st) for st in self._steps],
            }


AUDIO_PROGRESS_PAGE = """<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Macast · 系统声音一键设置进度</title>
<style>
 body{font-family:-apple-system,system-ui,sans-serif;background:#141821;color:#e8ecf3;
      margin:0;padding:28px 20px;}
 .wrap{max-width:640px;margin:0 auto;}
 h1{font-size:18px;font-weight:600;margin:0 0 14px;}
 .bar{height:10px;background:#232a38;border-radius:6px;overflow:hidden;margin:0 0 6px;}
 .fill{height:100%;width:0;background:#3b82f6;transition:width .5s;}
 #pct{font-size:12px;color:#93a0b5;margin:0 0 4px;}
 #msg{font-size:13px;color:#c8d2e0;min-height:20px;margin:8px 0 18px;}
 ul{list-style:none;padding:0;margin:0;}
 li{display:flex;gap:10px;align-items:baseline;padding:7px 0;
    border-bottom:1px solid #1d2431;font-size:14px;}
 li .sym{width:1.2em;flex:none;text-align:center;}
 li.run .sym{color:#3b82f6;} li.ok .sym{color:#22c55e;}
 li.bad .sym{color:#ef4444;} li.off .sym,li.pend{color:#5b6779;}
 li .label{font-weight:600;flex:none;}
 li .note{color:#93a0b5;font-size:12px;}
 .foot{color:#5b6779;font-size:12px;margin-top:16px;}
</style></head><body><div class="wrap">
<h1>系统声音一键设置（BlackHole + 多输出设备）</h1>
<div class="bar"><div class="fill" id="fill"></div></div>
<p id="pct">0%</p>
<p id="msg">正在连接…</p>
<ul id="steps"></ul>
<p class="foot">本页面只由 Macast 在本机提供，设置结束后会自动失效；此窗口可以直接关闭。</p>
</div>
<script>
var TOKEN = '@TOKEN@';
var last = null;
function sym(st){
  return {running:'▶', done:'✓', fail:'✗', skipped:'—', pending:'○'}[st] || '?';
}
function cls(st){
  return {running:'run', done:'ok', fail:'bad', skipped:'off pend', pending:'pend'}[st] || '';
}
function render(s){
  document.getElementById('fill').style.width = Math.round(s.pct*100) + '%';
  document.getElementById('pct').textContent = Math.round(s.pct*100) + '%';
  document.getElementById('msg').textContent =
      s.done ? (s.ok ? '全部完成：开始镜像后系统声音会一起投出去。' : '未完成，请见上方红色步骤。')
             : (s.message || '进行中…');
  var list = document.getElementById('steps');
  list.textContent = '';
  s.steps.forEach(function(st){
    var li = document.createElement('li');
    li.className = cls(st.state);
    var a = document.createElement('span'); a.className = 'sym';
    a.textContent = sym(st.state);
    var b = document.createElement('span'); b.className = 'label';
    b.textContent = st.label;
    li.appendChild(a); li.appendChild(b);
    if (st.note){
      var c = document.createElement('span'); c.className = 'note';
      c.textContent = st.note;
      li.appendChild(c);
    }
    list.appendChild(li);
  });
  last = s;
}
function tick(){
  fetch('/state?token=' + encodeURIComponent(TOKEN))
    .then(function(r){ return r.json(); })
    .then(render)
    .catch(function(){ document.getElementById('msg').textContent = '进度服务已关闭（设置已结束）。'; })
    .then(function(){ setTimeout(tick, last && last.done ? 5000 : 1000); });
}
tick();
</script></body></html>
"""


class _AudioProgressHandler(BaseHTTPRequestHandler):
    """Loopback-only viewer for one running assisted install.

    Same credential shape as the mirror pages (AGENTS.md 4.8): a per-run
    random token in the URL, never the app's stable Api_Token; every value
    the page shows goes through textContent; the body is ours end to end, so
    nothing user- or network-controlled is interpolated.
    """

    def _authorized(self):
        import hmac
        import urllib.parse
        query = urllib.parse.parse_qs(self.path.partition('?')[2] or '')
        token = (query.get('token') or [''])[0]
        return bool(token) and hmac.compare_digest(token, self.server.token)

    def do_GET(self):
        if not self._authorized():
            self.send_error(403, 'a token is required')
            return
        path = self.path.partition('?')[0]
        if path == '/state':
            body = json.dumps(self.server.progress.snapshot()).encode('utf-8')
            ctype = 'application/json'
        elif path in ('/', '/index.html'):
            body = AUDIO_PROGRESS_PAGE.replace(
                '@TOKEN@', self.server.token).encode('utf-8')
            ctype = 'text/html; charset=utf-8'
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET() if self.path.partition('?')[0] == '/state' \
            else self.send_error(405)

    def log_message(self, fmt, *args):
        pass


#: Kept between runs so a second click can replace the previous page's server
#: instead of stacking ports.
_audio_progress = None
_audio_server = None
_audio_server_timer = None


def _open_audio_progress(progress):
    """Start the loopback progress page and open it; '' when unavailable.

    A failure here must not fail the install -- the notifications still carry
    every step, the page is the comfortable view, not the only one.
    """
    server = ThreadingHTTPServer(('127.0.0.1', 0), _AudioProgressHandler)
    server.progress = progress
    server.token = secrets.token_hex(8)
    server.daemon_threads = True
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name='SCREEN_MIRROR_AUDIO_HTTP').start()
    url = 'http://127.0.0.1:{}{}?token={}'.format(
        server.server_address[1], '/', server.token)
    try:
        subprocess.run(['open', url], timeout=10)
    except Exception as e:
        logger.info('cannot open the progress page: %s', e)
    return url, server


def _close_audio_server():
    global _audio_server, _audio_server_timer
    if _audio_server_timer is not None:
        _audio_server_timer.cancel()
        _audio_server_timer = None
    server, _audio_server = _audio_server, None
    if server is not None:
        try:
            server.shutdown()
            server.server_close()
        except Exception:
            pass


def _schedule_audio_server_close(delay=300.0):
    """Leave the finished page readable, then stop serving."""
    global _audio_server_timer
    if _audio_server_timer is not None:
        _audio_server_timer.cancel()
    _audio_server_timer = threading.Timer(delay, _close_audio_server)
    _audio_server_timer.daemon = True
    _audio_server_timer.start()


def setup_system_audio(report=lambda message: None, progress=None):
    """One-click: install BlackHole if absent, then route audio through it.

    Runs in a worker thread -- it spawns ffmpeg, waits on the installer, and
    must never touch the UI thread. `progress` (a _SetupProgress) is the
    step-by-step account; everything still reports one-liners through `report`
    because the page can be closed and the notifications must stay sufficient.
    """
    if progress is None:
        progress = _NullProgress()
    progress.enter('env')
    if sys.platform != 'darwin':
        progress.fail('env', '辅助安装仅适用于 macOS')
        report('辅助安装仅适用于 macOS')
        return False
    ffmpeg = find_ffmpeg()
    if ffmpeg is None:
        progress.fail('env', '找不到 ffmpeg')
        report('找不到 ffmpeg：先 brew install ffmpeg')
        return False
    progress.leave('env', 'ffmpeg: {}'.format(ffmpeg))
    try:
        progress.enter('probe')
        state = _blackhole_state(ffmpeg)
        capturable = state == 'capturable'
        if state in ('capturable', 'loaded'):
            # The driver is loaded. Whatever else is wrong, downloading another
            # pkg cannot fix it -- that is the loop the user reported.
            progress.leave('probe', '设备已在，跳过安装' if capturable else
                           'CoreAudio 里已有 BlackHole，但 ffmpeg 的采集设备表读不到它：'
                           '不重装（重装改变不了这件事），先把多输出设备接好')
            for _inert in ('meta', 'download', 'verify', 'install', 'wait',
                           'reload'):
                progress.skip(_inert)
        elif state == 'on-disk':
            progress.leave('probe', '驱动文件在磁盘上但音频服务没有加载它（官方 .pkg 的 '
                                    'postinstall 只改权限、从不重启 coreaudiod）：'
                                    '跳过下载与安装，只做一次重载')
            for _inert in ('meta', 'download', 'verify', 'install', 'wait'):
                progress.skip(_inert)
            state, why = _reload_until_visible(ffmpeg, progress)
            if not state:
                report('一键设置没有完成：{}。驱动已经在磁盘上了，再点一次只会重试重载，'
                       '不会重新下载安装包；重启一次电脑也能达到同样效果'.format(why))
                return False
            capturable = state == 'capturable'
        else:
            progress.leave('probe', (
                '检测到残留的安装记录，但驱动文件已不在磁盘上：'
                '将重新下载官方安装包（这就是上次"装了却没有设备"的原因）'
                if state == 'stale-receipt' else
                '本机没有 BlackHole，将安装官方 2ch 驱动'))
            progress.enter('meta')
            url, expected, source = blackhole_pkg_pair()
            progress.leave('meta', '{}：{}'.format(source, url.rsplit('/', 1)[-1]))
            progress.enter('download')

            def _on_bytes(done, total):
                mb = '{} / {} MB'.format(done / 1048576.0, total / 1048576.0) \
                    if total else '{} KB'.format(done / 1024.0)
                progress.sub('download', (done / total) if total else None, mb)

            pkg = _fetch_blackhole_pkg(url, on_bytes=_on_bytes)
            progress.leave('download')
            progress.enter('verify')
            if not verify_blackhole_pkg(pkg, expected):
                progress.fail('verify', '下载到的文件与官方公布的 sha256 不一致，已删除')
                report('BlackHole 安装包校验失败（sha256 不匹配），已中止')
                return False
            progress.leave('verify', '哈希一致')
            progress.enter('install')
            subprocess.run(['open', pkg], timeout=10)
            progress.leave('install', '安装器已打开，请在弹窗里点「安装」并输入一次密码')
            report('正在打开安装器：请在弹窗里点「安装」并输入一次密码')
            progress.enter('wait')
            state = _wait_for_blackhole(ffmpeg, progress=progress)
            if not state and _blackhole_driver_installed():
                progress.sub('wait', None,
                             '驱动文件已就位但 CoreAudio 没加载它，尝试重载音频服务')
                state, _why = _reload_until_visible(ffmpeg, progress)
            if not state:
                progress.fail('wait', '超时仍未在设备列表里看到 BlackHole')
                report('没有检测到 BlackHole 设备：可能是安装被取消、密码未通过。'
                       '若驱动文件已经落盘，再点一次一键设置不会重新下载，'
                       '而是直接重试重载音频服务（或重启一次电脑）；'
                       '也可手动执行 brew install --cask blackhole-2ch')
                return False
            progress.leave('wait', '设备已出现')
            capturable = state == 'capturable'
        report('正在创建多输出设备并切换默认输出…')
        if not _route_audio_through_blackhole(ffmpeg, progress=progress):
            _open_audio_midi_setup(report)
            return False
        progress.leave('output')
        _capture_cache.clear()
        if capturable:
            report('系统声音已就绪：开始镜像后声音会一起投出去，本机照常能听到')
            return True
        # The aggregate is built and CoreAudio is happy, but the capture side
        # still cannot open the device. Re-marking the *finished* check as
        # failed is deliberate: this run's real outcome is that negative answer,
        # and a green page that leaves the user with no audio is the exact
        # "看起来装完了" illusion this whole branch exists to stop.
        progress.fail('probe', 'CoreAudio 已加载设备，但 ffmpeg 的采集设备表读不到它')
        report('多输出设备已建好，但采集侧仍然读不到 BlackHole，系统声音还不能用。'
               '这通常是 macOS 的麦克风权限：产物里的 Macast.app 若没有 '
               'NSMicrophoneUsageDescription，系统连授权弹窗都不会给。'
               '请在「系统设置 → 隐私与安全性 → 麦克风」里允许 Macast 并重启 Macast，'
               '再点一次一键设置（不会重新下载安装包）')
        return False
    except Exception as e:
        logger.error('blackhole assisted install failed: %s', e, exc_info=True)
        for st in progress.snapshot()['steps']:
            if st['state'] == 'running':
                progress.fail(st['id'], str(e))
                break
        report('一键设置失败：{}；可在「音频 MIDI 设置」里手动添加多输出设备'.format(e))
        return False


def _audio_setup_worker():
    """Worker for the menu item: run the assisted setup against a fresh step
    machine, serve the progress page on loopback while it runs, then clear the
    in-progress flag and schedule the page's retirement no matter how it ends.
    The page is a convenience -- if it cannot start, the install still runs
    and every step stays in the notifications."""
    global _audio_progress, _audio_server

    def _report(message):
        cherrypy.engine.publish('app_notify', 'Macast', message, sound=False)

    progress = _SetupProgress()
    _audio_progress = progress
    _close_audio_server()
    try:
        try:
            url, _audio_server = _open_audio_progress(progress)
            _report('已在本机打开详细进度页，可实时查看每一步：{}'.format(url))
        except Exception as e:
            logger.info('audio progress page unavailable: %s', e)
        ok = setup_system_audio(_report, progress=progress)
    except Exception as e:
        logger.error('audio setup worker crashed: %s', e, exc_info=True)
        ok = False
    finally:
        progress.finish(ok)
        _schedule_audio_server_close()
        _audio_setup_busy.clear()


def audio_setup_needed():
    """False only once a probe has actually produced system audio; the menu
    must not spawn ffmpeg to decide (it runs on the UI thread)."""
    capture = next(iter(_capture_cache.values()), None)
    return capture is None or capture.audio_map is None


def restore_system_audio(report=lambda message: None):
    """Point default output back at the speakers saved before routing."""
    original = Setting.get(SettingProperty.Mirror_Audio_Original, '') or ''
    if original == '':
        return False
    try:
        if _set_default_output(int(original)):
            report('已恢复原声音输出')
            return True
    except Exception as e:
        logger.error('cannot restore the audio output: %s', e)
    report('恢复失败：原输出设备可能已拔出')
    return False


# -- live stream HTTP server ------------------------------------------------
#
# One server, one encoder pipe, two consumers:
#   * a Chromecast LOADs the MPEG-TS URL we hand it;
#   * a browser opens /browser and feeds the fragmented-MP4 URL into MSE.
# Both URLs carry a per-session random id. That is deliberate: this port is
# reachable by every host on the LAN, a live mirror of someone's desktop is
# not something to hand to "anything that can open a socket", and a TV cannot
# present a token -- but it can be given a URL it invented nothing about.

class _Broadcaster(object):
    """Fan out encoder output to every connected client; drop for the slow."""

    def __init__(self, maxsize=256, ring_bytes=REPLAY_BYTES, init_marker=None):
        self._maxsize = maxsize
        self._ring_limit = ring_bytes
        #: bytes of the container header (fMP4: everything before the first
        #: `moof`). A late joiner needs it or MSE cannot start at all.
        self._init_marker = init_marker
        self._init = b''
        self._init_done = init_marker is None
        self._ring = deque()
        self._ring_bytes = 0
        self._subs = set()
        self._lock = threading.Lock()
        self.chunks = 0
        self.bytes = 0
        self.drops = 0

    @property
    def init_segment(self):
        with self._lock:
            return self._init if self._init_done else b''

    def subscribe(self, replay=False):
        q = Queue(maxsize=self._maxsize)
        if replay:
            if self._init_marker is not None:
                init = self.init_segment
                if init:
                    q.put_nowait(init)      # fresh subscriber, cannot be full
            for chunk in self.tail():
                try:
                    q.put_nowait(chunk)
                except Exception:
                    break
        with self._lock:
            self._subs.add(q)
        return q

    def unsubscribe(self, q):
        with self._lock:
            self._subs.discard(q)

    def tail(self):
        with self._lock:
            return list(self._ring)

    def feed(self, chunk):
        with self._lock:
            self.chunks += 1
            self.bytes += len(chunk)
            self._retain(chunk)
            subs = list(self._subs)
        for q in subs:
            try:
                q.put_nowait(chunk)
            except Exception:
                try:
                    q.get_nowait()      # live beats lossless: drop the oldest
                    q.put_nowait(chunk)
                    self.drops += 1
                except Exception:
                    pass

    def _retain(self, chunk):
        """Keep the header, then a bounded tail. Called under the lock."""
        if not self._init_done:
            self._init += chunk
            cut = self._init.find(self._init_marker)
            if cut < 0:
                if len(self._init) > 1 << 21:
                    # No marker in 2 MB: not the container we expect. Stop
                    # hoarding, and let bytes be treated as media from here.
                    self._init, self._init_done = b'', True
                # Everything held so far is the header, which replay serves
                # from `init_segment` -- ringing it too would hand a late joiner
                # the first fragment twice.
                return
            chunk = self._init[cut:]
            self._init = self._init[:cut]
            self._init_done = True
        if not self._ring_limit:
            return
        self._ring.append(chunk)
        self._ring_bytes += len(chunk)
        while self._ring_bytes > self._ring_limit and len(self._ring) > 1:
            self._ring_bytes -= len(self._ring.popleft())

    def clients(self):
        with self._lock:
            return len(self._subs)


class _ByteLog(object):
    """Encoder output kept addressable by **absolute byte offset**.

    A DLNA renderer is not watching a live stream: it was handed a URL it
    believes is a 1.9 GB file, so it closes the connection mid-way and comes
    back with `Range: bytes=14680064-` as if nothing happened. Unlike the
    queue broadcaster, nothing here is dropped for being slow -- the last 48
    MiB of produced bytes stay readable, and a reader that wants bytes the
    encoder has not made yet waits for them.
    """

    def __init__(self, limit=DLNA_RING_BYTES, pad=PS_PADDING):
        self._limit = limit
        self._pad = pad
        self._chunks = deque()
        self._held = 0
        self._start = 0                 # absolute offset of _chunks[0]
        self._end = 0                   # absolute offset of the next byte
        self._cond = threading.Condition()
        self._closed = False
        self._readers = 0
        #: Same names as _Broadcaster, so stats() can read either.
        self.chunks = 0
        self.bytes = 0
        self.drops = 0                  # chunks that aged out of the window

    def feed(self, chunk):
        with self._cond:
            self.chunks += 1
            self.bytes += len(chunk)
            self._end += len(chunk)
            self._chunks.append(chunk)
            self._held += len(chunk)
            while self._held > self._limit and len(self._chunks) > 1:
                evicted = self._chunks.popleft()
                self._held -= len(evicted)
                self._start += len(evicted)
                self.drops += 1
            self._cond.notify_all()

    def close(self):
        with self._cond:
            self._closed = True
            self._cond.notify_all()

    @property
    def start(self):
        with self._cond:
            return self._start

    @property
    def end(self):
        with self._cond:
            return self._end

    def clients(self):
        with self._cond:
            return self._readers

    def reader_enter(self):
        with self._cond:
            self._readers += 1
            return self._readers

    def reader_leave(self):
        with self._cond:
            self._readers = max(0, self._readers - 1)

    def anchor(self, prefill=DLNA_PREFILL_BYTES):
        """Where the advertised file's offset 0 sits.

        `prefill` bytes behind the live edge, so the renderer's opening sniff
        is answered out of memory instead of stalling on the encoder. That
        hoard is also the whole latency of this target: the TV drains it at
        line rate and then tracks the live edge from ~20 s behind.
        """
        with self._cond:
            return max(self._start, self._end - prefill)

    def read(self, absolute, length, deadline=None):
        """Up to `length` bytes at `absolute`, blocking until they exist.

        Returns (data, complete). `complete` is False when the encoder could
        not keep up before `deadline` -- or the session ended -- and the
        caller has to fill the rest in itself, because a renderer that asked
        for exactly n bytes must get exactly n bytes back.
        """
        out = bytearray()
        with self._cond:
            while len(out) < length:
                # Re-checked every round: the window keeps sliding while we
                # wait, and a reader that lags the eviction must resync to the
                # oldest byte still held rather than index off the front of the
                # deque (a negative slice would hand back the *tail* of a chunk
                # and the TV would blame our encoder for the garbage).
                cursor = max(absolute + len(out), self._start)
                have = self._collect(cursor, length - len(out))
                if have:
                    out += have
                    continue
                if self._closed:
                    return bytes(out), False
                remaining = None if deadline is None else deadline - time.time()
                if remaining is not None and remaining <= 0:
                    return bytes(out), False
                self._cond.wait(0.5 if remaining is None
                                else min(0.5, remaining))
            return bytes(out), True

    def _collect(self, absolute, length):
        """Bytes available right now in [absolute, absolute+length).

        Caller holds the lock; `absolute` must already be clamped to `_start`.
        """
        if absolute >= self._end:
            return b''
        offset = self._start
        for chunk in self._chunks:
            nxt = offset + len(chunk)
            if nxt > absolute:
                cut = absolute - offset
                return chunk[cut:min(len(chunk), cut + length)]
            offset = nxt
        return b''

    def pad(self, length):
        """`length` bytes of MPEG-PS padding: what a bounded sniff gets
        filled in with when the encoder has not produced that much yet."""
        if length <= 0:
            return b''
        return (self._pad * (length // len(self._pad) + 1))[:length]


def parse_range(header):
    """`Range: bytes=100-200` -> (100, 200); `bytes=100-` -> (100, None).

    Only a single range is meaningful here: the "file" is one endless stream,
    so a multi-range request is a client we cannot serve honestly. Suffix
    ranges (`bytes=-1024`) are dropped for the same reason -- "the last 1 kB of
    a file with no end" has no answer a TV would like.
    """
    if not header:
        return 0, None
    unit, _, spec = header.partition('=')
    if unit.strip().lower() != 'bytes' or ',' in spec:
        return None
    first, _, last = spec.partition('-')
    try:
        start = int(first)
    except ValueError:
        return None
    stop = None
    if last:
        try:
            stop = int(last)
        except ValueError:
            return None
    if stop is not None and stop < start:
        return None
    return start, stop


class _StreamHandler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, fmt, *args):
        logger.debug("stream http: " + fmt % args)

    @property
    def session(self):
        return self.server.session

    def _not_found(self):
        self.send_response(404)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def _authorized(self, suffix):
        """The stream id is the whole credential, so compare it in constant
        time -- a timing oracle on an 8-byte hex id is not worth the risk."""
        name = self.path.partition('?')[0].rsplit('/', 1)[-1]
        try:
            import hmac
            return hmac.compare_digest(name, self.session.stream_name(suffix))
        except Exception:
            return name == self.session.stream_name(suffix)

    def do_HEAD(self):
        # Renderers probe with HEAD before they commit to a read.
        if self.path.partition('?')[0].startswith(STREAM_PREFIX):
            self.do_GET(head_only=True)
        else:
            self._not_found()

    def do_GET(self, head_only=False):
        path = self.path.partition('?')[0]
        if path == BROWSER_PATH:
            return self._serve_page(head_only)
        if path.startswith(STREAM_PREFIX):
            if not self._authorized(self.session.suffix):
                return self._not_found()
            if self.session.bytelog:
                return self._serve_infinite_file(head_only)
            return self._serve_stream(head_only)
        return self._not_found()

    def _serve_stream(self, head_only=False):
        broadcaster = self.server.broadcaster
        queue = broadcaster.subscribe(replay=self.session.replay)
        try:
            self.send_response(200)
            self.send_header('Content-Type', self.session.content_type)
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Accept-Ranges', 'none')
            self.end_headers()
            if head_only:
                return
            while True:
                try:
                    chunk = queue.get(timeout=5.0)
                except Empty:
                    continue
                self.wfile.write(chunk)
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            broadcaster.unsubscribe(queue)

    def _serve_infinite_file(self, head_only=False):
        """The DLNA target: answer as if this were a finite media file.

        Three things make a ten-year-old renderer accept an endless stream:
        every response names a size below 2 GiB, a bounded sniff is answered
        with *exactly* the number of bytes it asked for (PS padding fills the
        gap while the encoder catches up), and a reconnect with an absolute
        byte offset lands on the same byte it left -- see _ByteLog.
        """
        log = self.server.broadcaster
        session = self.session
        if session.file_anchor is None:
            session.file_anchor = log.anchor()
        requested = parse_range(self.headers.get('Range'))
        if requested is None:
            self._not_found()
            return
        start, stop = requested
        if start >= session.file_size:
            # Past the end of the file we promised. 416 with our own size is
            # honest and keeps the client from concluding the file is broken.
            self.send_response(416)
            self.send_header('Content-Range',
                             'bytes */{}'.format(session.file_size))
            self.send_header('Content-Length', '0')
            self._dlna_headers()
            self.end_headers()
            return
        # A Range *header* is what makes this a 206, even when it only names a
        # start; a missing one means "the whole file", which is the same body
        # but a 200 and no Content-Range.
        has_range = bool(self.headers.get('Range'))
        bounded = stop is not None
        last = session.file_size - 1
        if bounded:
            last = min(stop, last)
        length = last - start + 1
        self.send_response(206 if has_range else 200)
        self.send_header('Content-Type', session.content_type)
        self.send_header('Content-Length', str(length))
        if has_range:
            # Content-Range on a 200 is malformed, and a 200 here means "the
            # whole file, from the top" -- which is also what the length says.
            self.send_header('Content-Range', 'bytes {}-{}/{}'.format(
                start, last, session.file_size))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self._dlna_headers()
        self.end_headers()
        if head_only:
            return
        log.reader_enter()
        try:
            absolute = session.file_anchor + start
            written = 0
            deadline = None if not bounded else \
                time.time() + DLNA_SNIFF_TIMEOUT
            while written < length:
                want = min(length - written, 1 << 20)
                if not bounded:
                    # Unbounded means "play until I say stop": block for as
                    # long as the encoder is alive, because a short body makes
                    # the renderer think the file ended and tear down.
                    data, complete = log.read(absolute, want)
                    if not data and not complete:
                        return          # the session closed
                else:
                    data, complete = log.read(absolute, want, deadline=deadline)
                    if not complete:
                        data += log.pad(want - len(data))
                if not data:
                    return
                self.wfile.write(data)
                self.wfile.flush()
                written += len(data)
                absolute += len(data)
                if bounded and written >= length:
                    return      # exactly n bytes, then the connection closes
        except (BrokenPipeError, ConnectionResetError, OSError):
            # The TV reconnecting with a Range is normal, not an error: it is
            # how a renderer fills its own buffer.
            logger.debug('the renderer closed the file read at offset %s',
                         start)
        finally:
            log.reader_leave()

    def _dlna_headers(self):
        """The DLNA-specific headers, on every answer of a DLNA session.

        `transferMode.dlna.org: Streaming` is what tells the renderer not to
        wait for the whole file; plenty of firmware refuses the stream without
        `contentFeatures.dlna.org`, and `Accept-Ranges: bytes` has to be there
        because the client's buffering strategy *is* seeking.
        """
        if self.session.kind != 'dlna':
            return
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('transferMode.dlna.org', DLNA_SECONDARY_HEADER)
        self.send_header('contentFeatures.dlna.org',
                         content_features(self.session.profile))

    def _serve_page(self, head_only=False):
        import hmac
        import urllib.parse
        query = urllib.parse.parse_qs(self.path.partition('?')[2] or '')
        token = (query.get('token') or [''])[0]
        if not token or not hmac.compare_digest(token, self.session.page_token):
            self.send_error(403, 'a token is required')
            return
        body = PLAYER_PAGE.replace('@STREAM@', self.session.stream_path()) \
                          .replace('@CODECS@', self.session.codecs) \
                          .replace('@TITLE@', self.session.page_title).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        if not head_only:
            self.wfile.write(body)


class _Session(object):
    """What this mirror session looks like to the HTTP server."""

    def __init__(self, kind, has_audio=False, title='Macast', profile=None):
        self.kind = kind if kind in OUTPUTS else DEFAULT_OUTPUT
        self.label, self.suffix, self.content_type, _args = OUTPUTS[self.kind]
        #: The DLNA target answers as a finite file, so it needs a profile
        #: (which container, which codec, which advertised size) and a byte log
        #: instead of the queue broadcaster. The other targets leave both None
        #: and keep the live-edge semantics.
        self.profile = None
        self.bytelog = False
        self.file_anchor = None
        if self.kind == 'dlna':
            self.profile = profile or dlna_profile()
            self.suffix = self.profile.suffix
            self.content_type = self.profile.content_type
            self.bytelog = True
            self.file_size, self.file_duration = advertised_file(
                self.profile.bitrate)
        #: A browser can only attach to a live fragmented stream at a
        #: keyframe, so replaying the header plus a short tail is what makes
        #: "open the URL a second time" work. A TV is never replayed: it would
        #: inherit a backlog and sit seconds behind for the rest of the session.
        self.replay = self.kind == 'browser'
        self.init_marker = b'moof' if self.kind == 'browser' else None
        self.stream_id = secrets.token_hex(8)
        #: Both credentials are per-session secrets and both die with the
        #: mirror. Deliberately *not* the app's stable management token: that
        #: one also opens the whole management API (AGENTS.md 4.7), so pasting
        #: a viewing URL would leak a credential that stays valid afterwards.
        self.page_token = secrets.token_hex(8)
        self.codecs = 'avc1.640028,mp4a.40.2' if has_audio else 'avc1.640028'
        self.page_title = title

    def stream_name(self, suffix=None):
        return '{}.{}'.format(self.stream_id, suffix or self.suffix)

    def stream_path(self):
        return '{}{}'.format(STREAM_PREFIX, self.stream_name())


def start_stream_server(session, broadcaster=None):
    server = ThreadingHTTPServer(('0.0.0.0', 0), _StreamHandler)
    server.session = session
    if broadcaster is None:
        broadcaster = _ByteLog() if session.bytelog else _Broadcaster(
            init_marker=session.init_marker)
    server.broadcaster = broadcaster
    server.daemon_threads = True
    # A client that vanished mid-stream stays in queue.get for seconds;
    # server_close must not wait for it.
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name="SCREEN_MIRROR_HTTP").start()
    return server


def stream_url(server):
    return 'http://{}:{}{}'.format(advertise_host(), server.server_address[1],
                                   server.session.stream_path())


def page_url(server):
    return 'http://{}:{}{}?token={}'.format(
        advertise_host(), server.server_address[1], BROWSER_PATH,
        server.session.page_token)


# -- the browser player page ------------------------------------------------
#
# Served by us, so the only substitutions are our own strings: a path, a codec
# label and a title. Nothing a sender controls reaches it (unlike the log
# panel's story in AGENTS.md 4.10), and it is textContent-only anyway.

PLAYER_PAGE = """<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>@TITLE@ 屏幕镜像</title>
<style>
 html,body{margin:0;height:100%;background:#000;color:#ccc;
   font:14px/1.5 -apple-system,system-ui,sans-serif}
 video{width:100%;height:100%;object-fit:contain;background:#000}
 #bar{position:fixed;left:0;right:0;top:0;display:flex;gap:8px;
   align-items:center;padding:6px 10px;background:#0008;color:#ddd;
   font-size:12px;opacity:.25;transition:opacity .3s}
 body:hover #bar{opacity:1}
 button{background:#222;color:#ddd;border:1px solid #444;border-radius:6px;
   padding:4px 10px;font:inherit}
 #err{color:#f88;display:none}
</style></head><body>
<div id="bar"><span id="st">连接中…</span><span id="err"></span>
 <span style="flex:1"></span>
 <button id="snd">开声音</button><button id="full">全屏</button></div>
<video id="v" autoplay playsinline muted></video>
<script>
var STREAM='@STREAM@',CODECS='@CODECS@',LIVE_EDGE=3,STALL_MS=8000;
var v=document.getElementById('v'),st=document.getElementById('st'),
    err=document.getElementById('err');
function say(t){st.textContent=t}
function fail(t){err.style.display='inline';err.textContent=' · '+t}
function lock(){if(navigator.wakeLock)navigator.wakeLock.request('screen')
  .catch(function(){})}
function unmute(){v.muted=false;document.getElementById('snd').style.display=
  'none'}
document.getElementById('snd').onclick=unmute;
document.getElementById('full').onclick=function(){
  (v.requestFullscreen||v.webkitRequestFullscreen||function(){}).call(v)};
// Autoplay policy: start muted, then any real gesture is consent to unmute.
['pointerdown','keydown','touchstart'].forEach(function(e){
  addEventListener(e,unmute,{once:true})});
function tail(){ // keep the buffer trimmed to the live edge
  try{var b=v.buffered;if(b.length&&v.duration){
    var end=b.end(b.length-1);
    if(end-v.currentTime>LIVE_EDGE)v.currentTime=end-LIVE_EDGE}}catch(x){}}
function progressive(){ // MSE missing or wedged: let the element stream it
  say('回退到渐进式播放');v.src=STREAM+'#t=0.001';v.play().catch(function(){});
  v.ontimeout=function(){location.reload()}}
function mse(){
  if(!window.MediaSource||!MediaSource.isTypeSupported){return progressive()}
  var ms=new MediaSource();
  ms.addEventListener('sourceopen',run,{once:true});
  v.src=URL.createObjectURL(ms);
  var timer=setTimeout(function(){ // sourceopen sometimes never fires on
    if(ms.readyState!=='open')progressive()},1500);
  function run(){
    clearTimeout(timer);var sb;
    try{sb=ms.addSourceBuffer(CODECS)}catch(e){return progressive()}
    sb.mode='sequence';var queue=[],last=Date.now();
    say('正在镜像');lock();
    fetch(STREAM).then(function(r){
      if(!r.ok||!r.body)throw new Error('http '+r.status);
      var rd=r.body.getReader();
      function pull(){return rd.read().then(function(x){
        if(!x.done){last=Date.now();
          if(sb.updating)queue.push(x.value);else sb.appendBuffer(x.value);
          return pull()}
        try{if(ms.readyState==='open')ms.endOfStream()}catch(e){}})}
        return pull()}).catch(function(e){fail('断开：'+e.message);
        setTimeout(function(){location.reload()},2000)});
    sb.addEventListener('updateend',function(){
      last=Date.now();
      if(queue.length)sb.appendBuffer(queue.shift());else tail()});
    setInterval(function(){ // watchdogs: stall, or a decoder that ate everything
      if(Date.now()-last>STALL_MS){fail('画面卡住，重连');location.reload()}
    },2000);
    v.play().catch(function(){})
  }
}
mse();
</script></body></html>
"""


def advertise_host():
    """The address a TV on the LAN can reach this Mac on."""
    try:
        addrs = Setting.get_advertisable_ip()
    except Exception:
        addrs = []
    return addrs[0] if addrs else '127.0.0.1'


# -- Cast v2 sender (same minimal subset cast_bridge uses) -------------------

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
        #: Only a mirroring session has one; it is what the receiver's STOP
        #: names, and without it the app stays running behind us.
        self.mirror_session_id = None
        #: A mirroring session writes to this socket from two threads (the
        #: teardown on the menu thread, the keepalive in the media loop), and
        #: interleaved CASTV2 frames are how a session dies quietly.
        self._write_lock = threading.Lock()

    def _send(self, destination, namespace, payload, binary=False):
        body = payload if binary else json.dumps(payload)
        blob = encode_cast_message(self.source_id, destination, namespace, body,
                                   binary)
        with self._write_lock:
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
        return self._await_any((wanted,), timeout)

    def _await_any(self, wanted, timeout=None):
        """The first message whose `type` is in `wanted`, or None.

        Anything else that arrives in between -- a PONG, a broadcast
        RECEIVER_STATUS -- is noise to be stepped over, not an error. The
        mirroring handshake has to watch for two types at once (the status that
        carries a transportId and the LAUNCH_ERROR that says there never will
        be one), which is why this exists next to `_await`.
        """
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
            if data.get("type") in wanted:
                return data
        return None

    def connect(self):
        raw = socket.create_connection((self.host, self.port), timeout=self.timeout)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.sock = context.wrap_socket(raw, server_hostname=None)
        self.sock.settimeout(self.timeout)
        self._send("receiver-0", "urn:x-cast:com.google.cast.tp.deviceauth",
                   DEVICE_AUTH_CHALLENGE, binary=True)
        self._recv()
        self._send("receiver-0", NS_CONNECTION, {"type": "CONNECT"})
        return self

    def launch(self):
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

    def load(self, url, content_type='', live=False):
        media = {"contentId": url,
                 "streamType": "LIVE" if live else "BUFFERED"}
        if content_type:
            media["contentType"] = content_type
        self._send(self.transport_id, NS_MEDIA,
                   {"type": "LOAD", "requestId": 2, "autoplay": True,
                    "media": media})
        status = self._await("MEDIA_STATUS") or {}
        entries = status.get("status") or [{}]
        if isinstance(entries, dict):
            entries = [entries]
        self.media_session_id = (entries[0] or {}).get("mediaSessionId", 1)
        return status

    def stop(self):
        if self.transport_id is None:
            return False
        try:
            self._send(self.transport_id, NS_MEDIA,
                       {"type": "STOP", "requestId": 4,
                        "mediaSessionId": self.media_session_id})
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "sessionId": None, "requestId": 5})
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info("could not send STOP to the device: %s", e)
            return False
        return True

    def set_volume(self, level):
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "SET_VOLUME", "requestId": 6,
                    "volume": {"level": max(0.0, min(1.0, level / 100.0))}})

    # -- mirroring (Cast Streaming): a different app, and no LOAD at all ------

    def receiver_status(self):
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "GET_STATUS", "requestId": 7})
        return (self._await("RECEIVER_STATUS") or {}).get("status") or {}

    def launch_mirroring(self, app_id=MIRROR_APP_ID, timeout=10.0,
                         settle=0.3):
        """Bring up the mirroring app and join its transport.

        Two things here are load-bearing rather than defensive, both of them
        learned from the reference sender: a stale instance of the app has to
        be stopped *before* LAUNCH, because the next session's first OFFER is
        otherwise rejected with "Invalid or missing codec on first OFFER"; and
        the app needs a moment to settle after its transportId appears before
        it will accept a CONNECT.

        LAUNCH_ERROR doubles as the capability probe -- this plugin does not
        filter devices by the `ca` mDNS flag, so a receiver without mirroring
        is discovered here rather than guessed at beforehand.
        """
        for app in self.receiver_status().get("applications") or []:
            if app.get("appId") != app_id:
                continue
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "requestId": 8,
                        "sessionId": app.get("sessionId")
                        or app.get("transportId")})
            # The receiver drops the app asynchronously; racing it is exactly
            # the stale-instance case above.
            time.sleep(1.0)
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "LAUNCH", "appId": app_id, "requestId": 9})
        deadline = time.time() + timeout
        session_id = None
        while time.time() < deadline and not self.transport_id:
            data = self._await_any(("RECEIVER_STATUS", "LAUNCH_ERROR"),
                                   max(0.2, deadline - time.time()))
            if data is None:
                break
            if data.get("type") == "LAUNCH_ERROR":
                raise RuntimeError('这台设备不接受镜像接收器（LAUNCH_ERROR: {}）'.format(
                    data.get("reason") or "未提供原因"))
            for app in (data.get("status") or {}).get("applications") or []:
                if app.get("appId") == app_id and app.get("transportId"):
                    self.transport_id = app["transportId"]
                    session_id = app.get("sessionId") or app.get("transportId")
                    break
        if not self.transport_id:
            raise RuntimeError("镜像接收器没有返回 transportId（启动超时）")
        self.mirror_session_id = session_id
        time.sleep(settle)
        self._send(self.transport_id, NS_CONNECTION, {"type": "CONNECT"})
        return self.transport_id

    def send_offer(self, offer):
        if self.transport_id is None:
            raise RuntimeError("还没有镜像会话，无法发出 OFFER")
        self._send(self.transport_id, NS_WEBRTC, offer)

    def await_answer(self, timeout=10.0):
        data = self._await_any(("ANSWER",), timeout)
        if data is None:
            raise RuntimeError("镜像接收器没有回答 OFFER（等待 ANSWER 超时）")
        return data

    def ping(self):
        """Cast v2 keepalive. Deprecated in the spec but every firmware still
        answers it, and it is the only signal that the TV is still there."""
        self._send("receiver-0", NS_CONNECTION, {"type": "PING"})

    def poll(self, timeout=0.0):
        """One pending control message, or None if nothing is waiting.

        `timeout` 0 puts the socket in non-blocking mode, where "no data" is an
        exception rather than a wait -- which is the whole point: the media loop
        must never be held by the control plane.
        """
        try:
            self.sock.settimeout(timeout)
            return self._recv()
        except (socket.timeout, ssl.SSLError, OSError):
            return None

    def close_mirroring(self):
        """Leave the app and the transports, in that order.

        CLOSE on the app transport without a receiver STOP leaves the mirroring
        app running, and the next session then hits the stale-instance OFFER
        rejection this class went to some trouble to avoid.
        """
        try:
            if self.transport_id is not None:
                self._send(self.transport_id, NS_CONNECTION,
                           {"type": "CLOSE"})
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "requestId": 11,
                        "sessionId": getattr(self, "mirror_session_id", None)})
            self._send("receiver-0", NS_CONNECTION, {"type": "CLOSE"})
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info("could not close the mirroring session: %s", e)
        self.transport_id = None

    def close(self):
        if self.sock is not None:
            self._drain()
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None

    def _drain(self, budget=0.3):
        """Empty the receive buffer before hanging up.

        The receiver answers every keepalive and every goodbye, so at teardown
        there are almost always unread replies sitting in this socket. Closing
        with unread bytes makes the kernel answer with RST instead of FIN, and
        that RST discards the final CLOSE that `close_mirroring` just wrote --
        leaving the mirroring app running and the next session facing the
        stale-instance OFFER rejection.
        """
        deadline = time.monotonic() + budget
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.sock.settimeout(min(0.2, remaining))
                if not self.sock.recv(4096):
                    break
        except (socket.timeout, ssl.SSLError, OSError, ValueError):
            pass


# -- DLNA control point (the TV is a renderer; we are its remote) -------------
#
# Everything above this line speaks Cast, to a device that wants to be told
# what to play. A DLNA MediaRenderer is the same idea over UPnP: SSDP to find
# it, an XML description to learn where its AVTransport control endpoint is,
# and SOAP to say "here is a URL, play it". Written on purpose from the
# standard library: `macast/ssdp.py` is a *device* (it answers M-SEARCH), not a
# control point, and a single-file plugin cannot pip-install one.


def _ssdp_search(st, timeout=3.0, interface=None):
    """(LOCATION, answering ip) pairs for one SSDP search target."""
    request = '\r\n'.join([
        'M-SEARCH * HTTP/1.1',
        'HOST: {}:{}'.format(SSDP_ADDR, SSDP_PORT),
        'MAN: "ssdp:discover"',
        'MX: 2',
        'ST: {}'.format(st),
        '', ''])
    found = set()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(0.4)
    if interface:
        # Binding is how a multi-homed Mac (VPN, bridges) picks which interface
        # the multicast leaves on -- the same worry AGENTS.md 4.1 has for the
        # mDNS advertiser.
        try:
            sock.bind((interface, 0))
        except OSError as e:
            logger.info('cannot search from %s: %s', interface, e)
    try:
        sock.sendto(request.encode('ascii'), (SSDP_ADDR, SSDP_PORT))
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                data, peer = sock.recvfrom(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            for line in data.decode('utf-8', 'replace').splitlines():
                name, _, value = line.partition(':')
                if name.strip().lower() != 'location':
                    continue
                value = value.strip()
                if value.startswith('http'):
                    found.add((value, peer[0]))
    finally:
        sock.close()
    return sorted(found)


def _local_name(tag):
    return tag.rpartition('}')[2].lower() if isinstance(tag, str) else ''


def parse_description(raw, base_url, peer_ip):
    """(friendly name, absolute AVTransport control URL) or None.

    Namespaces are matched by local name because UPnP descriptions in the wild
    disagree on prefix and case. The control URL is resolved against the
    description's own base, and a device that describes itself on a loopback or
    unspecified address is rewritten to the address that actually answered --
    a renderer behind a router hits that more often than the spec admits.
    """
    import urllib.parse
    import xml.etree.ElementTree as ET
    import ipaddress
    try:
        root = ET.fromstring(raw)
    except ET.ParseError:
        return None
    if root is None:
        return None

    def first_text(tag):
        for node in root.iter():
            if _local_name(node.tag) == tag:
                return (node.text or '').strip()
        return ''

    name = first_text('friendlyname') or peer_ip
    parsed = urllib.parse.urlsplit(base_url)
    host = parsed.hostname or peer_ip
    port = _port_suffix(parsed)
    try:
        if ipaddress.ip_address(host).is_loopback or host == '0.0.0.0':
            host = peer_ip
    except ValueError:
        pass                      # a hostname: keep it, DNS is the LAN's job
    control = ''
    for service in root.iter():
        if _local_name(service.tag) != 'service':
            continue
        values = {}
        for child in service:
            values[_local_name(child.tag)] = (child.text or '').strip()
        if values.get('servicetype', '').endswith(':AVTransport:1'):
            control = values.get('controlurl', '')
            break
    if not control:
        return None
    if not control.startswith('http'):
        # The port belongs to the answer, not to the hostname: dropping it
        # here sends every SOAP call to port 80 of a TV that serves it on 5246.
        same_origin = '{}://{}{}'.format(parsed.scheme, host, port)
        prefix = same_origin + ('' if control.startswith('/')
                                else parsed.path.rsplit('/', 1)[0])
        control = prefix + (control if control.startswith('/')
                            else '/' + control)
    else:
        described = urllib.parse.urlsplit(control)
        if described.hostname != host:
            control = '{}://{}{}{}'.format(described.scheme, host,
                                           _port_suffix(described, port),
                                           described.path)
    return name, control


def describe_renderer(url, peer_ip, timeout=3.0):
    import urllib.request
    try:
        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}))
        with opener.open(url, timeout=timeout) as response:
            raw = response.read(512 * 1024)
    except Exception as e:
        logger.debug('cannot read %s: %s', url, e)
        return None
    return parse_description(raw, url, peer_ip)


def discover_renderers(timeout=3.0):
    """[(friendly name, control url, host)] for DLNA renderers on the LAN.

    The host is what `advertise_host()` must be compared against: a TV on
    another subnet can answer SSDP and still never reach our stream URL.
    """
    seen = {}
    try:
        ours = set(Setting.get_advertisable_ip())
    except Exception:
        ours = set()
    for st in DLNA_SEARCH_TARGETS:
        for location, peer in _ssdp_search(st, timeout=timeout):
            if peer in ours:
                continue                      # do not mirror to ourselves
            parsed = describe_renderer(location, peer)
            if parsed is None:
                continue
            name, control = parsed
            host = _url_host(control) or peer
            seen[host] = (name, control, host)
    return sorted(seen.values())


_dlna_devices = []
_dlna_searching = False
#: wall clock of the last completed DLNA search; 0.0 = never (see _searched_at).
_dlna_searched_at = 0.0


def start_renderer_search():
    """Refresh the DLNA renderer list in the background (menu-safe)."""
    global _dlna_searching
    with _search_lock:
        if _dlna_searching:
            return False
        _dlna_searching = True

    def _run():
        global _dlna_devices, _dlna_searching, _dlna_searched_at
        try:
            found = discover_renderers()
            if found:
                _dlna_devices = found
        finally:
            with _search_lock:
                _dlna_searching = False
                _dlna_searched_at = time.time()

    threading.Thread(target=_run, daemon=True,
                     name="SCREEN_MIRROR_DLNA_SEARCH").start()
    return True


def dlna_target():
    """(name, control url) of the renderer chosen in the menu.

    A separate key from `Mirror_Target` on purpose: that one holds
    `host:port` for a Chromecast, and parsing a URL with that splitter is how
    'http://10.0.0.5:49152/x' turns into a host named 'http'.
    """
    control = Setting.get(SettingProperty.Mirror_Dlna_Control, '') or ''
    name = Setting.get(SettingProperty.Mirror_Target_Name, '') or ''
    if not control:
        return None, ''
    return (name or control), control


def source_address_for(peer_ip):
    """Which of our own addresses reaches this TV.

    A UDP connect() asks the routing table without sending anything. Mirroring
    from the wrong interface looks like a broken stream: the TV fetches the
    URL, gets nothing routable back, and reports 'file unsupported'.
    """
    try:
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    except OSError:
        return advertise_host()
    try:
        probe.connect((peer_ip, 9))
        return probe.getsockname()[0]
    except OSError:
        return advertise_host()
    finally:
        probe.close()


def _url_host(url):
    """The hostname inside a URL, for the route probe."""
    import urllib.parse
    return urllib.parse.urlsplit(url).hostname or ''


def _port_suffix(parts, default=''):
    """:port for a urlsplit result, `default` when it names none.

    ``parts.port`` raises on a non-numeric port, and a hand-written device
    description is exactly where that turns up; losing the renderer silently
    is worse than guessing the scheme's own port.
    """
    try:
        return ':{}'.format(parts.port) if parts.port else default
    except ValueError:
        return default


def dlna_stream_url(server, peer_ip=None):
    """The stream address as the TV on the other end will have to write it."""
    host = source_address_for(peer_ip) if peer_ip else advertise_host()
    return 'http://{}:{}{}'.format(host, server.server_address[1],
                                   server.session.stream_path())


class _DlnaSender(object):
    """The handful of AVTransport actions a mirror needs, over SOAP.

    InstanceID is remembered from whatever the renderer last answered with: a
    device is allowed to move off 0, and a control point that keeps sending 0
    gets error 701 from exactly the quirky firmware this target exists for.
    """

    def __init__(self, control_url, timeout=5.0):
        self.control_url = control_url
        self.timeout = timeout
        self.instance_id = '0'
        self.last_state = ''
        self._opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}))

    def _request(self, action, args):
        from xml.sax.saxutils import escape
        import xml.etree.ElementTree as ET
        fields = ''.join('<{name}>{value}</{name}>'.format(
            name=name, value=escape(unicode_text(args[name])))
            for name in args)
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
            's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
            '<s:Body><u:{action} xmlns:u="{service}">'
            '<InstanceID>{instance}</InstanceID>{fields}'
            '</u:{action}></s:Body></s:Envelope>'
        ).format(action=action, service=DLNA_SERVICE,
                 instance=self.instance_id, fields=fields)
        # Byte length, not str length: a Chinese device name is multi-byte, and
        # a Content-Length that counts characters truncates the envelope on the
        # wire -- the renderer then answers a SOAP fault and the mirror looks
        # like a dead TV rather than like our own bug.
        payload = body.encode('utf-8')
        request = urllib.request.Request(
            self.control_url, data=payload,
            headers={'SOAPAction': '"{}#{}"'.format(DLNA_SERVICE, action),
                     'Content-Type': 'text/xml; charset="utf-8"',
                     'Content-Length': str(len(payload)),
                     'Connection': 'close'})
        with self._opener.open(request, timeout=self.timeout) as response:
            raw = response.read(512 * 1024)
        values = {}
        try:
            root = ET.fromstring(raw)
        except ET.ParseError:
            logger.warning('%s replied with something that is not XML',
                           action)
            return values
        for node in root.iter():
            if _local_name(node.tag) == 'instanceid' and (node.text or '').strip():
                self.instance_id = node.text.strip()
            if _local_name(node.tag).endswith('response'):
                for child in node:
                    values[_local_name(child.tag)] = (child.text or '').strip()
        return values

    def set_uri(self, url, title, profile):
        size, duration = advertised_file(profile.bitrate)
        return self._request('SetAVTransportURI', {
            'CurrentURI': url,
            'CurrentURIMetaData': build_didl(url, title, profile, size,
                                             duration)})

    def play(self):
        return self._request('Play', {'Speed': '1'})

    def stop(self):
        try:
            self._request('Stop', {})
        except Exception as e:
            logger.debug('the renderer did not accept Stop: %s', e)

    def transport_state(self):
        state = self._request('GetTransportInfo', {}).get(
            'currenttransportstate', '')
        if state:
            self.last_state = state
        return self.last_state

    def position(self):
        return self._request('GetPositionInfo', {}).get('reltime', '')

    def close(self):
        pass


# -- Cast Streaming: AES-128-CTR without a crypto dependency -----------------

def _xtime(a):
    a <<= 1
    return (a ^ 0x1B) & 0xFF if a & 0x100 else a


def _build_aes_tables():
    """Rijndael's S-box and the four encryption T-tables, computed once.

    Generated rather than pasted: a 256-byte literal is a typo waiting to ship,
    and the derivation is a dozen lines that a test can pin with the FIPS-197
    vector.
    """
    exp = [0] * 256
    log = [0] * 256
    x = 1
    for i in range(255):
        exp[i] = x
        log[x] = i
        x = _xtime(x) ^ x                     # multiply by 3, the field generator
    sbox = bytearray(256)
    for i in range(256):
        b = 0 if i == 0 else exp[(255 - log[i]) % 255]        # GF(2^8) inverse
        s = b
        for shift in (1, 2, 3, 4):
            s ^= ((b << shift) | (b >> (8 - shift))) & 0xFF
        sbox[i] = (s ^ 0x63) & 0xFF
    te0 = [0] * 256
    te1 = [0] * 256
    te2 = [0] * 256
    te3 = [0] * 256
    for i in range(256):
        s = sbox[i]
        s2 = _xtime(s)
        s3 = s2 ^ s
        te0[i] = (s2 << 24) | (s << 16) | (s << 8) | s3
        te1[i] = (s3 << 24) | (s2 << 16) | (s << 8) | s
        te2[i] = (s << 24) | (s3 << 16) | (s2 << 8) | s
        te3[i] = (s << 24) | (s << 16) | (s3 << 8) | s2
    return bytes(sbox), tuple(te0), tuple(te1), tuple(te2), tuple(te3)


_AES_SBOX, _AES_TE0, _AES_TE1, _AES_TE2, _AES_TE3 = _build_aes_tables()


def _aes_expand_key(key):
    """The 44 round words of an AES-128 key schedule, big-endian."""
    if len(key) != 16:
        raise ValueError('AES-128 wants a 16-byte key')
    w = [int.from_bytes(key[i * 4:i * 4 + 4], 'big') for i in range(4)]
    rcon = 1
    for i in range(4, 44):
        t = w[i - 1]
        if i % 4 == 0:
            t = ((t << 8) | (t >> 24)) & 0xFFFFFFFF              # RotWord
            s = _AES_SBOX
            t = ((s[t >> 24] << 24) | (s[(t >> 16) & 0xFF] << 16)
                 | (s[(t >> 8) & 0xFF] << 8) | s[t & 0xFF])
            t ^= rcon << 24
            rcon = _xtime(rcon)
        w.append(w[i - 4] ^ t)
    return w


def _aes_encrypt_block(w, s0, s1, s2, s3):
    """One 16-byte block through the schedule from `_aes_expand_key`."""
    te0, te1, te2, te3 = _AES_TE0, _AES_TE1, _AES_TE2, _AES_TE3
    s0 ^= w[0]
    s1 ^= w[1]
    s2 ^= w[2]
    s3 ^= w[3]
    o = 4
    for _ in range(9):
        t0 = (te0[s0 >> 24] ^ te1[(s1 >> 16) & 0xFF]
              ^ te2[(s2 >> 8) & 0xFF] ^ te3[s3 & 0xFF] ^ w[o])
        t1 = (te0[s1 >> 24] ^ te1[(s2 >> 16) & 0xFF]
              ^ te2[(s3 >> 8) & 0xFF] ^ te3[s0 & 0xFF] ^ w[o + 1])
        t2 = (te0[s2 >> 24] ^ te1[(s3 >> 16) & 0xFF]
              ^ te2[(s0 >> 8) & 0xFF] ^ te3[s1 & 0xFF] ^ w[o + 2])
        t3 = (te0[s3 >> 24] ^ te1[(s0 >> 16) & 0xFF]
              ^ te2[(s1 >> 8) & 0xFF] ^ te3[s2 & 0xFF] ^ w[o + 3])
        s0, s1, s2, s3 = t0, t1, t2, t3
        o += 4
    s = _AES_SBOX
    r = w[o:]
    out = []
    words = (s0, s1, s2, s3)
    for c in range(4):                       # last round: SubBytes+ShiftRows+XOR
        word = 0
        for row in range(4):
            shift = 24 - 8 * row
            byte = (s[(words[(c + row) & 3] >> shift) & 0xFF]
                    ^ ((r[c] >> shift) & 0xFF)) & 0xFF
            word |= byte << shift
        out.append(word)
    return out


class Aes128Ctr(object):
    """AES-128 in counter mode, over a whole buffer.

    Cast Streaming restarts the counter for every access unit with a nonce the
    receiver can rebuild from the frame id, so the caller supplies the full
    16-byte initial counter and we keep keystream generation in one place that
    a test can pin against the NIST vectors.
    """

    __slots__ = ('_w',)

    def __init__(self, key):
        self._w = _aes_expand_key(key)

    def crypt(self, iv, data):
        """Return `data` XOR the keystream starting at the 16-byte counter `iv`."""
        w = self._w
        c0, c1, c2, c3 = struct.unpack('>4I', iv)
        out = bytearray(data)
        pos = 0
        total = len(out)
        pack = struct.pack
        while pos < total:
            k0, k1, k2, k3 = _aes_encrypt_block(w, c0, c1, c2, c3)
            take = min(16, total - pos)
            ks = pack('>4I', k0, k1, k2, k3)[:take]
            chunk = bytes(out[pos:pos + take])
            out[pos:pos + take] = int.to_bytes(
                int.from_bytes(chunk, 'big') ^ int.from_bytes(ks, 'big'),
                take, 'big')
            pos += take
            c3 += 1
            if c3 > 0xFFFFFFFF:
                c3 = 0
                c2 += 1
                if c2 > 0xFFFFFFFF:
                    c2 = 0
                    c1 += 1
                    if c1 > 0xFFFFFFFF:
                        c1 = 0
                        c0 = (c0 + 1) & 0xFFFFFFFF
        return bytes(out)


#: NAL types that carry a picture slice (H.264 table 7-1).
_VCL_NALS = (1, 2, 5)
#: Parameter sets the receiver needs in front of every IDR, not once per stream.
_PARAM_NALS = (7, 8)
#: Access unit delimiter: it names the start of a picture, so it also ends the
#: previous one. Only x264 gets asked for these (the `aud=1` flag); a stream
#: without them simply falls back to "closed by the next slice".
_AUD = 9


def _starts_picture(nal, header):
    """Whether a slice NAL is the first slice of a new picture.

    The slice header opens with `first_mb_in_slice` as an unsigned Exp-Golomb
    code, and zero -- which is what a picture's first slice carries -- encodes
    as a single '1' bit: the top bit of the byte after the NAL header. Slices
    two..n of the same picture have a non-zero first_mb_in_slice, and x264 under
    `-tune zerolatency` really does emit them, so this is the difference between
    one picture per frame and a fifth of a picture per frame.

    `header` is where the NAL header byte sits inside `nal` (after its start
    code). No emulation-prevention unescaping is needed for this one bit: an
    escape can only follow a run of two zeros, and a NAL header byte is never
    zero, so the byte after it is always where it looks.
    """
    return len(nal) > header + 1 and (nal[header + 1] & 0x80) != 0


def _nal_starts(data):
    """Offsets of every Annex-B start code in `data`, with its length.

    A 4-byte start code is a 3-byte one with a leading zero, so the offset is
    reported at the first of those zeros and `width` says how many to skip.
    """
    out = []
    i = data.find(b'\x00\x00\x01')
    while i != -1:
        width = 4 if i > 0 and data[i - 1] == 0 else 3
        out.append((i - (width - 3), width))
        i = data.find(b'\x00\x00\x01', i + 3)
    return out


class _AccessUnits(object):
    """ffmpeg's Annex-B byte stream -> one buffer per picture.

    Cast Streaming encrypts and timestamps *access units*, so the framing has
    to happen here rather than in a muxer. A picture is recognised by its first
    slice (`first_mb_in_slice == 0`, which is ue(v) zero, which is a single '1'
    bit right after the NAL header) and closed by the next one, or by the access
    unit delimiter when the encoder emits one. Slicing matters here: x264 under
    `-tune zerolatency` splits a 1080p picture into several slice NALs, and
    treating each as a frame would put a fifth of a picture on the panel.
    Parameter sets belong to the unit that follows them, and an IDR without its
    SPS/PPS in front is unwatchable, so they are repeated from the last set seen
    rather than trusted to `repeat_headers`.
    """

    def __init__(self):
        self._pending = bytearray()
        self._units = []
        self._open = bytearray()        # the unit being filled
        self._prefix = bytearray()      # non-VCL NALs ahead of the next slice
        self._params = {}               # nal type -> bytes, last seen

    def feed(self, data):
        """Queue the complete units this chunk finished; returns them."""
        self._pending += data
        self._parse()
        out, self._units = self._units, []
        return out

    def flush(self):
        """Hand over everything the stream ended on.

        A unit is only known to be finished when the next one starts, so at the
        end of the byte stream the last NAL is still open *and* the unit before
        it is still waiting for that NAL to be taken. Without this a teardown
        loses the final pictures; with `-aud` the encoder removes the lag
        entirely (see `_AUD`).
        """
        self._parse(final=True)
        if self._open:
            self._close()
        out, self._units = self._units, []
        return out

    def _parse(self, final=False):
        while True:
            starts = _nal_starts(self._pending)
            if not starts or (len(starts) < 2 and not final):
                return                  # the last NAL is still arriving
            offset, width = starts[0]
            nxt = starts[1][0] if len(starts) > 1 else len(self._pending)
            body = offset + width
            ntype = (self._pending[body] & 0x1F) if body < nxt else 0
            nal = bytes(self._pending[offset:nxt])
            del self._pending[:nxt]     # also drops any junk before offset
            self._take(nal, ntype, width)

    def _take(self, nal, ntype, header):
        if ntype not in _VCL_NALS:
            if ntype == _AUD and self._open:
                # An delimiter announces a new picture, which is what tells us
                # the one we were filling is over -- one frame earlier than
                # waiting for its first slice NAL to arrive.
                self._close()
            if ntype in _PARAM_NALS:
                self._params[ntype] = nal
            self._prefix += nal         # belongs to the unit that follows
            return
        if self._open and _starts_picture(nal, header):
            self._close()
        if self._open:
            # Another slice of the picture already being filled.
            self._open += nal
            return
        head = bytearray()
        if ntype == 5:
            for t in _PARAM_NALS:
                cached = self._params.get(t)
                if cached and cached not in self._prefix:
                    head += cached
        # SPS/PPS in front of the prefix, not appended after it: an AU that
        # hands the decoder SEI -> SPS -> IDR is out of order, and whichever
        # set the encoder happened to repeat in-band stays where it was.
        self._open = head + self._prefix + nal
        self._prefix = bytearray()

    def _close(self):
        unit = bytes(self._open)
        self._open = bytearray()
        if unit.strip(b'\x00'):
            self._units.append(unit)


# -- the mirroring session ---------------------------------------------------
#
# Everything below is the media plane: what a Chromecast expects to find on UDP
# once the control plane has handed it an OFFER. The field layouts follow
# Google's own openscreen implementation (the C++ that ships in Chrome) as
# transcribed by the MIT-licensed omacast reference, and each one is pinned by a
# case in scripts/verify_cast_airplay.py Part 24. What has *not* been verified
# is a real television: the far end of that loop is a fake receiver built from
# these same tables, so these bytes are self-consistent rather than
# field-proven -- AGENTS.md 4.9 is explicit that those are not the same claim.

#: openscreen's payload types, including the pair its own header calls a
#: "hack for Android TV" -- which is the pair shipping mirroring firmware
#: answers. The canonical table in the same header (video 101) is for real
#: WebRTC receivers, and this protocol is not that.
RTP_VIDEO_PT = 96
VIDEO_CLOCK = 90000
#: The receiver's jitter-buffer target. Chrome's tab-cast default of 400 ms
#: reads as a slideshow from a desktop; 200 ms is what a LAN can hold.
TARGET_DELAY_MS = 200
#: 12 bytes of RTP header + 7 bytes of Cast header. The 1400 ceiling keeps the
#: IP packet inside an ordinary Ethernet MTU: this protocol has no fragmentation
#: story, because a frame travels as whole access units or not at all.
MAX_PACKET = 1400
CAST_PACKET_HEADER = 19
MAX_PAYLOAD = MAX_PACKET - CAST_PACKET_HEADER
#: How many frames the receiver may sit on before we call it behind us. The
#: reference also bounds the window by time; a frame count is what can be
#: enforced without knowing the round trip.
MAX_IN_FLIGHT_FRAMES = 12
#: A receiver will not paint anything until it has the NTP<->RTP mapping, so
#: the first sender report rides with the first picture rather than waiting for
#: this timer.
SR_INTERVAL = 0.5
#: With nothing new to send, resend the last packet of the newest
#: un-acknowledged frame. It is the protocol's only way of saying "I am still
#: here, and this is the frame I am waiting on you for", and it is what unsticks
#: a receiver that lost a frame's final packet.
KICKSTART_INTERVAL = 0.25
CONTROL_PING_SECONDS = 5.0
#: The reference's own definition of a dead session.
CONTROL_LOSS_SECONDS = 20.0
#: Seconds between the NTP epoch (1900) and the UNIX epoch (1970).
NTP_UNIX_OFFSET = 2208988800
#: Pure-Python AES-128-CTR runs at about 1.3 MB/s, so this target buys its
#: latency with a lower ceiling than the LOAD path has: 10 Mbps of 1080p would
#: leave the encryptor permanently behind the encoder, and the mirror would
#: slow-walk further and further into the past.
CAST_STREAM_MAX_BITRATE = 4500000
#: Requested height -> the frame this target actually pins. The OFFER has to
#: claim a resolution before the encoder has produced a single picture, and
#: "whatever the desktop happens to be" cannot be claimed then -- so 'source'
#: means 1080p on this target, and the scale filter letterboxes into the box
#: rather than stretching a 3:2 laptop to make the claim true.
CAST_STREAM_SIZES = {360: (640, 360), 720: (1280, 720), 1080: (1920, 1080),
                     0: (1920, 1080)}


def cast_stream_shape(height):
    """(width, height, ffmpeg scale/pad filter) for the low-latency target."""
    width, height = CAST_STREAM_SIZES.get(height, CAST_STREAM_SIZES[720])
    return width, height, (
        'scale={w}:{h}:force_original_aspect_ratio=decrease,'
        'pad={w}:{h}:(ow-iw)/2:(oh-ih)/2'.format(w=width, h=height))


def aes_material():
    """(key, iv mask), sixteen raw bytes each.

    The *sender* makes both up and offers them in the clear; the ANSWER carries
    no key material at all. Worth being blunt about: this is encryption for
    framing, not confidentiality. Anyone on the LAN who can read the OFFER can
    decrypt the picture.
    """
    return secrets.token_bytes(16), secrets.token_bytes(16)


def frame_iv(iv_mask, frame_id):
    """openscreen's per-frame counter block: a zero block with the frame id
    big-endian at bytes 8..12, then the whole thing XORed with the mask."""
    block = (b'\x00' * 8 + struct.pack('>I', frame_id & 0xFFFFFFFF)
             + b'\x00' * 4)
    return bytes(bytearray(a ^ b for a, b in zip(block, iv_mask)))


def build_offer(ssrc, key, iv_mask, width, height, fps, bitrate, seq_num=1):
    """The OFFER for one video stream (audio is phase two -- see the plan).

    `aesKey` and `aesIvMask` have to be exactly 32 hex digits. The receiver
    reads the length as the key size, so anything else comes back as
    `result: error` rather than a negotiation.
    """
    return {
        'type': 'OFFER', 'seqNum': seq_num,
        'offer': {
            'castMode': 'mirroring',
            'receiverGetStatus': True,
            'supportedStreams': [{
                'index': 0,
                'type': 'video_source',
                'codecName': 'h264',
                'rtpProfile': 'cast',
                'rtpPayloadType': RTP_VIDEO_PT,
                'ssrc': ssrc,
                'targetDelay': TARGET_DELAY_MS,
                'aesKey': key.hex(),
                'aesIvMask': iv_mask.hex(),
                'timeBase': '1/{}'.format(VIDEO_CLOCK),
                'maxBitRate': bitrate,
                'maxFrameRate': '{}000/1000'.format(fps),
                'resolutions': [{'width': width, 'height': height}],
                'receiverRtcpEventLog': False,
            }],
        },
    }


def parse_answer(data):
    """(udp port, receiver ssrc) out of an ANSWER; raise if it is a refusal.

    `sendIndexes` and `ssrcs` are positional pairs into our own OFFER. The
    receiver's SSRC exists only so feedback can be addressed -- the RTP we send
    carries *our* SSRC.
    """
    if (data.get('result') or 'ok') != 'ok':
        raise RuntimeError('接收端拒绝了镜像邀请：{}'.format(
            json.dumps(data.get('error') or {}, ensure_ascii=False)))
    answer = data.get('answer') or {}
    port = answer.get('udpPort')
    indexes = answer.get('sendIndexes') or []
    ssrcs = answer.get('ssrcs') or []
    if not isinstance(port, int) or isinstance(port, bool) or \
            not 0 < port < 65536:
        raise RuntimeError('接收端给的 udpPort 不可用：{!r}'.format(port))
    if not indexes or len(indexes) != len(ssrcs):
        raise RuntimeError('接收端的 ANSWER 自相矛盾：{} 条流、{} 个 SSRC'.format(
            len(indexes), len(ssrcs)))
    if 0 not in indexes:
        raise RuntimeError('接收端没有接受这条视频流')
    return port, ssrcs[list(indexes).index(0)]


def _unit_is_key(unit):
    """Whether an access unit holds an IDR, i.e. redraws the picture."""
    for offset, width in _nal_starts(unit):
        body = offset + width
        if body < len(unit) and (unit[body] & 0x1F) == 5:
            return True
    return False


def cast_packet(payload, ssrc, sequence, timestamp, frame_id, is_key,
                packet_id, packet_count, referenced_frame_id, marker=False):
    """One RTP header and Cast header in front of an encrypted slice."""
    flags = 0x40              # "the referenced frame id field is used"
    if is_key:
        flags |= 0x80
    return struct.pack(
        '>BBHIIBBHHB', 0x80,
        (0x80 if marker else 0) | RTP_VIDEO_PT,
        sequence & 0xFFFF, timestamp & 0xFFFFFFFF, ssrc & 0xFFFFFFFF,
        flags, frame_id & 0xFF, packet_id, max(0, packet_count - 1),
        referenced_frame_id & 0xFF) + payload


def cast_packets(ciphertext, frame_id, is_key, referenced_frame_id, ssrc,
                 sequence, timestamp):
    """Slice one encrypted frame into packets; returns (packets, next sequence).

    The whole access unit is encrypted before slicing -- one nonce per frame,
    which is why the ciphertext is exactly as long as the plaintext and only the
    last packet is short. `sequence` advances on every packet *including* a
    resend, because the receiver's reorder buffer counts arrivals rather than
    ids. A zero-length frame still sends one header-only packet.
    """
    count = max(1, -(-len(ciphertext) // MAX_PAYLOAD))
    out = []
    for index in range(count):
        chunk = ciphertext[index * MAX_PAYLOAD:(index + 1) * MAX_PAYLOAD]
        out.append(cast_packet(chunk, ssrc, sequence, timestamp, frame_id,
                               is_key, index, count, referenced_frame_id,
                               marker=(index == count - 1)))
        sequence = (sequence + 1) & 0xFFFF
    return out, sequence


def ntp_timestamp(now=None):
    """32.32 fixed point from the NTP epoch. The receiver pairs this with an
    RTP timestamp, so both clocks must be read at the same instant."""
    now = time.time() if now is None else now
    seconds = int(now)
    fraction = int((now - seconds) * 0x100000000) & 0xFFFFFFFF
    return ((seconds + NTP_UNIX_OFFSET) << 32) | fraction


def rtcp_sender_report(ssrc, ntp, timestamp, packets, octets):
    """28 bytes. No report, no picture -- see SR_INTERVAL."""
    return struct.pack('>BBHIQIII', 0x80, 200, 6, ssrc & 0xFFFFFFFF, ntp,
                       timestamp & 0xFFFFFFFF, packets & 0xFFFFFFFF,
                       octets & 0xFFFFFFFF)


def parse_rtcp(data, media_ssrc):
    """The events in one compound RTCP packet that are addressed to us.

    ('picture-loss',), ('checkpoint', frame-id-8, playout-delay-ms) or
    ('nack', frame-id-8, packet-id, bitmask). The receiver's feedback is RTCP on
    the media socket, not a message on the control plane: payload-specific
    (206) with FMT 1 is a picture loss, and FMT 15 carrying the ASCII word
    CAST is the acknowledgement. A trailing CST2 block is skipped -- we do not
    retransmit individual packets, so per-packet loss tells us nothing.
    """
    events = []
    position = 0
    while position + 4 <= len(data):
        head = data[position]
        if head >> 6 != 2:
            break
        ptype = data[position + 1]
        length = (struct.unpack('>H', data[position + 2:position + 4])[0]
                  + 1) * 4
        body = data[position + 4:position + length]
        position += length
        if ptype != 206 or len(body) < 8:
            continue                # RR / XR / SDES / BYE: nothing to act on
        if struct.unpack('>I', body[4:8])[0] != media_ssrc:
            continue                # about somebody else's stream
        fmt = head & 0x1F
        if fmt == 1:
            events.append(('picture-loss',))
        elif fmt == 15 and len(body) >= 16 and body[8:12] == b'CAST':
            events.append(('checkpoint', body[12],
                           struct.unpack('>H', body[14:16])[0]))
            losses = body[13] * 4
            for index in range(16, min(len(body), 16 + losses) - 3, 4):
                events.append(('nack', body[index],
                               struct.unpack('>H', body[index + 1:index + 3])[0],
                               body[index + 3]))
    return events


def expand_frame_id(id8, latest):
    """Widen the 8-bit id a receiver reports back to a full frame id.

    A frame only carries its low byte on the wire, so "17" means 17, 273 or 529
    -- whichever is within 256 of what we last sent. Get this wrong at a 256
    boundary and an acknowledgement pops nothing, the window fills, and the
    mirror sheds every frame from then on.
    """
    if latest < 0:
        return id8
    candidate = (latest & ~0xFF) | id8
    return candidate - 256 if candidate > latest else candidate


class _CastStreamSender(object):
    """Access units in, a picture on a television out.

    Threading is deliberately lopsided: the encoder's pump thread is the only
    caller of `feed`, and it does the sending inline -- queueing frames for a
    sender thread would just add a buffer whose contents are already stale by
    the time they leave. One background thread owns the other half (the
    receiver's feedback, sender reports, kickstart probes and the Cast
    keepalive), because all four are timers rather than data.

    There is no retransmit path on purpose. This is a live desktop: the next
    frame supersedes a lost one within 42 ms, and the protocol's own recovery
    (checkpoint, PLI, the next one-second GOP) is what the reference relies on
    too.
    """

    def __init__(self, control, sock, address, ssrc, receiver_ssrc, key,
                 iv_mask, on_lost=None):
        self._control = control
        self._sock = sock
        self._address = address
        self.ssrc = ssrc
        self._receiver_ssrc = receiver_ssrc
        self._cipher = Aes128Ctr(key)
        self._iv_mask = iv_mask
        self._on_lost = on_lost
        self._units = _AccessUnits()
        self._frame_id = 0
        self._sequence = secrets.randbelow(0x10000)
        self._in_flight = deque()       # (frame id, last packet, timestamp)
        self._awaiting_key = False
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = None
        self._t0 = time.monotonic()
        self._timestamp = 0
        self._octets = 0
        self._last_sent = self._t0
        self._last_report = 0.0
        self._last_ping = 0.0
        self._last_pong = time.time()
        self._reported = False
        self.alive = False
        self.acked = -1
        self.playout_delay = 0
        #: The counting surface `_Broadcaster` offers, so the pump and `stats()`
        #: do not care which target produced these bytes.
        self.chunks = 0
        self.bytes = 0
        self.drops = 0
        self.frames = 0
        self.packets = 0

    # -- handshake -----------------------------------------------------------

    @classmethod
    def open(cls, host, port, width, height, fps, bitrate, timeout=10.0,
             on_lost=None):
        """Go all the way to a socket the receiver is listening on.

        Deliberately before the encoder starts: every failure in here is a
        reason to take the compatible LOAD path instead, and unwinding a live
        ffmpeg plus an HTTP server to make that decision would be the messy
        version of the same call.
        """
        control = _CastSender(host, port)
        control.connect()
        try:
            control.launch_mirroring(MIRROR_APP_ID, timeout=timeout)
            key, iv_mask = aes_material()
            ssrc = secrets.randbits(32) | 1
            control.send_offer(build_offer(ssrc, key, iv_mask, width, height,
                                           fps, bitrate))
            udp_port, receiver_ssrc = parse_answer(
                control.await_answer(timeout=max(2.0, timeout)))
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            # Bound, never connected: the receiver answers from a source port
            # of its own choosing, and a connected socket makes the kernel drop
            # those packets before we ever see a checkpoint.
            sock.bind(('', 0))
            return cls(control, sock, (host, udp_port), ssrc, receiver_ssrc,
                       key, iv_mask, on_lost=on_lost)
        except Exception:
            # An app we launched and then abandoned is worse than no app: the
            # next session would meet this one as the stale instance it has to
            # stop first. Only worth attempting once a socket exists.
            if control.sock is not None:
                try:
                    control.close_mirroring()
                except Exception as e:
                    logger.debug('could not leave the mirroring app: %s', e)
            _close_quietly(control)
            raise

    def start(self):
        self.alive = True
        self._last_pong = time.time()
        self._thread = threading.Thread(target=self._loop, daemon=True,
                                        name="CAST_STREAM_LOOP")
        self._thread.start()

    # -- what the pump thread calls ------------------------------------------

    def feed(self, chunk):
        self.chunks += 1
        self.bytes += len(chunk)
        for unit in self._units.feed(chunk):
            self._send_unit(unit)

    def flush(self):
        for unit in self._units.flush():
            self._send_unit(unit)

    def _send_unit(self, unit):
        is_key = _unit_is_key(unit)
        with self._lock:
            behind = len(self._in_flight) >= MAX_IN_FLIGHT_FRAMES
        if behind or (self._awaiting_key and not is_key):
            # Shed *whole* frames: half a picture stays on the screen until the
            # next IDR, which is worse than the last complete one still there.
            # Even a key frame waits when the window is full -- the next GOP is
            # a second away and the checkpoint will free the slots by then.
            self._awaiting_key = True
            self.drops += 1
            return
        frame_id = self._frame_id
        self._frame_id += 1
        now = time.monotonic()
        timestamp = int((now - self._t0) * VIDEO_CLOCK) & 0xFFFFFFFF
        cipher = self._cipher.crypt(frame_iv(self._iv_mask, frame_id), unit)
        packets, self._sequence = cast_packets(
            cipher, frame_id, is_key, frame_id if is_key else frame_id - 1,
            self.ssrc, self._sequence, timestamp)
        try:
            for packet in packets:
                self._sock.sendto(packet, self._address)
        except OSError:
            # Teardown closed the media socket under us. Counting a frame the
            # receiver never got would be the worse lie; hand the slot back.
            self._frame_id -= 1
            self._awaiting_key = True
            raise
        with self._lock:
            self._in_flight.append((frame_id, packets[-1], timestamp))
            self._timestamp = timestamp
            self._octets = (self._octets + len(cipher)) & 0xFFFFFFFF
            self.packets += len(packets)
            self.frames += 1
            self._last_sent = now
            if is_key:
                self._awaiting_key = False
            first = not self._reported
        if first:
            self._send_report()

    # -- timers and feedback (the one background thread) ---------------------

    def _send_report(self):
        self._reported = True
        self._last_report = time.monotonic()
        with self._lock:
            packet = rtcp_sender_report(self.ssrc, ntp_timestamp(),
                                        self._timestamp, self.packets,
                                        self._octets)
        try:
            self._sock.sendto(packet, self._address)
        except OSError as e:
            logger.debug('sender report lost: %s', e)

    def _kickstart(self):
        with self._lock:
            packet = self._in_flight[-1][1] if self._in_flight else None
        if packet is None:
            return
        self._sequence = (self._sequence + 1) & 0xFFFF
        resend = packet[:2] + struct.pack('>H', self._sequence) + packet[4:]
        try:
            self._sock.sendto(resend, self._address)
        except OSError:
            pass

    def _on_rtcp(self, data):
        for event in parse_rtcp(data, self.ssrc):
            if event[0] == 'picture-loss':
                self._awaiting_key = True
            elif event[0] == 'checkpoint':
                with self._lock:
                    acked = expand_frame_id(event[1], self._frame_id - 1)
                    while self._in_flight and self._in_flight[0][0] <= acked:
                        self._in_flight.popleft()
                    self.acked = max(self.acked, acked)
                    self.playout_delay = event[2]
            # 'nack': noted and dropped. See the class docstring.

    def _keepalive(self):
        self._last_ping = time.monotonic()
        try:
            self._control.ping()
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.debug('keepalive could not be sent: %s', e)
            return
        while True:
            message = self._control.poll()
            if message is None:
                return
            if message.get('payload_type') != 0:
                continue
            try:
                data = json.loads(message.get('payload_utf8') or '{}')
            except ValueError:
                continue
            if data.get('type') == 'PONG':
                self._last_pong = time.time()
                return

    def _loop(self):
        self._sock.settimeout(0.1)
        while not self._stop.is_set():
            now = time.monotonic()
            try:
                data, peer = self._sock.recvfrom(65536)
            except socket.timeout:
                data = None
            except OSError:
                return
            if data and peer[0] == self._address[0]:
                self._on_rtcp(data)
            if now - self._last_report >= SR_INTERVAL:
                self._send_report()
            if now - self._last_sent >= KICKSTART_INTERVAL:
                self._kickstart()
                # Re-arm the probe ourselves: recvfrom already paced this loop,
                # so without this every 100 ms would resend the same packet.
                self._last_sent = now
            if now - self._last_ping >= CONTROL_PING_SECONDS:
                self._keepalive()
            if time.time() - self._last_pong > CONTROL_LOSS_SECONDS:
                self.alive = False
                self._stop.set()
                if self._on_lost is not None:
                    self._on_lost('电视不再应答保活（可能已关机或换了网络）')
                return

    # -- status and teardown -------------------------------------------------

    def clients(self):
        return 1 if self.alive else 0

    def in_flight(self):
        with self._lock:
            return len(self._in_flight)

    def stop(self):
        if self._stop.is_set():
            return
        self._stop.set()
        self.alive = False
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(1.0)
        self._control.close_mirroring()

    def close(self):
        self._stop.set()
        try:
            self._sock.close()
        except OSError:
            pass
        self._control.close()


def unicode_text(value):
    return value if isinstance(value, str) else str(value)


# -- the renderer ------------------------------------------------------------

class _Aborted(Exception):
    """The generation moved on while _mirror was setting up; stand down."""


class ScreenMirrorRenderer(Renderer):

    def __init__(self):
        super(ScreenMirrorRenderer, self).__init__()
        self._lock = threading.Lock()
        self._sender = None
        self._proc = None
        self._server = None
        #: The low-latency target has no HTTP server: the encoder's bytes go
        #: straight into a sender that pushes them. `_kind` says which target
        #: this is even when there is no session to ask.
        self._sink = None
        self._kind = ''
        self._awake = None
        self._generation = 0
        self._mirroring = False
        self._started_at = 0.0
        self._url = ''
        #: What the DLNA renderer last said (TransportState), for the menu.
        self._dlna_state = ''
        self.renderer_setting = ScreenMirrorSetting()

    # -- settings helpers ----------------------------------------------------

    def target(self):
        raw = Setting.get(SettingProperty.Mirror_Target, '') or ''
        name = Setting.get(SettingProperty.Mirror_Target_Name, '') or ''
        if not raw:
            return None, None, name
        host, _, port = raw.partition(':')
        try:
            port = int(port or CAST_PORT)
        except ValueError:
            port = CAST_PORT
        return host, port, name or host

    def quality(self):
        return quality_preset()

    def is_mirroring(self):
        return self._mirroring

    def playing_url(self):
        return self._url

    def viewer_url(self):
        """The /browser address for the running session, or ''."""
        with self._lock:
            server = self._server
        if server is None or server.session.kind != 'browser':
            return ''
        return page_url(server)

    def stats(self):
        """What the mirror is doing right now, for the menu and status page.

        The pump is the only thing that counts, so this costs nothing to ask --
        but it also means the numbers are only as fresh as the last chunk.
        """
        with self._lock:
            server, sink, kind = self._server, self._sink, self._kind
        #: Every target counts bytes the same way; only the low-latency one has
        #: no HTTP server holding a broadcaster to count them with.
        source = server.broadcaster if server is not None else sink
        if source is None or not kind:
            return {}
        seconds = max(1.0, time.time() - self._started_at) \
            if self._started_at else 1.0
        info = {'kind': kind,
                'clients': source.clients(),
                'chunks': source.chunks,
                'bytes': source.bytes,
                'drops': source.drops,
                'mbps': round(source.bytes * 8 / 1000000.0 / seconds, 2),
                'seconds': int(time.time() - self._started_at)
                if self._started_at else 0}
        if kind == 'caststream':
            info['frames'] = sink.frames
            info['in_flight'] = sink.in_flight()
            info['delay'] = sink.playout_delay
        elif server.session.bytelog:
            # On this target the meaningful health number is how far behind
            # the TV is -- it is reading from the hoard we pre-filled.
            info['profile'] = dlna_profile_id(server.session.profile)
            info['state'] = self._dlna_state
            info['buffered'] = max(0, source.end - source.start)
        return info

    # -- Renderer API ----------------------------------------------------------

    def start(self):
        super(ScreenMirrorRenderer, self).start()
        start_search()
        # The「输出目标」submenu reads the DLNA renderer list when a TV is the
        # target -- the Chromecast search above does not fill it. Warming it
        # here is what keeps the first open of that submenu from having to say
        #「搜索中…再展开一次菜单」while it waits.
        if output_kind() == 'dlna':
            start_renderer_search()

    def set_media_url(self, url, start="0"):
        """A phone pushed something: mirror stops and the pushed url plays.

        Macast can only have one renderer, and while this one is selected the
        phone deserves the same bridge behaviour cast_bridge gives.
        """
        if not url:
            return
        if url == self._url:
            # The same address pushed twice is a client retry, not a request to
            # tear the stream down and start it again.
            logger.info('ignoring a repeat push of %s', url)
            return
        kind = output_kind()
        if kind in ('cast', 'caststream'):
            ready = bool(self.target()[0])
        elif kind == 'dlna':
            ready = bool(dlna_target()[1])
        else:
            # A browser target has nothing to bridge to: its "device" is a
            # webpage we drive by streaming, not by pushing a URL.
            ready = False
        if not ready:
            # Nothing to bridge to: this user has no device selected, and mpv
            # is not ours to drive here. Say so instead of silently swallowing
            # the push (or killing a running mirror for it).
            cherrypy.engine.publish(
                'app_notify', 'Macast',
                'Screen Mirror 无法播放推送的网址：把「输出目标」切到 '
                'Chromecast 或 DLNA 电视做中继，或换回默认渲染器播放',
                sound=False)
            return
        self._url = url
        with self._lock:
            self._generation += 1
            generation = self._generation
        self._teardown_async()
        runner = self._cast_url if kind != 'dlna' else self._dlna_url
        threading.Thread(target=runner, args=(url, generation),
                         daemon=True, name="SCREEN_MIRROR_CAST").start()
        self.set_state_transport('PLAYING')
        cherrypy.engine.publish('renderer_av_uri', url)

    def set_media_stop(self):
        with self._lock:
            self._generation += 1
        self._teardown_async()
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        logger.info('pause ignored: a live screen mirror cannot be paused')

    def set_media_resume(self):
        logger.info('resume ignored: a live screen mirror never pauses')

    def set_media_volume(self, data):
        with self._lock:
            sender = self._sender
        if sender is None:
            return
        if not hasattr(sender, 'set_volume'):
            # A DLNA renderer's volume lives in RenderingControl, a different
            # service we never resolved -- and every TV has a remote for it.
            logger.info('volume left to the renderer: this target speaks no '
                        'Cast volume channel')
            return
        try:
            sender.set_volume(int(data))
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.error('cannot set the device volume: %s', e)

    def stop(self):
        with self._lock:
            self._generation += 1
        self._teardown()
        super(ScreenMirrorRenderer, self).stop()

    # -- mirror control (menu bar) ----------------------------------------------

    def start_mirror(self):
        with self._lock:
            if self._mirroring:
                return
            self._generation += 1
            generation = self._generation
        threading.Thread(target=self._mirror, args=(generation,),
                         daemon=True, name="SCREEN_MIRROR").start()

    def stop_mirror(self):
        with self._lock:
            self._generation += 1
        self._teardown_async()

    # -- internals ---------------------------------------------------------------

    def _mirror(self, generation):
        kind = output_kind()
        host = port = None
        name = ''
        control = ''
        if kind in ('cast', 'caststream'):
            host, port, name = self.target()
            if host is None:
                self._fail('还没有选择投屏目标：在菜单栏的「输出目标」里选一台 '
                           'Chromecast 设备，或改用「浏览器」目标', generation)
                return
        elif kind == 'dlna':
            name, control = dlna_target()
            if not control:
                self._fail('还没有选择 DLNA 电视：在菜单栏的「输出目标 → DLNA '
                           '电视」里选一台，或改用「浏览器」目标', generation)
                return
        ffmpeg = find_ffmpeg()
        if ffmpeg is None:
            self._fail('找不到 ffmpeg：{}后重试'.format(
                {'darwin': 'brew install ffmpeg',
                 'win32': '安装 ffmpeg 并加入 PATH',
                 }.get(sys.platform, '用包管理器安装 ffmpeg')), generation)
            return
        capture = probe_capture(ffmpeg)
        if capture is None:
            self._fail(capture_unavailable_hint(), generation)
            return
        height, bitrate = self.quality()
        encoder = encoder_kind()
        if encoder == 'hardware' and not has_hardware_encoder(ffmpeg):
            logger.warning('h264_videotoolbox is unavailable; using libx264')
            encoder = 'software'
        first_bytes = threading.Event()
        tail = deque(maxlen=20)
        stream = None
        if kind == 'caststream':
            # Ask the device first. Its mirroring app is the part of this
            # target we cannot self-prove, and a device that refuses it is
            # common enough that the answer is worth having *before* an encoder
            # and an HTTP server exist to unwind.
            width, pinned, _filter = cast_stream_shape(height)
            try:
                stream = _CastStreamSender.open(
                    host, port, width, pinned, FPS,
                    min(bitrate, CAST_STREAM_MAX_BITRATE),
                    on_lost=self._stream_lost)
            except Exception as e:
                logger.warning('Cast Streaming refused (%s); using LOAD', e)
                kind = 'cast'
        session = _Session(kind, has_audio=bool(capture.audio_map),
                           title=socket.gethostname() or 'Macast',
                           profile=dlna_profile() if kind == 'dlna' else None)
        handed = False
        try:
            server = None if stream is not None else start_stream_server(session)
            proc = subprocess.Popen(
                build_ffmpeg_command(ffmpeg, capture, height, bitrate,
                                     kind=kind, encoder=encoder),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                stdin=subprocess.DEVNULL,
                env=_clean_env())
            with self._lock:
                if generation != self._generation:
                    raise _Aborted()
                # Owned from here on: the pump thread reports the encoder's
                # death through _teardown(), so _mirror must not also clean up.
                self._proc = proc
                self._server = server
                self._sink = stream
                self._kind = kind
                handed = True
            threading.Thread(
                target=_pump,
                args=(proc, server.broadcaster if stream is None else stream,
                      self, generation, first_bytes),
                daemon=True, name="SCREEN_MIRROR_PUMP").start()
            threading.Thread(target=_drain_stderr, args=(proc, tail),
                             daemon=True, name="SCREEN_MIRROR_LOG").start()
            url = ('' if stream is not None else
                   dlna_stream_url(server, _url_host(control))
                   if kind == 'dlna' else stream_url(server))
            if stream is not None:
                stream.start()
            # Wait for the encoder to actually produce something: a Screen
            # Recording denial exits in under a second, and the pump is
            # reporting that while we wait.
            first_bytes.wait(timeout=3.0)
            if proc.poll() is not None or generation != self._generation:
                raise _Aborted()
            if kind == 'dlna':
                self._prefill(server, proc, generation)
                if generation != self._generation:
                    raise _Aborted()
            sender = None
            if kind == 'cast':
                sender = _CastSender(host, port)
                sender.connect()
                sender.launch()
                sender.load(url, content_type=session.content_type, live=True)
            elif kind == 'dlna':
                sender = _DlnaSender(control)
                sender.set_uri(url, '屏幕镜像 · {}'.format(session.page_title),
                               session.profile)
                sender.play()
        except _Aborted:
            if stream is not None and not handed:
                # Nobody else ever held this sender, and the mirroring app is
                # already up on the device: leave without saying goodbye and
                # the next session meets the stale instance it refuses.
                _cleanup(None, None, None, stream)
            self._teardown()
            return
        except Exception as e:
            if stream is not None and not handed:
                _cleanup(None, None, None, stream)
            self._teardown()
            detail = str(e).strip() or ' '.join(list(tail)[-3:])
            target_label = name if kind in ('cast', 'caststream', 'dlna') \
                else '浏览器'
            self._fail('镜像到 {} 启动失败：{}'.format(target_label, detail),
                       generation)
            return
        # Spawned outside the lock: a menu click that aborts this session must
        # not queue behind an OS call.
        awake = _keep_awake()
        with self._lock:
            if generation != self._generation:
                self._teardown()
                _stop_awake(awake)
                return
            self._sender = sender
            self._awake = awake
            self._mirroring = True
            self._started_at = time.time()
            self._url = url
        self.set_state_transport('PLAYING')
        if kind == 'dlna':
            # The renderer needs a push of its own to recover from the pauses
            # and seek-stalls old firmware does on a stream it thinks is a
            # file. It is ours to hold, so it dies with the generation.
            threading.Thread(target=self._watch_dlna,
                             args=(sender, url, session, generation),
                             daemon=True, name="SCREEN_MIRROR_DLNA_WATCH").start()
            message = '已开始镜像到 {}（档位 {}，约 {} 秒延迟）'.format(
                name, session.profile.label,
                max(1, DLNA_PREFILL_BYTES * 8 // session.profile.bitrate))
        elif kind == 'browser':
            message = '镜像已开始，浏览器打开：{}'.format(page_url(server))
        elif kind == 'caststream':
            message = '已开始低延迟镜像到 {}（此通道还没有声音）'.format(name)
        else:
            message = '已开始镜像到 {}'.format(name)
        cherrypy.engine.publish('app_notify', 'Macast', message)
        logger.info('mirroring screen (%s) to %s via %s', kind, name or 'LAN',
                    url)

    def _prefill(self, server, proc, generation):
        """Hold the URL back until the ring holds `DLNA_PREFILL_BYTES`.

        The renderer's first move is a bounded sniff of a few megabytes; if we
        have to answer it by waiting on the encoder, the TV concludes the file
        is broken. Waiting here is what buys a steady read -- and the latency
        this target has, which is why the start message says so.
        """
        log = server.broadcaster
        deadline = time.time() + DLNA_PREFILL_TIMEOUT
        while time.time() < deadline and generation == self._generation:
            if log.bytes >= DLNA_PREFILL_BYTES or proc.poll() is not None:
                break
            time.sleep(0.1)
        else:
            logger.info('prefill stopped at %s bytes', log.bytes)

    def _dlna_url(self, url, generation):
        """Bridge path for a DLNA renderer: play a pushed URL, no capture."""
        name, control = dlna_target()
        if not control:
            self._fail('还没有选择 DLNA 电视：在菜单栏的「输出目标 → DLNA 电视」'
                       '里选一台', generation)
            return
        profile = dlna_profile()
        try:
            sender = _DlnaSender(control)
            sender.set_uri(url, 'Macast · {}'.format(name), profile)
            sender.play()
        except Exception as e:
            self._fail('投给 {} 失败：{}'.format(name, e), generation)
            return
        with self._lock:
            if generation != self._generation:
                _close_quietly(sender)
                return
            self._sender = sender
        logger.info('cast %s to %s via %s', url, name, control)

    def _watch_dlna(self, sender, url, session, generation):
        """Keep a renderer that stopped on its own going.

        Old firmware pauses, seeks into a corner of the fake file, or just
        drops out of PLAYING when a sniff came back short. `GetTransportInfo`
        every few seconds is the only window into that; the success proof is
        `RelTime` actually moving, because a renderer will happily report
        PLAYING while it sits on a frozen frame.
        """
        misses = 0
        last_position = None
        while True:
            with self._lock:
                if generation != self._generation:
                    return
            time.sleep(DLNA_POLL_SECONDS)
            try:
                state = sender.transport_state()
                position = sender.position()
            except Exception as e:
                logger.debug('DLNA poll failed: %s', e)
                misses += 1
                if misses >= DLNA_MAX_REPUSHES:
                    self._give_up(sender, '电视不再应答（可能已关机，或换了网络）')
                    return
                continue
            if state == 'PLAYING':
                if position and position != last_position:
                    last_position = position
                    misses = 0
                with self._lock:
                    self._dlna_state = state
                continue
            with self._lock:
                self._dlna_state = state
            misses += 1
            if misses >= DLNA_MAX_REPUSHES:
                self._give_up(sender, '电视连续 {} 次没有播起来'.format(misses))
                return
            logger.info('renderer is %s; pushing again (%d/%d)',
                        state, misses, DLNA_MAX_REPUSHES)
            try:
                sender.set_uri(url, '屏幕镜像 · {}'.format(session.page_title),
                               session.profile)
                sender.play()
            except Exception as e:
                logger.debug('re-push failed: %s', e)

    def _give_up(self, sender, reason):
        """Stop blaming the TV one poll at a time and tell the user why."""
        current = dlna_profile_id(dlna_profile())
        nxt = profile_order()[0]
        self._fail('{}：当前档位是「{}」。在菜单栏「兼容档位」里换成「{}」再试一次'.format(
            reason, DLNA_PROFILES[current].label,
            DLNA_PROFILES[nxt].label),
                   self._generation)

    def _stream_lost(self, reason):
        """The media loop stopped believing the television is there.

        Bumping the generation is what silences the pump: teardown kills the
        encoder, and without it the resulting exit code 15 would be reported as
        a capture failure on top of the real reason.
        """
        with self._lock:
            self._generation += 1
            generation = self._generation
        self._fail(reason, generation)
        self._teardown()

    def _cast_url(self, url, generation):
        """Bridge path: play a finished URL on the device, no capture."""
        host, port, name = self.target()
        if host is None:
            self._fail('还没有选择投屏目标：在菜单栏的「输出目标 → Chromecast」'
                       '里选一台设备', generation)
            return
        try:
            sender = _CastSender(host, port)
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
        logger.info('cast %s to %s (%s:%s)', url, name, host, port)

    def _encoder_died(self, generation, proc, started_at):
        """ffmpeg exited while we still wanted it running."""
        with self._lock:
            if generation != self._generation:
                return
        code = proc.poll()
        if time.time() - started_at < EARLY_DEATH_SECONDS:
            hint = ('：若是首次使用，请在「系统设置 → 隐私与安全性 → 屏幕录制」'
                    '中允许 Macast' if sys.platform == 'darwin' else '')
            self._fail('屏幕采集启动失败（ffmpeg 退出码 {}）{}'.format(code, hint),
                       generation)
        else:
            self._fail('屏幕采集中断（ffmpeg 退出码 {}）'.format(code), generation)
        self._teardown()

    def _fail(self, message, generation):
        with self._lock:
            if generation != self._generation:
                return
            self._mirroring = False
        logger.error(message)
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        cherrypy.engine.publish('app_notify', 'Macast', message)

    def _teardown_async(self):
        threading.Thread(target=self._teardown, daemon=True,
                         name="SCREEN_MIRROR_TEARDOWN").start()

    def _teardown(self):
        with self._lock:
            sender, proc, server = self._sender, self._proc, self._server
            sink = self._sink
            self._sender = self._proc = self._server = None
            self._sink = None
            self._kind = ''
            self._mirroring = False
            # Claim the assert here so a second teardown (the encoder dying
            # right after a manual stop) cannot release someone else's.
            awake, self._awake = self._awake, None
        _cleanup(sender, proc, server, sink)
        if awake is not None:
            _stop_awake(awake)


def _cleanup(sender, proc, server, sink=None):
    for thing in (sender, sink):
        if thing is not None:
            thing.stop()
            _close_quietly(thing)
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
    if server is not None:
        # A DLNA reader is parked in _ByteLog.read() waiting for the next
        # byte; nothing else releases it before server_close() would block.
        close = getattr(server.broadcaster, 'close', None)
        if close is not None:
            close()
        server.shutdown()
        server.server_close()


def _close_quietly(sender):
    try:
        sender.close()
    except Exception:
        pass


def _pump(proc, broadcaster, owner, generation, first_bytes):
    """stdout -> broadcaster, and notice when the encoder dies."""
    started = time.time()
    stdout = proc.stdout
    while True:
        chunk = stdout.read(CHUNK)
        if not chunk:
            break
        broadcaster.feed(chunk)
        first_bytes.set()
    flush = getattr(broadcaster, 'flush', None)
    if flush is not None:
        # Only the low-latency sink has one: the last picture is still inside
        # the splitter when stdout closes, and an unterminated access unit is
        # a picture nobody ever sees.
        try:
            flush()
        except OSError:
            pass
    proc.wait()
    owner._encoder_died(generation, proc, started)


def _clean_env():
    """ffmpeg inherits the shell environment; proxies must not intercept the TV."""
    env = dict(os.environ)
    for key in ('http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY',
                'all_proxy', 'ALL_PROXY', 'no_proxy', 'NO_PROXY'):
        env.pop(key, None)
    return env


def _keep_awake(platform=None):
    """Return a handle on a sleep assertion, or None where we have no way.

    JustStream's most common complaint is the stream dying when the Mac falls
    asleep mid-presentation; `caffeinate -dimsu` is the stock answer (display,
    idle, system and disk assertions) and needs no entitlement. The handle is
    handed back so teardown can drop the assertion instead of leaving the
    machine awake forever.
    """
    if (platform or sys.platform) != 'darwin':
        return None
    try:
        return subprocess.Popen(['caffeinate', '-dimsu'],
                               stdin=subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL)
    except Exception as e:
        # Not having caffeinate is no reason to refuse the mirror.
        logger.info('cannot keep the machine awake: %s', e)
        return None


def _stop_awake(handle):
    if handle is None:
        return
    try:
        if handle.poll() is None:
            handle.terminate()
            handle.wait(timeout=3)
    except Exception:
        pass


def _drain_stderr(proc, tail):
    """Move ffmpeg's stderr into a bounded buffer, forever.

    An unread stderr pipe fills up and ffmpeg blocks inside the encoder -- which
    looks exactly like a dead mirror, with a healthy process and a stalled
    stream. The last few lines are kept because they are the only explanation a
    startup failure ever gives.
    """
    try:
        for raw in iter(proc.stderr.readline, b''):
            line = raw.decode('utf-8', 'replace').rstrip()
            if not line:
                continue
            tail.append(line)
            logger.debug('ffmpeg: %s', line)
    except Exception as e:
        logger.debug('stderr drain ended: %s', e)
    finally:
        try:
            proc.stderr.close()
        except Exception:
            pass



class ScreenMirrorSetting(RendererSetting):

    def _renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        renderer = renderers.pop() if renderers else None
        return renderer if isinstance(renderer, ScreenMirrorRenderer) else None

    def build_menu(self):
        kind = output_kind()
        now = time.time()
        # Only look when there is nothing to show AND the last answer has gone
        # stale -- otherwise a menu opened a second apart re-ran the multicast
        # search each time and could never report what it had already found.
        if (kind in ('cast', 'caststream') and not _devices
                and not _searching and _search_due(_searched_at, now)):
            start_search()
        if (kind == 'dlna' and not _dlna_devices
                and not _dlna_searching
                and _search_due(_dlna_searched_at, now)):
            start_renderer_search()
        renderer = self._renderer()
        mirroring = bool(renderer and renderer.is_mirroring())

        items = [
            MenuItem('Screen Mirror v0.9', enabled=False),
            MenuItem('停止镜像' if mirroring else '开始镜像',
                     self.on_toggle_clicked),
            MenuItem('输出目标', children=self._output_children(kind)),
        ]
        if kind == 'dlna':
            items.append(MenuItem('兼容档位', children=self._profile_children()))
        if kind == 'browser' and mirroring:
            items.append(MenuItem('复制观看地址', self.on_copy_url_clicked))
            items.append(MenuItem(renderer.viewer_url(), enabled=False))
        items.append(MenuItem('画质', children=self._quality_children()))
        note = self._quality_note()
        if note:
            items.append(MenuItem(note, enabled=False))
        screen_children = self._screen_children()
        if screen_children:
            items.append(MenuItem('采集屏幕', children=screen_children))
        #: The pointer flag reaches ffmpeg through the capture probe, so the
        #: menu shows the stored wish, not what the running stream does --
        #: changing it restarts the mirror (see on_cursor_clicked).
        items.append(MenuItem('显示鼠标指针', self.on_cursor_clicked,
                              checked=cursor_enabled()))
        if sys.platform == 'darwin':
            items.append(MenuItem(
                '硬件编码（VideoToolbox）', self.on_encoder_clicked,
                checked=str(Setting.get(SettingProperty.Mirror_Encoder,
                                        'software')) == 'hardware'))
            audio_children = [MenuItem(self._audio_line(), enabled=False)]
            if audio_setup_needed():
                audio_children.append(
                    MenuItem('一键设置（BlackHole + 多输出设备）',
                             self.on_audio_setup_clicked))
            if Setting.get(SettingProperty.Mirror_Audio_Original, None) not in (
                    None, ''):
                audio_children.append(
                    MenuItem('恢复原声音输出', self.on_audio_restore_clicked))
            items.append(MenuItem('系统声音', children=audio_children))
        else:
            items.append(MenuItem(self._audio_line()))
        if mirroring:
            items.append(MenuItem(self._status_line(renderer), enabled=False))
        return items

    def _output_children(self, kind):
        children = [MenuItem(OUTPUTS[key][0], self.on_output_clicked,
                             checked=(kind == key), data=key)
                    for key in ('cast', 'caststream', 'dlna', 'browser')]
        if kind in ('cast', 'caststream'):
            # Both targets are the same device reached a different way, so the
            # discovered list hangs under either choice.
            children.append(MenuItem('— — —', enabled=False))
            children.extend(self._device_children())
        elif kind == 'dlna':
            children.append(MenuItem('— — —', enabled=False))
            children.extend(self._renderer_children())
        return children

    def _renderer_children(self):
        """DLNA renderers from the last background SSDP search.

        Cached like the Chromecast list: the menu is built on the UI thread and
        a search sends multicast and waits seconds for replies.
        """
        name, control = dlna_target()
        children = []
        for device_name, device_control, host in list(_dlna_devices):
            children.append(MenuItem('{} · {}'.format(device_name, host),
                                     self.on_dlna_target_clicked,
                                     checked=(device_control == control),
                                     data=(device_name, device_control)))
        if not children:
            #「搜索中」is only honest while the very first look is in flight:
            # once a search has completed, an empty list is a fact to state
            # (with the time it was established), not a reason to claim the
            # search is still running.
            if _dlna_searching and not _dlna_searched_at:
                children.append(MenuItem('搜索中…再展开一次菜单', enabled=False))
            else:
                children.append(MenuItem(
                    '没有发现 DLNA 电视' + _searched_suffix(_dlna_searched_at),
                    enabled=False))
        children.append(MenuItem('重新搜索', self.on_refresh))
        return children

    def _profile_children(self):
        current = dlna_profile_id(dlna_profile())
        return [MenuItem(profile.label, self.on_profile_clicked,
                         checked=(current == key), data=key)
                for key, profile in DLNA_PROFILES.items()]

    def _device_children(self):
        """The discovered Chromecasts, as the v0.3 menu had them."""
        current = Setting.get(SettingProperty.Mirror_Target, '') or ''
        children = []
        for name, host, port in list(_devices):
            target = '{}:{}'.format(host, port)
            children.append(MenuItem('{} · {}'.format(name, host),
                                     self.on_target_clicked,
                                     checked=(target == current),
                                     data=(name, target)))
        if not children:
            if _searching and not _searched_at:
                children.append(MenuItem('搜索中…再展开一次菜单', enabled=False))
            else:
                children.append(MenuItem(
                    '没有发现 Chromecast' + _searched_suffix(_searched_at),
                    enabled=False))
        children.append(MenuItem('重新搜索', self.on_refresh))
        return children

    def _quality_children(self):
        quality = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
        labels = {'360': '360p · 2.5 Mbps（省带宽）',
                  '720': '720p · 5 Mbps（默认）',
                  '1080': '1080p · 10 Mbps（更吃带宽）',
                  'source': '原始分辨率 · 12 Mbps（不缩放）'}
        return [MenuItem(labels[key], self.on_quality_clicked,
                         checked=(quality == key), data=key)
                for key in ('360', '720', '1080', 'source')]

    def _quality_note(self):
        """The low-latency channel cannot spend what the menu offers."""
        if output_kind() != 'caststream':
            return None
        width, height, _filter = cast_stream_shape(quality_preset()[0])
        return ('低延迟通道上限 {:.1f} Mbps（软件加密跟不上）· '
                '帧尺寸固定 {}x{}（信箱化）'.format(
                    CAST_STREAM_MAX_BITRATE / 1000000.0, width, height))

    def _screen_children(self):
        """One entry per display the last probe saw; empty when it has no list.

        The list comes from the cache instead of a fresh probe because
        build_menu runs on the UI thread and probing spawns ffmpeg.
        """
        capture = next(iter(_capture_cache.values()), None)
        if capture is None or not capture.screens:
            return []
        wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
        children = [MenuItem('第一块屏幕（默认）', self.on_screen_clicked,
                             checked=(wanted == ''), data='')]
        for index, name in capture.screens:
            children.append(MenuItem('{} · {}'.format(index, name),
                                     self.on_screen_clicked,
                                     checked=(wanted == str(index)),
                                     data=str(index)))
        return children

    @staticmethod
    def _status_line(renderer):
        """Live throughput, so 'it is fine, it is just 200 kbps' is visible."""
        stats = renderer.stats()
        if not stats:
            return '状态：正在启动…'
        if stats.get('kind') == 'dlna':
            minutes, seconds = divmod(stats['seconds'], 60)
            return '已镜像 {:d}:{:02d} · 档位 {} · 电视 {} · 缓冲 {:.0f} MiB'.format(
                minutes, seconds, stats.get('profile', '?'),
                stats.get('state') or '未上报',
                max(0, stats.get('buffered', 0)) / 1048576.0)
        minutes, seconds = divmod(stats['seconds'], 60)
        if stats.get('kind') == 'caststream':
            #: Frames in the receiver's own words: 在途 is how many pictures it
            #: has not acknowledged, and 丢帧 counts the ones we shed because
            #: that number hit its ceiling.
            return '已镜像 {:d}:{:02d} · {:.1f} Mbps · {:d} 帧 · 在途 {:d} · 丢帧 {:d}'.format(
                minutes, seconds, stats['mbps'], stats['frames'],
                stats['in_flight'], stats['drops'])
        return '已镜像 {:d}:{:02d} · {:.1f} Mbps · {:d} 个观看端 · 丢块 {:d}'.format(
            minutes, seconds, stats['mbps'], stats['clients'], stats['drops'])


    @staticmethod
    def _audio_line():
        """What the last probe decided about system audio (never probes:
        build_menu runs on the UI thread)."""
        capture = next(iter(_capture_cache.values()), None)
        if capture is None:
            return '系统声音：开始镜像后这里会显示'
        if capture.audio_map:
            return '系统声音：已启用 · {}'.format(capture.label)
        if sys.platform == 'darwin':
            return '系统声音：未启用（可一键安装 BlackHole）'
        if sys.platform == 'win32':
            return '系统声音：Windows 下仅画面'
        return '系统声音：未启用（需要 PulseAudio）'

    def on_audio_setup_clicked(self, item):
        if _audio_setup_busy.is_set():
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '系统声音设置已在进行中', sound=False)
            return
        _audio_setup_busy.set()
        threading.Thread(target=_audio_setup_worker, daemon=True,
                         name="SCREEN_MIRROR_AUDIO_SETUP").start()
        cherrypy.engine.publish('app_notify', 'Macast',
                                '开始一键设置系统声音…', sound=False)

    def on_audio_restore_clicked(self, item):
        restore_system_audio(
            lambda message: cherrypy.engine.publish('app_notify', 'Macast',
                                                    message, sound=False))

    def on_toggle_clicked(self, item):
        renderer = self._renderer()
        if renderer is None:
            cherrypy.engine.publish('app_notify', 'Macast',
                                    'Screen Mirror 未选中为当前渲染器')
            return
        if renderer.is_mirroring():
            renderer.stop_mirror()
            cherrypy.engine.publish('app_notify', 'Macast', '已停止镜像',
                                    sound=False)
        else:
            renderer.start_mirror()

    def on_target_clicked(self, item):
        name, target = item.data
        Setting.set(SettingProperty.Mirror_Target, target)
        Setting.set(SettingProperty.Mirror_Target_Name, name)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '镜像目标：{}'.format(name), sound=False)
        self._restart()

    def on_dlna_target_clicked(self, item):
        name, control = item.data
        Setting.set(SettingProperty.Mirror_Dlna_Control, control)
        Setting.set(SettingProperty.Mirror_Target_Name, name)
        cherrypy.engine.publish('app_notify', 'Macast',
                                'DLNA 电视：{}'.format(name), sound=False)
        self._restart()

    def on_output_clicked(self, item):
        """Switching target switches the muxer too, so the running pipeline is
        already wrong the moment the setting changes -- restart it."""
        if item.data == output_kind():
            return
        Setting.set(SettingProperty.Mirror_Output, item.data)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '输出目标：{}'.format(OUTPUTS[item.data][0]),
                                sound=False)
        self._restart()

    def on_screen_clicked(self, item):
        if item.data:
            Setting.set(SettingProperty.Mirror_Screen, item.data)
        else:
            Setting.unset(SettingProperty.Mirror_Screen)
        # The choice is baked into the probe result, not read per frame.
        invalidate_capture_cache()
        cherrypy.engine.publish('app_notify', 'Macast',
                                '采集屏幕：{}（下次镜像生效）'.format(item.text),
                                sound=False)

    def on_cursor_clicked(self, item):
        want = not cursor_enabled()
        Setting.set(SettingProperty.Mirror_Cursor, want)
        invalidate_capture_cache()
        cherrypy.engine.publish('app_notify', 'Macast',
                                '鼠标指针：{}（下次镜像生效）'.format(
                                    '显示' if want else '不显示'), sound=False)

    def on_encoder_clicked(self, item):
        """Only store the wish. Probing `ffmpeg -encoders` here would spawn a
        process on the UI thread; _mirror() falls back to libx264, with a
        warning, when the tap is not actually there."""
        want = 'software' if encoder_kind() == 'hardware' else 'hardware'
        Setting.set(SettingProperty.Mirror_Encoder, want)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '编码器：{}（下次镜像生效）'.format(
                                    'VideoToolbox 硬件编码' if want == 'hardware'
                                    else '软件编码'), sound=False)

    def on_copy_url_clicked(self, item):
        renderer = self._renderer()
        url = renderer.viewer_url() if renderer is not None else ''
        if not url:
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '现在没有镜像到浏览器', sound=False)
            return
        try:
            import pyperclip
            pyperclip.copy(url)
            message = '观看地址已复制：{}'.format(url)
        except Exception as e:
            logger.info('cannot reach the clipboard: %s', e)
            message = '观看地址：{}'.format(url)
        cherrypy.engine.publish('app_notify', 'Macast', message, sound=False)

    def _restart(self):
        renderer = self._renderer()
        if renderer is not None and renderer.is_mirroring():
            renderer.stop_mirror()
            renderer.start_mirror()

    def on_quality_clicked(self, item):
        Setting.set(SettingProperty.Mirror_Quality, item.data)
        renderer = self._renderer()
        if renderer is not None and renderer.is_mirroring():
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '画质将在下次镜像时生效', sound=False)

    def on_profile_clicked(self, item):
        """A profile is a different encoder, muxer and advertised file, so the
        running stream is wrong the instant the choice changes. Unlike quality
        this one restarts immediately: the menu offers five shapes precisely
        because the TV rejects the first one, and making the user toggle the
        mirror again after each try is busywork."""
        if item.data == dlna_profile_id(dlna_profile()):
            return
        Setting.set(SettingProperty.Mirror_Dlna_Profile, item.data)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '兼容档位：{}'.format(DLNA_PROFILES[item.data].label),
                                sound=False)
        self._restart()

    def on_refresh(self, item):
        if output_kind() == 'dlna':
            start_renderer_search()
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '正在搜索 DLNA 电视…', sound=False)
            return
        start_search()
        cherrypy.engine.publish('app_notify', 'Macast', '正在搜索 Chromecast…',
                                sound=False)


if __name__ == '__main__':
    gui(ScreenMirrorRenderer())
