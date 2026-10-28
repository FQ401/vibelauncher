//! Installing a game version: metadata, client jar, libraries, natives, assets.
//!
//! [`Installer::install`] is idempotent. It makes everything the game needs exist below the
//! shared [`Layout`] and returns what the command line is built from ([`Installed`]).
//!
//! A mod loader is an optional [`LoaderProfile`]: it takes the place of the root version JSON, the
//! vanilla version it inherits from is installed as usual, and the merged result (loader main
//! class, loader libraries first on the class path) is what the rest of the steps work on.
//!
//! 1. The version JSON (and the JSONs of an `inheritsFrom` chain) is read from
//!    `versions/<id>/<id>.json`; when it is not there, the version manifest is fetched (with the
//!    HTTP cache) and the JSON is downloaded and checked against the manifest's SHA-1. An
//!    installed version therefore needs no network at all.
//! 2. Libraries are planned for the platform (`vl-meta`), after the platform adjustments of
//!    [`crate::replace`].
//! 3. The asset index is fetched and parsed, the objects to fetch are listed.
//! 4. The client jar, libraries, logging configuration and asset objects are downloaded as one
//!    batch with one byte-progress. Files already present with the expected size are kept
//!    ([`Verify::Quick`]) or hashed again ([`Verify::Full`]). A library without an address
//!    (a loader's installer or processors make it) is never downloaded: it must exist, and
//!    under [`Verify::Full`] match its SHA-1 when the version names one.
//! 5. Natives are unpacked, and the files of the `virtual` / `map_to_resources` asset layouts
//!    are restored.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vl_core::platform::Platform;
use vl_core::task::ProgressUnit;
use vl_core::{Error, ErrorKind, Result};
use vl_meta::assets::{AssetIndex, AssetLayout, AssetPlan, MAX_ASSET_INDEX_BYTES};
use vl_meta::libraries::{self, LibraryFile, LibraryPlan, apply_replacements};
use vl_meta::loader::LoaderProfile;
use vl_meta::manifest::{MAX_MANIFEST_BYTES, VersionManifest};
use vl_meta::rules::{Features, RuleContext};
use vl_meta::version::{MAX_VERSION_BYTES, ResolvedVersion, VersionJson};
use vl_net::{Checksum, Client, Download, GetOptions, NetError};

use crate::job::Job;
use crate::layout::{Layout, check_component, join_relative};
use crate::{natives, replace};

/// The version manifest.
pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// How much to trust files that are already on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verify {
    /// Keep a file that exists with the expected size.
    #[default]
    Quick,
    /// Hash every file again.
    Full,
}

/// How an [`Installer`] works.
#[derive(Debug, Clone)]
pub struct InstallerOptions {
    /// The platform to install for.
    pub platform: Platform,
    /// The version manifest URL.
    pub manifest_url: String,
    /// How existing files are checked.
    pub verify: Verify,
    /// How long a fetched manifest is used without asking the server again.
    pub manifest_max_age: Duration,
}

impl Default for InstallerOptions {
    fn default() -> Self {
        Self {
            platform: Platform::current(),
            manifest_url: MANIFEST_URL.to_owned(),
            verify: Verify::Quick,
            manifest_max_age: Duration::from_secs(10 * 60),
        }
    }
}

/// The logging configuration file of a version and the JVM argument that points at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLogging {
    /// The JVM argument template, with `${path}`.
    pub argument: String,
    /// The file on disk.
    pub path: PathBuf,
}

/// An installed version: everything the command line needs.
#[derive(Debug, Clone)]
pub struct Installed {
    /// The version id.
    pub id: String,
    /// The merged version JSON.
    pub version: ResolvedVersion,
    /// The platform the files were chosen for (see [`crate::replace`]).
    pub platform: Platform,
    /// The libraries for that platform.
    pub libraries: LibraryPlan,
    /// The client jar.
    pub client_jar: PathBuf,
    /// The directory the natives were unpacked to.
    pub natives_dir: PathBuf,
    /// The asset index id (`${assets_index_name}`).
    pub assets_index_id: String,
    /// The asset layout the game reads.
    pub asset_layout: AssetLayout,
    /// The logging configuration, if the version names one.
    pub logging: Option<InstalledLogging>,
    /// Things worth telling the user that do not stop the launch.
    pub warnings: Vec<String>,
}

/// Installs versions below a [`Layout`] through a [`Client`].
pub struct Installer {
    client: Client,
    layout: Layout,
    options: InstallerOptions,
}

/// One file to fetch in the batch.
struct Fetch {
    spec: Download,
    /// Size counted into the progress total.
    size: u64,
}

impl Installer {
    /// An installer.
    pub fn new(client: Client, layout: Layout, options: InstallerOptions) -> Self {
        Self {
            client,
            layout,
            options,
        }
    }

