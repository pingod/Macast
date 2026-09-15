# Copyright (c) 2021 by xfangfang. All Rights Reserved.

import os
import sys
import uuid
import json
import time
import ctypes
import appdirs
import logging
import platform
import locale
import cherrypy
import subprocess
from enum import Enum
import netifaces as ni
from ._version import __version__ as _PACKAGE_VERSION

if sys.platform == 'darwin':
    from AppKit import NSBundle
# Note: Windows registry access is handled via the stdlib `winreg` module
# inside `Setting.set_start_at_login`. We deliberately avoid `pywin32`
# (`win32api` / `win32con`) because shipping it in the PyInstaller bundle
# would add ~14 MB of binary wheels and the v0.7.2 Windows CI does not
# install it — leading to an immediate ModuleNotFoundError on launch.

logger = logging.getLogger("Utils")
DEFAULT_PORT = 58880
SETTING_DIR = appdirs.user_config_dir('Macast', 'xfangfang')
PROTOCOL_DIR = 'protocol'
RENDERER_DIR = 'renderer'


class SettingProperty(Enum):
    USN = 0
    CheckUpdate = 1
    StartAtLogin = 2
    MenubarIcon = 3
    ApplicationPort = 4
    DLNA_FriendlyName = 5
    Macast_Renderer = 6
    Macast_Protocol = 7
    Blocked_Interfaces = 8
    Additional_Interfaces = 9
    Play_History = 10
    Https_Enabled = 11
    Https_Port = 12
    Https_Cert = 13
    Https_Key = 14
    # Protocols enabled at the same time (list of titles). Replaces the older
    # single-value Macast_Protocol, which is still read as a migration source.
    Macast_Protocols = 15
    # Interface name (e.g. 'en0') the user pinned for discovery/advertisement.
    # Empty string means automatic: whichever interface carries the IPv4
    # default route.
    Network_Interface = 16
    # Plugin keys switched off. Protocols live in Macast_Protocols instead;
    # this covers renderer plugins, which are only ever instantiated on demand
    # and so need their own off-switch to be "unloaded" rather than "unused".
    Disabled_Plugins = 17


def unadvertisable_reason(addr, netmask):
    """Why `addr` must not be published to other devices, or '' if it may.

    Single source of truth for the address filter. The network picker shows
    this text to the user and discovery applies the same test, so the two can
    never disagree about which interfaces are usable -- which is exactly the
    bug that made Macast "discoverable but not castable" (mDNS published an
    address from a VM bridge or a Tailscale tunnel, and the sender dialled it).
    """
    if not addr:
        return '没有 IPv4 地址'
    if addr.startswith('127.'):
        return '回环地址，其他设备不可达'
    if addr.startswith('169.254.'):
        return '链路本地地址，未取得 DHCP 租约'
    if netmask in ('255.255.255.255', '32'):
        return '点对点地址（/32），不属于局域网'
    parts = str(addr).split('.')
    if len(parts) != 4:
        return '非 IPv4 地址'
    try:
        if parts[0] == '100' and 64 <= int(parts[1]) <= 127:
            return '共享地址空间（Tailscale 等虚拟网卡）'
    except ValueError:
        return '非 IPv4 地址'
    return ''


def is_advertisable_address(addr, netmask):
    """Whether an IPv4 address is worth publishing on a LAN."""
    return not unadvertisable_reason(addr, netmask)


