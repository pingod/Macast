# Copyright (c) 2021 by xfangfang. All Rights Reserved.

import os
import sys
import gettext
import logging
from macast import Setting, SETTING_DIR
from macast.macast import gui

logger = logging.getLogger("Macast")
logger.setLevel(logging.DEBUG)


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
        mpv_path = get_base_path('bin/mpv.exe')
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


def clear_env():
    # todo clear pyinstaller file on start
    log_path = os.path.join(SETTING_DIR, 'macast.log')
    try:
        os.remove(log_path)
    except:
        pass


if __name__ == '__main__':
    clear_env()
    get_lang()
    set_mpv_default_path()
    gui(lang=_)
