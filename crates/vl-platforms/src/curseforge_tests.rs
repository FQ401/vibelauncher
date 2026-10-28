use std::collections::VecDeque;
use std::io::Cursor;

use serde_json::{Value, json};
use vl_auth::MemoryVault;
use vl_core::catalog::{ProjectType, SearchSort};
use vl_net::{HttpRequest, HttpResponse, HttpTransport, NetConfig, PostRequest, RetryPolicy};

use super::*;
use crate::rate::fake::FakeClock;

/// A synthetic key; real keys never appear in tests.
const KEY: &str = "$2a$10$synthetic-test-key-0000000000";

/// One request as the server saw it.
#[derive(Clone)]
struct Seen {
    method: &'static str,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Default)]
struct Fake {
    script: Mutex<VecDeque<(u16, Vec<u8>)>>,
    seen: Mutex<Vec<Seen>>,
}

impl Fake {
    fn push(&self, status: u16, body: &Value) {
        self.script
            .lock()
            .unwrap()
            .push_back((status, serde_json::to_vec(body).unwrap()));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn urls(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.url).collect()
    }

    fn respond(&self, seen: Seen) -> HttpResponse {
        self.seen.lock().unwrap().push(seen);
        let (status, body) = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((500, Vec::new()));
        HttpResponse {
            status,
            headers: Vec::new(),
            body: Box::new(Cursor::new(body)),
        }
    }
}

impl HttpTransport for Fake {
    fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
        Ok(self.respond(Seen {
            method: "GET",
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: Vec::new(),
        }))
    }

    fn post(&self, request: &PostRequest) -> std::result::Result<HttpResponse, NetError> {
        Ok(self.respond(Seen {
            method: "POST",
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        }))
    }
}

/// A fixed key, or none.
struct Fixed(Option<&'static str>);

impl ApiKeySource for Fixed {
    fn api_key(&self) -> Result<Option<ApiKey>> {
        self.0.map(ApiKey::new).transpose()
    }
}

struct Setup {
    source: CurseForgeSource,
    fake: Arc<Fake>,
}

fn setup_with(keys: Arc<dyn ApiKeySource>) -> Setup {
    let fake = Arc::new(Fake::default());
    let mut config = NetConfig::new("0.0.0");
    config.retry = RetryPolicy {
        attempts: 2,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
    };
    let client = Client::with_transport(config, fake.clone());
    let options = CurseForgeOptions {
        endpoints: Endpoints::new("https://curseforge.test"),
        ..CurseForgeOptions::default()
    };
    let limiter = Arc::new(RateLimiter::with_clock(
        options.rate,
        Arc::new(FakeClock::default()),
    ));
    Setup {
        source: CurseForgeSource::with_limiter(client, options, keys, limiter),
        fake,
    }
}

fn setup() -> Setup {
    setup_with(Arc::new(Fixed(Some(KEY))))
}

fn file(id: u64, date: &str, versions: &[&str]) -> Value {
    json!({
        "id": id, "modId": 910001, "displayName": format!("file {id}"),
        "fileName": format!("example-{id}.jar"), "releaseType": 1, "fileDate": date,
        "fileLength": 3, "downloadUrl": null, "gameVersions": versions,
        "fileFingerprint": id
    })
}

fn hit(id: u64, slug: &str, class: u64) -> Value {
    json!({"id": id, "slug": slug, "name": slug, "classId": class})
}

#[test]
fn every_request_carries_the_key_header() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": [hit(1, "example", 6)],
                "pagination": {"index": 0, "pageSize": 20, "totalCount": 1}}),
    );
    let query = SearchQuery {
        query: "example".into(),
        project_type: Some(ProjectType::Mod),
        loader: Some("fabric".into()),
        sort: SearchSort::Downloads,
        ..SearchQuery::default()
    };
    let page = s.source.search(&query, &Control::default()).unwrap();
    assert_eq!(page.hits[0].project_id, "1");
    let seen = s.fake.seen();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .url
            .starts_with("https://curseforge.test/v1/mods/search?gameId=432&classId=6&"),
        "{}",
        seen[0].url
    );
    assert!(!seen[0].url.contains(KEY));
    assert_eq!(
        seen[0].headers,
        [(API_KEY_HEADER.to_string(), KEY.to_string())]
    );
}

