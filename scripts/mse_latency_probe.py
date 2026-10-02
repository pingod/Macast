#!/usr/bin/env python3
"""How much of the `browser` output shape's latency is ours to take back.

The shape today is: ffmpeg writes fragmented MP4 to a pipe, a plugin-local HTTP
server streams it, and a page appends it to a `MediaSource` while parking the
playhead `LIVE_EDGE_SECONDS` behind the buffer head. End to end that is

    ffmpeg startup + **fragment cadence** (average half of it) + **park**
    + decode/render

and two of those four terms are ours to choose. Both were chosen by reasoning
-- and the matrix below is that reasoning measured; v0.19 ships the `every12`
row because of it, so read the first bullet as what the cadence *was*:

* the cadence was `gop_size('browser') = FPS // 2 = 12 frames = 0.5 s`
  because `movflags=frag_keyframe` ends a fragment at the next keyframe -- so
  the GOP was not only a seek granularity here, it was the floor on how long
  a finished picture sits in the encoder. `frag_every_frame` at the same GOP
  broke that coupling and took this path from 1305 ms to 838 ms end to end
  (real capture, hardware encoder);
* `LIVE_EDGE_MIN_SECONDS = 0.5` because "分片节奏本来就是 0.5 秒，1.0 秒留了
  两个分片余量" -- the floor exists *to cover the cadence*, so lowering one
  without the other is not a choice anyone actually made.

The lever that decouples them is `movflags=+frag_every_frame` (or
`frag_duration`, which cuts on a timer instead of on a keyframe). Whether it
buys anything is **not** something to reason about, because the spec is soft
on exactly this point. W3C `media-source-2`, Segment Parser Loop:

    "Note: The frequency at which the coded frame processing algorithm is run
     is implementation-specific. The coded frame processing algorithm MAY be
     called when the input buffer contains the complete media segment or it
     MAY be called multiple times as complete coded frames are added to the
     input buffer."

So a browser is allowed to hand per-frame fragments to the decoder one at a
time, and allowed to sit on them until it thinks a segment is complete. Which
one Chrome does is a measurement.

What the first 12-cell matrix settled (2026-10-02, this machine)
---------------------------------------------------------------
Synthetic `-re` testsrc2 1280x720@24, **libx264 ultrafast+zerolatency at a
3 Mbps target with no VBV** -- i.e. not the shipping argv; see finding 8 before
quoting any absolute number from this block. 12 s window,
chrome-headless-shell 151. `seek` controller throughout.

1. **Chromium appends incrementally.** Reported granularity equals the
   configured cadence in every variant: 500.0 ms for `prod`, 1000 ms for
   `gop24`, 125 ms for `frag100`, 41.7 ms (= 1/24 s) for `every_frame`. The
   spec's MAY resolves in favour of per-frame processing, so `frag_every_frame`
   is not buying bytes for nothing.
2. **Withdrawn.** This finding used to read "it costs ~2% on the wire:
   3847-3858 kbps for `every_frame` against 3781 for `prod`". The number is
   real and the conclusion is not: `-b:v` is a *target*, so rate control pays
   for a shorter GOP in QP rather than in bytes, and the kbps column measures
   the rate controller's obedience. With the VBV now in the argv (finding 8)
   every variant is additionally clamped by `-maxrate` and the column converges
   on the target no matter what changes. What a short GOP actually spends is
   **quality at a fixed bitrate**, and the honest statement of that is a VMAF
   delta -- `encoder_latency_probe.py` has the harness, it has not been run for
   these variants, and until it is the cost is an open question.
3. **Where the park actually fires, `behind_p50 = park - cadence/2`**
   (every_frame@0.5: 479 predicted, 485 reported; frag100@0.5: 438 / 444;
   prod@0.5: 250 / 265; every_frame@0.1: 79 / 91). "Fires" is readable off the
   row: `beh_p95` reaches the park. **Where it does not fire, `behind` is simply
   wherever the startup ramp left the playhead, and the park is doing nothing at
   all** -- at park 1.0 s with a 0.5 s cadence, `prod` sits at behind_p50 331
   with `beh_p95` 556, i.e. the shipping `LIVE_EDGE_SECONDS = 1.0` is not
   controlling a from-start viewer's latency; it is a recovery distance that has
   not had to recover anything. Consequence for design: a finer cadence at the
   same park makes `behind` *worse* (485 vs 265 at park 0.5), because the
   sawtooth no longer drains. The cadence's value is that it lets the park go
   lower, not that it lowers `behind` by itself.
4. **The hard seek is what stalls.** At park 0.1 every variant stalled 65-329
   times for 3.6-11.2 s inside a 12 s window; at park 0.5, 43-273 times; at park
   1.0, 1-4 times (38-111 ms) except `gop24` at 36/1863. `behind` and `stalls`
   are one trade and the shipping controller buys the low end of it with force,
   which is why `--controller rate` exists.
5. **A coarser GOP is worse everywhere**: `gop24` against `prod` at park 1.0 is
   behind 556 vs 331 and stalls 36/1863 ms vs 4/111 ms.
6. **Absolute `lag` here is not a glass-to-glass number, and the reason is now
   measured rather than suspected.** `-re` does pace the encoder (moof arrivals
   551/1051/1548/2048 ms for `prod` -- 500.0 +- 3 ms), but the *first* fragment
   is emitted at ~50 ms of wall time already carrying a full cadence of media,
   because the encoder primes faster than real time. That front-loading *is*
   `drift_med`: +212 ms for `prod` (0.5 s front-loaded minus 0.25 s average
   completion lag), +430 for `every_frame`, +394 for `frag100`, and -41 for
   `gop24`, whose first fragment took 542 ms so nothing was front-loaded. It is
   flat over the window (end-of-window `drift_ms` differs from `drift_med` by
   <40 ms across 14.5 s, a <0.3% rate error), so cross-variant comparison is
   sound and the absolute zero is not. **A capture source cannot emit a frame
   before it is captured, so `--capture` is how a quotable zero is obtained.**
7. **`-g 24` is not a 1 s cadence in general.** `frag_keyframe` cuts on
   keyframes and x264 scenecut inserts them, so on a constantly-changing
   synthetic source the cadence is content-dependent -- `gop24`'s first two moofs
   arrived 50 and 551 ms apart, not 1002. A real desktop is mostly static and
   would cut *less* often, so this row understates the shipping cadence's
   stability. If it ever matters the fix is `-sc_threshold 0`, not a new probe.
8. **The probe was measuring its own idea of the argv, not the product's, and
   it drifted three times in one session.** Each drift was silent and each went
   the way that flattered the result:
     a. It hardcoded `libx264 -preset ultrafast -tune zerolatency`. That is not
        the product's x264 branch either (which also carries `-flags +low_delay
        -thread_type slice`) and it is not what `auto` picks on a Mac, which is
        `h264_videotoolbox`. So findings 1-7 and every early real-capture row
        **exclude VideoToolbox's ~200 ms first-frame cost**.
     b. It omitted the half-second VBV (`rate_caps`: `-maxrate` 1.5x,
        `-bufsize` 0.5x) -- the one omission that biases precisely the
        comparison this probe exists to make, since a short GOP turns every
        keyframe into a burst and an uncapped encoder is free to spend it.
     c. It omitted `-level 42` because a comment here asserted `vt_level(720)`
        returns None. `VT_LEVELS` is a **threshold** table (`height <= limit`),
        not an exact-match one, so 720 returns `'42'`. The same diff caught
        `-vf scale=-2:720` copied as `scale=1280:720`, which squashes a
        3456x2234 desktop instead of letterboxing it -- different pixel count,
        different bits per pixel, different rate control.
   Found only by diffing the two argvs token by token, which nothing did until
   the third time. The fix is not a better transcription: `product_tail()` now
   runs `build_ffmpeg_command(kind='browser')` in a **subprocess** and takes
   everything from `-map` onward, and `replace_value()` raises rather than
   appends if the product stops emitting a flag a variant overrides. A
   transcription of a shipping configuration is a claim that the two match; the
   probe is now the thing that checks the claim instead of making it.
   **Consequence for the numbers: cross-variant ordering survives (it is a
   property of where the muxer cuts, which was never wrong); absolute latency
   and kbps do not.**

What this probe reports, per (cadence, park) pair
-------------------------------------------------
`granularity`  the median positive step of `buffered.end` sampled every rAF.
               This is the direct answer to the MAY: if per-frame fragments
               come out at ~1/fps steps, Chrome processes coded frames as they
               arrive; if they come out in ~0.5 s steps, it batches, and
               `frag_every_frame` buys nothing but bytes.
`behind`       `buffered.end - currentTime` while parked. The park term, and
               the jitter on top of it.
`lag`          `(server clock now - ffmpeg spawn) - mediaTime of the frame on
               screen`, from `requestVideoFrameCallback`. **Includes ffmpeg
               startup** (~0.5 s, measured separately by
               `encoder_latency_probe.py`), so compare rows against each other
               and never against a glass-to-glass number.
`lag2`         the same quantity with `v.currentTime` in place of rVFC's
               `mediaTime`. Two measures of one number exist because on the
               shipping shape they came out 111 ms and (by `behind - drift`)
               369 ms, and `behind = lag + drift` is arithmetic, not opinion.
`mt-cur`       rVFC `mediaTime` minus the playhead, on the frames where both
               were read. This is the number that says which of the two lags to
               quote; ~0 means they are the same measurement.
`identity`     `behind_last - (lag2_last + drift)`, from one instant. ~0 = the
               row describes one stream. Anything else = do not quote the row.
`drift_med`    the median instantaneous `head_media - wall_elapsed`, which is
               the term that reconciles the two medians: `behind_p50` =
               `lag2_p50` + `drift_med`. The row's `drift_ms` is a single
               end-of-window sample and, with a 0.5 s cadence, swings by a full
               cadence -- so it cannot be used to reconcile anything.
`stalls`       `waiting` events and their total duration. A park that is too
               tight shows up here before it shows up anywhere else.
`seeks`        how many times the controller moved the playhead. Under `seek`
               this is the park firing, and a row with hundreds of seeks is a
               row whose `behind` is being held down by force -- read it next to
               `stalls`, which is the price. Under `rate` it should be 1 (the
               initial jump onto the edge), and more than that means the
               emergency seek fired, i.e. the rate clamp cannot keep up.
`rate_*`       the `playbackRate` range the controller used, as min / p50 / max.
               Sitting on the clamp continuously means the gain is too small for
               the park; barely leaving 1.000 means the controller is idle.
`bytes_per_s`  what actually reached the wire. Read it as a **rate-control
               check, not a cost**: with `-b:v` plus the product's `-maxrate`
               every variant is aiming at the same ceiling, so this column says
               whether the encoder hit it. It cannot say what a shorter GOP
               cost -- that is quality at a fixed bitrate (finding 2).

Usage:
    scripts/mse_latency_probe.py --list
    scripts/mse_latency_probe.py                       # the default matrix
    scripts/mse_latency_probe.py --quick               # one variant, one park
    scripts/mse_latency_probe.py --variants prod,every_frame --edges 0.1,0.5
    scripts/mse_latency_probe.py --controller rate --edges 0.5,0.3,0.15
    scripts/mse_latency_probe.py --capture             # real avfoundation

Writes nothing outside the process pipes and /tmp. It does now run the product
in a **subprocess** to ask it for the shipping argv (finding 8); that subprocess
stubs `appdirs.user_config_dir` to a temp dir before `import macast`, so no
config directory is read or written and `Setting` is never touched from here.
It also starts a real browser window (headless) and, with `--capture`, a real
screen capture.
"""

