# Copyright (c) 2026 by pingod. All Rights Reserved.
# The relay that serves a resolved page address to the device.
#
# Macast metadata
# <macast.title>Media Relay</macast.title>
# <macast.role>core</macast.role>
#
# What this is for: `macast/media_resolve.py` produces an address that *we* can
# read -- usually because it came with a Referer, a Cookie, or a signature that
# expires. Handing that address to a TV is the failure the whole feature exists
# to avoid: the device fetches it minutes later, from its own IP, with no headers,
# and reports "won't play" -- a symptom that looks like a protocol bug. So the
# renderer is given an address on *this* machine instead, and this module answers
# it by fetching upstream with the headers attached.
#
# It runs on its own HTTP server, not the app's CherryPy tree, for the same
# reason `screen_mirror` and `cast_local_file` each have one: a media fetch holds
# its connection for the length of the video, and the management API has to stay
# answerable while that is happening. A random port on all interfaces, the port
# only ever appearing inside the media URL we hand out.
#
# Three serving shapes, chosen by `media_resolve.plan()`:
#
#   proxy   Progressive files (mp4/webm/mkv...). No ffmpeg, no CPU, no temp file:
#           the upstream bytes are forwarded. If the origin honours Range, the
#           relay honours Range too and the viewer can seek; if it does not, the
#           relay has no length to advertise and the page says so.
#   remux   HLS/DASH manifests. ffmpeg copies the elementary streams into one
#           progressive MP4 in a temp file. While that file is growing the relay
#           can only answer from byte 0 with no length (so: no seeking), and the
#           moment ffmpeg exits 0 with real bytes the length becomes known and
#           Range serving starts to work. That asymmetry is the honest UI copy,
#           not a bug.
#   merge   A DASH ladder that split the picture and the sound into two addresses.
#           Same file, same states, one more `-i` -- so `Relay.mode` stays 'remux'
#           and the split lives only in `Relay.audio_url`. A relay does not need to
#           know where its bytes came from in order to serve them, and every reader
#           that asked "is this a growing file" would otherwise have to learn a
#           second answer.
#
# Two red lines this repo has already bled over, both restated here because the
# same traps are live in a third place now:
#   * `parse_range(None)` is "from the beginning" -- a whole-file answer, a 200,
#     and NO `Content-Range`. A 206 is only for a request that actually carried a
#     Range header (§4.8, cast_local_file's red line ①).
#   * a name that is registered but whose bytes are gone is refused, not answered
#     with a lengthless 200 that leaves the player waiting forever (§4.8 again).
#
# `parse_range` is the second copy of that parser in this repo; the first is in
# the `cast_local_file` plugin, which core may not import. Part 59 pins the two
# against one table of headers so they cannot drift apart silently -- that is the
# alternative to moving the plugin's copy, which would reopen a shipped plugin's
# behaviour for a feature that does not need it.

import hmac
import json
import logging
import os
import re
import secrets
import subprocess
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from macast import media_resolve
from macast.utils import SETTING_DIR, Setting

logger = logging.getLogger("MediaRelay")
logger.setLevel(logging.INFO)

#: The relay's own address space: /relay/<id>/media, /relay/<id>/status.
RELAY_PREFIX = '/relay/'
MEDIA_TAIL = 'media'
STATUS_TAIL = 'status'

#: A relay lives until nobody watches it for this long. It is a URL the user can
#: paste into a chat, so it must not outlive the session that made it -- and it
#: carries the upstream cookies, which is a second reason not to keep it forever.
RELAY_IDLE_TTL = 30 * 60
#: ... and no more than this many at once, oldest first out, so a browsing
#: session cannot grow unbounded temp files.
RELAY_MAX = 8

#: Bytes read from upstream per turn. Large, because this path is a pipe.
COPY_CHUNK = 128 * 1024
#: How long a reader that outruns the encoder waits for more bytes before the
#: answer is "nothing yet" rather than a short read that looks like an EOF.
GROW_WAIT_SECONDS = 20
GROW_POLL = 0.1

#: A probe of the origin's Range support: `bytes=0-0` asks for one byte and the
#: answer (206 or 200) is the whole of the fact we need.
RANGE_PROBE = 'bytes=0-0'

REFUSED_STATUS = (401, 403, 404, 410)

