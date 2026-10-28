//! Files of one instance's resource folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use vl_core::resources::{ResourceEntry, ResourceKind};
use vl_core::{Error, ErrorKind};

use crate::index::ModClassIndex;
use crate::jar::read_jar;

/// Appended to a file name while the file is disabled.
pub const DISABLED_SUFFIX: &str = ".disabled";

/// The resource folders of one instance.
#[derive(Debug, Clone)]
pub struct Resources {
    game_dir: PathBuf,
    trash_dir: PathBuf,
}

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Splits a trailing `.disabled` (any case) off a file name.
fn split_disabled(name: &str) -> (&str, bool) {
    let cut = name.len().saturating_sub(DISABLED_SUFFIX.len());
    match name.get(cut..) {
        Some(tail) if cut > 0 && tail.eq_ignore_ascii_case(DISABLED_SUFFIX) => (&name[..cut], true),
        _ => (name, false),
    }
}

/// A plain file name: one path segment, no traversal.
fn check_name(name: &str) -> vl_core::Result<()> {
    let bad =
        name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']);
    if bad {
        return Err(Error::invalid_input(format!("invalid file name {name:?}")));
    }
    Ok(())
}

/// The extension a file of `kind` must have to be added.
fn allowed_extension(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Mods => "jar",
        _ => "zip",
    }
}

impl Resources {
    /// The resources of an instance with game directory `game_dir`; removed files go to
    /// `trash_dir`. Nothing is created until needed.
    pub fn new(game_dir: PathBuf, trash_dir: PathBuf) -> Self {
        Self {
            game_dir,
            trash_dir,
        }
    }

    /// The folder of `kind`.
    pub fn dir(&self, kind: ResourceKind) -> PathBuf {
        kind.dir()
            .split('/')
            .fold(self.game_dir.clone(), |p, seg| p.join(seg))
    }

    /// Lists a folder, sorted by name; a missing folder is empty. Mod jars carry their
    /// metadata; a file that cannot be read carries a problem and stays in the list.
    pub fn list(&self, kind: ResourceKind) -> vl_core::Result<Vec<ResourceEntry>> {
        let dir = self.dir(kind);
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&dir, e)),
        };
        let mut entries = Vec::new();
        for item in read.flatten() {
            let raw = item.file_name().to_string_lossy().into_owned();
            if raw.starts_with('.') || raw.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let Ok(meta) = fs::metadata(item.path()) else {
                continue;
            };
            let (file, disabled) = split_disabled(&raw);
            let mut entry = ResourceEntry {
                file: file.to_owned(),
                enabled: !disabled,
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_secs()).ok()),
                is_dir: meta.is_dir(),
                mods: Vec::new(),
                nested_jars: Vec::new(),
                problem: None,
            };
            if kind == ResourceKind::Mods
                && !meta.is_dir()
                && file.to_ascii_lowercase().ends_with(".jar")
            {
                let info = read_jar(&item.path());
                entry.mods = info.mods;
                entry.nested_jars = info.nested_jars;
                entry.problem = info.problem;
            }
            entries.push(entry);
        }
        entries.sort_by_key(|e| (e.file.to_lowercase(), e.file.clone()));
        Ok(entries)
    }

    /// Enables or disables `file` by renaming it. Doing what is already done succeeds.
    pub fn set_enabled(
        &self,
        kind: ResourceKind,
        file: &str,
        enabled: bool,
    ) -> vl_core::Result<()> {
        let file = split_disabled(file).0;
        check_name(file)?;
        let dir = self.dir(kind);
        let on = dir.join(file);
        let off = dir.join(format!("{file}{DISABLED_SUFFIX}"));
        let (from, to) = if enabled { (off, on) } else { (on, off) };
        let to_exists = fs::symlink_metadata(&to).is_ok();
        let from_exists = fs::symlink_metadata(&from).is_ok();
        match (from_exists, to_exists) {
            (true, true) => Err(Error::conflict(format!(
                "both {file} and {file}{DISABLED_SUFFIX} exist"
            ))),
            (true, false) => fs::rename(&from, &to).map_err(|e| io_err(&to, e)),
            (false, true) => Ok(()),
            (false, false) => Err(Error::not_found(format!("{} {file}", kind.name()))),
        }
    }

    /// Copies the file at `source` into the folder and returns its name there. A name that is
    /// already taken (enabled or disabled) is a conflict.
    pub fn add(&self, kind: ResourceKind, source: &Path) -> vl_core::Result<String> {
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::invalid_input("the source has no file name"))?
            .to_owned();
        check_name(&name)?;
        let wanted = allowed_extension(kind);
        let ok = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(wanted));
        if !ok {
            return Err(Error::invalid_input(format!(
                "{} must be a .{wanted} file",
                kind.name()
            )));
        }
        if !source.is_file() {
            return Err(Error::not_found(format!("file {}", source.display())));
        }
        let dir = self.dir(kind);
        fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let target = dir.join(&name);
        let disabled = dir.join(format!("{name}{DISABLED_SUFFIX}"));
        if fs::symlink_metadata(&target).is_ok() || fs::symlink_metadata(&disabled).is_ok() {
            return Err(Error::conflict(format!("{name} already exists")));
        }
        // Copy beside the target first so a half-copied file never has a final name.
        let temp = dir.join(format!(".adding-{name}"));
        let copied = fs::copy(source, &temp)
            .map_err(|e| io_err(source, e))
            .and_then(|_| fs::rename(&temp, &target).map_err(|e| io_err(&target, e)));
        if copied.is_err() {
            let _ = fs::remove_file(&temp);
        }
        copied.map(|()| name)
    }

    /// Moves `file` (enabled or disabled) into the trash; returns where it went.
    pub fn remove(&self, kind: ResourceKind, file: &str) -> vl_core::Result<PathBuf> {
        let file = split_disabled(file).0;
        check_name(file)?;
        let dir = self.dir(kind);
        let found = [dir.join(file), dir.join(format!("{file}{DISABLED_SUFFIX}"))]
            .into_iter()
            .find(|p| fs::symlink_metadata(p).is_ok())
            .ok_or_else(|| Error::not_found(format!("{} {file}", kind.name())))?;
        fs::create_dir_all(&self.trash_dir).map_err(|e| io_err(&self.trash_dir, e))?;
        let stem = kind.dir().replace('/', "-");
        let leaf = found
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stamp = now();
        let mut n = 0u32;
        loop {
            let name = if n == 0 {
                format!("{stem}-{stamp}-{leaf}")
            } else {
                format!("{stem}-{stamp}-{n}-{leaf}")
            };
            let target = self.trash_dir.join(name);
            if fs::symlink_metadata(&target).is_err() {
                fs::rename(&found, &target).map_err(|e| io_err(&target, e))?;
                return Ok(target);
            }
            n += 1;
        }
    }

    /// Builds the class index of the enabled mod jars.
    pub fn class_index(&self) -> vl_core::Result<ModClassIndex> {
        let dir = self.dir(ResourceKind::Mods);
        let mut index = ModClassIndex::new();
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(index),
            Err(e) => return Err(io_err(&dir, e)),
        };
        let mut jars: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("jar"))
            })
            .collect();
        jars.sort();
        for path in jars {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            index.add_jar(&stem, &read_jar(&path));
        }
        Ok(index)
    }
}
