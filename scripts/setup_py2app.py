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
import re
import shutil
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

TARGET_ARCH = parse_arch()


# ---------------------------------------------------------------------------
# Optional bundled mpv player
# ---------------------------------------------------------------------------
# Shipping a *self-contained* mpv makes the app independent of what happens to
# be installed on the user's machine, at the price of roughly 50-80 MB. Two
# ways to provide one:
#     * drop the binary at bin/MacOS/mpv (the upstream project's convention), or
#     * point MACAST_BUNDLE_MPV at it.
#
# We deliberately do NOT fall back to Homebrew's mpv any more. That binary
# links against absolute /opt/homebrew/opt/* paths, which py2app rewrites to
# @executable_path/../Frameworks/… — and for a file living in
# Resources/bin/MacOS that resolves to Resources/bin/Frameworks, a directory
# that never exists. The bundled copy can therefore never start (dyld fails on
# the first dylib) while it still makes py2app copy every FFmpeg / libass /
# libplacebo / x265 dylib it references into Contents/Frameworks: ~53 MB of
# dead weight next to a broken 3.8 MB binary.
#
# Without a bundled binary Macast uses the mpv already on the machine — see
# Macast.set_mpv_default_path (`brew install mpv` is enough).
def is_portable_mpv(path):
    """True when `path` links only against system libraries / @rpath, i.e. it
    can be copied into a bundle and still load."""
    try:
        out = subprocess.run(['otool', '-L', path], capture_output=True,
                             text=True, timeout=15).stdout
    except Exception:
        return False
    for line in out.splitlines()[1:]:
        dep = line.strip().split(' (')[0].strip()
        if not dep:
            continue
        if dep.startswith(('@executable_path', '@loader_path', '@rpath',
                           '/System/Library/', '/usr/lib/')):
            continue
        return False
    return True


def find_bundlable_mpv():
    candidates = [
        os.environ.get('MACAST_BUNDLE_MPV'),
        os.path.join(PROJECT_ROOT, 'bin', 'MacOS', 'mpv'),
        os.path.join(PROJECT_ROOT, 'bin', 'mac', 'mpv'),
    ]
    for candidate in candidates:
        if not candidate or not os.path.isfile(candidate):
            continue
        if not os.access(candidate, os.X_OK):
            continue
        if is_portable_mpv(candidate):
            return candidate
        sys.stderr.write(
            'WARNING: not bundling {}\n'
            '         It links against absolute paths outside the system '
            '(Homebrew build?), so it cannot\n'
            '         load from inside a bundle. Macast will use the mpv '
            'installed on the target\n'
            '         machine instead (`brew install mpv`).\n'.format(candidate))
    return None


BUNDLED_MPV = find_bundlable_mpv()
if BUNDLED_MPV:
    print('==> bundling mpv: {}'.format(BUNDLED_MPV))
else:
    print('==> no portable mpv to bundle: the app will use the system mpv '
          '(brew install mpv)')

# ---------------------------------------------------------------------------
# gettext catalogues: compile .po -> .mo
# ---------------------------------------------------------------------------
# gettext can only load a *compiled* .mo, and .mo files are build output (they
# are in .gitignore). Historically this step lived in the shell wrapper and
# shelled out to `msgfmt`, which meant:
#   * the CI job (which calls this script directly) shipped an app with no
#     translations at all, and
#   * a build host without gettext could not produce a localized app.
# Compiling here, in pure Python, removes both problems and keeps every entry
# point - `make build-arm`, CI, a bare `py2app` invocation - identical.
_ESCAPE_MAP = {'n': '\n', 't': '\t', 'r': '\r', 'a': '\a', 'b': '\b',
               'f': '\f', 'v': '\v', '\\': '\\', '"': '"', "'": "'"}


