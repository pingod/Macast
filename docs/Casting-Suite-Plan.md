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

1. **内置插件（`macast/plugins/*.py`）只能是单个 .py 文件**，依赖仅限：标准库 + Macast 已带的库
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
  所以它和 `macast/plugins/protocol/raop.py`（shairport-sync）**广播的是同一个名字**：两个都启用时 iPhone 只会看到
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

- **这一阶段动了核心**，虽然是「内置插件」阶段：`macast/protocol_cast.py` 的 STOP / QUIT_APP
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
拆成三个单文件插件（内置插件必须单文件，见 AGENTS.md §4.8）= 复制三份同样的坑，且**渲染器互斥**
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
| P5 AirPlay 镜像接收 | ✅ 已交付（**真机未验证 —— 这台机器上没有 uxplay**） | `6811496`（`plugins/airplay_mirror.py` + Part 26 + Part 24 的排空用例 + selfcheck + 文档；**这一提交同时并入了另一会话并行完成、按文件已不可拆的改动**：`macast/logsplit.py`＋Part 27、`macast/module_settings.py`＋Part 28、`screen_mirror` v0.7 一键设置步骤机＋Part 29、`raop` v0.2 设备名）**＋ `d7fc2ec`（info.json 提交**：三条：新增 `AirPlay Screen Mirror 0.1`、`Screen Mirror` 0.6→0.7、`AirPlay Audio (RAOP)` 0.1→0.2，全部指到 `6811496` 的 40 位 SHA） | `pyflakes` 干净（只剩基线的 `pyperclip` / `PIL.Image` 两条历史告警，以及 HEAD 里就有的 `protocol.py` 未用 `e` 与 `Macast.py` 的 `_`）；`verify_cast_airplay.py` 发布后 **1064/1064**（索引每条贡献 6~7 个断言，所以加一条目总数就涨，1053 → 1064；新增 Part 26 43 条：假 uxplay 走完 启动 → 选项文件内容（`n "…"` 引号化、`vsync no`、`vs osxvideosink` 只在 darwin、用户附加选项排在最后因而能覆盖默认）→ 连接/断开/被拒/mDNS 失败四类事件各且只通知一次 → 意外退出带 uxplay 最后几句里的 error → `reload` → `stop` 不报错，外加「找不到二进制」这条正常路径：只通知一次 + 日志里有配方 + 不拉起任何东西 + 不写文件，以及 `uses_ssdp is False` 与两个 `__init__` 契约）。**A/B 六轮**（Part 26 单独红）：M1 不找二进制 → **957/961**；M2 不写选项文件 → **959/961**；M3 名字不做引号处理（`n %s`）→ **959/961**；M4 事件重复上报 → **960/961**；M5 `_read_output` 直接抛 → **957/961**；M6 意外退出不通知 → **960/961**（各轮基线 961 = 当时套件总数）。**排空修复另有一轮**：把 `self._drain()` 从 `_CastSender.close()` 删掉 → **1048/1053**，三条红各自是「goodbye 序列只到达 `['CLOSE']` 且对端 `BrokenPipeError`」「那是干净挂断而不是吃掉最后一写的复位」「teardown 先离 app 再断 transport」—— 后者是**既有用例**，说明这条修复不只服务于新用例。**未验证**：真 iPhone 镜像到真 Mac、真 Linux，以及 **uxplay 本身在这台机器上不存在**（`selfcheck` 现在会 warn 并给出配方）。风险原条目里"Homebrew 未证实"已按上游证实为**没有 formula、release 也没有 macOS 二进制**，所以"用户自备"是硬前置。**SHA 固定只能本地证，而"装得到"这件事在本轮被错判过一次**：Part 5c 用 `git show <sha>:plugins/<file>` 比对清单，这条真绿；当时 `cdn.jsdelivr.net` / `api.github.com` 对旧固定链接也回 404，结论被写成"沙箱测不出可达性" —— **那句结论是错的**，P6 用三条带公开对照组的探针重测（匿名 GitHub API 404 而上游 200、jsDelivr 元数据 API 404、`gh api repos/pingod/Macast --jq .private` → **true**），真相是**仓库是私有的**，别人本来就拉不到，能测、也测出来了。教训写进 AGENTS §5：一个 404 说不出是"私有"还是"不通"，必须配一个**公开对照**才能定性 |
| P6 文档/索引/发版 | ⏳ 进行中 —— 第一~五批已落（§4 点名要重验的那条已知不一致已修；索引可达性已定性并按"保持私有"落地；用户指南 + 自检随行项；端到端回归 + 它的耦合守卫；v0.9 修掉用户报的「永远装不完」）。**只剩发版**：版本号与 tag 已推（`aece62e` / `v0.7.15`），但 Release 产物被 Actions 存储配额挡在门外，见下面的 §6.3 | `7028bd4`（`screen_mirror` v0.8 解析修复 + Part 31 + Part 29 用例改判 + Part 30 + pyobjc 声明）＋ 紧随的 info.json 提交（Screen Mirror 0.7 → 0.8，指到 `7028bd4` 的 40 位 SHA）＋ `8a27b47`（第二批：`scripts/check_index_reachability.py` + selfcheck「online plugin index」段 + AGENTS §4.6/§5/§6 + 两份 README + 本文 §6 的可达性结论）＋ `d8643ea`／`fa32676`（第三批：`docs/Casting-Suite.md` 用户指南 + selfcheck 的「sender plugins」段 + Part 32 一致性用例）＋ 第四批 `839b208`（`scripts/e2e_smoke.py` + Part 33 + "保持私有"决定的文案与文档落地，已推送）＋ 第五批 `0996789`（`screen_mirror` v0.9 一键设置五态判定 + Part 29 反循环用例 + `NSMicrophoneUsageDescription` + selfcheck 的「盘上有驱动 ≠ 能采集」，紧随的 info.json 提交 `e58274b` 把条目指回它） | **§4 原条目"复核 `_avfoundation_lists` 的引号解析"结论：那不是一个解析瑕疵，而是一条从 v0.1 就断掉的主路径。** 真实 `ffmpeg -f avfoundation -list_devices true -i ""` 在这台机器上输出的是小写 `AVFoundation video devices:` + `[0] OBS Virtual Camera`（**全程没有双引号**），而旧解析找的是大写 `Video devices:` 并且只取双引号之间的内容 ⇒ 真机上两个列表恒为空 ⇒ `_probe_avfoundation` 回 None ⇒ 菜单报「ffmpeg 没有列出任何屏幕采集设备（avfoundation）」，**macOS 镜像四个版本根本起不来**。为什么一直没被发现：Part 21/22/23 的假 ffmpeg 输出的正是那个虚构格式（测试与实现共享同一个错误假设）。修法与防线：解析器重写（同时认旧版 `List of Video devices:` + `0) name`；**没有索引的行不算设备** —— `-i N:none` 需要那个数字），Part 21/22/23/25 的假 ffmpeg 全部换成真机逐字输出，新增 **Part 31（11 条）**：真机输出 / 旧版写法 / 无索引行 / 真 subprocess 的 argv / 不可解码的设备名 / 探测最终交给 ffmpeg 的 `-i 2:none` 与 BlackHole 的 `2:2`+`-map 0:a:0` / 空列表仍判"无从采集"，外加两条**自我审查**：把被替换掉的旧解析原样留在用例里（它在真机输出上回 `([], [])`，错误保持可执行而不是轶事），以及扫描测试文件自己 —— 任何 `-list_devices` 回答里出现"带引号却没有索引"的设备行立即变红。**A/B 两轮**：① 只把旧 `_avfoundation_lists` 换回去（保留新解析器与修正后的 fixture）→ **968/979**，红的 11 条横跨 Part 21/22/23/31（`probe is None`、`没有列出任何屏幕采集设备`、DLNA 段落整段中止）；② 只把 Part 23 的 fixture 换回虚构格式 → **1059/1064**，红的 5 条里点名了两行虚构 fixture —— 也就是"假 ffmpeg 说谎"这件事现在既能被实现层抓到，也能被形状层抓到。③ Part 29 那条 "announces v0.7" 改成"公告版本号 == 清单版本号"（文件里出现任何其他版本号即红）：它原来要求每次发版都记得改测试文件，而它要抓的恰恰是"标签过期"。**pyobjc 那条也已定性**（§4 原文列的第二处不一致）：`Foundation`/`objc` 来自 `pyobjc-framework-Cocoa`，`requirements/darwin.txt` 与 macOS CI 的 pip 列表现在都点名它，Part 30（8 条）反过来禁止 `plugins/*.py` 引入任何 Macast 没声明的包（负样本 `import aiortc`，正样本 `cherrypy`/`Foundation`/`os`），并互相咬住两处：darwin.txt ⊆ build.yml 的 pip 列表、utils.py 在 darwin 守卫下 import AppKit 是 pyobjc 的**锚**。**发版后套件 1083/1083**，`pyflakes` 干净（只剩基线两条）。**未验证**：真机上重跑镜像（修复本身就是冲着"真机从没成功起过"去的，这台 Mac 有 ffmpeg 与采集设备，但完整镜像链路要在 GUI 里点头授权才算走通）。**同批次的第二个发现（P6 的 §4.6 前提条件）**：为了回答"v0.8 这条固定链接别人拉得到吗"，把 P5 那句"沙箱测不出"重测了一遍 —— **测得出，而且答案是拉不到**：`pingod/Macast` 是私有仓库 （`gh api repos/pingod/Macast --jq .private` → true；匿名 API 404、公开上游 200；`data.jsdelivr.com/v1/packages/gh/pingod/Macast` 404 "Couldn't fetch versions"）。9 条固定链接实测 5 条还回 200（CDN 缓存），4 条已经 404，包括刚发的 Screen Mirror 0.8 与 P5 的 RAOP 0.2 / AirPlay Screen Mirror —— 私有状态不变，剩下的会逐条掉光，没有人碰它也会坏。新增 `scripts/check_index_reachability.py`（纯 stdlib、无凭据、带公开上游对照组，`INDEX_OK` / `INDEX_PRIVATE` / `INCONCLUSIVE` / `AMBIGUOUS` + 逐条状态表 + 退出码 0/2/3；`--json` 给 CI/cron，`--url-only` 单问一个地址），selfcheck 增「online plugin index」段跑同样的三问，`plugins/README.md` 顶部与 README_ZH 的插件段落改为**明说这个前提**。三条出路（改公开 / 把 plugins/ 发到公开仓库并改 `plugin_repo.REPO` / 保持私有并只承诺手动安装）里第一条是**所有者决定**。**［2026-09-21 已定：保持私有，官方承诺只有「手动安装」这一条路］** —— 决定已落进产品文案与全部文档：`plugins/README.md` 顶部改口为"这个目录是**源码**，不是安装源"，设置页 `repo_failed` 的兜底从一句话扩成一段（说明原因 + 两条手动路线），`README_ZH.md` 与 `docs/Casting-Suite.md` §0 同步，AGENTS §4.6/§5 写明**以后不要再提"改公开 / 另立公开仓库"**，`check_index_reachability.py` 在私有状态下稳定报 `INDEX_PRIVATE`（退出码 2）**从此是预期信号而不是待修的 bug**。**同批次的第三项（P6 的文档与随行自检）**：写了 `docs/Casting-Suite.md` —— 面向使用者的**逐目标首次设置流程**（Chromecast 兼容通道 / Chromecast 低延迟 / DLNA 老电视 / 浏览器 / 本地文件 / uxplay / shairport-sync），每条链路都写明"验证到什么程度"，把散在 §2.1–§2.6 各段末的"未验证"落到用户读得到的地方。**§3.2 承诺而 P1-P4 都没做的 selfcheck 随行项也补上了**：`selfcheck.py` 新增「sender plugins」段，问的是发送端真正会卡住的六件事 —— ffprobe 在不在、`-encoders` 里有没有 libx264/h264_videotoolbox/mpeg2video/ac3（这四个各自对应一条**静默失效**的链路）、有没有 libass、**avfoundation 到底列没列出屏幕**（就是 Part 31 那条 bug 想骗过去的同一个问题，自检现在自己会答）、系统音频采集口（mac 查 HAL 里的 BlackHole 驱动文件，Linux 问 `pactl` 要 sink monitor —— 都**不碰 CoreAudio**，因为沙箱里设备枚举不可用）、转码临时目录剩余空间、以及**这个局域网里到底有没有东西可投**（真 mDNS browse + 真 SSDP `MediaRenderer` 探测）。本机实测：编码器四条全 OK、**libass 确实没有**（ffmpeg 9.0.2 的 configuration 里没有 `--enable-libass`，所以"烧字幕"这条路在这台机器上真的不可用，报告说的就是事实）、avfoundation 列出 1 块屏幕、BlackHole 已装、149 GiB 可用、**搜到 1 台 Chromecast 与 1 台 DLNA 渲染器**（就是 Macast 自己）。自检读设置**只读 JSON 文本、不 import `Setting`**（AGENTS §10）。**新增 Part 32（7 条）**守的是"自检与插件各说各话"这一族：编码器集合要与插件实际 `-c:v/-c:a` 双向对齐（多一个过期探针也红）、查找目录必须覆盖插件的搜索路径、只允许读真实存在的设置键、mDNS/SSDP 目标串两侧逐字一致、以及"永远不写用户设置"。**A/B 两轮**：① 从自检里删掉 `ac3` 探针与 `/opt/local/bin` → **1088/1090**，两条红分别点名 codecs 与 search dirs；② 加一个插件不用的 `libx265` 探针 + 一句 `Setting.set(` → **1088/1090**，红在"过期探针"与"不许写设置"。**发版后套件 1090/1090**，`pyflakes` 干净（只剩基线两条）。**第四批（端到端回归 + "保持私有"决定的落地）**见下面的 §6.1 —— 它把发版前套件推到 **1106/1106**；**第五批**（用户报的「系统声音一键安装永远装不完」）见下面的 §6.2 —— 它把套件推到 **1121/1121**，并且第一次让"跳过安装"这一步变得可测 |
| P7 控制面搬到网页 | ✅ 已交付（**打桩 + 真实例浏览器 + 真产物**；投屏链路本身仍未碰过真电视） | 已提交 `224d6d6`：删除 `macast/mirror_console.py`（1931 行）与 `macast/config_window.py`（229 行），新增 `macast/mirror_view.py`（252 行），`screen_mirror` 0.10 → 0.11（净 -341 行），`macast/xml/setting.html` +436 行（新增「电脑投屏」页签），`Macast.py` / `macast/gui.py` / `macast/macast.py` 一起瘦身，Part 35 整段重写为 115 条 | **1187/1187**、`pyflakes` 干净；A/B 两轮（删端点那一行 → 1186；删"镜像中预览让位"六行 → Part 35 三条红）；真实例 + 真浏览器抓到**三条**打桩抓不到的缺陷；产物 `bash scripts/build_macos_arm.sh` 真启动并验过页签在不在。细节与那三条缺陷见下面的 §6.4 |

