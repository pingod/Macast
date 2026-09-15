# `plugins/` — 在线插件索引

这个目录**不是** Macast 内置插件所在的地方（那是 `macast/plugins/`）。
它是设置页「插件 → 可安装」分组的数据源：一个静态的 `info.json`，由设置页在
浏览器里直接拉取；提供安装的插件 `.py` 也放这里。

## 为什么不放上游那 6 个插件

上游插件合集 `xfangfang/Macast-plugins` 里发布的 6 个插件，现在**全部内置**在
本仓库的 `macast/plugins/` 下，随应用一起发布：

| 插件 | 类型 | 平台 |
|---|---|---|
| IINA Renderer | renderer | darwin |
| Web Renderer | renderer | darwin,linux,win32 |
| Live Renderer | renderer | win32,darwin,linux |
| PotPlayer Renderer | renderer | win32 |
| PIFMRDS Renderer | renderer | linux |
| NVA Protocol | protocol | darwin,linux,win32 |

继续读上游索引只会给每个内置插件再生成一张重复卡片，还挂着指回上游旧文件的
「可更新」角标。所以索引搬到了这里。索引为空也是合法状态：设置页拉到一个空
`plugin_v1` 时只显示本机插件，不报错。

内置插件的发现、热插拔与 `<macast.*>` 清单格式见 `AGENTS.md` §4.4 / §4.5。

## 当前提供的插件

| 文件 | 插件 | 为什么放在线而不是内置 |
|---|---|---|
| `macast_ytdlp.py` | **yt-dlp Downloader** — 把投屏链接交给 yt-dlp 下载到本地，或边下边播 | 依赖用户自己装的 `yt-dlp` 命令 |
| `external_player.py` | **External Player (VLC / MPC-BE / mpv.net)** — 用你自己的播放器放 | 只对装了那个播放器的人有用 |
| `floating.py` | **Floating Player** — 角落置顶小窗，含实验性壁纸模式 | 纯偏好，跟版本无关 |
| `hooks.py` | **Automation Hooks** — 投屏 / 暂停 / 继续 / 停止时执行你的命令 | 命令因人而异，配置在设置里 |
| `cast_bridge.py` | **Chromecast Bridge** — 把收到的投屏转投给另一台 Chromecast | 只对有多台设备的人有用 |
| `raop.py` | **AirPlay Audio (RAOP)** — 监督 shairport-sync，接收 AirPlay 音频 | 需要用户自己装 `shairport-sync` |

**Macast 一次只能用一种渲染器**，所以 `macast_ytdlp` / `external_player` / `floating` /
`hooks` / `cast_bridge` 是互斥的（菜单栏里切换）；`raop.py` 是协议插件，可以和任意渲染器同时开。

各插件要点：

- **yt-dlp**：模式在菜单里切（下载 / 边下边播）。下载目录默认 `~/Downloads/Macast`，
  可用设置页「高级设置」的 JSON 编辑器改 `YTDLP_Dir`。边下边播走 mpv 自带的 ytdl hook，
  插件会把 `ytdl_hook-ytdl_path` 指给你装的那个 yt-dlp。暂停/继续在下载模式下**故意不实现**
  （它是下载器不是播放器，假装 `PAUSED_PLAYBACK` 只会骗发送端）。
- **External Player**：玩家列表在菜单里选；没装的会灰显。选中的播放器可以用设置里的
  `External_Player` / `External_Player_Path` 覆盖。位置是模拟的（每秒 +1 秒），
  停止只杀我们启动的那个进程 —— VLC 单实例模式下会由已有窗口接管播放，这时停止不生效，
  我们不会去杀无关窗口。
- **Floating Player**：`Floating_Size` 是 `SIZES` 的下标，`Floating_Mode=1` 打开壁纸模式。
  壁纸模式需要 mpv 支持 `--ontop-level=desktop`（新版才有），不支持时会退回全屏置顶窗口并提示。
  壁纸模式是**实验性**的：能不能压在桌面图标下面取决于系统版本。
- **Automation Hooks**：四个命令写在设置里（菜单「Edit Hooks」直接打开设置文件）：
  `Hook_On_Cast` / `Hook_On_Pause` / `Hook_On_Resume` / `Hook_On_Stop`，都是 shell 命令，
  环境变量带 `MACAST_EVENT` / `MACAST_URL` / `MACAST_TITLE`。URL 只通过环境变量传递，
  **不拼进命令行**（它是网络来的字符串）。命令是 spawn 出去的，不等待、不阻塞协议线程。