def _unescape(text):
    """Turn the C escapes of a .po string literal into real characters."""
    out = []
    i, length = 0, len(text)
    while i < length:
        ch = text[i]
        if ch != '\\' or i + 1 >= length:
            out.append(ch)
            i += 1
            continue
        nxt = text[i + 1]
        if nxt in '01234567':                       # octal: \0 \12 \123
            j, digits = i + 1, ''
            while j < length and len(digits) < 3 and text[j] in '01234567':
                digits += text[j]
                j += 1
            out.append(chr(int(digits, 8) & 0xFF))
            i = j
        elif nxt in 'xX':                           # hex: \x0a
            j, digits = i + 2, ''
            while (j < length and len(digits) < 2
                   and text[j] in '0123456789abcdefABCDEF'):
                digits += text[j]
                j += 1
            if digits:
                out.append(chr(int(digits, 16)))
                i = j
            else:
                out.append(nxt)
                i += 2
        else:
            out.append(_ESCAPE_MAP.get(nxt, nxt))
            i += 2
    return ''.join(out)


def parse_po(path):
    """Parse a .po file into ({msgid: msgstr}, charset).

    Understands msgctxt and plural forms even though Macast's catalogues use
    neither, so adding them in a translation does not silently drop entries.
    """
    catalogue = {}
    charset = 'utf-8'
    entry = None
    field = None          # ('msgctxt' | 'msgid' | 'msgid_plural' | 'msgstr', idx)

    def current():
        if field is None:
            return None
        if field[0] == 'msgstr':
            return ('msgstr', field[1])
        return (field[0], None)

    def flush():
        if not entry or 'msgid' not in entry:
            return
        msgid = entry['msgid']
        msgstrs = entry.get('msgstr', [])
        plural = entry.get('msgid_plural')
        if plural is not None:
            msgid = msgid + '\x00' + plural
            value = '\x00'.join(msgstrs)
        else:
            value = msgstrs[0] if msgstrs else ''
        if entry.get('msgctxt') is not None:
            msgid = entry['msgctxt'] + '\x04' + msgid
        catalogue[msgid] = value
        if msgid == '' and value:
            match = re.search(r'charset=([\w.:-]+)', value)
            if match:
                nonlocal charset
                charset = match.group(1).lower()

    with open(path, encoding='utf-8') as handle:
        for raw in handle:
            line = raw.strip()
            if not line or line.startswith('#'):
                continue
            if line.startswith('"'):
                # Continuation of the previous field.
                if entry is not None and field is not None:
                    kind, idx = field
                    if kind == 'msgstr':
                        entry['msgstr'][idx] += _unescape(line[1:-1])
                    else:
                        entry[kind] = entry.get(kind, '') + _unescape(line[1:-1])
                continue
            keyword, _, value = line.partition(' ')
            value = value.strip()
            if value.startswith('"') and value.endswith('"'):
                value = _unescape(value[1:-1])
            if keyword == 'msgctxt':
                flush()
                entry, field = {'msgctxt': value}, ('msgctxt', None)
            elif keyword == 'msgid':
                if entry is not None and 'msgid' in entry:
                    flush()
                    entry = None
                entry = entry or {}
                entry['msgid'] = value
                field = ('msgid', None)
            elif keyword == 'msgid_plural':
                entry['msgid_plural'] = value
                field = ('msgid_plural', None)
            elif keyword.startswith('msgstr'):
                idx = 0
                if '[' in keyword:
                    idx = int(keyword[keyword.index('[') + 1:keyword.rindex(']')])
                entry.setdefault('msgstr', [])
                while len(entry['msgstr']) <= idx:
                    entry['msgstr'].append('')
                entry['msgstr'][idx] = value
                field = ('msgstr', idx)
    flush()
    return catalogue, charset


