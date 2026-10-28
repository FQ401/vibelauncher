//! Where the launcher keeps its files.
//!
//! - The data folder (the root): `--dir`, the `VIBELAUNCHER_DIR` variable, the portable marker
//!   next to the executable, the pointer file in the OS folder for settings, or the OS convention:
//!   see [`paths`] and [`pointer`].
//! - The sections with folders of their own (instances, shared game files, Java, cache, backups,
//!   logs), by default inside the root: see [`layout`].
//! - Checks of a folder before data goes there: see [`check`].
//! - Moving a section by copying with rollback, and removing old copies: see [`mover`].
//! - Size limits of logs and the cache: see [`retention`].
//! - Suggesting a data folder off the system drive: see [`suggest`].
//! - The on-disk format version and the migration chain, with a snapshot before each migration:
//!   see [`format`].
//! - A lock that keeps a second launcher process out of the same data folder: see [`lock`].
//! - Atomic file writes (temporary file, then rename) for every other native crate: see
//!   [`write_atomic`].
//!
//! [`Store::open`] ties them together. The layout is described in `docs/architecture.md`.

mod atomic;
pub mod check;
mod error;
pub mod format;
pub mod layout;
pub mod lock;
pub mod mover;
pub mod paths;
pub mod pointer;
pub mod retention;
pub mod suggest;

use std::path::{Path, PathBuf};

use vl_core::storage::{
    CleanupReport, Leftover, MoveRequest, PathCheck, RootSource, Section, SectionInfo, StorageInfo,
    StorageSuggestion,
};

pub use atomic::{write_atomic, write_atomic_with};
pub use error::StoreError;
pub use format::{CURRENT_FORMAT, FORMAT_FILE, Migration, MigrationReport};
pub use layout::{Layout, StorageConfig};
pub use lock::DirLock;
pub use mover::{MovePlan, MoveProgress, MoveReport};
pub use paths::{DIR_VAR, Env, PORTABLE_MARKER};

/// How to choose the data folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreOptions {
    /// The data folder from `--dir`; beats every other source.
    pub portable_dir: Option<PathBuf>,
}

/// An opened data folder: located, locked and migrated to the current format, with the folder of
/// every section.
///
/// Dropping the store releases the lock.
#[derive(Debug)]
pub struct Store {
    env: Env,
    layout: Layout,
    unavailable: Vec<Section>,
    _lock: DirLock,
    migration: Option<MigrationReport>,
}

impl Store {
    /// Opens the store for this process: picks the data folder from the process environment,
    /// creates it, takes the lock, migrates the data to [`CURRENT_FORMAT`] and
    /// prunes logs and the cache to their limits.
    pub fn open(options: &StoreOptions) -> Result<Self, StoreError> {
        Self::open_with(&Env::from_process(), options)
    }

    /// [`Store::open`] with an explicit environment.
    ///
    /// A section with an explicit folder that does not exist is not created again (its drive may
    /// be disconnected); it is listed by [`Store::unavailable`].
    pub fn open_with(env: &Env, options: &StoreOptions) -> Result<Self, StoreError> {
        let root = paths::resolve(env, options.portable_dir.as_deref())?;
        std::fs::create_dir_all(&root.dir).map_err(|e| StoreError::io(&root.dir, e))?;
        let dir = paths::normalize(&root.dir)?;
        let lock = DirLock::acquire(&dir)?;
        let config = StorageConfig::load(&dir)?;
        let layout = Layout::new(dir, root.source, config);
        let unavailable: Vec<Section> = Section::ALL
            .into_iter()
            .filter(|s| !layout.is_default(*s) && !layout.path(*s).is_dir())
            .collect();
        let snapshots = if unavailable.contains(&Section::Backups) {
            layout.default_path(Section::Backups)
        } else {
            layout.path(Section::Backups)
        };
        let migration = format::migrate_with_snapshots(
            layout.root(),
            format::MIGRATIONS,
            CURRENT_FORMAT,
            &snapshots,
        )?;
        let store = Self {
            env: env.clone(),
            layout,
            unavailable,
            _lock: lock,
            migration,
        };
        store.prune(&retention::Policy::default());
        Ok(store)
    }

    /// Prunes launcher logs and the cache to `policy`; the settings are read after the store is
    /// open, so the backend calls this again with the configured limits.
    pub fn prune(&self, policy: &retention::Policy) {
        self.view().prune(policy);
    }

    /// The layout: the data folder and the folder of every section.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The data folder: settings, accounts, the format version, the layout.
    pub fn data_dir(&self) -> &Path {
        self.layout.root()
    }

    /// The folder of `section`.
    pub fn dir(&self, section: Section) -> PathBuf {
        self.layout.path(section)
    }

    /// Instances.
    pub fn instances_dir(&self) -> PathBuf {
        self.dir(Section::Instances)
    }

    /// Shared game files: the folder with `versions/`, `libraries/`, `assets/` and `loaders/`.
    pub fn game_dir(&self) -> PathBuf {
        self.dir(Section::Game)
    }

    /// Managed Java runtimes.
    pub fn java_dir(&self) -> PathBuf {
        self.dir(Section::Java)
    }

    /// Files that can be deleted at any time (HTTP cache).
    pub fn cache_dir(&self) -> PathBuf {
        self.dir(Section::Cache)
    }

    /// Backups and snapshots.
    pub fn backups_dir(&self) -> PathBuf {
        self.dir(Section::Backups)
    }

