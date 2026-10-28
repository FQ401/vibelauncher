//! Loaders installed from an installer jar with an `install_profile.json` (NeoForge, Forge 1.13+).
//!
//! A [`Source`] names the loader version, the installer's URL and the version's directory,
//! `<profiles dir>/<loader>/<version>/`, which holds the installer jar, its
//! `install_profile.json` and `version.json`, the data files the processors read, the processor
//! log and the record of a finished installation.
//!
//! - The installer is downloaded once, checked against the repository's `.sha1`.
//! - The launch profile is the normalized `version.json`, stored like every loader profile; a
//!   published installer never changes, so a stored profile is used without asking the server.
//! - [`install`] puts the libraries the installer carries (`maven/...`) into the library
//!   directory, and makes the files the profile expects but cannot download (the patched game jar
//!   and its mappings) by running the client processors with Java. A record (`installed.json`)
//!   keeps the SHA-1 of every file the data values name and of the vanilla jar; when all of them
//!   still match, the processors are not run again.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::component::ComponentKind;
use vl_core::{Error, ErrorKind, Result};
use vl_meta::install_profile::{
    InstallProfile, MAX_INSTALL_PROFILE_BYTES, PlanContext, ProfileLibrary, Side, local_libraries,
};
use vl_meta::loader::{CachedProfile, LoaderProfile, profile_cache_key};
use vl_meta::version::MAX_VERSION_BYTES;
use vl_net::{Checksum, Control, Download, GetOptions, NetError};
use zip::ZipArchive;

use crate::Loaders;
use crate::processors::{self, ProcessorRunner, StepHooks, io_error, sha1_file};

/// The largest installer jar accepted.
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;

/// The largest file taken out of an installer.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// The format of `installed.json`.
const RECORD_FORMAT: u32 = 1;

/// How many output lines of the processors an error message carries.
const ERROR_TAIL_LINES: usize = 12;

/// Where the installation runs and what it runs with.
pub struct InstallContext<'a> {
    /// The Java that runs the processors.
    pub java: &'a Path,
    /// The vanilla client jar (`{MINECRAFT_JAR}`), already installed.
    pub minecraft_jar: &'a Path,
    /// The directory that holds `libraries/` (`{ROOT}`).
    pub root: &'a Path,
    /// The shared library directory.
    pub libraries_dir: &'a Path,
    /// The directory of version files (`<id>/<id>.jar`), where a loader that runs the game from
    /// its own copy of the vanilla jar writes it.
    pub versions_dir: &'a Path,
    /// The game directory of the instance, when the installation is for one: files the loader
    /// expects there are put in place.
    pub game_dir: Option<&'a Path>,
    /// What runs the processors.
    pub runner: &'a dyn ProcessorRunner,
    /// Cancellation, and byte progress of the downloads.
    pub control: &'a Control,
    /// Called before each processor with its index (from 0) and their number.
    pub on_step: &'a dyn Fn(usize, usize),
}

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    /// The files were there from an earlier installation; no processor ran.
    pub reused: bool,
    /// Processors that ran.
    pub ran: usize,
    /// Processors skipped because their declared outputs were there.
    pub skipped: usize,
    /// The processor log.
    pub log_file: PathBuf,
}

/// One installer-based loader version.
pub(crate) struct Source {
    /// The loader.
    pub kind: ComponentKind,
    /// The loader version as the launcher names it (the profile's `loader_version`).
    pub version: String,
    /// The version's directory.
    pub dir: PathBuf,
    /// The URL of the installer jar.
    pub installer_url: String,
    /// Builds the launch profile from `version.json`: game, loader version, document.
    pub make_profile: fn(&str, &str, &[u8]) -> Result<LoaderProfile>,
}

impl Source {
    fn name(&self) -> &'static str {
        self.kind.display_name()
    }
}

/// The record of a finished installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    format: u32,
    version: String,
    minecraft_jar_sha1: String,
    /// Data value name to the SHA-1 of its file.
    files: BTreeMap<String, String>,
}

