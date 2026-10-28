//! The metadata cache of one resource folder.
//!
//! Reading a mod jar or a pack opens an archive, so the result is kept in
//! `<meta>/cache-<folder>.json` and reused while the file's name, size and modification time
//! are unchanged. The cache is only a shortcut: a missing, damaged, oversized or foreign-format
//! file is ignored and rebuilt, and a failed write is silently dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use vl_core::resources::{PackMetadata, ResourceKind};
use vl_mods::ModMetadata;

use crate::jar::JarInfo;

/// Version of the cache file format; a file of another version is ignored.
pub(crate) const FORMAT: u32 = 1;
/// Largest cache file that is read or written.
pub(crate) const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Name of the cache file of a folder, inside the metadata directory.
pub(crate) fn file_name(kind: ResourceKind) -> String {
    format!("cache-{}.json", kind.dir().replace('/', "-"))
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    entries: BTreeMap<String, Item>,
}

/// What is remembered about one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Item {
    size: u64,
    secs: i64,
    nanos: u32,
    #[serde(default)]
    pub(crate) mods: Vec<ModMetadata>,
    #[serde(default)]
    pub(crate) nested_jars: Vec<String>,
    #[serde(default)]
    pub(crate) packages: Vec<String>,
    #[serde(default)]
    pub(crate) problem: Option<String>,
    #[serde(default)]
    pub(crate) pack: Option<PackMetadata>,
}

/// Size and modification time of a file, to the nanosecond.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stamp {
    size: u64,
    secs: i64,
    nanos: u32,
}

impl Stamp {
    /// `None` when the file has no usable modification time (it is then never cached).
    pub(crate) fn of(meta: &fs::Metadata) -> Option<Self> {
        let d = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            size: meta.len(),
            secs: i64::try_from(d.as_secs()).ok()?,
            nanos: d.subsec_nanos(),
        })
    }
}

impl Item {
    pub(crate) fn from_jar(stamp: Stamp, info: JarInfo) -> Self {
        Self {
            size: stamp.size,
            secs: stamp.secs,
            nanos: stamp.nanos,
            mods: info.mods,
            nested_jars: info.nested_jars,
            packages: info.packages,
            problem: info.problem,
            pack: None,
        }
    }

    pub(crate) fn from_pack(stamp: Stamp, pack: Result<PackMetadata, String>) -> Self {
        let (pack, problem) = match pack {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            size: stamp.size,
            secs: stamp.secs,
            nanos: stamp.nanos,
            mods: Vec::new(),
            nested_jars: Vec::new(),
            packages: Vec::new(),
            problem,
            pack,
        }
    }

    pub(crate) fn into_jar(self) -> JarInfo {
        JarInfo {
            mods: self.mods,
            nested_jars: self.nested_jars,
            packages: self.packages,
            problem: self.problem,
        }
    }

    fn matches(&self, stamp: Stamp) -> bool {
        self.size == stamp.size && self.secs == stamp.secs && self.nanos == stamp.nanos
    }
}

/// The loaded cache of one folder; [`FolderCache::finish`] writes it back when it changed.
pub(crate) struct FolderCache {
    path: Option<PathBuf>,
    items: BTreeMap<String, Item>,
    seen: BTreeSet<String>,
    dirty: bool,
}

impl FolderCache {
    /// Loads the cache file at `path`; without a path nothing is kept.
    pub(crate) fn open(path: Option<PathBuf>) -> Self {
        let items = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            items,
            seen: BTreeSet::new(),
            dirty: false,
        }
    }

    /// The remembered item of `name` when its size and time still match.
    pub(crate) fn get(&mut self, name: &str, stamp: Stamp) -> Option<Item> {
        self.seen.insert(name.to_owned());
        self.items.get(name).filter(|i| i.matches(stamp)).cloned()
    }

    pub(crate) fn put(&mut self, name: &str, item: Item) {
        self.seen.insert(name.to_owned());
        self.items.insert(name.to_owned(), item);
        self.dirty = true;
    }

    /// Forgets `name`.
    pub(crate) fn forget(&mut self, name: &str) {
        if self.items.remove(name).is_some() {
            self.dirty = true;
        }
    }

    /// Writes the cache when it changed. With `prune`, entries of files not asked for since
    /// opening (deleted or renamed away) are dropped first.
    pub(crate) fn finish(mut self, prune: bool) {
        if prune {
            let before = self.items.len();
            let seen = &self.seen;
            self.items.retain(|k, _| seen.contains(k));
            self.dirty |= self.items.len() != before;
        }
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if !self.dirty {
            return;
        }
        let file = CacheFile {
            format: FORMAT,
            entries: self.items,
        };
        match serde_json::to_vec(&file) {
            Ok(bytes) if bytes.len() as u64 <= MAX_BYTES => {
                let _ = vl_store::write_atomic(path, &bytes);
            }
            // Too big (or unserializable): keep no cache rather than a stale one.
            _ => {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn load(path: &Path) -> BTreeMap<String, Item> {
    let Ok(meta) = fs::metadata(path) else {
        return BTreeMap::new();
    };
    if meta.len() > MAX_BYTES {
        return BTreeMap::new();
    }
    let Ok(bytes) = fs::read(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(f) if f.format == FORMAT => f.entries,
        _ => BTreeMap::new(),
    }
}
