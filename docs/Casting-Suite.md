# Casting Suite 使用指南（发送端插件族）

面向**使用者**：怎么把这台电脑的屏幕、声音、本地文件弄到电视上，以及反过来
怎么让 iPhone 把屏幕镜像到这台电脑。规划与取证件见
[`Casting-Suite-Plan.md`](Casting-Suite-Plan.md)，实现层的红线见 `AGENTS.md` §4.8，
插件的安装与索引机制见 [`../plugins/README.md`](../plugins/README.md)。

> **诚实声明**：下表每一个"目标"都只在**打桩测试**（`scripts/verify_cast_airplay.py`）
> 与自家假设备/假电视上验证过。**真实 Chromecast、真实老电视、真实 iPhone 一台都没碰过。**
> 每条链路末尾都标注了它的验证状态和现场排障入口。

---

## 0. 先决条件（三件插件都共用）

| 需要 | 为什么 | 装法 |
|---|---|---|
| **ffmpeg** | 屏幕镜像与转码全靠它 | `brew install ffmpeg`。插件会按 PATH + `/opt/homebrew/opt/ffmpeg/bin` 等常见目录找（**从 Finder 启动的菜单栏 App 不继承 shell 的 PATH**，所以只写在 `~/.zshrc` 里不算数） |
| **ffprobe** | 本地文件投屏判断"直通还是转码" | 随 ffmpeg 一起来 |
| **屏幕录制权限**（macOS） | 没有它 ffmpeg 一秒内退出 | 首次失败时插件会提示去「系统设置 → 隐私与安全性 → 屏幕录制」勾选 Macast，**勾完要重启 Macast** |
| 同一局域网 | 电视要能反过来访问这台电脑 | 代理变量会被自动摘掉（否则局域网流量走代理 → 502），见 `AGENTS.md` §4.1 |

**渲染器互斥**：`Screen Mirror`、`Local File Caster`、`Chromecast Bridge`、
`Floating Player` 等都是 renderer 类插件，**同一时刻只能启用一个**，
在菜单栏「选择播放器」里切换。`AirPlay Audio (RAOP)` 与 `AirPlay Screen Mirror`
是协议类插件，可与任意渲染器共存。

**装插件**：**只有手动安装这一条路**（2026-09-21 所有者已定：`pingod/Macast` 继续私有，
不再讨论"改公开 / 另立公开仓库"）。未登录的浏览器和 Macast 的匿名下载都读不到私有仓库，
所以设置页「插件」页里的「可安装」卡片对别人只是展示，点下去会失败。走这两条：

1. 设置页（<http://127.0.0.1:58880/>）→「插件」→ 在输入框里贴一个**你这边可达的** `.py` 直链 →「从网址安装」；
2. 或直接把 `.py` 放进 `~/Library/Application Support/Macast/renderer/`（协议类放 `protocol/`）——
   **放完即热生效，不用重启**（手工放进的目录需要有个 `__init__.py`，装过任一插件就有了）。

想确认自己这台机器上到底通不通：`python scripts/check_index_reachability.py`，
私有状态下稳定报 `INDEX_PRIVATE`（退出码 2）**是预期信号，不是待修的 bug**。

---

## 1. Screen Mirror —— 把这台电脑的屏幕推出去

菜单栏切到 `Screen Mirror`，菜单结构：

```
Screen Mirror v0.9
开始镜像 / 停止镜像
输出目标 ▸  Chromecast / Google TV | Chromecast 低延迟（实验 · 无声音）
           | DLNA 电视（老电视，MPEG-PS）| 浏览器（打开网址即可看）
           └ 设备列表（Chromecast 或 DLNA 电视，含「重新搜索」）
兼容档位 ▸   （只在输出目标 = DLNA 电视时出现）
复制观看地址  （只在浏览器目标 + 正在镜像时出现）
画质 ▸      360p · 2.5 Mbps | 720p · 5 Mbps（默认）| 1080p · 10 Mbps | 原始分辨率 · 12 Mbps
采集屏幕 ▸   多显示器时出现；拔掉显示器会自动回落第一块
显示鼠标指针
硬件编码（VideoToolbox）  （仅 macOS，且探测到 ffmpeg 认这个编码器时才允许开）
系统声音 ▸   当前状态一行 + 一键设置 / 恢复原声音输出
（镜像中）状态行：已镜像 m:ss · 码率 · 观看端/在途 · 丢块/丢帧
```

