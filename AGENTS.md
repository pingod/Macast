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
- 日志：`~/Library/Application Support/Macast/macast.log`（**排障看这个，不是 stdout**）
- 配置：`~/Library/Application Support/Macast/macast_setting.json`
- 用户插件目录：`.../Macast/renderer/` 与 `.../Macast/protocol/`

## 2. 改完必须做的两件事

```shell
# 1) 静态检查（能秒抓"删代码块时误删变量赋值"这类错误）
env -u PYTHONPATH .venv/bin/python -m pyflakes <改动文件>

# 2) 回归验证（当前 411/411）
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py
```

> 加新功能/修 bug 时**同时补用例**。这个仓库的验证脚本是唯一能防回退的东西。
> 验证套件把网络全部打桩，所以它**抓不到"真实发送端栈不认账"这类问题**——
> 那类问题要跑 `scripts/cast_conformance.py`（见 §6）。

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
  plugin_repo.py           在线插件索引的仓库坐标（唯一来源）；见 §4.6
  gui.py                   跨平台菜单抽象（darwin: rumps；其他: pystray）
  plugins/renderer/        内置渲染器插件：iina / web / live / potplayer / pi_fm
  plugins/protocol/        内置协议插件：nirvana（NVA「哔哩必连」）
  xml/setting.html         设置页（Vue2 + Element UI，**单文件内嵌模板**）
plugins/                   在线插件索引 + 可安装插件（仓库根目录，**不是** macast/plugins/）；见 §4.6
macast_renderer/mpv.py     MPVRenderer：启动 mpv、走 IPC 收发、把 mpv 事件转成状态
scripts/                  见 §6
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
- **`mpv --http-proxy=` 不能替代摘环境变量**，ffmpeg 的 HTTP 层仍然读 `http_proxy`。

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

### 4.6 在线插件索引：地址只有一处，页面不写死

上游合集 `xfangfang/Macast-plugins` 发布的 6 个插件**全部内置**之后，设置页原来
硬编码的那个 `info.json` 就只剩副作用了：每个内置插件都会多出一张重复卡片，还带一个
指向上游旧文件的「可更新」角标（因为内置版本号与上游不同）。

现在的做法：

- 仓库坐标与索引地址只在 **`macast/plugin_repo.py`**（`pingod/Macast` →
  `plugins/info.json`），由 `/api?query=plugin-info` 的 `plugin_repo` 字段下发；
  `setting.html` 里**不允许**再出现任何插件仓库 URL（Part 5c 有用例守着）。
- 索引本体在**仓库根目录**的 `plugins/`（不是 `macast/plugins/`，后者是内置插件），
  当前 6 条（见 §4.8）。空索引也是合法状态，页面只显示本机插件，不报错。
- **索引本身**（唯一一个没法固定 SHA 的文件）走三个地址，按「新鲜度」排序：
  `raw.githubusercontent.com`（永远最新，国内常拉不动）→
  `ghproxy.net/https://raw.githubusercontent.com/...`（国内可达；自称 `max-age=300`，
  实测推送 6 分钟后仍给旧文件）→ `cdn.jsdelivr.net`（**分支文件缓存数小时、`?v=` 也破不了**）。
  也就是说索引最多可能滞后几小时，**但它只影响「新插件多久能被看到」**，装到手的文件永远是对的
  （安装 URL 固定了 SHA，见下）。
  浏览器按序回退；三者都必须返回 CORS 头，因为这是在**浏览器里** fetch，不是 Python 抓。
  改顺序前先想一遍：把会缓存的放前面 = 用户看到的插件列表可能落后半天。
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
- 在线插件与内置插件是**两回事**：`plugins/` 里的是「用户自己装、单个 .py、只能用
  Macast 自带依赖 + 标准库 + 外部命令」；需要 pip 库或要随包发布的一律走
  `macast/plugins/`（并同步 §4.4 的三处打包配置）。

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

### 4.8 在线插件目录（`plugins/`）：6 个插件，各自的红线

