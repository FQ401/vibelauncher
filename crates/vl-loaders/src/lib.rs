//! Mod loader sources: the version list of a loader for a game version, and its launch profile.
//!
//! Fabric ([`fabric`]) and Quilt ([`quilt`]) publish a ready launch profile, so a source is a URL
//! scheme ([`meta::Endpoints`]) plus the parsers of `vl-meta`. [`Loaders`] fetches through `vl-net`
//! (HTTP cache with `ETag`, retries, size caps, offline reads) and keeps the normalized profile of
//! every loader version it used on disk, so an installed instance launches without the network.
//!
//! A profile on disk is a [`CachedProfile`] at `<profiles dir>/loader/<uid>/<game>/<loader>.json`.
//! A fresh entry is used as is; an old one is renewed from the source, and when the source cannot
//! be reached the old one is used. Hashes of the loader's libraries come from the profile; the
//! files are downloaded and verified by `vl-launch` like any other library.
//!
//! NeoForge ([`neoforge`], for 1.20.1 also its `net.neoforged:forge` builds) and Forge ([`forge`]) are published as installer
//! jars in a Maven repository: the version list is the repository's metadata (for Forge, with its
//! promotions), the profile is the installer's `version.json`, and [`Loaders::install`] runs the
//! installer's processors (Java programs that patch the game jar) through a
//! [`processors::ProcessorRunner`]; [`installer`] holds what both share.
//! [`Loaders::needs_install`] tells which loaders need that step after the game files are
//! installed. Legacy Forge installers (most builds before Minecraft 1.13) have no processors:
//! their installation puts the carried Forge jar, the game's copy of the vanilla jar without
//! `META-INF/` (1.5.2) and the FML files in place. Forge before Minecraft 1.5.2 is
//! `Unsupported`.

pub mod fabric;
pub mod forge;
mod forge_legacy;
pub mod installer;
pub mod meta;
pub mod neoforge;
pub mod processors;
pub mod quilt;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vl_core::component::ComponentKind;
use vl_core::{Error, Result};
use vl_meta::loader::{
    CachedProfile, FetchOutcome, LoaderProfile, LoaderVersion, LoaderVersions, ProfileSource,
    choose_source, profile_cache_key, resolve_profile,
};
use vl_meta::version::MAX_VERSION_BYTES;
use vl_net::{Client, Control, GetOptions, NetError};

use meta::Endpoints;

/// How [`Loaders`] works.
#[derive(Debug, Clone)]
pub struct LoadersOptions {
    /// The Fabric meta server.
    pub fabric: Endpoints,
    /// The Quilt meta server.
    pub quilt: Endpoints,
    /// The NeoForge repository.
    pub neoforge: neoforge::Maven,
    /// The Forge repository and promotions.
    pub forge: forge::Maven,
    /// How long a fetched version list is used without asking the server again.
    pub versions_max_age: Duration,
    /// How long a stored profile is used without asking the server again.
    pub profile_max_age: Duration,
}

