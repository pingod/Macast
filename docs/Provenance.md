# 来源台账（Provenance）

> 机器可读的账目合计在下面的 `provenance-ledger` 注释行里，由
> `scripts/verify_cast_airplay.py` **Part 34** 与 `scripts/provenance.py --json` 双向比对。
> 改完任何 `.py` 的归属之后请三个动作一起做：
> `env -u PYTHONPATH .venv/bin/python scripts/provenance.py --check --stamp` →
> 更新本文件的表 → 跑套件。少做一步，Part 34 就会变红。

<!-- provenance-ledger: fork=19879235ef98a64b813de968306bc91a0d663518 files=54 upstream=3 vendored=6 mixed=12 ours=33 upstream_lines=6587 our_lines=35628 -->

## 0. 这份文档存在的理由，以及它不做的两件事

本仓库是 `xfangfang/Macast` 的 fork。"这是 fork"这件事成立的时候，
署名就不是格式问题而是**陈述**：

- 上游的代码还在文件里，就把上游的声明删掉 —— 这是对代码来源的虚假陈述；
- 一行都不是我们写的文件，盖上我们的名字 —— 这是对作者身份的虚假陈述。

所以本仓库的做法是：**度量，然后按度量结果补声明，只加不减**。
用户提出的"去掉 fork 与所有原项目信息"这一半不做（`AGENTS.md` §4.6、§5 已记录该决定）；
要做的那一半 —— 把自己的代码逐步重写出来 —— 有队列、有判据，就在下面 §5。

**它也不做的第二件事**：把重写当成换许可证的手段。GPLv3 下，只要产物仍是原始
作品的衍生作品，重写其中一部分并不改变整体的许可证；真正能脱离的只有**净室实现**
（不看原代码、只按公开行为规格重写）或**完全独立**的作品。本项目本来就以 GPLv3 发布，
所以重写队列的目的只有一个：让"我们写了什么"这句话可以被核对。

## 1. 怎么度量的

`scripts/provenance.py` 回答一个可以机械核对的问题：这个文件**的每一行**，
最后一次被修改发生在 fork 起点之前还是之后？一条台账行是**两个时刻的混合**（见 §2 第三条要点）：
文件列表来自索引，"这个 fork 一行都没写过 / 一行都没动过 / vendored 进来的"三类文件按**磁盘**数行，
而两边都还在的 `mixed` 文件按 **HEAD 里的那份内容**跑 blame。

- fork 起点 = `19879235ef98a64b813de968306bc91a0d663518`（2026-09-03，本 fork 作者的第一条提交）。
  它的父提交链 = 上游历史，共 **215** 条提交（`xfangfang` 209 条，其余 6 位贡献者 6 条）。
- 判定手段是 `git blame --line-porcelain`，把每行的提交 SHA 落进"上游集合"还是"我们的集合"。
- 两个盲区，脚本显式处理而不是留给读者：

| 盲区 | 为什么 blame 会错 | 怎么补 |
|---|---|---|
| 从**另一个仓库**复制进来的文件 | 上游插件合集 `xfangfang/Macast-plugins` 从来不在本仓库历史里，粘贴它的那条提交会被当成作者 | `VENDORED` 常量表显式列出这 6 个内置插件 ⇒ 状态 `vendored`：要求上游归属，且**禁止**出现我们的声明 |
| 改名 / 挪位置的文件 | blame 不跨重命名，历史被截断在同一行 | 比对 blob SHA：内容若与上游提交过的某个 blob **逐字节相同**，就是上游的，不管现在叫什么名字（少于 5 行的文件不参与，空文件的 blob 全世界都一样） |

四条快速路径（新文件、未改动文件）走 `cat-file` / `diff --quiet`，只对真正动过的老文件跑 blame，
所以全仓 54 个 `.py` 一遍 1.7 秒。

四个状态就是声明必须对齐的四种事实：

