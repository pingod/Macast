# Copyright (c) 2026 by pingod. All Rights Reserved.
# Floating player for Macast (plus an experimental wallpaper mode)
#
# Macast Metadata
# <macast.title>Floating Player</macast.title>
# <macast.renderer>FloatingRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.1</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.role>addon</macast.role>
# <macast.desc>一个常驻桌角、置顶的小 mpv 窗口，方便工作时瞄一眼内容。附带一个实验性的桌面壁纸模式。</macast.desc>
#
# Why: the built-in window is sized from the global Player Size setting, which
# is a full-size viewing window. "Small" in that menu is still ~30% of the
# screen in the middle of nowhere. This renderer is the other use case: a
# caption-sized window docked in a corner, which you leave open for an hour.
#
# The wallpaper mode is opt-in and off by default: it needs `--ontop-level=desktop`
# (newer mpv only) and even then whether it sits under the desktop icons depends
# on the OS. When the running mpv does not support the option we fall back to a
# full-screen window instead of refusing to start -- see `mpv_supports`.

import subprocess
from enum import Enum

import cherrypy

from macast import Setting, MenuItem, gui
from macast.gui import App
from macast_renderer.mpv import MPVRenderer, MPVRendererSetting

#: (menu label, --autofit percentage)
SIZES = (('Small', 16), ('Medium', 26), ('Large', 38))

#: Options that decide where and how big the window is. Whatever the inherited
#: params say about them is dropped first: otherwise the global Player Size /
#: Player Ontop settings would fight the choice made here.
CONFLICTING = ('--ontop', '--fullscreen')

#: (executable, option) -> bool, filled in on first use.
_SUPPORTS = {}


def mpv_supports(path, option):
    """Whether this mpv accepts `option` (cached per process).

    Worth the subprocess: an unknown option makes mpv refuse to start, and the
    launcher's response to "mpv never connected" is to retry three times and
    then stop the service. Probing once is much cheaper than that failure.
    """
    key = (path, option)
    if key not in _SUPPORTS:
        try:
            result = subprocess.run([path, '--no-config', option, '--version'],
                                    stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, timeout=5)
            _SUPPORTS[key] = result.returncode == 0
        except Exception:
            _SUPPORTS[key] = False
    return _SUPPORTS[key]


class SettingProperty(Enum):
    Floating_Mode = 1
    Floating_Mode_Window = 0
    Floating_Mode_Wallpaper = 1
    #: Index into SIZES.
    Floating_Size = 2


class FloatingRenderer(MPVRenderer):

    def __init__(self, path="mpv"):
        super(FloatingRenderer, self).__init__(path=path)
        self.renderer_setting = FloatingRendererSetting()

    def floating_mode(self):
        mode = Setting.get(SettingProperty.Floating_Mode,
                           SettingProperty.Floating_Mode_Window.value)
        return mode if mode in (SettingProperty.Floating_Mode_Window.value,
                                SettingProperty.Floating_Mode_Wallpaper.value) \
            else SettingProperty.Floating_Mode_Window.value

    def floating_size(self):
        index = Setting.get(SettingProperty.Floating_Size, 1)
        if not isinstance(index, int) or not 0 <= index < len(SIZES):
            index = 1
        return index

    def build_mpv_params(self):
        params = super(FloatingRenderer, self).build_mpv_params()
        params = [p for p in params
                  if p not in CONFLICTING
                  and not p.startswith(('--geometry=', '--autofit',
                                        '--ontop-level='))]
        if self.floating_mode() == SettingProperty.Floating_Mode_Wallpaper.value \
                and mpv_supports(self.path, '--ontop-level=desktop'):
            # Bottom of the window stack: normal windows cover it, which is as
            # close to a wallpaper as mpv gets. `--no-osc` and no input bindings
            # because there is nothing sane to click on a wallpaper.
            params += [
                '--geometry=100%:100%',
                '--no-border', '--no-osc', '--no-input-default-bindings',
                '--ontop', '--ontop-level=desktop',
                '--cursor-autohide=always', '--no-window-dragging',
            ]
        else:
            if self.floating_mode() == SettingProperty.Floating_Mode_Wallpaper.value:
                cherrypy.engine.publish(
                    'app_notify', 'Macast',
                    '当前 mpv 不支持桌面层，已改用全屏置顶窗口', sound=False)
            params += [
                '--autofit={}%'.format(SIZES[self.floating_size()][1]),
                '--geometry=98%:3%',
                '--ontop', '--no-border',
            ]
        return params


class FloatingRendererSetting(MPVRendererSetting):

    def __init__(self):
        super(FloatingRendererSetting, self).__init__()
        self.size_item = None
        self.mode_item = None

    def build_menu(self):
        # Read the settings, not the live renderer: the renderer reads exactly
        # these keys, so there is no second source of truth to keep in sync
        # (and the menu can be built before any renderer instance exists).
        mode = Setting.get(SettingProperty.Floating_Mode,
                           SettingProperty.Floating_Mode_Window.value)
        size = Setting.get(SettingProperty.Floating_Size, 1)
        if not isinstance(size, int) or not 0 <= size < len(SIZES):
            size = 1
        self.mode_item = MenuItem('Wallpaper Mode (experimental)',
                                  self.on_mode_toggled,
                                  checked=(mode == SettingProperty.Floating_Mode_Wallpaper.value))
        self.size_item = MenuItem(
            'Floating Size',
            children=App.build_menu_item_group([name for name, _ in SIZES],
                                               self.on_size_clicked))
        self.size_item.items()[size].checked = True
        return [
            MenuItem('Floating Player v0.1', enabled=False),
            self.mode_item,
            self.size_item,
        ] + super(FloatingRendererSetting, self).build_menu()

    def on_size_clicked(self, item):
        for child in self.size_item.items():
            child.checked = False
        item.checked = True
        Setting.set(SettingProperty.Floating_Size, item.data)
        self.reload_player()

    def on_mode_toggled(self, item):
        item.checked = not item.checked
        Setting.set(SettingProperty.Floating_Mode,
                    1 if item.checked else 0)
        self.reload_player()

    def reload_player(self):
        cherrypy.engine.publish('app_notify', 'Floating Player',
                                '正在重载播放器…', sound=False)
        cherrypy.engine.publish('reload_renderer')


if __name__ == '__main__':
    gui(FloatingRenderer())
