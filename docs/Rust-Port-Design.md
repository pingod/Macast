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

- 用例总数的家是 `macast-cast/src/proto.rs` 里那个 `const TESTS_IN_THIS_CRATE`（**按名字找，别按行号**：
  这一条原来写的是 `:410`，两轮之后它已经在 513 行 —— 行号是会烂的引用，而文档正是教过「会腐烂的锚要改成问法」的那一个）
  加同一文件里那条 `scanned >= TESTS_IN_THIS_CRATE` 的全仓扫描用例；
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

**2026-10-20 这座孤岛分了两层。** 同一条 Python 规则被三个 crate 各抄了一遍 ——
truthiness 在 `macast-mpv/src/pyops.rs`、`macast-upnp` 的 `py_truthy`、`macast-cast` 的
`resolve::truthy` 里各有一份，`type_name` 在 mpv 与 `module_settings::py_type_name` 里各有一份 ——
而三份**今天恰好同意**，所以没有任何一条用例是红的。这正是散布的代价：漂移不是发生在写下的那一天，
是发生在"只有一个人改了其中一份"的那一天。现在规则体在 `macast-core/src/pyops.rs`（四个读者都够得着的
最低一层），其余三处是委托；`pyops.rs` 里那条 `each_value_rule_has_one_body_in_the_workspace`
按整棵树扫**规则体**，地板 40 个源文件，并且**故意把表单开关留成两处** ——
参考实现自己就有两种拼法（`remove` / `set-github-mirror` 不 `strip()`，`?refresh=` 要 `strip()`），
所以那条用例钉的是"两种拼法、各有各的主人、strip 的那一处真的 strip"，而不是"全树只许一个"。
变异电池在这一格里没有用：**杀一份副本，另外两份照样答**，只有结构扫描问得出"有几份"。

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
| `plugin_repo`（「启用国内镜像地址」的**读法**） | 与参考实现**一致**，而它以前不一致：`bool(value)` 的 Python 语义（`"0"` 是**真**、`""` 是假、`"  "` 是真），而不是"值是不是那个词 true" | 关掉的一条偏离（2026-10-20）。旧实现只认 `true` 与 `"1"/"true"/"True"` 三个串，于是在高级设置里手写下 `"Github_CN_Mirror": "0"` 的那台机器，Python 开镜像、本壳关 —— 同一份文件两个答案，而页面按这个答案决定去哪个 host 拉索引。规则体现在只有 `macast-core/src/pyops.rs` 一份（`truthy`），三条委托都从它走 |
| `plugin_repo` / `module_settings`（表单里那个"是"的开关） | **不 `strip()`**，而本壳以前 `strip()` 过 | 参考实现两处都不 strip（`protocol.py:2493` 与 `:3103`），所以 `" on "` 在那里是**关**、`remove="  1  "` 是**不删**。这一格被自己的用例钉错过：`remove_flag_is_true` 的表把 `"  ON "` 写在"算删除"那一侧，也就是**测试替错误的形状作证**（§4.2「桩自己也会撒谎」的镜像形状 —— 判据本身是错的那一半时，变异电池不会响）。现在的三层：`pyops` 的字面表、`module_settings` 的表、以及真机 §29 把十一种拼写打给**参考实现自己的表达式**比。方向也要读对：这一侧的错答案是"删掉用户的键"，不是"少删一次" |
| `announce`（一个 socket 承担广播 + 应答；2026-10-18 接线前它不 join、也从不读） | 参考实现给**每一个** IPv4 接口各绑一个发送 socket 并逐口加入多播组（本机量到 6 个 socket、每轮广播 6 份相同字节）；本端口一个 `0.0.0.0:5353` socket，**不 join** | 这一行原来的理由**是一句从没问过内核的猜测**，现已量掉（`scripts/mdns_reuseport_probe.py`；2026-10-18 在这台 Mac 与那台 CachyOS 上各跑；egress 用 `IP_MULTICAST_IF=127.0.0.1` + TTL 1 钉在 lo0，一个字节都不上局域网；每例的报文**事务 id 各不相同**，所以"每个 socket 都拿到全部"是逐条可核而不是两个计数相等；并且只统计**自己发出去的那些 id** —— 第一次在 Linux 上读出 `14 of 12`，那是场上真实 mDNS 的噪声，不是发现）。两台机器的答案：① **多播是复制的** —— macOS 两个 joined `[12, 12, 0]`、三个 `[12, 12, 12]`；Linux 同样三行是 `[8, 8, 8]`，**包括那个没 join 的**（Linux 上绑到 5353 的 socket 不管加没加入组都收得到多播）。所以旧句子里"`SO_REUSEPORT` 把进来的数据报只交给一个读者，第二个 joined socket 会偷走设备列表的一半"**在两个内核上都不成立**。② **单播整条只交给一个 socket，而交给谁不是"后绑的那个"** —— Linux 同一进程三趟都是 `[8, 0, 0]`，换两个进程（源端口不同）则分别给出 `[0, 0, 12]` 与 `[0, 8, 0]`：选谁是**按流**算的，任何一趟都不能当成规则。macOS 上 wildcard 绑定的 socket 连**对照组**都是 `0`（场上系统的 mDNSResponder 很可能就是持有者，非特权 `lsof` 点不出它），而绑到**确切目的地址**的 socket 把整条流从 wildcard 那个手里全部拿走（`[12, 0]`，逐趟稳定）。③ **结论因此不是"不用合并"**：要合并，但理由换成**单播**，而且合并的位置是**长期在场的那个 socket**，不是每次三秒钟的临时 browse。接线**之前**它是单播的一个**竞争者却从不读**，在 Linux 上还是每一份多播的**接收者而队列没人排空**。responder 落地之后（2026-10-18，§15 那一行）同一个 socket **join 且读**：答查询，并把听到的喂进设备账本 —— 于是"一直在端口上的那个 socket"恰好是"有人在读的那个"。**还没做的那半句**是设备账本那一侧：读到的**别人的**记录今天只统计不入库（`Served::absorbed` 那一个计数器），所以合并的是 socket，不是两张表。`mdns.rs::open` 那句" replies 会以单播回到我们这里"也已按测量改成**条件句**：单播只回到**某一个**绑在 5353 的 socket，只有读它的那个是我们的时候才算数 |
| `announce`（不做唯一性探测） | 参考实现在广播之前先发 3 个 `QU` 的 PTR 查询并等回话（`generate_service_query`，`_core.py:263-283`），撞名就 `NonUniqueNameException` —— 而 `advertise()` 让它冒出去，所以 Python 那一侧撞名的后果是**这台机器根本不广播** | 我们照播。代价写在模块头：同一局域网里两个相同友好名的实例会发布**同一个** instance 与 SRV 目标，发送端可能把名字解到另一台的端口上。**而且我们连那个探测包都不构造**：没有"发完再听"的探测就只是一句问话，谁也不答它，写出来就是一段长得像覆盖的死代码（§4.13「不提问的检查」的形状）。语料里那行 `probe` 因此是**参考实现的**证据，`every_packet_we_write_is_a_response_and_never_a_query` 断言我们四个构造器没有一个交回它 |
| `announce`（属性过长时） | 参考实现**整条起不来**：`ServiceInfo(...)` 打包 TXT 时对 >255 字节的属性抛 `ValueError: bytes must be in range(0, 256)`，而这一句不在 `advertise()` 那两个 `try` 里（本机实测：200 字节过、300 字节抛），于是异常一路冲出 `ChromecastProtocol.start()` —— 用户改一个长设备名，买到的是 Chromecast 接收端没了 | 本端口砍属性、不砍接收端：按字符边界截到 255 字节，WARNING 里点名字段、原长度与上限，然后照常广播。方向与 §4.8「越界夹住而不是拒绝」、`vt_level` 的"少钉一个 level 只是不明确，钉错是每一帧都没了"是同一条。**Python 那一侧今天仍然如此**，这句话不是说它修好了 —— 记在这里是因为它是一条会被下一次"照抄参考实现"重新引进来的行为 |
| `announce`（没有监听就不广播） | 参考实现里 `_advertise()` 在 `_start_tls_server()` 之后无条件跑，而 `cast_port` 绑定失败时保持默认值 —— 于是它广播一个**没人听的端口** | 本端口只在 8009 真的在听时广播（`main.rs` 里那一格是 `match cast_channel.as_ref()`）。一台出现在菜单里却投不上的设备，正是 `AGENTS.md` §4.1 第一行的症状，不是它的小版本 |
| `announce`（QU 经组播到达的那一问**已落地**） | `add_qu_question_response` 的另一半 —— ¼ TTL 内多播过就**只回单播**，否则**立即多播** —— 现在照 `query_handler.py:245-248` 逐分支实现：`Group::from_qu` 由问题打上，`split_by_bucket` 交出第四条路 `Route::Direct`（它**不是**第四个桶 —— `_ucast` 从不进队列） | **剩下的只有顺序**：参考实现按 `sorted(answers, key=NAME_GETTER)` 排答案（让同名记录挤进同一个压缩窗口，纯尺寸优化），本端口按问题的计划序排。语料比的是集合，所以这一条**看不见也不是判据**；写在这里是因为「两份实现产出同一个包」这句话在它面前是假的 |
| `aggregate`（账本答两问，仍**不是** cache） | 参考实现的两问（「最后一秒」与「¼ TTL 内多播过」）现在问的是同一本 `Ledger`，条目带上 `created` **和 `ttl`**；`prune` 因此按较长的 ¼ TTL 老化 —— 按一秒老化会让 `is_recent` 永远答「没有」，而那一格只有判据不会响时才会发生（判据就是这条） | 仍然**不是** `DNSCache`：`ServiceBrowser` 监听者、过期堆、goodbye 记账在别处（`targets.rs` / `mdns::browse`）。**代价照旧**：`is_stale` 与 `mark_unique_records_older_than_1s_to_expire` 那两条没有落地，因为这里没有读者 |
| `aggregate`（NSEC 听不见） | 参考实现把 NSEC 解进 cache，于是重复的 `AAAA` 问题第二次走的是保护队列 | `mdns.rs` 按设计不读 NSEC（广播语料钉的是它**写**的字节，不是它读），`wire_identity` 因此对 NSEC 交回 `None` 而不是一个从名字编出来的键 —— 编出来的键会保护**另一条**记录。判据是 `the_ledger_cannot_hear_an_nsec_and_says_so`，它同时断言同一包里的 TXT 是被保护的，所以这一格是「一种记录类型」而不是「整个机制」 |
| `aggregate`（一条 A 计划 = 一个桶） | 一台有两个可广播地址的机器，参考实现对两条 A 各判一次；本端口的 `Chosen` 是一条计划、写到线上才展开成 N 条记录 | `plan_identity` 交回 **Vec**（每条地址一个键），`last_heard` 取其中**最新**的那一次 —— 即「任何一条刚被听见就整体延后一秒」。反向的读法（要全部被听见才保护）会把刚多播过的那条地址再喊一遍，而 RFC 6762 §14 要的正是别喊。判据：`one_planned_a_record_is_one_record_per_advertisable_address` + `a_heard_answer_protects_every_address_it_named` || `relay`（等待预算） | 两个等待（响应头之前、EOF 之后管道里）**合成一个连接级参数** | Python 里它们是同一个 `GROW_WAIT_SECONDS`，因为它们是**同一条策略**："读者可以先干等多久，才轮到'还没有'这个诚实答案"。写死成两个常量之后，一个夹具里两条预算可以悄悄漂成不同值 |
| `status`（四个键用 `peek` 读） | 参考实现的文档是 `Setting.get(key, default)` 拼出来的，而**那一次读会把默认值写进表里**，下一次 `save` 就落盘（`AGENTS.md` §4.2 的第一条陷阱）。本脸对 `Macast_Protocols` / `Macast_Protocol` / `Macast_Renderer` / `Play_History` 改用 `peek`，答案逐字相同、磁盘上不多一个键 | 边界要写清，因为它**不是**"本端口再也不写设置"：`ApplicationPort`、`Https_enabled`、`Https_Port` 与网卡钉 (`Network_Interface`) 仍然走共享读接口（前两者是壳自己启动时就在用的值，后者是 `macast_core::net::network_interfaces` 内部读的，改它要一起动 SSDP/广播/中转三张脸）。判据 `the_status_read_creates_no_key_that_was_not_there` 钉的就是这条边界，并且它**断言 `Network_Interface` 确实被创建** —— 一个把"什么都没写"当成整体主张的测试，会在下一次"顺手改成 peek"时红得说不清是谁的锅 |
| `launch-param`（键的顺序） | 参考实现交回 `Setting.setting` —— 那个 dict 保的是**文件里的插入顺序**；本端口这张表是 `BTreeMap`，于是键按名字排序 | 页面的「高级设置」是一个 JSON 编辑器，本机没有任何读者按顺序读它（量过：这条路上唯一的读者还没写）。真要保序，改的是那张表而不是在这条路由上再排一次 —— 那会是第二个主人。**同一张路由携带 `Api_Token`**：这条不是偏离而是决定，写在路由的行注释里，并由 `the_face_never_answers_a_cors_question` 与 `launch_param_answers_the_whole_table…` 两条一起守（前者守"为什么可以带"，后者守"它确实带"） |
| `subscribers`（文档比参考实现多两个键） | 参考实现的 `query=subscribers` 交回 `{aggregated, children[{title, type, running, event_thread_alive, subscribed, pending_append, pending_state}]}`；本端口那份**多** `pending_remove` 与 `children[].rows` | `pending_remove`：一个卡住的回收线程与一个卡住的追加线程是**同一种故障形状**，而少了这个数字，"订阅者永远不走"就没有见证者。`rows`：想知道"哪个 SID 落在哪个回调口"的调用方不该被迫去读 `query=status` 的 `clients` 再自己 diff 一份名单。**两个读者共用一个建造者**（`subscribers_document` —— HTTP 路由与窗口命令各调一次），所以"多出来的键"只可能长在一处而不是两份文档各长各的；审计与它的 0 红教训见 §16 第 16 条 |
| `status`（`history` 现在写在设置文件里） | **偏离已经关掉**（2026-10-18）：`StateTable::add_history` / `clear_history` 之后把整份行写进 `Play_History`，启动时 `with_setting` 读回来 —— 参考实现一直是 `Setting[Play_History]`，重启之后还在，而这一版曾经住在内存里，重启即空 | 发现方式是 §23 真机一节：投完之后 `history` 仍然是 `[]`，两条红。补它的判据必须问**第二次进程**：单元层用同一路径上的第二个 `Setting` 实例（`a_history_row_survives_a_second_setting_instance`），真机层杀进程再起来读 `query=status`（§27）。读侧一律 `peek` 而不是 `get` —— `get` 的默认值副作用会把 `Play_History: []` 造进一份从来没有过它的文件里，于是"第一次运行"在磁盘上变成"用户清空过"（§4.2 第一条陷阱，`reading_the_history_leaves_the_key_absent_on_a_first_run` 钉住）。**还剩两处小偏离，都在这同一行里记账**：① 文件里读不懂的条目本端口**在读取时就丢掉并数出来**（WARNING），而参考实现只在下一次写入时过滤、`query=status` 原样回显 —— 差别是"一份坏文件在两边页面上长得不一样"，而计数让这件事不再是静默的；② 每行对象里的键按 `BTreeMap` 排（`time`、`title`、`uri`），参考实现是插入顺序 —— 与顶层表同一条记账理由，读者都按名字取值 |
| `channel`（读的方式） | Python 在**裸 socket** 上阻塞读、读期间不持任何锁；本端口**每条连接一把锁**，所以读改成 `READ_POLL`（200 ms）轮询，**锁在两批字节之间放开** | 这是"参考实现做不到"的那一类，不是风格：rustls 的一次 `read` 会动会话状态，必须串行化，而把锁跨在一次无限阻塞读上会让**同一根 socket 上的所有推送饿死** —— `_watch_playback` 与状态变化都排在那把锁后面，发送端 LOAD 之后只拿到那一次请求驱动的答复、此后一句话都没有，直到它自己断开（2026-10-18 接进壳之后由 §21 量出，见 §16 第 11 条）。Python 那边根本没有这一层，所以这一格**不可能**由"照抄它的写法"得到正确答案 |
| `macast-mpv`（`set_observe` 之后才交出去的默认音量） | **不是偏离，是一格量到的同形缺陷**：链接一赋值就可写，而八条 `observe_property` 与那条**默认音量**是在这之后才写的 —— 落在同一个几毫秒窗口里的 `SET_PARAMETER` 会被 100 盖掉 | 2026-10-18 实测：全新实例的第一个会话，四条同形状跑（`PLAY` 之后 0 毫秒就发音量）**三次丢、一次没丢**（这正是"窗口"而不是"规则"的样子），同一实例后来的会话 4/4 落地。参考实现同形：`macast_renderer/mpv.py:404-414` 先 `self.ipc_sock = …` 再 `self.set_observe()`，`send_command` 每次单独拿 `command_lock` —— 所以这里**不单边修**（修它就是把自己的行为挪出参考实现，那需要自己的判据）。`verify_rust_receiver.sh` §26 因此隔 0.4 秒再发动作（真手机音量滑块不会在按播放的同一毫秒动），并把判据换成两条持久的：我们那句"两个数字都说"的 INFO + 从 mpv **读回来**的那个值。要动这一格的人先读 `set_observe` 的 docstring，读数就写在那里 |
| `airplay`（谁写播放状态） | AirPlay 的投屏**不写 DLNA 状态表、不写播放列表、不写历史** | `protocol_airplay.py:251-280` 调的是 `self.renderer.…`，那里 renderer 是**渲染器实例本身**而不是 `PlaybackGuard`。移植照抄这个形状：壳给的是 `ShellRenderer` 上的 `airplay::RendererCallback`，五个方法都是直通。后果要说明白：用手机投完一段之后，设置页那张卡继续显示**上一次 DLNA/Cast 交接**的内容 —— **这不是 AirPlay 坏了**，是把主人留在发送端该在的那一层；谁要在 Rust 侧"改进"成也写状态，要先改的是 §15 与 `AGENTS.md` §4.2 那句"只有一个主人"的归属，而不是悄悄加一次 `set_state` |
| `channel`（`MAX_FRAME_BYTES`） | 帧头声称的长度 > 8 MiB 时**先拒再分配**（`InvalidData`，不 `vec![0u8; length]`）；参考实现没有这个上限，它 `_recv_exact(sock, length)` 一路累积 | 那 4 字节是**对端给的**，`>I` 能声称 4 GiB。Python 的累积读至少不会一次性申请，而我们的实现一旦先申请就给了"一个谎长度打爆接收端"的形状。代价要写清：一台**真的**发 >8 MiB 帧的发送端在这里会被断开而不是收下。**8 MiB 是我们选的数，不是从设备上量出来的上限**（本局域网没有真 Chromecast，§16 第 1 条），它的依据只有两条：正文是 JSON 与 protobuf 控制消息，而 `>I` 那个字段可以对端随口写。**这一句只到"会话结束"，不到"用户看得见"**：`client_loop` 的错误统一走那句 `Cast client disconnected: {e}`，而它是 **DEBUG** 级（与对端正常断开同一句，`channel.rs` 的 `handle_connection` 尾部），所以默认阈值下这条拒绝**不进日志**。这是今天的形状，不是已验证的决定 —— 要把安全侧的那一格抬到 INFO，改的是这一句分级，不是上限 |

| `announce`（一个服务一个 socket，而参考实现是一个引擎两个服务） | **已合并**（2026-10-19）：本壳曾经有三个 5353 socket（浏览器 + 两个 announcer），现在与参考实现同形 —— `Announcer` 持有 `Vec<Service>`，一次 bind、一轮读、**按每个 service 的 `responder_answers()` 分派答问**，突发/重播/goodbye 逐 service 发 | 合并的理由是两台机器（macOS 与 CachyOS，`scripts/mdns_reuseport_probe.py`）量到的分发规则：**多播复制给每一个 socket，单播整条只交给一个，而交给谁随流变** —— 所以第三个竞争 socket 让"会答问的那个"变成一次抛硬币。合并之后剩下的两个（浏览器 + 引擎）与参考实现一致。判据：`one_socket_answers_the_measured_type_and_stays_silent_for_the_other`（同一个 socket 上 Cast 的查询被答、AirPlay 的查询被读到但不答，且答的那份报文按名字确认是 Cast 的）、`every_service_in_the_engine_gets_its_own_burst`（按 TTL>0 区分公告与 goodbye —— 第一版没区分，于是"只广播第一个 service"的变异体 0 红）、`the_join_list_is_the_union_of_every_service_and_not_the_first_one`（同样先 0 红：本机两个 service 地址相同，行为看不出来）。**仍然欠着**：`_airplay._tcp` 的查询现在会由合并后的 socket 按 shape 匹配应答 —— 这是参考实现的行为（一个引擎答所有注册类型），但本机的 `§25` 送达那一格仍是 SKIP，所以"iPhone 问一句能得到答案"没有真机见证者 |

