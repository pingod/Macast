#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Preflight: check the host can actually receive a cast *before* blaming the
# code. Every failure here has, at some point, looked like a Macast bug:
# an unreachable interface (mDNS advertised, nothing could connect), a port held
# by a stale instance, a missing mpv, a proxy that makes local playback fail.
#
# Modelled on miraclecast's res/test-hardware-capabilities.sh: check the
# environment first, print the command that fixes it, and never say "probably".
#
#   env -u PYTHONPATH .venv/bin/python scripts/selfcheck.py
#
# Exit status is 0 when nothing blocks receiving, 1 otherwise. Warnings (things
# that only degrade behaviour) do not fail the run.

import glob
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if REPO not in sys.path:
    sys.path.insert(0, REPO)

FAILURES = []
WARNINGS = []


def ok(msg):
    print("[ OK ] " + msg)


def warn(msg, fix=None):
    WARNINGS.append(msg)
    print("[WARN] " + msg + ("\n       fix: " + fix if fix else ""))


def fail(msg, fix=None):
    FAILURES.append(msg)
    print("[FAIL] " + msg + ("\n       fix: " + fix if fix else ""))


# ---------------------------------------------------------------------------
# 1. dependencies
# ---------------------------------------------------------------------------
print("=== dependencies ===")
for mod in ("zeroconf", "netifaces", "cherrypy", "requests"):
    try:
        __import__(mod)
        ok("{} importable".format(mod))
    except Exception as e:
        fail("{} is not importable ({})".format(mod, e),
             "pip install -r requirements/darwin.txt")


# ---------------------------------------------------------------------------
# 2. ports
# ---------------------------------------------------------------------------
print("\n=== ports ===")
PORTS = (
    (8009, "Chromecast Cast v2 (TLS)"),
    (8008, "Chromecast setup HTTP"),
    (7000, "AirPlay RTSP"),
    (58880, "Macast web settings"),
)


def _holders(port):
    """[(pid, command)] listening on `port`.

    The identity matters, not just the count: a port held by *Macast itself*
    means the app is already running (start a second copy and it will write a
    bogus port into the settings file), while port 7000 held by macOS's own
    AirPlay receiver is the normal state of a modern Mac and is something Macast
    already handles by falling back.
    """
    try:
        out = subprocess.run(
            ["lsof", "-nP", "-iTCP:{}".format(port), "-sTCP:LISTEN", "-Fpc"],
            capture_output=True, text=True, timeout=10).stdout
    except Exception:
        return []
    found, pid = [], None
    for line in out.splitlines():
        if line.startswith("p"):
            pid = line[1:]
        elif line.startswith("c") and pid:
            found.append((pid, line[1:].replace("\\x20", " ")))
            pid = None
    return found


def _classify(port, holders):
    """Return 'free' | 'macast' | 'macos-airplay' | 'foreign'."""
    if not holders:
        return "free"
    names = " ".join(c.lower() for _, c in holders)
    if "macast" in names:
        return "macast"
    if port == 7000 and any(k in names for k in
                            ("controlce", "rapportd", "sharingd", "airplay")):
        return "macos-airplay"
    return "foreign"


try:
    from macast.utils import Setting
    try:
        web_port = Setting.get_port()
    except Exception:
        web_port = 58880
except Exception as e:
    web_port = 58880
    warn("could not read the configured web port ({}), assuming 58880".format(e))

for base_port, label in PORTS:
    port = web_port if base_port == 58880 else base_port
    holders = _holders(port)
    kind = _classify(port, holders)
    who = ", ".join("{} (pid {})".format(c, p) for p, c in holders)
    if kind == "free":
        ok("{} is free ({})".format(port, label))
    elif kind == "macast":
        ok("{} is already served by Macast -- the app is running ({})".format(
            port, who))
    elif kind == "macos-airplay":
        warn("{} is held by macOS's own AirPlay receiver ({})".format(port, who),
             "Macast falls back to the next free port and advertises that one, "
             "so casting still works; free it under System Settings -> General "
             "-> AirDrop & Handoff -> AirPlay Receiver if you want port 7000")
    else:
        warn("{} is held by another process ({})".format(port, who),
             "if this is a second Macast copy, quit it first: two instances make "
             "the older one rewrite ApplicationPort and reset the USN")


