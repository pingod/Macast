#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Drive the plugin's real Cast Streaming sender against a real Chromecast.

Why this exists: `scripts/verify_cast_airplay.py` Part 24 builds its far end out
of the same tables as the sender, so it can only prove the bytes are
self-consistent. AGENTS.md 4.9 is explicit that "self-consistent" and "a
television painted it" are different claims, and only hardware can settle the
second one. This script is the hardware half -- and it imports
`macast/plugins/renderer/screen_mirror.py` rather than reimplementing it, so a failure here is a
failure of the code the menu bar actually runs.

    env -u PYTHONPATH .venv/bin/python scripts/cast_streaming_probe.py 192.168.1.30
    ... --height 1080 --seconds 30          # a bigger frame, longer look
    ... --encoder hardware                   # HW h264, same sender
    ... --live                               # mirror the real desktop, not a test pattern
    ... --source /tmp/clip.mp4               # a known file instead of the pattern
    ... --dump /tmp/stream.h264              # keep the Annex-B for ffprobe / x264

What each step can tell you:
  LAUNCH_ERROR      -> this firmware has no mirroring app; the plugin would LOAD instead.
  no ANSWER         -> the OFFER was refused (bad key length, unsupported size).
  ANSWER, no ack     -> RTP reached nothing, or the receiver could not parse the header.
  checkpoints, no picture -> framing survived, the decoder did not. Check --dump.