### 6.1 P6 第四批明细（commit `839b208`，已推送）：端到端冒烟 + "保持私有"决定的落地

`scripts/e2e_smoke.py` 是**这个仓库第一个跑真应用的检查**（33 条：30 PASS + 3 INFO，启动 2.1 秒）。
隔离手法是把 `appdirs.user_config_dir` 在 `import macast` **之前**打桩（AGENTS §4.9 那招），
配置目录整个搬到临时目录，种子设置直接写 JSON：`ApplicationPort=58999` +
`Macast_Protocols=['DLNA Protocol']` + `DLNA_FriendlyName='Macast E2E Smoke'` + 预置 `Api_Token`，
然后 `runpy` 跑真的 `Macast.py`。它问的是：设置页回不回（88 724 字节）、`/api?query=status`
报的版本对不对、**9 个内置插件在活进程里能不能全 import 出来**（标题/版本/类型/已装可用逐条对
`plugins/info.json`）、11 个日志模块、6 个网卡与广播地址、DLNA 事件线程活着、`cast-info`
回显刚种的令牌、无令牌的 GET `cast` 被 403、裸路径被 "url must be absolute" 拒、SSDP `M-SEARCH`
由**这一个实例**应答（6 个 LOCATION，其中 3 个是它的）且描述文件能解析出 `friendlyName`
与 AVTransport 服务、日志里没有 Traceback 且 0 条 ERROR。`--play` 才做真播放回环（自己找 ffmpeg
→ 生成 12 秒测试片 → 起一个**支持 Range** 的小 HTTP 服务 → 经带令牌的 GET 投出去 → 要求 mpv 到
`PLAYING` **且进度数值真的在动** + 日志有 `video-reconfig`）；默认 SKIP 并说明为什么。
**代价明说**：那 ~30 秒里局域网内的 DLNA 电视会短暂看到第二个渲染器，所以它**不进套件**、
只在发版前手动跑（AGENTS §9 有这一行）。

