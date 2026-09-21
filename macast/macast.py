# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.

import os
import re
import sys
import time
import json
import shutil
import cherrypy
import logging
import threading
import requests
import pyperclip
import gettext
import importlib

from .utils import SettingProperty, SETTING_DIR, notify_error, format_class_name
from . import logsplit
from . import plugin_repo
# This fork is distributed from pingod/Macast; the "check for updates" feature
# must query that repo, not the upstream xfangfang/Macast.
GITHUB_REPO = 'pingod/Macast'
from .gui import App, MenuItem, Platform
from .protocol import DLNAProtocol, Protocol
from .server import Service
from .utils import RENDERER_DIR, PROTOCOL_DIR, Setting
from macast_renderer.mpv import MPVRenderer

logger = logging.getLogger("main")
logger.setLevel(logging.DEBUG)
_ = gettext.gettext


def _import_plugin_module(dotted):
    """Import a plugin module, re-reading it if we already hold a stale copy.

    Hot install/update depends on this: importlib caches modules, so without
    the reload a re-installed plugin would keep running the old code until the
    process restarted -- exactly what hot-plug exists to avoid.
    """
    module = sys.modules.get(dotted)
    if module is None:
        return importlib.import_module(dotted)
    try:
        return importlib.reload(module)
    except Exception as e:
        logger.warning("Reload of %s failed (%s), importing a fresh copy",
                       dotted, e)
        sys.modules.pop(dotted, None)
        return importlib.import_module(dotted)


def _import_bundled_module(dotted):
    """Import a module that ships inside the package, exactly once.

    Deliberately *not* `_import_plugin_module`: that one reloads on every call
    so a re-installed user plugin picks up new code, but reloading a bundled
    module would re-execute its class bodies and hand out a *different* class
    object than the one an already-registered plugin holds. A plugin in use
    would then fail `isinstance` checks against its own class.
    """
    module = sys.modules.get(dotted)
    if module is not None:
        return module
    return importlib.import_module(dotted)


#: `<macast.key>value</macast.key>` pairs in a plugin's header comment. One
#: parser serves both user-installed files and bundled modules so the two
#: cannot drift apart.
#:
#: The value may not contain `<`, which is what makes this robust: a manifest
#: is a sequence of tags on consecutive lines, and letting the value span them
#: (as a plain `(.*?)` does, with DOTALL) means any later mention of a closing
#: tag -- in a comment documenting the format, say -- swallows the tags in
#: between. `[^<]*` simply cannot run past the next tag.
_PLUGIN_META_RE = re.compile(r"<macast\.([\w.]+)>([^<]*)</macast\.[\w.]+>")


def _plugin_header_comment(source):
    """Return the plugin's leading comment block, and nothing else.

    The manifest lives in the header, so there is no reason to scan the whole
    file: doing so only creates chances for unrelated code or documentation to
    be mistaken for manifest text.
    """
    lines = []
    for line in source.splitlines():
        stripped = line.strip()
        if stripped.startswith('#'):
            lines.append(stripped.lstrip('#').strip())
        elif stripped == '':
            continue
        else:
            break
    return '\n'.join(lines)


def _read_plugin_metadata(path):
    """Parse the ``<macast.*>`` manifest out of a plugin's header comment."""
    meta = {}
    try:
        with open(path, 'r', encoding='utf-8') as f:
            source = f.read()
    except OSError as e:
        logger.error("Cannot read plugin manifest from %s: %s", path, e)
        return meta
    for key, value in _PLUGIN_META_RE.findall(_plugin_header_comment(source)):
        meta[key.strip()] = value.strip()
    return meta


def _guess_plugin_class(module, kind):
    """Find a plugin class in `module` when its manifest names none.

    A manifest typo (the bundled PotPlayer header used to close its
    `<macast.renderer>` and `<macast.platform>` tags with `</macast.title>`)
    should cost a class lookup, not the whole plugin.
    """
    from .protocol import Protocol
    from .renderer import Renderer
    for name, value in vars(module).items():
        if not isinstance(value, type) or name.startswith('_'):
            continue
        if kind == 'protocol' and issubclass(value, Protocol):
            return value
        if kind == 'renderer' and issubclass(value, Renderer):
            return value
    return None