#[test]
fn no_key_is_a_typed_error_and_sends_nothing() {
    let s = setup_with(Arc::new(Fixed(None)));
    assert!(!s.source.has_key().unwrap());
    let err = s
        .source
        .search(&SearchQuery::text("x"), &Control::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    let err = s.source.project("1", &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    let err = s
        .source
        .fingerprints(&[1], &Control::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    assert!(s.fake.urls().is_empty());
}

#[test]
fn a_refused_key_is_an_auth_error_without_the_key() {
    let s = setup();
    s.fake.push(403, &json!({}));
    let err = s.source.project("1", &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    let shown = format!("{err} {err:?}");
    assert!(
        !shown.contains(KEY) && !shown.contains("synthetic"),
        "{shown}"
    );
}

#[test]
fn invalid_queries_never_reach_the_network() {
    let s = setup();
    let mut query = SearchQuery::text("x");
    query.offset = MAX_RESULT_WINDOW;
    let err = s.source.search(&query, &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert!(s.fake.urls().is_empty());
}

#[test]
fn category_slugs_become_ids() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": [
            {"id": 6, "slug": "mc-mods", "name": "Mods", "isClass": true},
            {"id": 412, "slug": "technology", "name": "Technology", "classId": 6}
        ]}),
    );
    s.fake.push(200, &json!({"data": [], "pagination": {}}));
    let query = SearchQuery {
        project_type: Some(ProjectType::Mod),
        categories: vec!["Technology".into(), "17".into()],
        ..SearchQuery::default()
    };
    s.source.search(&query, &Control::default()).unwrap();
    let urls = s.fake.urls();
    assert_eq!(
        urls[0],
        "https://curseforge.test/v1/categories?gameId=432&classId=6"
    );
    assert!(
        urls[1].contains("categoryIds=%5B412%2C17%5D"),
        "{}",
        urls[1]
    );

    let unknown = SearchQuery {
        categories: vec!["nothing-like-it".into()],
        ..SearchQuery::default()
    };
    s.fake.push(200, &json!({"data": []}));
    let err = s.source.search(&unknown, &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
}

#[test]
fn slugs_are_resolved_preferring_mods() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": [hit(7, "example", 4471), hit(8, "example", 6), hit(9, "other", 6)]}),
    );
    s.fake.push(
        200,
        &json!({"data": {"id": 8, "slug": "example", "classId": 6}}),
    );
    let project = s.source.project("example", &Control::default()).unwrap();
    assert_eq!(project.id, "8");
    let urls = s.fake.urls();
    assert!(urls[0].contains("slug=example"), "{}", urls[0]);
    assert_eq!(urls[1], "https://curseforge.test/v1/mods/8");

    s.fake.push(200, &json!({"data": [hit(9, "other", 6)]}));
    let err = s
        .source
        .project("missing", &Control::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn versions_page_through_filter_and_sort() {
    let s = setup();
    let first: Vec<Value> = (0..50)
        .map(|i| file(1000 + i, "2026-01-01T00:00:00Z", &["1.21.1", "Fabric"]))
        .collect();
    s.fake.push(
        200,
        &json!({"data": first, "pagination": {"index": 0, "pageSize": 50, "totalCount": 52}}),
    );
    s.fake.push(
        200,
        &json!({"data": [
            file(2000, "2026-06-01T00:00:00Z", &["1.21", "Quilt"]),
            file(2001, "2026-07-01T00:00:00Z", &["1.20.1", "Fabric"])
        ], "pagination": {"index": 50, "pageSize": 50, "totalCount": 52}}),
    );
    let filter = VersionFilter {
        loaders: vec!["fabric".into(), "quilt".into()],
        game_versions: vec!["1.21".into(), "1.21.1".into()],
    };
    let versions = s
        .source
        .versions("910001", &filter, &Control::default())
        .unwrap();
    assert_eq!(versions.len(), 51, "the 1.20.1 file does not fit");
    assert_eq!(versions[0].id, "2000", "newest first");
    let urls = s.fake.urls();
    assert_eq!(urls.len(), 2);
    assert_eq!(
        urls[0],
        "https://curseforge.test/v1/mods/910001/files?index=0&pageSize=50"
    );
    assert!(urls[1].ends_with("index=50&pageSize=50"), "{}", urls[1]);
}

#[test]
fn one_file_by_id() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": file(5, "2026-01-01T00:00:00Z", &["1.21.1"])}),
    );
    let v = s.source.file("910001", 5, &Control::default()).unwrap();
    assert_eq!(v.id, "5");
    assert_eq!(
        v.files[0].url,
        "https://edge.forgecdn.net/files/0/5/example-5.jar"
    );
    assert_eq!(
        s.fake.urls(),
        ["https://curseforge.test/v1/mods/910001/files/5"]
    );
}