#: `Content-Type` from what the probe saw. A wrong type here is a TV that refuses
#: to open a stream it would otherwise play, so the value is the one the *source*
#: claimed whenever it claimed one.
CONTENT_TYPES = {
    '.mp4': 'video/mp4',
    '.m4v': 'video/mp4',
    '.mov': 'video/quicktime',
    '.mkv': 'video/x-matroska',
    '.webm': 'video/webm',
    '.flv': 'video/x-flv',
    '.ts': 'video/mp2t',
    '.m3u8': 'application/vnd.apple.mpegurl',
    '.mpd': 'application/dash+xml',
    '.mp3': 'audio/mpeg',
    '.m4a': 'audio/mp4',
    '.aac': 'audio/aac',
    '.ogg': 'audio/ogg',
    '.opus': 'audio/ogg',
    '.wav': 'audio/wav',
    '.flac': 'audio/flac',
}
CONTAINER_TYPES = {
    'mp3': 'audio/mpeg',
    'mp4': 'video/mp4',
    'mov,mp4,m4a,3gp,3g2,mj2': 'video/mp4',
    'matroska,webm': 'video/x-matroska',
    'webm': 'video/webm',
    'mpegts': 'video/mp2t',
    'flv': 'video/x-flv',
    'hls': 'application/vnd.apple.mpegurl',
    'dash': 'application/dash+xml',
}
UNKNOWN_TYPE = 'application/octet-stream'


def content_type_for(url, probe=None):
    """The type to advertise for one address, from the source's own facts first."""
    if probe is not None:
        name = (probe.container or '').lower()
        exact = CONTAINER_TYPES.get(name)
        if exact is not None:
            return exact
        if name:
            # ffprobe can answer `mov,mp4,m4a,3gp,3g2,mj2` or a one-off name; the
            # two most common after the exact match are worth a substring test.
            if 'matroska' in name or 'webm' in name:
                return 'video/x-matroska'
            if 'mp4' in name or 'mov' in name:
                return 'video/mp4'
            if 'mpegts' in name:
                return 'video/mp2t'
    suffix = media_resolve.Candidate(url=url, origin='page').path_suffix
    return CONTENT_TYPES.get(suffix, UNKNOWN_TYPE)


