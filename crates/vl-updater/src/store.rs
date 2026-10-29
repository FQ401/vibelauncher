//! The update folder `<data>/updates/`: the newest verified manifest version, downloads waiting
//! to be installed with their record, the "just updated" marker, the record of a failed
//! installation and the update helper.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_update::{Artifact, Version};

use crate::UpdaterError;

/// The largest state or marker file read.
const MAX_STATE_BYTES: u64 = 16 * 1024;
const STATE_FILE: &str = "state.json";
const UPDATED_FILE: &str = "just-updated.json";
const STAGING_DIR: &str = "staging";
const STAGED_FILE: &str = "staged.json";
const FAILURE_FILE: &str = "last-error.json";
const HELPER_DIR: &str = "helper";
/// The longest failure message kept.
const MAX_MESSAGE_CHARS: usize = 1024;

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

/// A downloaded release waiting to be installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedUpdate {
    /// Its version.
    pub version: Version,
    /// The manifest entry it was downloaded from (size and SHA-256 are checked again).
    pub artifact: Artifact,
    /// The downloaded file.
    pub file: PathBuf,
}

/// An installation that failed and was undone; written by the helper, read by the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateFailure {
    /// The version that was being installed.
    pub version: Version,
    /// What went wrong.
    pub message: String,
    /// When, in seconds since the Unix epoch.
    pub at_unix: u64,
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

    /// Records the download of `artifact` of `version` at `file`.
    pub fn record_staged(
        &self,
        version: &Version,
        artifact: &Artifact,
        file: &Path,
    ) -> Result<(), UpdaterError> {
        let record = StagedUpdate {
            version: version.clone(),
            artifact: artifact.clone(),
            file: file.to_path_buf(),
        };
        write_json(&self.root.join(STAGED_FILE), &record)
    }

    /// The download waiting to be installed: its record names a file inside the staging folder
    /// of its version that still exists. Anything else reads as none.
    pub fn staged(&self) -> Option<StagedUpdate> {
        let record = read_json::<StagedUpdate>(&self.root.join(STAGED_FILE))?;
        let inside = record.file.parent() == Some(self.staging(&record.version).as_path());
        (inside && record.file.is_file()).then_some(record)
    }

    /// Removes every download and its record (best effort).
    pub fn clear_staged(&self) {
        let _ = fs::remove_file(self.root.join(STAGED_FILE));
        self.clear_staging(None);
    }

    /// Records a failed installation of `version`; the message is cut to 1024 characters.
    pub fn record_failure(&self, version: &Version, message: &str) -> Result<(), UpdaterError> {
        let at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let failure = UpdateFailure {
            version: version.clone(),
            message: message.chars().take(MAX_MESSAGE_CHARS).collect(),
            at_unix,
        };
        write_json(&self.root.join(FAILURE_FILE), &failure)
    }

    /// Reads and removes the record of a failed installation.
    pub fn take_failure(&self) -> Option<UpdateFailure> {
        let path = self.root.join(FAILURE_FILE);
        let failure = read_json::<UpdateFailure>(&path);
        let _ = fs::remove_file(&path);
        failure
    }

    /// The folder the update helper runs from.
    pub fn helper_dir(&self) -> PathBuf {
        self.root.join(HELPER_DIR)
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
    fn staged_record_needs_its_file_in_the_staging_folder() {
        let dir = Dir::new();
        let store = UpdateDir::new(dir.path());
        let artifact = vl_update::Artifact {
            platform: vl_core::platform::Platform::current(),
            kind: vl_update::ArtifactKind::Portable,
            url: "https://downloads.example.test/a.zip".into(),
            sha256: "ab".repeat(32),
            size: 1,
        };
        let file = store.staging(&v("1.1.0")).join("a.zip");
        store.record_staged(&v("1.1.0"), &artifact, &file).unwrap();
        assert_eq!(store.staged(), None, "the file is missing");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        assert_eq!(store.staged().unwrap().version, v("1.1.0"));
        // A record pointing outside the staging folder of its version is ignored.
        let outside = dir.path().join("a.zip");
        fs::write(&outside, b"x").unwrap();
        store
            .record_staged(&v("1.1.0"), &artifact, &outside)
            .unwrap();
        assert_eq!(store.staged(), None);
        store.record_staged(&v("1.1.0"), &artifact, &file).unwrap();
        store.clear_staged();
        assert_eq!(store.staged(), None);
        assert!(!file.exists());
    }

    #[test]
    fn failure_record_is_read_once_and_capped() {
        let dir = Dir::new();
        let store = UpdateDir::new(dir.path());
        assert_eq!(store.take_failure(), None);
        store
            .record_failure(&v("1.1.0"), &"x".repeat(5000))
            .unwrap();
        let failure = store.take_failure().unwrap();
        assert_eq!(failure.version, v("1.1.0"));
        assert_eq!(failure.message.len(), MAX_MESSAGE_CHARS);
        assert!(failure.at_unix > 0);
        assert_eq!(store.take_failure(), None);
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
