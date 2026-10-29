//! Synthetic catalog: invented projects of both platforms (a Modrinth and a CurseForge flavour),
//! searched, sorted and paged like the real thing. Installing and updating are in `installs`,
//! modpacks in `packs`.

use std::sync::Arc;

use vl_core::catalog::{
    ApiKeyOrigin, ApiKeyState, ApiKeyStatus, CatalogCommand, Dependency, DependencyKind, Project,
    ProjectType, ProjectVersion, Provider, ReleaseChannel, SearchHit, SearchPage, SearchQuery,
    SearchSort, SideSupport, VersionFile, VersionFilter,
};
use vl_core::install::{MinStability, VersionLabel};
use vl_core::{Error, ErrorKind, Response, Result};

use crate::{Inner, State, installs, packs};

/// An invented project.
pub(crate) struct Entry {
    pub slug: &'static str,
    pub title: &'static str,
    pub kind: ProjectType,
    pub provider: Provider,
    pub downloads: u64,
    pub updated: &'static str,
    pub loaders: &'static [&'static str],
}

const MR_MOD: &[&str] = &["fabric", "neoforge"];
const CF_MOD: &[&str] = &["forge", "neoforge"];

const fn entry(
    slug: &'static str,
    title: &'static str,
    kind: ProjectType,
    provider: Provider,
    downloads: u64,
    updated: &'static str,
    loaders: &'static [&'static str],
) -> Entry {
    Entry {
        slug,
        title,
        kind,
        provider,
        downloads,
        updated,
        loaders,
    }
}

use Provider::{CurseForge as Cf, Modrinth as Mr};

/// The projects: indexes 0..12 are Modrinth's, 12..18 CurseForge's.
pub(crate) const ENTRIES: [Entry; 18] = [
    entry(
        "mock-library",
        "Mock Library",
        ProjectType::Mod,
        Mr,
        9_800_000,
        "2026-08-10T09:00:00Z",
        MR_MOD,
    ),
    entry(
        "mock-renderer",
        "Mock Renderer",
        ProjectType::Mod,
        Mr,
        4_200_000,
        "2026-09-02T09:00:00Z",
        MR_MOD,
    ),
    entry(
        "mock-minimap",
        "Mock Minimap",
        ProjectType::Mod,
        Mr,
        2_650_000,
        "2026-07-21T09:00:00Z",
        MR_MOD,
    ),
    entry(
        "mock-weather-fx",
        "Mock Weather FX",
        ProjectType::Mod,
        Mr,
        830_000,
        "2026-09-18T09:00:00Z",
        MR_MOD,
    ),
    entry(
        "mock-inventory-tweaks",
        "Mock Inventory Tweaks",
        ProjectType::Mod,
        Mr,
        1_450_000,
        "2026-06-30T09:00:00Z",
        MR_MOD,
    ),
    entry(
        "mock-adventure-pack",
        "Mock Adventure Pack",
        ProjectType::Modpack,
        Mr,
        610_000,
        "2026-10-01T09:00:00Z",
        &["fabric"],
    ),
    entry(
        "mock-skyblock-lite",
        "Mock Skyblock Lite",
        ProjectType::Modpack,
        Mr,
        240_000,
        "2026-05-12T09:00:00Z",
        &["fabric"],
    ),
    entry(
        "mock-textures",
        "Mock Textures",
        ProjectType::ResourcePack,
        Mr,
        1_900_000,
        "2026-08-25T09:00:00Z",
        &["minecraft"],
    ),
    entry(
        "mock-pixel-pack",
        "Mock Pixel Pack",
        ProjectType::ResourcePack,
        Mr,
        770_000,
        "2026-04-02T09:00:00Z",
        &["minecraft"],
    ),
    entry(
        "mock-shaders-soft",
        "Mock Soft Shaders",
        ProjectType::Shader,
        Mr,
        3_100_000,
        "2026-09-09T09:00:00Z",
        &["iris"],
    ),
    entry(
        "mock-shaders-vivid",
        "Mock Vivid Shaders",
        ProjectType::Shader,
        Mr,
        520_000,
        "2026-03-14T09:00:00Z",
        &["iris"],
    ),
    entry(
        "mock-recipes-datapack",
        "Mock Recipes",
        ProjectType::DataPack,
        Mr,
        150_000,
        "2026-02-20T09:00:00Z",
        &["datapack"],
    ),
    entry(
        "mock-cf-library",
        "Mock CF Library",
        ProjectType::Mod,
        Cf,
        7_300_000,
        "2026-08-11T09:00:00Z",
        CF_MOD,
    ),
    entry(
        "mock-pocket-storage",
        "Mock Pocket Storage",
        ProjectType::Mod,
        Cf,
        2_100_000,
        "2026-09-05T09:00:00Z",
        CF_MOD,
    ),
    entry(
        "mock-terrain-gen",
        "Mock Terrain Gen",
        ProjectType::Mod,
        Cf,
        1_320_000,
        "2026-07-01T09:00:00Z",
        CF_MOD,
    ),
    entry(
        "mock-tech-pack",
        "Mock Tech Pack",
        ProjectType::Modpack,
        Cf,
        980_000,
        "2026-10-05T09:00:00Z",
        &["forge"],
    ),
    entry(
        "mock-kitchen-pack",
        "Mock Kitchen Pack",
        ProjectType::Modpack,
        Cf,
        410_000,
        "2026-06-06T09:00:00Z",
        &["forge"],
    ),
    entry(
        "mock-cf-textures",
        "Mock CF Textures",
        ProjectType::ResourcePack,
        Cf,
        640_000,
        "2026-05-30T09:00:00Z",
        &["minecraft"],
    ),
];