它抓到的三件事都是**只有跑真应用才看得到**的：① **SIGINT 杀得掉菜单栏 Macast，SIGTERM 杀不掉**（三次跑三次一致）—— AGENTS §4.2 那条"起新实例前先杀干净"原来默认了 `kill` 好用；② **信号退出会留下空闲 mpv**（三次里两次；本机另有 5 个 `ppid=1`、活了 1–3 天的 mpv 佐证），所以被反复重启的 Macast 会**攒播放器**；③ **HTTP 报"running"早于 `ProtocolGroup` 挂上子协议**，于是 `subscribers` 在启动后 1.1 秒回 `children=[]` —— 冒烟脚本第一次跑就把它当成 FAIL，改成轮询 + **把等待时长本身当结论报出来**。还有两处是套件层面的教训：「真实配置目录没被我们写脏」这条一开始永远不可能过，因为它把 `macast.log` 也算进了摘要，而**用户那个在跑的实例每分钟往同一份文件里追加 ~27 KB** ⇒ 文件名进摘要、`*.log` 内容按设计排除；以及一次**假 PASS**："进度在动"被 `'00:00:00' != '0:00:00'` 满足了（两个都是 0 秒），于是加了 `_clock()` 按秒比较并要求 `latest > first`。

**Part 33（16 条）**守的是这个脚本自己的隔离性：它是唯一跑真应用的检查，而隔离全靠**字符串**（设置键名、appdirs 打桩顺序、只开 DLNA 的协议表、代理变量名单、它问的 `query=` 键名、`get_status` 的 server 键名）。应用侧改个名，这些字符串就**静默失效**、脚本照样全绿，所以 Part 33 逐条拿 `macast/utils.py` / `macast/protocol.py` 的源码比对。**它当场抓到两个真问题**：`PROXY_VARS` 漏了 `ftp_proxy`/`FTP_PROXY`（应用会摘，脚本没摘），以及脚本自己的 `urllib` 调用没关代理（补 `ProxyHandler({})`）。**A/B 两轮 + 一次反例自查**：① 把 `ApplicationPort = 4` 改名 → 红；② 删掉 BOOTSTRAP 里的 appdirs 打桩 → 红；③ Part 33 自己那条"插件目录要写 `__init__.py`"最初被脚本里的一句**解释性注释**满足了（假 PASS），现在正则限定在 `install_online_plugins` 的函数体内 —— 这个教训和 Part 31 是同一族：**用例能被注释骗过去，就不算用例**。发版后套件 **1106/1106**，`pyflakes` 干净（只剩基线两条）。**仍未验证**：`--play` 只在 127.0.0.1 上自打自，真手机/真电视投进来仍是 §4.9 那一族。

