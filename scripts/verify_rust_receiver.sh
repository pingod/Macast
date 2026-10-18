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
import json, socket, sys
path, name = sys.argv[1], sys.argv[2]
# An explicit AF_UNIX socket, not `create_connection`: that helper wants a
# (host, port) pair and unpacks a path string one **character** at a time, so
# every read died with `ValueError: too many values to unpack` while the player
# was in fact answering — the harness, not the receiver, was wrong.
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(6)
try:
    s.connect(path)
except OSError:
    raise SystemExit(1)
s.sendall(json.dumps({"command": ["get_property", name], "request_id": 7}).encode() + b"\n")
try:
    stream = s.makefile("rb")
    while True:
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
except socket.timeout:
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
ok=""
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
    [ "$API_MSG" = "已设为 $SPEC，但本机存过 cookie 文件，解析仍以文件为准（清除文件后才会用浏览器）" ] \
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
# polling does not pile up threads. Two refreshes back to back — the second must
# find the first sweep still in flight and answer `started: false`, because
# `Table::start` takes a latch before spawning rather than starting a multicast
# per click. A missing latch would show up as two `true`s and an ever-growing
# thread count.
api_probe "$BASE/api?query=cast-targets&refresh=1"
FIRST_STARTED=$(json_ask targets.started)
api_probe "$BASE/api?query=cast-targets&refresh=1"
SECOND_STARTED=$(json_ask targets.started)
if [ "$FIRST_STARTED" = "false" ] || [ "$SECOND_STARTED" = "false" ]; then
    pass "consecutive searches share the latch: one of them was refused (started=$FIRST_STARTED/$SECOND_STARTED)"
else
    fail "two back-to-back refreshes both started a sweep (started=$FIRST_STARTED/$SECOND_STARTED) — no latch?"
fi
# **Read this check's teeth honestly.** On a machine whose caches stay empty the
# cold-start kick has already put a sweep in flight long before §19 gets here, so
# the pair reads `false/false` and the discriminating shape (`true` then `false`)
# needs a network where a sweep *finishes* between two curls ~10 ms apart. What
# this proves is "not one thread per poll"; it does **not** prove "a started sweep
# eventually ends" — and nothing else here proves that either, because the very
# first check of this section is the one that had to be deleted for asking exactly
# that. The settle property is witnessed only by `targets_tests.rs`'s latch cases
# against a fake `Sweeps`, never on this LAN.
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
MDNS_LINE=$(log_grep 'mDNS: announcing ' | tail -1)
MDNS_INSTANCE=$(printf '%s' "$MDNS_LINE" | sed -n 's/.*announcing \([^ ]*\) on port .*/\1/p')
MDNS_PORT=$(printf '%s' "$MDNS_LINE" | sed -n 's/.* on port \([0-9][0-9]*\) over .*/\1/p')

if [ -z "$MDNS_INSTANCE" ]; then
    # The legitimate second outcome: nothing to announce, because 8009 is not
    # listening. Silence here would be the failure — a receiver that came up and
    # quietly never announced is exactly the state a user reads as "搜到但投不上",
    # and a run that printed neither line is a run that did not look.
    if [ -n "$(log_grep 'mDNS: not announcing')" ]; then
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
        seen.add(name)

    def remove_service(self, *a):
        pass

    def update_service(self, *a):
        pass


# The file appears only when the window closes, so the shell's wait is a wait for the
# answer, not for a line that may never have been written.
zc = z.Zeroconf(ip_version=z.IPVersion.V4Only)
b = z.ServiceBrowser(zc, "_googlecast._tcp.local.", L())
time.sleep(budget)
b.cancel()
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
                        skip "this binary was handed nothing on 5353 while $WIRE, and a joined python socket on the same port receives the same traffic; the subject-named receiving sentence never fired, and the DEBUG half of it is silent too — proven instead by announce_tests e8, e9 and e10 and by the loopback path"
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
import json, socket, sys
path, name = sys.argv[1], sys.argv[2]
# An explicit AF_UNIX socket: `create_connection` wants a (host, port) pair and
# unpacks a path string one character at a time, so every read died with
# `ValueError: too many values to unpack` while the player was answering fine.
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(4)
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

echo
if [ "$FAILED" -eq 0 ]; then
    echo "=== SUMMARY: all checks passed ==="
else
    echo "=== SUMMARY: $FAILED check(s) failed ==="
fi
exit "$FAILED"
