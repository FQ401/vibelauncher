//! Carrying out an [`UpdatePlan`]: the game guard, the final hash check and the replacement.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use vl_update::{Replacement, UpdatePlan};
use zip::ZipArchive;

use crate::UpdaterError;

/// Folder inside a portable install that receives the new files before they are moved in.
pub const NEW_DIR: &str = ".vl-update-new";
/// Folder inside a portable install that keeps the replaced files until the next start.
pub const OLD_DIR: &str = ".vl-update-old";
/// The most entries a portable archive may hold.
pub const MAX_ARCHIVE_ENTRIES: usize = 10_000;
/// The most bytes a portable archive may unpack to.
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Names a portable archive may not hold at its top level: the data folder and portable marker
/// of the user (`vl_store`), the install marker, and the update's own folders.
const PROTECTED: &[&str] = &[
    "data",
    vl_store::PORTABLE_MARKER,
    vl_update::INSTALL_MARKER,
    NEW_DIR,
    OLD_DIR,
];

/// Tells whether a game is running. An update is never installed while one is: the caller shows
/// that the update waits and tries again after the game exits.
pub trait GameGuard {
    /// Whether any game started by the launcher is running.
    fn game_running(&self) -> bool;
}

impl<F: Fn() -> bool> GameGuard for F {
    fn game_running(&self) -> bool {
        self()
    }
}

/// Installs the update of `plan`: refuses while a game runs, checks the size and SHA-256 of the
/// downloaded file again, then replaces the launcher. Portable folders and AppImages are
/// replaced here; the MSI and disk image replacements are not implemented yet
/// ([`UpdaterError::NotImplemented`]).
pub fn apply(plan: &UpdatePlan, guard: &dyn GameGuard) -> Result<(), UpdaterError> {
    if plan.replacement == Replacement::Link {
        return Err(UpdaterError::NothingToReplace);
    }
    if guard.game_running() {
        return Err(UpdaterError::GameRunning);
    }
    verify_file(&plan.file, plan.size, &plan.sha256)?;
    match &plan.replacement {
        Replacement::Unpack { archive, dir } => replace_portable(archive, dir).map(|_| ()),
        Replacement::RenameOver { file, target } => replace_appimage(file, target),
        Replacement::RunInstaller { .. } => {
            Err(UpdaterError::NotImplemented("installing an MSI update"))
        }
        Replacement::CopyFromDmg { .. } => Err(UpdaterError::NotImplemented(
            "installing a macOS update from a disk image",
        )),
        Replacement::Link => Err(UpdaterError::NothingToReplace),
    }
}

/// Checks that `path` is exactly `size` bytes with SHA-256 `sha256` (lowercase hex).
pub fn verify_file(path: &Path, size: u64, sha256: &str) -> Result<(), UpdaterError> {
    let file = File::open(path).map_err(|e| UpdaterError::io(path, &e))?;
    let mut hasher = Sha256::new();
    let mut reader = file.take(size.saturating_add(1));
    let mut chunk = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| UpdaterError::io(path, &e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(chunk.get(..n).unwrap_or_default());
    }
    if total != size {
        return Err(UpdaterError::Integrity(format!(
            "expected {size} bytes, found {total}"
        )));
    }
    let actual = hasher.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(UpdaterError::Integrity(format!(
            "sha256 mismatch: expected {sha256}, got {actual}"
        )));
    }
    Ok(())
}

/// Unpacks the zip `archive` over the portable folder `dir` and returns the replaced top-level
/// names.
///
/// The archive's root is the folder's content. Everything is unpacked into `dir/.vl-update-new`
/// first (entry names checked, no links, entry count and unpacked size capped); then each
/// top-level item of the archive moves the old item of that name to `dir/.vl-update-old` and
/// takes its place. Items the archive does not have — the user's `data` folder among them —
/// are not touched. A failure while moving puts every moved item back.
pub fn replace_portable(archive: &Path, dir: &Path) -> Result<Vec<String>, UpdaterError> {
    let new_dir = dir.join(NEW_DIR);
    let old_dir = dir.join(OLD_DIR);
    remove_dir_if_exists(&new_dir)?;
    let result = unpack(archive, &new_dir).and_then(|names| {
        remove_dir_if_exists(&old_dir)?;
        fs::create_dir_all(&old_dir).map_err(|e| UpdaterError::io(&old_dir, &e))?;
        swap_in(&names, dir, &new_dir, &old_dir)?;
        Ok(names)
    });
    let _ = fs::remove_dir_all(&new_dir);
    result
}

/// Removes what a portable update left behind (`dir/.vl-update-old`, `dir/.vl-update-new`);
/// best effort, for the first start after an update.
pub fn clean_portable(dir: &Path) {
    let _ = fs::remove_dir_all(dir.join(OLD_DIR));
    let _ = fs::remove_dir_all(dir.join(NEW_DIR));
}

fn remove_dir_if_exists(path: &Path) -> Result<(), UpdaterError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UpdaterError::io(path, &e)),
    }
}