| `announce`（第二种 socket 只收不发） | 参考实现的 `new_respond_socket`（`_utils/net.py:415-455`）绑接口地址之后**既收也发**（zeroconf 用它发那份接口的突发）；本端口绑出来的第二只是 **receive-only** —— 突发与答复仍全部从那只通配组播 socket 出去 | 两件事是分开的：把出口钉住需要 `IP_MULTICAST_IF`（那是 §16 第 39 条那一格，已按地址形式量过），而把**单播答复的源地址**也变成"被问的那个地址"需要按地址各发一份，而那只 socket 一发就同时把答复多播出去了 —— 参考实现能一步做到是因为它本来就一口一 socket。今天这条偏离的**可见后果没有被计价**：`§33` 能证到"答复的源**端口**是 5353"（发送端按端口接受，这是它唯一必须对的字段），而"一台把回复目的地址绑到自己学到的那个接口上的发送端"本机测不出来，需要第二台真机器。**没测的就不写**：这一行只说我们少做了什么，不说因此坏什么 |
| `save-launch-param`（非对象也被拒） | 参考实现把 `json.loads` 的结果直接赋给 `Setting.setting`；一个 JSON **数组**照样被存下去，下一次 `Setting.get` 在列表上崩溃。本壳对非对象也答 `json format error`（同一句），文件不动 | 一个"存进去然后把每个读者都绊倒"的值比一次响亮的拒绝更贵；拒绝的句子沿用参考实现自己的，所以网关的读者看不出多了一问 |
| `set-renderer`（列表里只有一个名字） | 参考实现的渲染器列表来自插件管理器；本壳只有一个内建渲染器，所以列表恰好一项：`Macast_Renderer`（空则 `MPV`）。只有这个名字被接受（接受时**落盘并 reload 播放器**，等价于参考实现的 stop/start 同一渲染器），其它一律 `未找到播放器：{title}` | 一个只有一项的列表不许假装找得到不存在的插件；而"接受自己"这件事仍然有可观察的效果（新装的 `''` 变成 `MPV`，那就是下拉里显示的名字），所以它不是只改文件的旋钮。reload 的可观察那一半要有媒体在放，见 §16 第 44 条 |
| `app-action`（`check-update` 被拒绝） | 参考实现把 `check-update` 交给它的发布者，答「正在检查更新」；本壳没有版本检查也没有更新器，答 **code 1** 并说明这一点 | 一个不存在的进度条比没有这个概念更糟（§4.8 那条「代价印在旁边」的反面）。`quit` 是照抄的：先答复、再在自己那根线程里退出（`protocol.py:2407-2409` 同一个形状），退出钩子负责 byebye/goodbye/关面/停播放器 |
| `toggle-protocol`（DLNA 这一格被本壳拒绝） | 参考实现能把 DLNA 也关掉：`_apply_enabled_protocols` 重启整个 service，DLNA 的 handler 跟着消失。本壳对 `toggle-protocol=DLNA Protocol` 答 **code 1** 并且**不写文件**（`Discovery::toggle_protocol` 的那一句） | 参考实现的用户还有菜单栏能把服务开回来，本壳没有：DLNA 的 HTTP 面**就是**设置页与管理接口（§16 第 12 条那半句「故意不做」的同一件事），关掉它等于关掉唯一能把它开回来的地方 —— 与 `macast-core` 把设置页放在那个面上是同一条理由。答复点名原因（不能停用 DLNA：…），所以它不是静默的偏离；手改 `Macast_Protocols` 去掉 DLNA 的文件仍按读侧规则跑（DLNA HTTP 无条件起），两者的差别记在这里 |
| `set-interface`（答复比参考实现多两键） | **加宽**：参考实现的 `_set_network_interface` 交回 `{code, message, interface}` 三格，本端口的答复在之后追加 `ssdp_reannounced` / `mdns_reannounced` 两个布尔 | 参考实现那句「已切换到 en0」是**发布之后**写的：`cherrypy.engine.publish('network_interface_changed')` 拿不回任何答案，所以它只能说三格。这里是一次直接调用，**那两个布尔是结果而不是请求** —— 而"控件读起来像结果"正是本轮的动机（一条 `code: 0` 配一根仍在广播旧地址的天线）。加宽是**附加**的：读那三格的页面不受影响，键序也把参考实现的三键放在前 |
| `announce::pin_multicast_if`（出口用**地址形式**，而 `sock.rs` 说 BSD 要下标） | 每次组播前 `IP_MULTICAST_IF = <该接口的 IPv4 地址>`，一份拷贝一个出口；不使用 `macast-upnp/src/sock.rs:28-36` 那套 macOS **下标**写法 | 量出来的：加 `IP_MULTICAST_IF=192.168.139.3` 的报文出现在**加入该口**的监听器上，而加入默认口（192.168.1.5）的那个读到 **0**（`scripts/mdns_iface_listener.py`，2026-10-20；§32 用它做前后对比）。`sock.rs` 的下标写法是照 Python `ssdp.py` 的形状来的，而 SSDP 与 mDNS 在这里不是同一个问题：SSDP 那个发送 socket 从不 join、也从不读，而本壳的引擎 socket 必须**同时**是听众与说者，所以它只能是一个 socket 转出口。**Windows 这一侧 `pin_multicast_if` 是明写的空操作** —— 那块卡上的 mDNS 本仓库从未量过，所以那里出口今天仍随路由；这一句写进代码注释与本行，不留成"看起来也做了" |
| 日志布局（核心模块也各自一个文件） | 参考实现只给**插件** claim 独立文件（`logsplit.claim` 的全部调用点是 `macast.py:336` 与 `:905` 两条插件加载路径）；`Protocol` / `Discovery` / `Chromecast` / `AirPlay` / `server` / `SSDPServer` 都 `propagate` 到根 handler ⇒ 全在 **`macast.log`**。本壳把 `MPV` / `Protocol` / `Discovery` / `Chromecast` 四个名字 claim 成了 `logs/<Name>.log` | 这是**移植决定**，不是照抄，所以它自己记账（§12.1b 那一格的理由曾在这一点上说错，已在原地改成量到的形状）。决定站得住的一侧：设置页「模块日志」那个下拉读的就是 `logs/`，高频行不把核心行埋掉是 §4.10 那一族的动机，而这四个名字在页面上都点得到。代价要说清并写在这里：**`AGENTS.md` §5 那条排障配方（`grep -aE "Cast LOAD|mDNS|ERROR" …/macast.log`）对本壳不成立**，按那一行去 grep 的人会在一个没有这些句子的文件里找它们。今天把四条 mDNS 总结句搬进了 `logs/Discovery.log`（它们的同伴句本来就在那），而壳自己的其余总结句仍骑在 `MPV` logger 上 —— 逐句归位的账记在 §16 与本节末尾那条待办 |

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
  `setState: …` 与整段播放历史的故事写进 `logs/MPV.log`。修法是表的构造点自己
  claim 一个名字（`host::PROTOCOL_LOG`），而**判据按文件名 grep**（§27 读
  `logs/Protocol.log`）：一个"在所有日志里找那句话"的检查会对着错误的目的地照样绿。
  **这一条当年写的理由里有一句是假的，就地改掉**：那句写的是「`protocol.py:30` 用的是
  `getLogger("Protocol")`、`logsplit` 给它 `logs/Protocol.log`」。量过的事实是：
  参考实现里 `claim()` 只有一个入口链 —— `macast.py:336`（`load_from_file`）与
  `:905`（`_load_bundled_plugins`）各调一次 `claim_from_module`，**只给插件**；
  `Protocol` / `Discovery` / `Chromecast` / `AirPlay` / `server` / `SSDPServer` 这些
  核心 logger 一律没被 claim，`propagate` 开着，所以它们全部落 **`macast.log`**。
  本壳把其中四个名字 claim 成了独立文件，那是**移植的决定不是参考实现的行为**，
  §8 有它一行；这一格真正成立的部分是**内部一致性**：既然这份壳里 `Protocol` 有自己
  的文件，状态表的话就该在那个文件里，而不是挨着播放器事件。理由换成了这个，
  修法与判据一个字没动 —— 一个错了的理由配一条对的判据，要改的是理由。
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
对拍它的是 `macast-cast/src/respond.rs`（判决层 + 装配层，24 条用例 —— 2026-10-20 由聚合队列补了两条）—— 还差的只有 socket
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
| 插件那七行（`plugin-enable/disable/uninstall`、`install-plugin`、`plugin-file`、`install/uninstall-webrtc-extras`） | **已交付答复**（2026-10-20，§18 的设计）：POST 表 16 → **22** 行 + `plugin-file` 的文件路由；六行表单端点以 `code: 1` 回答并点名"这个构建没有插件宿主/没有安装器"，`plugin-file` 在**读正文之前**就被拒；四行 code-execution 的门照抄（`install-plugin` / 两个 extras / `set-module-setting` / `save-launch-param` 共五行） | 判据：`macast-upnp` 1 条（三种开关的原话、局域网不带令牌的 install 被门拒）+ 表循环 + **真机 §38 共 7 条**：答复原话、局域网只听见门、带令牌仍是拒绝、`plugin-file` 同句、设置文件字节不变、设置目录下没有任何 `.py`、`plugin-info` 仍是空列表。**插件本身装不了**是这一行的结论，不是缺陷 |
| 管理面的 `save-launch-param`（整份设置替换 + 重启） | **已交付**（2026-10-20）：POST 表 **16** 行（参考实现第 10 行，门 **code-execution**，排在 `app-action` 之后）；`Setting::replace_all`（已有的"高级设置"写入口）整表替换并落盘，答复 **先走**、`MACAST_RESTART` 线程 250 ms 后 `app.restart()`。坏 JSON 与非对象都回参考实现原话 `json format error` | 判据：`macast-upnp` 1 条（表 16 行、code-execution 恰好两行、字段原样）+ **真机 §37 共 8 条**：两种坏输入都回同一句且**文件与进程都不动**；好输入回 `success`、旧 pid 消失、新 pid 在同一端口监听、新文件里的友好名生效 |
| 管理面的 `set-renderer`（页面上的播放器切换） | **已交付**（2026-10-20）：POST 表 **15** 行（参考实现第 6 行，插在 `toggle-protocol` 之前）；`host::renderer_title` 是名字的**唯一**主人（下拉那一行与这一行读同一个函数），接受时落盘并把播放器 reload，其它回参考实现的拒绝句 | 判据：`macast-tauri` 1 条（空 → MPV、存了名字 → 用它、前后空白不算另一个渲染器）+ `macast-upnp` 1 条（字段原样交出去、两种答复透传）+ 表循环 + **真机 §36 共 5 条**：不存在的名字被拒且不写盘、`MPV` 被接受并写进文件、HTTP 面照旧 |
| 管理面的 `app-action`（页面上的退出按钮真的退出） | **quit 已交付**（2026-10-20）：POST 表 **14** 行（参考实现第 8 行，插在 `toggle-protocol` 之后）；`Discovery::app_action` 先答复再在自己那根 `MACAST_QUIT` 线程里 250 ms 后 `app.exit(0)`，退出钩子照旧走 `stop_services`（byebye + goodbye + 关面 + 停播放器）。`check-update` 诚实拒绝（§8 那一行），未知动词回参考实现的「未知应用操作：{}」 | 判据：`macast-upnp` 1 条（字段原样交给 shell、三种答复透传）+ 表循环 + **真机 §35 共 9 条**：未知动词与 check-update 都**不动进程**、quit 的答复先到、进程在数秒内消失、应用端口与 8009/8008 全部空、日志里有那句 quit 与 SSDP 的 byebye |
| 管理面的 `toggle-protocol`（协议开关真的关/开脸，不必重启进程） | **已交付**（2026-10-20）：POST 表 **13** 行（参考实现的第 7 行，插在 `set-github-mirror` 之后）；写侧 `host::set_protocol_enabled`（未知标题剔除并 warning、已在表里仍答成功、停用不在表里的答"没发生"、关掉最后一个以 `至少要保留一个启用的协议` 拒绝、按 `available_titles()` 排序落盘）；`Discovery::{start,stop}_{cast,airplay}` 是 boot 与开关**共用的**构造/拆除函数，`apply_protocols` 在两者之后重建广播 | 判据：`macast-tauri` 3 条（写侧四条规则对着**文件**问、最后一个协议拒绝且不写盘、未知标题被剔除并留下 warning）+ `macast-upnp` 2 条（行在位、字段**原样**交给 shell —— 方向由 shell 判，页码只转发）+ 表循环（每行的门都被无令牌局域网与带令牌各打一遍）+ **真机 §34 共 24 条**：同一个进程里关掉 Chromecast ⇒ 8009 与 8008 都关、文件变成 [DLNA, AirPlay]、只有幸存的协议重新广播、DLNA 的 `description.xml` 照旧 200；再开回来 ⇒ 两个口重新绑定、Cast 的广播行回来、文件回到三项；两处拒绝（DLNA、未知）各自不写盘。**最后一格只有单元见证**：在活着的实例上改文件不会改它内存里的表，所以"关掉最后一个"在真机上问不出来，这一点写进 §34 的开头 |
| 应答者的**聚合队列**（四个答案桶里那三个延后桶的调度） | **已落地**（2026-10-20，`macast-cast/src/aggregate.rs`）+ QU 那一问同日（§16 第 38 条） | 先取证（§16 第 36 条四行读数），再照那四行落地（第 37 条），最后把 QU 经组播到达时的第四条路补上（第 38 条）。两个 `OutgoingQueue`（普通 0+500 / 保护 1000+200）、按记录身份合并、`_remove_answers_from_queue`、到点前顺延，加一本只从**听见**里学、同时答两问的 `Ledger`。判决语料仍是 17 行无状态判定 —— 队列那 4 行按名字排除并且数过。**剩下的不是这些**：答案顺序的 `NAME_GETTER` 排序、`is_stale` 一族 cache 卫生，都在 §8 |
| 5353 上的**第二种 socket**：让送到本机地址的单播查询真的进得来 | **已交付**（2026-10-20，`macast-cast/src/announce.rs` 的 `bind_unicast_listener` / `Engine::unicast` / `recv_any`，`start()` 把两只一起交给轮转） | 这一格是 §16 第 40 条那次取证的直接后果：一只 socket 做不了两个角色（绑了地址就不再收组播，通配口又收不到单播）。判据三层：`e12` 行为（外来那份 40 字节的查询被轮转听见并被答，答复源端口 5353；另有一条**对照**把轮转只指向通配口，1200 ms 内不许看见同样的字节 —— 有对照才说明第二只是承重的而不是"在场"）、`e13`（引擎那句总结必须从 socket 读回来，绑不上的那张卡不许出现在句子里）、一条装配审计（`.extend(` 与 `engine.unicast` 必须同在听众表那一段 —— **这一条的第一版被它自己的变异体证明不提问**，见 §16 第 41 条）。**真机 §33 共 8 条**，它给出的是这个 responder 历史上第一条**跨进程**见证：另一个进程单播问本机地址，shipped 进程答了，而答复由 `.venv` 里那套 zeroconf 的 `DNSIncoming` 读回五条记录（问与读都不是我们自己的实现）。§25 那一格因此**再收窄一次**：到得了的是单播，到不了的仍是组播，原因照旧不命名。**没做的**：`mdns::open`（浏览器）没有第二种 socket，因为本端口的查询从不设 QU（第 40 条那句"今天不受影响"仍然成立）；答复的**源地址**仍由路由决定（§8 那一行）；5353 上仍是两个 socket（浏览器 + 引擎），第 13 条的另一半没做 |
| 管理面的 `set-interface`（网卡选择器真的重新绑 SSDP、重新广播 mDNS），与组播**出口**跟着那张卡 | **已交付**（2026-10-20：POST 表第 **12** 行、门 management、行序照参考实现的第 1 行；`src-tauri/src/main.rs` 的 `Discovery` 是一个把手两扇门；`macast-cast/src/announce.rs` 的 `send_group` 让每份组播逐出口设 `IP_MULTICAST_IF` 各发一份） | 判据三层：`macast-tauri` 新增 3 条（四态句子逐字、拒绝点名拼错与可用表并且**磁盘字节不变**、成功文档五键顺序 + `" en0 "` 那种空格按参考实现是先 strip 再查表）、`macast-upnp` 新增 1 条（LAN 无令牌整条拒且 seam 一次都没被问 / 字段**原样**交给 seam / 空字段是"自动选择" / `set-interface&cast-uri` 只有前者跑）、**真机 §32 共 17 条**。两条只有真机能问：`ssdp_reannounced` 与 mDNS 那份突发是否**真的从那块卡出去**（A4 那支变异在单元层 0 红 —— 单元实例的 SSDP 本来就没起，`false` 是正确答案；§24 那条「门必须是真机判据」同一条）。**产品事实是手工探针先量到的**（§32 是把它变成判据的那一节）：钉桥接口时日志与文档都改了而加入该口的监听器读到 0 个包（`192.168.139.3`），突发仍在默认口上（`192.168.1.5`）—— 也就是说 `set-interface` 曾是一只**只动文档的旋钮**；修完之后那一条读数是 `3 datagram(s) … heard on 192.168.139.3`。**没做的两半**：Windows 的 `pin_multicast_if` 是明写的空操作（那块卡上的 mDNS 从未量过，见 §8 那一行），而 5353 上仍是两个 socket（浏览器 + 引擎），§16 第 13 条的另一半没做。见 §16 第 39 条 |
| SSDP **客户端**（M-SEARCH 之后拉描述、建设备表） | **库层已完成**（issue #27，见 §14.5）；**2026-10-18 接进壳** | `macast-cast/src/targets.rs` + `lib.rs`（`pub mod targets;`）。同一判据本轮重量：`grep -rn "targets::" src-tauri/src/` 从 **0 行** 变成 **2 行**（`host.rs` 的 `pub targets: Arc<targets::Table>` 与 `main.rs` 的 `Table::new(setting)`）|
| 渲染器插件的 `target_push` 面（设备下拉真正投到远端） | **已交付这一半**（2026-10-18）：壳把 `cast-targets` 从拒绝文档改成读这张表，`cast-resolved` 的设备臂做 `target_by_id` → `push_url_to` | 之前该面答的是**参考实现的完整拒绝文档**（提交 `d9cf9de`）。**剩下的不是接线**：本局域网里没有可投的真设备（§16 第 1 条），所以设备臂的**成功**那一路在真机上仍无见证者，只有 `push_tests` 的 46 条打在自家假设备上；`CastSender` 的 pause/seek/音量与 `text_tracks` 是 §15 下面那行的裁剪决定 |
| 管理面的 `query=status` 与 `query=interfaces`（设置页心跳文档） | **已交付**（2026-10-18，`macast-upnp/src/http.rs` 一张文档两个读者；`Face` 拿到 `SharedStore`，`Subscription` 多一个 `rows()` 读门） | 判据：`http_tests` 12 条（文档形状、16 个状态名、六种客户端键、`current` 与 `interface` 两种拼法、四键 peek 边界、`py_truthy` 表）+ 真机 §23。**同一轮后面又把 `query=subscribers` 接了进来**（GET 表 7→8 行、`Subscription::diagnostics()` 一扇门、`subscribers_document` 一个建造者两个读者 —— 窗口那条命令过去自己 `json!` 一份，现在与 HTTP 那份出自同一函数），`http_tests` 再加 5 条。**两条偏离记在 §8**（四个键用 `peek`；`history` 那一条**已经关掉** —— 2026-10-18 起它写进设置文件，判据在 §27）。**M9 那一支是本分支第一条"单元 0 红 / 真机红"的读数**：把宽地址集合喂给这一张文档，328 条单元用例全绿，只有 §23 的"页面与线上必须给同一个集合"红了 —— 因为一次 `network_facts()` 同时喂两个键，单元层里那两个键永远互等。见 §16 第 15 条 |
| 管理面的 `module-settings` 两面（「模块设置」那张卡的数据源，与它唯一的写入口） | **已交付**（2026-10-19：读 = GET 表第 **13** 行，门 management；写 = POST 表第 **8** 行 `set-module-setting`，门 **code-execution** —— 本壳第一行这种门，`macast_core::module_settings::set_value` 与读侧共用同一张归属表） | 判据三层。**单元 11 条**：`Key::all()` 的每一个名字被恰好一张表认领（两张表的行数之和 **恰好等于** 键表，两个方向都问 —— 有键没标签 = 漂移，有标签没键 = 指向一个这台机器写不了的键）、label 必须是中文（唯一例外是参考实现自己写成 `SSDP USN` 的那一格，例外表钉成 1 条且必须继续对得上）、七种 `type(value).__name__` 与 `editable` 的规则、缺席的键仍然是**一行**（`present: false` + `type: ''` + 可写）、其他（未归类）逐字用键名当 label 并给那句下一步、**建 payload 不创造键**（对着真的 `Setting` 问 `has()` 仍然为假 —— §4.2 第一条陷阱）。**HTTP 3 条**：文档形状与 absent 行、`launch-param` 与这份读文档**逐键比同一个值**（一份文件两个读者 —— §4.8 那条「两处答案不一致比没有数字更糟」的第三种形状）、LAN 无令牌整份拒。**真机 §28 共 16 条**：把 `macast/module_settings.py` **按文件路径** load 进来（`import macast` 会把 cherrypy 和 `SETTING_DIR` 一起拖进来），用**本机 HTTP 刚交回来的那些行**重算一份文档，再逐格对拍 —— 对拍用 present 行重建输入，是为了不去和"启动时实例自己写了 USN 与友好名"这件事赛跑。**三处记账的偏离**：① **没有插件组** —— 本壳还没有插件子系统，而参考实现在 `plugins=[]` 时交回的就是这个形状，所以这一格不是"我们少了"而是**用参考实现自己算一遍来证明**；② **`Timeout` 从 `cast_local_file` 的键变成本壳 core 的键**，连带 hint 改成本壳的读者（对拍时按名字排除这一格，并且**数过**：排除必须恰好一行、参考侧必须没有 Timeout，否则判据自己变红 —— 一条允许无界差异的比较就不再是比较，与 `--ci` 的豁免名单是同一条规矩）；③ 每格内部字段顺序两侧**相同**（工作区 manifest 开了 `serde_json` 的 `preserve_order`），§8 那条字母序偏离说的是 `Setting` 自己的 `BTreeMap` 表（`launch-param` 交回的就是它），不是文档格 —— 这两件事在写这一节的第一版里被我混成一件事，于是报出一条不存在的产品偏离，见 §16 第 30 条 |
| 管理面的 `plugin-info` 与两张 mirror 读 | **已交付答复**（2026-10-20；同一天的下一轮把 `plugin_repo` 那半句**翻面了**，见下面那一行）：`plugin-info` 是真数据（platform / version / 空的插件列表 / **真坐标**）；`mirror-state` 与 `mirror-snapshot` 是"这个壳没有镜像面"的完整文档，门是 **token**（loopback 也不例外，照参考实现） | 判据：单元 4 条 + 表循环（每行的门都被无令牌局域网与带令牌各打一遍）+ 真机 §23 的 5 条 + 真机 §29 的整节。`plugins` 仍然是**空列表而不是拒绝**（"从这个构建里装不了东西"是答案）。见 §16 第 33 条与第 34 条 |
| `macast/plugin_repo.py`（插件仓库坐标、「启用国内镜像地址」那一个开关、extras 的命名与地址规则） | **坐标与开关已交付**（2026-10-20，`macast-core/src/plugin_repo.rs`）：`mirror_enabled` / `set_mirror_enabled` / `to_mirror_url` / `index_urls` / `describe` / `extras_describe` / `extras_asset_name` / `extras_urls`；GET 面 `plugin-info` 交回 `describe()`，POST 表第 1 行 `set-github-mirror`（门 management，照参考实现的行序排在 `cast-uri` 之前）。**插件管理器与 extras 安装器仍未做** —— `plugins` 因此是空的，`extras_urls` 与 `extras_asset_name` 今天是**纯规则 + 用例**，没有产品读者（先中再壳，与 `targets.rs` 那条同一形状） | 判据：单元 14 条（三行的新鲜顺序、镜像开时**三行塌成两行**、jsDelivr 永不加前缀、关是**键消失**而不是存 `false`、读不造键、资产名四洞按序、开关的四词表）+ 单元 3 条 HTTP（两个读者读同一个键必须同答、门的三种来源、表序优先）+ **真机 §29**：把 `macast/plugin_repo.py` **按文件路径** load 进来（它模块级就 `from .utils import Setting`，所以要一个假父包；而桩 `Setting` 读的是**实例自己写的那份文件**），两个开关状态各逐格对拍一次；十一种拼写的"是"打给**参考实现自己的表达式**比；`Github_CN_Mirror` 从 HTTP 面与「模块设置」面各问一次。见 §16 第 34 条 |
| 管理面的两行 POST：`set-subtitle-show` 与 `clear-play-history`（页面上那两个按钮真的动东西） | **已交付**（2026-10-20）：POST 表 9 → **11** 行，两行都按参考实现的相对位置插入（`set-subtitle-show` 是第 14 行 ⇒ 排在 `cast-uri` 之前；`clear-play-history` 是第 21 行 ⇒ 排在 `clear-log` 之前 —— 两个"清空"按钮在同一个请求里时只有前者跑）；门都是 management（与 `set-module-setting` 那道码门不是同一种）；`StateStore` 多一个 `clear_history`（四份实现：壳的表、soap 的双、eventing 的双、HTTP 的双），`Management` 多一个 `set_sub_show` | 判据：单元 5 条（四种拼写的"是/否"表逐格比、**门不许写状态变量**——比较两次 `media` 读数的相等而不是比一个字面值、被拒的 POST 一次都不许到达播放器、清空之后 `query=status` 与账本自己同读为空、两字段同请求的先后）+ **真机 §30 共 18 条**（自带一台实例，因为 §24 故意杀掉了主实例）：真 mpv 在 PLAYING、播放器的**首次回声**落进表之后才开始动它、`"1 "` 必须**隐藏**（这一格把 §29 在镜像开关上量的"不 strip"在第二扇门上再问一遍）、文件里 `Play_History` 是空列表而不是被删、清空之后再一次投屏仍然恰好回填一行、两个清空字段的先后在线上再验一次。见 §16 第 35 条 |
| `link` 的 **Windows 列** | **未在本机行使** | §4.2 的原文边界句 || AirPlay RTSP 接收端接进壳（`airplay.rs` 的 7000/20 口回落、十一种方法、会话与音量） | **已交付**（2026-10-18）：`main.rs` 在**同一道 `Macast_Protocols` 门**之后起 `AirPlayReceiver::new(setting.friendly_name())` → `start()` → 线程 `AIRPLAY_RSTP`，渲染面是 `ShellRenderer` 实现的 `airplay::RendererCallback`（五个直通，没有第二个主人）；标题走 `resolve::title_for_uri` 那一个主人，`serve` 的 accept 改非阻塞轮询 | 判据：`verify_rust_receiver.sh` §26（**27 条**，其中 7 条问的是下面那串广播）—— 第二台实例只开 DLNA+AirPlay，真 RTSP 字节打进去，见证是 mpv 自己写的 `file-loaded`、窗口上那个名字、与 mpv 读回来的音量；另有两个新用例把 `SET_PARAMETER` 那两句（"两个数字都说" 与 "读不懂也要说"）钉在单元层。**`_airplay._tcp` 的广播同日接上了**（`Service::for_airplay` / `airplay_from_setting`，由**同一个** announcer 与 Cast 一起广播 —— 2026-10-19 合并成一个 5353 socket，见 §16 第 32 条）：§26 那 7 条是 `.venv` 里那套 zeroconf **从突发里**折出完整设备（PTR+SRV+TXT+A），端口读回来等于实际绑的那个 7001，TXT 的 `deviceid` / `model=Macast` / `name=` 三条逐个对；§24 的浏览现在两种类型都听，所以"关掉 Chromecast"这一问同时管住两条广播。**"会广播 ≠ 会答问"这一格在合并之后变了一半**：判决表只对着 `_googlecast._tcp` 量过（`Service::responder_answers` 是那道门），合并后的 socket 对**每个** `responder_answers()` 为真的 service 才答问 —— 所以 AirPlay 的查询**今天有答案**，那是参考实现的行为（一个引擎答所有注册类型），而它的单元见证是 `one_socket_answers_the_measured_type_and_stays_silent_for_the_other`。**没有真机见证者**：§25 的送达那一格在本机仍 SKIP，§26 的 browse 从突发就能成功，它既证不了答问也证不了不答问。announcer 自己那一行日志仍然明说这件事（`it announces this type and does not answer it — the responder table was measured for _googlecast._tcp`），§26 有一条问它在不在，因为一个把读者送去修一个没坏的东西的日志比没有日志更贵。**取证与形状的账**（`macast-cast/tests/golden/mdns_airplay.hex`）：与 Chromecast 那一串逐条同构（PTR/SRV/TXT/NSEC/A、TTL 4500/120/4500/120/120、除 PTR 外全带 cache-flush、`flags=0x8400`、goodbye 是同样五条记录把 TTL 写成 0），差别全在 TXT 那九条与端口，读法与那两条"为什么不能只冻一行"的理由见 §16 第 28 条。还有一格记在 §8：新实例的第一个会话若在 `set_observe` 写完默认音量之前发 `SET_PARAMETER`，音量会被 100 盖掉（参考实现同形） |
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
    `host::faces_enabled` 一次读取（AirPlay 接进来之后它交回 `Faces { cast, airplay }`，门从三张脸变四张），门住 8009 接收端、8008/8443 setup 脸、`_googlecast._tcp`
    广播，以及同日下一轮接上的 `_airplay._tcp` 广播（后者在 `airplay` 那一半门后：`main.rs` 只在 receiver 真的绑到口之后才建 announcer）—— 前三者是一个协议，分开门会得到"身份答得出来、连接握不上"的那台设备。
    证书生成也在门后（它往设置目录里写两个文件；一个用户关掉的协议不该留下密钥材料）。
    判据两层：`host::tests` 问**读法**（存的字符串要当成一条记录、legacy 键是追加不是优先、
    不认识的标题跳过并说一句、空表 = 全开、`ChromecastProtocol` 这种紧凑拼写在**两边都**解不出——
    参考实现的 `canonical_title` 只去 `" protocol"` 带空格那一截），
    §24 问**关上的门**（第二个实例只开 DLNA：8009/8008 必须拒连、日志必须说出是谁关的、
    浏览器必须看不见它、证书文件必须不存在）。**"看不见它"今天问的是两种服务类型**：
    §24 那个浏览同时 watch `_googlecast._tcp` 与 `_airplay._tcp`（名字带类型标签落盘），
    所以"关掉 Chromecast"这一问不再只对 Cast 的广播说话 —— 而这一格意外多了一个**正向对照**：
    同一趟浏览在线上读到了 **别人**的一条 `_airplay._tcp`（`Pavia的MacBook Pro._airplay._tcp.local.`，
    与本机 ControlCenter 占着 7000 那一条读数同形）。要说清这条对照证明到哪一步：它证明
    "线不是空的、这套浏览读得到这个类型的实例"，它**不**证明是谁发的 —— §24 的浏览只记名字，
    不解析 A 记录，所以"那是本机 macOS 自己的接收端"是从端口占用那一条读数**推**出来的，
    不是这一格量出来的（§4.14「空结果要说清是哪一种空」+ §10「先拿证据再动手」）。
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
23. **`Timing` 在装配层还是惰的，所以一个把 QU 位读成恒假的变异体 0 红**（2026-10-18，实测。
    **`Timing` 那一半在 2026-10-20 翻面了** —— 聚合队列落地（第 37 条），`serve` 现在逐答案按桶行动；
    **QU 位那一半仍然开着**，而且开着的原因不是遗忘：`ucast_request` 在参考实现里唯一的消费者是
    `add_qu_question_response` 的 ¼ TTL 那一问，而本端口的账本不存 TTL（第 8 节新增的两行记的就是这一条）。
    下面那段 688/0 红的读数照旧是它的出处，别当成"已经补完了"读。）。
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
    计败而不是静默；§26 自己那 20 条今天全绿（**这一节的 20 条是那一趟的读数**：同日下一轮 §29 给
    §26 加了 7 条问广播的判据，那一节今天是 **27** 条，整跑 **288 PASS / 1 SKIP**）。
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

