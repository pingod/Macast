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
| `announce`（一个 socket 承担广播 + 应答；2026-10-18 接线前它不 join、也从不读） | 参考实现给**每一个** IPv4 接口各绑一个发送 socket 并逐口加入多播组（本机量到 6 个 socket、每轮广播 6 份相同字节）；本端口一个 `0.0.0.0:5353` socket，**不 join** | 这一行原来的理由**是一句从没问过内核的猜测**，现已量掉（`scripts/mdns_reuseport_probe.py`；2026-10-18 在这台 Mac 与那台 CachyOS 上各跑；egress 用 `IP_MULTICAST_IF=127.0.0.1` + TTL 1 钉在 lo0，一个字节都不上局域网；每例的报文**事务 id 各不相同**，所以"每个 socket 都拿到全部"是逐条可核而不是两个计数相等；并且只统计**自己发出去的那些 id** —— 第一次在 Linux 上读出 `14 of 12`，那是场上真实 mDNS 的噪声，不是发现）。两台机器的答案：① **多播是复制的** —— macOS 两个 joined `[12, 12, 0]`、三个 `[12, 12, 12]`；Linux 同样三行是 `[8, 8, 8]`，**包括那个没 join 的**（Linux 上绑到 5353 的 socket 不管加没加入组都收得到多播）。所以旧句子里"`SO_REUSEPORT` 把进来的数据报只交给一个读者，第二个 joined socket 会偷走设备列表的一半"**在两个内核上都不成立**。② **单播整条只交给一个 socket，而交给谁不是"后绑的那个"** —— Linux 同一进程三趟都是 `[8, 0, 0]`，换两个进程（源端口不同）则分别给出 `[0, 0, 12]` 与 `[0, 8, 0]`：选谁是**按流**算的，任何一趟都不能当成规则。macOS 上 wildcard 绑定的 socket 连**对照组**都是 `0`（场上系统的 mDNSResponder 很可能就是持有者，非特权 `lsof` 点不出它），而绑到**确切目的地址**的 socket 把整条流从 wildcard 那个手里全部拿走（`[12, 0]`，逐趟稳定）。③ **结论因此不是"不用合并"**：要合并，但理由换成**单播**，而且合并的位置是**长期在场的那个 socket**，不是每次三秒钟的临时 browse。接线**之前**它是单播的一个**竞争者却从不读**，在 Linux 上还是每一份多播的**接收者而队列没人排空**。responder 落地之后（2026-10-18，§15 那一行）同一个 socket **join 且读**：答查询，并把听到的喂进设备账本 —— 于是"一直在端口上的那个 socket"恰好是"有人在读的那个"。**还没做的那半句**是设备账本那一侧：读到的**别人的**记录今天只统计不入库（`Served::absorbed` 那一个计数器），所以合并的是 socket，不是两张表。`mdns.rs::open` 那句" replies 会以单播回到我们这里"也已按测量改成**条件句**：单播只回到**某一个**绑在 5353 的 socket，只有读它的那个是我们的时候才算数 |
| `announce`（不做唯一性探测） | 参考实现在广播之前先发 3 个 `QU` 的 PTR 查询并等回话（`generate_service_query`，`_core.py:263-283`），撞名就 `NonUniqueNameException` —— 而 `advertise()` 让它冒出去，所以 Python 那一侧撞名的后果是**这台机器根本不广播** | 我们照播。代价写在模块头：同一局域网里两个相同友好名的实例会发布**同一个** instance 与 SRV 目标，发送端可能把名字解到另一台的端口上。**而且我们连那个探测包都不构造**：没有"发完再听"的探测就只是一句问话，谁也不答它，写出来就是一段长得像覆盖的死代码（§4.13「不提问的检查」的形状）。语料里那行 `probe` 因此是**参考实现的**证据，`every_packet_we_write_is_a_response_and_never_a_query` 断言我们四个构造器没有一个交回它 |
| `announce`（属性过长时） | 参考实现**整条起不来**：`ServiceInfo(...)` 打包 TXT 时对 >255 字节的属性抛 `ValueError: bytes must be in range(0, 256)`，而这一句不在 `advertise()` 那两个 `try` 里（本机实测：200 字节过、300 字节抛），于是异常一路冲出 `ChromecastProtocol.start()` —— 用户改一个长设备名，买到的是 Chromecast 接收端没了 | 本端口砍属性、不砍接收端：按字符边界截到 255 字节，WARNING 里点名字段、原长度与上限，然后照常广播。方向与 §4.8「越界夹住而不是拒绝」、`vt_level` 的"少钉一个 level 只是不明确，钉错是每一帧都没了"是同一条。**Python 那一侧今天仍然如此**，这句话不是说它修好了 —— 记在这里是因为它是一条会被下一次"照抄参考实现"重新引进来的行为 |
| `announce`（没有监听就不广播） | 参考实现里 `_advertise()` 在 `_start_tls_server()` 之后无条件跑，而 `cast_port` 绑定失败时保持默认值 —— 于是它广播一个**没人听的端口** | 本端口只在 8009 真的在听时广播（`main.rs` 里那一格是 `match cast_channel.as_ref()`）。一台出现在菜单里却投不上的设备，正是 `AGENTS.md` §4.1 第一行的症状，不是它的小版本 |
| `announce`（**立即作答，没有聚合队列**） | 参考实现把非立即答案压进一个 `U(20,120)` ms 的聚合队列（`_RESTRICTED_QUERIES` / `outgoing` 那条路），只有 `{SRV,A,AAAA,NSEC}` 那一集合立刻发；本端口的 `serve` 收到查询**当场发**，`respond::timing()` 算出的四个 `Timing` 值**没有任何产品代码读它** | 这是本轮量出来的惰性格，不是风格选择：把 `mdns.rs` 里 `unicast_request: class & CLASS_UNIQUE != 0` 换成恒假，整 crate **688 passed / 0 failed**（2026-10-18），因为 QU 位唯一的消费者就是那个没人读的 `Timing::Direct`。**代价是真实的**：同一分钟内 N 个冷启动的浏览器会把 N 份答案打到组播上，而 §4.10 在本机日志里量到的膨胀（SSDP 那一路 49% 的行是同一件事重播）就是这条队列存在的理由。补法不是"再断言一次"，是把聚合落地（按记录键合并 + 随机延迟），落地时 `delays_ms` 那批读数**仍故意不作判据**（断言毫秒数就是断言这台机器的计时器）；判据见 §16 第 23 条 |
| `relay`（等待预算） | 两个等待（响应头之前、EOF 之后管道里）**合成一个连接级参数** | Python 里它们是同一个 `GROW_WAIT_SECONDS`，因为它们是**同一条策略**："读者可以先干等多久，才轮到'还没有'这个诚实答案"。写死成两个常量之后，一个夹具里两条预算可以悄悄漂成不同值 |
| `status`（四个键用 `peek` 读） | 参考实现的文档是 `Setting.get(key, default)` 拼出来的，而**那一次读会把默认值写进表里**，下一次 `save` 就落盘（`AGENTS.md` §4.2 的第一条陷阱）。本脸对 `Macast_Protocols` / `Macast_Protocol` / `Macast_Renderer` / `Play_History` 改用 `peek`，答案逐字相同、磁盘上不多一个键 | 边界要写清，因为它**不是**"本端口再也不写设置"：`ApplicationPort`、`Https_enabled`、`Https_Port` 与网卡钉 (`Network_Interface`) 仍然走共享读接口（前两者是壳自己启动时就在用的值，后者是 `macast_core::net::network_interfaces` 内部读的，改它要一起动 SSDP/广播/中转三张脸）。判据 `the_status_read_creates_no_key_that_was_not_there` 钉的就是这条边界，并且它**断言 `Network_Interface` 确实被创建** —— 一个把"什么都没写"当成整体主张的测试，会在下一次"顺手改成 peek"时红得说不清是谁的锅 |
| `launch-param`（键的顺序） | 参考实现交回 `Setting.setting` —— 那个 dict 保的是**文件里的插入顺序**；本端口这张表是 `BTreeMap`，于是键按名字排序 | 页面的「高级设置」是一个 JSON 编辑器，本机没有任何读者按顺序读它（量过：这条路上唯一的读者还没写）。真要保序，改的是那张表而不是在这条路由上再排一次 —— 那会是第二个主人。**同一张路由携带 `Api_Token`**：这条不是偏离而是决定，写在路由的行注释里，并由 `the_face_never_answers_a_cors_question` 与 `launch_param_answers_the_whole_table…` 两条一起守（前者守"为什么可以带"，后者守"它确实带"） |
| `subscribers`（文档比参考实现多两个键） | 参考实现的 `query=subscribers` 交回 `{aggregated, children[{title, type, running, event_thread_alive, subscribed, pending_append, pending_state}]}`；本端口那份**多** `pending_remove` 与 `children[].rows` | `pending_remove`：一个卡住的回收线程与一个卡住的追加线程是**同一种故障形状**，而少了这个数字，"订阅者永远不走"就没有见证者。`rows`：想知道"哪个 SID 落在哪个回调口"的调用方不该被迫去读 `query=status` 的 `clients` 再自己 diff 一份名单。**两个读者共用一个建造者**（`subscribers_document` —— HTTP 路由与窗口命令各调一次），所以"多出来的键"只可能长在一处而不是两份文档各长各的；审计与它的 0 红教训见 §16 第 16 条 |
| `status`（`history` 现在写在设置文件里） | **偏离已经关掉**（2026-10-18）：`StateTable::add_history` / `clear_history` 之后把整份行写进 `Play_History`，启动时 `with_setting` 读回来 —— 参考实现一直是 `Setting[Play_History]`，重启之后还在，而这一版曾经住在内存里，重启即空 | 发现方式是 §23 真机一节：投完之后 `history` 仍然是 `[]`，两条红。补它的判据必须问**第二次进程**：单元层用同一路径上的第二个 `Setting` 实例（`a_history_row_survives_a_second_setting_instance`），真机层杀进程再起来读 `query=status`（§27）。读侧一律 `peek` 而不是 `get` —— `get` 的默认值副作用会把 `Play_History: []` 造进一份从来没有过它的文件里，于是"第一次运行"在磁盘上变成"用户清空过"（§4.2 第一条陷阱，`reading_the_history_leaves_the_key_absent_on_a_first_run` 钉住）。**还剩两处小偏离，都在这同一行里记账**：① 文件里读不懂的条目本端口**在读取时就丢掉并数出来**（WARNING），而参考实现只在下一次写入时过滤、`query=status` 原样回显 —— 差别是"一份坏文件在两边页面上长得不一样"，而计数让这件事不再是静默的；② 每行对象里的键按 `BTreeMap` 排（`time`、`title`、`uri`），参考实现是插入顺序 —— 与顶层表同一条记账理由，读者都按名字取值 |
| `channel`（读的方式） | Python 在**裸 socket** 上阻塞读、读期间不持任何锁；本端口**每条连接一把锁**，所以读改成 `READ_POLL`（200 ms）轮询，**锁在两批字节之间放开** | 这是"参考实现做不到"的那一类，不是风格：rustls 的一次 `read` 会动会话状态，必须串行化，而把锁跨在一次无限阻塞读上会让**同一根 socket 上的所有推送饿死** —— `_watch_playback` 与状态变化都排在那把锁后面，发送端 LOAD 之后只拿到那一次请求驱动的答复、此后一句话都没有，直到它自己断开（2026-10-18 接进壳之后由 §21 量出，见 §16 第 11 条）。Python 那边根本没有这一层，所以这一格**不可能**由"照抄它的写法"得到正确答案 |
| `macast-mpv`（`set_observe` 之后才交出去的默认音量） | **不是偏离，是一格量到的同形缺陷**：链接一赋值就可写，而八条 `observe_property` 与那条**默认音量**是在这之后才写的 —— 落在同一个几毫秒窗口里的 `SET_PARAMETER` 会被 100 盖掉 | 2026-10-18 实测：全新实例的第一个会话，四条同形状跑（`PLAY` 之后 0 毫秒就发音量）**三次丢、一次没丢**（这正是"窗口"而不是"规则"的样子），同一实例后来的会话 4/4 落地。参考实现同形：`macast_renderer/mpv.py:404-414` 先 `self.ipc_sock = …` 再 `self.set_observe()`，`send_command` 每次单独拿 `command_lock` —— 所以这里**不单边修**（修它就是把自己的行为挪出参考实现，那需要自己的判据）。`verify_rust_receiver.sh` §26 因此隔 0.4 秒再发动作（真手机音量滑块不会在按播放的同一毫秒动），并把判据换成两条持久的：我们那句"两个数字都说"的 INFO + 从 mpv **读回来**的那个值。要动这一格的人先读 `set_observe` 的 docstring，读数就写在那里 |
| `airplay`（谁写播放状态） | AirPlay 的投屏**不写 DLNA 状态表、不写播放列表、不写历史** | `protocol_airplay.py:251-280` 调的是 `self.renderer.…`，那里 renderer 是**渲染器实例本身**而不是 `PlaybackGuard`。移植照抄这个形状：壳给的是 `ShellRenderer` 上的 `airplay::RendererCallback`，五个方法都是直通。后果要说明白：用手机投完一段之后，设置页那张卡继续显示**上一次 DLNA/Cast 交接**的内容 —— **这不是 AirPlay 坏了**，是把主人留在发送端该在的那一层；谁要在 Rust 侧"改进"成也写状态，要先改的是 §15 与 `AGENTS.md` §4.2 那句"只有一个主人"的归属，而不是悄悄加一次 `set_state` |
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

