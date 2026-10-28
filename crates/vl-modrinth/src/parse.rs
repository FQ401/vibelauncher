//! Parsing of Modrinth responses into catalog types.
//!
//! Every function takes the response bytes and returns launcher types. The raw structs below
//! name only the fields the launcher reads; unknown fields are ignored and a field of the wrong
//! type reads as missing.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::Value;
use vl_core::catalog::{
    Dependency, DependencyKind, HashAlgorithm, HashMatch, MAX_SEARCH_LIMIT, Project, ProjectType,
    ProjectVersion, ReleaseChannel, SearchHit, SearchPage, SideSupport, VersionFile,
};
use vl_core::{Error, ErrorKind, Result};

/// Largest accepted search response.
pub const MAX_SEARCH_BYTES: usize = 4 * 1024 * 1024;
/// Largest accepted project response.
pub const MAX_PROJECT_BYTES: usize = 2 * 1024 * 1024;
/// Largest accepted list of versions.
pub const MAX_VERSIONS_BYTES: usize = 32 * 1024 * 1024;

/// Most versions taken from one list.
pub const MAX_VERSIONS: usize = 2_000;
/// Most hash matches taken from one response.
pub const MAX_HASH_MATCHES: usize = 1_000;

const MAX_ID: usize = 64;
const MAX_SLUG: usize = 128;
const MAX_TITLE: usize = 256;
const MAX_DESCRIPTION: usize = 2_048;
const MAX_BODY: usize = 100_000;
const MAX_URL: usize = 2_048;
const MAX_NAME: usize = 256;
const MAX_TAG: usize = 64;
const MAX_TAGS: usize = 64;
const MAX_GAME_VERSIONS: usize = 1_024;
const MAX_VERSION_IDS: usize = 5_000;
const MAX_FILES: usize = 64;
const MAX_DEPENDENCIES: usize = 256;

fn invalid(what: &str) -> Error {
    Error::new(ErrorKind::Integrity, format!("unusable Modrinth {what}"))
}

/// Parses a JSON object; any other JSON is refused (a derived struct would also read an array).
fn object<T: DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid(what))?;
    if !value.is_object() {
        return Err(invalid(what));
    }
    serde_json::from_value(value).map_err(|_| invalid(what))
}

fn check_size(bytes: &[u8], limit: usize, what: &str) -> Result<()> {
    if bytes.len() > limit {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!("Modrinth {what} is larger than {limit} bytes"),
        ));
    }
    Ok(())
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

/// A list whose wrongly typed entries are dropped.
fn lenient_list<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(d)?;
    Ok(match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

fn cut(mut text: String, max_chars: usize) -> String {
    if let Some((index, _)) = text.char_indices().nth(max_chars) {
        text.truncate(index);
    }
    text
}

fn tags(list: Vec<String>, max_items: usize) -> Vec<String> {
    list.into_iter()
        .filter(|t| !t.trim().is_empty())
        .take(max_items)
        .map(|t| cut(t, MAX_TAG))
        .collect()
}

/// Merges lists, dropping repeats and keeping the first-seen order.
fn merged(lists: [Vec<String>; 2], max_items: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in lists.into_iter().flatten() {
        if !tag.trim().is_empty() && !out.contains(&tag) {
            out.push(cut(tag, MAX_TAG));
        }
        if out.len() >= max_items {
            break;
        }
    }
    out
}

/// A web link: `http` or `https` only, length-limited.
fn link(url: Option<String>) -> Option<String> {
    let url = url?;
    let lower = url.get(..8).unwrap_or(&url).to_ascii_lowercase();
    let ok = lower.starts_with("https://") || lower.starts_with("http://");
    (ok && url.len() <= MAX_URL).then_some(url)
}

fn project_type(name: &str) -> ProjectType {
    ProjectType::parse(name).unwrap_or(ProjectType::Other)
}

fn side(name: &str) -> SideSupport {
    match name {
        "required" => SideSupport::Required,
        "optional" => SideSupport::Optional,
        "unsupported" => SideSupport::Unsupported,
        _ => SideSupport::Unknown,
    }
}

// ---------------------------------------------------------------- search

#[derive(Deserialize, Default)]
struct RawSearch {
    #[serde(default, deserialize_with = "lenient")]
    hits: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    offset: u32,
    #[serde(default, deserialize_with = "lenient")]
    limit: u32,
    #[serde(default, deserialize_with = "lenient")]
    total_hits: u64,
}

#[derive(Deserialize, Default)]
struct RawHit {
    #[serde(default, deserialize_with = "lenient")]
    project_id: String,
    #[serde(default, deserialize_with = "lenient")]
    slug: String,
    #[serde(default, deserialize_with = "lenient")]
    title: String,
    #[serde(default, deserialize_with = "lenient")]
    description: String,
    #[serde(default, deserialize_with = "lenient")]
    project_type: String,
    #[serde(default, deserialize_with = "lenient")]
    author: String,
    #[serde(default, deserialize_with = "lenient_list")]
    categories: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    display_categories: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    versions: Vec<String>,
    #[serde(default, deserialize_with = "lenient")]
    downloads: u64,
    #[serde(default, deserialize_with = "lenient")]
    follows: u64,
    #[serde(default, deserialize_with = "lenient")]
    icon_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    date_modified: String,
}

/// Parses a `/search` response. A hit without an id is skipped.
pub fn parse_search(bytes: &[u8]) -> Result<SearchPage> {
    check_size(bytes, MAX_SEARCH_BYTES, "search response")?;
    let raw: RawSearch = object(bytes, "search response")?;
    let hits = raw
        .hits
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawHit>(v).ok())
        .filter(|h| !h.project_id.is_empty() && h.project_id.len() <= MAX_ID)
        .take(MAX_SEARCH_LIMIT as usize)
        .map(|h| SearchHit {
            project_id: h.project_id,
            slug: cut(h.slug, MAX_SLUG),
            title: cut(h.title, MAX_TITLE),
            description: cut(h.description, MAX_DESCRIPTION),
            project_type: project_type(&h.project_type),
            author: cut(h.author, MAX_NAME),
            categories: merged([h.categories, h.display_categories], MAX_TAGS),
            game_versions: tags(h.versions, MAX_GAME_VERSIONS),
            downloads: h.downloads,
            follows: h.follows,
            icon_url: link(h.icon_url),
            date_modified: cut(h.date_modified, MAX_TAG),
        })
        .collect();
    Ok(SearchPage {
        hits,
        offset: raw.offset,
        limit: raw.limit.min(MAX_SEARCH_LIMIT),
        total_hits: raw.total_hits,
    })
}

