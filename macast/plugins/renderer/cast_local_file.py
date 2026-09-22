# Copyright (c) 2026 by pingod. All Rights Reserved.
# Local File Caster for Macast
#
# Macast Metadata
# <macast.title>Local File Caster</macast.title>
# <macast.renderer>LocalFileRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Cast what is already on this machine's disk to a Chromecast / Google TV or to a DLNA television: choose a folder in the menu bar, click a file, and it plays on the TV. Files the device decodes natively are streamed byte-for-byte by a built-in Range/206 HTTP server, so the remote's own pause and seek act on the real file. Anything else (AVI, MKV, HEVC, DTS, AC-3, a second audio track, an embedded subtitle) is transcoded to MPEG-TS by ffmpeg while it plays, and a seek restarts the encoder at the new position. Also: a playlist that advances when the device reports the last item finished, audio track choice plus an audio/video sync offset on the transcoding path, embedded subtitles re-served as WebVTT for Chromecast (or burnt in when this ffmpeg has libass), a watchdog that takes the television back when another sender squats it, a real QUIT_APP so the set returns to its input instead of a frozen frame, direct casting of an ordinary URL without touching any file, and an audio-only mode that streams this machine's system sound where a capture tap exists.</macast.desc>
#
# Why: Macast receives a URL and plays it here. The other half of the
# JustStream feature set is "the file is on this Mac, the screen I want is in
# the living room" -- a sender that serves what it has and lets the television
# decode it.
#
# The two decisions that shape the whole file:
#
#   * Native or converted, decided per file by ffprobe rather than per format
#     by guess. A file the target can decode is served verbatim: byte ranges,
#     real length, and the device's own UI does pause/seek/volume with no work
#     from us. Everything else goes through ffmpeg, which is why the
#     conversion path can be simple -- it only ever handles what the copy path
#     cannot.
#   * The conversion path writes a *growing file* and serves that, instead of
#     piping. A pipe forces live semantics (no length, no reconnect, no seek),
#     and a DLNA renderer will not play a stream it cannot get a length for at
#     all. So ffmpeg appends to a temporary MPEG-TS while the HTTP server
#     hands out byte ranges of it; a reader that outruns the encoder waits for
#     it rather than getting a short answer. Seeking restarts ffmpeg at the new
#     time, which is honest about what a transcode in progress can do.
#     The advertised length is duration x bitrate, capped below 2 GiB because
#     old firmware does signed 32-bit arithmetic there (the same reason
#     screen_mirror caps its fake file) -- and when that cap is what limits
#     the size, the bitrate comes down to fit rather than the length lying.
#
# Other notes worth knowing before editing:
#   * Track selection and the A/V offset exist only on the conversion path:
#     copying hands the device the original bytes, and its track list cannot
#     be reordered from here. The decision function routes any file where the
#     user picked a non-default audio track to ffmpeg for exactly that reason,
#     and the menu says so in those words.
#   * Subtitles split three ways because the targets genuinely differ: for
#     Chromecast an embedded track is converted to WebVTT and declared in the
#     LOAD's `textTracks` (the receiver draws it); on the conversion path it is
#     burnt in when this ffmpeg was built with libass; a DLNA renderer gets
#     neither.
#   * Discovery is mDNS for Chromecasts and SSDP for MediaRenderers, both in
#     the background: `build_menu` runs on the UI thread and must never block
#     on a three-second multicast wait. Same rule for every capability probe.
#   * Cast framing comes from `macast.protocol_cast` (we are the sender, the
#     library implements the receiver, the wire format is one table). No
#     pychromecast: a single-file plugin cannot install a pip package.
#   * The watchdog is the `--hijack` idea without the name. Every few seconds
#     it asks the device what it is doing; if our app or URI is gone it
#     re-pushes from the last reported position, with bounded retries -- a
#     phone that wants the television should get the television. When the
#     device reports the item *ended*, the playlist advances.
#   * Stopping sends QUIT_APP, not just STOP. A bare STOP leaves the receiver
#     app parked on the last frame, which is how "I stopped casting but the TV
#     is still showing my desktop" reaches us as a bug report.

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
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import deque
from enum import Enum
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import cherrypy

from macast import Setting, MenuItem, gui
from macast.renderer import Renderer, RendererSetting
from macast.protocol_cast import (encode_cast_message, parse_cast_message,
                                 DEFAULT_MEDIA_APP_ID, NS_CONNECTION,
                                 NS_MEDIA, NS_RECEIVER)

logger = logging.getLogger("LocalFileCaster")
logger.setLevel(logging.INFO)

CAST_PORT = 8009
SERVICE_TYPE = "_googlecast._tcp.local."
NS_DEVICE_AUTH = "urn:x-cast:com.google.cast.tp.deviceauth"
#: `DeviceAuthMessage{challenge: Challenge{}}` -- the empty opener every sender
#: sends. The answer is not verified: we are an uncertified sender.
DEVICE_AUTH_CHALLENGE = b"\x0a\x00"
SSDP_ADDR = "239.255.255.250"
SSDP_PORT = 1900
DLNA_SERVICE = "urn:schemas-upnp-org:service:AVTransport:1"
DLNA_SEARCH_TARGETS = ("urn:schemas-upnp-org:device:MediaRenderer:1",
                       DLNA_SERVICE)

#: Everything served lives under one prefix and the file name is random: no
#: directory listing, nothing to walk.
MEDIA_PREFIX = "/media/"
CHUNK = 64 * 1024
#: Renderers that keep this field in a signed 32-bit int answer "file not
#: supported" to anything larger.
MAX_ADVERTISED_SIZE = 1900000000
#: A transcode with no duration to divide by is budgeted against this.
FALLBACK_DURATION = 3600.0
DEFAULT_BITRATE = 6000000
DEFAULT_AUDIO_BITRATE = 192000
#: Chromecasts resample high-rate audio audibly badly, and only wav/flac are
#: true HD on them, so the encoder is pinned to 48 kHz.
AUDIO_SAMPLE_RATE = 48000
WATCHDOG_SECONDS = 5.0
#: Retries against a device that keeps getting taken over; past this the user
#: hears about it instead of watching us fight for the television forever.
MAX_REPUSH = 3
#: How long an HTTP reader waits for the encoder to catch up before it gets a
#: short answer. A forward seek outruns a transcode for a moment; this is that
#: moment.
TAIL_WAIT_SECONDS = 45.0
MENU_FILES = 40
MENU_TRACKS = 12

MEDIA_EXTENSIONS = ('.mp4', '.m4v', '.mov', '.mkv', '.webm', '.avi', '.wmv',
                    '.flv', '.mpg', '.mpeg', '.ts', '.m2ts', '.vob', '.mxf',
                    '.mp3', '.m4a', '.aac', '.wav', '.flac', '.ogg', '.opus',
                    '.ac3', '.alac')

#: container -> the value senders put in `contentType` / DIDL `protocolInfo`.
CONTAINER_CONTENT = {
    'mp4': 'video/mp4', 'mov': 'video/quicktime', 'm4v': 'video/mp4',
    'matroska': 'video/x-matroska', 'webm': 'video/webm',
    'mpegts': 'video/mp2t', 'mp3': 'audio/mpeg', 'm4a': 'audio/mp4',
    'wav': 'audio/wav', 'flac': 'audio/flac', 'ogg': 'audio/ogg',
    'asf': 'video/x-ms-asf', 'avi': 'video/x-msvideo', 'flv': 'video/x-flv',
}

#: What each target can be trusted to decode unaided. Deliberately
#: conservative: a wrong "yes" is a black screen blamed on us, a wrong "no" is
#: a few CPU seconds.
COPY_CONTAINERS = {'cast': ('mp4', 'mov', 'm4v', 'matroska', 'webm'),
                   'dlna': ('mp4', 'mov', 'm4v')}
COPY_VIDEO_CODECS = ('h264',)
COPY_AUDIO_CODECS = {'cast': ('aac', 'mp3', 'flac', 'vorbis', 'opus'),
                     'dlna': ('aac', 'mp3')}
#: A renderer's plain-music path is far more tolerant than its video path, so
#: audio-only files get their own list.
COPY_AUDIO_ONLY = {'cast': ('mp3', 'aac', 'flac', 'wav', 'vorbis', 'opus'),
                   'dlna': ('mp3', 'aac', 'flac', 'wav')}

TARGETS = {'cast': 'Chromecast / Google TV', 'dlna': 'DLNA 电视'}


class SettingProperty(Enum):
    #: 'cast' or 'dlna'.
    Target_Kind = 1
    #: "host:port" of the chosen Chromecast.
    Cast_Target = 2
    Cast_Target_Name = 3
    #: AVTransport control URL of the chosen renderer. Its own key because
    #: running a URL through the `host:port` splitter makes "http" the host.
    Dlna_Control = 4
    Dlna_Target_Name = 5
    Folder = 6
    #: 'auto' | 'direct' | 'convert'.
    Mode = 7
    #: ffprobe stream index of the audio track to keep; '' = as-is.
    Audio_Stream = 8
    #: ffprobe stream index of the subtitle track; '' = none.
    Subtitle_Stream = 9
    #: Audio shift in milliseconds, positive = audio later.
    Audio_Delay = 10
    Bitrate = 11
    Hardware = 12
    #: Audio-only mode.
    System_Audio = 13
    Auto_Next = 14
    #: Socket timeout in seconds.
    Timeout = 15
    #: Where the conversion path keeps its growing file.
    Temp_Dir = 16


# -- external programs -------------------------------------------------------

def find_tool(name):
    """Path to ffmpeg/ffprobe, or None.

    PATH first, then the directories a Finder-launched menu-bar app never
    sees; a GUI launch does not inherit the shell's PATH.
    """
    found = shutil.which(name)
    if found:
        return found
    suffix = '.exe' if sys.platform == 'win32' else ''
    for base in ("/opt/homebrew/opt/ffmpeg/bin", "/opt/homebrew/bin",
                 "/usr/local/opt/ffmpeg/bin", "/usr/local/bin",
                 "/opt/local/bin"):
        candidate = os.path.join(base, name + suffix)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def _clean_env():
    """A proxy in the environment would intercept the television."""
    env = dict(os.environ)
    for key in ('http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY',
                'all_proxy', 'ALL_PROXY', 'no_proxy', 'NO_PROXY'):
        env.pop(key, None)
    return env


#: Capability probes spawn ffmpeg, so they are cached; `build_menu` must never
#: trigger one.
_tool_cache = {}


def invalidate_tool_cache():
    _tool_cache.clear()


def _ask(tool, args, timeout=20):
    """Run a tool, hand back stdout; '' when it will not answer."""
    try:
        done = subprocess.run([tool] + list(args), stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT,
                              stdin=subprocess.DEVNULL, env=_clean_env(),
                              timeout=timeout)
    except Exception as e:
        logger.info('%s %s failed: %s', tool, ' '.join(args), e)
        return ''
    return (done.stdout or b'').decode('utf-8', 'replace')


def ffmpeg_has(tool, name):
    """True when this build lists `name` among its encoders or filters.

    VideoToolbox and libass are both optional at the far end -- asking is the
    only way to know, and asking once is the only way the menu stays fast.
    """
    if not tool:
        return False
    key = ('feature', tool, name)
    if key not in _tool_cache:
        listing = (_ask(tool, ['-hide_banner', '-encoders']) +
                   _ask(tool, ['-hide_banner', '-filters']))
        _tool_cache[key] = bool(re.search(r'\b%s\b' % re.escape(name),
                                          listing))
    return _tool_cache[key]


# -- what a file contains ----------------------------------------------------

