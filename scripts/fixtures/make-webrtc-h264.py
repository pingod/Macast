#!/usr/bin/env python3
"""Re-make `webrtc-h264.bin`, the Annex-B fixture Part 56 feeds to aiortc.

Same reasoning as `make-live-mkv.py` (AGENTS.md 4.2, last bullet): the bytes
come out of the production `build_ffmpeg_command` with `kind='webrtc'`, so the
framing under test -- an access unit delimiter in front of every picture, SPS/
PPS repeated ahead of each IDR -- is ffmpeg's arrangement, not one this file
imagines. The one substitution is the source: lavfi instead of the screen.

One second at 24 fps is two GOPs (`gop_size('webrtc')` is FPS // 2 = 12): the
shortest stream where "a viewer waits for the first IDR" and "a late viewer's
queue loses the oldest half" can both be exercised without decoding for long.
No audio map -- this target's menu label says 无声音 out loud, and the argv
carries `-an` for it.

    env -u PYTHONPATH .venv/bin/python scripts/fixtures/make-webrtc-h264.py
"""
import hashlib
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, REPO)
FF = os.environ.get('FFMPEG', 'ffmpeg')
OUT = os.path.join(HERE, 'webrtc-h264.bin')

from macast.plugins.renderer import screen_mirror as m  # noqa: E402


def build():
    """The production webrtc argv, its source swapped for a test pattern."""
    capture = m._Capture('s', inputs=[
        ['-f', 'lavfi', '-i',
         'testsrc2=size=160x90:rate=%d:duration=1' % m.FPS]])
    return m.build_ffmpeg_command(FF, capture, 0, 400000,
                                  kind='webrtc', encoder='software')


cmd = build()
print(' '.join(cmd))
env = {k: v for k, v in os.environ.items()
       if k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy')}
data = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                      stdin=subprocess.DEVNULL, env=env).stdout
# `-aud 1` is in the argv, so every picture opens with an AUD and the very
# first bytes are one too -- if this fails, the fixture cannot exercise the
# splitter or the receiver at all and the file must not be committed.
if data[:5] != b'\x00\x00\x00\x01\x09':
    sys.exit('not an Annex-B stream starting with an AUD: %r' % data[:8])
open(OUT, 'wb').write(data)
print('%s  %d bytes  %d AUDs  sha256 %s' % (
    os.path.relpath(OUT, REPO), len(data),
    data.count(b'\x00\x00\x00\x01\x09'),
    hashlib.sha256(data).hexdigest()[:16]))
