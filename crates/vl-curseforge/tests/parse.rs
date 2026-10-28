//! Parsing of hand-written CurseForge responses (fictional projects in the API's shape).

use vl_core::catalog::{DependencyKind, ProjectType, ReleaseChannel, SideSupport};
use vl_curseforge::parse::{
    parse_categories, parse_file, parse_files, parse_fingerprints, parse_project, parse_search,
};

const SEARCH: &[u8] = include_bytes!("fixtures/search.json");
const MOD: &[u8] = include_bytes!("fixtures/mod.json");
const FILES: &[u8] = include_bytes!("fixtures/files.json");
const CATEGORIES: &[u8] = include_bytes!("fixtures/categories.json");
const FINGERPRINTS: &[u8] = include_bytes!("fixtures/fingerprints.json");

#[test]
fn search_hits_paging_and_damaged_entries() {
    let page = parse_search(SEARCH).unwrap();
    assert_eq!(page.offset, 20);
    assert_eq!(page.limit, 20);
    assert_eq!(
        page.total_hits, 10_000,
        "cut to the window CurseForge serves"
    );
    assert_eq!(page.hits.len(), 2, "the entry without an id is skipped");

    let hit = &page.hits[0];
    assert_eq!(hit.project_id, "910001");
    assert_eq!(hit.slug, "example-storage");
    assert_eq!(hit.title, "Example Storage");
    assert_eq!(hit.description, "Chests that sort themselves.");
    assert_eq!(hit.project_type, ProjectType::Mod);
    assert_eq!(hit.author, "example_author");
    assert_eq!(
        hit.categories,
        ["storage", "technology", "fabric", "neoforge", "forge"]
    );
    assert_eq!(hit.game_versions, ["1.21.1", "1.20.1"]);
    assert_eq!(hit.downloads, 1_234_567, "a decimal count is read");
    assert_eq!(hit.follows, 42);
    assert_eq!(
        hit.icon_url.as_deref(),
        Some("https://media.forgecdn.net/avatars/thumbnails/0/5/256/256/logo.png")
    );
    assert_eq!(hit.date_modified, "2026-09-30T12:00:00.123Z");

    let pack = &page.hits[1];
    assert_eq!(pack.project_type, ProjectType::Modpack);
    assert!(pack.categories.contains(&"forge".to_string()));
    assert_eq!(pack.icon_url, None);
    assert_eq!(pack.author, "");
}

#[test]
fn project_details() {
    let project = parse_project(MOD).unwrap();
    assert_eq!(project.id, "910001");
    assert_eq!(project.slug, "example-storage");
    assert_eq!(project.project_type, ProjectType::Mod);
    assert_eq!(project.categories, ["storage"]);
    assert_eq!(project.loaders, ["fabric", "forge", "quilt"]);
    assert_eq!(project.game_versions, ["1.21.1", "1.20.1", "1.12.2"]);
    assert_eq!(project.downloads, 1_234_567);
    assert_eq!(project.followers, 42);
    assert_eq!(
        project.icon_url.as_deref(),
        Some("https://media.forgecdn.net/avatars/0/5/logo.png"),
        "the full logo when there is no thumbnail"
    );
    assert_eq!(project.source_url, None, "not a web link");
    assert_eq!(
        project.issues_url.as_deref(),
        Some("https://git.example.org/storage/issues")
    );
    assert_eq!(project.client_side, SideSupport::Unknown);
    assert_eq!(project.version_ids, ["5100200", "5000100", "2500000"]);
    assert!(project.body.is_empty());
}

#[test]
fn files_become_versions() {
    let page = parse_files(FILES).unwrap();
    assert_eq!(page.received, 5);
    assert_eq!(page.total, 5);
    assert_eq!(
        page.versions.len(),
        3,
        "the entries without an id are skipped"
    );

    let v = &page.versions[0];
    assert_eq!(v.id, "5100200");
    assert_eq!(v.project_id, "910001");
    assert_eq!(v.name, "Example Storage 2.0.0 (Fabric)");
    assert_eq!(v.channel, ReleaseChannel::Release);
    assert_eq!(v.game_versions, ["1.21.1", "1.21"]);
    assert_eq!(v.loaders, ["fabric"]);
    assert_eq!(v.date_published, "2026-09-30T11:59:00Z");
    assert_eq!(v.downloads, 1000);
    let file = v.primary_file().unwrap();
    assert_eq!(file.filename, "example-storage-fabric-2.0.0.jar");
    assert_eq!(
        file.url,
        "https://edge.forgecdn.net/files/5100/200/example-storage-fabric-2.0.0.jar"
    );
    assert_eq!(file.size, 524_288);
    assert_eq!(
        file.sha1.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(file.sha512, None);
    let deps: Vec<(Option<&str>, DependencyKind)> = v
        .dependencies
        .iter()
        .map(|d| (d.project_id.as_deref(), d.kind))
        .collect();
    assert_eq!(
        deps,
        [
            (Some("306612"), DependencyKind::Required),
            (Some("910050"), DependencyKind::Optional),
            (Some("910051"), DependencyKind::Incompatible),
            (Some("910052"), DependencyKind::Embedded),
        ]
    );

    // No download URL: the CDN address is built from the id and the encoded name.
    let beta = &page.versions[1];
    assert_eq!(beta.channel, ReleaseChannel::Beta);
    assert_eq!(beta.loaders, ["forge", "neoforge"]);
    assert_eq!(beta.downloads, 55);
    assert!(beta.dependencies.is_empty());
    let file = beta.primary_file().unwrap();
    assert_eq!(
        file.url,
        "https://edge.forgecdn.net/files/5000/100/example%20storage%20forge%201.9.0%2Bbeta.jar"
    );
    assert_eq!(file.sha1, None, "a damaged hash is dropped");

    // An unsafe file name keeps the version but loses the file.
    let alpha = &page.versions[2];
    assert_eq!(alpha.channel, ReleaseChannel::Alpha);
    assert!(alpha.files.is_empty());
}

#[test]
fn one_file() {
    let body = format!(
        r#"{{"data": {}}}"#,
        serde_json::to_string(
            &serde_json::from_slice::<serde_json::Value>(FILES).unwrap()["data"][0]
        )
        .unwrap()
    );
    let v = parse_file(body.as_bytes()).unwrap();
    assert_eq!(v.id, "5100200");
    assert!(parse_file(br#"{"data": "garbage"}"#).is_err());
}

#[test]
fn categories_and_classes() {
    let categories = parse_categories(CATEGORIES).unwrap();
    assert_eq!(categories.len(), 3);
    assert!(categories[0].is_class);
    assert_eq!(categories[0].slug, "mc-mods");
    assert_eq!(categories[0].class_id, None);
    assert_eq!(categories[1].id, 412);
    assert_eq!(categories[1].name, "Technology");
    assert_eq!(categories[1].class_id, Some(6));
    assert_eq!(categories[2].parent_id, Some(412));
    assert_eq!(categories[2].icon_url, None, "not a web link");
}

#[test]
fn fingerprint_matches() {
    let matches = parse_fingerprints(FINGERPRINTS).unwrap();
    let found: Vec<(u32, &str)> = matches
        .iter()
        .map(|m| (m.fingerprint, m.version.id.as_str()))
        .collect();
    assert_eq!(found, [(17, "5000300"), (3_000_000_000, "5100200")]);
    assert_eq!(
        matches[0].version.primary_file().unwrap().url,
        "https://edge.forgecdn.net/files/5000/300/example-library-1.0.jar"
    );
    assert!(parse_fingerprints(br#"{"data": []}"#).is_err());
}
