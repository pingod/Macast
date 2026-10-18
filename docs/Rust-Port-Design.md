# Macast Rust 移植 —— 详细设计文档

分支：`refactor/rust-tauri-2`。基线：2026-10-15 的 `d9cf9de`。
本文写的是**这个移植的设计**：分层为什么这样切、状态的唯一主人是谁、可测性靠哪些接缝、
与参考实现（Python 版）的偏差如何记账、依赖图受哪些纪律约束、验证到什么程度、
以及哪些事**刻意还没做**。

它不复述产品功能（见 `README_ZH.md`），不重复踩坑史（见 `AGENTS.md` §4），
也不重复构建步骤（见 `BUILDING.md`）。凡是一条结论已经坐在某个源文件的注释里，
本文只给**指针**（`文件:行`），不抄第二遍 —— 一条陈述有两个抄本，就是漂移的开始。

> 读法建议：先读 §1 与 §5（分层与所有权），那是这个移植的骨架；
> §8～§10（偏离政策 / 线序即格式 / 依赖纪律）是三条最容易被"顺手简化"掉的规矩；
> §13 说明这台机器上"通过"一词到底意味着什么。

---

## 1. 目标与非目标

**目标**

1. 把 DLNA / Chromecast / AirPlay 接收端的核心链路（发现 → 收媒体地址 → 交给播放器 →
   把状态报回控制点）迁到 Rust，行为与 Python 版**逐字对齐**，包括日志行的措辞与
   JSON 的键序。
2. 每个 crate 是一个**独立的验证面**：移植一层时不需要其它层能编译，
   也不需要把它们的存在当成前提（§2）。
3. 让 `AGENTS.md` §4 里那些"只在真机 / 真实网络才暴露"的事实，尽可能落到
   **单元测试能问**的位置上 —— 靠接缝，不靠计时（§6）。

**非目标**

- **不追求"更地道的 API"**。参考实现的语义就是需求：Python 的 dict 插入序是线格式的一部分
  （§9），`Setting.get(key, default)` 的副作用要用显式 `Key` 表达（§5.3）。
  把这些"整理干净"等于改变对外行为。
- **不做跨会话的抽象层**。一个 crate 只有一个消费方时不预先设计第二个消费方。
- **不在本分支上发版**。Rust 分支不推送到 `main`、不打 tag（§15），
  它现在是一条验证过行为的移植队列，不是一个产物。

---

## 2. 工作区分层：一个 crate = 一个迁移面

根 `Cargo.toml` 的头部注释就是这个决定的完整论证，这里只提取结构：

```
macast-core     设置（BOM 容错读取 + 坏文件整份搬开）、网卡选择与"可广播"判定、
                摘掉代理的播放器环境、按模块轮转日志、令牌、废纸篓、时间格式化
macast-mpv      mpv argv 构造器 + JSON IPC 渲染器（播放器那一半）
macast-upnp     SSDP + DLNA MediaRenderer（描述/SOAP/GENA/DIDL）
macast-cast     Chromecast Cast v2 接收端（TLS 上的 protobuf + setup HTTP）
src-tauri       托盘 + 设置窗口；唯一知道 UI 的 crate
```

三条承重规则，全部写在 `Cargo.toml:1-21`：

1. **分层的理由是"能单独验证"**，不是"看起来整洁"：
   *"each crate is one migration surface so a port can be verified without the others compiling"*。
2. **`src-tauri` 留在 members 里**，这样仓库根的 `cargo check` 会用同一份 lockfile
   把壳也证一遍。
3. **members == 磁盘上存在的目录**。一个还不存在的成员对 cargo 不是"计划中的 crate"，
   而是 manifest 加载错误，**这个错误会杀掉树上的每一个 crate，包括正在做的那一个**。
   所以每一步迁移只在"写它自己那份 `Cargo.toml` 的那个提交"里加它自己的成员。
   注释的收尾一句是给未来读者的：*"Nothing here is a promise."*

**一处已知的不整洁**（写在这里而不是留着让人猜）：`[workspace.dependencies]`
在 `Cargo.toml:130` 声明了 `macast-airplay = { path = "macast-airplay" }`，而仓库里
**没有** `macast-airplay` 目录 —— 头部注释（`:12`）也把 AirPlay 列为独立一层。
它现在无害，因为没被任何成员引用的 workspace 依赖不会被解析；AirPlay 的 RTSP 实际住在
`macast-cast/src/airplay.rs`（§4.4）。这是一个**待归位的陈述**：要么真拆出该 crate，
要么把两处文字改成"RTSP 在 cast 里"。按 §3 的记账规矩，它记在这里，不算已修。

---

## 3. 数字放哪儿：本文不持有计数

本文故意**不维护**"某个模块有多少处 / 多少个用例 / 多少行"这类数字，除了下面这张
一次性基线表。理由来自移植自己踩过的教训：`AGENTS.md` §4.13 那条
"一个没被机器问过的数字和不提问的检查是同一件事"，在 Rust 侧同样成立 ——
而**计数最好的家是决定它的那个文件**。

- 用例总数的家是 `macast-cast/src/proto.rs:410` 的 `const TESTS_IN_THIS_CRATE`
  加 `proto.rs:847` 那条全仓扫描用例；
- 模块清单的家是每个 crate 的 `lib.rs` 头注释；
- Python↔Rust 对应关系的家是 §4 这一张表（它是设计，不是计数）。

**基线快照**（2026-10-15 按 §附录 A 的命令量出；引用前先重跑）：

| crate | `.rs` 总行数 | `#[test]` |
|---|---:|---:|
| `macast-core` | 4,581 | 76 |
| `macast-mpv` | 10,410 | 161 |
| `macast-upnp` | 17,418 | 320 |
| `macast-cast` | 40,292 | 423 |
| `src-tauri` | 2,837 | 0 |

`src-tauri` 的用例数为 0 是**分层的结果**：它只做胶水（把 trait 接到真应用上），
值得测的行为都被推到了下面四个 crate 的接缝上。这句话目前是真的，但它是一条
**要被维持的约束**，不是一个自动成立的事实 —— 哪天 `host.rs` 里长出判决逻辑，
它就得有自己的用例。

---

## 4. Python → Rust 模块映射

### 4.1 `macast-core` ← `macast/utils.py` + `server.py`/`Macast.py` 里与"服务 HTTP"无关的部分

`macast-core/src/lib.rs:1-20` 的自我描述：*"where files live, how the settings JSON is
read and protected, which network addresses are worth publishing, what environment a
child process gets, and how logs rotate."*

| Python | Rust | 设计要点 |
|---|---|---|
| `Setting`（类属性 + 静态方法） | `setting::Setting` + `Key` | 判存在用 `has`、删用 `unset`，因为 Python 的 `get(key, default)` 会把 default **写进**内存表（§4.2 那条副作用）在 Rust 里必须变成两个不同的动作 |
| `get_system_env()` 摘代理 | `env::player_env` / `PlayerEnv` / `PROXY_ENV_VARS` | 对应 `mpv --http-proxy=` 不够、ffmpeg 仍读 `http_proxy` 那一格 |
| 网卡枚举与可广播判定 | `net::{unadvertisable_reason, InterfaceRow, Snapshot, Steering}` | `Snapshot` / `Steering` 是接缝（§6.6）：mDNS 只广播能走到默认路由的口 |
| `logsplit.claim()` + 轮转 | `logging::{Logger, claim, read_tail, remove_log_files}` | 阈值默认 `Info`；`claim()` 用 `OnceLock` ⇒ **第一个认领者赢**；`read_tail` 里"文件不存在"**不是**错误 |
| `api_token()` | `token` | 常驻、首次使用时生成 |
| 插件卸载进 `.trash/` | `trash::TrashError` | 破坏性操作要可逆（§10 那条约定） |
| `Setting.get_version()` 等 | `timefmt` / `paths` | `paths::setting_dir()` 是日志落点的唯一入口（§12） |

`Core` 结构体只有一件成员 `pub setting: Setting`，注释给了理由：
Python 把这些放类属性，这里放"一个带锁的 struct"，因为 Rust 侧有好几个线程读同一张表
（SSDP 收包循环、事件线程、mpv IPC 线程、HTTP worker）。

**这一层刻意不含 socket 与子进程**（`lib.rs` 明写），目的是让"曾经要靠真机才学到的规则"
能用单元测试问出来，而不是靠复现。

### 4.2 `macast-mpv` ← `macast_renderer/mpv.py`

`macast-mpv/src/lib.rs:1-45` 逐个模块给了一行职责，映射照它抄：

| 模块 | 职责（原文提炼） |
|---|---|
| `command` | 每一种 `loadfile` / `set_property` / `seek` 的形状、**逐文件选项字典**、`--input-ipc-server` 的路径 |
| `argv` | mpv 命令行，**每次启动尝试都重建** |
| `ipc` | 字节缓冲的封帧：哪一块值得解析，以及一行解不开时日志说什么 |
| `state` | 把 mpv 的回复翻译成控制点被告知的那些状态更新 |
| `pyjson` | 读那些回复用的解码器，**从 CPython 自己的 JSON 扫描器重放出来** |
| `link` | 已连接的传输：Unix 上 AF_UNIX，Windows 上命名管道 |
| `player` | 起 mpv、按三次预算重试、连 IPC、在**一个** `Feed` 下读行、在**一把**锁下写命令、并按有理由的顺序停 |

三处值得单独记的设计决定：

1. **argv 在重试循环的 `try` 之外构造**。Python 里有些旋钮的失败无法用返回值比较表达
   （拼不出命令就是拼不出），把它放进循环里会烧掉一次重试并印出一句
   「mpv 起不来」——那是把开发期错误伪装成用户故障。
2. **socket 先赋值再 `connect()`**，逐字照 `mpv.py:404`。跨平台错误消息**不对称**且已实测：
   macOS 是 `[Errno 57] Socket is not connected`，Linux 是 `[Errno 107] Transport endpoint
   is not connected`，而 **Linux 上 57 是「Invalid slot」** —— 所以错误号不是那句话的名字，
   引用它的用例必须钉在量出它的那台机器上。Windows 的 `CreateFileW` 不会留下这个状态，
   于是 `link::NO_SOCKET` 成为 `link` 里**写下来的第一处分歧**。
3. 诚实边界（原文照抄）：*"the Unix sentences were measured on this Mac and read off a
   Linux run, while the Windows column is a port that no machine here has exercised."*

⚠ **`lib.rs:41` 有一句现在错了**：它称事件总线与设置在 `src-tauri`，
*"which has not been written yet"*。`src-tauri` 已经存在（§3 基线表里 2,837 行）。
这一格属 §8 的"偏离不许沉默"管辖：改注释，不是改本文来迁就它。

### 4.3 `macast-upnp` ← `macast/ssdp.py` + `macast/plugin.py::SSDPPlugin` + `macast/protocol.py`

对应关系写死在 `macast-upnp/src/lib.rs:1-40`：SSDP 收发 ← `ssdp.py`；
一个 MediaRenderer 广播的**六个 USN** ← `SSDPPlugin`；设备描述、SCPD、
`DLNAHandler.call` 那个 SOAP 派发器 ← `protocol.py`；GENA ← 同样是 `protocol.py`，
落在 `[eventing]`（订阅者表、初连接 greeting、把变更变成 NOTIFY 的 250 ms 折叠）。

