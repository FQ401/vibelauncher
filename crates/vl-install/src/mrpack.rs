//! Installing a Modrinth modpack (`.mrpack`) into a fresh game directory.
//!
//! [`Pack::open`] reads the archive and checks it before anything is written: the index
//! ([`vl_modrinth::mrpack::parse_index`]), the number and names of the entries, and every entry
//! under `overrides/` and `client-overrides/` (a name that could leave the game directory, such
//! as `..`, an absolute or drive-qualified path, or a symlink entry, refuses the whole pack).
//! [`install`] then downloads the files the client needs (those whose `env.client` is not
//! `unsupported`; `optional` files are installed too and listed in the [`Report`]), checking size
//! and SHA-512 against the index, places them, and extracts `overrides/` followed by
//! `client-overrides/` on top. Extraction is capped in entries and in bytes actually written,
//! whatever the archive declares.
//!
//! The directory is expected to be new: a failure may leave part of the pack in it, and the
//! caller removes the whole instance. [`fetch`] downloads the `.mrpack` of a catalog project, and
//! [`PackRecord`] is the origin of the pack kept in `<instance>/.vibelauncher/pack.json` for
//! later updates.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::catalog::{ProjectType, Provider, VersionFilter};
use vl_core::install::MinStability;
use vl_core::{Error, ErrorKind, Result};
use vl_modrinth::mrpack::{
    INDEX_NAME, MAX_INDEX_BYTES, PackFile, PackIndex, Support, parse_index, safe_path,
};
use vl_net::{Checksum, Client, Control, Download};
use vl_platforms::CatalogSource;
use zip::ZipArchive;

use crate::origin::now_unix;

/// File name of the pack record inside `<instance>/.vibelauncher/`.
pub const PACK_RECORD: &str = "pack.json";
/// The format of the pack record this launcher writes.
pub const RECORD_FORMAT: u32 = 1;
/// Most entries accepted in one archive.
pub const MAX_ENTRIES: usize = 50_000;
/// Most bytes extracted from `overrides/` and `client-overrides/` together.
pub const MAX_OVERRIDE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Largest `.mrpack` fetched from a catalog.
pub const MAX_PACK_BYTES: u64 = 1024 * 1024 * 1024;

fn io(path: &Path, e: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

fn bad(why: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Integrity, format!("unusable modpack: {why}"))
}

/// One file inside `overrides/` or `client-overrides/`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverrideEntry {
    /// Index of the entry in the archive.
    index: usize,
    /// Checked path relative to the game directory.
    rel: String,
    /// Under `client-overrides/` (applied after the plain overrides).
    client: bool,
}

/// An opened and checked `.mrpack`.
#[derive(Debug, Clone)]
pub struct Pack {
    /// The index.
    pub index: PackIndex,
    path: PathBuf,
    overrides: Vec<OverrideEntry>,
}

impl Pack {
    /// Opens the archive and checks it; nothing is extracted.
    ///
    /// # Errors
    ///
    /// `Io` when the file cannot be read, `Integrity` for a damaged archive, a missing or
    /// unusable index, too many entries or an unsafe override path, `Unsupported` for a format
    /// or game this launcher does not know.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| io(path, &e))?;
        let mut archive =
            ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
        if archive.len() > MAX_ENTRIES {
            return Err(bad(format!("more than {MAX_ENTRIES} entries")));
        }
        let index = {
            let mut entry = archive
                .by_name(INDEX_NAME)
                .map_err(|_| bad(format!("{INDEX_NAME} is missing")))?;
            let mut bytes = Vec::new();
            (&mut entry)
                .take(MAX_INDEX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| bad(format!("{INDEX_NAME}: {e}")))?;
            parse_index(&bytes)?
        };
        let mut overrides = Vec::new();
        for i in 0..archive.len() {
            let entry = archive
                .by_index_raw(i)
                .map_err(|e| bad(format!("entry {i}: {e}")))?;
            let name = entry
                .name()
                .map_err(|e| bad(format!("entry {i}: {e}")))?
                .into_owned();
            let (client, rest) = if let Some(r) = name.strip_prefix("client-overrides/") {
                (true, r)
            } else if let Some(r) = name.strip_prefix("overrides/") {
                (false, r)
            } else {
                continue;
            };
            if entry.is_dir() || rest.is_empty() {
                // A folder entry; its files carry their own paths.
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
            overrides.push(OverrideEntry {
                index: i,
                rel,
                client,
            });
        }
        // A later copy of the same path wins, so the order is: plain first, client after.
        overrides.sort_by_key(|o| (o.client, o.index));
        Ok(Self {
            index,
            path: path.to_path_buf(),
            overrides,
        })
    }

    /// Files the client installs: those not marked unsupported on the client.
    pub fn client_files(&self) -> impl Iterator<Item = &PackFile> {
        self.index
            .files
            .iter()
            .filter(|f| f.client != Support::Unsupported)
    }

