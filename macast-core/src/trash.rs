//! Recoverable removal: nothing in this application deletes a user file.
//!
//! AGENTS.md §10 states the rule ("破坏性操作要可逆"), and the Python original
//! implements it in exactly one shape: `shutil.move` into
//! `SETTING_DIR/.trash/<%Y%m%d-%H%M%S>/`. The same shape is used for uninstalled
//! plugins (`macast.py:uninstall`) and for a removed WebRTC extras tree, so it
//! lives here rather than in either caller.
//!
//! Two properties the callers depend on and that the tests below check:
//!
//! * the destination keeps the *base name*, so a restored file has the name the
//!   loader looks for;
//! * a collision inside the same second does not overwrite. The stamp is
//!   second-granular, and one click can move several files (a plugin plus its
//!   sidecar), so a taken name gets a numeric suffix rather than clobbering the
//!   earlier one — the same lesson [`crate::setting`] learned for corrupt
//!   settings files, where two quarantines in one second would otherwise lose
//!   the first user's bytes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{paths, timefmt};

/// What a move refused to do, in the words the UI shows.
///
/// The three shapes are the three things a caller can act on: nothing was there
/// to move, the destination could not be made, or the source could not be
/// moved. A failed move leaves the source exactly where it was.
#[derive(Debug)]
pub enum TrashError {
    Missing(PathBuf),
    Create(PathBuf, String),
    Move(PathBuf, String),
}

impl TrashError {
    /// The sentence for the settings page. Each one names the path involved —
    /// AGENTS.md §4.8: every failure message must give the user a next step,
    /// and "removed failed" without a path is not one.
    pub fn message(&self) -> String {
        match self {
            TrashError::Missing(p) => format!("要移除的文件不存在：{}", p.display()),
            TrashError::Create(p, why) => {
                format!("废纸篓目录创建失败（{}）：{}", p.display(), why)
            }
            TrashError::Move(p, why) => format!(
                "没能把 {} 移进废纸篓：{}。文件还在原来的位置，没有被删除。",
                p.display(),
                why
            ),
        }
    }
}

impl std::fmt::Display for TrashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for TrashError {}

/// Move `source` into the trash and return where it landed.
pub fn move_to_trash(source: &Path) -> Result<PathBuf, TrashError> {
    move_to_trash_under(source, &paths::trash_dir())
}

/// Same rules, caller-chosen trash root, and the stamp is taken from the clock.
pub fn move_to_trash_under(source: &Path, trash_root: &Path) -> Result<PathBuf, TrashError> {
    let stamp = timefmt::filename_stamp(timefmt::now_secs(), timefmt::local_offset_secs());
    move_to_trash_stamp(source, trash_root, &stamp)
}

/// The seam the tests use: same rules, **caller-chosen stamp**.
///
/// The stamp is not decoration — it is the collision domain. Two moves that
/// share it compete for the same file name, so a test that wants to prove the
/// loser gets a suffix has to be able to *say* they share it. Going through the
/// real clock would make that assertion depend on whether the previous
/// filesystem call happened to land in the same second, which is a test that
/// passes most of the time and is therefore worth nothing.
pub fn move_to_trash_stamp(
    source: &Path,
    trash_root: &Path,
    stamp: &str,
) -> Result<PathBuf, TrashError> {
    if !source.exists() {
        return Err(TrashError::Missing(source.to_path_buf()));
    }
    let dir = trash_root.join(stamp);
    fs::create_dir_all(&dir).map_err(|e| TrashError::Create(dir.clone(), e.to_string()))?;

    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("item");
    let mut target = dir.join(name);
    let mut suffix = 1usize;
    while target.exists() {
        target = dir.join(format!("{}-{}", name, suffix));
        suffix += 1;
    }

    match fs::rename(source, &target) {
        Ok(()) => return Ok(target),
        Err(rename_err) => {
            // Cross-device move (the config dir and a mounted drive are often
            // different volumes on Linux): copy, verify the bytes arrived, then
            // drop the original. `shutil.move` does the same dance.
            if copy_then_remove(source, &target).is_ok() {
                return Ok(target);
            }
            let _ = fs::remove_file(&target);
            Err(TrashError::Move(source.to_path_buf(), rename_err.to_string()))
        }
    }
}

