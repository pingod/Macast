# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Per-module log files, kept out of macast.log.

A screen mirror streaming at 30 fps logs far more than the core protocol
traffic; mixed into one file it buries exactly the lines a user needs when
something on the LAN goes wrong. So a plugin's logger can be *claimed*: its
records then go only to ``<config>/logs/<Name>.log`` (rotating, small,
independently viewable from the settings page), and -- because claiming turns
propagation off -- never reach the root handler, i.e. never into macast.log.

Claiming is by logger name and idempotent. ``MacastPluginManager`` claims
whatever ``logger`` a plugin module defines at import; nothing a plugin has to
remember to do except the usual ``logger = logging.getLogger("X")``.

The directory is resolved at call time through ``utils.SETTING_DIR`` (not
imported once), so the regression suite can point it at a temp directory.
"""
import glob
import logging
import logging.handlers
import os
import re

from . import utils

LOG_DIR_NAME = 'logs'
#: Module logs are per-plugin transcripts, not the system ledger: one
#: megabyte plus one backup is plenty to debug "what did it do a minute ago",
#: and it keeps a chatty plugin from eating the disk.
MODULE_LOG_MAX_BYTES = 1024 * 1024
MODULE_LOG_BACKUP_COUNT = 1
MODULE_LOG_MAX = 2 * 1024 * 1024

#: Same shape as the root handler's format in Macast.py, so a line pasted
#: from either file reads the same.
_FORMAT = '[%(asctime)s] %(name)s %(levelname)s: %(message)s'

_claimed = {}

_NAME_SAFE = re.compile(r'[^A-Za-z0-9_.-]')


def log_dir():
    return os.path.join(utils.SETTING_DIR, LOG_DIR_NAME)


def file_name(name):
    """The on-disk name for a logger. Sanitised because the name becomes a
    path component and logger names are chosen by third-party plugin files."""
    cleaned = _NAME_SAFE.sub('_', str(name)).strip('._')
    return (cleaned or 'logger') + '.log'


def path_for(name):
    return os.path.join(log_dir(), file_name(name))


def claim(name):
    """Route logger `name` to its own file. Returns the path, or '' on failure.

    Never raises: logging must not be able to stop a plugin from loading.
    """
    try:
        logger = logging.getLogger(name)
        target = path_for(name)
        for h in logger.handlers:
            if isinstance(h, logging.handlers.RotatingFileHandler) and \
                    getattr(h, '_macast_module_log', False):
                if os.path.abspath(h.baseFilename) == os.path.abspath(target):
                    _claimed[name] = target
                    return target
                h.close()
                logger.removeHandler(h)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        handler = logging.handlers.RotatingFileHandler(
            target, maxBytes=MODULE_LOG_MAX_BYTES,
            backupCount=MODULE_LOG_BACKUP_COUNT, encoding='utf-8')
        handler.setFormatter(logging.Formatter(_FORMAT))
        handler._macast_module_log = True
        logger.addHandler(handler)
        # The whole point: records stop at this logger, so the root handler
        # (macast.log) never sees them.
        logger.propagate = False
        logger.setLevel(logging.INFO)
        _claimed[name] = target
        return target
    except Exception:
        return ''


def claim_from_module(module):
    """Claim the ``logger`` a plugin module defines, if it has one."""
    logger = getattr(module, 'logger', None)
    if isinstance(logger, logging.Logger) and logger.name:
        return claim(logger.name)
    return ''


def claimed_names():
    return sorted(_claimed)


def list_logs():
    """Every module log on disk (claimed or left by an earlier run), as
    ``{'name', 'file', 'size', 'mtime'}`` sorted by name."""
    out = []
    directory = log_dir()
    try:
        entries = sorted(os.listdir(directory))
    except OSError:
        entries = []
    for fname in entries:
        if re.search(r'\.log(\.\d+)?$', fname):
            path = os.path.join(directory, fname)
            try:
                st = os.stat(path)
            except OSError:
                continue
            name = re.sub(r'\.log(\.\d+)?$', '', fname)
            out.append({'name': name, 'file': fname,
                        'size': st.st_size, 'mtime': int(st.st_mtime)})
    return out


def clear(name):
    """Truncate one module log and drop its rotated backups."""
    path = path_for(name)
    removed = 0
    try:
        with open(path, 'w'):
            pass
        removed += 1
    except OSError:
        pass
    for extra in sorted(glob.glob(path + '.*')):
        try:
            os.remove(extra)
            removed += 1
        except OSError:
            pass
    return removed


def remove_all():
    """Startup wipe: module logs are cleared with macast.log (AGENTS §4.10 --
    deleting only the main file would let the per-plugin ones accumulate
    forever)."""
    removed = 0
    directory = log_dir()
    try:
        entries = os.listdir(directory)
    except OSError:
        return 0
    for fname in entries:
        if '.log' not in fname:
            continue
        try:
            os.remove(os.path.join(directory, fname))
            removed += 1
        except OSError:
            pass
    return removed
