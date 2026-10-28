//! Online platforms that host mods, packs and other projects.
//!
//! A platform is a [`CatalogSource`]: search, one project, the versions of a project, and the
//! versions that own given files (to tell where an installed file came from). [`ModrinthSource`]
//! and [`CurseForgeSource`] are implemented; a platform is a module with its own source and a
//! variant of `vl_core::catalog::Provider`, and [`Platforms::source`] hands it out. CurseForge
//! needs the user's API key ([`curseforge::ApiKeySource`]); without one its calls fail with
//! `ErrorKind::NoApiKey` and Modrinth works as before.
//!
//! Requests go through `vl-net` (HTTP cache, retries, size caps, offline reads); the request URLs
//! and the parsing of responses live in the pure crates `vl-modrinth` and `vl-curseforge`. A
//! source paces itself with a
//! [`RateLimiter`] that follows the server's rate-limit headers, so the launcher never exceeds
//! the allowed rate. The client is blocking: callers run it inside a task or on a worker thread.

pub mod curseforge;
pub mod modrinth;
pub mod rate;

use std::sync::Arc;

use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectVersion, Provider, ReleaseChannel, SearchPage,
    SearchQuery, VersionFilter,
};
use vl_core::{Error, Result};
use vl_net::{Client, Control};

pub use curseforge::{CurseForgeOptions, CurseForgeSource, KeyringApiKey};
pub use modrinth::{ModrinthOptions, ModrinthSource};
pub use rate::{Clock, RateConfig, RateLimiter, SystemClock};

/// What every platform offers.
pub trait CatalogSource: Send + Sync {
    /// Which platform this is.
    fn provider(&self) -> Provider;

    /// Searches projects.
    fn search(&self, query: &SearchQuery, control: &Control) -> Result<SearchPage>;

    /// One project by id or slug.
    fn project(&self, id: &str, control: &Control) -> Result<Project>;

    /// The versions of a project, newest first.
    fn versions(
        &self,
        project: &str,
        filter: &VersionFilter,
        control: &Control,
    ) -> Result<Vec<ProjectVersion>>;

    /// The versions owning the files with these hashes; unknown hashes are left out.
    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        control: &Control,
    ) -> Result<Vec<HashMatch>>;

    /// For the files with these hashes, the newest version of the file's project that fits
    /// `filter` and is of one of `channels` (any when empty), in one batched request. The result
    /// is keyed by the asked hash; files whose project has no such version are left out. The
    /// answer is data: the caller checks that it fits and is newer.
    fn latest_versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        filter: &VersionFilter,
        channels: &[ReleaseChannel],
        control: &Control,
    ) -> Result<Vec<HashMatch>> {
        let _ = (algorithm, hashes, filter, channels, control);
        Err(Error::unsupported("update lookup by hash"))
    }
}

/// Every platform the launcher can ask.
pub struct Platforms {
    modrinth: ModrinthSource,
    curseforge: CurseForgeSource,
}

impl Platforms {
    /// All platforms over one network client. CurseForge reads its key from the environment
    /// variable, then from the OS keyring (opened on first use).
    pub fn new(client: Client) -> Self {
        let keys = curseforge::default_api_keys(Arc::new(KeyringApiKey::os()));
        Self::with_sources(
            ModrinthSource::new(client.clone(), ModrinthOptions::default()),
            CurseForgeSource::new(client, CurseForgeOptions::default(), Arc::new(keys)),
        )
    }

    /// All platforms with prepared sources (tests point them at another server).
    pub fn with_sources(modrinth: ModrinthSource, curseforge: CurseForgeSource) -> Self {
        Self {
            modrinth,
            curseforge,
        }
    }

    /// The source of `provider`.
    pub fn source(&self, provider: Provider) -> Result<&dyn CatalogSource> {
        match provider {
            Provider::Modrinth => Ok(&self.modrinth),
            Provider::CurseForge => Ok(&self.curseforge),
            other => Err(Error::unsupported(format!("platform {other:?}"))),
        }
    }

    /// CurseForge with what only it offers: categories, single files, fingerprint lookups.
    pub fn curseforge(&self) -> &CurseForgeSource {
        &self.curseforge
    }
}
