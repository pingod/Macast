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
python3 - "$PORT" <<'PY'
import socket, sys, time
port = int(sys.argv[1])
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
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
sys.exit(0 if ok else 1)
PY
if [ "$?" -eq 0 ]; then
    pass "receiver answered a live M-SEARCH with an HTTP 200"
else
    fail "no M-SEARCH answer (receiver not on UDP/1900?)"
fi

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

echo
if [ "$FAILED" -eq 0 ]; then
    echo "=== SUMMARY: all checks passed ==="
else
    echo "=== SUMMARY: $FAILED check(s) failed ==="
fi
exit "$FAILED"
