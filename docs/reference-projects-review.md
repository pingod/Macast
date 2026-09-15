# 参考项目评估：miraclecast / mkchromecast / AirConnect

评估日期：2026-09（AirConnect 一节补于 2026-09-15）
对照 commit：miraclecast `0b7f1f1`、mkchromecast `aad239c`、AirConnect `1.11.3`（release，2026-09-06）、
Macast `8e02bb5` / v0.7.11
发送端基准库：**pychromecast 14.0.10**（就装在 Macast 自己的 `.venv` 里，本文件的库侧引用都取自它）

**问题**：这几个项目有没有必要集成？有没有可参考的？它们有我们没有的功能吗？它们的实现方式更正确吗？

**一句话结论**：

- **miraclecast**：同类，但**不是同一个协议**（Wi-Fi Display / Miracast），RTSP 角色相反、
  Linux-only、需要 root。**不要集成**；它的 RTSP 报文语义和自检脚本值得抄。
- **mkchromecast**：**方向相反**（它是发送端）。**不要集成**；真正的价值是——它走的是真实发送端栈
  （pychromecast），据此对我们做一致性核对，**已经定位到 7 处 Macast 接收端的真实缺陷**。
- **AirConnect**：**一半同向、一半反向**（收 AirPlay 音频 → 发 UPnP/Cast）。**不要集成**；
  但它是第一个提供了"Macast 缺的那半边"的现成实现，其中 **Cast v2 发送端**和
  **"长度未知的实时流怎么喂给挑剔的 HTTP 客户端"**两处可直接对照。注意它**没有本地播放后端**，
  所以那半边缺口对 Macast 而言仍然由 `plugins/raop.py` 监督 shairport-sync 覆盖。

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

## 3. AirConnect（philippe44/AirConnect，4.2k★，C）

### 3.1 它到底是什么

**AirPlay → UPnP/Sonos/Chromecast 的桥。** 两个可执行文件，同一套内核、两个出口：

- `airupnp-<os>-<cpu>`：出口是 UPnP/Sonos（用 SOAP 控制）
- `aircast-<os>-<cpu>`：出口是 Chromecast（用 Cast v2 控制）

它自己实现 RAOP/AirPlay **音频接收端**（`common/libraop`），收到的 ALAC 音频解码后
（可选重编码成 mp3 / aac / flac / wav / pcm），通过**它自己的 HTTP 服务**以"无限流"的形式
喂给 mDNS/SSDP 发现的**远端**播放器。所以它是「AirPlay 接收端 + UPnP/Cast 发送端」的合体，
而 Macast 是「UPnP/Cast/AirPlay 接收端 + 本地播放」——**它有一半和我们对齐，另一半正好反向。**

两条 README 原文决定了很多结论：

- "*This is an **audio-only** application. Do not expect to play a video on your device and have
  the audio from UPnP/Sonos or ChromeCast synchronized. It does not, cannot and will not work*"
  —— 它不碰视频，也不可能做音视频同步（原因是 RTP 推流 → HTTP 拉流这一跳无法回传播放时刻）。
- "*looking like a **wire** to iPhone and looking like a **file** to the UPnP/CC*"
  —— 这句是它的核心：对上游装成"一根有延迟的模拟线"，对下游装成"一个可随机读的文件"。
  Macast 的形状反过来：上游是 URL，下游交给 mpv。

构件（全部是 C）：

| 位置 | 职责 |
|---|---|
| `aircast/src/aircast.c`（32KB） | Cast 版本的 main：配置、设备生命周期、AirPlay 事件分发 |
| `aircast/src/castcore.c`（21KB） | **Cast v2 会话**：TLS 连接、source/destination id、LAUNCH、LOAD、心跳 |
| `aircast/src/cast_util.c`（11KB）+ `cast_parse.c`（3KB） | Cast 消息构造与解析（nanopb + `CastMessage.proto`） |
| `aircast/src/config_cast.c`（9KB） | `config.xml` 解析 |
| `airupnp/src/*` | UPnP/Sonos 版本，复用同一套 libraop，出口换成 SOAP |
| `common/libraop` | **RAOP / AirPlay 音频接收**（子模块 philippe44/libraop） |
| `common/libmdns`、`common/libpupnp`、`common/libcodecs`、`common/libopenssl`、`common/dmap-parser`、`common/crosstools`、`common/libpthreads4w`、`aircast/nanopb`、`aircast/libjansson` | 其余 9 个子模块 |

自己的胶水其实很薄（`aircast/src/*.c` 合计约 76 KB），**真正的协议都在子模块里**——
这一点决定了"集成"到底意味着什么（见下）。

