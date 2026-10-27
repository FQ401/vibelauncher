//! Game versions as the launcher offers them.

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
}

/// Changes to the version list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VersionEvent {
    /// The cached list was replaced; list it again.
    ListChanged,
}
