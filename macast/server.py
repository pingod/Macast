# Derived from xfangfang/Macast (GPLv3). See docs/Provenance.md.
# Copyright (c) 2026 by pingod. All Rights Reserved.
import os
import random
import shutil
import subprocess
import sys
import logging
import threading

import cherrypy
import portend
from cherrypy._cpserver import Server
from cherrypy.process.plugins import Monitor

from .utils import Setting, XMLPath, SettingProperty, SETTING_DIR
from .plugin import ProtocolPlugin, RendererPlugin, SSDPPlugin
from .protocol import Protocol

logger = logging.getLogger("server")
logger.setLevel(logging.DEBUG)


def auto_change_port(fun):
    """See AutoPortServer"""

    def wrapper(self):
        try:
            return fun(self)
        except portend.Timeout as e:
            logger.error(e)
            bind_host, bind_port = self.bind_addr
            if bind_port == 0:
                raise e
            else:
                self.httpserver = None
                self.bind_addr = (bind_host, 0)
                self.start()

    return wrapper


def _openssl_error(exc):
    """The first line openssl actually printed for this failure, or ''.

    `subprocess.run(check=True)` throws away the useful half of a failed
    certificate run -- the string repr is the command and the exit status -- so
    every message about cert generation used to say nothing a reader could act
    on. stderr is where openssl writes the reason.
    """
    raw = getattr(exc, 'stderr', None)
    if not raw:
        return ''
    text = raw.decode('utf-8', 'replace') if isinstance(raw, bytes) else str(raw)
    for line in text.splitlines():
        line = line.strip()
        if line:
            return line
    return ''


class AutoPortServer(Server):
    """
    The modified Server can give priority to the preset port (Setting.DEFAULT_PORT).
    When the preset port or the port in the configuration file cannot be used,
    using the port randomly assigned by the system
    """

    @auto_change_port
    def start(self):
        super(AutoPortServer, self).start()

    @auto_change_port
    def _start_http_thread(self):
        try:
            self.httpserver.start()
        except KeyboardInterrupt:
            self.bus.log('<Ctrl-C> hit: shutting down HTTP server')
            self.interrupt = sys.exc_info()[1]
            self.bus.exit()
        except SystemExit:
            self.bus.log('SystemExit raised: shutting down HTTP server')
            self.interrupt = sys.exc_info()[1]
            self.bus.exit()
            raise
        except Exception:
            self.interrupt = sys.exc_info()[1]
            if 'WinError 10013' in str(self.interrupt):
                self.bus.log('Error in HTTP server: WinError 10013')
                raise portend.Timeout
            else:
                self.bus.log('Error in HTTP server: shutting down',
                             traceback=True, level=40)
                self.interrupt = sys.exc_info()[1]
                self.bus.exit()
                raise


class OptionalServer(Server):
    """An HTTP server whose failure must not take the application down.

    CherryPy's bus publishes `start` to every subscriber and aborts the whole
    engine (`ChannelFailures` -> shutdown) if one of them raises. That is the
    right call for the DLNA HTTP server — without it there is no product — but
    wrong for the *optional* HTTPS channel: a busy port, a certificate cheroot
    cannot parse, or a frozen build missing `cheroot.ssl` used to kill the
    entire app, menu-bar icon and all, instead of just disabling one listener.
    """

    def start(self):
        try:
            super().start()
        except Exception as e:
            logger.error("HTTPS channel unavailable, continuing without it: "
                         "{}: {}".format(type(e).__name__, e))
            # Also drop the 'stop' listener: there is no server to stop, and
            # ServerAdapter.stop() would trip over the half-built instance.
            self.unsubscribe()
    start.priority = 75