# ---------------------------------------------------------------------------
# 3. the interface we would advertise
# ---------------------------------------------------------------------------
print("\n=== network ===")
try:
    from macast.discovery import advertisable_addresses, _advertisable_interfaces

    ifaces = _advertisable_interfaces()
    addrs = advertisable_addresses()
    ok("interfaces considered reachable: {}".format(
        ", ".join(sorted(ifaces)) if ifaces else "(default-route fallback)"))
    if addrs and addrs != ["127.0.0.1"]:
        ok("advertising {}".format(", ".join(addrs)))
    else:
        fail("no reachable non-loopback address to advertise (got {})".format(addrs),
             "check Wi-Fi/Ethernet; if you use a VPN, pin the right interface in "
             "Settings -> Network")
except Exception as e:
    warn("could not evaluate advertisable addresses ({})".format(e))

# A sender in another room must be able to reach us; loopback proves nothing.
try:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.connect(("10.255.255.255", 1))
    local_ip = s.getsockname()[0]
    s.close()
    ok("the default route leaves via {}".format(local_ip))
except OSError as e:
    warn("could not determine the outgoing address ({})".format(e))

# Multicast is how discovery works; a blocked group makes the device invisible.
# Note: on macOS mDNSResponder *always* owns UDP 5353, so failing to bind it is
# normal and proves nothing. What matters is whether multicast egress works and
# whether we can join the group with SO_REUSEPORT, which is what zeroconf does.
try:
    mcast = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    mcast.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        mcast.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    try:
        mcast.bind(("", 5353))
        bound = True
    except OSError:
        # Held by mDNSResponder (normal on macOS) -- egress is still testable.
        bound = False
    mcast.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    try:
        mcast.sendto(b"\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00",
                     ("224.0.0.251", 5353))
        egress = True
    except OSError as e:
        egress = False
        fail("cannot send to the mDNS multicast group ({})".format(e),
             "a firewall is blocking multicast; discovery will not work")
    finally:
        mcast.close()
    if egress:
        ok("multicast egress to 224.0.0.251 works{}".format(
            "" if bound else " (port 5353 is held by the system mDNS responder, "
                             "which is normal on macOS)"))
except Exception as e:  # pragma: no cover - defensive
    warn("multicast check inconclusive ({})".format(e))


# ---------------------------------------------------------------------------
# 4. player and helpers
# ---------------------------------------------------------------------------
print("\n=== player ===")
mpv = shutil.which("mpv")
try:
    from macast.utils import Setting as _S
    configured = _S.mpv_default_path()
except Exception:
    configured = None
if configured and os.path.exists(configured):
    ok("mpv at the configured path: {}".format(configured))
elif mpv:
    ok("mpv on PATH: {}".format(mpv))
else:
    fail("mpv was not found",
         "install mpv, or set its path in Macast's settings")

if mpv or (configured and os.path.exists(configured)):
    binary = mpv or configured
    try:
        out = subprocess.run([binary, "--version"], capture_output=True,
                             text=True, timeout=10).stdout.splitlines()
        ok("mpv reports {}".format(out[0] if out else "(no version line)"))
    except Exception as e:
        warn("could not run mpv --version ({})".format(e))
    try:
        out = subprocess.run([binary, "--list-options"], capture_output=True,
                             text=True, timeout=15).stdout
        if "input-ipc-server" in out:
            ok("mpv supports --input-ipc-server (needed for status reporting)")
        else:
            fail("mpv does not support --input-ipc-server; Macast cannot read "
                 "playback state or pause/seek",
                 "upgrade mpv")
    except Exception as e:
        warn("could not list mpv options ({})".format(e))

ffmpeg = shutil.which("ffmpeg") or "/opt/homebrew/opt/ffmpeg/bin/ffmpeg"
if os.path.exists(ffmpeg):
    ok("ffmpeg present ({}), so you can build a test stream".format(ffmpeg))
else:
    warn("ffmpeg not found",
         "only needed to synthesise a test stream locally: "
         "brew install ffmpeg")

