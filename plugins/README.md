# `plugins/` — 插件索引（第三方 / 用户自建）

本仓库 15 个第一方插件已内置在 `macast/plugins/`（`renderer/` 与 `protocol/`，见 AGENTS.md §4.8），随应用一起发布。本目录不再是插件代码所在，只保留**可选的第三方 / 用户自建插件索引** `info.json`。
它是设置页「插件 → 可安装」分组的数据源：一个静态的 `info.json`，由设置页在
浏览器里直接拉取；提供安装的插件 `.py` 也放这里。

## ⚠ 这个目录是**源码**，不是安装源：仓库保持私有（所有者已定）

索引与安装 URL 都指向本仓库（`pingod/Macast`），而 jsDelivr 和 raw **都读不到私有仓库**，
Macast 下载插件时又不带任何凭据 —— 于是设置页的卡片区照常渲染、点「安装」才失败，
症状长得像网络问题。实测（2026-09-21）：`gh api repos/pingod/Macast --jq .private` → `true`；
匿名打 `api.github.com/repos/pingod/Macast` 回 404，而同一请求打公开的上游
`xfangfang/Macast` 回 200；9 条固定链接里只有 5 条还回 200，那是 CDN 在仓库还可读时
缓存下来的副本，会**逐条**过期 —— v0.8 的 Screen Mirror、RAOP 0.2、AirPlay Screen Mirror
三条已经拉不到了。

**2026-09-21 的所有者决定：仓库继续私有，官方承诺只有「手动安装」这一条路。**
（另两条出路 —— 把仓库改公开、或把 `plugins/` 挪到一个公开仓库 —— **都不做**，见本节末。）

- **手动安装的两条路**：① 设置页 →「插件」→「从网址安装」，贴一个**你这边可达**的 `.py`
  地址（自己有仓库读权限的人可以直接用 GitHub 的 raw 链接）；② 把 `.py` 直接放进
  `~/Library/Application Support/Macast/renderer/`（协议插件放 `protocol/`），
  菜单重载插件或重启即生效。两条都是热生效、不需要重装 Macast。
- **因此这个 README 与下面所有条目都要按「源码在这里」来读**：`info.json` 仍然是
  索引格式的唯一样板，Part 5c 仍然逐字校验条目与 `.py` 顶部清单一致 —— 它证明的是
  「条目 == 它所固定的内容」，**不证明任何人拉得到**。可达性另有一个脚本问外面：

```shell
env -u PYTHONPATH python3 scripts/check_index_reachability.py   # 退出码 0/2/3
```

它在私有状态下会稳定报 `INDEX_PRIVATE`（**故意不算成功**：CDN 缓存还剩几条时不能让人
误读成"索引好了"），退出码 2 是给 cron/CI 用的信号，不是待修的 bug。

- **不要为了"让安装能用"去改仓库可见性或另立公开仓库**：那是所有者已经回答过的问题
  （AGENTS.md §4.6 / §5）。要改只能由用户提出。

## 为什么不放上游那 6 个插件

上游插件合集 `xfangfang/Macast-plugins` 里的 6 个插件，以及本 fork 自研的 9 个插件，
现在**全部内置**在本仓库的 `macast/plugins/` 下（`renderer/` 与 `protocol/`），随应用一起发布；
其中 6 个来自上游合集的插件状态为 `vendored`，9 个自研插件为本 fork 自己的 `ours`：

| 插件 | 类型 | 平台 | 状态 |
|---|---|---|---|
| IINA Renderer | renderer | darwin | vendored |
| Web Renderer | renderer | darwin,linux,win32 | vendored |
| Live Renderer | renderer | win32,darwin,linux | vendored |
| PotPlayer Renderer | renderer | win32 | vendored |
| PIFMRDS Renderer | renderer | linux | vendored |
| NVA Protocol | protocol | darwin,linux,win32 | vendored |
| yt-dlp Downloader / External Player / Floating / Hooks / Chromecast Bridge / Screen Mirror / Local File Caster / RAOP / AirPlay Screen Mirror | renderer / protocol | 见 §4.8 | ours（本 fork 自研） |

继续读上游索引只会给每个内置插件再生成一张重复卡片，还挂着指回上游旧文件的
「可更新」角标。所以第一方插件的索引不再出现在这里。第三方 / 用户自建插件的条目仍可加进
本目录的 `info.json`（目前为空 `plugin_v1`）：设置页拉到一个空索引时只显示本机已内置的插件，不报错。

