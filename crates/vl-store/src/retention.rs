//! Size limits for the folders that grow by themselves: launcher logs and the cache.
//!
//! The newest files are kept until a limit is reached; older ones are removed. Removing any cache
//! file is safe: a missing file is a cache miss.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vl_core::settings::AdvancedSettings;

/// How much a folder may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total bytes.
    pub max_bytes: u64,
    /// Number of files.
    pub max_files: usize,
}

/// The limits of the launcher logs and of the cache.
///
/// The default is 30 files and 64 MiB of logs and 2 GiB of cache, which are also the defaults of
/// the advanced settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Launcher logs.
    pub logs: Limits,
    /// The cache.
    pub cache: Limits,
}

impl From<&AdvancedSettings> for Policy {
    fn from(a: &AdvancedSettings) -> Self {
        Self {
            logs: Limits {
                max_bytes: u64::from(a.log_max_mib) << 20,
                max_files: a.log_max_files as usize,
            },
            cache: Limits {
                max_bytes: u64::from(a.cache_max_mib) << 20,
                max_files: usize::MAX,
            },
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::from(&AdvancedSettings::default())
    }
}

/// What a prune removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Files removed.
    pub files: usize,
    /// Bytes freed.
    pub bytes: u64,
}

/// Removes the oldest files below `dir` (by modification time) until it fits `limits`. Files
/// that cannot be removed are skipped; a missing folder is empty.
pub fn prune(dir: &Path, limits: Limits) -> Pruned {
    let mut files = Vec::new();
    collect(dir, &mut files);
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    let mut kept_bytes = 0u64;
    let mut pruned = Pruned::default();
    for (n, (path, _, size)) in files.into_iter().enumerate() {
        if n < limits.max_files && kept_bytes.saturating_add(size) <= limits.max_bytes {
            kept_bytes += size;
        } else if fs::remove_file(&path).is_ok() {
            pruned.files += 1;
            pruned.bytes += size;
        }
    }
    pruned
}

fn collect(dir: &Path, out: &mut Vec<(PathBuf, SystemTime, u64)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&entry.path(), out);
        } else if kind.is_file()
            && let Ok(meta) = entry.metadata()
        {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((entry.path(), modified, meta.len()));
        }
    }
}

/// Bytes used below `path` (links are not followed); 0 for a missing path.
pub fn size_of(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::time::Duration;

    fn file(path: &Path, size: usize, age_secs: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; size]).unwrap();
        let time = SystemTime::now() - Duration::from_secs(age_secs);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn the_policy_follows_the_settings_and_defaults_to_the_former_constants() {
        let p = Policy::default();
        assert_eq!((p.logs.max_files, p.logs.max_bytes), (30, 64 << 20));
        assert_eq!(p.cache.max_bytes, 2 << 30);
        let a = AdvancedSettings {
            log_max_files: 5,
            log_max_mib: 1,
            cache_max_mib: 100,
            ..AdvancedSettings::default()
        };
        let p = Policy::from(&a);
        assert_eq!((p.logs.max_files, p.logs.max_bytes), (5, 1 << 20));
        assert_eq!(p.cache.max_bytes, 100 << 20);
    }

    #[test]
    fn keeps_the_newest_files_within_the_limits() {
        let dir = TempDir::new();
        let d = dir.path();
        file(&d.join("new.log"), 10, 0);
        file(&d.join("mid.log"), 10, 100);
        file(&d.join("sub/old.log"), 10, 200);
        let pruned = prune(
            d,
            Limits {
                max_bytes: 25,
                max_files: 10,
            },
        );
        assert_eq!(
            pruned,
            Pruned {
                files: 1,
                bytes: 10
            }
        );
        assert!(!d.join("sub/old.log").exists());
        let pruned = prune(
            d,
            Limits {
                max_bytes: 1000,
                max_files: 1,
            },
        );
        assert_eq!(pruned.files, 1);
        assert!(d.join("new.log").exists() && !d.join("mid.log").exists());
        assert_eq!(size_of(d), 10);
        assert_eq!(
            prune(&d.join("missing"), Policy::default().logs),
            Pruned::default()
        );
    }
}
