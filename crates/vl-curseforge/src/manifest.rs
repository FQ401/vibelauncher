//! The CurseForge modpack format: the `manifest.json` document at the root of a pack zip.
//!
//! A pack names the Minecraft version, its mod loaders (`forge-47.2.0`, `neoforge-21.1.7`,
//! `fabric-0.16.5`, `quilt-0.27.1`; one of them is primary), the files it needs as CurseForge
//! project and file ids, and the folder inside the zip that is copied into the game directory
//! (`overrides` unless it names another). The files themselves are not in the zip: their names,
//! hashes, download URLs and project classes come from the API. This module only reads the
//! document from bytes (opening the zip is the native layer's job). The document is hostile
//! input: its size, the number of files, every id and the override folder are checked.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use vl_core::{Error, ErrorKind, Result};

use crate::loader::ModLoader;

/// Name of the manifest inside the archive.
pub const MANIFEST_NAME: &str = "manifest.json";
/// The override folder when the manifest names none.
pub const DEFAULT_OVERRIDES: &str = "overrides";
/// Largest accepted manifest.
pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
/// Most files listed in one manifest.
pub const MAX_MANIFEST_FILES: usize = 20_000;

const MAX_NAME: usize = 256;
const MAX_VERSION: usize = 128;
const MAX_LOADERS: usize = 16;
const MAX_OVERRIDES_LEN: usize = 128;

/// One mod loader of the pack, as the manifest lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLoader {
    /// The id as written (`forge-47.2.0`).
    pub id: String,
    /// Whether the pack marks it primary.
    pub primary: bool,
}

/// One file the pack needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestFile {
    /// The CurseForge project (mod) id.
    pub project_id: u64,
    /// The CurseForge file id.
    pub file_id: u64,
    /// Whether the pack needs it; an optional file is installed disabled.
    pub required: bool,
}

/// A parsed `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The pack's name.
    pub name: String,
    /// The pack's own version label.
    pub version: String,
    /// The pack's author.
    pub author: String,
    /// The Minecraft version.
    pub minecraft_version: String,
    /// The mod loaders, in the manifest's order.
    pub loaders: Vec<ManifestLoader>,
    /// The files, each file id once, in the manifest's order.
    pub files: Vec<ManifestFile>,
    /// The folder inside the zip copied into the game directory, without a trailing `/`.
    pub overrides: String,
}

impl Manifest {
    /// The loader the pack runs on: the one marked primary, else the first. `None` for a
    /// vanilla pack.
    pub fn primary_loader(&self) -> Option<&ManifestLoader> {
        self.loaders
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.loaders.first())
    }

    /// The primary loader as a CurseForge loader type and a version.
    ///
    /// # Errors
    ///
    /// `Unsupported` when the primary loader's id is not `<loader>-<version>` of a loader
    /// CurseForge knows.
    pub fn loader(&self) -> Result<Option<(ModLoader, String)>> {
        self.primary_loader()
            .map(|l| parse_loader_id(&l.id, &self.minecraft_version))
            .transpose()
    }
}

/// Splits a loader id (`forge-47.2.0`) into the loader and its version. A version written
/// with the Minecraft version in front (`forge-1.20.1-47.2.0`) loses that prefix.
///
/// # Errors
///
/// `Unsupported` for an id without a version or of an unknown loader.
pub fn parse_loader_id(id: &str, minecraft_version: &str) -> Result<(ModLoader, String)> {
    let unsupported = || Error::unsupported(format!("the mod loader {id:?} of the modpack"));
    let (name, version) = id.trim().split_once('-').ok_or_else(unsupported)?;
    let loader = ModLoader::parse(name).ok_or_else(unsupported)?;
    let mc = minecraft_version.trim();
    let version = match version.strip_prefix(mc).and_then(|v| v.strip_prefix('-')) {
        Some(rest) if !mc.is_empty() => rest,
        _ => version,
    };
    if version.trim().is_empty() {
        return Err(unsupported());
    }
    Ok((loader, version.trim().to_owned()))
}