**P6 剩下的**：端到端回归已从"剩下的"里划掉；**版本号与 tag 已推** —— `aece62e`（两处版本号 → 0.7.15）
+ tag `v0.7.15`。发版前跑过一轮完整回归：**1121/1121**、`pyflakes` 干净。
**但 Release 还没建出来**：CI 的产物上传被 Actions 存储配额卡住，细节与重跑办法见下面的 §6.3。

### 6.3 P6 第六批：v0.7.15 发版，以及 CI 其实已经坏了六天

tag 推上去之后 run 99 四个平台**全部失败**，报错是
`Failed to CreateArtifact: Artifact storage quota has been hit`。查下来这不是本次改动：
`actions/upload-artifact` 四处都写着 `retention-days: 14`，而四个平台一次约 170 MB、
免费方案 Actions 存储上限 500 MB —— 从 9 月 14/15 那两波密集构建（154 份 artefacts）开始
就一直在撞，上一次 main 推送（run 97）同样四个 job 全红在**同一步**，
其余步骤（构建、arm64/i18n 校验、签名、打包 zip）全绿。
处理：删掉 189 份旧 artefacts（保留最新一次），把四处 `retention-days` 改成 **2**（`7012268`），
`gh run rerun --failed` 重跑 run 99。
**Releases 的下载文件不受影响** —— 那是另一个桶（GitHub Releases，不是 Actions artefacts）：
删完之后 v0.7.14 的四个产物仍完整在线（24.9 / 41.4 / 39.4 / 74.7 MB，`state: uploaded`）。

