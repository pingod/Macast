# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Nothing in this file came from xfangfang/Macast: it is written in this fork,
# which is why it carries no "Derived from" line (docs/Provenance.md, §0).
"""Macast 投屏控制台 —— 屏幕镜像的桌面控制窗口.

Why this file exists
--------------------
`screen_mirror` used to be driven from the menu bar: about fifty rows, nested
four deep, where「输出目标」「兼容档位」「系统声音」all fought for the same
160-pixel column and the one thing you actually wanted to see -- is it working,
what does the TV see -- had no room at all. This window is the whole control
surface, and the menu bar keeps one item that opens it.

Why a *separate process*
------------------------
`App.start()` owns the main thread on every platform (rumps runs the Cocoa
loop, pystray its own) and Tk will not pump from anywhere else. So Macast
spawns this file with `MACAST_CONSOLE_API` / `_TOKEN` / `_DIR` in the
environment and the two talk over the management API, which is also what makes
the window able to run under *any* python that carries Tk 8.6 or newer -- it
imports nothing from `macast`.

Consequences worth knowing while editing
----------------------------------------
* the plugin's `console_state()` / `console_action()` are the only surface, so
  anything the window can show must be in that dict. `sections_for()` below
  decides what is *laid out*, which keeps the arrangement testable without a
  display;
* every control that changes a setting POSTs and then re-reads state: the
  watchdog, the encoder dying, and the other possible console all mutate the
  same truth, so an optimistic UI would drift;
* the preview is a snapshot, not video -- see `snapshot_frame` in the plugin.
  It is polled at ~1 fps and the last good frame stays up through a failure;
* Tk 8.5 -- still what Apple's Command Line Tools ship as `/usr/bin/python3` --
  paints nothing but native widgets on a current macOS, so `MIN_TK` and the
  launcher's probe both refuse it rather than open an empty window. The frame is
  written to a temp file and loaded by name anyway: no Tk generation reads PPM
  from a string, and `preview_format()` negotiates which format to ask for;
* colours are set on plain widgets rather than `tk.Button`, because the macOS
  Aqua theme silently ignores `bg=` on themed widgets and every other platform
  would then look like this one. Labels-with-bindings are the only way one
  palette covers three platforms.
"""

import hashlib
import hmac
import json
import math
import os
import secrets
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

#: Bumped when the state/action contract changes. The app compares it with its
#: own `CONSOLE_VERSION`, so an old window against a new app says so instead of
#: quietly losing buttons. v2 is the protocol-first view model: `channels`
#: replaced `search`, and `requirements` appeared.
PROTOCOL_VERSION = 2

API_BASE = os.environ.get('MACAST_CONSOLE_API', 'http://127.0.0.1:58880')
API_TOKEN = os.environ.get('MACAST_CONSOLE_TOKEN', '')
SETTING_DIR = os.environ.get('MACAST_CONSOLE_DIR', '')
LOCK_NAME = 'mirror_console.lock'
#: How often the window re-reads the plugin's state, and the preview. The state
#: poll is what keeps a DLNA watchdog's profile switch visible; 1 s costs one
#: loopback request and keeps the elapsed-time counter alive.
POLL_MS = 1000
PREVIEW_MS = 1000
#: The native width of the frame the app sends (SNAPSHOT_WIDTH in the plugin);
#: only used to decide how much to subsample it down into this column.
FRAME_WIDTH = 480
SNAPSHOT_URL = '/api?query=mirror-snapshot&fmt={fmt}&token={token}'

# -- 外观：与插件自带网页同一套深色色板 ----------------------------------------
#
# The browser viewer page and the BlackHole progress page already look like
# this. One palette across all of them is why the window reads as part of the
# product rather than a dialog that appeared.
BG = '#141821'
PANEL = '#1b2130'
PANEL_RAISED = '#222b3d'
PANEL_SUNKEN = '#0d1017'
BORDER = '#2a3446'
BORDER_STRONG = '#3d4a63'
TEXT = '#e8ecf3'
MUTED = '#93a0b5'
DIM = '#5b6779'
ACCENT = '#3b82f6'
ACCENT_HOVER = '#2f8ef7'
ACCENT_BG = '#16233a'
GOOD = '#22c55e'
BAD = '#ef4444'
WARN = '#f59e0b'
WINDOW_SIZE = '1080x780'
WINDOW_MIN = (940, 620)
#: What a screen keeps for itself: the menu bar, the Dock, and the gap the
#: window manager leaves when it places a window. `window_size_for` takes this
#: off the screen height before deciding how tall the console may be.
SCREEN_RESERVE = 110
#: Chrome between a column's canvas edge and the text inside one of its panels:
#: panel margin 10 + border 1 + panel padding 14, mirrored. Measured, not
#: guessed -- 593 - 50 = the 543 px a left-column label is given.
COLUMN_INSET = 50
#: A protocol or requirement row is a card inside the panel, so its labels are
#: inset by the card's own `padx=10` on both sides on top of the column inset.
#: Added to every row label's `wraplength` slack -- the wrap is measured from
#: `_text_width`, which knows about the column but not about the card.
ROW_INSET = 20
#: Card names to fall back to when the app answers without a catalog -- which is
#: what it does while the app is still starting, or with a plugin that failed to
#: load. Without these the rows read「cast」/「dlna」, which is a key name, not a
#: label. Part 35 pins each one against the plugin's own ``OUTPUTS``, so this
#: cannot drift.
OUTPUT_FALLBACK = {
    'cast': 'Chromecast / Google TV',
    'caststream': 'Chromecast 低延迟（实验 · 无声音）',
    'dlna': 'DLNA 电视（老电视，MPEG-PS）',
    'browser': '浏览器（打开网址即可看）',
}
#: What a protocol with no devices to find is asked for. The browser target has
#: nothing to search, and「没有发现」about it would be a lie -- its answer is the
#: address the window shows once a mirror runs.
NO_PROBE_WORDS = '不需要设备'

#: A step's note shares its row with the mark and the step name, and the longest
#: name the plugin reports is「获取官方安装包信息」-- 9 glyphs plus the 2-char
#: status mark and its padding. Anything shorter leaves the note extra room.
STEP_TEXT_INSET = 170
#: A label asks for `wraplength` plus a few pixels of slack, so a wrap derived
#: from a sibling's width has to leave that much out or the text clips by a
#: handful of pixels -- which is how the cards looked truncated at 1080 wide.
LABEL_SLACK = 8
#: Narrower than this and a CJK line holds two glyphs; the window may shrink to
#: ``WINDOW_MIN`` but the text should still be readable.
MIN_WRAP = 60


# -- 视图：哪些区块该出现（不依赖 Tk，可脱离显示器测试）--------------------------

SECTION_ORDER = ('channels', 'devices', 'requirements', 'profiles', 'quality',
                 'capture', 'audio', 'viewer', 'preview', 'activity')


def window_size_for(screen_height, size=WINDOW_SIZE, reserve=SCREEN_RESERVE):
    """The size this screen can actually show.

    1080x780 is what the console wants so the left column ends on a panel edge
    instead of mid-line; a 13-inch laptop is 800 points tall and would be given
    a window taller than the desktop, whose bottom half the user could never
    reach. So the height gives way to the screen -- and never below
    ``WINDOW_MIN``, because a window shorter than that hides the「开始镜像」
    button behind a scrollbar.
    """
    width, wanted = (int(part) for part in size.split('x'))
    if screen_height <= 0:                 # no display answered: ask as written
        return size
    return '{}x{}'.format(width, max(WINDOW_MIN[1],
                                     min(wanted, screen_height - reserve)))


def sections_for(state):
    """Which panels this state warrants, in layout order.

    Not「everything, greyed out」: a Windows machine has no BlackHole to
    install and an old TV target has no compatibility profiles to pick when the
    output is a browser, and rows that can never be touched make the window
    look broken rather than complete.

    「投屏方式」is always there: it is the answer to「这台机器能往哪几种协议投」,
    which is a question with an honest reply even when every search came back
    empty. 「设备」belongs to whichever protocol is chosen, so it appears only
    for the two that have devices to find; 「需要安装」only while something is
    actually outstanding, because that is when it is the whole point.
    """
    kind = (state.get('output') or {}).get('kind', 'cast')
    channel = current_channel(state)
    sections = ['channels']
    if channel.get('needs_device') or kind in ('cast', 'caststream', 'dlna'):
        sections.append('devices')
    if state.get('requirements'):
        sections.append('requirements')
    if kind == 'dlna':
        sections.append('profiles')
    sections.append('quality')
    sections.append('capture')
    if state.get('platform') == 'darwin' or (state.get('audio') or {}).get('line'):
        sections.append('audio')
    if kind == 'browser':
        sections.append('viewer')
    sections.append('preview')
    sections.append('activity')
    return sections


def current_channel(state):
    """The channel row for the protocol being mirrored to, or `{}`.

    The device list, the「还要选一台」prompt and the compatibility profiles all
    hang off whichever protocol is chosen, so they read from this row rather
    than from a single list that silently changes meaning with the target.
    """
    kind = (state.get('output') or {}).get('kind')
    for row in state.get('channels') or []:
        if row.get('key') == kind:
            return row
    return {}


