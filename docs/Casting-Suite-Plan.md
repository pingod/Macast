# Casting Suite 规划：把 JustStream 的场景全部开源化

制定日期：2026-09-21 ｜ 状态：**Phase 0（规划）** ｜ 负责：无常 + AI agent
上游文档：产品说明 `README_ZH.md`、坑位清单 `AGENTS.md`、插件索引 `plugins/README.md`、
参考项目评估 `docs/reference-projects-review.md`（该文覆盖**接收端**视角，本文覆盖**发送端**视角）

---

## 0. 这份文档要解决什么

JustStream（macOS 菜单栏投屏发送端，现属 Electronic Team/Eltima，v2.14 发布于 2025-10-06）
是**发送端**，而 Macast 是**接收端**。本文的目标不是移植某个项目，而是把
「Mac 屏幕/声音/本地文件要上电视」这一整类客户需求，用 Macast 的插件机制**全部补齐**，
让 Macast 同时具备收发两半能力。

每个参考项目的可取之处都已经过代码级精读（证据见 §2），本文把它们落成
**可分阶段交付、每阶段可独立测试并推送** 的插件清单（§4）。

### 硬约束（来自 AGENTS.md，违反即返工）

1. **在线插件（`plugins/*.py`）只能是单个 .py 文件**，依赖仅限：标准库 + Macast 已带的库
   （`cherrypy / requests / zeroconf / lxml / pillow / netifaces / appdirs / pystray / pyperclip / rumps`）
   + 机器上已有的命令行程序（`ffmpeg / ffprobe / caffeinate / uxplay / shairport-sync`）。
   要 pip 库就必须走内置插件路线（`macast/plugins/`）+ §4.4 的三处打包配置同步。
2. **渲染器互斥**：`ScreenMirror`、`CastLocalFile`、`CastBridge` 等都是 renderer 类插件，
   同一时刻只能启用一个（菜单栏切换）。协议类插件（`raop.py`、未来的 `airplay_mirror.py`）可与任意渲染器共存。
   —— 这条直接决定了 §3 的架构选择。
3. **`Setting.get(key, default)` 有副作用**，判存在用 `Setting.has`，删除用 `Setting.unset`。
4. **不新增顶层 import 的第三方库**（§4.3 的漂移教训）。新增任何外部依赖必须同步
   `requirements/*.txt`、CI 4 个 job、`setup_py2app.py`、PyInstaller 参数。
5. **每加功能同时补 `scripts/verify_cast_airplay.py` 用例**，改完跑 pyflakes + 全量回归。
6. **不为了验证改用户真实配置**（临时 `SETTING_DIR`）。
7. 插件安装 URL 固定 commit SHA；`plugins/info.json` 条目与 `.py` 头部清单逐字对齐（Part 5c 校验）。

### 环境与推送路线（本轮实测）

- `github.com` 走 HTTPS 被墙：`git ls-remote origin` 在 `http.proxy=127.0.0.1:9565` 下
  `SSL_ERROR_SYSCALL`，但 `api.github.com` / `codeload.github.com` 直连 200，且
  **SSH 可用**（`ssh -T git@github.com` → `Hi pingod!`）。
- 因此**每阶段推送走 SSH**，不改用户的 remote：
  `git push git@github.com:pingod/Macast.git main`
- 源码研究副本在 `/tmp/caststudy/`（不落仓库、不打包）。

---

## 1. 需求矩阵（JustStream 实际提供的能力）

来源：App Store 页 + 厂商页 + Macworld 评测 + 2023-2026 近期评论（iTunes RSS）。
标注 ⚠️ = 未能从公开来源证实，不做承诺。

| # | 能力 | JustStream 现状 | 我们的覆盖计划 |
|---|---|---|---|
| R1 | 镜像到 Chromecast / Google TV | ✅ Cast 协议 | **已交付**：兼容的 LOAD/mpegts 通道（既有）+ **P3 的 Cast Streaming 低延迟通道**（`caststream`，设备拒绝即自动回落）；低延迟那条**未经真电视验证** |
| R2 | 镜像到 Apple TV / AirPlay 2 电视 | ✅ AirPlay 镜像 | ❌ 需要 FairPlay；**不做发送端**，接收端由 `airplay_mirror.py`(P5) 补 |
| R3 | 镜像到 DLNA/UPnP 智能电视（2012-2018 老电视） | ✅ | **已交付（`screen_mirror.py` v0.5，P2）**：五档兼容档位 + 自动回退；真机矩阵未验证 |
| R4 | 镜像到 Roku / Fire TV | 厂商标称支持 ⚠️ 机制未证实 | P1「任意浏览器」路线覆盖 Fire TV/Roku 浏览器可用场景 |
| R5 | 多显示器选择 | ✅ 选屏 | **已交付（P1）**：`采集屏幕` 子菜单，插拔后回落默认屏 |
| R6 | 光标显示/隐藏、鼠标高亮、缩放适配 | ✅ | **已交付（P1）**：`显示鼠标指针`（`-capture_cursor`）；高亮不做；「缩放适配」= 低延迟通道的信箱化 |
| R7 | 画质 Auto/720p/1080p、码率、编码器 | 有档位（4K 仅文件模式）⚠️ 具体 UI 未证实 | **已交付（P1）**：四档画质 + macOS VideoToolbox 开关；4K 与区域捕获不做 |
| R8 | 系统声音，且「不想再装驱动」 | 需要装音频驱动 + 重启（评论吐槽点） | 我们已有 BlackHole 一键辅助 + 多输出聚合（**优于它**）；P0 复核 pyobjc 依赖 |
| R9 | 投本地文件（AVI/MKV/MOV/MP4/MP3…）+ 边转边投 | ✅ 且带播放列表 | **已交付（`cast_local_file.py` v0.1，P4）**：ffprobe 逐文件判直通/转码 + 播放列表自动连播；真机未验证 |
| R10 | 字幕/音轨选择、音画同步延迟、字体样式 | ✅（2026 年评论：字幕坏） | **已交付（P4）**：选轨 + `AudioDelay` + 字幕三条路（Cast 侧 WebVTT / 转码侧 libass 烧制 / DLNA 无）；样式不做 |
| R11 | 暂停/继续/拖动进度 | ✅ | **已交付（P4）**：直通路径由设备对真文件操作；转码路径 seek = 重启编码器。DLNA 音量故意不做（属设备侧 RenderingControl） |
| R12 | 菜单栏启停、防休眠 | ✅（2.14 加了 keep-awake） | **已交付（P1）**：`caffeinate -dimsu` 持有断言，停止镜像即释放 |
| R13 | 延迟 | 评论抱怨 ~5-10 s | **P3 已交付代码路径**（`caststream`，目标 <500 ms）；**数字没人量过**，量它 = `scripts/cast_streaming_probe.py` + 秒表 |
| R14 | 麦克风直通、HDR、窗口级捕获、Miracast | 未文档化/无 | 不做（avfoundation 无窗口源，见 §2.4） |
| R15 | 免费 | 20 分钟限制 + $9.99/年 或 $19.99 PRO | GPL-3.0，全功能无限制 |