class Service:

    def __init__(self, renderer, protocol):
        self.thread = None
        # Replace the default server
        cherrypy.server.unsubscribe()
        cherrypy.server = AutoPortServer()
        cherrypy.server.bind_addr = ('0.0.0.0', Setting.get_port())
        cherrypy.server.subscribe()
        # start plugins
        self.ssdp_plugin = None
        if getattr(protocol, "uses_ssdp", True):
            # SSDP (UPnP discovery) is only relevant for the DLNA protocol.
            # Chromecast/AirPlay advertise over mDNS instead.
            self.ssdp_plugin = SSDPPlugin(cherrypy.engine)
            self.ssdp_plugin.subscribe()
            self.ssdp_monitor_counter = 0  # restart ssdp every 30s
            self.ssdp_monitor = Monitor(cherrypy.engine, self.notify, 3, name="SSDP_NOTIFY_THREAD")
            self.ssdp_monitor.subscribe()
        self._renderer = renderer
        self.renderer_plugin = RendererPlugin(cherrypy.engine, renderer)
        self.renderer_plugin.subscribe()
        self._protocol = protocol
        self.protocol_plugin = ProtocolPlugin(cherrypy.engine, protocol)
        self.protocol_plugin.subscribe()
        cherrypy.config.update({
            'log.screen': False,
            # Deliberately *no* CherryPy file handlers. `_cplogging` installs a
            # plain (never rotating) logging.FileHandler, and CherryPy's
            # loggers propagate to the root logger, which already writes
            # macast.log through a RotatingFileHandler. Pointing these at the
            # same path meant every single line was written twice -- one bare
            # copy plus one from the root handler -- and the second writer grew
            # the file without any ceiling:
            #
            #   127.0.0.1 - - [...] "GET /api?query=status ..." 200 22735
            #   [2026-09-16 23:25:39,275] cherrypy.access.4411285168 INFO: ...
            #
            # Measured on a live instance: 2 x 2621 access lines in 49 minutes
            # (~64% of the 1.35 MB file). Leaving the config empty removes only
            # CherryPy's own handler; the records still reach macast.log
            # through the root logger, rotation included.
            'log.access_file': '',
            'log.error_file': '',
        })
        # cherrypy.engine.autoreload.files.add(Setting.setting_path)
        cherrypy_config = {
            '/dlna': {
                'tools.staticdir.root': XMLPath.BASE_PATH.value,
                'tools.staticdir.on': True,
                'tools.staticdir.dir': "xml"
            },
            '/assets': {
                'tools.staticdir.root': XMLPath.BASE_PATH.value,
                'tools.staticdir.on': True,
                'tools.staticdir.dir': "assets"
            },
            '/': {
                'request.dispatch': cherrypy.dispatch.MethodDispatcher(),
                'tools.response_headers.on': True,
                'tools.response_headers.headers':
                    [('Content-Type', 'text/xml; charset="utf-8"'),
                     ('Server', Setting.get_server_info())],
            }
        }

        self.cherrypy_application = cherrypy.tree.mount(self.protocol.handler, '/', config=cherrypy_config)
        cherrypy.engine.signals.subscribe()
        # A frozen bundle (.app / PyInstaller) never changes on disk: the
        # Autoreloader has nothing useful to watch, and watching the bundle is
        # actively harmful — moving or rebuilding the .app while it runs makes
        # CherryPy "Re-spawn" into a process whose resource files are gone,
        # which surfaces as 500 errors instead of a clean exit.
        if getattr(sys, 'frozen', False):
            cherrypy.engine.autoreload.unsubscribe()
        # HTTPS (Web/管理 API) channel — DLNA control/SSDP stays on plain
        # HTTP because the UPnP standard requires it. The HTTPS server reuses
        # the same WSGI app (cherrypy.tree) so the settings UI and management
        # API are also reachable over an encrypted LAN connection (PWA).
        self._start_https_server()

    @property
    def renderer(self):
        return self._renderer

    @renderer.setter
    def renderer(self, value):
        self._renderer = value
        self.renderer_plugin.set_renderer(self._renderer)

    @property
    def protocol(self):
        return self._protocol

    @protocol.setter
    def protocol(self, value: Protocol):
        self.stop()
        self._protocol = value
        self.protocol_plugin.set_protocol(self._protocol)
        self.cherrypy_application.root = self._protocol.handler
        self._sync_ssdp(self._protocol)
        self._protocol.handler.reload()

    def refresh_protocol(self):
        """Re-bind discovery to the protocols currently enabled.

        Called after the user toggles a protocol. Restarting the whole service
        would do this too, but it would also cut off anything playing, so the
        membership change is applied in place instead.
        """
        # The root handler depends on which protocols are enabled: DLNA's
        # handler serves the UPnP endpoints on top of the web UI.
        if getattr(self, "cherrypy_application", None) is not None:
            self.cherrypy_application.root = self._protocol.handler
        self._sync_ssdp(self._protocol)

    def _sync_ssdp(self, protocol):
        """Start or stop SSDP so it matches the protocol's discovery mechanism.

        SSDP is wired up once in ``__init__`` for whichever protocol was
        selected at launch, but the user can switch protocols from the menu at
        any time. Without this, moving from DLNA to Chromecast/AirPlay leaves a
        DLNA device being announced that no longer answers (and switching back
        would announce nothing at all), because the plugin only ever followed
        the initial choice.
        """
        wanted = getattr(protocol, "uses_ssdp", True)
        if wanted and self.ssdp_plugin is None:
            self.ssdp_plugin = SSDPPlugin(cherrypy.engine)
            self.ssdp_plugin.subscribe()
            self.ssdp_monitor_counter = 0
            self.ssdp_monitor = Monitor(cherrypy.engine, self.notify, 3,
                                        name="SSDP_NOTIFY_THREAD")
            self.ssdp_monitor.subscribe()
            # A plugin created after the engine is already running never gets
            # its start() called by the bus, so do it by hand.
            if cherrypy.engine.state != cherrypy.engine.states.STOPPED:
                self.ssdp_plugin.start()
                self.ssdp_monitor.start()
        elif not wanted and self.ssdp_plugin is not None:
            for plugin in (getattr(self, "ssdp_monitor", None), self.ssdp_plugin):
                if plugin is None:
                    continue
                try:
                    plugin.stop()
                except Exception as e:  # pragma: no cover - defensive
                    logger.warning("Stopping %s failed: %s",
                                   type(plugin).__name__, e)
                try:
                    plugin.unsubscribe()
                except Exception as e:  # pragma: no cover - defensive
                    logger.warning("Unsubscribing %s failed: %s",
                                   type(plugin).__name__, e)
            self.ssdp_plugin = None
            self.ssdp_monitor = None

    def _start_https_server(self):
        """Start an optional HTTPS server that reuses the same WSGI app
        (cherrypy.tree) as the DLNA HTTP server. DLNA control/SSDP stays on
        plain HTTP (the UPnP standard requires it); this second server only
        secures the Web settings UI and the management API for LAN clients
        (e.g. the PWA opened on a phone)."""
        if not Setting.is_https_enabled():
            logger.info("HTTPS channel disabled by setting")
            return
        cert = Setting.get_https_cert()
        key = Setting.get_https_key()
        if not cert or not key or not (os.path.exists(cert) and os.path.exists(key)):
            cert, key = self._ensure_self_signed_cert()
        if not cert or not key:
            logger.warning("HTTPS channel unavailable: no usable certificate")
            return
        try:
            https_port = Setting.get_https_port()
            # Pre-flight the SSL adapter: cheroot resolves it lazily, inside
            # the engine's 'start' listener, where a failure (e.g. a frozen
            # build that missed cheroot.ssl) is fatal to the whole app instead
            # of just disabling this one channel. (OptionalServer above is the
            # second line of defence for the failures we cannot pre-flight,
            # such as the port being taken.)
            __import__('cheroot.ssl.builtin')
            https_server = OptionalServer()
            https_server.bind_addr = ('0.0.0.0', https_port)
            https_server.ssl_module = 'builtin'
            https_server.ssl_certificate = cert
            https_server.ssl_private_key = key
            https_server.subscribe()
            logger.info("HTTPS channel will run on port: {}".format(https_port))
        except Exception as e:
            logger.error("Failed to start HTTPS channel: {}".format(e))

    @staticmethod
    def _find_openssl():
        """Return a usable openssl binary, preferring a modern OpenSSL.

        Inside a bundled .app the PATH is minimal, so Homebrew's OpenSSL is
        usually *not* on it, while macOS' own /usr/bin/openssl is LibreSSL and
        rejects `-addext` (the flag we use to embed the subjectAltName).
        """
        candidates = [
            shutil.which('openssl'),
            '/opt/homebrew/bin/openssl',
            '/usr/local/bin/openssl',
            '/usr/bin/openssl',
        ]
        for candidate in candidates:
            if candidate and os.path.exists(candidate):
                return candidate
        return None

    @staticmethod
    def _ensure_self_signed_cert():
        """Generate a self-signed certificate in SETTING_DIR if none exists.
        Reusing a stable on-disk cert keeps browsers from re-prompting on
        every restart. Returns (cert_path, key_path) or (None, None)."""
        cert_path = os.path.join(SETTING_DIR, 'macast.crt')
        key_path = os.path.join(SETTING_DIR, 'macast.key')
        if os.path.exists(cert_path) and os.path.exists(key_path):
            return cert_path, key_path
        openssl = Service._find_openssl()
        if not openssl:
            logger.error("Failed to generate self-signed certificate: "
                         "no openssl binary found")
            return None, None
        # openssl will not create a file inside a directory that does not exist,
        # and on a fresh install nothing has made it yet -- Setting.save() is the
        # only other writer, and the HTTPS/cast channels can be reached before
        # the first save. Without this the generation failed, the cast receiver
        # then raised FileNotFoundError out of load_cert_chain on the real port,
        # and the one line in the log blamed '-addext'.
        try:
            os.makedirs(SETTING_DIR, exist_ok=True)
        except OSError as e:
            logger.error("Cannot create the config directory %s: %s",
                         SETTING_DIR, e)
            return None, None
        cmd = [
            openssl, 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
            '-keyout', key_path, '-out', cert_path,
            '-days', '3650', '-subj', '/CN=Macast Local Service',
            '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
        ]
        try:
            try:
                subprocess.run(cmd, check=True, capture_output=True, timeout=30)
            except subprocess.CalledProcessError as first:
                # LibreSSL / older OpenSSL cannot parse -addext: retry with a
                # plain self-signed cert (works, but has no subjectAltName).
                # Quote what openssl actually said -- this branch used to claim
                # '-addext unsupported' for *every* non-zero exit, so a failure
                # with another cause (a directory that vanished, a read-only
                # home) was logged as a flag-compatibility problem.
                logger.warning("%s rejected -addext (%s), retrying without it",
                               openssl, _openssl_error(first))
                subprocess.run(cmd[:-2], check=True, capture_output=True,
                               timeout=30)
            logger.info("Generated self-signed certificate: {}".format(cert_path))
            Setting.set(SettingProperty.Https_Cert, cert_path)
            Setting.set(SettingProperty.Https_Key, key_path)
            return cert_path, key_path
        except Exception as e:
            logger.error("Failed to generate self-signed certificate: %s (%s)",
                         _openssl_error(e) or e, openssl)
            return None, None

    def notify(self):
        """ssdp do notify
        Using cherrypy builtin plugin Monitor to trigger this method
        see also: plugin.py -> class SSDPPlugin -> notify
        """
        if self.ssdp_plugin is None:
            return
        self.ssdp_monitor_counter += 1
        if Setting.is_ip_changed() or self.ssdp_monitor_counter == 10:
            self.ssdp_monitor_counter = 0
            cherrypy.engine.publish('ssdp_update_ip')
        cherrypy.engine.publish('ssdp_notify')

    def run(self):
        """Start macast thread
        """
        cherrypy.engine.start()
        # update current port
        _, port = cherrypy.server.bound_addr
        logger.info("Server current run on port: {}".format(port))
        if port != Setting.get(SettingProperty.ApplicationPort, 0):
            # todo 验证正确性
            usn = Setting.get_usn(refresh=True)
            logger.error("Change usn to: {}".format(usn))
            Setting.set(SettingProperty.ApplicationPort, port)
            name = "Macast({0:04d})".format(random.randint(0, 9999))
            logger.error("Change name to: {}".format(name))
            Setting.set_temp_friendly_name(name)
            self.protocol.handler.reload()
            cherrypy.engine.publish('ssdp_update_ip')
        # service started
        cherrypy.engine.block()
        # service stopped
        logger.info("Service stopped")

    def stop(self):
        """Stop macast thread
        """
        Setting.stop_service()
        if self.thread is not None:
            self.thread.join()

    def run_async(self):
        if Setting.is_service_running():
            return
        self.thread = threading.Thread(target=self.run, name="SERVICE_THREAD")
        self.thread.start()
