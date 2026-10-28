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
    /// The world (folder name in `saves/`) whose data packs the install goes into. Data packs
    /// need one; other projects must not name one.
    #[serde(default)]
    pub world: Option<String>,
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
    /// A pack file on disk: a Modrinth `.mrpack`, or a CurseForge zip with `manifest.json`
    /// (told apart by content).
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
        /// A version id (on CurseForge a file id) to install instead of choosing one.
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

/// A part of an instance's game directory that an export may carry, beyond the mods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExportInclude {
    /// `config/` and `defaultconfigs/`.
    Config,
    /// `kubejs/`.
    Kubejs,
    /// `resourcepacks/`.
    ResourcePacks,
    /// `shaderpacks/`.
    ShaderPacks,
    /// `options.txt`.
    Options,
}

impl ExportInclude {
    /// What an export carries unless told otherwise.
    pub const DEFAULT: [ExportInclude; 3] = [Self::Config, Self::ResourcePacks, Self::ShaderPacks];

    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Kubejs => "kubejs",
            Self::ResourcePacks => "resourcepacks",
            Self::ShaderPacks => "shaderpacks",
            Self::Options => "options",
        }
    }

    /// Parses [`ExportInclude::name`].
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Config,
            Self::Kubejs,
            Self::ResourcePacks,
            Self::ShaderPacks,
            Self::Options,
        ]
        .into_iter()
        .find(|i| i.name().eq_ignore_ascii_case(name))
    }

    /// The paths in the game directory it covers: folders end in `/`.
    pub fn paths(self) -> &'static [&'static str] {
        match self {
            Self::Config => &["config/", "defaultconfigs/"],
            Self::Kubejs => &["kubejs/"],
            Self::ResourcePacks => &["resourcepacks/"],
            Self::ShaderPacks => &["shaderpacks/"],
            Self::Options => &["options.txt"],
        }
    }
}

fn default_include() -> Vec<ExportInclude> {
    ExportInclude::DEFAULT.to_vec()
}

/// Exporting an instance as a Modrinth modpack (`.mrpack`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackExportRequest {
    /// The instance.
    pub instance: InstanceId,
    /// The file to write; it must not exist yet.
    pub out: String,
    /// The pack's name; the instance's name when `None`.
    #[serde(default)]
    pub name: Option<String>,
    /// The pack's version label; `1.0.0` when `None`.
    #[serde(default)]
    pub version: Option<String>,
    /// A short description.
    #[serde(default)]
    pub summary: Option<String>,
    /// What to carry besides the enabled mods.
    #[serde(default = "default_include")]
    pub include: Vec<ExportInclude>,
}

impl PackExportRequest {
    /// An export of `instance` to `out` with the default contents.
    pub fn new(instance: InstanceId, out: String) -> Self {
        Self {
            instance,
            out,
            name: None,
            version: None,
            summary: None,
            include: default_include(),
        }
    }
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PackExport {
    /// The file written.
    pub path: String,
    /// Files listed in the index with a catalog download link.
    pub linked: usize,
    /// Files carried inside the pack (`overrides/`).
    pub bundled: usize,
    /// Bundled mods, resource packs and shaders the catalog does not know; the author answers
    /// for the right to distribute them.
    pub unlisted: Vec<String>,
    /// Disabled files that were left out.
    pub disabled: usize,
    /// Size of the written file in bytes.
    pub size: u64,
    /// Things worth saying.
    pub warnings: Vec<String>,
}

/// Checking an instance's mods, packs and shaders for newer versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckRequest {
    /// The instance.
    pub instance: InstanceId,
    /// The platform.
    #[serde(default)]
    pub provider: Provider,
    /// The least stable channel an update may come from. Releases only by default.
    #[serde(default = "default_update_stability")]
    pub min_stability: MinStability,
    /// Also check files that came with the instance's modpack (they are skipped by default;
    /// the pack is updated as a whole).
    #[serde(default)]
    pub include_pack_files: bool,
    /// Check the data packs of this world (folder name in `saves/`) instead of the instance's
    /// folders.
    #[serde(default)]
    pub world: Option<String>,
}

fn default_update_stability() -> MinStability {
    MinStability::Release
}

impl UpdateCheckRequest {
    /// A check of `instance` with the defaults.
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            provider: Provider::default(),
            min_stability: MinStability::Release,
            include_pack_files: false,
            world: None,
        }
    }
}

/// A version of a project, as shown in an update list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionLabel {
    /// Version id on the platform.
    pub id: String,
    /// Version number.
    pub number: String,
    /// Release channel.
    pub channel: ReleaseChannel,
}

/// A file for which a newer version exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCandidate {
    /// The folder.
    pub kind: ResourceKind,
    /// The file name without `.disabled`.
    pub file: String,
    /// Whether the file is enabled; an update keeps it so.
    pub enabled: bool,
    /// What to call it: the mod's name when the jar says one, else the file name.
    pub name: String,
    /// Project id on the platform.
    pub project_id: String,
    /// The version in the folder.
    pub current: VersionLabel,
    /// The version an update installs.
    pub latest: VersionLabel,
    /// The file came with the instance's modpack.
    pub from_pack: bool,
}

/// Why a file could not be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UncheckedReason {
    /// No origin record, and the platform does not know the file (added by hand, or not from
    /// this platform).
    NoOrigin,
    /// An origin record exists but the platform no longer knows the file.
    UnknownToPlatform,
    /// A mod in an instance without a mod loader.
    NoLoader,
}

/// A file that could not be checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncheckedFile {
    /// The folder.
    pub kind: ResourceKind,
    /// The file name without `.disabled`.
    pub file: String,
    /// Why.
    pub reason: UncheckedReason,
}

