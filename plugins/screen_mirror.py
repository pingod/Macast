# Screen Mirror for Macast
#
# Macast Metadata
# <macast.title>Screen Mirror</macast.title>
# <macast.renderer>ScreenMirrorRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.3</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Mirror this Mac/PC/desktop screen to a Chromecast on the LAN: ffmpeg captures (avfoundation / gdigrab / x11grab), hard-encodes to H.264, a live MPEG-TS stream is served from this machine and LOADed on the TV. System audio rides along where a tap exists: macOS gets a one-click assisted install (official BlackHole pkg, sha256-verified, plus an auto-created multi-output device), Linux uses the PulseAudio monitor; Windows is video only.</macast.desc>
#
# Why: Macast is a receiver -- everything it plays was pushed to it. This
# plugin turns it around for one case: cast what is on this Mac's display,
# the way JustStream does, without leaving the menu bar.
#
# Implementation notes, because this is a *live* sender (see also
# cast_bridge.py, which forwards a finished URL):
#   * the pipeline is  ffmpeg screen capture -> libx264 zerolatency ->
#     mpegts on stdout -> a tiny HTTP server that broadcasts every chunk to
#     every connected client. The TV pulls http://<this machine>:<port>/screen.ts
#     and simply keeps reading;
#   * capture is platform dispatched: avfoundation (macOS), gdigrab
#     (Windows), x11grab (Linux/X11). System audio rides along where a tap
#     exists: macOS needs a BlackHole device (no released FFmpeg can see
#     system audio natively -- the screencapturekit demuxer never shipped),
#     Linux uses the PulseAudio `<sink>.monitor`. Windows stays video-only.
#     The probe is cached because the menu must never spawn ffmpeg;
#   * slow consumers drop whole chunks rather than blocking the reader --
#     for a live stream a stale frame is worse than a missing one, and a
#     blocked stdout pipe would stall the encoder;
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
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time
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
STREAM_PATH = "/screen.ts"
CHUNK = 4096
#: How long ffmpeg may survive before its death is blamed on the
#: Screen Recording permission instead of a genuine mid-stream failure.
EARLY_DEATH_SECONDS = 5.0

#: height -> (label, video bitrate)
QUALITIES = {'720': (720, 5000000), '1080': (1080, 10000000)}
FPS = 24

_devices = []
_searching = False
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

    def __init__(self, label, inputs, audio_map=None):
        self.label = label        # for the menu / logs
        self.inputs = inputs      # one list of input args per ffmpeg -i
        self.audio_map = audio_map  # '0:a:0' / '1:a:0' / None


#: probe results are cached per (ffmpeg, platform): probing spawns ffmpeg, and
#: the menu must never block on it.
_capture_cache = {}


def probe_capture(ffmpeg, platform=None):
    """Figure out the capture pipeline for this platform; None if impossible.

    `platform` is a test seam; production always means sys.platform.
    """
    platform = platform or sys.platform
    key = (ffmpeg, platform)
    if key in _capture_cache:
        return _capture_cache[key]
    if platform == 'win32':
        capture = _Capture(
            'Desktop (GDI)',
            [['-f', 'gdigrab', '-framerate', str(FPS),
              '-draw_mouse', '1', '-i', 'desktop']])
    elif platform == 'darwin':
        capture = _probe_avfoundation(ffmpeg)
    else:
        capture = _probe_linux(ffmpeg)
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


def _probe_avfoundation(ffmpeg):
    videos, audios = _avfoundation_lists(ffmpeg)
    screen = None
    for index, name in enumerate(videos):
        if 'capture screen' in name.lower():
            screen = index
            break
    if screen is None and videos:
        screen = 0
    if screen is None:
        return None
    # macOS exposes no system-audio sink to avfoundation (FFmpeg's proposed
    # screencapturekit demuxer was never released); BlackHole is the open
    # source way to make one appear. Absent it, we mirror video only.
    blackhole = None
    for index, name in enumerate(audios):
        if 'blackhole' in name.lower():
            blackhole = index
            break
    base = ['-f', 'avfoundation', '-framerate', str(FPS),
            '-capture_cursor', '1']
    if blackhole is None:
        return _Capture('屏幕 (avfoundation)',
                        [base + ['-i', '{}:none'.format(screen)]])
    return _Capture('屏幕 + 系统声音 (BlackHole)',
                    [base + ['-i', '{}:{}'.format(screen, blackhole)]],
                    audio_map='0:a:0')


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


def _probe_linux(ffmpeg):
    display = os.environ.get('DISPLAY')
    if not display:
        # x11grab cannot see a Wayland session; there is no ffmpeg-native
        # Wayland capture to fall back to.
        return None
    inputs = [['-f', 'x11grab', '-framerate', str(FPS),
               '-draw_mouse', '1', '-i', '{}+0,0'.format(display)]]
    monitor = _default_pulse_monitor()
    if monitor:
        inputs.append(['-f', 'pulse', '-i', monitor])
        return _Capture('屏幕 (X11) + 系统声音 (PulseAudio)',
                        inputs, audio_map='1:a:0')
    return _Capture('屏幕 (X11)', inputs)


