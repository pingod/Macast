# 参考项目评估：miraclecast / mkchromecast

评估日期：2026-09
对照 commit：miraclecast `0b7f1f1`、mkchromecast `aad239c`、Macast `8e02bb5` / v0.7.11
发送端基准库：**pychromecast 14.0.10**（就装在 Macast 自己的 `.venv` 里，本文件的库侧引用都取自它）

**问题**：这两个项目有没有必要集成？有没有可参考的？它们有我们没有的功能吗？它们的实现方式更正确吗？

**一句话结论**：

- **miraclecast**：同类，但**不是同一个协议**（Wi-Fi Display / Miracast），RTSP 角色相反、
  Linux-only、需要 root。**不要集成**；它的 RTSP 报文语义和自检脚本值得抄。
- **mkchromecast**：**方向相反**（它是发送端）。**不要集成**；真正的价值是——它走的是真实发送端栈
  （pychromecast），据此对我们做一致性核对，**已经定位到 7 处 Macast 接收端的真实缺陷**。

---

## 1. miraclecast（albfan/miraclecast，4.3k★，C）

### 1.1 它到底是什么

Wi-Fi Display（Miracast）实现，**只实现了 sink 侧**（README 原文：Display-Source side
"*This is not implemented yet*"）。组件：

| 文件 | 职责 |
|---|---|
| `src/wifi/wifid.c` | udev 网卡管理器；只接管被 udev 打上 `miracle` 标签或被 `--interface` 指定的网卡 |
| `src/wifi/wifid-supplicant.c` | 拉起 wpa_supplicant 做 P2P 组网；组内起 DHCP server / client；169.254 兜底 |
| `src/ctl/ctl-sink.c` | RTSP 对端；**向 source 的 7236 端口主动拨出**（`:588`）——WFD 里 sink 是 RTSP 客户端，和 AirPlay 里接收端当 server 正好相反 |
| `src/shared/rtsp.c` | 通用 RTSP 报文总线（3274 行，含增量解析器 + `$` 交织二进制帧），不含 WFD 语义 |
| `src/ctl/wfd.c` | 只有分辨率位图，**没有任何 RTSP 逻辑** |
| `res/miracle-gst`、`res/miracle-vlc` | 把 RTP 端口交给播放器；RTP/MPEG-TS **miraclecast 自己不解**，和 Macast 交给 mpv 是同一形状 |

### 1.2 协议重叠：零

对 `src/`、`res/`、`README.md` grep `ssdp|upnp|dlna|googlecast|_airplay|raop|mdns|avahi|bonjour`
**无任何命中**。发现机制是 udev + Wi-Fi Direct，不是 DNS-SD/SSDP。WFD profile 的 RTSP 动词
只有 `OPTIONS`/`GET_PARAMETER`/`SET_PARAMETER`（入）和 `OPTIONS`/`SETUP`/`PLAY`（出），
没有 SDP、没有 ANNOUNCE、拒绝非 `wfd_*` 参数。**没有任何非 Miracast 发送端会跟它说话**。

### 1.3 为什么不能集成

- 依赖 systemd sd-event/sd-bus、libudev、glib、wpa_supplicant、gstreamer；macOS 上**一行都编不过**。
  移植等于重写。AGENTS.md §4.3 已经证明 Macast 连多加一个纯 Python 依赖都会在打包时翻车。
- Miracast 需要 Wi-Fi Direct P2P 组网，**macOS 没有 P2P API**；Linux 上还要杀掉
  NetworkManager/wpa_supplicant 独占网卡（`res/miracle-utils.sh:164-177`），和"菜单栏常驻"的产品形态直接冲突。
- Macast 的能力边界（AGENTS.md §9）里 Miracast 本来就属于"不做"的那类。

### 1.4 值得借鉴（**不是代码，是语义和工具**）

1. **RTSP 分帧是字节精确的状态机**（`src/shared/rtsp.c:2184-2459`）：处理 `\r`、`\n`、`\r\r\n`、
   引号续行，还有 `$` 交织帧。Macast 的 `_RtspHandlerBase._read_request`
   （`macast/protocol_airplay.py:286-303`）按第一个 `\r\n\r\n` 切分，**把 Content-Length 之后的
   剩余字节直接丢掉** —— 客户端把 SETUP+PLAY 合并在一个 TCP 段里发出时，第二条请求静默消失。
   同一段的 `int(headers.get("Content-Length", "0"))` 遇到非数字头会抛异常，连接线程死掉且不回复。