class MacastPlugin:

    def __init__(self, path, title="None", plugin_instance=None, platform='none',
                 desc='', plugin_factory=None, module=None, version='',
                 author=''):
        # path is allowed to be set to None only when renderer is macast default plugin
        self.path = path
        self.title = title
        self.plugin_class = None
        self.plugin_instance = plugin_instance
        #: Zero-argument callable building the instance on first use. Built-in
        #: renderers use this instead of an eager instance: IINA launches an
        #: external app and PotPlayer imports win32-only modules, so
        #: constructing one at startup is both wasteful and, off-platform,
        #: fatal.
        self.plugin_factory = plugin_factory
        #: Module the plugin was loaded from, when it has one. Several plugins
        #: are identified by the class they define, not by a file name.
        self.module = module
        self.platform = platform
        self.desc = desc
        self.version = version
        self.author = author
        # 'protocol' / 'renderer'. Known immediately for built-ins (from the
        # instance handed in); load_from_file() fills it in for file plugins.
        self.plugin_type = '' if plugin_instance is None else (
            'protocol' if isinstance(plugin_instance, Protocol) else 'renderer')
        if self.plugin_type == '' and module is not None:
            self.plugin_type = self._infer_type(module)
        if path:
            try:
                self.load_from_file(path)
            except Exception as e:
                cherrypy.engine.publish('app_notify', 'ERROR', 'Custom plugin load error.')
                logger.error(str(e))

    @staticmethod
    def _infer_type(module):
        """Guess whether a plugin module is a renderer or a protocol.

        A renderer is the common case, and `Renderer` is imported by every
        plugin file; a protocol is recognised by an exported class that is a
        `Protocol` subclass. Reading it off the module beats trusting a
        hand-written manifest tag.
        """
        for value in vars(module).values():
            if isinstance(value, type) and issubclass(value, Protocol) \
                    and value is not Protocol:
                return 'protocol'
        return 'renderer'

    def kind(self):
        """'protocol' or 'renderer'.

        Built-ins are constructed with an instance rather than a file, and
        nothing sets `self.protocol` / `self.renderer` on them, so the kind
        cannot be read off those attributes alone.
        """
        if self.plugin_type:
            return self.plugin_type
        if isinstance(self.plugin_instance, Protocol):
            return 'protocol'
        return 'renderer'

    @property
    def key(self):
        """Stable identifier used by the enable/disable/uninstall API.

        Built-ins are keyed by title (they have no file to remove); file
        plugins by ``<kind>:<basename>`` so a renderer and a protocol that
        happen to share a file name stay distinct.
        """
        if self.path is None:
            return "{}:{}".format(self.kind(), self.title)
        return "{}:{}".format(self.kind(),
                              os.path.splitext(os.path.basename(self.path))[0])

    @property
    def installed(self):
        """Whether this plugin came from a file the user can remove."""
        return self.path is not None

    def get_info(self):
        props = ['protocol', 'title', 'renderer', 'platform', 'version', 'author', 'desc']
        res = {'default': False}
        for i in props:
            res[i] = getattr(self, i, '')
        res['type'] = self.kind()
        res['key'] = self.key
        res['installed'] = self.installed
        res['can_uninstall'] = self.installed
        # Whether this plugin can run on the machine we are answering from.
        # The settings page greys out the ones that cannot, instead of hiding
        # them: "PotPlayer is here but Windows-only" is information worth
        # showing, while a card that silently vanished looks like a bug.
        # Derived from the declared platform list rather than check(), because
        # a bundled plugin for another OS is deliberately never imported and
        # so has no class to inspect.
        res['available'] = self.matches_platform(quiet=True) if self.platform else False
        res['bundled'] = self.path is None
        if self.path is None:
            res['default'] = True
            res['desc'] = self._builtin_desc(res['type'])
            # A bundled plugin has no file version of its own; it moves with
            # the app, so the app version is the honest answer.
            res['version'] = self.version or Setting.version
        return res

    def _builtin_desc(self, kind):
        """Description for a plugin that ships with Macast.

        The old wording was derived from the title, which produced
        "DLNA Protocol protocol (built-in)" and called the renderer a
        protocol. Callers may pass their own text instead.
        """
        if self.desc:
            return self.desc
        if kind == 'renderer':
            return 'Built-in renderer.'
        return 'Built-in protocol.'

    def get_instance(self):
        if self.plugin_instance is None:
            if self.plugin_class is not None:
                self.plugin_instance = self.plugin_class()
            elif self.plugin_factory is not None:
                self.plugin_instance = self.plugin_factory()
        return self.plugin_instance

    def check(self, quiet=False):
        """ Check if this renderer can run on your device
        """
        # A plugin we could neither import nor build has nothing to run. This
        # is the expected state for a bundled plugin belonging to another OS:
        # its manifest is registered but its module was never imported (see
        # _load_bundled_plugins), and the settings page still lists it greyed
        # out because get_info() reports `available` from the platform list.
        if self.plugin_class is None and self.plugin_factory is None:
            return False
        # self.platform is a comma-separated list like 'darwin,win32,linux'.
        # Use an explicit split+membership test instead of `in` on the raw
        # string, which would also match substrings incorrectly.
        return self.matches_platform(quiet=quiet)

    def matches_platform(self, quiet=False):
        """Whether this plugin declares support for the running OS.

        Kept separate from `check()` because the settings page must be able to
        ask "does this plugin fit this machine?" for a plugin whose module has
        deliberately not been imported yet (see `_load_builtin_renderers`).
        """
        if sys.platform in [p.strip() for p in self.platform.split(',')]:
            return True

        # `quiet` exists because get_info() calls this for every plugin on
        # every settings-page load: logging a warning per foreign plugin per
        # refresh drowned the log in complaints about plugins that are simply
        # not meant for this OS.
        if not quiet:
            logger.error("{} support platform: {}".format(self.title, self.platform))
            logger.error("{} is not suit for this system.".format(self.title))
        return False

    def load_from_file(self, path):
        self.path = path
        base_name = os.path.basename(path)[:-3]
        metadata = _read_plugin_metadata(path)
        # Logged rather than printed: the manager is refreshed on every
        # install/uninstall, and these used to spew the whole plugin
        # manifest onto the console each time the settings page reloaded.
        logger.debug("Loading plugin from %s", base_name)
        for key, value in metadata.items():
            logger.debug("  %-10s: %s", key, value)
            setattr(self, key, str(value))
        if hasattr(self, 'renderer'):
            self.plugin_type = 'renderer'
            module = _import_plugin_module(f'{RENDERER_DIR}.{base_name}')
            logger.debug("Loaded renderer %s from %s", self.renderer, base_name)
            self.plugin_class = getattr(module, self.renderer, None)
            # The 模块设置 panel asks the module for its SettingProperty enum;
            # without this the card was always empty for file plugins.
            self.module = module
        elif hasattr(self, 'protocol'):
            self.plugin_type = 'protocol'
            module = _import_plugin_module(f'{PROTOCOL_DIR}.{base_name}')
            logger.debug("Loaded protocol %s from %s", self.protocol, base_name)
            self.plugin_class = getattr(module, self.protocol, None)
            self.module = module
        else:
            logger.error(f"Cannot find any plugin in {base_name}")
            return
        # A plugin's own log goes to logs/<Logger>.log, not into macast.log --
        # a 30 fps mirror would bury the core lines (see macast/logsplit.py).
        logsplit.claim_from_module(module)