运行形态：需要 UDP 5353（mDNS）可达；每个设备长期占 1 个 RTSP 端口，播放时再加 1 个 HTTP + 3 个
RTP；`-z` 才能自守护（**README 明确警告：别用 `&` 后台化而不加 `-Z`，否则吃满 CPU**）。

### 3.2 为什么不能集成

- **方向卡在它的出口上**：Macast 要的是"把收到的媒体在本机放出来"，AirConnect 的出口是
  "把音频以 HTTP 流推给**别的**播放器"，它**没有任何本地播放后端**。接进 Macast 只会得到
  一条"AirPlay → AirConnect → 远端播放器"的链路，和 Macast 自己造的接收端不搭。
- **本地那半已经有更合适的解**：`plugins/raop.py` 监督 shairport-sync，而 shairport-sync
  的强项恰好是 AirConnect 完全不做的部分——**本地播放**、系统音频设备与 mixer 选择、
  自带 mDNS 广播。对"iPhone 音频在这台 Mac 上响"这个需求，shairport-sync 是严格更顺手的工具。
- **构建**：`build.sh` 是**交叉编译**流程，`common/crosstools` 是它自己维护的工具链仓库，
  10 个子模块要逐个 init。Macast 是 py2app/PyInstaller 打的 Python 应用，要塞一个 C 二进制
  进去等于新开一条构建链——而 AGENTS.md §4.3/§4.4 已经证明现有打包链对依赖漂移极度敏感
  （少一个 `zeroconf` 就让全部产物起不来）。收益是"多一个可选的 AirPlay 音频实现"，
  代价是"多一条必须每次发版都验证的构建链"，不划算。
- **许可证**：AirConnect 自身代码是 MIT，但它的 LICENSE 原文写明
  "*This program uses 3rd party software that is licensed by their author under their own
  conditions.*"；`common/libraop` 在 GitHub API 里是 **NOASSERTION**（无标准 SPDX），
  `libcodecs` 是 MIT。**要捆二进制就必须逐个子模块审计**（openssl、nanopb、libpupnp、
  libcodecs、libpthreads4w…），这不是一次性工作。
  ⚠️ 顺带一个判读坑：**AirConnect 自己的 `spdx_id` 也是 `NOASSERTION`**——因为它的 LICENSE
  不是标准 MIT 模板，而不是因为它不是 MIT。别只看 API 的 `spdx_id` 就下结论，要读正文。
- **能力边界**：AGENTS.md §9 已经把"AirPlay 音频（RAOP）"定为"由插件监督 shairport-sync 解决"，
  把原生 RAOP 定为不做。集成 AirConnect 等于用第三方的 C 实现去替代这个已经成立的方案，
  却没有解决它唯一的短板（没有本地播放）。

### 3.3 值得借鉴（三处，都能直接落到现有代码上）

1. **Cast v2 发送端实现** —— `aircast/src/castcore.c` + `cast_util.c` + `CastMessage.proto`。
   Macast 的 `plugins/cast_bridge.py` 也是**发送端**（把 DLNA 来的 URL 中继到 Chromecast），
   正好同类。这是一份经过 251 个 fork、多年真机打磨的对照实现，可用来核对：投屏序列
   （我们记录的是 deviceauth CHALLENGE → CONNECT receiver-0 → LAUNCH → CONNECT transportId
   → LOAD）、`transportId` 是不是真的取自 LAUNCH 的回复、心跳与重连时机，以及各代
   Chromecast 固件的差异。**建议动作：照着它把 `cast_bridge.py` 的序列读一遍**，
   比读 pychromecast（那是接收端视角）更贴我们的用法。
2. **"长度未知的实时源"的 HTTP 供给** —— README 的
   *HTTP content-length and transfer modes* 一节。这与 AGENTS.md §6 的
   "mpv 需要 Range 否则 loading failed"、以及 AGENTS.md §4.9 里"探针的测试流必须支持 Range"
   是**同一族问题**：源是实时的、长度未知，而 HTTP 客户端还想要 content-length / Range。
   AirConnect 给了三种模式：不发 content-length（默认，`http_length=-1`）／chunked（`-3`）／
   **假 content-length `2^31-1`**（`0`）；并保留一份"最近发过的字节"以便客户端重开连接时重发。
   它还记录了一个很实用的观察：*当客户端请求 Range 而服务器回 200，就意味着源不支持 Range，
   但有些客户端不认*。这一节值得整段读，尤其如果以后要做"边下边播"类的流服务
   （`plugins/macast_ytdlp.py` 的流模式就在这个方向）。
