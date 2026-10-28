//! The CurseForge side of a modpack update.
//!
//! A CurseForge manifest names its files by project and file id only; [`CfNew::new`] turns the
//! files [`crate::cfpack::resolve`] found into the same list of files with paths, hashes, sizes
//! and URLs that a Modrinth index gives, so [`crate::mrpack_update`] compares and applies both
//! formats the same way. What only CurseForge has is kept here:
//!
//! - a file the manifest marks not required is installed disabled, and is added disabled;
//! - a file without a download URL from the API is fetched from the CDN address, and when that
//!   fails it is left to the user like at install ([`crate::cfpack::BlockedFile`]); so is a file
//!   CurseForge no longer has or whose name is unusable. The old file of the same project then
//!   stays in the folder instead of being removed;
//! - the origin of every new file is its project and file id, known without a lookup.

use std::collections::{HashMap, HashSet};

use vl_core::catalog::Provider;
use vl_modrinth::mrpack::{PackFile, Support};

use crate::cfpack::{BlockReason, BlockedFile, CfPack, PlannedFile, Resolved};
use crate::mrpack::PackRecord;
use crate::origin::{Origin, now_unix};

/// A resolved CurseForge pack, as an update needs it.
#[derive(Debug, Clone)]
pub struct CfNew {
    /// The opened pack.
    pub pack: CfPack,
    /// What the API said about its files.
    pub resolved: Resolved,
    /// The files to place, as pack files (paths without `.disabled`).
    files: Vec<PackFile>,
}

impl CfNew {
    /// The pack with its resolved files. A file the API gives no SHA-1 for takes the baseline's
    /// hash when the baseline has the same path (the same file name is the same file on
    /// CurseForge), so an unchanged file is not downloaded again.
    pub fn new(pack: CfPack, resolved: Resolved, baseline: &PackRecord) -> Self {
        let old: HashMap<&str, &str> = baseline
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.sha1.as_str()))
            .collect();
        let files = resolved
            .files
            .iter()
            .map(|f| {
                let path = path(f);
                let sha1 = f
                    .sha1
                    .clone()
                    .or_else(|| old.get(path.as_str()).map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                let mut downloads: Vec<String> = f.url.iter().cloned().collect();
                if !f.fallback.is_empty() && !downloads.contains(&f.fallback) {
                    downloads.push(f.fallback.clone());
                }
                PackFile {
                    path,
                    sha1,
                    sha512: String::new(),
                    client: if f.required {
                        Support::Required
                    } else {
                        Support::Optional
                    },
                    server: Support::Required,
                    downloads,
                    size: f.size,
                }
            })
            .collect();
        Self {
            pack,
            resolved,
            files,
        }
    }

    /// The files to place.
    pub fn files(&self) -> impl Iterator<Item = &PackFile> {
        self.files.iter()
    }

    /// Paths of the files that are installed disabled (not required by the pack).
    pub(crate) fn optional(&self) -> HashSet<String> {
        self.resolved
            .files
            .iter()
            .filter(|f| !f.required)
            .map(path)
            .collect()
    }

    /// Paths of the files the API gives no download URL for: a failed download leaves them to
    /// the user instead of failing the update.
    pub(crate) fn may_fail(&self) -> HashSet<String> {
        self.resolved
            .files
            .iter()
            .filter(|f| f.url.is_none())
            .map(path)
            .collect()
    }

    /// Path in the game directory to the project id of every file to place.
    pub(crate) fn projects(&self) -> HashMap<String, String> {
        self.resolved
            .files
            .iter()
            .map(|f| (path(f), f.project_id.to_string()))
            .collect()
    }

    /// Projects whose new file is known now not to be downloadable.
    pub(crate) fn blocked_projects(&self) -> HashSet<String> {
        self.resolved
            .blocked
            .iter()
            .map(|b| b.project_id.to_string())
            .collect()
    }

    /// The files left to the user: those known now, then those of `failed` (paths whose
    /// download failed).
    pub(crate) fn blocked(&self, failed: &HashSet<String>) -> Vec<BlockedFile> {
        let mut out = self.resolved.blocked.clone();
        out.extend(
            self.resolved
                .files
                .iter()
                .filter(|f| failed.contains(&path(f)))
                .map(|f| f.blocked(BlockReason::DistributionDisabled)),
        );
        out
    }

    /// Lines for the user about what the update cannot do by itself.
    pub(crate) fn notes(&self) -> Vec<String> {
        let mut notes: Vec<String> = self
            .resolved
            .blocked
            .iter()
            .map(BlockedFile::describe)
            .collect();
        for s in &self.resolved.skipped {
            notes.push(format!(
                "skipped {} ({}): CurseForge class {} is not installed by the launcher",
                s.name, s.file_name, s.class_id
            ));
        }
        let fallback: Vec<&str> = self
            .resolved
            .files
            .iter()
            .filter(|f| f.url.is_none())
            .map(|f| f.file_name.as_str())
            .collect();
        if !fallback.is_empty() {
            notes.push(format!(
                "the authors of {} do not allow downloads through other apps; the launcher tries \
                 CurseForge's CDN and lists what fails",
                fallback.join(", ")
            ));
        }
        notes
    }

    /// The origin records of the files with these paths. The hashes are those CurseForge gives;
    /// the caller sets them from the placed files.
    pub(crate) fn origins(&self, paths: &HashSet<String>) -> Vec<Origin> {
        let now = now_unix();
        self.resolved
            .files
            .iter()
            .filter(|f| paths.contains(&path(f)))
            .map(|f| Origin {
                kind: f.kind,
                file: f.file_name.clone(),
                sha1: f.sha1.clone().unwrap_or_default(),
                sha512: None,
                size: f.size,
                provider: Provider::CurseForge,
                project_id: f.project_id.to_string(),
                slug: f.slug.clone(),
                version_id: f.file_id.to_string(),
                version_number: Some(f.display_name.clone()).filter(|d| !d.is_empty()),
                installed_unix: now,
                dependency: false,
            })
            .collect()
    }
}

/// The path of a planned file in the game directory, without `.disabled`.
fn path(f: &PlannedFile) -> String {
    format!("{}/{}", f.kind.dir(), f.file_name)
}
