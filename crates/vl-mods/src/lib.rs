//! Mod metadata: parsing the metadata files that mod loaders read from a jar.
//!
//! Pure: every function takes the bytes of one file and returns the mods it declares. Reading
//! the jar is the caller's job. Supported files:
//!
//! - `fabric.mod.json` — Fabric ([`parse_fabric`]);
//! - `quilt.mod.json` — Quilt ([`parse_quilt`]);
//! - `META-INF/mods.toml` — Forge ([`parse_mods_toml`] with [`ModLoader::Forge`]);
//! - `META-INF/neoforge.mods.toml` — NeoForge ([`parse_mods_toml`] with [`ModLoader::NeoForge`]);
//! - `mcmod.info` — legacy Forge ([`parse_mcmod_info`]).
//!
//! Damaged input is an `Integrity` error, never a panic. Fields of an unexpected type are
//! ignored; only a missing mod id makes a file unusable.

mod forge;
mod json;

pub use vl_core::resources::{DependencyKind, ModDependency, ModLoader, ModMetadata};
use vl_core::{Error, ErrorKind};

pub use forge::parse_mods_toml;
pub use json::{parse_fabric, parse_mcmod_info, parse_quilt};

/// Path of the Fabric metadata file in a jar.
pub const FABRIC_FILE: &str = "fabric.mod.json";
/// Path of the Quilt metadata file in a jar.
pub const QUILT_FILE: &str = "quilt.mod.json";
/// Path of the Forge metadata file in a jar.
pub const FORGE_FILE: &str = "META-INF/mods.toml";
/// Path of the NeoForge metadata file in a jar.
pub const NEOFORGE_FILE: &str = "META-INF/neoforge.mods.toml";
/// Path of the legacy Forge metadata file in a jar.
pub const MCMOD_FILE: &str = "mcmod.info";
/// Path of the jar manifest.
pub const MANIFEST_FILE: &str = "META-INF/MANIFEST.MF";

/// The metadata files in the order they are tried.
pub const METADATA_FILES: [(&str, ModLoader); 5] = [
    (FABRIC_FILE, ModLoader::Fabric),
    (QUILT_FILE, ModLoader::Quilt),
    (NEOFORGE_FILE, ModLoader::NeoForge),
    (FORGE_FILE, ModLoader::Forge),
    (MCMOD_FILE, ModLoader::LegacyForge),
];

/// Parses the metadata file at `path` (one of [`METADATA_FILES`]); `None` for any other path.
pub fn parse_file(path: &str, bytes: &[u8]) -> Option<Result<Vec<ModMetadata>, Error>> {
    let (_, loader) = METADATA_FILES.iter().find(|(p, _)| *p == path)?;
    Some(match loader {
        ModLoader::Fabric => parse_fabric(bytes).map(|m| vec![m]),
        ModLoader::Quilt => parse_quilt(bytes).map(|m| vec![m]),
        ModLoader::Forge | ModLoader::NeoForge => parse_mods_toml(bytes, *loader),
        _ => parse_mcmod_info(bytes),
    })
}

/// The `Implementation-Version` of a jar manifest, which Forge mods use through
/// `${file.jarVersion}`.
pub fn manifest_version(manifest: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(manifest);
    // Manifest lines wrap with a leading space; unfold them first.
    let unfolded = text.replace("\r\n ", "").replace("\n ", "");
    unfolded.lines().find_map(|line| {
        let value = line.strip_prefix("Implementation-Version:")?.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// Replaces the `${file.jarVersion}` placeholder in mods' versions with `version`.
pub fn resolve_jar_version(mods: &mut [ModMetadata], version: Option<&str>) {
    for m in mods {
        if m.version.as_deref() == Some("${file.jarVersion}") {
            m.version = version.map(str::to_owned);
        }
    }
}

pub(crate) fn damaged(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Integrity, format!("{what}: {detail}"))
}

pub(crate) fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