`plugins/` 现在提供 6 个插件（yt-dlp 下载 / 外部播放器 / 小窗+壁纸 / 自动化钩子 /
Chromecast 中继 / AirPlay 音频）。它们是**单文件**插件，所以：

- 只能用「Macast 已带的库 + 标准库 + 机器上已有的命令行程序」。要 pip 库就走内置插件
  路线（§4.4 的三处打包配置）。
- **Macast 一次只能选一种渲染器**：5 个渲染器类插件互斥（菜单栏切换），`raop.py` 是协议
  插件，可以和任意渲染器共存。
- 定位一律用 PATH + 常见安装目录（GUI 从 Finder 启动拿不到 shell 的 PATH；`yt-dlp`
  / `shairport-sync` / 播放器都踩这条）。

最容易踩的几处，改这些插件前先看：

| 插件 | 红线 |
|---|---|
| yt-dlp | 下载模式**不启动 mpv**，所以它覆写了 `start/stop`，并用 `_mpv_started` 记住「mpv 到底起没起」——`MPVRenderer.stop()` 会 join 只有真起过才存在的线程。流模式把 ytdl 路径**合并进已有的 `--script-opts`**（再写一条会顶掉 OSC 的设置）。 |
| external_player | 播放器用 **argv 列表**启动，绝不拼命令行（url 来自网络）。暂停不实现，停止只杀自己起的进程。 |
| floating | 覆写 `build_mpv_params` 前先**删掉继承来的 `--geometry/--autofit/--fullscreen/--ontop-level`**，否则会和全局 Player Size 打架。壁纸模式要先探测 mpv 是否认识 `--ontop-level=desktop`：未知选项会让 mpv 起不来，而启动器的反应是重试 3 次后**把服务停掉**。 |
| hooks | 命令是 shell 命令（用户自己写的），但 **url 只走环境变量**，不拼进命令串。spawn 完即返回，不阻塞 CherryPy 工作线程。 |
| cast_bridge | 复用 `protocol_cast` 的 Cast v2 收发，不引 pychromecast。投屏序列是 deviceauth CHALLENGE → CONNECT receiver-0 → LAUNCH → CONNECT transportId → LOAD，`transportId` 来自 LAUNCH 的回复，**不要硬编码**。发现用 mDNS 且必须在后台跑（`build_menu` 在 UI 线程上）。 |
| raop | 只监督 shairport-sync（生成最小配置 + 拉起 + 报连接/断开），**不**把 RAOP 映射成 DLNA 播放状态。`uses_ssdp = False`，否则只有它启用时也会把 SSDP 服务拉起来。 |

回归用例：Part 14（外部播放器 / 小窗 / yt-dlp 两种模式）、Part 15（钩子）、
Part 16（中继对打 Macast 自己的 Chromecast 接收端）、Part 17（RAOP 监督）。
测试用的是假二进制（PATH 上放个 shell 脚本），所以跑测试不需要 yt-dlp / VLC / shairport-sync。

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
- **WorkBuddy/CLI 沙箱**：`PYTHONPATH` 被注入 shim，`mkdir(exist_ok=True)` 会抛
  `PermissionError: EEXIST`；`ps` 不可用。一律 `env -u PYTHONPATH`，用 `lsof`/`pgrep` 代替 `ps`。

## 6. scripts/ 里的工具

| 脚本 | 用途 |
|---|---|
| `run-from-source.sh` | 从源码启动（会 unset PYTHONPATH） |
| `verify_cast_airplay.py` | **主验证套件**（411/411）：协议逻辑 + 真实 socket 端到端 + mDNS/网卡/插件热插拔 + 内置插件加载 + 插件索引/条目与清单一致性 + 网页投屏入口与令牌门控 + 6 个在线插件（下载器/外部播放器/小窗/钩子/中继/RAOP）+ Cast 接收端一致性（Part 18）与 8443 HTTPS setup API（Part 19）|
| `cast_conformance.py` | **用真实 pychromecast 栈打真实接收端**（见 §4.9）。`verify_cast_airplay.py` 把网络打桩，所以抓不到"发送端不认账"；`vlc_sender_sim.py` 只复刻 VLC。这个跑的是手机/HA 实际用的那套代码 |
| `selfcheck.py` | 收屏前的环境自检：依赖、端口占用者身份、可广播网卡、组播出口、mpv/`--input-ipc-server`、代理变量。端口占用会区分"Macast 自己在跑"/"macOS 自带 AirPlay"/"别的进程" |
| `vlc_sender_sim.py` | **忠实复刻 VLC 状态机**的发送端（含严格 protobuf 语义）。必须等到 `PLAYING` 才算通过 |
| `cast_probe.py` | 手写 TLS/CASTV2 的最小发送端，打逐步日志 |
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

