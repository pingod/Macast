#!/usr/bin/env bash
#
# Build Macast.app for Apple Silicon (arm64) macOS.
#
# This script does the full pipeline:
#   1. Verify the host is arm64 macOS.
#   2. Warn if no mpv is available on this machine (the .app plays media with
#      the system mpv; set MACAST_BUNDLE_MPV to ship a portable build instead).
#   3. Create an isolated Python 3.12 virtualenv with py2app and all deps.
#   4. Run py2app to produce dist/Macast.app.
#   5. Verify the resulting bundle is arm64-only and runnable.
#
# All build artifacts stay inside the project directory:
#   .venv-build/        isolated virtualenv
#   dist/               py2app output (Macast.app)
#   build/              py2app scratch space
#
# Usage:    bash scripts/build_macos_arm.sh
# Clean:    bash scripts/build_macos_arm.sh clean
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VENV_DIR="${PROJECT_ROOT}/.venv-build"
DIST_DIR="${PROJECT_ROOT}/dist"
BUILD_DIR="${PROJECT_ROOT}/build"
PY2APP_SETUP="${SCRIPT_DIR}/setup_py2app.py"

# --- step 0: handle clean --------------------------------------------------
if [[ "${1:-}" == "clean" ]]; then
    echo "==> cleaning build outputs"
    rm -rf "${DIST_DIR}" "${BUILD_DIR}"
    echo "==> done"
    exit 0
fi

cd "${PROJECT_ROOT}"

# --- step 1: host must be arm64 macOS --------------------------------------
if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "ERROR: this script only runs on macOS." >&2
    exit 1
fi

ARCH="$(uname -m)"
if [[ "${ARCH}" != "arm64" ]]; then
    echo "WARNING: host architecture is ${ARCH}, expected arm64." >&2
    echo "The resulting .app will still be tagged arm64, but binaries" >&2
    echo "(mpv, libpython) will be ${ARCH}." >&2
    read -r -p "Continue anyway? [y/N] " ans
    [[ "${ans}" == "y" || "${ans}" == "Y" ]] || exit 1
fi

# Pick a Python 3 that py2app and rumps both support. 3.12 is the sweet spot
# on Apple Silicon with current Homebrew.
PYTHON_BIN=""
for candidate in \
    /opt/homebrew/bin/python3.12 \
    /opt/homebrew/bin/python3.11 \
    /opt/homebrew/bin/python3.10 \
    /usr/bin/python3; do
    if [[ -x "${candidate}" ]]; then
        PYTHON_BIN="${candidate}"
        break
    fi
done

if [[ -z "${PYTHON_BIN}" ]]; then
    echo "ERROR: no compatible Python found. Install via:" >&2
    echo "  brew install python@3.12" >&2
    exit 1
fi

echo "==> using Python: ${PYTHON_BIN} ($(${PYTHON_BIN} -V 2>&1))"

# --- step 2: is a player available for the *built* app? --------------------
# The bundle does not ship mpv any more (a Homebrew mpv cannot load from inside
# a bundle — see scripts/setup_py2app.py — and dragging its dylibs in cost
# ~50 MB). The app therefore uses the mpv installed on the machine, so warn
# when this host has none. Ship a self-contained player by dropping it at
# bin/MacOS/mpv or by exporting MACAST_BUNDLE_MPV=/path/to/mpv.
if [[ -x "/opt/homebrew/bin/mpv" || -x "/usr/local/bin/mpv" || -n "${MACAST_BUNDLE_MPV:-}" ]]; then
    echo "==> mpv: found on this machine (will be used at runtime)"
else
    echo "WARNING: no mpv found on this host and MACAST_BUNDLE_MPV is unset." >&2
    echo "         The built app needs an mpv to play media:" >&2
    echo "           brew install mpv        # or MACAST_BUNDLE_MPV=/path/to/mpv" >&2
fi

