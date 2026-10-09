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
# Macast on the LAN via SSDP (UDP/1900). The teardown sends a goodbye and the
# run is short, but do not run this during a demo.
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
api_probe() {
    API_STATUS=$(curl -s --noproxy '*' -m 8 -o "$API_BODY" -w '%{http_code}' "$@")
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
    API_CODE=${API_LINE%%	*}
    API_MSG=${API_LINE#*	}
}

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
printf '{"ApplicationPort": %d, "FriendlyName": "Macast Rust Verify"}' "$PORT" \
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
CLIP_BYTES=""
[ -s "$CLIP" ] && CLIP_BYTES=$(wc -c < "$CLIP" | tr -d ' ')
if [ -n "$CLIP_BYTES" ]; then
    python3 - "$CLIP" "$MEDPORT" <<'PY' > /tmp/verify_rust_receiver_media.log 2>&1 &
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
path, port = sys.argv[1], int(sys.argv[2])
size = os.path.getsize(path)
class Range(BaseHTTPRequestHandler):
    # HTTP/1.1 because the whole point is accurate framing: a keep-alive
    # connection that lies about Content-Length hangs the client's seek.
    protocol_version = "HTTP/1.1"
    def log_message(self, fmt, *args): pass
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
    def do_GET(self): self._respond(True)
    def do_HEAD(self): self._respond(False)
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
    api_probe "$BASE/api?query=cast&url=$MEDIA_URL&cast-title=$TITLE_ARG&token=$EXTRACTED"
    [ "$API_STATUS" = "200" ] && [ "$API_CODE" = "0" ] \
        && case "$API_MSG" in *"已投屏"*) true ;; *) false ;; esac \
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
        && pass "the window name follows the cast-title, not the last path segment" \
        || fail "mpv force-media-title reads '$(mpv_ask force-media-title)', not $TITLE_ARG"
fi

echo
if [ "$FAILED" -eq 0 ]; then
    echo "=== SUMMARY: all checks passed ==="
else
    echo "=== SUMMARY: $FAILED check(s) failed ==="
fi
exit "$FAILED"
