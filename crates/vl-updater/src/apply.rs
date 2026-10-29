//! Carrying out an [`UpdatePlan`]: the game guard, the final hash check and the replacement.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// How long a rename is tried again while Windows reports the file as locked (an antivirus
/// scan, the last handles of the exiting launcher).
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

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
/// downloaded file again, then replaces the launcher. Portable folders, AppImages and (on macOS)
/// application bundles from a disk image are replaced here; the MSI replacement is not
/// implemented yet ([`UpdaterError::NotImplemented`]).
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
        Replacement::CopyFromDmg { image, bundle } => crate::dmg::replace_app_bundle(image, bundle),
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

/// Unpacks the portable `archive` over the portable folder `dir` and returns the replaced
/// top-level names.
///
/// A zip archive's root is the folder's content (Windows); a gzip-compressed tar archive holds
/// the folder as its one top-level folder (Linux). A portable marker file at the folder's root is
/// skipped.
/// Everything is unpacked into `dir/.vl-update-new` first (entry names checked, no links, entry
/// count and unpacked size capped); then each top-level item of the archive moves the old item
/// of that name to `dir/.vl-update-old` and takes its place. Items the archive does not have — the user's `data` folder among them —
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

/// Unpacks `archive` (a zip archive, or a gzip-compressed tar archive) into `into` and returns
/// its top-level names, sorted.
fn unpack(archive: &Path, into: &Path) -> Result<Vec<String>, UpdaterError> {
    let mut magic = [0u8; 2];
    let mut file = File::open(archive).map_err(|e| UpdaterError::io(archive, &e))?;
    let read = file
        .read(&mut magic)
        .map_err(|e| UpdaterError::io(archive, &e))?;
    drop(file);
    fs::create_dir_all(into).map_err(|e| UpdaterError::io(into, &e))?;
    let mut unpacker = Unpacker {
        into,
        budget: MAX_UNPACKED_BYTES,
        top: std::collections::BTreeSet::new(),
    };
    if read == 2 && magic == [0x1f, 0x8b] {
        unpack_tar_gz(archive, &mut unpacker)?;
    } else {
        unpack_zip(archive, &mut unpacker)?;
    }
    if unpacker.top.is_empty() {
        return Err(UpdaterError::Integrity("the archive is empty".into()));
    }
    Ok(unpacker.top.into_iter().collect())
}