class Track(object):
    """One stream as ffprobe described it, plus the label the menu shows."""

    def __init__(self, index, kind, codec='', language='', title='',
                 default=False):
        self.index = index
        self.kind = kind                  # 'v' | 'a' | 's'
        self.codec = codec or ''
        self.language = language or ''
        self.title = title or ''
        self.default = bool(default)

    def label(self):
        parts = ['#{} {}'.format(self.index, self.codec or '?')]
        if self.language:
            parts.append(self.language)
        if self.title:
            parts.append(self.title)
        if self.default:
            parts.append('默认')
        return ' · '.join(parts)


class Media(object):
    """The part of an ffprobe report this plugin acts on."""

    def __init__(self, ok=False, container='', duration=0.0, size=0,
                 bitrate=0, tracks=()):
        self.ok = ok
        self.container = container
        self.duration = duration
        self.size = size
        self.bitrate = bitrate
        self.tracks = list(tracks)

    @property
    def video(self):
        return [t for t in self.tracks if t.kind == 'v']

    @property
    def audio(self):
        return [t for t in self.tracks if t.kind == 'a']

    @property
    def subtitles(self):
        return [t for t in self.tracks if t.kind == 's']

    @property
    def audio_only(self):
        return self.ok and not self.video and bool(self.audio)

    @property
    def upnp_class(self):
        return ('object.item.audioItem.musicTrack' if self.audio_only
                else 'object.item.videoItem')

    def track(self, index):
        for item in self.tracks:
            if item.index == index:
                return item
        return None

    def content_type(self):
        return CONTAINER_CONTENT.get(self.container, 'video/mp4')

    def stream_position(self, index):
        """ffprobe's absolute index -> the `0:a:N` relative selector."""
        return self.position_in(self.audio, index)

    def subtitle_position(self, index):
        return self.position_in(self.subtitles, index)

    @staticmethod
    def position_in(tracks, index):
        """`0:s:2` counts subtitle streams, not ffprobe's whole list."""
        for position, track in enumerate(tracks):
            if track.index == index:
                return position
        return 0


def _seconds(value):
    try:
        return max(0.0, float(value))
    except (TypeError, ValueError):
        return 0.0


def parse_probe(raw):
    """ffprobe's JSON -> Media.

    Anything unreadable is `ok = False` rather than a half-filled object: the
    point of probing is to decide whether the container can be trusted, and a
    report without its format section is not a report.
    """
    try:
        data = json.loads(raw)
    except (TypeError, ValueError):
        return Media()
    if not isinstance(data, dict):
        return Media()
    fmt = data.get('format') or {}
    tracks = []
    for stream in data.get('streams') or []:
        if not isinstance(stream, dict):
            continue
        kind = {'video': 'v', 'audio': 'a', 'subtitle': 's'}.get(
            stream.get('codec_type'))
        if kind is None:
            continue
        try:
            index = int(stream.get('index'))
        except (TypeError, ValueError):
            continue
        tags = stream.get('tags') or {}
        disposition = stream.get('disposition') or {}
        tracks.append(Track(index, kind, stream.get('codec_name'),
                            (tags.get('language') or '').strip(),
                            (tags.get('title') or
                             tags.get('handler_name') or '').strip(),
                            str(disposition.get('default', 0)) == '1'))
    duration = _seconds(fmt.get('duration'))
    size = int(_seconds(fmt.get('size')))
    try:
        bitrate = int(_seconds(fmt.get('bit_rate')))
    except (TypeError, ValueError):
        bitrate = 0
    if not bitrate and duration and size:
        bitrate = int(size * 8 / duration)
    return Media(ok=bool(tracks), container=_container(fmt.get('format_name')),
                 duration=duration, size=size, bitrate=bitrate, tracks=tracks)


def _container(names):
    """`"mov,mp4,m4a,3gp,3g2,mj2"` -> 'mp4'.

    ffprobe reports MP4 under the mov name first, which would sort every MP4
    into the wrong bucket; the extension decides between the two aliases.
    """
    first = (names or '').split(',')[0].strip()
    if first == 'mov':
        return 'mp4'
    return first


def probe_media(path, timeout=25):
    ffprobe = find_tool('ffprobe')
    if not ffprobe:
        logger.info('no ffprobe: every file will be treated as convertible')
        return Media()
    return parse_probe(_ask(ffprobe, [
        '-v', 'quiet', '-print_format', 'json', '-show_format',
        '-show_streams', '-analyzeduration', '20000000', '-probesize',
        '20000000', path], timeout=timeout))


# -- the native-or-converted decision ---------------------------------------

def plan(media, target, audio_index=None, subtitle_index=None, delay_ms=0,
         mode='auto', libass=False):
    """('copy' | 'convert', reason). The reason reaches the user verbatim.

    Everything we cannot prove the device will decode converts, and so does
    anything the user asked us to *change* about the file -- the copy path
    hands over the original bytes and cannot edit a track list.
    """
    if mode == 'convert':
        return 'convert', '手动选择强制转码'
    if mode == 'direct':
        return 'copy', '手动选择强制直通'
    if not media or not media.ok:
        return 'convert', '读不出文件信息，按转码处理'
    reasons = []
    if media.audio_only:
        if media.audio[0].codec not in COPY_AUDIO_ONLY.get(target, ()):
            reasons.append('音乐编码 {}'.format(media.audio[0].codec))
    else:
        if not media.video:
            return 'convert', '文件里没有视频轨'
        if media.container not in COPY_CONTAINERS.get(target, ()):
            reasons.append('容器 {}'.format(media.container))
        if media.video[0].codec not in COPY_VIDEO_CODECS:
            reasons.append('画面编码 {}'.format(media.video[0].codec))
        codecs = {t.codec for t in media.audio}
        allowed = set(COPY_AUDIO_CODECS.get(target, ()))
        if codecs and not codecs <= allowed:
            reasons.append('声音编码 {}'.format(
                '/'.join(sorted(c or '?' for c in codecs))))
    if audio_index is not None and len(media.audio) > 1:
        reasons.append('选音轨要重新混流')
    if subtitle_index is not None and target != 'cast' and not libass:
        reasons.append('这个 ffmpeg 没有字幕烧录能力')
    if delay_ms:
        reasons.append('音画同步偏移要重新混流')
    if reasons:
        return 'convert', '、'.join(reasons) + '，电视吃不下'
    return 'copy', '电视能直接解码'


def bitrate_for_convert(media, wanted, limit=MAX_ADVERTISED_SIZE):
    """(bitrate, advertised_size) for the transcode's pretend file.

    The advertised length is what the device draws on its progress bar, so it
    has to be the length the encoder will actually produce. When that would
    cross 2 GiB, the bitrate comes down instead of the length lying.
    """
    duration = media.duration if media and media.duration else FALLBACK_DURATION
    audio = DEFAULT_AUDIO_BITRATE
    bitrate = max(600000, int(wanted or DEFAULT_BITRATE))
    ceiling = int(limit * 8 / duration * 0.9) - audio
    if ceiling < bitrate:
        bitrate = max(600000, ceiling)
    return bitrate, int((bitrate + audio) * duration / 8.0)


# -- the conversion job: ffmpeg writing one growing file ---------------------

class Job(object):
    """An ffmpeg process appending to a file that we serve while it grows.

    The renderer owns the job, never the thread that started it, so stopping is
    one call and a job cannot outlive the session that made it. The temporary
    directory leaves with it.
    """

    def __init__(self, ffmpeg, args, suffix, content_type, total=0,
                 live=False, label=''):
        self.ffmpeg = ffmpeg
        self.args = list(args)
        self.suffix = suffix
        self.content_type = content_type
        #: Advertised length; 0 for the live flavour, which never claims one.
        self.total = int(total)
        self.live = live
        self.label = label
        self.directory = None
        self.path = None
        self.proc = None
        self.stderr = deque(maxlen=40)
        self._drain = None

    def start(self):
        self.directory = tempfile.mkdtemp(prefix='macast-cast-',
                                          dir=temp_root())
        self.path = os.path.join(self.directory, 'stream.%s' % self.suffix)
        argv = [self.ffmpeg, '-hide_banner', '-loglevel', 'error', '-y']
        argv += self.args + [self.path]
        try:
            self.proc = subprocess.Popen(
                argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE, env=_clean_env())
        except OSError as e:
            logger.error('cannot start ffmpeg: %s', e)
            self.cleanup()
            return False
        # An unread stderr pipe fills up and ffmpeg blocks inside the encoder,
        # which from the sofa looks exactly like a stalled transcode.
        self._drain = threading.Thread(target=_drain_stderr,
                                       args=(self.proc, self.stderr),
                                       daemon=True, name='CAST_FILE_STDERR')
        self._drain.start()
        return True

    def size_now(self):
        try:
            return os.path.getsize(self.path)
        except (OSError, TypeError):
            return 0

    def alive(self):
        return bool(self.proc) and self.proc.poll() is None

    def error(self):
        for line in reversed(self.stderr):
            if line:
                return line
        return ''

    def wait_for(self, offset):
        """True once the file holds byte `offset`; False if it never will."""
        limit = time.time() + TAIL_WAIT_SECONDS
        while time.time() < limit:
            if self.size_now() > offset:
                return True
            if not self.alive():
                return False
            time.sleep(0.05)
        return self.size_now() > offset

    def read(self, offset, length):
        if offset < 0:
            offset = 0
        if self.size_now() <= offset and not self.wait_for(offset):
            return b''
        try:
            with open(self.path, 'rb') as handle:
                handle.seek(offset)
                return handle.read(length)
        except OSError:
            return b''

    def stop(self):
        proc, self.proc = self.proc, None
        if proc is None or proc.poll() is not None:
            return
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass

    def cleanup(self):
        if self.directory:
            shutil.rmtree(self.directory, ignore_errors=True)
            self.directory = None


def temp_root():
    root = os.path.expanduser(str(Setting.get(SettingProperty.Temp_Dir, '')
                                  or ''))
    if root and os.path.isdir(root) and os.access(root, os.W_OK):
        return root
    return tempfile.gettempdir()


def free_space(path):
    try:
        return shutil.disk_usage(path).free
    except OSError:
        return 1 << 62


def _drain_stderr(proc, tail):
    """Move ffmpeg's stderr into a bounded buffer, forever."""
    try:
        for line in iter(proc.stderr.readline, b''):
            tail.append(line.decode('utf-8', 'replace').strip())
    except Exception:
        pass
    finally:
        try:
            proc.stderr.close()
        except Exception:
            pass


def escape_filter_path(path):
    """A subtitle path inside an ffmpeg filter argument.

    `subtitles=` is parsed twice: ffmpeg's filter graph treats ':' as an
    argument separator and libass has its own quoting, so a path with a colon
    or an apostrophe has to survive both.
    """
    text = path.replace('\\', '/').replace("'", "\\'")
    if ':' in text:
        text = text.replace(':', '\\:')
    return text


