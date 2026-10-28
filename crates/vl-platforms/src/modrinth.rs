//! Modrinth over `vl-net`.
//!
//! Reading needs no key. The launcher's `User-Agent` (set by `vl-net`) names the launcher and its
//! address, as Modrinth requires. Every network attempt goes through the [`RateLimiter`]; answers
//! are kept in the HTTP cache of the client and reused while young, and an old copy serves when
//! the network is down.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectVersion, Provider, SearchPage, SearchQuery,
    VersionFilter,
};
use vl_core::{Error, ErrorKind, Result};
use vl_modrinth::parse::{self, MAX_PROJECT_BYTES, MAX_SEARCH_BYTES, MAX_VERSIONS_BYTES};
use vl_modrinth::{Endpoints, MAX_HASHES_PER_REQUEST};
use vl_net::{Client, Control, GetOptions, NetError, RequestHooks};

use crate::CatalogSource;
use crate::rate::{RateConfig, RateLimiter};

/// The most hashes one lookup accepts.
pub const MAX_LOOKUP_HASHES: usize = 10_000;

/// How a [`ModrinthSource`] works.
#[derive(Debug, Clone)]
pub struct ModrinthOptions {
    /// The API address.
    pub endpoints: Endpoints,
    /// Rate limiting.
    pub rate: RateConfig,
    /// How long a search answer is used without asking again.
    pub search_max_age: Duration,
    /// How long a project or version list is used without asking again.
    pub project_max_age: Duration,
}

impl Default for ModrinthOptions {
    fn default() -> Self {
        Self {
            endpoints: Endpoints::default(),
            rate: RateConfig::default(),
            search_max_age: Duration::from_secs(5 * 60),
            project_max_age: Duration::from_secs(10 * 60),
        }
    }
}

/// The Modrinth API as a [`CatalogSource`].
pub struct ModrinthSource {
    client: Client,
    options: ModrinthOptions,
    limiter: Arc<RateLimiter>,
}

impl ModrinthSource {
    /// A source with its own rate limiter.
    pub fn new(client: Client, options: ModrinthOptions) -> Self {
        let limiter = Arc::new(RateLimiter::new(options.rate));
        Self::with_limiter(client, options, limiter)
    }

    /// A source with a given rate limiter (shared, or on a test clock).
    pub fn with_limiter(
        client: Client,
        options: ModrinthOptions,
        limiter: Arc<RateLimiter>,
    ) -> Self {
        Self {
            client,
            options,
            limiter,
        }
    }

    fn get(
        &self,
        url: &str,
        max_bytes: usize,
        max_age: Duration,
        control: &Control,
    ) -> Result<Vec<u8>> {
        let options = GetOptions {
            max_bytes: Some(max_bytes as u64),
            max_age: Some(max_age),
            use_cache: true,
        };
        let hooks: &dyn RequestHooks = &*self.limiter;
        self.client
            .get_bytes_with(url, &options, control, Some(hooks))
            .map(|fetched| fetched.bytes)
            .map_err(convert)
    }

