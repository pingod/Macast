# AirPlay screen mirroring (iPhone / iPad / Mac -> this machine) for Macast,
# by supervising uxplay
#
# Macast Metadata
# <macast.title>AirPlay Screen Mirror</macast.title>
# <macast.protocol>AirPlayMirrorProtocol</macast.protocol>
# <macast.platform>darwin,linux,win32</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Show your iPhone / iPad / Mac screen on this computer. Macast's own AirPlay code only accepts a video URL; screen mirroring is a different thing (H.264 over an encrypted AirPlay session) and is done here by supervising uxplay, which you build and install yourself -- there is no Homebrew formula and no macOS binary in its releases.</macast.desc>
#
# Why a supervisor and not an implementation: uxplay already speaks the whole
# AirPlay mirroring session -- pairing, the stream key, the AES-128-CTR data
# channel, H.264 decoding through GStreamer -- and is maintained against real
# iOS builds. Re-implementing that in a single-file online plugin (which may
# only use Macast's own dependencies, the standard library and command-line
# programs) is not on the table. What Macast adds on top is the part uxplay
# leaves to the user: a service name that matches this machine's Macast name, a
# lifecycle that follows the plugin's enable / disable switch, and a report of
# whether it is actually running.
#
# Correction to an old note in this repo: AirPlay *mirroring* is not FairPlay.
# The mirror data channel is AES-128-CTR with a key derived from the RSA/AES
# handshake, so a receiver can be built without touching DRM -- which is what
# AGENTS.md used to claim it could not be. DRM *content* is still out of reach:
# an app that refuses to be mirrored shows up black, and that is by design.
#
# Notes for whoever touches this next:
#   * the options go in a file we write, passed with `-rc <file>`; `--rc`-style
#     command-line guessing is not done anywhere here. `-rc` is chosen over the
#     `$UXPLAYRC` environment variable on purpose: when `$UXPLAYRC` points at a
#     file that does not exist, uxplay silently falls back to the user's own
#     `~/.uxplayrc`, so a failed write would look like "our settings were
#     ignored". `-rc` on a missing file exits with an error instead.
#   * the file lives in Macast's own config directory. `~/.uxplayrc` is a file
#     the user may have written by hand, and this project does not overwrite
#     user files to verify or to run a plugin (AGENTS.md §10).
#   * no `-p`: left alone, uxplay picks dynamic ports and advertises them in
#     mDNS, which is what AirPlay clients read. `-p` means the *legacy* port set
#     including TCP 7000 -- the port Macast's own AirPlay protocol binds, and
#     the one macOS's built-in AirPlay Receiver already holds on Apple Silicon.
#   * `uses_ssdp = False`: nothing here is discovered over SSDP, and setting it
#     true would keep the SSDP server alive when this is the only protocol on.
#     uxplay advertises `_airplay._tcp` over mDNS itself.
#   * state is *not* reported as DLNA transport state. There is no media URL in
#     a mirror session and no position to report; connection and disconnection
#     are surfaced as notifications instead, the same way the RAOP plugin does
#     it, so the ledger DLNA keeps for the other protocols stays truthful.
#   * with the built-in AirPlay protocol switched on as well, the phone lists
#     two receivers with nearly the same name. `start()` says so once -- see
#     `_warn_duplicate_receiver`. For the same reason, do not run this plugin
#     next to "AirPlay Audio (RAOP)": uxplay is a full AirPlay receiver (its
#     own banner says "mirroring and audio-streaming"), so shairport-sync and
#     uxplay would both advertise the same name and only one of them answers.

import collections
import logging
import os
import re
import shutil
import subprocess
import sys
import threading
from enum import Enum

import cherrypy

from macast import utils
from macast.protocol import Protocol
from macast.utils import Setting

logger = logging.getLogger("AirPlayMirror")
logger.setLevel(logging.INFO)

#: uxplay's startup file. Its own name, and its own format: one option per
#: line with no leading dash, `#` for comments.
RC_NAME = 'uxplayrc'

#: Free-text escape hatch: extra option lines appended to the generated file.
OPTIONS_KEY = 'Mirror_Uxplay_Options'


class SettingProperty(Enum):
    #: see OPTIONS_KEY -- the key lives here because Setting.get() reads
    #: `property.name`, so a plugin needs an enum member even for a key it
    #: only ever reads.
    Mirror_Uxplay_Options = 1


#: PATH first, then the usual install spots: a GUI app started from Finder or
#: the Dock does not inherit the shell's PATH, and `sudo make install` puts
#: uxplay in /usr/local/bin while a MacPorts build puts it in /opt/local/bin.
EXTRA_BIN_DIRS = ('/opt/homebrew/bin', '/usr/local/bin', '/opt/local/bin',
                  '/usr/bin')

