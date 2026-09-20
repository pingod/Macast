# Screen Mirror for Macast
#
# Macast Metadata
# <macast.title>Screen Mirror</macast.title>
# <macast.renderer>ScreenMirrorRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.4</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Mirror this Mac/PC/desktop screen to a Chromecast on the LAN, or to any browser on the LAN (open a URL -- no app needed on the TV). ffmpeg captures (avfoundation / gdigrab / x11grab), hard-encodes to H.264, and a live stream is served from this machine: MPEG-TS LOADed on the TV for Chromecast, or fragmented MP4 played in a bundled web page. System audio rides along where a tap exists: macOS gets a one-click assisted install (official BlackHole pkg, sha256-verified, plus an auto-created multi-output device), Linux uses the PulseAudio monitor; Windows is video only. Also selectable: which display, cursor or no cursor, four quality presets, and VideoToolbox hardware encoding.</macast.desc>
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
#   * the Cast sequence is the sender-side one: deviceauth CHALLENGE ->
#     CONNECT receiver-0 -> LAUNCH(CC1AD845) -> CONNECT <transportId> ->
#     LOAD with streamType LIVE. Framing is reused from
#     macast.protocol_cast, so no pychromecast dependency (a single-file
#     plugin cannot install pip packages);
#   * ffmpeg is located via PATH *and* the usual install directories, because
#     a menu-bar app launched from Finder does not inherit the shell PATH;
#   * capture fails closed: without Screen Recording permission (macOS)
#     ffmpeg exits within a second, and the plugin turns that into a
#     message naming the System Settings pane instead of a silent nothing.

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
}
DEFAULT_OUTPUT = 'cast'
BROWSER_PATH = '/browser'

_devices = []
_searching = False
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
    global _devices, _searching
    try:
        found = discover()
        if found:
            _devices = found
    finally:
        with _search_lock:
            _searching = False


class SettingProperty(Enum):
    Mirror_Target = 1
    Mirror_Target_Name = 2
    Mirror_Quality = 3
    #: CoreAudio id of the aggregate we created, kept so a second run reuses
    #: it instead of stacking another one.
    Mirror_Audio_Aggregate = 4
    #: CoreAudio id of the user's real speakers, so「恢复原声音输出」can go back.
    Mirror_Audio_Original = 5
    #: 'cast' | 'browser' -- see OUTPUTS
    Mirror_Output = 6
    #: avfoundation video device index to capture; '' means "first screen".
    Mirror_Screen = 7
    #: draw the pointer. On by default; JustStream calls it「显示鼠标指针」.
    Mirror_Cursor = 8
    #: 'software' | 'hardware' (h264_videotoolbox, macOS only)
    Mirror_Encoder = 9


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
    """'cast' | 'browser', validated against OUTPUTS."""
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


def _avfoundation_lists(ffmpeg):
    """(video device names, audio device names) from -list_devices.

    The avfoundation input index is the position inside the respective list.
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

    def _section(marker):
        tail = text.split(marker)
        if len(tail) < 2:
            return []
        body = tail[1]
        for other in ('Video devices:', 'Audio devices:'):
            body = body.split(other)[0]
        return re.findall(r'"([^"]*)"', body)

    return _section('Video devices:'), _section('Audio devices:')


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
                         encoder='software'):
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map:
        cmd += ['-map', capture.audio_map,
                '-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2']
    else:
        cmd += ['-an']
    if height:
        # -2 keeps the aspect ratio and still satisfies yuv420p's even edges.
        cmd += ['-vf', 'scale=-2:{}'.format(height)]
    cmd += encoder_args(encoder)
    cmd += ['-pix_fmt', 'yuv420p', '-g', str(FPS), '-b:v', str(bitrate)]
    cmd += OUTPUTS[kind][3]
    return cmd


def capture_unavailable_hint():
    if sys.platform == 'darwin':
        return 'ffmpeg 没有列出任何屏幕采集设备（avfoundation）'
    if sys.platform == 'win32':
        return '这个 ffmpeg 构建不支持 gdigrab'
    return '没有 DISPLAY：x11grab 只认 X11 会话（纯 Wayland 桌面请暂用 DLNA 投屏）'


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


def _download_blackhole():
    """Official pkg to a temp file, sha256-verified. Returns the path."""
    import hashlib
    import tempfile

    import requests
    url, expected = BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256
    try:
        meta = requests.get(BLACKHOLE_CASK_API, timeout=10).content
        url, expected = blackhole_pkg_source(meta)
    except Exception as e:
        logger.info('cask API unavailable (%s), using the pinned pkg', e)
    path = os.path.join(tempfile.mkdtemp(prefix='macast-blackhole-'),
                        'BlackHole2ch.pkg')
    digest = hashlib.sha256()
    try:
        with requests.get(url, stream=True, timeout=60) as response:
            response.raise_for_status()
            with open(path, 'wb') as handle:
                for chunk in response.iter_content(65536):
                    handle.write(chunk)
                    digest.update(chunk)
    except Exception:
        _remove_quietly(path)
        raise
    if digest.hexdigest() != expected:
        _remove_quietly(path)
        raise RuntimeError('BlackHole 安装包校验失败（sha256 不匹配）')
    return path


def _remove_quietly(path):
    try:
        os.remove(path)
    except OSError:
        pass


def _wait_for_blackhole(ffmpeg, timeout=300.0):
    """Poll until ffmpeg lists the device (the installer needs a password)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _has_blackhole(ffmpeg):
            return True
        time.sleep(5.0)
    return _has_blackhole(ffmpeg)