/// The index of the Modrinth library every Modrinth mod requires.
pub(crate) const MR_LIBRARY: usize = 0;
/// The index of the CurseForge library every CurseForge mod requires.
pub(crate) const CF_LIBRARY: usize = 12;
/// The CurseForge pack that has a file the API will not serve.
pub(crate) const BLOCKED_PACK: usize = 15;

/// The versions every project has, newest first.
pub(crate) const VERSIONS: [(&str, ReleaseChannel, &str); 3] = [
    ("2.1.0-beta.1", ReleaseChannel::Beta, "2026-10-01T09:00:00Z"),
    ("2.0.0", ReleaseChannel::Release, "2026-08-10T09:00:00Z"),
    ("1.1.0", ReleaseChannel::Release, "2026-03-05T09:00:00Z"),
];

const GAME_VERSIONS: [&str; 2] = ["26.3", "1.21.11"];

pub(crate) fn library_of(provider: Provider) -> usize {
    match provider {
        Provider::CurseForge => CF_LIBRARY,
        _ => MR_LIBRARY,
    }
}

pub(crate) fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::CurseForge => "CurseForge",
        _ => "Modrinth",
    }
}

fn project_id(idx: usize, provider: Provider) -> String {
    match provider {
        Provider::CurseForge => format!("{}", 910_000 + idx),
        _ => format!("mockmr{idx:02}"),
    }
}

/// The platform id of project `idx`.
pub(crate) fn id_of(idx: usize) -> String {
    ENTRIES
        .get(idx)
        .map(|e| project_id(idx, e.provider))
        .unwrap_or_default()
}

/// The version id of version `v` of project `idx`.
pub(crate) fn version_id(idx: usize, v: usize) -> String {
    match ENTRIES.get(idx).map(|e| e.provider) {
        Some(Provider::CurseForge) => format!("{}", 8_100_000 + idx * 10 + v),
        _ => format!("{}v{}", id_of(idx), v + 1),
    }
}

/// The file name of version `v` of project `idx`.
pub(crate) fn file_name(idx: usize, v: usize) -> String {
    let Some(entry) = ENTRIES.get(idx) else {
        return String::new();
    };
    let ext = match (entry.kind, entry.provider) {
        (ProjectType::Mod, _) => "jar",
        (ProjectType::Modpack, Provider::Modrinth) => "mrpack",
        _ => "zip",
    };
    let number = VERSIONS.get(v).map_or("0", |version| version.0);
    format!("{}-{number}.{ext}", entry.slug)
}