def write_mo(catalogue, charset, mo_path):
    """Write a little-endian .mo (GNU gettext MO format)."""
    import struct
    items = sorted((k.encode(charset), v.encode(charset))
                   for k, v in catalogue.items())
    if not items or items[0][0] != b'':
        # gettext expects the metadata entry as msgid "". Add a minimal one so
        # the file stays loadable by any consumer.
        header = ('Content-Type: text/plain; charset={}\n'
                  'Content-Transfer-Encoding: 8bit\n'.format(charset))
        items.insert(0, (b'', header.encode(charset)))
    count = len(items)
    key_table_at = 28
    value_table_at = 28 + 8 * count
    offset = 28 + 16 * count
    key_table, key_data = b'', b''
    for key, _ in items:
        key_table += struct.pack('<II', len(key), offset)
        key_data += key + b'\x00'
        offset += len(key) + 1
    value_table, value_data = b'', b''
    for _, value in items:
        value_table += struct.pack('<II', len(value), offset)
        value_data += value + b'\x00'
        offset += len(value) + 1
    blob = struct.pack('<7I', 0x950412DE, 0, count, key_table_at,
                       value_table_at, 0, 0)
    blob += key_table + value_table + key_data + value_data
    with open(mo_path, 'wb') as handle:
        handle.write(blob)


def verify_mo(mo_path, catalogue, charset):
    """Load the compiled catalogue back and compare it with the source."""
    import gettext as _gettext
    with open(mo_path, 'rb') as handle:
        compiled = _gettext.GNUTranslations(handle)
    for msgid, msgstr in catalogue.items():
        if '\x00' in msgid or '\x04' in msgid:
            continue    # plural/context keys: _catalog lookup, not gettext()
        if compiled.gettext(msgid) != msgstr:
            raise RuntimeError(
                'compiled {}: {!r} -> {!r} (expected {!r})'.format(
                    mo_path, msgid, compiled.gettext(msgid), msgstr))


def compile_i18n_catalogues(i18n_dir, locales):
    """Compile every .po that is newer than its .mo (or missing one)."""
    for locale in locales:
        po_path = os.path.join(i18n_dir, locale, 'LC_MESSAGES', 'macast.po')
        mo_path = po_path[:-3] + '.mo'
        if not os.path.isfile(po_path):
            continue
        if (os.path.isfile(mo_path)
                and os.path.getmtime(mo_path) >= os.path.getmtime(po_path)):
            continue
        catalogue, charset = parse_po(po_path)
        write_mo(catalogue, charset, mo_path)
        verify_mo(mo_path, catalogue, charset)
        print('==> compiled {} ({} messages, {})'.format(
            os.path.relpath(mo_path, PROJECT_ROOT), len(catalogue), charset))


