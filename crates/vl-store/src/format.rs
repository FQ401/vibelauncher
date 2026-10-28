//! The on-disk format version and the migration chain.
//!
//! The data directory holds [`FORMAT_FILE`] with the version of the layout. A directory with
//! content but no format file is version 0 (the layout before versions existed); an empty
//! directory is new and gets the current version at once. Opening older data runs the chain of
//! [`Migration`]s forward, after a snapshot of the data directory; data from a newer launcher is
//! refused.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{StoreError, write_atomic};

/// The format this launcher reads and writes.
pub const CURRENT_FORMAT: u32 = 1;
/// The file in the data directory that records the format version.
pub const FORMAT_FILE: &str = "format.json";
/// Directory (inside the data directory) that holds snapshots taken before migrations.
pub const SNAPSHOT_DIR: &str = "snapshots";

/// Top-level entries that are not data: skipped when deciding whether a directory is new and when
/// taking a snapshot.
const NOT_DATA: &[&str] = &[crate::lock::LOCK_FILE, "cache", "logs", SNAPSHOT_DIR];

/// One step of the chain: turns the layout of format `from` into the layout of format `from + 1`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// The format this step starts from.
    pub from: u32,
    /// What the step does, for reports.
    pub name: &'static str,
    /// Rewrites the data directory in place. Must be safe to run again after a crash, because the
    /// version is recorded only after the step succeeds.
    pub apply: fn(&Path) -> Result<(), String>,
}

/// The steps from every old format to [`CURRENT_FORMAT`], in order of `from`.
pub const MIGRATIONS: &[Migration] = &[Migration {
    from: 0,
    name: "create the standard directories",
    apply: v0_to_v1,
}];

/// Format 0 had no version file and no guaranteed layout; format 1 always has `instances/`.
fn v0_to_v1(data: &Path) -> Result<(), String> {
    let dir = data.join("instances");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// What a migration run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// The format the data had.
    pub from: u32,
    /// The format the data has now.
    pub to: u32,
    /// The snapshot taken before the first step.
    pub snapshot: PathBuf,
}

/// Reads the version recorded in `data`; `None` when there is no format file.
pub fn read_version(data: &Path) -> Result<Option<u32>, StoreError> {
    let path = data.join(FORMAT_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::io(&path, e)),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| StoreError::corrupt(&path, e.to_string()))?;
    value
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .map(Some)
        .ok_or_else(|| StoreError::corrupt(&path, "missing or invalid \"format\""))
}

/// Records `version` in `data`.
pub fn write_version(data: &Path, version: u32) -> Result<(), StoreError> {
    let text = format!("{{\n  \"format\": {version}\n}}\n");
    write_atomic(&data.join(FORMAT_FILE), text.as_bytes())
}

/// Brings `data` to format `current` through `chain`; `None` when nothing needed migrating.
///
/// A snapshot is taken before the first step. The version is recorded after each step, so a
/// failed step leaves the data at the last completed version.
pub fn migrate(
    data: &Path,
    chain: &[Migration],
    current: u32,
) -> Result<Option<MigrationReport>, StoreError> {
    migrate_with_snapshots(data, chain, current, &data.join(SNAPSHOT_DIR))
}

