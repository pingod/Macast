#!/usr/bin/env python3
"""Regenerate every differential corpus under `macast-*/tests/golden/`.

## Why this file exists

Each corpus was originally produced by a throwaway inline script, and that caused **two wrong
corpora in one session**: a `serve_plan` generator that omitted the reference's `asked` gate, and
a `local_serve` generator that omitted the DLNA header branch. In both cases the port looked
wrong first and the *generator* turned out to be wrong — the expensive direction, because the
temptation is to "fix" the implementation until it matches the broken corpus.

A corpus that cannot be regenerated is not evidence: it is a snapshot of whatever the author
believed that afternoon. So the reference-side logic lives here, one function per corpus, and
`--check` proves the committed files are what these functions produce right now.

## How it extracts the reference

Nothing here imports `macast` (that pulls in cherrypy and freezes `SETTING_DIR` at import time —
AGENTS.md §4.9). Instead each generator executes just the reference's own table/function text,
lifted by marker, so the numbers come from **the shipped Python** rather than from a copy of my
reading of it. Where a reference function is tangled with network or a renderer, the generator
says so and reimplements the decision with a comment naming the file:line it came from.

## Usage

    python3 scripts/build_corpora.py            # rewrite every corpus
    python3 scripts/build_corpora.py --check     # exit 1 if any committed file is stale
    python3 scripts/build_corpora.py parse_range # just one

`--check` is the useful mode: it answers "is the committed corpus still what the reference says",
which is the question nobody was asking before.
"""
import argparse
import json
import math
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CAST = ROOT / "macast-cast/tests/golden"
MPV = ROOT / "macast-mpv/tests/golden"
UPNP = ROOT / "macast-upnp/tests/golden"


# ---------------------------------------------------------------------------
# Helpers that lift the reference's own source text
# ---------------------------------------------------------------------------

def reference_text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def lift(relative: str, start: str, end: str) -> str:
    """The slice of `relative` between two markers, inclusive of the start.

    When `end` is `"<<next top-level>>"` the slice runs to the next line that begins a top-level
    `def`/`class` — needed for functions with nested definitions, where `"\n\ndef "` matches
    inside the function body and silently drops the rest of it (which is how the first version of
    this file cut `parse_range` in half).
    """
    text = reference_text(relative)
    at = text.index(start)
    if end == "<<next top-level>>":
        import re as _re
        after = at + len(start)
        match = _re.search(r"^(?:def |class )", text[after:], _re.M)
        return text[at:after + match.start()] if match else text[at:]
    return text[at:text.index(end, at)]


def exec_lifted(relative: str, start: str, end: str, seed: str = "") -> dict:
    """Execute a slice of the reference and hand back its globals."""
    namespace = {"__name__": "lifted"}
    exec(seed, namespace)
    exec(lift(relative, start, end), namespace)
    return namespace


# ---------------------------------------------------------------------------
# media_relay.py: parse_range and serve_plan  (the two I got wrong)
# ---------------------------------------------------------------------------
#
# These are lifted verbatim — no reimplementation — because that is exactly the pair whose
# reimplementation in a generator caused the `asked`-gate defect.

RELAY = "macast/media_relay.py"


def gen_parse_range() -> dict:
    ns = exec_lifted(RELAY, "def parse_range(", "<<next top-level>>")
    parse_range = ns["parse_range"]

    headers = [
        "", "bytes=0-", "bytes=-0", "bytes=-", "bytes=00-09", "bytes=+5-9",
        "bytes= 5-9", "bytes=5 -9", "bytes=5- 9", "bytes=0-9 ", "bytes= 0-9",
        "BYTES=0-9", "Bytes=0-9", "bytes =0-9", "bytes= 0 - 9 ", "bytes=0 - 9",
        "bytes=0-9\n", "bytes=\t0-9", "bytes=0-0", "bytes=5-4", "bytes=5-5",
        "bytes=-1", "bytes=-500", "bytes=1--2", "bytes=0x10-", "bytes=abc",
        "kilobytes=0-9", "bytes=0-9,10-19", "bytes=1e3-",
        "bytes=999999999999999999999999-", "bytes=0-999999999999999999999999",
        "bytes=-999999999999999999999999", "bytes=  ", "bytes=- ", "bytes= -5",
        "bytes=5- ",
    ]
    rows = []
    for header in headers:
        parsed = parse_range(header)
        row = {"header": header, "out": list(parsed) if parsed is not None else None}
        if parsed is not None and any(
            value is not None and value > (1 << 64) - 1 for value in parsed
        ):
            row["exceeds_u64"] = True
        rows.append(row)
    return rows


def gen_serve_plan() -> dict:
    """The `asked` gate, which is the whole reason this generator exists."""
    ns = exec_lifted(RELAY, "def parse_range(", "<<next top-level>>")
    parse_range = ns["parse_range"]
    text = reference_text(RELAY)
    body = text[text.index("def serve_plan("):]
    body = body[:body.index("\ndef ", 1)]
    # `serve_plan` takes a relay object; give it a duck-typed one.
    class Relay:
        def __init__(self, length, ranges):
            self.length, self.ranges = length, ranges

    # `serve_plan` constructs `ServePlan`, so the dataclass has to be in scope too.
    scope = {"parse_range": parse_range, "dataclass": __import__("dataclasses").dataclass}
    # **From the decorator**, not from `class`: `@dataclass` is what gives it a constructor, and
    # lifting from `class` alone produced `ServePlan() takes no arguments`.
    exec(lift(RELAY, "@dataclass\nclass ServePlan:", "<<next top-level>>"), scope)
    exec(body, scope)
    serve_plan = scope["serve_plan"]

    rows = []
    for length, ranges in ((None, False), (None, True), (1000, False), (1000, True)):
        for header in (None, "", "bytes=-", "bytes=0-", "bytes=2-5", "bytes=-500",
                       "bytes=2000-", "garbage", "bytes=0-9 "):
            plan = serve_plan(Relay(length, ranges), header)
            rows.append({
                "length": length,
                "ranges": ranges,
                "header": header,
                "status": plan.status,
                "start": plan.start,
                "stop": plan.stop,
                "total": plan.total,
                "partial": plan.partial,
            })
    return rows


