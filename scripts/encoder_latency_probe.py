#!/usr/bin/env python3
"""Measure what an encoder change actually buys, on this machine, right now.

`screen_mirror`'s latency budget is a pile of numbers that were each measured
once and then quoted in prose for releases afterwards. This probe re-measures
the ones a change is about to invalidate, so the answer to "does HEVC lower the
delay" and "does mpdecimate save anything on a static desktop" is a transcript
instead of an argument.

It captures the *real* desktop through the same avfoundation device the plugin
would pick, so screen-recording permission for the calling shell is a
prerequisite (the plugin's own permission is a different identity -- see
AGENTS.md §8). Numbers it reports:

  first byte   ms from exec to the first byte of encoded output. This is the
               term a viewer waits for before anything at all appears, and the
               one VideoToolbox is famously bad at. **It includes ffmpeg's own
               startup (~0.5 s here), which every variant pays**, so the number
               to read is the *difference* between rows, not any single row.
               Measured 2026-10-02: x264-zl 528.9 ms, h264_videotoolbox
               753.4 ms -- the ~225 ms gap is the finding, the 528.9 is not.
  frames       frames the encoder emitted, from ffmpeg's own final `frame=`.
  bytes        everything that came out of `pipe:1`.
  cpu          child user+system seconds, from RUSAGE_CHILDREN. The honest cost
               on a laptop: this is the fan and the battery.
  cores        cpu / wall, i.e. the column above expressed as "how many cores
               this kept busy". Added because the raw seconds got misread as
               cores once already: `cpu 4.58` next to an 8.6 s run is 0.53
               cores, not 4.5 of them.
               **Read this as the cost of the live path, and never compare it
               with an offline throughput number.** Two different "4.5 cores"
               readings exist in this repo's history and they measure different
               things. This probe, encoding a real desktop at the 24 fps the
               capture delivers, measures **0.59 cores for x264
               ultrafast+zerolatency against 0.41 for h264_videotoolbox** at
               1080p (2026-10-02, three runs each). The same encoder handed a
               *file* and told to go as fast as it can measures **4.20 cores at
               1920x1080, 4.63 at 2560x1600, 4.94 at 3456x2234** -- because it
               is then encoding 486 fps, not 24. Both are true; only the first
               is what mirroring costs. Quoting the second as the first is how
               "x264 吃约 4.5 核 / VideoToolbox 约 0.2 核" reached AGENTS.md and
               made the hardware default look twenty times cheaper than it is:
               on the live path the difference is 0.18 of a core, and what it
               buys is the ~200 ms of first-byte latency it does *not* pay.
  realtime     frames / (fps x wall). **Below 1.0 means it could not keep up;
               above ~1.0 means the measurement is broken, not that there is
               headroom.** The source is a live 24 fps capture, so an encoder
               cannot emit more than it is given and this column is capped at
               1.0 by construction -- a healthy row reads ~0.93, the shortfall
               being ffmpeg startup inside `wall`. It answers "did it keep up",
               never "how much room is left"; headroom needs a file source, and
               §2.3 of docs/research-screen-mirroring-encoder-2026-10.md is
               exactly that measurement.
               **Do not read that section's 17.9x as a violation of this cap.**
               The two look alike and are not the same claim: §2.3's 17.9x is
               genuine offline throughput -- re-measured 2026-10-02 as 486 fps
               (16.2x) on a `testsrc2` file source with `-f null`, alongside
               295 fps at 2560x1600 and 171 fps at 3456x2234, matching that
               table's 536.9 / 315.9 / 185.3 row for row -- whereas a reading
               above ~1.0 *from this probe* is frame duplication caused by a
               missing output `-r` (see `insane`). For one revision this
               docstring asserted that the 17.9x was that duplication bug. It
               was not, and what settled it was running the offline shape and
               watching it reproduce, not re-reading either document.

Usage:
    scripts/encoder_latency_probe.py [--seconds 8] [--variant all]
    scripts/encoder_latency_probe.py --list

Read-only: it writes nothing outside the process pipes, never touches
`SETTING_DIR`, and never imports `macast`.
"""

import argparse
import json
import os
import re
import resource
import subprocess
import sys
import tempfile
import threading
import time

FFMPEG_CANDIDATES = [
    os.environ.get('MACAST_FFMPEG'),
    'ffmpeg',
    '/opt/homebrew/opt/ffmpeg/bin/ffmpeg',
    '/usr/local/bin/ffmpeg',
]

#: Capture side only. Which screen index to use is discovered, never assumed:
#: `-list_devices` numbering moves when a virtual camera is installed, and the
#: plugin's own bug history (AGENTS.md §4.2) is exactly a parser that guessed.
CAPTURE = ['-f', 'avfoundation', '-framerate', '24', '-i', '{device}:none']

FPS = 24
#: Encoded height. The capture is whatever the display is (3456x2234 on the
#: machine this was written on) and `scale=-2:HEIGHT` brings it down, so this
#: one number decides how much of the work is the scaler and how much is the
#: encoder -- which is exactly the question that decides whether VideoToolbox's
#: ~210 ms of extra first-byte latency is worth what it saves in CPU. At 1080 it
#: is not much: x264 ultrafast costs 0.59 cores against VideoToolbox's 0.41
#: (measured 2026-10-02, three runs). `--height` exists so that claim can be
#: re-measured at 2160 instead of being argued about.
HEIGHT = 1080

