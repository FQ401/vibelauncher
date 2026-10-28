//! Installing a CurseForge modpack (a zip with `manifest.json`) into a fresh game directory.
//!
//! [`CfPack::open`] reads the archive and checks it before anything is written: the manifest
//! ([`vl_curseforge::parse_manifest`]), the number and names of the entries, and every entry in
//! the pack's override folder (a name that could leave the game directory or a symlink entry
//! refuses the whole pack). The manifest names its files by CurseForge project and file id
//! only, so [`resolve`] asks the API for all of them in batches (`POST /v1/mods/files`, then
//! `POST /v1/mods` for the projects' classes): the class decides the folder (mods `6` into
//! `mods/`, resource packs `12` into `resourcepacks/`, shaders `6552` into `shaderpacks/`, data
//! packs `6945` into `datapacks/`; a file whose project is unknown goes into `mods/`, other
//! classes are skipped and listed). Without a CurseForge key [`resolve`] fails with `NoApiKey`
//! before anything is sent.
//!
//! [`install`] downloads every file, checking the size and the SHA-1 CurseForge gives, places it
//! (a file the pack marks not required is installed disabled, as `<name>.disabled`), and then
//! extracts the override folder on top. A file the author does not allow other apps to
//! download has no download URL; it is fetched from CurseForge's CDN address instead, and when
//! that fails too it becomes a [`BlockedFile`] with the project's page, so the user can download
//! it by hand. Blocked files do not stop the install: the instance is made with everything else,
//! and the blocked files are listed in the [`Report`] and kept in `pack.json`. A file the API
//! does offer that cannot be downloaded or fails its checks fails the install.
//!
//! The directory is expected to be new: a failure may leave part of the pack in it, and the
//! caller removes the whole instance. [`fetch`] downloads the zip of a catalog modpack, and
//! [`record`] builds the pack record and the origin records of the installed files.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::catalog::{ProjectType, ProjectVersion, Provider, VersionFilter};
use vl_core::install::MinStability;
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_curseforge::manifest::{MANIFEST_NAME, MAX_MANIFEST_BYTES};
use vl_curseforge::{Manifest, ModFile, ModLoader, ModSummary, numeric_id, parse_manifest};
use vl_modrinth::mrpack::{INDEX_NAME, PackLoader, safe_path};
use vl_net::{Checksum, Control, Download};
use vl_platforms::{CatalogSource, CurseForgeSource};
use zip::ZipArchive;

use crate::mrpack::{
    InstallInput, MAX_ENTRIES, MAX_PACK_BYTES, OverrideRecord, PackFileRecord, PackKind,
    PackRecord, PackSourceRecord, RECORD_FORMAT, bad, check_cancel, extract_entries,
    extract_entries_to, hash_entries, io,
};
use crate::origin::{Origin, now_unix};

/// The largest file of a pack downloaded when CurseForge does not give its size.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Whether the archive at `path` is a CurseForge modpack: it has `manifest.json` at its root
/// and no `modrinth.index.json`.
///
/// # Errors
///
/// `Io` when the file cannot be read, `Integrity` when it is not a zip archive.
pub fn is_curseforge_pack(path: &Path) -> Result<bool> {
    let file = File::open(path).map_err(|e| io(path, &e))?;
    let archive = ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
    Ok(archive.index_for_name(MANIFEST_NAME).is_some()
        && archive.index_for_name(INDEX_NAME).is_none())
}

/// An opened and checked CurseForge modpack.
#[derive(Debug, Clone)]
pub struct CfPack {
    /// The manifest.
    pub manifest: Manifest,
    path: PathBuf,
    /// `(entry index, checked path in the game directory)` of each override file, in archive
    /// order.
    overrides: Vec<(usize, String)>,
}

