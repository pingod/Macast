"""
py2app build script for Macast on macOS.

Builds:    dist/Macast.app
Run from:  project root, with the build virtualenv activated

Usage:
    python scripts/setup_py2app.py py2app          # build for the host arch
    python scripts/setup_py2app.py py2app --arch=arm64
    python scripts/setup_py2app.py py2app --arch=x86_64

The bundle layout matches what Macast.py expects at runtime:
    Macast.app/Contents/Resources/bin/MacOS/mpv   <-- mpv binary lives here
    Macast.app/Contents/Resources/i18n/...        <-- translations
    Macast.app/Contents/MacOS/Macast              <-- the py2app launcher
"""

import argparse
import os
import platform
import sys
import datetime
import subprocess
from setuptools import setup


def parse_arch():
    """Decide the target arch. CLI arg wins, then env, then the host machine."""
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('--arch', default=None,
                        choices=('arm64', 'x86_64'))
    args, _unknown = parser.parse_known_args()

    if args.arch:
        return args.arch
    if os.environ.get('MACAST_ARCH') in ('arm64', 'x86_64'):
        return os.environ['MACAST_ARCH']
    # Host detection: Apple Silicon → arm64, everything else → x86_64.
    machine = platform.machine().lower()
    if machine in ('arm64', 'aarch64'):
        return 'arm64'
    return 'x86_64'


# ---------------------------------------------------------------------------
# Compatibility shim: py2app 0.28 calls `distutils.util.spawn(cmd, verbose=...)`
# but setuptools 84 dropped `verbose` from its spawn shim, causing build to
# crash on Python 3.12 with `TypeError: Popen.__init__() got an unexpected
# keyword argument 'verbose'`. We monkey-patch the spawn entry point that
# py2app imports at build time so the call succeeds.
def _patch_py2app_spawn():
    """py2app 0.28 calls `distutils.util.spawn(cmd, verbose=...)` from inside
    `byte_compile`. setuptools 84 dropped the `verbose` kwarg from its spawn
    shim, crashing on Python 3.12 with
    `TypeError: Popen.__init__() got an unexpected keyword argument 'verbose'`.

    We can't reach the local binding inside py2app.util.byte_compile, so we
    wrap `distutils.util.spawn` itself: drop unknown kwargs before delegating.
    """
    try:
        import distutils.util as _du
    except ImportError:
        return
    _orig = _du.spawn

    def _spawn(cmd, **kwargs):
        for stale in ('verbose', 'dry_run'):
            kwargs.pop(stale, None)
        return _orig(cmd, **kwargs)

    _du.spawn = _spawn
    # Some distutils versions also expose spawn via the `spawn` name imported
    # into distutils.spawn itself; cover both.
    try:
        import distutils.spawn as _ds
        _ds.spawn = _spawn
    except ImportError:
        pass


_patch_py2app_spawn()
# ---------------------------------------------------------------------------

# Project root is the parent of this script's directory.
PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir))

# Resolve the bundled mpv binary. We prefer the system Homebrew arm64 mpv
# (so the app uses the same architecture as the user's machine). If not found
# we fall back to an mpv shipped next to this script.
TARGET_ARCH = parse_arch()

# mpv locations: prefer the architecture that matches TARGET_ARCH.
MPV_CANDIDATES = {
    'arm64': [
        '/opt/homebrew/bin/mpv',          # Homebrew on Apple Silicon
        '/opt/homebrew/opt/mpv/bin/mpv',  # explicit cellar
        os.path.join(PROJECT_ROOT, 'bin', 'MacOS', 'mpv'),
    ],
    'x86_64': [
        '/usr/local/bin/mpv',             # Homebrew on Intel macOS
        '/usr/local/Cellar/mpv/' + '' + '/bin/mpv',  # placeholder, ignored
        os.path.join(PROJECT_ROOT, 'bin', 'MacOS', 'mpv'),
    ],
}

BUNDLED_MPV = None
for candidate in MPV_CANDIDATES[TARGET_ARCH] + MPV_CANDIDATES[
        'arm64' if TARGET_ARCH == 'x86_64' else 'x86_64']:
    if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
        # Confirm the candidate actually runs (dyld sanity).
        try:
            rc = subprocess.run(
                [candidate, '--version'],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
            ).returncode
        except Exception:
            rc = 1
        if rc == 0:
            BUNDLED_MPV = candidate
            break

if BUNDLED_MPV is None:
    sys.stderr.write(
        'ERROR: cannot find a runnable mpv binary for arch={}.\n'
        'Install via `brew install mpv` on the build host, or drop the binary\n'
        'at bin/MacOS/mpv.\n'.format(TARGET_ARCH))
    sys.exit(1)

# i18n catalogues shipped with the app.
DATA_FILES = [
    ('i18n', [os.path.join(PROJECT_ROOT, 'i18n')]),
    ('bin/MacOS', [BUNDLED_MPV]),
]

# Version is read from the same source the pip setup.py uses.
VERSION = '0.0.0'
with open(os.path.join(PROJECT_ROOT, 'macast', '.version'), 'r') as f:
    VERSION = f.read().strip()

COPYRIGHT = 'Copyright {} xfangfang and the Macast contributors.'.format(
    datetime.datetime.now().year)

APP = [os.path.join(PROJECT_ROOT, 'Macast.py')]

OPTIONS = {
    'argv_emulation': True,
    'plist': {
        'LSUIElement': True,
        'NSHighResolutionCapable': True,
        'LSMinimumSystemVersion': '11.0',
        'CFBundleIdentifier': 'cn.xfangfang.Macast',
        'NSHumanReadableCopyright': COPYRIGHT,
        'CFBundleShortVersionString': str(VERSION),
        'CFBundleVersion': str(VERSION),
        'CFBundleName': 'Macast',
        'LSArchitecturePriority': [TARGET_ARCH],
        # macOS 14+ requires apps that send multicast on the local network
        # to declare both the usage description and the Bonjour service
        # types they browse / advertise. Without these, the OS silently
        # filters outgoing 239.255.255.250:1900 packets and DLNA clients
        # never see Macast.
        'NSLocalNetworkUsageDescription': (
            'Macast advertises itself as a DLNA media renderer on the local '
            'network so phones, TVs, and other devices can cast to it.'
        ),
        'NSBonjourServices': ['_http._tcp', '_dlna._tcp', '_smb._tcp'],
    },
    'excludes': ['PIL', 'tkinter', 'PyQt5', 'PyQt6', 'PySide2', 'PySide6',
                 'wx', 'gtk', 'gnome', 'Xlib'],
    'packages': ['rumps', 'macast', 'macast_renderer'],
    'iconfile': os.path.join(PROJECT_ROOT, 'macast', 'assets', 'icon.icns'),
    'arch': TARGET_ARCH,
    'strip': True,
    'optimize': 1,
    # The DLNA server uses cherrypy; include it explicitly so py2app's
    # modulegraph does not skip it.
    'includes': ['cherrypy', 'lxml', 'netifaces', 'appdirs', 'pyperclip',
                 'requests'],
}

setup(
    app=APP,
    data_files=DATA_FILES,
    options={'py2app': OPTIONS},
    setup_requires=['py2app'],
    py_modules=[],
)