模块切分照 `lib.rs:40-80` 的列表（每个都在 `src/` 里，见 §附录 A）：

| 模块 | 设计的落点 |
|---|---|
| `sock` | **每个网卡一个 UDP 发送者**，以及 `IP_MULTICAST_IF` 的两种平台形状 |
| `ssdp` | UDP/1900 的接收侧、已广播设备的注册表、`ssdp:alive` / `ssdp:byebye`、M-SEARCH 的答复 |
| `device` | 六个 USN；那句**留着 `{}` 给发送者填**的 `LOCATION`；IP 变化时在锁下重启 |
| `scpd` | 从 `macast/xml/*.xml` 读出的扁平状态表与动作清单；**按 lxml 的导航规则读**，因为控制点 `GetStateVariables` 的答案就是那些规则 |
| `description` | 唯一一份**被填**而不是被构造的文档：`Description.xml` 换掉十一个名字，与线上跑的那份逐字节同 |
| `soap` | 按 lxml 的方式读信封、按 SCPD `<argumentList>` 写状态变量、答 `<u:{Action}Response>`、**用真 SOAP fault 拒绝**；媒体交给 `store::Renderer`、变更值交给 `store::Broadcast`，**两者都不留副本** |
| `http` | 派发**之前**的拒绝（`411` 之所以先于 `405`，因为发生在 dispatch 前）、每个见证行测量时的头序、`DLNAHandler` 实现的四个动词 —— 含那两个**刻意互不相同**的 `SUBSCRIBE` / `UNSUBSCRIBE` 形状 |
| `eventing` | 路由**按订阅者**：只有 `store::observed_for` 那一行点名了变量的服务才听到；`store::Broadcast` 自己**不做门控**，因为 `[soap]` 已经筛过入队的东西 |

### 4.4 `macast-cast` ← `protocol_cast.py` + `protocol_airplay.py` + `media_resolve.py` + `media_relay.py`

`macast-cast/src/lib.rs:1-15` 说这个 crate 是**分层移植**而不是整块搬：
先 `proto`（编码面与消息形状），再 `cert`，再 `channel`（socket 那一半），
再 `airplay`（RTSP），然后 8008 / 8443 两个 setup 面，最后 mDNS 广播。

| 模块 | 内容 |
|---|---|
| `proto` | `_encode_varint` / `_field_bytes` / `_parse_fields` / `parse_cast_message`、五个 namespace、`app_namespaces()`、device-auth 的答复、大端四字节帧头 |
| `cert` | **在进程内生成**密钥对；`macast_cast.*` 槽位**已存在就收养**，共享的 `macast.*` 才是本模块写的，两者都**从不覆写** —— 一份坏的外来密钥对按名字报告后原样留着 |
| `channel` | TLS on 8009、**只按 namespace 派发**、发送者账本与会话账本、音量、`LAUNCH_ERROR`、播放看门狗 |
| `airplay` | RTSP on 7000：十一个方法、会话管理、`Content-Location` 解析（头与 SDP 正文两条路）、端口回落、以及播放器答复所在的 `RendererCallback` 接缝 |
| `mdns` | 线格式编解码 + **浏览器**（`discover_cast`：在 5353 上找**发送者**的接收端，好让设置页有设备可点名）。原文限定语：*"Reading a reply is not the same job as broadcasting our own receiver, and the broadcast side is still ahead of this module."* |
| `resolve` / `relay` / `html` / `entities` / `charmap` / `gbk` / `cjk` | `media_resolve.py` 与 `media_relay.py` 的对应层（抓取、判定表、中转供流），以及**字符表**（§11） |

`lib.rs` 再导出的公开面（节选，名字照 `lib.rs`）：
`cert::{ensure_in_setting_dir, ensure_pair, Pair, Source, …}`、
`channel::{launch_refusal, live_from_stream_type, random_session, sender_media_title,
CastChannel, APP_BACKDROP_ID, DEFAULT_MEDIA_APP_ID, DEFAULT_MEDIA_APP_NAME,
SERVED_APP_IDS, TRANSPORT_ID, …}`、
`proto::{app_namespaces, encode_auth_challenge, encode_auth_response, frame_length,
parse_cast_message, CastMessage, Field, Payload, NS_*, FRAME_HEADER_LEN, …}`、
`airplay::{get_header, info_plist, parse_content_location, random_mac, reason_phrase,
AirPlayReceiver, AirPlayState, RendererCallback, AIRPLAY_PORT, FEATURES, …}`。

**这个 crate 自己的 backlog**（`lib.rs` 的模块图；两条原本都标着 `*(next)*`）：

- `setup` —— HTTP/8008 与 HTTPS/8443：`/setup/eureka_info`（`ssdp_udn` 里必须是**裸** UUID）、
  `params=` 过滤、`/setup/icon.png`；**已交付**（2026-10-18）
- `_googlecast._tcp` 的**广播**侧 —— 我们自己接收端的 PTR / SRV / TXT / NSEC / A，主动
  unsolicited 发出去，好让发送者找得到；**已交付**（同一天，`announce.rs`）

划掉的两条不是"这一节结束"：还开着的是**responder**（答别人的查询），§15 倒数第二行与
§16 第 10、13 条写的是它现在能做到哪、以及落地时为什么不能简单再加一个 socket。

### 4.5 `src-tauri` ← `Macast.py` 菜单栏 + `macast/gui.py` + `macast/macast.py`

`src/host.rs` 是四个 `store` trait 的**唯一实现处**（§5）：
`impl macast_upnp::store::Renderer for ShellRenderer`（`host.rs:443`）、
`impl macast_upnp::store::Hooks for TauriHost`（`host.rs:691`），
并持有 `store::SharedBroadcast`（`host.rs:55`）。

---

## 5. 所有权模型：谁持有状态

这是整个移植最重要的一节，因为它是**唯一会让用户当面看到矛盾**的那一层。

### 5.1 两个协议 crate 都只拥有"线上"

`macast-cast/Cargo.toml` 里那段注释是原文表述：Cast 面**不**拥有播放状态，
也**不**拥有传输表 —— *"a Cast `LOAD` and a `SetAVTransportURI` are two doors into one
living room"*。媒体交给 `store::Renderer`，回报的状态从 `store::StateStore` 读回来，
用的正是 `macast-upnp` 定义、`src-tauri` 在**同一张** `StateTable` 上实现的那四个 trait。
注释直接点名了后果：*"Depending on the traits is the whole point; depending on a second
table would be the bug AGENTS.md §4.8's oldest lesson says always shows up in front of a
user."*

### 5.2 四个接缝（`macast-upnp/src/store.rs`）

| trait | 行 | 谁调用 | 谁实现 |
|---|---|---|---|
| `StateStore` | `:100` | 读回播放状态（SOAP 的状态变量、Cast 的 `MEDIA_STATUS`） | `src-tauri` |
| `Renderer` | `:170` | 把媒体交给播放器 | `src-tauri`（`ShellRenderer`） |
| `Hooks` | `:239` | 服务启停、通知等副作用 | `src-tauri`（`TauriHost`） |
| `Broadcast` | `:269` | 把变更了的值广播出去 | `src-tauri`（`SharedBroadcast`） |

配套的判据函数 `is_observed`（`:72`）与 `observed_for`（`:78`）回答的是
"这个变量有没有人在观察" —— `eventing` 用它决定路由，而**门不在 `Broadcast` 里**。

### 5.3 三态读数不许塌成两态

`live_from_stream_type`（`channel` 再导出）返回 `Option<bool>` 而不是 `bool`，
对应 `AGENTS.md` §4.1 末与 §4.2 那条：`None` 不是 `False`，它意味着
"保持今天的行为"，那是任何我们不了解的东西都安全的那一边。
Python 侧那个陷阱（`Protocol` 自己定义了返回 `None` 的 `media_is_live()` 桩，
所以 `ProtocolGroup` 必须按**名字**把它 `setattr` 到实例上）在 Rust 里由类型表达：
三态是类型的一部分，不是运行时的约定。

---

## 6. 接缝策略：可测性靠注入，不靠计时

`AGENTS.md` §4.2 有一族反复出现的坑：判据抓的是"客户端返回的那一瞬间"、
或"计数器在答复之后才加"。Rust 侧的应对不是加等待，而是**把决定事实的那个东西变成参数**。

现存的接缝（六个，逐个都有实现者）：

1. `macast-mpv::player::Host` —— crate 定义接缝，**它自己的用例是唯一的实现者**。
   起进程、连套接字、拿环境，全部经 `Host`。
2. `macast-cast::mdns::Wire`（`mdns.rs:1210`，`send` / `recv` / `source`）——
   于是折叠、账本、重发、超时都能在**没有局域网**的情况下测；
   `Multicast`（`:1353`，`impl Wire` `:1359`）是线上那一个。
3. `resolve` 的 `fetch_page(page_url, opener)` —— 抓取层可喂 fixture，不碰网络。
4. `relay` 的 ffmpeg 注入接缝 —— argv 与 spawn 可替换，"增长的文件"那一族判据可测。
5. `airplay::RendererCallback` —— RTSP 的播放器答复面，测试自己实现它。
6. `core::net::Snapshot` / `Steering` —— 网卡表与"能不能广播"的判定分开注入，
   所以 §4.2 那条"mDNS 把所有本地 IPv4 都发出去"的坑有用例而不是靠运气。

**反向的一条**：`macast-cast` 里所有 HTTP 语义用例都打**真回环监听 socket**
（`relay_tests.rs` 头部明写 *"Every test that opens a socket uses a real loopback
listener; no mocks."*）。这两句不矛盾 —— 接缝放在**决策**上（谁去 fetch、喂什么 argv），
线格式与 HTTP 语义放在**真 socket** 上（那正是 Python 侧 §4.2 里 ureq 代理默认值、
桩少了终止空行这两格唯一能变红的地方）。写新用例时要先判断自己属于哪一类，
**别把"no mocks"抄进 mDNS 那一族** —— 那里驱动 `browse` 的是假 `Wire`。

---

## 7. Python 语义的孤岛：`pyops`

移植里有一些地方不是在实现 Rust 的语义，而是在**重放 Python 的语义**：
整数除法与取模的符号、`len()` 数的是码点、字符串切片的越界宽容、
dict 的插入序、`str.strip()` 的字符集、时间戳的舍入方向。

策略是把它们集中到 `macast-mpv/src/pyops.rs` 一个模块里，让分歧**可数而不是散布**。
那个模块的头注释里有一句话是给所有后来者的（原文照引，因为它是政策而不是实现）：

> *"How many there are is that module's own business; restating the number here is how
> it goes stale."*

同样的形状在 §3 里被本文遵守了一次：不抄计数。

`pyjson` 是这个政策的极端例子：它不是"一个 JSON 解析器"，而是**从 CPython 自己的扫描器
重放出来**的解码器，因为 mpv 回复的读取语义要与 Python 版逐字一致（重复键、
超大整数、非法转义的容忍方向）。

---

## 8. 与参考实现的偏离：永不沉默