// ---------------------------------------------------------------- project

#[derive(Deserialize, Default)]
struct RawLicense {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
}

#[derive(Deserialize, Default)]
struct RawProject {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    slug: String,
    #[serde(default, deserialize_with = "lenient")]
    title: String,
    #[serde(default, deserialize_with = "lenient")]
    description: String,
    #[serde(default, deserialize_with = "lenient")]
    body: String,
    #[serde(default, deserialize_with = "lenient")]
    project_type: String,
    #[serde(default, deserialize_with = "lenient_list")]
    categories: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    additional_categories: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    loaders: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    game_versions: Vec<String>,
    #[serde(default, deserialize_with = "lenient")]
    downloads: u64,
    #[serde(default, deserialize_with = "lenient")]
    followers: u64,
    #[serde(default, deserialize_with = "lenient")]
    icon_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    license: RawLicense,
    #[serde(default, deserialize_with = "lenient")]
    source_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    issues_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    client_side: String,
    #[serde(default, deserialize_with = "lenient")]
    server_side: String,
    #[serde(default, deserialize_with = "lenient")]
    updated: String,
    #[serde(default, deserialize_with = "lenient_list")]
    versions: Vec<String>,
}

/// Parses a `/project/{id}` response. A project without an id is an error.
pub fn parse_project(bytes: &[u8]) -> Result<Project> {
    check_size(bytes, MAX_PROJECT_BYTES, "project response")?;
    let raw: RawProject = object(bytes, "project response")?;
    if raw.id.is_empty() || raw.id.len() > MAX_ID {
        return Err(invalid("project response (no id)"));
    }
    let license = [raw.license.name, raw.license.id]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .map(|s| cut(s, MAX_NAME));
    Ok(Project {
        id: raw.id,
        slug: cut(raw.slug, MAX_SLUG),
        title: cut(raw.title, MAX_TITLE),
        description: cut(raw.description, MAX_DESCRIPTION),
        body: cut(raw.body, MAX_BODY),
        project_type: project_type(&raw.project_type),
        categories: merged([raw.categories, raw.additional_categories], MAX_TAGS),
        loaders: tags(raw.loaders, MAX_TAGS),
        game_versions: tags(raw.game_versions, MAX_GAME_VERSIONS),
        downloads: raw.downloads,
        followers: raw.followers,
        icon_url: link(raw.icon_url),
        license,
        source_url: link(raw.source_url),
        issues_url: link(raw.issues_url),
        client_side: side(&raw.client_side),
        server_side: side(&raw.server_side),
        updated: cut(raw.updated, MAX_TAG),
        version_ids: raw
            .versions
            .into_iter()
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID)
            .take(MAX_VERSION_IDS)
            .collect(),
    })
}

