//! Asset indexes and objects, including the legacy and virtual layouts.
//!
//! [`AssetIndex::parse`] reads an asset index file (`assets/indexes/<id>.json`); [`AssetIndex::plan`]
//! turns it into an [`AssetPlan`]: which objects to download and where, how much that is in
//! total, and which files to restore for old versions. Nothing here touches the network or the
//! disk; the native layer downloads the plan's objects, checks them, and performs the restores.
//!
//! # Storage
//!
//! Objects are content-addressed and shared by all instances. An object with hash `ab12...` is
//! stored at `objects/ab/ab12...` under the assets directory and downloaded from
//! `https://resources.download.minecraft.net/ab/ab12...`.
//!
//! # Layouts
//!
//! - [`AssetLayout::Objects`]: the game reads `objects/` through the index (1.7 and newer).
//! - [`AssetLayout::Virtual`] (`"virtual": true`): the game reads plain files from
//!   `virtual/<index id>/` under the assets directory (1.6 up to 1.7.x, index `legacy`).
//! - [`AssetLayout::Resources`] (`"map_to_resources": true`): the game reads plain files from
//!   `resources/` in the game directory (before 1.6, index `pre-1.6`).
//!
//! When both flags are set, `map_to_resources` wins. For the last two layouts every index entry
//! becomes an [`AssetRestore`]: a copy of the object at the entry's name under the layout's root.
//! Restores come from objects, so the objects are downloaded in every layout.
//!
//! Index input is hostile: its size is capped, hashes must be SHA-1 hex, and names must be
//! relative paths without `.`, `..`, empty components, backslashes, drive letters or control
//! characters, so a restore can never leave its root.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vl_core::error::{Error, ErrorKind, Result};

use crate::manifest::is_sha1;

/// The largest asset index accepted, in bytes. The real ones are under 1 MiB.
pub const MAX_ASSET_INDEX_BYTES: usize = 16 * 1024 * 1024;

/// The host objects are downloaded from.
pub const RESOURCES_HOST: &str = "https://resources.download.minecraft.net";

/// Where the game reads its assets from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetLayout {
    /// `objects/` through the index.
    Objects,
    /// Plain files under `virtual/<index id>/` in the assets directory.
    Virtual,
    /// Plain files under `resources/` in the game directory.
    Resources,
}

/// One entry of an asset index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetObject {
    /// The logical path, e.g. `minecraft/sounds/mob/cow/say1.ogg`. Always a safe relative path.
    pub name: String,
    /// The SHA-1 of the content, 40 lowercase hexadecimal digits.
    pub hash: String,
    /// The size in bytes.
    pub size: u64,
}

impl AssetObject {
    /// The path of the object relative to the assets directory: `objects/<2>/<hash>`.
    pub fn store_path(&self) -> String {
        format!("objects/{}", self.hash_path())
    }

    /// The download URL: `https://resources.download.minecraft.net/<2>/<hash>`.
    pub fn url(&self) -> String {
        format!("{RESOURCES_HOST}/{}", self.hash_path())
    }

    /// `<first two hash characters>/<hash>`.
    fn hash_path(&self) -> String {
        format!("{}/{}", &self.hash[..2], self.hash)
    }
}

/// A parsed asset index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIndex {
    /// `virtual`: plain files under `virtual/<id>/`.
    pub is_virtual: bool,
    /// `map_to_resources`: plain files under the game directory's `resources/`.
    pub map_to_resources: bool,
    /// The entries, ordered by name.
    pub objects: Vec<AssetObject>,
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(default, rename = "virtual")]
    is_virtual: bool,
    #[serde(default)]
    map_to_resources: bool,
    objects: BTreeMap<String, RawObject>,
}

#[derive(Deserialize)]
struct RawObject {
    hash: String,
    size: u64,
}