规则（`AGENTS.md` §4.14 那条「"和参考实现不一样"必须是文档，不能是沉默」的 Rust 版）：
**每一处偏离都要写在偏离它的那段代码的 docstring 里，并在这里能查到**。
理由不是礼貌，是**下一次重写会把它当成冗余删掉**。

已记账的偏离（每条都在对应文件的注释里能找到）：

| 位置 | 偏离 | 为什么 |
|---|---|---|
| `mdns.rs`（名字长度上限） | 上限按 `chars().count()` 判，Python 的 `len()` 数的也是码点 | 若按字节判，一条 >253 **字节**但 ≤253 **字符**的 UTF-8 名字会被拒 —— 那是把别人的标题读成违规 |
| `mdns.rs`（`MAX_MSG_LEN`） | 当作**溢出前提**，不是格式规则 | 它是"缓冲区有多大"的条件，与 RFC 的报文上限不是同一问 |
| `mdns.rs`（TXT） | 不强制 RFC 6763 §6 的完整性 | 参考实现（zeroconf）也不强制；强制会让真设备解不出 |
| `mdns.rs`（地址族） | 只走 IPv4，`IPVersion.All` 不做 | 与 Python 侧当前行为对齐 |
| `mdns.rs`（`drop_ours`） | `ours` 为空集时**不过滤** | 退化方向选"什么都看得见"而不是"什么都看不见"；`IP_MULTICAST_LOOP` 故意不设，所以自我回环是后果不是 bug（§9.3 那条）|
| `macast_core::url`（`unquote_plus`） | cherrypy 对 query 用 `errors='strict'`，一个 `?url=%FF` 在它那里是 **404**；本端口**有损解码并继续路由** | 这一格不是"发送端打错了"的判决点，而 404 不带正文、不带解释；用户读到的是"不认识的字段"那句可行动的话。**主人 2026-10-18 搬过一次**：`parse_query` / `split_once_ascii` / `unquote_plus` 从 `macast-upnp/src/http.rs` 搬进 `macast-core/src/url.rs`，因为 setup 面的 `?params=a,b` 是第二个读者（`macast-cast` 依赖 `macast-upnp`、从不反向），"%2B 是加号还是空格"这一条规则在一个进程里不能有两位答案 |
| `macast_core::url`（`parse_qs_first`） | 参考实现**自己**就有两个读者，而它们对空值的答案相反 —— cherrypy 的 `parse_query_string` 默认 `keep_blank_values=True`（`lib/httputil.py:369`），`urllib.parse.parse_qs` 默认 `False`；本移植交回**两个函数**，不交回"一个带 flag 的函数" | 两边都在本机问过（`?params=` / `?params=&params=name` / `?params` 三行各自的答案，记在 `url.rs` 的 `parse_qs_first` doc 表里）。合成一个带布尔的函数等于把"哪一个读者"这个决定推迟到调用点，而 `?params=` 在 setup 面是**整份文档**、在管理面是**一个空串参数** —— 两种命运都不该由一个 flag 的默认值决定 |
| `setup`（`Server:` 头） | 交回 `Macast/<版本>`，不抄 `BaseHTTP/0.6 Python/3.12.15` | 参考实现那个值是"它恰好继承了哪个类"的产物；把 CPython 的名字写到线上是关于"谁在答"的一句谎。`dial.py` 不读这个头，所以这不是兼容性代价，只是诚实 |
| `setup`（非 `GET`） | `501` + **无正文**；Python 也回 501，但带 `BaseHTTPRequestHandler` 生成的 `text/html` 错误页 | 抄那页 HTML 就是把 CPython 的模板串放进我们的字节里，而唯一的读者（`dial.py:173-184`）把 `HTTPError` 折成一句 warning，从不渲染正文 |
| `setup`（8008 的端口回落） | **不回落**：绑不上 8008 就交回 `Err`，而不是参考实现那样沿 `PORT_FALLBACK_RANGE` 走 20 个口 | 两条理由合成一条：`dial.py:25` 把 8008 钉死，所以 8010 上的这张脸是"绑上了但没人敲"；而回落的第一步就是 **8009**，那是 Cast socket 的口。参考实现能承受这个重叠，是因为它 `start()` 里**先**绑 Cast 再绑 setup（`protocol_cast.py:411-413`），8009 已在自己手里；一个还没接上 Cast 通道的壳只会把它抢走。判据：`setup_tests` 的 `d_a_taken_port_…` 用一份**同样绑在 0.0.0.0** 的夹具占住口（第一版用 `127.0.0.1` 占，结果 wildcard 绑定照样成功，那条"拒绝"从未发生 —— 夹具没生产会遇到的形状，用例就是在测夹具） |
| `announce`（一个发送 socket，且不加入组） | 参考实现给**每一个** IPv4 接口各绑一个发送 socket 并逐口加入多播组（本机量到 6 个 socket、每轮广播 6 份相同字节）；本端口一个 `0.0.0.0:5353` socket，**不 join** | 不是省事，是**别抢读数**：`SO_REUSEPORT` 把每个**进来的**数据报只交给其中一个 socket，而我们自己的浏览器（`mdns.rs`）已经 join 过、正在听设备列表要用的那些包。一个会读的第二个 socket = 设备下拉随机丢一半邻居 —— 而这条丢法在单元层不可见（每个测试各开各的 socket），在真机上读起来像"网络不稳"。**代价也说清**：加入组才能答别人的查询，所以 responder 那一格（§15 的「mDNS **responder**」行）落地时必须把这两个 socket 合成一个读者，而不是再加第三个 |
| `announce`（不做唯一性探测） | 参考实现在广播之前先发 3 个 `QU` 的 PTR 查询并等回话（`generate_service_query`，`_core.py:263-283`），撞名就 `NonUniqueNameException` —— 而 `advertise()` 让它冒出去，所以 Python 那一侧撞名的后果是**这台机器根本不广播** | 我们照播。代价写在模块头：同一局域网里两个相同友好名的实例会发布**同一个** instance 与 SRV 目标，发送端可能把名字解到另一台的端口上。**而且我们连那个探测包都不构造**：没有"发完再听"的探测就只是一句问话，谁也不答它，写出来就是一段长得像覆盖的死代码（§4.13「不提问的检查」的形状）。语料里那行 `probe` 因此是**参考实现的**证据，`every_packet_we_write_is_a_response_and_never_a_query` 断言我们四个构造器没有一个交回它 |
| `announce`（属性过长时） | 参考实现**整条起不来**：`ServiceInfo(...)` 打包 TXT 时对 >255 字节的属性抛 `ValueError: bytes must be in range(0, 256)`，而这一句不在 `advertise()` 那两个 `try` 里（本机实测：200 字节过、300 字节抛），于是异常一路冲出 `ChromecastProtocol.start()` —— 用户改一个长设备名，买到的是 Chromecast 接收端没了 | 本端口砍属性、不砍接收端：按字符边界截到 255 字节，WARNING 里点名字段、原长度与上限，然后照常广播。方向与 §4.8「越界夹住而不是拒绝」、`vt_level` 的"少钉一个 level 只是不明确，钉错是每一帧都没了"是同一条。**Python 那一侧今天仍然如此**，这句话不是说它修好了 —— 记在这里是因为它是一条会被下一次"照抄参考实现"重新引进来的行为 |
| `announce`（没有监听就不广播） | 参考实现里 `_advertise()` 在 `_start_tls_server()` 之后无条件跑，而 `cast_port` 绑定失败时保持默认值 —— 于是它广播一个**没人听的端口** | 本端口只在 8009 真的在听时广播（`main.rs` 里那一格是 `match cast_channel.as_ref()`）。一台出现在菜单里却投不上的设备，正是 `AGENTS.md` §4.1 第一行的症状，不是它的小版本 |
| `relay`（等待预算） | 两个等待（响应头之前、EOF 之后管道里）**合成一个连接级参数** | Python 里它们是同一个 `GROW_WAIT_SECONDS`，因为它们是**同一条策略**："读者可以先干等多久，才轮到'还没有'这个诚实答案"。写死成两个常量之后，一个夹具里两条预算可以悄悄漂成不同值 |
| `channel`（读的方式） | Python 在**裸 socket** 上阻塞读、读期间不持任何锁；本端口**每条连接一把锁**，所以读改成 `READ_POLL`（200 ms）轮询，**锁在两批字节之间放开** | 这是"参考实现做不到"的那一类，不是风格：rustls 的一次 `read` 会动会话状态，必须串行化，而把锁跨在一次无限阻塞读上会让**同一根 socket 上的所有推送饿死** —— `_watch_playback` 与状态变化都排在那把锁后面，发送端 LOAD 之后只拿到那一次请求驱动的答复、此后一句话都没有，直到它自己断开（2026-10-18 接进壳之后由 §21 量出，见 §16 第 11 条）。Python 那边根本没有这一层，所以这一格**不可能**由"照抄它的写法"得到正确答案 |
| `channel`（`MAX_FRAME_BYTES`） | 帧头声称的长度 > 8 MiB 时**先拒再分配**（`InvalidData`，不 `vec![0u8; length]`）；参考实现没有这个上限，它 `_recv_exact(sock, length)` 一路累积 | 那 4 字节是**对端给的**，`>I` 能声称 4 GiB。Python 的累积读至少不会一次性申请，而我们的实现一旦先申请就给了"一个谎长度打爆接收端"的形状。代价要写清：一台**真的**发 >8 MiB 帧的发送端在这里会被断开而不是收下。**8 MiB 是我们选的数，不是从设备上量出来的上限**（本局域网没有真 Chromecast，§16 第 1 条），它的依据只有两条：正文是 JSON 与 protobuf 控制消息，而 `>I` 那个字段可以对端随口写。**这一句只到"会话结束"，不到"用户看得见"**：`client_loop` 的错误统一走那句 `Cast client disconnected: {e}`，而它是 **DEBUG** 级（与对端正常断开同一句，`channel.rs` 的 `handle_connection` 尾部），所以默认阈值下这条拒绝**不进日志**。这是今天的形状，不是已验证的决定 —— 要把安全侧的那一格抬到 INFO，改的是这一句分级，不是上限 |

**读代码时要找的下一类偏离**：任何一处"Rust 里做不到 / 做起来别扭"的 Python 行为，
就是必须新增一行账的地方。不允许的处理方式是把它简化掉并在提交信息里不提。

---

## 9. 线序即格式

这一节集中说明**为什么不能"整理"输出**。三条都是被真设备问过的。

### 9.1 SSDP 的头序

`register()` 发出的顺序是 `USN, LOCATION, ST, EXT, SERVER, CACHE-CONTROL`；
`do_notify` / `do_byebye` 里那次 NT 改写（`copy`、`NT = ST`、`del ST`）交回
`USN, LOCATION, EXT, SERVER, CACHE-CONTROL, NT` —— **不同的顺序，故意的**，
而且是唯一被真控制点测过的那一份。所以 SSDP 注册表是**有序 `Vec`**，不是 `HashMap`：
一张"更整洁"的表会打乱头序，而这台机器上没有任何用例看得见。

### 9.2 JSON 的键序 = 发送端读到的答案

