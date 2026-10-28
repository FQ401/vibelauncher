//! The metadata cache of one resource folder.
//!
//! Reading a mod jar or a pack opens an archive, so the result is kept in
//! `<meta>/cache-<folder>.json` and reused while the file's name, size and modification time
//! are unchanged. The digests of a file (SHA-1, SHA-512, the CurseForge fingerprint) are kept
//! the same way, so a repeated update check does not read the files again. The cache is only a
//! shortcut: a missing, damaged, oversized or foreign-format
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
    /// The digests of files, kept apart from the metadata so either can be known alone.
    #[serde(default)]
    digests: BTreeMap<String, DigestItem>,
}

/// The digests of a file, as far as they were computed: what the catalog platforms find files
/// by. They are remembered with the file's size and modification time, like the metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digests {
    /// The size in bytes that was hashed.
    #[serde(default, rename = "bytes", skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Lowercase hex SHA-1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// Lowercase hex SHA-512.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    /// The CurseForge fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<u32>,
}

/// Digests with the stamp they belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DigestItem {
    size: u64,
    secs: i64,
    nanos: u32,
    #[serde(flatten)]
    digests: Digests,
}

impl DigestItem {
    fn matches(&self, stamp: Stamp) -> bool {
        self.size == stamp.size && self.secs == stamp.secs && self.nanos == stamp.nanos
    }
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
    digests: BTreeMap<String, DigestItem>,
    seen: BTreeSet<String>,
    dirty: bool,
}

impl FolderCache {
    /// Loads the cache file at `path`; without a path nothing is kept.
    pub(crate) fn open(path: Option<PathBuf>) -> Self {
        let (items, digests) = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            items,
            digests,
            seen: BTreeSet::new(),
            dirty: false,
        }
    }

    /// Marks `name` as present, so pruning keeps what is known about it.
    pub(crate) fn touch(&mut self, name: &str) {
        self.seen.insert(name.to_owned());
    }

    /// The remembered item of `name` when its size and time still match.
    pub(crate) fn get(&mut self, name: &str, stamp: Stamp) -> Option<Item> {
        self.touch(name);
        self.items.get(name).filter(|i| i.matches(stamp)).cloned()
    }

    pub(crate) fn put(&mut self, name: &str, item: Item) {
        self.touch(name);
        self.items.insert(name.to_owned(), item);
        self.dirty = true;
    }

    /// The remembered digests of `name` when its size and time still match.
    pub(crate) fn get_digests(&mut self, name: &str, stamp: Stamp) -> Option<Digests> {
        self.touch(name);
        self.digests
            .get(name)
            .filter(|i| i.matches(stamp))
            .map(|i| i.digests.clone())
    }

    pub(crate) fn put_digests(&mut self, name: &str, stamp: Stamp, digests: Digests) {
        self.touch(name);
        self.digests.insert(
            name.to_owned(),
            DigestItem {
                size: stamp.size,
                secs: stamp.secs,
                nanos: stamp.nanos,
                digests,
            },
        );
        self.dirty = true;
    }

    /// Forgets `name`.
    pub(crate) fn forget(&mut self, name: &str) {
        let item = self.items.remove(name).is_some();
        let digests = self.digests.remove(name).is_some();
        self.dirty |= item || digests;
    }

    /// Writes the cache when it changed. With `prune`, entries of files not asked for since
    /// opening (deleted or renamed away) are dropped first.
    pub(crate) fn finish(mut self, prune: bool) {
        if prune {
            let before = self.items.len() + self.digests.len();
            let seen = &self.seen;
            self.items.retain(|k, _| seen.contains(k));
            self.digests.retain(|k, _| seen.contains(k));
            self.dirty |= self.items.len() + self.digests.len() != before;
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
            digests: self.digests,
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

type Loaded = (BTreeMap<String, Item>, BTreeMap<String, DigestItem>);

fn load(path: &Path) -> Loaded {
    let Ok(meta) = fs::metadata(path) else {
        return Loaded::default();
    };
    if meta.len() > MAX_BYTES {
        return Loaded::default();
    }
    let Ok(bytes) = fs::read(path) else {
        return Loaded::default();
    };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(f) if f.format == FORMAT => (f.entries, f.digests),
        _ => Loaded::default(),
    }
}
