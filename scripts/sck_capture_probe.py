#!/usr/bin/env python3
# ScreenCaptureKit capture probe -- the feasibility spike behind batch 3-3.
#
# Batch 3-2 moved the Windows picture onto ddagrab. This probe asks the same
# question for the Mac: can Macast's capture leave `-f avfoundation` behind?
# The prize is not the pixels (avfoundation works); it is `capturesAudio`,
# which Apple made native for SCK on macOS 13 -- retiring BlackHole and the
# CoreAudio aggregate-device dance entirely (research doc 4.3).
#
# Questions, answered in order, printed as evidence:
#   1. Can a plain .venv interpreter (Screen Recording TCC already granted to
#      it for avfoundation) list shareable content and receive SCK frames?
#   2. What are the frames' shape and cadence -- pixel format, bytes per row,
#      inter-frame gaps, and what does a static display actually deliver?
#   3. Does `capturesAudio` deliver system audio, and at what byte rate?
#   4. How far behind the wall clock is the first frame (start -> handler) --
#      the number research doc 7.8 says is missing before any SCK rewrite.
#
# Everything here is a probe: it prints what it sees, tolerates what it cannot
# read (pyobjc has no CoreVideo bindings; pixels are reached through ctypes,
# the same habit the plugin already has for CoreAudio), and exits non-zero
# only when a *question* could not be asked. The numbers are allowed to be
# ugly -- they are the point.
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/sck_capture_probe.py \
#       --seconds 6 --fps 24 --scale 1920x1080 --play-sound
#
# Exit codes: 0 all questions answered; 2 no video frames arrived (TCC or
# pipeline); 3 frames arrived but audio did not (capturesAudio broken here).

from __future__ import annotations

import argparse
import ctypes
import os
import re
import shutil
import subprocess
import sys
import threading
import time

try:
    import objc
    import Foundation
    import CoreMedia
    import ScreenCaptureKit as SCK
except Exception as _exc:  # pragma: no cover - environment gate
    sys.exit(
        f"this probe needs the .venv pyobjc stack "
        f"(pyobjc-framework-ScreenCaptureKit): {_exc}"
    )

# ---------------------------------------------------------------------------
# CoreVideo / CoreMedia through ctypes. pyobjc-framework-Quartz (which carries
# the CoreVideo bindings) is NOT installed and this probe deliberately does
# not add it -- `ctypes.CDLL` on the absolute framework path is the same
# zero-dependency route screen_mirror already uses for CoreAudio.
# ---------------------------------------------------------------------------

_CV = ctypes.CDLL("/System/Library/Frameworks/CoreVideo.framework/CoreVideo")
_CM = ctypes.CDLL("/System/Library/Frameworks/CoreMedia.framework/CoreMedia")

_CV.CVPixelBufferGetWidth.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetWidth.restype = ctypes.c_size_t
_CV.CVPixelBufferGetHeight.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetHeight.restype = ctypes.c_size_t
_CV.CVPixelBufferGetBytesPerRow.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetBytesPerRow.restype = ctypes.c_size_t
_CV.CVPixelBufferGetPixelFormatType.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetPixelFormatType.restype = ctypes.c_uint32
_CV.CVPixelBufferLockBaseAddress.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
_CV.CVPixelBufferLockBaseAddress.restype = ctypes.c_int32
_CV.CVPixelBufferUnlockBaseAddress.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
_CV.CVPixelBufferUnlockBaseAddress.restype = ctypes.c_int32
_CV.CVPixelBufferGetBaseAddress.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetBaseAddress.restype = ctypes.c_void_p
_CV.CVPixelBufferIsPlanar.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferIsPlanar.restype = ctypes.c_int32
_CV.CVPixelBufferGetPlaneCount.argtypes = [ctypes.c_void_p]
_CV.CVPixelBufferGetPlaneCount.restype = ctypes.c_size_t
_CV.CVPixelBufferGetBaseAddressOfPlane.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
_CV.CVPixelBufferGetBaseAddressOfPlane.restype = ctypes.c_void_p
_CV.CVPixelBufferGetBytesPerRowOfPlane.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
_CV.CVPixelBufferGetBytesPerRowOfPlane.restype = ctypes.c_size_t