#[test]
fn fingerprints_are_deduplicated_and_posted() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": {"exactMatches": [
            {"id": 910001, "file": file(17, "2026-01-01T00:00:00Z", &["1.21.1"])},
            {"id": 910001, "file": file(99, "2026-01-01T00:00:00Z", &["1.21.1"])}
        ]}}),
    );
    let found = s
        .source
        .fingerprints(&[17, 3, 17], &Control::default())
        .unwrap();
    assert_eq!(found.len(), 1, "a fingerprint not asked for is ignored");
    assert_eq!(found[0].fingerprint, 17);
    let seen = s.fake.seen();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].url, "https://curseforge.test/v1/fingerprints/432");
    assert_eq!(
        seen[0].headers,
        [(API_KEY_HEADER.to_string(), KEY.to_string())]
    );
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body, json!({"fingerprints": [3, 17]}));

    let too_many = vec![0; MAX_LOOKUP_FINGERPRINTS + 1];
    assert!(
        s.source
            .fingerprints(&too_many, &Control::default())
            .is_err()
    );
}

#[test]
fn file_and_mod_batches_are_posted_once_per_chunk() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": [
            {"id": 7, "modId": 70, "fileName": "a.jar", "downloadUrl": null},
            {"id": 99, "modId": 70, "fileName": "not-asked.jar"}
        ]}),
    );
    let files = s
        .source
        .files_by_ids(&[7, 3, 7], &Control::default())
        .unwrap();
    assert_eq!(files.len(), 1, "a file not asked for is ignored");
    assert_eq!(files[0].download_url, None);
    s.fake.push(
        200,
        &json!({"data": [{"id": 70, "slug": "a", "name": "A", "classId": 12}]}),
    );
    let mods = s.source.mods_by_ids(&[70], &Control::default()).unwrap();
    assert_eq!(mods[0].class_id, 12);
    let seen = s.fake.seen();
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].url, "https://curseforge.test/v1/mods/files");
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body, json!({"fileIds": [3, 7]}));
    assert_eq!(seen[1].url, "https://curseforge.test/v1/mods");
    let body: Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(body, json!({"modIds": [70]}));

    let many: Vec<u64> = (1..=(MAX_IDS_PER_REQUEST as u64 + 1)).collect();
    s.fake.push(200, &json!({"data": []}));
    s.fake.push(200, &json!({"data": []}));
    s.source.files_by_ids(&many, &Control::default()).unwrap();
    assert_eq!(s.fake.seen().len(), 4, "split into two requests");

    let too_many = vec![0; MAX_LOOKUP_IDS + 1];
    assert!(
        s.source
            .mods_by_ids(&too_many, &Control::default())
            .is_err()
    );
}

#[test]
fn batches_without_a_key_send_nothing() {
    let s = setup_with(Arc::new(Fixed(None)));
    for err in [
        s.source
            .files_by_ids(&[1], &Control::default())
            .unwrap_err(),
        s.source.mods_by_ids(&[], &Control::default()).unwrap_err(),
    ] {
        assert_eq!(err.kind, ErrorKind::NoApiKey);
    }
    assert!(s.fake.seen().is_empty());
}