3. **RAOP / AirPlay 音频协议本体** —— `common/libraop`。C 里少有的生产级 RAOP 实现：
   RTP 帧编号与丢包重传（"收到 1,2,3,6 时要先补请求 4,5，不能直接发 6"）、AES、
   ALAC 解码、FLUSH 语义、时钟漂移补偿（README 的 *Latency parameters explained*
   把这个讲得比多数协议文档都清楚）。
   **如果哪天要重新评估"原生 RAOP"，这是比读 shairport-sync 更小的起点**；
   按 AGENTS.md §9 的现状，只作存档。

### 3.4 一个不靠集成的组合：AirPlay → aircast → Macast 自己（**猜想，未实测**）

`aircast` 是靠 mDNS `_googlecast._tcp` 发现 Chromecast 的，而 **Macast 自己就在广播这个服务**。
所以理论上可以直接串起来：

```
iPhone (AirPlay) ──RAOP──▶ aircast ──HTTP 音频流 + Cast v2──▶ Macast 自己的 Cast 接收端 ──▶ mpv
```

一行 RAOP 代码都不用写，也可能让 iPhone 的音频在这台 Mac 上响。

**但我不推荐把它当方案，且以下全部未经验证：**

- `aircast` 是否愿意把**本机/回环**设备当作投放目标，没有验证过（这是整个链路成立的前提）。
- 会多一跳；README 自述 AirPlay 侧本身就有 1–2 s 基础延迟，再加 HTTP 缓冲。
- `aircast` 会把 Macast 当成**远端 Chromecast**：暂停/音量/停止的语义走 Cast 而不是 AirPlay，
  与 3.1 描述的"对上游装成一根线"不一致的地方会在这里露出来。
- 想试的话用 release 里那个 zip 中的 `aircast-macos-arm64` 预编译件
  （最新 release `1.11.3` 只挂了一个资产 `AirConnect-1.11.3.zip`，各平台二进制在里面；
  **不要**从源码构建），并且**不要**把它写进 repo 或打包配置。

### 3.5 评级

| 做法 | 结论 |
|---|---|
| 集成代码 | **不做**（C + 10 子模块 + 自带交叉编译链；出口方向与产品需求相反；**没有本地播放**；许可证要逐个审计） |
| 集成协议 | **不做**——它做"AirPlay 收 → UPnP/Cast 发"，Macast 做"UPnP/Cast/AirPlay 收 → 本机播"，只在一半上重叠且方向错开 |
| 借鉴具体做法 | **做**：§3.3 的三条。第 1 条对 `plugins/cast_bridge.py` 有直接对照价值，第 2 条与 AGENTS.md §6/§4.9 的 Range 经验同源 |
| 当依赖/子模块引入 | **不做**：会同时放大 AGENTS.md §4.3/§4.4 的打包脆弱性，并引入一个 NOASSERTION 子模块 |
| 那个串联组合 | **可选实验**，不进产品（§3.4，未实测） |

---

## 4. 功能对照

| 能力 | Macast | miraclecast | mkchromecast | AirConnect |
|---|---|---|---|---|
| DLNA / UPnP 接收 | ✅ | ❌ | ❌ | ❌ |
| Google Cast 接收 | ✅（未认证） | ❌ | ❌（是发送端） | ❌（是发送端） |
| AirPlay 视频 URL 接收 | ✅ | ❌ | ❌ | ❌（audio-only） |
| AirPlay 音频（RAOP）接收 | ⚠️ 插件监督 shairport-sync | ❌ | ❌ | ✅（自带实现，是它的核心能力） |
| **本地播放** | ✅（mpv） | ❌（把 RTP 端口交给播放器） | ❌ | ❌（**只把 HTTP 流推给远端播放器**） |
| **Miracast / Wi-Fi Display** | ❌ | ✅（仅 sink） | ❌ | ❌ |
| 屏幕镜像（任意协议） | ❌ | ✅（Wi-Fi Direct，Linux + root） | ✅（Wayland 截图后当流推给 CC，发送端侧） | ❌ |
| 系统音频采集并投出 | ❌（不需要） | ❌ | ✅ | ❌（不采集，只转发收到的 AirPlay 音频） |
| 转发到 UPnP / Chromecast 播放器 | ❌ | ❌ | ✅（进 CC） | ✅（这就是它存在的意义） |
| Sonos | ❌ | ❌ | ❌（代码自认 broken，从未实例化） | ✅（`airupnp` 的主要目标） |
| 多协议同时在线 | ✅ | ❌ | ❌ | ❌（一个进程一个出口） |
| macOS / Windows | ✅ | ❌（仅 Linux） | ⚠️ 仅 macOS/Linux，且是发送端 | ✅（含 `macos-arm64` 预编译件） |
| 插件热插拔 | ✅ | ❌ | ❌ | ❌ |
| 真实发送端回归测试 | ✅（VLC 发送端仿真 + pychromecast 一致性探针，套件共 411 用例） | ⚠️ check(1) 单元测试 | ❌（集成测试 skip） | ❌ |
| 硬件/环境自检脚本 | ✅（`scripts/selfcheck.py`） | ✅ | ❌ | ❌ |
| 全量线上报文日志 | ⚠️ 部分 | ✅ | ⚠️ | ✅ |