def parse_range(header):
    """`bytes=100-200` -> (100, 200); `bytes=100-` -> (100, None).

    Junk returns None and the caller serves the whole file: RFC 7233 says an
    unsupported or malformed Range is ignored, not answered 416, because 416 is
    what a DLNA renderer reads as a dead file. `(None, n)` is the suffix form --
    the last `n` bytes -- which only a real length can resolve.
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


def total_from_content_range(value):
    """`bytes 0-0/1234567` -> 1234567, or None."""
    match = re.search(r'/\s*(\d+)\s*$', value or '')
    return int(match.group(1)) if match else None


class UpstreamError(Exception):
    """The origin said no. Carries the status so the relay can say which no."""

    def __init__(self, message, status=None):
        Exception.__init__(self, message)
        self.status = status
        self.refused = status in REFUSED_STATUS


def open_upstream(url, headers=None, range_header=None, timeout=30):
    """One GET against the origin, with the captured request headers attached.

    Returns the live response object: the caller reads it, and closing it is the
    caller's business. `redirect` is followed by urllib, which is what a CDN
    signing scheme expects.
    """
    request = urllib.request.Request(url)
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    if range_header:
        request.add_header('Range', range_header)
    try:
        return urllib.request.urlopen(request, timeout=timeout)
    except urllib.error.HTTPError as exc:
        status = getattr(exc, 'code', None)
        raise UpstreamError('源站返回 {}'.format(status), status)
    except (urllib.error.URLError, OSError) as exc:
        raise UpstreamError('无法连接源站：{}'.format(exc))


# ---------------------------------------------------------------------------
# the bookkeeping
# ---------------------------------------------------------------------------

@dataclass
class Relay:
    """One registered stream: what to fetch, how to serve it, and for how long."""
    relay_id: str
    url: str
    headers: dict = field(default_factory=dict)
    mode: str = 'proxy'
    title: str = ''
    content_type: str = UNKNOWN_TYPE
    created: float = field(default_factory=time.time)
    last_seen: float = field(default_factory=time.time)

    # learned facts
    ranges: bool = False
    length: int = None
    probe_error: str = ''

    # remux only
    path: str = ''
    proc: object = None
    watcher: object = None
    state: str = 'open'        # open | running | complete | failed
    error: str = ''

    # DASH hands the picture and the sound as two addresses, so a remux can be
    # fed a second input. It changes how the file is *made* and nothing about how
    # it is served, which is why `mode` still says 'remux' either way.
    audio_url: str = ''
    audio_headers: dict = field(default_factory=dict)

    # The watcher thread and the encoder kill both write `state`; the store's own
    # lock must not be what serialises them, because a reader can hold the request
    # thread for the length of a video. One lock per relay, never nested.
    lock: threading.Lock = field(default_factory=threading.Lock, repr=False)

    def touch(self):
        self.last_seen = time.time()

    @property
    def seekable(self):
        """Whether a viewer can scrub this one. The page shows the answer."""
        return self.ranges and bool(self.length)

    @property
    def alive(self):
        return self.state in ('open', 'running', 'complete')

    def status(self):
        return {
            'id': self.relay_id,
            'title': self.title,
            'mode': self.mode,
            'state': self.state,
            'seekable': self.seekable,
            'ranges': self.ranges,
            'length': self.length,
            'written': self.size(),
            'error': self.error or self.probe_error,
            'seconds': round(time.time() - self.created, 1),
            'idle': round(time.time() - self.last_seen, 1),
        }

    def size(self):
        """Bytes available to read right now (None when nothing established one)."""
        if self.mode == 'remux':
            try:
                return os.path.getsize(self.path)
            except (OSError, TypeError):
                return 0
        return self.length

    def shutdown(self):
        """Stop the encoder and take its temp file with the entry.

        Called after the store has already forgotten this relay, never under the
        store's lock: `proc.wait()` here can take seconds, and the media path
        reads the store on every request.
        """
        proc, self.proc = self.proc, None
        if proc is not None:
            try:
                if proc.poll() is None:
                    proc.kill()
                proc.wait(timeout=5)
            except Exception:
                pass
        path, self.path = self.path, ''
        if path:
            try:
                os.remove(path)
            except OSError:
                logger.warning('relay %s: temp file left behind at %s',
                               self.relay_id, path)


class RelayStore(object):
    """The addresses this server answers for. An unknown one is always 404.

    A relay is found by comparing its id against the ids we issued, in
    `hmac.compare_digest`, rather than by a dict lookup: the id is the only
    credential this path has, and a timing side channel on it would tell an
    attacker who is guessing whether a prefix was right. With 8 entries the loop
    is free, and it is the difference between "hard to guess" and "not leaky".

    Dropping an entry pops it under the lock and shuts it down *after* releasing
    the lock -- killing ffmpeg is not something the read path should wait behind.
    """

    def __init__(self, max_relays=RELAY_MAX, ttl=RELAY_IDLE_TTL):
        self.entries = {}
        self.max_relays = max_relays
        self.ttl = ttl
        self._lock = threading.Lock()
        self._sweeper = None
        self._stop = threading.Event()

    def add(self, relay):
        dropped = []
        with self._lock:
            dropped.extend(self._expire_locked(keep=relay.relay_id))
            while len(self.entries) >= self.max_relays:
                oldest = min(self.entries.values(), key=lambda r: r.last_seen)
                dropped.append(self.entries.pop(oldest.relay_id))
            self.entries[relay.relay_id] = relay
        for gone in dropped:
            gone.shutdown()
        return relay

    def get(self, relay_id):
        with self._lock:
            relay = self._find_locked(relay_id)
            if relay is None:
                return None
            expired = time.time() - relay.last_seen > self.ttl
            if expired:
                self.entries.pop(relay.relay_id, None)
            else:
                relay.touch()
        if expired:
            relay.shutdown()
            return None
        return relay

    def peek(self, relay_id):
        """Like `get` but does not extend the life -- for the status endpoint."""
        with self._lock:
            return self._find_locked(relay_id)

    def drop(self, relay_id):
        with self._lock:
            relay = self.entries.pop(relay_id, None)
        if relay is not None:
            relay.shutdown()
        return relay

    def clear(self):
        with self._lock:
            relays = list(self.entries.values())
            self.entries.clear()
        for relay in relays:
            relay.shutdown()

    def active(self):
        with self._lock:
            return [r.status() for r in sorted(self.entries.values(),
                                               key=lambda r: r.created)]

    def _find_locked(self, relay_id):
        wanted = (relay_id or '').encode('utf-8', 'replace')
        found = None
        for stored in list(self.entries):
            # Every entry is compared, so the answer cannot depend on position.
            if hmac.compare_digest(stored.encode('utf-8'), wanted):
                found = self.entries.get(stored)
        return found

    def _expire_locked(self, keep=None):
        """Drop what has gone idle; hand the callers what to shut down."""
        deadline = time.time() - self.ttl
        dropped = []
        for relay_id in list(self.entries):
            relay = self.entries[relay_id]
            if relay.relay_id == keep:
                continue
            if relay.last_seen < deadline:
                dropped.append(self.entries.pop(relay_id))
        return dropped

    def start_sweeper(self, interval=60):
        """Expire idle relays even while nobody reads the page.

        A relay whose TTL ran out while the app is idle must not leave ffmpeg
        running and a temp file growing: the user's disk is not a scratch pad for
        a session they forgot about.
        """
        if self._sweeper is not None:
            return
        self._stop.clear()

        def loop():
            while not self._stop.wait(interval):
                dropped = []
                with self._lock:
                    dropped.extend(self._expire_locked())
                for relay in dropped:
                    try:
                        relay.shutdown()
                    except Exception:
                        logger.warning('relay %s: cleanup failed',
                                       relay.relay_id, exc_info=True)

        self._sweeper = threading.Thread(target=loop, daemon=True,
                                         name='RELAY_SWEEP')
        self._sweeper.start()

    def stop_sweeper(self):
        self._stop.set()
        self._sweeper = None


def new_id():
    return secrets.token_urlsafe(16)


# ---------------------------------------------------------------------------
# the two bodies
# ---------------------------------------------------------------------------

def probe_proxy(relay):
    """Ask the origin whether it honours Range, and how long the file is.

    This is the fact that decides whether the viewer can seek, and it is only in
    the origin's answer -- nothing about our own request predicts it.
    """
    try:
        response = open_upstream(relay.url, relay.headers,
                                 range_header=RANGE_PROBE, timeout=30)
    except UpstreamError as exc:
        relay.probe_error = str(exc)
        relay.state = 'failed'
        return False
    try:
        status = getattr(response, 'status', 200)
        if status == 206:
            relay.ranges = True
            relay.length = total_from_content_range(
                response.headers.get('Content-Range'))
        else:
            relay.ranges = False
            value = response.headers.get('Content-Length')
            relay.length = int(value) if value and value.isdigit() else None
        if not relay.content_type or relay.content_type == UNKNOWN_TYPE:
            declared = (response.headers.get('Content-Type') or '').split(';')[0].strip()
            if declared and declared != UNKNOWN_TYPE:
                relay.content_type = declared
        relay.state = 'open'
        return True
    finally:
        try:
            response.close()
        except Exception:
            pass


#: An HLS or DASH manifest is a document that names more documents; without the
#: segment protocols ffmpeg will not open it at all.
PROTOCOL_WHITELIST = 'file,http,https,tcp,tls,crypto,httpproxy,data'


def input_arguments(url, headers=None):
    """ffmpeg's read side for one address, options in front of its own `-i`."""
    args = []
    if headers:
        args += ['-headers', media_resolve.header_field(headers)]
    return args + ['-protocol_whitelist', PROTOCOL_WHITELIST, '-i', url]


