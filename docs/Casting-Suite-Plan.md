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
**2026-09-23 又偏了一次**：预填不再是固定的 20 MiB，而是 `DLNA_PREFILL_SECONDS` 秒画面
按档位码率折算、夹在一个字节区间里（`dlna_prefill_bytes`）。上面那句「约 20-25 s 延迟是物理代价」
只对参考项目成立 —— 固定的**字节**预算在这几档码率下就是 27–37 秒，那部分延迟与"老电视能播"
无关，只是预算单位选错了。开始提示与状态行报的是折算出来的秒数。

**2026-10 第三次修这一格，这次修的是文档自己**：上一条写的是「`= 6` 秒、夹在 3–8 MiB」，
而代码里一直是 `DLNA_PREFILL_SECONDS = 4`、`DLNA_PREFILL_MIN_BYTES = 2 << 20`、
`DLNA_PREFILL_MAX_BYTES = 8 << 20`（AGENTS.md §4.8 与 Part 37/39/44 绑的都是这一组）。
同一趟把它从常量变成了**用户旋钮**：`Mirror_Dlna_Prefill`（1–8 秒，`dlna_prefill_seconds_setting()`
越界夹住而不是拒绝，因为拒绝的后果是一台不肯播的电视加上没有任何解释），默认仍是**实测过的 4**
而不是下限 —— 理由写在 `DLNA_PREFILL_MIN_SECONDS` 的注释里：MirrorCast 在真 Philips
43PFS5301（2016，非 Android）上预填约 20 MiB ≈ 20–25 秒且明说，所以"要得比我们下限更多"的
固件存在，而这台机器上没有那样的电视可以量出**我们的**下限在哪。量不出来的数就交给能量它的人，
代价印在旋钮旁边（`DLNA_PREFILL_HINT`：「这个数字就是看得见的延迟」）。
`dlna_prefill_bytes(profile)` 保持**一个位置参数**：Part 21/23/37/39 都用
`lambda profile: …` 的形状把它打桩掉，多一个参数就会在与打桩无关的地方炸成 TypeError。

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
  **2026-10 这一条整条作废**（原文保留，因为它是当时的事实）：加密改走操作系统的原生
  AES-128-CTR —— macOS CommonCrypto（`CCCryptorCreateWithMode` / `CCCryptorReset` +
  一次 `CCCryptorUpdate`）/ Windows bcrypt（`BCryptOpenAlgorithmProvider` +
  `BCryptEncrypt`，**chaining-mode 用 `ChainingModeCTR`**）/ Linux libcrypto
  （`EVP_CIPHER_CTX` + `EVP_EncryptInit_ex` 走 `EVP_aes_128_ctr`），全部经 `ctypes`，
  **一个新依赖都没有**（这是它能进第一批的原因：Part 30 的白名单不用动，
  三处打包配置也不用动）。纯 Python 的密钥流**降为最后的兜底**，启动时跑一次
  known-answer 自检（`_aes_known_answer`，四条向量含空输入），过了才认这个后端，
  并且后端**有名字**（`_NATIVE_AES = (name, make, crypt, release)`，`native_aes_name()`
  读它）—— 三个闭包叫 make/crypt/release 时长得一模一样，"某个 OS 后端加载了"
  是日志、页面提示和用例能做出的最强陈述，而那不够用。
  于是 `CAST_STREAM_MAX_BITRATE` 提到 **8 Mbps**（`CAST_STREAM_DEGRADED_BITRATE = 4.5 Mbps`
  只在降级时生效，`cast_stream_bitrate_cap()` 是唯一的判定点）。**8 Mbps 是我们选的数，
  不是量出来的**：Google 自己的 `kDefaultVideoMaxBitRate` 是 10 Mbps，Chrome 标签页投屏默认 5 Mbps。
  本机的余量：按真实链路形状（复用句柄 + 每帧重算 counter block + 41,667 字节的访问单元）
  **5,367 MB/s、每帧 7.8 µs**，纯 Python 同负载 1.35 MB/s ⇒ 约 4000×，对 8 Mbps 有约 5000× 余量。
  三条实现上的红线，都被量过：① **`CCCryptorReset` 每帧重播 IV，与新建一个 cryptor 逐字节相同**
  （frame id 0/1/2/3/7/255/256/65535/2³¹ 都验过），这才是复用句柄值得的原因（4,994 → 11,297 MB/s）；
  ② **一个访问单元只调一次 `Update`，绝不切块** —— 按 1,200 字节切会把 CommonCrypto 打到
  811 MB/s（ctypes 调用本身约 1.4 µs）；③ **每个后端的 `crypt` 都要有 `if not data: return b''`**，
  因为 `ctypes.create_string_buffer(0)` 给的是一个 1 字节缓冲、`.raw` 是 `b'\x00'`，
  空输入那条 KAT 会红。另外两条**故意不做**的：Windows 上"每 16 字节调一次 `BCryptEncrypt`
  当 ECB 拼 CTR"的退路**被否掉了**（一次调用比它要替代的那段 Python 密钥表还贵，
  那不是兜底，是戴着原生徽章的降级）；`libgcrypt.so.20` 从 Linux 那条链里**删掉了**
  （一个没法测的第四后端是负债不是兜底）。**macOS 上绝不去探 `libcrypto`**：
  `ctypes.CDLL('libcrypto.dylib')` 按裸 soname 加载会**直接把解释器 abort 掉**
  （SIGABRT，退出码 134），因为 CPython 自己已经载了一个不同版本的 libcrypto，
  扁平命名空间下的符号冲突是致命的。**还有一句诚实的话必须留在这里**：
  KAT **永远区分不出**"只加低 64 位计数器"与"整个 128 位级联加"—— 那要 2⁶⁴ 个块才现形。
  安全性靠的是 `frame_iv` 的布局（frame id 在字节 8..12，12..16 是零 ⇒ 每个访问单元有
  2³² 个块 = 68.7 GB 余量；要进位到固定的高半区，需要 `aesIvMask[8:16]` 落在距 2⁶⁴
  约 2,600 以内，每会话概率约 2⁻⁵¹）。那条自检是**"操作系统换了"的绊线**，不是安全论证。
- 编码器形状与参考实现差两处，且都是**实测出来的**：
  ① `-tune zerolatency` 已含 `bframes=0` 与 lookahead=0，所以不必再写 `bf=0`；
  ② x264 参数名是 `keyint` / `min_keyint` / `scenecut`（写 `i-frame-min`、`sc_threshold`
  只会打印一行 error 然后**静默忽略**）。另外 **`-aud` 是 AVOption，必须带值 `1`**，
  否则它把下一个选项吞成自己的值。
- **2026-10 又对齐了三处**（都是拿 Google 参考发送端 Chromium 标签页投屏那段
  `context->flags |= AV_CODEC_FLAG_LOW_DELAY; max_b_frames = 0; thread_type = FF_THREAD_SLICE;
  pix_fmt = YUV420P; framerate = {30,1}; bit_rate = rc_max_rate = target_bitrate;` 逐条比出来的）：
  ① `-flags +low_delay`；② `-thread_type slice`（**只加在 x264 上** —— VideoToolbox 报告
  **没有**线程能力，且 `-tune` / `-preset` / `-entropy` / `-forced-idr` 经确认对 videotoolbox
  **不存在**）；③ `rc_buffer_size = bitrate / 2`（`rate_caps()`，原来是 1 秒 —— 一秒的 VBV
  意味着允许攒一秒，而这一条通道的全部预算是 200 ms）。`-maxrate` **不在**那三处差异里，
  所以留在 1.5×。**`-flags +low_delay` 对 VideoToolbox 也安全**：上游 `videotoolboxenc.c`
  至今还在按它改行为（`c1dc2e2b7` 2025-09-10「ensure bitrate is set in low_delay mode」、
  `d87210745` 2025-09-06「allow low latency RC with HEVC」），说明它读这个 flag。
- **2026-10：VideoToolbox 要 1.5× 的线速码率**（`rate_target()`，`VT_BITRATE_MULT = 1.5`），
  因为 VT **实测把 `-b:v` 少给 30–33%**（要 4 Mbps 只出 2802 kbit）。这是保守不是画质差：
  同样 4 Mbps 目标下 VT 得 **91.47 VMAF**，libx264 ultrafast+zerolatency 是 **90.78 @ 4216 kbit**。
- **2026-10：`ENCODER_AUTO` 上面那段注释被证伪并改写了。** 它原来写「x264 ultrafast 撑不住
  Retina 桌面的实时帧率，用户报的『不开硬件编码几乎看不到画面』就是这个意思」——
  吞吐那一半**在我们发过的任何分辨率上都不成立**：本机 libx264 ultrafast+zerolatency 编**桌面**
  是 1080p **17.9×**、2560×1600 **10.5×**、3456×2234 **6.2×**、4K **5.65×** 实时。
  那位用户报的是另一件事（见 `NO_FRAME_SECONDS`：采集一帧都不返回，不是编码器跟不上）。
  真实的取舍是**延迟换 CPU**，两个轴方向相反：x264 ultrafast+zl 首字节 **41–49 ms**、
  约 **4.5 核**；`h264_videotoolbox` 首字节 **200.9–253.1 ms**、约 **0.2 核**。
  VT 那约 190 ms **不是我们忘了关的某个开关**：`-realtime`、`-prio_speed`、`+constant_bit_rate`、
  `+max_ref_frames 1`、`-bf 0`、`-coder cabac` 逐个试过，没有一个能挪动它
  （而 `-realtime 1` 与不传它产出**逐字节相同**——它只是选低延迟码控路径，在这里买不到延迟）。
  它是硬件流水线自己的深度。所以 `auto` 仍然落在硬件（笔记本上 4.5 核是风扇和电池，
  而 190 ms 不是多数人说「投屏卡」时指的东西），但它是**默认值不是判决**：
  页面必须把这笔账说出来（`ENCODER_TRADEOFF` → `_capture_state()['encoder_note']` →
  「采集」卡上那行提示），否则那个开关读起来就是「硬件=好，软件=差」，没人会去关它。
  **没有加第四档 `lowlatency`**（原计划里有）：`encoder_args('software')` 已经就是
  x264 `ultrafast` + `zerolatency`，再加一档是两个菜单项做同一件事。