# --- step 3: build virtualenv and install deps ----------------------------
if [[ ! -d "${VENV_DIR}" ]]; then
    echo "==> creating virtualenv at ${VENV_DIR}"
    "${PYTHON_BIN}" -m venv "${VENV_DIR}"
fi

# shellcheck disable=SC1091
source "${VENV_DIR}/bin/activate"

echo "==> installing build dependencies"
pip install --quiet --upgrade pip

# Runtime dependencies come from requirements/darwin.txt, NOT from a second
# inline list. The two used to be maintained separately and they drifted:
# `zeroconf` was added to the code and to requirements/common.txt but not to
# the inline list here, so the app it produced built fine and then died on
# launch with "ModuleNotFoundError: No module named 'zeroconf'".
pip install --quiet -r "${PROJECT_ROOT}/requirements/darwin.txt"

# Build-only extras (py2app itself; pillow is what the CI job installs and is
# harmless at runtime — the macOS menu bar uses rumps, not PIL).
pip install --quiet 'py2app>=0.28' 'pillow'

# --- step 3b: gettext catalogues ------------------------------------------
# Only .po sources are kept in git; gettext needs the compiled .mo at runtime.
# scripts/setup_py2app.py compiles them in pure Python as part of the build, so
# no gettext/msgfmt install is needed (and CI gets localized builds too).

# --- step 4: run py2app ---------------------------------------------------
echo "==> running py2app"
rm -rf "${DIST_DIR}" "${BUILD_DIR}"
python "${PY2APP_SETUP}" py2app

APP="${DIST_DIR}/Macast.app"
if [[ ! -d "${APP}" ]]; then
    echo "ERROR: py2app did not produce ${APP}" >&2
    exit 1
fi

# --- step 5: verify --------------------------------------------------------
# (The bundle is trimmed by scripts/setup_py2app.py right after py2app runs, so
# that CI and manual builds shrink identically.)
echo "==> verifying bundle"

LAUNCHER="${APP}/Contents/MacOS/Macast"
LAUNCHER_ARCH="$(file "${LAUNCHER}" | sed -n 's/.*: //p')"
MPV_PATH="${APP}/Contents/Resources/bin/MacOS/mpv"

echo "    launcher : ${LAUNCHER_ARCH}"
if [[ -f "${MPV_PATH}" ]]; then
    MPV_ARCH="$(file "${MPV_PATH}" | sed -n 's/.*: //p')"
    echo "    mpv      : ${MPV_ARCH} (bundled)"
else
    echo "    mpv      : not bundled (the app uses the system mpv)"
fi
echo "    size     : $(du -sh "${APP}" | cut -f1)"
MO_COUNT="$(find "${APP}/Contents/Resources/i18n" -name '*.mo' 2>/dev/null | wc -l | tr -d ' ')"
echo "    i18n     : ${MO_COUNT} compiled catalogue(s)"
echo "    bundle   : ${APP}"

if [[ "${LAUNCHER_ARCH}" != *arm64* ]]; then
    echo "ERROR: launcher is not arm64: ${LAUNCHER_ARCH}" >&2
    exit 1
fi
if [[ "${MO_COUNT}" -eq 0 ]]; then
    echo "ERROR: no .mo catalogue in the bundle — the app would be English-only." >&2
    echo "       i18n/*/LC_MESSAGES/*.po were not compiled (see" >&2
    echo "       compile_i18n_catalogues in scripts/setup_py2app.py)." >&2
    exit 1
fi
if [[ -f "${MPV_PATH}" ]]; then
    MPV_ARCH="$(file "${MPV_PATH}" | sed -n 's/.*: //p')"
    if [[ "${MPV_ARCH}" != *arm64* ]]; then
        echo "ERROR: bundled mpv is not arm64: ${MPV_ARCH}" >&2
        exit 1
    fi
fi

echo
echo "==> build complete"
echo "    open with:  open '${APP}'"
echo "    copy to  :  cp -R '${APP}' /Applications/"
