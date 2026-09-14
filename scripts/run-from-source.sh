#!/bin/zsh
# Run Macast directly from this source checkout.
#
# Why a wrapper: the WorkBuddy CLI exports PYTHONPATH to a shim directory whose
# sitecustomize patches os.mkdir so that mkdir(exist_ok=True) raises EEXIST.
# That breaks pip and can break Macast at runtime, so drop it before exec.
unset PYTHONPATH
unset PYTHONDONTWRITEBYTECODE

cd /Users/pavia/githome/Macast || exit 1
exec /Users/pavia/githome/Macast/.venv/bin/python Macast.py