// ---------------------------------------------------------------- versions

#[derive(Deserialize, Default)]
struct RawHashes {
    #[serde(default, deserialize_with = "lenient")]
    sha1: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    sha512: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawFile {
    #[serde(default, deserialize_with = "lenient")]
    hashes: RawHashes,
    #[serde(default, deserialize_with = "lenient")]
    url: String,
    #[serde(default, deserialize_with = "lenient")]
    filename: String,
    #[serde(default, deserialize_with = "lenient")]
    primary: bool,
    #[serde(default, deserialize_with = "lenient")]
    size: u64,
}

#[derive(Deserialize, Default)]
struct RawDependency {
    #[serde(default, deserialize_with = "lenient")]
    version_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    project_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    file_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    dependency_type: String,
}

#[derive(Deserialize, Default)]
struct RawVersion {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default, deserialize_with = "lenient")]
    project_id: String,
    #[serde(default, deserialize_with = "lenient")]
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    version_number: String,
    #[serde(default, deserialize_with = "lenient")]
    version_type: String,
    #[serde(default, deserialize_with = "lenient_list")]
    game_versions: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    loaders: Vec<String>,
    #[serde(default, deserialize_with = "lenient")]
    date_published: String,
    #[serde(default, deserialize_with = "lenient")]
    downloads: u64,
    #[serde(default, deserialize_with = "lenient")]
    files: Vec<Value>,
    #[serde(default, deserialize_with = "lenient")]
    dependencies: Vec<Value>,
}

/// A hash as lowercase hex of the right length, else nothing.
fn digest(hash: Option<String>, algorithm: HashAlgorithm) -> Option<String> {
    hash.filter(|h| algorithm.is_valid(h))
        .map(|h| h.to_ascii_lowercase())
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

fn convert_file(raw: RawFile) -> Option<VersionFile> {
    let url = raw.url;
    let https = url
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
    if !https || url.len() > MAX_URL || !is_plain_file_name(&raw.filename) {
        return None;
    }
    Some(VersionFile {
        filename: raw.filename,
        url,
        size: raw.size,
        sha1: digest(raw.hashes.sha1, HashAlgorithm::Sha1),
        sha512: digest(raw.hashes.sha512, HashAlgorithm::Sha512),
        primary: raw.primary,
    })
}

fn convert_dependency(raw: RawDependency) -> Option<Dependency> {
    let id = |v: Option<String>| v.filter(|s| !s.is_empty() && s.len() <= MAX_ID);
    let kind = match raw.dependency_type.as_str() {
        "required" => DependencyKind::Required,
        "optional" => DependencyKind::Optional,
        "incompatible" => DependencyKind::Incompatible,
        "embedded" => DependencyKind::Embedded,
        _ => DependencyKind::Unknown,
    };
    let dependency = Dependency {
        project_id: id(raw.project_id),
        version_id: id(raw.version_id),
        file_name: raw.file_name.filter(|n| is_plain_file_name(n)),
        kind,
    };
    let names_something = dependency.project_id.is_some()
        || dependency.version_id.is_some()
        || dependency.file_name.is_some();
    names_something.then_some(dependency)
}

fn convert_version(raw: RawVersion) -> Option<ProjectVersion> {
    if raw.id.is_empty() || raw.id.len() > MAX_ID {
        return None;
    }
    let channel = match raw.version_type.as_str() {
        "release" => ReleaseChannel::Release,
        "beta" => ReleaseChannel::Beta,
        "alpha" => ReleaseChannel::Alpha,
        _ => ReleaseChannel::Unknown,
    };
    let files = raw
        .files
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawFile>(v).ok())
        .filter_map(convert_file)
        .take(MAX_FILES)
        .collect();
    let dependencies = raw
        .dependencies
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawDependency>(v).ok())
        .filter_map(convert_dependency)
        .take(MAX_DEPENDENCIES)
        .collect();
    Some(ProjectVersion {
        id: raw.id,
        project_id: cut(raw.project_id, MAX_ID),
        name: cut(raw.name, MAX_TITLE),
        version_number: cut(raw.version_number, MAX_NAME),
        channel,
        game_versions: tags(raw.game_versions, MAX_GAME_VERSIONS),
        loaders: tags(raw.loaders, MAX_TAGS),
        date_published: cut(raw.date_published, MAX_TAG),
        downloads: raw.downloads,
        files,
        dependencies,
    })
}

