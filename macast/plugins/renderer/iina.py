# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Using IINA as DLNA media renderer
#
# Macast Metadata
# <macast.title>IINA Renderer</macast.title>
# <macast.renderer>IINARenderer</macast.renderer>
# <macast.platform>darwin</macast.platform>
# <macast.version>0.31</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod（原作者 xfangfang）</macast.author>
# <macast.desc>用 IINA 作为 Macast 的 DLNA 播放器。IINA 基于 mpv 开发，体验与内置的 mpv 渲染器相似。需要本机已安装 IINA。</macast.desc>


import os
import json
import time
import socket
import threading
import cherrypy
import subprocess
import gettext
import logging
from queue import Queue, Empty
from macast import RendererSetting, Setting, gui
from macast_renderer.mpv import MPVRenderer

IINA_PATH = '/Applications/IINA.app/Contents/MacOS/iina-cli'

logger = logging.getLogger("IINARenderer")
logger.setLevel(logging.DEBUG)


class IINARenderer(MPVRenderer):
    def __init__(self):
        super(IINARenderer, self).__init__(gettext.gettext, IINA_PATH)
        self.renderer_setting = RendererSetting()
        self.commond_queue = Queue()
        self.mpv_thread = None
        self.ipc_thread = None
        self.iina = None
        self.is_iina_start = False

    def command_send_thread(self):
        """Drain the command queue and write each command to IINA's IPC socket.

        The only writer of `ipc_sock`, which is why commands go through here
        instead of straight out of the base class' `send_command`.
        """
        logger.debug("command_send_thread start (running=%s)", self.running)
        while self.running:
            try:
                command = self.commond_queue.get(timeout=0.5)
            except Empty:
                continue
            if not self.running:
                return
            if not self.is_iina_start:
                # Nothing to talk to; drop it rather than spinning.
                continue
            error_time = 10
            while error_time > 0:
                error_time -= 1
                logger.debug("send command: %s", command)
                msg = json.dumps({"command": command}) + '\n'
                try:
                    self.ipc_sock.sendall(msg.encode())
                    time.sleep(0.05)
                    break
                except Exception as e:
                    logger.error('error sendCommand: ' + str(e))
                    time.sleep(1)
            else:
                cherrypy.engine.publish("app_notify", "Macast", "Cannot sending msg to iina.")
                logger.error("iina cannot start")
                threading.Thread(target=lambda: Setting.stop_service(), name="IINA_STOP_SERVICE").start()

    def set_media_stop(self):
        try:
            if self.iina is not None:
                self.iina.terminate()
        except Exception as e:
            logger.debug("terminating iina failed: %s", e)
        self.iina = None
        self.is_iina_start = False
        self.ipc_running = False
        # `os.waitpid(-1, 1)` used to run unconditionally: with no child to
        # reap it raises ChildProcessError, and `-1` could sweep up an
        # unrelated child of the process. Popen.wait() below is the correct,
        # targeted way to do this.
        if self.ipc_thread is not None and self.ipc_thread.is_alive() \
                and self.ipc_thread is not threading.current_thread():
            # Guard against self-join: this method is called from the IPC
            # thread itself when IINA dies (start_ipc -> set_state_stop), and
            # joining yourself blocks the thread forever.
            self.ipc_thread.join(timeout=5)
        self.ipc_thread = None
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_url(self, data, start='0'):
        """Load `data` into IINA, seeking to `start` seconds in.

        `start` may be seconds ("30") or a timestamp ("00:00:30").
        """

        def position_to_second(position: str) -> int:
            pos = position.split(':')
            if len(pos) < 3:
                return 0
            try:
                return int(pos[0]) * 3600 + int(pos[1]) * 60 + int(pos[2])
            except ValueError:
                return 0

        try:
            start = int(start)
        except (TypeError, ValueError):
            start = position_to_second(str(start))

        if not self.is_iina_start:
            self.set_media_stop()
            try:
                self.start_iina(data, start)
            except Exception as e:
                logger.error("cannot start IINA: %s", e)
                self.set_state_transport_error()
                cherrypy.engine.publish('app_notify', 'Macast', str(e))
                return
            self.ipc_thread = threading.Thread(target=self.start_ipc, name="IINA_IPC_THREAD")
            self.ipc_thread.start()
        else:
            # mpv IPC `loadfile <url> [<flags> [<index>]]`: the 4th argument is
            # the playlist INDEX, not a set of options. Passing `start=N`
            # there made mpv reject the whole command with "invalid parameter",
            # so a second cast in the same session never played -- the exact
            # trap already documented in macast_renderer/mpv.py's
            # set_media_url. Load, then seek separately.
            self.send_command(['loadfile', data, 'replace'])
            if start:
                self.send_command(['seek', str(start), 'absolute'])
        cherrypy.engine.publish('renderer_av_uri', data)

    def send_command(self, command):
        """Queue a command for the single sender thread.

        IINA writes exclusively from `command_send_thread`, because the base
        class' `send_command` would push straight to `ipc_sock` from whichever
        thread called it -- two writers on one socket.
        """
        if not self.is_iina_start:
            return
        logger.debug("queue command: %s", command)
        self.commond_queue.put(command)

    def set_observe(self):
        super(IINARenderer, self).set_observe()
        self.set_media_volume(100)

    def start_iina(self, url, start=0):
        """Start iina, returning True when the process actually launched."""
        if not os.path.exists(self.path):
            # Without this the plugin reported PLAYING and then sat forever
            # waiting for an IPC socket that would never appear.
            raise RuntimeError(
                'IINA was not found at {}. Install IINA or point the plugin '
                'at its CLI.'.format(self.path))
        params = [
            self.path,
            '--keep-running',
            f'--mpv-input-ipc-server={self.mpv_sock}',
            f'--mpv-start={start}',
            url,
        ]
        logger.info("iina starting")
        cherrypy.engine.publish('mpv_start')
        # DEVNULL, not PIPE: nothing ever reads these pipes, and a long
        # playback would eventually fill the 64 KiB pipe buffer and block IINA
        # forever on its next write to stdout.
        self.iina = subprocess.Popen(
            params,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            env=Setting.get_system_env())
        # Only now is it true: setting the flag before Popen meant a failed
        # launch still looked "started", so the command thread kept pushing
        # commands at a socket that did not exist.
        self.is_iina_start = True
        return True

    def start_ipc(self):
        """Start ipc thread
        Communicating with mpv
        """
        if self.ipc_running:
            logger.error("mpv ipc is already runing")
            return
        self.ipc_running = True
        error_time = 0
        internal = 0.5
        while self.ipc_running and self.running and self.mpv_thread is not None \
                and self.mpv_thread.is_alive():
            try:
                time.sleep(internal)
                logger.debug("mpv ipc socket start connect")
                self.ipc_sock = socket.socket(socket.AF_UNIX,
                                              socket.SOCK_STREAM)
                self.ipc_sock.connect(self.mpv_sock)
                cherrypy.engine.publish('mpvipc_start')
                cherrypy.engine.publish('renderer_start')
                self.ipc_once_connected = True
                internal = 0.5
            except Exception as e:
                error_time += 1
                if error_time > 20:
                    internal = 2
                if self.iina is not None and self.iina.poll() is not None:
                    self.is_iina_start = False
                    self.ipc_running = False
                    self.set_state_stop()
                logger.error("mpv ipc socket reconnecting: {}".format(str(e)))
                continue
            # Observed properties are requested once per connection, after the
            # socket is up -- they used to be sent before the connect could
            # succeed, so they were dropped.
            self.set_observe()
            res = b''
            try:
                while self.ipc_running:
                    try:
                        data = self.ipc_sock.recv(1048576)
                        if data == b'':
                            break
                        res += data
                        if data[-1] != 10:
                            continue
                    except Exception as e:
                        logger.debug(e)
                        break
                    try:
                        msgs = res.decode().strip().split('\n')
                        for msg in msgs:
                            self.update_state(msg)
                    except Exception as e:
                        logger.error("decode error: {}".format(e))
                        logger.error("decode error data: %r", res)
                    finally:
                        res = b''
            finally:
                # A raise inside the read loop used to leave the last chunk of
                # partial JSON behind for the next connection to choke on, and
                # leaked the socket when close() itself failed.
                res = b''
                try:
                    self.ipc_sock.close()
                except OSError:
                    pass
            logger.info("mpv ipc stopped")

    def start(self):
        super(MPVRenderer, self).start()
        logger.info("starting IINARenderer")
        self.mpv_thread = threading.Thread(target=self.command_send_thread, daemon=True, name="COMMAND_SEND")
        self.mpv_thread.start()

    def stop(self):
        super(MPVRenderer, self).stop()
        logger.info("stoping IINARenderer")
        self.set_media_stop()
        # Deliberately skip MPVRenderer.stop (there is no `self.proc`; IINA is
        # launched separately), but it is also the only place that removes the
        # IPC socket. Without this, /tmp/macast_mpvsocketNNNN accumulated one
        # stale file per session.
        try:
            os.remove(self.mpv_sock)
        except OSError:
            pass


if __name__ == '__main__':
    Setting.load()
    gui(IINARenderer())
