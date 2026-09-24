# Copyright (c) 2026 by pingod. All Rights Reserved.
# yt-dlp Downloader / Streamer for Macast
#
# Macast Metadata
# <macast.title>yt-dlp Downloader</macast.title>
# <macast.renderer>YTDLPRenderer</macast.renderer>
# <macast.platform>darwin,linux,win32</macast.platform>
# <macast.version>0.2</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.role>addon</macast.role>
# <macast.desc>把投屏的内容下载下来而不是播放（B 站、YouTube、m3u8 等），或边下边播。需本机已安装 yt-dlp 命令。</macast.desc>
#
# Why this exists: a sender can only hand over a URL. For Bilibili / YouTube /
# an m3u8 index page that URL is not a file, and the built-in mpv renderer has
# nothing to play unless mpv's own yt-dlp integration happens to be set up.
# This renderer offers both answers:
#
#   Download (default)  yt-dlp saves the media to disk; mpv is not started.
#   Stream              mpv plays through its ytdl hook while yt-dlp fetches
#                       (we tell the hook where the binary is); nothing is
#                       written to disk.
#
# Pick the mode from the menu bar. It shells out to the `yt-dlp` binary
# (brew install yt-dlp / pipx install yt-dlp / a release binary) rather than
# importing the library, so Macast needs no new Python dependency and this stays
# a single installable file.
#
# Notes for whoever touches this next:
#   * in download mode mpv is deliberately *not* started: an idle player window
#     staring at you while a file downloads is worse than no window.
#   * `_mpv_started` exists because the mode can change at runtime (`reload_renderer`
#     restarts the renderer), and MPVRenderer.stop() joins threads that only
#     exist when start() actually launched mpv.
#   * pause/resume in download mode are deliberately NOT implemented: faking a
#     PAUSED_PLAYBACK state for a downloader would lie to the phone.
#   * download progress reuses position/duration as a progress bar (elapsed vs
#     elapsed+ETA). That is the only reason the phone and the settings page show
#     a live percentage instead of a frozen 0:00:00.
#   * the child inherits the environment on purpose: unlike mpv, yt-dlp talks to
#     the internet, and stripping http_proxy (what Setting.get_system_env does
#     for players) would break YouTube for anyone who relies on a proxy.

import os
import re
import shutil
import subprocess
import sys
import threading
import time
import logging
from enum import Enum

import cherrypy

from macast import Setting, MenuItem, gui
from macast.renderer import Renderer
from macast.gui import App
from macast_renderer.mpv import MPVRenderer, MPVRendererSetting

logger = logging.getLogger("YTDLPRenderer")
logger.setLevel(logging.INFO)

DEFAULT_DIR = os.path.join(os.path.expanduser('~'), 'Downloads', 'Macast')

#: `[download]  12.3% of 123.45MiB at 1.23MiB/s ETA 00:12`
PROGRESS_RE = re.compile(
    r'^\[download\]\s+(?P<percent>[\d.]+)%'
    r'(?:\s+of\s+~?\s*(?P<size>\S+))?'
    r'(?:\s+at\s+(?P<speed>\S+))?'
    r'(?:\s+ETA\s+(?P<eta>\S+))?')
#: `[download] Destination: /Users/me/Downloads/Macast/title [id].mp4`
DEST_RE = re.compile(r'^\[(?:download|Merger|ExtractAudio|fixup[^\]]*)\]\s+'
                     r'Destination:\s+(?P<path>.+)$')
#: `1:02:03` / `02:03`
ETA_RE = re.compile(r'^(?:(\d+):)?(\d+):(\d+)$')

#: A GUI app started from Finder/Dock does not inherit the shell's PATH, so a
#: brew-installed yt-dlp looks missing even though the user has it.
EXTRA_BIN_DIRS = (
    '/opt/homebrew/bin',
    '/usr/local/bin',
    '/opt/local/bin',
    os.path.join(os.path.expanduser('~'), '.local', 'bin'),
)


def find_ytdlp():
    """Path of the yt-dlp binary, or None when nothing usable is installed.

    Module-level (and looked up per use, not at import) so tests can swap it,
    and so installing yt-dlp while Macast runs takes effect immediately.
    """
    found = shutil.which('yt-dlp') or shutil.which('youtube-dl')
    if found:
        return found
    for directory in EXTRA_BIN_DIRS:
        for name in ('yt-dlp', 'youtube-dl'):
            candidate = os.path.join(directory, name)
            if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                return candidate
    return None


def eta_seconds(text):
    """`00:12` / `1:02:03` / `42` -> seconds. 0 when unparseable."""
    text = (text or '').strip()
    match = ETA_RE.match(text)
    if match:
        hours, minutes, seconds = match.groups()
        return int(hours or 0) * 3600 + int(minutes) * 60 + int(seconds)
    try:
        return int(float(text))
    except ValueError:
        return 0