### 12.1b 名字就是目的地，所以目的地是一处决定

`Logger::named` 按名字记忆、`claim()` 第一个赢家定死文件，于是"这句话写在哪个文件里"
由**谁先拿到那个名字**决定 —— 这不是一句风格提醒，是被两个真 bug 换来的：

* **产品侧**：`main.rs` 当年把播放器的 `MPV` logger 交给了 DLNA 状态表，所以
  `setState: …` 与整段播放历史的故事写进 `logs/MPV.log`，而 `protocol.py:30` 用的是
  `getLogger("Protocol")`、`logsplit` 给它 `logs/Protocol.log`。修法是表的构造点自己
  claim 一个名字（`host::PROTOCOL_LOG`），而**判据按文件名 grep**（§27 读
  `logs/Protocol.log`）：一个"在所有日志里找那句话"的检查会对着错误的目的地照样绿。
* **测试侧**：`Logger::named("Macast")` 被任何一个用例 `claim()` 之后，全进程读到的
  都是那个文件 —— 于是"这条句子没写在 macast.log"这类**否定断言**的成败取决于
  **哪个测试先跑**。工具是分名的：`macast-cast` 的 e9 用 `Discovery-Echo` /
  `Discovery-Heard` / `Discovery-OtherType` / `Discovery-Capped` 四个名字，因为它五半
  判据里有两半问的是"这里应当没有"（环回不算、上限之外不再写）。反过来，只问"这句话在不在"的用例可以共用产品自己的那个名字 ——
  `src-tauri` 这一轮的几条历史用例就是这样（都 claim `Protocol`，都问正向句子），
  而**这不是一个可以照抄的决定**：往同一段里加一条否定断言的那一刻，就该分名。
  这是 §4.2「并行跑的用例共享夹具路径，单跑绿、整组红」在日志上的形状。

推论，写给下一个加句子的人：**先决定这句话属于哪个模块，再决定它说什么**。
说对了内容、写错了文件，等价于没写 —— 用户 `grep` 的那一下不会替他换文件。

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

### 12.3 `log` 门面：一条曾经写到空气里的路

`airplay.rs` 与 `relay.rs` 用的是 `log` crate 的自由宏（`info!` / `warn!` / `debug!`），
而不是本端口自己的 `Logger`。这本身不是错 —— 错在**全仓没有任何一处安装过它**：
`impl log::Log`、`log::set_logger`、`log::set_max_level` 三个 grep 在 2026-10-18 之前
**全都是 0 行**，而 `log` 的默认级别过滤是 `Off`。于是那 18 条句子从来没进过任何文件，
包括三句用户真的会需要的：

| 句子 | 它本来要回答的问题 |
|---|---|
| `AirPlay port 7000 already in use (macOS AirPlay Receiver?), using N instead` | "为什么 iPhone 找不到这台 Mac" —— macOS 自带接收端占了 7000 是**本机最常见的一种占法** |
| `AirPlay ANNOUNCE carried no Content-Location; refusing rather than pretending to load` | "投了却没画面"：发送端没给地址，我们拒了，而拒了这件事只存在于那个 400 里 |
| `relay: swept N abandoned temp file(s)` | "配置目录为什么越来越大" |

修法选的是**改成 `Logger::named`**，不是装一个全局 sink。理由不是省事：装 sink 会把
**第三方** crate 的 `log` 输出也接进我们的文件，而这条通路的音量从来不由我们决定
（`AGENTS.md` §4.10 那 49% 就是这么来的）；`Logger` 的名字由参考实现自己点名 ——
`protocol_airplay.py:29` 是 `logging.getLogger("AirPlay")`、`media_relay.py:72` 是
`"MediaRelay"`，两边都对得上，且**都不认领**（claim）自己的模块文件，所以记录照旧落
`macast.log`，与 Python 同一处。

守这条的是两层用例，而**承重的是后面那一层**：

1. `no_crate_even_declares_the_facade` 扫五份 `Cargo.toml`（断言真的读到了 **5** 份，
   "少读几份也算过"正是它要防的形状），要求没有任何一份声明 `log`。`macast-cast`
   那一行本轮**删掉了** —— 于是 `use log::` 从此是**编译错误**而不是测试失败：
   **结构比用例早一步拦住，这一格才是承重的那一格。**
2. `nothing_in_the_workspace_borrows_the_log_facade` 再用纯函数 `log_facade_uses` 扫五个
   crate 的每个 `.rs`（本轮量到 **62 个文件**：core 10、mpv 9、upnp 10、cast 31、
   src-tauri 2；地板写 20），四条合成对照（五种宏各一 + 那条 import），两条必须**不**报的
   （注释里提到宏名的散文、以及我们自己的 `logger().warning(`）。
   **(1) 成立时这一条永远不会响，这句话写在用例旁边而不是藏起来**：它在等的那一天是
   "有人重新声明那个依赖并给出理由"，那时它是唯一还在读源码的一层。

牙都量过：把 `relay.rs` 的一行改回 `log::warn!(…)` ⇒ (2) 红 **1** 条并点名
`["macast-cast/relay.rs"]`；把 `macast-cast/Cargo.toml` 那一行加回去 ⇒ (1) 红 **1** 条。
**扫描器自己的 needle 一律拼接而成** —— 本轮真的栽过一次：合成对照里写死的 `warn!(`
把装着它的那个文件自己报成了违规，于是干净树红。同 §13 那条 CORS 扫描的教训。


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
| mDNS 广播报文 | **每个服务 5 行**（announce ×2、goodbye ×2、probe ×1），两份语料 | `macast-cast/tests/golden/mdns_announce.hex`（`--fixture --dump-hex`，`_googlecast._tcp`）与 `mdns_airplay.hex`（`--protocol airplay --fixture --dump-hex`，`_airplay._tcp`，2026-10-18 冻的，**还没有消费者**：它是下一轮那个 builder 的输入，见 §16 第 28 条）。两份都是跑两遍逐字节一致才算冻住 |
| mDNS 名字规则 | **26 例**（友好名 → instance → SRV 目标）| `macast-cast/tests/golden/mdns_names.jsonl`，同一支探针的 `--dump-names` |
| mDNS **答复**规则 | **17 例**（一道查询形状一行：哪些记录、哪个段、TTL、flush、目的、抑制结果）| `macast-cast/tests/golden/mdns_responder.jsonl`，由 `scripts/mdns_responder_probe.py --dump-corpus` 产出。**冻解码表不冻字节**，理由见 §14.6 |

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
- **段 24 先杀掉第一段的那个实例，这不是收尾是判据的一部分**：8009 与 8008 是发送端钉死的两个口，第一台还活着的时候，第二台"没起来"和"起来了但抢不到口"在外部**长得一模一样** —— 而 §20 那条"8008 不回落"的决定之所以成立，正是为了不让这两种状态混起来。所以这一段的顺序是：确认监听者就是我们自己那个 pid → 杀掉 → 等端口真的空了 → 再起第二台。第二台的临时目录写进 `.socket`，`cleanup` 按套接字路径收掉它的 mpv —— `AGENTS.md` §4.2「被反复重启的 Macast 会攒播放器」那一格，在这份脚本里同样要还。
  这一段的**浏览必须排在第二台启动之前**（2026-10-18 同一轮改的）：广播的节奏是"3 包 / 225 ms，然后 60 秒静默"，而它原来是在实例起来之后再听 8 秒 —— 一台**确实在广播**的机器因此可以被答成"线上什么都没有"。这不是推测：把证书那道门删掉当变异体时这一段红了 5 条，而**这一条照旧绿**。挪到启动之前、窗口 30 秒之后，同一支变异体红 6 条，多的那条点名 `Macast-Gate-Off._googlecast._tcp.local.`。同一格还连带抓出一个**没有读者拥有的设置键**：两段 seed JSON 都写 `FriendlyName`，而真名是 `DLNA_FriendlyName`（`setting.rs:94`），于是被门住的那台用回退默认名广播，按名字匹配的这一问自然看不见它 —— 详见 §16 第 12 条 ②。
- **段 23 是"一份文档，三个读者"的检查**（2026-10-18 交付）：`query=status` 与 `query=interfaces` 的每一个键都由**别人**回答一遍 —— 播放状态对 SOAP 的 `GetPositionInfo`、发布的地址对 §22 从线上读回来的 A 记录集合、订阅者对我们自己刚发的 `SUBSCRIBE` 拿到的 SID、`server.platform` 对 `.venv` 那个解释器的 `sys.platform`。这一段的形状是"**先设事实再问文档**"：状态表与订阅表在单元层是桩（`TestStore` / `FakeSubscription`），只有这里走的是真 mpv、真 GENA 套接字。**它交付的第一条不是绿，是一条红**：投完之后 `history` 仍然是 `[]` —— 参考实现的历史在设置文件里、这一版在内存里，而打印设置文件会答出"这台机器什么都没投过"。那一格变成 §8 的一行偏离账与 §16 的一条待决，而不是被改成"断言它是空的"。同一段后来把 `query=subscribers` 也接了进来（HTTP 路由与窗口命令**共用一个** `subscribers_document`，§8 记着那份文档比参考实现多的两个键）：检查的时机是"我们自己的 SID 还活着的那一刻"，于是 `aggregated` 必须含它、`pending_append` 必须**等**它归零（订阅落表排在事件线程那一步，判据坐在别人的账本上 —— §4.2 那条规矩在这份脚本里的第三次出现）、`rows` 与 `aggregated` 必须是同一组 SID、`UNSUBSCRIBE` 之后两份文档必须一起空。这一段也因此交回一条**脚本自己**的红：subscribers 那串检查跑完之后 `$API_BODY` 装的是 subscribers 那份文档，后面 `history` 的两条断言于是读不到自己的键 —— 产品没错，见 §16 第 17 条。
  另一条只有仪器变坏才能学到的：`json_join` 交回的是 `、`-连接的串，而第一版用 `tr '、' '/'` 去比 `server.ip` 的 `/`-连接 —— **`、` 是三个字节，`tr` 是逐字节的**。只有一条地址时没有分隔符可拆，所以它绿；场上一多地址（M9 那支变异体把宽集合喂进来）它红了**两次**，其中一次根本不是产品的错。判据换成"在同一份解析里比两个列表"，而这条教训与 §4.2「夹具脚本里的那对括号是承重的」同族：**测试自己的实现也是实现**，而它只在机器恰好给出多个值时才现形。
  再后来同一段又接上日志面板那一族（`log` / `log-modules` / `log-download` / `POST clear-log`，文档由 `macast-core` 独自持有）：判据形状是"把这一次真跑出来的日志读回面板"——`log-modules` 每行的 `file` 与 `name` 必须自洽（不许从名字合成）、`Discovery.log` 里必须是 **announcer 自己写的那句**、不带 `module=` 的那次读的是 macast.log、`?tail=` 问的是"截断了自己说没说"而不是"历史在不在"，最后 POST 清空之后面板的下一次读必须是一个空盒子而不是报错。
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


### 14.6 responder 的规则表（取证产出，实现是下一轮）

参考实现**会**回答查询，而我们的端口不会。这一节的每一条都不是从 `zeroconf` 源码读的，
是 `scripts/mdns_responder_probe.py` 量出来的：起一个只绑 `127.0.0.1` 的真 `Zeroconf`，
注册**产品自己**会注册的那个服务（属性表来自 `_advertise` 换掉 advertiser，与广播语料同一个
主人），把查询真的发到 lo0 上的 `224.0.0.251:5353`，再把 `zeroconf._core.async_send_with_transport`
 recorder 截住的**答复字节**解码。一个字节都没上局域网（查询钉在 lo0 / TTL 1，答复被记录不发送）。