class MacastPluginManager:

    def __init__(self, renderer_default, protocol_default):
        sys.path.append(SETTING_DIR)
        self.create_plugin_dir(RENDERER_DIR)
        self.create_plugin_dir(PROTOCOL_DIR)
        self._renderer_default = renderer_default
        self._protocol_default = protocol_default
        # Built-in protocols are instantiated once and reused: refresh() must
        # not hand out new Chromecast/AirPlay objects, or the running
        # ProtocolGroup would keep talking to the old ones.
        self._builtin_protocols = self._load_builtin_protocols()
        # Bundled renderers are registered (not instantiated) once, for the
        # same reason, and because importing their modules is not free.
        self._builtin_renderers = self._load_builtin_renderers()
        self.renderer_all = []
        self.renderer_list = []
        self.protocol_list = []
        self.refresh()

    def refresh(self):
        """Rebuild the plugin lists from disk + settings.

        Install, uninstall and disable all funnel through here, so the
        in-memory lists can never drift from what is on disk and in
        macast_setting.json -- which is what makes hot-plug reliable without a
        restart.

        ``renderer_all`` keeps every renderer we found, including the ones the
        user switched off, so the page can still list them and switch them back
        on; ``renderer_list`` is what the rest of the app may select from.
        """
        disabled = set(Setting.get(SettingProperty.Disabled_Plugins, []) or [])
        installed = self.load_macast_plugin(RENDERER_DIR) or []
        renderers = [self._renderer_default] + self._builtin_renderers
        renderers += self._without_builtin_duplicates(installed)
        self.renderer_all = renderers
        self.renderer_list = [p for p in renderers if p.key not in disabled]
        if not self.renderer_list:
            # Never leave the app without a renderer.
            self.renderer_list = [self._renderer_default]

        protocols = [self._protocol_default] + self._builtin_protocols
        protocols += self._without_builtin_duplicates(
            self.load_macast_plugin(PROTOCOL_DIR) or [])
        self.protocol_list = protocols

    def _without_builtin_duplicates(self, installed):
        """Drop installed copies of plugins Macast now bundles itself.

        Every one of these started life in xfangfang/Macast-plugins and was
        installed by hand; now that they ship with the app, keeping both would
        list the same renderer twice -- once as a stale file, once as the
        maintained built-in -- and the menu would offer two identical players.
        The file itself is left alone (never deleted behind the user's back);
        it is simply not offered while the bundled version is present.
        """
        claimed_titles = set()
        claimed_classes = set()
        for plugin in self._builtin_renderers + self._builtin_protocols:
            claimed_titles.add(plugin.title)
            module = getattr(plugin, 'module', None)
            for name, value in vars(module).items() if module else ():
                if isinstance(value, type) and not name.startswith('_'):
                    claimed_classes.add(name)

        kept = []
        for plugin in installed:
            # Titles are user-editable, class names are not, so either match
            # is enough to call it a duplicate of something we now ship.
            exported = getattr(plugin.plugin_class, '__name__', None)
            if plugin.title in claimed_titles or exported in claimed_classes:
                logger.info("Not offering installed plugin %s (%s): Macast "
                            "now bundles it", plugin.title, exported or plugin.title)
                continue
            kept.append(plugin)
        return kept

    def plugin_by_key(self, key):
        """Find a plugin by its `key`, including ones that are switched off."""
        for plugin in self.renderer_all + self.protocol_list:
            if plugin.key == key:
                return plugin
        return None

    def is_plugin_enabled(self, plugin):
        if plugin.kind() == 'renderer':
            return any(p.key == plugin.key for p in self.renderer_list)
        return plugin.title in self.enabled_protocol_titles()

    def set_protocol_enabled(self, title, enabled):
        """Switch a protocol on or off.

        Refuses to switch off the last one: with every protocol off mpv stays
        up but nothing can reach it, which reads as "Macast is broken" rather
        than "casting is off".
        """
        titles = self.enabled_protocol_titles()
        resolved = self.resolve_title(title)
        if resolved is None:
            # Unknown/uninstalled plugin: just make sure it is not persisted.
            titles = [t for t in titles if t != title]
        elif enabled:
            if resolved not in titles:
                titles.append(resolved)
        else:
            if resolved not in titles:
                return False
            if len(titles) <= 1:
                raise ValueError('至少要保留一个启用的协议')
            titles.remove(resolved)
        order = [p.title for p in self.protocol_list]
        titles.sort(key=lambda t: order.index(t) if t in order else len(order))
        Setting.set(SettingProperty.Macast_Protocols, titles)
        return True

    def set_renderer_enabled(self, key, enabled):
        """Load/unload a renderer plugin.

        A renderer is only ever instantiated on demand, so "unloaded" has no
        visible effect unless we also stop offering it for selection. Refusing
        to unload the renderer in use is what keeps the app with a player.
        """
        plugin = self.plugin_by_key(key)
        if plugin is None:
            raise ValueError('未找到该插件')
        if plugin.kind() != 'renderer':
            raise ValueError('该插件不是渲染器')
        disabled = list(Setting.get(SettingProperty.Disabled_Plugins, []) or [])
        if enabled:
            if key in disabled:
                disabled.remove(key)
            Setting.set(SettingProperty.Disabled_Plugins, disabled)
            self.refresh()
            return True
        if key in disabled:
            return True
        # Checked before the "last one" rule so the message the user sees is
        # the actionable one: switch renderer first, rather than "keep one".
        if Setting.get(SettingProperty.Macast_Renderer, '') == plugin.title:
            raise ValueError('正在使用的渲染器不能停用，请先切换到其他渲染器')
        if len(self.renderer_list) <= 1:
            raise ValueError('至少要保留一个可用的渲染器')
        disabled.append(key)
        Setting.set(SettingProperty.Disabled_Plugins, disabled)
        self.refresh()
        return True

    def uninstall(self, key):
        """Remove an installed plugin from disk (recoverably) and from memory.

        Built-ins are refused -- they ship with Macast and have no file to
        remove. The file is *moved* to ``SETTING_DIR/.trash/<stamp>/`` rather
        than deleted, so a mistaken uninstall stays recoverable.
        """
        plugin = self.plugin_by_key(key)
        if plugin is None:
            raise ValueError('未找到该插件')
        if not plugin.installed:
            raise ValueError('内置插件不能卸载，只能停用')
        source = plugin.path
        if not os.path.exists(source):
            raise ValueError('插件文件不存在：{}'.format(source))
        stamp = time.strftime('%Y%m%d-%H%M%S')
        trash_dir = os.path.join(SETTING_DIR, '.trash', stamp)
        os.makedirs(trash_dir, exist_ok=True)
        shutil.move(source, os.path.join(trash_dir, os.path.basename(source)))
        logger.info('Uninstalled plugin %s (moved to %s)', key, trash_dir)

        # Drop the settings that referenced a plugin which no longer exists, so
        # the plugin cannot reappear as a phantom menu entry.
        titles = self.enabled_protocol_titles()
        if plugin.kind() == 'protocol' and plugin.title in titles:
            titles = [t for t in titles if t != plugin.title]
            Setting.set(SettingProperty.Macast_Protocols, titles)
        disabled = [k for k in (Setting.get(SettingProperty.Disabled_Plugins, []) or [])
                    if k != key]
        Setting.set(SettingProperty.Disabled_Plugins, disabled)
        # Forget the module too: a reinstall must pick up the new file.
        sys.modules.pop('{}.{}'.format(plugin.kind(),
                                       os.path.splitext(os.path.basename(source))[0]),
                        None)
        self.refresh()
        return plugin

    def install_file(self, source_path, plugin_type, filename=None):
        """Install a plugin ``.py`` into the user plugin dir and load it live.

        Returns the loaded plugin. The file is rolled back if it does not load,
        so a bad plugin cannot leave a half-installed entry behind that fails
        on every later start.
        """
        plugin_type = 'protocol' if plugin_type == 'protocol' else 'renderer'
        filename = os.path.basename(filename or source_path)
        if not filename.endswith('.py'):
            raise ValueError('插件文件必须是 .py')
        target_dir = os.path.join(SETTING_DIR, plugin_type)
        self.create_plugin_dir(plugin_type)
        target = os.path.join(target_dir, filename)
        if os.path.abspath(source_path) != os.path.abspath(target):
            shutil.copyfile(source_path, target)
        self.refresh()
        key = "{}:{}".format(plugin_type, filename[:-3])
        plugin = self.plugin_by_key(key)
        if plugin is None or not plugin.check():
            try:
                os.remove(target)
            except OSError:
                pass
            self.refresh()
            raise ValueError('插件无法加载或不适配当前系统')
        if plugin_type == 'protocol':
            self.set_protocol_enabled(plugin.title, True)
        logger.info('Installed plugin %s from %s', plugin.title, source_path)
        return plugin

    def install_url(self, url, plugin_type):
        """Download a plugin from `url` then install it hot."""
        plugin_type = 'protocol' if plugin_type == 'protocol' else 'renderer'
        if not url:
            raise ValueError('缺少插件下载地址')
        # Mirror mode rewrites canonical GitHub hosts only; the SHA-pinned
        # jsDelivr URLs the index ships with pass through unchanged.
        url = plugin_repo.mirror_url(url)
        filename = os.path.basename(url.split('?')[0])
        if not filename.endswith('.py'):
            raise ValueError('插件下载地址必须以 .py 结尾')
        local_path = os.path.join(SETTING_DIR, plugin_type, filename)
        os.makedirs(os.path.dirname(local_path), exist_ok=True)
        response = requests.get(url, timeout=30)
        response.raise_for_status()
        with open(local_path, 'wb') as f:
            f.write(response.content)
        return self.install_file(local_path, plugin_type, filename)

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

    def enabled_protocol_titles(self):
        """Canonical titles of the protocols currently switched on.

        Reads the persisted selection the same way `Macast` does, so the plugin
        page can show which protocols are live without duplicating the
        migration rules in the frontend.
        """
        stored = Setting.get(SettingProperty.Macast_Protocols, [])
        # has() rather than get(): get(Macast_Protocol, None) would *create* the
        # key as JSON null, which is exactly what the migration removes.
        legacy = (Setting.get(SettingProperty.Macast_Protocol, '')
                  if Setting.has(SettingProperty.Macast_Protocol) else '')
        if isinstance(stored, str):
            stored = [stored]
        elif not isinstance(stored, list):
            stored = []
        if isinstance(legacy, str):
            stored = list(stored) + [legacy]
        titles = []
        for name in stored:
            resolved = self.resolve_title(name)
            if resolved and resolved not in titles:
                titles.append(resolved)
        return titles

    def get_info(self):
        """Every known plugin, with the state the settings page needs.

        Renderers are listed from ``renderer_all`` rather than
        ``renderer_list`` so a renderer the user switched off is still visible
        (and can be switched back on); ``enabled`` says which state it is in.
        """
        res = []
        active_renderer = Setting.get(SettingProperty.Macast_Renderer, '')
        for r in self.renderer_all:
            info = r.get_info()
            info['enabled'] = self.is_plugin_enabled(r)
            info['active'] = info.get('title') == active_renderer
            res.append(info)
        enabled_protocols = self.enabled_protocol_titles()
        for p in self.protocol_list:
            info = p.get_info()
            info['enabled'] = info.get('title') in enabled_protocols
            info['active'] = info['enabled']
            res.append(info)
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
        plugins = [
            MacastPlugin(None, "Chromecast", ChromecastProtocol(), "darwin,win32,linux",
                         desc='接收 Google Cast v2 投屏（Chrome、VLC、Android 等）'),
            MacastPlugin(None, "AirPlay", AirPlayProtocol(), "darwin,win32,linux",
                         desc='接收 AirPlay 视频投屏（iPhone / iPad / macOS）'),
        ]
        plugins += self._load_bundled_plugins('protocol')
        return plugins

    def _load_builtin_renderers(self):
        """Renderers bundled with Macast.

        Same idea as `_load_builtin_protocols`, but renderers are registered
        lazily: a bundled renderer is only constructed when the user actually
        picks it. IINA spawns an external app and PotPlayer imports win32-only
        modules, so building them at startup would be wrong even on the right
        platform.
        """
        return self._load_bundled_plugins('renderer')

    def _load_bundled_plugins(self, kind, directory=None):
        """Discover plugins under ``macast/plugins/<kind>/``.

        Each module declares itself in a ``<macast.*>`` header comment, exactly
        like a user-installed plugin, so one parser serves both. The module is
        imported here (cheap: these modules only define classes) but the plugin
        instance is not built until something asks for it.

        Modules whose declared platform does not match this OS are still
        registered -- the settings page shows them greyed out with the reason,
        which is far more useful than having them silently vanish.

        `directory` is injectable so the verification suite can exercise the
        loader against fixtures without touching the shipped plugin tree.
        """
        if directory is None:
            directory = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                     'plugins', kind)
        if not os.path.isdir(directory):
            return []
        plugins = []
        for entry in sorted(os.listdir(directory)):
            if not entry.endswith('.py') or entry == '__init__.py':
                continue
            base_name = entry[:-3]
            source = os.path.join(directory, entry)
            meta = _read_plugin_metadata(source)
            title = meta.get('title') or base_name.replace('_', ' ').title()
            platform = meta.get('platform', 'darwin,win32,linux')

            # Read the manifest *before* importing, and skip the import
            # entirely for a foreign platform. This is not an optimisation: the
            # PotPlayer plugin imports `win32api` at module level and the
            # Raspberry Pi one shells out to `sudo pi_fm_rds`, so importing
            # them off-platform would raise (or worse, half-run) rather than
            # merely being unused. Registering the metadata alone still lets
            # the settings page show the plugin, greyed out with its reason.
            factory = None
            module = None
            if sys.platform in [p.strip() for p in platform.split(',')]:
                dotted = '{}.plugins.{}.{}'.format(__package__, kind, base_name)
                try:
                    module = _import_bundled_module(dotted)
                except Exception as e:
                    logger.error("Bundled %s plugin %s failed to import: %s",
                                 kind, entry, e)
                    continue
                class_name = meta.get('renderer') or meta.get('protocol')
                factory = getattr(module, class_name, None) if class_name else None
                if factory is None:
                    # Fall back to inferring the class from the module body
                    # rather than dropping a plugin over a header typo.
                    factory = _guess_plugin_class(module, kind)
                if factory is None:
                    logger.error("Bundled %s plugin %s exports no plugin class",
                                 kind, entry)
                    continue
                logsplit.claim_from_module(module)
            plugins.append(MacastPlugin(
                None, title,
                platform=platform,
                desc=meta.get('desc', ''),
                plugin_factory=factory,
                module=module,
                version=meta.get('version', ''),
                author=meta.get('author', ''),
            ))
        return plugins

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
            MacastPlugin(None, format_class_name(renderer), renderer, 'darwin,win32,linux',
                         desc='使用 mpv 播放收到的媒体'),
            MacastPlugin(None, format_class_name(protocol), protocol, 'darwin,win32,linux',
                         desc='接收 DLNA / UPnP 投屏（智能电视、音乐 App 常用）'))

        cherrypy.engine.subscribe('get_plugin_info', self.plugin_manager.get_info)
        # The settings page drives plugin hot-plug and the network picker
        # through the management API, which lives on the protocol Handler and
        # therefore cannot reach the app object directly. Expose the manager
        # for reads, and listen for the change notifications to apply them
        # live (this is what makes it hot rather than "restart to take effect").
        cherrypy.engine.subscribe('get_plugin_manager', self.get_plugin_manager)
        cherrypy.engine.subscribe('plugins_changed', self.on_plugins_changed)
        cherrypy.engine.subscribe('network_interface_changed',
                                  self.on_network_interface_changed)

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
        titles = self.plugin_manager.enabled_protocol_titles()
        if titles:
            # Rewrite in canonical form so the settings file (and the plugin
            # page) stop showing a short name that the code no longer uses.
            if Setting.get(SettingProperty.Macast_Protocols, None) != titles:
                Setting.set(SettingProperty.Macast_Protocols, titles)
            # The old scalar is now redundant; leaving it behind makes the
            # Advanced Setting page show two contradictory keys ("AirPlay"
            # next to ["DLNA","Chromecast","AirPlay"]) and invites edits to
            # the wrong one.
            if Setting.has(SettingProperty.Macast_Protocol):
                Setting.unset(SettingProperty.Macast_Protocol)
                logger.info("Migrated Macast_Protocol into Macast_Protocols")
            return titles

        # Nothing usable yet: a fresh install gets every protocol at once.
        available = [p.title for p in self.plugin_manager.protocol_list]
        Setting.set(SettingProperty.Macast_Protocols, list(available))
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

    def get_plugin_manager(self):
        """Bus accessor so the management API can read/modify plugins."""
        return self.plugin_manager

    def on_plugins_changed(self):
        """Apply a plugin install / uninstall / enable / disable immediately.

        Called after the management API has already updated settings and the
        plugin directory. Everything else -- the live protocol group, the
        renderer in use, the menu -- is re-derived here, which is what makes
        the change take effect without restarting Macast.
        """
        logger.info("plugins changed, re-applying")
        self.plugin_manager.refresh()
        # Protocols are driven by the persisted list, so pick it back up.
        self.enabled_protocols = self.plugin_manager.enabled_protocol_titles()
        if not self.enabled_protocols:
            available = [p.title for p in self.plugin_manager.protocol_list]
            self.enabled_protocols = list(available)
            Setting.set(SettingProperty.Macast_Protocols, list(available))
        # A renderer that was just uninstalled or switched off must not stay
        # selected, or the service would hold a player nobody can reach.
        renderer_titles = [r.title for r in self.plugin_manager.renderer_list]
        if self.setting_renderer not in renderer_titles:
            fallback = renderer_titles[0] if renderer_titles else None
            if fallback is not None:
                logger.info("Renderer %r unavailable, switching to %r",
                            self.setting_renderer, fallback)
                self.setting_renderer = fallback
                self.service.renderer = self.plugin_manager.get_renderer(fallback)
        self._apply_enabled_protocols()
        cherrypy.engine.publish('app_notify', _('Info'), _('Plugins updated.'))

    def on_network_interface_changed(self):
        """Re-advertise on the interface the user just picked.

        SSDP rebinds through its own update channel; mDNS belongs to each
        protocol, so the running ones are cycled once. Cycling does not touch
        mpv, so anything playing keeps playing.
        """
        logger.info("network interface changed -> %r",
                    Setting.get_network_interface() or 'auto')
        try:
            cherrypy.engine.publish('ssdp_update_ip')
        except Exception as e:  # pragma: no cover - defensive
            logger.error("SSDP update after interface change failed: %s", e)
        group = self.service.protocol
        for title in list(group.titles()):
            protocol = group.get(title)
            if protocol is None:
                continue
            try:
                protocol.stop()
                protocol.start()
            except Exception as e:
                logger.error("Re-advertising %s failed: %s", title, e)
        self.update_service_ip()

    def stop_cast(self):
        self.service.stop()

    def start_cast(self):
        self.service.run_async()

    def check_update(self, verbose=True):
        release_url = plugin_repo.mirror_url(
            'https://github.com/{}/releases/latest'.format(GITHUB_REPO))
        api_url = plugin_repo.mirror_url(
            'https://api.github.com/repos/{}/releases/latest'.format(GITHUB_REPO))
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
        self.call_on_main_thread(self._refresh_ip_menu)

    def _refresh_ip_menu(self):
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
        known = [p.title for p in self.plugin_manager.protocol_list]

        for title in list(group.titles()):
            # Drop what the user switched off *and* what is no longer
            # installed: an uninstalled plugin must stop serving immediately,
            # not linger in the group until the next restart.
            if title not in self.enabled_protocols or title not in known:
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
        # May be reached from a CherryPy worker thread (settings page), so the
        # menu rebuild is marshalled to the UI thread.
        self.call_on_main_thread(self._rebuild_menu)

    def _rebuild_menu(self):
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