class Setting:
    setting = {}
    version = None
    setting_path = os.path.join(SETTING_DIR, "macast_setting.json")
    last_ip = None
    base_path = None
    friendly_name = "Macast({})".format(platform.node())
    temp_friendly_name = None
    mpv_default_path = 'mpv'

    @staticmethod
    def save():
        """Save user settings
        """
        if not os.path.exists(SETTING_DIR):
            os.makedirs(SETTING_DIR)
        with open(Setting.setting_path, "w") as f:
            json.dump(obj=Setting.setting, fp=f, sort_keys=True, indent=4)

    @staticmethod
    def load():
        """Load user settings
        """
        logger.info("Load Setting")
        if Setting.version is None:
            try:
                with open(Setting.get_base_path('.version'), 'r') as f:
                    Setting.version = f.read().strip()
            except FileNotFoundError:
                Setting.version = _PACKAGE_VERSION
        if bool(Setting.setting) is False:
            if not os.path.exists(Setting.setting_path):
                Setting.setting = {}
            else:
                try:
                    with open(Setting.setting_path, "r") as f:
                        Setting.setting = json.load(fp=f)
                    logger.info("Loaded settings from %s", Setting.setting_path)
                except Exception as e:
                    logger.error(e)
        return Setting.setting

    @staticmethod
    def reload():
        Setting.setting = None
        Setting.load()

    @staticmethod
    def get_system_version():
        """Get system version
        """
        return str(platform.release())

    @staticmethod
    def get_system():
        """Get system name
        """
        return str(platform.system())

    @staticmethod
    def get_version():
        """Get application version
        """
        return Setting.version

    @staticmethod
    def get_friendly_name():
        """Get application friendly name
        This name will show in the device search list of the DLNA client
        and as player window default name.
        """
        if Setting.temp_friendly_name:
            return Setting.temp_friendly_name
        return Setting.get(SettingProperty.DLNA_FriendlyName, Setting.friendly_name)

    @staticmethod
    def set_temp_friendly_name(name):
        Setting.temp_friendly_name = name

    @staticmethod
    def get_usn(refresh=False):
        """Get device unique identification
        """
        dlna_id = str(uuid.uuid4())
        if not refresh:
            # Return the persisted USN, generating and storing one if absent.
            return Setting.get(SettingProperty.USN, dlna_id)
        else:
            Setting.set(SettingProperty.USN, dlna_id)
            return dlna_id

    @staticmethod
    def is_ip_changed():
        if Setting.last_ip != Setting.get_ip():
            return True
        return False

    @staticmethod
    def get_network_interface():
        """Interface the user pinned for discovery, or '' for automatic."""
        value = Setting.get(SettingProperty.Network_Interface, '')
        return value.strip() if isinstance(value, str) else ''

    @staticmethod
    def set_network_interface(name):
        """Pin discovery to `name`; '' restores automatic selection."""
        Setting.set(SettingProperty.Network_Interface,
                    str(name or '').strip())

    @staticmethod
    def resolved_network_interface():
        """The pinned interface if it can actually carry discovery, else ''.

        An explicit choice is honoured only while it has a LAN-reachable IPv4.
        Pinning an interface that has gone away, or that only carries a
        link-local / point-to-point / overlay address, would make Macast
        silently invisible; falling back to automatic selection keeps it
        reachable, and the picker shows the reason so the state is visible.
        """
        pinned = Setting.get_network_interface()
        if not pinned:
            return ''
        try:
            rows = [r for r in Setting.network_interfaces()
                    if r['name'] == pinned]
        except Exception as e:  # pragma: no cover - defensive
            logger.warning("Could not verify pinned interface %r: %s", pinned, e)
            return ''
        if not rows:
            logger.warning("Pinned interface %r not found; using automatic "
                           "selection", pinned)
            return ''
        if not any(r['usable'] for r in rows):
            logger.warning("Pinned interface %r has no LAN-reachable IPv4 "
                           "(%s); using automatic selection",
                           pinned, rows[0]['reason'])
            return ''
        return pinned

    @staticmethod
    def network_interfaces():
        """Every local IPv4 interface, annotated for the network picker.

        ``usable`` mirrors ``unadvertisable_reason()`` and ``reason`` explains
        a rejection, so the page can answer "why can't I pick this one?"
        without duplicating the rules in JavaScript.
        """
        pinned = Setting.get_network_interface()
        default_ifaces = set()
        try:
            gateways = ni.gateways() or {}
            for entry in gateways.get(ni.AF_INET, []):
                # ('192.168.1.1', 'en0', True) -- flag marks the default route
                if len(entry) > 1 and (len(entry) < 3 or entry[2]):
                    default_ifaces.add(entry[1])
        except Exception as e:  # pragma: no cover - defensive
            logger.debug("Could not read IPv4 gateways: %s", e)
        try:
            names = list(ni.interfaces())
        except Exception as e:  # pragma: no cover - defensive
            logger.warning("Could not enumerate interfaces: %s", e)
            names = []
        rows = []
        for name in names:
            try:
                addrs_v4 = ni.ifaddresses(name).get(ni.AF_INET, [])
            except (ValueError, OSError):
                continue
            for entry in addrs_v4:
                addr = entry.get('addr')
                if not addr:
                    continue
                netmask = entry.get('netmask', '')
                reason = unadvertisable_reason(addr, netmask)
                rows.append({
                    'name': name,
                    'ip': addr,
                    'netmask': netmask,
                    'default': name in default_ifaces,
                    'usable': not reason,
                    'reason': reason,
                    'selected': name == pinned,
                })
        # Default-route interface first, then anything else usable, so the row
        # a user almost always wants is the one at the top.
        rows.sort(key=lambda r: (not r['default'], not r['usable'],
                                 r['name'], r['ip']))
        return rows

    @staticmethod
    def get_ip():
        """(addr, netmask) pairs this host may advertise.

        A pinned interface narrows this to that interface alone. That is what
        makes the picker meaningful rather than cosmetic: SSDP only answers a
        peer whose address falls in the same subnet as an entry here, so
        pinning also stops Macast replying over bridges and tunnels the user
        never wanted it on.
        """
        last_ip = []
        # resolved_* rather than the raw pin: an unusable pin must not take
        # DLNA down with it any more than it takes mDNS down.
        pinned = Setting.resolved_network_interface()
        gateways = ni.gateways()  # {type: [{ip, interface, default},{},...], type: []}
        if pinned:
            # Additional_Interfaces must not widen an explicit choice.
            interfaces = {pinned}
        else:
            interfaces = set(Setting.get(SettingProperty.Additional_Interfaces, []))
            interface_type = [ni.AF_INET, ni.AF_LINK]
            for t in interface_type:
                if t in gateways:
                    for i in gateways[t]:
                        if len(i) > 1:
                            interfaces.add(i[1])
            for i in Setting.get(SettingProperty.Blocked_Interfaces, []):
                if i in interfaces:
                    interfaces.remove(i)
        logger.debug(interfaces)
        for i in interfaces:
            try:
                iface = ni.ifaddresses(i)
            except ValueError as e:
                continue
            if ni.AF_INET in iface:
                for j in iface[ni.AF_INET]:
                    if 'addr' in j and 'netmask' in j:
                        last_ip.append((j['addr'], j['netmask']))
        Setting.last_ip = set(last_ip)
        logger.debug(Setting.last_ip)
        return Setting.last_ip

    @staticmethod
    def get_advertisable_ip():
        """The addresses discovery will actually publish, as a list.

        Falls back to the unfiltered set so a host whose only address looks
        unusual to us still advertises something rather than nothing.
        """
        addrs = [ip for ip, mask in Setting.get_ip()
                 if is_advertisable_address(ip, mask)]
        if addrs:
            return addrs
        return [ip for ip, _mask in Setting.get_ip()]

    @staticmethod
    def get_port():
        """Get application port
        """
        return Setting.get(SettingProperty.ApplicationPort, DEFAULT_PORT)

    @staticmethod
    def is_https_enabled():
        """Whether the HTTPS admin/Web channel is enabled (default: on).
        """
        return bool(Setting.get(SettingProperty.Https_Enabled, 1))

    @staticmethod
    def get_https_port():
        """HTTPS listen port. Defaults to the DLNA port + 1 (not persisted,
        so it always tracks the actual DLNA port)."""
        if SettingProperty.Https_Port.name in Setting.setting:
            return Setting.setting[SettingProperty.Https_Port.name]
        return Setting.get_port() + 1

    @staticmethod
    def get_https_cert():
        return Setting.get(SettingProperty.Https_Cert, '')

    @staticmethod
    def get_https_key():
        return Setting.get(SettingProperty.Https_Key, '')

    @staticmethod
    def get_locale():
        """Get the language settings of the system
        Default: en_US
        """
        if sys.platform == 'darwin':
            lang = subprocess.check_output(
                ["osascript", "-e",
                 "user locale of (get system info)"]).decode().strip()
        elif sys.platform == 'win32':
            windll = ctypes.windll.kernel32
            lang = locale.windows_locale[windll.GetUserDefaultUILanguage()]
        else:
            lang = os.environ.get('LANGUAGE')
            if lang is None:
                lang = os.environ['LANG']
            if lang is None:
                return 'en_US'
            lang = lang.split(':')[0].split('.')[0]
        return lang

    @staticmethod
    def get(property, default=1):
        """Get application settings
        """
        if not bool(Setting.setting):
            Setting.load()
        if property.name in Setting.setting:
            return Setting.setting[property.name]
        Setting.setting[property.name] = default
        return default

    @staticmethod
    def set(property, data):
        """Set application settings
        """
        Setting.setting[property.name] = data
        Setting.save()

    @staticmethod
    def has(property):
        """Whether a key is present, without get()'s insert-on-read side effect.

        Setting.get() stores the default it was handed when the key is missing,
        so asking for a key with a None default *creates* it -- as JSON `null`.
        Use this to test for presence.
        """
        if not bool(Setting.setting):
            Setting.load()
        return property.name in Setting.setting

    @staticmethod
    def unset(property):
        """Remove a settings key outright.

        Setting.set(key, None) would persist a JSON `null`, which reads back as
        a present-but-empty value; a migration wants the key gone. Note the
        save happens whenever the key existed, even if its value was falsy.
        """
        if not bool(Setting.setting):
            Setting.load()
        if property.name in Setting.setting:
            Setting.setting.pop(property.name)
            Setting.save()

    @staticmethod
    def system_shell(shell):
        result = subprocess.run(shell, stdout=subprocess.PIPE)
        return result.returncode, result.stdout.decode('UTF-8').strip()

    @staticmethod
    def set_start_at_login(launch):
        if sys.platform == 'darwin':
            app_path = NSBundle.mainBundle().bundlePath()
            if not app_path.startswith("/Applications"):
                return (1, "You need move Macast.app to Applications folder.")
            app_name = app_path.split("/")[-1].split(".")[0]
            res = Setting.system_shell(
                ['osascript',
                 '-e',
                 'tell application "System Events" ' +
                 'to get the name of every login item'])
            if res[0] == 1:
                return (1, "Cannot access System Events.")
            apps = list(map(lambda app: app.strip(), res[1].split(",")))
            # apps which start at login
            if launch:
                if app_name in apps:
                    return (0, "Macast is already in login items.")
                res = Setting.system_shell(
                    ['osascript',
                     '-e',
                     'tell application "System Events" ' +
                     'to make login item at end with properties ' +
                     '{{name: "{}",path:"{}", hidden:false}}'.format(
                         app_name, app_path)
                     ])
            else:
                if app_name not in apps:
                    return (0, "Macast is already not in login items.")
                res = Setting.system_shell(
                    ['osascript',
                     '-e',
                     'tell application "System Events" ' +
                     'to delete login item "{}"'.format(app_name)])
            return res
        elif sys.platform == 'win32':
            """Find the path of Macast.exe so as to create shortcut.
            Uses the stdlib `winreg` module instead of `pywin32` to keep
            the Windows binary lean (pywin32 wheels are ~14 MB).
            """
            if "python" in os.path.basename(sys.executable).lower():
                return (1, "Not support to set start at login.")

            import winreg
            registry_path = r'Software\Microsoft\Windows\CurrentVersion\Run'
            logger.info(sys.executable)
            if launch:
                try:
                    key = winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                                         registry_path,
                                         0,
                                         winreg.KEY_SET_VALUE)
                    winreg.SetValueEx(key, 'Macast', 0, winreg.REG_SZ, sys.executable)
                    winreg.CloseKey(key)
                except Exception as e:
                    logger.error(e)
                    # cherrypy.engine.publish("app_notify", "ERROR", f"{e}")
                return 0, 1
            else:
                try:
                    key = winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                                         registry_path,
                                         0,
                                         winreg.KEY_SET_VALUE)
                    winreg.DeleteValue(key, 'Macast')
                    winreg.CloseKey(key)
                except FileNotFoundError:
                    # Value didn't exist; nothing to do
                    pass
                except Exception as e:
                    logger.error(e)
                    # cherrypy.engine.publish("app_notify", "ERROR", f"{e}")
                return 0, 1
        else:
            return (1, 'Not support current platform.')

    @staticmethod
    def get_base_path(path="."):
        """PyInstaller creates a temp folder and stores path in _MEIPASS
            https://stackoverflow.com/a/13790741
            see also: https://pyinstaller.readthedocs.io/en/stable/\
                runtime-information.html#run-time-information
        """
        if Setting.base_path is not None:
            return os.path.join(Setting.base_path, path)
        if getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
            Setting.base_path = sys._MEIPASS
        else:
            Setting.base_path = os.path.join(os.path.dirname(__file__), '.')
        return os.path.join(Setting.base_path, path)

    @staticmethod
    def get_server_info():
        return '{}/{} UPnP/1.0 Macast/{}'.format(Setting.get_system(),
                                                 Setting.get_system_version(),
                                                 Setting.get_version())

    @staticmethod
    def get_system_env():
        # Get system env(for GNU/Linux and *BSD).
        # https://pyinstaller.readthedocs.io/en/stable/runtime-information.html#run-time-information
        env = dict(os.environ)
        logger.debug(env)
        lp_key = 'LD_LIBRARY_PATH'
        lp_orig = env.get(lp_key + '_ORIG')
        if lp_orig is not None:
            env[lp_key] = lp_orig
        else:
            env.pop(lp_key, None)
        return env

    @staticmethod
    def stop_service():
        """Stop all DLNA threads
        stop MPV
        stop DLNA HTTP Server
        stop SSDP
        stop SSDP notify thread
        """
        if cherrypy.engine.state in [cherrypy.engine.states.STOPPED,
                                     cherrypy.engine.states.STOPPING,
                                     cherrypy.engine.states.EXITING,
                                     ]:
            return
        while cherrypy.engine.state != cherrypy.engine.states.STARTED:
            time.sleep(0.5)
        cherrypy.engine.exit()

    @staticmethod
    def is_service_running():
        return cherrypy.engine.state in [cherrypy.engine.states.STARTING,
                                         cherrypy.engine.states.STARTED,
                                         ]

    @staticmethod
    def restart():
        if sys.platform == 'darwin' and sys.executable.endswith("Contents/MacOS/python"):
            # run from py2app build
            Setting.stop_service()
            executable = sys.executable[:-6] + 'Macast'
            os.execv(executable, [executable, executable])
        elif sys.platform == 'linux' and getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
            # run from pyinstaller build on linux
            Setting.stop_service()
            env = Setting.get_system_env()
            executable = sys.executable
            os.execve(executable, [executable, executable], env)
        else:
            cherrypy.engine.restart()