语料冻在 **`macast-cast/tests/golden/mdns_responder.jsonl`**（17 行，一行一个查询形状），
对拍它的是 `macast-cast/src/respond.rs`（判决层 + 装配层，22 条用例）—— 还差的只有 socket
接线，见 §15 那一行。装配层的判据分两条读者：一条独立遍历（自己走记录骨架，读得到 NSEC 的
位置与字节）+ 一条 `mdns::parse_message`（证明发给发送端的字节我们自己的浏览器读得懂）。

冻的是**解码后的记录表**而不是原始字节，这是刻意的，理由与广播语料不同要写清：广播那侧的
不确定性只有 `A/NSEC` 一对（于是冻两行就够），而**答复里的 additionals 是 set 序**，本机两次跑
`ptr` 这一道题就分别交出 `TXT/SRV/A/NSEC` 与 `SRV/TXT/A/NSEC`，同一次运行里 `ptr-known-half` 又是
`A/TXT/SRV/NSEC`。所以比对的是「哪一条记录、在哪个段、TTL 多少、cache-flush 有没有」，不比对字节；
冻字节的代价是每一次跑都会红一条与产品无关的用例（§4.3 那对 `A/NSEC` 已经教过一次）。**延迟不是判据**：`delays_ms` 在语料里记着，因为它是量到的，
但 Rust 侧不许断言它——那是这台机器的计时器，不是协议事实。

| 问的是 | 答什么（an=答案段，ar=附带段） |
|---|---|
| `PTR _googlecast._tcp.local.` | `an PTR/4500`（无 flush）+ `ar SRV/120 TXT/4500 A/120 NSEC/120`（都带 flush） |
| `SRV <instance>` | `an SRV/120` + `ar A/120 NSEC/120` |
| `A <server>.local` | `an A/120` + `ar NSEC/120` |
| `TXT <instance>` | **只有** `an TXT/4500`，没有任何附带段 |
| `ANY <instance>` | `an TXT/4500 an SRV/120` + `ar A/120 NSEC/120`（PTR 不在答案里） |
| `AAAA <server>.local` | `an NSEC/120` —— 这是 mDNS 的"这个名字存在，但没有这个类型"，**不是**沉默 |
| `PTR _services._dns-sd._udp.local.` | `an PTR/4500` 指向**类型本身**，不是实例 |
| `PTR *._dns-sd._udp.local.`（RFC 6763 的 DQE 星形写法） | **不答**：参考实现只认识长名字那一个 |
| `PTR _airplay._tcp.local.`（我们不跑的类型） | **不答**，也不报错 |

五条判决规则，全部在语料里有自己的那一行：

* **已知应答抑制按记录，不按问题，且比较是严格的 `>`**。判据是
  `other.ttl > record.ttl / 2`（`_dns.py:609-615`），而语料里三行把它夹住了：known=4500 ⇒ 沉默、
  known=2250（正好一半）⇒ **仍然答**、known=100 ⇒ 答。谁把判据写成 `>=`，除了中间那一行之外
  没有别的可见后果 —— 所以那一行是这个 Part 存在的理由。
* **匹配是"记录相等"，而 PTR 的相等包含 alias**：喂一条名字相同、指向 `Other-Box` 的 known PTR ⇒
  答。这一行同时证明 `known` 段没被当成"同名即算"。
* **QU 位在读作"单播请求"，但答案仍然走多播**，除非这个记录最近四分之一 TTL 内多播过
  （`add_qu_question_response`）；本机那一次就是 `+0.3 ms → 224.0.0.251:5353`。
* **源端口不是 5353 ⇒ 单播回给询问者，并且照样多播一份**（`ptr-ephemeral-src` 那一行抓到**两个**
  目的：`127.0.0.1:55759` 在 0.3 ms、组播在 110.4 ms）。这是参考实现自己的注释：
  「We always multicast as well … as long as we haven't done it recently」。

* **抑制只管"这道题挣来的那条答案"，不管搭车的记录**：`ptr-known-srv` 那一行问的是"asker 已经
  知道 SRV、问的是 PTR"，参考实现把五条**照发**。这一行是补出来的 —— 一支"把抑制也应用到附带段"
  的变异体在第一版 16 行语料上 **0 红**，也就是说这条规则此前没有任何见证者；补行之后同一支变异体
  红 **2** 条。**0 红不是"覆盖够"，是"这一格没人问过"**（§13.4 那条纪律第三次在 Rust 侧现形），
  而这一次闭洞的方式是**去问参考实现**，不是去放宽断言。

分桶与延迟（记着，不作为判据）：`{SRV, A, AAAA, NSEC}` 是"立即"类型（`_RESPOND_IMMEDIATE_TYPES`），
其余进聚合队列，延迟 `U(20, 120)` ms、窗口 500 ms（刚多播过的那一支换成 200 ms + 1 s 保护）。
语料 17 行里 15 个延迟读数的跨度是 **0.2–115.9 ms**，与这个模型一致（那两个数是本轮
从 `mdns_responder.jsonl` 重算的，不是抄上一版的 —— 上一版写的是「16 行 0.3–110.4 ms」，
那是补入 `ptr-known-srv` 之前的表）。

**仪器自己犯过的两个错，都长得像产品规则**（§4.2 那一族，这次是 zeroconf 的 API 记忆）：
① `add_answer_at_time(record, now)` 的第二个参数是**"按哪个时钟算剩余 TTL"**，不是"asker 拿到它的
时刻"——传一个过去的时间戳会把写出去的 TTL **变大**（本机量到 `ttl=9000` 配一条 4500 的记录），
于是三条抑制行全部沉默，读起来像"参考实现比源码更激进"。所以记录的"旧"要用**它自己的 ttl** 表达。
② `ServiceInfo.dns_pointer()` 返回的是**缓存对象**，往它上面写 `ttl` 就是改注册着的那个服务：
第一版因此把 responder 自己的 PTR 改成了 100 秒，接下来三条抑制行答的全是这次污染，而第四行的
答案里直接印出 `an PTR ttl=100`。现在 known 段一律走 `copy.deepcopy`，并且**每个用例建完 known
之后都问一遍"注册着的那三条记录的 TTL 还是不是 4500/120/4500"**（`assert_service_untouched`）——
这条断言就是这一格不会再次静默发生的地方。

**同一天又犯了两个，这次抄的不是库 API 而是线格式本身**（§4.2「假命令行工具的输出必须是真工具
在这台机器上的逐字输出」那一族的 Rust 侧第四、第五例）：③ 手搓的那条"远端发来的查询"把第一个
标签的**长度字节**漏了（`_googlecast` 直接写字面文字，正确形状是 `\x0b_googlecast`），于是线上
是 39 字节而不是 40 —— responder 对它的沉默是**正确行为**，而我第一版把它读成"这台 Mac 收不到
局域网多播"，并据此写下了一条关于系统的结论。④ 判"有没有答复"的过滤器找的是
`_googlecast._tcp.local.`（带点），而 mDNS 的名字在线上是**长度前缀标签**、点永远不出现 ——
于是**任何**答复都会读成 0，连参照实现那一侧的对照组也是 0。**两个错叠在一起的样子是"环境挡住了
我们"，而真相是仪器看不见任何字节。**修好之后才有的读数：那台 Linux 机器确实收到了我们的
296 字节 `an=1 ar=4` 答复（见 §15 的 responder 行与 §16 第 22 条）。
⑤ 由此固化了一条新用例 `announce_tests::e10`：把**外写的**查询字节（事务 id 0x1234）喂给
`serve`，要求答，并断言它与我们自己的 `mdns::query` 交回**逐字节相同的 40 字节** —— 在此之前
每一条查询形状的用例用的都是自己写的包，那是自洽，不是兼容。

**两句让"问得出来"的日志**（本轮加，因为分辨一次沉默花了两个机器和一小时）：
`bind_engine` 的 `mDNS engine on 5353 joined 1 of 1 interface(s) [192.168.1.5]; …` 把**加入的是
哪些接口**写进句子（只数 N 的话，"join 在错的口上"与"没 join"读起来一样），`run` 的
`mDNS engine heard query 1 of 8 on 5353 (40 bytes, asking Ptr _googlecast._tcp.local.) from 192.168.1.68:5353`
把**送达**与**判决**分开：它只在**查询**上响，绝不在第一个数据报上响 —— 多播回环是默认
开着的（本模块第一条偏离就是因为它），我们的广播一毫秒后就会回到自己手里，用"第一个数据报"当
判据会让一台什么都听不到的机器也报喜。**这一格的形状在下一轮被重写过**（第 26 条）：
INFO 那句只报**问到我们头上的**查询，别人的另有一条 DEBUG 与一条自己的上限，两句都把
**问的是什么**写进文本；上限从三条抬到八条，因为一趟脚本里 §22、§24、§25 有三个浏览器在问。
`e9` 钉五半：环回不响、两条各响一次、两句都带主语、别人的查询不占 INFO 预算但记进 DEBUG、
十条只响八条。
**这一格里两支变异体第一次交出的是 0 红，而坏的都是用例本身**：① 那句否定断言的 needle 停在旧文案
`heard its first query` 上 —— 产品把文案改成 `heard query 1 of 3` 之后它永远成立，**改文案等于把一条
断言悄悄变成不提问**；② 计数断言写的是 `== crate::announce::HEARD_SENTENCES`，那是**同源等式**，常量
改成 4 它照样绿（§13.4 那条「对任何上限值都成立」第三次现形，而这一次是我一小时前写的用例）。
重新对上 needle、把期望改成字面 3 之后三支各红 **1** 条（永不响 / 任何数据报都响 / 上限挪到 4）；同一族在读过第 26 条之后又量了七支，各红 1 条。
另加一问 —— 每行打印的 `of N` 必须等于那个常量，且这一问排在计数**之后**，这样"数错了"不会被它盖掉。
判据：**先让变异体真的能红，再宣称这句话被测过**。


---

## 15. 刻意不做 / 尚未做

一份没有这一节的移植文档，读者会把"还没做"读成"已经安全"。
以下逐条来自各 crate 自己的注释，不是推测：

