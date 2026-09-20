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
| R1 | 镜像到 Chromecast / Google TV | ✅ Cast 协议 | 已有（`screen_mirror.py` LOAD）→ P0/P3 升级低延迟 |
| R2 | 镜像到 Apple TV / AirPlay 2 电视 | ✅ AirPlay 镜像 | ❌ 需要 FairPlay；**不做发送端**，接收端由 `airplay_mirror.py`(P5) 补 |
| R3 | 镜像到 DLNA/UPnP 智能电视（2012-2018 老电视） | ✅ | ❌ **完全空白 → P2 新增** |
| R4 | 镜像到 Roku / Fire TV | 厂商标称支持 ⚠️ 机制未证实 | P1「任意浏览器」路线覆盖 Fire TV/Roku 浏览器可用场景 |
| R5 | 多显示器选择 | ✅ 选屏 | 现插件仅默认屏 → **P1 补选择** |
| R6 | 光标显示/隐藏、鼠标高亮、缩放适配 | ✅ | 光标开关 → **P1**（`-capture_cursor` 已有开关位）；高亮不做 |
| R7 | 画质 Auto/720p/1080p、码率、编码器 | 有档位（4K 仅文件模式）⚠️ 具体 UI 未证实 | **P1 预设化**（含 4K/区域） |
| R8 | 系统声音，且「不想再装驱动」 | 需要装音频驱动 + 重启（评论吐槽点） | 我们已有 BlackHole 一键辅助 + 多输出聚合（**优于它**）；P0 复核 pyobjc 依赖 |
| R9 | 投本地文件（AVI/MKV/MOV/MP4/MP3…）+ 边转边投 | ✅ 且带播放列表 | ❌ **空白 → P4 新增 `cast_local_file.py`** |
| R10 | 字幕/音轨选择、音画同步延迟、字体样式 | ✅（2026 年评论：字幕坏） | P4 做「选轨 + 同步偏移」，样式不做 |
| R11 | 暂停/继续/拖动进度 | ✅ | P4（Cast MEDIA 命令已有底层） |
| R12 | 菜单栏启停、防休眠 | ✅（2.14 加了 keep-awake） | P1 补 `caffeinate` 防休眠 |
| R13 | 延迟 | 评论抱怨 ~5-10 s | **P3 Cast Streaming 目标 <500 ms**（真机待验） |
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
| **P1** | `screen_mirror` v0.4：目标=**浏览器**；多显示器选择；画质预设（Auto/720/1080/4K）；光标开关；`caffeinate` 防休眠；菜单/状态页显示 fps·码率·丢块 | fMP4(`frag_keyframe+empty_moov`) + init-segment 缓存 + 令牌门控的播放器页（MSE，1.5 s 超时退渐进式）+ 自动播放解锁 | **Part 22** | 低（全部复用已验证的采集/扇出）；iOS Safari 的 MSE 支持待实测 |
| **P2** | `screen_mirror` v0.5：目标=**DLNA 电视** | 假装有长度的 MPEG-PS HTTP（<2³¹、精确有界探测、PS padding）、64 KiB 绝对偏移环形缓冲+阻塞式按字节重连、stdlib SSDP/SOAP、`GetTransportInfo` 看门狗、5 档 profile、`transferMode/contentFeatures` 头 | **Part 23** | 中：**没有老电视可验**，只能拿 Macast 自己的 DLNA 接收端 + Kodi/upmpdcli 当替身；真实兼容矩阵必须标注「未验证」 |
| **P3** | `screen_mirror` v0.6：目标=**Chromecast 低延迟镜像**（Cast Streaming），失败自动回落 LOAD mpegts | LAUNCH `0F5096E8` + 残留 app 清理 + webrtc OFFER/ANSWER；不 connect 的 UDP；19 字节 RTP+Cast 头；**纯 Python AES-128-CTR**（无新依赖）；Annex-B AU 切分；RTCP SR（首帧立即发）；PLI/kickstart/在途 12 帧；视频优先（音频二期） | **Part 24** + `cast_streaming_probe.py` | **高**：作者自己没对真机验过，各家固件/代际差异未知；无手机时只能自证字节自洽（AGENTS §4.9 明确这不算证据） |
| **P4** | `cast_local_file` v0.1 | 本地文件/URL/播放列表 → Cast(含真 QUIT_APP)/DLNA；stdlib Range/206 静态服务；ffprobe copy-vs-transcode 启发式；音轨/字幕选择 + `AudioDelay`；只投系统声音的音频档（码率上限遵守 §2.6）；被抢占后的重连接看门狗 | **Part 25** | 低-中：DLNA 侧的 `SetAVTransportURI` 语义已有；Cast MEDIA 命令收发已有 |
| **P5** | `airplay_mirror` v.1（protocol 插件，`uses_ssdp=False`）+ §9 边界改写 | 监督 uxplay（`$UXPLAYRC` 生成 + 无窗口参数 + 日志解析连接/断开），一期让它自己开窗，二期尝试 `-vrtp/-artp → mpv` 统一渲染；`selfcheck` 里检查 uxplay 是否可用并给出安装指引 | **Part 26** | 中：macOS 无现成二进制（Homebrew 未证实）→ 必须**明确标注需要用户自备**，且 Apple 砍 Legacy 会静默失效 |
| **P6** | 文档与发布 | `docs/Casting-Suite.md` 用户指南（含每目标的首次设置流程）、`plugins/README.md` + `info.json` 条目（**两步提交：先插件文件，再指 SHA/version**）、AGENTS.md §4.8/§9 更新、复核 §4.8 里 pyobjc 依赖是否与「只用自带库」矛盾（`_create_aggregate` 用了 `Foundation`，而 `requirements/*.txt` 没有 pyobjc —— 要么去掉，要么按 §4.4 三处同步）、版本号两处 + tag | 5c 一致性 | 无（但 pyobjc 那条是**已知不一致**，必须给结论） |

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
| P0 规划 | ✅ 文档落地 | 待填 | 文档型改动 |
| P1 浏览器目标 + 采集预设 | ⏳ | | |
| P2 DLNA 电视目标 | ⏳ | | |
| P3 Cast Streaming | ⏳ | | |
| P4 本地文件/播放列表 | ⏳ | | |
| P5 AirPlay 镜像接收 | ⏳ | | |
| P6 文档/索引/发版 | ⏳ | | |