- **Chromecast Bridge**：目标在菜单里选（mDNS 搜索 `_googlecast._tcp`，也可以直接在设置里写
  `Cast_Bridge_Target` = `host:port`，测试就是靠这条路径）。它复用 `macast.protocol_cast`
  的 Cast v2 收发实现，**不引入 pychromecast 依赖**。首次投屏前必须选好目标。
- **AirPlay Audio (RAOP)**：`brew install shairport-sync`（Linux 用包管理器）后启用即可，
  它自己会做 mDNS 广播。插件只负责用你的 Macast 名字生成配置、拉起进程、把连接/断开报给你。
  **不**把 RAOP 映射成 DLNA 播放状态（RAOP 没有媒体 URL，硬报 PLAYING 会和 DLNA 的状态账本打架）。

## 什么时候该往这里加东西

只有当某个插件**不该随包发布**时才放这里，例如：

- 依赖体积大或平台受限、不适合塞进安装包的播放器适配；
- 只在特定环境才装得上的实验性协议（需要额外账号 / 外部服务）；
- 会频繁独立更新的插件。

已经内置的插件**不要**再往这里加一份。

## 索引地址（唯一来源）

`macast/plugin_repo.py` 定义仓库坐标与两个候选地址，由
`/api?query=plugin-info` 下发，设置页本身不写死任何 URL：

```
https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json      # 首选，永远最新
https://ghproxy.net/https://raw.githubusercontent.com/.../info.json        # 国内可达，也是最新的
https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json            # 垫底：会缓存数小时
```

设置页按顺序试，第一个能通的即采用；三个都不通就只显示本机插件，并给一句提示。
jsDelivr 垫底是因为它**缓存分支文件**（可能给出过期索引）；ghproxy 类镜像按需代理 raw，
拿到的是最新内容。改这个顺序前先想清楚「新插件多久能被看到」。

> 浏览器直接抓取要求对方返回 CORS 头 —— raw.githubusercontent.com 与
> cdn.jsdelivr.net 都满足；如果将来换成别的托管，先确认这一点。

## `info.json` 格式

```json
{
    "repo_url": "https://github.com/pingod/Macast/tree/main/plugins",
    "plugin_v1": [
        {
            "title": "Some Renderer",
            "type": "renderer",
            "renderer": "SomeRenderer",
            "platform": "darwin,win32,linux",
            "version": "1.2",
            "author": "pingod",
            "desc": "一句话说明这个插件做什么。",
            "host_version": "0.7",
            "url": "https://raw.githubusercontent.com/pingod/Macast/main/plugins/some_renderer.py"
        }
    ]
}
```

字段说明：

| 字段 | 说明 |
|---|---|
| `type` | `renderer` 或 `protocol`；决定安装到哪个插件目录 |
| `renderer` / `protocol` | 插件类的类名。**这是「本机装没装」的判定键**：卡片用它和本机插件比对，比对成功才显示成"已安装 / 可更新" |
| `platform` | 逗号分隔的 `sys.platform` 列表；不含当前系统的插件会灰显并说明原因 |
| `version` | 与 `.py` 里 `<macast.version>` 保持一致，否则会出现假的"可更新"角标 |
| `url` | `.py` 文件的直链，需以 `.py` 结尾（后端按扩展名落盘） |

`url` 指向本目录里的文件即可，例如 `plugins/some_renderer.py`。

**插件的 `url` 必须来自「每次都新鲜」的源**：安装走的是 **Python 侧**
`requests`（`MacastPluginManager.install_url`），没有设置页那种浏览器多地址回退。实测结论：

- `raw.githubusercontent.com` 最权威但国内经常拉不动；
- `cdn.jsdelivr.net` 能通，**但它会缓存分支文件数小时，而且 `?v=` 查询串不能破它的缓存**
  （实测：把插件升到 0.2 后，`...macast_ytdlp.py?v=0.2` 返回的仍是 0.1 的内容）；
- `ghproxy.net/https://raw.githubusercontent.com/...` 按需代理 raw，**每次都是最新文件**，
  所以索引里的 `url` 用它。

ghproxy 挂了的时候，把 raw 直链粘到设置页的「从网址安装」即可。索引里的
`url` 一律按这个规则写，Part 5c 会检查它不是缓存型 CDN。

## 改完怎么验

```shell
env -u PYTHONPATH .venv/bin/python -m pyflakes macast/plugin_repo.py
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py   # 含插件索引用例
curl -s 'http://127.0.0.1:58880/api?query=plugin-info' | python3 -m json.tool | head
```

注意 `setting.html` 在 `Handler.__init__` 里被缓存，改前端模板必须重启 Macast 才生效。
