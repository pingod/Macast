# Copyright (c) 2026 by pingod. All Rights Reserved.
# Screen Mirror for Macast
#
# Macast Metadata
# <macast.title>Screen Mirror</macast.title>
# <macast.renderer>ScreenMirrorRenderer</macast.renderer>
# <macast.platform>darwin,win32,linux</macast.platform>
# <macast.version>0.28</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>pingod</macast.author>
# <macast.role>addon</macast.role>
# <macast.desc>把这台 Mac / PC / 桌面镜像到局域网里的 Chromecast（两条通道：兼容的 MPEG-TS LOAD，或一条实验性的低延迟 Cast Streaming 通道——它讲 Chrome 自己的镜像协议，设备拒绝就回落到 LOAD）、一台老 DLNA 电视（五种兼容档位，电视上不用装任何东西）、或局域网里任意浏览器（打开一个网址即可，无需 App）。ffmpeg 负责采集与编码（Windows 走 ddagrab/gdigrab、Linux 走 x11grab；macOS 13 及以上由 ScreenCaptureKit 直接采进管道、老系统走 avfoundation），由本机持续吐出实时流：Chromecast 上用 MPEG-TS LOAD，浏览器里用内置网页播放分片 MP4，DLNA 电视则用一条故意永不结束的 MPEG-PS / MPEG-TS / MKV「文件」经 SOAP 推给它去拉。系统声音在存在采集口时一并带上：macOS 有一键辅助安装（官方 BlackHole 安装包，校验 sha256，并自动建好多输出聚合设备），Linux 用 PulseAudio 的 monitor，Windows 用 dshow 的「立体声混音 / Stereo Mix」回环设备（开启时）；Windows 下会报出这个设备名，没有则说清楚开哪扇门，而不是谎称只有画面。首帧预算自 0.14 起按平台区分：gdigrab 得先打开桌面才能开 dshow 输入，所以给 macOS 定的 3 秒预算曾把「声音设备已协商好立体声」的采集判死，而 Windows 那一路被指去了 macOS 的麦克风面板。还可选：哪块屏幕、要不要指针、四档画质、VideoToolbox 硬件编码（默认 auto，编码探测有答复才用硬件），以及电视掉出 PLAYING 时重新推送的 DLNA 看门狗。0.17 起看门狗能按自己的建议行动：电视拒绝的实时会话会自己走完五档兼容档位，每级重启一次、每轮最多四次，绝不在「伪装成文件」的形状里（那里该改的是形状不是容器），也绝不写进你的设置——下次手动启动仍从你选的档位开始。默认档位现在也跟着形状走，因为两者测得不一样：MPEG-TS + H.264 在实时流上稳定约 1.9 秒、MPEG-PS 约 5.1 秒，所以实时镜像默认 ts-h264，只有文件形状还默认 DVD 时代的 ps-pal。这些数字现在就列在设置页每个档位旁，并写明测量范围。自 0.11 起整个控制面都在 Macast 浏览器设置页的「电脑投屏」tab；自 0.12 起菜单栏不再有镜像行，只剩通知，停止走 tab、「停止接受投屏」或换渲染器——三条路汇到同一个 teardown。慢观众丢的包现在落在容器边界（整片 MP4 分片、整包 188 字节 TS），队列按画面的面积秒数预算而非字节，控制台会说明系统声音到底有没有真正进采集口，而不只说存在采集口。自 0.13 起本进程读不到的音频口不再让镜像报废：一帧都不回的采集会去掉它重试，降级成功与最终失败都会点名权限那扇门和要做的重启。0.18 起这一路的延迟预算整个重算过：低延迟通道的加密改走操作系统自带的 AES（macOS CommonCrypto、Windows bcrypt、Linux libcrypto，纯 Python 只作最后兜底，启动时跑一次已知答案自检），本机实测从 1.35 MB/s 提到 5367 MB/s，所以那条通道的码率上限从 4.5 Mbps 提到 8 Mbps（拿不到系统 AES 才降级回 4.5，菜单会写明是哪一种）；在途窗口不再按帧数算而按时长算（约 66 毫秒起、不超过协议自己承诺的目标延迟的三分之一），因为原来的 12 帧在 24 fps 下是 500 毫秒的排队、是 200 毫秒预算的 7.6 倍；编码器对齐了 Google 参考发送端的三处（+low_delay、slice 线程、半秒 VBV 而不是一秒）；VideoToolbox 那一路现在按 1.5 倍线速要码率，因为它实测比 -b:v 少给三成，而画质并不因此更好。另外两个过去写死的数字变成了你能调的旋钮：DLNA「伪装成文件」的预填秒数（1–8，默认 4，那个数字就是这条目标看得见的延迟）和浏览器播放页的落后上限（0.5–5，默认 1.0，原来是 3）。0.19 起浏览器这一路的分片改成每帧一片（原来是每 0.5 秒一片），本机实测（VideoToolbox）端到端延迟从 1305 毫秒降到 838 毫秒——买下延迟的是分片节奏，不是那个旋钮，所以它现在的角色是防漂移而不是调延迟；迟到的观看端拿到的积压会从最近的关键帧开始重播，不够一格的零头宁可丢掉也不给您花屏。低延迟通道的这些改动依然没有真电视验证过，本机局域网里没有 Chromecast。0.20 起 Windows 的画面采集换成 Desktop Duplication（ddagrab）：原先 gdigrab 抓的是整块虚拟桌面，多显示器时那里可能是个奇数高度，而奇数高度让编码器直接零字节退出——原画档在那种机器上什么都投不出来；现在每块屏幕按自己的尺寸采，Windows 的屏幕选择器也随之上线；ddagrab 在运行时被桌面拒绝会自动回落到 gdigrab，且本次运行内不再重试它。0.21 起 macOS 的画面采集优先走 ScreenCaptureKit：13 及以上系统用它把屏幕与系统声音一起原生采进来，不再需要安装 BlackHole 或任何东西，本机实测从启动到首帧比 avfoundation 快约 450 毫秒；如果 ScreenCaptureKit 一直没送来系统音频，本次镜像会先去掉声音继续（页面会立刻写明原因和「重启 Macast 再试」，之后的镜像连开始通知也会带上这句），而且本次运行里之后的镜像也会先跳过声音。老系统或这套框架不可用时自动回落到原来的 avfoundation 路径（Mac 的一键设置仍为那条路保留）。0.22 起多出第五种目标：WebRTC（浏览器 · 低延迟 · 此通道无声音）——页面里开一个真正的 RTCPeerConnection，编码器吐出的 H.264 NAL 零重编码原样装进 RTP 送过去（这条形状的 NAL 就是交给接收方打包的，不是容器）；它需要可选的 aiortc 与 av 依赖（没装时选中它会当场给出 pip 配方而不是静默失败），信令走本机 HTTP（一次换取 offer、一次交回 answer），观看地址与会话凭据和浏览器形状一样按会话临时。0.23 起三处按实测改正。浏览器这一路的发送队列不再按字节封顶，而是按容器自己的单位算（分片 MP4 数分片、Matroska 数簇），因为改成每帧一片之后，0.75 秒的预算在旧的字节上限里只装得下 8 片 = 0.11 秒，也就是说约 28% 的字节是我们自己丢的；设置页与观看页浮层现在都直说队列排了多久（多少秒、多少块），迟到的观看端「等不到可重播的起点」也从一件无声的事变成会点名的计数器（重播等关键帧 N 次）。告诉浏览器要解码什么不再靠猜：H.264 的 profile 是从编码器交出来的 avcC 现场读的，观看页、统计浮层与设置页三处读的是同一个答案，兜底那个字符串只在读不出时才出现。Windows 上「硬件编码」现在指的是 NVENC，而且判决方式跟着平台变：Mac 问的是 ffmpeg 的编码器清单，Windows 是真编 0.5 秒测试帧，因为清单里有不等于驱动能用；同一条真实采集上 NVENC 吃 0.21 个核而 x264 吃 0.44 个，并且它把码率落在档位承诺的那个数字上（不像 VideoToolbox 少给三成，所以那 1.5 倍补偿只给 VideoToolbox）。这一轮第一次做了跨机器实测：Windows 192.168.1.68 采集、Mac 上真浏览器观看，browser 与 webrtc 两种形状都跑通，页面读数与发送端逐格一致（此前所有延迟数字量的都是 Mac 投给自己）。0.24 起投给电视的那两路（Chromecast 兼容通道与 DLNA）多了「投屏最大时长」：三档 12 / 24 / 48 小时，默认 12，而且故意没有「不限」这一档——这一页的默认值从来没人去动，"没人动就等于不设限"正是这个旋钮要结束的状态。到点是主动停止并弹一条点名这个开关的通知，而不是悄悄把画面截掉；同一个数只问一次，ffmpeg 的 -t 与「伪装成文件」那对长度/时长都由它算出来，而且这个上限只许缩短那一对、不许拉长。我们的计时器和 ffmpeg 自己的 -t 谁先到是时序问题（预填与 LOAD 往返有时让 -t 抢先），两条入口因此都认这次是计划内的结束，generation 判定让晚到的那个闭嘴，用户只听到一次。浏览器页、低延迟通道与 WebRTC 不受它约束，那三路是直播边缘的消费端：截断它们省不下任何编码开销，代价却是切掉一个正在讲话的人——所以「画质」卡上也不给它们出现这组按钮，一个调不动东西的控件是句谎话。低延迟通道与真电视这一半依然没有验证过，本机局域网里没有 Chromecast。0.25 起「低延迟」这两个字后面跟着数字：WebRTC 这一路的屏幕到屏幕延迟第一次量出来了——本机同一套闪光测量（一个按墙钟时刻涂黑涂白的无边框窗口，配观看页自己每帧的亮度采样，同一台机器同一个时钟，两个时刻之差就是整条链路）给出 369 毫秒，同一趟里浏览器（MSE）那一页是 492 毫秒；两个数都偏保守，因为参考时刻取在 AppKit 把窗口推给显示器之前，最多多算一帧，而测量时旁边一直开着一个小窗在闪，因为 macOS 的采集是按变化给的：静止桌面上采集交出来的每一帧都被钉成 1/24 秒的间隔，媒体时钟只有墙钟的约 0.4 倍，读出来的每个延迟都会虚高。设置页两张卡片与帮助弹层现在都写这两个数，并写明它们和 mse_latency_probe 报的 838 毫秒量的不是同一段（那支读的是播放器缓冲边缘）；这个数字只有一处（MEASURED_LAG_MS），卡片、帮助与用例读的是同一份。0.26 起把 Windows 上这一档的代价写在「投屏方式」卡的浏览器那一行上：把系统声音的回环设备读进输出会把画面门住，跨机实测画面每秒只变化约 1.5 次，而同一台机器同一条链路上 WebRTC 那一路的解码与上屏实测都是 24 帧每秒；这句话只在被描述的机器是 Windows 且采集探测真的把声音映射进流时出现，两个数字读自有测量的常量而不是抄进散文，也不写毫秒——跨机的 browser 延迟没有可信读数，编一个数字比不写更糟。0.27 起 aiortc 与 av（连带它们拖进来的 cryptography / pylibsrtp / cffi）不再随四个平台的默认产物发布：要用 WebRTC 那一档的时候，设置页「电脑投屏」的「WebRTC 依赖」卡按一个按钮，从本项目的发布页取对应平台的那一条依赖包，解到配置目录里按这台机器的解释器分键的那个目录，并从那里导入——所以选完这一档不需要重启 Macast，那句话是被测出来的性质而不是承诺。这张卡上有两个见证者，而且它们故意要能不一致：「已落盘」问那份清单文件在不在，「已可用」问刚才这一次是不是真的导入成功了；包躺在盘上却 import 不起来（缺系统库、架构不对）是真实存在的一种状态，把安装说成成功而画面仍然黑着才是谎话。移除把那棵树挪进配置目录的 .trash 而不是删掉；如果那个目录不是我们种的而 aiortc 仍然导入得起来，它会拒绝并念出那个包真正的来路，让你用 pip 去卸它。这一档仍然没有真电视与跨机验证过。「一键安装」已经在打包产物上按过了，而那一次按暴露了两个出厂缺陷，0.28 修的就是它们：一是重启之后那棵已经落盘的树不再被挂回搜索路径，于是卡片说「已安装」而导入说「没有这个模块」，唯一的补救是再下二十六兆；现在每一次特征探测都先把我们那个目录挂回去（每个进程一次，不是每秒一次）。二是下载进度：原来那一条阶梯只会答「正在下载」，进度条钉在 3% 不动，而一条中途断掉的流会被当成一份成功——现在它按真的字节数走（源站不给长度就只报已下载多少 MB，不编一个分母），断流则明确说「下到 X MB 就断了（整份是 Y MB），重试会整份重来」，并把失败的那次落点目录清空。另一件事修在打包侧而不是这里：Windows 的默认产物从 0.27 起不再携带 CPython 的稳定 ABI 转发库，而那棵树里每一个 .pyd 都链接它，所以应用内装好的依赖在真机上加载失败——构建现在把这一个文件放回去，并在构建完成后打开产物清单核对它真的在里面。</macast.desc>
#
# Why: Macast is a receiver -- everything it plays was pushed to it. This
# plugin turns it around for one case: cast what is on this Mac's display,
# the way JustStream does, without leaving the menu bar.
#
# Implementation notes, because this is a *live* sender (see also
# cast_bridge.py, which forwards a finished URL):
#   * the pipeline is  ffmpeg screen capture -> H.264 (libx264 or
#     VideoToolbox) -> a muxer chosen by the output target -> a tiny HTTP
#     server that broadcasts every chunk to every connected client. The
#     Chromecast LOADs http://<this machine>:<port>/stream/<id>.ts; a browser
#     opens /browser?token=<page token> and feeds the same bytes from
#     the matching .m4s URL into MSE, falling back to a progressive <video>;
#   * both public addresses carry a per-session secret: the stream URL is
#     built from a random id, and /browser requires the matching token --
#     a live mirror is not something to hand to "any host that can reach
#     this port" (see the note on _StreamHandler and AGENTS.md 4.7);
#   * capture is platform dispatched: ScreenCaptureKit first on macOS 13+
#     with avfoundation behind it (see the ScreenCaptureKit section for when
#     each wins), ddagrab preferred with gdigrab behind it (Windows; see the
#     DDAGRAB_* notes for what the gdigrab path costs on a multi-monitor
#     desk), x11grab (Linux/X11).
#     System audio rides along where a tap
#     exists: on macOS 13+ that tap is ScreenCaptureKit's own -- no device
#     needed -- and older systems fall back to a BlackHole device (no
#     released FFmpeg can see system audio natively -- the screencapturekit
#     demuxer never shipped),
#     Windows needs a dshow *loopback* recording device (Stereo Mix where the
#     driver ships it, otherwise a virtual cable -- ffmpeg's dshow input reads
#     whatever the system exposes as an input, so a device that carries the
#     output is the whole requirement), and Linux uses the PulseAudio
#     `<sink>.monitor`. On all three the tap is probed for, never created:
#     a machine without one mirrors video and says which device to switch on.
#     The probe is cached because the menu must never spawn ffmpeg;
#   * slow consumers drop whole chunks rather than blocking the reader --
#     for a live stream a stale frame is worse than a missing one, and a
#     blocked stdout pipe would stall the encoder;
#   * for the browser target a late joiner is replayed the fMP4 init segment
#     plus a rolling tail, so it can attach mid-stream; the MPEG-TS target is
#     deliberately *not* replayed, because a TV would then have a backlog to
#     drain and would sit seconds behind for the rest of the session;
#   * the DLNA target is the awkward one, because a ten-year-old MediaRenderer
#     has no notion of "live", so it answers in one of two shapes. 「直播流」says
#     what the bytes are -- no length, no ranges, nothing to seek inside -- and
#     is the default because it is the only shape measured to play.
#     「伪装成文件」is the older trick for firmware that will only fetch a finite
#     file: Content-Length under 2 GiB (some of it does signed 32-bit arithmetic
#     there), Accept-Ranges plus transferMode/contentFeatures.dlna.org on every
#     answer, a bounded sniff answered with *exactly* the bytes asked for
#     (MPEG-PS padding, never a short read), and reconnects served by absolute
#     byte offset out of a ring 48 MiB deep rather than the drop-oldest queue the
#     other targets use. Only that shape hoards before handing the URL over, and
#     the seconds that costs are said out loud in the start message; the live
#     shape says 不预填 because it has nothing to pre-fill;
#   * the container inside those bytes is chosen *per shape*, because a
#     measurement said so: same machine, same receiver, same encoder, a
#     byte-equivalent MPEG-2/AC-3 payload -- MPEG-PS costs 5.86 s to the first
#     picture against 2.47 s for MPEG-TS, and stays ~3 s behind for the rest of
#     the session (see `default_dlna_profile_id` for the two mechanisms and for
#     the third suspect that was falsified). So the live shape defaults to
#     TS + H.264 and the file shape keeps MPEG-PS, which is what DVD-era
#     firmware -- that shape's only audience -- was built to find. And when a
#     live renderer refuses what it was handed, the watchdog walks the five
#     profiles itself instead of leaving that to a user who is looking at the
#     television: one rung per refusal, at most once per other profile, never in
#     the file shape, and never into `Mirror_Dlna_Profile`. See
#     `_rotate_dlna_profile`;
#   * the Cast sequence is the sender-side one: deviceauth CHALLENGE ->
#     CONNECT receiver-0 -> LAUNCH(CC1AD845) -> CONNECT <transportId> ->
#     LOAD with streamType LIVE. Framing is reused from
#     macast.protocol_cast, so no pychromecast dependency (a single-file
#     plugin cannot install pip packages);
#   * the 低延迟 target skips LOAD entirely and speaks Cast Streaming, which
#     Chrome's own "Cast desktop" uses: LAUNCH(0F5096E8) -> an OFFER on
#     urn:x-cast:com.google.cast.webrtc -> an ANSWER that names a UDP port ->
#     RTP packets with a 7-byte Cast header, each access unit encrypted once
#     with AES-128-CTR (the OS's own cipher via ctypes where one loads, pure
#     Python only as the last resort) and keyed per frame. Nothing is served
#     over HTTP for this one,
#     so it has no viewer URL and no audio, and because the field layouts are
#     transcribed rather than observed on a television it is opt-in and falls
#     back to LOAD when the device refuses the mirroring app;
#   * ffmpeg is located via PATH *and* the usual install directories, because
#     a menu-bar app launched from Finder does not inherit the shell PATH;
#   * capture fails closed: without Screen Recording permission (macOS)
#     ffmpeg exits within a second, and the plugin turns that into a
#     message naming the System Settings pane instead of a silent nothing.
#   * the BlackHole assisted install decides by *witness*, not by "is the
#     device missing", because five different machine states answer to that one
#     question and only two of them want an installer: see _blackhole_state.
#     Running the pkg again on a machine whose driver is merely unloaded is the
#     loop the user reported ("装不完"), and CoreAudio-yes/ffmpeg-no is a
#     microphone permission, which no download fixes either.

import asyncio
import ctypes
import hashlib
import importlib
import json
import locale
import logging
import math
import os
import platform
import re
import secrets
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
import urllib.request
from collections import deque
from enum import Enum
from fractions import Fraction
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from queue import Queue, Empty, Full

import cherrypy

from macast import Setting, gui, notice, plugin_repo, utils
from macast.renderer import Renderer, RendererSetting
from macast.protocol_cast import (encode_cast_message, parse_cast_message,
                                  DEFAULT_MEDIA_APP_ID, NS_CONNECTION,
                                  NS_MEDIA, NS_RECEIVER)

logger = logging.getLogger("ScreenMirror")
logger.setLevel(logging.INFO)

CAST_PORT = 8009
SERVICE_TYPE = "_googlecast._tcp.local."
DEVICE_AUTH_CHALLENGE = b"\x0a\x00"
#: The version this file announces. One place, because the header the settings
#: page shows and the `<macast.version>` manifest have to agree -- a regression
#: test compares both against this constant.
PLUGIN_VERSION = '0.28'
#: The receiver app that speaks Cast Streaming. Not the Default Media
#: Receiver: mirroring lives on its own app id, its own namespace, and it never
#: accepts a LOAD -- the media plane leaves TLS for UDP entirely.
MIRROR_APP_ID = '0F5096E8'
NS_WEBRTC = 'urn:x-cast:com.google.cast.webrtc'
STREAM_PREFIX = "/stream/"
CHUNK = 4096
#: One MPEG-TS packet. Where a slow consumer is being dropped for, the queue
#: unit is a whole number of these -- see `_Broadcaster(packet_align=...)`.
TS_PACKET = 188
#: Seconds of slack a live consumer gets in front of it. This is the budget;
#: `live_queue_units` is what spends it, and the unit it spends in is the
#: container's, not a fixed size.
LIVE_QUEUE_SECONDS = 0.75
LIVE_QUEUE_MIN_CHUNKS = 64
LIVE_QUEUE_MAX_CHUNKS = 256
#: Never fewer queue units than the framed shapes used to hard-code. Eight was
#: about half a second of Matroska clusters (50-60 KB each at these bitrates)
#: and about a sixth of what the fMP4 shape needed -- see `live_queue_units`.
LIVE_QUEUE_MIN_UNITS = 8
#: AAC emits one frame per 1024 samples and the browser target muxes at 48 kHz
#: (`-ar 48000`, in `_AAC` and in `build_ffmpeg_command`), so one second of its
#: audio track is this many samples-worth of fragments. Coupled to that argv by
#: a case in Part 37: change the sample rate there and this number has to move.
AAC_FRAMES_PER_SECOND = 48000 / 1024.0
#: The live Matroska shape queues whole clusters, and the muxer closes one by
#: *size*, measured at 50-60 KB on the shipping shape (see `_Clusters`). The
#: top of that range is used so the seconds buy a conservative count rather
#: than an optimistic one.
MKV_CLUSTER_BYTES = 60 << 10
#: How long ffmpeg may survive before its death is blamed on the
#: Screen Recording permission instead of a genuine mid-stream failure.
EARLY_DEATH_SECONDS = 5.0
#: Rolling tail replayed to a late-joining browser viewer. Enough for one
#: fragment plus the keyframe it starts on -- which is the whole requirement,
#: because a replay that begins mid-GOP is a green smear until the next
#: keyframe. Since 2026-10 that requirement is **enforced** by
#: `_Broadcaster.tail()` rather than handed over by the muxer: fragments now
#: start on any frame, so the tail is cut back to the last one whose first
#: sample is a keyframe. Measured at a shipping bitrate this ring is ~6 s of
#: tail, i.e. a dozen times the 0.5 s of GOP the cut can cost.
#:
#: Deliberately small: the replay is a *standing* delay, not a one-off. The
#: browser decodes everything it is handed before it shows anything, so 8 MiB
#: of tail meant a viewer that opened the page sat 10 s behind live at these
#: bitrates and stayed there. 2 MiB cuts that to under three seconds -- and
#: with the park doing the parking (see `LIVE_EDGE_SECONDS`) a late viewer
#: measured 123 ms behind in practice.
REPLAY_BYTES = 2 << 20
#: How far behind the live edge the browser player parks itself, in seconds.
#: Like `REPLAY_BYTES` and the DLNA prefill, this is a *standing* delay: the
#: player seeks back to it once and then never catches up.
#:
#: 3 s was transcribed from Mac-Screencast and never questioned. It is **not**
#: the floor on this target's end-to-end latency, which an earlier version of
#: this comment claimed: on a real desktop (2026-10-02) the shape sat at
#: 1305 ms with the park at 1.0 s and 1201 ms with it at 0.5 s, so halving it
#: bought 8%. The term that actually held latency up was the fragment cadence,
#: upstream of the player, and it is the one that changed.
#:
#: The park still has to clear a fragment. Until 2026-10 that was
#: `gop_size('browser')` = FPS // 2 = 12 frames = 0.5 s and this was 1.0 s, two
#: fragments of slack; the muxer now cuts a fragment per frame, so a fragment
#: is 1/FPS = 41.7 ms and this is twelve of them. Measured at both ends of the
#: range the pill row offers: 838 ms at 1.0 against 829 ms at 0.5, with **0
#: controller seeks at either** -- at a fine cadence the player's `behind` no
#: longer swings across the park value, so the park stops being a latency
#: control and is only a recovery distance. That is why the default did not
#: move, and why the floor below did not either: 0.5 s is now a dozen
#: fragments of slack rather than exactly one, and nothing measured asks for
#: less.
#:
#: Half a second is the floor rather than zero because a park distance of zero
#: means the player is always exactly at the write head, where one encoder
#: hiccup is a stall and a stall past `STALL_MS` is a full page reload.
LIVE_EDGE_SECONDS = 1.0
LIVE_EDGE_MIN_SECONDS = 0.5
LIVE_EDGE_MAX_SECONDS = 5.0


def live_edge_seconds():
    """The park distance the browser player should use, clamped.

    Read at request time and injected into the page, never baked into
    `PLAYER_PAGE`, because the page is a module-level constant string and a
    setting is not.
    """
    try:
        value = float(Setting.get(SettingProperty.Mirror_Live_Edge,
                                  LIVE_EDGE_SECONDS))
    except (TypeError, ValueError):
        return LIVE_EDGE_SECONDS
    return max(LIVE_EDGE_MIN_SECONDS,
               min(LIVE_EDGE_MAX_SECONDS, value))


#: The pill row for the browser player's park distance. Same reason as
#: DLNA_PREFILL_OPTIONS: values the clamp accepts, and a default the comment
#: above derives from the fragment cadence rather than picking out of the air.
LIVE_EDGE_OPTIONS = (LIVE_EDGE_MIN_SECONDS, LIVE_EDGE_SECONDS, 2.0, 3.0,
                     LIVE_EDGE_MAX_SECONDS)

#: What the control has to say about itself. The page has to say what pushing it
#: down does *not* buy, or「更快」is the only thing the user reads -- and after
#: 2026-10 that includes admitting the knob is nearly spent: the measured
#: difference between 1.0 and 0.5 is 9 ms, because what held this path back was
#: the fragment cadence rather than the park.
LIVE_EDGE_HINT = ('播放页会把自己停在直播边缘后面这么多秒，而且一直停在这里、'
                  '不会追上来。实测把它从 1.0 调到 0.5 只快了 9 毫秒——这一路'
                  '的延迟主要由分片节奏决定（现在是每帧一片，41.7 毫秒），'
                  '这一格再调小基本买不到什么。调到 0.5 秒以下会让播放头'
                  '接近写入头，一次编码打嗝就是卡顿，卡够 8 秒整页重载。')
#: A system-audio tap (BlackHole on macOS, a PulseAudio monitor on Linux) is
#: clocked by whatever the audio stack feels like, not by ffmpeg. Left alone,
#: the two clocks slide past each other by a sample at a time and every slip
#: comes out as a click -- the「杂音」this stream used to have on the Chromecast
#: and DLNA targets. `async=1` tells the resampler to absorb the drift by
#: repeating or dropping input samples instead of passing the gap through.
AUDIO_RESAMPLE = ['-af', 'aresample=async=1']

#: key -> (target height, video bitrate). A height of 0 means "do not scale",
#: which is what the 'source' preset is; the label the page shows comes from
#: `quality_text()` and is *not* in this tuple -- the comment used to claim it
#: was, which is the kind of drift that makes the next reader add a field.
#:
#: These are rates for 24 fps of *desktop*, not for film: a mostly-static
#: screen costs very little, and the bandwidth a frame asks for beyond what the
#: link can deliver in the moment does not buy quality -- it buys a full queue,
#: a dropped block (a hole in the audio) and a viewer that is further behind
#: every second. `rate_caps` bounds the same burst from the other side.
QUALITIES = {'360': (360, 2000000),
             '720': (720, 4000000),
             '1080': (1080, 6000000),
             'source': (0, 8000000)}


def rate_caps(bitrate):
    """Constrained VBV: a 1.5x peak inside a half-second buffer.

    Without a ceiling, one frame that redraws the whole screen (a window drag,
    a video, a terminal scroll) is emitted at whatever size it wants. Every
    consumer downstream is sized for the average, so that one frame is exactly
    what overflows the queue -- and the drop is audible, not just visible.

    The buffer is half a second, not a full one, because that is what Google's
    own reference sender uses (`rc_buffer_size = bitrate / 2`) and a full second
    of VBV is a full second the encoder is allowed to spend catching up on a
    burst before the rate control reacts. Half a second still smooths a window
    drag; it just stops paying for it with latency.
    """
    return ['-maxrate', str(int(bitrate * 1.5)),
            '-bufsize', str(int(bitrate) // 2)]


def quality_preset(key=None):
    """(height, bitrate) for a menu key; anything unknown is 720p.

    Two callers need this -- the renderer picks the encoder settings, the menu
    has to say what the low-latency channel will actually do with them.
    """
    if key is None:
        key = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
    return QUALITIES.get(key, QUALITIES['720'])


def quality_key():
    """The stored preset name, validated: an unknown value is 720p, exactly as
    `quality_preset` resolves it -- the console must not highlight a row that
    does not exist while encoding a different one."""
    key = str(Setting.get(SettingProperty.Mirror_Quality, '720') or '720')
    return key if key in QUALITIES else '720'

QUALITY_LABELS = {'360': '360p · 2 Mbps（省带宽）',
                  '720': '720p · 4 Mbps（默认）',
                  '1080': '1080p · 6 Mbps（更吃带宽）',
                  'source': '原始分辨率 · 8 Mbps（不缩放）'}
#: The console lists the presets in this order, cheapest bandwidth first.
QUALITY_ORDER = ('360', '720', '1080', 'source')
#: frames per second, and with `-g` below the keyframe cadence in seconds.
FPS = 24
#: WebRTC's queue is counted in access units rather than bytes, because the
#: unit a viewer either gets or does not is exactly one frame (`_AccessUnits`
#: closes them). Half a second of them is the same budget the browser
#: target's MSE queue gets; past that, a viewer is watching an old picture
#: at live bitrates, which buys nothing. See `_WebRTCBridge`.
WEBRTC_QUEUE_UNITS = FPS // 2
#: A new viewer past this many is refused out loud instead of quietly
#: starving the ones already connected: there is one encoder, one 4-8 Mbps
#: stream, and the promise to each viewer is that it stays live.
WEBRTC_MAX_VIEWERS = 4
#: Seconds a half-negotiated peer may sit before the janitor retires it: the
#: answer never arrived, or the connection never came up. A peer that is not
#: in 'new'/'connecting' is never retired by this clock.
WEBRTC_PEER_TIMEOUT = 30.0
#: How long the offer waits for ICE gathering to finish before shipping
#: whatever it has (host candidates only, on a LAN, are the whole story; the
#: cap exists so a VPN interface cannot hold the page's first paint hostage).
WEBRTC_GATHER_TIMEOUT = 2.0
#: The most the answer POST may claim to be. A real SDP answer is a couple of
#: kilobytes; this is the size past which the `Content-Length` header itself
#: has become the attack, and the body is refused before a byte is read.
WEBRTC_MAX_ANSWER_BYTES = 256 * 1024


def gop_size(kind):
    """Frames between keyframes for the browser and Chromecast targets.

    Until 2026-10 this described the browser target's *publish* cadence: a
    fragmented-MP4 fragment ends when the *next* `moof` shows up, keyframes
    were where `movflags=frag_keyframe` cut, so the GOP was the floor on how
    long a finished picture sat in the encoder before the viewer could have
    it. `frag_every_frame` cut that floor to 1/FPS; what this number still
    buys on `browser` is **recovery granularity** -- a late joiner's replay
    is cut back to the newest fragment whose first sample is a keyframe
    (`_Broadcaster.tail()`), so the keyframe spacing is how far back of
    live such a viewer may start. The MPEG-TS targets are not framed by
    keyframes at all, so their figure is only a seek granularity and a
    second of it is affordable; the DLNA target, which *is* joined
    mid-stream by a television, uses `live_gop` instead.

    On `webrtc` the same half second means something slightly different and
    slightly harder: there is no encoder on the receiving side of that wire
    to ask for a keyframe -- aiortc forwards the packets we hand it, and a
    PLI that arrives is not something this plugin can answer by re-encoding
    -- so the keyframe spacing *is* the recovery latency after a loss, and
    it doubles as how long a viewer that joins mid-picture waits for its
    first complete image. Half a second is the cost of a fast recovery
    there; a full second (the other targets' figure) would be a second of
    smear on every loss.
    """
    return FPS // 2 if kind in ('browser', 'webrtc') else FPS


#: How long a live DLNA stream may go without an IDR, in seconds. At the 25 fps
#: both H.264 profiles advertise that is `-g 6`.
LIVE_GOP_SECONDS = 0.25


def live_gop(fps):
    """Frames between keyframes on a stream a television joins at will.

    Two mechanisms, measured on this machine against its own receiver, and both
    about the *first picture* rather than the running delay -- steady lag did not
    move in a single one of these runs:

      * Matroska closes a Cluster when the next keyframe arrives, and the
        broadcaster cannot publish a cluster whose end it has not seen. On
        `mkv-h264` the GOP therefore *is* the publish cadence: with `-g 25` the
        first picture waited for the next IDR 2.88 / 2.88 s (CPU) and
        3.31 / 3.26 s (VideoToolbox); with `-g 6`, 2.52 / 2.44 s and
        2.47 / 2.44 s.
      * A demuxer whose probe window runs past the opening IDR has to wait for
        the next one, which costs up to a GOP. On `ts-h264` with the CPU
        encoder `-g 25` measured 2.97 / 2.97 / 2.97 s against 2.50 / 2.49 /
        2.50 s for `-g 6`; VideoToolbox is fast enough to be on screen before
        the probe ends, and there the same change is worth nothing
        (2.05 / 2.05 against 2.04 / 2.05).

    What it costs is bandwidth, not quality: 6% on x264 (6.61 -> 7.02 Mbps) and
    9% on VideoToolbox (5.66 -> 6.19 Mbps) on high-motion test material at these
    profiles' own rate control, with SSIM flat across the fourth decimal (0.9983
    against 0.9982, and 0.9985 against 0.9986). Four times as many I-frames, and
    the picture does not visibly pay for them.

    The MPEG-2 profiles keep the `-g` inside `_mpeg2` (`fps * 3 // 5`): PS and
    TS re-synchronise on their own packet headers, neither mechanism above
    applies, and that figure was chosen for DVD-era decoders rather than
    measured here.
    """
    return max(2, int(round(fps * LIVE_GOP_SECONDS)))


#: output kind -> (menu label, HTTP suffix, Content-Type, muxer args)
OUTPUTS = {
    #: `-muxdelay 0 -muxpreload 0`: the TS muxer's default interleave delay is
    #: what a live pipe has to give back -- see the same pair on the DLNA TS
    #: profiles, and why the MPEG-PS shapes do *not* carry it there.
    'cast': ('Chromecast / Google TV', 'ts', 'video/mp2t',
             ['-f', 'mpegts', '-muxdelay', '0', '-muxpreload', '0', 'pipe:1']),
    'browser': ('浏览器（打开网址即可看）', 'm4s', 'video/mp4',
                ['-f', 'mp4', '-movflags',
                 'frag_every_frame+empty_moov+default_base_moof', 'pipe:1']),
    #: Not a container either: these bytes are H.264 NAL units handed to a
    #: WebRTC peer, which repacketises them into RTP without ever decoding
    #: them (aiortc forwards what it is given). No suffix and no Content-Type
    #: for the same reason as caststream -- nothing is served over HTTP; the
    #: page negotiates a session and the media leaves the wire as SRTP.
    'webrtc': ('浏览器 · WebRTC（低延迟 · 此通道无声音）', 'h264', None,
               ['-f', 'h264', 'pipe:1']),
    #: Not a different device: the same Chromecast, driven by its mirroring app
    #: instead of by LOAD. No HTTP suffix and no Content-Type because nothing is
    #: served -- these bytes are pushed to a UDP port.
    'caststream': ('Chromecast 低延迟（实验 · 此通道无声音）', 'h264', None,
                   ['-f', 'h264', 'pipe:1']),
    #: A DLNA TV is told it is downloading a finite file, so everything about
    #: this target -- container, codec, even the file extension in the URL --
    #: comes from the compatibility profile rather than from here. The muxer
    #: slot is None for exactly that reason.
    'dlna': ('DLNA 电视（老电视，无需在电视上装东西）', 'mpg',
             'video/mpeg', None),
}
DEFAULT_OUTPUT = 'cast'
BROWSER_PATH = '/browser'
#: The viewing page's live read of the sender's counters. Same port, same
#: per-session token, one request a second -- and deliberately *not* under
#: `STREAM_PREFIX`, because it is not a media request and must not be counted
#: as one: `end_headers` logs the first few exchanges as the evidence of what a
#: renderer actually asked for, and a once-a-second poll would blow through that
#: limit and bury the requests it exists to record.
BROWSER_STATS_PATH = BROWSER_PATH + '/stats'
#: The WebRTC target's signalling, served by the same tiny HTTP server and
#: behind the same per-session `page_token` (never the standing `Api_Token`,
#: for the same reason the viewing page never gets it). One POST to /session
#: trades an offer for a peer id, one POST to /answer hands the answer back;
#: GET /webrtc itself is the page a viewer opens. Like the stats endpoint
#: these are deliberately *not* under `STREAM_PREFIX` -- they are not media
#: and must not be counted as stream exchanges in the log.
WEBRTC_PATH = '/webrtc'
WEBRTC_SESSION_PATH = WEBRTC_PATH + '/session'
WEBRTC_ANSWER_PATH = WEBRTC_PATH + '/answer'


class _DlnaProfile(object):
    """One 'shape' of the infinite file a DLNA TV is asked to download.

    Old renderers are a compatibility matrix, not a spec: which one of these
    works is a property of the TV, which is why the menu offers all five and
    the watchdog tells you to switch. Facts (container, CBR rates, GOP, the
    advertised size/duration pair) come from the MirrorCast reference study in
    docs/Casting-Suite-Plan.md 2.1; **no real old TV has been tested** -- see
    the same section for what that means for these claims.
    """

    def __init__(self, label, muxer, suffix, content_type, org_pn,
                 video_args, audio_args, width, height, fps, bitrate,
                 audio_bitrate):
        self.label = label
        self.muxer = muxer                      # ffmpeg args, minus pipe:1
        self.suffix = suffix
        self.content_type = content_type
        self.org_pn = org_pn
        self.video_args = video_args
        self.audio_args = audio_args
        #: width 0 means "keep the desktop's aspect and scale by height". Only
        #: the MPEG-PS shapes pin an exact frame: the DVD-derived muxers refuse
        #: anything else, and a 3400-pixel-wide Retina grab is not 720.
        self.width = width
        self.height = height
        self.fps = fps
        self.bitrate = bitrate
        #: AC3 is 192k and AAC is 128k, and the advertised file size is the
        #: *total* rate: a TV that cross-checks size against duration sees a
        #: lie of ~4% if only the video rate goes into the arithmetic.
        self.audio_bitrate = audio_bitrate

    @property
    def total_bitrate(self):
        return self.bitrate + self.audio_bitrate

    def video_filter(self):
        """Anamorphic on purpose: the PS shapes squeeze a 16:9 desktop into a
        4:3 frame and then tell the TV to stretch it back, which is exactly what
        a DVD does -- and what the reference implementation is measured on."""
        if self.width:
            return 'scale={}:{}'.format(self.width, self.height)
        return 'scale=-2:{}'.format(self.height)


def _mpeg2(fps, bitrate):
    """CBR MPEG-2: the rate control matters as much as the codec, because the
    TV models a fullness buffer and a variable bitrate reads as starvation."""
    return ['-c:v', 'mpeg2video', '-b:v', str(bitrate),
            '-minrate', str(bitrate), '-maxrate', str(bitrate),
            '-bufsize', '2304k', '-g', str(fps * 3 // 5), '-r', str(fps),
            '-pix_fmt', 'yuv420p']


#: profile id -> shape. Ordered from "most likely to work on an old TV" to
#: "modern renderer that only speaks TS/MKV"; the watchdog walks this list
#: forward when the TV keeps refusing.
#:
#: The muxer lists carry `-muxdelay 0 -muxpreload 0` on the TS and MKV shapes
#: only: measured against this ffmpeg build, MPEG-PS (`vob`) *underflows its own
#: VRV model* with a zero delay and warns on every stream, so the DVD muxer
#: keeps its default interleaving delay.
_AC3 = ['-c:a', 'ac3', '-b:a', '192k', '-ar', '48000', '-ac', '2']
_AAC = ['-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2']
#: Every one of these shapes ends with the drift correction in
#: `AUDIO_RESAMPLE`: see there.
_AUDIO = [list(one) + AUDIO_RESAMPLE for one in (_AC3, _AAC)]
DLNA_PROFILES = {
    'ps-pal': _DlnaProfile(
        'MPEG-PS · PAL 576p（老电视首选）', ['-f', 'vob'], 'mpg', 'video/mpeg',
        'MPEG_PS_PAL', _mpeg2(25, 4500000), _AUDIO[0],
        720, 576, 25, 4500000, 192000),
    'ps-ntsc': _DlnaProfile(
        'MPEG-PS · NTSC 480p（北美/日本老电视）', ['-f', 'vob'], 'mpg',
        'video/mpeg', 'MPEG_PS_NTSC', _mpeg2(30, 4500000), _AUDIO[0],
        720, 480, 30, 4500000, 192000),
    'ts-mpeg2': _DlnaProfile(
        'MPEG-TS · MPEG-2 576p（认 TS 不认 PS 的电视）',
        ['-f', 'mpegts', '-muxdelay', '0', '-muxpreload', '0'],
        'ts', 'video/vnd.dlna.mpeg-tts', 'MPEG_TS_SD_EU',
        _mpeg2(25, 5000000), _AUDIO[0],
        720, 576, 25, 5000000, 192000),
    'ts-h264': _DlnaProfile(
        'MPEG-TS · H.264 720p（较新的电视，清晰度更高）',
        ['-f', 'mpegts', '-muxdelay', '0', '-muxpreload', '0'],
        'ts', 'video/vnd.dlna.mpeg-tts', None, None, _AUDIO[1],
        0, 720, 25, 6000000, 128000),
    'mkv-h264': _DlnaProfile(
        'Matroska · H.264 720p（只认 MKV 的电视/Kodi）',
        ['-f', 'matroska', '-muxdelay', '0', '-muxpreload', '0'],
        'mkv', 'video/x-matroska', None, None, _AUDIO[1],
        0, 720, 25, 6000000, 128000),
}
#: What an install that never touched「兼容档位」gets, per answer shape -- see
#: `default_dlna_profile_id`. `ps-pal` is the historical one and stays the
#: answer for the file shape; the live shape is served by `ts-h264`.
DEFAULT_DLNA_PROFILE = 'ps-pal'
LIVE_DEFAULT_DLNA_PROFILE = 'ts-h264'
#: The element a Matroska file stops being a header at: the first Cluster.
#: Everything before it (EBML header + Segment + SeekHead + Info + Tracks) is
#: written exactly once, at the head of the stream -- 1717 bytes measured.
#:
#: That is fatal for a live joiner. MPEG-PS and MPEG-TS re-synchronise on their
#: own packet headers, so a viewer that connects mid-stream can read them; a
#: Matroska reader that never saw the Tracks element cannot name a single codec.
#: Measured against this repository's own receiver: pointing mpv at a live
#: `mkv-h264` session produced 3.7 MB of *later* bytes, one clean `200 GET`,
#: `matroska,webm: EBML header parsing failed`, and a player that sits idle for
#: the rest of the session -- it does not retry the probe. Waiting before the
#: push does not help either (same command, receiver started 0.5-4 s later, with
#: up to 3.7 MB already produced: every case fails). So the header has to be
#: held back and written onto every connection, which is what the browser
#: target already does for its fMP4 init segment -- see `_Broadcaster._retain`.
#:
#: Retaining the header is necessary and, on its own, not enough: measured
#: again with it in place, a viewer that joined 10 s into the session was handed
#: a stream `ffprobe` parsed as Matroska with both tracks (so the header
#: half-worked) and still sat in mpv's demuxer probe forever. The other half is
#: that the bytes after the header started in the *middle* of a Cluster, and
#: Matroska has no packet marker to re-synchronise on -- see `_Clusters`.
MKV_FIRST_CLUSTER = b'\x1f\x43\xb6\x75'
#: The container every other top-level element lives inside. A live muxer gives
#: it no size of its own (measured: `Segment` with the all-ones length), which
#: is why the framer descends into it instead of skipping past it.
MKV_SEGMENT = b'\x18\x53\x80\x67'


def profile_needs_header(profile):
    """Does this container have to start with a one-time header to be readable?

    The DVD-era shapes are self-synchronising and the answer is no; Matroska is
    the one profile in `DLNA_PROFILES` where the answer is yes.
    """
    return 'matroska' in (profile.muxer or ())


def _ebml_id(buf, pos=0):
    """The element ID at `pos`, and how many bytes it is: 1-4, by its own high
    bits. `(None, 0)` when the buffer does not hold a whole ID."""
    if pos >= len(buf):
        return None, 0
    for bit, length in ((0x80, 1), (0x40, 2), (0x20, 3), (0x10, 4)):
        if buf[pos] & bit:
            if pos + length > len(buf):
                return None, -length
            return bytes(buf[pos:pos + length]), length
    return None, 0


def _ebml_size(buf, pos):
    """The element's data length, in Matroska's self-describing integers: the
    marker bit that ends the leading zeros says how many bytes follow.

    Returns `(value, length)`, `('unknown', length)` for the all-ones value a
    streaming muxer writes when it has no idea how long the element will run,
    and a `length` of 0 (unreadable) or a negative one (still arriving).
    """
    if pos >= len(buf):
        return None, 0
    first = buf[pos]
    if not first:
        return None, 0
    length, mask = 1, 0x80
    while not first & mask:
        mask >>= 1
        length += 1
        if length > 8:
            return None, 0
    if pos + length > len(buf):
        return None, -length
    value = first & (mask - 1)
    for byte in buf[pos + 1:pos + length]:
        value = (value << 8) | byte
    if value == (1 << (7 * length)) - 1:
        return 'unknown', length
    return value, length

#: How much of the stream the TV gets before we hand it the URL, **in seconds
#: of picture**. The renderer's first move is a bounded sniff, and the point of
#: prefilling is that the answer comes from memory instead of stalling on the
#: encoder -- but the old 20 MiB fixed budget made that cost 27-37 seconds of
#: latency at these bitrates, which is why the budget is now derived from the
#: profile's bitrate and clamped.
#:
#: This number is not a startup cost that goes away: the TV starts reading from
#: the head of what we buffered and never catches up, so the prefill *is* how
#: far behind live this target runs for the whole session. 6 s measured out to
#: 5.4 s on ps-pal once the floor clamped it, which is what「投到电视上延迟明显」
#: is; 4 s with a 2 MiB floor is the same shape with a second and a half less
#: delay, and still several times a keyframe interval (3-frame GOP), which is
#: the only thing the sniff actually needs.
DLNA_PREFILL_SECONDS = 4
DLNA_PREFILL_MIN_BYTES = 2 << 20
DLNA_PREFILL_MAX_BYTES = 8 << 20
#: The knob's ends. 1 s is the smallest that still covers several keyframe
#: intervals, which is all the receiver's sniff actually needs; 8 s is where
#: "buffering" stops being a compromise and starts being a recording.
#:
#: The **default stays at the measured 4** rather than dropping to the floor,
#: and that is a decision, not an oversight: MirrorCast's own measurement on a
#: real Philips 43PFS5301 (2016, non-Android) prefills ~20 MiB, i.e. 20-25 s,
#: and says so openly -- so firmware that needs more than our floor exists in
#: the wild, and we have no such television here to find where ours is. A
#: number we cannot measure becomes a knob the user can, with the cost printed
#: next to it; guessing a smaller default would just move the failure to a
#: machine we cannot see.
DLNA_PREFILL_MIN_SECONDS = 1
DLNA_PREFILL_MAX_SECONDS = 8


def dlna_prefill_seconds_setting():
    """How many seconds the user asked the DLNA target to buffer.

    A stored value outside the range is clamped rather than rejected: the
    alternative is a television that stops playing because somebody typed 40
    into the settings JSON, and that failure arrives with no explanation.
    """
    try:
        value = int(Setting.get(SettingProperty.Mirror_Dlna_Prefill,
                                DLNA_PREFILL_SECONDS))
    except (TypeError, ValueError):
        return DLNA_PREFILL_SECONDS
    return max(DLNA_PREFILL_MIN_SECONDS,
               min(DLNA_PREFILL_MAX_SECONDS, value))


def dlna_prefill_bytes(profile):
    """The prefill budget for one DLNA shape, in bytes.

    One positional argument on purpose. The regression suite replaces this
    function with a stub of exactly that shape to take the buffer out of a
    test, and a second parameter would turn every such stub into a TypeError
    somewhere that has nothing to do with what the stub is testing.
    """
    return int(max(DLNA_PREFILL_MIN_BYTES,
                   min(DLNA_PREFILL_MAX_BYTES,
                       profile.total_bitrate
                       * dlna_prefill_seconds_setting() // 8)))


def dlna_prefill_seconds(profile):
    """What that budget costs the user, in seconds -- the start message says so.

    Derived from the byte budget and not read back from the setting, so that a
    budget the clamp moved (2 MiB floor, 8 MiB ceiling) still reports the
    delay the user is actually getting. Those two answers disagreeing is worse
    than either of them being approximate.
    """
    return max(1, dlna_prefill_bytes(profile) * 8 // profile.total_bitrate)

#: Ring size: 48 MiB of produced bytes stay addressable by absolute offset.
DLNA_RING_BYTES = 48 << 20
#: The advertised file must stay under 2 GiB. Some firmware does signed 32-bit
#: arithmetic on Content-Length, which turns 3.9 GB into
#: `Range: bytes=0-18446744072566584319` and "this file is unsupported".
DLNA_MAX_ADVERTISED_SIZE = 1900000000
DLNA_SECONDARY_HEADER = 'Streaming'
#: `DLNA.ORG_FLAGS` bits, named because the wire form is 32 hex digits of which
#: a reader can verify exactly nothing. What this file shipped for two releases
#: was the bare string `01500000…`, and the only way to know that means
#: STREAMING | BACKGROUND | DLNA_V15 is to already know it.
DLNA_FLAG_STREAMING_TRANSFER_MODE = 1 << 24
DLNA_FLAG_BACKGROUND_TRANSFER_MODE = 1 << 22
#: "This stream may stall while the renderer waits for data." Not a hedge -- a
#: description of what our own HTTP layer does. When a renderer reads faster
#: than the encoder writes, the byte log **waits** instead of answering short
#: (AGENTS.md §4.8, red line ③), so the connection genuinely goes quiet, and
#: firmware that was not told about that is firmware that concludes the file
#: ended. VLC declares the bit (`01700000…`); MirrorCast, whose flags we
#: copied, does not (`01500000…`).
#:
#: **So does our own sibling sender, and it always has**: `cast_local_file.py`
#: has shipped the literal `01700000…` since its first release, because its
#: transcode pump parks a reader that outruns ffmpeg for exactly the same
#: reason. Until v0.19 this file was the odd one out *in its own repository* --
#: two plugins that behave identically were telling televisions two different
#: things, and nothing could notice because nothing compared them. Part 23 now
#: reads the word out of that file instead of retyping it, and asserts it
#: occurs there exactly once, so a second flags word cannot hide a drift.
#:
#: **Whether any real television changes its behaviour because of this is
#: unverified and cannot be verified here** -- there is no DLNA renderer on
#: this LAN that stalls. The claim being made is the weaker and defensible one:
#: declaring a stall we really do cause is honest, and the bit is in the
#: specification.
DLNA_FLAG_CONNECTION_STALL = 1 << 21
DLNA_FLAG_DLNA_V15 = 1 << 20
#: The flags word occupies the first four bytes; the remaining twelve are
#: reserved and every field implementation sends them as zeros.
DLNA_ORG_FLAGS = '{:08x}{}'.format(
    DLNA_FLAG_STREAMING_TRANSFER_MODE | DLNA_FLAG_BACKGROUND_TRANSFER_MODE
    | DLNA_FLAG_CONNECTION_STALL | DLNA_FLAG_DLNA_V15, '0' * 24)
#: An MPEG-PS padding packet (private_stream_1, zero length). A renderer that
#: asks for exactly n bytes gets exactly n bytes -- it is sniffing a file, and
#: a short answer is what makes it give up.
PS_PADDING = b'\x00\x00\x01\xbe\x00\x00'
DLNA_POLL_SECONDS = 5.0
#: How far past the encoder's newest byte a `Range` may start before it is read
#: as a probe of the size we advertised rather than a reconnect. A renderer that
#: resumes asks for the next byte it expects, which the encoder has almost
#: always already produced; something hunting for a container index asks for an
#: offset megabytes or gigabytes ahead of anything that exists.
DLNA_MAX_AHEAD_BYTES = 4 << 20
#: How long a bounded sniff waits for the encoder before we pad the rest.
#: Longer than a keyframe interval, shorter than most TVs' own read timeout.
DLNA_SNIFF_TIMEOUT = 10.0
#: Ceiling on the prefill wait: a slow encoder must not hold the session open
#: forever, and the TV is better off starting laggy than never starting.
DLNA_PREFILL_TIMEOUT = 45.0
DLNA_MAX_REPUSHES = 8
#: How many re-pushes may be spent on a renderer that fetched the URL and then
#: read nothing of it. Fewer than `DLNA_MAX_REPUSHES` by design: those eight are
#: budgeted for a television that keeps stopping on its own, where a fresh
#: `SetAVTransportURI` is a genuine second chance. A player that opened the
#: stream, was answered, and took no byte has already told us what it thinks of
#: the shape we chose, and the identical URL will get the identical refusal --
#: measured with the file shape against mpv, which spent all eight re-pushes
#: (~40 s) on a `Range` hunt it lost every single time.
DLNA_MAX_REFUSALS = 2
#: How many times one mirror run may change the compatibility shape on its own.
#: The page has always offered all five shapes and the console has always named
#: the next one, but a user who is not sitting at the settings page -- the whole
#: point of a mirror -- was left watching a refusal they could not see. So the
#: watchdog now spends a rung when the evidence says the *container* is what was
#: refused, and says out loud which one it moved to. Bounded, because every step
#: restarts the encoder and costs the seconds a re-push would not.
#: `len(DLNA_PROFILES) - 1` is the honest ceiling: starting from any rung, that
#: many switches visits every other one exactly once.
DLNA_MAX_PROFILE_TRIES = len(DLNA_PROFILES) - 1
DLNA_SERVICE = 'urn:schemas-upnp-org:service:AVTransport:1'
DLNA_SEARCH_TARGETS = ('urn:schemas-upnp-org:device:MediaRenderer:1',
                       DLNA_SERVICE)
SSDP_ADDR = '239.255.255.250'
SSDP_PORT = 1900

_devices = []
_searching = False
#: wall clock of the last completed Chromecast search; 0.0 = never. The console
#: reads it to tell「still looking」from「looked, found nothing」-- without it
#: a LAN with no device to find made the menu read「搜索中」on every open.
_searched_at = 0.0
#: Why the last Chromecast search could not run at all (no zeroconf, a socket
#: that would not bind).「没有发现」and「这台机器搜不了」are different things to
#: read: only one of them is fixed by switching the TV on.
_search_error = ''
#: How many answers of the last search were this machine's own services, per
#: probe. 「没有发现 Chromecast」and「只有这台 Mac 自己（已排除）」are two different
#: answers: the first sends the reader to check the TV's power cable, the second
#: already tells them the search ran and the LAN simply has nobody else on it.
_self_alone = {'cast': 0, 'dlna': 0}
#: Opening the console again inside this window reuses the cached answer instead
#: of kicking another multicast search (see `_search_due`).
SEARCH_REFRESH_SECONDS = 15.0
#: …and a list that is *not* empty is re-checked this often, so a Chromecast that
#: wakes up later turns up on the page without anyone pressing「重新搜索」.
STALE_SEARCH_SECONDS = 300.0
#: How long one browse lasts, and how long a single hit may take to resolve.
#: The browse is a fixed sleep, so these two numbers are the page's first
#:「一台都没找到」arriving: 3 s of silence plus 2 s per device used to mean a
#: LAN with three Chromecasts took eleven seconds to answer.
DISCOVER_TIMEOUT = 2.0
DISCOVER_RESOLVE_TIMEOUT = 800
_search_lock = threading.Lock()
#: cached result of `ffmpeg -encoders`, because the menu must never spawn it
_hw_encoder_cache = {}


class DiscoveryError(Exception):
    """Discovery could not run, which is not the same as finding nothing."""


def discover(timeout=DISCOVER_TIMEOUT):
    """[(friendly name, host, port)] for Chromecasts answering on the LAN.

    Raises DiscoveryError when the search itself was impossible; an honest
    empty list then means nobody answered.

    Our own receiver is not among them. Macast broadcasts `_googlecast._tcp`
    like any Chromecast does, so a machine with the Chromecast receiver enabled
    finds *itself* first -- and mirroring a screen into the player that is
    showing it is a loop, not a target. The DLNA search drops itself the same
    way (`discover_renderers`).
    """
    try:
        from zeroconf import Zeroconf, ServiceBrowser
    except ImportError:
        raise DiscoveryError('zeroconf 没有装，无法用 mDNS 搜索 Chromecast')
    found = {}

    class _Listener(object):
        def add_service(self, zc, type_, name):
            info = zc.get_service_info(type_, name,
                                       timeout=DISCOVER_RESOLVE_TIMEOUT)
            if info is None:
                return
            addresses = info.parsed_addresses()
            if not addresses:
                return
            properties = {}
            for key, value in (info.properties or {}).items():
                if isinstance(key, bytes):
                    key = key.decode('utf-8', 'replace')
                if isinstance(value, bytes):
                    value = value.decode('utf-8', 'replace')
                properties[key] = value
            found[name] = (properties.get('fn') or info.server or name,
                           addresses[0], info.port)

        def update_service(self, *args):
            pass

        def remove_service(self, zc, type_, name):
            found.pop(name, None)

    try:
        zeroconf = Zeroconf()
    except Exception as e:
        raise DiscoveryError(
            '这台机器起不了 mDNS 服务（没有可用网卡或被防火墙拦住）：{}'.format(e))
    try:
        ServiceBrowser(zeroconf, SERVICE_TYPE, _Listener())
        time.sleep(timeout)
    except Exception as e:
        logger.error("Chromecast discovery failed: %s", e)
        raise DiscoveryError('Chromecast 搜索失败：{}'.format(e))
    finally:
        zeroconf.close()
    try:
        ours = set(Setting.get_advertisable_ip())
    except Exception:
        # Failing to name our own addresses must not empty the list: a device
        # list with nothing in it reads as "no TV on the LAN".
        ours = set()
    others = [hit for hit in found.values() if hit[1] not in ours]
    _self_alone['cast'] = len({hit[1] for hit in found.values()}
                              - {hit[1] for hit in others})
    return sorted(others)


def start_search():
    global _searching
    with _search_lock:
        if _searching:
            return False
        _searching = True
    threading.Thread(target=_search, daemon=True,
                     name="SCREEN_MIRROR_SEARCH").start()
    return True


def _search():
    """One Chromecast search, keeping whatever it concluded.

    The thread must not die on a discovery that throws: that is how a search
    which ran once and failed left the console reading「正在搜索」forever.
    """
    global _devices, _searching, _searched_at, _search_error
    try:
        _devices = discover()
        _search_error = ''
    except DiscoveryError as e:
        _search_error = str(e)
        logger.error('chromecast search failed: %s', e)
    except Exception as e:
        _search_error = 'Chromecast 搜索失败：{}'.format(e)
        logger.error('chromecast search failed: %s', e)
    finally:
        with _search_lock:
            _searching = False
            _searched_at = time.time()


def _search_due(searched_at, now, found=False):
    """Whether a fresh multicast search is warranted for this protocol.

    Never searched counts as due, so the first open always looks. A completed
    search younger than its interval does not -- and that is what stops the
    console's one-second polling from keeping the LAN busy forever, which is
    how the menu came to read「搜索中…再展开一次菜单」forever on a LAN that has
    no device to find. An empty list asks again sooner than a list that found
    something: nothing to show is the case worth chasing.
    """
    interval = STALE_SEARCH_SECONDS if found else SEARCH_REFRESH_SECONDS
    return searched_at == 0.0 or (now - searched_at) >= interval


def _searched_suffix(searched_at):
    """「（搜于 08:47:31）」 once a search has completed, '' before the first."""
    if not searched_at:
        return ''
    return '（搜于 {}）'.format(
        time.strftime('%H:%M:%S', time.localtime(searched_at)))


# -- 说给用户听的话 ----------------------------------------------------------
#
# Every user-visible sentence this plugin produces goes through `notify`, which
# fires the system notification *and* writes it to `macast/notice.py`. A mirror
# runs for minutes with its control page sitting in a background tab, so a
# message that only reaches Notification Center is one the user reads after the
# fact, if at all. The board is what the「电脑投屏」tab shows them when they
# come back.


def notify(message, sound=False):
    """Tell the user something, on the message board and as a notification.

    Both, because the notification is what catches them when they are elsewhere
    and the board is what they can still read ten minutes later -- a recipe for
    `brew install …` that only ever reached Notification Center was the report
    this replaces: gone before it could be copied.

    The board is written first and the balloon is fire-and-forget on purpose:
    `publish` re-raises a subscriber's failure into its caller, and the callers
    here are the failure reporters -- `_fail` used to lose the teardown on its
    own next line to pystray's 256-character buffer.
    """
    notice.record(message)
    try:
        cherrypy.engine.publish('app_notify', 'Macast', message, sound=sound)
    except Exception as e:
        logger.warning('app_notify failed: %s', e)


def recent_messages(limit=20):
    """The app's recent sentences, oldest first (for the console feed).

    The whole board, not just this plugin's: the missing thing on a machine
    where「屏幕镜像」won't start is often a binary another plugin looks for.
    """
    return notice.recent(limit)


class SettingProperty(Enum):
    Mirror_Target = 1
    Mirror_Target_Name = 2
    Mirror_Quality = 3
    #: CoreAudio id of the aggregate we created, kept so a second run reuses
    #: it instead of stacking another one.
    Mirror_Audio_Aggregate = 4
    #: CoreAudio id of the user's real speakers, so「恢复原声音输出」can go back.
    Mirror_Audio_Original = 5
    #: 'cast' | 'caststream' | 'browser' | 'dlna' -- see OUTPUTS
    Mirror_Output = 6
    #: avfoundation video device index to capture; '' means "first screen".
    Mirror_Screen = 7
    #: draw the pointer. On by default; JustStream calls it「显示鼠标指针」.
    Mirror_Cursor = 8
    #: 'software' | 'hardware' (h264_videotoolbox, macOS only)
    Mirror_Encoder = 9
    #: key into DLNA_PROFILES -- which container/codec an old TV swallows is a
    #: property of the TV, so the user (and the watchdog) picks it.
    Mirror_Dlna_Profile = 10
    #: AVTransport control URL of the chosen DLNA renderer. Its own key
    #: because Mirror_Target holds `host:port` for a Chromecast.
    Mirror_Dlna_Control = 11
    #: 'live' | 'file' -- how the DLNA target answers a renderer. See
    #: DLNA_SHAPES: the live shape is the default because it is the only one
    #: measured to play on both clients we can test against.
    Mirror_Dlna_Shape = 12
    #: Seconds of picture the DLNA target buffers before handing the stream to
    #: the television. Its own key because on that target this number *is* the
    #: latency -- see DLNA_PREFILL_SECONDS.
    Mirror_Dlna_Prefill = 13
    #: Seconds the browser player parks itself behind the live edge. Same
    #: shape of fact as the one above, on the other end of the pipe: it is a
    #: standing delay the viewer never catches up from -- see LIVE_EDGE_SECONDS.
    Mirror_Live_Edge = 14
    #: Hours a television target may run before it stops itself -- see
    #: MAX_DURATION_HOURS. Only the two TV shapes obey it, so the key is worth
    #: a page row only there; a browser viewer can simply reload.
    Mirror_Max_Duration = 15


# -- ffmpeg ----------------------------------------------------------------

def find_ffmpeg():
    """Path to an ffmpeg binary, or None.

    PATH first, then the directories a Finder-launched app never sees.
    """
    found = shutil.which("ffmpeg")
    if found:
        return found
    for candidate in ("/opt/homebrew/opt/ffmpeg/bin/ffmpeg",
                      "/opt/homebrew/bin/ffmpeg",
                      "/usr/local/opt/ffmpeg/bin/ffmpeg",
                      "/usr/local/bin/ffmpeg",
                      "/opt/local/bin/ffmpeg"):
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


#: Where ffmpeg comes from on each platform. A notification is gone in five
#: seconds, so the same words live on the console's 「需要安装」 card.
FFMPEG_WAY = {'darwin': 'brew install ffmpeg',
              'win32': '从 ffmpeg.org 下载，并把解压出的 bin 目录加入 PATH'}


def ffmpeg_requirement(found=None):
    """Report ffmpeg's state to the message board; return the binary.

    Called from every place that was about to give up for the want of it, so
    the card appears whether the user pressed 开始镜像, opened the preview, or
    ran the sound installer -- and disappears on the first call that finds it.
    """
    if found is None:
        found = find_ffmpeg()
    if found:
        notice.satisfied('ffmpeg')
        return found
    way = FFMPEG_WAY.get(
        sys.platform, '用发行版的包管理器安装 ffmpeg，例如 apt install ffmpeg')
    notice.requirement(
        'ffmpeg',
        label='电脑投屏需要 ffmpeg',
        detail='截屏、编码和预览都是它做的，Macast 只负责指挥它。装上之后点「重新扫描」，'
               '不用重启应用。',
        command=way if sys.platform == 'darwin' else '')
    return None



class _Capture(object):
    """What one probe decided: how to grab this machine's screen, and system
    audio too if a sink for it exists."""

    def __init__(self, label, inputs, audio_map=None, screens=None,
                 method='', spec=None):
        self.label = label        # for the menu / logs
        self.inputs = inputs      # one list of input args per ffmpeg -i
        self.audio_map = audio_map  # '0:a:0' / '1:a:0' / None
        #: [(index, device name)] for the display picker: avfoundation's screen
        #: devices on macOS, ddagrab's DXGI outputs on Windows (0.20+), empty
        #: where the platform has no list to offer (Linux).
        self.screens = screens or []
        #: How this grab was built: 'ddagrab' / 'gdi' on Windows, 'sck' on
        #: macOS 13+ (an in-process ScreenCaptureKit feeder), '' elsewhere.
        #: The mirror's runtime fallback keys on it -- ddagrab can refuse a
        #: display at attach time ("Generic error in an external library" on a
        #: desktop the DWM will not duplicate), sck can be denied at
        #: SCStream start, and only this field tells the starter thread that
        #: the refusal it just watched is that one.
        self.method = method
        #: Build parameters the in-process feeder needs and ffmpeg does not:
        #: 'sck' carries {'display_id', 'size': (w, h), 'cursor'}. None for
        #: every ffmpeg-driven method.
        self.spec = spec


#: probe results are cached per (ffmpeg, platform): probing spawns ffmpeg, and
#: the menu must never block on it. The cursor and screen choices live in
#: `Setting`, so changing one has to clear this cache -- see
#: invalidate_capture_cache().
_capture_cache = {}


def invalidate_capture_cache():
    _capture_cache.clear()
    # The ddagrab attach test is cached separately (it spawns one ffmpeg per
    # output), so it has to be cleared here too or a cursor/screen change would
    # keep serving the old answer. `_ddagrab_refused` and `_sck_refused` are
    # deliberately NOT cleared: a display that refused to be duplicated will
    # refuse again in this process, and forgetting that would send the next
    # session back into the same multi-second refusal.
    _ddagrab_cache.clear()


def cursor_enabled():
    """Whether to draw the pointer. Default on -- and because Setting.get has
    side effects, "off" is the only value ever stored for this key."""
    return Setting.get(SettingProperty.Mirror_Cursor, True) is not False


def output_kind():
    """The chosen target, validated against OUTPUTS."""
    kind = str(Setting.get(SettingProperty.Mirror_Output, DEFAULT_OUTPUT)
               or DEFAULT_OUTPUT)
    return kind if kind in OUTPUTS else DEFAULT_OUTPUT


#: What an untouched install holds. VideoToolbox stays the default on a Mac, but
#: the reason it used to give was measured and found false, so here is the reason
#: that survives measurement.
#:
#: This comment used to say "x264 at `ultrafast` cannot keep a Retina desktop at
#: a watchable frame rate, which is what「不开硬件就几乎看不到画面」was". It
#: cannot, and undoing that claim has two halves -- the second of which this
#: block itself got wrong for one revision:
#:
#: **Throughput (offline).** libx264 ultrafast+zerolatency handed a *file* and
#: told to go as fast as it can measures 486 fps at 1920x1080, 295 fps at
#: 2560x1600 and 171 fps at the native 3456x2234 -- i.e. 5.6x-17.9x realtime,
#: costing 4.20 / 4.63 / 4.94 cores. Every one of those numbers is real. None of
#: them is what mirroring costs, because mirroring never asks for more frames
#: than the capture hands over.
#:
#: **Cost (live).** Fed the real 24 fps desktop capture, the same argv costs
#: **0.59 of a core at 1080p** against VideoToolbox's **0.41** (2026-10-02,
#: three runs each, `scripts/encoder_latency_probe.py`; at 2160p it is 1.12
#: against 0.64 for hardware HEVC). So the tradeoff that is real is ~200 ms of
#: first byte against **0.18 of a core**, not 41 ms against 4.5 cores -- quoting
#: the offline ceiling as the live cost made the hardware default look twenty
#: times cheaper than it is.
#:
#: And a user who reported「不开硬件就几乎看不到画面」was reporting something
#: else entirely: a capture that produced **zero frames**, of which this file
#: shipped two independent causes (`vt_level` pinning a level too small for the
#: picture, and the missing output `-r` pin in build_ffmpeg_command). Both were
#: invisible on the hardware path and both pointed the reader at
#: PERMISSION_DOOR, which is a real door but not the one they needed.
#:
#: VideoToolbox's ~200 ms is not a setting we forgot to turn: `-realtime`,
#: `-prio_speed`, `+constant_bit_rate`, `+max_ref_frames 1`, `-bf 0` and
#: `-coder cabac` were each tried and none of them moved it (and `-realtime 1`
#: produced byte-identical output to not passing it -- it only selects the
#: low-delay rate-control path, it does not buy latency here). It is the
#: hardware pipeline's own depth.
#:
#: Windows joined this default on 2026-10-02, when `hardware_encoder()` started
#: answering `h264_nvenc` there: the same argument, measured on that machine's
#: own argv (0.44 of a core for x264 against 0.21 for NVENC, both at 24 fps and
#: ~19 MB at `-b:v 6000000`). What was **not** measured there is the first-frame
#: difference, which is why `ENCODER_TRADEOFF_WIN32` refuses to quote macOS's
#: 200 ms on the other machine's behalf.
#:
#: So `auto` still lands on hardware -- but that is now a **default whose reason
#: got weaker**, not a verdict: 0.2 of a core is cheap and 200 ms is not, and
#: flipping the default is a user decision rather than a measurement, so it has
#: not been made here. Either way the page has to say the tradeoff out loud
#: (ENCODER_TRADEOFF), because a switch that reads as「硬件 = 好，软件 = 差」is
#: a switch nobody turns off. The resolution has to come from a probe that
#: may not have answered yet, hence "hardware if something already proved it
#: exists, otherwise software".
ENCODER_AUTO = 'auto'

#: The one sentence the encoder switch on the page has to carry, because without
#: it the switch reads as「硬件 = 好，软件 = 差」and nobody turns it off. Both
#: numbers are measured on this machine, on a live desktop capture (not a file
#: source -- see ENCODER_AUTO for why the distinction is the whole point), and
#: they are the entire tradeoff: there is no third axis. VT also undershoots its
#: bitrate target by 30%, which is why rate_target() asks it for 1.5x, but that
#: is correctness, not a quality difference the user has to choose between.
#:
#: Both readings carry ~0.5 s of ffmpeg startup, so the sentence quotes them as
#: a *difference* and says so -- a user who times it with a stopwatch and gets
#: 700 ms will otherwise conclude the page lied.
ENCODER_TRADEOFF = ('关掉硬件编码大约省下 200 毫秒的首帧延迟（实测同一台机器、'
                    '同一份采集：x264 509-529 毫秒，VideoToolbox 706-753 毫秒，'
                    '两边都含约 0.5 秒的 ffmpeg 启动，所以要看的是差值）。'
                    '代价是 CPU：1080p 下 0.59 核对 0.41 核。'
                    '这不是「软件编码跟不上」——两者都跑满 24 fps——'
                    '只是多耗约 0.2 个核心：笔记本上会发热、耗电。')

#: The same sentence for the machine where「硬件编码」is NVENC, not VideoToolbox.
#: Written as its own string because the two halves of the tradeoff are not
#: equally known here: the CPU cost was measured on .68 with the shipped argv,
#: while the first-frame difference was measured only on macOS. A Windows user
#: who reads macOS's 200 ms would be reading somebody else's machine, so this
#: one gives the number it has and says out loud which one it does not.
#:
#:     0.44 of a core  libx264  (the shipped argv, 1080p, 24 fps, 1.00x)
#:     0.21 of a core  h264_nvenc (same argv shape, ~19 MB at -b:v 6000000)
#:
#: The byte totals matching is the point of quoting them: it says both rows did
#: the same work, so 0.44 vs 0.21 is a price difference rather than a quality
#: difference nobody agreed to pay.
ENCODER_TRADEOFF_WIN32 = ('关掉硬件编码的代价是 CPU：这台机器上实测同一份采集、'
                          '同样跑满 24 fps、同样的码率（约 19 MB / 25 秒 @6 Mbps），'
                          'x264 吃 0.44 个核心，NVENC 吃 0.21 个。'
                          '首帧延迟的差值我没有在 Windows 上量过——Mac 上量到的是'
                          '硬件晚约 200 毫秒，这里不替那台机器转述这个数字。')


def encoder_tradeoff(platform=None):
    """The tradeoff sentence for the encoder this platform's switch means."""
    if hardware_encoder(platform) == _NVENC:
        return ENCODER_TRADEOFF_WIN32
    return ENCODER_TRADEOFF


def encoder_kind():
    """'software' | 'hardware' -- which of `encoder_args`' two shapes to ship.

    "Hardware" is only a name this file can honour where `hardware_encoder()`
    has an entry for the platform (macOS: VideoToolbox, Windows: NVENC), so the
    old `sys.platform == 'darwin'` in two places here was not a platform fact --
    it was the same single-name table the previous paragraph describes, spelled
    inline. Linux falls through to software and means it.

    Never spawns ffmpeg: this is consulted while the menu and the console are
    being built, so `auto` may only read a probe answer that a mirror start (or
    `selfcheck.py`) already produced. A user who picks 硬件编码 explicitly gets
    that wish stored, and `_mirror` falls back to x264 with a warning naming the
    encoder the machine turned out not to offer.
    """
    kind = str(Setting.get(SettingProperty.Mirror_Encoder, ENCODER_AUTO)
               or ENCODER_AUTO).strip().lower()
    if kind == 'hardware':
        return 'hardware' if hardware_encoder() else 'software'
    if kind != ENCODER_AUTO:
        return 'software'
    if not hardware_encoder():
        return 'software'
    return ('hardware' if _hw_encoder_cache.get((find_ffmpeg(), sys.platform))
            else 'software')


def default_dlna_profile_id(shape=None):
    """The compatibility shape an install that never chose one gets.

    This is not one value any more, and the reason is a measurement rather than
    a taste. Same machine, same receiver, same live shape, same encoder, 20 s
    each, alternating twice (the numbers below are the recorded rows; the
    repeat spread is a few hundredths of a second):

        ps-pal     first picture 5.86 s   steady lag 5.08 s   (mpv holds ~1.2 s of cache)
        ts-mpeg2   first picture 2.47 s   steady lag 2.15 s   (mpv holds 0 s)

    -- and `ts-mpeg2` carries a byte-for-byte equivalent MPEG-2 + AC-3 payload
    (same `-c:v mpeg2video`, same CBR shape, same `-vf`, same 25 fps and 720x576;
    see `build_dlna_command`). So the codec is not the difference and neither is
    the audio: the container costs 3.4 s of the receiver's start-up and it keeps
    that debt for the whole session, because a stream that begins 3.4 s late is
    3.4 s behind forever -- the position advances at exactly 1.00x in both.
    `DLNA_PROFILE_LATENCY` is the sweep over all five profiles on this same
    receiver, and it is what the settings page shows; these two rows are the A/B
    that decided the default, and they rank the same way.

    Two mechanisms, one of them ours:

      * `-preload` (the vob muxer's "initial demux-decode delay", default
        500000 us) is stamped into the stream: `ffprobe` reports
        `start_time 0.534667` as shipped and `0.034667` with `-preload 0`, and
        the live measurement moves 5.86 -> 5.30 s. That half-second is ours to
        take back -- see why we deliberately do not below.
      * The rest is MPEG-PS being hard to *identify*: `ffprobe -show_entries
        format=probe_score` on the two files this function's own command builds
        gives **26** for PS and **100** for TS. A receiver that is not sure what
        it is holding keeps reading before it commits, which is exactly the
        1.1-1.4 s of demuxer cache the PS row shows and the TS row does not.

    `-muxrate` was the obvious third suspect -- the VOB path resolves it to the
    DVD constant 10080 kbps, which would stamp an arrival model 2.15x faster
    than our 4.692 Mbps really is -- and it is **falsified**: 10080000 written
    out loud landed on top of as-shipped (5.85/5.00 against 5.86/5.08), as did
    the true rate (5.72/4.93) and 20160 kbps. The muxer's stamps are not what a
    receiver here obeys.

    Why the default still depends on the *shape*: a renderer that needs the file
    shape is, by that setting's own description, an old television, and DVD-era
    firmware is the one audience MPEG-PS exists for -- measured here against mpv,
    on a machine with no old TV in the room. The live shape is the opposite
    audience: it is chosen because modern clients refuse a fabricated size, and
    those same clients read TS without hunting for a PS pack header. So PS keeps
    the file shape, TS + H.264 -- the fastest thing measured on this receiver,
    2.03 s first / 1.94 s steady with the hardware encoder -- takes the live one,
    and `next_profile_id` plus the watchdog's ladder mean a wrong guess
    self-corrects instead of sitting there for the whole session.

    Reading through `dlna_shape()` is only safe because that function asks with
    `Setting.has`; a `Setting.get` here would write a default into an untouched
    install and destroy the very fact this depends on. See `dlna_profile`.
    """
    if shape is None:
        shape = dlna_shape()
    return (LIVE_DEFAULT_DLNA_PROFILE if shape == DLNA_SHAPE_LIVE
            else DEFAULT_DLNA_PROFILE)


def dlna_profile(profile_id=None):
    """The compatibility shape in use for the DLNA target.

    Unknown or missing values fall back to the default for the shape being
    served rather than failing: the menu writes these, and a stale setting from
    a rolled-back plugin must not make the mirror refuse to start.

    Asking is not allowed to answer. `Setting.get(key, default)` *stores* the
    default when the key is absent (AGENTS.md 4.2), so reading the profile once
    used to pin `ps-pal` into an untouched install's settings -- which is the
    same thing as erasing the one fact a later default has to know: whether the
    user ever chose. Same reason `dlna_shape()` reads through `Setting.has`.
    """
    fallback = DLNA_PROFILES[default_dlna_profile_id()]
    if profile_id is None:
        if not Setting.has(SettingProperty.Mirror_Dlna_Profile):
            return fallback
        profile_id = str(Setting.get(SettingProperty.Mirror_Dlna_Profile, '')
                         or '')
    return DLNA_PROFILES.get(profile_id, fallback)


def dlna_profile_id(profile):
    """The stored key for a profile object (the menu and the watchdog both
    need to name the current one to the user)."""
    for key, value in DLNA_PROFILES.items():
        if value is profile:
            return key
    return default_dlna_profile_id()


def next_profile_id(profile):
    """One rung up the compatibility ladder from `profile`, wrapping around.

    The list is ordered "most likely to work on an old TV" first, so the rung
    above is always a more modern container -- which is the direction a renderer
    that refused the DVD shape is asking for. None when `profile` is not one of
    the five, so a caller cannot rotate into a shape it cannot name: this asks
    by *identity* rather than through `dlna_profile_id` on purpose, because that
    one answers an unknown object with the default, and "the default's next rung"
    is a rotation nobody was asked for.
    """
    keys = list(DLNA_PROFILES)
    for key, value in DLNA_PROFILES.items():
        if value is profile:
            return keys[(keys.index(key) + 1) % len(keys)]
    return None


#: How the DLNA target answers a renderer.
#:
#: 'live' says what this is: an endless stream with no length, no ranges and no
#: end to seek within. 'file' is the older shape, which fabricates a size below
#: 2 GiB and promises byte ranges -- the MirrorCast-era trick for a renderer
#: that will only play a finite file.
#:
#: Live is the default because it is the only shape measured to work. On the
#: same LAN and with the same encoder: `live` gives mpv on the receiving Mac
#: H.264 at 1680x1080 with the position advancing (0 -> 19.5 s, vo-configured),
#: while `file` makes mpv *and* a TCL Android 11 television read the whole
#: thing, ask for `Range: bytes=<just before the fabricated end>-` -- the
#: container-index hunt -- and start over rather than play.
DLNA_SHAPE_LIVE = 'live'
DLNA_SHAPE_FILE = 'file'
DLNA_SHAPES = {
    DLNA_SHAPE_LIVE: ('直播流',
                      '不报长度、不声明可拖动；现代电视和播放器都认'),
    DLNA_SHAPE_FILE: ('伪装成文件',
                      '只认有限文件的老电视才需要；现代设备会去读尾部索引'),
}


def dlna_shape():
    """The stored shape, or the live one when nothing was chosen.

    Read through `Setting.has` on purpose: `Setting.get(key, default)` writes
    that default into the user's settings as a side effect (AGENTS.md 4.2), so
    asking would leave a key behind on an untouched install.
    """
    if not Setting.has(SettingProperty.Mirror_Dlna_Shape):
        return DLNA_SHAPE_LIVE
    chosen = str(Setting.get(SettingProperty.Mirror_Dlna_Shape, '') or '')
    return chosen if chosen in DLNA_SHAPES else DLNA_SHAPE_LIVE


#: What each shape costs, measured on the same machine, same live shape, same
#: hardware encoder, 20 s each: seconds until the first picture, and how far
#: behind the wall the picture then stays. See `default_dlna_profile_id` for the
#: mechanism and for what this sample does **not** cover.
DLNA_PROFILE_LATENCY = {
    'ts-h264': ('2.0', '1.9'),
    'mkv-h264': ('2.4', '2.1'),
    'ts-mpeg2': ('2.5', '2.2'),
    'ps-pal': ('5.7', '5.1'),
    'ps-ntsc': ('5.7', '5.0'),
}

#: The receiver in that sentence is another Macast on this LAN, not a television.
#: Saying so is the whole point of keeping the table next to the page text: the
#: ordering (TS before PS by ~3 s) is a property of reading a container that
#: identifies itself weakly, which every receiver does, but the sizes are this
#: one receiver's numbers.
DLNA_PROFILE_LATENCY_SCOPE = ('实测：同一台 Mac、直播形状、硬件编码，'
                              '接收端是本机另一台 mpv/Macast（不是真老电视）')


def dlna_profile_note():
    """The paragraph under「兼容档位」: the five shapes, ranked by what they cost.

    The card used to say only "换档位会立刻重启镜像", which is the mechanics and
    no help in choosing. A user staring at a 5-second mirror should be able to
    see that the container is worth about 3 seconds of that -- and see it in the
    same honest units the measurement has, rather than a claim about televisions
    nobody has in the room.
    """
    parts = ['{}：首帧 {} 秒 · 落后 {} 秒'.format(
        DLNA_PROFILES[key].label.split('（')[0].strip(), first, steady)
        for key, (first, steady) in sorted(
            DLNA_PROFILE_LATENCY.items(),
            key=lambda item: float(item[1][0]))]
    why = ('PS 比 TS 慢的那 3 秒是容器自己要的：它的包头顶不住 TS 每 188 字节'
           '一次的自同步，接收端要多探一秒多才敢开始放，然后整场都欠着这笔时间；'
           '但只有 DVD 时代的老电视认 PS，所以「伪装成文件」这一条仍然默认 PS。')
    return '{}。{}。{}'.format(DLNA_PROFILE_LATENCY_SCOPE, '；'.join(parts), why)


def dlna_shape_words(session):
    """What this session's shape costs the picture, for the start-up message.

    Only the file shape pays a prefill, and it cannot avoid it: the TV is about
    to read by absolute offset, so what it sniffs has to exist already. The live
    shape used to be announced with that same number anyway, which told the
    user to expect seconds of delay that this session never added.
    """
    if session.bytelog:
        return '伪装成文件，先预填约 {} 秒'.format(
            dlna_prefill_seconds(session.profile))
    if getattr(session, 'shape_forced', False):
        # The setting says one thing and the session does another, so the
        # sentence has to carry both -- and the reason, or the next user reads
        # it as the software ignoring them.
        return '直播流，不预填（「伪装成文件」读不到这一档位的文件头，已按直播流处理）'
    return '直播流，不预填'


def profile_order(profile=None):
    """Profiles in "try this next" order, starting after `profile`.

    `profile` defaults to the stored choice, because this is the *manual*
    advice: what to point a user at on the page. The watchdog's own walk uses
    `next_profile_id` on the session's profile, which is a different thing --
    with a ladder running, the stored choice and the bytes on the wire are not
    the same container.
    """
    keys = list(DLNA_PROFILES)
    current = dlna_profile_id(profile or dlna_profile())
    return keys[keys.index(current) + 1:] + keys[:keys.index(current)]


#: Hours a television session may run before it stops itself. The user asked
#: for a ceiling, not a countdown: an unattended mirror that nobody remembered
#: to stop keeps encoding, keeps the machine awake (`_keep_awake` holds a
#: `caffeinate` assertion for the whole session) and keeps serving a stream
#: forever. Three options and a default, with **no "unlimited" row on purpose**
#: -- a knob whose safest setting is "off" is a knob nobody will ever turn on.
MAX_DURATION_HOURS = (12, 24, 48)
DEFAULT_MAX_DURATION_HOURS = 12
#: Which output shapes obey it. Only the two television targets: `cast` LOADs a
#: live stream into a device that will sit on it indefinitely, and `dlna` hands
#: a renderer a file with a fabricated length it will read to the end. A browser
#: viewer that has run too long reloads its page, and the two low-latency
#: channels (`caststream`, `webrtc`) are live-edge consumers whose receivers
#: hold no backlog worth protecting a television from -- putting `-t` on them
#: would truncate a presentation to save nothing.
MAX_DURATION_TARGETS = ('cast', 'dlna')


def max_duration_hours():
    """The stored ceiling in hours, validated the way `quality_key` is.

    An unknown value is the default rather than a refusal: the alternative is a
    settings JSON edit that ends a presentation mid-sentence, and the person
    watching the television gets no explanation of why.
    """
    try:
        hours = int(Setting.get(SettingProperty.Mirror_Max_Duration,
                                DEFAULT_MAX_DURATION_HOURS))
    except (TypeError, ValueError):
        return DEFAULT_MAX_DURATION_HOURS
    return hours if hours in MAX_DURATION_HOURS else DEFAULT_MAX_DURATION_HOURS


def bound_video_seconds():
    """The single truth about how long one television session may encode.

    Both halves of the promise read this: ffmpeg's `-t` (which decides when the
    bytes stop) and `advertised_file` (which decides what length the television
    is told). Those two were allowed to be computed separately exactly once, and
    what that produced is the failure this function exists to prevent -- a file
    shape whose advertised length is further than the encoder ever writes is the
    Android-11-TV story in `protocol_info`'s docstring: read to the end, go
    looking for an index that is not there, never PLAYING.
    """
    return max_duration_hours() * 3600


def duration_bound(kind):
    """`bound_video_seconds()` for a shape that obeys it, else None.

    None means "no `-t` on this argv", which is the honest answer for the four
    live shapes and what the「统计信息」card then leaves off rather than printing
    a blank row.
    """
    return bound_video_seconds() if kind in MAX_DURATION_TARGETS else None


def duration_args(max_seconds):
    """The argv that bounds one encode: `-t <seconds>`, or nothing.

    Both television builders call this rather than writing `-t` themselves, so
    "which shapes get a ceiling" has one answer in the file. It is an *output*
    option, so it rides after the inputs and codec args and before the muxer
    flags -- as an input option it would bound the file ffmpeg opens, and a
    live capture has no duration to bound there.
    """
    return ['-t', str(int(max_seconds))] if max_seconds else []


#: Sits on the「画质」card rather than getting one of its own, for the same
#: reason the prefill seconds sit under「投屏形状」: it only exists for some
#: targets, and a control two cards away from the thing that makes it meaningful
#: reads as a setting for the whole mirror. 「画质」is the one card *both*
#: television shapes always have.
MAX_DURATION_HINT = ('到点主动停止并弹通知，不是悄悄把画面截掉：无人看管的镜像会一直'
                     '编码、一直阻止这台机器休眠。只对 Chromecast 兼容通道与 DLNA 电视'
                     '生效（浏览器页刷新一下就好，两条低延迟通道不该被截断）。'
                     '换这里会立刻重启镜像。')


def max_duration_phrase(seconds):
    """The ceiling in words, for the notice and the option pills.

    Whole hours are the only thing `max_duration_hours` can answer with, so this
    never has to say "and 37 minutes" -- it is a formatter for three numbers, not
    a duration library.
    """
    return '{} 小时'.format(int(seconds) // 3600)


def advertised_file(bitrate, max_seconds=None):
    """(size, 'H:MM:SS') claimed for the endless stream.

    The two numbers have to agree with each other and with the bitrate, or a
    renderer that cross-checks them sees a broken file. The size is capped
    below 2 GiB because some firmware treats Content-Length as a signed 32-bit
    integer and asks for `bytes=0-18446744072566584319`.

    `bitrate` is the *total* rate (video + audio): the video-only figure made
    the advertised duration about 4% short of the bytes actually produced, and
    a TV that seeks from the end notices.

    `max_seconds` is the ceiling the encoder was told to stop at (`-t`). It may
    only ever **shorten** this pair: the size cap already bounds the file at
    roughly an hour on the shipping profiles, so a 12-hour ceiling changes
    nothing here, while a ceiling below the cap has to shrink the advertised
    length or the television is promised bytes that will never arrive.
    """
    size = min(bitrate * 3600 // 8, DLNA_MAX_ADVERTISED_SIZE)
    if max_seconds:
        size = min(size, int(bitrate) * int(max_seconds) // 8)
    seconds = max(1, size * 8 // bitrate)
    return size, '{}:{:02d}:{:02d}'.format(seconds // 3600,
                                           seconds % 3600 // 60, seconds % 60)


def protocol_info(profile):
    """The DIDL `protocolInfo` attribute for a DLNA profile.

    `DLNA.ORG_OP=00` with `DLNA.ORG_CI=1` is the honest pair for what this is: a
    live transcode with no seek of any kind. It used to say `OP=01`
    ("byte-seek supported") and `CI=0` ("not converted"), and neither is true --
    the file has no end to seek within, and every byte comes out of ffmpeg
    rather than a stored file. Claiming byte-seek is what made a real Android 11
    television take our fabricated 1.9 GB size as fact and go looking for a
    container index at the end of it (measured: a whole-file read, then
    `Range: bytes=1899887200-`, then that same pair again, never PLAYING).
    """
    parts = ['DLNA.ORG_OP=00', 'DLNA.ORG_CI=1',
             'DLNA.ORG_FLAGS={}'.format(DLNA_ORG_FLAGS)]
    if profile.org_pn:
        parts.insert(0, 'DLNA.ORG_PN={}'.format(profile.org_pn))
    return 'http-get:*:{}:{}'.format(profile.content_type, ';'.join(parts))


def content_features(profile):
    """The `contentFeatures.dlna.org` response header. Renderers that sniff it
    refuse the stream when it is missing, so it travels with every answer.

    It carries the same DLNA.ORG_PN the DIDL `protocolInfo` advertises. Sending
    only the flags was the one place the two disagreed: a renderer that checks
    the *response* for the profile name it was promised sees a stream with no
    name at all, and a TCL television measured here opened up to eight
    connections, downloaded 26 MB, and never reached PLAYING.
    """
    parts = []
    if profile.org_pn:
        parts.append('DLNA.ORG_PN={}'.format(profile.org_pn))
    # Same policy as protocol_info: no seek, and it is a conversion. See there.
    parts.append('DLNA.ORG_OP=00')
    parts.append('DLNA.ORG_CI=1')
    parts.append('DLNA.ORG_FLAGS={}'.format(DLNA_ORG_FLAGS))
    return ';'.join(parts)


def build_didl(url, title, profile, size=None, duration=None):
    """DIDL-Lite for SetAVTransportURI.

    Both interpolations are escaped: `title` is this machine's hostname and
    `url` is ours while mirroring, but the same builder is used when a phone
    pushes a third-party URL through us -- an unescaped & or < there is a
    malformed envelope the TV blames on us.

    `size` and `duration` belong to the file shape alone. A live stream has
    neither, and claiming them is exactly what sent a real television looking
    for a container index at the end of a file that has no end.
    """
    from xml.sax.saxutils import escape
    claimed = '' if size is None else ' size="{}" duration="{}"'.format(
        size, escape(duration or ''))
    return (
        '<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/" '
        'xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">'
        '<item id="0" parentID="-1" restricted="1">'
        '<dc:title>{title}</dc:title>'
        '<upnp:class>object.item.videoItem</upnp:class>'
        '<res protocolInfo="{pn}"{claimed}>'
        '{url}</res></item></DIDL-Lite>'
    ).format(title=escape(title), pn=escape(protocol_info(profile)),
             claimed=claimed, url=escape(url))


def probe_capture(ffmpeg, platform=None, cursor=None):
    """Figure out the capture pipeline for this machine; None if impossible.

    `platform` and `cursor` are test seams; production always means
    sys.platform and the stored cursor preference.
    """
    platform = platform or sys.platform
    if cursor is None:
        cursor = cursor_enabled()
    key = (ffmpeg, platform, cursor)
    if key in _capture_cache:
        return _capture_cache[key]
    if platform == 'win32':
        capture = _probe_windows(ffmpeg, cursor=cursor)
    elif platform == 'darwin':
        capture = _probe_darwin(ffmpeg, cursor=cursor)
    else:
        capture = _probe_linux(ffmpeg, cursor=cursor)
    if capture is not None:
        _capture_cache[key] = capture
    return capture


#: What `ffmpeg -f avfoundation -list_devices true -i ""` really prints, on the
#: ffmpeg this plugin runs against (verified against 7.x on macOS 26):
#:
#:   [AVFoundation indev @ 0x775701c140] AVFoundation video devices:
#:   [AVFoundation indev @ 0x775701c140] [0] OBS Virtual Camera
#:   [AVFoundation indev @ 0x775701c140] AVFoundation audio devices:
#:   [AVFoundation indev @ 0x775701c140] [0] MacBook Pro麦克风
#:
#: Lower-case block names, and no quotes anywhere. Both details were wrong in
#: the first parser -- it split on 'Video devices:' and then kept only what
#: appeared between double quotes, so on a real Mac it returned two empty lists
#: and the whole darwin probe gave up. An older ffmpeg spelled the headers
#: `List of Video devices:` with `0) name` lines, which is still accepted.
_AVFOUNDATION_BLOCK = re.compile(r'(?:list of\s+)?(video|audio)\s+devices',
                                 re.I)
_AVFOUNDATION_DEVICE = re.compile(r'(?:\[\s*(\d+)\s*\]|(\d+)\s*\))\s*"?(.*?)"?\s*$')


def _avfoundation_lists(ffmpeg):
    """(video device names, audio device names) from -list_devices.

    The avfoundation input index is the position inside the respective list:
    ffmpeg numbers each block from zero with no holes, so the printed number and
    the position are the same thing -- the printed one is what is kept, so a
    future gap would show up as a wrong device rather than a wrong assumption.
    """
    try:
        proc = subprocess.run([ffmpeg, '-hide_banner', '-loglevel', 'info',
                               '-f', 'avfoundation', '-list_devices', 'true',
                               '-i', ''],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              timeout=10)
        text = proc.stdout.decode('utf-8', 'replace')
    except Exception as e:
        logger.error("cannot list avfoundation devices: %s", e)
        return [], []
    return _parse_avfoundation_lists(text)


def _parse_avfoundation_lists(text):
    """The two device lists, read out of ffmpeg's own log lines."""
    blocks = {'video': {}, 'audio': {}}
    current = None
    for line in str(text or '').splitlines():
        header = _AVFOUNDATION_BLOCK.search(line)
        if header:
            current = header.group(1).lower()
            continue
        if current is None:
            continue
        match = _AVFOUNDATION_DEVICE.search(line.strip())
        if not match:
            continue
        index = int(match.group(1) if match.group(1) is not None
                    else match.group(2))
        name = match.group(3).strip()
        if name:
            blocks[current][index] = name
    return ([blocks['video'][i] for i in sorted(blocks['video'])],
            [blocks['audio'][i] for i in sorted(blocks['audio'])])


def _probe_avfoundation(ffmpeg, cursor=True):
    videos, audios = _avfoundation_lists(ffmpeg)
    screens = [(index, name) for index, name in enumerate(videos)
               if 'capture screen' in name.lower()]
    if not screens and videos:
        screens = [(0, videos[0])]
    if not screens:
        return None
    wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
    screen = screens[0][0]
    if wanted.isdigit():
        for index, _name in screens:
            if str(index) == wanted:
                screen = index
                break
        else:
            logger.warning("screen %s is gone, falling back to %s",
                           wanted, screen)
    # avfoundation itself exposes no system-audio sink (the ScreenCaptureKit
    # demuxer FFmpeg proposed was never released); BlackHole is the open
    # source way to make one appear on *this* path. Since 0.21 the preferred
    # darwin path is `_probe_darwin`, which taps SCK in-process and carries
    # audio natively -- this probe is the fallback when SCK is unavailable
    # (macOS < 13, missing pyobjc bindings) or has been refused at runtime.
    # Absent an SCK tap, an absent BlackHole means we mirror video only.
    blackhole = None
    for index, name in enumerate(audios):
        if 'blackhole' in name.lower():
            blackhole = index
            break
    base = ['-f', 'avfoundation', '-framerate', str(FPS),
            '-pixel_format', 'uyvy422',
            '-capture_cursor', '1' if cursor else '0']
    if blackhole is None:
        return _Capture('屏幕 (avfoundation)',
                        [base + ['-i', '{}:none'.format(screen)]],
                        screens=screens)
    return _Capture('屏幕 + 系统声音 (BlackHole)',
                    [base + ['-i', '{}:{}'.format(screen, blackhole)]],
                    audio_map='0:a:0', screens=screens)


def video_only_capture(capture):
    """The same screen grab with the system-audio tap removed.

    A tap the process is not allowed to read -- no microphone grant, or a
    BlackHole nothing is clocking -- does not fail loudly: avfoundation opens
    the session, delivers nothing at all, and the *video* starves with it. The
    one visible symptom is a capture that never returns a frame, so the retry
    has to be able to ask for video alone. Returns None when there is nothing
    to give up. Cached probes are never mutated.
    """
    if capture is None or capture.audio_map is None:
        return None
    try:
        audio_input = int(capture.audio_map.split(':')[0])
    except (IndexError, ValueError):
        return None
    inputs = [list(one) for one in capture.inputs]
    if audio_input == 0 and inputs:
        # avfoundation carries screen and sound in a single -i ('1:3'): the
        # audio half goes back to `none`. Only that exact shape -- x11grab's -i
        # is a display (`:0+0,0`), and rewriting *that* would break the video
        # instead of the sound.
        for pos, arg in enumerate(inputs[0]):
            if arg == '-i' and pos + 1 < len(inputs[0]) \
                    and re.match(r'^\d+:\d+$', inputs[0][pos + 1]):
                inputs[0][pos + 1] = inputs[0][pos + 1].split(':')[0] + ':none'
    else:
        # One input per device (PulseAudio's monitor sink): the audio half of
        # an SCK pair is its own `-i pipe:` entry and drops the same way.
        del inputs[audio_input:audio_input + 1]
    return _Capture('屏幕 (无系统声音)', inputs, screens=capture.screens,
                    method=capture.method, spec=capture.spec)


# ---------------------------------------------------------------------------
# ScreenCaptureKit (macOS 13+): darwin capture without avfoundation
# ---------------------------------------------------------------------------
#
# avfoundation can only see what a capture *device* exposes, and on macOS no
# device exposes the system's own audio -- which is why the BlackHole
# aggregate rig exists. ScreenCaptureKit taps the screen and system audio
# directly, from inside this process, and starts sooner: measured 2026-10-02
# on this machine (ffmpeg spawn -> first encoded frame, three runs each),
# SCK at 624/600/575 ms against avfoundation's 1954/1048/1031 ms -- roughly
# 450 ms off every mirror session, plus a setup dialog that no longer
# exists. The price is two pyobjc packages (ScreenCaptureKit, CoreMedia)
# which are used lazily, never at module import (Part 30).
#
# The division of labour: ffmpeg still does the encoding, exactly as for
# every other capture. What changes is that ffmpeg's first input is
# `rawvideo nv12` reading *stdin*, which this plugin's feeder thread fills
# from SCK callbacks, and its optional second input is `f32le` PCM from a
# pipe the feeder also owns. The token below stands in for the fd at
# command-build time and is resolved at spawn time, because the command is
# also composed by probes and tests that have no pipe at all.
#
# Four pipe facts were measured before this design was fixed (2026-10-02,
# local ffmpeg, this machine); each one is load-bearing:
#
#   * `-use_wallclock_as_timestamps 1` on the rawvideo stdin is what makes
#     the frames' timestamps real arrival times, and `-r FPS` then stamps a
#     CFR timeline from them (2 duplicate frames in a 6 s probe).
#   * An f32le pipe that is open but silent **freezes the whole command at
#     open time** -- ffmpeg consumes ~1.2 MB of video and then waits. The
#     silent desktop is not that case: SCK delivers full-size zero PCM at
#     its 20 ms cadence (measured), so the pipe is never silent in practice.
#   * Zero bytes of audio followed by EOF does *not* freeze anything: a
#     noise-video run reading mpegts back from a pipe produced 179,540
#     bytes against 179,352 for the same run with no audio input at all.
#     That is why giving up on audio is `close the write end`, not a
#     capture restart.
#   * CMSampleBuffers are recycled the moment the callback returns, so
#     every byte that outlives the callback is copied inside it.
#
# Fallback shape: `_probe_darwin` tries SCK first and falls back to
# `_probe_avfoundation` -- unchanged -- whenever SCK is unavailable (macOS
# too old, bindings missing), unproven (no screen-recording grant yet; the
# avfoundation path owns that permission flow), or refused at runtime (the
# feeder's first frame never arrives; `_sck_refused` then keeps every later
# session of this run on avfoundation, so one refusal costs one attempt).

#: Stands in for the read end of the audio pipe in a built command. The real
#: fd is not known when the command is composed -- and most callers (probes,
#: fixtures, the DLNA builder's own tests) have no pipe at all, which is why
#: `_resolve_audio_fd` refuses loudly rather than letting the token reach
#: ffmpeg as a filename.
_AUDIO_FD_TOKEN = '@AUDIO_FD@'


def _resolve_audio_fd(cmd, audio_fd):
    """Replace the audio-pipe placeholder in `cmd` with the real read fd.

    A command that carries no token -- every non-SCK command -- passes
    through untouched. A token without a pipe is a programming error and
    must never spawn: ffmpeg would read a file literally named
    `@AUDIO_FD@`, fail, and look like a capture problem.
    """
    if not any(_AUDIO_FD_TOKEN in one for one in cmd):
        return cmd
    if audio_fd is None:
        raise ValueError(
            'ffmpeg was handed the ScreenCaptureKit audio pipe token but '
            'no pipe: this attempt must never spawn')
    return [one.replace(_AUDIO_FD_TOKEN, str(audio_fd)) for one in cmd]


#: ffmpeg binary -> True for the rest of the run once an SCK attempt died
#: without producing a frame. Same latch shape as `_ddagrab_refused`: not
#: cleared by invalidate_capture_cache(), because a cursor or screen change
#: is not new evidence that SCK works now.
_sck_refused = set()

#: SCK exists from macOS 13; below that the API this file drives does not.
SCK_MIN_MACOS = (13, 0)
#: How long the feeder waits for the first SCK video frame before calling the
#: attempt dead. Measured first frame is well under 300 ms; a denied or
#: revoked screen-recording grant produces nothing at all, and the mirror
#: must learn that in seconds, not at the no-frame watchdog's 3 s (which is
#: needed for the ffmpeg side and stays).
SCK_FIRST_FRAME_SECONDS = 2.0
#: After video is flowing, how long an audio tap gets to deliver anything.
#: The silent desktop still delivers zero-PCM every 20 ms, so silence this
#: long means the audio output never attached or the tap is broken.
SCK_AUDIO_GRACE_SECONDS = 2.0
#: Frames dropped (`Queue(maxsize=...)`) rather than blocked on: a stalled
#: writer must not stall ScreenCaptureKit's callback queue. 2 frames bounds
#: worst-case queue latency to ~83 ms at 24 fps; older frames are worth less
#: than current ones on a live stream.
SCK_VIDEO_QUEUE = 2
#: ~6 seconds of 20 ms audio buffers; dropping oldest keeps the pipe near
#: live when the encoder hiccups.
SCK_AUDIO_QUEUE = 300
#: One 20 ms block of silence in the shape the audio pipe carries (f32le,
#: 48 kHz, stereo: 48 000/50 samples x 2 ch x 4 bytes). Queued into the pipe
#: before the capture starts -- see `_SckFeeder.start` -- because ffmpeg
#: blocks the whole command at open on a *silent but open* audio pipe
#: (measured 2026-10-02), and on a run where SCK's first real buffer was
#: slow that freeze consumed the mirror's no-frame budget and killed an
#: otherwise healthy session. Twenty milliseconds of zeros are inaudible
#: and the pipe is a byte stream, so the first real buffer follows seamlessly.
SCK_AUDIO_SILENCE = bytes((48000 // 50) * 2 * 4)
#: The failure text the retry path keys on; a constant so the log line, the
#: page and the suite cannot drift apart.
SCK_REASON_NO_VIDEO = 'sck: no frames from ScreenCaptureKit'
#: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange -- what SCK's default
#: configuration delivers, and the one format the copy path understands.
SCK_PIXEL_FORMAT_420V = 0x34323076


def _sck_version_ok():
    """True when this macOS is new enough for the SCK API used here."""
    release = platform.mac_ver()[0] or ''
    try:
        parts = tuple(int(one) for one in release.split('.')[:2])
    except ValueError:
        return False
    return len(parts) == 2 and parts >= SCK_MIN_MACOS


#: One-shot caches. Nothing here runs at import time -- on Windows/Linux the
#: framework paths below do not exist and must never be touched.
_COREGRAPHICS = {}
_CG_PREFLIGHT = {}
_SCK_MODULES = {}
_CV_API = {}
_SCK_HANDLER_CLASSES = {}


def _coregraphics():
    lib = _COREGRAPHICS.get('lib')
    if lib is None:
        lib = ctypes.CDLL(
            '/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics')
        lib.CGPreflightScreenCaptureAccess.restype = ctypes.c_bool
        lib.CGPreflightScreenCaptureAccess.argtypes = []
        _COREGRAPHICS['lib'] = lib
    return lib


def _preflight_screen_capture():
    """True when this process already holds Screen Recording permission.

    ScreenCaptureKit would *prompt* on first use, but a prompt cannot be
    answered from a background thread -- and a probe that blocks on a dialog
    the user never sees is worse than not probing. The preflight call asks
    without prompting; a machine that has not granted the permission falls
    through to avfoundation, whose own prompt flow is the one users have
    always met.
    """
    if 'ok' not in _CG_PREFLIGHT:
        answer = False
        try:
            answer = bool(_coregraphics().CGPreflightScreenCaptureAccess())
        except Exception as e:              # pragma: no cover - darwin only
            logger.debug('CGPreflightScreenCaptureAccess unavailable: %s', e)
        _CG_PREFLIGHT['ok'] = answer
    return _CG_PREFLIGHT['ok']


def _sck_modules():
    """(objc, Foundation, CoreMedia, ScreenCaptureKit), or None.

    Imported lazily and cached: the plugin must load on Windows and Linux,
    where these imports either fail or succeed against the wrong framework
    set. A failed attempt is cached too, so the probe does not pay for the
    import machinery on every run.
    """
    if 'mods' not in _SCK_MODULES:
        try:
            import objc
            import Foundation
            import CoreMedia
            import ScreenCaptureKit
            _SCK_MODULES['mods'] = (objc, Foundation, CoreMedia,
                                    ScreenCaptureKit)
        except Exception as e:
            logger.debug('ScreenCaptureKit bindings unavailable: %s', e)
            _SCK_MODULES['mods'] = False
    return _SCK_MODULES['mods'] or None


def _cv_api():
    """(CoreVideo, CoreMedia) CDLLs with every signature this file uses.

    ctypes without argtypes truncates 64-bit pointers, and these entry
    points pass nothing but pointers. Loaded lazily; the feeder thread is
    the only caller, and it exists only on darwin.
    """
    api = _CV_API.get('api')
    if api is not None:
        return api
    cv = ctypes.CDLL('/System/Library/Frameworks/CoreVideo.framework/'
                     'CoreVideo')
    cm = ctypes.CDLL('/System/Library/Frameworks/CoreMedia.framework/'
                     'CoreMedia')
    vp = ctypes.c_void_p
    cv.CVPixelBufferGetWidth.argtypes = [vp]
    cv.CVPixelBufferGetWidth.restype = ctypes.c_size_t
    cv.CVPixelBufferGetHeight.argtypes = [vp]
    cv.CVPixelBufferGetHeight.restype = ctypes.c_size_t
    cv.CVPixelBufferGetPixelFormatType.argtypes = [vp]
    cv.CVPixelBufferGetPixelFormatType.restype = ctypes.c_uint32
    cv.CVPixelBufferGetPlaneCount.argtypes = [vp]
    cv.CVPixelBufferGetPlaneCount.restype = ctypes.c_size_t
    cv.CVPixelBufferGetBaseAddressOfPlane.argtypes = [vp, ctypes.c_size_t]
    cv.CVPixelBufferGetBaseAddressOfPlane.restype = vp
    cv.CVPixelBufferGetBytesPerRowOfPlane.argtypes = [vp, ctypes.c_size_t]
    cv.CVPixelBufferGetBytesPerRowOfPlane.restype = ctypes.c_size_t
    cv.CVPixelBufferLockBaseAddress.argtypes = [vp, ctypes.c_ulong]
    cv.CVPixelBufferLockBaseAddress.restype = ctypes.c_int32
    cv.CVPixelBufferUnlockBaseAddress.argtypes = [vp, ctypes.c_ulong]
    cv.CVPixelBufferUnlockBaseAddress.restype = ctypes.c_int32
    cm.CMSampleBufferGetImageBuffer.argtypes = [vp]
    cm.CMSampleBufferGetImageBuffer.restype = vp
    cm.CMSampleBufferGetDataBuffer.argtypes = [vp]
    cm.CMSampleBufferGetDataBuffer.restype = vp
    cm.CMBlockBufferGetDataLength.argtypes = [vp]
    cm.CMBlockBufferGetDataLength.restype = ctypes.c_size_t
    cm.CMBlockBufferCopyDataBytes.argtypes = [vp, ctypes.c_size_t,
                                              ctypes.c_size_t, vp]
    cm.CMBlockBufferCopyDataBytes.restype = ctypes.c_int32
    api = (cv, cm)
    _CV_API['api'] = api
    return api


def _objc_ptr(obj):
    """The raw pointer behind a pyobjc-wrapped object."""
    return _sck_modules()[0].pyobjc_id(obj)


def _sck_pixel_size(sample):
    """(width, height) of the pixel buffer inside a video sample, or None."""
    cv, cm = _cv_api()
    image = cm.CMSampleBufferGetImageBuffer(_objc_ptr(sample))
    if not image:
        return None
    return (int(cv.CVPixelBufferGetWidth(image)),
            int(cv.CVPixelBufferGetHeight(image)))


def _nv12_from_planes(planes, width, height):
    """Strip row padding from NV12 planes into one tightly packed frame.

    `planes` is [(data, bytes_per_row, rows)] in NV12 order -- Y, then the
    interleaved UV plane -- exactly the layout `CVPixelBufferGetPlaneCount`
    promises. Rows wider than the frame are cut down; anything that cannot
    add up to exactly width*height*3//2 bytes returns None. A short frame
    would be silent corruption downstream (green smear) that the encoder
    cannot notice, so it is refused here instead. Pure function: the copy
    path feeds it real plane bytes, the suite feeds it synthetic ones.
    """
    if width <= 0 or height <= 0:
        return None
    luma = width * height
    out = bytearray(luma + luma // 2)
    pos = 0
    for data, row_bytes, rows in planes:
        if row_bytes < width or len(data) < row_bytes * rows:
            return None
        for row in range(rows):
            start = row * row_bytes
            out[pos:pos + width] = data[start:start + width]
            pos += width
    if pos != len(out):
        return None
    return bytes(out)


def _sck_nv12_bytes(sample, width, height):
    """Copy an NV12 pixel buffer out as tightly packed bytes, or None.

    Synchronous by contract: SCStream recycles sample buffers the moment
    the handler returns. Padding is stripped by `_nv12_from_planes`, so the
    byte stream matches the `-s WxH` rawvideo input exactly.
    """
    cv, cm = _cv_api()
    image = cm.CMSampleBufferGetImageBuffer(_objc_ptr(sample))
    if not image:
        return None
    if int(cv.CVPixelBufferGetPixelFormatType(image)) != SCK_PIXEL_FORMAT_420V:
        return None
    if cv.CVPixelBufferLockBaseAddress(image, 1) != 0:  # kCVPixelBufferLock_ReadOnly
        return None
    try:
        if int(cv.CVPixelBufferGetPlaneCount(image)) < 2:
            return None
        y_row = int(cv.CVPixelBufferGetBytesPerRowOfPlane(image, 0))
        c_row = int(cv.CVPixelBufferGetBytesPerRowOfPlane(image, 1))
        y_ptr = cv.CVPixelBufferGetBaseAddressOfPlane(image, 0)
        c_ptr = cv.CVPixelBufferGetBaseAddressOfPlane(image, 1)
        if not y_ptr or not c_ptr:
            return None
        planes = [(ctypes.string_at(y_ptr, y_row * height), y_row, height),
                  (ctypes.string_at(c_ptr, c_row * (height // 2)), c_row,
                   height // 2)]
    finally:
        cv.CVPixelBufferUnlockBaseAddress(image, 1)
    return _nv12_from_planes(planes, width, height)


def _sck_audio_bytes(sample):
    """The float32 PCM inside an audio sample, or b''.

    The size cannot come from `CMSampleBufferGetTotalSampleSize` (it answers
    0 for audio buffers -- it is defined for image data), so it comes from
    the sample's block buffer. `CMBlockBufferCopyDataBytes` is used rather
    than `GetDataPointer` because the buffer is not promised to be
    contiguous.
    """
    _, cm = _cv_api()
    block = cm.CMSampleBufferGetDataBuffer(_objc_ptr(sample))
    if not block:
        return b''
    length = int(cm.CMBlockBufferGetDataLength(block))
    if length <= 0:
        return b''
    out = ctypes.create_string_buffer(length)
    if cm.CMBlockBufferCopyDataBytes(block, 0, length, out) != 0:
        return b''
    return out.raw


def _shareable_content(Foundation, SCK, timeout=8.0):
    """SCShareableContent, asked synchronously. None on error or timeout.

    Completion-handler API run on an SCK-owned queue; a semaphore turns it
    back into a blocking call. The probe runs on a background thread
    (probes never run on the UI thread) and the feeder reuses this. The
    wait is bounded: a machine that never calls back returns None instead
    of hanging a session forever.
    """
    box = {}
    done = threading.Event()

    def on_content(content, error):
        box['content'] = content
        box['error'] = error
        done.set()

    SCK.SCShareableContent.getShareableContentWithCompletionHandler_(
        on_content)
    if not done.wait(timeout):
        logger.warning('SCShareableContent did not answer within %.1f s',
                       timeout)
        return None
    if box.get('error') is not None or box.get('content') is None:
        logger.debug('SCShareableContent answered an error: %s',
                     box.get('error'))
        return None
    return box['content']


def _sck_filter(SCK, display):
    """An SCContentFilter for one display, excluding nothing.

    Both constructors exist across the macOS 13-27 range; the four-argument
    one is preferred (it is the one that can later exclude overlay windows)
    and the deprecation-era two-argument one is the fallback.
    """
    try:
        return SCK.SCContentFilter.alloc(
        ).initWithDisplay_excludingApplications_exceptingWindows_(
            display, [], [])
    except Exception:
        return SCK.SCContentFilter.alloc().initWithDisplay_excludingWindows_(
            display, [])


def _display_scale(display, Foundation, SCK):
    """Pixels per point for this display (Retina panels report 2.0).

    SCDisplay sizes are in points; the rawvideo input is sized in pixels.
    Getting this wrong makes ffmpeg reject every frame as a size mismatch,
    so this asks NSScreen first (display id lives in its device
    description), then the content filter's own pointPixelScale, then
    falls back to 1.0.
    """
    display_id = int(display.displayID())
    try:
        for screen in Foundation.NSScreen.screens():
            device = screen.deviceDescription() or {}
            if int(device.get('NSScreenNumber', 0)) == display_id:
                return float(screen.backingScaleFactor()) or 1.0
    except Exception as e:
        logger.debug('NSScreen scale lookup failed: %s', e)
    try:
        return float(_sck_filter(SCK, display).pointPixelScale()) or 1.0
    except Exception as e:
        logger.debug('pointPixelScale lookup failed: %s', e)
    return 1.0


def _sck_handler_class(Foundation):
    """One NSObject subclass carrying both SCK callback protocols.

    The ObjC runtime names classes globally, so a class is created once per
    Foundation module and reused (tests load the plugin under several names,
    each with its own fake Foundation -- keying on the module object keeps
    those apart). One instance serves as both the stream delegate
    (`stream_didStopWithError_`) and the sample handler for both outputs
    (`stream_didOutputSampleBuffer_ofType_`); the feeder is hung off it as
    a plain Python attribute.
    """
    cls = _SCK_HANDLER_CLASSES.get(id(Foundation))
    if cls is None:
        class _SckHandler(Foundation.NSObject):

            def stream_didOutputSampleBuffer_ofType_(self, stream, sample,
                                                     otype):
                feeder = self._feeder
                if feeder is not None:
                    feeder.on_sample(sample, int(otype))

            def stream_didStopWithError_(self, stream, error):
                feeder = self._feeder
                if feeder is not None:
                    feeder.on_stream_stopped(error)

        # The class statement binds `_SckHandler`, not `cls`: caching and
        # returning `cls` right after the block shipped None (it was still
        # the `.get()`'s None), so every call raised "'NoneType' object is
        # not callable" at the `handler = ...()` line and the feeder died
        # three seconds later looking exactly like "SCK gave no frame".
        # The first end-to-end run caught it.
        cls = _SckHandler
        _SCK_HANDLER_CLASSES[id(Foundation)] = cls
    return cls


def _probe_screencapturekit(ffmpeg, cursor=True):
    """An SCK capture spec, or None when this machine cannot run one.

    Returns a _Capture whose `method` is 'sck': first input reads rawvideo
    NV12 from stdin (the feeder writes frames there), optional second input
    reads float32 PCM from the tokenized pipe (`_resolve_audio_fd` swaps in
    the real fd at spawn time; only this probe ever emits the token).
    """
    if not _sck_version_ok():
        return None
    modules = _sck_modules()
    if not modules:
        return None
    if not _preflight_screen_capture():
        # No grant yet. Stay silent so the avfoundation probe's permission
        # flow -- the one users have always met, with its own dialog and
        # its own doors -- stays byte-for-byte what it was.
        logger.debug('ScreenCaptureKit preflight: no screen-recording grant')
        return None
    _objc, Foundation, _coremedia, SCK = modules
    content = _shareable_content(Foundation, SCK)
    if content is None:
        return None
    try:
        displays = list(content.displays())
    except Exception as e:
        logger.debug('cannot list displays: %s', e)
        return None
    if not displays:
        return None
    wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
    display = None
    if wanted.isdigit():
        for one in displays:
            if str(int(one.displayID())) == wanted:
                display = one
                break
        if display is None:
            logger.warning('screen %s is gone, falling back to the main '
                           'display', wanted)
    if display is None:
        display = displays[0]
    screens = []
    for position, one in enumerate(displays, 1):
        scale = _display_scale(one, Foundation, SCK)
        screens.append((int(one.displayID()),
                        '屏幕 {}（{}×{}）'.format(
                            position,
                            int(int(one.width()) * scale) & ~1,
                            int(int(one.height()) * scale) & ~1)))
    scale = _display_scale(display, Foundation, SCK)
    width = int(int(display.width()) * scale) & ~1
    height = int(int(display.height()) * scale) & ~1
    if width <= 0 or height <= 0:
        return None
    inputs = [['-f', 'rawvideo', '-pix_fmt', 'nv12', '-s',
               '{}x{}'.format(width, height), '-framerate', str(FPS),
               '-use_wallclock_as_timestamps', '1', '-i', '-'],
              ['-f', 'f32le', '-ar', '48000', '-ac', '2', '-i',
               'pipe:' + _AUDIO_FD_TOKEN]]
    return _Capture('屏幕 + 系统声音 (ScreenCaptureKit)',
                    inputs,
                    audio_map='1:a:0', screens=screens, method='sck',
                    spec={'display_id': int(display.displayID()),
                          'size': (width, height),
                          'cursor': bool(cursor)})


def _probe_darwin(ffmpeg, cursor=True):
    """Darwin capture: ScreenCaptureKit when possible, avfoundation else.

    SCK carries system audio natively (no BlackHole, no aggregate device)
    and starts ~450 ms sooner; avfoundation is the unchanged fallback for
    macOS < 13, missing pyobjc bindings, no screen-recording grant yet, or
    an SCK attempt that was refused earlier in this run.
    """
    if ffmpeg not in _sck_refused:
        capture = _probe_screencapturekit(ffmpeg, cursor=cursor)
        if capture is not None:
            return capture
    return _probe_avfoundation(ffmpeg, cursor=cursor)


class _SckFeeder(object):
    """Pumps ScreenCaptureKit frames (and audio) into ffmpeg's stdin(s).

    One capture thread drives SCK and two writer threads drain the queues
    into the pipes. Every failure path sets the same one-way stop flag and
    records the first reason; the mirror thread reads `failed()` to decide
    whether the no-frame it watched was SCK's refusal (retry once on
    avfoundation, then latch). Dropping is always oldest-first and never
    blocking: a stalled encoder must not stall ScreenCaptureKit, and on a
    live stream an old frame is worth less than a current one.
    """

    def __init__(self, audio_w=None, on_audio_absent=None):
        self._audio_w = audio_w
        self._on_audio_absent = on_audio_absent
        self._video_w = None
        self._stop = threading.Event()
        self._failure = None
        self._audio_absent_flag = False
        self._audio_seen = threading.Event()
        self._audio_pending = threading.Event()
        self._first_video = threading.Event()
        self._audio_lock = threading.Lock()
        self._lock = threading.Lock()
        self._threads = []
        self._video_q = Queue(maxsize=SCK_VIDEO_QUEUE)
        self._audio_q = Queue(maxsize=SCK_AUDIO_QUEUE)
        self._sinks = []
        self._stream = None
        self._spec = None
        self._expected_size = None
        self._screen_type = None
        self._audio_type = None

    # -- status ------------------------------------------------------------

    def failed(self):
        """The first failure reason, or None while healthy."""
        with self._lock:
            return self._failure

    def audio_absent(self):
        """True once video runs but the audio tap never delivered."""
        return self._audio_absent_flag

    def audio_pending(self):
        """True while a fed audio pipe has neither delivered nor been given up.

        FFmpeg blocks at open on a fed-but-silent audio pipe (measured
        2026-10-02), so while this lasts the encoder legitimately cannot have
        produced a frame yet and the mirror's no-frame budget must not count
        this window against the capture. The feeder bounds it: the first
        delivered buffer, the grace's `_abandon_audio`, or a failure reason
        that ends the capture outright.
        """
        return self._audio_pending.is_set()

    # -- lifecycle ---------------------------------------------------------

    def attach(self, video_w):
        """Hand over the write end of ffmpeg's stdin, exactly once."""
        self._video_w = video_w

    def start(self, spec, feed_audio=True):
        """Spawn the capture and writer threads; call once per session."""
        self._spec = spec or {}
        self._expected_size = tuple(self._spec.get('size') or ()) or None
        if feed_audio and self._audio_w is not None:
            # The prime: 20 ms of silence queued before any thread runs, so
            # ffmpeg never opens onto a silent but open audio pipe -- that
            # freeze (measured 2026-10-02) used to eat the no-frame budget
            # on runs where SCK's first audio buffer was slow, and the
            # session died as "SCK gave no frame" though nothing was wrong.
            # Deliberately not `_audio_seen`: this silence is ours, and the
            # "delivered no audio" verdict must keep meaning "SCK delivered
            # nothing". Cleared by `_on_audio` (first real buffer) or
            # `_abandon_audio`; until either, `audio_pending()` reports the
            # window during which the encoder cannot yet have output.
            self._audio_q.put_nowait(SCK_AUDIO_SILENCE)
            self._audio_pending.set()
        # (target, name, args) built here, not compared in the loop: bound
        # methods are created fresh on every attribute access, so
        # `target is self._run` was always False -- `_run` never got its
        # `feed_audio` and died with a TypeError the instant the thread
        # started, which looked exactly like "ScreenCaptureKit gave no
        # frame" three seconds later. The first end-to-end run caught it.
        workers = [(self._run, 'SCREEN_MIRROR_SCK', (bool(feed_audio),)),
                   (self._write_video, 'SCREEN_MIRROR_SCK_W', ())]
        if self._audio_w is not None:
            workers.append((self._write_audio, 'SCREEN_MIRROR_SCK_A', ()))
        for target, name, args in workers:
            thread = threading.Thread(target=target, args=args,
                                      name=name, daemon=True)
            thread.start()
            self._threads.append(thread)

    def request_stop(self):
        """Ask the capture to end; safe before start and repeatedly."""
        self._stop.set()
        self._stop_capture()

    def finish(self, timeout=2.0):
        """Join the workers and close the write ends; idempotent."""
        for thread in list(self._threads):
            thread.join(timeout)
        self._close_video()
        self._close_audio()

    # -- closing -----------------------------------------------------------

    def _close_video(self):
        writer = self._video_w
        self._video_w = None
        if writer is not None:
            try:
                writer.close()
            except (OSError, ValueError):
                pass

    def _close_audio(self):
        with self._audio_lock:
            fd = self._audio_w
            self._audio_w = None
        if fd is not None:
            try:
                os.close(fd)
            except OSError:
                pass

    def _stop_capture(self):
        """Best-effort SCStream shutdown; also runs from callback threads."""
        stream = self._stream
        if stream is None:
            return
        try:
            stream.stopCaptureWithCompletionHandler_(None)
        except Exception as e:
            logger.debug('stopCapture failed: %s', e)

    def _note_failure(self, reason):
        """Record the first failure and end the capture.

        A failure arriving while stopping (the user asked for it, or an
        earlier failure is already recorded) is not one: SCStream reports
        an abort error for a stream it was told to stop, and counting that
        would turn every clean teardown into a retry.
        """
        if self._stop.is_set():
            return
        with self._lock:
            if self._failure is None:
                self._failure = reason
                logger.warning('ScreenCaptureKit capture failed: %s', reason)
        self._stop.set()
        self._stop_capture()
        # Close the video write end so ffmpeg cannot outlive the capture. It
        # would otherwise stay blocked on a stdin nobody writes again, the
        # pump would never see stdout close, and the mirror would freeze with
        # the page still saying it runs. The writer thread polls the handle
        # each iteration, so it exits on the next one; a concurrent close
        # under an in-flight `write` surfaces as a `ValueError` there and is
        # swallowed by its own handler (the stop flag is already set).
        self._close_video()

    def _abandon_audio(self):
        """Stop feeding audio for this session (one-way).

        Closing the write end is what tells ffmpeg to end the audio stream:
        a silent-but-open audio input freezes the whole command at open
        time, while a zero-byte EOF'd one is byte-for-byte as harmless as
        no audio input at all (both measured 2026-10-02).
        """
        if self._audio_absent_flag:
            return
        self._audio_absent_flag = True
        self._audio_pending.clear()
        logger.warning('ScreenCaptureKit delivered no audio; continuing '
                       'video-only')
        self._close_audio()
        callback = self._on_audio_absent
        if callback is not None:
            try:
                callback()
            except Exception as e:
                logger.debug('audio-absent callback failed: %s', e)

    # -- SCK callbacks (SCK-owned queues) ----------------------------------

    def on_sample(self, sample, otype):
        if self._stop.is_set():
            return
        if otype == self._screen_type:
            self._on_video(sample)
        elif otype == self._audio_type and self._audio_w is not None:
            self._on_audio(sample)

    def on_stream_stopped(self, error):
        self._note_failure('sck: capture stopped ({})'.format(error))

    def _on_video(self, sample):
        try:
            size = _sck_pixel_size(sample)
        except Exception as e:
            self._note_failure('sck: cannot read a frame (%s)' % e)
            return
        if size is None:
            self._note_failure('sck: sample has no pixel buffer')
            return
        if self._expected_size is not None and size != self._expected_size:
            self._note_failure(
                'sck: display geometry changed ({}x{}, expected {}x{})'
                .format(size[0], size[1], self._expected_size[0],
                        self._expected_size[1]))
            return
        try:
            frame = _sck_nv12_bytes(sample, size[0], size[1])
        except Exception as e:
            self._note_failure('sck: frame copy failed (%s)' % e)
            return
        if frame is None:
            self._note_failure('sck: frame copy failed')
            return
        self._first_video.set()
        self._enqueue(self._video_q, frame)

    def _on_audio(self, sample):
        try:
            data = _sck_audio_bytes(sample)
        except Exception:
            data = b''
        if not data:
            return
        self._audio_seen.set()
        self._audio_pending.clear()
        self._enqueue(self._audio_q, data)

    @staticmethod
    def _enqueue(queue, item):
        """Put without ever blocking; oldest goes when full."""
        while True:
            try:
                queue.put_nowait(item)
                return
            except Full:
                try:
                    queue.get_nowait()
                except Empty:
                    pass

    # -- threads -----------------------------------------------------------

    def _run(self, feed_audio):
        if self._stop.is_set():
            return
        modules = _sck_modules()
        if not modules:
            self._note_failure('sck: bindings vanished after the probe')
            return
        _objc, Foundation, CoreMedia, SCK = modules
        try:
            self._screen_type = int(SCK.SCStreamOutputTypeScreen)
            self._audio_type = int(SCK.SCStreamOutputTypeAudio)
            content = _shareable_content(Foundation, SCK)
            if content is None:
                self._note_failure('sck: shareable content unavailable')
                return
            wanted = int((self._spec or {}).get('display_id', -1))
            display = None
            for one in content.displays():
                if int(one.displayID()) == wanted:
                    display = one
                    break
            if display is None:
                self._note_failure('sck: display {} is gone'.format(wanted))
                return
            width, height = self._spec['size']
            config = SCK.SCStreamConfiguration.alloc().init()
            config.setWidth_(width)
            config.setHeight_(height)
            config.setScalesToFit_(True)
            config.setMinimumFrameInterval_(CoreMedia.CMTimeMake(1, int(FPS)))
            config.setQueueDepth_(3)
            config.setShowsCursor_(bool(self._spec.get('cursor')))
            config.setPixelFormat_(SCK_PIXEL_FORMAT_420V)
            feed = bool(feed_audio) and self._audio_w is not None
            config.setCapturesAudio_(feed)
            config.setExcludesCurrentProcessAudio_(True)
            config.setSampleRate_(48000)
            config.setChannelCount_(2)
            filt = _sck_filter(SCK, display)
            handler = _sck_handler_class(Foundation)()
            handler._feeder = self
            self._sinks.append(handler)
            stream = SCK.SCStream.alloc(
            ).initWithFilter_configuration_delegate_(filt, config, handler)
            self._stream = stream
            ok, err = stream.addStreamOutput_type_sampleHandlerQueue_error_(
                handler, self._screen_type, None, None)
            if not ok:
                self._note_failure('sck: cannot attach the video output '
                                   '({})'.format(err))
                return
            if feed:
                ok, err = stream.\
                    addStreamOutput_type_sampleHandlerQueue_error_(
                        handler, self._audio_type, None, None)
                if not ok:
                    # Video-only is still better than avfoundation; the
                    # console line will say so once the grace below runs
                    # out (this path only shortens it).
                    logger.warning('cannot attach the SCK audio output: %s',
                                   err)
                    self._abandon_audio()
            start_error = {}

            def on_start(*args):
                start_error['error'] = args[0] if args else None
                start_error['done'].set()

            start_error['done'] = threading.Event()
            stream.startCaptureWithCompletionHandler_(on_start)
            if not start_error['done'].wait(8.0):
                self._note_failure('sck: SCStream did not start within 8 s')
                return
            if start_error['error'] is not None:
                self._note_failure('sck: SCStream start refused ({})'
                                   .format(start_error['error']))
                return
            if not self._first_video.wait(SCK_FIRST_FRAME_SECONDS):
                self._note_failure(SCK_REASON_NO_VIDEO)
                return
            if feed:
                deadline = time.monotonic() + SCK_AUDIO_GRACE_SECONDS
                while not self._stop.is_set() and \
                        not self._audio_seen.is_set():
                    if time.monotonic() >= deadline:
                        self._abandon_audio()
                        break
                    time.sleep(0.05)
            while not self._stop.is_set():
                time.sleep(0.2)
        except Exception as e:
            self._note_failure('sck: {}'.format(e))

    def _write_video(self):
        while not self._stop.is_set():
            writer = self._video_w
            if writer is None:
                break
            try:
                item = self._video_q.get(timeout=0.2)
            except Empty:
                continue
            if item is None:
                break
            try:
                writer.write(item)
                writer.flush()
            except (OSError, ValueError) as e:
                self._note_failure('sck: encoder pipe closed (%s)' % e)
                break

    def _write_audio(self):
        while not self._stop.is_set():
            try:
                item = self._audio_q.get(timeout=0.2)
            except Empty:
                continue
            if item is None:
                break
            view = memoryview(item)
            try:
                with self._audio_lock:
                    fd = self._audio_w
                    if fd is None:
                        break
                    while view:
                        view = view[os.write(fd, view):]
            except OSError as e:
                if self._stop.is_set():
                    break
                logger.debug('SCK audio write failed: %s', e)
                self._abandon_audio()
                break


#: What `ffmpeg -f dshow -list_devices true -i dummy` prints on Windows
#: (verified against ffmpeg 8.1.2 on a real Windows 11 box):
#:
#:   [in#0 @ 00000000007c1300] "Astra Pro HD Camera" (video)
#:   [in#0 @ 00000000007c1300]   Alternative name "@device_pnp_\\?\usb#vid_..."
#:   [in#0 @ 00000000007c1300] "OBS Virtual Camera" (none)
#:   [in#0 @ 00000000007c1300] "立体声混音 (Realtek(R) Audio)" (audio)
#:
#: The quoted name is what `-i audio=<name>` takes, so that is what is kept.
#: The marker in parentheses is the only thing on the line that says which list
#: it belongs to, and the `Alternative name` line is skipped by simply not
#: matching -- both details are read off real output rather than assumed, the
#: same mistake §4.2 records for the avfoundation list.
#:
#: Note the third marker: a device whose pin category cannot be resolved prints
#: `(none)` (OBS Virtual Camera did, on that machine). Such a device is not
#: listed as either video or audio -- deliberately, because the alternative
#: (guessing "video") is one bad guess away from being handed to `audio=<name>`
#: and taking the whole capture down. Nothing in this plugin needs a `(none)`
#: device: the picture comes from ddagrab (or its gdigrab fallback), and only an
#: audio tap is looked for here.
_DSHOW_DEVICE = re.compile(r'"([^"]+)"\s*\((video|audio)\)')

#: ffmpeg also prints an `Alternative name` for each device -- an ASCII
#: identifier (`@device_cm_{GUID}\wave_{GUID}`) that looks like the obvious way
#: to dodge every code-page problem. Measured on the real box: it is not.
#: ffmpeg 8.1.2 answers `Error opening input file @device_cm_{...}\wave_{...}`
#: for a tap that works fine when named, so the name -- correctly decoded -- is
#: what gets handed over. See `_dshow_text`.
#: Names of Windows recording devices that carry the *output* back to us.
#:
#: Windows gives ffmpeg no system-audio tap any more than macOS does: the
#: sound has to exist as a recording device before any input can read it.
#: Some drivers ship the built-in 立体声混音 (Stereo Mix); everyone else
#: installs a virtual cable. Matched on substrings, because the same device
#: is named differently by every vendor.
WINDOWS_LOOPBACK_HINTS = ('stereo mix', '立体声混音', 'what u hear',
                          'wave out mix', 'virtual-audio-capturer',
                          'cable output', 'voicemeeter', 'loopback',
                          'soundflower', 'vb-audio')


def _dshow_text(raw):
    """ffmpeg's console output, decoded so a device name survives the trip.

    `errors='replace'` is what broke this: on a cp936 console it turns every
    non-ASCII device name into characters that can never be handed back to
    ffmpeg -- the bytes are gone, not merely wrong -- and the mangled name was
    then passed as `audio=<name>`, which ffmpeg can only refuse. The mirror paid
    for it with a full no-frame budget and a session that came back silent.

    Trying the encodings in order keeps the name intact whichever code page the
    console is on; UTF-8 goes first because it is the only one that *fails*
    (rather than silently producing mojibake) when the bytes are not what it
    expects. Latin-1 never raises, so it is the last resort.
    """
    for encoding in ('utf-8', 'mbcs', locale.getpreferredencoding(False),
                     'cp936', 'latin-1'):
        try:
            return raw.decode(encoding)
        except (UnicodeDecodeError, LookupError):
            continue
    return raw.decode('utf-8', 'replace')


def _dshow_lists(ffmpeg):
    """(video names, audio names) from the dshow device table.

    `-i dummy` is the documented way to ask for the table: ffmpeg fails to open
    that input and prints both lists on its way out, so the exit status is
    ignored here and only the output is read.
    """
    try:
        proc = subprocess.run([ffmpeg, '-hide_banner', '-list_devices', 'true',
                               '-f', 'dshow', '-i', 'dummy'],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              timeout=10)
        text = _dshow_text(proc.stdout)
    except Exception as e:
        logger.error("cannot list dshow devices: %s", e)
        return [], []
    return _parse_dshow_devices(text)


def _parse_dshow_devices(text):
    """The two dshow lists, read out of ffmpeg's own log lines."""
    blocks = {'video': [], 'audio': []}
    for name, kind in _DSHOW_DEVICE.findall(str(text or '')):
        name = name.strip()
        # A name with a double quote in it cannot be handed to ffmpeg as
        # `audio=<name>` without escaping we do not do, so it is left out
        # rather than passed on as a broken argument.
        if not name or '"' in name:
            continue
        blocks[kind].append(name)
    return blocks['video'], blocks['audio']


def windows_loopback_device(audios):
    """The first audio device that mirrors the output, or None.

    The device table's own order decides, so a machine with both Stereo Mix and
    an installed cable uses whichever its driver lists first -- the user's
    machine is not ours to re-rank.
    """
    for name in audios or []:
        lowered = name.lower()
        if any(hint in lowered for hint in WINDOWS_LOOPBACK_HINTS):
            return name
    return None


#: Desktop Duplication (ddagrab) became the preferred Windows picture source in
#: 0.20, and the reason is measured, not architectural. On a real box (Windows
#: 11, ffmpeg 8.1.2, two monitors 2560x1440 + 1440x2560 rotated):
#:
#:   * gdigrab captures the *virtual desktop* -- 4000x2571 across that pair --
#:     and an odd height is not something the encoders accept:
#:     `[libx264] height not divisible by 2 (4000x2571)`, zero bytes, exit
#:     -542398533. The 原画 profile sends no `-vf`, so on a setup like this the
#:     Windows 原画 path produced nothing at all, on any quality rung.
#:   * ddagrab captures one DXGI output at a time, at that output's own even
#:     size, which is also exactly what the screen picker asks for. Attach is
#:     cheap: a `-frames:v 1` run into the null muxer, under a second per
#:     output on the box measured.
#:   * the audio tap is unchanged -- dshow below still carries Stereo Mix, and
#:     a ddagrab + dshow pair was measured to mux (AAC, ~0.30 cores at 1440p24,
#:     first byte 2.0 s).
#:
#: The filter is consumed inside the lavfi input string
#: (`ddagrab=...,hwdownload,format=bgra`), so the `-vf` composition in
#: `build_ffmpeg_command` / `build_dlna_command` never has to know about it,
#: and 原画 (no `-vf`) works for the same reason.

#: How many DXGI outputs to try. Four covers everything a desktop machine
#: plausibly drives, and the probe stops at the first refusal -- outputs
#: enumerate from 0, and on every machine measured, the first hole is where
#: the list ends.
DDAGRAB_MAX_OUTPUTS = 4

#: Attach test results, keyed by ffmpeg path only: the test pins draw_mouse=0
#: and the frame rate, so the cursor setting does not enter the answer. One
#: test spawns one ffmpeg per output, and the console must not pay that on
#: every read; `invalidate_capture_cache()` clears this along with the probe
#: cache.
_ddagrab_cache = {}

#: ffmpeg builds whose ddagrab refused the display *at runtime* in this
#: process. Like `_audio_refused`, this is a latch for the whole run: a display
#: the DWM will not duplicate will not start duplicating on the next session,
#: and the only thing re-asking buys is the same stall. Deliberately NOT
#: cleared by `invalidate_capture_cache()` -- see there.
_ddagrab_refused = set()

#: Attach either answers quickly or it is not going to answer; measured runs
#: were all under a second. A wedged desktop-duplication call must not hold the
#: probe hostage -- the settings page reads through `probe_capture`.
DDAGRAB_ATTACH_TIMEOUT = 10.0

#: The dimensions out of the one line a successful attach prints:
#:   Stream #0:0: Video: wrapped_avframe, bgra, 2560x1440 [SAR 1:1 DAR 16:9]...
#: `wrapped_avframe` appears twice in that log (input and null output) and both
#: carry the same WxH, so the first match is the answer.
_DDAGRAB_SIZE = re.compile(r'wrapped_avframe[^\n]*?(\d{3,5})x(\d{3,5})')

#: What a refused output prints (verbatim, ffmpeg 8.1.2, output 2 of 2):
#:
#:   [Parsed_ddagrab_0 @ ...] Failed to enumerate DXGI output 2
#:   [Parsed_ddagrab_0 @ ...] Failed to configure output pad on Parsed_ddagrab_0
#:   [in#0 @ ...] Error opening input: Generic error in an external library
#:   Error opening input file ddagrab=output_idx=2:...
#:
#: The `Error opening input file ddagrab=` line is the anchor -- ffmpeg's own
#: verdict, and it names the filter -- while the Parsed_ddagrab tag lines catch
#: a refusal that words itself differently.
_DDAGRAB_FAILURE = re.compile(
    r'Error opening input file ddagrab='
    r'|\[Parsed_ddagrab_\d+ @ \w+\] (?:Failed|Error)')


def _ddagrab_attach_test(ffmpeg, index):
    """One output's attach answer: 'WxH', '' (size unknown), or None (refused).

    Runs the real filter against the real output for exactly one frame: a
    successful attach prints the stream line with the dimensions, a refusal
    exits non-zero with the DXGI complaint. The timeout is here because this
    is reached from the settings page through `probe_capture` -- a wedged
    duplication call would otherwise hold the whole console.
    """
    command = [ffmpeg, '-hide_banner', '-loglevel', 'info', '-nostdin',
               '-f', 'lavfi', '-i',
               'ddagrab=output_idx={}:framerate={}:draw_mouse=0'
               ',hwdownload,format=bgra'.format(index, FPS),
               '-frames:v', '1', '-f', 'null', '-']
    try:
        proc = subprocess.run(command, stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, stdin=subprocess.DEVNULL,
                              timeout=DDAGRAB_ATTACH_TIMEOUT)
    except Exception as e:
        logger.info('ddagrab attach test for output %s did not run: %s',
                    index, e)
        return None
    text = _dshow_text(proc.stderr)
    if proc.returncode != 0:
        for line in text.splitlines():
            if _DDAGRAB_FAILURE.search(line):
                logger.info('ddagrab output %s: %s', index, line.strip())
                break
        else:
            logger.info('ddagrab attach test for output %s exited %s',
                        index, proc.returncode)
        return None
    match = _DDAGRAB_SIZE.search(text)
    size = '{}x{}'.format(match.group(1), match.group(2)) if match else ''
    logger.info('ddagrab output %s attaches at %s', index, size or 'unknown')
    return size


def _ddagrab_outputs(ffmpeg):
    """[(output index, 'WxH')] for every DXGI output ddagrab can attach to.

    Empty when the build has no ddagrab filter or output 0 refuses. Cached per
    ffmpeg path because each entry costs one spawned ffmpeg.
    """
    if ffmpeg in _ddagrab_cache:
        return _ddagrab_cache[ffmpeg]
    outputs = []
    for index in range(DDAGRAB_MAX_OUTPUTS):
        size = _ddagrab_attach_test(ffmpeg, index)
        if size is None:
            break
        outputs.append((index, size))
    _ddagrab_cache[ffmpeg] = outputs
    return outputs


def _ddagrab_refusal(proc, tail, drain):
    """Whether a dead ddagrab session died of the display refusing to attach.

    Only meaningful once the encoder process is gone: a running process has
    not refused anything (a slow start is the audio rung's territory). The
    stderr reader is a separate thread, so its tail can lag the process exit
    by a moment -- joining it is what makes the verdict about the dead
    process rather than about how fast the reader was.
    """
    if proc.poll() is None:
        return False
    if drain is not None:
        drain.join(timeout=1.0)
    return bool(_DDAGRAB_FAILURE.search('\n'.join(tail)))


def _probe_windows(ffmpeg, cursor=True):
    """Desktop Duplication for the picture, plus a loopback tap for the sound.

    Unlike macOS there is no driver to install for us to trigger: either the
    machine already has a device that carries the output (Stereo Mix, or a
    virtual cable someone installed), or the mirror is video only and the
    console says which device to switch on. That is why this probe asks dshow
    once and never tries to make a device appear.

    gdigrab is the fallback, and it is not the safer choice: whole-virtual-
    desktop capture can have an odd height, and an odd height is a zero-byte
    encode (see the DDAGRAB_* notes above). `_ddagrab_refused` skips the
    enquiry entirely after a runtime refusal: the display has already answered.
    """
    draw = '1' if cursor else '0'
    outputs = [] if ffmpeg in _ddagrab_refused else _ddagrab_outputs(ffmpeg)
    if outputs:
        wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
        index = outputs[0][0]
        if wanted.isdigit():
            for output, _size in outputs:
                if str(output) == wanted:
                    index = output
                    break
            else:
                logger.warning("screen %s is gone, falling back to %s",
                               wanted, index)
        source = ['-f', 'lavfi', '-i',
                  'ddagrab=output_idx={}:framerate={}:draw_mouse={}'
                  ',hwdownload,format=bgra'.format(index, FPS, draw)]
        screens = [(output, size or '输出 {}'.format(output + 1))
                   for output, size in outputs]
        _videos, audios = _dshow_lists(ffmpeg)
        device = windows_loopback_device(audios)
        if device is None:
            return _Capture('屏幕 (Desktop Duplication)', [source],
                            screens=screens, method='ddagrab')
        return _Capture(
            '屏幕 (Desktop Duplication) + 系统声音 ({})'.format(device),
            [source,
             # The name, not the ASCII identifier ffmpeg prints under it: handing
             # ffmpeg `audio=@device_cm_{...}\wave_{...}` was measured on the real
             # box -- ffmpeg 8.1.2 answers `Error opening input file` and the tap
             # delivers nothing, while the name (once decoded correctly, see
             # `_dshow_text`) gives a first frame in 1.4 s.
             ['-f', 'dshow', '-i', 'audio={}'.format(device)]],
            audio_map='1:a:0', screens=screens, method='ddagrab')
    base = ['-f', 'gdigrab', '-framerate', str(FPS),
            '-draw_mouse', draw]
    _videos, audios = _dshow_lists(ffmpeg)
    device = windows_loopback_device(audios)
    if device is None:
        return _Capture('Desktop (GDI)', [base + ['-i', 'desktop']],
                        method='gdi')
    return _Capture(
        '屏幕 (GDI) + 系统声音 ({})'.format(device),
        [base + ['-i', 'desktop'],
         # (Same name-not-identifier rule as above.)
         ['-f', 'dshow', '-i', 'audio={}'.format(device)]],
        audio_map='1:a:0', method='gdi')


def _default_pulse_monitor():
    """`<default sink>.monitor`, the PipeWire/PulseAudio system-audio tap."""
    try:
        proc = subprocess.run(['pactl', 'get-default-sink'],
                              stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=3)
    except OSError as e:
        # Usually no pactl at all, i.e. no PulseAudio/PipeWire on this box --
        # a different answer than "pactl is there but says nothing", and the
        # probe used to discard both.
        logger.debug("pulse tap probe: cannot run pactl: %s", e)
        return None
    except Exception as e:
        logger.debug("pulse tap probe: pactl failed: %s", e)
        return None
    sink = proc.stdout.decode('utf-8', 'replace').strip()
    if not sink:
        logger.debug("pulse tap probe: pactl exited %s without a sink name (%s)",
                     proc.returncode,
                     proc.stderr.decode('utf-8', 'replace').strip()[:200])
    return sink + '.monitor' if sink else None


def _probe_linux(ffmpeg, cursor=True):
    display = os.environ.get('DISPLAY')
    if not display:
        # x11grab cannot see a Wayland session; there is no ffmpeg-native
        # Wayland capture to fall back to.
        return None
    inputs = [['-f', 'x11grab', '-framerate', str(FPS),
               '-draw_mouse', '1' if cursor else '0', '-i',
               '{}+0,0'.format(display)]]
    monitor = _default_pulse_monitor()
    if monitor:
        inputs.append(['-f', 'pulse', '-i', monitor])
        return _Capture('屏幕 (X11) + 系统声音 (PulseAudio)',
                        inputs, audio_map='1:a:0')
    return _Capture('屏幕 (X11)', inputs)


def uses_videotoolbox(kind, platform=None):
    """Whether `encoder_args` will really select h264_videotoolbox.

    Its own predicate because two other things have to agree with
    `encoder_args` about which encoder is live -- the bitrate it asks for
    (`rate_target`) and the pixel format it asks for (`encoder_pix_fmt`) -- and
    a second copy of this condition is exactly how they stop agreeing.
    """
    return kind == 'hardware' and hardware_encoder(platform) == _VT_ENCODER


#: The encoder each platform's「硬件编码」means, and why the list is as short as
#: it is. Windows used to have no entry at all -- `auto` landed on x264 on every
#: platform but macOS because `h264_videotoolbox` was the only name this file
#: knew -- which is how the Windows→Mac case ended up paying for a CPU encoder
#: on a machine with an RTX 5060 Ti in it.
#:
#:     0.44 of a core  libx264, the shipped argv, 1080p/24 fps
#:     0.21 of a core  h264_nvenc, same argv shape, same fps, same 1.00x
#:
#: (2026-10-02, the interactive-session matrix on .68: one animated desktop,
#: four rows, 25 s each, CPU sampled between two `Get-Process` reads. Both
#: produced ~19 MB at `-b:v 6000000` and ran the muxer at speed=1.00x, so this
#: is the same work priced twice, not two different amounts of video.)
_VT_ENCODER = 'h264_videotoolbox'
_NVENC = 'h264_nvenc'
HARDWARE_ENCODERS = {'darwin': _VT_ENCODER, 'win32': _NVENC}


def hardware_encoder(platform=None):
    """The encoder name the hardware switch selects here, or None for "this
    platform has no hardware encoder this code is willing to name".

    Linux is deliberately absent rather than unfinished: `h264_vaapi` needs a
    DRM render node and a working `FFMPEG_VAAPI` path that has never been
    exercised here, and the pipeline already runs at 24 fps on CPU. Naming an
    encoder we cannot measure on any machine we have is how the `-level 42`
    class of bug gets shipped (see `vt_level`), and §4.8's other lesson -- the
    avfoundation parser written against an output format that never existed --
    says the same thing about test fixtures.
    """
    return HARDWARE_ENCODERS.get(platform or sys.platform)


def uses_nvenc(kind, platform=None):
    """Whether `encoder_args` will really select h264_nvenc. Same reason
    `uses_videotoolbox` exists: three call sites have to agree about which
    encoder is live, and they must not each keep their own copy of the test.
    """
    return kind == 'hardware' and hardware_encoder(platform) == _NVENC


def encoder_pix_fmt(kind, platform=None):
    """The `-pix_fmt` this encoder wants, measured rather than assumed.

    nvenc was probed with nv12 (both the 0.5 s capability probe and the 25 s
    matrix row), and asking it for yuv420p instead inserts a conversion the
    encoder does not need -- ffmpeg would still work, but every number in the
    comment above was measured on the shape being shipped here.
    """
    return 'nv12' if uses_nvenc(kind, platform) else 'yuv420p'


#: What the switch answers after it has been turned, per platform's encoder.
#:
#: The macOS software row used to read「延迟约 45 毫秒，代价约 4.5 核」. Both
#: numbers were the offline file-source measurement (see ENCODER_AUTO), which is
#: exactly the mistake that comment goes on about: 4.5 cores is what x264 burns
#: when it is *allowed* to run 486 fps, and a live 24 fps mirror pays 0.59. The
#: rows below quote the live capture numbers, and the Windows software row says
#: which number was never measured on that machine rather than borrowing
#: macOS's.
ENCODER_NOTES = {
    _VT_ENCODER: {
        'hardware': 'VideoToolbox 硬件编码（实测 0.41 核；'
                    '首帧比 x264 晚约 200 毫秒）',
        'software': '软件编码（x264，实测 0.59 核、跑满 24 fps；'
                    '首帧比硬件早约 200 毫秒）',
    },
    _NVENC: {
        'hardware': 'NVENC 硬件编码（实测 0.21 核，帧率、码率与 x264 那一路一致）',
        'software': '软件编码（x264，实测 0.44 核、跑满 24 fps；'
                    '这台机器上的首帧差值没有量过）',
    },
}

#: A machine with no hardware encoder this file can name still has to say
#: something true when the user picks 软件编码 -- so this row makes no
#: comparison, because there is nothing on this platform to compare against.
ENCODER_NOTE_SOFTWARE_DEFAULT = '软件编码（x264，跑满 24 fps）'

#: The switch's own words. The settings page used to carry「VideoToolbox
#: 硬件编码」as a literal, so on a Windows machine the control named an encoder
#: that machine was never offered -- the same one-switch-two-dialects mistake
#: the「统计信息」card made until it started reading `-c:v` off the argv.
ENCODER_SWITCH_LABELS = {_VT_ENCODER: 'VideoToolbox 硬件编码',
                         _NVENC: 'NVENC 硬件编码'}
ENCODER_SWITCH_LABEL_DEFAULT = '硬件编码'


def encoder_switch_label(platform=None):
    """The label for the hardware switch on this platform."""
    return ENCODER_SWITCH_LABELS.get(hardware_encoder(platform) or '',
                                     ENCODER_SWITCH_LABEL_DEFAULT)


def encoder_note(kind, platform=None):
    """The confirmation sentence for the encoder the switch just stored."""
    table = ENCODER_NOTES.get(hardware_encoder(platform) or '')
    if not table:
        return ENCODER_NOTE_SOFTWARE_DEFAULT
    return table[kind]


#: VideoToolbox's rate control undershoots whatever `-b:v` it is handed by
#: 30-33%: asked for 4 Mbps it emits 2802 kbit. That is conservatism, not a
#: quality loss -- at a 4 Mbps target VT scores 91.47 VMAF against libx264
#: ultrafast+zerolatency's 90.78 at 4216 kbit, a better picture at two thirds
#: the bitrate. So when VT is the encoder we ask for 1.5x, and the number that
#: reaches the wire is the number the quality menu promised.
#:
#: nvenc gets no multiplier because it does not need one, which is the first
#: thing measured about it (2026-10-02, .68, four argv shapes around a 6000 kbit
#: ask): 6052, 6081, 6087 and 6029 kbit/s on the wire. Applying VT's 1.5x here
#: would put 9 Mbps on a link the menu described as 6.
VT_BITRATE_MULT = 1.5


def rate_target(bitrate, kind, platform=None):
    """The `-b:v` value that actually produces `bitrate` on the wire."""
    if uses_videotoolbox(kind, platform):
        return int(bitrate * VT_BITRATE_MULT)
    return int(bitrate)


#: The H.264 level VideoToolbox is told to write into the SPS, by the height of
#: the picture it is actually encoding. A level is a promise about frame size
#: and luma rate; pinning one that the picture does not fit is not a constraint,
#: it is a crash -- see `vt_level`.
VT_LEVELS = ((1080, '42'), (2160, '51'))


def vt_level(height):
    """The `-level` value for a picture of `height` lines, or None for "let
    VideoToolbox work it out".

    This used to be a hardcoded `'42'` in `encoder_args`, and it was a live
    bug on this machine (measured 2026-10-02, `h264_videotoolbox`, 3 s of real
    desktop, same capture and same rate control in every row):

        1080p   + `-level 42`  + maxrate/bufsize  -> works, 3/3
        2160p   + `-level 42`                     -> exit 187, 0 bytes
        2160p   + `-level 51`                     -> works, 2130382 bytes
        2160p   + no `-level`                     -> works, VT picks its own
        native 3456x2234 + `-level 42`            -> exit 187, 0 bytes, 2/2
        native 3456x2234 + `-level 51`            -> works, 1797725 bytes
        native 3456x2234 + no `-level`            -> works, 1799372 bytes
        1080p   + `-level 42`, no maxrate/bufsize -> fails too
        no `-level`, VT's own choice at 1080p     -> level 4.0, High, 1920x1080

    Level 4.2 caps the frame at 8704 macroblocks (2208x1242); a 3456x2234
    desktop is 30384 of them, so the encoder is handed a promise it cannot
    keep and dies. ffmpeg prints
    `Error encoding a frame: Generic error in an external library` and exits
    187 with **five lines of stderr and no output at all**.

    The reason that is worse than an ordinary crash: `auto` picks hardware
    whenever the probe finds it, so on a Mac the shipped default for the 原画
    preset was "produce zero frames", and the no-frame path sends the user to
    the **screen-recording permission** door (`PERMISSION_DOOR`) -- the wrong
    door for a number we typed ourselves. `has_hardware_encoder` cannot catch
    it either: it greps `ffmpeg -encoders`, which happily lists an encoder
    that then refuses the picture.

    So: pin the level where the pin is a real promise (a Cast device's decoder
    limit, which is what `42` was chosen for, and which only means something up
    to 1080p), name the next one up for pictures that fit it, and **omit it
    entirely when we cannot know the size** (`height == 0` is the 原画 preset:
    "do not scale", so the picture is whatever the display is -- 3456x2234
    here, 5120x2880 on a Studio Display). Omitting is not a shrug: measured
    above, VT computes a level that matches the real picture at every size we
    tried, and at 1080p it picks **4.0**, which is *more* conservative than the
    4.2 we were pinning. A table that stopped at 5.1 would just move this bug
    to the next display someone buys.
    """
    if not height:
        return None
    for limit, level in VT_LEVELS:
        if height <= limit:
            return level
    return None


def encoder_args(kind, platform=None, height=None, output=None):
    """Video encoder flags.

    Hardware encoding is opt-in and goes through `hardware_encoder()`, so the
    encoder a machine gets named there is the only encoder this function will
    write. macOS answers `h264_videotoolbox` (Apple's one real tap, and unlike
    the Castify reference -- which never probes for it and always lands on CPU
    x264 on a Mac -- we ask first, see `has_hardware_encoder`); Windows answers
    `h264_nvenc`; Linux answers nothing, so `hardware` falls through to x264
    there exactly as it did before this function knew about NVIDIA.

    Both low-latency branches carry `-flags +low_delay`: it is the first flag
    Google's own reference sender sets, and upstream `videotoolboxenc.c` keeps
    adding support for it ("ensure bitrate is set in low_delay mode"), so it is
    not an x264-only idea. `-thread_type slice` rides on the x264 branch alone,
    because videotoolbox reports *no* threading capability at all and asking
    it for a thread type is asking for an option it does not have. x264 already
    gets sliced threads implicitly from `-tune zerolatency`; naming it is cheap
    insurance against a preset change quietly costing a frame of latency.
    nvenc gets neither, because the two numbers it was chosen for (0.21 of a
    core, speed=1.00x) were measured on an argv that carries neither -- a flag
    that costs nothing to add is still a change to a measured shape.

    `height` is the encoded picture height, and only the VideoToolbox branch
    reads it (`vt_level`); x264 computes its own level and is right about it.
    It defaults to None -- "unknown" -- which for VT means "do not pin one",
    the safe direction: a missing level costs a slightly less explicit SPS,
    while a wrong one costs every frame.

    `output` names the *target* (None for every caller that is not the live
    pipeline), and one target overrides the profile: `webrtc` asks for
    `baseline`, because constrained baseline is WebRTC's mandatory-to-
    implement shape and a `high` SPS is what a strict receiver is entitled
    to refuse. The research doc's "-level 3.1 is mandatory" line describes
    that same constrained-baseline datapoint, and honouring it blindly is
    exactly the 0.10 zero-frame bug: x264 computes its own level and is
    right about it, and VT stays unpinned whenever `height` is unknown.
    """
    profile = 'baseline' if output == 'webrtc' else 'high'
    if uses_videotoolbox(kind, platform):
        args = ['-c:v', _VT_ENCODER, '-profile:v', profile]
        level = vt_level(height)
        if level:
            args += ['-level', level]
        return args + ['-flags', '+low_delay', '-realtime', '1']
    if uses_nvenc(kind, platform):
        # The spelling was measured on the machine this branch exists for,
        # because the obvious one is wrong in this build and fails silently:
        #
        #   `-tuned ll`   -> unrecognized option, **0 bytes out**
        #   `-preset ll`  -> works, but warns that it is deprecated in favour
        #                    of p1..p7 plus -tune
        #   `-preset p1 -tune ll`  -> works, no warning, lands on the asked
        #                    bitrate (6052/6081/6087/6029 kbit/s for 6000)
        #
        # `-rc vbr` is the mode that measurement was taken in; `-profile:v`
        # follows `output` like the other branches (baseline and high both open
        # clean). No `-level`: nvenc derives its own from the picture, and the
        # table `vt_level` consults is a VideoToolbox answer to a VideoToolbox
        # failure. No `-realtime`, which is a VT AVOption and not an nvenc one
        # -- passing it there is the same class of mistake as `-tuned` was, only
        # noisier.
        return ['-c:v', _NVENC, '-preset', 'p1', '-tune', 'll', '-rc', 'vbr',
                '-profile:v', profile]
    return ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
            '-profile:v', profile, '-flags', '+low_delay',
            '-thread_type', 'slice']


#: The tiny encode that asks nvenc whether it is *live*, not whether it is
#: listed. Sized to be cheap (268 ms on the machine it was written for, ffmpeg
#: startup included) and to exercise the same open path a mirror session will:
#: a real video stream, the real pixel format, the real container.
_PROBE_SOURCE = 'testsrc=size=640x360:rate=24'
_PROBE_SECONDS = '0.5'


def _encoder_opens(ffmpeg, name, pix_fmt):
    """Ask one encoder to encode half a second of test pattern.

    Returns the name on success, None on any other answer. This is the only
    honest question to ask a Windows ffmpeg: `ffmpeg -encoders` on .68 lists
    h264_nvenc, h264_qsv *and* h264_amf, and h264_qsv then refuses to open a
    session at all on the same machine --

        [h264_qsv] Error creating a MFX session: -9
        [out#0/mp4] Nothing was written into output file
        exit=-1313558101  bytes=0

    -- which is §4.8's `-level 42` lesson wearing the opposite clothes: a
    listing says the symbol is in the binary, and says nothing about the driver
    behind it. The failure is loud here (non-zero exit, zero bytes) rather than
    silent, but "loud at probe time" is only better than "loud at mirror time"
    if somebody actually runs the probe before offering the switch.
    """
    try:
        proc = subprocess.run(
            [ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin',
             '-f', 'lavfi', '-i', _PROBE_SOURCE, '-t', _PROBE_SECONDS,
             '-c:v', name, '-pix_fmt', pix_fmt,
             '-f', 'mp4', '-movflags',
             'frag_every_frame+empty_moov+default_base_moof', '-'],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
    except Exception as e:
        logger.info('cannot probe %s: %s', name, e)
        return None
    if proc.returncode or not proc.stdout:
        logger.info('%s is not usable here (exit %s): %s', name,
                    proc.returncode,
                    proc.stderr.decode('utf-8', 'replace').strip()[:300])
        return None
    return name


def has_hardware_encoder(ffmpeg, platform=None):
    """Whether `hardware` means anything on this machine.

    Cached because it spawns ffmpeg, and the answer is only ever consulted
    from a background thread or the menu's status line. The platform is part
    of the key: the same binary answers differently under a different OS, and
    the caller may name one explicitly (tests, and any future cross-check).

    Two platforms, two probe shapes, because their failure modes differ:

      * **macOS** asks the listing. VideoToolbox is an OS framework rather
        than a driver -- if the build names `h264_videotoolbox`, the operating
        system has it -- and this is the shape the encoder-latency measurements
        were taken under.
      * **Windows** asks for a real encode, for the reason in
        `_encoder_opens`: three hardware encoders are listed and the machine
        only has one of them.
    """
    key = (ffmpeg, platform or sys.platform)
    if key in _hw_encoder_cache:
        return _hw_encoder_cache[key]
    verdict = False
    name = hardware_encoder(key[1])
    if name and key[1] == 'darwin':
        try:
            proc = subprocess.run([ffmpeg, '-hide_banner', '-encoders'],
                                  stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, timeout=10)
            verdict = name.encode() in proc.stdout
        except Exception as e:
            logger.info("cannot probe the encoders: %s", e)
    elif name:
        verdict = _encoder_opens(ffmpeg, name,
                                 encoder_pix_fmt('hardware', key[1])) is not None
    _hw_encoder_cache[key] = verdict
    return verdict


def build_ffmpeg_command(ffmpeg, capture, height, bitrate, kind=DEFAULT_OUTPUT,
                         encoder='software', profile=None, audio_fd=None,
                         max_seconds=None):
    # `audio_fd` resolves the ScreenCaptureKit audio pipe token (see
    # `_AUDIO_FD_TOKEN`): the command carries a placeholder because the fd does
    # not exist until the spawn, and resolution is deliberately loud -- a
    # command that still holds the token at exec time is a bug, not a video
    # stream. Every non-SCK capture passes None and the resolver is a no-op.
    if kind == 'dlna':
        # A renderer of ten years ago is being handed this file, so the
        # profile -- not the user's quality menu -- decides the shape: the
        # height, the codec, the rate control and the container all have to be
        # ones that TV's demuxer knows.
        return _resolve_audio_fd(build_dlna_command(ffmpeg, capture,
                                  profile or dlna_profile(), encoder,
                                  max_seconds=max_seconds), audio_fd)
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map and kind not in ('caststream', 'webrtc'):
        cmd += ['-map', capture.audio_map,
                '-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2']
        cmd += AUDIO_RESAMPLE
    else:
        # Both low-latency targets take video only, which is why their menu
        # labels say 无声音 out loud: the mirroring app has no audio stream
        # wired up at all, and the WebRTC page is a bare <video> -- this
        # fork's audio plane for it is not built (v1 is video-only, stated
        # in the same words as caststream).
        cmd += ['-an']
    extra = []
    if kind == 'caststream':
        # The OFFER promised a frame of exactly this size, so pin it and
        # letterbox rather than scaling by height and hoping.
        _width, height, video_filter = cast_stream_shape(height)
        bitrate = min(bitrate, cast_stream_bitrate_cap())
        cmd += ['-vf', video_filter]
    elif height:
        # -2 keeps the aspect ratio and still satisfies yuv420p's even edges.
        cmd += ['-vf', 'scale=-2:{}'.format(height)]
    if kind in ('caststream', 'webrtc') and encoder == 'software':
        # `-aud` makes every picture start with an access unit delimiter,
        # which is what lets the splitter close a frame at its boundary
        # instead of one frame late -- at 24 fps that is 42 ms of the
        # latency these targets exist to avoid. `scenecut=0` because x264
        # deciding on its own when to redraw is not a promise the OFFER's
        # one-second GOP can survive, and the PLI path assumes it; the
        # WebRTC scan (`_AccessUnits`) reads the same boundaries and its
        # own `-g` is the recovery promise there. `keyint` follows each
        # target's own `gop_size`, so caststream's argv keeps its `FPS`
        # spelling through the same formatter. `-aud` is an AVOption, not
        # an ffmpeg flag: it takes its value as a separate argument, so
        # omitting the 1 eats the next option.
        extra = ['-aud', '1', '-x264-params',
                 'keyint={}:min_keyint={}:scenecut=0'.format(
                     gop_size(kind), gop_size(kind))]
    # `height` here is the height that will actually be encoded, not the one
    # the menu asked for: the caststream branch above has just replaced it with
    # the size the OFFER pinned, and 0 means "do not scale" (原画), which is
    # exactly the case `vt_level` must not pin a level for.
    cmd += encoder_args(encoder, height=height, output=kind)
    # Decided once, then used for both `-b:v` and the VBV that bounds it: a
    # ceiling computed from a different number than the target is not a
    # ceiling. Note the ordering against the caststream clamp above -- that one
    # bounds the *wire* rate the OFFER promised, and `rate_target` then inflates
    # the request so the wire actually lands there. Reversing the two would
    # either overshoot the promise or underfill it.
    target = rate_target(bitrate, encoder)
    # `-r` is not a frame-rate *preference* here, it is the thing that keeps the
    # encoder from being handed an impossible one. Measured on this machine
    # (2026-10-02) with the shipped argv, one variable changed, video-only
    # capture, 原画 (no `-vf` at all so the scaler is not involved):
    #
    #     no output `-r`  -> 0 bytes, stderr
    #         [libx264] MB rate (14400000000) > level limit (16711680)
    #     `-r 24`         -> 3,932,160 bytes
    #
    # The mechanism: when avfoundation cannot estimate the rate -- it prints
    # `Configuration of video device failed, falling back to default` and then
    # `Stream #0: not enough frames to estimate rate` -- the input arrives with
    # a degenerate timebase, x264 multiplies the frame's macroblock count by
    # something near 1e6 to get its MB rate, cannot find a level that covers
    # it, and encodes **nothing at all**. VideoToolbox tolerates the same input
    # and emits bytes, which is why this never showed up on the `auto` default
    # on a Mac: the fallback to software is precisely when the capture is
    # already unhappy, and that is when x264 dies.
    #
    # So the failure the user sees is "no frames", and the door the no-frame
    # path points at is **screen-recording permission** (`PERMISSION_DOOR`) --
    # a real door, but not this one. Pinning the rate we already promise
    # everywhere else closes it: the input asks for `-framerate FPS`, the GOP
    # is `gop_size(kind)` frames, the Cast Streaming OFFER advertises `FPS` in
    # its `resolutions`, and `build_dlna_command` has always passed
    # `-r profile.fps`. This branch was the only one left guessing.
    #
    # What it costs: CFR duplicates frames when the capture delivers fewer
    # than FPS (a sleeping or occluded display). That is the honest behaviour
    # for a live stream whose receiver is told 24 fps -- the alternative is a
    # clock that runs slow. **It does collide with `mpdecimate`**, which needs
    # `-fps_mode vfr`: CFR puts straight back every frame mpdecimate dropped,
    # so if that filter is ever added here it must replace this `-r`, not
    # ride alongside it.
    cmd += ['-pix_fmt', encoder_pix_fmt(encoder), '-r', str(FPS),
            '-g', str(gop_size(kind)), '-b:v', str(target)]
    cmd += rate_caps(target)
    # Encoder private options only resolve after -c:v, so they ride at the end.
    cmd += extra
    # The ceiling the television targets were promised. Re-checked against
    # `MAX_DURATION_TARGETS` here rather than trusted from the caller: a live
    # shape whose viewer follows the live edge must never be cut short just
    # because some call site passed a number for it. It is an output option, so
    # it rides after the encoder args and before the muxer flags.
    # `_session_diagnostics` reads this same argv back with `_int_flag`, so the
    #「统计信息」card cannot promise a bound a run does not carry.
    cmd += duration_args(max_seconds
                         if kind in MAX_DURATION_TARGETS else None)
    cmd += OUTPUTS[kind][3]
    return _resolve_audio_fd(cmd, audio_fd)


def build_dlna_command(ffmpeg, capture, profile, encoder='software',
                       max_seconds=None):
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin']
    for one_input in capture.inputs:
        cmd += one_input
    cmd += ['-map', '0:v:0']
    if capture.audio_map:
        cmd += ['-map', capture.audio_map] + profile.audio_args
    else:
        cmd += ['-an']
    # setdar because a scaled desktop is not 4:3 just because the frame is.
    cmd += ['-vf', profile.video_filter() + ',setdar=16/9']
    if profile.video_args is not None:
        cmd += profile.video_args
    else:
        # The H.264 shapes: same encoder path as the other targets, with the
        # frame rate the profile advertises and a quarter-second GOP -- the
        # picture a television can start on is the one after an IDR, and on
        # Matroska the cluster holding it cannot be published before that IDR
        # arrives. See `live_gop` for what that buys and what it costs.
        # The profile owns the frame size (`video_filter` above scales to it),
        # so it also owns the level: a profile is a promise about what the
        # television's demuxer knows, and `-level` is part of that promise.
        cmd += encoder_args(encoder, height=profile.height)
        # `profile.total_bitrate` is what the fake-file shape advertises as a
        # Content-Length and what the prefill budget is derived from, so it
        # describes the wire. `rate_target` turns that into the request that
        # actually produces it -- on VideoToolbox the two differ by 1.5x, and
        # before this they differed by 30% in the other direction, which made
        # every advertised number on this target a mild fiction.
        target = rate_target(profile.bitrate, encoder)
        cmd += ['-pix_fmt', encoder_pix_fmt(encoder),
                '-g', str(live_gop(profile.fps)),
                '-r', str(profile.fps), '-b:v', str(target)]
        cmd += rate_caps(target)
    cmd += profile.muxer + duration_args(max_seconds) + ['pipe:1']
    return cmd


def capture_unavailable_hint():
    if sys.platform == 'darwin':
        return 'ffmpeg 没有列出任何屏幕采集设备（avfoundation）'
    if sys.platform == 'win32':
        return '这个 ffmpeg 构建既没有 ddagrab 也没有 gdigrab，无法采集桌面'
    #: Derived, not written out: this sentence used to say「三种目标」and stayed
    #: saying it for two releases after the fourth target shipped, which is the
    #: failure mode a count in prose always has -- nobody re-reads it when they
    #: add the row. `len(OUTPUTS)` cannot drift.
    return ('没有 DISPLAY：x11grab 只认 X11 会话（Wayland 下 ffmpeg 无法截屏，'
            '本插件的{}种目标都收不到画面；请切到 XWayland/X11 会话）').format(
                len(OUTPUTS))


#: How long the encoder may stay silent before we call the capture dead. A
#: denied input exits inside a second; this is the slower half of the same
#: failure -- a session that opens and never delivers a frame.
NO_FRAME_SECONDS = 3.0

#: Windows needs longer, and not by a little. gdigrab opens the desktop first
#: and the dshow loopback input is opened *after* it, so the pair takes seconds
#: to reach its first byte. Measured on a real Windows 11 box (AMD-YES, ffmpeg
#: 8.1.2), the combined pipeline printed both of these at the 3 s mark:
#:
#:   [in#0/gdigrab @ ...] Stream #0: not enough frames to estimate rate;
#:                        consider increasing probesize
#:   [aist#1:0/pcm_s16le @ ...] Guessed Channel Layout: stereo
#:
#: -- a slow start and a *working* sound device, and this budget killed them.
#: The video-only retry then came up 1.2 s later, which is exactly how a
#: machine with a working Stereo Mix reported「没有系统声音」: the verdict was
#: ours, not ffmpeg's. Nothing here is macOS-derived any more.
NO_FRAME_SECONDS_WIN32 = 8.0


def no_frame_budget(platform=None):
    """Seconds to wait for the first byte of encoder output on this platform."""
    return (NO_FRAME_SECONDS_WIN32 if (platform or sys.platform) == 'win32'
            else NO_FRAME_SECONDS)

#: Lines that show up on *working* captures too, so quoting them as the reason
#: for a failure would be a guess. Measured on this machine: a 6-second grab
#: that wrote 3.9 MB of H.264 printed both of these.
FFMPEG_NOISE = re.compile(
    r"^(objc\[\d+\]: "                             # Apple runtime chatter
    r"|\[in#0/avfoundation @ \w+\] Stream #0: not enough frames"
    r"|\[AVFoundation indev @ \w+\] Configuration of video device failed)")

#: macOS 的授权改动只对新启动的进程生效，而 ffmpeg 是我们 spawn 的子进程：
#: 不重启 Macast，勾了也没有用。每一句"没有画面"都必须说到这一步。
PERMISSION_DOOR = '请在「系统设置 → 隐私与安全性 → 屏幕录制」中允许 Macast'

#: avfoundation 把系统声音当作**输入**设备，所以它吃的是麦克风授权，不是屏幕录制
#: —— 两道门，两个后果。降级成功的那一句要说出少了什么、去哪补。
MICROPHONE_DOOR = '请在「系统设置 → 隐私与安全性 → 麦克风」中允许 Macast'

#: Linux 的声音口同样不是我们装的：`<sink>.monitor` 一直在那儿，只要 `pactl`
#: 问得出默认输出设备。所以这一句必须说出缺的是哪个命令、去哪确认，而不是
#: 只报一个「需要 PulseAudio」—— 那是 §4.2「每一条失败必须给出门」的同一条。
PULSE_DOOR = ('系统声音：未启用 —— Linux 要把系统声音录回来，需要 '
              'PulseAudio/PipeWire 的 monitor 源：`pactl get-default-sink` 得问得出'
              '一块默认输出设备（没有 pactl 就装 pulseaudio-utils，PipeWire 也提供它）。'
              '确认系统音量里有输出设备后，点上面的「重新探测采集」，本次镜像要重启一次。')

AUDIO_DROPPED_SUFFIX = (
    '（本次镜像没有系统声音：带上它就一直取不到帧，已改为只采集画面。'
    '要声音请{}，勾选后重启 Macast）'.format(MICROPHONE_DOOR))

#: 状态行要短：门的全名在通知与「系统声音」那一行里。
AUDIO_DROPPED_MARK = '· 本次无系统声音（麦克风未授权）'

#: The same two sentences for Windows, where the tap is lost for a different
#: reason: there is no microphone grant to give, the loopback device is either
#: busy, disabled, or too slow to clock the pipeline. Sending a Windows user to
#: 「隐私与安全性 → 麦克风」is a dead end -- and it is what the first Windows run
#: actually said.
AUDIO_DROPPED_WIN32_SUFFIX = (
    '（本次镜像没有系统声音：回环录音设备没能及时出帧，已改为只采集画面。'
    'Windows 上多半是「立体声混音」被别的程序独占、或者它被停用了；'
    '确认它可用后点「重新探测采集」，镜像会重启一次）')

AUDIO_DROPPED_WIN32_MARK = '· 本次无系统声音（回环设备未出帧）'

#: ScreenCaptureKit's tap dies for a third reason, and the two doors above
#: are wrong for it: there is no microphone grant behind an SCK audio tap,
#: and BlackHole cannot feed one either. The failure latches for the whole
#: run, so the only honest retry is the restart that clears the latch --
#:「重启镜像」would promise a second audio attempt this run has already
#: decided to skip (`_mirror` reads `_audio_refused`, not this sentence).
AUDIO_DROPPED_SCK_SUFFIX = (
    '（本次镜像没有系统声音：ScreenCaptureKit 一直没有送来系统音频，'
    '已改为只采集画面。本次运行里之后的镜像也会先跳过声音；'
    '要再试一次，请重启 Macast）')

AUDIO_DROPPED_SCK_MARK = '· 本次无系统声音（ScreenCaptureKit 未送出音频）'


def audio_dropped_suffix(platform=None, method=None):
    """The long sentence for a session that gave up its system audio.

    `method` outranks `platform`: the SCK tap is not a third platform, it
    is a third failure, and only the capture that ran knows which one it
    was. `platform` stays positional for the existing callers.
    """
    if method == 'sck':
        return AUDIO_DROPPED_SCK_SUFFIX
    return (AUDIO_DROPPED_WIN32_SUFFIX if (platform or sys.platform) == 'win32'
            else AUDIO_DROPPED_SUFFIX)


def audio_dropped_mark(platform=None, method=None):
    """The short one for the status line (same precedence as the suffix)."""
    if method == 'sck':
        return AUDIO_DROPPED_SCK_MARK
    return (AUDIO_DROPPED_WIN32_MARK if (platform or sys.platform) == 'win32'
            else AUDIO_DROPPED_MARK)


def no_frame_words(detail='', platform=None):
    """The sentence for a capture that opened and never produced a frame.

    `detail` is ffmpeg's own last words, and it is kept as an appendix only:
    the denied-input case prints objc registration noise that means nothing,
    and a message that leads with it leaves the user without a door to knock
    on -- which is the one thing this branch exists to provide.
    """
    text = '屏幕采集在 {:g} 秒内没有返回画面'.format(no_frame_budget(platform))
    if detail:
        text += '（ffmpeg：{}）'.format(detail)
    if (platform or sys.platform) == 'darwin':
        text += '；{}，勾选后重启 Macast'.format(PERMISSION_DOOR)
    return text


# -- macOS 系统声音辅助：一键安装 BlackHole + 多输出设备 -----------------------
#
# A .pkg with a kernel-adjacent audio driver cannot be installed silently from
# an app -- installer.app always asks for the password. The most a "one click"
# can honestly do is: download the official pkg (verified against the sha256
# Homebrew's cask API publishes), open the GUI installer, poll until the
# device shows up in ffmpeg's list, then create the multi-output aggregate
# device and switch default output to it -- both via CoreAudio APIs, no
# clicking in Audio MIDI Setup. Any step that fails degrades to opening that
# pane with instructions.

BLACKHOLE_CASK_API = 'https://formulae.brew.sh/api/cask/blackhole-2ch.json'
#: Fallback if the cask API is unreachable (the GFW eats formulae.brew.sh
#: sometimes). Redownload only makes sense after bumping this pin.
BLACKHOLE_PKG_URL = 'https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg'
BLACKHOLE_PKG_SHA256 = ('57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef'
                        '9dccf851400dcf')
MACAST_AGGREGATE_UID = 'com.macast.screenmirror.output'
MACAST_AGGREGATE_NAME = 'Macast Screen Mirror'
#: kAudioObjectSystemObject == 1. (0x1000 is the hardware *model* object;
#: asking it for the device list answers 'nope'/kAudioHardwareUnknownPropertyError.)
SYSTEM_OBJECT = 1

#: Set while the assisted install runs; an Event because a plain bool would
#: need `global` in two places and menu clicks race on the UI thread.
_audio_setup_busy = threading.Event()


def blackhole_pkg_source(meta_json):
    """(url, sha256) from cask API bytes; the pinned pair if unusable."""
    try:
        data = json.loads(meta_json.decode('utf-8', 'replace'))
        url = data['url']
        sha = data['sha256']
        if (url.lower().endswith('.pkg') and isinstance(sha, str)
                and re.fullmatch(r'[0-9a-fA-F]{64}', sha)):
            return url, sha.lower()
    except Exception:
        pass
    return BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256


def _coreaudio():
    import ctypes
    ca = ctypes.CDLL('/System/Library/Frameworks/CoreAudio.framework'
                     '/CoreAudio')

    class Addr(ctypes.Structure):
        _fields_ = [('mSelector', ctypes.c_uint32),
                    ('mScope', ctypes.c_uint32),
                    ('mElement', ctypes.c_uint32)]

    # Declare every signature: an un-prototyped call in this build was enough
    # to make the framework reject correct addresses ('nope'/'!dat').
    ca.AudioObjectGetPropertyData.restype = ctypes.c_int               # OSStatus
    ca.AudioObjectGetPropertyData.argtypes = [
        ctypes.c_uint32, ctypes.POINTER(Addr), ctypes.c_uint32,
        ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32), ctypes.c_void_p]
    ca.AudioObjectSetPropertyData.restype = ctypes.c_int
    ca.AudioObjectSetPropertyData.argtypes = [
        ctypes.c_uint32, ctypes.POINTER(Addr), ctypes.c_uint32,
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    ca.AudioHardwareCreateAggregateDevice.restype = ctypes.c_int
    ca.AudioHardwareCreateAggregateDevice.argtypes = [ctypes.c_void_p,
                                                      ctypes.POINTER(ctypes.c_uint32)]
    ca._macast_addr_type = Addr
    return ca


def _fourcc(chars):
    """FourCC as the big-endian OSType value CoreAudio expects (no Carbon
    helper is worth pulling in for this)."""
    return int.from_bytes(chars, 'big')


def _address(ca, selector_chars, scope=b'glob', element=0):
    """An AudioObjectPropertyAddress; scope defaults to global."""
    Addr = ca._macast_addr_type
    return Addr(_fourcc(selector_chars), _fourcc(scope), element)


def _cf_to_str(pointer):
    """A CFStringRef returned by CoreAudio -> Python str (via pyobjc bridging).

    The +1 retain from the Get rule is deliberately leaked: the bridged
    NSString has no owner otherwise.
    """
    if not pointer:
        return None
    from objc import objc_object
    return str(objc_object(c_void_p=int(pointer)))


def _audio_devices():
    """[(device id, uid)] from kAudioHardwarePropertyDevices (all devices).

    Asks for the size with a real buffer: this framework build rejects the
    documented NULL-data size probe with '!dat'.
    """
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'devs')
    max_devices = 256
    buffer = (ctypes.c_uint32 * max_devices)()
    size = ctypes.c_uint32(max_devices * 4)
    if ca.AudioObjectGetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr), 0,
                                     None, ctypes.byref(size), buffer) != 0:
        return []
    uid_addr = _address(ca, b'deui')
    result = []
    for raw_id in buffer[:size.value // 4]:
        device_id = raw_id.value
        try:
            value = ctypes.c_void_p(0)
            uid_size = ctypes.c_uint32(8)
            if ca.AudioObjectGetPropertyData(device_id, ctypes.byref(uid_addr),
                                             0, None, ctypes.byref(uid_size),
                                             ctypes.byref(value)) != 0:
                continue
            uid = _cf_to_str(value.value)
            if uid:
                result.append((device_id, uid))
        except Exception:
            continue
    return result


def _default_output():
    """CoreAudio id of the current default output device, or None."""
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'dOut')
    size = ctypes.c_uint32(4)
    out = ctypes.c_uint32(0)
    if ca.AudioObjectGetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr), 0,
                                     None, ctypes.byref(size),
                                     ctypes.byref(out)) != 0:
        return None
    return out.value or None


def _set_default_output(device_id):
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'dOut')
    value = ctypes.c_uint32(int(device_id))
    return ca.AudioObjectSetPropertyData(SYSTEM_OBJECT, ctypes.byref(addr),
                                         0, None, ctypes.sizeof(value),
                                         ctypes.byref(value)) == 0


def _create_aggregate(uids):
    """AudioHardwareCreateAggregateDevice; returns the new device id or None."""
    import ctypes
    try:
        from Foundation import NSMutableDictionary
    except ImportError:
        logger.error('pyobjc is unavailable: cannot create the aggregate device')
        return None
    ca = _coreaudio()
    sub_devices = []
    for uid in uids:
        one = NSMutableDictionary.dictionary()
        one['uid'] = uid
        one['isPrivate'] = False
        sub_devices.append(one)
    props = NSMutableDictionary.dictionary()
    props['name'] = MACAST_AGGREGATE_NAME
    props['uid'] = MACAST_AGGREGATE_UID
    props['deviceList'] = sub_devices
    # stacked=1 is the「多输出设备」checkbox: this device mixes to both sinks.
    props['stacked'] = True
    out = ctypes.c_uint32(0)
    status = ca.AudioHardwareCreateAggregateDevice(
        ctypes.c_void_p(props.pyobjc_id()), ctypes.byref(out))
    if status != 0:
        logger.error('AudioHardwareCreateAggregateDevice failed: %d', status)
        return None
    return out.value


def _device_uid(device_id):
    """One device's UID, or '' when this machine will not say."""
    import ctypes
    ca = _coreaudio()
    addr = _address(ca, b'deui')
    value = ctypes.c_void_p(0)
    size = ctypes.c_uint32(8)
    if ca.AudioObjectGetPropertyData(int(device_id), ctypes.byref(addr), 0,
                                     None, ctypes.byref(size),
                                     ctypes.byref(value)) != 0:
        return ''
    return _cf_to_str(value.value) or ''


def system_audio_routed():
    """True / False / None: does this machine's own sound reach the capture tap?

    BlackHole is a wire, not a splitter: it carries what is *sent* to it, which
    on macOS means the default output has to be the multi-output device the
    assisted install builds (or one the user made by hand). With the speakers
    still being the default output, mirroring captures a device nobody writes
    to -- silence, while the console cheerfully says「系统声音：已启用」. That is
    the difference this answers.

    None means "asked and could not tell": per-device property reads are
    refused in some sandboxes, and a shrug is the only honest answer there.
    """
    if sys.platform != 'darwin':
        return None
    try:
        out = _default_output()
        if out is None:
            return None
        uid = _device_uid(out)
    except Exception:
        return None
    if not uid:
        return None
    return uid == MACAST_AGGREGATE_UID or 'blackhole' in uid.lower()


def _device_uids(ffmpeg):
    """avfoundation names for both kinds; a freshly installed BlackHole shows
    up in the video *and* audio lists, so screen detection must not trust only
    one of them."""
    videos, audios = _avfoundation_lists(ffmpeg)
    return [n for n in videos if 'capture screen' not in n.lower()] + audios


def _find_blackhole(ffmpeg):
    for device_id, uid in _audio_devices():
        if 'blackhole' in uid.lower():
            return device_id, uid
    return None


def _has_blackhole(ffmpeg):
    return any('blackhole' in name.lower() for name in _device_uids(ffmpeg))


#: The five answers the assisted install can get, strongest first. Each one
#: takes a different branch, so the whole flow keys off this one value.
BH_STATES = ('capturable', 'loaded', 'on-disk', 'stale-receipt', 'absent')


def _blackhole_state(ffmpeg):
    """Where BlackHole is visible right now, as one of ``BH_STATES``.

    Three of these branches exist because the two witnesses disagree, and every
    one of them used to take the same action (download the pkg again):
    ``capturable`` -- ffmpeg's own device list, the only one that proves audio
        will actually be captured;
    ``loaded`` -- CoreAudio has the device but avfoundation does not, so the
        driver is alive and the blocker is the microphone permission;
    ``on-disk`` -- HAL driver files landed but the daemon never loaded them,
        which is what a reinstall can never fix;
    ``stale-receipt`` / ``absent`` -- the two cases an installer is for.
    """
    try:
        if _has_blackhole(ffmpeg):
            return 'capturable'
    except Exception as e:
        logger.info('avfoundation blackhole probe failed: %s', e)
    try:
        if _find_blackhole(ffmpeg) is not None:
            return 'loaded'
    except Exception as e:
        logger.info('CoreAudio blackhole probe failed: %s', e)
    if _blackhole_driver_installed():
        return 'on-disk'
    if _blackhole_receipt_present():
        return 'stale-receipt'
    return 'absent'


def blackhole_pkg_pair():
    """(url, sha256, source) — the cask API first, the pinned pair as fallback.

    The source string belongs on the progress page: "为什么下的是这个版本"
    deserves an answer when the answer is "拉不到 Homebrew 的公告，用了内置的".
    """
    import requests
    try:
        meta = requests.get(BLACKHOLE_CASK_API, timeout=10).content
    except Exception as e:
        logger.info('cask API unavailable (%s), using the pinned pkg', e)
        return BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256, '内置钉住版本（cask API 不可达）'
    url, sha = blackhole_pkg_source(meta)
    if (url, sha) == (BLACKHOLE_PKG_URL, BLACKHOLE_PKG_SHA256):
        return url, sha, 'Homebrew cask API（与钉住版本一致）'
    return url, sha, 'Homebrew cask API'


def _pkg_tmpdir():
    """A writable temp dir. The plain mkdtemp fails with ENOENT when the app
    inherited a TMPDIR pointing at a since-deleted CLI-sandbox directory
    (launching the .app binary from a tool shell does exactly that)."""
    import tempfile
    try:
        return tempfile.mkdtemp(prefix='macast-blackhole-')
    except OSError:
        fallback = os.path.join(os.path.expanduser('~/Library/Caches/Macast'),
                                'blackhole-tmp')
        os.makedirs(fallback, exist_ok=True)
        return tempfile.mkdtemp(prefix='macast-blackhole-', dir=fallback)


#: existential.audio answers 406 Not Acceptable to the default python-requests
#: User-Agent (verified: same URL, browser UA -> 200). The pkg itself is a
#: public download; presenting as a browser is the only way in.
PKG_USER_AGENT = ('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
                  'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 '
                  'Safari/537.36')


def _fetch_blackhole_pkg(url, on_bytes=None):
    """Stream the official pkg to a temp file; return its path.

    `on_bytes(done, total)` exists for the progress page -- a several-MB
    download over a slow link looked identical to a hang without it.
    """
    import requests
    path = os.path.join(_pkg_tmpdir(), 'BlackHole2ch.pkg')
    try:
        with requests.get(url, stream=True, timeout=60,
                          headers={'User-Agent': PKG_USER_AGENT}) as response:
            response.raise_for_status()
            total = int(response.headers.get('Content-Length') or 0)
            done = 0
            with open(path, 'wb') as handle:
                for chunk in response.iter_content(65536):
                    handle.write(chunk)
                    done += len(chunk)
                    if on_bytes is not None:
                        on_bytes(done, total)
    except Exception:
        _remove_quietly(path)
        raise
    return path


def verify_blackhole_pkg(path, expected_sha):
    """True when the file hashes to what Homebrew publishes for it.

    A mismatch deletes the file (the caller treats False as fatal): an
    unverified audio driver must never reach `open`.
    """
    import hashlib
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(65536), b''):
            digest.update(block)
    if digest.hexdigest() != expected_sha:
        _remove_quietly(path)
        return False
    return True


HAL_DRIVER_GLOBS = ('/Library/Audio/Plug-Ins/HAL/BlackHole*.driver',)
BLACKHOLE_PKG_IDS = ('audio.existential.BlackHole2ch',
                     'audio.existential.BlackHole16ch')


def _blackhole_driver_installed():
    """Driver *files* on disk -- weaker than "the device exists", and that
    gap is the whole story of one real failure mode: pkgutil keeps a receipt
    after the files are gone (so a reinstall is needed), and the official pkg
    does not restart coreaudiod after installing (so files can sit there
    forever without the daemon ever loading them)."""
    import glob
    return any(glob.glob(pattern) for pattern in HAL_DRIVER_GLOBS)


def _blackhole_receipt_present():
    try:
        res = subprocess.run(['pkgutil', '--pkgs'], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, timeout=15)
    except Exception:
        return False
    installed = set(res.stdout.split())
    return any(pkg in installed for pkg in BLACKHOLE_PKG_IDS)


def _reload_coreaudiod():
    """(ok, why). Restart the audio daemon with an admin prompt so an
    installed-but-unloaded HAL driver gets picked up.

    Deliberately its own password round-trip rather than silent sabotage:
    killing coreaudiod drops everyone's audio for a second, and macOS gives
    no way to do it without authorization. The pkg installer already asked
    for a password minutes earlier, so this prompt is expected, not a surprise.
    """
    script = ('do shell script "/bin/kill -TERM $(/usr/bin/pgrep -x coreaudiod)" '
              'with administrator privileges '
              'with prompt "Macast 需要重载音频服务，让刚安装的 BlackHole 驱动生效"')
    try:
        res = subprocess.run(['osascript', '-e', script],
                             stdout=subprocess.DEVNULL,
                             stderr=subprocess.PIPE, text=True, timeout=180)
    except Exception as e:
        return False, '无法执行重载：{}'.format(e)
    if res.returncode == 0:
        return True, '音频服务已重载'
    err = (res.stderr or '').strip()
    if '-128' in err:
        return False, '你取消了密码框'
    return False, err or '重载失败'


def _remove_quietly(path):
    try:
        os.remove(path)
    except OSError:
        pass


def _wait_for_blackhole(ffmpeg, timeout=300.0, progress=None, interval=5.0,
                        step='wait', note='请在安装器里点「安装」并输入密码'):
    """Poll until a witness sees the device, and report *which* one did.

    'capturable' / 'loaded' / '' (timeout). Keeping the two apart is the whole
    point: 'loaded' means the daemon picked the driver up but capture still
    cannot open it, and calling that a success would be a lie of exactly the
    kind this flow used to tell.

    The ticking message matters: this is the step where the user is supposed
    to be clicking through installer.app, and a silent 5 minutes reads exactly
    like a hang.
    """
    def _seen():
        state = _blackhole_state(ffmpeg)
        return state if state in ('capturable', 'loaded') else ''

    deadline = time.time() + timeout
    while time.time() < deadline:
        state = _seen()
        if state:
            return state
        time.sleep(min(interval, max(deadline - time.time(), 0.1)))
        if progress is not None:
            waited = timeout - max(deadline - time.time(), 0.0)
            progress.sub(step, min(waited / timeout, 0.99),
                         '已等 {:.0f} 秒：{}'.format(waited, note))
    return _seen()


def _reload_until_visible(ffmpeg, progress):
    """One admin password, then wait for the daemon to load the driver.

    Returns ``(state, why)`` with state in ('capturable', 'loaded', ''). The
    caller only needs the state to decide whether to keep going, but the user
    needs `why`: "you cancelled the password box" and "it reloaded and the
    device still isn't there" are different problems with different fixes, and
    conflating them is how this flow ended up blaming the user's reboot.
    """
    progress.enter('reload', '需要一次管理员密码')
    reloaded, why = _reload_coreaudiod()
    if not reloaded:
        progress.fail('reload', why)
        return '', why
    state = _wait_for_blackhole(ffmpeg, timeout=45.0, progress=progress,
                                interval=3.0, step='reload',
                                note='音频服务正在重启，等它把驱动读进来')
    if state:
        progress.leave('reload', '{}，设备已出现'.format(why))
    else:
        progress.fail('reload', '重载后仍然看不到 BlackHole 设备')
        state = ''
        why = '重载音频服务后仍然看不到 BlackHole 设备'
    return state, why


def _route_audio_through_blackhole(ffmpeg, progress=None):
    """Create/reuse the multi-output aggregate and make it the default output.

    Without it, installing BlackHole and selecting it as the output means the
    user hears nothing; the aggregate feeds BlackHole *and* the speakers.
    Returns True on success, False if the caller should show manual steps.
    Each False path fails the exact step that gave up, so the page never says
    "创建设备失败" when the device was created and the switch is what broke.
    """
    if progress is None:
        progress = _NullProgress()
    progress.enter('aggregate')
    try:
        devices = _audio_devices()
    except Exception as e:
        logger.error('CoreAudio query failed: %s', e)
        progress.fail('aggregate', 'CoreAudio 查询失败：{}'.format(e))
        return False
    if not devices:
        progress.fail('aggregate', 'CoreAudio 没有返回任何设备')
        return False
    blackhole = _find_blackhole(ffmpeg)
    aggregate_id = Setting.get(SettingProperty.Mirror_Audio_Aggregate, '') or ''
    for device_id, uid in devices:
        if uid == MACAST_AGGREGATE_UID:
            aggregate_id = device_id
            break
    if aggregate_id:
        for device_id, _uid in devices:
            if device_id == int(aggregate_id):
                progress.leave('aggregate', '复用已有聚合设备')
                progress.enter('output')
                if not _set_default_output_and_remember(device_id):
                    progress.fail('output', '切换到默认输出失败')
                    return False
                return True
    if blackhole is None:
        # Installed but CoreAudio has not surfaced it by UID yet.
        progress.fail('aggregate', 'BlackHole 尚未在 CoreAudio 设备表中出现')
        return False
    speakers = _default_output()
    if speakers is None:
        speakers = next((device_id for device_id, uid in devices
                         if 'blackhole' not in uid.lower()), None)
    if speakers is None or speakers == blackhole[0]:
        progress.fail('aggregate', '找不到可配对的扬声器输出')
        return False
    created = _create_aggregate([blackhole[1], dict(devices)[speakers]])
    if created is None:
        progress.fail('aggregate', '系统拒绝创建多输出聚合设备')
        return False
    progress.leave('aggregate', '已创建多输出设备')
    progress.enter('output')
    if not _set_default_output_and_remember(created):
        progress.fail('output', '切换到默认输出失败')
        return False
    return True


def _set_default_output_and_remember(device_id):
    if not Setting.has(SettingProperty.Mirror_Audio_Original):
        current = _default_output()
        if current is not None:
            Setting.set(SettingProperty.Mirror_Audio_Original, current)
    if not _set_default_output(device_id):
        return False
    Setting.set(SettingProperty.Mirror_Audio_Aggregate, device_id)
    return True


def _open_audio_midi_setup(report):
    report('降级为手动：已打开「音频 MIDI 设置」，请创建多输出设备并勾选 BlackHole + 你的扬声器')
    try:
        subprocess.run(['open', '-a', 'Audio MIDI Setup'],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=10)
    except Exception:
        pass


#: The assisted-install run plan, in order. 'reload' only runs when the driver
#: files landed but coreaudiod never picked them up -- the pkg's own
#: postinstall only chmods, and a driver installed into a running session can
#: stay invisible to every enumeration until the daemon restarts.
AUDIO_STEPS = (
    ('env', '环境自检'),
    ('probe', '检测 BlackHole 状态'),
    ('meta', '获取官方安装包信息'),
    ('download', '下载安装包'),
    ('verify', '校验 sha256'),
    ('install', '打开安装器'),
    ('wait', '等待设备安装完成'),
    ('reload', '重载音频服务'),
    ('aggregate', '创建/复用多输出设备'),
    ('output', '切换默认输出'),
)


class _NullProgress(object):
    """The no-op twin of _SetupProgress, for callers without a page.

    Same surface, so setup_system_audio never has to ask which one it got --
    including the except-path sweep over snapshot()['steps']."""

    def enter(self, step_id, note=''):
        pass

    def sub(self, step_id, frac, note=None):
        pass

    def leave(self, step_id, note=''):
        pass

    def skip(self, step_id, note=''):
        pass

    def fail(self, step_id, note=''):
        pass

    def finish(self, ok, message=None):
        pass

    def snapshot(self):
        return {'pct': 0.0, 'message': '', 'done': False, 'ok': None,
                'steps': []}


class _SetupProgress(object):
    """Step states + overall percent for the assisted install.

    One object, read from the HTTP handler thread and written from the worker;
    a plain lock around the dict copy keeps snapshots coherent without dragging
    in a framework. Steps already finished refuse re-entry -- a flow that
    re-opened a done step would make the bar run backwards.
    """

    def __init__(self, steps=AUDIO_STEPS):
        self._lock = threading.Lock()
        self._steps = [{'id': sid, 'label': label, 'state': 'pending',
                        'pct': None, 'note': ''} for sid, label in steps]
        self.message = ''
        self.done = False
        self.ok = None

    def _get(self, step_id):
        for st in self._steps:
            if st['id'] == step_id:
                return st
        return None

    def enter(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] in ('done', 'fail', 'skipped'):
                return
            st['state'] = 'running'
            st['note'] = note
            self.message = '{}：{}'.format(st['label'], note) if note else st['label']

    def sub(self, step_id, frac, note=None):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] != 'running':
                return
            st['pct'] = frac
            if note is not None:
                st['note'] = note
                self.message = '{}：{}'.format(st['label'], note)

    def leave(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] == 'fail':
                return
            st['state'] = 'done'
            st['pct'] = 1.0
            if note:
                st['note'] = note

    def skip(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None or st['state'] in ('done', 'fail'):
                return
            st['state'] = 'skipped'
            st['pct'] = 1.0
            if note:
                st['note'] = note

    def fail(self, step_id, note=''):
        with self._lock:
            st = self._get(step_id)
            if st is None:
                return
            st['state'] = 'fail'
            if note:
                st['note'] = note
                self.message = '{}失败：{}'.format(st['label'], note)

    def overall(self):
        weighted = {'done': 1.0, 'skipped': 1.0, 'fail': 1.0,
                    'running': None, 'pending': 0.0}
        total = 0.0
        count = 0
        for st in self._steps:
            if st['state'] == 'skipped':
                continue
            count += 1
            frac = weighted[st['state']]
            if frac is None:
                frac = st['pct'] if st['pct'] is not None else 0.15
            total += frac
        return total / count if count else 0.0

    def finish(self, ok, message=None):
        """Close the run, and -- when there is one -- make the *answer* the
        sentence the page reads.

        Without that second argument the card is left showing the last step's
        label (「确认 aiortc 与 av 导入成功」) while the real news
        (「已移除…，原来的文件在 .trash/…」) lives only in a toast that
        flashes past. §4.8 同一条: 这句话必须活在页面上而不只是一次性通知.
        """
        with self._lock:
            self.done = True
            self.ok = bool(ok)
            if message:
                self.message = message

    def snapshot(self):
        with self._lock:
            return {
                'pct': self.overall(),
                'message': self.message,
                'done': self.done,
                'ok': self.ok,
                'steps': [dict(st) for st in self._steps],
            }


AUDIO_PROGRESS_PAGE = """<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Macast · 系统声音一键设置进度</title>
<style>
 body{font-family:-apple-system,system-ui,sans-serif;background:#141821;color:#e8ecf3;
      margin:0;padding:28px 20px;}
 .wrap{max-width:640px;margin:0 auto;}
 h1{font-size:18px;font-weight:600;margin:0 0 14px;}
 .bar{height:10px;background:#232a38;border-radius:6px;overflow:hidden;margin:0 0 6px;}
 .fill{height:100%;width:0;background:#3b82f6;transition:width .5s;}
 #pct{font-size:12px;color:#93a0b5;margin:0 0 4px;}
 #msg{font-size:13px;color:#c8d2e0;min-height:20px;margin:8px 0 18px;}
 ul{list-style:none;padding:0;margin:0;}
 li{display:flex;gap:10px;align-items:baseline;padding:7px 0;
    border-bottom:1px solid #1d2431;font-size:14px;}
 li .sym{width:1.2em;flex:none;text-align:center;}
 li.run .sym{color:#3b82f6;} li.ok .sym{color:#22c55e;}
 li.bad .sym{color:#ef4444;} li.off .sym,li.pend{color:#5b6779;}
 li .label{font-weight:600;flex:none;}
 li .note{color:#93a0b5;font-size:12px;}
 .foot{color:#5b6779;font-size:12px;margin-top:16px;}
</style></head><body><div class="wrap">
<h1>系统声音一键设置（BlackHole + 多输出设备）</h1>
<div class="bar"><div class="fill" id="fill"></div></div>
<p id="pct">0%</p>
<p id="msg">正在连接…</p>
<ul id="steps"></ul>
<p class="foot">本页面只由 Macast 在本机提供，设置结束后会自动失效；此窗口可以直接关闭。</p>
</div>
<script>
var TOKEN = '@TOKEN@';
var last = null;
function sym(st){
  return {running:'▶', done:'✓', fail:'✗', skipped:'—', pending:'○'}[st] || '?';
}
function cls(st){
  return {running:'run', done:'ok', fail:'bad', skipped:'off pend', pending:'pend'}[st] || '';
}
function render(s){
  document.getElementById('fill').style.width = Math.round(s.pct*100) + '%';
  document.getElementById('pct').textContent = Math.round(s.pct*100) + '%';
  document.getElementById('msg').textContent =
      s.done ? (s.ok ? '全部完成：开始镜像后系统声音会一起投出去。' : '未完成，请见上方红色步骤。')
             : (s.message || '进行中…');
  var list = document.getElementById('steps');
  list.textContent = '';
  s.steps.forEach(function(st){
    var li = document.createElement('li');
    li.className = cls(st.state);
    var a = document.createElement('span'); a.className = 'sym';
    a.textContent = sym(st.state);
    var b = document.createElement('span'); b.className = 'label';
    b.textContent = st.label;
    li.appendChild(a); li.appendChild(b);
    if (st.note){
      var c = document.createElement('span'); c.className = 'note';
      c.textContent = st.note;
      li.appendChild(c);
    }
    list.appendChild(li);
  });
  last = s;
}
function tick(){
  fetch('/state?token=' + encodeURIComponent(TOKEN))
    .then(function(r){ return r.json(); })
    .then(render)
    .catch(function(){ document.getElementById('msg').textContent = '进度服务已关闭（设置已结束）。'; })
    .then(function(){ setTimeout(tick, last && last.done ? 5000 : 1000); });
}
tick();
</script></body></html>
"""


class _AudioProgressHandler(BaseHTTPRequestHandler):
    """Loopback-only viewer for one running assisted install.

    Same credential shape as the mirror pages (AGENTS.md 4.8): a per-run
    random token in the URL, never the app's stable Api_Token; every value
    the page shows goes through textContent; the body is ours end to end, so
    nothing user- or network-controlled is interpolated.
    """

    def _authorized(self):
        import hmac
        import urllib.parse
        query = urllib.parse.parse_qs(self.path.partition('?')[2] or '')
        token = (query.get('token') or [''])[0]
        return bool(token) and hmac.compare_digest(token, self.server.token)

    def do_GET(self):
        if not self._authorized():
            self.send_error(403, 'a token is required')
            return
        path = self.path.partition('?')[0]
        if path == '/state':
            body = json.dumps(self.server.progress.snapshot()).encode('utf-8')
            ctype = 'application/json'
        elif path in ('/', '/index.html'):
            body = AUDIO_PROGRESS_PAGE.replace(
                '@TOKEN@', self.server.token).encode('utf-8')
            ctype = 'text/html; charset=utf-8'
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET() if self.path.partition('?')[0] == '/state' \
            else self.send_error(405)

    def log_message(self, fmt, *args):
        pass


#: Kept between runs so a second click can replace the previous page's server
#: instead of stacking ports.
_audio_progress = None
#: The loopback URL of the assisted-install progress page, kept so the console
#: can offer it again after the notification that announced it scrolled away.
_audio_progress_url = ''
_audio_server = None
_audio_server_timer = None


def _open_audio_progress(progress):
    """Start the loopback progress page and open it; '' when unavailable.

    A failure here must not fail the install -- the notifications still carry
    every step, the page is the comfortable view, not the only one.
    """
    server = ThreadingHTTPServer(('127.0.0.1', 0), _AudioProgressHandler)
    server.progress = progress
    server.token = secrets.token_hex(8)
    server.daemon_threads = True
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name='SCREEN_MIRROR_AUDIO_HTTP').start()
    url = 'http://127.0.0.1:{}{}?token={}'.format(
        server.server_address[1], '/', server.token)
    try:
        subprocess.run(['open', url], timeout=10)
    except Exception as e:
        logger.info('cannot open the progress page: %s', e)
    return url, server


def _close_audio_server():
    """Retire the progress page: its URL is only offered while it serves."""
    global _audio_server, _audio_server_timer, _audio_progress_url
    if _audio_server_timer is not None:
        _audio_server_timer.cancel()
        _audio_server_timer = None
    server, _audio_server = _audio_server, None
    _audio_progress_url = ''
    if server is not None:
        try:
            server.shutdown()
            server.server_close()
        except Exception:
            pass


def _schedule_audio_server_close(delay=300.0):
    """Leave the finished page readable, then stop serving."""
    global _audio_server_timer
    if _audio_server_timer is not None:
        _audio_server_timer.cancel()
    _audio_server_timer = threading.Timer(delay, _close_audio_server)
    _audio_server_timer.daemon = True
    _audio_server_timer.start()


def setup_system_audio(report=lambda message: None, progress=None):
    """One-click: install BlackHole if absent, then route audio through it.

    Runs in a worker thread -- it spawns ffmpeg, waits on the installer, and
    must never touch the UI thread. `progress` (a _SetupProgress) is the
    step-by-step account; everything still reports one-liners through `report`
    because the page can be closed and the notifications must stay sufficient.
    """
    if progress is None:
        progress = _NullProgress()
    progress.enter('env')
    if sys.platform != 'darwin':
        progress.fail('env', '辅助安装仅适用于 macOS')
        report('辅助安装仅适用于 macOS')
        return False
    ffmpeg = ffmpeg_requirement()
    if ffmpeg is None:
        progress.fail('env', '找不到 ffmpeg')
        report('找不到 ffmpeg：先 brew install ffmpeg')
        return False
    progress.leave('env', 'ffmpeg: {}'.format(ffmpeg))
    try:
        progress.enter('probe')
        state = _blackhole_state(ffmpeg)
        capturable = state == 'capturable'
        if state in ('capturable', 'loaded'):
            # The driver is loaded. Whatever else is wrong, downloading another
            # pkg cannot fix it -- that is the loop the user reported.
            progress.leave('probe', '设备已在，跳过安装' if capturable else
                           'CoreAudio 里已有 BlackHole，但 ffmpeg 的采集设备表读不到它：'
                           '不重装（重装改变不了这件事），先把多输出设备接好')
            for _inert in ('meta', 'download', 'verify', 'install', 'wait',
                           'reload'):
                progress.skip(_inert)
        elif state == 'on-disk':
            progress.leave('probe', '驱动文件在磁盘上但音频服务没有加载它（官方 .pkg 的 '
                                    'postinstall 只改权限、从不重启 coreaudiod）：'
                                    '跳过下载与安装，只做一次重载')
            for _inert in ('meta', 'download', 'verify', 'install', 'wait'):
                progress.skip(_inert)
            state, why = _reload_until_visible(ffmpeg, progress)
            if not state:
                report('一键设置没有完成：{}。驱动已经在磁盘上了，再点一次只会重试重载，'
                       '不会重新下载安装包；重启一次电脑也能达到同样效果'.format(why))
                return False
            capturable = state == 'capturable'
        else:
            progress.leave('probe', (
                '检测到残留的安装记录，但驱动文件已不在磁盘上：'
                '将重新下载官方安装包（这就是上次"装了却没有设备"的原因）'
                if state == 'stale-receipt' else
                '本机没有 BlackHole，将安装官方 2ch 驱动'))
            progress.enter('meta')
            url, expected, source = blackhole_pkg_pair()
            progress.leave('meta', '{}：{}'.format(source, url.rsplit('/', 1)[-1]))
            progress.enter('download')

            def _on_bytes(done, total):
                mb = '{} / {} MB'.format(done / 1048576.0, total / 1048576.0) \
                    if total else '{} KB'.format(done / 1024.0)
                progress.sub('download', (done / total) if total else None, mb)

            pkg = _fetch_blackhole_pkg(url, on_bytes=_on_bytes)
            progress.leave('download')
            progress.enter('verify')
            if not verify_blackhole_pkg(pkg, expected):
                progress.fail('verify', '下载到的文件与官方公布的 sha256 不一致，已删除')
                report('BlackHole 安装包校验失败（sha256 不匹配），已中止')
                return False
            progress.leave('verify', '哈希一致')
            progress.enter('install')
            subprocess.run(['open', pkg], timeout=10)
            progress.leave('install', '安装器已打开，请在弹窗里点「安装」并输入一次密码')
            report('正在打开安装器：请在弹窗里点「安装」并输入一次密码')
            progress.enter('wait')
            state = _wait_for_blackhole(ffmpeg, progress=progress)
            if not state and _blackhole_driver_installed():
                progress.sub('wait', None,
                             '驱动文件已就位但 CoreAudio 没加载它，尝试重载音频服务')
                state, _why = _reload_until_visible(ffmpeg, progress)
            if not state:
                progress.fail('wait', '超时仍未在设备列表里看到 BlackHole')
                report('没有检测到 BlackHole 设备：可能是安装被取消、密码未通过。'
                       '若驱动文件已经落盘，再点一次一键设置不会重新下载，'
                       '而是直接重试重载音频服务（或重启一次电脑）；'
                       '也可手动执行 brew install --cask blackhole-2ch')
                return False
            progress.leave('wait', '设备已出现')
            capturable = state == 'capturable'
        report('正在创建多输出设备并切换默认输出…')
        if not _route_audio_through_blackhole(ffmpeg, progress=progress):
            _open_audio_midi_setup(report)
            return False
        progress.leave('output')
        _capture_cache.clear()
        if capturable:
            report('系统声音已就绪：开始镜像后声音会一起投出去，本机照常能听到')
            return True
        # The aggregate is built and CoreAudio is happy, but the capture side
        # still cannot open the device. Re-marking the *finished* check as
        # failed is deliberate: this run's real outcome is that negative answer,
        # and a green page that leaves the user with no audio is the exact
        # "看起来装完了" illusion this whole branch exists to stop.
        progress.fail('probe', 'CoreAudio 已加载设备，但 ffmpeg 的采集设备表读不到它')
        report('多输出设备已建好，但采集侧仍然读不到 BlackHole，系统声音还不能用。'
               '这通常是 macOS 的麦克风权限：产物里的 Macast.app 若没有 '
               'NSMicrophoneUsageDescription，系统连授权弹窗都不会给。'
               '请在「系统设置 → 隐私与安全性 → 麦克风」里允许 Macast 并重启 Macast，'
               '再点一次一键设置（不会重新下载安装包）')
        return False
    except Exception as e:
        logger.error('blackhole assisted install failed: %s', e, exc_info=True)
        for st in progress.snapshot()['steps']:
            if st['state'] == 'running':
                progress.fail(st['id'], str(e))
                break
        report('一键设置失败：{}；可在「音频 MIDI 设置」里手动添加多输出设备'.format(e))
        return False


def _audio_setup_worker():
    """Worker for the menu item: run the assisted setup against a fresh step
    machine, serve the progress page on loopback while it runs, then clear the
    in-progress flag and schedule the page's retirement no matter how it ends.
    The page is a convenience -- if it cannot start, the install still runs
    and every step stays in the notifications."""
    global _audio_progress, _audio_server, _audio_progress_url

    def _report(message):
        notify(message)

    progress = _SetupProgress()
    _audio_progress = progress
    _close_audio_server()
    try:
        try:
            url, _audio_server = _open_audio_progress(progress)
            _audio_progress_url = url
            _report('已在本机打开详细进度页，可实时查看每一步：{}'.format(url))
        except Exception as e:
            logger.info('audio progress page unavailable: %s', e)
        ok = setup_system_audio(_report, progress=progress)
    except Exception as e:
        logger.error('audio setup worker crashed: %s', e, exc_info=True)
        ok = False
    finally:
        progress.finish(ok)
        _schedule_audio_server_close()
        _audio_setup_busy.clear()


def audio_setup_needed():
    """False only once a probe has actually produced system audio; the menu
    must not spawn ffmpeg to decide (it runs on the UI thread).

    An SCK capture with no audio map is the one exception to「没有 map 就是
    没有声音可采」: the run gives the tap up after it failed, and BlackHole
    cannot feed an SCK input -- offering the setup button there would walk
    the user through an installer that changes nothing.
    """
    capture = next(iter(_capture_cache.values()), None)
    if capture is None:
        return True
    return capture.audio_map is None and capture.method != 'sck'


def restore_system_audio(report=lambda message: None):
    """Point default output back at the speakers saved before routing."""
    original = Setting.get(SettingProperty.Mirror_Audio_Original, '') or ''
    if original == '':
        return False
    try:
        if _set_default_output(int(original)):
            report('已恢复原声音输出')
            return True
    except Exception as e:
        logger.error('cannot restore the audio output: %s', e)
    report('恢复失败：原输出设备可能已拔出')
    return False


# -- live stream HTTP server ------------------------------------------------
#
# One server, one encoder pipe, two consumers:
#   * a Chromecast LOADs the MPEG-TS URL we hand it;
#   * a browser opens /browser and feeds the fragmented-MP4 URL into MSE.
# Both URLs carry a per-session random id. That is deliberate: this port is
# reachable by every host on the LAN, a live mirror of someone's desktop is
# not something to hand to "anything that can open a socket", and a TV cannot
# present a token -- but it can be given a URL it invented nothing about.

#: H.264 NAL unit type that says "this sample needs no earlier picture":
#: 5 = IDR. The browser target is H.264 only -- `encoder_args` offers
#: `h264_videotoolbox` and `libx264` and nothing else, and no HEVC path exists
#: in this file -- so one type is the whole table. A future HEVC browser path
#: must add 19/20/21 here, and would notice by getting no replay at all
#: (`_Broadcaster.tail` returns empty rather than unplayable bytes), not by
#: getting a smear.
H264_IDR_NAL = 5


def _first_mdat(fragment):
    """Offset of the first `mdat` box in a fragment, or None.

    A fragment as `_Fragments` assembles it is a `moof` followed by the
    `mdat`(s) it describes, and its own length is in its first four bytes, so
    this walks boxes from there rather than assuming the `mdat` is adjacent --
    an `free`/`skip` box in between would otherwise make the keyframe test
    answer "no" for every fragment, which silently costs the replay rather
    than breaking anything loudly.
    """
    if len(fragment) < 8:
        return None
    offset = int.from_bytes(fragment[:4], 'big')
    if offset < 8:
        return None
    while offset + 8 <= len(fragment):
        size = int.from_bytes(fragment[offset:offset + 4], 'big')
        if size < 8:
            return None
        if fragment[offset + 4:offset + 8] == b'mdat':
            return offset
        offset += size
    return None


#: H.264 NAL types that are not the picture itself: SEI, SPS, PPS, AUD, filler.
#: A keyframe sample may carry any of them in front of its slice, so the walk
#: in `_sample_nal_types` steps over them; the first type outside this set is
#: the sample's first VCL NAL, and that one decides.
H264_LEADING_NAL_TYPES = frozenset((6, 7, 8, 9, 12))


def _sample_nal_types(fragment):
    """The H.264 NAL types up to the fragment's **first slice**.

    First sample's leading units and its first slice, deliberately not the
    whole fragment: the question is whether a decoder can start here, and a
    later sample's IDR (a fragment cut by `-frag_duration` can contain one)
    must not answer it. Samples are AVCC length-prefixed -- `-movflags` does
    not request Annex-B -- and a keyframe sample may carry `SPS`/`PPS`/`AUD`
    before its slice, so this collects the leading set rather than reading one
    byte. Anything unreadable or truncated stops the walk with what it has,
    which for a cut-off keyframe means "no" -- the direction that costs a
    replay, not a picture.
    """
    start = _first_mdat(fragment)
    if start is None:
        return frozenset()
    # A short `mdat` is a wrong box header by construction, and a replay must
    # not hand one over -- `flush` can produce exactly this shape when the
    # encoder dies mid-fragment. (0 and 1 are the "to the end" and 64-bit
    # size forms; a live `mdat` uses neither, so both mean "not a fragment".)
    size = int.from_bytes(fragment[start:start + 4], 'big')
    if size < 8 or start + size > len(fragment):
        return frozenset()
    types = set()
    at = start + 8
    while at + 4 <= len(fragment):
        size = int.from_bytes(fragment[at:at + 4], 'big')
        if size <= 0 or at + 4 + size > len(fragment):
            break
        nal_type = fragment[at + 4] & 0x1F
        types.add(nal_type)
        at += 4 + size
        if nal_type not in H264_LEADING_NAL_TYPES:
            break
    return frozenset(types)


def starts_with_keyframe(fragment):
    """Can a decoder begin at this fragment's first byte?

    Until 2026-10 the answer for the browser target was structural -- the
    muxer cut only on keyframes (`movflags=frag_keyframe`), so every fragment
    began with an IDR and nobody had to ask. Cutting per frame
    (`frag_every_frame`) is what took this path's latency from 1305 ms to
    838 ms on a real desktop, and it gives that guarantee up: eleven fragments
    in twelve now begin on a P-frame, and a viewer handed one of those decodes
    nothing until the next IDR -- 0.5 s of green smear.

    Conservative on purpose: anything unreadable answers False, which costs a
    replay (the viewer waits one GOP for the live stream to reach a keyframe)
    rather than costing a picture. A wrong True is the expensive direction.
    """
    return H264_IDR_NAL in _sample_nal_types(fragment)


class _Fragments(object):
    """Re-frame an encoder pipe into whole container fragments.

    stdout is read in fixed-size pieces, so a read boundary is almost never a
    box boundary. Both ways this server sheds load -- evicting from the replay
    ring and dropping from a slow viewer's queue -- remove *units*, and a unit
    that is half an `mdat` leaves the viewer holding a byte stream whose box
    headers are wrong from that byte on, permanently: MSE sits on a black frame
    while the byte counter keeps climbing. Grouping the bytes into `moof` plus
    the `mdat`(s) that follow it is what makes "drop whole blocks" true for this
    container. The price is that a fragment is sent as a whole, so the browser
    target lags the encoder by up to one fragment -- 41.7 ms since the muxer
    went to `frag_every_frame`, where this used to say "one GOP (measured:
    ~0.5 s)". That is most of where this target's latency went.

    Units carry a third field, "this fragment's first sample is a keyframe".
    It exists because the muxer no longer guarantees the property the replay
    path needs; see `starts_with_keyframe` and `_Broadcaster.tail`.
    """

    #: A top-level box bigger than this is not a box: an `mdat` of one GOP is
    #: two orders of magnitude under it, so a header claiming more means either
    #: the wrong container or a lost sync mark.
    MAX_UNIT = 1 << 24

    def __init__(self):
        self._buf = b''
        self._header = b''
        self._open = b''
        self._started = False
        #: Set once, on the very first header, when this is not a box stream.
        #: Never after the first fragment -- `_take` resynchronises by handing
        #: the whole buffer over instead, so there is no mid-stream loss here.
        self.broken = False
        #: See `_Clusters.restartable`. Always True for this framer, because the
        #: only way it breaks is before it has emitted anything.
        self.restartable = True
        #: Every byte handed in, so the caller can restart from scratch.
        self.backlog = b''

    def feed(self, chunk):
        """Return `[(is_media, unit, is_sync)]` for the completed fragments.

        `is_sync` answers `tail()`'s question -- may a replay begin at this
        unit's first byte -- and for this container the answer is a keyframe
        test (`starts_with_keyframe`), asked here because this is where the
        whole fragment exists.
        """
        self.backlog += chunk
        out = []
        self._buf += chunk
        while True:
            box = self._take()
            if box is None:
                break
            out.extend(self._accept(box))
        return out

    def flush(self):
        """The pipe ended; a fragment that never completed is still a picture."""
        head, tail = self._open, self._buf
        self._open = self._buf = b''
        out = [(True, head, starts_with_keyframe(head))] if head else []
        if tail:
            # Unparsed leftover bytes: whatever they are, they are not a
            # fragment, so a replay may not begin here.
            out.append((True, tail, False))
        return out

    def _take(self):
        """One whole top-level box, or None while the buffer is short of one."""
        if len(self._buf) < 8:
            return None
        size = int.from_bytes(self._buf[:4], 'big')
        if size == 1:                       # 64-bit largesize follows
            if len(self._buf) < 16:
                return None
            size = int.from_bytes(self._buf[8:16], 'big')
        elif size == 0:                     # "to the end of the file", and a
            size = len(self._buf)           # live pipe has no such end
        elif size < 8 or size > self.MAX_UNIT:
            # Not a header. Before the first fragment this says "not an MP4",
            # and the caller takes over; after it we are out of sync, and the
            # least wrong thing is to hand the bytes over whole and look again.
            if not self._started:
                self.broken = True
                return None
            size = len(self._buf)
        if size > len(self._buf):
            return None
        box, self._buf = self._buf[:size], self._buf[size:]
        return box

    def _accept(self, box):
        """One whole box in; the fragments that box completed out."""
        if box[4:8] != b'moof':
            if not self._started:
                self._header += box         # ftyp / moov: the file header
                return []
            self._open += box               # the mdat belonging to this one
            return []
        out = []
        if not self._started:
            self._started = True
            if self._header:
                out.append((False, self._header, False))
                self._header = b''
        elif self._open:
            out.append((True, self._open, starts_with_keyframe(self._open)))
        self._open = box
        return out


#: What `_Clusters._take` answers when the bytes at the front of the buffer are
#: not a Matroska element header at all -- a different thing from "not yet".
_CLUSTERS_LOST = object()


class _Clusters(object):
    """Re-frame a live Matroska pipe into whole Clusters.

    The same job `_Fragments` does for fragmented MP4, for two reasons Matroska
    earns on its own. Everything this broadcaster does to shed load removes
    *units*, and a unit that is half a Cluster leaves the viewer holding an
    element header whose size no longer matches the bytes -- with no packet
    marker to re-synchronise on, unlike MPEG-TS. And a late joiner cannot start
    anywhere but a Cluster boundary, so the one-time header is kept back here
    and handed out per connection.

    Where the MP4 framer reads fixed-width box headers, this one reads Matroska's
    self-describing ones, so a boundary is *computed* rather than searched for:
    scanning for the four Cluster bytes would also find them inside the H.264
    data. (Measured on a real capture: 54 hits in 3 MB, every one of them a true
    cluster, spaced 50-60 KB -- which is how long a scanner would get away with
    it before the one false hit that ends the session.)

    The price is the browser target's price: a cluster goes out whole, so the
    live edge sits one cluster behind the encoder. Measured here that is 50-60 KB
    at these bitrates, about 0.07 s -- not a GOP, because this muxer rolls a
    cluster far more often than the keyframe interval would suggest.
    """

    #: An element claiming more than this is not an element: it is a byte we
    #: mistook for a header, and waiting for it to arrive would stall a live
    #: stream for minutes.
    MAX_UNIT = 1 << 26

    def __init__(self):
        #: Bytes not yet handed out, always starting at an element boundary.
        self._buf = bytearray()
        #: Elements seen since the last Cluster: the file header at the head of
        #: the stream, stray elements (`Void`, `Tags`) between clusters later.
        #: These ride at the front of the next unit so the byte stream a viewer
        #: reassembles stays an unbroken element sequence.
        self._pending = bytearray()
        self._started = False
        #: Set once, when the bytes stop being a parseable element tree. Framing
        #: is over; what happens next depends on `restartable`.
        self.broken = False
        #: Whether the caller may throw this framer away and start over from
        #: `backlog`. True only while nothing has been handed to a viewer: once
        #: the first cluster is on the wire, replaying the backlog would put that
        #: cluster there twice.
        self.restartable = True
        #: Every byte handed in, so the caller can restart from scratch.
        self.backlog = b''

    def feed(self, chunk):
        """Return `[(is_media, unit, is_sync)]` for the completed Clusters.

        `is_sync` is always False here: a Cluster boundary is a *parse* point
        (that is the whole reason this framer exists) but this framer does not
        read the SimpleBlock flags, so it cannot promise a *decode* point, and
        a wrong True is the expensive direction. Nothing replays this shape
        today -- `_Session.replay` is the browser target, which is fMP4 -- so
        the refusal costs nothing and is the direction that cannot smear.
        """
        self.backlog += chunk
        self._buf += chunk
        out = []
        while not self.broken:
            element = self._take()
            if element is None:
                break                         # the next element is still arriving
            if element is _CLUSTERS_LOST:
                self._lost_sync(out)
                break
            ident, body = element
            if ident != MKV_FIRST_CLUSTER:
                self._pending += body         # header, or a stray element
                continue
            prefix = bytes(self._pending)
            del self._pending[:]
            if not self._started:
                self._started = True
                self.restartable = False
                if prefix:
                    # Everything before the first cluster names the codecs, and
                    # `_hold` keeps a `is_media=False` unit out of the replay
                    # ring: it is written onto each connection instead.
                    out.append((False, prefix, False))
            elif prefix:
                # Stray elements in front of a cluster: media bytes for the
                # byte stream, but not a picture anyone can start on.
                out.append((True, prefix, False))
            out.append((True, body, False))
        return out

    def _take(self):
        """One whole top-level element, or None while the pipe is short of one.

        `_CLUSTERS_LOST` is the answer that says "this stopped being a Matroska
        element tree", which is a different thing from "not yet".
        """
        while True:
            buf = self._buf
            if not buf:
                # Nothing at all, which is "not yet" -- an element ended exactly
                # on this read boundary. `_ebml_id` cannot tell that apart from
                # an unreadable first byte, so it has to be asked before it.
                return None
            ident, ilen = _ebml_id(buf, 0)
            if ident is None:
                return None if ilen < 0 else _CLUSTERS_LOST
            if ilen == len(buf):
                return None                   # the ID is whole, its size is not
            size, slen = _ebml_size(buf, ilen)
            if slen < 0:
                return None                   # ... and so is the rest of it
            if slen == 0:
                return _CLUSTERS_LOST
            head = ilen + slen
            if size == 'unknown':
                if ident != MKV_SEGMENT:
                    # Unknown size means "to the end of my container", and a live
                    # pipe never says where that is. `Segment` is the one element
                    # a streaming muxer is allowed to leave like that, and its
                    # children follow immediately -- so descend into it, and treat
                    # anything else as the end of framing.
                    return _CLUSTERS_LOST
                # The bytes of the header we are descending past still have to
                # reach a late joiner: a Cluster outside a `Segment` is not a
                # stream. They ride at the front of the pending header, in order.
                self._pending += bytes(buf[:head])
                del self._buf[:head]
                continue
            if size > self.MAX_UNIT:
                return _CLUSTERS_LOST
            if len(buf) < head + size:
                return None                   # the element is still in the pipe
            body = bytes(buf[:head + size])
            del self._buf[:head + size]
            return ident, body

    def _lost_sync(self, out):
        """The front of the buffer is not an element header. Say what it means."""
        self.broken = True
        junk = bytes(self._pending) + bytes(self._buf)
        del self._pending[:]
        del self._buf[:]
        if self._started:
            # Out of sync with a viewer already watching. The choices are a
            # stalled live pipe or a unit that is not a whole element, and a
            # viewer that stops getting bytes is the worse report to the user --
            # so hand them over and let `_retain` stop framing for the run.
            if junk:
                out.append((True, junk, False))
        # Before the first cluster this is "not the container we were promised",
        # and `backlog` holds every byte of it. `_retain` restarts framing from
        # there without this framer, which for the live DLNA shape means the
        # plain marker search and for a pipe means the bytes as they came.

    def flush(self):
        """The pipe ended; a cluster that never completed is still a picture."""
        head = bytes(self._pending) + bytes(self._buf)
        del self._pending[:]
        del self._buf[:]
        if not head:
            return []
        # Never started: what is left is a header with no picture in it, and it
        # belongs to `init_segment`, not to the replay ring.
        return [(False if not self._started else True, head, False)]


class _Broadcaster(object):
    """Fan out encoder output to every connected client; drop for the slow."""

    def __init__(self, maxsize=256, ring_bytes=REPLAY_BYTES, init_marker=None,
                 packet_align=None):
        self._ring_limit = ring_bytes
        #: bytes of the container header (fMP4: everything before the first
        #: `moof`; Matroska: everything before the first `Cluster`). A late
        #: joiner needs it or neither MSE nor a demuxer can start at all.
        self._init_marker = init_marker
        #: Only the two containers that cannot be read any other way frame their
        #: bytes. Fragmented MP4 is the consumer whose replay *and* whose drops
        #: have to land on fragment edges; a live Matroska pipe additionally
        #: cannot be *joined* except on a Cluster boundary. MPEG-TS and the
        #: DVD-era shapes self-synchronise, and the DLNA log is byte-addressed.
        if init_marker == b'moof':
            self._framer = _Fragments()
        elif init_marker == MKV_FIRST_CLUSTER:
            self._framer = _Clusters()
        else:
            self._framer = None
        #: Unframed but never unaligned: where we drop for a slow consumer, the
        #: hole has to end on a container packet boundary. Cutting inside a
        #: 188-byte TS packet leaves the reader resynchronising on a stream whose
        #: audio frame just lost its middle, which is a click, not a stutter.
        self._align = packet_align if self._framer is None else None
        self._carry = b''
        #: `maxsize` is already counted in this container's own unit -- whole
        #: fragments where a framer is running, 4 KiB reads where one is not --
        #: because `live_queue_units` decided which unit that is from the same
        #: `init_marker` this object picks its framer from. There used to be a
        #: second, hard-coded clamp to eight in here, and eight fragments is
        #: 0.11 s of a 70.8-fragments-per-second stream: the sender shed 28 % of
        #: its own output to a loopback curl while the budget said 0.75 s.
        self._maxsize = maxsize
        self._init = b''
        #: An Event rather than a flag because the handler *waits* for the
        #: header: it is the one thing a viewer can never recover from losing,
        #: so it does not sit in a queue that drops -- see `await_init`.
        self._init_ready = threading.Event()
        if init_marker is None:
            self._init_ready.set()
        self._ring = deque()
        self._ring_bytes = 0
        self._subs = set()
        self._lock = threading.Lock()
        self.chunks = 0
        self.bytes = 0
        self.drops = 0
        #: Replays that found no keyframe to start on and so were not handed
        #: out at all -- see `tail`. Counted (and logged) because the
        #: alternative reading of "the viewer got nothing" is "the ring was
        #: empty", and those want different fixes.
        self.keyframe_misses = 0

    @property
    def init_segment(self):
        with self._lock:
            return self._init if self._init_ready.is_set() else b''

    def await_init(self, timeout=10.0):
        """The container header, or b'' if the encoder never got that far."""
        self._init_ready.wait(timeout)
        return self.init_segment

    def subscribe(self, replay=False):
        q = Queue(maxsize=self._maxsize)
        if replay:
            for chunk in self.tail():
                try:
                    q.put_nowait(chunk)
                except Exception:
                    break
        with self._lock:
            self._subs.add(q)
        return q

    def unsubscribe(self, q):
        with self._lock:
            self._subs.discard(q)

    def tail(self):
        """The ring from its newest keyframe on, or `[]` if it holds none.

        The muxer used to guarantee this by construction: `frag_keyframe` cut
        only on keyframes, so the newest whole fragment always began with an
        IDR and "replay the ring" was safe without asking. Cutting per frame
        (`frag_every_frame`, where this target's 1305 -> 838 ms came from) gave
        that up -- eleven fragments in twelve now begin on a P-frame, and a
        viewer handed one of those watches ~0.5 s of smear before the stream
        catches up.

        So walk back to the newest unit that says a replay may begin there and
        hand out from it: the viewer starts on a picture, and starts *behind*
        the live edge, which is the direction a live stream tolerates. When the
        ring holds no such unit -- a ring shorter than one GOP of fragments,
        which a keyframe every 0.5 s and ~6 s of ring makes rare -- the answer
        is no replay at all: the viewer waits for the live stream to reach its
        next keyframe, which costs the same wait without the smear. Empty,
        never unplayable bytes; counted in `keyframe_misses` and said in the
        log so "the viewer got nothing" and "there was nothing to send" stay
        two different answers.

        And when *every* unit says a replay may begin there -- the unframed
        shapes, where nothing in the ring is ever unplayable -- there is
        nothing to cut back to: the whole ring goes, which is also exactly
        what it handed out before the tag existed. Cutting to the newest unit
        in that case would quietly turn "the ring is entrable end to end" into
        "one fragment of pre-roll", and this path is the one a failed framing
        falls back to, not a place to make the late joiner's buffer smaller.
        """
        with self._lock:
            if all(sync for _chunk, sync in self._ring):
                return [chunk for chunk, _sync in self._ring]
            for index in range(len(self._ring) - 1, -1, -1):
                if self._ring[index][1]:
                    return [chunk for chunk, _sync in list(self._ring)[index:]]
            missed = bool(self._ring)
            held = (len(self._ring), self._ring_bytes)
            if missed:
                self.keyframe_misses += 1
        if missed:
            logger.warning(
                'screen_mirror: replay ring holds %d fragments / %d bytes and '
                'none begins on a keyframe; this viewer starts at the next '
                'one instead of being fed a smear', held[0], held[1])
        return []

    def feed(self, chunk):
        if self._align:
            chunk = self._carry + chunk
            keep = len(chunk) % self._align
            self._carry = chunk[len(chunk) - keep:] if keep else b''
            chunk = chunk[:len(chunk) - keep]
            if not chunk:
                return
        self._emit(chunk)

    def _emit(self, chunk):
        with self._lock:
            self.chunks += 1
            self.bytes += len(chunk)
            units = self._retain(chunk)
        self._publish(units)

    def _publish(self, units):
        with self._lock:
            subs = list(self._subs)
        for q in subs:
            for unit in units:
                while q.full():
                    # live beats lossless, and now the thing that goes is a
                    # whole fragment: the viewer's byte stream stays parseable
                    # across the hole instead of ending inside an `mdat`.
                    try:
                        q.get_nowait()
                        self.drops += 1
                    except Exception:
                        break
                try:
                    q.put_nowait(unit)
                except Exception:
                    pass

    def _retain(self, chunk):
        """Keep the header, then a bounded tail; return what to send.

        Called under the lock. Unframed, the answer is the bytes as they came
        from the pipe -- which is what a live viewer always got. Framed, it is
        whole fragments, and a viewer waits out the rest of one.
        """
        if self._framer is not None:
            units = self._framer.feed(chunk)
            if not self._framer.broken:
                for is_media, unit, is_sync in units:
                    self._hold(is_media, unit, is_sync)
                return [unit for _media, unit, _sync in units]
            if self._framer.restartable:
                # Not this container after all, and nothing has been handed out
                # yet -- the first header is the only thing that can say so -- so
                # falling back to the marker search restarts exactly rather than
                # half-restarted.
                chunk, self._framer = self._framer.backlog, None
            else:
                # Out of sync mid-stream. The framer has already put the bytes it
                # was holding into `units`; re-feeding the backlog would put them
                # on the wire a second time, so keep what was framed and stop
                # framing for the rest of the run.
                self._framer = None
                for is_media, unit, is_sync in units:
                    self._hold(is_media, unit, is_sync)
                return [unit for _media, unit, _sync in units]
        if not self._init_ready.is_set():
            self._init += chunk
            cut = self._init.find(self._init_marker)
            if cut < 0:
                if len(self._init) > 1 << 21:
                    # No marker in 2 MB: not the container we expect. Stop
                    # hoarding, and let bytes be treated as media from here.
                    self._init = b''
                    self._init_ready.set()
                # Everything held so far is the header, which replay serves
                # from `init_segment` -- ringing it too would hand a late joiner
                # the first fragment twice.
                return [chunk]
            chunk = self._init[cut:]
            self._init = self._init[:cut]
            self._init_ready.set()
        # Self-synchronising bytes (MPEG-TS/PS, and the marker-search fallback)
        # can be entered on any unit boundary -- that is what packet alignment
        # exists to guarantee -- so a replay starting here is sound. Nothing
        # asks today (`_Session.replay` is the browser target only); this keeps
        # the ring's old observable behaviour for the shapes that predate the
        # tag rather than inventing a refusal they never had.
        self._ring_append(chunk, True)
        return [chunk]

    def _hold(self, is_media, unit, is_sync):
        """Book one framed unit: the header is kept for replay, media is rung."""
        if is_media:
            self._ring_append(unit, is_sync)
        else:
            self._init = unit
            self._init_ready.set()

    def _ring_append(self, chunk, is_sync):
        if not self._ring_limit:
            return
        self._ring.append((chunk, is_sync))
        self._ring_bytes += len(chunk)
        while self._ring_bytes > self._ring_limit and len(self._ring) > 1:
            self._ring_bytes -= len(self._ring.popleft()[0])

    def flush(self):
        """stdout ended: the last picture is still inside the framer."""
        if self._framer is None:
            carry, self._carry = self._carry, b''
            if carry:
                # The tail of a container packet. It cannot be decoded -- a TS
                # packet is 188 bytes and this is the rest of one -- but the
                # encoder is gone, so nothing is coming to complete it. Hand it
                # over instead of stranding it in a buffer nobody reads again.
                self._emit(carry)
            return
        units = self._framer.flush()
        with self._lock:
            for is_media, unit, is_sync in units:
                self._hold(is_media, unit, is_sync)
        self._publish([unit for _media, unit, _sync in units])

    def clients(self):
        with self._lock:
            return len(self._subs)

    def queued(self):
        """Units sitting unsent in front of every live viewer, right now.

        A gauge for the statistics row, not a decision the stream makes:
        `Queue.qsize()` is documented as unreliable in a multithreaded context,
        and read next to `drops` it is the difference between "the queue is
        full and shedding" and "there is room and nothing is being lost".
        """
        with self._lock:
            subs = list(self._subs)
        return sum(q.qsize() for q in subs)


class _ByteLog(object):
    """Encoder output kept addressable by **absolute byte offset**.

    A DLNA renderer is not watching a live stream: it was handed a URL it
    believes is a 1.9 GB file, so it closes the connection mid-way and comes
    back with `Range: bytes=14680064-` as if nothing happened. Unlike the
    queue broadcaster, nothing here is dropped for being slow -- the last 48
    MiB of produced bytes stay readable, and a reader that wants bytes the
    encoder has not made yet waits for them.
    """

    def __init__(self, limit=DLNA_RING_BYTES, pad=PS_PADDING):
        self._limit = limit
        self._pad = pad
        self._chunks = deque()
        self._held = 0
        self._start = 0                 # absolute offset of _chunks[0]
        self._end = 0                   # absolute offset of the next byte
        self._cond = threading.Condition()
        self._closed = False
        self._readers = 0
        #: Same names as _Broadcaster, so stats() can read either.
        self.chunks = 0
        self.bytes = 0
        self.drops = 0                  # chunks that aged out of the window

    def feed(self, chunk):
        with self._cond:
            self.chunks += 1
            self.bytes += len(chunk)
            self._end += len(chunk)
            self._chunks.append(chunk)
            self._held += len(chunk)
            while self._held > self._limit and len(self._chunks) > 1:
                evicted = self._chunks.popleft()
                self._held -= len(evicted)
                self._start += len(evicted)
                self.drops += 1
            self._cond.notify_all()

    def close(self):
        with self._cond:
            self._closed = True
            self._cond.notify_all()

    @property
    def start(self):
        with self._cond:
            return self._start

    @property
    def end(self):
        with self._cond:
            return self._end

    def clients(self):
        with self._cond:
            return self._readers

    def reader_enter(self):
        with self._cond:
            self._readers += 1
            return self._readers

    def reader_leave(self):
        with self._cond:
            self._readers = max(0, self._readers - 1)

    def anchor(self, prefill):
        """Where the advertised file's offset 0 sits.

        `prefill` bytes behind the live edge, so the renderer's opening sniff
        is answered out of memory instead of stalling on the encoder. That
        hoard is also the whole latency of this target: the TV drains it at
        line rate and then tracks the live edge from that far behind -- which is
        why the budget is derived from the bitrate (`dlna_prefill_bytes`) rather
        than being a fixed number of megabytes.
        """
        with self._cond:
            return max(self._start, self._end - prefill)

    def read(self, absolute, length, deadline=None):
        """Up to `length` bytes at `absolute`, blocking until they exist.

        Returns (data, complete). `complete` is False when the encoder could
        not keep up before `deadline` -- or the session ended -- and the
        caller has to fill the rest in itself, because a renderer that asked
        for exactly n bytes must get exactly n bytes back.
        """
        out = bytearray()
        with self._cond:
            while len(out) < length:
                # Re-checked every round: the window keeps sliding while we
                # wait, and a reader that lags the eviction must resync to the
                # oldest byte still held rather than index off the front of the
                # deque (a negative slice would hand back the *tail* of a chunk
                # and the TV would blame our encoder for the garbage).
                cursor = max(absolute + len(out), self._start)
                have = self._collect(cursor, length - len(out))
                if have:
                    out += have
                    continue
                if self._closed:
                    return bytes(out), False
                remaining = None if deadline is None else deadline - time.time()
                if remaining is not None and remaining <= 0:
                    return bytes(out), False
                self._cond.wait(0.5 if remaining is None
                                else min(0.5, remaining))
            return bytes(out), True

    def _collect(self, absolute, length):
        """Bytes available right now in [absolute, absolute+length).

        Caller holds the lock; `absolute` must already be clamped to `_start`.
        """
        if absolute >= self._end:
            return b''
        offset = self._start
        for chunk in self._chunks:
            nxt = offset + len(chunk)
            if nxt > absolute:
                cut = absolute - offset
                return chunk[cut:min(len(chunk), cut + length)]
            offset = nxt
        return b''

    def pad(self, length):
        """`length` bytes of MPEG-PS padding: what a bounded sniff gets
        filled in with when the encoder has not produced that much yet."""
        if length <= 0:
            return b''
        return (self._pad * (length // len(self._pad) + 1))[:length]


def parse_range(header):
    """`Range: bytes=100-200` -> (100, 200); `bytes=100-` -> (100, None).

    Only a single range is meaningful here: the "file" is one endless stream,
    so a multi-range request is a client we cannot serve honestly. Suffix
    ranges (`bytes=-1024`) are dropped for the same reason -- "the last 1 kB of
    a file with no end" has no answer a TV would like.
    """
    if not header:
        return 0, None
    unit, _, spec = header.partition('=')
    if unit.strip().lower() != 'bytes' or ',' in spec:
        return None
    first, _, last = spec.partition('-')
    try:
        start = int(first)
    except ValueError:
        return None
    stop = None
    if last:
        try:
            stop = int(last)
        except ValueError:
            return None
    if stop is not None and stop < start:
        return None
    return start, stop


class _StreamHandler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    #: How many request/response exchanges of one session reach the normal log.
    #: A renderer that will not play our stream makes its whole case out of
    #: retries -- the television measured here opened eight connections and read
    #: 26 MB across them -- so the first few exchanges are the evidence, and all
    #: of them would be noise. The rest are still counted.
    EXCHANGE_LOG_LIMIT = 6

    def log_message(self, fmt, *args):
        logger.debug("stream http: " + fmt % args)

    # -- what the client asked for, and what we answered -------------------
    # None of this is inferable after the fact: the plugin logs requests at
    # DEBUG (which the module log does not keep) and a renderer that fails is
    # indistinguishable in the log from one that played. These three hooks
    # record the exchange at the level a user's log file actually holds.
    def handle_one_request(self):
        self._exchange_status = None
        self._exchange_headers = {}
        return super().handle_one_request()

    def send_response_only(self, code, message=None):
        self._exchange_status = code
        return super().send_response_only(code, message)

    def send_header(self, keyword, value):
        name = keyword.lower()
        if name in ('content-length', 'content-range', 'content-type',
                    'transfermode.dlna.org', 'accept-ranges'):
            self._exchange_headers[name] = value
        return super().send_header(keyword, value)

    def end_headers(self):
        summary = super().end_headers()
        path = self.path.partition('?')[0]
        if not (path.startswith(STREAM_PREFIX) or path == BROWSER_PATH):
            return summary
        session = getattr(self.server, 'session', None)
        if session is None:
            return summary
        session.exchanges += 1
        if session.exchanges <= self.EXCHANGE_LOG_LIMIT:
            logger.info(
                'exchange %d: %s %s -> %s len=%s range=%s from=%s ua=%s',
                session.exchanges, self.command,
                'dlna-file' if getattr(session, 'bytelog', None) else 'live',
                self._exchange_status,
                self._exchange_headers.get('content-length', '-'),
                self.headers.get('Range') or '(none)',
                self.headers.get('transferMode.dlna.org') or '(none)',
                (self.headers.get('User-Agent') or '-')[:40])
        return summary

    def _peer_gone(self):
        """True once the peer has closed its half.

        A renderer reads, closes, and opens another connection -- eight of them
        in one measured attempt -- and this side would sit in `log.read()`
        waiting for the encoder to produce the next chunk, which is how seven
        sockets were left in CLOSE_WAIT. A non-blocking peek finds the FIN
        without consuming a byte.
        """
        try:
            import select
            readable, _, _ = select.select([self.connection], [], [], 0)
            if not readable:
                return False
            return self.connection.recv(1, socket.MSG_PEEK) == b''
        except (OSError, ValueError):
            return True
        except Exception:                                      # noqa: BLE001
            return False

    @property
    def session(self):
        return self.server.session

    def _not_found(self):
        self.send_response(404)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def _no_delay(self):
        """Turn Nagle off on this connection, once per response.

        The stream is written as many small chunks -- a 4 KiB read per
        `wfile.write`, with a flush after each -- and Nagle holds a short write
        back while an earlier one is unacknowledged. Paired with the peer's
        delayed ACK that delays each chunk by tens of milliseconds, jittering,
        which is what "画面是清楚的，就是慢半拍" is made of on a LAN that has
        bandwidth to spare. A live stream is the case Nagle is worst for: it
        batches in *time*, and time is the thing being streamed. Never fatal --
        a socket that cannot take the option still streams.
        """
        try:
            self.connection.setsockopt(socket.IPPROTO_TCP,
                                       socket.TCP_NODELAY, 1)
        except (OSError, AttributeError) as e:
            logger.debug('cannot disable Nagle on this connection: %s', e)

    def _authorized(self, suffix):
        """The stream id is the whole credential, so compare it in constant
        time -- a timing oracle on an 8-byte hex id is not worth the risk."""
        name = self.path.partition('?')[0].rsplit('/', 1)[-1]
        try:
            import hmac
            return hmac.compare_digest(name, self.session.stream_name(suffix))
        except Exception:
            return name == self.session.stream_name(suffix)

    def do_HEAD(self):
        # Renderers probe with HEAD before they commit to a read.
        if self.path.partition('?')[0].startswith(STREAM_PREFIX):
            self.do_GET(head_only=True)
        else:
            self._not_found()

    def do_GET(self, head_only=False):
        path = self.path.partition('?')[0]
        if path == BROWSER_PATH:
            return self._serve_page(head_only)
        if path == WEBRTC_PATH:
            return self._serve_webrtc_page(head_only)
        if path == BROWSER_STATS_PATH:
            return self._serve_stats()
        if path.startswith(STREAM_PREFIX):
            if not self._authorized(self.session.suffix):
                return self._not_found()
            if self.session.bytelog:
                return self._serve_infinite_file(head_only)
            return self._serve_stream(head_only)
        return self._not_found()

    def do_POST(self):
        """The WebRTC signaling door: exactly two fields, both session-gated.

        Not part of `macast.protocol`'s `Handler.POST_ROUTES` table -- that
        table guards the *application's* management API on 58880, and this is
        the mirror session's own server on a random port. The credential rule
        is still the strictest one available here: only `page_token` (the
        same per-session token as the viewing page), never `Api_Token` -- a
        viewing URL gets copied and forwarded, and the management token opens
        the whole app's API (AGENTS.md 4.7). An offer is kilobytes of SDP, so
        the answer comes back in a request body -- there is no GET shape of
        this door.
        """
        path = self.path.partition('?')[0]
        if path not in (WEBRTC_SESSION_PATH, WEBRTC_ANSWER_PATH):
            return self._not_found()
        if not self._page_authorized():
            return self._json(403, {'code': 1, 'message': 'a token is required'})
        bridge = getattr(self.server, 'broadcaster', None)
        if getattr(bridge, 'create_offer', None) is None:
            return self._json(409, {
                'code': 1,
                'message': 'this session is not mirroring to WebRTC',
            })
        try:
            if path == WEBRTC_SESSION_PATH:
                peer, offer = bridge.create_offer()
                return self._json(200, {'code': 0, 'peer': peer,
                                        'offer': offer})
            return self._post_answer(bridge)
        except RuntimeError as e:
            # The bridge's own refusals: too many viewers, an unknown peer,
            # or a session that has meanwhile stopped. All of them are the
            # client's answer, not a server fault.
            return self._json(409, {'code': 1, 'message': str(e)})
        except Exception as e:                                 # noqa: BLE001
            logger.error('the WebRTC signaling request failed: %s', e)
            return self._json(500, {'code': 1, 'message': 'signaling failed'})

    def _post_answer(self, bridge):
        """Read one JSON answer out of the request body and hand it over."""
        import urllib.parse
        query = urllib.parse.parse_qs(self.path.partition('?')[2] or '')
        peer = (query.get('peer') or [''])[0]
        try:
            length = int(self.headers.get('Content-Length') or 0)
        except ValueError:
            length = 0
        # An SDP answer is a couple of kilobytes; the cap is how a broken or
        # hostile client cannot make this thread allocate its own claim.
        if length <= 0 or length > WEBRTC_MAX_ANSWER_BYTES:
            return self._json(400, {'code': 1,
                                    'message': 'a JSON body is required'})
        raw = self.rfile.read(length)
        try:
            message = json.loads(raw.decode('utf-8'))
        except (ValueError, UnicodeDecodeError):
            return self._json(400, {'code': 1,
                                    'message': 'the body is not JSON'})
        if not isinstance(message, dict):
            return self._json(400, {'code': 1,
                                    'message': 'the body is not an answer'})
        sdp = message.get('sdp')
        type_ = message.get('type') or 'answer'
        if not isinstance(sdp, str) or not sdp:
            return self._json(400, {'code': 1,
                                    'message': 'the answer carries no sdp'})
        bridge.set_answer(peer, sdp, type_)
        return self._json(200, {'code': 0, 'peer': peer})

    def _json(self, status, payload):
        """One JSON answer for the signaling door, with the same two headers
        every response of this server carries: `nosniff` and `no-store`."""
        body = json.dumps(payload).encode('utf-8')
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self.wfile.write(body)

    def _serve_stream(self, head_only=False):
        broadcaster = self.server.broadcaster
        if getattr(broadcaster, 'subscribe', None) is None:
            # The WebRTC session shares this server but has no byte stream to
            # serve (its frames travel over UDP/ICE, not HTTP). A client that
            # falls back to the stream URL deserves an honest 404, not the
            # AttributeError of an object with no `subscribe`.
            return self._not_found()
        # A viewer cannot decode a container without the header that precedes
        # it, and it can never ask for that again -- so the header is written
        # here, once, on this connection, rather than being queued up among
        # droppable fragments behind a tab that stopped reading. This is not a
        # browser-only courtesy: a live Matroska profile without it is
        # unopenable by anything, see `MKV_FIRST_CLUSTER`.
        init = (broadcaster.await_init()
                if self.session.send_init and not head_only else b'')
        queue = broadcaster.subscribe(replay=self.session.replay)
        try:
            self._no_delay()
            self.send_response(200)
            self.send_header('Content-Type', self.session.content_type)
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Accept-Ranges', 'none')
            #: A no-op unless this is the DLNA target: a renderer refuses the
            #: stream without `transferMode`/`contentFeatures`, and the live
            #: shape must not advertise ranges it cannot serve.
            self._dlna_headers(ranges=False)
            self.end_headers()
            if head_only:
                return
            if init:
                self.wfile.write(init)
                self.wfile.flush()
                self.session.note_written(len(init))
            while True:
                try:
                    chunk = queue.get(timeout=5.0)
                except Empty:
                    continue
                self.wfile.write(chunk)
                self.wfile.flush()
                self.session.note_written(len(chunk))
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            broadcaster.unsubscribe(queue)

    def _serve_infinite_file(self, head_only=False):
        """The DLNA target: answer as if this were a finite media file.

        Three things make a ten-year-old renderer accept an endless stream:
        every response names a size below 2 GiB, a bounded sniff is answered
        with *exactly* the number of bytes it asked for (PS padding fills the
        gap while the encoder catches up), and a reconnect with an absolute
        byte offset lands on the same byte it left -- see _ByteLog.
        """
        log = self.server.broadcaster
        session = self.session
        if session.file_anchor is None:
            session.file_anchor = log.anchor(
                dlna_prefill_bytes(session.profile))
        requested = parse_range(self.headers.get('Range'))
        if requested is None:
            self._not_found()
            return
        start, stop = requested
        # A renderer hunting for a container index asks for an offset near the
        # end of whatever size it believes in -- millions of bytes past anything
        # the encoder has produced. Padding that gap (what this used to do)
        # answers a fabricated offset with fabricated bytes, and a real
        # Android 11 television read that as a broken index and started over
        # instead of playing: whole file, tail, whole file, tail, eight times.
        # 416 with the offset we actually hold is the honest answer, and it is
        # the one that tells a player there is no index to go looking for.
        ahead = (session.file_anchor + start) - log.end
        if ahead > DLNA_MAX_AHEAD_BYTES:
            logger.info('the renderer asked for %d bytes past the encoder '
                        '(offset %d); answering 416 with the live size %d',
                        ahead, session.file_anchor + start, log.end)
            self.send_response(416)
            self.send_header('Content-Range', 'bytes */{}'.format(log.end))
            self.send_header('Content-Length', '0')
            self._dlna_headers()
            self.end_headers()
            return
        if start >= session.file_size:
            # Past the end of the file we promised. 416 with our own size is
            # honest and keeps the client from concluding the file is broken.
            self.send_response(416)
            self.send_header('Content-Range',
                             'bytes */{}'.format(session.file_size))
            self.send_header('Content-Length', '0')
            self._dlna_headers()
            self.end_headers()
            return
        # A Range *header* is what makes this a 206, even when it only names a
        # start; a missing one means "the whole file", which is the same body
        # but a 200 and no Content-Range.
        has_range = bool(self.headers.get('Range'))
        bounded = stop is not None
        last = session.file_size - 1
        if bounded:
            last = min(stop, last)
        length = last - start + 1
        self._no_delay()
        self.send_response(206 if has_range else 200)
        self.send_header('Content-Type', session.content_type)
        self.send_header('Content-Length', str(length))
        if has_range:
            # Content-Range on a 200 is malformed, and a 200 here means "the
            # whole file, from the top" -- which is also what the length says.
            self.send_header('Content-Range', 'bytes {}-{}/{}'.format(
                start, last, session.file_size))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self._dlna_headers()
        self.end_headers()
        if head_only:
            return
        log.reader_enter()
        try:
            absolute = session.file_anchor + start
            written = 0
            deadline = None if not bounded else \
                time.time() + DLNA_SNIFF_TIMEOUT
            while written < length:
                want = min(length - written, 1 << 20)
                #: How much of this response is content. Padding is a fabricated
                #: answer to an offset the encoder has not reached, and the
                #: watchdog asks `note_written` whether anyone is watching --
                #: counting invented bytes there would report a picture over a
                #: player that is only probing the tail of the fake file.
                content = 0
                if not bounded:
                    # Unbounded means "play until I say stop": block for as
                    # long as the encoder is alive, because a short body makes
                    # the renderer think the file ended and tear down.
                    # Wait in one-second slices, and between them look for the
                    # peer's FIN: a television that gave up on this connection
                    # closes it, and nothing else here would ever notice.
                    data, complete = log.read(absolute, want,
                                              deadline=time.time() + 1.0)
                    if not data and not complete and self._peer_gone():
                        return
                    if not data and not complete:
                        return          # the session closed
                else:
                    data, complete = log.read(absolute, want, deadline=deadline)
                content = len(data)
                if bounded and not complete:
                    data += log.pad(want - content)
                if not data:
                    return
                self.wfile.write(data)
                self.wfile.flush()
                session.note_written(content)
                written += len(data)
                absolute += len(data)
                if bounded and written >= length:
                    return      # exactly n bytes, then the connection closes
        except (BrokenPipeError, ConnectionResetError, OSError):
            # The TV reconnecting with a Range is normal, not an error: it is
            # how a renderer fills its own buffer.
            logger.debug('the renderer closed the file read at offset %s',
                         start)
        finally:
            log.reader_leave()

    def _dlna_headers(self, ranges=True):
        """The DLNA-specific headers, on every answer of a DLNA session.

        `transferMode.dlna.org: Streaming` is what tells the renderer not to
        wait for the whole file, and plenty of firmware refuses the stream
        without `contentFeatures.dlna.org`. `Accept-Ranges: bytes` belongs to
        the file shape, where the client's buffering strategy *is* seeking; it
        is left off the live one, where it would promise what cannot be kept.
        """
        if self.session.kind != 'dlna':
            return
        if ranges:
            self.send_header('Accept-Ranges', 'bytes')
        self.send_header('transferMode.dlna.org', DLNA_SECONDARY_HEADER)
        self.send_header('contentFeatures.dlna.org',
                         content_features(self.session.profile))

    def _page_authorized(self):
        """The viewing page and its stats share one credential.

        `page_token` is per-session and dies with the mirror -- deliberately not
        the app's stable `Api_Token`, which also opens the whole management API
        (AGENTS.md 4.7). A viewing URL gets copied around; a management token
        that leaks once stays open forever.
        """
        import hmac
        import urllib.parse
        query = urllib.parse.parse_qs(self.path.partition('?')[2] or '')
        token = (query.get('token') or [''])[0]
        return bool(token) and hmac.compare_digest(token,
                                                   self.session.page_token)

    def _serve_page(self, head_only=False):
        if not self._page_authorized():
            self.send_error(403, 'a token is required')
            return
        body = PLAYER_PAGE.replace('@STREAM@', self.session.stream_path()) \
                          .replace('@CODECS@', live_codecs(
                              self.session,
                              getattr(self.server, 'broadcaster', None))) \
                          .replace('@LIVE_EDGE@', repr(live_edge_seconds())) \
                          .replace('@DIAG@', json.dumps(page_diag(self.session))) \
                          .replace('@TITLE@', self.session.page_title).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def _serve_webrtc_page(self, head_only=False):
        """The WebRTC viewing page: same credential as the other page.

        `page_diag` is injected once, like the browser page's overlay gets it;
        everything live is polled from `/browser/stats` -- which works here
        unchanged because the bridge answers to the same counter names. The
        page reads its own token back out of its URL; nothing in the body
        repeats it (a copy would outlive the tab's address).
        """
        if not self._page_authorized():
            self.send_error(403, 'a token is required')
            return
        body = WEBRTC_PAGE.replace('@DIAG@', json.dumps(page_diag(self.session))) \
                          .replace('@TITLE@', self.session.page_title) \
                          .encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def _serve_stats(self):
        """The overlay's per-second read of the sender's own counters."""
        if not self._page_authorized():
            self.send_error(403, 'a token is required')
            return
        body = json.dumps(page_stats(
            self.session, getattr(self.server, 'broadcaster', None))
        ).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        self.wfile.write(body)


#: The video part of the MediaSource codec string this file used to *assert*.
#: It was wrong on every shape except one, and a wrong string here is not a
#: cosmetic lie: `MediaSource.isTypeSupported()` answers no, the page falls to
#: its progressive path, and a live fragmented MP4 has no playable duration
#: there -- a black player with no error.
#:
#: Measured with `scripts/codec_string_probe.py`, which reads the avcC box out
#: of the init segment each shipped encoder really produced -- the x264 rows on
#: this machine (2026-10-02) and the NVENC rows on the Windows box (2026-10-03,
#: ffmpeg 8.1.2; the x264 rows repeated there identically):
#:
#:     x264  1080p  avc1.42c028   (Constrained Baseline, level 4.0)
#:     x264  2160p  avc1.42c033
#:     x264   720p  avc1.42c01f
#:     VT    1080p w/ -level 42   avc1.64002a
#:     VT    2160p w/ -level 51   avc1.640033
#:     VT    1080p, no level      avc1.640028   <- the only row the claim fit
#:     NVENC 1080p  avc1.640028   (fits by coincidence, not by design)
#:     NVENC 2160p  avc1.640033   (same encoder, and this branch pins no level)
#:
#: The NVENC pair is the reason the pinned default stays a fallback rather than
#: becoming a per-platform constant: the level is a property of the picture, so
#: no string can be right for both rows.
#:
#: So the four bytes are not a constant of this plugin -- they are a property of
#: the encoder, its profile pin, and the size being encoded. `avc_codec_string`
#: reads them off the wire; this stays the fallback for the window before the
#: encoder has written its header, where nothing else can be said yet.
MSE_VIDEO_CODEC_DEFAULT = 'avc1.640028'

#: The audio part. AAC-LC in ISO BMF, which is what `-c:a aac` emits here and
#: what every browser with an MSE implementation accepts; unlike the video
#: fourcc it is not something the encoder decides.
MSE_AUDIO_CODEC = 'mp4a.40.2'


def codecs_for(fourcc, has_audio):
    """A MediaSource codec string, in the only shape `addSourceBuffer` takes:
    a MIME type with a quoted codec list."""
    if has_audio:
        return 'video/mp4; codecs="%s,%s"' % (fourcc, MSE_AUDIO_CODEC)
    return 'video/mp4; codecs="%s"' % fourcc


def avc_codec_string(header):
    """The `avc1.PPCCLL` fourcc inside an init segment's avcC box, or None.

    Scans for the four-character box type rather than walking the tree: an
    init segment is ftyp + moov and nothing else, there is no codec payload in
    it to produce a false hit, and a walker would have to know the sample-entry
    layout (avc1's 36 reserved bytes before avcC) to get the same answer. The
    declared box size bounds the slice it reads, but the two checks that decide
    are `len(body) >= 4` (a box truncated below the four bytes is not an avcC)
    and configurationVersion being 1 -- together they turn "these four bytes
    spelled avcC inside somebody's comment string" into a rejection. A separate
    minimum-size test was measured to be redundant with the first of them: a
    declared size below 12 leaves a slice shorter than four bytes, so the two
    questions have the same answer for every size from 0 to 2^31.

    avcC is `(configurationVersion, AVCProfileIndication,
    profile_compatibility, AVCLevelIndication, ...)`; the codec string is the
    last three as hex, which is why a profile byte read from a listing or from
    `-profile:v` would not do: x264 was *told* `high` and wrote
    `42c0` (Constrained Baseline) into the SPS it actually emitted.
    """
    if not header:
        return None
    start = 0
    while True:
        i = header.find(b'avcC', start)
        if i < 8:
            if i == -1:
                return None
            start = i + 4
            continue
        size = int.from_bytes(header[i - 4:i], 'big')
        # size counts the 8-byte header, so the body ends where the next box
        # starts.
        body = header[i + 4:i - 4 + size]
        if len(body) >= 4 and body[0] == 1:
            return 'avc1.%02x%02x%02x' % (body[1], body[2], body[3])
        start = i + 4


def avc_codec_of(source):
    """The fourcc this broadcast is really carrying, or '' while unknown.

    One function because three readers ask the same question -- the page's
    `addSourceBuffer` call, the viewing overlay, and the settings card -- and
    AGENTS.md §4.8's red line is that two readers must not each keep their own
    copy of an answer. A byte log (the DLNA file shape) and the WebRTC bridge
    have no init segment, so they answer '' and the rows built from this stay
    quiet instead of showing a guess.
    """
    header = getattr(source, 'init_segment', b'') if source is not None else b''
    return avc_codec_string(header or b'') or ''


def live_codecs(session, broadcaster):
    """What to hand the page's `MediaSource`/`addSourceBuffer` call.

    The real fourcc once the encoder has produced its init segment, the pinned
    default until then. This is called while serving the page, which is after
    `_serve_stream`'s `await_init()` on any viewer that actually plays -- so in
    practice a browser that opens the URL gets the truth, and the fallback is
    what a page fetched in the first few hundred milliseconds is left with.
    """
    return codecs_for(avc_codec_of(broadcaster) or MSE_VIDEO_CODEC_DEFAULT,
                      getattr(session, 'has_audio', False))


class _Session(object):
    """What this mirror session looks like to the HTTP server."""

    def __init__(self, kind, has_audio=False, title='Macast', profile=None,
                 bitrate=None, max_seconds=None):
        self.kind = kind if kind in OUTPUTS else DEFAULT_OUTPUT
        self.label, self.suffix, self.content_type, _args = OUTPUTS[self.kind]
        #: Whether this session's container carries an audio track at all. The
        #: browser target's queue is sized against its fragment cadence, and an
        #: audio track doubles that cadence -- see `fragment_rate`.
        self.has_audio = bool(has_audio)
        #: The encoder's target video rate, in bits/s. The queue in front of a
        #: live consumer is sized from it -- see `live_queue_chunks`.
        self.bitrate = bitrate
        #: The ceiling this session was started under, in seconds (None = this
        #: shape has none). Asked once, by the run, and handed down: the argv's
        #: `-t` and the file shape's advertised length are both computed from
        #: this value, so a settings edit made mid-session cannot leave the
        #: television promised a length the encoder no longer writes.
        self.max_seconds = max_seconds
        #: The DLNA target answers as a finite file, so it needs a profile
        #: (which container, which codec, which advertised size) and a byte log
        #: instead of the queue broadcaster. The other targets leave both None
        #: and keep the live-edge semantics.
        self.profile = None
        self.bytelog = False
        self.shape_forced = False
        self.file_anchor = None
        #: How many stream/player requests this session has answered. The first
        #: few are written to the log (see _StreamHandler.EXCHANGE_LOG_LIMIT):
        #: a renderer that refuses the stream makes its case by retrying, and
        #: that count is the difference between "one clean fetch" and "eight
        #: connections that each gave up".
        self.exchanges = 0
        #: Bytes actually handed to a socket, counted where they leave -- the
        #: two handlers. This is the only number that answers "is anyone
        #: watching this", and neither of the two byte counters below can stand
        #: in for it: `_Broadcaster.bytes` and `_ByteLog.bytes` count what the
        #: *encoder* produced, which keeps growing at full rate after a viewer
        #: opened the stream, failed to parse it, and quit. A watchdog reading
        #: those sees "alive" over a black screen.
        self.written = 0
        self._written_lock = threading.Lock()
        if self.kind == 'dlna':
            self.profile = profile or dlna_profile()
            self.suffix = self.profile.suffix
            self.content_type = self.profile.content_type
            #: Only the file shape keeps a byte log: it is the one that has to
            #: answer absolute offsets. The live shape serves from the queue in
            #: front of the encoder, exactly like the other live targets -- and
            #: a `_ByteLog` has no `subscribe`, so getting this wrong is not a
            #: slow mirror but a handler that throws on the first request.
            self.bytelog = dlna_shape() == DLNA_SHAPE_FILE
            #: ... unless this profile's container cannot be read that way. A
            #: byte log starts the advertised file `prefill` bytes behind the
            #: live edge, so the bytes that name the codecs are in no range a
            #: renderer can ask for. MPEG-PS and MPEG-TS ride that out because
            #: any packet decodes on its own; Matroska does not -- measured, a
            #: viewer that joined one mid-file sat in mpv's demuxer probe
            #: forever. The shape that *can* hand over a header wins, and says
            #: so (`dlna_shape_words`) rather than leaving the user to notice
            #: that the setting they chose is not what is happening.
            self.shape_forced = self.bytelog and profile_needs_header(
                self.profile)
            if self.shape_forced:
                self.bytelog = False
                logger.warning(
                    'screen_mirror: %s is a container that must be read from '
                    'its header, so the "pretend to be a file" shape is served '
                    'as a live stream instead', dlna_profile_id(self.profile))
            self.file_size = self.file_duration = None
            if self.bytelog:
                self.file_size, self.file_duration = advertised_file(
                    self.profile.total_bitrate, self.max_seconds)
        #: A browser can only attach to a live fragmented stream at a
        #: keyframe, so replaying the header plus a short tail is what makes
        #: "open the URL a second time" work. A TV is never replayed: it would
        #: inherit a backlog and sit seconds behind for the rest of the session.
        #: `frag_every_frame` (see `OUTPUTS`) means the newest fragment usually
        #: is *not* a keyframe, so `_Broadcaster.tail` cuts the replay back to
        #: the newest one that is.
        self.replay = self.kind == 'browser'
        self.init_marker = b'moof' if self.kind == 'browser' else None
        #: The one-time container header, written onto *every* connection.
        #: Distinct from `replay`: a late joiner must get the header without
        #: also getting a backlog. Only the live shapes need it -- and only the
        #: containers that cannot be read without it (see `MKV_FIRST_CLUSTER`).
        if (self.kind == 'dlna' and not self.bytelog
                and profile_needs_header(self.profile)):
            self.init_marker = MKV_FIRST_CLUSTER
        self.send_init = self.init_marker is not None
        self.stream_id = secrets.token_hex(8)
        #: Both credentials are per-session secrets and both die with the
        #: mirror. Deliberately *not* the app's stable management token: that
        #: one also opens the whole management API (AGENTS.md 4.7), so pasting
        #: a viewing URL would leak a credential that stays valid afterwards.
        self.page_token = secrets.token_hex(8)
        #: A MediaSource codec string, in the only form `addSourceBuffer`
        #: accepts: a MIME type with a quoted codec list. The bare
        #: `avc1.640028,mp4a.40.2` throws NotSupportedError, which used to
        #: silently drop every viewer onto the progressive fallback -- and a
        #: live fragmented MP4 has no playable duration there, so Safari showed
        #: a black page.
        #:
        #: This is the *fallback* the page gets when it arrives before the
        #: encoder has written its header; `_serve_page` asks `live_codecs` for
        #: the fourcc off the real avcC box, because this default turned out to
        #: describe one shape out of the seven the plugin can ship (see
        #: `MSE_VIDEO_CODEC_DEFAULT`).
        self.codecs = codecs_for(MSE_VIDEO_CODEC_DEFAULT, has_audio)
        self.page_title = title
        #: What this session *is* -- encoder, capture, budgets -- as
        #: `_session_diagnostics` computed it. The settings card reaches it
        #: through the renderer; the viewing page's overlay can only reach what
        #: hangs off the session, because its handler holds a session and a
        #: server, not a renderer. `{}` until the run attaches it, which is why
        #: the overlay draws the rows it has and stays quiet about the rest.
        self.diag = {}

    def stream_name(self, suffix=None):
        return '{}.{}'.format(self.stream_id, suffix or self.suffix)

    def stream_path(self):
        return '{}{}'.format(STREAM_PREFIX, self.stream_name())

    def note_written(self, count):
        """Record bytes that actually left on a socket.

        Called from the two stream handlers, never from the encoder side. This
        is what the DLNA watchdog asks about when it wants to know whether
        anyone is watching: production keeps going at full rate after a viewer
        opened the stream, failed to parse it, and quit, so only the bytes that
        reached a peer say something about the picture on the television.
        """
        if count:
            with self._written_lock:
                self.written += count


def live_queue_chunks(bitrate):
    """How many 4 KiB reads a live consumer's queue may hold.

    A fixed chunk count is a fixed number of *bytes*, which at these bitrates
    is anywhere between half a second and a second and a half of standing
    latency -- the queue is the sender's own contribution to the delay the user
    complains about, and it should be budgeted in seconds, not bytes. Clamped
    at both ends: below ~64 reads a 2 Mbps preset cannot survive one TCP
    stall, and nothing needs a second and a half of backlog to look smooth.

    This is the answer only for the containers that queue reads. The two framed
    shapes queue whole units and go through `live_queue_units` instead.
    """
    if not bitrate:
        return LIVE_QUEUE_MAX_CHUNKS
    budget = int(bitrate) * LIVE_QUEUE_SECONDS // 8
    return max(LIVE_QUEUE_MIN_CHUNKS,
               min(LIVE_QUEUE_MAX_CHUNKS, budget // CHUNK))


def fragment_rate(fps=FPS, has_audio=True):
    """How many fragments one second of live fragmented MP4 puts on the wire.

    `frag_every_frame` is a per-**sample** cadence, not a per-picture one: the
    audio track is muxed in the same flush, so a browser session carrying system
    sound at 24 fps writes 24 + 46.875 `moof` boxes a second. Budgeting the
    queue against `fps` alone therefore buys a third of the slack that was asked
    for, and budgeting it against a hard-coded eight buys a sixth.

    Measured on a real session served by the shipping code (2026-10-02, Windows
    source, 24 fps with sound): 1063 fragments in 15.02 s of stream -- 70.8 a
    second, 8 835 bytes each, which is the same answer the arithmetic gives.
    """
    rate = float(fps or FPS)
    return rate + (AAC_FRAMES_PER_SECOND if has_audio else 0.0)


def live_queue_units(session, bitrate=None):
    """The live queue's capacity, counted in the unit this container queues.

    One budget in seconds (`LIVE_QUEUE_SECONDS`), three different units:

    - unframed (MPEG-TS / MPEG-PS): 4 KiB reads, so `live_queue_chunks`.
    - fragmented MP4 (the browser target): whole fragments, and the fragment
      cadence is `fragment_rate` -- at 24 fps with sound that is 54 of them,
      where the queue used to hold eight.
    - live Matroska (a DLNA profile that must be read from its header): whole
      clusters, and the muxer closes those by size, measured at 50-60 KB, so
      the seconds are spent against `MKV_CLUSTER_BYTES`.

    The `init_marker` the broadcaster frames by is the same value this reads, so
    the unit the queue is counted in and the unit the framer emits are decided
    together or not at all.

    `bitrate` is the caller's override for the case where the rate is known from
    somewhere other than the session -- the diagnostics card reads it out of the
    argv that is actually running. Both answers must come from *here*, so the
    override is a parameter rather than a second copy of the arithmetic.
    """
    if bitrate is None:
        bitrate = session.bitrate
    if session.init_marker == b'moof':
        units = int(math.ceil(fragment_rate(FPS, session.has_audio)
                               * LIVE_QUEUE_SECONDS))
        return max(LIVE_QUEUE_MIN_UNITS, units)
    if session.init_marker == MKV_FIRST_CLUSTER:
        if not bitrate:
            return LIVE_QUEUE_MIN_UNITS
        units = int(math.ceil(bitrate * LIVE_QUEUE_SECONDS / 8.0
                              / MKV_CLUSTER_BYTES))
        return max(LIVE_QUEUE_MIN_UNITS, units)
    return live_queue_chunks(bitrate)


def queue_hold_seconds(session, units, bitrate=None):
    """What `live_queue_units` capacity is actually worth, in seconds.

    The inverse of the same arithmetic, for the two places that show it:
    the「统计信息」card and the viewing page's overlay. Framed units are divided
    by their cadence, queued reads by the bitrate, so a card that says 0.75 s
    means 0.75 s of picture rather than 0.75 s of whatever the read size was.

    The fMP4 branch needs no bitrate -- a fragment is a frame, and frames come at
    a fixed rate. The other two are byte-shaped, so without a rate to divide by
    they answer None: a duration invented from an unknown bitrate is the number
    this card exists to stop people guessing at.
    """
    if units is None:
        return None
    if bitrate is None:
        bitrate = session.bitrate
    if session.init_marker == b'moof':
        return round(units / fragment_rate(FPS, session.has_audio), 2)
    if not bitrate:
        return None
    if session.init_marker == MKV_FIRST_CLUSTER:
        return round(units * MKV_CLUSTER_BYTES * 8.0 / bitrate, 2)
    return round(units * CHUNK * 8.0 / bitrate, 2)


def start_stream_server(session, broadcaster=None):
    server = ThreadingHTTPServer(('0.0.0.0', 0), _StreamHandler)
    server.session = session
    if broadcaster is None:
        broadcaster = _ByteLog() if session.bytelog else _Broadcaster(
            init_marker=session.init_marker,
            maxsize=live_queue_units(session),
            packet_align=TS_PACKET if session.kind == 'cast' else None)
    server.broadcaster = broadcaster
    server.daemon_threads = True
    # A client that vanished mid-stream stays in queue.get for seconds;
    # server_close must not wait for it.
    server.block_on_close = False
    threading.Thread(target=server.serve_forever, daemon=True,
                     name="SCREEN_MIRROR_HTTP").start()
    return server


def stream_url(server):
    return 'http://{}:{}{}'.format(advertise_host(), server.server_address[1],
                                   server.session.stream_path())


def page_url(server):
    return 'http://{}:{}{}?token={}'.format(
        advertise_host(), server.server_address[1], BROWSER_PATH,
        server.session.page_token)


def webrtc_page_url(server):
    """The WebRTC target's viewing address, next to its browser cousin."""
    return 'http://{}:{}{}?token={}'.format(
        advertise_host(), server.server_address[1], WEBRTC_PATH,
        server.session.page_token)


def _flag_value(command, flag):
    """The token that follows `flag` in an argv list, or None.

    Same refusal as `_int_flag`: a command where the flag is the last token
    answers None rather than guessing. This one returns the string because
    `-c:v` is not a number.
    """
    if flag in command:
        try:
            return str(command[command.index(flag) + 1])
        except IndexError:
            return None
    return None


def _int_flag(command, flag):
    """The integer that follows `flag` in an argv list, or None.

    No exception escapes: a command that carries `-g` as its last token, or a
    value that is not a bare number (`-r 25.000` would do), answers None --
    which is what the「统计信息」card wants. A missing fact is displayable; a
    half-parsed one is a lie with a number in it.
    """
    if flag in command:
        try:
            return int(command[command.index(flag) + 1])
        except (IndexError, TypeError, ValueError):
            return None
    return None


def _session_diagnostics(kind, capture, command, encoder, height, bitrate,
                         session, audio_expected=True, refused=''):
    """The raw facts of one session, for the「统计信息」card.

    Numbers and identifiers only -- what ffmpeg was actually told, which tap and
    encoder the probe picked, and the budgets whose *sum* is the delay the user
    feels. The view layer turns these into rows and sentences, so this stays a
    plain dict the regression suite can assert on with no display and no
    browser.

    Deliberately not a copy of `stats()`: that one is a live throughput read
    from the broadcaster, this one is what the session *is*. The card shows
    both, and they answer different questions ("it is dropping" vs "it was
    always going to be 4 s behind on this target").
    """
    #: Which of the two DLNA shapes this session answers with decides what the
    #: sender's own delay *is*: the live shape serves out of the queue in front
    #: of the encoder, the file shape holds a prefill back before the TV is
    #: given the URL at all. Reporting the queue for both and the prefill only
    #: where it is actually paid keeps the「统计信息」card from naming a budget
    #: this session never spent.
    live_queue = not session.bytelog
    #: The queue's capacity and what it is worth, both from the two functions
    #: `start_stream_server` builds the broadcaster with. Until 2026-10 this
    #: pair was computed from `live_queue_chunks(bitrate)` alone, which is the
    #: budget of the *unframed* shapes: on a browser session the card said
    #: 0.75 s while the queue actually held eight fragments, i.e. 0.11 s.
    queue_units = (live_queue_units(session, bitrate)
                   if (live_queue and bitrate) else None)
    info = {
        'kind': kind,
        'capture': capture.label,
        'audio': bool(capture.audio_map),
        'audio_map': capture.audio_map or '',
        'audio_expected': bool(audio_expected),
        'encoder': encoder,
        #: The name that actually went into `-c:v`, read off the argv that is
        #: running rather than derived from the switch. `encoder` above is the
        #: user's *wish* ('hardware'), which means VideoToolbox on a Mac, NVENC
        #: on a Windows box, and -- when the probe refused it -- libx264 with a
        #: warning in the log. A card that only ever said「硬件编码
        #: （VideoToolbox）」was stating macOS's dialect about a machine in
        #: another one, and hiding the fallback.
        'encoder_name': _flag_value(command, '-c:v') or '',
        'height': height,
        'bitrate': bitrate,
        #: Both of these are read out of the argv that is actually running
        #: rather than recomputed from the target's defaults. A DLNA session is
        #: framed by its profile -- `_mpeg2` and the H.264 branch each write
        #: their own `-r` and `-g` -- so a card that quoted `FPS` and
        #: `gop_size(kind)` would name a cadence this encoder was never told to
        #: keep, which is the one thing this dict exists to be honest about.
        'fps': _int_flag(command, '-r') or FPS,
        'gop': _int_flag(command, '-g'),
        #: The ceiling the encoder was told to stop at, read off the same argv.
        #: This is the answer to "why did the mirror stop after N hours" -- and
        #: it is `None` (no row) for the three shapes that have no ceiling,
        #: which is why it is worth printing rather than deriving: the setting
        #: exists on every machine, the `-t` does not.
        'max_seconds': _int_flag(command, '-t'),
        'command': ' '.join(command),
        'cast_refused': str(refused or ''),
        'queue_chunks': queue_units,
        #: How much picture the sender's own queue may hold before the
        #: slowest viewer starts losing units -- the sender's share of the
        #: delay, in seconds so it can be read next to the rest. `queue_chunks`
        #: is that same capacity in the unit the container queues (4 KiB reads,
        #: fragments, or clusters), which is why the two are derived together.
        'queue_seconds': queue_hold_seconds(session, queue_units, bitrate),
        #: Only the browser target replays a backlog; the other live targets
        #: follow the live edge or are byte-addressed by the TV itself.
        'replay_bytes': REPLAY_BYTES if kind == 'browser' else 0,
    }
    if kind == 'webrtc':
        # The bridge's queue is counted in access units, not byte chunks: the
        # sender's share of this target's delay is these units at 1/FPS each
        # (the unit itself was already encoded; this is only the part that
        # queues here before SRTP leaves).
        info.update({
            'queue_chunks': WEBRTC_QUEUE_UNITS,
            'queue_seconds': round(WEBRTC_QUEUE_UNITS / float(FPS), 2),
        })
    if kind == 'dlna':
        profile = session.profile
        info.update({
            'profile': dlna_profile_id(profile),
            'profile_bitrate': profile.total_bitrate,
            'prefill_bytes': (dlna_prefill_bytes(profile)
                              if session.bytelog else 0),
            'prefill_seconds': (dlna_prefill_seconds(profile)
                                if session.bytelog else 0),
        })
    return info


# -- what the viewing page's overlay is allowed to say ---------------------

#: The subset of `_session_diagnostics` the browser overlay shows. Deliberately
#: a subset: `command` is the whole ffmpeg argv -- a paragraph the settings
#: card can copy out, and nothing the corner of a video has room for. A key
#: that is absent (a DLNA-only row on a browser session) simply draws no row.
PAGE_DIAG_KEYS = ('kind', 'capture', 'encoder', 'encoder_name', 'height',
                  'fps', 'gop',
                  'bitrate', 'queue_chunks', 'queue_seconds', 'replay_bytes',
                  'audio', 'audio_expected', 'audio_map', 'profile',
                  'profile_bitrate', 'prefill_seconds', 'cast_refused')


def page_diag(session):
    """The session's own facts, as the viewing page should receive them."""
    diag = getattr(session, 'diag', None) or {}
    return {k: diag[k] for k in PAGE_DIAG_KEYS if k in diag}


def page_stats(session, source):
    """The live counters of one running mirror, for the overlay's poll.

    A separate shape from `ScreenMirrorRenderer.stats()` rather than a call to
    it: that one is the settings card's answer, and it asks the *renderer* --
    which this handler does not hold. Both read the same two objects (the
    session and whatever is broadcasting its bytes), so the numbers cannot
    drift; what they must not do is each keep their own copy of one.
    """
    live = {
        'written': getattr(session, 'written', 0),
        'exchanges': getattr(session, 'exchanges', 0),
        'shape': 'file' if getattr(session, 'bytelog', False) else 'live',
    }
    #: `sent` is the WebRTC bridge's delivered-byte counter -- the same fact
    #: `written` is on a byte stream, under the name the bridge has room for.
    #: A byte-stream broadcaster does not have it, and answers 0, which the
    #: browser overlay never shows (only the WebRTC page reads it).
    for name in ('chunks', 'bytes', 'drops', 'sent'):
        live[name] = getattr(source, name, 0) if source is not None else 0
    #: A replay that found no keyframe to start on is invisible in `drops` --
    #: nothing was dropped, the viewer simply got no pre-roll and waits for the
    #: next IDR. It used to exist only in the log, which is the one place the
    #: person watching a late-joining viewer stutter does not look.
    live['misses'] = getattr(source, 'keyframe_misses', 0) if source is not None else 0
    #: Units queued unsent in front of the viewers right now. Only the queue
    #: broadcaster has an answer; the byte log is addressed by offset and sheds
    #: nothing, so it reports no depth rather than a zero that reads as "empty".
    depth = getattr(source, 'queued', None) if source is not None else None
    live['queued'] = depth() if callable(depth) else None
    clients = getattr(source, 'clients', None) if source is not None else None
    live['clients'] = clients() if callable(clients) else 0
    #: The H.264 fourcc off the encoder's own avcC box, '' while the header has
    #: not been written. Both statistics readers ask it of the same object
    #: through `avc_codec_of` (red line: one answer, not two).
    live['codec'] = avc_codec_of(source)
    return live


# -- the browser player page ------------------------------------------------
#
# Served by us, so the only substitutions are our own strings: a path, a codec
# label and a title. Nothing a sender controls reaches it (unlike the log
# panel's story in AGENTS.md 4.10), and it is textContent-only anyway.
#
# A raw string on purpose: the JS inside needs its own `'\n'`, and an
# unescaped literal would have Python eat the backslash first -- which is how
# a `SyntaxError: Invalid or unexpected token` once killed the whole overlay
# while every Python-side test stayed green.

PLAYER_PAGE = r"""<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>@TITLE@ 屏幕镜像</title>
<style>
 html,body{margin:0;height:100%;background:#000;color:#ccc;
   font:14px/1.5 -apple-system,system-ui,sans-serif}
 video{width:100%;height:100%;object-fit:contain;background:#000}
 #bar{position:fixed;left:0;right:0;top:0;display:flex;gap:8px;
   align-items:center;padding:6px 10px;background:#0008;color:#ddd;
   font-size:12px;opacity:.25;transition:opacity .3s}
 body:hover #bar{opacity:1}
 button{background:#222;color:#ddd;border:1px solid #444;border-radius:6px;
   padding:4px 10px;font:inherit}
 button.on{background:#28465f;border-color:#4d7ea8;color:#fff}
 #err{color:#f88;display:none}
 /* The推流 overlay: one textContent write per tick, so nothing here is ever
    parsed as markup. `pointer-events:none` keeps it from stealing a click on
    the video, which is the only way to unmute. */
 #stats{position:fixed;left:10px;top:38px;max-width:min(72ch,92%);
   background:#000c;color:#cfe3ff;font:11px/1.5 ui-monospace,Menlo,Consolas,
   monospace;padding:8px 10px;border-radius:6px;white-space:pre-wrap;
   overflow-wrap:break-word;pointer-events:none;display:none}
 #stats.on{display:block}
</style></head><body>
<div id="bar"><span id="st">连接中…</span><span id="err"></span>
 <span style="flex:1"></span>
 <button id="snd">开声音</button><button id="stat">统计</button>
 <button id="full">全屏</button></div>
<div id="stats"></div>
<video id="v" autoplay playsinline muted></video>
<script>
var STREAM='@STREAM@',CODECS='@CODECS@',LIVE_EDGE=@LIVE_EDGE@,STALL_MS=8000;
var DIAG=@DIAG@||{};
var v=document.getElementById('v'),st=document.getElementById('st'),
    err=document.getElementById('err'),panel=document.getElementById('stats'),
    toggle=document.getElementById('stat');
// The overlay's own credential: the same per-session token the page was opened
// with, read back out of the URL. Never the app's stable management token.
var TOKEN=(location.search.match(/[?&]token=([^&]+)/)||[,''])[1];
var STATS='/browser/stats?token='+encodeURIComponent(TOKEN);
function say(t){st.textContent=t}
function fail(t){err.style.display='inline';err.textContent=' · '+t}
function lock(){if(navigator.wakeLock)navigator.wakeLock.request('screen')
  .catch(function(){})}
function unmute(){v.muted=false;document.getElementById('snd').style.display=
  'none'}
document.getElementById('snd').onclick=unmute;
document.getElementById('full').onclick=function(){
  (v.requestFullscreen||v.webkitRequestFullscreen||function(){}).call(v)};

// -- the overlay: what the sender produced, and what this player did with it --
var MB=1048576;
function s(x){return (x===undefined||x===null||x==='')?'—':String(x)}
function mb(b){return b?((b/MB).toFixed(1)+' MB'):'0 MB'}
function rate(b){return b?(b*8/1e6).toFixed(2)+' Mbps':'0 Mbps'}
// The sender's preset speaks bits per second, its traffic counters bytes per
// second. One row reads each, so the two formatters must not be confused.
function bps(b){return b?(b/1e6).toFixed(2)+' Mbps':'0 Mbps'}
function secs(x){return (x===null||x===undefined)?'—':Number(x).toFixed(2)+' s'}
function line(a,b){return a+'：'+b}
// Everything the tick reads lives here so the reader loop can update it without
// reaching into a closure it does not own.
var M={bytes:0,pieces:0,qdepth:0,last:0,instant:0};
var live={};          // the sender's counters, from the poll
var shown=0;          // when the panel last painted
function rows(){
  var out=[],d=DIAG;
  out.push('— 发送端 —');
  out.push(line('目标',s(d.kind))+' · '+line('形状',s(live.shape)));
  if(d.capture!==undefined)out.push(line('采集',d.capture));
  out.push(line('声音',d.audio?(d.audio_map||'有')
                 :(d.audio_expected?'已弃（保画面）':'无')));
  if(d.encoder!==undefined)out.push(line('编码器',d.encoder_name||d.encoder)+' · '
    +s(d.height)+'p@'+s(d.fps)+' · GOP '+s(d.gop));
  // The fourcc the encoder really wrote into its avcC box, not the one this
  // page was compiled with: addSourceBuffer() was handed that string a moment
  // ago, so a mismatch reads as a black player rather than as a wrong label.
  if(live.codec)out.push(line('H.264 Profile',live.codec));
  if(d.bitrate)out.push(line('档位码率',bps(d.bitrate)));
  if(d.queue_seconds!==undefined&&d.queue_seconds!==null)
    out.push(line('发送端队列',secs(d.queue_seconds)
                  +(d.queue_chunks?' ('+d.queue_chunks+' 块)':'')));
  if(d.prefill_seconds)out.push(line('预填',secs(d.prefill_seconds)));
  if(d.replay_bytes)out.push(line('重播缓冲',mb(d.replay_bytes)));
  if(d.cast_refused)out.push(line('回落原因',d.cast_refused));
  out.push('— 推流（发送端实时）—');
  out.push(line('编码器产出',s(live.chunks)+' 块 · '+mb(live.bytes)));
  out.push(line('发送端丢块',s(live.drops)
                +(live.drops?'（慢消费者被丢整块）':'')));
  // Two facts `drops` cannot carry: how much is queued unsent right now (a
  // full queue means the next drop is already decided), and how many viewers
  // joined a ring that held no keyframe and are waiting for one instead of
  // being handed a smear. Both were log-only until 2026-10.
  if(live.queued!==null&&live.queued!==undefined)
    out.push(line('队列占用',s(live.queued)
                  +(d.queue_chunks?' / '+d.queue_chunks+' 块':'')));
  if(live.misses)out.push(line('重播等关键帧',s(live.misses)+' 次'));
  out.push(line('已交付',mb(live.written))+' · '+line('观看端',s(live.clients)));
  out.push('— 播放器（本机）—');
  out.push(line('接收码率',rate(M.instant)));
  out.push(line('已接收',mb(M.bytes))+' · '+s(M.pieces)+' 片 · 积压 '
              +s(M.qdepth));
  var ahead=null,fill=null;
  try{var b=v.buffered;if(b.length){
    ahead=b.end(b.length-1)-v.currentTime;
    fill=b.end(b.length-1)-b.start(0)}}catch(x){}
  out.push(line('距直播边缘',secs(ahead))+' · '+line('已缓冲',secs(fill)));
  out.push(line('帧率',(Q.dec==='—'?'—':Q.dec+' fps 解码')+' / '
                +(Q.fps==='—'?'—':Q.fps+' fps 上屏')
                +(Q.dec>=6&&Q.fps!=='—'&&Q.fps*2<Q.dec
                  ?'（窗口不在前台，上屏数不算链路）':''))
              +' · '+line('掉帧',Q.dropped+'/'+Q.total));
  out.push(line('解码',v.videoWidth?v.videoWidth+'×'+v.videoHeight:'—')
              +' · '+line('画面',v.clientWidth+'×'+v.clientHeight));
  out.push(line('最近一片',M.last?(Date.now()-M.last)+' ms 前':'—'));
  out.push(new Date().toLocaleTimeString());
  return out.join('\n')}
function paint(){panel.textContent=rows();shown=Date.now()}
var Q={fps:'—',dec:'—',dropped:0,total:0};
// Two frame rates, because they answer different questions: what the decoder
// is being fed, and what this window actually got on screen. A background or
// occluded window presents at a couple of fps while the decode side stays at
// the encoder's rate -- reporting only the second number blames the mirror.
var dmark=Date.now(),dtotal=null;
setInterval(function(){
  var q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();if(!q)return;
  var n=Date.now(),dt=n-dmark;
  if(dtotal!==null&&dt>0)Q.dec=Math.round((q.totalVideoFrames-dtotal)*1000/dt);
  dtotal=q.totalVideoFrames;dmark=n},1000);
if(v.requestVideoFrameCallback){ // what actually reached the screen
  var fcount=0,frate=0,fmark=Date.now();
  var tick=function(){fcount++;v.requestVideoFrameCallback(tick)};
  v.requestVideoFrameCallback(tick);
  setInterval(function(){var n=Date.now();
    if(n-fmark>0){Q.fps=Math.round(fcount*1000/(n-fmark))}
    fcount=0;fmark=n},1000)}
function poll(){fetch(STATS).then(function(r){
    if(!r.ok)throw new Error(''+r.status);return r.json()}).then(function(j){
      live=j;if(panel.classList.contains('on'))paint()})
    .catch(function(){})}
var poller=null;
function setOpen(on){
  panel.classList.toggle('on',on);toggle.classList.toggle('on',on);
  try{localStorage.setItem('macast.mirror.stats',on?'1':'0')}catch(x){}
  if(poller){clearInterval(poller);poller=null}
  if(on){poll();paint();poller=setInterval(poll,1000)}}
toggle.onclick=function(){setOpen(!panel.classList.contains('on'))};
var stored=null;try{stored=localStorage.getItem('macast.mirror.stats')}catch(x){}
setOpen(stored==='1');
// A rate needs a window; the reader only counts bytes, this turns the
// difference between two reads into bits per second.
var win_at=Date.now(),win_bytes=0;
setInterval(function(){
  var n=Date.now(),dt=n-win_at;
  if(dt>0){M.instant=Math.round((M.bytes-win_bytes)*1000/dt);
    win_at=n;win_bytes=M.bytes}
},1000);
setInterval(function(){ // keep the panel honest while it is open
  if(!panel.classList.contains('on'))return;
  var q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();
  if(q){Q.dropped=q.droppedVideoFrames;Q.total=q.totalVideoFrames}
  paint()},1000);

// Autoplay policy: start muted, then any real gesture is consent to unmute.
['pointerdown','keydown','touchstart'].forEach(function(e){
  addEventListener(e,unmute,{once:true})});
function tail(){ // keep the buffer trimmed to the live edge
  try{var b=v.buffered;if(b.length&&v.duration){
    var end=b.end(b.length-1);
    if(end-v.currentTime>LIVE_EDGE)v.currentTime=end-LIVE_EDGE}}catch(x){}}
function progressive(why){ // MSE missing or wedged: let the element stream it
  say('回退到渐进式播放');if(why)fail(why);
  v.src=STREAM+'#t=0.001';v.play().catch(function(){});
  v.ontimeout=function(){location.reload()}}
function mse(){
  if(!window.MediaSource||!MediaSource.isTypeSupported(CODECS)){
    return progressive('这个浏览器不支持 MSE')}
  var ms=new MediaSource();
  ms.addEventListener('sourceopen',run,{once:true});
  v.src=URL.createObjectURL(ms);
  var timer=setTimeout(function(){ // sourceopen sometimes never fires on
    if(ms.readyState!=='open')progressive('MSE 未打开')},1500);
  function run(){
    clearTimeout(timer);var sb;
    try{sb=ms.addSourceBuffer(CODECS)}catch(e){
      return progressive('编码器不认：'+e.message)}
    sb.mode='sequence';var queue=[],last=Date.now();
    say('正在镜像');lock();
    fetch(STREAM).then(function(r){
      if(!r.ok||!r.body)throw new Error('http '+r.status);
      var rd=r.body.getReader();
      function pull(){return rd.read().then(function(x){
        if(!x.done){last=Date.now();M.last=last;
          M.bytes+=x.value.length;M.pieces++;M.qdepth=queue.length;
          if(sb.updating)queue.push(x.value);else sb.appendBuffer(x.value);
          return pull()}
        try{if(ms.readyState==='open')ms.endOfStream()}catch(e){}})}
        return pull()}).catch(function(e){fail('断开：'+e.message);
        setTimeout(function(){location.reload()},2000)});
    sb.addEventListener('updateend',function(){
      last=Date.now();M.last=last;M.qdepth=queue.length;
      if(queue.length)sb.appendBuffer(queue.shift());else tail()});
    setInterval(function(){ // watchdogs: stall, or a decoder that ate everything
      if(Date.now()-last>STALL_MS){fail('画面卡住，重连');location.reload()}
    },2000);
    v.play().catch(function(){})
  }
}
mse();
</script></body></html>
"""


# -- the WebRTC player page -------------------------------------------------
#
# The fifth target's viewing page. Same credential, same overlay counters,
# same textContent-only rules as PLAYER_PAGE above -- what changes is the
# media path: one RTCPeerConnection instead of an MSE fetch loop. The page
# fetches the sender's offer from `/webrtc/session`, answers it, and the
# track it receives *is* the encoder's H.264 re-packetized for RTP -- no
# re-encode anywhere on this path (that is the whole point of the target).
#
# Raw string on purpose, same as PLAYER_PAGE: the JS has its own `'\n'`. The
# token is read back out of the URL rather than repeated in the body -- a
# copy down here would outlive the address bar it was opened from.
WEBRTC_PAGE = r"""<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>@TITLE@ 屏幕镜像 · WebRTC</title>
<style>
 html,body{margin:0;height:100%;background:#000;color:#ccc;
   font:14px/1.5 -apple-system,system-ui,sans-serif}
 video{width:100%;height:100%;object-fit:contain;background:#000}
 #bar{position:fixed;left:0;right:0;top:0;display:flex;gap:8px;
   align-items:center;padding:6px 10px;background:#0008;color:#ddd;
   font-size:12px;opacity:.25;transition:opacity .3s}
 body:hover #bar{opacity:1}
 button{background:#222;color:#ddd;border:1px solid #444;border-radius:6px;
   padding:4px 10px;font:inherit}
 button.on{background:#28465f;border-color:#4d7ea8;color:#fff}
 #err{color:#f88;display:none}
 #mute{color:#f8b46c}
 #stats{position:fixed;left:10px;top:38px;max-width:min(72ch,92%);
   background:#000c;color:#cfe3ff;font:11px/1.5 ui-monospace,Menlo,Consolas,
   monospace;padding:8px 10px;border-radius:6px;white-space:pre-wrap;
   overflow-wrap:break-word;pointer-events:none;display:none}
 #stats.on{display:block}
</style></head><body>
<div id="bar"><span id="st">连接中…</span><span id="err"></span>
 <span id="mute">此通道无声音</span>
 <span style="flex:1"></span>
 <button id="stat">统计</button><button id="full">全屏</button></div>
<div id="stats"></div>
<video id="v" autoplay playsinline muted></video>
<script>
var DIAG=@DIAG@||{};
var v=document.getElementById('v'),st=document.getElementById('st'),
    err=document.getElementById('err'),panel=document.getElementById('stats'),
    toggle=document.getElementById('stat');
// The page's own credential, read back out of its URL. The body must not
// repeat it, and it is never the app's stable management token.
var TOKEN=(location.search.match(/[?&]token=([^&]+)/)||[,''])[1];
var STATS='/browser/stats?token='+encodeURIComponent(TOKEN);
function say(t){st.textContent=t}
function fail(t){err.style.display='inline';err.textContent=' · '+t}
function unlock(){if(navigator.wakeLock)navigator.wakeLock.request('screen')
  .catch(function(){})}
document.getElementById('full').onclick=function(){
  (v.requestFullscreen||v.webkitRequestFullscreen||function(){}).call(v)};

// -- the negotiation: one offer in, one answer out -------------------------
// No ICE server: this is a LAN, host candidates are the whole story, and a
// STUN round trip to the internet would only delay first paint on exactly
// the networks where it would fail. Host candidates arrive as mDNS names
// (the browser hides local IPs from pages); aiortc resolves those itself.
if(!window.RTCPeerConnection){fail('这个浏览器不支持 WebRTC')}
var pc=window.RTCPeerConnection?new RTCPeerConnection({iceServers:[]}):null;
function post(url,body){return fetch(url,{method:'POST',
  headers:{'Content-Type':'application/json'},
  body:JSON.stringify(body||{})}).then(function(r){
    return r.json().catch(function(){throw new Error('http '+r.status)})
    .then(function(j){if(!r.ok||j.code!==0)
      throw new Error(j.message||('http '+r.status));return j})})}
function gathered(){return new Promise(function(res){
  if(pc.iceGatheringState==='complete')return res();
  var t=setTimeout(res,2000);
  pc.addEventListener('icegatheringstatechange',function(){
    if(pc.iceGatheringState==='complete'){clearTimeout(t);res()}})})}
function negotiate(){
  if(!pc)return;
  say('正在协商…');
  var peer='';
  post('/webrtc/session?token='+encodeURIComponent(TOKEN))
  .then(function(j){peer=j.peer;return pc.setRemoteDescription(j.offer)})
  .then(function(){return pc.createAnswer()})
  .then(function(a){return pc.setLocalDescription(a).then(gathered)})
  .then(function(){return post('/webrtc/answer?token='
    +encodeURIComponent(TOKEN)+'&peer='+encodeURIComponent(peer),
    {type:'answer',sdp:pc.localDescription.sdp})})
  .then(function(){say('等待画面…')})
  .catch(function(e){fail('协商失败：'+e.message)});
}
if(pc){
  pc.ontrack=function(ev){
    v.srcObject=ev.streams[0]||new MediaStream([ev.track]);
    v.play().catch(function(){});
    say('已连接');unlock()};
  pc.onconnectionstatechange=function(){
    if(pc.connectionState==='connected')say('已连接');
    if(pc.connectionState==='failed'){fail('连接中断，正在重连');
      setTimeout(function(){location.reload()},2000)}};
  negotiate();
}

// -- the overlay: the sender's counters, and what this page did with them --
var MB=1048576;
function s(x){return (x===undefined||x===null||x==='')?'—':String(x)}
function mb(b){return b?((b/MB).toFixed(1)+' MB'):'0 MB'}
function rate(b){return b?(b*8/1e6).toFixed(2)+' Mbps':'0 Mbps'}
function bps(b){return b?(b/1e6).toFixed(2)+' Mbps':'0 Mbps'}
function secs(x){return (x===null||x===undefined)?'—':Number(x).toFixed(2)+' s'}
function line(a,b){return a+'：'+b}
var live={};          // the sender's counters, from the same /browser/stats poll
var N={state:'new',w:0,h:0,fps:0,bytes:0,instant:0,lost:0,recv:0,
       rtt:'—',jitter:0};
var Q={fps:'—'};
function rows(){
  var out=[],d=DIAG;
  out.push('— 发送端 —');
  out.push(line('目标',s(d.kind))+' · '+line('形状',s(live.shape)));
  if(d.capture!==undefined)out.push(line('采集',d.capture));
  if(d.encoder!==undefined)out.push(line('编码器',d.encoder_name||d.encoder)+' · '
    +s(d.height)+'p@'+s(d.fps)+' · GOP '+s(d.gop));
  if(d.bitrate)out.push(line('档位码率',bps(d.bitrate)));
  if(d.queue_seconds!==undefined&&d.queue_seconds!==null)
    out.push(line('发送端队列',secs(d.queue_seconds)
                  +(d.queue_chunks?' ('+d.queue_chunks+' 块)':'')));
  out.push('— 推流（发送端实时）—');
  out.push(line('编码器产出',s(live.chunks)+' 块 · '+mb(live.bytes)));
  out.push(line('发送端丢块',s(live.drops)
                +(live.drops?'（慢消费者被丢整块）':'')));
  // Two facts `drops` cannot carry: how much is queued unsent right now (a
  // full queue means the next drop is already decided), and how many viewers
  // joined a ring that held no keyframe and are waiting for one instead of
  // being handed a smear. Both were log-only until 2026-10.
  if(live.queued!==null&&live.queued!==undefined)
    out.push(line('队列占用',s(live.queued)
                  +(d.queue_chunks?' / '+d.queue_chunks+' 块':'')));
  if(live.misses)out.push(line('重播等关键帧',s(live.misses)+' 次'));
  out.push(line('已交付',mb(live.sent))+' · '+line('观看端',s(live.clients)));
  out.push('— 连接（本机）—');
  out.push(line('连接',s(N.state))+' · '+line('解码',
              N.w?N.w+'×'+N.h:'—')+' @ '+s(N.fps)+' fps');
  out.push(line('接收码率',rate(N.instant))+' · '+line('已接收',mb(N.bytes)));
  out.push(line('丢包',s(N.lost)+'/'+s(N.recv))+' · '+line('往返时延',
              N.rtt==='—'?'—':N.rtt+' ms')+' · '+line('抖动',
              N.jitter?Math.round(N.jitter*1000)+' ms':'0 ms'));
  out.push(line('帧率',(N.fps===0?'—':N.fps+' fps 解码')+' / '
    +(Q.fps==='—'?'—':Q.fps+' fps 上屏')
    +(N.fps>=6&&Q.fps!=='—'&&Q.fps*2<N.fps
      ?'（窗口不在前台，上屏数不算链路）':'')));
  out.push(new Date().toLocaleTimeString());
  return out.join('\n')}
function paint(){panel.textContent=rows()}
// What actually reached the screen, next to what getStats says was decoded.
// A backgrounded or occluded window presents at a couple of fps while the
// decoder keeps the encoder's cadence -- one number alone blames the mirror.
if(v.requestVideoFrameCallback){
  var fcount=0,fmark=Date.now();
  var tick=function(){fcount++;v.requestVideoFrameCallback(tick)};
  v.requestVideoFrameCallback(tick);
  setInterval(function(){var n=Date.now();
    if(n-fmark>0){Q.fps=Math.round(fcount*1000/(n-fmark))}
    fcount=0;fmark=n},1000)}
// The connection's own numbers, straight out of getStats once a second.
var prev={bytes:0,at:Date.now()};
function net(){
  if(!pc)return;
  pc.getStats().then(function(rep){
    rep.forEach(function(x){
      if(x.type==='inbound-rtp'&&x.kind==='video'){
        N.w=x.frameWidth||N.w;N.h=x.frameHeight||N.h;
        N.fps=x.framesPerSecond||N.fps;
        N.lost=x.packetsLost||0;N.recv=x.packetsReceived||0;
        N.jitter=x.jitter||0;N.bytes=x.bytesReceived||0}
      if(x.type==='candidate-pair'&&x.state==='succeeded'
         &&x.currentRoundTripTime!==undefined){
        N.rtt=Math.round(x.currentRoundTripTime*1000)}});
    var n=Date.now(),dt=n-prev.at;
    if(dt>0){N.instant=Math.round((N.bytes-prev.bytes)*1000/dt)}
    prev={bytes:N.bytes,at:n};
    N.state=pc.connectionState;
    if(panel.classList.contains('on'))paint()}).catch(function(){})}
setInterval(net,1000);
function poll(){fetch(STATS).then(function(r){
    if(!r.ok)throw new Error(''+r.status);return r.json()}).then(function(j){
      live=j;if(panel.classList.contains('on'))paint()})
    .catch(function(){})}
var poller=null;
function setOpen(on){
  panel.classList.toggle('on',on);toggle.classList.toggle('on',on);
  try{localStorage.setItem('macast.mirror.stats',on?'1':'0')}catch(x){}
  if(poller){clearInterval(poller);poller=null}
  if(on){poll();net();paint();poller=setInterval(poll,1000)}}
toggle.onclick=function(){setOpen(!panel.classList.contains('on'))};
var stored=null;try{stored=localStorage.getItem('macast.mirror.stats')}catch(x){}
setOpen(stored==='1');
setInterval(function(){ // keep the panel honest while it is open
  if(panel.classList.contains('on'))paint()},1000);
</script></body></html>
"""


def advertise_host():
    """The address a TV on the LAN can reach this Mac on.

    `Setting.get_advertisable_ip()` is permissive on purpose -- it keeps every
    interface with a gateway entry, the `AF_LINK` ones included -- so on a
    machine running VMs or Tailscale it lists the VM bridges and the tunnel
    alongside the Wi-Fi address, and its first element is whatever the set
    happens to yield. Measured on that machine: 192.168.215.0, 192.168.97.0 and
    192.168.139.3 on three runs, none of which a phone can dial, while the real
    LAN address is 192.168.1.5. mDNS hit the same problem and answers it in
    `discovery.advertisable_addresses()` by keeping the interface that carries
    the IPv4 default route. Asking the core which of these addresses it would
    publish is what keeps the URL we hand a browser openable instead of merely
    printable -- the browser target has no peer to probe a route to, so this is
    the only judgement available to it.
    """
    try:
        addrs = Setting.get_advertisable_ip()
    except Exception:
        addrs = []
    if not addrs:
        return '127.0.0.1'
    reachable = [a for a in addrs if a in set(_reachable_hosts())]
    return (reachable or addrs)[0]


def _reachable_hosts():
    """What discovery would publish, or [] when the core cannot say.

    A separate function so the choice above is testable without pretending this
    machine has a second interface.
    """
    try:
        from macast.discovery import advertisable_addresses
        return list(advertisable_addresses())
    except Exception:  # pragma: no cover - discovery ships with the app
        return []


# -- Cast v2 sender (same minimal subset cast_bridge uses) -------------------

class _CastSender(object):
    """The smallest Cast v2 sender a receiver will accept."""

    def __init__(self, host, port=CAST_PORT, timeout=5.0, source_id="sender-0"):
        self.host = host
        self.port = port
        self.timeout = max(1.0, float(timeout))
        self.source_id = source_id
        self.sock = None
        self.transport_id = None
        self.media_session_id = 1
        #: Only a mirroring session has one; it is what the receiver's STOP
        #: names, and without it the app stays running behind us.
        self.mirror_session_id = None
        #: A mirroring session writes to this socket from two threads (the
        #: teardown on the menu thread, the keepalive in the media loop), and
        #: interleaved CASTV2 frames are how a session dies quietly.
        self._write_lock = threading.Lock()

    def _send(self, destination, namespace, payload, binary=False):
        body = payload if binary else json.dumps(payload)
        blob = encode_cast_message(self.source_id, destination, namespace, body,
                                   binary)
        with self._write_lock:
            self.sock.sendall(struct.pack('>I', len(blob)) + blob)

    def _recv_exactly(self, count):
        buf = b""
        while len(buf) < count:
            chunk = self.sock.recv(count - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def _recv(self):
        header = self._recv_exactly(4)
        if header is None:
            return None
        (length,) = struct.unpack('>I', header)
        body = self._recv_exactly(length)
        return parse_cast_message(body) if body else None

    def _await(self, wanted, timeout=None):
        return self._await_any((wanted,), timeout)

    def _await_any(self, wanted, timeout=None):
        """The first message whose `type` is in `wanted`, or None.

        Anything else that arrives in between -- a PONG, a broadcast
        RECEIVER_STATUS -- is noise to be stepped over, not an error. The
        mirroring handshake has to watch for two types at once (the status that
        carries a transportId and the LAUNCH_ERROR that says there never will
        be one), which is why this exists next to `_await`.
        """
        deadline = time.time() + (timeout or self.timeout)
        while time.time() < deadline:
            self.sock.settimeout(max(0.2, deadline - time.time()))
            try:
                message = self._recv()
            except (socket.timeout, ssl.SSLError, OSError):
                return None
            if message is None:
                return None
            if message.get("payload_type") != 0:
                continue
            try:
                data = json.loads(message.get("payload_utf8") or "{}")
            except ValueError:
                continue
            if data.get("type") in wanted:
                return data
        return None

    def connect(self):
        raw = socket.create_connection((self.host, self.port), timeout=self.timeout)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        self.sock = context.wrap_socket(raw, server_hostname=None)
        self.sock.settimeout(self.timeout)
        self._send("receiver-0", "urn:x-cast:com.google.cast.tp.deviceauth",
                   DEVICE_AUTH_CHALLENGE, binary=True)
        self._recv()
        self._send("receiver-0", NS_CONNECTION, {"type": "CONNECT"})
        return self

    def launch(self):
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "LAUNCH", "appId": DEFAULT_MEDIA_APP_ID,
                    "requestId": 1})
        status = self._await("RECEIVER_STATUS") or {}
        applications = (status.get("status") or {}).get("applications") or []
        if not applications:
            raise RuntimeError("目标设备没有启动媒体接收器（LAUNCH 无响应）")
        self.transport_id = applications[0].get("transportId")
        if not self.transport_id:
            raise RuntimeError("目标设备没有返回 transportId")
        self._send(self.transport_id, NS_CONNECTION, {"type": "CONNECT"})
        return self.transport_id

    def load(self, url, content_type='', live=False):
        media = {"contentId": url,
                 "streamType": "LIVE" if live else "BUFFERED"}
        if content_type:
            media["contentType"] = content_type
        self._send(self.transport_id, NS_MEDIA,
                   {"type": "LOAD", "requestId": 2, "autoplay": True,
                    "media": media})
        status = self._await("MEDIA_STATUS") or {}
        entries = status.get("status") or [{}]
        if isinstance(entries, dict):
            entries = [entries]
        self.media_session_id = (entries[0] or {}).get("mediaSessionId", 1)
        return status

    def stop(self):
        if self.transport_id is None:
            return False
        try:
            self._send(self.transport_id, NS_MEDIA,
                       {"type": "STOP", "requestId": 4,
                        "mediaSessionId": self.media_session_id})
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "sessionId": None, "requestId": 5})
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info("could not send STOP to the device: %s", e)
            return False
        return True

    def set_volume(self, level):
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "SET_VOLUME", "requestId": 6,
                    "volume": {"level": max(0.0, min(1.0, level / 100.0))}})

    # -- mirroring (Cast Streaming): a different app, and no LOAD at all ------

    def receiver_status(self):
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "GET_STATUS", "requestId": 7})
        return (self._await("RECEIVER_STATUS") or {}).get("status") or {}

    def launch_mirroring(self, app_id=MIRROR_APP_ID, timeout=10.0,
                         settle=0.3):
        """Bring up the mirroring app and join its transport.

        Two things here are load-bearing rather than defensive, both of them
        learned from the reference sender: a stale instance of the app has to
        be stopped *before* LAUNCH, because the next session's first OFFER is
        otherwise rejected with "Invalid or missing codec on first OFFER"; and
        the app needs a moment to settle after its transportId appears before
        it will accept a CONNECT.

        LAUNCH_ERROR doubles as the capability probe -- this plugin does not
        filter devices by the `ca` mDNS flag, so a receiver without mirroring
        is discovered here rather than guessed at beforehand.
        """
        for app in self.receiver_status().get("applications") or []:
            if app.get("appId") != app_id:
                continue
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "requestId": 8,
                        "sessionId": app.get("sessionId")
                        or app.get("transportId")})
            # The receiver drops the app asynchronously; racing it is exactly
            # the stale-instance case above.
            time.sleep(1.0)
        self._send("receiver-0", NS_RECEIVER,
                   {"type": "LAUNCH", "appId": app_id, "requestId": 9})
        deadline = time.time() + timeout
        session_id = None
        while time.time() < deadline and not self.transport_id:
            data = self._await_any(("RECEIVER_STATUS", "LAUNCH_ERROR"),
                                   max(0.2, deadline - time.time()))
            if data is None:
                break
            if data.get("type") == "LAUNCH_ERROR":
                raise RuntimeError('这台设备不接受镜像接收器（LAUNCH_ERROR: {}）'.format(
                    data.get("reason") or "未提供原因"))
            for app in (data.get("status") or {}).get("applications") or []:
                if app.get("appId") == app_id and app.get("transportId"):
                    self.transport_id = app["transportId"]
                    session_id = app.get("sessionId") or app.get("transportId")
                    break
        if not self.transport_id:
            raise RuntimeError("镜像接收器没有返回 transportId（启动超时）")
        self.mirror_session_id = session_id
        time.sleep(settle)
        self._send(self.transport_id, NS_CONNECTION, {"type": "CONNECT"})
        return self.transport_id

    def send_offer(self, offer):
        if self.transport_id is None:
            raise RuntimeError("还没有镜像会话，无法发出 OFFER")
        self._send(self.transport_id, NS_WEBRTC, offer)

    def await_answer(self, timeout=10.0):
        data = self._await_any(("ANSWER",), timeout)
        if data is None:
            raise RuntimeError("镜像接收器没有回答 OFFER（等待 ANSWER 超时）")
        return data

    def ping(self):
        """Cast v2 keepalive. Deprecated in the spec but every firmware still
        answers it, and it is the only signal that the TV is still there."""
        self._send("receiver-0", NS_CONNECTION, {"type": "PING"})

    def poll(self, timeout=0.0):
        """One pending control message, or None if nothing is waiting.

        `timeout` 0 puts the socket in non-blocking mode, where "no data" is an
        exception rather than a wait -- which is the whole point: the media loop
        must never be held by the control plane.
        """
        try:
            self.sock.settimeout(timeout)
            return self._recv()
        except (socket.timeout, ssl.SSLError, OSError):
            return None

    def close_mirroring(self):
        """Leave the app and the transports, in that order.

        CLOSE on the app transport without a receiver STOP leaves the mirroring
        app running, and the next session then hits the stale-instance OFFER
        rejection this class went to some trouble to avoid.
        """
        try:
            if self.transport_id is not None:
                self._send(self.transport_id, NS_CONNECTION,
                           {"type": "CLOSE"})
            self._send("receiver-0", NS_RECEIVER,
                       {"type": "STOP", "requestId": 11,
                        "sessionId": getattr(self, "mirror_session_id", None)})
            self._send("receiver-0", NS_CONNECTION, {"type": "CLOSE"})
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.info("could not close the mirroring session: %s", e)
        self.transport_id = None

    def close(self):
        if self.sock is not None:
            self._drain()
            try:
                self.sock.close()
            except OSError:
                pass
            self.sock = None

    def _drain(self, budget=0.3):
        """Empty the receive buffer before hanging up.

        The receiver answers every keepalive and every goodbye, so at teardown
        there are almost always unread replies sitting in this socket. Closing
        with unread bytes makes the kernel answer with RST instead of FIN, and
        that RST discards the final CLOSE that `close_mirroring` just wrote --
        leaving the mirroring app running and the next session facing the
        stale-instance OFFER rejection.
        """
        deadline = time.monotonic() + budget
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.sock.settimeout(min(0.2, remaining))
                if not self.sock.recv(4096):
                    break
        except (socket.timeout, ssl.SSLError, OSError, ValueError):
            pass


# -- DLNA control point (the TV is a renderer; we are its remote) -------------
#
# Everything above this line speaks Cast, to a device that wants to be told
# what to play. A DLNA MediaRenderer is the same idea over UPnP: SSDP to find
# it, an XML description to learn where its AVTransport control endpoint is,
# and SOAP to say "here is a URL, play it". Written on purpose from the
# standard library: `macast/ssdp.py` is a *device* (it answers M-SEARCH), not a
# control point, and a single-file plugin cannot pip-install one.

#: How long one M-SEARCH listens, and how long one description GET may take.
#: Both used to be 3 s *and* sequential, so a LAN with a TV that answers SSDP
#: and then takes its time over HTTP made the console's first device list arrive
#: a quarter of a minute after the page opened.
SSDP_SEARCH_TIMEOUT = 2.5
#: A TV that answers the discovery packet and then answers the description GET
#: slowly is the normal shape of a device coming out of standby, not an error.
#: 1.5 s was chosen to keep the first page load snappy and instead made those
#: devices vanish without a trace -- see `_dlna_unreadable`.
DESCRIBE_TIMEOUT = 3.0
#: Ceiling on how many descriptions are fetched at once.
MAX_DESCRIBE = 12


def _search_source_ip():
    """The address to send M-SEARCH from, or None to let the OS route it.

    Only a *pinned* interface changes the answer: `Setting.get_ip()` narrows to
    that NIC alone then, so any advertisable address belongs where the user
    asked to look. Without a pin the kernel's route table beats our guess --
    binding to whichever address sorts first is how a VM bridge (192.168.99.1)
    ends up carrying the search away from the TV, which is the same failure
    AGENTS.md 4.1 records for the mDNS advertiser.
    """
    try:
        if not Setting.resolved_network_interface():
            return None
        addrs = sorted(Setting.get_advertisable_ip())
    except Exception:
        return None
    return addrs[0] if addrs else None


#: How many local addresses one *empty* search may retry from.
#:
#: A Windows box grows an interface per feature -- Hyper-V, WSL, Docker, a
#: VPN -- and the multicast route the kernel picks for an unbound socket is
#: not necessarily the one the television is on, which is the same failure
#: AGENTS.md 4.1 records for the mDNS advertiser. Retrying from each address
#: that could carry a LAN is what turns「找不到设备」into a device list there.
#: Four keeps a fruitless search to a few seconds; a single-homed machine
#: never reaches the second attempt at all.
MAX_SEARCH_FALLBACKS = 4

#: What the last DLNA search actually tried, as
#: [{'interface', 'answers', 'error'}] -- raw material for the diagnostics
#: panel, because "no devices" and "the packet never left" look identical on
#: screen and are entirely different problems.
_dlna_trace = []


def dlna_trace():
    """The interfaces the last DLNA search tried, in order."""
    return list(_dlna_trace)


def _search_fallback_interfaces():
    """Local addresses to retry an empty search from, in order.

    Loopback is left out: a search that only ever answered from this machine
    would answer from the routed socket too.
    """
    try:
        return [addr for addr in sorted(Setting.get_advertisable_ip())
                if not str(addr).startswith('127.')][:MAX_SEARCH_FALLBACKS]
    except Exception:
        return []


def _ask_renderers_everywhere(targets, timeout):
    """Answers from whichever interface has them, with a trace of what was tried.

    Ordered: the OS-routed attempt first -- which is all a single-homed machine
    ever needs, and keeps the well-tested path in front -- then one attempt per
    local address that could reach a LAN. An attempt that already answered ends
    the sweep, so a working LAN pays nothing for this.
    """
    global _dlna_trace
    trace = []
    answers = []
    last_error = None
    attempts = [None] + _search_fallback_interfaces()
    seen = set()
    for interface in attempts:
        if interface in seen:
            continue
        seen.add(interface)
        try:
            found = list(_ask_renderers(targets, timeout, interface=interface))
            error = None
        except DiscoveryError as e:
            found, error = [], e
        except Exception as e:                      # pragma: no cover - defensive
            found, error = [], e
        trace.append({'interface': interface or '自动（内核路由）',
                      'answers': len(found),
                      'error': '' if error is None else str(error)})
        if found:
            answers, last_error = found, None
            break
        if error is not None:
            last_error = error
    _dlna_trace = trace
    if not answers and last_error is not None \
            and all(row['error'] for row in trace):
        # Every attempt failed to even send: that is a discovery failure, not
        # an empty LAN, and the console has a different sentence for it.
        raise last_error
    return answers


def _ssdp_search(st, timeout=SSDP_SEARCH_TIMEOUT, interface=None):
    """(LOCATION, answering ip) pairs for one SSDP search target."""
    request = '\r\n'.join([
        'M-SEARCH * HTTP/1.1',
        'HOST: {}:{}'.format(SSDP_ADDR, SSDP_PORT),
        'MAN: "ssdp:discover"',
        'MX: 2',
        'ST: {}'.format(st),
        '', ''])
    found = set()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(0.4)
    if interface:
        # Binding is how a multi-homed Mac (VPN, bridges) picks which interface
        # the multicast leaves on -- the same worry AGENTS.md 4.1 has for the
        # mDNS advertiser. `IP_MULTICAST_IF` is the half that actually selects
        # the egress for a *multicast* destination; a bound source address alone
        # still leaves the route lookup to the kernel.
        try:
            sock.bind((interface, 0))
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF,
                            socket.inet_aton(interface))
        except OSError as e:
            logger.info('cannot search from %s: %s', interface, e)
    try:
        now = time.time()
        # One M-SEARCH is not one search. A renderer that has just woken has not
        # joined 239.255.255.250:1900 yet, and SSDP tells clients to repeat for
        # exactly that reason; a single packet makes the device list depend on
        # which 100 ms its join happened to land in.
        sends = [now, now + timeout * 0.45]
        deadline = now + timeout
        while time.time() < deadline:
            if sends and time.time() >= sends[0]:
                sends.pop(0)
                try:
                    sock.sendto(request.encode('ascii'),
                                (SSDP_ADDR, SSDP_PORT))
                except OSError:
                    if not found:
                        raise
            try:
                data, peer = sock.recvfrom(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            for line in data.decode('utf-8', 'replace').splitlines():
                name, _, value = line.partition(':')
                if name.strip().lower() != 'location':
                    continue
                value = value.strip()
                if value.startswith('http'):
                    found.add((value, peer[0]))
    finally:
        sock.close()
    return sorted(found)


def _local_name(tag):
    return tag.rpartition('}')[2].lower() if isinstance(tag, str) else ''


def parse_description(raw, base_url, peer_ip):
    """(friendly name, absolute AVTransport control URL) or None.

    Namespaces are matched by local name because UPnP descriptions in the wild
    disagree on prefix and case. The control URL is resolved against the
    description's own base, and a device that describes itself on a loopback or
    unspecified address is rewritten to the address that actually answered --
    a renderer behind a router hits that more often than the spec admits.
    """
    import urllib.parse
    import xml.etree.ElementTree as ET
    import ipaddress
    try:
        root = ET.fromstring(raw)
    except ET.ParseError:
        return None
    if root is None:
        return None

    def first_text(tag):
        for node in root.iter():
            if _local_name(node.tag) == tag:
                return (node.text or '').strip()
        return ''

    name = first_text('friendlyname') or peer_ip
    parsed = urllib.parse.urlsplit(base_url)
    host = parsed.hostname or peer_ip
    port = _port_suffix(parsed)
    try:
        if ipaddress.ip_address(host).is_loopback or host == '0.0.0.0':
            host = peer_ip
    except ValueError:
        pass                      # a hostname: keep it, DNS is the LAN's job
    control = ''
    for service in root.iter():
        if _local_name(service.tag) != 'service':
            continue
        values = {}
        for child in service:
            values[_local_name(child.tag)] = (child.text or '').strip()
        if values.get('servicetype', '').endswith(':AVTransport:1'):
            control = values.get('controlurl', '')
            break
    if not control:
        return None
    if not control.startswith('http'):
        # The port belongs to the answer, not to the hostname: dropping it
        # here sends every SOAP call to port 80 of a TV that serves it on 5246.
        same_origin = '{}://{}{}'.format(parsed.scheme, host, port)
        prefix = same_origin + ('' if control.startswith('/')
                                else parsed.path.rsplit('/', 1)[0])
        control = prefix + (control if control.startswith('/')
                            else '/' + control)
    else:
        described = urllib.parse.urlsplit(control)
        if described.hostname != host:
            control = '{}://{}{}{}'.format(described.scheme, host,
                                           _port_suffix(described, port),
                                           described.path)
    return name, control


def describe_renderer(url, peer_ip, timeout=DESCRIBE_TIMEOUT):
    import urllib.request
    try:
        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}))
        with opener.open(url, timeout=timeout) as response:
            raw = response.read(512 * 1024)
    except Exception as e:
        logger.debug('cannot read %s: %s', url, e)
        return None
    return parse_description(raw, url, peer_ip)


def _ask_renderers(targets, timeout, interface=None):
    """[(LOCATION, ip)] answered for each target, plus why a target could not ask.

    The M-SEARCHes overlap: two targets searched one after the other is what
    made the console's first「没有发现」take half a minute to arrive.
    """
    buckets = [[] for _ in targets]
    failures = []

    def ask(index, st):
        try:
            buckets[index] = list(_ssdp_search(st, timeout=timeout,
                                               interface=interface))
        except Exception as e:
            failures.append(str(e))
            logger.error('ssdp search for %s failed: %s', st, e)

    workers = [threading.Thread(target=ask, args=(index, st), daemon=True,
                                name='SCREEN_MIRROR_SSDP')
               for index, st in enumerate(targets)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join(timeout + 2.0)
    answers = []
    for bucket in buckets:
        for answer in bucket:
            if answer not in answers:
                answers.append(answer)
    if not answers and len(failures) == len(targets):
        raise DiscoveryError('DLNA 搜索发不出去：{}'.format(failures[0]))
    return answers


def discover_renderers(timeout=SSDP_SEARCH_TIMEOUT):
    """[(friendly name, control url, host)] for DLNA renderers on the LAN.

    The host is what `advertise_host()` must be compared against: a TV on
    another subnet can answer SSDP and still never reach our stream URL.

    Reading the descriptions is the slow part when several devices answer --
    a TV that ignores HTTP holds a slot for the whole timeout -- so those
    requests overlap too, instead of adding up.
    """
    global _dlna_unreadable
    seen = {}
    try:
        ours = set(Setting.get_advertisable_ip())
    except Exception:
        ours = set()
    # Both verdict counters describe *this* round. Carrying a last-round value
    # into a round that failed early is how the console kept blaming the same
    # "only this Mac answered" for a search that never ran.
    _self_alone['dlna'] = 0
    _dlna_unreadable = 0
    answers = _ask_renderers_everywhere(DLNA_SEARCH_TARGETS, timeout)
    others = [answer for answer in answers if answer[1] not in ours]
    _self_alone['dlna'] = len({answer[1] for answer in answers} & ours)
    answers = others                            # do not mirror to ourselves
    described = [None] * len(answers)

    def read(index, location, peer):
        described[index] = describe_renderer(location, peer)

    workers = [threading.Thread(target=read, args=(index, location, peer),
                                daemon=True, name='SCREEN_MIRROR_DESCRIBE')
               for index, (location, peer) in enumerate(answers[:MAX_DESCRIBE])]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join(DESCRIBE_TIMEOUT + 2.0)
    for answer, parsed in zip(answers, described):
        if parsed:
            name, control = parsed
            host = _url_host(control) or answer[1]
            seen[host] = (name, control, host)
        else:
            # None covers "still reading" (the worker outlived its join), "the
            # GET failed", and "the description has no AVTransport in it". All
            # three are the same shape to the user -- a device that answers
            # discovery and then is not there -- and none of them is
            # 「没有发现」, because something *did* answer.
            _dlna_unreadable += 1
    return sorted(seen.values())


_dlna_devices = []
_dlna_searching = False
#: wall clock of the last completed DLNA search; 0.0 = never (see _searched_at).
_dlna_searched_at = 0.0
#: why the last DLNA search could not run (see _search_error).
_dlna_search_error = ''
#: how many answers of the last search spoke SSDP but never served a readable
#: description. 0 for a protocol that has no descriptions to read.
_dlna_unreadable = 0


def start_renderer_search():
    """Refresh the DLNA renderer list in the background (menu-safe)."""
    global _dlna_searching
    with _search_lock:
        if _dlna_searching:
            return False
        _dlna_searching = True

    def _run():
        global _dlna_devices, _dlna_searching, _dlna_searched_at
        global _dlna_search_error
        try:
            _dlna_devices = discover_renderers()
            _dlna_search_error = ''
        except DiscoveryError as e:
            _dlna_search_error = str(e)
            logger.error('dlna search failed: %s', e)
        except Exception as e:
            _dlna_search_error = 'DLNA 搜索失败：{}'.format(e)
            logger.error('dlna search failed: %s', e)
        finally:
            with _search_lock:
                _dlna_searching = False
                _dlna_searched_at = time.time()

    threading.Thread(target=_run, daemon=True,
                     name="SCREEN_MIRROR_DLNA_SEARCH").start()
    return True


def dlna_target():
    """(name, control url) of the renderer chosen in the menu.

    A separate key from `Mirror_Target` on purpose: that one holds
    `host:port` for a Chromecast, and parsing a URL with that splitter is how
    'http://10.0.0.5:49152/x' turns into a host named 'http'.
    """
    control = Setting.get(SettingProperty.Mirror_Dlna_Control, '') or ''
    name = Setting.get(SettingProperty.Mirror_Target_Name, '') or ''
    if not control:
        return None, ''
    return (name or control), control


def source_address_for(peer_ip):
    """Which of our own addresses reaches this TV.

    A UDP connect() asks the routing table without sending anything. Mirroring
    from the wrong interface looks like a broken stream: the TV fetches the
    URL, gets nothing routable back, and reports 'file unsupported'.
    """
    try:
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    except OSError:
        return advertise_host()
    try:
        probe.connect((peer_ip, 9))
        return probe.getsockname()[0]
    except OSError:
        return advertise_host()
    finally:
        probe.close()


def _url_host(url):
    """The hostname inside a URL, for the route probe."""
    import urllib.parse
    return urllib.parse.urlsplit(url).hostname or ''


def _port_suffix(parts, default=''):
    """:port for a urlsplit result, `default` when it names none.

    ``parts.port`` raises on a non-numeric port, and a hand-written device
    description is exactly where that turns up; losing the renderer silently
    is worse than guessing the scheme's own port.
    """
    try:
        return ':{}'.format(parts.port) if parts.port else default
    except ValueError:
        return default


def dlna_stream_url(server, peer_ip=None):
    """The stream address as the TV on the other end will have to write it."""
    host = source_address_for(peer_ip) if peer_ip else advertise_host()
    return 'http://{}:{}{}'.format(host, server.server_address[1],
                                   server.session.stream_path())


class _DlnaSender(object):
    """The handful of AVTransport actions a mirror needs, over SOAP.

    InstanceID is remembered from whatever the renderer last answered with: a
    device is allowed to move off 0, and a control point that keeps sending 0
    gets error 701 from exactly the quirky firmware this target exists for.
    """

    def __init__(self, control_url, timeout=5.0):
        self.control_url = control_url
        self.timeout = timeout
        self.instance_id = '0'
        self.last_state = ''
        self._opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}))

    def _request(self, action, args):
        from xml.sax.saxutils import escape
        import xml.etree.ElementTree as ET
        fields = ''.join('<{name}>{value}</{name}>'.format(
            name=name, value=escape(unicode_text(args[name])))
            for name in args)
        body = (
            '<?xml version="1.0" encoding="utf-8"?>'
            '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
            's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">'
            '<s:Body><u:{action} xmlns:u="{service}">'
            '<InstanceID>{instance}</InstanceID>{fields}'
            '</u:{action}></s:Body></s:Envelope>'
        ).format(action=action, service=DLNA_SERVICE,
                 instance=self.instance_id, fields=fields)
        # Byte length, not str length: a Chinese device name is multi-byte, and
        # a Content-Length that counts characters truncates the envelope on the
        # wire -- the renderer then answers a SOAP fault and the mirror looks
        # like a dead TV rather than like our own bug.
        payload = body.encode('utf-8')
        request = urllib.request.Request(
            self.control_url, data=payload,
            headers={'SOAPAction': '"{}#{}"'.format(DLNA_SERVICE, action),
                     'Content-Type': 'text/xml; charset="utf-8"',
                     'Content-Length': str(len(payload)),
                     'Connection': 'close'})
        with self._opener.open(request, timeout=self.timeout) as response:
            raw = response.read(512 * 1024)
        values = {}
        try:
            root = ET.fromstring(raw)
        except ET.ParseError:
            logger.warning('%s replied with something that is not XML',
                           action)
            return values
        for node in root.iter():
            if _local_name(node.tag) == 'instanceid' and (node.text or '').strip():
                self.instance_id = node.text.strip()
            if _local_name(node.tag).endswith('response'):
                for child in node:
                    values[_local_name(child.tag)] = (child.text or '').strip()
        return values

    def set_uri(self, url, title, profile, shape=None, max_seconds=None):
        """Tell the renderer what to play.

        Only the file shape states a size and a duration -- see `build_didl`.
        `max_seconds` is the ceiling *this session* was started under: the DIDL
        must not promise a length the encoder was told not to write, so the two
        callers that push a mirror's own URL hand their session's value down
        rather than letting this method ask the setting again mid-flight.
        """
        shape = shape or dlna_shape()
        if shape == DLNA_SHAPE_FILE:
            size, duration = advertised_file(profile.total_bitrate, max_seconds)
        else:
            size = duration = None
        return self._request('SetAVTransportURI', {
            'CurrentURI': url,
            'CurrentURIMetaData': build_didl(url, title, profile, size,
                                             duration)})

    def play(self):
        return self._request('Play', {'Speed': '1'})

    def stop(self):
        try:
            self._request('Stop', {})
        except Exception as e:
            logger.debug('the renderer did not accept Stop: %s', e)

    def transport_state(self):
        state = self._request('GetTransportInfo', {}).get(
            'currenttransportstate', '')
        if state:
            self.last_state = state
        return self.last_state

    def position(self):
        return self._request('GetPositionInfo', {}).get('reltime', '')

    def close(self):
        pass


# -- Cast Streaming: AES-128-CTR without a crypto dependency -----------------

def _xtime(a):
    a <<= 1
    return (a ^ 0x1B) & 0xFF if a & 0x100 else a


def _build_aes_tables():
    """Rijndael's S-box and the four encryption T-tables, computed once.

    Generated rather than pasted: a 256-byte literal is a typo waiting to ship,
    and the derivation is a dozen lines that a test can pin with the FIPS-197
    vector.
    """
    exp = [0] * 256
    log = [0] * 256
    x = 1
    for i in range(255):
        exp[i] = x
        log[x] = i
        x = _xtime(x) ^ x                     # multiply by 3, the field generator
    sbox = bytearray(256)
    for i in range(256):
        b = 0 if i == 0 else exp[(255 - log[i]) % 255]        # GF(2^8) inverse
        s = b
        for shift in (1, 2, 3, 4):
            s ^= ((b << shift) | (b >> (8 - shift))) & 0xFF
        sbox[i] = (s ^ 0x63) & 0xFF
    te0 = [0] * 256
    te1 = [0] * 256
    te2 = [0] * 256
    te3 = [0] * 256
    for i in range(256):
        s = sbox[i]
        s2 = _xtime(s)
        s3 = s2 ^ s
        te0[i] = (s2 << 24) | (s << 16) | (s << 8) | s3
        te1[i] = (s3 << 24) | (s2 << 16) | (s << 8) | s
        te2[i] = (s << 24) | (s3 << 16) | (s2 << 8) | s
        te3[i] = (s << 24) | (s << 16) | (s3 << 8) | s2
    return bytes(sbox), tuple(te0), tuple(te1), tuple(te2), tuple(te3)


_AES_SBOX, _AES_TE0, _AES_TE1, _AES_TE2, _AES_TE3 = _build_aes_tables()


def _aes_expand_key(key):
    """The 44 round words of an AES-128 key schedule, big-endian."""
    if len(key) != 16:
        raise ValueError('AES-128 wants a 16-byte key')
    w = [int.from_bytes(key[i * 4:i * 4 + 4], 'big') for i in range(4)]
    rcon = 1
    for i in range(4, 44):
        t = w[i - 1]
        if i % 4 == 0:
            t = ((t << 8) | (t >> 24)) & 0xFFFFFFFF              # RotWord
            s = _AES_SBOX
            t = ((s[t >> 24] << 24) | (s[(t >> 16) & 0xFF] << 16)
                 | (s[(t >> 8) & 0xFF] << 8) | s[t & 0xFF])
            t ^= rcon << 24
            rcon = _xtime(rcon)
        w.append(w[i - 4] ^ t)
    return w


def _aes_encrypt_block(w, s0, s1, s2, s3):
    """One 16-byte block through the schedule from `_aes_expand_key`."""
    te0, te1, te2, te3 = _AES_TE0, _AES_TE1, _AES_TE2, _AES_TE3
    s0 ^= w[0]
    s1 ^= w[1]
    s2 ^= w[2]
    s3 ^= w[3]
    o = 4
    for _ in range(9):
        t0 = (te0[s0 >> 24] ^ te1[(s1 >> 16) & 0xFF]
              ^ te2[(s2 >> 8) & 0xFF] ^ te3[s3 & 0xFF] ^ w[o])
        t1 = (te0[s1 >> 24] ^ te1[(s2 >> 16) & 0xFF]
              ^ te2[(s3 >> 8) & 0xFF] ^ te3[s0 & 0xFF] ^ w[o + 1])
        t2 = (te0[s2 >> 24] ^ te1[(s3 >> 16) & 0xFF]
              ^ te2[(s0 >> 8) & 0xFF] ^ te3[s1 & 0xFF] ^ w[o + 2])
        t3 = (te0[s3 >> 24] ^ te1[(s0 >> 16) & 0xFF]
              ^ te2[(s1 >> 8) & 0xFF] ^ te3[s2 & 0xFF] ^ w[o + 3])
        s0, s1, s2, s3 = t0, t1, t2, t3
        o += 4
    s = _AES_SBOX
    r = w[o:]
    out = []
    words = (s0, s1, s2, s3)
    for c in range(4):                       # last round: SubBytes+ShiftRows+XOR
        word = 0
        for row in range(4):
            shift = 24 - 8 * row
            byte = (s[(words[(c + row) & 3] >> shift) & 0xFF]
                    ^ ((r[c] >> shift) & 0xFF)) & 0xFF
            word |= byte << shift
        out.append(word)
    return out


#: ---------------------------------------------------------------------------
#: AES-128-CTR, from the operating system.
#:
#: The Cast Streaming target encrypts every access unit before slicing it into
#: RTP packets, so the keystream rate *is* the channel's bitrate ceiling. The
#: pure-Python schedule below runs at 1.33 MB/s on this hardware -- about 10
#: Mbps of keystream per eight Mbps of picture, which is what pinned
#: `CAST_STREAM_MAX_BITRATE` to 4.5. That number has nothing to do with the
#: protocol, the radio, or the receiver: it is a Python interpreter.
#:
#: Every platform already ships hardware AES-128-CTR and `ctypes` reaches it
#: without one new dependency: macOS `libcommonCrypto` (in the dyld shared
#: cache, so it loads by absolute path even though `find /usr/lib` cannot see
#: it), Windows `bcrypt.dll`, and OpenSSL's `EVP_aes_128_ctr` everywhere else
#: (already resident, because CPython's `_ssl` links libcrypto). Measured on an
#: Apple-silicon Mac, on the production shape -- one reused handle, a fresh
#: `frame_iv` and a 41,667-byte access unit per call -- **5,367 MB/s**, 7.8 us
#: per frame, ~4,000x the Python path and byte-identical to `/usr/bin/openssl
#: enc -aes-128-ctr` at every length from 1 to 262,144 bytes. (The crypto
#: alone, without `frame_iv` and the IV buffer, measures 11,297 MB/s; the lower
#: figure is the honest one because it is what a frame actually costs.)
#:
#: Two rules came out of that measurement and matter more than the speed:
#:
#:   * **One call per access unit, never chunked.** The same bytes fed in
#:     1,200-byte pieces drop CommonCrypto to 811 MB/s: the ctypes call itself
#:     costs ~1.4 us. This is why `crypt` takes a whole buffer.
#:   * **A known-answer test gates every backend, at import.** It cannot tell a
#:     full-128-bit counter from a low-64-bit one (that takes 2^64 blocks), but
#:     it does tell "the OS changed underneath us" from "we are silently
#:     sending frames no receiver can decode". CommonCrypto increments only the
#:     low 64 bits and treats the high half as a fixed nonce; `frame_iv` puts
#:     the frame id in bytes 8..12 and leaves 12..16 zero, so the counter has
#:     2^32 blocks -- 68.7 GB -- of headroom per access unit, and a carry into
#:     the fixed half would need `aesIvMask[8:16]` within ~2,600 of 2^64
#:     (probability ~2^-51 per session). That arithmetic is the safety argument;
#:     the test is the tripwire.
#:
#: When nothing loads, or something loads and fails the test, we keep the
#: pure-Python keystream and the 4.5 Mbps cap. The cap is then a *degraded
#: mode* constant, not a design constant -- which is why it says so.

def _commoncrypto_backend():
    """macOS: `CCCryptor` in CTR mode, one reset + one update per access unit.

    `CCCryptorReset` on a keyed handle is bit-exact against creating a fresh
    cryptor per frame (verified for frame ids 0, 1, 2, 3, 7, 255, 256, 65535
    and 2^31 through this plugin's own `frame_iv`), and it is what makes the
    reused handle worth keeping: creating one per frame measures 4,994 MB/s
    against 11,297 MB/s with the reset.
    """
    lib = ctypes.CDLL('/usr/lib/system/libcommonCrypto.dylib')
    create = lib.CCCryptorCreateWithMode
    create.argtypes = ([ctypes.c_uint] * 4 + [ctypes.c_void_p] * 2
                       + [ctypes.c_size_t, ctypes.c_void_p, ctypes.c_int,
                          ctypes.c_int, ctypes.c_uint,
                          ctypes.POINTER(ctypes.c_void_p)])
    create.restype = ctypes.c_int
    update = lib.CCCryptorUpdate
    update.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t,
                       ctypes.c_void_p, ctypes.c_size_t,
                       ctypes.POINTER(ctypes.c_size_t)]
    update.restype = ctypes.c_int
    reset = lib.CCCryptorReset
    reset.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    reset.restype = ctypes.c_int
    release = lib.CCCryptorRelease
    release.argtypes = [ctypes.c_void_p]
    release.restype = ctypes.c_int

    def make(key):
        ref = ctypes.c_void_p()
        # The IV here is a placeholder: `crypt` resets it per access unit, and
        # CommonCrypto requires a valid one at creation time.
        if create(0, 4, 0, 0,                       # encrypt, CTR, AES, no pad
                  ctypes.create_string_buffer(key[:16], 16),
                  key, len(key), None, 0, 0, 0,
                  ctypes.byref(ref)) != 0 or not ref.value:
            raise OSError('CCCryptorCreateWithMode refused the key')
        return ref

    def crypt(ref, iv, data):
        if not data:
            return b''
        if reset(ref, ctypes.create_string_buffer(iv, 16)) != 0:
            raise OSError('CCCryptorReset refused the counter block')
        out = ctypes.create_string_buffer(len(data))
        moved = ctypes.c_size_t(0)
        if update(ref, data, len(data), out, len(data),
                  ctypes.byref(moved)) != 0:
            raise OSError('CCCryptorUpdate failed')
        if moved.value != len(data):
            raise OSError('CCCryptorUpdate held back {} of {} bytes'.format(
                len(data) - moved.value, len(data)))
        return out.raw

    return make, crypt, release


def _bcrypt_backend():
    """Windows: CNG `BCryptEncrypt` in CTR mode.

    `pbIV` is documented `[in, out]` -- the call rewrites the buffer with the
    *next* counter block -- so there is no reset to call and every frame gets a
    fresh mutable IV. CTR chaining arrived with Windows 8 / Server 2012; on
    anything older `BCryptSetProperty` fails and we fall through to the
    pure-Python keystream rather than emulating CTR out of ECB blocks. That
    emulation is the obvious idea and it is a bad one: one `BCryptEncrypt` per
    16-byte block costs more than the Python schedule it would replace, so the
    "fallback" would be a downgrade wearing a native badge.
    """
    lib = ctypes.WinDLL('bcrypt')
    open_alg = lib.BCryptOpenAlgorithmProvider
    open_alg.argtypes = [ctypes.POINTER(ctypes.c_void_p), ctypes.c_wchar_p,
                         ctypes.c_wchar_p, ctypes.c_ulong]
    set_prop = lib.BCryptSetProperty
    set_prop.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_void_p,
                         ctypes.c_ulong, ctypes.c_ulong]
    gen_key = lib.BCryptGenerateSymmetricKey
    gen_key.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p),
                        ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p,
                        ctypes.c_ulong, ctypes.c_ulong]
    encrypt = lib.BCryptEncrypt
    encrypt.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong,
                        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong,
                        ctypes.c_void_p, ctypes.c_ulong,
                        ctypes.POINTER(ctypes.c_ulong), ctypes.c_ulong]
    destroy_key = lib.BCryptDestroyKey
    destroy_key.argtypes = [ctypes.c_void_p]
    close_alg = lib.BCryptCloseAlgorithmProvider
    close_alg.argtypes = [ctypes.c_void_p, ctypes.c_ulong]

    def make(key):
        if len(key) != 16:
            raise OSError('BCrypt wants a sixteen-byte key for AES-128')
        alg = ctypes.c_void_p()
        if open_alg(ctypes.byref(alg), 'AES', None, 0) != 0:
            raise OSError('BCryptOpenAlgorithmProvider failed')
        # A null-terminated UTF-16 property value, as the CNG docs require.
        mode = 'ChainingModeCTR'.encode('utf-16-le') + b'\x00\x00'
        if set_prop(alg, 'ChainingMode', mode, len(mode), 0) != 0:
            close_alg(alg, 0)
            raise OSError('this Windows has no CTR chaining mode')
        handle = ctypes.c_void_p()
        if gen_key(alg, ctypes.byref(handle), None, 0, key, len(key), 0) != 0:
            close_alg(alg, 0)
            raise OSError('BCryptGenerateSymmetricKey failed')
        return (alg, handle)

    def crypt(pair, iv, data):
        if not data:
            return b''
        out = ctypes.create_string_buffer(len(data))
        moved = ctypes.c_ulong(0)
        if encrypt(pair[1], data, len(data), None,
                   ctypes.create_string_buffer(iv, 16), 16,
                   out, len(data), ctypes.byref(moved), 0) != 0:
            raise OSError('BCryptEncrypt failed')
        if moved.value != len(data):
            raise OSError('BCryptEncrypt returned {} of {} bytes'.format(
                moved.value, len(data)))
        return out.raw

    def release(pair):
        destroy_key(pair[1])
        close_alg(pair[0], 0)

    return make, crypt, release


def _mapped_libcrypto():
    """The absolute path of a libcrypto this process already has mapped.

    The sonames below cover the common case, but a distribution that ships
    libcrypto under some other name (musl, a vendored build) still has it
    resident -- CPython's `_ssl` links it -- and the loader's own record of
    what is mapped is a better witness than a directory listing.
    """
    try:
        with open('/proc/self/maps', 'r', errors='replace') as maps:
            for line in maps:
                path = line.rsplit(' ', 1)[-1].strip()
                if (os.path.basename(path).startswith('libcrypto.so')
                        and os.path.isabs(path)):
                    return path
    except OSError:
        pass
    return None


def _libcrypto_backend():
    """OpenSSL's EVP: `EVP_aes_128_ctr`, one `Init` per access unit.

    Re-`Init`ing a live context with a new IV is the documented way to restart
    a cipher, so the context and its key schedule are built once per session
    and only the IV moves per frame.
    """
    lib = None
    for soname in ('libcrypto.so.3', 'libcrypto.so.1.1', 'libcrypto.so.1.0.0'):
        try:
            lib = ctypes.CDLL(soname)
            break
        except OSError:
            continue
    if lib is None:
        mapped = _mapped_libcrypto()
        if mapped is None:
            raise OSError('no libcrypto is mapped into this process')
        lib = ctypes.CDLL(mapped)
    new_ctx = lib.EVP_CIPHER_CTX_new
    new_ctx.restype = ctypes.c_void_p
    new_ctx.argtypes = []
    free_ctx = lib.EVP_CIPHER_CTX_free
    free_ctx.argtypes = [ctypes.c_void_p]
    free_ctx.restype = None
    cipher = lib.EVP_aes_128_ctr
    cipher.restype = ctypes.c_void_p
    cipher.argtypes = []
    init = lib.EVP_EncryptInit_ex
    init.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
                     ctypes.c_void_p, ctypes.c_void_p]
    init.restype = ctypes.c_int
    update = lib.EVP_EncryptUpdate
    update.argtypes = [ctypes.c_void_p, ctypes.c_void_p,
                       ctypes.POINTER(ctypes.c_int), ctypes.c_void_p,
                       ctypes.c_int]
    update.restype = ctypes.c_int
    final = lib.EVP_EncryptFinal_ex
    final.argtypes = [ctypes.c_void_p, ctypes.c_void_p,
                      ctypes.POINTER(ctypes.c_int)]
    final.restype = ctypes.c_int

    def make(key):
        if len(key) != 16:
            raise OSError('EVP_aes_128_ctr wants a sixteen-byte key')
        ctx = new_ctx()
        if not ctx:
            raise OSError('EVP_CIPHER_CTX_new returned NULL')
        return (ctx, cipher(), key)

    def crypt(triple, iv, data):
        if not data:
            return b''
        ctx, one_ctr, key = triple
        if init(ctx, one_ctr, None, key, iv) != 1:
            raise OSError('EVP_EncryptInit_ex failed')
        # Room for a Final that never has anything to say on a stream cipher,
        # so that "Update buffered a partial block" cannot pass as success.
        out = ctypes.create_string_buffer(len(data) + 32)
        moved = ctypes.c_int(0)
        if update(ctx, out, ctypes.byref(moved), data, len(data)) != 1:
            raise OSError('EVP_EncryptUpdate failed')
        done = moved.value
        tail = ctypes.c_int(0)
        if final(ctx, ctypes.addressof(out) + done, ctypes.byref(tail)) != 1:
            raise OSError('EVP_EncryptFinal_ex failed')
        return out.raw[:done + tail.value]

    def release(triple):
        free_ctx(triple[0])

    return make, crypt, release


#: Known-answer vectors, all of them computed with
#: `/usr/bin/openssl enc -aes-128-ctr -K 000102...0f -iv 0f0f0f0f0f0f0f0f0f0f0f080f0f0f0f`.
#: The counter block is `frame_iv(b'\x0f' * 16, 7)` spelled out as a literal,
#: because this test runs at import and `frame_iv` is defined further down.
_AES_KAT_KEY = bytes(range(16))
_AES_KAT_IV = b'\x0f' * 8 + b'\x0f\x0f\x0f\x08' + b'\x0f' * 4
#: Two whole blocks: block 0 pins the keystream, block 1 pins the counter
#: increment across a block boundary -- the one place a low-64-bit counter and
#: a full-128-bit one could part ways within a length we can actually test.
_AES_KAT_SHORT = bytes(range(32))
_AES_KAT_SHORT_OUT = bytes.fromhex(
    '54be03754f76e8a02f70d7bd5e6414fc2f00a09be1da68133b93b4f3fe1c07f8')
#: An unaligned tail. A real access unit is never a multiple of sixteen, so a
#: backend that parks a partial block instead of emitting keystream for it
#: would encrypt every frame wrong and still look healthy on the vector above.
_AES_KAT_ODD = bytes(range(37))
_AES_KAT_ODD_OUT = bytes.fromhex(
    '54be03754f76e8a02f70d7bd5e6414fc2f00a09be1da68133b93b4f3fe1c07f8'
    'b8ce93d2c8')
#: A realistic access unit, pinned by digest, on a **reused** handle -- which
#: is what production does (one cipher per session at `_CastStream.__init__`,
#: reseeded per frame). Every pin already in the suite builds a fresh cipher
#: per call, so none of them exercise this path.
_AES_KAT_LONG = bytes(range(256)) * 16
_AES_KAT_LONG_SHA = ('de71b7bd3d02e63cbc436109587c4cf8'
                     'b5d3ff169cd23e3d29e112e604d17868')


def _aes_known_answer(backend):
    """Whether `backend` reproduces `/usr/bin/openssl enc -aes-128-ctr`.

    A failure is logged as an error, not a debug line: the alternative is a
    session that sends frames no receiver can decode, with a log that has
    nothing to say about why.
    """
    import hashlib
    make, crypt, release = backend
    handle = None
    try:
        handle = make(_AES_KAT_KEY)
        if crypt(handle, _AES_KAT_IV, _AES_KAT_SHORT) != _AES_KAT_SHORT_OUT:
            logger.error('the OS AES backend disagrees with openssl on two '
                         'whole blocks; falling back to the pure-Python '
                         'keystream')
            return False
        if crypt(handle, _AES_KAT_IV, _AES_KAT_ODD) != _AES_KAT_ODD_OUT:
            logger.error('the OS AES backend disagrees with openssl on an '
                         'unaligned tail; falling back to the pure-Python '
                         'keystream')
            return False
        # Reseeding twice must land back on the same IV, or the per-frame reset
        # is lying about what it reset.
        if crypt(handle, _AES_KAT_IV, _AES_KAT_ODD) != _AES_KAT_ODD_OUT:
            logger.error('the OS AES backend does not return to the same '
                         'counter block when reseeded; falling back to the '
                         'pure-Python keystream')
            return False
        if crypt(handle, _AES_KAT_IV, b'') != b'':
            logger.error('the OS AES backend invents bytes for an empty '
                         'buffer; falling back to the pure-Python keystream')
            return False
        digest = hashlib.sha256(crypt(handle, _AES_KAT_IV, _AES_KAT_LONG))
        if digest.hexdigest() != _AES_KAT_LONG_SHA:
            logger.error('the OS AES backend disagrees with openssl over a '
                         '4 KiB buffer on a reused handle; falling back to '
                         'the pure-Python keystream')
            return False
        return True
    except Exception as exc:
        logger.error('the OS AES backend failed its known-answer test: %s', exc)
        return False
    finally:
        if handle is not None:
            try:
                release(handle)
            except Exception:
                pass


def _load_native_aes():
    """The OS AES-128-CTR backend for this platform, or None.

    The three backends are platform-disjoint on purpose, and macOS never probes
    libcrypto at all: `ctypes.CDLL('libcrypto.dylib')` by bare soname **aborts
    the interpreter** there, because CPython already has a different libcrypto
    loaded and the flat-namespace collision is fatal. None is a supported
    answer -- it is exactly the degraded mode `CAST_STREAM_MAX_BITRATE` is
    written for.
    """
    if sys.platform == 'darwin':
        builders = (('commoncrypto', _commoncrypto_backend),)
    elif os.name == 'nt':
        builders = (('bcrypt', _bcrypt_backend),)
    else:
        builders = (('libcrypto', _libcrypto_backend),)
    for name, build in builders:
        try:
            backend = build()
        except Exception as exc:
            logger.info('no OS AES backend from %s: %s', name, exc)
            continue
        if _aes_known_answer(backend):
            logger.info('AES-128-CTR is running on %s', name)
            #: The name rides along in slot 0. Without it the tuple is anonymous:
            #: three closures called make/crypt/release look identical no matter
            #: which operating system produced them, so "some OS backend loaded"
            #: would be the strongest statement anything could make -- and that
            #: is not the statement a log line, the console note or a test needs.
            return (name,) + backend
    logger.error('no OS AES-128-CTR backend is usable; the low-latency '
                 'channel runs on the pure-Python keystream and its bitrate '
                 'cap (see CAST_STREAM_MAX_BITRATE)')
    return None


#: The backend `_load_native_aes()` settled on -- `(name, make, crypt, release)`
#: -- or None.
_NATIVE_AES = _load_native_aes()


def native_aes_name():
    """Which OS cipher is carrying the low-latency channel, or ''.

    '' means the pure-Python keystream, which is the degraded mode the bitrate
    cap is written for. Its own accessor because slot 0 of that tuple is the
    only place the fact lives, and three call sites spelling `_NATIVE_AES[0]`
    is three places to get the index wrong.
    """
    return _NATIVE_AES[0] if _NATIVE_AES else ''


class Aes128Ctr(object):
    """AES-128 in counter mode, over a whole buffer.

    Cast Streaming restarts the counter for every access unit with a nonce the
    receiver can rebuild from the frame id, so the caller supplies the full
    16-byte initial counter and we keep keystream generation in one place that
    a test can pin against `/usr/bin/openssl enc -aes-128-ctr`.

    The keystream comes from the operating system when a backend loaded and
    passed its known-answer test at import, and from the pure-Python schedule
    above when it did not. Both produce the same bytes; only one of them is
    fast enough to carry a picture.
    """

    __slots__ = ('_native', '_w')

    def __init__(self, key):
        self._native = None
        self._w = _aes_expand_key(key)
        backend = _NATIVE_AES
        if backend is not None:
            #: Unpacked by name and then held as `(handle, crypt, release,
            #: name)`, so no call site below indexes the backend tuple. The
            #: tuple grew a name in slot 0 once already; three places spelling
            #: `[1]`/`[2]`/`[3]` is three places that would silently call the
            #: wrong function if it grows again.
            name, make, crypt, release = backend
            try:
                self._native = (make(key), crypt, release, name)
            except Exception as exc:
                logger.warning('the OS AES backend %s refused this key (%s); '
                               'this session uses the pure-Python keystream',
                               name, exc)

    def __del__(self):
        try:
            native = self._native
        except AttributeError:
            return                  # __init__ raised before the slot was set
        if native is None:
            return
        try:
            native[2](native[0])
        except Exception:
            # At interpreter shutdown the libraries may already be gone. One
            # leaked cryptor at process exit is reclaimed by the OS; crashing
            # on the way out is not.
            pass

    def crypt(self, iv, data):
        """Return `data` XOR the keystream starting at the 16-byte counter `iv`."""
        if self._native is not None:
            handle, encrypt, release, name = self._native
            try:
                return encrypt(handle, iv, data)
            except Exception as exc:
                # A backend that starts failing mid-session must not stop the
                # stream. Drop to the slow path and say so out loud: the
                # picture survives, the bitrate ceiling comes back, and the
                # log names the reason instead of leaving a mystery stall.
                logger.error('the OS AES backend %s failed mid-stream (%s); '
                             'this session falls back to the pure-Python '
                             'keystream, which caps the low-latency channel',
                             name, exc)
                #: Released here rather than left to `__del__`: the session can
                #: run for hours on the slow path, and a cryptor nobody will
                #: ever touch again is not a reason to hold OS resources.
                try:
                    release(handle)
                except Exception:
                    pass
                self._native = None
        w = self._w
        c0, c1, c2, c3 = struct.unpack('>4I', iv)
        out = bytearray(data)
        pos = 0
        total = len(out)
        pack = struct.pack
        while pos < total:
            k0, k1, k2, k3 = _aes_encrypt_block(w, c0, c1, c2, c3)
            take = min(16, total - pos)
            ks = pack('>4I', k0, k1, k2, k3)[:take]
            chunk = bytes(out[pos:pos + take])
            out[pos:pos + take] = int.to_bytes(
                int.from_bytes(chunk, 'big') ^ int.from_bytes(ks, 'big'),
                take, 'big')
            pos += take
            c3 += 1
            if c3 > 0xFFFFFFFF:
                c3 = 0
                c2 += 1
                if c2 > 0xFFFFFFFF:
                    c2 = 0
                    c1 += 1
                    if c1 > 0xFFFFFFFF:
                        c1 = 0
                        c0 = (c0 + 1) & 0xFFFFFFFF
        return bytes(out)


#: NAL types that carry a picture slice (H.264 table 7-1).
_VCL_NALS = (1, 2, 5)
#: Parameter sets the receiver needs in front of every IDR, not once per stream.
_PARAM_NALS = (7, 8)
#: Access unit delimiter: it names the start of a picture, so it also ends the
#: previous one. Only x264 gets asked for these (the `aud=1` flag); a stream
#: without them simply falls back to "closed by the next slice".
_AUD = 9


def _starts_picture(nal, header):
    """Whether a slice NAL is the first slice of a new picture.

    The slice header opens with `first_mb_in_slice` as an unsigned Exp-Golomb
    code, and zero -- which is what a picture's first slice carries -- encodes
    as a single '1' bit: the top bit of the byte after the NAL header. Slices
    two..n of the same picture have a non-zero first_mb_in_slice, and x264 under
    `-tune zerolatency` really does emit them, so this is the difference between
    one picture per frame and a fifth of a picture per frame.

    `header` is where the NAL header byte sits inside `nal` (after its start
    code). No emulation-prevention unescaping is needed for this one bit: an
    escape can only follow a run of two zeros, and a NAL header byte is never
    zero, so the byte after it is always where it looks.
    """
    return len(nal) > header + 1 and (nal[header + 1] & 0x80) != 0


def _nal_starts(data):
    """Offsets of every Annex-B start code in `data`, with its length.

    A 4-byte start code is a 3-byte one with a leading zero, so the offset is
    reported at the first of those zeros and `width` says how many to skip.
    """
    out = []
    i = data.find(b'\x00\x00\x01')
    while i != -1:
        width = 4 if i > 0 and data[i - 1] == 0 else 3
        out.append((i - (width - 3), width))
        i = data.find(b'\x00\x00\x01', i + 3)
    return out


class _AccessUnits(object):
    """ffmpeg's Annex-B byte stream -> one buffer per picture.

    Cast Streaming encrypts and timestamps *access units*, so the framing has
    to happen here rather than in a muxer. A picture is recognised by its first
    slice (`first_mb_in_slice == 0`, which is ue(v) zero, which is a single '1'
    bit right after the NAL header) and closed by the next one, or by the access
    unit delimiter when the encoder emits one. Slicing matters here: x264 under
    `-tune zerolatency` splits a 1080p picture into several slice NALs, and
    treating each as a frame would put a fifth of a picture on the panel.
    Parameter sets belong to the unit that follows them, and an IDR without its
    SPS/PPS in front is unwatchable, so they are repeated from the last set seen
    rather than trusted to `repeat_headers`.
    """

    def __init__(self):
        self._pending = bytearray()
        self._units = []
        self._open = bytearray()        # the unit being filled
        self._prefix = bytearray()      # non-VCL NALs ahead of the next slice
        self._params = {}               # nal type -> bytes, last seen

    def feed(self, data):
        """Queue the complete units this chunk finished; returns them."""
        self._pending += data
        self._parse()
        out, self._units = self._units, []
        return out

    def flush(self):
        """Hand over everything the stream ended on.

        A unit is only known to be finished when the next one starts, so at the
        end of the byte stream the last NAL is still open *and* the unit before
        it is still waiting for that NAL to be taken. Without this a teardown
        loses the final pictures; with `-aud` the encoder removes the lag
        entirely (see `_AUD`).
        """
        self._parse(final=True)
        if self._open:
            self._close()
        out, self._units = self._units, []
        return out

    def _parse(self, final=False):
        while True:
            starts = _nal_starts(self._pending)
            if not starts or (len(starts) < 2 and not final):
                return                  # the last NAL is still arriving
            offset, width = starts[0]
            nxt = starts[1][0] if len(starts) > 1 else len(self._pending)
            body = offset + width
            ntype = (self._pending[body] & 0x1F) if body < nxt else 0
            nal = bytes(self._pending[offset:nxt])
            del self._pending[:nxt]     # also drops any junk before offset
            self._take(nal, ntype, width)

    def _take(self, nal, ntype, header):
        if ntype not in _VCL_NALS:
            if ntype == _AUD and self._open:
                # An delimiter announces a new picture, which is what tells us
                # the one we were filling is over -- one frame earlier than
                # waiting for its first slice NAL to arrive.
                self._close()
            if ntype in _PARAM_NALS:
                self._params[ntype] = nal
            self._prefix += nal         # belongs to the unit that follows
            return
        if self._open and _starts_picture(nal, header):
            self._close()
        if self._open:
            # Another slice of the picture already being filled.
            self._open += nal
            return
        head = bytearray()
        if ntype == 5:
            for t in _PARAM_NALS:
                cached = self._params.get(t)
                if cached and cached not in self._prefix:
                    head += cached
        # SPS/PPS in front of the prefix, not appended after it: an AU that
        # hands the decoder SEI -> SPS -> IDR is out of order, and whichever
        # set the encoder happened to repeat in-band stays where it was.
        self._open = head + self._prefix + nal
        self._prefix = bytearray()

    def _close(self):
        unit = bytes(self._open)
        self._open = bytearray()
        if unit.strip(b'\x00'):
            self._units.append(unit)


# -- the mirroring session ---------------------------------------------------
#
# Everything below is the media plane: what a Chromecast expects to find on UDP
# once the control plane has handed it an OFFER. The field layouts follow
# Google's own openscreen implementation (the C++ that ships in Chrome) as
# transcribed by the MIT-licensed omacast reference, and each one is pinned by a
# case in scripts/verify_cast_airplay.py Part 24. What has *not* been verified
# is a real television: the far end of that loop is a fake receiver built from
# these same tables, so these bytes are self-consistent rather than
# field-proven -- AGENTS.md 4.9 is explicit that those are not the same claim.

#: openscreen's payload types, including the pair its own header calls a
#: "hack for Android TV" -- which is the pair shipping mirroring firmware
#: answers. The canonical table in the same header (video 101) is for real
#: WebRTC receivers, and this protocol is not that.
RTP_VIDEO_PT = 96
VIDEO_CLOCK = 90000
#: The receiver's jitter-buffer target. Chrome's tab-cast default of 400 ms
#: reads as a slideshow from a desktop; 200 ms is what a LAN can hold.
TARGET_DELAY_MS = 200
#: 12 bytes of RTP header + 7 bytes of Cast header. The 1400 ceiling keeps the
#: IP packet inside an ordinary Ethernet MTU: this protocol has no fragmentation
#: story, because a frame travels as whole access units or not at all.
MAX_PACKET = 1400
CAST_PACKET_HEADER = 19
MAX_PAYLOAD = MAX_PACKET - CAST_PACKET_HEADER
#: How far apart two un-acknowledged frame ids may be before an 8-bit id stops
#: being unambiguous. openscreen calls this `kMaxUnackedFrames = 120`, and it is
#: a *span* guard, not a burst window -- the burst is bounded by time, below.
MAX_IN_FLIGHT_FRAMES = 120
#: openscreen's floor and cap on the un-acknowledged window, in milliseconds:
#: `clamp(2 x RTT, 66 ms, targetDelay / 3)`. At our 200 ms target the two clamp
#: ends are 66 and 66.7 ms, so the window is effectively pinned at ~66 ms --
#: about one and a half frames -- and the measured round trip only starts to
#: matter if the target delay is ever raised. Worth saying out loud, because it
#: means this constant, not the RTT probe, is what decides the queue here.
MIN_WINDOW_MS = 66.0
#: A round trip longer than this is a lost checkpoint, not a slow network, and
#: feeding it back into the window would widen the queue for nothing.
RTT_MAX_MS = 1000.0
#: Exponential smoothing weight for a new round-trip sample.
RTT_SMOOTHING = 0.25
#: The receiver's "none of this frame arrived" marker -- `kAllPacketsLost` in
#: openscreen's `cast/streaming/rtp_defines.h`. It occupies the packet-id field,
#: so it must be tested *before* any bitmask expansion: expanding it would ask
#: for packets 65536..65543 of a frame that has thirty, and the reference is
#: explicit that the bit vector carries no meaning in this case.
ALL_PACKETS_LOST = 0xFFFF
#: Ceiling on the packets one RTCP feedback may make us resend. At the 8 Mbps
#: ceiling a frame is about forty-two kilobytes = thirty packets, and the
#: un-acknowledged window is one and a half frames wide, so two frames' worth is
#: already more than the protocol says can be outstanding. A receiver asking for
#: more than this is asking for frames we shed on purpose, and answering it would
#: turn a confused television into a traffic amplifier pointed at itself.
MAX_RETRANSMIT_PER_FEEDBACK = 64
#: How much of the un-acknowledged window we keep a resendable copy of. The span
#: guard allows 120 frames at ~42 KB each, so the arithmetic worst case is 5 MB;
#: this is the number that says the ceiling is intended rather than discovered.
#: When it is exceeded the *oldest* frames lose their copies and their NACKs are
#: then ignored -- losing the ability to repair a frame nobody has acknowledged
#: in five seconds is cheaper than losing the process.
RETRANSMIT_BUFFER_BYTES = 8 * 1024 * 1024
#: A receiver will not paint anything until it has the NTP<->RTP mapping, so
#: the first sender report rides with the first picture rather than waiting for
#: this timer.
SR_INTERVAL = 0.5
#: With nothing new to send, resend the last packet of the newest
#: un-acknowledged frame. It is the protocol's only way of saying "I am still
#: here, and this is the frame I am waiting on you for", and it is what unsticks
#: a receiver that lost a frame's final packet.
KICKSTART_INTERVAL = 0.25
CONTROL_PING_SECONDS = 5.0
#: The reference's own definition of a dead session.
CONTROL_LOSS_SECONDS = 20.0
#: Seconds between the NTP epoch (1900) and the UNIX epoch (1970).
NTP_UNIX_OFFSET = 2208988800
#: The ceiling this channel advertises, now that the keystream comes from the
#: operating system (`_NATIVE_AES`): 8 Mbps is what the OS AES measures out at
#: ~4,000x headroom, and it sits just under Google's own
#: `kDefaultVideoMaxBitRate = 10 Mbps` rather than under a Python interpreter.
#: 1080p desktop at 6 Mbps -- the menu's own '1080' preset -- now fits.
CAST_STREAM_MAX_BITRATE = 8000000
#: What the ceiling falls back to when no OS AES backend loaded and the
#: pure-Python keystream is doing the work: that runs at 1.35 MB/s, so 10 Mbps
#: of picture would leave the encryptor permanently behind the encoder and the
#: mirror would slow-walk further into the past every second. This is a
#: **degraded-mode** constant, not a design constant, and the two must not be
#: collapsed back into one -- that single number is what pinned this channel
#: below both Google's default and Chrome's for its whole life.
CAST_STREAM_DEGRADED_BITRATE = 4500000


def cast_stream_bitrate_cap():
    """The bitrate this channel may actually ask for, given the cipher it got.

    One function because the OFFER, the encoder argv and the console note all
    have to agree: promising a receiver 8 Mbps and then encoding at 4.5 is a
    lie the receiver can detect, and quoting the wrong number on the page is a
    lie the user can.
    """
    return (CAST_STREAM_MAX_BITRATE if _NATIVE_AES is not None
            else CAST_STREAM_DEGRADED_BITRATE)
#: Requested height -> the frame this target actually pins. The OFFER has to
#: claim a resolution before the encoder has produced a single picture, and
#: "whatever the desktop happens to be" cannot be claimed then -- so 'source'
#: means 1080p on this target, and the scale filter letterboxes into the box
#: rather than stretching a 3:2 laptop to make the claim true.
CAST_STREAM_SIZES = {360: (640, 360), 720: (1280, 720), 1080: (1920, 1080),
                     0: (1920, 1080)}


def cast_stream_shape(height):
    """(width, height, ffmpeg scale/pad filter) for the low-latency target."""
    width, height = CAST_STREAM_SIZES.get(height, CAST_STREAM_SIZES[720])
    return width, height, (
        'scale={w}:{h}:force_original_aspect_ratio=decrease,'
        'pad={w}:{h}:(ow-iw)/2:(oh-ih)/2'.format(w=width, h=height))


def aes_material():
    """(key, iv mask), sixteen raw bytes each.

    The *sender* makes both up and offers them in the clear; the ANSWER carries
    no key material at all. Worth being blunt about: this is encryption for
    framing, not confidentiality. Anyone on the LAN who can read the OFFER can
    decrypt the picture.
    """
    return secrets.token_bytes(16), secrets.token_bytes(16)


def frame_iv(iv_mask, frame_id):
    """openscreen's per-frame counter block: a zero block with the frame id
    big-endian at bytes 8..12, then the whole thing XORed with the mask."""
    block = (b'\x00' * 8 + struct.pack('>I', frame_id & 0xFFFFFFFF)
             + b'\x00' * 4)
    return bytes(bytearray(a ^ b for a, b in zip(block, iv_mask)))


def build_offer(ssrc, key, iv_mask, width, height, fps, bitrate, seq_num=1):
    """The OFFER for one video stream (audio is phase two -- see the plan).

    `aesKey` and `aesIvMask` have to be exactly 32 hex digits. The receiver
    reads the length as the key size, so anything else comes back as
    `result: error` rather than a negotiation.
    """
    return {
        'type': 'OFFER', 'seqNum': seq_num,
        'offer': {
            'castMode': 'mirroring',
            'receiverGetStatus': True,
            'supportedStreams': [{
                'index': 0,
                'type': 'video_source',
                'codecName': 'h264',
                'rtpProfile': 'cast',
                'rtpPayloadType': RTP_VIDEO_PT,
                'ssrc': ssrc,
                'targetDelay': TARGET_DELAY_MS,
                'aesKey': key.hex(),
                'aesIvMask': iv_mask.hex(),
                'timeBase': '1/{}'.format(VIDEO_CLOCK),
                'maxBitRate': bitrate,
                'maxFrameRate': '{}000/1000'.format(fps),
                'resolutions': [{'width': width, 'height': height}],
                'receiverRtcpEventLog': False,
            }],
        },
    }


def parse_answer(data):
    """(udp port, receiver ssrc) out of an ANSWER; raise if it is a refusal.

    `sendIndexes` and `ssrcs` are positional pairs into our own OFFER. The
    receiver's SSRC exists only so feedback can be addressed -- the RTP we send
    carries *our* SSRC.
    """
    if (data.get('result') or 'ok') != 'ok':
        raise RuntimeError('接收端拒绝了镜像邀请：{}'.format(
            json.dumps(data.get('error') or {}, ensure_ascii=False)))
    answer = data.get('answer') or {}
    port = answer.get('udpPort')
    indexes = answer.get('sendIndexes') or []
    ssrcs = answer.get('ssrcs') or []
    if not isinstance(port, int) or isinstance(port, bool) or \
            not 0 < port < 65536:
        raise RuntimeError('接收端给的 udpPort 不可用：{!r}'.format(port))
    if not indexes or len(indexes) != len(ssrcs):
        raise RuntimeError('接收端的 ANSWER 自相矛盾：{} 条流、{} 个 SSRC'.format(
            len(indexes), len(ssrcs)))
    if 0 not in indexes:
        raise RuntimeError('接收端没有接受这条视频流')
    return port, ssrcs[list(indexes).index(0)]


def _unit_is_key(unit):
    """Whether an access unit holds an IDR, i.e. redraws the picture."""
    for offset, width in _nal_starts(unit):
        body = offset + width
        if body < len(unit) and (unit[body] & 0x1F) == 5:
            return True
    return False


def cast_packet(payload, ssrc, sequence, timestamp, frame_id, is_key,
                packet_id, packet_count, referenced_frame_id, marker=False):
    """One RTP header and Cast header in front of an encrypted slice."""
    flags = 0x40              # "the referenced frame id field is used"
    if is_key:
        flags |= 0x80
    return struct.pack(
        '>BBHIIBBHHB', 0x80,
        (0x80 if marker else 0) | RTP_VIDEO_PT,
        sequence & 0xFFFF, timestamp & 0xFFFFFFFF, ssrc & 0xFFFFFFFF,
        flags, frame_id & 0xFF, packet_id, max(0, packet_count - 1),
        referenced_frame_id & 0xFF) + payload


def cast_packet_count(length):
    """How many packets one access unit of `length` bytes becomes.

    Separate from `cast_packets` because the answer is needed *before* the
    encryption, and it can be: AES-CTR is length preserving, so the ciphertext is
    exactly as long as the plaintext. That is what lets the sender claim its
    sequence numbers under the lock while leaving the keystream work outside it.
    A zero-length unit still sends one header-only packet.
    """
    return max(1, -(-length // MAX_PAYLOAD))


def cast_packets(ciphertext, frame_id, is_key, referenced_frame_id, ssrc,
                 sequence, timestamp):
    """Slice one encrypted frame into packets; returns (packets, next sequence).

    The whole access unit is encrypted before slicing -- one nonce per frame,
    which is why the ciphertext is exactly as long as the plaintext and only the
    last packet is short. `sequence` advances on every packet *including* a
    resend, because the receiver's reorder buffer counts arrivals rather than
    ids. A zero-length frame still sends one header-only packet.
    """
    count = cast_packet_count(len(ciphertext))
    out = []
    for index in range(count):
        chunk = ciphertext[index * MAX_PAYLOAD:(index + 1) * MAX_PAYLOAD]
        out.append(cast_packet(chunk, ssrc, sequence, timestamp, frame_id,
                               is_key, index, count, referenced_frame_id,
                               marker=(index == count - 1)))
        sequence = (sequence + 1) & 0xFFFF
    return out, sequence


def ntp_timestamp(now=None):
    """32.32 fixed point from the NTP epoch. The receiver pairs this with an
    RTP timestamp, so both clocks must be read at the same instant."""
    now = time.time() if now is None else now
    seconds = int(now)
    fraction = int((now - seconds) * 0x100000000) & 0xFFFFFFFF
    return ((seconds + NTP_UNIX_OFFSET) << 32) | fraction


def rtcp_sender_report(ssrc, ntp, timestamp, packets, octets):
    """28 bytes. No report, no picture -- see SR_INTERVAL."""
    return struct.pack('>BBHIQIII', 0x80, 200, 6, ssrc & 0xFFFFFFFF, ntp,
                       timestamp & 0xFFFFFFFF, packets & 0xFFFFFFFF,
                       octets & 0xFFFFFFFF)


def parse_rtcp(data, media_ssrc):
    """The events in one compound RTCP packet that are addressed to us.

    ('picture-loss',), ('checkpoint', frame-id-8, playout-delay-ms) or
    ('nack', frame-id-8, packet-id, bitmask). The receiver's feedback is RTCP on
    the media socket, not a message on the control plane: payload-specific
    (206) with FMT 1 is a picture loss, and FMT 15 carrying the ASCII word
    CAST is the acknowledgement.

    The loss fields that follow the CAST word are the receiver asking for
    specific packets back, and they are parsed rather than skipped: this docstring
    used to say "we do not retransmit individual packets, so per-packet loss
    tells us nothing", which was a claim about the protocol rather than about us
    and it was wrong -- openscreen's
    `cast/streaming/impl/sender_impl.cc:556`
    (`OnReceiverIsMissingPackets`) is exactly a retransmit path, and its own
    comment on the feedback loop counts two round trips, one to learn about the
    loss and one to repair it. See `_CastStreamSender._retransmit`.

    A nack always arrives behind a checkpoint in the same block, and its frame id
    is relative to *that* checkpoint (`ExpandGreaterThan` in
    `cast/streaming/impl/compound_rtcp_parser.cc:507`), not to the newest frame
    we sent -- which is why the caller expands it with `expand_frame_id_after`
    and not with `expand_frame_id`.
    """
    events = []
    position = 0
    while position + 4 <= len(data):
        head = data[position]
        if head >> 6 != 2:
            break
        ptype = data[position + 1]
        length = (struct.unpack('>H', data[position + 2:position + 4])[0]
                  + 1) * 4
        body = data[position + 4:position + length]
        position += length
        if ptype != 206 or len(body) < 8:
            continue                # RR / XR / SDES / BYE: nothing to act on
        if struct.unpack('>I', body[4:8])[0] != media_ssrc:
            continue                # about somebody else's stream
        fmt = head & 0x1F
        if fmt == 1:
            events.append(('picture-loss',))
        elif fmt == 15 and len(body) >= 16 and body[8:12] == b'CAST':
            events.append(('checkpoint', body[12],
                           struct.unpack('>H', body[14:16])[0]))
            losses = body[13] * 4
            for index in range(16, min(len(body), 16 + losses) - 3, 4):
                events.append(('nack', body[index],
                               struct.unpack('>H', body[index + 1:index + 3])[0],
                               body[index + 3]))
    return events


def expand_frame_id(id8, latest):
    """Widen the 8-bit id a receiver reports back to a full frame id.

    A frame only carries its low byte on the wire, so "17" means 17, 273 or 529
    -- whichever is within 256 of what we last sent. Get this wrong at a 256
    boundary and an acknowledgement pops nothing, the window fills, and the
    mirror sheds every frame from then on.
    """
    if latest < 0:
        return id8
    candidate = (latest & ~0xFF) | id8
    return candidate - 256 if candidate > latest else candidate


def expand_frame_id_after(id8, anchor):
    """Widen a NACKed 8-bit frame id against the checkpoint it came with.

    The *opposite* convention to `expand_frame_id`, and the difference is not
    cosmetic. A checkpoint says "everything up to here arrived", so its id is at
    most the newest thing we sent. A loss field says "this one did not arrive",
    and that frame is normally *newer* than the checkpoint in the same block --
    the receiver reports what it has already accepted, then what it is still
    waiting for. Widening a loss id with the "at most" rule therefore returns a
    frame 256 in the past, which is a frame we no longer hold, so every NACK
    would be silently dropped as unrecognised.

    openscreen spells this `FrameId::ExpandGreaterThan(other)`: same low-byte
    splice, then add 256 if the result is not strictly greater than the anchor.
    """
    candidate = (anchor & ~0xFF) | id8
    return candidate + 256 if candidate <= anchor else candidate


class _CastStreamSender(object):
    """Access units in, a picture on a television out.

    Threading is deliberately lopsided: the encoder's pump thread is the only
    caller of `feed`, and it does the sending inline -- queueing frames for a
    sender thread would just add a buffer whose contents are already stale by
    the time they leave. One background thread owns the other half (the
    receiver's feedback, sender reports, kickstart probes and the Cast
    keepalive), because all four are timers rather than data.

    There *is* a retransmit path, and this docstring used to say the opposite:
    "There is no retransmit path on purpose. This is a live desktop: the next
    frame supersedes a lost one within 42 ms, and the protocol's own recovery
    (checkpoint, PLI, the next one-second GOP) is what the reference relies on
    too." The last clause was the load-bearing one and it was false --
    openscreen's `cast/streaming/impl/sender_impl.cc:556`
    (`OnReceiverIsMissingPackets`) marks the NACKed packets for resend and asks
    the packet router to send them, and the receiver's per-packet loss fields
    exist precisely to drive it. The "next frame supersedes it" half was also
    wrong on its own terms: a frame that arrives with a hole in it is not
    superseded, it is undecodable, and the receiver then waits for the next IDR
    -- up to a second of frozen picture to repair thirty packets we still had.

    Retransmit is still *not* the primary recovery: the window is one and a half
    frames wide, so anything older than that has been shed and cannot be
    repaired. What it buys is the common case -- a single datagram lost on the
    way to a television that is otherwise keeping up.
    """

    def __init__(self, control, sock, address, ssrc, receiver_ssrc, key,
                 iv_mask, on_lost=None):
        self._control = control
        self._sock = sock
        self._address = address
        self.ssrc = ssrc
        self._receiver_ssrc = receiver_ssrc
        self._cipher = Aes128Ctr(key)
        self._iv_mask = iv_mask
        self._on_lost = on_lost
        self._units = _AccessUnits()
        self._frame_id = 0
        self._sequence = secrets.randbelow(0x10000)
        # (frame id, every packet of the frame, rtp timestamp, monotonic send
        # time, bytes retained for that frame). The send time is what makes the
        # window a duration rather than a count; see `_window_full`. The packets
        # are what makes a NACK answerable, and the byte count is what bounds
        # keeping them; see `_evict_retransmit_buffer`.
        self._in_flight = deque()
        #: Bytes of un-acknowledged packets we are holding copies of, against
        #: RETRANSMIT_BUFFER_BYTES.
        self._retained = 0
        #: Smoothed round trip in milliseconds, from checkpoint acks. None
        #: until the receiver has acknowledged something.
        self._rtt_ms = None
        self._awaiting_key = False
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = None
        self._t0 = time.monotonic()
        self._timestamp = 0
        self._octets = 0
        self._last_sent = self._t0
        self._last_report = 0.0
        self._last_ping = 0.0
        self._last_pong = time.time()
        self._reported = False
        self.alive = False
        self.acked = -1
        self.playout_delay = 0
        #: The counting surface `_Broadcaster` offers, so the pump and `stats()`
        #: do not care which target produced these bytes.
        self.chunks = 0
        self.bytes = 0
        self.drops = 0
        self.frames = 0
        self.packets = 0
        #: Bytes actually pushed onto the media socket, resends included. This is
        #: the low-latency channel's answer to `delivered`: `bytes` above is what
        #: the encoder made, and the two part company by exactly the amount of
        #: repair traffic -- which is the number worth seeing, because it is the
        #: only evidence on our side that the wireless is dropping datagrams.
        self.socket_bytes = 0
        #: Packets resent because a receiver asked for them.
        self.retransmits = 0
        #: NACKs we declined because the packet was sent less than one round trip
        #: ago. openscreen's rule: the receiver may have written the report while
        #: the packet was still in flight, and resending it then is redundant.
        self.retransmit_stale = 0
        #: Frames a NACK asked about that we no longer hold -- either shed by the
        #: window or evicted from the repair buffer. Both mean the same thing to
        #: the receiver: the next IDR is the recovery.
        self.retransmit_gone = 0

    # -- handshake -----------------------------------------------------------

    @classmethod
    def open(cls, host, port, width, height, fps, bitrate, timeout=10.0,
             on_lost=None):
        """Go all the way to a socket the receiver is listening on.

        Deliberately before the encoder starts: every failure in here is a
        reason to take the compatible LOAD path instead, and unwinding a live
        ffmpeg plus an HTTP server to make that decision would be the messy
        version of the same call.
        """
        control = _CastSender(host, port)
        control.connect()
        try:
            control.launch_mirroring(MIRROR_APP_ID, timeout=timeout)
            key, iv_mask = aes_material()
            ssrc = secrets.randbits(32) | 1
            control.send_offer(build_offer(ssrc, key, iv_mask, width, height,
                                           fps, bitrate))
            udp_port, receiver_ssrc = parse_answer(
                control.await_answer(timeout=max(2.0, timeout)))
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            # Bound, never connected: the receiver answers from a source port
            # of its own choosing, and a connected socket makes the kernel drop
            # those packets before we ever see a checkpoint.
            sock.bind(('', 0))
            return cls(control, sock, (host, udp_port), ssrc, receiver_ssrc,
                       key, iv_mask, on_lost=on_lost)
        except Exception:
            # An app we launched and then abandoned is worse than no app: the
            # next session would meet this one as the stale instance it has to
            # stop first. Only worth attempting once a socket exists.
            if control.sock is not None:
                try:
                    control.close_mirroring()
                except Exception as e:
                    logger.debug('could not leave the mirroring app: %s', e)
            _close_quietly(control)
            raise

    def start(self):
        self.alive = True
        self._last_pong = time.time()
        self._thread = threading.Thread(target=self._loop, daemon=True,
                                        name="CAST_STREAM_LOOP")
        self._thread.start()

    # -- what the pump thread calls ------------------------------------------

    def feed(self, chunk):
        self.chunks += 1
        self.bytes += len(chunk)
        for unit in self._units.feed(chunk):
            self._send_unit(unit)

    def flush(self):
        for unit in self._units.flush():
            self._send_unit(unit)

    def _rtt_estimate(self):
        """The round trip, or the one we assume before the first checkpoint.

        Half of MIN_WINDOW_MS, so that a sender with no measurement yet sizes its
        window at exactly the floor instead of at zero. One definition, because
        both the window and the stale-NACK rule below would otherwise invent
        their own idea of "we do not know yet".
        """
        return MIN_WINDOW_MS / 2.0 if self._rtt_ms is None else self._rtt_ms

    def _window_ms(self):
        """How long an un-acknowledged frame may stay in flight, in ms.

        openscreen's `clamp(2 x RTT, 66 ms, targetDelay / 3)`, transcribed.
        Callers hold `_lock`.
        """
        return min(max(2.0 * self._rtt_estimate(), MIN_WINDOW_MS),
                   TARGET_DELAY_MS / 3.0)

    def _window_full(self, now):
        """Whether sending one more frame would overrun the window.

        The unit is *time*, not frames, and that is the whole point: at a
        200 ms target delay this protocol allows about 66 ms of
        un-acknowledged data, which is one and a half of our frames. Counting
        twelve of them instead handed the receiver six times the queue the
        negotiation asked for, and every frame sitting in that queue is latency
        we have already paid for and cannot get back.

        Callers hold `_lock`.
        """
        if not self._in_flight:
            return False
        if self._frame_id - self._in_flight[0][0] >= MAX_IN_FLIGHT_FRAMES:
            # The 8-bit frame id in the Cast header has stopped being
            # unambiguous. Nothing to do with burst size -- just refuse to
            # guess which frame an ack is talking about.
            return True
        return (now - self._in_flight[0][3]) * 1000.0 >= self._window_ms()

    def _send_unit(self, unit):
        is_key = _unit_is_key(unit)
        # One clock read for the whole frame: the window check, the RTP
        # timestamp and `_last_sent` all describe this same moment, and
        # encrypting plus packetising in between is tens of microseconds.
        now = time.monotonic()
        if self._awaiting_key and not is_key:
            # Shed *whole* frames: half a picture stays on the screen until the
            # next IDR, which is worse than the last complete one still there.
            self.drops += 1
            return
        claimed = self._begin_frame(now, cast_packet_count(len(unit)))
        if claimed is None:
            # Even a key frame waits when the window is full -- the next GOP is
            # a second away and the checkpoint will free the slots by then.
            self._awaiting_key = True
            self.drops += 1
            return
        frame_id, sequence = claimed
        timestamp = int((now - self._t0) * VIDEO_CLOCK) & 0xFFFFFFFF
        cipher = self._cipher.crypt(frame_iv(self._iv_mask, frame_id), unit)
        packets, _ = cast_packets(
            cipher, frame_id, is_key, frame_id if is_key else frame_id - 1,
            self.ssrc, sequence, timestamp)
        pushed = 0
        try:
            for packet in packets:
                self._sock.sendto(packet, self._address)
                pushed += len(packet)
        except OSError:
            # Teardown closed the media socket under us. Counting a frame the
            # receiver never got would be the worse lie; hand the slot back. The
            # sequence numbers stay burned: a gap in them is what a lost packet
            # already looks like, whereas reusing one is not.
            with self._lock:
                if self._frame_id - 1 == frame_id:
                    self._frame_id = frame_id
            self._awaiting_key = True
            raise
        self._end_frame(frame_id, packets, timestamp, now, cipher, pushed,
                        is_key)

    def _begin_frame(self, now, count):
        """Claim a frame id and `count` sequence numbers; None if the window is
        full. Callers must not hold `_lock`.

        One atomic step because the window test and both counters describe the
        same decision, and because the sequence counter has had two allocators
        since retransmission landed: this thread and the feedback thread. Handing
        the same RTP sequence number out twice is not a cosmetic collision -- the
        receiver's reorder buffer counts arrivals rather than ids, so a duplicate
        reads as a packet that was delivered and then lost, and the repair it
        asks for costs more than the loss it reports.
        """
        with self._lock:
            if self._window_full(now):
                return None
            frame_id = self._frame_id
            self._frame_id += 1
            sequence = self._sequence
            self._sequence = (sequence + count) & 0xFFFF
            return frame_id, sequence

    def _end_frame(self, frame_id, packets, timestamp, now, cipher, pushed,
                   is_key):
        with self._lock:
            self._in_flight.append((frame_id, packets, timestamp, now,
                                    len(cipher)))
            self._retained += len(cipher)
            self._evict_retransmit_buffer()
            self._timestamp = timestamp
            self._octets = (self._octets + len(cipher)) & 0xFFFFFFFF
            self.packets += len(packets)
            self.frames += 1
            self.socket_bytes += pushed
            self._last_sent = now
            if is_key:
                self._awaiting_key = False
            first = not self._reported
        if first:
            self._send_report()

    def _evict_retransmit_buffer(self):
        """Give up the ability to repair our oldest frames, not the frames.

        The in-flight record has to stay -- the window arithmetic and the
        checkpoint pop both read it -- so what goes is the copy of the packets,
        and a NACK for a frame with no copy is then answered the same way as one
        for a frame we never sent: ignored, and the next IDR is the recovery.
        Callers hold `_lock`.
        """
        position = 0
        while self._retained > RETRANSMIT_BUFFER_BYTES \
                and position < len(self._in_flight):
            entry = self._in_flight[position]
            if entry[1]:
                self._retained -= entry[4]
                self._in_flight[position] = (entry[0], (), entry[2], entry[3], 0)
                self.retransmit_gone += 1
            position += 1

    # -- timers and feedback (the one background thread) ---------------------

    def _send_report(self):
        self._reported = True
        self._last_report = time.monotonic()
        with self._lock:
            packet = rtcp_sender_report(self.ssrc, ntp_timestamp(),
                                        self._timestamp, self.packets,
                                        self._octets)
        try:
            self._sock.sendto(packet, self._address)
        except OSError as e:
            logger.debug('sender report lost: %s', e)

    def _kickstart(self):
        with self._lock:
            packet = self._in_flight[-1][1][-1] if self._in_flight \
                and self._in_flight[-1][1] else None
            if packet is None:
                return
            self._sequence = (self._sequence + 1) & 0xFFFF
            resend = self._restamped(packet, self._sequence)
            self.socket_bytes += len(resend)
        try:
            self._sock.sendto(resend, self._address)
        except OSError:
            pass

    @staticmethod
    def _restamped(packet, sequence):
        """A copy of `packet` wearing a fresh RTP sequence number.

        Everything else -- the Cast header's frame and packet ids, the marker
        bit, the payload -- stays byte for byte, because that is how the receiver
        recognises the packet it asked for. Only the arrival counter moves.
        """
        return packet[:2] + struct.pack('>H', sequence) + packet[4:]

    def _on_rtcp(self, data):
        #: One clock read for the whole compound packet: the staleness rule
        #: below compares a frame's send time against the moment this feedback
        #: *arrived*, and per-event reads would make that comparison depend on
        #: how much parsing happened in between.
        arrived = time.monotonic()
        groups = {}
        anchor = None
        for event in parse_rtcp(data, self.ssrc):
            if event[0] == 'picture-loss':
                self._awaiting_key = True
            elif event[0] == 'checkpoint':
                with self._lock:
                    anchor = expand_frame_id(event[1], self._frame_id - 1)
                    acked = anchor
                    newest = None
                    while self._in_flight and self._in_flight[0][0] <= acked:
                        newest = self._in_flight.popleft()
                        if newest is not None:
                            self._retained -= newest[4]
                    if newest is not None:
                        # The newest frame this ack covers is the freshest round
                        # trip; the older ones have been waiting behind it. The
                        # window is a duration now, so this number is what
                        # sizes it -- and a sample above RTT_MAX_MS is a lost
                        # checkpoint rather than a slow network, so it is
                        # dropped instead of widening the queue for nothing.
                        sample = (arrived - newest[3]) * 1000.0
                        if 0.0 <= sample <= RTT_MAX_MS:
                            self._rtt_ms = (
                                sample if self._rtt_ms is None
                                else self._rtt_ms + RTT_SMOOTHING
                                * (sample - self._rtt_ms))
                    self.acked = max(self.acked, acked)
                    self.playout_delay = event[2]
            elif event[0] == 'nack':
                if anchor is None:
                    # Loss fields without the checkpoint they are relative to.
                    # Our own parser cannot produce that, so this is a receiver
                    # speaking a shape we have not seen -- guessing the frame
                    # would mean resending a picture 256 frames out.
                    continue
                frame_id = expand_frame_id_after(event[1], anchor)
                groups.setdefault(frame_id, []).append((event[2], event[3]))
        if groups:
            self._retransmit(groups, arrived)

    def _retransmit(self, groups, arrived):
        """Answer a receiver's per-packet loss report.

        openscreen's `SenderImpl::OnReceiverIsMissingPackets`
        (`cast/streaming/impl/sender_impl.cc:556`), transcribed rule by rule:

        * a packet sent less than one round trip before this feedback arrived is
          *not* resent. The receiver may have written the report while the packet
          was still in flight, and resending then is redundant traffic that
          competes with the original. This is the rule that keeps a NACK loop
          from becoming self-sustaining.
        * a frame we no longer hold is skipped whole, not per packet. The
          reference names out-of-order RTCP as the reason a receiver can ask
          about a frame the sender does not have; ours are shed by the window or
          evicted from the repair buffer. Either way the next IDR is the
          recovery, and there is nothing to gain from saying so forty times.
        * `ALL_PACKETS_LOST` means every packet of the frame, and its bit vector
          is not read at all.
        * otherwise bit *i* of the vector adds packet `id + 1 + i`, lowest bit
          first (`compound_rtcp_parser.cc:507`).
        * an id past the end of the frame is warned about and dropped rather than
          trusted.

        What is deliberately *not* transcribed is the pacing. The reference hands
        its resends to a packet router that interleaves them with new frames; we
        send the burst inline on the feedback thread, bounded by
        MAX_RETRANSMIT_PER_FEEDBACK. At one and a half frames of window there is
        no queue to interleave with.
        """
        plan = []
        with self._lock:
            too_recent = arrived - self._rtt_estimate() / 1000.0
            budget = MAX_RETRANSMIT_PER_FEEDBACK
            for frame_id, losses in sorted(groups.items()):
                record = None
                for entry in self._in_flight:
                    if entry[0] == frame_id:
                        record = entry
                        break
                if record is None or not record[1]:
                    self.retransmit_gone += 1
                    continue
                packets, sent_at = record[1], record[3]
                if sent_at > too_recent:
                    self.retransmit_stale += 1
                    continue
                wanted = []
                for packet_id, bits in losses:
                    if packet_id == ALL_PACKETS_LOST:
                        wanted.extend(range(len(packets)))
                        continue
                    wanted.append(packet_id)
                    identifier, remaining = packet_id, bits
                    while remaining:
                        identifier += 1
                        if remaining & 1:
                            wanted.append(identifier)
                        remaining >>= 1
                beyond = [i for i in wanted if i >= len(packets)]
                if beyond:
                    logger.debug('ignoring %d NACKed packet id(s) that frame %d '
                                 'does not have (%d packets)', len(beyond),
                                 frame_id, len(packets))
                chosen = 0
                for packet_id in sorted(set(wanted)):
                    if budget <= 0:
                        break
                    if packet_id >= len(packets):
                        continue
                    budget -= 1
                    chosen += 1
                    self._sequence = (self._sequence + 1) & 0xFFFF
                    resend = self._restamped(packets[packet_id], self._sequence)
                    self.socket_bytes += len(resend)
                    plan.append(resend)
                # Per frame, not `len(plan)`: this is inside the loop over
                # frames, and accumulating the running total would report the
                # first frame once, the second twice and so on.
                self.retransmits += chosen
        for packet in plan:
            try:
                self._sock.sendto(packet, self._address)
            except OSError:
                return
        if plan:
            logger.debug('retransmitted %d packet(s) for %d frame(s)', len(plan),
                         len(groups))

    def _keepalive(self):
        self._last_ping = time.monotonic()
        try:
            self._control.ping()
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.debug('keepalive could not be sent: %s', e)
            return
        while True:
            message = self._control.poll()
            if message is None:
                return
            if message.get('payload_type') != 0:
                continue
            try:
                data = json.loads(message.get('payload_utf8') or '{}')
            except ValueError:
                continue
            if data.get('type') == 'PONG':
                self._last_pong = time.time()
                return

    def _loop(self):
        self._sock.settimeout(0.1)
        while not self._stop.is_set():
            now = time.monotonic()
            try:
                data, peer = self._sock.recvfrom(65536)
            except socket.timeout:
                data = None
            except OSError:
                return
            if data and peer[0] == self._address[0]:
                self._on_rtcp(data)
            if now - self._last_report >= SR_INTERVAL:
                self._send_report()
            if now - self._last_sent >= KICKSTART_INTERVAL:
                self._kickstart()
                # Re-arm the probe ourselves: recvfrom already paced this loop,
                # so without this every 100 ms would resend the same packet.
                self._last_sent = now
            if now - self._last_ping >= CONTROL_PING_SECONDS:
                self._keepalive()
            if time.time() - self._last_pong > CONTROL_LOSS_SECONDS:
                self.alive = False
                self._stop.set()
                if self._on_lost is not None:
                    self._on_lost('电视不再应答保活（可能已关机或换了网络）')
                return

    # -- status and teardown -------------------------------------------------

    def clients(self):
        return 1 if self.alive else 0

    def in_flight(self):
        with self._lock:
            return len(self._in_flight)

    def rtt_ms(self):
        """The smoothed round trip, or None before the first checkpoint.

        A reading, not `_rtt_estimate()`: the statistics card must be able to say
        "the receiver has acknowledged nothing yet", and the estimate hides that
        behind a floor value it invented for the window arithmetic.
        """
        with self._lock:
            return round(self._rtt_ms, 1) if self._rtt_ms is not None else None

    def stop(self):
        if self._stop.is_set():
            return
        self._stop.set()
        self.alive = False
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(1.0)
        self._control.close_mirroring()

    def close(self):
        self._stop.set()
        try:
            self._sock.close()
        except OSError:
            pass
        self._control.close()


def unicode_text(value):
    return value if isinstance(value, str) else str(value)


# -- the WebRTC target --------------------------------------------------------
#
# The fifth output target, and the third one that takes the media plane off
# HTTP: the page opens a real RTCPeerConnection, the server answers with an
# offer, and the encoder's H.264 access units ride out as SRTP through
# aiortc -- which never sees a decoded frame, because the track hands it
# whole Annex-B pictures and it repacketises what it is given. That opacity
# is the point: a transcoder here would add a decode + encode to the one
# pipeline on this list that exists to remove latency.
#
# Why aiortc and not ffmpeg's own `-whip`: `-whip` is a *client* muxer -- it
# pushes to a WHIP endpoint, and here Macast is the endpoint. A WHIP server
# (ICE, DTLS, SRTP, RTP packetisation, congestion feedback) would still have
# to exist, and ffmpeg does not provide one; aiortc does, is importable
# from a single-file plugin, and forwards our access units without decoding
# them. The price is this file's only optional dependency, and everything
# below is shaped around paying it without letting it spread: nothing at
# module scope imports aiortc, and the failure stays recoverable.

#: The last successful `import aiortc; import av`, kept as a *record* for
#: readers and for the suite. The probe itself never short-circuits on it: the
#: matching failure is not cached, and neither can the win be, because the two
#: halves of this feature are "install it, come back, no restart" and "remove
#: it, come back, it is really gone". A cached yes would outlive the uninstall
#: and turn the card into a promise nobody kept. Re-running the import of an
#: already-loaded package is a `sys.modules` lookup, so this costs nothing.
_WEBRTC_MODULES = None
_WEBRTC_IMPORT_ERROR = None
#: The last failure text we put in the log. `extras_state()` re-probes on every
#: poll of the mirror card, so without this the same ImportError would be
#: written about once a second forever -- §4.2's "重试把日志刷爆" in a new place.
_WEBRTC_LOGGED_ERROR = None
#: The viewer track class, built once from the real aiortc base class.
_WEBRTC_VIEWER_CLASS = None
#: Whether this process has already tried to re-attach a landed extras tree.
#: One attempt per process, not one per probe: the card polls the probe about
#: once a second, and a machine with nothing installed would otherwise stat the
#: config directory forever. See `_restore_extras_path`.
_EXTRAS_PATH_RESTORED = False

# -- the extras bundle, and the one button that fetches it -------------------
#
# `av` alone is forty-some MB of the .app, and the five packages behind the
# WebRTC target are nobody else's download, so they left the default artefact
# (P9, `docs/Casting-Suite-Plan.md` §6.7). What is left here is the seam: one
# address rule (in `plugin_repo`, so the mirror switch and the index share it),
# one manifest shape (here, so the builder and the installer cannot disagree),
# and one chain that turns a zip into an *importable* tree.
#
# That chain is the most dangerous thing this app can be asked to do -- it puts
# code on `sys.path` -- so it is gated as code execution (§4.7b: 落代码的只认
# 令牌, loopback 不算), it verifies every member against a sha256 before
# unpacking anything, it refuses members the manifest does not list, and every
# removal goes to `.trash` the way a plugin uninstall does (§10).

#: Where the tree lands, and what vouches for it. The directory is named by ABI
#: so two Macast builds on one machine never share it, and so a bundle written
#: for another interpreter can be refused without touching the good one's files.
EXTRAS_DIR_NAME = 'webrtc_extras'
EXTRAS_MANIFEST_NAME = 'manifest.json'
#: The manifest's fields, in the order the builder writes them. `platform` and
#: `machine` are the asset's two halves of "your machine"; they are *not* the
#: ABI dict's keys, which is why `check_manifest` names both sides of a mismatch
#: rather than echoing one word back.
EXTRAS_MANIFEST_FIELDS = ('python', 'platform', 'machine', 'macast_version',
                          'files')
#: Per-file entry fields. One owner for this tuple is the whole defence against
#: a builder that writes `digest` while the installer asks for `sha256`.
EXTRAS_FILE_FIELDS = ('path', 'sha256', 'bytes')
#: The five things that happen between a click and a working target. Same step
#: machine as the BlackHole assisted install, same rules: a done step refuses
#: re-entry, so the bar never runs backwards.
EXTRAS_STEPS = (('download', '下载依赖包'),
                ('verify', '校验清单与 sha256'),
                ('unpack', '解包'),
                ('land', '挂到模块搜索路径'),
                ('probe', '确认 aiortc 与 av 导入成功'))
#: Removal has its own two steps because those are the two things that happen.
#: Sharing EXTRAS_STEPS would draw a download/verify/unpack ladder over a move.
EXTRAS_REMOVE_STEPS = (('trash', '移到废纸篓'),
                       ('unload', '从模块搜索路径收掉'))
#: Asset-name vocabulary. These are the strings in the release asset's name, so
#: changing one is a release-artifact change, not a refactor.
EXTRAS_OS_KEYS = {'darwin': 'macos', 'win32': 'windows', 'linux': 'linux'}
EXTRAS_ARCH_ALIASES = {'arm64': 'arm64', 'aarch64': 'arm64',
                       'x86_64': 'x86_64', 'amd64': 'x86_64'}
EXTRAS_INSTALL_LABEL = '一键安装 WebRTC 依赖'
EXTRAS_UNINSTALL_LABEL = '移除 WebRTC 依赖'
#: Top-level modules to purge when the bundle is removed or replaced. Anything
#: else loaded from under our directory is found by path, not by name.
_EXTRAS_MODULE_ROOTS = ('aiortc', 'av')
_EXTRAS_BUSY = threading.Event()
_EXTRAS_LOCK = threading.Lock()
_EXTRAS_PROGRESS = None

WEBRTC_INSTALL_HINT = (
    '这条通道需要两个可选依赖：aiortc 与 av。它们不再打进安装包（`av` 一个包就是'
    '四十多 MB，而这一条通道是少数人用的），所以从这里分两种机器：打包安装的 Macast '
    '点下面的「' + EXTRAS_INSTALL_LABEL + '」，本机从源码跑的请在运行 '
    'Macast 的那个 Python 里执行「pip install aiortc av」。之后回到这里重新选择本目标'
    '即可（失败不会被缓存，不需要重启 Macast）。')


def _webrtc_modules():
    """(aiortc, av), or None while the optional packages are unavailable.

    Lazy on purpose, twice over. At module scope a `pip` library would take
    the whole plugin down on any machine that never installed it -- which,
    since P9, is every packaged machine until the user asks for it -- and a run
    from source without the pip line must keep every other mirror target
    working. And inside this function the import is the *feature probe*:
    "is this target usable" is answered by actually loading the packages,
    the same way `has_hardware_encoder` answers by asking ffmpeg rather than by
    trusting a table.

    The restore call underneath it is what makes the promise in the comment
    above `_WEBRTC_MODULES` ("install it, come back, no restart") survive the
    next launch: `extras_state()` reports `installed` from a file on disk, so
    without it a restarted Macast says 已安装 while the probe says
    `No module named 'aiortc'`, and the only recovery the card offers is
    another twenty-six megabyte download for a tree that is already here.
    """
    global _WEBRTC_MODULES, _WEBRTC_IMPORT_ERROR, _WEBRTC_LOGGED_ERROR
    _restore_extras_path()
    try:
        import aiortc
        import av
    except Exception as exc:            # ImportError, or a broken wheel
        _WEBRTC_IMPORT_ERROR = exc
        text = '%s: %s' % (type(exc).__name__, exc)
        if text != _WEBRTC_LOGGED_ERROR:
            _WEBRTC_LOGGED_ERROR = text
            logger.info('the WebRTC packages are not importable: %s', exc)
        return None
    _WEBRTC_MODULES = (aiortc, av)
    _WEBRTC_IMPORT_ERROR = None
    _WEBRTC_LOGGED_ERROR = None
    return _WEBRTC_MODULES


def _machine_name():
    """`platform.machine()` behind a name the ABI seam can shadow around."""
    return platform.machine()


def extras_abi(platform=None, machine=None):
    """{'os', 'arch', 'python'} for a machine -- answers when told nothing.

    The two arguments are the §4.8/Part 40 seam: the card asks "is there a
    bundle for *this kind of machine*", and that must not be decided by which
    runner happens to execute the code. The python tag is deliberately *not*
    overridable -- it is what the running interpreter can load, not a claim
    about some other one.
    """
    if platform is None:
        platform = sys.platform
    if machine is None:
        machine = _machine_name()
    return {'os': EXTRAS_OS_KEYS.get(platform, ''),
            'arch': EXTRAS_ARCH_ALIASES.get(machine.lower(),
                                            machine.lower()),
            'python': 'cp%d%d' % sys.version_info[:2]}


def extras_asset_name(abi=None, version=None):
    """The one release asset for this ABI -- '' when there is no such thing.

    Empty is an answer, not a crash: a machine outside the four platforms we
    publish for should read "no bundle here" on the card, not an address that
    cannot exist.

    `version` is the §4.8/Part 40 seam for the builder: it is called there with
    the version CI is publishing under, because `Setting.get_version()` is a
    value `Macast.py` sets at startup and a build script never starts one. The
    app leaves it alone and gets the running version.
    """
    if abi is None:
        abi = extras_abi()
    if not abi['os']:
        return ''
    if version is None:
        version = Setting.get_version()
    return plugin_repo.extras_asset_name(abi['os'], abi['arch'], abi['python'],
                                         version)


def extras_dir(abi=None):
    """Where this ABI's tree lands, under the config directory."""
    if abi is None:
        abi = extras_abi()
    return os.path.join(utils.SETTING_DIR, EXTRAS_DIR_NAME,
                        '%s-%s-%s' % (abi['os'], abi['arch'], abi['python']))


def extras_manifest(abi, version, entries):
    """The one writer of the manifest's shape, for the builder to call.

    The builder's job is to walk a tree and hash it; which fields exist and in
    what order is decided here, in the same file as the reader that refuses a
    missing one. Otherwise the two drift the first time either side is edited,
    and the failure is a bundle every machine turns down.
    """
    return {'python': abi['python'], 'platform': abi['os'],
            'machine': abi['arch'], 'macast_version': version,
            'files': list(entries)}


def _extras_member_ok(path):
    """True for a relative member that stays inside the directory we own.

    `:` is checked because a Windows drive prefix survives a POSIX run's
    `splitdrive` as ordinary text -- and this table is read by both platforms.
    """
    if not path or '\\' in path or ':' in path or path.startswith('/'):
        return False
    for part in path.split('/'):
        if part in ('', '.', '..'):
            return False
    return True


def check_manifest(manifest, abi=None, version=None):
    """(ok, refusal) -- is this bundle for this machine, and is it verifiable?

    Ordered on purpose. A manifest missing a field is answered with that
    field's name (a KeyError here would reach the user as "安装失败" with
    nothing to read), and a field that disagrees is answered with *both* values,
    because "this bundle is not for your machine" with only half the comparison
    is not a sentence anyone can act on.

    The digests are not validated -- an entry whose sha256 is nonsense fails
    when it is compared against a real file, which is the same answer and one
    less place to be wrong about what a hex string is.

    `version` is the same seam `extras_asset_name` takes, for the same reason:
    the build script self-checks the zip it just wrote, and the process writing
    it has no startup version to read.
    """
    if abi is None:
        abi = extras_abi()
    own = {'python': abi['python'], 'platform': abi['os'],
           'machine': abi['arch'],
           'macast_version': Setting.get_version() if version is None
                              else version}
    for field in EXTRAS_MANIFEST_FIELDS:
        if field not in manifest:
            return False, ('依赖包清单缺少「%s」字段，无法判断这份包是给谁的。'
                           % field)
    for field in ('python', 'platform', 'machine', 'macast_version'):
        value = manifest[field]
        if value != own[field]:
            return False, ('这份依赖包是给 %s=%s 的，这台机器是 %s=%s。'
                           % (field, value, field, own[field]))
    entries = manifest['files']
    if not entries:
        return False, '依赖包清单里一个文件都没有，无法校验任何字节。'
    for entry in entries:
        for field in EXTRAS_FILE_FIELDS:
            if field not in entry:
                return False, ('依赖包清单里有文件缺少「%s」字段，无法校验。'
                               % field)
        if not _extras_member_ok(entry['path']):
            return False, ('依赖包清单里有一个不安全的文件路径：%s。'
                           % entry['path'])
    return True, ''


def _extras_tmpdir():
    """A scratch directory under the config directory, not in /tmp.

    Same reason the transcode writes its files next to the settings: the temp
    directory we are handed may be on another volume, may be noexec, and may
    not be where a user looks for "what did this app write".
    """
    return tempfile.mkdtemp(prefix='webrtc-extras-tmp-',
                            dir=utils.SETTING_DIR)


def _fetch_extras_zip(url, dest_path, on_bytes=None):
    """Download one candidate address to `dest_path`. (ok, refusal).

    `on_bytes(received, total)` is the card's progress feed. `total` comes from
    the response's own `Content-Length` and may be 0 -- an absent header is a
    slower bar, not a failure. The number is why this returns a sentence
    instead of a bool: a stream that stops early used to look like a success
    here (`got > 0`), and the user's next line was a sha256 complaint about a
    file we truncated ourselves.
    """
    try:
        import requests
    except ImportError:
        logger.info('requests is not importable; the extras cannot be fetched')
        return False, '这个 Macast 没有带 requests'
    try:
        response = requests.get(url, stream=True, timeout=60,
                                headers={'User-Agent': PKG_USER_AGENT})
    except Exception as exc:
        logger.info('the extras download failed (%s): %s', url, exc)
        return False, '%s: %s' % (type(exc).__name__, exc)
    got = 0
    total = 0
    why = ''
    try:
        if response.status_code != 200:
            logger.info('the extras download answered %s: %s',
                        response.status_code, url)
            return False, '依赖包那边回了 %s' % response.status_code
        try:
            total = int(response.headers.get('Content-Length') or 0)
        except (TypeError, ValueError):
            total = 0
        with open(dest_path, 'wb') as handle:
            for chunk in response.iter_content(chunk_size=65536):
                if not chunk:
                    continue
                handle.write(chunk)
                got += len(chunk)
                if on_bytes is not None:
                    on_bytes(got, total)
    except Exception as exc:
        logger.info('the extras download broke midway: %s', exc)
        why = '%s: %s' % (type(exc).__name__, exc)
    finally:
        response.close()
    if why:
        return False, '下到 %.1f MB 就断了（%s），重试会整份重来' % (
            got / 1048576.0, why)
    if not got:
        return False, '一个字节都没给'
    if total and got != total:
        # A body shorter than the header advertised is a dead stream that
        # raised nothing -- say which one, and that the retry starts over.
        return False, ('下到 %.1f MB 就断了（整份是 %.1f MB），重试会整份重来'
                       % (got / 1048576.0, total / 1048576.0))
    return True, ''


def _verify_extras_zip(zip_path, abi=None, version=None):
    """(manifest, refusal) -- read the table, then hold the zip to it.

    Three questions, all before anything is unpacked: does the manifest describe
    this machine, does every listed member exist and hash to what it claims, and
    does the zip contain nothing that is not listed. The last one is the
    half of the check that a builder with a bug (or a proxy with an idea)
    gets caught by -- "every file I put on `sys.path` is in the table".
    """
    try:
        with zipfile.ZipFile(zip_path) as archive:
            names = set(archive.namelist())
            if EXTRAS_MANIFEST_NAME not in names:
                return None, '下载的依赖包里没有找到 %s。' % EXTRAS_MANIFEST_NAME
            manifest = json.loads(
                archive.read(EXTRAS_MANIFEST_NAME).decode('utf-8'))
    except zipfile.BadZipFile:
        return None, '下载的依赖包不是一个有效的 zip。'
    except Exception as exc:
        return None, '下载的依赖包读不出清单：%s' % exc
    if not isinstance(manifest, dict):
        return None, '下载的依赖包清单不是一个对象。'
    ok, refusal = check_manifest(manifest, abi, version)
    if not ok:
        return None, refusal
    entries = {entry['path']: entry for entry in manifest['files']}
    present = names - {EXTRAS_MANIFEST_NAME}
    for path in sorted(entries):
        if path not in present:
            return None, '依赖包里缺少清单上的文件：%s' % path
    for path in sorted(present - set(entries)):
        return None, '依赖包里有一个清单没有列出的文件：%s' % path
    with zipfile.ZipFile(zip_path) as archive:
        for path in sorted(entries):
            data = archive.read(path)
            entry = entries[path]
            if len(data) != entry['bytes']:
                return None, ('依赖包里的 %s 是 %d 字节，清单说是 %d 字节。'
                              % (path, len(data), entry['bytes']))
            if hashlib.sha256(data).hexdigest() != entry['sha256']:
                return None, '依赖包里的 %s 与清单的 sha256 不一致。' % path
    return manifest, ''


def _unpack_extras_zip(zip_path, manifest, dest):
    """Write exactly the listed members under `dest`. Returns the file count.

    The manifest is written by the caller, not from the zip: what lands next to
    the tree must be the copy we verified, not a second read of the archive.
    """
    count = 0
    with zipfile.ZipFile(zip_path) as archive:
        for entry in manifest['files']:
            path = os.path.join(dest, *entry['path'].split('/'))
            parent = os.path.dirname(path)
            if parent and not os.path.isdir(parent):
                os.makedirs(parent)
            with open(path, 'wb') as handle:
                handle.write(archive.read(entry['path']))
            count += 1
    return count


def _trash_extras_tree(path):
    """Move a whole tree into `.trash/<stamp>/` -- reversible, like plugins."""
    if not os.path.isdir(path):
        return None
    stamp = time.strftime('%Y%m%d-%H%M%S')
    base = os.path.join(utils.SETTING_DIR, '.trash', stamp)
    target = os.path.join(base, os.path.basename(path))
    suffix = 0
    while os.path.exists(target):
        suffix += 1
        target = os.path.join(base, '%s-%d' % (os.path.basename(path),
                                               suffix))
    try:
        os.makedirs(base)
    except OSError:
        if not os.path.isdir(base):
            raise
    shutil.move(path, target)
    return target


def _extras_module_origin():
    """Where `aiortc` would be imported from right now, or '' if nowhere."""
    modules = _webrtc_modules()
    if modules is None:
        return ''
    return os.path.abspath(getattr(modules[0], '__file__', '') or '')


def _forget_webrtc_modules(under=''):
    """Drop every module loaded from our tree, and the record of the last good pair.

    `sys.path` changes do nothing to a package `sys.modules` already serves, so
    without this a removal would keep the target claiming to work -- and the
    cache would be the reason. `invalidate_caches()` is the other half: the
    directory may have just appeared or vanished, and `FileFinder` only
    re-lists on an mtime change (§4.2's "刚写进目录的那个 .py").
    """
    global _WEBRTC_MODULES, _WEBRTC_IMPORT_ERROR, _WEBRTC_VIEWER_CLASS
    _WEBRTC_MODULES = None
    _WEBRTC_IMPORT_ERROR = None
    _WEBRTC_VIEWER_CLASS = None
    prefix = (under + os.sep) if under else ''
    for name in list(sys.modules):
        module = sys.modules.get(name)
        if name.split('.')[0] in _EXTRAS_MODULE_ROOTS:
            sys.modules.pop(name, None)
            continue
        path = getattr(module, '__file__', None)
        if prefix and path and os.path.abspath(path).startswith(prefix):
            sys.modules.pop(name, None)
    importlib.invalidate_caches()


def _extras_landing(landing):
    """Put the tree on `sys.path` at the front, once, and remember where."""
    while landing in sys.path:
        sys.path.remove(landing)
    sys.path.insert(0, landing)
    importlib.invalidate_caches()


def _extras_installed(landing):
    """True when this ABI's slot holds a bundle we vouched for.

    One reader for that question, because two would drift: the card's
    `installed` and the restore below must never disagree about whether a tree
    is ours to put back on `sys.path`.
    """
    return os.path.isfile(os.path.join(landing, EXTRAS_MANIFEST_NAME))


def _restore_extras_path():
    """Re-attach a previously installed tree, the first time anything asks.

    `_extras_landing` was the only writer of `sys.path`, and the only thing that
    ever called it was the install's land step -- one process, once. So the
    landing was a promise about the *rest of this session*: press the button,
    the WebRTC target works, restart Macast, and the tree in
    `<config>/webrtc_extras/<abi>` is orphaned. The card then reads 已安装 with
    `ready` false, which is the exact lie §4.8 was written to end, and the only
    button next to it re-downloads twenty-six megabytes to do a `sys.path`
    insert.

    Asking from inside the probe is what makes the two answers agree: `ready` is
    defined by this import, so the restore happens before the question is
    answered, on the install path, on the card's poll, and on the next
    `Output=webrtc` start. One attempt per process, and only when the manifest
    is there -- after an uninstall the tree has moved to `.trash`, so there is
    nothing to re-attach and this stays out of the way.
    """
    global _EXTRAS_PATH_RESTORED
    if _EXTRAS_PATH_RESTORED:
        return
    _EXTRAS_PATH_RESTORED = True
    try:
        landing = extras_dir()
        if os.path.isdir(landing) and _extras_installed(landing):
            _extras_landing(landing)
    except Exception as exc:            # a config dir we cannot read is not a crash
        logger.info('the WebRTC extras tree could not be re-attached: %s', exc)


def install_webrtc_extras(progress=None):
    """(ok, message): fetch the bundle for this ABI and make it importable.

    Every refusal is paid for in the verify step, before a byte is written
    outside the scratch directory -- a half tree in the ABI slot is worse than
    no tree, because the card would then say "installed" about something that
    does not import. The probe is the one failure that *keeps* what it landed:
    if the import still fails, the answer the user needs is the real
    ImportError (a missing system library, an architecture mismatch), not a
    silently empty directory.
    """
    if progress is None:
        progress = _NullProgress()
    tmp = None
    at = 'download'
    try:
        abi = extras_abi()
        name = extras_asset_name(abi)
        if not name:
            return _extras_refuse(
                progress, at, None,
                '这台机器（%s/%s）没有对应的依赖包。'
                % (sys.platform, _machine_name()))
        version = Setting.get_version()
        progress.enter(at, name)
        tmp = _extras_tmpdir()
        zip_path = os.path.join(tmp, name)
        urls = plugin_repo.extras_urls(name, version)
        fetched = False
        last_error = ''
        for url in urls:

            def _on_bytes(got, total):
                # `sub(step, None, …)` leaves the step at its opening guess --
                # 0.15 of one step out of five, which reads as 3% forever. That
                # was this bar for the whole download of a twenty-six megabyte
                # bundle: the bytes were arriving, the fraction was thrown away.
                note = '已下载 %.1f MB' % (got / 1048576.0)
                frac = None
                if total:
                    note += ' / %.1f MB' % (total / 1048576.0)
                    frac = min(1.0, got / float(total))
                progress.sub(at, frac, note)

            ok, why = _fetch_extras_zip(url, zip_path, _on_bytes)
            if ok:
                fetched = True
                break
            last_error = '%s（%s）' % (why, url)
        if not fetched:
            return _extras_refuse(
                progress, at, tmp,
                '依赖包没下载下来：%s。检查一下网络，或者在设置页开启国内镜像。'
                % last_error)
        progress.leave(at, name)

        at = 'verify'
        progress.enter(at)
        manifest, refusal = _verify_extras_zip(zip_path, abi)
        if manifest is None:
            return _extras_refuse(progress, at, tmp, refusal)
        progress.leave(at, '%d 个文件' % len(manifest['files']))

        at = 'unpack'
        progress.enter(at)
        staging = os.path.join(tmp, 'tree')
        os.makedirs(staging)
        count = _unpack_extras_zip(zip_path, manifest, staging)
        progress.leave(at, '%d 个文件' % count)

        at = 'land'
        progress.enter(at)
        landing = extras_dir(abi)
        parent = os.path.dirname(landing)
        if not os.path.isdir(parent):
            os.makedirs(parent)
        # Whatever is loaded from the old tree has to be forgotten *before* it
        # moves: on Windows a live `.pyd` in there is a file in use, and a
        # locked move would fail the step we are standing in.
        while landing in sys.path:
            sys.path.remove(landing)
        _forget_webrtc_modules(landing)
        displaced = _trash_extras_tree(landing)
        shutil.move(staging, landing)
        with open(os.path.join(landing, EXTRAS_MANIFEST_NAME), 'wb') as handle:
            handle.write(json.dumps(manifest).encode('utf-8'))
        _extras_landing(landing)
        progress.leave(at, landing)

        at = 'probe'
        progress.enter(at)
        if _webrtc_modules() is None:
            why = _WEBRTC_IMPORT_ERROR
            return _extras_refuse(
                progress, at, tmp,
                '依赖包已经装上，但导入仍然失败：%s。这通常是缺系统库或架构不对，'
                '目录在 %s' % (why, landing))
        progress.leave(at)
        message = '已安装 WebRTC 依赖（%d 个文件）' % count
        if displaced:
            message += '，旧的一份已移入废纸篓目录'
        progress.finish(True, message)
        _shutil_rmtree(tmp)
        return True, message
    except Exception as exc:
        logger.exception('the WebRTC extras install broke')
        return _extras_refuse(progress, at, tmp,
                              '安装失败：%s: %s' % (type(exc).__name__, exc))


def uninstall_webrtc_extras(progress=None):
    """(ok, message): take our tree out of the way, recoverably.

    A bundle that came from `pip` is not ours to move -- refusing with the pip
    sentence is the difference between a user undoing the right thing and a
    user leaving a half-removed environment behind.
    """
    if progress is None:
        progress = _NullProgress()
    abi = extras_abi()
    landing = extras_dir(abi)
    at = 'trash'
    progress.enter(at, landing)
    if not os.path.isdir(landing):
        origin = _extras_module_origin()
        why = (('这一路依赖不是「电脑投屏」装的，它在 %s。'
                '请用「pip uninstall aiortc av」卸载。' % origin) if origin else
               '本机没有用这个按钮安装过 WebRTC 依赖，无需移除。')
        progress.fail(at, why)
        progress.finish(False)
        return False, why
    try:
        moved = _trash_extras_tree(landing)
    except OSError as exc:
        why = '移除失败：%s' % exc
        progress.fail(at, why)
        progress.finish(False)
        return False, why
    progress.leave(at, moved)
    at = 'unload'
    progress.enter(at)
    while landing in sys.path:
        sys.path.remove(landing)
    _forget_webrtc_modules(landing)
    progress.leave(at)
    message = '已移除 WebRTC 依赖（原来的文件在 %s）' % moved
    logger.info('the WebRTC extras tree moved to %s', moved)
    progress.finish(True, message)
    return True, message


def extras_state():
    """Everything the mirror card says about the bundle, from one reading.

    `ready` is the probe and `installed` is the manifest on disk; both answer
    *now*, which is why the probe cannot cache a success. The step list is the
    install's own progress -- it rides this same polling rather than a second
    localhost page, because unlike the audio installer this one is a single
    short download the mirror card is already standing next to.
    """
    abi = extras_abi()
    asset = extras_asset_name(abi)
    ready = _webrtc_modules() is not None
    landing = extras_dir(abi)
    installed = bool(asset) and _extras_installed(landing)
    with _EXTRAS_LOCK:
        progress = _EXTRAS_PROGRESS
    snap = progress.snapshot() if progress is not None else {
        'pct': 0.0, 'message': '', 'done': True, 'ok': None, 'steps': []}
    return {'supported': bool(asset), 'ready': bool(ready),
            'installed': bool(installed), 'asset': asset,
            'version': Setting.get_version(), 'packages': list(
                plugin_repo.EXTRAS_PACKAGES),
            'label': EXTRAS_INSTALL_LABEL,
            'uninstall_label': EXTRAS_UNINSTALL_LABEL,
            'hint': WEBRTC_INSTALL_HINT, 'directory': landing,
            'running': bool(progress is not None and not snap['done']),
            'steps': snap['steps'], 'pct': snap['pct'], 'message': snap['message'],
            'done': snap['done'], 'ok': snap['ok']}


def start_extras_uninstall():
    """Move our tree aside and answer with the same machine the install uses.

    Two reasons this is not a bare call to `uninstall_webrtc_extras()`: the
    sentence naming `.trash` has to outlive the toast (§4.8: 一次性通知闪过就没了),
    and a removal that raced an in-flight install's land step would leave the
    ABI slot holding a tree the card just promised to take away. A local move is
    quick enough to run inside the POST, so the ladder and the answer arrive
    together.
    """
    global _EXTRAS_PROGRESS
    with _EXTRAS_LOCK:
        if _EXTRAS_BUSY.is_set():
            return False, '已经在安装或移除 WebRTC 依赖，等它跑完再看结果。'
        _EXTRAS_BUSY.set()
        progress = _EXTRAS_PROGRESS = _SetupProgress(steps=EXTRAS_REMOVE_STEPS)
    try:
        ok, message = uninstall_webrtc_extras(progress=progress)
    except Exception as exc:            # uninstall_ has its own handler
        ok = False
        message = '移除失败：%s: %s' % (type(exc).__name__, exc)
        logger.exception('the WebRTC extras removal broke')
    finally:
        with _EXTRAS_LOCK:
            _EXTRAS_BUSY.clear()
    logger.info('the WebRTC extras removal answered %s: %s', ok, message)
    return ok, message


def start_extras_install():
    """Kick off the install on a daemon thread and answer at once.

    The POST must not hold a CherryPy worker through a forty-megabyte download
    (the same rule that keeps discovery off the UI thread); the page polls
    `mirror-state` for the steps already.
    """
    global _EXTRAS_PROGRESS
    with _EXTRAS_LOCK:
        if _EXTRAS_BUSY.is_set():
            return False, '已经在安装了，等它跑完再看结果。'
        _EXTRAS_BUSY.set()
        progress = _EXTRAS_PROGRESS = _SetupProgress(steps=EXTRAS_STEPS)
    thread = threading.Thread(target=_run_extras_install, args=(progress,),
                              name='MacastExtrasInstall', daemon=True)
    thread.start()
    return True, '开始下载 WebRTC 依赖，进度在这一张卡上。'


def _run_extras_install(progress):
    try:
        ok, message = install_webrtc_extras(progress=progress)
    except Exception as exc:            # install_ has its own handler
        ok = False
        message = '安装失败：%s: %s' % (type(exc).__name__, exc)
        logger.exception('the WebRTC extras thread broke')
    with _EXTRAS_LOCK:
        _EXTRAS_BUSY.clear()
    logger.info('the WebRTC extras install answered %s: %s', ok, message)
    #: The card reports this too, but a forty-megabyte download outruns an open
    #: settings page -- and a silent failure is how the audio installer used to
    #: read as "nothing happened".
    notify(message)
    return ok, message


def _extras_refuse(progress, step, tmp, why):
    """Fail one step, clean the scratch directory, and hand the sentence back."""
    progress.fail(step, why)
    progress.finish(False)
    _shutil_rmtree(tmp)
    logger.info('the WebRTC extras install refused at %s: %s', step, why)
    return False, why


def _shutil_rmtree(path):
    if path and os.path.isdir(path):
        shutil.rmtree(path, ignore_errors=True)


def _make_viewer_class():
    """The `aiortc.MediaStreamTrack` subclass, built once aiortc loads.

    A module-level `class _WebRTCViewer(aiortc.MediaStreamTrack)` would make
    aiortc a module-scope import again, one indirection up; resolving the
    base class here is what keeps the plugin loadable without it. Cached
    after the first success because the class is stateless -- the queue it
    drains lives on the *instance*, one per viewer.
    """
    global _WEBRTC_VIEWER_CLASS
    if _WEBRTC_VIEWER_CLASS is not None:
        return _WEBRTC_VIEWER_CLASS
    aiortc, av = _webrtc_modules()
    media_stream_error = aiortc.mediastreams.MediaStreamError

    class _WebRTCViewer(aiortc.MediaStreamTrack):
        """One viewer, as seen by aiortc: a queue of access units to drain.

        There is no encoder below `recv()`, so the pacing rules are simple
        and slightly unusual: a call must be answered *immediately* when a
        unit is waiting (the frame is already encoded; sleeping here would
        put an artificial 1/FPS clock on top of the encoder's own), and a
        viewer that falls behind is dropped for rather than caught up --
        sending it twenty stale frames to keep it "in sync" would be
        sending twenty frames of the past at live bitrates, which is the
        one thing a live view cannot want.
        """

        kind = 'video'

        def __init__(self, owner):
            super(_WebRTCViewer, self).__init__()
            self._owner = owner
            self._queue = deque()
            self._waiter = None
            self._closed = False
            self._frames = 0
            #: The first unit a new viewer may be sent is an IDR: anything
            #: earlier references pictures it has never seen. Everything
            #: from here until then is skipped *by the sender*, because
            #: nothing on the far side can ask for a keyframe -- see
            #: `gop_size`'s note on this target.
            self._awaiting_idr = True
            self.pc = None

        # -- called on the bridge's own loop thread ---------------------------

        def offer(self, unit):
            """One access unit for this viewer; never blocks."""
            if self._closed:
                return
            waiter = self._waiter
            if waiter is not None and not waiter.done():
                waiter.set_result(unit)
                self._waiter = None
                return
            if len(self._queue) >= WEBRTC_QUEUE_UNITS:
                self._queue.popleft()
                self._owner.note_drop()
            self._queue.append(unit)

        def sentinel(self):
            """No more units will ever come: wake `recv()` so it can end."""
            self._closed = True
            self._queue.clear()
            waiter, self._waiter = self._waiter, None
            if waiter is not None and not waiter.done():
                waiter.set_result(None)

        # -- called by aiortc, on the bridge's loop thread --------------------

        async def recv(self):
            while True:
                unit = await self._take()
                if unit is None:
                    raise media_stream_error('the mirroring session ended')
                if self._awaiting_idr and not _unit_is_key(unit):
                    continue
                self._awaiting_idr = False
                packet = av.Packet(unit)
                # pts counts *this viewer's* frames from zero, and the
                # timebase is the pipeline's own `-r`: aiortc extrapolates
                # RTP timestamps from these two, and the receiver's jitter
                # math is only honest if they describe the encoder we
                # actually run. A viewer joining mid-session therefore gets
                # a timestamp timeline that starts at whatever wall-clock
                # moment its first IDR was produced.
                packet.pts = self._frames
                packet.time_base = Fraction(1, FPS)
                self._frames += 1
                self._owner.note_sent(len(unit))
                return packet

        async def _take(self):
            while True:
                if self._queue:
                    return self._queue.popleft()
                if self._closed:
                    return None
                waiter = self._owner._loop.create_future()
                self._waiter = waiter
                try:
                    return await waiter
                finally:
                    self._waiter = None

    _WEBRTC_VIEWER_CLASS = _WebRTCViewer
    return _WEBRTC_VIEWER_CLASS


class _WebRTCBridge(object):
    """The WebRTC media plane: one ffmpeg byte stream -> N SRTP viewers.

    Sits in the slot `_Broadcaster` fills for every other target, and
    deliberately shares only the surface the rest of the plugin calls:
    `feed`/`flush` from the pump, `clients()`, `close()` from teardown. It
    is *not* a `_Broadcaster`: that class protects an HTTP socket per
    viewer by dropping whole container pieces, and there is no container
    and no socket here -- a viewer is an aiortc track, and the piece it
    either gets or misses is exactly one access unit (see `offer`).

    One dedicated asyncio loop, on its own thread, runs every piece of
    queue state: `feed` only ever crosses onto it with
    `call_soon_threadsafe`, so the asyncio objects (futures, the peers
    dict) have exactly one owner and no lock is needed for a pipeline
    that runs at frame rates. The counters are the exception -- written
    from both threads, read from the UI thread -- and each one is a single
    `+=` on an int: a stale-by-one read of a number printed once a second
    is not worth serialising four frame pipelines for.
    """

    def __init__(self):
        self._units = _AccessUnits()
        self._peers = {}
        self._closed = False
        self.chunks = 0
        self.bytes = 0
        self.drops = 0
        self.sent = 0
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._run_loop,
                                        name='SCREEN_MIRROR_WEBRTC')
        self._thread.daemon = True
        self._thread.start()

    def _run_loop(self):
        asyncio.set_event_loop(self._loop)
        try:
            self._loop.run_forever()
        except Exception:
            logger.exception('the WebRTC loop died')

    # -- the pump's side ------------------------------------------------------

    def feed(self, chunk):
        """One ffmpeg chunk, from the pump thread.

        Access units are split even while no viewer is connected: the
        splitter is what caches SPS/PPS in front of every IDR, and a viewer
        that joins later must inherit that cache warm. A stream whose
        parser only ran while someone watched would hand its first viewer
        an IDR without parameter sets -- a black picture, not a saving.
        """
        if self._closed:
            return
        self.chunks += 1
        self.bytes += len(chunk)
        for unit in self._units.feed(chunk):
            self._dispatch(unit)

    def flush(self):
        """Drain the splitter at end of stream (see `_AccessUnits.flush`)."""
        if self._closed:
            return
        for unit in self._units.flush():
            self._dispatch(unit)

    def _dispatch(self, unit):
        if self._peers:
            self._loop.call_soon_threadsafe(self._offer, unit)

    def _offer(self, unit):
        for viewer in tuple(self._peers.values()):
            viewer.offer(unit)

    def note_drop(self):
        self.drops += 1

    def note_sent(self, count):
        self.sent += count

    # -- the HTTP handler's side ----------------------------------------------

    def clients(self):
        return len(self._peers)

    def create_offer(self, timeout=10.0):
        """A fresh (peer_id, {type, sdp}) pair, or raise with the reason.

        Synchronous wrapper for the signalling POST: the coroutine runs on
        the loop thread and this thread waits for it, because the answer
        has to go back to the HTTP client in the same request.
        """
        if self._closed:
            raise RuntimeError('the mirror has stopped')
        future = asyncio.run_coroutine_threadsafe(self._create_offer(),
                                                  self._loop)
        return future.result(timeout)

    def set_answer(self, peer_id, sdp, type_, timeout=10.0):
        if self._closed:
            raise RuntimeError('the mirror has stopped')
        future = asyncio.run_coroutine_threadsafe(
            self._apply_answer(peer_id, sdp, type_), self._loop)
        return future.result(timeout)

    # -- on the loop thread ---------------------------------------------------

    async def _create_offer(self):
        aiortc, _av = _webrtc_modules()
        if self._closed:
            raise RuntimeError('the mirror has stopped')
        if len(self._peers) >= WEBRTC_MAX_VIEWERS:
            raise RuntimeError('too many WebRTC viewers (the limit is {})'
                               .format(WEBRTC_MAX_VIEWERS))
        viewer = _make_viewer_class()(self)
        pc = aiortc.RTCPeerConnection()
        viewer.pc = pc
        pc.addTrack(viewer)
        # aiortc's own codec list puts VP8 first, and a VP8 negotiation
        # would leave this track serving an encoder nobody has (the spike
        # for this target *did* negotiate VP8 once and the viewer got
        # zero frames). H.264 packetization-mode=1 is what this pipeline
        # produces, so that is what the offer may contain.
        #
        # The value in aiortc's capability table is the *string* `"1"`
        # (codecs/__init__.py, the two H264 profiles), and it must be
        # compared as such: the first spelling of this filter was `== 1`
        # (int), which matched nothing, and `setCodecPreferences([])` means
        # "no preference" to aiortc -- VP8 first. Negotiation then picked
        # VP8 for an H264 stream, `Vp8Encoder.pack` wrapped our Annex-B
        # payloads in VP8 descriptors without complaining, and the viewer
        # got zero frames with no error anywhere (found 2026-10-02 by this
        # suite's own aiortc client, which is exactly why that case exists).
        capabilities = aiortc.RTCRtpSender.getCapabilities('video')
        h264 = [codec for codec in capabilities.codecs
                if codec.mimeType == 'video/H264'
                and str(codec.parameters.get('packetization-mode')) == '1']
        if not h264:
            # A loud refusal on this side of the wire; silently proceeding
            # is the black-picture bug above, and the page will show this
            # message.
            raise RuntimeError('this build of aiortc offers no H264 '
                               'packetization-mode=1 codec')
        for transceiver in pc.getTransceivers():
            transceiver.setCodecPreferences(h264)
        peer_id = secrets.token_hex(8)
        # Registered *before* the SDP exists, so end-to-end wiring has no
        # lost-viewer window: units produced while the offer gathers ICE
        # are already queued for this viewer, and a teardown that happens
        # mid-handshake still finds it in `_peers` and closes it.
        self._peers[peer_id] = viewer
        logger.info('WebRTC viewer %s: offering (%d connected)',
                    peer_id, len(self._peers))

        @pc.on('connectionstatechange')
        def _state_changed():
            if pc.connectionState in ('failed', 'closed'):
                self._loop.create_task(self._retire(peer_id))

        # The page posts its answer within a second when the browser is
        # healthy; a peer still in 'new'/'connecting' after this long is a
        # tab that was closed mid-handshake or a browser that refused the
        # offer, and it holds a slot at `WEBRTC_MAX_VIEWERS` until reaped.
        self._loop.call_later(WEBRTC_PEER_TIMEOUT, self._check_peer, peer_id)
        await pc.setLocalDescription(await pc.createOffer())
        deadline = self._loop.time() + WEBRTC_GATHER_TIMEOUT
        while (pc.iceGatheringState != 'complete'
               and self._loop.time() < deadline):
            await asyncio.sleep(0.05)
        description = pc.localDescription
        if description is None:
            await self._retire(peer_id)
            raise RuntimeError('the offer could not be built')
        return peer_id, {'type': description.type, 'sdp': description.sdp}

    async def _apply_answer(self, peer_id, sdp, type_):
        # The unknown-peer refusal comes first, so it does not depend on the
        # optional import: with aiortc missing, `_webrtc_modules()` returns
        # None and unpacking it here would surface as a 500 "signaling
        # failed" instead of the honest 409.
        viewer = self._peers.get(peer_id)
        if viewer is None or viewer.pc is None:
            raise RuntimeError('no such WebRTC viewer')
        aiortc, _av = _webrtc_modules()
        await viewer.pc.setRemoteDescription(
            aiortc.RTCSessionDescription(sdp=sdp, type=type_))

    async def _retire(self, peer_id):
        viewer = self._peers.pop(peer_id, None)
        if viewer is None:
            return
        logger.info('WebRTC viewer %s: retired', peer_id)
        viewer.sentinel()
        try:
            await viewer.pc.close()
        except Exception:
            pass

    def _check_peer(self, peer_id):
        viewer = self._peers.get(peer_id)
        if viewer is None or viewer.pc is None:
            return
        if viewer.pc.connectionState in ('new', 'connecting'):
            logger.info('WebRTC viewer %s: the answer never came, retiring',
                        peer_id)
            self._loop.create_task(self._retire(peer_id))

    # -- teardown -------------------------------------------------------------

    def close(self):
        """Idempotent; returns without waiting for the loop to stop.

        The pump thread is already gone when this is called (teardown
        happens after the encoder died), and `feed`'s own `_closed` guard
        keeps any late chunk out. A second close only schedules a callback
        onto a loop that has stopped, which is a no-op -- closing the loop
        itself is deliberately never done, because aiortc objects touched
        after a closed loop are a worse failure than a stopped one.
        """
        if self._closed:
            return
        self._closed = True
        try:
            self._loop.call_soon_threadsafe(self._begin_shutdown)
        except RuntimeError:
            pass                       # the loop is already closed

    def _begin_shutdown(self):
        self._loop.create_task(self._finish_shutdown())

    async def _finish_shutdown(self):
        viewers = list(self._peers.values())
        self._peers.clear()
        for viewer in viewers:
            viewer.sentinel()
            try:
                await viewer.pc.close()
            except Exception:
                pass
        self._loop.stop()


# -- the renderer ------------------------------------------------------------

class _Aborted(Exception):
    """The generation moved on while _mirror was setting up; stand down."""


class _RetryVideoOnly(Exception):
    """The capture with system audio never produced a frame: unwind this
    attempt and start a video-only one, without calling it a failure."""


class _RetryCapture(Exception):
    """The capture source refused before producing a frame: latch it and start
    again on the fallback, without calling it a failure.

    Two raisers, one shape. Windows: ddagrab exits without a frame, the latch
    is `_ddagrab_refused`, the fallback is gdigrab. macOS: ScreenCaptureKit
    dies or stalls in the feeder, the latch is `_sck_refused`, the fallback is
    avfoundation. Distinct from `_RetryVideoOnly` because the *capture source*
    changes, not the audio: the retry keeps whatever audio the probe found.
    """


class ScreenMirrorRenderer(Renderer):

    #: Told to `MacastPluginManager` that this plugin owns the desktop console:
    #: the menu-bar door and the management API ask it for its surface without
    #: the user having picked it as the current renderer first.
    MIRROR_CONSOLE = True

    #: No row in the menu's Renderers group. The control surface is the settings
    #: page's 「电脑投屏」 tab (v0.12, AGENTS.md §4.8: 菜单栏里一条镜像行都没有，只剩
    #: 通知), and a row here reads as the way to reach it while only offering a
    #: renderer switch. A `Macast_Renderer` that still names this plugin keeps
    #: working -- the menu just does not put a checkmark on a renderer that is
    #: not playing.
    MENU_HIDDEN = True

    def __init__(self):
        super(ScreenMirrorRenderer, self).__init__()
        self._lock = threading.Lock()
        self._sender = None
        self._proc = None
        self._server = None
        #: The low-latency target has no HTTP server: the encoder's bytes go
        #: straight into a sender that pushes them. `_kind` says which target
        #: this is even when there is no session to ask.
        self._sink = None
        self._kind = ''
        self._awake = None
        #: The session's own ceiling, armed when the session is published and
        #: cancelled by `_teardown`. None for the shapes that have none. It is a
        #: timer rather than only ffmpeg's `-t` because the notice is ours to
        #: write: a mirror that ends because the encoder ran out of its budget
        #: leaves the television on a frozen frame and the user with no sentence
        #: explaining why.
        self._duration_timer = None
        self._generation = 0
        self._mirroring = False
        self._started_at = 0.0
        self._url = ''
        #: What the DLNA renderer last said (TransportState), for the menu.
        self._dlna_state = ''
        #: Whether the last DLNA poll found the renderer *disagreeing* with
        #: bytes that were visibly being consumed. Kept apart from
        #: `_dlna_state` on purpose: that field is the renderer's own word and
        #: the diagnostics card quotes it, while this one is our own finding --
        #: it is what makes the status line say「（客户端在读取）」instead of
        #: repeating STOPPED over a picture that is on screen.
        self._dlna_reading = False
        #: A session is being set up: the console's button must not start a
        #: second one while the first is still probing, and「已经在镜像了」would
        #: be a lie for the ~3 s the setup takes.
        self._starting = False
        #: This session had to drop the system-audio tap to get a frame at all;
        #: see AUDIO_DROPPED_SUFFIX.
        self._audio_dropped = False
        #: Whether a capture device has already refused to deliver a frame in
        #: this run. The answer does not change between sessions, and asking
        #: again costs the viewer seconds of black before the picture starts.
        self._audio_refused = False
        #: The capture source the running attempt was built on ('ddagrab' /
        #: 'gdi' on Windows, 'sck' / 'avfoundation' on macOS, '' elsewhere).
        #: Read by `_encoder_died`: a ddagrab session that died before its
        #: first byte is a refusal the starter thread is about to answer
        #: (gdigrab fallback, or a considered failure), and the pump must not
        #: race it with a「启动失败」of its own. `sck` shares this meaning, and
        #: the pump additionally asks the feeder whether it failed.
        self._capture_method = ''
        #: The in-process ScreenCaptureKit feeder for the running session
        #: (`_SckFeeder`), handed over exactly like `_proc`: the starter thread
        #: builds it, the pump owns its lifetime, `_teardown` stops it. None
        #: for every other capture method.
        self._feeder = None
        #: Which compatibility shape *this run* is on, when the watchdog moved
        #: it, and how many moves it has spent. Both belong to the run rather
        #: than to the setting: `Mirror_Dlna_Profile` is the user's choice, and
        #: an automatic ladder that wrote it would quietly replace what someone
        #: picked for a television it has never met. `start_mirror` clears them
        #: because a new click is a new question. See `_rotate_dlna_profile`.
        self._profile_id = None
        self._profile_tries = 0
        #: The running session's own raw facts for the「统计信息」card: what
        #: ffmpeg was actually asked to do, which tap and encoder the probe
        #: chose, and the budgets the end-to-end delay is assembled from.
        #: Written once per attempt, read by `stats()`; empty while nothing is
        #: running, so a stopped mirror cannot show a stale command.
        self._diag = {}
        self.renderer_setting = ScreenMirrorSetting()
        # The surface asks *this* renderer about the session, not the bus's
        # "whoever is playing": see `ScreenMirrorSetting._owner`.
        self.renderer_setting._owner = self

    # -- settings helpers ----------------------------------------------------

    def target(self):
        raw = Setting.get(SettingProperty.Mirror_Target, '') or ''
        name = Setting.get(SettingProperty.Mirror_Target_Name, '') or ''
        if not raw:
            return None, None, name
        host, _, port = raw.partition(':')
        try:
            port = int(port or CAST_PORT)
        except ValueError:
            port = CAST_PORT
        return host, port, name or host

    def quality(self):
        return quality_preset()

    def is_mirroring(self):
        return self._mirroring

    def playing_url(self):
        return self._url

    def viewer_url(self):
        """The viewing address for the running session, or ''.

        Two targets answer: `browser` (the MSE page) and `webrtc` (the peer
        connection page). Both are "open this on the device you want to watch
        from"; the other three targets are devices, not pages.
        """
        with self._lock:
            server = self._server
        if server is None or server.session.kind not in ('browser', 'webrtc'):
            return ''
        if server.session.kind == 'webrtc':
            return webrtc_page_url(server)
        return page_url(server)

    def stats(self):
        """What the mirror is doing right now, for the menu and status page.

        The pump is the only thing that counts, so this costs nothing to ask --
        but it also means the numbers are only as fresh as the last chunk.
        """
        with self._lock:
            server, sink, kind = self._server, self._sink, self._kind
            diag = dict(self._diag)
        #: Every target counts bytes the same way; only the low-latency one has
        #: no HTTP server holding a broadcaster to count them with.
        source = server.broadcaster if server is not None else sink
        if source is None or not kind:
            return {}
        seconds = max(1.0, time.time() - self._started_at) \
            if self._started_at else 1.0
        info = {'kind': kind,
                'clients': source.clients(),
                'chunks': source.chunks,
                'bytes': source.bytes,
                'drops': source.drops,
                #: The two facts `drops` cannot express: replays that found no
                #: keyframe, and units sitting unsent in front of the viewers
                #: right now. Same counter set as `/browser/stats` reads -- one
                #: source, two readers (AGENTS.md 4.8 red line ③).
                'misses': getattr(source, 'keyframe_misses', 0),
                'queued': (source.queued() if hasattr(source, 'queued')
                           else None),
                'codec': avc_codec_of(source),
                'mbps': round(source.bytes * 8 / 1000000.0 / seconds, 2),
                'seconds': int(time.time() - self._started_at)
                if self._started_at else 0}
        #: The session's own configuration, kept apart from the live numbers
        #: above: the「统计信息」card shows both, and only this half explains
        #: *why* the delay is what it is.
        if diag:
            info['diag'] = diag
        #: Delivered, as opposed to the `bytes` above: what left on a socket vs
        #: what the encoder made. They part company the moment a viewer quits,
        #: and a statistics card that only shows the second one describes a
        #: mirror nobody is watching as if someone were.
        #:
        #: Every target answers it, each from its own socket. This used to say
        #: that only the HTTP ones could, because "the low-latency channel pushes
        #: UDP datagrams with no receive window and no retransmission" -- and
        #: since retransmission landed that sentence is simply false.
        #:
        #: **The two counters have no fixed ordering, and a comment here once
        #: claimed they did.** On this channel `bytes` is incremented in `feed`,
        #: before the window is consulted, so it counts pictures the window then
        #: shed and never put on the wire; `socket_bytes` counts datagrams, each
        #: 19 bytes larger than its payload, plus every repair and every idle
        #: probe. So a stream that is dropping frames has `bytes` *ahead*, and a
        #: stream that is not has `socket_bytes` ahead. Reading the gap either
        #: way as "the repair traffic" is wrong twice over -- the number that
        #: says the wireless is losing datagrams is `retransmits`, below, which
        #: counts repairs directly and is not a difference of two moving totals.
        if kind == 'webrtc':
            #: The bridge's `sent` is this target's delivered count -- bytes
            #: the viewer track handed to aiortc. There is no HTTP body on
            #: this path, so the session's `written` stays 0 and pointing at
            #: it would report a working channel as one that delivered nothing.
            info['delivered'] = getattr(source, 'sent', 0)
        elif server is not None:
            info['delivered'] = server.session.written
        if kind == 'caststream':
            info['delivered'] = sink.socket_bytes
            info['frames'] = sink.frames
            info['in_flight'] = sink.in_flight()
            info['delay'] = sink.playout_delay
            #: Repair traffic, and the two reasons we decline to repair: the
            #: packet was too recent to have been lost (openscreen's staleness
            #: rule) or the frame is gone (shed by the window, or evicted from
            #: the repair buffer). A nonzero `retransmits` with `drops` also
            #: climbing is a congested link; a nonzero one with `drops` at zero
            #: is a lossy one, and only the second is what this channel's
            #: repair path exists for.
            info['retransmits'] = sink.retransmits
            info['retransmit_stale'] = sink.retransmit_stale
            info['retransmit_gone'] = sink.retransmit_gone
            info['rtt'] = sink.rtt_ms()
        elif kind == 'dlna':
            # Both shapes serve this target, and both need the same three facts.
            # Keying this on the byte log made the *default* shape (the live
            # stream, which keeps no hoard) report nothing at all -- the line
            # read「档位 ? · 电视 未上报」over a picture that was on screen.
            info['profile'] = dlna_profile_id(server.session.profile)
            info['state'] = self._dlna_state
            info['reading'] = self._dlna_reading
            if server.session.bytelog:
                # How far behind the TV is, which only means something when it
                # is reading from a hoard we pre-filled.
                info['buffered'] = max(0, source.end - source.start)
        return info

    # -- Renderer API ----------------------------------------------------------

    def start(self):
        super(ScreenMirrorRenderer, self).start()
        # Warm *both* searches. The page's first panel is「哪种投屏方式有设备」,
        # so an answer for only the stored target reads as the other protocol
        # being empty; and a menu opened before any search finished used to have
        # to say「搜索中…再展开一次」while it waited.
        start_search()
        start_renderer_search()

    def set_media_url(self, url, start="0"):
        """A phone pushed something: mirror stops and the pushed url plays.

        Macast can only have one renderer, and while this one is selected the
        phone deserves the same bridge behaviour cast_bridge gives.
        """
        if not url:
            return
        if url == self._url:
            # The same address pushed twice is a client retry, not a request to
            # tear the stream down and start it again.
            logger.info('ignoring a repeat push of %s', url)
            return
        kind = output_kind()
        if kind in ('cast', 'caststream'):
            ready = bool(self.target()[0])
        elif kind == 'dlna':
            ready = bool(dlna_target()[1])
        else:
            # A browser target has nothing to bridge to: its "device" is a
            # webpage we drive by streaming, not by pushing a URL.
            ready = False
        if not ready:
            # Nothing to bridge to: this user has no device selected, and mpv
            # is not ours to drive here. Say so instead of silently swallowing
            # the push (or killing a running mirror for it) -- and say *which*
            # of the two it is, because a browser target has no device to pick
            # and telling someone to go pick one is a dead end.
            why = ('浏览器目标没有可中继的设备，它只播这台电脑推出去的流；'
                   if kind in ('browser', 'webrtc')
                   else '还没有选择投屏目标；')
            notify('Screen Mirror 无法播放推送的网址：{}在「电脑投屏」页里把「投屏方式」'
                   '切到 Chromecast 或 DLNA 电视做中继，或换回默认渲染器播放'
                   .format(why))
            return
        self._url = url
        with self._lock:
            self._generation += 1
            generation = self._generation
        self._teardown_async()
        runner = self._cast_url if kind != 'dlna' else self._dlna_url
        threading.Thread(target=runner, args=(url, generation),
                         daemon=True, name="SCREEN_MIRROR_CAST").start()
        self.set_state_transport('PLAYING')
        cherrypy.engine.publish('renderer_av_uri', url)

    def set_media_stop(self):
        with self._lock:
            self._generation += 1
        self._teardown_async()
        self.set_state_transport('STOPPED')
        cherrypy.engine.publish('renderer_av_stop')

    def set_media_pause(self):
        logger.info('pause ignored: a live screen mirror cannot be paused')

    def set_media_resume(self):
        logger.info('resume ignored: a live screen mirror never pauses')

    def set_media_volume(self, data):
        with self._lock:
            sender = self._sender
        if sender is None:
            return
        if not hasattr(sender, 'set_volume'):
            # A DLNA renderer's volume lives in RenderingControl, a different
            # service we never resolved -- and every TV has a remote for it.
            logger.info('volume left to the renderer: this target speaks no '
                        'Cast volume channel')
            return
        try:
            sender.set_volume(int(data))
        except (OSError, ssl.SSLError, ValueError) as e:
            logger.error('cannot set the device volume: %s', e)

    def stop(self):
        with self._lock:
            self._generation += 1
        self._teardown()
        super(ScreenMirrorRenderer, self).stop()

    # -- mirror control (console window) ----------------------------------------

    def start_mirror(self, keep_ladder=False):
        """Begin a mirror; False when one is running or already being set up.

        The setup takes seconds (probe, encoder, prefill), and during it
        `_mirroring` is still False -- so five clicks on「开始镜像」used to bump
        the generation five times and leave the fifth attempt racing the first.

        `keep_ladder` is how `_rotate_dlna_profile` asks for the *next shape*
        without losing its place: every other start is a fresh question from the
        user, and the answer to the previous one is not theirs to inherit.
        """
        with self._lock:
            if self._mirroring or self._starting:
                return False
            if not keep_ladder:
                self._profile_id = None
                self._profile_tries = 0
            self._starting = True
            self._audio_dropped = False
            self._generation += 1
            generation = self._generation
        threading.Thread(target=self._mirror, args=(generation,),
                         daemon=True, name="SCREEN_MIRROR").start()
        return True

    def active_dlna_profile(self):
        """The compatibility shape a session started right now would use.

        The run's ladder position wins over the setting, and both resolve
        through `dlna_profile` so an unknown stored value still lands on the
        default.
        """
        return dlna_profile(self._profile_id)

    def dlna_profile_display(self):
        """The rung the page should highlight, which is not always the setting.

        While a session is up or coming up it is the one that is actually being
        encoded -- a card that still lit the stored `ps-pal` after the watchdog
        moved to TS was pointing at a knob nothing was obeying. While nothing is
        running it is the setting, because that is what the next click starts
        from: the ladder belongs to a run, and a run has ended.
        """
        if self._mirroring or self._starting:
            return dlna_profile_id(self.active_dlna_profile())
        return dlna_profile_id(dlna_profile())

    def is_starting(self):
        """A session is in flight but has not produced a frame yet."""
        return self._starting

    def audio_dropped(self):
        """This session gave up the system-audio tap to get a frame at all.

        The「系统声音」row answers what the machine is *configured* for; only the
        running capture knows what it actually carries, and a page that says
        「已启用」over a silent mirror is the lie v0.12 was meant to end.
        """
        return self._audio_dropped

    def capture_method(self):
        """Which grab is (or was last) feeding this session: 'sck',
        'avfoundation', 'ddagrab', 'gdi', or '' before any ran. Called by
        `_status_line` to pick the right「没有声音」annotation -- the SCK tap,
        the microphone-granted one and the Windows loopback die for three
        different reasons and each sentence offers a different door.
        """
        return self._capture_method

    def stop_mirror(self):
        with self._lock:
            self._generation += 1
        self._teardown_async()

    # -- internals ---------------------------------------------------------------

    def _mirror(self, generation):
        """Set one session up, and stop saying「正在启动」whichever way it ends."""
        # Asked once per run, not once per session: a device that delivered
        # nothing a minute ago will deliver nothing now, and the only thing a
        # second enquiry buys is a longer wait before the picture appears.
        with_audio = not self._audio_refused
        if not with_audio:
            logger.info(
                'starting without system audio: a capture device refused to '
                'deliver a frame earlier in this run, so the %g s enquiry that'
                ' would otherwise precede every session is skipped',
                no_frame_budget())
        try:
            while self._run_mirror(generation, with_audio):
                # The retry is a new session, not a second half of this one:
                # `_run_mirror` has already retired the generation that failed,
                # so pick the new number up before it is taken. What the retry
                # gives up lives on the renderer's latches -- `_audio_refused`
                # here, `_ddagrab_refused` inside the probe -- which is why the
                # audio answer is read back instead of nailed to False: a
                # ddagrab refusal must not silently cost the sound on its way
                # to the gdigrab fallback.
                with self._lock:
                    generation = self._generation
                    self._starting = True
                with_audio = not self._audio_refused
        finally:
            with self._lock:
                # A newer generation owns the flag now; it clears its own.
                if generation == self._generation:
                    self._starting = False

    def _run_mirror(self, generation, with_audio=True):
        """One capture attempt. True means "retry this without system audio"."""
        kind = output_kind()
        host = port = None
        name = ''
        control = ''
        if target_prompt(kind):
            # Written by the same function the console shows, so the two can
            # never disagree about what is missing --「…或改用「浏览器」目标」used
            # to appear on a page that was already on the browser target.
            # The verdict goes on this copy: one line, no card above it.
            self._fail(target_prompt(kind, with_verdict=True), generation)
            return False
        if kind in ('cast', 'caststream'):
            host, port, name = self.target()
        elif kind == 'dlna':
            name, control = dlna_target()
        ffmpeg = ffmpeg_requirement()
        if ffmpeg is None:
            self._fail('找不到 ffmpeg：{}后重试'.format(FFMPEG_WAY.get(
                sys.platform, '用包管理器安装 ffmpeg')), generation)
            return False
        if kind == 'webrtc' and _webrtc_modules() is None:
            # This target's one optional dependency, refused before a capture
            # probe or an encoder exists to unwind. The sentence is the pip
            # line and where to run it -- the failure is a missing install on
            # one machine, not a broken mirror.
            self._fail(WEBRTC_INSTALL_HINT, generation)
            return False
        capture = probe_capture(ffmpeg)
        if capture is None:
            self._fail(capture_unavailable_hint(), generation)
            return False
        if not with_audio:
            capture = video_only_capture(capture) or capture
        height, bitrate = self.quality()
        encoder = encoder_kind()
        if encoder == 'hardware' and not has_hardware_encoder(ffmpeg):
            # Name the encoder this platform's switch *means*. The sentence used
            # to say "h264_videotoolbox" unconditionally, which on a Windows
            # machine fell back from NVENC and then described an encoder the
            # user had never been offered -- and VideoToolbox is exactly the
            # candidate whose refusal is worth reading precisely, because
            # §4.8's `-level 42` class of bug hides in "which encoder said no".
            logger.warning('%s is unavailable here; using libx264',
                           hardware_encoder() or 'the hardware encoder')
            encoder = 'software'
        first_bytes = threading.Event()
        tail = deque(maxlen=20)
        stream = None
        refused = ''
        if kind == 'caststream':
            # Ask the device first. Its mirroring app is the part of this
            # target we cannot self-prove, and a device that refuses it is
            # common enough that the answer is worth having *before* an encoder
            # and an HTTP server exist to unwind.
            width, pinned, _filter = cast_stream_shape(height)
            try:
                stream = _CastStreamSender.open(
                    host, port, width, pinned, FPS,
                    min(bitrate, cast_stream_bitrate_cap()),
                    on_lost=self._stream_lost)
            except Exception as e:
                logger.warning('Cast Streaming refused (%s); using LOAD', e)
                refused = str(e) or e.__class__.__name__
                kind = 'cast'
        # One profile object for the whole attempt. The session and the argv
        # used to each ask `dlna_profile()`, which was fine while nothing could
        # answer differently between the two calls -- with a watchdog that moves
        # the ladder, a session on rung 3 and an encoder still on rung 2 is a
        # black screen with two confident logs about it.
        profile = self.active_dlna_profile() if kind == 'dlna' else None
        #: Asked once, on the possibly-lowered `kind` above: a caststream the
        #: device refused is now a LOAD session, and it obeys the ceiling the
        #: same as every other television shape. Both the argv and the session
        #: read this one value -- see `bound_video_seconds`.
        max_seconds = duration_bound(kind)
        session = _Session(kind, has_audio=bool(capture.audio_map),
                           title=socket.gethostname() or 'Macast',
                           profile=profile, bitrate=bitrate,
                           max_seconds=max_seconds)
        handed = False
        # Locals the unwind paths below hand to `_cleanup` when an attempt
        # never reaches its `handed` moment: nothing else has ever seen them,
        # so `_teardown` -- which reads the published fields -- cannot reap
        # them. All of them are None on every non-SCK capture. `server` joins
        # them for the same reason the encoder does: the unwind paths read it
        # before `start_stream_server` has had a chance to run (a bridge that
        # refuses to build, a command that will not assemble), and an
        # `UnboundLocalError` here would replace the real reason.
        proc = None
        feeder = None
        feed_audio = False
        server = None
        audio_r = audio_w = None
        try:
            # The ScreenCaptureKit pair: ffmpeg reads NV12 frames from stdin
            # and f32le PCM from a second pipe whose read fd is argv material
            # (`_AUDIO_FD_TOKEN`), so the pipe exists before the command is
            # built. The low-latency targets unmap audio entirely (`-an`), so
            # feeding it would be wasted work -- but its write end must still
            # be closed: an audio input that is open and silent freezes the
            # whole command at open time (measured 2026-10-02).
            try:
                if capture.method == 'sck' and capture.audio_map:
                    audio_r, audio_w = os.pipe()
                    feed_audio = kind not in ('caststream', 'webrtc')
                # Built once and reused for the spawn and for the log line: the
                # same argv is what the「统计信息」card shows, and building it
                # twice was two chances for the logged command to differ from
                # the run one.
                command = build_ffmpeg_command(
                    ffmpeg, capture, height, bitrate, kind=kind,
                    encoder=encoder, profile=profile, audio_fd=audio_r,
                    max_seconds=max_seconds)
                diagnostics = _session_diagnostics(
                    kind=kind, capture=capture, command=command,
                    encoder=encoder, height=height, bitrate=bitrate,
                    session=session, audio_expected=with_audio,
                    refused=refused)
                #: Attached before the server exists to answer: the viewing
                #: page's overlay reads its facts off the session, and a viewer
                #: that opens the URL inside the first second would otherwise
                #: get an empty panel and conclude the overlay is broken.
                session.diag = diagnostics
                # The WebRTC bridge takes the slot a byte broadcaster fills
                # for every other HTTP target: built first, handed to the
                # server as its `broadcaster`, and reaped by `_cleanup`
                # through the same `close` hook. It has to exist before the
                # server answers, or the viewing page's first POST has
                # nothing to signal.
                bridge = _WebRTCBridge() if kind == 'webrtc' else None
                try:
                    server = None if stream is not None \
                        else start_stream_server(session, broadcaster=bridge)
                except Exception:
                    # The bridge never reached the server, so nothing else
                    # can reap it: its loop thread must not outlive the
                    # attempt.
                    if bridge is not None:
                        bridge.close()
                    raise
                proc = subprocess.Popen(
                    command,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    stdin=subprocess.PIPE if capture.method == 'sck'
                    else subprocess.DEVNULL,
                    pass_fds=(audio_r,) if audio_r is not None else (),
                    env=_clean_env())
            finally:
                # The read end lives in the child now. The write end goes to
                # the feeder -- or, when nothing will feed it, closes right
                # here, which is the audio-absent shape ffmpeg is known to
                # accept (measured 2026-10-02). A failed spawn leaves neither
                # end open.
                if audio_r is not None:
                    os.close(audio_r)
                    audio_r = None
                if audio_w is not None and (proc is None or not feed_audio):
                    os.close(audio_w)
                    audio_w = None
            logger.info('screen capture command: %s', ' '.join(command))
            if capture.method == 'sck':
                # The feeder is the only source of frames for this attempt: it
                # owns the video stdin from here on, plus the audio pipe's
                # write end when one was left open. Handed over to the pump
                # below exactly like `proc`, and stopped by `_teardown`.
                feeder = _SckFeeder(audio_w=audio_w,
                                    on_audio_absent=self._sck_audio_absent)
                audio_w = None
                feeder.attach(proc.stdin)
            with self._lock:
                if generation != self._generation:
                    raise _Aborted()
                # Owned from here on: the pump thread reports the encoder's
                # death through _teardown(), so _mirror must not also clean up.
                self._proc = proc
                self._server = server
                self._sink = stream
                self._kind = kind
                self._diag = diagnostics
                # Which grab this attempt runs on. `_encoder_died` reads it
                # to know whose death it is looking at; see the field comment.
                self._capture_method = capture.method
                self._feeder = feeder
                handed = True
            threading.Thread(
                target=_pump,
                args=(proc, server.broadcaster if stream is None else stream,
                      self, generation, first_bytes),
                daemon=True, name="SCREEN_MIRROR_PUMP").start()
            # The handle is kept: `_ddagrab_refusal` joins this reader before
            # reading `tail`, so the verdict about a dead process is not a
            # verdict about how fast the reader was.
            drain = threading.Thread(target=_drain_stderr, args=(proc, tail),
                                     daemon=True, name="SCREEN_MIRROR_LOG")
            drain.start()
            url = ''
            if stream is None:
                if kind == 'dlna':
                    url = dlna_stream_url(server, _url_host(control))
                elif kind == 'webrtc':
                    # Not media bytes: the address of the page that will
                    # negotiate the session and receive SRTP.
                    url = webrtc_page_url(server)
                else:
                    url = stream_url(server)
            if stream is not None:
                stream.start()
            if feeder is not None:
                # Started only after the pump and the drain thread are up: the
                # first frames land in the pipe buffer instead of on the floor,
                # and the no-frame watchdog below measures the capture rather
                # than our own bookkeeping.
                feeder.start(capture.spec, feed_audio=feed_audio)
            # Wait for the encoder to actually produce something: a Screen
            # Recording denial exits in under a second, and the pump is
            # reporting that while we wait. A process that dies calls the wait
            # off early -- the refusal that matters on Windows (ddagrab) is an
            # exit, not a timeout, and waiting out the full budget beside a
            # corpse would add that whole budget to every fallback.
            budget = no_frame_budget()
            started_wait = time.time()
            deadline = started_wait + budget
            while not first_bytes.is_set():
                if proc.poll() is not None or generation != self._generation:
                    break
                if feeder is not None and feeder.failed():
                    # The feeder records its reason; ffmpeg may still be
                    # alive, blocked on a stdin nobody will write again, so
                    # waiting out the budget beside it buys nothing.
                    break
                if feeder is not None and feeder.audio_pending():
                    # The SCK audio tap has neither delivered nor been given
                    # up on yet: ffmpeg is (or just was) frozen at open on
                    # that silent pipe (measured 2026-10-02), so no output
                    # can exist through no fault of the capture. This window
                    # spent most of the budget and killed the session as an
                    # "SCK gave no frame" -- over our own audio grace, which
                    # is the only thing allowed to end it. Push the deadline
                    # while it lasts; the feeder bounds it (first delivery,
                    # the grace that closes the pipe, or a failure that
                    # breaks the loop above).
                    deadline = time.time() + budget
                remaining = deadline - time.time()
                if remaining <= 0:
                    break
                first_bytes.wait(timeout=min(0.25, remaining))
            if not first_bytes.is_set():
                detail = '；'.join(list(tail)[-3:])
                if generation != self._generation:
                    # The wait was cancelled by a stop or a newer start:
                    # whoever moved the generation owns the cleanup, and a
                    # retry from here would race it. This used to raise a
                    # spurious `_RetryVideoOnly` in exactly this window.
                    if proc.poll() is None:
                        proc.terminate()
                    raise _Aborted()
                if capture.method == 'ddagrab' \
                        and _ddagrab_refusal(proc, tail, drain):
                    # The DWM refused to duplicate this display: attach tests
                    # pass (a fresh session opens the duplication fine), the
                    # long-running one dies on it. Latch the build, re-probe
                    # (which now answers gdigrab) and take the attempt again --
                    # with the same audio: this failure has nothing to do with
                    # it. The latch is set here, before `_RetryCapture` unwinds
                    # to the starter: the re-probe the retry performs is what
                    # must not ask ddagrab again, and it reads the latch --
                    # without it the probe re-asks, ddagrab refuses again, and
                    # the loop never ends.
                    logger.warning(
                        'ddagrab refused the display at runtime (%s);'
                        ' retrying with gdigrab',
                        detail or 'no stderr from ffmpeg')
                    with self._lock:
                        # Retire the generation before the process dies, or
                        # the pump reports our own kill as an interruption.
                        self._generation += 1
                        _ddagrab_refused.add(ffmpeg)
                        invalidate_capture_cache()
                    raise _RetryCapture()
                if capture.method == 'sck' and feeder is not None:
                    # Three shapes, one verdict: the feeder recorded a reason
                    # (bindings died, the stream stopped, the frame copy
                    # failed), ffmpeg exited on a stdin that produced almost
                    # nothing, or the stream started and delivered no frame at
                    # all. Every one of them is the capture source refusing
                    # -- none is the audio -- so latch the build, re-probe
                    # (which now answers avfoundation) and take the attempt
                    # again with the same audio the probe found.
                    why = feeder.failed() or (
                        'ffmpeg exited (code %s)' % proc.poll()
                        if proc.poll() is not None
                        else 'no frames within %g s' % budget)
                    logger.warning('ScreenCaptureKit gave no frame (%s);'
                                   ' retrying with avfoundation', why)
                    with self._lock:
                        # Retire the generation before the process dies, or
                        # the pump reports our own kill as an interruption.
                        self._generation += 1
                        _sck_refused.add(ffmpeg)
                        invalidate_capture_cache()
                    raise _RetryCapture()
                if with_audio and capture.audio_map \
                        and kind not in ('caststream', 'webrtc'):
                    # An audio tap this process may not read -- no microphone
                    # grant, or a BlackHole nothing is clocking -- does not
                    # complain: the session opens and delivers nothing, video
                    # included. Give up the sound rather than the mirror.
                    # The seconds reported are the ones actually waited: since
                    # the wait ends early on a dead process, quoting the whole
                    # budget here would be a deadline restated as an event.
                    logger.warning(
                        'capture with system audio returned no frame in %.1f s'
                        ' (%s); retrying video only',
                        time.time() - started_wait,
                        detail or 'no stderr from ffmpeg')
                    # Retire the generation before the process dies, or the
                    # pump reports our own kill as an interruption.
                    with self._lock:
                        self._generation += 1
                        self._audio_dropped = True
                        # Remember it for the rest of the run: the next session
                        # starts without audio instead of paying this wait again.
                        self._audio_refused = True
                    raise _RetryVideoOnly()
                if proc.poll() is None:
                    proc.terminate()
                raise RuntimeError(no_frame_words(detail))
            if proc.poll() is not None or generation != self._generation:
                raise _Aborted()
            if kind == 'dlna' and server.session.bytelog:
                # Only the pretend-file shape pays for a prefill. It has to:
                # the TV is about to read this by absolute offset, so what it
                # sniffs must already be there. The live shape is a plain
                # endless stream -- the reader starts at the live edge and no
                # amount of waiting buys it anything, which is why holding the
                # URL back here used to cost 4-8 seconds of startup on a
                # session that otherwise shows its first picture in two.
                self._prefill(server, proc, generation)
                if generation != self._generation:
                    raise _Aborted()
            sender = None
            if kind == 'cast':
                sender = _CastSender(host, port)
                sender.connect()
                sender.launch()
                sender.load(url, content_type=session.content_type, live=True)
            elif kind == 'dlna':
                sender = _DlnaSender(control)
                sender.set_uri(url, '屏幕镜像 · {}'.format(session.page_title),
                               session.profile,
                               max_seconds=session.max_seconds)
                sender.play()
        except _Aborted:
            if not handed:
                # The unpublished attempt's locals: an encoder nobody has
                # seen, an HTTP server that was already accepting viewers,
                # the SCK feeder, and a Cast Streaming sender whose mirroring
                # app is already up on the device (leave without saying
                # goodbye and the next session meets the stale instance it
                # refuses). `_teardown` reaps the published fields only --
                # nothing was published here -- so this is the one place
                # these can be released.
                _cleanup(None, proc, server, stream, feeder)
            self._teardown()
            return
        except (_RetryVideoOnly, _RetryCapture):
            # Not a failure, so nothing goes on the status line: the next
            # attempt -- video only, gdigrab after a ddagrab refusal, or
            # avfoundation after an SCK one -- is already under way and will
            # report its own outcome.
            self._teardown()
            return True
        except Exception as e:
            if not handed:
                _cleanup(None, proc, server, stream, feeder)
            self._teardown()
            detail = str(e).strip() or ' '.join(list(tail)[-3:])
            target_label = name if kind in ('cast', 'caststream', 'dlna') \
                else '浏览器'
            self._fail('镜像到 {} 启动失败：{}'.format(target_label, detail),
                       generation)
            return False
        # Spawned outside the lock: a menu click that aborts this session must
        # not queue behind an OS call.
        awake = _keep_awake()
        aborted = False
        with self._lock:
            if generation != self._generation:
                aborted = True
            else:
                self._sender = sender
                self._awake = awake
                self._mirroring = True
                self._started_at = time.time()
                self._url = url
        if aborted:
            self._teardown()
            _stop_awake(awake)
            return
        self.set_state_transport('PLAYING')
        if max_seconds:
            self._arm_duration_timer(generation, max_seconds)
        if kind == 'dlna':
            # The renderer needs a push of its own to recover from the pauses
            # and seek-stalls old firmware does on a stream it thinks is a
            # file. It is ours to hold, so it dies with the generation.
            threading.Thread(target=self._watch_dlna,
                             args=(sender, url, session, generation),
                             daemon=True, name="SCREEN_MIRROR_DLNA_WATCH").start()
            message = '已开始镜像到 {}（档位 {}，{}）'.format(
                name, session.profile.label, dlna_shape_words(session))
        elif kind == 'browser':
            message = '镜像已开始，浏览器打开：{}'.format(page_url(server))
        elif kind == 'webrtc':
            message = '已开始低延迟镜像（此通道没有声音），浏览器打开：{}'.format(
                webrtc_page_url(server))
        elif kind == 'caststream':
            message = '已开始低延迟镜像到 {}（本通道没有声音）'.format(name)
        else:
            message = '已开始镜像到 {}'.format(name)
            if refused:
                # The user asked for the low-latency channel and did not get
                # it. Silence here is what made「无声音」read like a bug: the
                # session that actually started does have audio, and only the
                # fallback says why it looks different from what was picked.
                message = ('设备拒绝了低延迟通道（{}），已改用兼容通道 LOAD，'
                           '这一路有声音。已开始镜像到 {}'.format(
                               refused, name))
        if self._audio_dropped:
            # Same rule as the fallback above: the picture arrived, so the
            # sentence is green -- but the sound the user asked for did not,
            # and only this line says so and where the door is. Which door is
            # the capture's to say: the SCK tap dies differently from the
            # two the platform sentences were written for.
            message += audio_dropped_suffix(method=capture.method)
        notify(message, sound=True)
        logger.info('mirroring screen (%s) to %s via %s', kind, name or 'LAN',
                    url)

    def _prefill(self, server, proc, generation):
        """Hold the URL back until the log holds this profile's prefill budget.

        The renderer's first move is a bounded sniff of a few megabytes; if we
        have to answer it by waiting on the encoder, the TV concludes the file
        is broken. Waiting here is what buys a steady read -- and the latency
        this target has, which is why the start message says so.
        """
        log = server.broadcaster
        budget = dlna_prefill_bytes(server.session.profile)
        deadline = time.time() + DLNA_PREFILL_TIMEOUT
        while time.time() < deadline and generation == self._generation:
            if log.bytes >= budget or proc.poll() is not None:
                break
            time.sleep(0.1)
        else:
            logger.info('prefill stopped at %s bytes', log.bytes)

    def _dlna_url(self, url, generation):
        """Bridge path for a DLNA renderer: play a pushed URL, no capture."""
        name, control = dlna_target()
        if not control:
            self._fail('还没有选择 DLNA 电视：在「电脑投屏」页的「投屏方式 → DLNA 电视」'
                       '里选一台', generation)
            return
        profile = dlna_profile()
        try:
            sender = _DlnaSender(control)
            # No `max_seconds` here on purpose: this path plays a URL somebody
            # else pushed, so no encoder of ours holds a `-t` that the advertised
            # length would have to agree with. Capping it would shorten a file we
            # are not writing.
            sender.set_uri(url, 'Macast · {}'.format(name), profile)
            sender.play()
        except Exception as e:
            self._fail('投给 {} 失败：{}'.format(name, e), generation)
            return
        with self._lock:
            if generation != self._generation:
                _close_quietly(sender)
                return
            self._sender = sender
        logger.info('cast %s to %s via %s', url, name, control)

    def _watch_dlna(self, sender, url, session, generation):
        """Keep a renderer that stopped on its own going.

        Old firmware pauses, seeks into a corner of the fake file, or just
        drops out of PLAYING when a sniff came back short. `GetTransportInfo`
        every few seconds is the only window into that; the success proof is
        `RelTime` actually moving, because a renderer will happily report
        PLAYING while it sits on a frozen frame.

        Two things count as alive, and only the first of them is the renderer's
        own word:

          * PLAYING with `RelTime` moving -- what this has always checked;
          * bytes being **delivered**. A live stream that a client is reading
            *is* playing, whatever the transport state says. Measured pushing to
            another Macast on the same LAN: 52 seconds of clean playback (mpv
            reporting vo-configured, MPEG-2 at 720x576, position advancing),
            one momentary STOPPED in the middle, and this watchdog restarted
            the picture over it.

        Delivered, not produced: the encoder runs at full rate whether or not
        anyone is pulling, so the byte counter on the broadcaster says "the
        mirror is alive" over a black screen. That is the difference between
        this signal rescuing a renderer caught between states and it excusing a
        player that opened the stream, could not parse it, and quit.

        So a single non-PLAYING answer is not a verdict -- a renderer reports
        STOPPED for a moment while its player opens the stream -- and a re-push
        now needs two polls in a row that say so.
        """
        misses = 0
        last_position = None
        last_bytes = None
        #: Where this session stood when the last URL went out. Two counters,
        #: because the two failures need different answers: a television that
        #: never fetched anything deserves every re-push we have, while one that
        #: fetched, was answered, and read no byte is refusing the shape -- and
        #: saying so early is the difference between 10 seconds and 40.
        asked_at_push = int(session.exchanges)
        took_at_push = int(session.written)
        refusals = 0
        while True:
            with self._lock:
                if generation != self._generation:
                    return
            time.sleep(DLNA_POLL_SECONDS)
            consumed = int(session.written)
            previous = last_bytes
            reading = previous is not None and consumed > previous
            read_now = consumed - previous if previous is not None else 0
            last_bytes = consumed
            try:
                state = sender.transport_state()
                position = sender.position()
            except Exception as e:
                logger.debug('DLNA poll failed: %s', e)
                misses += 1
                if misses >= DLNA_MAX_REPUSHES:
                    self._give_up(sender, '电视不再应答（可能已关机，或换了网络）')
                    self._retire_session()
                    return
                continue
            if state == 'PLAYING':
                if position and position != last_position:
                    last_position = position
                with self._lock:
                    self._dlna_state = state
                    self._dlna_reading = False
                misses = 0
                continue
            if reading:
                # Someone is reading this stream right now. The bytes are the
                # proof and they outrank a renderer caught between states.
                logger.debug('renderer says %s while %d bytes were being read;'
                             ' leaving it alone', state, read_now)
                with self._lock:
                    self._dlna_state = state
                    self._dlna_reading = True
                misses = 0
                continue
            with self._lock:
                self._dlna_state = state
                self._dlna_reading = False
            misses += 1
            if misses < 2:
                logger.info('renderer is %s; waiting for one more poll before'
                            ' pushing again', state)
                continue
            if misses >= DLNA_MAX_REPUSHES:
                if self._rotate_dlna_profile(session, '连续 {} 次没有播起来'
                                             .format(misses)):
                    return
                self._give_up(sender, '电视连续 {} 次没有播起来'.format(misses),
                              session)
                self._retire_session()
                return
            # Was the last push even fetched? `exchanges` moved but `written`
            # did not: the player looked at what this shape offers and put it
            # back. Retire that excuse early and say which of the two settings
            # the evidence points at.
            if (session.exchanges > asked_at_push
                    and int(session.written) <= took_at_push):
                refusals += 1
                if refusals >= DLNA_MAX_REFUSALS:
                    reason = ('电视取走了地址，却一个字节都没有读（连续 {} 次）'
                              .format(refusals))
                    if self._rotate_dlna_profile(session, reason):
                        return
                    self._give_up(sender, reason, session)
                    self._retire_session()
                    return
            else:
                refusals = 0
            logger.info('renderer is %s; pushing again (%d/%d)',
                        state, misses, DLNA_MAX_REPUSHES)
            try:
                sender.set_uri(url, '屏幕镜像 · {}'.format(session.page_title),
                               session.profile,
                               max_seconds=session.max_seconds)
                sender.play()
            except Exception as e:
                logger.debug('re-push failed: %s', e)
            asked_at_push = session.exchanges
            took_at_push = int(session.written)

    def _rotate_dlna_profile(self, session, reason):
        """Spend one rung of the compatibility ladder; True when it worked.

        Five shapes exist because the first one gets refused; until now the
        watching was the user's job, and the user of a screen mirror is looking
        at the television, not at the settings page. So the watchdog walks the
        list itself -- but only where the evidence points at the *container*,
        which is two situations and no others: a renderer that will not settle
        in PLAYING with nobody reading the stream, and one that fetches the URL
        and reads no byte.

        Three limits, each with a reason:

          * never in the file shape. A byte-refusing player there has been handed
            a fabricated size it went looking an index inside of; the fix is the
            shape, and five containers will not supply it -- `_give_up` has said
            so since that failure was measured, and rotating would bury it.
          * never more than `DLNA_MAX_PROFILE_TRIES` times per run, because a
            step is not a re-push: it restarts the encoder and costs the seconds
            a re-push would not.
          * never into the user's settings. `Mirror_Dlna_Profile` is their
            choice, and one session's television is not evidence about the next
            one; the ladder lives on this instance and `start_mirror` clears it,
            so the next click starts from what they picked.

        Ordering matters on the way out: bump the generation *before* teardown so
        the pump cannot report the encoder we are about to kill as「采集中断」
        (AGENTS.md 4.8), and tear down synchronously so the `start_mirror` that
        follows cannot find `_starting` still set and quietly do nothing -- which
        is the failure mode the manual restart path has, and a ladder that
        silently stops at rung 2 is worse than no ladder.
        """
        if session is None or getattr(session, 'bytelog', False):
            return False
        with self._lock:
            if self._profile_tries >= DLNA_MAX_PROFILE_TRIES:
                return False
            nxt = next_profile_id(session.profile)
            if nxt is None:
                return False
            self._profile_id = nxt
            self._profile_tries += 1
            step = self._profile_tries
            self._generation += 1
        logger.info('DLNA renderer refused this shape (%s); switching the '
                    'compatibility profile from %s to %s (%d/%d this run)',
                    reason, dlna_profile_id(session.profile), nxt, step,
                    DLNA_MAX_PROFILE_TRIES)
        notify('{}：已自动把兼容档位换成「{}」（本次第 {}/{} 档）。'
               '你在设置页里选的档位没有改动，下次手动开始镜像还是从它起步'.format(
                   reason, DLNA_PROFILES[nxt].label, step + 1,
                   len(DLNA_PROFILES)), sound=False)
        self._teardown()
        return self.start_mirror(keep_ladder=True)

    def _retire_session(self):
        """A run that just said「放弃」must not keep capturing the screen.

        `_fail` flips `_mirroring` off, and until here nothing else took the
        encoder or the HTTP service with it: the page read「未镜像」over a
        running ffmpeg, which is the same two-answers-for-one-state this plugin
        keeps having to fix. Guarded on owning a real encoder, because the
        watchdog is also driven in tests by a renderer that has a stubbed server
        and no process to retire.
        """
        if self._proc is None:
            return
        with self._lock:
            # The teardown below kills the encoder on purpose; without this
            # bump the pump reports that kill as「屏幕采集中断」，overwriting
            # the sentence that sent us here (同一形状见 `_stream_lost`).
            self._generation += 1
        self._teardown()

    def _give_up(self, sender, reason, session=None):
        """Stop blaming the TV one poll at a time and tell the user why.

        Which of the two DLNA knobs to turn is decided by what the session
        actually did, not by a fixed script. A byte-refusing player in the file
        shape has been handed a size it then went looking an index inside of,
        and the fix is the shape -- five containers will not supply it. For
        anything else the container inside the stream is the lever, which is
        what this has always said.

        Reaching here with a ladder already spent changes the sentence, not just
        its tone:「换成下一档」is advice the watchdog has follow four times
        already, and repeating it would send the user to re-do a loop that ran.
        """
        if session is not None and getattr(session, 'bytelog', False):
            self._fail('{}：这一条是把直播流「伪装成文件」投给电视的，'
                       '而它会去文件尾部找并不存在的索引。在「电脑投屏」页的'
                       '「投屏形状」里换成「{}」再试一次'.format(
                           reason, DLNA_SHAPES[DLNA_SHAPE_LIVE][0]),
                       self._generation)
            return
        profile = getattr(session, 'profile', None) or dlna_profile()
        if self._profile_tries:
            self._fail('{}：本次镜像已经自动换过 {} 档封装（最后一档「{}」），'
                       '它一种都没有接受。这不是档位能解决的问题：确认电视和这台 Mac'
                       '在同一个网络，或在「电脑投屏」页把「投屏形状」换成「{}」再试'
                       .format(reason, self._profile_tries,
                               DLNA_PROFILES[dlna_profile_id(profile)].label,
                               DLNA_SHAPES[DLNA_SHAPE_FILE][0]),
                       self._generation)
            return
        current = dlna_profile_id(profile)
        nxt = next_profile_id(profile) or profile_order()[0]
        self._fail('{}：当前档位是「{}」。在「电脑投屏」页的「兼容档位」里换成「{}」再试一次'.format(
            reason, DLNA_PROFILES[current].label,
            DLNA_PROFILES[nxt].label),
                   self._generation)

    def _stream_lost(self, reason):
        """The media loop stopped believing the television is there.

        Bumping the generation is what silences the pump: teardown kills the
        encoder, and without it the resulting exit code 15 would be reported as
        a capture failure on top of the real reason.
        """
        with self._lock:
            self._generation += 1
            generation = self._generation
        self._fail(reason, generation)
        self._teardown()

    def _sck_audio_absent(self):
        """The SCK audio tap never delivered; the session goes on video-only.

        Called back from the feeder's grace timer (`_abandon_audio`), by
        which time the start notification has already gone out -- so the
        durable answer lives on the page rather than in a second balloon:
        `audio_dropped()` turns True here and `_audio_line` stops saying
        「已启用」. Latching `_audio_refused` spares every later session on
        this run the same two-second grace.
        """
        with self._lock:
            self._audio_dropped = True
            self._audio_refused = True
        logger.warning('ScreenCaptureKit audio never arrived; this session'
                       ' continues without system sound')

    def _cast_url(self, url, generation):
        """Bridge path: play a finished URL on the device, no capture."""
        host, port, name = self.target()
        if host is None:
            self._fail('还没有选择投屏目标：在「电脑投屏」页的「投屏方式」里，'
                       '从 Chromecast / Google TV 那一行选一台设备', generation)
            return
        try:
            sender = _CastSender(host, port)
            sender.connect()
            sender.launch()
            sender.load(url)
        except Exception as e:
            self._fail('投屏到 {}（{}:{}）失败：{}'.format(name, host, port, e),
                       generation)
            return
        with self._lock:
            if generation != self._generation:
                _close_quietly(sender)
                return
            self._sender = sender
        logger.info('cast %s to %s (%s:%s)', url, name, host, port)

    def _encoder_died(self, generation, proc, started_at, produced=True):
        """ffmpeg exited while we still wanted it running.

        `produced` is whether any encoder output had arrived. A ddagrab or
        ScreenCaptureKit session that died before its first byte is answered
        by the starter thread -- fallback capture, or a failure sentence that
        carries the capture's own last words -- so reporting it here as well
        would race that. And a「启动失败」that is about to turn into a working
        fallback is a lie the user reads just before the picture that was
        already coming. Every other death, on every other capture, is
        reported here exactly as before -- with the feeder's recorded reason
        standing in for the exit code when there is one, because「ffmpeg 退出
        码 0」is what an SCK death with a closed stdin looks like on paper.
        """
        with self._lock:
            if generation != self._generation:
                return
            method = self._capture_method
            feeder = self._feeder
            #: The ceiling this session's argv carries, read off that argv when
            #: it was published (`_session_diagnostics`), not from the setting:
            #: three of the five shapes have none, and the setting is a number on
            #: every machine.
            bound = (self._diag or {}).get('max_seconds')
        if method in ('ddagrab', 'sck') and not produced:
            logger.info('ffmpeg (%s) exited before its first frame'
                        ' (code %s); the capture starter owns this verdict',
                        method, proc.poll())
            return
        code = proc.poll()
        why = feeder.failed() if feeder is not None else None
        if bound and time.time() - started_at >= bound:
            # The encoder wrote exactly as much picture as it was told to and
            # then stopped on its own `-t`. This branch only gets reached when
            # `_duration_expired` did not: that timer is armed on the same
            # ceiling and bumps the generation first, so a session it ended
            # reports nothing here. Reaching this line means that thread never
            # got to run -- and an OS that starved a daemon timer for a whole
            # session still has no business reporting「采集中断」about a mirror
            # that finished its job.
            logger.info('ffmpeg reached its own duration limit (%s s, exit %s),'
                        ' ending the session as planned', bound, code)
            self._duration_expired(generation, bound)
            return
        if time.time() - started_at < EARLY_DEATH_SECONDS:
            hint = ('：若是首次使用，{}，勾选后重启 Macast'.format(PERMISSION_DOOR)
                    if sys.platform == 'darwin' else '')
            self._fail('屏幕采集启动失败（{}）{}'.format(
                why or 'ffmpeg 退出码 {}'.format(code), hint), generation)
        else:
            self._fail('屏幕采集中断（{}）'.format(
                why or 'ffmpeg 退出码 {}'.format(code)), generation)
        self._teardown()

    def _arm_duration_timer(self, generation, max_seconds):
        """Stop *here*, at the ceiling, rather than letting the encoder's own
        `-t` be the event that ends the session.

        The notification, the transport state and the `caffeinate` release all
        live on this side of the pipe, and a pump reporting ffmpeg's planned exit
        would call it「屏幕采集中断」over a mirror that did exactly what it was
        told. The `-t` stays in the argv as the backstop for the case where this
        thread never gets to run -- but a timer nobody started is not a backstop
        for anything, so this method's whole job is to hand a *running* timer to
        `_teardown`.
        """
        timer = threading.Timer(max_seconds, self._duration_expired,
                                args=(generation, max_seconds))
        timer.daemon = True
        timer.name = 'SCREEN_MIRROR_MAX_DURATION'
        with self._lock:
            self._duration_timer = timer
        timer.start()

    def _duration_expired(self, generation, max_seconds):
        """The session ran into its ceiling: stop, and say which knob did it.

        Not `_fail`. Nothing went wrong -- the encoder wrote exactly the amount
        of picture it was told to -- so there is no transport error to publish
        and no ERROR line to leave in the log. What the user needs is the
        sentence naming the limit, because a mirror that stops on its own with
        no explanation reads like the crash they set a limit to contain.

        Reached from two places on purpose: the timer armed when the session was
        published, and `_encoder_died` when ffmpeg's own `-t` got there first.
        The generation guard makes the pair idempotent, so whichever arrives
        second is silent rather than sending a second notification.
        """
        with self._lock:
            if generation != self._generation:
                return
            # Bumped before anything is reaped, exactly like `stop_mirror`: the
            # pump thread is about to watch the encoder die, and that report must
            # not reach the user as「屏幕采集中断」.
            self._generation += 1
        phrase = max_duration_phrase(max_seconds)
        logger.info('mirroring stopped at its maximum duration (%s)', phrase)
        notify('已达投屏最大时长（{}），镜像已停止。要更长的时间就在'
               '「电脑投屏 → 画质」里换一档：最大时长'.format(phrase), sound=True)
        self._teardown()
        # The session really is over: the television is holding a frozen frame
        # and the encoder is gone, so leaving the published transport state at
        # PLAYING is the same stale claim the state page has been bitten by
        # before. `set_media_stop` uses this same word for this same physical
        # situation.
        self.set_state_transport('STOPPED')

    def _fail(self, message, generation):
        with self._lock:
            if generation != self._generation:
                return
            self._mirroring = False
        logger.error(message)
        self.set_state('CurrentTrackTitle', message)
        self.set_state_transport_error()
        notify(message, sound=True)

    def _teardown_async(self):
        threading.Thread(target=self._teardown, daemon=True,
                         name="SCREEN_MIRROR_TEARDOWN").start()

    def _teardown(self):
        with self._lock:
            sender, proc, server = self._sender, self._proc, self._server
            sink = self._sink
            feeder = self._feeder
            self._sender = self._proc = self._server = None
            self._sink = None
            self._feeder = None
            self._kind = ''
            self._diag = {}
            self._mirroring = False
            self._starting = False
            # Claim the assert here so a second teardown (the encoder dying
            # right after a manual stop) cannot release someone else's.
            awake, self._awake = self._awake, None
            # Same reason the generation is bumped first at every other stop: a
            # teardown for a *new* session must not leave the previous session's
            # ceiling timer armed to end it. `cancel()` on a thread that is
            # running its own callback is a no-op, so `_duration_expired` can
            # come through here safely.
            timer, self._duration_timer = self._duration_timer, None
        _cleanup(sender, proc, server, sink, feeder)
        if awake is not None:
            _stop_awake(awake)
        if timer is not None:
            timer.cancel()


def _cleanup(sender, proc, server, sink=None, feeder=None):
    for thing in (sender, sink):
        if thing is not None:
            thing.stop()
            _close_quietly(thing)
    if feeder is not None:
        # Ask the capture to end before killing the encoder: the feeder's
        # `finish` closes stdin's write end, and EOF should land where the
        # frame boundary is rather than mid-write.
        feeder.request_stop()
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
    if feeder is not None:
        # Joined only after the terminate: a writer blocked in `write()` on a
        # full pipe unblocks with BrokenPipe the moment the reader is gone,
        # so the joins cannot hold this up for long. The threads are daemons
        # regardless, and `finish` is idempotent.
        feeder.finish(timeout=2.0)
    if server is not None:
        # A DLNA reader is parked in _ByteLog.read() waiting for the next
        # byte; nothing else releases it before server_close() would block.
        close = getattr(server.broadcaster, 'close', None)
        if close is not None:
            close()
        server.shutdown()
        server.server_close()


def _close_quietly(sender):
    try:
        sender.close()
    except Exception:
        pass


def _pump(proc, broadcaster, owner, generation, first_bytes):
    """stdout -> broadcaster, and notice when the encoder dies."""
    started = time.time()
    stdout = proc.stdout
    while True:
        chunk = stdout.read(CHUNK)
        if not chunk:
            break
        broadcaster.feed(chunk)
        first_bytes.set()
    flush = getattr(broadcaster, 'flush', None)
    if flush is not None:
        # Only the low-latency sink has one: the last picture is still inside
        # the splitter when stdout closes, and an unterminated access unit is
        # a picture nobody ever sees.
        try:
            flush()
        except OSError:
            pass
    proc.wait()
    owner._encoder_died(generation, proc, started, first_bytes.is_set())


def _clean_env():
    """ffmpeg inherits the shell environment; proxies must not intercept the TV."""
    env = dict(os.environ)
    for key in ('http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY',
                'all_proxy', 'ALL_PROXY', 'no_proxy', 'NO_PROXY'):
        env.pop(key, None)
    return env


def _keep_awake(platform=None):
    """Return a handle on a sleep assertion, or None where we have no way.

    JustStream's most common complaint is the stream dying when the Mac falls
    asleep mid-presentation; `caffeinate -dimsu` is the stock answer (display,
    idle, system and disk assertions) and needs no entitlement. The handle is
    handed back so teardown can drop the assertion instead of leaving the
    machine awake forever.
    """
    if (platform or sys.platform) != 'darwin':
        return None
    try:
        return subprocess.Popen(['caffeinate', '-dimsu'],
                               stdin=subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL)
    except Exception as e:
        # Not having caffeinate is no reason to refuse the mirror.
        logger.info('cannot keep the machine awake: %s', e)
        return None


def _stop_awake(handle):
    if handle is None:
        return
    try:
        if handle.poll() is None:
            handle.terminate()
            handle.wait(timeout=3)
    except Exception:
        pass


def _drain_stderr(proc, tail):
    """Move ffmpeg's stderr into a bounded buffer, forever.

    An unread stderr pipe fills up and ffmpeg blocks inside the encoder -- which
    looks exactly like a dead mirror, with a healthy process and a stalled
    stream. The last few lines are kept because they are the only explanation a
    startup failure ever gives.
    """
    try:
        for raw in iter(proc.stderr.readline, b''):
            line = raw.decode('utf-8', 'replace').rstrip()
            if not line:
                continue
            if FFMPEG_NOISE.match(line):
                # Apple's objc runtime prints this on every avfoundation screen
                # grab, granted or not. It is the only thing a denied capture
                # ever says, so it must not crowd out the sentence we do want
                # the user to read -- the log below keeps it.
                logger.debug('ffmpeg (noise): %s', line)
                continue
            tail.append(line)
            logger.debug('ffmpeg: %s', line)
    except Exception as e:
        logger.debug('stderr drain ended: %s', e)
    finally:
        try:
            proc.stderr.close()
        except Exception:
            pass


# -- 投屏控制台：设置页那一整块的后端 ------------------------------------------
#
# Everything the menu used to carry (about fifty rows, nested four deep) now
# lives in the 电脑投屏 tab of the settings page: the menu keeps a door and the
# live status line, the page owns the controls. Two facts shaped this:
#
#   * the page talks to this plugin only over the management API -- so the whole
#     control surface is `console_state()` for reads and `console_action()` for
#     writes, and nothing else;
#   * that split is worth having on its own: the entire control surface is
#     testable without a browser, and every sentence the user reads is written
#     here, next to the setting it describes. What the *layout* of those facts
#     means is decided in the core (`macast/mirror_view.py`), not in the page's
#     JS -- a rule inside a cached page keeps running after an upgrade.
#
# One behaviour change came with the page. The menu said「下次镜像生效」for
# quality, screen, cursor and encoder, which is forgivable in a submenu you have
# to reopen and unforgivable next to a slider you just moved -- so applying any
# of them restarts a running mirror.

#: Bumped when the state/action contract changes; a page from another generation
#: says so in its banner instead of quietly missing buttons. `mirror_view.VIEW_VERSION`
#: is the same number on the core's side, and the regression suite pins the two.
CONSOLE_VERSION = 5

#: Screen-to-screen lag, measured on one Mac on 2026-10-03 with a flash
#: instrument: a borderless window paints black/white at recorded wall-clock
#: instants and the viewer page samples its own picture's luminance per frame.
#: Same machine, same epoch, so the difference between the two clocks *is* the
#: whole pipeline -- capture, encode, wire, player, compositor.
#:
#: Both numbers live in one dict because 「低延迟」 is a comparison, not a
#: property: the WebRTC line means nothing on its own, and means something when
#: the same instrument, on the same session conditions, says what the other
#: browser target costs. Conditions held fixed: VideoToolbox, avfoundation
#: capture, one display, a fresh session. Two caveats a reader needs:
#:   * a small flashing window is kept live beside the measured one, because
#:     macOS screen capture is change-gated -- on a static desktop every
#:     delivered frame is stamped 1/24 s apart no matter when it happened, so
#:     the media clock runs at roughly 0.4x wall and everything read off it
#:     inflates (this instrument reported 800-1500 ms for `browser` before that
#:     was understood);
#:   * the reference instant is taken inside the paint callback, before AppKit
#:     pushes the window to the display, so every figure is biased **high** by
#:     at most one capture frame.
#: The `browser` shape also ages: the same instrument read 800-1500 ms on a
#: page twenty minutes into one session, and the page presented only 7 of 17
#: flashes. These are fresh-session numbers. They are not the
#: `mse_latency_probe.py` figures -- that probe quotes the player's buffer edge
#: (838 ms for this shape), which is a different segment of the same chain, so
#: the two numbers must never be presented as alternatives to each other.
MEASURED_LAG_MS = {'browser': 492, 'webrtc': 369}

#: One line of trade-off language per target: the cards in the page have room to
#: say what choosing this costs, which a menu label never did.
#:
#: Two of these lines carry a number, and both numbers are computed from the
#: same function that decides them rather than typed in again -- 「上限 4.5
#: Mbps」outlived its own cause by a whole release once, when the keystream
#: moved to the operating system, and a hint that names a limit the code no
#: longer has sends the user looking for a setting that does not exist.
#: The other two carry `MEASURED_LAG_MS`, formatted at import from the same
#: table the help page and the regression suite are pinned against.
#: The `dlna` line is the *default-state* text: `output_hint` recomputes it with
#: the stored prefill, because a dict literal is evaluated at import and reading
#: a setting at import would persist it on a machine that never opened the page.
OUTPUT_HINTS = {
    'cast': ('兼容性最好 · 有声音 · 发送端缓冲约 0.8 秒，电视端解码另计'
             '（这一路的端到端延迟没在真电视上量过）'),
    'caststream': ('实验通道 · 加密走{crypto}，上限 {cap:.1f} Mbps · 本通道无声音 · '
                   '电视不认这一通道时会自动回落上面的兼容通道，那时是有声音的 · '
                   '未在真电视上验证过').format(
        crypto=('系统原生 AES（{}）'.format(native_aes_name())
                if _NATIVE_AES is not None else '纯 Python（已降级）'),
        cap=cast_stream_bitrate_cap() / 1000000.0),
    'dlna': ('给没有 Google 栈的老电视 · 「伪装成文件」会先攒约 {} 秒画面再交给它，'
             '所以一开始就有秒级延迟；默认的「直播流」不预填').format(
        DLNA_PREFILL_SECONDS),
    'browser': ('局域网内任意浏览器打开一个网址即可，无需安装 · '
                '本机实测屏幕到屏幕约 {browser} 毫秒（偏保守，误差最多一帧）'
                ).format(**MEASURED_LAG_MS),
    'webrtc': ('浏览器直连（WebRTC / SRTP）：没有播放器这一层缓冲 · '
               '此通道无声音 · 需要可选依赖 aiortc 与 av（未安装时开始会给出安装命令） · '
               '本机实测屏幕到屏幕约 {webrtc} 毫秒（偏保守，误差最多一帧），'
               '同一套测量里上面那条浏览器（MSE）通道是 {browser} 毫秒'
               ).format(**MEASURED_LAG_MS),
}

#: How often the picture actually changes when Windows reads its own loopback
#: device into a browser stream: 1.5 content changes per second, measured
#: screen-to-screen on 2026-10-03 with the .68 machine capturing and this one
#: watching (`docs/research-screen-mirroring-transport-2026-10.md` §11 -- twelve
#: arms of the production argv, scored by counting luminance crossings of a
#: flashing square inside the decoded frames, not bytes). `prod` scored 9
#: crossings in 6 s while the capture handed over 125 frames; the arm that opens
#: the same device and throws it away with `-an` -- which is exactly the shape
#: `webrtc` and `caststream` already use -- scored 45, and no ffmpeg knob
#: (buffer size, thread queue, our resampler, the interleave cap, MPEG-TS
#: instead of fragmented MP4) got back above 8.
#:
#: Two samples of the same A/B read 9 and 5, so this quotes the faster one: the
#: card would rather under-warn than overstate, and both readings are in the
#: document for anyone who wants the worse number.
#:
#: The number is a constant rather than a phrase because §4.8 has already buried
#: one card with 「上限 4.5 Mbps」 that outlived its own cause: a figure typed
#: into prose is a figure nobody re-checks.
WINDOWS_LOOPBACK_CONTENT_HZ = 1.5

#: The price of the browser target on the one machine that pays it, appended to
#: that target's own line by `output_hint`. It is a separate constant instead of
#: a second copy inside OUTPUT_HINTS because the literal has to stay the literal
#: -- it is what the help page, Part 52 and a no-argument call all compare
#: against, on a Mac, on Linux, and on the Windows box alike.
WINDOWS_BROWSER_SOUND_COST = (
    ' · Windows 上这一档的系统声音要读回环录音设备，代价落在画面上：跨机实测画面每秒只'
    '变化约 {hz} 次（采集交出 {fps} 帧每秒）。要完整画面就选「{webrtc}」，同一台机器'
    '同一条链路上它的解码与上屏实测都是 {fps} 帧每秒'
).format(hz=WINDOWS_LOOPBACK_CONTENT_HZ, fps=FPS, webrtc=OUTPUTS['webrtc'][0])


def system_audio_mapped():
    """Will the next session read a system-sound input into its output?

    Answers from the last probe and never runs one -- the console polls this
    view while it is open, and a probe spawns ffmpeg. `False` covers both "this
    platform has no tap" and "this machine has no loopback device", which is
    exactly the distinction the card does not need: either way nothing is read,
    so nothing is being paid.
    """
    capture = next(iter(_capture_cache.values()), None)
    return bool(capture and capture.audio_map)


#: The sentence the DLNA prefill control has to carry. Not in OUTPUT_HINTS
#: because it is about a knob, not about a target: the user lowering this number
#: is buying latency with a receiver that may stop playing, and that trade only
#: makes sense if the page says which side of it they are on.
DLNA_PREFILL_HINT = ('这个数字就是看得见的延迟：电视会先攒够这么多秒的画面才开始播，'
                     '然后一直慢这么多，不会追上来。调小更快，但太小时老电视可能'
                     '探测不出这是一个能播的流——本机没有那样的电视可量，'
                     '所以默认停在实测过的那一档。')

#: The pill row, so the page offers values the clamp accepts rather than a free
#: text field it has to argue with. Every entry sits inside
#: DLNA_PREFILL_MIN_SECONDS..DLNA_PREFILL_MAX_SECONDS by construction, and the
#: default is the measured one rather than the floor -- see the reasoning on
#: DLNA_PREFILL_MIN_SECONDS.
DLNA_PREFILL_OPTIONS = (1, 2, DLNA_PREFILL_SECONDS, 6, 8)


def output_hint(kind, platform=None, audio_mapped=False):
    """The trade-off line for one target, with any user setting folded in.

    Its own function because `OUTPUT_HINTS` is a module literal and two of its
    lines have to answer to more than the literal: one quotes a number the user
    can change, the other is only true of a Windows machine that is reading a
    loopback device. A card that says「先攒约 4 秒」while the session buffers 2 is
    the same lie as a stats overlay that disagrees with the counter it is
    supposed to be showing.

    `platform` and `audio_mapped` are the same seam `_audio_line` uses, for the
    reason §4.8 of AGENTS.md writes down: this function reads nothing off the
    machine it is *running on*, only about the machine its caller is
    *describing*. Filling the default in from `sys` here would make the sentence
    change with whichever box happens to execute the code -- and would stop a
    no-argument call from returning the literal, which is what the help page,
    the regression suite, and any caller that has not looked at a probe all
    compare against.
    """
    if kind == 'dlna':
        hint = ('给没有 Google 栈的老电视 · 「伪装成文件」会先攒约 {} 秒画面再交给它，'
                '所以一开始就有秒级延迟；默认的「直播流」不预填').format(
            dlna_prefill_seconds_setting())
    else:
        hint = OUTPUT_HINTS.get(kind, '')
    #: The one line that depends on which machine is being described. The
    #: measurement is capture-side, so the same read would cost the two
    #: television shapes as well -- but the browser row is the one the finding
    #: was measured through and the one the user approved, so the claim stops
    #: there instead of being extended to receivers nobody watched.
    if kind == 'browser' and platform == 'win32' and audio_mapped:
        return hint + WINDOWS_BROWSER_SOUND_COST
    return hint

# -- 投屏方式：先问用哪种协议，再问投给哪台设备 --------------------------------
#
# The window used to ask the second question first: it showed the device list of
# whichever target happened to be stored, so opening it on「浏览器」measured
# nothing and a Chromecast that had never been looked for was reported as「没有
# 发现」. One row per protocol the plugin can drive, each naming how its devices
# are *found* -- a receiver protocol written later adds a row and a probe, and
# the page lists it without knowing either name.
CHANNELS = (('cast', 'cast'),
            ('caststream', 'cast'),
            ('dlna', 'dlna'),
            ('browser', ''),
            ('webrtc', ''))
#: what an empty answer is called, per probe.「没有发现」is a verdict about the
#: LAN, and the two protocols have different things to have not found.
PROBE_LABELS = {'cast': ('Chromecast', '没有发现 Chromecast'),
                'dlna': ('DLNA 电视', '没有发现 DLNA 电视')}


def _probe_answer(probe):
    """(devices, searching, searched_at, error) for one way of searching.

    Reads the module globals when called, not when defined: the searches own
    them and the regression suite swaps them out.
    """
    if probe == 'cast':
        return _devices, _searching, _searched_at, _search_error
    if probe == 'dlna':
        return (_dlna_devices, _dlna_searching, _dlna_searched_at,
                _dlna_search_error)
    return [], False, 0.0, ''


def _probe_alone(probe):
    """How many of the last search's answers were this machine's own."""
    return _self_alone.get(probe, 0)


def _probe_unreadable(probe):
    """How many answers spoke the discovery protocol but never served a
    readable description. Only DLNA has that second step to fail."""
    return _dlna_unreadable if probe == 'dlna' else 0


def _probe_items(probe):
    """The device rows for one probe, `selected` from the stored choice."""
    if probe == 'cast':
        current = str(Setting.get(SettingProperty.Mirror_Target, '') or '')
        return [{'id': '{}:{}'.format(host, port),
                 'name': name,
                 'host': host,
                 'label': '{} · {}'.format(name, host),
                 'selected': '{}:{}'.format(host, port) == current}
                for name, host, port in list(_devices)]
    if probe == 'dlna':
        _name, control = dlna_target()
        return [{'id': device_control,
                 'name': device_name,
                 'host': host,
                 'label': '{} · {}'.format(device_name, host),
                 'selected': device_control == control}
                for device_name, device_control, host in list(_dlna_devices)]
    return []


def _search_words(devices, searching, searched_at, error, nothing='', alone=0,
                  unreadable=0, trace=None):
    """One phrase: what the last search of this protocol actually concluded.

    「正在搜索」is only ever true of a first look still in flight -- a completed
    empty search is a result, and says when it was taken. A search that could
    not run says that instead, because「没有发现」sends the user to look for a
    TV that is fine when the real problem is this machine.

    `alone` is the third case: the search ran, everything that answered was
    this Mac's own receiver, and those are dropped. Saying only「没有发现」here is
    what makes an empty but healthy LAN look like a broken search.

    `unreadable` is the fourth, and was silent until now: a device that answers
    SSDP and then fails the description GET leaves the list as if it had never
    spoken. A user told「没有发现 DLNA 电视」about a TV that *did* answer looks for
    a device that is not there, when the thing to do is wake it or turn its
    DLNA on. Every empty branch here ends in the next step, not just a verdict.

    `trace` is how many interfaces the search actually tried (see
    `_ask_renderers_everywhere`). It only ever adds a clause: on the last-resort
    branch, "nothing answered" and "we asked from five different NICs and none
    of them was heard" are different facts, and only the second one means the
    user should look at which adapter is on the TV's network.
    """
    if devices:
        if unreadable:
            return '发现 {} 台（另有 {} 台应答了发现但读不到描述，已跳过）'\
                .format(len(devices), unreadable)
        return '发现 {} 台'.format(len(devices))
    if error:
        return error
    if searching:
        return '正在搜索局域网设备…' if not searched_at else '正在重新搜索…'
    if not searched_at:
        return '还没有搜过'
    if alone:
        return (nothing + '：局域网里只有这台 Mac 自己的接收端，已排除；'
                '电视需要和这台 Mac 连同一个网络才会应答，'
                '确认后点「重搜设备」'
                + _searched_suffix(searched_at))
    if unreadable:
        return ('有 {} 台设备应答了发现，但都读不到描述：通常是它在休眠，'
                '或者固件不报 AVTransport。唤醒它或在它上面打开 DLNA/投屏，'
                '然后点「重搜设备」' + _searched_suffix(searched_at))
    words = (nothing + '；如果设备就在这台 Mac 的同一个网络里，'
             '检查它是否开机、再点「重搜设备」')
    if trace and len(trace) > 1:
        words += ('（已从 {} 个网卡分别发过发现，都没有应答：确认电视和这台电脑'
                  '在同一个网段，或在上面的网络设置里指定网卡）'.format(len(trace)))
    return words + _searched_suffix(searched_at)


def _probe_chosen(probe):
    """Whether the device a protocol would use is actually stored.

    Asked of the setting, not of the device list: a Chromecast that answered
    once and is now switched off is still the target the user chose, and start
    reads it from there. Judging readiness by list membership would tell them
    to choose again the moment the search stopped seeing it.
    """
    if probe == 'cast':
        return bool(str(Setting.get(SettingProperty.Mirror_Target, '') or ''))
    if probe == 'dlna':
        return bool(str(Setting.get(SettingProperty.Mirror_Dlna_Control, '')
                        or ''))
    return True


def _probe_choice(probe, items):
    """What this protocol would actually mirror to, as one line -- or ''.

    Read off the stored setting, not off the row the user is looking at: a
    Chromecast that answered once and is now switched off is still the chosen
    target, and the row has to say so honestly instead of claiming a device
    that start would never use.
    """
    if not probe:
        return ''
    if not _probe_chosen(probe):
        return ''
    hit = next((item['label'] for item in items if item['selected']), '')
    return '投给 ' + hit if hit else '已选设备本轮没有应答'


def channels_state(current=None):
    """Every protocol this plugin can mirror over, and what each one answers to."""
    current = output_kind() if current is None else current
    #: The two facts a trade-off line can depend on the machine for, read once
    #: for the whole list: they cannot change between one row and the next, and
    #: rows that disagree about whether sound is being read would read as a
    #: rendering bug rather than as an answer.
    machine, sound_is_read = sys.platform, system_audio_mapped()
    out = []
    for key, probe in CHANNELS:
        _, searching, searched_at, error = _probe_answer(probe)
        items = _probe_items(probe) if probe else []
        #: A protocol with nothing to search has no verdict to report. Saying
        #: 「还没有搜过」 over the browser target would send the user to look for
        #: a device that does not exist; the page fills the blank with
        #: 「不需要设备」.
        words = '' if not probe else _search_words(
            items, searching, searched_at, error,
            PROBE_LABELS.get(probe, ('', '没有发现设备'))[1],
            alone=_probe_alone(probe), unreadable=_probe_unreadable(probe),
            trace=dlna_trace() if probe == 'dlna' else None)
        out.append({'key': key,
                    'label': OUTPUTS[key][0],
                    'hint': output_hint(key, platform=machine,
                                        audio_mapped=sound_is_read),
                    'probe': probe,
                    'selected': key == current,
                    'devices': items,
                    'searching': searching,
                    'searched_at': searched_at,
                    'error': error,
                    'words': words,
                    'choice': _probe_choice(probe, items),
                    'needs_device': bool(probe),
                    'chosen': _probe_chosen(probe)})
    return out


def target_prompt(kind=None, with_verdict=False):
    """What still has to be chosen before this target can start, '' if nothing.

    One function writes both the line the console shows and the line a refused
    start reports -- they used to disagree, and「…或改用「浏览器」目标」appeared
    on a page that was already on the browser target.

    `with_verdict` appends what the last search concluded. The page does not
    want it: it prints that verdict on the line above, and「没有发现 Chromecast」
    twice inside one card reads like a bug rather than like an answer. A refused
    start does want it -- that one line leaves the page for a notification,
    where the reason is all the user gets.
    """
    kind = output_kind() if kind is None else kind
    for key, probe in CHANNELS:
        if key != kind or not probe or _probe_chosen(probe):
            continue
        name, nothing = PROBE_LABELS[probe]
        line = '还没有选择「{}」：在「设备」里点一台'.format(name)
        if with_verdict:
            devices, searching, searched_at, error = _probe_answer(probe)
            line += '（{}）'.format(_search_words(
                devices, searching, searched_at, error, nothing,
                alone=_probe_alone(probe), unreadable=_probe_unreadable(probe)))
        return line
    return ''

# -- 预览快照：低帧率的「这台电脑现在投出去的是什么」 --------------------------
#
# A mirror with no picture of its own is a guess: the page cannot show what the
# TV sees unless it grabs the screen too. This is deliberately *not* a video
# preview -- one ffmpeg per request, single-flight, at most one per second, and
# the last good frame stays on screen through a failure, because the alternative
# to a stale frame is a black rectangle where the picture was.

#: Minimum spacing between grabs, and the pause after one fails (a machine
#: without Screen Recording permission would otherwise pay a process spawn
#: every second for the rest of the session).
SNAPSHOT_MIN_INTERVAL = 1.0
SNAPSHOT_RETRY_INTERVAL = 15.0
#: A first grab on macOS waits for the Screen Recording prompt to be answered,
#: which is minutes when the user is away -- this only bounds the ffmpeg itself.
SNAPSHOT_TIMEOUT = 8.0
#: Wide enough to read a slide deck at, cheap enough to encode in one frame.
SNAPSHOT_WIDTH = 480
#: What a real PNG starts with. ffmpeg is asked for no encoder and no muxer --
#: the `.png` output extension already selects one -- and these magic bytes are
#: how we notice when a differently-built ffmpeg answered with something else.
SNAPSHOT_MAGIC = b'\x89PNG'

_snapshot_lock = threading.Lock()
_snapshot = {'frame': b'', 'at': 0.0, 'busy': False,
             'failed_at': 0.0, 'reason': ''}


def snapshot_frame(min_interval=SNAPSHOT_MIN_INTERVAL):
    """(frame bytes, reason) -- newest preview frame, or why there isn't one.

    Blocking by design: the caller is an HTTP handler in a worker thread, and a
    second grab in flight would mean two ffmpegs reading the same display.
    """
    now = time.time()
    with _snapshot_lock:
        if _snapshot['busy']:
            return _snapshot['frame'], '正在采集上一帧'
        if _snapshot['frame'] and now - _snapshot['at'] < min_interval:
            return _snapshot['frame'], ''
        if (_snapshot['failed_at']
                and now - _snapshot['failed_at'] < SNAPSHOT_RETRY_INTERVAL):
            return _snapshot['frame'], _snapshot['reason']
        _snapshot['busy'] = True
    try:
        frame, reason = _grab_snapshot()
    finally:
        with _snapshot_lock:
            _snapshot['busy'] = False
            _snapshot['reason'] = reason
            if frame:
                _snapshot['frame'] = frame
                _snapshot['at'] = time.time()
                _snapshot['failed_at'] = 0.0
            else:
                _snapshot['failed_at'] = time.time()
            retained = _snapshot['frame']
    # `retained` rather than `frame`: a failed grab still owes the page the
    # last good picture, because the alternative is a black rectangle.
    return retained, reason


def snapshot_state():
    """What the page needs to know about the preview without asking for it."""
    with _snapshot_lock:
        return {'busy': _snapshot['busy'],
                'reason': _snapshot['reason'],
                'has_frame': bool(_snapshot['frame']),
                'at': _snapshot['at']}


#: Why the preview stands down while a mirror runs. Measured on macOS: a second
#: avfoundation grab of a display the mirror is already reading never produces a
#: frame -- it costs the full `SNAPSHOT_TIMEOUT` and returns nothing.
MIRRORING_PREVIEW_NOTE = '镜像进行中：画面正发给观看端，预览不再另开一路采集'


def snapshot_wanted():
    """Is a first preview frame still missing, with nobody on its way to get it?

    The page's `<img>` only exists once a frame does, so nothing in the page ever
    requested the *first* one -- 「正在抓取第一帧…」was a permanent answer on a
    freshly started app. The frame rate after that is the image's own doing (its
    URL changes when `at` does), which is why this asks once rather than always.
    """
    now = time.time()
    with _snapshot_lock:
        if _snapshot['busy'] or _snapshot['frame']:
            return False
        if (_snapshot['failed_at']
                and now - _snapshot['failed_at'] < SNAPSHOT_RETRY_INTERVAL):
            return False
        return True


def _frame_path():
    import tempfile
    return os.path.join(tempfile.gettempdir(), 'macast-mirror-{}-{}.png'.format(
        os.getpid(), secrets.token_hex(4)))


def _grab_snapshot():
    """One frame of what this machine would be sending right now.

    Same capture path as the mirror (first input, video only), so the preview
    cannot disagree with the stream about which display or which crop is live.
    """
    ffmpeg = ffmpeg_requirement()
    if ffmpeg is None:
        return b'', '找不到 ffmpeg，无法生成预览'
    capture = probe_capture(ffmpeg)
    if capture is None:
        return b'', capture_unavailable_hint()
    path = _frame_path()
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin']
    cmd += list(capture.inputs[0])
    cmd += ['-map', '0:v:0', '-frames:v', '1',
            '-vf', 'scale={}:-2'.format(SNAPSHOT_WIDTH)]
    cmd += ['-y', path]
    data = b''
    try:
        proc = subprocess.run(cmd, stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, stdin=subprocess.DEVNULL,
                              env=_clean_env(), timeout=SNAPSHOT_TIMEOUT)
        if proc.returncode != 0:
            err = (proc.stderr or b'').decode('utf-8', 'replace').strip()
            return b'', '预览采集失败（ffmpeg 退出码 {}）{}'.format(
                proc.returncode, '：' + err.splitlines()[-1] if err else '')
        with open(path, 'rb') as handle:
            data = handle.read()
    except subprocess.TimeoutExpired:
        # `str(e)` spells out the whole argv, which is a wall of text in a card
        # whose only useful content is「it did not finish」-- and a locked or
        # sleeping display is exactly the case this fires on. It is not the only
        # case, though: a build that was never granted 屏幕录制 sits here too,
        # and a fresh source checkout lands in that second one every time. The
        # rule the capture path follows -- every「没有返回画面」names the door --
        # applies to the preview as well, because the preview is where a user
        # looks first when the mirror will not start.
        return b'', ('预览采集超时（{:.0f} 秒）：屏幕可能锁了，也可能是这个 Macast '
                     '还没有屏幕录制授权 —— {}，勾选后重启 Macast').format(
            SNAPSHOT_TIMEOUT, PERMISSION_DOOR)
    except Exception as e:
        return b'', '预览采集失败：{}'.format(e)
    finally:
        _remove_quietly(path)
    if not data.startswith(SNAPSHOT_MAGIC):
        return b'', 'ffmpeg 没有返回 PNG 画面'
    return data, ''


# -- 采集探测：菜单/页面都只读缓存，探测在后台跑 -------------------------------

_probe_busy = threading.Event()


def request_capture_probe():
    """Warm the capture and encoder caches in the background; False if busy.

    Both probes spawn ffmpeg, and neither the menu nor the console's polling
    loop may do that on the UI thread -- the console asks once when it opens and
    again when the user presses「重新探测」.
    """
    if _probe_busy.is_set():
        return False
    _probe_busy.set()
    threading.Thread(target=_run_capture_probe, daemon=True,
                     name='SCREEN_MIRROR_PROBE').start()
    return True


def _run_capture_probe():
    try:
        ffmpeg = ffmpeg_requirement()
        if ffmpeg is None:
            notify('找不到 ffmpeg：镜像与预览都没法工作，先安装 ffmpeg')
            return
        if probe_capture(ffmpeg) is None:
            notify(capture_unavailable_hint())
        has_hardware_encoder(ffmpeg)
    except Exception as e:
        logger.info('capture probe failed: %s', e)
    finally:
        _probe_busy.clear()




class ScreenMirrorSetting(RendererSetting):

    #: The renderer that built this surface (`ScreenMirrorRenderer.__init__`).
    #: Deliberately not the bus's `get_renderer`: that answers「whoever is
    #: playing media right now」, which would make this surface depend on Screen
    #: Mirror being the selected renderer -- and capturing this desktop is an act
    #: with nothing to do with whichever player holds the DLNA stream.
    _owner = None

    def _renderer(self):
        return self._owner

    def mirror_running(self):
        """Is this machine's screen being captured right now?

        The menu bar asks, and keeps a「停止电脑投屏」row for exactly as long as
        the answer is yes: a browser tab -- minimised, closed, or open on another
        machine -- must never be the only way out of a live capture.
        """
        renderer = self._renderer()
        return bool(renderer and renderer.is_mirroring())

    # -- the console's view model ----------------------------------------------

    def console_state(self):
        """One JSON-ready description of everything the page shows.

        Deliberately a view model rather than markup: the page is another
        program's renderer, and keeping the strings here means the regression
        suite can assert on the whole control surface without a browser.
        """
        renderer = self._renderer()
        mirroring = bool(renderer and renderer.is_mirroring())
        kind = output_kind()
        self._kick_searches()
        #: Same reason `channels_state` reads these once: the「投屏方式」cards and
        #: the protocol rows below are two renderings of one answer, so they get
        #: one call to the machine and one to the probe.
        machine, sound_is_read = sys.platform, system_audio_mapped()
        return {
            'console_version': CONSOLE_VERSION,
            'version': PLUGIN_VERSION,
            'platform': sys.platform,
            # False means the plugin could not build a session at all (no
            # renderer behind this surface): the console says so instead of
            # letting a start button do nothing.
            'available': renderer is not None,
            'mirroring': mirroring,
            'starting': bool(renderer and renderer.is_starting()),
            'status_line': (self._status_line(renderer) if mirroring else ''),
            'stats': renderer.stats() if renderer is not None else {},
            'recent': recent_messages(),
            'requirements': notice.requirements(),
            #: The optional WebRTC bundle's whole answer: whether this machine
            #: has a build to download, whether the probe can import it right
            #: now, and the install's own step list. One reading, because the
            #: card must not say「已安装」next to「不可用」.
            'extras': extras_state(),
            'output': {
                'kind': kind,
                'options': [{'key': key,
                             'label': OUTPUTS[key][0],
                             'hint': output_hint(key, platform=machine,
                                                 audio_mapped=sound_is_read)}
                            for key, _probe in CHANNELS],
            },
            'channels': channels_state(kind),
            #: Which interfaces the last DLNA search tried, and what each one
            #: answered. Raw, because "电视没开" and "发现包根本没从对的网卡出去"
            #: are the same sentence on screen ("没有发现") and completely
            #: different problems to fix.
            'search_trace': {'dlna': dlna_trace()},
            'prompt': target_prompt(kind),
            'profiles': {
                # The rung the page lights is the renderer's answer, not this
                # object's: with a ladder running it is the shape being encoded,
                # not the stored choice. The fallback is for the case the suite
                # hits first -- a `ScreenMirrorSetting()` nobody has installed as
                # the current renderer -- where the card still has to name what
                # the next click would start from.
                'current': (renderer.dlna_profile_display()
                            if renderer is not None
                            else dlna_profile_id(dlna_profile())),
                'options': [{'key': key, 'label': profile.label,
                             'cost': ' · '.join(
                                 '{} {} 秒'.format(word, value)
                                 for word, value in zip(
                                     ('首帧', '落后'),
                                     DLNA_PROFILE_LATENCY.get(key, ('—', '—'))))}
                            for key, profile in DLNA_PROFILES.items()],
                'note': dlna_profile_note(),
            },
            #: How the DLNA target answers the renderer. Live by default, and
            #: the page says so as a choice rather than a hidden default: the
            #: endless-file shape is what a television that only plays finite
            #: files needs, and every modern client (mpv on the receiving
            #: machine, an Android 11 television) treats its fabricated size as
            #: real, hunts for a container index at the end of it, and starts
            #: over instead of playing.
            'shape': {
                'current': dlna_shape(),
                'options': [{'key': key, 'label': DLNA_SHAPES[key][0],
                             'hint': DLNA_SHAPES[key][1]}
                            for key in (DLNA_SHAPE_LIVE, DLNA_SHAPE_FILE)],
                #: The prefill lives on this card rather than getting one of its
                #: own because it only exists for the endless-file shape above,
                #: and a control two cards away from the thing that makes it
                #: meaningful reads as a setting for the whole target.
                'prefill': {
                    'current': dlna_prefill_seconds_setting(),
                    'options': [{'key': str(sec),
                                 'label': '{} 秒{}'.format(
                                     sec, '（默认）'
                                     if sec == DLNA_PREFILL_SECONDS else '')}
                                for sec in DLNA_PREFILL_OPTIONS],
                    'note': DLNA_PREFILL_HINT,
                },
            },
            'quality': self._quality_state(kind),
            'capture': self._capture_state(),
            'audio': self._audio_state(renderer),
            'viewer': self._viewer_state(renderer, mirroring, kind),
            'preview': self._preview_state(mirroring),
        }

    def snapshot_frame(self):
        """The preview frame the settings page polls for, as PNG.

        Reached through the same surface as the state and the actions, but note
        what it does *not* need: the running session. The grab goes straight to
        the capture probe, so the picture is there before a mirror starts and
        after it stops -- which is the point of a preview. While one *is* running
        it stands down: the mirror holds that display, and a second capture of it
        spends eight seconds to produce nothing (see `MIRRORING_PREVIEW_NOTE`).
        """
        renderer = self._renderer()
        if renderer is not None and renderer.is_mirroring():
            return b'', MIRRORING_PREVIEW_NOTE
        return snapshot_frame()

    def request_preview(self):
        """Sponsor the first preview frame; the page's `<img>` asks for the rest.

        A grab is an ffmpeg, so this never runs inside the request that asked for
        the state -- and it never runs while the mirror holds the display.
        """
        renderer = self._renderer()
        if renderer is not None and renderer.is_mirroring():
            return False
        if not snapshot_wanted():
            return False
        threading.Thread(target=snapshot_frame, daemon=True,
                         name='SCREEN_MIRROR_SNAPSHOT').start()
        return True

    @staticmethod
    def _preview_state(mirroring):
        """The preview card's data, with the mirror's claim on it applied."""
        state = snapshot_state()
        if mirroring:
            # A frame grabbed before the mirror started would keep refreshing
            # into a stale picture presented as live, so the card says why it
            # stands down instead of showing one.
            state['has_frame'] = False
            state['reason'] = MIRRORING_PREVIEW_NOTE
        return state

    @staticmethod
    def _kick_searches():
        """Look for *every* protocol's devices, not just the stored choice's.

        The page's first panel answers「哪种协议有东西可投」, which needs all
        of the answers: searching only the current target is how an opened
        console reported「没有发现」about a Chromecast it had never looked for.

        The 15-second guard the menu grew stays, and matters more now: the
        console polls this view once a second, so without it a page merely being
        open would keep the LAN busy forever.
        """
        now = time.time()
        if not _searching and _search_due(_searched_at, now, bool(_devices)):
            start_search()
        if not _dlna_searching and _search_due(_dlna_searched_at, now,
                                               bool(_dlna_devices)):
            start_renderer_search()

    def request_probes(self):
        """Warm the capture and encoder caches if this page has never seen them.

        Called by the mirror-state endpoint rather than by `console_state()`, so
        the state builder stays a pure read and the caller that has to render
        「正在探测…」is the one that makes it come true. Both answers come from
        spawning ffmpeg, so neither is taken on the request that asks for it --
        but a spinner that never resolves is a lie by omission, and the 编码 row
        rots worst: the preview's own grab warms the capture cache and never the
        encoder one, which is how a Mac was told a probe was in flight forever.

        The guard is the caches themselves, because the page polls once a second;
        `request_capture_probe()` adds the single-flight one.
        """
        if _capture_cache and (hardware_encoder() is None
                               or _hw_encoder_cache):
            return
        request_capture_probe()

    @staticmethod
    def _capture_state():
        """What the last probe decided -- never probes, the console polls this.
        """
        capture = next(iter(_capture_cache.values()), None)
        wanted = str(Setting.get(SettingProperty.Mirror_Screen, '') or '')
        screens = [{'index': '', 'label': '第一块屏幕（默认）',
                     'selected': wanted == ''}]
        for index, name in (capture.screens if capture else []):
            screens.append({'index': str(index),
                            'label': '{} · {}'.format(index, name),
                            'selected': wanted == str(index)})
        hardware = hardware_encoder() is not None
        #: None means the encoder probe has not answered yet; the console hides
        #: the switch rather than offering a choice it cannot keep.
        hardware_available = _hw_encoder_cache.get(
            (find_ffmpeg(), sys.platform)) if hardware else None
        return {
            'probed': capture is not None,
            'probing': _probe_busy.is_set(),
            'label': capture.label if capture else '',
            'screens': screens,
            'cursor': cursor_enabled(),
            'encoder': encoder_kind(),
            'hardware_supported': hardware,
            #: The name this platform's「硬件编码」actually buys (VideoToolbox on a
            #: Mac, NVENC on Windows) and the words the switch wears -- the page
            #: used to hard-code macOS's name for a control that means something
            #: else on every other platform.
            'hardware_encoder': hardware_encoder() or '',
            'hardware_label': encoder_switch_label(),
            'hardware_probed': hardware_available is not None,
            'hardware_available': bool(hardware_available),
            #: Only non-empty where the switch itself is shown: on a machine with
            #: no hardware encoder at all there is nothing to turn off, and a
            #: note about a choice the page is not offering reads as a bug.
            'encoder_note': (encoder_tradeoff()
                             if hardware and hardware_available else ''),
        }

    @staticmethod
    def _audio_state(renderer=None):
        capture = next(iter(_capture_cache.values()), None)
        progress = _audio_progress.snapshot() if _audio_progress else {}
        return {
            'line': ScreenMirrorSetting._audio_line(renderer),
            'capturable': bool(capture and capture.audio_map),
            'setup_available': sys.platform == 'darwin' and audio_setup_needed(),
            'restore_available': Setting.get(
                SettingProperty.Mirror_Audio_Original, None) not in (None, ''),
            'running': _audio_setup_busy.is_set(),
            'steps': progress.get('steps', []),
            'pct': progress.get('pct', 0.0),
            'message': progress.get('message', ''),
            'done': progress.get('done', False),
            'ok': progress.get('ok'),
            'progress_url': _audio_progress_url,
        }

    def _quality_state(self, kind):
        """The「画质」card: the bitrate rungs, plus the ceiling where one exists.

        The ceiling is nested here instead of being its own card because it only
        exists for the two television shapes, and because it belongs next to the
        number that decides how many bytes an hour of mirror costs -- 12 hours at
        6 Mbps is a different promise than 12 hours at 1.5 Mbps, and the two are
        read together or not at all.

        Absent (no key at all) for the three shapes that ignore it, which the
        page hides. A pill that changes a setting nothing obeys is the same lie
        here as a stale hint would be.
        """
        state = {
            'current': quality_key(),
            'options': [{'key': key, 'label': QUALITY_LABELS[key]}
                        for key in QUALITY_ORDER],
            'note': self._quality_note() or '',
        }
        if kind not in MAX_DURATION_TARGETS:
            return state
        state['max_duration'] = {
            'current': max_duration_hours(),
            'options': [{'key': str(one),
                         'label': '{} 小时{}'.format(
                             one, '（默认）'
                             if one == DEFAULT_MAX_DURATION_HOURS else '')}
                        for one in MAX_DURATION_HOURS],
            'note': MAX_DURATION_HINT,
        }
        return state

    @staticmethod
    def _viewer_state(renderer, mirroring, kind):
        url = renderer.viewer_url() if renderer is not None else ''
        hint = '不要把这条地址转发出去：它带着本次会话的观看令牌' if url else (
            '开始镜像后这里会给出观看地址'
            if kind in ('browser', 'webrtc') else '')
        state = {'url': url, 'available': bool(url), 'hint': hint,
                 'kind': kind, 'mirroring': mirroring}
        if kind == 'webrtc':
            # No park knob on this target: SRTP plays as it arrives, there is
            # no player buffer to hold back and no backlog to seek within.
            # The page hides the row when the key is absent -- a control that
            # adjusts nothing is the same lie here as a stale hint would be.
            return state
        current = live_edge_seconds()
        state['live_edge'] = {
            'current': current,
            'options': [{'key': repr(sec),
                         'label': '{} 秒{}'.format(
                             ('%g' % sec), '（默认）'
                             if sec == LIVE_EDGE_SECONDS else '')}
                        for sec in LIVE_EDGE_OPTIONS],
            'note': LIVE_EDGE_HINT,
        }
        return state

    # -- the console's actions --------------------------------------------------

    #: Everything the page may do, in one list: the HTTP endpoint refuses any
    #: name that is not here rather than dispatching on it.
    CONSOLE_ACTIONS = ('start', 'stop', 'toggle', 'set-output', 'set-target',
                       'set-dlna-target', 'set-dlna-shape', 'set-profile',
                       'set-dlna-prefill', 'set-quality', 'set-max-duration',
                       'set-screen', 'set-cursor', 'set-encoder',
                       'set-live-edge', 'refresh', 'probe', 'audio-setup',
                       'audio-restore')

    def console_action(self, action, args=None):
        """Single entry point for the console; {'code', 'message'} either way.

        `args` comes off the network, so nothing here trusts its shape -- every
        value is validated against the same tables the state reports from.
        """
        args = args if isinstance(args, dict) else {}
        if action not in self.CONSOLE_ACTIONS:
            return {'code': 1, 'message': '未知操作：{}'.format(action)}
        return getattr(self, '_do_' + action.replace('-', '_'))(args)

    def _ok(self, message, restart=False):
        notify(message, sound=False)
        if restart:
            self._restart()
        return {'code': 0, 'message': message}

    def _no(self, message):
        logger.info('console action refused: %s', message)
        return {'code': 1, 'message': message}

    def _renderer_or_no(self):
        renderer = self._renderer()
        if renderer is None:
            return None, self._no('这个 Screen Mirror 插件实例没有加载成功：在设置页'
                                  '的「插件」里重新启用它再试')
        return renderer, None

    def _do_start(self, args):
        renderer, refused = self._renderer_or_no()
        if refused:
            return refused
        if renderer.is_mirroring():
            return self._ok('已经在镜像了')
        if not renderer.start_mirror():
            return self._ok('正在开始镜像…（上一台还在启动）')
        return {'code': 0, 'message': '正在开始镜像…'}

    def _do_stop(self, args):
        renderer, refused = self._renderer_or_no()
        if refused:
            return refused
        if not renderer.is_mirroring() and not renderer.is_starting():
            return self._ok('现在没有镜像')
        renderer.stop_mirror()
        return self._ok('已停止镜像')

    def _do_toggle(self, args):
        renderer, refused = self._renderer_or_no()
        if refused:
            return refused
        if renderer.is_mirroring():
            return self._do_stop(args)
        return self._do_start(args)

    def _do_set_output(self, args):
        key = str(args.get('value') or '')
        if key not in OUTPUTS:
            return self._no('没有这种投屏方式：{}'.format(key))
        if key == output_kind():
            return {'code': 0,
                    'message': '投屏方式已经是{}'.format(OUTPUTS[key][0])}
        Setting.set(SettingProperty.Mirror_Output, key)
        return self._ok('投屏方式：{}（正在切换封装与目标）'.format(OUTPUTS[key][0]),
                        restart=True)

    def _do_set_target(self, args):
        target = str(args.get('target') or '')
        name = str(args.get('name') or '') or target
        if not target.partition(':')[0]:
            return self._no('没有给出 Chromecast 地址')
        Setting.set(SettingProperty.Mirror_Target, target)
        Setting.set(SettingProperty.Mirror_Target_Name, name)
        return self._ok('镜像目标：{}'.format(name), restart=True)

    def _do_set_dlna_target(self, args):
        control = str(args.get('control') or '')
        name = str(args.get('name') or '') or control
        if not control:
            return self._no('没有给出 DLNA 电视的控制地址')
        Setting.set(SettingProperty.Mirror_Dlna_Control, control)
        Setting.set(SettingProperty.Mirror_Target_Name, name)
        return self._ok('DLNA 电视：{}'.format(name), restart=True)

    def _do_set_profile(self, args):
        """A profile is a different encoder, muxer and advertised file, so a
        running stream is wrong the instant the choice changes; it restarts for
        the same reason the menu did -- the point of five shapes is that the TV
        rejects the first one, and asking the user to flip the mirror by hand
        between each try is busywork.

        The comparison is with `dlna_profile_display()`, not with the stored
        setting: after the watchdog moved a run to another rung, the pill the
        page lights *is* the one in effect, and clicking it has to say「已经是」
        rather than write a setting and restart onto the same container.
        """
        key = str(args.get('value') or '')
        if key not in DLNA_PROFILES:
            return self._no('没有这个兼容档位：{}'.format(key))
        renderer = self._renderer()
        shown = (renderer.dlna_profile_display() if renderer is not None
                 else default_dlna_profile_id())
        if key == shown:
            return {'code': 0, 'message': '档位已经是{}'.format(DLNA_PROFILES[key].label)}
        Setting.set(SettingProperty.Mirror_Dlna_Profile, key)
        return self._ok('兼容档位：{}'.format(DLNA_PROFILES[key].label),
                        restart=True)

    def _do_set_dlna_shape(self, args):
        """How the DLNA target answers a renderer: live stream, or fake file.

        Restarts for the same reason a profile change does: the whole shape of
        every answer -- headers, DIDL, the size the renderer believes in -- is
        settled when the session is built, so a running one would keep the
        shape the user just rejected.
        """
        key = str(args.get('value') or '')
        if key not in DLNA_SHAPES:
            return self._no('没有这种投屏形状：{}'.format(key))
        if key == dlna_shape():
            return {'code': 0,
                    'message': '投屏形状已经是{}'.format(DLNA_SHAPES[key][0])}
        Setting.set(SettingProperty.Mirror_Dlna_Shape, key)
        return self._ok('DLNA 形状：{}'.format(DLNA_SHAPES[key][0]),
                        restart=True)

    def _do_set_dlna_prefill(self, args):
        """How many seconds of picture the endless-file shape buffers first.

        Restarts, because the budget is turned into a byte count when the
        session is built and a running television is already reading from the
        head of the buffer it was given -- changing the number underneath it
        would move the offsets every absolute read is keyed on.

        Validated against the same tuple the state reports its pills from, not
        against the clamp: the clamp exists so a hand-edited settings file
        cannot produce a television that will not play, and accepting arbitrary
        numbers here would make the page the only thing enforcing the list.
        """
        raw = str(args.get('value') or '')
        try:
            seconds = int(raw)
        except ValueError:
            return self._no('预填秒数得是个整数：{}'.format(raw))
        if seconds not in DLNA_PREFILL_OPTIONS:
            return self._no('只能选这几档：{}'.format(
                '、'.join(str(one) for one in DLNA_PREFILL_OPTIONS)))
        Setting.set(SettingProperty.Mirror_Dlna_Prefill, seconds)
        return self._ok('DLNA 预填：{} 秒（这就是看得见的延迟）'.format(seconds),
                        restart=True)

    def _do_set_live_edge(self, args):
        """How far behind the live edge the browser player parks itself.

        Deliberately the one knob here that does *not* restart: the value is
        injected into the viewing page when that page is served, so the running
        session is untouched and a reload of the viewing page picks it up.
        Restarting a capture to change a number the player reads from its own
        HTML would be the expensive way to do nothing.
        """
        raw = str(args.get('value') or '')
        try:
            seconds = float(raw)
        except ValueError:
            return self._no('播放页落后上限得是个数字：{}'.format(raw))
        if seconds not in LIVE_EDGE_OPTIONS:
            return self._no('只能选这几档：{}'.format(
                '、'.join('%g' % one for one in LIVE_EDGE_OPTIONS)))
        Setting.set(SettingProperty.Mirror_Live_Edge, seconds)
        return self._ok('浏览器播放页停在直播边缘后 {} 秒'
                        '（刷新观看页生效，不用重启镜像）'.format('%g' % seconds))

    def _do_set_quality(self, args):
        key = str(args.get('value') or '')
        if key not in QUALITIES:
            return self._no('没有这个画质档位：{}'.format(key))
        Setting.set(SettingProperty.Mirror_Quality, key)
        return self._ok('画质：{}'.format(QUALITY_LABELS[key]), restart=True)

    def _do_set_max_duration(self, args):
        """How many hours one television session may run before it stops itself.

        Restarts, because the ceiling is baked into two things a running session
        cannot change underneath itself: the encoder's `-t`, and the length the
        file shape already advertised (a television that believes in 12 hours has
        already been promised that many bytes -- shortening the session would leave
        it reading toward an end that is no longer there).

        Validated against the options tuple rather than the clamp, exactly like
        the prefill knob above: the clamp exists so a hand-edited settings file
        cannot produce a session that stops for a reason nobody can name, and
        accepting arbitrary numbers here would make the page the only thing
        enforcing the list.
        """
        raw = str(args.get('value') or '')
        try:
            hours = int(raw)
        except ValueError:
            return self._no('最大时长得是几小时这样的整数：{}'.format(raw))
        if hours not in MAX_DURATION_HOURS:
            return self._no('只能选这几档：{}'.format(
                '、'.join('{} 小时'.format(one)
                         for one in MAX_DURATION_HOURS)))
        Setting.set(SettingProperty.Mirror_Max_Duration, hours)
        return self._ok('投屏最大时长：{} 小时'.format(hours), restart=True)

    def _do_set_screen(self, args):
        index = str(args.get('value') or '')
        if index == '':
            Setting.unset(SettingProperty.Mirror_Screen)
            label = '第一块屏幕（默认）'
        else:
            if not index.isdigit():
                return self._no('屏幕编号只能是数字：{}'.format(index))
            Setting.set(SettingProperty.Mirror_Screen, index)
            label = '第 {} 块采集设备'.format(index)
        # The choice is baked into the probe result, not read per frame.
        invalidate_capture_cache()
        return self._ok('采集屏幕：{}'.format(label), restart=True)

    def _do_set_cursor(self, args):
        want = bool(args.get('value'))
        Setting.set(SettingProperty.Mirror_Cursor, want)
        invalidate_capture_cache()
        return self._ok('鼠标指针：{}'.format('显示' if want else '不显示'),
                        restart=True)

    def _do_set_encoder(self, args):
        """Only stores the wish; a mirror whose probe says the encoder is not
        there falls back to libx264 with a warning naming it, which is better
        than refusing here (the probe may not have run yet)."""
        want = str(args.get('value') or '')
        if want not in ('software', 'hardware'):
            return self._no('没有这种编码器：{}'.format(want))
        name = hardware_encoder()
        if want == 'hardware' and not name:
            return self._no('这台机器没有可命名的硬件编码器（macOS 用 VideoToolbox、'
                            'Windows 用 NVENC），只能用软件编码')
        Setting.set(SettingProperty.Mirror_Encoder, want)
        return self._ok('编码器：{}'.format(encoder_note(want)), restart=True)

    def _do_refresh(self, args):
        """Re-run every protocol's search, not just the chosen target's.

        The window asks「哪种投屏方式有设备」, so an answer for one protocol is
        what made a rerun look like it had done nothing.
        """
        started = [name for name, kicked in
                   (('Chromecast', start_search()),
                    ('DLNA 电视', start_renderer_search())) if kicked]
        if len(started) == 2:
            return {'code': 0, 'message': '正在搜索 Chromecast 与 DLNA 电视…'}
        if started:
            return {'code': 0, 'message': '正在搜索 {}…（另一种已在进行中）'
                    .format(started[0])}
        return {'code': 0, 'message': '两种搜索都已在进行中'}

    def _do_probe(self, args):
        if not request_capture_probe():
            return {'code': 0, 'message': '采集探测已在进行中'}
        return {'code': 0, 'message': '正在重新探测采集设备与编码器…'}

    def _do_audio_setup(self, args):
        if sys.platform != 'darwin':
            return self._no('系统声音辅助安装只有 macOS 有')
        if _audio_setup_busy.is_set():
            return self._ok('系统声音设置已在进行中')
        _audio_setup_busy.set()
        threading.Thread(target=_audio_setup_worker, daemon=True,
                         name="SCREEN_MIRROR_AUDIO_SETUP").start()
        return {'code': 0, 'message': '开始一键设置系统声音…'}

    def _do_audio_restore(self, args):
        restore_system_audio(lambda message: notify(message, sound=False))
        return {'code': 0, 'message': '正在恢复原声音输出'}

    # -- the optional WebRTC bundle -------------------------------------------

    def extras_install(self):
        """POST `install-webrtc-extras`: start the download and answer at once.

        Not a `CONSOLE_ACTIONS` member on purpose -- that surface is reached by
        `mirror-action`, whose gate is the weaker page token, and this lands
        importable code (§4.7b: 落代码的只认令牌).
        """
        ok, message = start_extras_install()
        return {'code': 0 if ok else 1, 'message': message}

    def extras_uninstall(self):
        """POST `uninstall-webrtc-extras`: move our tree to `.trash`, on the card.

        Goes through the step machine rather than calling `uninstall_webrtc_extras`
        directly, so the answer stays on the page after the toast is gone and so a
        removal cannot slip in beside an in-flight land step.
        """
        ok, message = start_extras_uninstall()
        return {'code': 0 if ok else 1, 'message': message}

    # -- menu-era callbacks kept for the two rows the menu still has ------------

    def on_toggle_clicked(self, item):
        renderer = self._renderer()
        if renderer is None:
            notify('Screen Mirror 未选中为当前渲染器')
            return
        if renderer.is_mirroring():
            renderer.stop_mirror()
            notify('已停止镜像')
        else:
            renderer.start_mirror()

    def _restart(self):
        renderer = self._renderer()
        if renderer is not None and renderer.is_mirroring():
            renderer.stop_mirror()
            renderer.start_mirror()

    @staticmethod
    def _status_line(renderer):
        """Live throughput, so 'it is fine, it is just 200 kbps' is visible.

        The one thing throughput cannot tell is whether the sound arrived: that
        is the session's own answer, so it rides on the same line.
        """
        stats = renderer.stats()
        if not stats:
            return '状态：正在启动…'
        minutes, seconds = divmod(stats['seconds'], 60)
        if stats.get('kind') == 'dlna':
            # A renderer reports STOPPED for a moment while its player opens the
            # stream, and its own word alone then reads as「什么都没发生」while
            # the picture is on screen. Bytes being consumed say otherwise, so
            # the line carries both facts instead of picking one.
            shown = stats.get('state') or '未上报'
            if stats.get('reading') and shown != 'PLAYING':
                shown += '（客户端在读取）'
            line = '已镜像 {:d}:{:02d} · 档位 {} · 电视 {} · 缓冲 {:.0f} MiB'.format(
                minutes, seconds, stats.get('profile', '?'), shown,
                max(0, stats.get('buffered', 0)) / 1048576.0)
        elif stats.get('kind') == 'caststream':
            #: Frames in the receiver's own words: 在途 is how many pictures it
            #: has not acknowledged, and 丢帧 counts the ones we shed because
            #: that number hit its ceiling.
            line = '已镜像 {:d}:{:02d} · {:.1f} Mbps · {:d} 帧 · 在途 {:d} · 丢帧 {:d}'.format(
                minutes, seconds, stats['mbps'], stats['frames'],
                stats['in_flight'], stats['drops'])
        else:
            line = '已镜像 {:d}:{:02d} · {:.1f} Mbps · {:d} 个观看端 · 丢块 {:d}'.format(
                minutes, seconds, stats['mbps'], stats['clients'], stats['drops'])
        if renderer.audio_dropped():
            line += ' ' + audio_dropped_mark(method=renderer.capture_method())
        return line

    @staticmethod
    def _quality_note():
        """The low-latency channel's ceiling, and the reason it sits there.

        The reason is not decoration: it used to say「软件加密跟不上」
        unconditionally, which stopped being true the moment the keystream came
        from the operating system. A note that names a cause the code no longer
        has is worse than no note, because the user goes looking for the
        encryption setting.
        """
        if output_kind() != 'caststream':
            return None
        width, height, _filter = cast_stream_shape(quality_preset()[0])
        cap = cast_stream_bitrate_cap()
        why = ('纯 Python 加密跟不上，已自动降级' if _NATIVE_AES is None
               else '加密走系统原生 {}，上限是我们选的'.format(native_aes_name()))
        return ('低延迟通道上限 {:.1f} Mbps（{}）· '
                '帧尺寸固定 {}x{}（信箱化）'.format(
                    cap / 1000000.0, why, width, height))

    @staticmethod
    def _audio_line(renderer=None, platform=None):
        """What the last probe decided about system audio (never probes:
        the console polls this view while it is open).

        `renderer` is the live session, and it outranks the probe: a tap can be
        configured, probed, and still unreadable -- in which case this session
        has no sound in it, whatever the machine is set up to deliver.

        `platform` is a test seam; production means sys.platform, because the
        two platforms lose the tap for entirely different reasons and a sentence
        that names the wrong one is worse than no sentence at all.
        """
        platform = platform or sys.platform
        capture = next(iter(_capture_cache.values()), None)
        if capture is None:
            return '系统声音：开始镜像后这里会显示'
        if capture.audio_map:
            line = '系统声音：已启用 · {}'.format(capture.label)
            if renderer is not None and renderer.audio_dropped():
                if capture.method == 'sck':
                    # A third failure, and the two platform sentences are
                    # both wrong doors for it: there is no microphone grant
                    # behind an SCK tap and no BlackHole to install under
                    # one. The latch holds for the whole run, so the only
                    # retry that actually re-asks is a Macast restart.
                    return ('系统声音：这一次镜像没有声音 —— ScreenCaptureKit'
                            ' 一直没有送来系统音频，所以只采了画面。本次运行里'
                            '之后的镜像也会先跳过声音；要再试一次，请重启 Macast')
                if platform == 'win32':
                    return ('系统声音：这一次镜像没有声音 —— 回环录音设备没能'
                            '及时出帧，所以只采了画面。要声音请确认「立体声混音」'
                            '已启用、且没有被别的程序独占，点上面的'
                            '「重新探测采集」，再把镜像重启一次')
                return ('系统声音：这一次镜像没有声音 —— 那个采集口打不开，'
                        '连画面也不给，所以只采了画面。要声音请{}，勾选后重启 '
                        'Macast').format(MICROPHONE_DOOR)
            if capture.method != 'sck' and system_audio_routed() is False:
                # "已启用" is a statement about the tap, not about the sound.
                # Saying only that is what made a silent mirror look like an
                # encoder bug: the machine's output was never pointed at the
                # device being captured. SCK is exempt: its tap reads the
                # system mix directly, so which device the default output is
                # cannot make this capture silent, and the BlackHole-routing
                # sentence would name a device this capture never touches.
                line += ('；但系统声音没有路由到它（默认输出不是「{}」），'
                         '镜像里会是静音。点「一键设置」切换，或在「音频 MIDI '
                         '设置」里做一个包含它的多输出设备').format(
                    MACAST_AGGREGATE_NAME)
            return line
        if capture.method == 'sck':
            # audio_map is None under SCK only when this run already gave the
            # tap up (`_sck_audio_absent` latches, and every later probe has
            # it stripped by `video_only_capture`). The BlackHole sentence
            # would send the user to install a driver that cannot feed an SCK
            # capture; the latch, not the system, is what has to change.
            return ('系统声音：未启用 —— 本次运行里 ScreenCaptureKit 没能送出'
                    '系统音频，之后的镜像先只采画面。要再试一次系统声音，'
                    '请重启 Macast')
        if platform == 'darwin':
            return '系统声音：未启用（可一键安装 BlackHole）'
        if platform == 'win32':
            # Windows 的回环录音设备不是我们装的：要么驱动自带「立体声混音」，
            # 要么用户自己装了虚拟声卡。所以这里必须给出去哪打开它，而不是
            # 只说一句「仅画面」—— 那正是「Windows 投屏没声音」的原始答案。
            return ('系统声音：未启用 —— Windows 要把输出录回来需要一个回环录音'
                    '设备。在「设置 → 系统 → 声音 → 更多声音设置 → 录制」里右键'
                    '空白处勾选「显示已禁用的设备」，启用「立体声混音 (Stereo '
                    'Mix)」；驱动没提供就装一块虚拟声卡（VB-Cable / VoiceMeeter），'
                    '装好后点上面的「重新探测采集」，本次镜像要重启一次。')
        return PULSE_DOOR


if __name__ == '__main__':
    gui(ScreenMirrorRenderer())