| 状态 | 含义 | 声明要求 |
|---|---|---|
| `upstream` | 本 fork 一行都没写 | 只归上游；**不许**出现我们的声明 |
| `vendored` | 上游的，但来自历史看不见的仓库 | 同上，且必须点名 `Macast-plugins` |
| `mixed` | 两边都还在 | 两条都要有，**任何一条都不许删** |
| `ours` | 每一行都是本 fork 写的 | 我们的声明；这也是 §5 队列的终点状态 |

`--check` 把这三条规则当成缺陷报出来（缺上游归属 / 缺我们的归属 / 在不该有的地方署名），
`--stamp --apply` 只写**加法**：它从不删除任何一行已有声明。插入位置也是有讲究的 ——
插件的 `<macast.*>` 清单只从**文件顶部连续的注释块**解析（`AGENTS.md` §4.5），
所以新行必须留在那个块里；以 docstring 开头的文件不能让注释把 docstring 挤成普通表达式。

## 2. 台账总览

按域统计（行数是 blame 行数，不是文件大小；`ours 占比` = 我们写的行数 ÷ 总行数）：

| 域 | 上游行 | 本 fork 行 | 文件数 | ours 占比 |
|---|---:|---:|---:|---:|
| 核心接收端 `macast/**`（不含内置插件） | 3014 | 6346 | 19 | 67.8% |
| 内置插件 `macast/plugins/**`（vendored，来自上游合集 `Macast-plugins`） | 2885 | 0 | 6 | 0.0% |
| 内置插件 `macast/plugins/**`（本 fork 自研并内置，含 3 个空 `__init__.py`） | 0 | 11483 | 12 | 100.0% |
| mpv 渲染器 `macast_renderer/**` | 573 | 102 | 2 | 15.1% |
| 入口与打包 `Macast.py` / `setup*.py` / `hook-pystray.py` | 115 | 219 | 4 | 65.6% |
| 工具与验证 `scripts/*.py` | 0 | 17478 | 11 | 100.0% |
| **合计** | **6587** | **35628** | **54** | **84.4%** |

三个读数要点：

- **占比 84.4% 是被测试撑起来的**：`scripts/verify_cast_airplay.py` 一个文件就占 13079 行
  （`scripts/` 里还包括本工具自己）。把 `scripts/` 摘掉是 73.4%；只看**运行时真正加载的**代码
  （核心接收端 + mpv 渲染器）是 64.1%，也就是说应用本体还有约 3587 行是上游的。
- **一条台账行混着两个时刻：列表来自索引，行数一半来自磁盘、一半来自 HEAD。**
  `ledger()` 的文件列表是 `git ls-files *.py` ⇒ 暂存当场生效（`git rm` 掉的文件立刻短一条，
  新建的文件不 `git add` 就不进账）。行数则分两条路：`ours` / `vendored` / 未改动的 `upstream`
  走 `counted_lines()`，读**磁盘**；`mixed` 走 `git blame --line-porcelain HEAD -- <path>` ——
  **点了名的 rev 让 blame 读那条提交的 blob，工作树里没提交的改动它根本看不见**。
  两边都实测过：SSDP 多网卡那次改动还挂在 `macast/ssdp.py`（`mixed`）的工作树里时（磁盘 435 行），
  台账仍报 278+138=416（= `git show HEAD:macast/ssdp.py` 的行数），脏工作树与 `224d6d6`
  的干净 worktree 逐行报出完全相同的 54 行；`screen_mirror.py`（`ours`）则相反 ——
  A/B 里临时删掉六行预览逻辑，`our_lines` 当场从 34107 掉到 34101。
- **于是有一个方向吃了亏还不自知：提交之前量出来的表，描述的是上一条提交。**
  本轮的真实经过是 —— P7 的改动还没提交时 `--json` 报 6636/34112，文档照这个数改完、连同改动一起提交；
  提交之后同一条命令在**两个树里**都报 6632/33707，Part 34 那两条"文档 == 历史"的用例当场红，
  红的正是这次删 Tk 层动到的四个 `mixed` 文件（`macast/protocol.py`、`macast/macast.py`、
  `macast/gui.py`、`Macast.py`）。判据因此只有一条：**先提交，再量，再改文档**（想在提交前验，
  就 `git worktree add -d <tmp> <rev>` 跑一份干净树），并且改动 `.py` 归属之后必须重跑
  `provenance.py --check --stamp` —— 它只加声明，从不删除。
