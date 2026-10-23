//! Bundle `frontend/js/**` into `frontend/bundle.js`.
//!
//! Replaces `scripts/bundle_frontend.py`, which is gone. Run it with
//! `cargo run -p macast-frontend`, or `--check` to verify without writing.
//!
//! ## Why this is its own crate and not a bin of `src-tauri`
//!
//! It started as `src-tauri/src/bin/bundle-frontend.rs`, which worked on the machine that had
//! already built Tauri — and would have failed in CI. A bin inherits its package's
//! `[dependencies]`, so `cargo run -p macast-tauri --bin bundle-frontend` compiles `tauri`
//! first, and `tauri` needs **webkit2gtk + libgtk** on Linux. The "bundle is current" check
//! belongs in a headless CI job; a bundler that drags a GUI toolkit into it does not.
//!
//! ## Why a bundler at all
//!
//! The window's webview does not fetch subresources. Measured 2026-10-22 by asking the live
//! document through a sink socket (`docs/Rust-Port-Design.md` §16 第 60 条):
//!
//! ```text
//! <body>            parsed, 17375 bytes of DOM        — the HTML itself arrives
//! cards             13 static cards present            — the markup is all there
//! window.__TAURI__  present, `core.invoke` callable    — the bridge is injected
//! <script src>      never requested, module *or* classic, relative *or* absolute
//!                   (`performance.getEntriesByType('resource')` stays empty)
//! eval(...)         runs                                — the engine executes JS
//! ```
//!
//! So the page cannot depend on loading a file: `src-tauri` reads `frontend/bundle.js` with
//! `include_str!` and hands it to the window with `eval`.
//!
//! ## Why not a real bundler
//!
//! `esbuild`/`rollup` would add a toolchain and a lockfile to a workspace that builds with
//! `cargo build` alone. The graph is eleven modules with named imports, no dynamic imports and
//! no cycles; a resolver that handles exactly that shape is this file, and it cannot silently
//! grow a config nobody reads.
//!
//! ## What it deliberately refuses
//!
//! Only two module shapes exist in this tree — named imports and `export function|const|let|
//! class`. Anything else is a hard error rather than a pass-through, because a bundler that
//! skips what it does not recognise produces a bundle that fails at runtime in exactly the way
//! this file exists to prevent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the modules live, and where the bundle goes.
pub struct Layout {
    pub js_root: PathBuf,
    pub index: PathBuf,
    pub bundle: PathBuf,
}

impl Layout {
    /// The repository layout, found from this binary's own manifest directory.
    pub fn repo() -> Layout {
        // `macast-frontend/` sits one level under the repository root, like every other crate:
        // `car/` itself is the manifest directory, so the root is its parent.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("macast-frontend has a parent directory")
            .to_path_buf();
        Layout {
            js_root: root.join("frontend/js"),
            index: root.join("frontend/index.html"),
            bundle: root.join("frontend/bundle.js"),
        }
    }
}

/// The marker pair that must stay empty in `index.html`.
pub const BEGIN: &str = "<!-- BUNDLE:BEGIN -->";
pub const END: &str = "<!-- BUNDLE:END -->";

/// One `import … from './x.js';` line.
///
/// Hand-parsed rather than regex-driven: the shape is fixed (one line, single quotes, always a
/// relative `.js` path), and a regex dependency for this would be the only one in the binary.
#[derive(Debug, PartialEq)]
enum ImportLine {
    /// `import * as playing from './user/x.js';`
    Namespace { alias: String, spec: String },
    /// `import { a, b as c } from './lib/y.js';`
    Named { names: Vec<(String, String)>, spec: String },
}

fn parse_import(line: &str) -> Option<ImportLine> {
    let rest = line.trim_start().strip_prefix("import ")?;
    if let Some(tail) = rest.strip_prefix("* as ") {
        let (alias, after) = tail.split_once(" from '")?;
        let spec = after.strip_suffix("';")?;
        return Some(ImportLine::Namespace {
            alias: alias.trim().to_string(),
            spec: spec.to_string(),
        });
    }
    let after_brace = rest.strip_prefix('{')?;
    let (names, after) = after_brace.split_once("} from '")?;
    let spec = after.strip_suffix("';")?;
    let parsed = names
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let part = part.trim();
            match part.split_once(" as ") {
                Some((name, alias)) => (name.trim().to_string(), alias.trim().to_string()),
                None => (part.to_string(), part.to_string()),
            }
        })
        .collect();
    Some(ImportLine::Named {
        names: parsed,
        spec: spec.to_string(),
    })
}