| 事项 | 状态 | 出处 |
|---|---|---|
| setup 面（HTTP/8008 + HTTPS/8443：`/setup/eureka_info` 裸 UUID、`params=` 过滤、`/setup/icon.png`） | **已交付**（2026-10-18，`macast-cast/src/setup.rs` 655 行 + 壳在 `main.rs` 里起它） | 三处**刻意偏离**写在模块头，不在这里复述：`Server:` 说我们自己的名字、非 `GET` 回 501 且无正文、以及 **8008 不回落**（参考实现走 20 个口，而发送端把 8008 钉死在 `dial.py:25` —— 回落只买到一个没人敲的口，还会在通道没起来时把 8009 从它嘴里抢走）。判据：`verify_rust_receiver.sh` §20（**26 条**，含一次真 TLS 握手与"8008/8443 交回同一串字节"）+ `setup_tests` 的 30 条 |
| Cast 接收端接进壳（`channel.rs` 的 8009 TLS 会话、`RECEIVER_STATUS` 回报、`_watch_playback` 那一格） | **已交付**（2026-10-18）：`main.rs` 走 `cert::ensure_in_setting_dir` → `CastChannel::new` → `host.set_cast` → `channel.start(pair)`，且**排在 setup 脸之前**（8009 才是发送端要连的那个口，两句话都在 `main.rs` 的注释里）；`host.rs` 把 `Pause`/`Play`/`Stop`/`Eof`/`TransportError` 五次 `set_state` 扇进这根通道，`cast_uri` 交还播放权时调 `release_playback()` | 判据是同一句 grep：`grep -rn "CastChannel" src-tauri/src/` 从 **0 行** 变成 **6 行**（`host.rs` 4、`main.rs` 2）。**接进来才现形的那一格：读不许跨阻塞持锁** —— 见 §16 第 10 条与 `AGENTS.md` §4.14 的 2026-10-18 段。见证 = 真机 §21 的 **5 条**（一支按 VLC 状态机写的发送端打进来并读回 `PLAYING`）+ `channel_tests` 的 **40 条**。这一行标题里的"播放看门狗"指的是参考实现的 `_watch_playback`（`protocol_cast.py:1056`），**不是**重投看门狗：接收端那侧的参考实现里没有重投，那是 `cast_local_file` 发送侧的东西 |
| `_googlecast._tcp` 的**广播**（我们自己接收端的 PTR/SRV/TXT/NSEC/A，unsolicited） | **已交付**（2026-10-18，`macast-cast/src/announce.rs` + 壳在 `main.rs` 里随 8009 起停） | 判据不是"我们自己读得懂"：`verify_rust_receiver.sh` §22 让 `.venv` 里那套 **zeroconf**（参考实现自己用的栈）冷启动去解析，**42.8 秒**后交回完整设备（那一趟整套 **183 条 / 0 失败 / EXIT=0**）—— 而第一串广播那时早就发完了，所以这条同时是 **60 秒 keepalive 的真机见证**。字节层面另有两条冻结语料（`macast-cast/tests/golden/mdns_announce.hex`、`mdns_names.jsonl`，由 `scripts/mdns_announce_probe.py` 产出），其中 announce 有**两行**，因为参考实现那一对 `A/NSEC` 的先后随 `PYTHONHASHSEED` 变（§8 的偏离表与探针注释都写了这一条）。**剩下的不是这一行**：下一行 |
| mDNS **responder**（回答别人的查询） | **已接线**（2026-10-18：判决层 + 装配层 + `announce::serve` 挂在 announcer 那个 join 且读的 socket 上）；**跨机答复已量到，app 进程那一格仍无见证** | 读代码的人要把这两句分开：① 一个**只跑产品那批函数**的长命进程（`bind_engine` + `serve`）收到 192.168.1.68 发来的真 `PTR _googlecast._tcp` 并答了，**那台机器看见了 296 字节 an=1 ar=4 的回包** —— 所以"我们的答复能不能跨过一台机器"这一问今天有答案了；② `macast-tauri` 那个 app 进程**今天仍然没有收到**冷浏览器那三条 40 字节 `PTR _googlecast._tcp` —— 而这一句今天是重新量出来的，因为它上一版是错的：那条 hearing 句子当时按**任何**查询计数，而本机自己的栈每 0.8 秒就问一次 `Ptr _irohv1._udp.local.`，上限三条在四秒内烧光，于是 §25 到达时日志是空的，我们把它写成了"这个二进制收不到 LAN 多播"。今天改成**INFO 只报问我们的、DEBUG 另报问别人的**（各自一条上限），再重测：同一台机器、一个 python 浏览器 + 一个加入同组的 python 监听器看得见那三条查询，app 进程两句都没有；把监听器整个去掉、场上只剩 app 与浏览器，app 仍然一句没有。**同一天早些时候它确实收到过**`_irohv1` 那一族查询（source 同样是 192.168.1.5:5353）。所以"这个进程的入站多播被系统挡了"这句也不成立 —— 成立的是"这个 socket 收不到这一条"，而**原因未定，不在这里挑一个填**（§16 第 22 条记仪器错、上限被淹死那一格与全部读数）。回环端到端另有 e8，外写的查询形状另有 e10（含"与我们自己的 writer 逐字节相同"那一问）；`verify_rust_receiver.sh` §25 现在按 `heard` / `answered` 两句判决分三种答案说话（有 hearing 无 answering ⇒ 红；两句都没有 ⇒ skip 计败）。今天用户能读到的话仍然要说清：后果是可行动的那句：**只听的发送端**（libvlc 的 microdns，`discovery.py:230-237` 自己写的）第一串广播就看得见我们；**问了才看的**（Cast app、`dns-sd -B`、冷缓存的浏览器）要等下一次重播，也就是最多 60 秒。判据与规则都在 §14.6。(a) 已经做完：`Writer` 现在有两个段计数器，`record_additional` 写 AR，
`respond::datagram` 按 `Section` 分派，广播那五条记录的字节**一个都没变**（19 条 golden 对拍用例
照旧绿就是这一句的见证）。(b) **也已做完**（2026-10-18 接线）：`announce.rs` 那个长期在场的 socket 
现在 join 且读，读到的查询交 `respond::respond` + `respond::datagram` 答出去。**仍然欠着的那半句**是
"读到的**别人的**记录喂设备账本"：今天非查询的数据报只进 `Served::absorbed` 一个计数器，所以 
§8/§16 那条"合并"结论只合并了 socket，没有合并两张表。
17 例答复规则冻在 `macast-cast/tests/golden/mdns_responder.jsonl`，包括抑制的严格 `>` 边界、PTR 相等含 alias、QU 仍走多播、单播源额外多播一份、`AAAA` 答 NSEC 而不是沉默、`*._dns-sd._udp` 那种星形写法参考实现根本不答。形状那半句**按本机与那台 Linux 的读数改过两轮**：原写「`SO_REUSEPORT` 只交给一个读者，所以 responder 必须与 `announce` 的发送 socket 合成一个」—— 多播在两台机器上都是**复制**，那句不成立；但**单播确实整条只交给一个 socket 且随流变**，所以"合并"这个结论**留着**，理由与位置都换了：落在**长期在场的那个 socket** 上（`announce` 的那个），它 join 且读。见 §8 的 `announce` 行与 §16 第 13 条 |
| SSDP **客户端**（M-SEARCH 之后拉描述、建设备表） | **库层已完成**（issue #27，见 §14.5）；**2026-10-18 接进壳** | `macast-cast/src/targets.rs` + `lib.rs`（`pub mod targets;`）。同一判据本轮重量：`grep -rn "targets::" src-tauri/src/` 从 **0 行** 变成 **2 行**（`host.rs` 的 `pub targets: Arc<targets::Table>` 与 `main.rs` 的 `Table::new(setting)`）|
| 渲染器插件的 `target_push` 面（设备下拉真正投到远端） | **已交付这一半**（2026-10-18）：壳把 `cast-targets` 从拒绝文档改成读这张表，`cast-resolved` 的设备臂做 `target_by_id` → `push_url_to` | 之前该面答的是**参考实现的完整拒绝文档**（提交 `d9cf9de`）。**剩下的不是接线**：本局域网里没有可投的真设备（§16 第 1 条），所以设备臂的**成功**那一路在真机上仍无见证者，只有 `push_tests` 的 46 条打在自家假设备上；`CastSender` 的 pause/seek/音量与 `text_tracks` 是 §15 下面那行的裁剪决定 |
| 管理面的 `query=status` 与 `query=interfaces`（设置页心跳文档） | **已交付**（2026-10-18，`macast-upnp/src/http.rs` 一张文档两个读者；`Face` 拿到 `SharedStore`，`Subscription` 多一个 `rows()` 读门） | 判据：`http_tests` 12 条（文档形状、16 个状态名、六种客户端键、`current` 与 `interface` 两种拼法、四键 peek 边界、`py_truthy` 表）+ 真机 §23。**同一轮后面又把 `query=subscribers` 接了进来**（GET 表 7→8 行、`Subscription::diagnostics()` 一扇门、`subscribers_document` 一个建造者两个读者 —— 窗口那条命令过去自己 `json!` 一份，现在与 HTTP 那份出自同一函数），`http_tests` 再加 5 条。**两条偏离记在 §8**（四个键用 `peek`；`history` 那一条**已经关掉** —— 2026-10-18 起它写进设置文件，判据在 §27）。**M9 那一支是本分支第一条"单元 0 红 / 真机红"的读数**：把宽地址集合喂给这一张文档，328 条单元用例全绿，只有 §23 的"页面与线上必须给同一个集合"红了 —— 因为一次 `network_facts()` 同时喂两个键，单元层里那两个键永远互等。见 §16 第 15 条 |
| `link` 的 **Windows 列** | **未在本机行使** | §4.2 的原文边界句 |
| AirPlay RTSP 接收端接进壳（`airplay.rs` 的 7000/20 口回落、十一种方法、会话与音量） | **已交付**（2026-10-18）：`main.rs` 在**同一道 `Macast_Protocols` 门**之后起 `AirPlayReceiver::new(setting.friendly_name())` → `start()` → 线程 `AIRPLAY_RSTP`，渲染面是 `ShellRenderer` 实现的 `airplay::RendererCallback`（五个直通，没有第二个主人）；标题走 `resolve::title_for_uri` 那一个主人，`serve` 的 accept 改非阻塞轮询 | 判据：`verify_rust_receiver.sh` §26（**20 条**）—— 第二台实例只开 DLNA+AirPlay，真 RTSP 字节打进去，见证是 mpv 自己写的 `file-loaded`、窗口上那个名字、与 mpv 读回来的音量；另有两个新用例把 `SET_PARAMETER` 那两句（"两个数字都说" 与 "读不懂也要说"）钉在单元层。**这一行仍然欠的那半句**：`_airplay._tcp` 的广播没接 —— 所以"iPhone 的屏幕镜像/音频列表里看得到这台 Mac"今天说不出口，只有**知道端口**的发送端（手动填地址、将来的测试）走得进来。**取证已完成**（同日，`macast-cast/tests/golden/mdns_airplay.hex`）：形状与 Chromecast 那一串逐条同构（PTR/SRV/TXT/NSEC/A、TTL 4500/120/4500/120/120、除 PTR 外全带 cache-flush、`flags=0x8400`、goodbye 是同样五条记录把 TTL 写成 0），差别全在 TXT 那九条与端口，读法与那两条"为什么不能只冻一行"的理由见 §16 第 28 条。还有一格记在 §8：新实例的第一个会话若在 `set_observe` 写完默认音量之前发 `SET_PARAMETER`，音量会被 100 盖掉（参考实现同形） |
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
12. **`Macast_Protocols` 现在是 Chromecast 三张脸的门；DLNA 那一半仍然不是**
    （2026-10-18 关掉一半，剩下一半是有意的）：
    `host::faces_enabled` 一次读取（AirPlay 接进来之后它交回 `Faces { cast, airplay }`，门从三张脸变四张），门住 8009 接收端、8008/8443 setup 脸与 `_googlecast._tcp`
    广播 —— 三者是一个协议，分开门会得到"身份答得出来、连接握不上"的那台设备。
    证书生成也在门后（它往设置目录里写两个文件；一个用户关掉的协议不该留下密钥材料）。
    判据两层：`host::tests` 问**读法**（存的字符串要当成一条记录、legacy 键是追加不是优先、
    不认识的标题跳过并说一句、空表 = 全开、`ChromecastProtocol` 这种紧凑拼写在**两边都**解不出——
    参考实现的 `canonical_title` 只去 `" protocol"` 带空格那一截），
    §24 问**关上的门**（第二个实例只开 DLNA：8009/8008 必须拒连、日志必须说出是谁关的、
    浏览器必须看不见它、证书文件必须不存在）。
    **剩下那一半不是遗漏**：DLNA 的 HTTP 脸在这份壳里同时承载设置页与管理 API，
    把它一起门住 = 用户关掉 DLNA 就没有页面可改回来。参考实现里 DLNA 关闭时页面还在不在，
    本机**没有量过**（`build_protocol_group` 只在分组为空时兜底加回 DLNA，正好说明它考虑过
    "一个都不剩"的情形，但没考虑"只剩 Chromecast"时页面归属谁）。那一格要先量再改，
    所以它记在这里而不是顺手做掉。§20/§21/§22 的真机读数也因此都来自"全开"的那台。
    **§24 的两条仪器错，是变异体把它们逼出来的**（同日，`host.rs` 9 支 + `main.rs` 2 支）：
    ① 广播那一问原来是"实例起来之后再浏览 8 秒"，而发送端的节奏是 3 包 / 225 ms、之后
    `REANNOUNCE` 静默 60 秒 —— 删掉证书的门当变异体时（gated 实例真的建了 announcer、真的绑了
    8009、日志里真的写了 `mDNS: announcing`），§24 红 5 条而**这一条照旧绿**：它答的是"我这个窗口
    没赶上突发"，不是"这台没广播"。修法是浏览**排在实例启动之前**，窗口 30 秒横跨初始突发
    （§22 的 75 秒窗口负责 keepalive 那一次，两个窗口问的是两段）。重测同一支变异体：红 **6** 条，
    多出的那条点名 `Macast-Gate-Off._googlecast._tcp.local.`。清洁跑不变，仍 **190 条 / 0 失败**。
    ② 两段 seed JSON 写的键是 `FriendlyName`，而真正的键名是 `DLNA_FriendlyName`
    （`macast-core/src/setting.rs:94`）—— 一个没有读者拥有的键，于是①里那台广播的是回退默认名，
    §24 按名字匹配自然看不见。今天 §22 印的是 `Macast-Rust-Verify`，那是键真的生效的证据。
    ③ 电池里最有用的是两支 **0 红**：`canonical_title` 的 `trim()` 与末尾 `strip()` 各自删掉都
    不响（折叠空白之后两者都不可能留下边界空格），而删折叠 1 红、删大小写 4 红。docstring 原来
    写"每一步都承重"，现在写的是**两颗牙 + 两个保险**，并且写明"删掉保险听不到声音"这件事 ——
    读信"全都承重"的人会删掉一个保险然后什么也不听见。
    ④ 那条审计现在按**三问**问 `main.rs` 这段文本：门读在每张脸**之前**、整份运行**只读一次**、
    每张脸的构造点**恰好一个**（针眼带 `(`，注释里提类型名不算）。后两问各有自己的变异体
    （第二次读门 1 红 / 第二处构造 announcer 1 红）；第一问**没有可编译的同型变异体** —— 挪读取
    必须同时删掉它前面的两个用户，那是重写不是替换。它的见证是 §24 那 6 条一起红。这句局限写在
    测试的注释里，不当成覆盖。
    ⑤ 一支方法学的读数：第二处构造那支的第一版**是编译错误**（`setup::SetupServer::start` 把
    `setup_config` 移走了），`cargo test` 于是既不打印 `test result` 也不打印红 —— 只看退出码会把它
    读成"这一格没人问"。同型替换要连"能编译"一起满足。