**除了 AirConnect 的「AirPlay 音频接收」这一半，它们有而我们没有的功能都落在"发送端"或"另一种协议"上，
对接收端产品没有意义。** AirConnect 那一半虽然同向，但它没有本地播放（只有"推给远端播放器"），
所以对 Macast 的直接价值仍然只是参考，不是集成——本地那半继续由 `plugins/raop.py` 承担。

---

## 5. 落地情况（本节在修复完成后更新过）

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

**明确不做**：集成任何一个代码库（**含 AirConnect**——C + 10 个子模块 + 自带交叉编译链，
且它没有本地播放，见 §3）；实现 Miracast/WFD（macOS 无 Wi-Fi Direct API）；
为独占网卡而杀网络栈；抄 mkchromecast 的子进程/暂停实现。

### 5.1 修复的验证方式

| 证据 | 手段 |
|---|---|
| 411/411 | `scripts/verify_cast_airplay.py`（Part 2b 的 11 条 RTSP 用例 + Part 18 的 19 条 Cast 一致性用例 + Part 19 的 6 条 8443 用例） |
| 新用例**确实能抓旧代码** | 把 `protocol_cast.py` / `protocol_airplay.py` 分别 `git stash` 回旧版重跑：Part 18 当场 6 红，Part 2 当场 11 红（含"小写 `cseq: 42` 被回成伪造的 `CSeq: 1`"、"合并请求把客户端读到超时"） |
| 真机 23/23 | `scripts/cast_conformance.py` 打在本轮改过的源码上：`set_volume` 0.00s 应答、音量回读 0.35、`displayName='Default Media Receiver'`、`ssdp_udn` 是裸 UUID、**`BUFFERING → PLAYING → IDLE` 且 `idleReason='FINISHED'`** |
| 探针抓到过一个新 bug | 第一次真机跑就把**旧文件 end-file 追尾**的 race 暴露出来（新视频被错标 `CANCELLED`），据此加了 `_note_stopped` 的门控和对应用例——这是任何打桩测试都抓不到的 |

---

## 6. 复现方式（本次评估怎么得出的）

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

AirConnect 那一节的取证（**没有 clone**——它的 master 里带着一个 95 MB 的预编译 zip，
`git clone` 不划算，全部走 GitHub API）：

```shell
# 方向、规模、许可证
curl -s https://api.github.com/repos/philippe44/AirConnect | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["stargazers_count"], d["language"], d["license"]["spdx_id"])'

# 自己写的胶水有多薄：aircast/src 下所有 .c 之和（76248 字节，约 76 KB）
curl -s https://api.github.com/repos/philippe44/AirConnect/contents/aircast/src | \
  python3 -c 'import json,sys; print(sum(f["size"] for f in json.load(sys.stdin) if f["name"].endswith(".c")))'

# “真正的协议都在子模块里”：10 个 submodule
# （raw.githubusercontent 从这边经常拉不动，见 AGENTS.md §4.6，所以走 API contents）
curl -s https://api.github.com/repos/philippe44/AirConnect/contents/.gitmodules | \
  python3 -c 'import json,sys,base64; c=base64.b64decode(json.load(sys.stdin)["content"]).decode(); print(c.count("[submodule"))'

# 许可证：自身正文写 MIT（但 API 的 spdx_id 是 NOASSERTION——模板不标准，别误判）；
# libraop 同样是 NOASSERTION，libcodecs 是 MIT
# （LICENSE 走 raw；拉不动就换成 .../contents/LICENSE 的 API 端点）
curl -s https://raw.githubusercontent.com/philippe44/AirConnect/master/LICENSE
curl -s https://api.github.com/repos/philippe44/libraop   | grep -o '"spdx_id":"[^"]*"'
curl -s https://api.github.com/repos/philippe44/libcodecs | grep -o '"spdx_id":"[^"]*"'

# 预编译件：release 只挂一个 zip（AirConnect-<ver>.zip），各平台二进制在里面；
# 不要从 master 构建
curl -s https://api.github.com/repos/philippe44/AirConnect/releases/latest | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["tag_name"], [(a["name"], a["size"]) for a in d["assets"]])'
```

§3.4 那个串联组合（AirPlay → aircast → Macast 自己）**没有取证**，是读构件得出的结构推断，
在文中已明确标为"未实测"。要验证它，第一步是确认 `aircast` 愿不愿意把本机地址当作投放目标。
