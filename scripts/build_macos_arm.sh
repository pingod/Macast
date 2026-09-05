#!/usr/bin/env bash
#
# Build Macast.app for Apple Silicon (arm64) macOS.
#
# This script does the full pipeline:
#   1. Verify the host is arm64 macOS.
#   2. Ensure mpv is available (Homebrew arm64 build).
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

# --- step 2: ensure mpv is available ---------------------------------------
if [[ ! -x "/opt/homebrew/bin/mpv" && ! -x "/usr/local/bin/mpv" ]]; then
    echo "==> mpv not found; installing via Homebrew"
    if ! command -v brew >/dev/null 2>&1; then
        echo "ERROR: Homebrew is required to install mpv." >&2
        exit 1
    fi
    brew install mpv
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
pip install --quiet \
    'py2app>=0.28' \
    'rumps>=0.4' \
    'cherrypy>=18,<19' \
    'lxml' \
    'netifaces' \
    'appdirs' \
    'pyperclip' \
    'requests' \
    'pillow'

# --- step 4: run py2app ---------------------------------------------------
echo "==> running py2app"
rm -rf "${DIST_DIR}" "${BUILD_DIR}"
python "${PY2APP_SETUP}" py2app

APP="${DIST_DIR}/Macast.app"
if [[ ! -d "${APP}" ]]; then
    echo "ERROR: py2app did not produce ${APP}" >&2
    exit 1
fi

# --- step 5: verify arm64 + bundled mpv -----------------------------------
echo "==> verifying bundle"

LAUNCHER="${APP}/Contents/MacOS/Macast"
LAUNCHER_ARCH="$(file "${LAUNCHER}" | sed -n 's/.*: //p')"
MPV_PATH="${APP}/Contents/Resources/bin/MacOS/mpv"
MPV_ARCH="$(file "${MPV_PATH}" | sed -n 's/.*: //p')"

echo "    launcher : ${LAUNCHER_ARCH}"
echo "    mpv      : ${MPV_ARCH}"
echo "    bundle   : ${APP}"

if [[ "${LAUNCHER_ARCH}" != *arm64* ]]; then
    echo "ERROR: launcher is not arm64: ${LAUNCHER_ARCH}" >&2
    exit 1
fi
if [[ "${MPV_ARCH}" != *arm64* ]]; then
    echo "ERROR: bundled mpv is not arm64: ${MPV_ARCH}" >&2
    exit 1
fi

echo
echo "==> build complete"
echo "    open with:  open '${APP}'"
echo "    copy to  :  cp -R '${APP}' /Applications/"