    /// A copy of this installer that checks existing files as `verify` says.
    #[must_use]
    pub fn with_verify(&self, verify: Verify) -> Self {
        Self {
            client: self.client.clone(),
            layout: self.layout.clone(),
            options: InstallerOptions {
                verify,
                ..self.options.clone()
            },
        }
    }

    /// The shared directories.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The HTTP client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Makes version `id` ready to launch. `game_dir` receives the files of the
    /// `map_to_resources` asset layout; without it those are skipped.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown version, `Network`, `Integrity` and `Io` for failed transfers,
    /// bad hashes and disk problems, `Unsupported` when the platform cannot run the version,
    /// `Cancelled` when the job was cancelled.
    pub fn install(&self, id: &str, game_dir: Option<&Path>, job: &Job) -> Result<Installed> {
        self.install_with(id, None, game_dir, job)
    }

    /// [`install`](Self::install) with a mod loader: `game` is the vanilla version the profile
    /// inherits from, and [`Installed::id`] is the profile's id. The profile's libraries are
    /// downloaded and checked with everything else.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install); `InvalidInput` when the profile does not inherit from
    /// `game`.
    pub fn install_with(
        &self,
        game: &str,
        loader: Option<&LoaderProfile>,
        game_dir: Option<&Path>,
        job: &Job,
    ) -> Result<Installed> {
        check_component("version id", game)?;
        job.check()?;
        job.report("metadata", 0, None, ProgressUnit::Steps);
        let mut meta = Metadata {
            installer: self,
            job,
            manifest: None,
        };
        let (id, root) = match loader {
            Some(profile) => {
                if profile.inherits_from != game {
                    return Err(Error::invalid_input(format!(
                        "loader profile {} is for Minecraft {}, not {game}",
                        profile.id, profile.inherits_from
                    )));
                }
                check_component("loader profile id", &profile.id)?;
                (profile.id.as_str(), profile.to_version_json())
            }
            None => (game, meta.load(game)?),
        };
        let version = root.resolve(|parent| meta.load(parent))?;
        let mut warnings = Vec::new();

        let adjusted = replace::adjust(&version.libraries, self.options.platform);
        warnings.extend(adjusted.notes);
        let platform = adjusted.platform;
        let libs = apply_replacements(&version.libraries, &adjusted.replacements, platform);
        let features = Features::new();
        let ctx = RuleContext::new(platform, &features);
        let plan = libraries::plan(&libs, &ctx)?;
        for name in &plan.missing_natives {
            warnings.push(format!("library {name} has no natives for this platform"));
        }

        let mut fetches = Vec::new();
        let libraries_dir = self.layout.libraries_dir();
        for file in &plan.downloads {
            if let Some(fetch) = self.library_fetch(&libraries_dir, file)? {
                fetches.push(fetch);
            }
        }

        // The client download belongs to the root version. A loader that runs the game from its
        // own copy of that jar (`jar` naming another version) made the copy when it was
        // installed.
        let client_jar = self.layout.client_jar(&version.client_jar);
        let downloaded_jar = match version.chain.last() {
            Some(root) if *root != version.client_jar => {
                if !client_jar.is_file() {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "version {id} runs from {} and it is missing: install the loader again",
                            version.client_jar
                        ),
                    ));
                }
                self.layout.client_jar(root)
            }
            _ => client_jar.clone(),
        };
        match version.downloads.get("client") {
            Some(d) => {
                if let Some(fetch) = self.checked_fetch(&d.url, &downloaded_jar, &d.sha1, d.size) {
                    fetches.push(fetch);
                }
            }
            None if !downloaded_jar.is_file() => {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "version {id} has no client download and {} is missing",
                        version.client_jar
                    ),
                ));
            }
            None => {}
        }

        let logging = match &version.logging {
            Some(config) => {
                let name = file_name(&config.file.url, &config.file.id)?;
                let path = self.layout.log_configs_dir().join(name);
                if let Some(fetch) =
                    self.checked_fetch(&config.file.url, &path, &config.file.sha1, config.file.size)
                {
                    fetches.push(fetch);
                }
                Some(InstalledLogging {
                    argument: config.argument.clone(),
                    path,
                })
            }
            None => None,
        };

        let assets_index_id = version.asset_index_id().to_owned();
        check_component("asset index id", &assets_index_id)?;
        let assets = match &version.asset_index {
            Some(index_ref) => {
                let dest = self.layout.asset_index_file(&assets_index_id);
                let control = job.net_control("metadata", 0);
                let spec = Download::new(&index_ref.url, &dest)
                    .size(index_ref.size)
                    .max_bytes(MAX_ASSET_INDEX_BYTES as u64);
                let spec = with_sha1(spec, &index_ref.sha1);
                self.client.download(&spec, &control).map_err(Error::from)?;
                let bytes = read_capped(&dest, MAX_ASSET_INDEX_BYTES as u64)?;
                let index = AssetIndex::parse(&bytes)?;
                let mut plan = index.plan(&assets_index_id)?;
                let assets_dir = self.layout.assets_dir();
                let full = self.options.verify == Verify::Full;
                plan.retain_missing(|o| {
                    full || !has_size(&assets_dir.join(o.store_path()), o.size)
                });
                for object in &plan.downloads {
                    let spec = with_sha1(
                        Download::new(
                            object.url(),
                            join_relative(&assets_dir, &object.store_path()),
                        )
                        .size(object.size),
                        &object.hash,
                    );
                    fetches.push(Fetch {
                        spec,
                        size: object.size,
                    });
                }
                Some(plan)
            }
            None => None,
        };
        let asset_layout = assets.as_ref().map_or(AssetLayout::Objects, |a| a.layout);

        self.download_batch(&fetches, job)?;

        job.check()?;
        job.report(
            "natives",
            0,
            Some(plan.natives.len() as u64),
            ProgressUnit::Items,
        );
        let natives_dir = self.layout.natives_dir(&version.client_jar);
        natives::extract_all(&plan.natives, &libraries_dir, &natives_dir, job)?;

        if let Some(assets) = &assets {
            restore_assets(assets, &self.layout.assets_dir(), game_dir, job)?;
        }

        Ok(Installed {
            id: id.to_owned(),
            version,
            platform,
            libraries: plan,
            client_jar,
            natives_dir,
            assets_index_id,
            asset_layout,
            logging,
            warnings,
        })
    }

    fn library_fetch(&self, base: &Path, file: &LibraryFile) -> Result<Option<Fetch>> {
        let dest = join_relative(base, &file.path);
        let known = fs::metadata(&dest).ok().filter(|m| m.is_file());
        let Some(url) = &file.url else {
            return self
                .check_unfetchable(&dest, file, known.is_some())
                .map(|()| None);
        };
        let full = self.options.verify == Verify::Full && file.sha1.is_some();
        if !full && known.is_some_and(|m| file.size.is_none_or(|s| s == m.len())) {
            return Ok(None);
        }
        let mut spec = Download::new(url, dest);
        if let Some(size) = file.size {
            spec = spec.size(size);
        }
        if let Some(sha1) = &file.sha1 {
            spec = with_sha1(spec, sha1);
        }
        Ok(Some(Fetch {
            spec,
            size: file.size.unwrap_or(0),
        }))
    }

    /// A library without an address cannot be downloaded again: its loader's installer made it.
    /// It must be there, and a full check hashes it too.
    fn check_unfetchable(&self, dest: &Path, file: &LibraryFile, exists: bool) -> Result<()> {
        if !exists {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!(
                    "library file {} is missing and has no download address: install the mod loader again",
                    file.path
                ),
            ));
        }
        let wrong_size = fs::metadata(dest)
            .ok()
            .is_some_and(|m| file.size.is_some_and(|s| s != m.len()));
        let wrong_hash = self.options.verify == Verify::Full
            && match &file.sha1 {
                Some(expected) => !sha1_matches(dest, expected)?,
                None => false,
            };
        if wrong_size || wrong_hash {
            return Err(Error::new(
                ErrorKind::Integrity,
                format!(
                    "library file {} does not match its checksum and has no download address: install the mod loader again",
                    file.path
                ),
            ));
        }
        Ok(())
    }

    fn checked_fetch(&self, url: &str, dest: &Path, sha1: &str, size: u64) -> Option<Fetch> {
        let full = self.options.verify == Verify::Full;
        if !full && has_size(dest, size) {
            return None;
        }
        Some(Fetch {
            spec: with_sha1(Download::new(url, dest).size(size), sha1),
            size,
        })
    }

    fn download_batch(&self, fetches: &[Fetch], job: &Job) -> Result<()> {
        if fetches.is_empty() {
            return Ok(());
        }
        let total: u64 = fetches.iter().map(|f| f.size).sum();
        let specs: Vec<Download> = fetches.iter().map(|f| f.spec.clone()).collect();
        let control = job.net_control("downloading", total);
        let results = self.client.download_all(&specs, &control);
        let mut first = None;
        for error in results.into_iter().filter_map(std::result::Result::err) {
            if first.is_none()
                || (matches!(first, Some(NetError::Cancelled)) && error != NetError::Cancelled)
            {
                first = Some(error);
            }
        }
        match first {
            None => Ok(()),
            Some(_) if job.is_cancelled() => Err(Error::cancelled()),
            Some(e) => Err(e.into()),
        }
    }
}

