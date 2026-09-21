# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Which settings key belongs to which module — and what to call it.

The 高级设置 tab has always exposed the raw JSON, which answers "what is
stored" but never "who wrote this". Every plugin persists under its own
``class SettingProperty(Enum)`` member names, so the owner of a key is
knowable; this module turns that into per-module groups for the 模块设置 tab.

Labels live here rather than in plugin manifests on purpose: bumping a plugin
file means the two-step SHA-pinned index dance (AGENTS §4.6), and a missing
label is only cosmetic anyway — unknown keys still surface verbatim under
「其他（未归类）」. The regression suite (Part 28) requires every key used by
an in-repo module to have a label here, which is what keeps the map honest.

Values that are scalars (str/int/float/bool/None) are editable from the page;
lists and dicts stay read-only there — they are structure, and the JSON tab
remains the place to change them.
"""
import logging
import os

logger = logging.getLogger("ModuleSettings")

#: Group ids that are not plugins.
CORE_GROUP = 'core'
PLAYER_GROUP = 'player'
OTHER_GROUP = 'other'

#: name -> (label, hint). Persisted keys owned by macast itself (utils).
CORE_LABELS = {
    'USN': ('SSDP USN', '设备标识序号，换网卡/重置发现时会变，一般不用手动改。'),
    'CheckUpdate': ('自动检查更新', 'bool：启动时是否向 GitHub 检查新版本。'),
    'StartAtLogin': ('开机自启', 'bool：登录后自动运行 Macast。'),
    'MenubarIcon': ('菜单栏图标', '控制菜单栏图标的显示/样式。'),
    'ApplicationPort': ('应用端口', '设置页与网页投屏入口的 HTTP 端口（默认 58880）。'),
    'DLNA_FriendlyName': ('DLNA 设备名', '投屏发送端看到的本机设备名称。'),
    'Macast_Renderer': ('当前渲染器', '正在使用的渲染器插件标题；渲染器一次只能选一种。'),
    'Macast_Protocol': ('（旧）单一协议', 'Macast_Protocols 出现前的旧键，仅作迁移来源保留。'),
    'Blocked_Interfaces': ('禁用的网卡', '列表：不参与发现与广播的网卡名。'),
    'Additional_Interfaces': ('附加的网卡', '列表：默认路由之外额外允许广播的网卡名。'),
    'Play_History': ('投屏历史', '列表：设置页「历史」面板的数据，可整表清空。'),
    'Https_Enabled': ('启用 HTTPS 通道', 'bool：为管理 API 启用 HTTPS。'),
    'Https_Port': ('HTTPS 端口', ''),
    'Https_Cert': ('HTTPS 证书路径', ''),
    'Https_Key': ('HTTPS 私钥路径', ''),
    'Macast_Protocols': ('启用的协议', '列表：同时开启的协议标题（DLNA / Chromecast / AirPlay…）。'),
    'Network_Interface': ('固定网卡', '网卡名；空串表示自动选择承载 IPv4 默认路由的网卡。'),
    'Disabled_Plugins': ('已停用的插件', '列表：被关掉（不加载）的插件标题。'),
    'Api_Token': ('管理令牌', '网页投屏入口与远程管理 API 的凭据，状态页可见可复制。'),
    'Github_CN_Mirror': ('启用国内镜像地址', 'bool：所有 GitHub 地址改走国内可达的镜像。'),
}

#: Persisted keys of the built-in mpv renderer (macast_renderer/mpv.py).
PLAYER_LABELS = {
    'PlayerHW': ('硬件解码', '0 关 / 1 开 / 2 强制，透传给 mpv 的 hwdec。'),
    'PlayerSize': ('播放器尺寸', '0 小 / 1 正常 / 2 大 / 3 自适应 / 4 全屏。'),
    'PlayerPosition': ('播放器位置', '0..4：左上 / 左下 / 右上 / 右下 / 居中。'),
    'PlayerOntop': ('播放器置顶', '0 否 / 1 是。'),
    'PlayerDefaultVolume': ('默认音量', '每次开始播放时设置的音量（0-100）。'),
}

#: Per-plugin-module persisted keys, keyed by plugin module file name
#: (``screen_mirror.py`` -> ``screen_mirror``). Bundled plugins included.
PLUGIN_LABELS = {
    'screen_mirror': {
        'Mirror_Target': ('镜像目标（Cast）', '"host:port"，局域网里的 Chromecast。'),
        'Mirror_Target_Name': ('镜像目标名称', '仅用于提示文案。'),
        'Mirror_Quality': ('画质档位', '菜单「画质」写入的档位索引。'),
        'Mirror_Audio_Aggregate': ('聚合设备 ID', '一键设置创建的 CoreAudio 多输出设备，复用而非叠建。'),
        'Mirror_Audio_Original': ('原声音输出 ID', '「恢复原声音输出」要回到的默认设备。'),
        'Mirror_Output': ('输出目标', "'cast' | 'caststream' | 'browser' | 'dlna'。"),
        'Mirror_Screen': ('采集屏幕', 'avfoundation 视频设备序号；空串 = 第一块屏幕。'),
        'Mirror_Cursor': ('显示鼠标指针', 'bool。'),
        'Mirror_Encoder': ('编码器', "'software'（x264）| 'hardware'（VideoToolbox，仅 macOS）。"),
        'Mirror_Dlna_Profile': ('DLNA 档位', 'DLNA_PROFILES 的键；老电视吃得下什么封装由它决定。'),
        'Mirror_Dlna_Control': ('DLNA 控制 URL', '所选 DLNA 渲染器的 AVTransport 控制地址。'),
    },
    'cast_local_file': {
        'Target_Kind': ('目标类型', "'cast'（Chromecast）或 'dlna'（电视）。"),
        'Cast_Target': ('Cast 设备', '"host:port"。'),
        'Cast_Target_Name': ('Cast 设备名称', '仅用于提示文案。'),
        'Dlna_Control': ('DLNA 控制 URL', 'AVTransport 控制地址。'),
        'Dlna_Target_Name': ('DLNA 设备名称', '仅用于提示文案。'),
        'Folder': ('投屏文件夹', '菜单「选择文件夹」记录的媒体目录。'),
        'Mode': ('播放模式', "'auto' 按 ffprobe 判定 | 'direct' 强制直供 | 'convert' 强制转码。"),
        'Audio_Stream': ('音轨索引', 'ffprobe 流序号；空串 = 保持原样。'),
        'Subtitle_Stream': ('字幕流索引', 'ffprobe 流序号；空串 = 不选字幕。'),
        'Audio_Delay': ('音画同步偏移', '毫秒，正值 = 声音更晚。'),
        'Bitrate': ('转码码率', '档位 kbps。'),
        'Hardware': ('转码用硬件编码', 'bool：macOS 上有 VideoToolbox 就用。'),
        'System_Audio': ('只投系统声音', 'bool：音频档，不投画面。'),
        'Auto_Next': ('自动连播', 'bool：播放列表放完一首自动投下一首。'),
        'Timeout': ('网络超时', '秒，socket 超时。'),
        'Temp_Dir': ('转码临时目录', '转码路径存放「会一直变大的临时文件」的目录。'),
    },
    'cast_bridge': {
        'Bridge_Target': ('中继目标', '"host:port" 的真 Chromecast。'),
        'Bridge_Target_Name': ('中继目标名称', '仅用于提示文案。'),
        'Bridge_Timeout': ('超时（秒）', 'socket 超时，最小 1。'),
    },
    'macast_ytdlp': {
        'YTDLP_Dir': ('下载目录', '下载模式落盘的位置。'),
        'YTDLP_Mode': ('工作模式', '0 = 下载到磁盘，1 = 边下边播（喂给 mpv）。'),
    },
    'external_player': {
        'External_Player': ('播放器选择', 'PLAYERS 表里的键（vlc/mpv/iina/…）。'),
        'External_Player_Path': ('播放器路径', '手写的可执行文件绝对路径，优先于自动探测。'),
    },
    'floating': {
        'Floating_Mode': ('小窗模式', '0 = 悬浮小窗，1 = 桌面壁纸。'),
        'Floating_Size': ('小窗尺寸', 'SIZES 表的索引。'),
    },
    'hooks': {
        'Hook_On_Cast': ('投屏时执行', 'shell 命令；媒体 URL 走环境变量传入。'),
        'Hook_On_Pause': ('暂停时执行', '同上。'),
        'Hook_On_Resume': ('恢复时执行', '同上。'),
        'Hook_On_Stop': ('停止时执行', '同上。'),
    },
    'web': {
        'Web_AutoCopy': ('自动复制 URL', '0 关 / 1 开：投屏时把 URL 复制到剪贴板。'),
    },
    'potplayer': {
        'Potplayer_Path': ('PotPlayer 路径', 'potplayermini64/potplayermini 的绝对路径。'),
    },
    'airplay_mirror': {
        'Mirror_Uxplay_Options': ('uxplay 选项', '写入 -rc 选项文件的 uxplay 参数（字符串）。'),
    },
    'raop': {
        'RAOP_Device_Name': ('RAOP 扬声器名称',
                             'AirPlay 音频列表里显示的名字；空/未设置 = 跟随 DLNA 设备名。'),
    },
}

_SCALARS = (str, int, float, bool)


def plugin_module_name(module):
    """`screen_mirror` for the plugin file screen_mirror.py.

    The file name is the identity the label map is keyed by; the import-time
    ``__name__`` is only a fallback (a frozen bundle may not carry a real
    ``__file__``, and the suite imports plugins under alias names).
    """
    path = getattr(module, '__file__', None)
    if path:
        return os.path.splitext(os.path.basename(path))[0]
    return str(getattr(module, '__name__', '')).split('.')[-1]


def _labels_for_module(module):
    enum = getattr(module, 'SettingProperty', None)
    if enum is None:
        # An older installed copy of a plugin may predate its settings (RAOP
        # v0.1 has no enum at all). Showing a key it never reads would be a
        # lie, so: no enum, no settings group.
        return {}
    name = plugin_module_name(module)
    try:
        members = {m.name for m in enum}
    except Exception:
        return {}
    if name in PLUGIN_LABELS:
        return {k: v for k, v in PLUGIN_LABELS[name].items() if k in members}
    # A third-party plugin we have no labels for: still show its declared
    # keys verbatim rather than pretending it has none.
    return {m: (m, '') for m in sorted(members)}


def _item(key, label_hint, stored):
    label, hint = label_hint
    present = key in stored
    value = stored.get(key)
    return {
        'key': key,
        'label': label or key,
        'hint': hint,
        'present': present,
        'value': value,
        'type': type(value).__name__ if present else '',
        # Absent keys are editable (writing creates them); present ones only
        # while the value is a scalar.
        'editable': (not present) or isinstance(value, _SCALARS) or value is None,
    }


def build_groups(plugins, stored):
    """Ordered groups: 核心 → MPV 播放器 → 每个插件 → 其他（未归类）.

    `plugins` is an iterable of loaded MacastPlugin objects (anything with
    `.module` / `.title` / `.version`); pass [] when there is no manager.
    `stored` is the persisted settings dict.
    """
    stored = stored or {}
    groups = [{
        'id': CORE_GROUP,
        'title': 'Macast 核心',
        'version': '',
        'keys': [_item(k, v, stored) for k, v in CORE_LABELS.items()],
    }, {
        'id': PLAYER_GROUP,
        'title': 'MPV 播放器',
        'version': '',
        'keys': [_item(k, v, stored) for k, v in PLAYER_LABELS.items()],
    }]
    claimed = set(CORE_LABELS) | set(PLAYER_LABELS)
    seen_titles = set()
    for plugin in plugins or []:
        module = getattr(plugin, 'module', None)
        if module is not None and plugin_module_name(module) == 'mpv':
            # The built-in renderer's keys are the 播放器 group above; giving
            # it its own card would list PlayerHW twice (and its enum holds
            # value constants the player group deliberately omits).
            continue
        labels = _labels_for_module(module) if module is not None else {}
        if not labels:
            # A plugin without a SettingProperty of its own still gets a
            # card: the page says「该插件没有持久化配置」instead of silently
            # dropping it, so "where did my plugin go" cannot happen here.
            title = getattr(plugin, 'title', '') or plugin_module_name(module)
            if title and title not in seen_titles:
                seen_titles.add(title)
                groups.append({
                    'id': 'plugin:' + (plugin_module_name(module) or title),
                    'title': title,
                    'version': getattr(plugin, 'version', '') or '',
                    'keys': [],
                })
            continue
        mid = plugin_module_name(module) or getattr(plugin, 'title', 'plugin')
        groups.append({
            'id': 'plugin:' + mid,
            'title': getattr(plugin, 'title', mid),
            'version': getattr(plugin, 'version', '') or '',
            'keys': [_item(k, v, stored) for k, v in labels.items()],
        })
        claimed |= set(labels)
    leftovers = sorted(k for k in stored if k not in claimed)
    if leftovers:
        groups.append({
            'id': OTHER_GROUP,
            'title': '其他（未归类）',
            'version': '',
            'keys': [_item(k, (k, '未识别归属的键；确认无用后可直接删除。'), stored)
                     for k in leftovers],
        })
    return groups


def owned_keys(plugins, stored):
    """Every key the page may write: a group member (claimed or not)."""
    keys = set(CORE_LABELS) | set(PLAYER_LABELS)
    for plugin in plugins or []:
        module = getattr(plugin, 'module', None)
        if module is not None:
            keys |= set(_labels_for_module(module))
    keys |= set(stored or {})
    return keys


def payload(plugins, stored):
    """GET module-settings response body."""
    return {'code': 0, 'groups': build_groups(plugins, stored)}


def _load_stored():
    from .utils import Setting
    if not bool(Setting.setting):
        Setting.load()
    return Setting.setting


def plugins_from_manager():
    """Loaded MacastPlugin objects from the manager (renderer + protocol),
    tolerating a manager that has not been published (tests, early boot)."""
    from .utils import cherrypy_publish
    manager = cherrypy_publish('get_plugin_manager', None)
    if manager is None:
        return []
    plugins = []
    for attr in ('renderer_all', 'protocol_list'):
        try:
            plugins.extend(getattr(manager, attr) or [])
        except Exception:
            continue
    return plugins


def settings_payload():
    return payload(plugins_from_manager(), _load_stored())


def set_value(key, raw_value, remove=False):
    """One write path for the page: scalar values and key removal only.

    CherryPy hands us text; anything that parses as JSON container is
    refused here on purpose — lists/dicts belong to the raw JSON editor so
    this tab can never half-edit a structure a plugin depends on.
    """
    from .utils import Setting
    stored = _load_stored()
    key = str(key or '').strip()
    if not key:
        return {'code': 1, 'message': '缺少设置项名称'}
    if key not in owned_keys(plugins_from_manager(), stored):
        return {'code': 1, 'message': '未知设置项：%s' % key}
    if remove:
        was_present = key in stored
        if was_present:
            del stored[key]
            Setting.save()
        return {'code': 0, 'message': '已删除' if was_present else '本就未设置',
                'removed': was_present}
    import json
    try:
        value = json.loads(raw_value)
    except (ValueError, TypeError):
        value = raw_value
    if isinstance(value, (dict, list)):
        return {'code': 1,
                'message': '列表/对象值请在「高级设置」JSON 里编辑'}
    stored[key] = value
    Setting.save()
    return {'code': 0, 'message': '已保存', 'value': value}
