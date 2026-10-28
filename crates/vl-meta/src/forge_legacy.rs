//! Legacy Forge installers (Minecraft 1.5.2 to 1.12.2): an `install_profile.json` with an
//! `install` section and a `versionInfo` launch profile, and no processors.
//!
//! - `install` names the Forge jar the installer carries (`filePath`), its library coordinate
//!   (`path`), the game version (`minecraft`) and `stripMeta`: the game runs from a copy of the
//!   vanilla jar without `META-INF/` (Minecraft 1.5.2, whose vanilla jar is signed while Forge
//!   replaces classes of the same packages).
//! - `versionInfo` is a version JSON: `mainClass` (`net.minecraft.launchwrapper.Launch`), a single
//!   `minecraftArguments` string with the `--tweakClass` of FML, and libraries in the old shape
//!   (`name`, an optional repository `url`, `checksums`, `clientreq`/`serverreq`). Since Minecraft
//!   1.7.10 it inherits from the game (`inheritsFrom`); before, it repeats the vanilla libraries.
//!
//! [`installer_format`] tells the legacy format from the processor-based one
//! ([`crate::install_profile`]); both exist among the 1.12.2 builds. [`LegacyInstaller::profile`]
//! turns `versionInfo` into a [`LoaderProfile`]:
//!
//! - the library at `install.path` has no download address: it is the jar the installer carries;
//! - a library with a repository gets `downloads.artifact` with that repository (old Forge
//!   addresses are moved to `https://maven.minecraftforge.net/`, other `http` ones to `https`)
//!   and the first of its `checksums`, which is the SHA-1 of the jar; without a repository the
//!   library comes from `https://libraries.minecraft.net/`;
//! - a profile without `inheritsFrom` drops the platform libraries (with `natives` or `rules`
//!   and no repository: LWJGL, JInput) that the vanilla version lists with hashes;
//! - a repeated `group:artifact[:classifier]` keeps its first entry.
//!
//! FML for Minecraft 1.5.2 also reads `<game dir>/lib/deobfuscation_data_<game>.zip` and checks
//! it against a SHA-1 in its `fmlversion.properties`; [`fml_deobfuscation_data`] reads that
//! requirement. Later FML versions carry the data in the Forge jar. That FML finds the game
//! directory through the `minecraft.applet.TargetDirectory` system property (else it uses the
//! default `.minecraft` of the user), so a standalone profile sets it to `${game_directory}`.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use vl_core::component::ComponentKind;
use vl_core::error::{Error, ErrorKind, Result};

use crate::arguments::Argument;
use crate::install_profile::MAX_INSTALL_PROFILE_BYTES;
use crate::libraries::DEFAULT_REPOSITORY;
use crate::loader::{LoaderProfile, check_coordinate, maven_path};
use crate::manifest::is_sha1;
use crate::version::MAX_VERSION_BYTES;

/// The Forge Maven repository that replaces the old Forge addresses.
pub const FORGE_REPOSITORY: &str = "https://maven.minecraftforge.net/";

/// Old Forge repository addresses, moved to [`FORGE_REPOSITORY`].
const OLD_FORGE_REPOSITORIES: [&str; 3] = [
    "http://files.minecraftforge.net/maven/",
    "https://files.minecraftforge.net/maven/",
    "http://maven.minecraftforge.net/",
];

/// The most libraries accepted in one `versionInfo`.
const MAX_LIBRARIES: usize = 1024;

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// The format of an installer's `install_profile.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerFormat {
    /// `spec`, `data`, `processors` and a separate `version.json` ([`crate::install_profile`]).
    Processors,
    /// `install` and `versionInfo` ([`LegacyInstaller`]).
    Legacy,
}

/// Tells the format of an `install_profile.json`.
///
/// # Errors
///
/// `Integrity` for a document that is too large, not a JSON object, or of neither format.
pub fn installer_format(bytes: &[u8]) -> Result<InstallerFormat> {
    if bytes.len() > MAX_INSTALL_PROFILE_BYTES {
        return Err(damaged("install profile is too large"));
    }
    let doc: Map<String, Value> = serde_json::from_slice(bytes)
        .map_err(|e| damaged(format!("invalid install profile: {e}")))?;
    if doc.contains_key("install") && doc.contains_key("versionInfo") {
        Ok(InstallerFormat::Legacy)
    } else if doc.contains_key("json") || doc.contains_key("spec") {
        Ok(InstallerFormat::Processors)
    } else {
        Err(damaged("install profile of an unknown format"))
    }
}

