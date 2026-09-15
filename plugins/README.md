# `plugins/` — 在线插件索引（**当前为空，这是正常的**）

这个目录**不是** Macast 内置插件所在的地方（那是 `macast/plugins/`）。
它是设置页「插件 → 可安装」分组的数据源：一个静态的 `info.json`，由设置页在
浏览器里直接拉取。

## 为什么是空的

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
「可更新」角标。所以索引搬到了这里，`plugin_v1` 留空；设置页拉到一个空索引时
只显示本机插件，不会报错。

内置插件的发现、热插拔与 `<macast.*>` 清单格式见 `AGENTS.md` §4.4 / §4.5。

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
https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json   # 首选
https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json         # 镜像回退
```

设置页按顺序试，第一个能通的即采用；两个都不通就只显示本机插件，并给一句提示。
国内网络下 raw 域名常年不可达，jsDelivr 通常还行，所以顺序不要反过来。

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

`url` 指向本目录里的文件即可，例如 `plugins/some_renderer.py`；
用 raw 直链还是 `ghproxy` 之类的镜像由你决定，只要最终返回的是纯文本 `.py` 源码。

## 改完怎么验

```shell
env -u PYTHONPATH .venv/bin/python -m pyflakes macast/plugin_repo.py
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py   # 含插件索引用例
curl -s 'http://127.0.0.1:58880/api?query=plugin-info' | python3 -m json.tool | head
```

注意 `setting.html` 在 `Handler.__init__` 里被缓存，改前端模板必须重启 Macast 才生效。
