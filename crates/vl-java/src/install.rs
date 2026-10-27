//! Managed Java runtimes: download, verify, install.
//!
//! The main source is Mojang's Java runtime manifest (the one the official launcher uses): an
//! index of components (`jre-legacy`, `java-runtime-delta`, ...) per platform, each pointing to a
//! file manifest with a SHA-1 and a download link per file. Adoptium (Eclipse Temurin) is the
//! fallback for platforms and architectures Mojang has no runtime for, and when Mojang cannot be
//! reached; it serves one archive (`.zip` or `.tar.gz`) with a SHA-256.
//!
//! A runtime is built in a temporary directory under the managed root, checked with the probe,
//! and only then renamed into `<root>/<component>`. A failed or cancelled install leaves the
//! previous state untouched. An install never replaces a working runtime that already satisfies
//! the request: it is reused without any network access.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use vl_core::java::JavaRuntime;
use vl_core::platform::{Arch, Os, Platform};
use vl_core::{Error, ErrorKind};
use vl_net::{CancelToken, Checksum, Client, Control, Download, GetOptions, NetError};

use crate::probe::{self, ProbeError};
use crate::select::{self, JavaRequirement};

/// The Mojang Java runtime index (every platform, every component).
pub const MOJANG_RUNTIME_INDEX: &str = "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

/// The Adoptium API root.
pub const ADOPTIUM_API: &str = "https://api.adoptium.net";

/// Most entries one archive may hold.
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
/// Most bytes one archive may expand to.
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How deep a runtime's `bin/java` may sit below its directory (`jre.bundle/Contents/Home/bin`).
const LOCATE_DEPTH: usize = 5;

/// Where runtimes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// URL of the Mojang runtime index.
    pub mojang_index: String,
    /// Root of the Adoptium API (without a trailing slash).
    pub adoptium_api: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            mojang_index: MOJANG_RUNTIME_INDEX.to_owned(),
            adoptium_api: ADOPTIUM_API.to_owned(),
        }
    }
}

/// Which source a runtime was installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Mojang's runtime manifest.
    Mojang,
    /// Adoptium (Eclipse Temurin).
    Adoptium,
}

/// An installed (or reused) managed runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The checked runtime; `managed` is `true`.
    pub runtime: JavaRuntime,
    /// The directory name under the managed root.
    pub component: String,
    /// The source; `None` when an existing install was reused.
    pub source: Option<Source>,
    /// Bytes downloaded (`0` when reused).
    pub downloaded: u64,
}

/// Why an install failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// A network or download failure, including a hash or size mismatch.
    Net(NetError),
    /// A file-system failure.
    Io(String),
    /// A manifest or archive is malformed or unsafe.
    Invalid(String),
    /// No source has a runtime for the request on this platform.
    Unavailable(String),
    /// The installed runtime failed its check.
    Verify(String),
    /// The install was cancelled.
    Cancelled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net(e) => write!(f, "download failed: {e}"),
            Self::Io(e) => write!(f, "file system error: {e}"),
            Self::Invalid(e) => write!(f, "invalid runtime data: {e}"),
            Self::Unavailable(e) => write!(f, "no runtime available: {e}"),
            Self::Verify(e) => write!(f, "the installed runtime failed its check: {e}"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<NetError> for InstallError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Cancelled => Self::Cancelled,
            other => Self::Net(other),
        }
    }
}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<InstallError> for Error {
    fn from(e: InstallError) -> Self {
        match e {
            InstallError::Net(net) => Error::from(net),
            InstallError::Cancelled => Error::cancelled(),
            InstallError::Io(m) => Error::new(ErrorKind::Io, m),
            InstallError::Invalid(m) => Error::new(ErrorKind::Integrity, m),
            InstallError::Unavailable(_) | InstallError::Verify(_) => {
                Error::new(ErrorKind::Java, e.to_string())
            }
        }
    }
}

/// Checks a runtime executable; replaceable in tests.
pub type ProbeFn = Arc<dyn Fn(&Path) -> Result<JavaRuntime, ProbeError> + Send + Sync>;