**判定标准**：R1/R3/R5/R6/R7/R8/R9/R11/R12/R13 全部落地，才算「替代 JustStream」。

---

## 2. 参考项目精读结论（可取之处 + 许可判定）

许可总体判定：Macast 是 **GPL-3.0**（`LICENSE`）。GPL-3 项目的**思路**可直接借鉴；
非商业许可项目**只读思路、绝不搬代码**；本项目自己写的插件继续用 GPL-3.0。

| 项目 | 许可 | 活跃度 | 一句话定位 |
|---|---|---|---|
| [mkchromecast](https://github.com/muammar/mkchromecast) | 见 `LICENSE`（NOASSERTION） | master 2026-09-14 仍在推，release 停在 0.3.8.1(2017) | 发送端，Python，**但 macOS 屏幕采集根本没有实现** |
| [MirrorCast for macOS](https://github.com/greensteindesign/mirrorcast-macos) | **PolyForm Noncommercial** | 4★，2026-07 | Mac 屏幕+声音 → 任意 DLNA 电视，**老电视兼容性的活字典** |
| [omacast](https://github.com/Aphrodine-wq/omacast) | MIT | 2026-09 新仓库，README 自述 m0 清单全未勾 | **Cast Streaming 镜像协议**的完整可读实现（Rust，5114 行非测试码） |
| [UxPlay](https://github.com/antimof/UxPlay) | GPL-3.0 | 2.1k★，2026-04 | AirPlay **镜像接收端**（含 FairPlay 之外的解密与配对） |
| [Castify](https://github.com/sh1zen/Castify) | LICENSE=Apache-2.0，`Cargo.toml:9`/README 写 GPL-3（**自相矛盾**） | 1★，2026-07 | WebRTC 双端投屏，**自适应画质/统计/强制 IDR** 的工程范式 |
| [Mac-Screencast](https://github.com/hamiz-ahmed/Mac-Screencast) | MIT | 2026-08 | Mac 屏幕+声音 → **任意浏览器**（fMP4 + MSE + 渐进式兜底） |

### 2.1 MirrorCast：DLNA 老电视到底怎么才能吃下「无限长的直播流」

这是 P2 的全部难度所在，逐条抄它的**事实**（不抄代码）：

- 送过去的是**一个假装成有限文件的无限 MPEG-PS 流**：`SetAVTransportURI` +
  `Play`，DIDL `<res protocolInfo="http-get:*:video/mpeg:DLNA.ORG_PN=MPEG_PS_PAL;DLNA.ORG_OP=01;
  DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01500000000000000000000000000000" size="1900000000" duration="0:36:00">`。
- **`Content-Length` 必须 < 2³¹**：给 3.9 GB 时某些固件按有符号 32 位算，产生
  `Range: bytes=0-18446744072566584319` → 「文件不支持」。
- 每个响应都要带 `Accept-Ranges: bytes` + `transferMode.dlna.org: Streaming` +
  `contentFeatures.dlna.org`，支持 HEAD，`Connection: close`。
- **有界探测（≤4 MiB）必须精确回 n 个字节**，不足就用 MPEG-PS padding 包
  （`0x00 0x00 0x01 0xBE`）补齐 —— 电视在「试读文件头」，回多了就废。
- **按字节绝对偏移重连**：64 KiB 环形缓冲（约 48 MiB）+ 绝对偏移索引，新会话把电视的
  文件偏移 0 映射到 hub 偏移；要未来的字节就**阻塞等编码器产出**。直接给「最新数据」
  会造成快进 + 爆音。
- 编码：`-c:v mpeg2video -b:v 4500k -minrate=-maxrate`（CBR 喂电视的缓冲模型）
  `-g 15 -vf scale=720:576,setdar=16/9 -c:a ac3 -b:a 192k -f vob`。
- 兼容换档：5 个固定 profile（`video/mpeg` / `video/vnd.dlna.mpeg-tts` / `video/x-matroska`），
  `GetTransportInfo` 每 5 s 看门狗，`STOPPED`/`NO_MEDIA_PRESENT` 就重推，8 次失败提示换 profile。
- 成功判据：`TRANSITIONING` 卡住 = 根本没解码；`PLAYING` + `RelTime` 走动 = 真播了。
- **它选推流地址的方式**：对电视 IP 做 UDP `connect()` 挑网卡 —— 与 AGENTS.md §4.1
  「只广播承载默认路由的网卡」同源，我们有 `Setting.get_advertisable_ip()` 可用。
- 代价：**约 20-25 s 延迟**（预填 20 MiB 缓冲防电视卡顿），默认 720×576 SD（文字发糊）。
  这是「老电视能播」的物理代价，不是我们的实现问题。

**P2 落地的偏差**（2026-09-21）：环形缓冲取 **48 MiB**（上面那句「64 KiB（约 48 MiB）」按
4.5 Mbps 只有 0.1 秒余量，任何一次重连都会掉出环外）；`size` 不再固定 1.9e9，而是**按档位码率
反算出自洽的整数**（`Duration` 与它同源，否则电视按 `<res size>`/码率估的进度会和我们报的时长
打架）；档位从 3 个扩到 **5 个**（PAL 与 NTSC 分开：帧率 / `-g` / `protocolInfo` 三样都不同，
合并会让另一半电视在第一秒就判「不支持」）；预填之后的等待是**阻塞式按偏移读**，编码器死了要
立刻醒（`_ByteLog.close()` 唤醒所有 parked reader，teardown 才不会挂在 `server.shutdown()`）。
**未验证项**（诚实记录）：真实老电视兼容矩阵 —— 打桩用例能证明「我们发出去的字节和 SOAP 连自家
接收端都认账」，**不能**证明某台 2014 年的电视认账（AGENTS.md §4.9 同一族陷阱）。

### 2.2 omacast：Chromecast 真正的低延迟镜像走的是 Cast Streaming，不是 LOAD

**协议事实全部代码可查（但作者自己没对真机验收过，README「First contact with real hardware 🚧」）**：

- 控制面仍走我们已有的 **TLS :8009**，但**没有 LOAD**：
  `CONNECT sender-0→receiver-0` → GET_STATUS → **先停掉残留的镜像 app**（残留实例会让后续
  OFFER 失败，报 `Invalid or missing codec on first OFFER`）→
  `LAUNCH {"appId":"0F5096E8"}`（音频专用 `85CDB22F`，媒体 `CC1AD845`）→ 等 `transportId`
  → 300 ms 稳定延时 → CONNECT transportId → 在 `urn:x-cast:com.google.cast.webrtc`
  命名空间发 **OFFER** → 收 ANSWER。
- 媒体面**完全离开 TLS**：ANSWER 给 `udpPort`/`sendIndexes`/`ssrcs`，之后一个**不 connect 的
  UDP socket** 复用视频+音频+RTCP（connect 会丢 RTCP，因为它从别的源端口回来）。
- 包格式：12 字节标准 RTP 头 + **7 字节 Cast 头**（bit7=关键帧，bit6=有 referenced-id，
  低 4 位=扩展数；frame-id 低 8 位；packet-id u16；max-packet-id u16；referenced-frame-id 低 8 位）
  = 19 字节，payload 是**整帧 AES-128-CTR 加密**的 **Annex-B 访问单元原样字节**（含起始码，
  SPS/PPS 每个 IDR 都带 —— 镜像不接受带外 codec 配置）。
  Nonce = 全零块 + 帧号 BE 放在第 8..12 字节，再 XOR `ivMask`。
- OFFER 里视频 `codecName=h264` PT 96 `timeBase=1/90000`；音频 `opus` PT 127 `1/48000` 双声道，
  **opus 码率超过 192 kbps 真机会拒收**。`targetDelay=200ms`。
- **没有 SR 就不出画**：28 字节 RTCP PT 200，NTP↔RTP 时间戳映射，每 500 ms 一次，
  **且第一帧之后立刻发一次**。
- 拥塞与丢包：在途 12 帧上限 → 丢非关键帧直到下个关键帧；收到 PLI 同理；
  **kickstart**：空闲时每 250 ms 重发「最新未 ACK 帧的最后一个包」；checkpoint ACK 排空缓冲。
  编码器侧 `tune=zerolatency`、`keyint=fps`（1 s GOP）、`bf=0`、`sc_threshold=0`。
- 会话存活：Cast v2 PING/PONG 5 s，20 s 无 PONG 判丢；teardown 是
  「杀编码器 → EOF → feeder 退出 → drain → STOP → CLOSE transport → CLOSE receiver-0」，
  并且**发送任务有监工**（「编码器活着、发送任务死了 = 电视永远静止画面」）。
- 发现：`_googlecast._tcp`，**优先 IPv4 A 记录**（部分固件 TLS 只监听 v4）；
  它不按 `ca` 位过滤，而是**用 LAUNCH_ERROR 当能力探测**。

**P3 落地的偏差**（2026-09-21）：

- **只做视频**（`video_source` 一条流，ANSWER 里 `sendIndexes` 必须含 0 才继续）。opus 音频
  是二期：参考实现报的 192 kbps 上限、双声道、PT 127 都记在上面，代码没写。
- **纯 Python AES-128-CTR**（单文件插件不能加依赖），实测约 1.3 MB/s ⇒ 码率天花板
  `CAST_STREAM_MAX_BITRATE = 4.5 Mbps`。这是**加密速度**的上限，不是网络的；菜单会直说。
- 编码器形状与参考实现差两处，且都是**实测出来的**：
  ① `-tune zerolatency` 已含 `bframes=0` 与 lookahead=0，所以不必再写 `bf=0`；
  ② x264 参数名是 `keyint` / `min_keyint` / `scenecut`（写 `i-frame-min`、`sc_threshold`
  只会打印一行 error 然后**静默忽略**）。另外 **`-aud` 是 AVOption，必须带值 `1`**，
  否则它把下一个选项吞成自己的值。
- **多 slice 图像是这一阶段最大的坑**：`-tune zerolatency` 下 720p 一帧切成 **10 个 slice NAL**
  （真 ffmpeg 实测），所以访问单元只能按 `first_mb_in_slice == 0`（NAL 头后第一字节的最高位，
  即 ue(v) 0）判定新帧起点。按「一个 NAL = 一张图」写的结果是电视上永远只有十分之一张画。
- 参考实现的「发送任务监工」在这里由 pump 线程 + generation 承担（与 LOAD 通道同一套），
  没有另起一个看门狗。
- **以上没有一条见过真电视**：Part 24 的假设备与发送端共用同一张表，只能证明字节自洽
  （AGENTS.md §4.9 明确这不算证据）。真机那一步是 `scripts/cast_streaming_probe.py`。

### 2.3 Mac-Screencast：投到任意浏览器

- 传输选型即答案：**首选 fetch 喂 MSE 的 fragmented MP4，1.5 s 内 `sourceopen` 不成就退渐进式
  `<video src=/live.mp4>`** —— 两条路共用同一条字节流。MJPEG 带宽爆炸且没声音，WebCodecs
  各家支持度不齐（Safari 26 才完整，Firefox 的 H.264 解码器可用性存疑），都不作为主路。
- 关键帧间隔 = 分片节奏（1 s），**服务端缓存每代的 init segment** 让晚到观众能中途接上而不影响已有观众。
- 自动播放策略：先试非静音 `play()`，失败退静音，任意键/点击/触摸解锁后**重连也保持解锁**。
- 实时边缘留 3 s 缓冲 + 8 s 卡死看门狗；`Wake Lock` 防手机熄屏。
- 一个 GPU 帧相关的坑：VideoToolbox 出来的帧不发带标签的关键帧，要过一道 `OffscreenCanvas`。
- 安全：它**完全没有鉴权**（假设纯内网）—— 我们**必须**带令牌（AGENTS.md §4.7 的教训）。

### 2.4 Castify：工程范式（不是功能）值得抄

- **编码器探测是「逐个 try 构造器，失败就下一个」**：NVENC→QSV→AMF→libx264；
  **完全没有 VideoToolbox**（grep 为空），所以 Mac 上它其实一直在用 CPU x264 —— 我们要比它做得对：
  macOS 首选 `h264_videotoolbox`，探测手段就是 `ffmpeg -encoders` + 一次 1 帧试编。
- **自适应档位**：EWMA(发送耗时, 失败率)，1 s 一拍，High/Balanced/Low/Emergency = 60/40/24/15 fps 上限，
  均值 >28 ms 或失败 8% 降级、>55 ms 或 25% 直接跳到最低、连续 4 拍稳定才回升。
- **新观众强制 IDR**、**丢包后直到 IDR 之前不渲染**、150 ms/60 序列号的乱序缓冲、A/V 漂移 >100 ms 丢视频帧。
- 多显示器选择、区域裁剪有；**窗口级/单 App 捕获**靠 macOS ScreenCaptureKit（还 vendor 了一份 MIT 的 SCK 桥），
  **avfoundation 给不了这个能力** → 列入不做（R14）。
- 全局热键用 `rdev`（需要辅助功能权限 + GUI 事件循环），菜单栏形态不值这个成本 → 不做。
- 成熟度：约 19k 行、只有 30 个 `#[test]`、1 个 release、1 star —— 只取思路。

### 2.5 UxPlay：把「iPhone 镜像进这台 Mac」补上（AGENTS.md §9 的边界要改）

- 三种模式：镜像（AirPlay 2 「Legacy Protocol」的 H.264 通道）、RAOP 音频、`-hls`。
  iOS 屏幕镜像列表要的是 `_airplay._tcp`（TXT `features=0x5A7FFEE6,0x0`、`deviceid`、`srcvers`），
  配对（`/pair-setup` `/pair-verify` ed25519，密钥落 `~/.uxplay.pem`）**已实现**，PlayFair 只为 30-pin 老设备。
- **镜像解密只是 AES-128-CTR**，密钥 = SHA1("AirPlayStreamKey"+streamConnectionID+RAOP 音频 key) ——
  不需要 FairPlay 破解！AGENTS.md §9 那句「需要 FairPlay 解密，不打算做」对**镜像**这一路是**过时判断**（音频/DRM 内容仍不可能）。
- 有 **`-vrtp` 转发模式**：把解密后的 H.264 重新打包成 RTP/UDP 发给 `127.0.0.1:port`，
  理论上能喂给 mpv（未实测）；`-vs 0 -as 0` 可以完全无窗口，`$UXPLAYRC` 支持配置文件。
- 官方支持 macOS 构建（cmake + GStreamer framework），macOS 只有 `glimagesink/osxvideosink/osxaudiosink`；
  **Homebrew 是否有 uxplay formula 未证实**。macOS 上最小可用形态是「让 uxplay 弹自己的窗口」，
  喂 mpv 是更好的形态但风险更高（列为 P5 的两个子任务）。
- 最大风险：Apple 哪天砍掉 AirPlay 2 Legacy → 静默失效；DRM 应用镜像出来是黑屏（要提前在 UI 说明）。

**P5 落地的偏差**（2026-09-21，全部按上游 master 源码复核，见下）：

- **`$UXPLAYRC` 换成 `-rc <file>`**。`$UXPLAYRC` 在文件不存在时**静默回落到 `~/.uxplayrc`**，
  于是用户的配置文件会被读进来（我们写的默认值可能被覆盖），更糟的是它还可能被**写**进用户主目录。
  `-rc` 只读指定的一份、文件缺失时明确报错退出，argv 在 rc 之后解析（argv 赢），
  rc 内部**后写的行赢** ⇒ 用户附加项放在文件末尾就是覆盖我们的默认值，这个顺序是故意的。
  rc 格式：一行一个选项、**不写前导 `-`**、`#` 注释、空格分词、值以 `-` 开头会被拒收。
- **不加 `-p`**。`-p` 是 legacy 固定端口（TCP 7100 / 7000 / 7001），7000 正是 Macast 自己的
  AirPlay 接收端端口（`AIRPLAY_PORT`）和 macOS 自带接收端的地盘；不带 `-p` 时 uxplay 用**动态端口**
  并写进自己的 mDNS TXT，发送端按它说的连，所以固定端口对我们没有任何好处，只有撞车。
- **默认值取 `vsync no`，macOS 再加 `vs osxvideosink`**（README 对镜像的推荐；
  macOS 侧只有 `glimagesink`/`osxvideosink`/`osxaudiosink`，而 `glimagesink` 有已知问题）。
- **日志必须挂在 pty 上，不是管道**。uxplay 的 `log()` 是 `printf` 到 stdout，
  全程序只有一处 `fflush`（音频进度那条）⇒ 管道下 stdout 是**块缓冲**，
  「谁连上了」这类事件可能永远不出现；`pty.openpty()` 恢复行缓冲后事件才是即时的。
  Windows 没有 pty，退回管道并如实说明（只有 `audio progress` 那种高频行会先被读掉）。
- **Homebrew 没有 uxplay formula，官方 release 也不提供 macOS 二进制**（assets 只有
  `uxplay.spec` / `PKGBUILD`）⇒ 「未证实」现在有答案了：**必须用户自己编译**。
  所以插件在找不到二进制时打的是完整配方（Xcode CLT + `cmake libplist openssl@3` +
  GStreamer runtime/-devel `.pkg` + `cmake . && make && sudo make install`），
  `selfcheck.py` 里同样给这段话，而不是只说「装一下 uxplay」。
- **一期仍然让它自己开窗**（`-vs osxvideosink`），`-vrtp → mpv` 那条留在二期 ——
  没做实机证据之前不把「统一渲染」写进承诺。
- uxplay 是**完整的 AirPlay 接收端**（镜像 + RAOP 音频，看 `lib/dnssdint.h` 的 TXT 键就知道），
  所以它和 `plugins/raop.py`（shairport-sync）**广播的是同一个名字**：两个都启用时 iPhone 只会看到
  一个入口，谁抢到算谁。这一点插件不猜，只在启动时提示一次，交给用户关一个。

### 2.6 mkchromecast：这次只挖到「发送端该抄的 4 件事」

- **它的 macOS `--screenshare` 根本没实现**（只有 x11grab / GStreamer-Wayland），系统音频硬编码
  `BlackHole 16ch` 且直接把默认输出改成 BlackHole（**Mac 自己就没声了**）——
  我们的名字自适应探测 + 多输出聚合是严格更好的。
- 值得搬的：① 本地文件的 **Range/206 静态服务**（`nodejs/html5-video-streamer.js`，要搬就搬语义，
  它是 JS）；② **ffprobe 决定 copy 还是转码**的启发式；③ `--hijack` 的**意图**（被抢走/app 变了要重新接管，
  我们目前没有任何重连接看门狗）；④ **真 QUIT_APP**（我们现在 `stop` 只发 `sessionId:None` 的 STOP，
  电视不会回主页）。
- 别搬：node/webcast 后端（`UnboundLocalError` + 自陈「Never worked」）、Sonos（自陈 broken）、
  Flask、psutil、暂停用 `pkill -STOP`、pychromecast（我们自研 Cast v2）。
- 音频真机约束（它文档实测）：Chromecast 上限 24-bit/96 kHz，**只有 wav/flac 是真 HD**，
  mp3/ogg 会被限到 48 kHz，向上重采样「不是好主意」。

**P4 落地的偏差**（2026-09-21）：

- **这一阶段动了核心**，虽然是「在线插件」阶段：`macast/protocol_cast.py` 的 STOP / QUIT_APP
  现在会清掉会话账本（`_session_id` / `_media` / `_observed_transport` / `_idle_reason` 置空 +
  `generation` 递增）。因为「停止投屏电视还挂着最后一帧」有两半：设备侧要 QUIT_APP（插件发），
  **我们自己的接收端**也不能继续声称还在播那部片子（核心记账）。Part 25 的 A/B 就是打在这 11 行上：
  把 `protocol_cast.py` 换回 HEAD 版重跑 → 910/911，红的那条正是
  「…and the receiver stops claiming a media it no longer holds」。
- **`MAX_ADVERTISED_SIZE` 取 1.9e9 而不是 `2**31-1`**：同一族「老固件在有符号 32 位里做长度运算」
  的约束（§2.1 里 MirrorCast 那条），留出 HTTP/DIDL 开销余量；被这个上限卡住时
  **降码率来适配**（`bitrate_for_convert`）而不是谎报长度。
- **转码路径的「拖动」= 从新时间点重启编码器**（`seek_to` 里 `mode == 'convert'` 分支），
  因为边播边转的东西没有索引可跳；直通路径的暂停/拖动/音量都真的作用在设备上。R11 因此是
  「两条路径都成立，但语义不同」，菜单与文档都按这个措辞。
- **DLNA 目标的音量故意不做**：音量在设备自己的 `RenderingControl` 服务上（第二个控制 URL，
  我们不去解析）—— 远端比这个菜单更靠近功放。Cast 目标才走 `SET_VOLUME`。
- **avfoundation 的设备表按真机输出重写了解析**：本机 `ffmpeg -list_devices` 实际打的是
  `AVFoundation audio devices:` + **不带引号**的 `[0] 名称`，而按 `"名称"` 解析的版本在这里
  **一条都读不到**（症状是「系统声音档位永远说没有采集口」，不报错）。现在两种拼写都认。
  顺带记下：`screen_mirror._avfoundation_lists` 是同一套引号解析，对今天这份真实输出同样值得复核
  → 排进 P6。
- **未验证**：真实 Chromecast 与真实 DLNA 电视**一台都没有接触过**。Part 25 用的是自家假 Cast 设备
  （真 TLS + 真 Cast v2 帧）+ 真 HTTP 服务 + **自家 DLNA 接收端**校验我们发出的 SOAP/DIDL。
  §4.9 那一族「我们没崩、只是设备不认账」的问题只能等真机。

---

## 3. 架构决定

### 3.1 为什么镜像只做一个插件（而不是「Cast 插件 / DLNA 插件 / 浏览器插件」三个）

因为**采集与系统音频那 600 行只有一份**：avfoundation/gdigrab/x11grab 探测、BlackHole 探测与
CoreAudio 聚合设备、`_Broadcaster` 扇出、ffmpeg 进程监工与 generation 判定。
拆成三个单文件插件（在线插件必须单文件，见 AGENTS.md §4.8）= 复制三份同样的坑，且**渲染器互斥**
（§0 硬约束 2）意味着用户装三个也只能用一个。

所以：`screen_mirror.py` 从「镜像到 Chromecast」升级为**镜像中枢，多目标**（Chromecast-LOAD /
Chromecast-Mirroring / DLNA 电视 / 浏览器），目标就是它的一个设置项 + 菜单一级子菜单。

### 3.2 插件划分

| 插件 | 类型 | 场景 | 阶段 |
|---|---|---|---|
| `screen_mirror.py` v0.4→v0.7 | renderer | R1/R3/R5/R6/R7/R8/R12/R13（镜像，四种目标） | P1/P2/P3 |
| `cast_local_file.py` v0.1 | renderer | R9/R10/R11 + 只投系统声音（音频档）+ 播放列表 | P4 |
| `airplay_mirror.py` v0.1 | protocol（与渲染器共存） | iPhone/iPad/Mac 镜像**进来**（补 §9 空白） | P5 |
| `scripts/selfcheck.py` | 工具 | 环境自检扩展：ffmpeg 编码器能力、BlackHole/聚合设备、uxplay 是否存在、DLNA 渲染器探测 | P1-P5 随行 |
| `scripts/cast_streaming_probe.py` | 工具 | Cast Streaming 的裸机探针（假接收端 + 真接收端两侧） | P3 |

### 3.3 公共形状（每个新插件都必须遵守）

- 文件头清单：`<macast.title>` `<macast.version>` `<macast.platform>` `<macast.description>`
  （值里**绝不能出现 `<`**，只写头部，AGENTS.md §4.5）。
- 定位外部程序：PATH + 常见安装目录（Finder 启动没有 shell PATH）；`_clean_env()` 摘代理变量。
- 子进程：`terminate → wait(5) → kill`，且**移交 renderer 持有**；杀/让位前先增 generation。
- 慢消费者**丢整块、绝不阻塞读管道**（阻塞会把编码器冻住）。
- 菜单在 UI 线程上：绝不为 `build_menu` 去 spawn ffmpeg 探测（结果缓存 + 后台刷新）。
- 任何新的运行时 GitHub 地址必须走 `plugin_repo.py` 的镜像函数；页面里不写死镜像前缀。
- 日志正文/标题一律不用 `v-html`（AGENTS.md §4.10）。

---

## 4. 阶段计划（每阶段：代码 + 用例 + 文档 + pyflakes + 全量回归 + SSH 推送）

验收统一要求：`env -u PYTHONPATH .venv/bin/python -m pyflakes <改动文件>` 干净；
`scripts/verify_cast_airplay.py` 全绿且**新用例确实能抓旧代码**（把改动 stash 回旧版重跑必须变红，
AGENTS.md §4.9 的举证习惯）；不触碰用户真实配置；每次推送后把 commit 号记回本文 §6。

| 阶段 | 交付 | 关键技术点 | 新增用例 | 风险 |
|---|---|---|---|---|
| **P0** | 本文档 + 台账 | 取证与许可判定 | — | 无 |
| **P1** ✅ | `screen_mirror` v0.4：目标=**浏览器**；多显示器选择；画质四档（**360 / 720 默认 / 1080 / 原始分辨率**，计划里的「4K」并入「原始分辨率」—— 采集高度由 `avfoundation` 给，缩放档位没有意义）；光标开关；macOS **VideoToolbox 硬件编码**（先探测再允许）；`caffeinate` 防休眠；菜单状态页显示 时长·码率·观看端·丢块 | fMP4(`frag_keyframe+empty_moov`) + init-segment 缓存 + **每会话** token 门控的播放器页（MSE，1.5 s 超时退渐进式）+ 自动播放解锁 | **Part 22**（69 条） | 低（全部复用已验证的采集/扇出）；iOS Safari 的 MSE 支持待实测 |
| **P2** ✅ | `screen_mirror` v0.5：目标=**DLNA 电视** | 假装有长度的直播 HTTP：对外 `Content-Length` = 按档位码率算出的固定值且 **< 2³¹**（`DLNA_MAX_ADVERTISED_SIZE = 1.9e9`）、探测请求**恰好回 n 字节**（不足补 MPEG-PS 填充包）、`Accept-Ranges` + `transferMode.dlna.org: Streaming` + `contentFeatures.dlna.org`、**48 MiB**（`DLNA_RING_BYTES`，计划里的 64 KiB 太小：按 4.5 Mbps 只有 0.1 秒余量）按绝对字节偏移的阻塞式重连 + 20 MiB 预填（`DLNA_PREFILL_BYTES` ⇒ 菜单明说的 ~35 s 延迟）、stdlib SSDP/SOAP（`urllib`，不打第三方）、`GetTransportInfo` 看门狗 + `RelTime` 前进才算活着、**5 档 profile**（ps-pal / ps-ntsc / ts-mpeg2 / ts-h264 / mkv-h264，PAL/NTSC 用 AC-3）、连续失败自动换档并在用尽后提示手选 | **Part 23**（93 条） | 中：**没有老电视可验**，只能拿 Macast 自己的 DLNA 接收端当替身；真实兼容矩阵必须标注「未验证」 |
| **P3** | `screen_mirror` v0.6：目标=**Chromecast 低延迟镜像**（Cast Streaming），失败自动回落 LOAD mpegts | LAUNCH `0F5096E8` + 残留 app 清理 + webrtc OFFER/ANSWER；不 connect 的 UDP；19 字节 RTP+Cast 头；**纯 Python AES-128-CTR**（无新依赖）；Annex-B AU 切分；RTCP SR（首帧立即发）；PLI/kickstart/在途 12 帧；视频优先（音频二期） | **Part 24** + `cast_streaming_probe.py` | **高**：作者自己没对真机验过，各家固件/代际差异未知；无手机时只能自证字节自洽（AGENTS §4.9 明确这不算证据） |
| **P4** ✅ | `cast_local_file` v0.1 | 本地文件/URL/播放列表 → Cast(含真 QUIT_APP)/DLNA；stdlib Range/206 静态服务；ffprobe copy-vs-transcode 启发式；音轨/字幕选择 + `AudioDelay`；只投系统声音的音频档（码率上限遵守 §2.6）；被抢占后的重连接看门狗。**偏差见 §2.6 末**（含"这一阶段动了 `protocol_cast.py` 的会话账本"） | **Part 25**（155 条） | 低-中：DLNA 侧的 `SetAVTransportURI` 语义已有；Cast MEDIA 命令收发已有；**真机一台没验** |
| **P5** | `airplay_mirror` v0.1（protocol 插件，`uses_ssdp=False`）+ §9 边界改写 + `selfcheck` uxplay 探测 | 监督 uxplay：**`-rc <file>` 生成在 Macast 自己的配置目录**（不是 `$UXPLAYRC`，它静默回落 `~/.uxplayrc`）、**不带 `-p`**（legacy 7000 与自家 AirPlay 接收端撞车）、默认 `vsync no` + macOS `vs osxvideosink`、**stdout 挂 pty** 才拿得到即时事件（C 侧块缓冲）、日志解析 连接/断开/被拒/mDNS 失败/自行退出、不映射 DLNA 播放状态、与 `raop.py` 同名竞争只提示一次。一期让它自己开窗，二期再试 `-vrtp/-artp → mpv`。**偏差见 §2.5 末** | **Part 26**（43 条）+ Part 24 两条（teardown 先排空再挂断） | 中：macOS **确认**没有现成二进制（无 formula、release 只有 spec/PKGBUILD）→ 必须**明确标注需要用户自备**，且 Apple 砍 Legacy 会静默失效 |
| **P6** | 文档与发布 | `docs/Casting-Suite.md` 用户指南（含每目标的首次设置流程）、`plugins/README.md` + `info.json` 条目（**两步提交：先插件文件，再指 SHA/version**）、AGENTS.md §4.8/§9 更新、复核 §4.8 里 pyobjc 依赖是否与「只用自带库」矛盾（`_create_aggregate` 用了 `Foundation`，而 `requirements/*.txt` 没有 pyobjc —— 要么去掉，要么按 §4.4 三处同步）、**`scripts/selfcheck.py` 补齐 §3.2 承诺的随行项**（P1-P4 都没动它：现在只报「ffmpeg 在不在」，缺 编码器能力 / ffprobe 在不在 / 系统音频采集口 / DLNA 渲染器与 Chromecast 探测 / 转码临时目录剩余空间）、**复核 `screen_mirror._avfoundation_lists` 的引号解析**（本机真实 `ffmpeg -list_devices` 输出是 `AVFoundation audio devices:` + 不带引号的 `[0] 名称`，P4 已按这份实测重写了 `cast_local_file` 的解析，那条老路径要用同一条实测输出重验）、版本号两处 + tag | 5c 一致性 | 无（但 pyobjc 与 avfoundation 解析这两条是**已知不一致**，必须给结论） |

**每阶段完成后立即 `git push git@github.com:pingod/Macast.git main`**，并在 §6 记录 commit。

---

## 5. 明确不做

- FairPlay 破解、任何 DRM 内容（Apple TV 应用/Netflix 镜像出来是黑屏，UI 要提前说明）。
- Miracast / Wi-Fi Display（macOS 无 P2P API；`docs/reference-projects-review.md` §1 已定论）。
- 窗口级/单 App 级捕获（avfoundation 无此输入源，需要 ScreenCaptureKit 原生桥 = 新构建链）。
- 真 WebRTC 发送到浏览器（`aiortc` 不是自带库；单文件插件路线禁止 pip）。
- AirPlay **发送端**（推给 Apple TV）：需要 pyatv（pip）→ 若将来要做只能走内置插件 + §4.4 三处打包，
  本文暂不排期；macOS 系统自带「屏幕镜像」已经覆盖这个场景。
- Sonos、多房间、麦克风直通、全局热键、HDR、鼠标点击高亮。
- 搬任何 PolyForm Noncommercial（MirrorCast）代码 —— 只实现同一协议事实。

## 6. 进度台账

| 阶段 | 状态 | commit | 验证 |
|---|---|---|---|
| P0 规划 | ✅ 文档落地 | `ed429fe` | 文档型改动 |
| P1 浏览器目标 + 采集预设 | ✅ 已交付 | `df4a021`（索引指过去）+ 紧随的 info.json 提交 | `pyflakes` 干净；`verify_cast_airplay.py` **582 条全绿**（Part 21 修到 v0.4 契约、新增 Part 22 62 条）。A/B 举证：把 `screen_mirror.py` 换回 HEAD 版重跑 → 套件 510/514，Part 22 立刻 `TypeError: build_ffmpeg_command() got an unexpected keyword argument 'kind'` |
| P2 DLNA 电视目标 | ✅ 已交付 | `94174cb`（插件+测试+文档）+ 紧随的 info.json 提交 | `pyflakes` 干净；`verify_cast_airplay.py` **675 条全绿**（新增 Part 23 93 条）。A/B 举证：把 `screen_mirror.py` 换回 HEAD 版重跑 → 套件 582/584，Part 23 当场 `module has no attribute 'DLNA_PROFILES'`（整段 91 条不再执行）。**未验证**：真实老电视兼容矩阵（无设备），见 §2.1 末「P2 落地的偏差」 |
| P3 Cast Streaming | ✅ 已交付（**真机未验证**） | `e939b66`（插件 v0.6 + Part 24 + `cast_streaming_probe.py` + 文档）+ 紧随的 info.json 提交 | `pyflakes` 干净；`verify_cast_airplay.py` **756 条全绿**（新增 Part 24 81 条：OFFER/ANSWER 形状、AES 与 `/usr/bin/openssl` 逐字节对齐、19 字节头与切片/序号回绕、SR 与 NTP 纪元、`parse_rtcp` 五种读法、8 位帧号扩展、多 slice 访问单元，再到真 TLS + 真 UDP 上对打自家假设备：清理残留 app → LAUNCH → OFFER → 解密首帧 → 12 帧窗口 → checkpoint 解锁 → PLI → teardown 顺序 → `LAUNCH_ERROR` 回落 LOAD/mpegts）。A/B 举证：把 `screen_mirror.py` 换回 HEAD 版重跑 → 676/678，Part 24 两段各自报 `module has no attribute 'build_offer'` / `'MIRROR_APP_ID'`（整段不再执行）。**另用真 ffmpeg 交叉验证**（不是打桩）：2 秒 48 帧、关键帧恰好落在 0 与 24（GOP 承诺成立）、每个访问单元以 AUD 开头、**720p 每帧 10 个 slice** —— 正是这条实测把「一个 NAL 一帧」的写法判死。**未验证**：任何真电视（见 §2.2 末「P3 落地的偏差」与 AGENTS §4.9） |
| P4 本地文件/播放列表 | ✅ 已交付（**真机未验证**） | `c2ebb9c`（`plugins/cast_local_file.py` 2916 行 + Part 25 + `protocol_cast.py` 账本 + 文档）+ `5a6e338`（info.json 条目，指到 `c2ebb9c` 的 SHA） | `pyflakes` 干净（仅 §基线的 `pyperclip` / `PIL.Image` 两条历史告警）；`verify_cast_airplay.py` **911 条全绿**（Part 25 155 条：ffprobe 决策表逐条含理由文案、stdlib Range/206 服务的真 HTTP 语义、会增长的转码文件与长度/码率回退、自家假 Cast 设备上的 LOAD 与 `QUIT_APP`、DLNA 的 `SetAVTransportURI` + 断点 `Seek` + SOAP 的 UTF-8 长度、自动连播、看门狗重试预算与认输、音轨/字幕与 sidecar WebVTT、avfoundation 真实设备表、菜单与页脚），加完索引条目后 **922/922**。**A/B 举证两轮**：① 把 `macast/protocol_cast.py` 换回 `git show HEAD:` 版重跑 → **910/911**，红的正是「…and the receiver stops claiming a media it no longer holds」；② 把 P4 中途修好的三处退回修前写法（`spec = parse_range(header)` / 去掉「文件已消失就拒答」/ avfoundation 只认带引号的名字）→ **905/911**，六条红各自落在「整文件读也回 206」「HEAD 承诺区间」「消失的文件」「sidecar 当 WebVTT 供」「audio 块不被读成 video 块」「假 ffmpeg 说真话」。**顺手把 Part 24 的一条改判为确定性**：「the next key frame gets through regardless」曾在满载的 12 帧在途窗口上偶发失败（假设备只在测试点名时才 ack，而窗口满时**连关键帧都要等**是设计如此），现在观测时同时把信用打开，测的才是「丢图指示之后关键帧优先」这一件事。**未验证**：真 Chromecast 与真 DLNA 电视各一台都没碰过（见 §2.6 末「P4 落地的偏差」） |
| P5 AirPlay 镜像接收 | ✅ 已交付（**真机未验证 —— 这台机器上没有 uxplay**） | `6811496`（`plugins/airplay_mirror.py` + Part 26 + Part 24 的排空用例 + selfcheck + 文档；**这一提交同时并入了另一会话并行完成、按文件已不可拆的改动**：`macast/logsplit.py`＋Part 27、`macast/module_settings.py`＋Part 28、`screen_mirror` v0.7 一键设置步骤机＋Part 29、`raop` v0.2 设备名）**＋紧随的 info.json 提交**（三条：新增 `AirPlay Screen Mirror 0.1`、`Screen Mirror` 0.6→0.7、`AirPlay Audio (RAOP)` 0.1→0.2，全部指到 `6811496` 的 40 位 SHA） | `pyflakes` 干净（只剩基线的 `pyperclip` / `PIL.Image` 两条历史告警，以及 HEAD 里就有的 `protocol.py` 未用 `e` 与 `Macast.py` 的 `_`）；`verify_cast_airplay.py` 发布后 **1064/1064**（索引每条贡献 6~7 个断言，所以加一条目总数就涨，1053 → 1064；新增 Part 26 43 条：假 uxplay 走完 启动 → 选项文件内容（`n "…"` 引号化、`vsync no`、`vs osxvideosink` 只在 darwin、用户附加选项排在最后因而能覆盖默认）→ 连接/断开/被拒/mDNS 失败四类事件各且只通知一次 → 意外退出带 uxplay 最后几句里的 error → `reload` → `stop` 不报错，外加「找不到二进制」这条正常路径：只通知一次 + 日志里有配方 + 不拉起任何东西 + 不写文件，以及 `uses_ssdp is False` 与两个 `__init__` 契约）。**A/B 六轮**（Part 26 单独红）：M1 不找二进制 → **957/961**；M2 不写选项文件 → **959/961**；M3 名字不做引号处理（`n %s`）→ **959/961**；M4 事件重复上报 → **960/961**；M5 `_read_output` 直接抛 → **957/961**；M6 意外退出不通知 → **960/961**（各轮基线 961 = 当时套件总数）。**排空修复另有一轮**：把 `self._drain()` 从 `_CastSender.close()` 删掉 → **1048/1053**，三条红各自是「goodbye 序列只到达 `['CLOSE']` 且对端 `BrokenPipeError`」「那是干净挂断而不是吃掉最后一写的复位」「teardown 先离 app 再断 transport」—— 后者是**既有用例**，说明这条修复不只服务于新用例。**未验证**：真 iPhone 镜像到真 Mac、真 Linux，以及 **uxplay 本身在这台机器上不存在**（`selfcheck` 现在会 warn 并给出配方）。风险原条目里"Homebrew 未证实"已按上游证实为**没有 formula、release 也没有 macOS 二进制**，所以"用户自备"是硬前置 |
| P6 文档/索引/发版 | ⏳ | | |