/// A module path relative to `js_root`, always with `/` separators.
fn relative_to_root(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or_else(|_| panic!("{} is not under {}", path.display(), root.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

/// The namespace binding for one module, derived from its path.
///
/// `lib/bridge.js` becomes `__bridge`, `user/now-playing.js` becomes `__now_playing`. The
/// basename is unique in this tree; `the_page_guards_read_every_module_that_exists` and the
/// bundle check both fail loudly if that ever stops being true.
fn module_binding(relative: &str) -> String {
    let stem = relative
        .rsplit('/')
        .next()
        .unwrap_or(relative)
        .trim_end_matches(".js")
        .replace('-', "_");
    format!("__{stem}")
}

/// Resolve `spec` against the importer, as a path relative to `js_root`.
fn resolve(spec: &str, importer: &Path, root: &Path) -> PathBuf {
    let joined = importer
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(spec);
    // `Path::join` keeps `..` segments; normalise them so two spellings of one file compare
    // equal and so the printed name matches what the module header must say.
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    };
    debug_assert!(out.starts_with(root));
    out
}

/// One module, after its import/export syntax has been stripped.
struct Module {
    relative: String,
    body: String,
    exports: Vec<String>,
}

/// Read one module and rewrite its module syntax.
fn load(path: &Path, root: &Path) -> Module {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display()));
    let relative = relative_to_root(path, root);

    let mut body: Vec<String> = Vec::new();
    let mut exports: Vec<String> = Vec::new();

    for line in source.lines() {
        if let Some(import) = parse_import(line) {
            match import {
                ImportLine::Namespace { alias, spec } => {
                    let target = relative_to_root(&resolve(&spec, path, root), root);
                    // `import * as playing` — the namespace object *is* the binding.
                    body.push(format!("const {alias} = {};", module_binding(&target)));
                }
                ImportLine::Named { names, spec } => {
                    let target = relative_to_root(&resolve(&spec, path, root), root);
                    let pairs: Vec<String> = names
                        .iter()
                        .map(|(name, alias)| {
                            if name == alias {
                                name.clone()
                            } else {
                                format!("{name}: {alias}")
                            }
                        })
                        .collect();
                    body.push(format!(
                        "const {{ {} }} = {};",
                        pairs.join(", "),
                        module_binding(&target)
                    ));
                }
            }
            continue;
        }

        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("export ") {
            // `export async function f` is the only `async` shape in the tree. Strip it
            // **before** looking at the keyword: `rest.split_whitespace().next()` is `async`,
            // not `function`, and stripping it from that single word leaves `"async"` — the
            // first version of this did exactly that and refused a module the bundler must
            // accept (measured by the byte-for-byte check against the Python original).
            // Two views of the same line, and they must not be confused: `stripped` is for
            // *classification* (`async function f` starts with `async`), while `rest` is what
            // goes into the bundle — dropping the `async` while emitting turns every awaited
            // module into a synchronous one that returns a promise the caller does not await,
            // which is a whole category of runtime bug with no compile-time tell.
            let stripped = rest.strip_prefix("async ").unwrap_or(rest);
            let keyword = stripped.split_whitespace().next().unwrap_or("");
            if matches!(keyword, "function" | "const" | "let" | "class") {
                // The name is the identifier after the keyword, cut at whichever of `(`, `=`,
                // `:` or `<` ends it.
                let after = stripped
                    .split_whitespace()
                    .skip(1)
                    .next()
                    .and_then(|word| word.split(['(', '=', ':', '<']).next())
                    .unwrap_or("")
                    .trim();
                if after.is_empty() {
                    panic!("{relative}: cannot read the exported name from: {trimmed}");
                }
                exports.push(after.to_string());
                body.push(rest.to_string());
                continue;
            }
            panic!(
                "{relative}: unsupported export form: {} — this bundler inlines one module per \
                 scope, so it must know every name a module offers",
                trimmed.chars().take(60).collect::<String>()
            );
        }
        body.push(line.to_string());
    }

    Module {
        relative,
        body: body.join("\n").trim().to_string(),
        exports,
    }
}

/// Walk the graph depth-first, dependencies first, so a module is defined before its importer.
fn order(entry: &Path, root: &Path) -> Vec<PathBuf> {
    fn visit(
        path: &Path,
        root: &Path,
        seen: &mut HashMap<PathBuf, ()>,
        stack: &mut Vec<PathBuf>,
        out: &mut Vec<PathBuf>,
    ) {
        if seen.contains_key(path) {
            return;
        }
        if stack.iter().any(|held| held == path) {
            let names: Vec<String> = stack
                .iter()
                .map(|p| relative_to_root(p, root))
                .chain(std::iter::once(relative_to_root(path, root)))
                .collect();
            panic!("import cycle: {}", names.join(" -> "));
        }
        if !path.exists() {
            panic!("missing module: {}", path.display());
        }

        stack.push(path.to_path_buf());
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display()));
        for line in source.lines() {
            if let Some(import) = parse_import(line) {
                let spec = match &import {
                    ImportLine::Namespace { spec, .. } | ImportLine::Named { spec, .. } => spec,
                };
                visit(&resolve(spec, path, root), root, seen, stack, out);
            }
        }
        stack.pop();

        seen.insert(path.to_path_buf(), ());
        out.push(path.to_path_buf());
    }

    let mut out = Vec::new();
    visit(
        entry,
        root,
        &mut HashMap::new(),
        &mut Vec::new(),
        &mut out,
    );
    out
}