impl CfPack {
    /// Opens the archive and checks it; nothing is extracted.
    ///
    /// # Errors
    ///
    /// `Io` when the file cannot be read, `Integrity` for a damaged archive, a missing or
    /// unusable manifest, too many entries or an unsafe override path, `Unsupported` for a
    /// manifest this launcher does not know.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| io(path, &e))?;
        let mut archive =
            ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
        if archive.len() > MAX_ENTRIES {
            return Err(bad(format!("more than {MAX_ENTRIES} entries")));
        }
        let manifest = {
            let mut entry = archive
                .by_name(MANIFEST_NAME)
                .map_err(|_| bad(format!("{MANIFEST_NAME} is missing")))?;
            let mut bytes = Vec::new();
            (&mut entry)
                .take(MAX_MANIFEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| bad(format!("{MANIFEST_NAME}: {e}")))?;
            parse_manifest(&bytes)?
        };
        let prefix = format!("{}/", manifest.overrides);
        let mut overrides = Vec::new();
        for i in 0..archive.len() {
            let entry = archive
                .by_index_raw(i)
                .map_err(|e| bad(format!("entry {i}: {e}")))?;
            let name = entry
                .name()
                .map_err(|e| bad(format!("entry {i}: {e}")))?
                .into_owned();
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            if entry.is_dir() || rest.is_empty() {
                if !rest.is_empty() && safe_path(rest.trim_end_matches('/')).is_none() {
                    return Err(bad(format!("unsafe path {name:?}")));
                }
                continue;
            }
            if entry
                .unix_mode()
                .is_some_and(|m| m & 0o170_000 == 0o120_000)
            {
                return Err(bad(format!("{name:?} is a symbolic link")));
            }
            let rel = safe_path(rest).ok_or_else(|| bad(format!("unsafe path {name:?}")))?;
            overrides.push((i, rel));
        }
        Ok(Self {
            manifest,
            path: path.to_path_buf(),
            overrides,
        })
    }

    /// The Minecraft version.
    pub fn game_version(&self) -> &str {
        &self.manifest.minecraft_version
    }

    /// The pack's loader and its version, `None` for a vanilla pack.
    ///
    /// # Errors
    ///
    /// `Unsupported` for a loader the launcher cannot install (Cauldron, LiteLoader) or an id
    /// it cannot read.
    pub fn loader(&self) -> Result<Option<(PackLoader, String)>> {
        let Some((loader, version)) = self.manifest.loader()? else {
            return Ok(None);
        };
        let kind = match loader {
            ModLoader::Forge => PackLoader::Forge,
            ModLoader::NeoForge => PackLoader::NeoForge,
            ModLoader::Fabric => PackLoader::Fabric,
            ModLoader::Quilt => PackLoader::Quilt,
            other => {
                return Err(Error::unsupported(format!(
                    "the mod loader {} of the modpack",
                    other.name()
                )));
            }
        };
        Ok(Some((kind, version)))
    }

    /// The number of files in the override folder.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// The SHA-1 of every override file as an install writes it, by path.
    ///
    /// # Errors
    ///
    /// `Io` or `Integrity` when the archive cannot be read, `Integrity` past the byte budget.
    pub fn override_hashes(&self) -> Result<Vec<OverrideRecord>> {
        hash_entries(&self.path, self.final_overrides())
    }

    /// The archive entry of each override path; a later copy of the same path wins.
    fn final_overrides(&self) -> std::collections::BTreeMap<&str, usize> {
        self.overrides
            .iter()
            .map(|(i, rel)| (rel.as_str(), *i))
            .collect()
    }

    /// Extracts the named override files, each to its own destination.
    ///
    /// # Errors
    ///
    /// `NotFound` for a path the pack does not carry, `Cancelled`, `Io`, `Integrity`.
    pub(crate) fn extract_overrides_to(
        &self,
        wanted: &[(String, PathBuf)],
        control: &Control,
    ) -> Result<()> {
        extract_entries_to(&self.path, &self.final_overrides(), wanted, control)
    }
}

/// Why a file of the pack was not downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// The author does not allow downloads through other apps, and the CDN address failed.
    DistributionDisabled,
    /// CurseForge does not know the file (deleted or hidden).
    NotFound,
    /// The file's name cannot be used as a file name.
    UnusableName,
}

impl BlockReason {
    /// A sentence for the user.
    pub fn text(self) -> &'static str {
        match self {
            Self::DistributionDisabled => "the author does not allow downloads through other apps",
            Self::NotFound => "CurseForge no longer offers the file",
            Self::UnusableName => "the file name cannot be used",
        }
    }
}

/// A file of the pack that the user has to download by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedFile {
    /// The CurseForge project id.
    pub project_id: u64,
    /// The CurseForge file id.
    pub file_id: u64,
    /// The project's name (the file id when unknown).
    pub name: String,
    /// The file's name when known.
    #[serde(default)]
    pub file_name: String,
    /// The folder of the game directory the file belongs in (`mods`, `resourcepacks`, ...).
    pub folder: String,
    /// The project's page on the CurseForge website.
    pub page_url: String,
    /// The page of this exact file, when the project's address is known.
    #[serde(default)]
    pub file_url: Option<String>,
    /// Whether the pack needs it (an optional file would be installed disabled).
    pub required: bool,
    /// Why it was not downloaded.
    pub reason: BlockReason,
}