/// Finds a project by id or slug.
pub(crate) fn find(key: &str) -> Result<usize> {
    let key = key.trim();
    ENTRIES
        .iter()
        .enumerate()
        .find(|(idx, e)| e.slug == key || project_id(*idx, e.provider) == key)
        .map(|(idx, _)| idx)
        .ok_or_else(|| Error::not_found(format!("project {key}")))
}

/// Recognizes a catalog file by its name: the project and the version index.
pub(crate) fn find_by_file(file: &str) -> Option<(usize, usize)> {
    let stem = file
        .strip_suffix(".jar")
        .or_else(|| file.strip_suffix(".zip"))
        .or_else(|| file.strip_suffix(".mrpack"))?;
    ENTRIES.iter().enumerate().find_map(|(idx, e)| {
        let number = stem.strip_prefix(e.slug)?.strip_prefix('-')?;
        let v = VERSIONS.iter().position(|version| version.0 == number)?;
        Some((idx, v))
    })
}

/// The newest version a stability floor allows.
pub(crate) fn newest(min: MinStability) -> usize {
    VERSIONS
        .iter()
        .position(|(_, channel, _)| min.rank(*channel).is_some())
        .unwrap_or(0)
}

/// Picks a version: the one with id `wanted`, else the newest the floor allows.
pub(crate) fn pick_version(idx: usize, wanted: Option<&str>, min: MinStability) -> Result<usize> {
    match wanted {
        Some(id) => (0..VERSIONS.len())
            .find(|v| version_id(idx, *v) == id)
            .ok_or_else(|| Error::not_found(format!("version {id}"))),
        None => Ok(newest(min)),
    }
}

pub(crate) fn label(idx: usize, v: usize) -> VersionLabel {
    let (number, channel, _) = VERSIONS.get(v).copied().unwrap_or(VERSIONS[0]);
    VersionLabel {
        id: version_id(idx, v),
        number: number.into(),
        channel,
    }
}

/// A fixed-looking hex digest derived from `seed`.
fn fake_hex(seed: &str, len: usize) -> String {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for byte in seed.bytes().chain([out.len() as u8]) {
            state ^= u64::from(byte);
            state = state.wrapping_mul(0x0100_0000_01b3);
        }
        out.push_str(&format!("{state:016x}"));
    }
    out.truncate(len);
    out
}

pub(crate) fn version_file(idx: usize, v: usize) -> VersionFile {
    let name = file_name(idx, v);
    let big = matches!(ENTRIES.get(idx).map(|e| e.kind), Some(ProjectType::Modpack));
    let size = (40_000 + idx as u64 * 9_000 + v as u64 * 1_000) * if big { 100 } else { 1 };
    let curseforge = ENTRIES
        .get(idx)
        .is_some_and(|e| e.provider == Provider::CurseForge);
    VersionFile {
        url: format!("https://cdn.example.invalid/{name}"),
        sha1: Some(fake_hex(&name, 40)),
        sha512: (!curseforge).then(|| fake_hex(&name, 128)),
        filename: name,
        size,
        primary: true,
    }
}

pub(crate) fn project_version(idx: usize, v: usize) -> ProjectVersion {
    let entry = &ENTRIES[idx];
    let (number, channel, published) = VERSIONS[v];
    let dependencies = if entry.kind == ProjectType::Mod && idx != library_of(entry.provider) {
        vec![Dependency {
            project_id: Some(id_of(library_of(entry.provider))),
            version_id: None,
            file_name: None,
            kind: DependencyKind::Required,
        }]
    } else {
        Vec::new()
    };
    ProjectVersion {
        id: version_id(idx, v),
        project_id: id_of(idx),
        name: format!("{} {number}", entry.title),
        version_number: number.into(),
        channel,
        game_versions: GAME_VERSIONS.map(Into::into).to_vec(),
        loaders: entry.loaders.iter().map(|l| (*l).into()).collect(),
        date_published: published.into(),
        downloads: entry.downloads / (4 + 3 * v as u64),
        files: vec![version_file(idx, v)],
        dependencies,
    }
}

fn category(kind: ProjectType) -> &'static str {
    match kind {
        ProjectType::Mod => "utility",
        ProjectType::Modpack => "adventure",
        ProjectType::ResourcePack => "realistic",
        ProjectType::Shader => "atmosphere",
        ProjectType::DataPack => "recipes",
        _ => "misc",
    }
}

