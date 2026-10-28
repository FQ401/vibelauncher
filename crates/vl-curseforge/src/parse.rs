//! Parsing of CurseForge responses into catalog types.
//!
//! Every function takes the response bytes and returns launcher types. CurseForge wraps each
//! answer in `{"data": ...}`; the raw structs below name only the fields the launcher reads,
//! unknown fields are ignored and a field of the wrong type reads as missing.
//!
//! A CurseForge file becomes a [`ProjectVersion`] with one [`VersionFile`]: the version id is the
//! file id, the project id is the mod id, and the file's game version labels are split into game
//! versions (`1.20.1`) and loaders (`Forge` → `forge`); labels such as `Client` or `Java 17` are
//! left out.

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::Value;
use vl_core::catalog::{
    Dependency, DependencyKind, HashAlgorithm, MAX_SEARCH_LIMIT, Project, ProjectVersion,
    ReleaseChannel, SearchHit, SearchPage, SideSupport, VersionFile, VersionFilter,
};
use vl_core::{Error, ErrorKind, Result};

use crate::endpoints::{MAX_RESULT_WINDOW, encode, project_type};
use crate::loader::ModLoader;

/// Largest accepted search response.
pub const MAX_SEARCH_BYTES: usize = 8 * 1024 * 1024;
/// Largest accepted project or file response.
pub const MAX_PROJECT_BYTES: usize = 2 * 1024 * 1024;
/// Largest accepted page of files.
pub const MAX_FILES_BYTES: usize = 16 * 1024 * 1024;
/// Largest accepted category list.
pub const MAX_CATEGORIES_BYTES: usize = 4 * 1024 * 1024;
/// Largest accepted answer of a batch of mods or files.
pub const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Largest accepted fingerprint lookup response.
pub const MAX_FINGERPRINTS_BYTES: usize = 32 * 1024 * 1024;

/// Most files taken from one page.
pub const MAX_FILES: usize = 1_000;
/// Most categories taken from one list.
pub const MAX_CATEGORIES: usize = 2_000;
/// Most fingerprint matches taken from one response.
pub const MAX_FINGERPRINT_MATCHES: usize = 2_000;

const MAX_ID: usize = 64;
const MAX_SLUG: usize = 128;
const MAX_TITLE: usize = 256;
const MAX_DESCRIPTION: usize = 2_048;
const MAX_URL: usize = 2_048;
const MAX_NAME: usize = 256;
const MAX_TAG: usize = 64;
const MAX_TAGS: usize = 64;
const MAX_GAME_VERSIONS: usize = 1_024;
const MAX_VERSION_IDS: usize = 5_000;
const MAX_DEPENDENCIES: usize = 256;

/// The host of CurseForge's file CDN, used when a file has no download URL.
const EDGE_BASE: &str = "https://edge.forgecdn.net/files";

fn invalid(what: &str) -> Error {
    Error::new(ErrorKind::Integrity, format!("unusable CurseForge {what}"))
}

fn check_size(bytes: &[u8], limit: usize, what: &str) -> Result<()> {
    if bytes.len() > limit {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("CurseForge {what} is larger than {limit} bytes"),
        ));
    }
    Ok(())
}

/// The `data` member of a response object.
fn data(bytes: &[u8], what: &str) -> Result<(Value, Value)> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid(what))?;
    let Value::Object(mut map) = value else {
        return Err(invalid(what));
    };
    let data = map.remove("data").ok_or_else(|| invalid(what))?;
    let pagination = map.remove("pagination").unwrap_or(Value::Null);
    Ok((data, pagination))
}

fn list(value: Value, what: &str) -> Result<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(invalid(what)),
    }
}

fn from<T: DeserializeOwned>(value: Value) -> Option<T> {
    if value.is_object() {
        serde_json::from_value(value).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------- tolerant reading

/// Reads a field as `T`; a missing, `null` or wrongly typed value gives `T::default()`.
fn lenient<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Option::<Value>::deserialize(d)?;
    Ok(value
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// A count that CurseForge may send as an integer or a decimal number; negative or missing is 0.
fn count<'de, D>(d: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(d)?;
    Ok(match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite() && *f >= 0.0)
                    .map(|f| f as u64)
            })
            .unwrap_or(0),
        _ => 0,
    })
}

/// A list whose wrongly typed entries are dropped.
fn lenient_list<'de, D>(d: D) -> std::result::Result<Vec<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(d)?;
    Ok(match value {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    })
}