def convert_args(path, media, options):
    """ffmpeg arguments for the conversion path, output path excluded."""
    offset = float(options.get('delay_ms') or 0) / 1000.0
    video = media.video[0].index if media.video else None
    wanted = options.get('audio_index')
    audio = (wanted if wanted is not None and media.track(wanted)
             else (media.audio[0].index if media.audio else None))
    args = ['-nostdin', '-i', path]
    # A second input of the same file is how the A/V offset lands: an
    # -itsoffset in front of that input moves only what is mapped out of it.
    audio_source = '0'
    if offset:
        args += ['-itsoffset', '%.3f' % offset, '-i', path]
        audio_source = '1'
    filters = []
    if options.get('height'):
        filters.append('scale=-2:{}'.format(int(options['height'])))
    burn = options.get('subtitle_path') if options.get('burn_subtitle') else None
    if burn:
        filters.append('subtitles={}'.format(escape_filter_path(burn)))
    if video is None:
        args += ['-vn']
    else:
        args += ['-map', '0:v:0']
    if audio is not None:
        args += ['-map', '{}:a:{}'.format(
            audio_source, media.stream_position(audio))]
    if filters:
        args += ['-vf', ','.join(filters)]
    if video is not None:
        bitrate = int(options.get('bitrate') or DEFAULT_BITRATE)
        hardware = (options.get('encoder') == 'hardware' and
                    options.get('hardware_name'))
        if hardware:
            args += ['-c:v', options['hardware_name']]
        else:
            args += ['-c:v', 'libx264', '-preset', 'veryfast',
                     '-pix_fmt', 'yuv420p', '-profile:v', 'high',
                     '-level', '4.1']
        # A one-second buffer clips peaks the source has; the file is read
        # locally, so the rate control can breathe.
        args += ['-b:v', str(bitrate), '-maxrate', str(bitrate),
                 '-bufsize', str(bitrate * 2), '-g', '60']
    if audio is not None:
        args += ['-c:a', 'aac',
                 '-b:a', str(int(options.get('audio_bitrate') or
                                 DEFAULT_AUDIO_BITRATE)),
                 '-ar', str(AUDIO_SAMPLE_RATE), '-ac', '2']
    return args + ['-f', 'mpegts']


# -- the HTTP face of all this ----------------------------------------------

class Entry(object):
    """One registered thing to serve: a real file, or a job's growing file."""

    def __init__(self, name, suffix, path, content_type, job=None, title='',
                 dlna=False, temp=False):
        self.name = name
        self.suffix = suffix
        self.path = path
        self.content_type = content_type
        self.job = job
        self.title = title
        #: Renderers read `transferMode.dlna.org` before committing to a
        #: stream; only the ones pushed at a television need it.
        self.dlna = dlna
        #: We created this file (a subtitle sidecar), so we delete it. The copy
        #: path serves the user's own file and must never remove it.
        self.temp = temp

    @property
    def filename(self):
        return '{}.{}'.format(self.name, self.suffix)

    def size(self):
        """The length we advertise, which for a job is the estimate."""
        if self.job is not None:
            return self.job.total
        try:
            return os.path.getsize(self.path)
        except OSError:
            return 0

    def read(self, offset, length):
        if self.job is not None:
            return self.job.read(offset, length)
        try:
            with open(self.path, 'rb') as handle:
                handle.seek(offset)
                return handle.read(length)
        except OSError:
            return b''


class Store(object):
    """The names this server answers for. An unknown name is always 404."""

    def __init__(self):
        self.entries = {}

    def add(self, path, content_type, suffix, job=None, title='',
            dlna=False, temp=False):
        entry = Entry(secrets.token_hex(8), suffix, path, content_type,
                      job=job, title=title, dlna=dlna, temp=temp)
        self.entries[entry.filename] = entry
        return entry

    def lookup(self, filename):
        return self.entries.get(filename)

    def drop(self, entry):
        self.entries.pop(entry.filename, None)

    def clear(self):
        self.entries.clear()


def parse_range(header):
    """`bytes=100-200` -> (100, 200); `bytes=100-` -> (100, None).

    Junk returns None, and the caller then serves the whole file: RFC 7233 says
    an unsupported or malformed Range is to be ignored, not answered 416,
    because 416 is what a DLNA renderer reads as a dead file.
    `(None, n)` means the suffix form -- the last `n` bytes -- which only the
    real length can resolve, so the caller decides.
    """
    if not header:
        return 0, None
    unit, _, spec = header.partition('=')
    if unit.strip().lower() != 'bytes' or ',' in spec or not spec:
        return None
    first, _, last = spec.partition('-')
    try:
        if not first:
            return None, int(last) if last else None
        start = int(first)
    except ValueError:
        return None
    if not last:
        return start, None
    try:
        stop = int(last)
    except ValueError:
        return None
    if stop < start:
        return None
    return start, stop


class MediaHandler(BaseHTTPRequestHandler):
    """Range/206 serving for exactly the names `Store` knows.

    Two shapes, because the two paths need different things: a finished file is
    answered to the byte (that is what makes the remote's seek bar work), and a
    file still being encoded is answered up to its growing tail, waiting for
    the encoder when the reader gets ahead of it.
    """

    protocol_version = 'HTTP/1.0'

    def log_message(self, fmt, *args):
        logger.debug('caster http: ' + fmt % args)

    @property
    def store(self):
        return self.server.store

    def _entry(self):
        path = self.path.partition('?')[0]
        if not path.startswith(MEDIA_PREFIX):
            return None
        filename = path.rsplit('/', 1)[-1]
        return self.store.lookup(filename)

    def _refuse(self, code=404):
        self.send_response(code)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def do_HEAD(self):
        self.do_GET(head_only=True)

    def do_GET(self, head_only=False):
        entry = self._entry()
        if entry is None:
            return self._refuse()
        if entry.job is not None and entry.job.live:
            return self._follow(entry, head_only)
        header = self.headers.get('Range')
        # `parse_range(None)` answers "(0, None) -- from the start", which is a
        # whole-file answer, not a partial one: only a Range the client actually
        # sent earns a 206.
        spec = parse_range(header) if header else None
        start, stop = spec if spec else (0, None)
        total = entry.size()
        if total == 0 and entry.job is None:
            # Registered while the file was there; it has since moved or gone.
            # A 200 with no length would leave a renderer waiting for bytes that
            # never come.
            return self._refuse()
        if start is None:                  # "the last n bytes"
            start, stop = (max(0, total - stop), None) if (total and stop) \
                else (0, None)
        if total and start >= total:
            self.send_response(416)
            self.send_header('Content-Range', 'bytes */{}'.format(total))
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        end = total - 1 if (stop is None or stop >= total) else stop
        if end < start:
            end = start
        # `spec` rather than `header`: an unparsable Range is ignored, and a
        # whole-file answer to it must be a 200 with no Content-Range.
        self._headers(entry, 206 if spec else 200, start, end, total)
        if head_only:
            return
        self._pump(entry, start, end - start + 1)

    def _headers(self, entry, code, start, end, total):
        self.send_response(code)
        self.send_header('Content-Type', entry.content_type)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Accept-Ranges', 'bytes')
        if code == 206 and total:
            self.send_header('Content-Range',
                             'bytes {}-{}/{}'.format(start, end, total))
        if total:
            self.send_header('Content-Length',
                             str(end - start + 1) if code == 206
                             else str(total))
        if entry.dlna:
            self.send_header('transferMode.dlna.org', 'Streaming')
            self.send_header('contentFeatures.dlna.org',
                             'DLNA.ORG_OP=01;DLNA.ORG_CI=0;'
                             'DLNA.ORG_FLAGS=01700000000000000000000000000000')
        self.end_headers()

    def _pump(self, entry, offset, length):
        remaining = length
        try:
            while remaining > 0:
                want = min(CHUNK, remaining)
                # No deadline of our own: each read waits TAIL_WAIT_SECONDS for
                # the encoder, so a 90-minute transcode is not cut short at 45
                # s, while a dead ffmpeg still answers within one chunk.
                data = entry.read(offset, want)
                if not data:
                    break           # the encoder stopped, or the file ran out
                self.wfile.write(data)
                self.wfile.flush()
                offset += len(data)
                remaining -= len(data)
                if len(data) < want and entry.job is None:
                    break           # a real file shorter than it claimed
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass

    def _follow(self, entry, head_only):
        """The live flavour: no length, no ranges, follow the tail."""
        self.send_response(200)
        self.send_header('Content-Type', entry.content_type)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Accept-Ranges', 'none')
        if entry.dlna:
            self.send_header('transferMode.dlna.org', 'Streaming')
        self.end_headers()
        if head_only:
            return
        job = entry.job
        offset = 0
        try:
            while self.server.serving and job.alive():
                size = job.size_now()
                if size <= offset:
                    if not job.wait_for(max(0, size - 1)):
                        break
                    continue
                data = job.read(offset, min(CHUNK, size - offset))
                if not data:
                    break
                self.wfile.write(data)
                self.wfile.flush()
                offset += len(data)
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass


def start_media_server(store):
    server = ThreadingHTTPServer(('0.0.0.0', 0), MediaHandler)
    server.store = store
    server.serving = True
    server.daemon_threads = True
    # A client that vanished mid-stream sits in a read for seconds; closing
    # must not wait for it.
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name='CAST_FILE_HTTP').start()
    return server


def advertise_host():
    """The address a television on this LAN can reach us on.

    `Setting.get_advertisable_ip()` is permissive on purpose -- it keeps every
    interface with a gateway entry, the `AF_LINK` ones included -- so on a
    machine running VMs or Tailscale it lists the VM bridges and the tunnel
    alongside the Wi-Fi address, and its first element is whatever the set
    happens to yield. Measured on that machine: 192.168.215.0, 192.168.97.0 and
    192.168.139.3 on three runs, none of which a phone can dial, while the real
    LAN address is 192.168.1.5. mDNS hit the same problem and answers it in
    `discovery.advertisable_addresses()` by keeping the interface that carries
    the IPv4 default route. Asking the core which of these addresses it would
    publish is what keeps the URL we hand a browser openable instead of merely
    printable. A peer we already know about is a better answer still, which is
    what host_for() is for; this one covers the address we announce before
    anything has been dialled.
    """
    try:
        addrs = Setting.get_advertisable_ip()
    except Exception:
        addrs = []
    if not addrs:
        return '127.0.0.1'
    reachable = [a for a in addrs if a in set(_reachable_hosts())]
    return (reachable or addrs)[0]


def _reachable_hosts():
    """What discovery would publish, or [] when the core cannot say.

    A separate function so the choice above is testable without pretending this
    machine has a second interface.
    """
    try:
        from macast.discovery import advertisable_addresses
        return list(advertisable_addresses())
    except Exception:  # pragma: no cover - discovery ships with the app
        return []


def host_for(peer_ip):
    """Which of our own addresses reaches that one.

    A UDP connect() asks the routing table without sending anything. Serving
    from the wrong interface looks like a broken stream: the device fetches
    the URL, gets nothing routable back, and blames the file.
    """
    if not peer_ip:
        return advertise_host()
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


def entry_url(server, entry, peer_ip=None, suffix=None):
    name = '{}.{}'.format(entry.name, suffix or entry.suffix)
    return 'http://{}:{}{}{}'.format(host_for(peer_ip),
                                     server.server_address[1],
                                     MEDIA_PREFIX, name)


# -- discovery ---------------------------------------------------------------

_devices = []
_dlna_devices = []
_searching = False
_dlna_searching = False
_search_lock = threading.Lock()


def discover_cast(timeout=3.0):
    """[(friendly name, host, port)] for Chromecasts answering on the LAN."""
    try:
        from zeroconf import Zeroconf, ServiceBrowser
    except ImportError:
        logger.error('zeroconf is unavailable: cannot look for Chromecasts')
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
        logger.error('Chromecast discovery failed: %s', e)
    finally:
        zeroconf.close()
    return sorted(found.values())


def start_search():
    """Kick off a Chromecast search in the background."""
    global _searching
    with _search_lock:
        if _searching:
            return False
        _searching = True
    threading.Thread(target=_search, daemon=True,
                     name='CAST_FILE_SEARCH').start()
    return True


def _search():
    global _devices, _searching
    try:
        found = discover_cast()
        if found:
            # A device asleep right now is still the one being cast to.
            _devices = found
    finally:
        with _search_lock:
            _searching = False