/// Installs runtimes below one managed root (`<data>/java`).
pub struct Installer {
    client: Client,
    root: PathBuf,
    sources: Sources,
    platform: Platform,
    probe: ProbeFn,
}

/// Installs never overlap inside one process: they share the temporary-directory namespace.
static INSTALL_LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Installer {
    /// An installer for the platform this binary runs on.
    pub fn new(client: Client, root: impl Into<PathBuf>) -> Self {
        Self {
            client,
            root: root.into(),
            sources: Sources::default(),
            platform: Platform::current(),
            probe: Arc::new(|java| probe::probe(java, Duration::from_secs(15))),
        }
    }

    /// Uses other source URLs.
    #[must_use]
    pub fn with_sources(mut self, sources: Sources) -> Self {
        self.sources = sources;
        self
    }

    /// Installs for another platform (tests).
    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Uses another check (tests).
    #[must_use]
    pub fn with_probe(mut self, probe: ProbeFn) -> Self {
        self.probe = probe;
        self
    }

    /// The managed root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The installed managed runtime that satisfies `requirement`, if any. Every directory under
    /// the root is checked with the probe; nothing touches the network.
    pub fn find_installed(&self, requirement: &JavaRequirement) -> Option<Installed> {
        let mut dirs: Vec<(String, PathBuf)> = fs::read_dir(&self.root)
            .ok()?
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .filter(|(name, _)| !name.starts_with('.'))
            .collect();
        dirs.sort();
        let mut found: Vec<(String, JavaRuntime)> = Vec::new();
        for (name, dir) in dirs {
            let Some(exe) = locate_java(&dir, self.platform.os) else {
                continue;
            };
            if let Ok(mut runtime) = (self.probe)(&exe) {
                runtime.managed = true;
                found.push((name, runtime));
            }
        }
        let runtimes: Vec<JavaRuntime> = found.iter().map(|(_, r)| r.clone()).collect();
        // The component named by the version is preferred over any other fitting directory.
        let named = found.iter().find(|(name, r)| {
            !requirement.component.is_empty()
                && *name == requirement.component
                && select::satisfies(r, requirement, self.platform.arch)
        });
        let chosen = match named {
            Some((_, r)) => Some(r),
            None => select::choose(&runtimes, requirement, self.platform.arch),
        }?;
        let (component, runtime) = found.iter().find(|(_, r)| r.path == chosen.path)?;
        Some(Installed {
            runtime: runtime.clone(),
            component: component.clone(),
            source: None,
            downloaded: 0,
        })
    }

    /// Makes a managed runtime for `requirement` available: reuses an installed one, else
    /// downloads from Mojang, else from Adoptium. Progress (bytes) and cancellation go through
    /// `control`.
    pub fn install(
        &self,
        requirement: &JavaRequirement,
        control: &Control,
    ) -> Result<Installed, InstallError> {
        let _guard = INSTALL_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        fs::create_dir_all(&self.root)?;
        self.remove_stale();
        if let Some(found) = self.find_installed(requirement) {
            return Ok(found);
        }
        check(&control.cancel)?;

        let mojang = match self.install_mojang(requirement, control) {
            Ok(Some(done)) => return Ok(done),
            Ok(None) => None,
            Err(InstallError::Cancelled) => return Err(InstallError::Cancelled),
            Err(e) => Some(e),
        };
        match self.install_adoptium(requirement, control) {
            Ok(done) => Ok(done),
            Err(InstallError::Cancelled) => Err(InstallError::Cancelled),
            Err(e) => Err(match mojang {
                // Mojang was the real source and failed; say why, not only the fallback's reason.
                Some(first) => InstallError::Unavailable(format!("{first}; fallback: {e}")),
                None => e,
            }),
        }
    }

    /// Removes temporary directories that an earlier, crashed process left behind.
    fn remove_stale(&self) {
        let own = format!("-{}-", std::process::id());
        let Ok(read) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let temporary = name.starts_with(".tmp-") || name.starts_with(".old-");
            if temporary && !name.contains(&own) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // ----- Mojang -----

    /// `Ok(None)` when Mojang has no runtime for the request on this platform.
    fn install_mojang(
        &self,
        requirement: &JavaRequirement,
        control: &Control,
    ) -> Result<Option<Installed>, InstallError> {
        let Some(key) = mojang_platform(self.platform) else {
            return Ok(None);
        };
        let (index, _): (HashMap<String, HashMap<String, Vec<IndexEntry>>>, _) = self
            .client
            .get_json(&self.sources.mojang_index, &GetOptions::default(), control)?;
        let Some(components) = index.get(key) else {
            return Ok(None);
        };
        let Some((component, entry)) = pick_component(components, requirement) else {
            return Ok(None);
        };
        valid_name(&component)?;
        let stage = Stage::new(&self.root)?;

        let manifest_file = stage.path().join("manifest.json");
        let checksum = checksum_sha1(&entry.manifest.sha1)?;
        self.client.download(
            &Download::new(&entry.manifest.url, &manifest_file)
                .checksum(checksum)
                .size(entry.manifest.size),
            control,
        )?;
        let manifest: ComponentManifest = serde_json::from_slice(&fs::read(&manifest_file)?)
            .map_err(|e| InstallError::Invalid(format!("runtime manifest: {e}")))?;

        let tree = stage.path().join("runtime");
        fs::create_dir_all(&tree)?;
        let mut downloads = Vec::new();
        let mut executables = Vec::new();
        let mut links = Vec::new();
        let mut bytes = 0u64;
        for (name, node) in &manifest.files {
            let rel = safe_relative(name)?.ok_or_else(|| {
                InstallError::Invalid(format!("empty path in manifest: {name:?}"))
            })?;
            let target = tree.join(&rel);
            match node.kind.as_str() {
                "directory" => fs::create_dir_all(&target)?,
                "file" => {
                    let raw = node
                        .downloads
                        .as_ref()
                        .and_then(|d| d.raw.as_ref())
                        .ok_or_else(|| InstallError::Invalid(format!("no download for {name}")))?;
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    downloads.push(
                        Download::new(&raw.url, &target)
                            .checksum(checksum_sha1(&raw.sha1)?)
                            .size(raw.size),
                    );
                    bytes = bytes.saturating_add(raw.size);
                    if node.executable {
                        executables.push(target);
                    }
                }
                "link" => {
                    let link = node.target.clone().ok_or_else(|| {
                        InstallError::Invalid(format!("link {name} has no target"))
                    })?;
                    if !link_stays_inside(&rel, &link) {
                        return Err(InstallError::Invalid(format!(
                            "link {name} points outside the runtime"
                        )));
                    }
                    links.push((target, link));
                }
                _ => {}
            }
        }
        for result in self.client.download_all(&downloads, control) {
            result?;
        }
        check(&control.cancel)?;
        for path in &executables {
            make_executable(path)?;
        }
        for (path, target) in &links {
            make_link(path, target)?;
        }
        let installed = self.finish(&tree, &component, requirement, Some(Source::Mojang), bytes)?;
        drop(stage);
        Ok(Some(installed))
    }

    // ----- Adoptium -----

    fn install_adoptium(
        &self,
        requirement: &JavaRequirement,
        control: &Control,
    ) -> Result<Installed, InstallError> {
        let unavailable =
            || InstallError::Unavailable(format!("Java {} for this platform", requirement.major));
        let (os, arch) = adoptium_platform(self.platform).ok_or_else(unavailable)?;
        let url = format!(
            "{}/v3/assets/latest/{}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse",
            self.sources.adoptium_api.trim_end_matches('/'),
            requirement.major,
        );
        let (assets, _): (Vec<AdoptiumAsset>, _) =
            self.client
                .get_json(&url, &GetOptions::default(), control)?;
        let wanted = if self.platform.os == Os::Windows {
            ".zip"
        } else {
            ".tar.gz"
        };
        let package = assets
            .into_iter()
            .map(|a| a.binary.package)
            .find(|p| p.name.ends_with(wanted))
            .ok_or_else(unavailable)?;

        let component = format!("temurin-{}", requirement.major);
        let stage = Stage::new(&self.root)?;
        let archive = stage.path().join(format!("archive{wanted}"));
        self.client.download(
            &Download::new(&package.link, &archive)
                .checksum(
                    Checksum::sha256(&package.checksum)
                        .ok_or_else(|| InstallError::Invalid("bad archive checksum".into()))?,
                )
                .size(package.size),
            control,
        )?;
        let extracted = stage.path().join("extracted");
        fs::create_dir_all(&extracted)?;
        if wanted == ".zip" {
            extract_zip(&archive, &extracted, &control.cancel)?;
        } else {
            extract_tar_gz(&archive, &extracted, &control.cancel)?;
        }
        let tree = archive_root(&extracted);
        let installed = self.finish(
            &tree,
            &component,
            requirement,
            Some(Source::Adoptium),
            package.size,
        )?;
        drop(stage);
        Ok(installed)
    }

    // ----- common -----

    /// Checks the runtime in `tree` and moves it to `<root>/<component>`.
    fn finish(
        &self,
        tree: &Path,
        component: &str,
        requirement: &JavaRequirement,
        source: Option<Source>,
        downloaded: u64,
    ) -> Result<Installed, InstallError> {
        let exe = locate_java(tree, self.platform.os)
            .ok_or_else(|| InstallError::Verify("no java executable in the runtime".into()))?;
        let relative = exe
            .strip_prefix(tree)
            .map_err(|e| InstallError::Io(e.to_string()))?
            .to_path_buf();
        let mut runtime = (self.probe)(&exe).map_err(|e| InstallError::Verify(e.to_string()))?;
        if !select::satisfies(&runtime, requirement, self.platform.arch) {
            return Err(InstallError::Verify(format!(
                "expected Java {} ({:?}), the runtime reports Java {} ({:?})",
                requirement.major, self.platform.arch, runtime.major, runtime.arch
            )));
        }
        let dest = self.commit(tree, component)?;
        runtime.path = dest.join(relative);
        runtime.managed = true;
        Ok(Installed {
            runtime,
            component: component.to_owned(),
            source,
            downloaded,
        })
    }

    /// Replaces `<root>/<name>` with `tree` by renames; the old directory returns on failure.
    fn commit(&self, tree: &Path, name: &str) -> Result<PathBuf, InstallError> {
        let dest = self.root.join(name);
        let old = if dest.exists() {
            let old = self.root.join(format!(
                ".old-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            rename_retry(&dest, &old)?;
            Some(old)
        } else {
            None
        };
        match rename_retry(tree, &dest) {
            Ok(()) => {
                if let Some(old) = old {
                    let _ = fs::remove_dir_all(old);
                }
                Ok(dest)
            }
            Err(e) => {
                if let Some(old) = old {
                    let _ = fs::rename(old, &dest);
                }
                Err(e.into())
            }
        }
    }
}

/// A temporary directory next to the final location, removed on drop.
struct Stage(PathBuf);

impl Stage {
    fn new(root: &Path) -> Result<Self, InstallError> {
        let path = root.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check(cancel: &CancelToken) -> Result<(), InstallError> {
    if cancel.is_cancelled() {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}

/// Renames, retrying briefly: antivirus scanners on Windows may hold a fresh directory.
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

// ----- Mojang data -----

#[derive(Debug, Deserialize)]
struct IndexEntry {
    manifest: FileRef,
    version: IndexVersion,
}

#[derive(Debug, Deserialize)]
struct IndexVersion {
    name: String,
}

#[derive(Debug, Deserialize)]
struct FileRef {
    sha1: String,
    size: u64,
    url: String,
}

#[derive(Debug, Deserialize)]
struct ComponentManifest {
    files: BTreeMap<String, FileNode>,
}

#[derive(Debug, Deserialize)]
struct FileNode {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    downloads: Option<NodeDownloads>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NodeDownloads {
    #[serde(default)]
    raw: Option<FileRef>,
}

/// The key of the Mojang index for a platform; `None` when Mojang publishes none.
fn mojang_platform(platform: Platform) -> Option<&'static str> {
    match (platform.os, platform.arch) {
        (Os::Windows, Arch::X86_64) => Some("windows-x64"),
        (Os::Windows, Arch::X86) => Some("windows-x86"),
        (Os::Windows, Arch::Aarch64) => Some("windows-arm64"),
        (Os::MacOs, Arch::X86_64) => Some("mac-os"),
        (Os::MacOs, Arch::Aarch64) => Some("mac-os-arm64"),
        (Os::Linux, Arch::X86_64) => Some("linux"),
        (Os::Linux, Arch::X86) => Some("linux-i386"),
        _ => None,
    }
}

/// The component for `requirement`: the one the version names (when its runtime fits), else any
/// with exactly the required major version, release builds before snapshots.
fn pick_component<'a>(
    components: &'a HashMap<String, Vec<IndexEntry>>,
    requirement: &JavaRequirement,
) -> Option<(String, &'a IndexEntry)> {
    let major_of = |e: &IndexEntry| probe::parse_major(&e.version.name);
    let fits =
        |e: &IndexEntry| major_of(e).is_some_and(|m| select::major_fits(m, requirement.major));
    if let Some(entry) = components
        .get(&requirement.component)
        .and_then(|list| list.first())
        && fits(entry)
    {
        return Some((requirement.component.clone(), entry));
    }
    let mut names: Vec<&String> = components.keys().collect();
    names.sort_by_key(|n| (n.contains("snapshot"), (*n).clone()));
    names.into_iter().find_map(|name| {
        let entry = components.get(name)?.first()?;
        (major_of(entry) == Some(requirement.major)).then(|| (name.clone(), entry))
    })
}

fn checksum_sha1(hex: &str) -> Result<Checksum, InstallError> {
    Checksum::sha1(hex).ok_or_else(|| InstallError::Invalid(format!("bad SHA-1 {hex:?}")))
}

/// A directory name taken from remote data must be a plain name.
fn valid_name(name: &str) -> Result<(), InstallError> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(InstallError::Invalid(format!(
            "bad component name {name:?}"
        )))
    }
}

// ----- Adoptium data -----

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    name: String,
    link: String,
    checksum: String,
    size: u64,
}

/// The `os` and `architecture` query values of the Adoptium API.
fn adoptium_platform(platform: Platform) -> Option<(&'static str, &'static str)> {
    let os = match platform.os {
        Os::Windows => "windows",
        Os::MacOs => "mac",
        Os::Linux => "linux",
        Os::Other => return None,
    };
    let arch = match platform.arch {
        Arch::X86_64 => "x64",
        Arch::Aarch64 => "aarch64",
        Arch::X86 => "x32",
        Arch::Arm => "arm",
        Arch::Other => return None,
    };
    Some((os, arch))
}

// ----- paths -----

/// A relative path from remote data: `Ok(None)` for an empty path (`./`), an error for anything
/// that could leave the target directory (`..`, absolute paths, drive prefixes, `:`).
fn safe_relative(path: &str) -> Result<Option<PathBuf>, InstallError> {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) if !name.to_string_lossy().contains([':', '\\']) => {
                out.push(name);
            }
            Component::CurDir => {}
            _ => return Err(InstallError::Invalid(format!("unsafe path {path:?}"))),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Whether a symbolic link at `link` (relative to the tree) with `target` stays inside the tree.
fn link_stays_inside(link: &Path, target: &str) -> bool {
    let mut depth = link.components().count().saturating_sub(1);
    for component in Path::new(target).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            _ => return false,
        }
    }
    true
}

