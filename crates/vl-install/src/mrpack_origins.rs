//! Origin records for the files of an installed modpack.
//!
//! The files of a pack come from the pack's index, which names URLs and hashes but not the
//! catalog projects. [`resolve`] looks the installed files up by hash (one batched request per
//! algorithm) so each file that is a catalog file gets the same [`Origin`] record that an install
//! from the catalog writes, which update checks read. A file the catalog does not know, or one
//! outside the resource folders, has no record and stays marked as "from the pack, origin
//! unknown" in `pack.json`.

use std::collections::{HashMap, HashSet};

use vl_core::catalog::{HashAlgorithm, HashMatch};
use vl_core::resources::ResourceKind;
use vl_core::{ErrorKind, Result};
use vl_net::Control;
use vl_platforms::CatalogSource;

use crate::mrpack::{Pack, Report};
use crate::origin::{Origin, now_unix};

/// What [`resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    /// Records for the files the catalog knows.
    pub origins: Vec<Origin>,
    /// Paths of installed files with no known origin.
    pub unknown: Vec<String>,
    /// The lookup failed (network); every file counts as unknown.
    pub lookup_failed: bool,
}

/// The folder kind and file name of a path that sits directly in a resource folder.
fn kind_and_name(path: &str) -> Option<(ResourceKind, &str)> {
    ResourceKind::ALL.into_iter().find_map(|kind| {
        let rest = path.strip_prefix(kind.dir())?.strip_prefix('/')?;
        (!rest.is_empty() && !rest.contains('/')).then_some((kind, rest))
    })
}

fn lookup(
    source: &dyn CatalogSource,
    algorithm: HashAlgorithm,
    hashes: Vec<String>,
    control: &Control,
) -> Result<HashMap<String, HashMatch>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(source
        .versions_by_hashes(algorithm, &hashes, control)?
        .into_iter()
        .map(|m| (m.hash.to_ascii_lowercase(), m))
        .collect())
}

/// Looks the installed files of `pack` up in the catalog: by SHA-512 first, then the rest by
/// SHA-1. A failed lookup is not an error (the pack is installed; its files are just unknown);
/// only cancellation is.
///
/// # Errors
///
/// `Cancelled`.
pub fn resolve(
    source: &dyn CatalogSource,
    pack: &Pack,
    report: &Report,
    control: &Control,
) -> Result<Resolved> {
    let installed: HashSet<&str> = report.installed.iter().map(String::as_str).collect();
    let files: Vec<_> = pack
        .index
        .files
        .iter()
        .filter(|f| installed.contains(f.path.as_str()))
        .collect();
    let mut resolved = Resolved::default();
    let by_512 = lookup(
        source,
        HashAlgorithm::Sha512,
        files
            .iter()
            .filter(|f| HashAlgorithm::Sha512.is_valid(&f.sha512))
            .map(|f| f.sha512.to_ascii_lowercase())
            .collect(),
        control,
    );
    let by_512 = match by_512 {
        Ok(found) => found,
        Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
        Err(_) => {
            resolved.lookup_failed = true;
            resolved.unknown = files.iter().map(|f| f.path.clone()).collect();
            return Ok(resolved);
        }
    };
    let rest = |f: &&&vl_modrinth::mrpack::PackFile| {
        !by_512.contains_key(&f.sha512.to_ascii_lowercase())
            && HashAlgorithm::Sha1.is_valid(&f.sha1)
    };
    let by_1 = match lookup(
        source,
        HashAlgorithm::Sha1,
        files
            .iter()
            .filter(rest)
            .map(|f| f.sha1.to_ascii_lowercase())
            .collect(),
        control,
    ) {
        Ok(found) => found,
        Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
        Err(_) => {
            resolved.lookup_failed = true;
            HashMap::new()
        }
    };
    let now = now_unix();
    for f in files {
        let found = by_512
            .get(&f.sha512.to_ascii_lowercase())
            .or_else(|| by_1.get(&f.sha1.to_ascii_lowercase()));
        match (found, kind_and_name(&f.path)) {
            (Some(m), Some((kind, name))) => resolved.origins.push(Origin {
                kind,
                file: name.to_owned(),
                sha1: f.sha1.to_ascii_lowercase(),
                sha512: Some(f.sha512.to_ascii_lowercase()),
                size: f.size,
                provider: source.provider(),
                project_id: m.version.project_id.clone(),
                slug: None,
                version_id: m.version.id.clone(),
                version_number: Some(m.version.version_number.clone()),
                installed_unix: now,
                dependency: false,
            }),
            _ => resolved.unknown.push(f.path.clone()),
        }
    }
    Ok(resolved)
}
