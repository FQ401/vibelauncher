//! NeoForge: `https://maven.neoforged.net/releases`, artifact `net.neoforged:neoforge`.
//!
//! - The version list is the artifact's `maven-metadata.xml` (through the HTTP cache), mapped to
//!   game versions by `vl_meta::neoforge`.
//! - A version is installed from its installer jar,
//!   `neoforge-<version>-installer.jar`, checked against the repository's `.sha1`. The jar,
//!   its `install_profile.json` and `version.json`, the data files the processors read, the
//!   processor log and the record of a finished installation live in
//!   `<profiles dir>/neoforge/<version>/`.
//! - The launch profile is the normalized `version.json`, stored like every loader profile; a
//!   NeoForge version never changes, so a stored profile is used without asking the server.
//! - [`install`] makes the files the profile expects but does not list (the patched game jar and
//!   its mappings) by running the client processors with Java. A record (`installed.json`) keeps
//!   the SHA-1 of every file the data values name and of the vanilla jar; when all of them still
//!   match, the processors are not run again.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::component::ComponentKind;
use vl_core::{Error, ErrorKind, Result};
use vl_meta::install_profile::{InstallProfile, MAX_INSTALL_PROFILE_BYTES, PlanContext, Side};
use vl_meta::loader::{CachedProfile, LoaderProfile, LoaderVersions, profile_cache_key};
use vl_meta::neoforge::{self as meta, MAX_METADATA_BYTES};
use vl_meta::version::MAX_VERSION_BYTES;
use vl_net::{Checksum, Control, Download, GetOptions, NetError};
use zip::ZipArchive;

use crate::Loaders;
use crate::processors::{self, ProcessorRunner, StepHooks, io_error, sha1_file};

/// The NeoForge Maven repository.
pub const MAVEN_URL: &str = "https://maven.neoforged.net/releases";

/// The largest installer jar accepted.
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;

/// The largest file taken out of an installer.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

/// The format of `installed.json`.
const RECORD_FORMAT: u32 = 1;

/// How many output lines of the processors an error message carries.
const ERROR_TAIL_LINES: usize = 12;

/// The NeoForge repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maven {
    base: String,
}

impl Default for Maven {
    fn default() -> Self {
        Self::new(MAVEN_URL)
    }
}

impl Maven {
    /// The repository at `base`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    /// The URL of the version list.
    pub fn metadata_url(&self) -> String {
        format!("{}/net/neoforged/neoforge/maven-metadata.xml", self.base)
    }

    /// The URL of the installer of `version`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a version that cannot be a path part.
    pub fn installer_url(&self, version: &str) -> Result<String> {
        Ok(format!("{}/{}", self.base, meta::installer_path(version)?))
    }
}

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
    /// The files were there from an earlier installation; nothing ran.
    pub reused: bool,
    /// Processors that ran.
    pub ran: usize,
    /// Processors skipped because their declared outputs were there.
    pub skipped: usize,
    /// The processor log.
    pub log_file: PathBuf,
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

fn version_dir(loaders: &Loaders, version: &str) -> Result<PathBuf> {
    meta::installer_path(version)?;
    Ok(loaders.profiles_dir.join("neoforge").join(version))
}

/// The NeoForge versions for `game`, newest first.
pub(crate) fn versions(loaders: &Loaders, game: &str, control: &Control) -> Result<LoaderVersions> {
    let options = GetOptions {
        max_bytes: Some(MAX_METADATA_BYTES as u64),
        max_age: Some(loaders.options.versions_max_age),
        use_cache: true,
    };
    let fetched = loaders
        .client
        .get_bytes(&loaders.options.neoforge.metadata_url(), &options, control)
        .map_err(Error::from)?;
    let all = meta::parse_maven_versions(&fetched.bytes)?;
    meta::versions_for_game(game, &all)
}

/// The launch profile of NeoForge `version` for `game`.
pub(crate) fn profile(
    loaders: &Loaders,
    game: &str,
    version: &str,
    control: &Control,
    now: i64,
) -> Result<LoaderProfile> {
    let key = profile_cache_key(ComponentKind::NeoForge, game, version)?;
    let path = key
        .split('/')
        .fold(loaders.profiles_dir.clone(), |p, part| p.join(part));
    if let Some(cached) = fs::read(&path)
        .ok()
        .and_then(|bytes| CachedProfile::decode(&bytes).ok())
        .filter(|c| {
            c.profile.kind == ComponentKind::NeoForge
                && c.profile.loader_version == version
                && c.profile.inherits_from == game
        })
    {
        return Ok(cached.profile);
    }
    let dir = fetch_installer(loaders, version, control)?;
    let install = read_install_profile(&dir, game, version)?;
    let json = read_capped(
        &dir.join(file_part(&install.json)),
        MAX_VERSION_BYTES as u64,
    )?;
    let profile = meta::profile(game, version, &json)?;
    // The store is best effort: a profile that cannot be saved is still used.
    if let Ok(bytes) = CachedProfile::new(profile.clone(), None, now).encode() {
        let _ = vl_store::write_atomic(&path, &bytes);
    }
    Ok(profile)
}

