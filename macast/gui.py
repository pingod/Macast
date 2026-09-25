# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.

import sys
import logging
import subprocess
from enum import Enum
from .utils import Setting

if sys.platform == 'darwin':
    import rumps
else:
    import webbrowser
    from PIL import Image


_pystray_module = None


def _pystray():
    """The tray backend, imported the first time something actually needs a tray.

    Importing pystray on Linux is not a cheap name binding: its package `__init__`
    picks a backend and `pystray/_xorg.py` opens an X display **at module scope**,
    so on a machine with no display server it raises `Xlib.error.DisplayNameError`
    right there. That poisoned every import of this module -- including
    `macast.macast`, whose `cli()` never builds a tray and whose `Service` the
    settings page drives just fine headless. The documented Linux entry point
    (`macast-cli`, README_ZH) therefore could not start on a server at all.
    """
    global _pystray_module
    if _pystray_module is None:
        import pystray
        _pystray_module = pystray
    return _pystray_module


logger = logging.getLogger("gui")
logger.setLevel(logging.INFO)


class Platform(Enum):
    Darwin = 0
    Win32 = 1
    Others = 2


# pystray writes a balloon into fixed-size character buffers (NOTIFYICONDATAW's
# szInfo / szInfoTitle); ctypes raises ValueError for anything longer, and
# cherrypy re-raises it as ChannelFailures in whichever thread was reporting a
# failure. See `fit_notification` and §4.8 of AGENTS.md.
NOTIFY_TEXT_LIMIT = 256
NOTIFY_TITLE_LIMIT = 64
NOTIFY_TRUNCATED = u'……（全文见设置页「电脑投屏」的活动）'


def fit_notification(text, limit):
    """Keep the head *and the tail* of `text` inside `limit` characters: the
    last clause is where the actionable half lives (「在兼容档位里换成…再试一次」).
    """
    if len(text) <= limit:
        return text
    room = max(0, limit - len(NOTIFY_TRUNCATED))
    head = room // 2
    return text[:head] + NOTIFY_TRUNCATED + text[len(text) - (room - head):]


class MenuItem:
    def __init__(self, text, callback=None, checked=None, enabled=True,
                 children=None, data=None, key=None):
        self.view = None
        if sys.platform == 'darwin':
            self.platform = Platform.Darwin
        elif sys.platform == 'win32':
            self.platform = Platform.Win32
        else:
            self.platform = Platform.Others

        self._text = text
        self.callback = callback
        self.children = children
        self._checked = checked
        self._enabled = enabled
        self.data = data
        self.id = text
        self.key = key

    @property
    def text(self):
        return self._text

    @property
    def checked(self):
        return self._checked

    @property
    def enabled(self):
        return self._enabled

    @text.setter
    def text(self, value):
        self._text = value
        if self.view is None:
            return
        if self.platform == Platform.Darwin:
            self.view.title = self._text

    @checked.setter
    def checked(self, value):
        self._checked = value
        if self.view is None:
            return
        if self.platform == Platform.Darwin:
            self.view.state = 1 if self._checked else 0

    @enabled.setter
    def enabled(self, value):
        self._enabled = value
        if self.view is None:
            return
        if self.platform == Platform.Darwin:
            self.view.set_callback(
                self._rumpsCallback if self._enabled else None,
                self.key)

    def items(self):
        return [] if self.children is None else self.children

    def _pystrayCallback(self, app, item):
        self.callback(self)

    def _rumpsCallback(self, item):
        self.callback(self)




