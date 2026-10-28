//! Exporting an instance as a Modrinth modpack (`.mrpack`).
//!
//! The pack lists a file in `modrinth.index.json` (with its hashes and the catalog's download
//! link) when the catalog knows the file by hash, and carries every other file inside
//! `overrides/`. What goes in is chosen from the game directory: the enabled mods always, plus
//! the parts named in `include`. Disabled files (`*.disabled`) are left out, and everything
//! personal or generated (saves, logs, crash reports, screenshots, the launcher's own folder,
//! caches) is never carried, because those folders are never walked.
//!
//! The archive is deterministic: the index first, then the overrides sorted by path, every entry
//! stored with the same fixed date and mode, so the same directory exports to the same bytes.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use vl_core::catalog::{HashAlgorithm, HashMatch};
use vl_core::install::{ExportInclude, PackExport};
use vl_core::{Error, ErrorKind, Result};
use vl_modrinth::mrpack::{
    INDEX_NAME, MAX_FILE_BYTES, MAX_FILES, OVERRIDES_DIR, PackFile, PackIndex, PackLoader, Support,
    is_allowed_download, safe_path, write_index,
};
use vl_net::Control;
use vl_platforms::CatalogSource;
use zip::write::SimpleFileOptions;

use crate::hashing::{FileHashes, hash_file};
use crate::mrpack::check_cancel;

/// Folders whose files are looked up in the catalog.
const CATALOG_DIRS: [&str; 3] = ["mods", "resourcepacks", "shaderpacks"];

/// What an export is made of.
pub struct ExportInput<'a> {
    /// The catalog that knows files by hash.
    pub source: &'a dyn CatalogSource,
    /// The instance's game directory.
    pub game_dir: &'a Path,
    /// The file to write; it must not exist.
    pub out: &'a Path,
    /// The pack's name.
    pub name: &'a str,
    /// The pack's version label.
    pub version_id: &'a str,
    /// A short description.
    pub summary: Option<&'a str>,
    /// The Minecraft version.
    pub game_version: &'a str,
    /// The loader and its version; `None` for the game alone.
    pub loader: Option<(PackLoader, &'a str)>,
    /// What to carry besides the enabled mods.
    pub include: &'a [ExportInclude],
    /// Cancellation.
    pub control: &'a Control,
}

fn io_err(path: &Path, e: &io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

struct Candidate {
    /// Path in the game directory, `/`-separated.
    path: String,
    file: PathBuf,
}

#[derive(Default)]
struct Found {
    files: Vec<Candidate>,
    disabled: usize,
    warnings: Vec<String>,
}

/// Lists the regular files under `rel` (recursively); counts disabled ones and notes what
/// cannot be carried.
fn walk(game_dir: &Path, rel: &str, found: &mut Found) -> Result<()> {
    let dir = game_dir.join(rel);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(&dir, &e)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&dir, &e))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            found.warnings.push(format!(
                "{rel}/: a file with a non-Unicode name was left out"
            ));
            continue;
        };
        names.push((name, entry));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, entry) in names {
        let path = format!("{rel}/{name}");
        let kind = entry.file_type().map_err(|e| io_err(&dir, &e))?;
        if kind.is_symlink() {
            found
                .warnings
                .push(format!("{path}: links are not carried"));
        } else if kind.is_dir() {
            walk(game_dir, &path, found)?;
        } else if name.to_ascii_lowercase().ends_with(".disabled") {
            found.disabled += 1;
        } else if safe_path(&path).is_none() {
            found
                .warnings
                .push(format!("{path}: the name cannot go into a pack"));
        } else {
            found.files.push(Candidate {
                path,
                file: entry.path(),
            });
        }
    }
    Ok(())
}

