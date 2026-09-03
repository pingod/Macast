"""
Backwards-compatible shim that forwards to the real py2app build script.

The canonical build entry point is `scripts/setup_py2app.py`. This file is
kept at the project root for any external CI that still runs `python setup.py
py2app` from the repo root. It simply re-execs the new script.

Real build instructions live in BUILDING.md.
"""

import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = os.path.join(HERE, 'scripts', 'setup_py2app.py')

if not os.path.exists(TARGET):
    sys.stderr.write(
        'ERROR: setup_py2app.py shim cannot find scripts/setup_py2app.py\n')
    sys.exit(1)

runpy.run_path(TARGET, run_name='__main__')