内置插件的发现、热插拔与 `<macast.*>` 清单格式见 `AGENTS.md` §4.4 / §4.5。

## 本仓库内置的插件（已在 `macast/plugins/`，开箱即得）

| 文件 | 插件 | 为什么放在线而不是内置 |
|---|---|---|
| `macast_ytdlp.py` | **yt-dlp Downloader** — 把投屏链接交给 yt-dlp 下载到本地，或边下边播 | 依赖用户自己装的 `yt-dlp` 命令 |
| `external_player.py` | **External Player (VLC / MPC-BE / mpv.net)** — 用你自己的播放器放 | 只对装了那个播放器的人有用 |
| `floating.py` | **Floating Player** — 角落置顶小窗，含实验性壁纸模式 | 纯偏好，跟版本无关 |
| `hooks.py` | **Automation Hooks** — 投屏 / 暂停 / 继续 / 停止时执行你的命令 | 命令因人而异，配置在设置里 |
| `cast_bridge.py` | **Chromecast Bridge** — 把收到的投屏转投给另一台 Chromecast | 只对有多台设备的人有用 |
| `screen_mirror.py` | **Screen Mirror v0.9** — 把桌面屏幕实时镜像到局域网：**两条 Chromecast 通道**（兼容 LOAD / 实验性低延迟 Cast Streaming）、**没有 Google 栈的老电视（DLNA）**、或任意浏览器打开一个网址（三平台，macOS 一键装好系统声音：带进度页的步骤机，v0.9 起按机器状态判定，**不会重复下载已装好的驱动**） | 依赖用户自己装的 `ffmpeg` 命令 |
| `cast_local_file.py` | **Local File Caster v0.1** — 把**这台机器磁盘上的文件**投到电视：菜单里选文件夹、点文件即在 Chromecast / Google TV 或 DLNA 电视上播；能原生解码的文件由内置 Range/206 服务按字节直供（远端的暂停/拖动直接作用在真文件上），其余边播由 ffmpeg 转码；带播放列表自动连播、音轨/字幕选择、音画同步偏移、被抢占后看门狗重投、退出时 QUIT_APP | 依赖用户自己装的 `ffmpeg` / `ffprobe` 命令 |
| `raop.py` | **AirPlay Audio (RAOP)** — 监督 shairport-sync，接收 AirPlay 音频 | 需要用户自己装 `shairport-sync` |
| `airplay_mirror.py` | **AirPlay Screen Mirror** — 监督 uxplay，让 iPhone / 另一台 Mac 把屏幕**镜像到这台机器**（镜像流是 AES-128-CTR，不需要 FairPlay；uxplay 自己开窗渲染） | 需要用户自己编译 `uxplay`（macOS 既无 Homebrew formula 也无官方二进制，插件日志里有完整配方） |

**Macast 一次只能用一种渲染器**，所以 `macast_ytdlp` / `external_player` / `floating` /
`hooks` / `cast_bridge` / `screen_mirror` / `cast_local_file` 是互斥的（菜单栏里切换）；`raop.py` 与
`airplay_mirror.py` 是协议插件，可以和任意渲染器同时开。

各插件要点（**面向使用者的逐目标首次设置流程在 [`docs/Casting-Suite.md`](../docs/Casting-Suite.md)**，
这一节讲的是实现要点）：

- **yt-dlp**：模式在菜单里切（下载 / 边下边播）。下载目录默认 `~/Downloads/Macast`，
  可用设置页「高级设置」的 JSON 编辑器改 `YTDLP_Dir`。边下边播走 mpv 自带的 ytdl hook，
  插件会把 `ytdl_hook-ytdl_path` 指给你装的那个 yt-dlp。暂停/继续在下载模式下**故意不实现**
  （它是下载器不是播放器，假装 `PAUSED_PLAYBACK` 只会骗发送端）。
- **External Player**：玩家列表在菜单里选；没装的会灰显。选中的播放器可以用设置里的
  `External_Player` / `External_Player_Path` 覆盖。位置是模拟的（每秒 +1 秒），
  停止只杀我们启动的那个进程 —— VLC 单实例模式下会由已有窗口接管播放，这时停止不生效，
  我们不会去杀无关窗口。