impl BlockedFile {
    /// One line for the user: what is missing, why, and where to get it.
    pub fn describe(&self) -> String {
        let file = if self.file_name.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.file_name)
        };
        let target = if self.required {
            format!("{}/", self.folder)
        } else {
            format!(
                "{}/ (an optional file; add .disabled to keep it off)",
                self.folder
            )
        };
        format!(
            "not downloaded: {}{file}, {}; download it from {} and put it into {target}",
            self.name,
            self.reason.text(),
            self.file_url.as_deref().unwrap_or(&self.page_url),
        )
    }
}

/// A file of the pack with what the API says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// The CurseForge project id.
    pub project_id: u64,
    /// The CurseForge file id.
    pub file_id: u64,
    /// The folder.
    pub kind: ResourceKind,
    /// The file name (without `.disabled`).
    pub file_name: String,
    /// Whether the pack needs it; an optional file is installed disabled.
    pub required: bool,
    /// Size in bytes, 0 when unknown.
    pub size: u64,
    /// Lowercase hex SHA-1, when CurseForge gives one.
    pub sha1: Option<String>,
    /// The download URL the API gives; `None` when the author does not allow it.
    pub url: Option<String>,
    /// The CDN address built from the id and name.
    pub fallback: String,
    /// The project's name.
    pub project_name: String,
    /// The project's slug.
    pub slug: Option<String>,
    /// The file's display name.
    pub display_name: String,
    /// The project's page.
    pub page_url: String,
    /// The project's website address, when known.
    pub website_url: Option<String>,
}

impl PlannedFile {
    /// The path in the game directory as installed, `/`-separated.
    pub fn path(&self) -> String {
        let disabled = if self.required { "" } else { ".disabled" };
        format!("{}/{}{disabled}", self.kind.dir(), self.file_name)
    }

    pub(crate) fn blocked(&self, reason: BlockReason) -> BlockedFile {
        BlockedFile {
            project_id: self.project_id,
            file_id: self.file_id,
            name: self.project_name.clone(),
            file_name: self.file_name.clone(),
            folder: self.kind.dir().to_owned(),
            page_url: self.page_url.clone(),
            file_url: self
                .website_url
                .as_ref()
                .map(|w| format!("{}/files/{}", w.trim_end_matches('/'), self.file_id)),
            required: self.required,
            reason,
        }
    }
}

/// A file of a project class the launcher does not place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    /// The project's name.
    pub name: String,
    /// The file's name.
    pub file_name: String,
    /// The project's class id.
    pub class_id: u64,
}

/// What [`resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    /// The files to download, in the manifest's order.
    pub files: Vec<PlannedFile>,
    /// Files that can be known now not to be downloadable.
    pub blocked: Vec<BlockedFile>,
    /// Files of classes that are not placed.
    pub skipped: Vec<SkippedFile>,
}

/// The folder of a CurseForge class; `None` for classes the launcher does not place.
fn folder_of(class_id: u64) -> Option<ResourceKind> {
    match vl_curseforge::project_type(class_id) {
        ProjectType::Mod => Some(ResourceKind::Mods),
        ProjectType::ResourcePack => Some(ResourceKind::ResourcePacks),
        ProjectType::Shader => Some(ResourceKind::ShaderPacks),
        ProjectType::DataPack => Some(ResourceKind::DataPacks),
        _ => None,
    }
}

