<img align="center" src="macast_slogan.png" alt="slogan" height="auto"/>

# Macast

[![visitor](https://visitor-badge.glitch.me/badge?page_id=xfangfang.Macast)](https://github.com/xfangfang/Macast/releases/latest)
[![stars](https://img.shields.io/badge/dynamic/json?label=github%20stars&query=stargazers_count&url=https%3A%2F%2Fapi.github.com%2Frepos%2Fxfangfang%2FMacast)](https://github.com/xfangfang/Macast)
[![plugins](https://img.shields.io/badge/plugins-6%20built--in-blueviolet)](https://github.com/pingod/Macast/tree/main/macast/plugins)
[![build](https://img.shields.io/github/workflow/status/xfangfang/Macast/Build%20Macast)](https://github.com/xfangfang/Macast/actions/workflows/build-macast.yaml)
[![mac](https://img.shields.io/badge/MacOS-10.14%20and%20higher-lightgrey?logo=Apple)](https://github.com/xfangfang/Macast/releases/latest)
[![windows](https://img.shields.io/badge/Windows-10-lightgrey?logo=Windows)](https://github.com/xfangfang/Macast/releases/latest)
[![linux](https://img.shields.io/badge/Linux-Xorg-lightgrey?logo=Linux)](https://github.com/xfangfang/Macast/releases/latest)

[README_EN](README.md)

Macast是一个跨平台的 **菜单栏\状态栏** 应用，用户可以使用电脑接收发送自手机的视频、图片和音乐，支持主流视频音乐软件和其他任何符合DLNA协议的投屏软件。


😂 **请尽量使用英语在Github交流，如果喜欢的话可以点个star关注后续更多协议支持的更新**

## 本分支（pingod/Macast）的新增内容

在 DLNA 之外，本分支把 Macast 做成了一个**多协议并发**的接收端，并把设置页做成了
可视化管理台：

- **Chromecast 接收端**：mDNS 广播 + Cast v2（TLS 8009 + setup HTTP 8008）。
  Chrome / VLC / Android 等发送端可直接投屏。未做 Google 设备认证，官方 SDK 发送端可能失败。
- **AirPlay 接收端**：mDNS + RTSP，支持视频 URL 投屏。音频(RAOP)与屏幕镜像未实现。
- **三协议并发**：DLNA / Chromecast / AirPlay 可同时在线、各自可被发现，互不干扰
  （`macast/protocol_group.py`）。
- **网卡选择**：设置页 → 状态 → 网络与广播。多网卡（虚拟机网桥 / VPN / Tailscale）时
  可显式指定广播网卡；不选则自动用承载默认路由的网卡。避免"能搜到但投不上"。
- **插件热插拔**：设置页 → 插件，可**启用 / 停用 / 卸载 / 安装**，全部**即时生效、无需重启**。
  卸载是可恢复的（文件移到配置目录 `.trash/<时间戳>/`）。
- **插件索引自持**：设置页「可安装」分组的数据源是本仓库的 [`plugins/info.json`](plugins/info.json)（现在**为空** —— 第一方插件已全部内置，
  这个索引只留给第三方 / 用户自建插件），
  不再读上游 `xfangfang/Macast-plugins`（它的 6 个插件现已全部内置，继续读只会产生重复卡片和
  指回上游旧文件的「可更新」角标）。地址只写在 `macast/plugin_repo.py` 一处，由 `/api?query=plugin-info`
  下发；浏览器按 `raw.githubusercontent.com` → `ghproxy` 代理 → `cdn.jsdelivr.net` 顺序回退，
  全都不通时只显示本机插件。插件安装地址**固定到 commit SHA**，所以 CDN 缓存多旧都不会给错文件。
  **注意**：仓库保持私有，所以这份索引对匿名用户拉不到 —— 设置页的「可安装」卡片在别的机器上
  只是展示，安装要走手动（见下方「普通用户」）。
- **9 个自研插件，已全部内置**（设置页 → 插件 → 直接启用 / 停用 / 卸载，即时生效）：
  yt-dlp 下载/边下边播、外部播放器
  （VLC / MPC-BE / mpv.net）、角落置顶小窗（含实验性壁纸模式）、自动化钩子（投屏/暂停/停止时
  执行你的命令）、Chromecast 中继（把收到的投屏转投给电视）、**屏幕镜像**（把这台机器的画面
  实时投给 Chromecast / 只认 DLNA 的老电视 / 任意浏览器，三平台，macOS 可一键装好系统声音）、
  **本地文件投屏**（磁盘上的文件/播放列表投给电视：能原生解码就按字节直供、远端自己暂停拖动，
  否则 ffmpeg 边播边转，含音轨/字幕选择、自动连播与被抢占后重投）、AirPlay 音频（RAOP，
  监督 shairport-sync）、**AirPlay 镜像接收**（让 iPhone 把屏幕镜像到这台 Mac，监督 uxplay）。
  RAOP 与 AirPlay 镜像接收是协议插件，可与任意渲染器共存；其余 7 个是渲染器插件，一次只能选一个。
- **网页投屏入口**：`GET /api?query=cast&url=<绝对地址>&token=<令牌>`，给 iOS 快捷指令、
  书签、`curl`、脚本用，绕开 DLNA 发现也能投屏；`POST cast-uri` 继续服务设置页的重投。
  令牌是**常驻**的（存在 `macast_setting.json` 的 `Api_Token`），在「状态 → 网页投屏入口」
  里可一键复制；GET 版即使来自本机也要求令牌 —— 任意网页都能往 `127.0.0.1` 发 GET，
  不能让它们指使你的 Mac 播片。
- **播放状态如实上报**：Chromecast 的 `LOAD` 不再无脑回 `PLAYING`，而是等播放器确认；
  播放器报错则回 `LOAD_FAILED`。

**接手开发 / 排查问题请先读 [`AGENTS.md`](AGENTS.md)**（代码地图、踩坑清单、发版流程），
真机验证与历史问题复盘见 [`docs/Cast-AirPlay-Testing.md`](docs/Cast-AirPlay-Testing.md)。
上面那批发送端插件**怎么用**（每个目标的首次设置流程：Chromecast、低延迟通道、老电视、
浏览器、本地文件、uxplay / shairport-sync）见
[`docs/Casting-Suite.md`](docs/Casting-Suite.md)。



## 安装

进入页面选择对应的操作系统下载即可，应用使用方法及截图见下方。

- ### MacOS || Windows || Debian

  下载地址1:  [Macast 最新正式版 github下载](https://github.com/xfangfang/Macast/releases/latest)

  下载地址2:  [Macast 最新正式版 gitee下载（上面访问无效可使用此备用链接）](https://gitee.com/xfangfang/Macast/releases/)

- ### 包管理
  你也可以使用包管理器安装macast  
  ```shell
  # 需要 python>=3.6
  pip install macast
  ```

  请查看我们的wiki页面获取更多的包管理相关信息（如：aur）: [Macast/wiki/Installation#package-manager](https://github.com/xfangfang/Macast/wiki/Installation#package-manager)  
  Linux用户使用包管理器安装时运行可能会有问题，建议替换如下两个库为我修改过的库（分别负责`菜单显示`与`文本复制`）：

  ```shell
  pip install git+https://github.com/xfangfang/pystray.git
  pip install git+https://github.com/xfangfang/pyperclip.git
  ```

  **Linux用户如果安装或运行有问题，可以查看 [这里](https://github.com/xfangfang/Macast/wiki/Installation#linux)**

- ### 从源码构建或运行

  构建请参阅: [Macast Development](docs/Development.md) 和 [build-macast.yaml](https://github.com/xfangfang/Macast/blob/main/.github/workflows/build-macast.yaml)
  
  运行只需要clone仓库，根据不同的操作系统于requirements文件中安装相关的包，并在项目根目录运行 `Macast.py` 即可。



## 使用方法

- **普通用户**  
  1. 打开应用后，**菜单栏 \ 状态栏 \ 任务栏** 会出现一个图标，这时你的设备就可以接收来自同一局域网的DLNA投放了。
  2. 点那个图标就是菜单：**开始 / 停止接受投屏** → 正在播的东西（标题、**停止播放**、**复制视频链接**）
     → **打开设置页** → **投屏历史**（最近 10 条，点一下重投，末尾可清空）→ **设置** → **退出**。
     全部功能都在设置页里，菜单只留这几样天天用得上的。

- **进阶用户**  
  1. 以前要手动下载的插件（IINA、Web、Live、PotPlayer、PIFMRDS、NVA 协议）现在**都已内置**，
     在设置页 → 插件里直接启用 / 停用 / 卸载即可，即时生效、不用重启。
  2. 本仓库自研的 9 个插件同样**全部内置**（随包发布，无需安装、即时生效）：
     **yt-dlp Downloader**（投屏 = 下载到本地，或边下边播）、**External Player**
     （用你自己的 VLC / MPC-BE / mpv.net 播放）、**Floating Player**（角落置顶小窗，
     含实验性壁纸模式）、**Automation Hooks**（投屏 / 暂停 / 停止时执行你的命令）、
     **Chromecast Bridge**（把收到的投屏转投给另一台 Chromecast）、
     **Screen Mirror**（把这台机器的屏幕实时镜像到 Chromecast / 只认 DLNA 的老电视 /
     任意浏览器打开一个网址；三平台，macOS 可一键装好系统声音；**控制项全在设置页的「电脑投屏」
     页签里，菜单栏一项都不放**（2026-09-23 定）。从「正在采集你的桌面」里出来的路有三条，
     不必恰好开着那个页签：页签上的「停止镜像」、菜单栏「停止接受投屏」（停掉整个服务）、
     以及「设置 → 选择播放器」切走渲染器 —— 后两条触发的是同一个 `stop()` 收尾），
     **Local File Caster**（把磁盘上的文件 / 播放列表投给电视：能原生解码就按字节直供、
     遥控器上的暂停拖动直接生效，否则 ffmpeg 边播边转，含音轨与字幕选择、自动连播、
     被别人抢占后自动重投、停止时把电视还给机顶盒）、
     **AirPlay Audio (RAOP)**（监督 shairport-sync，接收 iPhone 的 AirPlay 音频）、
     **AirPlay Screen Mirror**（监督 uxplay，让 iPhone 把屏幕镜像到这台 Mac —— macOS 上
     uxplay 没有 Homebrew 包也没有官方二进制，要自己编译，插件的日志里给了完整配方）。
     RAOP 与 AirPlay Screen Mirror 是协议插件，可与任意渲染器共存；其余 7 个是渲染器插件，
     **一次只能选一个**（菜单栏切换）。
     yt-dlp / ffmpeg / shairport-sync / uxplay / 外部播放器都要你自己先装；设置页拉不到索引时只会显示
     本机插件，不影响使用。
     > ⚠ **在线「安装」对别人不可用，这是已定的产品状态不是待修的 bug**：`plugins/` 索引现在只收
> 第三方 / 用户自建插件（目前为空 —— 第一方插件已全部内置），而本仓库**保持私有**
     > （所有者决定，2026-09-21），而 jsDelivr / raw 读不到私有仓库，Macast 下载插件时也不带
     > 凭据 —— 卡片能显示、点安装会失败（少数条目还在回 200，那是 CDN 在仓库还可读时缓存的
     > 副本，会逐条过期）。所以 `plugins/` 请当作**源码目录**，官方承诺只有手动安装这一条路：
     > 拿 `.py` 用设置页的「从网址安装」贴一个可达的地址，
     > 或直接放进 `~/Library/Application Support/Macast/renderer/`（协议插件放 `protocol/`），
     > 放好即生效、无需重启。
     > 想知道自己这台机器上到底通不通：`python3 scripts/check_index_reachability.py`
     > （私有状态下稳定报 `INDEX_PRIVATE` = 预期结果）。
  3. 支持修改默认播放器的快捷键或其他参数，见：[#how-to-set-personal-configurations-to-mpv](https://github.com/xfangfang/Macast/wiki/FAQ#how-to-set-personal-configurations-to-mpv)

- **程序员**  
  1. 可以依照教程完成自己的脚本，快速地适配到你喜欢的播放器，或者增加一些新的功能插件，比如：边下边看，自动复制视频链接等等。教程和一些示例代码在：[Macast/wiki/Custom-Renderer](https://github.com/xfangfang/Macast/wiki/Custom-Renderer)  
  2. 也可以参考内置的 [nirvana](macast/plugins/protocol/nirvana.py) 快速适配第三方魔改的DLNA协议。

内置插件放在 [macast/plugins/](macast/plugins/)；欢迎向 [pingod/Macast](https://github.com/pingod/Macast) 提 PR 或提 issue。  
**注意：不要轻易加载非官方仓库下载的插件，这里“插件”本身是可以运行在电脑上的任意代码，不建议加载非官方提供的插件。**


## 开发计划

- [x] 完成第一版应用，支持MacOS
- [x] 添加对Linux和Windows的支持
- [x] 完善协议，增强软件适配性
- [x] 统一MacOS与其他平台的UI
- [x] 添加多播放器支持
- [x] 添加多网卡支持
- [x] 添加自定义端口和自定义播放器名称
- [ ] 改进目前的播放器控制页面
- [x] 增加插件商店
- [x] 添加bilibili弹幕投屏
- [x] 增加 Chromecast 接收端协议（mDNS + Cast v2，URL 投屏，见 macast/protocol_cast.py）
- [x] 增加 AirPlay 接收端协议（mDNS + RTSP，视频 URL 投屏，见 macast/protocol_airplay.py）
- [x] 多协议并发：DLNA / Chromecast / AirPlay 可同时在线并各自可被发现（见 macast/protocol_group.py）
- [x] 网卡选择界面（设置页可指定用于发现/广播的网卡）
- [x] 插件热插拔：设置页启用 / 停用 / 卸载 / 安装，即时生效无需重启
- [x] 播放状态如实上报（Chromecast LOAD 等播放器确认后才回 PLAYING）
- [ ] AirPlay 屏幕镜像 / 音频(RAOP) 支持（需实时编码 / ALAC 解码，超出当前范围）
- [ ] 支持airplay 完整能力（当前为视频 URL 投屏子集）

## 出现问题的可能原因及解决办法（更详细内容见项目的wiki）

0. 应用闪退  
    大概率是由windows的hyper-v占用端口号导致的，建议修改hyper-v占用的端口号范围或修改本应用的启动端口号（[Macast配置文件位置](https://github.com/xfangfang/Macast/wiki/FAQ#where-is-the-configuration-file-located)）
2. 无法搜索到Macast——被电脑防火墙拦截  
    手机尝试访问 http://电脑ip:1068，如:192.168.1.123:1068 如果出现helloworld 等字样排除问题。  
    *具体端口号见应用菜单设置的第一项，如果没有则为默认的1068*
2. 无法搜索到Macast——路由器问题  
    路由器需要开启UPnP，关闭ap隔离，确认固件正常（部分openwrt有可能有问题）
4. 无法搜索到Macast——手机软件有问题  
    可以重启软件或更换软件尝试，或向其他投屏接收端电视测试
    尝试在搜索页面等待久一点（最多1分钟如果搜不到那应该就是别的问题了）
    如操作系统为IOS，注意要开启软件的**本地网络发现**权限
5. 无法搜索到Macast——网络问题  
    请确定手机和电脑处在同一网段下，比如说：电脑连接光猫的网线，手机连接路由器wifi，这种情况大概率是不在同一网段的，可以查看手机和电脑的ip前缀是否相同。
6. 无法搜索到Macast——其他未知问题  
    尝试在同一局域网手机投电视，如果可以正常投说明问题还是出在电脑端，继续检查电脑问题或查看如何报告bug

## 对于反馈问题的说明

  1. 先确保自己有认真读过使用说明
  2. 在提issue时，请及时地回复作者的消息，太多人提完问题或者反馈就消失，提之前先看别人问过没有，提之后积极参与讨论。如果您做不到回复issue，请不要随便提issue浪费开发者的时间。
  3. 遇到问题不要只说现象，请附带所有你认为能帮助开发者解决问题的信息，这会让开发者认为你很聪明，且极大的帮助加快解决你的问题与节省开发者的时间。
  4.  如果你遇到了某个问题，请优先考虑是自己没有看使用说明，比如我遇到过很多很多遭遇了投屏搜索不到的用户，直接评论说，“这个软件用不了”。用不了那是我编出来逗你玩的吗？检查一下自己的防火墙OK？
  5. 如果你不能自己去写，请不要提出那种很难实现的需求，开发者愿意解决的是：“我有个需求，讨论一下要怎么实现” 而不是 “可以帮我给这个软件加上***功能吗？”

## 如何报告bug
  准备以下信息，推荐到Github报告问题，点击 **[new issue](https://github.com/xfangfang/Macast/issues/new/choose)** 去反馈问题：
  1. 你的电脑系统类型和版本：如Win10 20h2
  2. 你使用的手机系统和软件：如 安卓 bilibili
  3. bug复现：如何复现bug与bug是否可以稳定复现
  4. 程序运行的log（复现问题时候的log）：  
    - windows下载debug版应用, 拖入cmd执行，复现问题后，关闭应用，ctrl-a全选复制：[download debug](https://github.com/xfangfang/Macast/releases/latest)  
    - mac 终端输入：`/Applications/Macast.app/Contents/MacOS/Macast` 回车运行，复现问题后，关闭应用，复制log  
    - linux 安装deb后，命令行运行 `macast` \\ 或直接从源码运行 \\ 或包管理安装后命令行运行 `macast-cli`，复现问题后，关闭应用，复制log  

## 用户反馈

点击链接加入群聊【小方的软件工地】：[983730955](https://jq.qq.com/?_wv=1027&k=4ioK8gQs)

当然也可以考虑捐赠 ~~获得贵宾售后服务（开玩笑）~~ 支持Macast和他的开发者们为了这个软件熬过的日日夜夜

<img align="center" width="400" src="sponsorships.png" alt="sponsorships" height="auto"/>

<img align="center" width="400" src="https://service-65diwogz-1252652631.bj.apigw.tencentcs.com/release/sponsor.svg" alt="sponsors" height="auto"/>

## 使用截图
*如果系统设置为中文，Macast会自动切换中文界面*  

在投放视频或其他媒体文件后，可以点击应用图标复制媒体下载链接  
<img align="center" width="400" src="https://gitee.com/xfangfang/xfangfang/raw/master/assets/img/macast/copy_uri.png" alt="copy_uri" height="auto"/>

支持选择第三方播放器  
<img align="center" width="400" src="https://gitee.com/xfangfang/xfangfang/raw/master/assets/img/macast/select_renderer.png" alt="select_renderer" height="auto"/>


## 相关链接

[UPnP™ Device Architecture 1.1](http://upnp.org/specs/arch/UPnP-arch-DeviceArchitecture-v1.1.pdf)

[UPnP™ Resources](http://upnp.org/resources/upnpresources.zip)

[UPnP™ ContentDirectory:1 service](http://upnp.org/specs/av/UPnP-av-ContentDirectory-v1-Service.pdf)

[UPnP™ MediaRenderer:1 device](http://upnp.org/specs/av/UPnP-av-MediaRenderer-v1-Device.pdf)

[UPnP™ AVTransport:1 service](http://upnp.org/specs/av/UPnP-av-AVTransport-v1-Service.pdf)

[UPnP™ RenderingControl:1 service](http://upnp.org/specs/av/UPnP-av-RenderingControl-v1-Service.pdf)

[python-upnp-ssdp-example](https://github.com/ZeWaren/python-upnp-ssdp-example)
