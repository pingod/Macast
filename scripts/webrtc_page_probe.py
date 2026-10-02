#!/usr/bin/env python3
"""End-to-end check of the WebRTC viewing page, in a real browser.

The `webrtc` target is the one screen-mirror shape whose far half is
JavaScript: the page fetches the sender's offer, answers it, and paints
whatever the SRTP track delivers. No Python test owns that half -- the
regression suite's Part 56 plays the *peer* side with aiortc and never loads
the page -- so the failures this probe exists to catch are exactly the ones
no assertion can reach:

  * the page's own script runs at all (the MSE page once shipped a literal
    `\\n` that killed every overlay while 18 Python cases stayed green --
    AGENTS.md 4.8 red line 4; this page is the same kind of artefact);
  * media negotiates and *decodes* (an H264 stream negotiated as VP8 -- the
    `== 1` vs `"1"` packetization-mode bug of 2026-10-02 -- decodes zero
    frames with no error anywhere except a codec stat nobody reads);
  * frames are *presented*, not merely decoded (a backgrounded window
    presents at 1-2 fps while the decoder keeps up -- two different facts);
  * the overlay draws the sender's counters from `/browser/stats` under the
    page's own session token.

It drives the cached playwright `chrome-headless-shell` over a **minimal
CDP client built from the standard library** -- no websocket package, no
playwright. Neither `--screenshot` nor `--timeout` can be given a real wait
(`--timeout` stops loading *early*; `--virtual-time-budget` fast-forwards
timers, which is the opposite of what a live handshake needs), and there is
no CLI flag for "click 统计 once frames arrive". A page that is never
watched by an instrument cannot be reported on.

Usage -- point it at a viewer URL (the address the menu/card shows, token
included) while a `webrtc` mirror is running:

    scripts/webrtc_page_probe.py \
        --url 'http://127.0.0.1:58999/webrtc?token=<page-token>' \
        --out-dir /tmp/webrtc-shots

Exit code 0 only when the page said 已连接, the video element decoded a
real picture (canvas grab: non-uniform pixels), frames were presented, and
the sender counters moved for this viewer.
"""

import argparse
import base64
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

#: Same candidate list `mse_latency_probe.py` uses, imported rather than
#: copied: two lists would drift, and this machine's only working headless
#: browser is the cached playwright shell -- a GUI-launched process has no
#: shell PATH, so "just call chromium" is the version that fails on the
#: machine it was written for.
try:
    from mse_latency_probe import BROWSER_CANDIDATES
except Exception:                                     # pragma: no cover
    BROWSER_CANDIDATES = []

PAGE_MARK = '/webrtc'


# == the smallest CDP client that can hold this conversation ================