# ---------------------------------------------------------------------------
# cast_local_file.py: the local-file HTTP face
# ---------------------------------------------------------------------------
#
# `do_GET`/`_headers` are methods on a handler; the generator reimplements their decision path
# with the file:line of each branch in the comment, because the surrounding class needs a socket.

LOCAL = "macast/plugins/renderer/cast_local_file.py"
DLNA_FEATURES = "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000"


def gen_local_serve() -> dict:
    ns = exec_lifted(LOCAL, "def parse_range(", "<<next top-level>>")
    parse_range = ns["parse_range"]

    def do_get(total, header, dlna):
        """cast_local_file.py:810-878, branch for branch."""
        if total == 0:
            # Registered while the file existed; it has since gone. (:820)
            return {"status": 404, "headers": {"Content-Length": "0"}}
        spec = parse_range(header) if header else None
        start, stop = spec if spec else (0, None)
        if start is None:
            start, stop = (max(0, total - stop), None) if (total and stop) else (0, None)
        if total and start >= total:
            # :826 — only these two headers, no DLNA ones.
            return {
                "status": 416,
                "headers": {"Content-Range": "bytes */%d" % total, "Content-Length": "0"},
            }
        end = total - 1 if (stop is None or stop >= total) else stop
        if end < start:
            end = start
        code = 206 if spec else 200
        headers = {
            "Accept-Ranges": "bytes",
            "Cache-Control": "no-store",
            "X-Content-Type-Options": "nosniff",
            "Content-Type": "video/mp4",
        }
        if code == 206 and total:
            headers["Content-Range"] = "bytes %d-%d/%d" % (start, end, total)
        if total:
            headers["Content-Length"] = str(end - start + 1) if code == 206 else str(total)
        if dlna:
            # :866-869
            headers["transferMode.dlna.org"] = "Streaming"
            headers["contentFeatures.dlna.org"] = DLNA_FEATURES
        return {"status": code, "headers": headers}

    ranges = [
        None, "bytes=2-5", "bytes=7-", "bytes=-3", "bytes=99-", "bytes=0-0",
        "bytes=9-0", "bytes=1-2,4-5", "kilobytes=0-9", "bytes=", "garbage",
        "bytes=0-9999", "bytes=-", "bytes=0-9 ", "bytes= 5-9", "bytes=0 - 9",
        "bytes=0-9\n", "bytes=+5-9",
    ]
    rows = []
    for total in (10, 1000):
        for header in ranges:
            for dlna in (False, True):
                rows.append({
                    "total": total, "range": header, "dlna": dlna,
                    "result": do_get(total, header, dlna),
                })
    rows.append({"total": 0, "range": None, "dlna": False, "result": do_get(0, None, False)})
    return rows


# ---------------------------------------------------------------------------
# macast_ytdlp.py: ETA parsing
# ---------------------------------------------------------------------------

YTDLP = "macast/plugins/renderer/macast_ytdlp.py"


def gen_ytdlp_eta() -> dict:
    # One namespace, with the imports and `PROGRESS_RE`/`DEST_RE` the file defines around it (the
    # slice between `ETA_RE` and the next def pulled a function that reads `os`).
    ns = {"__name__": "ytdlp", "re": re, "os": __import__("os")}
    # The regex and the two functions are not adjacent in the file, so lift each by name.
    match = re.search(r"^ETA_RE = .*$", reference_text(YTDLP), re.M)
    exec(match.group(0), ns)
    exec(lift(YTDLP, "def eta_seconds(", "<<next top-level>>"), ns)
    exec(lift(YTDLP, "def hms(", "<<next top-level>>"), ns)
    eta_seconds, hms = ns["eta_seconds"], ns["hms"]

    inputs = [
        "", "   ", "unknown", "1:2:3:4", "1::3", ":", "-5", "1:-2", "00:12", "1:02:03",
        "42", "42.7", " 0:07 ", "-1:02:03", "0:00:00", "99:59:59", "5:6", "5:06",
        "1.9", "abc:def", "10:", "  :30",
        "1_000", "1e3", "nan", "1e30", "1e19", "0x10", "42.", ".5", "+42",
        "1E3", "1 :02:03", "42\n", "00:00:60", "1:60:00",
        "inf", "-inf", "1e400",
    ]
    rows = []
    for text in inputs:
        try:
            seconds = eta_seconds(text)
            row = {"eta_text": text, "seconds": seconds, "hms": hms(seconds), "raises": None}
            if seconds > (1 << 63) - 1:
                # Python's `int` is unbounded and this port's is not. Marked so the replay
                # compares "saturated high" rather than trying to fit a 31-digit number in an i64.
                row["exceeds_i64"] = True
            rows.append(row)
        except OverflowError as exc:
            # **Only** OverflowError escapes the reference: its `except ValueError` catches the
            # rest and answers 0. Catching `Exception` here made an empty string look like a
            # raise, which then looked like a port defect.
            rows.append({"eta_text": text, "seconds": None, "hms": None,
                         "raises": type(exc).__name__})
    return rows