/// Asks CurseForge for every file of the pack and its project, in batches.
///
/// # Errors
///
/// `NoApiKey` before anything is sent when no key is set, `Auth` for a refused key, `Cancelled`,
/// and the errors of the network.
pub fn resolve(source: &CurseForgeSource, pack: &CfPack, control: &Control) -> Result<Resolved> {
    if !source.has_key()? {
        return Err(vl_curseforge::no_api_key());
    }
    let wanted = &pack.manifest.files;
    if wanted.is_empty() {
        return Ok(Resolved::default());
    }
    let file_ids: Vec<u64> = wanted.iter().map(|f| f.file_id).collect();
    let files: HashMap<u64, ModFile> = source
        .files_by_ids(&file_ids, control)?
        .into_iter()
        .map(|f| (f.id, f))
        .collect();
    check_cancel(control)?;
    let mod_ids: Vec<u64> = wanted.iter().map(|f| f.project_id).collect();
    let mods: HashMap<u64, ModSummary> = source
        .mods_by_ids(&mod_ids, control)?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    let mut resolved = Resolved::default();
    let mut paths = HashSet::new();
    for want in wanted {
        let project = mods.get(&want.project_id);
        let name = project.map_or_else(
            || format!("CurseForge project {}", want.project_id),
            |m| m.name.clone(),
        );
        let page_url = project.map_or_else(
            || vl_curseforge::project_page_url(want.project_id),
            ModSummary::page_url,
        );
        let class_id = project.map_or(0, |m| m.class_id);
        let kind = if class_id == 0 {
            Some(ResourceKind::Mods)
        } else {
            folder_of(class_id)
        };
        let Some(file) = files.get(&want.file_id) else {
            resolved.blocked.push(BlockedFile {
                project_id: want.project_id,
                file_id: want.file_id,
                name,
                file_name: String::new(),
                folder: kind.unwrap_or(ResourceKind::Mods).dir().to_owned(),
                page_url,
                file_url: None,
                required: want.required,
                reason: BlockReason::NotFound,
            });
            continue;
        };
        let Some(kind) = kind else {
            resolved.skipped.push(SkippedFile {
                name,
                file_name: file.file_name.clone(),
                class_id,
            });
            continue;
        };
        let planned = |file_name: &str, fallback: String| PlannedFile {
            project_id: want.project_id,
            file_id: want.file_id,
            kind,
            file_name: file_name.to_owned(),
            required: want.required,
            size: file.size,
            sha1: file.sha1.clone(),
            url: file.download_url.clone(),
            fallback,
            project_name: name.clone(),
            slug: project.map(|m| m.slug.clone()).filter(|s| !s.is_empty()),
            display_name: file.display_name.clone(),
            page_url: page_url.clone(),
            website_url: project.and_then(|m| m.website_url.clone()),
        };
        let usable = file
            .safe_file_name()
            .filter(|n| safe_path(n).is_some())
            .zip(file.edge_url());
        match usable {
            Some((file_name, fallback)) => {
                let plan = planned(file_name, fallback);
                // Two files with one name: the first is kept.
                if paths.insert(plan.path().to_ascii_lowercase()) {
                    resolved.files.push(plan);
                }
            }
            None => resolved
                .blocked
                .push(planned(&file.file_name, String::new()).blocked(BlockReason::UnusableName)),
        }
    }
    Ok(resolved)
}

/// One installed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFile {
    /// What was planned.
    pub file: PlannedFile,
    /// Lowercase hex SHA-1 of the installed file.
    pub sha1: String,
    /// Lowercase hex SHA-512 of the installed file.
    pub sha512: String,
    /// Size in bytes.
    pub size: u64,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The downloaded files that were placed.
    pub installed: Vec<InstalledFile>,
    /// Files left to the user, with where to get them.
    pub blocked: Vec<BlockedFile>,
    /// Files of classes that are not placed.
    pub skipped: Vec<SkippedFile>,
    /// Paths of files installed without a SHA-1 to check (CurseForge gave none).
    pub unverified: Vec<String>,
    /// Files extracted from the override folder.
    pub overrides: usize,
}

