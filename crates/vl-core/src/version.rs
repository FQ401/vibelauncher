//! Game versions as the launcher offers them.

use crate::component::ComponentKind;
use crate::instance::LoaderRequest;
use serde::{Deserialize, Serialize};

/// The kind of a game version (the version manifest's `type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VersionKind {
    /// A release.
    Release,
    /// A snapshot, pre-release or release candidate.
    Snapshot,
    /// A beta from before 1.0.
    OldBeta,
    /// An alpha (or older) from before the betas.
    OldAlpha,
}

impl VersionKind {
    /// The kind a version manifest `type` denotes (`release`, `snapshot`, `old_beta`,
    /// `old_alpha`); `None` for an unknown type.
    pub fn from_mojang_name(name: &str) -> Option<Self> {
        match name {
            "release" => Some(Self::Release),
            "snapshot" => Some(Self::Snapshot),
            "old_beta" => Some(Self::OldBeta),
            "old_alpha" => Some(Self::OldAlpha),
            _ => None,
        }
    }
}

/// One game version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameVersion {
    /// The version id, e.g. `26.3`.
    pub id: String,
    /// The kind.
    pub kind: VersionKind,
    /// The release time as given upstream (RFC 3339).
    pub release_time: String,
}

/// Which version kinds a list includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionFilter {
    /// Releases.
    pub releases: bool,
    /// Snapshots.
    pub snapshots: bool,
    /// Old betas and alphas.
    pub old: bool,
}

impl Default for VersionFilter {
    fn default() -> Self {
        Self {
            releases: true,
            snapshots: false,
            old: false,
        }
    }
}

impl VersionFilter {
    /// Whether a version of `kind` passes the filter.
    pub fn accepts(&self, kind: VersionKind) -> bool {
        match kind {
            VersionKind::Release => self.releases,
            VersionKind::Snapshot => self.snapshots,
            VersionKind::OldBeta | VersionKind::OldAlpha => self.old,
        }
    }
}

/// Version-list commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VersionCommand {
    /// Versions from the cached list, newest first → `Response::Versions`.
    List(VersionFilter),
    /// Fetches the list from upstream → `Response::Task`.
    Refresh,
    /// Installs a version's files (metadata, client, libraries, natives, assets) without
    /// starting the game → `Response::Task`.
    ///
    /// With a loader, its profile and libraries are installed as well.
    Install {
        /// The version id, as in the version manifest.
        id: String,
        /// The mod loader to install with it.
        #[serde(default)]
        loader: Option<LoaderRequest>,
    },
    /// The versions of a mod loader that support a game version, newest first →
    /// `Response::LoaderVersions`.
    LoaderVersions {
        /// The loader.
        loader: ComponentKind,
        /// The game version id.
        game_version: String,
    },
}

/// One version of a mod loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderVersionInfo {
    /// The loader's version string.
    pub version: String,
    /// Whether it is a stable release. Forge has no such division: all its versions are stable.
    pub stable: bool,
    /// Whether it is the build the loader's authors recommend for the game version.
    #[serde(default)]
    pub recommended: bool,
    /// Whether it is the newest version for the game version.
    #[serde(default)]
    pub latest: bool,
}

/// Changes to the version list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VersionEvent {
    /// The cached list was replaced; list it again.
    ListChanged,
}