13. **「5353 上第二个读者会偷读数」这句话是错的，而它已经决定过代码的形状**（2026-10-18 量掉；
    这一格的价值不在结论而在形状）。原句：`SO_REUSEPORT` 把每个**进来的**数据报只交给绑在同一
    端口上的**一个** socket，所以 responder 若再开一个加入组的 socket，局域网里的设备就会
    **随机少看一半**，因此 responder 落地时**必须把两个读者合成一个**。仪器
    `scripts/mdns_reuseport_probe.py`：egress 用 `IP_MULTICAST_IF=127.0.0.1` + TTL 1 钉在 lo0
    （一个字节都不上局域网），每例的报文**事务 id 各不相同**（于是"每个 socket 都拿到全部 N 份"
    是逐条可核，不是两个计数碰巧相等），且每个 socket 的队列**只统计自己发出的那些 id**
    —— 第一次在 Linux 上读出 `14 of 12`，那是场上真实 mDNS 的包，若不过滤就会被当成"分发规则"。
    **两台机器的读数**：① 多播**复制给每一个 socket**（macOS `[12, 12, 0]` / `[12, 12, 12]`；
    Linux `[8, 8, 8]`，其中第三格是**没加入组**的那个 —— Linux 上绑 5353 就收得到多播）。
    所以旧句子在两个内核上都不成立，`announce.rs` 那个不 join 的发送 socket 也不需要靠"不 join"
    来避免抢读数。② **单播整条只交给一个 socket**，而且**交给谁随流变**：Linux 同一进程三趟
    `[8, 0, 0]` 稳定，换进程给出 `[0, 0, 12]`、`[0, 8, 0]` —— 按流哈希，不是"后绑的赢"，
    任何一趟都不配当规则；macOS 上 wildcard 绑定连**对照组**都收 0（很可能是 mDNSResponder 持有，
    非特权 `lsof` 点不出它），而绑到确切目的地址的 socket 把整条流从 wildcard 那个手里**全部**拿走。
    ③ **合并这个结论仍然成立，但换了理由和位置**：要合并的是**长期在场的那个 socket**，
    动机是**单播**（接线前那个 socket 是单播的竞争者却从不读，在 Linux 上还替每份多播攒了一个
    没人排空的队列），而不是"第二个 joined socket 会分到多播"。responder 落地时它 join 且读：
    答查询 + 把听到的喂进设备账本，于是"一直占着 5353 的 socket"就是"有人在读的 socket"。
    **留在文档里的形状**：一句没问过内核的机制陈述，只要写成了"所以下一步必须怎么做"，它就会
    决定代码，而它错的时候没有任何一条用例会红 —— 单元层每个测试各开各的 socket，真机层它表现成
    "网络不稳"。参考实现没有这一格也不是因为它想到了：zeroconf 的 `Engine` 本来就同时读 listener
    与 respond socket，所有包进同一个缓存。
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
15. **status 这一张文档第一次给出"单元 0 红 / 真机红"的读数**（2026-10-18，同一轮 9 支定点变异）：
    媒体表砍成"只广播的那 8 个名字"红 1；文档把存量值解析成标题（门的读法）红 1；
    `history` 改读设置文件红 1；两种拼法统一成一种红 1；客户端行里的 `host` 抹掉红 1；
    `status` 那行的门名从 management 改成 token 红 **11**（每一份读文档的用例都先撞这道门）；
    `py_truthy` 把数字 0 当真理红 2；内存历史不再按地址去重红 1（`src-tauri` 自己那条）；
    而**把宽地址集合喂给 `network_facts` 那一支，328 条单元用例全绿**，真机 §23 红一条。
    原因写在上面那一行里：一次调用同时产出 `server.ip` 与 `network.advertised`，
    所以"两个键互等"这条单元断言对任何集合都成立 —— **它是设计上恒真**，
    不是漏测；真正问这一问的是"页面给的集合 == 广播发出去的集合"那条跨读数者的比对
    （§22 从线上读回来的那一串）。这一格与 §16 第 14 条是同一枚硬币的两面：
    第 14 条是"广播侧读错集合，别人家栈看得见"，第 15 条是"页面侧读错集合，只有别人家栈看得见"。
    两条仪器错也记在这里，因为它们都是**这一轮的判据先坏后修**：
    ① `tr '、' '/'` 逐字节切一个三字节分隔符 —— 只有一条地址时没有分隔符可切，所以它绿；
    ② `MANAGEMENT_REFUSAL` 用 `text(body).contains(...)` 比对，而正文是 `ensure_ascii` 的
    `\uXXXX`，**永远不命中**（同 §9.3），改成解析之后再比 `message` 字段。
    还有一条方法学的：M2 的第一版交回的是**编译错误**（`json!` 不在非测试作用域里），
    当天第二次踩同一个形状（`main.rs` 那支 M10 的第一版因为移走 `setup_config` 也是编译错误）——
    只看退出码时"编译不过"和"这一格没人问"完全同形，**同型替换要连能编译一起满足**。
16. **一条审计被自己的变异体证伪过一次**（2026-10-18，`query=subscribers` 那一轮）：
    新加的文本审计 `the_window_command_builds_its_subscriber_document_from_the_same_function`
    第一版找的是 `"pending_append":` 这种**带冒号的整键**，而变异体写的是
    `doc["pending_append_total"] = json!(…)` —— 往文档上**赋值**而不是重打一份 `json!`，
    针眼没咬住，**0 红**。红不了不是因为规则没被破坏，而是审计的形状与破坏的形状不是同一个。
    加强后的 needle 是**开引号 + 键名**（不带冒号），并列禁止 `"aggregated`/`"subscribed`/
    `"pending_append`/`"pending_state`/`"pending_remove`/`"event_thread_alive`/`"children`/
    `"rows`/`"timed_out`；重测同一支变异体：**1 红**。两个附带决定写在同一条里：
    ① `"title"` **不在**针眼里 —— 窗口命令合法地写自己的 `title`（`player_snapshot` 与历史行
    各一处），把它列进去会让审计在**干净树上红**，而"干净树会红的审计"的下场是被删掉而不是被修好；
    ② 同一轮的门名变异体（`subscribers` 那行 gate 改成 token）红 **4** 条。
17. **§23 里那条"先重新问一次再断言"**（同日，脚本自己的顺序 bug）：subscribers 那一串检查接在
    clients 之后跑，而 `$API_BODY` 是**一个共享文件** —— 后面 `history` 的两条断言读到的是
    subscribers 那份文档，`json_len history` 回"没有这个键"，两条红。产品一个字节没错，
    **红的是我们对夹具状态的假设**。修法是在 history 之前重新 `api_probe` 一次，并把这件事写在
    检查上方。这一格与 `AGENTS.md` §4.2「计数器在 flush 之后才加」「夹具脚本里的那对括号是承重的」
    同族，只是这次共享的不是计数器也不是字符串拼接，而是**一个被三个读者先后覆盖的临时文件**。
18. **日志面板那三份文档是"一个建造者、两扇门"的第三次应用**（2026-10-18，紧接第 16 条）：
    `query=log` / `log-modules` / `log-download` 与 `POST clear-log` 的答案全部来自
    `macast_core::logging` 的三个 `*_document`，而窗口那三条命令调的是**同一批函数**。
    电池：加 `message` 让两个分支"统一"红 1；不再列旋转的 `.log.1` 红 1；让 GET 也能清
    红 1；窗口重新自己写一个键红 1。**四条仪器错先于四条红**，都要记：
    ① 手写 `Content-Length: 26` 与真实 32 字节的 body 不符 ⇒ 分发器没看见字段 ⇒
       答复是 fall-through 的 200/`code:1`，读起来像"门开着"；用 `post_form`（它自己算长度，
       其文档正解释 411 是**路由前**状态）之后才看到真正的 403-in-body；
    ② 又一次拿原始字节 `contains` 中文句（§9.3 的 `ensure_ascii` 后果，本轮第三次）；
    ③ 我在检查里断言 `Discovery.log` 含 `mDNS: announcing` —— 那句是**壳**写的，落在
       `macast.log`；`Discovery` 那个 logger 写的是 `mDNS advertised …`。两条都改成
       "各自文件里各自那句话"，并把谁写哪句写在检查上方；
    ④ 我又在检查里断言主日志的尾部含启动期那行总结 —— 它确实在文件里，只是**在窗口外面**：
       `?tail=` 的字节预算随行数缩放，`tail=20` 在 8.5 KB 的日志上就已经截断。
       一条"尾部断言"如果测的是"历史在不在"，它就在测一个产品从不承诺的东西；判据因此换成
       `truncated` / `size` / `lines` 三个数**互相自洽**（问超过行数 ⇒ 整份回来且说自己是整份）。
    旋转文件的行现在带**真实 `file`**（`name.log.1` 就是 `name.log.1`），因为
    `clear-log` 的 `removed` 数的是"文件 + 它带走的那几代"——面板若只列一代，那个数字
    就在用户面前没有解释。

19. **CORS 那条不变式现在也被这台端口自己扫**（2026-10-18，随 `launch-param` 一起补）：
    参考实现的 §4.7b 有一遍全仓扫描，断言"我们从不发 `Access-Control-Allow-*`"，而这份移植
    **此前没有任何等价的守卫** —— 这很讽刺，因为正是这条缺失让"回环可读、局域网不可读"这套
    门在浏览器面前还成立（`cast-info` 交回令牌、`launch-param` 交回整份设置表，靠的都是
    跨源脚本读不到响应体）。补上的是一个纯函数 `header_emissions(sources, needle)`：
    真实测试喂三个 crate 的每个 `.rs`（先断言扫到 ≥20 个文件，免得空扫描当成通过），
    合成控制喂四种形状 —— 会发的、只在注释里提的、**小写拼的**（HTTP 头名大小写不敏感，
    扫描器也就必须不敏感）、与无关的。两支变异体各红 1/2：把 `launch-param` 的门名改成
    `token` 红在"回环也被拒"那一条上（局域网那句听不出区别，因为检查是累积且有序的 ——
    这正是 §4.7c 的设计在测试层现形）；删掉"注释行不算"红 **2** 条，其中一条是真实扫描本身 ——
    我们仓库里关于这条不变式的散文正是那句话的活样本，一个会对文档变红的扫描器的寿命是
    一次"这测试干嘛挡我写注释"。