2. **CSeq 是结构化的**（`rtsp.c:2102-2111`、`:1253-1275`），库层不可能写错；Macast 用
   `headers.get("CSeq", "1")`（`:142`）且头部字典**大小写敏感**（`:292`），客户端发 `cseq:`
   就会收到伪造的 `CSeq: 1`，CSeq 永久错位。
3. **真正的状态码与短语**（`rtsp.c:195-249`）；Macast 只有 `200 OK` 和 `"Error"`（`:307`），
   对**任何未知方法都回 200**（`:207`），ANNOUNCE 没带 URL 也回 200（`:164-170`），
   接着 PLAY 还回 `Range: npt=now-` / `RTP-Info`（`:185-186`）而实际什么都没播。
4. **每连接超时 + HUP 处理**（`rtsp.c:42`、`:2663-2678`、`:2850-2900`）；Macast 在 `recv`
   上无限阻塞（`:281-285`），每连接一个线程（`:343-345`），发送端消失就泄漏线程和 fd。
5. **会话串里的 `;` 参数要剥掉**（`ctl-sink.c:182-191`）：合法的 `Session: 1;timeout=60` 现在会被当成会话 id。
6. **全量报文日志**：`cli_debug("INCOMING: %s", rtsp_message_get_raw(m))`（`ctl-sink.c:28,49,176,205`），
   两个方向都打整条报文。Macast 的 AirPlay 只有零散日志。
7. **自检脚本**：`res/test-hardware-capabilities.sh`（先查 `iw phy | grep P2P` 再干活）；
   `res/test-viewer.sh` 逐个 `gst-inspect-1.0` 并打印可执行的补救步骤。
   Macast 目前这些检查只存在于 AGENTS.md 的散文里。

### 1.5 评级

| 做法 | 结论 |
|---|---|
| 集成代码 | **不做**（C + sd-bus/libudev/glib，macOS 编不过） |
| 集成协议 | **不做**（WFD 动词没有传输层，也没有发送端） |
| 借鉴具体做法 | **做**，约 120 行改 `protocol_airplay.py` + 一个自检脚本 |
| 当解析器 oracle | 可选：写分帧测试时拿 `rtsp.c` 当对照 |

---

## 2. mkchromecast（muammar/mkchromecast，2.3k★，Python）

### 2.1 它到底是什么

**发送端/控制点**：把 macOS/Linux 的本地音频/视频/系统声音投到 Chromecast / Sonos。
依赖 `pychromecast>=4.2`、`soco`、`Flask`、`PyQt5`、`psutil`、`netifaces`。

- 主链路：`pychromecast.get_chromecasts()`（`cast.py:79`）→ ffmpeg 采集编码
  （`pipeline_builder.py:68-138`）→ Flask `/stream` 逐块吐出（`stream_infra.py:187-227`）
  → `play_media("http://ip:5000/stream", type, stream_type="LIVE")`（`cast.py:323-327`）
  → `block_until_active(timeout=30)`（`cast.py:336`）。
- **node 路径是默认但是坏的**：仍是 macOS 默认后端（`__init__.py:131-132`），
  但 `node.py:45` 读一个只有类型标注的 `bitrate`（`node.py:41`）→ `UnboundLocalError`；
  它的二进制 `webcast.js` 根本没随仓库分发；退出时打印 "Reconnecting…" 后
  `raise Exception("Internal error: Never worked")`（`node.py:152`）。
- **Sonos 是死代码**：`_DisabledSonosCasting`（`cast.py:469-473`，docstring 自己写着
  "Half-hearted attempt at refactoring. **This is broken**"）**从未被实例化**，
  其 `play_cast` 在 `play_uri` 之前就抛异常（`cast.py:702-703`）。
- 它**没有**任何 SSDP/mDNS/UPnP/SOAP 代码（grep `SSDP|M-SEARCH|1900|239.255|SOAP|UPnP`
  零命中），全部委托给 pychromecast。

**它是反向的，所以功能层面没有可直接搬的东西。** 真正的价值在下一节。

### 2.2 它是一台"接收端一致性测试仪"（最有价值的部分）

**已经满足的**（对着 `.venv` 里的 pychromecast 14.0.10 逐条核对）：