四个目标共用同一份采集与编码，区别只在**封装**和**怎么交给对方**。

### 1.1 目标：Chromecast / Google TV（默认，兼容通道）

首次设置：

1. 输出目标 → **Chromecast / Google TV**。
2. 展开「输出目标」，下面就是后台 mDNS 搜到的设备（第一次展开可能显示
   「搜索中…再展开一次菜单」，再展开一次即可）。点选一台。
3. 点「开始镜像」。插件走 deviceauth → CONNECT → LAUNCH → LOAD，
   把一条 `http://<本机>:<端口>/stream/<随机id>.ts` 的**实时 MPEG-TS** 推给电视。

现场读法：菜单底部的状态行报「已镜像 · 码率 · 观看端数 · 丢块数」。
**丢块在涨是正常的**（慢消费者丢整块，绝不阻塞编码器）；码率明显低于档位设定值
说明编码器跟不上，降一档画质。

排障：电视黑屏但状态行有码率 ⇒ 看 `ffmpeg` 是否只推了音频；
日志在 `~/Library/Application Support/Macast/logs/ScreenMirror.log`（设置页 →「日志」下拉选它）。

### 1.2 目标：Chromecast 低延迟（Cast Streaming，实验）

同一条 Chromecast，换 Chrome「投射桌面」用的那套协议：不 serve HTTP、不发 LOAD，
LAUNCH 镜像接收器 `0F5096E8` → OFFER/ANSWER → **UDP 上推 RTP**。

- **设备不认就直接回落** 1.1 的 LOAD 通道，不需要你拆摊子重来。
- **没有声音**（菜单里写明了）。
- **码率上限 4.5 Mbps**，天花板是纯 Python 的软件加密速度，不是网络。
  所以选 1080p/原始分辨率也不会更快，菜单会显示「低延迟通道上限 4.5 Mbps」并把帧尺寸
  信箱化到一个固定尺寸 —— 这不是 bug，是这一条通道的实际能力。
- 状态行多两个字段：**在途**（设备还没确认的帧数，上限 12）与**丢帧**
  （因窗口满而整帧丢弃的张数）。在途长期贴着 12 ⇒ 设备消费不动，回落 1.1。
- **验证状态：未经任何真实电视验证。** 字段布局是从 openscreen 转写的，
  打桩测试只证明字节自洽。第一次上真机请用探针，它 `import` 的就是插件本体：

  ```shell
  .venv/bin/python scripts/cast_streaming_probe.py --live   # 采真桌面
  .venv/bin/python scripts/cast_streaming_probe.py --source /path.mp4 --dump /tmp/out.h264
  ```

### 1.3 目标：DLNA 电视（没有 Google 栈的老电视）

原理：十年前的 MediaRenderer 没有"直播"概念，所以**把直播伪装成一个有限长度的文件**
推给它去 fetch。代价写在开始提示里：**「约 37 秒延迟」**（要先攒够 20 MiB 缓冲才敢交出去，
否则电视开局那次探测就会判定文件坏了）。

首次设置：

1. 输出目标 → **DLNA 电视**，在下面的设备列表里点一台。
2. 「兼容档位」先保持默认 **MPEG-PS · PAL 576p（老电视首选）**。
3. 点「开始镜像」，然后**耐心等那半分钟**。

五档按"老电视可能性"从高到低排：`ps-pal` / `ps-ntsc`（北美、日本老电视）/
`ts-mpeg2`（认 TS 不认 PS）/ `ts-h264` / `mkv-h264`（只认 MKV 的电视、Kodi）。
看门狗每 5 秒问一次 `GetTransportInfo`，电视掉出 `PLAYING` 就重投；`RelTime` 在动才算活着。
**连续失败会自动换下一档**，五档试完会提示你手动选。

- 状态行是另一种形状：`已镜像 m:ss · 档位 … · 电视 … · 缓冲 … MiB`。
  「电视 未上报」意味着它连 `GetTransportInfo` 都不答 —— 换档。
- 这个目标的观看地址**会重放**（按绝对字节偏移从 48 MiB 环形缓冲里取），
  因为电视会 HEAD、探边界、断线重连同一偏移。Chromecast 目标故意**不重放**
  （重放 = 它从此一直慢几秒）。