#: The variants worth comparing. Each is (name, encoder argv, extra argv,
#: filter suffix). `filter` is applied after the scale, so mpdecimate sees the
#: same pixels the encoder would.
VARIANTS = [
    ('x264-ultrafast-zerolatency',
     ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
      '-profile:v', 'high', '-flags', '+low_delay', '-thread_type', 'slice'],
     [], ''),
    ('h264_videotoolbox',
     ['-c:v', 'h264_videotoolbox', '-profile:v', 'high', '-level', '42',
      '-flags', '+low_delay', '-realtime', '1'],
     [], ''),
    ('hevc_videotoolbox',
     ['-c:v', 'hevc_videotoolbox', '-profile:v', 'main',
      '-flags', '+low_delay', '-realtime', '1'],
     [], ''),
    ('libx265-ultrafast',
     ['-c:v', 'libx265', '-preset', 'ultrafast', '-tune', 'zerolatency'],
     [], ''),
    #: mpdecimate on top of the software path. `-tune zerolatency` pins
    #: `b_vfr_input = 0`, i.e. x264 is told the input is constant-rate while
    #: `-fps_mode vfr` makes it not, so the pair is measured as its own variant
    #: rather than assumed harmless.
    #:
    #: `fps=24` is not decoration. Without it x264 derives the frame rate from
    #: the filter's timebase, which under VFR is 1/1000000, and refuses to run:
    #: `MB rate (8160000000) > level limit (16711680)` -- 8160000000 is 8160
    #: macroblocks per 1080p frame times a million. Measured, not guessed.
    ('x264-zl + mpdecimate/vfr',
     ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
      '-profile:v', 'high', '-flags', '+low_delay', '-thread_type', 'slice'],
     ['-fps_mode', 'vfr', '-x264-params', 'fps=24'], ',mpdecimate'),
    ('x264 (no zl) + mpdecimate/vfr',
     ['-c:v', 'libx264', '-preset', 'ultrafast',
      '-x264-params', 'fps=24:b_vfr_input=1:sliced_threads=0:rc_lookahead=0:'
                      'sync_lookahead=0:bframes=0'],
     ['-fps_mode', 'vfr'], ',mpdecimate'),
    ('h264_videotoolbox + mpdecimate/vfr',
     ['-c:v', 'h264_videotoolbox', '-profile:v', 'high', '-level', '42',
      '-flags', '+low_delay', '-realtime', '1'],
     ['-fps_mode', 'vfr'], ',mpdecimate'),
]

BITRATE = 6_000_000

#: How long the capture preflight runs, per variant. Two seconds is enough for
#: avfoundation to settle and short enough that seven of them cost less than one
#: variant does.
PREFLIGHT_SECONDS = 2.0


class _AwakeDisplay(object):
    """Hold the display awake for as long as this probe measures.

    The probe knows it needs an awake display, so it holds one instead of asking
    the user to wave at the machine every ninety seconds -- the same reason
    `screen_mirror` holds `caffeinate -dimsu` for the length of a mirror.
    `caffeinate -d` cannot *wake* a display that is already asleep, which is why
    the preflight below still runs per variant: this reduces the chance of a
    sleeping capture, it does not make the numbers trustworthy on its own.

    No-op anywhere but Darwin, and a no-op there too if `caffeinate` is missing
    (it is part of the base system, but a container or a stripped install may
    not have it) -- a probe that refused to run without it would be worse than
    one that runs and says so.
    """

    def __init__(self):
        self._proc = None
        self.held = False

    def __enter__(self):
        if sys.platform != 'darwin':
            return self
        try:
            #: `-w` on our own pid would be tidier, but a plain child we kill in
            #: `__exit__` is the shape `screen_mirror` already uses and the one
            #: that cannot outlive us if this process is SIGKILLed mid-run.
            self._proc = subprocess.Popen(
                ['caffeinate', '-dims'],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self.held = True
        except OSError:
            self._proc = None
        return self

    def __exit__(self, *_exc):
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.SubprocessError:
                self._proc.kill()
        return False


def capture_rate(ffmpeg, device, seconds=PREFLIGHT_SECONDS):
    """Frames per second the *capture* delivers, with no encoder in the way.

    `-f null -` makes ffmpeg's output frame count equal its input frame count,
    so this reads avfoundation and nothing else. It has to be a separate
    measurement because the per-variant gate cannot do this job: that one
    compares *encoded* frames against the requested rate, and `mpdecimate`
    exists to make that number smaller. Whenever the capture delivers frame
    after identical frame, mpdecimate drops nearly all of them and reports a
    plausible-looking single-digit rate -- so a gate reading the encoder's
    output is blind exactly where it is needed.

    Measured, not reasoned about (2026-10-02): one run reported 350 frames/s for
    `x264-ultrafast-zerolatency` -- caught -- and 58 frames in 8 s for
    `x264 + mpdecimate/vfr`, which sailed straight through and printed a table of
    numbers that looked like a result. **This function's first revision blamed
    that pair on a sleeping display, and that was wrong**: the display was awake
    (`-f null -` measured a clean 24 fps, `time=` advancing, seven times in a
    row), and the real cause was the encode argv omitting an output `-r` against
    a raw muxer, which duplicates frames ~18x -- see the corrected note on
    `insane`. The mpdecimate half of the story survives the correction and is
    the reason this preflight stays: whatever makes the capture degenerate,
    mpdecimate will hide it from an encoder-side gate. This is the same shape as
    the fake ffmpeg that was written against an invented device listing
    (AGENTS.md §4.2) -- a check that shares its subject's assumption checks
    nothing.

    Returns (frames_per_second, frames, wall_seconds). The rate is diluted by
    ffmpeg's own startup (~0.6 s on this machine, so a healthy 24 fps capture
    reads ~19), which is the safe direction for a gate that refuses to run.
    """
    cmd = ([ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin',
            '-stats', '-stats_period', '1']
           + [part.replace('{device}', device) for part in CAPTURE]
           + ['-map', '0:v:0', '-t', '{:.2f}'.format(seconds),
              '-f', 'null', '-'])
    started = time.monotonic()
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True,
                              timeout=seconds + 25)
    except (OSError, subprocess.SubprocessError):
        return 0.0, 0, 0.0
    wall = time.monotonic() - started
    match = None
    for match in re.finditer(r'frame=\s*(\d+)', proc.stderr):
        pass
    frames = int(match.group(1)) if match else 0
    return (frames / wall if wall > 0 else 0.0), frames, wall