_CM.CMSampleBufferGetImageBuffer.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetImageBuffer.restype = ctypes.c_void_p
_CM.CMSampleBufferGetTotalSampleSize.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetTotalSampleSize.restype = ctypes.c_size_t
_CM.CMSampleBufferGetDataBuffer.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetDataBuffer.restype = ctypes.c_void_p
_CM.CMBlockBufferGetDataLength.argtypes = [ctypes.c_void_p]
_CM.CMBlockBufferGetDataLength.restype = ctypes.c_size_t
_CM.CMBlockBufferGetDataPointer.argtypes = [
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
    ctypes.POINTER(ctypes.c_size_t),
    ctypes.POINTER(ctypes.c_void_p),
]
_CM.CMBlockBufferGetDataPointer.restype = ctypes.c_int32


class _CMTime(ctypes.Structure):
    """CMTime, as the caller sees it (struct returned by value)."""

    _fields_ = [
        ("value", ctypes.c_int64),
        ("timescale", ctypes.c_int32),
        ("flags", ctypes.c_uint32),
        ("epoch", ctypes.c_int64),
    ]


_CM.CMSampleBufferGetNumSamples.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetNumSamples.restype = ctypes.c_int64
_CM.CMSampleBufferGetPresentationTimeStamp.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetPresentationTimeStamp.restype = _CMTime
_CM.CMSampleBufferGetDuration.argtypes = [ctypes.c_void_p]
_CM.CMSampleBufferGetDuration.restype = _CMTime


def _cmtime_seconds(t: _CMTime) -> float:
    if t.timescale <= 0:
        return float("nan")
    return t.value / t.timescale


def _ptr(obj) -> int | None:
    """The raw pointer behind a pyobjc-wrapped CF object, or None."""
    try:
        return objc.pyobjc_id(obj)
    except Exception:
        return None


def _read_frame(sbuf) -> dict | None:
    """Pixel geometry of a video sample buffer, read through ctypes."""
    sbuf_ptr = _ptr(sbuf)
    if sbuf_ptr is None:
        return None
    pb = _CM.CMSampleBufferGetImageBuffer(ctypes.c_void_p(sbuf_ptr))
    if not pb:
        return None
    info = {
        "width": _CV.CVPixelBufferGetWidth(pb),
        "height": _CV.CVPixelBufferGetHeight(pb),
        "bytes_per_row": _CV.CVPixelBufferGetBytesPerRow(pb),
        "pixel_format": _CV.CVPixelBufferGetPixelFormatType(pb),
        "planar": _CV.CVPixelBufferIsPlanar(pb),
    }
    if info["planar"]:
        planes = []
        for i in range(_CV.CVPixelBufferGetPlaneCount(pb)):
            planes.append({
                "bytes_per_row": _CV.CVPixelBufferGetBytesPerRowOfPlane(pb, i),
                "base": _CV.CVPixelBufferGetBaseAddressOfPlane(pb, i),
            })
        info["planes"] = planes
    return info


def _frame_bytes(sbuf, pixfmt: str) -> bytes | None:
    """The whole frame as ffmpeg's `-f rawvideo` wants it.

    For 420v that is NV12: plane 0 (Y, full height) followed by plane 1
    (CbCr, half height), row padding stripped if the buffer has any.  For
    BGRA it is one plane, no padding in practice (bpr == 4 * width) --
    the same fallback shape _dump_frame already writes.
    """
    sbuf_ptr = _ptr(sbuf)
    if sbuf_ptr is None:
        return None
    pb = _CM.CMSampleBufferGetImageBuffer(ctypes.c_void_p(sbuf_ptr))
    if not pb:
        return None
    if _CV.CVPixelBufferLockBaseAddress(pb, 0) != 0:
        return None
    try:
        height = _CV.CVPixelBufferGetHeight(pb)
        if not _CV.CVPixelBufferIsPlanar(pb):
            bpr = _CV.CVPixelBufferGetBytesPerRow(pb)
            addr = _CV.CVPixelBufferGetBaseAddress(pb)
            if not addr:
                return None
            return ctypes.string_at(addr, bpr * height)
        # NV12: two planes, copied whole when their rows are dense.
        chunks = []
        for i in range(_CV.CVPixelBufferGetPlaneCount(pb)):
            bpr = _CV.CVPixelBufferGetBytesPerRowOfPlane(pb, i)
            addr = _CV.CVPixelBufferGetBaseAddressOfPlane(pb, i)
            if not addr:
                return None
            rows = height if i == 0 else height // 2
            chunks.append(ctypes.string_at(addr, bpr * rows))
        return b"".join(chunks)
    finally:
        _CV.CVPixelBufferUnlockBaseAddress(pb, 0)


