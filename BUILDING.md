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
| `scripts/setup_py2app.py` (new) | Modern py2app config: explicit `arch='arm64'`, excludes desktop GUI stacks we don't use, and trims the bundle after the build (test-suites, CPython's own test-suite, `.dSYM` debug bundles). **mpv is not bundled** — see "Player (mpv)" below. |
| `Macast.set_mpv_default_path` (updated) | Probe each candidate mpv with `mpv --version` before accepting it, then fall back to Homebrew's `/opt/homebrew/bin/mpv` (or `/usr/local/bin/mpv`) and finally `$PATH`. The probe exists because a bundled copy can be present-and-executable yet still fail to launch with `dyld: Library not loaded` (see "Player (mpv)"). |
| `scripts/build_macos_arm.sh` (new) | One-command end-to-end build: verifies host, creates the venv, installs deps, runs py2app, and verifies the output is arm64. All build artefacts stay in the project directory. |
| gettext catalogues (moved into the build) | `.mo` files are build output and stay out of git, so they must be compiled at build time. `scripts/setup_py2app.py` now does it in pure Python (`compile_i18n_catalogues`, cross-checked against `msgfmt`), which removed the `msgfmt`/gettext requirement — and fixed the CI-built `.app`, which previously got no `.mo` files at all and shipped English-only. |
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
- Homebrew with `python@3.12`:
  ```bash
  brew install python@3.12
  ```
- `mpv` is **not** needed to build, but the built app needs one to play media
  (`brew install mpv`). See "Player (mpv)" below.

### One-command build

```bash
bash scripts/build_macos_arm.sh
```

This produces `dist/Macast.app` (≈ 36 MB) containing:

- `Contents/MacOS/Macast` — py2app launcher (arm64)
- `Contents/Resources/i18n/` — translations (compiled from `.po` at build time)
- `Contents/Resources/lib/python3.X/` — embedded Python stdlib + site-packages
- `Contents/Resources/bin/MacOS/mpv` — *only if you opted into a bundled
  player*

### Player (mpv)

The .app does **not** ship a player by default; `Macast.set_mpv_default_path`
finds the `mpv` already installed on the machine
(`/opt/homebrew/bin/mpv` → `/usr/local/bin/mpv` → `$PATH`). That is the same
contract the upstream README states for macOS: install mpv, then run Macast.

Bundling Homebrew's `mpv` does not work and costs a lot:

- it links against absolute `/opt/homebrew/opt/*` paths (libass, ffmpeg,
  libplacebo, mujs, lcms2, libarchive, …), so py2app rewrites ~14 dylibs and
  copies them into `Contents/Frameworks/` — about **53 MB**;
- those dylibs are placed where the binary does not look for them, so the copy
  still dies with `dyld: Library not loaded` when spawned from inside the
  bundle;
- net effect: a **102 MB** bundle whose embedded player never runs and which
  silently falls back to the system mpv anyway.

To ship a genuinely self-contained player, point `MACAST_BUNDLE_MPV` at a
**portable** build — upstream's CI used
`https://laboratory.stolendata.net/~djinn/mpv_osx/mpv-latest.tar.gz`:

```bash
curl -LO https://laboratory.stolendata.net/~djinn/mpv_osx/mpv-latest.tar.gz
mkdir -p bin && tar --strip-components 2 -C bin -xzvf mpv-latest.tar.gz \
    mpv.app/Contents/MacOS
MACAST_BUNDLE_MPV="$PWD/bin/MacOS/mpv" python scripts/setup_py2app.py py2app
```

`scripts/setup_py2app.py` runs `otool -L` on the candidate and refuses anything
that links outside `/usr/lib` + `/System/Library` + `@rpath`/`@loader_path`/
`@executable_path`, so a Homebrew mpv cannot be bundled by accident.

### Bundle size

| Item | Size | Notes |
| --- | --- | --- |
| `Contents/Frameworks/` | 11 MB | libpython + OpenSSL (`libcrypto` 4.3 MB, `libssl` 0.8 MB) |
| stdlib archive (`python312.zip`) | 4.65 MB | pyobjc, stdlib; test-suite and `.dSYM` are pruned |
| `lib-dynload/` | 17 MB | 7.9 MB of that is `lxml/etree.so` |
| `macast/` + `cherrypy/` + `rumps/` | 4 MB | app code, settings-page assets, cherrypy |
| **total** | **≈ 36 MB** | |

