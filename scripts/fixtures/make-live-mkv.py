#!/usr/bin/env python3
"""Re-make `live-mkv-h264.bin`, the container fixture the regression suite reads.

Why this exists instead of a hand-written byte string: the suite has already
been bitten once by a fake whose shape came from an imagination (AGENTS.md 4.2,
last bullet -- a parser and its tests copied the same format that never
existed, and real machines failed while the suite was green). The bytes this
fixture is made of come out of the same `build_dlna_command` a live session
runs, so the framing under test is ffmpeg's, not mine.

Two profile fields are edited -- frame height and bitrate -- because a 720p
6 Mbps second is 750 KB of test fixture and a 240p 0.3 Mbps second is not.
Nothing else changes: the muxer args, the encoder args, the audio args and the
pipe are the production ones, and the element tree is what the framer must
handle.

    env -u PYTHONPATH .venv/bin/python scripts/fixtures/make-live-mkv.py
"""
import hashlib
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, REPO)
FF = os.environ.get('FFMPEG', 'ffmpeg')
OUT = os.path.join(HERE, 'live-mkv-h264.bin')

from macast.plugins.renderer import screen_mirror as m  # noqa: E402


def build():
    """The profile as production defines it, with a frame small enough to commit."""
    src = m.DLNA_PROFILES['mkv-h264']
    profile = m._DlnaProfile(src.label, list(src.muxer), src.suffix,
                             src.content_type, src.org_pn, src.video_args,
                             list(src.audio_args), 0, 240, src.fps, 300000,
                             src.audio_bitrate)
    capture = m._Capture('s', inputs=[
        ['-f', 'lavfi', '-i',
         'testsrc2=size=320x240:rate=%d:duration=2' % profile.fps],
        ['-f', 'lavfi', '-i',
         'sine=frequency=440:sample_rate=48000:duration=2']], audio_map='1:a:0')
    return m.build_dlna_command(FF, capture, profile, encoder='software')


cmd = build()
print(' '.join(cmd))
env = {k: v for k, v in os.environ.items()
       if k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy')}
data = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                      stdin=subprocess.DEVNULL, env=env).stdout
if data[:4] != b'\x1a\x45\xdf\xa3':
    sys.exit('not an EBML stream: %r' % data[:8])
open(OUT, 'wb').write(data)
print('%s  %d bytes  sha256 %s' % (os.path.relpath(OUT, REPO), len(data),
                                   hashlib.sha256(data).hexdigest()[:16]))
