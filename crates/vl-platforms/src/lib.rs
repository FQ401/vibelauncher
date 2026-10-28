//! Online platforms that host mods, packs and other projects.
//!
//! A platform is a [`CatalogSource`]: search, one project, the versions of a project, and the
//! versions that own given files (to tell where an installed file came from). [`ModrinthSource`]
//! is the one implemented; a second platform adds a module with its own source and a variant of
//! `vl_core::catalog::Provider`, and [`Platforms::source`] hands it out.
//!
//! Requests go through `vl-net` (HTTP cache, retries, size caps, offline reads); the request URLs
//! and the parsing of responses live in the pure crate `vl-modrinth`. A source paces itself with a
//! [`RateLimiter`] that follows the server's rate-limit headers, so the launcher never exceeds
//! the allowed rate. The client is blocking: callers run it inside a task or on a worker thread.

pub mod modrinth;
pub mod rate;

use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectVersion, Provider, SearchPage, SearchQuery,
    VersionFilter,
};
use vl_core::{Error, Result};
use vl_net::{Client, Control};

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
}

/// Every platform the launcher can ask.
pub struct Platforms {
    modrinth: ModrinthSource,
}

impl Platforms {
    /// All platforms over one network client.
    pub fn new(client: Client) -> Self {
        Self::with_modrinth(ModrinthSource::new(client, ModrinthOptions::default()))
    }

    /// All platforms with a prepared Modrinth source (tests point it at another server).
    pub fn with_modrinth(modrinth: ModrinthSource) -> Self {
        Self { modrinth }
    }

    /// The source of `provider`.
    pub fn source(&self, provider: Provider) -> Result<&dyn CatalogSource> {
        match provider {
            Provider::Modrinth => Ok(&self.modrinth),
            other => Err(Error::unsupported(format!("platform {other:?}"))),
        }
    }
}