- **2026-10-02：上一条自己也被更正了一半 —— 它引的是**离线**吞吐，而镜像的成本要按**在线**量。**
  本机重测两遍（`scripts/encoder_latency_probe.py` 量在线、`testsrc2` 文件源 + `-f null` 量离线）：
  离线那栏是 486 / 295 / 171 fps 对 4.20 / 4.63 / 4.94 核，与上条的 536.9 / 315.9 / 185.3 逐行对得上；
  但**在线**（真实 24 fps 采集）同一条 argv 在 1080p 只吃 **0.59 核**，VideoToolbox **0.41 核**
  （三次跑），2160p 是 1.12 核对硬件 HEVC 的 0.64 核。所以"约 4.5 核"是**天花板不是成本**，
  真实取舍是 **约 200 ms 首帧 换 0.18 个核**，上条那句"笔记本上 4.5 核是风扇和电池"**因此不成立**：
  硬件默认看起来便宜二十倍，实际便宜不到一半。`auto` 仍然落在硬件，但这已经是一个
  **理由变弱了的默认值**，翻不翻是用户的决定，所以没在代码里替用户翻；
  `ENCODER_TRADEOFF` 同步改成引**在线**数，并注明两边都含约 0.5 秒 ffmpeg 启动所以要看差值
  （否则用户拿秒表量到 700 ms 会以为页面在骗他）。上条把"用户报的看不到画面"归给
  `NO_FRAME_SECONDS` 那一路是对的，但**不完整**：同一趟量出**两个各自独立的零帧成因**，
  都只在软件路径上现形、都把读者送去屏幕录制授权那个**真实却不是他们需要的门** ——
  ① `-level 42` 曾写死给 `h264_videotoolbox`，画面大于 level 4.2 的 8704 宏块上限就退出 **187**、
  只写约 2 KB（原画 3456×2234 与 2160p 各复现两次，换 `testsrc2` 文件源照旧 ⇒ 与采集无关），
  而 `auto` 在 Mac 上就是硬件 ⇒ **原画档出厂即"什么都不产出"**；修成 `vt_level(height)`
  按将被编码的高度查表、查不到就不钉（VT 自己在 1080p 选 level 4.0，比我们钉的 4.2 更保守），
  caststream 仍钉 42 是对的（那里 OFFER 已经把画面钉成 1920×1080，level 是承诺不是猜测）。
  ② 直播形状曾没有输出 `-r`：采集在 `Configuration of video device failed, falling back to default`
  之后时间基退化到无法估帧率，x264 于是算出 `MB rate (14400000000) > level limit (16711680)`
  而**一个字节都不编**（VideoToolbox 容忍同一个畸形输入，所以只在软件回落时现形）；
  单变量实测 **0 字节 → 3,932,160 字节**，修成每条直播形状都钉 `-r FPS`
  （`build_dlna_command` 一直钉 `-r profile.fps`，所以 DLNA 从没中过这一枪）。
- **2026-10-02：三条量完不做/推迟的**（数在 `docs/research-screen-mirroring-encoder-2026-10.md` §2.3b）。
  ① **`mpdecimate` + `-fps_mode vfr` 不采纳**：收益是**带宽不是延迟**（首帧 +3.9/+12.9/+14.2 ms ≈ 免费，
  字节 **−26～−28%**，核 0.52 对 0.59），而那是静止桌面的最好情况、放视频时几乎不掉；
  两个碰撞让它进不来 —— **`-r` 与 `-fps_mode vfr` 互斥**（CFR 会把丢掉的帧原样复制回来，
  而 `-r` 正是上面②那条零帧 bug 的修法），以及 DLNA"伪装成文件"的 `Content-Length`
  由名义码率推出、少产字节会抽干电视的缓冲。
  ② **多编解码 OFFER（H.264 + HEVC）推迟到第 3 批**：`hevc_videotoolbox` 对 `h264_videotoolbox`
  三次跑的首帧差是 −33/+54/−7 ms ⇒ **不可分辨**，字节小 11–13%、核相同；2160p 上 HEVC 反而
  **多产字节**并掉到 **0.82×（跟不上了）**。软件 HEVC 直接关掉这个问题：libx265 ultrafast
  首帧 +42 ms、**1.67 核**（x264 的 2.8 倍、VT 的 4 倍）而字节只省 1.3%（2160p 4.17 核）。
  ⇒ HEVC 买的是带宽，而编解码协商在第 3 批（WebRTC）本来就要做，放一起谈。
  ③ **顺带量到一条独立确认**：拿掉 `-tune zerolatency` 的代价是首帧 **+743～+910 ms** ——
  这张表里最大的一项延迟，也就是说插件早就在做的那个选择是对的。
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
- **2026-10-02：上一条被真浏览器探针推翻了一半 —— 分片节奏确实是延迟，而且是最贵的一段。**
  v0.18 从"上一条"推出一句结论：「分片节奏是 `FPS // 2` = 0.5 秒，所以 `+frag_every_frame`
  买不到东西」；v0.19 用 `scripts/mse_latency_probe.py`（真采集 + 硬件编码 + 无头 Chromium
  当观看端，A/B 打的就是**同一棵树改动前的那份代码**、同一个 `-g 12`）量到：只把 `movflags`
  从 `frag_keyframe` 换成 `frag_every_frame`，端到端 lag p50 **1305 → 838 毫秒（−36%）**，
  画质同值（VMAF 94.42 对 94.42），字节只多约 1%。也就是约 470 毫秒的延迟正好住在
  "等一个 GOP 长的片段封完"里。连带三条：停在直播边缘后 0.5 秒对 1 秒只差 9 毫秒
  （829 / 838 ⇒ `Mirror_Live_Edge` 从"延迟旋钮"变成"恢复距离"）；`-g 2` 单独否掉
  （码控被抽干，93.9 → 81.1，不值得）；`-g 12` 保留但语义变了 —— 它现在是**恢复粒度**
  （重播割点最多往回退这么远才遇到一个可播画面），不再是发布节奏。
  代价是一条新契约：分片不再保证以 IDR 开头，所以晚加入者的重播要逐片做真关键帧检查
  （`starts_with_keyframe`，读不准一律当"不是"），一个可播起点都没有时**不从尾部喂旧字节**
  （计入 `keyframe_misses` 并 warning 一次），而"每片都 entrable"的旧形状（marker 回退路径）
  照旧整环重播 —— 这一条是被 Part 22 的旧契约当场抓出来的（第一版把回退环也割短了）。
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