def hms(seconds):
    seconds = max(0, int(seconds))
    return '%d:%02d:%02d' % (seconds // 3600, (seconds % 3600) // 60, seconds % 60)


class SettingProperty(Enum):
    #: Where downloads land. Readable and editable from the settings page's
    #: JSON editor, which is how a user moves it off the boot volume.
    YTDLP_Dir = 1
    #: 0 = download to disk, 1 = stream through mpv.
    YTDLP_Mode = 2
    YTDLP_Mode_Download = 0
    YTDLP_Mode_Stream = 1


class YTDLPRenderer(MPVRenderer):

    def __init__(self, path="mpv"):
        super(YTDLPRenderer, self).__init__(path=path)
        self._lock = threading.Lock()
        self._proc = None
        self._thread = None
        #: Bumped on every cast/stop so a finishing download cannot overwrite
        #: the state of the one that replaced it.
        self._generation = 0
        #: Whether MPVRenderer.start() actually launched mpv -- see the header.
        self._mpv_started = False
        self.renderer_setting = YTDLPRendererSetting()

    # -- mode --------------------------------------------------------------

    def stream_mode(self):
        mode = Setting.get(SettingProperty.YTDLP_Mode,
                           SettingProperty.YTDLP_Mode_Download.value)
        return mode == SettingProperty.YTDLP_Mode_Stream.value

    # -- lifecycle ---------------------------------------------------------

    def start(self):
        self._mpv_started = self.stream_mode()
        if self._mpv_started:
            super(YTDLPRenderer, self).start()
        else:
            Renderer.start(self)
            logger.info('download mode: mpv stays closed until a cast arrives')

    def stop(self):
        self._cancel()
        if self._mpv_started:
            super(YTDLPRenderer, self).stop()
        else:
            Renderer.stop(self)
        self._mpv_started = False

    def build_mpv_params(self):
        params = super(YTDLPRenderer, self).build_mpv_params()
        if not self.stream_mode():
            return params
        binary = find_ytdlp()
        if binary is None:
            return params
        # Merge into the --script-opts the base class already set: a second
        # occurrence of a key/value-list option would drop the OSC options.
        option = 'ytdl_hook-ytdl_path=' + binary
        for index, param in enumerate(params):
            if param.startswith('--script-opts='):
                params[index] = param + ',' + option
                break
        else:
            params.append('--script-opts=' + option)
        return params

    # -- Renderer API ------------------------------------------------------

    def set_media_url(self, url, start="0"):
        if self.stream_mode():
            if find_ytdlp() is None:
                self._fail('未找到 yt-dlp，请先安装：brew install yt-dlp / pipx install yt-dlp')
                return
            # mpv's own ytdl hook resolves the page url and plays it while it
            # downloads; nothing is written to disk. The hook needs the binary
            # path, which build_mpv_params passes in.
            MPVRenderer.set_media_url(self, url, start)
            self.set_state('CurrentTrackTitle',
                           os.path.basename(url.split('?')[0]) or url)
            return
        if not url:
            return
        self._cancel()
        with self._lock:
            self._generation += 1
        self.set_state_url(url)
        self.set_state('CurrentTrackTitle', os.path.basename(url.split('?')[0]) or url)
        self.set_state_transport('PLAYING')
        self._thread = threading.Thread(target=self._download, args=(url,),
                                        daemon=True, name='YTDLP_DOWNLOAD')
        self._thread.start()

    def set_media_stop(self):
        if self.stream_mode():
            super(YTDLPRenderer, self).set_media_stop()
            return
        self._cancel()
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        if self.stream_mode():
            super(YTDLPRenderer, self).set_media_pause()
            return
        # A download cannot be paused. Saying so is better than reporting a
        # PAUSED_PLAYBACK the phone would then wait on forever.
        logger.info('pause ignored: yt-dlp is a downloader, not a player')

    def set_media_resume(self):
        if self.stream_mode():
            super(YTDLPRenderer, self).set_media_resume()
            return
        logger.info('resume ignored: yt-dlp is a downloader, not a player')

    # -- internals ---------------------------------------------------------

    def _cancel(self):
        with self._lock:
            self._generation += 1
            proc, self._proc = self._proc, None
        if proc is None or proc.poll() is not None:
            return
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        except OSError as e:
            logger.error('cannot stop yt-dlp: %s', e)

    def _fail(self, message):
        """Report a failure where a user will actually see it."""
        logger.error('yt-dlp downloader: %s', message)
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        cherrypy.engine.publish('app_notify', 'Macast', message)
        cherrypy.engine.publish('renderer_av_stop')

    def _download(self, url):
        with self._lock:
            generation = self._generation
        binary = find_ytdlp()
        if binary is None:
            self._fail('未找到 yt-dlp，请先安装：brew install yt-dlp / pipx install yt-dlp')
            return
        out_dir = Setting.get(SettingProperty.YTDLP_Dir, DEFAULT_DIR)
        try:
            os.makedirs(out_dir, exist_ok=True)
        except OSError as e:
            self._fail('无法创建下载目录 {}：{}'.format(out_dir, e))
            return
        cmd = [binary, '--newline', '--no-color', '--no-playlist',
               '-o', os.path.join(out_dir, '%(title).120B [%(id)s].%(ext)s'),
               url]
        logger.info('yt-dlp: %s', ' '.join(cmd))
        try:
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT,
                                    stdin=subprocess.DEVNULL,
                                    text=True, bufsize=1)
        except OSError as e:
            self._fail('无法启动 yt-dlp：{}'.format(e))
            return
        with self._lock:
            self._proc = proc
        started = time.time()
        title = ''
        last_error = ''
        for line in proc.stdout:
            line = line.rstrip()
            if not line:
                continue
            logger.debug('yt-dlp: %s', line)
            destination = DEST_RE.match(line)
            if destination:
                title = os.path.basename(destination.group('path'))
                self.set_state('CurrentTrackTitle', title)
                continue
            progress = PROGRESS_RE.match(line)
            if progress:
                # Position/duration double as the progress bar -- see the file
                # header. elapsed vs elapsed+ETA is what the percentage means.
                elapsed = time.time() - started
                eta = eta_seconds(progress.group('eta'))
                self.set_state_position(hms(elapsed))
                self.set_state_duration(hms(elapsed + eta))
                continue
            if line.startswith(('ERROR:', 'WARNING:')):
                last_error = line
        try:
            code = proc.wait()
        except OSError:
            code = -1
        with self._lock:
            if self._proc is proc:
                self._proc = None
            superseded = generation != self._generation
        if superseded:
            # A newer cast (or a stop) already replaced this download; its
            # state is the truth now, not ours.
            logger.info('download superseded, ignoring its exit code')
            return
        if code == 0:
            elapsed = time.time() - started
            self.set_state_position(hms(elapsed))
            self.set_state_duration(hms(elapsed))     # 100% on the progress bar
            self.set_state_transport('STOPPED')
            self.set_state('CurrentTrackTitle', title or url)
            logger.info('download finished: %s', title or url)
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '下载完成：{}'.format(title or url))
            cherrypy.engine.publish('renderer_av_stop')
        else:
            self._fail(last_error or 'yt-dlp 退出码 {}'.format(code))