def channel_rows(channels):
    """[(key, label, words, selected, needs_device, choice, trade-off)].

    `words` is what that protocol's last search actually concluded -- per
    protocol, which is the point of the panel: an empty Chromecast answer says
    nothing about the DLNA side of the same LAN. `choice` is the device the
    stored settings would actually use, and the trade-off line is what choosing
    this protocol costs, which is the other half of the decision.
    """
    out = []
    for row in channels or []:
        needs = bool(row.get('needs_device'))
        out.append((row.get('key', ''),
                    row.get('label') or OUTPUT_FALLBACK.get(row.get('key'), ''),
                    row.get('words') or ('' if needs else NO_PROBE_WORDS),
                    bool(row.get('selected')), needs,
                    row.get('choice') or '',
                    row.get('hint') or ''))
    return out


#: Apple's Command Line Tools still ship Tcl/Tk 8.5.9, and on a current macOS
#: that build paints nothing except native buttons: the window opens, every
#: Label / Frame / Entry / Canvas text stays invisible, and no error is raised
#: anywhere. Measured 2026-09-22 with /usr/bin/python3 both from a CLI and
#: through Launch Services, so it is not a sandbox artifact.
MIN_TK = (8, 6)


def tk_version_of(tk_module):
    """(major, minor) of a tkinter module's Tk, or None when unreadable."""
    try:
        return tuple(int(part) for part in
                     str(tk_module.TkVersion).split('.')[:2])
    except (AttributeError, ValueError):
        return None


def tk_ok(tk_module):
    """This Tk version when the interpreter may draw the window, else None."""
    version = tk_version_of(tk_module)
    return version if version and version >= MIN_TK else None


def preview_format(tk_version):
    """'png' or 'ppm' for this Tk.

    Tk 8.5 has no PNG decoder. The launcher refuses to run under it (§ MIN_TK),
    but a console started by hand still has to pick the one raster format every
    Tk reads, so the choice is made here rather than guessed by the plugin.
    """
    try:
        major, minor = str(tk_version).split('.')[:2]
        if (int(major), int(minor)) >= (8, 6):
            return 'png'
    except (ValueError, AttributeError):
        pass
    return 'ppm'


def frame_ok(raw, fmt):
    """Is this really the image format we asked for?

    Tk reads frames from a *file*: 8.5's `PhotoImage(data=)` refuses even a valid
    PPM ("couldn't recognize image data"), so one code path -- write, load by
    name -- covers both Tk generations. Checking the magic bytes first is what
    keeps a truncated response, or the JSON error the app answers with when it
    has no frame, from raising inside Tk with a message nobody can act on.
    """
    magic = b'\x89PNG' if fmt == 'png' else b'P6'
    return bool(raw) and raw.startswith(magic)


def frame_suffix(fmt):
    return '.png' if fmt == 'png' else '.ppm'


def fit_factor(image_width, available_width):
    """How many pixels to drop per pixel when a frame meets this column.

    Subsampled rather than resized because Tk's only resize is subsampling --
    and a 480-wide desktop frame shown in a 300-pixel column loses detail
    either way, while a frame that pushes the window wider loses the layout.
    """
    if image_width <= 0 or available_width <= 0:
        return 1
    return max(1, int(math.ceil(image_width / float(available_width))))


def format_stats(state):
    """One line of live numbers for the header, or '' before there are any.

    The plugin already has `_status_line` for the menu; reusing its text keeps
    the two from ever disagreeing about what a mirror is doing.
    """
    line = state.get('status_line') or ''
    if line:
        return line
    if state.get('starting'):
        return '正在开始镜像…'
    return '未镜像'


def device_rows(channel):
    """[(key, label, selected, name)] for one protocol's devices -- [] if none.

    The key is what `console_action` needs back, so it is carried here instead
    of being reconstructed from the label (the label has a ` · ` in it). `name`
    travels too: the plugin records targets by name, not by display string.
    """
    return [(item.get('id', ''), item.get('label', ''),
             bool(item.get('selected')), item.get('name', ''))
            for item in (channel or {}).get('devices') or []]


def search_note(channel):
    """The line under the device list: what this protocol's search concluded.

    Written by the plugin (`_search_words`), because the wording has to match
    the one a refused start reports -- two phrasings drift, and the user is left
    deciding which「没有发现」was meant.
    """
    return (channel or {}).get('words') or ''


def quality_text(quality):
    """({key: pill text}, note line) for a quality block.

    Four pills share one row, so each can only carry its resolution -- and the
    bandwidth cost is exactly what the choice turns on, so the chosen preset's
    full label goes on the line below rather than being dropped with it. The
    plugin's own note wins when it has one: it is situational (the low-latency
    channel writing「上限 4.5 Mbps」over a 10 Mbps preset is precisely the case
    where quoting the preset's number would mislead).
    """
    labels = {item.get('key'): item.get('label', '')
              for item in quality.get('options') or []}
    short = {key: (text.split('·')[0].strip() or text)
             for key, text in labels.items()}
    note = quality.get('note') or labels.get(quality.get('current')) or ''
    return short, note


def audio_step_mark(step):
    """A state -> glyph pair for one assisted-install step."""
    state = step.get('state')
    return {'running': ('▶', ACCENT), 'done': ('✓', GOOD),
            'fail': ('✗', BAD), 'skipped': ('—', DIM),
            'pending': ('○', DIM)}.get(state, ('?', MUTED))


def banner_for(state, error):
    """What the strip at the bottom has to say, newest problem first.

    Order is deliberate: no connection beats a version mismatch beats a missing
    plugin beats an unprobed machine, because each one earlier explains why the
    ones after it are not worth acting on yet.
    """
    if error:
        return '连不上 Macast（{}）：应用可能已经退出'.format(error), BAD
    remote = state.get('console_version')
    if remote is not None and remote != PROTOCOL_VERSION:
        return ('本窗口按 v{} 的接口写的，Macast 是 v{}：请重启 Macast 或改用'
                '它自带的控制台文件').format(PROTOCOL_VERSION, remote), WARN
    if not state.get('available'):
        return ('没有可用的电脑投屏插件：在设置页的「插件」里启用 Screen Mirror'), WARN
    if not (state.get('capture') or {}).get('probed'):
        return '正在探测这台电脑的采集设备…（首次打开需要一两秒）', MUTED
    return '', None


# -- 与 Macast 主进程说话 ------------------------------------------------------

def _opener():
    """An opener that ignores `http_proxy`.

    This machine's proxy settings must not intercept a loopback request: mpv
    already taught this app that lesson (AGENTS 4.1), and a console that could
    not reach its own app would look like the app had died.
    """
    return urllib.request.build_opener(urllib.request.ProxyHandler({}))


class Api:
    """The management API, with the token on every call.

    The token is required rather than merely sufficient: these endpoints start
    a capture of the user's desktop, and loopback alone does not prove that a
    request came from this window (any page in any browser can hit 127.0.0.1).
    """

    def __init__(self, base=API_BASE, token=API_TOKEN):
        self.base = base.rstrip('/')
        self.token = token
        self._opener = _opener()

    def _request(self, url, data=None, headers=None):
        request = urllib.request.Request(url, data=data)
        request.add_header('X-Macast-Token', self.token)
        for key, value in (headers or {}).items():
            request.add_header(key, value)
        with self._opener.open(request, timeout=6) as response:
            return response.read(), response.headers.get('Content-Type', '')

    def state(self):
        payload, _ = self._request(
            '{}/api?query=mirror-state&token={}'.format(
                self.base, urllib.parse.quote(self.token, safe='')))
        body = json.loads(payload.decode('utf-8', 'replace'))
        if not isinstance(body, dict):
            raise ValueError('mirror-state 返回了意外的内容')
        return body

    def action(self, name, args=None):
        form = {'mirror-action': name,
                'mirror-args': json.dumps(args or {}, ensure_ascii=False)}
        data = urllib.parse.urlencode(form).encode()
        payload, _ = self._request(
            '{}/api'.format(self.base), data=data,
            headers={'Content-Type': 'application/x-www-form-urlencoded'})
        body = json.loads(payload.decode('utf-8', 'replace'))
        return body if isinstance(body, dict) else {'code': 1, 'message': ''}

    def snapshot(self, fmt='png'):
        """One preview frame, or b'' when the app answered with a reason.

        Content type is the answer: 'no frame yet' and 'no ffmpeg' are ordinary
        states of a window that polls, not errors to show the user every second.
        """
        payload, content_type = self._request(
            self.base + SNAPSHOT_URL.format(
                fmt=fmt, token=urllib.parse.quote(self.token, safe='')))
        return payload if content_type.startswith('image/') else b''


# -- 单实例：锁文件 + /ping + /raise -------------------------------------------