# i18n catalogues shipped with the app.
#
# py2app copies each source *directory* into the destination directory, so we
# list the locale folders individually. Passing the parent `i18n/` folder
# produced a double nesting (`Resources/i18n/i18n/<locale>/...`) and every
# translation lookup failed at runtime.
I18N_DIR = os.path.join(PROJECT_ROOT, 'i18n')
I18N_LOCALES = sorted(
    name for name in os.listdir(I18N_DIR)
    if os.path.isdir(os.path.join(I18N_DIR, name))
)
compile_i18n_catalogues(I18N_DIR, I18N_LOCALES)
DATA_FILES = [
    ('i18n', [os.path.join(I18N_DIR, name) for name in I18N_LOCALES]),
]
if BUNDLED_MPV:
    DATA_FILES.append(('bin/MacOS', [BUNDLED_MPV]))

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
        # LaunchServices starts a .app with a minimal environment: with no
        # locale, Python falls back to ASCII for locale-dependent encodings and
        # CherryPy's log file handlers then blow up (UnicodeEncodeError) on any
        # non-ASCII log line, e.g. a Chinese media title. UTF-8 mode plus a
        # UTF-8 locale keeps file I/O and logs sane for every user.
        'LSEnvironment': {
            'PYTHONUTF8': '1',
            'LANG': 'en_US.UTF-8',
            'LC_CTYPE': 'en_US.UTF-8',
        },
    },
    # Keep the bundle lean. py2app's modulegraph happily follows imports that
    # only exist for building/testing, or for stdlib features this app never
    # touches, and every extra package drags its own bytecode (plus, for
    # extension modules, its dylibs) into the bundle.
    'excludes': [
        # GUI stacks are unused: the menu-bar UI comes from rumps/AppKit, and
        # the player is mpv (a separate process).
        'PIL', 'tkinter', 'PyQt5', 'PyQt6', 'PySide2', 'PySide6',
        'wx', 'gtk', 'gnome', 'Xlib',
        # Build-time only (setuptools alone is ~7 MB of shipped bytecode; the
        # app is frozen and never resolves entry points or versions).
        'setuptools', 'pkg_resources', '_distutils_hack', 'distutils',
        'pip', 'wheel', 'py2app', 'PyInstaller', 'pytest',
        # CPython test helpers nothing here imports.
        # (Do NOT exclude `decimal`/`_decimal`: cherrypy -> xmlrpc.client
        # imports `decimal` at import time.)
        '_testcapi', '_testinternalcapi', '_testmultiphase', '_xxtestfuzz',
        '_sqlite3',
        # cherrypy's wheel ships its own test-suite, tutorials and scaffold.
        'cherrypy.test', 'cherrypy.tutorial', 'cherrypy.scaffold',
    ],
    # `packages` (not `includes`) matters for zeroconf: it ships Cython-compiled
    # modules *next to* their .py sources, and `zeroconf/_services/__init__` is
    # one of the compiled ones. modulegraph therefore resolves
    # `zeroconf._services` to a single extension module, decides it is a leaf
    # and never descends into it -- the bundle then contains
    # `lib-dynload/zeroconf/_services.so` *instead of* the `_services/`
    # directory and the app dies at launch with:
    #
    #   ModuleNotFoundError: No module named 'zeroconf._services.info';
    #   'zeroconf._services' is not a package
    #
    # `packages` copies the whole directory verbatim, so the compiled
    # `__init__` and the submodules all ship together. `ifaddr` is zeroconf's
    # only runtime dependency (imported from zeroconf._utils.ipaddress).
    'packages': ['rumps', 'macast', 'macast_renderer', 'zeroconf', 'ifaddr'],
    'iconfile': os.path.join(PROJECT_ROOT, 'macast', 'assets', 'icon.icns'),
    'arch': TARGET_ARCH,
    'strip': True,
    # 2 = strip docstrings as well; smaller .pyc everywhere and nothing in the
    # app reads __doc__ at runtime.
    'optimize': 2,
    # The DLNA server uses cherrypy; include it explicitly so py2app's
    # modulegraph does not skip it.
    #
    # `cheroot.ssl.builtin` is imported *dynamically* (by name, from
    # cheroot.server.get_ssl_adapter_class) when a server is bound with
    # ssl_module='builtin', so modulegraph cannot see it. Without it the HTTPS
    # admin channel dies with `ModuleNotFoundError: No module named
    # 'cheroot.ssl'` — and because the failure happens inside the engine's
    # start listeners it takes the whole app down with it.
    # `zeroconf` must be listed here AND installed in the build environment.
    # It is imported at module level by macast/discovery.py, so leaving it out
    # produces an .app that builds fine and then dies on launch with
    # "ModuleNotFoundError: No module named 'zeroconf'".
    'includes': ['cherrypy', 'lxml', 'netifaces', 'appdirs', 'pyperclip',
                 'requests', 'cheroot.ssl.builtin'],
}

setup(
    app=APP,
    data_files=DATA_FILES,
    options={'py2app': OPTIONS},
    setup_requires=['py2app'],
    py_modules=[],
)