def _route_audio_through_blackhole(ffmpeg):
    """Create/reuse the multi-output aggregate and make it the default output.

    Without it, installing BlackHole and selecting it as the output means the
    user hears nothing; the aggregate feeds BlackHole *and* the speakers.
    Returns True on success, False if the caller should show manual steps.
    """
    try:
        devices = _audio_devices()
    except Exception as e:
        logger.error('CoreAudio query failed: %s', e)
        return False
    if not devices:
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
                return _set_default_output_and_remember(device_id)
    if blackhole is None:
        # Installed but CoreAudio has not surfaced it by UID yet.
        return False
    speakers = _default_output()
    if speakers is None:
        speakers = next((device_id for device_id, uid in devices
                         if 'blackhole' not in uid.lower()), None)
    if speakers is None or speakers == blackhole[0]:
        return False
    created = _create_aggregate([blackhole[1], dict(devices)[speakers]])
    if created is None:
        return False
    return _set_default_output_and_remember(created)


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


def setup_system_audio(report=lambda message: None):
    """One-click: install BlackHole if absent, then route audio through it.

    Runs in a worker thread -- it spawns ffmpeg, waits on the installer, and
    must never touch the UI thread.
    """
    if sys.platform != 'darwin':
        report('辅助安装仅适用于 macOS')
        return False
    ffmpeg = find_ffmpeg()
    if ffmpeg is None:
        report('找不到 ffmpeg：先 brew install ffmpeg')
        return False
    try:
        if not _has_blackhole(ffmpeg):
            report('正在下载 BlackHole 2ch（官方安装包，sha256 校验）…')
            path = _download_blackhole()
            report('正在打开安装器：请在弹窗里点「安装」并输入一次密码')
            subprocess.run(['open', path], timeout=10)
            report('等待安装完成…（最长 5 分钟）')
            if not _wait_for_blackhole(ffmpeg):
                _remove_quietly(path)
                report('没有检测到 BlackHole 设备：安装被取消了吗？'
                       '可手动执行 brew install --cask blackhole-2ch')
                return False
        report('正在创建多输出设备并切换默认输出…')
        if not _route_audio_through_blackhole(ffmpeg):
            _open_audio_midi_setup(report)
            return False
        _capture_cache.clear()
        report('系统声音已就绪：开始镜像后声音会一起投出去，本机照常能听到')
        return True
    except Exception as e:
        logger.error('blackhole assisted install failed: %s', e, exc_info=True)
        report('一键设置失败：{}；可在「音频 MIDI 设置」里手动添加多输出设备'.format(e))
        return False