#[test]
fn sha_lookups_are_unsupported_and_files_are_found_by_fingerprint() {
    let s = setup();
    assert_eq!(s.source.file_hash(), HashAlgorithm::Fingerprint);
    let err = s
        .source
        .versions_by_hashes(HashAlgorithm::Sha1, &["a".repeat(40)], &Control::default())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    let err = s
        .source
        .versions_by_hashes(
            HashAlgorithm::Fingerprint,
            &["x1".into()],
            &Control::default(),
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert!(s.fake.seen().is_empty());

    s.fake.push(
        200,
        &json!({"data": {"exactMatches": [
            {"id": 910001, "file": file(17, "2026-01-01T00:00:00Z", &["1.21.1", "Fabric"])}
        ]}}),
    );
    let found = s
        .source
        .versions_by_hashes(
            HashAlgorithm::Fingerprint,
            &["17".into(), "4".into()],
            &Control::default(),
        )
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].hash, "17");
    assert_eq!(found[0].version.id, "17");
    assert_eq!(found[0].version.project_id, "910001");
}

#[test]
fn fingerprint_lookups_without_a_key_send_nothing() {
    let s = setup_with(Arc::new(Fixed(None)));
    let filter = VersionFilter::default();
    for err in [
        s.source
            .versions_by_hashes(HashAlgorithm::Fingerprint, &[], &Control::default())
            .unwrap_err(),
        s.source
            .latest_versions_by_hashes(
                HashAlgorithm::Fingerprint,
                &["17".into()],
                &filter,
                &[],
                &Control::default(),
            )
            .unwrap_err(),
    ] {
        assert_eq!(err.kind, ErrorKind::NoApiKey);
    }
    assert!(s.fake.seen().is_empty());
}

/// A file of project `project` with a release type.
fn typed_file(id: u64, project: u64, date: &str, release: u64, versions: &[&str]) -> Value {
    let mut f = file(id, date, versions);
    f["modId"] = json!(project);
    f["releaseType"] = json!(release);
    f
}

#[test]
fn latest_versions_list_each_project_once_and_pick_the_newest_allowed() {
    let s = setup();
    // Two installed files of project 1 (a mod) and one of project 2 (a resource pack, no
    // loader).
    s.fake.push(
        200,
        &json!({"data": {"exactMatches": [
            {"id": 1, "file": typed_file(11, 1, "2026-01-01T00:00:00Z", 1, &["1.21.1", "Fabric"])},
            {"id": 1, "file": typed_file(12, 1, "2026-01-02T00:00:00Z", 1, &["1.21.1", "Fabric"])},
            {"id": 2, "file": typed_file(21, 2, "2026-01-01T00:00:00Z", 1, &["1.21.1"])}
        ]}}),
    );
    // Project 1: a newer beta, a newer release, a Forge file the server let through.
    s.fake.push(
        200,
        &json!({"data": [
            typed_file(15, 1, "2026-05-01T00:00:00Z", 2, &["1.21.1", "Fabric"]),
            typed_file(14, 1, "2026-04-01T00:00:00Z", 1, &["1.21.1", "Fabric"]),
            typed_file(16, 1, "2026-06-01T00:00:00Z", 1, &["1.21.1", "Forge"]),
            typed_file(12, 1, "2026-01-02T00:00:00Z", 1, &["1.21.1", "Fabric"])
        ], "pagination": {"index": 0, "pageSize": 50, "totalCount": 4}}),
    );
    // Project 2: no loader on its files.
    s.fake.push(
        200,
        &json!({"data": [
            typed_file(22, 2, "2026-03-01T00:00:00Z", 1, &["1.21.1"])
        ], "pagination": {"index": 0, "pageSize": 50, "totalCount": 1}}),
    );
    let filter = VersionFilter {
        loaders: vec!["fabric".into()],
        game_versions: vec!["1.21.1".into()],
    };
    let found = s
        .source
        .latest_versions_by_hashes(
            HashAlgorithm::Fingerprint,
            &["11".into(), "12".into(), "21".into()],
            &filter,
            &[ReleaseChannel::Release],
            &Control::default(),
        )
        .unwrap();
    let pairs: Vec<(&str, &str)> = found
        .iter()
        .map(|m| (m.hash.as_str(), m.version.id.as_str()))
        .collect();
    assert_eq!(pairs, [("11", "14"), ("12", "14"), ("21", "22")]);
    let urls = s.fake.urls();
    assert_eq!(
        urls.len(),
        3,
        "one lookup and one listing per project: {urls:?}"
    );
    assert_eq!(urls[0], "https://curseforge.test/v1/fingerprints/432");
    assert!(urls[1].starts_with("https://curseforge.test/v1/mods/1/files?"));
    assert!(urls[1].contains("gameVersion=1.21.1"), "{}", urls[1]);
    assert!(urls[1].contains("modLoaderType=4"), "{}", urls[1]);
    assert!(urls[2].starts_with("https://curseforge.test/v1/mods/2/files?"));
    assert!(!urls[2].contains("modLoaderType"), "{}", urls[2]);
}