29. **`_airplay._tcp` 真的广播了，而"会广播"与"会答问"是两格 —— 这一轮的四条读数各暴露了一处仪器或文档的谎**（2026-10-18 → 10-19，`macast-cast/src/announce.rs` 的 `service_type` / `for_airplay` / `airplay_from_setting` / `responder_answers` + `src-tauri/src/main.rs` 的第二个 announcer + 真机 §26 的 **+7 条**（这一节现在 **27** 条）与 §24 的双类型浏览）：
    *形状*：`Service` 多的是**一个字段**而不是第二份实现（第 28 条那条接缝照兑现）；两个 builder
    （`from_setting` / `airplay_from_setting`）共用同两个私人主人 `advertisable()`（窄地址集合，
    §4.1 第一行）与 `report_clipped()`（超长友好名截属性不截接收端）。答不答问收在
    `responder_answers()` 一行里：它问的是"这份判决表是为哪个类型量的"，答案只有 `_googlecast._tcp`。
    *真机读到的*（`.venv` 那套 zeroconf，浏览在实例启动**之前**开 30 秒，横跨初始突发）：
    `Macast-Rust-AirPlay._airplay._tcp.local.` 折成完整设备（PTR+SRV+TXT+A），
    **端口读回来 = 实际绑的那个 7001**（macOS 自己占 7000），TXT 的 `deviceid` 非空、
    `model=Macast`、`name=` 等于这趟种子给的友好名。§24 那一趟还**意外拿到一个正向对照**：
    线上真有一条别人的 `_airplay._tcp`（`Pavia的MacBook Pro._airplay._tcp.local.`），
    于是"没有我们的"不再是空线上的答复 —— 但它证明到哪一步要写清：那一格的浏览只记名字，
    **不**证明是谁发的（"本机 ControlCenter"是从 7000 被占那条读数推的，不是这格量的）。
    *四条各自独立的错，全在被发现之前长得像结论*：
    ① **§22 的 `tail -1` 从此读的是"最后一个服务类型"**。两个 announcer 一存在，`log_grep 'mDNS: announcing ' | tail -1`
    交回的就是 AirPlay 那条，于是 §22 拿 `_googlecast._tcp` 的浏览去解 `_airplay._tcp` 的名字，
    读出 `unresolved`，并把 **7 条** TXT 顺序 / `ca` / SRV 目标 / A 记录的产品判据报成红 ——
    而那一趟广播本身逐字节是对的（同一个浏览自己的 seen 列表里就躺着 Cast 那条）。判据修成
    **按类型选行**，理由写在行上方：从"每个服务类型一行"的日志里挑最后一行，等于随手挑一个类型。
    ② **共享模块的拒绝句会替另一个协议撒谎**：`start()` 的两条失败句写死「Chromecast will not be
    discoverable」，AirPlay 的广播失败时用户读到的是 Chromecast —— 而参考实现那句 `%s` 格式化的是
    **调用方给的 service type**（`discovery.py:268-269`），所以照抄的正确读法本来就是类型名。
    变异体把 `self.service.service_type` 换回 `"Chromecast"` → **2 红**（两条拒绝句用例各一）。
    ③ **一格 coverage 空洞是在写电池时发现的，不是在跑电池时**：`start()` 里
    `let answers = self.service.responder_answers();` 这一次派生**没有任何见证者** —— 单元用例都是自己
    手工把 flag 交给 `run()`，而真机那一问是 §25，本机 §25 的送达那一格是 **SKIP**。所以今天补的是
    `the_answering_half_is_derived_not_written`（结构性审计：派生在、字面量不在），并把它为什么是审计
    而不是行为用例写进测试自己的注释 —— **"§25 会抓"不是一条判据，尤其当 §25 在这台机器上跳过的时候**。
    ④ **我的仪器 grep 错了文件，而那次错把一条真缺陷挖了出来**：M8 那一支把日志句子改坏之后，
    探针在 `macast.log` 里找不到那行，读数一度读成"0 命中 = 判据有牙"。真相是那行**从来没写在
    `macast.log`**：壳自己的启动总结句一直骑在 `Logger::named("MPV")` 上，落的是 `logs/MPV.log` ——
    一个跟着 `AGENTS.md` §5 配方 grep `macast.log` 的人，永远读不到"这台 Mac 到底有没有在广播"。
    四条 mDNS 总结句搬进 `logs/Discovery.log`（它们的同伴句「mDNS advertised …」本来就在那），
    §22/§26 的读取改成**按文件名**（§27 立的规矩：在所有日志里找那句话的检查，对着错误的目的地照样绿）。
    搬的过程中又量出一句**假理由**：上一轮本文与 `AGENTS.md` 都写着「`protocol.py:30` 是
    `getLogger("Protocol")`，logsplit 给它 `logs/Protocol.log`」——量 `logsplit.claim` 的全部调用点是
    `macast.py:336` 与 `:905` 两条**插件加载**路径，核心的 `Protocol` / `Discovery` / `Chromecast` /
    `AirPlay` / `server` / `SSDPServer` **都没被 claim**，全部 `propagate` 到根 handler，
    在参考实现里都落 `macast.log`。本壳把核心名字也分文件是**移植决定**（§8 新加一行，含它的代价：
    §5 那条排障配方对本壳不成立），而 §12.1b 那格的原理由已就地换成量到的形状 —— 修法与判据一个字没动。
    *八支定点变异（一支一条命令，`/tmp` 备份 + trap 还原 + md5 回到当轮基线；每支都先验证文件确实变了）*：
    `responder_answers` 恒真 → **1** 红（`the_airplay_service_is_announced_and_never_answered`）；
    恒假 → **1** 红（同一条的另一问：Cast 必须是那个会答问的）；
    TXT `features` 的 `0x{:X}` 改小写 → **1** 红（逐字节对拍那行语料）；
    `advertisable()` 换回宽集合 → **1** 红，**而红的是行为那一问**（`the Chromecast announcement carries 192.168.139.3, which is not in the set a sender can reach`），
    审计那一问在同一条用例里排在它后面 —— 这台机器两个集合确实不同，所以"审计只是兜底"这句话在这里是真的；
    `run()` 里 `if answers` 换成 `if true` → **1** 红（同一格证明 flag 是**被 obeyed 的**而不只是被算出来的）；
    `start()` 的字面量 `true` → **1** 红（③那条新审计）；拒绝句写回 Chromecast → **2** 红；
    M8（日志句子改坏）见 ④ —— 它的第一次读数**无效**，无效的原因是仪器而不是产品。
    **没有一支 0 红；但 ③ 那一支在被补之前是真正的 0 红**，那一格从"没人问过"变成"有人问，只是问的是结构"。
    *计数（两台仪器一致）*：`macast-cast` **699**（695 → 697 → 699：语料对拍、"广播但绝不答问"、
    AirPlay 的拒绝句点名 AirPlay、答问那半是派生的不是写死的），`macast-core` 90、`macast-mpv` 149、
    `macast-upnp` 347、`macast-tauri` 18。真机 **288 PASS / 0 FAIL / 1 SKIP，退出码 1**
    （那一条 SKIP 仍是 §25 的送达那一格）。
    *诚实边界（三条，方向别读错）*：① **会广播 ≠ 会答问**，而本机上"不答问"这件事也没有见证者 ——
    §26 那条 browse 从突发就能成功，它既证不了答问也证不了不答问；要答 `_airplay._tcp` 的查询，
    先得对参考实现做 §16 第 27 条那一族的取证，不是在这层猜。② **本壳现在在 5353 上有三个 socket**
    （浏览器 + 两个 announcer），两台机器量过的规则是单播整条只交给其中一个 —— 今天的读数没有观察到损失
    （那些查询是多播送达的，而多播本来就复制），**但"没观察到"不是"没有代价"**，合并那一条已经排进 §8 与本节待办。
    ③ **没有任何一台 iPhone 连过这台接收端**：§26 的发送端是本脚本自己写的 RTSP 字节，
    真机的"手机列表里看得见并投进来"仍然欠着。**以及本分支从未推送 ⇒ 这一轮的 699 条与那 288 条读数没有任何 CI 见证过。**