- **Floating Player**：`Floating_Size` 是 `SIZES` 的下标，`Floating_Mode=1` 打开壁纸模式。
  壁纸模式需要 mpv 支持 `--ontop-level=desktop`（新版才有），不支持时会退回全屏置顶窗口并提示。
  壁纸模式是**实验性**的：能不能压在桌面图标下面取决于系统版本。
- **Automation Hooks**：四个命令写在设置里（菜单「Edit Hooks」直接打开设置文件）：
  `Hook_On_Cast` / `Hook_On_Pause` / `Hook_On_Resume` / `Hook_On_Stop`，都是 shell 命令，
  环境变量带 `MACAST_EVENT` / `MACAST_URL` / `MACAST_TITLE`。URL 只通过环境变量传递，
  **不拼进命令行**（它是网络来的字符串）。命令是 spawn 出去的，不等待、不阻塞协议线程。
- **Chromecast Bridge**：目标在菜单里选（mDNS 搜索 `_googlecast._tcp`，也可以直接在设置里写
  `Cast_Bridge_Target` = `host:port`，测试就是靠这条路径）。它复用 `macast.protocol_cast`
  的 Cast v2 收发实现，**不引入 pychromecast 依赖**。首次投屏前必须选好目标。
- **Screen Mirror**：菜单栏「输出目标」先选一类，再「开始镜像」。链路是 ffmpeg 屏幕采集 →
  H.264/MPEG-2 → 插件内的 HTTP 服务持续输出实时流。三类目标只是**封装不同**：
  **Chromecast / Google TV** 走 `video/mp2t`（MPEG-TS）+ Cast `LOAD streamType=LIVE`；
  **浏览器**走分片 MP4（`frag_keyframe+empty_moov+default_base_moof`），镜像开始时菜单会给出
  一个 `http://<本机>:<端口>/browser?token=…` 网址，局域网里任何浏览器打开即看（MSE 播，
  不支持 MSE 的会退回渐进式下载）。关键帧节奏就是分片节奏（每秒一个），所以后加入的观看端
  能立刻接上。采集按平台分派：macOS `avfoundation`、Windows `gdigrab`、Linux `x11grab`
  （**只认 X11 会话**，纯 Wayland 会明确报出来）。
  **v0.4 的采集选项**（都在菜单里，改完对下一次镜像生效）：多显示器选择（探针缓存里那台机器
  列出的 `Capture screen N`，插拔后自动回落到默认屏而不是报错）、画质四档 `360p / 720p（默认）/
  1080p / 原始分辨率`、是否画鼠标指针、以及 macOS 上的**硬件编码（VideoToolbox）**开关
  —— 开关会先真的去问这个 ffmpeg 认不认识 `h264_videotoolbox`（问一次缓存一次，菜单每次重绘
  不再 spawn ffmpeg），机器上没这个编码口时就直接拒绝。镜像期间用 `caffeinate` 阻止 Mac 休眠，
  停止镜像即释放；镜像中的状态行显示「已镜像时长 · 实时码率 · 观看端数 · 丢块数」。
  观看地址带**每会话随机**的 stream id 和页面 token（不是应用那个常驻管理令牌），
  所以 token 外泄只影响一次会话，别人也猜不到你的流。**Chromecast 目标永远只拿「接下来」的字节**
  （否则会永远慢着几秒）；**浏览器目标重播缓冲**（后加入的观看端要立刻看到画面）；
  **DLNA 目标把直播当成一个「无限大的文件」来供**，见下。
  **v0.5 的 DLNA 电视目标**（给没有 Google 栈、也不开浏览器的那类电视：老智能电视、
  机顶盒、只认 DLNA 的投影仪）：这类设备只肯播它能 `HEAD`/`Range` 到的**固定大小文件**，
  所以插件把直播伪装成一个文件 —— 对外报一个按档位码率算出来的固定 `Content-Length`
  （永远小于 2³¹，老固件的有符号 32 位上限），每个响应都带 `Accept-Ranges: bytes` +
  `transferMode.dlna.org: Streaming` + `contentFeatures.dlna.org`，探边界的那几个请求
  必须**恰好**回 n 个字节（不够就用 MPEG-PS 的填充包补齐），断线重连按**绝对字节偏移**
  从一个 48 MiB 环形缓冲里取，偏移落在环外就报丢块而不是回错数据。代价是**延迟**：
  推流前先攒够约 20 MiB（菜单状态行与开始提示都会说出这个秒数），所以这条链路是
  「客厅挂机上给爸妈看屏幕」而不是「会议低延迟投屏」。封装由**兼容档位**决定
  （`ps-pal` / `ps-ntsc` / `ts-mpeg2` / `ts-h264` / `mkv-h264`，PAL 与 NTSC 分两套帧率与
  帧尺寸，声音是 AC-3 而不是 AAC —— DVD 时代的电视认得它），菜单里可手选；
  连续读不到 `PLAYING` 会**自动换到下一个档位**并重启编码链路，全部试完就给一句
  「在菜单栏「兼容档位」里换成 X 再试一次」。电视的发现走 SSDP `MediaRenderer:1` →
  抓设备描述 → 只认带 `AVTransport:1` 的（控制 URL 可能是相对路径，也可能是设备自称的
  另一个地址；描述里漏端口/写错地址都按**实际应答的那个地址**修正），菜单「输出目标 →
  DLNA 电视」里列出候选，选中即写入 `Mirror_Dlna_Control`（与 Chromecast 的 `Mirror_Target`
  分开的键 —— 那是 `host:port`，用它存 URL 会把 `http` 当成主机名）。推给电视的
  `SetAVTransportURI` 里带 DIDL-Lite（`protocolInfo` 与档位一致），之后每 5 秒
  `GetTransportInfo` 看门狗：掉出 `PLAYING` 就重投 URL，`RelTime` 在动才算真的活着。
  **让位/中继**：DLNA 与 Chromecast 目标一样，镜像期间手机再投别的内容会把该 URL
  转投给电视（电视已经在放那个网址时不重复推），浏览器目标下这样的推送会被明确拒绝。
  **系统声音**跟随条件：macOS 需要虚拟声卡 `blackhole-2ch`（FFmpeg 至今没有任何发行版
  能直接抓 mac 系统音频——提议中的 screencapturekit demuxer 从未合并）。**v0.3 起不用你手动装**：
  菜单「系统声音 → 一键设置（BlackHole + 多输出设备）」会自动下载官方 pkg（地址与 sha256
  以 Homebrew cask API 为准，校验后才安装）、弹出图形安装器（你只需输一次密码——.pkg
  无法静默安装），装好后用 CoreAudio 自动创建「多输出设备」（扬声器 + BlackHole，
  这样电视有声、你自己也听得见）并把默认输出切过去；再点一次可「恢复原声音输出」。
  任何一步失败会降级为帮你打开「音频 MIDI 设置」并给出文字指引。Linux 走 PulseAudio/PipeWire 的
  `<sink>.monitor`，通常开箱即有。Windows 仅画面。macOS 首次使用需在
  「系统设置 → 隐私与安全性 → 屏幕录制」给 Macast 授权（没授权时插件会明确报出来，
  不会静默黑屏）。**让位/中继行为属于两个电视目标**（Chromecast 与 DLNA）：镜像进行中若手机
  DLNA 投了东西，镜像会让位并把收到的 URL 转投给电视；浏览器目标下这样的推送会被明确拒绝，
  而不是把正在跑的镜像弄停。目标同样可以直接写设置：Chromecast 用 `Mirror_Target` =
  `host:port`，DLNA 电视用 `Mirror_Dlna_Control` = 控制 URL（测试走这两条路径）。
  **v0.6 的第二条 Chromecast 通道 —— 「Chromecast 低延迟（实验 · 无声音）」**：
  LOAD/mpegts 那条路的延迟来自 MPEG-TS 的缓冲，客厅够用、开会不够。这一档改说
  Chrome「投放桌面」用的那套协议（Cast Streaming）：直接向电视的镜像接收器
  （`0F5096E8`）发 OFFER，拿回一个 UDP 端口，然后把画面切成带 Cast 头的 RTP 包
  推过去 —— 没有 HTTP 服务、没有 `LOAD`、也没有观看网址。**代价与边界要说清**：
  ① 这条通道**目前没有声音**（镜像接收器的音频流是下一步）；② 单文件插件不能装加密库，
  加密用纯 Python 实现，所以**码率上限 4.5 Mbps**，1080p 会被压到该上限（菜单会提示）；
  ③ 帧尺寸是 OFFER 里**先声明后编码**的，所以这一档只跑 360p/720p/1080p 三档，
  「原始分辨率」会按 1080p 信箱化而不是拉伸；④ **这套字段是从参考实现转写的，还没有在任何
  真电视上验证过**，所以它是 opt-in：设备不认（`LAUNCH_ERROR`）就自动回落到 LOAD 通道，
  提示语会说明「此通道还没有声音」。想自己拿真机验一遍：
  `.venv/bin/python scripts/cast_streaming_probe.py <电视 IP>`（同一份代码，`--live` 采桌面，
  `--dump` 留下码流给 ffprobe）。
  **v0.7 把上面那条「一键设置」变成了一个看得见的步骤机**（`AUDIO_STEPS` 十步：环境自检 →
  检测 BlackHole → 取官方包信息 → 下载 → 校验 sha256 → 打开安装器 → 等待装完 → 重载音频服务
  → 建/复用多输出设备 → 切默认输出）。之前它只在终端里打日志，你点了菜单之后面对的是一个
  没有任何反馈的等待，失败了也只有一句「降级为手动」。现在进度页在 `127.0.0.1` 上开一个
  带**本次运行随机 token** 的页面（错的或没有 token 一律 403，`nosniff` + `no-store`，
  页面里**不出现**管理令牌），每步显示 未开始 / 进行中 / 完成 / 跳过 / 失败，百分比只按
  「真正需要做的步」算（健康机器上被跳过的装包步骤不进分母）。四条安装分支各自指名它停在哪一步：
  已经装好的直接跳过、有残留记录的重装、盘上有驱动但 coreaudiod 没加载 → 只重载、
  你在安装器里取消了密码 → 如实报「安装未授权」。sha256 不匹配**不会**打开安装器；
  崩溃只把**当时在跑的那一步**判失败，晚到的完成事件压不过它；官方包信息优先走 Homebrew cask
  API，兜底值会标注来源；「设备装没装好」用两个独立的探针问（`pkgutil` 的收据 vs
  `/Library/Audio/Plug-Ins/HAL` 里的文件），因为它们在半装状态下会给出不一致的答案；
  重载 coreaudiod 走 `osascript ... with administrator privileges`，所以它会弹一次授权。
  **仍然要说清**：.pkg 无法静默安装，那一次密码是省不掉的。
  **v0.9 修的是「永远装不完」**（用户原话：安装时候需要重启，重启后再点一键安装又要安装一遍）：
  旧的 probe 分支把"安装会被跳过"**写成了提示语**，然后照样 fall through 到下载 + 打开安装器，
  而唯一有用的那一步（重载 coreaudiod）要等 300 秒超时之后才可达。现在判定收成一个值
  （`_blackhole_state`：capturable / loaded / on-disk / stale-receipt / absent），
  **跳过 = 步骤机里的 skipped 状态**。两个见证者分开用是有原因的：
  avfoundation（`_has_blackhole`，采集真正会读的那份）与 CoreAudio（`_find_blackhole`，不受权限门槛影响）
  不一致 ⇒ 那是**麦克风权限**，装多少个 .pkg 都修不好，于是这一支不重装也不重载、照样建好聚合设备，
  然后把结论点名到「系统设置 → 隐私与安全性 → 麦克风」（`.app` 侧的 `NSMicrophoneUsageDescription`
  这次补进了 `scripts/setup_py2app.py`；没有它，系统连授权弹窗都不会给）。
  每一支失败文案都保证同一句话：**再点一次不会重新下载已经装好的东西**。
  Part 29 的两条反循环断言就是守这个的：「盘上有驱动」那一支必须既没有 `_fetch_blackhole_pkg`
  调用也没有 `open`，「CoreAudio 有 / ffmpeg 没有」那一支必须连 `osascript` 都不弹。
  **v0.8 修的是从 v0.1 就在的一条坏路：macOS 上镜像根本起不来。** 采集探测要问
  `ffmpeg -f avfoundation -list_devices true -i ""` 这台机器有哪些设备，而那段解析是按一个
  **从未存在过的输出格式**写的 —— 它找 `Video devices:`（大写 V）并且只取双引号里的名字，
  真实输出却是小写的 `AVFoundation video devices:`、设备名**不加引号**（`[0] OBS Virtual Camera`），
  于是两个列表永远为空，菜单只会说「ffmpeg 没有列出任何屏幕采集设备（avfoundation）」。
  现在的解析也认旧版 ffmpeg 的 `List of Video devices:` + `0) name` 写法；**没有索引的行不算设备**，
  因为 `-i N:none` 需要那个数字。为什么四个版本都没被发现：Part 21/22/23 里的假 ffmpeg 输出的
  正是那个虚构格式 —— 测试和实现共享了同一个错误假设。现在所有假 ffmpeg 一律照抄真机输出，
  并且验证套件 Part 31 有一条用例**去扫测试文件自己**：任何 `-list_devices` 回答里出现
  「带引号却没有索引」的设备行，当场变红。
