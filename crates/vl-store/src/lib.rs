//! Where the launcher keeps its files.
//!
//! - Data, cache and log directories by OS convention (Windows `%APPDATA%`, macOS
//!   `Application Support`, Linux XDG), or one portable directory (`--dir <path>` or a marker file
//!   next to the executable): see [`paths`].
//! - The on-disk format version and the migration chain, with a snapshot before each migration:
//!   see [`format`].
//! - A lock that keeps a second launcher process out of the same data directory: see [`lock`].
//! - Atomic file writes (temporary file, then rename) for every other native crate: see
//!   [`write_atomic`].
//!
//! [`Store::open`] ties them together. The directory layout is described in
//! `docs/architecture.md`.

mod atomic;
mod error;
pub mod format;
pub mod lock;
pub mod paths;

use std::path::{Path, PathBuf};

pub use atomic::{write_atomic, write_atomic_with};
pub use error::StoreError;
pub use format::{CURRENT_FORMAT, FORMAT_FILE, Migration, MigrationReport};
pub use lock::DirLock;
pub use paths::{Dirs, Env, PORTABLE_MARKER};

/// How to choose the directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreOptions {
    /// A portable directory (`--dir`); beats the marker file and the OS conventions.
    pub portable_dir: Option<PathBuf>,
}

/// An opened data directory: located, locked and migrated to the current format.
///
/// Dropping the store releases the lock.
#[derive(Debug)]
pub struct Store {
    dirs: Dirs,
    _lock: DirLock,
    migration: Option<MigrationReport>,
}

impl Store {
    /// Opens the store for this process: picks the directories from the process environment,
    /// creates them, takes the lock and migrates the data to [`CURRENT_FORMAT`].
    pub fn open(options: &StoreOptions) -> Result<Self, StoreError> {
        Self::open_with(&Env::from_process(), options)
    }

    /// [`Store::open`] with an explicit environment.
    pub fn open_with(env: &Env, options: &StoreOptions) -> Result<Self, StoreError> {
        let dirs = paths::resolve(env, options.portable_dir.as_deref())?;
        for dir in [&dirs.data, &dirs.cache, &dirs.logs] {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
        let lock = DirLock::acquire(&dirs.data)?;
        let migration = format::migrate(&dirs.data, format::MIGRATIONS, CURRENT_FORMAT)?;
        Ok(Self {
            dirs,
            _lock: lock,
            migration,
        })
    }

    /// All three directories.
    pub fn dirs(&self) -> &Dirs {
        &self.dirs
    }

    /// Settings, instances, shared game files, Java runtimes, metadata cache, accounts.
    pub fn data_dir(&self) -> &Path {
        &self.dirs.data
    }

    /// Files that can be deleted at any time (HTTP cache).
    pub fn cache_dir(&self) -> &Path {
        &self.dirs.cache
    }

    /// Launcher logs.
    pub fn logs_dir(&self) -> &Path {
        &self.dirs.logs
    }

    /// Whether everything lives under one portable directory.
    pub fn is_portable(&self) -> bool {
        self.dirs.portable
    }

    /// What the migration run while opening did, if any ran.
    pub fn migration(&self) -> Option<&MigrationReport> {
        self.migration.as_ref()
    }
}

/// Test helpers shared by the unit tests.
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh empty directory removed on drop.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "vl-store-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempDir;
    use super::*;
    use vl_core::platform::Os;

    fn env() -> Env {
        Env {
            os: Os::Linux,
            ..Env::default()
        }
    }

    fn portable(dir: PathBuf) -> StoreOptions {
        StoreOptions {
            portable_dir: Some(dir),
        }
    }

    #[test]
    fn portable_mode_writes_only_inside_its_directory() {
        let sandbox = TempDir::new();
        let root = sandbox.path().join("portable");
        let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
        assert!(store.is_portable());
        write_atomic(&store.data_dir().join("settings.toml"), b"x = 1\n").unwrap();
        for dir in [store.data_dir(), store.cache_dir(), store.logs_dir()] {
            assert!(dir.starts_with(&root), "{dir:?}");
        }
        drop(store);
        let siblings: Vec<_> = std::fs::read_dir(sandbox.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(siblings, ["portable"]);
    }

    #[test]
    fn a_second_store_on_the_same_directory_is_refused() {
        let sandbox = TempDir::new();
        let options = portable(sandbox.path().join("p"));
        let first = Store::open_with(&env(), &options).unwrap();
        let second = Store::open_with(&env(), &options).unwrap_err();
        assert!(matches!(second, StoreError::Locked { .. }), "{second}");
        drop(first);
        Store::open_with(&env(), &options).unwrap();
    }

    #[test]
    fn a_new_store_gets_the_current_format_without_a_snapshot() {
        let sandbox = TempDir::new();
        let store = Store::open_with(&env(), &portable(sandbox.path().join("p"))).unwrap();
        assert_eq!(
            format::read_version(store.data_dir()).unwrap(),
            Some(CURRENT_FORMAT)
        );
        assert!(store.migration().is_none());
        assert!(!store.data_dir().join(format::SNAPSHOT_DIR).exists());
    }
}