/// The launch profile of `source` for `game`, from the profile store or the installer.
pub(crate) fn profile(
    loaders: &Loaders,
    source: &Source,
    game: &str,
    control: &Control,
    now: i64,
) -> Result<LoaderProfile> {
    if let Some(stored) = stored_profile(loaders, source, game)? {
        return Ok(stored);
    }
    let profile = read_launch_profile(loaders, source, game, control)?;
    store_profile(loaders, source, game, &profile, now)?;
    Ok(profile)
}

/// The stored launch profile of `source` for `game`, if there is a valid one.
pub(crate) fn stored_profile(
    loaders: &Loaders,
    source: &Source,
    game: &str,
) -> Result<Option<LoaderProfile>> {
    let path = profile_path(loaders, source, game)?;
    Ok(fs::read(&path)
        .ok()
        .and_then(|bytes| CachedProfile::decode(&bytes).ok())
        .filter(|c| {
            c.profile.kind == source.kind
                && c.profile.loader_version == source.version
                && c.profile.inherits_from == game
        })
        .map(|c| c.profile))
}

/// Stores the launch profile of `source` for `game`. The store is best effort: a profile that
/// cannot be saved is still used.
pub(crate) fn store_profile(
    loaders: &Loaders,
    source: &Source,
    game: &str,
    profile: &LoaderProfile,
    now: i64,
) -> Result<()> {
    let path = profile_path(loaders, source, game)?;
    if let Ok(bytes) = CachedProfile::new(profile.clone(), None, now).encode() {
        let _ = vl_store::write_atomic(&path, &bytes);
    }
    Ok(())
}

fn profile_path(loaders: &Loaders, source: &Source, game: &str) -> Result<PathBuf> {
    let key = profile_cache_key(source.kind, game, &source.version)?;
    Ok(key
        .split('/')
        .fold(loaders.profiles_dir.clone(), |p, part| p.join(part)))
}

fn read_launch_profile(
    loaders: &Loaders,
    source: &Source,
    game: &str,
    control: &Control,
) -> Result<LoaderProfile> {
    fetch_installer(loaders, source, control)?;
    let install = read_install_profile(source, game)?;
    let json = read_capped(
        &source.dir.join(file_part(&install.json)),
        MAX_VERSION_BYTES as u64,
    )?;
    (source.make_profile)(game, &source.version, &json)
}