/// The zip archive's root is the launcher folder's content.
fn unpack_zip(archive: &Path, unpacker: &mut Unpacker<'_>) -> Result<(), UpdaterError> {
    let bad = UpdaterError::Integrity;
    let file = File::open(archive).map_err(|e| UpdaterError::io(archive, &e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(bad(format!("more than {MAX_ARCHIVE_ENTRIES} entries")));
    }
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad(format!("entry {i}: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| bad(format!("entry {i}: {e}")))?
            .into_owned();
        let is_dir = entry.is_dir();
        let mode = entry.unix_mode();
        if mode.is_some_and(|m| m & 0o170_000 == 0o120_000) {
            return Err(bad(format!("{name:?} is a symbolic link")));
        }
        let rel = safe_path(name.trim_end_matches('/'))
            .ok_or_else(|| bad(format!("unsafe path {name:?}")))?;
        unpacker.entry(&name, rel, is_dir, mode, &mut entry)?;
    }
    Ok(())
}

/// The gzip-compressed tar archive holds the launcher folder as its one top-level folder (as the
/// Linux `.tar.gz` package does); that folder's content is unpacked. Only files and folders are
/// taken: links and special files make the archive invalid.
fn unpack_tar_gz(archive: &Path, unpacker: &mut Unpacker<'_>) -> Result<(), UpdaterError> {
    let bad = UpdaterError::Integrity;
    let one_folder = || bad("the tar archive must hold exactly one top-level folder".into());
    let file = File::open(archive).map_err(|e| UpdaterError::io(archive, &e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
    let entries = tar
        .entries()
        .map_err(|e| bad(format!("not a tar archive: {e}")))?;
    let mut root: Option<String> = None;
    for (i, entry) in entries.enumerate() {
        if i >= MAX_ARCHIVE_ENTRIES {
            return Err(bad(format!("more than {MAX_ARCHIVE_ENTRIES} entries")));
        }
        let mut entry = entry.map_err(|e| bad(format!("entry {i}: {e}")))?;
        let kind = entry.header().entry_type();
        let is_dir = match kind {
            tar::EntryType::Regular | tar::EntryType::Continuous => false,
            tar::EntryType::Directory => true,
            // Archive-wide pax metadata, not a file.
            tar::EntryType::XGlobalHeader => continue,
            _ => {
                return Err(bad(format!(
                    "entry {i} is not a file or a folder ({kind:?})"
                )));
            }
        };
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| bad(format!("entry {i}: the name is not UTF-8")))?;
        let trimmed = name
            .strip_prefix("./")
            .unwrap_or(&name)
            .trim_end_matches('/');
        let mut parts = safe_path(trimmed).ok_or_else(|| bad(format!("unsafe path {name:?}")))?;
        let first = parts.remove(0);
        match &root {
            None => root = Some(first),
            Some(r) if *r == first => {}
            Some(_) => return Err(one_folder()),
        }
        if parts.is_empty() {
            if !is_dir {
                return Err(one_folder());
            }
            continue;
        }
        let mode = entry.header().mode().ok();
        unpacker.entry(&name, parts, is_dir, mode, &mut entry)?;
    }
    Ok(())
}

/// Writes the entries of an archive below `into`, within the size budget, and collects the
/// top-level names.
struct Unpacker<'a> {
    into: &'a Path,
    budget: u64,
    top: std::collections::BTreeSet<String>,
}

impl Unpacker<'_> {
    /// One entry named `name` in the archive, at the checked path `rel` below the folder.
    fn entry(
        &mut self,
        name: &str,
        rel: Vec<String>,
        is_dir: bool,
        mode: Option<u32>,
        content: &mut dyn Read,
    ) -> Result<(), UpdaterError> {
        let bad = UpdaterError::Integrity;
        let first = rel.first().cloned().unwrap_or_default();
        // The release archive is the one users download, so it carries the portable marker as a
        // top-level file: it is skipped and the folder keeps its own (or none).
        if !is_dir && rel.len() == 1 && first.eq_ignore_ascii_case(vl_store::PORTABLE_MARKER) {
            return Ok(());
        }
        if PROTECTED.iter().any(|p| first.eq_ignore_ascii_case(p)) {
            return Err(bad(format!("the archive must not contain {first:?}")));
        }
        self.top.insert(first);
        let path = rel
            .iter()
            .fold(self.into.to_path_buf(), |p, part| p.join(part));
        if is_dir {
            return fs::create_dir_all(&path).map_err(|e| UpdaterError::io(&path, &e));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| UpdaterError::io(parent, &e))?;
        }
        let mut out = File::create(&path).map_err(|e| UpdaterError::io(&path, &e))?;
        let written = io::copy(&mut content.take(self.budget + 1), &mut out)
            .map_err(|e| bad(format!("{name}: {e}")))?;
        if written > self.budget {
            return Err(bad(format!(
                "the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }
        self.budget -= written;
        out.flush().map_err(|e| UpdaterError::io(&path, &e))?;
        set_mode(&path, mode)
    }
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
///
/// Renaming the old item away first is what makes this work on Windows: a running executable or
/// a loaded library can be renamed but not overwritten or deleted. A rename that finds the file
/// locked is tried again for up to [`LOCK_WAIT`].
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
            if let Err(e) = rename(&target, &old_dir.join(name)) {
                result = Err(UpdaterError::io(&target, &e));
                break;
            }
            step.1 = true;
        }
        let moved = rename(&new_dir.join(name), &target);
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
                let _ = rename(&target, &new_dir.join(name));
            }
            if had_old {
                let _ = rename(&old_dir.join(name), &target);
            }
        }
    }
    result
}

/// `fs::rename`, tried again for up to [`LOCK_WAIT`] while the file is locked.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let start = Instant::now();
    loop {
        match fs::rename(from, to) {
            Err(e) if is_locked(&e) && start.elapsed() < LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

/// Whether `e` is Windows reporting a file in use: access denied (5), sharing violation (32),
/// lock violation (33).
fn is_locked(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
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
        rename(&temp, target).map_err(|e| UpdaterError::io(target, &e))
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

    /// A file another process holds open without sharing (as an antivirus scan may) is moved
    /// once it is released.
    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_moved_once_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = Dir::new();
        let app = dir.path();
        let (new_dir, old_dir) = (app.join(NEW_DIR), app.join(OLD_DIR));
        fs::create_dir_all(&new_dir).unwrap();
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(app.join("a"), b"old a").unwrap();
        fs::write(new_dir.join("a"), b"new a").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(app.join("a"))
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            drop(held);
        });
        let start = Instant::now();
        swap_in(&["a".to_owned()], app, &new_dir, &old_dir).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(300));
        release.join().unwrap();
        assert_eq!(fs::read(app.join("a")).unwrap(), b"new a");
        assert_eq!(fs::read(old_dir.join("a")).unwrap(), b"old a");
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
