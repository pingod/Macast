#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Who actually wrote each Python file in this repository, line by line?
#
# This fork carries upstream's code and upstream's copyright notices, and the
# two facts constrain each other in a direction that is easy to get wrong: a
# notice may not be deleted while the lines it names are still in the file, and
# a notice may not be *added* to a file whose lines we never wrote. Either
# mistake is a misrepresentation, and this project is being commercialised, so
# the claim is one that will be made to paying customers.
#
# "Still in the file" is not a feeling -- it is a `git blame` fact, with two
# gaps this tool closes explicitly:
#
#   1. a file copied in from *another* repository (the bundled plugins came from
#      xfangfang/Macast-plugins) has no ancestor commit in this history, so
#      blame credits the commit that pasted it. Hence `VENDORED`.
#   2. a file renamed or moved loses blame the same way. Hence the blob test: a
#      file whose bytes match something upstream committed is upstream's,
#      wherever it now lives.
#
# Those give the four states the notices have to track:
#
#   upstream   nobody in this fork wrote a line of it  -> upstream attribution only
#   mixed      both sides still in it                  -> both attributions, neither may go
#   ours       every line is ours                      -> our notice, and the end
#                                                        state of the rewrite queue
#   vendored   upstream's, from a repo outside history  -> upstream attribution, and
#                                                        our notice must NOT appear
#
# `FORK_POINT` marks the boundary; its parent chain is upstream's history.
#
# What a row is a picture *of*: the file list is the index, line counts of a
# file this fork created (or never touched, or vendored in) are counted off the
# disk, and line counts of a file both sides still share are counted off HEAD by
# `git blame`. So commit first and re-measure second: numbers read beside an
# uncommitted edit describe the *previous* commit for exactly those mixed files,
# which is how a ledger that matched the suite an hour ago fails a clean tree.
#
# Output shapes: a table for humans (default), `--json` for the regression suite
# (Part 34, so docs/Provenance.md cannot drift away from the history it
# describes), `--check` to exit non-zero on notice drift, and `--stamp --apply`
# to write in the notices the line counts license. The last one only ever
# *adds* attribution.
#
# Usage:
#   env -u PYTHONPATH .venv/bin/python scripts/provenance.py
#   env -u PYTHONPATH .venv/bin/python scripts/provenance.py --json
#   env -u PYTHONPATH .venv/bin/python scripts/provenance.py --check
#   env -u PYTHONPATH .venv/bin/python scripts/provenance.py --stamp --apply

import json
import os
import subprocess
import sys

# First commit authored inside this fork. Everything reachable from its parent
# is upstream's history; a line blamed to that set is upstream's line.
FORK_POINT = "19879235ef98a64b813de968306bc91a0d663518"

# Paths whose content was copied in from `xfangfang/Macast-plugins` (AGENTS.md
# §4.4). Blame cannot see that repo, so this list -- not history -- is what
# stops the tool from claiming these lines for us.
VENDORED = (
    "macast/plugins/protocol/nirvana.py",
    "macast/plugins/renderer/iina.py",
    "macast/plugins/renderer/live.py",
    "macast/plugins/renderer/pi_fm.py",
    "macast/plugins/renderer/potplayer.py",
    "macast/plugins/renderer/web.py",
)

# Matched on the author name rather than a whole header line, so a reworded
# notice (year, "All Rights Reserved") still counts.
UPSTREAM_MARK = "by xfangfang"
OURS_MARK = "by pingod"

# Upstream's own files carry a notice only where upstream chose to put one.
# These hold upstream lines and say nothing, which is a gap in the *other*
# direction: the accurate fix is to add attribution, never to invent a claim.
DERIVATIVE_MARK = "Derived from xfangfang/Macast"
DERIVATIVE_NOTICE = "# Derived from xfangfang/Macast (GPLv3). See docs/Provenance.md."

# Same for the vendored plugins, which came from the *collection* repo rather
# than from this one -- so their attribution has to name the right source.
VENDORED_MARK = "Copied from xfangfang/Macast-plugins"
VENDORED_NOTICE = "# Copied from xfangfang/Macast-plugins (GPLv3). See docs/Provenance.md."

OUR_NOTICE = "# Copyright (c) 2026 by pingod. All Rights Reserved."