def build_ffmpeg_command(ffmpeg, capture, height, bitrate):
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map:
        cmd += ['-map', capture.audio_map,
                '-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2']
    else:
        cmd += ['-an']
    cmd += ['-vf', 'scale=-2:{}'.format(height),
            '-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
            '-profile:v', 'high', '-pix_fmt', 'yuv420p',
            '-g', str(FPS * 2), '-b:v', str(bitrate),
            '-f', 'mpegts', 'pipe:1']
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


# -- live MPEG-TS HTTP server -----------------------------------------------

class _Broadcaster(object):
    """Fan out encoder output to every connected client; drop for the slow."""

    def __init__(self, maxsize=256):
        self._maxsize = maxsize
        self._subs = set()
        self._lock = threading.Lock()

    def subscribe(self):
        q = Queue(maxsize=self._maxsize)
        with self._lock:
            self._subs.add(q)
        return q

    def unsubscribe(self, q):
        with self._lock:
            self._subs.discard(q)

    def feed(self, chunk):
        with self._lock:
            subs = list(self._subs)
        for q in subs:
            try:
                q.put_nowait(chunk)
            except Exception:
                try:
                    q.get_nowait()      # live beats lossless: drop the oldest
                    q.put_nowait(chunk)
                except Exception:
                    pass

    def clients(self):
        with self._lock:
            return len(self._subs)


class _StreamHandler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, fmt, *args):
        logger.debug("stream http: " + fmt % args)

    def do_GET(self):
        broadcaster = self.server.broadcaster
        queue = broadcaster.subscribe()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'video/mp2t')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
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


def start_stream_server():
    server = ThreadingHTTPServer(('0.0.0.0', 0), _StreamHandler)
    server.broadcaster = _Broadcaster()
    server.daemon_threads = True
    # A client that vanished mid-stream stays in queue.get for seconds;
    # server_close must not wait for it.
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name="SCREEN_MIRROR_HTTP").start()
    return server


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
        host, port, name = self.target()
        if host is None:
            self._fail('还没有选择投屏目标：在菜单栏的「Target」里选一台 Chromecast',
                       generation)
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
        first_bytes = threading.Event()
        try:
            server = start_stream_server()
            proc = subprocess.Popen(
                build_ffmpeg_command(ffmpeg, capture, height, bitrate),
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
            url = 'http://{}:{}{}'.format(advertise_host(),
                                          server.server_address[1], STREAM_PATH)
            # Wait for the encoder to actually produce something: a Screen
            # Recording denial exits in under a second, and the pump is
            # reporting that while we wait.
            first_bytes.wait(timeout=3.0)
            if proc.poll() is not None or generation != self._generation:
                raise _Aborted()
            sender = _CastSender(host, port)
            sender.connect()
            sender.launch()
            sender.load(url, content_type='video/mp2t', live=True)
        except _Aborted:
            self._teardown()
            return
        except Exception as e:
            self._teardown()
            self._fail('镜像到 {}（{}:{}）启动失败：{}'.format(name, host, port, e),
                       generation)
            return
        with self._lock:
            if generation != self._generation:
                self._teardown()
                return
            self._sender = sender
            self._mirroring = True
            self._started_at = time.time()
            self._url = url
        self.set_state_transport('PLAYING')
        cherrypy.engine.publish('app_notify', 'Macast',
                                '已开始镜像到 {}'.format(name))
        logger.info('mirroring screen to %s via %s', name, url)

    def _cast_url(self, url, generation):
        """Bridge path: play a finished URL on the device, no capture."""
        host, port, name = self.target()
        if host is None:
            self._fail('还没有选择投屏目标：在菜单栏的「Target」里选一台 Chromecast',
                       generation)
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
            self._sender = self._proc = self._server = None
            self._mirroring = False
        _cleanup(sender, proc, server)


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


class ScreenMirrorSetting(RendererSetting):

    def _renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        renderer = renderers.pop() if renderers else None
        return renderer if isinstance(renderer, ScreenMirrorRenderer) else None

    def build_menu(self):
        if not _devices:
            start_search()
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

        renderer = self._renderer()
        mirroring = bool(renderer and renderer.is_mirroring())
        quality = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
        quality_children = [
            MenuItem('720p（默认）', self.on_quality_clicked,
                     checked=(quality == '720'), data='720'),
            MenuItem('1080p（更吃带宽）', self.on_quality_clicked,
                     checked=(quality == '1080'), data='1080'),
        ]
        items = [
            MenuItem('Screen Mirror v0.3', enabled=False),
            MenuItem('停止镜像' if mirroring else '开始镜像',
                     self.on_toggle_clicked),
            MenuItem('Target', children=children),
            MenuItem('画质', children=quality_children),
        ]
        if sys.platform == 'darwin':
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
        return items

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
