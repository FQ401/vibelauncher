//! CurseForge over `vl-net`.
//!
//! Every request carries the user's API key in the `x-api-key` header; the launcher ships no key.
//! The key comes from an [`ApiKeySource`]: [`default_api_keys`] reads the
//! `VL_CURSEFORGE_API_KEY` environment variable (for development) and then the user's key in the
//! OS keyring ([`KeyringApiKey`], the store that keeps account tokens). Without a key every call
//! fails with `ErrorKind::NoApiKey` before anything is sent; the other platforms are not
//! affected. A key CurseForge refuses (`401`, `403`) is `ErrorKind::Auth`. The key is never part
//! of a URL, so neither the HTTP cache nor an error message holds it.
//!
//! Local files are found by their CurseForge fingerprint, not by SHA hash
//! ([`CatalogSource::file_hash`] is [`HashAlgorithm::Fingerprint`]).
//!
//! Answers go through the HTTP cache of the client like Modrinth's, and every network attempt
//! goes through a [`RateLimiter`] (CurseForge publishes no limit; the window budget and its
//! `429` handling apply).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vl_auth::{AuthError, OsVault, SecretVault};
use vl_core::catalog::{
    ApiKeyState, HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion, Provider,
    ReleaseChannel, SearchPage, SearchQuery, VersionFilter,
};
use vl_core::{Error, ErrorKind, Result};
use vl_curseforge::parse::{
    self, Category, FingerprintMatch, MAX_BATCH_BYTES, MAX_CATEGORIES_BYTES, MAX_FILES_BYTES,
    MAX_FINGERPRINTS_BYTES, MAX_PROJECT_BYTES, MAX_SEARCH_BYTES, ModFile, ModSummary,
};
use vl_curseforge::{
    API_KEY_ENV, API_KEY_HEADER, Endpoints, MAX_FINGERPRINTS_PER_REQUEST, MAX_IDS_PER_REQUEST,
    MAX_PAGE_SIZE, MAX_RESULT_WINDOW, Page, numeric_id, rejected_api_key, unsent_filter,
};
use vl_net::{Client, Control, GetOptions, NetError, RequestHooks};

use crate::CatalogSource;
use crate::rate::{RateConfig, RateLimiter};

pub use vl_curseforge::ApiKey;

/// The keyring entry (under the launcher's service name) that holds the user's key.
pub const KEYRING_ENTRY: &str = "api-key:curseforge";

/// The most fingerprints one lookup accepts.
pub const MAX_LOOKUP_FINGERPRINTS: usize = 10_000;

/// The most project or file ids one batch lookup accepts.
pub const MAX_LOOKUP_IDS: usize = 20_000;

/// Where the CurseForge API key comes from.
pub trait ApiKeySource: Send + Sync {
    /// The key to send, if one is set. Errors never contain the key.
    fn api_key(&self) -> Result<Option<ApiKey>>;
}

/// The key in an environment variable (by default [`API_KEY_ENV`]), read on every call.
#[derive(Debug, Clone)]
pub struct EnvApiKey {
    name: String,
}

impl Default for EnvApiKey {
    fn default() -> Self {
        Self::named(API_KEY_ENV)
    }
}

impl EnvApiKey {
    /// The key in the variable `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl ApiKeySource for EnvApiKey {
    fn api_key(&self) -> Result<Option<ApiKey>> {
        match std::env::var(&self.name) {
            Ok(value) if value.trim().is_empty() => Ok(None),
            Ok(value) => ApiKey::new(value).map(Some).map_err(|_| {
                Error::invalid_input(format!("{} does not hold a valid API key", self.name))
            }),
            Err(_) => Ok(None),
        }
    }
}

type Opener = dyn Fn() -> std::result::Result<Arc<dyn SecretVault>, AuthError> + Send + Sync;