# uxplay is what the AirPlay Screen Mirror plugin supervises -- an iPhone
# mirroring its screen lands there, not in Macast. It is never a *failure*
# here because the plugin is optional, but a missing binary is the one thing
# that makes that plugin do nothing at all, and there is no package to name:
# upstream publishes no Homebrew formula and no macOS binary.
for _name, _dirs in (("uxplay", ("/opt/homebrew/bin", "/usr/local/bin",
                                 "/opt/local/bin", "/usr/bin")),
                     ("shairport-sync", ("/opt/homebrew/bin", "/usr/local/bin",
                                         "/opt/local/bin", "/usr/bin"))):
    found = shutil.which(_name) or next(
        (os.path.join(d, _name) for d in _dirs
         if os.path.exists(os.path.join(d, _name))), None)
    if found:
        ok("{} present ({})".format(_name, found))
    elif _name == "uxplay":
        warn("uxplay not found -- iPhone screen mirroring has nowhere to go",
             "the AirPlay Screen Mirror plugin needs it and there is no package "
             "for it: build from source (Xcode command line tools, "
             "brew install cmake libplist openssl@3, the GStreamer runtime + "
             "-devel .pkg from gstreamer.freedesktop.org, then cmake . && make "
             "&& sudo make install). The plugin repeats this recipe in the log")
    else:
        warn("shairport-sync not found -- AirPlay audio (RAOP) has nowhere to go",
             "brew install shairport-sync (the AirPlay Audio plugin supervises it)")


# ---------------------------------------------------------------------------
# 4a. what the *sender* plugins need on this machine
#
# Everything above asks "can this Mac receive a cast". The Casting Suite plugins
# (Screen Mirror, Local File Caster) turn the question around, and each of them
# fails for a different missing piece that used to only surface as "the menu
# says nothing happened": an ffmpeg without the encoder a given target demands,
# no ffprobe (so a file cannot be classified as copy or convert), no system
# audio tap, no television on the LAN to pick, or a transcode that runs the
# volume out mid-stream.
#
# Nothing here reads or writes macast_setting.json -- AGENTS.md 10. The one
# setting that changes an answer below (the transcode directory) is read
# straight from the file as text.
# ---------------------------------------------------------------------------
print("\n=== sender plugins ===")

COMMON_BIN_DIRS = ("/opt/homebrew/opt/ffmpeg/bin", "/opt/homebrew/bin",
                   "/usr/local/opt/ffmpeg/bin", "/usr/local/bin",
                   "/opt/local/bin", "/usr/bin")


def _find(name):
    """PATH first, then the directories a Finder-launched app never sees.

    Same rule the plugins use: a menu-bar app started from the Dock does not
    inherit the shell's PATH, so `which` alone reports "missing" for a tool
    that works fine in the plugin.
    """
    return shutil.which(name) or next(
        (os.path.join(d, name) for d in COMMON_BIN_DIRS
         if os.path.exists(os.path.join(d, name))), None)


ffprobe = _find("ffprobe")
if ffprobe:
    ok("ffprobe present ({}), so Local File Caster can decide copy-vs-convert "
       "per file".format(ffprobe))
else:
    warn("ffprobe is missing",
         "brew install ffmpeg -- Local File Caster decides per file whether the "
         "television can decode it, and without ffprobe it cannot")