`serde_json` 开 `preserve_order`，根 `Cargo.toml` 给的论证是：
`LIVE_LOAD_OPTIONS` 在 Python 是一个 **dict**，它的插入序**就是**键到达 mpv 的顺序；
而日志里引用一条已解码回复的行，按服务器顺序印键 ——
*"A BTreeMap-backed Map silently re-sorts both."*

Cast 侧同一个形状：`RECEIVER_STATUS` / `MEDIA_STATUS` 的键序是答案的一部分
（`macast-cast/Cargo.toml` 的 `serde_json` 注释）。
还有那条 `app_namespaces()` 必须交回 `[{"name": …}]` 而不是 `Vec<&str>` ——
schema 是 `message Namespace { string name = 1; }`，交裸字符串数组**在我们这一侧不产生任何错误**，
而结果是"设备搜得到、投不上"（`AGENTS.md` §4.1 第二行）。

### 9.3 CJK 转义让 grep 失效

本端口**每个 CJK 字符都转义成小写 `\uXXXX`**（对齐 Python `json.dumps` 默认）。
后果是验收脚本里那几句中文拒绝文案**在原始字节里搜不到** ——
`grep` 它们在一个**正确**的答案上什么都匹配不到。所以
`scripts/verify_rust_receiver.sh` 是**解析**响应再比较字段，不是 grep 正文。

---

## 10. 依赖图纪律

根 `Cargo.toml` 的每个依赖都带一段"为什么是这个版本 / 为什么开这些 feature"的注释。
下面四条是最容易被"顺手升个版 / 打开默认 feature"破坏的。

1. **`socket2 = "0.6.5"`** —— 它已经以 0.6.5 在图里（经 `tokio` / `hyper-util`；
   `Cargo.lock:1303`、`:3394`，包体行 `:2850`）。点名一个更旧的版本
   *"would put a second copy of the same library in the tree to buy one setsockopt"*。
2. **`windows-sys = "0.61"`** —— `Cargo.lock` 为 src-tauri 侧十个包（dbus、dirs-sys、mio、
   muda、softbuffer、socket2、tokio、tray-icon、winapi-util、window-vibrancy）
   携带 0.61.2；0.59.0 只经 `winreg` 可达，0.45.0 只经 `jni`。
3. **`rustls = 0.23`，`default-features = false`，`features = ["ring","std","tls12"]`** ——
   默认的 `aws-lc-rs` provider 要**编译一个 C 库**，因此需要 CMake 加 C 工具链，
   而那是**每一台**编译 Macast 的机器的新要求，包括一台普通笔记本上的 `cargo test`。
   `ring` 5.49 秒编完且正是 `rcgen` 已经在用的那个；`tls12` 留着是因为
   *"a Chromecast sender offers TLS 1.2, and dropping it would make the receiver
   undiscoverable-broken rather than merely slower"*；而且
   *"Nothing else in `Cargo.lock` asked for rustls at all, so this choice is not a feature
   *unification* — it is the only opinion in the graph"*。
4. **`ureq = 3`，`default-features = false`，`features = ["rustls"]`** —— 逐条对齐 Python 的
   fetch 形状：`http_status_as_error: true` == `HTTPError`、`max_redirects: 10` == urllib 的
   `max_redirections`、超时**设在请求上**而不是 agent 上（"so the fetch seam stays a pure
   function of its arguments"）；gzip / brotli / charset 关掉是故意的 ——
   *"Python's urllib sends no `Accept-Encoding` either, and a server answering a request
   *we* spelled differently is a difference nobody can see."*

**通则**：加依赖之前先问"图里已经有几个意见"。两个 provider、两份 socket2、
两个 windows-sys，读起来都是"版本选择"，实际上都是**同一件事被实现两次**，
而下一次调试的人会同时改一处、测另一处。

---

## 11. 字符表为什么不是依赖

`media_resolve` / `html` 这一层的编码表（gbk / big5 / shift_jis / cp932 / gb18030）
**留在仓库里**（`macast-cast/src/{gbk,cjk}.rs`），不用 `encoding_rs`。这是量出来的决定：
试过之后与 **CPython 逐格对拍**，四组双字节家族的 lead/trail 单元格有分歧 ——

| 家族 | 分歧格数（每家族 32,386 格） |
|---|---:|
| big5 | 5,144 |
| shift_jis | 2,795 |
| cp932 | 841 |
| gb18030 | 21 |

**"一格错不是卡片上报错，是标题里一段安静的乱码"**（原文：*"A wrong cell is not an
error on the cast card, it is silent mojibake in a title"*）。
表是从这台解释器生成的，之后在**五组家族共 505,120 条有词行的语料**上重测，**0 分歧**。
utf-8 仍走 `std::str::from_utf8`；单字节标签是手抄表，理由是
*"their whole alphabet is 256 cells"*。

---

## 12. 日志见证与测试隔离

这一节是**任何新增用例的作者必须先读的**，因为它决定"你能不能说这句通过了"。

### 12.1 机制

`macast-core/src/logging.rs`：默认阈值 `Info`；`set_threshold` / `threshold`；
`Logger::log` 先按阈值早退，再把 `"{时间} [级别] 消息\n"` 在写锁下格式化，
分发到 `rotate_write`（模块日志 `MODULE_LOG_MAX_BYTES` = 1 MiB × 1 份备份，
主日志 `LOG_MAX_BYTES` = 2 MiB × 2 份）；`Logger::named` 是一张按名字索引的
进程全局表；`claim()` 用 `OnceLock` ⇒ **第一个认领者赢**；
`read_tail(path, lines)` 把 `lines` 夹在 `1..=MAX_TAIL_LINES`，**打开失败回
`TailRead::empty()`**（文件不存在不是错误）。

于是"这条日志有没有写"是一个**可以断言的事实**，前提是它写到了我们能读走的地方。

### 12.2 隔离（`pin_test_dir`）

`cfg(test)` 只作用于**被测 crate**。所以 `macast_core::paths::setting_dir()` 里那句
`#[cfg(test)]` 保护，在依赖它的 crate 编译测试时**根本不存在**。后果不是测试变丑，
而是 **`cargo test -p macast-cast` 会往用户真实的
`~/Library/Application Support/Macast/macast.log` 里写**；而 append 模式下，
每一个"恰好一行说 X"的断言都会在**正确的代码**上失败。

对策是每个 crate 自己的 `pin_test_dir()`，形如（`macast-cast/src/lib.rs`，逐字）：

```rust
#[cfg(test)]
pub(crate) fn pin_test_dir() {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(|| {
        let dir = std::env::temp_dir().join(format!("macast-cast-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        macast_core::logging::remove_log_files(&dir);
        macast_core::paths::set_setting_dir_override(&dir);
    });
}
```

**这条规矩有牙**，不是靠自觉：`macast-cast/src/proto.rs:847` 那条用例
glob 本 crate `src/` 下每个 `.rs`，对每一行 `#[test]` 找到函数体第一句，
要求它逐字是 `crate::pin_test_dir();`，并且扫描本身不许为空 ——
*"an empty scan is not a pass"*。

**见证日志的用法**：要断言某句日志写过，用**每条用例自己唯一的**
`Logger::named(...)` 名字（配合 `claim()`），不要在共享名字的日志里 grep。
需要 DEBUG 行的见证要先 `set_threshold(Level::Debug)`，**并在同一个作用域结束时还原** ——
阈值是进程全局的，不还原就等于让后面的用例跑在一个不同的产品配置上。

---

## 13. 验证模型：三层，各自答不同的问题

### 13.1 第一层 —— 单元（打桩，不碰网络与子进程）

`cargo test -p <crate>`，逐 crate 跑。
这一层问的是"我们的读法与算法对不对"，测的是**我们自己那一侧**。
它的天花板写死在 `AGENTS.md` §4.9：网络全打桩的套件**抓不到"真实发送端栈不认账"**这一类。

**而且这一层有两个粒度，谁也不是谁的超集**：`cargo test -p macast-cast --lib` 与
`cargo test -p macast-cast --lib <module>` 是两台仪器。2026-10-18 那轮的读数是决定性的：
一个用例被**进程级日志阈值**的邻居决定生死，**模块级 14 跑红 12，整 crate 6 跑红 0**
（见 §16 第 9 条）。所以一轮改完至少两个粒度各跑一次，且一句"整 crate 绿"不许被复述成
"这一族没问题" —— 上一轮的 `DebugWindow` 就是这么被当成收干了那族 flake 的。

### 13.2 第二层 —— 与 CPython 对拍的差分语料

关键判断：一条语义**不该由 Rust 侧的推测来决定**。做法是在这台机器上从
**CPython 3.12.15** 把真答复冻成语料，再要求 Rust **逐字节答回**。
语料产生在 `/tmp`，行**冻结进 `*_tests.rs` 模块**（本 crate 没有 `tests/` 目录）：

| 语料 | 规模 | 冻结位置 |
|---|---|---|
| resolve job（MR15c） | 162 行 | `macast-cast/src/job_tests.rs:1` 头注释点明出处 |
| scrape / html | 325 与 263 条 | `scrape_tests.rs` |
| 引擎判定（MR*） | 601 行 | 提交 `db53144` 的交付 |
| 字符表 | 32,386 格 × 家族；505,120 条有词行 | `charmap.rs` 直接把真值文件对到自己的答案上 |
| `pyjson` | CPython JSON 扫描器重放 | `macast-mpv/src/pyjson.rs` |
| mDNS 广播报文 | **4 行**（announce ×2、goodbye ×2，加一行 probe）| `macast-cast/tests/golden/mdns_announce.hex`，由 `scripts/mdns_announce_probe.py --fixture --dump-hex` 产出 |
| mDNS 名字规则 | **26 例**（友好名 → instance → SRV 目标）| `macast-cast/tests/golden/mdns_names.jsonl`，同一支探针的 `--dump-names` |

**这两行与上面几行的区别值得写出来**：它们是仓库里的文件而不是测试源码里的行，因为报文是
二进制 —— 而语料的产生器在仓库里（`scripts/mdns_announce_probe.py`），所以"重新冻一次"是一条
命令，不是一次考古。产生器只替换 zeroconf 唯一的发送口，所以冻语料这件事**一个字节都不上局域网**。

引用的真值文件路径写在用例里（例：`resolve.rs:1900` 引 `/tmp/mr5_corpus.json`，
`resolve.rs:3139` 引 `/tmp/mr15c_truth.txt`）——**它们是证据，不是夹具**：
用例冻的是行，不是每次重跑 Python。

### 13.3 第三层 —— 真机：`scripts/verify_rust_receiver.sh`

它的存在理由（脚本头部原文提炼）：*"The unit suite stubs the network, so it cannot say
'a control point that does DESCRIBE / SetAVTransportURI / SUBSCRIBE on the wire actually
reaches the mpv this page drives.'"*

设计与陷阱（都在脚本注释里，这里给索引）：

- **隔离**：全新的 `MACAST_SETTING_DIR` + 自己的 `MACAST_MPV_SOCKET`
  （*"so it never lands in the Python app's /tmp namespace"*），退出时按套接字路径
  收掉应用与它自己的 mpv。**绝不碰用户配置**（§16 第一条约定）。
