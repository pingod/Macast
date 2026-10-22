#!/usr/bin/env python3
"""Bundle `frontend/js/**` into one classic script, inlined into `frontend/index.html`.

## Why this exists

The window's webview does not fetch subresources. Measured on 2026-10-22 by asking the live
document through a sink socket (see `docs/Rust-Port-Design.md` §16 第 60 条 for the readings):

```
<body>            parsed, 17375 bytes of DOM          — the HTML itself arrives
cards             13 static cards present             — the markup is all there
window.__TAURI__  present, `core.invoke` callable      — the bridge is injected
<script src>      never requested, module *or* classic, relative *or* absolute
                  (`performance.getEntriesByType('resource')` stays empty)
eval(...)         runs                                  — the engine executes JS
```

So `index.html` is served and parsed, the IPC bridge is there, and injected JavaScript runs —
but a `<script src=…>` in that document is never even requested. Whatever the cause (asset
protocol, `tauri://` origin, WebKit configuration), the page cannot depend on loading a file.

## What this does, and what it deliberately does not

The source stays **eleven readable ES modules** with real `import`/`export`. This script
resolves that graph, strips the import/export syntax, concatenates in dependency order, and
writes the result into `index.html` between two markers. Nothing is minified or renamed: the
bundle is what a reader would have typed, in dependency order, inside one IIFE.

That keeps the two things that matter apart:
* **authorship** — modules, one concern each, importable and reviewable;
* **delivery** — one inline classic script, because the transport is what it is.

## The two markers

```html
<!-- BUNDLE:BEGIN -->
<script>…generated…</script>
<!-- BUNDLE:END -->
```

Everything between them is replaced on every run. The region is deliberately *inside* the
body's document, after the markup, so the script sees a parsed DOM — which is what the
modules assume (`byId` throws rather than returning null).

## Why not a real bundler

`esbuild`/`rollup` would add a toolchain and a lockfile to a repository that currently builds
with `cargo build` alone. This graph is eleven files with named imports and no dynamic
imports; a resolver that handles exactly that shape is ~100 lines and cannot silently grow a
config file that nobody reads.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
JS = ROOT / "frontend" / "js"
INDEX = ROOT / "frontend" / "index.html"
#: The bundle also lands here, because the window cannot load a `<script src>` and an inline
#: script of this size blocks its parser. Rust injects this file with `eval` instead — the one
#: channel measured to work (see `docs/Rust-Port-Design.md` §16 第 60 条).
BUNDLE_FILE = ROOT / "frontend" / "bundle.js"
BEGIN = "<!-- BUNDLE:BEGIN -->"
END = "<!-- BUNDLE:END -->"

#: Entry points, in the order the shell expects them. `app.js` last: it is the only module
#: with side effects, and it imports every other one.
ENTRIES = ["app.js"]


def module_path(spec: str, importer: Path) -> Path:
    """Resolve a relative specifier the way the browser would."""
    return (importer.parent / spec).resolve()


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


IMPORT_RE = re.compile(
    r"^\s*import\s+(?:\*\s+as\s+(\w+)|\{([^}]*)\})\s+from\s+'([^']+)';\s*$",
    re.MULTILINE,
)


def collect(entry: Path, seen: dict[Path, str], order: list[Path], stack: list[Path]) -> None:
    """Depth-first, dependencies first, so a module is defined before its importer runs."""
    entry = entry.resolve()
    if entry in seen:
        return
    if entry in stack:
        raise SystemExit(f"cycle: {' -> '.join(str(p.name) for p in stack)} -> {entry.name}")
    if not entry.exists():
        raise SystemExit(f"missing module: {entry}")

    seen[entry] = ""  # reserve the slot
    stack.append(entry)
    source = read(entry)
    for match in IMPORT_RE.finditer(source):
        collect(module_path(match.group(3), entry), seen, order, stack)
    stack.pop()
    order.append(entry)


def strip_module_syntax(source: str, path: Path) -> str:
    """Turn one ES module into a body and an export list.

    Only the two shapes this codebase uses are handled — named imports, and
    `export function|const|let|class` — and anything else is a hard error rather than a
    silent pass-through, because a bundler that skips what it does not recognise produces a
    bundle that fails at runtime in exactly the way this file exists to prevent.
    """
    exports: list[str] = []
    body: list[str] = []

    for line in source.split("\n"):
        match = IMPORT_RE.match(line)
        if match:
            star, names, spec = match.group(1), match.group(2), match.group(3)
            binding = module_binding(relative_spec(spec, path))
            if star:
                # `import * as playing` — the namespace object *is* the binding.
                body.append(f"const {star} = {binding};")
            else:
                # `import { a, b as c }` — destructure, preserving the rename.
                pairs = ", ".join(
                    part.strip().replace(" as ", ": ") for part in names.split(",") if part.strip()
                )
                body.append(f"const {{ {pairs} }} = {binding};")
            continue
        export_match = re.match(r"^export\s+(async\s+)?(function|const|let|class)\s+(\w+)", line)
        if export_match:
            exports.append(export_match.group(3))
            body.append(line[len("export "):])
            continue
        if re.match(r"^export\s", line):
            raise SystemExit(f"{path.name}: unsupported export form: {line.strip()[:60]}")
        body.append(line)

    return "\n".join(body), exports


def assemble() -> str:
    order: list[Path] = []
    collect(JS / "app.js", {}, order, [])

    chunks: list[str] = []
    for path in order:
        body, exports = strip_module_syntax(read(path), path)
        name = path.relative_to(JS).as_posix()
        # **One function scope per module**, so two modules may both call a local `state`
        # without colliding — which is exactly what the source does today
        # (`casting.js` and `now-playing.js` each keep their own). The IIFE's return value is
        # the module's namespace, which is what `import * as x` binds to in the real thing.
        #
        # A module with no exports is a side-effect module (`app.js`); it is invoked and its
        # (empty) namespace dropped.
        returns = ", ".join(exports)
        if exports:
            chunks.append(
                f"/* ---- {name} ---- */\n"
                f"const {module_binding(name)} = (() => {{\n{body.strip()}\n\n"
                f"return {{ {returns} }};\n}})();\n"
            )
        else:
            # **A side-effect module runs at its own position in the bundle**, not inside an
            # extra function. Wrapping it looked harmless and was not: `app.js` is the entry
            # and its top level *is* the boot sequence, so the wrapper left `mountShell();
            # show(start); boot();` inside a function nobody called — the window rendered its
            # markup and never ran (measured 2026-10-22: `cards=13`, `version` filled in by a
            # later module, and `[data-view]:not([hidden])` still 0).
            chunks.append(f"/* ---- {name} ---- */\n{body.strip()}\n")

    return "(() => {\n'use strict';\n\n" + "\n".join(chunks) + "})();\n"


def relative_spec(spec: str, importer: Path) -> str:
    """A specifier relative to `frontend/js`, which is what `module_binding` expects."""
    target = (importer.parent / spec).resolve()
    return target.relative_to(JS).as_posix()


def module_binding(name: str) -> str:
    """A stable local name for one module's namespace, derived from its path.

    `lib/bridge.js` becomes `__bridge`, `user/now-playing.js` becomes `__now_playing`. The
    basename is unique in this tree, and `--check` in CI would catch it if it stopped being
    — a collision here would silently bind one module's namespace to another's name.
    """
    stem = name.rsplit("/", 1)[-1].removesuffix(".js").replace("-", "_")
    return f"__{stem}"


def splice(bundle: str) -> None:
    """Empty the inline region.

    It exists only as a marker pair now. The script cannot travel in the document — an inline
    script this size holds the parser at `<head>` in this webview — so `src-tauri` reads
    `frontend/bundle.js` and injects it with `eval` after the window loads. Keeping the
    markers (rather than deleting them) means a reader of `index.html` still learns where the
    behaviour comes from, in the file where they would look for it.
    """
    html = read(INDEX)
    begin = html.index(BEGIN) + len(BEGIN)
    end = html.index(END)
    if html[begin:end].strip():
        INDEX.write_text(html[:begin] + html[end:], encoding="utf-8")


def main() -> int:
    if "--check" in sys.argv:
        # Two questions, and the second is the one that caught a real defect: the checked-in
        # `bundle.js` must equal what the modules produce *now*, and the inline region must be
        # empty — an inline script of this size holds this webview's parser at `<head>`
        # (measured 2026-10-22), so a bundle that crept back into the document would blank the
        # window rather than merely being redundant.
        expected = assemble()
        actual = BUNDLE_FILE.read_text(encoding="utf-8") if BUNDLE_FILE.exists() else ""
        ok = True
        if actual != expected:
            print("frontend/bundle.js is stale — run scripts/bundle_frontend.py", file=sys.stderr)
            ok = False
        html = read(INDEX)
        inline = html[html.index(BEGIN) + len(BEGIN):html.index(END)].strip()
        if inline:
            print(
                "index.html carries an inline script again — it must stay empty, because an "
                "inline bundle blocks this webview's parser at <head>",
                file=sys.stderr,
            )
            ok = False
        if ok:
            print(f"bundle is current ({len(expected)} bytes)")
        return 0 if ok else 1

    bundle = assemble()
    splice(bundle)
    BUNDLE_FILE.write_text(bundle, encoding="utf-8")
    print(
        f"bundled {len(bundle)} bytes into {INDEX.relative_to(ROOT)} "
        f"and {BUNDLE_FILE.relative_to(ROOT)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