import argparse
import http.server
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FFMPEG_CANDIDATES = [
    os.environ.get('MACAST_FFMPEG'),
    'ffmpeg',
    '/opt/homebrew/opt/ffmpeg/bin/ffmpeg',
    '/usr/local/bin/ffmpeg',
]

#: Same list the plugin uses, for the same reason: a GUI-launched Macast does
#: not inherit a shell's PATH, and a probe that only tried `ffmpeg` would report
#: "no ffmpeg here" on a machine that has it.
BROWSER_CANDIDATES = [
    os.environ.get('MACAST_PROBE_BROWSER'),
    os.path.expanduser('~/Library/Caches/ms-playwright/'
                       'chromium_headless_shell-1234/'
                       'chrome-headless-shell-mac-arm64/chrome-headless-shell'),
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Vivaldi.app/Contents/MacOS/Vivaldi',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
    shutil.which('chromium'),
]

FPS = 24
WIDTH, HEIGHT = 1280, 720

#: cadence name -> (encoder GOP in frames, `-movflags`, extra muxer options)
#:
#: `every12` is what `screen_mirror.py` ships today, character for character
#: (v0.19, 2026-10): `gop_size('browser')` is 12 and `OUTPUTS['browser'][3]`
#: is `frag_every_frame+empty_moov+default_base_moof`. `prod` is the shape it
#: replaced -- the same GOP with `frag_keyframe` -- kept because the pair is
#: the honest A/B this file exists to run and re-run. Every other row changes
#: exactly one of those numbers, so a difference between rows is a difference
#: in that number and not in anything else.
#:
#: **`frag_duration` is not a `movflags` value** -- it is its own AVOption on
#: the mov muxer, and writing it inside `-movflags` makes ffmpeg fail with
#: `[Eval @ ...] Undefined constant or missing '(' in 'frag_duration=100000'`
#: and `Invalid argument`, i.e. nothing is written at all. The first version of
#: this table had it inside the flags string; the symptom downstream was a cell
#: reporting `stream ended`, which reads like the browser hung up. It is the
#: third entry in the tuple for exactly that reason.
#:
#: Verified against ffmpeg 9.0.2 by counting `moof` boxes in the output:
#: `frag_every_frame` at `-g 12` and 24 fps for 4 s produced **96 moofs for 96
#: frames**, so the muxer really does cut per frame and the cadence row below is
#: a statement about the muxer, not a hope.
VARIANTS = {
    #: The pre-0.19 shipping shape -- what the latency improvement is measured
    #: against. Fragment cadence 0.5 s.
    'prod': (12, 'frag_keyframe+empty_moov+default_base_moof', []),
    #: Same muxer flags, double GOP: cadence 1.0 s. This row exists to prove
    #: the coupling in the other direction -- if `prod` and `gop24` differ by
    #: ~0.25 s of cadence, the GOP really is the cadence.
    'gop24': (24, 'frag_keyframe+empty_moov+default_base_moof', []),
    #: The lever under question: cut a fragment on every frame, keyframe or
    #: not. GOP is held at 24 so that the *only* thing this row changes
    #: against `gop24` is where the muxer cuts.
    'every_frame': (24, 'frag_every_frame+empty_moov+default_base_moof', []),
    #: ... and that pairing is exactly the trap this row closes. `every_frame`
    #: differs from `prod` in **two** places -- where the muxer cuts *and* how
    #: often a keyframe lands -- so "every_frame beat prod by 550 ms on real
    #: capture" (2026-10-02, `--capture 2`) does not by itself say which one
    #: bought it, and the keyframe half also moves the bitrate (fewer IDR
    #: frames: 4007 vs 4864 kbps measured), which is the number a design would
    #: quote for "what does the finer cadence cost on the wire". This row holds
    #: the GOP at the shipping 12 so the only difference against `prod` is the
    #: cut. Added after the A/B was already run, i.e. it exists because the
    #: first comparison was confounded, not because anyone asked for it.
    #: Then it won: measured against `prod` at park 1.0 on real capture,
    #: 838 ms vs 1305 ms, same VMAF, ~1% more bytes -- and v0.19 ships this
    #: row, so it is now the shape the other rows are read against.
    'every12': (12, 'frag_every_frame+empty_moov+default_base_moof', []),
    #: The option that keeps every invariant the shipping shape has, and pays
    #: in bytes instead. `_Broadcaster._retain` hands a late joiner the init
    #: segment plus the **whole fragments** at the tail of the ring, and with
    #: `frag_keyframe` each of those necessarily starts at an IDR -- so a
    #: viewer arriving mid-stream decodes from a clean picture. Cut per frame
    #: instead and the tail can start on a P-frame with no reference until the
    #: next keyframe: up to 0.5 s of garbage at `-g 12`, up to 1 s at `-g 24`.
    #: The latency probe cannot see that at all (its viewer is present from the
    #: first byte), so the two rows below are the alternative: shorten the
    #: keyframe interval and let the muxer keep cutting on keyframes. Same
    #: direction, no alignment hazard, and the bill is bitrate -- which is why
    #: `bytes_per_s` is a column and not an afterthought.
    'gop4': (4, 'frag_keyframe+empty_moov+default_base_moof', []),
    'gop2': (2, 'frag_keyframe+empty_moov+default_base_moof', []),
    #: The middle option nobody has to argue about: cut on a timer, keep the
    #: short GOP. `-frag_duration` is in microseconds.
    'frag100': (12, 'empty_moov+default_base_moof',
                ['-frag_duration', '100000']),
    'frag50': (12, 'empty_moov+default_base_moof',
               ['-frag_duration', '50000']),
}

#: Probe encoder name -> the `encoder=` value `build_ffmpeg_command` takes.
#:
#: The probe's CLI speaks in codec names because that is what a reader of a
#: latency table wants to see; the product speaks in `software`/`hardware`
#: because `auto` has to resolve a probe result into one of them. This is the
#: whole of the mapping, and it is the only thing about the encoder that this
#: file still knows.
ENCODERS = {'x264': 'software', 'vt': 'hardware'}

#: Target wire bitrate, before the encoder's multiplier. Not a round number
#: chosen here: it is `QUALITIES['720'][1]`, because the probe scales to 720
#: lines and the menu's 720p row is the configuration this measures.
BITRATE = 4000000