/// A file that came with the modpack and was left out of the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackFile {
    /// The folder.
    pub kind: ResourceKind,
    /// The file name without `.disabled`.
    pub file: String,
}

/// The result of an update check.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UpdateReport {
    /// The game version of the instance.
    pub game_version: String,
    /// The mod loader of the instance.
    pub loader: Option<String>,
    /// Files with a newer version.
    pub updates: Vec<UpdateCandidate>,
    /// How many files are on their newest allowed version.
    pub up_to_date: u32,
    /// Files left out because they came with the modpack.
    pub from_pack: Vec<PackFile>,
    /// Files that could not be checked.
    pub unchecked: Vec<UncheckedFile>,
    /// Things worth saying.
    pub warnings: Vec<String>,
}

/// Updating files to newer versions, or switching one to a chosen version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// What to check and how.
    pub check: UpdateCheckRequest,
    /// Update everything that has an update.
    #[serde(default)]
    pub all: bool,
    /// Update these files, projects or mods: a file name, a project id or a mod id.
    #[serde(default)]
    pub files: Vec<String>,
    /// Switch the one selected file to this version id, newer or older than the current one.
    /// Needs exactly one entry in `files`.
    #[serde(default)]
    pub version: Option<String>,
}

/// Updating the modpack an instance was installed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackUpdateRequest {
    /// The instance.
    pub instance: InstanceId,
    /// The least stable channel the new pack version may come from. Releases only by default.
    #[serde(default = "default_update_stability")]
    pub min_stability: MinStability,
    /// Switch to this version id of the pack project instead of the newest one (it may be older).
    #[serde(default)]
    pub version: Option<String>,
    /// Update from this `.mrpack` file; for packs that were installed from a file.
    #[serde(default)]
    pub file: Option<String>,
    /// Put every file of the pack back as the pack has it: restore deleted files, enable
    /// disabled ones, overwrite changed configs. Files the user added stay.
    #[serde(default)]
    pub reset: bool,
}

impl PackUpdateRequest {
    /// An update of `instance` to the newest release, keeping the user's changes.
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            min_stability: MinStability::Release,
            version: None,
            file: None,
            reset: false,
        }
    }
}

/// The result of checking an instance's modpack for a newer version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackUpdateInfo {
    /// The pack's name.
    pub name: String,
    /// The installed version (the pack's own label when the catalog does not know it).
    pub current: VersionLabel,
    /// The newest allowed version when it is newer than the installed one.
    pub latest: Option<VersionLabel>,
    /// The pack was installed from a file and cannot be checked; update it with a new file.
    pub from_file: bool,
    /// Things worth saying.
    pub warnings: Vec<String>,
}

/// What happens to one file of a pack when it is updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PackAction {
    /// The new pack has a file the instance lacks: it is added.
    Add,
    /// The pack changed the file: it is replaced.
    Update,
    /// The pack dropped the file: it is removed.
    Remove,
    /// A disabled file is enabled again (reset only).
    Enable,
    /// The user changed the config; the pack's new version is saved beside it as
    /// `<file>.pack-new` and the file itself is left alone.
    SaveNew,
    /// The pack changed or added the file but the user deleted it: it stays deleted.
    KeepDeleted,
    /// The user changed or replaced the file: it stays as it is.
    KeepModified,
}

/// One file of a pack update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackChange {
    /// Path in the game directory (without `.disabled`).
    pub path: String,
    /// What happens.
    pub action: PackAction,
    /// The file is a pack download (a mod, pack or shader) rather than a config or other
    /// override.
    pub download: bool,
    /// The file is disabled by the user and stays disabled.
    pub disabled: bool,
    /// Why, when it is not obvious.
    pub note: Option<String>,
}

/// A change of the game or the loader a pack asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackVersionChange {
    /// What the pack had before.
    pub from: String,
    /// What the new pack has.
    pub to: String,
}

/// What updating a modpack would do, from the old and the new index and the folder as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PackChanges {
    /// The pack's name.
    pub name: String,
    /// The installed version.
    pub from: String,
    /// The version it becomes.
    pub to: String,
    /// The new Minecraft version, when it differs.
    pub game_version: Option<PackVersionChange>,
    /// The new loader (`fabric 0.16.7`), when it differs.
    pub loader: Option<PackVersionChange>,
    /// Every file that is added, replaced, removed or left alone on purpose.
    pub changes: Vec<PackChange>,
    /// How many files of the pack stay as they are.
    pub unchanged: u32,
    /// Bytes to download.
    pub download_bytes: u64,
    /// The update puts every file back as the pack has it.
    pub reset: bool,
    /// Things worth saying.
    pub warnings: Vec<String>,
}

impl PackChanges {
    /// Whether applying it changes anything on disk or in the instance.
    pub fn is_empty(&self) -> bool {
        self.game_version.is_none()
            && self.loader.is_none()
            && !self.changes.iter().any(|c| {
                matches!(
                    c.action,
                    PackAction::Add
                        | PackAction::Update
                        | PackAction::Remove
                        | PackAction::Enable
                        | PackAction::SaveNew
                )
            })
    }
}

/// A file whose origin was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifiedFile {
    /// The folder.
    pub kind: ResourceKind,
    /// The file name without `.disabled`.
    pub file: String,
    /// Project id on the platform.
    pub project_id: String,
    /// The version.
    pub version: VersionLabel,
}

/// The result of identifying files added by hand.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IdentifyReport {
    /// Files whose origin was found and recorded now.
    pub identified: Vec<IdentifiedFile>,
    /// Files that already had a record.
    pub already_known: u32,
    /// Files the platform does not know.
    pub unknown: Vec<PackFile>,
    /// Things worth saying.
    pub warnings: Vec<String>,
}