class App:
    #: 'tray' is the normal menu-bar / notification-area shell. 'headless' is for
    #: everything that must construct the app without a tray backend (a test, a
    #: machine with no display server): it keeps the same API and does nothing.
    def __init__(self, name, icon, menu, template=True, mode='tray'):
        self.name = name
        self.icon = icon
        self.app = None
        self.menu = menu
        self.menuDict = {}
        self.template = template
        self.mode = mode
        self.platform = (Platform.Darwin if sys.platform == 'darwin'
                         else Platform.Win32 if sys.platform == 'win32'
                         else Platform.Others)
        if mode == 'headless':
            return
        if sys.platform == 'darwin':
            self.platform = Platform.Darwin
            self.init_platform_darwin()
        elif sys.platform == 'win32':
            self.platform = Platform.Win32
            self.init_platform_win32()
        else:
            self.platform = Platform.Others
            self.init_platform_others()

        if self.platform == Platform.Darwin:
            self.app = rumps.App(self.name,
                                 icon=self.icon,
                                 menu=self._build_menu_rumps(self.menu),
                                 template=self.template,
                                 quit_button=None)
            rumps.debug_mode(True)
        else:
            pystray = _pystray()
            self.app = pystray.Icon(self.name,
                                    Image.open(self.icon),
                                    menu=pystray.Menu(
                                        lambda:
                                        self._build_menu_pystray(self.menu)))

    def init_platform_darwin(self):
        pass

    def init_platform_win32(self):
        pass

    def init_platform_others(self):
        pass

    def _build_menu_rumps(self, menu):
        items = []
        for item in menu:
            if item is None:
                items.append(None)
            elif item.children is not None:
                menu_item = rumps.MenuItem(item.text)
                items.append([menu_item, self._build_menu_rumps(item.children)])
            else:
                items.append(self._build_menu_item_rumps(item))
        return items

    def _build_menu_item_rumps(self, item):
        callback = item._rumpsCallback if item.enabled else None
        menu_item = rumps.MenuItem(item.text, callback, item.key)
        menu_item.state = 1 if item.checked else 0
        item.view = menu_item
        return menu_item

    def _build_menu_pystray(self, menu):
        pystray = _pystray()
        items = []
        for item in menu:
            if item is None:
                items.append(pystray.Menu.SEPARATOR)
            elif item.children is not None and len(item.children) > 0:
                menu_item = pystray.MenuItem(
                    item.text, pystray.Menu(
                        *self._build_menu_pystray(item.children)))
                items.append(menu_item)
            else:
                menu_item = pystray.MenuItem(lambda i: i.view.text,
                                             item._pystrayCallback,
                                             lambda i: True if i.view.checked
                                             else None,
                                             enabled=lambda i: i.view.enabled)
                item.view = menu_item
                menu_item.view = item
                items.append(menu_item)
        return items

    def update_icon(self, icon, template=True):
        self.icon = icon
        if self.platform == Platform.Darwin:
            self.app.template = template
            self.app.icon = self.icon
        else:
            self.app.icon = Image.open(self.icon)

    def update_menu(self):
        """Refresh the legacy tray menu when a tray backend is active."""
        if self.mode == 'headless' or self.app is None:
            return
        if self.platform != Platform.Darwin:
            self.app.update_menu()

    def call_on_main_thread(self, fn):
        """Run `fn` on the UI thread.

        Menu mutation belongs on the main thread. Protocol and plugin changes
        now also arrive from CherryPy worker threads -- the settings page
        drives them through the management API -- and rebuilding the menu from
        there makes AppKit unhappy. Falls back to an inline call wherever no
        main-thread pump is available, so this never becomes a silent no-op.
        """
        if self.mode == 'headless':
            try:
                fn()
            except Exception as e:
                logger.error("Headless callback failed: %s", e)
            return
        if self.platform == Platform.Darwin:
            try:
                from PyObjCTools import AppHelper
                AppHelper.callAfter(fn)
                return
            except Exception as e:  # pragma: no cover - pyobjc always present on darwin
                logger.warning("callAfter unavailable (%s); updating menu inline", e)
        try:
            fn()
        except Exception as e:
            logger.error("Menu refresh failed: %s", e)

    def set_menu(self, menu):
        self.menu = menu
        if self.app is None:
            return
        if self.platform == Platform.Darwin:
            self.app.menu.clear()
            self.app.menu = self._build_menu_rumps(menu)
        else:
            self.app.menu = _pystray().Menu(
                lambda: self._build_menu_pystray(menu))

    def _find_menu_item_index_by_id(self, id):
        #  TODO find all items
        for i, item in enumerate(self.menu):
            if item.id is not None and item.id == id:
                return i
        logger.error("Canot find id:{}.".format(id))
        return -1

    def append_menu_item_after(self, id, menu_item):
        if self.platform == Platform.Darwin:
            self.app.menu.insert_after(id, self._build_menu_item_rumps(menu_item))
        else:
            index = self._find_menu_item_index_by_id(id)
            print("index: ", index)
            if index != -1:
                self.menu.insert(index + 1, menu_item)
                self.app.update_menu()

    def append_menu_item_before(self, id, menu_item):
        if self.platform == Platform.Darwin:
            self.app.menu.insert_before(id, self._build_menu_item_rumps(menu_item))
        else:
            index = self._find_menu_item_index_by_id(id)
            if index != -1:
                self.menu.insert(index, menu_item)
                self.app.update_menu()

    def remove_menu_item_by_id(self, id):
        if self.platform == Platform.Darwin:
            if id in self.app.menu:
                self.app.menu.pop(id)
        else:
            index = self._find_menu_item_index_by_id(id)
            if index != -1:
                self.menu.pop(index)
                self.app.update_menu()

    def start(self):
        if self.mode == 'headless':
            return
        self.app.run()

    def quit(self, _):
        if self.mode == 'headless':
            return
        if self.platform == Platform.Darwin:
            rumps.quit_application()
        else:
            try:
                self.app.remove_notification()
            except NotImplementedError:
                pass
            self.app.stop()

    def alert(self, content):
        if self.platform == Platform.Darwin:
            rumps.alert(content)
        else:
            self.notification(content, "Macast")

    def notification(self, title, content, sound=True):
        if self.mode == 'headless':
            logger.info('%s: %s', title, content)
            return
        if self.platform == Platform.Darwin:
            # rumps marshals nothing of its own -- this reaches
            # NSUserNotificationCenter and NSApp directly on the caller's
            # thread -- and `app_notify` is published from CherryPy workers,
            # the player's IPC thread and the mirror threads. Talking to AppKit
            # from there is what dismisses a menu the user is still holding.
            self.call_on_main_thread(
                lambda: rumps.notification(title, "", content, sound=sound))
        else:
            try:
                self.app.notify(
                    message=fit_notification(content, NOTIFY_TEXT_LIMIT),
                    title=fit_notification(title, NOTIFY_TITLE_LIMIT))
            except Exception as e:
                # Whatever was reporting has bigger problems than this balloon,
                # and `notice` already holds the full sentence.
                logger.warning('notification failed: %s', e)

    def dialog(self, content, callback=None, cancel="Cancel", ok="Ok"):
        if self.platform == Platform.Darwin:
            try:
                res = Setting.system_shell(
                    ['osascript',
                     '-e',
                     'display dialog "{}" buttons {{"{}","{}"}}'.format(
                         content, cancel, ok)
                     ])
                if ok in res[1] and callback:
                    callback()
            except Exception as e:
                self.notification("Error", "Cannot access System Events")
                logger.error(e)
                callback()
        else:
            self.notification("Macast", content)
            if callback:
                callback()

    def get_env(self):
        # https://github.com/pyinstaller/pyinstaller/issues/3668#issuecomment-742547785
        env = Setting.get_system_env()
        toDelete = []
        for (k, v) in env.items():
            if k != 'PATH' and 'tmp' in v:
                toDelete.append(k)
        for k in toDelete:
            env.pop(k, None)
        return env

    def open_browser(self, url):
        if self.platform == Platform.Darwin:
            subprocess.Popen(['open', url])
        elif self.platform == Platform.Win32:
            webbrowser.open(url)
        else:
            subprocess.Popen(["xdg-open", url], env=self.get_env())

    def open_directory(self, path):
        if self.platform == Platform.Darwin:
            subprocess.Popen(['open', path])
        elif self.platform == Platform.Win32:
            subprocess.Popen(['explorer.exe', path])
        else:
            subprocess.Popen(["xdg-open", path], env=self.get_env())

    @staticmethod
    def build_menu_item_group(titles, callback):
        items = []
        for index, title in enumerate(titles):
            item = MenuItem(title, callback, data=index)
            items.append(item)
        return items


if __name__ == '__main__':
    class DemoApp(App):
        def __init__(self):
            super(DemoApp, self).__init__("Macast",
                                          "assets/menu_light.png",
                                          [MenuItem("Add",
                                                    self.add,
                                                    data=1,
                                                    key="a"),
                                           MenuItem("Remove",
                                                    self.remove,
                                                    data=1,
                                                    key="b"),
                                           MenuItem("Quit", self.quit)])

        def test_call(self, item):
            print("testCall: ", item)
            item.text = "123"

        def remove(self, item):
            menu = [MenuItem("Add",
                             self.add,
                             data=1,
                             key="a"),
                    MenuItem("Remove",
                             self.remove,
                             data=1,
                             key="r"),
                    MenuItem("Quit", self.quit)]
            self.set_menu(menu)

        def add(self, item):
            print("add", item.data)
            item = MenuItem("Test", None, children=[
                MenuItem("Test1", self.test_call, data=1),
                None,
                MenuItem("Test2", self.test_call, data=2),
                MenuItem("Test3", self.test_call, data=3),
            ])
            self.append_menu_item_after("Add", item)

        def quit(self, _):
            super(DemoApp, self).quit(_)

    DemoApp().start()