/// Whether the file's SHA-1 is `expected` (hex, any case).
fn sha1_matches(path: &Path, expected: &str) -> Result<bool> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let mut file = fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| io_error(path, &e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(actual.eq_ignore_ascii_case(expected))
}

/// Loads version JSONs from disk, or from the network through the manifest.
struct Metadata<'a> {
    installer: &'a Installer,
    job: &'a Job,
    manifest: Option<VersionManifest>,
}

impl Metadata<'_> {
    fn load(&mut self, id: &str) -> Result<VersionJson> {
        check_component("version id", id)?;
        let path = self.installer.layout.version_json(id);
        if let Ok(bytes) = read_capped(&path, MAX_VERSION_BYTES as u64)
            && let Ok(json) = VersionJson::parse(&bytes)
            && json.id == id
        {
            return Ok(json);
        }
        let (url, sha1) = {
            let manifest = self.manifest()?;
            let entry = manifest
                .find(id)
                .ok_or_else(|| Error::not_found(format!("game version {id}")))?;
            (entry.url.clone(), entry.sha1.clone())
        };
        let control = self.job.net_control("metadata", 0);
        let spec = with_sha1(
            Download::new(url, &path).max_bytes(MAX_VERSION_BYTES as u64),
            &sha1,
        );
        self.installer
            .client
            .download(&spec, &control)
            .map_err(Error::from)?;
        let bytes = read_capped(&path, MAX_VERSION_BYTES as u64)?;
        let json = VersionJson::parse(&bytes)?;
        if json.id != id {
            let _ = fs::remove_file(&path);
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("version JSON of {id} names itself {}", json.id),
            ));
        }
        Ok(json)
    }

    fn manifest(&mut self) -> Result<&VersionManifest> {
        if self.manifest.is_none() {
            let options = GetOptions {
                max_bytes: Some(MAX_MANIFEST_BYTES as u64),
                max_age: Some(self.installer.options.manifest_max_age),
                use_cache: true,
            };
            let control = self.job.net_control("metadata", 0);
            let fetched = self
                .installer
                .client
                .get_bytes(&self.installer.options.manifest_url, &options, &control)
                .map_err(Error::from)?;
            self.manifest = Some(VersionManifest::parse(&fetched.bytes)?);
        }
        self.manifest
            .as_ref()
            .ok_or_else(|| Error::internal("manifest not loaded"))
    }
}

