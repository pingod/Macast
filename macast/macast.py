# Copyright (c) 2021 by xfangfang. All Rights Reserved.

import os
import re
import sys
import time
import json
import cherrypy
import logging
import threading
import requests
import pyperclip
import gettext
import importlib

from .utils import SettingProperty, SETTING_DIR, notify_error, format_class_name
# This fork is distributed from pingod/Macast; the "check for updates" feature
# must query that repo, not the upstream xfangfang/Macast.
GITHUB_REPO = 'pingod/Macast'
from .gui import App, MenuItem, Platform
from .protocol import DLNAProtocol
from .server import Service
from .utils import RENDERER_DIR, PROTOCOL_DIR, Setting
from macast_renderer.mpv import MPVRenderer

logger = logging.getLogger("main")
logger.setLevel(logging.DEBUG)
_ = gettext.gettext


class MacastPlugin:

    def __init__(self, path, title="None", plugin_instance=None, platform='none'):
        # path is allowed to be set to None only when renderer is macast default plugin
        self.path = path
        self.title = title
        self.plugin_class = None
        self.plugin_instance = plugin_instance
        self.platform = platform
        if path:
            try:
                self.load_from_file(path)
            except Exception as e:
                cherrypy.engine.publish('app_notify', 'ERROR', 'Custom plugin load error.')
                logger.error(str(e))

    def get_info(self):
        props = ['protocol', 'title', 'renderer', 'platform', 'version', 'author', 'desc']
        res = {'default': False}
        for i in props:
            res[i] = getattr(self, i, '')
        if getattr(self, 'renderer', None) is not None:
            res['type'] = 'renderer'
        if getattr(self, 'protocol', None) is not None:
            res['type'] = 'protocol'
        if self.path is None:
            res['default'] = True
            if getattr(self, 'plugin_instance', None) is not None:
                res['desc'] = '{} protocol (built-in)'.format(self.title)
            else:
                res['desc'] = 'Macast default plugin'
            res['version'] = Setting.version
        return res

    def get_instance(self):
        if self.plugin_instance is None and self.plugin_class is not None:
            self.plugin_instance = self.plugin_class()

        return self.plugin_instance

    def check(self):
        """ Check if this renderer can run on your device
        """
        if self.plugin_class is None:
            return False
        # self.platform is a comma-separated list like 'darwin,win32,linux'.
        # Use an explicit split+membership test instead of `in` on the raw
        # string, which would also match substrings incorrectly.
        if sys.platform in [p.strip() for p in self.platform.split(',')]:
            return True

        logger.error("{} support platform: {}".format(self.title, self.platform))
        logger.error("{} is not suit for this system.".format(self.title))
        return False

    def load_from_file(self, path):
        base_name = os.path.basename(path)[:-3]
        with open(path, 'r', encoding='utf-8') as f:
            renderer_file = f.read()
            metadata = re.findall("<macast.(.*?)>(.*?)</macast", renderer_file)
            print("<Load Plugin from {}".format(base_name))
            for key, value in metadata:
                print('%-10s: %s' % (key, value))
                setattr(self, key, str(value))
        if hasattr(self, 'renderer'):
            module = importlib.import_module(f'{RENDERER_DIR}.{base_name}')
            print(f'Load plugin {self.renderer} done />\n')
            self.plugin_class = getattr(module, self.renderer, None)
        elif hasattr(self, 'protocol'):
            module = importlib.import_module(f'{PROTOCOL_DIR}.{base_name}')
            print(f'Load plugin {self.protocol} done />\n')
            self.plugin_class = getattr(module, self.protocol, None)
        else:
            logger.error(f"Cannot find any plugin in {base_name}")
            return


