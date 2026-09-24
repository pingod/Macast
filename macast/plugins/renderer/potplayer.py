# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Using potplayer as DLNA media renderer
#
# Macast Metadata
# <macast.title>PotPlayer Renderer</macast.title>
# <macast.renderer>PotplayerRenderer</macast.renderer>
# <macast.platform>win32</macast.platform>
# <macast.version>0.5</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod（原作者 xfangfang）</macast.author>
# <macast.desc>用 PotPlayer 作为 Macast 的 DLNA 播放器。仅实现播放与停止，需本机已安装 PotPlayer。</macast.desc>
#
# Upstream note: the original header closed its renderer and platform tags
# with the WRONG closing name -- it reused the title tag for all three -- so
# the class name and the platform were silently lost. That survived because the
# loader used to fall back to the file name; now that the manifest decides
# whether a plugin is imported at all, a typo there is not survivable. Fixed.


import os
import time
import logging
import cherrypy
import threading
import subprocess
from enum import Enum
from contextlib import contextmanager

from macast import gui, Setting
from macast.renderer import Renderer
from macast.utils import SETTING_DIR

# win32 is only needed to read PotPlayer's install path out of the registry and
# to hide the player's console window. It is imported lazily -- never at module
# level -- because the bundle ships this plugin on every platform and the
# settings page is supposed to *list* it on the others (greyed out) rather than
# die importing it.
try:
    import win32api
    import win32con
except ImportError:  # pragma: no cover - not Windows
    win32api = None
    win32con = None

POTPLAYER_PATH_64 = r'C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe'
POTPLAYER_PATH_32 = r'C:\Program Files (x86)\DAUM\PotPlayer\PotPlayerMini.exe'

logger = logging.getLogger("PotPlayer")
subtitle = os.path.join(SETTING_DIR, r"macast.ass")

# Only defined on Windows; referencing it unconditionally made this a
# Windows-only module in a way that was invisible until the plugin was run
# anywhere else.
NO_WINDOW = getattr(subprocess, 'CREATE_NO_WINDOW', 0)


class SettingProperty(Enum):
    Potplayer_Path = 0


@contextmanager
def win32_reg_open(key, access=None, hive=None):
    if win32api is None:
        raise RuntimeError("PotPlayer plugin requires the pywin32 package")
    if access is None:
        access = win32con.KEY_SET_VALUE
    if hive is None:
        hive = win32con.HKEY_CURRENT_USER
    handle = win32api.RegOpenKey(
        hive,
        key,
        0,
        access)
    try:
        yield handle
    finally:
        # The original closed the key on the success path only, so a failure
        # inside the `with` body leaked a registry handle on every attempt.
        win32api.RegCloseKey(handle)


def get_potplayer_path():
    """Locate PotPlayer, or return None.

    Order: 64-bit registry -> 32-bit registry -> the path the user configured
    -> the two default install locations.
    """
    for key in (r'Software\DAUM\PotPlayer64', r'Software\DAUM\PotPlayer'):
        try:
            with win32_reg_open(key, win32con.KEY_QUERY_VALUE) as handle:
                path = win32api.RegQueryValueEx(handle, 'ProgramPath')[0]
            if path and os.path.exists(path):
                return path
        except Exception:
            # Absent key, missing pywin32, or a path that no longer exists:
            # all mean "ask the next source", not "fail".
            continue

    # Setting.has() first: Setting.get() with a None default *writes* that
    # None into the settings file, which is how a missing key used to turn
    # into a persisted JSON null.
    if Setting.has(SettingProperty.Potplayer_Path):
        path = Setting.get(SettingProperty.Potplayer_Path, None)
        if path and os.path.exists(path):
            return path

    for candidate in (POTPLAYER_PATH_64, POTPLAYER_PATH_32):
        if os.path.exists(candidate):
            return candidate
    return None

class PotplayerRenderer(Renderer):
    def __init__(self):
        super(PotplayerRenderer, self).__init__()
        self.pid = None
        self.start_position = 0
        self.position_thread_running = True
        self.position_thread = threading.Thread(target=self.position_tick, daemon=True)
        self.position_thread.start()
        # a thread is started here to increase the playback position once per second
        # to simulate that the media is playing.

    def position_tick(self):
        while self.position_thread_running:
            time.sleep(1)
            self.start_position += 1
            sec = self.start_position
            position = '%d:%02d:%02d' % (sec // 3600, (sec % 3600) // 60, sec % 60)
            self.set_state_position(position)

    def set_media_stop(self):
        if self.pid is not None:
            subprocess.Popen(['taskkill', '/f', '/pid', str(self.pid)],
                             creationflags=NO_WINDOW).communicate()
        try:
            os.remove(subtitle)
        except OSError:
            pass
        self.pid = None
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def start_player(self, url):
        path = get_potplayer_path()
        if path is None:
            subprocess.Popen(['notepad.exe', Setting.setting_path],
                             creationflags=NO_WINDOW)
            cherrypy.engine.publish('app_notify', "Error", "You should modify 'Potplayer_Path' to your local potplayer and restart Macast.")
            logger.error('cannot find potplayer, tried: %s, %s',
                         POTPLAYER_PATH_64, POTPLAYER_PATH_32)
            return
        try:
            # An argv LIST, not an f-string command line. With shell=False the
            # string form is handed to CreateProcess verbatim, so the cast URL
            # -- which arrives from the network and is not ours -- could close
            # the quote and append extra PotPlayer switches of its choosing.
            # A list makes each element one argument, whatever it contains.
            proc = subprocess.Popen(
                [path, url, '/autoplay', '/sub={}'.format(subtitle)],
                creationflags=NO_WINDOW)
            self.pid = proc.pid
            # wait potplayer to stop
            proc.communicate()
            logger.info('Potplayer stopped')
        except Exception as e:
            logger.exception("cannot start potplayer", exc_info=e)
            self.set_media_stop()
            cherrypy.engine.publish('app_notify', "Error", str(e))

    def set_media_url(self, url, start="0"):
        """`:param start:` kept for interface compatibility.

        PotPlayer is handed a URL, not a seek offset, so the value is accepted
        and ignored -- but it must accept it: the DLNA protocol passes a
        string, and the upstream signature defaulted to the int 0.
        """
        self.set_media_stop()
        self.start_position = 0
        threading.Thread(target=self.start_player, daemon=True, kwargs={'url': url}).start()
        self.set_state_transport("PLAYING")
        cherrypy.engine.publish('renderer_av_uri', url)

    def stop(self):
        super(PotplayerRenderer, self).stop()
        self.position_thread_running = False
        self.set_media_stop()
        logger.info("PotPlayer stop")

    def start(self):
        super(PotplayerRenderer, self).start()
        logger.info("PotPlayer start")


if __name__ == '__main__':
    gui(PotplayerRenderer())
    # or using cli to disable taskbar menu
    # from macast import cli
    # cli(PotplayerRenderer())