fn with_sha1(spec: Download, sha1: &str) -> Download {
    match Checksum::sha1(sha1) {
        Some(c) => spec.checksum(c),
        None => spec,
    }
}

fn has_size(path: &Path, size: u64) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == size)
}

fn io_error(path: &Path, e: &std::io::Error) -> Error {
    let kind = if e.kind() == std::io::ErrorKind::NotFound {
        ErrorKind::NotFound
    } else {
        ErrorKind::Io
    };
    Error::new(kind, format!("{}: {e}", path.display()))
}

/// Reads a file that must not be larger than `cap` bytes.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::metadata(path).map_err(|e| io_error(path, &e))?;
    if meta.len() > cap {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("{} is larger than {cap} bytes", path.display()),
        ));
    }
    fs::read(path).map_err(|e| io_error(path, &e))
}

/// The file name for a logging configuration: its id when that is a safe component, else the
/// last part of its URL.
fn file_name(url: &str, id: &str) -> Result<String> {
    let from_url = url.rsplit('/').next().unwrap_or_default();
    for candidate in [id, from_url] {
        if crate::layout::is_safe_component(candidate) {
            return Ok(candidate.to_owned());
        }
    }
    Err(Error::new(
        ErrorKind::Integrity,
        "logging configuration has no safe file name",
    ))
}

/// Copies the objects of a `virtual` or `map_to_resources` layout to where the game reads them.
fn restore_assets(
    plan: &AssetPlan,
    assets_dir: &Path,
    game_dir: Option<&Path>,
    job: &Job,
) -> Result<()> {
    let total = plan.restores.len() as u64;
    for (done, restore) in plan.restores.iter().enumerate() {
        if done % 64 == 0 {
            job.check()?;
            job.report("assets", done as u64, Some(total), ProgressUnit::Items);
        }
        let base = match plan.layout {
            AssetLayout::Resources => match game_dir {
                Some(dir) => dir,
                None => continue,
            },
            _ => assets_dir,
        };
        let dest = join_relative(base, &restore.target);
        if has_size(&dest, restore.size) {
            continue;
        }
        let source = join_relative(assets_dir, &restore.source);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        let temp = dest.with_extension("vl-part");
        fs::copy(&source, &temp).map_err(|e| io_error(&source, &e))?;
        fs::rename(&temp, &dest).map_err(|e| io_error(&dest, &e))?;
    }
    Ok(())
}
