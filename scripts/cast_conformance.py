#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Conformance probe: drive a *running* Macast with the same client stack real
# senders use (pychromecast, the library mkchromecast and Home Assistant talk
# to receivers with) and report every place the receiver does not answer the way
# a real Chromecast does.
#
# Why a probe and not more unit tests: scripts/verify_cast_airplay.py stubs the
# network out, and scripts/vlc_sender_sim.py reproduces *VLC's* state machine.
# Neither runs the code a phone actually runs. Every defect listed below was
# invisible to both of them:
#
#   * SET_VOLUME never answered   -> pychromecast's set_volume() blocks in
#     WaitResponse(REQUEST_TIMEOUT=10.0) and then raises RequestTimeout.
#   * volume hardcoded to 1.0     -> senders compute level +/- 0.1 from what we
#     report, so the slider froze and mute was never visible.
#   * displayName != "Default Media Receiver" -> senders that guard against a
#     hijacked receiver re-issue LOAD in a loop.
#   * ssdp_udn not a bare UUID    -> the host-scanning fallback cannot parse it.
#
# Usage:
#   ./scripts/run-from-source.sh                    # in another terminal
#   env -u PYTHONPATH .venv/bin/python scripts/cast_conformance.py
#   env -u PYTHONPATH .venv/bin/python scripts/cast_conformance.py --name Macast
#
# Exit status is 0 only when every applicable check passes. A check that needs
# something this machine cannot do (no mpv, no mDNS) is SKIPPED, never silently
# passed.

import argparse
import json
import os
import socket
import ssl
import sys
import time
import urllib.error
import urllib.request
import uuid

try:
    import pychromecast
except ImportError:
    sys.exit("pychromecast is required: it is what real senders use. "
             "Install it into the venv you run this with.")

DEFAULT_TIMEOUT = 15.0
CAST_PORT = 8009
SETUP_PORT = 8008

RESULTS = []


def record(name, state, detail=""):
    """state: 'PASS' | 'FAIL' | 'SKIP'"""
    RESULTS.append((name, state, detail))
    print("[{}] {}".format(state, name) + ("  -- " + detail if detail else ""))


def ok(name, cond, detail=""):
    record(name, "PASS" if cond else "FAIL", detail)
    return bool(cond)


# ---------------------------------------------------------------------------
# Discovery
# ---------------------------------------------------------------------------


def find_receiver(name=None, timeout=DEFAULT_TIMEOUT, attempts=3):
    """Return (Chromecast, browser) for a Macast receiver, or (None, None).

    Uses ``get_chromecasts()`` -- the same entry point a real sender uses.
    Retries briefly: a receiver that was just started may not have announced
    itself yet, and a false "not discoverable" is the least useful answer this
    tool can give.
    """
    want = (name or "").strip().lower()
    for attempt in range(attempts):
        casts, browser = pychromecast.get_chromecasts(timeout=timeout)
        for cast in casts:
            friendly = (cast.name or "")
            # Macast's friendly name is free text
            # ("Macast(MyHost.local)"), so match loosely instead of
            # requiring an exact string.
            if not want or want in friendly.lower():
                return cast, browser
        pychromecast.discovery.stop_discovery(browser)
        if attempt < attempts - 1:
            time.sleep(3.0)
    return None, None


# ---------------------------------------------------------------------------
# HTTP bits (no dependency on the Cast channel)
# ---------------------------------------------------------------------------


def _fetch(url, timeout=5.0, insecure_tls=False):
    ctx = None
    if insecure_tls:
        ctx = ssl._create_unverified_context()
    with urllib.request.urlopen(url, timeout=timeout, context=ctx) as resp:
        return resp.status, resp.read().decode("utf-8", "replace")


def check_eureka_info(host, setup_port):
    """What a sender learns from /setup/eureka_info."""
    url = "http://{}:{}/setup/eureka_info?params=device_info,name".format(
        host, setup_port)
    try:
        status, body = _fetch(url)
    except (urllib.error.URLError, OSError) as e:
        ok("eureka_info is served", False, "{}: {}".format(type(e).__name__, e))
        return
    if not ok("eureka_info is served", status == 200, "status={}".format(status)):
        return
    try:
        info = json.loads(body)
    except ValueError as e:
        ok("eureka_info is JSON", False, str(e))
        return
    ok("eureka_info is JSON", True)

    device_info = info.get("device_info") or {}
    udn = device_info.get("ssdp_udn") or info.get("ssdp_udn") or ""
    # pychromecast does UUID(udn.replace("-", "")) and gives up on the whole
    # device when that raises, so this must be a bare UUID -- no "uuid:" prefix.
    try:
        uuid.UUID(str(udn).replace("-", ""))
        parses = True
    except Exception:
        parses = False
    ok("ssdp_udn is a bare UUID", parses, repr(udn))

    caps = device_info.get("capabilities") or {}
    # Absent means True to pychromecast whenever device_info is present, which
    # makes every poll try https://host:8443/...?params=multizone.
    ok("multizone_supported is explicitly false",
       caps.get("multizone_supported") is False,
       "capabilities={}".format(caps))


