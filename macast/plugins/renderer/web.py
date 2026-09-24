# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Using web browser as DLNA media renderer
# This plugin can be used to download media files or get some m3u8 played
#
# Macast Metadata
# <macast.title>Web Renderer</macast.title>
# <macast.renderer>WebRenderer</macast.renderer>
# <macast.platform>darwin,linux,win32</macast.platform>
# <macast.version>0.3</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod（原作者 xfangfang）</macast.author>
# <macast.desc>用浏览器作为 DLNA 媒体渲染器，可下载媒体文件或播放部分 m3u8 流。需在系统默认浏览器可打开的地址。</macast.desc>
#
# Upstream notes for this version:
#   * The position thread started in __init__ and ticked forever, so the
#     reported position kept climbing from 0:00:00 with nothing playing.
#   * pause/resume did not exist, so the transport state never left PLAYING.
#   * `'tmp' in v` over every environment value raised on a non-string value.
#   * `xdg-open` was spawned without checking it exists.
#   * start() / stop() used print() and published renderer_av_stop twice.


import sys
import time
import shutil
import threading
import pyperclip
import cherrypy
import subprocess
import webbrowser
from enum import Enum
from macast import gui, Setting, MenuItem
from macast.renderer import Renderer, RendererSetting


class WebRenderer(Renderer):

    def __init__(self):
        super(WebRenderer, self).__init__()
        self.start_position = 0
        self._playing = False
        self.position_thread_running = False
        self.position_thread = threading.Thread(target=self.position_tick,
                                                daemon=True,
                                                name="WEB_POSITION")
        self.position_thread.start()
        self.renderer_setting = WebRendererSetting()

    def position_tick(self):
        """Tick once a second, but only count while something is playing."""
        while self.position_thread_running:
            time.sleep(1)
            if not self._playing:
                continue
            self.start_position += 1
            sec = self.start_position
            position = '%d:%02d:%02d' % (sec // 3600, (sec % 3600) // 60, sec % 60)
            self.set_state_position(position)

    def set_media_stop(self):
        self._playing = False
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        self._playing = False
        self.set_state_transport('PAUSED_PLAYBACK')

    def set_media_resume(self):
        self._playing = True
        self.set_state_transport('PLAYING')

    def set_media_url(self, url, start="0"):
        """`:param start:` accepted for interface compatibility, then ignored.

        The browser is handed the URL and seeks itself; the upstream signature
        defaulted to the int 0 where the protocol passes a string.
        """
        self.set_media_stop()
        self.start_position = 0
        if self.open_browser(url):
            self._playing = True
            self.set_state_transport("PLAYING")
            cherrypy.engine.publish('renderer_av_uri', url)

    def get_env(self):
        # https://github.com/pyinstaller/pyinstaller/issues/3668#issuecomment-742547785
        env = Setting.get_system_env()
        toDelete = []
        for (k, v) in env.items():
            # Only string values can be searched: a non-str value here (or an
            # env var that is not a path at all) used to raise mid-iteration.
            if k != 'PATH' and isinstance(v, str) and 'tmp' in v:
                toDelete.append(k)
        for k in toDelete:
            env.pop(k, None)
        return env

    def open_browser(self, url):
        """Open `url` in the user's browser. Returns True on success."""
        if self.renderer_setting.setting_autocopy:
            try:
                pyperclip.copy(url)
            except Exception as e:
                # No clipboard on a headless box; not worth refusing to play.
                cherrypy.engine.publish('app_notify', 'Macast',
                                        'Could not copy the url: {}'.format(e))
        try:
            if sys.platform == 'darwin':
                subprocess.Popen(['open', url])
            elif sys.platform == 'win32':
                webbrowser.open(url)
            else:
                opener = shutil.which('xdg-open')
                if opener is None:
                    raise RuntimeError('xdg-open is not installed')
                subprocess.Popen([opener, url], env=self.get_env())
            return True
        except Exception as e:
            # Without this the sender was told PLAYING for a browser that
            # never opened, and the failure was completely invisible.
            self.set_state_transport_error()
            cherrypy.engine.publish('app_notify', 'Macast',
                                    'Could not open a browser: {}'.format(e))
            return False

    def stop(self):
        super(WebRenderer, self).stop()
        self.position_thread_running = False
        self.set_media_stop()

    def start(self):
        super(WebRenderer, self).start()
        self.position_thread_running = True
        if not self.position_thread.is_alive():
            self.position_thread = threading.Thread(target=self.position_tick,
                                                    daemon=True,
                                                    name="WEB_POSITION")
            self.position_thread.start()


class SettingProperty(Enum):
    Web_AutoCopy = 1
    Web_AutoCopy_Disable = 0
    Web_AutoCopy_Enable = 1


class WebRendererSetting(RendererSetting):
    def __init__(self):
        # Setting.load() is not needed: Setting.get() loads on first use, and
        # calling it here raced the app's own settings load while the menu was
        # being assembled.
        self.webAutoCopyItem = None
        self.setting_autocopy = Setting.get(SettingProperty.Web_AutoCopy,
                                            SettingProperty.Web_AutoCopy_Enable.value)

    def build_menu(self):
        self.webAutoCopyItem = MenuItem("Auto Copy Url",
                                        self.on_autocopy_clicked,
                                        checked=self.setting_autocopy)
        return [
            self.webAutoCopyItem,
        ]

    def on_autocopy_clicked(self, item):
        item.checked = not item.checked
        self.setting_autocopy = 1 if item.checked else 0
        Setting.set(SettingProperty.Web_AutoCopy, self.setting_autocopy)


if __name__ == '__main__':
    gui(WebRenderer())