fn bad(why: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::Integrity,
        format!("unusable {MANIFEST_NAME}: {why}"),
    )
}

fn cut(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawManifest {
    #[serde(default)]
    manifest_type: Option<String>,
    #[serde(default)]
    manifest_version: Option<u64>,
    minecraft: RawMinecraft,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    files: Vec<Value>,
    #[serde(default)]
    overrides: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMinecraft {
    version: String,
    #[serde(default)]
    mod_loaders: Vec<Value>,
}

#[derive(Deserialize)]
struct RawLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(rename = "projectID")]
    project_id: u64,
    #[serde(rename = "fileID")]
    file_id: u64,
    #[serde(default = "yes")]
    required: bool,
}

fn yes() -> bool {
    true
}

/// Whether the override folder is a plain relative path: `/`-separated names, no `.` or `..`,
/// no backslash, colon or control character.
fn safe_folder(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_OVERRIDES_LEN
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':'])
                && !part.chars().any(char::is_control)
        })
}

/// Parses the manifest.
///
/// # Errors
///
/// `Integrity` for a document that is too large, is not JSON, has no Minecraft version, lists
/// too many files or a file without positive ids, or names an unsafe override folder;
/// `Unsupported` for another `manifestType` or `manifestVersion`.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(bad(format!("larger than {MAX_MANIFEST_BYTES} bytes")));
    }
    let raw: RawManifest = serde_json::from_slice(bytes).map_err(bad)?;
    if let Some(kind) = raw.manifest_type.as_deref()
        && kind != "minecraftModpack"
    {
        return Err(Error::unsupported(format!(
            "a CurseForge manifest of type {kind:?}"
        )));
    }
    if let Some(version) = raw.manifest_version
        && version != 1
    {
        return Err(Error::unsupported(format!(
            "CurseForge manifest version {version}"
        )));
    }
    let minecraft_version = cut(&raw.minecraft.version, MAX_VERSION);
    if minecraft_version.is_empty() {
        return Err(bad("no Minecraft version"));
    }
    if raw.files.len() > MAX_MANIFEST_FILES {
        return Err(bad(format!("more than {MAX_MANIFEST_FILES} files")));
    }
    let loaders = raw
        .minecraft
        .mod_loaders
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawLoader>(v).ok())
        .map(|l| ManifestLoader {
            id: cut(&l.id, MAX_VERSION),
            primary: l.primary,
        })
        .filter(|l| !l.id.is_empty())
        .take(MAX_LOADERS)
        .collect();
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(raw.files.len());
    for (n, value) in raw.files.into_iter().enumerate() {
        let file: RawFile =
            serde_json::from_value(value).map_err(|e| bad(format!("file {n}: {e}")))?;
        if file.project_id == 0 || file.file_id == 0 {
            return Err(bad(format!("file {n} has no project or file id")));
        }
        if seen.insert(file.file_id) {
            files.push(ManifestFile {
                project_id: file.project_id,
                file_id: file.file_id,
                required: file.required,
            });
        }
    }
    let overrides = raw
        .overrides
        .as_deref()
        .map(|o| o.trim().trim_end_matches('/'))
        .filter(|o| !o.is_empty())
        .unwrap_or(DEFAULT_OVERRIDES)
        .to_owned();
    if !safe_folder(&overrides) {
        return Err(bad(format!("unsafe override folder {overrides:?}")));
    }
    let name = raw
        .name
        .as_deref()
        .map(|n| cut(n, MAX_NAME))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "CurseForge modpack".to_owned());
    Ok(Manifest {
        name,
        version: raw
            .version
            .as_deref()
            .map(|v| cut(v, MAX_VERSION))
            .unwrap_or_default(),
        author: raw
            .author
            .as_deref()
            .map(|a| cut(a, MAX_NAME))
            .unwrap_or_default(),
        minecraft_version,
        loaders,
        files,
        overrides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_ids_split_into_loader_and_version() {
        let cases = [
            ("forge-47.2.0", ModLoader::Forge, "47.2.0"),
            ("neoforge-21.1.7", ModLoader::NeoForge, "21.1.7"),
            ("neoforge-1.20.1-47.1.106", ModLoader::NeoForge, "47.1.106"),
            ("fabric-0.16.5", ModLoader::Fabric, "0.16.5"),
            ("Quilt-0.27.1-beta.1", ModLoader::Quilt, "0.27.1-beta.1"),
            ("forge-1.20.1-47.2.0", ModLoader::Forge, "47.2.0"),
        ];
        for (id, loader, version) in cases {
            assert_eq!(
                parse_loader_id(id, "1.20.1").unwrap(),
                (loader, version.to_owned()),
                "{id}"
            );
        }
        for id in ["forge", "forge-", "rift-1.0", "", "forge-1.20.1-"] {
            assert_eq!(
                parse_loader_id(id, "1.20.1").unwrap_err().kind,
                ErrorKind::Unsupported,
                "{id}"
            );
        }
    }

    #[test]
    fn the_primary_loader_wins_then_the_first() {
        let doc = br#"{"minecraft":{"version":"1.20.1","modLoaders":[
            {"id":"forge-47.1.0"},{"id":"forge-47.2.0","primary":true}]},"files":[]}"#;
        let m = parse_manifest(doc).unwrap();
        assert_eq!(
            m.loader().unwrap(),
            Some((ModLoader::Forge, "47.2.0".into()))
        );
        let doc = br#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"fabric-0.15.0"},{"id":"forge-1"}]}}"#;
        let m = parse_manifest(doc).unwrap();
        assert_eq!(
            m.loader().unwrap(),
            Some((ModLoader::Fabric, "0.15.0".into()))
        );
        let doc = br#"{"minecraft":{"version":"1.20.1"}}"#;
        let m = parse_manifest(doc).unwrap();
        assert_eq!(m.loader().unwrap(), None);
        assert_eq!(m.name, "CurseForge modpack");
        assert_eq!(m.overrides, "overrides");
    }

    #[test]
    fn refuses_other_formats_and_unsafe_input() {
        let unsupported = [
            br#"{"manifestType":"other","minecraft":{"version":"1"}}"#.as_slice(),
            br#"{"manifestVersion":2,"minecraft":{"version":"1"}}"#,
        ];
        for doc in unsupported {
            assert_eq!(
                parse_manifest(doc).unwrap_err().kind,
                ErrorKind::Unsupported
            );
        }
        let damaged = [
            b"not json".as_slice(),
            br#"{"files":[]}"#,
            br#"{"minecraft":{"version":" "}}"#,
            br#"{"minecraft":{"version":"1"},"files":[{"projectID":0,"fileID":1}]}"#,
            br#"{"minecraft":{"version":"1"},"files":[{"projectID":"x","fileID":1}]}"#,
            br#"{"minecraft":{"version":"1"},"overrides":"../up"}"#,
            br#"{"minecraft":{"version":"1"},"overrides":"/abs"}"#,
            br#"{"minecraft":{"version":"1"},"overrides":"c:\\x"}"#,
        ];
        for doc in damaged {
            assert_eq!(
                parse_manifest(doc).unwrap_err().kind,
                ErrorKind::Integrity,
                "{}",
                String::from_utf8_lossy(doc)
            );
        }
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert!(parse_manifest(&big).is_err());
    }

    #[test]
    fn repeated_files_are_listed_once() {
        let doc = br#"{"minecraft":{"version":"1.21.1"},"overrides":"extra/",
            "files":[{"projectID":1,"fileID":10},{"projectID":1,"fileID":10,"required":false},
                     {"projectID":2,"fileID":20,"required":false}]}"#;
        let m = parse_manifest(doc).unwrap();
        assert_eq!(m.overrides, "extra");
        assert_eq!(
            m.files,
            [
                ManifestFile {
                    project_id: 1,
                    file_id: 10,
                    required: true
                },
                ManifestFile {
                    project_id: 2,
                    file_id: 20,
                    required: false
                }
            ]
        );
    }
}