    /// Launcher logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.dir(Section::Logs)
    }

    /// What chose the data folder.
    pub fn source(&self) -> RootSource {
        self.layout.source()
    }

    /// Whether the data folder is fixed from outside (`--dir`, the variable or the marker).
    pub fn is_portable(&self) -> bool {
        self.source().is_fixed()
    }

    /// Sections whose explicit folder does not exist.
    pub fn unavailable(&self) -> &[Section] {
        &self.unavailable
    }

    /// What the migration run while opening did, if any ran.
    pub fn migration(&self) -> Option<&MigrationReport> {
        self.migration.as_ref()
    }

    /// A detached copy of the layout for work on other threads (it does not hold the lock).
    pub fn view(&self) -> StoreView {
        StoreView {
            env: self.env.clone(),
            layout: self.layout.clone(),
            unavailable: self.unavailable.clone(),
        }
    }

    /// The layout for front ends; with `sizes` every folder and old copy is measured.
    pub fn info(&self, sizes: bool) -> StorageInfo {
        self.view().info(sizes)
    }

    /// Checks `path` as a new folder for `section`.
    pub fn check(&self, section: Section, path: &Path) -> PathCheck {
        self.view().check(section, path)
    }

    /// Checks a move and measures what it copies; [`mover::execute`] runs it.
    pub fn plan_move(&self, request: &MoveRequest) -> Result<(MovePlan, PathCheck), StoreError> {
        self.view().plan_move(request)
    }

    /// Removes old copies left by moves and prunes logs and the cache to `policy`.
    pub fn cleanup(&self, policy: &retention::Policy) -> Result<CleanupReport, StoreError> {
        self.view().cleanup(policy)
    }

    /// Suggests a data folder; see [`suggest::suggest`].
    pub fn suggest(&self) -> StorageSuggestion {
        self.view().suggest(&suggest::SystemDisks)
    }
}

/// The layout of an opened store, detached from its lock: cheap to clone and to send to a worker
/// thread.
#[derive(Debug, Clone)]
pub struct StoreView {
    env: Env,
    layout: Layout,
    unavailable: Vec<Section>,
}

impl StoreView {
    fn is_portable(&self) -> bool {
        self.layout.source().is_fixed()
    }

    /// The layout for front ends; with `sizes` every folder and old copy is measured.
    pub fn info(&self, sizes: bool) -> StorageInfo {
        let measure =
            |paths: &[PathBuf]| sizes.then(|| paths.iter().map(|p| retention::size_of(p)).sum());
        let sections = Section::ALL
            .into_iter()
            .map(|section| {
                let path = self.layout.path(section);
                let size = if section == Section::Data {
                    // The data folder without the default sections inside it.
                    sizes.then(|| {
                        let inner: u64 = Section::ALL
                            .into_iter()
                            .filter(|s| *s != Section::Data && self.layout.is_default(*s))
                            .flat_map(|s| self.layout.owned(s))
                            .map(|p| retention::size_of(&p))
                            .sum();
                        retention::size_of(&path).saturating_sub(inner)
                    })
                } else {
                    measure(&self.layout.owned(section))
                };
                SectionInfo {
                    section,
                    default: self.layout.is_default(section),
                    available: !self.unavailable.contains(&section),
                    path,
                    size,
                }
            })
            .collect();
        // Old copies as recorded now: a cleanup since opening changed them.
        let recorded = StorageConfig::load(self.layout.root())
            .map(|c| c.leftovers)
            .unwrap_or_else(|_| self.layout.config().leftovers.clone());
        let mut leftovers: Vec<Leftover> = recorded
            .iter()
            .map(|l| Leftover {
                section: l.section,
                path: l.path.clone(),
                size: measure(&l.entries.iter().map(|e| l.path.join(e)).collect::<Vec<_>>()),
            })
            .collect();
        if !self.is_portable() {
            for (section, path) in paths::legacy_dirs(&self.env) {
                if path.exists() && !self.layout.is_live(&path) {
                    leftovers.push(Leftover {
                        section,
                        size: measure(std::slice::from_ref(&path)),
                        path,
                    });
                }
            }
        }
        StorageInfo {
            root: self.layout.root().to_path_buf(),
            source: self.layout.source(),
            sections,
            leftovers,
            restart_required: false,
        }
    }

    /// Checks `path` as a new folder for `section`.
    pub fn check(&self, section: Section, path: &Path) -> PathCheck {
        check::check_path(&self.env, &self.layout, section, path, None, None)
    }

    /// Checks a move and measures what it copies; [`mover::execute`] runs it.
    pub fn plan_move(&self, request: &MoveRequest) -> Result<(MovePlan, PathCheck), StoreError> {
        mover::plan(&self.env, &self.layout, &self.unavailable, request)
    }

    /// Removes old copies left by moves and prunes logs and the cache to `policy`.
    pub fn cleanup(&self, policy: &retention::Policy) -> Result<CleanupReport, StoreError> {
        mover::cleanup(&self.env, &self.layout, policy)
    }

    /// Prunes launcher logs and the cache to `policy`.
    pub fn prune(&self, policy: &retention::Policy) {
        for (section, limits) in [(Section::Logs, policy.logs), (Section::Cache, policy.cache)] {
            if !self.unavailable.contains(&section) {
                retention::prune(&self.layout.path(section), limits);
            }
        }
    }

    /// Suggests a data folder from the drives of `disks`; the data folder in use is compared with
    /// the system drive.
    pub fn suggest(&self, disks: &dyn suggest::DiskSource) -> StorageSuggestion {
        suggest::suggest(&self.env, disks, self.layout.root())
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
mod tests;