#: Where the shipping argv comes from. See `product_argv`.
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PRODUCT = os.path.join(REPO, 'macast', 'plugins', 'renderer',
                       'screen_mirror.py')

#: Asks the product for its own argv instead of transcribing it.
#:
#: This replaced a hand-copied `ENCODERS` table, and the reason it had to is
#: that the copy drifted **three times in one session**, each time silently and
#: each time in the direction that flattered the measurement:
#:
#:   1. It hardcoded `libx264 -preset ultrafast -tune zerolatency` while
#:      `auto` on a Mac ships `h264_videotoolbox` -- so every early row
#:      excluded VideoToolbox's ~200 ms first-frame cost.
#:   2. It omitted the product's half-second VBV (`rate_caps`: `-maxrate`
#:      1.5x, `-bufsize` 0.5x). That matters most for exactly the comparison
#:      being made here, since a short GOP turns every keyframe into a burst
#:      and an uncapped encoder is free to spend it.
#:   3. It omitted `-level 42` on the strength of a comment claiming
#:      `vt_level(720)` returns None. `VT_LEVELS` is a **threshold** table
#:      (`height <= limit`), not an exact-match one, so 720 returns `'42'`.
#:      The same pass found `-vf scale=-2:720` had been copied as
#:      `scale=1280:720`, which squashes a 3456x2234 desktop instead of
#:      letterboxing it -- different pixel count, different bits per pixel,
#:      different rate control.
#:
#: Three for three, all invisible in the output. A transcription of a shipping
#: configuration is a claim that the two match, and nothing here was checking
#: that claim. So the probe now asks: it runs the product in a **subprocess**
#: (never in-process -- importing the plugin pulls in `Setting` and would put
#: module-level work inside the very window being timed), takes everything from
#: `-map` onward, and owns only the source side, which is deliberately not the
#: product's (synthetic `testsrc2`, or a capture index the probe was handed).
#:
#: `appdirs` is stubbed before `import macast` so the subprocess touches no
#: real config dir (AGENTS.md 4.9).
_ASK_PRODUCT = r'''
import json, sys, tempfile
import appdirs
TMP = tempfile.mkdtemp(prefix='mse_probe_')
appdirs.user_config_dir = lambda *a, **k: TMP
sys.path.insert(0, %(repo)r)
import importlib.util
spec = importlib.util.spec_from_file_location('sm', %(product)r)
sm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sm)
capture = sm._Capture('probe', [['-f', 'avfoundation',
                                 '-framerate', str(sm.FPS), '-i', '0:none']])
print(json.dumps(sm.build_ffmpeg_command(
    sys.argv[1], capture, %(height)r, %(bitrate)r,
    kind='browser', encoder=sys.argv[2])))
'''

_product_cache = {}


def product_argv(ffmpeg, encoder_kind, height=720, bitrate=BITRATE):
    """The product's own `build_ffmpeg_command(..., kind='browser')`, as a list.

    Cached per (ffmpeg, encoder, height, bitrate): each call is a subprocess,
    and a matrix calls it once per cell.
    """
    key = (ffmpeg, encoder_kind, height, bitrate)
    if key not in _product_cache:
        script = _ASK_PRODUCT % {'repo': REPO, 'product': PRODUCT,
                                 'height': height, 'bitrate': bitrate}
        proc = subprocess.run([sys.executable, '-c', script, ffmpeg,
                               encoder_kind],
                              capture_output=True, text=True, timeout=60)
        if proc.returncode:
            #: Loud, and naming the file: a probe that silently fell back to
            #: its own idea of the argv is the failure mode this function
            #: exists to end.
            raise RuntimeError(
                'cannot ask {} for its argv (exit {}):\n{}'.format(
                    PRODUCT, proc.returncode, proc.stderr.strip()[-2000:]))
        _product_cache[key] = json.loads(proc.stdout)
    return _product_cache[key]


def product_tail(ffmpeg, encoder_kind, height=720, bitrate=BITRATE):
    """The product's argv from `-map` onward: everything except the source."""
    full = product_argv(ffmpeg, encoder_kind, height, bitrate)
    try:
        cut = full.index('-map')
    except ValueError:
        raise RuntimeError('product argv has no -map: {}'.format(full))
    return full[cut:]


def replace_value(argv, flag, value):
    """Set `flag` to `value` in an argv that already carries it.

    Raises rather than appends: silently adding a second `-g` would leave two
    in the argv and ffmpeg would honour the last one, which is a correct
    result reached by accident -- and if the product ever stops emitting the
    flag, "the variant changed it" would become a lie.
    """
    out = list(argv)
    index = out.index(flag)
    out[index + 1] = value
    return out

DEFAULT_MATRIX = [
    ('prod', 1.0), ('prod', 0.5), ('prod', 0.1),
    ('frag100', 0.5), ('frag100', 0.1),
    ('every_frame', 0.5), ('every_frame', 0.1), ('every_frame', 0.05),
]

#: How the playhead is held near the live edge.
#:
#: `seek` is what the production page does today: whenever
#: `buffered.end - currentTime` exceeds the park, jump the playhead back to
#: exactly that distance. It works, and the 12-cell matrix says it is the thing
#: that stalls -- at park 0.1 every cadence variant stalled 65-329 times for
#: 3.6-11.2 s inside a 12 s window. A seek leaves the playhead a fixed distance
#: from the buffer head with no margin, so the first append that is later than
#: that margin starves it.
#:
#: `rate` trims with `playbackRate` instead and seeks at most once, to get onto
#: the edge in the first place. `rate = 1 + clamp(err * gain, ±clamp)`, so at
#: gain 0.25 an error of 0.4 s asks for the full clamp, and clamp 0.06 drains
#: 400 ms of excess buffer in about 7 s. That is much slower than a seek, which
#: is the entire point: the margin is rebuilt over seconds instead of being
#: spent in one jump.
CONTROLLERS = {
    'seek': {'gain': 0.0, 'clamp': 0.0},
    'rate': {'gain': 0.25, 'clamp': 0.06},
}