- **Local File Caster**：JustStream 的另一半能力 ——「文件在这台 Mac 上，想看的屏幕在客厅」。
  菜单选一个文件夹（`File_Folder` / 设置里的 `Folder`），列出其中的媒体文件，点一下就投出去。
  两个塑造整个文件的判断：
  ① **直通还是转码按文件判断（ffprobe），不按扩展名猜**：目标能自己解的文件（mp4/mov/m4v/
  mkv/webm + H.264 + AAC/MP3/FLAC/Vorbis/Opus）由插件内置的 **stdlib Range/206 静态服务**
  按字节原样供出去，于是遥控器上的暂停、拖动、音量都作用在真文件上，插件什么也不用做；
  其余（AVI、HEVC、DTS、AC-3、第二条音轨、内嵌字幕）交给 ffmpeg 边播边转。
  ② **转码路径写的是"会一直变大的文件"，不是管道**：管道没有长度、不能重连、不能拖动，
  而 DLNA 接收端**根本不肯播一个报不出长度的流**。所以 ffmpeg 追加写一个临时 MPEG-TS，
  HTTP 服务按字节区间发它；读得比编码快就**等**而不是回一个短答案。对外报的长度是
  时长 × 码率并压在 **2 GiB 以内**（老固件在那里做有符号 32 位运算），如果是这个上限卡住了
  长度就**把码率降下来**而不是让长度说谎。拖动 = 从新时间点重启编码器（这是对"转码进行中
  能做什么"的诚实答案）。
  **音轨选择与音画同步偏移只存在于转码路径** —— 直通是把原始字节交给设备，它的轨道列表
  没法从这里重排；选了非默认音轨的文件会被决策表**特意**改判为转码，菜单里就是这个措辞。
  **字幕分三条路**（目标确实不同）：Chromecast 把内嵌字幕转成 **WebVTT** 再塞进 `LOAD` 的
  `textTracks`（由接收端渲染）；转码路径在**这个 ffmpeg 编了 libass** 时烧进画面；
  DLNA 电视两条都没有。
  **发现（Chromecast 走 mDNS、电视走 SSDP）一律在后台线程**：`build_menu` 跑在 UI 线程上，
  绝不能为了列菜单去等三秒组播；能力探测（VideoToolbox、libass）同理，问一次缓存一次。
  **看门狗**就是 `--hijack` 的正面版本：每几秒问一次设备在干什么，我们的 app / URI 不在了
  就从**设备上报的位置**重投，**最多 3 次**（`MAX_REPUSH`）—— 别人也想用这台电视时，电视归他；
  设备上报"这条播完了"才推进播放列表（`Auto_Next`）。
  **停止发的是 QUIT_APP 而不是只有 STOP**：只 STOP 会把接收端 app 停在最后一帧上，
  那就是"我明明停了投屏电视还挂着画面"这类 bug 报告的来源。
  另外两档：**直接投一个普通 URL**（不碰任何文件，也不起本地服务），以及**只投系统声音**的
  音频档（macOS 需要 BlackHole 之类的采集口、Linux 走 `<sink>.monitor`，探测不到就明确说）。
  转码临时目录默认在系统临时目录下，**结束即删**，菜单页脚会写出它的路径与容量上限。
