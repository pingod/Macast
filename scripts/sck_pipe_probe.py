#!/usr/bin/env python3
# ffmpeg pipe-contract probe for the ScreenCaptureKit integration (batch 3-3).
#
# The SCK feeder hands ffmpeg two pipes this plugin has never used before:
#
#   * video: rawvideo NV12 on stdin, frames arriving at SCK's measured ~23.3/s
#     cadence while the output claims 24. The integration's answer is
#     `-use_wallclock_as_timestamps 1`: stamp each frame with its arrival time
#     so the output `-r 24` duplicates/drops against the wall clock instead of
#     trusting a frame counter that runs slow. That claim is checked here --
#     a stubbed ffmpeg in the suite can only prove the argv SHAPE, not that
#     the real demuxer behaves.
#
#   * audio: float32 PCM on a second `pipe:N` input, N handed over through
#     subprocess `pass_fds`; the parent closes its copy of the read end right
#     after Popen (the shape the product will use).
#
# Both checks run the same two-input command shape the plugin will build.
# Numbers printed are the evidence; exit 0 = both questions answered.
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/sck_pipe_probe.py
#
# Expectations it verifies (tolerances printed):
#   1. WITHOUT wallclock, 65 frames written at 43 ms cadence (= 2.795 s wall)
#      mux out to 65/24 = 2.708 s -- the timeline drifts ~87 ms behind wall.
#      WITH wallclock, the same write muxes to ~2.795 s (filled to CFR).
#   2. The audio input on pipe:N lands as a stream whose duration matches the
#      wall span within two 20 ms audio chunks.

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import threading
import time

FPS = 24
W, H = 320, 180
FRAME_MS = 43.0          # SCK's measured cadence: 140 frames / 6.014 s
FRAMES = 65              # -> 2.795 s of wall clock at 43 ms
AUDIO_CHUNK = 7680       # 960 samples x 2 ch x 4 B, one 20 ms SCK buffer