# ---------------------------------------------------------------------------
# cast_local_file.py: the copy/convert decision
# ---------------------------------------------------------------------------

def gen_local_file_plan() -> dict:
    # One namespace: `plan` reads the constants and the tables, so they must live together.
    ns = {}
    exec(lift(LOCAL, "MAX_ADVERTISED_SIZE =", "AUDIO_SAMPLE_RATE ="), ns)
    exec(lift(LOCAL, "COPY_CONTAINERS =", "TARGETS = "), ns)
    exec(lift(LOCAL, "def plan(media, target", "# -- the conversion job"), ns)
    plan = ns["plan"]

    class Track:
        def __init__(self, codec):
            self.codec = codec

    class Media:
        def __init__(self, container, video, audio, audio_only):
            self.ok = True
            self.container = container
            self.audio_only = audio_only
            self.duration = 60.0
            self.video = [Track(video)] if video else []
            self.audio = [Track(c) for c in audio]

    rows = []
    for container in ("mp4", "mov", "m4v", "matroska", "webm", "avi", "flac", "wav", "mp3"):
        for video in ("h264", "hevc", "mpeg2video", ""):
            for audio_only in ((True, False) if video == "" else (False,)):
                for audio in (("aac",), ("ac3",), ("opus",), ("flac",), ("aac", "ac3"), ()):
                    if audio_only and not audio:
                        continue
                    for target in ("cast", "dlna"):
                        for mode in ("auto", "direct", "convert"):
                            verdict, reason = plan(
                                Media(container, video, audio, audio_only), target, mode=mode
                            )
                            rows.append({
                                "container": container, "video": video,
                                "audio": list(audio), "audio_only": audio_only,
                                "target": target, "mode": mode,
                                "copy": verdict == "copy", "reason": reason,
                            })
    return rows


# ---------------------------------------------------------------------------
# screen_mirror.py: profiles, queue arithmetic, encoder choice, knobs
# ---------------------------------------------------------------------------

MIRROR = "macast/plugins/renderer/screen_mirror.py"


def gen_mirror_profiles() -> dict:
    ns = {"__name__": "sm", "_DlnaProfile": lambda *a: a,
          "_mpeg2": lambda *a: ["-c:v", "mpeg2video"], "_AUDIO": (("a",), ("b",))}
    exec(lift(MIRROR, "OUTPUTS = {", "DEFAULT_OUTPUT ="), ns)
    exec(lift(MIRROR, "DLNA_PROFILES = {", "DEFAULT_DLNA_PROFILE ="), ns)
    exec(lift(MIRROR, "DLNA_PROFILE_LATENCY = {", "DLNA_PROFILE_LATENCY_SCOPE ="), ns)

    outputs = {
        key: {"label": value[0], "suffix": value[1], "content_type": value[2],
              "muxer": value[3] if value[3] is not None else None}
        for key, value in ns["OUTPUTS"].items()
    }
    profiles = {}
    for key, value in ns["DLNA_PROFILES"].items():
        profiles[key] = {
            "label": value[0], "muxer": value[1], "suffix": value[2],
            "content_type": value[3], "pn": value[4], "width": value[7],
            "height": value[8], "fps": value[9], "vbr": value[10], "abr": value[11],
        }
    return {"outputs": outputs, "profiles": profiles, "latency": ns["DLNA_PROFILE_LATENCY"]}