- **"看声明头"会把这件事估反**：按文件头里有没有 `by xfangfang` 数，会得出"21623 行是上游的"
  ——因为上游的头贴在了一堆**代码早被我们重写干净**的文件上（`macast/discovery.py`、
  `protocol_cast.py`、`protocol_group.py`、`protocol_airplay.py`、`scripts/verify_cast_airplay.py`
  全是这种：blame 显示 0 上游行）。头标记的是**当初的意图**，blame 标记的是**现在的事实**。
  这几个文件里上游的头**保留**：删掉它换来的不是自由，只是又一条没法核对的陈述。

## 3. 逐文件台账（只列需要解释的行）

`ours` 的 33 个文件不需要解释（12 个自研并已内置的插件、11 个脚本 —— 含 `scripts/provenance.py` 自己、
`macast/` 里的新模块、3 个空 `__init__.py`）。剩下 21 个：

| 文件 | 状态 | 上游行 | 我们的行 | 备注 |
|---|---|---:|---:|---|
| `macast/protocol.py` | mixed | 1000 | 1190 | DLNA 接收端骨架是上游的，一半以上已经是我们写的 |
| `macast/macast.py` | mixed | 422 | 1147 | 菜单栏与插件热插拔 |
| `macast/utils.py` | mixed | 400 | 290 | `Setting` 与环境准备 |
| `macast/gui.py` | mixed | 356 | 55 | 跨平台菜单层（rumps / pystray）＋主线程派发与通知；删掉那个 Tk 控制台窗口之后只剩这一点了 |
| `macast/ssdp.py` | mixed | 258 | 177 | **三层归属**，见 §4 |
| `macast/server.py` | mixed | 183 | 214 | 上游文件本来没有头 ⇒ 补的是 `Derived from` 而不是编造版权行 |
| `macast/plugin.py` | mixed | 175 | 14 | 渲染器基类：队列里第 1 位 |
| `macast_renderer/mpv.py` | mixed | 573 | 102 | 与 mpv 的 IPC 契约 |
| `Macast.py` | mixed | 52 | 164 | 入口 |
| `macast/plugins/protocol/nirvana.py` | vendored | 1822 | 0 | 「哔哩必连」，来自 `Macast-plugins` |
| `macast/plugins/renderer/iina.py` | vendored | 282 | 0 | 同上 |
| `macast/plugins/renderer/live.py` | vendored | 250 | 0 | 同上（原先没有任何头 ⇒ 补 `Copied from`） |
| `macast/plugins/renderer/potplayer.py` | vendored | 199 | 0 | 同上 |
| `macast/plugins/renderer/web.py` | vendored | 173 | 0 | 同上 |
| `macast/plugins/renderer/pi_fm.py` | vendored | 159 | 0 | 同上 |
| `macast/renderer.py` | upstream | 212 | 0 | 一行都没动过 ⇒ 队列第 1 位 |
| `macast/__init__.py` | upstream | 8 | 0 | |
| `setup.py` | mixed | 42 | 35 | 署名已按量测补齐 |
| `setup_py2app.py` | mixed | 7 | 18 | 署名已按量测补齐 |
| `hook-pystray.py` | mixed | 14 | 2 | 署名已按量测补齐 |
| `macast_renderer/__init__.py` | upstream | 0 | 0 | 空文件：没有任何表达作者身份的代码，两种声明都不需要 |

## 4. 第三层：`macast/ssdp.py` 里的 MIT 血统