The largest single remaining item is `lxml` (≈ 8 MB). `macast/protocol.py` uses
it for SOAP/`description.xml` construction; swapping those 23 call sites for
`xml.etree.ElementTree` would remove it, at the cost of changing namespace and
serialization behaviour in DLNA responses.

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

# 3. run py2app (this also trims the bundle)
rm -rf dist build
python scripts/setup_py2app.py py2app

# 4. verify
file dist/Macast.app/Contents/MacOS/Macast   # → arm64
du -sh dist/Macast.app                       # → ~39M
```

## Verify the bundled plugins actually made it into the artefact

`macast/plugins/**` is imported **by name at runtime**, never by a static
`import` statement, so no dependency scanner can see it. `packages: ['macast']`
copies the directory, and `hiddenimports` names each module so modulegraph
cannot drop it — but a green build still proves nothing, so check the bundle:

```bash
APP=/Applications/Macast.app     # or dist/Macast.app
ls "$APP/Contents/Resources/lib/python3.12/macast/plugins/renderer/"
# → iina.py web.py live.py potplayer.py pi_fm.py
ls "$APP/Contents/Resources/lib/python3.12/macast/plugins/protocol/"
# → nirvana.py

# and confirm the app finds them at runtime
"$APP/Contents/MacOS/Macast" & sleep 10
curl -s 'http://127.0.0.1:58880/api?query=plugin-info' | python3 -m json.tool \
  | grep -E '"title"|"available"'
```

On macOS only `IINA Renderer`, `Web Renderer`, `Live Renderer` and
`NVA Protocol` should report `"available": true`; PotPlayer (Windows) and
PIFMRDS (Linux) must still be **listed** with `"available": false`. A plugin
missing from the list entirely means it was stripped from the bundle.

## Verify the mirror console ships

The 「电脑投屏」 control surface is no longer a module of its own — it is the
settings page (`macast/xml/setting.html`, the 「电脑投屏」 tab) plus
`macast/mirror_view.py`, which decides which cards that tab renders. Neither is
reached by an `import`, so the same blind spot as the plugins applies: an artefact
that lost one of them still starts, still serves `/`, and simply shows a page with
no mirror tab.

```bash
APP=/Applications/Macast.app     # or dist/Macast.app
ls "$APP/Contents/Resources/lib/python3.12/macast/xml/setting.html"
ls "$APP/Contents/Resources/lib/python3.12/macast/mirror_view.py"

# then ask the running artefact, not the filesystem
"$APP/Contents/MacOS/Macast" & sleep 10
curl -s 'http://127.0.0.1:58880/' | grep -c '电脑投屏'
```

`0` means the tab is not in the artefact. Restarting does not fix it —
`Handler.__init__` caches the template at startup — and a missing `mirror_view.py`
is worse than a crash: the page falls back to no cards at all.

Two things this surface needs that no import will check either:

- **`NSMicrophoneUsageDescription` in the built `Info.plist`** (declared in
  `scripts/setup_py2app.py`). Without it macOS refuses the audio capture *without
  ever showing the prompt*, and the one-click BlackHole setup reports a failure the
  user cannot explain. Part 29 pins the key in the build script.
- **The screen-recording grant belongs to the binary**, so replacing or re-signing
  the `.app` revokes it. A fresh artefact shows the preview card's reason instead of
  a picture until the grant is given again — that is expected, not a regression.

The window this section used to describe (`macast/mirror_console.py`, a Tk process
launched from the menu) is deleted; Part 35 asserts that no build file, workflow or
app module still names it.

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
| `bundled mpv is not arm64` | You vendored an mpv at `bin/MacOS/mpv` for the wrong architecture | Use a native arm64 portable build, or unset `MACAST_BUNDLE_MPV`/remove the file and rely on the system mpv |
| App plays nothing / logs `No mpv found` | No mpv on the machine and none bundled | `brew install mpv`, or bundle a portable build via `MACAST_BUNDLE_MPV` |
| Bundle is ~100 MB | An mpv got bundled from Homebrew (`otool -L` shows `/opt/homebrew/opt/*`) | Remove `bin/MacOS/mpv` or `MACAST_BUNDLE_MPV`; only a portable mpv is accepted |
| `LSArchitecturePriority: arm64` rejected on launch | Trying to run the bundle on macOS older than 11 | Bump `LSMinimumSystemVersion` in `scripts/setup_py2app.py` down to your target |
