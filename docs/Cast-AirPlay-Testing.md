# Chromecast / AirPlay 接收端 · 真机验证指南

本指南用于在**真实局域网 + 真实手机/电脑**上验证 Macast 新增的 Chromecast 与 AirPlay
接收端能力。自动化验证（`scripts/verify_cast_airplay.py`，138/138 通过）只覆盖协议逻辑与
本机 socket，无法替代真机验证。

> 已用真实局域网跑过两轮验证，期间修掉若干**只在真机/真实网络才暴露**的问题，
> 见 [第 10 节](#10-本轮已修的真机问题2026-09-15) 与
> [第 10.5 节](#105-第二轮真机问题2026-09-15-晚)。

---

## 0. 先明确能力边界（避免误判为 Bug）

| 能力 | 状态 |
|---|---|
| Chromecast 接收端（发送端给 URL，mpv 播放） | 已实现，待真机验证 |
| AirPlay **视频** URL 投屏 | 已实现，待真机验证 |
| AirPlay **音频**（RAOP / ALAC） | **未实现** |
| AirPlay **屏幕镜像** | **未实现**（需实时 H.264 解码） |
| Google 官方发送端（Chrome / 安卓 Play 服务 / iOS SDK） | **大概率失败**：Google 用出厂密钥做设备认证，第三方接收端无法签名 |
| 第三方发送端（VLC、Home Assistant、go-chromecast、Jellyfin 系） | 可用；VLC 已实测通过（**但它会做设备认证**，回复必须严格合法，见 10.5） |
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

## 2.5 这些协议是"插件"吗？在设置页哪里看？

Chromecast / AirPlay **不是外部安装的插件**，而是**内置协议（built-in）**，
随 Macast 一起发布，源码在 `macast/protocol_cast.py` 与 `macast/protocol_airplay.py`。
两者与 DLNA 一样，都是 `Protocol` 子类，和用户插件走同一套注册机制。

在哪里看：

- **菜单栏 → Setting → Protocols**：勾选框，控制哪个协议在跑（多选、可同时开）。
- **设置页 → 插件**：浏览 `http://127.0.0.1:58880/` 的「插件」标签页。
  内置项在 **「内置」** 分组里，每张卡片标注了 `协议`/`渲染器`、`内置`，
  正在运行的还会带一个绿色 **`启用中`** 标签 —— 这样才不会和下面
  「可安装」的在线插件混在一起看不出区别。
- **设置页 → 状态**：`协议` 一行列出当前所有在跑的协议。

> 插件页的「可安装」分组要访问 GitHub 上的插件仓库，**取不到时该分组直接不显示**，
> 不影响内置协议与页面其余部分（以前会一直卡在 loading）。

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

## 10.5 「能找到但投不上去」——两个真正的投屏失败原因（2026-09-15）

设备能被搜到、点投屏却一直失败。用第三方发送端 **pychromecast** 复现后定位到两个
**只有在真实发送端 + 真实局域网下才暴露** 的问题，与 Google 设备认证无关
（VLC 用的是自己的发送端实现，不做设备认证）：

### (1) mDNS 广播了 5 个地址，只有 1 个能连

`Setting.get_ip()` 会把**所有"带网关"的接口**都算进来。本机有 VM 网桥和 Tailscale，
于是广播出去的是：

| 地址 | 来源 | 手机能连 |
|---|---|---|
| `192.168.1.6` | Wi-Fi (en0，默认路由) | ✅ |
| `192.168.139.3` | VM 网桥 bridge100 | ❌ |
| `192.168.97.0` | VM 网桥 bridge102 | ❌ |
| `192.168.215.0` | VM 网桥 bridge101 | ❌ |
| `100.85.176.107` | Tailscale (100.64/10) | ❌ |

zeroconf 把这些**全部**写成 SRV 主机的 A 记录，发送端解析后 5 选 1 —— 复现时
pychromecast 恰好挑中 `192.168.215.0`，连接直接超时。

修复：`discovery.py` 只广播**承载 IPv4 默认路由的接口**（外加用户显式配置的
`Additional_Interfaces`，减去 `Blocked_Interfaces`），并过滤回环、链路本地、
`/32` 点对点隧道和 CGNAT 段。SRV 主机名也改为由服务实例名推导
（`Macast-<主机名>.local.`），不再用机器主机名 —— 后者由系统另行广播，
容易出现两套不一致的 A 记录。

### (2) RECEIVER_STATUS 的 `namespaces` 格式错了

`cast_channel.proto` 里是 `message Namespace { string name = 1; }`，所以
`applications[].namespaces` 必须是**对象数组**：

```json
"namespaces": [{"name": "urn:x-cast:com.google.cast.media"}, ...]
```

我们之前发的是**字符串数组**。宽松的发送端不报错，严格的发送端（pychromecast、
Cast SDK）会解析失败并认定"当前 app 不支持 media 命名空间"，
于是**永远不发 LOAD** —— 表现就是"能搜到、点投屏没反应"。

修复：`_app_namespaces()` 输出对象数组；同时把 8008 的 `/setup/eureka_info`
改成真实 Chromecast 的嵌套结构（`device_info`），并支持 `?params=` 过滤。

两个问题都补了回归用例（93/93）。

### 10.5 第二轮真机问题（2026-09-15 晚）

手机 VLC **仍然投不上**，日志显示它连上了 8009、发了一个 device-auth challenge、
然后就没有下文了。查 VLC 源码
（`modules/stream_out/chromecast/chromecast_ctrl.cpp`）才看清它到底在等什么：

```cpp
void intf_sys_t::processAuthMessage( const castchannel::CastMessage& msg )
{
    castchannel::DeviceAuthMessage authMessage;
    if ( authMessage.ParseFromString(msg.payload_binary()) == false ) {
        msg_Warn( m_module, "Failed to parse the payload" );
        return;                      // 永远停在 Authenticating
    }
    if (authMessage.has_error()) { ... }
    else if (!authMessage.has_response()) { msg_Err(...); }
    else {
        setState( Connecting );
        m_communication->msgConnect( DEFAULT_CHOMECAST_RECEIVER );
        m_communication->msgReceiverGetStatus();
    }
}
```

**根因 3：device-auth 回复的 protobuf 少套了一层。**
`cast_channel.proto` 里：

```protobuf
message DeviceAuthMessage {
  optional AuthChallenge challenge = 1;
  optional AuthResponse  response  = 2;   // ← 回复必须放这里
  optional AuthError     error     = 3;
}
message AuthResponse {
  required bytes signature                = 1;   // required！
  required bytes client_auth_certificate  = 2;   // required！
  repeated bytes intermediate_certificate = 3;
}
```

我们之前直接把 `AuthResponse` 当顶层消息发（`0a001200`），发出来被解析成
`DeviceAuthMessage{ challenge: {}, response: {} }` —— `response` 字段在、但里面
**空**，`signature` / `client_auth_certificate` 这两个 **required** 字段缺失，
C++ protobuf 在 `ParseFromString()` 阶段就报错。VLC 于是 `return`，状态机卡死在
`Authenticating`，一个字节都不再发。

修复后（`12040a001200`）：`encode_auth_response()` 输出
`DeviceAuthMessage{ response: AuthResponse{ signature: "", client_auth_certificate: "" } }`，
并支持传入真证书与 CA 链（有证书时走认证态）。

**根因 4：一次失败的 TLS 握手会永久打死接收端。**
原来的写法是把**监听 socket** 包成 TLS：

```python
raw.listen(5)
self._tls_server = ctx.wrap_socket(raw, server_side=True)   # ← 问题在这
```

`SSLSocket.accept()` 会在**调用线程里同步跑握手**，于是任何一次握手失败都会从
`accept()` 抛 `ssl.SSLError` —— 而 `SSLError` 是 `OSError` 的子类，正好被

```python
try:
    conn, addr = self._tls_server.accept()
except OSError:
    break                    # ← 被当成"监听器没了"，整个循环退出
```

吃掉并 `break`。后果：**设备 mDNS 广播照旧，8009 也还是 LISTEN，但没人 accept**，
端口探活成功、TLS 握手超时。手机端看到的正是"能搜到、投不上去"，而且**重启前无法恢复**。
（我用 `lsof` 看到端口在听、`socket.connect()` 也能成功，但 `wrap_socket` 5 秒超时，
才定位到这里。）

修复：
- 监听 socket 保持明文，**每条连接**再 `wrap_socket`，握手失败只影响那一条；
- `_accept_loop(server)` 把 socket 作为参数传入，避免上一轮的僵死线程 accept 到新监听器；
- 循环里区分"监听器没了"（`_stop_event` 或 `fileno() < 0`）和"单次连接出错"，后者只记日志继续。

同时把 `Handler.get_status()` 里 `protocol.event_subscribes` 改成 `getattr(..., {})`：
`ProtocolGroup` 在只启用 Chromecast 时会把 `event_subscribes` 委派给
`ChromecastProtocol`，抛 AttributeError（虽然被 catch，但日志全是噪声）。

以上都补了回归用例（138/138），包括"3 次半开连接后仍能正常建会话"和
"stop/start 之后仍能建会话"。

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

## 11.5 网卡选择 & 插件热插拔（2026-09-15 新增）

### 网卡选择（设置页 → 状态 → 网络与广播）

多网卡机器（虚拟机网桥、VPN、Tailscale）上，"能搜到但投不上"十有八九是广播用了
错的地址。现在可以显式指定：

- `GET /api?query=interfaces` 返回 `{current, advertised, interfaces[]}`，
  每项含 `name / ip / netmask / default / usable / reason`。
- `POST /api  set-interface=<name>`（空串=自动）落盘 `Network_Interface`，
  然后**立即重新广播**：`ssdp_update_ip` 让 SSDP 重绑，mDNS 由协议 stop/start 重新注册。
  不重启，mpv 正在播的内容不受影响。
- 后端拒绝不存在的网卡名（否则会静默退化成回环，看起来"配置好了"但其实不可见）。

语义统一在 `Setting.resolved_network_interface()`：显式 pin 只在**该网卡确有
局域网可达 IPv4** 时生效，否则退回自动选择并打 warning。SSDP（`get_ip()`）和
mDNS（`discovery._advertisable_interfaces()`）都走这一个判断，不会再出现
"mDNS 走 Wi-Fi、SSDP 却绑在死掉的隧道上"。

地址过滤规则（`utils.unadvertisable_reason()`，前端直接展示这段文案）：
回环、链路本地、`/32` 点对点、`100.64/10`（Tailscale CGNAT）。

### 插件热插拔（设置页 → 插件）

内置协议过去只能在菜单栏勾选，第三方插件装上就必须重启。现在设置页就能
**启用 / 停用 / 卸载 / 安装**，全部**即时生效、不重启**：

| 动作 | 接口 | 说明 |
|---|---|---|
| 启用/停用 | `plugin-enable` / `plugin-disable` + `plugin-key` | 协议改 `Macast_Protocols`；渲染器改 `Disabled_Plugins` |
| 卸载 | `plugin-uninstall` + `plugin-key` | 仅文件插件；内置插件拒绝 |
| 安装 | `install-plugin` + `plugin-url`/`plugin-type`（或上传 `plugin-file`） | 下载/上传后热加载 |

要点：

- **`plugin-key` 是稳定标识**：内置插件 `protocol:<标题>` / `renderer:<标题>`；
  文件插件 `protocol:<文件名>` / `renderer:<文件名>`（避免同名文件冲突）。
- **卸载是可恢复的**：文件 `mv` 到 `SETTING_DIR/.trash/<时间戳>/`，不是删除。
- **保护规则**：不能停用最后一个启用协议；不能停用正在使用的渲染器；内置插件不能卸载。
- **安装会回滚**：加载失败或平台不匹配的插件立即删掉，不留半装状态。
- **`_import_plugin_module()` 会 `reload`**：importlib 有模块缓存，不 reload 的话
  重装插件仍跑旧代码 —— 那正是热插拔想避免的。
- 菜单重建走 `App.call_on_main_thread()`：CherryPy 工作线程改 AppKit 菜单不安全。

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
mDNS 广播参数与实例名规范化、SSDP 随协议切换、`DeviceAuthMessage` 线格式
（含"旧写法必须被拒"的反例）、网卡枚举/过滤/pin 语义、插件热插拔
（键稳定性、停用保护、卸载进回收站、安装回滚、坏插件不残留），以及
"半开连接 + stop/start 后接收端仍然可用"。

想用一个**忠实复刻 VLC 状态机**的发送端做端到端回归：

```shell
.venv/bin/python scripts/vlc_sender_sim.py          # 默认 127.0.0.1:8009
```

> 源码运行依赖 `.venv`（Python 3.12 + `requirements/darwin.txt` + `pillow zeroconf pyperclip`）。
> 若用 WorkBuddy 的 Bash 跑 pip 报 `EEXIST: file already exists, mkdir ...`，是 CLI 注入的
> `sitecustomize` 补丁导致，加 `env -u PYTHONPATH` 即可；
> 从源码启动也请用 `scripts/run-from-source.sh`（它同样会清掉该变量）。