def _dump_frame(sbuf, path: str) -> int:
    """Write the whole BGRA plane to disk; returns bytes written (0 on fail)."""
    sbuf_ptr = _ptr(sbuf)
    if sbuf_ptr is None:
        return 0
    pb = _CM.CMSampleBufferGetImageBuffer(ctypes.c_void_p(sbuf_ptr))
    if not pb:
        return 0
    if _CV.CVPixelBufferLockBaseAddress(pb, 0) != 0:
        return 0
    try:
        addr = _CV.CVPixelBufferGetBaseAddress(pb)
        bpr = _CV.CVPixelBufferGetBytesPerRow(pb)
        h = _CV.CVPixelBufferGetHeight(pb)
        if not addr:
            return 0
        data = ctypes.string_at(addr, bpr * h)
        with open(path, "wb") as fh:
            fh.write(data)
        return len(data)
    finally:
        _CV.CVPixelBufferUnlockBaseAddress(pb, 0)


def _read_status(sbuf) -> int | None:
    """SCFrameStatus from the sample buffer's attachments, best-effort."""
    sbuf_ptr = _ptr(sbuf)
    if sbuf_ptr is None:
        return None
    try:
        getter = CoreMedia.CMSampleBufferGetAttachment
    except AttributeError:
        return None
    try:
        key = SCK.SCStreamFrameInfoStatus
        value = getter(sbuf, key, None)
        return int(value) if value is not None else None
    except Exception:
        return None


# ---------------------------------------------------------------------------
# The SCStreamOutput sink. SCK calls ObjC protocol methods on a queue; a
# plain pyobjc NSObject subclass with the selector implemented is all that
# is needed (the runtime dispatches dynamically -- no explicit protocol
# conformance required).
# ---------------------------------------------------------------------------


class _Sink(Foundation.NSObject):
    def stream_didOutputSampleBuffer_ofType_(self, stream, sbuf, otype):
        state = self._state
        try:
            if int(otype) == int(SCK.SCStreamOutputTypeScreen):
                self._on_video(state, sbuf)
            else:
                self._on_audio(state, sbuf)
        except Exception as exc:  # a probe never dies inside a callback
            state["handler_errors"].append(repr(exc))

    def _on_video(self, state, sbuf):
        now = time.monotonic()
        first = not state["v_times"]
        state["v_times"].append(now)
        if first:
            state["first_frame_at"] = now
            state["frame_info"] = _read_frame(sbuf)
            status = _read_status(sbuf)
            state["first_status"] = status
            if state["dump_frame"]:
                state["frame_dumped"] = _dump_frame(sbuf, state["dump_frame"])
        status = _read_status(sbuf)
        state["statuses"][status] = state["statuses"].get(status, 0) + 1
        proc = state["ffmpeg"]
        if proc is not None:
            t0 = time.monotonic()
            buf = _frame_bytes(sbuf, state["pixfmt"])
            if buf:
                try:
                    proc.stdin.write(buf)
                    state["piped_frames"] += 1
                    state["piped_bytes"] += len(buf)
                except (BrokenPipeError, OSError) as exc:
                    state["pipe_error"] = repr(exc)
                    state["ffmpeg"] = None
            state["write_ms"].append(1000 * (time.monotonic() - t0))

    def _on_audio(self, state, sbuf):
        now = time.monotonic()
        state["a_times"].append(now)
        sbuf_ptr = _ptr(sbuf)
        size = 0
        if sbuf_ptr is not None:
            size = int(_CM.CMSampleBufferGetTotalSampleSize(ctypes.c_void_p(sbuf_ptr)))
        state["a_bytes"] += size
        state["a_sizes"].append(size)
        # Per-buffer shape. Two traps recorded here the hard way:
        #   * CMSampleBufferGetTotalSampleSize() returns 0 for audio -- the
        #     a_bytes counter below is meaningless for audio; the block
        #     buffer length (dlen) is the truth;
        #   * silent desktops deliver FULL-SIZE buffers (960 samples, 7680
        #     bytes of zeros, 20 ms apart) -- there is no empty-buffer case
        #     for a feeder to paper over.
        if sbuf_ptr is not None and len(state["a_shapes"]) < 600:
            p = ctypes.c_void_p(sbuf_ptr)
            block = _CM.CMSampleBufferGetDataBuffer(p)
            dlen = int(_CM.CMBlockBufferGetDataLength(ctypes.c_void_p(block))) if block else 0
            state["a_block_bytes"] += dlen
            ns = int(_CM.CMSampleBufferGetNumSamples(p))
            pts = _CM.CMSampleBufferGetPresentationTimeStamp(p)
            dur = _CM.CMSampleBufferGetDuration(p)
            state["a_shapes"].append(
                (ns, dlen, _cmtime_seconds(pts), pts.flags,
                 _cmtime_seconds(dur), dur.flags))
        if state["dump_audio"] and len(state["audio_buf"]) < state["audio_cap"]:
            data = _read_audio_bytes(sbuf_ptr)
            if data:
                state["audio_buf"] += data