def _ssdp_search(st, timeout=3.0):
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


def _port_suffix(parts, default=''):
    """:port for a urlsplit result, `default` when it names none.

    ``parts.port`` raises on a non-numeric port, and a hand-written device
    description is where that turns up; dropping the whole renderer is worse
    than guessing the scheme's own port.
    """
    try:
        return ':{}'.format(parts.port) if parts.port else default
    except ValueError:
        return default


def parse_description(raw, base_url, peer_ip):
    """(friendly name, absolute AVTransport control URL) or None.

    Namespaces are matched by local name because descriptions in the wild
    disagree on prefix and case, and the control URL is rewritten to the
    address that actually answered: a device that describes itself on loopback
    or forgets its port is common, and a television behind a router hits it
    more often than the spec admits.
    """
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
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(url, timeout=timeout) as response:
            raw = response.read(512 * 1024)
    except Exception as e:
        logger.debug('cannot read %s: %s', url, e)
        return None
    return parse_description(raw, url, peer_ip)


def discover_renderers(timeout=3.0):
    """[(friendly name, control url, host)] for DLNA renderers on the LAN."""
    seen = {}
    try:
        ours = set(Setting.get_advertisable_ip())
    except Exception:
        ours = set()
    for st in DLNA_SEARCH_TARGETS:
        for location, peer in _ssdp_search(st, timeout=timeout):
            if peer in ours:
                continue                      # do not cast to ourselves
            parsed = describe_renderer(location, peer)
            if parsed is None:
                continue
            name, control = parsed
            host = urllib.parse.urlsplit(control).hostname or peer
            seen[host] = (name, control, host)
    return sorted(seen.values())


def start_renderer_search():
    """Kick off a MediaRenderer search in the background."""
    global _dlna_searching
    with _search_lock:
        if _dlna_searching:
            return False
        _dlna_searching = True

    def _run():
        global _dlna_devices, _dlna_searching
        try:
            found = discover_renderers()
            if found:
                _dlna_devices = found
        finally:
            with _search_lock:
                _dlna_searching = False

    threading.Thread(target=_run, daemon=True,
                     name='CAST_FILE_DLNA_SEARCH').start()
    return True


# -- sending to a device -----------------------------------------------------

class CastSender(object):
    """Cast v2 media sending: the handful of requests a remote control makes.

    One socket, one lock, and a request/response `_await` per command. This is
    a controller, not a streaming endpoint, so it needs none of the heartbeat
    machinery the mirroring channel does.
    """

    def __init__(self, host, port=CAST_PORT, timeout=5.0, source_id='sender-0'):
        self.host = host
        self.port = int(port or CAST_PORT)
        self.timeout = max(1.0, float(timeout))
        self.source_id = source_id
        self.sock = None
        self.transport_id = None
        self.app_id = DEFAULT_MEDIA_APP_ID
        self.media_session_id = 1
        self.request_id = 0
        self.lock = threading.Lock()

    def _send(self, destination, namespace, payload, binary=False):
        body = payload if binary else json.dumps(payload)
        blob = encode_cast_message(self.source_id, destination, namespace,
                                   body, binary)
        with self.lock:
            self.sock.sendall(struct.pack('>I', len(blob)) + blob)

    def _recv_exactly(self, count):
        buf = b''
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
            if message.get('payload_type') != 0:
                continue
            try:
                data = json.loads(message.get('payload_utf8') or '{}')
            except ValueError:
                continue
            if data.get('type') == wanted:
                return data
        return None

    def _next_id(self):
        self.request_id += 1
        return self.request_id

    def connect(self):
        raw = socket.create_connection((self.host, self.port),
                                       timeout=self.timeout)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.sock = context.wrap_socket(raw, server_hostname=None)
        self.sock.settimeout(self.timeout)
        self._send('receiver-0', NS_DEVICE_AUTH, DEVICE_AUTH_CHALLENGE,
                   binary=True)
        self._recv()
        self._send('receiver-0', NS_CONNECTION, {'type': 'CONNECT'})
        return self

    @staticmethod
    def _applications(status):
        return (status.get('status') or {}).get('applications') or []

    def launch(self, app_id=None):
        """Launch the default media receiver; return its transport id."""
        self.app_id = app_id or DEFAULT_MEDIA_APP_ID
        self._send('receiver-0', NS_RECEIVER,
                   {'type': 'LAUNCH', 'appId': self.app_id,
                    'requestId': self._next_id()})
        status = self._await('RECEIVER_STATUS') or {}
        applications = self._applications(status)
        if not applications:
            raise RuntimeError('目标设备没有启动媒体接收器（LAUNCH 无响应）')
        self.transport_id = applications[0].get('transportId')
        if not self.transport_id:
            raise RuntimeError('目标设备没有返回 transportId')
        self._send(self.transport_id, NS_CONNECTION, {'type': 'CONNECT'})
        return self.transport_id

    def load(self, url, content_type='', live=False, start=0.0, title='',
             text_tracks=()):
        """LOAD one item. `text_tracks` are sender-declared WebVTT tracks."""
        media = {'contentId': url,
                 'streamType': 'LIVE' if live else 'BUFFERED',
                 'metadata': {'metadataType': 0, 'title': title or ''}}
        if content_type:
            # An empty contentType makes some receivers guess a handler and
            # pick the audio-only one; leaving it out is the documented way.
            media['contentType'] = content_type
        request = {'type': 'LOAD', 'requestId': self._next_id(),
                   'autoplay': True, 'currentTime': float(start),
                   'media': media}
        if text_tracks:
            media['textTracks'] = [
                {'trackId': track['trackId'], 'contentId': track['url'],
                 'type': 'SUBTITLES', 'language': track.get('language') or 'und',
                 'name': track.get('name') or ''} for track in text_tracks]
            # The track ids are ours, so the same numbers go back in
            # activeStreamIds -- that is what switches one on.
            request['activeStreamIds'] = [
                {'streamId': track['trackId'],
                 'displayStatus': track.get('name') or '',
                 'language': track.get('language') or 'und'}
                for track in text_tracks]
        self._send(self.transport_id, NS_MEDIA, request)
        status = self._await('MEDIA_STATUS') or {}
        entries = status.get('status') or []
        if isinstance(entries, dict):
            entries = [entries]
        entry = entries[0] if entries else {}
        self.media_session_id = entry.get('mediaSessionId', 1)
        return entry

    def _media_command(self, payload, reply='MEDIA_STATUS', timeout=None):
        payload = dict(payload)
        payload.setdefault('mediaSessionId', self.media_session_id)
        payload['requestId'] = self._next_id()
        self._send(self.transport_id, NS_MEDIA, payload)
        return self._await(reply, timeout)

    def play(self):
        return self._media_command({'type': 'PLAY'})

    def pause(self):
        return self._media_command({'type': 'PAUSE'})

    def seek(self, seconds):
        return self._media_command({'type': 'SEEK',
                                    'currentTime': float(seconds),
                                    'resumeState': 'PLAYBACK_START'})

    def stop(self):
        try:
            self._media_command({'type': 'STOP'}, timeout=2.0)
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info('the device did not take STOP: %s', e)

    def quit_app(self):
        """Really leave. A bare STOP parks the app on the last frame.

        QUIT_APP is the documented verb, but the sender stacks in the wild do
        not agree on it -- pychromecast's quit_app() sends a receiver-namespace
        STOP -- so a device that stays silent about the first gets the second.
        """
        if self._receiver_command({'type': 'QUIT_APP', 'appId': self.app_id}):
            return
        self._receiver_command({'type': 'STOP'})

    def _receiver_command(self, payload, timeout=2.0):
        """Send one receiver command; True if the device answered."""
        payload = dict(payload)
        payload['requestId'] = self._next_id()
        try:
            self._send('receiver-0', NS_RECEIVER, payload)
            return self._await('RECEIVER_STATUS', timeout) is not None
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info('the device did not take %s: %s', payload['type'], e)
            return False

    def set_volume(self, level):
        self._send('receiver-0', NS_RECEIVER,
                   {'type': 'SET_VOLUME', 'requestId': self._next_id(),
                    'volume': {'level': max(0.0, min(1.0, level / 100.0))}})

    def media_status(self):
        """(playerState, idleReason, currentTime) -- what the watchdog asks."""
        reply = self._media_command({'type': 'GET_STATUS'},
                                    timeout=self.timeout) or {}
        entries = reply.get('status') or []
        if isinstance(entries, dict):
            entries = [entries]
        entry = entries[0] if entries else {}
        return ((entry.get('playerState') or '').upper(),
                entry.get('idleReason') or '',
                float(entry.get('currentTime') or 0.0))

    def applications(self):
        """The app ids the receiver currently has open."""
        self._send('receiver-0', NS_RECEIVER,
                   {'type': 'GET_STATUS', 'requestId': self._next_id()})
        status = self._await('RECEIVER_STATUS') or {}
        return [app.get('appId') for app in self._applications(status)
                if app.get('appId')]

    def close(self):
        if self.sock is not None:
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None


class DlnaSender(object):
    """AVTransport over SOAP: enough verbs to play a file and follow it."""

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
            name=name, value=escape(str(args[name]))) for name in args)
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
            's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
            '<s:Body><u:{action} xmlns:u="{service}">'
            '<InstanceID>{instance}</InstanceID>{fields}'
            '</u:{action}></s:Body></s:Envelope>'
        ).format(action=action, service=DLNA_SERVICE,
                 instance=self.instance_id, fields=fields)
        # Byte length, not character count: a Chinese title is multi-byte and a
        # Content-Length in characters truncates the envelope on the wire.
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
            logger.warning('%s replied with something that is not XML', action)
            return values
        for node in root.iter():
            name = _local_name(node.tag)
            if name == 'instanceid' and (node.text or '').strip():
                self.instance_id = node.text.strip()
            if name.endswith('response'):
                for child in node:
                    values[_local_name(child.tag)] = (child.text or '').strip()
        return values

    def set_uri(self, url, didl):
        return self._request('SetAVTransportURI', {
            'CurrentURI': url, 'CurrentURIMetaData': didl})

    def play(self):
        return self._request('Play', {'Speed': '1'})

    def pause(self):
        return self._request('Pause', {})

    def stop(self):
        try:
            self._request('Stop', {})
        except Exception as e:
            logger.debug('the renderer did not accept Stop: %s', e)

    def seek(self, seconds):
        return self._request('Seek', {'Unit': 'REL_TIME',
                                      'Target': hms(seconds)})

    def transport_state(self):
        state = self._request('GetTransportInfo', {}).get(
            'currenttransportstate', '')
        if state:
            self.last_state = state
        return self.last_state

    def position(self):
        """(reltime seconds, trackduration seconds)."""
        info = self._request('GetPositionInfo', {})
        return parse_time(info.get('reltime')), parse_time(
            info.get('trackduration'))

    def close(self):
        pass