class _LoopbackHandler(BaseHTTPRequestHandler):
    """`/ping` proves this window is alive, `/raise` asks it to come forward.

    Liveness has to be an HTTP question, not `os.kill(pid, 0)`: on Windows that
    signal *terminates* the process, so the check for "is the console open"
    would be the thing that closed it.
    """

    server_version = 'MacastMirrorConsole/1'

    def log_message(self, *args):
        pass

    def _send(self, code, body=b'', content_type='application/json'):
        self.send_response(code)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):
        if self.path.split('?')[0] == '/ping':
            self._send(200, json.dumps({'ok': True,
                                        'version': PROTOCOL_VERSION}).encode())
        else:
            self._send(404)

    def do_POST(self):
        if self.path.split('?')[0] != '/raise':
            self._send(404)
            return
        # The token comes from the lock file, so only a process that can read
        # the user's settings dir can raise the window. Without it a stranger
        # on the LAN could not do much -- but it would be rude.
        want = getattr(self.server, 'raise_token', '')
        got = self.headers.get('X-Macast-Console-Token') or ''
        if not got or not hmac.compare_digest(str(want), str(got)):
            self._send(403)
            return
        self.server.on_raise()
        self._send(200, json.dumps({'ok': True}).encode())


def _lock_path():
    return os.path.join(SETTING_DIR or os.path.expanduser('~'), LOCK_NAME)


class ConsoleLock:
    """Advertise this window, or discover that another one already exists."""

    def __init__(self, path=None):
        self.path = path or _lock_path()
        self.server = None
        self.thread = None
        self.token = secrets.token_hex(8)

    def existing(self):
        try:
            with open(self.path) as handle:
                info = json.loads(handle.read())
        except Exception:
            return None
        if not isinstance(info, dict) or not info.get('port'):
            return None
        try:
            request = urllib.request.Request(
                'http://127.0.0.1:{}/ping'.format(info['port']))
            with _opener().open(request, timeout=1.5) as response:
                json.loads(response.read().decode('utf-8', 'replace'))
        except Exception:
            self.remove()
            return None
        return info

    def acquire(self, on_raise):
        """Bind to a random loopback port and write the lock. False if busy.

        The bind is the lock: two windows racing to start would both see no
        lock file, so whoever cannot take the port is the loser and exits --
        which is also why a killed window's stale file is simply overwritten.
        """
        try:
            server = ThreadingHTTPServer(('127.0.0.1', 0), _LoopbackHandler)
        except OSError:
            return False
        server.raise_token = self.token
        server.on_raise = on_raise
        server.daemon_threads = True
        server.block_on_close = False
        self.server = server
        self.thread = threading.Thread(target=server.serve_forever, daemon=True,
                                       name='MIRROR_CONSOLE_LOOPBACK')
        self.thread.start()
        try:
            with open(self.path, 'w') as handle:
                json.dump({'pid': os.getpid(),
                           'port': server.server_address[1],
                           'token': self.token,
                           'version': PROTOCOL_VERSION,
                           'api': API_BASE,
                           'started': time.time()}, handle)
        except OSError:
            pass
        return True

    def release(self):
        if self.server is not None:
            try:
                self.server.shutdown()
                self.server.server_close()
            except Exception:
                pass
            self.server = None
        self.remove()

    def remove(self):
        try:
            os.remove(self.path)
        except OSError:
            pass


# -- 可滚动的列 ---------------------------------------------------------------