def _read_audio_bytes(sbuf_ptr) -> bytes:
    """Copy an audio sample buffer's PCM payload, best-effort."""
    if sbuf_ptr is None:
        return b""
    try:
        block = _CM.CMSampleBufferGetDataBuffer(ctypes.c_void_p(sbuf_ptr))
        if not block:
            return b""
        length = int(_CM.CMBlockBufferGetDataLength(ctypes.c_void_p(block)))
        if length <= 0:
            return b""
        length_out = ctypes.c_size_t(0)
        total_out = ctypes.c_size_t(0)
        ptr_out = ctypes.c_void_p(0)
        rc = _CM.CMBlockBufferGetDataPointer(
            ctypes.c_void_p(block),
            0,
            ctypes.byref(length_out),
            ctypes.byref(total_out),
            ctypes.byref(ptr_out),
        )
        if rc != 0 or not ptr_out.value:
            return b""
        return ctypes.string_at(ptr_out.value, int(total_out.value))
    except Exception:
        return b""


def _wait_event(ev: threading.Event, timeout: float, what: str) -> bool:
    if ev.wait(timeout):
        return True
    print(f"[sck] TIMEOUT waiting for {what} ({timeout:.0f} s)", flush=True)
    return False


def _pct(values: list[float], pct: float) -> float:
    if not values:
        return float("nan")
    s = sorted(values)
    idx = min(len(s) - 1, max(0, int(round(pct / 100.0 * (len(s) - 1)))))
    return s[idx]


