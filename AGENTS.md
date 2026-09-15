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

# 2) 回归验证（当前 145/145）
env -u PYTHONPATH .venv/bin/python scripts/verify_cast_airplay.py
```

> 加新功能/修 bug 时**同时补用例**。这个仓库的验证脚本是唯一能防回退的东西。

## 3. 代码地图

```
Macast.py                  入口（GUI / CLI 两种模式）
macast/
  macast.py                MacastApp（菜单栏）、MacastPlugin / MacastPluginManager（插件热插拔）
  server.py                CherryPy 服务、HTTP/HTTPS 通道、SSDP 生命周期
  protocol.py              DLNA 协议 + DLNAHandler（Web UI / 管理 API 都在这里）、PlaybackGuard
  protocol_group.py        ProtocolGroup：把多个协议伪装成一个，扇出 start/stop/set_state_*
  protocol_cast.py         Chromecast 接收端（mDNS + Cast v2 over TLS:8009 + setup HTTP:8008）
  protocol_airplay.py      AirPlay 接收端（mDNS + RTSP:7000，仅视频 URL）
  ssdp.py                  SSDP（DLNA 发现）；Sock.send_it 里的 LOCATION 走 Setting.get_ip()
  discovery.py             mDNS 广播（zeroconf），只广播可达地址
  utils.py                 Setting（含网卡枚举/选择）、环境准备、XML 路径
  gui.py                   跨平台菜单抽象（darwin: rumps；其他: pystray）
  xml/setting.html         设置页（Vue2 + Element UI，**单文件内嵌模板**）
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
- **DLNA 默认插件的真实标题是 `DLNA Protocol`**（由类名推导），持久化/比对一律走
  `MacastPluginManager.resolve_title()`，别硬编码 `"DLNA"`。
- **多协议并发时 `event_subscribes` 等 DLNA 专属属性要 `getattr(..., default)`**，
  否则只启用 Chromecast 时会抛 AttributeError。
- **CherryPy 工作线程改 AppKit 菜单不安全**：走 `App.call_on_main_thread()`。
- **多实例会把端口写歪**：旧实例占着 58880 时起新实例，端口回退会把 `ApplicationPort`
  改成随机值并重置 USN。起新实例前先杀干净（`lsof -nP -iTCP:8009 -sTCP:LISTEN -t`）。
- **`mpv --http-proxy=` 不能替代摘环境变量**，ffmpeg 的 HTTP 层仍然读 `http_proxy`。

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
| `verify_cast_airplay.py` | **主验证套件**（145/145）：协议逻辑 + 真实 socket 端到端 + mDNS/网卡/插件热插拔 |
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

## 9. 当前能力边界（别当成 bug）

| 能力 | 状态 |
|---|---|
| DLNA（SSDP + UPnP）接收 | 完整 |
| Chromecast 接收（Cast v2，URL 投屏） | 可用；**未认证接收端**，Google 官方发送端可能因设备认证失败 |
| AirPlay 视频 URL 投屏 | 可用 |
| AirPlay 音频（RAOP）/ 屏幕镜像 | **未实现** |
| DRM 内容 | **不可能支持** |
| 插件 | 支持启用/停用/卸载/安装（**热生效，不重启**）；卸载进 `.trash/` 可恢复 |

## 10. 与用户协作的约定（这个仓库的历史教训）

- **不要为了验证去改用户的真实配置**：所有涉及设置的测试用临时 `SETTING_DIR` +
  临时 `setting_path`，测试完还原。（曾因此被用户明确纠正。）
- **明确区分"发送端问题"和"我们的问题"再动手**：这个项目里多次出现"用户报我们的 bug，
  实际是发送端行为"，先拿证据（日志/事件/源码）再改代码。
- **破坏性操作要可逆**：删插件用 `mv` 到 `.trash/`；删除/覆盖用户文件前先备份。
- **提交前自查暂存内容**，别夹带构建产物/密钥/`macast.log`。