# ---------------------------------------------------------------------------
# Post-build trimming
# ---------------------------------------------------------------------------
# Two kinds of ballast survive the py2app build no matter what OPTIONS says:
#
#   * packages listed in `includes` are copied *as directories*, so cherrypy's
#     wheel drags its own test-suite, tutorials and project scaffold along,
#     and `excludes` cannot touch individual files inside a copied directory;
#   * the stdlib archive (python312.zip) is copied verbatim from
#     python-build-standalone and contains CPython's own test-suite (~7.9 MB)
#     plus pydoc's data files, none of which anything imports at runtime.
#
# Doing it here (rather than in the shell wrapper) means every entry point —
# `make build-arm`, a bare `python scripts/setup_py2app.py py2app`, and CI —
# produces the same lean bundle.
def prune_zip(path, prefixes=(), package_suffixes=None):
    """Drop entries of the zip at `path` that no import path can ever reach.

    `prefixes`      — top-level names to drop outright (e.g. 'test/').
    `package_suffixes` — {package_prefix: (suffix, ...)}: inside a *shipped*
        package only build-time sources may be dropped (e.g. lxml keeps its
        Cython headers for downstream projects; nothing imports a `.pxi`).

    Debug bundles (`.dSYM`) are always dropped: pyobjc's wheels ship DWARF
    symbols next to their extension modules and they are the single biggest
    item left in the archive after the test-suite.
    """
    import zipfile
    package_suffixes = package_suffixes or {}

    def dead(name):
        if '.dSYM/' in name:
            return True
        if name.startswith(prefixes):
            return True
        for package, suffixes in package_suffixes.items():
            if name.startswith(package) and name.endswith(suffixes):
                return True
        return False

    with zipfile.ZipFile(path) as zin:
        infos = zin.infolist()
        keep = [i for i in infos if not dead(i.filename)]
        if len(keep) == len(infos):
            return 0
        # Recompress without the dropped members. Using the stored
        # compress_type keeps every remaining file byte-identical.
        tmp = path + '.tmp'
        with zipfile.ZipFile(tmp, 'w', zipfile.ZIP_DEFLATED) as zout:
            for info in keep:
                zout.writestr(info, zin.read(info.filename))
    os.replace(tmp, path)
    return len(infos) - len(keep)


def trim_bundle(app_dir):
    """Remove dead weight from a freshly built bundle."""
    if not os.path.isdir(app_dir):
        return
    contents = os.path.join(app_dir, 'Contents')
    lib = os.path.join(contents, 'Resources', 'lib', 'python3.12')

    # Directories copied wholesale because their package is in `includes`.
    # cherrypy's wheel ships its own test-suite/tutorial/scaffold, and
    # python-build-standalone ships the C headers of the interpreter — nothing
    # in a frozen app compiles an extension module or runs a test.
    for rel in ('cherrypy/test', 'cherrypy/tutorial', 'cherrypy/scaffold',
                'setuptools'):
        target = os.path.join(lib, *rel.split('/'))
        if os.path.isdir(target):
            shutil.rmtree(target)
            print('==> pruned {}'.format(rel))
    for rel in ('Resources/include',):
        target = os.path.join(contents, *rel.split('/'))
        if os.path.isdir(target):
            shutil.rmtree(target)
            print('==> pruned {}'.format(rel))
    fw = os.path.join(contents, 'Frameworks', 'Python.framework', 'Versions')
    if os.path.isdir(fw):
        for version in os.listdir(fw):
            target = os.path.join(fw, version, 'include')
            if os.path.isdir(target):
                shutil.rmtree(target)
                print('==> pruned Frameworks/Python.framework/{}/include'
                      .format(version))

    # Assets that only other platforms (or the build itself) need: the .icns
    # is read by py2app at build time and already lives at
    # Resources/icon.icns, and the Windows-only icons are never looked up on
    # macOS (see Macast.ICON_MAP).
    assets = os.path.join(lib, 'macast', 'assets')
    for name in ('icon.icns', 'icon.ico', 'menu_light_large.png',
                 'menu_dark_large.png'):
        target = os.path.join(assets, name)
        if os.path.isfile(target):
            os.remove(target)
            print('==> pruned macast/assets/{}'.format(name))

    stdlib_zip = os.path.join(contents, 'Resources', 'lib', 'python312.zip')
    if os.path.isfile(stdlib_zip):
        dropped = prune_zip(
            stdlib_zip,
            prefixes=('test/', 'pydoc_data/'),
            package_suffixes={'lxml/': ('.pxi', '.pyx', '.pxd', '.h', '.c')},
        )
        if dropped:
            print('==> pruned {} entries from the stdlib archive'.format(dropped))


trim_bundle(os.path.join(PROJECT_ROOT, 'dist', 'Macast.app'))
