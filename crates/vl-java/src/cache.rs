//! The cache of probe results, keyed by path and validated by the executable's modification time
//! and size, so an unchanged Java is not started again.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use vl_core::java::JavaRuntime;

/// The on-disk format version; a file of another version is ignored.
const FORMAT: u32 = 1;

/// What identifies one state of an executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    /// Modification time, milliseconds since the Unix epoch.
    modified_ms: u128,
    /// Size in bytes.
    len: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(Stamp {
        modified_ms: modified.as_millis(),
        len: meta.len(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    stamp: Stamp,
    runtime: JavaRuntime,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: u32,
    entries: Vec<Entry>,
}

/// Probe results by executable path.
#[derive(Debug, Default)]
pub struct ProbeCache {
    entries: HashMap<PathBuf, Entry>,
}

impl ProbeCache {
    /// Reads a cache file; a missing, damaged or foreign file gives an empty cache.
    pub fn load(file: &Path) -> Self {
        let parsed = fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
            .filter(|f| f.format == FORMAT);
        let entries = parsed
            .map(|f| {
                f.entries
                    .into_iter()
                    .map(|e| (e.runtime.path.clone(), e))
                    .collect()
            })
            .unwrap_or_default();
        Self { entries }
    }

    /// Writes the cache atomically (a temporary file, then a rename).
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let mut entries: Vec<Entry> = self.entries.values().cloned().collect();
        entries.sort_by(|a, b| a.runtime.path.cmp(&b.runtime.path));
        let bytes = serde_json::to_vec_pretty(&File {
            format: FORMAT,
            entries,
        })?;
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, file)
    }

    /// The remembered result for `path`, if the executable is unchanged since.
    pub fn get(&self, path: &Path) -> Option<JavaRuntime> {
        let entry = self.entries.get(path)?;
        (stamp(path)? == entry.stamp).then(|| entry.runtime.clone())
    }

    /// Remembers a successful probe of `runtime.path`.
    pub fn insert(&mut self, runtime: &JavaRuntime) {
        if let Some(stamp) = stamp(&runtime.path) {
            self.entries.insert(
                runtime.path.clone(),
                Entry {
                    stamp,
                    runtime: runtime.clone(),
                },
            );
        }
    }

    /// Forgets every path for which `keep` is false.
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        self.entries.retain(|path, _| keep(path));
    }

    /// The number of remembered executables.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::testutil::TempDir;
    use vl_core::platform::Arch;

    fn runtime(path: &Path) -> JavaRuntime {
        JavaRuntime {
            path: path.to_path_buf(),
            major: 17,
            version: "17.0.2".into(),
            arch: Arch::X86_64,
            vendor: "Synthetic".into(),
            managed: false,
        }
    }

    #[test]
    fn hits_until_the_executable_changes() {
        let tmp = TempDir::new();
        let exe = tmp.path().join("java");
        fs::write(&exe, b"one").unwrap();
        let mut cache = ProbeCache::default();
        assert!(cache.get(&exe).is_none());
        cache.insert(&runtime(&exe));
        assert_eq!(cache.get(&exe).map(|r| r.major), Some(17));

        fs::write(&exe, b"a different binary").unwrap();
        assert!(cache.get(&exe).is_none());
        fs::remove_file(&exe).unwrap();
        assert!(cache.get(&exe).is_none());
    }

    #[test]
    fn survives_a_round_trip_through_a_file() {
        let tmp = TempDir::new();
        let exe = tmp.path().join("java");
        fs::write(&exe, b"x").unwrap();
        let file = tmp.path().join("sub/cache.json");
        let mut cache = ProbeCache::default();
        cache.insert(&runtime(&exe));
        cache.save(&file).unwrap();

        let loaded = ProbeCache::load(&file);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&exe).map(|r| r.vendor), Some("Synthetic".into()));
    }

    #[test]
    fn a_damaged_or_foreign_file_gives_an_empty_cache() {
        let tmp = TempDir::new();
        let file = tmp.path().join("cache.json");
        assert!(ProbeCache::load(&file).is_empty());
        fs::write(&file, b"{ not json").unwrap();
        assert!(ProbeCache::load(&file).is_empty());
        fs::write(&file, br#"{"format":99,"entries":[]}"#).unwrap();
        assert!(ProbeCache::load(&file).is_empty());
    }
}
