//! The mod catalog: searching and reading projects of an online platform (Modrinth, CurseForge).
//!
//! The types are platform-neutral: a platform crate converts what its API returns into them, so
//! a second platform adds a [`Provider`] and a parser without changing any front end. Every
//! string and list that comes from a platform is length-limited by the parser that builds it.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ids::InstanceId;
use crate::install::{
    InstallRequest, PackExportRequest, PackInstallRequest, PackUpdateRequest, UpdateCheckRequest,
    UpdateRequest,
};
use crate::secret::Secret;

/// The most results one search page may ask for.
pub const MAX_SEARCH_LIMIT: u32 = 100;

/// The default size of a search page.
pub const DEFAULT_SEARCH_LIMIT: u32 = 20;

/// An online platform that hosts projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provider {
    /// Modrinth.
    #[default]
    Modrinth,
    /// CurseForge; needs the user's own API key.
    #[serde(rename = "curseforge")]
    CurseForge,
}

/// What a project is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectType {
    /// A mod.
    Mod,
    /// A modpack.
    Modpack,
    /// A resource pack.
    ResourcePack,
    /// A shader pack.
    Shader,
    /// A data pack.
    DataPack,
    /// Something else (a plugin, or a type this launcher does not know yet).
    Other,
}

impl ProjectType {
    /// The types a search can be limited to.
    pub const SEARCHABLE: [ProjectType; 5] = [
        Self::Mod,
        Self::Modpack,
        Self::ResourcePack,
        Self::Shader,
        Self::DataPack,
    ];

    /// The name used on the command line (`mod`, `modpack`, `resourcepack`, `shader`, `datapack`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::Modpack => "modpack",
            Self::ResourcePack => "resourcepack",
            Self::Shader => "shader",
            Self::DataPack => "datapack",
            Self::Other => "other",
        }
    }

    /// Parses a name from [`name`](Self::name); `None` for anything else (including `other`).
    pub fn parse(name: &str) -> Option<Self> {
        Self::SEARCHABLE
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
    }
}

/// How search results are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SearchSort {
    /// Best match for the query.
    #[default]
    Relevance,
    /// Most downloaded first.
    Downloads,
    /// Most followed first.
    Follows,
    /// Newest project first.
    Newest,
    /// Most recently updated first.
    Updated,
}

impl SearchSort {
    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Downloads => "downloads",
            Self::Follows => "follows",
            Self::Newest => "newest",
            Self::Updated => "updated",
        }
    }

    /// Parses a name from [`name`](Self::name).
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Relevance,
            Self::Downloads,
            Self::Follows,
            Self::Newest,
            Self::Updated,
        ]
        .into_iter()
        .find(|s| s.name().eq_ignore_ascii_case(name))
    }
}

/// A catalog search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The platform to ask.
    #[serde(default)]
    pub provider: Provider,
    /// Free text; may be empty to browse.
    #[serde(default)]
    pub query: String,
    /// Only this kind of project.
    #[serde(default)]
    pub project_type: Option<ProjectType>,
    /// Only projects with a version for this game version (`1.21.4`).
    #[serde(default)]
    pub game_version: Option<String>,
    /// Only projects for this loader (`fabric`, `quilt`, `neoforge`, `forge`).
    #[serde(default)]
    pub loader: Option<String>,
    /// Only projects in all of these categories.
    #[serde(default)]
    pub categories: Vec<String>,
    /// The order of the results.
    #[serde(default)]
    pub sort: SearchSort,
    /// How many results to skip.
    #[serde(default)]
    pub offset: u32,
    /// How many results to return (1 to [`MAX_SEARCH_LIMIT`]).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_SEARCH_LIMIT
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            query: String::new(),
            project_type: None,
            game_version: None,
            loader: None,
            categories: Vec::new(),
            sort: SearchSort::default(),
            offset: 0,
            limit: DEFAULT_SEARCH_LIMIT,
        }
    }
}