def main() -> int:
    ap = argparse.ArgumentParser(description="ScreenCaptureKit capture probe")
    ap.add_argument("--seconds", type=float, default=6.0, help="capture window")
    ap.add_argument("--fps", type=int, default=24, help="minimumFrameInterval target")
    ap.add_argument("--scale", default="1920x1080",
                    help="WxH output size, or 'native' for the display's own size")
    ap.add_argument("--queue-depth", type=int, default=3)
    ap.add_argument("--display", type=int, default=0, help="index into SCShareableContent.displays")
    ap.add_argument("--no-audio", action="store_true", help="skip the audio output")
    ap.add_argument("--dump-frame", default=None, help="write the first frame's raw plane here")
    ap.add_argument("--dump-audio", default=None, help="write up to --audio-cap bytes of PCM here")
    ap.add_argument("--audio-cap", type=int, default=4_000_000)
    ap.add_argument("--play-sound", action="store_true",
                    help="loop a system sound during capture (proves audio is real)")
    ap.add_argument("--ffmpeg", action="store_true",
                    help="pipe every frame into ffmpeg (h264_videotoolbox)")
    ap.add_argument("--pixfmt", choices=["nv12", "bgra"], default="nv12",
                    help="SCK output pixel format to request (and to hand ffmpeg)")
    ap.add_argument("--ffmpeg-out", default=None,
                    help="ffmpeg output path (default: null muxer, throughput only)")
    ap.add_argument("--ffmpeg-bin", default=None,
                    help="ffmpeg binary (default: $SCK_PROBE_FFMPEG, PATH, brew)")
    ap.add_argument("--progress", default=None,
                    help="write ffmpeg -progress here and report start->first frame")
    args = ap.parse_args()

    # -- 1. shareable content (this is where a lost TCC grant shows up) ----
    box: dict = {}
    done = threading.Event()

    def on_content(content, error):
        box["content"] = content
        box["error"] = error
        done.set()

    SCK.SCShareableContent.getShareableContentWithCompletionHandler_(on_content)
    if not _wait_event(done, 15.0, "shareable content"):
        return 2
    if box.get("error") is not None:
        err = box["error"]
        print(f"[sck] shareable content failed: {err}", flush=True)
        return 2
    content = box["content"]
    displays = list(content.displays())
    windows = list(content.windows())
    print(f"[sck] displays: {len(displays)}, windows: {len(windows)}", flush=True)
    for i, d in enumerate(displays):
        try:
            print(f"  display[{i}]: id={d.displayID()} {d.width()}x{d.height()}", flush=True)
        except Exception as exc:
            print(f"  display[{i}]: <unreadable: {exc}>", flush=True)
    if not displays:
        print("[sck] no displays shareable -- TCC or session problem", flush=True)
        return 2
    display = displays[args.display]

    # -- 2. configuration --------------------------------------------------
    config = SCK.SCStreamConfiguration.alloc().init()
    if args.scale != "native":
        w, h = (int(v) for v in args.scale.lower().split("x"))
        config.setWidth_(w)
        config.setHeight_(h)
        config.setScalesToFit_(True)
    else:
        config.setWidth_(display.width())
        config.setHeight_(display.height())
    config.setMinimumFrameInterval_(CoreMedia.CMTimeMake(1, args.fps))
    config.setQueueDepth_(args.queue_depth)
    config.setShowsCursor_(True)
    if args.pixfmt == "bgra":
        # kCVPixelFormatType_32BGRA -- one plane, ffmpeg swscale does the rest.
        config.setPixelFormat_(0x42475241)
    if not args.no_audio:
        config.setCapturesAudio_(True)
        config.setExcludesCurrentProcessAudio_(True)
        config.setSampleRate_(48000)
        config.setChannelCount_(2)

    # -- 3. filter + stream ------------------------------------------------
    try:
        filt = SCK.SCContentFilter.alloc().initWithDisplay_excludingApplications_exceptingWindows_(
            display, [], [])
    except Exception:
        filt = SCK.SCContentFilter.alloc().initWithDisplay_excludingWindows_(display, [])
    stream = SCK.SCStream.alloc().initWithFilter_configuration_delegate_(filt, config, None)

    state: dict = {
        "v_times": [], "a_times": [], "statuses": {}, "a_bytes": 0, "a_sizes": [],
        "a_shapes": [], "a_block_bytes": 0,
        "first_frame_at": None, "frame_info": None, "first_status": None,
        "handler_errors": [], "audio_buf": bytearray(), "audio_cap": args.audio_cap,
        "dump_frame": args.dump_frame, "dump_audio": args.dump_audio,
        "frame_dumped": 0,
        "ffmpeg": None, "pixfmt": args.pixfmt, "piped_frames": 0, "piped_bytes": 0,
        "write_ms": [], "pipe_error": None,
    }
    video_sink = _Sink.alloc().init()
    video_sink._state = state
    try:
        ok, err = stream.addStreamOutput_type_sampleHandlerQueue_error_(
            video_sink, SCK.SCStreamOutputTypeScreen, None, None)
    except Exception as exc:
        print(f"[sck] addStreamOutput(video) raised: {exc!r}", flush=True)
        return 2
    if not ok:
        print(f"[sck] addStreamOutput(video) refused: {err}", flush=True)
        return 2
    if not args.no_audio:
        audio_sink = _Sink.alloc().init()
        audio_sink._state = state
        try:
            ok, err = stream.addStreamOutput_type_sampleHandlerQueue_error_(
                audio_sink, SCK.SCStreamOutputTypeAudio, None, None)
        except Exception as exc:
            print(f"[sck] addStreamOutput(audio) raised: {exc!r}", flush=True)
            return 2
        if not ok:
            print(f"[sck] addStreamOutput(audio) refused: {err}", flush=True)
            return 2

    # -- 4. capture --------------------------------------------------------
    ffmpeg_proc = None
    ffmpeg_err: list[bytes] = []
    if args.ffmpeg:
        binp = (args.ffmpeg_bin or os.environ.get("SCK_PROBE_FFMPEG")
                or shutil.which("ffmpeg")
                or "/opt/homebrew/opt/ffmpeg/bin/ffmpeg")
        if args.scale != "native":
            ow, oh = (int(v) for v in args.scale.lower().split("x"))
        else:
            ow, oh = int(display.width()), int(display.height())
        ffmpeg_argv = [
            binp, "-hide_banner", "-loglevel", "warning",
            "-f", "rawvideo", "-pix_fmt", args.pixfmt, "-s", f"{ow}x{oh}",
            "-framerate", str(args.fps), "-i", "-",
            "-c:v", "h264_videotoolbox", "-realtime", "1", "-b:v", "4000k",
        ]
        ffmpeg_argv += [args.ffmpeg_out] if args.ffmpeg_out else ["-f", "null", "-"]
        if args.progress:
            ffmpeg_argv += ["-progress", args.progress, "-nostats"]
        t_ffmpeg = time.monotonic()
        ffmpeg_proc = subprocess.Popen(
            ffmpeg_argv, stdin=subprocess.PIPE, stderr=subprocess.PIPE)

        def _drain(pipe, sink):
            for line in pipe:
                sink.append(line)
                del sink[:-40]

        threading.Thread(
            target=_drain, args=(ffmpeg_proc.stderr, ffmpeg_err), daemon=True).start()
        state["ffmpeg"] = ffmpeg_proc
        print(f"[sck] ffmpeg started: {' '.join(ffmpeg_argv)}", flush=True)

        if args.progress:
            def _watch_progress():
                # First progress line with frame>0 == the first encoded frame
                # has been handed to the muxer. 50 ms poll; ffmpeg itself
                # writes progress once per second, so this is a coarse but
                # same-shaped number on both capture paths.
                deadline = time.monotonic() + 30.0
                while time.monotonic() < deadline:
                    try:
                        with open(args.progress) as fh:
                            text = fh.read()
                    except OSError:
                        text = ""
                    m = re.search(r"frame=\s*(\d+)", text)
                    if m and int(m.group(1)) > 0:
                        state["ffmpeg_first_frame_at"] = time.monotonic()
                        return
                    if state["ffmpeg"] is None:
                        return
                    time.sleep(0.05)

            threading.Thread(target=_watch_progress, daemon=True).start()
            state["ffmpeg_start_mono"] = t_ffmpeg

    started = threading.Event()
    start_error: list = []

    def on_start(error):
        start_error.append(error)
        started.set()

    t_go = time.monotonic()
    stream.startCaptureWithCompletionHandler_(on_start)
    if not _wait_event(started, 15.0, "startCapture"):
        return 2
    t_started = time.monotonic()
    if start_error and start_error[0] is not None:
        print(f"[sck] startCapture failed: {start_error[0]}", flush=True)
        return 2
    print(f"[sck] startCapture callback after {1000 * (t_started - t_go):.1f} ms", flush=True)
    state["started_at"] = t_started

    sound_proc = None
    if args.play_sound:

        def _loop_sound():
            while not stop_sound.is_set():
                subprocess.run(
                    ["afplay", "/System/Library/Sounds/Glass.aiff"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                stop_sound.wait(0.3)

        stop_sound = threading.Event()
        sound_proc = threading.Thread(target=_loop_sound, daemon=True)
        sound_proc.start()

    time.sleep(args.seconds)
    if args.play_sound:
        stop_sound.set()

    stopped = threading.Event()

    def on_stop(error):
        stopped.set()

    stream.stopCaptureWithCompletionHandler_(on_stop)
    _wait_event(stopped, 15.0, "stopCapture")

    # -- 5. report ---------------------------------------------------------
    print("", flush=True)
    v_times = state["v_times"]
    span = (v_times[-1] - v_times[0]) if len(v_times) > 1 else 0.0
    print(f"[sck] video: {len(v_times)} frames in {span:.3f} s "
          f"({len(v_times) / args.seconds:.2f}/s over the {args.seconds:.0f} s window)",
          flush=True)
    if state["first_frame_at"] is not None:
        print(f"[sck] first frame {1000 * (state['first_frame_at'] - t_started):.1f} ms "
              f"after startCapture returned", flush=True)
    else:
        print("[sck] NO VIDEO FRAMES ARRIVED", flush=True)
    if state["frame_info"]:
        fi = state["frame_info"]
        print(f"[sck] frame: {fi['width']}x{fi['height']} bpr={fi['bytes_per_row']} "
              f"pixel_format=0x{fi['pixel_format']:08x}", flush=True)
        if state["dump_frame"]:
            print(f"[sck] dumped first frame: {state['frame_dumped']} bytes -> "
                  f"{args.dump_frame}", flush=True)
    gaps = [1000 * (b - a) for a, b in zip(v_times, v_times[1:])]
    if gaps:
        print(f"[sck] inter-frame gaps (ms): p50={_pct(gaps, 50):.1f} "
              f"p95={_pct(gaps, 95):.1f} max={max(gaps):.1f}", flush=True)
    if state["statuses"]:
        names = {}
        for const in ("Complete", "Idle", "Blank", "Started", "Stopped", "Suspended"):
            value = getattr(SCK, f"SCFrameStatus{const}", None)
            if value is not None:
                names[int(value)] = const
        dist = ", ".join(f"{names.get(k, k)}={v}" for k, v in sorted(state["statuses"].items()))
        print(f"[sck] frame status: {dist}", flush=True)
    if state["handler_errors"]:
        print(f"[sck] handler errors (first 3): {state['handler_errors'][:3]}", flush=True)

    a_times = state["a_times"]
    if not args.no_audio:
        a_span = (a_times[-1] - a_times[0]) if len(a_times) > 1 else 0.0
        rate = state["a_block_bytes"] / a_span if a_span > 0 else 0.0
        print(f"[sck] audio: {len(a_times)} buffers, {state['a_block_bytes']} block bytes "
              f"over {a_span:.3f} s ({rate / 1000:.1f} kB/s); "
              f"GetTotalSampleSize sum={state['a_bytes']} (0 for audio = CM quirk)",
              flush=True)
        if state["a_sizes"]:
            print(f"[sck] audio buffer sizes: min={min(state['a_sizes'])} "
                  f"max={max(state['a_sizes'])}", flush=True)
        shapes = state["a_shapes"]
        if shapes:
            empty = [r for r in shapes if r[1] == 0]
            fill = [r for r in shapes if r[1] > 0]
            print(f"[sck] audio buffer shape: {len(shapes)} rows sampled, "
                  f"{len(empty)} with 0 data bytes, {len(fill)} with data", flush=True)
            for tag, rows in (("empty", empty[:6]), ("data", fill[:3])):
                for r in rows:
                    print(f"[sck]   {tag}: numSamples={r[0]} dataLen={r[1]} "
                          f"pts={r[2]:.6f} (flags=0x{r[3]:x}) "
                          f"dur={r[4]:.6f} (flags=0x{r[5]:x})", flush=True)
            if empty:
                # The zero-fill rule the feeder needs: bytes that keep the PCM
                # byte-count timeline equal to the buffer's own time span.
                r = empty[0]
                print(f"[sck]   empty-buffer zero-fill: numSamples*8 = {r[0] * 8} B "
                      f"(dur*384000 = {r[4] * 384000:.0f} B)", flush=True)
        if args.dump_audio and state["audio_buf"]:
            with open(args.dump_audio, "wb") as fh:
                fh.write(state["audio_buf"])
            print(f"[sck] dumped {len(state['audio_buf'])} bytes of PCM -> "
                  f"{args.dump_audio}", flush=True)

    if args.ffmpeg and ffmpeg_proc is not None:
        if ffmpeg_proc.stdin:
            try:
                ffmpeg_proc.stdin.close()
            except OSError:
                pass
        try:
            rc = ffmpeg_proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            ffmpeg_proc.kill()
            rc = ffmpeg_proc.wait()
        wm = state["write_ms"]
        print(f"[sck] ffmpeg: piped {state['piped_frames']} frames "
              f"({state['piped_bytes']} bytes), exit={rc}", flush=True)
        if state.get("ffmpeg_first_frame_at") and state.get("ffmpeg_start_mono"):
            print(f"[sck] ffmpeg start -> first encoded frame: "
                  f"{1000 * (state['ffmpeg_first_frame_at'] - state['ffmpeg_start_mono']):.0f} ms",
                  flush=True)
        if wm:
            print(f"[sck] pipe write cost (ms): p50={_pct(wm, 50):.2f} "
                  f"p95={_pct(wm, 95):.2f} max={max(wm):.2f}", flush=True)
        if state["pipe_error"]:
            print(f"[sck] pipe error: {state['pipe_error']}", flush=True)
        for line in ffmpeg_err[-6:]:
            print(f"[ffmpeg] {line.decode(errors='replace').rstrip()}", flush=True)

    if not v_times:
        return 2
    if not args.no_audio and not a_times:
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