def gen_mirror_queue() -> dict:
    ns = {"__name__": "sm", "math": math, "SETTING": {}}
    # `text.index("FPS = ")` found the phrase "FPS = 41.7 ms" inside a comment. Pull the numeric
    # values by pattern instead, and let a missing one be loud.
    text = reference_text(MIRROR)
    for name in ("FPS", "LIVE_QUEUE_SECONDS", "AAC_FRAMES_PER_SECOND"):
        # The value must be the whole right-hand side: `48_000 / 1024.0` for one of them, and a
        # regex that stopped at the first number silently turned it into 48000.
        match = re.search(rf"^{name} = ([0-9][^#\n]*)$", text, re.M)
        if match is None:
            raise SystemExit(f"cannot find {name} in {MIRROR}")
        ns[name] = eval(match.group(1).strip(), {"__builtins__": {}})  # noqa: S307
        if name == "LIVE_QUEUE_SECONDS" and not 0 < ns[name] < 5:
            raise SystemExit(f"LIVE_QUEUE_SECONDS came out as {ns[name]}, which is not a budget")
    for name in ("MKV_CLUSTER_BYTES", "CHUNK", "LIVE_QUEUE_MIN_UNITS",
                 "LIVE_QUEUE_MIN_CHUNKS", "LIVE_QUEUE_MAX_CHUNKS"):
        # **The whole right-hand side, not the first integer.** `MKV_CLUSTER_BYTES = 60 << 10`
        # became 60 under a `(\d+)` pattern, which turned every Matroska hold into a hundredth of
        # a second and would have looked like a port defect.
        match = re.search(rf"^{name} = ([^#\n]+)$", reference_text(MIRROR), re.M)
        if match is None:
            raise SystemExit(f"cannot find {name} in {MIRROR}")
        ns[name] = eval(match.group(1).strip(), {"__builtins__": {}})  # noqa: S307
    if ns["MKV_CLUSTER_BYTES"] < 10_000:
        raise SystemExit(f"MKV_CLUSTER_BYTES came out as {ns['MKV_CLUSTER_BYTES']}")
    exec(lift(MIRROR, "def fragment_rate(", "def live_queue_units("), ns)
    exec(lift(MIRROR, "def live_queue_chunks(", "def live_queue_units("), ns)
    exec(lift(MIRROR, "def live_queue_units(", "def queue_hold_seconds("), ns)
    exec(lift(MIRROR, "def queue_hold_seconds(", "def start_stream_server("), ns)

    class Session:
        def __init__(self, marker, bitrate, has_audio):
            self.init_marker = marker
            self.bitrate = bitrate
            self.has_audio = has_audio

    # `live_queue_units` compares the session's marker against the module constant, so the
    # constant has to be in this namespace. It is the first Cluster element id (Matroska's own
    # re-sync point), which is also what `MKV_FIRST_CLUSTER` holds in the reference.
    MKV_FIRST_CLUSTER = b"\x1f\x43\xb6\x75"
    ns["MKV_FIRST_CLUSTER"] = MKV_FIRST_CLUSTER
    markers = {"moof": b"moof", "cluster": MKV_FIRST_CLUSTER, "chunk": b""}
    rows = []
    for name, marker in markers.items():
        for bitrate in (None, 600000, 1024000, 1280000, 4000000, 6000000, 20000000):
            for audio in (True, False):
                session = Session(marker, bitrate, audio)
                units = ns["live_queue_units"](session)
                # `live_queue_chunks` clamps with `min`/`max` over ints *and* a float, so Python
                # can hand back a float here (`91.0`). The Rust port counts `usize`, so the corpus
                # must say what the reference actually returns and let the replay compare numbers.
                rows.append({
                    "marker": name, "bitrate": bitrate, "audio": audio,
                    "units": units,
                    "hold": ns["queue_hold_seconds"](session, units),
                })
    # The rounding boundaries the scan found, with *explicit* units.
    for name, marker, units, bitrate in (
        ("cluster", MKV_FIRST_CLUSTER, 48, 1024000),
        ("cluster", MKV_FIRST_CLUSTER, 51, 1088000),
        ("cluster", MKV_FIRST_CLUSTER, 12, 1280000),
        ("cluster", MKV_FIRST_CLUSTER, 20, 2000000),
        ("chunk", b"", 128, 600000),
        ("chunk", b"", 200, 1000000),
    ):
        session = Session(marker, bitrate, True)
        rows.append({
            "marker": name, "bitrate": bitrate, "audio": True, "units": units,
            "hold": ns["queue_hold_seconds"](session, units),
            "note": "rounding divergence",
        })
    return rows


def gen_mirror_encoder() -> dict:
    ns = {"__name__": "sm"}
    exec("VT_LEVELS = ((1080, '42'), (2160, '51'))\n"
         + lift(MIRROR, "def vt_level(", "def encoder_args("), ns)
    exec("VT_BITRATE_MULT = 1.5\n"
         + lift(MIRROR, "def uses_videotoolbox(", "VT_BITRATE_MULT = ")
         + lift(MIRROR, "def rate_target(", "VT_LEVELS = "), ns)
    rows = []
    for height in (0, 1, 480, 720, 1080, 1081, 1440, 2160, 2161, 2234, 2880, 4320):
        rows.append({"kind": "vt_level", "height": height, "out": ns["vt_level"](height)})
    for kind in ("hardware", "software"):
        for platform in ("darwin", "win32", "linux"):
            for bitrate in (600000, 4000000, 6000000, 20000000):
                rows.append({
                    "kind": "rate_target", "encoder": kind, "platform": platform,
                    "bitrate": bitrate,
                    "out": ns["rate_target"](bitrate, kind, platform),
                })
    return rows