impl SearchQuery {
    /// A query for `text` with every other field at its default.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            query: text.into(),
            ..Self::default()
        }
    }

    /// Checks the page size and that no filter is blank.
    pub fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > MAX_SEARCH_LIMIT {
            return Err(Error::invalid_input(format!(
                "limit must be between 1 and {MAX_SEARCH_LIMIT}"
            )));
        }
        let blank = |s: &Option<String>| s.as_deref().is_some_and(|s| s.trim().is_empty());
        if blank(&self.game_version) || blank(&self.loader) {
            return Err(Error::invalid_input("a filter must not be blank"));
        }
        if self.categories.iter().any(|c| c.trim().is_empty()) {
            return Err(Error::invalid_input("a category must not be blank"));
        }
        Ok(())
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// The platform's project id.
    pub project_id: String,
    /// The short name used in links.
    pub slug: String,
    /// The display name.
    pub title: String,
    /// A one-line description.
    pub description: String,
    /// What the project is.
    pub project_type: ProjectType,
    /// The author's name.
    pub author: String,
    /// Categories (and loaders) the project is listed under.
    pub categories: Vec<String>,
    /// Game versions the project has files for.
    pub game_versions: Vec<String>,
    /// Total downloads.
    pub downloads: u64,
    /// Followers.
    pub follows: u64,
    /// The icon's URL, when the project has one.
    pub icon_url: Option<String>,
    /// When the project was last updated (RFC 3339 text as sent).
    pub date_modified: String,
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchPage {
    /// The results of this page.
    pub hits: Vec<SearchHit>,
    /// How many results were skipped.
    pub offset: u32,
    /// The page size that was asked for.
    pub limit: u32,
    /// How many results the whole search has.
    pub total_hits: u64,
}

/// Whether a project runs on one side of a game session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SideSupport {
    /// Needed on that side.
    Required,
    /// Works on that side, not needed.
    Optional,
    /// Does not work on that side.
    Unsupported,
    /// The platform did not say.
    #[default]
    Unknown,
}

/// A project's details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// The platform's project id.
    pub id: String,
    /// The short name used in links.
    pub slug: String,
    /// The display name.
    pub title: String,
    /// A one-line description.
    pub description: String,
    /// The long description (Markdown as sent, cut to a size limit).
    pub body: String,
    /// What the project is.
    pub project_type: ProjectType,
    /// Categories.
    pub categories: Vec<String>,
    /// Loaders the project has files for.
    pub loaders: Vec<String>,
    /// Game versions the project has files for.
    pub game_versions: Vec<String>,
    /// Total downloads.
    pub downloads: u64,
    /// Followers.
    pub followers: u64,
    /// The icon's URL, when the project has one.
    pub icon_url: Option<String>,
    /// The license's name (an SPDX id when the platform gives one).
    pub license: Option<String>,
    /// The source code's URL.
    pub source_url: Option<String>,
    /// The issue tracker's URL.
    pub issues_url: Option<String>,
    /// Support on a client.
    pub client_side: SideSupport,
    /// Support on a server.
    pub server_side: SideSupport,
    /// When the project was last updated (RFC 3339 text as sent).
    pub updated: String,
    /// Ids of the project's versions (a long list is cut to a limit).
    pub version_ids: Vec<String>,
}

/// How stable a version is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReleaseChannel {
    /// A stable release.
    Release,
    /// A beta.
    Beta,
    /// An alpha.
    Alpha,
    /// The platform did not say, or said something new.
    #[default]
    Unknown,
}

/// How a version relates to another project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DependencyKind {
    /// Must be installed too.
    Required,
    /// Works better with it.
    Optional,
    /// Does not work with it.
    Incompatible,
    /// Already included in the files.
    Embedded,
    /// The platform sent a kind this launcher does not know.
    Unknown,
}

