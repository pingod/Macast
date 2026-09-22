# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.

import os
import sys
import glob
import shutil
import gettext
import locale
import logging
import logging.handlers
from macast import Setting, SETTING_DIR
from macast.utils import LOG_FILE_NAME
from macast.macast import gui

logger = logging.getLogger("Macast")
logger.setLevel(logging.DEBUG)
_ = gettext.gettext

#: One run used to be able to write ~40 MB/day (measured on a busy instance:
#: 27 KB/min with a video playing and the settings page open — every HTTP
#: request, every SOAP body and every mpv property change goes in there), and
#: the settings page read the whole file back into the DOM. Keep one file plus
#: two rotations instead; the file is deleted once per start by `clear_env()`.
LOG_MAX_BYTES = 2 * 1024 * 1024
LOG_BACKUP_COUNT = 2


def get_base_path(path="."):
    if getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
        base_path = sys._MEIPASS
    else:
        base_path = os.getcwd()
    return os.path.join(base_path, path)


def set_mpv_default_path():
    """Pick the mpv binary Macast will spawn.

    Resolution order on macOS:
      1. mpv shipped inside the app bundle at Contents/Resources/bin/MacOS/mpv.
         We *probe* the binary with `mpv --version` (dry-run) before accepting
         it, because Homebrew's mpv depends on ~14 dylibs in
         /opt/homebrew/opt/* that py2app does not copy into the bundle; a
         present-and-executable bundled mpv can still fail to launch with
         `dyld: Library not loaded` if those dylibs are missing.
      2. Homebrew arm64 mpv at /opt/homebrew/bin/mpv, or the legacy Intel
         install at /usr/local/bin/mpv.
      3. Whatever $PATH resolves to (`mpv`).
    """
    mpv_path = 'mpv'
    if sys.platform == 'darwin':
        candidates = [get_base_path('bin/MacOS/mpv'),
                      '/opt/homebrew/bin/mpv',
                      '/usr/local/bin/mpv']
        for candidate in candidates:
            if not (os.path.isfile(candidate) and os.access(candidate, os.X_OK)):
                continue
            if _mpv_is_runnable(candidate):
                mpv_path = candidate
                break
            else:
                logger.warning(
                    "Found mpv at %s but it failed to launch "
                    "(missing dylibs?), trying next candidate", candidate)
    elif sys.platform == 'win32':
        # Bundled mpv first, then a copy next to the exe, then $PATH.
        exe_dir = os.path.dirname(sys.executable)
        candidates = [
            get_base_path('bin/mpv.exe'),
            os.path.join(exe_dir, 'bin', 'mpv.exe'),
            os.path.join(exe_dir, 'mpv.exe'),
        ]
        found = None
        for cand in candidates:
            if os.path.isfile(cand):
                found = cand
                break
        if not found:
            found = shutil.which('mpv')
        mpv_path = found if found else 'mpv'
    Setting.mpv_default_path = mpv_path
    logger.info("Using mpv at: %s", mpv_path)
    return mpv_path


def _mpv_is_runnable(path):
    """Return True if the mpv binary at `path` can actually launch on this
    machine. `mpv --version` is a fast, headless invocation that prints
    version info to stdout and exits 0; if any of mpv's dylibs are missing,
    dyld prints the error to stderr and the process exits non-zero before
    main() runs.
    """
    import subprocess
    try:
        result = subprocess.run(
            [path, '--version'],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=5,
        )
    except Exception as e:
        logger.debug("mpv probe failed for %s: %s", path, e)
        return False
    return result.returncode == 0


def get_lang():
    locale = Setting.get_locale()
    i18n_path = get_base_path('i18n')
    if not os.path.exists(os.path.join(i18n_path, locale, 'LC_MESSAGES', 'macast.mo')):
        locale = locale.split("_")[0]
    logger.error("Macast Loading Language: {}".format(locale))
    try:
        lang = gettext.translation('macast', localedir=i18n_path, languages=[locale])
        lang.install()
    except Exception:
        import builtins
        builtins.__dict__['_'] = gettext.gettext
        logger.error("Macast Loading Default Language en_US")


def remove_log_files(directory):
    """Delete the log and its rotated backups (`macast.log`, `.1`, `.2`, ...).

    Split out of `clear_env()` so the regression suite can prove the rotated
    backups are cleaned up too -- leaving them behind would defeat the rotation
    the moment the file is wiped at the next start.
    """
    for path in sorted(glob.glob(os.path.join(directory, LOG_FILE_NAME + '*'))):
        try:
            os.remove(path)
        except OSError:
            pass


def clear_env():
    # todo clear pyinstaller file on start
    remove_log_files(SETTING_DIR)
    # Per-module logs (logs/<Name>.log, see macast/logsplit.py) are wiped with
    # the main file: clearing only macast.log would let the plugin files
    # accumulate forever.
    try:
        from macast import logsplit
        logsplit.remove_all()
    except Exception:
        pass


def _force_utf8_ctype():
    """Make text I/O UTF-8 even when the app runs without a locale.

    LaunchServices starts a .app with a nearly empty environment (and the
    LSEnvironment plist key is not always honoured), so Python falls back to
    US-ASCII as its preferred encoding — and then any non-ASCII log line
    (a Chinese media title, for instance) raises UnicodeEncodeError inside
    CherryPy's own log handlers. Setting only LC_CTYPE fixes the encoding
    without touching LC_TIME / LC_NUMERIC / LC_MESSAGES.
    """
    if locale.getpreferredencoding(False).lower().replace('-', '') == 'utf8':
        return
    for name in ('UTF-8', 'en_US.UTF-8', 'C.UTF-8'):
        try:
            locale.setlocale(locale.LC_CTYPE, name)
            return
        except locale.Error:
            continue


def setup_logging():
    """Mirror Python-side log records into macast.log.

    In a bundle (.app / PyInstaller) the process has no console, so without a
    handler of our own every logger.error()/warning() from macast.* is thrown
    away. Silent failures are the hardest kind to debug: a HTTPS channel that
    never came up looked exactly like "nothing happened at all".

    The handler rotates (see LOG_MAX_BYTES): this is the *only* writer of
    macast.log now. CherryPy used to own a second, non-rotating FileHandler on
    the same path (see macast/server.py), which both duplicated every access
    line and let the file grow without a ceiling.

    Logging must never be able to prevent the app from starting, hence the
    guarded setup (SETTING_DIR is otherwise created later by Setting.init).
    """
    _force_utf8_ctype()
    try:
        os.makedirs(SETTING_DIR, exist_ok=True)
        handler = logging.handlers.RotatingFileHandler(
            os.path.join(SETTING_DIR, LOG_FILE_NAME),
            maxBytes=LOG_MAX_BYTES, backupCount=LOG_BACKUP_COUNT,
            encoding='utf-8')
    except Exception:
        return
    handler.setFormatter(logging.Formatter(
        '[%(asctime)s] %(name)s %(levelname)s: %(message)s'))
    root = logging.getLogger()
    root.addHandler(handler)
    root.setLevel(logging.INFO)
    # Handy when diagnosing a bundled app: tells us where the log lives and
    # whether the process ended up in UTF-8 mode (a bundle without a locale
    # falls back to ASCII, and then any log line containing non-ASCII — a
    # Chinese media title, for instance — raises UnicodeEncodeError).
    root.info("Python %s, preferred encoding %s, logging to %s",
              sys.version.split()[0], locale.getpreferredencoding(False),
              handler.baseFilename)




if __name__ == '__main__':
    clear_env()
    setup_logging()
    get_lang()
    set_mpv_default_path()
    gui(lang=_)