def start_remux(relay, ffmpeg=None):
    """Copy the manifest's streams into one growing MP4 and hand it to the store.

    `-c copy`: the point is a container a device can walk with byte offsets, not a
    re-encode. If the source needs re-encoding (an HEVC stream on an old TV), that
    is the *renderer's* problem and the renderer plugin that owns transcoding is
    `cast_local_file`, not this module.

    A DASH ladder hands the picture and the sound as two addresses, so this may be
    called with two inputs and it still produces one file. The per-input options
    sit before their own `-i` because that is where ffmpeg reads them from -- one
    `-headers` for the whole command would leave the second input unauthenticated,
    which on these origins means "403", and a merged cast that fails this way fails
    silently: ffmpeg exits, `watch_remux` says so, and the reason would be a header
    nobody sent.
    """
    ffmpeg = ffmpeg or media_resolve.find_command('ffmpeg')
    if ffmpeg is None:
        relay.state = 'failed'
        relay.error = '这台机器没有 ffmpeg，无法把清单流转封装'
        return False
    relay.path = os.path.join(relay_dir(), relay.relay_id + '.mp4')
    command = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y']
    command += input_arguments(relay.url, relay.headers)
    if relay.audio_url:
        command += input_arguments(relay.audio_url, relay.audio_headers)
    command += ['-c', 'copy', '-movflags', '+faststart', '-f', 'mp4', relay.path]
    try:
        relay.proc = subprocess.Popen(
            command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE, text=True, encoding='utf-8', errors='replace',
            creationflags=media_resolve.console_flags())
    except OSError as exc:
        relay.state = 'failed'
        relay.error = '无法启动 ffmpeg：{}'.format(exc)
        return False
    relay.state = 'running'
    relay.length = None
    relay.ranges = False
    # What leaves this process is now an MP4 whatever the source's address looked
    # like, and that is the type the handler answers with, the Cast LOAD reports,
    # and the DIDL puts in `protocolInfo`. A manifest URL has no useful suffix to
    # map, so without this a remuxed cast advertises `application/octet-stream` --
    # which a television reads as "not a media file" and a Chromecast reads as a
    # reason to guess a handler (see `CastSender.load`).
    relay.content_type = 'video/mp4'
    watch_remux(relay)
    return True


