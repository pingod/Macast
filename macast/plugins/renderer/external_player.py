# Copyright (c) 2026 by pingod. All Rights Reserved.
# External Player for Macast -- VLC / MPC-BE / MPC-HC / mpv.net
#
# Macast Metadata
# <macast.title>External Player (VLC / MPC-BE / mpv.net)</macast.title>
# <macast.renderer>ExternalPlayerRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.desc>Play the cast url in a player you already use -- VLC, MPC-BE/HC or mpv.net -- instead of the built-in mpv. Pick it from the menu bar.</macast.desc>
#
# Why: the bundled mpv is a good default, but people keep their own player for a
# reason -- custom shortcuts, OSC plugins, hardware-decoding quirks, a remote
# control app, or simply a window they already know. This renderer hands the url
# to that player instead.
#
# Notes for whoever touches this next:
#   * the player is launched with an argv LIST, never a command string: the url
#     arrives from the network, and a string command would let it append
#     switches of its own (same reasoning as the PotPlayer plugin).
#   * stop() kills the process we started. A player that hands the file to an
#     already-running instance (VLC in one-instance mode) keeps playing -- we
#     will not kill unrelated windows to hide that.
#   * position is simulated (one tick per second), like the PotPlayer plugin:
#     none of these players reports progress over anything we could poll
#     without asking the user to reconfigure their own setup.

import os
import shutil
import subprocess
import sys
import threading
import time
import logging
from enum import Enum

import cherrypy

from macast import Setting, MenuItem, gui
from macast.renderer import Renderer, RendererSetting

logger = logging.getLogger("ExternalPlayer")
logger.setLevel(logging.INFO)

#: name -> how to find it and how to call it, in menu order.
#: `args` is everything after the executable; every player here accepts the url
#: as the last argument.
PLAYERS = {
    'VLC': {
        'names': ('vlc',),
        'paths': {
            'darwin': ('/Applications/VLC.app/Contents/MacOS/VLC',),
            'win32': (r'C:\Program Files\VideoLAN\VLC\vlc.exe',
                      r'C:\Program Files (x86)\VideoLAN\VLC\vlc.exe'),
            'linux': ('/usr/bin/vlc', '/snap/bin/vlc'),
        },
        'args': lambda url: [url],
    },
    'MPC-BE / MPC-HC': {
        'names': ('mpc-be64', 'mpc-be', 'mpc-hc64', 'mpc-hc'),
        'paths': {
            'win32': (r'C:\Program Files\MPC-BE x64\mpc-be64.exe',
                      r'C:\Program Files\MPC-BE\mpc-be.exe',
                      r'C:\Program Files\MPC-HC\mpc-hc64.exe'),
        },
        'args': lambda url: ['/play', '/close', url],
    },
    'mpv.net': {
        'names': ('mpvnet',),
        'paths': {
            'win32': (r'C:\Program Files\mpv.net\mpvnet.exe',
                      r'C:\Program Files (x86)\mpv.net\mpvnet.exe'),
            'darwin': ('/Applications/mpv.net.app/Contents/MacOS/mpvnet',),
        },
        'args': lambda url: [url],
    },
}


class SettingProperty(Enum):
    #: Which entry of PLAYERS to use. Editable from the settings JSON editor.
    External_Player = 1
    #: Explicit executable, used instead of the auto-detected one.
    External_Player_Path = 2


def find_player(entry):
    """Absolute path of an installed player, or None.

    PATH first, then the usual install locations: a GUI app started from Finder
    or the Dock does not inherit the shell's PATH, so a brew / Program Files
    install would look missing otherwise.
    """
    for name in entry['names']:
        found = shutil.which(name)
        if found:
            return found
    for candidate in entry['paths'].get(sys.platform, ()):
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def installed_players():
    """[(name, path)] for everything launchable, in table order."""
    found = []
    for name, entry in PLAYERS.items():
        path = find_player(entry)
        if path:
            found.append((name, path))
    return found


