#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Can an installer actually fetch what the plugin index advertises?
#
# The settings page builds every plugin card out of `plugins/info.json`, and
# installing one is a plain HTTP GET with no credentials. That makes two
# separate facts true at once, and the suite only checks the first:
#
#   * the entry matches the bytes at the commit it pins -- proven locally, with
#     `git show <sha>:plugins/<file>` (AGENTS.md §4.6, Part 5c);
#   * some server will hand those bytes to a stranger -- **not** proven by
#     anything in the repo, because it depends on the repository's visibility,
#     which is not in the tree.
#
# `pingod/Macast` is private, so jsDelivr and raw both answer 404 and cannot
# refill: the pinned URLs that still serve are copies the CDN cached while the
# repository was readable, and they expire one by one. That is why this is a
# script to run after a release rather than a test -- it decays without anyone
# touching it, and a green suite says nothing about it.
#
# Three questions, each with a public control, because "404" alone is ambiguous:
# a blocked host and a private repository look identical from one request.
#
#   env -u PYTHONPATH .venv/bin/python scripts/check_index_reachability.py
#   ... --json                # machine-readable, for CI or cron
#   ... --url-only URL        # just ask about one address
#
# Exit status: 0 every pinned URL serves, 2 at least one is dead, 3 inconclusive
# (no usable egress -- which says nothing about the repository).

import json
import os
import re
import ssl
import sys
import urllib.error
import urllib.request

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
INDEX = os.path.join(REPO_ROOT, "plugins", "info.json")

# A public repository that must keep answering 200. If ours 404s and this one
# does not, the difference is the repository's visibility and not our network.
PUBLIC_CONTROL = 'xfangfang/Macast'

USER_AGENT = 'Macast-index-check'
TIMEOUT = 12


def repo_coordinates():
    """`user/repo` from macast/plugin_repo.py, without importing the app.

    Importing it would need netifaces and zeroconf installed; this script is
    meant to run on a build box that has neither, and certainly on a machine
    where Macast itself is not the thing being debugged.
    """
    path = os.path.join(REPO_ROOT, "macast", "plugin_repo.py")
    with open(path, encoding="utf-8") as handle:
        match = re.search(r"^REPO\s*=\s*'([^']+)'", handle.read(), re.M)
    if not match:
        raise RuntimeError("no REPO constant in {}".format(path))
    return match.group(1)


def ask(url, retries=1):
    """(status, detail). status 0 means the request never got an answer.

    Retries only the questions that got no answer at all: the CDN's edge is
    intermittently slow from here, and a TLS timeout read as a dead pin would
    send someone off to look at a commit that is fine. A 404 is never retried --
    that is an answer.
    """
    status, detail = _ask_once(url)
    for _attempt in range(retries):
        if status != 0:
            break
        status, detail = _ask_once(url)
    return status, detail