- **进程管理**：`perl` + `POSIX::setsid` —— 这个二进制是前台 Tauri 应用，裸 `&` 会随
  shell 一起死，而 **macOS 上没有 `setsid` 命令**。
- **明说的代价**：跑的这段时间里本机会经 SSDP（UDP/1900）在局域网上**再广播一台
  Macast**；不要在演示时跑。
- **`skip()` 也累加 `FAILED`**：*"A check that cannot run (missing tool) is not a green
  … so 'all green' is never vacuous."* 退出码 = 失败检查的条数。
- **`MACAST_VERIFY_KEEP=1`** 保留临时目录，*"because a failing run is the one you actually
  need the logs for; without this the trap deletes the only witness before anyone can
  read it"*。
- **两条拒绝文案存成变量、按最短前缀识别**（`:186` / `:189`）：段 9 与段 11 断言调用者
  听到的是**令牌**那句；段 8 断言某条**在任何门跑之前就答了**的路由上两句都**不出现**；
  段 10 断言局域网调用者听到**管理**那句而非令牌那句。检查是累积且有序的，
  所以"调用者听到哪一句"是唯一能证明**哪道门跑过**的外部证据 ——
  *"a route cannot write its own apology"*（对齐 `AGENTS.md` §4.7c）。
- **`api_post` 故意不带令牌**：loopback **就是**这些路由的管理门准入（§4.7），
  段 15–16 有意义的断言是**门文案缺席**，不是秘密的存在。
- **段 22 是这一份脚本里唯一一段**用**别人家的栈**当判据的**：`.venv` 里的 zeroconf 冷启动当监听者（它的 `ServiceBrowser` 只登记名字，完整性由 `load_from_cache` 逐条问 PTR/SRV/TXT/A），所以"我们的写包器和我们的读包器互相认同"这种自证在这一段不成立。两个坑记在这里：① 0.151.3 的 `ServiceBrowser` 第三个位置参数是 handlers 不是 timeout，写错之后一段变 7 条红；② `IPVersion` 的成员叫 `V4Only` 不叫 `V4`，写错之后 traceback 打进本该放数据的那个文件，后面 5 条检查全读成 `none`。**两次都是仪器先坏，产品没事** —— 判据（§4.2）在这里的形状是"先去看那条已经 PASS 的解析检查"。另外这一段的等待时长本身是读数：跑在第一串广播之后好几分钟，能解析就等于 60 秒重播活着。
- **正文解析而非 grep**（§9.3 那个转义后果）。`--noproxy`：一个游离的 `http_proxy`
  会把回环请求送去代理，产出一个看起来像"端口死了"的 `000`。
  `-m 40`：`cast-resolved` 会同步跑源站探测（`probe_proxy`，转封装那一路最长
  `GROW_WAIT_SECONDS` = 20）。
- **段 21 的"投屏进 Cast 通道"要**并发**轮状态表，不能等发送端脚本返回**（2026-10-18 自己踩过）：
  `vlc_sender_sim.py` 跑到自己是 `PLAYING` 就退出，而共享传输状态表在那之后还会继续被
  渲染器更新；第一版把 `wait_transport` 写在脚本返回**之后**，于是**产品是对的、读数是空的**。
  现在先 `soap_action Stop` 当控制（证明"表停在 STOPPED 不是因为我们没问"），再起一条
  后台轮询（`STATE_WAIT`），然后才跑发送端 —— 与 `AGENTS.md` §4.2 那条
  「凡是"某件事发生过"的判据坐在别人的账本上，就要等那个账本」同一条规矩，
  只是这次的账本是我们自己的状态表。

### 13.4 变异纪律：0 红是问题，不是成绩

一条修复如果删掉它全绿，它就没被测过。Rust 侧沿用 `AGENTS.md` §4.2 / §4.14 的做法，
并把两个操作约束写死（memory 里同样记着）：

- **一条命令一个变异体**，带 EXIT trap 从 `/tmp` 还原 + md5 校验；
  **还原之后必须重新构建**（同一长度的还原会留下一个"有效"的旧 `target/` 产物，
  那是 Python 侧 stale `.pyc` 那一族的 Rust 版本）。
- **落刀之后先问"改上了吗"**：`perl -0pi -e` 的模式如果根本不匹配，文件纹丝未动，
  而紧跟其后的 `cargo test` 会在**未变异**的产品上打出一句没有意义的绿。
  现在每一支都带 `grep -q '<新文本>' || { echo "MUTATION DID NOT APPLY"; 还原; exit 9; }`，
  反向也要问（把**旧文本**删干净的那一支还要 `! grep -q '<旧文本>'`）。
- **编译错误不是"0 红"，是"根本没跑"**：把 `String` 侧的方法换掉会改掉表达式的类型
  （去掉一次 `to_uppercase()` 让 `Ok((state, …))` 变成 `&str`，`E0308` 之后一条用例都没执行），
  而只看退出码时长得和"这一格没人问"一模一样。**变异体必须同型替换**，
  并且读的是 `tee` 下来的那份日志，不是终端上最后一行。
- **"0 红"必须当覆盖空洞处理**：Python 侧有三支定点变异第一次交回 0 红，
  全都是"这一格没人问过"，而不是"实现更稳"。补法是**让那一格可达**
  （空文件但状态仍在跑的中转 ⇒ 503；真的把请求送出去并断言源站收到的 `Range`），
  不是再断言一遍。
- **撤回的"0 红"要留下新读数，而不是抹掉旧句子**：一句写进注释的覆盖判定如果在这台机器上
  不复现，正确动作是把**这一轮的读数**写在提问的那个位置旁边，并明说那句旧的没有复现 ——
  这和 §14 那条"散文落后于代码、和散文抄错代码，是同一类缺陷"是同一件事，
  只是方向反过来：这次是注释**低估**了自己的牙。

---

## 14. 设备发现那一层的具体设计（#22-A / issue #27）

这一层有两半，都在 2026-10-16 是本机的读数：`macast-cast/src/mdns.rs`（Cast 那半，
mDNS browse）加它的 `mdns_tests.rs`，以及 `macast-cast/src/targets.rs`（DLNA 那半，
SSDP 客户端 + 两张表 + 卡片文档）加 `targets_tests.rs`。
`cargo test -p macast-cast --lib -- --test-threads=1` ⇒ **556 passed / 0 failed**（10.62 秒），
其中 `mdns::mdns_tests` **66** 条、`targets::targets_tests` **75** 条。

> 这一节原先写的是「`mdns.rs` 已完成，唯一缺的是它的测试模块（该文件不存在 ⇒
> 这是目前唯一挡住 `cargo test -p macast-cast` 的东西）」。那句在写它的那一刻是对的。
> 撤回留在文档里而不是抹掉，因为它正是 §4.13 那条规矩的形状：
> **散文落后于代码，和散文抄错代码，是同一类缺陷** —— 区别只在于落后要由下一次读数纠正，
> 而抄错要由一个见证纠正。上面那行 556/66/75 就是这一次的读数，且**只来自这台机器**（§16 第 2 条）。

### 14.1 五条红线（原文在 `mdns.rs:57-88`）

1. **比较任何记录之前先把 class 掩码**。真 mDNS 答案带 cache-flush 位，
   所以 SRV/TXT/A 在线上是 `0x8001`。
   *"The failure is silent and total: zero devices, on a LAN full of them, while every
   hand-made fixture stays green"*（`AGENTS.md` §4.2「不提问的检查」）。
   注释还预告了用例：**"There is a test that sets `0x8000` for exactly this."**
2. **账本按小写名做键**。SRV 的 target 写 `Macast-abc.LOCAL.`、A 的主人写
   `macast-abc.local.`，在局域网上是一台设备，在我们这儿若忘了就是两台。
   **输出保留原始拼写。**
3. **不设 `IP_MULTICAST_LOOP`**，因此需要 `drop_ours` ——
   否则第一条永远是本机，而「投给我自己」不是一个目标。
4. **不许把轮询循环变成查询风暴**：`asked` 按大小写不敏感键，一次 browse 内
   每个 (name, type) 只有一条。
5. **发送 id 一律 0 且不按 id 匹配**（RFC 6762 §10）：unsolicited 公告带 id 0，
   按 id 匹配会**恰好丢掉这些**，而 §10 允许摄入它们。

### 14.2 套接字构造：为什么这里没有 unsafe

`socket2` 只用于 create / options / bind / join —— 而**选项必须排在 bind 之前**，
这正是 socket2 进本 crate 依赖清单的全部理由（根 manifest 有长注）。
`SO_REUSEPORT` 走裸 `libc::setsockopt`，因为 socket2 的 setter 在
`features = ["all"]` 后面，而这个工作区不开它。
最后 `let sock: UdpSocket = sock.into();` —— **解析器因此不持有任何 unsafe**。
这是**故意与 `macast_upnp::ssdp` 相反**：那边直接从 socket2 的 `Socket` 上读
（`[MaybeUninit<u8>; 1024]`，`ssdp.rs:597-609`）。两个形状都成立，
差别是"unsafe 住在哪一层"。

### 14.3 来源第三层

DNS 常量、指针保护集、class 掩码规则是从 **zeroconf** 转写的
（`_protocol/incoming.py:43-49`、`_dns.py:116-122`、`const.py:38-41`、`:109-137`），
许可 **LGPL-2.1-or-later** —— *"a layer whose obligations belong neither to us nor to
`xfangfang/Macast`, so rewriting the code does not make it disappear"*
（`AGENTS.md` §4.12；同一形状还有 `macast/ssdp.py` 里的 Coherence 作者群）。

### 14.4 实现过程已经被用例纠正过的五处（留在设计里，因为它们是判据的形状）

1. `decode_labels` 第一版"从指针目标继续扫"，而不是返回 `off + 2`
   ⇒ 凡 rdata 跟在压缩名之后的记录，都读在错的字节上。
2. `read_others` 第一版把三种恢复写成同一种；**Python 那里是三种不同的**。
3. `ingest` 的 PTR 分支把 `ttl == 0` 判在去重**之后** ⇒ 一个全新实例的 goodbye
   把它的 PTR 名留下了，而第二次 goodbye 什么都不做（就地记录在 `:867-878`）。
4. `query` 里 `min(63)` 的标签截断会发出一个长度字节与实写字节不一致的报文 ⇒ 改成**拒绝**。
5. `txt_value` / `device_name` 的空串兜底必须显式写出，不能被折叠掉。

### 14.5 DLNA 那一半：`targets.rs`（SSDP 客户端 + 两张表）

移植对象是 `macast/plugins/renderer/cast_local_file.py` 的发现块（`:1085-1247`）、
两张缓存（`:1001-1005`）、卡片文档（`:1622-1665`）与设置页轮询的两个入口
（`:2895-2914`）。与 `mdns.rs` 不同，**这一层没有第三层来源**：SSDP 是规范，
`M-SEARCH` 是六行头，设备描述是我们已经会读的 XML（`macast_upnp::scpd`）。

形状上照 `resolve.rs` / `relay.rs` 的同一套接缝纪律：**判决住在函数里，
系统调用作为参数进来** —— `SsdpWire` / `Searcher` / `Describer` / `Fetcher` / `Sweeps`。
`parse_description` 是全部怪癖的落点，而**这些怪癖没有一条能从真套接字问出来**。