def gen_mirror_knobs() -> dict:
    text = reference_text(MIRROR)
    ns = {"__name__": "sm"}
    for name in ("DLNA_PREFILL_SECONDS", "DLNA_PREFILL_MIN_SECONDS", "DLNA_PREFILL_MAX_SECONDS",
                 "DLNA_PREFILL_MIN_BYTES", "DLNA_PREFILL_MAX_BYTES", "LIVE_EDGE_SECONDS",
                 "LIVE_EDGE_MIN_SECONDS", "LIVE_EDGE_MAX_SECONDS",
                 "DEFAULT_MAX_DURATION_HOURS"):
        match = re.search(rf"^{name} = ([0-9.<< ]+)$", text, re.M)
        ns[name] = eval(match.group(1), {"__builtins__": {}})          # noqa: S307
    match = re.search(r"^MAX_DURATION_HOURS = \(([^)]*)\)", text, re.M)
    ns["MAX_DURATION_HOURS"] = tuple(int(v) for v in match.group(1).split(","))
    # The knob functions read `Setting`; stub it so the decision runs without the app.
    class Setting:
        values = {}

        @classmethod
        def get(cls, prop, default=None):
            # `prop` is a `SettingProperty` member; its `.value` is the key handle. Using
            # `str(prop)` produced `"<__main__._P object at 0x…>"`, so every lookup missed and
            # every knob answered its default — which looked like the port ignoring the setting.
            key = getattr(prop, "value", prop)
            return cls.values.get(key, default)

    ns["Setting"] = Setting
    class _P:
        def __init__(self, value): self.value = value
    class SettingProperty:
        # One attribute per key the knobs read; the names are the reference's, the values are
        # only handles for the stub `Setting`.
        Mirror_Dlna_Prefill = _P("prefill")
        Mirror_Max_Duration = _P("maxdur")
        Mirror_Live_Edge = _P("edge")
    ns["SettingProperty"] = SettingProperty

    def function_body(signature: str) -> str:
        """A top-level function's text, ending at the first line that is not indented or blank.

        `text.index("\ndef ")` was not enough: `live_edge_seconds`'s slice ran into the
        module-level `PLAYER_PAGE` block and pulled `QUALITIES`, `AUDIO_RESAMPLE` and the rest into
        the namespace, so the generator's behaviour depended on unrelated constants.
        """
        at = text.index(signature)
        lines = text[at:].splitlines(keepends=True)
        out = [lines[0]]
        for line in lines[1:]:
            if line.strip() and not line.startswith((" ", "\t")):
                break
            out.append(line)
        return "".join(out)

    for fn in ("def dlna_prefill_seconds_setting(", "def dlna_prefill_bytes(",
               "def dlna_prefill_seconds(", "def live_edge_seconds(",
               "def max_duration_hours(", "def bound_video_seconds("):
        exec(function_body(fn), ns)

    rows = []
    for stored in (None, 0, 1, 2, 4, 6, 8, 9, 40, -5):
        Setting.values = {"prefill": stored} if stored is not None else {}
        rows.append({"kind": "prefill_setting", "stored": stored,
                     "out": ns["dlna_prefill_seconds_setting"]()})
    for rate in (1000, 1000000, 4500000, 8000000, 20000000, 100000000):
        for stored in (None, 1, 4, 8):
            Setting.values = {"prefill": stored} if stored is not None else {}
            class Profile:
                total_bitrate = rate
            rows.append({"kind": "prefill", "rate": rate, "stored": stored,
                         "bytes": ns["dlna_prefill_bytes"](Profile),
                         "seconds": ns["dlna_prefill_seconds"](Profile)})
    for value in (None, 0.0, 0.1, 0.5, 1.0, 2.0, 3.0, 5.0, 99.0):
        Setting.values = {"edge": value} if value is not None else {}
        rows.append({"kind": "live_edge", "stored": value,
                     "out": ns["live_edge_seconds"]()})
    for stored in (None, 0, 12, 24, 48, 40, -1):
        Setting.values = {"maxdur": stored} if stored is not None else {}
        rows.append({"kind": "max_hours", "stored": stored,
                     "out": ns["max_duration_hours"]()})
    return rows


# ---------------------------------------------------------------------------
# media_resolve.py: py_float's underscore rules
# ---------------------------------------------------------------------------

def gen_py_float() -> dict:
    cases = ["1_000", "1_0.5", "1__0", "_1", "1_", "1_.5", "1._5", "0x1_0", "1e1_0",
             " 1_0 ", "inf", "nan", "1_0_0_0", "42", "-42", "+42", ".5", "42.", "1e400",
             "-inf", "Infinity", "1E3", ""]
    rows = []
    for text in cases:
        try:
            value = float(text)
            special = None
            if value != value:
                special, value = "nan", None
            elif value == float("inf"):
                special, value = "inf", None
            elif value == float("-inf"):
                special, value = "-inf", None
            rows.append({"text": text, "value": value, "special": special})
        except ValueError:
            rows.append({"text": text, "value": None, "special": "ValueError"})
    return rows


# ---------------------------------------------------------------------------
# nirvana.py: the NVA request line, frames, decoder and command table
# ---------------------------------------------------------------------------
#
# The plugin is 1,822 lines and imports `cheroot`, so nothing here can `import nirvana`. The four
# corpora come from lifting the parser's own text where it is self-contained, and from
# reimplementing `read_request_line`'s decision path branch for branch where it is not — with the
# line numbers named, as elsewhere in this file.

NIRVANA = "macast/plugins/protocol/nirvana.py"


def gen_nva_request() -> dict:
    """`NVAHTTPRequest.read_request_line` (`nirvana.py:266-340`), decision for decision."""
    CRLF, SPACE = b"\r\n", b" "

    def parse(line: bytes):
        if not line:
            return {"ok": False, "status": 408}
        if not line.endswith(CRLF):
            return {"ok": False, "status": 400, "why": "crlf"}
        try:
            method, uri, protocol = line.strip().split(SPACE, 2)
        except (ValueError, IndexError):
            return {"ok": False, "status": 400, "why": "malformed"}
        is_nva = b"NVA" in protocol
        if is_nva:
            protocol = protocol.replace(b"NVA/1.0", b"HTTP/1.0")
        if not protocol.startswith(b"HTTP/"):
            return {"ok": False, "status": 400, "why": "protocol"}
        parts = protocol[5:].split(b".", 1)
        if len(parts) != 2:
            return {"ok": False, "status": 400, "why": "version"}
        try:
            version = tuple(map(int, parts))
        except ValueError:
            return {"ok": False, "status": 400, "why": "malformed"}
        if version > (1, 1):
            return {"ok": False, "status": 505}
        return {
            "ok": True, "is_nva": is_nva,
            "method": method.upper().decode("latin-1"),
            "uri": uri.decode("latin-1"),
            "close_connection": is_nva,
        }

    lines = [
        b"GET http://h/a HTTP/1.0\r\n", b"GET http://h/a NVA/1.0\r\n",
        b"PLAY http://h/a NVA/1.0\r\n", b"GET http://h/a HTTP/1.1\r\n",
        b"GET http://h/a HTTP/2.0\r\n", b"GET http://h/a NVA/2.0\r\n",
        b"GET http://h/a FOO/1.0\r\n", b"GET http://h/a HTTP/1\r\n",
        b"GET  HTTP/1.0\r\n", b"GET http://h/a HTTP/1.0\n",
        b"", b"GET http://h/a HTTP/x.y\r\n", b"nva http://h/a NVA/1.0\r\n",
    ]
    return [{"line": line.decode("latin-1"), "result": parse(line)} for line in lines]


