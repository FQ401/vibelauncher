//! Forge: `https://maven.minecraftforge.net`, artifact `net.minecraftforge:forge`, for Minecraft
//! 1.5.2 and newer.
//!
//! - The version list is the artifact's `maven-metadata.xml`, and the recommended builds come
//!   from `promotions_slim.json` (both through the HTTP cache), read by `vl_meta::forge`. The
//!   promotions are best effort: without them no version is recommended.
//! - A version is installed from its installer jar, `forge-<maven version>-installer.jar`; its
//!   files live in `<profiles dir>/forge/<game>-<forge>/`. Some builds before Minecraft 1.13
//!   carry a branch in their Maven version (`1.7.10-10.13.4.1614-1.7.10`), found in the list.
//! - An installer with processors goes through [`crate::installer`]; a legacy one (most builds
//!   before Minecraft 1.13) through `crate::forge_legacy`. Which one it is shows in its
//!   `install_profile.json`.
//! - Game versions before 1.5.2 are `Unsupported`.

use vl_core::component::ComponentKind;
use vl_core::{Error, Result};
use vl_meta::forge::{self as meta, MAX_METADATA_BYTES, MAX_PROMOTIONS_BYTES};
use vl_meta::forge_legacy::{InstallerFormat, installer_format};
use vl_meta::install_profile::MAX_INSTALL_PROFILE_BYTES;
use vl_meta::loader::{LoaderProfile, LoaderVersions};
use vl_net::{Control, GetOptions, NetError};

use crate::installer::{self, InstallContext, Installation, Source};
use crate::{Loaders, forge_legacy};

/// The Forge Maven repository.
pub const MAVEN_URL: &str = "https://maven.minecraftforge.net";

/// The Forge promotions document.
pub const PROMOTIONS_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";

/// Mirrors of the FML libraries that FML for Minecraft 1.5.2 downloaded from
/// `files.minecraftforge.net/fmllibs/`, which no longer serves them. Every file is checked
/// against the SHA-1 the Forge jar names.
pub const FML_LIBRARY_MIRRORS: [&str; 1] =
    ["https://web.archive.org/web/2015/http://files.minecraftforge.net/fmllibs/"];

/// The Forge repository, promotions and FML library mirrors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maven {
    base: String,
    promotions: String,
    fml_libraries: Vec<String>,
}

impl Default for Maven {
    fn default() -> Self {
        Self::new(MAVEN_URL, PROMOTIONS_URL)
    }
}

impl Maven {
    /// The repository at `base` and the promotions document at `promotions`, with the default
    /// FML library mirrors.
    pub fn new(base: impl Into<String>, promotions: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            promotions: promotions.into(),
            fml_libraries: FML_LIBRARY_MIRRORS
                .iter()
                .map(|m| (*m).to_owned())
                .collect(),
        }
    }

    /// The same with other FML library mirrors, tried in order.
    #[must_use]
    pub fn with_fml_libraries(mut self, mirrors: Vec<String>) -> Self {
        self.fml_libraries = mirrors;
        self
    }

    /// The FML library mirrors.
    pub fn fml_libraries(&self) -> &[String] {
        &self.fml_libraries
    }

    /// The URL of the version list.
    pub fn metadata_url(&self) -> String {
        format!("{}/net/minecraftforge/forge/maven-metadata.xml", self.base)
    }

    /// The URL of the promotions document.
    pub fn promotions_url(&self) -> &str {
        &self.promotions
    }

    /// The URL of the installer of Forge `forge` for `game`, for a Maven version without a
    /// branch.
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

    /// The URL of the installer of the Maven version `maven` (with its branch, if any).
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a version that is not a Forge Maven version.
    pub fn installer_url_of(&self, maven: &str) -> Result<String> {
        Ok(format!("{}/{}", self.base, meta::installer_path_of(maven)?))
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

/// The full Maven list.
fn maven_versions(loaders: &Loaders, control: &Control) -> Result<Vec<String>> {
    let options = GetOptions {
        max_bytes: Some(MAX_METADATA_BYTES as u64),
        max_age: Some(loaders.options.versions_max_age),
        use_cache: true,
    };
    let fetched = loaders
        .client
        .get_bytes(&loaders.options.forge.metadata_url(), &options, control)
        .map_err(Error::from)?;
    meta::parse_maven_versions(&fetched.bytes)
}

/// The Forge versions for `game`, newest first.
pub(crate) fn versions(loaders: &Loaders, game: &str, control: &Control) -> Result<LoaderVersions> {
    meta::check_game(game)?;
    let all = maven_versions(loaders, control)?;
    let options = GetOptions {
        max_bytes: Some(MAX_PROMOTIONS_BYTES as u64),
        max_age: Some(loaders.options.versions_max_age),
        use_cache: true,
    };
    let promotions =
        match loaders
            .client
            .get_bytes(loaders.options.forge.promotions_url(), &options, control)
        {
            Ok(f) => meta::parse_promotions(&f.bytes).ok(),
            Err(NetError::Cancelled) => return Err(Error::cancelled()),
            Err(_) => None,
        };
    meta::versions_for_game(game, &all, promotions.as_ref())
}

/// Downloads the installer of a version for a game before 1.13 (whose Maven version may have a
/// branch) and tells its format.
fn legacy_format(
    loaders: &Loaders,
    source: &mut Source,
    game: &str,
    control: &Control,
) -> Result<InstallerFormat> {
    let jar = source.dir.join("installer.jar");
    if !jar.is_file() {
        let all = maven_versions(loaders, control)?;
        let maven = meta::find_maven_version(game, &source.version, &all).ok_or_else(|| {
            Error::not_found(format!("Forge {} for Minecraft {game}", source.version))
        })?;
        source.installer_url = loaders.options.forge.installer_url_of(maven)?;
    }
    let jar = installer::download_installer(loaders, source, control)?;
    let mut zip = installer::open_zip(&jar)?;
    let bytes = installer::read_entry(
        &mut zip,
        &jar,
        "install_profile.json",
        MAX_INSTALL_PROFILE_BYTES as u64,
    )?;
    installer_format(&bytes)
}

/// The launch profile of Forge `forge` for `game`.
pub(crate) fn profile(
    loaders: &Loaders,
    game: &str,
    forge: &str,
    control: &Control,
    now: i64,
) -> Result<LoaderProfile> {
    let mut source = source(loaders, game, forge)?;
    if !meta::is_legacy_game(game) {
        return installer::profile(loaders, &source, game, control, now);
    }
    if let Some(stored) = installer::stored_profile(loaders, &source, game)? {
        return Ok(stored);
    }
    match legacy_format(loaders, &mut source, game, control)? {
        InstallerFormat::Processors => installer::profile(loaders, &source, game, control, now),
        InstallerFormat::Legacy => {
            let profile = forge_legacy::profile(&source, game)?;
            installer::store_profile(loaders, &source, game, &profile, now)?;
            Ok(profile)
        }
    }
}

/// Installs Forge `forge` for `game`: runs the client processors of an installer with
/// processors (unless an earlier run left files that still match), or puts the files of a
/// legacy installer in place.
pub(crate) fn install(
    loaders: &Loaders,
    game: &str,
    forge: &str,
    ctx: &InstallContext<'_>,
) -> Result<Installation> {
    let mut source = source(loaders, game, forge)?;
    if meta::is_legacy_game(game)
        && legacy_format(loaders, &mut source, game, ctx.control)? == InstallerFormat::Legacy
    {
        return forge_legacy::install(loaders, &source, game, ctx);
    }
    installer::install(loaders, &source, game, ctx)
}