/// Recursively copy `from` to `to`, then delete `from`.
fn copy_then_remove(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        copy_dir(from, to)?;
        fs::remove_dir_all(from)
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "macast-core-trash-{}-{}-{}",
                tag,
                std::process::id(),
                suffix_of(tag)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn suffix_of(tag: &str) -> u32 {
        tag.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 100000
    }

    fn file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_removal_is_a_move_not_a_delete() {
        let scratch = Scratch::new("move");
        let source = file(&scratch.0, "hooks.py", "print(1)");
        let landed = move_to_trash_under(&source, &scratch.0.join(".trash")).unwrap();

        assert!(!source.exists(), "the original must be gone");
        assert_eq!(landed.file_name().unwrap(), "hooks.py");
        assert_eq!(fs::read_to_string(&landed).unwrap(), "print(1)");
        // `<trash>/<stamp>/<name>`: recoverable *and* findable by name.
        assert_eq!(landed.parent().unwrap().parent().unwrap(), scratch.0.join(".trash"));
    }

    #[test]
    fn missing_files_are_refused_with_a_path() {
        let scratch = Scratch::new("missing");
        let gone = scratch.0.join("nope.py");
        let err = move_to_trash_under(&gone, &scratch.0.join(".trash")).unwrap_err();
        assert!(err.message().contains("不存在"));
        assert!(err.message().contains("nope.py"));
    }

    /// Two moves inside the same stamp second must not overwrite each other:
    /// the second one's bytes are somebody else's recoverable file.
    #[test]
    fn same_stamp_collisions_get_a_suffix() {
        let scratch = Scratch::new("collide");
        let trash = scratch.0.join(".trash");
        // Two *different* files that share a base name, which is the only shape
        // that can collide in the trash: same name, different directories. The
        // first draft of this test built both paths from `scratch.0` and so
        // handed `move_to_trash_under` the same file twice — the second call
        // then failed with `Missing`, which read like an implementation bug and
        // was really a fixture that described one file.
        let a = file(&scratch.0, "same.py", "first");
        let elsewhere = scratch.0.join("nested");
        fs::create_dir_all(&elsewhere).unwrap();
        let b = file(&elsewhere, "same.py", "second");

        let moved_a = move_to_trash_stamp(&a, &trash, "20261006-120000").unwrap();
        let moved_b = move_to_trash_stamp(&b, &trash, "20261006-120000").unwrap();
        assert_ne!(moved_a, moved_b, "one of the two files was overwritten");
        // The first keeps the name the loader looks for; the second takes the
        // suffix, and the suffix goes on the *end* so the stem still reads.
        assert_eq!(moved_a.file_name().unwrap(), "same.py");
        assert_eq!(moved_b.file_name().unwrap(), "same.py-1");
        assert_eq!(fs::read_to_string(&moved_a).unwrap(), "first");
        assert_eq!(fs::read_to_string(&moved_b).unwrap(), "second");
        // Neither source is left behind, and nothing else landed.
        assert!(!a.exists() && !b.exists());
        assert_eq!(fs::read_dir(moved_a.parent().unwrap()).unwrap().count(), 2);
    }

    #[test]
    fn directories_move_too() {
        let scratch = Scratch::new("dir");
        let tree = scratch.0.join("webrtc_extras");
        fs::create_dir_all(tree.join("aiortc")).unwrap();
        fs::write(tree.join("aiortc").join("rtc.py"), "x").unwrap();
        let landed = move_to_trash_under(&tree, &scratch.0.join(".trash")).unwrap();
        assert_eq!(fs::read_to_string(landed.join("aiortc/rtc.py")).unwrap(), "x");
        assert!(!tree.exists());
    }

    /// The clock-driven wrapper still has to be asked once: above, every move is
    /// given its stamp by hand, so nothing there proves `move_to_trash_under`
    /// computes one at all, let alone in the `%Y%m%d-%H%M%S` shape that
    /// [`crate::setting`] and the plugin uninstaller both write.
    #[test]
    fn the_real_clock_produces_a_stamp_shaped_directory() {
        let scratch = Scratch::new("stamp");
        let source = file(&scratch.0, "floating.py", "print(2)");
        let landed = move_to_trash_under(&source, &scratch.0.join(".trash")).unwrap();
        let stamp = landed.parent().unwrap().file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(stamp.len(), 15, "{stamp}");
        assert_eq!(&stamp[8..9], "-", "{stamp}");
        assert!(
            stamp
                .bytes()
                .enumerate()
                .all(|(i, b)| if i == 8 { b == b'-' } else { b.is_ascii_digit() }),
            "{stamp}"
        );
    }
}