def find_ffmpeg():
    for candidate in FFMPEG_CANDIDATES:
        if not candidate:
            continue
        try:
            out = subprocess.run([candidate, '-version'], capture_output=True,
                                 timeout=20)
        except (OSError, subprocess.SubprocessError):
            continue
        if out.returncode == 0:
            return candidate
    return None


def encoders(ffmpeg):
    out = subprocess.run([ffmpeg, '-hide_banner', '-encoders'],
                         capture_output=True, text=True, timeout=30)
    return set(re.findall(r'^\s*V[.\w]*\s+(\S+)', out.stdout, re.M))


def find_screen_device(ffmpeg):
    """The index of the first real screen, from ffmpeg's own listing.

    The listing is `AVFoundation video devices:` followed by `[N] name` with no
    quoting -- a virtual camera (OBS, JustStream) sorts first and is *not* the
    desktop, so the name has to say screen/Display/显示器.
    """
    out = subprocess.run([ffmpeg, '-hide_banner', '-f', 'avfoundation',
                          '-list_devices', 'true', '-i', ''],
                         capture_output=True, text=True, timeout=30)
    section = None
    for line in out.stderr.splitlines():
        if 'AVFoundation video devices:' in line:
            section = 'video'
            continue
        if 'AVFoundation audio devices:' in line:
            section = None
            continue
        if section != 'video':
            continue
        match = re.search(r'\[(\d+)]\s*(.+?)\s*$', line)
        if not match:
            continue
        index, name = match.group(1), match.group(2)
        if re.search(r'screen|display|显示器|屏幕', name, re.I):
            return index, name
    return None, None


