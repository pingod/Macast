# Building Macast

This document covers building Macast from source for Apple Silicon (arm64)
macOS. Windows and Linux paths are unchanged — see `docs/Development.md`.

## What changed (refactor notes)

The 2025 maintenance pass modernized packaging and added a one-command build
without touching runtime code:

| Change | Why |
| --- | --- |
| `pyproject.toml` (new) | PEP 517 / 621 standard; modern `pip` and `build` work without falling back to `python setup.py install`. |
| `setup.py` updated | Dropped EOL Python 3.6–3.9, raised `python_requires` to 3.10, tightened classifier list. |
| `macast/_version.py` (new) | Single source of truth for the version (the `.version` dotfile is kept as a fallback for third-party tooling). |
| `requirements/darwin.txt` updated | Removed the GitHub forks of `pyperclip` on macOS — the upstream wheels are fine. |
| `scripts/setup_py2app.py` (new) | Modern py2app config: explicit `arch='arm64'`, bundles the Homebrew mpv binary into `Contents/Resources/bin/MacOS/mpv` (matches the path `Macast.py` looks up at runtime), excludes desktop GUI stacks we don't use. |
| `Macast.set_mpv_default_path` (updated) | Probe each candidate mpv with `mpv --version` before accepting it, fall back to Homebrew's `/opt/homebrew/bin/mpv` (or `/usr/local/bin/mpv`) when the bundled copy can't load its dylibs. The bundled mpv in a stock py2app build is present-but-broken because its 14 Homebrew dylibs are not copied alongside it; the probe lets the .app still play media without dragging ~30 MB of FFmpeg/libass/libplacebo into the bundle. |
| `scripts/build_macos_arm.sh` (new) | One-command end-to-end build: verifies host, creates the venv, installs deps, runs py2app, and verifies the output is arm64. All build artefacts stay in the project directory. |
| `setup_py2app.py` (root) | Now a shim that forwards to `scripts/setup_py2app.py` so old CI that runs `python setup.py py2app` still works. |
| `Makefile` (new) | `make build-arm`, `make run`, `make clean`, `make deep-clean`. |

Nothing under `macast/`, `macast_renderer/`, or the runtime entry points
(`Macast.py`, `macast/macast.py`) was changed — the existing code imports and
runs cleanly on Python 3.12.

## Building the macOS .app

### Prerequisites

- macOS 11 (Big Sur) or newer
- Apple Silicon (M1 / M2 / M3 / M4) for the arm64 build
- Xcode command line tools: `xcode-select --install`
- Homebrew with `python@3.12` and `mpv`:
  ```bash
  brew install python@3.12 mpv
  ```

### One-command build

```bash
bash scripts/build_macos_arm.sh
```

This produces `dist/Macast.app` containing:

- `Contents/MacOS/Macast` — py2app launcher (arm64)
- `Contents/Resources/bin/MacOS/mpv` — bundled mpv binary (arm64; Homebrew's mpv)
- `Contents/Resources/i18n/` — translations
- `Contents/Resources/lib/python3.X/` — embedded Python stdlib + site-packages

**About the bundled mpv:** Homebrew's `mpv` depends on ~14 dylibs in
`/opt/homebrew/opt/*` (libass, ffmpeg, libplacebo, mujs, lcms2, libarchive,
…). Copying just the `mpv` binary is not enough — dyld will fail to load the
shared libraries when launched from inside the bundle. To keep the build
script simple and the bundle size down, `Macast.set_mpv_default_path` now
*probes* each candidate mpv with `mpv --version` at startup and falls back
to the system Homebrew copy (`/opt/homebrew/bin/mpv` or
`/usr/local/bin/mpv`) if the bundled copy can't launch. So:

- **Recommended:** have `mpv` installed via Homebrew before opening the app.
  `brew install mpv` is enough; Macast picks it up automatically.
- **Self-contained:** copy all of mpv's dylibs into
  `Contents/Resources/bin/MacOS/` and rewrite their load paths with
  `dylibbundler` or `macdylibbundler`. Out of scope for this build script.

### Open the build

```bash
open dist/Macast.app
# or install system-wide:
cp -R dist/Macast.app /Applications/
```

### Clean build outputs

```bash
bash scripts/build_macos_arm.sh clean
# or via the Makefile:
make clean          # remove dist/ + build/
make deep-clean     # also remove the .venv-build/ virtualenv
```

### Manual build (if you need to tweak)

```bash
# 1. create venv
python3.12 -m venv .venv-build
source .venv-build/bin/activate

# 2. install runtime + build deps
pip install -U pip
pip install rumps 'py2app>=0.28' 'cherrypy>=18,<19' \
            lxml netifaces appdirs pyperclip requests pillow

# 3. run py2app
rm -rf dist build
python scripts/setup_py2app.py py2app

# 4. verify
file dist/Macast.app/Contents/MacOS/Macast   # → arm64
file dist/Macast.app/Contents/Resources/bin/MacOS/mpv   # → arm64
```

## Why py2app and not PyInstaller

The original project uses `py2app` (visible in `setup_py2app.py`,
`Macast.py`, `Macast.py`'s `_MEIPASS` lookups, and the GitHub Actions
workflow). It produces a real `.app` bundle with proper `Info.plist` keys
(LSUIElement = true → menu-bar-only app, NSHighResolutionCapable = true,
LSArchitecturePriority = arm64). PyInstaller would produce a single-file
binary without those niceties, and the existing `Macast.py` code path
(`osascript` checks, `NSBundle.mainBundle().bundlePath()` lookups) assumes
a real `.app`.

## Running without building

If you just want to run the app from source:

```bash
# from the project root
python3 Macast.py
# or
make run
```

The CLI (no menu bar UI) is `make cli` or `macast-cli` once installed.

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `py2app did not produce dist/Macast.app` | Stale `build/` dir | `rm -rf dist build` and re-run |
| `launcher is not arm64` | Running on Intel Mac | This build script targets arm64 only; the existing app on Intel macOS can be built from the legacy `setup_py2app.py` after switching `arch` to `'x86_64'` |
| `bundled mpv is not arm64` | Old mpv in `bin/MacOS/mpv` (if you maintain a vendored copy) | `brew install mpv` — Homebrew installs the native arm64 build on Apple Silicon |
| App launches then immediately quits with `mpv cannot start` | The bundled mpv is missing or wrong arch | Check `dist/Macast.app/Contents/Resources/bin/MacOS/mpv` and rebuild |
| `LSArchitecturePriority: arm64` rejected on launch | Trying to run the bundle on macOS older than 11 | Bump `LSMinimumSystemVersion` in `scripts/setup_py2app.py` down to your target |