/// A parsed legacy `install_profile.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyInstaller {
    /// The game version it is for.
    pub minecraft: String,
    /// The library coordinate of the Forge jar the installer carries.
    pub path: String,
    /// The name of that jar inside the installer.
    pub file_path: String,
    /// Whether the game runs from a copy of the vanilla jar without `META-INF/`.
    pub strip_meta: bool,
    version_info: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstall {
    path: String,
    file_path: String,
    minecraft: String,
    #[serde(default)]
    strip_meta: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLegacy {
    install: RawInstall,
    version_info: Map<String, Value>,
}

impl LegacyInstaller {
    /// Parses a legacy `install_profile.json`.
    ///
    /// # Errors
    ///
    /// `Integrity` for a document that is too large, of another shape, or names an unsafe
    /// coordinate or file.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_INSTALL_PROFILE_BYTES {
            return Err(damaged("install profile is too large"));
        }
        let raw: RawLegacy = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid legacy install profile: {e}")))?;
        check_coordinate(&raw.install.path)?;
        if !safe_entry(&raw.install.file_path) {
            return Err(damaged(format!(
                "unsafe installer file {:?}",
                raw.install.file_path
            )));
        }
        Ok(Self {
            minecraft: raw.install.minecraft,
            path: raw.install.path,
            file_path: raw.install.file_path,
            strip_meta: raw.install.strip_meta,
            version_info: raw.version_info,
        })
    }

    /// The path of the carried Forge jar below the library directory.
    ///
    /// # Errors
    ///
    /// `Integrity` for an unsafe coordinate (checked by [`parse`](Self::parse) already).
    pub fn library_path(&self) -> Result<String> {
        maven_path(&self.path)
    }

    /// The launch profile of Forge `forge` (the short version) for `game`; see the module
    /// documentation for how the libraries are read.
    ///
    /// # Errors
    ///
    /// `Integrity` for a profile for another game version, without a main class, with a library
    /// that is damaged or not on `https`, or too many libraries.
    pub fn profile(&self, game: &str, forge: &str) -> Result<LoaderProfile> {
        if self.minecraft != game {
            return Err(damaged(format!(
                "the installer is for Minecraft {}, not {game}",
                self.minecraft
            )));
        }
        let info = &self.version_info;
        let text = |key: &str| info.get(key).and_then(Value::as_str);
        let standalone = match text("inheritsFrom") {
            Some(parent) if parent != game => {
                return Err(damaged(format!(
                    "profile is for Minecraft {parent}, not {game}"
                )));
            }
            Some(_) => false,
            None => true,
        };
        let raw_libraries = match info.get("libraries") {
            Some(Value::Array(l)) => l.as_slice(),
            None => &[],
            Some(_) => return Err(damaged("profile libraries are not a list")),
        };
        if raw_libraries.len() > MAX_LIBRARIES {
            return Err(damaged("profile has too many libraries"));
        }
        let mut keys = HashSet::new();
        let mut libraries = Vec::new();
        for lib in raw_libraries {
            let Some(lib) = lib.as_object() else {
                return Err(damaged("a profile library is not an object"));
            };
            if let Some(entry) = convert_library(lib, &self.path, standalone)?
                && keys.insert(library_key(&entry))
            {
                libraries.push(entry);
            }
        }
        let mut doc = Map::new();
        doc.insert("id".to_owned(), json!(text("id").unwrap_or_default()));
        doc.insert("inheritsFrom".to_owned(), json!(game));
        doc.insert("type".to_owned(), json!(text("type").unwrap_or("release")));
        doc.insert(
            "releaseTime".to_owned(),
            json!(text("releaseTime").unwrap_or_default()),
        );
        for key in ["mainClass", "minecraftArguments"] {
            if let Some(v) = text(key) {
                doc.insert(key.to_owned(), json!(v));
            }
        }
        doc.insert("libraries".to_owned(), Value::Array(libraries));
        let bytes = serde_json::to_vec(&doc).map_err(|e| damaged(e.to_string()))?;
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(damaged("profile is too large"));
        }
        let mut profile = LoaderProfile::normalize(ComponentKind::Forge, game, forge, &bytes)?;
        if standalone {
            profile.arguments.jvm.push(Argument::plain(
                "-Dminecraft.applet.TargetDirectory=${game_directory}",
            ));
        }
        if self.strip_meta {
            profile.jar = Some(profile.id.clone());
            profile.validate()?;
        }
        Ok(profile)
    }
}

/// `group:artifact[:classifier]`, as the version merge compares libraries.
fn library_key(entry: &Value) -> String {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parts: Vec<&str> = name.split('@').next().unwrap_or(name).split(':').collect();
    match parts.as_slice() {
        [group, artifact, _, classifier] => format!("{group}:{artifact}:{classifier}"),
        [group, artifact, ..] => format!("{group}:{artifact}"),
        _ => name.to_owned(),
    }
}