/// The `bin/java` executable at or below `dir`.
pub fn locate_java(dir: &Path, os: Os) -> Option<PathBuf> {
    let name = if os == Os::Windows {
        "java.exe"
    } else {
        "java"
    };
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..=LOCATE_DEPTH {
        let mut next = Vec::new();
        for dir in &level {
            let exe = dir.join("bin").join(name);
            if exe.is_file() {
                return Some(exe);
            }
            if let Ok(read) = fs::read_dir(dir) {
                let mut children: Vec<PathBuf> = read
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect();
                children.sort();
                next.extend(children);
            }
        }
        level = next;
    }
    None
}

/// The runtime directory inside an extracted archive: below the single top directory, and below
/// `Contents/Home` for a macOS bundle.
fn archive_root(extracted: &Path) -> PathBuf {
    let mut root = extracted.to_path_buf();
    let children: Vec<PathBuf> = fs::read_dir(extracted)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    if let [only] = children.as_slice()
        && only.is_dir()
    {
        root = only.clone();
    }
    let bundle = root.join("Contents").join("Home");
    if bundle.is_dir() { bundle } else { root }
}

// ----- permissions and links -----

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_link(path: &Path, target: &str) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

// Runtimes for Windows carry no links, and creating one needs a privilege there.
#[cfg(not(unix))]
fn make_link(_path: &Path, _target: &str) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode((mode & 0o777) | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// ----- archives -----

/// Counts what an archive expands to.
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
}