/// Puts the installer's libraries in place and runs the client processors of `source` for
/// `game`, unless an earlier run left files that still match.
pub(crate) fn install(
    loaders: &Loaders,
    source: &Source,
    game: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    let dir = &source.dir;
    let launch = read_launch_profile(loaders, source, game, ctx.control)?;
    let profile = read_install_profile(source, game)?;
    let jar = dir.join("installer.jar");
    // Launch libraries without an address: carried by the installer, or made by the processors.
    let local = local_libraries(&launch.libraries)?;
    extract_bundled(source, &jar, &local, ctx.libraries_dir, false)?;

    let plan_ctx = PlanContext {
        side: Side::Client,
        minecraft_jar: text(ctx.minecraft_jar)?,
        root: text(ctx.root)?,
        library_dir: text(ctx.libraries_dir)?,
        installer: text(&jar)?,
        extracted_dir: text(dir)?,
        separator: std::path::MAIN_SEPARATOR,
    };
    let log_file = dir.join("processors.log");
    let record_file = dir.join("installed.json");
    let artifacts = profile.data_artifacts(&plan_ctx)?;
    let minecraft_jar_sha1 = sha1_file(ctx.minecraft_jar)?;
    if record_matches(
        &record_file,
        &source.version,
        &minecraft_jar_sha1,
        &artifacts,
    ) && missing(&local, ctx.libraries_dir).is_none()
    {
        return Ok(Installation {
            reused: true,
            ran: 0,
            skipped: 0,
            log_file,
        });
    }

    fetch_libraries(loaders, &profile, ctx)?;
    extract_files(source, &profile, ctx.libraries_dir)?;
    let steps = profile.plan(&plan_ctx)?;

    let file = fs::File::create(&log_file).map_err(|e| io_error(&log_file, &e))?;
    let mut log = BufWriter::new(file);
    let mut tail: VecDeque<String> = VecDeque::new();
    let mut on_line = |line: &str| {
        let _ = writeln!(log, "{line}");
        if tail.len() == ERROR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.to_owned());
    };
    on_line(&format!(
        "{} {} for Minecraft {game}: {} processors, Java {}",
        source.name(),
        source.version,
        steps.len(),
        ctx.java.display()
    ));
    let mut hooks = StepHooks {
        on_step: ctx.on_step,
        on_line: &mut on_line,
    };
    let result = processors::run_steps(
        &steps,
        ctx.java,
        dir,
        ctx.runner,
        &ctx.control.cancel,
        &mut hooks,
    );
    let _ = log.flush();
    drop(log);
    let run = result.map_err(|e| {
        if e.kind == ErrorKind::Cancelled {
            return e;
        }
        let lines: Vec<String> = tail.into_iter().collect();
        Error::new(
            e.kind,
            format!(
                "{} (log: {})\n{}",
                e.message,
                log_file.display(),
                lines.join("\n")
            ),
        )
    })?;

    let mut files = BTreeMap::new();
    for (name, path) in &artifacts {
        if let Ok(sha1) = sha1_file(Path::new(path)) {
            files.insert(name.clone(), sha1);
        }
    }
    if profile.data.contains_key("PATCHED") && !files.contains_key("PATCHED") {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!(
                "the {} processors did not write the patched game jar (log: {})",
                source.name(),
                log_file.display()
            ),
        ));
    }
    if let Some(path) = missing(&local, ctx.libraries_dir) {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!(
                "the {} installation did not make {path} (log: {})",
                source.name(),
                log_file.display()
            ),
        ));
    }
    let record = Record {
        format: RECORD_FORMAT,
        version: source.version.clone(),
        minecraft_jar_sha1,
        files,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&record) {
        let _ = vl_store::write_atomic(&record_file, &bytes);
    }
    Ok(Installation {
        reused: false,
        ran: run.ran,
        skipped: run.skipped,
        log_file,
    })
}

/// The first of `libraries` whose file is not in the library directory.
fn missing<'a>(libraries: &'a [ProfileLibrary], libraries_dir: &Path) -> Option<&'a str> {
    libraries
        .iter()
        .find(|l| !join(libraries_dir, &l.path).is_file())
        .map(|l| l.path.as_str())
}

fn record_matches(
    file: &Path,
    version: &str,
    minecraft_jar_sha1: &str,
    artifacts: &[(String, String)],
) -> bool {
    let Some(record) = fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Record>(&b).ok())
    else {
        return false;
    };
    record.format == RECORD_FORMAT
        && record.version == version
        && record.minecraft_jar_sha1 == minecraft_jar_sha1
        && !record.files.is_empty()
        && artifacts
            .iter()
            .all(|(name, path)| match record.files.get(name) {
                Some(sha1) => sha1_file(Path::new(path)).is_ok_and(|h| &h == sha1),
                // A value without a file after the last run (a file of the other side).
                None => !Path::new(path).exists(),
            })
}