# ---------------------------------------------------------------------------
# The main event: a real sender session
# ---------------------------------------------------------------------------


def check_session(cast, media_url, duration, timeout=DEFAULT_TIMEOUT):
    checks = []

    try:
        cast.wait(timeout=timeout)
        checks.append(("the device answers GET_STATUS", True, ""))
    except Exception as e:
        checks.append(("the device answers GET_STATUS", False, repr(e)))
        return checks

    # `Chromecast.set_volume` does not exist in pychromecast 14 (volume_up() even
    # calls it and would raise AttributeError, which is an upstream bug); the
    # real entry point is the receiver controller, and it is the one that blocks
    # in WaitResponse(REQUEST_TIMEOUT=10.0) until the receiver acknowledges.
    receiver = cast.socket_client.receiver_controller

    status = cast.status
    checks.append((
        "the receiver does not report itself as standby",
        status.is_stand_by is False,
        "isStandBy={!r}".format(status.is_stand_by),
    ))

    # Volume: set_volume() blocks in WaitResponse(10s) and raises if we never
    # acknowledge. This is the check that would have caught the original defect.
    try:
        t0 = time.time()
        receiver.set_volume(0.35)
        elapsed = time.time() - t0
        checks.append(("set_volume is acknowledged", True,
                       "{:.2f}s".format(elapsed)))
        checks.append(("the acknowledgement arrives promptly",
                       elapsed < 5.0, "{:.2f}s".format(elapsed)))
    except Exception as e:
        checks.append(("set_volume is acknowledged", False, repr(e)))

    got = None
    try:
        receiver.update_status()
        deadline = time.time() + 5.0
        while time.time() < deadline:
            got = cast.status.volume_level
            if got is not None and abs(got - 0.35) < 0.02:
                break
            time.sleep(0.2)
    except Exception as e:
        checks.append(("the reported volume is the one we set", False, repr(e)))
    else:
        checks.append(("the reported volume is the one we set",
                       got is not None and abs(got - 0.35) < 0.02,
                       "reported={!r}".format(got)))

    try:
        receiver.set_volume_muted(True)
        checks.append(("set_volume_muted is acknowledged", True, ""))
    except Exception as e:
        checks.append(("set_volume_muted is acknowledged", False, repr(e)))
    try:
        receiver.set_volume_muted(False)
        receiver.set_volume(1.0)
    except Exception:
        pass

    mc = cast.media_controller
    try:
        mc.play_media(media_url, "video/mp4", stream_type="LIVE")
        mc.block_until_active(timeout=30.0)
        checks.append(("LOAD establishes a media session", mc.is_active, ""))
        # The app name only exists once an app is running, so this has to be
        # checked here rather than before the LOAD. Senders key off it:
        # mkchromecast's --hijack treats anything other than "Default Media
        # Receiver" as a stolen receiver and re-loads every 5 seconds.
        app = cast.status
        checks.append((
            "the media app is named the way senders expect",
            app.display_name == "Default Media Receiver",
            "displayName={!r} appId={!r}".format(app.display_name, app.app_id),
        ))
    except Exception as e:
        checks.append(("LOAD establishes a media session", False, repr(e)))
        return checks

    try:
        time.sleep(2.0)
        mc.update_status()
        state = mc.status.player_state
        checks.append(("playback reaches a playing state",
                       state in ("PLAYING", "BUFFERING", "PAUSED"),
                       "playerState={!r}".format(state)))
    except Exception as e:
        checks.append(("playback reaches a playing state", False, repr(e)))

    # Let this first load play all the way out, *before* touching pause or seek.
    # Asking for a clean end-of-file after the receiver has been seeked around
    # reports CANCELLED instead of FINISHED -- truthfully, because the player
    # really was interrupted -- and that would make this probe's most important
    # check depend on the order of the checks above it.
    try:
        deadline = time.time() + duration + 30.0
        idle_reason = None
        seen = []
        while time.time() < deadline:
            mc.update_status()
            state = mc.status.player_state
            if not seen or seen[-1] != state:
                seen.append(state)
            if state == "IDLE":
                idle_reason = mc.status.idle_reason
                break
            time.sleep(0.5)
        # The receiver used to answer PLAYING for as long as a media descriptor
        # existed, so a video that had already finished still looked healthy and
        # the sender never offered to replay it.
        checks.append((
            "a finished video is reported IDLE",
            mc.status.player_state == "IDLE",
            "playerState={!r} seen={}".format(mc.status.player_state, seen),
        ))
        checks.append((
            "a finished video carries idleReason FINISHED",
            idle_reason == "FINISHED",
            "idleReason={!r} states={}".format(idle_reason, seen),
        ))
    except Exception as e:
        checks.append(("a finished video is reported IDLE", False, repr(e)))

    # pause / play / seek on a fresh load. These deliberately come last: they
    # interrupt playback, which is exactly what the finish check above needs not
    # to have happened yet.
    try:
        mc.play_media(media_url, "video/mp4", stream_type="BUFFERED",
                      metadata={"metadataType": 0, "title": "controls"})
        mc.block_until_active(timeout=30.0)
    except Exception as e:
        checks.append(("a second LOAD is accepted", False, repr(e)))
        return checks
    checks.append(("a second LOAD is accepted", True, ""))

    for label, fn in (("pause", mc.pause), ("play", mc.play),
                      ("seek", lambda: mc.seek(0.0))):
        try:
            fn()
            checks.append(("{} is acknowledged".format(label), True, ""))
        except Exception as e:
            checks.append(("{} is acknowledged".format(label), False, repr(e)))

    try:
        cast.quit_app()
        checks.append(("quit_app is accepted", True, ""))
    except Exception as e:
        checks.append(("quit_app is accepted", False, repr(e)))

    try:
        cast.disconnect(timeout=5.0)
    except Exception:
        pass
    return checks