class ThumbBar:
    """A scrollbar that can be coloured, because Tk's cannot be on macOS.

    `tk.Scrollbar` under Aqua ignores `-background` and `-troughcolor`, so the
    dark window grew three light-grey strips as soon as it became scrollable.
    This keeps both ends of the real widget's contract -- `set(lo, hi)` from the
    canvas's `yscrollcommand`, `yview` back to the canvas -- and draws the rest
    itself. It draws nothing while the column fits: a thumb spanning the whole
    track is a control with no function.
    """

    WIDTH = 10
    INSET = 3
    MIN_THUMB = 28

    def __init__(self, tk_module, parent, view):
        self.tk = tk_module
        self.view = view
        self.canvas = tk_module.Canvas(parent, width=self.WIDTH, bg=BG,
                                       highlightthickness=0, bd=0)
        self.canvas.pack(side='right', fill='y')
        self.lo = 0.0
        self.hi = 1.0
        self._grab = None
        self.canvas.bind('<Configure>', lambda _e: self.redraw())
        self.canvas.bind('<ButtonPress-1>', self._press)
        self.canvas.bind('<B1-Motion>', self._drag)
        self.canvas.bind('<ButtonRelease-1>', self._release)

    def set(self, lo, hi):
        self.lo = float(lo)
        self.hi = float(hi)
        self.redraw()

    def _track(self):
        """(height, thumb top, thumb bottom), or None when there is nothing to
        scroll."""
        height = self.canvas.winfo_height()
        if height < self.MIN_THUMB + 2 or self.hi - self.lo >= 0.999:
            return None
        top = int(round(self.lo * height))
        bottom = int(round(self.hi * height))
        if bottom - top < self.MIN_THUMB:
            # A long column still deserves a thumb a hand can hit.
            middle = (top + bottom) // 2
            top = max(0, min(middle - self.MIN_THUMB // 2,
                             height - self.MIN_THUMB))
            bottom = top + self.MIN_THUMB
        return height, top, bottom

    def redraw(self):
        self.canvas.delete('thumb')
        box = self._track()
        if box is None:
            return
        _height, top, bottom = box
        self.canvas.create_polygon(
            [self.INSET, top, self.WIDTH - self.INSET, top,
             self.WIDTH - self.INSET, bottom, self.INSET, bottom],
            smooth=True, outline='', fill=BORDER_STRONG, tags='thumb')

    def _press(self, event):
        box = self._track()
        if box is None:
            return
        _height, top, bottom = box
        if top <= event.y <= bottom:
            self._grab = event.y - top
        else:
            self._move_to(event.y - (bottom - top) // 2)
            self._grab = (bottom - top) // 2

    def _drag(self, event):
        if self._grab is not None:
            self._move_to(event.y - self._grab)

    def _release(self, _event=None):
        self._grab = None

    def _move_to(self, y):
        """Put the thumb's top at pixel `y` -- which is the same fraction of the
        track that `moveto` wants, up to the point where the view hits bottom."""
        height = self.canvas.winfo_height()
        span = self.hi - self.lo
        if height <= 0 or span >= 0.999:
            return
        self.view('moveto', max(0.0, min(float(y) / height, 1.0 - span)))


class Column:
    """A scrolling stack of panels.

    Not laziness: with「系统声音」expanded during an install, the left column is
    taller than the minimum window height, and a clipped「一键设置」button is
    worse than a scrollbar. Both columns scroll for the same reason.
    """

    WHEEL = ('<MouseWheel>', '<Button-4>', '<Button-5>')

    def __init__(self, tk_module, parent):
        self.tk = tk_module
        self.panels = {}
        self.shown = None
        outer = tk_module.Frame(parent, bg=BG)
        outer.pack(fill='both', expand=True)
        self.canvas = tk_module.Canvas(outer, bg=BG, highlightthickness=0, bd=0)
        self.inner = tk_module.Frame(self.canvas, bg=BG)
        self._window = self.canvas.create_window((0, 0), window=self.inner,
                                                 anchor='nw')
        self.bar = ThumbBar(tk_module, outer, self.canvas.yview)
        self.canvas.configure(yscrollcommand=self.bar.set)
        self.canvas.pack(side='left', fill='both', expand=True)
        self.inner.bind('<Configure>', self._sync_region)
        self.canvas.bind('<Configure>', self._sync_width)
        self.canvas.bind('<Enter>', self._activate, add='+')
        self.canvas.bind('<Leave>', self._deactivate, add='+')

    def width(self):
        """Usable width, or a sane stand-in before the window is mapped."""
        value = self.canvas.winfo_width()
        return value if value > 24 else 300

    def add(self, key, box, **pack_kwargs):
        self.panels[key] = (box, pack_kwargs)

    def relayout(self, visible):
        order = [key for key in visible if key in self.panels]
        if order == self.shown:
            return
        self.shown = order
        for key, (box, _kwargs) in self.panels.items():
            box.pack_forget()
        for key in order:
            box, kwargs = self.panels[key]
            box.pack(**kwargs)

    def _sync_region(self, _event=None):
        self.canvas.configure(scrollregion=self.canvas.bbox('all'))

    def _sync_width(self, event):
        self.canvas.itemconfigure(self._window, width=event.width)
        self._sync_region()

    def _activate(self, _event=None):
        for sequence in self.WHEEL:
            self.canvas.bind_all(sequence, self._on_wheel, add='+')

    def _deactivate(self, _event=None):
        for sequence in self.WHEEL:
            self.canvas.unbind_all(sequence)

    def _on_wheel(self, event):
        if getattr(event, 'num', None) == 4:
            self.canvas.yview_scroll(-1, 'units')
        elif getattr(event, 'num', None) == 5:
            self.canvas.yview_scroll(1, 'units')
        else:
            delta = getattr(event, 'delta', 0)
            if not delta:
                return
            step = int(delta / -120) if abs(delta) >= 100 else (-1 if delta > 0
                                                                else 1)
            self.canvas.yview_scroll(step or 1, 'units')


# -- 窗口 ----------------------------------------------------------------------

class MirrorConsole:
    """The window itself.

    Plain object rather than a `tk.Tk` subclass, so this module imports without
    Tk at all: the pure view helpers above stay testable on a headless machine
    and in CI, which is the only reason this file has tests.
    """

    def __init__(self, tk_module, api=None):
        self.tk = tk_module
        self.api = api or Api()
        self.lock = ConsoleLock()
        self.root = None
        self.state = {}
        self.error = ''
        self.image_raw = None
        self.image = None
        self.frame_files = {}
        self.preview_fmt = 'png'
        self.preview_busy = threading.Event()
        self.quitting = threading.Event()
        self.fonts = {}
        self.widgets = {}
        self.scratch = {}
        self.device_keys = []
        self._wrappers = []
        self.embedded = False

    # -- build ---------------------------------------------------------------

    def run(self, root=None, parent=None, mainloop=True):
        """Build the console in its own window or inside an existing Tk tab."""
        if root is None:
            try:
                self.root = self.tk.Tk()
            except Exception as e:
                print('Tk 无法创建窗口（{}）：这台机器可能没有可用的显示服务。'.format(e),
                      file=sys.stderr)
                return 4
            self.root.title('Macast 电脑投屏')
            self.root.configure(bg=BG)
            self.root.minsize(*WINDOW_MIN)
            self.root.geometry(window_size_for(self.root.winfo_screenheight()))
            host = self.root
        else:
            self.root = root
            self.embedded = True
            host = parent or root
        try:
            self.preview_fmt = preview_format(
                self.root.tk.call('info', 'patchlevel'))
        except Exception:
            self.preview_fmt = 'ppm'
        self._fonts()
        self._build(host)
        if not self.embedded:
            self.root.protocol('WM_DELETE_WINDOW', self.quit)
        self._refresh()
        self._fetch_preview()
        if mainloop:
            self.root.mainloop()
        return 0

    def _fonts(self):
        from tkinter import font as tkfont
        try:
            family = tkfont.nametofont('TkDefaultFont').actual('family')
        except Exception:
            family = None

        def make(size, weight='normal', name=None):
            key = name or '{}{}'.format(size, weight[0])
            args = {'size': size, 'weight': weight}
            if family:
                args['family'] = family
            face = tkfont.Font(**args)
            self.fonts[key] = face
            return face

        self.f_title = make(15, 'bold', 'title')
        self.f_h = make(12, 'bold', 'h')
        self.f_body = make(11, name='body')
        self.f_small = make(10, name='small')
        self.f_mono = tkfont.Font(
            family='Menlo' if sys.platform == 'darwin' else 'Courier', size=10)

    def _panel(self, column, key, title=None, hint=None, expand=False):
        box = self.tk.Frame(column.inner, bg=PANEL, highlightbackground=BORDER,
                            highlightcolor=BORDER, highlightthickness=1, bd=0)
        if title:
            head = self.tk.Frame(box, bg=PANEL)
            head.pack(fill='x', padx=14, pady=(12, 0))
            self.tk.Label(head, text=title, bg=PANEL, fg=TEXT,
                          font=self.f_h).pack(side='left')
            if hint:
                self.tk.Label(head, text=hint, bg=PANEL, fg=MUTED,
                              font=self.f_small).pack(side='right')
        body = self.tk.Frame(box, bg=PANEL)
        body.pack(fill='both', expand=True, padx=14,
                  pady=(2, 12) if title else 12)
        column.add(key, box, fill='both' if expand else 'x', expand=expand,
                   padx=10, pady=(0, 10))
        return body

    def _wrap(self, widget, source, extra=0):
        """Wrap a label to a width its own text cannot move.

        Deriving `wraplength` from the label's *own* allocation makes the layout
        chase its tail: the wrap changes the request, the request changes the
        grid's column widths, and every widget settles one pass behind the
        truth -- that is how the four output cards ended up clipped by 3–5 px at
        a perfectly ordinary 1080-wide window. `source` is a callable giving a
        width decided by the window instead (``_text_width``), so one recompute
        is correct and the labels only ever get shorter.
        """
        self._wrappers.append((widget, source, extra))
        self._rewrap()
        return widget

    def _rewrap(self, _event=None):
        for widget, source, extra in self._wrappers:
            value = max(source() - extra, MIN_WRAP)
            if widget.cget('wraplength') != value:
                widget.configure(wraplength=value)

    def _window_width(self):
        """The window's width, or the size it opens at before it is mapped."""
        value = self.root.winfo_width()
        return value if value > 24 else int(WINDOW_SIZE.split('x')[0])

    def _text_width(self, column):
        """Width a plain label inside one of `column`'s panels is given."""
        return column.width() - COLUMN_INSET

    def _button(self, parent, text, command, accent=False, small=False,
                key=None):
        """A Label dressed as a button -- see the palette note at the top.

        Focusable and keyboard-triggerable, because a mouse-only window is not
        「易操作」, and because Tab-away is how a user finds out a control is
        disabled rather than clicking it four times.
        """
        bg = ACCENT if accent else PANEL_RAISED
        button = self.tk.Label(parent, text=text, bg=bg, fg=TEXT,
                               font=self.f_small if small else self.f_body,
                               padx=12, pady=6, cursor='hand2', takefocus=1)
        button.default_bg = bg
        button.accent = accent
        #: Held on the widget rather than closed over by `fire`, because one
        #: caller -- the copy button on a requirement row -- re-points it every
        #: render: the row's index is stable, its command is not.
        button.command = command
        # `state` is a real Tk method on these wrappers and answers nothing
        # useful for a Label, so disabled-ness is tracked here instead.
        button.off = False

        def fire(_event=None):
            if button.off:
                return
            button.command()

        button.bind('<Button-1>', fire)
        button.bind('<Return>', fire)
        button.bind('<space>', fire)
        button.bind('<Enter>', lambda _e: self._hover(button, True))
        button.bind('<Leave>', lambda _e: self._hover(button, False))
        button.bind('<FocusIn>', lambda _e: button.configure(
            highlightcolor=ACCENT, highlightbackground=ACCENT))
        button.bind('<FocusOut>', lambda _e: button.configure(
            highlightcolor=BORDER, highlightbackground=BORDER))
        if key:
            self.widgets[key] = button
        return button

    def _hover(self, button, inside):
        if button.off:
            return
        if not inside:
            button.configure(bg=button.default_bg)
        elif button.accent:
            button.configure(bg=ACCENT_HOVER)
        else:
            button.configure(bg=self._lighten(button.default_bg))

    def _disable(self, button, disabled):
        button.off = bool(disabled)
        button.configure(bg=BORDER if disabled else button.default_bg,
                         fg=DIM if disabled else TEXT,
                         cursor='circle' if disabled else 'hand2')

    def _lighten(self, hexcolor):
        try:
            rgb = [min(255, int(hexcolor[i:i + 2], 16) + 22)
                   for i in (1, 3, 5)]
            return '#{:02x}{:02x}{:02x}'.format(*rgb)
        except (ValueError, IndexError):
            return hexcolor

    def _build(self, host=None):
        host = host or self.root
        self._build_header(host)
        middle = self.tk.Frame(host, bg=BG)
        middle.pack(fill='both', expand=True)
        middle.columnconfigure(0, weight=5)
        middle.columnconfigure(1, weight=2, minsize=340)
        # Without a row weight the grid row stays at the canvases' *requested*
        # height, and a Canvas asks for its own default rather than the height of
        # the window item inside it -- the columns then fill a third of the
        # window and the rest is empty background.
        middle.rowconfigure(0, weight=1)
        self.left = Column(self.tk, self._column(middle, 0))
        self.right = Column(self.tk, self._column(middle, 1))
        # Registered after Column's own handler, so `item_width` is already the
        # new width by the time the labels are re-measured. A column also changes
        # width when a scrollbar appears or a panel is hidden, which is why this
        # is not just a window-resize handler.
        self.root.bind('<Configure>', self._rewrap, add='+')
        for column in (self.left, self.right):
            column.canvas.bind('<Configure>', self._rewrap, add='+')
        self._build_channels(self.left)
        self._build_devices(self.left)
        self._build_requirements(self.left)
        self._build_profiles(self.left)
        self._build_quality(self.left)
        self._build_capture(self.left)
        self._build_audio(self.left)
        self._build_viewer(self.right)
        self._build_preview(self.right)
        self._build_activity(self.right)
        self._build_footer(host)
        self._apply_layout()

    def _column(self, middle, index):
        holder = self.tk.Frame(middle, bg=BG)
        holder.grid(row=0, column=index, sticky='nsew')
        return holder

    def _build_header(self, root):
        bar = self.tk.Frame(root, bg=BG)
        bar.pack(fill='x', padx=16, pady=(14, 4))
        self.tk.Label(bar, text='电脑投屏', bg=BG, fg=TEXT,
                      font=self.f_title).pack(side='left')
        self.version_label = self.tk.Label(bar, text='', bg=BG, fg=DIM,
                                           font=self.f_small)
        self.version_label.pack(side='left', padx=(8, 0), pady=(5, 0))
        self.toggle_button = self._button(bar, '开始镜像', self._toggle,
                                          accent=True, key='toggle')
        self.toggle_button.pack(side='right')
        self.status_pill = self.tk.Label(bar, text='未镜像', bg=PANEL, fg=MUTED,
                                         font=self.f_small, padx=12, pady=7,
                                         highlightbackground=BORDER,
                                         highlightcolor=BORDER,
                                         highlightthickness=1)
        self.status_pill.pack(side='right', padx=(0, 12))

    def _build_footer(self, root):
        line = self.tk.Frame(root, bg=BORDER)
        line.pack(fill='x', padx=16)
        self.footer = self._wrap(self.tk.Label(root, text='', bg=BG, fg=MUTED,
                                               font=self.f_small, anchor='w',
                                               justify='left'),
                                 self._window_width, 2 * 16 + LABEL_SLACK)
        self.footer.pack(fill='x', padx=16, pady=(4, 10))

    # -- sections -------------------------------------------------------------

    def _build_channels(self, column):
        body = self._panel(column, 'channels', '投屏方式',
                           hint='先问这台电脑能往哪种协议投，再选投给哪台设备')
        #: Rows are built from the app's catalog on the first render, not from a
        #: constant in this file: a receiver protocol written later adds a row to
        #: ``CHANNELS`` and appears here without an edit to the window.
        self.channel_holder = body
        self.channel_widgets = {}

    def _build_devices(self, column):
        body = self._panel(column, 'devices', '设备',
                           hint='所选协议在同一局域网内搜索到的接收端')
        row = self.tk.Frame(body, bg=PANEL)
        row.pack(fill='x')
        self.device_list = self.tk.Listbox(
            row, bg=PANEL_RAISED, fg=TEXT, selectbackground=ACCENT,
            selectforeground=TEXT, highlightbackground=BORDER,
            highlightcolor=BORDER, relief='flat', borderwidth=0,
            font=self.f_body, height=4, activestyle='none',
            exportselection=False)
        self.device_list.pack(side='left', fill='both', expand=True)
        self.device_list.bind('<<ListboxSelect>>', self._device_chosen)
        side = self.tk.Frame(row, bg=PANEL)
        side.pack(side='left', fill='y', padx=(8, 0))
        side.pack_propagate(False)
        self._button(side, '重新搜索', lambda: self._call('refresh'),
                     small=True, key='refresh').pack(anchor='ne')
        self.device_note = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.device_note.pack(fill='x', pady=(6, 0))

    def _build_requirements(self, column):
        body = self._panel(column, 'requirements', '需要安装',
                           hint='装好之前，这几件事会一直写在这里')
        #: Built per render like the channel rows: the list is whatever the
        #: plugins currently report missing, and it shrinks as they are satisfied.
        self.requirement_holder = body
        self.requirement_widgets = {}

    def _build_profiles(self, column):
        body = self._panel(column, 'profiles', '兼容档位',
                           hint='老电视认哪种封装，就选哪种')
        self.profile_menu = self._choice(body, self._set_profile)
        self.profile_hint = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.profile_hint.pack(fill='x', pady=(6, 0))

    def _build_quality(self, column):
        body = self._panel(column, 'quality', '画质',
                           hint='改动画质会重开当前镜像')
        row = self.tk.Frame(body, bg=PANEL)
        row.pack(fill='x')
        self.quality_buttons = {}
        for index, key in enumerate(('360', '720', '1080', 'source')):
            button = self._button(row, '', lambda k=key: self._set_quality(k))
            button.grid(row=0, column=index, sticky='ew', padx=3)
            row.columnconfigure(index, weight=1, uniform='quality')
            self.quality_buttons[key] = button
        self.quality_note = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=WARN, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.quality_note.pack(fill='x', pady=(8, 0))

    def _build_capture(self, column):
        body = self._panel(column, 'capture', '这台电脑')
        row = self.tk.Frame(body, bg=PANEL)
        row.pack(fill='x')
        left = self.tk.Frame(row, bg=PANEL)
        left.pack(side='left', fill='x', expand=True)
        self.screen_menu = self._choice(left, self._set_screen)
        self.cursor_var = self.tk.IntVar(value=1)
        self._check(left, '显示鼠标指针', self.cursor_var,
                    lambda: self._set_cursor(bool(self.cursor_var.get()))).pack(
            anchor='w', pady=(9, 0))
        self.encoder_var = self.tk.IntVar(value=0)
        self.encoder_check = self._check(
            left, '硬件编码（VideoToolbox）', self.encoder_var,
            lambda: self._set_encoder('hardware' if self.encoder_var.get()
                                      else 'software'))
        self.encoder_check.pack(anchor='w', pady=(6, 0))
        side = self.tk.Frame(row, bg=PANEL)
        side.pack(side='left', fill='y', padx=(10, 0))
        self._button(side, '重新探测', lambda: self._call('probe'), small=True,
                     key='probe').pack(anchor='ne')
        self.capture_note = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.capture_note.pack(fill='x', pady=(8, 0))

    def _build_audio(self, column):
        body = self._panel(column, 'audio', '系统声音',
                           hint='macOS 可一键装好 BlackHole')
        self.audio_line = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=TEXT, font=self.f_body, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.audio_line.pack(fill='x')
        row = self.tk.Frame(body, bg=PANEL)
        row.pack(fill='x', pady=(9, 0))
        self.audio_row = row
        self.audio_setup = self._button(row, '一键设置',
                                        lambda: self._call('audio-setup'),
                                        small=True, key='audio-setup')
        self.audio_restore = self._button(row, '恢复原声音输出',
                                          lambda: self._call('audio-restore'),
                                          small=True)
        self.audio_page = self._button(row, '打开详细进度页',
                                       self._open_audio_page, small=True)
        self.audio_progress = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.audio_progress.pack(fill='x', pady=(8, 0))
        self.audio_steps = {}
        grid = self.tk.Frame(body, bg=PANEL)
        grid.pack(fill='x', pady=(4, 0))
        for index in range(11):
            mark = self.tk.Label(grid, text='', bg=PANEL, fg=DIM, width=2,
                                 font=self.f_small)
            mark.grid(row=index, column=0, sticky='w')
            label = self.tk.Label(grid, text='', bg=PANEL, fg=MUTED,
                                  font=self.f_small, anchor='w')
            label.grid(row=index, column=1, sticky='w')
            note = self._wrap(self.tk.Label(grid, text='', bg=PANEL, fg=DIM,
                                            font=self.f_small, anchor='w',
                                            justify='left'),
                              lambda: self._text_width(column),
                              STEP_TEXT_INSET + LABEL_SLACK)
            note.grid(row=index, column=2, sticky='ew', padx=(8, 0))
            self.audio_steps[index] = (mark, label, note)
        grid.columnconfigure(2, weight=1)
        self.audio_bar = self.tk.Canvas(body, height=6, bg=PANEL_RAISED,
                                        highlightthickness=0)
        self.audio_bar.pack(fill='x', pady=(8, 0))

    def _build_preview(self, column):
        body = self._panel(column, 'preview', '这台电脑的画面',
                           hint='约每秒刷新一次')
        self.preview_label = self.tk.Label(body, bg=PANEL_SUNKEN, fg=DIM,
                                           text='还没有画面', font=self.f_small,
                                           highlightbackground=BORDER,
                                           highlightcolor=BORDER,
                                           highlightthickness=1)
        self.preview_label.pack(fill='x')
        self.preview_note = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.preview_note.pack(fill='x', pady=(6, 0))

    def _build_viewer(self, column):
        body = self._panel(column, 'viewer', '观看地址')
        self.viewer_entry = self.tk.Entry(body, bg=PANEL_RAISED, fg=TEXT,
                                          insertbackground=TEXT,
                                          readonlybackground=PANEL_RAISED,
                                          disabledbackground=PANEL_RAISED,
                                          highlightbackground=BORDER,
                                          highlightcolor=ACCENT,
                                          relief='flat', font=self.f_mono)
        self.viewer_entry.pack(fill='x', pady=(2, 0))
        row = self.tk.Frame(body, bg=PANEL)
        row.pack(fill='x', pady=(6, 0))
        self._button(row, '复制', self._copy_viewer, small=True).pack(side='left')
        self._button(row, '在本机浏览器打开', self._open_viewer,
                     small=True).pack(side='left', padx=(6, 0))
        self.viewer_hint = self._wrap(self.tk.Label(
            body, text='', bg=PANEL, fg=MUTED, font=self.f_small, anchor='w',
            justify='left'), lambda: self._text_width(column), LABEL_SLACK)
        self.viewer_hint.pack(fill='x', pady=(6, 0))

    def _build_activity(self, column):
        body = self._panel(column, 'activity', '动态', hint='最近 20 条',
                           expand=True)
        self.activity = self.tk.Text(body, bg=PANEL_RAISED, fg=TEXT,
                                     highlightbackground=BORDER,
                                     highlightcolor=BORDER, relief='flat',
                                     font=self.f_small, height=10, width=20,
                                     wrap='word',
                                     state='disabled', padx=8, pady=6,
                                     insertbackground=TEXT)
        self.activity.pack(fill='both', expand=True)
        self.activity.tag_configure('time', foreground=DIM)
        self.activity.tag_configure('text', foreground=TEXT)
        self.activity.tag_configure('bad', foreground=BAD)

    # -- small widgets --------------------------------------------------------

    def _choice(self, parent, command):
        """A dark, native-looking dropdown.

        `tk.Menubutton` is themed away on macOS (Aqua paints it no matter what
        `bg=` says), while `tk.Menu` honours colours on all three platforms --
        so the trigger is a Label and the menu is posted by hand.
        """
        holder = self.tk.Frame(parent, bg=PANEL)
        label = self.tk.Label(holder, text='—', bg=PANEL_RAISED, fg=TEXT,
                              font=self.f_body, relief='flat', anchor='w',
                              padx=10, pady=6, highlightbackground=BORDER,
                              highlightcolor=BORDER_STRONG,
                              highlightthickness=1, cursor='hand2')
        menu = self.tk.Menu(holder, tearoff=0, bg=PANEL_RAISED, fg=TEXT,
                            activebackground=ACCENT, activeforeground=TEXT,
                            borderwidth=0)
        holder.pack(fill='x')
        label.pack(fill='x')
        holder.bind('<Button-1>', lambda _e: self._post(menu, label))
        label.bind('<Button-1>', lambda _e: self._post(menu, label))
        label.bind('<Down>', lambda _e: self._post(menu, label))
        label.bind('<Return>', lambda _e: self._post(menu, label))
        holder._label = label
        holder._menu = menu
        holder._command = command
        return holder

    def _post(self, menu, label):
        if menu.index('end') is None:      # nothing to choose yet
            return
        menu.post(label.winfo_rootx(),
                  label.winfo_rooty() + label.winfo_height())

    def _choice_fill(self, holder, entries, current, fallback='—'):
        """Rebuild a dropdown's rows; `entries` is [(value, label)]."""
        menu = holder._menu
        menu.delete(0, 'end')
        labels = {}
        for value, label in entries:
            menu.add_command(label=label,
                             command=lambda v=value: holder._command(v))
            labels[value] = label
        holder._label.configure(text=labels.get(current, entries[0][1]
                                                if entries else fallback))

    def _check(self, parent, text, variable, command):
        return self.tk.Checkbutton(parent, text=text, variable=variable,
                                   command=command, bg=PANEL, fg=TEXT,
                                   activebackground=PANEL,
                                   activeforeground=TEXT,
                                   selectcolor=PANEL_RAISED, font=self.f_body,
                                   highlightthickness=0, anchor='w',
                                   disabledforeground=DIM, bd=0)

    # -- actions --------------------------------------------------------------

    def _call(self, action, args=None, key=None):
        """Fire one action, then re-read. The reply goes in the feed.

        `key` names the control that should look busy while the request is in
        flight; without it a slow LAN looks like an ignored click, which is the
        one thing a desktop window must not be.
        """
        def work():
            root = self.root
            if root is None:
                return
            try:
                root.after(0, lambda: self._busy(key, True))
            except (RuntimeError, self.tk.TclError):
                return
            try:
                reply = self.api.action(action, args)
            except Exception as e:
                reply = {'code': 1, 'message': '操作失败：{}'.format(e)}
            try:
                root.after(0, lambda: self._busy(key, False))
            except (RuntimeError, self.tk.TclError):
                return
            message = reply.get('message') or ''
            failed = reply.get('code') != 0
            try:
                self.root.after(0, lambda: self._done(message, failed))
            except (RuntimeError, self.tk.TclError):
                return
        threading.Thread(target=work, daemon=True,
                         name='MIRROR_CONSOLE_ACTION').start()

    def _done(self, message, failed):
        if message:
            self._note(message, failed)
        self._refresh()

    def _busy(self, key, busy):
        widget = self.widgets.get(key)
        if widget is None:
            return
        try:
            widget.configure(cursor='watch' if busy else 'hand2')
        except Exception:
            pass

    def _toggle(self):
        if not self.state.get('available'):
            self._note('没有可用的电脑投屏插件：在设置页的「插件」里启用 Screen Mirror',
                       True)
            return
        self._call('toggle', key='toggle')

    def _set_output(self, key):
        if key == (self.state.get('output') or {}).get('kind'):
            return
        self._call('set-output', {'value': key})

    def _device_chosen(self, _event=None):
        selection = self.device_list.curselection()
        if not selection:
            return
        index = selection[0]
        if index >= len(self.device_keys):
            return
        key, name, kind = self.device_keys[index]
        if kind == 'dlna':
            self._call('set-dlna-target', {'name': name, 'control': key})
        else:
            self._call('set-target', {'name': name, 'target': key})

    def _set_profile(self, key):
        self._call('set-profile', {'value': key})

    def _set_quality(self, key):
        self._call('set-quality', {'value': key})

    def _set_screen(self, value):
        self._call('set-screen', {'value': value})

    def _set_cursor(self, value):
        self._call('set-cursor', {'value': value})

    def _set_encoder(self, value):
        self._call('set-encoder', {'value': value})

    def _open_audio_page(self):
        url = (self.state.get('audio') or {}).get('progress_url') or ''
        if url:
            _open_in_browser(url)
        else:
            self._note('现在没有进度页', True)

    def _copy_viewer(self):
        url = (self.state.get('viewer') or {}).get('url') or ''
        if not url:
            self._note('现在没有观看地址', True)
            return
        self._copy(url, '观看地址')

    def _copy(self, text, what):
        """Put `text` on the clipboard and say so in the feed.

        Two panels need this (the address to open, the command to install), and
        the failure has to quote the text back: a clipboard that refuses is
        useless, but the user still has to be able to finish the task.
        """
        if not text:
            self._note('现在没有' + what, True)
            return
        try:
            self.root.clipboard_clear()
            self.root.clipboard_append(text)
            self._note('{}已复制到剪贴板'.format(what))
        except Exception as e:
            self._note('复制失败：{}；内容是 {}'.format(e, text), True)

    def _open_viewer(self):
        url = (self.state.get('viewer') or {}).get('url') or ''
        if url:
            _open_in_browser(url)
        else:
            self._note('开始镜像后才有观看地址', True)

    # -- polling --------------------------------------------------------------

    def _refresh(self):
        if self.quitting.is_set():
            return
        self._fetch_state()
        self.root.after(POLL_MS, self._refresh)

    def _fetch_state(self):
        def work():
            try:
                body = self.api.state()
                error = ''
            except Exception as e:
                body, error = {}, str(e)
            try:
                self.root.after(0, lambda: self._apply(body, error))
            except (RuntimeError, self.tk.TclError):
                return
        threading.Thread(target=work, daemon=True,
                         name='MIRROR_CONSOLE_POLL').start()

    def _apply(self, body, error):
        if self.quitting.is_set():
            return
        self.error = error
        state = body.get('state') if body.get('code') == 0 else None
        if state is None:
            # The app answered, but not with a state:「换渲染器」/「插件没装」. Keep
            # whatever the last real state said about devices so the list does
            # not blink out every second.
            state = dict(self.state)
            state['available'] = False
            state['unavailable_message'] = body.get('message') or ''
            state.setdefault('recent', self.state.get('recent', []))
        self.state = state
        self._render()

    def _render(self):
        state = self.state
        self.version_label.configure(
            text='Screen Mirror v{}'.format(state.get('version') or '?'))
        mirroring = bool(state.get('mirroring'))
        available = bool(state.get('available'))
        self._disable(self.toggle_button, not available)
        self.toggle_button.configure(
            text='停止镜像' if mirroring else '开始镜像')
        self.status_pill.configure(text=format_stats(state),
                                   fg=GOOD if mirroring else MUTED)
        self._apply_layout()
        self._render_channels(state)
        self._render_devices(state)
        self._render_requirements(state)
        self._render_profiles(state)
        self._render_quality(state)
        self._render_capture(state)
        self._render_audio(state)
        self._render_viewer(state)
        self._render_activity(state)
        self._render_preview(state)
        text, color = banner_for(state, self.error)
        extra = state.get('unavailable_message') or ''
        if extra and not text:
            text, color = extra, WARN
        self.footer.configure(text='　'.join(x for x in (text, extra) if x),
                              fg=color or MUTED)

    def _apply_layout(self):
        visible = sections_for(self.state)
        self.left.relayout(visible)
        self.right.relayout(visible)

    def _render_channels(self, state):
        rows = channel_rows(state.get('channels'))
        prompt = state.get('prompt') or ''
        for index, (key, label, words, selected, needs, choice, tradeoff) \
                in enumerate(rows):
            card, title, mark, status, hint = self._channel_widgets(key)
            bg = ACCENT_BG if selected else PANEL_RAISED
            title.configure(text=label, bg=bg)
            mark.configure(text='● 正在使用' if selected else '', bg=bg)
            status.configure(text='　'.join(x for x in (words, choice) if x),
                             bg=bg,
                             fg=GOOD if words.startswith('发现 ')
                             else WARN if needs and not choice else MUTED)
            #: The「还要选一台」line belongs to the protocol in use: repeating it on
            #: every row would put a prompt under a protocol the user is not
            #: choosing, and then two of them would be wrong at once.
            hint.configure(text='　'.join(
                x for x in (tradeoff,
                            prompt if selected and needs else '') if x),
                bg=bg,
                fg=WARN if (selected and needs and prompt) else MUTED)
            card.configure(bg=bg,
                           highlightbackground=ACCENT if selected else BORDER)
        for key in list(self.channel_widgets):
            if not any(row[0] == key for row in rows):
                self.channel_widgets.pop(key)[0].pack_forget()

    def _channel_widgets(self, key):
        """The five labels of one protocol's row, built the first time it appears."""
        widgets = self.channel_widgets.get(key)
        if widgets:
            return widgets
        card = self.tk.Frame(self.channel_holder, bg=PANEL_RAISED,
                             highlightbackground=BORDER, highlightcolor=ACCENT,
                             highlightthickness=1)
        card.pack(fill='x', pady=(0, 6))
        head = self.tk.Frame(card, bg=PANEL_RAISED)
        head.pack(fill='x', padx=10, pady=(9, 0))
        title = self.tk.Label(head, text='', bg=PANEL_RAISED, fg=TEXT,
                              font=self.f_h, anchor='w', justify='left')
        title.pack(side='left')
        mark = self.tk.Label(head, text='', bg=PANEL_RAISED, fg=ACCENT,
                             font=self.f_small)
        mark.pack(side='right')
        status = self._wrap(self.tk.Label(
            card, text='', bg=PANEL_RAISED, fg=MUTED, font=self.f_small,
            anchor='w', justify='left'),
            lambda: self._text_width(self.left), LABEL_SLACK + ROW_INSET)
        status.pack(fill='x', padx=10, pady=(4, 0))
        hint = self._wrap(self.tk.Label(
            card, text='', bg=PANEL_RAISED, fg=MUTED, font=self.f_small,
            anchor='w', justify='left'),
            lambda: self._text_width(self.left), LABEL_SLACK + ROW_INSET)
        hint.pack(fill='x', padx=10, pady=(2, 10))
        for widget in (card, title, mark, status, hint):
            widget.bind('<Button-1>', lambda _e, k=key: self._set_output(k))
        widgets = (card, title, mark, status, hint)
        self.channel_widgets[key] = widgets
        return widgets

    def _render_devices(self, state):
        channel = current_channel(state)
        kind = channel.get('probe') or (state.get('output') or {}).get('kind')
        rows = device_rows(channel)
        self.device_keys = [(key, name, kind)
                            for key, _label, _sel, name in rows]
        self.device_list.delete(0, 'end')
        for index, (_key, label, selected, _name) in enumerate(rows):
            self.device_list.insert('end', label)
            self.device_list.itemconfigure(
                index, fg=ACCENT if selected else TEXT)
            if selected:
                self.device_list.selection_clear(0, 'end')
                self.device_list.selection_set(index)
        self.device_list.configure(state='normal' if rows else 'disabled')
        note = search_note(channel)
        prompt = state.get('prompt') or ''
        if prompt:
            note = (note + '　' if note else '') + prompt
        self.device_note.configure(text=note,
                                   fg=WARN if not rows else MUTED)

    def _render_requirements(self, state):
        items = state.get('requirements') or []
        for index, item in enumerate(items):
            box, head, label, button, detail, command = \
                self._requirement_widgets(index)
            label.configure(text=item.get('label') or '')
            detail.configure(text=item.get('detail') or '')
            text = item.get('command') or ''
            command.configure(text=text)
            if text:
                command.pack(fill='x', padx=10, pady=(6, 10),
                             before=box.requirement_tail)
            else:
                command.pack_forget()
            button.command = lambda t=text, w=item.get('label', ''): \
                self._copy(t, '「{}」的安装命令'.format(w))
            self._disable(button, not text)
            button.configure(text='复制命令' if text else '无可复制命令')
            box.pack(fill='x', pady=(0, 8))
        for index in sorted(self.requirement_widgets)[len(items):]:
            self.requirement_widgets.pop(index)[0].pack_forget()

    def _requirement_widgets(self, index):
        """One outstanding-requirement row: what is missing, and how to fix it.

        The command is a label, not an entry the user has to retype, and it is
        copied rather than run: what it installs is the plugin's call, and this
        window does not shell out on someone's behalf.
        """
        widgets = self.requirement_widgets.get(index)
        if widgets:
            return widgets
        box = self.tk.Frame(self.requirement_holder, bg=PANEL_RAISED,
                            highlightbackground=BORDER, highlightcolor=BORDER,
                            highlightthickness=1)
        head = self.tk.Frame(box, bg=PANEL_RAISED)
        head.pack(fill='x', padx=10, pady=(8, 0))
        label = self.tk.Label(head, text='', bg=PANEL_RAISED, fg=WARN,
                              font=self.f_h, anchor='w', justify='left')
        label.pack(side='left', fill='x', expand=True)
        button = self._button(head, '复制命令', lambda: None, small=True)
        button.pack(side='right', padx=(8, 0))
        detail = self._wrap(self.tk.Label(
            box, text='', bg=PANEL_RAISED, fg=MUTED, font=self.f_small,
            anchor='w', justify='left'),
            lambda: self._text_width(self.left), LABEL_SLACK + ROW_INSET)
        detail.pack(fill='x', padx=10, pady=(3, 0))
        command = self._wrap(self.tk.Label(
            box, text='', bg=PANEL_SUNKEN, fg=TEXT, font=self.f_mono,
            anchor='w', justify='left'),
            lambda: self._text_width(self.left), LABEL_SLACK + ROW_INSET)
        #: The command goes *under* the note and above the box's bottom padding,
        #: so `pack(after=)` has a stable reference whichever of the two rows the
        #: next render shows; a `before=` on a forgotten widget is an error.
        box.requirement_tail = self.tk.Frame(box, bg=PANEL_RAISED)
        box.requirement_tail.pack(fill='x')
        widgets = (box, head, label, button, detail, command)
        self.requirement_widgets[index] = widgets
        return widgets

    def _render_profiles(self, state):
        profiles = state.get('profiles') or {}
        self._choice_fill(self.profile_menu,
                          [(item['key'], item['label'])
                           for item in profiles.get('options') or []],
                          profiles.get('current'), fallback='未列出档位')
        kind = (state.get('output') or {}).get('kind')
        self.profile_hint.configure(
            text='' if kind == 'dlna' else '仅在选择「DLNA 电视」时生效',
            fg=MUTED)

    def _render_quality(self, state):
        quality = state.get('quality') or {}
        current = quality.get('current')
        short, note = quality_text(quality)
        for key, button in self.quality_buttons.items():
            button.configure(text=short.get(key, key))
            button.default_bg = ACCENT if key == current else PANEL_RAISED
            button.configure(bg=button.default_bg, fg=TEXT)
        self.quality_note.configure(text=note)

    def _render_capture(self, state):
        capture = state.get('capture') or {}
        screens = capture.get('screens') or []
        current = next((item['index'] for item in screens
                        if item.get('selected')), '')
        self._choice_fill(self.screen_menu,
                          [(item['index'], item['label']) for item in screens],
                          current, fallback='探测中…')
        self.cursor_var.set(1 if capture.get('cursor') else 0)
        self.encoder_var.set(1 if capture.get('encoder') == 'hardware' else 0)
        hardware = bool(capture.get('hardware_supported'))
        known = bool(capture.get('hardware_probed'))
        available = bool(capture.get('hardware_available'))
        self.encoder_check.configure(
            state='normal' if (hardware and known and available) else 'disabled')
        parts = []
        if capture.get('label'):
            parts.append('采集：' + capture['label'])
        if hardware and not known:
            parts.append('编码器探测中…（点「重新探测」可立刻再问一次）')
        elif hardware and not available:
            parts.append('这台机器的 ffmpeg 没有 VideoToolbox，会用 x264')
        if capture.get('probing'):
            parts.append('正在探测…')
        self.capture_note.configure(text=' · '.join(parts), fg=MUTED)

    def _render_audio(self, state):
        audio = state.get('audio') or {}
        self.audio_line.configure(text=audio.get('line') or '')
        self.audio_setup.pack_forget()
        self.audio_restore.pack_forget()
        self.audio_page.pack_forget()
        if audio.get('setup_available'):
            self.audio_setup.pack(side='left')
        if audio.get('restore_available'):
            self.audio_restore.pack(side='left', padx=(8, 0))
        if audio.get('progress_url'):
            self.audio_page.pack(side='left', padx=(8, 0))
        steps = audio.get('steps') or []
        for index, (mark, label, note) in self.audio_steps.items():
            if index >= len(steps):
                mark.configure(text='')
                label.configure(text='')
                note.configure(text='')
                continue
            step = steps[index]
            glyph, color = audio_step_mark(step)
            mark.configure(text=glyph, fg=color)
            label.configure(text=step.get('label', ''),
                            fg=TEXT if step.get('state') != 'pending' else DIM)
            note.configure(text=step.get('note') or '')
        self.audio_bar.delete('all')
        self.audio_progress.configure(text=audio.get('message') or '')
        if steps:
            width = max(1, int(self.audio_bar.winfo_width() *
                               min(1.0, float(audio.get('pct') or 0.0))))
            self.audio_bar.create_rectangle(0, 0, width, 6,
                                            fill=ACCENT, outline='')

    def _render_viewer(self, state):
        viewer = state.get('viewer') or {}
        url = viewer.get('url') or ''
        self.viewer_entry.configure(state='normal')
        self.viewer_entry.delete(0, 'end')
        if url:
            self.viewer_entry.insert(0, url)
        self.viewer_entry.configure(state='normal' if url else 'disabled')
        self.viewer_hint.configure(text=viewer.get('hint') or '', fg=MUTED)

    def _render_activity(self, state):
        recent = list(state.get('recent') or [])
        signature = hashlib.sha1(
            json.dumps([item.get('text') for item in recent],
                       ensure_ascii=False).encode('utf-8')).hexdigest()
        if self.scratch.get('feed') == signature:
            return
        self.scratch['feed'] = signature
        self.activity.configure(state='normal')
        self.activity.delete('1.0', 'end')
        for item in reversed(recent):
            try:
                stamp = time.strftime('%H:%M:%S',
                                      time.localtime(item.get('at', 0)))
            except (TypeError, ValueError):
                stamp = '--:--:--'
            text = item.get('text', '')
            self.activity.insert('end', stamp + '  ', 'time')
            self.activity.insert('end', text + '\n',
                                 'bad' if _looks_bad(text) else 'text')
        self.activity.configure(state='disabled')

    def _render_preview(self, state):
        preview = state.get('preview') or {}
        reason = preview.get('reason') or ''
        if not preview.get('has_frame'):
            note = reason or '还没有抓到画面'
        else:
            note = reason
        self.preview_note.configure(text=note,
                                    fg=BAD if reason else MUTED)

    def _note(self, message, bad=False):
        """Put one line in the feed right now, without waiting for the poll.

        A refused action never reaches the plugin's own `notify` (the plugin
        only records what it actually did), and a click that produced nothing
        is indistinguishable from a click that was dropped.
        """
        self.scratch.pop('feed', None)
        self.state.setdefault('recent', []).append(
            {'at': time.time(), 'text': message})
        del self.state['recent'][:-20]
        self._render_activity(self.state)
        if bad:
            self.footer.configure(text=message, fg=BAD)

    def _fetch_preview(self):
        if self.quitting.is_set():
            return
        self._poll_preview()
        self.root.after(PREVIEW_MS, self._fetch_preview)

    def _poll_preview(self):
        if self.preview_busy.is_set():
            return
        fmt = self.preview_fmt

        def work():
            self.preview_busy.set()
            failure = ''
            try:
                raw = self.api.snapshot(fmt)
            except Exception as e:
                raw, failure = b'', str(e)
            self.preview_busy.clear()
            if raw:
                self.root.after(0, lambda: self._show_frame(raw, fmt))
            elif failure and self.image is None:
                # Only while there is nothing to show: the app already reports a
                # failed *grab* through `preview.reason`, and a stale frame beats
                # a blank box with a stack trace in it.
                self.root.after(0, lambda: self._preview_failed(failure))
        threading.Thread(target=work, daemon=True,
                         name='MIRROR_CONSOLE_PREVIEW').start()

    def _preview_failed(self, error):
        if self.image is not None or self.quitting.is_set():
            return
        self.preview_note.configure(text='预览取不到：{}'.format(error), fg=BAD)

    def _show_frame(self, raw, fmt):
        if self.quitting.is_set():
            return
        if not frame_ok(raw, fmt):
            return
        path = self._frame_path(fmt)
        try:
            with open(path, 'wb') as handle:
                handle.write(raw)
            image = self.tk.PhotoImage(file=path)
            factor = fit_factor(image.width(), self.preview_width())
            scaled = image.subsample(factor, factor)
        except Exception as e:
            self.preview_note.configure(text='预览解码失败：{}'.format(e),
                                        fg=BAD)
            return
        self.image_raw, self.image = image, scaled
        self.preview_label.configure(image=scaled, text='')

    def _frame_path(self, fmt):
        """One reused temp file per format, removed when the window closes.

        Reused rather than fresh each second because Tk keeps the file open only
        while decoding, and a leftover frame of the user's desktop in /tmp is
        the thing to avoid -- not the write itself.
        """
        if self.frame_files.get(fmt) is None:
            import tempfile
            self.frame_files[fmt] = os.path.join(
                tempfile.gettempdir(),
                'macast-console-{}-{}{}'.format(os.getpid(),
                                                secrets.token_hex(3),
                                                frame_suffix(fmt)))
        return self.frame_files[fmt]

    def _remove_frame_files(self):
        for fmt, path in list(self.frame_files.items()):
            try:
                os.remove(path)
            except OSError:
                pass
            self.frame_files[fmt] = None

    def preview_width(self):
        width = self.preview_label.winfo_width()
        return max(120, width - 4)

    # -- lifecycle ------------------------------------------------------------

    def start(self):
        """Single instance: raise the window that exists, or become it."""
        existing = self.lock.existing()
        if existing is not None:
            _raise_existing(existing)
            return False
        if not self.lock.acquire(self._show):
            existing = self.lock.existing()
            if existing is not None:
                _raise_existing(existing)
            return False
        return True

    def _show(self):
        root = getattr(self, 'root', None)
        if root is None:
            return
        try:
            root.after(0, self._bring_to_front)
        except RuntimeError:
            pass

    def _bring_to_front(self):
        self.root.deiconify()
        self.root.lift()
        # The topmost pulse is how a background app on macOS actually gets its
        # window in front; leaving it set would pin the console over everything.
        try:
            self.root.wm_attributes('-topmost', True)
            self.root.after(300, lambda: self.root.wm_attributes(
                '-topmost', False))
        except Exception:
            pass
        self.root.focus_force()

    def quit(self):
        self.quitting.set()
        if self.embedded and self.state.get('mirroring'):
            try:
                self.api.action('stop')
            except Exception:
                pass
        if not self.embedded:
            self.lock.release()
        self._remove_frame_files()
        if self.embedded:
            return
        try:
            self.root.destroy()
        except Exception:
            pass


def _looks_bad(text):
    return any(word in text for word in ('失败', '没有', '不能', '无法', '错误'))


def _open_in_browser(url):
    """Open a URL in the user's browser, portably and without a shell."""
    try:
        if sys.platform == 'darwin':
            args = ['open', url]
        elif sys.platform == 'win32':
            os.startfile(url)          # windows-only attribute
            return
        else:
            args = ['xdg-open', url]
        subprocess.Popen(args, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception as e:
        print('cannot open {}: {}'.format(url, e), file=sys.stderr)


def _raise_existing(info):
    """Ask the open window to come forward; print, because we are the loser."""
    request = urllib.request.Request(
        'http://127.0.0.1:{}/raise'.format(info['port']), data=b'')
    request.add_header('X-Macast-Console-Token', str(info.get('token') or ''))
    try:
        with _opener().open(request, timeout=1.5):
            pass
        print('已经有投屏控制台在运行，已把它带到前面', file=sys.stderr)
        return True
    except Exception as e:
        print('已有控制台窗口但叫不醒它：{}'.format(e), file=sys.stderr)
        return False


def main():
    """Entry point for both `python macast/mirror_console.py` and `--mirror-console`."""
    try:
        import tkinter
    except ImportError as e:
        print('这个 Python 没有自带 Tk，无法打开投屏控制台：{}'.format(e),
              file=sys.stderr)
        return 1
    if tk_ok(tkinter) is None:
        print('这个 Python 的 Tk 版本是 {}，本窗口画不出内容'
              '（macOS 自带的 /usr/bin/python3 正是 8.5.9）。\n'
              '请改用带 Tk 8.6+ 的 Python：brew install python-tk，'
              '或在 Windows/Linux 的安装包里勾选 tcl/tk。'.format(
                  getattr(tkinter, 'TkVersion', '?')), file=sys.stderr)
        return 3
    if not API_TOKEN:
        print('没有 MACAST_CONSOLE_TOKEN：控制台需要管理令牌才能开关镜像。\n'
              '请从 Macast 菜单栏的「电脑投屏」打开本窗口（它会带上令牌），'
              '或手工设置 MACAST_CONSOLE_API / MACAST_CONSOLE_TOKEN。',
              file=sys.stderr)
        return 2
    console = MirrorConsole(tkinter)
    if not console.start():
        return 0
    try:
        return console.run()
    finally:
        console.quit()


if __name__ == '__main__':
    sys.exit(main())