因为**采集与系统音频那 600 行只有一份**：avfoundation、Windows 的 ddagrab+gdigrab、x11grab 探测、BlackHole 探测与
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
| **P1** ✅ | `screen_mirror` v0.4：目标=**浏览器**；多显示器选择；画质四档（**360 / 720 默认 / 1080 / 原始分辨率**，计划里的「4K」并入「原始分辨率」—— 采集高度由 `avfoundation` 给，缩放档位没有意义）；光标开关；macOS **VideoToolbox 硬件编码**（先探测再允许）；`caffeinate` 防休眠；菜单状态页显示 时长·码率·观看端·丢块 | fMP4(`frag_keyframe+empty_moov`；**v0.19 起 `frag_every_frame`**，见 §2.3 的更正条目) + init-segment 缓存 + **每会话** token 门控的播放器页（MSE，1.5 s 超时退渐进式）+ 自动播放解锁 | **Part 22**（69 条） | 低（全部复用已验证的采集/扇出）；iOS Safari 的 MSE 支持待实测 |
| **P2** ✅ | `screen_mirror` v0.5：目标=**DLNA 电视** | 假装有长度的直播 HTTP：对外 `Content-Length` = 按档位码率算出的固定值且 **< 2³¹**（`DLNA_MAX_ADVERTISED_SIZE = 1.9e9`）、探测请求**恰好回 n 字节**（不足补 MPEG-PS 填充包）、`Accept-Ranges` + `transferMode.dlna.org: Streaming` + `contentFeatures.dlna.org`、**48 MiB**（`DLNA_RING_BYTES`，计划里的 64 KiB 太小：按 4.5 Mbps 只有 0.1 秒余量）按绝对字节偏移的阻塞式重连 + 20 MiB 预填（`DLNA_PREFILL_BYTES` ⇒ 菜单明说的 ~35 s 延迟）、stdlib SSDP/SOAP（`urllib`，不打第三方）、`GetTransportInfo` 看门狗 + `RelTime` 前进才算活着、**5 档 profile**（ps-pal / ps-ntsc / ts-mpeg2 / ts-h264 / mkv-h264，PAL/NTSC 用 AC-3）、连续失败自动换档并在用尽后提示手选 | **Part 23**（93 条） | 中：**没有老电视可验**，只能拿 Macast 自己的 DLNA 接收端当替身；真实兼容矩阵必须标注「未验证」 |
| **P3** | `screen_mirror` v0.6：目标=**Chromecast 低延迟镜像**（Cast Streaming），失败自动回落 LOAD mpegts | LAUNCH `0F5096E8` + 残留 app 清理 + webrtc OFFER/ANSWER；不 connect 的 UDP；19 字节 RTP+Cast 头；**OS 原生 AES-128-CTR**（ctypes，无新依赖；2026-10 前是纯 Python，见 §2.2 末的偏差段）；Annex-B AU 切分；RTCP SR（首帧立即发）；PLI/kickstart/**在途窗口按时长算**（`clamp(2×RTT, 66 ms, targetDelay/3)` + 120 帧硬顶；2026-10 前是固定 12 帧 = 24 fps 下 500 ms 排队）；视频优先（音频二期） | **Part 24** + `cast_streaming_probe.py` | **高**：作者自己没对真机验过，各家固件/代际差异未知；无手机时只能自证字节自洽（AGENTS §4.9 明确这不算证据） |
| **P4** ✅ | `cast_local_file` v0.1 | 本地文件/URL/播放列表 → Cast(含真 QUIT_APP)/DLNA；stdlib Range/206 静态服务；ffprobe copy-vs-transcode 启发式；音轨/字幕选择 + `AudioDelay`；只投系统声音的音频档（码率上限遵守 §2.6）；被抢占后的重连接看门狗。**偏差见 §2.6 末**（含"这一阶段动了 `protocol_cast.py` 的会话账本"） | **Part 25**（155 条） | 低-中：DLNA 侧的 `SetAVTransportURI` 语义已有；Cast MEDIA 命令收发已有；**真机一台没验** |
| **P5** | `airplay_mirror` v0.1（protocol 插件，`uses_ssdp=False`）+ §9 边界改写 + `selfcheck` uxplay 探测 | 监督 uxplay：**`-rc <file>` 生成在 Macast 自己的配置目录**（不是 `$UXPLAYRC`，它静默回落 `~/.uxplayrc`）、**不带 `-p`**（legacy 7000 与自家 AirPlay 接收端撞车）、默认 `vsync no` + macOS `vs osxvideosink`、**stdout 挂 pty** 才拿得到即时事件（C 侧块缓冲）、日志解析 连接/断开/被拒/mDNS 失败/自行退出、不映射 DLNA 播放状态、与 `raop.py` 同名竞争只提示一次。一期让它自己开窗，二期再试 `-vrtp/-artp → mpv`。**偏差见 §2.5 末** | **Part 26**（43 条）+ Part 24 两条（teardown 先排空再挂断） | 中：macOS **确认**没有现成二进制（无 formula、release 只有 spec/PKGBUILD）→ 必须**明确标注需要用户自备**，且 Apple 砍 Legacy 会静默失效 |
| **P6** | 文档与发布 | `docs/Casting-Suite.md` 用户指南（含每目标的首次设置流程）、`plugins/README.md` + `info.json` 条目（**两步提交：先插件文件，再指 SHA/version**）、AGENTS.md §4.8/§9 更新、复核 §4.8 里 pyobjc 依赖是否与「只用自带库」矛盾（`_create_aggregate` 用了 `Foundation`，而 `requirements/*.txt` 没有 pyobjc —— 要么去掉，要么按 §4.4 三处同步）、**`scripts/selfcheck.py` 补齐 §3.2 承诺的随行项**（P1-P4 都没动它：现在只报「ffmpeg 在不在」，缺 编码器能力 / ffprobe 在不在 / 系统音频采集口 / DLNA 渲染器与 Chromecast 探测 / 转码临时目录剩余空间）、**复核 `screen_mirror._avfoundation_lists` 的引号解析**（本机真实 `ffmpeg -list_devices` 输出是 `AVFoundation audio devices:` + 不带引号的 `[0] 名称`，P4 已按这份实测重写了 `cast_local_file` 的解析，那条老路径要用同一条实测输出重验）、版本号两处 + tag | 5c 一致性 | 无（但 pyobjc 与 avfoundation 解析这两条是**已知不一致**，必须给结论） |

**每阶段完成后立即 `git push git@github.com:pingod/Macast.git main`**，并在 §6 记录 commit。

---

## 5. 明确不做

- FairPlay 破解、任何 DRM 内容（Apple TV 应用/Netflix 镜像出来是黑屏，UI 要提前说明）。
- Miracast / Wi-Fi Display（macOS 无 P2P API；`docs/reference-projects-review.md` §1 已定论）。
- 窗口级/单 App 级捕获。**这句的理由已经换过一次**：原文写的是「avfoundation 无此输入源，
  需要 ScreenCaptureKit 原生桥 = 新构建链」，而 v0.21 已经把 ScreenCaptureKit 接进来了
  （pyobjc，与 CoreAudio 同一套 ctypes/pyobjc 习惯，没有新构建链），所以"做不到"不成立了。
  它仍然不做，理由是**另一件事**：整屏镜像的语义是"这块屏"，窗口级要引入窗口选择器、
  窗口消失时的会话语义、以及每窗口帧率门控 —— 那是一张新 UI 而不是一根新管道，本规划没有排期。
- ~~真 WebRTC 发送到浏览器（`aiortc` 不是自带库；单文件插件路线禁止 pip）~~
  **这一条已被 v0.22 推翻，推翻的方式正是它警告的那个前提**：`aiortc` 不能进**单文件插件**路线，
  但 `screen_mirror` 走的是内置插件路线（§4.4 三处打包配置），所以两个可选 pip 包（aiortc、av）
  是允许的代价。留着这一行是为了记住判定的形状：**"禁止 pip"约束的是插件形态，不是这个仓库的能力上限**。
- AirPlay **发送端**（推给 Apple TV）：需要 pyatv（pip）→ 若将来要做只能走内置插件 + §4.4 三处打包，
  本文暂不排期；macOS 系统自带「屏幕镜像」已经覆盖这个场景。
- Sonos、多房间、麦克风直通、全局热键、HDR、鼠标点击高亮。
- 搬任何 PolyForm Noncommercial（MirrorCast）代码 —— 只实现同一协议事实。
- **网页地址投屏**（P8）一侧：需要 **JavaScript 才算得出地址**的站点（那要每个页面多养一个浏览器
  进程 = 二阶段的决定，已把覆盖空洞写在卡片上而不是藏在手册里）·
  把 `/relay/<随机 id>/media` 链接**当保密链接**（它故意只有随机 id + 空闲 TTL，没有令牌，
  转发出去拿到的人就能读这路流）· **代用户登录**（这里做的是"把用户自己粘进来的 cookie 存好、
  交给引擎"，不是弹一个表单替他填账号密码 —— Macast 从不登录任何站点）。
  **同一栏里有两条在 v0.19 被用户自己撤销了，别再抄回去**：DASH 音画**合并**（v0.18 只标注
  「这条没有音轨」，因为中转当时没有第二条可合；现在 `plan()` 会答 `merge` = 带第二条输入的
  remux）、**「选一台设备投过去」**（v0.18 一律交给**当前渲染器**；现在卡片上有下拉，第一项
  永远是本机，其余复用 `cast_local_file` 的发现与投递，**没有一行新的协议代码**）。
  留在"不做"那一侧的相关边界只剩两条：合并后**不做音轨切换**（一个视频行只配一条音频行，
  按码率挑）、以及上面那句"不代登录"。**把这两件留在"不做"清单里是一句过期的谎**，与
  §4.8「提示比代码活得久」同族。

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
| P8 网页地址投屏（**本规划之外的一条功能，2026-10-03 由用户直接提出**） | ✅ 已交付（**真站解析验过一次；真电视一台都没碰过**） | 单个 `feat+release` 提交，带 tag `v0.18.0`（应用版本两处 0.17.0 → 0.18.0）：新增 `macast/media_resolve.py`（解析层，纯 stdlib）+ `macast/media_relay.py`（中转层）+ `macast/protocol.py` 两个 POST（`resolve-page` / `cast-resolved`，都是 `GATE_MANAGEMENT`）与一个 GET（`query=resolve-status`）+ `macast/xml/setting.html`「状态」tab 的新卡片与帮助段 + **Part 59（132 条）**与 Part 44 里把帮助那几句绑回三个文件的用例 + 文档（`docs/Casting-Suite.md` §2b、AGENTS §3/§4.14/§6/§9、本文 §5/§6） | 见下面的 §6.5 —— 取证（castor 精读 + bilibili 真站）、七个定点变异体 1/1/1/3/1/2/2、以及一次**测试自己**的崩（`check()` 的 detail 传成 `None` ⇒ Part 59 后半 8 条根本没跑） |
| P8.1 网页地址投屏第二轮（**同一张卡片上的四件事，2026-10-04 由用户提出并逐项拍板**） | ✅ 代码 + 用例 + 文档已交付（**三件验证仍欠着**：merge 的真站复验、设备下拉与 cookie 那块的三档真浏览器验收、投给一台真电视） | 单个 `feat+release` 提交，带 tag `v0.19.0`（应用版本两处 0.18.0 → 0.19.0）：① `screen_mirror` 的 `MENU_HIDDEN` —— 菜单栏 Renderers 组里不再有「电脑投屏」那一行（控制面在 P7 就搬进网页了，见 §6.4；已持久化成 `Macast_Renderer = Screen Mirror` 的安装仍然按名字命中，`set_media_url` 的 Bridge 让位照旧）；② `plan()` 新增第三种判决 `merge` + `pair_tracks()`（DASH 的画面行配上声音行，第二条输入**带自己的请求头**）；③ `visible()` 把"度量过、是活动画面、既没有声音也没有可配的声音地址"的行**收起**并在旁边报出条数（**先合并、后过滤**是用户钉的顺序，反过来 bilibili 那张卡片是空的）；④ `looks_like_url` / `extract_share_urls`（整段分享文案）、`engine_note()`（引擎拒绝原话第一行 + `needs_cookies` 词表）、cookie jar（`resolve_cookies.txt`，0600，只在门控内读写，页面只报条数）、`_cast_targets()` + `cast_local_file.target_push`（设备下拉，第一项永远是本机）+ `macast/xml/setting.html` 同一张卡片新增的两块 + **Part 59 132→174 条**（新 R 段）+ Part 36 的 13 条菜单栏用例 + 文档（`docs/Casting-Suite.md` §2b/§5/§6、AGENTS §2/§3/§4.14/§6/§9、本文 §5/§6.5/§6.6） | 见下面的 §6.6 —— 十七个定点变异体（v0.18 七个 1/1/1/3/1/2/2 + v0.19 十个 1/3/1/4/1/1/2/7/2/2），两轮"0 红"的自我修正（`_local_target()` 的名字写死那一版第一版用例是**自证**的），以及**写文档时抓到的一条真 bug**（cookie jar 把 `#HttpOnly_` 那一行读成注释） |
| P9 包体瘦身（**WebRTC 的五个可选包移出默认产物**，2026-10-04 由用户提出并选定形状） | ✅ 代码 + 用例 + 构建脚本已交付，随 **v0.20.0**（**两处真机验收仍欠着**：Mac 的打包 `.app` 里按一次按钮；`.68` 装一次并重跑跨机 WebRTC —— 后者要用户点头。**CI 还没发布过任何一个 extras 资产**，所以"别人拉得到吗"这一问本轮只能答到"仓库是公开的、匿名 Range 请求拿得到字节"这一层。**页面那一半今天走过了**（临时实例 + `meta_path` 藏包，28 项判定、三档截图），它逼出一条产品缺陷：卡片在成功收尾时显示的仍是**最后一步的标签**，真正的新闻只活在一次性通知里 —— 现已由 `_SetupProgress.finish(ok, message)` 把答案写进卡片，移除另起一条自己的两步机，回归用例 **Part 60/C2（9 条）**只问卡片不问函数返回值） | 单个 `feat+release` 提交（应用版本两处 0.19.0 → 0.20.0）：新增 `scripts/build_webrtc_extras.py`（四平台各打一个 extras zip：`manifest.json` 在根，zip 内**只有清单列出的那些文件**、没有目录条目）；`.github/workflows/build.yml` 四个平台 job 各加一次 extras 构建与一个 Release 资产上传，三个 PyInstaller job 的 pip 列表与 `--hidden-import` 摘掉 `aiortc` / `av` / `cryptography` / `pylibsrtp` / `cffi`；`scripts/setup_py2app.py` 的 `packages` / `includes` 同步摘掉，`av` 那条"整目录拷贝才让 `@loader_path` 原样解析"的说明改写成"现在随 extras zip 走"；`macast/plugin_repo.py` 的 `EXTRAS_PACKAGES` / `extras_asset_name()`（**四个孔**：os、arch、cpython tag、版本）/ `extras_urls()`（镜像在前、直链兜底、去重）/ `extras_describe()`；`screen_mirror` 的应用内安装器（`extras_abi` / `extras_asset_name` / `extras_dir` / `extras_manifest` / `check_manifest` / `install_webrtc_extras` / `uninstall_webrtc_extras` / `extras_state` / `start_extras_install`，五步 download → verify → unpack → land → probe，落点 `SETTING_DIR/webrtc_extras/<os>-<arch>-<pytag>`，`sys.path` **插到最前面**而不是追加）；`protocol.py` 两条 `POST_ROUTES`（`install-webrtc-extras` / `uninstall-webrtc-extras`，门 `GATE_CODE` —— 它落的是**能被 import 的代码**，所以本机不算数，见 AGENTS §4.7b）；`mirror_view.py` 的 `extras` 卡与 `extras_step_marks`（`VIEW_VERSION` = 5）；`macast/xml/setting.html` 的「WebRTC 依赖」卡与两个按钮 + **Part 60（A / B / C / C2 / D 五段，65 条）**与 Part 56/D 四条断言翻面 | 见下面的 §6.7 —— 全套 **2305/2311**（红的 6 条是 Part 34 的来源台账，本克隆缺 fork 点对象，见 AGENTS §4.12）；同一份代码在 Linux 容器里 `--ci` **2304/2310 通过**，红集同样是那 6 条（总数差 1 是**构成**差：Part 19 的 8443 在这台 Mac 上被用户实例占着、Part 37 有一段按声音路由表参数化，见 AGENTS §2），`pyflakes` 干净；构建脚本真跑过一次，产物 `Macast-WebRTC-extras-macos-arm64-cp312-v0.20.0.zip` = **26,884,928 字节**，它自己的清单自检要求 `macast_version` 由调用方告知（`version=` 那道接缝，Part 60/B 两条用例分别问"没被告知时拒绝、且把两边值都念出来"与"被告知时接受"）。**同一天翻掉的一条设计前提**：§6.7 原本写着「私有仓库 ⇒ 那个按钮在别人机器上注定 404」，实测**仓库是公开的**（四条匿名探针，外加对 v0.19.0 资产的匿名 `Range: bytes=0-1023` 拿到 **206 / 1024 字节**），所以那一半代价不存在，卡片上也刻意没有为它写道歉文案 |

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

> **2026-09-23 更新**：那两行也删了 —— 所有者定「电脑投屏功能就不要放菜单了」。
> 出口这个顾虑由别的行接住：菜单栏「停止接受投屏」停掉整个服务、
> 「设置 → 选择播放器」切走渲染器，两者都触发 `ScreenMirrorRenderer.stop()`，
> 与页签上「停止镜像」是同一场清理。同一轮还修回了一个更严重的回归：
> `224d6d6` 把构造托盘应用时的 `build_app_menu()` 换成了 `[]`，
> 于是**状态栏图标整个菜单都没有了**（不是少两行）—— 而当时没有任何用例建过菜单，
> 所以它一路发版全绿。现在 Part 36 就是干这个的。

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
`AGENTS.md` 按当时的约定只读，所以它 §2 那句"当前 1141/1141"是**过期的**（现在 1191），
正确数字记在本节与 §6 台账里。**那句约定作废**（2026-09-23 所有者授权改）：AGENTS.md 里被证伪的
陈述要就地改、保留理由与历史，见下面的 §6.5 —— 它 §2 与 §6 的计数行从 v0.18.0 这一批起直接跟着更新。

**仍未验证 / 边界要说清**：① 投屏链路本身一条都没碰过真电视，P1-P4 的"真机未验证"原样成立
（§4.9 那一族：我们不崩、日志也没 ERROR，只是发送端不认账）；② 这一页**只在这台机器自己的
浏览器里打得开**（令牌门控，loopback 也不例外），它是本机控制面，不是远程控制面；
③ 本轮最该传下去的一条：**Part 35 全绿不能替代真开一次实例** —— 上面三条缺陷没有一条是
套件抓到的，它们全都是"页面上那个 spinner 永远转不完"这种只在人眼里才成立的形状。


### 6.5 P8 明细（应用 v0.18.0，tag `v0.18.0`）：网页地址投屏

**这条功能不在原规划里**，是 2026-10-03 用户直接提的：「用户粘贴网页地址后，应用自动解析页面里的
视频地址，而后投屏」。动手前先做了 castor（`stupside/castor`，MIT）的代码级精读，然后**三项决定由用户
拍板**（原话「确认这三点，开始写代码」）：

1. **引擎 = 两路合并**：`yt-dlp -J`（覆盖上千站点，本机已装，仓库早有使用这个外部命令的先例）+
   stdlib 抓页面直链（`<video src>` / `<source>` / `og:video` / JSON-LD）→ 一张候选表，
   **一律由 Macast 侧中转供流**；不引 castor 依赖。必须 JS 才算得出地址的站点留作二阶段（CDP），
   而**这个覆盖空洞要写在页面上**。
2. **目标 = 当前渲染器**：不做"选一台设备"，交出去走现成的 `_cast_url`。
   **这一条在下一轮被用户自己撤销**（2026-10-04，「投给哪台设备要能选」）：现在卡片上有下拉，
   第一项永远是本机、其余复用 `cast_local_file` 已有的发现与投递，仍然**没有一行新的协议代码**。
   留在原地的理由是当时那句判定本身没错 —— **"不做设备选择"约束的是"要不要新写一条投递路径"，
   而不是"页面上能不能出现一行设备名"**；复用现成插件的缓存清单，代价是零。见下面的 §6.6。
3. **供流 = 代理优先、只在需要时转封装**：直链 = 带请求头注入的反向转发 + **Range 翻译**（零 CPU，
   观众能拖）；HLS/DASH = `ffmpeg -c copy` 转封装成**会增长的临时文件**（拖动只能用满才准，
   这句代价印在卡片上）。

外加两条同一批确认的边界：**做核心模块而不是插件**（§4.8 的单文件插件不许 pip，而这里要用的是
**外部命令** `yt-dlp` / `ffprobe`，两者都按 PATH + 常见目录在**每次用的时候**找，所以装上 yt-dlp
不需要重启应用）；**中转门只要随机 id + TTL，故意不要令牌**（那条链接天生是给电视用的，而
`/relay/<id>/media` 没有管理面可开放 —— 代价写在页面上：它不是保密通道）。

**取证落在四处判定上**（都不是试错试出来的）：

- **`-J` 而不是 `-g`**：`-g` 只印一条地址并且**把 `http_headers` 丢掉**，而那些头正是这条地址可读的
  原因；JSON 还带着整条清晰度梯子，`rank()` 才有东西可排。
- **判定表有序、first-match**（`ADMISSION_TABLE` 六行）：「源站拒绝」与「静态图当视频轨」要**丢**，
  「探针没问出来」与「只有头没有体」要**留但说清**（`last_resort`）。丢掉的代价变成页面上的
  `rejected` 数字，用户靠它区分「这个站解不出」与「解出来了但都不行」—— 这两句话在卡片上是两句
  不同的话，把它们合并就是这条功能最没用的形态。
- **排序与 castor 故意相反：直链排在清单之前**。castor 把 master playlist 排前面，是因为梯子是它那个
  引擎的**回退信号**；而我们自己供字节，直链上能翻译 Range（能拖），清单只能转封装成"一直在变大的文件"。
  改顺序就必须同时改这两句话 —— 所以这一条写在 AGENTS §4.14 的红线表里。
- **两批集合是从 castor 逐字搬的**：`STILL_IMAGE_CODECS`（ffprobe 会把海报报成视频轨的那批 codec）与
  拒绝码 `401/403/404/410`。后者在两个模块里各有一份写法（一个正则、一个元组），Part 59 把它们**互相
  钉住**；只匹配文字（`Server returned 403 Forbidden`）会把 500 也读成拒绝。

**真实取证改变了产品**（这是本轮唯一一次"跑真站教了一课"）：bilibili 一次解析 6.5 秒走完四步，
它的 DASH 梯子把**画面一条地址、声音另一条地址**分开交回来 —— 于是页面上最高的那一条候选投出去
**没有声音**，而中转合并不了（没有第二条可合）。结果是卡片挂「这条没有音轨」标签，
`describe()` 补出 `video_codec` / `audio_codec` 两个键，那个 `v-if` **两个操作数都要在**
（只写 `!c.audio_codec` 会把纯音频那条 —— `relay_reason()` 自己就明说那是有声投屏 —— 标成坏视频）。
这与 §4.8「价格写在会付它的那个人眼前」同族。

**验证**：Part 59 **132 条** + Part 44 新增 **7 条**（帮助那几句绑回三个文件），套件
2033/2039 → **2172/2178**（红的仍是 §4.12 那 6 条 Part 34）。分段判据是"这个事实住在哪一层，
以及它**碰不碰网络与子进程**"：A–H 全喂 fixture（两个解析器 + 两张表），I 是页面轮的 job
（另有一条在 PATH 上放假 `yt-dlp` 与假 `ffprobe` 跑完的端到端），J–N 是中转（含**真监听 socket
上的 HTTP 语义**），O–P 是 handler 接线，Q 是页面的文本契约。七个定点变异体各自红 1/1/1/3/1/2/2 条
（`advertise_host` 抄回 `get_advertisable_ip` / 空结果不拦 / 死中转不摘 / `plan()` 契约漂 /
卡片编一个模板键 / 卡片用 `v-html` / 无声标签少一个操作数），每个跑完整套件、从 /tmp 备份还原、
md5 逐字节校验（**绝不用 `git restore`**）。
**最后一个变异体顺带抓到测试自己的 bug**：新用例把 `check()` 的 detail 传成 `None`，
而 `check()` 在判红时要做 `"  -- " + detail` 的字符串拼接 ⇒ **Part 59 后半 8 条根本没跑**，
红法长得像"整段崩了"（2161/2170）而不是"这一条错了"（2170/2178，两条红各自落在自己那句话上）。
这是 §4.2「测试自己的实现也是实现」的第 N 次现身，判据现在写进 AGENTS §4.14 的测试面那一段。
页面侧另按 §10 用**真实例 + 真浏览器**三档宽度看过卡片（打桩套件从不渲染 DOM）。

**未验证 / 边界要说清**：① **真 Chromecast、真老电视一台都没碰过**，中转出去的字节只在自家假设备与
自家接收端上验过（§4.9 那一族的边界原样适用：我们不崩、日志也没 ERROR，只是发送端可能不认账）；
② 真站解析**只跑过一次**（bilibili），它证明的是这条链路在这台机器上通，不是"上千站点都解得出"；
③ **需要 JavaScript 才算得出地址的站点解不出**，这句写在卡片上而不是藏在手册里；④ 中转链接**不是
保密通道**（随机 id + 30 分钟空闲 TTL，无令牌）；⑤ DASH 音画分两条时**只标注不合并** —— 这一条
在 v0.19 已被推翻（`plan()` 现在会答 `merge`），见 §6.6；当时的取证（"页面上最高那条投出去没有声音"）
仍然是对的，改变的是我们对它的回应方式。

**署名（AGENTS §4.12 那一族）**：两个新文件都是**我们写的**（文件头已是 pingod），但 castor 的 MIT 层
只落在 `macast/media_resolve.py` 上 —— 它的头点名 `https://github.com/stupside/castor`（MIT）与
`internal/source/rank/`，说清判定表与偏好序是 ported 的、`STILL_IMAGE_CODECS` 是逐字搬的，
并把两处**主动偏离**写在同一处（没有它的"不足五分钟就不投"运行时门槛，以及**排序与它相反**）；
`macast/media_relay.py` 里没有搬来的表，它的 `REFUSED_STATUS` 是那个拒绝码集合的**第二份写法**
（元组 vs 正则），Part 59 把两份互相钉住，所以它不是第三层来源。**本轮没有动 `provenance.py`
的 `THIRD_PARTY` 表**：这台克隆算不出台账（§4.12 末条：fork 点的 215 条上游对象不在这里），
而 §4.12 明令不许用手算差值去凑。欠的那一步留在这里，**由有完整历史的克隆补**：把这两个文件登记进
`THIRD_PARTY`，然后让 `provenance.py --check --stamp` 去更新 `docs/Provenance.md` 的台账行与两张表。

### 6.6 P8.1 明细（应用 v0.19.0，tag `v0.19.0`）：同一张卡片上的四件事

**用户 2026-10-04 一口气提了四项，逐项拍板后合并成这一轮**（原话「没问题，按此方案执行」）。
四条都不是新流程，而是 P8 那张卡片**已经在这里**之上加的动作 —— 所以这一轮的形状是"bounded"：
`media_resolve.py` / `media_relay.py` / `protocol.py` / `setting.html` 四个文件里改，
没有新的模块、没有新的协议代码、没有新的 pip 依赖。

**① 菜单栏里那一行（`screen_mirror.MENU_HIDDEN`）**。P7 把控制面整个搬进网页之后（§6.4），
菜单栏 Renderers 组里留着的那一行只剩一个作用：让用户以为关掉它能停掉镜像。
隐藏的是**行**不是插件 —— 已持久化成 `Macast_Renderer = Screen Mirror` 的安装仍然按名字命中，
`set_media_url` 的 Bridge 让位照旧，那两件事由 Part 36 的 13 条菜单栏用例钉住
（顶层菜单该有什么、以及"不许出现电脑投屏行"这条用户裁定本身）。

**② 音画合并排在过滤之前**。`plan()` 交出第三种判决 `merge`，`pair_tracks()` 给画面行配上声音行，
第二条输入**带自己的请求头**（`input_arguments(url, headers)` 把 `-headers` 写在那条 `-i` **之前** ——
DASH 的两条轨道各有各的签名头，共用一份等于第二条必然 403）。
`visible()` 随后才收起"度量过、是活动画面、既没有声音地址也没有音轨"的行，并在旁边报出收起条数。
**先合并、后过滤是用户钉的顺序**，反过来做的话 bilibili 那张卡片是空的：它的 DASH 梯子里
每一条画面行单独看都"没有音轨"，先过滤等于把合并的对象全删了。未度量的行**永不**收起 ——
"探针没问出来"与"确实没有声音"是两句不同的话，这句从 v0.18 就写在判定表上。

**③ 整段分享文案 + 引擎自己的拒绝原话**。`looks_like_url` 与 `extract_share_urls` 是两问
（前者判"这串是不是一个地址"，后者从一段抖音分享文案里**挖**地址），挖出来之后仍要过裸 host 那一问
—— 交给引擎的必须是"一个网址"，否则 `example.com/watch/7` 会变成对这串文本的搜索（§6.5 那条老红线）。
`engine_note()` 把引擎 stderr 的第一行非空文本原样交回卡片（截 200 字符），`_COOKIE_WORDS` 命中
就单独答 `needs_cookies`：**"解不出"最有用的一种是源站把原因说清楚了而我们没转述**。

**④ 登录 cookie 与投给哪台设备**。这一条同时是 ⑥ 和 ② 两问，落在一处：
- **不代登录**是硬边界（没有账号密码表单、不弹登录窗），给的是两条**交 cookie** 的路：
  粘贴 Netscape 文件到 `SETTING_DIR/resolve_cookies.txt`，或允许 `--cookies-from-browser <白名单>`。
  文件优先于浏览器；白名单在**每次读**的时候复查，因为设置 JSON 是手改的，而一个以 `-` 开头的
  浏览器名会变成一条新的 argv 开关。文件 0600、**不进设置 JSON**、页面与日志只报条数不报值 ——
  那份 JSON 会被导出、被贴进 bug 报告，而 cookie 就是登录态本身。
- **设备下拉**复用 `cast_local_file` 已有的发现与投递（`target_state` / `target_refresh` / `target_push`），
  **没有一行新的协议代码**；`_cast_targets()` 只读插件的**缓存**（同步问一次要按住一个 CherryPy
  worker 三秒），第一项永远是「本机」。"本机"那一行的名字读的是决定它的那个事实
  （`Setting.get(Macast_Renderer)` 或 `MPV`），不是抄一个字符串 —— 这一条是被变异体逼出来的，见下。
  投给别台设备时**不碰我们自己的播放态**：两个 owner 会让状态页同时声称在放两件事。

**验证**：`pyflakes` 干净；套件 **2229/2235**（红的仍是 §4.12 那 6 条 Part 34，本机跑的时候
用户自己的实例在 8009/58880 上，总数按 §2 那条规矩先看过）。**Part 59 132 → 174 条**（新 R 段：
分享文案 / 引擎原话 / cookie 一整条链 / 设备下拉），Part 36 +13 条。**十七个定点变异体**
= v0.18 的七个（1/1/1/3/1/2/2）+ v0.19 的十个（1/3/1/4/1/1/2/7/2/2）：
`rstrip` 挪进正则 / 裸 host 白名单接受空 scheme / 合并的第二路输入复用画面那份 headers /
`plan()` 把 merge 报成 remux / jar 写成 0644 / 白名单只在写入时查 / 核心不再拼「本机」/
`target` 只认 `'local'` 不认空 / `_local_target()` 的名字写死 / 删掉 `#HttpOnly_` 那一支。

**这一轮的两轮"0 红"自我修正**（都是**用例没有牙**，不是产品没问题）：
① `target` 只认 `'local'` 那个变异体第一次跑让 Part 59 **在中间抛 `IndexError` 就死了**，
把后面约 53 条一起藏起来（`check()` 不捕异常）—— 修法是失败路径用 `.get()` 读回中转 id、
清扫循环只摘还活着的那几条，重跑之后红 7 条且**每条都落在自己那句话上**。
② `_local_target()` 的名字写死那个变异体**第一次跑出 0 红**，因为原用例写的是
`items[0] == _local_target()`：把答复和产出它的函数比，是同义反复。现在这个名字必须
**第二遍问那个决定它的事实**（`Setting` 里那个键），并新增一条"换一个渲染器选法，那一行跟着变"。
改判顺带又抓到一条漂移：那次多出来的读取让后面一条用例的 `_surfR59.states` 从 4 变 5，
读起来完全像产品坏了 —— 同一族的教训是"两个见证者必须分开报"。
③ 第三条同族、这一轮自己踩的：新用例里一个局部变量取了 `_restore59` 这个名字，而那正是
Part 59 的 I 段定义的还原助手（`verify_cast_airplay.py:24414`，整段末尾的 `finally` 靠它把
`media_resolve` 的三个全局挂回去）⇒ `finally` 拿到一个 dict，红法是
`TypeError: 'dict' object is not callable` 加一句「Part 59 runs」，把这一格之后**所有**用例吞掉。
写新用例前先 grep 一遍那个文件的作用域（AGENTS §4.14）。

**写文档时抓到一条真 bug，随 v0.19 一起发**（这是这一轮唯一一处"代码跟着文档改"）：
`cookie_jar_state()` 原先把每一行以 `#` 开头的都当注释跳过，而浏览器导出的 Netscape 文件里
**`#HttpOnly_` 那一行开头是 cookie 不是注释** —— 那是格式标记"脚本读不到"的属性，
一次真会话导出的 cookie 常常**整片**都带它。后果正是这个功能存在的理由被反着实现了一遍：
用户从浏览器导出、整段贴进来，计数回 0，卡片说"这份文件里没有 cookie"，
而他手里那份是他全部的登录态。判据照**标准库的读取规则**写（`MozillaCookieJar._really_load`：
在**原始行的 index 0** 上剥前缀，**剥完之后**才套用"空行/以 `#` 开头就跳过"）——
顺序就是整个 bug；所以 `#HttpOnly_` 光秃秃一行、`#HttpOnly_# …` 都是 `(0, 0)` 而不是"一条坏行"，
而缩进的 `  #HttpOnly_…` 对引擎仍是注释（前缀按 index 0 绑定），把它数成 cookie 等于
承诺一份 yt-dlp 从来不会加载的登录态。**旧的 jar 用例为什么没抓到**：它喂的是
`'# HttpOnlyCookieJar'` —— 带空格的**头注释**，真实形状零覆盖，所以删掉那一支时
**只有新写的两条用例变红、旧用例全绿**（§4.2「假 ffmpeg 抄虚构格式」同族：
不提问的检查等于通过的检查）。同步改了 `setting.html` 的粘贴提示与 `docs/Casting-Suite.md` §2b.4，
免得用户去手删那个前缀。

**这一轮欠着的三件验证**（发版前后都要读成"欠着"而不是"过了"）：
① 以 `merge` 供流的**真站复验**（bilibili 那一次是 v0.18 的取证，那时只有标注没有合并）；
② 设备下拉与 cookie 那块的**三档真浏览器验收**（§10 那条要求：临时配置目录 + 错开端口的实例，
绝不碰用户的 58880）；③ **投给一台真电视**——本局域网没有 Chromecast（打桩套件只证明
`target_push` 的三种失败形状各自带走中转，不证明任何一台真设备认账，§4.9 那一族的边界原样适用）。
cookie 那一格另有一条边界要说清：**它的对齐对象是标准库的读取规则，不是某一份真浏览器导出的实跑**
—— 后者同样欠着，而这一轮那个 bug 恰恰说明这两件事不一样。

**署名（AGENTS §4.12 那一族）**：这一轮**没有引入新的第三方层**。`#HttpOnly_` 那条判据来自
标准库（`http.cookiejar`，MIT 许可的 CPython 本体，不是又一层转写），四个改动都落在
我们自己写的 `media_resolve.py` / `media_relay.py` / `protocol.py` / `setting.html` 上；
`media_resolve.py` 头上那层 **castor** 归属仍只在 v0.18 那一批搬来的两张表上
（`STILL_IMAGE_CODECS` + `ADMISSION_TABLE` / `preference()` 骨架），这一轮没有新增搬来的东西。
`THIRD_PARTY` 那笔欠账不变，仍**由有完整历史的克隆补**（本机算不出台账，见 §4.12 末条与 §6.5 末段）。

### 6.7 P9 明细（设计批准 2026-10-04，**已实施，随 v0.20.0 发布**）：包体瘦身 —— WebRTC 的五个可选包移出默认产物

**这一轮是 architectural 而不是 bounded**：它动四个平台的构建流水线，并新增一个运行时装载器
（下载 → 校验 → 落盘 → 改 `sys.path` → 重新探测）。"某个包在不在产物里"这件事从构建期
搬到了运行期，而构建期是这套代码里唯一已经证明过自己会骗人的阶段（§4.3、§4.4）。

**实施与设计不同的四处**（都是落地时才看见的，不是改主意）：

1. **资产名多了一个 python 标签**：`Macast-WebRTC-extras-<os>-<arch>-<cpython 标签>-v<版本>.zip`
   （四个洞，不是下面 §2 原先写的三个）。理由是 §2 自己那句"cp312 与 cp313 的 wheel 不同"——
   既然它是**判据**，就该进名字：一个装错 python 的机器在设置页读到的资产名就写着"没有我的份"，
   而不是拉回一个 404 或者一个要靠 manifest 比对才发现不匹配的 zip。
   判据本身仍然在 manifest（`check_manifest` 那一步一个字节都没少做），名字只是**更早的一句真话**。
2. **进度不另开一个 127.0.0.1 页面**（§4 原设计借用了声音安装器那套进度页）：它骑在
   `extras_state()['steps']` 上，走设置页「电脑投屏」那张卡**已经有的**那一轮轮询。
   声音安装器要独立页面是因为它在**授权弹窗之前**就要有地方说话、而那时设置页还没回到用户手里；
   这一路是一次几十秒的下载，卡片就站在旁边。少一个随机端口、少一枚 token、少一份要守的
   `nosniff`/`no-store`/textContent 契约。
3. **`sys.path` 是插到最前面，不是追加**（§4 原话是"追加"）：追加的话，一个解释器本来就装着
   `aiortc` 的机器（源码用户在 `.venv` 里 pip 装过）会继续用它自己那份，于是这个按钮
   **装完了却什么都不变** —— 用户读到的仍是"这条通道不可用"。Part 60/C 有一条按
   `sys.path[0] == 落点` 断言的用例，另有一条断言 `aiortc.__file__` 真的落在我们那棵树里。
4. **`WEBRTC_INSTALL_HINT` 没有按"这台机器是不是冻结产物"分裂成两句**（§5 原设计）：那个判据
   只能靠猜（`sys.frozen` 认得 PyInstaller 不认得 py2app），而卡片上已经有两个**真回答**在替它
   判（`supported` / `ready`）。所以是一句话、两种读法，见下面的 §5。

**动机与实测**（本机 2026-10-04，`/Applications/Macast.app` = 已发布的 v0.19.0 产物）：

- `.app` **119 MB**，其中这条链 **≈58 MB**：`av` 一家 44 MB、`cryptography` ≈8 MB、
  `pylibsrtp` ≈4.2 MB、`cffi` + `_cffi_backend` ≈1.1 MB、`aiortc` 与纯 Python 层 ≈1.5 MB。
  同一批包在 `.venv` 里量到 ≈62 MB —— 差的是 py2app 把纯 Python 部分压进 `python312.zip`。
  **这两个数不是两个可互换的引用**：讲产物就用 58，讲依赖归属才用 venv 那一组。
- 这 58 MB 只服务 `screen_mirror` 的**第五种目标**（`webrtc`，v0.22）。其余四个形状
  （cast / browser / dlna / caststream）**一个都不碰它** —— 也就是说四个平台 × 每个下载者都在为
  一个可能永远不用的目标付这笔钱。
- 打成 extras zip 后 **≈25 MB**（`zip -9` 实测：av 18,264 KB、cryptography 3,188 KB、
  pylibsrtp 2,068 KB、cffi 280 KB、`_cffi_backend` 72 KB）。

**AGENTS §4.6/§4.8 把它捆进产物的理由是一句现在要被替换的话**：「`.app` 或 onefile 二进制
没有 pip 可用 ⇒ 打包工具不带这两个包，这条目标就是一个**没有任何下载能兑现的承诺**」。
**诊断仍然成立，不成立的是它开的药方**：捆进产物确实兑现了承诺，代价是让不投浏览器的用户
也付 58 MB。

**用户在两个出路里选了第二个**：① 只做减法（WebRTC 在打包版里退化成"源码用户才有的功能"）
—— **否**，理由是**用户自己的两台机器都跑打包产物**（这台 Mac 的 `.app` 与 `.68` 的打包 `.exe`，
而 §4.8 那组 400 毫秒跨机 WebRTC 实测正是从后者发出的）；② **可选 extras 包 + 应用内一键装**
（**选定**）：四平台各发布一个 extras zip，缺包时卡片给一个按钮，装完当场可用。

**门用 `GATE_CODE`**（用户批准的建议）：这一跳装的是**可 import 的代码**，而 §4.7b 那句判据
写的正是"落代码的只认令牌，loopback 不算数"。`mirror-action` 那条 `GATE_TOKEN` 先例**不适用**
—— 那个动作只操作已经在机器上的东西。

**1. 五个位点删，声明一处不删**

| 位点 | 动作 |
|---|---|
| `build.yml` macOS job 的 pip 列表（现 169 行） | 去掉 `'aiortc' 'av'` |
| 三个 PyInstaller job 的 pip 列表（现 325 / 434 / 540） | 去掉 `aiortc av` |
| 同三个 job 的 `--hidden-import=`（现 343–345 / 452–454 / 585–587） | 去掉 `aiortc`、`av`、`cffi` 三行 |
| `scripts/setup_py2app.py` 的 `packages`（现 486） | 去掉 `'av'` |
| 同文件的 `includes`（现 519 / 529） | 去掉 `'aiortc'`、`'cffi'` |

`requirements/common.txt` 与 `requirements/darwin.txt` 的**声明保留**：源码安装继续自带这两个包，
而 Part 30 那张允许面问的是"Macast 声明过没有"，不是"产物里有没有" —— 所以它**一个字节都不动**。
`scripts/build_macos_arm.sh` 不需要单独改（它 `-r requirements/darwin.txt`，见 §4.3 那条"唯一来源"），
但它现在会顺带把这两个包装进构建环境 —— 这是**对的**：py2app 不再捆它们之后，本机开发者
从源码跑仍然能测 WebRTC，而产物里没有。

**2. extras 资产怎么建**（CI，四平台各一步）

- `pip install --target <dir> aiortc av` → 逐文件 sha256 → `manifest.json`
  （`python` 标签、`platform`、`machine`、`macast_version`、`files[]{path,sha256,bytes}`、
  `total_bytes`）→ `zip -9` → 资产名 `Macast-WebRTC-extras-<os>-<arch>-<cpython 标签>-v<版本>.zip`
  （见上面「实施与设计不同的三处」第 1 条）。清单里**只有** `files[]` 点名的那些文件：
  目录条目一概不许进 zip，所以"解出来的东西 ⊆ 校验过的东西"是结构成立的，而不是靠解包时再判一遍。
- **`--target` 而不是 venv/`--prefix`**：要的是一整棵能原样搬到别人 `site-packages` 旁边的树。
- **`pip --target` 不解决 ABI**：cp312 与 cp313、arm64 与 x86_64 的 wheel 不同，所以 manifest
  那三个字段不是元数据而是**判据** —— 装之前对不上就拒绝，并把**两边的值都念出来**
  （"这个产物是 arm64/cp312，这份 extras 是 x86_64/cp312"）。解完之后才 `ImportError` 是
  把一个能一句话说清的问题留给一次崩溃去报告。
- **`av` 整树逐字节**：§4.3 那条 `@loader_path` 的理由在这里原样成立 —— `av/.dylibs` 必须与
  `av/` 同层，任何"只挑需要的文件"的裁剪都会造出一个起不来的 `av`。

**3. 地址只有一个主人**：`plugin_repo.py` 新增 `EXTRAS_PACKAGES` / `EXTRAS_ASSET` /
`extras_asset_name()` / `extras_urls()` / `extras_describe()`，跟随 `Github_CN_Mirror`
（§4.6 的三个 host 前缀规则，jsDelivr 永不加前缀；镜像开着时镜像在前、**直链留在后面兜底**，
因为一个死掉的代理不该是致命的）。页面与插件里**不许**出现资产 URL 字面量 —— 那条规矩对插件索引
成立的理由（改地址要改四处）对资产同样成立；卡片上那个资产名是 `query=plugin-info` 下发的
`extras_describe()` 读回来的（Part 60/A 按**精确相等**钉住它只有 `packages` 与 `asset` 两个键，
加键要同时改用例）。

**4. 应用内安装器**（`screen_mirror`，复用现有机械而不是新造一套）

- 骑在 `_SetupProgress` 步骤机上（**done 步骤拒绝重入 ⇒ 进度条永不回退**；skipped 不进分母），
  步骤就是这条功能的五段：`download / verify / unpack / land / probe`。**进度不另开页面**
  （见上面「实施与设计不同的三处」第 2 条）：那五步与一句当前消息经 `extras_state()`
  交出去，走设置页「电脑投屏」那张卡**已经在跑**的那一轮轮询，版式由 `mirror_view` 的
  `extras_step_marks` 决定 —— 和声音安装器同一枚标记函数，所以「跳过」在这一张卡上也是一个
  状态而不是"没做"。**这里没有第二个 token**：POST 那两扇门要的是管理令牌（`GATE_CODE`，
  §4.7b），而读取走的是页面自己那一份门控，全程没有新的可转发 URL。
- 落点 `SETTING_DIR/webrtc_extras/<os>-<arch>-<cpython 标签>/`；**每次装一棵新的，旧的整树
  `mv` 到 `.trash/`**（§10「破坏性操作要可逆」，也 §4.8 里插件卸载的同一条路）。按 ABI 分目录
  是为了"同一台机器上两个 Macast 版本不共用一棵树"，也是为了**拒绝错的包时不动对的那棵**。
- 链条：下载 → 逐文件 sha256 → 解到**临时名** → 全树校验通过才 `os.replace` 进位 →
  **`importlib.invalidate_caches()`** → `sys.path` **插到最前面**（理由见上面第 3 条）→
  清掉已加载的 `aiortc`/`av` 模块重新探测。
- **`invalidate_caches()` 那一行是这条链上最便宜的保险**：§4.2「刚写进目录的那个 `.py`，
  导入系统可以根本看不见它」量过**同一个形状**（`FileFinder` 按目录 `st_mtime` 缓存 `listdir`），
  而这条路每次落地的是**一整棵新目录树**。少了它，用户读到的是"装完了但还得重启" ——
  而那正是这个按钮要消灭的那句话。
- **每一次拒绝都发生在写盘之前**，而且都点名是哪一步：没有对应 ABI 的包 / 一个地址都没拉下来
  （并把最后一个试过的地址念出来，好让用户想到镜像开关）/ 清单与这台机器对不上（两边的值都念）/
  清单里有不安全的路径 / 少文件 / 哈希不符 / **多了清单里没有的文件**。唯一"留着已落地的树"的
  失败是最后的 `probe`：导入仍失败时，用户需要的是那个真的 `ImportError`（缺系统库、架构不对），
  不是一个安静变空的目录。
- **卸载分清是谁装的**：那一棵树是我们放的就移进 `.trash`；`aiortc` 来自 pip 就**拒绝动手**，
  回一句"它在 <路径>，请用 pip uninstall"—— 否则用户会以为自己刚把环境拆了一半。

**5. 文案**（`WEBRTC_INSTALL_HINT`）：设计里那一版要求"按这台机器是不是冻结产物判"，把
`pip install aiortc av` 与按钮各发给一种机器 —— **实施没有做那个判定**，因为它的判据只能是猜
（`sys.frozen` 只回答 PyInstaller，py2app 不置它，而 `.68` 那台恰恰是 PyInstaller），
而这一句旁边**已经有两个后端回答在替它判了**：`supported`（这台机器有没有对应的包）与
`ready`（现在 import 不 import 得动）。所以现在是**一句话、两种读法**：打包版点「一键安装
WebRTC 依赖」，源码用户在运行 Macast 的那个 Python 里执行 `pip install aiortc av`，
两边都跟着那句"回来重新选这个目标就行，不需要重启"（`_webrtc_modules()` 故意不缓存失败，
所以那句是真的，不是安慰）。Part 60/D 钉的是这两个动作**同时出现在这一句里** —— 少任何一个，
另一种机器的读者就会照着一个在自己机器上根本不存在的按钮找。

**6. 测试怎么写**（先写用例再动流水线）

- Part 56/D 现在那四条"必须捆进产物"**翻转**成三条新契约：① 默认产物的**四处 pip 列表**
  （macOS 一处 + 三个 PyInstaller job）与**三个 job 的 `--hidden-import`** 与 py2app 的
  `includes`/`packages` 里，这五个名字一个都不许出现 —— 注意 `cryptography` 与 `pylibsrtp`
  从来没被点名（它们随 `aiortc` 传递进来），所以这一条断言的是"**减法做干净了**"，
  不是"删过五行"；② extras 构建步骤**存在**且四平台各有；③ manifest 的字段与
  `requirements/*.txt` 的声明**互相对得上**（声明里那两个 == extras 装的那两个，
  少一个就是"卡片上有个装不上的按钮"）。
- 新段：下载 → 校验 → 落盘 → `sys.path` → `_webrtc_modules()` 的探测从"没有"翻成"有" →
  卸载可逆；**全部打桩 HTTP + 临时 `SETTING_DIR`**（§10）。
- ABI 不匹配那一格必须**两边的值都出现在拒绝消息里**，而不是只有"不兼容"。
- 门：Part 51 的"按表循环"自动覆盖新行，另加一条"这一行的门是 `GATE_CODE`" —— §4.7c 的
  结构保证只回答"登记过没有"，而"登记成哪一格"是这一轮的决定，得有人单独钉。
- 页面：Part 51 的字段对齐用例会自动要求新字段在表里登记；Q 段补按钮那句的文本契约。
- Part 30 **不动**，并写明为什么不动，免得下一轮有人以为漏了。

**7. 这一轮的诚实边界**（发版前后都读成"欠着"，不是"过了"）

- **「私有仓库 ⇒ 那个按钮注定 404」这一句已经不成立了，而且是今天量出来的**（2026-10-04）：
  设计批准时（以及 AGENTS §4.6/§5 写下"保持私有"那句决定时）`pingod/Macast` 确实是私有的，
  本轮动手前重问了一遍，四问全部一致地指向**公开**：`gh api repos/pingod/Macast` →
  `{"private": false, "visibility": "public"}`；**匿名**（不带任何凭据）`api.github.com/repos/pingod/Macast`
  → 200、`.../releases` → 200、`data.jsdelivr.com/v1/packages/gh/pingod/Macast` → 200；
  并且真的匿名带 `Range` 打了一个已发布资产（`v0.19.0` 的第一个）→ **206，1024 字节原样回来**。
  所以**别人点这个按钮是装得上的**，卡片上没有写"这个地址对你可能是坏的"那句预先准备的道歉。
  **这条改变的是世界的状态，不是那个决定**："不要再提改公开"仍然是用户的裁定（§4.6），
  这里只是把"因为私有所以坏"那句**已被观测否证的陈述**换成观测本身。
  顺带一句留给读到这里的人：AGENTS §4.6/§5/§9 与 `plugins/README.md`、`docs/Casting-Suite.md` §0
  里那些"索引对别人是坏的 / 官方只承诺手动安装 / `INDEX_PRIVATE` 是预期信号"的句子，
  前提同样已经变了 —— 本轮把它们改成**按今天测到的状态**说，历史数字（9 条固定链接里 5 条还 200、
  4 条已 404）保留为当时的实测。
- **国内镜像对 Release 资产仍没有验过**：资产下载走 `github.com` → `objects.githubusercontent.com`
  重定向，而 §4.6 那三个 host 前缀规则是从索引与 raw 量出来的。`extras_urls()` 因此**把直链留在
  镜像后面兜底**（一个死掉的代理不该是致命的），但"ghproxy 代理 `github.com` 的 release 下载到底
  行不行"这句话在本轮**仍然是转述不是实测** —— 卡片与这里都别把它写成已验证。
- **真机验收两处**：这台 Mac 的打包 `.app` 装一次并真开一路 WebRTC 镜像；`.68` 的打包 `.exe`
  装一次并重跑 §4.8 那组跨机数 —— **动那台机器要先征求用户同意**。这两处要等 CI 真把四个 extras
  资产发出来才有东西可装，所以它们的正确状态是**欠着**，不是"过了"。
- **页面这一半今天（2026-10-04）走过了**：临时配置目录 + 错开端口的第二个实例，用 `meta_path`
  拦截器把这台 venv 里**真实存在**的 `aiortc`/`av` 藏起来，好让卡片读到打包产物上那个状态；
  28 项判定全过、零红、页面没有任何 JS 报错，520 / 1000 / 1680 三档各留初始与已落盘两张截图。
  走过的是三条路，判据都是**卡片上渲染出来的字**：① 按「一键安装」，在没有资产的今天**必须**听到
  一句实话（「下载依赖包失败：依赖包没下载下来（<那条 Release 地址>）…」），同时进度落回、
  按钮重新可点、阶梯留着、scratch 清空 —— **这句失败是今天的正确行为**；② 手工把构建出来的包
  放进 ABI 位而解释器仍导入不了 ⇒ 卡片必须把**两个见证者的分歧**印出来（「依赖包已落盘，
  现在还导入不了」）并同时给两个按钮；③ 按「移除」⇒ 答案落在卡片上、点名 `.trash` 里的新位置、
  两步阶梯、槽位清空、树仍可恢复、回到只有一个按钮。**这一趟逼出一条产品缺陷**：卡片那句
  `message` 在成功收尾时装的仍是**最后一步的标签**（「确认 aiortc 与 av 导入成功」），真正的新闻
  只活在 `notify()` 那条一闪而过的通知里 —— 安装与移除**两个方向**都是，所以 `_SetupProgress.finish`
  加了第二个参数、移除另起一条自己的两步机，并补了 **Part 60/C2（9 条）**，它只问**卡片**而不问
  函数的返回值（C 段问的是后者）。**仍然欠着的只有"一次成功的安装"**：它需要真发布出来的资产
  （①里失败的那条链接）加上把这台 Mac 的 `.app` 换成 v0.20.0 的产物，而换包会**再要一次屏幕录制
  授权**（AGENTS §8）。源码跑的机器上 `ready` 为真、提示同时点名按钮与 `pip install aiortc av`
  那一行，是设计而不是缺陷（Part 60/D）。
- **允许用户目录里的原生扩展，前提是没有 hardened runtime**：本机唯一的签名步骤是
  `build.yml:259` 的 `codesign --force --sign -`（ad-hoc，无 hardened runtime、无 library
  validation），所以 `SETTING_DIR/webrtc_extras/` 里那棵 `.so`/`.dylib` 载得动。**这句话在
  Windows 侧没有等价证据**（PyInstaller 不签名），而哪天构建加上 hardened runtime，这条链会
  **静默**失效 —— 所以它写在这里，而不只是留在代码注释里。

**明确不做**（写在这里免得下一轮又提）：

- **不从 PyPI 按需装**：那要在用户机器上找一个 pip、猜一套解释器 ABI，并把供应链交给第三方索引；
  固定到我们自己构建、带逐文件 sha256 清单的那棵树是**更小**的攻击面，不是更大。
- **不顺手搬别的包**：`ScreenCaptureKit` / `CoreMedia` / pyobjc 是 macOS 采集快路（v0.21），
  `zeroconf` / `pillow` 在核心路径上（§4.3 那出"产物全部无法启动"就是漏了 zeroconf）。
  这一轮只搬**唯一服务第五种目标**的那一条链，其余的"看起来也能瘦"都不在范围内。
- **不给 `av`/`aiortc` 做纯 Python 替身**：那等于重写 SRTP 与 DTLS，而 §4.8 那条
  "纯 Python 密钥流"的教训（4.5 Mbps 天花板）说明这种替身会变成一个更贵的默认路径。
