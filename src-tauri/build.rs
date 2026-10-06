//! The app icons here are **derived**, never duplicated.
//!
//! `macast/assets/` already owns the artwork: `icon.png` (1024), `icon-512.png`,
//! `icon.icns`. A checkout of this branch that also kept a `src-tauri/icons/`
//! copy would be two copies of a logo that can disagree, which is the same
//! mistake §4.3 of AGENTS.md documents for dependency lists — one fact, one
//! owner. So the bundler's directory is filled from the owner on every build and
//! is gitignored, and `tauri.conf.json` lists only what this function can
//! produce deterministically on any host: a plain copy, no resampling.
//!
//! Cross-packaging note for the packaging step (not this one): a Linux bundle
//! wants `32x32.png` / `128x128.png` / `128x128@2x.png` and a Windows one wants
//! `icon.ico`. Resizing needs a real image library (`sips` is macOS-only), so
//! those sizes are *not* faked here — the job that builds those platforms runs
//! `cargo tauri icon ../macast/assets/icon.png`, which writes the whole set.

use std::fs;
use std::path::Path;

/// `(source under macast/assets, destination under src-tauri/icons)`.
const ICONS: &[(&str, &str)] = &[
    ("icon-512.png", "icon.png"),
    ("icon.icns", "icon.icns"),
];

fn main() {
    derive_icons();
    tauri_build::build()
}

fn derive_icons() {
    let assets = Path::new("../macast/assets");
    let out = Path::new("icons");
    if let Err(why) = fs::create_dir_all(out) {
        println!("cargo:warning=cannot create src-tauri/icons: {why}");
        return;
    }
    for (src_name, dst_name) in ICONS {
        let src = assets.join(src_name);
        let dst = out.join(dst_name);
        println!("cargo:rerun-if-changed={}", src.display());
        if let Err(why) = refresh(&src, &dst) {
            // Loud, but not fatal: `cargo check` of the *core* crates must keep
            // working in a checkout without the artwork (a `--depth 1` clone of
            // a submodule-less tree, a source-only build of the CLI).
            println!("cargo:warning=icon {src_name} -> {dst_name}: {why}");
        }
    }
    println!("cargo:rerun-if-changed=tauri.conf.json");
}

/// Copy `src` to `dst` unless `dst` already holds exactly those bytes.
///
/// Byte equality rather than mtime: `git checkout` rewrites mtimes, so a
/// timestamp comparison would make every clone look stale and re-copy files that
/// did not change — and, worse, a partially written copy could be newer than its
/// source and be trusted forever after.
fn refresh(src: &Path, dst: &Path) -> Result<(), String> {
    let want = fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    if fs::read(dst).ok().as_deref() == Some(want.as_slice()) {
        return Ok(());
    }
    // Write-then-rename, so a build interrupted mid-copy cannot leave the
    // bundler reading half an icon.
    let tmp = dst.with_extension("tmp");
    fs::write(&tmp, &want).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, dst).map_err(|e| format!("rename into {}: {e}", dst.display()))
}