#: The page. Kept as a raw string for the reason §4.8 records about
#: `PLAYER_PAGE`: inside a non-raw Python string a JS `'\n'` becomes a real
#: newline and the page throws a SyntaxError, which reads exactly like "the
#: measurement is 0".
PAGE = r"""<!doctype html>
<meta charset=utf-8>
<style>html,body{margin:0;background:#000}video{width:100vw;height:100vh}</style>
<video id=v muted playsinline></video>
<script>
var CFG = @CFG@;
function post(o){o.stage=o.stage||'x';fetch('/r',{method:'POST',
  body:JSON.stringify(o),keepalive:true}).catch(function(){})}
var v=document.getElementById('v');
var R={stalls:0,stall_ms:0,frames:0,bytes:0,chunks:0,first_byte_ms:null,
       playing_ms:null,err:null,samples:[],presents:[],behind:[],lags:[],
       lags2:[],mt_cur:[],drifts:[],clock_offset_ms:null,t0:null,
       seeks:0,rate_sets:0,rates:[]};
var stallStart=null;
v.addEventListener('waiting',function(){R.stalls++;stallStart=performance.now()});
v.addEventListener('playing',function(){
  if(stallStart!==null){R.stall_ms+=performance.now()-stallStart;stallStart=null}
});
v.addEventListener('stalled',function(){R.stalled_events=(R.stalled_events||0)+1});
v.addEventListener('error',function(){
  R.err='MediaError code '+(v.error?v.error.code:'?');post({stage:'error',err:R.err})});

// One clock for both halves. The server owns the wall clock that ffmpeg's
// spawn was stamped against; asking for it once and differencing against
// Date.now() gives an offset that survives the page's own load time.
fetch('/clock').then(function(r){return r.json()}).then(function(c){
  R.clock_offset_ms=c.server_ms-Date.now();
  R.t0=c.t0;
  return fetch('/t0').then(function(r){return r.text()})
}).then(function(){return start()}).catch(function(e){
  post({stage:'error',err:'clock: '+e})});

function start(){
  var ms=new MediaSource();
  v.src=URL.createObjectURL(ms);
  ms.addEventListener('sourceopen',function(){
    var sb;
    try{sb=ms.addSourceBuffer(CFG.codecs)}
    catch(e){post({stage:'error',err:'addSourceBuffer: '+e});return}
    sb.mode='sequence';
    var queue=[],pending=null;
    sb.addEventListener('updateend',function(){
      pending=null;
      if(queue.length){pending=queue.shift();try{sb.appendBuffer(pending)}
        catch(e){post({stage:'error',err:'append: '+e})}}
    });
    sb.addEventListener('error',function(){
      post({stage:'error',err:'sourcebuffer error'})});
    var t_start=performance.now();
    fetch('/stream').then(function(resp){
      var rd=resp.body.getReader();
      (function pump(){
        rd.read().then(function(res){
          if(res.done){post({stage:'error',err:'stream ended'});return}
          if(R.first_byte_ms===null)R.first_byte_ms=performance.now()-t_start;
          R.bytes+=res.value.length;R.chunks++;
          if(sb.updating||pending)queue.push(res.value);
          else{pending=res.value;try{sb.appendBuffer(res.value)}
               catch(e){post({stage:'error',err:'append: '+e})}}
          pump();
        }).catch(function(e){post({stage:'error',err:'read: '+e})});
      })();
    }).catch(function(e){post({stage:'error',err:'fetch: '+e})});

    // The park, verbatim from the production page, with L as the only knob.
    var lastData=performance.now();
    function tick(){
      var now=performance.now();
      if(sb.buffered.length){
        var end=sb.buffered.end(sb.buffered.length-1);
        var cur=v.currentTime;
        R.samples.push([Math.round(now-t_start),+end.toFixed(4)]);
        R.behind.push([Math.round(now-t_start),+(end-cur).toFixed(4)]);
        // How the playhead is held near the live edge. Two controllers, because
        // the 12-cell matrix said the shipping one is the thing that stalls:
        // `behind_p50` tracks `park - cadence/2` almost exactly, so tightening
        // the park *does* cut the distance -- and at park 0.1 every variant
        // stalled 65-329 times for 3.6-11.2 s inside a 12 s window. A seek puts
        // the playhead a fixed distance from the buffer head and leaves it there
        // with no margin; the next append that is late by more than that margin
        // starves it. 'rate' instead trims with playbackRate, which buys margin
        // back over seconds rather than spending it in one jump.
        if(CFG.park_mode==='rate'){
          var err=(end-cur)-CFG.live_edge;         // >0: further behind than target
          if(!R.seeks&&end-cur>CFG.live_edge+0.15){
            // one jump to get onto the edge, then never again unless forced
            v.currentTime=end-CFG.live_edge;R.seeks++;
          }else{
            var want=1.0+Math.max(-CFG.rate_clamp,
                                  Math.min(CFG.rate_clamp,err*CFG.rate_gain));
            if(Math.abs(v.playbackRate-want)>0.005){
              v.playbackRate=want;R.rate_sets++;
              if(R.rates.length<2000)R.rates.push(+want.toFixed(3));
            }
            if(end-cur>CFG.live_edge+1.5){
              v.currentTime=end-CFG.live_edge;R.seeks++;v.playbackRate=1.0;
            }
          }
        }else if(end-cur>CFG.live_edge){
          v.currentTime=end-CFG.live_edge;R.seeks++;
        }
        // Second measure of the same quantity, from two numbers this page owns:
        // `v.currentTime` in place of rVFC's `mediaTime`. Both are reported
        // because they once disagreed by 258 ms and a design document must not
        // be built on whichever one flatters us. **They turned out to agree**:
        // `mt-cur` below is +14..+18 ms across the matrix, so the disagreement
        // was never between the two lags -- it was between `lag` and `behind`,
        // and `drift` is the term that separates them.
        // NOTE: `performance.timeOrigin + performance.now()` already IS a wall
        // clock in `Date.now()` units, so `clock_offset_ms` (server minus page,
        // measured once at load) is the only correction it needs. A third term
        // double-counts the epoch and lands thousands of ms out.
        var wall_now=performance.timeOrigin+now+(R.clock_offset_ms||0);
        if(R.t0!==null){
          var l2=wall_now-R.t0-cur*1000;
          R.lags2.push(l2);
          // Instantaneous drift = behind - lag2 = (head_media - wall_elapsed)
          // at this tick. The row's `drift_ms` samples that quantity once, at
          // the end of the window; the median over the window is what actually
          // reconciles the two percentiles, and without it `behind 326` and
          // `lag 102` look like a contradiction instead of `326 = 102 + 224`.
          R.drifts.push((end-cur)*1000-l2);
        }
        if(!v.paused&&v.readyState>=2&&now-lastData>CFG.stall_ms){
          // production reloads the page here; a probe that reloads measures
          // nothing, so it records the fact and keeps going.
          R.hard_stalls=(R.hard_stalls||0)+1;lastData=now;
        }
      }
      if(!v.paused&&v.readyState>=3)lastData=now;
      if(now-t_start<CFG.seconds*1000+2500)requestAnimationFrame(tick);
      else finish();
    }

    // Which frames actually reached the screen, and what they were stamped.
    // `requestVideoFrameCallback` is the only API that says so per frame:
    // `timeupdate` fires at ~4 Hz and would round the whole answer away.
    function onFrame(now,meta){
      R.frames++;
      // Wall clock of this presentation, on the server's clock: the page's
      // `performance.timeOrigin` is its own epoch as a wall time, and
      // `clock_offset_ms` was measured once at load against `/clock`. So
      // `lag` = when this frame was shown - when it was captured, where
      // "captured" is `t0 + mediaTime` and t0 is ffmpeg's spawn stamp.
      var wall=performance.timeOrigin+performance.now()+
               (R.clock_offset_ms||0);
      var lag=(R.t0===null)?null:wall-(R.t0+meta.mediaTime*1000);
      if(lag!==null)R.lags.push(lag);
      R.mt_cur.push([+(meta.mediaTime*1000).toFixed(1),
                     +(v.currentTime*1000).toFixed(1)]);
      R.presents.push([+performance.now().toFixed(1),
                       +meta.mediaTime.toFixed(4),
                       lag===null?null:+lag.toFixed(0)]);
      v.requestVideoFrameCallback(onFrame);
    }
    if(v.requestVideoFrameCallback)v.requestVideoFrameCallback(onFrame);

    v.play().then(function(){
      R.playing_ms=performance.now()-t_start;
      requestAnimationFrame(tick);
    }).catch(function(e){post({stage:'error',err:'play: '+e})});
  });
}

function pct(a,p){if(!a.length)return null;a=a.slice().sort(function(x,y){
  return x-y});return a[Math.min(a.length-1,Math.floor(a.length*p))]}
function finish(){
  var behinds=R.behind.map(function(x){return x[1]});
  // Buffer-head growth granularity: the median *positive* step. Zero steps
  // (a sample where nothing new had arrived) are excluded on purpose -- their
  // share is itself reported, as `idle_samples`, because a browser that
  // batches has many of them.
  var steps=[],idle=0;
  for(var i=1;i<R.samples.length;i++){
    var d=R.samples[i][1]-R.samples[i-1][1];
    if(d>1e-6)steps.push(+d.toFixed(4));else idle++;
  }
  // Absolute lag: drop the first second of presents, which is the ramp from
  // an empty buffer and is not the steady state anyone is choosing between.
  var warm=R.lags.slice(Math.max(0,Math.floor(R.lags.length*0.1)));
  // Same warm-slice rule for the `currentTime`-based lag, so the two rows are
  // comparable without either one flattering itself on the ramp.
  var warm2=R.lags2.slice(Math.max(0,Math.floor(R.lags2.length*0.1)));
  // And the offset that decides which of them to quote: rVFC's `mediaTime`
  // minus the playhead, on the frames where both were read. If this is ~0 the
  // two lags are the same measurement; if it is 258 ms then rVFC is stamping
  // frames from a different origin than the SourceBuffer timeline (which is
  // what `sb.mode='sequence'` makes plausible) and the rVFC number is the
  // unusable one.
  var mtd=R.mt_cur.map(function(x){return x[0]-x[1]});
  // `drift` is the check on `lag`'s zero point, and it is reported rather than
  // trusted: `head_media` is where the media timeline has got to and
  // `wall_elapsed` is how long the encoder has been running, both in seconds.
  // A healthy live stream has head_media *behind* wall clock (by the cadence
  // plus encoder startup), so drift is negative. **A positive drift means the
  // media timeline is running ahead of the wall clock, and then every `lag`
  // in the row is an artefact of that, not a latency** -- which is what the
  // first run of this probe produced (-298 ms of "lag", a frame presented
  // before it was captured) and why the number is not allowed to stand alone.
  var head=R.samples.length?R.samples[R.samples.length-1][1]:null;
  var wall_elapsed=(R.t0===null||R.clock_offset_ms===null)?null:
      (performance.timeOrigin+performance.now()+R.clock_offset_ms-R.t0)/1000;
  post({stage:'done',
    head_media:head,wall_elapsed:wall_elapsed,
    drift_ms:(head===null||wall_elapsed===null)?null:
              Math.round((head-wall_elapsed)*1000),
    granularity_ms:pct(steps,0.5)===null?null:+(pct(steps,0.5)*1000).toFixed(1),
    granularity_p95_ms:pct(steps,0.95)===null?null:+(pct(steps,0.95)*1000).toFixed(1),
    steps:steps.length,idle_samples:idle,
    samples:R.samples.length,
    behind_ms:pct(behinds,0.5)===null?null:+(pct(behinds,0.5)*1000).toFixed(0),
    behind_p95_ms:pct(behinds,0.95)===null?null:+(pct(behinds,0.95)*1000).toFixed(0),
    lag_ms:pct(warm,0.5),lag_p95_ms:pct(warm,0.95),lag_n:warm.length,
    lag2_ms:pct(warm2,0.5)===null?null:Math.round(pct(warm2,0.5)),
    lag2_p95_ms:pct(warm2,0.95)===null?null:Math.round(pct(warm2,0.95)),
    lag2_n:warm2.length,
    // Whole-window companions, so that the sum below is checkable: `behind_ms`
    // is also whole-window, and mixing a warm-sliced term into the sum would
    // make the residual a statement about the slicing rather than the stream.
    lag2_all_ms:pct(R.lags2,0.5)===null?null:Math.round(pct(R.lags2,0.5)),
    drift_med_ms:pct(R.drifts,0.5)===null?null:Math.round(pct(R.drifts,0.5)),
    drift_p95_ms:pct(R.drifts,0.95)===null?null:Math.round(pct(R.drifts,0.95)),
    mt_minus_cur_ms:pct(mtd,0.5)===null?null:+pct(mtd,0.5).toFixed(1),
    mt_n:mtd.length,
    seeks:R.seeks,rate_sets:R.rate_sets,
    rate_min:pct(R.rates,0)===null?null:+pct(R.rates,0).toFixed(3),
    rate_p50:pct(R.rates,0.5)===null?null:+pct(R.rates,0.5).toFixed(3),
    rate_max:pct(R.rates,1)===null?null:+pct(R.rates,1).toFixed(3),
    // The identity `behind = lag + drift` is exact only at one instant, and the
    // percentiles above are medians over the window, so these three are posted
    // from the same end-of-run moment to make the residual checkable rather
    // than a matter of taste.
    behind_last_ms:R.behind.length?
        Math.round(R.behind[R.behind.length-1][1]*1000):null,
    lag2_last_ms:R.lags2.length?Math.round(R.lags2[R.lags2.length-1]):null,
    stalls:R.stalls,stall_ms:Math.round(R.stall_ms),
    stalled_events:R.stalled_events||0,hard_stalls:R.hard_stalls||0,
    frames:R.frames,bytes:R.bytes,chunks:R.chunks,
    first_byte_ms:R.first_byte_ms===null?null:Math.round(R.first_byte_ms),
    playing_ms:R.playing_ms===null?null:Math.round(R.playing_ms),
    err:R.err,presents:R.presents.length,
    head_samples:R.samples.slice(0,40)});
}
</script>
"""