- **验证状态：没有老电视可验**，替身是 Macast 自己的 DLNA 接收端
  （它真的用 lxml 解析并解嵌 DIDL，所以"我们发的东西连自己都不认账"能抓到，
  但抓不到"某品牌固件挑食"）。

### 1.4 目标：浏览器（任何一台能开网址的设备）

不装任何 App：局域网里的电脑/平板/手机浏览器打开一个网址就能看。

1. 输出目标 → **浏览器（打开网址即可看）**，点「开始镜像」。
2. 菜单里会出现「复制观看地址」和地址本身（形如
   `http://<本机>:<端口>/browser?token=<每会话随机>`）。粘到目标浏览器。
3. 页面里是 fragmented MP4 → MSE 播放；没有 MSE 的浏览器自动回退渐进式 `<video>`。
4. **声音默认是关的**：浏览器自动播放策略要求"静音起播"。点页面右上角
   「开声音」按钮，或直接点画面任意处（任何真实手势都算同意）。
5. 晚加入的观看端会先拿到 init segment + 最近 8 MiB 尾部，所以能中途接入。
6. 页面顶部条会显示延迟/缓冲状态；`LIVE_EDGE=3` 秒，缓冲区超出就跳到直播边缘。

安全说明（为什么地址长这样）：镜像页地址里的 token 是**每会话随机**的，
故意不复用 Macast 的常驻管理令牌 —— 镜像 URL 会被复制、转发、贴进群，
而管理令牌泄露一次等于永久开放整套管理 API。地址请只发给愿意给他看屏幕的人。

DLNA/Chromecast 投屏推过来时会**让位**并转投该 URL（Bridge 行为）；
**浏览器目标下推送会被明确拒绝**，不会偷偷杀掉你的镜像。

### 1.5 系统声音（三个平台不一样，这不是我们的选择）

| 平台 | 状态 | 首次设置 |
|---|---|---|
| **macOS** | 需要 BlackHole 虚拟声卡（**FFmpeg 发行版至今抓不到 mac 系统音频**） | 菜单「系统声音 → 一键设置（BlackHole + 多输出设备）」。它**先判断你的机器处在哪一格**（下表），再决定做什么：只有"真的没装"才下载官方 `.pkg` → **弹出图形安装器，你输一次密码**（`.pkg` 无法静默安装，别期待全自动）→ 等设备出现 → 建/复用名为 `Macast Screen Mirror` 的多输出聚合设备 → 把默认输出切过去。**任何一步失败就降级**：打开「音频 MIDI 设置」+ 文字指引。装完菜单里会出现「恢复原声音输出」，走它回去 |
| **Linux** | PulseAudio 直接有 tap | 采集口是 `<sink>.monitor`，插件自动探测；Wayland 下**没有画面可采**（x11grab  only），会明说 |
| **Windows** | 仅画面 | 这一版不做 Windows 音频，菜单里写明「Windows 下仅画面」 |

**五种机器状态、五种做法**（v0.9）。之前它把"驱动文件在磁盘上"这种情况**写在提示里说会跳过安装，
然后照样重新下载一次安装器** —— 于是出现「装了却没有设备 → 再点又装一遍 → 永远装不完」。
现在"跳过"是流程里真正的跳过，不是文案：

| 你的机器 | 怎么判出来的 | 一键设置做什么 |
|---|---|---|
| 能采集系统声音 | ffmpeg 的采集设备表里有 BlackHole | 什么都不装，只把多输出设备建好/复用 |
| 驱动已加载，但采集看不到 | CoreAudio 有、ffmpeg 没有 | **不重装也不重载**（两者都改变不了这件事）：照样把聚合设备建好，然后指名去「系统设置 → 隐私与安全性 → 麦克风」给 Macast 授权，重启 Macast 再点一次。旧产物（v0.9 之前打包的 `.app`）没声明麦克风用途，系统连授权弹窗都不会给 |
| 驱动在磁盘上，但音频服务没加载 | `/Library/Audio/Plug-Ins/HAL/BlackHole*.driver` 在、设备表里没有 | **跳过下载与安装**，只要一次管理员密码**重载音频服务**（官方 `.pkg` 的 postinstall 只改权限、从不重启 coreaudiod。**不需要重启电脑**，重启只是等价手段） |
| 只有残留的安装记录 | pkgutil 有记录、文件不在 | 重新下载官方 `.pkg` —— 这就是"上次装了却没有设备"的原因 |
| 什么都没装过 | —— | 完整走一遍：下载 → 校验 sha256 → 安装器 → 等设备安装 → 重载 → 建聚合设备 |