30. **`query=module-settings` 接进管理面，而电池逼出三条「同源等式」与一次把自己读成产品的假偏离**（2026-10-19，`macast-core/src/module_settings.rs` + `macast-upnp/src/http.rs` 的 GET 表第 13 行 + 真机 §28 的 **16 条**）：
    *为什么是一张表而不是一个路由*：这一张脸的答复是**给用户读的文字**（label / hint）加三个派生布尔（`present` / `type` / `editable`），而参考实现里那张表是 `module_settings.py` 的唯一主人。所以移植的形状是"core 一张表 + http 一行"，写侧（`set-module-setting`，门是 code-execution）以后必须问**同一张表**，不能长第二份归属名单 —— §4.7c 那张 POST 表教的是同一件事：归属一旦散在两个读者手里，页面就会写着"这一格可写"而后端拒收。
    *跨实现的那道对拍是这一节的主体*：`scripts/verify_rust_receiver.sh` §28 把 `macast/module_settings.py` **按文件路径** load 进来（`import macast` 会把 cherrypy 与 `SETTING_DIR` 一起拖进来，而 `SETTING_DIR` 是 import 时冻住的常量 —— 那条已经写进 §4.9），用**本机 HTTP 刚交回来的 present 行**重建成 `stored`，再让参考实现算一份文档与本机那份**逐格 canonically 比**。输入取自答复而不是设置文件，是为了不去和"实例在启动时自己写了 USN 与友好名"赛跑；这一点在第一版里就红过，因为那一版拿种子文件当输入，而字段顺序那一问又把"文档里格子的顺序"与"设置表键的顺序"混成一件事（见下面第 ③ 条）。
    *三处偏离，各处都有自己的写法*：① **没有插件组** —— 本壳没有插件子系统。这一格没有用"我们还没做"答辩，而是让**参考实现在 `plugins=[]` 时自己算一遍**：它算出来的就是同一个形状，所以"两边在这个壳的现实里一致"是一句读数。② **`Timeout` 的归属换了**（Python 里它是 `cast_local_file` 的键，本壳是 core 的键，hint 也跟着改成本壳的读者）。对拍时按名字**排除**这一格，并且**数过排除**：必须恰好一行、必须来自 core、参考侧必须完全没有 Timeout —— 任何第二条差异仍然把比较判红。一个允许无界差异的"对拍"不是对拍，这一条与 `--ci` 的豁免名单是同一条规矩（§4.12 末）。③ **格子字段顺序两侧相同**（工作区开了 `serde_json` 的 `preserve_order`，root `Cargo.toml` 里那条注释说它是为 `LIVE_LOAD_OPTIONS` 开的）；§8 那条"键按字母排"的偏离讲的是 `Setting` 自己的 `BTreeMap` 表，也就是 `launch-param` 交回的那一份，**不是**文档格子。第一版把两件事当成一件，于是报出一条**不存在的产品偏离**，并且把它写成了判据 —— 修法是把判据改成问真实的那一侧（两侧顺序必须等于参考实现的 `key,label,hint,present,value,type,editable`），并把这句话留在检查上方，免得下一个人再把两个 map 混起来。
    *电池逼出来的四条「同源等式」/盲区*（S 系列八支，一支一条命令，`/tmp` 备份 + trap 还原 + md5 回到当轮基线；每支先验证文件确实变了）：
    S1 把 `Timeout` 那一行的键名改错 → **2** 红（认领表 + `is_owned`）；S2 `py_type_name` 里"是不是整数"那一问改成恒真 → **1** 红；S3 `editable` 对在场值恒真 → **1** 红；S4 `is_claimed` 恒真（于是永远没有"其他"桶）→ **2** 红；S5 把格子的 label 换成键名 → **macast-core 0 红 / macast-upnp 1 红**；S6 成功文档多长一个 `message` 键 → **1** 红；S7 「其他」桶那句下一步被截短 → **1** 红；S8 `is_owned` 恒真（invent 一个名字也认）→ **1** 红。
    **S5 那一支是这一轮学到的形状**：一个**建造者**的性质，在建造者自己的 crate 里没人问 —— `labels_are_not_empty_and_are_written_in_chinese` 读的是**表**，而把表里的 label 交出去那一步（`item()`）只有 HTTP 层在用。补的是 core 自己读一格（`first["label"] == "SSDP USN"`，顺带成了那条中文例外表的使用见证）。**"0 红"在这里不等于"覆盖够"，等于"这一格没人问"**（§4.14 中转那一节同一条）。
    **三条本来永远问不出东西的断言，是被电池而不是被机器发现的**：`assert_eq!(groups[0]["title"], json!(CORE_TITLE))` 这种写法拿常量和常量比，对任何标题都成立 —— 三处（两个组名 + 那句未识别 hint）全部换成字面量。这一族的判据已经写在 memory 里（「同源等式对任何常量都成立」），本轮第一次是在**自己刚写的代码**里抓到三条。
    *一条仪器谎，第五次是同一个形状*：§28 的 `pass "… \`preserve_order\`, so the page's row layout is not a coin flip"` 在双引号里放了两枚反引号，于是 bash 真去执行了 `preserve_order`，把那句话印成空串。**这一次是脚本自己的第一道闸抓的**（`bt-hits=1`，那一道带栽赃正控的审计，见 §16 第 27 条末）—— 记在这里是因为它是 AGENTS.md §4.13 那一族**第五次**现形，而且是第一次由闸而不是由人拦住：没有那道闸，这一行会以一条绿色进仓库。
    *LAN 那一格读错了层*：第一版拿 `sed` 在原始正文里找 `"code": 403`，而拒绝是 `{"code": 1, "message": "管理接口只回答…"}` 配 **HTTP 403** —— 状态码与正文里的 code 是两层，写成一层的判据会对着一台正确的机器变红（§9.3 那条"`ensure_ascii` 的正文不许裸 grep"也是同一族的邻居）。改走 `api_probe` + `json_ask` / `json_has`：先解析再问，四条各自独立（403、那句管理话、正文 code 是 1、正文里没有表）。
    *计数*：`macast-core` **101**（+11，`cargo test --lib` 与 `--list` 两台仪器一致）、`macast-upnp` **350**（+3）、`macast-cast` 699、`macast-mpv` 149、`macast-tauri` 18。GET 表 12 → **13** 行，而 `the_get_route_table_is_well_formed` 里那句地板从 `>= 12` 换成 `== 13`：**低一行的地板拦不住删掉一行**，而"这张表还在不在"正是它存在的理由。真机 **304 PASS / 0 FAIL / 1 SKIP，退出码 1**（那一条 SKIP 仍是 §25 的送达那一格；上一轮的读数是 288/0/1，这一轮 §28 带来 16 条新判据）。
    *诚实边界*：① **写侧还没移植** —— `set-module-settings`（门 = code-execution，且参考实现拒绝 list/dict 值）留在 POST 那一轮，所以这张卡今天读得到、点不动；② **没有浏览器读过这份文档**（`frontend/index.html` 还是占位页），所以"页面按这个形状渲染得出来"仍然是文本契约而不是截图；③ **`plugin-info` / `mirror-state` / `mirror-snapshot` 三张脸仍未移植**，而 `cast-targets` 那种"交回参考实现的完整拒绝文档"的形状是给它们预备的，不是已经给出去了。**以及本分支从未推送 ⇒ 这一轮的 101 + 3 条与那 304 条读数没有任何 CI 见证过。**

31. **`set-module-setting` 是这张脸上第一行 `code-execution`，而"读得到点不动"这件事补上之后立刻抓到两处形状**（2026-10-19，`macast-core/src/module_settings.rs` 的 `set_value` / `remove_flag_is_true` + `macast-core/src/setting.rs` 的 `set_name` / `remove_name` + `macast-upnp/src/http.rs` 的 `Gate::Code` 与 POST 表第 8 行 + 真机 §28 的写路径 **+11 条**（该节 16 → **27** 条））：
    *为什么这一行值得单独一种门*：这张表里住着 自动化钩子 的键，它们的值下一次投屏就进 `subprocess(shell=True)` —— 所以**写它等于运行它**，而 DNS 重绑定之下"本机"这个事实毫无信息量（AGENTS.md §4.7b 的原话）。这也是本壳第一次在表里出现第二种门名：`code-execution`，走的是同一道**累积且有序**的检查（管理门先问，所以局域网无令牌的调用者听到的仍然是管理那句 —— 把更响的那句先说出去，等于向一个连"这字段是私有的"都不该知道的人宣布"这字段能跑代码"）。
    *一个主人的两种读法*：`set_value` 住在 core，不在 http —— 写侧与 `query=module-settings` 必须问**同一张**归属表，否则页面把某格渲染成可编辑而后端拒收（§4.8 那条「两处答案不一致比没有数字更糟」的第三种形状：不是两处数字，是一处说能写、一处说不能写）。判据里有一条专门问这个：对六个名字，写侧的接受/拒绝与读侧"这一格在不在表里"**必须逐项一致**，包括 `Timeout` 与那个不存在的名字。
    *`Setting` 多了两个按名字的写入口*（`set_name` / `remove_name`）：`set(Key, …)` 表达不了"用户手写的、或将来某个插件的键"，而给每个插件键在 `Key` 里加一个变体等于让枚举成为"文件里有什么"的第二份主人。`remove_name` 回的是**在场与否**，因为页面要说「已删除」还是「本就未设置」，而**不在场时一个字都不写** —— 参考实现的 `if was_present: del …; Setting.save()` 就是这个形状（§4.2「读不懂的设置文件必须整份搬开」那一族的另一头：删除也要认得出自己删了个空）。
    *电池（W 系列六支，一支一条命令，`/tmp` 备份 + trap 还原 + md5 回到当轮基线，每支先验证文件确实变了）*：把这一行的门降级成 `management` → **2** 红（安全那一条 + 表审计里"码门恰好一行"那条新计数）；`is_owned` 那一问被绕开 → **3** 红；容器值不再被拒 → **1** 红；`remove` 的真值表恒假 → **1** 红；`set_name` 不再落盘 → **2** 红（磁盘那一条 + 删除那一条，因为"删了但文件没变"正是它抓的形状）；拒绝句不再把名字念回去 → **1** 红。**没有一支 0 红。**
    *两条形状是在补判据时现形的，都不是产品*：① 写侧的拒绝是 **HTTP 200 + 正文 `code: 403`**，而 GET 侧本壳是 **HTTP 403 + 正文 `code: 1``** —— 这是参考实现本来的两层（`Answer::denied` 与 `_body` 从不设状态码），我第一版拿 GET 的形状去断言 POST，于是对着一台正确的机器变红；判据现在把两层各写一次并在注释里点出"这是同一族第三次"。② `json_body_save` 那个手抄的正文串（`{"code": 0, "message": "已保存", …}`）与真正文差在 `ensure_ascii` 的 `\uXXXX` 与四空格缩进上 —— **能比较语义就解析，别比较字节的形状**（§4.14 那条"对原始字节 contains 中文句"的同一条），这一路只在两侧都出自同一个 `json_body` 时才允许逐字节比。
    *脚本的自审又拦下一次*：§28 写路径里有一条 pass 文案含未转义反引号，`bt-hits=1` 在正式跑之前就把整趟判败拦下 —— 这是同一族**第六次**现形、第二次由闸而不是由人拦住（第一次见 §16 第 30 条）。
    *计数*：`macast-core` 101 → **107**（写路径 6 条）、`macast-upnp` 350 → **353**（门 3 条），`macast-cast` 699、`macast-mpv` 149、`macast-tauri` 18；POST 表 7 → **8** 行（同一形状的两条计数断言：`== 8` 与 `code-execution` 那一列恰好一行）；真机 **315 PASS / 0 FAIL / 1 SKIP，退出码 1（上一轮是 304/0/1）**。
    *仍未做*：`install-plugin` / `save-launch-param` / `install-webrtc-extras` 三行还是参考实现独有（本壳没有插件子系统，也就没有能落代码的另外三条），`plugin-file` 上传同理；钩子的**执行**侧没移植，所以今天"写一个钩子键"在这台机器上还不会真的跑命令 —— **这句要说清，它不是安全已经完备，是攻击面还没被接上**：门已经按参考实现摆好，等移植的那一步不必重新决定门。

32. **5353 上只剩两个 socket，而合并顺带把「会广播 ≠ 会答问」那半句补成了行为**（2026-10-19，`macast-cast/src/announce.rs` 的 `Announcer{services}` / `run(&[Service])` / `join_addresses` + `src-tauri/src/main.rs` 的一个 announcer + 壳自己的日志归位；真机 §22 +3 条）：
    *为什么是合并而不是再加一层*：两台机器量到的规则是**多播复制、单播整条只交给一个 socket 且交给谁随流变**。上一轮给 AirPlay 加了第二个 announcer，于是本壳在 5353 上有**三个**竞争 socket，而参考实现只有一个（`discover()` 是单例，两个协议注册进同一个 `Zeroconf`）。**"没观察到损失"不是"没有代价"**：那一族的查询是多播送达的，多播本来就每个 socket 都收到，所以真机读数看不见这个风险 —— 这一格只能靠结构修，不能靠测出来再修。
    *合并的形状*：`Announcer` 持 `Vec<Service>`；`bind_engine` 一次绑定、join 所有 service 地址的**并集**；`run` 起手逐 service 发突发、每 60 秒逐 service 重播、退出逐 service goodbye；读到查询时对**每个 `responder_answers()` 为真的 service** 各调一次 `serve`（`respond::concerns` 是共享匹配器，所以 `_googlecast._tcp` 的问题找 Cast、`_airplay._tcp` 的问题找 AirPlay），计数器合并。**这同时把上一轮欠的那半句补上了**：参考实现的一个引擎会答**所有**注册类型，所以 `_airplay._tcp` 的查询现在有答案 —— 但要说清见证到哪一步：真机 §25 的送达那一格在本机仍是 SKIP，所以"iPhone 问一句能得到答案"**没有真机见证者**，只有合并 socket 的单元见证。
    *两条 0 红是写电池时抓到的，而两条都长得像"覆盖够了"*：
    ① **只 join 第一个 service 的地址** → **0 红**。本机两个 service 公布的是同一个地址，行为上分不出"并集"与"第一个"，这与 §22 那条"两个集合在这里恰好不同，所以计数比较能说话"是同一枚硬币：断言必须先拿到两个**不同**的集合。补的是 `the_join_list_is_the_union_of_every_service_and_not_the_first_one`（用 `192.0.2.1` 与 `192.0.2.2` 两个集合），补完该变异体红 1。
    ② **只给第一个 service 发突发** → **0 红**，而且第一次补的判据也**没抓到**：新判据统计"报文里出现过 `_airplay` 的名字"，而**goodbye 也带那个名字**（TTL 0）—— 一个只广播 Cast、却在退出时替 AirPlay 说再见的实现照样通过。判据改成只数 **`ttl > 0`** 的记录之后，同一支变异体红 1。**这一格值得留在文档里**：它说明"按名字出现"与"真的公告了"是两件事，而后者才是 iPhone 列表要的那一件。
    *顺带做完的 #25（壳自己的日志归位）*：四条 mDNS 句子上一轮已搬进 `logs/Discovery.log`，这一轮把其余**启动总结**（`receiver: HTTP …`、`Cast receiver: …`、`setup face: …`、`renderer log: …`、`AirPlay RTSP: …` 三条、`Chromecast is switched off …`）全部改走 `macast_core::logging::root()`（= `macast.log`），播放器的句子（`mpv candidate` / `no mpv found`）留在 `logs/MPV.log` —— 这是**分家不是搬家**。判据三层：单元文本审计（每条句子往回找 220 字符必须看到 `app_log.`，播放器的两条必须看不到）、`root()` 的名字仍是 `Macast` 且产品里没人 claim 它、真机 §22 的三条按**文件名**读（总结在 `macast.log`、不在 `MPV.log`、播放器句子仍在 `MPV.log`）。**一条断言因此被删掉**：第一版写的是 `!root.is_claimed()`，单跑绿、整组红 —— 同文件另一个测试为了自己的目的 claim 了 `"Macast"`，于是这条断言读的是**测试调度器**而不是产品（§4.2 共享夹具那一族在 logger 上的形状）。换成问源码"产品里有没有人 claim 它"。
    *变异电池*（一支一条命令，`/tmp` 备份 + trap 还原 + md5 回到当轮基线）：`answering` 不过滤（所有类型都答）→ **3** 红（合并见证 / 绝不答问 / 派生审计）；只 join 第一个 service → **1**（补完后）；只发第一个 service 的突发 → **1**（补完后）；把一条总结句改回 `log.` → **1**。
    *计数*：`macast-cast` 699 → **702**（+3：合并见证、逐 service 突发、join 并集）、`macast-tauri` 18 → **19**（日志归位）、`macast-core` 107、`macast-upnp` 353、`macast-mpv` 149。真机 **318 PASS / 0 FAIL / 1 SKIP，退出码 1（上一轮 315/0/1）**。
    *诚实边界*：① 合并后的 socket 会答 `_airplay._tcp`，但**没有一台 iPhone 连过这台接收端**，§25 的送达那一格在本机仍 SKIP；② 5353 上仍有两个 socket（浏览器 + 引擎），参考实现也是一个引擎服务浏览与广播，所以"两个"里剩下的那一个仍是本壳的结构差异（§16 第 13 条记着）；③ 本分支从未推送 ⇒ 这一轮的 702 + 3 条与那 318 条读数 没有任何 CI 见证过。

33. **管理面的十五个 GET 全部有了答复，而最后三个是「这个壳没有那一面」的完整文档**（2026-10-20，`macast-upnp/src/http.rs` 的 GET 表 13 → **16** 行 + 真机 §23 +5 条）：
    *为什么不是 404，也不是空对象*：`cast-targets` 立过那个形状（§16 第 23 条）—— 一张卡片轮询一个读，必须拿到一个**能让它停下来**的答复。`plugin-info` 是**真数据**：`platform`（`paths::sys_platform()`，与 `query=status` 报同一个值）、`version`、`plugins: []`、以及 `plugin_repo` 的四个键（`repo_url` / `index_urls` / `mirror_enabled` / `extras`）。**插件列表是空表而不是拒绝**："从这个构建里装不了东西"是一句答案，而一个缺失的键在渲染器里是 `undefined`。`plugin_repo` 的四个值都是**诚实的空**（没有 URL 可指、没有索引可拉、镜像关、extras 没有资产）—— 本壳没有插件子系统与 extras 安装器，编一个地址等于让页面指向一个这个构建装不了的仓库，与 `mirror-action` 不在 POST 表里是同一条理由。
    *两张 mirror 读的门口比别处严*：`GATE_TOKEN`，**本机也不例外** —— 参考实现自己写了理由（`protocol.py:2097-2103`：一个返回局域网设备列表与活动流，另一个返回用户桌面的**一帧**，"loopback 在这里不是意图的证明"）。本壳没有镜像面，答复是 `{code: 1, message: "这台设备还没有镜像面：…"}` —— 一句能让人行动的话，而不是 404（读起来像应用坏了）或 `{}`（读起来像"正在镜像但没有任何状态"）。
    *判据*：单元 2 条（`plugin_info_keeps_the_shape_and_says_the_list_is_empty`：四个键都在、`plugins` 是空**列表**不是 null、`extras.packages` 空；`the_mirror_reads_need_the_token_and_answer_a_complete_absence`：两个查询各自在无令牌 loopback 上 403、带令牌拿到 `code: 1` 且句子里有「镜像」）。表循环自动覆盖了这三行的门（每一行都被一个无令牌局域网请求打过一遍、再带令牌服务一遍），所以"新增一行忘了门"这件事在这里不是靠记忆。真机 §23 加 5 条：`plugin-info` 的空列表与 platform 串（与 `.venv` 解释器算出的 `sys.platform` 比）、`mirror-state` 无令牌 403、带令牌拿到完整缺席文档、句子点名缺的那一块。
    *变异*：`mirror-state` 的门降级成 management → **1** 红（无令牌的 loopback 被放行）；`plugins` 从空列表改成 `null` → **1** 红。两支都落在自己的判据上。
    *计数*：`macast-upnp` 353 → **355**、GET 表 13 → **16** 行（`the_get_route_table_is_well_formed` 的 `== 16` 与附录 B 同步）、其余不变；真机 **323 PASS / 0 FAIL / 1 SKIP，退出码 1（上一轮 318/0/1）**。
    *诚实边界*：① `plugin-info` 的 `plugin_repo` 是**空坐标**，不是参考实现那份能拉索引的地址 —— 谁要在这个壳里做插件安装，先移植 `plugin_repo` 与插件管理器，再把这四个值填上；② 两张 mirror 读的**门**与**形状**是照参考实现抄的，但它们背后没有任何镜像实现，所以"本机能不能镜像"这个问题在这个壳里没有可验证的答案；③ 本分支从未推送 ⇒ 这一轮 355 条与那 323 条读数 没有任何 CI 见证过。

34. **`plugin_repo` 移植进来，第一件事是把它上一轮那句「诚实的空」证明成一句谎**（2026-10-20，`macast-core/src/plugin_repo.rs` + `macast-core/src/pyops.rs`（值语义收敛到一个主人）+ `macast-core/src/setting.rs` 的 `github_cn_mirror` + `macast-upnp/src/http.rs` 的 POST 表第 1 行 `set-github-mirror` + 真机 §29）：
    *谎长在哪一格*：`Github_CN_Mirror` **是本壳的一个键** —— 它在「模块设置」归属表里（`module_settings.rs:104`，标签「启用国内镜像地址」），而 §28 的写路径刚刚能让页面真的写它。所以上一轮那份 `mirror_enabled: false` 不是"这个壳没有那一面"的文档，是**对着用户已经打开的开关说它没开**：同一份文件，「模块设置」那张卡答 `true`，`plugin-info` 答 `false`。这跟 §4.8 那句「两处答案不一致比没有数字更糟」是同一件事，只是这次的两个读者一个是我写的、另一个也是。**坐标本身是无害的**（这个构建里没有任何一侧去 fetch 它们，索引在参考实现里也是空的），所以交回真坐标不是许诺，交回假的那个才是谎。
    *收敛值语义的收获是一支 `.trim()`*：把 `truthy` / `type_name` / 表单开关收成一位主人之后逐条读参考实现，发现 `remove_flag_is_true` **过去 strip，而 `protocol.py:3103` 与 `:2493` 都不 strip** —— 也就是带空格的 `remove` 在 Python 里**不删**、在本壳里**删**。更贵的是这一格**有自己的用例写着"必须算删除"**（`"  ON "` 在 truthy 那一列），所以这不是"电池没抓到"而是**判据替错误的形状作证**：一条把规则本身抄错的用例，会让下一次有人"顺手 trim 一下"时全绿。现在三层各问一遍（`pyops` 的字面表、`module_settings` 的表、真机 §29 把十一种拼写打给**参考实现自己的表达式**比），并把方向写进 doc：这一侧的错答案是删掉用户的键，不是少删一次。
    *另一条同族的窄读法*：`github_cn_mirror` 过去只认 `true` 和 `"1"/"true"/"True"` 三个串，而参考实现是 `bool(value)` —— **`"0"` 在 Python 里是真**。手改高级设置写下 `"Github_CN_Mirror": "0"` 的那台机器，Python 开镜像、本壳关。改成 `pyops::truthy` 之后两边逐字一致（§8 两行都记了，一行是"关掉一条偏离"、一行是"不 strip"）。
    *新鲜顺序是这张表唯一会腐烂的地方*：三行索引在开关开的时候**塌成两行**，因为 mirrored raw 与表里本来那条 ghproxy 行是同一个串。把去重删掉的后果不是"少一行"而是多出一行 `ghproxy.net/https://ghproxy.net/…` —— 一条谁都不服务的地址；把 jsDelivr 也加进 `MIRROR_HOSTS` 的后果是给唯一那个国内永远可达的 host 再加一跳。两条各有单元用例，§29 再各问一遍线上串。
    *关是键消失，不是存 `false`*（AGENTS §4.6）：`set_mirror_enabled(false)` 走 `unset`，判据在真机上是对**那份文件** grep 那个键必须不存在 —— 对着答复断言 `false` 是问不到这一格的，两种形状在 HTTP 面上读起来一模一样。
    *判据三层与两台仪器*：单元 `macast-core` 14 条（坐标）+ 6 条（`pyops` 的三张表 + 那条整棵树的"一份规则体"扫描）、`macast-upnp` 3 条（两个读者同答一问、门的三种来源、表序优先于 `cast-uri`）；真机 §29 把 `macast/plugin_repo.py` **按文件路径**加载（它模块级 `from .utils import Setting`，所以要一个假父包；桩 `Setting` 读的是**实例自己刚写的那份文件**，不是第二份夹具），两个开关状态各逐格对拍一次，加上线上的门、十一格拼写表、四洞资产名两侧合成比对、以及三次轮询之后文件 md5 不变。
    *两条仪器谎都在 §29 自己身上，都长得像产品坏了*（§4.2 那一族在 Rust 侧的第 N 次）：① `pr_panel` 把 curl 的正文**从 stdin 灌进一段程序，而程序自己也从 stdin 来** —— 于是读者解析的是它自己的源码，报 `JSONDecodeError`、`panel=` 空串，读起来像「模块设置」那张卡没有这一行。这正是 §28 的 `ms_ask` 头注释写过的同一件事，我在下一节里重犯了一次；改法是落文件、按名字开。② 那张十一格的表把 Python 的 `True`/`False` 与 JSON 的 `true`/`false` 直接比字符串，于是**十一种拼写全部"漂移"**，而十一格答案逐格是对的。判据换成"两侧都归一化之后再比"，并把这件事写在循环旁边：**比不出大小写区别的比较，问的不是那件事**。
    *变异电池：16 支，逐支的红数*（一支一条命令，`/tmp` 备份 + 还原 + md5 回到当轮基线，且**先证明那一处真的改了**再跑；`/tmp/vrr_mut_*.txt` 与 `/tmp/bat29/` 是当时量过的那批）：
    ① 把 `.trim()` 加回表单开关 → **4** 红（`pyops` 的字面表、`module_settings` 的表、`plugin_repo` 门自己的表，以及那条整棵树的"一个主人"扫描 —— 它看见两处都 strip 了）；
    ② `to_mirror_url` 不再问 host、一律加前缀 → **4** 红；
    ③ `index_urls` 不去重 → 单元 **3** 红，**真机 §29 另交 3 红**（mirror-on 那份对拍报 DIFFER、开链是三行而不是两行、jsDelivr 那一格读回来的是 ghproxy 那一行 —— 三支各落在自己的那一格上，红法逐条印出来）；
    ④ `github_cn_mirror` 从 `peek` 改回带默认的 `get` → **2** 红（§4.2 第一条陷阱在 Rust 侧的形状）；
    ⑤ 把开关读窄回"只认那个词 true" → **1** 红，而且红在**本轮新加的那一条**（`the_switch_is_python_s_bool_not_the_word_true`）—— 它是因为我先去数"这一格有没有人问"才写出来的，不是电池逼的；
    ⑥ 关的时候存 `false` 而不是 `unset` → **2** 红；
    ⑦ 资产名的 arch 与 python tag 两个洞换序 → **2** 红；
    ⑧ `set-github-mirror` 的门从 management 升成 token → **3** 红（本机那次"门是承重的"在第二张脸上再现）；
    ⑨ 把 POST 表里那一行删掉 → 单元 **4** 红，**真机 §29 红 12 条**（LAN 那一格读到的是 fall-through 的「不认识字段」而不是 403、十一格拼写表整张塌下来 —— 每一格的答案都是空串、文件里那位从来没出现过、四张 mirror 行全部不动、两个读者那一格读到 panel=False / face=False）；
    ⑩ `describe()` 换成上一轮那份常量文档 → **2** 红（其中一条就是"两个读者读同一个键"那一格 —— 上一轮的那个形状今天有名字了）；
    ⑪ `truthy` 的字符串臂去 strip → **1** 红；
    ⑫ `type_name` 的 int/float 两臂换序 → **3** 红；
    ⑬ `py_type_name` 不再委托、交回常量 `"str"` → **3** 红；
    ⑭ extras 加第三个包 → **1** 红；
    ⑮ 成功文档把 `code` 与 `message` 换序 → **1** 红；
    ⑯ **把第二份 truthiness 规则体抄回 `macast-cast/src/resolve.rs`**（行为逐字等价，委托与本地体答同一个问题）→ **1** 红，而且**只有那条整棵树的扫描问得到**：这一支就是 §7 那两句"副本会替被杀那一份答话"的具体形状 —— 传统电池在这一格上结构性失明，因为被杀的那一份没有任何用例专属地读它。
    **本轮没有一支 0 红。**
    *仪器的错一支，而且伪装成"干净跑"*：那趟"收尾的全量跑"我加了 `--no-build`，而上一支变异体（⑨）改的是 `macast-upnp` 的产品码 —— 源文件已经按 md5 还原，**产物没有重新构建**，于是那 12 条 §29 红**全是真的**，只是真的在一份变异的二进制上。这是 Python 侧那条"等长还原留下一份有效的旧 `.pyc`"的 Rust 版本，方向也一样：**还原之后必须重新构建再跑**，`--no-build` 只在你确认过产物与源码同时钟的时候才是省时间的。判据换成先 `cargo build` 再跑，重跑读数 **350 / 0 / 1**。
    *仍然欠着的*：① **插件子系统本身** —— 管理器、热插拔、`plugin-enable/disable/uninstall`、`install-plugin`、`plugin-file` 上传、`save-launch-param`，以及 extras 的**安装器**（`extras_urls` / `extras_asset_name` 今天是纯规则加用例，没有产品读者）；② POST 表 **9 / 23 行**（参考实现那 23 行是 2026-10-20 数出来的，不是抄的）；③ §29 的对拍是**坐标文档**，不是"有人真去拉了那份索引"。
    *计数*（两台仪器一致：`--list` 与整跑读数；`macast-cast` 那台扫描仪器说 **702**）：`macast-core` 107 → **128**（+14 坐标 / +6 `pyops` 的表与扫描 / +1 开关的 bool 规则）、`macast-upnp` 355 → **358**、`macast-mpv` **149**（委托之后一条不少：它的表继续通过本模块的名字问这条规则）、`macast-cast` **702**、`macast-tauri` **19**；POST 表 8 → **9** 行（`the_post_route_table_is_well_formed` 的 `== 9`），GET 表仍是 **16** 行；真机脚本 31 → **32** 节，**350 PASS / 0 FAIL / 1 SKIP，退出码 1**（那一条 SKIP 仍是 §25 的送达那一格；§29 一节 **27 条**，上一轮那份 323 是 §29 之前的数）。
    顺手补了一格**落后的地板**：`macast-cast` 的 `TESTS_IN_THIS_CRATE` 停在 699 而两台仪器都说 702 —— 上一轮我在散文里写了 702，没有回来改这个常量，而它的判据是 `>=`，**低 3 不会红**（§4.13「一个没被机器问过的数字」在地板上的形状）。现在按量出来的 702 钉住。

