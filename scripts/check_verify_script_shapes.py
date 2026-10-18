#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# One shape question about `verify_rust_receiver.sh`, asked because getting it wrong
# produces a **green** line rather than a red one:
#
#   Does a `pass "…"`, `fail "…"` or `skip "…"` message contain an **unescaped**
#   backtick?
#
# Inside double quotes a backtick is command substitution. When line 2592 wrapped the
# field name `_add_history` in backticks, the script ran that command, bash answered
# `_add_history: command not found`, the label collapsed to an empty string, and the
# check **still recorded a PASS** with exit code 0. The failure mode of this family is
# not a red test — it is a test that stops asking anything while looking like an
# answer (AGENTS.md §4.14's 「验收脚本自己撒谎」 series; this is its fourth member).
#
# The first version of this rule was a grep:
#
#     grep -c 'pass "[^"]*`\|fail "[^"]*`' scripts/verify_rust_receiver.sh   # "must be 0"
#
# and it fails in **both** directions. It cannot tell an escaped `\`` — two literal
# characters in a label, which the script legitimately has — from a bare backtick, so
# it cries wolf on safe lines; and because the number it printed was measured before
# those lines were written, the "0" then got quoted forward as if it were still a
# reading. So: match the message body, unescape nothing, and require the *unescaped*
# backtick specifically. And carry a positive control, because a scanner that finds
# nothing and a scanner that is broken print the same thing.
#
# Usage:
#   python3 scripts/check_verify_script_shapes.py scripts/verify_rust_receiver.sh
#   python3 scripts/check_verify_script_shapes.py --selftest
#
# Exit code: 0 = clean; 1 = offender(s) listed; 2 = cannot read the file.

import re
import sys

MESSAGE = re.compile(r'(?:pass|fail|skip) "((?:[^"\\]|\\.)*)"')
BARE_BACKTICK = re.compile(r'(?<!\\)' + re.escape('`'))

# The line that started this, verbatim (2026-10-18, §23 of the acceptance script). The
# positive control feeds it to the scanner and requires a hit: without that, "0 found"
# would be indistinguishable from a regex that matches nothing.
OFFENDER = '        && pass "newest first, as `_add_history` writes it" \\'


def scan(lines):
    hits = []
    for number, line in enumerate(lines, 1):
        message = MESSAGE.search(line)
        if message and BARE_BACKTICK.search(message.group(1)):
            hits.append((number, line.strip()))
    return hits


def selftest():
    hits = scan([OFFENDER])
    if len(hits) != 1:
        print("SELFTEST FAILED: the scanner does not see the line that broke the "
              "script (got %d hits)" % len(hits))
        return 1
    print("selftest: the scanner reports the known-bad line -> %s" % (hits[0][1][:60],))
    # And the safe spelling must NOT be reported: `\`` is two literal characters, and a
    # rule that flags it is a rule people delete rather than satisfy.
    safe = '        && pass "server.platform is \\`sys.platform\\` as the reference spells it" \\'
    clean = scan([safe])
    if clean:
        print("SELFTEST FAILED: the scanner flags an escaped backtick (%s)" % (safe[:60],))
        return 1
    print("selftest: an escaped backtick in a label is not reported")
    return 0


def main() -> int:
    args = sys.argv[1:]
    if not args or args[0] == "--selftest":
        return selftest()
    path = args[0]
    try:
        with open(path, encoding="utf-8") as handle:
            lines = handle.read().split("\n")
    except OSError as exc:
        print("cannot read %s: %s" % (path, exc), file=sys.stderr)
        return 2
    hits = scan(lines)
    print("%s: %d unescaped backtick(s) in a pass/fail/skip label" % (path, len(hits)))
    for number, line in hits:
        print("  %d: %s" % (number, line[:100]))
    if hits:
        print("\na backtick inside double quotes is command substitution: that check "
              "runs a command that does not exist, prints an empty label, and still "
              "records a PASS.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