if os.path.exists(ffmpeg):
    # One `-encoders` run answers four questions, because the four targets want
    # four different codecs and each has its own "why is the menu empty".
    try:
        out = subprocess.run([ffmpeg, "-hide_banner", "-encoders"],
                             capture_output=True, text=True,
                             timeout=20).stdout
    except Exception as e:
        out = ""
        warn("could not ask ffmpeg about its encoders ({})".format(e))
    for flag, why in (("libx264", "every mirror target except the hardware one"),
                      ("h264_videotoolbox",
                       "the macOS '硬件编码（VideoToolbox）' switch"),
                      ("mpeg2video", "the DLNA profiles for an old television"),
                      ("ac3", "audio on those same MPEG-PS profiles")):
        if flag in out:
            ok("ffmpeg can encode {} -- needed for {}".format(flag, why))
        elif flag == "h264_videotoolbox":
            warn("this ffmpeg has no h264_videotoolbox, so the hardware "
                 "encoding switch stays unavailable",
                 "mirror in software instead; a Homebrew ffmpeg does have it")
        else:
            warn("this ffmpeg cannot encode {}, so {} will fail".format(
                flag, why),
                 "reinstall ffmpeg from Homebrew -- a stripped build without "
                 "libx264/mpeg2video/ac3 is not usable for the sender plugins")
    try:
        filters = subprocess.run([ffmpeg, "-hide_banner", "-filters"],
                                 capture_output=True, text=True,
                                 timeout=20).stdout
        if "subtitles" in filters:
            ok("ffmpeg has the subtitles filter, so a burnt-in subtitle track "
               "is possible on the conversion path")
        else:
            warn("this ffmpeg has no libass (no 'subtitles' filter)",
                 "Chromecast still gets subtitles as WebVTT; only the burnt-in "
                 "fallback for other targets is unavailable")
    except Exception as e:
        warn("could not ask ffmpeg about its filters ({})".format(e))

    # Which screen ffmpeg can see is the question Screen Mirror actually asks,
    # and it is the one that has been wrong before (AGENTS.md 4.2).
    if sys.platform == "darwin":
        try:
            listing = subprocess.run(
                [ffmpeg, "-hide_banner", "-f", "avfoundation",
                 "-list_devices", "true", "-i", ""],
                capture_output=True, text=True, timeout=20)
            text = (listing.stdout or "") + (listing.stderr or "")
            screens = [line for line in text.splitlines()
                       if "Capture screen" in line]
            if screens:
                ok("avfoundation lists {:d} screen capture device(s), so Screen "
                   "Mirror has something to grab".format(len(screens)))
            else:
                warn("avfoundation answered without any screen capture device",
                     "grant Screen Recording to Macast under System Settings -> "
                     "Privacy & Security, then restart it")
        except Exception as e:
            warn("could not list avfoundation devices ({})".format(e))

# System audio: where a tap exists at all, and whether it is set up.
if sys.platform == "darwin":
    taps = sorted(glob.glob("/Library/Audio/Plug-Ins/HAL/BlackHole*.driver") +
                  glob.glob(os.path.expanduser(
                      "~/Library/Audio/Plug-Ins/HAL/BlackHole*.driver")))
    if taps:
        # Two questions, not one: the files being on disk does not mean the
        # audio daemon loaded them (the official .pkg's postinstall only
        # chmods), and the plugin's capture probe talks to avfoundation.
        # Reporting "installed, so you're covered" for a driver that never
        # loaded is how "装了却没有设备" ended up looking like the user's
        # problem -- and Screen Mirror then re-ran its installer forever.
        live = True
        if ffmpeg:
            try:
                listing = subprocess.run(
                    [ffmpeg, "-hide_banner", "-f", "avfoundation",
                     "-list_devices", "true", "-i", ""],
                    capture_output=True, text=True, timeout=20)
                text = (listing.stdout or "") + (listing.stderr or "")
                live = "blackhole" in text.lower()
            except Exception:
                live = False
        if live:
            ok("a BlackHole HAL driver is installed and ffmpeg lists it, so "
               "Screen Mirror can carry system audio")
        else:
            warn("a BlackHole HAL driver is on disk but ffmpeg does not list "
                 "it: coreaudiod has not loaded it",
                 "Screen Mirror -> 系统声音 -> 一键设置 reloads the audio "
                 "service with one admin password (it will not re-download "
                 "the .pkg); rebooting does the same thing")
    else:
        warn("no BlackHole driver, so mirroring will be video-only",
             "Screen Mirror -> 系统声音 -> 一键设置 installs and configures it "
             "(the .pkg still asks for your password)")
elif sys.platform.startswith("linux"):
    try:
        sinks = subprocess.run(["pactl", "list", "short", "sinks"],
                               capture_output=True, text=True,
                               timeout=10).stdout
        if ".monitor" in sinks:
            ok("PulseAudio exposes sink monitors, so Screen Mirror can carry "
               "system audio")
        else:
            warn("no PulseAudio sink monitor found",
                 "system audio will be dropped from the mirror")
    except Exception:
        warn("could not ask PulseAudio about sink monitors (pactl unavailable?)")
else:
    ok("Windows: mirroring is video-only by design (no system audio tap)")

# Where the conversion path writes, and whether it will fit.
transcode_dir = tempfile.gettempdir()
try:
    from macast.utils import SETTING_DIR
except Exception:
    SETTING_DIR = None