#: Short version, for a notification. The recipe goes to the log.
NO_UXPLAY_MESSAGE = ('未找到 uxplay，AirPlay 屏幕镜像无法启动：它没有 Homebrew '
                     '包，官方发布也不提供 macOS 二进制，需要自己编译（步骤见日志）')

INSTALL_GUIDE = """\
uxplay is not installed, so AirPlay screen mirroring cannot start. There is no
Homebrew formula and no macOS binary among its release assets, so it has to be
built (verified against FDH2/UxPlay, README "Building UxPlay on macOS"):
  1. Xcode command line tools:  sudo xcode-select --install
  2. brew install cmake libplist openssl@3
  3. install GStreamer from https://gstreamer.freedesktop.org/download/ --
     BOTH the runtime and the -devel .pkg (they land in
     /Library/Frameworks/GStreamer.framework). Do not mix this with Homebrew's
     or MacPorts' GStreamer.
  4. git clone https://github.com/FDH2/UxPlay && cd UxPlay
     cmake . && make && sudo make install
On Linux the same options apply but package names differ (and Wayland users
usually want "vs waylandsink" in the extra options below).
Extra options: advanced setting "{}".""".format(OPTIONS_KEY)

#: uxplay's own words for the events worth telling the user about.
CONNECTED_RE = re.compile(r'connection request from (.*?) \((.*?)\)'
                          r' with deviceID = (\S+)')
DISCONNECTED = 'lost connection with client'
BLOCKED = 'attempt to connect by blocked client'
MDNS_FAILED = 'dnssd_register_airplay failed'
READY = 'initialized server socket'
#: uxplay's audio-mode progress line: it rewrites itself with `\\r` and no
#: newline, and a pty's universal-newline reading turns each rewrite into its
#: own line. Worth logging once a second it is not.
PROGRESS = 'audio progress'


def find_uxplay():
    """Path of the uxplay binary, or None.

    Module-level so tests can swap it, and looked up at start() rather than at
    import: building and installing uxplay while Macast runs should not need a
    restart.
    """
    found = shutil.which('uxplay')
    if found:
        return found
    for directory in EXTRA_BIN_DIRS:
        candidate = os.path.join(directory, 'uxplay')
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def safe_name(name):
    """The AirPlay server name, made safe for the option file it travels in.

    uxplay's config parser splits a line on spaces, honours `"..."` as one
    item, and gives the first item a leading `-`. That has three consequences
    this function exists for: the value must be quoted (a friendly name with a
    space is the normal case, not an edge case), a quote or backslash inside
    the value cannot be escaped -- the backslash survives into the token and a
    quote can end the item early -- so they become spaces, and a value
    beginning with `-` is read as "this option had no argument", which makes
    uxplay refuse to start.
    """
    cleaned = name or 'Macast'
    for char in '"\\\'\r\n\t':
        cleaned = cleaned.replace(char, ' ')
    cleaned = ' '.join(cleaned.split()).lstrip('-').strip()
    return cleaned or 'Macast'


def extra_rc_lines(raw=None):
    """Option lines the user added through the advanced settings.

    A protocol plugin gets no menu, so this is the whole knob -- deliberately
    the option language itself rather than three hand-picked toggles, because
    `-pin`, `-restrict`, `-vrtp`, `-vs 0` and the videosink choice all matter
    to somebody and guessing which three to expose is not this plugin's job.
    The text is the user's own, typed into their own settings file (same trust
    level as the Automation Hooks plugin's commands); it reaches uxplay as a
    line in a file written here, never through a shell.
    """
    if raw is None:
        raw = (Setting.get(SettingProperty.Mirror_Uxplay_Options, '')
               if Setting.has(SettingProperty.Mirror_Uxplay_Options) else '')
    lines = []
    for line in str(raw).splitlines():
        line = line.strip()
        if not line:
            continue
        # Accept `-pin 1234` as well as `pin 1234`: the file format wants no
        # dash, but everybody types one because every other tool uses it.
        lines.append(line[1:].strip() if line.startswith('-') else line)
    return lines


def rc_body(name, extra_lines=()):
    """The generated uxplay option file, as text.

    uxplay feeds this file through the same argument parser as the command
    line, last occurrence wins, and the extra lines go last -- so anything the
    user puts in the advanced setting overrides the two defaults above rather
    than fighting with them.
    """
    lines = [
        '# Generated by Macast\'s "AirPlay Screen Mirror" plugin.',
        '# The server name comes from Macast\'s friendly-name setting; edit it',
        '# there rather than here, or the next start will overwrite this file.',
        '# Extra options: advanced setting {} (one uxplay option per line,'
        ' no leading dash).'.format(OPTIONS_KEY),
        'n "%s"' % safe_name(name),
        # Upstream's own recommendation for mirroring: with timestamp sync
        # (the default since uxplay 1.64) many frames are dropped on macOS,
        # and for a screen mirror there is nothing to sync audio against.
        'vsync no',
    ]
    if sys.platform == 'darwin':
        # The official GStreamer build for macOS only offers glimagesink and
        # osxvideosink; upstream points at the former's problems, so pick the
        # latter instead of letting autovideosink choose.
        lines.append('vs osxvideosink')
    lines.extend(extra_lines)
    return '\n'.join(lines) + '\n'