/// Downloads the installer of `source` (checked against the repository's SHA-1) and takes its
/// profiles out, unless they are there.
fn fetch_installer(loaders: &Loaders, source: &Source, control: &Control) -> Result<()> {
    let dir = &source.dir;
    let jar = dir.join("installer.jar");
    let profile_file = dir.join("install_profile.json");
    if jar.is_file() && profile_file.is_file() {
        return Ok(());
    }
    download_installer(loaders, source, control)?;
    let mut zip = open_zip(&jar)?;
    let profile_bytes = read_entry(
        &mut zip,
        &jar,
        "install_profile.json",
        MAX_INSTALL_PROFILE_BYTES as u64,
    )?;
    let profile = InstallProfile::parse(&profile_bytes)?;
    let json_name = file_part(&profile.json);
    let json = read_entry(&mut zip, &jar, json_name, MAX_VERSION_BYTES as u64)?;
    write(&dir.join(json_name), &json)?;
    write(&profile_file, &profile_bytes)?;
    Ok(())
}

/// Downloads the installer jar of `source` to `<dir>/installer.jar`, checked against the
/// repository's SHA-1, unless it is there. Returns its path.
pub(crate) fn download_installer(
    loaders: &Loaders,
    source: &Source,
    control: &Control,
) -> Result<PathBuf> {
    let jar = source.dir.join("installer.jar");
    let url = &source.installer_url;
    if !jar.is_file() {
        let options = GetOptions {
            max_bytes: Some(1024),
            max_age: None,
            use_cache: false,
        };
        let sha1 = match loaders
            .client
            .get_bytes(&format!("{url}.sha1"), &options, control)
        {
            Ok(f) => f.bytes,
            Err(NetError::Status { status: 404, .. }) => {
                return Err(Error::not_found(format!(
                    "{} {}",
                    source.name(),
                    source.version
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let sha1 = String::from_utf8_lossy(&sha1);
        let checksum = sha1
            .split_whitespace()
            .next()
            .and_then(Checksum::sha1)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Integrity,
                    format!(
                        "{} {}: invalid installer sha1",
                        source.name(),
                        source.version
                    ),
                )
            })?;
        let spec = Download::new(url, &jar)
            .checksum(checksum)
            .max_bytes(MAX_INSTALLER_BYTES);
        loaders
            .client
            .download(&spec, control)
            .map_err(Error::from)?;
    }
    Ok(jar)
}

fn read_install_profile(source: &Source, game: &str) -> Result<InstallProfile> {
    let bytes = read_capped(
        &source.dir.join("install_profile.json"),
        MAX_INSTALL_PROFILE_BYTES as u64,
    )?;
    let profile = InstallProfile::parse(&bytes)?;
    if profile.minecraft != game {
        return Err(Error::invalid_input(format!(
            "{} {} is for Minecraft {}, not {game}",
            source.name(),
            source.version,
            profile.minecraft
        )));
    }
    Ok(profile)
}

fn fetch_libraries(
    loaders: &Loaders,
    profile: &InstallProfile,
    ctx: &InstallContext<'_>,
) -> Result<()> {
    let mut specs = Vec::new();
    for lib in profile.libraries()? {
        let Some(url) = lib.url else { continue };
        let mut spec = Download::new(url, join(ctx.libraries_dir, &lib.path));
        if let Some(c) = lib.sha1.as_deref().and_then(Checksum::sha1) {
            spec = spec.checksum(c);
        }
        if let Some(size) = lib.size {
            spec = spec.size(size);
        }
        specs.push(spec);
    }
    let mut first = None;
    for result in loaders.client.download_all(&specs, ctx.control) {
        if let Err(e) = result
            && (first.is_none() || matches!(first, Some(NetError::Cancelled)))
        {
            first = Some(e);
        }
    }
    match first {
        None => {}
        Some(_) if ctx.control.cancel.is_cancelled() => return Err(Error::cancelled()),
        Some(e) => return Err(e.into()),
    }
    Ok(())
}

/// Takes the installer's own files out for the processors: data files below the version
/// directory, bundled libraries (`maven/...`) into the library directory.
fn extract_files(source: &Source, profile: &InstallProfile, libraries_dir: &Path) -> Result<()> {
    let jar = source.dir.join("installer.jar");
    let data: Vec<String> = profile
        .installer_files(Side::Client)?
        .into_iter()
        .filter(|f| !f.starts_with("maven/"))
        .collect();
    if !data.is_empty() {
        let mut zip = open_zip(&jar)?;
        for name in data {
            let dest = join(&source.dir, &name);
            if !dest.is_file() {
                let bytes = read_entry(&mut zip, &jar, &name, MAX_ENTRY_BYTES)?;
                write(&dest, &bytes)?;
            }
        }
    }
    let bundled: Vec<ProfileLibrary> = profile
        .libraries()?
        .into_iter()
        .filter(|l| l.url.is_none())
        .collect();
    extract_bundled(source, &jar, &bundled, libraries_dir, true)
}

/// Copies libraries without a download address from the installer's `maven/` directory into the
/// library directory, unless they are there with the right hash. A library the installer does
/// not carry is an error when `required`; otherwise it is left to the processors, and a file
/// that is there is only checked by size (the processors' outputs are checked by the record).
fn extract_bundled(
    source: &Source,
    jar: &Path,
    libraries: &[ProfileLibrary],
    libraries_dir: &Path,
    required: bool,
) -> Result<()> {
    let mut zip: Option<ZipArchive<fs::File>> = None;
    for lib in libraries {
        let dest = join(libraries_dir, &lib.path);
        let present = match fs::metadata(&dest) {
            Ok(m) if m.is_file() => {
                if required {
                    lib.sha1
                        .as_ref()
                        .is_none_or(|s| sha1_file(&dest).is_ok_and(|h| &h == s))
                } else {
                    lib.size.is_none_or(|s| s == m.len())
                }
            }
            _ => false,
        };
        if present {
            continue;
        }
        let name = format!("maven/{}", lib.path);
        let archive = match &mut zip {
            Some(z) => z,
            None => zip.insert(open_zip(jar)?),
        };
        if !required && archive.by_name(&name).is_err() {
            continue;
        }
        let bytes = read_entry(archive, jar, &name, MAX_ENTRY_BYTES)?;
        write(&dest, &bytes)?;
        if let Some(s) = &lib.sha1
            && &sha1_file(&dest)? != s
        {
            let _ = fs::remove_file(&dest);
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("{name} in the {} installer has another sha1", source.name()),
            ));
        }
    }
    Ok(())
}

