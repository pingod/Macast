# Copyright (c) 2026 by pingod. All Rights Reserved.
# Automation hooks for Macast
#
# Macast Metadata
# <macast.title>Automation Hooks</macast.title>
# <macast.renderer>HooksRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.role>addon</macast.role>
# <macast.desc>在投屏开始、暂停、继续、停止时执行你自己的命令——切换音频输出、暂停音乐、调暗灯光、发通知都行。播放仍使用内置 mpv。</macast.desc>
#
# Configure the commands in the settings JSON (menu: "Edit Hooks"):
#
#   Hook_On_Cast    a command run when a new url is cast
#   Hook_On_Pause   run on pause
#   Hook_On_Resume  run on resume
#   Hook_On_Stop    run on stop (including the stop that precedes a new cast)
#
# Each one is a shell command and gets the same three environment variables:
#
#   MACAST_EVENT   cast / pause / resume / stop
#   MACAST_URL     the url being cast ('' for pause/stop)
#   MACAST_TITLE   the title the protocol reports, when it has one
#
# Notes for whoever touches this next:
#   * hooks run through the shell on purpose -- that is the whole point of a
#     hook ("notify-send ... && osascript ..."), and the command comes from the
#     user's own settings file, so it carries the same trust as their shell
#     profile. Never build one out of a *network* string (the url is passed as
#     an environment variable, not interpolated into the command).
#   * the hook is spawned, not waited on: a slow command must not stall the
#     CherryPy worker that is answering the phone.

import os
import subprocess
import sys
import logging
from enum import Enum

import cherrypy

from macast import Setting, MenuItem, gui
from macast_renderer.mpv import MPVRenderer, MPVRendererSetting

logger = logging.getLogger("Hooks")
logger.setLevel(logging.INFO)

#: setting key -> event name handed to the hook
HOOKS = (
    ('Hook_On_Cast', 'cast'),
    ('Hook_On_Pause', 'pause'),
    ('Hook_On_Resume', 'resume'),
    ('Hook_On_Stop', 'stop'),
)


class SettingProperty(Enum):
    Hook_On_Cast = 1
    Hook_On_Pause = 2
    Hook_On_Resume = 3
    Hook_On_Stop = 4


HOOK_PROPS = {
    'cast': SettingProperty.Hook_On_Cast,
    'pause': SettingProperty.Hook_On_Pause,
    'resume': SettingProperty.Hook_On_Resume,
    'stop': SettingProperty.Hook_On_Stop,
}


def hook_command(event):
    """The configured command for `event`, or ''.

    Reading with an empty default *creates* the key in the settings file the
    first time the menu is opened. That is deliberate here: a menu bar item
    cannot ask for text input, so the keys showing up in the settings JSON (and
    on the settings page's JSON editor) is how the feature is discovered.
    """
    prop = HOOK_PROPS.get(event)
    if prop is None:
        return ''
    return Setting.get(prop, '') or ''


def run_hook(event, url='', title=''):
    """Run the hook for `event` without blocking the caller."""
    command = hook_command(event)
    if not command:
        return False
    env = dict(os.environ)
    env.update({'MACAST_EVENT': event, 'MACAST_URL': url or '',
                'MACAST_TITLE': title or ''})
    try:
        proc = subprocess.Popen(command, shell=True, env=env,
                                stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL,
                                stdin=subprocess.DEVNULL)
    except OSError as e:
        logger.error('cannot run the %s hook: %s', event, e)
        return False
    logger.info('%s hook started (pid %s): %s', event, proc.pid, command)
    return True


class HooksRenderer(MPVRenderer):
    """mpv playback, plus a command of your own at every transition."""

    def __init__(self, path="mpv"):
        super(HooksRenderer, self).__init__(path=path)
        self.renderer_setting = HooksRendererSetting()
        self._current_url = ''

    def current_title(self):
        try:
            return self.protocol.get_state_title()
        except Exception:
            return ''

    def set_media_url(self, url, start="0"):
        self._current_url = url or ''
        super(HooksRenderer, self).set_media_url(url, start)
        run_hook('cast', self._current_url, self.current_title())

    def set_media_pause(self):
        super(HooksRenderer, self).set_media_pause()
        run_hook('pause', self._current_url, self.current_title())

    def set_media_resume(self):
        super(HooksRenderer, self).set_media_resume()
        run_hook('resume', self._current_url, self.current_title())

    def set_media_stop(self):
        super(HooksRenderer, self).set_media_stop()
        # Report the url we were playing: by the time the hook runs, a new cast
        # may already have replaced it.
        run_hook('stop', self._current_url, self.current_title())
        self._current_url = ''


class HooksRendererSetting(MPVRendererSetting):

    def build_menu(self):
        configured = sum(1 for key, _ in HOOKS if Setting.get(SettingProperty[key], ''))
        return [
            MenuItem('Hooks: {}/{} configured'.format(configured, len(HOOKS)),
                     enabled=False),
            MenuItem('Edit Hooks (opens settings JSON)', self.open_settings),
        ] + super(HooksRendererSetting, self).build_menu()

    def open_settings(self, item):
        """Open the settings file, which is where the hooks live.

        There is no text input in a menu bar item, so the documented way to set
        a hook is this file (or the settings page's JSON editor).
        """
        path = Setting.setting_path
        try:
            if sys.platform == 'darwin':
                subprocess.Popen(['open', path])
            elif sys.platform == 'win32':
                subprocess.Popen(['notepad.exe', path])
            else:
                opener = 'xdg-open'
                subprocess.Popen([opener, path])
        except OSError as e:
            logger.error('cannot open %s: %s', path, e)
            cherrypy.engine.publish('app_notify', 'Macast',
                                    '无法打开设置文件：{}'.format(e))


if __name__ == '__main__':
    gui(HooksRenderer())