/// Produce the bundle from the modules on disk.
pub fn assemble(layout: &Layout) -> String {
    let entry = layout.js_root.join("app.js");
    let mut chunks: Vec<String> = Vec::new();

    for path in order(&entry, &layout.js_root) {
        let module = load(&path, &layout.js_root);
        if module.exports.is_empty() {
            // **A side-effect module runs at its own position**, not inside an extra function.
            // Wrapping it looked harmless and was not: `app.js` is the entry and its top level
            // *is* the boot sequence, so a wrapper left `mountShell(); show(start); boot();`
            // inside a function nobody called — the window rendered 13 cards and stayed hidden
            // (measured 2026-10-22).
            chunks.push(format!(
                "/* ---- {} ---- */\n{}\n",
                module.relative, module.body
            ));
        } else {
            // One function scope per module, so two modules may both keep a local `state` —
            // which the source does today. The IIFE's return value is the namespace that
            // `import * as x` binds to in the real thing.
            chunks.push(format!(
                "/* ---- {name} ---- */\nconst {binding} = (() => {{\n{body}\n\nreturn {{ {exports} }};\n}})();\n",
                name = module.relative,
                binding = module_binding(&module.relative),
                body = module.body,
                exports = module.exports.join(", "),
            ));
        }
    }

    format!("(() => {{\n'use strict';\n\n{}}})();\n", chunks.join("\n"))
}

/// Empty the inline region, if anything crept into it.
///
/// It exists only as a marker pair: the script cannot travel in the document, because an
/// inline script this size holds this webview's parser at `<head>` (measured 2026-10-22). The
/// markers stay so a reader of `index.html` learns where the behaviour comes from.
fn clear_inline(layout: &Layout) -> Result<(), String> {
    let html = std::fs::read_to_string(&layout.index)
        .map_err(|why| format!("cannot read {}: {why}", layout.index.display()))?;
    let begin = html
        .find(BEGIN)
        .ok_or_else(|| format!("{} has no {}", layout.index.display(), BEGIN))?
        + BEGIN.len();
    let end = html
        .find(END)
        .ok_or_else(|| format!("{} has no {}", layout.index.display(), END))?;
    if !html[begin..end].trim().is_empty() {
        let cleaned = format!("{}{}", &html[..begin], &html[end..]);
        std::fs::write(&layout.index, cleaned)
            .map_err(|why| format!("cannot write {}: {why}", layout.index.display()))?;
    }
    Ok(())
}

/// What `--check` decided.
pub struct Check {
    pub stale_bundle: bool,
    pub inline_present: bool,
    pub expected: String,
}

/// Compare the checked-in bundle with what the modules produce now.
pub fn check(layout: &Layout) -> Check {
    let expected = assemble(layout);
    // **Normalise the checked-in file's line endings before comparing.** `assemble` joins with
    // `\n` unconditionally (`source.lines()` drops any CR), so a checkout that arrived with
    // `core.autocrlf=true` would differ on every line and report "stale" for a bundle that is
    // byte-identical in the repository. That is exactly what happened on the Windows CI job the
    // first time this check ran there (2026-10-23) while macOS and Linux passed.
    //
    // `.gitattributes` pins `frontend/bundle.js` to `eol=lf`, which is the real fix; this is the
    // second half, so a developer whose global git config rewrites it still gets a true answer
    // instead of a red that says "run the bundler" when running it changes nothing.
    let actual = std::fs::read_to_string(&layout.bundle)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let html = std::fs::read_to_string(&layout.index).unwrap_or_default();
    let inline_present = match (html.find(BEGIN), html.find(END)) {
        (Some(b), Some(e)) => !html[b + BEGIN.len()..e].trim().is_empty(),
        _ => true,
    };
    Check {
        stale_bundle: actual != expected,
        inline_present,
        expected,
    }
}

/// Write the bundle and clear the inline region.
pub fn run(layout: &Layout) -> Result<usize, String> {
    let bundle = assemble(layout);
    std::fs::write(&layout.bundle, &bundle)
        .map_err(|why| format!("cannot write {}: {why}", layout.bundle.display()))?;
    clear_inline(layout)?;
    Ok(bundle.len())
}

fn main() {
    let layout = Layout::repo();
    let checking = std::env::args().any(|arg| arg == "--check");

    if checking {
        let result = check(&layout);
        let mut ok = true;
        if result.stale_bundle {
            eprintln!("frontend/bundle.js is stale — run `cargo run -p macast-tauri --bin bundle-frontend`");
            ok = false;
        }
        if result.inline_present {
            eprintln!(
                "index.html carries an inline script again — it must stay empty, because an \
                 inline script this size holds this webview's parser at <head>"
            );
            ok = false;
        }
        if ok {
            println!("bundle is current ({} bytes)", result.expected.len());
        }
        std::process::exit(if ok { 0 } else { 1 });
    }

    match run(&layout) {
        Ok(bytes) => println!(
            "bundled {bytes} bytes into {}",
            layout
                .bundle
                .strip_prefix(layout.js_root.parent().and_then(Path::parent).unwrap_or(Path::new(".")))
                .unwrap_or(&layout.bundle)
                .display()
        ),
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(1);
        }
    }
}