五处**照抄的怪癖**（每条都读起来像 bug，留着是因为局域网上有设备依赖参考实现的这个行为）：
`port 0` 是「没意见」（`if parts.port` 对 0 为假）；`startswith('http')` 而非
`startswith('http://')`（两处）；AVTransport 匹配**区分大小写**（`_local_name` 只小写标签，
不小写文本）；`port` 在**宿主改写之前**算（`:1161-1168`，`_port_suffix(parsed)` 在
`:1163`，回环改写在其后的 `try` 里）；第一个 `<service>` 命中就 `break`，
**即使它没话说**（后来那条完整的 AVTransport 因此救不回这台设备）。

**刻意偏离的八条**逐条写在模块头 `:66-113`，其中最贵的三条：

1. **一个坏 `base_url` 只丢一行，不再掀掉整趟扫描**（Python 的 `describe_renderer`
   在 `return` 里调 `parse_description`，那在其 `try` **之外**）。
2. **应答侧同一次裸 `urlsplit` 也不再掀掉整趟** —— `:1222` 的
   `host = urlsplit(control).hostname or peer` 坐在三个 `try` 的**外面**
   （`parse_description` 的只包 `ET.fromstring`，`describe_renderer` 的只包取回，
   `discover_renderers` 的循环只包 `peer in ours` 这一问），
   所以一个 `urlsplit` 咽不下的控制 URL 在参考实现里是「循环终止 + 两趟 `ST` 里
   所有还没读的答复全部丢掉」。这一格**曾在本节被判为「按构造不可达」，那是错的，两处都错**：
   `Describer` 是一个 trait，正是为了让别的答复能进来；而 Python 对同样字节的答复不是「跳过」。
3. **`start_target_search` 两个扫描都起**。Python 的
   `bool(start_search() or start_renderer_search())` 短路：mDNS 一起，SSDP 就不起 ——
   于是**场上有一台 Chromecast 和一台电视的家里，「重新搜索」永远刷不出 DLNA 列表**，
   而两句 docstring 都写着「两种设备都问」。这是参考实现的缺陷，不是它描述里的缺陷，
   所以**报告而不是照抄**。

**这一层没有见证者的四格**（按 §13.4 记账，而不是补一句假断言）：

- **Python 的 `if root is None`（`:1151-1152`）** 与 **`_local_name` 里的 `isinstance(tag, str)`（`:1120`）** ——
  `Dom::parse` 要么给根要么给 `Err`，`macast_upnp::scpd` 里注释与处理指令**不是节点**，
  所以这两支在 Rust 侧**没有可到达的形状**。「什么都不会失败」不等于「覆盖了」。
- **`ours.contains(&peer)`**：`host` 来自 `macast_core::net::advertised_hosts`，
  它的第一步是 `Snapshot::collect()` —— **活的网卡状态，没有注入接缝**。
  能驱动它的用例只能取决于这台机器上插着哪些线，所以 K 段条件性地问它，
  并**明说它跑不了**。这一格和红线 3（`drop_ours`）是同一件事的两个方向：
  滤不掉自己，卡片上的第一台设备就是**我们自己那台接收端**，
  而「投给我自己」不是一个目标。
- **`thread::Builder::spawn` 返回 `Err`**：要撞到它得耗尽操作系统线程上限，
  而唯一诚实的安排方式会同时打断本进程其余每一条用例。它的句子**没有断言**，
  在这里记为欠账。

**变异读数**（2026-10-16，一条命令一个变异体，`/tmp` 备份 + EXIT trap 还原 + md5 校验，
还原后重新构建；两条都是 `md5=010232d4…` 逐字节回来）：

| 变异体 | 红 |
|---|---|
| `Err(e) => { log.debug(…); continue; }` 改成 `Err(_) => peer.clone()`（坏控制 URL 不再跳过，而是拿应答者地址建行） | **1 条**，且红的是「跳过的渲染器不许变成一行」这一问本身 |
| 同一支只删掉那句 DEBUG（保留 `continue`） | **1 条**，红在同一个见证的另一半（行没了 ⇒ 上一支不响；DEBUG 没了 ⇒ 这一支不响） |

两支各红 1 条且**红在互补的两问**上，这正是那条用例的形状：它同时问「少一行」与「说一句」，
而这两问各自都能被一支定点变异单独打掉。

**这一趟没有一条红来自产品，而这是它自己的缺陷登记**。收口时把注释与文档里的每个 Python
行号都 `awk` 了一遍，结果是**二十多处从计划/记忆里抄来、从未问过机器的锚点**：`start_search`
写成 `:1061-1082`（真值是 `:1061-1070`，`finally` 在 `:1080` 不是 `:1079`），卡片文档写成
`:1640-1650`（真值 `:1639-1647`），`target_refresh` 写成 `:2904-2914`（真值 `:2909-2914`），
而 `isinstance(tag, str)` 写成 `:1117` —— **那一行在文件里是空的**。另有两处**不是行号错，
是陈述错**：`targets.rs` 与它的用例各写了一句「`src-tauri/src/host.rs` 拒绝不带这三个键的答复」
⇒ 那个壳**只是产出这份文档，没有任何一处检查它**，真正的判据在
`scripts/verify_rust_receiver.sh:1512-1523`；`host.rs` 的 doc-comment 里 `json!` 那一段被引成
`:1543-1552` ⇒ 那一行在 **1553**，落在被引用区间之外。
AGENTS.md §4.13 那条「一个没被机器问过的数字」在这里的形状就是**行号**：注释里的锚点读起来
和测量一样权威，而它会随被引用文件的下一次编辑而腐烂。修法不是"以后小心"，是
**每一处引用都问一遍机器**（`awk 'NR>=a && NR<=b'`），并把「谁在检查这句话」写进句子本身。
（同一趟曾误判 `_url_target_unavailable`「全仓不存在」——那是 `grep --include=*.py` 被 zsh
的 glob 吃掉之后**命令根本没跑**，而我把它的失败读成了结论；协议侧那一处是真的存在，
在 `macast/protocol.py:1754-1763`，已按真实出处改写。这条也要留着：**一条没有跑起来的
命令不是「零匹配」**。）


---

## 15. 刻意不做 / 尚未做

一份没有这一节的移植文档，读者会把"还没做"读成"已经安全"。
以下逐条来自各 crate 自己的注释，不是推测：

| 事项 | 状态 | 出处 |
|---|---|---|
| setup 面（HTTP/8008 + HTTPS/8443：`/setup/eureka_info` 裸 UUID、`params=` 过滤、`/setup/icon.png`） | **已交付**（2026-10-18，`macast-cast/src/setup.rs` 655 行 + 壳在 `main.rs` 里起它） | 三处**刻意偏离**写在模块头，不在这里复述：`Server:` 说我们自己的名字、非 `GET` 回 501 且无正文、以及 **8008 不回落**（参考实现走 20 个口，而发送端把 8008 钉死在 `dial.py:25` —— 回落只买到一个没人敲的口，还会在通道没起来时把 8009 从它嘴里抢走）。判据：`verify_rust_receiver.sh` §20（**26 条**，含一次真 TLS 握手与"8008/8443 交回同一串字节"）+ `setup_tests` 的 30 条 |
| Cast 接收端接进壳（`channel.rs` 的 8009 TLS 会话、`RECEIVER_STATUS` 回报、`_watch_playback` 那一格） | **已交付**（2026-10-18）：`main.rs` 走 `cert::ensure_in_setting_dir` → `CastChannel::new` → `host.set_cast` → `channel.start(pair)`，且**排在 setup 脸之前**（8009 才是发送端要连的那个口，两句话都在 `main.rs` 的注释里）；`host.rs` 把 `Pause`/`Play`/`Stop`/`Eof`/`TransportError` 五次 `set_state` 扇进这根通道，`cast_uri` 交还播放权时调 `release_playback()` | 判据是同一句 grep：`grep -rn "CastChannel" src-tauri/src/` 从 **0 行** 变成 **6 行**（`host.rs` 4、`main.rs` 2）。**接进来才现形的那一格：读不许跨阻塞持锁** —— 见 §16 第 10 条与 `AGENTS.md` §4.14 的 2026-10-18 段。见证 = 真机 §21 的 **5 条**（一支按 VLC 状态机写的发送端打进来并读回 `PLAYING`）+ `channel_tests` 的 **40 条**。这一行标题里的"播放看门狗"指的是参考实现的 `_watch_playback`（`protocol_cast.py:1056`），**不是**重投看门狗：接收端那侧的参考实现里没有重投，那是 `cast_local_file` 发送侧的东西 |
| `_googlecast._tcp` 的**广播**（我们自己接收端的 PTR/SRV/TXT/NSEC/A，unsolicited） | **已交付**（2026-10-18，`macast-cast/src/announce.rs` + 壳在 `main.rs` 里随 8009 起停） | 判据不是"我们自己读得懂"：`verify_rust_receiver.sh` §22 让 `.venv` 里那套 **zeroconf**（参考实现自己用的栈）冷启动去解析，**42.8 秒**后交回完整设备（那一趟整套 **183 条 / 0 失败 / EXIT=0**）—— 而第一串广播那时早就发完了，所以这条同时是 **60 秒 keepalive 的真机见证**。字节层面另有两条冻结语料（`macast-cast/tests/golden/mdns_announce.hex`、`mdns_names.jsonl`，由 `scripts/mdns_announce_probe.py` 产出），其中 announce 有**两行**，因为参考实现那一对 `A/NSEC` 的先后随 `PYTHONHASHSEED` 变（§8 的偏离表与探针注释都写了这一条）。**剩下的不是这一行**：下一行 |
| mDNS **responder**（回答别人的查询） | **尚未** | 我们只发 unsolicited 广播，不答查询。后果是可行动的那句：**只听的发送端**（libvlc 的 microdns，`discovery.py:230-237` 自己写的）第一串广播就看得见我们；**问了才看的**（Cast app、`dns-sd -B`、冷缓存的浏览器）要等下一次重播，也就是最多 60 秒。修法不是"再加一个 socket"：`SO_REUSEPORT` 会把进来的数据报只交给一个读者，所以 responder 落地时必须与 `announce` 的发送 socket 合成一个读者（§8 的「`announce`（一个发送 socket，且不加入组）」那一行写着这个代价） |
| SSDP **客户端**（M-SEARCH 之后拉描述、建设备表） | **库层已完成**（issue #27，见 §14.5）；**2026-10-18 接进壳** | `macast-cast/src/targets.rs` + `lib.rs`（`pub mod targets;`）。同一判据本轮重量：`grep -rn "targets::" src-tauri/src/` 从 **0 行** 变成 **2 行**（`host.rs` 的 `pub targets: Arc<targets::Table>` 与 `main.rs` 的 `Table::new(setting)`）|
| 渲染器插件的 `target_push` 面（设备下拉真正投到远端） | **已交付这一半**（2026-10-18）：壳把 `cast-targets` 从拒绝文档改成读这张表，`cast-resolved` 的设备臂做 `target_by_id` → `push_url_to` | 之前该面答的是**参考实现的完整拒绝文档**（提交 `d9cf9de`）。**剩下的不是接线**：本局域网里没有可投的真设备（§16 第 1 条），所以设备臂的**成功**那一路在真机上仍无见证者，只有 `push_tests` 的 46 条打在自家假设备上；`CastSender` 的 pause/seek/音量与 `text_tracks` 是 §15 下面那行的裁剪决定 |
| `link` 的 **Windows 列** | **未在本机行使** | §4.2 的原文边界句 |
| AirPlay 作为独立 crate | **不做**（现在住在 cast 里） | §2 那处待归位的陈述 |