    /// The version that owns the file with this hash, if Modrinth knows it.
    pub fn version_by_hash(
        &self,
        algorithm: HashAlgorithm,
        hash: &str,
        control: &Control,
    ) -> Result<Option<ProjectVersion>> {
        let url = self.options.endpoints.version_by_hash(hash, algorithm)?;
        // A file's owner does not change: the cached answer may be old.
        match self.get(
            &url,
            MAX_PROJECT_BYTES,
            Duration::from_secs(24 * 3600),
            control,
        ) {
            Ok(bytes) => parse::parse_version(&bytes).map(Some),
            Err(e) if e.kind == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl CatalogSource for ModrinthSource {
    fn provider(&self) -> Provider {
        Provider::Modrinth
    }

    fn search(&self, query: &SearchQuery, control: &Control) -> Result<SearchPage> {
        let url = self.options.endpoints.search(query)?;
        let bytes = self.get(&url, MAX_SEARCH_BYTES, self.options.search_max_age, control)?;
        parse::parse_search(&bytes)
    }

    fn project(&self, id: &str, control: &Control) -> Result<Project> {
        let url = self.options.endpoints.project(id)?;
        let bytes = self.get(
            &url,
            MAX_PROJECT_BYTES,
            self.options.project_max_age,
            control,
        )?;
        parse::parse_project(&bytes)
    }

    fn versions(
        &self,
        project: &str,
        filter: &VersionFilter,
        control: &Control,
    ) -> Result<Vec<ProjectVersion>> {
        let url = self.options.endpoints.versions(project, filter)?;
        let bytes = self.get(
            &url,
            MAX_VERSIONS_BYTES,
            self.options.project_max_age,
            control,
        )?;
        parse::parse_versions(&bytes)
    }

    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        control: &Control,
    ) -> Result<Vec<HashMatch>> {
        if hashes.len() > MAX_LOOKUP_HASHES {
            return Err(Error::invalid_input(format!(
                "at most {MAX_LOOKUP_HASHES} hashes per lookup"
            )));
        }
        let unique: Vec<String> = hashes
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let url = self.options.endpoints.versions_by_hashes_url();
        let hooks: &dyn RequestHooks = &*self.limiter;
        let mut matches = Vec::new();
        for chunk in unique.chunks(MAX_HASHES_PER_REQUEST) {
            let body = self
                .options
                .endpoints
                .versions_by_hashes_body(chunk, algorithm)?;
            let bytes = self
                .client
                .post_bytes(
                    &url,
                    "application/json",
                    &body,
                    Some(MAX_VERSIONS_BYTES as u64),
                    control,
                    Some(hooks),
                )
                .map_err(convert)?;
            matches.extend(parse::parse_version_files(&bytes, algorithm)?);
        }
        matches.sort_by(|a, b| a.hash.cmp(&b.hash));
        Ok(matches)
    }
}

/// Converts a network failure; `410 Gone` means the API version was retired.
fn convert(error: NetError) -> Error {
    match &error {
        NetError::Status { status: 410, .. } => Error::new(
            ErrorKind::Unsupported,
            "Modrinth no longer serves this API version; update the launcher",
        ),
        NetError::Status { status: 429, .. } => Error::new(
            ErrorKind::Network,
            "Modrinth is rate limiting this address; try again in a minute",
        ),
        _ => Error::from(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use vl_core::catalog::{DependencyKind, ProjectType};
    use vl_net::{HttpRequest, HttpResponse, HttpTransport, NetConfig, PostRequest, RetryPolicy};

    use super::*;
    use crate::rate::fake::FakeClock;

    struct Scripted {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn reply(status: u16, body: &Value) -> Scripted {
        Scripted {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(body).unwrap(),
        }
    }

    #[derive(Default)]
    struct Fake {
        script: Mutex<VecDeque<Scripted>>,
        /// `(method, url, body)` of every request.
        seen: Mutex<Vec<(&'static str, String, Vec<u8>)>>,
    }

    impl Fake {
        fn push(&self, s: Scripted) {
            self.script.lock().unwrap().push_back(s);
        }

        fn urls(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|(_, u, _)| u.clone())
                .collect()
        }

        fn respond(&self, method: &'static str, url: &str, body: &[u8]) -> HttpResponse {
            self.seen
                .lock()
                .unwrap()
                .push((method, url.to_string(), body.to_vec()));
            let next = self.script.lock().unwrap().pop_front();
            let s = next.unwrap_or(Scripted {
                status: 500,
                headers: Vec::new(),
                body: Vec::new(),
            });
            HttpResponse {
                status: s.status,
                headers: s.headers,
                body: Box::new(Cursor::new(s.body)),
            }
        }
    }

    impl HttpTransport for Fake {
        fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
            Ok(self.respond("GET", &request.url, &[]))
        }

        fn post(&self, request: &PostRequest) -> std::result::Result<HttpResponse, NetError> {
            Ok(self.respond("POST", &request.url, &request.body))
        }
    }

    struct Setup {
        source: ModrinthSource,
        fake: Arc<Fake>,
        clock: Arc<FakeClock>,
    }

    fn setup(cache_dir: Option<PathBuf>) -> Setup {
        let fake = Arc::new(Fake::default());
        let mut config = NetConfig::new("0.0.0");
        config.cache_dir = cache_dir;
        config.retry = RetryPolicy {
            attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        };
        let client = Client::with_transport(config, fake.clone());
        let clock = Arc::new(FakeClock::default());
        let options = ModrinthOptions {
            endpoints: Endpoints::new("https://modrinth.test/v2"),
            ..ModrinthOptions::default()
        };
        let limiter = Arc::new(RateLimiter::with_clock(options.rate, clock.clone()));
        Setup {
            source: ModrinthSource::with_limiter(client, options, limiter),
            fake,
            clock,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-platforms-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn version(id: &str, sha1: &str) -> Value {
        json!({
            "id": id, "project_id": "PROJ1", "name": id, "version_number": "1.0",
            "version_type": "release", "game_versions": ["1.21.4"], "loaders": ["fabric"],
            "files": [{"hashes": {"sha1": sha1}, "url": "https://cdn.test/a.jar",
                       "filename": "a.jar", "primary": true, "size": 3}],
            "dependencies": [{"project_id": "DEP1", "dependency_type": "required"}]
        })
    }

    #[test]
    fn searches_with_facets_and_parses_hits() {
        let s = setup(None);
        s.fake.push(reply(
            200,
            &json!({"hits": [{"project_id": "P1", "slug": "example", "title": "Example",
                              "project_type": "mod"}],
                    "offset": 0, "limit": 20, "total_hits": 1}),
        ));
        let query = SearchQuery {
            query: "example".into(),
            project_type: Some(ProjectType::Mod),
            game_version: Some("1.21.4".into()),
            loader: Some("fabric".into()),
            ..SearchQuery::default()
        };
        let page = s.source.search(&query, &Control::default()).unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.hits[0].slug, "example");
        let urls = s.fake.urls();
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("https://modrinth.test/v2/search?query=example&facets="));
        assert!(urls[0].contains("project_type%3Amod"), "{}", urls[0]);
    }

    #[test]
    fn invalid_queries_never_reach_the_network() {
        let s = setup(None);
        let mut query = SearchQuery::text("x");
        query.limit = 0;
        let err = s.source.search(&query, &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(s.fake.urls().is_empty());
    }

    #[test]
    fn missing_projects_and_retired_api_versions_are_told_apart() {
        let s = setup(None);
        s.fake.push(reply(404, &json!({"error": "not_found"})));
        let err = s.source.project("nope", &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);

        s.fake.push(reply(410, &json!({})));
        let err = s.source.project("old", &Control::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
        assert!(err.message.contains("update the launcher"), "{err}");
    }

    #[test]
    fn lists_versions_with_a_filter_and_dependencies() {
        let s = setup(None);
        s.fake
            .push(reply(200, &json!([version("v2", "a"), version("v1", "b")])));
        let filter = VersionFilter {
            loaders: vec!["fabric".into()],
            game_versions: vec!["1.21.4".into()],
        };
        let versions = s
            .source
            .versions("example", &filter, &Control::default())
            .unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].id, "v2", "the server's order is kept");
        assert_eq!(versions[0].dependencies[0].kind, DependencyKind::Required);
        let url = &s.fake.urls()[0];
        assert!(url.contains("/project/example/version?"), "{url}");
        assert!(url.contains("loaders=%5B%22fabric%22%5D"), "{url}");
    }

    #[test]
    fn hash_lookups_are_batched_deduplicated_and_validated() {
        let s = setup(None);
        let h1 = "1".repeat(40);
        let h2 = "2".repeat(40);
        s.fake.push(reply(
            200,
            &json!({ h1.clone(): version("v1", &h1), "9".repeat(40): "garbage" }),
        ));
        let found = s
            .source
            .versions_by_hashes(
                HashAlgorithm::Sha1,
                &[h1.clone(), h1.to_uppercase(), h2.clone()],
                &Control::default(),
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].hash, h1);
        let seen = s.fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one request for both hashes");
        assert_eq!(seen[0].0, "POST");
        let body: Value = serde_json::from_slice(&seen[0].2).unwrap();
        assert_eq!(body["hashes"], json!([h1, h2]));
        assert_eq!(body["algorithm"], "sha1");
        drop(seen);

        // Nothing to look up sends nothing; a bad digest is refused before sending.
        let none = s
            .source
            .versions_by_hashes(HashAlgorithm::Sha1, &[], &Control::default())
            .unwrap();
        assert!(none.is_empty());
        let err = s
            .source
            .versions_by_hashes(HashAlgorithm::Sha1, &["xyz".into()], &Control::default())
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert_eq!(s.fake.urls().len(), 1);
    }

    #[test]
    fn large_lookups_are_split_into_requests() {
        let s = setup(None);
        let hashes: Vec<String> = (0..MAX_HASHES_PER_REQUEST + 1)
            .map(|i| format!("{i:040x}"))
            .collect();
        s.fake.push(reply(200, &json!({})));
        s.fake.push(reply(200, &json!({})));
        let found = s
            .source
            .versions_by_hashes(HashAlgorithm::Sha1, &hashes, &Control::default())
            .unwrap();
        assert!(found.is_empty());
        assert_eq!(s.fake.urls().len(), 2);
    }

    #[test]
    fn one_version_by_hash_reads_an_unknown_file_as_none() {
        let s = setup(None);
        let hash = "3".repeat(40);
        s.fake.push(reply(404, &json!({})));
        let none = s
            .source
            .version_by_hash(HashAlgorithm::Sha1, &hash, &Control::default())
            .unwrap();
        assert!(none.is_none());
        s.fake.push(reply(200, &version("v1", &hash)));
        let some = s
            .source
            .version_by_hash(HashAlgorithm::Sha1, &hash, &Control::default())
            .unwrap();
        assert_eq!(some.map(|v| v.id), Some("v1".to_string()));
        assert!(s.fake.urls()[0].ends_with(&format!("/version_file/{hash}?algorithm=sha1")));
    }

    #[test]
    fn a_429_waits_for_the_reset_and_then_succeeds() {
        let s = setup(None);
        s.fake.push(Scripted {
            status: 429,
            headers: vec![("x-ratelimit-reset".into(), "4".into())],
            body: Vec::new(),
        });
        s.fake.push(reply(
            200,
            &json!({"id": "P1", "slug": "example", "title": "Example"}),
        ));
        let project = s.source.project("example", &Control::default()).unwrap();
        assert_eq!(project.id, "P1");
        assert_eq!(s.fake.urls().len(), 2);
        assert_eq!(s.clock.slept(), [Duration::from_secs(4)]);
    }

    #[test]
    fn persistent_rate_limiting_is_reported_as_a_network_error() {
        let s = setup(None);
        for _ in 0..3 {
            s.fake.push(Scripted {
                status: 429,
                headers: vec![("x-ratelimit-reset".into(), "1".into())],
                body: Vec::new(),
            });
        }
        let err = s
            .source
            .project("example", &Control::default())
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Network);
        assert!(err.message.contains("rate limiting"), "{err}");
    }

    #[test]
    fn many_requests_never_exceed_the_window_budget() {
        let s = setup(None);
        let budget = s.source.options.rate.budget as usize;
        for _ in 0..budget + 5 {
            s.fake.push(reply(200, &json!({"hits": []})));
        }
        for i in 0..budget + 5 {
            let q = SearchQuery::text(format!("q{i}"));
            s.source.search(&q, &Control::default()).unwrap();
        }
        // The first `budget` requests went at time zero; the next ones waited a full window.
        assert_eq!(s.fake.urls().len(), budget + 5);
        assert_eq!(s.clock.slept(), [Duration::from_secs(60)]);
    }

    #[test]
    fn young_answers_come_from_the_cache_without_a_request() {
        let dir = temp_dir("cache");
        let s = setup(Some(dir.clone()));
        s.fake.push(reply(
            200,
            &json!({"id": "P1", "slug": "example", "title": "Example"}),
        ));
        let first = s.source.project("example", &Control::default()).unwrap();
        let second = s.source.project("example", &Control::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(s.fake.urls().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