if SETTING_DIR:
    try:
        with open(os.path.join(SETTING_DIR, "macast_setting.json"),
                  encoding="utf-8") as _fh:
            transcode_dir = (json.load(_fh).get("Temp_Dir") or "").strip() \
                or transcode_dir
    except Exception:
        pass  # no settings file yet -- the default is the honest answer
try:
    usage = shutil.disk_usage(os.path.expanduser(transcode_dir))
    if usage.free >= 3 << 30:
        ok("{:d} GiB free in {} (the transcode path writes there)".format(
            usage.free >> 30, transcode_dir))
    else:
        warn("only {:d} MiB free in {}".format(
            usage.free >> 20, transcode_dir),
             "a transcode writes a growing file there and stops being able to "
             "grow; point Temp_Dir at a larger volume from the settings page")
except Exception as e:
    warn("could not measure the transcode directory ({})".format(e))

# A television that is not on this LAN cannot be picked from any menu.
found_casts, found_renderers = [], []
try:
    from zeroconf import ServiceBrowser, Zeroconf

    _zc = Zeroconf()
    try:
        class _SenderListener:
            def __init__(self):
                self.names = []

            def add_service(self, zc, service_type, name):
                self.names.append(name)

            def remove_service(self, *args):
                pass

            def update_service(self, *args):
                pass

        _listener = _SenderListener()
        _browsers = [ServiceBrowser(_zc, "_googlecast._tcp.local.", _listener)]
        try:
            _browsers.append(ServiceBrowser(_zc, "_airplay._tcp.local.",
                                            _listener))
        except Exception:
            pass
        time.sleep(3.0)
        found_casts = [n for n in _listener.names if "googlecast" in n.lower()]
    finally:
        _zc.close()
except Exception as e:
    warn("could not browse mDNS for Chromecasts ({})".format(e))

try:
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    probe.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    probe.settimeout(2.0)
    probe.sendto((
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n"
        'MAN: "ssdp:discover"\r\nMX: 2\r\n'
        "ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n"
    ).encode("ascii", "strict"), ("239.255.255.250", 1900))
    while True:
        try:
            data, _peer = probe.recvfrom(4096)
        except socket.timeout:
            break
        if b"LOCATION" in data.upper():
            found_renderers.append(data)
    probe.close()
except Exception as e:
    warn("could not send the SSDP probe ({})".format(e))

if found_casts:
    ok("{:d} Chromecast-like device(s) answering mDNS: {}".format(
        len(found_casts), ", ".join(sorted(set(
            n.split(".local.")[0] for n in found_casts)))[:200]))
if found_renderers:
    ok("{:d} UPnP MediaRenderer(s) answered the SSDP probe".format(
        len(found_renderers)))
if not found_casts and not found_renderers:
    warn("no Chromecast and no DLNA television answered discovery",
         "the sender plugins have nowhere to send: check the two devices are on "
         "the same subnet, that the network is not client-isolated, and that no "
         "firewall is dropping multicast")