class XMLPath(Enum):
    BASE_PATH = os.path.dirname(__file__)
    DESCRIPTION = BASE_PATH + '/xml/Description.xml'
    AV_TRANSPORT = BASE_PATH + '/xml/AVTransport.xml'
    CONNECTION_MANAGER = BASE_PATH + '/xml/ConnectionManager.xml'
    RENDERING_CONTROL = BASE_PATH + '/xml/RenderingControl.xml'
    SETTING_PAGE = BASE_PATH + '/xml/setting.html'
    PROTOCOL_INFO = BASE_PATH + '/xml/SinkProtocolInfo.csv'
    MANIFEST = BASE_PATH + '/xml/manifest.webmanifest'
    SW_JS = BASE_PATH + '/xml/sw.js'


def load_xml(path):
    with open(path, encoding="utf-8") as f:
        xml = f.read()
    return xml


def notify_error(msg=None):
    """publish a notification when error occured
    """

    def wrapper_fun(fun):
        def wrapper(*args, **kwargs):
            nonlocal msg
            try:
                return fun(*args, **kwargs)
            except Exception as e:
                logger.error(str(e))
                if msg is None:
                    msg = str(e)
                else:
                    logger.error(msg)
                cherrypy.engine.publish('app_notify', 'Error', msg)

        return wrapper

    return wrapper_fun


def publish_method(func):
    def wrap(*args, **kwargs):
        func(*args, **kwargs)
        cherrypy.engine.publish(func.__name__, *args, **kwargs)

    return wrap


def format_class_name(instance):
    """
    eg1: DLNAHandler -> DLNA Handler
    eg2: AabcBabc -> Aabc Babc
    :param instance:
    :return:
    """
    name = instance.__class__.__name__
    res = name[0]
    for i in range(1, len(name) - 1):
        if 'A' <= name[i] <= 'Z' and 'a' <= name[i + 1] <= 'z':
            res += f' {name[i]}'
        else:
            res += name[i]
    res += name[-1]
    return res


def cherrypy_publish(method, default=None):
    res = cherrypy.engine.publish(method)
    if len(res) > 0:
        return res.pop()
    return default