fn collect(input: &ExportInput<'_>) -> Result<Found> {
    let mut roots: Vec<&str> = vec!["mods"];
    let mut single: Vec<&str> = Vec::new();
    for include in input.include {
        for path in include.paths() {
            match path.strip_suffix('/') {
                Some(dir) => roots.push(dir),
                None => single.push(path),
            }
        }
    }
    roots.sort_unstable();
    roots.dedup();
    let mut found = Found::default();
    for root in roots {
        walk(input.game_dir, root, &mut found)?;
    }
    single.sort_unstable();
    single.dedup();
    for name in single {
        let file = input.game_dir.join(name);
        if file.is_file() {
            found.files.push(Candidate {
                path: name.to_owned(),
                file,
            });
        }
    }
    found.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

fn in_catalog_dir(path: &str) -> bool {
    CATALOG_DIRS
        .iter()
        .any(|d| path.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
}

/// A mod, resource pack or shader archive: the files whose rights matter to the pack's author.
fn is_archive(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    in_catalog_dir(path) && (lower.ends_with(".jar") || lower.ends_with(".zip"))
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

/// The catalog's download link for a file with these hashes, when the match names one.
fn link(found: Option<&HashMatch>, hashes: &FileHashes) -> Option<String> {
    let same = |theirs: &Option<String>, ours: &str| {
        theirs
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(ours))
    };
    found?
        .version
        .files
        .iter()
        .find(|f| {
            f.size == hashes.size
                && (same(&f.sha512, &hashes.sha512) || same(&f.sha1, &hashes.sha1))
        })
        .map(|f| f.url.clone())
        .filter(|u| is_allowed_download(u))
}

fn env_for(path: &str) -> (Support, Support) {
    if path.starts_with("mods/") {
        (Support::Required, Support::Optional)
    } else {
        (Support::Required, Support::Unsupported)
    }
}

/// Writes the pack to `input.out`.
///
/// # Errors
///
/// `Conflict` when `out` exists, `Network` (and the like) when the catalog cannot be asked,
/// `Cancelled`, `Io`.
pub fn export(input: &ExportInput<'_>) -> Result<PackExport> {
    if input.out.exists() {
        return Err(Error::conflict(format!(
            "{} already exists",
            input.out.display()
        )));
    }
    let Found {
        files: candidates,
        disabled,
        warnings,
    } = collect(input)?;
    let mut report = PackExport {
        disabled,
        warnings,
        ..PackExport::default()
    };
    if candidates.len() > MAX_FILES {
        return Err(Error::invalid_input(format!(
            "more than {MAX_FILES} files to carry"
        )));
    }

    // Hash what the catalog could know.
    let mut hashed: HashMap<String, FileHashes> = HashMap::new();
    for c in candidates.iter().filter(|c| in_catalog_dir(&c.path)) {
        check_cancel(input.control)?;
        match hash_file(&c.file) {
            Ok(h) => {
                hashed.insert(c.path.clone(), h);
            }
            Err(e) => report.warnings.push(format!(
                "{}: cannot be hashed ({e}); it is carried inside the pack",
                c.path
            )),
        }
    }
    let by_512 = lookup(
        input.source,
        HashAlgorithm::Sha512,
        hashed.values().map(|h| h.sha512.clone()).collect(),
        input.control,
    )?;
    let by_1 = lookup(
        input.source,
        HashAlgorithm::Sha1,
        hashed
            .values()
            .filter(|h| !by_512.contains_key(&h.sha512))
            .map(|h| h.sha1.clone())
            .collect(),
        input.control,
    )?;

    let mut files = Vec::new();
    let mut bundled: Vec<&Candidate> = Vec::new();
    for c in &candidates {
        let linked = hashed.get(&c.path).and_then(|h| {
            let found = by_512.get(&h.sha512).or_else(|| by_1.get(&h.sha1));
            link(found, h).map(|url| (h, url))
        });
        if let Some((h, url)) = linked {
            let (client, server) = env_for(&c.path);
            files.push(PackFile {
                path: c.path.clone(),
                sha1: h.sha1.clone(),
                sha512: h.sha512.clone(),
                client,
                server,
                downloads: vec![url],
                size: h.size,
            });
            continue;
        }
        let size = fs::metadata(&c.file)
            .map_err(|e| io_err(&c.file, &e))?
            .len();
        if size > MAX_FILE_BYTES {
            report
                .warnings
                .push(format!("{}: too large, left out", c.path));
        } else {
            bundled.push(c);
        }
    }

    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_owned(), input.game_version.to_owned());
    if let Some((loader, version)) = input.loader {
        dependencies.insert(loader.key().to_owned(), version.to_owned());
    }
    report.linked = files.len();
    report.bundled = bundled.len();
    report.unlisted = bundled
        .iter()
        .filter(|c| is_archive(&c.path))
        .map(|c| c.path.clone())
        .collect();
    if !report.unlisted.is_empty() {
        report.warnings.push(format!(
            "{} mods, resource packs or shaders are not in the catalog and are carried inside the \
             pack; you are responsible for having the right to distribute them",
            report.unlisted.len()
        ));
    }
    let index = PackIndex {
        version_id: input.version_id.to_owned(),
        name: input.name.to_owned(),
        summary: input.summary.map(str::to_owned),
        files,
        dependencies,
    };
    let index_bytes = write_index(&index)?;
    write_archive(input, &index_bytes, &bundled)?;
    report.path = input.out.display().to_string();
    report.size = fs::metadata(input.out)
        .map_err(|e| io_err(input.out, &e))?
        .len();
    Ok(report)
}

fn write_archive(input: &ExportInput<'_>, index: &[u8], bundled: &[&Candidate]) -> Result<()> {
    if let Some(parent) = input.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
    }
    let mut partial = input.out.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    if let Err(e) = write_zip(&partial, input, index, bundled) {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    fs::rename(&partial, input.out).map_err(|e| {
        let _ = fs::remove_file(&partial);
        io_err(input.out, &e)
    })
}

fn write_zip(
    partial: &Path,
    input: &ExportInput<'_>,
    index: &[u8],
    bundled: &[&Candidate],
) -> Result<()> {
    let file = File::create(partial).map_err(|e| io_err(partial, &e))?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    // The default date is the DOS epoch (1980-01-01), the same for every entry.
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let zerr =
        |e: zip::result::ZipError| Error::new(ErrorKind::Io, format!("writing the pack: {e}"));
    zip.start_file(INDEX_NAME, options).map_err(zerr)?;
    zip.write_all(index).map_err(|e| io_err(partial, &e))?;
    for c in bundled {
        check_cancel(input.control)?;
        let name = format!("{OVERRIDES_DIR}/{}", c.path);
        zip.start_file(name, options.large_file(true))
            .map_err(zerr)?;
        let mut source = File::open(&c.file).map_err(|e| io_err(&c.file, &e))?;
        io::copy(&mut source, &mut zip).map_err(|e| io_err(&c.file, &e))?;
    }
    let mut inner = zip.finish().map_err(zerr)?;
    inner.flush().map_err(|e| io_err(partial, &e))?;
    Ok(())
}