class CDP(object):
    """One websocket to one page target; text frames carry JSON.

    RFC 6455, client side, only the corners the protocol calls for: masked
    client frames, unmasked server frames, continuation frames, ping/pong.
    Every message that is not the reply being waited for is kept as a
    notification -- `Runtime.exceptionThrown` and `Log.entryAdded` are half
    the verdict (a page whose script died reports nothing else at all).
    """

    def __init__(self, ws_url, timeout=30.0):
        path = ws_url.split('://', 1)[1]
        hostport, _, route = path.partition('/')
        host, _, port = hostport.partition(':')
        self.sock = socket.create_connection((host, int(port)), timeout=10)
        self.sock.settimeout(timeout)
        self._buf = b''
        key = base64.b64encode(os.urandom(16)).decode('ascii')
        request = (
            'GET /{route} HTTP/1.1\r\n'
            'Host: {host}:{port}\r\n'
            'Upgrade: websocket\r\n'
            'Connection: Upgrade\r\n'
            'Sec-WebSocket-Key: {key}\r\n'
            'Sec-WebSocket-Version: 13\r\n\r\n').format(
                route=route, host=host, port=port, key=key)
        self.sock.sendall(request.encode('ascii'))
        head = b''
        while not head.endswith(b'\r\n\r\n'):
            byte = self.sock.recv(1)
            if not byte:
                raise RuntimeError('the DevTools socket closed during the '
                                   'websocket handshake')
            head += byte
        if b' 101 ' not in head.split(b'\r\n', 1)[0]:
            raise RuntimeError('DevTools refused the websocket: {}'.format(
                head.split(b'\r\n', 1)[0].decode('latin-1')))
        self._id = 0
        self.notifications = []

    # -- transport ------------------------------------------------------------

    def _read_exact(self, n):
        while len(self._buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError('the DevTools socket closed mid-frame')
            self._buf += chunk
        out, self._buf = self._buf[:n], self._buf[n:]
        return out

    def _send_frame(self, opcode, payload):
        mask = os.urandom(4)
        length = len(payload)
        header = bytes([0x80 | opcode])
        if length < 126:
            header += bytes([0x80 | length])
        elif length < 65536:
            header += bytes([0x80 | 126]) + struct.pack('>H', length)
        else:
            header += bytes([0x80 | 127]) + struct.pack('>Q', length)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def _recv_message(self):
        data = b''
        while True:
            b1, b2 = self._read_exact(2)
            fin, opcode = b1 & 0x80, b1 & 0x0F
            masked, length = b2 & 0x80, b2 & 0x7F
            if length == 126:
                length = struct.unpack('>H', self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack('>Q', self._read_exact(8))[0]
            mask = self._read_exact(4) if masked else None
            chunk = self._read_exact(length)
            if mask:
                chunk = bytes(b ^ mask[i % 4] for i, b in enumerate(chunk))
            if opcode == 0x9:                      # ping -> pong
                self._send_frame(0xA, chunk)
                continue
            if opcode == 0xA:                      # pong: nobody asked
                continue
            if opcode == 0x8:
                raise RuntimeError('the DevTools socket was closed')
            data += chunk
            if fin:
                return data

    # -- the protocol ---------------------------------------------------------

    def call(self, method, params=None, timeout=None):
        self._id += 1
        message_id = self._id
        if timeout is not None:
            self.sock.settimeout(timeout)
        self._send_frame(1, json.dumps(
            {'id': message_id, 'method': method,
             'params': params or {}}).encode('utf-8'))
        while True:
            message = json.loads(self._recv_message().decode('utf-8'))
            if message.get('id') == message_id:
                if 'error' in message:
                    raise RuntimeError('{}: {}'.format(
                        method, message['error'].get('message')))
                return message.get('result', {})
            self.notifications.append(message)

    def evaluate(self, expression, await_promise=False):
        result = self.call('Runtime.evaluate', {
            'expression': expression,
            'returnByValue': True,
            'awaitPromise': await_promise})
        if 'exceptionDetails' in result:
            detail = result['exceptionDetails']
            raise RuntimeError('the page threw while evaluating: {}'.format(
                detail.get('text', detail)))
        return result.get('result', {}).get('value')

    def close(self):
        try:
            self._send_frame(0x8, b'')
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


# == launching and finding the page =========================================

def find_browser(explicit):
    for candidate in ([explicit] if explicit else []) + list(BROWSER_CANDIDATES):
        if candidate and os.path.isfile(candidate) \
                and os.access(candidate, os.X_OK):
            return candidate
    return None


def launch(browser, url, profile, width, height):
    argv = [browser, '--headless=new', '--disable-gpu', '--no-sandbox',
            '--hide-scrollbars', '--mute-audio',
            '--autoplay-policy=no-user-gesture-required',
            '--no-first-run', '--no-default-browser-check',
            '--disable-extensions', '--disable-background-timer-throttling',
            '--disable-renderer-backgrounding',
            '--user-data-dir=' + profile,
            '--remote-debugging-port=0',
            '--window-size={},{}'.format(width, height),
            url]
    env = dict(os.environ)
    for key in ('http_proxy', 'https_proxy', 'HTTP_PROXY', 'HTTPS_PROXY',
                'all_proxy', 'ALL_PROXY', 'PYTHONPATH'):
        env.pop(key, None)
    stderr = open(os.path.join(profile, 'browser.err'), 'wb')
    return subprocess.Popen(argv, env=env, stdout=subprocess.DEVNULL,
                            stderr=stderr), stderr


def page_target(profile, proc, timeout=25.0):
    """(websocket url, devtools port) once the page target exists.

    `--remote-debugging-port=0` has the browser pick the port and drop it
    into `DevToolsActivePort` -- two lines, port then browser path -- so
    there is no port to race for and none to collide with.
    """
    active = os.path.join(profile, 'DevToolsActivePort')
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError('the browser exited with {} before opening '
                               'DevTools'.format(proc.returncode))
        try:
            with open(active) as handle:
                port = int(handle.readline().strip())
        except (OSError, ValueError):
            time.sleep(0.2)
            continue
        try:
            with urllib.request.urlopen(
                    'http://127.0.0.1:{}/json/list'.format(port),
                    timeout=5) as response:
                targets = json.load(response)
        except OSError:
            time.sleep(0.2)
            continue
        for target in targets:
            if target.get('type') == 'page' \
                    and target.get('webSocketDebuggerUrl'):
                return target['webSocketDebuggerUrl'], port
        time.sleep(0.2)
    raise RuntimeError('no page target appeared within {} s'.format(timeout))


# == page-side facts ========================================================

#: An independent presented-frame counter. The page keeps its own (`Q.fps`);
#: this one is installed by the probe, counted by `requestVideoFrameCallback`
#: on the same element, and can only move when a real frame reaches the
#: compositor. Two counters agreeing is the point; one counter is a claim.
INSTALL_COUNTER = """
(function(){
  var x=document.getElementById('v');
  if(!x)return 'no video element';
  window.__probe={n:0,t:Date.now()};
  var step=function(){window.__probe.n++;x.requestVideoFrameCallback(step)};
  x.requestVideoFrameCallback(step);
  return 'installed';
})()
"""

PAGE_FACTS = """
(function(){
  var st=document.getElementById('st'),
      er=document.getElementById('err'),
      x=document.getElementById('v');
  var q=(window.__probe&&window.__probe.n)||0;
  var N=window.N||{}, Q=window.Q||{};
  return {status:st?st.textContent:'',
          err:(er&&er.style.display!=='none'&&er.textContent)?er.textContent:'',
          connection:(window.pc&&window.pc.connectionState)||'',
          video:{w:x?x.videoWidth:0,h:x?x.videoHeight:0},
          probe_frames:q,
          decoded_fps:N.fps||0, presented_fps:Q.fps==='—'?null:Q.fps,
          bytes:N.bytes||0, lost:N.lost||0, recv:N.recv||0,
          rtt:N.rtt==='—'?null:N.rtt, jitter:N.jitter||0,
          role:(DIAG&&DIAG.kind)||'', shape:(window.live&&live.shape)||''};
})()
"""

#: The negotiated codec, the fact the VP8 story turns on: a receiver that was
#: handed VP8 for an H264 payload decodes nothing and logs nothing, but the
#: codec stat names it.
CODEC_STAT = """
new Promise(function(res){
  pc.getStats().then(function(rep){
    var codec=null, rtp=null;
    rep.forEach(function(x){
      if(x.type==='inbound-rtp'&&x.kind==='video')rtp=x;
      if(x.type==='codec')codec=codec||x});
    if(rtp&&codec){rep.forEach(function(x){
      if(x.type==='codec'&&x.id===rtp.codecId)codec=x})}
    res(JSON.stringify({mime:codec?codec.mimeType:null,
                        pt:codec?codec.payloadType:null,
                        framesDecoded:rtp?rtp.framesDecoded:null,
                        framesReceived:rtp?rtp.framesReceived:null}))
  }).catch(function(e){res(JSON.stringify({error:String(e)}))})
})
"""

#: One frame straight off the video element, with its pixel spread: a black
#: screen has min==max, a real desktop picture does not. This is the probe's
#: own witness of "a picture exists" -- separate from anything the page says.
CANVAS_GRAB = """
(function(){
  var x=document.getElementById('v');
  if(!x||!x.videoWidth)return null;
  var c=document.createElement('canvas');
  c.width=x.videoWidth;c.height=x.videoHeight;
  var g=c.getContext('2d');g.drawImage(x,0,0);
  var d=g.getImageData(0,0,c.width,c.height).data;
  var n=0,mn=255,mx=0,sum=0;
  for(var i=0;i<d.length;i+=4*97){
    var l=(d[i]+d[i+1]+d[i+2])/3;
    mn=Math.min(mn,l);mx=Math.max(mx,l);sum+=l;n++}
  return {png:c.toDataURL('image/png'),w:c.width,h:c.height,
          min:Math.round(mn),max:Math.round(mx),
          mean:Math.round(sum/n),samples:n};
})()
"""


def page_errors(cdp):
    """JS exceptions and error-level log entries, favicon excluded."""
    errors = []
    for message in cdp.notifications:
        method = message.get('method')
        params = message.get('params') or {}
        if method == 'Runtime.exceptionThrown':
            details = params.get('exceptionDetails') or {}
            text = details.get('text') or ''
            description = (details.get('exception') or {}).get('description')
            errors.append('exception: {}'.format(description or text))
        elif method == 'Log.entryAdded':
            entry = params.get('entry') or {}
            if entry.get('level') != 'error':
                continue
            url = entry.get('url') or ''
            if entry.get('source') == 'network' and 'favicon' in url:
                continue                       # the page never declared one
            errors.append('{}: {}'.format(
                entry.get('source', '?'), entry.get('text', '')))
    return errors


def http_get_json(url, timeout=5.0):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(url, timeout=timeout) as response:
            return json.loads(response.read().decode('utf-8'))
    except Exception as exc:
        return {'error': repr(exc)}


def png_size(path):
    with open(path, 'rb') as handle:
        head = handle.read(24)
    if head[:8] != b'\x89PNG\r\n\x1a\n':
        return None
    return struct.unpack('>II', head[16:24])


def stats_url(viewer_url):
    #: `/webrtc?token=...` -> `/browser/stats?token=...`; the two share one
    #: door and one token, and the page itself polls this same address.
    base, _, query = viewer_url.partition('?')
    base = base.split('?')[0].rstrip('/')
    if base.endswith(PAGE_MARK):
        base = base[:-len(PAGE_MARK)] + '/browser/stats'
    return base + ('?' + query if query else '')


# == the run ================================================================

def wait_for_picture(cdp, seconds):
    """Poll until the page says connected and a picture exists, or give up.

    Polls rather than sleeps so the facts used for the verdict are the ones
    observed at the moment the wait ends -- a failure report filled in from
    an earlier sample is how "waiting" gets misread as "broken".
    """
    deadline = time.time() + seconds
    facts = {}
    while time.time() < deadline:
        try:
            facts = cdp.evaluate(PAGE_FACTS)
        except RuntimeError as exc:
            return facts, 'the page cannot be inspected: {}'.format(exc)
        if facts.get('err'):
            return facts, facts['err']
        video = facts.get('video') or {}
        if facts.get('connection') == 'connected' \
                and video.get('w') and facts.get('probe_frames'):
            return facts, ''
        time.sleep(0.5)
    return facts, 'no picture within {} s'.format(seconds)


def run(args):
    browser = find_browser(args.browser)
    if not browser:
        print('no headless browser: tried {!r}'.format(
            [args.browser] if args.browser else BROWSER_CANDIDATES))
        return 2
    out_dir = args.out_dir or tempfile.mkdtemp(prefix='macast-webrtc-probe-')
    os.makedirs(out_dir, exist_ok=True)
    profile = tempfile.mkdtemp(prefix='macast-webrtc-profile-')
    widths = [int(w) for w in args.widths.split(',') if w.strip()]
    proc = stderr = cdp = None
    verdict = {'passed': False, 'reasons': [], 'errors': [], 'shots': [],
               'url': args.url, 'browser': browser}
    try:
        proc, stderr = launch(browser, args.url, profile, 1000, args.height)
        ws_url, _port = page_target(profile, proc)
        cdp = CDP(ws_url)
        cdp.call('Runtime.enable')
        cdp.call('Log.enable')
        print('browser: {}'.format(browser))
        print('page   : {}'.format(args.url))
        installed = cdp.evaluate(INSTALL_COUNTER)
        if installed != 'installed':
            verdict['reasons'].append('the video element is not there: '
                                      '{}'.format(installed))
        facts, why = wait_for_picture(cdp, args.seconds)
        verdict['page'] = facts
        if why:
            verdict['reasons'].append(why)
        #: The overlay first: its poll is what fills the page's `live`, and
        #: the shots want it open anyway (one button click, the page's own).
        try:
            cdp.evaluate("document.getElementById('stat').click()")
        except RuntimeError:
            pass
        time.sleep(max(1.5, args.settle))
        try:
            verdict['page'] = cdp.evaluate(PAGE_FACTS)
        except RuntimeError:
            pass
        verdict['codec'] = cdp.evaluate(CODEC_STAT, await_promise=True)
        verdict['live'] = cdp.evaluate('JSON.stringify(window.live||{})')
        verdict['server'] = http_get_json(stats_url(args.url))
        grab = None
        try:
            grab = cdp.evaluate(CANVAS_GRAB)
        except RuntimeError:
            pass
        if grab:
            verdict['frame'] = {'w': grab['w'], 'h': grab['h'],
                                'min': grab['min'], 'max': grab['max'],
                                'mean': grab['mean']}
            path = os.path.join(out_dir, 'frame.png')
            with open(path, 'wb') as handle:
                handle.write(base64.b64decode(
                    grab['png'].split(',', 1)[1]))
            verdict['shots'].append(path)
        for width in widths:
            cdp.call('Emulation.setDeviceMetricsOverride', {
                'width': width, 'height': args.height,
                'deviceScaleFactor': args.zoom, 'mobile': False})
            time.sleep(1.5)             # layout, and a frame or two more
            shot = cdp.call('Page.captureScreenshot', {'format': 'png'})
            path = os.path.join(out_dir, 'webrtc-{}.png'.format(width))
            with open(path, 'wb') as handle:
                handle.write(base64.b64decode(shot['data']))
            size = png_size(path)
            verdict['shots'].append(path)
            if size and size[0] != width * args.zoom:
                verdict['reasons'].append(
                    'shot {} is {} px wide, wanted {}'.format(
                        os.path.basename(path), size[0], width * args.zoom))
        verdict['errors'] = page_errors(cdp)
        # -- the verdict, from the facts and nothing else --------------------
        page = verdict.get('page') or {}
        video = page.get('video') or {}
        codec = {}
        try:
            codec = json.loads(verdict.get('codec') or '{}')
        except (TypeError, ValueError):
            pass
        live = {}
        try:
            live = json.loads(verdict.get('live') or '{}')
        except (TypeError, ValueError):
            pass
        if page.get('status') != '已连接':
            verdict['reasons'].append(
                'the page never said 已连接 (says {!r})'.format(
                    page.get('status')))
        if not video.get('w'):
            verdict['reasons'].append('the video element decoded nothing')
        if not page.get('probe_frames'):
            verdict['reasons'].append('no frame reached the compositor')
        if verdict.get('frame') and grab and grab['max'] - grab['min'] < 4:
            verdict['reasons'].append(
                'the grabbed frame is uniform ({}..{}): black, not a '
                'picture'.format(grab['min'], grab['max']))
        if codec.get('mime') and codec['mime'] != 'video/H264':
            verdict['reasons'].append(
                'the receiver reports codec {}: the sender pipeline speaks '
                'H264 and nothing would decode'.format(codec['mime']))
        if not live.get('clients') or not live.get('sent'):
            verdict['reasons'].append(
                'the sender counters do not see this viewer '
                '(clients={}, sent={})'.format(
                    live.get('clients'), live.get('sent')))
        if verdict['errors']:
            verdict['reasons'].append('the page logged errors')
        verdict['passed'] = not verdict['reasons']
        # -- report -----------------------------------------------------------
        print('')
        print('== the page ==')
        for key in ('status', 'connection', 'decoded_fps', 'presented_fps',
                    'probe_frames'):
            if key in page:
                print('{:>14}  {}'.format(key, page[key]))
        if video.get('w'):
            print('{:>14}  {}x{}'.format('video', video['w'], video['h']))
        if codec:
            print('{:>14}  {} pt={} decoded={} received={}'.format(
                'codec', codec.get('mime'), codec.get('pt'),
                codec.get('framesDecoded'), codec.get('framesReceived')))
        if page.get('recv'):
            print('{:>14}  {} bytes, lost {}/{} · rtt {} · jitter {}'.format(
                'received', page.get('bytes'), page.get('lost'),
                page.get('recv'), page.get('rtt'), page.get('jitter')))
        if verdict.get('frame'):
            frame = verdict['frame']
            print('{:>14}  {}x{} luminance {}..{} (mean {}) over {} samples'
                  .format('grabbed frame', frame['w'], frame['h'],
                          frame['min'], frame['max'], frame['mean'],
                          grab['samples'] if grab else '?'))
        print('== the sender ==')
        print('{:>14}  {}'.format('page sees', live))
        print('{:>14}  {}'.format('fetched', verdict.get('server')))
        print('== page errors ==')
        print('  ' + ('\n  '.join(verdict['errors']) or '(none)'))
        print('== shots ==')
        for shot in verdict['shots']:
            print('  {} ({})'.format(shot, png_size(shot)))
        print('')
        if verdict['passed']:
            print('VERDICT: PASS')
        else:
            print('VERDICT: FAIL')
            for reason in verdict['reasons']:
                print('  - {}'.format(reason))
        if args.json:
            with open(args.json, 'w', encoding='utf-8') as handle:
                json.dump(verdict, handle, indent=2, ensure_ascii=False)
            print('raw verdict -> {}'.format(args.json))
        return 0 if verdict['passed'] else 1
    finally:
        if cdp is not None:
            try:
                cdp.call('Browser.close', timeout=3)
            except Exception:
                pass
            cdp.close()
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if stderr is not None:
            stderr.close()
        if proc is not None and proc.returncode not in (0, None) \
                and not verdict.get('passed'):
            try:
                with open(os.path.join(profile, 'browser.err'),
                          'rb') as handle:
                    tail = handle.read()[-600:].decode('utf-8', 'replace')
                if tail.strip():
                    print('browser stderr tail:\n{}'.format(tail))
            except OSError:
                pass
        if not args.keep:
            shutil.rmtree(profile, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(
        description='validate a WebRTC viewer URL in a real browser')
    parser.add_argument('--url', required=True,
                        help='the viewer URL, page token included')
    parser.add_argument('--out-dir', default=None,
                        help='where the screenshots go '
                             '(default: a fresh temp dir, printed)')
    parser.add_argument('--widths', default='520,1000,1680',
                        help='comma-separated CSS widths to shoot')
    parser.add_argument('--height', type=int, default=820)
    parser.add_argument('--zoom', type=int, default=2,
                        help='deviceScaleFactor for crisp CJK in the shots')
    parser.add_argument('--seconds', type=float, default=30.0,
                        help='how long to wait for a picture')
    parser.add_argument('--settle', type=float, default=2.0,
                        help='seconds between opening the overlay and '
                             'reading counters')
    parser.add_argument('--browser', default=None)
    parser.add_argument('--json', default=None)
    parser.add_argument('--keep', action='store_true',
                        help='keep the browser profile (for post mortems)')
    args = parser.parse_args()
    return run(args)


if __name__ == '__main__':
    sys.exit(main())