/// The user's key in the OS keyring, entry [`KEYRING_ENTRY`]. The keyring is opened on first
/// use and the key is kept in memory after the first read; [`set`](Self::set) and
/// [`remove`](Self::remove) change both. Without a usable keyring there is no key.
pub struct KeyringApiKey {
    open: Box<Opener>,
    state: Mutex<KeyState>,
}

#[derive(Default)]
struct KeyState {
    vault: Option<Arc<dyn SecretVault>>,
    /// The last value read or written; `None` when not read yet.
    cached: Option<Option<ApiKey>>,
}

impl KeyringApiKey {
    /// The key in the OS keyring.
    pub fn os() -> Self {
        Self::with_opener(Box::new(|| {
            OsVault::open().map(|v| Arc::new(v) as Arc<dyn SecretVault>)
        }))
    }

    /// The key in a given vault (tests use a memory vault).
    pub fn new(vault: Arc<dyn SecretVault>) -> Self {
        Self::with_opener(Box::new(move || Ok(vault.clone())))
    }

    fn with_opener(open: Box<Opener>) -> Self {
        Self {
            open,
            state: Mutex::new(KeyState::default()),
        }
    }

    fn vault(&self, state: &mut KeyState) -> std::result::Result<Arc<dyn SecretVault>, AuthError> {
        if let Some(vault) = &state.vault {
            return Ok(vault.clone());
        }
        let vault = (self.open)()?;
        state.vault = Some(vault.clone());
        Ok(vault)
    }

    /// Stores the user's key.
    pub fn set(&self, key: &ApiKey) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.vault(&mut state)?.set(KEYRING_ENTRY, key.expose())?;
        state.cached = Some(Some(key.clone()));
        Ok(())
    }

    /// Forgets the user's key; no key is not an error.
    pub fn remove(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.vault(&mut state)?.delete(KEYRING_ENTRY)?;
        state.cached = Some(None);
        Ok(())
    }
}