| 客户端要求 | 出处 | Macast |
|---|---|---|
| `applications[].namespaces` 必须是 `[{"name":…}]` 对象数组 | `controllers/receiver.py` `_parse_status` 里 `[item["name"] …]` | ✅ `protocol_cast.py:55-64` |
| TXT `id` 必须能当 UUID 解析 | `discovery.py:227-238` | ✅ `Setting.get_usn()` 是裸 uuid4 |
| PING/PONG 心跳，20s 无 PONG 判 LOST | `controllers/heartbeat.py` | ✅ `protocol_cast.py:472-476` |
| LOAD 后要出现带 `mediaSessionId` 的 MEDIA_STATUS（`block_until_active` 靠它） | `controllers/media.py` | ✅ `protocol_cast.py:612,714` |
| LOAD_FAILED 要能收到 | `controllers/media.py` | ✅ `protocol_cast.py:602-605,693-696` |
| LAUNCH / STOP / GET_STATUS 要有回复 | `controllers/receiver.py` | ✅ `protocol_cast.py:481-489` |

**已定位的缺陷**（每条都给出了「Macast 行号 + pychromecast 行号 + 现象」）：

| # | 缺陷 | Macast | 客户端侧证据 | 现象 |
|---|---|---|---|---|
| 1 | `SET_VOLUME` **从不回复** `RECEIVER_STATUS` | `protocol_cast.py:497-502` 只调 renderer，不 `_send` | `controllers/receiver.py:244-272` 用 `WaitResponse` 等回复，`const.py:13` `REQUEST_TIMEOUT = 10.0` | 发送端调音量**阻塞 10s 后抛 `RequestTimeout`**。任何直接调 `ReceiverController.set_volume()` 的发送端都会撞上（Home Assistant 的音量条就是这条路径）。⚠️ 修正：mkchromecast 自己的 `cast.py:388,399`、`systray.py:523` 调的是 `self.cast.set_volume(...)`，而 **pychromecast 14 的 `Chromecast` 上根本没有这个方法**（只有 `volume_up/down`，而它们的实现又调用了这个不存在的方法，是上游 bug）——所以那份代码在 14 上会先 AttributeError，走不到这条路径。缺陷本身由 pychromecast 的代码路径确认，与 mkchromecast 是否能跑通无关 |
| 2 | `RECEIVER_STATUS.volume` **硬编码** `{"level": 1.0, "muted": False}` | `protocol_cast.py:527` | `receiver.py` `_parse_status` → `volume_data.get("level", 1.0)` | 发送端按 `status.volume_level ± 0.1` 算新音量（`cast.py:387-399`）：**音量减永远发 0.9，音量加永远顶到 1.0**，滑块冻死 |
| 3 | `MEDIA_STATUS.volume` 同样硬编码 | `protocol_cast.py:719` | `controllers/media.py` `volume_data = status_data.get("volume", {})` | 媒体级音量显示错误 |
| 4 | `displayName` 不是 `"Default Media Receiver"` | `protocol_cast.py:511-512` 回 `"Macast"`，但 appId 是 `CC1AD845` | mkchromecast `--hijack` 把 `display_name != "Default Media Receiver"` 当作"接收端被抢了" | **每 5 秒重新 LOAD 一次，无限重投循环**（`cast.py:410-447`）。真机对 `CC1AD845` 一律回 `"Default Media Receiver"` |
| 5 | `ssdp_udn` 带了 `"uuid:"` 前缀 | `protocol_cast.py:766`：`"uuid:{}".format(Setting.get_usn())` | `dial.py:258` `UUID(udn.replace("-",""))`，`dial.py:269` 捕获 `ValueError` → 返回 `None` | `"uuid:xxx"` 不是合法 UUID → `get_device_info` 返回 None → pychromecast 的**主机扫描兜底路径**（`discovery.py:365`）永远认不出 Macast。应发裸 UUID |
| 6 | `capabilities.multizone_supported` 缺失 | `protocol_cast.py:775-780` 的 capabilities 里没有这个键 | `dial.py:245` `capabilities.get("multizone_supported", True)`——**device_info 存在时缺省是 True** | pychromecast 认为我们支持多房间，去走 `https://host:8443/setup/eureka_info?params=multizone`（`dial.py:296-314`），而 Macast 只有 8008（`protocol_cast.py:757` 只是注释）→ 每次多一次失败请求。应显式 `false` |
| 7 | 端口回退会被**误判成音箱组** | `protocol_cast.py:40` `PORT_FALLBACK_RANGE=20` | `discovery.py:241-247`：`if service.port != 8009: cast_type = CAST_TYPE_GROUP` | 8009 被占（例如多开一个实例）时，Macast 在 pychromecast 眼里变成 *group*。这让 AGENTS.md §4.2 的"多实例"坑更糟 |