def gen_nva_frames() -> dict:
    """`send_res` / `send_ping` / `send_cmd` (`nirvana.py:860-893`)."""
    import struct
    RET, SEND, PING = b"\xc0", b"\xe0", b"\xe4"
    COMMAND = b"Command"

    rows = []
    for counter in (0, 1, 7, 255, 256, 65535, 2**31):
        rows.append({"kind": "res_empty", "counter": counter,
                     "hex": struct.pack(">cBI", RET, 0, counter).hex()})
        rows.append({"kind": "ping", "counter": counter,
                     "hex": struct.pack(">cBI", PING, 0, counter).hex()})
        for body in (b"{}", b'{"a":1}', b'{"a":1,"b":[1,2]}', "弹幕".encode()):
            rows.append({"kind": "res_body", "counter": counter, "body": body.decode(),
                         "hex": struct.pack(f">cB2I{len(body)}s", RET, 1, counter,
                                            len(body), body).hex()})
        for verb in ("pause", "seek", "play", "a"):
            encoded = verb.encode()
            rows.append({"kind": "cmd", "counter": counter, "command": verb,
                         "hex": struct.pack(f">cBI2B7sB{len(encoded)}s", SEND, 2, counter,
                                            1, 7, COMMAND, len(encoded), encoded).hex()})
            for body in (b"{}", b'{"p":2}'):
                rows.append({"kind": "cmd_body", "counter": counter, "command": verb,
                             "body": body.decode(),
                             "hex": struct.pack(f">cBI2B7sB{len(encoded)}sI{len(body)}s",
                                                SEND, 3, counter, 1, 7, COMMAND,
                                                len(encoded), encoded, len(body), body).hex()})
    return rows


def gen_nva_decode() -> dict:
    """`parse_request` (`nirvana.py:905-978`) with recording callbacks."""
    import struct
    RET, SEND, PING = b"\xc0", b"\xe0", b"\xe4"
    COMMAND = b"Command"

    def res(counter, body=None):
        if body is None:
            return struct.pack(">cBI", RET, 0, counter)
        return struct.pack(f">cB2I{len(body)}s", RET, 1, counter, len(body), body)

    def cmd(counter, verb, body=None):
        encoded = verb.encode()
        if body is None:
            return struct.pack(f">cBI2B7sB{len(encoded)}s", SEND, 2, counter, 1, 7,
                               COMMAND, len(encoded), encoded)
        return struct.pack(f">cBI2B7sB{len(encoded)}sI{len(body)}s", SEND, 3, counter,
                           1, 7, COMMAND, len(encoded), encoded, len(body), body)

    def parse_events(chunks):
        cache, events = b"", []

        def cmd_from_client(method, counter, params=None):
            events.append(["cmd", method, counter, params])

        def res_from_client(counter, params=None):
            events.append(["res", counter, params])

        for data in chunks:
            index = 0
            cache += data
            data, length = cache, len(cache)
            while index < length:
                if length - index < 6:
                    break
                if data[index] not in (SEND[0], RET[0]):
                    index += 1
                    continue
                try:
                    _header, flags, counter = struct.unpack(">cBI", data[index:index + 6])
                    if data[index] == SEND[0]:
                        assert flags in (2, 3)
                        if length - index - 6 < 10:
                            break
                        n, field_len, field, verb_len = struct.unpack(
                            ">BB7sB", data[index + 6:index + 16])
                        assert n == 1 and field_len == 7 and field == COMMAND
                        if flags == 2:
                            if length - index - 16 < verb_len:
                                break
                            verb = struct.unpack(f">{verb_len}s",
                                                 data[index + 16:index + 16 + verb_len])[0]
                            cmd_from_client(verb.decode(), counter)
                            index += 16 + verb_len
                        else:
                            if length - index - 16 < verb_len + 4:
                                break
                            verb, body_len = struct.unpack(
                                f">{verb_len}sI", data[index + 16:index + verb_len + 20])
                            if length - index - verb_len - 20 < body_len:
                                break
                            body = struct.unpack(
                                f">{body_len}s",
                                data[index + verb_len + 20:index + verb_len + 20 + body_len])[0]
                            try:
                                body = json.loads(body)
                            except Exception:                       # noqa: BLE001
                                pass                                    # no callback
                            else:
                                cmd_from_client(verb.decode(), counter, body)
                            finally:
                                index += verb_len + 20 + body_len
                    else:
                        assert flags in (0, 1)
                        if flags == 0:
                            res_from_client(counter)
                            index += 6
                        else:
                            if length - index - 6 < 4:
                                break
                            body_len = struct.unpack(">I", data[index + 6:index + 10])[0]
                            if length - index - 10 < body_len:
                                break
                            body = struct.unpack(
                                f">{body_len}s",
                                data[index + 10:index + 10 + body_len])[0]
                            try:
                                body = json.loads(body)
                            except Exception:                       # noqa: BLE001
                                pass
                            else:
                                res_from_client(counter, body)
                            finally:
                                index += 10 + body_len
                except Exception:                                   # noqa: BLE001
                    index += 1
            cache = data[index:]
        return events

    cases = {
        "single_res": [res(5, b'{"volume":0}')],
        "two_frames": [cmd(1, "GetVolume") + res(2, b'{"volume":50}')],
        "byte_at_a_time": [bytes([b]) for b in cmd(3, "OnProgress", b'{"position":64}')],
        "junk_then_frames": [b"\xff\x00\x13\x99" + res(1) + res(2)],
        "ping_then_res": [struct.pack(">cBI", PING, 0, 1) + res(9)],
        "bad_json": [res(4, b"not json")],
        "bad_flag": [bytes([0xe0, 9, 0, 0, 0, 1]) + res(7)],
        "two_chunks": [cmd(1, "pause")[:8], cmd(1, "pause")[8:] + res(2, b"{}")],
    }
    return [{"case": name, "chunks": [c.hex() for c in chunks],
             "events": parse_events(chunks)} for name, chunks in cases.items()]