fn follows(idx: usize) -> u64 {
    ENTRIES
        .get(idx)
        .map_or(0, |e| e.downloads / (10 + (idx as u64 % 5)))
}

fn description(entry: &Entry) -> String {
    format!("{}: synthetic catalog entry", entry.title)
}

fn hit(idx: usize) -> SearchHit {
    let entry = &ENTRIES[idx];
    let mut categories: Vec<String> = entry.loaders.iter().map(|l| (*l).into()).collect();
    categories.push(category(entry.kind).into());
    SearchHit {
        project_id: id_of(idx),
        slug: entry.slug.into(),
        title: entry.title.into(),
        description: description(entry),
        project_type: entry.kind,
        author: "mock-author".into(),
        categories,
        game_versions: GAME_VERSIONS.map(Into::into).to_vec(),
        downloads: entry.downloads,
        follows: follows(idx),
        icon_url: None,
        date_modified: entry.updated.into(),
    }
}

fn project(idx: usize) -> Project {
    let entry = &ENTRIES[idx];
    Project {
        id: id_of(idx),
        slug: entry.slug.into(),
        title: entry.title.into(),
        description: description(entry),
        body: "A synthetic project for tests and demos.".into(),
        project_type: entry.kind,
        categories: vec![category(entry.kind).into()],
        loaders: entry.loaders.iter().map(|l| (*l).into()).collect(),
        game_versions: GAME_VERSIONS.map(Into::into).to_vec(),
        downloads: entry.downloads,
        followers: follows(idx),
        icon_url: None,
        license: Some("MIT".into()),
        source_url: None,
        issues_url: None,
        client_side: SideSupport::Required,
        server_side: match entry.kind {
            ProjectType::Mod | ProjectType::Modpack => SideSupport::Optional,
            _ => SideSupport::Unsupported,
        },
        updated: entry.updated.into(),
        version_ids: (0..VERSIONS.len()).map(|v| version_id(idx, v)).collect(),
    }
}

/// `NoApiKey` when `provider` needs the user's key and the mock has none.
pub(crate) fn check_key(state: &State, provider: Provider) -> Result<()> {
    if provider == Provider::CurseForge && !state.curseforge_key {
        return Err(Error::new(
            ErrorKind::NoApiKey,
            "CurseForge needs an API key: enter your own key in the settings",
        ));
    }
    Ok(())
}

fn search(state: &State, query: &SearchQuery) -> Result<SearchPage> {
    query.validate()?;
    check_key(state, query.provider)?;
    let needle = query.query.trim().to_lowercase();
    let mut found: Vec<usize> = ENTRIES
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.provider == query.provider
                && query.project_type.is_none_or(|t| t == e.kind)
                && (needle.is_empty()
                    || e.slug.contains(&needle)
                    || e.title.to_lowercase().contains(&needle))
                && query
                    .game_version
                    .as_deref()
                    .is_none_or(|g| GAME_VERSIONS.contains(&g))
                && query
                    .loader
                    .as_deref()
                    .is_none_or(|l| e.loaders.iter().any(|m| m.eq_ignore_ascii_case(l)))
                && query
                    .categories
                    .iter()
                    .all(|c| c == category(e.kind) || e.loaders.contains(&c.as_str()))
        })
        .map(|(idx, _)| idx)
        .collect();
    match query.sort {
        SearchSort::Downloads => found.sort_by_key(|i| std::cmp::Reverse(ENTRIES[*i].downloads)),
        SearchSort::Follows => found.sort_by_key(|i| std::cmp::Reverse(follows(*i))),
        SearchSort::Newest => found.sort_by_key(|i| std::cmp::Reverse(*i)),
        SearchSort::Updated => found.sort_by_key(|i| std::cmp::Reverse(ENTRIES[*i].updated)),
        _ => {
            // Relevance: titles that start with the text come first, then table order.
            found.sort_by_key(|i| !ENTRIES[*i].title.to_lowercase().starts_with(&needle));
        }
    }
    let total_hits = found.len() as u64;
    let hits = found
        .into_iter()
        .skip(query.offset as usize)
        .take(query.limit as usize)
        .map(hit)
        .collect();
    Ok(SearchPage {
        hits,
        offset: query.offset,
        limit: query.limit,
        total_hits,
    })
}