**其他规范层面不对但不痛**：

- 未知 namespace/type **静默丢弃**（`protocol_cast.py:446-454` 四个 `_on_*` 全是 if/elif，**没有 else**）。
  真实接收端回 `INVALID_REQUEST`。注意：**pychromecast 14 在这几条路径上并不强制要求**（未验证到它因此超时），
  所以这条按"规范正确性"排，不按"已知客户端故障"排。
- `MEDIA_STATUS.sessionId` 拿不到 app session 时会 `_random_session()` 造一个（`:720`）——
  规范里媒体会话必须属于某个 app 会话。
- `playerState` 缺 `idleReason`：播完/出错没有 `FINISHED`/`ERROR`。pychromecast 明确
  「消息里没有就清空 idle_reason」，HA 依赖这个字段。
- `isStandBy` 从未发送（只发了 `isActiveInput`，`:526`）；`receiver.py` `_parse_status` 对
  非 audio/group 类型缺省成 `True` = 待机。真机发 `false`。
- **播放中途状态不推送**：`_send_media_status` 只在应答命令时调用，`_watch_playback` 只在
  LOAD 后盯 8 秒（`:638,663-700`）。mpv 侧播完/报错时发送端不会收到任何消息。
  （mpv 的 EOF/错误已经映射成 `TransportState=STOPPED`，`macast/protocol.py:1035-1050`，
  所以这一点是可实现的——但**必须先确认不会破坏 VLC 状态机**。）

**做对的地方（不要改）**：`LOAD` 先回 `BUFFERING`、再由后台线程推 `PLAYING`/`LOAD_FAILED`
（`:587-615`、`:663-700`）——很多玩具接收端在这里直接撒谎回 PLAYING；
`supportedMediaCommands: 15` 只声明真支持的四项（PAUSE/SEEK/STREAM_VOLUME/STREAM_MUTE），
没有虚报 EDIT_TRACKS/PLAYBACK_RATE；`_clear_session` 在没有 app 时回 `applications: []`。

### 2.3 其他可借鉴 / 明确不借鉴

**借鉴**：

- **端到端发送端测试脚本**：`test.py:128-158` 的 `--test-connect-to <friendly_name>` 骨架
  （get_chromecast → play_media → 等 15s → stop → quit_app）。它现在是 **skip 状态**
  （`test.py:136` 自述 broken），但骨架正确。Macast 目前只有一个手写的
  `scripts/vlc_sender_sim.py`（忠实复刻 VLC 状态机），**缺一个真实 pychromecast 栈的探针**。
  补上它是本次评估里性价比最高的一件事。
- **Node 流服务的 Range/206 实现**（`nodejs/html5-video-streamer.js:25-27`）——
  和 AGENTS.md §6 里"mpv 需要 Range 否则 loading failed"的经验一致，可当参考实现。

**明确不借鉴**（mkchromecast 在这些地方比 Macast **差**）：

- 子进程生命周期：ffmpeg 是**每个 HTTP 请求** `Popen(stdout=PIPE)`（`stream_infra.py:196,200,215,218,225`），
  从不 wait/kill，靠下一次 Popen 的清理兜底。Macast 是 terminate→wait(5)→kill
  （`macast_renderer/mpv.py:449-456`），**我们更好**。
- 暂停/恢复用 `pkill -STOP -f ffmpeg`（`bin/mkchromecast:256-271`，代码里的 TODO 自己承认不对）。
- `psutil` 进程树清理（`utils.py:127-133`）带一个自己承认先杀自己的 ParentMonitor
  （`stream_infra.py:290-296`）。唯一可取的只有"mpv 孤儿看门狗"——mpv 现在确实会活过 Macast 崩溃，
  优先级低。
- 测试：1186 行全是 mock 单元测试，唯一的端到端测试是自禁用的。Macast 的
  `scripts/verify_cast_airplay.py` + `vlc_sender_sim.py` **明显更强**。

