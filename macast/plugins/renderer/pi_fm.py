# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Using pi_fm_rds as DLNA media renderer
# https://github.com/ChristopheJacquet/PiFmRds
#
# Macast Metadata
# <macast.title>PIFMRDS Renderer</macast.title>
# <macast.renderer>PIFMRenderer</macast.renderer>
# <macast.platform>linux</macast.platform>
# <macast.version>0.3</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod（原作者 xfangfang）</macast.author>
# <macast.desc>把 Macast 收到的媒体通过树莓派的 FM 射频播出去（PIFMRDS）。仅适用于树莓派，需本机安装 PIFMRDS。</macast.desc>
#
# Upstream notes for this version:
#   * `__init__` spawned `sudo pi_fm_rds` unconditionally. Selecting this
#     renderer on a machine without the binary (or with sudo prompting for a
#     password on a thread nobody is watching) hung the app.
#   * `stop()` called `os._exit()`, which is a TypeError with no argument --
#     it could never have worked -- and, had it worked, would have killed
#     Macast outright instead of stopping the renderer.
#   * `os.waitpid(-1, 1)` in set_media_stop could reap an unrelated child.
#   * The position counter never reset on stop and never paused.

import time
import shutil
import logging
import threading
import subprocess
from macast import cli
from macast.renderer import Renderer

logger = logging.getLogger("PIFMRenderer")
logger.setLevel(logging.INFO)

#: The transmitter: `pi_fm_rds -freq 108 -audio -` reads raw WAV on stdin.
FM_COMMAND = ['sudo', 'pi_fm_rds', '-freq', '108', '-audio', '-']
#: Decodes whatever the sender pushed into the WAV the transmitter expects.
SOX_COMMAND = ['sox', '-t', 'mp3']


class PIFMRenderer(Renderer):
    def __init__(self):
        super(PIFMRenderer, self).__init__()
        self.start_position = 0
        self._playing = False
        self.position_thread_running = False
        self.sox = None
        self.fm = None
        self.position_thread = threading.Thread(target=self.position_tick,
                                                daemon=True,
                                                name="PIFM_POSITION")
        self.position_thread.start()

    def position_tick(self):
        while self.position_thread_running:
            time.sleep(1)
            if not self._playing:
                continue
            self.start_position += 1
            sec = self.start_position
            position = '%d:%02d:%02d' % (sec // 3600, (sec % 3600) // 60, sec % 60)
            self.set_state_position(position)

    def _require(self, program):
        path = shutil.which(program)
        if path is None:
            raise RuntimeError(
                '{} is not installed; the PIFMRDS renderer needs it (see the '
                'plugin README)'.format(program))
        return path

    def start_transmitter(self):
        """Start pi_fm_rds, once. Raises if it is not installed."""
        if self.fm is not None and self.fm.poll() is None:
            return
        # `shutil.which` on the first element: `sudo` resolves the real binary
        # itself, so this only catches the common "not installed at all" case.
        if shutil.which(FM_COMMAND[1]) is None:
            raise RuntimeError(
                'pi_fm_rds is not installed; the PIFMRDS renderer only works '
                'on a Raspberry Pi with PiFmRds available')
        logger.info("starting pi_fm_rds")
        self.fm = subprocess.Popen(FM_COMMAND,
                                   stdin=subprocess.PIPE,
                                   bufsize=1024)

    def set_media_stop(self):
        self._playing = False
        if self.sox is not None:
            try:
                self.sox.terminate()
            except OSError as e:
                logger.debug("terminating sox failed: %s", e)
        self.sox = None
        self.set_state_transport('STOPPED')
        # No os.waitpid here: Popen.wait() reaps this specific child, where
        # `waitpid(-1, 1)` could collect an unrelated one.
        if self.fm is not None:
            try:
                self.fm.wait(timeout=2)
            except Exception:
                pass

    def set_media_pause(self):
        self._playing = False
        self.set_state_transport('PAUSED_PLAYBACK')

    def set_media_resume(self):
        self._playing = True
        self.set_state_transport('PLAYING')

    def set_media_url(self, data, start="0"):
        """`:param start:` accepted for interface compatibility, then ignored.

        The transmitter broadcasts from the beginning of the stream; the
        upstream signature defaulted to the int 0 where the protocol passes a
        string.
        """
        self.set_media_stop()
        self.start_position = 0
        try:
            self.start_transmitter()
            sox = self._require('sox')
        except RuntimeError as e:
            logger.error("%s", e)
            self.set_state_transport_error()
            return
        self.sox = subprocess.Popen([sox] + SOX_COMMAND + [data, '-t', 'wav', '-'],
                                    stdout=self.fm.stdin)
        self._playing = True
        self.set_state_transport("PLAYING")

    def stop(self):
        super(PIFMRenderer, self).stop()
        self.position_thread_running = False
        self.set_media_stop()
        # The upstream body replaced this with `os._exit()` (a TypeError: it
        # requires an exit code) -- which, had it been spelled correctly, would
        # have killed Macast rather than stopping this renderer.
        if self.fm is not None:
            try:
                self.fm.terminate()
            except OSError:
                pass
            self.fm = None

    def start(self):
        super(PIFMRenderer, self).start()
        self.position_thread_running = True
        if not self.position_thread.is_alive():
            self.position_thread = threading.Thread(target=self.position_tick,
                                                    daemon=True,
                                                    name="PIFM_POSITION")
            self.position_thread.start()


if __name__ == '__main__':
    cli(PIFMRenderer())
