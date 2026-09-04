# PotPlayer renderer for Macast.
#
# mpv exposes a clean IPC interface that Macast uses to drive playback and to
# report the current position/state back to the DLNA client. PotPlayer has no
# equivalent standard IPC, so this renderer drives it by:
#   * launching PotPlayer with the media URL as a command-line argument
#     (this is what makes the content actually play), and
#   * best-effort control through PotPlayer's remote-control HTTP endpoint
#     (pause / resume / volume / seek / stop) when it is enabled.
#
# State reporting to the DLNA client is therefore best-effort: without
# PotPlayer's remote control the phone can see the transport state (playing /
# stopped) but the position will not advance, because there is no IPC feed.

import os
import sys
import time
import logging
import gettext
import subprocess
import urllib.request
import urllib.error

from macast.renderer import Renderer, RendererSetting
from macast.utils import Setting

logger = logging.getLogger("PotPlayerRenderer")
logger.setLevel(logging.INFO)

# PotPlayer remote-control default port (enable via F5 -> 播放/网络 -> 远程控制).
REMOTE_PORT = 13579

_POTPLAYER_CANDIDATES = [
    r"D:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
    r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
    r"E:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
    r"D:\Program Files\PotPlayer\PotPlayerMini64.exe",
    r"C:\Program Files\PotPlayer\PotPlayerMini64.exe",
    r"C:\Program Files (x86)\DAUM\PotPlayer\PotPlayerMini.exe",
    r"C:\Program Files (x86)\PotPlayer\PotPlayerMini.exe",
]


def find_potplayer():
    """Return the path to PotPlayer, or None if it is not installed."""
    import shutil
    for cand in _POTPLAYER_CANDIDATES:
        if os.path.isfile(cand):
            return cand
    return (shutil.which("PotPlayerMini64") or shutil.which("PotPlayerMini"))


class PotPlayerRenderer(Renderer):
    """A Macast renderer backed by PotPlayer."""

    def __init__(self, lang=gettext.gettext, path=None):
        super(PotPlayerRenderer, self).__init__(lang)
        self.path = path or find_potplayer()
        self.proc = None
        self.playing = False
        self.paused = False
        self._title = ""

    # -- helpers ----------------------------------------------------------

    def _remote(self, path):
        """Send one command to PotPlayer's remote control. Best-effort."""
        if not self.path:
            return False
        try:
            url = "http://127.0.0.1:{}/{}".format(REMOTE_PORT, path.lstrip("/"))
            urllib.request.urlopen(url, timeout=1.5).read()
            return True
        except Exception:
            return False

    def _kill(self):
        if self.proc is not None:
            try:
                self.proc.terminate()
            except Exception:
                pass
            try:
                self.proc.kill()
            except Exception:
                pass
        self.proc = None

    # -- lifecycle --------------------------------------------------------

    def start(self):
        super(PotPlayerRenderer, self).start()

    def stop(self):
        super(PotPlayerRenderer, self).stop()
        self.set_media_stop()

    # -- media control (called by the DLNA protocol) ----------------------

    def set_media_url(self, url, start="0"):
        if not self.path or not os.path.isfile(self.path):
            logger.error("PotPlayer not found: %s", self.path)
            cherrypy_engine_publish("app_notify", "Macast", "PotPlayer not found")
            return
        self.playing = True
        self.paused = False
        # Build the PotPlayer command line.  PotPlayerMini64 <url> starts
        # playing the given URI; pass start via PotPlayer's --start option.
        cmd = [self.path]
        if start and start != "0":
            cmd += ["/start={}".format(start)]
        cmd.append(url)
        logger.info("PotPlayer launch: %s %s", self.path, url)
        try:
            self.proc = subprocess.Popen(cmd)
        except Exception as e:
            logger.error("PotPlayer launch failed: %s", e)
            self.playing = False
            return
        self.set_state('TransportState', 'PLAYING')
        self.set_state_url(url)
        if self._title:
            self.set_state('CurrentTrackTitle', self._title)
        cherrypy_engine_publish("renderer_av_uri", url)

    def set_media_stop(self):
        self.playing = False
        self.paused = False
        self._remote("cmd=stop")
        self._kill()
        self.set_state('TransportState', 'STOPPED')
        cherrypy_engine_publish("renderer_av_stop")

    def set_media_pause(self):
        self.paused = True
        self._remote("cmd=pause")
        self.set_state('TransportState', 'PAUSED_PLAYBACK')

    def set_media_resume(self):
        self.paused = False
        # A second launch of the same instance can be used to resume, but the
        # cleanest way is the remote control; fall back to re-launching.
        if not self._remote("cmd=play") and self.proc is not None:
            self._remote("cmd=play")
        self.set_state('TransportState', 'PLAYING')
        self.set_media_text('Resume')

    def set_media_volume(self, data):
        if data is not None:
            self._remote("cmd=vol&val={}".format(int(data)))
            self.set_state_volume(int(data))

    def set_media_mute(self, data):
        if data:
            self._remote("cmd=vol&val=0")
        else:
            self._remote("cmd=vol&val=50")
        self.set_state_mute(bool(data))

    def set_media_position(self, data):
        self._remote("cmd=seek&val={}".format(data))

    def set_media_title(self, data):
        self._title = data
        self.set_state('CurrentTrackTitle', data)

    def set_media_speed(self, data=None):
        pass

    def set_media_text(self, data, duration=1000):
        pass

    def set_media_sub_file(self, data):
        pass

    def set_media_sub_show(self, data):
        pass


def cherrypy_engine_publish(event, *args):
    """Publish a cherrypy engine event without importing cherrypy at module
    import time (keeps this module import-light)."""
    try:
        import cherrypy
        cherrypy.engine.publish(event, *args)
    except Exception:
        pass


class _PlayerSize:
    # Kept for parity with MPVRenderer interface expectations.
    pass