    /// The number of files in the override folders.
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

/// Where an install writes.
pub struct InstallInput<'a> {
    /// Downloads.
    pub client: &'a Client,
    /// The (new) game directory.
    pub game_dir: &'a Path,
    /// A scratch directory for downloads; removed afterwards.
    pub staging: &'a Path,
    /// Cancellation and progress.
    pub control: &'a Control,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Paths of downloaded files that were placed.
    pub installed: Vec<String>,
    /// Paths of the installed files that the pack marks optional on the client.
    pub optional: Vec<String>,
    /// Paths skipped because the pack marks them unsupported on the client.
    pub skipped: Vec<String>,
    /// Files extracted from the override folders.
    pub overrides: usize,
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Downloads the pack's client files and extracts its overrides into `game_dir`.
///
/// # Errors
///
/// `Network`, `Integrity` (size or hash mismatch, damaged archive), `Cancelled`, `Io`.
pub fn install(pack: &Pack, input: &InstallInput<'_>) -> Result<Report> {
    let mut report = Report::default();
    fs::create_dir_all(input.game_dir).map_err(|e| io(input.game_dir, &e))?;
    let _staging = Staging(input.staging.to_path_buf());
    fs::create_dir_all(input.staging).map_err(|e| io(input.staging, &e))?;

    let wanted: Vec<&PackFile> = pack.client_files().collect();
    for file in &pack.index.files {
        if file.client == Support::Unsupported {
            report.skipped.push(file.path.clone());
        }
    }
    let dests: Vec<PathBuf> = (0..wanted.len())
        .map(|n| input.staging.join(n.to_string()).join("file"))
        .collect();
    download_files(input, &wanted, &dests)?;

    for (file, source) in wanted.iter().zip(&dests) {
        check_cancel(input.control)?;
        let target = input.game_dir.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
        }
        fs::rename(source, &target).map_err(|e| io(&target, &e))?;
        report.installed.push(file.path.clone());
        if file.client == Support::Optional {
            report.optional.push(file.path.clone());
        }
    }
    report.overrides = extract_overrides(pack, input.game_dir, input.control)?;
    Ok(report)
}

fn check_cancel(control: &Control) -> Result<()> {
    if control.cancel.is_cancelled() {
        Err(Error::cancelled())
    } else {
        Ok(())
    }
}

/// Downloads every file from its first URL; a file that failed is tried on its other URLs.
fn download_files(input: &InstallInput<'_>, files: &[&PackFile], dests: &[PathBuf]) -> Result<()> {
    let spec = |file: &PackFile, url: &str, dest: &Path| {
        let mut spec = Download::new(url, dest).size(file.size);
        if let Some(sum) = Checksum::sha512(&file.sha512) {
            spec = spec.checksum(sum);
        }
        spec
    };
    let specs: Vec<Download> = files
        .iter()
        .zip(dests)
        .filter_map(|(f, d)| Some(spec(f, f.downloads.first()?, d)))
        .collect();
    let results = input.client.download_all(&specs, input.control);
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
        for url in file.downloads.iter().skip(1) {
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
        if !done {
            return Err(Error::new(
                last.kind,
                format!("{}: {}", file.path, last.message),
            ));
        }
    }
    Ok(())
}

/// Extracts the override files in order. The byte budget counts what is written, not what the
/// archive declares.
fn extract_overrides(pack: &Pack, game_dir: &Path, control: &Control) -> Result<usize> {
    if pack.overrides.is_empty() {
        return Ok(0);
    }
    let file = File::open(&pack.path).map_err(|e| io(&pack.path, &e))?;
    let mut archive = ZipArchive::new(file).map_err(|e| bad(format!("not a zip archive: {e}")))?;
    let mut budget = MAX_OVERRIDE_BYTES;
    for entry in &pack.overrides {
        check_cancel(control)?;
        let mut source = archive
            .by_index(entry.index)
            .map_err(|e| bad(format!("{}: {e}", entry.rel)))?;
        let target = game_dir.join(&entry.rel);
        if target.is_dir() {
            return Err(bad(format!("{} is a folder and a file", entry.rel)));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
        }
        let mut out = File::create(&target).map_err(|e| io(&target, &e))?;
        let written = std::io::copy(&mut (&mut source).take(budget + 1), &mut out)
            .map_err(|e| io(&target, &e))?;
        if written > budget {
            return Err(bad(format!(
                "the overrides are larger than {MAX_OVERRIDE_BYTES} bytes"
            )));
        }
        budget -= written;
        out.flush().map_err(|e| io(&target, &e))?;
    }
    Ok(pack.overrides.len())
}

/// Which catalog version a pack came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackSourceRecord {
    /// The platform.
    pub provider: Provider,
    /// Project id.
    pub project_id: String,
    /// Project slug.
    pub slug: String,
    /// Version id.
    pub version_id: String,
    /// Version number.
    pub version_number: String,
}

/// One file of the pack as installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFileRecord {
    /// Path in the game directory.
    pub path: String,
    /// Lowercase hex SHA-1.
    pub sha1: String,
    /// Lowercase hex SHA-512.
    pub sha512: String,
    /// Size in bytes.
    pub size: u64,
    /// The pack marks it optional on the client.
    pub optional: bool,
}