def watch_remux(relay):
    """Finish the growing file: the length becomes real when ffmpeg exits 0.

    The exit code decides whether seeking is offered, so this thread is not a
    courtesy -- and its stderr has to be drained or a long job blocks on a full
    pipe and never exits at all.
    """
    def finish():
        proc = relay.proc
        stderr = ''
        try:
            stderr = _drain(proc)
            code = proc.wait()
        except Exception as exc:
            code = -1
            stderr = stderr or str(exc)
        with relay.lock:
            if relay.state == 'failed':
                # Killed on purpose (evicted, expired, or the app stopped). The
                # entry is already gone; saying so to nobody would be noise.
                return
            if code == 0 and relay.size():
                relay.state = 'complete'
                relay.length = relay.size()
                relay.ranges = True
            else:
                relay.state = 'failed'
                relay.ranges = False
                relay.length = None
                relay.error = '转封装没有产出可用的文件（ffmpeg 退出码 {}{}）'.format(
                    code, '：' + _last_line(stderr) if stderr else '')

    thread = threading.Thread(target=finish, daemon=True, name='RELAY_REMUX')
    relay.watcher = thread
    thread.start()


def _drain(proc):
    """Read ffmpeg's stderr to EOF so it never blocks writing to it."""
    if proc is None or proc.stderr is None:
        return ''
    text = []
    try:
        for line in proc.stderr:
            text.append(line)
    except (OSError, ValueError):
        pass
    return ''.join(text)


def _last_line(text):
    for line in reversed((text or '').splitlines()):
        if line.strip():
            return line.strip()[:200]
    return ''


def relay_dir():
    path = os.path.join(SETTING_DIR, 'relay')
    os.makedirs(path, exist_ok=True)
    return path


def sweep_orphans(max_age=RELAY_IDLE_TTL, now=None):
    """Drop remux temp files nobody is watching any more.

    A clean shutdown empties the directory (`stop_server` -> `store.clear`), but
    §4.2 records that a signal-killed Macast skips its cleanup -- and a live HLS
    remux is an ffmpeg that never ends, so its temp file would grow across every
    later session unnoticed. Only files older than the idle TTL go: a *second*
    running instance keeps its own file freshly written, so this cannot reach into
    a stream somebody is watching (multi-instance is normal on this LAN).

    Returns the paths removed, so the caller can log it and the suite can assert
    the age rule rather than the accident.
    """
    now = time.time() if now is None else now
    removed = []
    try:
        names = os.listdir(relay_dir())
    except OSError:
        return removed
    for name in names:
        if not name.endswith('.mp4'):
            continue
        if name[:-4] in store.entries:
            continue
        path = os.path.join(SETTING_DIR, 'relay', name)
        try:
            if now - os.path.getmtime(path) < max_age:
                continue
            os.remove(path)
        except OSError:
            continue
        removed.append(path)
    if removed:
        logger.info('relay: swept %d abandoned temp file(s)', len(removed))
    return removed