def run_one(ffmpeg, device, name, enc, extra, filt, seconds, height=HEIGHT):
    vf = 'scale=-2:{}{}'.format(height, filt)
    #: The raw muxer has to match the codec. `-f h264` in front of a HEVC
    #: elementary stream is not a warning, it is "nothing was written", and the
    #: first version of this probe reported hevc_videotoolbox and libx265 as
    #: unavailable because of its own muxer choice.
    codec = enc[enc.index('-c:v') + 1]
    muxer = 'hevc' if codec in ('hevc_videotoolbox', 'libx265') else 'h264'
    cmd = ([ffmpeg, '-hide_banner', '-loglevel', 'warning', '-nostdin',
            #: Without `-stats` the `-loglevel warning` above also suppresses
            #: ffmpeg's progress line, which is the only place the encoded
            #: frame count appears -- so the capture sanity check below had
            #: nothing to read and every run reported `frames 0`.
            '-stats', '-stats_period', '1']
           + [part.replace('{device}', device) for part in CAPTURE]
           + ['-map', '0:v:0', '-vf', vf] + enc
           + ['-pix_fmt', 'yuv420p', '-g', str(FPS), '-b:v', str(BITRATE),
              '-maxrate', str(int(BITRATE * 1.5)),
              '-bufsize', str(BITRATE // 2)]
           #: **Never leave the frame-sync mode to `auto`.** Measured on this
           #: machine (2026-10-02), same capture, four seconds each:
           #:
           #:     -f null  -                  ->  24 fps, `time=` advances
           #:     -f null  - + scale filter   ->  24 fps, `time=` advances
           #:     -f h264 <file>, no `-r`     -> 442 fps, `time=00:00:00.00`
           #:     -f h264 <file>, `-r 24`     ->  24 fps, `time=` advances
           #:
           #: The raw Annex-B muxer carries no container timebase, so `auto`
           #: resolves to something that duplicates frames without bound while
           #: every output pts stays zero. `-r 24` is what `screen_mirror.py`
           #: itself passes (`'-g', str(fps * 3 // 5), '-r', str(fps)`), so
           #: adding it here is not a workaround -- it is the probe finally
           #: running the argv it exists to make a decision about.
           #:
           #: The mpdecimate variants say `-fps_mode vfr` themselves and must
           #: keep it: `-r` would be CFR, and CFR duplicates straight back
           #: every frame mpdecimate just dropped, so the two cannot coexist.
           + extra
           + ([] if any(part == '-fps_mode' for part in extra)
              else ['-fps_mode', 'cfr', '-r', str(FPS)])
           + ['-an', '-f', muxer, 'pipe:1'])
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE)
    first = None
    total = 0
    stderr = []

    def drain_err():
        for chunk in iter(lambda: proc.stderr.read(65536), b''):
            stderr.append(chunk)

    reader = threading.Thread(target=drain_err, daemon=True)
    reader.start()
    while True:
        chunk = proc.stdout.read(65536)
        if not chunk:
            break
        if first is None:
            first = (time.monotonic() - started) * 1000.0
        total += len(chunk)
        if time.monotonic() - started > seconds:
            break
    #: **Stop the clock before asking the process to die.** This line used to
    #: sit after `wait(timeout=5)` + `reader.join(timeout=5)`, which silently
    #: added up to ten seconds of teardown to `wall` for any encoder that does
    #: not die on SIGTERM promptly -- VideoToolbox does not. Measured
    #: 2026-10-02: `frames=187, realtime=0.57x` back-solves to `wall=13.6s`
    #: against an 8 s run, so both `realtime` and `cores` were understated by
    #: ~1.6x for exactly the two variants the plugin chooses between in
    #: practice, and not for the x264 ones. A column that is wrong for one
    #: encoder and right for another is worse than a column that is missing.
    wall = time.monotonic() - started
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.SubprocessError:
        proc.kill()
        proc.wait(timeout=5)
    reader.join(timeout=5)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    text = b''.join(stderr).decode('utf-8', 'replace')
    match = None
    for match in re.finditer(r'frame=\s*(\d+)', text):
        pass
    frames = int(match.group(1)) if match else 0
    cpu = ((after.ru_utime - before.ru_utime)
           + (after.ru_stime - before.ru_stime))
    #: Only a refusal to run counts as `unsupported`. Everything else ffmpeg
    #: prints on the way to a perfectly good stream -- `VBV underflow`,
    #: `Color range not set for yuv420p`, `not enough frames to estimate rate`
    #: -- is a warning, and the first version of this probe classified those as
    #: failures and reported four healthy encoders as missing. Whether the run
    #: produced anything is a question about the output, not about the text.
    unsupported = ('Unknown encoder' in text or 'No such filter' in text
                   or 'Invalid option' in text or 'Unrecognized option' in text)
    #: A third verdict the first two miss: ffmpeg *ran*, complained, and
    #: produced nothing. Found at 2160 on 2026-10-02, where h264_videotoolbox
    #: printed `Error encoding a frame: Generic error in an external library`,
    #: exited 187 and wrote zero bytes -- and the table printed it as an
    #: ordinary row reading `frames=0 bytes=0 realtime=0.00x`, which looks like
    #: "this encoder could not keep up" rather than what it was, "this encoder
    #: refused the picture". Same shape as the `-level 42` bug this probe led
    #: to (`vt_level` in screen_mirror.py): a hardcoded level smaller than the
    #: frame, and a five-line stderr nobody read because the row still printed.
    #:
    #: The test is the output, not the exit status: a healthy run is SIGTERMed
    #: by the loop above, so `returncode` is negative for success and cannot
    #: distinguish anything. `total == 0` can.
    dead = total == 0 and not unsupported
    #: An output frame rate far above the request makes every number below it
    #: fiction, so it is refused rather than printed. The threshold is 1.5x and
    #: not 3x because this does not fail loudly: one run produced 178 frames/s
    #: under x264 and a *plausible-looking* 45 frames/s under VideoToolbox, and
    #: only the first of those would have been caught by a looser gate.
    #:
    #: **What actually caused it, corrected 2026-10-02.** This gate's first
    #: diagnosis was "the display is asleep or locked, so avfoundation emits
    #: frames as fast as it can with degenerate pts". That was wrong, and the
    #: wrong diagnosis was expensive: it sent the fix off to hold the display
    #: awake with `caffeinate` while the real cause sat in this function's own
    #: argv. The measured cause is `-fps_mode auto` against the raw Annex-B
    #: muxer, which duplicates frames without bound while output pts stay at
    #: zero (see the table on the `extra` argument above; 442 frames/s next to
    #: `time=00:00:00.00`). The numbers this gate caught -- 350, 442, and the
    #: 44.5 MB / `frame=5793` "6 second" run recorded in an earlier revision of
    #: this comment -- are all that one bug, not a sleeping screen.
    #:
    #: The gate stays, and it earns its place twice over: it is what refused to
    #: print those numbers as a result, and its refusal is what made the real
    #: cause findable. A sleeping display remains a thing that can happen (the
    #: preflight measures the capture directly, which is the check that would
    #: see it), it just was not what was happening.
    rate = frames / wall if wall > 0 else 0.0
    insane = rate > FPS * 1.5
    #: For a `dead` row the interesting line is the complaint, not whatever
    #: ffmpeg printed last on its way out, so prefer a line that says so and
    #: fall back to the tail. A run that produced output has no complaint worth
    #: surfacing and this string is never read for it.
    _lines = [line.strip()[:160] for line in text.splitlines() if line.strip()]
    _err = next((line for line in reversed(_lines)
                 if re.search(r'error|failed|invalid|unable|not supported',
                              line, re.I)), '')
    return {
        'name': name, 'first_ms': first, 'frames': frames, 'bytes': total,
        'cpu': cpu, 'wall': wall, 'unsupported': unsupported, 'dead': dead,
        'insane': insane, 'rate': rate,
        'error': _err or (_lines[-1] if _lines else ''),
    }


#: GOPs compared by `--quality`. The first is what `gop_size('browser')` ships
#: (`FPS // 2`); the rest are what `mse_latency_probe.py` found buys the
#: latency back, and the reason this mode exists is that its kbps column showed
#: those bytes going somewhere -- 3252 kbps at `-g 12` against 6733 at `-g 2`
#: under an identical `-b:v` and VBV -- and "somewhere" is either quality or
#: waste. Latency cannot tell the two apart; VMAF can.
QUALITY_GOPS = (12, 8, 4, 2)


def product_browser_tail(ffmpeg, encoder):
    """The shipping `browser` argv from `-map` onward, asked of the product.

    Delegated to `mse_latency_probe` instead of transcribed here, because that
    file's finding 8 is a list of three ways a hand-copy of this exact argv
    drifted from the real one (missing VideoToolbox, missing the VBV, missing
    `-level 42`) and a fourth copy in a fourth file is how a fourth drift
    happens. One asker, two callers.
    """
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import mse_latency_probe
    return mse_latency_probe.product_tail(
        ffmpeg, mse_latency_probe.ENCODERS[encoder])


def _run(cmd, what):
    """Run one ffmpeg, and say what it was when it fails.

    A non-zero exit here is not a measurement, and the alternative -- letting
    the caller report an empty VMAF -- reads as "the encoder produced nothing"
    when the real answer is usually a bad flag.
    """
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode:
        raise RuntimeError('{} failed (exit {}): {}'.format(
            what, proc.returncode,
            proc.stderr.strip()[-600:] or '(nothing on stderr)'))
    return proc


def vmaf_stats(path):
    """Pool a libvmaf JSON log's per-frame numbers here, in Python.

    Not by reading `pooled_metrics['vmaf']['perc_1']`: this libvmaf build puts
    only `min`/`max`/`mean`/`harmonic_mean` there and leaves
    `aggregate_metrics` as `{}`, so the percentile keys do not exist and asking
    for them is how this mode died on its first run (`KeyError: 'perc_1'`).
    Deriving p1 from `frames[].metrics.vmaf` is the same number libvmaf would
    have printed, and it stops depending on which options the filter was
    invoked with.

    `motion` comes back too, and it is not decoration: VMAF on a near-static
    desktop is a weak test of what IDR density costs, because almost every
    frame is a skip. Reporting the motion mean is what turns "the reference was
    static, so read this row with care" from a caveat someone has to remember
    into a number sitting next to the score.
    """
    with open(path, encoding='utf-8') as handle:
        doc = json.load(handle)
    frames = doc.get('frames') or []
    scores = [f['metrics']['vmaf'] for f in frames if 'vmaf' in f.get('metrics', {})]
    if not scores:
        #: A zero-frame log means the comparison never ran, and a silent 0.00
        #: would read as "catastrophic quality" rather than "no measurement".
        raise RuntimeError('no per-frame VMAF in {}: {}'.format(
            path, json.dumps(doc)[:400]))
    scores.sort()
    motion = [f['metrics']['integer_motion2'] for f in frames
              if 'integer_motion2' in f.get('metrics', {})]
    return {'mean': sum(scores) / len(scores),
            'p1': scores[max(0, int(0.01 * (len(scores) - 1)))],
            'min': scores[0],
            'motion': (sum(motion) / len(motion)) if motion else float('nan'),
            'frames': len(scores)}


def quality(ffmpeg, device, seconds, encoder, gops, variants=()):
    """VMAF of the shipping browser argv at several shapes, from **one** capture.

    Recorded once to a lossless reference and then encoded N times offline, so
    the rows differ in the one knob named on the row and in nothing else -- not
    even in what was on the screen. Separate captures compared against each
    other would be a measurement of the desktop, not of the knob. (The latency
    matrix has no such control: its kbps column spans 12 s windows of a live
    desktop taken minutes apart, so a bitrate difference *between* rows there
    is content noise. Only this mode can price a change.)

    Offline on purpose: `-re` and the live pipe are what `mse_latency_probe.py`
    is for, and a quality number does not want a wall clock in it.

    `gops` sweeps `-g` under the shipping muxer flags. `variants` names rows of
    `mse_latency_probe.VARIANTS` instead, which is how the *muxer* half gets
    held to the same controlled test. Both exist because the first run of this
    mode answered the question that was easy to ask and left the one the design
    turns on unmeasured: not "what does a short GOP cost" but "what does
    cutting a fragment per frame cost, at the shipping GOP".
    """
    tail = product_browser_tail(ffmpeg, encoder)
    #: The geometry comes from the product's own `-vf`, not from `--height`:
    #: this mode measures the browser shape, and that shape's scaler is part of
    #: what is being held constant across rows.
    vf = tail[tail.index('-vf') + 1]
    #: Imported once here rather than per row, and reached through
    #: `product_browser_tail`'s own `sys.path` insertion above.
    import mse_latency_probe
    #: `(label, gop, movflags, mux_extra)` -- the same shape the latency probe's
    #: `VARIANTS` carries, so a row here and a row there are the same
    #: configuration and the two tables can be read side by side.
    specs = []
    if variants:
        for name in variants:
            gop, movflags, extra = mse_latency_probe.VARIANTS[name]
            specs.append((name, gop, movflags, list(extra)))
    else:
        prod_movflags = mse_latency_probe.VARIANTS['prod'][1]
        specs = [('g{}'.format(g), g, prod_movflags, []) for g in gops]
    work = tempfile.mkdtemp(prefix='gop_quality_')
    ref = os.path.join(work, 'ref.mp4')
    print('recording {} s of screen [{}] losslessly at {} -> {}'.format(
        seconds, device, vf, ref))
    _run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin', '-y',
          '-f', 'avfoundation', '-framerate', str(FPS),
          '-i', '{}:none'.format(device), '-t', str(seconds),
          '-vf', vf, '-c:v', 'libx264', '-qp', '0', '-pix_fmt', 'yuv420p',
          '-r', str(FPS), ref], 'the lossless reference')
    ref_bytes = os.path.getsize(ref)
    print('reference: {:.1f} MiB ({:.0f} kbps -- this is the ceiling every '
          'row below is losing something against)'.format(
              ref_bytes / 1048576.0, ref_bytes * 8 / 1000.0 / seconds))
    print()
    print('{:>12} {:>5} {:>9} {:>8} {:>10} {:>9} {:>8}'.format(
        'variant', 'gop', 'cadence', 'kbps', 'VMAF mean', 'VMAF p1', 'min'))
    rows = []
    for label, gop, movflags, extra in specs:
        out = os.path.join(work, '{}.mp4'.format(label))
        vjson = os.path.join(work, '{}.vmaf.json'.format(label))
        #: Same argv the latency probe runs, with the two knobs this row varies
        #: overridden and the pipe swapped for a file. `replace_value` raises if
        #: the product stops emitting either flag, rather than quietly appending
        #: a second one that ffmpeg would then honour in place of the first.
        body = mse_latency_probe.replace_value(tail, '-g', str(gop))
        body = mse_latency_probe.replace_value(body, '-movflags', movflags)
        if extra:
            #: Muxer private options resolve only after `-f mp4`, so they go
            #: immediately before the output, not up with the encoder flags.
            cut = body.index('pipe:1')
            body = body[:cut] + list(extra) + body[cut:]
        body = body[:body.index('pipe:1')] + [out]
        _run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin',
              '-y', '-i', ref] + body, '{} encode'.format(label))
        _run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-nostdin',
              '-i', out, '-i', ref, '-lavfi',
              '[0:v][1:v]libvmaf=log_fmt=json:log_path={}'.format(vjson),
              '-f', 'null', '-'], '{} VMAF'.format(label))
        #: Every row is scored against the same reference, so the motion mean
        #: is identical across rows; reading it off the first one is enough and
        #: printing it once keeps the table readable.
        stats = vmaf_stats(vjson)
        kbps = os.path.getsize(out) * 8 / 1000.0 / seconds
        rows.append({'label': label, 'gop': gop, 'kbps': kbps,
                     'mean': stats['mean'], 'p1': stats['p1'],
                     'min': stats['min'], 'motion': stats['motion'],
                     'frames': stats['frames']})
        print('{:>12} {:>5} {:>8.1f}ms {:>8.0f} {:>10.2f} {:>9.2f} '
              '{:>8.2f}'.format(label, gop, gop * 1000.0 / FPS, kbps,
                                 stats['mean'], stats['p1'], stats['min']))
    print()
    motion = rows[0]['motion']
    print('reference motion (mean integer_motion2): {:.4f} over {} frames'.format(
        motion, rows[0]['frames']))
    if motion < 1.0:
        #: The threshold is not a standard, it is the number that separates
        #: "a desktop nobody was touching" from "something was moving". Below
        #: it most frames are skips, so a row's byte cost is dominated by how
        #: often it is forced to spend on a keyframe rather than by picture.
        print('  -> that is a near-static desktop. These rows price the knob on')
        print('     a mostly-frozen picture; on real motion the byte cost rises')
        print('     and the VMAF gap can widen. Re-run with something moving')
        print('     on screen before quoting a number from this table.')
    print()
    base = rows[0]
    print('against `{}` (gop {}, {:.0f} kbps, VMAF {:.2f}):'.format(
        base['label'], base['gop'], base['kbps'], base['mean']))
    for row in rows[1:]:
        print('  {:<12} {:+7.0f} kbps ({:+.0f}%)   {:+.2f} VMAF mean   '
              '{:+.2f} VMAF p1'.format(
                  row['label'], row['kbps'] - base['kbps'],
                  (row['kbps'] / base['kbps'] - 1) * 100.0,
                  row['mean'] - base['mean'], row['p1'] - base['p1']))
    print()
    print('How to read it -- the three shapes, all of which occur:')
    print('  * costs bytes, VMAF flat      -> the bits bought keyframes. The')
    print('    preset is now over-delivering; `-b:v` should come down to keep')
    print('    the wire at the number the menu promises.')
    print('  * costs bytes, VMAF *gains*   -> the bits bought picture. Leave')
    print('    `-b:v` alone and pay the wire.')
    print('  * costs bytes AND loses VMAF  -> the knob is fighting the rate')
    print('    control: the same `-b:v`/VBV budget is being spent on the new')
    print('    frames and the rest of the clip is starved. Do not read that as')
    print('    "the codec got worse", and do not ship it at this bitrate. Check')
    print('    whether the loss is uniform or a decay over the clip (the JSON')
    print('    has per-frame scores): a decay is the VBV draining, and the')
    print('    question becomes what buffer the shape actually needs.')
    print('work dir kept: {}'.format(work))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--seconds', type=float, default=8.0)
    parser.add_argument('--height', type=int, default=HEIGHT,
                        help='encoded height; the capture is scaled to it '
                             '(default {})'.format(HEIGHT))
    parser.add_argument('--variant', default='all',
                        help='substring of a variant name, or "all"')
    parser.add_argument('--list', action='store_true')
    #: `--quality` answers a different question than the rest of this file
    #: (what does a change *cost* in picture, rather than what does it buy in
    #: milliseconds), and it needs its own encoder selector because it holds
    #: the argv constant instead of varying it.
    parser.add_argument('--quality', action='store_true',
                        help='VMAF of the shipping browser argv at several '
                             'shapes, from one recorded capture')
    parser.add_argument('--encoder', default='vt', choices=('x264', 'vt'),
                        help='which encoder --quality holds constant '
                             '(default vt, because that is what `auto` picks '
                             'on a Mac and therefore what ships)')
    parser.add_argument('--gops', default='',
                        help='comma-separated GOPs for --quality '
                             '(default {})'.format(
                                 ','.join(str(g) for g in QUALITY_GOPS)))
    #: Not called `--variants`: `--variant` already exists in this file and
    #: means "substring filter over the latency matrix". These are the same
    #: names, but here they select rows to *encode offline and score*, so the
    #: separate word stops `--variant prod` from silently meaning two things.
    parser.add_argument('--shapes', default='',
                        help='comma-separated names from mse_latency_probe.'
                             'VARIANTS for --quality (overrides --gops; use '
                             'this to price a muxer change at the shipping GOP)')
    args = parser.parse_args()

    ffmpeg = find_ffmpeg()
    if ffmpeg is None:
        print('no ffmpeg found; looked in: {}'.format(
            ', '.join(c for c in FFMPEG_CANDIDATES if c)))
        return 2
    have = encoders(ffmpeg)
    if args.quality:
        #: Checked before the capture rather than discovered during it: a build
        #: without libvmaf would otherwise fail on the first row, after a
        #: screen recording had already been made for nothing.
        listed = subprocess.run([ffmpeg, '-hide_banner', '-filters'],
                                capture_output=True, text=True).stdout
        if ' libvmaf ' not in listed:
            print('this ffmpeg has no libvmaf filter, so --quality cannot '
                  "score anything (`ffmpeg -filters | grep vmaf` to check).")
            return 2
        device, devname = find_screen_device(ffmpeg)
        if device is None:
            print('no screen device in ffmpeg\'s avfoundation listing -- is '
                  'this shell allowed to record the screen?')
            return 2
        shapes = tuple(s.strip() for s in args.shapes.split(',') if s.strip())
        gops = tuple(int(g) for g in args.gops.split(',') if g.strip()) \
            or QUALITY_GOPS
        #: Validated before the recording, for the same reason as libvmaf: a
        #: typo would otherwise cost a capture and four encodes to discover.
        import mse_latency_probe
        unknown = [s for s in shapes if s not in mse_latency_probe.VARIANTS]
        if unknown:
            print('--shapes: unknown variant(s) {}; this probe knows: {}'.format(
                ', '.join(unknown),
                ', '.join(sorted(mse_latency_probe.VARIANTS))))
            return 2
        print('quality: screen=[{}] {}  encoder={}  {}s  rows={}'.format(
            device, devname, args.encoder, args.seconds,
            ','.join(shapes) if shapes
            else ','.join('g{}'.format(g) for g in gops)))
        with _AwakeDisplay():
            quality(ffmpeg, device, args.seconds, args.encoder, gops, shapes)
        return 0
    if args.list:
        print('ffmpeg: {}'.format(ffmpeg))
        print('encoders present: {}'.format(
            ', '.join(sorted(e for e in have if 'x26' in e or 'videotoolbox'
                             in e or 'hevc' in e))))
        for name, _, _, _ in VARIANTS:
            print('  variant: {}'.format(name))
        return 0

    device, devname = find_screen_device(ffmpeg)
    if device is None:
        print('no screen device in ffmpeg\'s avfoundation listing -- is this '
              'shell allowed to record the screen?')
        return 2
    with _AwakeDisplay() as awake:
        print('ffmpeg={}  screen=[{}] {}  {}s per variant  height={}  display '
              'held awake: {}'.format(
                  ffmpeg, device, devname, args.seconds, args.height,
                  'yes' if awake.held else 'NO (caffeinate unavailable)'))
        print('{:34s} {:>8s} {:>10s} {:>7s} {:>10s} {:>8s} {:>7s} {:>9s}'.format(
            'variant', 'cap fps', 'first ms', 'frames', 'bytes', 'cpu s',
            'cores', 'realtime'))

        picked = [v for v in VARIANTS
                  if args.variant == 'all' or args.variant in v[0]]
        results = []
        for name, enc, extra, filt in picked:
            wanted = enc[enc.index('-c:v') + 1]
            if wanted not in have:
                print('{:34s} {:>8s} {:>10s}  (encoder {} not in this build)'
                      .format(name, '-', '-', wanted))
                continue
            #: Preflighted per variant, not once at the top: a full run is well
            #: over a minute and the capture can go bad in the middle of it.
            #: Note what this preflight is *for*. It is not the check that
            #: caught the 350/442 frames/s runs -- that was `insane` below, and
            #: at the time its message blamed a sleeping display, which was
            #: wrong (see the corrected note on `insane`: the cause was this
            #: probe's own missing output `-r`). What measuring the capture on
            #: its own buys is the thing that gate cannot do: tell a sick
            #: *capture* apart from a sick *encode*. Without it, "the numbers
            #: are nonsense" has two candidate causes and no way to choose.
            cap_fps, _pf_frames, _pf_wall = capture_rate(ffmpeg, device)
            if cap_fps <= 0.0 or cap_fps > FPS * 1.5:
                print('{:34s} {:>8.1f} {:>10s}  CAPTURE UNSOUND: avfoundation '
                      'delivered {} frames in {:.1f}s against a {} fps request, '
                      'so every number this variant could produce would be '
                      'fiction -- skipped, not reported. Candidates, in the '
                      'order worth checking: the display is asleep or the lid is '
                      'closed; the screen-recording permission belongs to a '
                      'different binary than this ffmpeg; another process is '
                      'already holding this display. Note the rate is diluted by '
                      'ffmpeg startup (~0.6s here), so it reads low on purpose.'
                      .format(name, cap_fps, '-', _pf_frames, _pf_wall, FPS))
                results.append({'name': name, 'insane': True, 'dead': False,
                                'unsupported': False, 'cap_fps': cap_fps})
                continue
            result = run_one(ffmpeg, device, name, enc, extra, filt,
                             args.seconds, args.height)
            result['cap_fps'] = cap_fps
            results.append(result)
            if result['unsupported']:
                print('{:34s} {:>8.1f} {:>10s}  REFUSED: {}'.format(
                    name, cap_fps, '-', result['error'] or 'ffmpeg refused'))
                continue
            if result['dead']:
                #: Refused, not printed as a 0.00x row: "produced nothing" and
                #: "could not keep up" are different findings, and only one of
                #: them is about the encoder's speed. The message points at the
                #: two things that have actually caused it on this machine.
                print('{:34s} {:>8.1f} {:>10s}  PRODUCED NOTHING: {}. '
                      'Nothing came out of pipe:1, so this row has no speed to '
                      'report. On this machine the two causes have been a level '
                      'or profile the picture does not fit (see `vt_level` in '
                      'screen_mirror.py) and a capture that returned no frames '
                      'at all -- the stderr line above says which.'.format(
                          name, cap_fps, '-',
                          result['error'] or 'ffmpeg wrote zero bytes'))
                continue
            if result['insane']:
                #: Not "the encoder cannot keep up". A rate *above* the request
                #: means frames are being manufactured, not lost, and every time
                #: this has fired on this machine the cause was the frame-sync
                #: mode rather than the encoder -- see `insane`. The message
                #: names the two things worth looking at, in that order.
                print('{:34s} {:>8.1f} {:>10s}  OUTPUT RATE UNSOUND: {:.0f} '
                      'encoded frames/s against a {} fps request, with a capture '
                      'that measured sane. Frames are being duplicated somewhere '
                      'between the two, so this row is not comparable to the '
                      'others -- check the output `-r` / `-fps_mode` this variant '
                      'ends up with before believing any of its numbers.'.format(
                          name, cap_fps, '-', result['rate'], FPS))
                continue
            realtime = (result['frames'] / (FPS * result['wall'])
                        if result['wall'] else 0.0)
            #: The same cost as `cpu s`, in the unit people actually compare
            #: (see the module docstring: the seconds got read as cores once
            #: and the misreading reached AGENTS.md).
            cores = result['cpu'] / result['wall'] if result['wall'] else 0.0
            print('{:34s} {:>8.1f} {:>10.1f} {:>7d} {:>10d} {:>8.2f} '
                  '{:>7.2f} {:>8.2f}x'.format(
                      name, cap_fps, result['first_ms'] or float('nan'),
                      result['frames'], result['bytes'], result['cpu'],
                      cores, realtime))
        good = [r for r in results if not r['unsupported'] and not r['insane']
                and not r['dead']]
        if not good:
            print('\nnothing was measured: every variant was refused or its '
                  'capture was unsound. No table is printed because a table of '
                  'skipped rows reads like a result.')
            return 2
        if len(good) > 1:
            base = next((r for r in good if r['first_ms'] and r['frames']), None)
            if base:
                print('\nrelative to {}:'.format(base['name']))
                for r in good:
                    if r is base or not r['first_ms']:
                        continue
                    print('  {:34s} first byte {:+7.1f} ms   bytes {:+6.1f}%   '
                          'cpu {:+6.1f}%'.format(
                              r['name'], r['first_ms'] - base['first_ms'],
                              (r['bytes'] / base['bytes'] - 1) * 100
                              if base['bytes'] else 0.0,
                              (r['cpu'] / base['cpu'] - 1) * 100
                              if base['cpu'] else 0.0))
    return 0


if __name__ == '__main__':
    sys.exit(main())
