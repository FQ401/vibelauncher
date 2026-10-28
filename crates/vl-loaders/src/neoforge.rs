//! NeoForge: `https://maven.neoforged.net/releases`, artifact `net.neoforged:neoforge`.
//!
//! - The version list is the artifact's `maven-metadata.xml` (through the HTTP cache), mapped to
//!   game versions by `vl_meta::neoforge`. For Minecraft 1.20.1 it is merged with the list of the
//!   artifact `net.neoforged:forge` (`1.20.1-47.1.x`), whose versions are named in the short form
//!   (`47.1.106`); a failure to read that second list leaves the first.
//! - A version is installed from its installer jar, `neoforge-<version>-installer.jar` (for
//!   1.20.1, `forge-1.20.1-<version>-installer.jar`), by [`crate::installer`]; its files live in
//!   `<profiles dir>/neoforge/<version>/`.

use vl_core::component::ComponentKind;
use vl_core::{Error, Result};
use vl_meta::loader::{LoaderProfile, LoaderVersions};
use vl_meta::neoforge::{self as meta, MAX_METADATA_BYTES};
use vl_net::{Control, GetOptions, NetError};

use crate::Loaders;
use crate::installer::{self, Source};

pub use crate::installer::{InstallContext, Installation};

/// The NeoForge Maven repository.
pub const MAVEN_URL: &str = "https://maven.neoforged.net/releases";

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

    /// The URL of the version list of the `net.neoforged:forge` artifact (Minecraft 1.20.1).
    pub fn legacy_metadata_url(&self) -> String {
        format!("{}/net/neoforged/forge/maven-metadata.xml", self.base)
    }

    /// The URL of the installer of `version` for `game`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a version that cannot be a path part.
    pub fn installer_url(&self, game: &str, version: &str) -> Result<String> {
        Ok(format!(
            "{}/{}",
            self.base,
            meta::installer_path_for(game, version)?
        ))
    }
}

/// The installer source of NeoForge `version`.
fn source(loaders: &Loaders, game: &str, version: &str) -> Result<Source> {
    let version = meta::short_version(game, version);
    Ok(Source {
        kind: ComponentKind::NeoForge,
        version: version.to_owned(),
        dir: loaders.profiles_dir.join("neoforge").join(version),
        installer_url: loaders.options.neoforge.installer_url(game, version)?,
        make_profile: meta::profile,
    })
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
    let legacy = if meta::is_legacy_game(game) {
        match loaders.client.get_bytes(
            &loaders.options.neoforge.legacy_metadata_url(),
            &options,
            control,
        ) {
            Ok(f) => meta::parse_maven_versions(&f.bytes).unwrap_or_default(),
            Err(NetError::Cancelled) => return Err(Error::cancelled()),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    meta::versions_for_game_with_legacy(game, &all, &legacy)
}

/// The launch profile of NeoForge `version` for `game`.
pub(crate) fn profile(
    loaders: &Loaders,
    game: &str,
    version: &str,
    control: &Control,
    now: i64,
) -> Result<LoaderProfile> {
    installer::profile(
        loaders,
        &source(loaders, game, version)?,
        game,
        control,
        now,
    )
}

/// Runs the client processors of NeoForge `version` for `game`, unless an earlier run left
/// files that still match.
pub(crate) fn install(
    loaders: &Loaders,
    game: &str,
    version: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    installer::install(loaders, &source(loaders, game, version)?, game, ctx)
}
