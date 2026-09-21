#!/usr/bin/env python
# -*- coding: utf-8 -*-
# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Start the real app and ask it the questions a stubbed suite cannot.

``verify_cast_airplay.py`` patches every socket, and ``cast_conformance.py``
only ever brings up the receiver protocols. Neither one proves what a user
proves on the first minute of a fresh install: that ``python Macast.py`` comes
up, binds, serves the settings page, imports all nine single-file online
plugins *inside a live process*, answers ``/api``, advertises itself over SSDP,
and leaves nothing behind when it is killed.

What this costs the machine it runs on: for ~30 seconds DLNA telephones on the
LAN see a second renderer (``Macast E2E Smoke``). Everything else is isolated
by construction --
  * a throwaway config dir. ``SETTING_DIR`` is computed at import time by
    appdirs (AGENTS.md section 4.9), so the child patches ``appdirs`` *before*
    importing ``macast``; the log, the settings file, ``logs/`` and the plugin
    dirs all land there.
  * port 58999 instead of 58880, so it never fights a running instance, and
    DLNA only: no Chromecast receiver (8009), no HTTPS channel, no mDNS
    (5353) -- those are protocol plugins that are not enabled here.
  * ``CheckUpdate: false``, no ``StartAtLogin``, a freshly generated
    ``Api_Token`` that belongs to the temp dir and dies with it.

The last thing it checks is the promise those bullets make: the *real* config
dir is hashed before the child starts and again after it is gone, and any
difference is a FAIL.

Usage::

    env -u PYTHONPATH .venv/bin/python scripts/e2e_smoke.py
    env -u PYTHONPATH .venv/bin/python scripts/e2e_smoke.py --play   # spawn mpv too
    env -u PYTHONPATH .venv/bin/python scripts/e2e_smoke.py --keep   # keep the temp dir

``--play`` is off by default because it opens an mpv window on the screen and
needs ffmpeg; a check this machine cannot do is reported SKIP, never passed
(the convention borrowed from ``cast_conformance.py``).

Exit code 0 = nothing failed, 1 = at least one FAIL.
"""

import argparse
import hashlib
import http.server
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
PORT = 58999
FRIENDLY = 'Macast E2E Smoke'
BOOT_TIMEOUT = 60.0
HTTP_TIMEOUT = 8.0
#: How long each of SIGINT / SIGTERM gets to stop a Cocoa run loop that has no
#: reason to notice a signal (see `teardown`).
SIGNAL_GRACE = 8.0
#: The protocol group is filled a moment after the HTTP service says it is
#: running; see the `subscribers` check for the measurement this covers.
SUBSCRIBERS_TIMEOUT = 15.0

# Proxy variables must go: a proxy in the environment makes every 127.0.0.1
# request leave the machine (AGENTS.md section 4.1 / section 5), which would
# turn a working app into a "the settings page does not answer" report. This
# list is a copy of `Setting.PROXY_ENV_VARS` on purpose -- this script must not
# import the package it is about to launch (Part 33 of the verification suite
# holds both halves of that to account).
PROXY_VARS = ('http_proxy', 'HTTP_PROXY', 'https_proxy', 'HTTPS_PROXY',
              'ftp_proxy', 'FTP_PROXY',
              'all_proxy', 'ALL_PROXY', 'no_proxy', 'NO_PROXY')

_results = []


def record(name, state, detail=''):
    _results.append((state, name, detail))
    print('  [%-4s] %s%s' % (state, name, ('  -- ' + detail) if detail else ''))


def ok(name, condition, detail=''):
    record(name, 'PASS' if condition else 'FAIL', detail)
    return bool(condition)


def skip(name, reason):
    record(name, 'SKIP', reason)


def info(name, detail=''):
    """Something worth reading in the report that is not a verdict.

    Used where the answer is a finding about the machine rather than a
    pass/fail contract -- e.g. which signal it actually took to stop a Cocoa
    app. The convention is the same as ``cast_conformance.py``'s: a check this
    machine cannot satisfy is SKIPPED, never silently passed, and things that
    are merely informative do not pretend to be either.
    """
    record(name, 'INFO', detail)


def section(title):
    print('\n=== %s ===' % title)


# ---------------------------------------------------------------------------
# The child's environment: one temp dir, and nothing else
# ---------------------------------------------------------------------------

def hash_config_dir(path):
    """A digest of the user's real config dir, for "nothing was written".

    Two things make this more than a hash of the tree:

    * File **names** are part of the digest even where content is not, so a
      plugin installed into ``renderer/``, a ``.trash`` entry or a stray
      ``logs/<Plugin>.log`` shows up as a difference.
    * Log **content** is deliberately ignored. The user's own Macast is
      usually running (measured here: ``macast.log`` grows ~27 KB/min,
      AGENTS.md section 4.10), so comparing those bytes would make this check
      fail on a machine where nothing of ours was written -- and a check that
      cannot pass gets ignored, which is worse than no check.

    Returns ``''`` when the directory does not exist.
    """
    if not os.path.isdir(path):
        return 'ABSENT'
    names, digest = [], hashlib.sha256()
    for root, dirs, files in os.walk(path):
        dirs.sort()
        rel = os.path.relpath(root, path)
        for name in sorted(files):
            sub = os.path.join(rel, name)
            names.append(sub)
            if name.endswith('.log'):
                continue
            try:
                with open(os.path.join(root, name), 'rb') as fh:
                    blob = fh.read()
            except OSError:
                blob = b'<unreadable>'
            digest.update(sub.encode('utf-8'))
            digest.update(b'\0')
            digest.update(hashlib.sha256(blob).digest())
    digest.update(b'#')
    digest.update(','.join(sorted(names)).encode('utf-8'))
    return '%d files, %s' % (len(names), digest.hexdigest()[:12])


def real_config_dir():
    """The config dir the app would use if we did not patch appdirs.

    Computed here rather than imported from ``macast.utils`` so this script
    never has to import the package it is about to launch -- the child does
    that, with the patch in place.
    """
    if sys.platform == 'darwin':
        return os.path.expanduser(
            '~/Library/Application Support/Macast')
    if sys.platform.startswith('win'):
        return os.path.join(os.environ.get('APPDATA', ''), 'Macast', 'xfangfang')
    return os.path.expanduser('~/.config/Macast/xfangfang')


BOOTSTRAP = '''\
"""Launch Macast with its config directory redirected (see e2e_smoke.py).