# ---------------------------------------------------------------------------
# the answer to one request, as a decision anyone can test
# ---------------------------------------------------------------------------

@dataclass
class ServePlan:
    """What HTTP to make of one read request.

    Split out of the handler because every one of these numbers is a contract a
    renderer reads, and a contract you cannot call is a contract you can only
    assert by opening a socket -- which is how the "whole-file read also answered
    206" bug survived in this repo once already.
    """
    status: int
    start: int
    stop: int          # None = to the end
    total: int         # None = the origin never told us
    partial: bool
    length: int        # the Content-Length to advertise, None = advertise none

    @property
    def remaining(self):
        """Bytes to write, None = until the source ends."""
        if self.stop is None:
            return None
        return max(0, self.stop - self.start + 1)


def serve_plan(relay, range_header):
    """Decide the answer for `range_header` against one relay's known facts.

    `relay.length` and `relay.ranges` are two different facts and only the second
    one makes seeking possible: an origin that declares 40 MB and ignores `Range`
    still deserves a `Content-Length`, because that is what lets a DLNA renderer
    show a duration at all. Answering 206 to such an origin would be the lie --
    the bytes we hand over are the whole file either way.
    """
    asked = bool(range_header)
    parsed = parse_range(range_header)
    if parsed is None:
        # Junk means "no usable Range", which is the same answer as the suffix
        # form with nothing to resolve it against: serve the whole file.
        start = stop = None
    else:
        start, stop = parsed
    total = relay.length if relay.ranges else None
    if total is None:
        if relay.ranges:
            # The origin honours Range but never told us how long it is (a 206
            # with no parseable `Content-Range`): stream from the beginning with
            # no length. An estimate here would be a lie the player seeks by.
            return ServePlan(200, 0, None, None, False, None)
        declared = relay.length
        if declared is None:
            return ServePlan(200, 0, None, None, False, None)
        # Whole file, real length, ignoring any Range the reader asked for. The
        # stop is still filled in so the write is bounded by the Content-Length
        # we just advertised rather than by however long the origin talks.
        return ServePlan(200, 0, declared - 1, declared, False, declared)
    if start is None and stop is None:
        # Junk Range: ignored, whole file (RFC 7233, and Part 25's table).
        return ServePlan(200, 0, total - 1, total, False, total)
    if start is None:
        start = max(0, total - (stop or 0))
        stop = None
    elif stop is not None:
        stop = min(stop, total - 1)
    if start >= total:
        return ServePlan(416, start, None, total, False, None)
    if not asked:
        return ServePlan(200, 0, total - 1, total, False, total)
    end = total - 1 if stop is None else stop
    return ServePlan(206, start, end, total, True, end - start + 1)


# ---------------------------------------------------------------------------
# serving
# ---------------------------------------------------------------------------