"""

import argparse
import importlib.util
import os
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLUGIN = os.path.join(REPO, 'macast', 'plugins', 'renderer', 'screen_mirror.py')
# The plugin imports `macast`, so the checkout has to be on the path the way
# `scripts/run-from-source.sh` puts it there.
sys.path.insert(0, REPO)

EVENT_TALLY = {}


def load_plugin():
    spec = importlib.util.spec_from_file_location('screen_mirror_probe', PLUGIN)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def instrument(sm):
    """Log the three places a receiver talks back, without changing any of them."""
    offer = sm.build_offer

    def build_offer(*a, **k):
        data = offer(*a, **k)
        stream = data['offer']['supportedStreams'][0]
        print('  -> OFFER  {}x{} @ {} fps, {} bps, pt {}, ssrc {:08x}'.format(
            stream['resolutions'][0]['width'],
            stream['resolutions'][0]['height'],
            stream['maxFrameRate'], stream['maxBitRate'],
            stream['rtpPayloadType'], stream['ssrc']))
        return data

    parse_answer = sm.parse_answer

    def parsed_answer(data):
        port, ssrc = parse_answer(data)
        print('  <- ANSWER udp {} -> ssrc {:08x} (result {})'.format(
            port, ssrc, data.get('result', 'ok')))
        return port, ssrc

    parse_rtcp = sm.parse_rtcp

    def parsed_rtcp(data, media_ssrc):
        events = parse_rtcp(data, media_ssrc)
        for event in events:
            EVENT_TALLY[event[0]] = EVENT_TALLY.get(event[0], 0) + 1
        return events

    sm.build_offer = build_offer
    sm.parse_answer = parsed_answer
    sm.parse_rtcp = parsed_rtcp


def make_capture(sm, ffmpeg, args):
    """Real desktop, a file, or the pattern -- always paced with -re, because a
    sender that outruns its own clock is a different test than the one asked for."""
    if args.live:
        capture = sm.probe_capture(ffmpeg)
        if capture is None:
            raise SystemExit('探测不到可采集的屏幕（Wayland 或权限被拒）')
        capture.inputs = [['-re'] + one for one in capture.inputs]
        return capture, '桌面 ' + capture.label
    if args.source:
        return (sm._Capture('file', [['-re', '-i', args.source]]),
                '文件 ' + os.path.basename(args.source))
    width, height, _filter = _shape(sm, args.height)
    return (sm._Capture('pattern',
                        [['-re', '-f', 'lavfi', '-i',
                          'testsrc2=size={}x{}:rate={}'.format(
                              width, height, sm.FPS)]]),
            '测试图案 {}x{}'.format(width, height))


def _shape(sm, height):
    """The frame this target pins, from the same table the plugin uses."""
    return sm.cast_stream_shape(height)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('host', help='电视的 IP（8009 端口）')
    ap.add_argument('--port', type=int, default=8009)
    ap.add_argument('--seconds', type=float, default=20.0)
    ap.add_argument('--height', type=int, default=720,
                    choices=[360, 720, 1080],
                    help='镜像目标会钉死的帧高（发送端不允许随手变）')
    ap.add_argument('--bitrate', type=int, default=None,
                    help='默认取纯 Python 加密扛得住的上限')
    ap.add_argument('--encoder', default='software',
                    choices=['software', 'hardware'])
    ap.add_argument('--live', action='store_true',
                    help='采集真实桌面而不是测试图案')
    ap.add_argument('--source', help='用这个文件代替测试图案')
    ap.add_argument('--dump', help='把 Annex-B 字节流写到这个文件')
    ap.add_argument('--timeout', type=float, default=12.0,
                    help='等设备应答握手的最长时间')
    args = ap.parse_args()

    sm = load_plugin()
    instrument(sm)
    ffmpeg = sm.find_ffmpeg()
    if not ffmpeg:
        raise SystemExit('找不到 ffmpeg（插件用同一套定位逻辑，见 --help）')

    width, height, _filter = _shape(sm, args.height)
    bitrate = min(args.bitrate or sm.CAST_STREAM_MAX_BITRATE,
                  sm.CAST_STREAM_MAX_BITRATE)
    capture, label = make_capture(sm, ffmpeg, args)
    cmd = sm.build_ffmpeg_command(ffmpeg, capture, height, bitrate,
                                  kind='caststream', encoder=args.encoder)

    print('== 1. 握手 {}:{}（内容：{}）=='.format(args.host, args.port, label))
    try:
        sender = sm._CastStreamSender.open(
            args.host, args.port, width, height, sm.FPS, bitrate,
            timeout=args.timeout,
            on_lost=lambda why: print('  !! 链路断了:', why))
    except Exception as e:
        print('  !! 失败:', e)
        if 'LAUNCH' in str(e) or '镜像' in str(e) or 'refuse' in str(e).lower():
            print('  这通常意味着这台设备没有镜像 app —— 插件会自动回落到 '
                  'LOAD/mpegts 通道，用 cast_probe.py 可以单独验证那条路。')
        else:
            print('  先确认 IP、8009 端口和同一局域网（dns-sd -B _googlecast._tcp）')
        return 1
    print('  TLS 控制通道、镜像 app、OFFER/ANSWER 全部通过')

    print('== 2. 编码并推送 {:.0f} 秒 =='.format(args.seconds))
    sender.start()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL,
                            stdin=subprocess.DEVNULL)
    dump = open(args.dump, 'wb') if args.dump else None
    started = time.time()
    last = (0, 0, time.time())
    interrupted = False
    try:
        while time.time() - started < args.seconds:
            chunk = proc.stdout.read(65536)
            if not chunk:
                print('  编码器结束了（测试图案已播完或进程退出）')
                break
            if dump:
                dump.write(chunk)
            sender.feed(chunk)
            now = time.time()
            if now - last[2] >= 2.0:
                print('     {:.0f}s  帧 {}  在途 {}  丢 {}  RTCP {}'.format(
                    now - started, sender.frames, sender.in_flight(),
                    sender.drops, EVENT_TALLY or '尚无'))
                last = (sender.frames, sender.drops, now)
    except KeyboardInterrupt:
        interrupted = True
    finally:
        if dump:
            dump.close()
        proc.terminate()
        try:
            sender.stop()
        except Exception as e:
            print('  退出镜像 app 时出错:', e)
        sender.close()

    print('== 3. 结论 ==')
    stats = [
        ('帧 / 包', '{} / {}'.format(sender.frames, sender.packets)),
        ('丢帧（窗口满或等关键帧）', sender.drops),
        ('编码器输出的字节', sender.bytes),
        ('最新已确认的帧', sender.acked),
        ('电视报告的播放延迟 ms', sender.playout_delay),
        ('收到的 RTCP 事件', EVENT_TALLY or '一条都没有'),
    ]
    for key, value in stats:
        print('  {:<28} {}'.format(key, value))
    if args.dump:
        print('  Annex-B 已写到 {}，可用 ffprobe 复核分片与 GOP'.format(args.dump))
    if interrupted:
        print('  （你按了 Ctrl-C：以上是被打断时的状态）')
    print('  以上都是机器能自证的部分。剩下唯一的问题是：**电视上出现画面了吗？'
          '延迟大概多少？** 只有人能回答。')
    return 0


if __name__ == '__main__':
    sys.exit(main())