/// Runs the client processors of NeoForge `version` for `game`, unless an earlier run left
/// files that still match.
pub(crate) fn install(
    loaders: &Loaders,
    game: &str,
    version: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    let dir = fetch_installer(loaders, version, ctx.control)?;
    let profile = read_install_profile(&dir, game, version)?;
    let plan_ctx = PlanContext {
        side: Side::Client,
        minecraft_jar: text(ctx.minecraft_jar)?,
        root: text(ctx.root)?,
        library_dir: text(ctx.libraries_dir)?,
        installer: text(&dir.join("installer.jar"))?,
        extracted_dir: text(&dir)?,
        separator: std::path::MAIN_SEPARATOR,
    };
    let log_file = dir.join("processors.log");
    let record_file = dir.join("installed.json");
    let artifacts = profile.data_artifacts(&plan_ctx)?;
    let minecraft_jar_sha1 = sha1_file(ctx.minecraft_jar)?;
    if record_matches(&record_file, version, &minecraft_jar_sha1, &artifacts) {
        return Ok(Installation {
            reused: true,
            ran: 0,
            skipped: 0,
            log_file,
        });
    }

    fetch_libraries(loaders, &profile, ctx)?;
    extract_files(&profile, &dir, ctx.libraries_dir)?;
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
        "NeoForge {version} for Minecraft {game}: {} processors, Java {}",
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
        &dir,
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
                "the NeoForge processors did not write the patched game jar (log: {})",
                log_file.display()
            ),
        ));
    }
    let record = Record {
        format: RECORD_FORMAT,
        version: version.to_owned(),
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

/// Downloads the installer of `version` (checked against the repository's SHA-1) and takes its
/// profiles out, unless they are there. Returns the version's directory.
fn fetch_installer(loaders: &Loaders, version: &str, control: &Control) -> Result<PathBuf> {
    let dir = version_dir(loaders, version)?;
    let jar = dir.join("installer.jar");
    let profile_file = dir.join("install_profile.json");
    if jar.is_file() && profile_file.is_file() {
        return Ok(dir);
    }
    let url = loaders.options.neoforge.installer_url(version)?;
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
                return Err(Error::not_found(format!("NeoForge {version}")));
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
                    format!("NeoForge {version}: invalid installer sha1"),
                )
            })?;
        let spec = Download::new(&url, &jar)
            .checksum(checksum)
            .max_bytes(MAX_INSTALLER_BYTES);
        loaders
            .client
            .download(&spec, control)
            .map_err(Error::from)?;
    }
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
    Ok(dir)
}

fn read_install_profile(dir: &Path, game: &str, version: &str) -> Result<InstallProfile> {
    let bytes = read_capped(
        &dir.join("install_profile.json"),
        MAX_INSTALL_PROFILE_BYTES as u64,
    )?;
    let profile = InstallProfile::parse(&bytes)?;
    if profile.minecraft != game {
        return Err(Error::invalid_input(format!(
            "NeoForge {version} is for Minecraft {}, not {game}",
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

/// Takes the installer's own files out: data files below the version directory, bundled
/// libraries (`maven/...`) into the library directory.
fn extract_files(profile: &InstallProfile, dir: &Path, libraries_dir: &Path) -> Result<()> {
    let files = profile.installer_files(Side::Client)?;
    if files.is_empty() {
        return Ok(());
    }
    let jar = dir.join("installer.jar");
    let mut zip = open_zip(&jar)?;
    let libraries = profile.libraries()?;
    for name in files {
        let (dest, sha1) = match name.strip_prefix("maven/") {
            Some(rest) => (
                join(libraries_dir, rest),
                libraries
                    .iter()
                    .find(|l| l.path == rest)
                    .and_then(|l| l.sha1.clone()),
            ),
            None => (join(dir, &name), None),
        };
        if dest.is_file()
            && sha1
                .as_ref()
                .is_none_or(|s| sha1_file(&dest).is_ok_and(|h| &h == s))
        {
            continue;
        }
        let bytes = read_entry(&mut zip, &jar, &name, MAX_ENTRY_BYTES)?;
        write(&dest, &bytes)?;
        if let Some(s) = sha1
            && sha1_file(&dest)? != s
        {
            let _ = fs::remove_file(&dest);
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("{name} in the NeoForge installer has another sha1"),
            ));
        }
    }
    Ok(())
}

fn open_zip(jar: &Path) -> Result<ZipArchive<fs::File>> {
    let file = fs::File::open(jar).map_err(|e| io_error(jar, &e))?;
    ZipArchive::new(file).map_err(|e| {
        // A damaged installer is fetched again next time.
        let _ = fs::remove_file(jar);
        Error::new(ErrorKind::Integrity, format!("{}: {e}", jar.display()))
    })
}

fn read_entry(zip: &mut ZipArchive<fs::File>, jar: &Path, name: &str, cap: u64) -> Result<Vec<u8>> {
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

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::metadata(path).map_err(|e| io_error(path, &e))?;
    if meta.len() > cap {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("{} is too large", path.display()),
        ));
    }
    fs::read(path).map_err(|e| io_error(path, &e))
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    vl_store::write_atomic(path, bytes)
        .map_err(|e| Error::new(ErrorKind::Io, format!("{}: {e}", path.display())))
}

/// `/version.json` → `version.json` (checked safe by the profile parser).
fn file_part(inner: &str) -> &str {
    inner.trim_start_matches('/')
}

fn join(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |p, part| p.join(part))
}

fn text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_input(format!("{} is not a UTF-8 path", path.display())))
}