def write_rc(name):
    """Write the option file and return its path."""
    os.makedirs(utils.SETTING_DIR, exist_ok=True)
    path = os.path.join(utils.SETTING_DIR, RC_NAME)
    with open(path, 'w', encoding='utf-8') as handle:
        handle.write(rc_body(name, extra_rc_lines()))
    return path


def log_event(line):
    """Map one uxplay log line to `(event, message)`, or `(None, None)`.

    Split out of the reader because this table is the plugin's whole interface
    to uxplay's output, and it has to keep working when the wording shifts a
    little: the same sentence arrives with a `*** ERROR: ` prefix, without one,
    or from a library logger, depending on which component wrote it. The
    sentence is the contract, the prefix is not -- hence the case-insensitive
    substring tests.
    """
    lowered = line.lower()
    match = CONNECTED_RE.search(line)
    if match:
        device, model = match.group(1).strip(), match.group(2).strip()
        detail = '{}（{}）'.format(device, model) if model else device
        return 'connected', 'AirPlay 屏幕镜像已连接：{}'.format(detail)
    if DISCONNECTED in lowered:
        return 'disconnected', 'AirPlay 屏幕镜像已断开'
    if BLOCKED in lowered:
        return 'blocked', '有被拒绝的 AirPlay 镜像连接请求（uxplay 的客户端限制）'
    if MDNS_FAILED in lowered:
        return 'mdns-failed', ('uxplay 的 mDNS 注册失败：iPhone 的镜像列表里'
                               '不会有这台 Mac（多为同名接收端已在运行）')
    if READY in lowered:
        return 'ready', ''
    return None, None


def _open_stdout():
    """`(child_fd, read_fd)` for uxplay's merged output.

    uxplay reports its events with plain `printf()`, and C's stdout is
    block-buffered when it is a pipe: "connection request from ..." would then
    sit in a 4 KiB buffer until enough logging happened to push it out, which
    is exactly the latency this plugin cannot tolerate -- an immediate event is
    the only thing it has to report. A pty keeps stdout line-buffered. Windows
    has no pty, so there the pipe stays block-buffered and the notifications
    may be late; the process-liveness report below does not depend on the log,
    so a silent death is still caught on every platform.
    """
    try:
        import pty
    except ImportError:
        return os.pipe()
    master, slave = pty.openpty()
    return slave, master