35. **两行 POST 接上之后，第一条自己交出来的红是"我这条判据没在问任何东西"**（2026-10-20，`macast-upnp/src/http.rs` 的 POST 表 9 → **11** 行 + `store.rs` 的 `StateStore::clear_history` + `Management::set_sub_show` + `src-tauri/src/host.rs` 的转发 + 真机 §30 的 **18 条**）：
    *接的是什么*：`set-subtitle-show`（参考实现第 14 行）与 `clear-play-history`（第 21 行）。这两行不需要任何新子系统 —— 播放器侧的 `set_media_sub_show` 与账本侧的 `clear_history` 早就在了（§27 把账本接进了设置文件，macast-mpv 有 `command::sub_show`），缺的只是 HTTP 面上那两行表和两个口。**这是"壳先于库"的_reverse_**：先前几轮是库做完了等接线，这一轮是线接上了，什么都没新建。
    *一条空洞的判据，由变异体抓出来*：第一版 §30 写的是"POST `=1` 之后等 `DisplayCurrentSubtitle` 变 `true`"，跑绿了 —— **为Nothing**：mpv 在安装观察器时会把每个被观察的属性先报一遍（`set_observe`），而字幕默认可见，所以那张表在任何一个请求到达之前就已经要变成 `true` 了。把壳里的转发改成 `!show` 之后只有**第二行**红，这一点就是证据：第一行从没问过它声称在问的事（§4.13「不提问的检查」，而这次是我这一轮刚写下的判据）。修法两条：先等播放器把首报落进表（等的是"值是 bool 而不是设置文件里那个文本 `1`"），再从**隐藏态**出发，让每一行都是一次只有请求能造成的迁移。修完之后同一支变异体红 **4** 条，红话逐条是"expected false, got true"。
    *量到的一条产品时序*：播放器对 `sub-visibility` 的**首次报告落在 `PLAYING` 之后约 0.4–1.0 秒**（仪器 `/private/tmp/subtitle_probe.py`，逐条打时间戳与值）。也就是说"投完立刻按字幕开关"在两个实现里都会被那次首报盖一下 —— 参考实现同形（`mpv.py:404-414` 先装观察器再答话），所以这不是移植缺陷，也不必改产品；要改的是判据：**等那个状态，再动它**（§4.2 那一族的第三种形状）。
    *门不许写状态变量，这一格现在有名字了*：`DisplayCurrentSubtitle` 在 §23 那句"16 个状态名是超集，值可以是 null"里一直没有写点；今天它有了，而且写它的是**播放器的回声**，不是 HTTP 面。判据写成"两次 `media` 读数相等"而不是"等于某个字面值"——那个默认值 `''1''` 的主人是谁？是 `src-tauri` 的 `StateTable`（`host.rs:226-230` 按 Python 加载器的原话把状态装成裸文本），HTTP 侧的用例去复抄它就是把同一件事写成两个主人。变异体 N3（让面自己 `store.write(...)`）红 1 条，正是这一格。
    *"不 strip"在第二扇门上*：§29 在镜像开关那里量到参考实现不 `strip()`；`set-subtitle-show` 是同一位主人的第三个读者（`remove` 是第二个），而 `?refresh=` **要** `strip()` —— 两种拼法两个主人这件事已经写进 `pyops` 的头。线上那一格用的是 `"1 "`（表单里一个多余的空格）：它必须**隐藏**字幕。N2（面改成先 trim）红单元 1 + 真机 1。
    *电池：7 支，逐支读数*（一支一条命令、`/tmp/bat30/` 备份 + 还原 + md5 回到基线，并且**先证明那一处真的改了**）：N1 清空只答成功不碰账本 → 单元 1 红，**真机 4 红**（账本还剩 1 行、文件读成 OTHER、md5 没动、两个清空字段的先后也塌）；N2 门改成 strip → 1 + 1；N3 面自己写状态变量 → 1；N4 壳把布尔转发的方向反了 → **单元 0 红 / 真机 4 红**；N5 把 `set-subtitle-show` 挪到 `cast-uri` 之后 → 2；N6 那行的门从 management 升成 code-execution → 2；N7 成功文档多长一个 `removed` 键 → 1。
    **N4 那一支是本轮最该留下的一格**：`src-tauri` 那 19 条单元用例一条都不会红 —— 壳里那一行 `self.player.set_media_sub_show(&json!(show))` 的唯一主张就是"这个布尔原样到播放器"，而 mpv 侧"布尔→`yes`/`no`"已经有 `macast-mpv/src/player.rs:3917` 在问，真机上"到不到"只有 §30 那四行问得出。这不是覆盖洞要补，而是**一句话的转发的见证本来就在它两端**；写在这里是因为下一次有人看到"单元 0 红"就想加一条断言，那条断言只会把 `json!(show)` 再抄一遍。
    *还欠的*：POST 表 当时 **11 / 23**。其中 `set-interface` **已在 2026-10-20 同日关掉**（第 39 条 ——
    它欠的就是那句"重新广播"，而写键不重新绑口正是这条记录当时拒绝把它算作完成的原因）；
    剩下 11 行里，`set-renderer` / `toggle-protocol` / `plugin-enable·disable·uninstall` /
    `install-plugin` / `plugin-file` / `install-webrtc-extras` / `uninstall-webrtc-extras` 都要等
    插件子系统；`save-launch-param` 要等重启语义；`app-action` 的 `quit` 能做，`check-update`
    在这个壳里没有对应物。**本分支从未推送 ⇒ 本轮 128 + 362 条单元与那 368 条真机读数没有任何 CI 见证过。**

36. **聚合队列先量再做：四行读数，其中一行把"参考实现的防洪是从听见自己学到的"这件事说破了**（2026-10-20，`scripts/mdns_responder_probe.py` 加 4 个 `queue` 用例；产品代码一个字节没改）：
    *为什么要量*：`respond::timing()` 已经冻过（17 行判决语料里那 15 个延迟读数），但它只答"这条记录属于哪一桶"；本壳今天**没有桶** —— `announce::serve` 拿到什么都立刻发。要不要建一个队列，不能从 `_handlers/multicast_outgoing_queue.py` 读出来：合并判据是 `send_after(new) <= send_after(last_group)`，而两个 `send_after` 各自带一个新的 `randint(20, 120)` —— **答案是概率性的**，而那个包又恰好有 Cython 构建（§4.2 那句"包发的是编译产物，答案在字节里"）。
    *四行读数*（`--case txt-twice|ptr-then-txt|srv-then-txt|txt-twice-1s`，2026-10-20 本机）：
      ① 同一个聚合问题问两次、间隔 10 ms → **一个**数据报（212 字节，@+104.1 ms）：第二个问题一个字节的线都没占到；
      ② PTR 与 TXT 各问一次、间隔 10 ms → **一个**数据报（@+502.3 ms，带 TXT+PTR+A+SRV+NSEC）：合并保留搭车记录，而偏移落在**聚合上限 500 ms**而不是 20-120 ms —— 那是队列自己的 `send_before` 顺延（有两个组时第一组等死线，不提前发）；
      ③ SRV（立即）后接 TXT（聚合）→ **两个**数据报（+0.7 ms 与 +87.5 ms）：四个桶是真的，桶间的 86.8 ms 就是那个 `randint`；
      ④ 同一个 TXT 问两次、间隔 1.25 s → **两个**数据报（+45.9 ms 与 **+1339.6 ms**）：第二次走的是 `out_delay_queue`（`additional_delay=1000`，`query_handler.py:293`）。
    *④ 里那句才是这一节的奖品*：第二问之所以进"最后一秒"那一桶，是因为 `_has_mcast_record_in_last_second` 查的是 **cache**，而 cache 里那条 TXT 是**应答者自己发出去、被内核环回、又被自己收进来**的。也就是说参考实现的防洪不是"我记得我发过"，而是"**我听见了自己**"。一个从不读自己答案的端口要复现这一行，只能另立一本"我刚广播过什么"的账 —— 这与 §16 第 13 条（合并 5353 上的读者、`Served::absorbed` 只数不存）是同一件事的两端：**听见自己不是卫生问题，是行为前提**。
    *判据的纪律*：这四行**不进判决语料**。`respond::respond` 是无状态的，"几个数据报、在第几毫秒"它答不了；把它们冻进同一份 JSONL 就是给 Rust 的 diff 一行正确答案依赖队列的记录，而那件事失败的诚实形状是一次静默跳过。所以按名字排除、并且**数过**（`queue` 行不是恰好 4 条 → 探针退出 1）。顺手改掉一处自己骗人的计数：那行 `wrote 21 case row(s)` 数的是收集到的用例而不是写出的行 —— 一个把被排除的行也算进去的数字，会让下一个读者以为过滤器坏了。
    *仍然欠着的那半*：队列本体。要落地至少需要：两组 `AnswerGroup`（`send_after`/`send_before`、合并判据、到点前顺延）、发送时按记录身份去重（`_remove_answers_from_queue`）、以及一本"我刚多播过什么"的账来喂"最后一秒"那一桶（或者把 §16 第 13 条的合并做完，直接用它听见的那一份）。今天记为**没做**，不是做错。

37. **聚合队列落地：四行读数各自有了牙，而其中一行的牙是我自己的第二个集合磨掉的**（2026-10-20，
    `aggregate_tests` 22 条 + `announce_tests` 新增 2 条（36→38）+ `respond_tests` 新增 2 条（22→24）；
    `macast-cast` **728** 条，两台仪器一致：`--list` 728、逐文件签名扫描 728；裸 `grep -c '#[test]'` 给 734，
    多出来的六处是散文与 needle 里的字面提及 —— 第 16 节第 34 条那一条规矩第二次用在上线前的计数上）。
    *形状*：两个队列（普通 `0 + 500`、保护 `1000 + 200`，`_core.py:200-201` 那两个构造点逐字照抄）、
    合并判据 `send_after(新) <= send_after(最后一组)`、`dict.update` 的**覆盖**而不是并集、
    `async_ready` 的「两组以上就等到第一组的 `send_before`」、发送前 `_remove_answers_from_queue`。
    随机延迟**由调用方交进来**（`Outgoing::with_random`），否则第 36 条那四行读数没法当判据用；
    时钟也是参数，且是本线程的单调毫秒而不是参考实现的 epoch 毫秒 —— 这里没有任何一秒跟墙钟比过，
    所以 NTP 往回纠一次不会把一条答案困在一个已经过去的 `send_after` 后面。
    *听见自己*：`Ledger` 只从**读到的响应**里学（`_listener.py:326` 那句 `if not msg.is_query()`，
    分派看的是 QR 位、不是「有没有问题」——所以有一条**同时带问题和记录**的响应才有判据，
    `a_response_that_also_carries_questions_is_ingested_and_not_answered`；只看 `questions.is_empty()`
    的旧写法在那一条上红）。TTL 0 是**忘掉**而不是学会（`record_manager.py:130-135` 的不对称：
    不在表里的过期记录什么都不做，所以 `learn` 的计数也不给它 —— 第一版数了，M7 一支红）。
    *真机那半*：本机 2026-10-20 量到 `mDNS engine heard a response on 5353 and learned 4 record(s)`
    在第一串广播之后约 0.8 秒出现（§31），四个不是五个 —— NSEC 我们的解析器不读，那条偏离单独记 §8，
    并由 `the_ledger_cannot_hear_an_nsec_and_says_so` 同时断言同一包里的 TXT **确实**被保护，
    免得那一行被读成「机制在这里不生效」。**同一支探针还量到**：另一个进程发的查询（单播到
    `127.0.0.1:5353`、以及打到组播的）在这台 Mac 上让这个进程听不见，而它自己的多播环回听得见 ——
    于是 §25 那一格的"这个二进制收不到入站多播"收窄成"收不到**别人**的入站多播"，原因仍未定，不挑一个填。
    *电池 16 支，逐支读数*：M1 1 / M2 1 / M3 1 / M4 1 / M5 2 / M6 1 / M7 1 / M8 1 / M9 3 / M10 1 /
    **M11 第一次 0 红** / M12 1 / M13 1 / M14 1 / M15 1 / M16 1。
    **M11 那一支是本轮的形状**：`datagram_multi` 的去重我当时写成了**两个集合** ——
    一个用全部答案记录预置（对应 `sending = set(answers)`），一个记"这一段里已经写过"。
    把前者删干净，套件**一个字都不红**，因为我们的用例都是「一个服务、一段」的形状，
    后者把活全干了。也就是说这条规则的两半**在测试眼里是同一半**，而参考实现只有一份集合。
    修法不是补一条断言，是**删掉第二个集合**（一份 `sending`，先被答案灌满、随写随长），
    改完 M11 红 1 条（`two_questions_that_share_the_host_pair_cost_one_copy_of_it`）。
    这条与"两条规则在纸上只差一个 `HashSet`，在答案上差一份重复记录"是同一件事，
    而它只有在一支**0 红**的变异体面前才看得见。
    *顺带被这条纪律抓到的第二格*：`the_production_draw_stays_inside_the_inclusive_window` 第一版
    从 `Outgoing::new()` 抽 400 次样，要求上下两端都出现过 —— 那是一条**概率判据**，
    本机一跑就红（`low=0 high=3`，miss 概率约 4%）。把抽样次数抬到 5000 是止痛不是修：
    修法是给生产延迟一个**有名字的主人**（`production_draw`），用例问它，
    再加一条文本审计问 `Outgoing::new` 确实用的是它 —— 于是 `..120`（M15）与
    "构造函数里另写一份延迟"（M16）各自红 1 条，而断言本身不再是硬币。
    *仍未做*：QU 的 ¼ TTL 那一问（§8）、`Ledger` 之外的第二问（它只回答"最后一秒"）、
    以及 §25 的送达那一格 —— 所以**这一节里聚合的"少发一份"在真机上没有跨机器见证**，
    它有的是第 36 条那四行读数、728 条单元判据，和 §31 那句"我们听得见自己"。
    **以及本分支从未推送 ⇒ 这一轮的 728 条与 §31 的读数没有任何 CI 见证过。**