这个文件不属于"上游 vs 我们"两分法。它是 GUPnP/Coherence 那一支的 SSDP 实现，
头部同时挂着 **Tim Potter、John-Mark Gurney、Fluendo、Frank Scholz、Erwan Martin、
FangYuecheng** 的版权行和一句 `Licensed under the MIT license`。这些声明的义务主体
是**那几位作者**，既不是本 fork 也不是 `xfangfang/Macast` —— 所以任何人都没有资格删它，
包括把整个文件重写一遍之后（重写只消灭上游行，不消灭那几位作者留下的行）。

`provenance.py` 的 `THIRD_PARTY` 表把这句话变成了检查项：那句 MIT 声明不见了就报红。

## 5. 重写队列

判据只有一个，而且是可以机检的：**该文件的 `upstream_lines` 变成 0**。
在那之前 `mixed` 状态的两条声明都得留着；变成 0 之后，`provenance.py` 会自己把
"该文件不再需要上游归属"这件事说出来 —— 不需要人来判断，也就不存在判断错。

顺序按"改动频率 × 上游行数"排，先动最挡路的：

| 顺位 | 模块 | 上游行 | 为什么先/后 | 完成后额外要动的地方 |
|---|---|---:|---|---|
| 1 | `macast/plugin.py` + `macast/renderer.py` | 387 | 渲染器基类是所有插件的地基，它越薄，后面每一步越安全 | `docs/Development.md` 的插件接口段 |
| 2 | `macast/ssdp.py` | 258 | DLNA 发现的核心；**注意 §4 的 MIT 行不随上游行归零** | `AGENTS.md` §3、§4.1 |
| 3 | `macast/protocol.py` | 1000 | 全仓最大的一块上游代码，也是 XML 状态变量的真值来源 | Part 6/12/13 的用例要跟着重写，不能只改实现 |
| 4 | `macast/utils.py` | 400 | `Setting` 的副作用（§4.2 第一条坑）就住在这里 | `module_settings.py` 的标签表 |
| 5 | `macast/gui.py` | 356 | rumps/pystray 适配层，接口窄、最好换 | `BUILDING.md` 的 pystray 说明 |
| 6 | `macast/server.py` | 183 | CherryPy 装配；和 §4.10 的日志红线同一条链 | `logsplit.py` 的注释 |
| 7 | `macast_renderer/mpv.py` | 573 | mpv IPC 契约（`--input-ipc-server`、事件名）——这一层的行为规格在 mpv 那边，不在我们这边，净室可做 | `AGENTS.md` §5 排障手法 |
| 8 | `macast/macast.py` + `Macast.py` | 497 | 最后动菜单栏宿主：它牵 `App.call_on_main_thread`、插件装载、`install_url` 三处门控点 | `AGENTS.md` §3 |
| — | `macast/plugins/**`（vendored） | 2885 | **不在队列里**：这些是上游插件合集的完整作品，"重写"的正当形式是另写一个插件放进 `plugins/`，而不是原地替换后声称不是抄的 | — |

每一步的收尾动作（顺序错了 Part 34 会红）：改代码 → **先提交** → `provenance.py --check --stamp`
量一次 → 更新 §2/§3 的表与 `provenance-ledger` 行 → 跑套件 → 提交这一次文档改动。
（急着在提交前看到绿，就 `git worktree add -d <tmp> <rev>` 跑干净树，或者用不带 rev 的
`git blame --line-porcelain -- <path>` 预量 —— 未提交的行报 `0000…`，落不进上游集合，
和它提交之后被记成本 fork 的行是同一个数。）

## 6. 明确不做

- 不删任何已有声明。`--stamp` 只会 `insert`，代码里没有 `remove` 这条路径。
- 不在 `upstream_lines > 0` 的文件上写我们的声明，也不在 `vendored` 的文件上写 —— 后者是
  `problems()` 的第三条规则。
- 不为了"看起来更干净"把 `macast/plugins/**` 原地改名或搬位置：那只会让 blame 更瞎，
  而内容一个字都没变。
- 不把重写叙述成摆脱 GPLv3 的路径（§0）。