class RelayHandler(BaseHTTPRequestHandler):
    """Range/206 serving for exactly the ids `RelayStore` knows.

    `protocol_version` is HTTP/1.0 on purpose, the same choice the local-file
    caster makes: a close-delimited body is what older renderers handle, and it
    removes any promise of keep-alive we would then have to honour on a stream
    that ends when the encoder ends.
    """

    protocol_version = 'HTTP/1.0'
    server_version = 'MacastRelay'

    @property
    def store(self):
        return self.server.relay_store

    def log_message(self, fmt, *args):
        logger.debug('relay: ' + (fmt % args))

    def _relay_from_path(self):
        path = self.path.partition('?')[0]
        if not path.startswith(RELAY_PREFIX):
            return None, None
        parts = [p for p in path[len(RELAY_PREFIX):].split('/') if p]
        if len(parts) != 2:
            return None, None
        relay_id, tail = parts
        if tail not in (MEDIA_TAIL, STATUS_TAIL):
            return None, None
        # `peek` for status: polling the page must not keep a relay alive forever.
        if tail == STATUS_TAIL:
            return self.store.peek(relay_id), tail
        return self.store.get(relay_id), tail

    def do_HEAD(self):
        relay, tail = self._relay_from_path()
        if tail == STATUS_TAIL:
            # The status document is a GET-only JSON read; no body to head.
            self._refuse(405)
            return
        if relay is None:
            self._refuse(404)
            return
        self._respond(relay, head_only=True)

    def do_GET(self):
        relay, tail = self._relay_from_path()
        if relay is None:
            self._refuse(404)
            return
        if tail == STATUS_TAIL:
            self._send_json(relay.status())
            return
        self._respond(relay)

    def do_POST(self):
        # Nothing here is a control endpoint: a relay is started by the management
        # API (which is gated), and the only thing this port takes is a read.
        self._refuse(405)

    def _send_json(self, payload):
        body = json.dumps(payload, ensure_ascii=False).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'application/json;charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self._write(body)

    def _write(self, body):
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass

    def _refuse(self, status):
        self.send_response(status)
        self.send_header('Content-Type', 'text/plain;charset=utf-8')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self._write(b'not here')

    def _respond(self, relay, head_only=False):
        if relay.state == 'failed':
            self._refuse(502)
            return
        if relay.mode == 'remux' and not relay.path:
            # Registered but with no bytes to hand over: refuse rather than open a
            # lengthless 200 the player would wait on forever.
            self._refuse(410)
            return
        plan = serve_plan(relay, self.headers.get('Range'))
        if plan.status == 416:
            self.send_response(416)
            self.send_header('Content-Range', 'bytes */{}'.format(plan.total))
            self.send_header('Accept-Ranges', 'bytes')
            self.end_headers()
            return
        self.send_response(plan.status)
        self.send_header('Content-Type', relay.content_type)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Accept-Ranges', 'bytes' if relay.seekable else 'none')
        if plan.length is not None:
            self.send_header('Content-Length', str(plan.length))
        if plan.partial:
            self.send_header('Content-Range',
                             'bytes {}-{}/{}'.format(plan.start, plan.stop,
                                                     plan.total))
        self.end_headers()
        if head_only:
            return
        try:
            self._pipe(relay, plan.start, plan.remaining)
        except (BrokenPipeError, ConnectionResetError, OSError):
            # The viewer left. That is normal (a TV stops fetching when the user
            # pauses); it is not our error and must not close the relay.
            pass

    def _pipe(self, relay, start, remaining):
        if relay.mode == 'remux':
            self._pipe_file(relay, start, remaining)
        else:
            self._pipe_upstream(relay, start, remaining)

    def _pipe_upstream(self, relay, start, remaining):
        range_header = None
        if relay.ranges and (start or remaining is not None):
            end = '' if remaining is None else (start + remaining - 1)
            range_header = 'bytes={}-{}'.format(start, end)
        response = open_upstream(relay.url, relay.headers,
                                 range_header=range_header)
        try:
            sent = 0
            while remaining is None or sent < remaining:
                size = COPY_CHUNK if remaining is None else min(
                    COPY_CHUNK, remaining - sent)
                block = response.read(size)
                if not block:
                    break
                self.wfile.write(block)
                sent += len(block)
        finally:
            try:
                response.close()
            except Exception:
                pass

    def _pipe_file(self, relay, start, remaining):
        sent = 0
        waited = 0.0
        with open(relay.path, 'rb') as handle:
            handle.seek(start)
            while remaining is None or sent < remaining:
                size = COPY_CHUNK if remaining is None else min(
                    COPY_CHUNK, remaining - sent)
                block = handle.read(size)
                if not block:
                    if relay.state == 'complete':
                        break
                    if waited >= GROW_WAIT_SECONDS:
                        break
                    # Ahead of the encoder: *wait*, rather than answer short. A
                    # short read is an EOF to every player that reads it.
                    time.sleep(GROW_POLL)
                    waited += GROW_POLL
                    continue
                waited = 0.0
                self.wfile.write(block)
                sent += len(block)


# ---------------------------------------------------------------------------
# the server
# ---------------------------------------------------------------------------

_server = None
_server_thread = None
_server_lock = threading.Lock()
store = RelayStore()