class MacastPluginManager:

    def __init__(self, renderer_default, protocol_default):
        sys.path.append(SETTING_DIR)
        self.create_plugin_dir(RENDERER_DIR)
        self.create_plugin_dir(PROTOCOL_DIR)
        self.renderer_list = [renderer_default]
        self.renderer_list += self.load_macast_plugin(RENDERER_DIR)
        self.protocol_list = [protocol_default]
        self.protocol_list += self._load_builtin_protocols()
        self.protocol_list += self.load_macast_plugin(PROTOCOL_DIR)

    def get_renderer(self, name):
        plugin = self.get_plugin_from_list(self.renderer_list, name)
        Setting.set(SettingProperty.Macast_Renderer, plugin.title)
        return plugin.get_instance()

    def get_protocol(self, name):
        plugin = self.get_plugin_from_list(self.protocol_list, name)
        return plugin.get_instance()

    @staticmethod
    def canonical_title(title):
        """Normalise a protocol title for comparison (case/space tolerant)."""
        return " ".join(str(title).strip().lower().split()).replace(" protocol", "").strip()

    def resolve_title(self, title):
        """Map a stored protocol name onto the current plugin list.

        The default DLNA plugin is titled from its class name —
        ``format_class_name(DLNAProtocol())`` yields "DLNA Protocol" — so a
        stored value of "DLNA" would otherwise match nothing and silently drop
        the protocol (taking SSDP discovery down with it). Match fuzzily, but
        always return the *real* title so what gets persisted stays canonical.
        """
        for plugin in self.protocol_list:
            if plugin.title == title:
                return plugin.title
        canonical = self.canonical_title(title)
        for plugin in self.protocol_list:
            if self.canonical_title(plugin.title) == canonical:
                return plugin.title
        return None

    def get_protocol_instance(self, title):
        """Return a fresh instance for `title`, or None if it is not known."""
        resolved = self.resolve_title(title)
        if resolved is None:
            return None
        for plugin in self.protocol_list:
            if plugin.title == resolved:
                return plugin.get_instance()
        return None

    def build_protocol_group(self, titles):
        """Instantiate `titles` into a ProtocolGroup that runs them together.

        Unknown titles are skipped so a stale settings entry (a plugin that has
        since been removed) cannot prevent Macast from starting.
        """
        from .protocol_group import ProtocolGroup
        group = ProtocolGroup()
        for title in titles:
            # Store the *canonical* title so that membership checks elsewhere
            # (toggling a protocol later) compare like with like.
            resolved = self.resolve_title(title)
            if resolved is None:
                logger.warning("Skipping unknown protocol: {}".format(title))
                continue
            if resolved in group:
                continue
            instance = self.get_protocol_instance(resolved)
            if instance is None:
                logger.warning("Skipping unavailable protocol: {}".format(resolved))
                continue
            group.add(resolved, instance)
        if len(group) == 0:
            # Never leave the app with nothing listening.
            fallback = self.resolve_title("DLNA")
            if fallback is not None:
                group.add(fallback, self.get_protocol_instance(fallback))
        return group

    def get_info(self):
        res = []
        for r in self.renderer_list:
            res.append(r.get_info())
        for p in self.protocol_list:
            res.append(p.get_info())
        return res

    @staticmethod
    def get_plugin_from_list(plugin_list, title) -> MacastPlugin:
        for i in plugin_list:
            if title == i.title:
                print("using plugin: {}".format(title))
                return i
        else:
            print("using default plugin")
            return plugin_list[0]

    def _load_builtin_protocols(self):
        """Protocols shipped with Macast, selectable from the menu.

        These mirror the DLNA default: they are built-in (not loaded from the
        user plugin directory) and advertise over mDNS instead of SSDP.
        """
        from .protocol_cast import ChromecastProtocol
        from .protocol_airplay import AirPlayProtocol
        return [
            MacastPlugin(None, "Chromecast", ChromecastProtocol(), "darwin,win32,linux"),
            MacastPlugin(None, "AirPlay", AirPlayProtocol(), "darwin,win32,linux"),
        ]

    @staticmethod
    def load_macast_plugin(path: str):
        plugin_path = os.path.join(SETTING_DIR, path)
        if not os.path.exists(plugin_path):
            return
        plugin_list = []
        plugins = os.listdir(plugin_path)
        plugins = filter(lambda s: s.endswith('.py') and s != '__init__.py', plugins)
        for plugin in plugins:
            path = os.path.join(plugin_path, plugin)
            plugin_config = MacastPlugin(path)
            if plugin_config.check():
                plugin_list.append(plugin_config)
        return plugin_list

    @staticmethod
    @notify_error('Cannot create custom plugin dir.')
    def create_plugin_dir(path):
        custom_module_path = os.path.join(SETTING_DIR, path)
        if not os.path.exists(custom_module_path):
            os.makedirs(custom_module_path)
        init_file_path = os.path.join(custom_module_path, '__init__.py')
        if not os.path.exists(init_file_path):
            open(init_file_path, 'a').close()


