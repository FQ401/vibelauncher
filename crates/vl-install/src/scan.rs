//! What is in a resource folder and which projects it belongs to.

use std::collections::HashMap;
use std::path::Path;

use vl_core::Result;
use vl_core::catalog::{HashAlgorithm, ProjectVersion, Provider};
use vl_core::install::MatchedBy;
use vl_core::resources::ResourceKind;
use vl_net::Control;
use vl_platforms::CatalogSource;
use vl_resources::{Digests, Resources};

use crate::hashing::{FileHashes, fingerprint_file, hash_file};
use crate::origin::Origin;

/// The project and version a file is known to be.
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub project_id: String,
    pub version_id: String,
    pub version_number: Option<String>,
    /// The platform's description of the version, when it was looked up.
    pub version: Option<ProjectVersion>,
    pub matched_by: MatchedBy,
}

/// One file of a folder.
#[derive(Debug, Clone)]
pub(crate) struct InstalledFile {
    /// The name without `.disabled`.
    pub file: String,
    pub enabled: bool,
    /// The mod's name when the jar says one, else the file name.
    pub name: String,
    pub hashes: FileHashes,
    /// What the platform finds the file by ([`CatalogSource::file_hash`]): the SHA-1, or the
    /// CurseForge fingerprint in decimal.
    pub key: String,
    /// Mod ids declared in the jar.
    pub mod_ids: Vec<String>,
    pub identity: Option<Identity>,
    /// The platform of the file's origin record, when the record is of another platform than
    /// the one asked.
    pub foreign: Option<Provider>,
}

/// `Sodium-Extra` and `sodium_extra` compare equal.
pub(crate) fn normalize_id(id: &str) -> String {
    id.to_ascii_lowercase().replace('-', "_")
}

/// The files of `kind` with their identities: origin records first, then the platform's answer
/// for the hashes. A file that cannot be read is left out.
pub(crate) fn scan(
    resources: &Resources,
    kind: ResourceKind,
    origins: &[Origin],
    source: &dyn CatalogSource,
    control: &Control,
) -> Result<Vec<InstalledFile>> {
    Ok(scan_many(resources, &[kind], origins, source, control)?
        .remove(&kind)
        .unwrap_or_default())
}

/// [`scan`] for several folders with one batched lookup for all their files, by the hash the
/// platform finds files by.
pub(crate) fn scan_many(
    resources: &Resources,
    kinds: &[ResourceKind],
    origins: &[Origin],
    source: &dyn CatalogSource,
    control: &Control,
) -> Result<HashMap<ResourceKind, Vec<InstalledFile>>> {
    let algorithm = source.file_hash();
    let mut all: HashMap<ResourceKind, Vec<InstalledFile>> = HashMap::new();
    for &kind in kinds {
        all.insert(kind, list(resources, kind, algorithm)?);
    }
    let mut keys: Vec<String> = all.values().flatten().map(|f| f.key.clone()).collect();
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        return Ok(all);
    }
    let provider: Provider = source.provider();
    let mut known: HashMap<String, ProjectVersion> = HashMap::new();
    for m in source.versions_by_hashes(algorithm, &keys, control)? {
        known.insert(m.hash.to_ascii_lowercase(), m.version);
    }
    for (&kind, files) in &mut all {
        for file in files {
            let looked_up = known.get(&file.key).cloned();
            let recorded = |o: &&Origin| o.kind == kind && o.sha1 == file.hashes.sha1;
            let origin = origins
                .iter()
                .filter(recorded)
                .find(|o| o.provider == provider);
            file.foreign = match origin {
                Some(_) => None,
                None => origins.iter().find(recorded).map(|o| o.provider),
            };
            file.identity = match (looked_up, origin) {
                (Some(v), origin) => Some(Identity {
                    project_id: v.project_id.clone(),
                    version_id: v.id.clone(),
                    version_number: Some(v.version_number.clone()),
                    matched_by: if origin.is_some() {
                        MatchedBy::Origin
                    } else {
                        MatchedBy::Hash
                    },
                    version: Some(v),
                }),
                (None, Some(o)) => Some(Identity {
                    project_id: o.project_id.clone(),
                    version_id: o.version_id.clone(),
                    version_number: o.version_number.clone(),
                    version: None,
                    matched_by: MatchedBy::Origin,
                }),
                (None, None) => None,
            };
        }
    }
    Ok(all)
}

/// `known` with what is missing for `algorithm` computed from the file: the SHA digests and the
/// size always, the CurseForge fingerprint when it is the key. Nothing is read when nothing is
/// missing.
fn complete(path: &Path, known: &Digests, algorithm: HashAlgorithm) -> std::io::Result<Digests> {
    let mut digests = known.clone();
    if digests.sha1.is_none() || digests.sha512.is_none() || digests.size.is_none() {
        let hashes = hash_file(path)?;
        digests.sha1 = Some(hashes.sha1);
        digests.sha512 = Some(hashes.sha512);
        digests.size = Some(hashes.size);
    }
    if algorithm == HashAlgorithm::Fingerprint && digests.fingerprint.is_none() {
        digests.fingerprint = Some(fingerprint_file(path)?);
    }
    Ok(digests)
}

/// The hashes and the key of a file for `algorithm`, when all are known.
fn hashes_and_key(digests: Digests, algorithm: HashAlgorithm) -> Option<(FileHashes, String)> {
    let hashes = FileHashes {
        sha1: digests.sha1?,
        sha512: digests.sha512?,
        size: digests.size?,
    };
    let key = match algorithm {
        HashAlgorithm::Fingerprint => digests.fingerprint?.to_string(),
        HashAlgorithm::Sha512 => hashes.sha512.clone(),
        _ => hashes.sha1.clone(),
    };
    Some((hashes, key))
}

/// The readable files of a folder, without identities. Their digests come from the folder's
/// metadata cache while a file is unchanged, so a repeated scan reads no file.
fn list(
    resources: &Resources,
    kind: ResourceKind,
    algorithm: HashAlgorithm,
) -> Result<Vec<InstalledFile>> {
    let entries: Vec<_> = resources
        .list(kind)?
        .into_iter()
        .filter(|e| !e.is_dir)
        .collect();
    let names: Vec<(String, bool)> = entries
        .iter()
        .map(|e| (e.file.clone(), e.enabled))
        .collect();
    let digests = resources.digests(kind, &names, |path, known| complete(path, known, algorithm));
    let mut files = Vec::new();
    for (entry, digests) in entries.into_iter().zip(digests) {
        let Some((hashes, key)) = digests.ok().and_then(|d| hashes_and_key(d, algorithm)) else {
            continue;
        };
        files.push(InstalledFile {
            name: entry
                .mods
                .first()
                .map_or_else(|| entry.file.clone(), |m| m.name.clone()),
            enabled: entry.enabled,
            file: entry.file,
            hashes,
            key,
            mod_ids: entry.mods.iter().map(|m| m.id.clone()).collect(),
            identity: None,
            foreign: None,
        });
    }
    Ok(files)
}