**当前状态：还是发不出来，而且不是我们的代码问题。** 重跑的 run 99 四个 job 仍然红在
同一步、同一句 `Artifact storage quota has been hit` —— 因为 GitHub 那句提示的后半段是
**"Usage is recalculated every 6-12 hours"**：删掉的 8.2 GB 要到下一次结算才生效，
现存的 170 MB 早已在限额之下也照样被挡。所以 v0.7.15 这个 Release **暂时不存在**
（`releases/tags/v0.7.15` → 404，发布 job 被 skip），代码与 tag 都已经在 main 上；
下一次配额结算后重跑一次即可，命令就是
`gh run rerun 35583336178 --failed`（不用重推 tag，仓库和产物内容都不会变）。
写进 AGENTS §4.3 的一句话教训：**上传步骤全红 ≠ 代码坏了**，先看红在哪一步。

### 6.2 P6 第五批明细（commit `0996789` + `e58274b`，已推送）：「永远装不完」的根因不是"没重启"

用户报的是：**「系统声音-一键安装有 bug，安装时候需要重启，重启后再点一键安装又要安装一遍，
这样安装流程永远装不完」**，并且明确纠正过一句「我还没有重启电脑，不是没有加载」——
所以这一条不能按"驱动没加载，去重载"收场。

**机器上的证据**（2026-09-21 实测，全部只读）：`kern.boottime` = 9 月 17 日、
coreaudiod 还是 pid 376（同一开机以来的老进程），而 `BlackHole2ch.driver` 与它的
pkgutil 收据都是**今天 12:58** 落盘的；`ffmpeg -f avfoundation -list_devices` 的音频表里
只有「MacBook Pro麦克风」和「JustStream Audio Driver」——**同机另一个厂商的虚拟声卡列得出来**，
所以"枚举被权限挡了"这条解释在这台机器上至少对 CLI 不成立，真实状态就是**驱动在盘上、守护进程没加载它**。

**代码层的事实**：旧 `probe` 分支把「安装会被跳过，改为提示重载音频服务」**写进了 note**，
下一行照样 `progress.enter('meta')` → 下载 → 校验 → `open` 安装器 → `_wait_for_blackhole(300)`，
而唯一有用的 `reload` 只有在那 300 秒超时之后才可达。于是每一次点击都是完整重装一遍，
"永远装不完"是字面意思。**跳过必须是步骤机里的 skipped 状态，不能是一句话。**

**修法**：判定收成一个值 —— `_blackhole_state(ffmpeg)` 返回
`capturable / loaded / on-disk / stale-receipt / absent` 五态，每态一条分支；
`on-disk` 直接跳到 `reload`（一次管理员密码 + 45 秒），`_wait_for_blackhole` 现在回答
**"哪个见证者看见了"**（`capturable` / `loaded` / 空），因为「CoreAudio 有、ffmpeg 没有」
是**麦克风权限**，再装十个 pkg 也不会变，把它当成功报"已就绪"就是第二个谎言。
这一支照样建聚合设备、切默认输出（授权一旦给上就立刻可用），但把 `probe` 改标失败并点名面板。
配套两处：`.app` 的 plist 补上 `NSMicrophoneUsageDescription`（缺它系统连弹窗都不给），
`selfcheck.py` 不再把"HAL 目录里有文件"报成 ok —— 它现在再问一次 ffmpeg，
文件在而设备不在就说"coreaudiod 没加载它，一键设置会去重载、不会重新下载"。

