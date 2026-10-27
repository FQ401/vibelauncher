//! Atomic file writes: a reader sees the old file or the new one, never half of it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::StoreError;

/// Replaces the file at `path` with `bytes` atomically.
///
/// Writes a temporary file next to `path`, flushes it to disk and renames it over `path`. Missing
/// parent directories are created. On failure the temporary file is removed and the old file is
/// untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    write_atomic_with(path, |w| w.write_all(bytes))
}

/// [`write_atomic`] for content produced by a writer (serializers, streams).
pub fn write_atomic_with(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), StoreError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).map_err(|e| StoreError::io(parent, e))?;
    let tmp = temp_path(parent, path);
    let result = write_and_rename(&tmp, path, parent, write);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| StoreError::io(path, e))
}

fn write_and_rename(
    tmp: &Path,
    path: &Path,
    parent: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut file = File::create_new(tmp)?;
    write(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)?;
    sync_dir(parent);
    Ok(())
}

/// Makes the rename durable where the OS allows opening a directory.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn temp_path(parent: &Path, target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn creates_parents_and_replaces_content() {
        let dir = TempDir::new();
        let file = dir.path().join("a").join("b").join("f.toml");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        assert_eq!(names(file.parent().unwrap()), ["f.toml"]);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file_and_leaves_no_temporary() {
        let dir = TempDir::new();
        let file = dir.path().join("f");
        write_atomic(&file, b"old").unwrap();
        let err = write_atomic_with(&file, |w| {
            w.write_all(b"partial")?;
            Err(io::Error::other("boom"))
        })
        .unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }));
        assert_eq!(fs::read(&file).unwrap(), b"old");
        assert_eq!(names(dir.path()), ["f"]);
    }
}