def hms(seconds):
    seconds = int(max(0, seconds or 0))
    return '{:d}:{:02d}:{:02d}'.format(seconds // 3600,
                                        seconds % 3600 // 60, seconds % 60)


def parse_time(text):
    """'0:12:34', '1:02:03.5' or '90' -> seconds; garbage -> 0."""
    try:
        parts = str(text).strip().split(':')
        if len(parts) == 1:
            return float(parts[0])
        value = 0.0
        for part in parts:
            value = value * 60 + float(part)
        return value
    except (TypeError, ValueError):
        return 0.0


def build_didl(url, title, content_type, duration=0.0, size=0,
               upnp_class='object.item.videoItem'):
    """The DIDL-Lite a renderer expects beside its URI."""
    from xml.sax.saxutils import escape
    attrs = ['protocolInfo="http-get:*:{}:DLNA.ORG_OP=01;DLNA.ORG_CI=0"'.format(
        content_type)]
    if duration:
        attrs.append('duration="{}"'.format(hms(duration)))
    if size:
        attrs.append('size="{}"'.format(int(size)))
    return (
        '<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/" '
        'xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">'
        '<item id="0" parent="0" restricted="1">'
        '<dc:title>{}</dc:title><upnp:class>{}</upnp:class>'
        '<res {}>{}</res>'
        '</item></DIDL-Lite>'
    ).format(escape(title or ''), upnp_class, ' '.join(attrs), escape(url))


# -- the settings this plugin reads back ------------------------------------

def target_kind():
    kind = str(Setting.get(SettingProperty.Target_Kind, 'cast') or 'cast')
    return kind if kind in TARGETS else 'cast'


def selected_target():
    """(kind, name, address, peer_ip, port). Empty address when unset.

    `address` is "host:port" for a Chromecast and the control URL for a DLNA
    renderer; `peer_ip` is what the route probe runs against.
    """
    kind = target_kind()
    if kind == 'cast':
        raw = str(Setting.get(SettingProperty.Cast_Target, '') or '')
        name = str(Setting.get(SettingProperty.Cast_Target_Name, '') or '')
        host, _, port = raw.partition(':')
        return kind, name or host, raw, host, int(port) if port.isdigit() \
            else CAST_PORT
    control = str(Setting.get(SettingProperty.Dlna_Control, '') or '')
    name = str(Setting.get(SettingProperty.Dlna_Target_Name, '') or '')
    return kind, name or control, control, urllib.parse.urlsplit(
        control).hostname, CAST_PORT


def mode():
    text = str(Setting.get(SettingProperty.Mode, 'auto') or 'auto')
    return text if text in ('auto', 'direct', 'convert') else 'auto'


def auto_next():
    return Setting.get(SettingProperty.Auto_Next, True) is not False


def hardware_wanted():
    return Setting.get(SettingProperty.Hardware, False) is True


def system_audio_wanted():
    return Setting.get(SettingProperty.System_Audio, False) is True


def options():
    """What the user has chosen, as one plain dict the command builders take."""
    ffmpeg = find_tool('ffmpeg')
    return {
        'audio_index': int_or_none(Setting.get(SettingProperty.Audio_Stream,
                                              '')),
        'subtitle_index': int_or_none(Setting.get(
            SettingProperty.Subtitle_Stream, '')),
        'delay_ms': int_or_none(Setting.get(SettingProperty.Audio_Delay, 0))
        or 0,
        'bitrate': int_or_none(Setting.get(SettingProperty.Bitrate,
                                          DEFAULT_BITRATE))
        or DEFAULT_BITRATE,
        'audio_bitrate': DEFAULT_AUDIO_BITRATE,
        'height': 0,
        'encoder': 'hardware' if (hardware_wanted() and
                                 ffmpeg_has(ffmpeg, 'h264_videotoolbox'))
        else 'software',
        'hardware_name': 'h264_videotoolbox',
    }


def int_or_none(value):
    text = str(value).strip() if value is not None else ''
    try:
        return int(text) if text else None
    except ValueError:
        return None


def number(value, default=0.0):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def parse_start(text, duration=0.0):
    """A DLNA/mpv style `start` string -> seconds.

    Same grammar mpv is handed elsewhere in this app: absolute seconds,
    clock times, an offset, or a percentage of the duration.
    """
    raw = str(text or '').strip()
    if not raw:
        return 0.0
    if raw.endswith('%'):
        try:
            return max(0.0, float(raw[:-1]) / 100.0 * (duration or 0.0))
        except ValueError:
            return 0.0
    negative = raw.startswith('-')
    if raw[:1] in ('+', '-'):
        raw = raw[1:]
    value = parse_time(raw)
    return -value if (negative and value) else value


def local_path(url):
    """A path we can serve out of whatever arrived, or None.

    The menu hands over paths; DLNA and the web entry hand over URLs. A
    `file:` URL is ours to decode, an `http(s)` one is a stream the device can
    fetch by itself, and anything else is not media we can serve.
    """
    text = str(url or '').strip()
    if not text:
        return None
    if text.lower().startswith(('http://', 'https://')):
        return text
    if text.lower().startswith('file:'):
        parsed = urllib.parse.urlsplit(text)
        if parsed.netloc not in ('', 'localhost'):
            return None
        text = urllib.parse.unquote(parsed.path)
        if sys.platform == 'win32' and text.startswith('/') and len(text) > 3:
            text = text.lstrip('/')[:1] + ':' + text[1:]
    path = os.path.expanduser(text)
    if not os.path.isfile(path):
        return None
    return os.path.abspath(path)


def guess_content(path):
    suffix = os.path.splitext(str(path))[1].lower()
    return {'.mp4': 'video/mp4', '.m4v': 'video/mp4', '.mov': 'video/quicktime',
            '.mkv': 'video/x-matroska', '.webm': 'video/webm',
            '.avi': 'video/x-msvideo', '.ts': 'video/mp2t',
            '.m2ts': 'video/mp2t', '.mpg': 'video/mpeg',
            '.mpeg': 'video/mpeg', '.mp3': 'audio/mpeg', '.m4a': 'audio/mp4',
            '.aac': 'audio/aac', '.wav': 'audio/wav',
            '.flac': 'audio/flac', '.ogg': 'audio/ogg'}.get(
                suffix, 'video/mp4')


def suffix_for(content_type, path=''):
    """The file name ending a renderer will expect for this content type."""
    return {
        'video/mp4': 'mp4', 'video/quicktime': 'mov', 'video/mp2t': 'ts',
        'video/x-matroska': 'mkv', 'video/webm': 'webm', 'video/mpeg': 'mpg',
        'audio/mpeg': 'mp3', 'audio/aac': 'aac', 'audio/mp4': 'm4a',
        'audio/wav': 'wav', 'audio/flac': 'flac', 'text/vtt': 'vtt',
    }.get(content_type, os.path.splitext(str(path))[1].lstrip('.') or 'bin')


def list_media(folder, limit=MENU_FILES):
    """(names, total) of media files in `folder`; sorted, capped for the menu."""
    base = os.path.expanduser(str(folder or ''))
    if not base or not os.path.isdir(base):
        return [], 0
    try:
        names = sorted((name for name in os.listdir(base)
                        if not name.startswith('.') and
                        name.lower().endswith(MEDIA_EXTENSIONS)),
                       key=lambda text: text.lower())
    except OSError as e:
        logger.info('cannot read %s: %s', base, e)
        return [], 0
    return names[:limit], len(names)


def choose_folder_dialog():
    """Ask for a folder. macOS only: osascript is always there, and neither
    of the other two has an equally dependency-free picker."""
    if sys.platform != 'darwin':
        return None
    script = 'POSIX path of (choose folder with prompt "选择要投屏的媒体文件夹")'
    try:
        done = subprocess.run(['osascript', '-e', script],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              env=_clean_env(), timeout=120)
    except Exception as e:
        logger.info('the folder picker failed: %s', e)
        return None
    return (done.stdout or b'').decode('utf-8', 'replace').strip() or None


_AUDIO_HEADER = re.compile(r'(?:list of )?audio devices', re.I)
_OTHER_HEADER = re.compile(r'devices\s*:', re.I)
_AUDIO_DEVICE = re.compile(r'(?:\[\s*(\d+)\s*\]|(\d+)\s*\))\s*"?(.*?)"?\s*$')


def _avfoundation_audio_devices(listing):
    """[(index, name)] from `ffmpeg -f avfoundation -list_devices true -i ""`.

    The audio block is the only one that can carry a BlackHole-class loopback,
    and ffmpeg has spelled that block two ways over the years --
    `AVFoundation audio devices:` with `[0] name` lines, and an older
    `List of Audio devices:` with `0) name`. Both are read; a name in quotes is
    stripped. Getting this wrong is silent: system-sound casting just never
    offers itself.
    """
    devices = []
    inside = False
    for line in str(listing or '').splitlines():
        if _AUDIO_HEADER.search(line):
            inside = True
            continue
        if not inside:
            continue
        if _OTHER_HEADER.search(line):
            break                    # the next block: video, monitors, anything
        match = _AUDIO_DEVICE.search(line.strip())
        if match:
            index = match.group(1) or match.group(2)
            name = match.group(3).strip()
            if name:
                devices.append((index, name))
    return devices


def pulse_monitor():
    """The PulseAudio/PipeWire monitor source, or None."""
    try:
        done = subprocess.run(['pactl', 'list', 'short', 'sources'],
                              stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, env=_clean_env(),
                              timeout=5)
    except Exception:
        return None
    for line in (done.stdout or b'').decode('utf-8', 'replace').splitlines():
        fields = line.split('\t')
        if len(fields) > 1 and fields[1].endswith('.monitor'):
            return fields[1]
    return None


def system_audio_input():
    """(label, input args) for this machine's system sound, or None.

    A single-file plugin may not pip-install, so this is limited to what the
    operating system already exposes: a BlackHole-class loopback device on
    macOS, a PulseAudio monitor on Linux. Windows has neither.
    """
    ffmpeg = find_tool('ffmpeg')
    if not ffmpeg:
        return None
    key = ('audio-input', ffmpeg, sys.platform)
    if key not in _tool_cache:
        found = None
        if sys.platform == 'darwin':
            listing = _ask(ffmpeg, ['-hide_banner', '-f', 'avfoundation',
                                    '-list_devices', 'true', '-i', ''])
            for index, name in _avfoundation_audio_devices(listing):
                if re.search(r'blackhole|loopback|soundflower', name, re.I):
                    found = (name, ['-f', 'avfoundation', '-i',
                                    ':{}'.format(index)])
                    break
        elif sys.platform.startswith('linux'):
            monitor = pulse_monitor()
            if monitor:
                found = (monitor, ['-f', 'pulse', '-i', monitor])
        _tool_cache[key] = found
    return _tool_cache[key]


def _keep_awake(platform=None):
    """A two-hour film should not end because the Mac fell asleep."""
    if (platform or sys.platform) != 'darwin':
        return None
    try:
        return subprocess.Popen(['caffeinate', '-dimsu'],
                                stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
    except Exception as e:
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


def _close_quietly(output):
    if output is not None:
        try:
            output.close()
        except Exception:
            pass


def _remove(path):
    try:
        os.remove(path)
    except OSError:
        pass


# -- the renderer -----------------------------------------------------------

class LocalFileRenderer(Renderer):
    """Serves local media to one chosen device and follows what it does."""

    def __init__(self):
        super(LocalFileRenderer, self).__init__()
        self.store = Store()
        self.server = None
        #: The device connection: a CastSender or a DlnaSender.
        self.output = None
        self.job = None
        self.entries = []
        self.queue = []
        self.index = -1
        #: What is playing: a local path, a remote URL, or '系统声音'.
        self.current = ''
        #: 'copy' | 'convert' | 'remote' | 'audio'.
        self.mode = ''
        self.note = ''
        self.position = 0.0
        self.duration = 0.0
        self.state = ''
        #: Bumped on every hand-off so a slow worker cannot overwrite the
        #: session that replaced it.
        self.generation = 0
        self.repush = 0
        self.seen_playing = False
        self.awake = None
        self._lock = threading.RLock()
        self._watchdog = None
        self.renderer_setting = LocalFileSetting()

    # -- what the menu and the state page read ----------------------------

    def status(self):
        """One line saying what is on the television and how it got there."""
        if not self.current:
            return self.note or '等待投屏'
        name = os.path.basename(self.current) if self.mode != 'audio' \
            else '系统声音'
        label = {'copy': '直通', 'convert': '转码', 'remote': '直投网址',
                 'audio': '系统声音'}.get(self.mode, self.mode)
        line = '{} · {}'.format(name, label)
        if 0 <= self.index < len(self.queue) and len(self.queue) > 1:
            line += ' · {}/{}'.format(self.index + 1, len(self.queue))
        if self.duration:
            line += ' · {}/{}'.format(hms(self.position), hms(self.duration))
        return line

    # -- lifecycle ---------------------------------------------------------

    def start(self):
        super(LocalFileRenderer, self).start()
        self._ensure_watchdog()

    def stop(self):
        self.set_media_stop()
        self.running = False
        with self._lock:
            thread, self._watchdog = self._watchdog, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2)
        if self.server is not None:
            try:
                self.server.serving = False
                self.server.shutdown()
                self.server.server_close()
            except Exception:
                pass
            self.server = None
        self.store.clear()
        super(LocalFileRenderer, self).stop()

    # -- entry points ------------------------------------------------------

    def set_media_url(self, url, start='0'):
        """What DLNA, the web entry point and the menu all funnel through."""
        if not url:
            return
        if system_audio_wanted():
            # Audio-only mode owns the device; a pushed URL would otherwise
            # kill a live system-sound stream without saying so.
            self._complain('当前正在只投系统声音，先在菜单里关掉它才能投文件')
            return
        path = local_path(url)
        if path is None:
            self._complain('只认本机文件路径或 http(s) 网址：{}'.format(url))
            return
        if path.startswith('http'):
            # A remote URL is not a list item: it goes straight out.
            threading.Thread(target=self._deliver, args=(path, str(start)),
                             daemon=True, name='CAST_FILE_DELIVER').start()
            return
        with self._lock:
            if path not in self.queue:
                self.queue.append(path)
            index = self.queue.index(path)
        self.play_index(index, start)

    def play_path(self, path, start='0'):
        self.set_media_url(str(path), start)

    def enqueue(self, paths):
        with self._lock:
            for path in paths:
                expanded = os.path.abspath(os.path.expanduser(str(path)))
                if os.path.isfile(expanded) and expanded not in self.queue:
                    self.queue.append(expanded)
            return len(self.queue)

    def clear_queue(self):
        with self._lock:
            self.queue = []
            self.index = -1

    def play_index(self, index, start='0'):
        with self._lock:
            if not (0 <= index < len(self.queue)):
                return False
            self.index = index
            path = self.queue[index]
        threading.Thread(target=self._deliver, args=(path, str(start)),
                         daemon=True, name='CAST_FILE_DELIVER').start()
        return True

    def play_next(self):
        with self._lock:
            if self.index + 1 >= len(self.queue):
                self._complain('已经是列表最后一个')
                return False
            index = self.index + 1
        return self.play_index(index)

    def play_previous(self):
        with self._lock:
            if self.index <= 0:
                self._complain('已经是列表第一个')
                return False
            index = self.index - 1
        return self.play_index(index)

    def play_folder(self, folder=None, start_at=0):
        base = str(folder or Setting.get(SettingProperty.Folder, '') or '')
        names, _total = list_media(base)
        if not names:
            self._complain('这个文件夹里没有认得的媒体文件')
            return False
        paths = [os.path.join(os.path.expanduser(base), name)
                 for name in names]
        self.clear_queue()
        self.enqueue(paths)
        return self.play_index(min(start_at, len(paths) - 1))

    def set_system_audio(self, on):
        """Audio-only mode: this machine's system sound, streamed live."""
        Setting.set(SettingProperty.System_Audio, bool(on))
        if not on:
            self.set_media_stop()
            return True
        kind = target_kind()
        if kind != 'cast':
            self._complain('只投系统声音目前只支持 Chromecast 目标')
            Setting.set(SettingProperty.System_Audio, False)
            return False
        source = system_audio_input()
        if not source:
            self._complain('找不到系统声音采集口（macOS 需要 BlackHole 这类'
                           '虚拟声卡，Linux 需要 PulseAudio monitor）')
            Setting.set(SettingProperty.System_Audio, False)
            return False
        threading.Thread(target=self._deliver_audio, args=(source,),
                         daemon=True, name='CAST_FILE_AUDIO').start()
        return True

    # -- playback verbs ----------------------------------------------------

    def set_media_pause(self):
        with self._lock:
            output = self.output
        if output is None:
            return
        try:
            output.pause()
            self.state = 'PAUSED'
            self.set_state_transport('PAUSED_PLAYBACK')
        except (OSError, ssl.SSLError, ValueError,
                urllib.error.URLError) as e:
            logger.info('pause did not reach the device: %s', e)

    def set_media_resume(self):
        with self._lock:
            output = self.output
        if output is None:
            return
        try:
            output.play()
            self.state = 'PLAYING'
            self.set_state_transport('PLAYING')
        except (OSError, ssl.SSLError, ValueError,
                urllib.error.URLError) as e:
            logger.info('resume did not reach the device: %s', e)

    def set_media_stop(self, quit_app=True):
        with self._lock:
            self.generation += 1
            output, self.output = self.output, None
            job, self.job = self.job, None
            entries, self.entries = self.entries, []
            awake, self.awake = self.awake, None
        _teardown(output, job, entries, self.store, keep_app=not quit_app)
        _stop_awake(awake)
        self.state = 'STOPPED'
        self.position = 0.0
        self.seen_playing = False
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_volume(self, data):
        with self._lock:
            output = self.output
        if not isinstance(output, CastSender):
            # Volume lives in the renderer's RenderingControl service, a second
            # control URL we deliberately do not parse here: the remote is
            # closer to the amplifier than this menu is.
            logger.info('this target\'s volume belongs to its own remote')
            return
        try:
            output.set_volume(int(number(data)))
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info('volume did not reach the device: %s', e)

    def set_media_position(self, data):
        """Seek. On the conversion path this restarts the encoder, because a
        transcode in progress has no index to jump into."""
        self.seek_to(parse_time(data))

    def seek_to(self, seconds):
        seconds = max(0.0, float(seconds))
        with self._lock:
            output = self.output
            converting = self.mode == 'convert'
            path = self.current
        if output is None:
            return False
        if converting:
            self.play_path(path, hms(seconds))
            return True
        try:
            output.seek(seconds)
            self.position = seconds
            return True
        except (OSError, ssl.SSLError, ValueError,
                urllib.error.URLError) as e:
            logger.info('seek did not reach the device: %s', e)
            return False

    # -- the delivery worker ----------------------------------------------

    def _deliver(self, path, start, retry=False):
        generation = self._begin(path, retry)
        kind, name, address, peer, port = selected_target()
        if not address:
            return self._fail(generation,
                              '还没有选择投屏目标：在菜单栏「输出目标」里选一台')
        if path.startswith('http'):
            # A finished URL the device can fetch by itself. Serving it through
            # us would only add a hop and a timeout.
            return self._push(generation, kind, name, address, peer, port,
                              path, guess_content(path), False,
                              path.rsplit('/', 1)[-1], Media(),
                              parse_start(start), [], 'remote')
        media = probe_media(path)
        remember_probe(path, media)
        chosen = options()
        decision, reason = plan(
            media, kind, chosen['audio_index'], chosen['subtitle_index'],
            chosen['delay_ms'], mode(),
            ffmpeg_has(find_tool('ffmpeg'), 'subtitles'))
        self.note = reason
        logger.info('%s -> %s: %s（%s）', os.path.basename(path), decision,
                    reason, TARGETS.get(kind, kind))
        try:
            if decision == 'copy':
                url, content_type, live = self._serve_copy(path, media, peer)
                tracks = []
                if chosen['subtitle_index'] is not None and kind == 'cast':
                    sidecar = self._subtitle_sidecar(path, media,
                                                     chosen['subtitle_index'],
                                                     peer)
                    if sidecar:
                        tracks = [sidecar]
            else:
                url, content_type, live = self._serve_convert(
                    path, media, chosen, kind, peer)
                tracks = []
            offset = parse_start(start, media.duration)
        except Exception as e:
            logger.exception('preparing %s failed', path)
            self._retire()
            return self._fail(generation, '准备媒体失败：{}'.format(e))
        self._push(generation, kind, name, address, peer, port, url,
                   content_type, live, os.path.basename(path), media, offset,
                   tracks, decision)

    def _deliver_audio(self, source):
        generation = self._begin('系统声音')
        kind, name, address, peer, port = selected_target()
        if not address:
            return self._fail(generation, '还没有选择投屏目标')
        label, inputs = source
        ffmpeg = find_tool('ffmpeg')
        args = inputs + ['-vn', '-c:a', 'aac', '-ar', str(AUDIO_SAMPLE_RATE),
                         '-ac', '2', '-b:a', str(DEFAULT_AUDIO_BITRATE),
                         '-f', 'adts']
        job = Job(ffmpeg, args, 'aac', 'audio/aac', live=True, label=label)
        if not job.start():
            return self._fail(generation, 'ffmpeg 没能开始采集系统声音')
        entry = self.store.add(None, 'audio/aac', 'aac', job=job, title=label)
        with self._lock:
            self.job = job
            self.entries = [entry]
        url = entry_url(self._ensure_server(), entry, peer)
        self._push(generation, kind, name, address, peer, port, url,
                   'audio/aac', True, '系统声音（{}）'.format(label), Media(),
                   0.0, [], 'audio')

    def _serve_copy(self, path, media, peer):
        content_type = media.content_type() if media.ok else \
            guess_content(path)
        entry = self.store.add(
            path, content_type, suffix_for(content_type, path),
            title=os.path.basename(path), dlna=target_kind() == 'dlna')
        with self._lock:
            self.entries = [entry]
        return entry_url(self._ensure_server(), entry, peer), \
            content_type, False

    def _serve_convert(self, path, media, chosen, kind, peer):
        ffmpeg = find_tool('ffmpeg')
        if not ffmpeg:
            raise RuntimeError('这个格式需要转码，但机器上没有 ffmpeg')
        bitrate, advertised = bitrate_for_convert(media, chosen['bitrate'])
        arguments = dict(chosen)
        arguments['bitrate'] = bitrate
        if media.duration:
            # A long film at a capped bitrate does not need full resolution;
            # this is where the "it looks softer than the file" complaint comes
            # from, so it is capped here rather than by lying about the length.
            arguments['height'] = 720 if bitrate < 3000000 else 0
        # What the device is handed is this transcode, so its length and size
        # are the estimated ones; the source file's size would not match.
        media.size = advertised
        root = temp_root()
        estimate = max(advertised, int(bitrate / 8 * 600))
        spare = free_space(root)
        if spare < estimate:
            raise RuntimeError('临时目录放不下这次的转码（约需 {:d} MB，{} 只剩 '
                               '{:d} MB），在菜单里换个目录或调低码率'.format(
                                   int(estimate / 1048576), root,
                                   int(spare / 1048576)))
        sidecar = None
        if arguments.get('subtitle_index') is not None and ffmpeg_has(
                ffmpeg, 'subtitles'):
            sidecar = self._extract_subtitle(path, media,
                                             arguments['subtitle_index'])
        arguments['burn_subtitle'] = bool(sidecar)
        arguments['subtitle_path'] = sidecar
        job = Job(ffmpeg, convert_args(path, media, arguments), 'ts',
                  'video/mp2t', total=0 if kind == 'cast' else advertised,
                  live=kind == 'cast', label=os.path.basename(path))
        if not job.start():
            job.cleanup()
            raise RuntimeError('ffmpeg 没能开始转码：{}'.format(job.error()))
        entry = self.store.add(job.path, 'video/mp2t', 'ts', job=job,
                               title=os.path.basename(path),
                               dlna=kind == 'dlna')
        with self._lock:
            self.job = job
            self.entries = [entry]
        return entry_url(self._ensure_server(), entry, peer), \
            'video/mp2t', kind == 'cast'

    def _extract_subtitle(self, path, media, index):
        """Embedded track -> SRT on disk, for burning in."""
        target = os.path.join(temp_root(),
                              'macast-sub-%s.srt' % secrets.token_hex(4))
        if not self._run_subtitle(path, media.subtitle_position(index), target):
            _remove(target)
            return None
        with self._lock:
            self.entries.append(
                self.store.add(target, 'text/plain', 'srt', title='subtitle',
                               temp=True))
        return target

    def _subtitle_sidecar(self, path, media, index, peer):
        """WebVTT sidecar plus the `textTracks` entry Chromecast draws."""
        target = os.path.join(temp_root(),
                              'macast-sub-%s.vtt' % secrets.token_hex(4))
        if not self._run_subtitle(path, media.subtitle_position(index), target):
            _remove(target)
            self.note = '第 {} 条字幕转 WebVTT 失败（电视只认这一种）'.format(index)
            return None
        entry = self.store.add(target, 'text/vtt', 'vtt', title='subtitle',
                               temp=True)
        with self._lock:
            self.entries.append(entry)
        return {'trackId': 1,
                'url': entry_url(self._ensure_server(), entry, peer),
                'language': 'und', 'name': 'Macast 字幕 #{}'.format(index)}

    def _run_subtitle(self, path, position, target):
        """One subtitle track, by its position among the subtitle streams."""
        ffmpeg = find_tool('ffmpeg')
        if not ffmpeg:
            return False
        args = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-i', path,
                '-map', '0:s:{}'.format(int(position)), target]
        try:
            done = subprocess.run(args, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.PIPE,
                                  stdin=subprocess.DEVNULL, env=_clean_env(),
                                  timeout=120)
        except Exception as e:
            logger.info('subtitle extraction failed: %s', e)
            return False
        if done.returncode != 0 or not os.path.isfile(target) or \
                os.path.getsize(target) == 0:
            _remove(target)
            return False
        return True

    def _push(self, generation, kind, name, address, peer, port, url,
              content_type, live, title, media, offset, tracks, decision):
        if not self._alive(generation):
            # A newer hand-off already owns the renderer; give back what this
            # one made, or its ffmpeg outlives the session that started it.
            return self._retire()
        timeout = number(Setting.get(SettingProperty.Timeout, 5), 5.0)
        try:
            if kind == 'cast':
                sender = CastSender(address.partition(':')[0], port,
                                    timeout=timeout)
                sender.connect()
                sender.launch()
                entry = sender.load(url, content_type, live=live, start=offset,
                                    title=title, text_tracks=tracks)
                state = (entry.get('playerState') or '').upper()
            else:
                sender = DlnaSender(address, timeout=timeout)
                sender.set_uri(url, build_didl(
                    url, title, content_type, media.duration,
                    0 if live else media.size, media.upnp_class))
                sender.play()
                if offset:
                    # A renderer always starts at zero, so a resume is a seek
                    # after the load. A device that refuses it still plays the
                    # item -- from the top, which is not worth failing for.
                    try:
                        sender.seek(offset)
                    except Exception as e:
                        logger.info('the renderer would not seek to %.0f s: %s',
                                    offset, e)
                state = sender.transport_state()
        except Exception as e:
            logger.exception('casting %s failed', title)
            return self._fail(generation, '投屏失败：{}'.format(e))
        with self._lock:
            # A hand-off superseded while we were shaking hands owns nothing:
            # retire it outside the lock, because killing an ffmpeg waits.
            stale = generation != self.generation
            awake = False
            if not stale:
                self.output = sender
                self.mode = decision
                self.duration = media.duration or 0.0
                self.position = offset
                self.state = state
                self.seen_playing = state in ('PLAYING', 'CURRENTLY_PLAYING')
                awake = self.awake is None
        if stale:
            sender.close()
            return self._retire()
        if awake:
            self.awake = _keep_awake()
        self.set_state_transport('PLAYING')
        if self.duration:
            self.set_state_duration(hms(self.duration))
        cherrypy.engine.publish('renderer_av_uri', url)
        self._ensure_watchdog()
        logger.info('casting %s to %s (%s)', title, name, address)

    # -- the watchdog ------------------------------------------------------

    def _ensure_watchdog(self):
        with self._lock:
            if self._watchdog is not None and self._watchdog.is_alive():
                return
            self._watchdog = threading.Thread(
                target=self._watch_loop, daemon=True,
                name='CAST_FILE_WATCHDOG')
            self._watchdog.start()

    def _watch_loop(self):
        """Follow what the device says: advance the list, take the TV back."""
        while self.running:
            time.sleep(WATCHDOG_SECONDS)
            with self._lock:
                output = self.output
                generation = self.generation
                active = bool(self.current)
            if output is None or not active:
                continue
            try:
                state, reason, position = self._poll(output)
            except Exception as e:
                logger.debug('the watchdog cannot reach the device: %s', e)
                continue
            with self._lock:
                if output is not self.output or generation != self.generation:
                    continue
                self.state = state or self.state
                if position:
                    self.position = position
            if state in ('PLAYING', 'CURRENTLY_PLAYING'):
                self.seen_playing = True
                continue
            if state in ('PAUSED', 'PAUSED_PLAYBACK') or reason == 'INTERRUPTED':
                continue
            if not self.seen_playing and not self.repush:
                # The load has not landed yet -- not a fault. Once we are past
                # the first take-back, though, a device that never comes back
                # is the fault, or `MAX_REPUSH` would never be reached.
                continue
            if reason == 'FINISHED' or self._at_end():
                self._ended(generation)
                continue
            self._take_back(generation)

    def _poll(self, output):
        """(state, idleReason, position) from whichever protocol we are on."""
        if isinstance(output, CastSender):
            state, reason, position = output.media_status()
            if state == 'IDLE':
                apps = output.applications()
                if apps and output.app_id not in apps:
                    return 'SQUATTED', '', position
            return state, reason, position
        state = output.transport_state()
        try:
            position, duration = output.position()
        except Exception:
            position, duration = 0.0, 0.0
        if duration:
            self.duration = duration
        return state, '', position

    def _at_end(self):
        if not self.duration or self.mode != 'copy':
            return False          # a transcode's clock restarts with the file
        return self.position >= self.duration - 1.5

    def _ended(self, generation):
        with self._lock:
            self.generation += 1
            output, self.output = self.output, None
            job, self.job = self.job, None
            entries, self.entries = self.entries, []
            index = self.index + 1
            advance = auto_next() and index < len(self.queue)
            awake, self.awake = self.awake, None
        # Quitting the app between two items would flash the TV's idle screen
        # and cost a re-LAUNCH; only the last one gives the panel back.
        _teardown(output, job, entries, self.store, keep_app=advance)
        _stop_awake(awake)
        self.seen_playing = False
        if advance:
            logger.info('the device finished this item; advancing the list')
            self.play_index(index)
        else:
            self.set_media_stop()

    def _take_back(self, generation):
        """Somebody else used the television, or our session dropped."""
        with self._lock:
            self.repush += 1
            attempts = self.repush
            path = self.current
            position = self.position
            mode_label = self.mode
        if mode_label == 'audio':
            self._fail(generation, '系统声音投屏已断开：设备不再播放')
            # Switch the mode off as well: leaving the checkbox on would promise
            # a stream that is gone, and the watchdog would keep re-reporting it.
            self.set_system_audio(False)
            return
        if attempts > MAX_REPUSH:
            self._fail(generation,
                       '电视被别的设备占用，已停止重试（最多 {} 次）'.format(
                           MAX_REPUSH))
            # Hand the panel back: without this the watchdog keeps polling a
            # device that refuses to play and re-notifies every tick.
            self.set_media_stop()
            return
        logger.info('the device no longer plays our item; re-pushing from '
                    '%.0f s (attempt %s)', position, attempts)
        with self._lock:
            self.generation += 1
            output, self.output = self.output, None
            job, self.job = self.job, None
            entries, self.entries = self.entries, []
        _teardown(output, job, entries, self.store, keep_app=True)
        self._deliver(path, hms(position), retry=True)

    # -- shared plumbing ---------------------------------------------------

    def _begin(self, path, retry=False):
        """Start a hand-off: retire whatever the last one owned.

        `retry` is the watchdog's own re-push: it must not buy itself a fresh
        budget, or `MAX_REPUSH` would never be reached and a television that
        refuses to play would be hammered for as long as the plugin runs.
        """
        with self._lock:
            self.generation += 1
            generation = self.generation
            output, self.output = self.output, None
            job, self.job = self.job, None
            entries, self.entries = self.entries, []
            awake, self.awake = self.awake, None
            self.current = path
            self.note = ''
            self.mode = ''
            self.duration = 0.0
            self.position = 0.0
            self.seen_playing = False
            if not retry:
                self.repush = 0
        _teardown(output, job, entries, self.store, keep_app=True)
        _stop_awake(awake)
        return generation

    def _retire(self):
        """Give back what this session made but will never own."""
        with self._lock:
            job, entries = self.job, self.entries
            self.job, self.entries = None, []
        return _teardown(None, job, entries, self.store)

    def _alive(self, generation):
        with self._lock:
            return generation == self.generation

    def _ensure_server(self):
        with self._lock:
            if self.server is None:
                self.server = start_media_server(self.store)
            return self.server

    def _fail(self, generation, message):
        if not self._alive(generation):
            return
        logger.error(message)
        self.note = message
        with self._lock:
            self.mode = ''
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        cherrypy.engine.publish('app_notify', 'Macast', message)

    def _complain(self, message):
        logger.error(message)
        self.note = message
        cherrypy.engine.publish('app_notify', 'Macast', message, sound=False)