- **AirPlay Audio (RAOP)**：`brew install shairport-sync`（Linux 用包管理器）后启用即可，
  它自己会做 mDNS 广播。插件只负责用你的 Macast 名字生成配置、拉起进程、把连接/断开报给你。
  **不**把 RAOP 映射成 DLNA 播放状态（RAOP 没有媒体 URL，硬报 PLAYING 会和 DLNA 的状态账本打架）。
  **v0.2**：音箱名可以用高级设置 `RAOP_Device_Name` 单独指定（留空 = 跟着 Macast 的友好名走）。
  和「AirPlay Screen Mirror」（uxplay）同时开着会**撞名** —— 两者都广播 `_airplay._tcp`，
  手机上只会剩一个，给它们起不一样的名字。
- **AirPlay Screen Mirror**：监督 `uxplay`，把 iPhone / 另一台 Mac 的**屏幕镜像到这台机器**
  （uxplay 自己开一个窗口渲染，Macast 不碰画面）。要点：选项写在 Macast 自己的配置目录里
  并用 `-rc` 传入（不用 `$UXPLAYRC` —— 它指向不存在的文件时 uxplay 会静默回落到你的
  `~/.uxplayrc`，看起来像"我们的设置没生效"）；**绝不传 `-p`**（那是包括 TCP 7000 在内的
  传统端口组，和 Macast 内置的 AirPlay 以及 macOS 自带的接收端撞车）；日志走 **pty** 而不是管道
  （uxplay 用 `printf` 且几乎不 `fflush`，管道是块缓冲的，"谁连上了"这条会卡住不说）。
  **macOS 上没有 Homebrew formula、官方发布也没有二进制**，要自己编译 —— 插件日志和
  `scripts/selfcheck.py` 都写了完整配方（Xcode 命令行工具、`cmake libplist openssl@3`、
  GStreamer 的 runtime + `-devel` 两个 .pkg、`cmake . && make && sudo make install`）。
  高级设置 `Mirror_Uxplay_Options` 是全部的旋钮（每行一个 uxplay 选项，不带前导 `-`，
  写在生成的文件最后，所以能覆盖上面那几个默认值）。**镜像流不是 FairPlay**：数据通道是
  AES-128-CTR，密钥来自 RSA/AES 握手，所以接收端不需要破解 DRM —— 但受保护的内容（Apple TV、
  Netflix）本来就不肯被镜像，投过来就是黑屏，那是设计如此。

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
https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json      # 首选，永远最新
https://ghproxy.net/https://raw.githubusercontent.com/.../info.json        # 国内可达，也是最新的
https://cdn.jsdelivr.net/gh/pingod/Macast@main/plugins/info.json            # 垫底：会缓存数小时
```

设置页按顺序试，第一个能通的即采用；三个都不通就只显示本机插件，并给一句提示。
jsDelivr 垫底是因为它**缓存分支文件数小时**（可能给出过期索引，且 `?v=` 破不了）；
ghproxy 类镜像按需代理 raw，实测推送 6 分钟后仍在给旧文件，但通常比 jsDelivr 快。
改这个顺序前先想清楚「新插件多久能被看到」——**索引本身**没法固定 SHA（它就是那个要变的文件），
所以新插件的出现最多可能延迟几小时（指到 raw 的话是立刻）。

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

`url` 指向本目录里的文件即可，例如 `plugins/some_renderer.py`。

**插件的 `url` 固定到 commit SHA，绝不指向分支**：
`https://cdn.jsdelivr.net/gh/pingod/Macast@<40位sha>/plugins/<file>.py`。