# ---------------------------------------------------------------------------
# 4b. can anyone actually install a plugin from the index?
#
# The settings page lists what `plugins/info.json` offers and installs it from a
# jsDelivr URL pinned to a commit. Neither jsDelivr nor raw can read a private
# repository, and Macast sends no credentials when it downloads -- so the cards
# render, look installable, and fail minutes later with a download error that
# reads like a network problem.
#
# Three questions, because they have different answers and the failure mode is
# subtle: does GitHub show the repo to an anonymous caller, does jsDelivr's own
# metadata API resolve it, and does each pinned URL actually serve. A pin can
# answer 200 while the repository is private -- the CDN keeps serving copies it
# cached while the repo was reachable, which says nothing about a cold request
# for a file nobody has fetched yet. The control request to the known-public
# upstream is what separates all of that from "this machine has no egress".
# ---------------------------------------------------------------------------
print("\n=== online plugin index ===")
try:
    import json as _json
    import requests as _requests
    from macast import plugin_repo

    _heads = {'User-Agent': 'Macast-selfcheck'}

    def _status(url, timeout=8):
        """HTTP status for `url`; 0 when the request never got an answer."""
        try:
            resp = _requests.get(url, headers=_heads, timeout=timeout,
                                 stream=True)
            try:
                return resp.status_code
            finally:
                resp.close()
        except _requests.exceptions.RequestException:
            return 0

    _ours = _status('https://api.github.com/repos/{}'.format(plugin_repo.REPO))
    _theirs = _status('https://api.github.com/repos/xfangfang/Macast')
    _cdn = _status('https://data.jsdelivr.com/v1/packages/gh/{}'
                   .format(plugin_repo.REPO))
    with open(os.path.join(REPO, "plugins", "info.json"), encoding="utf-8") as _fh:
        _entries = _json.load(_fh).get("plugin_v1") or []

    if _ours == 200 and _cdn == 200:
        ok("index repo {} is public and jsDelivr resolves it, so {} entries are "
           "installable by anyone".format(plugin_repo.REPO, len(_entries)))
    elif _ours == 404 and _theirs == 200:
        warn("{} is invisible to anonymous callers (GitHub 404 where a public "
             "repo answers 200, and jsDelivr's own API says {} for it), so no "
             "fresh install can be served".format(
                 plugin_repo.REPO,
                 "the same 404" if _cdn == 404 else "HTTP {}".format(_cdn)),
             "the repository is private. Either make it public, or publish "
             "plugins/ from a public repo and point macast/plugin_repo.py::REPO "
             "at it. Meanwhile install by pasting a reachable URL into the "
             "settings page's '从网址安装', or by copying the .py into "
             "~/Library/Application Support/Macast/renderer/")
    elif _ours == 0 or _theirs == 0:
        warn("could not reach GitHub to judge the index ({} vs {})".format(
            _ours, _theirs),
             "a blocking proxy makes this inconclusive; it says nothing about "
             "the repository itself")
    else:
        warn("unexpected answers while judging the index repo (ours {}, the "
             "public control {}, jsDelivr metadata {})".format(
                 _ours, _theirs, _cdn),
             "check these by hand: `gh api repos/{} --jq .private` and "
             "`curl -s -o /dev/null -w '%{{http_code}}' "
             "https://data.jsdelivr.com/v1/packages/gh/{}`".format(
                 plugin_repo.REPO, plugin_repo.REPO))

    _dead = [_e["title"] for _e in _entries
             if _status(_e["url"], timeout=8) != 200]
    if _entries and not _dead:
        ok("all {} pinned install URLs answer 200".format(len(_entries)))
    elif _dead:
        warn("{} of {} pinned install URLs do not serve the file: {}".format(
            len(_dead), len(_entries), ", ".join(_dead)),
             "the pins are correct -- Part 5c proves each one against "
             "`git show <sha>:plugins/<file>`, which is local. Reachability is a "
             "different thing, and the entries that still answer 200 do so from "
             "the CDN's cache of a time when the repository was readable")
except Exception as _e:
    warn("could not check the plugin index ({})".format(_e))


# ---------------------------------------------------------------------------
# 5. environment traps from this repo's history
# ---------------------------------------------------------------------------
print("\n=== environment ===")
proxy = [k for k in ("http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY",
                     "all_proxy", "ALL_PROXY") if os.environ.get(k)]
if proxy:
    warn("proxy variables are set: {}".format(", ".join(proxy)),
         "Macast strips them for the player, but they break the local-network "
         "tests in scripts/ -- run those with "
         "`env -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY`")
else:
    ok("no proxy variables to work around")

try:
    from macast.utils import SETTING_DIR
    log = os.path.join(os.path.expanduser("~"), "Library", "Application Support",
                       "Macast", "macast.log")
    ok("settings directory: {}".format(SETTING_DIR))
    if os.path.exists(log):
        ok("log file: {}".format(log))
    else:
        warn("no log file yet at {}".format(log),
             "start Macast once; the log is where playback and discovery are "
             "recorded")
except Exception as e:
    warn("could not locate the settings directory ({})".format(e))


# ---------------------------------------------------------------------------
print()
if FAILURES:
    print("=== {} blocking problem(s): ===".format(len(FAILURES)))
    for msg in FAILURES:
        print("  - {}".format(msg))
    sys.exit(1)
if WARNINGS:
    print("=== environment is workable, with {} warning(s) ===".format(
        len(WARNINGS)))
else:
    print("=== environment looks healthy ===")