def _teardown(output, job, entries, store, keep_app=False):
    """Release one session's ownership. STOP alone leaves the last frame on
    the panel, so a real stop also quits the receiver app; `keep_app` skips
    that for a hand-off, where the next LOAD follows immediately."""
    if output is not None:
        try:
            output.stop()
            if isinstance(output, CastSender) and not keep_app:
                output.quit_app()
        except Exception as e:
            logger.info('teardown did not reach the device: %s', e)
        _close_quietly(output)
    if job is not None:
        job.stop()
        job.cleanup()
    for entry in entries:
        store.drop(entry)
        if entry.temp:
            _remove(entry.path)


# -- the menu ---------------------------------------------------------------

class LocalFileSetting(RendererSetting):
    """The menu bar half: choose a device, choose files, steer playback."""

    def _renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        renderer = renderers.pop() if renderers else None
        return renderer if isinstance(renderer, LocalFileRenderer) else None

    def build_menu(self):
        kind = target_kind()
        if kind == 'cast' and not _devices:
            start_search()
        if kind == 'dlna' and not _dlna_devices:
            start_renderer_search()
        renderer = self._renderer()
        items = [
            MenuItem('Local File Caster v0.1', enabled=False),
            MenuItem(renderer.status() if renderer else '等待投屏',
                     enabled=False),
        ]
        controls = self._control_children(renderer)
        if controls:
            items.append(MenuItem('播放控制', children=controls))
        items.append(MenuItem('输出目标', children=self._target_children()))
        items.append(MenuItem('媒体文件夹', children=self._folder_children()))
        items.append(MenuItem('播放列表', children=self._queue_children(renderer)))
        items.append(MenuItem('音轨与字幕', children=self._track_children()))
        items.append(MenuItem('同步与码率', children=self._quality_children()))
        items.append(MenuItem('处理方式', children=self._mode_children()))
        items.append(MenuItem('只投系统声音', self.on_audio_clicked,
                              checked=system_audio_wanted()))
        note = self._note(renderer)
        if note:
            items.append(MenuItem(note, enabled=False))
        return items

    def _note(self, renderer):
        """What the current choices cost, in the user's terms."""
        if mode() == 'convert':
            return ('转码临时目录 {}（结束即删）· 上限 {:d} MB'.format(
                temp_root(), int(MAX_ADVERTISED_SIZE / 1048576)))
        if renderer is None or not renderer.current:
            return None
        if renderer.mode == 'convert':
            return '转码中：{}'.format(renderer.note)
        if renderer.mode == 'copy':
            return '直通：{}'.format(renderer.note)
        if renderer.mode == 'audio':
            return '只有声音；目标设备不支持时请先关掉头选'
        return renderer.note

    # -- playback ----------------------------------------------------------

    def _control_children(self, renderer):
        if renderer is None or not renderer.current:
            return []
        paused = renderer.state in ('PAUSED', 'PAUSED_PLAYBACK')
        return [MenuItem('继续' if paused else '暂停', self.on_toggle_clicked),
                MenuItem('下一个', self.on_next_clicked),
                MenuItem('上一个', self.on_previous_clicked),
                MenuItem('停止（让电视回主页）', self.on_stop_clicked)]

    def on_toggle_clicked(self, item):
        renderer = self._renderer()
        if renderer is None or not renderer.current:
            return
        if renderer.state in ('PAUSED', 'PAUSED_PLAYBACK'):
            renderer.set_media_resume()
        else:
            renderer.set_media_pause()

    def on_next_clicked(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.play_next()

    def on_previous_clicked(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.play_previous()

    def on_stop_clicked(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.set_media_stop()

    def on_audio_clicked(self, item):
        renderer = self._renderer()
        if renderer is None:
            return
        if system_audio_wanted():
            renderer.set_system_audio(False)
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '已停止只投系统声音', sound=False)
        else:
            renderer.set_system_audio(True)

    # -- targets -----------------------------------------------------------

    def _target_children(self):
        kind = target_kind()
        _, name, address, _peer, _port = selected_target()
        children = [MenuItem(TARGETS[key], self.on_kind_clicked,
                             checked=(key == kind), data=key)
                    for key in ('cast', 'dlna')]
        children.append(MenuItem('未选择设备' if not address else
                                 '{} · {}'.format(name, address), enabled=False))
        if kind == 'cast':
            for device, host, port in list(_devices):
                target = '{}:{}'.format(host, port)
                children.append(MenuItem('{} · {}'.format(device, host),
                                         self.on_cast_clicked,
                                         checked=(target == address),
                                         data=(device, target)))
            children.append(MenuItem('重新搜索 Chromecast', self.on_cast_search))
        else:
            for device, control, host in list(_dlna_devices):
                children.append(MenuItem('{} · {}'.format(device, host),
                                         self.on_dlna_clicked,
                                         checked=(control == address),
                                         data=(device, control)))
            children.append(MenuItem('重新搜索 DLNA 电视', self.on_dlna_search))
        return children

    def on_kind_clicked(self, item):
        Setting.set(SettingProperty.Target_Kind, item.data)
        cherrypy.engine.publish('app_notify', 'Macast',
                                '输出目标类型：{}'.format(TARGETS[item.data]),
                                sound=False)

    def on_cast_clicked(self, item):
        name, target = item.data
        Setting.set(SettingProperty.Target_Kind, 'cast')
        Setting.set(SettingProperty.Cast_Target, target)
        Setting.set(SettingProperty.Cast_Target_Name, name)
        self._move(name)

    def on_cast_search(self, item):
        start_search()
        cherrypy.engine.publish('app_notify', 'Macast', '正在搜索 Chromecast…',
                                sound=False)

    def on_dlna_clicked(self, item):
        name, control = item.data
        Setting.set(SettingProperty.Target_Kind, 'dlna')
        Setting.set(SettingProperty.Dlna_Control, control)
        Setting.set(SettingProperty.Dlna_Target_Name, name)
        self._move(name)

    def on_dlna_search(self, item):
        start_renderer_search()
        cherrypy.engine.publish('app_notify', 'Macast',
                                '正在搜索 DLNA 电视…', sound=False)

    def _move(self, name):
        """Re-cast what is playing, so changing target needs no replay."""
        renderer = self._renderer()
        if renderer is not None and renderer.current:
            position = renderer.position
            path = renderer.current
            renderer.play_path(path, hms(position))
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '正在改投：{}'.format(name), sound=False)
        else:
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '投屏目标：{}'.format(name), sound=False)

    # -- folder and queue --------------------------------------------------

    def _folder_children(self):
        folder = str(Setting.get(SettingProperty.Folder, '') or '')
        names, total = list_media(folder) if folder else ([], 0)
        children = [MenuItem('选择文件夹…', self.on_choose_folder),
                    MenuItem(folder or '尚未选择（也可在设置页里填路径）',
                             enabled=False)]
        for name in names:
            children.append(MenuItem(name, self.on_file_clicked,
                                     data=os.path.join(folder, name)))
        if total > len(names):
            children.append(MenuItem('……共 {} 个，只显示前 {} 个'.format(
                total, len(names)), enabled=False))
        if folder and not names:
            children.append(MenuItem('这个文件夹里没有认得的媒体文件',
                                     enabled=False))
        if names:
            children.append(MenuItem('全部按顺序播', self.on_play_folder,
                                     data=folder))
        return children

    def on_choose_folder(self, item):
        def _run():
            path = choose_folder_dialog()
            if not path:
                if sys.platform != 'darwin':
                    cherrypy.engine.publish(
                        'app_notify', 'Macast',
                        '这个平台请在设置页里填「媒体文件夹」路径', sound=False)
                return
            Setting.set(SettingProperty.Folder, path.rstrip('/'))
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '媒体文件夹：{}'.format(path), sound=False)

        threading.Thread(target=_run, daemon=True,
                         name='CAST_FILE_CHOOSE').start()

    def on_file_clicked(self, item):
        renderer = self._renderer()
        path = item.data
        if renderer is None or not os.path.isfile(path):
            return
        folder = os.path.dirname(path)
        Setting.set(SettingProperty.Folder, folder)
        names, _total = list_media(folder)
        renderer.clear_queue()
        renderer.enqueue([os.path.join(folder, name) for name in names])
        renderer.play_path(path)

    def on_play_folder(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.play_folder(item.data)

    def _queue_children(self, renderer):
        if renderer is None:
            return []
        children = []
        for position, path in enumerate(renderer.queue[:MENU_FILES]):
            children.append(MenuItem(os.path.basename(path),
                                     self.on_queue_clicked,
                                     checked=(position == renderer.index),
                                     data=position))
        if not children:
            children.append(MenuItem('队列为空（在「媒体文件夹」里点一个文件）',
                                     enabled=False))
        children.append(MenuItem('清空列表', self.on_clear_clicked))
        children.append(MenuItem('播完自动下一个', self.on_auto_clicked,
                                 checked=auto_next()))
        return children

    def on_queue_clicked(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.play_index(item.data)

    def on_clear_clicked(self, item):
        renderer = self._renderer()
        if renderer is not None:
            renderer.clear_queue()

    def on_auto_clicked(self, item):
        Setting.set(SettingProperty.Auto_Next, not auto_next())

    # -- tracks, sync, bitrate, mode ---------------------------------------

    def _current_media(self):
        """Probed tracks for what is playing, from a cached probe.

        The menu must not spawn ffprobe on the UI thread, so this reads a
        small per-path cache that the delivery worker fills.
        """
        renderer = self._renderer()
        path = renderer.current if renderer else ''
        if not path or path.startswith('http'):
            return Media()
        return _probe_cache.get(path) or Media()

    def _track_children(self):
        media = self._current_media()
        audio = Setting.get(SettingProperty.Audio_Stream, '')
        subtitle = Setting.get(SettingProperty.Subtitle_Stream, '')
        chosen_audio = int_or_none(audio)
        chosen_subtitle = int_or_none(subtitle)
        children = [MenuItem('音频：跟随文件', self.on_audio_stream_clicked,
                             checked=chosen_audio is None, data='')]
        children += [MenuItem(track.label(), self.on_audio_stream_clicked,
                              checked=(track.index == chosen_audio),
                              data=str(track.index))
                     for track in media.audio[:MENU_TRACKS]]
        children.append(MenuItem('字幕：不投', self.on_subtitle_clicked,
                                 checked=chosen_subtitle is None, data=''))
        children += [MenuItem(track.label(), self.on_subtitle_clicked,
                              checked=(track.index == chosen_subtitle),
                              data=str(track.index))
                     for track in media.subtitles[:MENU_TRACKS]]
        if media.ok and not media.subtitles:
            children.append(MenuItem('这个文件没有内嵌字幕', enabled=False))
        children.append(MenuItem('音画同步', children=self._delay_children()))
        return children

    def on_audio_stream_clicked(self, item):
        Setting.set(SettingProperty.Audio_Stream, item.data)
        self._restart('音轨')

    def on_subtitle_clicked(self, item):
        Setting.set(SettingProperty.Subtitle_Stream, item.data)
        self._restart('字幕')

    def _restart(self, what):
        renderer = self._renderer()
        if renderer is not None and renderer.current and \
                not renderer.current.startswith('http'):
            position = renderer.position
            path = renderer.current
            renderer.play_path(path, hms(position))
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '{}已更改，重新投屏'.format(what),
                                    sound=False)

    def _delay_children(self):
        delay = int(number(Setting.get(SettingProperty.Audio_Delay, 0)))
        return [MenuItem('不同步' if not value else '{} ms'.format(value),
                         self.on_delay_clicked, checked=(delay == value),
                         data=value)
                for value in (0, -750, -500, -250, 250, 500, 750, 1000)]

    def on_delay_clicked(self, item):
        Setting.set(SettingProperty.Audio_Delay, int(item.data))
        self._restart('音画同步')

    def _quality_children(self):
        bitrate = int(number(Setting.get(SettingProperty.Bitrate,
                                         DEFAULT_BITRATE)))
        children = [MenuItem('{:.0f} Mbps'.format(value / 1000000.0),
                             self.on_bitrate_clicked, checked=(bitrate == value),
                             data=value)
                    for value in (2000000, 4000000, 6000000, 10000000,
                                  16000000)]
        children.append(MenuItem('转码用硬件编码器（可用时）',
                                 self.on_hardware_clicked,
                                 checked=hardware_wanted()))
        return children

    def on_bitrate_clicked(self, item):
        Setting.set(SettingProperty.Bitrate, int(item.data))

    def on_hardware_clicked(self, item):
        Setting.set(SettingProperty.Hardware, not hardware_wanted())

    def _mode_children(self):
        current = mode()
        return [MenuItem(label, self.on_mode_clicked,
                         checked=(current == key), data=key)
                for key, label in (('auto', '自动（按文件判断）'),
                                   ('direct', '强制直通（可能黑屏）'),
                                   ('convert', '强制转码'))]

    def on_mode_clicked(self, item):
        Setting.set(SettingProperty.Mode, item.data)
        self._restart('处理方式')


#: The menu reads this instead of probing: ffprobe on the UI thread would
#: freeze the menu bar on a network volume. The delivery worker fills it.
_probe_cache = {}


def remember_probe(path, media):
    if path and media.ok and len(_probe_cache) < 64:
        _probe_cache[path] = media


if __name__ == '__main__':
    gui(LocalFileRenderer())