def _ask_once(url):
    request = urllib.request.Request(url, headers={'User-Agent': USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            return getattr(response, 'status', 200), ''
    except urllib.error.HTTPError as error:
        return error.code, str(error.reason)
    except urllib.error.URLError as error:
        # A TLS failure is its own answer here: an intercepting proxy that
        # cannot present a valid certificate would otherwise be reported as
        # "the repository is unreachable", which is a different diagnosis.
        if isinstance(error.reason, ssl.SSLError):
            return 0, 'TLS: {}'.format(error.reason)
        return 0, str(error.reason)
    except Exception as error:                      # pragma: no cover - safety
        return 0, '{}: {}'.format(type(error).__name__, error)


def verdict(repo, ours, control, cdn):
    """What the three reachability answers mean, as one short string.

    Only `INDEX_PRIVATE` when the evidence is actually that: our repository 404s
    *and* the public control does not. Anything else on a network that answers
    nothing is inconclusive, and saying so is the whole point -- the first read
    of these 404s was "the sandbox has no egress", which was wrong in one
    direction and right in the other only by luck.
    """
    if ours == 200 and cdn == 200:
        return 'INDEX_OK'
    if ours == 404 and control == 200:
        return 'INDEX_PRIVATE'
    if ours == 0 and control == 0:
        return 'INCONCLUSIVE'
    return 'AMBIGUOUS'


def main(argv):
    as_json = '--json' in argv
    if '--url-only' in argv:
        try:
            target = argv[argv.index('--url-only') + 1]
        except IndexError:
            print('give a URL after --url-only', file=sys.stderr)
            return 3
        code, detail = ask(target)
        print(json.dumps({'url': target, 'status': code, 'detail': detail})
              if as_json else '{}  {}'.format(code or 'NO ANSWER', detail))
        return 0 if code == 200 else 2

    repo = repo_coordinates()
    ours, _ = ask('https://api.github.com/repos/{}'.format(repo))
    control, _ = ask('https://api.github.com/repos/{}'.format(PUBLIC_CONTROL))
    cdn, _ = ask('https://data.jsdelivr.com/v1/packages/gh/{}'.format(repo))

    with open(INDEX, encoding='utf-8') as handle:
        entries = json.load(handle).get('plugin_v1') or []
    rows = []
    for entry in entries:
        status, detail = ask(entry['url'])
        rows.append({'title': entry.get('title', '?'),
                     'version': entry.get('version', '?'),
                     'status': status, 'detail': detail, 'url': entry['url']})
    dead = [row for row in rows if row['status'] != 200]
    result = verdict(repo, ours, control, cdn)
    if result == 'INDEX_OK':
        code = 2 if dead else 0
    elif result == 'INDEX_PRIVATE':
        # Broken for installers even while cached copies still answer 200, so
        # the cache is not allowed to report success.
        code = 2
    else:
        # Nothing answered usefully, which says nothing about the repository.
        code = 3

    if as_json:
        print(json.dumps({'verdict': result, 'repo': repo,
                          'github_anonymous': ours, 'control': control,
                          'jsdelivr_metadata': cdn, 'entries': rows,
                          'exit_status': code}, ensure_ascii=False, indent=2))
        return code

    print('repo                : {}'.format(repo))
    print('anonymous GitHub API: {} (public control {})'.format(ours, control))
    print('jsDelivr metadata   : {}'.format(cdn))
    print('verdict             : {}'.format(result))
    print('pinned install URLs : {} of {} serve'.format(
        len(rows) - len(dead), len(rows)))
    for row in rows:
        print('  {:<6} {:<38} {:<6} {}'.format(
            row['status'] or 'n/a', row['title'], row['version'], row['detail']))

    if result == 'INDEX_PRIVATE':
        print("\n{} is not visible to anonymous callers, so jsDelivr and raw "
              "cannot serve it.\nThe {} URLs above that answer 200 are cached "
              "copies from when the\nrepository was readable, and they expire "
              "individually -- nothing in the repo\nkeeps them alive, and the "
              "suite cannot see any of this (it checks that an\nentry matches "
              "the commit it pins, which is local and still true).".format(
                  repo, len(rows) - len(dead)))
        print("Options: make the repository public; publish plugins/ from a "
              "public repo and\npoint macast/plugin_repo.py::REPO at it; or "
              "document that installation is\nmanual ('从网址安装' with a "
              "reachable URL, or drop the .py into\n~/Library/Application "
              "Support/Macast/renderer/).")
    elif result == 'INCONCLUSIVE':
        print("\nNeither this repository nor the public control answered, so "
              "nothing here says\nanything about visibility. Check egress and "
              "any intercepting proxy first.")
    elif dead:
        print("\nThe repository looks public, so these {} dead URLs are worth "
              "an individual\nlook: a wrong SHA, a file that never existed at "
              "that commit, or a CDN\nthat has not been warmed yet right after "
              "the push.".format(len(dead)))
    return code


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
