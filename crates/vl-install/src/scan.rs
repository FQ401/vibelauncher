//! What is in a resource folder and which projects it belongs to.

use std::collections::HashMap;

use vl_core::Result;
use vl_core::catalog::{HashAlgorithm, ProjectVersion, Provider};
use vl_core::install::MatchedBy;
use vl_core::resources::ResourceKind;
use vl_net::Control;
use vl_platforms::CatalogSource;
use vl_resources::{DISABLED_SUFFIX, Resources};

use crate::hashing::{FileHashes, hash_file};
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
    pub hashes: FileHashes,
    /// Mod ids declared in the jar.
    pub mod_ids: Vec<String>,
    pub identity: Option<Identity>,
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
    let dir = resources.dir(kind);
    let mut files = Vec::new();
    for entry in resources.list(kind)? {
        if entry.is_dir {
            continue;
        }
        let on_disk = if entry.enabled {
            entry.file.clone()
        } else {
            format!("{}{DISABLED_SUFFIX}", entry.file)
        };
        let Ok(hashes) = hash_file(&dir.join(&on_disk)) else {
            continue;
        };
        files.push(InstalledFile {
            file: entry.file,
            hashes,
            mod_ids: entry.mods.iter().map(|m| m.id.clone()).collect(),
            identity: None,
        });
    }
    if files.is_empty() {
        return Ok(files);
    }
    let provider: Provider = source.provider();
    let hashes: Vec<String> = files.iter().map(|f| f.hashes.sha1.clone()).collect();
    let mut known: HashMap<String, ProjectVersion> = HashMap::new();
    for m in source.versions_by_hashes(HashAlgorithm::Sha1, &hashes, control)? {
        known.insert(m.hash.to_ascii_lowercase(), m.version);
    }
    for file in &mut files {
        let looked_up = known.remove(&file.hashes.sha1);
        let origin = origins
            .iter()
            .find(|o| o.provider == provider && o.kind == kind && o.sha1 == file.hashes.sha1);
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
    Ok(files)
}
