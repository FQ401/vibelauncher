//! Installing a catalog project into an instance: the choice of release channels, the plan that
//! is shown before anything is downloaded, and the request that starts it.
//!
//! The plan is plain data. Building it needs the network (versions, dependencies, file owners);
//! carrying it out is a task.

use serde::{Deserialize, Serialize};

use crate::catalog::{ProjectType, Provider, ReleaseChannel, VersionFile};
use crate::ids::InstanceId;
use crate::resources::ResourceKind;

/// The least stable release channel an install may pick. Within what is allowed, `release` is
/// preferred over `beta` over `alpha`, and the newest version wins inside a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MinStability {
    /// Releases only.
    Release,
    /// Releases and betas.
    #[default]
    Beta,
    /// Everything.
    Alpha,
}

impl MinStability {
    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Beta => "beta",
            Self::Alpha => "alpha",
        }
    }

    /// Parses [`MinStability::name`].
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Release, Self::Beta, Self::Alpha]
            .into_iter()
            .find(|s| s.name().eq_ignore_ascii_case(name))
    }

    /// How much `channel` is preferred: 3 for release, 2 beta, 1 alpha; `None` when it is not
    /// allowed (a channel the platform did not name is never picked).
    pub fn rank(self, channel: ReleaseChannel) -> Option<u8> {
        let rank = match channel {
            ReleaseChannel::Release => 3,
            ReleaseChannel::Beta => 2,
            ReleaseChannel::Alpha => 1,
            _ => return None,
        };
        let floor = match self {
            Self::Release => 3,
            Self::Beta => 2,
            _ => 1,
        };
        (rank >= floor).then_some(rank)
    }
}

/// What to install and how to choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    /// The instance to install into.
    pub instance: InstanceId,
    /// The platform.
    #[serde(default)]
    pub provider: Provider,
    /// Project id or slug.
    pub project: String,
    /// A version id to install instead of choosing one.
    #[serde(default)]
    pub version: Option<String>,
    /// The least stable channel allowed when choosing.
    #[serde(default)]
    pub min_stability: MinStability,
}

/// Why an item is in the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanRole {
    /// The project the user asked for.
    Requested,
    /// A required dependency of another item.
    Dependency {
        /// The title of the project that needs it.
        required_by: String,
    },
}

/// What happens to an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanAction {
    /// The file is downloaded and added.
    Install,
    /// The wanted version (or, for a dependency, a usable one) is already in the folder.
    Present {
        /// The file in the folder.
        file: String,
    },
    /// Another version of the project is in the folder; it is moved to the trash and the new
    /// file takes its place.
    Replace {
        /// The file that is replaced.
        file: String,
        /// The version it is, when known.
        old_version: Option<String>,
    },
}

/// One project version in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    /// The platform.
    pub provider: Provider,
    /// Project id.
    pub project_id: String,
    /// Project slug.
    pub slug: String,
    /// Project title.
    pub title: String,
    /// What kind of project it is.
    pub project_type: ProjectType,
    /// The folder it goes to.
    pub kind: ResourceKind,
    /// Why it is here.
    pub role: PlanRole,
    /// Version id.
    pub version_id: String,
    /// Version number.
    pub version_number: String,
    /// Release channel.
    pub channel: ReleaseChannel,
    /// The file to download.
    pub file: VersionFile,
    /// What happens to it.
    pub action: PlanAction,
}

impl PlanItem {
    /// Whether the plan downloads this item.
    pub fn downloads(&self) -> bool {
        !matches!(self.action, PlanAction::Present { .. })
    }
}

/// An optional dependency: offered, never installed on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    /// Project id.
    pub project_id: String,
    /// Project slug, when it could be read.
    pub slug: Option<String>,
    /// Project title, when it could be read.
    pub title: Option<String>,
    /// The title of the project that suggests it.
    pub suggested_by: String,
}

/// How an installed file was recognized as a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchedBy {
    /// The launcher installed the file (its origin record matches the file's hash).
    Origin,
    /// The platform knows a file with this hash.
    Hash,
    /// The mod id inside the jar equals the project's slug (a guess for files from elsewhere).
    ModId,
}

/// A required dependency that is already in the folder, so nothing is done for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Satisfied {
    /// Project id.
    pub project_id: String,
    /// Project slug.
    pub slug: String,
    /// Project title.
    pub title: String,
    /// The file in the folder.
    pub file: String,
    /// The installed version, when known.
    pub version: Option<String>,
    /// How the file was recognized.
    pub matched_by: MatchedBy,
}

/// What an install would do.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InstallPlan {
    /// The game version of the instance.
    pub game_version: String,
    /// The mod loader of the instance, when it has one.
    pub loader: Option<String>,
    /// Items in install order: dependencies before the projects that need them.
    pub items: Vec<PlanItem>,
    /// Required dependencies that are already installed.
    pub satisfied: Vec<Satisfied>,
    /// Optional dependencies not installed or planned.
    pub recommended: Vec<Recommendation>,
    /// Things worth saying that do not stop the install.
    pub warnings: Vec<String>,
}

impl InstallPlan {
    /// The items that are downloaded.
    pub fn downloads(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| i.downloads())
    }

    /// Bytes to download.
    pub fn download_bytes(&self) -> u64 {
        self.downloads().map(|i| i.file.size).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_orders_channels() {
        use ReleaseChannel::*;
        assert_eq!(MinStability::Release.rank(Release), Some(3));
        assert_eq!(MinStability::Release.rank(Beta), None);
        assert_eq!(MinStability::Beta.rank(Beta), Some(2));
        assert_eq!(MinStability::Beta.rank(Alpha), None);
        assert_eq!(MinStability::Alpha.rank(Alpha), Some(1));
        assert_eq!(MinStability::Alpha.rank(Unknown), None);
        assert_eq!(MinStability::parse("BETA"), Some(MinStability::Beta));
        assert_eq!(MinStability::parse("x"), None);
    }
}

/// Where a modpack comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum PackSource {
    /// An `.mrpack` file on disk.
    File {
        /// Path of the file.
        path: String,
    },
    /// A modpack project of a platform.
    Catalog {
        /// The platform.
        #[serde(default)]
        provider: Provider,
        /// Project id or slug.
        project: String,
        /// A version id to install instead of choosing one.
        #[serde(default)]
        version: Option<String>,
    },
}

/// Installing a modpack as a new instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackInstallRequest {
    /// The pack.
    pub source: PackSource,
    /// The name of the new instance; the pack's name when `None`.
    #[serde(default)]
    pub name: Option<String>,
    /// The group to show the instance in.
    #[serde(default)]
    pub group: Option<String>,
    /// The least stable channel allowed when choosing a catalog version.
    #[serde(default)]
    pub min_stability: MinStability,
}