fn integrity(message: String) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// Whether `name` is a relative path that stays inside the directory it is joined to.
fn is_safe_relative_path(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':'])
        && !name.chars().any(char::is_control)
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl AssetIndex {
    /// Parses an asset index file.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Integrity`] when the document is too large, is not an index, or has an entry
    /// with an unsafe name or a hash that is not SHA-1 hex.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_ASSET_INDEX_BYTES {
            return Err(integrity(format!(
                "asset index is larger than {MAX_ASSET_INDEX_BYTES} bytes"
            )));
        }
        let raw: RawIndex = serde_json::from_slice(bytes)
            .map_err(|e| integrity(format!("invalid asset index: {e}")))?;
        let mut objects = Vec::with_capacity(raw.objects.len());
        for (name, object) in raw.objects {
            if !is_safe_relative_path(&name) {
                return Err(integrity(format!(
                    "asset index has an unsafe name: {name:?}"
                )));
            }
            if !is_sha1(&object.hash) {
                return Err(integrity(format!(
                    "asset index entry {name:?} has an invalid hash"
                )));
            }
            objects.push(AssetObject {
                name,
                hash: object.hash.to_ascii_lowercase(),
                size: object.size,
            });
        }
        Ok(Self {
            is_virtual: raw.is_virtual,
            map_to_resources: raw.map_to_resources,
            objects,
        })
    }

    /// The layout the game expects.
    pub fn layout(&self) -> AssetLayout {
        if self.map_to_resources {
            AssetLayout::Resources
        } else if self.is_virtual {
            AssetLayout::Virtual
        } else {
            AssetLayout::Objects
        }
    }

    /// Builds the plan for the index with id `index_id` (used for the `virtual/<id>` root).
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidInput`] when `index_id` is not a single safe path component.
    pub fn plan(&self, index_id: &str) -> Result<AssetPlan> {
        if !is_safe_relative_path(index_id) || index_id.contains('/') {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("unsafe asset index id: {index_id:?}"),
            ));
        }
        let layout = self.layout();
        let mut seen = BTreeSet::new();
        let mut downloads = Vec::new();
        let mut restores = Vec::new();
        let (mut total_size, mut total_count) = (0u64, 0u64);
        for object in &self.objects {
            total_count += 1;
            total_size = total_size.saturating_add(object.size);
            if seen.insert(object.hash.as_str()) {
                downloads.push(object.clone());
            }
            let target = match layout {
                AssetLayout::Objects => continue,
                AssetLayout::Virtual => format!("virtual/{index_id}/{}", object.name),
                AssetLayout::Resources => format!("resources/{}", object.name),
            };
            restores.push(AssetRestore {
                source: object.store_path(),
                target,
                size: object.size,
            });
        }
        Ok(AssetPlan {
            layout,
            downloads,
            restores,
            total_size,
            total_count,
        })
    }
}

/// A file to copy out of the object store for the `virtual` and `map_to_resources` layouts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRestore {
    /// The object, relative to the assets directory (`objects/<2>/<hash>`).
    pub source: String,
    /// The destination. For [`AssetLayout::Virtual`] relative to the assets directory
    /// (`virtual/<id>/<name>`); for [`AssetLayout::Resources`] relative to the game directory
    /// (`resources/<name>`).
    pub target: String,
    /// The size in bytes, to skip files that already have it.
    pub size: u64,
}

/// What the native layer has to do for one asset index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPlan {
    /// The layout the game expects.
    pub layout: AssetLayout,
    /// The objects to download, one per distinct hash, ordered by first name.
    pub downloads: Vec<AssetObject>,
    /// The files to restore (empty for [`AssetLayout::Objects`]).
    pub restores: Vec<AssetRestore>,
    /// The size of all objects in bytes, counting each index entry (a shared hash once per name).
    pub total_size: u64,
    /// The number of index entries.
    pub total_count: u64,
}

impl AssetPlan {
    /// The number of bytes in [`Self::downloads`], each distinct object once.
    pub fn download_size(&self) -> u64 {
        self.downloads
            .iter()
            .fold(0u64, |sum, o| sum.saturating_add(o.size))
    }

    /// Keeps only the downloads for which `is_missing` is true. The native layer passes a check
    /// of the object store (existence and size by default, hash on request).
    pub fn retain_missing(&mut self, mut is_missing: impl FnMut(&AssetObject) -> bool) {
        self.downloads.retain(|o| is_missing(o));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/assets/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    const H1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const H2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn modern_index_uses_the_object_store_only() {
        let index = AssetIndex::parse(&fixture("modern.json")).unwrap();
        assert_eq!(index.layout(), AssetLayout::Objects);
        let plan = index.plan("12").unwrap();
        assert!(plan.restores.is_empty());
        assert_eq!(plan.total_count, 4);
        assert_eq!(plan.total_size, 100 + 2000 + 2000 + 30000);
        // The shared hash is downloaded once.
        assert_eq!(plan.downloads.len(), 3);
        assert_eq!(plan.download_size(), 100 + 2000 + 30000);
    }

    #[test]
    fn object_paths_and_urls() {
        let index = AssetIndex::parse(&fixture("modern.json")).unwrap();
        let icon = &index.objects[0];
        assert_eq!(icon.name, "icons/icon_16x16.png");
        assert_eq!(icon.store_path(), format!("objects/aa/{H1}"));
        assert_eq!(
            icon.url(),
            format!("https://resources.download.minecraft.net/aa/{H1}")
        );
    }

    #[test]
    fn virtual_index_restores_under_virtual_id() {
        let index = AssetIndex::parse(&fixture("legacy.json")).unwrap();
        assert_eq!(index.layout(), AssetLayout::Virtual);
        let plan = index.plan("legacy").unwrap();
        assert_eq!(plan.downloads.len(), 2);
        assert_eq!(plan.restores.len(), 3);
        // Both names of the shared object are restored.
        let targets: Vec<&str> = plan.restores.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "virtual/legacy/icons/icon_16x16.png",
                "virtual/legacy/sound/mob/cow/say1.ogg",
                "virtual/legacy/sound/mob/cow/say2.ogg",
            ]
        );
        assert_eq!(plan.restores[1].source, format!("objects/bb/{H2}"));
    }

    #[test]
    fn pre_1_6_index_maps_to_game_resources() {
        let index = AssetIndex::parse(&fixture("pre16.json")).unwrap();
        assert_eq!(index.layout(), AssetLayout::Resources);
        let plan = index.plan("pre-1.6").unwrap();
        let targets: Vec<&str> = plan.restores.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "resources/music/calm1.ogg",
                "resources/newsound/mob/ghast/moan1.ogg"
            ]
        );
        assert_eq!(plan.total_size, 32000);
    }

    #[test]
    fn map_to_resources_wins_over_virtual() {
        let doc = br#"{"virtual":true,"map_to_resources":true,"objects":{}}"#;
        assert_eq!(
            AssetIndex::parse(doc).unwrap().layout(),
            AssetLayout::Resources
        );
    }

    #[test]
    fn retain_missing_filters_downloads() {
        let index = AssetIndex::parse(&fixture("modern.json")).unwrap();
        let mut plan = index.plan("12").unwrap();
        plan.retain_missing(|o| o.hash != H1);
        assert_eq!(plan.downloads.len(), 2);
        assert_eq!(plan.download_size(), 32000);
        // Totals describe the index, not what is left to fetch.
        assert_eq!(plan.total_count, 4);
    }

    #[test]
    fn uppercase_hash_is_normalized() {
        let doc = format!(
            r#"{{"objects":{{"a":{{"hash":"{}","size":1}}}}}}"#,
            H1.to_uppercase()
        );
        let index = AssetIndex::parse(doc.as_bytes()).unwrap();
        assert_eq!(index.objects[0].hash, H1);
    }

    #[test]
    fn hostile_input_is_rejected() {
        let err = AssetIndex::parse(&fixture("damaged.json")).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Integrity);
        for name in ["/abs", "a//b", "a/./b", "a\\b", "C:/x", "", "a/", "a\u{0}b"] {
            let doc = serde_json::json!({"objects": {name: {"hash": H1, "size": 1}}});
            assert!(
                AssetIndex::parse(doc.to_string().as_bytes()).is_err(),
                "{name:?}"
            );
        }
        for hash in ["", "abc", &"z".repeat(40)] {
            let doc = serde_json::json!({"objects": {"a": {"hash": hash, "size": 1}}});
            assert!(
                AssetIndex::parse(doc.to_string().as_bytes()).is_err(),
                "{hash:?}"
            );
        }
        assert!(AssetIndex::parse(b"[]").is_err());
        assert!(AssetIndex::parse(b"{}").is_err());
        assert!(AssetIndex::parse(&vec![b' '; MAX_ASSET_INDEX_BYTES + 1]).is_err());
    }

    #[test]
    fn unsafe_index_id_is_rejected() {
        let index = AssetIndex::parse(&fixture("legacy.json")).unwrap();
        for id in ["", "..", "a/b", "a\\b", "x:y"] {
            assert_eq!(
                index.plan(id).unwrap_err().kind,
                ErrorKind::InvalidInput,
                "{id:?}"
            );
        }
    }

    #[test]
    fn totals_do_not_overflow() {
        let doc = format!(
            r#"{{"objects":{{"a":{{"hash":"{H1}","size":18446744073709551615}},"b":{{"hash":"{H2}","size":5}}}}}}"#
        );
        let plan = AssetIndex::parse(doc.as_bytes())
            .unwrap()
            .plan("1")
            .unwrap();
        assert_eq!(plan.total_size, u64::MAX);
        assert_eq!(plan.download_size(), u64::MAX);
    }

    #[test]
    fn plan_is_serializable() {
        let plan = AssetIndex::parse(&fixture("legacy.json"))
            .unwrap()
            .plan("legacy")
            .unwrap();
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<AssetPlan>(&json).unwrap(), plan);
    }
}