---

## 16. 未覆盖风险（诚实边界）

1. **这个局域网里没有真 Chromecast**（项目 memory 记录）。电视 192.168.1.13 常常整机关掉，
   看到的 `Macast(...)` 是**别的 Macast 实例**（192.168.1.68），不是接收端；
   自己应答自己的 mDNS/SSDP 查询**不算"发现了设备"**。
   所以 §13.3 的真机脚本从头到尾验的都是**我们这侧的接口认不认账**，不是"别人家的设备行不行"。
   Cast 的 TLS 那一侧**从 2026-10-18 起多了一个见证者**：真机脚本的 §21 用仓库里那支
   **复刻 VLC 状态机**的发送端（`scripts/vlc_sender_sim.py`，含严格 protobuf 语义、必须读到
   `PLAYING` 才算通过）打我们自己这张脸。但它**不是手机/HA 实际用的那套 pychromecast 栈**
   （AGENTS.md §4.9 那一族靠的是 `cast_conformance.py`，而它打的是 **Python** 接收端），
   也**不是真电视**：Google 官方发送端的设备认证那一格，本局域网里没有设备可问，仍然欠着。
2. **本分支从不推送**（项目 memory：`refactor/rust-tauri-2 … never push it`）。
   因此**没有任何 CI 见证**过这里的任何一条用例；每一句"通过"都必须来自本机的读数。
3. **两处"陈述落后于代码"的债，本轮（2026-10-18）关掉，写法改成不钉行号**：
   - `macast-mpv/src/lib.rs` 那句 *"the event bus and the settings live in
     `src-tauri`, which has not been written yet"* 已按 §8「偏离不许沉默」**改注释**
     （不是改本文迁就它）：现在那句话点名 `src-tauri/src/host.rs` 的
     `impl Host for TauriHost` 是该特质的**唯一非测试实现**，并按名字数过 ——
     全仓 `impl Host for` 共四处，其余三处都在 `macast-mpv/src/player.rs` 的测试模块里；
   - `macast-cast/src/proto.rs` 的 `TESTS_IN_THIS_CRATE` 上一轮已经跟着读数走到 **603**，
     并且注释里那份**逐文件构成**（`job_tests` 162 / `targets_tests` 75 / …）就是量出来的，
     所以本文原先记的那句"`= 65`，散文过期"本身才是过期那句 —— 按 §4.13 的规矩，
     一条没有机器在问的陈述要改的是陈述，不是被引用的代码。
   ⚠ 这两条原来都写成 `lib.rs:41` / `proto.rs:410` 这种**行号锚点**，而本轮的编辑把它们
   挪走了。这正是本文 §14.5 末那条"锚点会腐烂"的教训**打在自己文档上**：附录 B 的
   引用体检现在改成按名字问（`grep -n "TESTS_IN_THIS_CRATE" / grep -n "impl Host for"`），
   不再 `sed -n '<行号>p'`。
4. **`Cargo.toml` 曾声明一个不存在的 `macast-airplay`**（§2）—— 本轮删掉那一行，并把
   头部 crate 清单里的那一格改成"AirPlay 是 `macast-cast` 里的模块"。
   **删之前量过它今天并不致命**：`[workspace.dependencies]` 是惰性的，
   `cargo metadata --format-version 1 --no-deps` 在删之前**退出 0**（没有任何成员引用它），
   所以"清单里有一个不存在的 crate"这句话要连着说"直到有人写 `workspace = true` 才变成
   全盘加载错误" —— 那正是这份清单自己的注释写下的形状。
5. **Python 侧 §4.9 那一族边界照旧适用**：真机验证欠的那两件
   —— 一台**真电视**在转封装还没写完时就去打开它，以及**需要登录的站点被真的解开**
   —— 在 Rust 侧同样欠着，且不会因为移植完成而自动补上。
6. **发现层（#27，`targets.rs`）有四处判据没有任何夹具能够到达**，登记处是
   `macast-cast/src/targets_tests.rs:38-56`（模块头 `targets.rs:116-125` 说同一件事）：
   - `root is None`（`cast_local_file.py:1151-1152`）—— `Dom::parse` 要么给根要么给 `Err`，
     这一臂在 Rust 里没有形状；
   - `isinstance(tag, str)`（`:1120`）—— 注释与处理指令在那边是节点、在 `macast_upnp::scpd` 里不是；
   - `ours.contains(&peer)`（`targets.rs:721`）—— `advertised_hosts` 从 `Snapshot::collect()` 起，
     读的是**活的接口表**，**没有注入接缝**。J 段那条用例因此只能**有条件地**问：它拿
     `203.0.113.77`（TEST-NET-3，真局域网里不会是本机接口）问"陌生人保留"这一侧，
     另一侧（"对端是我们自己"）在这台机器上无法诚实构造。
     **这一格的失败形状值得写出来**：滤不掉自己时，下拉里第一台设备就是**我们自己的接收端**，
     用户按下去是把媒体投给正在播它的那台渲染器 —— **没有报错、没有失败消息**，
     只有本机 mpv 重新载入同一路地址，而电视上什么都没变；
   - `thread::Builder::spawn` 返回 `Err`（`targets.rs:924/964`）—— 到达它要把 OS 线程数耗尽，
     而唯一能安排出那个的夹具会同时弄坏本进程里每一条用例。所以那一支**一条断言都没有**
     （手写在字符串层上的"假断言"比没有断言更糟，它会让下一次重写以为这里验过了）。
   这四格**不算已覆盖**，也不许用"分支存在"来代替"分支被走过"。
7. **发现层的客户端那一半，真机上只走到"没有设备可发现"这一格**（2026-10-18 接线后重写）：
   以前这条写的是"`targets.rs` 从未被真机脚本经过，§19 拿到的是壳的拒绝文档，
   `src-tauri/src/` 里 `targets::` 出现 **0 次**" —— 那是 §15 那两行的同一句，本轮已翻面。
   今天的读数是：`verify_rust_receiver.sh` §19 真的把这张表问了一遍（列表是 success 文档、
   第一行是本机、键序 `items,searching,known`、refresh 多一个末位 `started`），
   脚本里与 SSDP 有关的老那一段（服务端那一半，我们自己应答 M-SEARCH）不变。
   **仍然欠的是同一条**：本局域网里没有 Chromecast，DLNA 侧的真设备只有**别人家的
   Macast 实例**，而把媒体推到用户另一台机器不是这个脚本可以擅自做的事（§10 第一条），
   所以"拉到一个陌生邻居的描述、建出一行、投给它"这一路在真机上没有见证者，
   只有 `push_tests` 的 46 条打在自家假设备与假 SOAP 服务器上。
   一句诚实的边界：`known` 为假时上面那些断言**都还能全绿** —— 它们问的是壳与表之间的
   文档形状，不是"网络上有没有东西"。
8. **`searching` 在一台发现不到任何东西的机器上永远是 `true`，而且这是设计**（本轮量出来的，
   第一版 §19 把它写成了一条永远红不起来的判据）：`target_state()` 的冷启动踢法是
   "两份缓存都空 ⇒ 再起一次搜索"（`cast_local_file.py:2897-2907`），所以缓存永远填不满的
   机器上，**每一次轮询都恰好看见有人在搜**。Python 一字不差是同一个行为，
   因此这不是移植缺陷，而是一条关于"卡片能停下来的到底是什么"的事实：停下来的不是
   `searching`，是**用户关掉那张卡**。§19 现在断言的是**闩**（连续两次 refresh 里至多一次
   真的起搜），并且在那条检查旁边写清楚它**没有**证明什么：`false/false` 在这种网络上是常态，
   而"起飞的搜索会落地"这一问只有 `targets_tests` 那两条 `k_…`（拿着 `HoldGate` 放行之后
   要求 `searching` 转假）答得出，真机答不出。
   ⚠ 这一格留在文档里的形状比结论重要：**"等它变假"这条断言在这台机器上不可能为真**，
   写出来就是第二个 Part 34 —— 一组永远红的用例教人忽略红（AGENTS.md §4.12 那条）。
9. **模块级运行与整 crate 运行是两台不同的仪器，谁也不是谁的超集**（与第 8 条同一轮，
   由那条 flake 量出）：`targets_tests` 里"断言见证文件为空"的用例被**进程级日志阈值**
   决定生死（`macast_core::logging::THRESHOLD` 是一个 `static AtomicU8`，
   `lib.rs` 的 `DebugWindow` 故意一次只让一个用例抬高它）。改成按**级别**断言（`loud()`：
   只看 INFO 及以上）之前的读数是 **`--lib targets` 14 跑红 12**，而**整 crate 6 跑红 0**；
   之后是模块级 4 跑 0 红、整 crate 3 跑 0 红（603 passed 每次，约 7 秒 —— 那两个读数都是
   **setup 落进之前**的 603；Cast 接进壳之后（2026-10-18）同两台仪器都说 **637**，
   整 crate 那一趟是 `637 passed; 0 failed; finished in 6.85s`）。
   上一轮那次"并发整 crate 5 红 / 24 次、串行 12 次 0 红"的读数让人以为
   `DebugWindow` 已经收干了这一族 —— **它只收了读 DEBUG 的那一半，没收断言沉默的那一半**，
   而后者只在模块级过滤器下才现形。所以 §13 那三层里第一层的正确做法是
   **两个粒度都要跑**，且一句"整 crate 绿"不许被复述成"这一族没问题"。
   **同一族的第四例在本轮又出现一次，方向是"该有却没有"**：`setup_tests` 里那条"8008 被占时
   什么都不绑"的断言，先要求日志里出现那行 **DEBUG**（每个候选口的失败），而它永远不在 ——
   默认阈值 INFO 把它滤掉了。修法不是放宽断言，是**把窗口打开再问**（`crate::DebugWindow::armed()`
   包住那一次 `start_on`，断言之后再放手）：**沉默的证据要按级别问，响亮的证据也一样**，
   两边都不能让别人的调度决定这一格有没有被问过。
10. **靠"问"来发现设备的发送端，仍要等最多 60 秒**（2026-10-18 关掉这一条的前一半）：
    它最初写的是"setup 面今天没有任何发送端会走到"，理由两格：8009 通道没接进壳，以及
    `_googlecast._tcp` 没有广播。两格现在**都关了**（`grep -rn "CastChannel" src-tauri/src/`
    回 6 行；`announce.rs` 随 8009 起停，§22 用 `.venv` 里那套 **zeroconf** 冷启动解析成功）。
    **还开着的是第三格：我们没有 responder** —— 只发 unsolicited 广播，不答别人的查询。
    所以今天能说出口的边界是这两句：只**听**的发送端（libvlc 的 microdns，
    `discovery.py:230-237` 明说它「never sends a query of its own」）第一串广播就看得见我们；
    先**问**再看的（Cast app、`dns-sd -B`、冷缓存的浏览器）要等下一次重播，最多
    `REANNOUNCE_SECONDS` = 60 秒。§22 那条 42.8 秒就是这个等待的实测形状 ——
    它不是"慢"，是"这一格还没实现"的读数，别把它读成性能问题。
