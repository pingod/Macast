#!/bin/zsh
# Run Macast directly from this source checkout.
#
# Why a wrapper: the WorkBuddy CLI exports PYTHONPATH to a shim directory whose
# sitecustomize patches os.mkdir so that mkdir(exist_ok=True) raises EEXIST.
# That breaks pip and can break Macast at runtime, so drop it before exec.
unset PYTHONPATH
unset PYTHONDONTWRITEBYTECODE

# Resolve the repo root from this script's own location, so the checkout works
# wherever it lives. (This used to hardcode one developer's home directory,
# which made the script unusable for anyone else.)
REPO=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd "$REPO" || exit 1

# Prefer the checkout's own virtualenv, else fall back to python3 on PATH.
if [ -x "$REPO/.venv/bin/python" ]; then
    exec "$REPO/.venv/bin/python" Macast.py
fi
exec python3 Macast.py
