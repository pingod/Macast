# Chromecast / AirPlay 接收端 · 真机验证指南

本指南用于在**真实局域网 + 真实手机/电脑**上验证 Macast 新增的 Chromecast 与 AirPlay
接收端能力。自动化验证（`scripts/verify_cast_airplay.py`，48/48 通过）只覆盖协议逻辑与
本机 socket，无法替代真机验证。

> 本轮已用真实局域网跑过一遍发现验证，期间修掉 4 个只在真机/真实网络才暴露的问题，
> 见 [第 10 节](#10-本轮已修的真机问题2026-09-15)。

---

## 0. 先明确能力边界（避免误判为 Bug）

| 能力 | 状态 |
|---|---|
| Chromecast 接收端（发送端给 URL，mpv 播放） | 已实现，待真机验证 |
| AirPlay **视频** URL 投屏 | 已实现，待真机验证 |
| AirPlay **音频**（RAOP / ALAC） | **未实现** |
| AirPlay **屏幕镜像** | **未实现**（需实时 H.264 解码） |
| Google 官方发送端（Chrome / 安卓 Play 服务 / iOS SDK） | **大概率失败**：Google 用出厂密钥做设备认证，第三方接收端无法签名 |
| 第三方发送端（VLC、Home Assistant、go-chromecast、Jellyfin 系） | 可能可用（常跳过设备认证） |
| 受 DRM 保护内容（Netflix / Apple TV app 等） | **不可能支持**（连 UxPlay 等成熟实现也做不到） |

---

## 1. 准备

```shell
cd $REPO
pip install -r requirements/common.txt     # 本轮新增了 zeroconf
```

> 说明：`requirements/darwin.txt` 另含 `rumps`（菜单栏）。从源码运行需要它。

## 2. 从源码启动（推荐，便于看日志）

```shell
cd $REPO
python Macast.py
```

⚠️ **重要**：`Macast.py` 启动时会调用 `clear_env()` **清空** `macast.log`。
因此顺序必须是：**先启动 → 再复现问题 → 再看日志**，不要事后翻旧日志。

## 3. 选择协议

Macast **同时运行多个协议**（DLNA 走 SSDP，Chromecast/AirPlay 各走 mDNS），
菜单栏里每一项都是开关（复选框），可任意组合、默认全开。
切换即时生效，**不会重启服务**——钩掉 Chromecast 不会中断正在播放的 DLNA。

> 想在 GUI 外改配置：编辑
> `~/Library/Application Support/Macast/macast_setting.json` 的 `Macast_Protocols`
> （数组，如 `["DLNA Protocol","Chromecast","AirPlay"]`）后重启。
> 旧的 `Macast_Protocol` 单值字段仍会被识别并自动迁移。

> 注意 DLNA 插件的真实标题是 **`DLNA Protocol`**（由类名推导），
> 写 `"DLNA"` 也能识别（有容错匹配），但落盘的始终是规范名。

## 4. 确认服务已起（日志定位点）

日志路径（macOS）：

```shell
LOG="$HOME/Library/Application Support/Macast/macast.log"
tail -f "$LOG"
```

关键日志行（两个协议各自会打出来）：

```shell
grep -E "ChromecastProtocol started|AirPlayProtocol started|mDNS advertised" "$LOG"
```

预期看到：

```
ChromecastProtocol started on port 8009      # 或回退端口
AirPlayProtocol started on port 7001         # 7000 常被系统 AirPlay 接收器占用 → 自动回退
mDNS name 'Macast(Host.local)._...' normalised to 'Macast-Host._...' for DNS-SD
mDNS advertised Macast-Host._googlecast._tcp.local. on port 8009
mDNS advertised Macast-Host._airplay._tcp.local. on port 7001
```

> 设备名里的括号/点会被规范化成 `-`（DNS-SD 标签只允许 `A-Za-z0-9-`），
> 这是**必须的**：不规范化时 zeroconf 会“注册成功”但局域网根本搜不到。

若看到 `AirPlay port 7000 already in use (macOS AirPlay Receiver?), using 7001 instead`
——这是**正常的自动回退**，端口会写进 mDNS，发送端会自动跟随。

## 5. 局域网发现验证（先于投屏做，最容易定位问题）

在**同一局域网**的另一台机器（或本机）执行：

```shell
# macOS / Windows(Bonjour)
dns-sd -B _googlecast._tcp
dns-sd -B _airplay._tcp

# 查看 SRV + TXT 记录（确认端口与主机名；名字是规范化后的）
dns-sd -L "Macast-MyHost-1754" _googlecast._tcp local
dns-sd -L "Macast-MyHost-1754" _airplay._tcp    local

# Linux
avahi-browse -rt _googlecast._tcp
avahi-browse -rt _airplay._tcp
```

✅ 能看到设备名和端口 = 发现正常。
❌ 看不到 → 检查：同一网段、路由器未开 AP 隔离、防火墙放行（mDNS UDP 5353、
Cast TCP 8009/8008、AirPlay TCP 7000–7020）。

**注意**：mDNS 走 UDP 5353 且是链路本地多播，跨网段/VLAN/部分企业网络不可达。

## 6. Chromecast 投屏验证

1. 先确认通道可达（无需手机）：

```shell
curl http://<你的IP>:<setup_port>/setup/eureka_info
# setup_port 默认 8008，若被占用会回退，见日志
```

2. 用发送端尝试：
   - **Chrome 桌面版**：菜单 → 投射…（**预计因设备认证失败**，属已知预期）
   - **第三方发送端更可能成功**：VLC 移动端、Home Assistant、go-chromecast、Jellyfin

3. 成功标志（日志）：

```
Cast sender connected: sender-0
Cast device-auth challenge from sender-0 (uncertified: empty signature)
Cast LOAD url=http://...
```

随后 mpv 应拉起并播放。

## 7. AirPlay 投屏验证

1. iPhone/iPad 与电脑同一 Wi-Fi → 控制中心 → 屏幕镜像 / 或视频 App 的 AirPlay 图标。
2. 若镜像选项**不出现**：属预期（我们未在 features 里声明镜像能力，避免虚标）。
3. 用支持 URL 投屏的 App（如相册视频、Safari 视频）尝试投**视频**。
4. 成功标志（日志）：

```
AirPlay ANNOUNCE url=http://...
```

随后 mpv 应拉起并播放。

## 8. 排障速查

| 现象 | 可能原因 / 处理 |
|---|---|
| 发送端根本搜不到设备 | mDNS 未通：网段/AP 隔离/防火墙 UDP 5353 |
| 搜到但连不上（Chromecast） | Google 设备认证；换第三方发送端试试 |
| 连上但立刻断开 | 心跳/连接命名空间问题；贴日志给我 |
| 能连但不播放 | 看日志里是否有 `Cast LOAD url=` / `AirPlay ANNOUNCE url=`，没有说明发送端没发 URL |
| AirPlay 只有镜像选项、投视频无反应 | 当前只支持视频 URL 投屏 |
| 端口冲突 | 看日志里的 `already in use ... using NNNN instead`，属正常回退 |

## 9. 不开 GUI 也能验证发现（推荐先跑这个）

`scripts/smoke_discovery.py` 会**脱离 Macast 主程序**，直接在真实局域网上拉起
Chromecast(8009) 与 AirPlay(7001) 两个接收端并广播 mDNS：

```shell
cd $REPO
.venv/bin/python scripts/smoke_discovery.py --seconds 60

# 另开一个终端
dns-sd -B _googlecast._tcp local
dns-sd -B _airplay._tcp    local
```

看到 `Macast-<主机名>` 即发现正常。它会打印发送端送来的 URL，方便不拿手机也能确认链路。

## 10. 本轮已修的真机问题（2026-09-15）

真实网络验证比单元测试多抓到 4 个 bug，均已修复并有回归用例（48/48）：

| # | 问题 | 表现 | 修复 |
|---|---|---|---|
| 1 | zeroconf 默认双栈 | IPv6 socket 发不出去，`sendto ... Can't assign requested address`；**广播发出但全网搜不到** | `Zeroconf(ip_version=IPVersion.V4Only)` |
| 2 | 实例名含括号/点 | `Macast(Host.local)` 被 zeroconf 接受且“注册成功”，但 `dns-sd -B` 永远搜不到 | `sanitize_instance_name()` 规范化为 `A-Za-z0-9-` |
| 3 | SRV 主机名重复后缀 | macOS 主机名自带 `.local`，又拼一次 → `Host.local.local.` | `_normalize_server()` |
| 4 | 切协议不重新评估 SSDP | DLNA 切到 AirPlay 后仍在广播已失效的 DLNA 设备 | `Service._sync_ssdp()` |

另外修复：**`ProtocolPlugin` 无条件订阅 `protocol.cast_uri`**，导致 AirPlay 一启动就
`AttributeError` 崩溃（只有真跑起来才会遇到，单元测试没覆盖）。

## 11. 多协议并发（2026-09-15 起支持）

三个协议可以同时在线。实现要点：

- `macast/protocol_group.py` 的 `ProtocolGroup`：对外伪装成单个 `Protocol`，
  对内把 `start/stop/uses_ssdp/set_state_*` 扇出到所有子协议。
  这样既有的 CherryPy 树根、总线订阅、SSDP 逻辑一行不动。
- `handler` 取自 **primary**（优先 DLNA，因为 `DLNAHandler` 继承自 `Handler`，
  既提供 UPnP 路由也提供 Web UI / PWA / 管理 API）。
- `macast/protocol.py` 的 `PlaybackGuard`：mpv 只有一个，多发送端同时投屏时
  记录播放归属，后来的接管并通知前一个协议释放（默认空实现，可覆盖）。
- 菜单从单选改为多选；设置项 `Macast_Protocol`(str) → `Macast_Protocols`(list)，
  旧值自动迁移。

⚠️ **踩过的坑**：基类 `Protocol` 本身就定义了 `set_state_*` 空方法，
导致 `group.set_state_play` 命中类属性、`__getattr__` 扇出根本不会被调用
——播放状态会静默丢弃。必须把扇出函数装到**实例属性**上才能盖过类方法。

## 12. 反馈给我时请贴出

```shell
LOG="$HOME/Library/Application Support/Macast/macast.log"
grep -E "Chromecast|AirPlay|Cast |mDNS|Discovery|ERROR|Error|Traceback" "$LOG" | tail -100
```

以及：发送端（什么 App / 系统版本）、现象（搜不到 / 连不上 / 连上不播）。

---

## 附：自动化测试（本机，无需真机）

```shell
cd $REPO
.venv/bin/python scripts/verify_cast_airplay.py
```

覆盖：Cast 编解码与指令路由、AirPlay RTSP 路由、真实 TLS/RTSP 端到端、
mDNS 广播参数与实例名规范化、SSDP 随协议切换、以及
`MacastPluginManager` 是否真的注册了三个协议。

> 源码运行依赖 `.venv`（Python 3.12 + `requirements/darwin.txt` + `pillow zeroconf pyperclip`）。
> 若用 WorkBuddy 的 Bash 跑 pip 报 `EEXIST: file already exists, mkdir ...`，是 CLI 注入的
> `sitecustomize` 补丁导致，加 `env -u PYTHONPATH` 即可；
> 从源码启动也请用 `scripts/run-from-source.sh`（它同样会清掉该变量）。