38. **QU 的那一问：第四条路不是第四个桶，而它的判据正好是语料里那行 `delays_ms`**（2026-10-20，
    `respond::Group::from_qu` + `announce::split_by_bucket` 的 `Route::Direct` + `Ledger` 多存一个 `ttl`；
    `macast-cast` 728 → **730**，两台仪器一致）。
    *为什么这一格现在才有牙*：上一轮落地的 `Ledger` 只存「什么时候听见的」，答不了参考实现那两问里的
    第二问 —— `_has_mcast_within_one_quarter_ttl` 走的是 `is_recent`（`_dns.py:208-210`，
    `created + 250 * ttl > now`，TTL 单位是**秒**）。所以条目必须带上 TTL，而 `prune` 必须跟着改：
    按一秒老化会让 `is_recent` 永远答「没有」—— 这是本轮**唯一一支 0 红变判据**的路径（N5 就是它，
    量出来红 1 条，而那条红的是新写的判据自己）。
    *读数*：语料里 `ptr-qu` 那行答的是 **+0.3 ms**（而同一份语料的 `ptr` 是 +92 ms）。
    那 0.3 毫秒不是计时器噪声，是 `_mcast_now` —— QU 经组播到达时**不进聚合队列**。
    本端口在上一轮之后会把这一问答成 Aggregate（20–120 ms 之后），也就是**读数早就在指出这一格，
    只是没人按毫秒去读**（第 36 条故意定过"延迟不作判据"，所以这条也不是断言毫秒：断言的是桶，
    毫秒是它为什么该是那个桶的证据）。
    *四条判据*：QU 而记录不新 → 立即多播、**不**回单播；QU 而记录新 → **只**回单播（组里一个字节的
    多播都不许有 —— 反读 RFC 6762 §5.4 就是"六个浏览器开一次 Cast 菜单，组里六份重复"）；
    源端口是临时口时 QU 位**不参与**（`if not ucast_source and is_unicast` 里那个前提，
    判据同时钉住"两份拷贝都在"，与语料 `ptr-ephemeral-src` 那一对接得上）；
    单播那条拷贝仍是清掉 cache-flush 位的（与临时口那条腿共用 `respond::unicast_of` 一个主人）。
    *电池 7 支*：N1 打上 from_qu 的那一步删掉 **2 红** / N2 去掉源端口前提 **1 红** /
    N3 把 recent 读反 **3 红**（第三条红的是 e10 —— 外来栈那支 QU 查询从 5353 进来，反向读法会把它
    整条改成单播）/ N4 `>` 改 `>=` **1 红** / N5 prune 按一秒 **1 红** / N6 `learn` 不存 TTL **2 红** /
    N7 单播腿不清 flush **4 红**。**没有一支 0 红**，本轮不需要那一格的新记录。
    *顺带收成一位主人的一个数字*：`MDNS_PORT` 之前在 `mdns.rs` 与 `announce.rs` 各声明一份
    （都是 `pub const … = 5353`）。两份字面量互相核对只会得到"它们现在一致"，而这条协议数字
    唯一会出事的方式就是有人只改一份 —— 附录 B 因此加了一条 `grep -c '^pub const MDNS_PORT'` 期望 **1**。
    *仍然欠着*：语料那 17 行的顺序（`NAME_GETTER` 排序）与 `is_stale` 那一族 cache 卫生，
    都是"没有读者"的偏离，不是待办。**本分支从未推送 ⇒ 这 730 条与 §31 的读数没有任何 CI 见证过。**
39. **`set-interface` 接进管理面，第一次把"钉网卡"这件事量到线上：文档跟着改了，出口没跟着改**（2026-10-20，
    `src-tauri/src/main.rs` 的 `Discovery` + `macast-upnp/src/http.rs` 的 POST 表第 **12** 行 +
    `macast-cast/src/announce.rs` 的 `send_group` / `pin_multicast_if` + `scripts/mdns_iface_listener.py`
    + 真机 §32 的 **17 条**；`macast-cast` 730 → **731**、`macast-upnp` 362 → **363**、
    `macast-tauri` 19 → **22**）：
    *把手只有一个，门有两个*：`Runtime` 不再自己持有 `ssdp` / `cast` / `announce` / `airplay`，
    这四样搬进 `Discovery`，窗口命令与 `set-interface` 那一行调**同一个** `set_interface`。
    顺序审计的形状因此改了一次：**"门读在每张脸之前"改为只看 `run()` 的函数体**（helper 在文件里
    第一次出现的那一行不再是顺序的证据 —— 它只在门之后被调用），"每类构造点恰好一个"仍按整文件数，
    并新增 `fn discovery_services(` / `fn start_announcement(` 各一个、`.restart(` 调用点恰好两个。
    *拒绝的判据要落在磁盘上*：查已知网卡表这一步会经 `Setting.get(Network_Interface, '')`，
    于是默认值进内存表 —— Python 一模一样（`utils.py:325-334` 走 `network_interfaces()`）。
    所以"拒绝之后键仍然不存在"这条断言会把下一位作者推向 `peek` 以取悦测试，判据改成**文件字节不变**；
    与 §24 那条"门这次读取不许造出键"看着同形，问句不同（那条问门自己的读法，这条问一次被拒的写有没有落盘）。
    *一支空变异体把概率性断言抓了出来*：e11 原钉"环回监听器在两份拷贝里只收到 1 份"，干净树绿、
    而 B5（`egress.push(*address);` 之后加 `let _ = address;`，零行为改动）**红**。判据换成读回
    `IP_MULTICAST_IF` 本身（必须等于 egress 最后一个，顺带问出"逐口旋转"而不是"设一次"），
    而"包真的从那块卡出去"交给真机。空变异的意义就是电池的自我校准：**改了零行为却改了判决 ⇒ 该修的是判据**。
    *产品事实是手工探针先量到的，§32 是把它变成判据的那一节*（那支 sniff 在 `/tmp` 未入库，入库的是
    `scripts/mdns_iface_listener.py`）：`set-interface=bridge100` 之后日志写 `joined 1 of 1 [192.168.139.3]` /
    `over [192.168.139.3]`，而加入该口的监听器收到 **0** 个包、突发仍在 192.168.1.5 上 ——
    合并后的引擎只 bind+join、从不设出口，内核按路由选卡。修法是每次组播逐出口设 `IP_MULTICAST_IF`
    并各发一份，正好等于参考实现"一块卡一个 socket、每串各发一份"的线上形状（§22 数到 6 个 socket），
    而这里仍只有一个 socket。答案形式也量了：`sock.rs:28-36` 说 BSD 要**下标**，实测**地址形式**在本机有效。
    修完 §32 的读数是 `3 datagram(s) of Macast-Rust-Iface heard on 192.168.139.3`。
    *两处只有真机能看的读数*（与第 24 条「门必须是真机判据」同一条）：把 `ssdp_reannounced` 钉死成
    `false`（A4）与少调一次 SSDP rebind 在单元层 **0 红** —— 那台测试实例的 SSDP 本来就没起，
    `false` 是正确答案。反之删掉 `set`（A2）与删掉 `restart`（A5）各红 2 / 1 条，
    且**红里都包含那条结构审计**（`set(Key::NetworkInterface` 恰好一个、`.restart(` 恰好两个）：
    行为断言看不见"少了一个调用点"，计数看得见。
    *仪器自己的两条*：① `struct.pack('4sl', …)` 的 Linux 口味在 macOS 上让 `IP_ADD_MEMBERSHIP` 回
    `EINVAL`，监听器死在加入组那一行 —— 若把 `hits 0` 读成"产品没发"，这一格就成了第二个 §25；
    现在它退出码 2 并打 `LISTEN_FAILED`，与"没有命中"分开说。② 名字过滤要取**本节自己**那行日志的
    实例名、且用线上形状（长度前缀标签里的裸名，没有点）—— 取 §22 那个会变成一支恒返回 0 的过滤。
    *电池 15 支逐支读数*：A1 1 / A2 2 / A3 1 / A4 **0（真机才有牙）** / A5 1 / A6（去掉 trim）1 /
    A7（`interface` 键改名）1 / B1（不设出口）1 / B2（只发第一口）1 / B3（被拒仍照发）1 /
    B4（空 egress 不发）1 / B5 **0（空变异对照）** / C1（门降级…实为升成 token）1 / C2（删掉那一行）2 /
    C3（面抢先 trim）1。每支一条命令、`/tmp` 备份 + 还原 + md5 回到基线，且**先 grep 证明那一处真的变了**。
    *一处刻意的加宽，记在 §8*：`set-interface` 的答复比参考实现多两个键（`ssdp_reannounced` /
    `mdns_reannounced`），因为这里是直接调用而不是引擎总线 —— 参考实现的发布拿不回任何答案，
    所以它只能说三格；那两个布尔是"结果"而不是"请求"，而把控件读成结果正是这一轮的全部动机。
    *仍然欠着*：`pin_multicast_if` 在非 unix 上是**明写的空操作**（Windows 这一路从未量过，
    那里出口仍随路由，§8 记一行）；5353 上仍是两个 socket（浏览器 + 引擎）；POST 表 12 / 23。
    **本分支从未推送 ⇒ 这 731 条与那 389 条真机读数没有任何 CI 见证过。**

40. **合并两只通配 socket 之前先量了一下"单播到底给谁"，结果这一问的前提塌了**（2026-10-20，
    `scripts/mdns_unicast_bind_probe.py`（新增）；**产品代码一个字节没改**，改的是一条已经写进
    §16 第 13 条、并且被写成任务 #40 的结论）：
    *原来那句话*：「单播整条只交给 reuse group 里的一个 socket，交给谁随流变」—— 从
    `mdns_reuseport_probe.py` 量来的，本身没错；但它被推成了一句"所以浏览器与引擎合成一只就对了"。
    写 #40 之前先照本端口的真实形状量一次（两只都绑 `0.0.0.0:5353`，一只是引擎、一只是每次搜索新开的
    浏览器），读数是这样，两条对照都先过：
    ```
    control, bound to '192.168.1.5':      6 of 6      ← 这台机器确实送得到 5353 的单播
    control, wildcard bind, group:        6 of 6      ← 组播仍进 joined 的通配口
    shipped shape, two wildcard binds:    A 0 / B 0   ← 我们两只都收不到
    bound AND joined on 192.168.1.5:      group 0 / unicast 6
    ```
    所以"合并两只"买到的东西是 0 + 0 = 0：**送到我们地址上的单播 mDNS 报文在进任何一只之前就被
    判给了别人**（本机上是系统自己的 `mDNSResponder` 握着确切地址的绑定 —— **这句解释当天晚些时候被
    §33 的读数否掉了**：场上没有任何确切绑定时 asker 得到的是"谁都不答"，而 app 绑上之后是 5/5 全答，
    所以通配口收不到是目的地址匹配的性质，不是有一只竞争对手在旁边偷，见第 41 条）。而第四行同时否掉了
    "把绑定从通配挪到接口地址"这条省事的修法 —— 绑了地址就不再收组播，两个角色必须两只 socket。
    *参考实现的形状正好是答案*（读 `.venv` 里那个包，不读 RFC）：`_utils/net.py:455-461`
    `new_listen_socket` 只有**一只**通配 socket，docstring 写着「used to receive multicast queries」；
    `_utils/net.py:415-455` `new_respond_socket` **一块接口一只**，注释写着「Bind explicitly to the
    interface address」。本端口只做了前者，所以"会听组播、听不到单播"。
    *今天咬到哪一层*：`mdns::query` 的 class 里**没有** `0x8000`，我们的浏览器不设 QU，设备答复走多播
    —— 所以设备下拉今天不受这一格影响（这句必须写出来，否则下一位会把它读成"投屏列表为空的原因"）。
    受影响的是**反向**：别人单播问我们，我们听不见、也就不答；以及任何一天我们把查询设成 QU，
    答复会掉进同一个洞。§25 那格 SKIP 与此**无关**（那三条查询是多播到达的，另一个进程确实看见了），
    这一行不许被引用成 §25 的答案。
    *#40 因此重新定义为*：给 `announce::bind_engine` 与 `mdns::open` 各加一只"绑到接口地址"的 socket
    （组播那只留着），读到查询就按同一张判决表答，读到答复仍进防洪账本；判据要能区分"我们听见了单播"
    与"仍然只有组播"，所以真机那一节必须用**单播**打进来（一支本地 socket 发给本机地址即可，不需要局域网）。
    **本分支从未推送 ⇒ 这四行读数没有任何 CI 见证过。**
41. **第二种 socket 落地，而它交付的第一样东西是这一轮自己那条判据不提问**（2026-10-20，
    `macast-cast/src/announce.rs`（`bind_unicast_listener` / `Engine::unicast` / `recv_any` /
    `start()` 的轮转）+ `announce_tests` 的 e12·e13 与一条装配审计 + `scripts/mdns_unicast_ask.py`
    （新增）+ 真机 §33 的 **8** 条；`macast-cast` 731 → **734**，整跑 **397 PASS / 0 FAIL / 1 SKIP，
    退出码 1**）：
    *形状*：`bind_engine` 在原来那只（通配 + join + 读）之外，**每个 offered 地址再绑一只**，
    不 join（第 40 条第四行量过：绑了地址就收不到组播，join 也买不回来），只进轮转；
    `recv_any` 是**轮转而不是 poll(2)** —— 本 crate 的 `libc` 只为 socket2 到不了的那一个
    `setsockopt` 存在，加一条 `WSAPoll` 分支就是加一条本仓库从未运行过的平台路径。每个 socket
    分到的是**剩余时间的均分**，所以总等待仍受调用方给的 `wait` 约束 —— 这不是修辞：§16 第 37 条
    那条循环的 `wait = min(poll, reannounce, queue)`，轮转一旦超时冲过头，排好的答复就晚于它自己的
    due time。
    *上一轮那句解释被这一轮否掉了*。第 40 条写的是"送到我们地址的单播在进任何一只之前就被判给了
    别人（本机上是系统自己的 `mDNSResponder` 握着确切地址的绑定）"。半句对（通配口确实收不到），
    半句没被问过：**场上没有确切绑定时，那个 asker 得到的是"谁都不答"**（2026-10-20 手工测：
    app 起来之前问 192.168.1.5:5353，4 秒内 0 份答复），而 app 绑上之后是 **5/5 全答**。
    所以不存在一个在偷报文的邻居 —— 通配口收不到是**目的地址匹配**的性质。§25 那一格因此再往
    里收一层：**别的进程发的单播到得了我们绑的口**，到不了的仍是组播，而那个原因这一轮还是没有名字。
    *判据三层，缺一层就有一个形状没人问*：
    ① `e12` 行为 —— 外来那份 40 字节的查询被轮转听见并被答，答复的**源端口**是 5353；
       同一条用例里带**对照**：把轮转只指向通配那只，1200 ms 内不许再看见同样的字节。
       没有对照，"第二只在场"与"第二只承重"是同一句话。
    ② `e13` —— 引擎那句总结里的地址表是**从 socket 读回来的**（`local_addr()`），不是抄 offered
       列表：offered 里放一张绑不上的卡（`192.0.2.1`，TEST-NET-1，三个平台都拒），句子里就必须
       没有它，而 WARNING 必须带着那张卡与操作系统的原话。第一版用的是 `127.0.0.53`，而 macOS 拒
       （EADDRNOTAVAIL 49）、Linux 放行 —— 那是 §4.8 Part 40 那条"判决由跑测试的机器决定"的形状，
       这次栽在一个刚写下的夹具里。
    ③ 装配审计 —— `start()` 的听众表必须由 `.extend(` 与 `engine.unicast` 一起构成，轮转只能有
       一个驱动点（`recv_any(&listeners` 恰好 1）。**这一条的第一版问的是"这一带有没有 `unicast`
       这个词"**，而 U3 那支变异体（把 extend 换成 `let _ = &engine.unicast;`，也就是"绑了但从不读"）
       交回 **0 红** —— 替换后的那一行仍然提到 `unicast`。电池抓到的不是我写的产品，是我一小时前
       写的判据（§4.2「不提问的检查」在这一族的第四次现形，也是第二次由我自己刚交的测试里挖出来）。
    *电池六支，逐支读数*（一条命令一支，`/private/tmp/bat40/` 备份 + `finally` 还原 + md5 回到
    `3908f1a7…`，且脚本在文件没变时**拒绝报告结果**）：
    U1 第二只改绑通配 **2 红**（e12 与 e13 —— 后者是因为句子会诚实地印出 `0.0.0.0`）/
    U2 一只都不造 **2 红** / U3 造了但不进轮转 **1 红**（修好判据之后的审计；修之前 0 红，见上）/
    U4 轮转只读第一只 **1 红**（e12）/ U5 句子改抄 offered 列表 **1 红**（e13）/
    NUL 一支**无意义的改动**（把 push 包进一个块，语义不动）**0 红** —— 这一支是电池自己的对照：
    它要是红了，上面五个数就都在读噪声。
    *一条没再现的读数留在记录里*：U5 第一趟除了 e13 还带出 **4 条 `relay_tests`** 红
    （"the latch records that we asked"、三处 spawn 计数翻倍），而 U5 重跑与**五趟干净整跑**都不
    再现。本轮不解释它也不修它 —— 但它必须留在这里，因为"某一支变异带出四条别的红"这句话一旦
    出现在读数栏里而没人标注复现与否，下一次就会有人拿它当连带证据（§4.2「两个见证者必须分开报」
    的反向形状：**没复现的读数不是读数**）。
    *仪器的另外两次自己的错，都长得像产品坏了*：① 新探针 `mdns_unicast_ask.py` 第一版按
    `record.name` 判"这份答复是我们吗"，而 **PTR 答复记录的 `.name` 是服务类型**、实例名在 rdata 里
    —— 它对着 stand-in 那份**正确**的答复报"3 份都不是我们的"。判据换成按**线上字节**找首标签
    （§25 那条"点号名在线从不存在"的同一条规矩），而这条理由写进了函数自己的 docstring。
    ② 那个 stand-in 夹具：这台机器的 zeroconf 是 Cython 构建，`DNSPointer(...)` 要 **5 个**位置参数
    （`name, rtype, class, ttl, alias`），少一个就 TypeError —— 栈打在仪器里，产品一个字没错。
    ③ e12 的判据也跟着换过一次：从 `answered`（把 `served.group + served.direct` 一起数）改成
    "听见的是**我发的那一份**字节"，因为组播口 join 的是真卡，场上邻居的查询也会让 `answered` 涨 ——
    那条判据本来在问"这台机器今天吵不吵"。
    *收尾时抓到的两条仪器缺陷，都不改产品*：④ `mpv_ask` 那 6 秒是**两次 recv 之间的静默上限**，
    不是总等待 —— mpv 只要继续吐事件，那个读循环就永不返回，而它占着的正是播放器那根 IPC 连接，
    于是后面每一条读都回空串，节里交出的却是**产品**红。判据改成按墙钟的总截止（8 秒，
    `ap_mpv_ask` 同一形状一起改），`except` 也从 `socket.timeout` 放宽到 `TimeoutError/OSError`。
    ⑤ 那一格真正的成因是**这台 Mac 的屏幕锁了**（`CGSSessionScreenIsLocked=True`，量于写这道门
    的当时）：锁屏之下 shell 写的 `loadfile` 与外部探针**直接写进播放器自己那根 socket** 的
    `loadfile` 都不被执行 —— 没有 `start-file`、没有任何事件、进程 0.0% CPU，而第二条连接问
    `get_property` 照旧答得出。§13/§14/§16/§21/§26/§27/§30 全都要读播放器，所以脚本现在起跑前
    问一次锁屏并 `exit 2` —— 与"端口被人占着就拒绝跑"同一族规矩：**测不出来的仪器不许打印判决**。
    这也把上面那个 397 的出处钉清楚：它是这一轮**唯一一趟屏幕没锁的整跑**（05:5x）读的，之后三趟
    都停在锁屏上，改的是仪器，不是产品，也不是判据。
    *仍然欠着，方向别读成"这一格结了"*：`mdns::open`（浏览器）仍然只有一只通配 socket，因为本端口的
    查询从不设 QU，第 40 条那句"设备下拉今天不受影响"仍然成立；答复的**源地址**由路由决定（§8 新增
    那一行，本机给不出价）；5353 上仍是两个 socket（浏览器 + 引擎），§16 第 13 条的另一半没做。
    **以及本分支从未推送 ⇒ 这一轮的 734 条与 §33 那 8 条读数没有任何 CI 见证过。**