### 2.4 评级

| 做法 | 结论 |
|---|---|
| 集成代码 | **不做**（零接收端代码：没有 protobuf、没有 TLS listener、没有 mDNS/SDPP；约 15% 是自认死的代码） |
| 集成协议 | **不做**——它靠 pychromecast *消费* Cast v2，Macast *实现* Cast v2，方向相反 |
| 借鉴具体做法 | **做**：拿 pychromecast 14 当 oracle（一个测试 + §2.2 的 7 处修复，`protocol_cast.py` 里几十行） |
| 其他 | **全部忽略**：采集/编码管线、Flask `/stream`、node/webcast、soco/Sonos、托盘、`messages.py`、参数解析 |

---

## 3. 功能对照

| 能力 | Macast | miraclecast | mkchromecast |
|---|---|---|---|
| DLNA / UPnP 接收 | ✅ | ❌ | ❌ |
| Google Cast 接收 | ✅（未认证） | ❌ | ❌（是发送端） |
| AirPlay 视频 URL 接收 | ✅ | ❌ | ❌ |
| AirPlay 音频 / 屏幕镜像 | ❌ | ❌（Miracast 是另一套） | ❌ |
| **Miracast / Wi-Fi Display** | ❌ | ✅（仅 sink） | ❌ |
| 屏幕镜像（任意协议） | ❌ | ✅（Wi-Fi Direct，Linux + root） | ✅（Wayland 截图后当流推给 CC，发送端侧） |
| 系统音频采集并投出 | ❌（不需要） | ❌ | ✅ |
| Sonos | ❌ | ❌ | ❌（代码自认 broken，从未实例化） |
| 多协议同时在线 | ✅ | ❌ | ❌ |
| macOS / Windows | ✅ | ❌（仅 Linux） | ⚠️ 仅 macOS/Linux，且是发送端 |
| 插件热插拔 | ✅ | ❌ | ❌ |
| 真实发送端回归测试 | ✅（VLC 发送端仿真，206 用例） | ⚠️ check(1) 单元测试 | ❌（集成测试 skip） |
| 硬件/环境自检脚本 | ❌ | ✅ | ❌ |
| 全量线上报文日志 | ⚠️ 部分 | ✅ | ⚠️ |

**它们有而我们没有的功能，全部落在"发送端"或"另一种协议"上，对接收端产品没有意义。**

---

## 4. 落地情况（本节在修复完成后更新过）

| 优先级 | 事项 | 位置 | 状态 |
|---|---|---|---|
| P0 | `SET_VOLUME` 回 `RECEIVER_STATUS`（回显 requestId）；把 `level`/`muted` 存成真状态 | `protocol_cast.py` `_on_receiver` / `_apply_volume` | ✅ 已修 |
| P0 | `RECEIVER_STATUS` / `MEDIA_STATUS` 的音量回真值 | 同上 + `_send_media_status` | ✅ 已修 |
| P0 | `displayName` 改成 `"Default Media Receiver"`（`DEFAULT_MEDIA_APP_NAME`） | `_send_receiver_status` | ✅ 已修 |
| P1 | `ssdp_udn` 去掉 `"uuid:"` 前缀；`capabilities.multizone_supported` 显式 `false` | `_eureka_info` | ✅ 已修 |
| P1 | 播完/出错不再谎报 `PLAYING`：从播放器的 `set_state_*` 扇出观测真实状态，结束带 `idleReason` | `set_state_*` 覆写 + `_observed_state()` | ✅ 已修（**只应答、不主动推送**，理由见下） |
| P1 | RTSP 持久分帧、头部大小写不敏感、不伪造 CSeq、真实状态码、每连接超时、accept 循环不再被单次失败打死 | `protocol_airplay.py` | ✅ 已修 |
| P2 | `scripts/cast_conformance.py`：真实 pychromecast 栈端到端探针 | 新增 | ✅ 已加（23 项） |
| P2 | `scripts/selfcheck.py`：环境自检（依赖/端口占用者身份/网卡/组播/mpv/代理） | 新增 | ✅ 已加 |
| P3 | 8443 `eureka_info` | `_start_setup_server` | ❌ 未做，**且原定理由已被证伪**（见下） |
| P3 | 未识别 type 回 `INVALID_REQUEST` | `_on_message` / `_on_receiver` / `_on_media` | ⚠️ 只做了一半：**不回复**（回复有风险），但已加 DEBUG 日志记录未知 namespace/type，供以后判断到底有没有客户端真的需要它 |
| P3 | 端口回退时不要被当成 `CAST_TYPE_GROUP` | `_start_tls_server` | ❌ 未做（只在 8009 被占时出现，而那时更该先解决多实例） |