impl Report {
    /// Lines for the user about what needs attention.
    pub fn notes(&self) -> Vec<String> {
        let mut notes: Vec<String> = self.blocked.iter().map(BlockedFile::describe).collect();
        for s in &self.skipped {
            notes.push(format!(
                "skipped {} ({}): CurseForge class {} is not installed by the launcher",
                s.name, s.file_name, s.class_id
            ));
        }
        let disabled: Vec<&str> = self
            .installed
            .iter()
            .filter(|f| !f.file.required)
            .map(|f| f.file.file_name.as_str())
            .collect();
        if !disabled.is_empty() {
            notes.push(format!(
                "installed the optional files of the pack disabled: {}",
                disabled.join(", ")
            ));
        }
        if !self.unverified.is_empty() {
            notes.push(format!(
                "CurseForge gave no SHA-1 for: {}; only their size was checked",
                self.unverified.join(", ")
            ));
        }
        notes
    }
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn spec(file: &PlannedFile, url: &str, dest: &Path) -> Download {
    let mut spec = Download::new(url, dest);
    spec = if file.size > 0 {
        spec.size(file.size)
    } else {
        spec.max_bytes(MAX_FILE_BYTES)
    };
    if let Some(sum) = file.sha1.as_deref().and_then(Checksum::sha1) {
        spec = spec.checksum(sum);
    }
    spec
}

/// The addresses of a file, in order: the API's URL, then the CDN address.
fn urls(file: &PlannedFile) -> Vec<&str> {
    let mut urls: Vec<&str> = file.url.iter().map(String::as_str).collect();
    if !file.fallback.is_empty() && !urls.contains(&file.fallback.as_str()) {
        urls.push(&file.fallback);
    }
    urls
}

/// Downloads the resolved files and extracts the overrides into `game_dir`.
///
/// # Errors
///
/// `Network` or `Integrity` for a file the API offers that cannot be fetched or fails its
/// checks, `Integrity` for a damaged archive, `Cancelled`, `Io`.
pub fn install(pack: &CfPack, resolved: &Resolved, input: &InstallInput<'_>) -> Result<Report> {
    let mut report = Report {
        blocked: resolved.blocked.clone(),
        skipped: resolved.skipped.clone(),
        ..Report::default()
    };
    fs::create_dir_all(input.game_dir).map_err(|e| io(input.game_dir, &e))?;
    let _staging = Staging(input.staging.to_path_buf());
    fs::create_dir_all(input.staging).map_err(|e| io(input.staging, &e))?;

    let files = &resolved.files;
    let dests: Vec<PathBuf> = (0..files.len())
        .map(|n| input.staging.join(n.to_string()).join("file"))
        .collect();
    let specs: Vec<Download> = files
        .iter()
        .zip(&dests)
        .filter_map(|(f, d)| Some(spec(f, urls(f).first()?, d)))
        .collect();
    let results = input.client.download_all(&specs, input.control);
    let mut fetched = vec![true; files.len()];
    for (n, result) in results.into_iter().enumerate() {
        let Err(first) = result else { continue };
        check_cancel(input.control)?;
        let (Some(file), Some(dest)) = (files.get(n), dests.get(n)) else {
            continue;
        };
        let mut last = Error::from(first);
        if last.kind == ErrorKind::Cancelled {
            return Err(last);
        }
        let mut done = false;
        for url in urls(file).into_iter().skip(1) {
            match input.client.download(&spec(file, url, dest), input.control) {
                Ok(_) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    last = Error::from(e);
                    if last.kind == ErrorKind::Cancelled {
                        return Err(last);
                    }
                }
            }
        }
        if done {
            continue;
        }
        if file.url.is_none() {
            fetched[n] = false;
            report
                .blocked
                .push(file.blocked(BlockReason::DistributionDisabled));
        } else {
            return Err(Error::new(
                last.kind,
                format!("{}: {}", file.file_name, last.message),
            ));
        }
    }

    for ((file, source), ok) in files.iter().zip(&dests).zip(fetched) {
        check_cancel(input.control)?;
        if !ok {
            continue;
        }
        let rel = file.path();
        let target = input.game_dir.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
        }
        fs::rename(source, &target).map_err(|e| io(&target, &e))?;
        let hashes = crate::hashing::hash_file(&target).map_err(|e| io(&target, &e))?;
        if file.sha1.is_none() {
            report.unverified.push(rel);
        }
        report.installed.push(InstalledFile {
            file: file.clone(),
            sha1: hashes.sha1,
            sha512: hashes.sha512,
            size: hashes.size,
        });
    }
    let entries: Vec<(usize, &str)> = pack
        .overrides
        .iter()
        .map(|(i, rel)| (*i, rel.as_str()))
        .collect();
    report.overrides = extract_entries(&pack.path, &entries, input.game_dir, input.control)?;
    Ok(report)
}