# ---------------------------------------------------------------------------
# A local clip, so the probe needs no internet and no phone
# ---------------------------------------------------------------------------


def make_clip(duration, path, host=None):
    """Render a short clip with ffmpeg and serve it. Returns (url, cleanup, err)."""
    import http.server
    import shutil
    import subprocess
    import tempfile
    import threading

    ffmpeg = (shutil.which("ffmpeg")
              or "/opt/homebrew/opt/ffmpeg/bin/ffmpeg"
              or "/usr/local/bin/ffmpeg")
    if not os.path.exists(ffmpeg):
        return None, None, "ffmpeg not found"

    tmpdir = tempfile.mkdtemp(prefix="macast-conf-")
    clip = os.path.join(tmpdir, "clip.mp4")
    cmd = [
        ffmpeg, "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=15",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
        "-t", str(duration), "-c:v", "libx264", "-preset", "ultrafast",
        "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", clip,
    ]
    try:
        subprocess.run(cmd, check=True, timeout=120)
    except Exception as e:
        return None, None, "ffmpeg failed: {}".format(e)

    class Handler(http.server.BaseHTTPRequestHandler):
        """Minimal file server that honours a single Range request.

        Range is not optional: mpv needs it for any seek, and without it the
        player gives up with "no audio or video data played" (AGENTS.md
        section 6). Python's SimpleHTTPRequestHandler does not implement it.
        """

        def log_message(self, *a):
            pass

        def do_HEAD(self):
            self._serve(head_only=True)

        def do_GET(self):
            self._serve()

        def _serve(self, head_only=False):
            try:
                with open(clip, "rb") as fh:
                    data = fh.read()
            except OSError:
                self.send_error(404)
                return
            total = len(data)
            start, end = 0, total - 1
            status = 200
            rng = self.headers.get("Range")
            if rng and rng.startswith("bytes="):
                spec = rng[len("bytes="):].split(",")[0].strip()
                first, _, last = spec.partition("-")
                try:
                    if first:
                        start = int(first)
                        end = int(last) if last else total - 1
                    else:  # bytes=-N  => last N bytes
                        start = max(0, total - int(last))
                        end = total - 1
                except ValueError:
                    self.send_error(400)
                    return
                start = max(0, min(start, total - 1))
                end = max(start, min(end, total - 1))
                status = 206
            chunk = data[start:end + 1]
            self.send_response(status)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(len(chunk)))
            if status == 206:
                self.send_header("Content-Range",
                                 "bytes {}-{}/{}".format(start, end, total))
            self.end_headers()
            if not head_only:
                self.wfile.write(chunk)

    httpd = http.server.ThreadingHTTPServer(("0.0.0.0", 0), Handler)
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    # Advertise a routable address: 0.0.0.0 is not something a receiver can
    # fetch from. The receiver's own host is ideal (it is this machine), but
    # fall back to the UDP-connect trick when only a URL was supplied.
    if not host:
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            probe.connect(("10.255.255.255", 1))
            host = probe.getsockname()[0]
        except OSError:
            host = "127.0.0.1"
        finally:
            probe.close()

    def cleanup():
        httpd.shutdown()
        shutil.rmtree(tmpdir, ignore_errors=True)

    return "http://{}:{}/clip.mp4".format(host, port), cleanup, None


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default=None,
                    help="friendly name (substring) of the receiver to probe")
    ap.add_argument("--host", default=None,
                    help="skip discovery and use this address")
    ap.add_argument("--media-url", default=None,
                    help="a playable URL; skips generating a local clip")
    ap.add_argument("--clip-seconds", type=int, default=6,
                    help="length of the generated clip (default 6)")
    ap.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT)
    ap.add_argument("--verbose", action="store_true",
                    help="log the pychromecast conversation and raw status")
    args = ap.parse_args()

    if args.verbose:
        import logging
        logging.basicConfig(level=logging.DEBUG,
                            format="%(relativeCreated)6.0fms %(name)s %(message)s")

    print("=== Cast receiver conformance probe ===")

    cast = None
    browser = None
    host = args.host
    cast_port = CAST_PORT
    setup_port = SETUP_PORT
    if host is None:
        print("Looking for a receiver over mDNS ...")
        cast, browser = find_receiver(args.name, timeout=args.timeout)
        if cast is None:
            record("the receiver is discoverable over mDNS", "FAIL",
                   "no _googlecast._tcp service found -- is Macast running "
                   "with the Chromecast protocol enabled?")
            return 1
        host = cast.cast_info.host
        cast_port = cast.cast_info.port or CAST_PORT
        record("the receiver is discoverable over mDNS", "PASS",
               "{} @ {}:{}".format(cast.name, host, cast_port))
        # A receiver on any port other than 8009 is classified as a speaker
        # *group* by pychromecast, which changes how senders treat it.
        if cast_port != CAST_PORT:
            record("the receiver advertises the canonical port 8009", "FAIL",
                   "port={} (pychromecast reports this as a group)".format(cast_port))
        else:
            record("the receiver advertises the canonical port 8009", "PASS", "")
    else:
        # --host: skip discovery but still drive the real client stack.
        cast = pychromecast.get_chromecast_from_host(
            (host, CAST_PORT, uuid.uuid4(), None, args.name))
        record("the receiver is discoverable over mDNS", "SKIP", "--host was given")

    try:
        check_eureka_info(host, setup_port)

        media_url = args.media_url
        cleanup = None
        if media_url is None:
            media_url, cleanup, err = make_clip(args.clip_seconds, None, host=host)
            if err is not None:
                record("a local test clip could be prepared", "SKIP", err)
                media_url = None
            else:
                record("a local test clip could be prepared", "PASS", media_url)

        if media_url is not None:
            for name, state, detail in check_session(cast, media_url,
                                                     args.clip_seconds,
                                                     timeout=args.timeout):
                record(name, "PASS" if state else "FAIL", detail)
        if cleanup is not None:
            cleanup()
    finally:
        try:
            cast.disconnect(timeout=5.0)
        except Exception:
            pass
        if browser is not None:
            try:
                pychromecast.discovery.stop_discovery(browser)
            except Exception:
                pass

    passed = sum(1 for _, s, _ in RESULTS if s == "PASS")
    failed = [r for r in RESULTS if r[1] == "FAIL"]
    skipped = [r for r in RESULTS if r[1] == "SKIP"]
    print("\n=== {} passed, {} failed, {} skipped ===".format(
        passed, len(failed), len(skipped)))
    if failed:
        print("Failed checks:")
        for name, _, detail in failed:
            print("  - {}  ({})".format(name, detail))
        return 1
    print("The receiver answers a real sender stack the way a Chromecast does.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