11. **读不许跨阻塞持锁 —— 这一格是接进壳之后才现形的产品 bug**（同一轮，2026-10-18）：
    `client_loop` 第一版在**持有那条连接的 `Mutex` 期间**做 `read_exact`，而读超时是
    `set_read_timeout(None)`（无限）。后果不是崩溃，是**推送饿死**：`_watch_playback`
    和状态变化都想 `send` 到同一个 socket，它们拿不到锁，于是发送端 LOAD 之后**只拿到那一次
    请求驱动的答复，此后一句话都没有**，直到它自己断开。Python 那边根本没有这一层 —— 它的裸 socket
    不需要一把锁来保护加密状态，而 rustls 需要（一次 `read` 会动到会话状态）。
    现在读走 `READ_POLL`（200 ms）轮询 + `read_frame`，**锁在两批字节之间放开**；
    同一趟还把 `vec![0u8; length]` 换成 `MAX_FRAME_BYTES`（8 MiB）之上的**先拒后分配**，
    因为参考实现的 `_recv_exact` 是**累积**读的，从未按对端声称的长度一次性分配 ——
    那是"一个谎言长度就能让接收端 malloc"的形状，而它在 36 条老的 `channel_tests` 里
    **一条都问不到**（它们喂的是已经到齐的帧）。
    教训的形状：**这一族 bug 只有真 socket 才教得了**，而"接进壳"之所以是一等里程碑，
    就是因为它把单元层够不到的那部分换成了 §21 与四条新用例（饿死见证、帧上限、
    分片帧在轮询之间补齐、对端关掉时结束会话而不是空转）。
12. **壳对所有脸都不看 `Macast_Protocols`**（一处既有缺口，setup 与本轮接进来的 8009 都继承它）：
    那份协议启用表今天只用来生成 `query=status` 的 `protocol` 字段，DLNA 的 HTTP/SSDP、
    8008/8443 与 **Cast 的 8009 TLS 会话**都无条件起。后果是可行动的：一个在页面里
    "关掉 Chromecast"的用户，仍会在 8008 上被问到设备身份、**并真的能在 8009 上完成一次投屏**。
    这一格**记在代码注释里**（`main.rs` 的那段
    "Started unconditionally"）并在这里登记；等启用表变成门的时候，那**三**处是同一批要改的地方。
    缓解只有一条事实那么宽：setup 那张脸不吐令牌、不吐路径、不吐任何控制动词 ——
    §20 拿**局域网地址**（不是回环）问过一遍，正文里既没有 `Api_Token` 也没有 `$SDIR`。
    （8009 那一侧**没有**这条缓解：它就是控制面，所以这一格的真实代价要按 §21 那条路读。）
13. **5353 上第二个加入组的 socket 会偷走设备列表的读数**（本轮设计 responder 时撞到的边界，
    记下来因为它决定下一轮的形状）：`SO_REUSEPORT` 的语义是把每个**进来的**数据报只交给
    绑在同一端口上的**一个** socket。我们的浏览器（`mdns.rs::open`）已经加入组并在听
    PTR/SRV/A；如果 responder 也开一个加入组的 socket 去听查询，局域网里的设备就会
    **随机少看一半** —— 而这条丢法在单元层不可见（每个测试各开各的 socket），
    在真机上读起来像"网络不稳"，正是 §4.9 那一族"我们没有崩、也没有 ERROR，只是别人不认账"。
    因此 `announce.rs` 的 socket **故意不 join**（它只发不收，见 §8 的 `announce`（一个发送 socket）那一行），
    而 responder 落地时必须是**把两个读者合成一个**，不是再加第三个。
    参考实现没有这一格不是因为它想到了，是因为它只有一个引擎：zeroconf 的 `Engine`
    本来就同时读 listener 与 respond socket，所有包都进同一个缓存。
14. **广播侧第一条被真机抓到、且第一次就抓到的产品 bug：A 记录发的是"能加入组的接口"，
    不是"发送端够得着的地址"**（2026-10-18，§22 首跑）。线上量到的是
    `192.168.1.5, 192.168.148.0, 192.168.215.0, 192.168.139.3` —— 后三个是 VMware /
    VirtualBox / Docker 的宿主网段，因为 `from_setting` 当时读的是 `advertised_hosts`，
    而那个函数是**浏览器加入组用的宽集合**（`mdns.rs` 的模块头把它与"广播侧要窄"写在同一段里，
    我读到了那句话，还是挑错了函数）。后果不是崩溃也不是报错：**发送端从 A 记录里随机挑一个**，
    挑到虚拟网段就没有路由 —— `AGENTS.md` §4.1 的第一行，本仓库为它写过 `discovery.py` 的
    网卡筛选、写过 `advertise_host()` 的两份副本互钉，这次是**我们自己在 Rust 侧把它装回去**。
    两条判据（`the_published_addresses_come_from_the_reachable_owner`）：行为上比对
    `advertised_addresses` 与 `advertised_hosts` 两个集合的**长度差**（本机 1 对 4，所以这一条
    问得出东西），源码上按名字审计 `from_setting` 的函数体只许出现窄集合那一个 ——
    因为"本机只有一块可用网口"的那一天，前半条会静默变成空话。变异体（换回宽集合）红 1 条。
    **教训的形状**：`§22` 之所以值得写，正是因为它用**另一个实现**读我们发出去的东西。
    自己的解析器读自己的写包器，四个地址里三个是虚拟网段这件事**一条都不会红**。

---

## 17. 提交与文档的规矩

- **提交信息写行为，不写文件**。本分支的历史就是这个格式
  （例：`feat(rust): the cookie panel speaks in counts, the device dropdown refuses as a
  full document, and a cast that arrives on a socket with no peer is held instead of
  logged away`）。读一条提交应当能读出**用户会遇到什么**。
- **契约类改动同一轮改文档**（memory「Proactively maintain project docs」）：
  改了门、改了线序、改了文案，`AGENTS.md` §4 与本文都要跟上；
  引用体检要跑（§附录 B）。
- 与用户协作的四条硬约定在 `AGENTS.md` §10，移植不豁免它们：
  **不为用户配置做任何写测试**、**先分清"发送端问题"还是"我们的问题"再动手**、
  **破坏性操作要可逆**、**提交前自查暂存内容**（别夹带产物 / 密钥 / `macast.log`）。
  凭据与 cookie **只在命令里从文件构造，绝不抄进文档、消息或提交**。

---

## 附录 A：重跑本文引用的每个数

```shell
cd /Users/pavia/githome/Macast
for c in macast-core macast-mpv macast-upnp macast-cast src-tauri; do
  n=$(find $c/src -name '*.rs' | xargs wc -l | tail -1 | awk '{print $1}')
  t=$(grep -rn '#\[test\]' $c/src | wc -l | tr -d ' ')
  echo "$c lines=$n tests=$t"
done
cargo test -p macast-core -p macast-mpv -p macast-upnp -p macast-cast   # 逐 crate，不跑全工作区
```

## 附录 B：引用体检（改完文档跑）

**按名字问，不按行号问**（§14.5 末那条教训落在本文自己身上：本文曾经写
`sed -n '41p' macast-mpv/src/lib.rs` 与 `sed -n '410p' macast-cast/src/proto.rs`，
而 2026-10-18 那一轮改注释就把那两个锚挪走了 —— 一个会因为邻行加了三句话而腐烂的引用，
不是判据，是待修的债）。

```shell
cd /Users/pavia/githome/Macast
# 本文提到的文件确实存在
grep -oE 'macast-(core|mpv|upnp|cast)/src/[a-z_]+\.rs' docs/Rust-Port-Design.md \
  | sort -u | while read f; do [ -f "$f" ] || echo "MISSING $f"; done
# 本文引用的**语句**仍在（每条都有唯一的字串可搜；右边的期望值是 2026-10-18 量出来的，
# 不是"看起来应该"）
grep -n "TESTS_IN_THIS_CRATE" macast-cast/src/proto.rs          # 期望 **656**
grep -n "impl Host for TauriHost" src-tauri/src/host.rs         # 期望 1 行
grep -c "has not been written yet" macast-mpv/src/lib.rs        # 期望 **0**（本轮改的注释）
grep -rn "targets::" src-tauri/src/                             # 期望 ≥2 行（§15）
grep -rn "CastChannel" src-tauri/src/                           # 期望 **6** 行（`host.rs` 4 + `main.rs` 2，§15）
grep -c "set_read_timeout(None)" macast-cast/src/channel.rs      # 期望 **0**（§8 与 §16 第 11 条那一格：
#   一次跨阻塞读的无限超时就是那次推送饿死；这一行是那条修复的**反向**哨兵，
#   谁把它改回 `None`，§21 与 `a_quiet_sender_still_hears_about_playback` 会红，
#   而这条 grep 是它在文档里的对应物）
grep -c "pub const READ_POLL\|pub const MAX_FRAME_BYTES" macast-cast/src/channel.rs  # 期望 **2**
grep -c '^pub fn unquote_plus' macast-core/src/url.rs           # 期望 **1**（主人在这儿）
grep -c 'fn unquote_plus' macast-upnp/src/http.rs               # 期望 **0**（搬走了）
grep -c '^echo "== ' scripts/verify_rust_receiver.sh            # 期望 **25**（§22 是最后一节）
grep -c 'pub fn announcement\|pub fn goodbye' macast-cast/src/announce.rs  # 期望 **4**（两种顺序 × 两种报文）
grep -c 'net::advertised_addresses(' macast-cast/src/announce.rs            # 期望 **1**，且下面这条 **0**：
grep -c 'net::advertised_hosts(' macast-cast/src/announce.rs                # 宽集合回到 A 记录就是 §4.1 第一行
grep -c 'set_multicast_loop_v4(true)' macast-cast/src/announce.rs            # 期望 **1**（发送侧，参考实现的 responder socket 也开环回）
grep -c 'join_multicast_v4' macast-cast/src/announce.rs                      # 期望 **0**：广播口故意不入组，见 §16 第 13 条
ls macast-cast/tests/golden/mdns_announce.hex macast-cast/tests/golden/mdns_names.jsonl
#   两份 mDNS 语料都在，且产生器在仓库里：
#   scripts/mdns_announce_probe.py --fixture --dump-hex <前者> / --dump-names <后者>
grep -n "DEVICE_PUSH_UNAVAILABLE\|这台设备还投不了" src-tauri/src/host.rs \
     scripts/verify_rust_receiver.sh
#   期望恰好两处，而且**都不是产品代码**：host.rs 里那句"这替换了什么"的历史说明，
#   和脚本 §16 里那条**反向**断言（设备臂再说出这句旧话就算红）。
#   除这两处之外任何命中都表示那句话回来了 —— 它是"这台机器还没有发送面"的谎。
# 本文提到的脚本节号确实存在
grep -c '^echo "== ' scripts/verify_rust_receiver.sh
```