20. **responder 的判决层与装配层落地，十支 + 六支变异，其中一格从 0 红补成真判据**（2026-10-18，
    `respond.rs`）。判决层电池：`>` 改 `>=` 红 **2** / 抑制不看 alias **2** / 不抑制 **2** /
    共享 PTR 自称 flush **6** / 单播那份保留 flush **2** / ANY 多答 PTR **2** / `AAAA` 改沉默 **2** /
    TXT 拖上 host pair **2** / 无视 `has_v4` **1** / **M10（把抑制扩到附带段）第一版 0 红**。
    那支 0 红是本轮唯一的新形状：语料里没有任何一行问过"asker 已经知道 SRV、问的是 PTR 时
    那条搭车的 SRV 还发不发"，两种实现都绿。闭洞的方式是**去问参考实现**（探针加
    `ptr-known-srv` 一行，量到照发五条），而不是放宽断言 —— 语料因此 17 行，同一支变异体现在
    红 **2**。装配层电池六支：分段记错 **1** / 只写第一个地址 **1** / PTR 指主机 **2** /
    SRV 端口 +1 **1** / TXT 丢键名 **1** / **NSEC 位图清零 1**。最后一支之前**完全没有见证者**
    （我们的 parser 按设计跳过 NSEC，语料对拍不比正文），补的判据是把答复里那条 NSEC 的字节
    与广播语料逐字节钉住的那一条对齐 —— 同一个派生、两条路，而不是"再断言一次"。
    另两处仪器自己的错都在这一段：`walk` 先推进游标再切片（于是每条 rdata 都从下一条的开头
    读起，报"range end index 258 out of range"，读起来像 writer 写坏了长度），和我给 NSEC
    手抄的那个期望字节串 —— 压缩指针是**包内偏移**，广播与答复记录次序不同指针就不同，
    不变的只有最后三个字节。**语料对拍看得见 (section, type, ttl, owner, flush)，看不见
    rdata**，所以段这一层的判据是一条独立遍历（不走 `mdns::parse_message`）+ 一条把 PTR
    指向谁、SRV 端口与目标、A 的四个字节、TXT 的 `id=` 全部读回来的用例。
21. **`verify_rust_receiver.sh` 第四次被同一个反引号形状骗过，而这次连"守卫"也在撒谎**
    （2026-10-18）：`§23:2592` 的文案把字段名 `_add_history` 用反引号包起来 ⇒ 双引号里那是
    命令替换 ⇒ 脚本执行一条不存在的命令、标签塌成空串、**照记 PASS、EXIT 仍为 0**。
    更该留的一句是上一轮那条"必须是 0"的 grep：它**既漏报又误报** —— 分不清 `\``（合法的
    字面反引号，场上有两行）与裸反引号；而它当时报的 0 是在这两行与 2592 写进去**之前**量的，
    之后被当成规则抄了下去。现在换成 `scripts/check_verify_script_shapes.py`：只数未转义的
    那一个，`--selftest` 把出事那一行原样造进内存并要求报 **1**（量过：1；同时要求转义那行
    报 0）。**一条没有正控的扫描器，它的 0 只是"它没在工作的另一种写法"**（Part 50 那一族），
    而**一个不再复测的数字会伪装成一条判据**（§4.13）。
22. **responder 的跨机答复已经量到了，而 app 进程那一格仍然没有见证 —— 两半都要分开说**
    （2026-10-18，本轮把"问不出来"变成了"问得出来"，也把两条自己仪器的错翻了出来）。
    *现在量到的*：同一台 Mac 上，一个**只跑产品那批函数**的长命进程（`bind_engine` +
    `serve` + `respond` + `datagram`）收到 192.168.1.68 发来的真实 `PTR _googlecast._tcp`
    （40 字节，事务 id 0x1234），并且**那台机器看见了我们的答复**：
    `296 bytes from 192.168.1.5:5353 an=1 ar=4`。这就是 §25 outcome 1 想要的形状，
    只是它出自测试二进制而不是 app。
    *仍然没有的*：`macast-tauri` 这个 app 进程对同一条查询既不答也不说收到
    （`heard query …` 只在启动后 0.8 秒为一条 36 字节本地查询响过一次，之后三条上限内再没有第二条）。**这一句的读法下一轮被推翻过一次**：那「三条上限用光」不是送达的证据，而是本机自己的查询把预算淹死了 —— 见第 26 条，那里有重写过的仪器与重测的读数。
    同窗口一台 python 监听器（同样 wildcard 绑 5353、同样加入组）收到那条 40 字节查询，
    而**同一进程的 UDP/1900 确实收到来自 192.168.1.68 的 M-SEARCH** —— 所以"这个进程被
    系统挡了入站"这句**不成立**，"内核没把这条多播交给这个 socket"也没有解释为什么
    同码同形状的测试进程收得到。**原因未定，不许在这里挑一个填上**（§4.2「先拿证据再动手」）。
    *两条仪器错，都是我的*（§4.2「测试自己的实现也是实现」那一族，这次抄的是**线格式本身**）：
    ① 手搓的第一版查询把第一个标签的长度字节漏了（`_googlecast` 直接写字面文字），
    于是线上是 39 字节而正确形状是 40 —— responder 沉默是**对的**，我却把它读成"这台机器收不到"；
    ② 判"有没有答复"的过滤器找的是 `_googlecast._tcp.local.`（带点），而 mDNS 名字在线上是
    **长度前缀标签**，点永远不出现 —— 于是**任何**答复都读成 0，连参照实现那一侧的对照组也是 0。
    修好这两处之后才有了上面那条 296 字节读数。
    *一条新学到的可见性*：`bind_engine` 现在把**加入的是哪些接口**写进那句 INFO
    （`joined 1 of 1 interface(s) [192.168.1.5]`），`run` 把**收到查询**写成 INFO（问到我们头上的才算，上限八条）
    （`heard query N of 8 … asking Ptr _googlecast._tcp.local. … from <peer>`），别人的查询另有一条 DEBUG 与一条自己的上限。在此之前"从未答"有三种读法（没绑上 / 绑了没加入 /
    加入了但聋），本轮为分清它们花了两台机器和一小时；现在一条 grep 就能把"送达"与"判决"分开。
    §25 因此改成读这两句：`heard` 有而 `answered` 没有 = 产品的问题，红；两句都没有 =
    本机送达问题，`skip`（计败，不静默）。**而 §25 自己的监听器也中了同一条针**：它按带点的
    `_googlecast._tcp` 过滤线上字节，于是把满组的查询数成 `queries=0`，脚本一直走"仪器没听到"
    那一支。改成线标签 `\x0b_googlecast` 之后现跑整套是 **250 PASS / 1 SKIP / 0 FAIL、退出码 1**，
    读数是 `wire queries=3 answers=0` + app 一句没响 —— 这一格从此是**计败的 skip**，不是静默，
    也不是"responder 坏了"。
23. **`Timing` 在装配层还是惰的，所以一个把 QU 位读成恒假的变异体 0 红**（2026-10-18，实测）。
    `mdns.rs` 读出的 `unicast_request` 只喂给 `respond::timing()`，而 `announce::serve`
    **从不读 `reply.timing`** —— 它把每条答复立即发出去。把 `class & CLASS_UNIQUE != 0`
    换成 `false` 之后整 crate **688 passed / 0 failed**。这不是"覆盖够"，是**这一格在结构上
    无人可问**：参考实现把非立即答案压进 `U(20,120)` ms 的聚合队列（语料里的 `delays_ms`
    就是它的读数，本轮仍故意不作判据），聚合队列没落地之前，`Timing` 的四个值只有单元测试
    在算它，没有一行产品代码按它行动。登记在 §8 的偏离表里：**我们的 responder 立即作答**，
    后果是同一分钟内多个查询会把多份答案打到组播上（正是 §4.10 那类日志/流量膨胀的形状）。
24. **两条模块的日志从来没有落过盘，而这件事是读代码读出来的、不是用户报出来的**（2026-10-18）。
    `airplay.rs` 与 `relay.rs` 用 `log::` 自由宏，而全仓没有 `impl log::Log` / `set_logger` /
    `set_max_level` 中的任何一个（三条 grep 当时都是 0 行），`log` 的默认级别过滤又是 `Off` ⇒
    **18 条句子写进了空气**。里面最贵的三条恰好都是"用户会来问的那种问题"的唯一答案：
    macOS 自带接收端占了 7000 之后我们换了哪个口、`ANNOUNCE` 没带地址所以我们拒了、
    以及中转扫走了哪些遗留临时文件。修法是改走本端口自己的 `Logger`（名字照抄参考实现的
    `"AirPlay"` / `"MediaRelay"`、都不 claim，所以照旧落 `macast.log`），
    **并把 `log` 从 `macast-cast` 的依赖里删掉** —— 这一句才是承重的：从此 `use log::` 是编译错误。
    为什么没更早发现：这条通路**没有任何一条用例问过文件**（§12.3）。现在问了，两条各一
    （AirPlay 的两句拒绝、中转那句点名文件的清扫报告），而且中转那条顺手把消息补成了
    能行动的形状 —— 只报"扫走 3 个"回答不了"那三个里有没有我页面上还在用的那个"，
    所以现在带上前 5 个文件名。四支定点变异：把 logger 改成 claim（句子换文件）→ 红 1、
    把某条拒绝降到 DEBUG（默认阈值下不可见）→ 红 1、清扫消息去掉名字 → 红 1、
    一行改回 `log::warn!` → 全仓扫描那条红 1。**方向值得记一句**：这一格不是"用户报了 bug"
    那一类，是"日志指错嫌疑人比不指更贵"（§4.2/§4.3）那一类的**前一档**——日志一个字都不写。
25. **AirPlay 的 RTSP 面接进壳，而这一节的三条判据全在被测之前先错过了目标**（2026-10-18，
    `macast-cast/src/airplay.rs` + `src-tauri/src/{host,main}.rs` + 真机 §26 的 **20 条**）。
    *线是通的*：第二台实例只开 `DLNA + AirPlay`，真 RTSP 字节打进 7001（macOS 自己的接收端占着 7000，
    回落这件事本轮第一次被断言），`OPTIONS/ANNOUNCE/SETUP/RECORD/PLAY/SET_PARAMETER/TEARDOWN`
    全部 200，且 mpv 那边有 `file-loaded`、窗口名字 `macast_verify_clip`、读回来的音量 50。
    整跑读数是 **271 条 PASS / 1 条 SKIP / 0 条 FAIL，退出码 1** —— 那一条 SKIP 就是第 22 条那格（本机送达），
    计败而不是静默；§26 自己那 20 条今天全绿。
    *三条仪器错，都在我自己那一侧*：① `ap_grep "\'event\': \'file-loaded\'"` —— 双引号里的
    `\'` 是**字面反斜杠加引号**，于是这条判据从写下那天起就没问过任何东西，而它失败时印的是
    `tail -3`，读起来完全像"加载到一半被 stop 打断了"；产品没错，日志里 `file-loaded` 一直在那儿。
    ② 一次 splice 留下**两条 PLAY**，第二条把第一条的加载打断（mpv 那边 `end-file reason=stop` 加
    第二条 `start-file`），音量那一格反而**因为这次错误而通过**。③ 音量判据读的是 mpv 的
    `property-change` 事件，而 mpv **在值没变时不发事件**（同一条 0.5 发第二遍，日志一条不动、
    属性读回仍是 50）—— 判据因此换成两条持久的：接收端自己那句"两个数字都说"的 INFO，
    加从 mpv **读回来**的值。这条错属于 §4.2「等那个账本」的最后一档：**账本在不在，先问它会不会写**。
    *一格产品事实，量出来而不是猜的*：全新实例的第一个会话若在 `set_observe` 写完默认音量之前发
    `SET_PARAMETER`，音量被 100 盖掉（四条同形状跑：三次丢、一次没丢；后来的会话 4/4 落地）。
    参考实现同形（`mpv.py:404-414`），所以**不单边修**，§8 记一行，`player.rs` 的 `set_observe`
    docstring 里放着读数；§26 的客户端按真发送端的节奏隔 0.4 秒再发。
    *仍然欠的*：`_airplay._tcp` 的广播没接 —— **iPhone 的镜像/音频列表里看不到这台 Mac**，
    只有知道端口的发送端走得进来（§15 的 AirPlay 行写了这一句）。