**A/B**：把 `macast/plugins/renderer/screen_mirror.py` 换成 `git show HEAD~1` 的那一份重跑套件 →
**1081/1089**，红的 8 条正是新写的反循环用例（这一轮跑在中途，所以总数还是当时的 1089；
下面补完排他性用例与打桩后才是最终那份 **1121**）。其中两条最能说明旧代码有多不该过：
盘上有驱动那一支旧流程留下 `'install': done` + `['open', '/tmp/bh29.pkg']`（**它自己刚说完会跳过**），
而"CoreAudio 有 / ffmpeg 没有"那一支旧流程一路走到
`系统声音已就绪：开始镜像后声音会一起投出去` —— 用户看到的正是这种绿色。
Part 29 另加 6 条排他性用例（五态判定的先后次序、见证者抛异常不许往上冒、
`_wait_for_blackhole` 的两种答案、进度归属按调用方给的 step 走），
并且把几处**依赖本机真实状态**的旧打桩补全（旧用例在没有 BlackHole 的机器上是运气好才过的）。
最终工作树：**1121/1121**，`pyflakes` 干净（只剩基线两条）。

**仍未验证 / 要说清的**：① 「这台机器还需要那一次重载」这一条**已在 2026-09-21 被真机证实**：
经用户同意用 `osascript ... with administrator privileges` 重载了一次 coreaudiod
（pid 376 → 55784），`ffmpeg -list_devices` 的音频表当场出现 `[0] BlackHole 2ch` ——
也就是"`on-disk` 这一格的判据（盘上有 / 守护进程没加载）与它的解法（一次重载，不用重启）
在真机上是对的"，这是本节唯一一条从"打桩"升级到"真机"的东西。
② 缺 `NSMicrophoneUsageDescription` 到底只挡住"采集"还是连"枚举"一起挡，
在这台机器上**没测过**（CLI 侧枚举是通的，`.app` 从没声明过麦克风用途），
所以这一支的文案写的是"通常是麦克风权限"而不是断言；③ 整条链路（重载设备 → 建聚合设备 →
镜像带系统声音）依旧只在打桩用例与假设备上验过，Part 29 不触碰 CoreAudio。

### 6.4 P7 明细（已提交 `224d6d6`）：把控制面搬到网页，删掉那个 Tk 窗口

**决定**：v0.10 那个桌面控制台窗口（`macast/mirror_console.py` 1931 行 +
`macast/config_window.py` 229 行）**整个删掉**，控制面搬进设置页的「电脑投屏」页签。
理由不是"网页更漂亮"，而是那个窗口要一个**会画 Tk 的 python**：本机 `/usr/bin/python3` 的 Tk 是
8.5.9（`import tkinter` 成功、画出来只有原生按钮），所以"能不能用"取决于用户装没装 `python-tk` ——
一个投屏功能背着一个解释器矩阵。删掉之后菜单栏只留两行：开门的「电脑投屏…」，和镜像进行中才出现的
「停止电脑投屏」（页面也能停，但浏览器崩了 / 标签页被划走时总得有个出口）。
换上来的是 `macast/mirror_view.py` 252 行 + `macast/xml/setting.html` 的 +436 行。

**架构只有三条约束**（这一族以后再加面板都照这三条走）：

1. **事实出自插件，排版出自核心，同一个响应交回去。** 插件只答 `console_state()`
   （它知道设备、档位、探测结果、进度机状态），`mirror_view.view_for()` 决定**哪些卡片出现、按什么顺序**
   （`SECTION_ORDER`：`channels / devices / requirements / profiles / quality / capture / audio /
   viewer / preview / activity`）。分两次请求返回会给出两个时刻的事实 —— 页面就会拿新排版配旧数据。
2. **两边的契约版本必须对上**：`CONSOLE_VERSION == VIEW_VERSION == 3`，对不上时 `banner_for()`
   把两个版本号都写进顶部横幅，并且**排在这张页所有其它提示之前**（旧缓存里的页面点新按钮，
   症状是"按钮没反应"，没人会怀疑版本）。
3. **门控一个字都没放松**：`mirror-state` / `mirror-snapshot` / `mirror-action` 三个端点都要
   `_token_present()`，**loopback 也不例外**；页面自己从 loopback 专属的 `query=cast-info` 取令牌。
   结论要写清：这一页**只在这台机器自己的浏览器里打得开** —— 因为预览就是一张桌面截图，
   而"任何网页都能发一个到 127.0.0.1 的 GET"这条（AGENTS §4.7）在这儿同样成立。

宿主找插件用的是**类旗标** `MIRROR_CONSOLE`（`MacastPluginManager.console_plugin()`），
不按标题、也不按渲染器列表的顺序；而且"要控制面板"是**实例化**插件、不 `start()` 它 ——
所以手头是 mpv / IINA / 任何一个渲染器，这一页都点得开（Part 35 有两条分别钉住"旗标不是标题"
与"顺序不决定归属"）。