42. **协议开关第一次能真正关掉一张脸，而这一轮最贵的一课是门审计没抓住我自己**（2026-10-20，
    `src-tauri/src/host.rs` 的 `set_protocol_enabled` 与 `Discovery::toggle_protocol` /
    `apply_protocols` / `{start,stop}_{cast,airplay}` + `macast-upnp` POST 表第 **13** 行 +
    真机 §34 的 **24** 条；`macast-tauri` 22 → **25**、`macast-upnp` 363 → **364**）：
    *形状*：参考实现的 `_toggle_protocol`（`protocol.py:2394-2402`）用「标题不在启用表里」决定方向，
    写侧 `set_protocol_enabled`（`macast.py:429-453`）四条规则逐行照抄（未知剔除而仍答成功、
    已在表里仍成功、停用不在表里答"没发生"、关最后一个以 `至少要保留一个启用的协议` 拒绝、
    按 `available_titles()` 排序落盘）。本壳把 **boot 与开关收成同一批构造函数**：
    `Discovery::start_cast` 拥有 8009 → 8008 的顺序、证书与每一句拒绝，`start_airplay` 拥有
    RTSP 与那根服务线程 —— 于是"只有启动进程才能开脸"这条形状消失了，`apply_protocols`
    读一次 `faces_enabled`、按它开关、再 `restart` 广播。**DLNA 那一格是拒绝的**，理由写进 §8。
    *这一轮最贵的读数*：重构之后 §24 的「关掉 Chromecast 的实例上 8009/8008 仍在答」**红了 7 条**
    —— boot 那次 `discovery.start_cast()` 漏了 `if cast_on`。三条结构审计（每张脸一个构造点、
    `run()` 里门在调用之前、文件级恰好两处读门）**一条都没响**：它们问的是"构造点在哪"，而构造点
    没变，只是不再被 if 包住。**一条真机读数抓住了它**，这正是 §24 那条「门必须是真机判据，单元
    判据只能问读法」的第一次反向现形 —— 这次不是单元测不到，是**审计的形状与破坏的形状不同**。
    修法一行，并补一条 `apply_protocols` 的"先读门再开关"的位置审计。
    *§34 自己有三条是测试的错*：① "只有幸存的协议重新广播"用绝对行数比（cast 3 / airplay 2），
    而 boot 已经印过两条 —— 改成步骤前后的增量。② 那个计数器本身也错：`announcing .*_googlecast`
    会命中 **AirPlay 那一行**，因为它的句子末尾拿 `_googlecast._tcp` 当"我没被量过"的宾语
    （`…the responder table was measured for _googlecast._tcp`）—— 与 §4.14「扫描器数到自己
    的文档」同一族，修成按**被公告的名字**匹配（`announcing [^ ]*_googlecast\._tcp\.local\.`）。
    ③ 小写标题那一格拿字节比中文，而正文是 `ensure_ascii` 的 `\uXXXX`（§9.3 那个后果的又一次），
    改成解析后再比 `message`。
    *另一个把自己的机器挡在门外的东西*：§34 第一次跑（以及它前面那次）**被一台我自己留在场上的
    冒烟实例**污染了 —— 它占着 8009/8008 并广播 `Macast-Rust-Toggle`，于是 §24 的"关掉还在答"、
    §25/§26 的浏览、§34 的 pid 断言一起红。脚本只拒绝自己的 `ApplicationPort` 被占，不拒绝 8009/8008
    被别人占（后者是每个 Macast 实例共用的固定口）。**收尾必须按 pid 杀掉自己的实例**，这条是
    AGENTS.md §4.2「起新实例前先杀干净」在验证侧的同一条。
    *电池*（一条命令一支，`/private/tmp/bat41/` 备份 + 还原 + md5 回到 `3f99d56e…`）：M1 删掉
    "最后一个不许关"那一段 → **1 红**（`the_last_enabled_protocol_cannot_be_switched_off`）/
    M2 删掉按 `available_titles()` 排序 → **1 红**（`toggling_a_protocol_rewrites_the_list_and_keeps_the_canonical_order`）/
    NUL 语义不动的空改动 → **0 红**（电池自己的对照）。**三支只有真机才问得到的没有逐支跑**（boot 的
    `if cast_on`、`stop_cast` 不再关 8008、`apply_protocols` 不再重建广播）：它们的见证是 §24 与 §34，
    而第一支这一轮**已经作为事故量过**（7 条 §24 红），另外两支没有单独读数 —— 写在这里而不是留成
    "都测过"。
    *读数*：`macast-tauri` **25**、`macast-upnp` **364**、`macast-cast` 734、core 128、mpv 149；
    真机 **421 PASS / 0 FAIL / 1 SKIP，退出码 1**（那一条 SKIP 仍是 §25 的送达那一格）。
    **本分支从未推送 ⇒ 这些读数没有任何 CI 见证过。**

43. **退出按钮真的会退出，而这一格的判据是"进程不见了"**（2026-10-20，`Discovery::app_action` +
    `macast-upnp` POST 表第 **14** 行 + 真机 §35 的 **9** 条；`macast-upnp` 364 → **365**）：
    `_app_action`（`protocol.py:2404-2414`）的形状照抄：**先答复、再退出** —— 参考实现起一根
    `MACAST_QUIT` 守护线程，这里起同名线程、250 ms 后 `app.exit(0)`，退出钩子照旧跑
    `stop_services`（byebye、goodbye、关面、停播放器）。`check-update` **拒绝**并说明（§8 那一行）：
    本构建没有版本检查，答「正在检查更新」就是让人去看一个不存在的进度条。§35 的三条判据只有真机
    能问：未知动词与 `check-update` 都**不动进程**（`kill -0` 对着 pid 问）、quit 的答复先到、以及
    "进程消失之后应用端口与 8009/8008 全都空着"——最后一条把"退出钩子真的跑了"从一句日志变成
    三个可以被别人 bind 的端口。读数 **430 PASS / 0 FAIL / 1 SKIP，退出码 1**（SKIP 仍是 §25 的
    送达那一格）。**本分支从未推送 ⇒ 这 365 条与 §35 那 9 条读数没有任何 CI 见证过。**

44. **播放器切换只认一个名字，而"接受它"仍然要有可观察的效果**（2026-10-20，`host::renderer_title` +
    `ShellManagement::set_renderer` + `macast-upnp` POST 表第 **15** 行 + 真机 §36 的 **5** 条；
    `macast-upnp` 365 → **366**、`macast-tauri` 25 → **26**）：参考实现的列表来自插件管理器，
    本壳只有一个内建渲染器，所以列表恰好一项（`Macast_Renderer`，空则 `MPV`）。**若只"接受自己、
    拒绝别人"，这个开关就只改文件**；接受时落盘（新装的 `''` 变成 `MPV`，正是下拉显示的那一行）
    并 `Player::reload()`，等价于参考实现 stop/start 同一个渲染器。reload 的可见那一半要有媒体在放，
    §36 因此**只问到答复与文件**，把"窗口重新起来"留给 §13/§14 那些真播放的见证者 —— 这一条写在
    那一节的输出里，而不是假装 pid 变过。读数填在 §15 那一行。**本分支从未推送 ⇒ 没有任何 CI
    见证过。**

45. **整份设置替换 + 重启：这一格只有进程能回答"它有没有回来"**（2026-10-20，`Setting::replace_all`
    + `ShellManagement::save_launch_param` + `macast-upnp` POST 表第 **16** 行 + 真机 §37 的 **8** 条；
    `macast-upnp` 365 → **366**）：形状照 `_post_save_launch_param`（`protocol.py:2640-2648`），
    答复与 §35 的 quit 同一条规矩——**先发出去、再重启**，否则那句 `success` 会和它描述的那个
    进程一起消失。两处要知道的：`app.restart()`（Tauri 2 的 `AppHandle`）在编译期就答得出有没有；
    而"新进程读的是新文件"只有真机能问——§37 用三个读数把它钉住（旧 pid 消失、同一个端口上换了一个
    pid、文件里的友好名变成新值）。**非对象被拒**是比参考实现严的一位（§8 那一行）。电池与文档
    见 §15 那一行。
    *电池*（定点、对着活实例的同一形状；`/private/tmp/host41.bak` 还原、md5 回到原值）：M1 删掉
    `replace_all`（整表写）→ 重启后文件里仍是旧名（`Before`），§37 最后一条会红 —— **一次真红**。
    M2 **把重启挪到答复之前**（同型的反方向，端口干净重测）→ curl 拿到 **HTTP=000**（没有答复），
    而进程确实重启了、文件也确实变成了 `After` —— **一次真红**，红在 §37 的第一句
    「好文档回 `success`」上。两次读数合起来说明这一节的牙不在"文件变了没有"，而在"答复先不先到"。

---

## 18. 插件子系统：这个壳的形状（设计，2026-10-20）

参考实现的插件是**单个 `.py` 文件**：`MacastPluginManager` 用 `importlib` 导入、按清单里的
`<macast.*>` 取标题/版本、启用/停用/卸载/安装都围绕这件事。**本壳没有 Python 运行时**，
所以"移植插件子系统"不能照抄这条链。决定如下，先落纸再写代码（todo 2）：

* **不引入脚本宿主。** 嵌 Python / WASM / Lua 任何一条都会把一个"把 URL 交给播放器"的接收端
  变成一个**代码执行平台**，而它的权限模型（§4.7b/§4.7c）现在是按"只有三行 POST 会落代码"
  写的。这不是这一轮能顺手做的事，也不是把 Macast 变回可用的必要事。
* **于是"插件"在本壳里只有两层**：**内建渲染器表**（一项：mpv，`host::renderer_title`）与
  **管理面对插件词汇的答复**。前者已经能切换（§16 第 44 条）；后者的原则是**说清"这个构建
  装不了东西"，而不是 404 或空成功** —— `plugin-info` 从 §16 第 33 条起就是这么答的（空列表
  是"从这个构建里装不了"的答案）。
* **七行的形状**（全部沿用参考实现的字段名、门与答复文风；实现在 todo 3）：

  | 字段 | 参考实现的行 | 门 | 本壳的答复 |
  |---|---|---|---|
  | `plugin-enable` | 2 | management | `code: 1`，点名"这个构建里没有可启用的插件"，并说明内建渲染器不经过这张表 |
  | `plugin-disable` | 3 | management | 同上（停用不是"没找到"，而是"没有这个概念"） |
  | `plugin-uninstall` | 4 | management | 同上 |
  | `install-plugin` | 11 | **code-execution** | `code: 1`，点名"这个构建没有插件宿主，装进来的 `.py` 不会被加载"；**不落盘、不 import** |
  | `plugin-file`（上传） | FILE_ROUTE | **code-execution** | 同 `install-plugin`，先拒再读正文（与 §4.7b 的 `plugin-file` 同一条规矩） |
  | `install-webrtc-extras` | 12 | **code-execution** | `code: 1`，点名本壳没有 extras 安装器（`extras_urls` 是纯规则，见 §16 第 34 条） |
  | `uninstall-webrtc-extras` | 13 | **code-execution** | 同上 |

* **门一律照抄、一行不降**：这七行里有四行在参考实现里就能落代码；即使本壳今天不执行，
  把它们装进 management 门就等于给"以后接一个宿主"留一条无门的路（§4.7c 的"门是数据"）。
* **判据按"这一行说了什么"分三层**：单元（每行一条：答复原话 + 门 + 不写盘）、表循环（16 → 23
  行，每行仍被无令牌局域网与带令牌各打一遍）、真机 §38（三条 code 门在局域网上不带令牌照样被拒、
  两条 plugin-enable/disable 在本机被拒且设置文件字节不变、`plugin-info` 的列表仍为空）。
* **什么会改变这个决定**：用户明确要求插件宿主（那时先做权限模型，再做宿主），或者某个内建插件
  的能力被真的需要（那就把它按**原生**实现进壳，而不是开一个脚本口）。

**这一节的边界**：它只决定"答复什么、门放哪、怎么判"，不声称插件能装。页面上那张卡仍然显示
"这个构建里没有可安装的插件"——`plugin-info` 已经在这么说（§15 那一行）。

46. **插件那七行落地：先答"不能"，再答"为什么"**（2026-10-20，§18 的设计 + POST 表 16 → **22** 行 +
`plugin-file` 的文件路由 + 真机 §38 的 **7** 条；`macast-upnp` 366 → **367**）：六行表单端点各自
`code: 1` 并点名按钮；`plugin-file` 走**文件**路由但同样在**读正文之前**拒绝，门是
`code-execution`；四行在参考实现里会落代码的门一行没降。整跑 **450 PASS / 0 FAIL / 1 SKIP，
退出码 1**。**这一格最贵的一条**不是代码，是环境的第二次同一个坑：§38 第一次跑被一台我留下的
实例污染（`Before` 那台，占着 8009/8008，§24/§26 一起红）—— 收尾杀实例是按 8009/8008 的 pid
收的，不是按模式。**本分支从未推送 ⇒ 这些读数没有任何 CI 见证过。**

47. **§16 第 13 条的另一半：先量清楚"浏览 socket"到底是什么**（2026-10-20，只测量，未改代码）：
    今天的形状是 `mdns::discover_cast`（`mdns.rs:1531`）**每次搜索开一只 socket**（`open()` →
    `browse(&mut wire, …)` → 丢弃），`browse` 本身只吃 `&mut dyn Wire`。所以"合并两只 socket"不是
    把两处读者拼起来，而是**把这只临时 socket 换成长期在场的那一只**（缓存 `open()` 的结果）。
    下一轮的补丁因此是三点：① `open()` 的结果放进 `OnceLock<Mutex<Multicast>>`，搜索用它；
    ② 为了能问"到底开了几次"，`open()` 增加一个 `#[cfg(test)]` 的计数器，测试先断言**今天**是
    每次一开、改完是**一次**；③ 风险写清楚：缓存会让两次搜索之间的报文**留到下一次读**（这既是
    "吸收听见的答复"的一半，也可能让下一次搜索先看见旧答案）、而且换网卡之后组播成员是要重建的
    （`set-interface` 已经会重建引擎，浏览缓存必须跟着失效）。**§33 的单播保证不受影响**：它证明的
    是引擎那只长期 socket，被换成缓存的只是搜索用的临时那只。
    *落地与第二条决定*（同日）：缓存已经实现（`BROWSER` + `with_browser` + `forget_browser`，
    `macast-cast` 735 条）。**"把听见的答复并进设备表"这一半决定不做**，理由是表今天替一条**活着的列表**的语义在服务：每次搜索用新结果**替换**缓存（`targets.rs:951`），于是"不再应答的设备"
    会从下拉里消失；把它改成"合并"而不带 TTL，一台关掉的电视会永远留在列表里 —— 参考实现的
    zeroconf 缓存靠 **75 分钟 TTL** 老化，我们没有那套时钟。要做就得给 `Inner` 加每行的时间戳、
    让渲染与查找都带它，那是另一轮；在它落地之前，"合并"比"替换"更糟。缓存本身已经带来一半的
    吸收效果：两次搜索之间到达的报文会在下一次 `browse` 里被读到（因为 socket 不再重建）。
    **本分支从未推送 ⇒ 无 CI 见证。**

