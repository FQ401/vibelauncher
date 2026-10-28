//! The storage layout: where each section lives.
//!
//! `storage.json` in the data folder records the sections with an explicit folder and the old
//! copies left by moves. Without the file every section is at its default place:
//!
//! ```text
//! <root>/instances/                                    instances
//! <root>/versions, libraries, assets, loaders          shared game files
//! <root>/java/                                         managed Java runtimes
//! <root>/cache/                                        cache
//! <root>/snapshots/                                    backups and snapshots
//! <root>/logs/                                         launcher logs
//! ```
//!
//! An explicit folder for the shared game files holds the same four directories.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::storage::{RootSource, Section};

use crate::paths::is_within;
use crate::{StoreError, write_atomic};

/// The layout file in the data folder.
pub const STORAGE_FILE: &str = "storage.json";
/// The directories of the shared game files section.
pub const GAME_ENTRIES: [&str; 4] = ["versions", "libraries", "assets", "loaders"];

/// An old copy left by a move: the named entries inside `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftoverRecord {
    /// The section it was a copy of.
    pub section: Section,
    /// The folder the entries are in.
    pub path: PathBuf,
    /// The top-level names that belong to the copy; nothing else in `path` is touched.
    pub entries: Vec<String>,
}

/// The content of [`STORAGE_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Sections with an explicit folder.
    #[serde(default)]
    pub sections: BTreeMap<Section, PathBuf>,
    /// Old copies waiting for a cleanup.
    #[serde(default)]
    pub leftovers: Vec<LeftoverRecord>,
    /// Fields of a newer launcher, kept as they are.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl StorageConfig {
    /// Reads the layout of `root`; defaults when there is no file. A damaged file is an error:
    /// default paths would hide the user's moved data.
    pub fn load(root: &Path) -> Result<Self, StoreError> {
        let path = root.join(STORAGE_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(StoreError::io(&path, e)),
        };
        let mut config: Self = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::corrupt(&path, e.to_string()))?;
        // The data folder is chosen by the pointer, never by this file.
        config.sections.remove(&Section::Data);
        if let Some((section, dir)) = config.sections.iter().find(|(_, p)| !p.is_absolute()) {
            return Err(StoreError::corrupt(
                &path,
                format!("{section} has a relative folder {}", dir.display()),
            ));
        }
        Ok(config)
    }

    /// Writes the layout into `root` atomically.
    pub fn save(&self, root: &Path) -> Result<(), StoreError> {
        let path = root.join(STORAGE_FILE);
        let text = serde_json::to_vec_pretty(self)
            .map_err(|e| StoreError::corrupt(&path, e.to_string()))?;
        write_atomic(&path, &text)
    }
}

/// The data folder and the folder of every section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub(crate) root: PathBuf,
    pub(crate) source: RootSource,
    pub(crate) config: StorageConfig,
}

impl Layout {
    /// The layout of `root` with `config`.
    pub fn new(root: PathBuf, source: RootSource, config: StorageConfig) -> Self {
        Self {
            root,
            source,
            config,
        }
    }

    /// The data folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What chose the data folder.
    pub fn source(&self) -> RootSource {
        self.source
    }

    /// The recorded layout.
    pub fn config(&self) -> &StorageConfig {
        &self.config
    }

    /// Where `section` lives when it has no explicit folder.
    pub fn default_path(&self, section: Section) -> PathBuf {
        default_path(&self.root, section)
    }

    /// The folder of `section`.
    pub fn path(&self, section: Section) -> PathBuf {
        self.config
            .sections
            .get(&section)
            .cloned()
            .unwrap_or_else(|| self.default_path(section))
    }

    /// Whether `section` is at its default place.
    pub fn is_default(&self, section: Section) -> bool {
        section == Section::Data || !self.config.sections.contains_key(&section)
    }

    /// The paths that belong to `section`: its folder, or for the shared game files the four
    /// directories in it. The data folder owns its root.
    pub fn owned(&self, section: Section) -> Vec<PathBuf> {
        owned(&self.path(section), section)
    }

    /// Whether removing `path` would touch data a section uses now.
    pub fn is_live(&self, path: &Path) -> bool {
        if is_within(&self.root, path) {
            return true;
        }
        Section::ALL
            .into_iter()
            .filter(|s| *s != Section::Data)
            .flat_map(|s| self.owned(s))
            .any(|owned| is_within(&owned, path) || is_within(path, &owned))
    }
}

/// Where `section` lives inside `root` by default.
pub fn default_path(root: &Path, section: Section) -> PathBuf {
    match section {
        Section::Data | Section::Game => root.to_path_buf(),
        Section::Instances => root.join("instances"),
        Section::Java => root.join("java"),
        Section::Cache => root.join("cache"),
        Section::Backups => root.join(crate::format::SNAPSHOT_DIR),
        Section::Logs => root.join("logs"),
    }
}

/// The paths of `section` when its folder is `path`.
pub fn owned(path: &Path, section: Section) -> Vec<PathBuf> {
    match section {
        Section::Game => GAME_ENTRIES.iter().map(|e| path.join(e)).collect(),
        _ => vec![path.to_path_buf()],
    }
}