pub(crate) fn open_zip(jar: &Path) -> Result<ZipArchive<fs::File>> {
    let file = fs::File::open(jar).map_err(|e| io_error(jar, &e))?;
    ZipArchive::new(file).map_err(|e| {
        // A damaged installer is fetched again next time.
        let _ = fs::remove_file(jar);
        Error::new(ErrorKind::Integrity, format!("{}: {e}", jar.display()))
    })
}

pub(crate) fn read_entry(
    zip: &mut ZipArchive<fs::File>,
    jar: &Path,
    name: &str,
    cap: u64,
) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name).map_err(|_| {
        Error::new(
            ErrorKind::Integrity,
            format!("{} has no {name}", jar.display()),
        )
    })?;
    if entry.size() > cap {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("{name} in {} is too large", jar.display()),
        ));
    }
    let mut bytes = Vec::new();
    (&mut entry)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(ErrorKind::Integrity, format!("{name}: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("{name} in {} is too large", jar.display()),
        ));
    }
    Ok(bytes)
}

pub(crate) fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::metadata(path).map_err(|e| io_error(path, &e))?;
    if meta.len() > cap {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("{} is too large", path.display()),
        ));
    }
    fs::read(path).map_err(|e| io_error(path, &e))
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    vl_store::write_atomic(path, bytes)
        .map_err(|e| Error::new(ErrorKind::Io, format!("{}: {e}", path.display())))
}

/// `/version.json` → `version.json` (checked safe by the profile parser).
fn file_part(inner: &str) -> &str {
    inner.trim_start_matches('/')
}

pub(crate) fn join(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |p, part| p.join(part))
}

fn text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_input(format!("{} is not a UTF-8 path", path.display())))
}