**三条只有"真开一次实例 + 真浏览器"才暴露的缺陷**（2026-09-22 凌晨，实例跑在 58998、
配置目录搬到临时目录，AGENTS §10 那条；这三条打桩套件**全绿**）：

| 症状（用户在页面上看到什么） | 真因 | 修法 |
|---|---|---|
| 「编码」一行永远是「正在探测这台机器有没有硬件编码…」 | Tk 时代是**开窗口**那一刻去起探测的；网页只*读*状态，于是谁都不会去 spawn 那个问 ffmpeg 的探测。更绕的一面：当时 `capture.probed` 已经是 true —— **预览自己那一抓暖了采集缓存，却从不暖编码器缓存** | `request_probes()` 由 `mirror-state` 端点调，守卫就是缓存本身（`_capture_cache` + darwin 下还要 `_hw_encoder_cache`），页面一秒一次轮询因此不会反复 spawn |
| 「正在抓取第一帧…」也是一个永远走不完的句子 | 预览的 `<img>` **要有帧才存在于 DOM**，所以页面里没有任何一处会去要第一帧 | `request_preview()` 同样由状态端点补一次（后台线程），Part 35 另加 12 条钉住"空预览 = 还没人抓过" |
| 镜像进行中预览还在转，而且**什么都没有** | 镜像已经占住那块屏；第二路 avfoundation 采集 8 秒超时空手而归。而 `snapshot_frame()` 的 docstring 原本**明写着"镜像进行中也能抓"** —— 真实例当场证伪，那是文档层的一条假陈述 | 预览在镜像期间**主动站下来**，卡片写明原因（`MIRRORING_PREVIEW_NOTE`），docstring 改写成同一句话；停止后同一个缓存自己回到预览 |

**另外两件事不是缺陷，但要写清免得下一个人误判**：① 页面上 DLNA 电视报「没有发现」是
**诚实的**（这台局域网里唯一可投的是 192.168.1.13 那台 TCL，当时它睡了；本机压根没有 Chromecast）；
② `_kick_searches()` 的"两个协议一起搜 + 15 秒节流"**是 v0.10 就有的形状**，P7 没改它的判定，
只是把它的重要性说白了 —— 窗口是"打开才轮询"，网页是**标签页开着就每秒轮询**，
没有那个节流等于"开着页面把局域网打满"。

**中间踩到的那条设计红线值得单独记**：头一版把探测的 kick 放进了 `console_state()` 里，
套件立刻变成**非确定**的 —— 同一份工作树两次跑出 1072/1077 与 1086/1088（当时的工作树总数，
Part 35 还在写），红的还全是**别的** Part：Part 21/22 那两段假 ffmpeg 环境里，一个真后台线程
把假 ffmpeg 真的 spawn 了并写进 `_capture_cache`，于是"系统声音"两行读到的是探测结果而不是打桩值
（`can only concatenate str (not "dict")`），另一处 `capture.screens` 读到我塞进缓存的
`object()` 哨兵。搬回端点之后立刻可复现。
所以「**状态构造器永远是纯读，谁要渲染 spinner 谁负责让它落地**」现在是 Part 35 一条
按源码写法钉住的用例（"so it is the endpoint that asks, not the state builder"）。

**Part 35 整段重写为 115 条**（旧 Tk 那段的条数不再可比 —— 它们测的是已经不存在的那个窗口）：
派生层的排版规则（每种输出各出现/消失哪些卡片、Windows 没有可报的东西就没有音频卡、
未知 kind 也要能出一张页）、契约版本互指、菜单那道门（旗标归属、装载后恰好多一行、
插件答不上来是**少一行而不是死掉整个菜单**）、设备行带的是**回填用的键**不是标签、
搜索注释区分"搜过且空（带时间）/ 没能搜 / 正在搜"、三个端点的门控（含"query 与表体各带一个
令牌时只认调用方先说的那个"）、动作参数（畸形 JSON 与不是对象的值都在问插件之前被拒）、
预览缓存的时序（间隔内回缓存、失败退避、并发读等在途那一帧、状态里绝不漏字节），
最后三条是**反向守卫**：整个仓库（构建文件与应用模块）都不许再提到那个桌面窗口，
`.app` 依旧不带自己的 Tk。

