"""
py2app build script for Macast on Apple Silicon (arm64) macOS.

Builds:    dist/Macast.app
Run from:  project root, with the build virtualenv activated

Usage:
    python scripts/setup_py2app.py py2app

The bundle layout matches what Macast.py expects at runtime:
    Macast.app/Contents/Resources/bin/MacOS/mpv   <-- mpv binary lives here
    Macast.app/Contents/Resources/i18n/...        <-- translations
    Macast.app/Contents/MacOS/Macast              <-- the py2app launcher
"""

import os
import sys
import datetime
import subprocess
from setuptools import setup


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
MPV_BREW = '/opt/homebrew/bin/mpv'
MPV_BREW_X86 = '/usr/local/bin/mpv'
MPV_LOCAL = os.path.join(PROJECT_ROOT, 'bin', 'MacOS', 'mpv')

if os.path.exists(MPV_BREW):
    BUNDLED_MPV = MPV_BREW
elif os.path.exists(MPV_BREW_X86):
    BUNDLED_MPV = MPV_BREW_X86
elif os.path.exists(MPV_LOCAL):
    BUNDLED_MPV = MPV_LOCAL
else:
    sys.stderr.write(
        'ERROR: cannot find mpv binary. Install via `brew install mpv` '
        'or drop it at bin/MacOS/mpv.\n')
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
        # arm64-only build; the build host must be Apple Silicon.
        'LSArchitecturePriority': ['arm64'],
    },
    'excludes': ['PIL', 'tkinter', 'PyQt5', 'PyQt6', 'PySide2', 'PySide6',
                 'wx', 'gtk', 'gnome', 'Xlib'],
    'packages': ['rumps', 'macast', 'macast_renderer'],
    'iconfile': os.path.join(PROJECT_ROOT, 'macast', 'assets', 'icon.icns'),
    # Force a universal-2 bundle off; we only build for arm64 in this script.
    'arch': 'arm64',
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