26. **hearing 句子被本机自己的查询淹死，而一条被淹死的判据会写出一条假的历史**（2026-10-18，
    `announce.rs` 的两级句子 + `respond::concerns` + §25 重写 + §26 那节末尾的引号审计）。
    *上一轮写下的那句"这个二进制收不到 LAN 多播"是**仪器造成的误判**，不是读数*：
    `mDNS engine heard query` 当时对**任何**查询计数，上限三条，而本机自己的栈每约 0.8 秒问一次
    `Ptr _irohv1._udp.local.` —— 四条日志在启动后四秒内把预算吃光，§25 的冷浏览器于是问进一份
    空日志，读出来就是"送达失败"。修法不是把上限调大（本机换成更吵的网络照样淹得死）：
    INFO 只报**问我们**的（`respond::concerns` 复用出答案的那同一个匹配器，不在外面再抄一份名字规则），
    报别人的降到 DEBUG、各自一条上限，两条句子都**把问的东西写进行里**
    （`asking Ptr _googlecast._tcp.local.`）。上限 3→8 的理由是量出来的：一趟脚本里有 §22、§24、
    §25 三个浏览器在问。
    *重测之后的读数*（去掉被证伪的那句）：app 进程对浏览器的三条 40 字节查询**两句都不响**，
    同形状加入同组的 python 监听器三条全看见；把监听器整个撤掉、场上只剩 app 与浏览器，app 仍然一句没有。
    同一小时它确实收到过 `_irohv1` 那一族（source 同为 192.168.1.5:5353）。所以"入站多播被系统挡了"
    这句也**不成立**；成立的是"这一条没交到这只 socket 手里"，原因未定，§15 与 §16 第 22 条都不挑一个填。
    *七支定点变异，各红 1 条*（都是 e9，红在各自那句话上）：去掉 for_us 门 / 删掉 DEBUG 那一支 /
    上限 8→7 / INFO 句子丢掉主语 / `concerns` 恒真 / `concerns` 恒假 / 答复句子丢掉主语。
    **其中一支第一次是空操作**：我的 perl 模式匹配的是多行形状，而产品那行参数在同一行上 ——
    于是"0 红"读的其实是"没改动"，与 §13.4 那条编译错误冒充 0 红同一族。加了一条"变异必须真的改到文件"
    的做法：每次先 `grep` 证明那一处已经变了，再跑测试。
    *引号审计*（§26 那节顶部，每次跑脚本都执行）：本脚本的**代码行**里不许出现
    反斜杠紧跟单引号那一对字符（注释豁免，因为教训总要写在某处，而会冤枉文档的扫描器活不过下一次），
    并配一条**栽赃正控**：现造一个含一个 bug 的临时文件，扫描必须报 1。干净树报 0（本轮量过）。
27. **播放历史写进设置文件了，而"写在哪个文件"这一问也是这一轮才问出来的**（2026-10-18，
    `src-tauri/src/host.rs` 的 `with_setting` / `persist_history` + 真机 §27 的 **10 条**）。
    *为什么这一格值一节脚本*：§23 交付的第一条红就是它 —— 投完之后 `history` 是 `[]`，
    而参考实现把行写在 `Setting[Play_History]`（重启还在）。那一轮补的是"打印内存那份"
    （诚实的一半），持久化是另一半，它需要一条**重启之后还在**的断言才算数：
    单元层用同一路径上的第二个 `Setting` 实例（`a_history_row_survives_a_second_setting_instance`），
    真机层杀进程再起来读 `query=status`（§27）。内存断言永远问不到这件事，
    所以这不是"补一行写文件"，是把判据搬到只有操作系统能回答的那一层。
    *三条各自独立的判据形状*：① 读侧走 `peek` 不走 `get` —— `get` 的默认值副作用会把
    `Play_History: []` 造进一份从来没有过它的文件，于是"第一次运行"在磁盘上变成"用户清空过"
    （§4.2 第一条陷阱在启动路径上的再现，`reading_the_history_leaves_the_key_absent_on_a_first_run`
    断的是调用之后 `has()` **仍然为假**）；② 50 行上限是**关于文件**的承诺
    （`protocol.py:1215` 那句注释说的是"让设置文件保持小"），所以计数从磁盘上取而不是从刚写它的表里取；
    ③ 清空写的是**空列表**而不是删键 —— 对页面两者一样，对下一次启动的那句日志不是。
    *一处顺带修对的归属*：这张状态表的话一直写在 **`logs/MPV.log`** 里，因为 `main.rs` 当年把
    播放器的 logger 交给了它。`protocol.py:30` 是 `getLogger("Protocol")`、`logsplit` 给它
    `logs/Protocol.log`，所以 `setState: …` 与整段播放历史的故事本来就属于那个文件 ——
    一个追着"卡片为什么自己空了"的读者会被送去满是 mpv 事件的日志里，这是 §4.3
    "日志指错嫌疑人"的上一档：**指错文件**。修法在 `main.rs`（表拿自己的 logger 并 claim），
    而**没有**改成"脚本去所有日志里找"：§27 现在按**文件名**grep，
    所以这一格再被写回 MPV.log 就会红。单元用例同理各自 claim 一个独占名字的 logger ——
    `Logger::named` 按名字记忆，共享的 `"Macast"` 一旦被哪个用例 claim，别人的断言
    就读到那份文件去了（§4.2 并行夹具那一族）。
    *六支定点变异，各红 1/4 条*（一支一条命令，`/tmp` 备份 + EXIT trap 还原 + md5 回到
    `92ac018baf4b57cf6c25cc27216f3727`）：`add_history` 不再持久化 → 单元 **4** 红
    （重启、三键、上限、以及那句"没有设置文件"的自白），真机 §27 **4** 红
    （文件里没有 `Play_History`、重启后 0 行、标题为空、Protocol.log 没有读回那一句）；
    `peek` 换成 `get(..., [])` → 1 红；`clear` 不再持久化 → 1 红；垃圾条目不再被丢 → 1 红；
    丢掉那条 WARNING → 1 红；丢掉 INFO 那句"read back" → 1 红。**没有一支 0 红**。
    **两条读数要分开说**：① §27 最后那条"同一地址第二次投屏仍然是 1 行"在那支持久化变异体下
    **照旧绿** —— 它问的不是持久化，是"读回来的行与写入的行是不是同一张表"（display-copy 形状），
    写在用例旁边免得下一个人把它当持久化的第二个见证者；② 本机 `history` 的**顺序**与
    每行对象里键的顺序仍是记账的偏离（`BTreeMap` 排序），§8 那一行里带着。
    *验收脚本自己多了两道闸*（见第 26 条末与脚本顶部）：`\'` 引号形状与**未转义**反引号，
    两道各自配一条栽赃正控，任何一道在正控上失灵就整趟计败 —— 一个不会被栽赃骗到的 0
    不是"干净"，是"没在跑"。
    *计数（两台仪器一致）*：`macast-cast` **695**、`macast-core` 90、`macast-mpv` 149、
    `macast-upnp` 347、`macast-tauri` **18**（这一轮 +7）。真机
    **281 PASS / 1 SKIP / 0 FAIL、退出码 1**（那一条 SKIP 仍是第 22/26 条的送达那一格）。
    **以及本分支从未推送 ⇒ 这一轮的用例与那 281 条读数没有任何 CI 见证过。**

28. **`_airplay._tcp` 先取证再写包：一条语料、两个不能只冻一行的理由**（2026-10-18，
    `scripts/mdns_announce_probe.py --protocol airplay` + `macast-cast/tests/golden/mdns_airplay.hex`；
    **产品代码一个字节没改**，这一条是下一轮那个 builder 的输入）。
    *怎么问的*：`airplay_properties()` 用录下器把 `AirPlayProtocol._advertise` **叫一遍**，
    所以那九条 TXT 是产品交出来的表而不是从 `protocol_airplay.py` 里用眼睛抄的 ——
    `features=0x3B` 是 `"0x%X" % FEATURES` 的**格式化结果**，`deviceid` 在源码里根本不是字面量，
    文本扫描读不到这两件事。探针照旧只替换 `zeroconf._core.async_send_with_transport`
    （全包唯一一处 `transport.sendto`），所以一个字节都没上局域网 —— 真广播会在跑的这一段时间里
    让本机多出一台 AirPlay 接收端。
    *量到的形状*（与 Chromecast 那一串逐条同构，所以 `announce.rs` 只需要一个字段而不是第二份实现）：
    五条 PTR/SRV/TXT/NSEC/A；TTL 4500/120/4500/120/120；除 PTR 外全带 cache-flush；`flags=0x8400`；
    goodbye 是同样五条记录把 TTL 写成 0（不是空报文）；广播 345 字节（fixture 下 638 个十六进制字符）；
    probe 是 `qd=1 ns=1`、问 `_airplay._tcp.local.` PTR class `0x8001` QU 位**开着**。
    *两条"不能只冻一行"*：① 与广播语料同一条理由 —— `A/NSEC` 那一对来自 `set`，顺序随哈希种子变，
    所以 announce 与 goodbye 各两行（`-nsec-first` / `-a-first`），只冻一行就是钉一个 `PYTHONHASHSEED`；
    ② fixture 必须把**这台机器的身份**一起钉掉：第一版只钉了 `deviceid` 与端口，于是语料的实例名是
    `Macast-Fixture-1` 而 TXT 里那条 `name=` 仍然是 `Macast(Pavia-MacBookPro-1859.local)` ——
    一份"没法从自己的表头重建"的语料就是截图，不是格式。补上 `name` 的钉法之后表头与字节自洽。
    *一条关于行为而不是形状的量到事实*：`_random_mac()` 在**每次进程启动**重新抽
    （`protocol_airplay.py:99-103`，`_device_id` 不持久化）。所以手机在两次启动之间看到的是
    两台"不同"的接收端 —— 这是参考实现的行为，Rust 侧照抄（要改它要先改参考实现，
    而不是在这一层悄悄把 deviceid 存进设置文件）。
    *下一轮的接缝已经量好了*：`Service::datagram` 里 `let service_type = SERVICE_TYPE;` 是唯一一处
    写死的类型（其余全从 `instance` / `server` / `txt` / `port` 派生），所以广播侧加的是**一个字段 +
    一份九键 TXT 表**；responder 那侧仍只认 `_googlecast._tcp`，这一格要如实写进 §16 与 §15，
    别让"会广播"被读成"会答问"。

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
cargo test -p macast-core -p macast-mpv -p macast-upnp -p macast-cast -p macast-tauri   # 逐 crate，不跑全工作区
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
grep -n "TESTS_IN_THIS_CRATE: usize" macast-cast/src/proto.rs   # 期望它**等于两台仪器量出的数**：
#   `cargo test -p macast-cast --lib -- --list` 的计数，与 proto.rs 里那条扫描用例扫到的条数。
#   2026-10-18 最后一趟两边都是 **695**（AirPlay 那一轮 +5）。这里故意不写成一个可以照抄的常量：上一版写的 656
#   在被写下的那一轮就已经落后两级（656 → 671 → 686 → 688 → 690），一个会随加用例腐烂的数
#   不是引用体检，是§4.13 那句「一个没被机器问过的数字」的文档版本。
grep -n "impl Host for TauriHost" src-tauri/src/host.rs         # 期望 1 行
grep -c "has not been written yet" macast-mpv/src/lib.rs        # 期望 **0**（本轮改的注释）
grep -rn "targets::" src-tauri/src/                             # 期望 ≥2 行（§15）
grep -c 'CastChannel::new(' src-tauri/src/main.rs                # 期望 **1**（8009 那张脸的构造点恰好一个；
#   这一条由 `every_cast_face_is_behind_the_gate` 的"每张脸只构造一次"断言在管，这里是它的文档对应物）
grep -rn "CastChannel" src-tauri/src/ | wc -l                    # 今天 **8**（`host.rs` 6 + `main.rs` 2）。
#   §15 那一行当年记的是"从 0 行变成 **6** 行"——那个数**按散文数**，会随解释性注释涨（门审计的针眼
#   与它的新注释就各涨了一行）。所以这里钉的是**构造点**而不是提及数：一个会随写解释而腐烂的计数
#   不是判据（本节开头那条「按名字问，不按行号问」的教训，落在它自己写的这一行上）。
grep -c "set_read_timeout(None)" macast-cast/src/channel.rs      # 期望 **0**（§8 与 §16 第 11 条那一格：
#   一次跨阻塞读的无限超时就是那次推送饿死；这一行是那条修复的**反向**哨兵，
#   谁把它改回 `None`，§21 与 `a_quiet_sender_still_hears_about_playback` 会红，
#   而这条 grep 是它在文档里的对应物）
grep -c "pub const READ_POLL\|pub const MAX_FRAME_BYTES" macast-cast/src/channel.rs  # 期望 **2**
grep -c '^pub fn unquote_plus' macast-core/src/url.rs           # 期望 **1**（主人在这儿）
grep -c 'fn unquote_plus' macast-upnp/src/http.rs               # 期望 **0**（搬走了）
grep -c '^echo "== ' scripts/verify_rust_receiver.sh            # 期望 **30**（§27 是最后一节，
#   重启之后重投历史还在；§26 是 iPhone 的 RTSP 问答，§25 是 responder 的局域网那一节，§24 是门。§23 是 2026-10-18 的 status 一节，
#   门那一节当时从 §23 挪到 §24 —— 本文、脚本注释与 `AGENTS.md` 里指"那一节"的句子一起跟着改了编号，
#   编号变了而判据没变）
grep -c 'a query that is not ours' macast-cast/src/announce.rs        # 期望 **1**（DEBUG 那半句也只有一处；
#   两级句子是 §16 第 26 条那一格：INFO 只报问我们的，预算才不会被本机自己的查询淹死）
grep -c 'fn concerns' macast-cast/src/respond.rs                       # 期望 **1**（复用出答案的那个匹配器，不是第二份名字规则）
grep -c 'AirPlay SET_PARAMETER' macast-cast/src/airplay.rs             # 期望 **2**（"两个数字都说" 与 "读不懂也要说"）
grep -c 'self.persist_history();' src-tauri/src/host.rs                 # 期望 **2**（投完一条、清空一次 —— 写盘只有这一处）
grep -c 'PROTOCOL_LOG' src-tauri/src/host.rs src-tauri/src/main.rs       # 期望 **3 + 1**（名字的主人与唯一的 claim 点；
#   §12.1b 那一格说的是"目的地是一处决定"，而这一行是它的外部对应物 —— 谁在别处再 claim 一次，
#   `claim()` 的先到先得就会让句子的目的地变成一次赛跑）
grep -c 'AirPlayReceiver::new' src-tauri/src/main.rs                   # 期望 **1**（AirPlay 那张脸的构造点恰好一个）
grep -c 'pub fn announcement\|pub fn goodbye' macast-cast/src/announce.rs  # 期望 **4**（两种顺序 × 两种报文）
grep -c 'net::advertised_addresses(' macast-cast/src/announce.rs            # 期望 **1**，且下面这条 **0**：
grep -c 'net::advertised_hosts(' macast-cast/src/announce.rs                # 宽集合回到 A 记录就是 §4.1 第一行
grep -c 'set_multicast_loop_v4(true)' macast-cast/src/announce.rs            # 期望 **1**（发送侧，参考实现的 responder socket 也开环回）
grep -c 'join_multicast_v4' macast-cast/src/announce.rs                      # 期望 **1**（2026-10-18 起：
#   responder 接线后那个长期在场的口 join 且读，见 §16 第 13 条与 §14.6；这一行以前钉的是「广播口
#   故意不入组」，那个形状已经反向 —— 现在**不**加入组才是缺陷）
grep -c 'pub fn faces_enabled' src-tauri/src/host.rs                          # 期望 **1**（门只有一个主人）
# 应答这一格的三句话，各自只有一个主人（§14.6）：
grep -c 'mDNS engine heard query' macast-cast/src/announce.rs       # 期望 **1**（写句子的地方只有一处）
grep -c 'mDNS answered its first query' macast-cast/src/announce.rs # 期望 **1**（送达与判决是两句，不是一句）
grep -c 'HEARD_SENTENCES' macast-cast/src/announce.rs                # 期望 **7**（定义 + 两道上限判断 + 两处打印
#   + 文档里两处引用）；用例里的期望写的是**字面 8**，不是常量 —— 同源等式对任何常量都成立，
#   见 §16 第 22 末、第 26 条与 §13.4
grep -c 'x0b_googlecast' scripts/verify_rust_receiver.sh             # 期望 **1**：§25 的监听器按**线标签**过滤；
# `log` 门面这一格的两道闸（§12.3）：依赖不在图里是**编译错误**，源码扫描是它后面那一层
grep -c '^log = ' macast-core/Cargo.toml macast-mpv/Cargo.toml macast-upnp/Cargo.toml \
  macast-cast/Cargo.toml src-tauri/Cargo.toml | grep -v ':0' ; echo "五份 manifest 的 log 声明数应为空"