def gen_nva_commands() -> dict:
    """`cmd_from_client` (`nirvana.py:1204-1340`), arm structure only."""
    def dispatch(method, params):
        events, raised = [], None

        def send_res(*_a, **_k):
            events.append("res")

        try:
            if method == "GetVolume":
                send_res(); events.append("GetVolume")
            elif method == "SetVolume":
                if params is None:
                    raise TypeError()
                params.get("volume", -1)          # -1 fails the range check, no raise
                events.append("SetVolume")
            elif method == "Pause":
                send_res(); events.append("Pause")
            elif method == "Resume":
                send_res(); events.append("Resume")
            elif method == "SendDanmaku":
                send_res(); events.append("SendDanmaku")
            elif method == "SwitchDanmaku":
                send_res(); events.append("SwitchDanmaku")
            elif method == "SwitchSpeed":
                float(params["speed"]); events.append("SwitchSpeed")
            elif method == "SwitchQn":
                send_res(); params["qn"]; events.append("SwitchQn")
            elif method == "Stop":
                send_res(); events.append("Stop")
            elif method == "Play":
                send_res(); params["aid"]; params["cid"]; events.append("Play")
            elif method == "PlayUrl":
                send_res(); params["url"]; events.append("PlayUrl")
            elif method == "Seek":
                send_res()
                if params is None:
                    raise TypeError()
                params.get("seekTs", 0); events.append("Seek")
            else:
                send_res()
        except (KeyError, TypeError) as exc:
            raised = type(exc).__name__
        return {"events": events, "raised": raised}

    cases = [("GetVolume", None), ("Pause", None), ("Pause", {"x": 1}),
             ("SetVolume", None), ("SetVolume", {"volume": 50}), ("SetVolume", {"other": 1}),
             ("SwitchSpeed", None), ("SwitchSpeed", {"speed": "2"}),
             ("SwitchQn", {"qn": 80}), ("Play", {"aid": 1}), ("Play", {"aid": 1, "cid": 2}),
             ("PlayUrl", {"url": "http://x"}), ("Seek", {"seekTs": 30}), ("Seek", None),
             ("Unknown", None)]
    return [{"method": m, "params": p, "result": dispatch(m, p)} for m, p in cases]


# ---------------------------------------------------------------------------
# Constants: every number the port copied by hand out of `screen_mirror.py`
# ---------------------------------------------------------------------------
#
# This exists because one of them was copied wrong: `MKV_CLUSTER_BYTES` became 55_000 (the midpoint
# of a comment that said "50–60 KB") instead of `60 << 10`. The port and its corpus agreed with each
# other because both started from the same misreading — the failure mode a differential corpus is
# supposed to prevent. Now the reference's own text is the source, and a hand-copied value has
# somewhere to be checked.

CONSTANT_SITES = {
    "mirror_queue": (
        "macast/plugins/renderer/screen_mirror.py",
        "macast-cast/src/mirror_queue.rs",
        ["LIVE_QUEUE_SECONDS", "LIVE_QUEUE_MIN_CHUNKS", "LIVE_QUEUE_MAX_CHUNKS",
         "LIVE_QUEUE_MIN_UNITS", "CHUNK", "MKV_CLUSTER_BYTES", "FPS", "TS_PACKET"],
    ),
    "mirror_knobs": (
        "macast/plugins/renderer/screen_mirror.py",
        "macast-cast/src/mirror_knobs.rs",
        ["DLNA_PREFILL_SECONDS", "DLNA_PREFILL_MIN_SECONDS", "DLNA_PREFILL_MAX_SECONDS",
         "DLNA_PREFILL_MIN_BYTES", "DLNA_PREFILL_MAX_BYTES", "LIVE_EDGE_SECONDS",
         "LIVE_EDGE_MIN_SECONDS", "LIVE_EDGE_MAX_SECONDS", "DEFAULT_MAX_DURATION_HOURS"],
    ),
    "mirror_encoder": (
        "macast/plugins/renderer/screen_mirror.py",
        "macast-cast/src/mirror_encoder.rs",
        ["VT_BITRATE_MULT"],
    ),
    # `resolve.rs`: the timeouts and quotas AGENTS.md calls a family. The extraction patterns for
    # these live in `gen_constants` and differ from the mirror ones (class attributes, `u64`
    # typed, `2 << 20` style), which is why each group names its own parser below.
    "resolve": (
        "macast/media_resolve.py",
        "macast-cast/src/resolve.rs",
        ["SCRAPE_TIMEOUT", "YTDLP_TIMEOUT", "PROBE_TIMEOUT"],
    ),
}