**A/B 两轮**（都在恢复之后重跑确认过基线 **1187/1187** —— 那是 P7 交付时点的总数，此后每加一条用例它就往上走，下面引用的都是当时的数）：
① 删掉 `protocol.py` 里那行 `setting.request_preview()` → **1186/1187**。
红的**只有**那条钉写法的用例，行为组十一条全绿 —— 因为它们直接驱动 `ScreenMirrorSetting` 对象，
不经过端点。这不是用例设计缺陷而是分工：行为组测"预览这套机制对不对"，写法那条测
"页面真的会被喂第一帧"，删掉端点这一行时只有后者能红，说明它承担的就是这一件事（记在这里，
免得下一个人以为行为组应该红）。
② 删掉"镜像期间预览让位"那六行（三处）→ Part 35 三条红：
"while the mirror owns the display the preview takes no frame"、
"so a running mirror shows its own reason, not a stale picture presented as live"、
"the snapshot endpoint refuses on the same verdict, not in different words"。
同一轮还红了**两条与变异无关的**：一条 DLNA 看门狗用例（时序门控，恢复后单独跑全绿，
按 §2 那句"±2"如实记为 flake），以及 **Part 34 的来源台账** —— `our_lines` 从 34107 变成 34101。
后者是个真发现，但**结论当时写反了**：`provenance.py` 的一行台账混着两个时刻 ——
`ours` / `vendored` / 未改动的 `upstream` 读**磁盘**（未提交的行归属是 `0000…`，一律算"我们的"），
`mixed` 走 `git blame --line-porcelain HEAD`，读的是**已提交的那份 blob**。
`screen_mirror.py` 是 `ours`，所以删六行当场少六行；同一轮里 `macast/protocol.py` 这类 `mixed`
文件却对脏工作树完全无感。`docs/Provenance.md` §2 的判据（**先提交，再量，再改文档**）是对的，
脚本的 docstring 现在把这两条路径写清楚了。

**真实例复验（不是打桩）**：在浏览器里打开 `?page=13` 之后 —— 没有手动 `curl` 过任何
`mirror-snapshot`，预览卡片自己长出了一张 480×270 的真 PNG；起一路浏览器目标的镜像，
卡片换成「镜像进行中：画面正发给观看端，预览不再另开一路采集」且 `<img>` 不在；停掉之后
预览自己恢复。页面上的选择**真的落盘**（`Mirror_Quality='360'`、`Mirror_Output='browser'`
写进了那份临时配置），活动流如实记下开始与结束。

**产物侧**（`bash scripts/build_macos_arm.sh` → exit 0，arm64，43 MB）：
Tk 窗口属于"按路径加载、任何依赖扫描器都看不见"的那一族（AGENTS §4.4），所以删掉它之后
"随包"这件事换了问法 —— `BUILDING.md`「Verify the mirror console ships」现在查的是
包里的 `macast/xml/setting.html` 与 `macast/mirror_view.py` 两个文件在不在，然后**问运行中的产物**：
`curl -s http://127.0.0.1:58880/ | grep -c 电脑投屏`（回 0 是致命的：`Handler.__init__` 只在启动时
缓存模板，重启修不好，只能是文件没进包）。本轮实测：两个文件都在，`mirror_console.py` /
`config_window.py` 各 0 个；产物在临时 `HOME` 下真启动，监听 58880 + 8009，
`/api?query=status` 回 `0.7.15`，服务出去的页面里「电脑投屏」出现 6 次，
`logs/` 下 11 个模块日志 —— 认领动作就发生在插件导入那一刻（`logsplit.claim_from_module`，
AGENTS §4.10b），所以这 11 个文件证明的是"这 11 个插件在冻结的进程里真被 import 过"。
它**不等于**"15 个内置插件全起来了"（另外 4 个不写独立日志），后者是 `scripts/e2e_smoke.py`
问的问题，见 §6.1。
顺带**又实证了一次** AGENTS §4.2 那条：`kill -2` 杀掉菜单栏实例之后，那个 `--idle=yes` 的 mpv
留在场上（41 秒大，pid 16484），已手动收掉 —— 被反复重启的 Macast 确实会攒播放器。

**文档同步**（用户要求"边做边把文档捋顺、错的直接改"）：`BUILDING.md` 上述整节重写；
`docs/Casting-Suite.md` §0 删掉"需要一个会画 Tk 的 python"这条前置、§1 按新形状重写，
`mirror_console.log` 的排障入口改成 `logs/ScreenMirror.log`；`plugins/README.md` 的 v0.11 行与
三段网页控制台约束；`README.md` / `README_ZH.md` 改成"控制项在设置页的「电脑投屏」页签，
菜单栏只留开门与停止"并说明理由；`setting.html` 里「高级设置」那句提示原来指向
"菜单栏设置"和一个不存在的"窗口位置"运行时状态，改指「模块设置」页签；
`screen_mirror.py` 四条会念给用户听的文案不再说"窗口"。
`AGENTS.md` 按约定只读，所以它 §2 那句"当前 1141/1141"是**过期的**（现在 1191），
正确数字记在本节与 §6 台账里。

**仍未验证 / 边界要说清**：① 投屏链路本身一条都没碰过真电视，P1-P4 的"真机未验证"原样成立
（§4.9 那一族：我们不崩、日志也没 ERROR，只是发送端不认账）；② 这一页**只在这台机器自己的
浏览器里打得开**（令牌门控，loopback 也不例外），它是本机控制面，不是远程控制面；
③ 本轮最该传下去的一条：**Part 35 全绿不能替代真开一次实例** —— 上面三条缺陷没有一条是
套件抓到的，它们全都是"页面上那个 spinner 永远转不完"这种只在人眼里才成立的形状。

