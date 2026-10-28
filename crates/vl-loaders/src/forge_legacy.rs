//! Forge with a legacy installer (Minecraft 1.5.2 to 1.12.2): no processors, the launch profile
//! is the installer's `versionInfo` ([`vl_meta::forge_legacy`]).
//!
//! [`install`] puts in place what the launch profile cannot download:
//!
//! - the Forge jar the installer carries, at its library path;
//! - with `stripMeta` (Minecraft 1.5.2), the game's own copy of the vanilla jar without
//!   `META-INF/`, at `<versions dir>/<profile id>/<profile id>.jar`; `installed.json` keeps the
//!   SHA-1 of the vanilla jar and of the copy, so the copy is made again only when one changes;
//! - the deobfuscation data FML 1.5.2 reads from `<game dir>/lib/`: its SHA-1 is in the Forge
//!   jar, the file comes from the first FML library mirror that serves it with that SHA-1, is
//!   kept in the version's directory and copied into the game directory of the instance.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::{Error, ErrorKind, Result};
use vl_meta::forge_legacy::{FmlLibrary, LegacyInstaller, fml_deobfuscation_data};
use vl_meta::install_profile::MAX_INSTALL_PROFILE_BYTES;
use vl_meta::loader::LoaderProfile;
use vl_net::{Checksum, Download, NetError};
use zip::ZipArchive;
use zip::write::ZipWriter;

use crate::Loaders;
use crate::installer::{InstallContext, Installation, Source, join, open_zip, read_entry, write};
use crate::processors::{io_error, sha1_file};

/// The format of `installed.json`.
const RECORD_FORMAT: u32 = 1;

/// The largest file taken out of an installer.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// The largest `fmlversion.properties` read.
const MAX_PROPERTIES_BYTES: u64 = 64 * 1024;

/// The largest FML library downloaded.
const MAX_FML_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;

/// The record of the game's copy of the vanilla jar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    format: u32,
    version: String,
    minecraft_jar_sha1: String,
    jar_sha1: String,
}

fn installer_of(source: &Source) -> Result<LegacyInstaller> {
    let jar = source.dir.join("installer.jar");
    let mut zip = open_zip(&jar)?;
    let bytes = read_entry(
        &mut zip,
        &jar,
        "install_profile.json",
        MAX_INSTALL_PROFILE_BYTES as u64,
    )?;
    LegacyInstaller::parse(&bytes)
}

/// The launch profile of `source` for `game` from its downloaded installer.
pub(crate) fn profile(source: &Source, game: &str) -> Result<LoaderProfile> {
    installer_of(source)?.profile(game, &source.version)
}

/// Puts the carried Forge jar, the game's copy of the vanilla jar and the FML files in place.
pub(crate) fn install(
    loaders: &Loaders,
    source: &Source,
    game: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    let installer = installer_of(source)?;
    let profile = installer.profile(game, &source.version)?;
    let log_file = source.dir.join("install.log");
    let mut log = Vec::new();

    let forge_jar = join(ctx.libraries_dir, &installer.library_path()?);
    if extract_forge_jar(source, &installer, &forge_jar)? {
        log.push(format!("extracted {}", forge_jar.display()));
    }
    if let Some(id) = &profile.jar {
        let target = ctx.versions_dir.join(id).join(format!("{id}.jar"));
        if strip_game_jar(source, ctx.minecraft_jar, &target)? {
            log.push(format!("wrote {} without META-INF", target.display()));
        }
    }
    if let Some(lib) = needed_fml_library(&forge_jar)? {
        if fetch_fml_library(loaders, source, &lib, ctx)? {
            log.push(format!("downloaded {}", lib.name));
        }
        if let Some(game_dir) = ctx.game_dir {
            let dest = game_dir.join("lib").join(&lib.name);
            if !sha1_file(&dest).is_ok_and(|h| h == lib.sha1) {
                let bytes = fs::read(source.dir.join(&lib.name))
                    .map_err(|e| io_error(&source.dir.join(&lib.name), &e))?;
                write(&dest, &bytes)?;
                log.push(format!("copied {} to {}", lib.name, dest.display()));
            }
        }
    }
    let reused = log.is_empty();
    if !reused {
        let _ = write(&log_file, (log.join("\n") + "\n").as_bytes());
    }
    Ok(Installation {
        reused,
        ran: 0,
        skipped: 0,
        log_file,
    })
}