impl Budget {
    fn entry(&mut self) -> Result<(), InstallError> {
        self.entries += 1;
        if self.entries > MAX_ARCHIVE_ENTRIES {
            return Err(InstallError::Invalid(
                "the archive has too many entries".into(),
            ));
        }
        Ok(())
    }

    /// Copies at most the remaining byte budget into a new file.
    fn write_file(&mut self, from: &mut dyn Read, path: &Path) -> Result<(), InstallError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(path)?;
        let left = MAX_ARCHIVE_BYTES.saturating_sub(self.bytes);
        let copied = io::copy(&mut from.take(left.saturating_add(1)), &mut out)?;
        out.flush()?;
        self.bytes = self.bytes.saturating_add(copied);
        if self.bytes > MAX_ARCHIVE_BYTES {
            return Err(InstallError::Invalid("the archive is too large".into()));
        }
        Ok(())
    }
}

fn extract_zip(archive: &Path, dest: &Path, cancel: &CancelToken) -> Result<(), InstallError> {
    let invalid = |e: zip::result::ZipError| InstallError::Invalid(format!("zip: {e}"));
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?).map_err(invalid)?;
    let mut budget = Budget::default();
    for i in 0..zip.len() {
        check(cancel)?;
        budget.entry()?;
        let mut entry = zip.by_index(i).map_err(invalid)?;
        let name = entry.name().map_err(invalid)?.into_owned();
        let Some(rel) = safe_relative(&name)? else {
            continue;
        };
        let target = dest.join(&rel);
        let mode = entry.unix_mode();
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else if mode.is_some_and(|m| m & 0o170_000 == 0o120_000) {
            let mut link = String::new();
            entry
                .by_ref()
                .take(4096)
                .read_to_string(&mut link)
                .map_err(|e| InstallError::Invalid(format!("zip link: {e}")))?;
            link_entry(&target, &rel, &link)?;
        } else {
            budget.write_file(&mut entry, &target)?;
            if let Some(mode) = mode {
                set_mode(&target, mode)?;
            }
        }
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path, cancel: &CancelToken) -> Result<(), InstallError> {
    let invalid = |e: io::Error| InstallError::Invalid(format!("tar: {e}"));
    let gz = flate2::read::GzDecoder::new(io::BufReader::new(fs::File::open(archive)?));
    let mut tar = tar::Archive::new(gz);
    let mut budget = Budget::default();
    for entry in tar.entries().map_err(invalid)? {
        check(cancel)?;
        budget.entry()?;
        let mut entry = entry.map_err(invalid)?;
        let name = entry
            .path()
            .map_err(invalid)?
            .to_string_lossy()
            .into_owned();
        let Some(rel) = safe_relative(&name)? else {
            continue;
        };
        let target = dest.join(&rel);
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().ok();
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_file() {
            budget.write_file(&mut entry, &target)?;
            if let Some(mode) = mode {
                set_mode(&target, mode)?;
            }
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(invalid)?
                .map(|l| l.to_string_lossy().into_owned())
                .unwrap_or_default();
            link_entry(&target, &rel, &link)?;
        }
        // Hard links, devices and extension headers are not part of a runtime.
    }
    Ok(())
}

/// Creates a link from an archive when it stays inside the tree; refuses one that does not.
fn link_entry(target: &Path, rel: &Path, link: &str) -> Result<(), InstallError> {
    if link.is_empty() || !link_stays_inside(rel, link) {
        return Err(InstallError::Invalid(format!(
            "link {} points outside the runtime",
            rel.display()
        )));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    make_link(target, link)?;
    Ok(())
}

#[cfg(test)]
mod tests;
