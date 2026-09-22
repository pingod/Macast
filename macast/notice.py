# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Nothing in this file came from xfangfang/Macast: it is written in this fork,
# which is why it carries no "Derived from" line (docs/Provenance.md, §0).
"""The message board behind the notifications.

A macOS notification is gone in five seconds, and the user was told to install
something by one. Everything a plugin says to the user therefore goes here as
well as to the notification centre, so the settings page can still show it an
hour later -- and anything the user has to *install* is recorded as an open
requirement, with the command, until it is satisfied.

Stdlib only: plugins import this, and `macast/plugins/**` may not add a
dependency (§4.4 / Part 30).
"""

import threading
import time
from collections import deque

#: Messages kept for the activity feed. Long enough that a scrollback survives a
#: whole mirrored session, short enough that the page never has to page.
MAX_MESSAGES = 60

_lock = threading.Lock()
_recent = deque(maxlen=MAX_MESSAGES)
#: key -> {'key', 'label', 'detail', 'command'}; a requirement is only ever in
#: the table while it is *unmet*, so "satisfied" is `requirement(ok=True)`.
_requirements = {}


def record(message):
    """Say something to the user where they can read it later."""
    text = str(message or '').strip()
    if not text:
        return
    with _lock:
        _recent.append({'at': time.time(), 'text': text})


def recent(limit=20):
    """The last `limit` messages, oldest first."""
    with _lock:
        items = list(_recent)
    return [dict(item) for item in items[-max(1, int(limit)):]]


def requirement(key, label, detail='', command=''):
    """Record that something the user has to install or grant is missing.

    The console card lists *outstanding* problems, so a satisfied one is
    withdrawn by `satisfied()` rather than recorded with a green state -- a
    machine that has since grown the binary should stop being told about it.
    """
    with _lock:
        _requirements[key] = {'key': key, 'label': str(label or key),
                              'detail': str(detail or ''),
                              'command': str(command or '').strip()}


def satisfied(key):
    """Something that was missing has turned up: stop showing the card row."""
    with _lock:
        _requirements.pop(key, None)


def requirements():
    """Outstanding requirements, oldest first."""
    with _lock:
        return [dict(item) for item in _requirements.values()]