#: `(python pattern, rust pattern)` overrides for groups whose spelling differs.
CONSTANT_PATTERNS = {
    "resolve": {
        # Python: `SCRAPE_TIMEOUT = 15`; Rust: `pub const SCRAPE_TIMEOUT_SECS: u64 = 15;`
        "SCRAPE_TIMEOUT": (r"^SCRAPE_TIMEOUT = (\d+)$",
                           r"pub const SCRAPE_TIMEOUT_SECS: u64 = ([^;]+);"),
        "YTDLP_TIMEOUT": (r"^YTDLP_TIMEOUT = (\d+)$",
                          r"pub const YTDLP_TIMEOUT_SECS: u64 = ([^;]+);"),
        "PROBE_TIMEOUT": (r"^PROBE_TIMEOUT = (\d+)$",
                          r"pub const PROBE_TIMEOUT_SECS: u64 = ([^;]+);"),
    },
}


def gen_constants() -> dict:
    """Reference value and Rust expression for each constant, so a drift names itself."""
    rows = []
    for group, (py_path, rs_path, names) in CONSTANT_SITES.items():
        py_src = reference_text(py_path)
        rs_src = reference_text(rs_path)
        overrides = CONSTANT_PATTERNS.get(group, {})
        for name in names:
            py_pattern, rs_pattern = overrides.get(
                name,
                (rf"^{name} = ([^#\n]+)$", rf"pub const {name}: [^=]+ = ([^;]+);"),
            )
            py_match = re.search(py_pattern, py_src, re.M)
            if py_match is None:
                raise SystemExit(f"cannot find {name} in {py_path} ({py_pattern})")
            rs_match = re.search(rs_pattern, rs_src)
            if rs_match is None:
                raise SystemExit(f"cannot find {name} in {rs_path} ({rs_pattern})")
            reference = eval(py_match.group(1).strip(), {"__builtins__": {}})  # noqa: S307
            expression = rs_match.group(1).strip()
            # Evaluate the Rust expression: it may be `60 << 10` or `2 << 20`.
            value = eval(expression.replace("_", ""), {"__builtins__": {}})   # noqa: S307
            rows.append({
                "group": group, "name": name,
                "reference": reference, "expression": expression, "value": value,
            })
    return rows


# ---------------------------------------------------------------------------
# Registration and CLI
# ---------------------------------------------------------------------------

CORPORA = {
    "parse_range": (CAST / "parse_range.json", gen_parse_range, "json"),
    "serve_plan": (CAST / "serve_plan.json", gen_serve_plan, "json"),
    "local_serve": (CAST / "local_serve.json", gen_local_serve, "json"),
    "local_file_plan": (CAST / "local_file_plan.jsonl", gen_local_file_plan, "jsonl"),
    "mirror_profiles": (CAST / "mirror_profiles.json", gen_mirror_profiles, "json"),
    "mirror_queue": (CAST / "mirror_queue.json", gen_mirror_queue, "json"),
    "mirror_encoder": (CAST / "mirror_encoder.json", gen_mirror_encoder, "json"),
    "mirror_knobs": (CAST / "mirror_knobs.json", gen_mirror_knobs, "json"),
    "py_float": (CAST / "py_float.json", gen_py_float, "json"),
    "ytdlp_eta": (MPV / "ytdlp_eta.json", gen_ytdlp_eta, "json"),
    "constants": (CAST / "constants.json", gen_constants, "json"),
    "nva_request": (UPNP / "nva_request.jsonl", gen_nva_request, "jsonl"),
    "nva_frames": (UPNP / "nva_frames.jsonl", gen_nva_frames, "jsonl"),
    "nva_decode": (UPNP / "nva_decode.jsonl", gen_nva_decode, "jsonl"),
    "nva_commands": (UPNP / "nva_commands.jsonl", gen_nva_commands, "jsonl"),
}


def render(name: str) -> str:
    path, generator, kind = CORPORA[name]
    data = generator()
    if kind == "jsonl":
        return "\n".join(json.dumps(row, ensure_ascii=False, sort_keys=True)
                         for row in data) + "\n"
    return json.dumps(data, ensure_ascii=False, indent=0) + "\n"


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("only", nargs="*", help="corpus names (default: all)")
    parser.add_argument("--check", action="store_true",
                        help="exit 1 if a committed corpus differs from what the reference says")
    args = parser.parse_args(argv)

    names = args.only or sorted(CORPORA)
    for name in names:
        if name not in CORPORA:
            print(f"unknown corpus: {name}", file=sys.stderr)
            return 2
    stale = []
    for name in names:
        path, _, _ = CORPORA[name]
        wanted = render(name)
        if args.check:
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current != wanted:
                stale.append(name)
                print(f"STALE {name}: {path.relative_to(ROOT)}", file=sys.stderr)
        else:
            path.write_text(wanted, encoding="utf-8")
            rows = wanted.count("\n") if path.suffix == ".jsonl" else wanted.count('{"')
            print(f"wrote {name}: {path.relative_to(ROOT)}")
    if args.check:
        if stale:
            print(f"{len(stale)} corpus file(s) stale; run scripts/build_corpora.py",
                  file=sys.stderr)
            return 1
        print(f"{len(names)} corpus file(s) match the reference")
    return 0


if __name__ == "__main__":
    sys.exit(main())
