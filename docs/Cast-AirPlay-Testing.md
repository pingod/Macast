# Chromecast / AirPlay 接收端 · 真机验证指南

本指南用于在**真实局域网 + 真实手机/电脑**上验证 Macast 新增的 Chromecast 与 AirPlay
接收端能力。自动化验证（`scripts/verify_cast_airplay.py`，37/37 通过）只覆盖协议逻辑与
本机 socket，无法替代真机验证。

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
cd /Users/pavia/githome/Macast
pip install -r requirements/common.txt     # 本轮新增了 zeroconf
```

> 说明：`requirements/darwin.txt` 另含 `rumps`（菜单栏）。从源码运行需要它。

## 2. 从源码启动（推荐，便于看日志）

```shell
cd /Users/pavia/githome/Macast
python Macast.py
```

⚠️ **重要**：`Macast.py` 启动时会调用 `clear_env()` **清空** `macast.log`。
因此顺序必须是：**先启动 → 再复现问题 → 再看日志**，不要事后翻旧日志。

## 3. 选择协议

菜单栏图标 → **Setting → Protocols** → 选 `Chromecast` 或 `AirPlay`。

> **已知瑕疵 + 规避**：`Service` 只在启动时评估一次是否启用 SSDP，运行时切换协议不会
> 重新评估——选了 Chromecast/AirPlay 后，SSDP（DLNA 广播）可能仍在发。
> **选完请重启一次 Macast**，可确保状态干净（重启会按保存的协议重新初始化）。

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
mDNS advertised Macast(...)._googlecast._tcp.local. on port 8009
mDNS advertised Macast(...)._airplay._tcp.local. on port 7001
```

若看到 `AirPlay port 7000 already in use (macOS AirPlay Receiver?), using 7001 instead`
——这是**正常的自动回退**，端口会写进 mDNS，发送端会自动跟随。

## 5. 局域网发现验证（先于投屏做，最容易定位问题）

在**同一局域网**的另一台机器（或本机）执行：

```shell
# macOS / Windows(Bonjour)
dns-sd -B _googlecast._tcp
dns-sd -B _airplay._tcp

# 查看 TXT 记录（确认端口）
dns-sd -L "Macast(Pavia-MacBookPro-1754.local)" _googlecast._tcp
dns-sd -L "Macast(Pavia-MacBookPro-1754.local)" _airplay._tcp

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

## 9. 反馈给我时请贴出

```shell
LOG="$HOME/Library/Application Support/Macast/macast.log"
grep -E "Chromecast|AirPlay|Cast |mDNS|Discovery|ERROR|Error|Traceback" "$LOG" | tail -100
```

以及：发送端（什么 App / 系统版本）、现象（搜不到 / 连不上 / 连上不播）。

---

## 附：自动化测试（本机，无需真机）

```shell
/Users/pavia/.workbuddy/binaries/python/envs/macast/bin/python scripts/verify_cast_airplay.py
```

覆盖：Cast 编解码与指令路由、AirPlay RTSP 路由、真实 TLS/RTSP 端到端、
mDNS 广播参数、以及 `MacastPluginManager` 是否真的注册了三个协议。