48. **§25 那一格收窄成"同一台机器上的进程"，而跨机器的问与答今天有了读数**（2026-10-20，无代码改动）：
    一台 **CachyOS**（192.168.1.68，ssh -p 10086）发了 3 条组播 + 3 条单播到 `192.168.1.5:5353`，
    app 进程的 `logs/Discovery.log` 收下 **6 条** `heard query … from 192.168.1.68:…`；再发一条单播并
    等答复，收到 **286 字节、来自 192.168.1.5:5353、正文里带 `Macast-Rust-LAN`** —— 也就是说
    "别人问，这个壳答"在跨机器上是成立的。§25 一直 SKIP 的那三条查询是**同一台 Mac 上的另一个
    进程**发的，所以那一格要改的话，是把它从"这个 responder 在局域网上没人听见"收窄成"本机另一个
    进程发的查询没被交给它"——后者对用户没有可观察面，而前者已经被上面两行读数否掉。
    **Windows 那一半（§16 第 39 条的 `pin_multicast_if` 空操作）仍然没量**：.68 的 22 端口
    `Connection refused`，这台 CachyOS 是同一 IP 的另一台机器；要量它得等 Windows 那台的
    sshd 回来。**本分支从未推送 ⇒ 无 CI 见证。**

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
#   2026-10-20 最后一趟两边都是 **702**（AirPlay 的 RTSP 那一轮 +5，`_airplay._tcp` 的广播这一轮 +4：语料对拍 /
#   "广播但绝不答问" / AirPlay 的拒绝句点名 AirPlay / 答问那半是派生的不是写死的）。这里故意不写成一个可以照抄的常量：上一版写的 656
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
grep -c '"queue": True' scripts/mdns_responder_probe.py                     # 期望 **4**（聚合行；判决语料必须把它们排除在外，
#   而排除是数过的：不是 4 条探针就退出 1 —— 这一行是那个数在文档里的对应物）
grep -c '^echo "== ' scripts/verify_rust_receiver.sh            # 期望 **41**（§38 是最后一节：
#   插件那七行：六行表单端点回答"这个构建没有宿主"，`plugin-file` 在读正文之前就被拒，
#   四行 code-execution 的门照抄（§18 的设计、§16 第 46 条）。§37 是它前面那一节：
#   整份设置替换 + 重启：两种坏输入不写盘也不动进程、好输入先答复再重启、新 pid 在同一端口
#   读新文件（§16 第 45 条）。§36 是它前面那一节：
#   渲染器切换只接受这个构建真的有的那一个名字，拒绝的是参考实现的原话（§16 第 44 条）。
#   §35 是它前面那一节：
#   页面上的退出按钮真的退出：答复先到、进程随后消失、每个监听口都跟着走、SSDP 说了 byebye
#   （§16 第 43 条）。§34 是它前面那一节：
#   协议开关在同一个进程里关掉 8009/8008 与那份广播、再开回来（§16 第 42 条）——
#   §24 只能问门的**读**侧：它靠手写列表起第二台实例，问不出"开关自己会不会动"。
#   §33 是它前面那一节：
#   另一个进程**单播**问本机地址，shipped 进程必须答 —— 这是这个 responder 第一条跨进程见证，
#   也是 §16 第 41 条那一格的判据来源。问与读都由 `.venv` 里那套 zeroconf 完成
#   （`scripts/mdns_unicast_ask.py`），所以"我们的报文别人认不认"不是自证。
#   §32 在它前面：`set-interface` 真的动了广播，而"动了"是**按线上看到的报文**判的 ——
#   `scripts/mdns_iface_listener.py` 加入被钉住的那块卡，读回三份突发（§16 第 39 条：
#   那一节首跑量到的正是"日志改了、出口没改"）。
#   §31 是它前面那一节：
#   引擎有没有听见自己发出的那份答案 —— 只有真机能答这一问，因为账本喂它的字节来自内核环回。
#   §30 是那两行 POST（清空与字幕显隐）打到真播放器与真文件，§29 是插件仓库坐标的跨实现对拍，
#   §28 是「哪个键属于谁」那张表的对拍；§27 是重投历史那一节，重启之后还在；
#   §26 是 iPhone 的 RTSP 问答，§25 是 responder 的局域网那一节，§24 是门。
#   §23 是 2026-10-18 的 status 一节，门那一节当时从 §23 挪到 §24 —— 本文、脚本注释与
#   `AGENTS.md` 里指"那一节"的句子一起跟着改了编号，编号变了而判据没变）
# §31 之前，聚合与账本的两条"只有一个主人"锚：
grep -c 'pub fn production_draw' macast-cast/src/aggregate.rs   # 期望 **1**（生产延迟的主人；
#   用例抽样的是这个函数，不是 `Outgoing::new` 里再抄一份表达式 —— 抄的那份证明的是测试自己）
grep -c 'Outgoing::with_random(Box::new(production_draw))' macast-cast/src/aggregate.rs  # 期望 **1**
grep -c 'written: HashSet' macast-cast/src/respond.rs           # 期望 **0**（去重只有一份 `sending`；
#   第二个集合是 M11 那一支 0 红的成因，见 §16 第 37 条 —— 两条规则在测试眼里是同一半）
#   同一轮把 `MDNS_PORT` 收成一位主人：两份 `pub const … = 5353` 互相核对只会得到"它们现在一致"，
#   而这条协议数字出事的方式恰恰是有人只改一份 —— 所以问的是**个数**，不是值）
grep -c '^pub const MDNS_PORT' macast-cast/src/mdns.rs macast-cast/src/announce.rs | grep -c ':1$'  # 期望 **1**
grep -c '^echo "== ' scripts/verify_rust_receiver.sh            # （旧值 33；§28 那一节：
#   两份实现各自把「哪个键属于谁」这张表写一遍，再逐格对拍；§27 是重投历史那一节，
#   重启之后重投历史还在；§26 是 iPhone 的 RTSP 问答，§25 是 responder 的局域网那一节，§24 是门。§23 是 2026-10-18 的 status 一节，
#   门那一节当时从 §23 挪到 §24 —— 本文、脚本注释与 `AGENTS.md` 里指"那一节"的句子一起跟着改了编号，
#   编号变了而判据没变）
grep -c 'a query that is not ours' macast-cast/src/announce.rs        # 期望 **1**（DEBUG 那半句也只有一处；
#   两级句子是 §16 第 26 条那一格：INFO 只报问我们的，预算才不会被本机自己的查询淹死）
grep -c 'fn concerns' macast-cast/src/respond.rs                       # 期望 **1**（复用出答案的那个匹配器，不是第二份名字规则）
# 出口与开关的三条"一位主人"锚（§16 第 39 条）：
grep -c 'set_multicast_if_v4' macast-cast/src/announce.rs              # 期望 **1**（平台形状只在这一个函数里；
#   第二份 `set_multicast_if_v4` 意味着有人绕开 `send_group` 直接组播，而那正是"出口与出口列表分家"的形状）
grep -c 'runtime.discovery.set_interface(&name)' src-tauri/src/main.rs  # 期望 **1**（窗口那一扇门）
grep -c 'self.discovery.set_interface(name)' src-tauri/src/host.rs       # 期望 **1**（HTTP 那一扇门）
#   两行合起来才是那句"一个实现两个门"：只数 `set_interface(` 会把用例里的四次调用也数进来，
#   而一个把测试自己算进去的计数，随写测试的密度涨 —— 与上面 `CastChannel` 那一行同一族教训。
grep -c 'field: "set-interface"' macast-upnp/src/http.rs               # 期望 **1**（表里那一行；`== 12` 的行数判据在
#   `the_post_route_table_is_well_formed` 里，这里是它在文档的对应物）
# 第二种 socket 的三条"一位主人"锚（§16 第 41 条）：
grep -c 'fn bind_unicast_listener' macast-cast/src/announce.rs         # 期望 **1**（构造函数只有一个；
#   第二处 `bind` 到接口地址意味着有人绕开 `Engine::unicast` 自己养一只没人读的 socket）
grep -c 'recv_any(' macast-cast/src/announce.rs                        # 期望 **2**（定义 + 产品里唯一那个驱动点；
#   第三个就是"有两条读路径，其中一条看不见第二只"的形状 —— 单元审计与 `the_reading_thread…` 各问一半）
grep -c 'it also hears unicast queries on' macast-cast/src/announce.rs scripts/verify_rust_receiver.sh | grep -c ':1$'  # 期望 **2**
#   （写这句话的地方与读这句话的地方各恰好一处。§33 与 e13 比的是**同一个字面**，所以格式串里的换行
#   续行不能把这个词组拆开 —— 拆了之后两侧都还在，只有这条锚会响，而它响的时候读者以为产品坏了）
grep -c 'AirPlay SET_PARAMETER' macast-cast/src/airplay.rs             # 期望 **2**（"两个数字都说" 与 "读不懂也要说"）
# 一个 socket、N 个 service（§16 第 32 条）：这三行合起来说"引擎只有一个、它 join 的是并集、
# 两个 service 的构造点各一个" —— 三处一起变才是对的，只变一处就是有人把合并拆了一半。
grep -cF 'announce::Announcer::new(' src-tauri/src/main.rs              # 期望 **1**
grep -c 'fn join_addresses' macast-cast/src/announce.rs                 # 期望 **1**
grep -cF 'announce::Service::from_setting(' src-tauri/src/main.rs       # 期望 **1**
grep -cF 'announce::Service::airplay_from_setting(' src-tauri/src/main.rs  # 期望 **1**
# 壳自己的启动总结走 root（= macast.log），播放器的句子留在 MPV —— 分家不是搬家。
# 判据在 `the_shells_own_summary_speaks_through_the_root_log`（文本）+ 真机 §22 的三行（按文件名）。
grep -c 'let app_log = macast_core::logging::root();' src-tauri/src/main.rs  # 期望 **1**
grep -cF 'does not answer it' scripts/verify_rust_receiver.sh             # 期望 **1**（§26 问"这行有没有说出
#   会广播不答问"，措辞在 §16 第 32 条改过一次；旧 needle `it announces only` 留在历史里，别再找它）
grep -c 'self.persist_history();' src-tauri/src/host.rs                 # 期望 **2**（投完一条、清空一次 —— 写盘只有这一处）
grep -c 'PROTOCOL_LOG' src-tauri/src/host.rs src-tauri/src/main.rs       # 期望 **3 + 1**（名字的主人与唯一的 claim 点；
#   §12.1b 那一格说的是"目的地是一处决定"，而这一行是它的外部对应物 —— 谁在别处再 claim 一次，
#   `claim()` 的先到先得就会让句子的目的地变成一次赛跑）
grep -c 'AirPlayReceiver::new' src-tauri/src/main.rs                   # 期望 **1**（AirPlay 那张脸的构造点恰好一个）
grep -c 'pub fn announcement\|pub fn goodbye' macast-cast/src/announce.rs  # 期望 **4**（两种顺序 × 两种报文）
grep -c 'net::advertised_addresses(' macast-cast/src/announce.rs            # 期望 **1**，且下面这条 **0**：
grep -c 'net::advertised_hosts(' macast-cast/src/announce.rs                # 宽集合回到 A 记录就是 §4.1 第一行
grep -c 'advertisable(setting, log)' macast-cast/src/announce.rs            # 期望 **2**：两个 builder 走同一个主人
#   （`from_setting` 与 `airplay_from_setting`），而 `report_clipped(&service, log)` 也**恰好 2** ——
#   「一个服务类型一个字段，不是一个服务类型一份实现」这句话的外部对应物：哪一天第三个类型进来，
#   这两对数字一起涨才是对的，只涨一处就是有人抄了第二份实现
grep -c 'responder_answers' macast-cast/src/announce.rs                     # 期望 **3**（定义 + `start()` 里那次派生 +
#   模块内那条文档引用）。这一行钉的是"答不答问只有一个判定点"；而 `run()` 里那个 `if answers` 是
#   **被 obeyed 的**（M5：换成 `if true` → 1 红），两件事各有各的用例
grep -cE 'discovery_log\.(info|warning)\(' src-tauri/src/main.rs            # 期望 **5** = 下面三条 + 协议开关那句（§16 第 42 条）
#   （2026-10-20 引用体检抓到这一行**在本轮之前就已经落后**：它写的 5 与 2/3 是"Cast 与 AirPlay 各
#   一条总结"那个形状 —— 而 §16 第 32 条把两个服务合并进**一个** announcer 之后，总结句按 service
#   类型在运行时循环，源码里只剩一条字面量。一个按"有几处字面"计的数，会在合并之后仍然答对问题
#   而答错自己，所以这一行现在钉的是**四句话**：1 announcing + 2 not-announcing + 1 网卡改动）
grep -c 'mDNS: announcing' src-tauri/src/main.rs                            # 期望 **1**（一句，运行时按类型各印一次）
grep -c 'mDNS: not announcing' src-tauri/src/main.rs                        # 期望 **2**（"8009 没在听" + 点名拒绝原因那一条）
# 值语义一座孤岛（§7 的两层，2026-10-20）：`truthy` 的规则体在整棵树里必须**恰好一处**，
# 而表单开关**故意两处**（参考实现自己有两种拼法：`remove`/`set-github-mirror` 不 strip，
# `?refresh=` 要 strip）。这两条 grep 是 `each_value_rule_has_one_body_in_the_workspace`
# 的外部对应物 —— 电池杀不掉它们（副本会替被杀那一份答话），所以只有扫描问得出"有几份"。
grep -rn 'Value::Null => false' --include='*.rs' macast-core/src macast-mpv/src \
  macast-upnp/src macast-cast/src src-tauri/src | wc -l                       # 期望 **1**
grep -c 'trim().to_ascii_lowercase()' macast-core/src/*.rs | awk -F: '{s+=$2} END {print s}'  # 期望 **0**
# 坐标只有一个主人，两个读者（GET 的 describe、POST 的成功文档）都从它走。
grep -c '^pub fn describe' macast-core/src/plugin_repo.rs                      # 期望 **1**
grep -c 'plugin_repo::describe(&self.setting)\|plugin_repo::set_mirror_answer(' macast-upnp/src/http.rs  # 期望 **2**
grep -c 'field: "set-github-mirror"' macast-upnp/src/http.rs                   # 期望 **1**（表里那一行；
#   行数由 `the_post_route_table_is_well_formed` 的 `== 11` 在管，这里是它在文档里的对应物）
# 两行新门各有各的构造点：一次转发、一个账本口，两处都不许多出第二份。
grep -c 'field: "set-subtitle-show"\|field: "clear-play-history"' macast-upnp/src/http.rs  # 期望 **2**
grep -c 'fn clear_history' macast-upnp/src/store.rs src-tauri/src/host.rs                   # 期望 **1 + 2**
#   （trait 一处；壳里"固有方法 + trait 转发"两处 —— 那两件事必须分开：落盘的方式只属于表自己）
grep -c 'fn set_sub_show' macast-upnp/src/http.rs src-tauri/src/host.rs                     # 期望 **2 + 1**
#   （trait 声明 + HTTP 测试双 + 壳那一条转发。"布尔→yes/no" 那半句在 `macast-mpv`，名字是
#    `set_media_sub_show`，本机量过它只有一处定义 —— 两个名字不是两位主人）
#   这三行合起来是一条**目的地**判据：壳自己的 mDNS 句子一条都不许再骑在播放器的 logger 上
#   （§16 第 29 条 ④ —— 它们以前全在 `logs/MPV.log` 里，而 §5 那条排障配方 grep 的是 `macast.log`）。
#   真机那侧的对应物是按文件名读：
grep -cF "grep -ahF 'mDNS: announcing ' " scripts/verify_rust_receiver.sh   # 期望 **5**（§22、§26、§32、§33、§34 各一次，
#   都只读 `logs/Discovery.log`；`log_grep` 那种"在所有日志里找"的宽度对**否定**断言是对的、
#   对**目的地**是错的 —— §27 立的同一条规矩。§32 起这一格按**服务类型**再筛一次：两个 announcer
#   一存在，`tail -1` 就是在随机挑一个类型（§16 第 33 条那 7 条假红的成因）
grep -cF "grep -ahF 'mDNS: not announcing' " scripts/verify_rust_receiver.sh # 期望 **3**
grep -c 'does not answer it' src-tauri/src/main.rs                         # 期望 **1**（"会广播 ≠ 会答问"这句话
#   必须活在日志里；§26 有一条就是问它在不在，M8 那一支改坏它的第一次读数**无效**，原因见 §16 ④）
grep -cF '_airplay._tcp.local.' scripts/verify_rust_receiver.sh             # 期望 **4**（§24 的浏览 + 它的缺席分支 +
#   §26 的 TYPE + 它的类型断言）—— §24 现在两种类型都听，所以"关掉 Chromecast"这一问同时管住两条广播
grep -c 'set_multicast_loop_v4(true)' macast-cast/src/announce.rs            # 期望 **1**（发送侧，参考实现的 responder socket 也开环回）
grep -c 'join_multicast_v4' macast-cast/src/announce.rs                      # 期望 **1**（2026-10-18 起：
#   responder 接线后那个长期在场的口 join 且读，见 §16 第 13 条与 §14.6；这一行以前钉的是「广播口
#   故意不入组」，那个形状已经反向 —— 现在**不**加入组才是缺陷）
grep -c 'pub fn faces_enabled' src-tauri/src/host.rs                          # 期望 **1**（门只有一个主人）
# 应答这一格的三句话，各自只有一个主人（§14.6）：
grep -c 'mDNS engine heard query' macast-cast/src/announce.rs       # 期望 **1**（写句子的地方只有一处）
grep -c 'mDNS answered its first query' macast-cast/src/announce.rs # 期望 **1**（送达与判决是两句，不是一句）
grep -c 'HEARD_SENTENCES' macast-cast/src/announce.rs                # 期望 **6**（定义 + 两道上限判断 + 两处打印
#   + 文档里两处引用）；用例里的期望写的是**字面 8**，不是常量 —— 同源等式对任何常量都成立，
#   见 §16 第 22 末、第 26 条与 §13.4
grep -c 'x0b_googlecast' scripts/verify_rust_receiver.sh             # 期望 **1**：§25 的监听器按**线标签**过滤；
# `log` 门面这一格的两道闸（§12.3）：依赖不在图里是**编译错误**，源码扫描是它后面那一层
grep -c '^log = ' macast-core/Cargo.toml macast-mpv/Cargo.toml macast-upnp/Cargo.toml \
  macast-cast/Cargo.toml src-tauri/Cargo.toml | grep -v ':0' ; echo "五份 manifest 的 log 声明数应为空"
grep -rc '^use log::' macast-core/src macast-mpv/src macast-upnp/src macast-cast/src src-tauri/src | grep -v ':0' ; echo "同上，源码里也不许出现"
#   注意后一条在 (前一条成立) 时永远为空 —— 它是给"哪天有人把依赖加回来并解释为什么"那一天读的
#   带回族的教训是这条针眼曾经写成带点的 `_googlecast._tcp`，于是满组查询被数成 0（§14.6 ③④）
grep -c 'host::faces_enabled(' src-tauri/src/main.rs                          # 期望 **2**（每条路径一次：boot 的 `run()` 与开关的 `apply_protocols`；一次读取管三张脸；
#   写成两处就会被 `every_cast_face_is_behind_the_gate` 的计数断言拒绝，这条 grep 是它在文档里的对应物）
grep -c '"DLNA_FriendlyName":' scripts/verify_rust_receiver.sh                # 期望 **16**（主实例 + §24 门 + §26 AirPlay + §27 历史 +
#   §28 模块设置 + §29 插件坐标 + §30 字幕与清空 + §31 听见自己 + §32 网卡 + §33 单播 —— 每一个自带实例的
#   一节都要给自己一个名字，否则它广播的是别人那台的名字，按名字匹配的检查就成了隔壁的证人），且下面这条 **0**：
grep -c '"FriendlyName":' scripts/verify_rust_receiver.sh                     # 期望 **0**：那是**没有读者拥有的键**
#   （`Key::DlnaFriendlyName` 映射到 `DLNA_FriendlyName`，`setting.rs` 那一行），2026-10-18 它让 §24 的
#   "线上没有广播"这句答复来自一个拼写错误而不是门 —— §16 第 12 条 ②
grep -c '^        query: "' macast-upnp/src/http.rs           # 期望 **16**（行首锚住：不锚就会数到
#   本文件里那条**引用这条 grep 自己**的注释（6101 行），于是扫描器把文档当成一行路由，
#   报出 17 —— 与 §4.14 那条"会对文档变红的扫描器"是同一族，这次红的是文档自己的引用）
#   GET 表：cast/cast-info/
#   resolve-status/resolve-cookies/cast-targets/status/interfaces/subscribers/log/log-modules/
#   log-download / launch-param / module-settings / plugin-info / mirror-state / mirror-snapshot
#   —— 参考实现的十五个 GET 全部有了答复（plugin-info 与两张 mirror 是"这个壳没有那一面"的
#   完整文档，不是 404）。这个数**不是地板是判据**：`the_get_route_table_is_well_formed`
#   钉的是 `len() == 16`（`http.rs:6104`；§16 第 29 条那一格的形状 —— 地板比真值低一行，删掉一行照样过，而删掉的正是"这张表
#   还在不在"这一问），所以改这张表的人两处一起动，文档里的这一行才有意义）
grep -c 'entry!(' macast-core/src/module_settings.rs                    # 期望 **27** = `Key::all().len()`，
#   而 `every_key_in_the_table_is_owned_by_exactly_one_group` 钉的是"两张表加起来恰好等于键表"
#   这个**双向**问句（有键没标签 = 漂移；有标签没这个键 = 表在指向一台机器写不了的键）
grep -c 'module_settings::' macast-upnp/src/http.rs                     # 期望 **3**（读一张文档 + 写侧两处：
#   `set_value` 与 `remove_flag_is_true`）。这一行以前钉的是 **1**，理由写的是"这张脸只有一个读者，
#   写侧移植时必须叫同一个名字" —— 写侧来了，名字也确实是同一个，所以数变了而规则没变：
#   **一张归属名单，两个门都问它**（§4.7c 的理由）。留这一句是因为"1"那个数读起来像规则本身
grep -cE 'pub fn (groups|payload)|fn is_claimed' macast-core/src/module_settings.rs  # 期望 **3**
#   （表 → 组 → 文档，各一个主人；`is_claimed` 是"这一格属不属于某张表"的唯一问句）

grep -rc 'fn header_emissions' macast-upnp/src/http.rs          # 期望 **1**（CORS 不变式的扫描器在这里，
#   它扫的是三个 crate 的所有 `.rs`；`the_cors_scanner_answers_when_a_cors_header_exists` 是它的合成正控，
#   删掉"注释行不算"那一支就会红 2 条 —— 见 §16 第 19 条）
grep -c 'PostRoute { field:' macast-upnp/src/http.rs            # 期望 **22**（POST 表：`set-github-mirror` /
#   `install-*` 之外那批里已移植的 12 行，含 `clear-log`、`set-module-setting` 与 `set-interface`；
#   同一形状的两条计数断言在 `the_post_route_table_is_well_formed` 里：`len() == 22`（`http.rs:6264`），
#   加上"门那一列里 `code-execution` 恰好一行" —— 安全上的区分要能被数出来。
#   参考实现那张表今天是 **23** 行（2026-10-20 逐行数出来的是 23，`protocol.py:2287-2331`；
#   `AGENTS.md` §4.7c 原文那句"16 条"是它写下时的数，已在原地标注）—— 12 与 23 之间的 11 行都等插件子系统）
grep -c 'gate: "code-execution"' macast-upnp/src/http.rs                  # 期望 **5**（`set-module-setting`
#   与 `save-launch-param`：两件的输入都会改变"什么会被执行"——一个写钩子的键，一个整份替换设置；
#   测试里那个"恰好两行"的断言在 `the_post_route_table_is_well_formed`）
grep -c 'const CODE_REFUSAL' macast-upnp/src/http.rs                      # 期望 **1**（拒绝文案只有一个主人，
#   §4.7c 那条「一条路由不能自己写拒绝文案」在这一行上是它的外部对应物）
grep -c 'pub fn set_name\|pub fn remove_name' macast-core/src/setting.rs  # 期望 **2**：按名字的写与删，
#   是 `set(Key, …)` 表达不了的形状（用户手写的键、将来插件的键）；两个都**不在场时不写盘**
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
grep -c '^pub fn datagram' macast-cast/src/respond.rs   # 期望 **2**（`datagram` 与 `datagram_multi` ——
#   §16 第 36 条那一格：装配层的两个入口共用同一个 `Writer`，而"一条答复里两个问题各交一份 host pair"
#   那条产品 bug 只能由多入口那一个修。以前钉 **1**，是 `datagram_multi` 落地之前写的数）
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
