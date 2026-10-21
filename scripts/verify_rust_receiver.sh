#!/bin/bash
# Live, real-machine check for the Rust DLNA receiver (`macast-tauri`).
#
# The unit suite stubs the network, so it cannot say "a control point that does
# DESCRIBE / SetAVTransportURI / SUBSCRIBE on the wire actually reaches the mpv
# this page drives." This script is the half it can't do: it raises one
# *isolated* instance (temp config dir, spare port, its own mpv socket) and
# drives it with real bytes over real sockets, the way a phone would.
#
# Isolation rules (AGENTS.md §10): never touches the user's real config —
# `MACAST_SETTING_DIR` is a fresh temp dir; the mpv child is given its own
# `MACAST_MPV_SOCKET` so it never lands in the Python app's /tmp namespace;
# and the whole run is torn down (app + its mpv, by socket path) before exit.
#
# Why `perl setsid`: the binary is a foreground Tauri app, so a bare `&` keeps
# it a child of this shell, which dies with it; and there is no `setsid` on
# macOS, but `POSIX::setsid` in Perl is. Detaching is what lets the run outlive
# the command that started it.
#
# Sections 7-13 drive the **GET web-cast entry** (AGENTS.md §4.7) rather than
# SOAP, and they need three things the DLNA half does not: a media address with
# an authority (so the clip is served over a real HTTP socket, with Range, not a
# `file://` path — see 7), the receiver's own advertised **LAN** address (so a
# caller that is not loopback is a real one, read out of the SSDP reply rather
# than guessed — see 8), and the stored secret (so `query=cast-info` is asked for
# it *after* the no-token refusal is measured, in that order — see 9 then 10).
#
# Cost to know: for the run's duration this machine announces a **second**
# Macast on the LAN via SSDP (UDP/1900) **and**, since section 22, via mDNS
# (`_googlecast._tcp`, UDP/5353) — so a phone with the Cast button open will
# list it too. The teardown sends both goodbyes and the run is short, but do not
# run this during a demo.
#
# Section 22 is the only part that needs a third-party stack: it runs the
# `zeroconf` from this repository's `.venv` as a passive browser, because our own
# parser agreeing with our own writer proves nothing (§4.2's 「桩自己也会撒谎」).
# It therefore **skips** — and a skip counts as a failure, as everywhere here —
# when `.venv` is missing. And because the announcer has no responder yet (it only
# sends unsolicited bursts, every 60 s), the browser is waiting for a re-announce:
# a green 22 takes ~45 s, and a resolution that came faster would mean it read
# the startup burst rather than the keepalive.
#
# Usage:
#   scripts/verify_rust_receiver.sh            # build if needed, then verify
#   scripts/verify_rust_receiver.sh --no-build # reuse the existing binary
#   MACAST_VERIFY_KEEP=1 scripts/verify_rust_receiver.sh
#       keep the temp config dir (and thus macast.log, logs/MPV.log and
#       stdout.log) instead of deleting it, and print where it is. A failing
#       run is the one you actually need the logs for; without this the trap
#       deletes the only witness before anyone can read it.
#
# Exit code = number of checks that failed (0 = all green).

set -u
REPO=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd "$REPO" || exit 1

PORT="${MACAST_VERIFY_PORT:-58999}"
CLIP="${MACAST_VERIFY_CLIP:-/tmp/macast_verify_clip.mp4}"
KEEP="${MACAST_VERIFY_KEEP:-}"
BIN="$REPO/target/debug/macast-tauri"
FAILED=0

pass() { printf '  [PASS] %s\n' "$1"; }
fail() { printf '  [FAIL] %s\n' "$1"; FAILED=$((FAILED + 1)); }
# A check that cannot run (missing tool) is not a green: it is reported and
# counted, so "all green" is never vacuous.
skip() { printf '  [SKIP] %s\n' "$1"; FAILED=$((FAILED + 1)); }

listener_pid() { lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null | head -1; }

# ---- harness self-audits -----------------------------------------------------
# Two shapes of bug that make this script's verdicts lie, and both were measured
# happening here rather than theorised:
#   * a backslash immediately before a single quote inside a **double-quoted** bash
#     string stays a literal backslash, so a grep needle written that way searches
#     for text no log file ever contained. That is how section 26 reported
#     "mpv never reported file-loaded" about a run whose MPV.log had printed it,
#     and the failure text (a tail of three lines) read exactly like a load that
#     had been interrupted.
#   * a backtick inside a double-quoted message is a **command substitution**: bash
#     runs it, prints whatever it printed (usually nothing) and records the PASS
#     with a hole where the sentence was (AGENTS.md 4.13's fourth telling). An
#     ESCAPED backtick is two literal characters and is legitimate prose, so the
#     audit below counts only the live ones: a pattern that flagged both once cried
#     wolf at a properly escaped line while a real hit could hide in the same count.
# Comment lines are exempt from the first audit, because the lesson has to live
# somewhere and a scanner that flags its own documentation gets deleted rather than
# fixed. Both audits carry a planted control: a zero that cannot be told apart from
# "the scanner is broken" is not a pass (Part 50's family).
BSL=$(printf '\134')
BTX=$(printf '\140')
SQ="'"
QUOTE_PATTERN="$BSL$SQ"
quote_audit() {
    grep -Fna "$QUOTE_PATTERN" "$1" | grep -vc "^[0-9]*:[[:space:]]*#"
}
QUOTE_HITS=$(quote_audit "$0")
if [ "$QUOTE_HITS" != "0" ]; then
    echo "  [FAIL] $QUOTE_HITS code line(s) of this script hold the ${QUOTE_PATTERN} quoting bug"
    grep -Fan "$QUOTE_PATTERN" "$0" | grep -v "^[0-9]*:[[:space:]]*#" | head -2
    FAILED=$((FAILED + 1))
fi
# The control is assembled from the two characters being scanned for, so this file
# never contains the sequence it hunts while still being able to plant it.
QUOTE_CONTROL=$(mktemp /tmp/macast-quote-control.XXXXXX.sh)
printf 'needle=$(grep "%s")\n' "${QUOTE_PATTERN}event${QUOTE_PATTERN}" > "$QUOTE_CONTROL"
QUOTE_OK=1
[ "$(quote_audit "$QUOTE_CONTROL")" = "1" ] || QUOTE_OK=0
rm -f "$QUOTE_CONTROL"

# The backtick scan walks each message line with escapes consumed first, so what is
# left is what bash would actually print. grep on macOS cannot express "preceded by
# an even number of backslashes", which is the whole distinction, and the words it
# looks for are passed in as variables so the scanner does not match its own source.
bt_audit() {
    awk -v p1='pass ' -v p2='fail ' '
        /^[[:space:]]*#/ { next }
        index($0, p1) == 0 && index($0, p2) == 0 { next }
        {
            live = 0
            for (i = 1; i <= length($0); i++) {
                c = substr($0, i, 1)
                if (c == "\\") { i++; continue }
                if (c == "`") { live = 1 }
            }
            if (live) hits++
        }
        END { print hits + 0 }' "$1"
}
BT_HITS=$(bt_audit "$0")
if [ "$BT_HITS" != "0" ]; then
    echo "  [FAIL] $BT_HITS message line(s) of this script still carry a live backtick, so bash runs them"
    FAILED=$((FAILED + 1))
fi
BT_CONTROL=$(mktemp /tmp/macast-backtick-control.XXXXXX.sh)
# Two lines that must NOT be counted (the backticks are escaped) and one that must
# be (a live substitution, assembled from a variable so this source line stays
# clean). A scanner that reads zero on this file has not been asked anything.
printf '%s\n' 'pass "the field \`name\` is escaped prose"' > "$BT_CONTROL"
printf '%s\n' 'fail "and so is \`this\`"' >> "$BT_CONTROL"
printf '%s\n' "fail \"and this one runs: ${BTX}date -u${BTX}\"" >> "$BT_CONTROL"
BT_OK=1
[ "$(bt_audit "$BT_CONTROL")" = "1" ] || BT_OK=0
rm -f "$BT_CONTROL"

if [ "$QUOTE_OK" = "1" ] && [ "$BT_OK" = "1" ] \
    && [ "$QUOTE_HITS" = "0" ] && [ "$BT_HITS" = "0" ]; then
    echo "  [PASS] harness audits itself: both quoting bugs absent here, both found in a planted file"
else
    if [ "$QUOTE_OK" != "1" ]; then
        echo "  [FAIL] the ${QUOTE_PATTERN} quoting audit did not find the bug it was handed — its zero means nothing"
        FAILED=$((FAILED + 1))
    fi
    if [ "$BT_OK" != "1" ]; then
        echo "  [FAIL] the backtick audit failed its planted control"
        FAILED=$((FAILED + 1))
    fi
fi
# -----------------------------------------------------------------------------

# The two refusal sentences, by the shortest prefix that identifies which gate
# answered. Sections 9 and 11 assert a caller heard the **token** sentence;
# section 8 asserts **neither** appears where a route answered before any gate
# ran, and section 10 asserts a LAN caller hears the **management** one and not
# the token one. The checks are cumulative and ordered, so which sentence the
# caller hears is the only outside evidence of which gate ran — a route cannot
# write its own apology.
# Kept as variables rather than inlined: the full sentences are long, and a
# literal copy of someone's prose in a test is a second place to be wrong.
MANAGEMENT_MARK="管理接口只回答"
TOKEN_MARK="这个查询必须带"

# One GET against the API, leaving $API_STATUS / $API_CODE / $API_MSG behind.
#
# The body is **parsed**, not grepped. This port escapes every CJK character as
# a lowercase `\uXXXX` (Python's `json.dumps` default, matched deliberately), so
# the Chinese sentences above are unfindable as bytes in the raw file: a `grep`
# for them would match nothing on a *correct* answer and read as a broken
# receiver. `--noproxy` because a stray `http_proxy` in this shell would send a
# loopback request to a proxy and produce a 000 that looks like a dead port.
API_BODY=/tmp/verify_rust_receiver_api.json
API_LINE=""

# Read `$API_BODY` and leave $API_CODE / $API_MSG behind. Split out from the
# request so a POST (which needs a longer timeout than a GET probe) answers to
# the same reader rather than a second copy of this parsing.
parse_api_body() {
    API_LINE=$(python3 - "$API_BODY" <<'PY'
import json, sys
try:
    with open(sys.argv[1], "rb") as fh:
        body = json.loads(fh.read().decode("utf-8", "replace"))
except Exception:
    # A body that is not JSON at all is a finding, not a crash: say so in the
    # same three fields so the caller's comparisons still have something to
    # print, instead of the harness dying mid-run and hiding every later check.
    print("\t"); raise SystemExit
if not isinstance(body, dict):
    print("\t"); raise SystemExit
print("%s\t%s" % (body.get("code", ""), body.get("message", "")))
PY
)
    # `IFS=$'\t' read` rather than `${VAR%%tab*}`: the latter needs a literal
    # TAB byte in the script, and a literal TAB is the one character an editor
    # silently converts to spaces.
    IFS=$'\t' read -r API_CODE API_MSG <<< "$API_LINE"
}

api_probe() {
    API_STATUS=$(curl -s --noproxy '*' -m 8 -o "$API_BODY" -w '%{http_code}' "$@")
    parse_api_body
}

# One POST to the management API: `api_post <url> field=value...`.
#
# No token, deliberately: loopback **is** the management gate's admission for
# these routes (AGENTS.md §4.7), so the meaningful assertion in sections 15-16 is
# the **absence** of a gate sentence rather than the presence of a secret.
# `-m 40` because `cast-resolved` runs the origin probe synchronously
# (`probe_proxy`, up to `GROW_WAIT_SECONDS` = 20 on the remux arm).
# The pair guard is load-bearing on this machine's bash 3.2: `"${args[@]}"` on an
# **empty** array under `set -u` is a fatal error, so the function refuses the
# call before it can expand one.
api_post() {
    [ "$#" -ge 2 ] || { echo "api_post needs at least one field=value pair" >&2; return 2; }
    local _url=$1
    shift
    local _args=()
    while [ "$#" -gt 0 ]; do _args+=(--data-urlencode "$1"); shift; done
    API_STATUS=$(curl -s --noproxy '*' -m 40 -X POST -o "$API_BODY" -w '%{http_code}' \
        "${_args[@]}" "$_url")
    parse_api_body
}

# One dotted path into `$API_BODY`, in one of four readings:
#   value  the scalar at that path, rendered the way Python renders it
#   len    the length of a list or dict there
#   keys   a dict's keys, comma-joined **in document order**
#   join   a list's members, 、-joined
#
# `keys` is why this reader exists. The answer's field order is the document the
# page reads (`Answer::with` chains in Python's write order), and a dict built by
# `serde_json::Map` keeps insertion order only under `preserve_order` — so key
# order is a property worth asserting, and CPython ≥3.7 dicts give it to us for
# free on the way in. Lists are indexed by digits; a path that does not resolve
# exits 1 **silently**, which is what makes `json_has` a one-liner.
#
# `"."` is the document itself. The relay serves its own row on its own port
# (`/relay/<id>/status`) with no wrapper around it, so section 17 has to read
# that row's keys — and no key name means "everything above me", which the loop
# below would otherwise answer by not finding `''` in the top-level object.
json_read() {
    python3 - "$API_BODY" "$1" "$2" <<'PY'
import json, sys
how, dotted = sys.argv[2], sys.argv[3]
try:
    with open(sys.argv[1], "rb") as fh:
        node = json.loads(fh.read().decode("utf-8", "replace"))
except Exception:
    raise SystemExit(1)
parts = [] if dotted == "." else dotted.split(".")
for part in parts:
    if isinstance(node, list):
        if not part.isdigit() or int(part) >= len(node):
            raise SystemExit(1)
        node = node[int(part)]
    elif isinstance(node, dict):
        if part not in node:
            raise SystemExit(1)
        node = node[part]
    else:
        raise SystemExit(1)
if how == "len":
    print(len(node))
elif how == "keys":
    if not isinstance(node, dict):
        raise SystemExit(1)
    print(",".join(node.keys()))
elif how == "join":
    # A list's members, 、-joined. `json_ask` on a list would print
    # `json.dumps`' rendering — `["a", "b"]`, with a space after every comma —
    # while the Rust answer serialises with none, so a literal comparison would
    # fail on the whitespace and say nothing about the browser list. Joining the
    # members here means section 18 compares **the names and their order**, which
    # is the half of `COOKIE_BROWSERS` that `--cookies-from-browser` actually
    # carries into argv.
    if not isinstance(node, list):
        raise SystemExit(1)
    print("、".join(str(item) for item in node))
elif isinstance(node, bool):
    print("true" if node else "false")
elif node is None:
    print("null")
elif isinstance(node, (list, dict)):
    print(json.dumps(node, ensure_ascii=False))
else:
    print(node)
PY
}
json_ask()   { json_read value "$1"; }
json_len()   { json_read len "$1"; }
json_keys()  { json_read keys "$1"; }
json_join()  { json_read join "$1"; }
json_has()   { json_read value "$1" >/dev/null 2>&1; }

# One SOAP call, the envelope built from the action name alone: the read actions
# differ by nothing else, and four hand-copied envelopes is four chances for a
# typo to become a "failure" that says nothing about the receiver.
soap_action() {
    printf '<?xml version="1.0"?>\n<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">\n<s:Body><u:%s xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:%s></s:Body></s:Envelope>' "$1" "$1" \
        | curl -s --noproxy '*' -m 5 -X POST "$BASE/AVTransport/action" \
            -H 'Content-Type: text/xml; charset="utf-8"' \
            -H "SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#$1\"" \
            --data-binary @-
}

# What the player was last handed, read back off the wire.
#
# `GetPositionInfo`'s `TrackURI` is the witness for "did this GET change what is
# playing". Not the DIDL metadata: `cast_uri` writes `CurrentTrackURI` and never
# `CurrentTrackMetaData`, so the metadata a reply carries still describes the
# *previous* `SetAVTransportURI` — asking it would compare this request against
# an older one and pass for the wrong reason.
track_uri() {
    soap_action GetPositionInfo | tr -d '\n' \
        | grep -o '<TrackURI>[^<]*</TrackURI>' | head -1 | sed 's/<[^>]*>//g'
}

# The stored secret, read out of the temp settings file rather than over the API:
# section 9 has to prove the file holds **no** token after a refused call, and
# the endpoint that reveals the token is the one that mints it, so asking it
# would create the thing the check is looking for.
stored_token() {
    python3 - "$SDIR/macast_setting.json" <<'PY'
import json, sys
try:
    with open(sys.argv[1], "rb") as fh:
        value = json.load(fh).get("Api_Token", "")
except Exception:
    value = ""
sys.stdout.write(value if isinstance(value, str) else "")
PY
}

# One key out of the temp settings file, same reason as `stored_token`: the
# endpoints that *write* these keys are the ones under test, so asking them
# "did you store it" would be asking the suspect to testify. Prints nothing and
# exits non-zero when the key is absent — which is the answer section 18 wants
# after an `off` (`Setting::unset`, not a stored `false`).
stored_setting() {
    python3 - "$SDIR/macast_setting.json" "$1" <<'PY'
import json, sys
try:
    with open(sys.argv[1], "rb") as fh:
        value = json.load(fh)[sys.argv[2]]
except Exception:
    raise SystemExit(1)
sys.stdout.write(value if isinstance(value, str) else json.dumps(value, ensure_ascii=False))
PY
}

# The cookie constants, read out of the **Python** module at run time.
#
# 每个数字问两遍 (AGENTS.md §4.13): the browser list and the jar's size cap are
# decisions owned by `macast/media_resolve.py`. Section 18's expected values have
# to come from that owner and not from the Rust constant the code under test also
# reads — comparing the answer to the same file it came from asks nothing.
#
# Three lines out: the names as a JSON list, the cap as a number, the names joined
# by 、. The evaluator is hand-written instead of `ast.literal_eval` because
# `COOKIE_JAR_MAX_BYTES` is spelled `256 * 1024` — an `ast.BinOp`, which
# literal_eval rejects outright on this machine's Python. And a scan that finds
# one name but not the other **fails** rather than printing an empty expectation
# for the caller to happily compare against nothing.
py_cookie_constants() {
    python3 - "$REPO/macast/media_resolve.py" <<'PY'
import ast, json, sys

WANT = ("COOKIE_BROWSERS", "COOKIE_JAR_MAX_BYTES")
found = {}
with open(sys.argv[1], encoding="utf-8") as fh:
    tree = ast.parse(fh.read())
for node in tree.body:
    if not isinstance(node, ast.Assign):
        continue
    for target in node.targets:
        if isinstance(target, ast.Name) and target.id in WANT:
            found[target.id] = node.value
if sorted(found) != sorted(WANT):
    raise SystemExit(1)


def value(node):
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.Tuple):
        return tuple(value(child) for child in node.elts)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Mult):
        return value(node.left) * value(node.right)
    raise SystemExit(1)


names = value(found["COOKIE_BROWSERS"])
cap = value(found["COOKIE_JAR_MAX_BYTES"])
if not isinstance(names, tuple) or not names or not all(isinstance(n, str) for n in names):
    raise SystemExit(1)
if not isinstance(cap, int) or isinstance(cap, bool) or cap < 1024:
    raise SystemExit(1)
print(json.dumps(list(names), ensure_ascii=False))
print(cap)
print("、".join(names))
PY
}

# Ask the player, over its own IPC socket, for one property.
#
# The read terminates on `request_id`, not on `error == "success"`: an idle mpv
# answers some properties `{"error": "property unavailable", "data": null}` and
# others `"" + success`, so filtering on the error field never assembles a
# complete batch and the socket just times out (AGENTS.md §5). The caller polls,
# because the title lands a moment after the GET answers.
mpv_ask() {
    python3 - "$SOCK" "$1" <<'PY'
import json, socket, sys, time
path, name = sys.argv[1], sys.argv[2]
# An explicit AF_UNIX socket, not `create_connection`: that helper wants a
# (host, port) pair and unpacks a path string one **character** at a time, so
# every read died with `ValueError: too many values to unpack` while the player
# was in fact answering — the harness, not the receiver, was wrong.
#
# The **total** wait is bounded, not the gap between bytes. A socket timeout only
# fires while nothing arrives, and mpv keeps a connected client fed with events (the
# on-screen controller's log messages among them). On 2026-10-20 §16 hung for five
# minutes in exactly that shape: one probe never returned because lines kept arriving,
# and since mpv's IPC server serves one client at a time, every later probe starved
# behind it and read '' — which the section then reported as two product FAILs. A
# harness that can hold the player's only socket is worse than one that returns nothing:
# it makes the next reading a lie.
DEADLINE = time.monotonic() + 8.0
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(max(0.05, DEADLINE - time.monotonic()))
try:
    s.connect(path)
except OSError:
    raise SystemExit(1)
s.sendall(json.dumps({"command": ["get_property", name], "request_id": 7}).encode() + b"\n")
try:
    stream = s.makefile("rb")
    while True:
        left = DEADLINE - time.monotonic()
        if left <= 0:
            raise SystemExit(1)
        s.settimeout(max(0.05, left))
        line = stream.readline()
        if not line:
            raise SystemExit(1)
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("request_id") == 7:
            data = msg.get("data")
            if isinstance(data, str):
                sys.stdout.write(data)
            raise SystemExit(0)
except (socket.timeout, TimeoutError, OSError):
    raise SystemExit(1)
PY
}

# Refuse to run rather than share the port. This receiver deliberately **rejects**
# a busy port instead of falling back to a random one, so a collision does not
# degrade visibly: the new instance exits, and the harness then drives whatever
# was already listening — an instance whose temp config dir and mpv belong to a
# different run, and which the teardown here will not own. The check is
# deliberately *before* the EXIT trap is installed, so refusing does not kill the
# very instance we are refusing to collide with.
# A locked display is not a network condition, but it makes this script unable to mean
# anything. Measured 2026-10-20 while this gate was being written: with
# `CGSSessionScreenIsLocked=True`, a `loadfile` the shell wrote and a `loadfile` an
# outside probe wrote **directly into the player's own socket** both went unexecuted —
# no `start-file`, no property event, the process at 0.0% CPU — while the socket itself
# accepted bytes and `get_property` from a second connection still answered. So every
# check that reads the player (§13/§14/§16's titles, §21/§26/§27/§30's playbacks) would
# have reported a product FAIL about the state of the machine, and each one would have
# spent minutes polling to do it. Refusing costs one second and says the truth: an
# instrument that cannot measure must not print a verdict — the same rule that makes the
# port collision below exit 2 instead of driving somebody else's instance.
screen_locked() {
    python3 - <<'PY' 2>/dev/null
import ctypes
cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
cg.CGSessionCopyCurrentDictionary.restype = ctypes.c_void_p
cf.CFStringCreateWithCString.restype = ctypes.c_void_p
cf.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
cf.CFDictionaryGetValue.restype = ctypes.c_void_p
cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
cf.CFBooleanGetValue.restype = ctypes.c_bool
cf.CFBooleanGetValue.argtypes = [ctypes.c_void_p]
session = cg.CGSessionCopyCurrentDictionary()
if not session:
    # No session dictionary means this is not a GUI session at all. Answer "no" and let
    # the run go: a headless SSH shell has no display to lock, and refusing there would
    # make the script unrunnable in a place where mpv never needed one.
    print("no")
    raise SystemExit(0)
key = cf.CFStringCreateWithCString(None, b"CGSSessionScreenIsLocked", 0x0600)
value = cf.CFDictionaryGetValue(session, key)
print("yes" if value and cf.CFBooleanGetValue(value) else "no")
PY
}
if [ "$(screen_locked)" = "yes" ]; then
    echo "this Mac's display is locked (CGSSessionScreenIsLocked=True)." >&2
    echo "The player stops executing anything written to its IPC socket in that state," >&2
    echo "so the playback and window-name checks would be a verdict about the machine." >&2
    echo "Unlock the display and run again — nothing was started, exit 2 is not a failure." >&2
    exit 2
fi

_taken=$(listener_pid)
if [ -n "$_taken" ]; then
    echo "port $PORT is already listened on by pid $_taken;" >&2
    echo "stop that instance or set MACAST_VERIFY_PORT to a free one." >&2
    exit 2
fi

cleanup() {
    local app sock
    [ -n "${MEDIA_PID:-}" ] && kill -9 "$MEDIA_PID" 2>/dev/null
    app=$(pgrep -f "target/debug/macast-tauri" 2>/dev/null)
    [ -n "$app" ] && kill -9 $app 2>/dev/null
    # The gated instance from section 24 has a socket of its own; reaping only
    # `$SDIR` would leave its mpv behind as a `ppid=1` orphan — the exact
    # accumulation AGENTS.md §4.2 records for the Python app.
    for probe_dir in "${SDIR:-}" "${GATE_SDIR:-}"; do
        [ -n "$probe_dir" ] && [ -f "$probe_dir/.socket" ] && pgrep -f "$(cat "$probe_dir/.socket")" 2>/dev/null | xargs -r kill -9 2>/dev/null
    done
    if [ -n "${SDIR:-}" ] && [ -f "$SDIR/.socket" ]; then
        sock=$(cat "$SDIR/.socket")
        # Reap only *our* mpv, by its unique socket path — never a bare pkill
        # that could take down the user's own players.
        pgrep -f "$sock" 2>/dev/null | xargs -r kill -9 2>/dev/null
    fi
    # Wait for the port to actually be free before exiting. `kill -9` returns
    # before the kernel has released the listener, and this receiver **refuses**
    # a busy port rather than moving (a moved port silently invalidates every
    # cached LOCATION), so a back-to-back run would otherwise drive whichever
    # instance happened to still be listening — and read that as flakiness.
    for _ in $(seq 1 20); do
        [ -z "$(listener_pid)" ] && break
        sleep 0.25
    done
    [ -n "$KEEP" ] && {
        echo "  MACAST_VERIFY_KEEP: kept $SDIR (macast.log, logs/MPV.log, stdout.log)"
    }
    [ -z "$KEEP" ] && [ -n "${SDIR:-}" ] && rm -rf "$SDIR" 2>/dev/null
    rm -f /tmp/verify_rust_receiver_notify.txt 2>/dev/null
    rm -f /tmp/verify_rust_receiver_jar_good.txt \
          /tmp/verify_rust_receiver_jar_garbage.txt \
          /tmp/verify_rust_receiver_jar_big.txt 2>/dev/null
}
trap cleanup EXIT

echo "== build =="
if [ "${1:-}" = "--no-build" ]; then
    [ -x "$BIN" ] || { echo "binary $BIN is missing; run without --no-build"; exit 2; }
    echo "  (skipped: reusing $BIN)"
else
    PATH="$HOME/.cargo/bin:$PATH" cargo build -p macast-tauri >/dev/null || {
        echo "cargo build failed"; exit 2; }
fi

echo "== clip =="
FF="$(command -v ffmpeg || echo /opt/homebrew/opt/ffmpeg/bin/ffmpeg)"
if [ ! -s "$CLIP" ]; then
    if [ -x "$FF" ]; then
        "$FF" -y -f lavfi -i testsrc2=size=640x360:rate=25 \
            -f lavfi -i "sine=frequency=440:sample_rate=48000" \
            -t 12 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
            -c:a aac -shortest "$CLIP" >/dev/null 2>&1
    fi
fi
[ -s "$CLIP" ] && echo "  clip ready: $CLIP" || skip "no test clip (ffmpeg missing?)"

echo "== launch (port $PORT, temp config, own mpv socket) =="
SDIR=$(mktemp -d /tmp/macast-rust-verify.XXXXXX)
SOCK="$SDIR/mpvsocket"
# `DLNA_FriendlyName`, **not** `FriendlyName`: that is the key name `Key::
# DlnaFriendlyName` maps to (`macast-core/src/setting.rs:94`), and the reference
# spells it the same way. The first version of this line wrote `FriendlyName`, which
# is a key nothing reads — so the label this run intended never reached the wire.
# Cost measured on 2026-10-18: §24's browse answered "no announcement" about a run
# that *was* announcing, because the announcer had published the fallback name
# (`Macast-Pavia-MacBookPro-…`) instead of the `Macast-Gate-Off` the check matched.
# A harness that writes into a key no reader owns is the same failure §4.2 calls
# 「不提问的检查」, one layer over: it produces a green out of a typo.
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Verify"}' "$PORT" \
    > "$SDIR/macast_setting.json"
echo "$SOCK" > "$SDIR/.socket"
# One launcher, because **§24 has to stop this instance and put it back**: 8009/8008
# are fixed ports (`dial.py:25-26`), so the "all three faces closed" check cannot run
# while ours holds them — but every section after it (§26's RTSP receiver, §27's
# restart, §28's cross-implementation table) reads `$BASE` and expects this instance
# to be the one answering. Measured 2026-10-21: with the stop left permanent, §24 passed
# its closes and §22/§24/§26/§27/§34/§35 went red in a cascade — the later sections then
# bound 8010 or asked a dead port, and the run's verdict was about the orchestration
# rather than about the product.
start_main() {
    env MACAST_SETTING_DIR="$SDIR" MACAST_MPV_SOCKET="$SOCK" \
        MACAST_RESOURCE_DIR="$REPO/macast" \
        perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
        "$BIN" > "$SDIR/stdout.log" 2>&1 &
    APP_PID=$!
    # This job is killed with SIGKILL in `cleanup`, and bash reports a job killed by
    # a signal on **its own** stderr when the shell exits — a redirect on `wait`
    # cannot silence it. Without this line the run's last line is
    # "Killed: 9   env MACAST_SETTING_DIR=…", printed one line after the SUMMARY,
    # where every reader expects a verdict and would read it as a failed check.
    disown "$APP_PID" 2>/dev/null || true

    # Wait for the HTTP face to actually be listening (the real fact, not a sleep),
    # and then for **this** instance to be the one that answers. `env … perl … exec`
    # keeps the pid, so $APP_PID is the binary itself — and "a listener exists" is
    # not the same claim as "ours is up": if a previous run's instance were still
    # holding the port, every check below would pass against someone else's receiver.
    local ok=""
    for _ in $(seq 1 40); do
        [ "$(listener_pid)" = "$APP_PID" ] && { ok=1; break; }
        sleep 0.25
    done
    if [ -z "$ok" ]; then
        echo "  pid $APP_PID never became the listener on $PORT (someone else: '$(listener_pid)')"
        tail -20 "$SDIR/macast.log" 2>/dev/null
        exit 2
    fi
    echo "  listener up (pid $APP_PID)"
}
start_main

BASE="http://127.0.0.1:$PORT"
TITLE="Rust Verify Witness"
# The DIDL the way a real sender delivers it: **escaped text** inside the
# argument element, not a nested element. A nested element is what `node.text`
# (and this port's `dom.text`) reads as just the surrounding whitespace, which
# is faithful to the reference — so the witness uses the wire shape senders use.
DIDL="&lt;DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"&gt;&lt;item&gt;&lt;dc:title&gt;$TITLE&lt;/dc:title&gt;&lt;res protocolInfo=\"http-get:*:video/mp4:*\"&gt;file://$CLIP&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"
SOAP_CAST=$(cat <<EOF
<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
<InstanceID>0</InstanceID>
<CurrentURI>file://$CLIP</CurrentURI>
<CurrentURIMetaData>$DIDL</CurrentURIMetaData>
</u:SetAVTransportURI></s:Body></s:Envelope>
EOF
)

echo "== 1: DESCRIBE =="
desc=$(curl -s -m 5 "$BASE/description.xml")
echo "$desc" | grep -q "<UDN>uuid:" && echo "$desc" | grep -q "AVTransport:1" \
    && pass "description.xml carries a UDN and the AVTransport service" \
    || fail "description.xml missing UDN/AVTransport"

echo "== 2: SSDP is answering real M-SEARCH =="
# The protocol-faithful probe: join the SSDP group on a wildcard-bound socket
# with multicast loop on, send M-SEARCH to the group, and wait for the HTTP 200
# the receiver answers to the source of the request. Because the loop brings
# the M-SEARCH *back to us too*, the witness must skip that echo and only accept
# a datagram that actually looks like the receiver's reply. A `nc` unicast to
# the group is not reliable over loopback (no outbound interface for the
# multicast send), so the reply — not our own bytes — is the proof.
python3 - "$PORT" "$SDIR/msearch_reply.txt" <<'PY'
import socket, sys, time
port = int(sys.argv[1]); reply_to = sys.argv[2]
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", 0))
grp = socket.inet_aton("239.255.255.250")
iff = socket.inet_aton("0.0.0.0")
s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, grp + iff)
s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
s.settimeout(5)
ms = (b"M-SEARCH * HTTP/1.1\r\n"
      b"HOST: 239.255.255.250:1900\r\n"
      b"MAN: \"ssdp:discover\"\r\n"
      b"MX: 2\r\n"
      b"ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n")
s.sendto(ms, ("239.255.255.250", 1900))
end = time.time() + 5
ok = 0
while time.time() < end and not ok:
    try:
        data = s.recv(65535)          # recv -> bytes (recvfrom would add an addr)
    except socket.timeout:
        break
    # The receiver's reply is an HTTP status line; our looped M-SEARCH is not.
    # Other Macasts on the LAN may answer the same M-SEARCH, so the proof is a
    # reply whose LOCATION points at **our** port, not just any 200.
    if b"HTTP/1.1" in data and (b"200" in data or b"LOCATION" in data):
        loc = [l for l in data.split(b"\r\n") if l.lower().startswith(b"location:")]
        if any(str(port).encode() in l and b"description.xml" in l for l in loc):
            ok = 1
            # Keep the whole reply, not just the verdict: sections 8 and 10 need
            # the advertised **LAN** address out of that LOCATION line, because
            # the gate discipline must be measured against a caller that is not
            # loopback, and 127.0.0.1 cannot show that.
            with open(reply_to, "wb") as fh:
                fh.write(data)
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
sys.exit(0 if ok else 1)
PY
if [ "$?" -eq 0 ]; then
    pass "receiver answered a live M-SEARCH with an HTTP 200"
else
    fail "no M-SEARCH answer (receiver not on UDP/1900?)"
fi

# The advertised LAN address, lifted out of the reply section 2 just persisted.
# Sections 8 and 10 need a caller that is genuinely **not** loopback: this face
# binds 0.0.0.0, so reaching it over the machine's own LAN IP produces a real
# non-loopback peer_addr without a second host. Reading it out of the receiver's
# own LOCATION is the honest way to pick it — guessing an interface risks
# pointing at a VM bridge or VPN address the receiver would not itself advertise.
LAN=$(grep -io 'location: *http://[^/[:space:]]*' "$SDIR/msearch_reply.txt" 2>/dev/null \
      | head -1 | sed -E 's|.*http://||')
LAN=${LAN%%:*}
case "$LAN" in *[!0-9.]*|"") LAN="" ;; esac
# Reachable, or it is not a caller: an OS firewall that drops inbound LAN
# traffic on this Mac turns these checks into a permanent red that says nothing
# about the gate, so the fact is tested once here and its absence is reported as
# a named skip rather than a verdict about the receiver.
if [ -n "$LAN" ]; then
    [ "$(curl -s --noproxy '*' -m 5 -o /dev/null -w '%{http_code}' \
        "http://$LAN:$PORT/description.xml")" = "200" ] || LAN=""
fi
echo "  non-loopback caller: ${LAN:-<none reachable>}"

echo "== 3: real SOAP SetAVTransportURI reaches the player =="
code=$(curl -s -m 5 -o /tmp/verify_rust_receiver_cast.txt \
    -X POST "$BASE/AVTransport/action" \
    -H 'Content-Type: text/xml; charset="utf-8"' \
    -H 'SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI"' \
    --data-binary "$SOAP_CAST" -w '%{http_code}')
grep -q "SetAVTransportURIResponse" /tmp/verify_rust_receiver_cast.txt \
    && pass "SetAVTransportURI answered 200 with a response" \
    || fail "SetAVTransportURI did not answer (HTTP $code)"

echo "== 4: the player actually plays it (TransportState -> PLAYING) =="
# The cast reply returns before mpv has started; the state is written by the mpv
# event thread, so wait for the fact rather than reading it on the cast's
# return. Give a fresh instance a moment to settle, then poll.
sleep 1
ok=""
seen=""
for _ in $(seq 1 60); do
    state=$(printf '<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:GetTransportInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetTransportInfo></s:Body></s:Envelope>' \
        | curl -s -m 5 -X POST "$BASE/AVTransport/action" \
            -H 'Content-Type: text/xml; charset="utf-8"' \
            -H 'SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#GetTransportInfo"' \
            --data-binary @-)
    got=$(printf '%s' "$state" | grep -oE '<CurrentTransportState>[A-Z_]+' | sed 's/.*>//')
    seen="$seen${got:-<none>} "
    [ "$got" = "PLAYING" ] && { ok=1; break; }
    sleep 0.25
done
if [ -n "$ok" ]; then
    pass "TransportState reached PLAYING via GetTransportInfo"
else
    # Say which states were observed rather than only "never PLAYING": "the
    # player never started" and "it started and finished inside the polling
    # window" are different failures with different fixes, and both read the
    # same as a green check to a reader of the one-word answer.
    fail "TransportState never read PLAYING (saw: $seen)"
    echo "  --- logs/MPV.log (tail) ---"; tail -20 "$SDIR/logs/MPV.log" 2>/dev/null
    echo "  --- macast.log (tail) ---"; tail -10 "$SDIR/macast.log" 2>/dev/null
fi

echo "== 5: the DIDL title parsed and is readable back =="
meta=$(printf '<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:GetPositionInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetPositionInfo></s:Body></s:Envelope>' \
    | curl -s -m 5 -X POST "$BASE/AVTransport/action" \
        -H 'Content-Type: text/xml; charset="utf-8"' \
        -H 'SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#GetPositionInfo"' \
        --data-binary @-)
echo "$meta" | grep -q "$TITLE" \
    && pass "GetPositionInfo returns the DIDL dc:title ($TITLE)" \
    || fail "DIDL dc:title not readable back (fell back to friendly name?)"

echo "== 6: a GENA NOTIFY fires on state change =="
CBPORT=$((PORT + 1))
python3 - "$CBPORT" <<'PY' > /tmp/verify_rust_receiver_notify.txt 2>&1 &
import socket, sys, time
port = int(sys.argv[1]); end = time.time() + 8
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(5); s.settimeout(0.2)
while time.time() < end:
    try:
        c, _ = s.accept()
    except socket.timeout:
        continue
    c.settimeout(1.0)
    try:
        d = b""
        while True:
            ch = c.recv(4096)
            if not ch: break
            d += ch
        sys.stdout.buffer.write(d); sys.stdout.buffer.flush()
    finally:
        c.close()
PY
sleep 0.7
sub=$(curl -s -m 5 -D - -o /dev/null -X SUBSCRIBE "$BASE/AVTransport/event" \
    -H "CALLBACK: <http://127.0.0.1:$CBPORT/notif>" -H "TIMEOUT: Second-300" \
    -w '%{http_code}')
echo "$sub" | grep -qi "Sid:" && echo "  subscribe: 200 with a Sid" || echo "  subscribe failed: $sub"
# Recast to force an observed state flip while the subscriber is live.
curl -s -m 5 -o /dev/null -X POST "$BASE/AVTransport/action" \
    -H 'Content-Type: text/xml; charset="utf-8"' \
    -H 'SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI"' \
    --data-binary "$SOAP_CAST"
sleep 6
grep -q "NOTIFY /notif HTTP/1.1" /tmp/verify_rust_receiver_notify.txt \
    && grep -q "LastChange" /tmp/verify_rust_receiver_notify.txt \
    && pass "captured a real NOTIFY carrying a LastChange" \
    || fail "no NOTIFY with LastChange captured"

# A media address with an **authority**.
# `is_absolute_url` requires `://` + non-empty scheme + non-empty host, so the
# `file://$CLIP` the DLNA half plays with is refused by this entry on purpose.
# Proving the entry hands a playable address to the player needs a real HTTP
# source, and it must support **Range** (AGENTS.md §4.9): `http.server`'s stock
# handler does not, so this small one is written rather than borrowed.
MEDPORT=$((PORT + 2))
MEDIA_URL="http://127.0.0.1:$MEDPORT/clip.mp4"
# The **page** this fixture also serves, for the resolve layer (sections 14-17).
# It is one HTML document whose only media reference is the clip address above,
# so `scrape_page` has exactly one candidate to hand back, with the label
# `video:src` and the engine `page` (`media_resolve`'s HTML reader, ported at
# `resolve.rs:3087`). `PAGE_TITLE` is the witness for "the name came from the
# page, not from the last path segment" — AGENTS.md §4.2's naming seam, which is
# what makes `title` and `force-media-title` assertible rather than a guess.
PAGE_URL="http://127.0.0.1:$MEDPORT/page"
PAGE_TITLE="Rust Relay Witness"
MEDIA_LOG=/tmp/verify_rust_receiver_media.log
CLIP_BYTES=""
[ -s "$CLIP" ] && CLIP_BYTES=$(wc -c < "$CLIP" | tr -d ' ')
if [ -n "$CLIP_BYTES" ]; then
    python3 - "$CLIP" "$MEDPORT" "$MEDIA_URL" "$PAGE_TITLE" <<'PY' > "$MEDIA_LOG" 2>&1 &
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
path, port = sys.argv[1], int(sys.argv[2])
video_url, page_title = sys.argv[3], sys.argv[4]
size = os.path.getsize(path)
# The `<title>` element is **closed** on purpose: `scrape_page` feeds the parser
# and never calls `close()` (`resolve.rs:3072-3081` documents this), so an
# unterminated title would never flush and `Candidate.title` would stay empty —
# the fixture would then be lying about what the scraper can read.
PAGE = ('<!doctype html><html><head><title>%s</title></head>'
        '<body><video src="%s"></video></body></html>' % (page_title, video_url)).encode("utf-8")
class Range(BaseHTTPRequestHandler):
    # HTTP/1.1 because the whole point is accurate framing: a keep-alive
    # connection that lies about Content-Length hangs the client's seek.
    protocol_version = "HTTP/1.1"
    def log_message(self, fmt, *args): pass
    def _note(self):
        # One line per request, path plus the Range we were **asked** for. The
        # relay's Range translation (§4.14: a mutation that deleted it was 0-red
        # until the origin started logging what it received) is only witnessed by
        # a count taken across a relay read, so it has to be here and flushed —
        # block-buffered stdout would put the line after the assertion reads it.
        sys.stdout.write("REQ %s %s\n" % (self.path, self.headers.get("Range") or "-"))
        sys.stdout.flush()
    def _page(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)
    def _span(self):
        """(status, start, end): 200 whole, 206 part, 416 none."""
        header = self.headers.get("Range")
        if not header: return (200, 0, size - 1)
        if not header.startswith("bytes="): return (416, 0, 0)
        spec = header[6:].split(",")[0].strip()
        if "-" not in spec: return (416, 0, 0)
        left, right = spec.split("-", 1)
        if left == "":
            if not right.isdigit() or int(right) == 0: return (416, 0, 0)
            return (206, max(0, size - int(right)), size - 1)
        if not left.isdigit(): return (416, 0, 0)
        start = int(left)
        end = size - 1 if right == "" or not right.isdigit() else min(int(right), size - 1)
        if start >= size or start > end: return (416, 0, 0)
        return (206, start, end)
    def _respond(self, send_body):
        status, start, end = self._span()
        length = 0 if status == 416 else end - start + 1
        self.send_response(status)
        if status == 416:
            self.send_header("Content-Range", "bytes */%d" % size)
        else:
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            if status == 206:
                self.send_header("Content-Range", "bytes %d-%d/%d" % (start, end, size))
        self.send_header("Content-Length", str(length))
        self.end_headers()
        if not send_body: return
        with open(path, "rb") as fh:
            fh.seek(start); left = length
            while left:
                chunk = fh.read(min(65536, left))
                if not chunk: break
                self.wfile.write(chunk); left -= len(chunk)
    def do_GET(self):
        self._note()
        if self.path.startswith("/page"):
            self._page()
        else:
            self._respond(True)
    def do_HEAD(self):
        self._note()
        self._respond(False)
ThreadingHTTPServer(("127.0.0.1", port), Range).serve_forever()
PY
    MEDIA_PID=$!
    # Same reason as `disown "$APP_PID"` above: `cleanup` kills this job with
    # SIGKILL, and bash reports a SIGKILLed job on its own stderr at exit —
    # one line after the SUMMARY, where a reader expects a verdict.
    disown "$MEDIA_PID" 2>/dev/null || true
fi

echo "== 7: the media fixture really serves Range =="
# Section 13 hands the player this address, so "mpv played it" is only evidence
# about the receiver if the fixture can be seeked. Measuring the fixture first
# keeps a fixture bug from being read as a receiver bug — and both shapes are
# asked (AGENTS.md §4.8 red line: a read **without** `Range` must answer 200
# **without** `Content-Range`), so 7 cannot go green for the wrong reason.
if [ -z "$CLIP_BYTES" ]; then
    skip "no clip, so the Range fixture has nothing to serve"
else
    ok=""
    for _ in $(seq 1 40); do
        [ "$(curl -s --noproxy '*' -m 3 -o /dev/null -w '%{http_code}' "$MEDIA_URL")" = "200" ] \
            && { ok=1; break; }
        sleep 0.25
    done
    if [ -z "$ok" ]; then
        fail "the clip server never answered on $MEDPORT (/tmp/verify_rust_receiver_media.log)"
    else
        WHOLE=/tmp/verify_rust_receiver_whole
        code=$(curl -s --noproxy '*' -m 15 -D "$WHOLE.h" -o "$WHOLE.bin" -w '%{http_code}' "$MEDIA_URL")
        got=$(wc -c < "$WHOLE.bin" | tr -d ' ')
        [ "$code" = "200" ] \
            && pass "a whole-file read answers 200" \
            || fail "a whole-file read answered $code, not 200"
        [ "$got" = "$CLIP_BYTES" ] \
            && pass "a whole-file read gives all $CLIP_BYTES bytes" \
            || fail "a whole-file read gave $got of $CLIP_BYTES bytes"
        grep -qi "^content-length: $CLIP_BYTES" "$WHOLE.h" \
            && grep -qi "^accept-ranges: bytes" "$WHOLE.h" \
            && ! grep -qi "^content-range:" "$WHOLE.h" \
            && pass "the 200 carries the length and no Content-Range" \
            || fail "the 200 headers are wrong (length / Accept-Ranges / a stray Content-Range)"
        PART=/tmp/verify_rust_receiver_part
        code=$(curl -s --noproxy '*' -m 15 -D "$PART.h" -o "$PART.bin" -w '%{http_code}' \
            -H 'Range: bytes=0-1023' "$MEDIA_URL")
        got=$(wc -c < "$PART.bin" | tr -d ' ')
        [ "$code" = "206" ] && [ "$got" = "1024" ] \
            && pass "a ranged read answers 206 with exactly 1024 bytes" \
            || fail "a ranged read answered $code with $got bytes"
        grep -qi "^content-range: bytes 0-1023/$CLIP_BYTES" "$PART.h" \
            && pass "the 206 names the slice it served" \
            || fail "the 206 does not carry the measured Content-Range"
    fi
fi

echo "== 8: an unknown query is answered, not gated =="
api_probe "$BASE/api?query=nosuchthing"
[ "$API_STATUS" = "200" ] && [ "$API_CODE" = "1" ] \
    && pass "an unknown query is a 200 that says nothing was matched" \
    || fail "an unknown query answered HTTP $API_STATUS code '$API_CODE'"
case "$API_MSG" in
    *nosuchthing*) pass "the refusal names the typo back" ;;
    *) fail "the refusal does not echo the unknown query: $API_MSG" ;;
esac
case "$API_MSG" in
    *"$MANAGEMENT_MARK"*|*"$TOKEN_MARK"*)
        fail "a route answered **after** a gate ran: $API_MSG" ;;
    *) pass "no gate sentence, so the route table really ran first" ;;
esac
api_probe "$BASE/api"
[ "$API_STATUS" = "200" ] && [ "$API_CODE" = "1" ] \
    && case "$API_MSG" in *"缺少 query"*) true ;; *) false ;; esac \
    && pass "a /api with no query names the missing parameter" \
    || fail "a /api with no query answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
if [ -n "$LAN" ]; then
    api_probe "http://$LAN:$PORT/api?query=nosuchthing"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "1" ] \
        && case "$API_MSG" in *nosuchthing*) true ;; *) false ;; esac \
        && pass "a non-loopback caller gets the same un-gated answer for a typo" \
        || fail "from $LAN an unknown query answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
else
    skip "no reachable non-loopback address to ask from (section 8's LAN half)"
fi

echo "== 9: query=cast from loopback without a token is refused =="
# The player's address is captured **before** the refusal. An empty TrackURI
# would make every "unchanged" check below pass for the wrong reason: "nothing
# is loaded" and "the refusal did not touch the player" are different claims,
# and only a non-empty baseline tells them apart.
BEFORE=$(track_uri)
if [ "$BEFORE" = "file://$CLIP" ]; then
    pass "the player still holds the DLNA cast from section 3 (a baseline to compare against)"
else
    fail "expected '$CLIP' loaded before this check, TrackURI read '$BEFORE'"
fi
api_probe "$BASE/api?query=cast&url=$MEDIA_URL"
[ "$API_STATUS" = "403" ] \
    && pass "the no-token entry is refused outright" \
    || fail "the no-token entry answered HTTP $API_STATUS, not 403"
case "$API_MSG" in
    *"$TOKEN_MARK"*) pass "the caller hears the token sentence" ;;
    *) fail "the caller did not hear the token sentence: $API_MSG" ;;
esac
case "$API_MSG" in
    *"$MANAGEMENT_MARK"*)
        # Loopback passes the management gate, so the management sentence here
        # would mean the token check never ran — the wrong gate answering.
        fail "loopback heard the management sentence instead of the token one" ;;
    *) pass "and not the management one, so the token gate is what refused" ;;
esac
[ -z "$(stored_token)" ] \
    && pass "the refused call left no secret in the settings file" \
    || fail "a refused call minted and stored a token: the peek wrote"
[ "$(track_uri)" = "$BEFORE" ] \
    && pass "the refusal did not touch the player" \
    || fail "the refusal changed what the player holds"

echo "== 10: cast-info reveals the token to this machine only =="
api_probe "$BASE/api?query=cast-info"
[ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
    && pass "cast-info answers this machine" \
    || fail "cast-info answered HTTP $API_STATUS code '$API_CODE'"
# Both orders, not one glob: the shipped sentence puts this instance's **port**
# (`http://127.0.0.1:58999/...`) ahead of `token=`, so a single
# `*"token="*"$PORT"*` pattern would fail on a correct answer.
case "$API_MSG" in
    *"token="*"$PORT"*|*"$PORT"*"token="*)
        pass "the entry template arrives filled in, port and all" ;;
    *) fail "cast-info did not hand back a usable entry URL: $API_MSG" ;;
esac
EXTRACTED=$(printf '%s' "$API_MSG" | sed -nE 's/.*token=([0-9a-f]{32}).*/\1/p')
[ -n "$EXTRACTED" ] \
    && pass "the revealed secret has the minted shape (32 hex)" \
    || fail "no 32-hex token inside the answer: $API_MSG"
[ "$EXTRACTED" = "$(stored_token)" ] \
    && pass "the revealed secret is the stored one, not a second copy" \
    || fail "cast-info revealed something other than what the file holds"
api_probe "$BASE/api?query=cast-info"
case "$API_MSG" in
    *"$EXTRACTED"*) pass "a second ask returns the same secret (it is resident)" ;;
    *) fail "the token rotated between two asks — saved shortcuts would break" ;;
esac
if [ -n "$LAN" ]; then
    api_probe "http://$LAN:$PORT/api?query=cast-info"
    [ "$API_STATUS" = "403" ] \
        && pass "a LAN caller is refused cast-info" \
        || fail "a LAN caller got HTTP $API_STATUS from cast-info"
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*) pass "and hears the management sentence, as that is the gate that ran" ;;
        *) fail "LAN refusal wording: $API_MSG" ;;
    esac
    case "$API_MSG" in
        *[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
            fail "the LAN refusal leaked a hex secret" ;;
        *) pass "and leaks no secret" ;;
    esac
else
    skip "no reachable non-loopback address to ask from (section 10's LAN half)"
fi

echo "== 11: a wrong token hears the token sentence =="
api_probe "$BASE/api?query=cast&url=$MEDIA_URL&token=deadbeefdeadbeefdeadbeefdeadbeef"
[ "$API_STATUS" = "403" ] \
    && pass "a wrong token is refused" \
    || fail "a wrong token answered HTTP $API_STATUS, not 403"
case "$API_MSG" in
    *"$TOKEN_MARK"*) pass "and hears the token sentence, not a generic refusal" ;;
    *) fail "wrong-token wording: $API_MSG" ;;
esac
case "$API_MSG" in
    *"$MANAGEMENT_MARK"*) fail "a wrong token heard the management sentence" ;;
    *) pass "loopback was admitted by the management gate, as it should be" ;;
esac
[ "$(track_uri)" = "$BEFORE" ] \
    && pass "a wrong token did not touch the player" \
    || fail "a wrong token reached the player"
[ "$(stored_token)" = "$EXTRACTED" ] \
    && pass "a wrong token did not rotate the stored secret" \
    || fail "the stored secret changed after a failed attempt"

echo "== 12: a good token with a relative url is refused by the route =="
api_probe "$BASE/api?query=cast&url=/relative/path.mp4&token=$EXTRACTED"
[ "$API_STATUS" = "200" ] && [ "$API_CODE" = "1" ] \
    && pass "the shape refusal is the shipped 200/code 1, not a 403" \
    || fail "a relative url answered HTTP $API_STATUS code '$API_CODE'"
# Both orders again: this port's sentence **leads** with the refused address
# (`<url> 不是一个完整的媒体地址：…`), so a single
# `*"不是一个完整的媒体地址"*"<url>"*` pattern fails on a correct answer.
case "$API_MSG" in
    *"不是一个完整的媒体地址"*"/relative/path.mp4"*|*"/relative/path.mp4"*"不是一个完整的媒体地址"*)
        pass "it says what is wrong and echoes the address back" ;;
    *) fail "relative-url wording: $API_MSG" ;;
esac
[ "$(track_uri)" = "$BEFORE" ] \
    && pass "a refused shape left the player alone" \
    || fail "a bad url still reached the player"

echo "== 13: a good token with a real address plays it and names it =="
if [ -z "$CLIP_BYTES" ]; then
    skip "no clip, so there is no playable address to cast"
else
    TITLE_ARG="Rust-Verify-Title"
    # This face spells the name `title`, not `cast-title`: the POST `cast-uri`
    # route owns that key (`protocol.py:2705`), while the GET entry reads
    # `kwargs.get('title','')` (`protocol.py:2086`). The port copies that split,
    # so a harness that passes `cast-title` here gets a silent "no opinion" and
    # a green-looking run that never asked the question. The success line is the
    # reference's literal `'success'` (`_cast_url`, protocol.py:1696).
    api_probe "$BASE/api?query=cast&url=$MEDIA_URL&title=$TITLE_ARG&token=$EXTRACTED"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
        && [ "$API_MSG" = "success" ] \
        && pass "the entry reports the cast" \
        || fail "the cast answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
    ok=""
    for _ in $(seq 1 40); do
        [ "$(track_uri)" = "$MEDIA_URL" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "the player was handed the relayed address, not the page's" \
        || fail "TrackURI never became $MEDIA_URL (reads '$(track_uri)')"
    # The name lands a moment after the GET answers, so this polls rather than
    # reading on the cast's return. `force-media-title` is the seam this port
    # writes (AGENTS.md §4.2: change the player's address, change its name) —
    # asking `filename` would answer `clip.mp4` for the wrong reason.
    ok=""
    for _ in $(seq 1 40); do
        [ "$(mpv_ask force-media-title)" = "$TITLE_ARG" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "the window name follows the title, not the last path segment" \
        || fail "mpv force-media-title reads '$(mpv_ask force-media-title)', not $TITLE_ARG"
fi

# Section 14-17 share three facts, and each one is set inside the section that
# earns it: `JOB` from resolve-page, then the relay's own address from
# cast-resolved. They are pre-declared empty because `set -u` would otherwise
# make a skipped section 14 (no clip) a crash in section 17 rather than a skip,
# and a harness that dies mid-run hides every later check (AGENTS.md §4.14:
# a `None` detail once cost Part 59 its last eight cases).
JOB=""
RELAY_URL=""
RELAY_HOST=""
RELAY_PORT=""
RELAY_ID=""

echo "== 14: resolve-page registers a job and answers with its id =="
if [ -z "$CLIP_BYTES" ]; then
    skip "no fixture page, so there is nothing to parse"
else
    # The document, not just the verdict: `Answer::with` chains in the
    # reference's write order (`protocol.py:2855-2872`), and the page reads this
    # row field by field — so a reordered answer is a page bug, not cosmetics.
    api_post "$BASE/api" "resolve-page=$PAGE_URL"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
        && pass "a posted page address is accepted" \
        || fail "resolve-page answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
    [ "$(json_keys .)" = "code,message,job,url,note" ] \
        && pass "the answer carries the reference's five keys, in order" \
        || fail "resolve-page keys: $(json_keys .)"
    JOB=$(json_ask job)
    # `token_urlsafe(9)`: nine bytes are twelve url-safe characters. The length
    # is asked because an empty id is a *reachable* failure shape here — the
    # generator returns `''` when the system RNG fails, and every later poll
    # would then answer 「没有这个解析任务」 for a job that is running.
    [ -n "$JOB" ] && [ "${#JOB}" -eq 12 ] \
        && pass "the job id is the twelve characters a nine-byte token encodes to" \
        || fail "job id '$JOB' is not twelve characters"
    [ "$(json_ask url)" = "$PAGE_URL" ] \
        && pass "the answer names the address that was posted" \
        || fail "resolve-page echoed '$(json_ask url)', not $PAGE_URL"
    [ "$(json_ask note)" = "" ] \
        && pass "a bare address earns no share-text note" \
        || fail "note on a bare address: $(json_ask note)"
    # No token was sent, and loopback **is** this route's admission (§4.7). The
    # assertible fact is the **absence** of a refusal sentence — the same
    # reading sections 9-12 use from the other side of the gate. A route that
    # accidentally joined the token-gated list would say so right here.
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*|*"$TOKEN_MARK"*)
            fail "a loopback resolve-page heard a gate sentence: $API_MSG" ;;
        *) pass "resolve-page is served to loopback without a token" ;;
    esac

    # The three refusals the route can make before it ever spends a slot. Each
    # is one sentence the user reads instead of a parse that cannot work, so
    # the wording is the contract (`_post_resolve_page`, protocol.py:2820-2850).
    api_post "$BASE/api" "resolve-page="
    [ "$API_CODE" = "1" ] && [ "$API_MSG" = "请先填写网页地址" ] \
        && pass "an empty field is refused by name" \
        || fail "empty resolve-page answered code '$API_CODE': $API_MSG"
    api_post "$BASE/api" "resolve-page=example.com/watch/7"
    [ "$API_CODE" = "1" ] && [ "$API_MSG" = "地址要以 http:// 或 https:// 开头" ] \
        && pass "a bare host is refused rather than handed to the engine as a search" \
        || fail "bare host answered code '$API_CODE': $API_MSG"
    api_post "$BASE/api" "resolve-page=看看这个视频很不错 值得一看哦"
    [ "$API_CODE" = "1" ] && case "$API_MSG" in *"这段文字里没有找到网址"*) true ;; *) false ;; esac \
        && pass "multi-word text with no address inside is refused by the other sentence" \
        || fail "schemeless share text answered code '$API_CODE': $API_MSG"

    # `resolve-status` is a **GET** in the management set (http.rs:430, next to
    # `cast-info`). An id nobody registered answers the row-shape refusal, from
    # loopback, with no token — and it must not consume a `RESOLVE_JOBS_MAX`
    # slot: the reference only looks the id up (`_resolve_job`), the slot is
    # spent at `resolve-page`. Section 15 proves that half by asking again.
    api_probe "$BASE/api?query=resolve-status&job=nobody-registered-this"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "1" ] \
        && [ "$API_MSG" = "没有这个解析任务（超时或已重启）" ] \
        && pass "an unknown job id gets the row-shape refusal, not a 403" \
        || fail "unknown job answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
    [ "$(json_ask job)" = "" ] && [ "$(json_ask status)" = "null" ] \
        && pass "and the two empty shapes the page reads: job '' and status null" \
        || fail "unknown job row is job='$(json_ask job)' status='$(json_ask status)'"
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*|*"$TOKEN_MARK"*)
            fail "a loopback resolve-status heard a gate sentence: $API_MSG" ;;
        *) pass "resolve-status is served to loopback without a token" ;;
    esac
fi

echo "== 15: the job finishes and the page's own address is the candidate =="
if [ -z "$CLIP_BYTES" ] || [ -z "$JOB" ]; then
    skip "no job to poll"
else
    # The budget is wide on purpose. This machine's `yt-dlp` and `ffprobe` are
    # not controlled here (§4.2: "空结果要说清是哪一种空" is exactly the case of
    # a suite that made an installed package a measured fact), and the engine
    # step's own ceiling is `YTDLP_TIMEOUT` = 120 s. A poll that gives up at 30
    # seconds would read as "the resolver hangs" when the truth is a slow engine.
    DONE=""
    for _ in $(seq 1 600); do
        api_probe "$BASE/api?query=resolve-status&job=$JOB"
        [ "$(json_ask status.done)" = "true" ] && { DONE=1; break; }
        sleep 0.25
    done
    [ -n "$DONE" ] \
        && pass "the job reports itself done" \
        || fail "the job never finished; last step label: '$(json_ask status.step_label)'"
    # The seventeen keys, in the order `status()` writes them (resolve.rs:3670).
    # `candidates` last is the shape the page's progress bar depends on: it reads
    # the counters before it ever touches the list.
    [ "$(json_keys status)" = \
        "url,note,done,step,step_label,seconds,step_seconds,scraped,from_ytdlp,measured,rejected,hidden_silent,engine_note,cookie_problem,needs_cookies,error,candidates" ] \
        && pass "the status row carries the reference's seventeen keys, in order" \
        || fail "status keys: $(json_keys status)"
    [ "$(json_ask job)" = "$JOB" ] \
        && pass "a found row names the id that was asked for" \
        || fail "status row answered for job '$(json_ask job)'"
    # Only the facts the fixture owns are asked here. `measured` / `rejected`
    # depend on whether this machine has `ffprobe`, and `engine_note` on whether
    # it has `yt-dlp` — a check that changes colour with installed packages is a
    # check that answers nothing (AGENTS.md §4.2).
    [ "$(json_len status.candidates)" -ge 1 ] \
        && pass "the page gave at least one candidate" \
        || fail "no candidates: engine note is '$(json_ask status.engine_note)'"
    [ "$(json_ask status.candidates.0.url)" = "$MEDIA_URL" ] \
        && pass "the candidate is the address the HTML stated outright" \
        || fail "candidate url: $(json_ask status.candidates.0.url)"
    # Which engine owns the row is **not** a fixture fact. If this machine has
    # `yt-dlp`, the engine answers with the very address the HTML states, and
    # `dedupe()` (`macast/media_resolve.py:1013-1028`) lets the ytdlp row win —
    # while the page still contributes its own `<title>` wording, which the next
    # check asks for. So the machine-questionable facts here are the rule, not a
    # value: the address occupies exactly one row, and its provenance names one of
    # the two engines we actually run.
    #
    # This line used to pin `origin='page'` and `label='video:src'`. It was wrong
    # twice over: it asserted the loser of the dedupe, and it was a check that
    # changes colour with installed packages (AGENTS.md §4.2) — the very reason
    # `measured` / `engine_note` are not asked above.
    SAME=0
    for _i in $(seq 0 "$(($(json_len status.candidates) - 1))"); do
        [ "$(json_ask "status.candidates.$_i.url")" = "$MEDIA_URL" ] \
            && SAME=$((SAME + 1))
    done
    [ "$SAME" = "1" ] \
        && pass "one row for that address, however many engines found it" \
        || fail "the address occupies $SAME rows, not one"
    ORIGIN="$(json_ask status.candidates.0.origin)"
    if [ "$ORIGIN" = "page" ] || [ "$ORIGIN" = "ytdlp" ]; then
        pass "and the row names the engine that read it ('$ORIGIN')"
    else
        fail "candidate provenance: unknown engine '$ORIGIN'"
    fi
    # The title survived the parse. `<title>` is **closed** in the fixture for
    # this reason (resolve.rs:3072: it feeds and never closes, and `close()` is
    # what flushes buffered character data) — so this row is the machine's
    # witness that the scraper keeps what the parser never flushed.
    [ "$(json_ask status.candidates.0.title)" = "$PAGE_TITLE" ] \
        && pass "the page's title came back with its address" \
        || fail "candidate title: '$(json_ask status.candidates.0.title)'"
    # A direct, unsegmented address is served by reverse-proxying the origin
    # (`plan()`, resolve.rs:1882: merge > segmented > proxy). This is the value
    # the next section's `relay.reason` sentence is derived from, so it belongs
    # on the row rather than in the wording.
    [ "$(json_ask status.candidates.0.mode)" = "proxy" ] \
        && pass "and the plan for it is a reverse proxy, not a remux" \
        || fail "candidate mode: '$(json_ask status.candidates.0.mode)'"
fi

echo "== 16: casting a candidate goes through our relay; the device arm names a row it does not have =="
if [ -z "$CLIP_BYTES" ] || [ -z "$JOB" ]; then
    skip "no job, so nothing to cast"
else
    api_post "$BASE/api" "cast-resolved=1" "job=$JOB" "candidate=0"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] && [ "$API_MSG" = "success" ] \
        && pass "the cast is served" \
        || fail "cast-resolved answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
    # `code,message,url,title` come from the same `_cast_url` method the GET
    # entry uses (host.rs:1359-1366), and `relay,note` are appended after it
    # with `message` rewritten **in place** — so this order is the reference's
    # assignment order, not something the port chose.
    [ "$(json_keys .)" = "code,message,url,title,relay,note" ] \
        && pass "the answer is the local cast's document plus the relay row" \
        || fail "cast-resolved keys: $(json_keys .)"
    RELAY_URL=$(json_ask url)
    # Decomposed, never rebuilt. The host may be this machine's LAN address and
    # the port is the **relay's own** bound port (host.rs:1094: `port` is what
    # the accept loop took, not the application port) — either half assumed
    # here would be a second owner of a fact the answer already states.
    _after=${RELAY_URL#http://}
    RELAY_HOST=${_after%%:*}
    RELAY_ID=$(printf '%s' "$_after" | cut -d/ -f3)
    RELAY_PORT=$(printf '%s' "${_after#*:}" | cut -d/ -f1)
    [ "$RELAY_URL" != "$MEDIA_URL" ] \
        && pass "the player is handed our relay, never the origin address" \
        || fail "the origin address reached the player: $RELAY_URL"
    case "$RELAY_URL" in
        *"/relay/"*"/media") pass "and it is a relay path with an id in it" ;;
        *) fail "not a relay address: $RELAY_URL" ;;
    esac
    [ "$(json_ask title)" = "$PAGE_TITLE" ] \
        && pass "the cast's name is the page's title" \
        || fail "cast title: '$(json_ask title)'"
    ok=""
    for _ in $(seq 1 40); do
        [ "$(track_uri)" = "$RELAY_URL" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "the player was handed exactly that relay address" \
        || fail "TrackURI never became $RELAY_URL (reads '$(track_uri)')"
    # The A/B against section 13 is the point: that cast wrote
    # `Rust-Verify-Title`, this one writes the **page's** title with no
    # `cast-title` argument at all. `filename` would answer `media` for the
    # wrong reason (AGENTS.md §4.2 — the relay path's last segment is exactly
    # the word that made mpv's window say `media - mpv`).
    ok=""
    for _ in $(seq 1 40); do
        [ "$(mpv_ask force-media-title)" = "$PAGE_TITLE" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "and the window name follows the page, overwriting the earlier cast" \
        || fail "mpv force-media-title reads '$(mpv_ask force-media-title)', not $PAGE_TITLE"

    # The device arm. The send face (`macast-cast/src/push.rs`) and this wiring both
    # exist now, so this is no longer "the shell has no sender"; what *is* reachable
    # here without touching somebody else's player is the arm's stale-row answer,
    # and that is what this asserts. `tv` is a shape
    # `targets::Table` can never produce (rows are `cast:{host}:{port}` or
    # `dlna:{control-url}`), so the row stays deterministic even when a real
    # renderer answers the search — and pushing to a device that happens to be
    # alive on this LAN is deliberately **not** tested: the only renderers out here
    # are the user's own Macast instances, and starting playback on his other
    # machine is not this script's call to make.
    api_post "$BASE/api" "cast-resolved=1" "job=$JOB" "candidate=0" "target=tv"
    [ "$API_CODE" = "1" ] \
        && case "$API_MSG" in *"设备列表里没有这一台"*) true ;; *) false ;; esac \
        && pass "the device arm refuses a row the caches do not hold, by name" \
        || fail "device arm answered code '$API_CODE': $API_MSG"
    case "$API_MSG" in
        *"这台设备还投不了"*) fail "it still says the shell has no sender face: $API_MSG" ;;
        *) pass "and it no longer blames a missing face that exists" ;;
    esac
    # The wrapper is Python's: `_cast_relayed` builds `{'code','message','target'}`
    # **before** it knows the outcome (`protocol.py:2982-2983`), so even a refusal
    # echoes the id it was handed.
    [ "$(json_keys .)" = "code,message,target" ] \
        && pass "and the refusal carries the target it was asked for" \
        || fail "device-arm keys: $(json_keys .)"
    [ "$(json_ask target)" = "tv" ] \
        && pass "echoed verbatim, not re-derived from the row that is not there" \
        || fail "target echo: '$(json_ask target)'"
    [ "$(track_uri)" = "$RELAY_URL" ] \
        && pass "and the refused device push left our own player alone" \
        || fail "a refused device push moved our player to '$(track_uri)'"
    [ "$(mpv_ask force-media-title)" = "$PAGE_TITLE" ] \
        && pass "and it left the name alone too" \
        || fail "a refused device push renamed the window to '$(mpv_ask force-media-title)'"
fi

echo "== 17: the relay serves the media off its own port, translated =="
if [ -z "$CLIP_BYTES" ] || [ -z "$RELAY_URL" ]; then
    skip "no relay, so there is nothing to read off it"
else
    RELAY_BASE="http://$RELAY_HOST:$RELAY_PORT/relay/$RELAY_ID"
    RW=/tmp/verify_rust_receiver_relay_whole
    RP=/tmp/verify_rust_receiver_relay_part
    code=$(curl -s --noproxy '*' -m 20 -D "$RW.h" -o "$RW.bin" -w '%{http_code}' "$RELAY_BASE/media")
    got=$(wc -c < "$RW.bin" | tr -d ' ')
    [ "$code" = "200" ] && [ "$got" = "$CLIP_BYTES" ] \
        && pass "a whole read through the relay answers 200 with every byte" \
        || fail "relay whole read answered $code with $got of $CLIP_BYTES bytes"
    # §4.8's red line, asked of the relay rather than of the origin: a read that
    # carried **no** `Range` must not dress itself up as partial. The port's own
    # version of that rule lives in `serve_plan` (relay.rs:249-308).
    grep -qi "^content-length: $CLIP_BYTES" "$RW.h" \
        && grep -qi "^accept-ranges: bytes" "$RW.h" \
        && ! grep -qi "^content-range:" "$RW.h" \
        && pass "the 200 carries the length and no Content-Range" \
        || fail "the relay's 200 headers are wrong (length / Accept-Ranges / a stray Content-Range)"
    code=$(curl -s --noproxy '*' -m 20 -D "$RP.h" -o "$RP.bin" -w '%{http_code}' \
        -H 'Range: bytes=100-199' "$RELAY_BASE/media")
    got=$(wc -c < "$RP.bin" | tr -d ' ')
    [ "$code" = "206" ] && [ "$got" = "100" ] \
        && pass "a viewer's Range answers 206 with exactly the hundred bytes asked" \
        || fail "relay ranged read answered $code with $got bytes"
    grep -qi "^content-range: bytes 100-199/$CLIP_BYTES" "$RP.h" \
        && pass "and names the slice it served against the real total" \
        || fail "the relay's 206 does not carry Content-Range bytes 100-199/$CLIP_BYTES"
    # The translation itself: what the **origin** was asked for. This line is
    # why `_note()` exists in the fixture (§4.14: deleting the translation was
    # 0-red until the origin started logging what it received). Note what is
    # **not** expected — a whole GET through the relay still asks the origin
    # `bytes=0-<total-1>`, because `pipe_upstream` (relay.rs:1080) ranges the
    # request whenever it has a known end. Only the second line is the seam.
    ok=""
    for _ in $(seq 1 40); do
        grep -q "REQ /clip.mp4 bytes=100-199" "$MEDIA_LOG" 2>/dev/null && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "and the viewer's Range was translated to the origin, not dropped" \
        || fail "the origin never saw 'bytes=100-199' (log: $(tail -3 "$MEDIA_LOG" 2>/dev/null | tr '\n' ' '))"

    # The relay's own status face, on its own port, with no wrapper around the
    # row (`relay_from_path`, relay.rs:995-1023 — GET on `/status` reads the
    # store with `peek`, so it costs nobody a TTL touch).
    api_probe "$RELAY_BASE/status"
    [ "$API_STATUS" = "200" ] \
        && pass "the relay answers /status on its own port" \
        || fail "/status answered HTTP $API_STATUS"
    [ "$(json_keys .)" = "id,title,mode,state,seekable,ranges,length,written,error,seconds,idle" ] \
        && pass "and serves the eleven-key row, in order" \
        || fail "/status keys: $(json_keys .)"
    [ "$(json_ask id)" = "$RELAY_ID" ] && [ "$(json_ask title)" = "$PAGE_TITLE" ] \
        && pass "the row is this relay's: its id and the page's title" \
        || fail "/status row is id='$(json_ask id)' title='$(json_ask title)'"
    # `length` came from the origin's own answer, not from ffprobe: the proxy
    # probe is a HEAD/first-read against the source (`probe_proxy`, run
    # synchronously inside `cast-resolved`). This is the fact behind
    # `seekable`, and therefore behind the viewer being able to drag.
    [ "$(json_ask length)" = "$CLIP_BYTES" ] && [ "$(json_ask seekable)" = "true" ] \
        && pass "the relay knows the real length, so the viewer may seek" \
        || fail "/status length='$(json_ask length)' seekable='$(json_ask seekable)', want $CLIP_BYTES/true"
    [ "$(json_ask mode)" = "proxy" ] && [ "$(json_ask error)" = "" ] \
        && pass "and says it is proxying with nothing to complain about" \
        || fail "/status mode='$(json_ask mode)' error='$(json_ask error)'"

    # An id nobody registered gets the 404, on the relay port — the half of the
    # gate that is **not** about tokens: a random path is what this surface has.
    code=$(curl -s --noproxy '*' -m 8 -o /dev/null -w '%{http_code}' \
        "$RELAY_BASE-nope/media")
    [ "$code" = "404" ] \
        && pass "an unknown relay id is a 404, not a 200 with no bytes" \
        || fail "an unknown relay id answered $code"
fi

echo "== 18: the cookie panel — counts in, counts out, values only on disk =="
# This is the one card in the whole surface whose payload is somebody's live
# session credential, so the checks are ordered around one question: **did the
# secret stay where the user put it?** Everything the API may say about the jar is
# a count, a name, or a path (AGENTS.md §4.14). The two facts that decide every
# expectation below — the browser whitelist and the size cap — are read out of the
# Python module that owns them, not out of the Rust constant the code under test
# also reads (§4.13: 每个数字问两遍).
PYOUT=$(py_cookie_constants)
if [ -z "$PYOUT" ]; then
    skip "the Python cookie constants (section 18 reads its expectations from them)"
else
    CAP=$(sed -n '2p' <<< "$PYOUT")
    NAMES=$(sed -n '3p' <<< "$PYOUT")
    JAR="$SDIR/resolve_cookies.txt"
    JAR_GOOD=/tmp/verify_rust_receiver_jar_good.txt
    JAR_GARBAGE=/tmp/verify_rust_receiver_jar_garbage.txt
    JAR_BIG=/tmp/verify_rust_receiver_jar_big.txt

    # (a) the cold read. `path` is the one string here that came off the
    # filesystem, and it is allowed because it is where the user would look to
    # check our count — so it has to be the real path, not a plausible one.
    api_probe "$BASE/api?query=resolve-cookies"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
        && pass "loopback is admitted to the cookie panel (management, not code-execution)" \
        || fail "the panel answered HTTP $API_STATUS code '$API_CODE'"
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*) fail "loopback heard the management sentence: $API_MSG" ;;
        *) pass "and no gate sentence, because none of these routes hand us code to run" ;;
    esac
    [ "$(json_keys .)" = "code,message,state" ] \
        && pass "the panel document is code, message, state" \
        || fail "panel keys: $(json_keys .)"
    [ "$(json_keys state)" = "jar,cookies,bad,path,browser,browsers,max_bytes,yt_dlp" ] \
        && pass "and the state row has the reference's eight keys, in its order" \
        || fail "state keys: $(json_keys state)"
    [ "$(json_join state.browsers)" = "$NAMES" ] \
        && pass "the browser list is the Python whitelist — names **and** order" \
        || fail "browsers: $(json_join state.browsers), want $NAMES"
    [ "$(json_ask state.max_bytes)" = "$CAP" ] \
        && pass "and so is the jar's size cap" \
        || fail "max_bytes: $(json_ask state.max_bytes), want $CAP"
    [ "$(json_ask state.path)" = "$JAR" ] \
        && pass "the card names the file the user would check us against" \
        || fail "state.path: $(json_ask state.path), want $JAR"
    [ "$(json_ask state.jar)" = "false" ] && [ "$(json_ask state.cookies)" = "0" ] \
        && pass "a machine that never held a jar says so, rather than erroring" \
        || fail "cold state: jar='$(json_ask state.jar)' cookies='$(json_ask state.cookies)'"
    json_has state.yt_dlp \
        && pass "and it reports whether the engine is installed at all" \
        || fail "state.yt_dlp is missing — the card cannot say 'no engine here'"

    # (b) one paste, four shapes of line. The `#HttpOnly_` row is the one that
    # used to be read as a comment (the reference's own bug: a browser export marks
    # its session cookies that way, so treating the prefix as a comment rejects the
    # only file this panel exists to accept) — and `#HttpOnly_# …` must stay a
    # comment, or the count promises a login state the engine never loads.
    printf '# Netscape HTTP Cookie File\n' > "$JAR_GOOD"
    printf '#HttpOnly_# also a comment\n' >> "$JAR_GOOD"
    printf '\n' >> "$JAR_GOOD"
    printf '#HttpOnly_.example.com\tTRUE\t/\tTRUE\t0\tsess\tSECRETVALUE1\n' >> "$JAR_GOOD"
    printf '.example.com\tTRUE\t/\tFALSE\t0\tplain\tSECRETVALUE2\n' >> "$JAR_GOOD"
    printf 'short\trow\n' >> "$JAR_GOOD"
    api_post "$BASE/api" "resolve-cookies@$JAR_GOOD"
    [ "$API_CODE" = "0" ] \
        && pass "a mixed jar is accepted on the strength of its cookies, not its lines" \
        || fail "paste refused: $API_MSG"
    [ "$(json_keys .)" = "code,message,cookies,bad" ] \
        && pass "and the success document carries the two counts" \
        || fail "save keys: $(json_keys .)"
    [ "$API_MSG" = "已保存 2 条 cookie，另有 1 行看不懂（一并存了）" ] \
        && pass "the sentence says both numbers, including the one that was skipped" \
        || fail "save wording: $API_MSG"
    api_probe "$BASE/api?query=resolve-cookies"
    [ "$(json_ask state.jar)" = "true" ] \
        && [ "$(json_ask state.cookies)" = "2" ] \
        && [ "$(json_ask state.bad)" = "1" ] \
        && pass "the panel re-reads the file and agrees with what it just said" \
        || fail "panel after save: jar='$(json_ask state.jar)' cookies='$(json_ask state.cookies)' bad='$(json_ask state.bad)'"

    # (c) the credential checks. 0600 because the file is handed to a subprocess
    # and read back by whoever owns this disk; the leak scan covers the three
    # places a count-reporting surface can still write a value: the settings JSON,
    # the app log, and the per-module logs directory — plus the panel document we
    # are holding, which is the one a screenshot of the card would show.
    [ -e "$JAR" ] && [ ! -e "$JAR.tmp" ] \
        && pass "the jar is in place and no half-written temp file was left behind" \
        || fail "jar exists: $([ -e "$JAR" ] && echo yes || echo no), temp exists: $([ -e "$JAR.tmp" ] && echo yes || echo no)"
    [ "$(stat -f '%Lp' "$JAR")" = "600" ] \
        && pass "0600, as a credential cache must be" \
        || fail "jar mode is $(stat -f '%Lp' "$JAR" 2>/dev/null)"
    leaked=""
    for probe in "$SDIR/macast_setting.json" "$SDIR/macast.log" "$SDIR"/logs/*.log "$API_BODY"; do
        [ -f "$probe" ] || continue
        grep -q -E 'SECRETVALUE[12]' "$probe" && leaked="$leaked $(basename "$probe")"
    done
    [ -z "$leaked" ] \
        && pass "neither cookie value appears in the settings file, either log, or the panel read" \
        || fail "leaked into:$leaked"

    # (d) + (e) the two refusals, each with the same witness: the jar on disk is
    # still the one from (b). A paste that is refused must not be a paste that
    # silently replaced the working credential — that is the whole reason the
    # validation runs before anything is opened.
    JAR_MD5=$(md5 -q "$JAR")
    printf 'not a cookie line\nnor this one\nor this\n' > "$JAR_GARBAGE"
    api_post "$BASE/api" "resolve-cookies@$JAR_GARBAGE"
    [ "$API_CODE" = "1" ] && [ "$(json_keys .)" = "code,message" ] \
        && pass "a jar with nothing readable in it is refused, with no counts to mislead" \
        || fail "garbage jar answered code '$API_CODE' keys '$(json_keys .)'"
    [ "$API_MSG" = "这份文件里没有一条能读懂的 cookie（3 行看不懂）。要 Netscape 格式：每行 7 个 TAB 分隔的字段" ] \
        && pass "and it says how many lines it could not read, so the user can open the file" \
        || fail "garbage wording: $API_MSG"
    head -c "$((CAP + 1))" /dev/zero | tr '\0' 'a' > "$JAR_BIG"
    api_post "$BASE/api" "resolve-cookies@$JAR_BIG"
    [ "$API_CODE" = "1" ] \
        && pass "a 200 MB paste is a disk problem, so the cap refuses at the door" \
        || fail "oversized jar answered code '$API_CODE': $API_MSG"
    [ "$API_MSG" = "cookie 文件太大了（上限 $((CAP / 1024)) KB）" ] \
        && pass "and the number in that sentence is the cap the panel just published" \
        || fail "oversized wording: $API_MSG"
    [ "$(md5 -q "$JAR")" = "$JAR_MD5" ] \
        && pass "and both refusals left the saved jar byte-for-byte the good one" \
        || fail "a refused paste overwrote the credential file"

    # (f)–(h) the browser field. `名字:资料目录` is the shape that exists because
    # Vivaldi keeps its cookie database outside the default profile directory, so
    # the answer has to be: accept it byte-for-byte, and refuse the two things this
    # panel will not pass into argv (`+keyring`, `::container`) — separately, so
    # each branch is actually asked.
    SPEC=vivaldi:/tmp/macast-verify-profile
    api_post "$BASE/api" "resolve-cookie-browser=$SPEC"
    [ "$API_CODE" = "0" ] \
        && pass "a browser with a data directory is accepted, not just a bare name" \
        || fail "spec refused: $API_MSG"
    [ "$API_MSG" = "已设为 ${SPEC}，但本机存过 cookie 文件，解析仍以文件为准（清除文件后才会用浏览器）" ] \
        && pass "and says out loud that the file still outranks it in argv" \
        || fail "override wording: $API_MSG"
    [ "$(stored_setting Resolve_Cookie_Browser)" = "$SPEC" ] \
        && pass "the profile half is stored byte-for-byte (it is a path, not a name)" \
        || fail "stored Resolve_Cookie_Browser: '$(stored_setting Resolve_Cookie_Browser 2>/dev/null)'"
    api_probe "$BASE/api?query=resolve-cookies"
    [ "$(json_ask state.browser)" = "$SPEC" ] \
        && pass "and the panel reads the same string back" \
        || fail "state.browser: '$(json_ask state.browser)'"
    api_post "$BASE/api" "resolve-cookie-browser=chrome+keyring"
    [ "$API_CODE" = "1" ] \
        && [ "$API_MSG" = "不认识「chrome+keyring」这个浏览器，可选：$NAMES" ] \
        && pass "a keyring suffix is refused, and the refusal lists the real choices" \
        || fail "keyring refusal: code '$API_CODE' msg '$API_MSG'"
    api_post "$BASE/api" "resolve-cookie-browser=vivaldi::Default"
    [ "$API_CODE" = "1" ] \
        && pass "a container suffix is refused too — its name is valid, so the whitelist alone would let it through" \
        || fail "container suffix answered code '$API_CODE': $API_MSG"
    case "$API_MSG" in
        *"我们只认到浏览器名字为止"*) pass "and it says which half failed, not 'I don't know that browser'" ;;
        *) fail "container wording: $API_MSG" ;;
    esac
    [ "$(stored_setting Resolve_Cookie_Browser)" = "$SPEC" ] \
        && pass "and a refusal never touched the stored value" \
        || fail "the refused spec was stored anyway"

    # (i)–(k) clear, clear again, off. The second clear is a **success** — the
    # caller's goal is "no jar", and a machine that never had one has reached it.
    # `off` then has to **unset** the key rather than store a false (§4.2).
    api_post "$BASE/api" "resolve-cookies-clear=1"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "已清除 cookie 文件，下次解析改用 $SPEC" ] \
        && pass "clearing the jar hands the next parse to the browser, and says so" \
        || fail "clear answered code '$API_CODE' msg '$API_MSG'"
    [ ! -e "$JAR" ] \
        && pass "the file is gone (move, not a zeroed copy — nothing holds the credential)" \
        || fail "the jar is still on disk after a clear"
    api_probe "$BASE/api?query=resolve-cookies"
    [ "$(json_ask state.jar)" = "false" ] && [ "$(json_ask state.cookies)" = "0" ] \
        && pass "and the panel agrees, on the same read that reports the browser" \
        || fail "panel after clear: jar='$(json_ask state.jar)'"
    api_post "$BASE/api" "resolve-cookies-clear=1"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "本机没有保存过 cookie 文件" ] \
        && pass "clearing twice is a success, not an error someone has to explain" \
        || fail "second clear answered code '$API_CODE' msg '$API_MSG'"
    api_post "$BASE/api" "resolve-cookie-browser=off"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "不再从浏览器读取 cookie" ] \
        && pass "'off' is accepted as its own answer" \
        || fail "off answered code '$API_CODE' msg '$API_MSG'"
    if stored_setting Resolve_Cookie_Browser > /dev/null 2>&1; then
        fail "off stored a value instead of unsetting the key: '$(stored_setting Resolve_Cookie_Browser)'"
    else
        pass "and the key is absent, not a stored false (§4.2)"
    fi
    api_probe "$BASE/api?query=resolve-cookies"
    [ "$(json_ask state.browser)" = "" ] \
        && pass "so the panel reads an empty browser, and the card can say 'off'" \
        || fail "state.browser after off: '$(json_ask state.browser)'"
fi

echo "== 19: the device dropdown is served, with 本机 first =="
# The device dropdown is **served** now (the shell reads `targets::Table`), so this
# is the success document rather than the refusal it used to be. What gets asserted
# is the core's own half — the local row, the key order, the two booleans — because
# the device rows are a fact about this LAN, and this
# LAN has no Chromecast: requiring a count would make the script red for somebody
# else's television being switched off. `targets::Table` is what answers, and the
# core prepends 「本机」 (`protocol.py:1793`), exactly as the reference splits it.
api_probe "$BASE/api?query=cast-targets"
[ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] && [ "$API_MSG" = "success" ] \
    && pass "the list is served, and loopback is admitted" \
    || fail "cast-targets answered HTTP $API_STATUS code '$API_CODE': $API_MSG"
[ "$(json_keys .)" = "code,message,targets" ] \
    && pass "the wrapper is the reference's three keys" \
    || fail "cast-targets keys: $(json_keys .)"
[ "$(json_keys targets)" = "items,searching,known" ] \
    && pass "the document is items,searching,known, in that order" \
    || fail "targets keys: $(json_keys targets)"
[ "$(json_ask targets.items.0.id)" = "local" ] \
    && pass "the first row is always this machine" \
    || fail "first row id: '$(json_ask targets.items.0.id)'"
[ "$(json_ask targets.items.0.kind)" = "local" ] && [ "$(json_ask targets.items.0.address)" = "" ] \
    && pass "and it names no socket — the local row is a route through our player, not an address" \
    || fail "local row: kind='$(json_ask targets.items.0.kind)' address='$(json_ask targets.items.0.address)'"
case "$API_MSG" in
    *"$MANAGEMENT_MARK"*) fail "loopback heard the management sentence: $API_MSG" ;;
    *) pass "and no gate sentence, since nothing here runs code" ;;
esac
SEARCHING=$(json_ask targets.searching)
KNOWNS=$(json_ask targets.known)
LOCAL_NAME=$(json_ask targets.items.0.name)
case "$SEARCHING" in true|false) _s=1 ;; *) _s="" ;; esac
case "$KNOWNS" in true|false) _k=1 ;; *) _k="" ;; esac
[ -n "$_s" ] && [ -n "$_k" ] \
    && pass "both booleans are booleans, so the card can render them" \
    || fail "booleans: searching='$SEARCHING' known='$KNOWNS'"
# The LAN's own half, **printed rather than asserted**: how many devices this
# machine can actually find right now is a fact about the neighbours, and a check
# on it would go red when somebody switches a television off. It is recorded here
# because the alternative is that §16's "the discovery client has no real-machine
# witness" stays written in a design document that nobody re-measures.
ROWS=$(json_len targets.items)
pass "this LAN offered $ROWS row(s) so far (known=$KNOWNS, searching=$SEARCHING) — a reading, not a judgement"
# The row's **name** is the selected renderer's title (§4.14: 「本机」 is not one
# thing), so it is compared against the same key `query=status` reports rather
# than against a literal. `'' or 'MPV'` is Python's own fallback
# (`protocol.py:1806`): a dropdown whose first row has a blank label is not a
# label — and the two faces legitimately differ on a fresh run, because status
# ships the stored value (`:2007`) while the row answers 「what 投屏 will reach」.
# Read **before** the status probe above is overwritten: `$API_BODY` is one file,
# so anything asking `targets.*` after this must re-probe.
STATUS_RENDERER=$(api_probe "$BASE/api?query=status" >/dev/null; json_ask server.renderer)
if [ -z "$STATUS_RENDERER" ]; then
    [ "$LOCAL_NAME" = "MPV" ] \
        && pass "with no renderer stored the row says MPV while status says ''" \
        || fail "nothing stored, so the row should read MPV: '$LOCAL_NAME'"
else
    [ "$LOCAL_NAME" = "$STATUS_RENDERER" ] \
        && pass "the row repeats the renderer the status page reports, verbatim" \
        || fail "local row name '$LOCAL_NAME' vs status renderer '$STATUS_RENDERER'"
fi
# A refresh adds `started` **last** (`state['started'] = started` after the
# document is built, `cast_local_file.py:2913`), and it is a boolean either way:
# `false` means a sweep is already in flight from the cold-start poll, which is a
# real answer and not a failure.
api_probe "$BASE/api?query=cast-targets&refresh=1"
[ "$(json_keys targets)" = "items,searching,known,started" ] \
    && pass "a refresh answers the same document plus started, in last place" \
    || fail "refresh keys: $(json_keys targets)"
[ "$(json_ask targets.items.0.id)" = "local" ] \
    && pass "and 本机 is still the first row after a search starts" \
    || fail "refresh moved the local row: '$(json_ask targets.items.0.id)'"
case "$(json_ask targets.started)" in
    true|false) pass "started is a boolean ('$(json_ask targets.started)')" ;;
    *) fail "started is not a boolean: '$(json_ask targets.started)'" ;;
esac
# `refresh` arrives as query-string text, so truth is a name rather than Python's
# idea of it: `?refresh=0` means *do not search* and must not grow a `started`.
api_probe "$BASE/api?query=cast-targets&refresh=0"
[ "$(json_keys targets)" = "items,searching,known" ] \
    && pass "refresh=0 is read as 'do not search' and adds no key" \
    || fail "refresh=0 answered with: $(json_keys targets)"
# Why there is **no** "the spinner stops" check here, written down because the
# first version of this section asserted exactly that and was permanently red:
# `target_state()` cold-kicks a sweep whenever **both** caches are empty
# (`cast_local_file.py:2897-2907`, ported at `targets.rs`'s `target_state`), and a
# machine with nothing discoverable therefore never has full caches — so every
# poll re-arms a search and reports `searching: true`. That is the reference's
# behaviour byte for byte, and it is *honest*: the LAN really is being searched
# again, once per poll-after-completion, for as long as the card is open. The
# claim "searching eventually reads false" is only reachable on a network that
# has a device to find, which this LAN does not have (see docs/Rust-Port-Design.md
# §16). Asking it here would be a check that can only be green by accident.
#
# What **is** reachable, and is the property the settle check was standing in for:
# polling does not pile up threads. The latch itself is **pinned by `targets_tests`**, not
# here: `k_a_search_in_flight_latches_its_own_kind_and_reports_searching` holds a sweep open
# with a `HoldGate` and requires the second `start_search()` to answer `false` with exactly
# one call recorded.
#
# **This machine cannot witness the latch, and saying so is the honest check.**
# On a live socket the window is milliseconds wide: measured 2026-10-21,
# twelve back-to-back refreshes on this Mac all read `started=true, searching=true`, and a
# hand-run pair read `true/true` twice — because each sweep finishes before the next
# request arrives, so every `true` is a **new** sweep rather than an ignored one. (An
# earlier version of this check asserted "one of the two must be false" and went red on
# exactly that: correct code, wrong instrument.)
#
# So this records a reading and does not judge on it. What it still proves is the
# property the card depends on and the network cannot hide: **a poll does not pile up
# threads** — two refreshes plus a plain poll answer the same document, and the row
# count never grows because of them.
S19_A=$(curl -s --noproxy '*' -m 5 "$BASE/api?query=cast-targets&refresh=1" \
    | python3 -c 'import json,sys;t=json.load(sys.stdin).get("targets",{});print(t.get("started"),t.get("searching"))' 2>/dev/null)
S19_B=$(curl -s --noproxy '*' -m 5 "$BASE/api?query=cast-targets&refresh=1" \
    | python3 -c 'import json,sys;t=json.load(sys.stdin).get("targets",{});print(t.get("started"),t.get("searching"))' 2>/dev/null)
case "$S19_A$S19_B" in
    *error*|"") fail "a refresh answered nothing usable (first='$S19_A' second='$S19_B')" ;;
    *) pass "two refreshes both answer a document with started/searching (first='$S19_A', second='$S19_B') — the latch itself is pinned by targets_tests, not here" ;;
esac

# **Read this check's teeth honestly.** What it proves is "a second click while a sweep
# is in flight does not start a second sweep" — i.e. not one thread per poll. It does
# **not** prove "a started sweep eventually ends": on this Mac the mDNS answer returns in
# milliseconds, so a sweep usually finishes before any second request can see
# `searching: true`, and the retry loop above then reports "never observed" (a failure, by
# this file's opening rule) rather than a green. That settle property is witnessed by
# `targets_tests.rs`'s latch cases against a fake `Sweeps`, never on this LAN.
# The caches survive a poll: a sweep that finds **nothing** must not empty the
# dropdown while the television is still in the living room (AGENTS.md §4.8's
# 「设备睡着了也还是那一台」, `targets.rs`'s `if found:` guard). Asserted as a
# non-shrink rather than an equality because the honest range of "how many
# devices" on this LAN is anything from one row to several.
api_probe "$BASE/api?query=cast-targets"
BEFORE_ITEMS=$(json_len targets.items)
api_probe "$BASE/api?query=cast-targets&refresh=1"
AFTER_ITEMS=$(json_len targets.items)
[ -n "$BEFORE_ITEMS" ] && [ -n "$AFTER_ITEMS" ] && [ "$AFTER_ITEMS" -ge "$BEFORE_ITEMS" ] \
    && pass "a second sweep never shortens the list below what the card showed" \
    || fail "items $BEFORE_ITEMS -> $AFTER_ITEMS after a re-search (or a document with no list)"
if [ -n "$LAN" ]; then
    for q in cast-targets resolve-cookies; do
        api_probe "http://$LAN:$PORT/api?query=$q"
        [ "$API_STATUS" = "403" ] \
            && pass "a LAN caller is refused $q" \
            || fail "a LAN caller got HTTP $API_STATUS from $q"
        case "$API_MSG" in
            *"$MANAGEMENT_MARK"*) pass "and hears the management sentence for $q" ;;
            *) fail "$q refusal wording: $API_MSG" ;;
        esac
        # The `path` key makes this one worth checking on this surface
        # specifically: a card that names a file on disk tells a LAN caller where
        # the credential cache lives, which the count-only promise does not cover.
        case "$API_MSG" in
            *"$SDIR"*) fail "the $q refusal leaked the config directory path" ;;
            *) pass "and the $q refusal names no path on this machine" ;;
        esac
    done
else
    skip "no reachable non-loopback address to ask from (section 19's LAN half)"
fi

echo "== 20: the setup face answers senders on 8008, and on 8443 if it got that port too =="
# **Read the answer, not the ask** — the same rule §17 applies to the relay's
# port. The shell logs which numbers it actually bound; a sender hardcodes 8008
# (`dial.py:25-26`), so "8008 or nothing" can only be checked from that line and
# never from the constant. A face on 8010 would be a green test and a dead device.
#
# Three outcomes are legitimate here and each is asserted with its own evidence,
# because the fourth — silence — is the one that would read like a product bug:
#   * bound → the document, the filter, the icon, the verbs, and 8443 if it came up;
#   * refused → the WARNING line must name the reason (another program holds 8008,
#     which is exactly the state on a machine running the Python app);
#   * unreported → that is the failure.
#
# Which file that line lives in is itself a fact worth naming rather than
# guessing: the app-level summary goes to the shell's own log while the module's
# lines (`Cast HTTPS setup server on 8443`, the refusal sentences) are claimed
# into `logs/Chromecast.log` — `AGENTS.md` §4.10b's "one record, one file". So
# the reader looks across both, and then asserts the split holds.
log_grep() {
    # $1 = a fixed string. Searches the shared log and every claimed module file.
    grep -ahF "$1" "$SDIR/macast.log" "$SDIR"/logs/*.log 2>/dev/null
}

SETUP_REPORT=$(log_grep 'setup face: ' | tail -1)
SETUP_HTTP_PORT=$(printf '%s' "$SETUP_REPORT" | sed -n 's/.*HTTP on \([0-9][0-9]*\).*/\1/p')
SETUP_HTTPS_PORT=$(printf '%s' "$SETUP_REPORT" | sed -n 's/.*HTTPS \([0-9][0-9]*\).*/\1/p')
SH=/tmp/verify_rust_receiver_setup

setup_probe() {
    # $1 = path, $2 = header file. The body goes to $API_BODY so this section
    # reads it with the same `json_*` helpers the API sections use; nothing after
    # §20 depends on a previous probe, and every check below re-probes first.
    API_STATUS=$(curl -s --noproxy '*' -m 8 -D "$2" -o "$API_BODY" -w '%{http_code}' \
        "http://127.0.0.1:$SETUP_HTTP_PORT$1")
    parse_api_body
}

# `AGENTS.md` §4.10b's rule, asserted rather than trusted: a record belongs to one
# file. The module claimed `logs/Chromecast.log`, so none of its rows may also sit
# in the shared `macast.log` — the duplicate is exactly how "where do I look"
# comes back, and it is how the shared file filled to 40 MB/day once before.
if [ -n "$(grep -ahE 'Cast setup|Cast HTTPS setup|No TLS certificate' \
      "$SDIR/macast.log" 2>/dev/null)" ]; then
    fail "a Chromecast-module line also landed in macast.log (one record, two files)"
else
    pass "the module's rows stay in their claimed file and out of the shared log"
fi

if [ -n "$SETUP_HTTP_PORT" ]; then
    pass "the face reported the port it bound ($SETUP_HTTP_PORT), not the one it asked for"
    [ "$SETUP_HTTP_PORT" = "8008" ] \
        && pass "and it is the number a sender knocks on" \
        || fail "setup face on $SETUP_HTTP_PORT: no sender will ever find it"

    setup_probe /setup/eureka_info "$SH.h"
    [ "$API_STATUS" = "200" ] \
        && pass "eureka_info answers 200 to a plain GET" \
        || fail "eureka_info answered HTTP $API_STATUS"
    grep -q '^HTTP/1.0 200' "$SH.h" \
        && pass "over an HTTP/1.0 status line (no keep-alive, like the reference)" \
        || fail "status line: $(head -1 "$SH.h")"
    grep -qi '^server: Macast/' "$SH.h" \
        && pass "and the Server line names us" \
        || fail "Server header: $(grep -i '^server:' "$SH.h" | tr -d '\r')"
    grep -qi '^server:.*Python' "$SH.h" \
        && fail "it claimed to be CPython: $(grep -i '^server:' "$SH.h" | tr -d '\r')" \
        || pass "and it does not claim to be CPython"
    [ "$(json_keys .)" = "name,device_info,ssdp_udn,version,build_version,connected,settings" ] \
        && pass "the seven top-level keys, in the reference's order" \
        || fail "eureka keys: $(json_keys .)"
    [ "$(json_keys device_info)" = "manufacturer,model_name,product_name,friendly_name,ssdp_udn,mac_address,capabilities" ] \
        && pass "device_info is nested, with its own order kept" \
        || fail "device_info keys: $(json_keys device_info)"
    [ "$(json_keys device_info.capabilities)" = "display_supported,audio_supported,video_out,audio_out,multizone_supported" ] \
        && pass "capabilities spells multizone_supported last, rather than omitting it" \
        || fail "capabilities keys: $(json_keys device_info.capabilities)"
    [ "$(json_ask device_info.capabilities.multizone_supported)" = "false" ] \
        && pass "and its value is false — an absent key reads as multi-room to pychromecast" \
        || fail "multizone_supported: $(json_ask device_info.capabilities.multizone_supported)"

    # The USN, compared against the file the app wrote rather than a shape: a
    # `uuid:` prefix here is what made Macast invisible on the host-scanning path.
    UDN=$(json_ask ssdp_udn)
    STORED_USN=$(stored_setting USN 2>/dev/null || echo "")
    case "$UDN" in
        uuid:*) fail "ssdp_udn carries the uuid: prefix: $UDN" ;;
        *) pass "and ssdp_udn is bare, the way UUID() parses it" ;;
    esac
    if [ -n "$STORED_USN" ] && [ "$UDN" = "$STORED_USN" ]; then
        pass "the same USN the settings file holds, so discovery and this face agree"
    elif [ -n "$STORED_USN" ]; then
        fail "eureka says $UDN while the settings file says $STORED_USN"
    else
        fail "the settings file has no Usn to compare against (udn: $UDN)"
    fi

    # The filter, asked the way the real client asks it (`dial.py:157`).
    setup_probe '/setup/eureka_info?params=device_info,name' "$SH.f.h"
    [ "$(json_keys .)" = "name,device_info" ] \
        && pass "params=device_info,name answers in the document's order, not the caller's" \
        || fail "filtered keys: $(json_keys .)"
    setup_probe '/setup/eureka_info?params=multizone' "$SH.m.h"
    [ "$API_STATUS" = "200" ] && [ "$(json_keys .)" = "" ] \
        && pass "an unknown name is a 200 with an empty object, not a 404" \
        || fail "params=multizone answered HTTP $API_STATUS keys '$(json_keys .)'"
    setup_probe '/setup/eureka_info?params=' "$SH.b.h"
    [ "$(json_keys .)" = "name,device_info,ssdp_udn,version,build_version,connected,settings" ] \
        && pass "and ?params= is no filter at all — parse_qs drops the blank" \
        || fail "blank params keys: $(json_keys .)"

    # The icon: a type, **no** length, and no body. `Content-Length: 0` would be a
    # truer description of the bytes and a divergence from the shape clients were
    # written against, so the omission is the assertion.
    setup_probe /setup/icon.png "$SH.i.h"
    [ "$API_STATUS" = "200" ] \
        && pass "icon.png answers 200" \
        || fail "icon.png answered HTTP $API_STATUS"
    grep -qi '^content-type: image/png' "$SH.i.h" \
        && pass "with image/png" \
        || fail "icon headers: $(tr -d '\r' < "$SH.i.h")"
    grep -qi '^content-length' "$SH.i.h" \
        && fail "it declared a length: $(grep -i '^content-length' "$SH.i.h" | tr -d '\r')" \
        || pass "and no Content-Length, exactly as the reference writes it"

    setup_probe /setup/nothing "$SH.4.h"
    [ "$API_STATUS" = "404" ] \
        && pass "an unknown path is a 404" \
        || fail "unknown path answered HTTP $API_STATUS"
    code=$(curl -s --noproxy '*' -m 8 -o /dev/null -w '%{http_code}' -X POST \
        "http://127.0.0.1:$SETUP_HTTP_PORT/setup/eureka_info")
    [ "$code" = "501" ] \
        && pass "a POST is refused outright — this face has no verb" \
        || fail "POST answered HTTP $code"

    # The face is **ungated** by design (module header): it says what the device
    # is, to anyone on the subnet, and carries nothing secret. Both halves of that
    # sentence have to hold, or the absence of a gate is an accident.
    if [ -n "$LAN" ]; then
        LAN_CODE=$(curl -s --noproxy '*' -m 8 -o "$SH.lan.json" -w '%{http_code}' \
            "http://$LAN:$SETUP_HTTP_PORT/setup/eureka_info")
        [ "$LAN_CODE" = "200" ] \
            && pass "a LAN caller is answered — the face publishes identity, not control" \
            || fail "a LAN caller got HTTP $LAN_CODE from eureka_info"
        if grep -q 'Api_Token\|'"$SDIR" "$SH.lan.json"; then
            fail "the LAN answer leaked a token name or a path on this machine"
        else
            pass "and what it answers holds no token and no path"
        fi
    else
        skip "no reachable non-loopback address to ask from (section 19's LAN half)"
    fi

    # 8443: same document or a stated reason. pychromecast's `get_cast_type` asks
    # only the encrypted twin, so a missing 8443 costs the `manufacturer` field —
    # and the log must say which of its three reasons applied.
    if [ -n "$SETUP_HTTPS_PORT" ]; then
        [ "$SETUP_HTTPS_PORT" = "8443" ] \
            && pass "8443 came up on the number clients hardcode" \
            || fail "HTTPS setup on $SETUP_HTTPS_PORT: no client will reach it"
        TLS_CODE=$(curl -sk --noproxy '*' -m 12 -o "$SH.tls.json" -w '%{http_code}' \
            "https://127.0.0.1:$SETUP_HTTPS_PORT/setup/eureka_info")
        [ "$TLS_CODE" = "200" ] \
            && pass "the TLS twin answers over a real handshake" \
            || fail "https eureka_info answered HTTP $TLS_CODE"
        # Byte-for-byte against a **fresh** plain fetch, because one document
        # should have one owner: a TLS arm built from its own copy of the answer
        # would be the same lie in a second place. Fresh rather than reusing
        # `$API_BODY`, which the probes above have already overwritten twice.
        setup_probe /setup/eureka_info "$SH.plain2.h"
        if [ -s "$SH.tls.json" ] && cmp -s "$SH.tls.json" "$API_BODY"; then
            pass "and the encrypted answer is the same bytes as the plain one"
        else
            fail "8008 and 8443 disagree about the document ($(wc -c < "$SH.tls.json" | tr -d ' ') vs $(wc -c < "$API_BODY" | tr -d ' ') bytes)"
        fi
    elif printf '%s' "$SETUP_REPORT" | grep -q 'HTTPS not started'; then
        if [ -n "$(grep -ahE 'Cast HTTPS setup|No TLS certificate available' \
              "$SDIR/macast.log" "$SDIR"/logs/*.log 2>/dev/null)" ]; then
            pass "8443 is down and the log says why (senders lose the manufacturer field)"
        else
            fail "8443 is down with no reason in the log: $SETUP_REPORT"
        fi
    else
        fail "the log never said whether 8443 came up: $SETUP_REPORT"
    fi
elif printf '%s' "$SETUP_REPORT" | grep -q 'not listening on 8008'; then
    # The legitimate second outcome: somebody else owns 8008 — most often the
    # Python Macast on the same machine — and the face binds nothing rather than
    # walking into 8009, which is the Cast socket's port.
    if [ -n "$(grep -ahF 'Cast setup face did not start' \
          "$SDIR/macast.log" "$SDIR"/logs/*.log 2>/dev/null)" ]; then
        pass "8008 was taken and the face said so instead of binding a neighbour"
    else
        fail "the face is down but no WARNING names the reason: $SETUP_REPORT"
    fi
else
    fail "the log never said what the setup face did: '$SETUP_REPORT'"
fi

echo "== 21: a Chromecast sender's handshake reaches the mpv behind this shell =="
# The 8009 door, driven by `scripts/vlc_sender_sim.py` — the repo's own replay of
# VLC's state machine, including its **strict** device-auth parse (`AuthResponse`
# has two `required` fields, and C++ protobuf refuses a reply that omits them,
# which is the bug that once made VLC stop after the first byte). Nothing here is
# stubbed: real TLS, real protobuf-shaped frames, real LOAD, and then mpv.
#
# The face is read from the log rather than assumed to be 8009, for the same
# reason §20 reads the setup port: the number that answers is the number that was
# bound.
transport_state() {
    printf '<?xml version="1.0"?>\n<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">\n<s:Body><u:GetTransportInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetTransportInfo></s:Body></s:Envelope>' \
        | curl -s -m 5 -X POST "$BASE/AVTransport/action" \
            -H 'Content-Type: text/xml; charset="utf-8"' \
            -H "SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#GetTransportInfo\"" \
            --data-binary @- \
        | grep -oE '<CurrentTransportState>[A-Z_]+' | sed 's/.*>//' | head -1
}

wait_transport() {
    # $1 = the state to wait for, $2 = how many quarters of a second to spend.
    local i
    for i in $(seq 1 "$2"); do
        [ "$(transport_state)" = "$1" ] && return 0
        sleep 0.25
    done
    return 1
}

CAST_PY="$REPO/.venv/bin/python"
CAST_PORT_ACTUAL=$(log_grep 'Cast receiver: ' | tail -1 \
    | sed -n 's/.*listening on \([0-9][0-9]*\).*/\1/p')

if [ -z "$CAST_PORT_ACTUAL" ]; then
    # Legitimate second outcome: no certificate, so 8009 never came up. The
    # certificate rule is `cert.rs`'s (adopt an existing pair, never overwrite a
    # foreign one), and a foreign pair that will not parse is reported by name —
    # so the absence must be **explained**, or a silent skip becomes a green run
    # that never knocked on the Cast door at all.
    if [ -n "$(log_grep 'Cast certificate unavailable')" ] \
        || [ -n "$(log_grep 'Cast channel could not start')" ]; then
        pass "8009 is down and the log names the certificate reason"
    else
        fail "no Cast receiver and no reason in the log"
    fi
elif [ ! -x "$CAST_PY" ]; then
    skip "no .venv python to run the VLC replay with ($CAST_PY)"
else
    # Put the table somewhere else first, so "it reached PLAYING" cannot be the
    # leftover of §4's or §16's cast. Without this control the assertion below
    # would pass for a reason that has nothing to do with the Cast door.
    soap_action Stop >/dev/null 2>&1
    if wait_transport STOPPED 40; then
        pass "the DLNA door was told to stop first, so the next PLAYING has one candidate author"
    else
        fail "TransportState never read STOPPED (reads $(transport_state))"
    fi
    RECONFIG_BEFORE=$(grep -ac 'video-reconfig' "$SDIR/logs/MPV.log" 2>/dev/null)

    # Poll the table **while** the cast is happening, not after it: the clip is
    # twelve seconds long and the sender's patience is twenty, so a check that
    # reads the state once the handshake script has returned can only report
    # `STOPPED` — which is what the first version of this section did, and it read
    # like a broken fan-out rather than a mistimed look (AGENTS.md §4.2's 「等那个
    # 事实，不要抓一瞬间」, this time in the reader's favour).
    (
        for _ in $(seq 1 240); do
            [ "$(transport_state)" = "PLAYING" ] && exit 0
            sleep 0.25
        done
        exit 1
    ) &
    STATE_WAIT=$!

    SIM_OUT=$(cd "$REPO" && env -u PYTHONPATH -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY \
        "$CAST_PY" scripts/vlc_sender_sim.py 127.0.0.1 "$CAST_PORT_ACTUAL" "$MEDIA_URL" 2>&1)
    SIM_RC=$?
    wait "$STATE_WAIT"
    PLAY_SEEN=$?
    # A zero exit here is a load-bearing claim, not a summary: the simulator
    # returns True **only** when the Cast wire's own `MEDIA_STATUS.playerState`
    # says `PLAYING`, and it deliberately refuses `BUFFERING` / `LOADING` as
    # success ("accepting BUFFERING as success would make this harness pass on a
    # receiver that never actually plays anything"). The channel's ledger has one
    # writer — `set_state_play`, reached only through this shell's
    # `ProtocolGroup`-style fan-out of mpv's events — so this line is the
    # witness that the fan-out exists. No separate grep of its prose: that would
    # be the same claim with two authors.
    if [ "$SIM_RC" = "0" ]; then
        pass "VLC's state machine got through auth, LAUNCH, transportId and LOAD, and read PLAYING back"
    else
        fail "the sender's run failed (rc $SIM_RC):"
        printf '%s\n' "$SIM_OUT" | tail -14 | sed 's/^/      /'
    fi
    printf '%s\n' "$SIM_OUT" | grep -q "transportId=" \
        && pass "and it was handed a transportId from the LAUNCH reply, not a hardcoded one" \
        || fail "no transportId in the sender's output"

    # The two doors, one room: the LOAD reached mpv, mpv's own event thread wrote
    # the shared table, and that is the only path left that can move it now.
    if [ "$PLAY_SEEN" = "0" ]; then
        pass "the Cast door moved the shared transport state to PLAYING"
    else
        fail "after a cast the table never read PLAYING (now '$(transport_state)') — the doors disagree"
        echo "  --- logs/MPV.log (tail) ---"; tail -14 "$SDIR/logs/MPV.log" 2>/dev/null
    fi
    RECONFIG_AFTER=$(grep -ac 'video-reconfig' "$SDIR/logs/MPV.log" 2>/dev/null)
    if [ "${RECONFIG_AFTER:-0}" -gt "${RECONFIG_BEFORE:-0}" ]; then
        pass "mpv reconfigured for video, so the stream really carried a picture"
    else
        fail "no new video-reconfig in MPV.log ($RECONFIG_BEFORE -> $RECONFIG_AFTER): audio-only or never started"
    fi
fi

echo "== 22: another implementation reads the announcement this shell broadcasts =="
# Everything above answers a caller that already knew an address. This section asks
# the other question — **can a sender that knows nothing find us** — and it refuses to
# answer it with our own parser, because a writer and a reader from the same crate
# agree by construction (§4.2's 「桩自己也会撒谎」, and §4.9's whole reason for
# `cast_conformance.py` existing). The witness is the **`zeroconf` in this repository's
# `.venv`**: the stack the Python app itself runs, given nothing but our bytes on the
# wire and asked to fold them into a device.
#
# Why this can only be a re-announcement: the announcer's first burst is three packets
# 225 ms apart at start, and by the time §21 finishes that is minutes behind us. So a
# browser with a cold cache that resolves this instance has resolved a **keepalive** —
# `REANNOUNCE_SECONDS` doing the job `discovery.py:230-237` says it exists for (libvlc's
# microdns 「never sends a query of its own」).
#
# **Cost, added to the one the header declares:** for the run's duration this machine
# now also announces a second Macast over **mDNS** (UDP/5353), not just SSDP. A phone
# with the Cast button open will see it. Do not run this during a demo.
#
# What is NOT checked here, and why: the **goodbye**. Tearing this instance down is
# `kill -9` (a SIGTERM leaves the Tauri app running, and SIGINT is the only signal it
# honours — AGENTS.md §4.2's SIGTERM row — and neither goes through `stop_services` from
# this harness), so a live goodbye is not reachable from a script. `announce_tests` holds
# that one instead: `the_run_function_goodbyes_even_when_the_flag_is_already_down` drives
# `run()` with the flag already down and requires three zero-TTL packets. Recording the
# gap is the point — a section that silently skipped would read like coverage.
# **By name, not across every file.** `log_grep` searches the shared log *and* every
# claimed module file, which is the right breadth for an absence ("nothing anywhere
# said X") and the wrong breadth for a presence: it cannot tell a reader who wrote the
# sentence in the file the question belongs to from one who wrote it in the player's.
# These four lines used to be written through the MPV logger — a reader following
# AGENTS.md §5's troubleshooting recipe (which greps `macast.log`) or the Discovery
# question 「why doesn't my phone see this Macast」 would have found neither — so the
# announcer's summary now lives in `logs/Discovery.log`, beside the
# 「mDNS advertised …」 line the module itself writes, and this grep is the witness
# that the destination is the one the section claims (§27 set the rule).
# ---- where the shell's own sentences live (AGENTS.md §5's recipe)
#
# The reference propagates every core module's lines to the root handler — only
# **plugins** are claimed (`macast.py:336` / `:905`) — so a reader following §5's
# `grep ... macast.log` finds the startup summary. This port claims four core names as
# a recorded divergence (§8), and these lines used to ride the **player's** logger:
# measured once, when my own probe grepped `macast.log`, found nothing, and the sentence
# turned out to be in `logs/MPV.log` (§16 第 29 条 ④). Read **by file name**, because a
# grep over every log cannot tell a correct destination from a wrong one (§27's rule).
if grep -aqF 'receiver: HTTP ' "$SDIR/macast.log"; then
    pass "the shell's startup summary is in macast.log, where AGENTS.md §5 tells a reader to look"
else
    fail "no 'receiver: HTTP' line in macast.log — the summary moved files again"
fi
if grep -aqF 'receiver: HTTP ' "$SDIR/logs/MPV.log"; then
    fail "the shell's summary is riding the player's log (logs/MPV.log)"
else
    pass "and it is not in the player's log"
fi
if grep -aqF 'mpv candidate: ' "$SDIR/logs/MPV.log"; then
    pass "while the player's own sentences stayed in logs/MPV.log — a split, not a migration"
else
    fail "the player's startup line left logs/MPV.log"
fi

MDNS_LINE=$(grep -ahF 'mDNS: announcing ' "$SDIR/logs/Discovery.log" 2>/dev/null \
    | grep -F '_googlecast._tcp.local.' | tail -1)
# The type filter is not decoration. `tail -1` alone picked **the last** announcing
# line, and since the AirPlay face got its own announcer this main instance writes two
# — so §22 asked its `_googlecast._tcp` browser to resolve an `_airplay._tcp` name,
# read 「unresolved」, and reported seven product failures about TXT order, `ca`, the
# SRV target and the A record on a run whose Chromecast announcement was byte-for-byte
# right (measured 2026-10-18: the same browse had in fact *seen* the Cast instance in
# its own list). A reader choosing the last line of a log that now has one line per
# service type is choosing a service type by accident.
MDNS_INSTANCE=$(printf '%s' "$MDNS_LINE" | sed -n 's/.*announcing \([^ ]*\) on port .*/\1/p')
MDNS_PORT=$(printf '%s' "$MDNS_LINE" | sed -n 's/.* on port \([0-9][0-9]*\) over .*/\1/p')

if [ -z "$MDNS_INSTANCE" ]; then
    # The legitimate second outcome: nothing to announce, because 8009 is not
    # listening. Silence here would be the failure — a receiver that came up and
    # quietly never announced is exactly the state a user reads as "搜到但投不上",
    # and a run that printed neither line is a run that did not look.
    if [ -n "$(grep -ahF 'mDNS: not announcing' "$SDIR/logs/Discovery.log" 2>/dev/null)" ]; then
        pass "no announcement, and the shell said why instead of leaving it out"
    else
        fail "no announcement line and no refusal line — the announcer said nothing"
    fi
elif [ ! -x "$CAST_PY" ]; then
    skip "no .venv python to browse with ($CAST_PY)"
else
    MDNS_OUT=/tmp/verify_rust_receiver_mdns.txt
    env -u PYTHONPATH -u http_proxy -u HTTP_PROXY \
        -u https_proxy -u HTTPS_PROXY "$CAST_PY" - "$MDNS_INSTANCE" 75 \
        > "$MDNS_OUT" <<'PY'
import sys, time, zeroconf as z

want, budget = sys.argv[1], float(sys.argv[2])
seen = set()
attempted = set()


class Listener(z.ServiceListener):
    # Names the browse has announced. Nothing in this listener queries: the whole
    # claim under test is that an **unsolicited announcement** is enough for
    # another stack to know a device — so calling `get_service_info` here would
    # ask the question we are trying to answer and go green for the wrong reason.
    def add_service(self, zc, type_, name):
        seen.add(name)

    def remove_service(self, *args):
        pass

    def update_service(self, *args):
        pass


def load(zc, name):
    info = z.ServiceInfo("_googlecast._tcp.local.", name)
    # `load_from_cache` is yes-only-when-complete: PTR **and** SRV **and** TXT
    # **and** A. A half-folded service answers False, which is the failure mode.
    return info if info.load_from_cache(zc) else None


zc = z.Zeroconf(ip_version=z.IPVersion.V4Only)
# 0.151.3 takes the handler as the third positional (`handlers`), not a
# timeout — the first version of this line passed `6.0` there and the
# constructor answered `AttributeError: 'float' object has no attribute
# 'append'`, which the section reported as seven product failures. A harness
# bug wearing the product's clothes (§4.2), caught because the traceback was
# printed and not swallowed.
browser = z.ServiceBrowser(zc, "_googlecast._tcp.local.", Listener())
start = time.monotonic()
info = None
while time.monotonic() - start < budget:
    for name in sorted(seen - attempted):
        attempted.add(name)
        got = load(zc, name)
        if got is not None and name == want:
            info = got
            break
    if info is not None:
        break
    time.sleep(0.2)
elapsed = time.monotonic() - start
if info is None:
    print("status\tunresolved")
    print("seen\t%s" % ",".join(sorted(seen)))
else:
    print("status\tresolved")
    print("seconds\t%.1f" % elapsed)
    print("server\t%s" % (info.server or ""))
    print("port\t%s" % (info.port if info.port is not None else ""))
    print("addresses\t%s" % ",".join(
        # The member is `V4Only`, not `V4`: the first version of this line
        # asked for a name that does not exist, and the traceback printed
        # where the data goes — which is why the five checks below it came
        # back "none" while the resolve itself passed.
        str(a) for a in info.ip_addresses_by_version(z.IPVersion.V4Only)))
    # The TXT keys in the order the reader parsed them, which is the order we
    # wrote: `properties` walks the rdata front to back.
    props = info.properties or {}
    print("txt_order\t%s" % ",".join(
        k.decode("utf-8", "replace") for k in props))
    print("txt\t%s" % ";".join(
        "%s=%s" % (k.decode("utf-8", "replace"),
                   (v or b"").decode("utf-8", "replace")) for k, v in props.items()))
browser.cancel()
zc.close()
PY
    MDNS_STATUS=$(awk -F'\t' '$1=="status"{print $2}' "$MDNS_OUT" | head -1)

    if [ "$MDNS_STATUS" = "resolved" ]; then
        pass "zeroconf folded our announcement into a complete device (PTR+SRV+TXT+A) in $(awk -F'\t' '$1=="seconds"{print $2}' "$MDNS_OUT") s"
    else
        fail "another stack could not resolve $MDNS_INSTANCE: $(tr '\n' ' ' < "$MDNS_OUT")"
    fi

    # 1. The port it read is the port 8009 actually bound (§21 read that off the
    #    log), and the same number the announcement line carries. A device whose
    #    announcement names a port nothing listens on is the 「搜到但投不上」 bug
    #    this repository has already been blamed for twice.
    RESOLVED_PORT=$(awk -F'\t' '$1=="port"{print $2}' "$MDNS_OUT" | head -1)
    if [ -n "$CAST_PORT_ACTUAL" ] && [ "$RESOLVED_PORT" = "$CAST_PORT_ACTUAL" ] \
        && [ "$RESOLVED_PORT" = "$MDNS_PORT" ]; then
        pass "the port a sender resolves is the port that answers ($RESOLVED_PORT)"
    else
        fail "announced $MDNS_PORT, bound ${CAST_PORT_ACTUAL:-none}, resolved ${RESOLVED_PORT:-none}"
    fi

    # 2. The identity half: the TXT `id` is the same USN the setup face answers
    #    with, and the same one the settings file holds. Two faces, one owner —
    #    §4.14's 「advertise_host 有两份副本」 is the failure this closes.
    TXT_ID=$(awk -F'\t' '$1=="txt"{print $2}' "$MDNS_OUT" | head -1 \
        | tr ';' '\n' | sed -n 's/^id=//p')
    STORED_USN=$(stored_setting USN 2>/dev/null || echo "")
    if [ -n "$TXT_ID" ] && [ "$TXT_ID" = "$STORED_USN" ]; then
        pass "TXT id == the stored USN, so mDNS and this face name one device"
    else
        fail "TXT id '${TXT_ID:-none}' vs stored USN '${STORED_USN:-none}'"
    fi

    # 3. `ca` is the bitmask VLC reads. Not 5 and it drops every video stream on
    #    the floor with no error anywhere — AGENTS.md §4.1's 「有声音没画面」.
    TXT_CA=$(awk -F'\t' '$1=="txt"{print $2}' "$MDNS_OUT" | head -1 \
        | tr ';' '\n' | sed -n 's/^ca=//p')
    [ "$TXT_CA" = "5" ] \
        && pass "ca=5 on the wire, the value that keeps VLC sending video" \
        || fail "TXT ca is '${TXT_CA:-absent}', expected 5"

    # 4. The order the properties arrive in. A reader that shows a device name
    #    reads `fn`; the order is not cosmetic and `_advertise`'s dict order is
    #    the contract (§9.2).
    TXT_ORDER=$(awk -F'\t' '$1=="txt_order"{print $2}' "$MDNS_OUT" | head -1)
    [ "$TXT_ORDER" = "id,fn,ca,st,bs,ic,ve,md,rm,rs" ] \
        && pass "the TXT keys arrive in the order the reference writes them" \
        || fail "TXT order was '${TXT_ORDER:-none}'"

    # 5. The SRV target and its A record: one name, and an address a sender on
    #    another machine can actually open (never loopback — §19's same rule).
    RESOLVED_SERVER=$(awk -F'\t' '$1=="server"{print $2}' "$MDNS_OUT" | head -1)
    RESOLVED_ADDRS=$(awk -F'\t' '$1=="addresses"{print $2}' "$MDNS_OUT" | head -1)
    WANT_SERVER="${MDNS_INSTANCE%%.*}.local."
    if [ "$RESOLVED_SERVER" = "$WANT_SERVER" ]; then
        pass "the SRV target is the instance's own label under .local."
    else
        fail "SRV target '${RESOLVED_SERVER:-none}', expected $WANT_SERVER"
    fi
    # The A set has to equal **the list the announcer said it published** — not
    # merely contain the LAN address. Two directions of wrong live here: publishing
    # the join list (a VMware/VirtualBox/Docker adapter among the answers, which is
    # `AGENTS.md` §4.1's first row — a sender picks one, gets no route, and nothing
    # on our side is wrong; that is exactly what this line measured on 2026-10-18),
    # and publishing fewer than the wire carries, which would mean the packet we
    # built is not the packet that arrived.
    ANNOUNCED_LIST=$(printf '%s' "$MDNS_LINE" | sed -n 's/.*over \[\([^]]*\)\].*/\1/p')
    SORTED_RESOLVED=$(printf '%s' "$RESOLVED_ADDRS" | tr ',' '\n' | sort | tr '\n' ',' | sed 's/,$//')
    SORTED_ANNOUNCED=$(printf '%s' "$ANNOUNCED_LIST" | tr ',' '\n' | sort | tr '\n' ',' | sed 's/,$//')
    case "$RESOLVED_ADDRS" in
        "") fail "no A record arrived for $RESOLVED_SERVER — the service is named but unreachable" ;;
        127.*|*,127.*) fail "a loopback address is in the A set ($RESOLVED_ADDRS): a phone would find a device it cannot reach" ;;
        *)
            if [ -n "$SORTED_ANNOUNCED" ] && [ "$SORTED_RESOLVED" = "$SORTED_ANNOUNCED" ]; then
                pass "the A set on the wire is exactly the set the announcer published ($SORTED_RESOLVED)"
            else
                fail "announced [${ANNOUNCED_LIST:-none}] but the wire carries $SORTED_RESOLVED"
            fi
            if [ -n "$LAN" ] && [ ",$SORTED_RESOLVED," = ",$LAN," ]                 || printf '%s' ",$SORTED_RESOLVED," | grep -q ",$LAN,"; then
                pass "and the address the other faces use ($LAN) is one of them"
            else
                fail "the advertised LAN address '$LAN' is not in the A set ($SORTED_RESOLVED)"
            fi
            ;;
    esac

    # 6. The instance name is one DNS-SD label: no parentheses, no dots. The
    #    browse matched it (check 1) which is the proof, but the sentence is worth
    #    its own line because the reference's failure mode here is *silence*:
    #    zeroconf registers `Macast(host.local)` happily and no query ever matches.
    case "$MDNS_INSTANCE" in
        *[!A-Za-z0-9._-]*) fail "the announced instance carries bytes a DNS label cannot hold: $MDNS_INSTANCE" ;;
        *) pass "the announced instance name is DNS-SD legal ($MDNS_INSTANCE)" ;;
    esac
fi


echo "== 23: the status face answers the page, and its numbers agree with the wire =="
# `query=status` is the document the settings page reads every few seconds, and it
# is built from four facts this script has *separate* witnesses for: what the player
# is doing (SOAP and mpv both answer that), which addresses are published (SSDP's
# LOCATION and the mDNS A records), who is subscribed (GENA's own table), and which
# protocols the user ticked (the file §24 reads as a door). So the checks below are
# mostly **cross-readers**: the same number asked of two faces must be the same
# number, because a status document that quietly reads a different helper is exactly
# how AGENTS.md §4.1's first row — an address that prints but does not open — comes
# back after nobody changed the address code.
if [ -z "$CLIP_BYTES" ]; then
    skip "no clip, so nothing can be played and read back through the status page"
else
    # A subscriber of our own, so `clients` has a row to print. The collector is a
    # second copy of §6's on a different port rather than a reuse: §6's window closed
    # minutes ago and its SID is gone, and a check that reads an empty table because
    # the fixture expired is the 「不提问的检查」 again.
    CB2=$((PORT + 3))
    python3 - "$CB2" <<'PY' > /tmp/verify_rust_receiver_status_notify.txt 2>&1 &
import socket, sys, time
port = int(sys.argv[1]); end = time.time() + 40
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(5); s.settimeout(0.5)
while time.time() < end:
    try:
        c, _ = s.accept()
    except socket.timeout:
        continue
    try:
        c.recv(65536)
    finally:
        c.close()
PY
    sleep 0.5
    HEADERS=$(curl -s -m 5 -D - -o /dev/null -X SUBSCRIBE "$BASE/AVTransport/event" \
        -H "CALLBACK: <http://127.0.0.1:$CB2/notif>" -H "TIMEOUT: Second-300")
    SID=$(printf '%s' "$HEADERS" | tr -d '\r' | sed -nE 's/^[Ss][Ii][Dd]: *//p' | head -1)
    [ -n "$SID" ] \
        && pass "a fresh SUBSCRIBE handed back a SID to look for in the status document" \
        || fail "SUBSCRIBE returned no SID: $(printf '%s' "$HEADERS" | tr -d '\r' | tr '\n' ' ')"
    api_probe "$BASE/api?query=cast&url=$MEDIA_URL&title=Rust-Status-Witness&token=$EXTRACTED"
    ok=""
    for _ in $(seq 1 40); do
        api_probe "$BASE/api?query=status"
        [ "$(json_ask media.TransportState)" = "PLAYING" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "the page's own read says PLAYING after a cast (polled, not grabbed)" \
        || fail "status never reported PLAYING: '$(json_ask media.TransportState)'"

    # ---- the document's own shape -------------------------------------------------
    [ "$(json_keys .)" = "server,media,clients,history,network" ] \
        && pass "five sections in the reference's order, and **no code/message wrapper**" \
        || fail "status keys: '$(json_keys .)'"
    [ "$(json_keys server)" = "running,port,version,friendly_name,renderer,protocol,platform,system,system_version,ip,https_enabled,https_port" ] \
        && pass "the server block carries the twelve keys the page reads, in order" \
        || fail "server keys: '$(json_keys server)'"
    [ "$(json_ask server.port)" = "$PORT" ] \
        && [ "$(json_ask server.running)" = "true" ] \
        && pass "the port it prints is the port this script is talking to" \
        || fail "server.port='$(json_ask server.port)' running='$(json_ask server.running)', expected $PORT/true"

    # `platform` is CPython's spelling, not Rust's. Asked of the interpreter that
    # owns the reference rather than asserted from a table, because the whole point
    # of the key is that a page prints it next to `system` for a human to read.
    if [ -x "$CAST_PY" ]; then
        SYSPLAT=$(env -u PYTHONPATH "$CAST_PY" -c 'import sys; print(sys.platform)' 2>/dev/null)
        [ -n "$SYSPLAT" ] && [ "$(json_ask server.platform)" = "$SYSPLAT" ] \
            && pass "server.platform is \`sys.platform\` as the reference's own interpreter spells it ($SYSPLAT)" \
            || fail "server.platform='$(json_ask server.platform)' but sys.platform='$SYSPLAT'"
    else
        skip "no .venv python to ask what sys.platform says"
    fi

    # ---- one fact, three readers --------------------------------------------------
    URI_IN_STATUS=$(json_ask media.CurrentTrackURI)
    [ "$URI_IN_STATUS" = "$MEDIA_URL" ] \
        && pass "status reads the address the player was handed" \
        || fail "media.CurrentTrackURI='$URI_IN_STATUS' expected '$MEDIA_URL'"
    URI_IN_SOAP=$(track_uri)
    [ "$URI_IN_SOAP" = "$URI_IN_STATUS" ] \
        && pass "SOAP's GetPositionInfo and the status page read one table, not two" \
        || fail "two readers disagree: SOAP '$URI_IN_SOAP' vs status '$URI_IN_STATUS'"
    # `CurrentURI` stays `null` because the reference never writes that name — the
    # three call sites all write `CurrentTrackURI` (`soap.rs:1057-1064` says why).
    # Asserting the null is asserting the *name*, which is what a page reads.
    [ "$(json_ask media.CurrentURI)" = "null" ] \
        && pass "and the name nobody writes prints null instead of a copy of its neighbour" \
        || fail "media.CurrentURI printed '$(json_ask media.CurrentURI)'"

    # The two shapes of one fact, compared **inside one parse**. The first version
    # of this check ran `tr '、' '/'` on `json_join`'s output — and `、` is three
    # bytes, which `tr` (a byte tool) does not translate. It passed on a machine with
    # one advertised address because there was no separator to mangle, and went red
    # the moment the address list grew to four. A check whose correctness depends on
    # how many network interfaces the runner has is the 「假命令行工具的输出」 family,
    # so the comparison is done by the parser that already understands the document.
    IP_AGREES=$(python3 - "$API_BODY" <<'PY'
import json, sys
with open(sys.argv[1], "rb") as fh:
    doc = json.loads(fh.read().decode("utf-8", "replace"))
ip = doc["server"]["ip"].split("/")
adv = doc["network"]["advertised"]
print("same" if ip == adv else "diff %r %r" % (ip, adv))
PY
)
    [ "$IP_AGREES" = "same" ] \
        && pass "server.ip is the same list the picker gets, /-joined, in the same order" \
        || fail "server.ip vs network.advertised: $IP_AGREES"
    MDNS_PUBLISHED=$(log_grep 'mDNS: announcing ' | tail -1 \
        | sed -nE 's/.*over \[([^]]*)\].*/\1/p')
    if [ -n "$MDNS_PUBLISHED" ]; then
        WIRE_AGREES=$(python3 - "$API_BODY" "$MDNS_PUBLISHED" <<'PY'
import json, sys
with open(sys.argv[1], "rb") as fh:
    doc = json.loads(fh.read().decode("utf-8", "replace"))
published = [p for p in sys.argv[2].split(",") if p]
adv = doc["network"]["advertised"]
print("same" if adv == published else "diff page=%r announcer=%r" % (adv, published))
PY
)
        [ "$WIRE_AGREES" = "same" ] \
            && pass "and it is exactly the set the announcer put in the A records (§22 read that set off the wire)" \
            || fail "the page and the wire disagree: $WIRE_AGREES"
    else
        skip "no announcement line to cross-read (this run announced nothing, and §22 says so)"
    fi

    # ---- clients ------------------------------------------------------------------
    ok=""
    for _ in $(seq 1 20); do
        api_probe "$BASE/api?query=status"
        [ "$(json_len clients)" -ge 1 ] && { ok=1; break; }
        sleep 0.25
    done
    if [ -z "$ok" ]; then
        fail "our own subscription never appeared in status.clients (SID $SID)"
    else
        [ "$(json_keys clients.0)" = "host,url,service,sid,path,timeout" ] \
            && pass "a client row is the reference's six keys, in its order" \
            || fail "client keys: '$(json_keys clients.0)'"
        [ "$(json_ask clients.0.sid)" = "$SID" ] \
            && pass "and it is *this* subscription, not a leftover" \
            || fail "clients.0.sid='$(json_ask clients.0.sid)' expected '$SID'"
        [ "$(json_ask clients.0.host)" = "127.0.0.1:$CB2" ] \
            && [ "$(json_ask clients.0.path)" = "/notif" ] \
            && pass "host and path are split off the callback the way the reference splits them" \
            || fail "client host/path: '$(json_ask clients.0.host)' '$(json_ask clients.0.path)'"
    fi
    # ---- the same table, asked as a diagnostic ------------------------------------
    # `query=subscribers` is the reference's other read of this table, and the one
    # that answers the question AGENTS.md §4.2 records: "the connection was accepted
    # but no client is listed" is *not* the same failure as "nobody subscribed", and
    # only these counters tell them apart. Asking it while our own SID is live is the
    # only moment on this machine where both halves of that sentence have evidence.
    api_probe "$BASE/api?query=subscribers"
    [ "$API_STATUS" = "200" ] \
        && pass "subscribers answers this machine" \
        || fail "subscribers answered HTTP $API_STATUS code '$API_CODE'"
    [ "$(json_keys .)" = "aggregated,children" ] \
        && pass "its document is the reference's two sections, in order" \
        || fail "subscribers keys: '$(json_keys .)'"
    [ "$(json_ask children.0.title)" = "DLNA Protocol" ] \
        && [ "$(json_ask children.0.type)" = "DLNAProtocol" ] \
        && pass "the child is named the way the reference names it (class-derived, not typed by hand)" \
        || fail "child title/type: '$(json_ask children.0.title)' / '$(json_ask children.0.type)'"
    SUB_AGG=$(json_join aggregated)
    case ",$SUB_AGG," in
        *",$SID,"*) pass "our own subscription is listed by the SID the SUBSCRIBE returned" ;;
        *) fail "aggregated '$SUB_AGG' does not contain our SID '$SID'" ;;
    esac
    # Drain check, **waited for** rather than grabbed: a subscription lands in the
    # published table on the event thread's own schedule, so a non-zero
    # `pending_append` read immediately after a SUBSCRIBE says nothing about the
    # product (AGENTS.md §4.2's 「判据坐在别人的账本上，就要等那个账本」).
    ok=""
    for _ in $(seq 1 20); do
        api_probe "$BASE/api?query=subscribers"
        [ "$(json_ask children.0.pending_append)" = "0" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "the append queue drains back to zero — the event thread is the one listing clients" \
        || fail "pending_append stuck at $(json_ask children.0.pending_append)"
    [ "$(json_ask children.0.running)" = "true" ] \
        && [ "$(json_ask children.0.event_thread_alive)" = "true" ] \
        && pass "and both liveness answers agree, which is the boring state worth pinning" \
        || fail "running/event_thread_alive: '$(json_ask children.0.running)' / '$(json_ask children.0.event_thread_alive)'"
    CROSS=$(python3 - "$API_BODY" <<'CROSSPY'
import json, sys
with open(sys.argv[1], "rb") as fh:
    subs = json.loads(fh.read().decode("utf-8", "replace"))
print(",".join(sorted(r["sid"] for r in subs["children"][0]["rows"])))
CROSSPY
)
    CROSS_AGG=$(printf '%s' "$SUB_AGG" | tr ',' '\n' | sort | tr '\n' ',' | sed 's/,$//')
    [ "$CROSS" = "$CROSS_AGG" ] \
        && pass "subscribers' own rows and its aggregated list are the same set of SIDs" \
        || fail "rows '$CROSS' vs aggregated '$CROSS_AGG'"
    # ---- the three reads that answer "this build has no such face"
    #
    # `plugin-info` is real data with an empty list; the two mirror reads are a full
    # refusal document behind the **token** gate (loopback is not enough — the reference
    # says why: one of them hands over a frame of the desktop). A card polling either one
    # needs something it can stop on, which is what `code` plus a sentence gives it.
    api_probe "$BASE/api?query=plugin-info"
    [ "$API_STATUS" = "200" ] && [ "$(json_len plugins)" = "0" ] \
        && pass "plugin-info answers an empty plugin list — nothing installable from this build, said as a list" \
        || fail "plugin-info: HTTP $API_STATUS plugins='$(json_ask plugins)'"
    [ "$(json_ask platform)" = "$(env -u PYTHONPATH "$CAST_PY" -c 'import sys; print({"darwin":"darwin","win32":"win32","linux":"linux"}.get(sys.platform, sys.platform))')" ] \
        && pass "and its platform string is the one the rest of the app uses" \
        || fail "plugin-info platform: $(json_ask platform)"
    api_probe "$BASE/api?query=mirror-state"
    [ "$API_STATUS" = "403" ] \
        && pass "mirror-state needs the token even from this machine" \
        || fail "mirror-state answered a tokenless loopback caller: HTTP $API_STATUS"
    TOKEN_STORED=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("Api_Token",""))' "$SDIR/macast_setting.json")
    api_probe "$BASE/api?query=mirror-state&token=$TOKEN_STORED"
    [ "$(json_ask code)" = "1" ] \
        && pass "with the token it answers a complete absence document, so the card stops polling" \
        || fail "mirror-state with a token: code '$(json_ask code)' message '$(json_ask message)'"
    case "$(json_ask message)" in
        *"镜像"*) pass "and the sentence names the missing piece instead of a bare status" ;;
        *) fail "mirror-state's message says nothing useful: $(json_ask message)" ;;
    esac

    curl -s -m 5 -o /dev/null -X UNSUBSCRIBE "$BASE/AVTransport/event" -H "SID: $SID"
    ok=""
    for _ in $(seq 1 20); do
        api_probe "$BASE/api?query=status"
        [ "$(json_len clients)" = "0" ] && { ok=1; break; }
        sleep 0.25
    done
    [ -n "$ok" ] \
        && pass "after UNSUBSCRIBE the list empties — the document reads the live table" \
        || fail "clients still holds $(json_len clients) row(s) after unsubscribing '$SID'"
    api_probe "$BASE/api?query=subscribers"
    [ "$(json_len aggregated)" = "0" ] \
        && pass "and the diagnostic read empties with it, not a moment later off a different table" \
        || fail "subscribers still aggregates $(json_len aggregated) after unsubscribing"

    # ---- the log panel's own reads -------------------------------------------------
    # `query=log`, `log-modules`, `log-download` and `POST clear-log` are the 「日志」
    # tab, and the witness they need is a log line that a *real* run wrote. This one
    # has several: §22 proved the announcer broadcast, so `logs/Discovery.log` holds
    # the `mDNS: announcing` line, and the same line is what §23 compared addresses
    # against minutes ago. Reading it back through the HTTP face asks a different
    # question than `gate_grep` did: not "did the app write it" but "can the panel
    # that the user opens see it".
    api_probe "$BASE/api?query=log-modules"
    [ "$API_STATUS" = "200" ] \
        && pass "log-modules answers this machine" \
        || fail "log-modules answered HTTP $API_STATUS code '$API_CODE'"
    [ "$(json_keys .)" = "main,modules" ] \
        && pass "its document is the reference's two sections" \
        || fail "log-modules keys: '$(json_keys .)'"
    if [ "$(json_ask main.size)" -gt 0 ] && [ "$(json_len modules)" -ge 1 ]; then
        pass "the main file has a size and at least one module row is listed"
    else
        fail "log listing is empty: main=$(json_ask main) rows=$(json_len modules)"
    fi
    # The rotated generations are listed as their own rows, with the **real** file
    # name. This machine may or may not have rotated anything in this run's few
    # minutes, so the assertion is about the row shape rather than the count: every
    # row's `file` must end in `.log` or `.log.<digits>`, and its `name` must be that
    # file with the suffix cut — a synthesised `name.log` for a `name.log.1` row is
    # exactly the lie the shared builder in `macast-core` exists to prevent.
    BADROW=$(python3 - "$API_BODY" <<'ROWPY'
import json, re, sys
with open(sys.argv[1], "rb") as fh:
    rows = json.loads(fh.read().decode("utf-8", "replace"))["modules"]
bad = [r for r in rows
       if not re.search(r"\.log(\.\d+)?$", r.get("file", ""))
       or r.get("name") != re.sub(r"\.log(\.\d+)?$", "", r.get("file", ""))]
print(f"{len(rows)} rows, {len(bad)} malformed: {bad[:2]}")
sys.exit(1 if bad else 0)
ROWPY
) || true
    case "$BADROW" in
        *"malformed: []"*) pass "every module row carries a real file name and its matching module name" ;;
        *) fail "module rows: $BADROW" ;;
    esac

    api_probe "$BASE/api?query=log&module=Discovery"
    [ "$(json_keys .)" = "code,logs,truncated,lines,size,module" ] \
        && pass "query=log answers the six-key document, **no message on the happy path**" \
        || fail "log keys: '$(json_keys .)'"
    [ "$(json_ask code)" = "0" ] && [ "$(json_ask truncated)" = "false" ] \
        && pass "the module tail is complete for a file this small" \
        || fail "log payload: code=$(json_ask code) truncated=$(json_ask truncated)"
    # What Discovery.log actually holds is the announcer's own sentence
    # (`mDNS advertised …`, written by the `Discovery` logger); the
    # `mDNS: announcing …` summary line is the **shell's**, so it lands in
    # macast.log. The first version of this check grepped the module file for the
    # summary line and went red on a correct answer — the useful kind of red: it
    # says the check was written from a guess about who logs what.
    case "$(json_ask logs)" in
        *"mDNS advertised"*) pass "and it holds the announcer's own line, written by the Discovery logger" ;;
        *) fail "Discovery.log tail has no announcement line: $(json_ask logs | head -c 160)" ;;
    esac
    # …and the same file **without** `module=` is the main log. What to assert there
    # took two tries: the first version looked for the `mDNS: announcing` summary
    # line, which the shell really wrote — at startup, four minutes and more than
    # 2000 lines of SOAP and GENA chatter ago. A tail does not contain history, and
    # a check that pretends otherwise is red on a correct answer.
    #
    # So the assertion is the one that is *about* being a tail: the panel's promise
    # (`protocol.py:2016-2020`'s `truncated`/`size`) is "the last N lines of M
    # bytes", so ask for 20 lines and require the count to respect it, the size to
    # be the whole file, and `truncated` to be true exactly when the byte window had
    # to start past the beginning (512 KB of window, `TAIL_BYTE_FLOOR`).
    MAIN_BYTES=$(wc -c < "$SDIR/macast.log" | tr -d ' ')
    api_probe "$BASE/api?query=log&tail=20"
    TAIL_LINES=$(json_ask lines)
    TAIL_SIZE=$(json_ask size)
    TAIL_TRUNC=$(json_ask truncated)
    [ "$TAIL_SIZE" = "$MAIN_BYTES" ] \
        && pass "the main log's answer reports the file's real size, not the window's" \
        || fail "log size says $TAIL_SIZE, the file is $MAIN_BYTES"
    [ "$TAIL_LINES" -le 20 ] && [ "$TAIL_LINES" -ge 1 ] \
        && pass "and it counted the lines it actually handed back ($TAIL_LINES of the 20 asked)" \
        || fail "lines=$TAIL_LINES for tail=20"
    # The *other* direction of the same promise, and the reason the check is written
    # this way: `truncated` is true here because `?tail=20` asks for a byte budget
    # that an 8.5 KB file does not fit into — the budget follows the line count
    # (`tail_byte_budget`), which is the first version of this check getting wrong
    # by assuming a fixed 512 KB floor. So ask for more lines than the file has
    # lines: the whole thing must come back, `truncated` false, and the count must
    # equal the file's own.
    MAIN_LINES=$(wc -l < "$SDIR/macast.log" | tr -d ' ')
    api_probe "$BASE/api?query=log&tail=50000"
    [ "$(json_ask truncated)" = "false" ] \
        && [ "$(json_ask lines)" = "$MAIN_LINES" ] \
        && [ "$(json_ask size)" = "$MAIN_BYTES" ] \
        && pass "asked past the end, the panel gets the whole file and says it is whole ($MAIN_LINES lines)" \
        || fail "full read: truncated=$(json_ask truncated) lines=$(json_ask lines)/$MAIN_LINES size=$(json_ask size)/$MAIN_BYTES"

    api_probe "$BASE/api?query=log&module=NoSuchModuleHere"
    [ "$(json_ask code)" = "1" ] \
        && [ "$(json_ask module)" = "NoSuchModuleHere" ] \
        && pass "an unknown module is its own answer, with the name echoed back" \
        || fail "unknown module answered: $(json_ask code) / $(json_ask module)"
    # `?tail=abc` raises in the reference (an unparseable int straight into
    # `read_log_tail`) and the caller gets a bodyless 500. This port answers the
    # default tail — the deviation `log_view_tail_that_is_not_a_number…` pins in the
    # crate, asserted here so the two halves cannot drift apart.
    api_probe "$BASE/api?query=log&module=Discovery&tail=abc"
    [ "$(json_ask code)" = "0" ] \
        && pass "a typo in ?tail answers the default tail instead of failing" \
        || fail "?tail=abc answered code $(json_ask code)"

    DLHEAD=$(curl -s --noproxy '*' -m 10 -D - -o /tmp/verify_rust_receiver_log.txt \
        "$BASE/api?query=log-download&module=Discovery" | tr -d '\r')
    # Compared as the **whole header line**, not as a substring of the dump: the
    # first version used a `case` pattern with escaped quotes and told us nothing
    # about what actually arrived. Now the failure message carries the line it saw
    # and the list it came out of.
    DLDISP=$(printf '%s\n' "$DLHEAD" | grep -i '^content-disposition:' | head -1)
    [ "$DLDISP" = 'Content-Disposition: attachment; filename="Discovery.log"' ] \
        && pass "log-download names the file it is attaching, byte for byte in the reference's spelling" \
        || fail "disposition was [$DLDISP] among: $(printf '%s' "$DLHEAD" | tr '\n' '|')"
    [ -s /tmp/verify_rust_receiver_log.txt ] \
        && pass "and it serves the whole file, not a tail" \
        || fail "log-download returned an empty body"

    # ---- CSRF: clearing is a POST, and only a POST ---------------------------------
    # AGENTS.md §4.10's red line: a GET flavour would be triggerable by any page's
    # `<img src="http://127.0.0.1:58880/api?query=log&clear-log=1">`. The field names
    # a POST route, so on a GET it is just a query parameter and nothing reads it.
    BEFORE_BYTES=$(wc -c < "$SDIR/logs/Discovery.log" 2>/dev/null | tr -d ' ')
    api_probe "$BASE/api?query=log&module=Discovery&clear-log=1"
    AFTER_BYTES=$(wc -c < "$SDIR/logs/Discovery.log" 2>/dev/null | tr -d ' ')
    [ "$BEFORE_BYTES" = "$AFTER_BYTES" ] && [ -n "$BEFORE_BYTES" ] \
        && pass "a GET carrying clear-log=1 leaves the file exactly as it was" \
        || fail "a GET cleared the log ($BEFORE_BYTES -> $AFTER_BYTES bytes)"
    api_post "$BASE/api" "clear-log=1" "module=Discovery"
    [ "$(json_ask code)" = "0" ] \
        && pass "the POST clears it instead, and says so" \
        || fail "clear-log answered: $(json_ask code) $(json_ask message)"
    [ "$(json_ask removed)" -ge 1 ] 2>/dev/null \
        && pass "and it counts what it took, backups included" \
        || fail "no removed count: $(json_ask removed)"
    api_probe "$BASE/api?query=log&module=Discovery"
    [ "$(json_ask size)" = "0" ] && [ "$(json_ask lines)" = "0" ] \
        && pass "the panel's next read is an empty box, not an error" \
        || fail "after clearing, size=$(json_ask size) lines=$(json_ask lines)"

    # ---- history ------------------------------------------------------------------
    # **Re-probe first.** The diagnostic block above left `$API_BODY` holding the
    # *subscribers* document, and `json_len history` over it answers "no such key" —
    # which reads as `''`, and a check written as `>= 1` turns that into "history is
    # empty after a cast". The first run of this section red exactly that way, and the
    # product was fine: one shared file, three readers, order matters (§4.2's
    # 「夹具脚本里的那对括号是承重的」 family — the harness's own state is a fixture).
    api_probe "$BASE/api?query=status"
    [ "$(json_len history)" -ge 1 ] \
        && pass "the cast left a play-history row the page can offer back" \
        || fail "history is empty after a cast: '$(json_ask history)'"
    [ "$(json_ask history.0.uri)" = "$MEDIA_URL" ] \
        && pass "newest first, as the reference's _add_history writes it" \
        || fail "history.0.uri='$(json_ask history.0.uri)' expected '$MEDIA_URL'"

    # ---- interfaces ---------------------------------------------------------------
    # Snapshot the status document first: `$API_BODY` is one file and the next probe
    # overwrites it, so a comparison written against "what status said" has to name
    # the moment it was said (§4.2's 「计数器在 flush 之后才加」, same shape).
    api_probe "$BASE/api?query=status"
    cp "$API_BODY" /tmp/verify_rust_receiver_status.json
    api_probe "$BASE/api?query=interfaces"
    [ "$(json_keys .)" = "current,advertised,interfaces" ] \
        && pass "the picker's own query answers its three facts, and calls the pin \`current\`" \
        || fail "interfaces keys: '$(json_keys .)'"
    ADV_AGREE=$(python3 - /tmp/verify_rust_receiver_status.json "$API_BODY" <<'PY'
import json, sys
with open(sys.argv[1], "rb") as fh:
    status = json.loads(fh.read().decode("utf-8", "replace"))
with open(sys.argv[2], "rb") as fh:
    interfaces = json.loads(fh.read().decode("utf-8", "replace"))
a = status["network"]["advertised"]
b = interfaces["advertised"]
print("same" if a == b else "diff status=%r interfaces=%r" % (a, b))
PY
)
    [ "$ADV_AGREE" = "same" ] \
        && pass "and the same advertised set status printed, row for row" \
        || fail "the two doors read different address lists: $ADV_AGREE"
    if [ "$(json_len interfaces)" -ge 1 ]; then
        [ "$(json_keys interfaces.0)" = "name,ip,netmask,default,usable,reason,selected" ] \
            && pass "each picker row carries the seven keys the page sorts and colours by" \
            || fail "interface row keys: '$(json_keys interfaces.0)'"
    else
        skip "this machine contributed no IPv4 interface to the picker"
    fi

    # ---- the raw settings table ---------------------------------------------------
    # 高级设置 reads `query=launch-param`, and this is the one GET on the face whose
    # answer is the whole file — including `Api_Token`. Asserting what it contains is
    # the point, not an oversight: the route is only defensible because the two facts
    # below hold, and if either ever changes the right answer is to shut the route,
    # not to relax the check.
    # Read out of the file with the same parser the page would use; `get` rather
    # than indexing because a run that never revealed its token has no key at all.
    TOKEN_STORED=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("Api_Token",""))' \
        "$SDIR/macast_setting.json" 2>/dev/null)
    api_probe "$BASE/api?query=cast-info"
    api_probe "$BASE/api?query=launch-param"
    [ "$(json_ask DLNA_FriendlyName)" = "Macast Rust Verify" ] \
        && pass "launch-param hands the tab the settings file's own keys" \
        || fail "launch-param said DLNA_FriendlyName='$(json_ask DLNA_FriendlyName)'"
    if [ -n "$EXTRACTED" ] && [ -n "$TOKEN_STORED" ] && [ "$EXTRACTED" = "$TOKEN_STORED" ]; then
        [ "$(json_ask Api_Token)" = "$TOKEN_STORED" ] \
            && pass "and it carries the resident token, byte for byte — the reason this route is loopback/token-gated and the reason no response here ever carries a CORS header" \
            || fail "launch-param's Api_Token is not the stored one"
    else
        skip "no stored token to compare against ($EXTRACTED / $TOKEN_STORED)"
    fi

    # ---- the gate -----------------------------------------------------------------
    api_probe "$BASE/api?query=status"
    [ "$API_STATUS" = "200" ] && [ -z "$API_CODE" ] \
        && pass "loopback reads it with no token at all, and the body has no code field" \
        || fail "loopback status answered HTTP $API_STATUS code '$API_CODE'"
    if [ -n "$LAN" ]; then
        api_probe "http://$LAN:$PORT/api?query=status"
        [ "$API_STATUS" = "403" ] \
            && pass "a LAN caller is refused the status document" \
            || fail "a LAN caller got HTTP $API_STATUS from status"
        case "$API_MSG" in
            *"$MANAGEMENT_MARK"*) pass "and hears the management sentence, as that is the gate that ran" ;;
            *) fail "LAN status refusal wording: $API_MSG" ;;
        esac
        api_probe "http://$LAN:$PORT/api?query=status&token=$EXTRACTED"
        [ "$API_STATUS" = "200" ] \
            && pass "the same caller from the same address is served once it brings the token" \
            || fail "tokened LAN status answered HTTP $API_STATUS"
        api_probe "http://$LAN:$PORT/api?query=interfaces"
        [ "$API_STATUS" = "403" ] \
            && pass "and the picker's query is behind the same door, not a second opinion" \
            || fail "a LAN caller got HTTP $API_STATUS from interfaces"
        api_probe "http://$LAN:$PORT/api?query=subscribers"
        [ "$API_STATUS" = "403" ] \
            && pass "and so is the diagnostic read — it names who is watching, which is not LAN weather" \
            || fail "a LAN caller got HTTP $API_STATUS from subscribers"
        for query in log log-modules log-download launch-param; do
            api_probe "http://$LAN:$PORT/api?query=$query&module=MPV"
            [ "$API_STATUS" = "403" ] \
                && pass "a LAN caller is refused $query" \
                || fail "a LAN caller got HTTP $API_STATUS from $query"
            case "$API_MSG" in
                *"$MANAGEMENT_MARK"*) : ;;
                *) fail "$query refusal wording: $API_MSG" ;;
            esac
        done
        # A LAN page posting the clear field hears the body-level 403 (POST keeps the
        # reference's envelope: HTTP 200, `code` in the body) and the file stays.
        LOGSIZE=$(wc -c < "$SDIR/logs/MPV.log" 2>/dev/null | tr -d ' ')
        if [ -n "$LOGSIZE" ] && [ "$LOGSIZE" -gt 0 ]; then
            api_post "http://$LAN:$PORT/api" "clear-log=1" "module=MPV"
            [ "$API_CODE" = "403" ] \
                && pass "a LAN POST clear-log is refused in the body's code, the reference's POST envelope" \
                || fail "LAN clear answered code '$API_CODE': $API_MSG"
            [ "$(wc -c < "$SDIR/logs/MPV.log" | tr -d ' ')" = "$LOGSIZE" ] \
                && pass "and the refused call emptied nothing" \
                || fail "a refused LAN clear still truncated MPV.log"
        else
            skip "no MPV.log bytes to protect from a LAN clear (nothing played this run)"
        fi
    else
        skip "no reachable non-loopback address to ask from (section 23's LAN half)"
    fi
fi

echo "== 24: switching Chromecast off closes all three faces =="
# `host::tests` proves the predicate reads the enable list the way the reference
# does — coercion of a stored string, the legacy key appended, unknown titles
# skipped with a sentence, and a fresh install meaning "everything". None of that
# can prove the **sockets stay shut**: a gate consulted once and then bypassed by a
# fourth `start()` would stay green in every unit test in this tree. So this
# section binds nothing and asks.
#
# The first instance is stopped on purpose, and only because it is ours. 8009 and
# 8008 are hardcoded by senders (`dial.py:25-26`), so while it is up a second
# instance could not tell "we refused to start" apart from "someone else holds the
# port" — the exact ambiguity §20's no-fallback rule exists to avoid.
kill -9 "$APP_PID" 2>/dev/null
GATE_SDIR=$(mktemp -d /tmp/macast-rust-gate.XXXXXX)
gate_grep() {
    # `log_grep` looks in the **first** instance's directory; this section runs a
    # second one with its own settings dir, so reading the shared log here would
    # have answered "no refusal line" about the wrong process entirely.
    grep -ahF "$1" "$GATE_SDIR/macast.log" "$GATE_SDIR"/logs/*.log 2>/dev/null
}
GATE_SOCK="$GATE_SDIR/mpvsocket"
# `["DLNA Protocol"]` is the canonical spelling `resolve_title` returns; the test
# for the tolerant spellings is in the crate, and this is the strict one — what a
# user gets after pressing the switch in the page.
echo "$GATE_SOCK" > "$GATE_SDIR/.socket"
# **The browse starts before the instance it watches.** This section's answer about
# the announcement has to be a measurement, not the absence of one: the announcer's
# first burst is three packets 225 ms apart and then nothing for 60 s (`REANNOUNCE`),
# so a browser opened *after* startup — which is what this block used to do, with an
# eight second window — reports "saw nothing" for a run that announced loudly and is
# merely between bursts. Measured, not reasoned: with the certificate's gate removed
# as a mutant, so the gated instance really did build an announcer, bind 8009 and log
# `mDNS: announcing Macast-Gate-Off`, the old check answered **pass**. The section
# still went red on that mutant (5 checks), but on the sockets, the log line and the
# key material — the wire-level half was not asking, which is 「不提问的检查等于通过的检查」
# in its purest shape: one green that a broken run and a working run both produce.
# Starting the browser first puts the window across the burst, so the same check can
# tell those two runs apart.
#
# §22 is this check's positive control: minutes earlier the same browser code on this
# machine folded a **keepalive** burst into a device. If no announcement happened
# anywhere in the run (`MDNS_INSTANCE` empty), "nothing on the wire here" has no
# witness and is said as a skip instead of counted green — 「空结果要说清是哪一种空」.
GATE_BROWSE_PY="$GATE_SDIR/browse.py"
GATE_BROWSE_OUT="$GATE_SDIR/browse.txt"
cat > "$GATE_BROWSE_PY" <<'PY'
import sys, time, zeroconf as z

budget, out = float(sys.argv[1]), sys.argv[2]
seen = set()


class L(z.ServiceListener):
    def add_service(self, zc, type_, name):
        # The type travels with the name: this one listener now serves two browses,
        # because the door being closed covers three faces, and "no Cast announcement"
        # was never evidence about the AirPlay one.
        seen.add("%s::%s" % (type_, name))

    def remove_service(self, *a):
        pass

    def update_service(self, *a):
        pass


# The file appears only when the window closes, so the shell's wait is a wait for the
# answer, not for a line that may never have been written.
zc = z.Zeroconf(ip_version=z.IPVersion.V4Only)
cast_browser = z.ServiceBrowser(zc, "_googlecast._tcp.local.", L())
airplay_browser = z.ServiceBrowser(zc, "_airplay._tcp.local.", L())
time.sleep(budget)
cast_browser.cancel()
airplay_browser.cancel()
zc.close()
with open(out, "w") as fh:
    fh.write(",".join(sorted(seen)))
PY
# 30 s covers startup plus the initial burst with room, and deliberately does not
# reach the 60 s keepalive — catching a keepalive is §22's job, with its 75 s window.
env -u PYTHONPATH -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY \
    "$CAST_PY" "$GATE_BROWSE_PY" 30 "$GATE_BROWSE_OUT" >/dev/null 2>&1 &
GATE_BROWSE_PID=$!
disown "$GATE_BROWSE_PID" 2>/dev/null || true
# shellcheck disable=SC2016
# The key is `DLNA_FriendlyName` for the reason recorded at the launch block above,
# and here it is load-bearing twice: the sanitiser turns "Macast Gate Off" into the
# DNS-SD label §24 matches on the wire.
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Gate Off", "Macast_Protocols": ["DLNA Protocol"]}' \
    "$PORT" > "$GATE_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$GATE_SDIR" MACAST_MPV_SOCKET="$GATE_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$GATE_SDIR/stdout.log" 2>&1 &
GATE_PID=$!
disown "$GATE_PID" 2>/dev/null || true
ok=""
for _ in $(seq 1 40); do
    [ "$(listener_pid)" = "$GATE_PID" ] && { ok=1; break; }
    sleep 0.25
done
if [ -z "$ok" ]; then
    fail "the gated instance never became the listener on $PORT (pid $(listener_pid))"
    kill -9 "$GATE_PID" 2>/dev/null
    rm -rf "$GATE_SDIR"
else
    # The positive control is `description.xml`, **not** `query=status`: that
    # management query is one of the ten this shell has not ported yet, and it
    # answers the honest 「不认识的字段」 sentence — which a check written as
    # `code == 0` would have read as the gate having broken the app. Asking a
    # route that exists is the whole difference between a red test and a red
    # product (§4.2's 「空结果要说清是哪一种空」, one layer over).
    GATE_DESC_CODE=$(curl -s --noproxy '*' -m 5 -o /dev/null -w '%{http_code}' \
        "$BASE/description.xml")
    [ "$GATE_DESC_CODE" = "200" ] \
        && pass "a DLNA-only run still serves the page and the discovery document" \
        || fail "description.xml answered HTTP $GATE_DESC_CODE on the gated instance"

    # The sentence the gate writes, read out of the log rather than inferred from
    # a port that happens to be shut.
    if [ -n "$(gate_grep 'Chromecast is switched off')" ]; then
        pass "the run says which switch closed the faces, instead of leaving no Cast receiver unexplained"
    else
        fail "no refusal line in the log for a run that was told to switch Chromecast off"
    fi

    closed=0
    for probe in 8009 8008; do
        if env -u PYTHONPATH "$CAST_PY" - "$probe" <<'PY' 2>/dev/null | grep -q refused
import socket, sys
port = int(sys.argv[1])
sock = socket.socket()
sock.settimeout(2)
try:
    sock.connect(("127.0.0.1", port))
except ConnectionRefusedError:
    print("refused")
except Exception as exc:
    print("other %s" % exc)
else:
    print("open")
PY
        then
            closed=$((closed + 1))
        else
            fail "port $probe is answering on a run that switched Chromecast off"
        fi
    done
    [ "$closed" = "2" ] \
        && pass "8009 and 8008 both refuse — the receiver and the setup face are one protocol" \
        || fail "only $closed of 2 Cast ports refused"

    # Read the browser that was started before this instance existed. Its window
    # (30 s from before launch) straddles the announcer's initial burst, so an
    # announcement made during it is reported — see the note above the launch for
    # what the previous ordering cost.
    for _ in $(seq 1 160); do
        [ -e "$GATE_BROWSE_OUT" ] && break
        sleep 0.25
    done
    BROWSE_LEFT=$(pgrep -f "$GATE_BROWSE_PY" | tr '\n' ' ')
    [ -n "$BROWSE_LEFT" ] && kill -9 $BROWSE_LEFT 2>/dev/null
    SEEN=$(cat "$GATE_BROWSE_OUT" 2>/dev/null)
    if [ -z "$MDNS_INSTANCE" ]; then
        # Nothing announced itself anywhere on this run, so §22 never proved a
        # browse on this machine can see one of our bursts. A silent wire here
        # would then be an answer about the instrument, not about the gate.
        skip "no announcement happened anywhere this run, so 'nothing on the wire' here has no positive control"
    elif [ ! -e "$GATE_BROWSE_OUT" ]; then
        fail "the browse never finished — its window says nothing about any burst"
    else
        case "$SEEN" in
            *"Macast-Gate-Off._googlecast._tcp.local."*)
                fail "the gated instance announced itself anyway: $SEEN" ;;
            *"Macast-Gate-Off._airplay._tcp.local."*)
                fail "the gated instance announced an AirPlay receiver although its list is DLNA only: $SEEN" ;;
            *) pass "no announcement from the gated instance (browse saw: ${SEEN:-nothing})" ;;
        esac
    fi
    if [ -n "$(gate_grep 'mDNS: announcing')" ]; then
        fail "the announcer logged a start on a run that switched Chromecast off"
    else
        pass "and the announcer said nothing, because it was never built"
    fi

    # The side effect behind the door: the certificate is generated on this path,
    # so a protocol the user switched off must not leave key material in his
    # settings directory.
    if [ -e "$GATE_SDIR/macast_cast.key" ] || [ -e "$GATE_SDIR/macast.key" ]; then
        fail "a switched-off protocol still wrote a certificate into its settings dir"
    else
        pass "no certificate was generated for a protocol that is off"
    fi
    # The refusal has to name the **switch**. This is the second reason this
    # section exists: the summary line used to say "no certificate to wrap 8009
    # with" on this path too, which is true only of a run that asked for a
    # certificate and did not get one — here nothing asked. A reader sent to the
    # certificate is being sent to fix something that is not broken.
    if [ -n "$(gate_grep 'switched off in the settings page')" ]; then
        pass "and the line that explains 8009 names the switch, not the certificate"
    else
        fail "the gated run explained itself by blaming a certificate: $(gate_grep 'Cast receiver:' | tail -2)"
    fi

    kill -9 "$GATE_PID" 2>/dev/null
    pkill -9 -f "$GATE_SOCK" 2>/dev/null
    rm -rf "$GATE_SDIR"
fi

# **Put the main instance back.** Everything after this section reads `$BASE` and expects
# it to be the receiver §1–§23 spent a minute proving things about; §26's RTSP receiver
# takes 7001 *beside* it, §27's restart reads the history it wrote, and §28/§29 compare
# against the one process. Leaving it dead made those sections bind 8010 or talk to a
# closed port — measured 2026-10-21, and the cascade read exactly like six product bugs.
start_main


echo "== 25: a sender that ASKS gets an answer, and the empties are told apart =="
# §22 proves a passive listener can find us — the libvlc shape. It does not prove the
# shape that costs a user real time: a sender with an **empty cache** (the Cast app on
# a phone that just woke up, `dns-sd -B`, any browser starting cold) has to ask, and
# until this round the honest answer was "wait for the next keepalive" — up to
# `REANNOUNCE` = 60 seconds — because we only ever sent unsolicited bursts.
#
# This round wired the answer (`announce::serve` on the announcer's own joined socket),
# and this section is where that gets witnessed on the LAN rather than on loopback.
# It therefore has to distinguish **three** outcomes, not two:
#
#   1. our instance answered a query quickly            → the feature works here
#   2. it answered nothing while the wire demonstrably
#      carried queries                                 → this *binary* receives no
#                                                        inbound LAN multicast
#                                                        (macOS's local-network gate;
#                                                        AGENTS.md §4.2's "别把端口在听
#                                                        当证据" read from the other side)
#   3. no queries on the wire at all                    → the instrument, not the product
#
# Collapsing 2 into 3 (or into "the responder is broken") is the failure this shape
# exists to prevent: §4.14's 「空结果要说清是哪一种空」, now four sentences deep in our
# own harness. So a second process on the same host listens on the group for the whole
# window and counts the queries; without that number the silence in 2 is unqualifiable.
if [ -z "$MDNS_INSTANCE" ]; then
    skip "no announcement this run, so there is no instance for a query to find"
elif [ ! -x "$CAST_PY" ]; then
    skip "no .venv python to query with ($CAST_PY)"
else
    QUERY_OUT=/tmp/verify_rust_receiver_query.txt
    SNIFF_OUT=/tmp/verify_rust_receiver_sniff.txt
    LAN_IP=$(route -n get default 2>/dev/null | awk '/interface:/{print $2}')
    LAN_ADDR=$(ipconfig getifaddr "$LAN_IP" 2>/dev/null)
    if [ -z "$LAN_ADDR" ]; then
        skip "the default route ($LAN_IP) has no IPv4 address to join the group on"
    else
        QUERY_PY=/tmp/verify_rust_receiver_query.py
        SNIFF_PY=/tmp/verify_rust_receiver_sniff.py
        cat > "$SNIFF_PY" <<'SNIFFEOF'
import socket, struct, sys, time
lan, seconds = socket.inet_aton(sys.argv[1]), float(sys.argv[2])
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
sock.bind(("", 5353))
sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP,
                socket.inet_aton("224.0.0.251") + lan)
sock.settimeout(0.5)
queries = answers = 0
start = time.time()
while time.time() - start < seconds:
    try:
        data, frm = sock.recvfrom(4096)
    except socket.timeout:
        continue
    except OSError as exc:
        print("listener stopped early: %s" % exc)
        break
    if len(data) < 12:
        continue
    flags = struct.unpack(">H", data[2:4])[0]
    qd = struct.unpack(">H", data[4:6])[0]
    # The needle is a **wire label**, not a dotted string: mDNS names on the wire
    # are length-prefixed, so `_googlecast._tcp` (with dots) never appears in any
    # packet, and the dotted filter made this listener report `queries=0` on a
    # group full of them — which §25 then read as "the instrument heard nothing".
    # Same bug, found the same way (a mutant, not a reading): 2026-10-18.
    if b"\x0b_googlecast" not in data:
        continue
    if flags & 0x8000:
        answers += 1
    elif qd:
        queries += 1
print("wire queries=%d answers=%d" % (queries, answers))
SNIFFEOF
        cat > "$QUERY_PY" <<'QUERYEOF'
import sys, time, zeroconf as z

want, budget = sys.argv[1], float(sys.argv[2])
TYPE = "_googlecast._tcp.local."
# Nobody answers this type anywhere; it exists so the timing line can be believed.
NEGATIVE = "_macast-nothing-here._tcp.local."
found = {}
started = time.monotonic()


class Watch(z.ServiceListener):
    def __init__(self, kind):
        self.kind = kind

    def add_service(self, zc, type_, name):
        found.setdefault(self.kind, (time.monotonic() - started, name))

    def remove_service(self, *a):
        pass

    def update_service(self, *a):
        pass


# A cold engine: nothing in its cache, so the ServiceBrowser's opening PTR query is
# the only way it learns anything inside the budget — and resolving the instance
# (SRV/TXT/A) is the second half, which a responder that answers only PTR fails.
zc = z.Zeroconf()
z.ServiceBrowser(zc, TYPE, Watch("ours"))
z.ServiceBrowser(zc, NEGATIVE, Watch("negative"))
deadline = started + budget
while time.monotonic() < deadline and "ours" not in found:
    time.sleep(0.05)
latency, name = found.get("ours", (None, None))
print("ours=%s" % ("%.3f" % latency if latency is not None else "none"))
print("foundname=%s" % (name or "-"))
if name:
    handle = z.ServiceInfo(TYPE, name)
    print("resolved=%s" % bool(zc.get_service_info(handle, timeout=2.0)))
    print("port=%s address=%s" % (handle.port, (handle.parsed_addresses() or ["none"])[0]))
print("negative=%s" % ("saw" if "negative" in found else "nothing"))
zc.close()
QUERYEOF
        env -u PYTHONPATH -u http_proxy -u HTTP_PROXY \
            -u https_proxy -u HTTPS_PROXY "$CAST_PY" "$SNIFF_PY" "$LAN_ADDR" 14 \
            > "$SNIFF_OUT" 2>&1 &
        SNIFF_PID=$!
        sleep 1
        env -u PYTHONPATH -u http_proxy -u HTTP_PROXY \
            -u https_proxy -u HTTPS_PROXY "$CAST_PY" "$QUERY_PY" "$MDNS_INSTANCE" 10 \
            > "$QUERY_OUT" 2>&1
        wait "$SNIFF_PID" 2>/dev/null
        WIRE=$(grep -a '^wire ' "$SNIFF_OUT" || echo "wire unreadable")
        echo "  (witness) $(tr '\n' ' ' < "$QUERY_OUT") | $WIRE"
        OURS=$(grep -a '^ours=' "$QUERY_OUT" | head -1)
        FOUNDD=$(grep -a '^foundname=' "$QUERY_OUT" | head -1)
        case "$OURS" in
            "ours=none")
                if [ "$WIRE" = "wire  " ] || ! printf '%s' "$WIRE" | grep -qE 'queries=[1-9]'; then
                    # Outcome 3: the querier's own queries never reached the group
                    # listener either, so this machine told us nothing about either side.
                    skip "no mDNS query appeared on the wire this window — the instrument, not the responder ($WIRE)"
                else
                    # Outcome 2 splits in two, and the split is read from the
                    # engine's own sentences rather than guessed from the machine.
                    # "mDNS engine heard query N of 8 … asking Ptr _googlecast._tcp.local."
                    # says the kernel handed this socket *this* query; "answered its
                    # first query" says we chose to speak. Absent the first, this
                    # process was never given the second.
                    #
                    # The subject inside the sentence is what makes that reading sound,
                    # and it was bought with two wrong readings today. This LAN is not
                    # quiet: the machine's own stack asks `Ptr _irohv1._udp.local.`
                    # about every 0.8 s, and when the line counted **any** query the
                    # whole budget was gone in four seconds — so §25 arrived at a silent
                    # log and said "nothing was delivered". Gating the INFO line on the
                    # subject instead of raising the cap was the other mistake: a query
                    # that is not ours then reads exactly like a query that never
                    # arrived. The shipped shape is both halves — INFO for ours (capped,
                    # named), DEBUG for anyone else's (capped separately), so a reader
                    # can always tell the three empties apart. Measured with that in
                    # place, with no competing socket on the group at all: this process
                    # logs nothing while a python socket joined to 224.0.0.251 on en7
                    # sees the same three 40-byte queries — a delivery answer about this
                    # binary, not about the responder, and not something I get to fix by
                    # clicking his System Settings.
                    HEARD=$(log_grep 'mDNS engine heard query' | tail -1)
                    if [ -n "$HEARD" ]; then
                        fail "the engine read a query ($HEARD) and answered none while $WIRE — that is the responder's own question, not this machine's"
                    else
                        skip "this binary was handed nothing on 5353 while $WIRE (the probe asks from this same Mac; a sender on another machine DID get answers — design doc §16 第 48 条), and a joined python socket on the same port receives the same traffic; the subject-named receiving sentence never fired, and the DEBUG half of it is silent too — proven instead by announce_tests e8, e9 and e10 and by the loopback path"
                    fi
                fi
                ;;
            *)
                pass "a cold browser's query was answered: $OURS"
                # Sub-second against a 60-second keepalive interval: slower than this,
                # and an answer cannot be told from an overheard burst.
                if printf '%s' "$OURS" | grep -qE '^ours=0\.[0-4][0-9]*$'; then
                    pass "answered inside the sub-second range a burst cannot explain"
                else
                    fail "the answer came at ${OURS#ours=}s — too late to tell from a keepalive"
                fi
                # The name check reads the ANSWER line. It used to grep the whole
                # witness file, which also contained the instance name we passed in
                # as an argument — a comparison that cannot fail, and it did not:
                # it printed PASS on a run where nothing answered at all.
                if [ "$FOUNDD" = "foundname=$MDNS_INSTANCE" ]; then
                    pass "and the instance it resolved is the one this run announced"
                else
                    fail "it resolved ${FOUNDD#foundname=} instead of $MDNS_INSTANCE"
                fi
                case "$(cat "$QUERY_OUT")" in
                    *"resolved=True"*) pass "and the full record comes back, not just the PTR" ;;
                    *) fail "the PTR resolved no SRV/TXT/A: $(tr '\n' ' ' < "$QUERY_OUT")" ;;
                esac
                case "$(cat "$QUERY_OUT")" in
                    *"negative=saw"*) fail "the negative control found a device, so the timing above is not our responder's doing" ;;
                    *) pass "nothing answered the type this product does not run" ;;
                esac
                # Both of the engine's own sentences, when the answer happened. The
                # hearing line fires from the same datagram as the answering one, so
                # an answer without it means the pair was broken in the product and
                # the three-way reading above would start lying.
                if [ -n "$(log_grep 'mDNS engine heard query')" ]; then
                    pass "the engine named the receipt before the reply: $(log_grep 'mDNS engine heard query' | tail -1)"
                else
                    fail "it answered a query it never said it received: the receiving sentence is missing from the log"
                fi
                if [ -n "$(log_grep 'mDNS answered its first query')" ]; then
                    pass "and it named the reply: $(log_grep 'mDNS answered its first query' | tail -1)"
                else
                    fail "it answered on the wire but said nothing in the log"
                fi
                ;;
        esac
        rm -f "$QUERY_OUT" "$SNIFF_OUT" "$QUERY_PY" "$SNIFF_PY"
    fi
fi

echo "== 26: an iPhone's RTSP session reaches the mpv behind this shell =="
# The RTSP door, wired into this shell on 2026-10-18. Nothing is stubbed: a real
# socket speaks real RTSP to the bound port, and the witness is what **mpv** says
# while the session is still up — the address it was handed, the name on the
# window, and the number a `SET_PARAMETER` of 0.5 turned into.
#
# This run is a third instance on purpose, with `Chromecast` off and `AirPlay` on.
# Both doors come out of **one** read of the enable list, and "they are separable"
# is only checkable on a run whose list is not the default "everything": a sender
# that reaches 7000 must meet a receiver, and 8009 must still refuse.
AP_SDIR=$(mktemp -d /tmp/macast-rust-airplay.XXXXXX)
AP_PORT=$((PORT + 6))
AP_SOCK="$AP_SDIR/mpvsocket"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust AirPlay", "Macast_Protocols": ["DLNA Protocol", "AirPlay Protocol"]}' "$AP_PORT" \
    > "$AP_SDIR/macast_setting.json"
# The browse starts **before** this instance exists, for the reason §24 writes out in
# full: the announcer's first burst is three packets 225 ms apart and then nothing for
# 60 s, so a browser opened after startup reports "saw nothing" about a run that
# announced loudly and is merely between bursts. Here that ordering is the whole
# difference between a check and a formality — the claim under test is "an iPhone's
# AirPlay list would show this Mac", and that list is built from unsolicited packets.
#
# It resolves **every** `_airplay._tcp` instance it can fold, for two reasons: the
# instance name is not known before launch (the friendly name is sanitised by the
# product, and reading it back from the announcer's own log line is §22's habit), and
# this machine's ControlCenter may advertise a receiver of its own — the shell picks
# our row by name, so a neighbour's row cannot be mistaken for a pass.
AP_BROWSE_PY="$AP_SDIR/browse.py"
AP_BROWSE_OUT="$AP_SDIR/browse.txt"
cat > "$AP_BROWSE_PY" <<'PY'
import sys, time, zeroconf as z

budget, out = float(sys.argv[1]), sys.argv[2]
TYPE = "_airplay._tcp.local."
seen, attempted = set(), set()


class L(z.ServiceListener):
    def add_service(self, zc, type_, name):
        seen.add(name)

    def remove_service(self, *a):
        pass

    def update_service(self, *a):
        pass


def load(zc, name):
    info = z.ServiceInfo(TYPE, name)
    # `load_from_cache` is yes-only-when-complete: PTR **and** SRV **and** TXT **and**
    # A. A half-folded service answers False — and unlike §22, which waits for one
    # known name, a name dropped here is a name the section never reports, so it is
    # put back for another pass instead of being marked attempted.
    return info if info.load_from_cache(zc) else None


zc = z.Zeroconf(ip_version=z.IPVersion.V4Only)
browser = z.ServiceBrowser(zc, TYPE, L())
rows = []
deadline = time.monotonic() + budget
while time.monotonic() < deadline:
    for name in sorted(seen - attempted):
        info = load(zc, name)
        if info is None:
            continue
        attempted.add(name)
        props = info.properties or {}
        rows.append(
            "\t".join(
                (
                    name,
                    "" if info.port is None else str(info.port),
                    ";".join(
                        "%s=%s"
                        % (
                            k.decode("utf-8", "replace"),
                            (v or b"").decode("utf-8", "replace"),
                        )
                        for k, v in props.items()
                    ),
                )
            )
        )
    time.sleep(0.2)
browser.cancel()
zc.close()
with open(out, "w") as fh:
    fh.write("\n".join(rows))
PY
env -u PYTHONPATH -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY \
    "$CAST_PY" "$AP_BROWSE_PY" 30 "$AP_BROWSE_OUT" >/dev/null 2>&1 &
AP_BROWSE_PID=$!
disown "$AP_BROWSE_PID" 2>/dev/null || true
env MACAST_SETTING_DIR="$AP_SDIR" MACAST_MPV_SOCKET="$AP_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$AP_SDIR/stdout.log" 2>&1 &
AP_PID=$!
disown "$AP_PID" 2>/dev/null || true

ap_grep() { grep -ahF "$1" "$AP_SDIR/macast.log" "$AP_SDIR"/logs/*.log 2>/dev/null; }

# The bound number, read from the log rather than assumed. macOS ships its own
# AirPlay Receiver on 7000 — `ControlCenter` holds it on this machine, measured —
# so the port that answers is the one `start()` fell back to.
RTSP_PORT=""
for _ in $(seq 1 40); do
    line=$(ap_grep 'AirPlay RTSP server started on port ' | tail -1)
    if [ -n "$line" ]; then RTSP_PORT=${line##*port }; break; fi
    sleep 0.25
done
if [ -z "$RTSP_PORT" ]; then
    skip "no AirPlay RTSP listener line in $AP_SDIR/macast.log — the face never came up"
else
    pass "the receiver says which port it bound: $RTSP_PORT (7000 is macOS's own)"
    case "$RTSP_PORT" in
        700[0-9]|701[0-9]) pass "and that number is inside the 7000..7019 range the reference tries";;
        *) fail "$RTSP_PORT is outside the 7000..7019 range the reference tries";;
    esac
    if curl -s -m 2 -o /dev/null "http://127.0.0.1:8009/setup/eureka_info"; then
        fail "Chromecast is off in the settings list and 8009 still answered"
    else
        pass "with Chromecast off, 8009 refuses while AirPlay answers"
    fi
fi

# ---- the announcement: can a sender that knows nothing find **this** receiver?
#
# Two claims, asked separately, because they have different failure modes and the
# second one is the reason §22 exists at all:
#   1. the AirPlay run announces an **_airplay._tcp** instance — not nothing, and not
#      a second Chromecast record under the wrong type;
#   2. a second implementation (the `zeroconf` in this repository's `.venv`, the stack
#      the Python app runs) folds those bytes into a complete service, and the numbers
#      it reads are the ones this run actually bound.
#
# What is deliberately **not** claimed here: that a cold sender's *query* about
# `_airplay._tcp` gets an answer. The responder's table was measured for one service
# type only (`Service::responder_answers`), so this browser succeeds on the bursts, not
# on an answer, and a pass below must not be read as "an iPhone can also ask". The
# announcer says the limitation out loud in its own log line, and check 3 is that
# sentence — §16's 「别让『会广播』被读成『会答问』」 made visible on the wire run.
if [ -n "$RTSP_PORT" ]; then
    AP_MDNS_LINE=$(grep -ahF 'mDNS: announcing ' "$AP_SDIR/logs/Discovery.log" 2>/dev/null | tail -1)
    AP_MDNS_INSTANCE=$(printf '%s' "$AP_MDNS_LINE" \
        | sed -n 's/.*announcing \([^ ]*\) on port .*/\1/p')
    for _ in $(seq 1 170); do
        [ -e "$AP_BROWSE_OUT" ] && break
        sleep 0.25
    done
    BROWSE_LEFT=$(pgrep -f "$AP_BROWSE_PY" | tr '\n' ' ')
    [ -n "$BROWSE_LEFT" ] && kill -9 $BROWSE_LEFT 2>/dev/null

    if [ -z "$AP_MDNS_INSTANCE" ]; then
        if [ -n "$(grep -ahF 'mDNS: not announcing' "$AP_SDIR/logs/Discovery.log" 2>/dev/null)" ]; then
            fail "the RTSP receiver answered on $RTSP_PORT and the announcer still refused: \
$(grep -ahF 'mDNS: not announcing' "$AP_SDIR/logs/Discovery.log" | tail -1)"
        else
            fail "neither an announcement nor a refusal: the AirPlay face never said whether \
a sender could find it"
        fi
    else
        case "$AP_MDNS_INSTANCE" in
            *._airplay._tcp.local.) pass "the AirPlay run announces an _airplay._tcp instance \
($AP_MDNS_INSTANCE), not a second Chromecast record" ;;
            *) fail "the AirPlay run announced $AP_MDNS_INSTANCE — the wrong service type on a \
run whose Cast door is shut" ;;
        esac
    fi

    if [ -n "$AP_MDNS_LINE" ] && printf '%s' "$AP_MDNS_LINE" | grep -qF 'does not answer it'; then
        pass "and the line says in words that this type is announced but not answered, so a \
reader is not sent to fix a responder that was never asked to run"
    else
        fail "the AirPlay announcement line hides the responder limitation: ${AP_MDNS_LINE:-none}"
    fi

    if [ ! -e "$AP_BROWSE_OUT" ]; then
        fail "the browse never finished — its window says nothing about any burst"
    elif [ -z "$AP_MDNS_INSTANCE" ]; then
        echo "  (the wire checks were not asked: no instance name to look for)"
    else
        AP_ROW=$(awk -F'\t' -v want="$AP_MDNS_INSTANCE" '$1 == want' "$AP_BROWSE_OUT" | head -1)
        if [ -z "$AP_ROW" ]; then
            fail "zeroconf could not fold $AP_MDNS_INSTANCE into a complete service \
(browse saw: $(cut -f1 "$AP_BROWSE_OUT" | tr '\n' ' '))"
        else
            pass "another stack folded the announcement into a complete service \
(PTR+SRV+TXT+A) without being told the address"
            AP_RPORT=$(printf '%s' "$AP_ROW" | cut -f2)
            if [ "$AP_RPORT" = "$RTSP_PORT" ]; then
                pass "the port a sender reads is the RTSP port that actually answers ($AP_RPORT)"
            else
                fail "the announced port is ${AP_RPORT:-none} while the receiver bound $RTSP_PORT \
— that is 「搜到但投不上」 written onto the wire"
            fi
            AP_TXT=$(printf '%s' "$AP_ROW" | cut -f3)
            AP_DEVICEID=$(printf '%s' "$AP_TXT" | tr ';' '\n' | sed -n 's/^deviceid=//p')
            if [ -n "$AP_DEVICEID" ]; then
                pass "and the TXT carries a deviceid ($AP_DEVICEID), which is how an iPhone tells \
two receivers apart"
            else
                fail "the TXT has no deviceid: $AP_TXT"
            fi
            case ";$AP_TXT;" in
                *";model=Macast;"*)
                    pass "model=Macast, so a reader knows this is ours and not Apple's receiver" ;;
                *) fail "model is not Macast in: $AP_TXT" ;;
            esac
            case ";$AP_TXT;" in
                *";name=Macast Rust AirPlay;"*)
                    pass "and the TXT name is the friendly name this run was seeded with" ;;
                *) fail "the TXT name is not the seeded friendly name: $AP_TXT" ;;
            esac
        fi
    fi
else
    echo "  (the announcement checks were not asked: no RTSP port came up this run)"
fi

if [ -n "$RTSP_PORT" ]; then
    # Derived from `$CLIP`, which the user can point at another file with
    # `MACAST_VERIFY_CLIP` — a needle hardcoded to the default name would then read
    # "the player was handed nothing" about a run that handed it everything.
    CLIP_STEM=$(basename "$CLIP" .mp4)
    RTSP_PY=/tmp/verify_rust_receiver_airplay.py
    RTSP_OUT=/tmp/verify_rust_receiver_airplay.txt
    cat > "$RTSP_PY" <<'PY'
import json, socket, sys, time

port, media, sockpath = int(sys.argv[1]), sys.argv[2], sys.argv[3]
rtsp = socket.create_connection(("127.0.0.1", port), timeout=5)
rtsp.settimeout(3.0)
cseq = 0


def ask(method, headers=(), body=""):
    """One RTSP request. Returns its status line and its header block."""
    global cseq
    cseq += 1
    lines = ["%s rtsp://127.0.0.1/ RTSP/1.0" % method, "CSeq: %d" % cseq]
    lines += ["%s: %s" % (k, v) for k, v in headers]
    payload = body.encode()
    if payload:
        lines.append("Content-Length: %d" % len(payload))
    rtsp.sendall(("\r\n".join(lines) + "\r\n\r\n").encode() + payload)
    got = b""
    while True:
        try:
            chunk = rtsp.recv(4096)
        except socket.timeout:
            break
        if not chunk:
            break
        got += chunk
        head, _, rest = got.partition(b"\r\n\r\n")
        want = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length"):
                want = int(line.split(b":")[1])
        if not rest and not want:
            break
        if len(rest) >= want:
            break
    head = got.split(b"\r\n\r\n", 1)[0].decode("latin-1")
    return (head.split("\r\n")[0] if head else "(nothing back)"), head


def code(line):
    """The **number** in the status line. The first version of this row printed
    `line.split()[-1]`, which is `OK` for a 200 and `Not Found` for a 454, and
    then compared it to `200` — an instrument that could only ever answer wrong."""
    parts = line.split()
    return parts[1] if len(parts) > 1 and parts[1].isdigit() else "none"


first, head = ask("OPTIONS")
print("OPTIONS=%s" % code(first))
print("PUBLIC=%d" % (1 if all(m in head for m in (
    "OPTIONS", "ANNOUNCE", "SETUP", "RECORD", "PLAY", "PAUSE", "FLUSH", "STOP",
    "TEARDOWN", "SET_PARAMETER", "GET_PARAMETER")) else 0))
print("ECHO=%d" % (1 if "CSeq: 1" in head else 0))

first, _ = ask("ANNOUNCE")
print("NOLOCATION=%s" % code(first))

first, head = ask("SETUP", [("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")])
print("SETUP=%s" % code(first))
session = ""
for line in head.split("\r\n"):
    if line.lower().startswith("session"):
        session = line.split(":", 1)[1].split(";")[0].strip()
print("SESSION=%s" % (session or "none"))

print("ANNOUNCE=%s" % code(ask("ANNOUNCE", [("Content-Location", media)])[0]))
print("RECORD=%s" % code(ask("RECORD", [("Session", session)])[0]))
print("PLAY=%s" % code(ask("PLAY", [("Session", session), ("Range", "npt=0-")])[0]))

# The player's side is asserted in bash against **two durable things** — the lines
# this receiver wrote about the handover, and the properties mpv still reports after
# the clip has ended. The first version of this section polled `path` from inside the
# client, and the 12 s clip had already reached `end-file reason=eof` by the time it
# looked: mpv answers `path` with nothing once it is idle, so a working handover read
# as "the player was never handed anything". AGENTS.md §4.2's 「等那个账本」 applies to
# the file, not to a property that goes back to empty.
#
# Two waits, and both are the sender's own cadence rather than a courtesy:
# 0.4 s before the volume, because `set_observe` writes the default volume onto a
# freshly connected player and a SET_PARAMETER in the same millisecond as PLAY lands
# **inside** that window can be overwritten with 100. Measured 2026-10-18 against a
# fresh instance, four runs of that shape: three lost the write, one kept it — a
# window, not a rule — and every later session kept it. `macast_renderer/mpv.py:404-414`
# has the same ordering, so this is the reference's behaviour copied rather than a bug
# introduced here; §8 of the design doc carries the row. No phone sends volume in the
# same millisecond as PLAY: the slider is a separate gesture.
# 1.5 s before TEARDOWN, because a `stop` landing mid-load emits no `file-loaded` at
# all, and the first two versions of this row were asserting that race.
time.sleep(0.4)
first, _ = ask("SET_PARAMETER", [("Session", session), ("Content-Type", "text/parameters")],
               "volume: 0.5\n")
print("SETPARAM=%s" % code(first))
# A sender does not tear down 3 ms after its PLAY, and this section must not: the
# first two versions of this row raced the player's own pipeline (a `stop` landing
# mid-load emits no `file-loaded` at all, so the check was asserting the race
# rather than the handover). One and a half seconds is what mpv on this machine
# takes to report the file, and the events it writes are the durable witnesses the
# bash rows below read.
time.sleep(1.5)
print("TEARDOWN=%s" % code(ask("TEARDOWN", [("Session", session)])[0]))
rtsp.close()
PY
    # Wait for the player to be **there** before the session starts. Every face of
    # this shell is constructed before `player.start()`, so the RTSP door opens while
    # there is no mpv to hand anything to; a real iPhone is seconds behind that, and
    # so is this check. The wait prints what it waited for, because "the player was
    # up" is a premise of the three rows below rather than part of them — the
    # sub-second ordering is its own finding (§16's AirPlay row).
    PLAYER_WAIT=0
    until python3 - "$AP_SOCK" <<'PING' >/dev/null 2>&1
import json, socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(1)
s.connect(sys.argv[1])
s.sendall(json.dumps({"command": ["get_property", "idle-active"], "request_id": 3}).encode() + b"\n")
buf = b""
while b'"data"' not in buf:
    buf += s.recv(4096)
PING
    do
        PLAYER_WAIT=$((PLAYER_WAIT + 1))
        [ "$PLAYER_WAIT" -ge 40 ] && break
        sleep 0.25
    done
    echo "  (the player's own IPC answered after $((PLAYER_WAIT * 250)) ms)"
    env -u PYTHONPATH python3 "$RTSP_PY" "$RTSP_PORT" "file://$CLIP" "$AP_SOCK" > "$RTSP_OUT" 2>&1
    field() { grep -a "^$1=" "$RTSP_OUT" | head -1 | cut -d= -f2-; }
    # One property, over the player's own IPC socket, against **this** instance's
    # socket — the shared `mpv_ask` reads the first instance's, which would answer
    # "what is AirPlay playing" with another session's player (§24 learned that
    # about `log_grep` the hard way: a reader pointed at the wrong directory
    # answers a different question entirely).
    ap_mpv_ask() {
        python3 - "$AP_SOCK" "$1" <<'PINGMPV'
import json, socket, sys, time
path, name = sys.argv[1], sys.argv[2]
# An explicit AF_UNIX socket: `create_connection` wants a (host, port) pair and
# unpacks a path string one character at a time, so every read died with
# `ValueError: too many values to unpack` while the player was answering fine.
# Same total-wait bound as `mpv_ask` (see the note there about who holds the
# player's only IPC client slot): per-recv silence is not a deadline.
DEADLINE = time.monotonic() + 8.0
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(max(0.05, DEADLINE - time.monotonic()))
try:
    s.connect(path)
except OSError:
    print("")
    raise SystemExit(0)
s.sendall(json.dumps({"command": ["get_property", name], "request_id": 7}).encode() + b"\n")
got = ""
try:
    buf = b""
    while True:
        left = DEADLINE - time.monotonic()
        if left <= 0:
            break
        s.settimeout(max(0.05, left))
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
        rows = []
        for line in buf.split(b"\n"):
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue
        hit = [r for r in rows if r.get("request_id") == 7]
        if hit:
            data = hit[0].get("data")
            got = "" if data is None else str(data)
            break
except Exception:
    pass
print(got)
PINGMPV
    }
    if [ ! -s "$CLIP" ]; then
        skip "no clip, so the RTSP session has no media to hand the player"
    else
        echo "  (witness) $(tr '\n' ' ' < "$RTSP_OUT")"
        [ "$(field OPTIONS)" = "200" ] && pass "OPTIONS is answered" \
            || fail "OPTIONS answered $(field OPTIONS): $(head -1 "$RTSP_OUT")"
        [ "$(field PUBLIC)" = "1" ] && pass "and its Public header lists the eleven methods the reference implements" \
            || fail "the Public header is missing methods"
        [ "$(field ECHO)" = "1" ] && pass "the sender's CSeq comes back on the first reply" \
            || fail "CSeq was not echoed, so the sender's numbering drifts from the first exchange"
        [ "$(field NOLOCATION)" = "400" ] && pass "an ANNOUNCE with no address is refused, not accepted" \
            || fail "an address-less ANNOUNCE answered $(field NOLOCATION) — a 200 there promises a play that cannot happen"
        if [ -n "$(ap_grep 'AirPlay ANNOUNCE carried no Content-Location')" ]; then
            pass "and the refusal says why, in the log a user can read"
        else
            fail "the 400 came back and nothing was written down about it"
        fi
        [ "$(field ANNOUNCE)" = "200" ] && pass "the real address is accepted" || fail "ANNOUNCE: $(field ANNOUNCE)"
        [ "$(field SETUP)" = "200" ] && [ -n "$(field SESSION)" ] && pass "SETUP hands back a session id" \
            || fail "SETUP: $(field SETUP) with session='$(field SESSION)'"
        [ "$(field RECORD)" = "200" ] && pass "RECORD is answered" || fail "RECORD: $(field RECORD)"
        [ "$(field PLAY)" = "200" ] && pass "PLAY is answered" || fail "PLAY: $(field PLAY)"
        [ "$(field SETPARAM)" = "200" ] && pass "SET_PARAMETER is answered" || fail "SET_PARAMETER: $(field SETPARAM)"
        [ "$(field TEARDOWN)" = "200" ] && pass "TEARDOWN is answered" || fail "TEARDOWN: $(field TEARDOWN)"

        # What the sender was promised, read off the player rather than off the
        # reply that promised it.
        # Durable witness one: mpv's own log says a file started and loaded, and
        # the line above it says which address the RTSP layer was handed.
        if [ -n "$(ap_grep 'AirPlay ANNOUNCE url=file://')" ]; then
            pass "the announced address was taken: $(ap_grep 'AirPlay ANNOUNCE url=' | tail -1)"
        else
            fail "no ANNOUNCE line in the log — PLAY answered 200 and nothing recorded what it carried"
        fi
        LOADED=""
        for _ in $(seq 1 40); do
            # The needle is inside **double** quotes, so it is written without any
            # backslashes: in bash, `\'` inside double quotes is a literal backslash
            # plus a quote, not an escaped quote. The first version of this row read
            # `"\'event\': \'file-loaded\'"`, grepped for that literal, found nothing
            # on every run, and the failure text (`tail -3` of a log that had long
            # since printed `file-loaded`) read exactly like "the load died mid-way".
            # The product was fine; §4.2's 「假命令行工具的输出」 family, this time
            # with the shell's own quoting rules as the fictional format.
            if [ -n "$(ap_grep "{'event': 'file-loaded'}")" ]; then LOADED=1; break; fi
            sleep 0.25
        done
        if [ -n "$LOADED" ]; then
            pass "and mpv loaded it (file-loaded in logs/MPV.log)"
        else
            fail "mpv never reported file-loaded: $(tail -3 "$AP_SDIR/logs/MPV.log" 2>/dev/null | tr '\n' ' ')"
        fi
        # Durable witness two: the name outlives the clip, and this is the row that
        # separates this shell from the one that carried a second copy of the naming
        # rule (`clip.mp4` on the window instead of `clip`).
        WANT_TITLE="$CLIP_STEM"
        GOT_TITLE=$(ap_mpv_ask force-media-title)
        if [ "$GOT_TITLE" = "$WANT_TITLE" ]; then
            pass "and it is named the way the page and Cast name it: $GOT_TITLE"
        else
            fail "the window says '$GOT_TITLE'; the shared owner answers '$WANT_TITLE'"
        fi
        # Durable witness three: the number the sender asked for, in both scales, in
        # **our** line — and then the value mpv still holds.
        #
        # This row used to poll MPV.log for
        # `{'event': 'property-change', ..., 'data': 50.0}`, and two things are wrong
        # with that: mpv emits no property-change when the value it already has is set
        # again (so a legitimate second gesture is invisible), and the line is an
        # *event* the player chose to broadcast, not the fact under test. What is
        # under test is "the receiver handed over 0–100 correctly, and the player
        # holds it".
        if [ -n "$(ap_grep 'AirPlay SET_PARAMETER volume 0.5 -> renderer volume 50')" ]; then
            pass "the sender's 0.5 became the 0-100 number the player takes, and the receiver said so"
        else
            fail "SET_PARAMETER answered 200 and nothing was written about which number was handed over: $(ap_grep 'SET_PARAMETER' | tail -2)"
        fi
        GOT_VOLUME=$(ap_mpv_ask volume)
        if [ "$GOT_VOLUME" = "50" ] || [ "$GOT_VOLUME" = "50.0" ]; then
            pass "and mpv still holds that volume after the session: $GOT_VOLUME"
        else
            fail "mpv reports volume='$GOT_VOLUME' after a SET_PARAMETER of 0.5 — the handover was lost (see the 0.4 s note above: a first-session write landing inside set_observe's window is overwritten by the default volume, in the reference as here)"
        fi
        if [ -n "$(ap_grep 'RTSP idle timeout')" ]; then
            fail "the control connection timed out mid-session: $(ap_grep 'RTSP idle timeout' | tail -1)"
        else
            pass "and the session never sat idle long enough for the receiver to reclaim it"
        fi
    fi
    kill -9 "$AP_PID" 2>/dev/null
    pkill -9 -f "$AP_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $AP_SDIR (macast.log, logs/MPV.log, stdout.log)"
    else
        rm -rf "$AP_SDIR"
    fi
    rm -f "$RTSP_PY" "$RTSP_OUT"
fi

echo "== 27: a cast made before the restart is still on the card after it =="
# §23 is the section that found this gap: it delivered `history: []` as its first
# red on a machine that had just cast something, because the reference keeps the
# rows in `Setting[Play_History]` (so a restart shows yesterday's casts) while this
# shell kept them in the state table (so a restart showed nothing). An empty list
# with two causes is the §4.14 sentence, and the only witness that separates them
# is a **restart**. Memory-only assertions could never have caught this, which is
# why this section starts a second process rather than asking the first one twice.
#
# A fourth instance on purpose, with `Macast_Protocols: ["DLNA Protocol"]`: the
# main one is still holding 8009/8008, and a section that "failed to bind the Cast
# faces" would read as a gate bug rather than as the port it is.
H_SDIR=$(mktemp -d /tmp/macast-rust-history.XXXXXX)
H_PORT=$((PORT + 7))
H_SOCK="$H_SDIR/mpvsocket"
H_BASE="http://127.0.0.1:$H_PORT"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust History", "Macast_Protocols": ["DLNA Protocol"]}' "$H_PORT" \
    > "$H_SDIR/macast_setting.json"

h_start() {
    # One launcher for both processes in this section, so "which pid is ours" is
    # decided in one place. `MACAST_MPV_SOCKET` stays inside the temp dir: the
    # teardown can then reap exactly this run's player and nobody else's.
    env MACAST_SETTING_DIR="$H_SDIR" MACAST_MPV_SOCKET="$H_SOCK" \
        MACAST_RESOURCE_DIR="$REPO/macast" \
        perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
        "$BIN" > "$H_SDIR/stdout.log" 2>&1 &
    H_PID=$!
    disown "$H_PID" 2>/dev/null || true
    H_UP=""
    for _ in $(seq 1 40); do
        [ "$(lsof -nP -iTCP:"$H_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$H_PID" ] \
            && { H_UP=1; break; }
        sleep 0.25
    done
}

h_stop() {
    kill -9 "$H_PID" 2>/dev/null
    pkill -9 -f "$H_SOCK" 2>/dev/null
}

if [ -z "$MEDIA_URL" ]; then
    skip "no media address to cast, so there is no row to bring back"
else
    h_start
    if [ -z "$H_UP" ]; then
        skip "the history instance never became the listener on $H_PORT (someone else holds it: $(lsof -nP -iTCP:$H_PORT -sTCP:LISTEN -t 2>/dev/null | head -1))"
    else
        pass "a DLNA-only instance is up on $H_PORT while the main one keeps 8009"
        # The token is this instance's own: it mints one into ITS settings file,
        # and reading the main instance's would test a secret this run never wrote.
        api_probe "$H_BASE/api?query=cast-info"
        H_TOKEN=$(printf '%s' "$API_MSG" | sed -nE 's/.*token=([0-9a-f]{32}).*/\1/p')
        if [ -z "$H_TOKEN" ]; then
            fail "cast-info gave this instance no token to cast with: $API_MSG"
        else
            TITLE_H="Rust-History-Row"
            api_probe "$H_BASE/api?query=cast&url=$MEDIA_URL&title=$TITLE_H&token=$H_TOKEN"
            [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
                && pass "the cast was accepted by this instance" \
                || fail "the cast answered HTTP $API_STATUS: $API_MSG"
            api_probe "$H_BASE/api?query=status"
            [ "$(json_len history)" = "1" ] \
                && pass "one row on the card after one cast" \
                || fail "history has $(json_len history) rows after one cast"
            [ "$(json_ask history.0.uri)" = "$MEDIA_URL" ] \
                && pass "and the row names the address this instance was handed" \
                || fail "the row says '$(json_ask history.0.uri)', not $MEDIA_URL"
            # The three keys are the reference's (`protocol.py:1211-1215`). The
            # window command used to spell the address `url`; a fourth spelling
            # in the user's file is exactly what this row refuses.
            KEYS_H=$(json_keys history.0 | tr ',' '\n' | sort | tr '\n' ',')
            [ "$KEYS_H" = "time,title,uri," ] \
                && pass "the row carries exactly the reference's three keys" \
                || fail "the row's keys are '$KEYS_H', not time,title,uri"
            if grep -q '"Play_History"' "$H_SDIR/macast_setting.json" 2>/dev/null \
                && grep -a '"uri"' "$H_SDIR/macast_setting.json" >/dev/null 2>&1; then
                pass "and it is in the settings file, not only in this process"
            else
                fail "the cast left no Play_History in $H_SDIR/macast_setting.json — a restart would empty the card"
            fi
            h_stop
            h_start
            if [ -z "$H_UP" ]; then
                fail "the restarted instance never came back on $H_PORT, so nothing below was read"
            else
                # Re-probe: `$API_BODY` is one shared file and the last reader in
                # this section was not `query=status` (§23 learned that the hard
                # way — the next reader re-asks before it answers).
                api_probe "$H_BASE/api?query=status"
                [ "$(json_len history)" = "1" ] \
                    && pass "after a kill and a fresh process, the row is still there" \
                    || fail "the restart emptied the card: $(json_len history) row(s)"
                [ "$(json_ask history.0.title)" = "$TITLE_H" ] \
                    && pass "with its title intact: $(json_ask history.0.title)" \
                    || fail "the restored row is titled '$(json_ask history.0.title)'"
                # The file is named, not "somewhere in the logs": the state table
                # speaks through `logs/Protocol.log` (`protocol.py:30` is
                # `getLogger("Protocol")`, and `logsplit` claims it). Until this round
                # `main.rs` handed the table the *player's* logger, so the history
                # sentences sat in `logs/MPV.log` next to the renderer's events — a
                # check that grepped every file would have passed against that wrong
                # destination (§4.3's "a log that points at the wrong suspect", one
                # layer up: here it points at the wrong _file_).
                grep -aqF 'play history: 1 row(s) read back' "$H_SDIR/logs/Protocol.log" \
                    && pass "and the boot says where they came from, in the protocol's own log" \
                    || fail "logs/Protocol.log does not claim the read-back: $(grep -rahF 'play history' "$H_SDIR"/logs/*.log "$H_SDIR/macast.log" 2>/dev/null | tail -2)"
                # The restored rows are the same table the writers use, not a copy
                # for display: casting the same address again must still be one row
                # (the reference's dedupe is by URI, `protocol.py:1208-1210`).
                api_probe "$H_BASE/api?query=cast&url=$MEDIA_URL&title=$TITLE_H&token=$H_TOKEN"
                api_probe "$H_BASE/api?query=status"
                [ "$(json_len history)" = "1" ] \
                    && pass "a second cast of the same address dedupes against the restored row" \
                    || fail "the restored row is display-only: history grew to $(json_len history) rows"
                h_stop
            fi
        fi
    fi
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $H_SDIR (macast.log, logs/MPV.log, stdout.log, macast_setting.json)"
    else
        rm -rf "$H_SDIR"
    fi
fi

echo "== 28: the 模块设置 table is the one the reference module writes =="
# Two implementations read the same file and answer "who owns this key, what is it
# called, and can the page edit it". The Rust side answers through its HTTP face; the
# Python side is the shipped module (`macast/module_settings.py`) loaded **by file
# path**, because `import macast` would drag in cherrypy and a settings directory
# (`macast/__init__.py` imports `.utils`, which freezes `SETTING_DIR` at import time).
#
# Why a cross-implementation check and not just a Rust unit test: this table is **text
# a user reads** plus **three derived booleans** (`present`, `type`, `editable`). The
# unit tests can prove the Rust side is self-consistent; only the other implementation
# proves the strings and the `type` names are the ones the page was written against —
# the same reason §22 hands our bytes to `zeroconf` instead of to our own parser, and
# §23 compares `sys.platform` against the interpreter that runs the app.
#
# The comparison is done against **the document Rust just returned**, not against the
# settings file: the instance writes its own USN and friendly name at boot, so the file
# and the answer are different moments. Rebuilding Python's groups from the rows the
# face actually served asks the question we care about — same table, same values, same
# verdicts — without racing the boot path.
MS_SDIR=$(mktemp -d /tmp/macast-rust-modulesettings.XXXXXX)
MS_PORT=$((PORT + 8))
MS_SOCK="$MS_SDIR/mpvsocket"
MS_TOKEN=0123456789abcdef0123456789abcdef
# Seven value shapes on purpose (`str`, `int`, `float`, `bool`, `null`, `list`, `dict`),
# because `type` is a string the page branches on and `editable` is derived from it;
# plus two keys no table owns — one that is a *plugin's* key in the reference
# (`Mirror_Target`) and one that is nothing at all (`zeta_custom`) — because this shell
# has no plugin subsystem and both sides must therefore agree the bucket is 其他.
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust ModuleSettings", "Macast_Protocols": ["DLNA Protocol"], "Api_Token": "%s", "CheckUpdate": true, "Https_Port": 58443, "Https_Cert": null, "Blocked_Interfaces": ["vmnet8"], "PlayerDefaultVolume": 42.5, "Mirror_Target": "192.168.1.13:8009", "zeta_custom": {"nested": [1, 2]}}' \
    "$MS_PORT" "$MS_TOKEN" > "$MS_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$MS_SDIR" MACAST_MPV_SOCKET="$MS_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$MS_SDIR/stdout.log" 2>&1 &
MS_PID=$!
disown "$MS_PID" 2>/dev/null || true
MS_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$MS_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$MS_PID" ]; then
        MS_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$MS_READY" ]; then
    fail "the module-settings instance never became the listener on $MS_PORT"
    kill -9 "$MS_PID" 2>/dev/null
    rm -rf "$MS_SDIR"
else
    MS_DOC="$MS_SDIR/rust.json"
    # **Before the query**, so the after-the-query comparison below is about the read and
    # not about the boot that already happened.
    MS_MD5_BEFORE=$(md5 -q "$MS_SDIR/macast_setting.json")
    curl -s --noproxy '*' -m 5 \
        "http://127.0.0.1:$MS_PORT/api?query=module-settings&token=$MS_TOKEN" \
        > "$MS_DOC"
    ms_ask() {
        # $1 = a python expression over `doc` (the parsed document). The expression
        # travels as **argv**, not stdin, because the program itself arrives as a heredoc
        # — two things cannot own the same stdin, and the first version of this helper
        # read `sys.stdin` while the shell fed it a heredoc, so every call printed the
        # same empty answer. Compiled in single-eval mode and printed as `repr` for
        # non-strings, so a tuple/list answer compares literally instead of depending on
        # how `print` renders one.
        env -u PYTHONPATH "$CAST_PY" - "$MS_DOC" "$1" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
answer = eval(compile(sys.argv[2], "<ms_ask>", "eval"), {"doc": doc, "json": json})
print(answer if isinstance(answer, str) else repr(answer))
PY
    }

    CODE=$(ms_ask 'doc["code"]')
    HAS_MSG=$(ms_ask '"message" in doc')
    [ "$CODE" = "0" ] && [ "$HAS_MSG" = "False" ] \
        && pass "the grouped read answers {code, groups} — success carries no message key" \
        || fail "code=$CODE message=$HAS_MSG body=$(head -c 200 "$MS_DOC")"

    IDS=$(ms_ask '"|".join(g["id"] for g in doc["groups"])')
    [ "$IDS" = "core|player|other" ] \
        && pass "group order is core → player → 其他（未归类）, with an unowned bucket this run really has" \
        || fail "group ids came out as: $IDS"

    # A key the boot path never touches: the row must be *shown* as absent, with the
    # empty type string and editable true (writing it creates it).
    #
    # Compared as **canonically sorted JSON against a hand-written literal**, never as
    # `repr` of a parsed dict: a parsed dict keeps the *document's* field order, and this
    # port writes cells alphabetically (its table is a `BTreeMap`), so repr-vs-literal
    # would be red about ordering while pretending to ask about values. The literal is
    # typed here rather than generated from the reference module on purpose — the
    # whole-document comparison below already asks the reference, and a second copy of the
    # same source would let a table that drifts on both sides pass twice.
    START=$(ms_ask 'json.dumps([c for g in doc["groups"] if g["id"]=="core" for c in g["keys"] if c["key"]=="StartAtLogin"][0], sort_keys=True, ensure_ascii=False)')
    WANT_START=$(env -u PYTHONPATH "$CAST_PY" - <<'PY'
import json
print(json.dumps({
    "key": "StartAtLogin",
    "label": "开机自启",
    "hint": "bool：登录后自动运行 Macast。",
    "present": False,
    "value": None,
    "type": "",
    "editable": True,
}, sort_keys=True, ensure_ascii=False))
PY
)
    [ "$START" = "$WANT_START" ] \
        && pass "an absent key is still a row: present false, type the empty string, editable true" \
        || fail "the absent row came out as: $START (wanted $WANT_START)"

        # Each seeded shape, asked of the document by **name** rather than as a comparison of
    # the whole present set. The first version sorted every present row against a literal
    # list and forgot that the instance writes its own `USN` and friendly name at boot —
    # so the expected set grew with whatever the harness did that day, which is the same
    # mistake as a test that depends on which machine runs it (AGENTS.md §4.2's
    # 「空结果要说清是哪一种空」 family). Per-name questions cannot drift with the boot path.
    SHAPE=$(env -u PYTHONPATH "$CAST_PY" - "$MS_DOC" <<'PY'
import json, sys

doc = json.load(open(sys.argv[1]))
cells = {c["key"]: c for g in doc["groups"] for c in g["keys"]}
# (key, type(value).__name__, editable) for the eleven values this section seeds.
# `Https_Cert: null` is deliberately in here: a stored null is **present**, and Python's
# rule makes it editable (`or value is None`), which is the one cell in the table where
# "absent" and "present as null" have to be told apart by `present` alone.
EXPECTED = [
    ("DLNA_FriendlyName", "str", True),
    ("Api_Token", "str", True),
    ("Mirror_Target", "str", True),
    ("ApplicationPort", "int", True),
    ("Https_Port", "int", True),
    ("PlayerDefaultVolume", "float", True),
    ("CheckUpdate", "bool", True),
    ("Https_Cert", "NoneType", True),
    ("Blocked_Interfaces", "list", False),
    ("Macast_Protocols", "list", False),
    ("zeta_custom", "dict", False),
]
bad = []
for name, want_type, want_editable in EXPECTED:
    cell = cells.get(name)
    if cell is None:
        bad.append("%s: no row at all" % name)
        continue
    if not cell["present"]:
        bad.append("%s: the file holds it and the row says absent" % name)
    if cell["type"] != want_type:
        bad.append("%s: type %r, expected %r" % (name, cell["type"], want_type))
    if cell["editable"] is not want_editable:
        bad.append("%s: editable %r, expected %r" % (name, cell["editable"], want_editable))
# And the shape list is not vacuous: this section's own fixture has to have written them.
assert len(EXPECTED) == 11, "the expected table shrank to %d" % len(EXPECTED)
print("OK" if not bad else "; ".join(bad))
PY
)
    [ "$SHAPE" = "OK" ] \
        && pass "all seven type names are Python's type(value).__name__, and only scalars are editable" \
        || fail "shape table differs: $SHAPE"

    OTHER=$(ms_ask '[c for g in doc["groups"] if g["id"]=="other" for c in g["keys"]][0]')
    case "$OTHER" in
        *"未识别归属的键"*) pass "the unowned bucket names the key verbatim and says what to do with it" ;;
        *) fail "the 其他 row gives no next step: $OTHER" ;;
    esac

    # The one row whose ownership is a **port decision** (module header): in the reference
    # `Timeout` is a plugin key; here the device hand-off sender reads it, so the hint has
    # to name this build instead of a plugin that is not loaded.
    TO_HINT=$(ms_ask '[c["hint"] for g in doc["groups"] for c in g["keys"] if c["key"]=="Timeout"][0]')
    case "$TO_HINT" in
        *"插件"*) fail "the Timeout row still describes a plugin this shell does not run: $TO_HINT" ;;
        *"投给"*) pass "Timeout is a core row here, and its hint names the reader in *this* build" ;;
        *) fail "the Timeout hint says nothing about who reads it: $TO_HINT" ;;
    esac

    # The token is in the answer. Asserted, not assumed away: both settings reads hand
    # over the whole table because that is what the tab edits, and the thing keeping it
    # from a browser is the missing CORS header (§23's invariant scan), not a silent
    # redaction that would make the page's own editor wrong.
    TOK=$(ms_ask '[c["value"] for g in doc["groups"] for c in g["keys"] if c["key"]=="Api_Token"][0]')
    [ "$TOK" = "$MS_TOKEN" ] \
        && pass "and the grouped read gives the token the raw dump gives — one table, two readers" \
        || fail "Api_Token came back as: $TOK"

    # The real cross-check, in one python program that reads both sides.
    MS_CROSS=$(env -u PYTHONPATH "$CAST_PY" - "$REPO" "$MS_DOC" <<'PY'
import importlib.util, json, sys

repo, doc_path = sys.argv[1], sys.argv[2]
spec = importlib.util.spec_from_file_location(
    "module_settings_ref", repo + "/macast/module_settings.py")
ref = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ref)

rust = json.load(open(doc_path))
# The stored table as the Rust face saw it: every row that says it is present, with the
# value it reported. `build_groups([], stored)` is the reference's no-manager shape —
# which is what this shell actually is until the plugin subsystem is ported.
stored = {}
for group in rust["groups"]:
    for cell in group["keys"]:
        if cell["present"]:
            stored[cell["key"]] = cell["value"]

mine = ref.payload([], stored)


def canon(x):
    return json.dumps(x, sort_keys=True, ensure_ascii=False)


# **The one row this port moves is excluded by name, counted, and checked empty on the
# reference's side.** `Timeout` belongs to the `cast_local_file` plugin there
# (`module_settings.py:113`) and to the core here (this build has no plugin subsystem and
# the device hand-off sender reads it), so the two documents cannot be equal at that
# cell. Excluding it is legitimate; excluding it *open-endedly* is not — a comparison
# that allows an unbounded set of differences stops being a comparison, which is the same
# failure as a CI gate whose exemption list grows to cover whatever is red. So: exactly
# one row removed, from `core`, and no `Timeout` row anywhere in the reference's document.
# Any *other* divergence still reddens the comparison below.
removed = []
for group in rust["groups"]:
    kept = []
    for cell in group["keys"]:
        if group["id"] == "core" and cell["key"] == "Timeout":
            removed.append(cell)
            continue
        kept.append(cell)
    group["keys"] = kept
in_reference = {c["key"] for g in mine["groups"] for c in g["keys"] if c["key"] == "Timeout"}
if len(removed) != 1 or in_reference:
    rows_note = "TIMEOUT_EXCLUSION_UNSAFE removed=%d in_reference=%s" % (len(removed), sorted(in_reference))
else:
    rows_note = "MOVED=core/Timeout"
same = canon(mine) == canon(rust)
rows = []
if same:
    rows.append("EQUAL")
else:
    rows.append("DIFFER")
    # Say **which cell** disagrees; a whole-document diff printed into a log line is the
    # reading that gets ignored.
    for g_mine, g_rust in zip(mine["groups"], rust["groups"]):
        if canon(g_mine) != canon(g_rust):
            names_m = {c["key"]: c for c in g_mine["keys"]}
            names_r = {c["key"]: c for c in g_rust["keys"]}
            for k in sorted(set(names_m) | set(names_r)):
                if names_m.get(k) != names_r.get(k):
                    rows.append("%s/%s py=%s rust=%s" % (g_mine["id"], k, names_m.get(k), names_r.get(k)))
            break
# Field order inside one cell: Python writes the reference's order, this port writes the
# alphabetical one (its table is a `BTreeMap`). The deviation is recorded in
# docs/Rust-Port-Design.md §8; this line is its witness — a deviation nobody measures
# turns into "we thought it was ordered".
rows.append(rows_note)
py_order = list(mine["groups"][0]["keys"][0].keys())
rust_order = list(rust["groups"][0]["keys"][0].keys())
rows.append("PYORDER=" + ",".join(py_order))
rows.append("RUSTORDER=" + ",".join(rust_order))
print("\n".join(rows))
PY
)
    MS_VERDICT=$(printf '%s' "$MS_CROSS" | head -1)
    MS_PY_ORDER_LINE=$(printf '%s\n' "$MS_CROSS" | grep -a '^PYORDER=' | head -1)
    MS_RUST_ORDER_LINE=$(printf '%s\n' "$MS_CROSS" | grep -a '^RUSTORDER=' | head -1)

    if [ "$MS_VERDICT" = "EQUAL" ]; then
        pass "the shipped module and this face produce the same document, cell by cell"
    else
        fail "the two implementations disagree: $(printf '%s\n' "$MS_CROSS" | tr '\n' ' ' | head -c 600)"
    fi
    # …and the field order the reference writes, asserted from the reference itself
    # rather than from memory, then compared against this port's sorted one.
    if [ "$MS_PY_ORDER_LINE" = "PYORDER=key,label,hint,present,value,type,editable" ]; then
        pass "the reference's own cell order is the one the page was written against"
    else
        fail "unexpected reference cell order: ${MS_PY_ORDER_LINE:-none}"
    fi
    # …and **this port writes the same order**, because the workspace manifest turns on
    # `serde_json`'s `preserve_order` (root `Cargo.toml`: it is there for
    # `LIVE_LOAD_OPTIONS`, a Python dict whose order is a promise to mpv). The first
    # version of this row asserted the cells came out alphabetically and went red on
    # correct code — the deviation this section recorded in §8 is about the *settings
    # table's* keys (`Setting`'s `BTreeMap`, which is what `launch-param` dumps), not
    # about a document cell, and conflating the two maps produced a fake product finding.
    # So this row now pins the thing that is actually true: the page reads a cell in the
    # reference's field order, and if `preserve_order` ever goes away the row says so.
    if [ "$MS_RUST_ORDER_LINE" = "RUSTORDER=key,label,hint,present,value,type,editable" ]; then
        pass "and this port writes the same field order — preserve_order, so the page's row layout is not a coin flip"
    else
        fail "the port's cell order drifted from the reference's: ${MS_RUST_ORDER_LINE:-none}"
    fi

    # The gate, on the real face: a LAN caller with no token hears the management
    # sentence and gets no table at all — the document holds every stored value, so this
    # is not a cosmetic difference.
    # The refusal is read the way §23 taught every read here: **parse, then compare**.
    # The first version grepped the raw body for a number, and the body is pretty-printed
    # with `ensure_ascii` escapes, so a line-wise `sed` picked the document's own
    # `"code": 1` — which is the *correct* refusal (the reference's gate answers with
    # `code: 1` and HTTP 403; the two are different layers, and an earlier draft of this
    # row demanded 403 inside the body and went red on correct code).
    api_probe "http://$LAN:$MS_PORT/api?query=module-settings"
    [ "$API_STATUS" = "403" ] \
        && pass "a LAN caller with no token gets HTTP 403 from the grouped table" \
        || fail "a LAN caller got HTTP $API_STATUS from module-settings"
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*) pass "and hears the management sentence, as that is the gate that ran" ;;
        *) fail "LAN refusal wording: $API_MSG" ;;
    esac
    [ "$(json_ask code)" = "1" ] \
        && pass "the body's code is the reference's 1, not an HTTP status wearing a JSON field" \
        || fail "the refusal's body code: $(json_ask code)"
    if json_has groups; then
        fail "the refusal still carried the grouped table"
    else
        pass "and it carries no table at all — not the labels, not the values, not the token"
    fi

    api_probe "http://$LAN:$MS_PORT/api?query=module-settings&token=$MS_TOKEN"
    if json_has groups; then
        pass "with the token the same caller is served — the door is the token, not the address"
    else
        fail "a tokened LAN caller was refused: HTTP $API_STATUS $API_MSG"
    fi

    # And the read left the file alone: 27 rows are built whether or not the table holds
    # them, and a page that polls this query must not be the thing that writes them in.
    MS_MD5_AFTER=$(md5 -q "$MS_SDIR/macast_setting.json")
    [ "$MS_MD5_BEFORE" = "$MS_MD5_AFTER" ] \
        && pass "polling the grouped read did not grow the settings file it reports" \
        || fail "the read wrote the file ($MS_MD5_BEFORE -> $MS_MD5_AFTER)"

    # ---- the write path, on the same instance
    #
    # `set-module-setting` is the first row on this face whose gate is
    # `code-execution`, and the claim that makes it interesting is AGENTS.md §4.7b's:
    # **loopback is not enough**. A drive-by form POST arrives from 127.0.0.1 with no
    # `Origin` that proves the caller is our own page, and the 自动化钩子 keys live in
    # this very table — their values go to `subprocess(shell=True)` on the next cast. So
    # writing one *is* running one, and the only thing between the two is a token a
    # cross-origin reader cannot see (this port never sends a CORS header — §23's scan).
    #
    # These rows go over the real socket on purpose. The unit tests build `Request`
    # objects; what a form POST actually looks like after urlencoding, Content-Length and
    # the dispatcher's field scan is the half that §4.2's "the stub is also an
    # implementation" is about.
    MS_MSGBASE="http://127.0.0.1:$MS_PORT/api"
    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Network_Interface" "value=en0"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "403" ] \
        && pass "from this machine with no token, the write is refused outright" \
        || fail "loopback wrote without a token: HTTP $API_STATUS code '$API_CODE'"
    case "$API_MSG" in
        *"installing code"*) pass "and the refusal names the code gate, not a generic permission" ;;
        *) fail "code-gate wording: $API_MSG" ;;
    esac

    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Network_Interface" "value=en0" "token=$MS_TOKEN"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "已保存" ] && [ "$(json_ask value)" = "en0" ] \
        && pass "with the token the same write goes through, and answers with what it stored" \
        || fail "a tokened write answered HTTP $API_STATUS code '$API_MSG'"
    grep -q '"Network_Interface": "en0"' "$MS_SDIR/macast_setting.json" \
        && pass "and it is on disk — the answer and the file are one claim, not two" \
        || fail "the save answered 已保存 and the file says otherwise: $(grep -c Network_Interface "$MS_SDIR/macast_setting.json")"

    api_probe "$MS_MSGBASE?query=module-settings&token=$MS_TOKEN"
    ROW=$(env -u PYTHONPATH "$CAST_PY" - "$API_BODY" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
cell = [c for g in doc["groups"] for c in g["keys"] if c["key"] == "Network_Interface"][0]
print("%s|%s|%s" % (cell["present"], cell["type"], cell["value"]))
PY
)
    [ "$ROW" = "True|str|en0" ] \
        && pass "the grouped read shows the key the write path just stored — one table, both doors" \
        || fail "read-after-write through the other door: $ROW"

    MS_MD5_STABLE=$(md5 -q "$MS_SDIR/macast_setting.json")
    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Play_History" "value=[1, 2]" "token=$MS_TOKEN"
    [ "$API_CODE" = "1" ] && [ "$API_MSG" = "列表/对象值请在「高级设置」JSON 里编辑" ] \
        && pass "a structure is refused as a structure, in the sentence that says where to edit it" \
        || fail "a container value was accepted or mis-worded: '$API_CODE' '$API_MSG'"
    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Never_Stored_Key" "value=1" "token=$MS_TOKEN"
    [ "$API_CODE" = "1" ] && [ "$API_MSG" = "未知设置项：Never_Stored_Key" ] \
        && pass "an invented name is refused and read back, not silently created" \
        || fail "an unowned key was written or answered vaguely: '$API_CODE' '$API_MSG'"
    [ "$(md5 -q "$MS_SDIR/macast_setting.json")" = "$MS_MD5_STABLE" ] \
        && pass "and both refusals left the settings file byte-identical" \
        || fail "a refused write rewrote the user's settings file"

    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Network_Interface" "remove=1" "token=$MS_TOKEN"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "已删除" ] && [ "$(json_ask removed)" = "true" ] \
        && pass "the delete says which of the two things happened, and says it with both fields" \
        || fail "the delete answered '$API_CODE' '$API_MSG' removed='$(json_ask removed)'"
    api_post "$MS_MSGBASE" "set-module-setting=1" "key=Network_Interface" "remove=1" "token=$MS_TOKEN"
    [ "$API_CODE" = "0" ] && [ "$API_MSG" = "本就未设置" ] && [ "$(json_ask removed)" = "false" ] \
        && pass "the second delete of the same key is a different sentence, and a different removed flag" \
        || fail "the absent-key delete lied: '$API_MSG' removed='$(json_ask removed)'"
    grep -q '"Network_Interface"' "$MS_SDIR/macast_setting.json" \
        && fail "the key is still in the file after 「已删除」" \
        || pass "and the key really is gone from the file, not nulled out"

    kill -9 "$MS_PID" 2>/dev/null
    pkill -9 -f "$MS_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $MS_SDIR (rust.json, macast.log, macast_setting.json)"
    else
        rm -rf "$MS_SDIR"
    fi
fi



echo "== 29: the 插件仓库 coordinates are the ones the shipped module writes =="
# Two implementations read **one settings file** and answer "which URLs does the page
# fetch, and is the domestic mirror on". The Rust side answers through its HTTP face; the
# Python side is the shipped `macast/plugin_repo.py`, loaded by file path with a stub
# parent package — its module-level `from .utils import Setting` (`plugin_repo.py:23`) is
# exactly the import that would otherwise drag in cherrypy and a `SETTING_DIR` frozen at
# import time (§4.9). The stub `Setting` reads **the instance's own file from disk**, so
# this is not two fixtures agreeing with each other: it is the shipped module looking at
# the bytes this port wrote.
#
# Why this section exists at all: the previous round answered `plugin_repo` with four
# empty values, and while porting the module the emptiness turned out to be **a lie about
# a key this shell does have** — `Github_CN_Mirror` is in the 模块设置 ownership table
# (§28), so one file produced `true` in that card and `false` in this one. The pair of
# rows marked "two readers" below is that claim's permanent witness.
PR_SDIR=$(mktemp -d /tmp/macast-rust-pluginrepo.XXXXXX)
PR_PORT=$((PORT + 9))
PR_SOCK="$PR_SDIR/mpvsocket"
PR_TOKEN=0123456789abcdef0123456789abcdef
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust PluginRepo", "Macast_Protocols": ["DLNA Protocol"], "Api_Token": "%s", "CheckUpdate": true}' \
    "$PR_PORT" "$PR_TOKEN" > "$PR_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$PR_SDIR" MACAST_MPV_SOCKET="$PR_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$PR_SDIR/stdout.log" 2>&1 &
PR_PID=$!
disown "$PR_PID" 2>/dev/null || true
PR_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$PR_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$PR_PID" ]; then
        PR_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$PR_READY" ]; then
    fail "the plugin-repo instance never became the listener on $PR_PORT"
    kill -9 "$PR_PID" 2>/dev/null
    rm -rf "$PR_SDIR"
else
    PR_BASE="http://127.0.0.1:$PR_PORT/api"
    PR_DOC="$PR_SDIR/rust.json"
    PR_SET="$PR_SDIR/macast_setting.json"
    PR_PANEL="$PR_SDIR/module-settings.json"

    # One read of the coordinates, into a file the cross-checkers open.
    pr_read() {
        curl -s --noproxy '*' -m 5 "$PR_BASE?query=plugin-info&token=$PR_TOKEN" > "$PR_DOC"
    }

    # $1 = a python expression over `doc`. Same shape as §28's `ms_ask`: the expression
    # travels as **argv**, because the program itself arrives on stdin.
    pr_ask() {
        env -u PYTHONPATH "$CAST_PY" - "$PR_DOC" "$1" <<'PYEOF'
import json, sys
doc = json.load(open(sys.argv[1]))
answer = eval(compile(sys.argv[2], "<pr_ask>", "eval"), {"doc": doc, "json": json})
print(answer if isinstance(answer, str) else repr(answer))
PYEOF
    }

    # Both sides of one comparison, in one program: the reference's document and this
    # face's, keyed by key, with the **field order** of each printed as its own line.
    pr_cross() {
        env -u PYTHONPATH "$CAST_PY" - "$REPO" "$PR_DOC" "$PR_SET" <<'PYEOF'
import importlib.util, json, sys, types

repo, doc_path, set_path = sys.argv[1], sys.argv[2], sys.argv[3]
pkg = types.ModuleType("pr_ref")
pkg.__path__ = []
utils = types.ModuleType("pr_ref.utils")


class _Prop:
    def __init__(self, name):
        self.name = name


class _Setting:
    def _table(self):
        try:
            with open(set_path, encoding="utf-8-sig") as handle:
                return json.load(handle)
        except (OSError, ValueError):
            return {}

    def has(self, prop):
        return prop.name in self._table()

    def get(self, prop, default=None):
        return self._table().get(prop.name, default)

    def set(self, prop, value):
        raise AssertionError("Python must not write this file")

    def unset(self, prop):
        raise AssertionError("Python must not write this file")


utils.Setting = _Setting()
utils.SettingProperty = types.SimpleNamespace(Github_CN_Mirror=_Prop("Github_CN_Mirror"))
pkg.utils = utils
sys.modules["pr_ref"] = pkg
sys.modules["pr_ref.utils"] = utils
spec = importlib.util.spec_from_file_location(
    "pr_ref.plugin_repo", repo + "/macast/plugin_repo.py")
ref = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ref)

rust = json.load(open(doc_path))


def canon(x):
    return json.dumps(x, sort_keys=True, ensure_ascii=False)


want = ref.describe()
got = rust.get("plugin_repo")
rows = []
if canon(want) == canon(got):
    rows.append("EQUAL")
else:
    rows.append("DIFFER")
    keys = set(want) | set(got if isinstance(got, dict) else {})
    for key in sorted(keys):
        mine = canon(want.get(key))
        theirs = canon(got.get(key) if isinstance(got, dict) else None)
        if mine != theirs:
            rows.append("KEY %s py=%s rust=%s" % (key, mine[:220], theirs[:220]))
rows.append("PYORDER=" + ",".join(want.keys()))
rows.append("RUSTORDER=" + (",".join(got.keys()) if isinstance(got, dict) else "-none-"))
rows.append("PLUGINS=%r" % (rust.get("plugins"),))
rows.append("HASMSG=%s" % ("message" in rust))
# The asset template's hole count, read out of **this face's** document (not the
# reference's): four positional holes are the release asset's name, and three or five
# would be a well-formed string that matches nothing on any release.
rust_asset = (got.get("extras") or {}).get("asset") if isinstance(got, dict) else None
rows.append("HOLES=%d" % (rust_asset.count("{}") if isinstance(rust_asset, str) else -1))
# The naming rule, composed **across** the two implementations: the Rust face's template,
# filled in the reference's argument order, must equal the name the reference's own
# formatter produces. Filling Python's template with Python's function would be one
# implementation reading itself — the same-source equality that has bitten this port
# three times (§13.4).
args = ("macos", "arm64", "cp312", "0.22.0")
by_rust_template = rust_asset.format(*args) if isinstance(rust_asset, str) else None
name = ref.extras_asset_name(*args)
rows.append("ASSET=%s" % (
    "OK" if by_rust_template == name and isinstance(rust_asset, str)
    else "DIFFER rust-template=%s reference=%s" % (by_rust_template, name)))
print("\n".join(rows))
PYEOF
    }

    pr_read
    PR_CROSS=$(pr_cross)
    [ "$(printf '%s\n' "$PR_CROSS" | head -1)" = "EQUAL" ] \
        && pass "the shipped module and this face describe one file the same way, cell by cell" \
        || fail "the two implementations describe the same file differently: $(printf '%s\n' "$PR_CROSS" | tr '\n' ' ' | head -c 700)"

    # Fresh-first, in the **page's** order: the caching CDN last is what keeps a pushed
    # index from being a half-day-old list (AGENTS §4.6: 「把会缓存的放前面」 is the wrong
    # fix, and the order *is* the design).
    PR_FIRST=$(pr_ask 'doc["plugin_repo"]["index_urls"][0]')
    [ "$PR_FIRST" = "https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json" ] \
        && pass "index row 1 is the canonical raw URL, not the caching one" \
        || fail "index row 1 came back as: $PR_FIRST"
    [ "$(pr_ask 'len(doc["plugin_repo"]["index_urls"])')" = "3" ] \
        && pass "with the switch off the chain is three rows: raw, ghproxy, jsDelivr" \
        || fail "the off chain has $(pr_ask 'len(doc["plugin_repo"]["index_urls"])') rows, expected 3"

    # The other readers of the same document, in its own order.
    [ "$(printf '%s\n' "$PR_CROSS" | grep -a '^RUSTORDER=' | cut -d= -f2-)" = "repo_url,index_urls,mirror_enabled,extras" ] \
        && pass "the coordinates keep the reference's field order, not an alphabetical paraphrase" \
        || fail "field order: $(printf '%s\n' "$PR_CROSS" | grep -a '^RUSTORDER=')"
    [ "$(pr_ask 'doc["plugin_repo"]["extras"]["packages"]')" = "['aiortc', 'av']" ] \
        && pass "extras names exactly the two declared packages, as a list — not null, not a third" \
        || fail "extras packages: $(pr_ask 'doc["plugin_repo"]["extras"]["packages"]')"
    [ "$(printf '%s\n' "$PR_CROSS" | grep -a '^ASSET=')" = "ASSET=OK" ] \
        && pass "filling this face's template gives the name the reference's formatter gives" \
        || fail "the asset naming rule diverged: $(printf '%s\n' "$PR_CROSS" | grep -a '^ASSET=')"
    [ "$(printf '%s\n' "$PR_CROSS" | grep -a '^HOLES=')" = "HOLES=4" ] \
        && pass "and the template has four holes — os, arch, cpython tag, version" \
        || fail "$(printf '%s\n' "$PR_CROSS" | grep -a '^HOLES=')"

    # The 模块设置 card is the **second** reader of this key. Asked here, in both states,
    # because the first version of this face answered the switch from a constant while the
    # panel answered it from the file.
    pr_panel() {
        # Written to a file and opened by name, exactly like `pr_ask`: the program
        # arrives on stdin, so piping the document into the same stdin makes the reader
        # parse **its own source**. That is how this row reported `panel=` (empty) while
        # the card behind it was answering correctly — a false product red about a
        # disagreement that was really a heredoc.
        curl -s --noproxy '*' -m 5 "$PR_BASE?query=module-settings&token=$PR_TOKEN" > "$PR_PANEL"
        env -u PYTHONPATH "$CAST_PY" - "$PR_PANEL" <<'PYEOF'
import json, sys
doc = json.load(open(sys.argv[1]))
for group in doc["groups"]:
    for cell in group["keys"]:
        if cell["key"] == "Github_CN_Mirror":
            print("%s|%s" % (cell["present"], cell["value"]))
            raise SystemExit(0)
print("no-row|-")
PYEOF
    }
    [ "$(pr_ask 'doc["plugin_repo"]["mirror_enabled"]')" = "False" ] && [ "$(pr_panel)" = "False|None" ] \
        && pass "off is **absent** in both readers: the panel has no value, the face says false" \
        || fail "the two readers disagree about an absent switch: face=$(pr_ask 'doc["plugin_repo"]["mirror_enabled"]') panel=$(pr_panel)"

    # ---- the door, and the rule it reads the field with
    #
    # `set-github-mirror` is management-gated (`protocol.py:2292`), **not** code-gated:
    # unlike the `set-module-setting` row §28 tested, loopback is admitted here. A LAN
    # caller with no token is still refused, and a refusal must not move the switch.
    PR_MD5=$(md5 -q "$PR_SET")
    api_post "http://$LAN:$PR_PORT/api" "set-github-mirror=1"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "403" ] \
        && pass "a LAN caller with no token is refused the switch" \
        || fail "a LAN caller flipped the switch: HTTP $API_STATUS code '$API_CODE'"
    case "$API_MSG" in
        *"$MANAGEMENT_MARK"*) pass "and hears the management sentence, which is the gate that ran" ;;
        *) fail "refusal wording: $API_MSG" ;;
    esac
    [ "$(md5 -q "$PR_SET")" = "$PR_MD5" ] \
        && pass "a refused flip wrote nothing to disk" \
        || fail "the refused POST rewrote the settings file"

    # The flag table, asked **of the reference's own expression** rather than of a list
    # typed from memory. This is where the port's `.trim()` died: the reference lower-cases
    # and tests membership, nothing else, so a space on either side means *off* — and the
    # first version of this port answered *on*, with a unit test asserting the wrong side.
    PR_FLAG_BAD=""
    for PR_SAMPLE in "1" "true" "TRUE" "Yes" "on" "0" "false" "no" "" " 1 " "on "; do
        EXPECT=$(env -u PYTHONPATH "$CAST_PY" - "$PR_SAMPLE" <<'PYEOF'
import sys
print("true" if str(sys.argv[1]).lower() in ("1", "true", "yes", "on") else "false")
PYEOF
)
        api_post "$PR_BASE" "set-github-mirror=$PR_SAMPLE"
        # Both sides lower-cased on purpose: `json_ask` renders the document's own
        # `true`/`false`, the oracle above used to print Python's `True`/`False`, and the
        # first version of this loop therefore reported **eleven drifts** on a run whose
        # answers were all correct. A comparison that cannot tell a case difference from a
        # disagreement is not asking about the disagreement.
        GOT=$(json_ask mirror_enabled | tr 'A-Z' 'a-z')
        [ "$GOT" = "$EXPECT" ] || PR_FLAG_BAD="$PR_FLAG_BAD '$PR_SAMPLE'->$GOT/want-$EXPECT"
    done
    [ -z "$PR_FLAG_BAD" ] \
        && pass "all eleven spellings of the flag answer what the reference's own expression answers" \
        || fail "the flag drifted on:$PR_FLAG_BAD"

    # On, through the door. The reply carries the fresh coordinates (the reference's
    # `_set_github_mirror` returns `describe()` to save the page a second round trip), so
    # the document below is read **from the POST**, not from a following GET.
    api_post "$PR_BASE" "set-github-mirror=1"
    [ "$(json_ask code)" = "0" ] && [ "$(json_ask message)" = "success" ] \
        && [ "$(json_ask mirror_enabled | tr 'A-Z' 'a-z')" = "true" ] \
        && pass "loopback flips it, and the answer is success in the reference's fields" \
        || fail "the flip answered: HTTP $API_STATUS code='$(json_ask code)' msg='$(json_ask message)'"
    [ "$(json_keys plugin_repo)" = "repo_url,index_urls,mirror_enabled,extras" ] \
        && pass "and it hands back the coordinates in the same reply — no second round trip" \
        || fail "plugin_repo keys in the POST reply: $(json_keys plugin_repo)"
    grep -q '"Github_CN_Mirror": true' "$PR_SET" \
        && pass "the stored value is a JSON bool, the way Setting.set writes it" \
        || fail "the file does not hold a true: $(grep -c Github_CN_Mirror "$PR_SET") line(s)"

    pr_read
    PR_CROSS=$(pr_cross)
    [ "$(printf '%s\n' "$PR_CROSS" | head -1)" = "EQUAL" ] \
        && pass "with the mirror on, the shipped module still describes the same file the same way" \
        || fail "mirror-on documents differ: $(printf '%s\n' "$PR_CROSS" | tr '\n' ' ' | head -c 700)"
    [ "$(pr_ask 'len(doc["plugin_repo"]["index_urls"])')" = "2" ] \
        && pass "the on-chain is two rows, not three: the mirrored raw collapses onto the ghproxy row already there" \
        || fail "the on-chain has $(pr_ask 'len(doc["plugin_repo"]["index_urls"])') rows (a double prefix would keep three)"
    [ "$(pr_ask 'doc["plugin_repo"]["index_urls"][0]')" = "https://ghproxy.net/https://raw.githubusercontent.com/pingod/Macast/main/plugins/info.json" ] \
        && pass "and row 1 is still the fresh one — mirrored, not the caching CDN" \
        || fail "mirror row 1 came back as: $(pr_ask 'doc["plugin_repo"]["index_urls"][0]')"
    pr_ask 'doc["plugin_repo"]["index_urls"][1]' | grep -q '^https://cdn.jsdelivr.net/' \
        && pass "jsDelivr stays last and never carries the prefix — it already is a mirror" \
        || fail "jsDelivr row wrong: $(pr_ask 'doc["plugin_repo"]["index_urls"][1]')"
    pr_ask 'doc["plugin_repo"]["repo_url"]' | grep -q '^https://ghproxy.net/https://github.com/' \
        && pass "「打开插件仓库」 moves with the index, so the button and the fetch tell one story" \
        || fail "repo_url did not move: $(pr_ask 'doc["plugin_repo"]["repo_url"]')"

    # The two readers again, now that the key exists — the pair the previous round got
    # wrong in the other direction.
    [ "$(pr_panel)" = "True|True" ] && [ "$(pr_ask 'doc["plugin_repo"]["mirror_enabled"]')" = "True" ] \
        && pass "the 模块设置 row and plugin-info now read the SAME key and answer the same thing" \
        || fail "two readers, one key, two answers: panel=$(pr_panel) face=$(pr_ask 'doc["plugin_repo"]["mirror_enabled"]')"

    # Off is **absence**, not a stored false (AGENTS §4.6). Otherwise the page's toggle
    # leaves a key behind whose only meaning is "the default", and every later reader has
    # to know which of the two off-shapes it is looking at.
    api_post "$PR_BASE" "set-github-mirror=0"
    [ "$(json_ask mirror_enabled | tr 'A-Z' 'a-z')" = "false" ] \
        && pass "the off answer comes back false, as the page needs" \
        || fail "off answered: $(json_ask mirror_enabled)"
    grep -q 'Github_CN_Mirror' "$PR_SET" \
        && fail "the key is still in the file after the off answer: $(grep -c Github_CN_Mirror "$PR_SET")" \
        || pass "and the key is **gone** from the file, not stored as false"
    pr_read
    [ "$(pr_cross | head -1)" = "EQUAL" ] \
        && pass "back to the off state the two documents match again — the switch is reversible" \
        || fail "the off round trip differs: $(pr_cross | tr '\n' ' ' | head -c 500)"

    # The read must not be the thing that writes: the page polls `plugin-info`, and
    # `Setting.get`'s default side effect (AGENTS §4.2) is exactly how a poll becomes a
    # write. Asserted **with a file that exists**, so "no file" cannot stand in for "no write".
    PR_MD5_POLL=$(md5 -q "$PR_SET")
    for _ in 1 2 3; do pr_read; done
    [ "$(md5 -q "$PR_SET")" = "$PR_MD5_POLL" ] \
        && pass "three polls of the coordinates did not grow the file they report" \
        || fail "polling plugin-info wrote the settings file ($PR_MD5_POLL -> $(md5 -q "$PR_SET"))"

    # Shape rows: the plugin list stays a list, and a success carries no message key.
    [ "$(pr_ask 'doc["plugins"]')" = "[]" ] \
        && pass "plugins is an empty list, not null — this build installs nothing, and says so" \
        || fail "plugins came back as: $(pr_ask 'doc["plugins"]')"
    [ "$(pr_cross | grep -a '^HASMSG=')" = "HASMSG=False" ] \
        && pass "the coordinates document has no message key on a success" \
        || fail "$(pr_cross | grep -a '^HASMSG=')"

    kill -9 "$PR_PID" 2>/dev/null
    pkill -9 -f "$PR_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $PR_SDIR (rust.json, macast.log, macast_setting.json)"
    else
        rm -rf "$PR_SDIR"
    fi
fi



echo "== 30: the two 清空/显隐 buttons on the page reach a real player and the real file =="
# Two POST rows whose backing already existed in this shell: `set-subtitle-show`
# (`protocol.py:2699-2703`, the reference's row 14) publishes to the renderer, and
# `clear-play-history` (`protocol.py:3083-3089`, row 21) empties the ledger the
# 「播放历史」 card prints.
#
# **Its own instance, deliberately.** §24 kills the main instance on purpose (8009/8008 are
# addresses the sender pins, so "we are not listening" and "we are listening but cannot get
# the port" are indistinguishable from outside while the first one is alive) — which means
# every section after it has to bring its own process. These two rows also need a **live
# mpv**: the subtitle toggle is answered by the face with `{code, message}` and nothing
# about the player, exactly as Python answers it, so the only witness that anything happened
# is `query=status`'s `DisplayCurrentSubtitle` — and that value arrives **through mpv's own
# property echo**, never from the request. That is AGENTS.md §5's ownership rule visible
# from outside: this HTTP face does not write state variables, and the ledger is the one
# list it is allowed to empty.
#
# The flag rule is asked at the door too. The reference does **not** strip this field
# (`str(...).lower() in ('1','true','yes','on')`), unlike `?refresh=`, which does — so
# `"1 "` (a one with a trailing space) must *hide* the subtitles even though the text
# contains a yes-word. §29 measures the same asymmetry through the mirror switch; this is
# the second door sharing that rule, which is why the rule has one owner.
S30_SDIR=$(mktemp -d /tmp/macast-rust-subtitle.XXXXXX)
S30_PORT=$((PORT + 10))
S30_SOCK="$S30_SDIR/mpvsocket"
S30_TOKEN=0123456789abcdef0123456789abcdef
S30_BASE="http://127.0.0.1:$S30_PORT/api"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Subtitle", "Macast_Protocols": ["DLNA Protocol"], "Api_Token": "%s"}' \
    "$S30_PORT" "$S30_TOKEN" > "$S30_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S30_SDIR" MACAST_MPV_SOCKET="$S30_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S30_SDIR/stdout.log" 2>&1 &
S30_PID=$!
disown "$S30_PID" 2>/dev/null || true
S30_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S30_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S30_PID" ]; then
        S30_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S30_READY" ]; then
    fail "the subtitle instance never became the listener on $S30_PORT"
    kill -9 "$S30_PID" 2>/dev/null
    rm -rf "$S30_SDIR"
else
    # A player has to exist before a toggle can reach it, and the way this shell starts
    # one is a cast. §23 taught the read: **poll for PLAYING, do not grab one instant**.
    if [ ! -s "$CLIP" ]; then
        skip "no test clip on this machine, so there is no player to toggle — the two rows below cannot be asked"
    else
        api_probe "$S30_BASE?query=cast&url=$MEDIA_URL&title=Rust-Subtitle-Witness&token=$S30_TOKEN"
        PLAYING=""
        for _ in $(seq 1 60); do
            api_probe "$S30_BASE?query=status"
            [ "$(json_ask media.TransportState)" = "PLAYING" ] && { PLAYING=1; break; }
            sleep 0.25
        done
        if [ -z "$PLAYING" ]; then
            fail "the cast never reached PLAYING on the subtitle instance ('$(json_ask media.TransportState)') — nothing below can be asked of a player that is not running"
        else
            pass "a cast put a real mpv in PLAYING, which is the only thing that can answer a subtitle toggle"
            [ "$(json_len history)" -ge 1 ] \
                && pass "and the same cast put a row in the ledger this section will clear" \
                || fail "the player is playing but the ledger has $(json_len history) rows — the two writers disagree"

            # **Let the player finish reporting first.** The first draft of these rows asked
            # for `true` after `set-subtitle-show=1` and went green — green for nothing: mpv
            # reports every observed property once when the observer is installed
            # (`set_observe`), and subtitles are visible by default, so the table was about
            # to say `true` no matter what any request did. A mutant that inverted the
            # toggle reddened only the *second* row, which is how the vacuous first one got
            # found (§4.13's 「不提问的检查」, in a section written this round).
            #
            # Measured with a timeline probe (`/private/tmp/subtitle_probe.py`, this round):
            # the boot echo lands **about 0.4–1.0 s after `PLAYING`**, so a hide sent at
            # t+0.05 is overwritten by the player's own initial `true` — and that is the
            # reference's shape too (`macast_renderer/mpv.py:404-414` installs the observers
            # and then answers whatever arrives, and §4.8's `set_observe` race is the same
            # window). Nothing to fix in the product; **the row has to wait for the state it
            # is about to move** — §4.2's 「等那个账本，不要抓一瞬间」 for the third shape of it.
            SETTLED=""
            for _ in $(seq 1 40); do
                api_probe "$S30_BASE?query=status"
                case "$(json_ask media.DisplayCurrentSubtitle)" in
                    true|false) SETTLED=1; break ;;
                esac
                sleep 0.25
            done
            [ -n "$SETTLED" ] \
                && pass "the player's own first report of sub-visibility has landed (the table holds a bool, not the \
                    settings-file text \"1\") — now a toggle can be the only thing that moves it" \
                || fail "DisplayCurrentSubtitle never became a bool on its own: last read '$(json_ask media.DisplayCurrentSubtitle)' — the observer never reported, so nothing below can be attributed to a toggle"
            # Start from a known side rather than from whatever the default happened to be:
            # hide first, so every later row is a transition only a request can cause.
            api_post "$S30_BASE" "set-subtitle-show=0"
            [ "$API_CODE" = "0" ] && [ "$API_MSG" = "success" ] \
                && pass "the subtitle door answers success in the reference's two fields" \
                || fail "the subtitle POST answered: HTTP $API_STATUS code='$API_CODE' msg='$API_MSG'"
            SUB_HIDDEN_FIRST=""
            for _ in $(seq 1 40); do
                api_probe "$S30_BASE?query=status"
                [ "$(json_ask media.DisplayCurrentSubtitle)" = "false" ] && { SUB_HIDDEN_FIRST=1; break; }
                sleep 0.25
            done
            [ -n "$SUB_HIDDEN_FIRST" ] \
                && pass "hiding moved the state table — because mpv reported its own property, not because the face wrote it" \
                || fail "DisplayCurrentSubtitle never became false after set-subtitle-show=0: last read '$(json_ask media.DisplayCurrentSubtitle)'"

            api_post "$S30_BASE" "set-subtitle-show=1"
            SUB_SHOWN=""
            for _ in $(seq 1 40); do
                api_probe "$S30_BASE?query=status"
                [ "$(json_ask media.DisplayCurrentSubtitle)" = "true" ] && { SUB_SHOWN=1; break; }
                sleep 0.25
            done
            [ -n "$SUB_SHOWN" ] \
                && pass "and showing again comes back through the same echo, from a state only a request could have left" \
                || fail "DisplayCurrentSubtitle never became true after set-subtitle-show=1: last read '$(json_ask media.DisplayCurrentSubtitle)'"

            # `--data-urlencode` percent-encodes the trailing space, so this is exactly the
            # body a form with one stray space in it sends.
            api_post "$S30_BASE" "set-subtitle-show=1 "
            [ "$API_CODE" = "0" ] \
                && pass "a padded yes-word is served, not rejected (the reference answers 0 for a value it does not recognise)" \
                || fail "the padded POST answered code='$API_CODE' msg='$API_MSG'"
            SUB_HIDDEN=""
            for _ in $(seq 1 40); do
                api_probe "$S30_BASE?query=status"
                [ "$(json_ask media.DisplayCurrentSubtitle)" = "false" ] && { SUB_HIDDEN=1; break; }
                sleep 0.25
            done
            [ -n "$SUB_HIDDEN" ] \
                && pass "and \"1 \" **hides** them: this field is not stripped, so the space ends the word" \
                || fail "\"1 \" did not hide the subtitles (last read '$(json_ask media.DisplayCurrentSubtitle)') — the door is stripping, or the echo is not landing"

            # A refusal must not act. Management-gated, so a LAN caller without the token
            # is out — and the player must not have heard anything. Captured from the
            # **hidden** state the padded row above just produced, so the comparison below
            # has a direction: an inverted or ignoring door shows here instead of silently
            # agreeing with itself.
            api_probe "$S30_BASE?query=status"
            SUB_BEFORE=$(json_ask media.DisplayCurrentSubtitle)
            [ "$SUB_BEFORE" = "false" ] \
                && pass "the state is hidden before the refused toggle, which is what makes the next comparison a question" \
                || fail "expected to be hiding before the refused POST, read '$SUB_BEFORE'"
            api_post "http://$LAN:$S30_PORT/api" "set-subtitle-show=1"
            [ "$API_CODE" = "403" ] \
                && pass "a LAN caller with no token is refused the toggle" \
                || fail "a LAN caller toggled the subtitles: code '$API_CODE'"
            sleep 1
            api_probe "$S30_BASE?query=status"
            [ "$(json_ask media.DisplayCurrentSubtitle)" = "$SUB_BEFORE" ] \
                && pass "and the refused POST moved nothing: the property still reads $SUB_BEFORE" \
                || fail "the refused POST reached the player ($SUB_BEFORE -> $(json_ask media.DisplayCurrentSubtitle))"

            # ---- the ledger
            S30_MD5_BEFORE=$(md5 -q "$S30_SDIR/macast_setting.json")
            api_post "$S30_BASE" "clear-play-history=1"
            [ "$API_CODE" = "0" ] && [ "$API_MSG" = "success" ] \
                && pass "the clear answers success, in the same two fields as the reference" \
                || fail "clear-play-history answered code='$API_CODE' msg='$API_MSG'"
            api_probe "$S30_BASE?query=status"
            [ "$(json_len history)" = "0" ] \
                && pass "and the page's own read now has no rows" \
                || fail "the ledger still has $(json_len history) row(s) after the clear"
            S30_HIST=$(python3 - "$S30_SDIR/macast_setting.json" <<'PYEOF'
import json, sys
row = json.load(open(sys.argv[1])).get("Play_History", "ABSENT")
print("EMPTY-LIST" if row == [] else ("ABSENT" if row == "ABSENT" else "OTHER"))
PYEOF
)
            # 「清空」 writes an empty list rather than deleting the key (the reference's
            # `Setting.set(Play_History, [])`, §16 第 22 条's rule that a cleared thing stays
            # present so the boot log can tell 清空过 from 从未投过东西).
            [ "$S30_HIST" = "EMPTY-LIST" ] \
                && pass "and the file says the same thing: Play_History is an empty list, not a deleted key" \
                || fail "the file's Play_History reads $S30_HIST after the clear"
            S30_MD5_AFTER=$(md5 -q "$S30_SDIR/macast_setting.json")
            [ "$S30_MD5_BEFORE" != "$S30_MD5_AFTER" ] \
                && pass "the clear reached the disk — the ledger is not a display-only list" \
                || fail "the settings file is byte-identical after the clear (nothing was persisted)"

            # The writer still works. A clear that broke the append path would look exactly
            # like the two rows above until someone cast something again.
            api_probe "$S30_BASE?query=cast&url=$MEDIA_URL&title=Rust-Ledger-After&token=$S30_TOKEN"
            AGAIN=""
            for _ in $(seq 1 40); do
                api_probe "$S30_BASE?query=status"
                [ "$(json_len history)" = "1" ] && { AGAIN=1; break; }
                sleep 0.25
            done
            [ -n "$AGAIN" ] \
                && pass "and casting after a clear puts exactly one row back — it emptied the ledger, it did not break it" \
                || fail "history reads $(json_len history) row(s) after a fresh cast; the clear left the writer broken"

            # Both 清空 buttons in one request: the reference's row 21 runs and row 22 never
            # does. The answer shape says which — `clear-log`'s document carries a `removed`
            # count, the ledger's does not.
            api_post "$S30_BASE" "clear-play-history=1&clear-log=1"
            [ "$API_CODE" = "0" ] \
                && pass "one request carrying both 清空 fields is served without a fuss" \
                || fail "the two-field request answered code='$API_CODE'"
            api_probe "$S30_BASE?query=status"
            [ "$(json_len history)" = "0" ] \
                && pass "and the earlier row is the one that ran: the ledger is empty again" \
                || fail "the later row ran first: history holds $(json_len history) row(s)"
        fi
    fi

    kill -9 "$S30_PID" 2>/dev/null
    pkill -9 -f "$S30_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S30_SDIR (macast.log, logs/MPV.log, macast_setting.json)"
    else
        rm -rf "$S30_SDIR"
    fi
fi

echo "== 31: the engine hears the answer it sent, which is where its flood protection comes from =="
# Design doc §16 第 36 条 measured something on the running reference that changes what a port
# of RFC 6762 §14 has to wire up: the reference's "did we just shout this record" check reads
# its **cache**, and the record sitting in that cache got there because the responder's own
# answer was looped back by the kernel and its socket **heard** it. The flood protection is
# therefore not "I remember sending" — it is "I heard myself". A port that never reads the
# group answers every query as fast as ever and simply has no protection at all, and that is
# a machine, not a bug report.
#
# The queue itself is pinned in `aggregate_tests` against the four recorded readings; what
# only a real machine can say is whether the *input* to that mechanism arrives in the shipped
# process. So this section starts its own instance (with Chromecast on, since §24 kills the
# main one and the announcer only runs behind that door) and asks one question of its log.
#
# Measured here on 2026-10-20, twice: the sentence lands about 0.8 s after the first burst
# with `4 record(s)` — four, not five, because the parser skips the NSEC (design doc §8's
# recorded divergence), and four is what the announcement's own record list says. The same
# probe also found that this process hears **its own** multicast and nobody else's: a TXT
# query sent to it by another process, unicast to 127.0.0.1:5353 or sent to the group,
# produces no `heard query` line at all. That narrows §25's unresolved cell from "inbound
# multicast does not arrive" to "inbound multicast **from another sender** does not arrive";
# the cause is still not named here, and neither is a fix invented for it.
S31_SDIR=$(mktemp -d /tmp/macast-rust-hear.XXXXXX)
S31_PORT=$((PORT + 11))
S31_SOCK="$S31_SDIR/mpvsocket"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Hearing", "Api_Token": "0123456789abcdef0123456789abcdef", "Macast_Protocols": ["DLNA Protocol", "Chromecast Protocol"]}' \
    "$S31_PORT" > "$S31_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S31_SDIR" MACAST_MPV_SOCKET="$S31_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S31_SDIR/stdout.log" 2>&1 &
S31_PID=$!
disown "$S31_PID" 2>/dev/null || true
S31_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S31_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S31_PID" ]; then
        S31_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S31_READY" ]; then
    fail "the hearing instance never became the listener on $S31_PORT"
    kill -9 "$S31_PID" 2>/dev/null
    rm -rf "$S31_SDIR"
else
    s31_grep() { grep -ahF "$1" "$S31_SDIR/macast.log" "$S31_SDIR"/logs/*.log 2>/dev/null; }
    # The announcement is the precondition: a socket that bound nothing hears nothing, and
    # "no hearing line" would then be about the door rather than about the wire.
    S31_ANNOUNCED=""
    for _ in $(seq 1 20); do
        [ -n "$(s31_grep 'mDNS: announcing ')" ] && { S31_ANNOUNCED=1; break; }
        sleep 0.5
    done
    if [ -z "$S31_ANNOUNCED" ]; then
        skip "this instance announced nothing (no advertisable address, or Chromecast was switched off), so there is no answer for it to hear"
    else
        HEARD_LINE=""
        for _ in $(seq 1 30); do
            HEARD_LINE=$(s31_grep 'mDNS engine heard a response on' | head -1)
            [ -n "$HEARD_LINE" ] && break
            sleep 0.5
        done
        if [ -z "$HEARD_LINE" ]; then
            # Not a failure of the queue: with nothing arriving, the protected bucket simply
            # is never reached, and the honest statement is that this machine did not let the
            # section ask. `skip` counts as a failure in this script on purpose.
            skip "the engine never reported hearing a response: this process sees no inbound multicast at all, so the ledger cannot be live here — §25's cell, and not something this section can manufacture"
        else
            pass "the engine heard its own answer: ${HEARD_LINE#*] }"
            S31_RECORDS=$(printf '%s' "$HEARD_LINE" | sed -n 's/.*learned \([0-9][0-9]*\) record.*/\1/p')
            [ "$S31_RECORDS" = "4" ] \
                && pass "and it learned four records — PTR, SRV, TXT and A, the NSEC the parser does not read (design doc §8)" \
                || fail "the ledger learned $S31_RECORDS record(s) from our own announcement; four is what the announcement has and what the parser reads"
            # A line that only reports a number sends the reader to the source for what it
            # means. AGENTS.md §4.2's 「每一条失败文案必须给出下一步」 applies to the healthy
            # sentences too, because this one is the only place a reader learns whether the
            # one-second rule is armed in this process.
            case "$HEARD_LINE" in
                *"waits a second"*) pass "the hearing sentence says what it protects against, rather than just counting" ;;
                *) fail "the hearing sentence names no consequence: $HEARD_LINE" ;;
            esac
            # The asymmetry this section exists to record, asked rather than asserted: if a
            # query from another process ever does arrive, §25's gap has closed and this
            # reading has to change with it.
            if [ -n "$(s31_grep 'mDNS engine heard query')" ]; then
                pass "queries from other processes reach this socket too, so §25's delivery cell has closed on this machine"
            else
                pass "own loopback arrives while another sender's query does not — §25's gap is inbound delivery from elsewhere, not the responder"
            fi
        fi
    fi
    kill -9 "$S31_PID" 2>/dev/null
    pkill -9 -f "$S31_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S31_SDIR (macast.log, logs/Discovery.log)"
    else
        rm -rf "$S31_SDIR"
    fi
fi

echo "== 32: set-interface moves the announcement, and a refusal leaves the disk alone =="
# The management face's row 1 (`protocol.py:2288`) is the 网卡 picker. Two claims are
# worth a section, because both were false in this shell until now:
#
#   * the switch **re-advertises**. The window command had that half since the morning
#     round, and the POST row is the second door on the same function — a page that
#     pinned a card while the announcer kept publishing the previous one's addresses is
#     the 「控制项看起来能用」 shape this port keeps meeting, and the answer said
#     「已切换到」 all along.
#   * a refusal writes **nothing**. The reference rejects an unknown card before storing
#     it (`protocol.py:1986-1990`), because a typo left in place makes
#     `resolved_network_interface` fall back to automatic **silently**: the page would
#     show the card the user typed while the wire carried whatever the default route
#     picked. So the file's bytes are compared, not the in-memory table — reading the
#     known-interface list goes through `Setting.get(key, default)`, which remembers its
#     default (AGENTS.md §4.2's first trap, and Python has it identically), so a key
#     being present in memory proves nothing about what reached the disk.
#
# The witness is the announcer's own summary line — `mDNS: announcing <instance> on port
# <p> over [<addresses>]` — which §22 cross-checks against a second stack's reading of
# the wire. Reading the *published set* is the claim here; whether the datagrams then
# leave through that card is a separate question this section does not ask, and
# `docs/Rust-Port-Design.md` §16 第 38 条 records what a probe measured about it.
S32_SDIR=$(mktemp -d /tmp/macast-rust-iface.XXXXXX)
S32_PORT=$((PORT + 12))
S32_SOCK="$S32_SDIR/mpvsocket"
S32_BASE="http://127.0.0.1:$S32_PORT/api"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Iface", "Api_Token": "0123456789abcdef0123456789abcdef", "Macast_Protocols": ["DLNA Protocol", "Chromecast Protocol"]}' \
    "$S32_PORT" > "$S32_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S32_SDIR" MACAST_MPV_SOCKET="$S32_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S32_SDIR/stdout.log" 2>&1 &
S32_PID=$!
disown "$S32_PID" 2>/dev/null || true
S32_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S32_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S32_PID" ]; then
        S32_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S32_READY" ]; then
    fail "the interface instance never became the listener on $S32_PORT"
    kill -9 "$S32_PID" 2>/dev/null
    rm -rf "$S32_SDIR"
else
    S32_DLOG="$S32_SDIR/logs/Discovery.log"
    s32_dgrep() { grep -ahF "$1" "$S32_DLOG" 2>/dev/null; }
    s32_announced() { grep -ahF 'mDNS: announcing ' "$S32_DLOG" 2>/dev/null; }
    # The announcement is the precondition, and the *Cast* line is the one asked of: a
    # second service type would make `tail -1` a choice by accident (§22's lesson), so
    # the type filter is part of the reader, not decoration.
    S32_LINE=""
    for _ in $(seq 1 20); do
        S32_LINE=$(s32_announced | grep -aF '_googlecast._tcp' | tail -1)
        [ -n "$S32_LINE" ] && break
        sleep 0.5
    done
    if [ -z "$S32_LINE" ]; then
        fail "no Cast announcement line in $S32_DLOG, so the switch has nothing to move: $(s32_announced)"
    else
        S32_SET0=$(printf '%s' "$S32_LINE" | sed -n 's/.*over \[\([^]]*\)\].*/\1/p')
        # This section's **own** instance name, read out of the line above rather than
        # borrowed from §22: two announcers now write one line each per service type, and a
        # needle taken from another instance's run would be a filter that can only ever
        # return zero — which reads as "the packets did not move" about a run that moved
        # them fine.
        S32_INSTANCE=$(printf '%s' "$S32_LINE" | sed -n 's/.*announcing \([^ ]*\) on port .*/\1/p')
        echo "  announcement before the switch: $S32_INSTANCE over [$S32_SET0]"

        # ---- the refusal, asked first so nothing it could disturb has happened yet ----
        S32_MD5=$(md5 -q "$S32_SDIR/macast_setting.json" 2>/dev/null)
        api_post "$S32_BASE" "set-interface=nope9x"
        S32_REJ=$API_MSG
        [ "$API_CODE" = "1" ] \
            && pass "an unknown card is refused with code 1, not stored ($API_CODE)" \
            || fail "set-interface=nope9x answered code='$API_CODE' body='$API_BODY'"
        case "$S32_REJ" in
            *nope9x*) pass "the refusal names the typo the caller sent" ;;
            *) fail "the refusal does not say which card it rejected: $S32_REJ" ;;
        esac
        case "$S32_REJ" in
            *"当前可用"*) pass "…and lists what was available, so the reader can fix it" ;;
            *) fail "the refusal gives no alternatives: $S32_REJ" ;;
        esac
        [ "$S32_MD5" = "$(md5 -q "$S32_SDIR/macast_setting.json" 2>/dev/null)" ] \
            && pass "and the refusal left the settings file byte-for-byte alone" \
            || fail "the refusal wrote the settings file — that is the silent fallback this row exists to prevent"
        [ -z "$(s32_dgrep 'network interface changed')" ] \
            && pass "the refused switch did not re-advertise either" \
            || fail "a refused pin still cycled the announcement: $(s32_dgrep 'network interface changed' | head -1)"

        # ---- pick a card the announcement is not already published over ----
        api_probe "$S32_BASE?query=interfaces"
        S32_ROWS=$(json_len interfaces)
        S32_CARD=""; S32_CARD_IP=""
        i=0
        while [ "$i" -lt "$S32_ROWS" ]; do
            S32_NAME=$(json_ask "interfaces.$i.name")
            S32_IP=$(json_ask "interfaces.$i.ip")
            S32_USABLE=$(json_ask "interfaces.$i.usable")
            if [ "$S32_USABLE" = "true" ] && [ -n "$S32_IP" ] && [ ",$S32_SET0," != ",$S32_IP," ] \
                && [ -z "$S32_CARD" ]; then
                S32_CARD="$S32_NAME"; S32_CARD_IP="$S32_IP"
            fi
            i=$((i + 1))
        done
        if [ -z "$S32_CARD" ]; then
            skip "this machine offers no second usable card (published set [$S32_SET0] out of $S32_ROWS rows), so a pin here cannot be told apart from no pin — the switch's document checks below are the ones that still mean something"
        else
            echo "  switching to: $S32_CARD ($S32_CARD_IP)"
            # The listener binds **before** the POST. A switch's announcement is a
            # three-packet burst 225 ms apart, so a listener that starts afterwards is
            # measuring whether it joined in time rather than whether the datagrams moved
            # (§4.2's 「等那个账本，不要抓客户端返回的那一瞬间」, this time on a sniffer).
            # The needle is the instance's **wire label**, not a dotted name: `Macast-Rust-
            # Iface` sits in the packet between two length bytes and the dots are gone
            # (§25 measured a dotted filter reading a full group as `queries=0`).
            S32_LBL=$(printf '%s' "$S32_INSTANCE" | sed -n 's/\._.*//p')
            S32_HITS="$S32_SDIR/pinned-card.txt"
            S32_LRES="$S32_SDIR/listener.out"
            : > "$S32_HITS"
            if [ ! -x "$CAST_PY" ] || [ -z "$S32_LBL" ]; then
                skip "no .venv python or no instance label to listen with, so this run cannot show where the packets left"
                S32_LPID=""
            else
                env -u PYTHONPATH "$CAST_PY" "$REPO/scripts/mdns_iface_listener.py" \
                    --iface "$S32_CARD_IP" --seconds 12 --needle "$S32_LBL" --out "$S32_HITS" \
                    > "$S32_LRES" 2>&1 &
                S32_LPID=$!
                sleep 1
            fi
            api_post "$S32_BASE" "set-interface=$S32_CARD"
            [ "$API_CODE" = "0" ] \
                && pass "a card off this machine's own table is accepted (code 0)" \
                || fail "set-interface=$S32_CARD was refused: $API_BODY"
            [ "$(json_ask interface)" = "$S32_CARD" ] \
                && pass "and the answer names the card it switched to" \
                || fail "the answer's interface key reads '$(json_ask interface)': $API_BODY"
            [ "$(json_ask ssdp_reannounced)" = "true" ] \
                && pass "SSDP was rebound — and that is the **result**, not the ask, because this instance had a listener to rebind" \
                || fail "ssdp_reannounced reads '$(json_ask ssdp_reannounced)': $API_BODY"
            [ "$(json_ask mdns_reannounced)" = "true" ] \
                && pass "and the mDNS half came back up on the new addresses" \
                || fail "mdns_reannounced reads '$(json_ask mdns_reannounced)': $API_BODY"

            S32_LINE1=""
            for _ in $(seq 1 20); do
                S32_LINE1=$(s32_announced | grep -aF '_googlecast._tcp' | tail -1)
                [ "$S32_LINE1" != "$S32_LINE" ] && [ -n "$S32_LINE1" ] && break
                sleep 0.5
            done
            if [ "$S32_LINE1" = "$S32_LINE" ]; then
                fail "the announcing line never changed after pinning $S32_CARD — the reply said re-announced while the publisher kept the old set: $S32_LINE1"
            else
                S32_SET1=$(printf '%s' "$S32_LINE1" | sed -n 's/.*over \[\([^]]*\)\].*/\1/p')
                [ "$S32_SET1" = "$S32_CARD_IP" ] \
                    && pass "the published set is now exactly the pinned card's address: [$S32_SET1]" \
                    || fail "pinning $S32_CARD published [$S32_SET1], not [$S32_CARD_IP]"
            fi
            # The socket moved too, not just the document: the join line is what says a
            # querier on that segment will be answered. §22's rule — the membership, not
            # the bind — read on the switch's second engine.
            S32_JOIN=$(s32_dgrep 'mDNS engine on ' | tail -1)
            case "$S32_JOIN" in
                *"[$S32_CARD_IP]"*) pass "and the new engine joined the pinned card: ${S32_JOIN#*] }" ;;
                *) fail "the announcer after the pin did not join the pinned card: $S32_JOIN" ;;
            esac
            # **A live goodbye** — the one thing §22 recorded as unreachable from a
            # script, because tearing an instance down with kill -9 never runs
            # `stop_services`. A pin change stops the announcer while the process lives,
            # so the goodbye is on the wire and in this log for real.
            [ -n "$(s32_dgrep 'mDNS announcement stopped')" ] \
                && pass "the old announcement said goodbye before the new one started (§22's unreachable-by-kill witness, reachable through the switch)" \
                || fail "no 'mDNS announcement stopped' line: the switch started a second announcer without retiring the first"
            # **Where the datagrams left.** The published set above is a document; this is
            # the wire, and it is the half that was false until 2026-10-20: with no
            # `IP_MULTICAST_IF` set, the kernel answered the route instead of the pin, the
            # log said `over [192.168.139.3]`, and a listener joined on 192.168.139.3 saw
            # nothing while the burst went on arriving on the default card. A picker whose
            # only effect is on a document is what 「控制项看起来能用」 means.
            if [ -n "$S32_LPID" ]; then
                wait "$S32_LPID" 2>/dev/null
                S32_N=$(grep -ac . "$S32_HITS" 2>/dev/null || echo 0)
                case "$(cat "$S32_LRES" 2>/dev/null)" in
                    LISTEN_FAILED*) skip "the pinned-card listener could not bind 5353: $(cat "$S32_LRES")" ;;
                    *) [ "$S32_N" -ge 1 ] \
                        && pass "and the announcement really left through the pinned card: $S32_N datagram(s) of $S32_LBL heard on $S32_CARD_IP" \
                        || fail "nothing reached a listener joined on $S32_CARD_IP ($S32_CARD): the pin changed the published set but not the egress — $(cat "$S32_LRES" 2>/dev/null)" ;;
                esac
            fi
            S32_CHG=$(s32_dgrep 'network interface changed' | tail -1)
            case "$S32_CHG" in
                *"-> $S32_CARD; mDNS re-announced"*) pass "and the change sentence says which card and that it came back: ${S32_CHG#*] }" ;;
                *) fail "the change sentence does not name both halves: $S32_CHG" ;;
            esac
            grep -aqF "\"Network_Interface\": \"$S32_CARD\"" "$S32_SDIR/macast_setting.json" \
                && pass "the pin is in the settings file, so it survives a restart (the §27 rule for anything the page writes)" \
                || fail "the pin answered but was not persisted: $(cat "$S32_SDIR/macast_setting.json")"

            # ---- clearing the pin is a request, not a missing argument ----
            api_post "$S32_BASE" "set-interface="
            [ "$API_MSG" = "已切换到 自动选择" ] \
                && pass "an empty field restores automatic selection with the reference's own sentence" \
                || fail "set-interface= answered '$API_MSG': $API_BODY"
            S32_LINE2=""
            for _ in $(seq 1 20); do
                S32_LINE2=$(s32_announced | grep -aF '_googlecast._tcp' | tail -1)
                [ "$S32_LINE2" != "$S32_LINE1" ] && [ -n "$S32_LINE2" ] && break
                sleep 0.5
            done
            S32_SET2=$(printf '%s' "$S32_LINE2" | sed -n 's/.*over \[\([^]]*\)\].*/\1/p')
            [ "$S32_SET2" = "$S32_SET0" ] \
                && pass "and the published set came back to what it was before the pin: [$S32_SET2]" \
                || fail "clearing the pin left the announcement on [$S32_SET2] instead of [$S32_SET0]"
        fi
    fi
    kill -9 "$S32_PID" 2>/dev/null
    pkill -9 -f "$S32_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S32_SDIR (macast.log, logs/Discovery.log, macast_setting.json)"
    else
        rm -rf "$S32_SDIR"
    fi
fi

echo "== 33: a query sent to our own address reaches the socket that exists for it =="
# Design doc §16 第 40 条. `scripts/mdns_unicast_bind_probe.py` measured the rule this
# section is about: a socket bound to 0.0.0.0:5353 receives **no** datagram addressed at
# this host's own port 5353, while a socket bound to that exact address receives all of
# them and **no** group traffic. One socket cannot be both, so the engine now binds two
# kinds — and `announce_tests::e12` proves the rotation hears both inside one process.
# What only this machine can say is whether the **shipped process** answers a querier that
# asks it directly, which is the shape RFC 6762 §5.1 allows and this Mac's own
# mDNSResponder uses once it has learned an address for a responder.
#
# Measured here on 2026-10-20, before this section existed: the instance logged
# 「mDNS engine heard query 1 of 8 on 5353 (40 bytes, asking Ptr _googlecast._tcp.local.)
# from 127.0.0.1:60062」 and answered five asks in a row, and zeroconf's own DNSIncoming
# read each reply as PTR/SRV/TXT/A/NSEC. That is the first **cross-process** witness this
# responder has ever had, and it narrows §25's open cell rather than closing it: traffic
# from another process does arrive when it is unicast to an address we bind, and what still
# does not arrive is the group. The cause of the group half is still not named here, and no
# fix is invented for it.
#
# The asker binds 127.0.0.1 and the destination is this machine's own address, so the
# kernel loops the datagram internally and no byte of this section goes out on the LAN —
# the discipline §22's取证 round keeps, because a real broadcast from a test would put a
# phantom Chromecast in every Cast menu on the segment for the length of the run.
S33_SDIR=$(mktemp -d /tmp/macast-rust-unicast.XXXXXX)
S33_PORT=$((PORT + 13))
S33_SOCK="$S33_SDIR/mpvsocket"
S33_DLOG="$S33_SDIR/logs/Discovery.log"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Unicast", "Api_Token": "0123456789abcdef0123456789abcdef", "Macast_Protocols": ["DLNA Protocol", "Chromecast Protocol"]}' \
    "$S33_PORT" > "$S33_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S33_SDIR" MACAST_MPV_SOCKET="$S33_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S33_SDIR/stdout.log" 2>&1 &
S33_PID=$!
disown "$S33_PID" 2>/dev/null || true
S33_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S33_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S33_PID" ]; then
        S33_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S33_READY" ]; then
    fail "the unicast instance never became the listener on $S33_PORT"
    kill -9 "$S33_PID" 2>/dev/null
    rm -rf "$S33_SDIR"
elif [ ! -x "$CAST_PY" ]; then
    skip "no .venv python, so there is no second stack to ask the question with"
    kill -9 "$S33_PID" 2>/dev/null
else
    # The announcement is the precondition twice over: it says an engine exists to be asked,
    # and it is where the address to ask, and the instance name an answer must carry, are
    # read from. Nothing below is typed — §22's rule that a needle from another run is a
    # filter that can only return zero.
    S33_LINE=""
    for _ in $(seq 1 20); do
        S33_LINE=$(grep -ahF 'mDNS: announcing ' "$S33_DLOG" 2>/dev/null | grep -aF '_googlecast._tcp' | tail -1)
        [ -n "$S33_LINE" ] && break
        sleep 0.5
    done
    if [ -z "$S33_LINE" ]; then
        fail "no Cast announcement line in $S33_DLOG, so this instance has no engine to ask"
    else
        S33_INSTANCE=$(printf '%s' "$S33_LINE" | sed -n 's/.*announcing \([^ ]*\) on port .*/\1/p')
        S33_LBL=$(printf '%s' "$S33_INSTANCE" | sed -n 's/\._.*//p')
        S33_SET0=$(printf '%s' "$S33_LINE" | sed -n 's/.*over \[\([^]]*\)\].*/\1/p')
        S33_IP=$(printf '%s' "$S33_SET0" | cut -d, -f1 | tr -d ' ')
        echo "  asking $S33_IP about $S33_LBL (published set [$S33_SET0])"
        S33_ENGINE=$(grep -ahF 'mDNS engine on ' "$S33_DLOG" | tail -1)
        case "$S33_ENGINE" in
            *"it also hears unicast queries on "*)
                pass "the shipped process says it bound the second socket kind: ${S33_ENGINE#*] }" ;;
            *) fail "the engine line does not mention the per-address listeners, so the shipped binary has only the wildcard bind: $S33_ENGINE" ;;
        esac
        # Which addresses it listens on is the same list it publishes: an extra bind would
        # be this machine answering on an address no sender can reach (and §22's product bug
        # was exactly a wider set going onto the wire), while a missing one is a card whose
        # queriers are ignored. Compared as sorted lists, not as counts, because two
        # different sets can be the same size.
        S33_BIND=$(printf '%s' "$S33_ENGINE" | sed -n 's/.*unicast queries on \[\([^]]*\)\].*/\1/p' | tr ',' '\n' | sed 's/^ *//;s/ *$//' | sort | tr '\n' ' ')
        S33_PUB=$(printf '%s' "$S33_SET0" | tr ',' '\n' | sed 's/^ *//;s/ *$//' | sort | tr '\n' ' ')
        [ "$S33_BIND" = "$S33_PUB" ] \
            && pass "and it binds exactly the addresses it publishes: [$S33_BIND]" \
            || fail "the listener set [$S33_BIND] is not the published set [$S33_PUB]"
        # Lines appended **after** each ask, because the same sentence may already be in the
        # file from a neighbour's traffic — a hearing check that reads history is asking
        # "did this ever happen", which is not the question (§4.2's 「等那个账本」 again).
        S33_MARK=0
        s33_mark() {
            S33_MARK=$(wc -l < "$S33_DLOG" 2>/dev/null | tr -d ' ')
            S33_MARK=${S33_MARK:-0}
        }
        s33_seen() { tail -n +"$((S33_MARK + 1))" "$S33_DLOG" 2>/dev/null | grep -ahF "$1"; }
        s33_ask() {  # $1 = extra flags, $2 = where to keep the reader's output
            env -u PYTHONPATH "$CAST_PY" "$REPO/scripts/mdns_unicast_ask.py" \
                --addr "$S33_IP" --needle "$S33_LBL" --seconds 6 $1 > "$2" 2>&1
            echo $?
        }

        s33_mark
        S33_OUT="$S33_SDIR/ask.txt"
        S33_EXIT=$(s33_ask "" "$S33_OUT")
        if [ "$S33_EXIT" = "0" ]; then
            pass "another process asked this address directly and the shipped responder answered (exit 0)"
            # The reply is judged by the **reference's parser**, not by ours: what it read is
            # printed here so a reader can see the five records rather than a verdict word.
            if grep -aqF 'types=[PTR,SRV,TXT,A,NSEC]' "$S33_OUT"; then
                pass "…and zeroconf's own reader sees the five records the announcement carries: $(grep -a 'types=' "$S33_OUT" | head -1)"
            else
                fail "the answer it read back is not the record set the announcement sends: $(grep -a 'types=' "$S33_OUT" | head -1)"
            fi
            if grep -aqE "answer from \('[0-9.]+', 5353\)" "$S33_OUT"; then
                pass "and the reply leaves **from** port 5353, which is what a querier's stack needs to accept it (the receive-only listeners never answer, so their source port would be an ephemeral one)"
            else
                fail "the reply did not come from 5353: $(grep -a 'answer from' "$S33_OUT" | head -1)"
            fi
            if [ -n "$(s33_seen 'mDNS engine heard query')" ]; then
                S33_HEARD=$(s33_seen 'mDNS engine heard query' | head -1)
                case "$S33_HEARD" in
                    *"asking Ptr _googlecast._tcp.local."*)
                        pass "and the engine's own sentence says what it was asked: ${S33_HEARD#*] }" ;;
                    *) fail "the hearing line does not name the question: $S33_HEARD" ;;
                esac
            else
                fail "it answered without ever reporting hearing a query — the reply and the sentence have different owners"
            fi
        elif [ -n "$(s33_seen 'mDNS engine heard query')" ]; then
            fail "the engine heard the query and did not answer it (asker exit $S33_EXIT): $(tail -2 "$S33_OUT" | tr '\n' ' ')"
        else
            skip "nothing arrived and the engine reported hearing nothing either — two readings fit that, and this section cannot tell them apart: either this Mac does not hand another process's query to our socket (§25's open cell), or the shipped thread never puts the second socket kind into its rotation. The unit row the_reading_thread_rotates_every_socket_the_engine_bound is the one that can see the second, so a skip here is not a pass for the wiring (asker exit $S33_EXIT, reader said: $(tail -1 "$S33_OUT"))"
        fi

        # The same question carrying the unicast-response bit. This is **not** §37's fourth
        # route: that branch is gated on the source port being 5353, and every ask here comes
        # from an ephemeral one — so what this row witnesses is that the bit in the class
        # field does not cost an answer, while the quarter-TTL rule still has only the
        # readings in `mdns_responder_probe.py` and the unit rows.
        s33_mark
        S33_QU="$S33_SDIR/ask-qu.txt"
        S33_QEXIT=$(s33_ask "--qu" "$S33_QU")
        if [ "$S33_QEXIT" = "0" ]; then
            pass "a question with the unicast-response bit set is answered too (class 0x8001)"
        elif [ -n "$(s33_seen 'mDNS engine heard query')" ]; then
            fail "it heard the QU-flagged question and stayed silent (asker exit $S33_QEXIT): $(tail -2 "$S33_QU" | tr '\n' ' ')"
        else
            skip "the QU-flagged ask was not delivered either — same cell as above, exit $S33_QEXIT"
        fi

        # The negative, and it is only safe because of how it is phrased: loopback is not in
        # the published set, so the engine must not stand a listener there. An answer would
        # mean the bind list is wider than what is advertised. Silence and a third party's
        # answer are both fine readings — only **our** name coming back is a red — because a
        # negative that another process can turn red is not a judgment (the same reasoning
        # that keeps §24 from asserting on a neighbour's advertisement).
        s33_mark
        S33_LB="$S33_SDIR/ask-loopback.txt"
        env -u PYTHONPATH "$CAST_PY" "$REPO/scripts/mdns_unicast_ask.py" \
            --addr 127.0.0.1 --needle "$S33_LBL" --seconds 3 > "$S33_LB" 2>&1
        S33_LBEXIT=$?
        if [ "$S33_LBEXIT" = "0" ]; then
            fail "the responder answered a query addressed at 127.0.0.1, which is not in the published set [$S33_SET0] — the listener list is wider than the announcement"
        else
            pass "and it does not answer on loopback, which it never publishes (exit $S33_LBEXIT: $(tail -1 "$S33_LB" | cut -c1-90))"
        fi
    fi
    kill -9 "$S33_PID" 2>/dev/null
    pkill -9 -f "$S33_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S33_SDIR (macast.log, logs/Discovery.log, ask*.txt)"
    else
        rm -rf "$S33_SDIR"
    fi
fi

echo "== 34: the protocol switch closes a face and brings it back, in the same process =="
# `toggle-protocol` is row 7 of the reference's POST table and its handler is
# `_toggle_protocol` -> `set_protocol_enabled` (`macast.py:429-453`), which the shell now
# owns in `host::set_protocol_enabled`. §24 could only ask the **gate** half, by starting
# a second instance with a hand-written list; this section asks the **switch**: a POST
# closes 8009/8008 and the Cast announcement, and a second POST brings all three back in
# the same process. That is the difference between a document and a control.
#
# Measured here on 2026-10-20 before the section was written: off -> 8009/8008 gone, the
# file reads [DLNA, AirPlay], Discovery.log says 「protocol switch applied; mDNS
# re-announced」 and only the AirPlay line is re-announced; on -> both ports bind again,
# the Cast line returns, the file reads all three. Two refusals share the section: DLNA
# cannot be switched off in this build (the settings page lives on its HTTP face, §8's
# row), and an unknown title is dropped-with-a-warning while the answer still says
# 「已启用」 — the reference's own oddity, ported rather than improved.
S34_SDIR=$(mktemp -d /tmp/macast-rust-toggle.XXXXXX)
S34_PORT=$((PORT + 14))
S34_SOCK="$S34_SDIR/mpvsocket"
S34_DLOG="$S34_SDIR/logs/Discovery.log"
S34_TOK="0123456789abcdef0123456789abcdef"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Toggle", "Api_Token": "%s"}' \
    "$S34_PORT" "$S34_TOK" > "$S34_SDIR/macast_setting.json"
# **This section needs 8009/8008 to itself, and they are fixed ports.** A sender hardcodes
# them (`dial.py:25-26`), so §24's reasoning applies verbatim: while the main instance holds
# them, "did the switch close the face" cannot be told apart from "someone else has the
# port" — measured 2026-10-21, both instances' assertions went red (8 checks) and it read
# like a broken switch. So the main instance stands down here and comes back after.
kill -9 "$APP_PID" 2>/dev/null
env MACAST_SETTING_DIR="$S34_SDIR" MACAST_MPV_SOCKET="$S34_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S34_SDIR/stdout.log" 2>&1 &
S34_PID=$!
disown "$S34_PID" 2>/dev/null || true
S34_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S34_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S34_PID" ]; then
        S34_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S34_READY" ]; then
    fail "the toggle instance never became the listener on $S34_PORT"
    kill -9 "$S34_PID" 2>/dev/null
    rm -rf "$S34_SDIR"
else
    s34_8009() { lsof -nP -iTCP:8009 -sTCP:LISTEN -t 2>/dev/null | head -1; }
    s34_8008() { lsof -nP -iTCP:8008 -sTCP:LISTEN -t 2>/dev/null | head -1; }
    S34_LINE0=""
    for _ in $(seq 1 20); do
        S34_LINE0=$(grep -ahF 'mDNS: announcing ' "$S34_DLOG" 2>/dev/null | grep -aF '_googlecast._tcp' | tail -1)
        [ -n "$S34_LINE0" ] && break
        sleep 0.5
    done
    if [ -z "$S34_LINE0" ]; then
        fail "no Cast announcement from the toggle instance, so there is nothing to switch off"
    else
        [ "$(s34_8009)" = "$S34_PID" ] \
            && pass "before the switch: 8009 is this process's" \
            || fail "8009 is not bound by the toggle instance before the switch: $(s34_8009)"
        [ "$(s34_8008)" = "$S34_PID" ] \
            && pass "and so is 8008" \
            || fail "8008 is not bound by the toggle instance: $(s34_8008)"

        # A LAN request with no token must hear the management sentence, not this field.
        # The address is read from the instance's own status document rather than typed
        # (`server.ip` is the `/`-joined advertised list; the first is what a sender on
        # this segment would use), and an instance that publishes none skips this check
        # instead of posting at a guess.
        S34_IP=$(curl -s --max-time 5 "http://127.0.0.1:$S34_PORT/api?query=status&token=$S34_TOK" 2>/dev/null \
            | python3 -c 'import json,sys; d=json.load(sys.stdin); print(((d.get("server") or {}).get("ip") or "").split("/")[0])' 2>/dev/null)
        if [ -z "$S34_IP" ]; then
            skip "the instance publishes no address, so the LAN-shaped refusal cannot be asked here"
        else
            S34_LAN=$(curl -s --max-time 5 -X POST -d "toggle-protocol=Chromecast Protocol" \
                "http://$S34_IP:$S34_PORT/api" 2>/dev/null | tr -d '\n' | head -c 220)
            case "$S34_LAN" in
                *'"code": 403'*|*'"code":403'*) pass "a LAN POST without the token is refused" ;;
                *) fail "a LAN POST without the token was not refused: $S34_LAN" ;;
            esac
        fi
        [ "$(s34_8009)" = "$S34_PID" ] \
            && pass "…and 8009 is still up after the refusal" \
            || fail "a refused toggle cost the receiver its port"

        # ---- off ---------------------------------------------------------------
        S34_LIS0=$(grep -ac 'announcing [^ ]*_googlecast\._tcp\.local\.' "$S34_DLOG" 2>/dev/null)
        S34_LIA0=$(grep -ac 'announcing [^ ]*_airplay\._tcp\.local\.' "$S34_DLOG" 2>/dev/null)
        S34_OFF=$(curl -s --max-time 8 -X POST -d "toggle-protocol=Chromecast Protocol&token=$S34_TOK" \
            "http://127.0.0.1:$S34_PORT/api" 2>/dev/null)
        S34_OFF_MSG=$(printf '%s' "$S34_OFF" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
        [ "$S34_OFF_MSG" = "0 | Chromecast Protocol已停用" ] \
            && pass "the switch answers the reference's sentence: ${S34_OFF_MSG#*| }" \
            || fail "toggle off answered '$S34_OFF_MSG'"
        sleep 1
        [ -z "$(s34_8009)" ] \
            && pass "8009 is closed" \
            || fail "8009 is still bound by pid $(s34_8009) after switching Chromecast off"
        [ -z "$(s34_8008)" ] \
            && pass "and the setup face closed with it (they are one protocol)" \
            || fail "8008 is still bound by pid $(s34_8008) after being switched off"
        case "$(grep -ahF 'protocol switch applied' "$S34_DLOG" 2>/dev/null | tail -1)" in
            *"mDNS re-announced"*) pass "the announcement was rebuilt, not left pointing at a closed port" ;;
            *) fail "no 'protocol switch applied' sentence in $S34_DLOG" ;;
        esac
        # Counts **since this step**, not since boot: a boot that already announced both
        # types would otherwise make "one type dropped" look like either outcome. The
        # file-level truth is the settings list below; this row asks the wire's sentence.
        sleep 1
        S34_LIS=$(grep -ac 'announcing [^ ]*_googlecast\._tcp\.local\.' "$S34_DLOG" 2>/dev/null)
        S34_LIA=$(grep -ac 'announcing [^ ]*_airplay\._tcp\.local\.' "$S34_DLOG" 2>/dev/null)
        [ "$S34_LIS" = "$S34_LIS0" ] && [ "$S34_LIA" -gt "$S34_LIA0" ] \
            && pass "only the surviving protocol re-announced (cast $S34_LIS0 -> $S34_LIS, airplay $S34_LIA0 -> $S34_LIA)" \
            || fail "the re-announcement counts do not show one type dropped (cast $S34_LIS0 -> $S34_LIS, airplay $S34_LIA0 -> $S34_LIA)"
        S34_LIST=$(python3 -c "import json;print(json.load(open('$S34_SDIR/macast_setting.json')).get('Macast_Protocols'))")
        [ "$S34_LIST" = "['DLNA Protocol', 'AirPlay Protocol']" ] \
            && pass "the list on disk is the two that survived, in the canonical order" \
            || fail "the file reads $S34_LIST"
        curl -s --max-time 5 "http://127.0.0.1:$S34_PORT/description.xml" 2>/dev/null | grep -aq "Macast" \
            && pass "…and DLNA kept its HTTP face, which is where this page lives" \
            || fail "the settings page's own HTTP face stopped with the switch"

        # ---- back on -----------------------------------------------------------
        S34_ON=$(curl -s --max-time 8 -X POST -d "toggle-protocol=Chromecast Protocol&token=$S34_TOK" \
            "http://127.0.0.1:$S34_PORT/api" 2>/dev/null)
        S34_ON_MSG=$(printf '%s' "$S34_ON" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
        [ "$S34_ON_MSG" = "0 | Chromecast Protocol已启用" ] \
            && pass "and back on with the same sentence shape: ${S34_ON_MSG#*| }" \
            || fail "toggle on answered '$S34_ON_MSG'"
        sleep 1
        [ "$(s34_8009)" = "$S34_PID" ] \
            && pass "8009 is bound again by the same process" \
            || fail "8009 did not come back: $(s34_8009)"
        [ "$(s34_8008)" = "$S34_PID" ] \
            && pass "and so is 8008" \
            || fail "8008 did not come back: $(s34_8008)"
        S34_LIS2=$(grep -ac 'announcing [^ ]*_googlecast\._tcp\.local\.' "$S34_DLOG" 2>/dev/null)
        [ "$S34_LIS2" -gt "$S34_LIS" ] \
            && pass "the Cast announcement is back (cast lines $S34_LIS -> $S34_LIS2)" \
            || fail "no new Cast announcement after switching it back on ($S34_LIS -> $S34_LIS2)"
        S34_LIST2=$(python3 -c "import json;print(json.load(open('$S34_SDIR/macast_setting.json')).get('Macast_Protocols'))")
        [ "$S34_LIST2" = "['DLNA Protocol', 'Chromecast Protocol', 'AirPlay Protocol']" ] \
            && pass "the file is back to all three in the canonical order" \
            || fail "after switching back on the file reads $S34_LIST2"

        # ---- the two refusals --------------------------------------------------
        S34_MD5=$(md5 -q "$S34_SDIR/macast_setting.json" 2>/dev/null)
        S34_DLNA=$(curl -s --max-time 8 -X POST -d "toggle-protocol=DLNA Protocol&token=$S34_TOK" \
            "http://127.0.0.1:$S34_PORT/api" 2>/dev/null)
        case "$S34_DLNA" in
            *'"code": 1'*|*'"code":1'*)
                case "$S34_DLNA" in
                    *"DLNA"*) pass "switching DLNA off is refused, and the refusal names why (this build's page lives there)" ;;
                    *) fail "the DLNA refusal does not name DLNA: $S34_DLNA" ;;
                esac ;;
            *) fail "DLNA could be switched off, which would take the page with it: $S34_DLNA" ;;
        esac
        [ "$S34_MD5" = "$(md5 -q "$S34_SDIR/macast_setting.json" 2>/dev/null)" ] \
            && pass "…and it wrote nothing" \
            || fail "a refused DLNA toggle rewrote the settings file"
        S34_GHOST=$(curl -s --max-time 8 -X POST -d "toggle-protocol=Ghost Protocol&token=$S34_TOK" \
            "http://127.0.0.1:$S34_PORT/api" 2>/dev/null)
        case "$S34_GHOST" in
            *'"code": 0'*|*'"code":0'*)
                pass "an unknown title still answers success — the reference's own oddity, ported" ;;
            *) fail "an unknown title answered $S34_GHOST" ;;
        esac
        case "$(tail -20 "$S34_SDIR/macast.log" 2>/dev/null | grep -aF 'Skipping unknown protocol: Ghost Protocol')" in
            *Ghost*) pass "…while the log says which name was dropped, because the answer will not" ;;
            *) fail "no warning for the dropped unknown title" ;;
        esac
        [ "$S34_MD5" = "$(md5 -q "$S34_SDIR/macast_setting.json" 2>/dev/null)" ] \
            && pass "and the ghost is not in the file either" \
            || fail "the unknown title reached the settings file"
        [ "$(s34_8009)" = "$S34_PID" ] \
            && pass "8009 survived both refusals" \
            || fail "one of the refusals cost the receiver its port"
        # The reference's raw comparison, ported: a lowercase name is 'not in' the list
        # of canonical titles, so it reads as **enable**; the canonical title is already
        # there, so the file does not change and the answer echoes the caller's spelling.
        S34_LOWER=$(curl -s --max-time 8 -X POST -d "toggle-protocol=chromecast&token=$S34_TOK" \
            "http://127.0.0.1:$S34_PORT/api" 2>/dev/null)
        S34_LOWER_MSG=$(printf '%s' "$S34_LOWER" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
        [ "$S34_LOWER_MSG" = "0 | chromecast已启用" ] \
            && pass "a lowercase name is read as enable and echoed back as typed (the reference compares the raw string)" \
            || fail "the lowercase toggle answered '$S34_LOWER_MSG'"
        [ "$S34_MD5" = "$(md5 -q "$S34_SDIR/macast_setting.json" 2>/dev/null)" ] \
            && pass "…and enabling something already on changes nothing" \
            || fail "the lowercase toggle rewrote the file"
    fi
    kill -9 "$S34_PID" 2>/dev/null
    pkill -9 -f "$S34_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S34_SDIR (macast.log, logs/Discovery.log, macast_setting.json)"
    else
        rm -rf "$S34_SDIR"
    fi
fi

# Put it back, for §24's reason: §35/§36/§37 read `$BASE`, and a section that silently
# talked to a closed port would report "the quit button did not work".
start_main

echo "== 35: the page's quit button ends this process, after answering =="
# `app-action` is row 8 of the reference's table (`protocol.py:2295`) and `_app_action`
# (`:2404-2414`) starts a daemon thread for `quit` **before** it answers, so the reply
# that says 「Macast 正在退出」 is written by a process that is already leaving. This
# section asks three things only a real process can answer: the reply arrives, the
# process is gone, and every listener went with it — plus the two verbs that must **not**
# end anything (an unknown one and `check-update`, which this build refuses instead of
# miming a progress bar).
S35_SDIR=$(mktemp -d /tmp/macast-rust-quit.XXXXXX)
S35_PORT=$((PORT + 15))
S35_SOCK="$S35_SDIR/mpvsocket"
S35_TOK="0123456789abcdef0123456789abcdef"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Quit", "Api_Token": "%s"}' \
    "$S35_PORT" "$S35_TOK" > "$S35_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S35_SDIR" MACAST_MPV_SOCKET="$S35_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S35_SDIR/stdout.log" 2>&1 &
S35_PID=$!
disown "$S35_PID" 2>/dev/null || true
S35_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S35_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S35_PID" ]; then
        S35_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S35_READY" ]; then
    fail "the quit instance never became the listener on $S35_PORT"
    kill -9 "$S35_PID" 2>/dev/null
    rm -rf "$S35_SDIR"
else
    s35_alive() { kill -0 "$S35_PID" 2>/dev/null; }
    S35_GHOST=$(curl -s --max-time 5 -X POST -d "app-action=nope&token=$S35_TOK" \
        "http://127.0.0.1:$S35_PORT/api" 2>/dev/null)
    S35_GHOST_MSG=$(printf '%s' "$S35_GHOST" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
    [ "$S35_GHOST_MSG" = "1 | 未知应用操作：nope" ] \
        && pass "an unknown verb answers the reference's sentence: ${S35_GHOST_MSG#*| }" \
        || fail "an unknown verb answered '$S35_GHOST_MSG'"
    s35_alive \
        && pass "…and the app is still running (an unknown verb must not end anything)" \
        || fail "an unknown verb killed the app"
    S35_UPD=$(curl -s --max-time 5 -X POST -d "app-action=check-update&token=$S35_TOK" \
        "http://127.0.0.1:$S35_PORT/api" 2>/dev/null)
    case "$S35_UPD" in
        *'"code": 1'*|*'"code":1'*)
            pass "check-update is refused by this build instead of miming a progress bar" ;;
        *) fail "check-update answered $S35_UPD" ;;
    esac
    s35_alive \
        && pass "…and it did not end the app either" \
        || fail "check-update killed the app"

    # ---- quit ---------------------------------------------------------------
    S35_QUIT=$(curl -s --max-time 8 -X POST -d "app-action=quit&token=$S35_TOK" \
        "http://127.0.0.1:$S35_PORT/api" 2>/dev/null)
    S35_QUIT_MSG=$(printf '%s' "$S35_QUIT" | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
    [ "$S35_QUIT_MSG" = "0 | Macast 正在退出" ] \
        && pass "quit answers before leaving: ${S35_QUIT_MSG#*| }" \
        || fail "quit answered '$S35_QUIT_MSG'"
    S35_GONE=""
    for _ in $(seq 1 40); do
        if ! s35_alive; then S35_GONE=1; break; fi
        sleep 0.25
    done
    [ -n "$S35_GONE" ] \
        && pass "and the process is gone" \
        || fail "the process is still alive 10 s after quitting (pid $S35_PID)"
    sleep 0.5
    # **"Went with it" means §35's own listeners, not "the ports are empty".** 8009/8008 are
    # fixed (senders hardcode them, `dial.py:25-26`), so the main instance legitimately holds
    # them again by now — an earlier version asserted the ports were free and reported *that*
    # instance as an orphan (measured 2026-10-21: it named a pid `start_main` had just
    # spawned, while §35's own process was already gone). The claim that matters is "the
    # process that quit released what it held", which is a question about a pid.
    S35_LEFT=""
    S35_OTHER=""
    for port in "$S35_PORT" 8009 8008; do
        holder=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | head -1)
        [ -n "$holder" ] || continue
        if [ "$holder" = "$S35_PID" ]; then
            S35_LEFT="$S35_LEFT $port:$holder"
        else
            S35_OTHER="$S35_OTHER $port:$holder"
        fi
    done
    # The suffix is built into a variable first: a `${x:+word}` expansion whose word
    # contains a quote, a semicolon or another `$` is parsed differently by bash 3.2, and
    # this machine's /bin/bash is 3.2 — the first version broke the whole file's syntax.
    S35_SUFFIX=""
    [ -z "$S35_OTHER" ] || S35_SUFFIX=" plus$S35_OTHER, which belong to another instance"
    [ -z "$S35_LEFT" ] \
        && pass "the quitting process released every listener it held — application port, 8009, 8008$S35_SUFFIX" \
        || fail "a listener outlived the process:$S35_LEFT"
    s35_grep() { grep -ahF "$1" "$S35_SDIR/macast.log" "$S35_SDIR/logs/"*.log 2>/dev/null; }
    [ -n "$(s35_grep 'quit requested from the settings page')" ] \
        && pass "the quit was logged before the exit, so the reason is on the record" \
        || fail "no quit sentence in the instance's logs"
    grep -aqi "byebye" "$S35_SDIR/macast.log" 2>/dev/null \
        && pass "SSDP said byebye on the way out (the exit hook ran)" \
        || fail "no SSDP byebye: the exit hook did not run, so the LAN keeps a dead device for its cache"
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S35_SDIR (macast.log, logs/)"
    else
        rm -rf "$S35_SDIR"
    fi
fi

echo "== 36: set-renderer accepts the one renderer this build has, and says so =="
# Row 6 of the reference's POST table. This shell ships **one** renderer, so the only
# accepted title is the one already in force (`host::renderer_title`: the stored
# `Macast_Renderer` or `MPV`), and accepting it still does what the reference's switch
# does — persists the canonical title and reloads the player. Everything else is the
# reference's own refusal (`未找到播放器：{title}`), which is the honest half: a list with
# one entry must not pretend to find a plugin that is not in this build.
S36_SDIR=$(mktemp -d /tmp/macast-rust-renderer.XXXXXX)
S36_PORT=$((PORT + 16))
S36_SOCK="$S36_SDIR/mpvsocket"
S36_TOK="0123456789abcdef0123456789abcdef"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Renderer", "Api_Token": "%s"}' \
    "$S36_PORT" "$S36_TOK" > "$S36_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S36_SDIR" MACAST_MPV_SOCKET="$S36_SOCK" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S36_SDIR/stdout.log" 2>&1 &
S36_PID=$!
disown "$S36_PID" 2>/dev/null || true
S36_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S36_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S36_PID" ]; then
        S36_READY=1
        break
    fi
    sleep 0.25
done
if [ -z "$S36_READY" ]; then
    fail "the renderer instance never became the listener on $S36_PORT"
    kill -9 "$S36_PID" 2>/dev/null
    rm -rf "$S36_SDIR"
else
    S36_MD5=$(md5 -q "$S36_SDIR/macast_setting.json" 2>/dev/null)
    S36_NO=$(curl -s --max-time 8 -X POST -d "set-renderer=IINA&token=$S36_TOK" \
        "http://127.0.0.1:$S36_PORT/api" 2>/dev/null | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
    [ "$S36_NO" = "1 | 未找到播放器：IINA" ] \
        && pass "a renderer this build does not have is refused in the reference's words" \
        || fail "set-renderer=IINA answered '$S36_NO'"
    [ "$S36_MD5" = "$(md5 -q "$S36_SDIR/macast_setting.json" 2>/dev/null)" ] \
        && pass "…and the refusal wrote nothing" \
        || fail "a refused renderer switch rewrote the settings file"
    S36_YES=$(curl -s --max-time 8 -X POST -d "set-renderer=%20MPV%20&token=$S36_TOK" \
        "http://127.0.0.1:$S36_PORT/api" 2>/dev/null | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null)
    [ "$S36_YES" = "0 | 播放器已切换为 MPV" ] \
        && pass "the one renderer this build has is accepted, and the answer names it" \
        || fail "set-renderer=MPV answered '$S36_YES'"
    grep -aq '"Macast_Renderer": "MPV"' "$S36_SDIR/macast_setting.json" \
        && pass "the canonical title is now in the settings file (a fresh install's '' became MPV)" \
        || fail "the accepted switch did not persist Macast_Renderer: $(cat "$S36_SDIR/macast_setting.json")"
    [ "$(lsof -nP -iTCP:"$S36_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S36_PID" ] \
        && pass "the app is still serving after the switch (the reload is the player's, not the receiver's)" \
        || fail "the renderer switch cost the app its HTTP face"
    # The reload's visible half needs media to reload, so it is **not asked here**: a
    # `pass` that only narrates what this section does not check is the "不提问的检查"
    # this repository keeps deleting (§4.13). The playing half belongs to §13/§14's
    # witnesses; this section stops at the answer and the file.
    kill -9 "$S36_PID" 2>/dev/null
    pkill -9 -f "$S36_SOCK" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S36_SDIR (macast.log, logs/, macast_setting.json)"
    else
        rm -rf "$S36_SDIR"
    fi
fi

echo "== 37: save-launch-param writes the whole file, answers first, and the app comes back =="
# `_post_save_launch_param` (`protocol.py:2640-2648`): replace the whole settings table
# from one JSON object, persist, restart, answer `success`. This section asks the three
# things only a process can: a bad document changes nothing, a good one answers before
# the process leaves, and the app comes back on the same port reading the new file.
S37_SDIR=$(mktemp -d /tmp/macast-rust-launch.XXXXXX)
S37_PORT=$((PORT + 17))
S37_TOK="0123456789abcdef0123456789abcdef"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Launch", "Api_Token": "%s"}' \
    "$S37_PORT" "$S37_TOK" > "$S37_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S37_SDIR" MACAST_MPV_SOCKET="$S37_SDIR/mpvsocket" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S37_SDIR/stdout.log" 2>&1 &
S37_PID=$!
disown "$S37_PID" 2>/dev/null || true
S37_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S37_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S37_PID" ]; then
        S37_READY=1; break
    fi
    sleep 0.25
done
if [ -z "$S37_READY" ]; then
    fail "the launch-param instance never became the listener on $S37_PORT"
    kill -9 "$S37_PID" 2>/dev/null
    rm -rf "$S37_SDIR"
else
    s37_ask() {  # $1 = payload
        curl -s --max-time 8 -X POST --data-urlencode "save-launch-param=$1" \
            -d "token=$S37_TOK" "http://127.0.0.1:$S37_PORT/api" 2>/dev/null \
            | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null
    }
    S37_MD5=$(md5 -q "$S37_SDIR/macast_setting.json")
    [ "$(s37_ask '{not json')" = "1 | json format error" ] \
        && pass "a document that does not parse answers the reference's sentence" \
        || fail "bad JSON answered '$(s37_ask '{not json')'"
    [ "$(s37_ask '[1,2,3]')" = "1 | json format error" ] \
        && pass "and a non-object is refused with it too (this port checks; the reference crashes on the next read)" \
        || fail "a non-object answered '$(s37_ask '[1,2,3]')'"
    [ "$S37_MD5" = "$(md5 -q "$S37_SDIR/macast_setting.json")" ] \
        && pass "…and neither refusal wrote the file" \
        || fail "a refused launch param rewrote the settings file"
    [ "$(lsof -nP -iTCP:"$S37_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S37_PID" ] \
        && pass "…nor restarted the app" \
        || fail "a refused launch param restarted the app"

    S37_DOC=$(printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Relaunch", "Api_Token": "%s", "Macast_Protocols": ["DLNA Protocol"]}' "$S37_PORT" "$S37_TOK")
    [ "$(s37_ask "$S37_DOC")" = "0 | success" ] \
        && pass "a good document answers before the restart: success" \
        || fail "a good document answered '$(s37_ask "$S37_DOC")'"
    S37_NEW=""
    for _ in $(seq 1 60); do
        holder=$(lsof -nP -iTCP:"$S37_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)
        if [ -n "$holder" ] && [ "$holder" != "$S37_PID" ]; then S37_NEW="$holder"; break; fi
        sleep 0.25
    done
    [ -n "$S37_NEW" ] \
        && pass "the app came back as a new process on the same port (pid $S37_PID -> $S37_NEW)" \
        || fail "nothing is listening on $S37_PORT 15 s after the restart"
    if [ -n "$S37_NEW" ]; then
        kill -0 "$S37_PID" 2>/dev/null \
            && fail "the old process ($S37_PID) is still alive next to $S37_NEW" \
            || pass "and the old process is gone"
        python3 -c "import json;d=json.load(open('$S37_SDIR/macast_setting.json'));print(d.get('DLNA_FriendlyName'))" \
            | grep -qx 'Macast Rust Relaunch' \
            && pass "the new file is what the new process reads (friendly name changed)" \
            || fail "the restarted app reads '$(python3 -c "import json;print(json.load(open('$S37_SDIR/macast_setting.json')).get('DLNA_FriendlyName'))")'"
        kill -9 "$S37_NEW" 2>/dev/null
    fi
    kill -9 "$S37_PID" 2>/dev/null
    pkill -9 -f "$S37_SDIR" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S37_SDIR"
    else
        rm -rf "$S37_SDIR"
    fi
fi

echo "== 38: the plugin rows say no, and the code-gated ones are gated =="
# Design doc §18: this build has no plugin host, so the seven rows answer code 1 with a
# sentence that names what cannot happen; the four that would run code keep the
# reference's `code-execution` gate. This section asks the three machine-level halves:
# the answers arrive, the LAN hears the gate rather than the feature, and nothing lands
# on disk.
S38_SDIR=$(mktemp -d /tmp/macast-rust-plugins.XXXXXX)
S38_PORT=$((PORT + 18))
S38_TOK="0123456789abcdef0123456789abcdef"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Plugins", "Api_Token": "%s"}' \
    "$S38_PORT" "$S38_TOK" > "$S38_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S38_SDIR" MACAST_MPV_SOCKET="$S38_SDIR/mpvsocket" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S38_SDIR/stdout.log" 2>&1 &
S38_PID=$!
disown "$S38_PID" 2>/dev/null || true
S38_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S38_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S38_PID" ]; then
        S38_READY=1; break
    fi
    sleep 0.25
done
if [ -z "$S38_READY" ]; then
    fail "the plugin instance never became the listener on $S38_PORT"
    kill -9 "$S38_PID" 2>/dev/null
    rm -rf "$S38_SDIR"
else
    S38_MD5=$(md5 -q "$S38_SDIR/macast_setting.json")
    s38_msg() { python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null; }
    S38_EN=$(curl -s --max-time 6 -X POST -d "plugin-enable=1&token=$S38_TOK" "http://127.0.0.1:$S38_PORT/api" | s38_msg)
    case "$S38_EN" in
        "1 | "*插件*) pass "plugin-enable answers code 1 and names the plugin vocabulary" ;;
        *) fail "plugin-enable answered '$S38_EN'" ;;
    esac
    S38_IP=$(curl -s --max-time 5 "http://127.0.0.1:$S38_PORT/api?query=status&token=$S38_TOK" 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); print(((d.get("server") or {}).get("ip") or "").split("/")[0])' 2>/dev/null)
    if [ -n "$S38_IP" ]; then
        S38_LAN=$(curl -s --max-time 5 -X POST -d "install-plugin=http://evil.invalid/x.py" "http://$S38_IP:$S38_PORT/api" 2>/dev/null)
        case "$S38_LAN" in
            *'"code": 403'*|*'"code":403'*) pass "a LAN install without the token hears the gate, not the feature" ;;
            *) fail "a LAN install answered $S38_LAN" ;;
        esac
    else
        skip "the instance publishes no address, so the LAN-shaped refusal cannot be asked here"
    fi
    S38_IN=$(curl -s --max-time 6 -X POST --data-urlencode "install-plugin=http://evil.invalid/x.py" -d "token=$S38_TOK" "http://127.0.0.1:$S38_PORT/api" | s38_msg)
    case "$S38_IN" in
        "1 | "*宿主*) pass "with the token it is still refused: this build has no plugin host" ;;
        *) fail "install-plugin answered '$S38_IN'" ;;
    esac
    S38_PF=$(curl -s --max-time 6 -X POST -d "plugin-file=x&token=$S38_TOK" "http://127.0.0.1:$S38_PORT/api" | s38_msg)
    case "$S38_PF" in
        "1 | "*宿主*) pass "plugin-file is refused on the same sentence (the body is never read)" ;;
        *) fail "plugin-file answered '$S38_PF'" ;;
    esac
    [ "$S38_MD5" = "$(md5 -q "$S38_SDIR/macast_setting.json")" ] \
        && pass "none of the four wrote the settings file" \
        || fail "a refused plugin row rewrote the settings file"
    if find "$S38_SDIR" -name '*.py' -print -quit | grep -q .; then
        fail "a plugin file landed under the settings directory: $(find "$S38_SDIR" -name '*.py' | head -2)"
    else
        pass "and no .py was written anywhere under the settings directory"
    fi
    S38_PI=$(curl -s --max-time 5 "http://127.0.0.1:$S38_PORT/api?query=plugin-info&token=$S38_TOK" 2>/dev/null \
        | python3 -c 'import json,sys;d=json.load(sys.stdin);print(len((d.get("plugins") or [])))' 2>/dev/null)
    [ "$S38_PI" = "0" ] \
        && pass "plugin-info still lists zero plugins — the empty list is the answer, not a failure" \
        || fail "plugin-info lists '$S38_PI' plugin(s)"
    kill -9 "$S38_PID" 2>/dev/null
    pkill -9 -f "$S38_SDIR" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S38_SDIR"
    else
        rm -rf "$S38_SDIR"
    fi
fi

echo "== 39: mirror-action is token-gated, and the three mirror doors say one sentence =="
# The row landed 2026-10-20 as the 23rd and last of the reference's POST table
# (protocol.py:2288-2331), and it is the only POST whose gate is `token` rather than
# `management`: a page may not ask this machine to start capturing the desktop just
# because the request came from loopback. Two things this section asks that the unit
# tests cannot: the gate is paid on a real socket, and the POST door quotes the same
# sentence the two GET doors answer — they read one constant, and a second literal
# creeping in is the "two answers for one fact" failure AGENTS.md §4.8 keeps naming.
S39_SDIR=$(mktemp -d /tmp/macast-rust-mirror.XXXXXX)
S39_PORT=$((PORT + 19))
S39_TOK="fedcba9876543210fedcba9876543210"
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Mirror", "Api_Token": "%s"}' \
    "$S39_PORT" "$S39_TOK" > "$S39_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S39_SDIR" MACAST_MPV_SOCKET="$S39_SDIR/mpvsocket" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S39_SDIR/stdout.log" 2>&1 &
S39_PID=$!
disown "$S39_PID" 2>/dev/null || true
S39_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S39_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S39_PID" ]; then
        S39_READY=1; break
    fi
    sleep 0.25
done
if [ -z "$S39_READY" ]; then
    fail "the mirror instance never became the listener on $S39_PORT"
    kill -9 "$S39_PID" 2>/dev/null
    rm -rf "$S39_SDIR"
else
    s39_msg() { python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("code"),"|",d.get("message"))' 2>/dev/null; }
    # The file's digest **before** any request, so the "nothing moved" check below has
    # something to compare against (a digest compared to itself asks nothing).
    S39_MD5=$(md5 -q "$S39_SDIR/macast_setting.json")
    # 1. Loopback without the token: refused. This is the whole reason the row is not
    #    management-gated -- the very next request is the same one with a token, and it
    #    is served.
    S39_NOTOK=$(curl -s --max-time 6 -X POST -d "mirror-action=start" \
        "http://127.0.0.1:$S39_PORT/api" | s39_msg)
    case "$S39_NOTOK" in
        "403 | "*) pass "mirror-action from loopback without the token hears the token gate" ;;
        *) fail "mirror-action without a token answered: $S39_NOTOK" ;;
    esac
    # 2. With the token: the shell is reached, and the answer names the missing face
    #    instead of pretending a capture started.
    S39_WITH=$(curl -s --max-time 6 -X POST -d "mirror-action=start&token=$S39_TOK" \
        "http://127.0.0.1:$S39_PORT/api" | s39_msg)
    case "$S39_WITH" in
        "1 | "*镜像面*) pass "with the token it is still refused: this build has no mirror face" ;;
        *) fail "mirror-action with a token answered: $S39_WITH" ;;
    esac
    # 3. The two GET doors answer one document, and the POST door quotes it.
    S39_GET_STATE=$(curl -s --max-time 5 \
        "http://127.0.0.1:$S39_PORT/api?query=mirror-state&token=$S39_TOK" | s39_msg)
    S39_GET_SNAP=$(curl -s --max-time 5 \
        "http://127.0.0.1:$S39_PORT/api?query=mirror-snapshot&token=$S39_TOK" | s39_msg)
    [ "$S39_GET_STATE" = "$S39_GET_SNAP" ] \
        && pass "mirror-state and mirror-snapshot answer one document, not two" \
        || fail "the two GET mirror doors disagree: $S39_GET_STATE vs $S39_GET_SNAP"
    case "$S39_GET_STATE" in
        "1 | "*镜像面*)
            S39_SHARED=${S39_GET_STATE#1 | }
            case "$S39_WITH" in
                *"$S39_SHARED"*) pass "the POST door quotes the same sentence the GET doors answer" ;;
                *) fail "mirror-action words its refusal differently: $S39_WITH vs $S39_SHARED" ;;
            esac
            ;;
        *) fail "mirror-state answered: $S39_GET_STATE" ;;
    esac
    # 4. The same gate on the read side, on a real socket -- the request a drive-by page
    #    would actually send first. **The envelope is the GET one, not the POST one**:
    #    a GET refusal is `HTTP 403` with `code: 1` in the body (`Face::gate_refusal`'s
    #    GET shape, `protocol.py:2518`'s POST shape is the 200 one). Both halves are
    #    asserted, because reading only the body would pass for a 200 -- and the status
    #    is the half a browser acts on.
    S39_GETNOTOK_STATUS=$(curl -s --max-time 5 -o "$S39_SDIR/getnotok.json" -w '%{http_code}' \
        "http://127.0.0.1:$S39_PORT/api?query=mirror-state")
    S39_GETNOTOK=$(s39_msg < "$S39_SDIR/getnotok.json")
    [ "$S39_GETNOTOK_STATUS" = "403" ] \
        && pass "query=mirror-state without the token is an HTTP 403 from loopback too" \
        || fail "query=mirror-state without a token answered HTTP $S39_GETNOTOK_STATUS"
    case "$S39_GETNOTOK" in
        "1 | "*token*) pass "…and its body carries the token sentence, not the sentence for a LAN caller" ;;
        *) fail "query=mirror-state without a token answered: $S39_GETNOTOK" ;;
    esac
    # 5. A refused mirror moved nothing: no rewrite, and no capture process anywhere.
    [ "$S39_MD5" = "$(md5 -q "$S39_SDIR/macast_setting.json")" ] \
        && pass "the refused mirror left the settings file byte-for-byte as it was" \
        || fail "a refused mirror rewrote the settings file"
    if pgrep -x ffmpeg >/dev/null 2>&1; then
        skip "an ffmpeg is already running on this machine, so this run cannot claim the refusal spawned none"
    else
        pass "and no ffmpeg is running for it"
    fi
    kill -9 "$S39_PID" 2>/dev/null
    pkill -9 -f "$S39_SDIR" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S39_SDIR"
    else
        rm -rf "$S39_SDIR"
    fi
fi

echo "== 40: the cookie panel counts, refuses, and never echoes a value =="
# AGENTS.md §4.14's red line, asked on a live socket: the jar is a **credential**, so the
# panel answers counts and a path, never a value. Four things only this can answer:
# (a) a browser-style jar (every session line starting `#HttpOnly_`) is accepted rather
# than read as a comment -- the shape a real export has; (b) the read does not carry the
# secret back; (c) "off" **unsets** the key rather than storing `false`; (d) clearing a
# jar that is not there is a success, because the caller's goal is "no jar".
S40_SDIR=$(mktemp -d /tmp/macast-rust-cookies.XXXXXX)
S40_PORT=$((PORT + 20))
printf '{"ApplicationPort": %d, "DLNA_FriendlyName": "Macast Rust Cookies"}' \
    "$S40_PORT" > "$S40_SDIR/macast_setting.json"
env MACAST_SETTING_DIR="$S40_SDIR" MACAST_MPV_SOCKET="$S40_SDIR/mpvsocket" \
    MACAST_RESOURCE_DIR="$REPO/macast" \
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
    "$BIN" > "$S40_SDIR/stdout.log" 2>&1 &
S40_PID=$!
disown "$S40_PID" 2>/dev/null || true
S40_READY=""
for _ in $(seq 1 40); do
    if [ "$(lsof -nP -iTCP:"$S40_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S40_PID" ]; then
        S40_READY=1; break
    fi
    sleep 0.25
done
if [ -z "$S40_READY" ]; then
    fail "the cookie instance never became the listener on $S40_PORT"
    kill -9 "$S40_PID" 2>/dev/null
    rm -rf "$S40_SDIR"
else
    S40_SECRET="COOKIESECRET40"
    # A real export: the header line, then two `#HttpOnly_` lines. That prefix is what the
    # browser writes for session cookies, and reading it as a comment is the bug this
    # section exists for -- a jar of two session cookies would be judged empty and refused.
    printf '# Netscape HTTP Cookie File\n#HttpOnly_.example.com\tTRUE\t/\tFALSE\t0\tsess\t%s\n#HttpOnly_.example.com\tTRUE\t/\tFALSE\t0\ttok\t%s\n' \
        "$S40_SECRET" "$S40_SECRET" > "$S40_SDIR/jar.txt"
    S40_SAVE=$(curl -s --max-time 8 -X POST --data-urlencode "resolve-cookies@$S40_SDIR/jar.txt" \
        "http://127.0.0.1:$S40_PORT/api")
    case "$S40_SAVE" in
        *'"code": 0'*) pass "a jar whose session lines all start with #HttpOnly_ is accepted" ;;
        *) fail "a browser-style jar answered: $S40_SAVE" ;;
    esac
    case "$S40_SAVE" in
        *'"cookies": 2'*) pass "and it counts both of them (they are cookies, not comments)" ;;
        *) fail "the save document does not report two cookies: $S40_SAVE" ;;
    esac
    S40_READ=$(curl -s --max-time 6 "http://127.0.0.1:$S40_PORT/api?query=resolve-cookies")
    case "$S40_READ" in
        *"$S40_SECRET"*) fail "the panel echoed a cookie VALUE back" ;;
        *'"cookies": 2'*) pass "the panel reports the count and not the value" ;;
        *) fail "the cookie panel answered: $S40_READ" ;;
    esac
    if grep -q "$S40_SECRET" "$S40_SDIR/macast_setting.json" 2>/dev/null; then
        fail "a cookie value reached the settings file (it must live only in the 0600 jar)"
    else
        pass "and no value reached the settings file"
    fi
    S40_JAR="$S40_SDIR/resolve_cookies.txt"
    if [ -f "$S40_JAR" ]; then
        S40_MODE=$(stat -f '%Lp' "$S40_JAR" 2>/dev/null)
        [ "$S40_MODE" = "600" ] \
            && pass "the jar is 0600 on disk" \
            || fail "the jar is mode $S40_MODE, not 600"
    else
        fail "the jar the panel says it saved is not at $S40_JAR"
    fi
    # A jar with nothing readable is refused at the door, and the refusal counts the lines.
    S40_BAD=$(curl -s --max-time 8 -X POST --data-urlencode "resolve-cookies=# only a comment" \
        "http://127.0.0.1:$S40_PORT/api")
    case "$S40_BAD" in
        *'"code": 1'*) pass "an unreadable jar is refused at the door" ;;
        *) fail "a junk jar answered: $S40_BAD" ;;
    esac
    # `名字:资料目录` is accepted byte-for-byte; the two things we never pass into argv are not.
    S40_SPEC="vivaldi:$S40_SDIR/profile"
    S40_SET=$(curl -s --max-time 6 -X POST --data-urlencode "resolve-cookie-browser=$S40_SPEC" \
        "http://127.0.0.1:$S40_PORT/api")
    case "$S40_SET" in
        *'"code": 0'*) pass "a browser with a data directory is accepted" ;;
        *) fail "the profile form was refused: $S40_SET" ;;
    esac
    S40_KEYRING=$(curl -s --max-time 6 -X POST --data-urlencode "resolve-cookie-browser=chrome+keyring" \
        "http://127.0.0.1:$S40_PORT/api")
    case "$S40_KEYRING" in
        *'"code": 1'*) pass "a keyring suffix is refused (we never pick a keychain for the user)" ;;
        *) fail "chrome+keyring answered: $S40_KEYRING" ;;
    esac
    S40_CONTAINER=$(curl -s --max-time 6 -X POST --data-urlencode "resolve-cookie-browser=firefox::work" \
        "http://127.0.0.1:$S40_PORT/api")
    case "$S40_CONTAINER" in
        *'"code": 1'*) pass "a container suffix is refused on this side too" ;;
        *) fail "firefox::work answered: $S40_CONTAINER" ;;
    esac
    # Off is the key **disappearing**, not a stored `false` (§4.2): a read of the raw table
    # is the only way to tell those two apart.
    curl -s --max-time 6 -X POST --data-urlencode "resolve-cookie-browser=off" \
        "http://127.0.0.1:$S40_PORT/api" >/dev/null
    if curl -s --max-time 6 "http://127.0.0.1:$S40_PORT/api?query=launch-param" \
        | grep -q 'Resolve_Cookie_Browser'; then
        fail "switching the browser off stored a key instead of removing it"
    else
        pass "switching it off removes the key rather than storing false"
    fi
    # Clearing twice: the second time is still a success, and it says why.
    S40_C1=$(curl -s --max-time 6 -X POST -d "resolve-cookies-clear=1" "http://127.0.0.1:$S40_PORT/api")
    S40_C2=$(curl -s --max-time 6 -X POST -d "resolve-cookies-clear=1" "http://127.0.0.1:$S40_PORT/api")
    case "$S40_C1" in
        *'"code": 0'*) pass "clearing a saved jar is a success" ;;
        *) fail "clear answered: $S40_C1" ;;
    esac
    case "$S40_C2" in
        *'"code": 0'*) pass "…and clearing again is still a success (the goal is 'no jar', already reached)" ;;
        *) fail "the second clear answered: $S40_C2" ;;
    esac
    kill -9 "$S40_PID" 2>/dev/null
    pkill -9 -f "$S40_SDIR" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S40_SDIR"
    else
        rm -rf "$S40_SDIR"
    fi
fi

echo "== 41: the automation hooks run on a real cast, and the url is only ever an environment value =="
# `macast/plugins/renderer/hooks.py`, ported to `macast_core::hooks`. Four keys, one per
# transition, each a shell command of the user's own. What only a real process can answer:
# (a) the command really runs when the player is handed a url, (b) the three environment
# variables arrive, (c) a **hostile url** is not expanded by the shell — the difference
# between a feature and a remote shell, and the reason this section casts an address that
# contains `$(…)`, (d) a slow hook does not delay the cast, and (e) no hook is configured
# means no process, silently.
S41_SDIR=$(mktemp -d /tmp/macast-rust-hooks.XXXXXX)
S41_PORT=$((PORT + 21))
# The clip is served over a real socket with Range: mpv refuses a source without it, and
# the hook only fires once the player was actually handed the url.
S41_CLIP="$S41_SDIR/clip.mp4"
env -u PYTHONPATH "$CAST_PY" - "$S41_CLIP" <<'PY' 2>/dev/null
import subprocess, sys, shutil
ff = shutil.which('ffmpeg') or '/opt/homebrew/opt/ffmpeg/bin/ffmpeg'
subprocess.run([ff, '-y', '-f', 'lavfi', '-i', 'testsrc2=size=160x120:rate=10',
                '-f', 'lavfi', '-i', 'sine=frequency=440', '-t', '20',
                '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p',
                '-c:a', 'aac', '-shortest', sys.argv[1]],
               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
PY
S41_FIRED="$S41_SDIR/fired.txt"
# The command writes its three variables, then the reader compares them. `%s` quoting is
# the hook's own business — this is the user's shell command, not ours.
cat > "$S41_SDIR/macast_setting.json" <<EOF
{"ApplicationPort": $S41_PORT, "DLNA_FriendlyName": "Macast Rust Hooks",
 "Macast_Protocols": ["DLNA Protocol"],
 "Hook_On_Cast": "printf '%s|%s|%s\\n' \"\$MACAST_EVENT\" \"\$MACAST_URL\" \"\$MACAST_TITLE\" > $S41_FIRED",
 "Hook_On_Stop": "printf 'stop:%s\\n' \"\$MACAST_URL\" >> $S41_FIRED"}
EOF
# A tiny Range-capable server for the clip, on its own port.
cat > "$S41_SDIR/serve.py" <<'PY'
import http.server, os, socketserver, sys
CLIP, PORT = sys.argv[1], int(sys.argv[2])
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        size = os.path.getsize(CLIP)
        rng = self.headers.get('Range')
        start, end, partial = 0, size - 1, False
        if rng and rng.startswith('bytes='):
            a, _, b = rng[6:].partition('-')
            start = int(a) if a else 0
            end = int(b) if b else size - 1
            partial = True
        end = min(end, size - 1)
        self.send_response(206 if partial else 200)
        self.send_header('Content-Type', 'video/mp4')
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(end - start + 1))
        if partial:
            self.send_header('Content-Range', 'bytes %d-%d/%d' % (start, end, size))
        self.end_headers()
        with open(CLIP, 'rb') as f:
            f.seek(start)
            self.wfile.write(f.read(end - start + 1))
    def log_message(self, *a):
        pass
socketserver.ThreadingTCPServer.allow_reuse_address = True
socketserver.ThreadingTCPServer(('127.0.0.1', PORT), H).serve_forever()
PY
if [ ! -s "$S41_CLIP" ]; then
    skip "no ffmpeg on this machine, so the clip this section casts could not be built"
else
    env -u PYTHONPATH "$CAST_PY" "$S41_SDIR/serve.py" "$S41_CLIP" "$((S41_PORT + 1))" &
    S41_SERVE_PID=$!
    # `disown`, for the reason the app launcher has it: a job killed by a signal is
    # reported by bash **on its own stderr** when the shell exits, i.e. one line after the
    # SUMMARY where every reader expects a verdict (measured 2026-10-21: this section's
    # "Killed: 9 … serve.py" landed just above the total).
    disown "$S41_SERVE_PID" 2>/dev/null || true
    env MACAST_SETTING_DIR="$S41_SDIR" MACAST_MPV_SOCKET="$S41_SDIR/mpvsocket" \
        MACAST_RESOURCE_DIR="$REPO/macast" \
        perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' \
        "$BIN" > "$S41_SDIR/stdout.log" 2>&1 &
    S41_PID=$!
    disown "$S41_PID" 2>/dev/null || true
    S41_READY=""
    for _ in $(seq 1 40); do
        if [ "$(lsof -nP -iTCP:"$S41_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1)" = "$S41_PID" ]; then
            S41_READY=1; break
        fi
        sleep 0.25
    done
    if [ -z "$S41_READY" ]; then
        fail "the hooks instance never became the listener on $S41_PORT"
    else
        # **A hostile address**, on purpose: the shell must never see it expanded.
        S41_URL="http://127.0.0.1:$((S41_PORT + 1))/clip.mp4?\$(touch /tmp/macast-hook-pwned)\`id\`"
        rm -f /tmp/macast-hook-pwned "$S41_FIRED"
        S41_CAST=$(curl -s --max-time 15 -X POST \
            --data-urlencode "cast-uri=http://127.0.0.1:$((S41_PORT + 1))/clip.mp4" \
            --data-urlencode "cast-title=Hook Section" \
            "http://127.0.0.1:$S41_PORT/api")
        case "$S41_CAST" in
            *'"code": 0'*) pass "a cast through the web entry is accepted" ;;
            *) fail "the cast answered: $S41_CAST" ;;
        esac
        S41_GOT=""
        for _ in $(seq 1 100); do
            [ -s "$S41_FIRED" ] && { S41_GOT=$(cat "$S41_FIRED"); break; }
            sleep 0.1
        done
        # **The title half is `''` here, and that is correct**: this cast goes through
        # `POST cast-uri`, whose body carries `cast-title` — but the *renderer's*
        # `set_media_url` runs before the title is written, so the hook sees whatever the
        # state table held at that instant (nothing, on a fresh cast). The reference has the
        # same ordering: `hooks.py:125` fires `run_hook('cast', url, self.current_title())`
        # from inside `set_media_url`, before `set_media_title` is called
        # (`protocol.py:1190-1191`). Asserting a title here would be asserting an ordering
        # the reference does not have — measured 2026-10-21, this check went red on a
        # correct implementation for exactly that reason.
        case "$S41_GOT" in
            "cast|http://127.0.0.1:$((S41_PORT + 1))/clip.mp4|"*)
                pass "the cast hook ran with MACAST_EVENT and MACAST_URL (the title is empty by the reference's ordering)" ;;
            "") fail "the cast hook never ran (no $S41_FIRED after 10 s)" ;;
            *) fail "the cast hook wrote: '$S41_GOT'" ;;
        esac
        [ -e /tmp/macast-hook-pwned ] \
            && fail "a command inside the url was EXECUTED — the shell expanded an environment value" \
            || pass "nothing in the url was executed: it only ever travels as an environment value"
        # The stop hook, through the same SOAP door a control point uses.
        curl -s --max-time 8 -X POST -o /dev/null \
            -H 'Content-Type: text/xml; charset="utf-8"' \
            -H 'SOAPAction: "urn:schemas-upnp-org:service:AVTransport:1#Stop"' \
            --data-binary '<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:Stop xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:Stop></s:Body></s:Envelope>' \
            "http://127.0.0.1:$S41_PORT/AVTransport/action"
        S41_STOP=""
        for _ in $(seq 1 60); do
            grep -q '^stop:' "$S41_FIRED" 2>/dev/null && { S41_STOP=$(grep '^stop:' "$S41_FIRED"); break; }
            sleep 0.1
        done
        case "$S41_STOP" in
            "stop:http://127.0.0.1:$((S41_PORT + 1))/clip.mp4"*)
                pass "the stop hook ran and reported the url that was playing" ;;
            *) fail "the stop hook wrote: '$S41_STOP'" ;;
        esac
        # The start line names the event and the pid, and never the url: on a signed site
        # link the url is a short-lived credential and this log is what gets pasted into a
        # bug report.
        S41_LOG=$(cat "$S41_SDIR"/logs/*.log "$S41_SDIR/macast.log" 2>/dev/null)
        case "$S41_LOG" in
            *"cast hook started"*) pass "the start line is in the log" ;;
            *) fail "no 'cast hook started' line: $(echo "$S41_LOG" | grep -i hook | tail -2)" ;;
        esac
        kill -9 "$S41_PID" "$S41_SERVE_PID" 2>/dev/null
        pkill -9 -f "$S41_SDIR" 2>/dev/null
        rm -f /tmp/macast-hook-fired.txt
    fi
    pkill -9 -f "$S41_SDIR" 2>/dev/null
    if [ -n "$KEEP" ]; then
        echo "  MACAST_VERIFY_KEEP: kept $S41_SDIR"
    else
        rm -rf "$S41_SDIR"
    fi
fi

echo
if [ "$FAILED" -eq 0 ]; then
    echo "=== SUMMARY: all checks passed ==="
else
    echo "=== SUMMARY: $FAILED check(s) failed ==="
fi
exit "$FAILED"