/// A dependency of a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    /// The project that is needed, when named.
    pub project_id: Option<String>,
    /// The exact version that is needed, when named.
    pub version_id: Option<String>,
    /// The file name that is needed, when only a file is named.
    pub file_name: Option<String>,
    /// How the version relates to it.
    pub kind: DependencyKind,
}

/// One downloadable file of a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionFile {
    /// The file's name.
    pub filename: String,
    /// Where to download it (an `https` URL).
    pub url: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its SHA-1 as lowercase hex, when given.
    pub sha1: Option<String>,
    /// Its SHA-512 as lowercase hex, when given.
    pub sha512: Option<String>,
    /// Whether it is the version's main file.
    pub primary: bool,
}

/// A version of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectVersion {
    /// The platform's version id.
    pub id: String,
    /// The project it belongs to.
    pub project_id: String,
    /// The display name.
    pub name: String,
    /// The author's version string.
    pub version_number: String,
    /// How stable it is.
    pub channel: ReleaseChannel,
    /// Game versions it supports.
    pub game_versions: Vec<String>,
    /// Loaders it supports.
    pub loaders: Vec<String>,
    /// When it was published (RFC 3339 text as sent).
    pub date_published: String,
    /// Its downloads.
    pub downloads: u64,
    /// Its files.
    pub files: Vec<VersionFile>,
    /// Its dependencies.
    pub dependencies: Vec<Dependency>,
}

impl ProjectVersion {
    /// The main file: the one marked primary, else the first.
    pub fn primary_file(&self) -> Option<&VersionFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }
}

/// Which versions of a project to list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VersionFilter {
    /// Only versions for one of these loaders; empty means any.
    #[serde(default)]
    pub loaders: Vec<String>,
    /// Only versions for one of these game versions; empty means any.
    #[serde(default)]
    pub game_versions: Vec<String>,
}

/// The hash a file is looked up by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HashAlgorithm {
    /// SHA-1 (40 hex digits).
    #[default]
    Sha1,
    /// SHA-512 (128 hex digits).
    Sha512,
    /// The CurseForge fingerprint (MurmurHash2 of the file without whitespace bytes), a decimal
    /// 32-bit number.
    Fingerprint,
}

impl HashAlgorithm {
    /// Whether `hash` is a value of this algorithm: hex of the right length (either case) for
    /// the SHA digests, a decimal number below 2^32 without sign or leading zeros for the
    /// fingerprint.
    pub fn is_valid(self, hash: &str) -> bool {
        let hex = |len: usize| hash.len() == len && hash.bytes().all(|b| b.is_ascii_hexdigit());
        match self {
            Self::Sha1 => hex(40),
            Self::Sha512 => hex(128),
            Self::Fingerprint => {
                hash.bytes().all(|b| b.is_ascii_digit())
                    && hash.parse::<u32>().is_ok_and(|n| n.to_string() == hash)
            }
        }
    }
}

/// A file hash with the version it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashMatch {
    /// The hash that was asked for, lowercase.
    pub hash: String,
    /// The version that has a file with that hash.
    pub version: ProjectVersion,
}

/// Where the API key a platform uses comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiKeyOrigin {
    /// The developer's environment variable (`VL_CURSEFORGE_API_KEY`).
    Environment,
    /// The user's key in the OS keyring.
    Keyring,
}

/// What the platform said about the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiKeyState {
    /// No key is set.
    Missing,
    /// The platform accepted the key.
    Valid,
    /// The platform refused the key (`401`, `403`).
    Rejected,
    /// The platform could not be asked, or answered with an error; the key may be fine.
    Unreachable {
        /// English technical detail; never holds the key.
        detail: String,
    },
}