/// The pack record (`pack.json`) and the origin records (`resources.json`) of an install.
///
/// Every installed file in a resource folder gets an [`Origin`] with its CurseForge project and
/// file id, so update checks know where it came from.
///
/// # Errors
///
/// `Io` or `Integrity` when the override files cannot be hashed.
pub fn record(
    pack: &CfPack,
    report: &Report,
    source: Option<PackSourceRecord>,
) -> Result<(PackRecord, Vec<Origin>)> {
    let now = now_unix();
    let loader = pack.loader()?.map(|(l, v)| (l.key().to_owned(), v));
    let origins = report
        .installed
        .iter()
        .map(|f| Origin {
            kind: f.file.kind,
            file: f.file.file_name.clone(),
            sha1: f.sha1.clone(),
            sha512: Some(f.sha512.clone()),
            size: f.size,
            provider: Provider::CurseForge,
            project_id: f.file.project_id.to_string(),
            slug: f.file.slug.clone(),
            version_id: f.file.file_id.to_string(),
            version_number: Some(f.file.display_name.clone()).filter(|d| !d.is_empty()),
            installed_unix: now,
            dependency: false,
        })
        .collect();
    let record = PackRecord {
        format: RECORD_FORMAT,
        name: pack.manifest.name.clone(),
        pack_version: pack.manifest.version.clone(),
        source,
        game_version: pack.manifest.minecraft_version.clone(),
        loader,
        installed_unix: now,
        files: report
            .installed
            .iter()
            .map(|f| PackFileRecord {
                path: format!("{}/{}", f.file.kind.dir(), f.file.file_name),
                sha1: f.sha1.clone(),
                sha512: f.sha512.clone(),
                size: f.size,
                optional: !f.file.required,
                origin_known: true,
            })
            .collect(),
        overrides: pack.override_hashes()?,
        kind: PackKind::CurseForge,
        blocked: report.blocked.clone(),
    };
    Ok((record, origins))
}

fn is_zip(v: &ProjectVersion) -> bool {
    v.files
        .iter()
        .any(|f| f.filename.to_ascii_lowercase().ends_with(".zip"))
}

/// Downloads the zip of a CurseForge modpack project into `dest_dir`.
///
/// `file` is a file id; without it the most stable allowed channel wins, then the newest.
/// Returns the file and where it came from.
///
/// # Errors
///
/// `NoApiKey` before anything is sent when no key is set, `Unsupported` when the project is not
/// a modpack, `NotFound` when no file (or the pinned one) is a zip of that project, `Integrity`
/// for a file without a SHA-1, plus the errors of the network.
pub fn fetch(
    source: &CurseForgeSource,
    client: &vl_net::Client,
    project: &str,
    file: Option<&str>,
    min: MinStability,
    dest_dir: &Path,
    control: &Control,
) -> Result<(PathBuf, PackSourceRecord)> {
    let info = source.project(project, control)?;
    if info.project_type != ProjectType::Modpack {
        return Err(Error::unsupported(format!(
            "{} is not a modpack",
            info.title
        )));
    }
    let chosen = match file {
        Some(id) => {
            let id = numeric_id(id).ok_or_else(|| {
                Error::invalid_input(format!("{id:?} is not a CurseForge file id"))
            })?;
            let version = source.file(&info.id, id, control)?;
            if version.project_id != info.id {
                return Err(Error::not_found(format!(
                    "file {id} does not belong to {}",
                    info.title
                )));
            }
            version
        }
        None => {
            let versions = source.versions(&info.id, &VersionFilter::default(), control)?;
            let mut best: Option<(u8, &ProjectVersion)> = None;
            for v in &versions {
                let Some(rank) = min.rank(v.channel) else {
                    continue;
                };
                if is_zip(v) && best.is_none_or(|(r, _)| rank > r) {
                    best = Some((rank, v));
                }
            }
            best.map(|(_, v)| v.clone())
                .ok_or_else(|| Error::not_found(format!("no installable file of {}", info.title)))?
        }
    };
    let pack_file = chosen
        .files
        .iter()
        .find(|f| f.filename.to_ascii_lowercase().ends_with(".zip"))
        .ok_or_else(|| {
            Error::not_found(format!("{} is not a modpack zip", chosen.version_number))
        })?;
    let checksum = pack_file
        .sha1
        .as_deref()
        .and_then(Checksum::sha1)
        .ok_or_else(|| Error::new(ErrorKind::Integrity, "the pack file has no usable hash"))?;
    fs::create_dir_all(dest_dir).map_err(|e| io(dest_dir, &e))?;
    let dest = dest_dir.join("pack.zip");
    let mut spec = Download::new(&pack_file.url, &dest)
        .checksum(checksum)
        .max_bytes(MAX_PACK_BYTES);
    if pack_file.size > 0 {
        spec = spec.size(pack_file.size);
    }
    client.download(&spec, control)?;
    Ok((
        dest,
        PackSourceRecord {
            provider: Provider::CurseForge,
            project_id: info.id,
            slug: info.slug,
            version_id: chosen.id.clone(),
            version_number: chosen.version_number.clone(),
        },
    ))
}