def _audio_setup_worker():
    """Worker for the menu item: run the assisted setup, then clear the
    in-progress flag no matter how it ends."""

    def _report(message):
        cherrypy.engine.publish('app_notify', 'Macast', message, sound=False)

    try:
        setup_system_audio(_report)
    finally:
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

    def __init__(self, kind, has_audio=False, title='Macast'):
        self.kind = kind if kind in OUTPUTS else DEFAULT_OUTPUT
        self.label, self.suffix, self.content_type, _args = OUTPUTS[self.kind]
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
    server.broadcaster = broadcaster or _Broadcaster(
        init_marker=session.init_marker)
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

    def close(self):
        if self.sock is not None:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None


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
        self._awake = None
        self._generation = 0
        self._mirroring = False
        self._started_at = 0.0
        self._url = ''
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
        key = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
        return QUALITIES.get(key, QUALITIES['720'])

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
            server = self._server
        if server is None:
            return {}
        bc = server.broadcaster
        seconds = max(1.0, time.time() - self._started_at) \
            if self._started_at else 1.0
        return {'kind': server.session.kind,
                'clients': bc.clients(),
                'chunks': bc.chunks,
                'bytes': bc.bytes,
                'drops': bc.drops,
                'mbps': round(bc.bytes * 8 / 1000000.0 / seconds, 2),
                'seconds': int(time.time() - self._started_at)
                if self._started_at else 0}

    # -- Renderer API ----------------------------------------------------------

    def start(self):
        super(ScreenMirrorRenderer, self).start()
        start_search()

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
        if output_kind() != 'cast' and self.target()[0] is None:
            # Nothing to bridge to: a browser-only user has no Chromecast, and
            # mpv is not ours to drive here. Say so instead of silently
            # swallowing the push (or killing a running mirror for it).
            cherrypy.engine.publish(
                'app_notify', 'Macast',
                'Screen Mirror 无法播放推送的网址：把「输出目标」切到 '
                'Chromecast 做中继，或换回默认渲染器播放', sound=False)
            return
        self._url = url
        with self._lock:
            self._generation += 1
            generation = self._generation
        self._teardown_async()
        threading.Thread(target=self._cast_url, args=(url, generation),
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
        if kind == 'cast':
            host, port, name = self.target()
            if host is None:
                self._fail('还没有选择投屏目标：在菜单栏的「输出目标 → Chromecast」'
                           '里选一台设备，或改用「浏览器」目标', generation)
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
        session = _Session(kind, has_audio=bool(capture.audio_map),
                           title=socket.gethostname() or 'Macast')
        try:
            server = start_stream_server(session)
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
            threading.Thread(target=_pump, args=(proc, server.broadcaster,
                                                 self, generation, first_bytes),
                             daemon=True, name="SCREEN_MIRROR_PUMP").start()
            threading.Thread(target=_drain_stderr, args=(proc, tail),
                             daemon=True, name="SCREEN_MIRROR_LOG").start()
            url = stream_url(server)
            # Wait for the encoder to actually produce something: a Screen
            # Recording denial exits in under a second, and the pump is
            # reporting that while we wait.
            first_bytes.wait(timeout=3.0)
            if proc.poll() is not None or generation != self._generation:
                raise _Aborted()
            sender = None
            if kind == 'cast':
                sender = _CastSender(host, port)
                sender.connect()
                sender.launch()
                sender.load(url, content_type=session.content_type, live=True)
        except _Aborted:
            self._teardown()
            return
        except Exception as e:
            self._teardown()
            detail = str(e).strip() or ' '.join(list(tail)[-3:])
            target_label = name if kind == 'cast' else '浏览器'
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
        if kind == 'browser':
            message = '镜像已开始，浏览器打开：{}'.format(page_url(server))
        else:
            message = '已开始镜像到 {}'.format(name)
        cherrypy.engine.publish('app_notify', 'Macast', message)
        logger.info('mirroring screen (%s) to %s via %s', kind, name or 'LAN',
                    url)

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
            awake = self._awake
            self._sender = self._proc = self._server = None
            self._awake = None
            self._mirroring = False
        _cleanup(sender, proc, server)
        _stop_awake(awake)


def _cleanup(sender, proc, server):
    if sender is not None:
        sender.stop()
        _close_quietly(sender)
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
    if server is not None:
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
        if kind == 'cast' and not _devices:
            start_search()
        renderer = self._renderer()
        mirroring = bool(renderer and renderer.is_mirroring())

        items = [
            MenuItem('Screen Mirror v0.4', enabled=False),
            MenuItem('停止镜像' if mirroring else '开始镜像',
                     self.on_toggle_clicked),
            MenuItem('输出目标', children=self._output_children(kind)),
        ]
        if kind == 'browser' and mirroring:
            items.append(MenuItem('复制观看地址', self.on_copy_url_clicked))
            items.append(MenuItem(renderer.viewer_url(), enabled=False))
        items.append(MenuItem('画质', children=self._quality_children()))
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
                    for key in ('cast', 'browser')]
        if kind == 'cast':
            children.append(MenuItem('— — —', enabled=False))
            children.extend(self._device_children())
        return children

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
            children.append(MenuItem('搜索中…再展开一次菜单' if _searching
                                     else '没有发现 Chromecast', enabled=False))
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
        minutes, seconds = divmod(stats['seconds'], 60)
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

    def on_refresh(self, item):
        start_search()
        cherrypy.engine.publish('app_notify', 'Macast', '正在搜索 Chromecast…',
                                sound=False)


if __name__ == '__main__':
    gui(ScreenMirrorRenderer())