def find_first(paths, probe):
    for candidate in paths:
        if not candidate:
            continue
        if probe(candidate):
            return candidate
    return None


def find_ffmpeg():
    def ok(path):
        try:
            return subprocess.run([path, '-version'], capture_output=True,
                                  timeout=20).returncode == 0
        except (OSError, subprocess.SubprocessError):
            return False
    return find_first(FFMPEG_CANDIDATES, ok)


def find_browser():
    return find_first(BROWSER_CANDIDATES,
                      lambda path: os.path.exists(path)
                      and os.access(path, os.X_OK))


def free_port():
    sock = socket.socket()
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


class Run:
    """One (cadence, park, backlog) cell: one ffmpeg, one server, one browser.

    `backlog` is what makes a cell about late joiners rather than about viewers
    who were there from the first byte. Production serves a late joiner
    `init_segment` + the ring tail (`_Broadcaster.tail()`): a parseable start at
    a fragment boundary, then live bytes. This reproduces it by holding back the
    encoder's output for `backlog` seconds and cutting what it held at a `moof`
    boundary before handing it over. Without that knob every cell reports a park
    that never fires -- a playhead that has kept up since t=0 is never more than
    one cadence behind, so `LIVE_EDGE_SECONDS` is a *recovery distance*, not a
    standing cost. That is itself a finding, but not the one that decides how low
    the knob can go.
    """

    def __init__(self, ffmpeg, browser, variant, edge, seconds, capture,
                 backlog=0.0, controller='seek', encoder='x264', verbose=False):
        if controller not in CONTROLLERS:
            raise ValueError('unknown controller {!r} (try {})'.format(
                controller, ', '.join(sorted(CONTROLLERS))))
        #: Validated here rather than left to ffmpeg: an encoder name this
        #: table does not know is a typo, and a typo would otherwise surface as
        #: a dead cell twelve seconds later with the reason buried in stderr.
        if encoder not in ENCODERS:
            raise ValueError('unknown encoder {!r} (try {})'.format(
                encoder, ', '.join(sorted(ENCODERS))))
        self.ffmpeg = ffmpeg
        self.browser = browser
        self.variant = variant
        self.edge = edge
        self.seconds = seconds
        self.capture = capture
        self.backlog = backlog
        self.controller = controller
        self.encoder = encoder
        self.verbose = verbose
        self.result = {}
        self.done = threading.Event()
        #: Set by the `/clock` handler, which also stamps `t0`. The encoder is
        #: spawned only after this fires -- see the comment there for why that
        #: order is the whole difference between a measurement and a browser
        #: launch timer.
        self.page_ready = threading.Event()
        self.t0 = None
        self.proc = None
        self.browser_proc = None
        #: ffmpeg's own words. Empty until the drain thread sees EOF, which is
        #: why `go()` joins that thread before building the row: the first time
        #: a cell died (`--capture 3`, a device index this machine does not
        #: have -- the screen is 2) the row said only `stream ended`, which is
        #: the *page's* complaint about the socket closing, and ffmpeg's
        #: reason was reachable only under `-v`. A probe that reports a failure
        #: it cannot explain gets that failure misread, and this one was: the
        #: run read as "the browser hung up" until the device table was listed.
        self.stderr = ''
        self._stderr_thread = None
        #: The encoder's output, as whole chunks, plus how far `/stream` has
        #: consumed. A condition variable rather than a `deque` + `queue`
        #: because a second viewer is out of scope here and one shared list
        #: keeps the backlog path and the live path literally the same bytes.
        self.chunks = []
        self.cond = threading.Condition()
        self.attn = 0
        self.total = 0                     # bytes produced so far
        self.ci = 0                        # index of the chunk being served
        self.co = 0                        # offset within that chunk
        self.eof = False
        self.held = b''

    # ---- the encoder -------------------------------------------------------
    def argv(self, port):
        gop, movflags, mux_extra = VARIANTS[self.variant]
        #: `-re` is what makes the synthetic source a *live* one: without it
        #: lavfi hands ffmpeg frames as fast as the encoder can take them, the
        #: buffer runs ahead of the wall clock, and every latency number in the
        #: run becomes a measure of how fast this Mac encodes rather than of how
        #: the browser paces a live stream. That is the same offline/live
        #: confusion `encoder_latency_probe.py` exists to keep apart.
        if self.capture:
            source = ['-f', 'avfoundation', '-framerate', str(FPS),
                      '-i', '{}:none'.format(self.capture)]
        else:
            source = ['-re', '-f', 'lavfi',
                      '-i', 'testsrc2=size={}x{}:rate={}'.format(
                          WIDTH, HEIGHT, FPS)]
        #: Everything after the source is the product's own argv, so the
        #: encoder, the `-b:v`/VBV pair, the scaler and `-level` cannot drift
        #: from what ships. Only the two numbers this variant exists to change
        #: are overridden, and `replace_value` raises if the product stopped
        #: emitting either of them -- an override that silently appended a
        #: second flag would still "work", because ffmpeg honours the last one.
        tail = product_tail(self.ffmpeg, ENCODERS[self.encoder])
        tail = replace_value(tail, '-g', str(gop))
        tail = replace_value(tail, '-movflags', movflags)
        if mux_extra:
            #: Muxer private options resolve only after `-f mp4`, so they go
            #: immediately before the output and not up with the encoder flags.
            cut = tail.index('pipe:1')
            tail = tail[:cut] + list(mux_extra) + tail[cut:]
        return ([self.ffmpeg, '-hide_banner', '-loglevel', 'warning',
                 '-nostdin'] + source + tail)

    # ---- the server --------------------------------------------------------
    def handler(self):
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def log_message(self, *args):
                pass

            def _send(self, code, ctype, body=b'', extra=()):
                self.send_response(code)
                self.send_header('Content-Type', ctype)
                self.send_header('Content-Length', str(len(body)))
                self.send_header('Cache-Control', 'no-store')
                for key, value in extra:
                    self.send_header(key, value)
                self.end_headers()
                if body:
                    self.wfile.write(body)

            def do_GET(self):
                path = self.path.split('?')[0]
                if path == '/clock':
                    #: This request *is* the synchronisation point. The browser
                    #: takes a couple of seconds to launch, and if ffmpeg is
                    #: spawned first the encoder writes that whole startup into
                    #: a pipe nobody is reading -- so the page connects onto a
                    #: multi-second backlog it did not ask for, parks, stalls,
                    #: and reports a latency that is the browser's launch time.
                    #: The first run of this probe did exactly that (drift
                    #: -3290 ms, 59 stalls, lag p95 2537 ms on a cell whose real
                    #: cadence was 41.7 ms). Stamping t0 here and spawning the
                    #: encoder afterwards makes `wall_elapsed` and the media
                    #: timeline start at the same instant, which is what lets
                    #: `drift` mean something.
                    outer.t0 = int(time.time() * 1000)
                    outer.page_ready.set()
                    self._send(200, 'application/json',
                               json.dumps({'server_ms': outer.t0,
                                           't0': outer.t0}).encode())
                elif path == '/t0':
                    self._send(200, 'text/plain', str(outer.t0).encode())
                elif path == '/':
                    ctrl = CONTROLLERS[outer.controller]
                    cfg = {'codecs': 'video/mp4; codecs="avc1.640028"',
                           'live_edge': outer.edge,
                           'seconds': outer.seconds,
                           'stall_ms': 8000,
                           'park_mode': outer.controller,
                           'rate_gain': ctrl['gain'],
                           'rate_clamp': ctrl['clamp']}
                    page = PAGE.replace('@CFG@', json.dumps(cfg))
                    self._send(200, 'text/html; charset=utf-8',
                               page.encode('utf-8'),
                               [('X-Content-Type-Options', 'nosniff')])
                elif path == '/stream':
                    self.send_response(200)
                    self.send_header('Content-Type', 'video/mp4')
                    self.send_header('Cache-Control', 'no-store')
                    self.send_header('Transfer-Encoding', 'chunked')
                    self.end_headers()
                    try:
                        first = outer.backlog_bytes()
                        if first:
                            self.wfile.write(
                                '%X\r\n'.encode() % len(first) + first
                                + b'\r\n')
                            self.wfile.flush()
                        while not outer.done.is_set():
                            chunk = outer.next_chunk(timeout=1.0)
                            if chunk is None:
                                break
                            if not chunk:
                                continue
                            self.wfile.write(
                                '%X\r\n'.encode() % len(chunk) + chunk
                                + b'\r\n')
                            self.wfile.flush()
                        self.wfile.write(b'0\r\n\r\n')
                        self.wfile.flush()
                    except (BrokenPipeError, ConnectionResetError, OSError):
                        pass
                else:
                    self._send(404, 'text/plain', b'nope')

            def do_POST(self):
                length = int(self.headers.get('Content-Length') or 0)
                raw = self.rfile.read(length) or b'{}'
                try:
                    data = json.loads(raw)
                except ValueError as exc:
                    data = {'stage': 'error', 'err': 'parse: {!r}'.format(exc)}
                if data.get('stage') == 'error' and not outer.result:
                    outer.result = data
                elif data.get('stage') == 'done':
                    outer.result = data
                    outer.done.set()
                self.send_response(204)
                self.send_header('Content-Length', '0')
                self.end_headers()

        return H

    def _pump(self):
        """Drain ffmpeg's stdout into `chunks` so `/stream` never blocks on it.

        A reader thread rather than reading in the handler: the handler must be
        able to serve `/clock` and the POST-back while the encoder is running,
        and a pipe read inside the handler would leave nothing to hold back for
        the late-joiner path.
        """
        try:
            while True:
                #: `read1`, not `read`. `BufferedReader.read(65536)` on a pipe
                #: keeps issuing raw reads until it *has* 65536 bytes, which
                #: silently batches ~4 per-frame fragments into one HTTP chunk --
                #: and the first run of this probe reported `every_frame` as
                #: 166.7 ms of granularity (4 frames at 24 fps) for exactly that
                #: reason. The muxer was cutting per frame the whole time (96
                #: `moof` boxes for 96 frames); the probe was the thing that
                #: batched. A latency probe that changes the cadence it is
                #: measuring is measuring itself.
                chunk = self.reader.read1(65536)
                if not chunk:
                    break
                with self.cond:
                    self.chunks.append(chunk)
                    self.total += len(chunk)
                    self.cond.notify_all()
        except (OSError, ValueError, AttributeError):
            pass
        with self.cond:
            self.eof = True
            self.cond.notify_all()

    def next_chunk(self, timeout):
        """The next unconsumed bytes, b'' while waiting, None at end of stream.

        Booked as (index into `chunks`, offset within that chunk) rather than as
        a byte offset into a joined copy. The joined copy is what the second
        version of this method did -- `b''.join(self.chunks)` on every call --
        and at 3 Mbps over a 12 s window that is ~4.5 MB re-copied a few
        hundred times, i.e. the *sender* became the bottleneck: every cell then
        reported the same 41.7 ms granularity, ~2.2 s of drift and 65-71 stalls
        regardless of what the muxer was doing. A probe that cannot keep up with
        real time measures the probe.
        """
        with self.cond:
            if not self.cond.wait_for(lambda: self.total > self.attn
                                      or self.eof, timeout=timeout):
                return b''
            if self.ci >= len(self.chunks):
                return None if self.eof else b''
            chunk = self.chunks[self.ci]
            piece = chunk[self.co:]
            self.attn += len(piece)
            self.ci += 1
            self.co = 0
            return piece

    def backlog_bytes(self):
        """What a late joiner gets first: held bytes, cut at a fragment border.

        Production gets this for free because `_Broadcaster` rings *whole framed
        units*; here it is one box walk. Two cuts matter:

        * the head must start at ftyp+moov (the init segment), so it is kept
          whole and the media part must begin on a `moof`;
        * the tail must end on a box boundary, or MSE is handed a range that
          stops inside an `mdat`. Anything past the last complete box stays
          unconsumed (`self.attn`) and goes out on the live side instead, so
          nothing is sent twice and nothing is dropped.

        If no `moof` is found the honest answer is to send nothing and let the
        live path serve the whole stream -- half a fragment is worse than no
        backlog, because it makes MSE reject the append and the run reports a
        stall that is the probe's fault.
        """
        if not self.backlog:
            return b''
        deadline = time.time() + self.backlog + 20
        with self.cond:
            while time.time() < deadline and not self.eof:
                if self.total and (time.time() - self.t0 / 1000.0
                                   ) >= self.backlog:
                    break
                self.cond.wait(0.2)
            data = b''.join(self.chunks)
        cut = data.find(b'moof', 8)
        if cut < 4:
            return b''
        head_end = cut - 4                 # the moof box's own size field
        end = self._last_complete_box(data, head_end)
        if end <= head_end:
            return b''
        with self.cond:
            #: Move the live cursor to the byte the backlog stopped at, so the
            #: joiner gets the remainder of that chunk on the live side rather
            #: than twice or not at all.
            walked = 0
            self.ci = 0
            self.co = 0
            for index, chunk in enumerate(self.chunks):
                if walked + len(chunk) <= end:
                    walked += len(chunk)
                    self.ci = index + 1
                    self.co = 0
                    continue
                self.ci = index
                self.co = end - walked
                break
            self.attn = end
            self.backlog_bytes_n = end
        return data[:end]

    @staticmethod
    def _last_complete_box(data, start):
        """End offset of the last complete top-level box at or after `start`."""
        pos = start
        while pos + 8 <= len(data):
            size = int.from_bytes(data[pos:pos + 4], 'big')
            if size == 1:
                if pos + 16 > len(data):
                    break
                size = int.from_bytes(data[pos + 8:pos + 16], 'big')
            elif size == 0:
                break                      # "to end of file": not a live shape
            if size < 8 or pos + size > len(data):
                break
            pos += size
        return pos

    # ---- the run -----------------------------------------------------------
    def go(self):
        port = free_port()
        server = http.server.ThreadingHTTPServer(('127.0.0.1', port),
                                                 self.handler())
        threading.Thread(target=server.serve_forever, daemon=True).start()
        argv = self.argv(port)
        if self.verbose:
            print('  ffmpeg:', ' '.join(argv[1:]))
        profile = '/tmp/mse_probe_profile'
        browser_argv = [
            self.browser, '--user-data-dir=' + profile,
            'http://127.0.0.1:{}/'.format(port),
            '--headless=new', '--disable-gpu', '--no-sandbox',
            '--mute-audio', '--autoplay-policy=no-user-gesture-required',
            '--no-first-run', '--no-default-browser-check',
            '--window-size={},{}'.format(WIDTH, HEIGHT),
            '--disable-extensions', '--disable-background-timer-throttling',
            '--disable-renderer-backgrounding',
        ]
        env = dict(os.environ)
        for key in ('http_proxy', 'https_proxy', 'HTTP_PROXY', 'HTTPS_PROXY',
                    'all_proxy', 'ALL_PROXY', 'PYTHONPATH'):
            env.pop(key, None)
        try:
            self.browser_proc = subprocess.Popen(
                browser_argv, env=env, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
        except OSError as exc:
            server.shutdown()
            return {'stage': 'error', 'err': 'spawn browser: {!r}'.format(exc)}
        #: Browser first, encoder second. `/clock` stamps `t0` and sets
        #: `page_ready`, so waiting on it here is what keeps the browser's own
        #: launch time out of the measurement (see the comment in the handler).
        if not self.page_ready.wait(40):
            self._kill()
            server.shutdown()
            return {'stage': 'error',
                    'err': 'the page never asked for /clock -- the browser '
                           'did not load it'}
        try:
            self.proc = subprocess.Popen(argv, stdout=subprocess.PIPE,
                                         stderr=subprocess.PIPE)
        except OSError as exc:
            self._kill()
            server.shutdown()
            return {'stage': 'error', 'err': 'spawn ffmpeg: {!r}'.format(exc),
                    'argv': argv}
        self.reader = self.proc.stdout
        threading.Thread(target=self._pump, daemon=True).start()
        self._stderr_thread = threading.Thread(target=self._drain_stderr,
                                               daemon=True)
        self._stderr_thread.start()
        deadline = time.time() + self.seconds + 45
        while time.time() < deadline and not self.done.is_set():
            if self.result.get('stage') == 'error':
                break
            if self.proc.poll() is not None and self.done.wait(2.0):
                break
            self.done.wait(0.2)
        self.done.set()
        self._kill()
        #: Join before reading `self.stderr`: the drain thread only assigns it
        #: at EOF, and `_kill()` terminating the process is what produces EOF.
        #: Two seconds is generous for a pipe that is already closed.
        if self._stderr_thread is not None:
            self._stderr_thread.join(2.0)
        server.shutdown()
        out = dict(self.result)
        out.setdefault('stage', 'timeout')
        #: ffmpeg's exit status and the last of its output travel with the row,
        #: so a dead cell is reported as "the encoder died and here is what it
        #: said" rather than as whatever the page noticed. `None` means the
        #: encoder was never spawned (browser-side failure), which is a
        #: different fact from "spawned and exited 0".
        out['ffmpeg_rc'] = self.proc.poll() if self.proc else None
        out['ffmpeg_tail'] = [line[:200]
                              for line in self.stderr.splitlines()
                              if line.strip()][-4:]
        out['variant'] = self.variant
        out['live_edge'] = self.edge
        out['controller'] = self.controller
        #: Recorded per row, not just in the header: the whole point of adding
        #: the encoder table was that rows taken under libx264 had been read as
        #: statements about the shipped (VideoToolbox) configuration, and a JSON
        #: file outlives the terminal it was printed to.
        out['encoder'] = self.encoder
        out['backlog'] = self.backlog
        out['seconds'] = self.seconds
        out['wall_seconds'] = round(time.time() - self.t0 / 1000.0, 1) \
            if self.t0 else None
        #: bytes/s over the measured window, so the wire cost of a cadence is
        #: quoted in the same unit the queue budget is (`LIVE_QUEUE_SECONDS`).
        if out.get('bytes') and self.seconds:
            out['bytes_per_s'] = int(out['bytes'] / self.seconds)
        return out

    def _drain_stderr(self):
        try:
            text = self.proc.stderr.read()
        except (OSError, ValueError):
            return
        if text:
            self.stderr = text.decode('utf-8', 'replace')
            if self.verbose:
                for line in self.stderr.splitlines()[:12]:
                    print('    ffmpeg:', line[:160])

    def _kill(self):
        for proc in (self.browser_proc, self.proc):
            if proc is None or proc.poll() is not None:
                continue
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


def lag_of(row):
    """The absolute lag a row reported, or None.

    Kept as a named accessor because "no lag number" and "0 ms of lag" are
    different statements and a table that prints 0 for both is a lie in the
    direction that flatters us.
    """
    value = row.get('lag_ms')
    return None if value is None else int(round(value))


def lag2_of(row):
    """The playhead-based lag (`wall - t0 - currentTime`), or None.

    The counterpart to `lag_of`, which is derived from rVFC's `mediaTime`. Two
    measures of one quantity exist because they disagreed by 258 ms on the
    shipping shape and the disagreement has to be resolved before either number
    can be quoted in a design document.
    """
    value = row.get('lag2_ms')
    return None if value is None else int(round(value))


def identity_residual(row):
    """`behind_last - (lag2_last + drift)`, in ms, or None.

    Arithmetic on the page's own numbers taken at one instant, so the answer is
    either ~0 -- in which case `behind`, `lag2` and `drift` are the same story
    told three ways and the row is usable -- or it is not, and the row is
    unusable regardless of how plausible any single number in it looks.
    """
    triple = (row.get('behind_last_ms'), row.get('lag2_last_ms'),
              row.get('drift_ms'))
    if any(value is None for value in triple):
        return None
    behind, lag2, drift = (int(value) for value in triple)
    return behind - (lag2 + drift)


def quote(row):
    """Which lag this row's `lag` number should be, as a string.

    Says so when the two measures agree, and says *why not* when they do not,
    rather than printing the flattering one.
    """
    one, two = lag_of(row), lag2_of(row)
    if one is None or two is None:
        return 'n/a'
    offset = row.get('mt_minus_cur_ms')
    if abs(one - two) <= 40:
        return '{} ms'.format(one)
    detail = ('rVFC mediaTime runs {:+.0f} ms vs the playhead'
              .format(offset) if offset is not None
              else 'rVFC vs playhead offset unreported')
    return '{} vs {} ms ({})'.format(one, two, detail)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--list', action='store_true',
                        help='print the cadence variants and exit')
    parser.add_argument('--quick', action='store_true',
                        help='one cell: the shipping shape at its shipping park')
    parser.add_argument('--variants', default='',
                        help='comma-separated cadence names (default: matrix)')
    parser.add_argument('--edges', default='',
                        help='comma-separated park distances in seconds')
    parser.add_argument('--seconds', type=float, default=15.0,
                        help='measured window per cell (default 15)')
    parser.add_argument('--capture', default='',
                        help='avfoundation device index instead of testsrc2 '
                             '(needs screen-recording permission for whatever '
                             'runs this script)')
    parser.add_argument('--backlog', type=float, default=0.0,
                        help='seconds of encoder output to hand a viewer on '
                             'connect, reproducing a late joiner (production '
                             'serves init segment + the ring tail). 0 means a '
                             'viewer present from the first byte, where the '
                             'park never fires.')
    parser.add_argument('--controller', default='seek',
                        choices=sorted(CONTROLLERS),
                        help='how the playhead is held at the live edge: '
                             '`seek` jumps it (what production does today), '
                             '`rate` trims with playbackRate and seeks at most '
                             'once')
    parser.add_argument('--encoder', default='x264', choices=sorted(ENCODERS),
                        help='which encoder argv to use, transcribed from '
                             'screen_mirror.encoder_args(): `x264` is the '
                             'software branch, `vt` is what `auto` picks on a '
                             'Mac. `vt` asks for 1.5x the bitrate so both land '
                             'at the same wire rate.')
    parser.add_argument('--json', default='', help='write raw rows here')
    parser.add_argument('-v', '--verbose', action='store_true')
    args = parser.parse_args()

    if args.list:
        print('{:<12} {:>4}  {:<48} {}'.format(
            'variant', 'gop', 'movflags', 'muxer options'))
        for name, (gop, flags, extra) in VARIANTS.items():
            print('{:<12} {:>4}  {:<48} {}'.format(
                name, gop, flags, ' '.join(extra)))
        print()
        print('{:<12} {:>6} {:>7}'.format('controller', 'gain', 'clamp'))
        for name, ctrl in CONTROLLERS.items():
            print('{:<12} {:>6} {:>7}'.format(
                name, ctrl['gain'], ctrl['clamp']))
        print()
        #: `--list` runs before ffmpeg is resolved, and the argv is now the
        #: product's own, so ask it with a placeholder path. It is only ever
        #: substituted, never executed, from here.
        print('{:<12} {:>10}  {}'.format('encoder', 'product', 'argv (from -vf)'))
        for name, kind in ENCODERS.items():
            tail = product_tail('ffmpeg', kind)
            print('{:<12} {:>10}  {}'.format(
                name, kind, ' '.join(tail[tail.index('-vf'):])))
        return 0

    ffmpeg = find_ffmpeg()
    browser = find_browser()
    if not ffmpeg:
        print('no ffmpeg: tried {}'.format(
            ', '.join(c for c in FFMPEG_CANDIDATES if c)), file=sys.stderr)
        return 2
    if not browser:
        print('no headless browser: tried {}'.format(
            ', '.join(c for c in BROWSER_CANDIDATES if c)), file=sys.stderr)
        print('  set MACAST_PROBE_BROWSER=/path/to/chrome', file=sys.stderr)
        return 2
    print('ffmpeg : {}'.format(ffmpeg))
    print('browser: {}'.format(browser))
    print('source : {}'.format(
        'avfoundation {}'.format(args.capture) if args.capture
        else 'testsrc2 {}x{}@{} (synthetic, -re)'.format(WIDTH, HEIGHT, FPS)))
    print('window : {} s per cell'.format(args.seconds))
    ctrl = CONTROLLERS[args.controller]
    print('park   : {} controller (gain {}, clamp {}); park distance is '
          'per cell'.format(args.controller, ctrl['gain'], ctrl['clamp']))
    #: Named in the header because a latency row without its encoder is not a
    #: latency row: `auto` on a Mac picks VideoToolbox, and its ~200 ms
    #: first-frame cost is inside the "upstream" term these cells report.
    #: Printed from the derived argv rather than restated, so the header cannot
    #: claim a rate control the encoder is not actually getting -- which is
    #: what it did for the whole of the first four matrices.
    tail = product_tail(ffmpeg, ENCODERS[args.encoder])
    print('encoder: {} (product encoder={!r})'.format(
        args.encoder, ENCODERS[args.encoder]))
    print('argv   : {}'.format(' '.join(tail[tail.index('-vf'):])))
    print('backlog: {} s{}'.format(
        args.backlog,
        '' if args.backlog else
        '  (a viewer present from the first byte -- the park will not fire; '
        'pass --backlog to measure a late joiner)'))
    print()

    if args.quick:
        matrix = [('every12', 1.0)]
    elif args.variants:
        names = [n.strip() for n in args.variants.split(',') if n.strip()]
        unknown = [n for n in names if n not in VARIANTS]
        if unknown:
            print('unknown variant(s): {} (try --list)'.format(
                ', '.join(unknown)), file=sys.stderr)
            return 2
        edges = ([float(e) for e in args.edges.split(',') if e.strip()]
                 or [1.0, 0.5, 0.1])
        matrix = [(n, e) for n in names for e in edges]
    else:
        matrix = list(DEFAULT_MATRIX)
        if args.edges:
            edges = [float(e) for e in args.edges.split(',') if e.strip()]
            matrix = [(n, e) for n, _ in matrix for e in edges]

    rows = []
    for index, (name, edge) in enumerate(matrix, 1):
        print('[{}/{}] {} @ park {} s'.format(
            index, len(matrix), name, edge), flush=True)
        run = Run(ffmpeg, browser, name, edge, args.seconds, args.capture,
                  backlog=args.backlog, controller=args.controller,
                  encoder=args.encoder, verbose=args.verbose)
        row = run.go()
        rows.append(row)
        if row.get('stage') != 'done':
            print('   FAILED: {} {}'.format(row.get('stage'), row.get('err')))
            #: Name whose failure this is. `ffmpeg_rc` of 0 with a dead cell
            #: means the encoder finished and the page gave up (a short stream,
            #: a stall past `STALL_MS`); a non-zero rc means the encoder never
            #: produced what was asked for, and its own last lines are the
            #: only account of why.
            print('   ffmpeg exited {} ({}); last of its output:'.format(
                row.get('ffmpeg_rc'),
                'never spawned' if row.get('ffmpeg_rc') is None
                else 'ok' if row.get('ffmpeg_rc') == 0 else 'FAILED'))
            for line in row.get('ffmpeg_tail') or ['(nothing on stderr)']:
                print('     | {}'.format(line))
            continue
        print('   granularity p50 {:>7} ms  p95 {:>7} ms   '
              'behind p50 {:>5} ms p95 {:>5} ms'.format(
                  row.get('granularity_ms'), row.get('granularity_p95_ms'),
                  row.get('behind_ms'), row.get('behind_p95_ms')))
        print('   lag {}   (rVFC n={}, playhead n={})'.format(
            quote(row), row.get('lag_n'), row.get('lag2_n')))
        #: `drift` decides whether the `lag` on the line above means anything.
        #: Positive = the media timeline ran ahead of the wall clock, so the
        #: "lag" is an artefact; say so instead of printing a number.
        drift = row.get('drift_ms')
        if drift is not None and drift > 0:
            print('   !! drift +{} ms: media timeline ahead of wall clock, '
                  'so lag_ms above is NOT a latency'.format(drift))
        else:
            print('   drift {} ms  (head_media {} s vs wall {} s)'.format(
                drift, row.get('head_media'), row.get('wall_elapsed')))
        #: `drift_ms` is one sample, taken at the end of the window. `behind`
        #: and `lag` are medians over the whole window, and reconciling them
        #: needs the median drift, not that one sample: with a 0.5 s cadence the
        #: instantaneous value swings by a full cadence, so the end-of-window
        #: reading is essentially arbitrary.
        dmed = row.get('drift_med_ms')
        l2all = row.get('lag2_all_ms')
        beh = row.get('behind_ms')
        if None in (dmed, l2all, beh):
            print('   !! cannot reconcile behind/lag: terms missing')
        else:
            print('   median drift {} ms (p95 {}), so behind {} = lag2 {} + '
                  'drift {}  (residual {} ms)'.format(
                      dmed, row.get('drift_p95_ms'), beh, l2all, dmed,
                      beh - (l2all + dmed)))
        residual = identity_residual(row)
        if residual is None:
            print('   !! identity not checkable: behind/lag2/drift incomplete')
        elif abs(residual) > 40:
            print('   !! behind != lag + drift by {} ms: this row does not '
                  'describe one stream'.format(residual))
        else:
            print('   last-instant identity behind = lag2 + drift holds '
                  '({} ms residual)'.format(residual))
        print('   stalls {} ({:.0f} ms)  hard_stalls {}  frames {}  '
              'first_byte {} ms  {:.0f} kbps'.format(
                  row.get('stalls'), row.get('stall_ms') or 0,
                  row.get('hard_stalls'), row.get('frames'),
                  row.get('first_byte_ms'),
                  (row.get('bytes_per_s') or 0) * 8 / 1000.0))
        #: The controller's own activity. A `seek` row that seeks hundreds of
        #: times is a row whose `behind` is being held down by force, and the
        #: stall count next to it is the price; a `rate` row should seek once.
        if args.controller == 'rate':
            print('   controller: {} seeks, {} rate sets, playbackRate '
                  '{} / {} / {} (min/p50/max)'.format(
                      row.get('seeks'), row.get('rate_sets'),
                      row.get('rate_min'), row.get('rate_p50'),
                      row.get('rate_max')))
        else:
            print('   controller: {} seeks'.format(row.get('seeks')))
        steps = row.get('idle_samples') or 0
        total = (row.get('steps') or 0) + steps
        if total:
            print('   buffer head moved in {}/{} samples '
                  '({:.0f}% idle)'.format(row.get('steps'), total,
                                          100.0 * steps / total))

    print()
    print('=== {} cells ==='.format(len(rows)))
    #: `lag2` is listed before `lag` because it is the measure the identity
    #: check can vouch for; `lag` (rVFC `mediaTime`) is kept beside it with the
    #: `mt-cur` offset that explains any disagreement. A row where the two
    #: differ and the offset is ~0 is a row that does not describe one stream.
    header = ('{:>12} {:>5} {:>5} {:>4} {:>9} {:>9} {:>7} {:>7} {:>9} {:>7} '
              '{:>8} {:>9} {:>6} {:>5} {:>8} {:>8}'.format(
                  'variant', 'park', 'ctrl', 'enc', 'gran_p50', 'gran_p95',
                  'beh_p50', 'beh_p95', 'lag2_p50', 'lag_p50', 'mt-cur',
                  'drift_med', 'seeks', 'stall', 'stall_ms', 'kbps'))
    print(header)

    def cell(value, width):
        #: The width is baked in with `%` first: `'{:>%d}'.format(w, v)` would
        #: read `%d` as a *format spec* and raise at print time.
        return ('{:>%d}' % width).format('-' if value is None else value)

    for row in rows:
        if row.get('stage') != 'done':
            print('{:>12} {:>5}  FAILED {}'.format(
                row.get('variant'), row.get('live_edge'), row.get('stage')))
            continue
        offset = row.get('mt_minus_cur_ms')
        print('{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}'.format(
            '{:>12}'.format(row['variant']),
            cell(row.get('live_edge'), 5),
            cell((row.get('controller') or '')[:5], 5),
            cell((row.get('encoder') or '')[:4], 4),
            cell(row.get('granularity_ms'), 9),
            cell(row.get('granularity_p95_ms'), 9),
            cell(row.get('behind_ms'), 7),
            cell(row.get('behind_p95_ms'), 7),
            cell(lag2_of(row), 9),
            cell(lag_of(row), 7),
            cell(None if offset is None else round(offset), 8),
            cell(row.get('drift_med_ms'), 9),
            cell(row.get('seeks'), 6),
            cell(row.get('stalls'), 5),
            cell(row.get('stall_ms') or 0, 8),
            '{:>8.0f}'.format(
                (row.get('bytes_per_s') or 0) * 8 / 1000.0)))
    if args.json:
        with open(args.json, 'w', encoding='utf-8') as handle:
            json.dump(rows, handle, indent=2, ensure_ascii=False)
        print('\nraw rows -> {}'.format(args.json))
    return 0


if __name__ == '__main__':
    sys.exit(main())