`.github/workflows/build.yml` 会在 tag push 时构建 macOS arm64/x86_64、Linux x86_64/arm64、
Windows x86_64，并在版本一致性校验通过后创建 Release。
产物名形如 `Macast-MacOS-arm64-v<版本>.zip`。

> 也可在 GitHub UI → Actions → Build Macast → Run workflow → `release=true`。

**发版后必做（CI 绿不代表产物能用，见 §4.3）**：

```shell
# 1. 等 CI 跑完，确认 4 个产物都在（本机没有 gh CLI，用 REST API）
#    https://api.github.com/repos/pingod/Macast/releases/tags/v<版本>
# 2. 下载 macOS 产物、替换安装（旧包先移废纸篓，不要硬删）
#    注意下载来的包带 quarantine 属性，需 xattr -dr com.apple.quarantine
# 3. 真正启动并验证
open -a /Applications/Macast.app && sleep 15
lsof -nP -iTCP:8009 -sTCP:LISTEN      # 应有进程
curl -s 'http://127.0.0.1:58880/api?query=status' | head -c 200   # 应返回版本号
dns-sd -B _googlecast._tcp            # 5 秒后应有 Macast-<主机名>
```

**改成了同一版本号重新发布时**：由于无法用 API 删除已发布的 release（本机没有 token），
做法是把 tag 移到修复提交后强推（`git tag -f v<x> && git push -f origin v<x>`），
CI 会用同名文件**替换** release 里的产物。用 digest 对比确认真的换了：

```shell
# GET /releases/tags/v<x> 里每个 asset 的 digest 字段
```

## 9. 当前能力边界（别当成 bug）

| 能力 | 状态 |
|---|---|
| DLNA（SSDP + UPnP）接收 | 完整 |
| Chromecast 接收（Cast v2，URL 投屏） | 可用；**未认证接收端**，Google 官方发送端可能因设备认证失败 |
| AirPlay 视频 URL 投屏 | 可用 |
| AirPlay 音频（RAOP） | 核心未实现，但在线插件 `plugins/raop.py` 可监督 shairport-sync 接收（见 §4.8） |
| AirPlay 屏幕镜像 | **未实现**（需要 FairPlay 解密，不打算做） |
| DRM 内容 | **不可能支持** |
| 插件 | 支持启用/停用/卸载/安装（**热生效，不重启**）；卸载进 `.trash/` 可恢复 |
| 在线插件目录 | `plugins/` 下 6 个：yt-dlp 下载/边下边播、外部播放器、小窗+壁纸、自动化钩子、Chromecast 中继、AirPlay 音频（RAOP）|
| 网页投屏入口 | `GET /api?query=cast&url=<绝对地址>&token=<令牌>`（脚本 / 快捷指令 / 书签，绕开 DLNA 发现）；令牌常驻并显示在设置页 |

## 10. 与用户协作的约定（这个仓库的历史教训）

- **不要为了验证去改用户的真实配置**：所有涉及设置的测试用临时 `SETTING_DIR` +
  临时 `setting_path`，测试完还原。（曾因此被用户明确纠正。）
- **明确区分"发送端问题"和"我们的问题"再动手**：这个项目里多次出现"用户报我们的 bug，
  实际是发送端行为"，先拿证据（日志/事件/源码）再改代码。
- **破坏性操作要可逆**：删插件用 `mv` 到 `.trash/`；删除/覆盖用户文件前先备份。
- **提交前自查暂存内容**，别夹带构建产物/密钥/`macast.log`。