/// Unpacks `archive` into `into` and returns its top-level names, sorted.
fn unpack(archive: &Path, into: &Path) -> Result<Vec<String>, UpdaterError> {
    let bad = UpdaterError::Integrity;
    let file = File::open(archive).map_err(|e| UpdaterError::io(archive, &e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(bad(format!("more than {MAX_ARCHIVE_ENTRIES} entries")));
    }
    fs::create_dir_all(into).map_err(|e| UpdaterError::io(into, &e))?;
    let mut budget = MAX_UNPACKED_BYTES;
    let mut top = std::collections::BTreeSet::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad(format!("entry {i}: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| bad(format!("entry {i}: {e}")))?
            .into_owned();
        let is_dir = entry.is_dir();
        let rel = safe_path(name.trim_end_matches('/'))
            .ok_or_else(|| bad(format!("unsafe path {name:?}")))?;
        let first = rel.first().cloned().unwrap_or_default();
        if PROTECTED.iter().any(|p| first.eq_ignore_ascii_case(p)) {
            return Err(bad(format!("the archive must not contain {first:?}")));
        }
        let mode = entry.unix_mode();
        if mode.is_some_and(|m| m & 0o170_000 == 0o120_000) {
            return Err(bad(format!("{name:?} is a symbolic link")));
        }
        top.insert(first);
        let path = rel.iter().fold(into.to_path_buf(), |p, part| p.join(part));
        if is_dir {
            fs::create_dir_all(&path).map_err(|e| UpdaterError::io(&path, &e))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| UpdaterError::io(parent, &e))?;
        }
        let mut out = File::create(&path).map_err(|e| UpdaterError::io(&path, &e))?;
        let written = io::copy(&mut (&mut entry).take(budget + 1), &mut out)
            .map_err(|e| bad(format!("{name}: {e}")))?;
        if written > budget {
            return Err(bad(format!(
                "the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }
        budget -= written;
        out.flush().map_err(|e| UpdaterError::io(&path, &e))?;
        set_mode(&path, mode)?;
    }
    if top.is_empty() {
        return Err(bad("the archive is empty".into()));
    }
    Ok(top.into_iter().collect())
}

/// The parts of a relative archive path, `None` when it could leave its folder or is not a
/// portable file name.
fn safe_path(name: &str) -> Option<Vec<String>> {
    if name.is_empty() || name.starts_with('/') {
        return None;
    }
    name.split('/')
        .map(|part| {
            let ok = !part.is_empty()
                && part != "."
                && part != ".."
                && !part
                    .chars()
                    .any(|c| c.is_control() || "\\:*?\"<>|".contains(c));
            ok.then(|| part.to_owned())
        })
        .collect()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> Result<(), UpdaterError> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o755))
            .map_err(|e| UpdaterError::io(path, &e))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) -> Result<(), UpdaterError> {
    Ok(())
}

/// Moves each `names` item from `new_dir` into `dir`, the old one first into `old_dir`. On a
/// failure everything moved so far is put back.
fn swap_in(
    names: &[String],
    dir: &Path,
    new_dir: &Path,
    old_dir: &Path,
) -> Result<(), UpdaterError> {
    // (name, whether an old item was moved away, whether the new item was moved in)
    let mut done: Vec<(&str, bool, bool)> = Vec::new();
    let mut result = Ok(());
    for name in names {
        let target = dir.join(name);
        let mut step = (name.as_str(), false, false);
        if fs::symlink_metadata(&target).is_ok() {
            if let Err(e) = fs::rename(&target, old_dir.join(name)) {
                result = Err(UpdaterError::io(&target, &e));
                break;
            }
            step.1 = true;
        }
        let moved = fs::rename(new_dir.join(name), &target);
        step.2 = moved.is_ok();
        done.push(step);
        if let Err(e) = moved {
            result = Err(UpdaterError::io(&target, &e));
            break;
        }
    }
    if result.is_err() {
        for (name, had_old, moved_in) in done.into_iter().rev() {
            let target = dir.join(name);
            if moved_in {
                let _ = fs::rename(&target, new_dir.join(name));
            }
            if had_old {
                let _ = fs::rename(old_dir.join(name), &target);
            }
        }
    }
    result
}

/// Replaces the AppImage `target` with `file`: copies it next to the target, makes it
/// executable, flushes it and renames it over the target in one step. The running launcher
/// keeps its open copy of the old file.
pub fn replace_appimage(file: &Path, target: &Path) -> Result<(), UpdaterError> {
    let name = target
        .file_name()
        .ok_or_else(|| UpdaterError::Io(format!("{} is not a file", target.display())))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(".vl-new");
    let temp: PathBuf = target.with_file_name(temp_name);
    let result = (|| {
        fs::copy(file, &temp).map_err(|e| UpdaterError::io(&temp, &e))?;
        set_mode(&temp, Some(0o755))?;
        // Flushing needs write access on Windows.
        fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .and_then(|f| f.sync_all())
            .map_err(|e| UpdaterError::io(&temp, &e))?;
        fs::rename(&temp, target).map_err(|e| UpdaterError::io(target, &e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Dir;

    #[test]
    fn a_failed_swap_puts_everything_back() {
        let dir = Dir::new();
        let app = dir.path();
        let (new_dir, old_dir) = (app.join(NEW_DIR), app.join(OLD_DIR));
        fs::create_dir_all(&new_dir).unwrap();
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(app.join("a"), b"old a").unwrap();
        fs::write(app.join("b"), b"old b").unwrap();
        fs::write(new_dir.join("a"), b"new a").unwrap();
        // "b" is missing from the new files, so moving it in fails after "a" moved.
        let names = ["a".to_owned(), "b".to_owned()];
        assert!(swap_in(&names, app, &new_dir, &old_dir).is_err());
        assert_eq!(fs::read(app.join("a")).unwrap(), b"old a");
        assert_eq!(fs::read(app.join("b")).unwrap(), b"old b");
        assert_eq!(fs::read(new_dir.join("a")).unwrap(), b"new a");
        assert_eq!(fs::read_dir(&old_dir).unwrap().count(), 0);
    }

    #[test]
    fn safe_paths() {
        assert_eq!(
            safe_path("lib/x.dll"),
            Some(vec!["lib".to_owned(), "x.dll".to_owned()])
        );
        for bad in [
            "", "/abs", "a//b", "./a", "a/../b", "C:/x", "a\\b", "a\u{0}b",
        ] {
            assert_eq!(safe_path(bad), None, "{bad:?}");
        }
    }
}