#[test]
fn a_project_curseforge_no_longer_has_is_left_out_of_latest() {
    let s = setup();
    s.fake.push(
        200,
        &json!({"data": {"exactMatches": [
            {"id": 1, "file": typed_file(11, 1, "2026-01-01T00:00:00Z", 1, &["1.21.1"])}
        ]}}),
    );
    s.fake.push(404, &json!({}));
    let found = s
        .source
        .latest_versions_by_hashes(
            HashAlgorithm::Fingerprint,
            &["11".into()],
            &VersionFilter::default(),
            &[],
            &Control::default(),
        )
        .unwrap();
    assert!(found.is_empty());
}

#[test]
fn keyring_key_round_trips_and_is_cached() {
    let vault = Arc::new(MemoryVault::new());
    let keys = KeyringApiKey::new(vault.clone());
    assert_eq!(keys.api_key().unwrap(), None);
    keys.set(&ApiKey::new(KEY).unwrap()).unwrap();
    assert_eq!(vault.raw(KEYRING_ENTRY).as_deref(), Some(KEY));
    assert_eq!(keys.api_key().unwrap().unwrap().expose(), KEY);

    // A second reader sees the stored key; a damaged entry reads as none.
    assert!(
        KeyringApiKey::new(vault.clone())
            .api_key()
            .unwrap()
            .is_some()
    );
    vault.set(KEYRING_ENTRY, "two words").unwrap();
    assert_eq!(KeyringApiKey::new(vault.clone()).api_key().unwrap(), None);

    keys.remove().unwrap();
    assert_eq!(keys.api_key().unwrap(), None);
    assert_eq!(vault.raw(KEYRING_ENTRY), None);
}

#[test]
fn no_keyring_means_no_key() {
    let keys = KeyringApiKey::with_opener(Box::new(|| {
        Err(AuthError::KeyringUnavailable("No keyring.".into()))
    }));
    assert_eq!(keys.api_key().unwrap(), None);
    assert!(keys.set(&ApiKey::new(KEY).unwrap()).is_err());
}

#[test]
fn sources_are_asked_in_order() {
    let none: Arc<dyn ApiKeySource> = Arc::new(Fixed(None));
    let some: Arc<dyn ApiKeySource> = Arc::new(Fixed(Some(KEY)));
    let keys = ApiKeys::new(vec![none.clone(), some]);
    assert_eq!(keys.api_key().unwrap().unwrap().expose(), KEY);
    assert_eq!(ApiKeys::new(vec![none]).api_key().unwrap(), None);
    let env = EnvApiKey::named("VL_TEST_CURSEFORGE_KEY_THAT_IS_NEVER_SET");
    assert_eq!(env.api_key().unwrap(), None);
}

/// Live tests against the real API: they run only when `VL_CURSEFORGE_API_KEY` is set and are
/// skipped (they pass without doing anything) otherwise.
mod live {
    use super::*;

    fn live() -> Option<CurseForgeSource> {
        if EnvApiKey::default().api_key().ok().flatten().is_none() {
            eprintln!("skipped: {API_KEY_ENV} is not set");
            return None;
        }
        let client = Client::new(NetConfig::new("0.0.0")).ok()?;
        Some(CurseForgeSource::new(
            client,
            CurseForgeOptions::default(),
            Arc::new(EnvApiKey::default()),
        ))
    }