/// [`migrate`] with the snapshots in `snapshots` (the backups section) instead of
/// `<data>/snapshots`.
pub fn migrate_with_snapshots(
    data: &Path,
    chain: &[Migration],
    current: u32,
    snapshots: &Path,
) -> Result<Option<MigrationReport>, StoreError> {
    let from = match read_version(data)? {
        Some(v) => v,
        None if is_new(data)? => {
            write_version(data, current)?;
            return Ok(None);
        }
        None => 0,
    };
    if from > current {
        return Err(StoreError::FormatTooNew {
            found: from,
            supported: current,
        });
    }
    if from == current {
        return Ok(None);
    }
    let steps = (from..current)
        .map(|v| {
            chain.iter().find(|m| m.from == v).ok_or_else(|| {
                StoreError::corrupt(
                    &data.join(FORMAT_FILE),
                    format!("no migration from format {v}"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let snapshot = snapshot(data, snapshots, from)?;
    for step in steps {
        let failed = |detail: String| StoreError::Migration {
            from: step.from,
            snapshot: Some(snapshot.clone()),
            detail,
        };
        (step.apply)(data).map_err(failed)?;
        write_version(data, step.from + 1).map_err(|e| failed(e.to_string()))?;
    }
    Ok(Some(MigrationReport {
        from,
        to: current,
        snapshot,
    }))
}

fn is_new(data: &Path) -> Result<bool, StoreError> {
    let entries = fs::read_dir(data).map_err(|e| StoreError::io(data, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| StoreError::io(data, e))?;
        if !NOT_DATA.iter().any(|n| entry.file_name() == *n) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Copies the data (not cache, logs, the lock or older snapshots) to
/// `<snapshots>/format-<from>[-<n>]`.
fn snapshot(data: &Path, root: &Path, from: u32) -> Result<PathBuf, StoreError> {
    let mut target = root.join(format!("format-{from}"));
    let mut n = 1;
    while target.exists() {
        n += 1;
        target = root.join(format!("format-{from}-{n}"));
    }
    fs::create_dir_all(&target).map_err(|e| StoreError::io(&target, e))?;
    let result = copy_dir(data, &target, true);
    if result.is_err() {
        let _ = fs::remove_dir_all(&target);
    }
    result?;
    Ok(target)
}

fn copy_dir(from: &Path, to: &Path, top: bool) -> Result<(), StoreError> {
    for entry in fs::read_dir(from).map_err(|e| StoreError::io(from, e))? {
        let entry = entry.map_err(|e| StoreError::io(from, e))?;
        if top && NOT_DATA.iter().any(|n| entry.file_name() == *n) {
            continue;
        }
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| StoreError::io(&src, e))?;
        if kind.is_dir() {
            fs::create_dir_all(&dst).map_err(|e| StoreError::io(&dst, e))?;
            copy_dir(&src, &dst, false)?;
        } else if kind.is_file() {
            fs::copy(&src, &dst).map_err(|e| StoreError::io(&src, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A synthetic format-0 directory: content, no version file.
    fn legacy_dir() -> TempDir {
        let dir = TempDir::new();
        fs::write(dir.path().join("settings.toml"), "memory = 2048\n").unwrap();
        fs::create_dir_all(dir.path().join("accounts")).unwrap();
        fs::write(dir.path().join("accounts/list.json"), "[]").unwrap();
        dir
    }

    #[test]
    fn migrates_v0_to_v1_with_a_snapshot() {
        let dir = legacy_dir();
        let report = migrate(dir.path(), MIGRATIONS, CURRENT_FORMAT)
            .unwrap()
            .unwrap();
        assert_eq!((report.from, report.to), (0, 1));
        assert_eq!(read_version(dir.path()).unwrap(), Some(1));
        assert!(dir.path().join("instances").is_dir());
        // Existing data is kept ...
        assert_eq!(
            fs::read_to_string(dir.path().join("settings.toml")).unwrap(),
            "memory = 2048\n"
        );
        // ... and the snapshot has the data as it was before.
        assert_eq!(
            fs::read_to_string(report.snapshot.join("settings.toml")).unwrap(),
            "memory = 2048\n"
        );
        assert!(report.snapshot.join("accounts/list.json").is_file());
        assert!(!report.snapshot.join("instances").exists());
        assert!(!report.snapshot.join(SNAPSHOT_DIR).exists());
        // Running again does nothing.
        assert!(migrate(dir.path(), MIGRATIONS, 1).unwrap().is_none());
    }

    #[test]
    fn an_empty_directory_is_new() {
        let dir = TempDir::new();
        fs::create_dir(dir.path().join("cache")).unwrap();
        assert!(migrate(dir.path(), MIGRATIONS, 1).unwrap().is_none());
        assert_eq!(read_version(dir.path()).unwrap(), Some(1));
        assert!(!dir.path().join(SNAPSHOT_DIR).exists());
    }

    #[test]
    fn newer_data_is_refused_and_left_alone() {
        let dir = TempDir::new();
        write_version(dir.path(), 7).unwrap();
        let err = migrate(dir.path(), MIGRATIONS, 1).unwrap_err();
        assert!(matches!(
            err,
            StoreError::FormatTooNew {
                found: 7,
                supported: 1
            }
        ));
        assert_eq!(read_version(dir.path()).unwrap(), Some(7));
    }

    #[test]
    fn a_damaged_format_file_is_reported() {
        let dir = TempDir::new();
        for text in ["not json", "{}", "{\"format\": -1}", "{\"format\": \"1\"}"] {
            fs::write(dir.path().join(FORMAT_FILE), text).unwrap();
            assert!(
                matches!(read_version(dir.path()), Err(StoreError::Corrupt { .. })),
                "{text}"
            );
        }
    }

    fn mark_two(data: &Path) -> Result<(), String> {
        fs::write(data.join("two"), "2").map_err(|e| e.to_string())
    }

    fn fail(_: &Path) -> Result<(), String> {
        Err("boom".into())
    }

    fn step(from: u32, apply: fn(&Path) -> Result<(), String>) -> Migration {
        Migration {
            from,
            name: "test",
            apply,
        }
    }

    #[test]
    fn runs_every_step_in_order() {
        let dir = legacy_dir();
        let chain = [step(0, v0_to_v1), step(1, mark_two)];
        let report = migrate(dir.path(), &chain, 2).unwrap().unwrap();
        assert_eq!((report.from, report.to), (0, 2));
        assert_eq!(read_version(dir.path()).unwrap(), Some(2));
        assert!(dir.path().join("two").is_file());
        // One snapshot for the whole run, taken at the starting format.
        assert!(!report.snapshot.join("two").exists());
    }

    #[test]
    fn a_failed_step_keeps_the_last_good_version_and_the_snapshot() {
        let dir = legacy_dir();
        let chain = [step(0, v0_to_v1), step(1, fail)];
        let err = migrate(dir.path(), &chain, 2).unwrap_err();
        let StoreError::Migration { from, snapshot, .. } = err else {
            panic!("expected a migration error");
        };
        assert_eq!(from, 1);
        assert_eq!(read_version(dir.path()).unwrap(), Some(1));
        let snapshot = snapshot.unwrap();
        assert!(snapshot.join("settings.toml").is_file());
    }

    #[test]
    fn a_gap_in_the_chain_is_reported_before_anything_changes() {
        let dir = legacy_dir();
        let err = migrate(dir.path(), &[step(1, mark_two)], 2).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }));
        assert_eq!(read_version(dir.path()).unwrap(), None);
        assert!(!dir.path().join(SNAPSHOT_DIR).exists());
    }

    #[test]
    fn snapshots_do_not_overwrite_each_other() {
        let dir = legacy_dir();
        let a = snapshot(dir.path(), &dir.path().join(SNAPSHOT_DIR), 0).unwrap();
        let b = snapshot(dir.path(), &dir.path().join(SNAPSHOT_DIR), 0).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn the_shipped_chain_reaches_the_current_format() {
        for v in 0..CURRENT_FORMAT {
            assert!(MIGRATIONS.iter().any(|m| m.from == v), "no step from {v}");
        }
    }
}
