# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Standalone Tk configuration window for Macast."""

import json
import os
import subprocess
import sys
import tkinter as tk
from tkinter import ttk
import urllib.parse
import urllib.request

from mirror_console import Api as MirrorApi, MirrorConsole

API = os.environ.get('MACAST_CONFIG_API', 'http://127.0.0.1:58880').rstrip('/')
TOKEN = os.environ.get('MACAST_CONFIG_TOKEN', '')


def request(path, data=None):
    url = API + path
    headers = {'X-Macast-Token': TOKEN}
    payload = None
    if data is not None:
        payload = urllib.parse.urlencode(data).encode()
        headers['Content-Type'] = 'application/x-www-form-urlencoded'
    req = urllib.request.Request(url, data=payload, headers=headers)
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=5) as response:
        return json.loads(response.read().decode('utf-8', 'replace'))


def post(fields):
    return request('/api', fields)


def open_url(url):
    if sys.platform == 'darwin':
        subprocess.Popen(['open', url])
    elif sys.platform == 'win32':
        os.startfile(url)
    else:
        subprocess.Popen(['xdg-open', url])


class ConfigWindow:
    def __init__(self):
        self.root = tk.Tk()
        self.root.title('Macast 配置')
        self.root.geometry('900x650')
        self.root.minsize(760, 520)
        self.status = tk.StringVar(value='正在读取 Macast 状态…')
        self.detail = tk.StringVar()
        self.tabs = ttk.Notebook(self.root)
        self.tabs.pack(fill='both', expand=True, padx=16, pady=(16, 8))
        self._build_overview()
        self._build_playback_protocols()
        self._build_plugins()
        self._build_general()
        self._build_mirror()
        self.mirror_console = None
        self._mount_mirror_console()
        footer = ttk.Frame(self.root)
        footer.pack(fill='x', padx=16, pady=(0, 12))
        ttk.Label(footer, textvariable=self.status).pack(side='left')
        ttk.Button(footer, text='刷新', command=self.refresh).pack(side='right')
        self.root.protocol('WM_DELETE_WINDOW', self.close)
        self.refresh()

    def _tab(self, title):
        frame = ttk.Frame(self.tabs, padding=18)
        self.tabs.add(frame, text=title)
        return frame

    def _build_overview(self):
        frame = self._tab('概览')
        self.overview = tk.Text(frame, height=8, wrap='word', state='disabled')
        self.overview.pack(fill='x', pady=(0, 18))
        buttons = ttk.Frame(frame)
        buttons.pack(fill='x')
        ttk.Button(buttons, text='打开网页高级设置', command=lambda: open_url(API + '/')).pack(fill='x', pady=4)
        ttk.Button(buttons, text='退出 Macast', command=self.close).pack(fill='x', pady=4)
        ttk.Button(buttons, text='打开电脑投屏控制台', command=self.open_mirror).pack(fill='x', pady=4)

    def _build_playback_protocols(self):
        frame = self._tab('播放器与协议')
        ttk.Label(frame, text='播放器：负责播放手机投来的媒体。\n协议：负责让手机发现并连接 Macast。两者不是插件列表。',
                  justify='left').pack(anchor='w', pady=(0, 12))
        self.renderer_frame = ttk.LabelFrame(frame, text='当前播放器')
        self.renderer_frame.pack(fill='x', pady=6)
        self.protocol_frame = ttk.LabelFrame(frame, text='接收协议')
        self.protocol_frame.pack(fill='x', pady=6)

    def _build_plugins(self):
        frame = self._tab('插件')
        ttk.Label(frame, text='插件是附加能力，例如屏幕投屏、本地文件投屏、外部播放器和自动化钩子。\n这里不重复提供播放器或接收协议开关。',
                  justify='left').pack(anchor='w', pady=(0, 12))
        self.plugin_frame = ttk.Frame(frame)
        self.plugin_frame.pack(fill='both', expand=True)

    def _build_general(self):
        frame = self._tab('通用')
        ttk.Button(frame, text='打开配置目录', command=self.open_config).pack(fill='x', pady=4)
        ttk.Button(frame, text='检查更新', command=self.check_update).pack(fill='x', pady=4)
        self.general = ttk.Label(frame, justify='left')
        self.general.pack(anchor='w', pady=18)

    def _build_mirror(self):
        frame = self._tab('电脑投屏')
        self.mirror_tab = frame

    def _mount_mirror_console(self):
        """Embed the real mirror console into this window's tab."""
        self.mirror_console = MirrorConsole(
            tk, api=MirrorApi(base=API, token=TOKEN))
        self.mirror_console.run(root=self.root, parent=self.mirror_tab,
                                mainloop=False)

    def _set_text(self, widget, text):
        widget.configure(state='normal')
        widget.delete('1.0', 'end')
        widget.insert('1.0', text)
        widget.configure(state='disabled')

    def refresh(self):
        try:
            state = request('/api?query=status')
            plugins = request('/api?query=plugin-info').get('plugins', [])
            server = state.get('server', {})
            self._set_text(self.overview, '设备名称：{}\n地址：{}:{}\n状态：{}\n版本：{}\n播放器：{}\n协议：{}'.format(
                server.get('friendly_name', ''), server.get('ip', ''), server.get('port', ''),
                '运行中' if server.get('running') else '已停止', server.get('version', ''),
                server.get('renderer', ''), ', '.join(server.get('protocol', []))))
            self._render_playback_protocols(server, plugins)
            self._render_plugins(plugins)
            self.general.configure(text='配置目录：{}\n管理接口：{}'.format(os.environ.get('MACAST_CONFIG_DIR', ''), API))
            self.status.set('Macast 正在运行' if server.get('running') else 'Macast 已停止')
        except Exception as exc:
            self.status.set('读取失败：{}'.format(exc))

    def _render_plugins(self, plugins):
        for child in self.plugin_frame.winfo_children():
            child.destroy()
        # Player and receiver-protocol choices have dedicated controls. The
        # plugin page is only the add-on catalog, so it must not duplicate those
        # actions and confuse the user about ownership.
        for item in plugins:
            if item.get('role') != 'addon' or not item.get('available', False):
                continue
            title = item.get('title', '')
            state = '已启用' if item.get('enabled') else '已停用'
            key = item.get('key', '')
            command = lambda p=item, k=key: self.toggle_plugin(p, k)
            ttk.Button(self.plugin_frame, text='{}  [{}]'.format(title, state), command=command).pack(fill='x', pady=3)

    def _render_playback_protocols(self, server, plugins):
        for frame in (self.renderer_frame, self.protocol_frame):
            for child in frame.winfo_children():
                child.destroy()
        current_renderer = server.get('renderer', '')
        ttk.Label(self.renderer_frame, text='当前：{}'.format(current_renderer)).pack(anchor='w', padx=8, pady=6)
        renderers = [item for item in plugins
                     if item.get('role') == 'player' and item.get('available')]
        for item in renderers:
            title = item.get('title', '')
            ttk.Button(self.renderer_frame, text=title,
                       command=lambda value=title: self.set_renderer(value)).pack(
                           fill='x', padx=8, pady=2)
        enabled = set(server.get('protocol', []))
        protocols = [item for item in plugins
                     if item.get('role') == 'protocol' and item.get('available')]
        for item in protocols:
            title = item.get('title', '')
            label = '{}  [{}]'.format(title, '已启用' if title in enabled else '已停用')
            ttk.Button(self.protocol_frame, text=label,
                       command=lambda value=title: self.toggle_protocol(value)).pack(
                           fill='x', padx=8, pady=2)

    def set_renderer(self, title):
        self._post_and_refresh({'set-renderer': title}, '播放器已切换')

    def toggle_protocol(self, title):
        self._post_and_refresh({'toggle-protocol': title}, '协议状态已更新')

    def toggle_plugin(self, plugin, key):
        field = 'plugin-enable' if not plugin.get('enabled') else 'plugin-disable'
        self._post_and_refresh({field: '1', 'plugin-key': key}, '插件状态已更新')

    def _post_and_refresh(self, fields, fallback):
        try:
            result = post(fields)
            if result.get('code', 0) != 0:
                self.status.set(result.get('message', '操作失败'))
            else:
                self.status.set(result.get('message', fallback))
            self.refresh()
        except Exception as exc:
            self.status.set('操作失败：{}'.format(exc))


    def open_mirror(self):
        self.tabs.select(self.mirror_tab)
        self.status.set('已切换到电脑投屏页签')

    def open_config(self):
        path = os.environ.get('MACAST_CONFIG_DIR')
        if path:
            open_url(path)

    def check_update(self):
        try:
            result = post({'app-action': 'check-update'})
            self.status.set(result.get('message', '正在检查更新'))
        except Exception as exc:
            self.status.set('检查更新失败：{}'.format(exc))

    def run(self):
        self.root.mainloop()

    def close(self):
        if self.mirror_console is not None:
            self.mirror_console.quit()
        try:
            post({'app-action': 'quit'})
        except Exception:
            pass
        self.root.destroy()


if __name__ == '__main__':
    ConfigWindow().run()