/// Parses one version object (`/version_file/{hash}`, `/version/{id}`).
pub fn parse_version(bytes: &[u8]) -> Result<ProjectVersion> {
    check_size(bytes, MAX_PROJECT_BYTES, "version response")?;
    let raw: RawVersion = object(bytes, "version response")?;
    convert_version(raw).ok_or_else(|| invalid("version response (no id)"))
}

/// Parses a list of versions (`/project/{id}/version`), keeping the server's order (newest
/// first). A damaged entry is skipped.
pub fn parse_versions(bytes: &[u8]) -> Result<Vec<ProjectVersion>> {
    check_size(bytes, MAX_VERSIONS_BYTES, "version list")?;
    let items: Vec<Value> = serde_json::from_slice(bytes).map_err(|_| invalid("version list"))?;
    Ok(items
        .into_iter()
        .filter_map(|v| serde_json::from_value::<RawVersion>(v).ok())
        .filter_map(convert_version)
        .take(MAX_VERSIONS)
        .collect())
}

/// Parses a `/version_files` response (a JSON object from hash to version) into matches sorted
/// by hash. Keys that are not a digest of `algorithm` are skipped.
pub fn parse_version_files(bytes: &[u8], algorithm: HashAlgorithm) -> Result<Vec<HashMatch>> {
    check_size(bytes, MAX_VERSIONS_BYTES, "hash lookup response")?;
    let map: BTreeMap<String, Value> =
        serde_json::from_slice(bytes).map_err(|_| invalid("hash lookup response"))?;
    Ok(map
        .into_iter()
        .filter(|(hash, _)| algorithm.is_valid(hash))
        .filter_map(|(hash, value)| {
            let version = convert_version(serde_json::from_value(value).ok()?)?;
            Some(HashMatch {
                hash: hash.to_ascii_lowercase(),
                version,
            })
        })
        .take(MAX_HASH_MATCHES)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap_or_default()
    }

    fn version_json(id: &str) -> Value {
        json!({
            "id": id,
            "project_id": "PROJ1",
            "name": "Example 1.0",
            "version_number": "1.0.0",
            "version_type": "release",
            "game_versions": ["1.21.4"],
            "loaders": ["fabric"],
            "date_published": "2026-01-01T00:00:00Z",
            "downloads": 12,
            "future_field": {"anything": [1, 2]},
            "files": [{
                "hashes": {"sha1": "A".repeat(40), "sha512": "b".repeat(128)},
                "url": "https://cdn.example.invalid/data/PROJ1/versions/v1/example.jar",
                "filename": "example-1.0.0.jar",
                "primary": true,
                "size": 4096
            }],
            "dependencies": [
                {"project_id": "DEP1", "version_id": null, "dependency_type": "required"},
                {"project_id": "DEP2", "dependency_type": "something-new"},
                {"dependency_type": "optional"}
            ]
        })
    }

    #[test]
    fn parses_a_search_page_and_ignores_unknown_fields() {
        let doc = json!({
            "hits": [
                {
                    "project_id": "P1", "slug": "example", "title": "Example",
                    "description": "A synthetic mod", "project_type": "mod",
                    "author": "someone", "categories": ["fabric", "optimization"],
                    "display_categories": ["optimization", "utility"],
                    "versions": ["1.21.4", "1.21.3"], "downloads": 1000, "follows": 5,
                    "icon_url": "https://cdn.example.invalid/icon.png",
                    "date_modified": "2026-02-02T00:00:00Z", "gallery": ["x"], "color": 7
                },
                {"slug": "no-id"},
                {"project_id": "P2", "project_type": "plugin", "downloads": "many",
                 "icon_url": "javascript:alert(1)"}
            ],
            "offset": 0, "limit": 20, "total_hits": 2, "extra": true
        });
        let page = parse_search(&bytes(&doc)).unwrap_or_default();
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.total_hits, 2);
        let first = &page.hits[0];
        assert_eq!(first.slug, "example");
        assert_eq!(first.project_type, ProjectType::Mod);
        assert_eq!(first.categories, ["fabric", "optimization", "utility"]);
        assert_eq!(first.downloads, 1000);
        assert!(first.icon_url.is_some());
        let second = &page.hits[1];
        assert_eq!(second.project_type, ProjectType::Other);
        assert_eq!(second.downloads, 0, "a wrongly typed number reads as 0");
        assert_eq!(second.icon_url, None, "only web links are kept");
    }

    #[test]
    fn rejects_documents_that_are_not_what_was_asked() {
        assert!(parse_search(b"[]").is_err());
        assert!(parse_search(b"not json").is_err());
        assert!(parse_project(br#"{"slug":"x"}"#).is_err());
        assert!(parse_versions(b"{}").is_err());
        let big = vec![b' '; MAX_PROJECT_BYTES + 1];
        let err = parse_project(&big).err();
        assert_eq!(err.map(|e| e.kind), Some(ErrorKind::Integrity));
    }

    #[test]
    fn parses_a_project() {
        let doc = json!({
            "id": "P1", "slug": "example", "title": "Example", "description": "d",
            "body": "x".repeat(MAX_BODY + 10),
            "project_type": "shader", "categories": ["a"], "additional_categories": ["a", "b"],
            "loaders": ["iris"], "game_versions": ["1.21.4"],
            "downloads": 9, "followers": 3,
            "license": {"id": "MIT", "name": "MIT License", "url": null},
            "source_url": "https://example.invalid/src", "issues_url": null,
            "client_side": "required", "server_side": "unsupported",
            "updated": "2026-03-03T00:00:00Z",
            "versions": ["v1", "", "v2"], "donation_urls": []
        });
        let project = parse_project(&bytes(&doc)).unwrap();
        assert_eq!(project.project_type, ProjectType::Shader);
        assert_eq!(project.body.chars().count(), MAX_BODY);
        assert_eq!(project.categories, ["a", "b"]);
        assert_eq!(project.license.as_deref(), Some("MIT License"));
        assert_eq!(project.client_side, SideSupport::Required);
        assert_eq!(project.server_side, SideSupport::Unsupported);
        assert_eq!(project.version_ids, ["v1", "v2"]);
        assert_eq!(project.issues_url, None);
    }

    #[test]
    fn parses_versions_with_files_and_dependencies() {
        let doc = json!([version_json("v1"), {"name": "no id"}, "garbage", version_json("v2")]);
        let versions = parse_versions(&bytes(&doc)).unwrap_or_default();
        let ids: Vec<_> = versions.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["v1", "v2"]);
        let v = &versions[0];
        assert_eq!(v.channel, ReleaseChannel::Release);
        assert_eq!(v.files.len(), 1);
        let file = v.primary_file();
        assert_eq!(file.map(|f| f.size), Some(4096));
        assert_eq!(
            file.and_then(|f| f.sha1.clone()),
            Some("a".repeat(40)),
            "digests are lowercased"
        );
        assert_eq!(
            v.dependencies.len(),
            2,
            "a dependency naming nothing is dropped"
        );
        assert_eq!(v.dependencies[0].kind, DependencyKind::Required);
        assert_eq!(v.dependencies[0].project_id.as_deref(), Some("DEP1"));
        assert_eq!(v.dependencies[1].kind, DependencyKind::Unknown);
    }

    #[test]
    fn hostile_files_are_dropped() {
        let mut doc = version_json("v1");
        doc["files"] = json!([
            {"url": "http://cdn.example.invalid/a.jar", "filename": "a.jar", "size": 1},
            {"url": "https://cdn.example.invalid/b.jar", "filename": "../b.jar", "size": 1},
            {"url": "https://cdn.example.invalid/c.jar", "filename": "C:evil.jar", "size": 1},
            {"url": "https://cdn.example.invalid/d.jar", "filename": "d.jar", "size": 1,
             "hashes": {"sha1": "not-a-hash"}},
        ]);
        let v = parse_version(&bytes(&doc)).ok();
        let files = v.map(|v| v.files).unwrap_or_default();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "d.jar");
        assert_eq!(files[0].sha1, None, "a malformed digest reads as missing");
    }

    #[test]
    fn long_lists_are_capped() {
        let mut doc = version_json("v1");
        doc["dependencies"] = Value::Array(
            (0..MAX_DEPENDENCIES + 50)
                .map(|i| json!({"project_id": format!("D{i}"), "dependency_type": "optional"}))
                .collect(),
        );
        let v = parse_version(&bytes(&doc)).ok();
        assert_eq!(v.map(|v| v.dependencies.len()), Some(MAX_DEPENDENCIES));
    }

    #[test]
    fn parses_hash_lookups() {
        let sha1 = "c".repeat(40);
        let doc = json!({ sha1.to_uppercase(): version_json("v1"), "short": version_json("v2") });
        let matches = parse_version_files(&bytes(&doc), HashAlgorithm::Sha1).unwrap_or_default();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].hash, sha1);
        assert_eq!(matches[0].version.id, "v1");
        assert!(parse_version_files(b"[]", HashAlgorithm::Sha1).is_err());
    }
}
