//! Where the shared game files live below the data directory.
//!
//! ```text
//! <data>/libraries/<maven path>                 shared libraries
//! <data>/assets/indexes/<id>.json               asset indexes
//! <data>/assets/objects/<2>/<hash>              asset objects
//! <data>/assets/virtual/<id>/<name>             legacy `virtual` layout
//! <data>/assets/log_configs/<file>              logging configurations
//! <data>/versions/<id>/<id>.json                version JSON
//! <data>/versions/<id>/<id>.jar                 client jar
//! <data>/versions/<id>/natives/                 unpacked natives
//! ```

use std::path::{Path, PathBuf};

use vl_core::{Error, Result};

/// The shared game files directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout below `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: data_dir.into(),
        }
    }

    /// The data directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `libraries/`.
    pub fn libraries_dir(&self) -> PathBuf {
        self.root.join("libraries")
    }

    /// `assets/`.
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("assets")
    }

    /// `assets/indexes/<id>.json`.
    pub fn asset_index_file(&self, index_id: &str) -> PathBuf {
        self.assets_dir()
            .join("indexes")
            .join(format!("{index_id}.json"))
    }

    /// `assets/log_configs/`.
    pub fn log_configs_dir(&self) -> PathBuf {
        self.assets_dir().join("log_configs")
    }

    /// `versions/`.
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// `versions/<id>/`.
    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    /// `versions/<id>/<id>.json`.
    pub fn version_json(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }

    /// `versions/<id>/<id>.jar`.
    pub fn client_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }

    /// `versions/<id>/natives/`.
    pub fn natives_dir(&self, id: &str) -> PathBuf {
        self.version_dir(id).join("natives")
    }
}

/// Whether `id` is one safe path component (a version id from upstream is hostile input).
pub fn is_safe_component(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.len() <= 128
        && !id.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        && !id.chars().any(char::is_control)
        && !id.ends_with('.')
        && !id.ends_with(' ')
}

/// `id` itself when it is a safe component.
///
/// # Errors
///
/// `InvalidInput` otherwise.
pub fn check_component<'a>(what: &str, id: &'a str) -> Result<&'a str> {
    if is_safe_component(id) {
        Ok(id)
    } else {
        Err(Error::invalid_input(format!("unsafe {what}: {id:?}")))
    }
}

/// Joins `relative` (`/`-separated, as `vl-meta` produces) below `base`.
pub(crate) fn join_relative(base: &Path, relative: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
    }
    path
}
