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
    import pystray
    import webbrowser
    from PIL import Image

logger = logging.getLogger("gui")
logger.setLevel(logging.INFO)


class Platform(Enum):
    Darwin = 0
    Win32 = 1
    Others = 2


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


class DesktopWindow:
    """Native configuration window used instead of a tray/menu-bar host."""

    def __init__(self, name, actions):
        self.name = name
        self.actions = actions
        self.root = None
        self.status = None

    def run(self):
        import tkinter as tk
        from tkinter import ttk

        self.root = tk.Tk()
        self.root.title(self.name)
        self.root.minsize(760, 520)
        self.root.geometry('900x650')
        self.root.protocol('WM_DELETE_WINDOW', self.close)

        header = tk.Frame(self.root, padx=22, pady=16)
        header.pack(fill='x')
        tk.Label(header, text=self.name, font=('Helvetica', 22, 'bold')).pack(
            side='left')
        self.status = tk.Label(header, text='正在启动服务…', anchor='e',
                               justify='right')
        self.status.pack(side='right', fill='x', expand=True, padx=(20, 0))

        self.tabs = ttk.Notebook(self.root)
        self.tabs.pack(fill='both', expand=True, padx=16, pady=(0, 12))
        self._build_overview_tab(tk)
        self._build_renderer_tab(tk)
        self._build_protocol_tab(tk)
        self._build_plugin_tab(tk)
        self._build_general_tab(tk)
        self._build_mirror_tab(tk)

        footer = tk.Frame(self.root, padx=16, pady=10)
        footer.pack(fill='x')
        self._button(footer, '退出 Macast', self.close).pack(side='right')
        self.refresh()
        self.root.mainloop()

    @staticmethod
    def _button(parent, text, callback):
        import tkinter as tk
        return tk.Button(parent, text=text, command=callback, height=2)

    def _tab(self, title):
        import tkinter as tk
        frame = tk.Frame(self.tabs, padx=18, pady=18)
        self.tabs.add(frame, text=title)
        return frame

    def _build_overview_tab(self, tk):
        frame = self._tab('概览')
        self.overview = tk.Label(frame, anchor='nw', justify='left')
        self.overview.pack(fill='x', pady=(0, 18))
        self._button(frame, '打开网页高级设置',
                     self.actions.get('settings')).pack(fill='x', pady=4)
        self._button(frame, '电脑投屏', self.actions.get('mirror')).pack(
            fill='x', pady=4)
        self._button(frame, '启动 / 停止接收服务',
                     self.actions.get('toggle')).pack(fill='x', pady=4)

    def _build_renderer_tab(self, tk):
        frame = self._tab('播放器')
        self.renderer_var = tk.StringVar()
        self.renderer_buttons = tk.Frame(frame)
        self.renderer_buttons.pack(fill='both', expand=True, anchor='nw')

    def _build_protocol_tab(self, tk):
        frame = self._tab('协议')
        self.protocol_buttons = tk.Frame(frame)
        self.protocol_buttons.pack(fill='both', expand=True, anchor='nw')

    def _build_plugin_tab(self, tk):
        frame = self._tab('插件')
        self.plugin_buttons = tk.Frame(frame)
        self.plugin_buttons.pack(fill='both', expand=True, anchor='nw')

    def _build_general_tab(self, tk):
        frame = self._tab('通用')
        self.general = tk.Label(frame, anchor='nw', justify='left')
        self.general.pack(fill='x', pady=(0, 18))
        self.auto_update_var = tk.BooleanVar(value=True)
        self.start_login_var = tk.BooleanVar(value=False)
        tk.Checkbutton(frame, text='自动检查更新', variable=self.auto_update_var,
                       command=lambda: self._act('auto_update',
                                                 self.auto_update_var.get())).pack(
                                                     anchor='w', pady=3)
        tk.Checkbutton(frame, text='登录系统时启动', variable=self.start_login_var,
                       command=lambda: self._act('start_login',
                                                 self.start_login_var.get())).pack(
                                                     anchor='w', pady=3)
        self._button(frame, '检查更新',
                     lambda: self.actions.get('check_update', lambda: None)()).pack(
                         fill='x', pady=4)
        self._button(frame, '打开配置目录',
                     self.actions.get('open_config')).pack(fill='x', pady=4)

    def _build_mirror_tab(self, tk):
        frame = self._tab('电脑投屏')
        self.mirror = tk.Label(frame, anchor='nw', justify='left')
        self.mirror.pack(fill='x', pady=(0, 18))
        self._button(frame, '打开投屏控制台',
                     self.actions.get('mirror')).pack(fill='x', pady=4)

    def _clear(self, frame):
        for child in frame.winfo_children():
            child.destroy()

    def refresh(self):
        """Re-read controller state and redraw controls after every action."""
        if self.root is None:
            return
        try:
            state = self.actions['state']()
        except Exception as exc:
            state = {'error': str(exc)}
        self._render_state(state)

    def _render_state(self, state):
        if state.get('error'):
            text = '读取配置失败：{}'.format(state['error'])
            self.overview.configure(text=text)
            return
        service = state.get('service', {})
        self.overview.configure(text=(
            '设备名称：{name}\n服务地址：{address}\n服务状态：{status}\n版本：{version}'
        ).format(**service))
        self.general.configure(text=(
            '自动检查更新：{}\n启动时运行：{}\n配置目录：{}'
        ).format(state.get('auto_update'), state.get('start_at_login'),
                 state.get('config_dir')))
        self.auto_update_var.set(bool(state.get('auto_update_value')))
        self.start_login_var.set(bool(state.get('start_at_login_value')))
        self.mirror.configure(text=state.get('mirror', '电脑投屏由专用控制台管理。'))

        self._clear(self.renderer_buttons)
        for title in state.get('renderers', []):
            self._button(self.renderer_buttons, title,
                         lambda value=title: self._act('renderer', value)
                         ).pack(fill='x', pady=3)
        self._clear(self.protocol_buttons)
        for item in state.get('protocols', []):
            text = '{} [{}]'.format(item['title'],
                                    '已启用' if item['enabled'] else '已停用')
            self._button(self.protocol_buttons, text,
                         lambda value=item['title']: self._act('protocol', value)
                         ).pack(fill='x', pady=3)
        self._clear(self.plugin_buttons)
        for item in state.get('plugins', []):
            text = '{} [{}]'.format(item['title'],
                                    '已启用' if item['enabled'] else '已停用')
            self._button(self.plugin_buttons, text,
                         lambda value=item['key']: self._act('plugin', value)
                         ).pack(fill='x', pady=3)

    def _act(self, kind, value):
        try:
            self.actions['change'](kind, value)
        finally:
            self.refresh()

    def set_status(self, text):
        if self.status is not None:
            try:
                self.status.after(0, lambda: self.status.configure(text=text))
            except Exception:
                pass

    def close(self):
        callback = self.actions.get('quit')
        if callback is not None:
            callback()
        if self.root is not None:
            self.root.destroy()


