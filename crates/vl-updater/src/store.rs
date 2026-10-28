//! The update folder `<data>/updates/`: the newest verified manifest version, downloads waiting
//! to be installed, and the "just updated" marker.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_update::Version;

use crate::UpdaterError;

/// The largest state or marker file read.
const MAX_STATE_BYTES: u64 = 4 * 1024;
const STATE_FILE: &str = "state.json";
const UPDATED_FILE: &str = "just-updated.json";
const STAGING_DIR: &str = "staging";

/// The update folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateDir {
    root: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    highest_seen: Option<Version>,
}

/// Written after a replacement; read and removed by the next start of the launcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JustUpdated {
    /// The version before the update.
    pub from: Version,
    /// The version installed.
    pub to: Version,
}

impl UpdateDir {
    /// The update folder at `root` (normally `<data>/updates`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The newest manifest version verified on this machine. A missing or damaged state file
    /// reads as none; the running version is then the only rollback floor.
    pub fn highest_seen(&self) -> Option<Version> {
        read_json::<State>(&self.root.join(STATE_FILE)).and_then(|s| s.highest_seen)
    }

    /// Records a verified manifest version; the stored value only grows.
    pub fn record_seen(&self, version: &Version) -> Result<(), UpdaterError> {
        if self.highest_seen().is_some_and(|seen| seen >= *version) {
            return Ok(());
        }
        let state = State {
            highest_seen: Some(version.clone()),
        };
        write_json(&self.root.join(STATE_FILE), &state)
    }

    /// The folder that holds the download of `version`.
    pub fn staging(&self, version: &Version) -> PathBuf {
        self.root.join(STAGING_DIR).join(version.to_string())
    }

    /// Removes the downloads of every version but `keep` (best effort).
    pub fn clear_staging(&self, keep: Option<&Version>) {
        let keep = keep.map(|v| v.to_string());
        let Ok(entries) = fs::read_dir(self.root.join(STAGING_DIR)) else {
            return;
        };
        for entry in entries.flatten() {
            if keep.as_deref() != entry.file_name().to_str() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Writes the "just updated" marker.
    pub fn mark_updated(&self, from: &Version, to: &Version) -> Result<(), UpdaterError> {
        let marker = JustUpdated {
            from: from.clone(),
            to: to.clone(),
        };
        write_json(&self.root.join(UPDATED_FILE), &marker)
    }

    /// Reads and removes the "just updated" marker. A marker whose `to` is not the running
    /// version (the update did not take) is removed and not reported.
    pub fn take_just_updated(&self, running: &Version) -> Option<JustUpdated> {
        let path = self.root.join(UPDATED_FILE);
        let marker = read_json::<JustUpdated>(&path);
        let _ = fs::remove_file(&path);
        marker.filter(|m| m.to == *running)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_STATE_BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), UpdaterError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|e| UpdaterError::Io(format!("json: {e}")))?;
    vl_store::write_atomic(path, &bytes).map_err(|e| UpdaterError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Dir;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn highest_seen_only_grows() {
        let dir = Dir::new();
        let store = UpdateDir::new(dir.path().join("updates"));
        assert_eq!(store.highest_seen(), None);
        store.record_seen(&v("1.2.0")).unwrap();
        store.record_seen(&v("1.1.0")).unwrap();
        assert_eq!(store.highest_seen(), Some(v("1.2.0")));
        store.record_seen(&v("1.3.0")).unwrap();
        assert_eq!(store.highest_seen(), Some(v("1.3.0")));
        // A damaged file reads as nothing seen.
        fs::write(store.path().join(STATE_FILE), b"{oops").unwrap();
        assert_eq!(store.highest_seen(), None);
    }

    #[test]
    fn just_updated_marker() {
        let dir = Dir::new();
        let store = UpdateDir::new(dir.path());
        assert_eq!(store.take_just_updated(&v("1.0.0")), None);
        store.mark_updated(&v("1.0.0"), &v("1.1.0")).unwrap();
        assert_eq!(
            store.take_just_updated(&v("1.1.0")),
            Some(JustUpdated {
                from: v("1.0.0"),
                to: v("1.1.0")
            })
        );
        assert_eq!(store.take_just_updated(&v("1.1.0")), None);
        // An update that did not take is not reported, and the marker is gone.
        store.mark_updated(&v("1.0.0"), &v("1.1.0")).unwrap();
        assert_eq!(store.take_just_updated(&v("1.0.0")), None);
        assert!(!store.path().join(UPDATED_FILE).exists());
    }

    #[test]
    fn clear_staging_keeps_one_version() {
        let dir = Dir::new();
        let store = UpdateDir::new(dir.path());
        for s in ["1.0.0", "1.1.0"] {
            fs::create_dir_all(store.staging(&v(s))).unwrap();
            fs::write(store.staging(&v(s)).join("f"), b"x").unwrap();
        }
        store.clear_staging(Some(&v("1.1.0")));
        assert!(!store.staging(&v("1.0.0")).exists());
        assert!(store.staging(&v("1.1.0")).join("f").exists());
        store.clear_staging(None);
        assert!(!store.staging(&v("1.1.0")).exists());
    }
}