# A third layer, below both of the ones history can see: upstream's own files
# include code inherited from yet other authors, under a permissive licence that
# carries its own notice-preservation duty. `macast/ssdp.py` is the Coherence
# lineage (Tim Potter, John-Mark Gurney, Fluendo, Frank Scholz, Erwan Martin)
# and says "Licensed under the MIT license" -- so rewriting that file may not
# drop the licence statement either, and this checks it rather than trusting it.
THIRD_PARTY = (
    ("macast/ssdp.py", "MIT license"),
)

# How far into a file we look for a notice: upstream's headers sit on line 1,
# scripts here open with a shebang, and a plugin opens with its metadata block.
HEADER_WINDOW = 40

# A file with fewer lines than this cannot prove anything by matching a blob --
# every empty file in the world shares one SHA.
MIN_BLOB_LINES = 5

STATE_UPSTREAM = "upstream"
STATE_OURS = "ours"
STATE_MIXED = "mixed"
STATE_VENDORED = "vendored"


class GitError(Exception):
    """History is unreadable here -- usually a shallow checkout."""


REPO_ROOT = subprocess.run(
    ("git", "rev-parse", "--show-toplevel"),
    stdout=subprocess.PIPE, text=True).stdout.strip()


def git(*args, check=True):
    """`git` in the repo root, stdout decoded."""
    result = subprocess.run(
        ("git",) + args, cwd=REPO_ROOT,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if check and result.returncode != 0:
        raise GitError(result.stderr.strip() or 'git %s failed' % ' '.join(args))
    return result.stdout


def git_ok(*args):
    """True when `git` exits zero."""
    return subprocess.run(("git",) + args, cwd=REPO_ROOT,
                          stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode == 0


_UPSTREAM = []


def upstream_commits():
    """Every SHA authored before this fork existed.

    The ancestor check runs first so a `FORK_POINT` that is not in this history
    at all fails loudly here, instead of silently classifying the entire
    repository as ours.
    """
    if not _UPSTREAM:
        git("merge-base", "--is-ancestor", FORK_POINT, "HEAD")
        _UPSTREAM.append(set(git("rev-list", FORK_POINT + "^").split()))
    return _UPSTREAM[0]


def upstream_blobs():
    """Every blob SHA upstream ever committed.

    `rev-list --objects` walks the trees, so this is one pass over history
    rather than one per commit. It catches the case blame gets wrong: upstream
    code that was moved or renamed inside this fork.
    """
    if not _BLOBS:
        found = set()
        for line in git("rev-list", "--objects", FORK_POINT + "^").splitlines():
            fields = line.split(" ", 1)
            if len(fields) == 2 and not fields[1].endswith("/"):
                found.add(fields[0])
        _BLOBS.append(found)
    return _BLOBS[0]


_BLOBS = []


def is_upstream_bytes(path):
    """Byte-identical to something upstream committed?"""
    if counted_lines(path) < MIN_BLOB_LINES:
        return False
    digest = git("hash-object", "--", path).strip()
    return digest in upstream_blobs()


def blame_lines(path, before):
    """(upstream_lines, our_lines) for the file **as it stands in HEAD**.

    Naming a revision makes blame read the committed blob, so an uncommitted
    edit to a file that already existed at the fork point is invisible here.
    The other branches of `classify()` count lines off the **disk** instead --
    see the note on `ledger()`, and run `--check` against a committed tree.
    """
    counts = [0, 0]
    for line in git("blame", "--line-porcelain", "HEAD", "--", path).splitlines():
        head = line.split(" ", 1)[0]
        if len(head) == 40 and all(c in "0123456789abcdef" for c in head):
            counts[0 if head in before else 1] += 1
    return counts


def counted_lines(path):
    """Total lines, read once per file."""
    if path not in _LINE_CACHE:
        with open(os.path.join(REPO_ROOT, path), "rb") as handle:
            _LINE_CACHE[path] = sum(1 for _ in handle)
    return _LINE_CACHE[path]


_LINE_CACHE = {}


def header_text(path):
    if path not in _HEADER_CACHE:
        with open(os.path.join(REPO_ROOT, path), "r", encoding="utf-8",
                  errors="replace") as handle:
            lines = [handle.readline() for _ in range(HEADER_WINDOW)]
        _HEADER_CACHE[path] = "".join(lines)
    return _HEADER_CACHE[path]


_HEADER_CACHE = {}


def carries(path, mark):
    return mark in header_text(path)


def classify(path, before):
    """One row of the ledger: line counts, and what the notices currently say."""
    if path.startswith(VENDORED):
        upstream_lines, our_lines, state = counted_lines(path), 0, STATE_VENDORED
    elif not git_ok("cat-file", "-e", "%s:%s" % (FORK_POINT, path)):
        # New in this fork -- unless its bytes are upstream's under another name.
        if is_upstream_bytes(path):
            upstream_lines, our_lines, state = counted_lines(path), 0, STATE_UPSTREAM
        else:
            upstream_lines, our_lines, state = 0, counted_lines(path), STATE_OURS
    elif git_ok("diff", "--quiet", FORK_POINT, "HEAD", "--", path):
        # Untouched since the fork point: every line is upstream's, no blame needed.
        upstream_lines, our_lines, state = counted_lines(path), 0, STATE_UPSTREAM
    else:
        upstream_lines, our_lines = blame_lines(path, before)
        state = STATE_MIXED if upstream_lines else STATE_OURS

    return {
        "path": path,
        "state": state,
        "upstream_lines": upstream_lines,
        "our_lines": our_lines,
        "upstream_attributed": (carries(path, UPSTREAM_MARK)
                                or carries(path, DERIVATIVE_MARK)
                                or carries(path, VENDORED_MARK)),
        "our_attributed": carries(path, OURS_MARK),
    }


def ledger():
    """The whole repository, one row per tracked .py file.

    Two different moments feed one row, and neither is "the tree as it stands":
    the file list is the index (`git ls-files`), `vendored`/`ours`/untouched
    `upstream` rows are counted off the disk, and `mixed` rows off HEAD. So the
    numbers move when you commit -- which is why `docs/Provenance.md` is
    re-measured *after* the commit that changes code, not before it.
    """
    tracked = git("ls-files", "*.py").split()
    paths = [p for p in tracked if os.path.exists(os.path.join(REPO_ROOT, p))]
    before = upstream_commits()
    return [classify(path, before) for path in sorted(paths)]


def problems(rows):
    """Notice/line mismatches -- each one is a legal or honesty defect.

    Two directions on purpose: a missing notice understates someone's authorship,
    and a notice that was never earned overstates ours. Files with no lines in
    them either way are exempt -- an empty `__init__.py` expresses no
    authorship, so there is nothing to attribute and nothing to claim.
    """
    out = []
    for row in rows:
        path, state = row["path"], row["state"]
        if not row["upstream_lines"] and not row["our_lines"]:
            continue
        if row["upstream_lines"] and not row["upstream_attributed"]:
            out.append("%s: holds %d upstream line(s) and attributes them to "
                       "nobody" % (path, row["upstream_lines"]))
        if state in (STATE_OURS, STATE_MIXED) and not row["our_attributed"]:
            out.append("%s: %d line(s) were written in this fork but carry no "
                       "notice at all" % (path, row["our_lines"]))
        if state in (STATE_UPSTREAM, STATE_VENDORED) and row["our_attributed"]:
            out.append("%s: claims our copyright on a file this fork never wrote "
                       "a line of" % path)
    for path, mark in THIRD_PARTY:
        if os.path.exists(os.path.join(REPO_ROOT, path)) and not carries(path, mark):
            out.append("%s: inherits code under %s -- that notice is neither "
                       "ours nor upstream's to remove" % (path, mark))
    return out


def notice_index(lines):
    """Where a further copyright line may go without breaking anything.

    Plugins are the reason this is not simply "line 0": `_read_plugin_metadata`
    parses only the *contiguous* comment block at the top of the file (AGENTS.md
    §4.5), so an inserted line has to stay inside that block, and a file opening
    with a docstring must keep the docstring as its first statement. An existing
    notice stays above the new one, so the headers read as one block.
    """
    index = 0
    if lines and lines[0].startswith("#!"):
        index = 1
    while index < len(lines) and "coding" in lines[index][:12]:
        index += 1
    while index < len(lines) and "Copyright" in lines[index]:
        index += 1
    return index


def missing_notices(row):
    """The attribution lines this file's own line counts license. Adds only."""
    wanted = []
    if not row["upstream_lines"] and not row["our_lines"]:
        return wanted
    if row["upstream_lines"] and not row["upstream_attributed"]:
        wanted.append(VENDORED_NOTICE if row["state"] == STATE_VENDORED
                      else DERIVATIVE_NOTICE)
    if row["state"] in (STATE_OURS, STATE_MIXED) and not row["our_attributed"]:
        wanted.append(OUR_NOTICE)
    return wanted


def stamp(rows):
    """Write in the notices the line counts license. Nothing is ever removed."""
    touched = []
    for row in rows:
        additions = missing_notices(row)
        if not additions:
            continue
        touched.append((row["path"], additions))
        if "--apply" not in sys.argv:
            continue
        path = os.path.join(REPO_ROOT, row["path"])
        with open(path, "r", encoding="utf-8") as handle:
            lines = handle.read().splitlines(True)
        for offset, text in enumerate(additions):
            lines.insert(notice_index(lines) + offset, text + "\n")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("".join(lines))
    return touched


def report(rows):
    order = (STATE_UPSTREAM, STATE_VENDORED, STATE_MIXED, STATE_OURS)
    by_state = {state: [r for r in rows if r["state"] == state] for state in order}

    width = max(len(r["path"]) for r in rows)
    print("fork point %s -- upstream history: %d commit(s)\n"
          % (FORK_POINT[:9], len(upstream_commits())))
    header = "%-*s %-9s %7s %7s  %s" % (
        width, "file", "state", "upstr", "ours", "attribution")
    print(header)
    print("-" * len(header))
    for state in order:
        for row in by_state[state]:
            marks = ("upstream" if row["upstream_attributed"] else "none") + " / " + \
                    ("ours" if row["our_attributed"] else "none")
            print("%-*s %-9s %7d %7d  %s"
                  % (width, row["path"], row["state"], row["upstream_lines"],
                     row["our_lines"], marks))
        if by_state[state]:
            print("")

    def total(state, key):
        return sum(r[key] for r in by_state[state])

    upstream_lines = total(STATE_UPSTREAM, "upstream_lines") + \
        total(STATE_VENDORED, "upstream_lines") + total(STATE_MIXED, "upstream_lines")
    our_lines = total(STATE_MIXED, "our_lines") + total(STATE_OURS, "our_lines")
    all_lines = upstream_lines + our_lines
    print("%d file(s): %d upstream, %d vendored, %d mixed, %d ours"
          % (len(rows), len(by_state[STATE_UPSTREAM]), len(by_state[STATE_VENDORED]),
             len(by_state[STATE_MIXED]), len(by_state[STATE_OURS])))
    print("%d upstream line(s) + %d written in this fork = %d total (%.1f%% ours)"
          % (upstream_lines, our_lines, all_lines, 100.0 * our_lines / max(all_lines, 1)))
    return by_state


def main():
    try:
        rows = ledger()
    except GitError as error:
        print("cannot read history: %s" % error, file=sys.stderr)
        print("hint: this needs a full clone, not a shallow one "
              "(git fetch --unshallow)", file=sys.stderr)
        return 3

    bad = problems(rows)
    if "--json" in sys.argv:
        print(json.dumps({"fork_point": FORK_POINT,
                          "upstream_commits": len(upstream_commits()),
                          "vendored": list(VENDORED),
                          "files": rows,
                          "problems": bad}, indent=2))
        return 1 if (bad and "--check" in sys.argv) else 0

    report(rows)
    if bad:
        print("notice drift:")
        for line in bad:
            print("  ! %s" % line)
    if "--stamp" in sys.argv:
        targets = stamp(rows)
        print("\n%s %d file(s); nothing is ever removed:" % (
            "stamped" if "--apply" in sys.argv else "would stamp", len(targets)))
        for path, additions in targets:
            print("  %s %s (%s)" % ("+" if "--apply" in sys.argv else "?", path,
                                    ", ".join(a.strip("# ") for a in additions)))
    return 1 if (bad and "--check" in sys.argv) else 0


if __name__ == "__main__":
    sys.exit(main())