/// What `pack.json` keeps of the installed pack, for updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRecord {
    /// The record format.
    pub format: u32,
    /// The pack's name.
    pub name: String,
    /// The pack's own version label (`versionId` of the index).
    pub pack_version: String,
    /// The catalog version it came from; `None` for a local file.
    #[serde(default)]
    pub source: Option<PackSourceRecord>,
    /// The Minecraft version.
    pub game_version: String,
    /// Loader key and version (`fabric-loader`, `0.16.0`) when the pack has one.
    #[serde(default)]
    pub loader: Option<(String, String)>,
    /// When it was installed, Unix seconds.
    pub installed_unix: i64,
    /// The downloaded files that were installed.
    pub files: Vec<PackFileRecord>,
}

impl PackRecord {
    /// The record of `pack` after `report`.
    pub fn new(pack: &Pack, report: &Report, source: Option<PackSourceRecord>) -> Self {
        let installed: std::collections::HashSet<&str> =
            report.installed.iter().map(String::as_str).collect();
        Self {
            format: RECORD_FORMAT,
            name: pack.index.name.clone(),
            pack_version: pack.index.version_id.clone(),
            source,
            game_version: pack.index.game_version().unwrap_or_default().to_owned(),
            loader: pack
                .index
                .loader()
                .map(|(l, v)| (l.key().to_owned(), v.to_owned())),
            installed_unix: now_unix(),
            files: pack
                .index
                .files
                .iter()
                .filter(|f| installed.contains(f.path.as_str()))
                .map(|f| PackFileRecord {
                    path: f.path.clone(),
                    sha1: f.sha1.clone(),
                    sha512: f.sha512.clone(),
                    size: f.size,
                    optional: f.client == Support::Optional,
                })
                .collect(),
        }
    }

    /// Writes the record atomically to `<meta_dir>/pack.json`.
    ///
    /// # Errors
    ///
    /// `Io` when the file cannot be written.
    pub fn save(&self, meta_dir: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::internal(format!("pack record: {e}")))?;
        vl_store::write_atomic(&meta_dir.join(PACK_RECORD), &bytes)?;
        Ok(())
    }

    /// Reads `<meta_dir>/pack.json`; `None` when absent or unreadable.
    pub fn load(meta_dir: &Path) -> Option<Self> {
        let bytes = fs::read(meta_dir.join(PACK_RECORD)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// Downloads the `.mrpack` of a modpack project into `dest_dir`.
///
/// `version` is a version id; without it the most stable allowed channel wins, then the newest.
/// Returns the file and where it came from.
///
/// # Errors
///
/// `Unsupported` when the project is not a modpack, `NotFound` when no version (or the pinned
/// one) has an `.mrpack` file, `Integrity` for a file without a usable hash, plus the errors of
/// the network.
pub fn fetch(
    source: &dyn CatalogSource,
    client: &Client,
    project: &str,
    version: Option<&str>,
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
    let versions = source.versions(&info.id, &VersionFilter::default(), control)?;
    let pack_file = |v: &vl_core::catalog::ProjectVersion| {
        v.files
            .iter()
            .find(|f| f.primary && f.filename.to_ascii_lowercase().ends_with(".mrpack"))
            .or_else(|| {
                v.files
                    .iter()
                    .find(|f| f.filename.to_ascii_lowercase().ends_with(".mrpack"))
            })
            .cloned()
    };
    let chosen = match version {
        Some(id) => versions.iter().find(|v| v.id == id),
        None => {
            let mut best: Option<(u8, &vl_core::catalog::ProjectVersion)> = None;
            for v in &versions {
                let Some(rank) = min.rank(v.channel) else {
                    continue;
                };
                if pack_file(v).is_some() && best.is_none_or(|(r, _)| rank > r) {
                    best = Some((rank, v));
                }
            }
            best.map(|(_, v)| v)
        }
    }
    .ok_or_else(|| Error::not_found(format!("no installable version of {}", info.title)))?;
    let file = pack_file(chosen).ok_or_else(|| {
        Error::not_found(format!(
            "version {} has no .mrpack file",
            chosen.version_number
        ))
    })?;
    let checksum = file
        .sha512
        .as_deref()
        .and_then(Checksum::sha512)
        .or_else(|| file.sha1.as_deref().and_then(Checksum::sha1))
        .ok_or_else(|| Error::new(ErrorKind::Integrity, "the pack file has no usable hash"))?;
    fs::create_dir_all(dest_dir).map_err(|e| io(dest_dir, &e))?;
    let dest = dest_dir.join("pack.mrpack");
    let mut spec = Download::new(&file.url, &dest)
        .checksum(checksum)
        .max_bytes(MAX_PACK_BYTES);
    if file.size > 0 {
        spec = spec.size(file.size);
    }
    client.download(&spec, control)?;
    Ok((
        dest,
        PackSourceRecord {
            provider: source.provider(),
            project_id: info.id,
            slug: info.slug,
            version_id: chosen.id.clone(),
            version_number: chosen.version_number.clone(),
        },
    ))
}