class AirPlayMirrorProtocol(Protocol):
    """Keeps uxplay running; Macast never touches the mirrored video."""

    #: Nothing to discover over SSDP (uxplay advertises over mDNS).
    uses_ssdp = False

    def __init__(self):
        super(AirPlayMirrorProtocol, self).__init__()
        self._proc = None
        self._reader = None
        self._lock = threading.Lock()

    def running(self):
        with self._lock:
            return self._proc is not None and self._proc.poll() is None

    def start(self):
        if self.running():
            return
        binary = find_uxplay()
        if binary is None:
            logger.error(INSTALL_GUIDE)
            cherrypy.engine.publish('app_notify', 'Macast', NO_UXPLAY_MESSAGE)
            return
        try:
            rc_path = write_rc(Setting.get_friendly_name())
        except OSError as e:
            message = '无法写入 uxplay 选项文件：{}'.format(e)
            logger.error(message)
            cherrypy.engine.publish('app_notify', 'Macast', message)
            return
        argv = [binary, '-rc', rc_path]
        logger.info('starting %s', ' '.join(argv))
        child_fd = read_fd = None
        try:
            child_fd, read_fd = _open_stdout()
            proc = subprocess.Popen(argv, stdout=child_fd,
                                    stderr=subprocess.STDOUT,
                                    stdin=subprocess.DEVNULL)
        except OSError as e:
            message = '启动 uxplay 失败：{}'.format(e)
            logger.error(message)
            cherrypy.engine.publish('app_notify', 'Macast', message)
            for fd in (child_fd, read_fd):
                if fd is not None:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
            return
        # This process's copy of the child's end has to go: hold it and the
        # reader never sees end-of-file when uxplay dies.
        try:
            os.close(child_fd)
        except OSError:
            pass
        with self._lock:
            self._proc = proc
        try:
            stream = os.fdopen(read_fd, 'r', encoding='utf-8', errors='replace')
        except OSError as e:  # pragma: no cover - not seen in practice
            logger.error('cannot read uxplay output: %s', e)
            return
        self._reader = threading.Thread(target=self._read_output,
                                        args=(proc, stream), daemon=True,
                                        name='AIRPLAY_MIRROR_LOG')
        self._reader.start()
        self._warn_duplicate_receiver()

    def stop(self):
        with self._lock:
            proc, self._proc = self._proc, None
        if proc is not None and proc.poll() is None:
            try:
                proc.terminate()
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            except OSError as e:
                logger.error('cannot stop uxplay: %s', e)
        logger.info('uxplay stopped')

    def reload(self):
        self.stop()
        self.start()

    def _warn_duplicate_receiver(self):
        """Say it out loud when the phone has two AirPlay entries to choose.

        Macast's built-in AirPlay protocol accepts a video URL only, so an
        iPhone that mirrors to *that* one shows nothing at all -- a silent
        failure with a confusing cause. It reads the persisted protocol
        selection rather than asking the running app, so it cannot break when
        either side is uninstalled, and it is a hint only: this plugin still
        runs, because the two receivers do not collide (dynamic ports, and
        separate mDNS names).
        """
        if not Setting.has(utils.SettingProperty.Macast_Protocols):
            return
        stored = Setting.get(utils.SettingProperty.Macast_Protocols, [])
        titles = [stored] if isinstance(stored, str) else (stored or [])
        for title in titles:
            # The comparison is against the *built-in's* title, which is the
            # short "AirPlay" (`MacastPlugin(None, "AirPlay", ...)` in
            # macast.py), while a file plugin's title is its manifest
            # `<macast.title>` -- "AirPlay Screen Mirror" here, "AirPlay Audio
            # (RAOP)" for the RAOP supervisor. Lowercasing and dropping a
            # trailing " protocol" only covers the DLNA-style class-derived
            # spelling; nothing here is supposed to match this plugin, and
            # exactly one thing must: the competing video-URL receiver.
            if str(title).lower().replace(' protocol', '').strip() == 'airplay':
                cherrypy.engine.publish(
                    'app_notify', 'Macast',
                    '屏幕镜像列表里会有两个接收端：能镜像的是本插件起的 uxplay，'
                    'Macast 内置的 AirPlay 只接受视频网址；两者名字都跟这台机器的'
                    '友好名一致，分不清就在设置里关掉其中一个')
                return

    def _report_exit(self, proc, tail):
        """Report a uxplay that disappears by itself.

        The failures that need this are silent ones: GStreamer cannot find the
        videosink, an option line could not be parsed, something killed the
        process. Macast keeps running and the protocol still reads as enabled,
        so without a notification the only symptom is an iPhone that finds
        nothing. `tail` carries uxplay's own last words, which is usually the
        whole diagnosis.
        """
        with self._lock:
            if self._proc is not proc:
                return  # stopped on purpose, or replaced by a newer instance
            self._proc = None
        code = None
        try:
            code = proc.wait(timeout=5)
        except subprocess.TimeoutExpired:  # pragma: no cover - defensive
            logger.error('uxplay left behind after its log ended')
        detail = ''
        for line in reversed(tail):
            if 'error' in line.lower():
                detail = line
                break
        message = 'uxplay 已退出（退出码 {}）'.format(code)
        if detail:
            message = '{}：{}'.format(message, detail[-160:])
        logger.error(message)
        cherrypy.engine.publish('app_notify', 'Macast', message)

    def _read_output(self, proc, stream):
        """Follow uxplay's log and surface its session events to the user.

        There is no status socket and no control port to poll, so the log is
        the only signal available. Connection events are reported as
        notifications rather than as DLNA transport state: a mirror session has
        no media url and no position, and DLNA owns the transport ledger for
        the other protocols.
        """
        tail = collections.deque(maxlen=20)
        try:
            for line in stream:
                line = line.strip()
                if not line or line.startswith(PROGRESS):
                    continue
                logger.info('uxplay: %s', line)
                tail.append(line)
                event, message = log_event(line)
                if event in (None, 'ready'):
                    continue
                cherrypy.engine.publish('app_notify', 'Macast', message)
        except (OSError, ValueError) as e:
            # Reading a pty master whose child has gone raises OSError(ENOTTY
            # /EIO) rather than ending the iteration cleanly. That is this
            # stream's normal end, not a fault worth an ERROR line.
            logger.info('uxplay log reader stopped: %s', e)
        finally:
            try:
                stream.close()
            except OSError:
                pass
        self._report_exit(proc, tail)


if __name__ == '__main__':
    from macast import cli
    from macast_renderer.mpv import MPVRenderer

    cli(renderer=MPVRenderer(path='mpv'), protocol=AirPlayMirrorProtocol())
