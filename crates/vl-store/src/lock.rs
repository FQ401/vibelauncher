//! The lock that keeps two launcher processes out of one data directory.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use crate::StoreError;

/// Name of the lock file inside the data directory.
pub const LOCK_FILE: &str = ".lock";

/// An exclusive advisory lock on a data directory, held until dropped.
///
/// The OS releases the lock when the process exits, even after a crash, so a stale lock file
/// never blocks the next start.
#[derive(Debug)]
pub struct DirLock {
    _file: File,
}

impl DirLock {
    /// Takes the lock on `dir` (which must exist) or fails with [`StoreError::Locked`].
    pub fn acquire(dir: &Path) -> Result<Self, StoreError> {
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(StoreError::Locked {
                dir: dir.to_path_buf(),
            }),
            Err(TryLockError::Error(e)) => Err(StoreError::io(&path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn excludes_a_second_holder_until_released() {
        let dir = TempDir::new();
        let first = DirLock::acquire(dir.path()).unwrap();
        assert!(matches!(
            DirLock::acquire(dir.path()),
            Err(StoreError::Locked { .. })
        ));
        drop(first);
        DirLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn a_leftover_lock_file_does_not_block() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join(LOCK_FILE), b"").unwrap();
        DirLock::acquire(dir.path()).unwrap();
    }
}