/// The repository a legacy library names, as an `https` base ending in `/`.
fn repository(url: &str) -> Result<String> {
    let url = url.trim();
    let mut base = if url.ends_with('/') {
        url.to_owned()
    } else {
        format!("{url}/")
    };
    if let Some(old) = OLD_FORGE_REPOSITORIES
        .iter()
        .find(|o| base.starts_with(**o))
    {
        base = format!("{FORGE_REPOSITORY}{}", &base[old.len()..]);
    } else if let Some(rest) = base.strip_prefix("http://") {
        base = format!("https://{rest}");
    }
    if !base.starts_with("https://") || base.chars().any(char::is_whitespace) {
        return Err(damaged(format!("library repository {url:?} is not https")));
    }
    Ok(base)
}

fn convert_library(
    lib: &Map<String, Value>,
    local: &str,
    standalone: bool,
) -> Result<Option<Value>> {
    let name = lib
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| damaged("a profile library has no name"))?;
    check_coordinate(name)?;
    let url = lib
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| !u.trim().is_empty());
    let natives = lib.get("natives").filter(|n| n.is_object());
    let rules = lib.get("rules").filter(|r| r.is_array());
    if standalone && url.is_none() && (natives.is_some() || rules.is_some()) {
        return Ok(None);
    }
    let path = maven_path(name)?;
    let mut out = Map::new();
    out.insert("name".to_owned(), json!(name));
    if let Some(r) = rules {
        out.insert("rules".to_owned(), r.clone());
    }
    if name == local {
        out.insert(
            "downloads".to_owned(),
            json!({ "artifact": { "path": path, "url": "" } }),
        );
        return Ok(Some(Value::Object(out)));
    }
    let base = url.map_or_else(|| Ok(DEFAULT_REPOSITORY.to_owned()), repository)?;
    if let Some(n) = natives {
        out.insert("natives".to_owned(), n.clone());
        if let Some(e) = lib.get("extract") {
            out.insert("extract".to_owned(), e.clone());
        }
        out.insert("url".to_owned(), json!(base));
        return Ok(Some(Value::Object(out)));
    }
    let sha1 = lib
        .get("checksums")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(Value::as_str)
        .filter(|s| is_sha1(s))
        .map(str::to_ascii_lowercase);
    let mut artifact = Map::new();
    artifact.insert("path".to_owned(), json!(path));
    artifact.insert("url".to_owned(), json!(format!("{base}{path}")));
    if let Some(s) = sha1 {
        artifact.insert("sha1".to_owned(), json!(s));
    }
    out.insert(
        "downloads".to_owned(),
        json!({ "artifact": Value::Object(artifact) }),
    );
    Ok(Some(Value::Object(out)))
}

/// A file name inside an installer jar: one or more safe `/`-separated parts.
fn safe_entry(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':'])
                && !part.chars().any(char::is_control)
        })
}

/// A file FML expects in `<game dir>/lib/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmlLibrary {
    /// The file name (`deobfuscation_data_1.5.2.zip`).
    pub name: String,
    /// Its SHA-1.
    pub sha1: String,
}

/// The deobfuscation data FML reads from its `lib` directory, from the `fmlversion.properties`
/// of the Forge jar: `fmlbuild.mcversion` names the file and `fmlbuild.deobfuscation.hash` is
/// its SHA-1. `None` when the Forge jar carries the data itself (`bundled`) or the properties
/// name no such file.
pub fn fml_deobfuscation_data(fmlversion_properties: &str, bundled: bool) -> Option<FmlLibrary> {
    if bundled {
        return None;
    }
    let value = |key: &str| {
        fmlversion_properties.lines().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim())
        })
    };
    let game = value("fmlbuild.mcversion")?;
    let sha1 = value("fmlbuild.deobfuscation.hash").filter(|s| is_sha1(s))?;
    let safe_game = !game.is_empty()
        && game
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_');
    safe_game.then(|| FmlLibrary {
        name: format!("deobfuscation_data_{game}.zip"),
        sha1: sha1.to_ascii_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_old_repositories() {
        assert_eq!(
            repository("http://files.minecraftforge.net/maven/").unwrap(),
            FORGE_REPOSITORY
        );
        assert_eq!(
            repository("https://maven.minecraftforge.net").unwrap(),
            FORGE_REPOSITORY
        );
        assert_eq!(
            repository("http://repo.example.test/maven2").unwrap(),
            "https://repo.example.test/maven2/"
        );
        assert!(repository("ftp://repo.example.test/").is_err());
        assert!(repository("https://a b/").is_err());
    }

    #[test]
    fn checks_installer_entries() {
        assert!(safe_entry("forge-universal.jar"));
        assert!(safe_entry("maven/a/b.jar"));
        for bad in ["", "../x.jar", "a//b", "C:/x", "a\\b"] {
            assert!(!safe_entry(bad), "{bad}");
        }
    }
}