The order is the whole point: ``macast.utils`` computes SETTING_DIR at import
time, so appdirs has to be patched first -- and before anything imports
``macast``, which is why this is a script the child runs rather than a flag
the app has.
"""
import sys
sys.path.insert(0, r'%(repo)s')
import appdirs
appdirs.user_config_dir = lambda appname, appauthor=None, *a, **kw: r'%(temp)s'
import runpy
runpy.run_path(r'%(entry)s', run_name='__main__')
'''


def write_bootstrap(temp_dir):
    path = os.path.join(temp_dir, '_e2e_bootstrap.py')
    with open(path, 'w', encoding='utf-8') as fh:
        fh.write(BOOTSTRAP % {'temp': temp_dir, 'repo': REPO,
                              'entry': os.path.join(REPO, 'Macast.py')})
    return path


def seed_settings(temp_dir, token):
    path = os.path.join(temp_dir, 'macast_setting.json')
    with open(path, 'w', encoding='utf-8') as fh:
        json.dump({
            'ApplicationPort': PORT,
            'Macast_Protocols': ['DLNA Protocol'],
            'DLNA_FriendlyName': FRIENDLY,
            'CheckUpdate': False,
            'MenubarIcon': True,
            'Api_Token': token,
        }, fh, indent=4, sort_keys=True)
    return path


def install_online_plugins(temp_dir):
    """Copy every indexed online plugin into the temp dir, as if installed.

    Copying (not the install API) keeps this a *startup* test: the child has to
    import each file from disk the way a returning user's Macast does, with no
    CherryPy request and no stubbed ``requests`` behind it. The index is the
    source of truth for which file is which kind, so a wrong ``renderer`` /
    ``protocol`` field in ``plugins/info.json`` shows up here as an import
    failure in the right column.
    """
    with open(os.path.join(REPO, 'plugins', 'info.json'), encoding='utf-8') as fh:
        index = json.load(fh)
    placed = {}
    for entry in index.get('plugin_v1', []):
        kind = 'protocol' if entry.get('protocol') else 'renderer'
        name = entry.get('name') or os.path.basename(
            (entry.get('url') or '').split('?')[0])
        src = os.path.join(REPO, 'plugins', name)
        if not (name.endswith('.py') and os.path.isfile(src)):
            placed[entry.get('title', '?')] = 'MISSING-SOURCE:%s' % name
            continue
        target_dir = os.path.join(temp_dir, kind)
        if not os.path.isdir(target_dir):
            os.makedirs(target_dir)
        # The app itself puts an __init__.py in these directories (see
        # MacastPluginManager.create_plugin_dir); the loader imports them as
        # packages, so a hand-copied plugin needs one too.
        open(os.path.join(target_dir, '__init__.py'), 'a').close()
        shutil.copyfile(src, os.path.join(target_dir, name))
        placed[entry.get('title', '?')] = {'kind': kind, 'name': name, 'entry': entry}
    return placed


# ---------------------------------------------------------------------------
# Talking to the child
# ---------------------------------------------------------------------------

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **kw):
        return None


def fetch(url, data=None, timeout=HTTP_TIMEOUT, headers=None):
    """Return (status, headers, body bytes); never raises for an HTTP error.

    `ProxyHandler({})` because *this* process is the one making the 127.0.0.1
    requests: stripping the proxy variables from the child's environment does
    nothing for the harness's own urllib calls.
    """
    req = urllib.request.Request(url, data=data, headers=headers or {})
    opener = urllib.request.build_opener(NoRedirect,
                                         urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=timeout) as resp:
            return resp.getcode(), dict(resp.headers), resp.read()
    except urllib.error.HTTPError as e:
        try:
            body = e.read()
        except Exception:
            body = b''
        return e.code, dict(e.headers or {}), body
    except Exception as e:
        return None, {}, repr(e).encode()


def api(query, token=None, timeout=HTTP_TIMEOUT):
    url = 'http://127.0.0.1:%d/api?query=%s' % (PORT, query)
    if token:
        url += '&token=' + urllib.parse.quote(token)
    status, _, body = fetch(url, timeout=timeout)
    try:
        return status, json.loads(body.decode('utf-8'))
    except Exception:
        return status, body


def child_children(pid):
    """PIDs whose parent is `pid` -- mpv, ffmpeg, caffeinate, uxplay."""
    try:
        out = subprocess.run(['pgrep', '-P', str(pid)],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                             timeout=10).stdout
    except Exception:
        return []
    return [int(x) for x in out.split()]


def port_is_free(port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind(('127.0.0.1', port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


def ssdp_search(target_port, seconds=4.0):
    """M-SEARCH for MediaRenderer; return every LOCATION naming `target_port`.

    The LAN already carries other renderers (the user's own Macast among
    them), so the match is on the port this instance was given -- that is what
    proves *our* SSDP responder answered rather than a neighbour's.
    """
    request = (
        'M-SEARCH * HTTP/1.1\r\n'
        'HOST: 239.255.255.250:1900\r\n'
        'MAN: "ssdp:discover"\r\n'
        'MX: 2\r\n'
        'ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n'
        '\r\n').encode()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.settimeout(0.4)
    locations = []
    try:
        sock.bind(('0.0.0.0', 0))
        for _ in range(3):
            try:
                sock.sendto(request, ('239.255.255.250', 1900))
            except OSError:
                break
            deadline = time.time() + seconds / 3.0
            while time.time() < deadline:
                try:
                    data, _ = sock.recvfrom(4096)
                except socket.timeout:
                    continue
                except OSError:
                    break
                for line in data.split(b'\r\n'):
                    if line.upper().startswith(b'LOCATION:'):
                        locations.append(line.split(b':', 1)[1].decode(
                            'utf-8', 'replace').strip())
    finally:
        sock.close()
    needle = ':%d' % target_port
    return locations, [loc for loc in locations if needle in loc]


# ---------------------------------------------------------------------------
# --play: a stream mpv can actually open
# ---------------------------------------------------------------------------

def find_tool(name):
    found = shutil.which(name)
    if found:
        return found
    for directory in ('/opt/homebrew/opt/%s/bin' % name, '/opt/homebrew/bin',
                      '/usr/local/opt/%s/bin' % name, '/usr/local/bin'):
        candidate = os.path.join(directory, name)
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


class RangeFileHandler(http.server.BaseHTTPRequestHandler):
    """Serve one file and honour Range.

    mpv needs Range for any seek or prefetch; ``SimpleHTTPRequestHandler`` does
    not implement it, and the failure looks like a player bug
    (``end-file reason=error``) rather than a server gap (AGENTS.md section 4.9).
    """

    payload = b''
    mime = 'video/mp4'

    def log_message(self, *a):
        pass

    def do_HEAD(self):
        self._serve(head_only=True)

    def do_GET(self):
        self._serve()

    def _serve(self, head_only=False):
        total = len(self.payload)
        start, end, status = 0, max(total - 1, 0), 200
        rng = self.headers.get('Range')
        if rng and rng.startswith('bytes='):
            first, _, last = rng[len('bytes='):].split(',')[0].strip().partition('-')
            try:
                if first:
                    start = int(first)
                    end = int(last) if last else total - 1
                else:
                    start = max(0, total - int(last))
                status = 206
            except ValueError:
                self.send_error(400)
                return
            start = max(0, min(start, total - 1))
            end = max(start, min(end, total - 1))
        chunk = self.payload[start:end + 1]
        self.send_response(status)
        self.send_header('Content-Type', self.mime)
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(len(chunk)))
        if status == 206:
            self.send_header('Content-Range',
                             'bytes %d-%d/%d' % (start, end, total))
        self.end_headers()
        if not head_only:
            self.wfile.write(chunk)


def make_test_clip(ffmpeg, where):
    """A short, silent-enough, video-bearing MP4 -- the shape a phone sends."""
    clip = os.path.join(where, 'e2e_clip.mp4')
    cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y',
           '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=25',
           '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
           '-t', '12', '-c:v', 'libx264', '-preset', 'ultrafast',
           '-pix_fmt', 'yuv420p', '-movflags', '+faststart',
           '-c:a', 'aac', '-shortest', clip]
    try:
        proc = subprocess.run(cmd, stdout=subprocess.DEVNULL,
                              stderr=subprocess.PIPE, timeout=120)
    except Exception as e:
        return None, repr(e)
    if proc.returncode != 0 or not os.path.isfile(clip):
        return None, proc.stderr.decode('utf-8', 'replace')[-300:]
    return clip, ''


def _clock(text):
    """A playback position in seconds, or None when there is nothing to read.

    DLNA reports `H:MM:SS`, mpv's raw property is a float of seconds, and the
    two are not interchangeable as strings -- so compare them as numbers.
    """
    text = str(text or '').strip()
    if not text:
        return None
    try:
        total = 0.0
        for part in text.split(':'):
            total = total * 60 + float(part)
        return total
    except ValueError:
        return None


def routable_ip():
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(('10.255.255.255', 1))
        return probe.getsockname()[0]
    except OSError:
        return '127.0.0.1'
    finally:
        probe.close()


def playback_round(token, temp_dir, log_path):
    """Cast a generated clip to the running app and watch mpv confirm it."""
    ffmpeg = find_tool('ffmpeg')
    if not ffmpeg:
        skip('playback: a real mpv round-trip', 'no ffmpeg on this machine to '
             'generate the test clip; --play is opt-in either way')
        return
    clip, err = make_test_clip(ffmpeg, temp_dir)
    if not clip:
        record('playback: a real mpv round-trip', 'FAIL',
               'ffmpeg could not generate the clip: %s' % err)
        return
    with open(clip, 'rb') as fh:
        payload = fh.read()
    RangeFileHandler.payload = payload
    httpd = http.server.ThreadingHTTPServer(('0.0.0.0', 0), RangeFileHandler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    try:
        url = 'http://%s:%d/e2e_clip.mp4' % (routable_ip(), httpd.server_address[1])
        # The token-bearing GET, exactly as a phone Shortcut would call it.
        cast_url = ('http://127.0.0.1:%d/api?query=cast&url=%s&title=%s&token=%s'
                    % (PORT, urllib.parse.quote(url, safe=''),
                       urllib.parse.quote('E2E Smoke Clip'), token))
        _, _, body = fetch(cast_url)
        try:
            res = json.loads(body.decode())
        except Exception:
            res = {'raw': body[:200].decode('utf-8', 'replace')}
        if not ok('playback: the web cast endpoint accepted the url',
                  isinstance(res, dict) and res.get('code') == 0, str(res)[:200]):
            return

        playing, position = '', ''
        deadline = time.time() + 30
        while time.time() < deadline:
            _, st = api('status')
            media = (st or {}).get('media', {}) if isinstance(st, dict) else {}
            playing = media.get('TransportState', '')
            position = media.get('RelativeTimePosition', '') or ''
            if playing == 'PLAYING':
                break
            time.sleep(1.0)
        ok('playback: mpv reported PLAYING', playing == 'PLAYING',
           'TransportState=%r' % playing)

        # Comparing the reported strings is not enough: the two ways this
        # project renders a position ('00:00:00' from the state cache,
        # '0:00:00' from the raw property) differ without time having moved.
        # Parse it and require actual progress.
        first = _clock(position)
        latest, latest_raw = first, position
        for _ in range(10):
            time.sleep(1.0)
            _, st = api('status')
            media = (st or {}).get('media', {}) if isinstance(st, dict) else {}
            latest_raw = media.get('RelativeTimePosition', '') or ''
            got = _clock(latest_raw)
            if got is not None and (latest is None or got > latest):
                latest = got
            if first is not None and latest is not None and latest > first:
                break
        ok('playback: the position is moving (so the stream really decoded)',
           first is not None and latest is not None and latest > first,
           '%s (%s s) -> %s (%s s)' % (position, first, latest_raw, latest))

        try:
            with open(log_path, 'rb') as fh:
                log = fh.read().decode('utf-8', 'replace')
        except OSError:
            log = ''
        ok('playback: mpv saw a video track, not audio-only',
           'video-reconfig' in log,
           'AGENTS.md section 5: audio-reconfig alone means the stream had no video')
    finally:
        httpd.shutdown()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--play', action='store_true',
                        help='also cast a generated clip and watch mpv confirm it '
                             '(opens a player window on this screen)')
    parser.add_argument('--keep', action='store_true',
                        help='keep the temporary config dir instead of deleting it')
    args = parser.parse_args(argv)

    python = sys.executable
    token = uuid.uuid4().hex
    temp_dir = tempfile.mkdtemp(prefix='macast-e2e-')
    real_dir = real_config_dir()
    real_before = hash_config_dir(real_dir)

    print('Macast end-to-end smoke')
    print('  repo      %s' % REPO)
    print('  temp dir  %s' % temp_dir)
    print('  port      %d (protocols: DLNA only)' % PORT)
    print('  NOTICE    for ~30 s DLNA clients on this LAN will see a second '
          'renderer named "%s".' % FRIENDLY)

    if not os.path.isfile(os.path.join(REPO, 'Macast.py')):
        print('Macast.py not next to this script; run it from the repo.')
        return 1
    if not port_is_free(PORT):
        section('preflight')
        record('the smoke port is free', 'FAIL',
               'something is already on %d -- refusing to share it' % PORT)
        return 1

    placed = {}
    proc = None
    spawned = []
    log_path = os.path.join(temp_dir, 'macast.log')
    try:
        section('preflight')
        ok('the smoke port is free', True, 'port %d' % PORT)
        placed = install_online_plugins(temp_dir)
        ok('every indexed online plugin has a source file',
           all(not str(v).startswith('MISSING-SOURCE') for v in placed.values()),
           'index entries=%d, missing=%s' % (
               len(placed), [k for k, v in placed.items()
                             if str(v).startswith('MISSING-SOURCE')]))
        seed_settings(temp_dir, token)

        env = dict(os.environ)
        for name in PROXY_VARS:
            env.pop(name, None)
        env.pop('PYTHONPATH', None)   # the CLI sandbox shim (AGENTS.md section 5)
        env['MACAST_E2E'] = '1'
        started = time.time()
        proc = subprocess.Popen(
            [python, write_bootstrap(temp_dir)], cwd=REPO, env=env,
            stdout=open(os.path.join(temp_dir, 'stdout.log'), 'wb'),
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True)

        section('the app comes up')
        boot = None
        while time.time() - started < BOOT_TIMEOUT:
            status, res = api('status', timeout=3)
            if status == 200 and isinstance(res, dict) \
                    and (res.get('server') or {}).get('running'):
                boot = res
                break
            if proc.poll() is not None:
                break
            time.sleep(1.0)
        if not ok('the service answers /api?query=status', boot is not None,
                  'took %.1f s' % (time.time() - started) if boot else
                  'child exit=%s; see %s' % (proc.poll(),
                                             os.path.join(temp_dir, 'stdout.log'))):
            section('what the child printed')
            _dump_tail(os.path.join(temp_dir, 'stdout.log'), 4000)
            skip('every check that needs a running app',
                 'the service never answered /api?query=status')
            spawned = []
            teardown(proc, spawned)
            return closing_report(temp_dir, args.keep, real_dir, real_before)

        server = boot.get('server', {})
        media = boot.get('media', {})
        info('boot time', '%.1f s from exec to a running service' % (time.time() - started))
        ok('it is the instance we started (port, name, DLNA only)',
           server.get('port') == PORT
           and server.get('friendly_name') == FRIENDLY
           and server.get('protocol') == ['DLNA Protocol'],
           'port=%s name=%r protocols=%s' % (server.get('port'),
                                             server.get('friendly_name'),
                                             server.get('protocol')))
        version_file = ''
        try:
            with open(os.path.join(REPO, 'macast', '.version'),
                      encoding='utf-8') as fh:
                version_file = fh.read().strip()
        except OSError:
            pass
        ok('it reports the version this checkout says it is',
           bool(version_file) and server.get('version') == version_file,
           'reported=%s macast/.version=%s' % (server.get('version'), version_file))
        ok('nothing is playing before we ask it to',
           (media.get('TransportState') or '') != 'PLAYING'
           and not (media.get('CurrentURI') or media.get('CurrentTrackURI') or ''),
           'TransportState=%r uri=%r' % (media.get('TransportState'),
                                         media.get('CurrentURI')))

        section('the settings page')
        status, headers, body = fetch('http://127.0.0.1:%d/' % PORT)
        html = body.decode('utf-8', 'replace')
        ok('GET / serves the settings page',
           status == 200 and 'Macast' in html and len(html) > 20000,
           'status=%s bytes=%d' % (status, len(html)))
        status, res = api('plugin-info')
        infos = (res or {}).get('plugins', []) if isinstance(res, dict) else []
        by_title = {}
        for item in infos:
            by_title.setdefault(item.get('title'), []).append(item)
        missing = []
        mismatched = []
        for title, placed_info in placed.items():
            if isinstance(placed_info, str):
                continue
            found = by_title.get(title)
            if not found:
                missing.append(title)
                continue
            got = found[0]
            want = placed_info['entry']
            want_version = want.get('version')
            want_kind = placed_info['kind']
            if (got.get('version') != want_version
                    or got.get('type') != want_kind
                    or not got.get('installed')):
                mismatched.append('%s(v=%s want=%s type=%s want=%s installed=%s)'
                                  % (title, got.get('version'), want_version,
                                     got.get('type'), want_kind,
                                     got.get('installed')))
        ok('all %d online plugins imported in a live process' % len(placed),
           not missing and not mismatched,
           'not loaded=%s mismatched=%s' % (missing, mismatched))
        not_available = [t for t, group in by_title.items()
                         if group and group[0].get('installed')
                         and not group[0].get('available')]
        ok('every installed plugin claims it can run on this machine',
           not not_available, 'unusable=%s' % not_available)
        ok('the index coordinates are handed to the page, not hardcoded in it',
           isinstance(res, dict) and bool((res.get('plugin_repo') or {})
                                          .get('index_urls')),
           'plugin_repo=%s' % json.dumps((res or {}).get('plugin_repo', {}))[:160])

        section('the management API a bug report leans on')
        status, res = api('log')
        log_text = (res or {}).get('logs', '') if isinstance(res, dict) else ''
        ok('the log tail is readable through /api',
           status == 200 and len(log_text) > 100,
           '%d chars' % len(log_text))
        status, res = api('log-modules')
        ok('the log tab can list its module files',
           status == 200 and isinstance(res, dict)
           and (res.get('main') or {}).get('size', 0) > 0,
           'main=%s modules=%d' % ((res or {}).get('main'),
                                   len((res or {}).get('modules') or [])))
        status, res = api('interfaces')
        nets = (res or {}).get('interfaces') or [] if isinstance(res, dict) else []
        ok('the network picker has something to offer',
           status == 200 and len(nets) > 0,
           '%d interfaces, advertised=%s' % (
               len(nets), (res or {}).get('advertised') if isinstance(res, dict)
               else '?'))
        # `/api?query=status` reports `running` as soon as CherryPy is up, and
        # that is measurably earlier than the protocol group having any children
        # in it: across runs here this query answered `children=[]` once at 1.1 s
        # of uptime and `[('DLNA Protocol', True)]` at 2.1 s. The settings page
        # polls, so the race is invisible to a user -- but a smoke test that
        # asks once would flap, so ask until it is answerable and report the
        # wait, because the wait is the finding.
        children = []
        waited = 0.0
        while waited <= SUBSCRIBERS_TIMEOUT:
            status, res = api('subscribers')
            children = ((res or {}).get('children') or []
                        if isinstance(res, dict) else [])
            if children:
                break
            time.sleep(0.5)
            waited += 0.5
        ok('the DLNA protocol is installed and its event thread is alive '
           '(this is what a subscriber needs)',
           status == 200 and any(c.get('event_thread_alive') for c in children),
           'after %.1f s of polling; children=%s' % (
               waited, [(c.get('title'), c.get('event_thread_alive'),
                         c.get('running')) for c in children]))
        status, res = api('cast-info')
        ok('cast-info returns the token we seeded (so a Shortcut can be configured)',
           isinstance(res, dict) and res.get('token') == token
           and res.get('port') == PORT,
           'port=%s token_matches=%s' % ((res or {}).get('port'),
                                         isinstance(res, dict)
                                         and res.get('token') == token))
        status, res = api('module-settings')
        ok('module-settings answers', status == 200 and isinstance(res, dict),
           'status=%s' % status)
        # AGENTS.md section 4.7: a page the user visits can fire a GET at
        # 127.0.0.1, so the cast query must reject loopback without a token.
        # Part 12 proves this against a fake request context; this proves it
        # against a socket.
        _, res = api('cast')
        code = res.get('code') if isinstance(res, dict) else None
        ok('GET cast from loopback without a token is refused', code == 403,
           str(res)[:160])
        _, res = fetch('http://127.0.0.1:%d/api?query=cast&url=%s&token=%s'
                      % (PORT, urllib.parse.quote('/tmp/x.mp4'), token))[1:]
        try:
            res = json.loads(res.decode())
        except Exception:
            pass
        ok('GET cast refuses a bare path and says why (AGENTS.md section 4.7)',
           isinstance(res, dict) and res.get('code') == 1
           and 'absolute' in (res.get('message') or ''), str(res)[:160])

        section('what the LAN gets to see for these few seconds')
        locations, ours = ssdp_search(PORT)
        ok('an M-SEARCH for MediaRenderer is answered by *this* instance',
           bool(ours), '%d LOCATION answers, %d of them ours' % (
               len(locations), len(ours)))
        if ours:
            status, _, body = fetch(ours[0])
            desc = body.decode('utf-8', 'replace')
            named = re.search(r'<friendlyName>([^<]*)</friendlyName>', desc)
            ok('the description it points at parses and carries our name',
               status == 200 and bool(named)
               and named.group(1) == FRIENDLY,
               'status=%s friendlyName=%r' % (status,
                                              named and named.group(1)))
            ok('it advertises an AVTransport control service (that is what a '
               'phone dials)', 'AVTransport' in desc and 'serviceList' in desc,
               '%d bytes of XML' % len(desc))
        else:
            for loc in locations[:5]:
                print('    other renderer on the LAN: %s' % loc)

        section('the log a bug report would be built from')
        try:
            with open(log_path, encoding='utf-8', errors='replace') as fh:
                text = fh.read()
        except OSError as e:
            text = ''
            record('the log file exists', 'FAIL', repr(e))
        ok('the log file exists and is what /api?query=log showed',
           os.path.isfile(log_path) and os.path.getsize(log_path) > 0,
           '%s (%d bytes)' % (log_path, len(text)))
        ok('nothing raised an unhandled exception',
           'Traceback (most recent call last' not in text,
           'the log has %d lines' % text.count('\n'))
        noisy = [line for line in text.splitlines()
                 if ' ERROR ' in line and 'Loading Language' not in line]
        print('    %d ERROR lines (informational -- some are how this app '
              'reports normal state):' % len(noisy))
        for line in noisy[:8]:
            print('    | %s' % line[:160])

        if args.play:
            section('playback (--play)')
            playback_round(token, temp_dir, log_path)
        else:
            skip('playback: a real mpv round-trip',
                 'not requested; pass --play to cast a generated clip '
                 '(opens a window and needs ffmpeg)')

        # Anything the app spawned must be gone when it exits, so take the
        # inventory while it is still alive to answer for it.
        spawned = child_children(proc.pid)
    except Exception:
        import traceback
        traceback.print_exc()
        record('the smoke test itself completed', 'FAIL', 'see traceback above')
    finally:
        # Last resort only: `teardown` below is what normally stops the child,
        # so an exception in the middle of the checks cannot leave a second
        # Macast running on the user's machine.
        if proc is not None and proc.poll() is None:
            try:
                os.kill(proc.pid, signal.SIGKILL)
            except OSError:
                pass

    teardown(proc, spawned)
    return closing_report(temp_dir, args.keep, real_dir, real_before)


def _pid_alive(pid):
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def _dump_tail(path, chars, header=''):
    try:
        with open(path, encoding='utf-8', errors='replace') as fh:
            text = fh.read()[-chars:]
    except OSError:
        return
    if not text.strip():
        return
    if header:
        print('\n--- %s ---' % header)
    print(text)


def reap(pid, grace=6.0):
    """SIGTERM one process, escalate to SIGKILL, and report whether it died."""
    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        return True
    deadline = time.time() + grace
    while time.time() < deadline and _pid_alive(pid):
        time.sleep(0.2)
    if not _pid_alive(pid):
        return True
    try:
        os.kill(pid, signal.SIGKILL)
        time.sleep(0.3)
    except OSError:
        pass
    return not _pid_alive(pid)


def teardown(proc, spawned):
    """Stop the child, and make sure this run leaves no process behind.

    Measured over the runs that produced this file: **SIGINT stops a menu-bar
    Macast, SIGTERM does not** -- the Cocoa run loop never returns to Python
    bytecode to run the default handler until some UI event arrives, which is
    also what rumps warns about on stdout. So `kill <pid>` is not a way to
    restart this app; AGENTS.md section 4.2's "kill the old instance before
    starting a second one" has to mean `kill -2` (or the menu), and a plain
    `kill -9` skips the teardown entirely.

    Neither signal reliably stops the player either: the idle mpv was still
    running after the app exited in two of three runs, so a Macast that gets
    restarted over and over accumulates players (the instance this was written
    against had four). Reaping is therefore this script's job, and the hard
    check is the one a user feels: is everything this run started gone when it
    finishes?
    """
    section('teardown')
    how = 'already exited'
    for sig, label in ((signal.SIGINT, 'SIGINT'), (signal.SIGTERM, 'SIGTERM')):
        try:
            os.kill(proc.pid, sig)
        except OSError:
            break
        deadline = time.time() + SIGNAL_GRACE
        while time.time() < deadline and proc.poll() is None:
            time.sleep(0.2)
        if proc.poll() is not None:
            how = label
            break
    if proc.poll() is None:
        try:
            os.kill(proc.pid, signal.SIGKILL)
            proc.wait(timeout=10)
        except OSError:
            pass
        how = 'SIGKILL'
    info('what it took to stop the app',
         '%s (after %s s of grace per signal)' % (how, SIGNAL_GRACE))

    # Give a well-behaved teardown a moment: an mpv the app killed is
    # reparented to launchd and reaped there, and `os.kill(pid, 0)` answers
    # "alive" for a zombie in between. The pid-reuse window left by this wait
    # (seconds, against a 4-byte counter) is not one worth guarding.
    time.sleep(1.5)
    survivors = [pid for pid in spawned if _pid_alive(pid)]
    reaped = list(survivors)
    if survivors:
        # The app never got to run its own teardown, so the idle mpv (and
        # anything else it had started) is this script's mess to clean up.
        for pid in survivors:
            reap(pid)
        survivors = [pid for pid in spawned if _pid_alive(pid)]
    info('who reaped the %d child process(es)' % len(spawned),
         ('the app itself (it stopped on %s)' % how) if not reaped else
         ('this script had to kill %s after the app stopped on %s' % (reaped, how)))
    ok('nothing this run started is still running', not survivors,
       '%d children seen%s' % (len(spawned),
                               '' if not survivors else ', survivors=%s' % survivors))
    deadline = time.time() + 10
    while time.time() < deadline and not port_is_free(PORT):
        time.sleep(0.5)
    ok('port %d is free again' % PORT, port_is_free(PORT))


def closing_report(temp_dir, keep, real_dir, real_before):
    """Prove the isolation was real, clean up, and print the verdict."""
    real_after = hash_config_dir(real_dir)
    ok('the user wrote nothing of ours into the real config dir',
       real_after == real_before,
       '%s -> %s (dir=%s; log files excluded on purpose, the running instance '
       'is appending to them)' % (real_before, real_after, real_dir))
    if any(state == 'FAIL' for state, _, _ in _results):
        _dump_tail(os.path.join(temp_dir, 'stdout.log'), 3000,
                   'what the child printed on stdout/stderr')
    if keep:
        print('  kept: %s' % temp_dir)
    else:
        shutil.rmtree(temp_dir, ignore_errors=True)

    section('summary')
    passed = [r for r in _results if r[0] == 'PASS']
    failed = [r for r in _results if r[0] == 'FAIL']
    skipped = [r for r in _results if r[0] == 'SKIP']
    infos = [r for r in _results if r[0] == 'INFO']
    print('%d passed, %d failed, %d skipped, %d informational (%d checks)' % (
        len(passed), len(failed), len(skipped), len(infos), len(_results)))
    for _state, name, detail in failed:
        print('  FAIL %s -- %s' % (name, detail))
    for _state, name, detail in skipped:
        print('  SKIP %s -- %s' % (name, detail))
    for _state, name, detail in infos:
        print('  INFO %s -- %s' % (name, detail))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