fn cut(mut text: String, max_chars: usize) -> String {
    if let Some((index, _)) = text.char_indices().nth(max_chars) {
        text.truncate(index);
    }
    text
}

fn push_tag(out: &mut Vec<String>, tag: &str, max_items: usize) {
    let tag = tag.trim();
    if !tag.is_empty() && out.len() < max_items && !out.iter().any(|t| t == tag) {
        out.push(cut(tag.to_string(), MAX_TAG));
    }
}

/// A web link: `http` or `https` only, length-limited.
fn link(url: Option<String>) -> Option<String> {
    let url = url?;
    let lower = url.get(..8).unwrap_or(&url).to_ascii_lowercase();
    let ok = lower.starts_with("https://") || lower.starts_with("http://");
    (ok && url.len() <= MAX_URL).then_some(url)
}

fn is_https(url: &str) -> bool {
    url.get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

/// Whether a file name is one plain name: no directory part, no `..`, no control characters.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

/// Whether a label among a file's game versions is a game version (`1.20.1`, `1.21-Snapshot`)
/// rather than a loader, a side or a Java version.
fn is_game_version(label: &str) -> bool {
    label.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn id_text(id: u64) -> Option<String> {
    (id > 0).then(|| id.to_string())
}

// ---------------------------------------------------------------- download URLs

/// The address of a file on CurseForge's CDN, built from its id and name:
/// `https://edge.forgecdn.net/files/{id / 1000}/{id % 1000}/{name}` with the name
/// percent-encoded.
pub fn edge_url(file_id: u64, file_name: &str) -> String {
    format!(
        "{EDGE_BASE}/{}/{}/{}",
        file_id / 1000,
        file_id % 1000,
        encode(file_name)
    )
}

/// Where to download a file: its `downloadUrl` when that is an `https` URL, else (CurseForge
/// sends `null` when the author has not allowed downloads through other apps) the CDN address
/// from [`edge_url`]. `None` when the name is not a plain file name, the id is zero, or the
/// given URL is not `https`.
pub fn download_url(file_id: u64, file_name: &str, download_url: Option<&str>) -> Option<String> {
    if file_id == 0 || !is_plain_file_name(file_name) {
        return None;
    }
    match download_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => (is_https(url) && url.len() <= MAX_URL).then(|| url.to_string()),
        None => Some(edge_url(file_id, file_name)),
    }
}

// ---------------------------------------------------------------- projects

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawMod {
    #[serde(default, deserialize_with = "lenient")]
    id: u64,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    slug: String,
    #[serde(default, deserialize_with = "lenient")]
    summary: String,
    #[serde(default, deserialize_with = "lenient")]
    class_id: u64,
    #[serde(default, deserialize_with = "count")]
    download_count: u64,
    #[serde(default, deserialize_with = "count")]
    thumbs_up_count: u64,
    #[serde(default, deserialize_with = "lenient")]
    links: RawLinks,
    #[serde(default, deserialize_with = "lenient_list")]
    categories: Vec<Value>,
    #[serde(default, deserialize_with = "lenient_list")]
    authors: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    logo: Option<RawLogo>,
    #[serde(default, deserialize_with = "lenient_list")]
    latest_files: Vec<Value>,
    #[serde(default, deserialize_with = "lenient_list")]
    latest_files_indexes: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    date_modified: String,
    #[serde(default, deserialize_with = "lenient")]
    allow_mod_distribution: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawLinks {
    #[serde(default, deserialize_with = "lenient")]
    website_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    issues_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    source_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawLogo {
    #[serde(default, deserialize_with = "lenient")]
    thumbnail_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawName {
    #[serde(default, deserialize_with = "lenient")]
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    slug: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawFileIndex {
    #[serde(default, deserialize_with = "lenient")]
    game_version: String,
    #[serde(default, deserialize_with = "lenient")]
    file_id: u64,
    #[serde(default, deserialize_with = "lenient")]
    mod_loader: u64,
}

/// What the lists of a mod say about its files.
struct FileSummary {
    loaders: Vec<String>,
    game_versions: Vec<String>,
    file_ids: Vec<String>,
}

impl RawMod {
    fn icon(&self) -> Option<String> {
        let logo = self.logo.as_ref()?;
        link(logo.thumbnail_url.clone()).or_else(|| link(logo.url.clone()))
    }

    fn category_slugs(&self) -> Vec<String> {
        let mut out = Vec::new();
        for raw in self.categories.iter().cloned().filter_map(from::<RawName>) {
            push_tag(&mut out, &raw.slug, MAX_TAGS);
        }
        out
    }

    fn files(&self) -> FileSummary {
        let mut summary = FileSummary {
            loaders: Vec::new(),
            game_versions: Vec::new(),
            file_ids: Vec::new(),
        };
        let indexes = self
            .latest_files_indexes
            .iter()
            .cloned()
            .filter_map(from::<RawFileIndex>);
        for index in indexes {
            if let Some(loader) = ModLoader::from_id(index.mod_loader) {
                push_tag(&mut summary.loaders, loader.name(), MAX_TAGS);
            }
            if is_game_version(&index.game_version) {
                push_tag(
                    &mut summary.game_versions,
                    &index.game_version,
                    MAX_GAME_VERSIONS,
                );
            }
            if let Some(id) = id_text(index.file_id) {
                push_tag(&mut summary.file_ids, &id, MAX_VERSION_IDS);
            }
        }
        for file in self
            .latest_files
            .iter()
            .cloned()
            .filter_map(from::<RawFile>)
        {
            for label in &file.game_versions {
                match ModLoader::parse(label) {
                    Some(loader) => push_tag(&mut summary.loaders, loader.name(), MAX_TAGS),
                    None if is_game_version(label) => {
                        push_tag(&mut summary.game_versions, label, MAX_GAME_VERSIONS);
                    }
                    None => {}
                }
            }
            if let Some(id) = id_text(file.id) {
                push_tag(&mut summary.file_ids, &id, MAX_VERSION_IDS);
            }
        }
        summary
    }
}

fn convert_hit(raw: RawMod) -> Option<SearchHit> {
    let project_id = id_text(raw.id)?;
    let files = raw.files();
    let icon_url = raw.icon();
    let categories = raw.category_slugs();
    let author = raw
        .authors
        .iter()
        .cloned()
        .find_map(from::<RawName>)
        .map(|a| cut(a.name, MAX_NAME))
        .unwrap_or_default();
    let mut tags = categories;
    for loader in files.loaders {
        push_tag(&mut tags, &loader, MAX_TAGS);
    }
    Some(SearchHit {
        project_id,
        slug: cut(raw.slug, MAX_SLUG),
        title: cut(raw.name, MAX_TITLE),
        description: cut(raw.summary, MAX_DESCRIPTION),
        project_type: project_type(raw.class_id),
        author,
        categories: tags,
        game_versions: files.game_versions,
        downloads: raw.download_count,
        follows: raw.thumbs_up_count,
        icon_url,
        date_modified: cut(raw.date_modified, MAX_TAG),
    })
}

fn convert_project(raw: RawMod) -> Option<Project> {
    let id = id_text(raw.id)?;
    let files = raw.files();
    let icon_url = raw.icon();
    let categories = raw.category_slugs();
    Some(Project {
        id,
        slug: cut(raw.slug, MAX_SLUG),
        title: cut(raw.name, MAX_TITLE),
        description: cut(raw.summary, MAX_DESCRIPTION),
        // The long description is HTML behind another endpoint; it is not read.
        body: String::new(),
        project_type: project_type(raw.class_id),
        categories,
        loaders: files.loaders,
        game_versions: files.game_versions,
        downloads: raw.download_count,
        followers: raw.thumbs_up_count,
        icon_url,
        license: None,
        source_url: link(raw.links.source_url),
        issues_url: link(raw.links.issues_url),
        client_side: SideSupport::Unknown,
        server_side: SideSupport::Unknown,
        updated: cut(raw.date_modified, MAX_TAG),
        version_ids: files.file_ids,
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawPagination {
    #[serde(default, deserialize_with = "lenient")]
    index: u32,
    #[serde(default, deserialize_with = "lenient")]
    page_size: u32,
    #[serde(default, deserialize_with = "lenient")]
    total_count: u64,
}

/// Parses a `/v1/mods/search` response. A mod without an id is skipped. `total_hits` is cut
/// to the window CurseForge serves (10 000), so paging never asks past it.
pub fn parse_search(bytes: &[u8]) -> Result<SearchPage> {
    check_size(bytes, MAX_SEARCH_BYTES, "search response")?;
    let (data, pagination) = data(bytes, "search response")?;
    let hits = list(data, "search response")?
        .into_iter()
        .filter_map(from::<RawMod>)
        .filter_map(convert_hit)
        .take(MAX_SEARCH_LIMIT as usize)
        .collect();
    let pagination: RawPagination = serde_json::from_value(pagination).unwrap_or_default();
    Ok(SearchPage {
        hits,
        offset: pagination.index,
        limit: pagination.page_size,
        total_hits: pagination.total_count.min(u64::from(MAX_RESULT_WINDOW)),
    })
}

/// Parses a `/v1/mods/{modId}` response.
pub fn parse_project(bytes: &[u8]) -> Result<Project> {
    check_size(bytes, MAX_PROJECT_BYTES, "project response")?;
    let (data, _) = data(bytes, "project response")?;
    from::<RawMod>(data)
        .and_then(convert_project)
        .ok_or_else(|| invalid("project response (no id)"))
}

// ---------------------------------------------------------------- files

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    #[serde(default, deserialize_with = "lenient")]
    id: u64,
    #[serde(default, deserialize_with = "lenient")]
    mod_id: u64,
    #[serde(default, deserialize_with = "lenient")]
    display_name: String,
    #[serde(default, deserialize_with = "lenient")]
    file_name: String,
    #[serde(default, deserialize_with = "lenient")]
    release_type: u64,
    #[serde(default, deserialize_with = "lenient_list")]
    hashes: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    file_date: String,
    #[serde(default, deserialize_with = "lenient")]
    file_length: u64,
    #[serde(default, deserialize_with = "count")]
    download_count: u64,
    #[serde(default, deserialize_with = "lenient")]
    download_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    game_versions: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    dependencies: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    file_fingerprint: u64,
}

#[derive(Deserialize, Default)]
struct RawHash {
    #[serde(default, deserialize_with = "lenient")]
    value: String,
    #[serde(default, deserialize_with = "lenient")]
    algo: u64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawDependency {
    #[serde(default, deserialize_with = "lenient")]
    mod_id: u64,
    #[serde(default, deserialize_with = "lenient")]
    relation_type: u64,
}

fn convert_dependency(raw: RawDependency) -> Option<Dependency> {
    let kind = match raw.relation_type {
        1 | 6 => DependencyKind::Embedded,
        2 | 4 => DependencyKind::Optional,
        3 => DependencyKind::Required,
        5 => DependencyKind::Incompatible,
        _ => DependencyKind::Unknown,
    };
    Some(Dependency {
        project_id: Some(id_text(raw.mod_id)?),
        version_id: None,
        file_name: None,
        kind,
    })
}

fn convert_file(raw: RawFile) -> Option<ProjectVersion> {
    let id = id_text(raw.id)?;
    let channel = match raw.release_type {
        1 => ReleaseChannel::Release,
        2 => ReleaseChannel::Beta,
        3 => ReleaseChannel::Alpha,
        _ => ReleaseChannel::Unknown,
    };
    let sha1 = raw
        .hashes
        .into_iter()
        .filter_map(from::<RawHash>)
        .find(|h| h.algo == 1 && HashAlgorithm::Sha1.is_valid(&h.value))
        .map(|h| h.value.to_ascii_lowercase());
    let mut game_versions = Vec::new();
    let mut loaders = Vec::new();
    for label in &raw.game_versions {
        match ModLoader::parse(label) {
            Some(loader) => push_tag(&mut loaders, loader.name(), MAX_TAGS),
            None if is_game_version(label) => {
                push_tag(&mut game_versions, label, MAX_GAME_VERSIONS);
            }
            None => {}
        }
    }
    let files = download_url(raw.id, &raw.file_name, raw.download_url.as_deref())
        .map(|url| VersionFile {
            filename: raw.file_name.clone(),
            url,
            size: raw.file_length,
            sha1,
            sha512: None,
            primary: true,
        })
        .into_iter()
        .collect();
    let dependencies = raw
        .dependencies
        .into_iter()
        .filter_map(from::<RawDependency>)
        .filter_map(convert_dependency)
        .take(MAX_DEPENDENCIES)
        .collect();
    let name = cut(raw.display_name, MAX_TITLE);
    Some(ProjectVersion {
        id: cut(id, MAX_ID),
        project_id: id_text(raw.mod_id).unwrap_or_default(),
        version_number: cut(name.clone(), MAX_NAME),
        name,
        channel,
        game_versions,
        loaders,
        date_published: cut(raw.file_date, MAX_TAG),
        downloads: raw.download_count,
        files,
        dependencies,
    })
}

/// A page of a project's files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilePage {
    /// The files of this page as versions, in the server's order.
    pub versions: Vec<ProjectVersion>,
    /// How many entries the server sent on this page (damaged ones included).
    pub received: usize,
    /// How many files the whole list has.
    pub total: u64,
}

/// Parses a `/v1/mods/{modId}/files` response. A damaged entry is skipped.
pub fn parse_files(bytes: &[u8]) -> Result<FilePage> {
    check_size(bytes, MAX_FILES_BYTES, "file list")?;
    let (data, pagination) = data(bytes, "file list")?;
    let items = list(data, "file list")?;
    let received = items.len();
    let versions = items
        .into_iter()
        .filter_map(from::<RawFile>)
        .filter_map(convert_file)
        .take(MAX_FILES)
        .collect();
    let pagination: RawPagination = serde_json::from_value(pagination).unwrap_or_default();
    Ok(FilePage {
        versions,
        received,
        total: pagination.total_count,
    })
}

/// Parses a `/v1/mods/{modId}/files/{fileId}` response.
pub fn parse_file(bytes: &[u8]) -> Result<ProjectVersion> {
    check_size(bytes, MAX_PROJECT_BYTES, "file response")?;
    let (data, _) = data(bytes, "file response")?;
    from::<RawFile>(data)
        .and_then(convert_file)
        .ok_or_else(|| invalid("file response (no id)"))
}

/// Whether a version fits a filter: one of its game versions is asked for (any when the filter
/// lists none), and one of its loaders is asked for (any when the filter lists none). A version
/// that names no loader does not fit a loader filter.
pub fn fits(version: &ProjectVersion, filter: &VersionFilter) -> bool {
    let wanted = |list: &[String], have: &[String]| {
        let list: Vec<&str> = list
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        list.is_empty()
            || have
                .iter()
                .any(|h| list.iter().any(|w| w.eq_ignore_ascii_case(h)))
    };
    wanted(&filter.game_versions, &version.game_versions)
        && wanted(&filter.loaders, &version.loaders)
}

// ---------------------------------------------------------------- batches

/// Most entries taken from one batch answer.
pub const MAX_BATCH_ITEMS: usize = 20_000;

/// A file as a modpack install needs it, from `POST /v1/mods/files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    /// The file id.
    pub id: u64,
    /// The project (mod) id.
    pub mod_id: u64,
    /// The display name.
    pub display_name: String,
    /// The file name as CurseForge has it; check it with [`ModFile::safe_file_name`].
    pub file_name: String,
    /// Size in bytes (0 when unknown).
    pub size: u64,
    /// Lowercase hex SHA-1 when CurseForge gives a valid one.
    pub sha1: Option<String>,
    /// The `https` download URL the API gives. `None` when the author does not allow
    /// downloads through other apps (the API sends `null`) or the URL is not `https`.
    pub download_url: Option<String>,
}

impl ModFile {
    /// The file name when it is one plain name (no directory part, no `..`).
    pub fn safe_file_name(&self) -> Option<&str> {
        is_plain_file_name(&self.file_name).then_some(self.file_name.as_str())
    }

    /// The address on CurseForge's CDN built from the id and name ([`edge_url`]); `None` for
    /// an unsafe name.
    pub fn edge_url(&self) -> Option<String> {
        self.safe_file_name().map(|name| edge_url(self.id, name))
    }
}

fn convert_mod_file(raw: RawFile) -> Option<ModFile> {
    if raw.id == 0 {
        return None;
    }
    let sha1 = raw
        .hashes
        .into_iter()
        .filter_map(from::<RawHash>)
        .find(|h| h.algo == 1 && HashAlgorithm::Sha1.is_valid(&h.value))
        .map(|h| h.value.to_ascii_lowercase());
    let download_url = raw
        .download_url
        .map(|u| u.trim().to_owned())
        .filter(|u| is_https(u) && u.len() <= MAX_URL && !u.chars().any(char::is_control));
    Some(ModFile {
        id: raw.id,
        mod_id: raw.mod_id,
        display_name: cut(raw.display_name, MAX_TITLE),
        // One character over the limit keeps a too long name unsafe.
        file_name: cut(raw.file_name, MAX_NAME + 1),
        size: raw.file_length,
        sha1,
        download_url,
    })
}

/// Parses a `POST /v1/mods/files` response. A damaged entry or one without an id is skipped.
pub fn parse_mod_files(bytes: &[u8]) -> Result<Vec<ModFile>> {
    check_size(bytes, MAX_BATCH_BYTES, "file batch")?;
    let (data, _) = data(bytes, "file batch")?;
    Ok(list(data, "file batch")?
        .into_iter()
        .filter_map(from::<RawFile>)
        .filter_map(convert_mod_file)
        .take(MAX_BATCH_ITEMS)
        .collect())
}

/// What a modpack install needs to know of a project, from `POST /v1/mods`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSummary {
    /// The project (mod) id.
    pub id: u64,
    /// The display name.
    pub name: String,
    /// The short name.
    pub slug: String,
    /// The class (`6` mods, `12` resource packs, `6552` shaders, ...); 0 when unknown.
    pub class_id: u64,
    /// The project's page on the CurseForge website (`https` only).
    pub website_url: Option<String>,
    /// Whether the author allows downloads through other apps; `None` when not said.
    pub allow_distribution: Option<bool>,
}

impl ModSummary {
    /// The page where the user downloads the project's files by hand: the website URL, else
    /// [`project_page_url`].
    pub fn page_url(&self) -> String {
        self.website_url
            .clone()
            .unwrap_or_else(|| project_page_url(self.id))
    }
}

/// A web address of a project that works for every class: CurseForge redirects it to the
/// project's page.
pub fn project_page_url(mod_id: u64) -> String {
    format!("https://www.curseforge.com/projects/{mod_id}")
}

/// Parses a `POST /v1/mods` response. A damaged entry or one without an id is skipped.
pub fn parse_mods(bytes: &[u8]) -> Result<Vec<ModSummary>> {
    check_size(bytes, MAX_BATCH_BYTES, "mod batch")?;
    let (data, _) = data(bytes, "mod batch")?;
    Ok(list(data, "mod batch")?
        .into_iter()
        .filter_map(from::<RawMod>)
        .filter(|m| m.id > 0)
        .take(MAX_BATCH_ITEMS)
        .map(|m| ModSummary {
            id: m.id,
            name: cut(m.name, MAX_TITLE),
            slug: cut(m.slug, MAX_SLUG),
            class_id: m.class_id,
            website_url: link(m.links.website_url).filter(|u| is_https(u)),
            allow_distribution: m.allow_mod_distribution,
        })
        .collect())
}

// ---------------------------------------------------------------- categories

/// A CurseForge category, or a class (a top-level kind of project such as mods or modpacks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// The category id; searches filter by it.
    pub id: u64,
    /// The display name.
    pub name: String,
    /// The short name.
    pub slug: String,
    /// The class it belongs to (`None` for a class itself).
    pub class_id: Option<u64>,
    /// The parent category (`None` at the top).
    pub parent_id: Option<u64>,
    /// Whether this is a class rather than a category.
    pub is_class: bool,
    /// The icon's URL.
    pub icon_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawCategory {
    #[serde(default, deserialize_with = "lenient")]
    id: u64,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    slug: String,
    #[serde(default, deserialize_with = "lenient")]
    class_id: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    parent_category_id: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    is_class: bool,
    #[serde(default, deserialize_with = "lenient")]
    icon_url: Option<String>,
}

/// Parses a `/v1/categories` response. An entry without an id or slug is skipped.
pub fn parse_categories(bytes: &[u8]) -> Result<Vec<Category>> {
    check_size(bytes, MAX_CATEGORIES_BYTES, "category list")?;
    let (data, _) = data(bytes, "category list")?;
    Ok(list(data, "category list")?
        .into_iter()
        .filter_map(from::<RawCategory>)
        .filter(|c| c.id > 0 && !c.slug.trim().is_empty())
        .take(MAX_CATEGORIES)
        .map(|c| Category {
            id: c.id,
            name: cut(c.name, MAX_TITLE),
            slug: cut(c.slug, MAX_SLUG),
            class_id: c.class_id.filter(|id| *id > 0),
            parent_id: c.parent_category_id.filter(|id| *id > 0),
            is_class: c.is_class,
            icon_url: link(c.icon_url),
        })
        .collect())
}

// ---------------------------------------------------------------- fingerprints

/// A fingerprint CurseForge knows, with the file (as a version) it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintMatch {
    /// The fingerprint.
    pub fingerprint: u32,
    /// The file that has it.
    pub version: ProjectVersion,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawFingerprints {
    #[serde(default, deserialize_with = "lenient_list")]
    exact_matches: Vec<Value>,
}

#[derive(Deserialize, Default)]
struct RawMatch {
    #[serde(default, deserialize_with = "lenient")]
    file: Value,
}

/// Parses a `/v1/fingerprints` response into the exact matches, sorted by fingerprint. A match
/// whose file is damaged or has no fingerprint is skipped.
pub fn parse_fingerprints(bytes: &[u8]) -> Result<Vec<FingerprintMatch>> {
    check_size(bytes, MAX_FINGERPRINTS_BYTES, "fingerprint response")?;
    let (data, _) = data(bytes, "fingerprint response")?;
    let raw: RawFingerprints = from(data).ok_or_else(|| invalid("fingerprint response"))?;
    let mut matches: Vec<FingerprintMatch> = raw
        .exact_matches
        .into_iter()
        .filter_map(from::<RawMatch>)
        .filter_map(|m| from::<RawFile>(m.file))
        .filter_map(|file| {
            let fingerprint = u32::try_from(file.file_fingerprint).ok()?;
            let version = convert_file(file)?;
            Some(FingerprintMatch {
                fingerprint,
                version,
            })
        })
        .take(MAX_FINGERPRINT_MATCHES)
        .collect();
    matches.sort_by_key(|m| m.fingerprint);
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_urls_split_the_id_and_encode_the_name() {
        assert_eq!(
            edge_url(4_712_045, "Example Mod+[1.20.1].jar"),
            "https://edge.forgecdn.net/files/4712/45/Example%20Mod%2B%5B1.20.1%5D.jar"
        );
        assert_eq!(
            edge_url(999, "a.jar"),
            "https://edge.forgecdn.net/files/0/999/a.jar"
        );
    }

    #[test]
    fn download_urls_fall_back_and_refuse_unsafe_input() {
        assert_eq!(
            download_url(5_000_001, "a.jar", Some("https://cdn.test/a.jar")).as_deref(),
            Some("https://cdn.test/a.jar")
        );
        assert_eq!(
            download_url(5_000_001, "a.jar", None).as_deref(),
            Some("https://edge.forgecdn.net/files/5000/1/a.jar")
        );
        assert_eq!(
            download_url(5_000_001, "a.jar", Some("")).as_deref(),
            Some("https://edge.forgecdn.net/files/5000/1/a.jar")
        );
        assert_eq!(
            download_url(1, "a.jar", Some("http://cdn.test/a.jar")),
            None
        );
        assert_eq!(download_url(1, "../a.jar", None), None);
        assert_eq!(download_url(0, "a.jar", None), None);
    }

    fn version(game: &[&str], loaders: &[&str]) -> ProjectVersion {
        ProjectVersion {
            id: "1".into(),
            project_id: "2".into(),
            name: String::new(),
            version_number: String::new(),
            channel: ReleaseChannel::Release,
            game_versions: game.iter().map(|s| s.to_string()).collect(),
            loaders: loaders.iter().map(|s| s.to_string()).collect(),
            date_published: String::new(),
            downloads: 0,
            files: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    #[test]
    fn filters_fit_any_listed_value() {
        let v = version(&["1.20.1"], &["forge", "neoforge"]);
        assert!(fits(&v, &VersionFilter::default()));
        let f = VersionFilter {
            loaders: vec!["fabric".into(), "NeoForge".into()],
            game_versions: vec!["1.20".into(), "1.20.1".into()],
        };
        assert!(fits(&v, &f));
        let f = VersionFilter {
            loaders: vec!["fabric".into()],
            game_versions: vec![],
        };
        assert!(!fits(&v, &f));
        assert!(!fits(&version(&["1.20.1"], &[]), &f));
    }

    #[test]
    fn wrapped_shapes_are_required() {
        assert!(parse_project(b"[]").is_err());
        assert!(parse_project(br#"{"id": 1}"#).is_err());
        assert!(parse_project(br#"{"data": {"id": 0}}"#).is_err());
        assert!(parse_search(br#"{"data": {}}"#).is_err());
        assert!(parse_files(b"not json").is_err());
        let big = vec![b' '; MAX_PROJECT_BYTES + 1];
        assert!(parse_file(&big).is_err());
    }
}