grep -rc '^use log::' macast-core/src macast-mpv/src macast-upnp/src macast-cast/src src-tauri/src | grep -v ':0' ; echo "同上，源码里也不许出现"
#   注意后一条在 (前一条成立) 时永远为空 —— 它是给"哪天有人把依赖加回来并解释为什么"那一天读的
#   带回族的教训是这条针眼曾经写成带点的 `_googlecast._tcp`，于是满组查询被数成 0（§14.6 ③④）
grep -c 'host::faces_enabled(' src-tauri/src/main.rs                          # 期望 **1**（一次读取管三张脸；
#   写成两处就会被 `every_cast_face_is_behind_the_gate` 的计数断言拒绝，这条 grep 是它在文档里的对应物）
grep -c '"DLNA_FriendlyName":' scripts/verify_rust_receiver.sh                # 期望 **4**（launch 那台 + §24 门那台 + §26 AirPlay 那台 + §27 历史那台），且下面这条 **0**：
grep -c '"FriendlyName":' scripts/verify_rust_receiver.sh                     # 期望 **0**：那是**没有读者拥有的键**
#   （`Key::DlnaFriendlyName` 映射到 `DLNA_FriendlyName`，`setting.rs` 那一行），2026-10-18 它让 §24 的
#   "线上没有广播"这句答复来自一个拼写错误而不是门 —— §16 第 12 条 ②
grep -c '        query: "' macast-upnp/src/http.rs              # 期望 **12**（GET 表：cast/cast-info/
#   resolve-status/resolve-cookies/cast-targets/status/interfaces/subscribers/log/log-modules/
#   log-download / launch-param —— 参考实现十五个里的十二个，剩下三个是 plugin-info /
#   module-settings / mirror-* 那批还没移植的）
grep -rc 'fn header_emissions' macast-upnp/src/http.rs          # 期望 **1**（CORS 不变式的扫描器在这里，
#   它扫的是三个 crate 的所有 `.rs`；`the_cors_scanner_answers_when_a_cors_header_exists` 是它的合成正控，
#   删掉"注释行不算"那一支就会红 2 条 —— 见 §16 第 19 条）
grep -c 'PostRoute { field:' macast-upnp/src/http.rs            # 期望 **7**（POST 表，含 `clear-log`）
grep -c 'fn log_view_document\|fn log_modules_document\|fn clear_log_document' macast-core/src/logging.rs
#   期望 **3**：日志面板那三份文档的唯一建造者（HTTP 脸与窗口命令都调它）。
#   第二个写点由 `the_log_documents_have_one_builder_for_both_doors` 用文本审计钉住。
grep -c 'pub fn sys_platform' macast-core/src/paths.rs          # 期望 **1**（`sys.platform` 的拼法只有一个主人；
#   谁把它换回 `std::env::consts::OS`，§23 那条对问解释器要答案的用例会红）
grep -c 'fn play_history' macast-upnp/src/store.rs              # 期望 **1**（接缝在这里），且下面这条 **6 个文件**：
grep -rl 'fn play_history' macast-upnp/src macast-cast/src src-tauri/src | wc -l
#   （声明者 1 + 实现者 5：`store.rs` / `http.rs` 的 TestStore / `soap.rs` 的 MapStore /
#    `eventing.rs` 的 FakeStore / `channel_tests.rs` 的 RecordingStore / `src-tauri/host.rs`
#    —— 按**文件**数而不是按出现次数，因为 host.rs 里天生有两条同名方法：表自己的读与
#    `StateStore` 的转发。数目涨了 = 有人加了第二个读历史的门，那要先说清为什么）
grep -rc 'subscribers_document(' macast-upnp/src/http.rs src-tauri/src/main.rs
#   （两份文档读者共用一个建造者：`macast-upnp` 的 HTTP 路由与 `src-tauri` 的窗口命令各调一次。
#    `the_window_command_builds_its_subscriber_document_from_the_same_function` 把"命令里不许再写
#    那些键"钉成文本审计 —— 第一版审计漏了一支"往文档上赋值"的形状，见 §16 第 16 条）
grep -c 'STATUS_MEDIA_NAMES' macast-upnp/src/http.rs            # 期望 **4**（定义、文档块、用例两处）——
#   那张 16 行的表是 `protocol.py:2033-2043` 的**超集**，砍成"我们广播的那 8 个"红 1 条（§16 第 15 条）
ls macast-cast/tests/golden/mdns_announce.hex macast-cast/tests/golden/mdns_names.jsonl macast-cast/tests/golden/mdns_airplay.hex
#   三份 mDNS 语料都在，且产生器在仓库里（同一条命令的三种旗标）：
#   scripts/mdns_announce_probe.py --fixture --dump-hex <announce 那份>
#                                  --protocol airplay --fixture --dump-hex <airplay 那份>
#                                  --dump-names <names 那份>
wc -l macast-cast/tests/golden/mdns_responder.jsonl # 17 行，一行一道查询形状（§14.6）
grep -c '"case"' macast-cast/tests/golden/mdns_responder.jsonl
#   期望 **17**。它与 `respond.rs` 的关系是"规则 → 判据"：`every_corpus_row_is_answered_exactly_as_
#   the_reference_answered_it` 逐行对拍，`the_corpus_rows_and_the_mapping_agree_in_both_directions`
#   钉住"没有哪一行没人映射"。`ptr-known-half`（2250 = 恰好一半）那一行是抑制判据 `>` 与 `>=`
#   唯一的分界见证；`ptr-known-srv` 那一行是 M10 从 0 红补成 2 红的那一格（§14.6 末条）。
grep -c '^pub fn respond' macast-cast/src/respond.rs   # 期望 **1**（判决层只有一个入口）
grep -c '^pub fn datagram' macast-cast/src/respond.rs   # 期望 **1**（装配层也只有一个）
python3 scripts/check_verify_script_shapes.py scripts/verify_rust_receiver.sh
#   期望 **0**（干净树，本轮量过）。形状判据的来源：`verify_rust_receiver.sh:2592` 用反引号
#   包住了一个字段名，而双引号里的反引号是**命令替换** ⇒ 脚本执行了一条不存在的命令
#   （`_add_history: command not found`）、把文案印成空串、**照样记了一条 PASS**，EXIT 还是 0。
#   上一版那条 `grep -c 'pass "[^"]*\|fail "[^"]*`'` 既漏报又误报：它分不清 `\``（合法的
#   字面反引号，本轮场上有两行）与裸反引号，所以它对两行安全文案报"命中"，而那句
#   "本轮量过：0" 是在这两行写进去**之前**量的 —— 一个抄下来的 0 不是测量（§4.13）。
#   工具自带合成正控：`scripts/check_verify_script_shapes.py --selftest` 把出事那一行原样
#   造进内存里的副本，审计**必须**报 1（本轮量过：1）。没有正控的扫描器，它的 0 只是
#   "它没在工作的另一种写法"（Part 50 那条同一形状）。
grep -c '^def assert_service_untouched' scripts/mdns_responder_probe.py   # 期望 **1**
grep -c 'assert_service_untouched(info, baseline' scripts/mdns_responder_probe.py
#   期望 **1**（每个用例都过一遍）。这条断言之所以存在：第一版仪器把 responder 自己注册着的
#   PTR 记录的 ttl 改成了 100 —— 三条抑制行的"沉默"全是那次污染，而第四行的答案里直接印着
#   `an PTR ttl=100`。见 §14.6 末「仪器自己犯过的两个错」。
grep -c '^    ("multicast\|^    ("unicast' scripts/mdns_reuseport_probe.py
#   期望 **6**（三例多播 + 三例单播，两种形状各自带一个对照组）。这一支探针是 §8 的 `announce` 行、
#   §16 第 13 条与 `announce.rs` 头部那一整段读数的**出处**；两种形状必须**成对存在** —— 只留一种
#   就会重新犯"从一种投递外推另一种"的错，而那正是被量掉的那句话犯过的（它只谈多播，却由它决定了
#   responder 的形状）。`--repeat` 的默认值是 3 也是同一件事：单播交给哪个 socket 逐趟会动，
#   一趟不是规则。
grep -n "DEVICE_PUSH_UNAVAILABLE\|这台设备还投不了" src-tauri/src/host.rs \
     scripts/verify_rust_receiver.sh
#   期望恰好两处，而且**都不是产品代码**：host.rs 里那句"这替换了什么"的历史说明，
#   和脚本 §16 里那条**反向**断言（设备臂再说出这句旧话就算红）。
#   除这两处之外任何命中都表示那句话回来了 —— 它是"这台机器还没有发送面"的谎。
# 本文提到的脚本节号确实存在
grep -c '^echo "== ' scripts/verify_rust_receiver.sh
```