安装走的是 **Python 侧** `requests`（`MacastPluginManager.install_url`），没有设置页那种浏览器
多地址回退，而**每一种中间缓存都会长期冻结分支引用**，这是实测出来的：

| 源 | 实测结果 |
|---|---|
| `raw.githubusercontent.com` | 永远最新，但国内经常拉不动 |
| `cdn.jsdelivr.net` 分支 | 缓存数小时；**`?v=` 查询串不能破缓存**（升到 0.2 后 `?v=0.2` 仍返回 0.1 的内容） |
| `ghproxy.net` 代理 raw | 自称 `cache-control: max-age=300`，实测推送 6 分钟后仍在给旧文件 |

固定 SHA 后内容永远不变，缓存多旧都不会给错文件。**代价是插件更新要分两次提交**：

1. 改插件 → 提交（记下 commit SHA）；
2. 更新 `info.json` 的 `version` 与 `url`（指到第 1 步的 SHA）→ 再提交。

忘了第 2 步不会静默出错：Part 5c 会用 `git show <sha>:plugins/<file>` 校验条目与所固定内容一致。
jsDelivr 抽风时，把 raw 直链（`https://raw.githubusercontent.com/pingod/Macast/<sha>/plugins/<file>.py`）
粘到设置页的「从网址安装」即可。

## 改完怎么验

```shell
env -u PYTHONPATH .venv/bin/python -m pyflakes macast/plugin_repo.py
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py   # 含插件索引用例
curl -s 'http://127.0.0.1:58880/api?query=plugin-info' | python3 -m json.tool | head
```

注意 `setting.html` 在 `Handler.__init__` 里被缓存，改前端模板必须重启 Macast 才生效。