impl Default for LoadersOptions {
    fn default() -> Self {
        Self {
            fabric: fabric::endpoints(),
            quilt: quilt::endpoints(),
            neoforge: neoforge::Maven::default(),
            forge: forge::Maven::default(),
            versions_max_age: Duration::from_secs(10 * 60),
            profile_max_age: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Loader version lists and launch profiles.
#[derive(Clone)]
pub struct Loaders {
    client: Client,
    profiles_dir: PathBuf,
    options: LoadersOptions,
}

impl Loaders {
    /// Sources that fetch through `client` and keep profiles below `profiles_dir`.
    pub fn new(client: Client, profiles_dir: impl Into<PathBuf>, options: LoadersOptions) -> Self {
        Self {
            client,
            profiles_dir: profiles_dir.into(),
            options,
        }
    }

    fn endpoints(&self, kind: ComponentKind) -> Result<&Endpoints> {
        match kind {
            ComponentKind::Fabric => Ok(&self.options.fabric),
            ComponentKind::Quilt => Ok(&self.options.quilt),
            other => Err(Error::unsupported(format!(
                "{} is not available yet",
                other.display_name()
            ))),
        }
    }

    /// The versions of loader `kind` that support `game_version`, newest first.
    ///
    /// A game version the loader does not know gives an empty list ([`LoaderVersions::check`]
    /// turns that into `Unsupported`).
    ///
    /// # Errors
    ///
    /// `Unsupported` for a loader without a source, `Network` / `Offline` when the list cannot be
    /// fetched and none is cached, `Integrity` for a damaged answer, `Cancelled`.
    pub fn versions(
        &self,
        kind: ComponentKind,
        game_version: &str,
        control: &Control,
    ) -> Result<LoaderVersions> {
        match kind {
            ComponentKind::NeoForge => return neoforge::versions(self, game_version, control),
            ComponentKind::Forge => return forge::versions(self, game_version, control),
            _ => {}
        }
        let url = self.endpoints(kind)?.versions_url(game_version);
        let options = GetOptions {
            max_bytes: Some(MAX_VERSION_BYTES as u64),
            max_age: Some(self.options.versions_max_age),
            use_cache: true,
        };
        match self.client.get_bytes(&url, &options, control) {
            Ok(fetched) => LoaderVersions::parse_for_game(kind, game_version, &fetched.bytes),
            // The meta servers answer an unknown game version with 400 or 404.
            Err(NetError::Status {
                status: 400 | 404, ..
            }) => LoaderVersions::new(kind, game_version, Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Picks the loader version to use: `requested` when given and available, else the default
    /// one ([`LoaderVersions::recommended`]: the recommended build, else the newest stable one).
    /// A Forge version may be given in its Maven form (`1.20.1-47.3.0`), and so may a NeoForge
    /// version of 1.20.1.
    ///
    /// # Errors
    ///
    /// `Unsupported` when the loader does not support the game version, `NotFound` when
    /// `requested` is not available for it, and the errors of [`versions`](Self::versions).
    pub fn pick(
        &self,
        kind: ComponentKind,
        game_version: &str,
        requested: Option<&str>,
        control: &Control,
    ) -> Result<LoaderVersion> {
        let list = self.versions(kind, game_version, control)?;
        let requested = match requested {
            Some(v) if kind == ComponentKind::Forge => {
                Some(vl_meta::forge::short_version(game_version, v))
            }
            Some(v) if kind == ComponentKind::NeoForge => {
                Some(vl_meta::neoforge::short_version(game_version, v))
            }
            other => other,
        };
        let found = match requested {
            Some(version) => list.check(version)?,
            None => list.recommended().ok_or_else(|| {
                Error::unsupported(format!(
                    "{} does not support Minecraft {game_version}",
                    kind.display_name()
                ))
            })?,
        };
        Ok(found.clone())
    }

    /// The launch profile of a loader version for a game version, from the profile store, the
    /// source, or the old stored copy when the source cannot be reached.
    ///
    /// # Errors
    ///
    /// `Unsupported` for a loader without a source, `NotFound` when the source does not know the
    /// pair, `Network` / `Offline` without a stored copy, `Integrity` for a damaged answer,
    /// `Cancelled`.
    pub fn profile(
        &self,
        kind: ComponentKind,
        game_version: &str,
        loader_version: &str,
        control: &Control,
    ) -> Result<LoaderProfile> {
        self.profile_at(kind, game_version, loader_version, control, unix_now())
    }

    /// [`profile`](Self::profile) at a given time (Unix seconds).
    ///
    /// # Errors
    ///
    /// As [`profile`](Self::profile).
    pub fn profile_at(
        &self,
        kind: ComponentKind,
        game_version: &str,
        loader_version: &str,
        control: &Control,
        now: i64,
    ) -> Result<LoaderProfile> {
        match kind {
            ComponentKind::NeoForge => {
                return neoforge::profile(self, game_version, loader_version, control, now);
            }
            ComponentKind::Forge => {
                return forge::profile(self, game_version, loader_version, control, now);
            }
            _ => {}
        }
        let endpoints = self.endpoints(kind)?;
        let key = profile_cache_key(kind, game_version, loader_version)?;
        let path = key
            .split('/')
            .fold(self.profiles_dir.clone(), |p, part| p.join(part));
        let cached = fs::read(&path)
            .ok()
            .and_then(|bytes| CachedProfile::decode(&bytes).ok())
            .filter(|c| {
                c.profile.kind == kind
                    && c.profile.loader_version == loader_version
                    && c.profile.inherits_from == game_version
            });
        let max_age = i64::try_from(self.options.profile_max_age.as_secs()).unwrap_or(i64::MAX);
        if choose_source(cached.as_ref(), now, max_age) == ProfileSource::Cache
            && let Some(c) = &cached
        {
            return Ok(c.profile.clone());
        }
        let url = endpoints.profile_url(game_version, loader_version);
        let options = GetOptions {
            max_bytes: Some(MAX_VERSION_BYTES as u64),
            max_age: None,
            use_cache: true,
        };
        let outcome = match self.client.get_bytes(&url, &options, control) {
            Ok(fetched) => FetchOutcome::Fresh(fetched.bytes, None),
            Err(NetError::Cancelled) => return Err(Error::cancelled()),
            Err(NetError::Status {
                status: 400 | 404, ..
            }) => FetchOutcome::Failed(Error::not_found(format!(
                "{} {loader_version} for Minecraft {game_version}",
                kind.display_name()
            ))),
            Err(e) => FetchOutcome::Failed(e.into()),
        };
        let resolved = resolve_profile(kind, game_version, loader_version, cached, outcome, now)?;
        // The store is best effort: a profile that cannot be saved is still used.
        if let Ok(bytes) = resolved.encode() {
            let _ = vl_store::write_atomic(&path, &bytes);
        }
        Ok(resolved.profile)
    }
}

impl Loaders {
    /// Whether loader `kind` needs [`install`](Self::install) after the game files of its
    /// profile are installed.
    pub fn needs_install(kind: ComponentKind) -> bool {
        matches!(kind, ComponentKind::NeoForge | ComponentKind::Forge)
    }

    /// Finishes the installation of a loader version: for NeoForge and Forge, puts the libraries
    /// the installer carries in place, downloads the processors' libraries and runs the client
    /// processors (skipped when an earlier run left matching files); for a legacy Forge
    /// installer, puts its files in place (see [`forge`]). Call it after the vanilla
    /// game is installed and before the libraries of the loader profile are: some of them are
    /// made here. Loaders that need no such step return at once with `None`.
    ///
    /// # Errors
    ///
    /// `Network` / `NotFound` / `Integrity` for the installer and its libraries, `Java` when a
    /// processor cannot start or fails, `Integrity` when its outputs do not match, `Io`,
    /// `Cancelled`.
    pub fn install(
        &self,
        kind: ComponentKind,
        game_version: &str,
        loader_version: &str,
        ctx: &installer::InstallContext<'_>,
    ) -> Result<Option<installer::Installation>> {
        match kind {
            ComponentKind::NeoForge => {
                neoforge::install(self, game_version, loader_version, ctx).map(Some)
            }
            ComponentKind::Forge => {
                forge::install(self, game_version, loader_version, ctx).map(Some)
            }
            _ => Ok(None),
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