**关于 8443，更正一条我自己写错的理由。** 我原先写的是「pychromecast 会回退到 8008，
代价只是一次失败尝试」。查证后：**有 8008 回退的是 `get_device_info`（`dial.py:213-230`），
而 mDNS 路径用的 `get_cast_type`（`dial.py:154-161`）只试 `https://host:8443`，没有回退**。
所以现状是每次 mDNS 发现都留下一条 warning，并且 `get_cast_type` 里读 `device_info` 的那段
（`display_supported` / `manufacturer`）**永远不会执行**。功能上不算坏（异常被吞掉后
`cast_type` 仍保持正确的 `CAST_TYPE_CHROMECAST`，`dial.py:185`），但这比我说的重要。

顺带一个环境事实：本机 8443 被 **OrbStack** 占着，所以 pychromecast 收到的是
`HTTP Error 404`（而不是连接失败）——这解释了每次真机跑都打出的
`Failed to determine cast type for host <unknown> (HTTP Error 404)`。
这也意味着**本机无法验证 8443 的正常路径**，只能验证"端口被占时优雅跳过"。

**「不主动推送」是刻意的**：状态广播看着更"正确"，但这个仓库有 VLC 状态机被多余状态消息
打死的血泪记录（AGENTS.md §4.1）。发送端本来就会 `GET_STATUS` 轮询，所以把真相放在应答里
既能修好问题，又不引入任何非请求消息。

**明确不做**：集成任何一个代码库；实现 Miracast/WFD（macOS 无 Wi-Fi Direct API）；
为独占网卡而杀网络栈；抄 mkchromecast 的子进程/暂停实现。

### 4.1 修复的验证方式

| 证据 | 手段 |
|---|---|
| 405/405 | `scripts/verify_cast_airplay.py`（新增 Part 2b 的 11 条 RTSP 用例 + Part 18 的 19 条 Cast 一致性用例） |
| 新用例**确实能抓旧代码** | 把 `protocol_cast.py` / `protocol_airplay.py` 分别 `git stash` 回旧版重跑：Part 18 当场 6 红，Part 2 当场 11 红（含"小写 `cseq: 42` 被回成伪造的 `CSeq: 1`"、"合并请求把客户端读到超时"） |
| 真机 23/23 | `scripts/cast_conformance.py` 打在本轮改过的源码上：`set_volume` 0.00s 应答、音量回读 0.35、`displayName='Default Media Receiver'`、`ssdp_udn` 是裸 UUID、**`BUFFERING → PLAYING → IDLE` 且 `idleReason='FINISHED'`** |
| 探针抓到过一个新 bug | 第一次真机跑就把**旧文件 end-file 追尾**的 race 暴露出来（新视频被错标 `CANCELLED`），据此加了 `_note_stopped` 的门控和对应用例——这是任何打桩测试都抓不到的 |

---

## 5. 复现方式（本次评估怎么得出的）

```shell
git clone --depth 1 https://github.com/albfan/miraclecast.git /tmp/mc
git clone --depth 1 https://github.com/muammar/mkchromecast.git /tmp/mkc

# 协议重叠为零
grep -rniE "ssdp|upnp|dlna|mdns|_googlecast|_airplay|raop|avahi" /tmp/mc/src /tmp/mc/res   # 无命中
grep -rniE "SSDP|M-SEARCH|1900|239\.255|UPnP|SOAP" /tmp/mkc                                # 无命中

# 发送端要求的出处：用 Macast 自己的 venv 里的 pychromecast 14.0.10 核对
P=.venv/lib/python3.12/site-packages/pychromecast
grep -n "REQUEST_TIMEOUT" $P/const.py                                   # 10.0
sed -n 244,272p $P/controllers/receiver.py                              # set_volume 等 WaitResponse
sed -n 232,268p $P/dial.py                                              # multizone 缺省 True、UDN UUID()
sed -n 230,250p $P/discovery.py                                         # port != 8009 -> CAST_TYPE_GROUP
```