def resolve_player():
    """(name, path) for the configured player, or (name, None) when missing.

    The name comes from a settings file the user can edit by hand, so an unknown
    value falls back instead of raising inside a CherryPy worker.
    """
    name = Setting.get(SettingProperty.External_Player, '')
    if name not in PLAYERS:
        available = installed_players()
        name = available[0][0] if available else next(iter(PLAYERS))
    override = Setting.get(SettingProperty.External_Player_Path, '')
    if override and os.path.isfile(override):
        return name, override
    return name, find_player(PLAYERS[name])


class ExternalPlayerRenderer(Renderer):

    def __init__(self):
        super(ExternalPlayerRenderer, self).__init__()
        self._lock = threading.Lock()
        self._proc = None
        self._playing = False
        self._position = 0
        self._ticker = threading.Thread(target=self._tick, daemon=True,
                                        name='EXTERNAL_POSITION')
        self._ticker.start()
        self.renderer_setting = ExternalPlayerSetting()

    # -- simulated progress ------------------------------------------------

    def _tick(self):
        while True:
            time.sleep(1)
            if not self._playing:
                continue
            self._position += 1
            sec = self._position
            self.set_state_position('%d:%02d:%02d' % (sec // 3600,
                                                      (sec % 3600) // 60,
                                                      sec % 60))

    # -- Renderer API ------------------------------------------------------

    def set_media_url(self, url, start="0"):
        """`:param start:` accepted for interface compatibility, then ignored.

        These players take a url and seek themselves; the protocol passes the
        offset as a string, never as the int the upstream signature implied.
        """
        self.set_media_stop()
        if not url:
            return
        name, path = resolve_player()
        if path is None:
            self._fail('未找到外部播放器「{}」：请在菜单里换一个，'
                       '或在设置里填 External_Player_Path'.format(name))
            return
        argv = [path] + list(PLAYERS[name]['args'](url))
        logger.info('launching %s', argv)
        try:
            with self._lock:
                self._proc = subprocess.Popen(
                    argv,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    stdin=subprocess.DEVNULL)
        except OSError as e:
            self._fail('启动 {} 失败：{}'.format(name, e))
            return
        self._position = 0
        self._playing = True
        self.set_state_transport('PLAYING')
        cherrypy.engine.publish('renderer_av_uri', url)

    def set_media_stop(self):
        self._playing = False
        with self._lock:
            proc, self._proc = self._proc, None
        if proc is not None and proc.poll() is None:
            try:
                proc.terminate()
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
            except OSError as e:
                logger.error('cannot stop the external player: %s', e)
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        # No portable way to pause these players from the outside without
        # asking the user to enable an RC interface first, and reporting a
        # pause we did not perform would lie to the phone.
        logger.info('pause ignored: the external player is not under our control')

    def set_media_resume(self):
        logger.info('resume ignored: the external player is not under our control')

    def stop(self):
        self._playing = False
        self.set_media_stop()
        super(ExternalPlayerRenderer, self).stop()

    def _fail(self, message):
        logger.error(message)
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        cherrypy.engine.publish('app_notify', 'Macast', message)


class ExternalPlayerSetting(RendererSetting):

    def __init__(self):
        self.player_item = None

    def build_menu(self):
        chosen, _ = resolve_player()
        children = []
        for name, entry in PLAYERS.items():
            children.append(MenuItem(name, self.on_player_clicked,
                                     checked=(name == chosen),
                                     enabled=find_player(entry) is not None,
                                     data=name))
        self.player_item = MenuItem('Player', children=children)
        return [
            MenuItem('External Player v0.1', enabled=False),
            self.player_item,
        ]

    def on_player_clicked(self, item):
        for child in (self.player_item.items() if self.player_item else ()):
            child.checked = False
        item.checked = True
        Setting.set(SettingProperty.External_Player, item.data)
        # Nothing is running for this player yet; a reload just re-reads the
        # choice, so the next cast goes to the player that is now ticked.
        cherrypy.engine.publish('reload_renderer')


if __name__ == '__main__':
    gui(ExternalPlayerRenderer())