impl ApiKeySource for KeyringApiKey {
    fn api_key(&self) -> Result<Option<ApiKey>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cached) = &state.cached {
            return Ok(cached.clone());
        }
        let vault = match self.vault(&mut state) {
            Ok(vault) => vault,
            // No keyring on this machine: no stored key, and the user is asked for one.
            Err(AuthError::KeyringUnavailable(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let key = match vault.get(KEYRING_ENTRY) {
            // A damaged entry is as good as none: the user enters the key again.
            Ok(text) => text.and_then(|t| ApiKey::new(t).ok()),
            Err(AuthError::KeyringUnavailable(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        state.cached = Some(key.clone());
        Ok(key)
    }
}

/// Several sources asked in order; the first key found wins.
#[derive(Default)]
pub struct ApiKeys {
    sources: Vec<Arc<dyn ApiKeySource>>,
}

impl ApiKeys {
    /// The sources, in order.
    pub fn new(sources: Vec<Arc<dyn ApiKeySource>>) -> Self {
        Self { sources }
    }
}

impl ApiKeySource for ApiKeys {
    fn api_key(&self) -> Result<Option<ApiKey>> {
        for source in &self.sources {
            if let Some(key) = source.api_key()? {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }
}

/// The environment variable (development), then the user's key in the OS keyring.
pub fn default_api_keys(keyring: Arc<KeyringApiKey>) -> ApiKeys {
    ApiKeys::new(vec![Arc::new(EnvApiKey::default()), keyring])
}

/// The keys the launcher knows of: the environment variable (read only) and the user's key in
/// the keyring (set and removed here). The environment variable wins when both hold a key.
#[derive(Clone)]
pub struct CurseForgeKeys {
    env: Arc<EnvApiKey>,
    keyring: Arc<KeyringApiKey>,
}

/// Which places hold a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPlaces {
    /// The environment variable holds a key.
    pub environment: bool,
    /// The keyring holds a key.
    pub keyring: bool,
}

impl CurseForgeKeys {
    /// [`API_KEY_ENV`] and the OS keyring.
    pub fn os() -> Self {
        Self::new(EnvApiKey::default(), Arc::new(KeyringApiKey::os()))
    }

    /// A given variable and keyring (tests use a variable that is never set and a memory vault).
    pub fn new(env: EnvApiKey, keyring: Arc<KeyringApiKey>) -> Self {
        Self {
            env: Arc::new(env),
            keyring,
        }
    }

    /// The source a [`CurseForgeSource`] reads: the variable, then the keyring.
    pub fn source(&self) -> Arc<dyn ApiKeySource> {
        Arc::new(ApiKeys::new(vec![
            self.env.clone() as Arc<dyn ApiKeySource>,
            self.keyring.clone(),
        ]))
    }

    /// Stores the user's key in the keyring.
    pub fn set(&self, key: &ApiKey) -> Result<()> {
        self.keyring.set(key)
    }

    /// Removes the user's key from the keyring; no key is not an error.
    pub fn remove(&self) -> Result<()> {
        self.keyring.remove()
    }

    /// Which places hold a key. A variable that holds something that is not a key counts as
    /// holding one: it is what CurseForge requests would fail on.
    pub fn places(&self) -> Result<KeyPlaces> {
        Ok(KeyPlaces {
            environment: !matches!(self.env.api_key(), Ok(None)),
            keyring: self.keyring.api_key()?.is_some(),
        })
    }
}

/// How a [`CurseForgeSource`] works.
#[derive(Debug, Clone)]
pub struct CurseForgeOptions {
    /// The API address.
    pub endpoints: Endpoints,
    /// Rate limiting.
    pub rate: RateConfig,
    /// How long a search answer is used without asking again.
    pub search_max_age: Duration,
    /// How long a project or file list is used without asking again.
    pub project_max_age: Duration,
    /// How long the category list is used without asking again.
    pub categories_max_age: Duration,
    /// The most pages of files read for one project.
    pub max_file_pages: usize,
}

impl Default for CurseForgeOptions {
    fn default() -> Self {
        Self {
            endpoints: Endpoints::default(),
            rate: RateConfig::default(),
            search_max_age: Duration::from_secs(5 * 60),
            project_max_age: Duration::from_secs(10 * 60),
            categories_max_age: Duration::from_secs(24 * 3600),
            max_file_pages: 20,
        }
    }
}

/// The CurseForge API as a [`CatalogSource`], plus categories, single files and fingerprint
/// lookups. Project ids are CurseForge's numeric mod ids; a slug is looked up by a search.
pub struct CurseForgeSource {
    client: Client,
    options: CurseForgeOptions,
    limiter: Arc<RateLimiter>,
    keys: Arc<dyn ApiKeySource>,
}

/// The rate limiter plus the key header.
struct Hooks<'a> {
    limiter: &'a RateLimiter,
    key: &'a ApiKey,
}

impl RequestHooks for Hooks<'_> {
    fn before_request(&self, control: &Control) -> std::result::Result<(), NetError> {
        self.limiter.before_request(control)
    }

    fn after_response(&self, status: u16, headers: &[(String, String)]) {
        self.limiter.after_response(status, headers);
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![(API_KEY_HEADER.to_string(), self.key.expose().to_string())]
    }
}

impl CurseForgeSource {
    /// A source with its own rate limiter.
    pub fn new(client: Client, options: CurseForgeOptions, keys: Arc<dyn ApiKeySource>) -> Self {
        let limiter = Arc::new(RateLimiter::new(options.rate));
        Self::with_limiter(client, options, keys, limiter)
    }

    /// A source with a given rate limiter (shared, or on a test clock).
    pub fn with_limiter(
        client: Client,
        options: CurseForgeOptions,
        keys: Arc<dyn ApiKeySource>,
        limiter: Arc<RateLimiter>,
    ) -> Self {
        Self {
            client,
            options,
            limiter,
            keys,
        }
    }

    /// Whether a key is set (the interface asks for one when not).
    pub fn has_key(&self) -> Result<bool> {
        Ok(self.keys.api_key()?.is_some())
    }

    fn key(&self) -> Result<ApiKey> {
        self.keys.api_key()?.ok_or_else(vl_curseforge::no_api_key)
    }

    /// Tries the key in use with one small request that bypasses the HTTP cache
    /// (`GET /v1/games/432`). `Missing` without a key (nothing is sent), `Rejected` on `401` or
    /// `403`, `Unreachable` on any other failure. Only cancellation is an error.
    pub fn check_key(&self, control: &Control) -> Result<ApiKeyState> {
        let Some(key) = self.keys.api_key()? else {
            return Ok(ApiKeyState::Missing);
        };
        let options = GetOptions {
            max_bytes: Some(MAX_PROJECT_BYTES as u64),
            max_age: None,
            use_cache: false,
        };
        let hooks = Hooks {
            limiter: &self.limiter,
            key: &key,
        };
        let url = self.options.endpoints.game();
        match self
            .client
            .get_bytes_with(&url, &options, control, Some(&hooks))
            .map_err(convert)
        {
            Ok(_) => Ok(ApiKeyState::Valid),
            Err(e) if e.kind == ErrorKind::Auth => Ok(ApiKeyState::Rejected),
            Err(e) if e.kind == ErrorKind::Cancelled => Err(e),
            Err(e) => Ok(ApiKeyState::Unreachable { detail: e.message }),
        }
    }

    fn get(
        &self,
        url: &str,
        max_bytes: usize,
        max_age: Duration,
        control: &Control,
    ) -> Result<Vec<u8>> {
        let key = self.key()?;
        let options = GetOptions {
            max_bytes: Some(max_bytes as u64),
            max_age: Some(max_age),
            use_cache: true,
        };
        let hooks = Hooks {
            limiter: &self.limiter,
            key: &key,
        };
        self.client
            .get_bytes_with(url, &options, control, Some(&hooks))
            .map(|fetched| fetched.bytes)
            .map_err(convert)
    }

    fn post(&self, url: &str, body: &[u8], max_bytes: usize, control: &Control) -> Result<Vec<u8>> {
        let key = self.key()?;
        let hooks = Hooks {
            limiter: &self.limiter,
            key: &key,
        };
        self.client
            .post_bytes(
                url,
                "application/json",
                body,
                Some(max_bytes as u64),
                control,
                Some(&hooks),
            )
            .map_err(convert)
    }

    /// The numeric mod id of a project given by id or slug. A slug that names projects of
    /// several classes picks the mod, then the modpack, then the first.
    pub fn mod_id(&self, id: &str, control: &Control) -> Result<u64> {
        if let Some(id) = numeric_id(id) {
            return Ok(id);
        }
        let url = self.options.endpoints.search_slug(id, None)?;
        let bytes = self.get(
            &url,
            MAX_SEARCH_BYTES,
            self.options.project_max_age,
            control,
        )?;
        let hits = parse::parse_search(&bytes)?.hits;
        let exact: Vec<_> = hits
            .iter()
            .filter(|h| h.slug.eq_ignore_ascii_case(id.trim()))
            .collect();
        let chosen = [ProjectType::Mod, ProjectType::Modpack]
            .iter()
            .find_map(|kind| exact.iter().find(|h| h.project_type == *kind))
            .or_else(|| exact.first())
            .ok_or_else(|| Error::not_found(format!("no CurseForge project {:?}", id.trim())))?;
        numeric_id(&chosen.project_id).ok_or_else(|| Error::internal("a parsed id is numeric"))
    }

    /// The categories and classes of Minecraft projects, of one project type when given.
    pub fn categories(
        &self,
        kind: Option<ProjectType>,
        control: &Control,
    ) -> Result<Vec<Category>> {
        let url = self.options.endpoints.categories(kind);
        let bytes = self.get(
            &url,
            MAX_CATEGORIES_BYTES,
            self.options.categories_max_age,
            control,
        )?;
        parse::parse_categories(&bytes)
    }

    /// The query with category slugs or names replaced by CurseForge's category ids.
    fn with_category_ids(&self, query: &SearchQuery, control: &Control) -> Result<SearchQuery> {
        if query.categories.iter().all(|c| numeric_id(c).is_some()) {
            return Ok(query.clone());
        }
        let known = self.categories(query.project_type, control)?;
        let mut resolved = query.clone();
        for category in &mut resolved.categories {
            if numeric_id(category).is_some() {
                continue;
            }
            let wanted = category.trim();
            let found = known
                .iter()
                .filter(|c| !c.is_class)
                .find(|c| {
                    c.slug.eq_ignore_ascii_case(wanted) || c.name.eq_ignore_ascii_case(wanted)
                })
                .ok_or_else(|| {
                    Error::invalid_input(format!("CurseForge has no category {wanted:?}"))
                })?;
            *category = found.id.to_string();
        }
        Ok(resolved)
    }

    /// One file of a project, as a version.
    pub fn file(&self, project: &str, file_id: u64, control: &Control) -> Result<ProjectVersion> {
        let mod_id = self.mod_id(project, control)?;
        let url = self.options.endpoints.file(mod_id, file_id);
        let bytes = self.get(
            &url,
            MAX_PROJECT_BYTES,
            self.options.project_max_age,
            control,
        )?;
        parse::parse_file(&bytes)
    }

    /// The files CurseForge knows by these fingerprints ([`vl_curseforge::fingerprint`]), sorted
    /// by fingerprint; unknown fingerprints are left out. Repeats are asked once, in requests of
    /// at most [`MAX_FINGERPRINTS_PER_REQUEST`].
    pub fn fingerprints(
        &self,
        fingerprints: &[u32],
        control: &Control,
    ) -> Result<Vec<FingerprintMatch>> {
        if fingerprints.len() > MAX_LOOKUP_FINGERPRINTS {
            return Err(Error::invalid_input(format!(
                "at most {MAX_LOOKUP_FINGERPRINTS} fingerprints per lookup"
            )));
        }
        let unique: Vec<u32> = fingerprints
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let url = self.options.endpoints.fingerprints_url();
        let mut matches = Vec::new();
        for chunk in unique.chunks(MAX_FINGERPRINTS_PER_REQUEST) {
            let body = self.options.endpoints.fingerprints_body(chunk)?;
            let bytes = self.post(&url, &body, MAX_FINGERPRINTS_BYTES, control)?;
            matches.extend(
                parse::parse_fingerprints(&bytes)?
                    .into_iter()
                    .filter(|m| chunk.binary_search(&m.fingerprint).is_ok()),
            );
        }
        matches.sort_by_key(|m| m.fingerprint);
        Ok(matches)
    }

    /// The files with these ids (`POST /v1/mods/files`), in requests of at most
    /// [`MAX_IDS_PER_REQUEST`]; repeats are asked once and ids CurseForge does not know are
    /// left out. Without a key this fails with `NoApiKey` before anything is sent.
    pub fn files_by_ids(&self, file_ids: &[u64], control: &Control) -> Result<Vec<ModFile>> {
        let url = self.options.endpoints.files_by_ids_url();
        self.batch(file_ids, control, |chunk| {
            let body = self.options.endpoints.files_by_ids_body(chunk)?;
            let bytes = self.post(&url, &body, MAX_BATCH_BYTES, control)?;
            Ok(parse::parse_mod_files(&bytes)?
                .into_iter()
                .filter(|f| chunk.binary_search(&f.id).is_ok())
                .collect())
        })
    }

    /// The projects with these ids (`POST /v1/mods`), in requests of at most
    /// [`MAX_IDS_PER_REQUEST`]; repeats are asked once and unknown ids are left out. Without a
    /// key this fails with `NoApiKey` before anything is sent.
    pub fn mods_by_ids(&self, mod_ids: &[u64], control: &Control) -> Result<Vec<ModSummary>> {
        let url = self.options.endpoints.mods_by_ids_url();
        self.batch(mod_ids, control, |chunk| {
            let body = self.options.endpoints.mods_by_ids_body(chunk)?;
            let bytes = self.post(&url, &body, MAX_BATCH_BYTES, control)?;
            Ok(parse::parse_mods(&bytes)?
                .into_iter()
                .filter(|m| chunk.binary_search(&m.id).is_ok())
                .collect())
        })
    }

    fn batch<T>(
        &self,
        ids: &[u64],
        control: &Control,
        mut ask: impl FnMut(&[u64]) -> Result<Vec<T>>,
    ) -> Result<Vec<T>> {
        // The key is checked even for an empty list, so a missing key is reported at once.
        self.key()?;
        if ids.len() > MAX_LOOKUP_IDS {
            return Err(Error::invalid_input(format!(
                "at most {MAX_LOOKUP_IDS} ids per lookup"
            )));
        }
        let unique: Vec<u64> = ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut out = Vec::new();
        for chunk in unique.chunks(MAX_IDS_PER_REQUEST) {
            if control.cancel.is_cancelled() {
                return Err(Error::cancelled());
            }
            out.extend(ask(chunk)?);
        }
        Ok(out)
    }
}

impl CatalogSource for CurseForgeSource {
    fn provider(&self) -> Provider {
        Provider::CurseForge
    }

    fn search(&self, query: &SearchQuery, control: &Control) -> Result<SearchPage> {
        // Checked before the key and the categories, so a bad query never costs a request.
        self.options.endpoints.search(&SearchQuery {
            categories: Vec::new(),
            ..query.clone()
        })?;
        let query = self.with_category_ids(query, control)?;
        let url = self.options.endpoints.search(&query)?;
        let bytes = self.get(&url, MAX_SEARCH_BYTES, self.options.search_max_age, control)?;
        parse::parse_search(&bytes)
    }

    fn project(&self, id: &str, control: &Control) -> Result<Project> {
        let mod_id = self.mod_id(id, control)?;
        let url = self.options.endpoints.project(mod_id);
        let bytes = self.get(
            &url,
            MAX_PROJECT_BYTES,
            self.options.project_max_age,
            control,
        )?;
        parse::parse_project(&bytes)
    }

    /// Reads the project's files page by page (at most
    /// [`CurseForgeOptions::max_file_pages`] pages of [`MAX_PAGE_SIZE`]), keeps those that fit
    /// the filter, newest first.
    fn versions(
        &self,
        project: &str,
        filter: &VersionFilter,
        control: &Control,
    ) -> Result<Vec<ProjectVersion>> {
        let mod_id = self.mod_id(project, control)?;
        let unsent = unsent_filter(filter);
        let mut versions = Vec::new();
        let mut index = 0;
        for _ in 0..self.options.max_file_pages.max(1) {
            if index >= MAX_RESULT_WINDOW {
                break;
            }
            let page = Page::new(index, MAX_PAGE_SIZE)?;
            let url = self.options.endpoints.files(mod_id, filter, page);
            let bytes = self.get(&url, MAX_FILES_BYTES, self.options.project_max_age, control)?;
            let files = parse::parse_files(&bytes)?;
            versions.extend(
                files
                    .versions
                    .into_iter()
                    .filter(|v| parse::fits(v, &unsent)),
            );
            index += page.page_size;
            if files.received < page.page_size as usize || u64::from(index) >= files.total {
                break;
            }
        }
        // RFC 3339 times in UTC sort as text; the sort is stable for equal times.
        versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
        Ok(versions)
    }

    fn file_hash(&self) -> HashAlgorithm {
        HashAlgorithm::Fingerprint
    }

    /// The files with these fingerprints (decimal, [`HashAlgorithm::Fingerprint`]) through
    /// [`fingerprints`](Self::fingerprints); the hash of a match is the fingerprint in decimal.
    /// SHA hashes are `Unsupported`.
    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        control: &Control,
    ) -> Result<Vec<HashMatch>> {
        let prints = parse_fingerprints(algorithm, hashes)?;
        // The key is checked even for an empty list, so a missing key is reported at once.
        self.key()?;
        Ok(self
            .fingerprints(&prints, control)?
            .into_iter()
            .map(|m| HashMatch {
                hash: m.fingerprint.to_string(),
                version: m.version,
            })
            .collect())
    }

    /// CurseForge has no batched update lookup: the files are found by fingerprint in one
    /// batch, then the files of each of their projects are listed once with the game versions
    /// and loaders of `filter` ([`versions`](CatalogSource::versions)), and the newest one of an
    /// allowed channel is the answer. A file that names no loader (resource packs, shaders,
    /// some old mods) is matched against files that name none or one of the filter's. A project
    /// CurseForge no longer has is left out.
    fn latest_versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        filter: &VersionFilter,
        channels: &[ReleaseChannel],
        control: &Control,
    ) -> Result<Vec<HashMatch>> {
        let owners = self.versions_by_hashes(algorithm, hashes, control)?;
        let mut listed: HashMap<(String, bool), Vec<ProjectVersion>> = HashMap::new();
        let mut out = Vec::new();
        for owner in owners {
            let project = owner.version.project_id.clone();
            if numeric_id(&project).is_none() {
                continue;
            }
            let unlabeled = owner.version.loaders.is_empty();
            let key = (project, unlabeled);
            if !listed.contains_key(&key) {
                let sent = VersionFilter {
                    loaders: if unlabeled {
                        Vec::new()
                    } else {
                        filter.loaders.clone()
                    },
                    game_versions: filter.game_versions.clone(),
                };
                let versions = match self.versions(&key.0, &sent, control) {
                    Ok(v) => v,
                    Err(e) if e.kind == ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(e),
                };
                listed.insert(key.clone(), versions);
            }
            // The server filters too, but its answer is data: check it.
            let games = VersionFilter {
                loaders: Vec::new(),
                game_versions: filter.game_versions.clone(),
            };
            let fits = |v: &ProjectVersion| {
                parse::fits(v, &games)
                    && (filter.loaders.is_empty()
                        || (unlabeled && v.loaders.is_empty())
                        || v.loaders
                            .iter()
                            .any(|l| filter.loaders.iter().any(|w| w.eq_ignore_ascii_case(l))))
            };
            // Listed newest first.
            let newest = listed
                .get(&key)
                .into_iter()
                .flatten()
                .find(|v| (channels.is_empty() || channels.contains(&v.channel)) && fits(v));
            if let Some(version) = newest {
                out.push(HashMatch {
                    hash: owner.hash,
                    version: version.clone(),
                });
            }
        }
        Ok(out)
    }
}

/// The fingerprints in `hashes`, which must be of [`HashAlgorithm::Fingerprint`].
fn parse_fingerprints(algorithm: HashAlgorithm, hashes: &[String]) -> Result<Vec<u32>> {
    if algorithm != HashAlgorithm::Fingerprint {
        return Err(Error::unsupported(
            "CurseForge looks files up by fingerprint, not by SHA hash",
        ));
    }
    hashes
        .iter()
        .map(|h| {
            let h = h.trim();
            if HashAlgorithm::Fingerprint.is_valid(h) {
                h.parse::<u32>()
                    .map_err(|_| Error::invalid_input("not a CurseForge fingerprint"))
            } else {
                Err(Error::invalid_input(format!(
                    "{h:?} is not a CurseForge fingerprint"
                )))
            }
        })
        .collect()
}

/// Converts a network failure. The key is never in a URL, so no message holds it.
fn convert(error: NetError) -> Error {
    match &error {
        NetError::Status {
            status: 401 | 403, ..
        } => rejected_api_key(),
        NetError::Status { status: 429, .. } => Error::new(
            ErrorKind::Network,
            "CurseForge is rate limiting this address; try again in a minute",
        ),
        _ => Error::from(error),
    }
}

#[cfg(test)]
#[path = "curseforge_tests.rs"]
mod tests;