/// Copies the Forge jar out of the installer unless a file of its size is there. Returns
/// whether it was written.
fn extract_forge_jar(source: &Source, installer: &LegacyInstaller, dest: &Path) -> Result<bool> {
    let jar = source.dir.join("installer.jar");
    let mut zip = open_zip(&jar)?;
    let size = zip
        .by_name(&installer.file_path)
        .map(|e| e.size())
        .map_err(|_| {
            Error::new(
                ErrorKind::Integrity,
                format!("{} has no {}", jar.display(), installer.file_path),
            )
        })?;
    if fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == size) {
        return Ok(false);
    }
    let bytes = read_entry(&mut zip, &jar, &installer.file_path, MAX_ENTRY_BYTES)?;
    write(dest, &bytes)?;
    Ok(true)
}

/// Writes `target`, the vanilla jar without `META-INF/`, unless the record says the one there
/// was made from the same vanilla jar and is unchanged. Returns whether it was written.
fn strip_game_jar(source: &Source, vanilla: &Path, target: &Path) -> Result<bool> {
    let record_file = source.dir.join("installed.json");
    let minecraft_jar_sha1 = sha1_file(vanilla)?;
    let current = fs::read(&record_file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Record>(&b).ok());
    if current.is_some_and(|r| {
        r.format == RECORD_FORMAT
            && r.version == source.version
            && r.minecraft_jar_sha1 == minecraft_jar_sha1
            && sha1_file(target).is_ok_and(|h| h == r.jar_sha1)
    }) {
        return Ok(false);
    }
    let bytes = without_meta_inf(vanilla)?;
    write(target, &bytes)?;
    let record = Record {
        format: RECORD_FORMAT,
        version: source.version.clone(),
        minecraft_jar_sha1,
        jar_sha1: sha1_file(target)?,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&record) {
        let _ = vl_store::write_atomic(&record_file, &bytes);
    }
    Ok(true)
}

/// The entries of the jar at `path` except `META-INF/`, copied without recompressing.
fn without_meta_inf(path: &Path) -> Result<Vec<u8>> {
    let damaged = |e: &dyn std::fmt::Display| {
        Error::new(ErrorKind::Integrity, format!("{}: {e}", path.display()))
    };
    let file = fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| damaged(&e))?;
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| damaged(&e))?;
        if entry
            .name()
            .map_err(|e| damaged(&e))?
            .starts_with("META-INF/")
        {
            continue;
        }
        out.raw_copy_file(entry).map_err(|e| damaged(&e))?;
    }
    Ok(out.finish().map_err(|e| damaged(&e))?.into_inner())
}

/// The deobfuscation data the FML in `forge_jar` reads from its `lib` directory, if any.
fn needed_fml_library(forge_jar: &Path) -> Result<Option<FmlLibrary>> {
    let file = fs::File::open(forge_jar).map_err(|e| io_error(forge_jar, &e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| {
        Error::new(
            ErrorKind::Integrity,
            format!("{}: {e}", forge_jar.display()),
        )
    })?;
    let bundled = zip
        .file_names()
        .any(|n| n.is_ok_and(|n| n.starts_with("deobfuscation_data")));
    let Ok(entry) = zip.by_name("fmlversion.properties") else {
        return Ok(None);
    };
    let mut text = String::new();
    entry
        .take(MAX_PROPERTIES_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| Error::new(ErrorKind::Integrity, format!("fmlversion.properties: {e}")))?;
    Ok(fml_deobfuscation_data(&text, bundled))
}

/// Downloads `lib` into the version's directory from the first mirror that serves it with the
/// right SHA-1, unless it is there. Returns whether it was downloaded.
fn fetch_fml_library(
    loaders: &Loaders,
    source: &Source,
    lib: &FmlLibrary,
    ctx: &InstallContext<'_>,
) -> Result<bool> {
    let dest: PathBuf = source.dir.join(&lib.name);
    if sha1_file(&dest).is_ok_and(|h| h == lib.sha1) {
        return Ok(false);
    }
    let checksum = Checksum::sha1(&lib.sha1)
        .ok_or_else(|| Error::new(ErrorKind::Integrity, format!("{}: invalid sha1", lib.name)))?;
    let mut last = None;
    for mirror in loaders.options.forge.fml_libraries() {
        let sep = if mirror.ends_with('/') { "" } else { "/" };
        let spec = Download::new(format!("{mirror}{sep}{}", lib.name), &dest)
            .checksum(checksum.clone())
            .max_bytes(MAX_FML_LIBRARY_BYTES);
        match loaders.client.download(&spec, ctx.control) {
            Ok(_) => return Ok(true),
            Err(NetError::Cancelled) => return Err(Error::cancelled()),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => {
            let e = Error::from(e);
            Error::new(
                e.kind,
                format!("FML library {} is not available: {}", lib.name, e.message),
            )
        }
        None => Error::not_found(format!("FML library {}: no mirror is set", lib.name)),
    })
}