class YTDLPRendererSetting(MPVRendererSetting):

    def __init__(self):
        super(YTDLPRendererSetting, self).__init__()
        self.mode_item = None

    def build_menu(self):
        stream = Setting.get(SettingProperty.YTDLP_Mode,
                             SettingProperty.YTDLP_Mode_Download.value) \
            == SettingProperty.YTDLP_Mode_Stream.value
        self.mode_item = MenuItem(
            'Stream while downloading',
            children=App.build_menu_item_group(['Download to disk', 'Stream'],
                                               self.on_mode_clicked))
        self.mode_item.items()[1 if stream else 0].checked = True
        return [
            MenuItem('yt-dlp v0.2', enabled=False),
            self.mode_item,
            MenuItem('Open Download Folder', self.open_folder),
        ]

    def on_mode_clicked(self, item):
        for child in self.mode_item.items():
            child.checked = False
        item.checked = True
        Setting.set(SettingProperty.YTDLP_Mode, item.data)
        # The mode decides whether mpv is running at all, so the renderer has to
        # be restarted for the change to apply.
        cherrypy.engine.publish('app_notify', 'yt-dlp',
                                '正在重载渲染器…', sound=False)
        cherrypy.engine.publish('reload_renderer')

    def open_folder(self, item):
        path = Setting.get(SettingProperty.YTDLP_Dir, DEFAULT_DIR)
        try:
            os.makedirs(path, exist_ok=True)
            if sys.platform == 'darwin':
                subprocess.Popen(['open', path])
            elif sys.platform == 'win32':
                os.startfile(path)          # noqa: only defined on Windows
            else:
                opener = shutil.which('xdg-open')
                if opener is None:
                    raise RuntimeError('xdg-open is not installed')
                subprocess.Popen([opener, path])
        except Exception as e:
            logger.error('cannot open download folder %s: %s', path, e)
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '无法打开下载目录：{}'.format(e))


if __name__ == '__main__':
    gui(YTDLPRenderer())