/// The API key of a platform: whether one is set, where it comes from and whether it works. The
/// key itself is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyStatus {
    /// The platform.
    pub provider: Provider,
    /// The result of one request with the key in use.
    pub state: ApiKeyState,
    /// Where the key in use comes from; `None` without a key.
    pub source: Option<ApiKeyOrigin>,
    /// The environment variable holds a key.
    pub in_environment: bool,
    /// The OS keyring holds a key.
    pub in_keyring: bool,
}

impl ApiKeyStatus {
    /// The environment variable is used while the keyring holds a key of its own, so the
    /// stored key is ignored.
    pub fn environment_overrides_keyring(&self) -> bool {
        self.in_environment && self.in_keyring
    }
}

/// Catalog commands. They go to the network, so the backend answers from a worker thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CatalogCommand {
    /// Searches projects → `Response::CatalogSearch`.
    Search(SearchQuery),
    /// One project by id or slug → `Response::CatalogProject`.
    Project {
        /// The project id or slug.
        id: String,
    },
    /// The versions of a project, newest first → `Response::CatalogVersions`.
    Versions {
        /// The project id or slug.
        project: String,
        /// Which versions.
        #[serde(default)]
        filter: VersionFilter,
    },
    /// Finds the versions that own the files with these hashes (hashes nobody knows are left
    /// out) → `Response::CatalogHashMatches`.
    VersionsByHashes {
        /// The algorithm of the hashes.
        algorithm: HashAlgorithm,
        /// The hashes, in hex.
        hashes: Vec<String>,
    },
    /// Works out what installing a project into an instance would do → `Response::InstallPlan`.
    PlanInstall(InstallRequest),
    /// Installs a project and its required dependencies into an instance as a task
    /// → `Response::Task`. The plan is worked out again when the task starts.
    Install(InstallRequest),
    /// Installs a modpack (a Modrinth `.mrpack` or a CurseForge zip) as a new instance, as a
    /// task → `Response::Task`. The new instance is announced with `InstanceEvent::Added` when
    /// the task succeeds; when it fails or is cancelled no instance is left. Files of a
    /// CurseForge pack that cannot be downloaded through the API are reported as launcher log
    /// warnings with the page to get them from, and the install goes on without them.
    InstallPack(PackInstallRequest),
    /// Looks for newer versions of the instance's mods, packs and shaders with one or two batched
    /// requests → `Response::UpdateReport`.
    CheckUpdates(UpdateCheckRequest),
    /// Works out what an update (or a version switch) would do → `Response::InstallPlan`.
    PlanUpdate(UpdateRequest),
    /// Updates files as a task → `Response::Task`. All files are replaced or none; enabled and
    /// disabled files stay so.
    Update(UpdateRequest),
    /// Looks for a newer version of the modpack an instance was installed from
    /// → `Response::PackUpdate`.
    CheckPackUpdate(PackUpdateRequest),
    /// Works out what updating the modpack would change, comparing the old and the new pack
    /// with the folder as it is → `Response::PackChanges`. The new pack is downloaded.
    PlanPackUpdate(PackUpdateRequest),
    /// Updates the modpack as a task → `Response::Task`. Files the user deleted, disabled or
    /// changed are kept (unless the request resets), changed configs get the new version beside
    /// them, and every touched file is backed up under `.vibelauncher/backup/`; on failure or
    /// cancel the instance is put back as it was.
    UpdatePack(PackUpdateRequest),
    /// Writes the instance as a Modrinth modpack file → `Response::PackExported`. Files the
    /// catalog knows by hash become download links; the rest is carried inside the pack.
    ExportPack(PackExportRequest),
    /// Finds the origin of files added by hand (by hash) and records it → `Response::Identified`.
    IdentifyMods {
        /// The instance.
        instance: InstanceId,
        /// The platform.
        #[serde(default)]
        provider: Provider,
        /// Look at the data packs of this world instead of the instance's folders.
        #[serde(default)]
        world: Option<String>,
    },
    /// Stores the user's API key of a platform in the OS keyring, replacing the one stored
    /// → `Response::Done`. The key is checked for shape only; `CheckApiKey` asks the platform.
    /// The key never appears in a response, an event or an error.
    SetApiKey {
        /// The platform (only CurseForge takes a key).
        provider: Provider,
        /// The key.
        key: Secret,
    },
    /// Removes the user's API key of a platform from the OS keyring → `Response::Done`. No
    /// stored key is not an error.
    RemoveApiKey {
        /// The platform.
        provider: Provider,
    },
    /// Says whether a key is set and where it comes from, and tries it with one request
    /// → `Response::ApiKey`.
    CheckApiKey {
        /// The platform.
        provider: Provider,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_type_names_round_trip() {
        for t in ProjectType::SEARCHABLE {
            assert_eq!(ProjectType::parse(t.name()), Some(t));
        }
        assert_eq!(ProjectType::parse("Mod"), Some(ProjectType::Mod));
        assert_eq!(ProjectType::parse("plugin"), None);
    }

    #[test]
    fn search_query_is_validated() {
        assert!(SearchQuery::text("sodium").validate().is_ok());
        let mut q = SearchQuery::text("x");
        q.limit = 0;
        assert!(q.validate().is_err());
        q.limit = MAX_SEARCH_LIMIT + 1;
        assert!(q.validate().is_err());
        q.limit = 5;
        q.loader = Some(" ".into());
        assert!(q.validate().is_err());
    }

    #[test]
    fn query_defaults_apply_to_a_minimal_document() {
        let q: SearchQuery = serde_json::from_str(r#"{"query":"a"}"#).unwrap_or_default();
        assert_eq!(q.query, "a");
        assert_eq!(q.limit, DEFAULT_SEARCH_LIMIT);
        assert_eq!(q.sort, SearchSort::Relevance);
    }

    #[test]
    fn a_key_command_never_shows_the_key() {
        let command = CatalogCommand::SetApiKey {
            provider: Provider::CurseForge,
            key: Secret::new("synthetic-key-value"),
        };
        assert!(!format!("{command:?}").contains("synthetic"));
        let status = ApiKeyStatus {
            provider: Provider::CurseForge,
            state: ApiKeyState::Valid,
            source: Some(ApiKeyOrigin::Environment),
            in_environment: true,
            in_keyring: true,
        };
        assert!(status.environment_overrides_keyring());
    }

    #[test]
    fn hashes_are_checked_by_shape() {
        assert!(HashAlgorithm::Sha1.is_valid(&"a".repeat(40)));
        assert!(!HashAlgorithm::Sha1.is_valid(&"g".repeat(40)));
        assert!(!HashAlgorithm::Sha512.is_valid(&"a".repeat(40)));
        assert!(HashAlgorithm::Fingerprint.is_valid("0"));
        assert!(HashAlgorithm::Fingerprint.is_valid("4294967295"));
        for bad in ["", "4294967296", "+1", "-1", "01", "1a", " 1"] {
            assert!(!HashAlgorithm::Fingerprint.is_valid(bad), "{bad:?}");
        }
    }

    #[test]
    fn primary_file_falls_back_to_the_first() {
        let file = |name: &str, primary| VersionFile {
            filename: name.into(),
            url: "https://example.invalid/f".into(),
            size: 1,
            sha1: None,
            sha512: None,
            primary,
        };
        let mut v = ProjectVersion {
            id: "v".into(),
            project_id: "p".into(),
            name: String::new(),
            version_number: String::new(),
            channel: ReleaseChannel::Release,
            game_versions: vec![],
            loaders: vec![],
            date_published: String::new(),
            downloads: 0,
            files: vec![file("a", false), file("b", true)],
            dependencies: vec![],
        };
        assert_eq!(v.primary_file().map(|f| f.filename.as_str()), Some("b"));
        v.files[1].primary = false;
        assert_eq!(v.primary_file().map(|f| f.filename.as_str()), Some("a"));
    }
}