    #[test]
    fn live_search_project_and_files() {
        let Some(source) = live() else { return };
        let control = Control::default();
        let query = SearchQuery {
            query: "jei".into(),
            project_type: Some(ProjectType::Mod),
            game_version: Some("1.20.1".into()),
            loader: Some("forge".into()),
            categories: vec!["map-information".into()],
            sort: SearchSort::Downloads,
            limit: 10,
            ..SearchQuery::default()
        };
        let page = source.search(&query, &control).unwrap();
        assert!(page.hits.iter().any(|h| h.slug == "jei"), "{page:?}");

        let packs = SearchQuery {
            project_type: Some(ProjectType::Modpack),
            offset: MAX_RESULT_WINDOW - MAX_PAGE_SIZE,
            limit: MAX_PAGE_SIZE,
            ..SearchQuery::default()
        };
        let page = source.search(&packs, &control).unwrap();
        assert!(
            page.hits
                .iter()
                .all(|h| h.project_type == ProjectType::Modpack)
        );

        let project = source.project("jei", &control).unwrap();
        assert_eq!(project.id, "238222");
        assert_eq!(project.project_type, ProjectType::Mod);

        let filter = VersionFilter {
            loaders: vec!["forge".into()],
            game_versions: vec!["1.20.1".into()],
        };
        let versions = source.versions("238222", &filter, &control).unwrap();
        assert!(!versions.is_empty());
        assert!(
            versions
                .iter()
                .all(|v| v.game_versions.iter().any(|g| g == "1.20.1"))
        );
        let newest = &versions[0];
        let file = source
            .file("238222", newest.id.parse().unwrap(), &control)
            .unwrap();
        assert_eq!(&file, newest);

        let categories = source.categories(Some(ProjectType::Mod), &control).unwrap();
        assert!(categories.iter().any(|c| c.slug == "map-information"));
    }

    #[test]
    fn live_fingerprint_of_a_downloaded_file_is_found() {
        let Some(source) = live() else { return };
        let control = Control::default();
        let filter = VersionFilter {
            loaders: vec!["forge".into()],
            game_versions: vec!["1.12.2".into()],
        };
        let versions = source.versions("238222", &filter, &control).unwrap();
        let smallest = versions
            .iter()
            .filter_map(|v| v.primary_file().map(|f| (v, f)))
            .min_by_key(|(_, f)| f.size)
            .unwrap();
        let bytes = source
            .client
            .get_bytes(
                &smallest.1.url,
                &GetOptions {
                    max_bytes: Some(64 * 1024 * 1024),
                    max_age: None,
                    use_cache: false,
                },
                &control,
            )
            .unwrap()
            .bytes;
        let print = vl_curseforge::fingerprint(&bytes);
        let found = source.fingerprints(&[print, 1], &control).unwrap();
        assert_eq!(
            found.len(),
            1,
            "fingerprint {print} of {}",
            smallest.1.filename
        );
        assert_eq!(found[0].version.id, smallest.0.id);

        // The same file through the platform interface, and its newest release.
        let key = print.to_string();
        let owners = source
            .versions_by_hashes(
                HashAlgorithm::Fingerprint,
                std::slice::from_ref(&key),
                &control,
            )
            .unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].version.project_id, "238222");
        let latest = source
            .latest_versions_by_hashes(
                HashAlgorithm::Fingerprint,
                &[key],
                &filter,
                &[ReleaseChannel::Release],
                &control,
            )
            .unwrap();
        assert_eq!(latest.len(), 1, "{latest:?}");
        let newest = &latest[0].version;
        assert_eq!(newest.project_id, "238222");
        assert_eq!(newest.channel, ReleaseChannel::Release);
        assert!(newest.game_versions.iter().any(|g| g == "1.12.2"));
        assert!(
            newest.date_published >= smallest.0.date_published
                || smallest.0.channel != ReleaseChannel::Release
        );
        assert!(
            newest
                .primary_file()
                .and_then(|f| f.sha1.as_ref())
                .is_some()
        );
    }
}