fn versions(state: &State, key: &str, filter: &VersionFilter) -> Result<Vec<ProjectVersion>> {
    let idx = find(key)?;
    check_key(state, ENTRIES[idx].provider)?;
    Ok((0..VERSIONS.len())
        .map(|v| project_version(idx, v))
        .filter(|v| {
            (filter.loaders.is_empty() || filter.loaders.iter().any(|l| v.loaders.contains(l)))
                && (filter.game_versions.is_empty()
                    || filter
                        .game_versions
                        .iter()
                        .any(|g| v.game_versions.contains(g)))
        })
        .collect())
}

fn only_curseforge(provider: Provider) -> Result<()> {
    if provider == Provider::CurseForge {
        Ok(())
    } else {
        Err(Error::invalid_input("only CurseForge takes an API key"))
    }
}

pub(crate) fn handle(inner: &Arc<Inner>, command: CatalogCommand) -> Result<Response> {
    match command {
        CatalogCommand::Search(query) => search(&inner.lock(), &query).map(Response::CatalogSearch),
        CatalogCommand::Project { id } => {
            let idx = find(&id)?;
            check_key(&inner.lock(), ENTRIES[idx].provider)?;
            Ok(Response::CatalogProject(project(idx)))
        }
        CatalogCommand::Versions {
            project: key,
            filter,
        } => versions(&inner.lock(), &key, &filter).map(Response::CatalogVersions),
        CatalogCommand::VersionsByHashes { .. } => Ok(Response::CatalogHashMatches(Vec::new())),
        CatalogCommand::PlanInstall(request) => {
            installs::plan_install(&inner.lock(), &request).map(Response::InstallPlan)
        }
        CatalogCommand::Install(request) => installs::install(inner, &request),
        CatalogCommand::CheckUpdates(request) => {
            installs::check_updates(&inner.lock(), &request).map(Response::UpdateReport)
        }
        CatalogCommand::PlanUpdate(request) => {
            installs::plan_update(&inner.lock(), &request).map(Response::InstallPlan)
        }
        CatalogCommand::Update(request) => installs::update(inner, &request),
        CatalogCommand::IdentifyMods {
            instance, world, ..
        } => {
            installs::identify(&inner.lock(), &instance, world.as_deref()).map(Response::Identified)
        }
        CatalogCommand::SetApiKey { provider, key } => {
            only_curseforge(provider)?;
            if key.expose().trim().is_empty() {
                return Err(Error::invalid_input("the CurseForge API key is empty"));
            }
            inner.lock().curseforge_key = true;
            Ok(Response::Done)
        }
        CatalogCommand::RemoveApiKey { provider } => {
            only_curseforge(provider)?;
            inner.lock().curseforge_key = false;
            Ok(Response::Done)
        }
        CatalogCommand::CheckApiKey { provider } => {
            only_curseforge(provider)?;
            let set = inner.lock().curseforge_key;
            Ok(Response::ApiKey(ApiKeyStatus {
                provider,
                state: if set {
                    ApiKeyState::Valid
                } else {
                    ApiKeyState::Missing
                },
                source: set.then_some(ApiKeyOrigin::Keyring),
                in_environment: false,
                in_keyring: set,
            }))
        }
        CatalogCommand::InstallPack(request) => packs::install(inner, &request),
        CatalogCommand::CheckPackUpdate(request) => {
            packs::check_update(&inner.lock(), &request).map(Response::PackUpdate)
        }
        CatalogCommand::PlanPackUpdate(request) => {
            packs::plan_update(&inner.lock(), &request).map(Response::PackChanges)
        }
        CatalogCommand::UpdatePack(request) => packs::update(inner, &request),
        CatalogCommand::ExportPack(request) => {
            packs::export(&inner.lock(), &request).map(Response::PackExported)
        }
        other => Err(Error::unsupported(format!(
            "vl-mock does not simulate {other:?}"
        ))),
    }
}