class App:
    def __init__(self, name, icon, menu, template=True, mode='tray',
                 window_actions=None):
        self.name = name
        self.icon = icon
        self.app = None
        self.menu = menu
        self.menuDict = {}
        self.template = template
        self.mode = mode
        self.window = None
        if mode == 'headless':
            self.platform = (Platform.Darwin if sys.platform == 'darwin'
                             else Platform.Win32 if sys.platform == 'win32'
                             else Platform.Others)
            return
        if mode == 'window':
            self.platform = (Platform.Darwin if sys.platform == 'darwin'
                             else Platform.Win32 if sys.platform == 'win32'
                             else Platform.Others)
            self.window = DesktopWindow(name, window_actions or {})
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
        if self.mode in ('window', 'headless') or self.app is None:
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
        if self.mode == 'window' or self.app is None:
            return
        if self.platform == Platform.Darwin:
            self.app.menu.clear()
            self.app.menu = self._build_menu_rumps(menu)
        else:
            self.app.menu = pystray.Menu(lambda: self._build_menu_pystray(menu))

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
        if self.mode == 'window':
            if self.window is not None:
                self.window.run()
        else:
            self.app.run()

    def quit(self, _):
        if self.mode == 'headless':
            return
        if self.mode == 'window':
            if self.window is not None and self.window.root is not None:
                self.window.root.destroy()
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
        if self.mode == 'window':
            if self.window is not None:
                self.window.set_status('{}: {}'.format(title, content))
            return
        if self.platform == Platform.Darwin:
            rumps.notification(title, "", content, sound=sound)
        else:
            try:
                self.app.notify(message=content, title=title)
            except NotImplementedError:
                pass

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
