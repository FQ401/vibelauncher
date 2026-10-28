//! Forge for Minecraft 1.13 and newer: `https://maven.minecraftforge.net`, artifact
//! `net.minecraftforge:forge`.
//!
//! - The version list is the artifact's `maven-metadata.xml`, and the recommended builds come
//!   from `promotions_slim.json` (both through the HTTP cache), read by `vl_meta::forge`. The
//!   promotions are best effort: without them every version counts as stable.
//! - A version is installed from its installer jar, `forge-<game>-<forge>-installer.jar`, by
//!   [`crate::installer`]; its files live in `<profiles dir>/forge/<game>-<forge>/`.
//! - Older game versions are `Unsupported`.

use vl_core::component::ComponentKind;
use vl_core::{Error, Result};
use vl_meta::forge::{self as meta, MAX_METADATA_BYTES, MAX_PROMOTIONS_BYTES};
use vl_meta::loader::{LoaderProfile, LoaderVersions};
use vl_net::{Control, GetOptions, NetError};

use crate::Loaders;
use crate::installer::{self, InstallContext, Installation, Source};

/// The Forge Maven repository.
pub const MAVEN_URL: &str = "https://maven.minecraftforge.net";

/// The Forge promotions document.
pub const PROMOTIONS_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";

/// The Forge repository and promotions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maven {
    base: String,
    promotions: String,
}

impl Default for Maven {
    fn default() -> Self {
        Self::new(MAVEN_URL, PROMOTIONS_URL)
    }
}

impl Maven {
    /// The repository at `base` and the promotions document at `promotions`.
    pub fn new(base: impl Into<String>, promotions: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            promotions: promotions.into(),
        }
    }

    /// The URL of the version list.
    pub fn metadata_url(&self) -> String {
        format!("{}/net/minecraftforge/forge/maven-metadata.xml", self.base)
    }

    /// The URL of the promotions document.
    pub fn promotions_url(&self) -> &str {
        &self.promotions
    }

    /// The URL of the installer of Forge `forge` for `game`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a version that cannot be a path part.
    pub fn installer_url(&self, game: &str, forge: &str) -> Result<String> {
        Ok(format!(
            "{}/{}",
            self.base,
            meta::installer_path(game, forge)?
        ))
    }
}

/// The installer source of Forge `forge` (short or full form) for `game`.
fn source(loaders: &Loaders, game: &str, forge: &str) -> Result<Source> {
    meta::check_game(game)?;
    let full = meta::maven_version(game, forge)?;
    Ok(Source {
        kind: ComponentKind::Forge,
        version: meta::short_version(game, forge).to_owned(),
        dir: loaders.profiles_dir.join("forge").join(&full),
        installer_url: loaders.options.forge.installer_url(game, forge)?,
        make_profile: meta::profile,
    })
}

/// The Forge versions for `game`, newest first.
pub(crate) fn versions(loaders: &Loaders, game: &str, control: &Control) -> Result<LoaderVersions> {
    meta::check_game(game)?;
    let maven = &loaders.options.forge;
    let options = GetOptions {
        max_bytes: Some(MAX_METADATA_BYTES as u64),
        max_age: Some(loaders.options.versions_max_age),
        use_cache: true,
    };
    let fetched = loaders
        .client
        .get_bytes(&maven.metadata_url(), &options, control)
        .map_err(Error::from)?;
    let all = meta::parse_maven_versions(&fetched.bytes)?;
    let options = GetOptions {
        max_bytes: Some(MAX_PROMOTIONS_BYTES as u64),
        ..options
    };
    let promotions = match loaders
        .client
        .get_bytes(maven.promotions_url(), &options, control)
    {
        Ok(f) => meta::parse_promotions(&f.bytes).ok(),
        Err(NetError::Cancelled) => return Err(Error::cancelled()),
        Err(_) => None,
    };
    meta::versions_for_game(game, &all, promotions.as_ref())
}

/// The launch profile of Forge `forge` for `game`.
pub(crate) fn profile(
    loaders: &Loaders,
    game: &str,
    forge: &str,
    control: &Control,
    now: i64,
) -> Result<LoaderProfile> {
    installer::profile(loaders, &source(loaders, game, forge)?, game, control, now)
}

/// Runs the client processors of Forge `forge` for `game`, unless an earlier run left files that
/// still match.
pub(crate) fn install(
    loaders: &Loaders,
    game: &str,
    forge: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    installer::install(loaders, &source(loaders, game, forge)?, game, ctx)
}