def nv12_frame(i: int) -> bytes:
    row = bytes(((x * 3 + i * 5) & 0xFF) for x in range(W))
    return row * H + b"\x80" * (W * H // 2)


def _ffprobe_stream(ffprobe: str, kind: str, entry: str, path: str,
                    extra: list[str] | None = None) -> str:
    r = subprocess.run(
        [ffprobe, "-v", "error", *(extra or []), "-select_streams", kind,
         "-show_entries", entry, "-of", "csv=p=0", path],
        capture_output=True, text=True, timeout=30)
    return r.stdout.strip().splitlines()[0] if r.stdout.strip() else ""


def _ffprobe_last_pts(ffprobe: str, kind: str, path: str) -> float:
    """The last packet's presentation time (Matroska has no stream duration)."""
    r = subprocess.run(
        [ffprobe, "-v", "error", "-select_streams", kind,
         "-show_entries", "packet=pts_time", "-of", "csv=p=0", path],
        capture_output=True, text=True, timeout=60)
    vals = []
    for line in r.stdout.splitlines():
        line = line.strip().rstrip(",")
        try:
            vals.append(float(line))
        except ValueError:
            continue
    return max(vals) if vals else float("nan")


def _probe(binp: str, ffprobe: str, wallclock: bool) -> dict:
    out = f"/tmp/sck_pipe_{'wc' if wallclock else 'cnt'}.mkv"
    argv = [binp, "-hide_banner", "-loglevel", "warning", "-nostdin",
            "-f", "rawvideo", "-pix_fmt", "nv12", "-s", f"{W}x{H}",
            "-framerate", str(FPS)]
    if wallclock:
        argv += ["-use_wallclock_as_timestamps", "1"]
    argv += ["-i", "-"]
    audio_r, audio_w = os.pipe()
    argv += ["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", f"pipe:{audio_r}"]
    argv += ["-map", "0:v:0", "-map", "1:a:0",
             "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
             "-r", str(FPS), "-g", "12",
             "-c:a", "aac", "-b:a", "128k", "-ar", "48000", "-ac", "2",
             "-f", "matroska", "-y", out]
    proc = subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE, pass_fds=(audio_r,))
    os.close(audio_r)          # the child holds its own copy now

    stop = threading.Event()
    wrote = [0]

    def feed_audio():
        nxt = time.monotonic()
        silence = bytes(AUDIO_CHUNK)
        while not stop.is_set():
            try:
                os.write(audio_w, silence)
            except OSError:
                return
            wrote[0] += AUDIO_CHUNK
            nxt += 0.020
            delay = nxt - time.monotonic()
            if delay > 0:
                stop.wait(delay)

    feeder = threading.Thread(target=feed_audio, daemon=True)
    feeder.start()
    t0 = time.monotonic()
    for i in range(FRAMES):
        try:
            proc.stdin.write(nv12_frame(i))
            proc.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            print(f"[pipe] video write failed at frame {i}: {exc!r}", flush=True)
            break
        delay = t0 + (i + 1) * FRAME_MS / 1000.0 - time.monotonic()
        if delay > 0:
            time.sleep(delay)
    wall = time.monotonic() - t0
    time.sleep(FRAME_MS / 1000.0)          # let audio cover the same span
    stop.set()
    feeder.join(timeout=2.0)
    try:
        proc.stdin.close()
    except OSError:
        pass
    try:
        os.close(audio_w)
    except OSError:
        pass
    try:
        rc = proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        rc = proc.wait()
    stderr = proc.stderr.read().decode(errors="replace") if proc.stderr else ""
    return {
        "wall": wall, "rc": rc, "stderr": stderr, "audio_bytes": wrote[0],
        "v_last": _ffprobe_last_pts(ffprobe, "v:0", out),
        "a_last": _ffprobe_last_pts(ffprobe, "a:0", out),
        "v_frames": _ffprobe_stream(ffprobe, "v:0", "stream=nb_read_frames",
                                    out, extra=["-count_frames"]),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ffmpeg", default=None)
    ap.add_argument("--ffprobe", default=None)
    args = ap.parse_args()
    binp = (args.ffmpeg or os.environ.get("SCK_PROBE_FFMPEG")
            or shutil.which("ffmpeg")
            or "/opt/homebrew/opt/ffmpeg/bin/ffmpeg")
    ffprobe = (args.ffprobe or os.environ.get("SCK_PROBE_FFPROBE")
               or shutil.which("ffprobe")
               or "/opt/homebrew/opt/ffmpeg/bin/ffprobe")
    if not shutil.which(binp) and not os.path.exists(binp):
        print(f"no ffmpeg at {binp}", file=sys.stderr)
        return 2

    expect_wall = FRAMES * FRAME_MS / 1000.0
    counted = None
    ok = True
    for wallclock in (False, True):
        r = _probe(binp, ffprobe, wallclock)
        tag = "wallclock" if wallclock else "counted  "
        v_end = r["v_last"] + 1.0 / FPS          # last pts + one frame
        a_end = r["a_last"] + 0.021              # last pts + one AAC frame
        print(f"[pipe] {tag}: wall={r['wall']:.3f}s ffmpeg_rc={r['rc']} "
              f"video_end={v_end:.3f}s ({r['v_frames']} frames) "
              f"audio_end={a_end:.3f}s audio_written={r['audio_bytes']}B", flush=True)
        for line in r["stderr"].strip().splitlines()[-4:]:
            print(f"[ffmpeg] {line}", flush=True)
        if not wallclock:
            counted = v_end
        else:
            close = abs(v_end - expect_wall) < 0.06
            print(f"[pipe] wallclock video_end vs wall {expect_wall:.3f}s: "
                  f"{'MATCH' if close else 'MISMATCH'}", flush=True)
            ok = ok and close
            a_close = abs(a_end - expect_wall) < 0.12
            print(f"[pipe] audio_end vs wall: "
                  f"{'MATCH' if a_close else 'MISMATCH'}", flush=True)
            ok = ok and a_close
    if counted is not None:
        print(f"[pipe] counted-timestamps video_end={counted:.3f}s vs wallclock "
              f"{expect_wall:.3f}s -> wallclock moved the timeline by "
              f"{expect_wall - counted:+.3f}s", flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