def ensure_server(port=0):
    """Start the relay listener if it is not up; return (host, port).

    Binding `0.0.0.0` with port 0 is what the two sender plugins do: the port is
    only ever published inside the media URL we hand out, so nothing needs to
    guess it, and a fixed port would collide with them.
    """
    global _server, _server_thread
    with _server_lock:
        if _server is not None:
            return _server.server_address[0], _server.server_address[1]
        _server = ThreadingHTTPServer(('0.0.0.0', port), RelayHandler)
        _server.relay_store = store
        _server.daemon_threads = True
        sweep_orphans()
        _server_thread = threading.Thread(target=_server.serve_forever,
                                          daemon=True, name='RELAY_HTTP')
        _server_thread.start()
        store.start_sweeper()
        return _server.server_address[0], _server.server_address[1]


def server_port():
    return _server.server_address[1] if _server is not None else None


def stop_server():
    """Close the listener and drop every relay. Called when the service stops."""
    global _server, _server_thread
    with _server_lock:
        store.stop_sweeper()
        store.clear()
        if _server is not None:
            try:
                _server.shutdown()
                _server.server_close()
            except OSError:
                pass
        _server = None
        _server_thread = None


def advertise_host():
    """The address a device on this LAN can reach us on.

    The second reason this is not `Setting.get_ip()`: that returns `(addr,
    netmask)` pairs, and `Setting.get_advertisable_ip()` keeps every interface
    with a gateway entry -- VM bridges and the Tailscale address included, in
    set-iteration order, so its first element is not the Wi-Fi address a phone
    can dial. `discovery.advertisable_addresses()` is what mDNS publishes (the
    interface carrying the IPv4 default route), so asking it is what keeps the
    URL we hand out openable rather than merely printable.

    `cast_local_file.advertise_host()` in the plugin layer is the same function
    for the same reason; core may not import a plugin, so the choice lives twice
    and Part 59 pins the two copies against one stubbed interface table rather
    than letting them drift.
    """
    addrs = _advertisable_hosts()
    if not addrs:
        return '127.0.0.1'
    reachable = [a for a in addrs if a in set(_reachable_hosts())]
    return (reachable or addrs)[0]


def _advertisable_hosts():
    """Every address this machine could announce, or [] when it cannot say.

    Two one-line seams rather than inline calls, and for the same reason the
    plugin has `_reachable_hosts()`: the suite has to be able to hand back a
    four-row table of machine states -- two interfaces where only the second is
    routed, nothing routed, no address at all -- without pretending this machine
    owns a second network card.
    """
    try:
        return list(Setting.get_advertisable_ip())
    except Exception:
        return []


def _reachable_hosts():
    """What discovery would publish, or [] when the core cannot say."""
    try:
        from macast.discovery import advertisable_addresses
        return list(advertisable_addresses())
    except Exception:  # pragma: no cover - discovery ships with the app
        return []


def media_url(relay, host=None):
    host = host or advertise_host()
    _, port = ensure_server()
    return 'http://{}:{}{}{}/{}'.format(host, port, RELAY_PREFIX,
                                        relay.relay_id, MEDIA_TAIL)


def status_url(relay, host=None):
    """The JSON sibling of `media_url`, for the page that watches progress."""
    host = host or advertise_host()
    _, port = ensure_server()
    return 'http://{}:{}{}{}/{}'.format(host, port, RELAY_PREFIX,
                                        relay.relay_id, STATUS_TAIL)


def open_relay(candidate, title=''):
    """Register one resolved candidate and return the relay that serves it.

    The probe/planning work happens here, synchronously, because the caller is a
    management POST on a CherryPy worker -- which is exactly where a short probe
    belongs -- and because the answer ("this origin refuses you") has to reach the
    page in the same response.
    """
    plan = media_resolve.plan(candidate)
    relay = Relay(relay_id=new_id(), url=candidate.url,
                  headers=dict(candidate.headers),
                  # 'merge' is one of remux's shapes, not a third way to serve
                  # bytes: one growing file, two inputs. Keeping the word out of
                  # `mode` is what lets `size()`, `serve_plan` and `_pipe_file`
                  # answer for a merged cast without ever learning where the sound
                  # came from.
                  mode='remux' if plan == 'merge' else plan,
                  audio_url=candidate.audio_url,
                  audio_headers=dict(candidate.audio_headers),
                  title=title or candidate.label or candidate.host,
                  content_type=content_type_for(candidate.url, candidate.probe))
    store.add(relay)
    if relay.mode == 'remux':
        start_remux(relay)
    else:
        probe_proxy(relay)
    return relay