**每一格都保证同一件事：再点一次不会重新下载已经装好的东西。**

镜像期间持有 `caffeinate -dimsu` 休眠断言（合盖/息屏会把镜像打断），停止时释放。

---

## 2. Local File Caster —— 把这台电脑磁盘上的文件投到电视

菜单栏切到 `Local File Caster`（与 Screen Mirror 互斥）。

```
Local File Caster v0.1
<状态行：文件名 · 直通/转码/直投网址/系统声音 · 3/12 · 00:41/01:32>
播放控制 ▸   暂停/继续 · 下一个 · 上一个 · 停止（让电视回主页）
输出目标 ▸   Chromecast / Google TV | DLNA 电视 + 设备列表
媒体文件夹 ▸ 选择文件夹… → 文件列表 →「全部按顺序播」
播放列表   ▸ 当前队列（勾中的正在播）/ 清空列表 / 播完自动下一个
音轨与字幕 ▸ 音频：跟随文件 / 各音轨；字幕：不投 / 各字幕轨；音画同步（±ms）
同步与码率 ▸ 2/4/6/10/16 Mbps + 转码用硬件编码器（可用时）
处理方式   ▸ 自动（按文件判断）| 强制直通（可能黑屏）| 强制转码
只投系统声音
```

**最常用的一条路**：输出目标选一台设备 →「媒体文件夹 → 选择文件夹…」→
点一个文件。就这么多了。

### 2.1 「自动」在做什么，为什么值得信

每个文件先过 **ffprobe**，决定两件事之一：

- **直通**（copy）：电视能自己解码 ⇒ 用一个内置的 Range/206 HTTP 服务
  **按字节**供原文件。电视自己的暂停/seek/音量全都作用在真实文件上，我们什么都不做。
- **转码**（convert）：AVI/MKV/HEVC/DTS/AC-3、你手挑的第二条音轨、内嵌字幕
  ⇒ ffmpeg 边播边转成 MPEG-TS。实现上写的是一个**会增长的文件**并由 HTTP 供它的字节区间
  （不是管道 —— 管道没有长度，DLNA 渲染器对没长度的流直接拒播），
  读取方跑得太快会**等编码器**而不是拿到短答。**seek 会从新位置重启编码器**，
  这是"转码进行中"能做到的诚实上限。

菜单底部那一行会写明这次是哪条路和原因（`直通：…` / `转码中：…，电视吃不下`）。
出问题时的第一判据就是这行；「处理方式 → 强制直通/强制转码」是用来对照验证它的。

转码会占临时目录空间（菜单里报出路径与上限，"结束即删"）；空间不够就回落到
"降码率以适配被 2 GiB 上限卡住的长度" —— 宁可画质下来，也不让进度条说谎。

### 2.2 音轨 / 字幕 / 同步，以及它们的边界

- **音轨选择与 A/V 偏移只在转码路径上有效**。直通是把原始字节交出去，
  电视的轨道列表没法从这里重排 —— 所以一旦你手挑了非默认音轨，
  决策函数就会把这个文件改派给 ffmpeg，菜单也是这么写的。
- **字幕三条路**，因为目标确实不同：Chromecast 把内嵌字幕转成 WebVTT
  并写进 LOAD 的 `textTracks`（由接收端画）；转码路径在**这台机器的 ffmpeg 带 libass**
  时烧进画面；**DLNA 渲染器两条都没有**。
- **停止发的是 `QUIT_APP`，不只是 STOP**。裸 STOP 会把接收端 App 停在最后一帧，
  这就是"我停了投屏但电视还卡着我的画面"这类 bug 报告的来源。
- **看门狗**（就是 mkchromecast 的 `--hijack` 那个想法）：每隔几秒问设备在干什么，
  我们的 app/URI 不见了就从中断位置重投，重试预算有限并且会认输 ——
  **手机想看电视就把电视让给它**。设备上报"播完了"，播放列表自动前进。