class Macast(App):
    if sys.platform == 'win32':
        ICON_MAP = ['assets/icon.ico',
                    'assets/menu_light_large.png',
                    'assets/menu_dark_large.png']
    else:
        ICON_MAP = ['assets/icon.png',
                    'assets/menu_light.png',
                    'assets/menu_dark.png']

    def __init__(self, renderer, protocol, lang=gettext.gettext):
        global _
        _ = lang
        # menu items
        self.toggle_menuitem = None
        self.setting_menuitem = None
        self.quit_menuitem = None
        self.ip_menuitem = None
        self.version_menuitem = None
        self.auto_check_update_menuitem = None
        self.start_at_login_menuitem = None
        self.menubar_icon_menuitem = None
        self.check_update_menuitem = None
        self.about_menuitem = None
        self.open_config_menuitem = None
        self.renderer_menuitem = None
        self.protocol_menuitem = None
        self.advanced_menuitem = None

        self.plugin_manager = MacastPluginManager(
            MacastPlugin(None, format_class_name(renderer), renderer, 'darwin,win32,linux'),
            MacastPlugin(None, format_class_name(protocol), protocol, 'darwin,win32,linux'))

        cherrypy.engine.subscribe('get_plugin_info', self.plugin_manager.get_info)

        # setting items
        self.setting_start_at_login = None
        self.setting_check = None
        self.setting_menubar_icon = 0
        self.setting_renderer = ''
        self.setting_protocol = ''
        self.init_setting()

        # init service. Several protocols can be enabled at once; they are
        # bundled into one ProtocolGroup that behaves like a single Protocol
        # towards everything downstream (CherryPy tree, bus, SSDP).
        self.service = Service(
            self.plugin_manager.get_renderer(self.setting_renderer),
            self.plugin_manager.build_protocol_group(self.enabled_protocols))

        icon_path = os.path.join(os.path.dirname(__file__), Macast.ICON_MAP[self.setting_menubar_icon])
        template = None if self.setting_menubar_icon == 0 else True
        self.copy_menuitem = None
        super(Macast, self).__init__("Macast",
                                     icon_path,
                                     self.build_app_menu(),
                                     template
                                     )
        cherrypy.engine.subscribe('start', self.service_start)
        cherrypy.engine.subscribe('stop', self.service_stop)
        cherrypy.engine.subscribe('renderer_start', self.renderer_start)
        cherrypy.engine.subscribe('renderer_av_stop', self.renderer_av_stop)
        cherrypy.engine.subscribe('renderer_av_uri', self.renderer_av_uri)
        cherrypy.engine.subscribe('ssdp_update_ip', self.update_service_ip)
        cherrypy.engine.subscribe('app_notify', self.notification)
        self.start_cast()
        logger.debug("Macast APP started")

    def build_app_menu(self):
        self.toggle_menuitem = MenuItem(_("Stop Cast"), self.on_toggle_service_click, key="p")
        self.setting_menuitem = MenuItem(_("Setting"), children=self.build_setting_menu())
        self.quit_menuitem = MenuItem(_("Quit"), self.quit, key="q")
        return [
            self.toggle_menuitem,
            None,
            self.setting_menuitem,
            self.quit_menuitem
        ]

    def build_setting_menu(self):
        ip_text = "/".join([ip for ip, _ in Setting.get_ip()])
        port = Setting.get_port()
        self.ip_menuitem = MenuItem("{}:{}".format(ip_text, port), enabled=False)
        self.version_menuitem = MenuItem(
            "{} v{}".format(Setting.get_friendly_name(), Setting.get_version()), enabled=False)
        self.auto_check_update_menuitem = MenuItem(_("Auto Check Updates"),
                                                   self.on_auto_check_update_click,
                                                   checked=self.setting_check)
        self.start_at_login_menuitem = MenuItem(_("Start At Login"),
                                                self.on_start_at_login_click,
                                                checked=self.setting_start_at_login)

        renderer_names = [r.title for r in self.plugin_manager.renderer_list]
        renderer_select = []
        if len(renderer_names) > 1:
            self.renderer_menuitem = MenuItem(_("Renderers"),
                                              children=App.build_menu_item_group(renderer_names,
                                                                                 self.on_renderer_change_click))
            renderer_select = [self.renderer_menuitem]
            for i in self.renderer_menuitem.children:
                if i.text == self.setting_renderer:
                    i.checked = True
                    break
            else:
                self.renderer_menuitem.children[0].checked = True

        protocol_names = [r.title for r in self.plugin_manager.protocol_list]
        # Protocols are checkboxes, not radio buttons: multiple may run
        # concurrently (DLNA over SSDP, Chromecast/AirPlay over mDNS). The
        # previous single-choice menu became the degenerate case of this.
        protocol_select = []
        if len(protocol_names) > 1:
            self.protocol_menuitem = MenuItem(_("Protocols"),
                                              children=App.build_menu_item_group(
                                                  protocol_names,
                                                  self.on_protocol_toggle_click))
            protocol_select = [self.protocol_menuitem]
            for i in self.protocol_menuitem.children:
                i.checked = i.text in self.enabled_protocols


        platform_options = []
        """To judge whether Macast was launched by scripts or by packaged app.
        """
        if sys.platform == 'darwin' and sys.executable.endswith("Contents/MacOS/python"):
            platform_options = [self.start_at_login_menuitem]
            # Reset StartAtLogin to prevent the user from turning off
            # this option from the system settings
            Setting.set_start_at_login(self.setting_start_at_login)
        elif sys.platform == 'win32' and "python" not in os.path.basename(sys.executable).lower():
            platform_options = [self.start_at_login_menuitem]
            Setting.set_start_at_login(self.setting_start_at_login)
        if sys.platform == 'darwin':
            self.menubar_icon_menuitem = MenuItem(_("Menubar Icon"),
                                                  children=App.build_menu_item_group([
                                                      _("AppIcon"),
                                                      _("Pattern"),
                                                  ], self.on_menubar_icon_change_click))
        else:
            self.menubar_icon_menuitem = MenuItem(_("Menubar Icon"),
                                                  children=App.build_menu_item_group([
                                                      _("AppIcon"),
                                                      _("PatternLight"),
                                                      _("PatternDark"),
                                                  ], self.on_menubar_icon_change_click))
        self.open_config_menuitem = MenuItem(_("Open Config Directory"), self.on_open_config_click)
        self.advanced_menuitem = MenuItem(_("Advanced Setting"),
                                          lambda _: self.open_browser('http://127.0.0.1:{}'.format(Setting.get_port())))
        self.check_update_menuitem = MenuItem(_("Check For Updates"), self.on_check_click)
        self.about_menuitem = MenuItem(_("Help"), self.on_about_click)

        self.menubar_icon_menuitem.items()[self.setting_menubar_icon].checked = True
        player_settings = self.service.renderer.renderer_setting.build_menu()
        if len(player_settings) > 0:
            player_settings.append(None)

        return [self.version_menuitem, self.ip_menuitem] + \
               renderer_select + \
               protocol_select + \
               [None] + \
               player_settings + \
               [self.menubar_icon_menuitem, self.auto_check_update_menuitem,
                self.open_config_menuitem, self.advanced_menuitem] + \
               platform_options + \
               [None, self.check_update_menuitem, self.about_menuitem]

    def _load_enabled_protocols(self):
        """Read the enabled-protocol list, migrating the old single value.

        Before protocols could run concurrently the settings file held one
        string in `Macast_Protocol`. Existing installs should keep working
        exactly as they did, so that value is adopted as a one-element list;
        only fresh installs default to everything switched on.
        """
        stored = Setting.get(SettingProperty.Macast_Protocols, None)
        available = [p.title for p in self.plugin_manager.protocol_list]

        if isinstance(stored, list) and stored:
            titles = []
            for name in stored:
                resolved = self.plugin_manager.resolve_title(name)
                if resolved and resolved not in titles:
                    titles.append(resolved)
            if titles:
                return titles
        elif isinstance(stored, str):
            resolved = self.plugin_manager.resolve_title(stored)
            if resolved:
                return [resolved]

        # Nothing usable yet: adopt the legacy scalar if there is one...
        legacy = Setting.get(SettingProperty.Macast_Protocol, None)
        if isinstance(legacy, str):
            resolved = self.plugin_manager.resolve_title(legacy)
            if resolved:
                titles = [resolved]
                Setting.set(SettingProperty.Macast_Protocols, titles)
                return titles
        # ...otherwise a fresh install gets every protocol at once.
        return list(available)

    def save_enabled_protocols(self):
        Setting.set(SettingProperty.Macast_Protocols, list(self.enabled_protocols))

    def init_setting(self):
        self.setting_start_at_login = Setting.get(SettingProperty.StartAtLogin, 0)
        self.setting_check = Setting.get(SettingProperty.CheckUpdate, 1)
        self.setting_menubar_icon = Setting.get(SettingProperty.MenubarIcon, 1 if sys.platform == 'darwin' else 0)
        self.setting_renderer = Setting.get(SettingProperty.Macast_Renderer, 'MPV')
        self.enabled_protocols = self._load_enabled_protocols()
        self.setting_protocol = self.enabled_protocols[0] if self.enabled_protocols else 'DLNA'
        if self.setting_check:
            threading.Thread(target=self.check_update,
                             kwargs={
                                 'verbose': False
                             },
                             daemon=True,
                             name="CHECKUPDATE_THREAD").start()

    def stop_cast(self):
        self.service.stop()

    def start_cast(self):
        self.service.run_async()

    def check_update(self, verbose=True):
        release_url = 'https://github.com/{}/releases/latest'.format(GITHUB_REPO)
        api_url = 'https://api.github.com/repos/{}/releases/latest'.format(GITHUB_REPO)
        try:
            res = json.loads(requests.get(api_url, timeout=10).text)
            # Strip leading 'v' and grab the first dot-separated version
            # triple. e.g. 'v0.7.2' -> '0.7.2'. Using tuple comparison so
            # that '0.7.10' correctly sorts above '0.7.2'.
            tag = res['tag_name'].lstrip('v')
            parts = tag.split('.')[:3]
            while len(parts) < 3:
                parts.append('0')
            online_version = tuple(int(p) for p in parts)

            logger.info("tag_name: {}".format(res['tag_name']))

            cur = Setting.get_version().lstrip('v')
            cur_parts = cur.split('.')[:3]
            while len(cur_parts) < 3:
                cur_parts.append('0')
            cur_tuple = tuple(int(p) for p in cur_parts)

            if cur_tuple < online_version:
                self.dialog(_("Macast New Update {}").format(res['tag_name']),
                            lambda: self.open_browser(release_url),
                            ok="Update")
            else:
                if verbose:
                    self.notification("Macast", _("You're up to date."))
        except Exception as e:
            logger.error("get update info error: {}".format(e))

    # The followings are the callback function of program event

    def update_service_status(self):
        if Setting.is_service_running():
            self.toggle_menuitem.text = _('Stop Cast')
        else:
            self.toggle_menuitem.text = _('Start Cast')
        self.update_menu()

    def service_start(self):
        """This function is called every time the DLNA service is started.
        Displays a notification reminding the user that the service has started
        """
        logger.info("service_start")
        if self.platform is Platform.Win32:
            msg = _("running at task bar")
        elif self.platform is Platform.Darwin:
            msg = _("running at menu bar")
        else:
            msg = _("running at desktop panel")
        if self.platform == Platform.Darwin:
            self.notification(_("Macast is hidden"), msg, sound=False)
        else:
            # Pystray may fail to send notifications due to incomplete initialization
            # during the startup, so wait a moment
            threading.Thread(target=lambda: (
                time.sleep(2),
                self.notification(_("Macast is hidden"), msg, sound=False),
            )).start()
        self.update_service_status()

    def service_stop(self):
        """This function is called every time the DLNA service is stopped.
        """
        logger.info("service_stop")
        self.update_service_status()

    def update_service_ip(self):
        """When the IP or port of the device changes,
        call this function to refresh the device address on the menu
        """
        logger.info("ssdp_update_ip")
        if self.ip_menuitem is not None:
            ip_text = "/".join([ip for ip, _ in Setting.get_ip()])
            port = Setting.get_port()
            self.ip_menuitem.text = "{}:{}".format(ip_text, port)
        self.version_menuitem.text = "{} v{}".format(Setting.get_friendly_name(), Setting.get_version())
        self.update_menu()

    def renderer_av_stop(self):
        logger.info("renderer_av_stop")
        if self.copy_menuitem:
            self.remove_menu_item_by_id(self.copy_menuitem.id)
        self.copy_menuitem = None

    def renderer_start(self):
        pass

    def renderer_av_uri(self, uri):
        logger.info("renderer_av_uri: " + uri)
        if self.copy_menuitem is not None:
            self.copy_menuitem.callback = lambda _: pyperclip.copy(uri)
            return
        self.copy_menuitem = MenuItem(
            _("Copy Video URI"),
            key="c",
            callback=lambda _: pyperclip.copy(uri))
        self.append_menu_item_after(self.toggle_menuitem.id, self.copy_menuitem)

    # The followings are the callback function of menu click

    def on_protocol_toggle_click(self, item):
        """Enable/disable one protocol while the others keep running."""
        plugin = self.plugin_manager.protocol_list[item.data]
        title = plugin.title

        if title in self.enabled_protocols:
            if len(self.enabled_protocols) == 1:
                # Disabling the last one would leave nothing listening; mpv
                # would stay up with no way to reach it.
                cherrypy.engine.publish(
                    'app_notify', _('Info'),
                    _('At least one protocol must stay enabled.'))
                item.checked = True
                return
            self.enabled_protocols.remove(title)
        else:
            self.enabled_protocols.append(title)
        # Keep menu order stable regardless of the order things were clicked.
        order = [p.title for p in self.plugin_manager.protocol_list]
        self.enabled_protocols.sort(key=lambda t: order.index(t)
                                    if t in order else len(order))
        self.save_enabled_protocols()
        item.checked = title in self.enabled_protocols

        self._apply_enabled_protocols()
        cherrypy.engine.publish(
            'app_notify', _('Info'),
            _('{}.').format('{} {}'.format(
                _('Enabled'), title) if item.checked else '{} {}'.format(
                _('Disabled'), title)))

    def _apply_enabled_protocols(self):
        """Start/stop protocols to match `enabled_protocols` in place.

        Deliberately not a full service restart: toggling Chromecast must not
        interrupt a DLNA stream already playing.
        """
        group = self.service.protocol
        running = Setting.is_service_running()

        for title in list(group.titles()):
            if title not in self.enabled_protocols:
                protocol = group.remove(title)
                if protocol is not None:
                    try:
                        protocol.stop()
                    except Exception as e:
                        logger.error("Stopping protocol %s failed: %s", title, e)

        for title in self.enabled_protocols:
            if title in group:
                continue
            instance = self.plugin_manager.get_protocol_instance(title)
            if instance is None:
                continue
            group.add(title, instance)
            if running:
                try:
                    instance.start()
                except Exception as e:
                    logger.error("Starting protocol %s failed: %s", title, e)

        # The CherryPy tree root follows the group's primary handler (DLNA's
        # handler supersedes the base one by serving the UPnP routes too), and
        # SSDP is only wanted while a SSDP-based protocol is enabled.
        self.service.refresh_protocol()
        self.setting_protocol = (self.enabled_protocols[0]
                                 if self.enabled_protocols else 'DLNA')
        self.setting_menuitem.children = self.build_setting_menu()
        self.set_menu(self.menu)

    def on_renderer_change_click(self, item):
        renderer_config = self.plugin_manager.renderer_list[item.data]
        self.service.renderer = renderer_config.get_instance()
        Setting.set(SettingProperty.Macast_Renderer, renderer_config.title)
        self.setting_renderer = renderer_config.title
        self.setting_menuitem.children = self.build_setting_menu()
        # reload menu
        self.set_menu(self.menu)
        cherrypy.engine.publish('app_notify', _('Info'), _('Change Renderer to {}.').format(renderer_config.title))

    def on_open_config_click(self, item):
        self.open_directory(SETTING_DIR)

    def on_check_click(self, item):
        threading.Thread(target=self.check_update,
                         daemon=True,
                         name="CHECKUPDATE_M_THREAD").start()

    def on_auto_check_update_click(self, item):
        item.checked = not item.checked
        Setting.set(SettingProperty.CheckUpdate,
                    1 if item.checked else 0)

    def on_start_at_login_click(self, item):
        res = Setting.set_start_at_login(not item.checked)
        if res[0] == 0:
            item.checked = not item.checked
            Setting.set(SettingProperty.StartAtLogin,
                        1 if item.checked else 0)
        else:
            self.notification(_("Error"), _(res[1]))

    def on_about_click(self, _):
        self.open_browser('http://127.0.0.1:{}?page=4'.format(Setting.get_port()))

    def on_toggle_service_click(self, item):
        if Setting.is_service_running():
            self.stop_cast()
        else:
            self.start_cast()

    def on_menubar_icon_change_click(self, item):
        for i in self.menubar_icon_menuitem.items():
            i.checked = False
        item.checked = True
        Setting.set(SettingProperty.MenubarIcon, item.data)
        icon_path = os.path.join(os.path.dirname(__file__), Macast.ICON_MAP[item.data])
        template = None if item.data == 0 else True
        self.update_icon(icon_path, template)

    def quit(self, item):
        if Setting.is_service_running():
            self.stop_cast()
        super(Macast, self).quit(item)


def gui(renderer=None, protocol=None, lang=gettext.gettext):
    if renderer is None:
        renderer = MPVRenderer(lang, Setting.mpv_default_path)
    if protocol is None:
        protocol = DLNAProtocol()
    Macast(renderer, protocol, lang).start()


def cli(renderer=None, protocol=None):
    if renderer is None:
        renderer = MPVRenderer(path=Setting.mpv_default_path)
    if protocol is None:
        protocol = DLNAProtocol()
    Service(renderer, protocol).run()