### 2.3 只投系统声音

勾选后不投画面，只把这台机器的系统声音做成音频档推过去。
采集口与 §1.5 同一套（macOS 需 BlackHole、Linux pulse monitor、Windows 没有）。
目标设备不支持时会提示「只有声音；目标设备不支持时请先关掉头选」。

---

## 3. AirPlay Screen Mirror —— 让 iPhone 把屏幕镜像**进来**

协议类插件（`<macast.protocol>`），与任意渲染器共存。它**不实现** AirPlay 镜像，
而是监督 **uxplay**（配对、stream key、AES-128-CTR 数据通道、GStreamer 解码都归它）。

**硬前置：这台机器上没有现成的 uxplay。** Homebrew 没有 formula，上游 release 里
也没有 macOS 二进制 —— 必须自己编译：

```shell
sudo xcode-select --install                       # 1) 命令行工具
brew install cmake libplist openssl@3             # 2) 依赖
# 3) GStreamer 去 https://gstreamer.freedesktop.org/download/ 装，
#    runtime 和 -devel 两个 .pkg 都要（落在 /Library/Frameworks/GStreamer.framework）。
#    别和 Homebrew / MacPorts 的 GStreamer 混用。
git clone https://github.com/FDH2/UxPlay && cd UxPlay
cmake . && make && sudo make install              # 4) 装
```

装完在「插件」页启用 `AirPlay Screen Mirror`，插件会用它自己的名字（= Macast 的
友好名） advertise `_airplay._tcp`，iPhone 的屏幕镜像列表里就能看见。

要知道的三件事：

- **别和内置 AirPlay 协议同时开**：手机会列出两个名字几乎一样的接收端，插件启动时
  会提示一次。**也别和 `AirPlay Audio (RAOP)` 同时开** —— uxplay 本身就是个完整
  AirPlay 接收端（自带横幅写着 mirroring and audio-streaming），
  它和 shairport-sync 会广播同一个名字，结果只有一个应答。
- **选项写在 Macast 自己的配置目录里**（`-rc <file>`），绝不碰 `~/.uxplayrc`
  （那是用户手写的文件）；用 `-rc` 而不是 `$UXPLAYRC`，因为后者指向不存在的文件时
  uxplay 会**静默回落**到 `~/.uxplayrc`，写失败就表现为"我的设置没生效"。
- **不加 `-p`**：那是 legacy 端口集，含 TCP 7000 —— Macast 自己的 AirPlay 协议要用的端口。
- 一期是 uxplay **自己开一个窗口**放画面；不映射成 DLNA 播放状态
  （镜像会话里没有 URL 也没有进度，连接/断开以通知形式报）。
- **验证状态：真 iPhone → 真 Mac 没测过，这台机器上根本没有 uxplay。**
  打桩测试覆盖的是：找不到二进制的正常路径、选项文件内容、四类事件各且只通知一次、
  意外退出时把 uxplay 最后几行 error 带出来、reload 与 stop。
- Apple 随时可能砍掉 Legacy AirPlay —— 届时这一条会**静默失效**，不是我们的代码坏了。

## 4. AirPlay Audio (RAOP) —— 让 iPhone 把声音推到这台电脑

同样是监督型协议插件，外部程序是 **shairport-sync**：

```shell
brew install shairport-sync        # macOS/Linux；Linux 上也可 apt install
```

启用后插件生成一份**最小配置**（只有 `general = { name = ... }`）并拉起它，
上报连接/断开。输出后端、混音器、端口都是你那台机器的系统决定，
插件故意不猜 —— 猜了就会破坏已经能用的配置。`uses_ssdp = False`。
与 `AirPlay Screen Mirror` 的冲突见上一条。

---

## 5. 出问题先看哪里

```shell
# 环境自检：依赖、端口占用者、可广播网卡、mpv、代理变量、在线索引三问，
# 以及下面这一整段发送端前置
env -u PYTHONPATH .venv/bin/python scripts/selfcheck.py

# 在线索引到底能不能装（退出码 0/2/3，可进 cron/CI）
env -u PYTHONPATH .venv/bin/python scripts/check_index_reachability.py
```

`selfcheck.py` 的「sender plugins」段逐项回答这几个问题：

| 它问什么 | 为什么值得问 |
|---|---|
| ffprobe 在不在 | 没有它，本地文件投屏无法判断"直通还是转码" |
| `-encoders` 里有没有 `libx264` / `h264_videotoolbox` / `mpeg2video` / `ac3` | **各对应一条会静默失效的链路**：前两个是镜像，后两个是老电视的 MPEG-PS 档位 |
| 有没有 libass（`subtitles` 滤镜） | 没有就只能走 WebVTT 字幕，烧字幕那条路不可用 |
| avfoundation 列没列出屏幕 | 没列出 = 屏幕录制权限没给。（v0.8 之前这一条**在真机上永远答"没有"** —— 是解析器读不懂 ffmpeg 的输出，不是机器没屏幕） |
| 系统音频采集口 | macOS 先 glob `/Library/Audio/Plug-Ins/HAL/BlackHole*.driver`，**再问 ffmpeg 列不列得出它** —— 文件在盘上但 coreaudiod 没加载 = warn（v0.9 之前这里会说 ok，而插件说没有采集口，两边看起来像互相打脸）；Linux 问 `pactl` 要 sink monitor；Windows 直接说"仅画面" |
| 转码临时目录剩余空间 | 转码写的是"会增长的文件"，空间在半路用尽比开头就报更难查 |
| 局域网里有没有东西可投 | 真发一次 mDNS browse（`_googlecast._tcp` / `_airplay._tcp`）和一次 SSDP `MediaRenderer` 探测。**没搜到电视的时候，"菜单里设备列表是空的"就不是插件的问题** |

这一段是**只读**的：它直接读 `macast_setting.json` 的文本，不经过 `Setting`，
所以跑自检不会改你的配置。回归用例 **Part 32** 守着它和两个插件说的是同一套话
（编码器集合、查找目录、设置键名、mDNS/SSDP 目标串），谁漂了就红。

- **日志**：设置页 →「日志」。每个插件**独立成文件**
  （`~/Library/Application Support/Macast/logs/<LoggerName>.log`，下拉框选），
  不会淹没在 `macast.log` 里；每次启动清空，1 MB × 1 份轮转。
- **镜像起不来，报「ffmpeg 没有列出任何屏幕采集设备（avfoundation）」** ⇒
  要么真没接屏幕，要么**屏幕录制权限没给**（macOS）。给完权限重启 Macast 再试。
  （v0.8 之前这条提示在**有摄像头的机器上也会误报** —— 解析器读不懂 ffmpeg
  的真实输出，见 `AGENTS.md` §4.2 末。）
- **"有声音没画面"**：`grep -aE "video-reconfig|audio-reconfig" ~/Library/Application\ Support/Macast/logs/*.log`
  —— 只有 `audio-reconfig` 说明流里根本没有视频轨（发送端问题）。
- **系统声音「装了却没有设备 / 每次点一键设置都要重装」**：先跑 `scripts/selfcheck.py` 看它落在
  §1.5 那张表的哪一格。最常见的是**驱动在磁盘上但音频服务没加载它**（官方 `.pkg` 的 postinstall
  只改权限，从不重启 coreaudiod），**不需要重启电脑**：再点一次一键设置只会要一次管理员密码去重载，
  不会再下载。第二常见的是**麦克风权限**（CoreAudio 看得见、ffmpeg 看不见），
  那条路的终点在「系统设置 → 隐私与安全性 → 麦克风」，装多少个 `.pkg` 都不会改变它。
- **电视找不到这台电脑**：`dns-sd -B _googlecast._tcp` /
  `curl -s 'http://127.0.0.1:58880/api?query=status'`；虚拟机网桥与 Tailscale
  地址会在 `AGENTS.md` §4.1 那一族里出现（只广播承载默认路由的网卡）。
- **本地测试被代理搅局**：`env -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY <cmd>`。
- **改了设置页前端没反应**：`setting.html` 在启动时缓存，**必须重启应用**。

## 6. 这一族明确不做

FairPlay / 任何 DRM（App 拒绝被镜像就是黑屏，这是设计）· Miracast / Wi-Fi Display
（macOS 没有 P2P API）· AirPlay 镜像的**发送端**（接收端由 §3 补）·
鼠标高亮 · 区域捕获 · 4K 档 · Windows 系统音频 · Cast Streaming 的音频（二期）。
