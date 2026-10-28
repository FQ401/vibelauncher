//! Request URLs and bodies of the CurseForge API.

use vl_core::catalog::{ProjectType, SearchQuery, SearchSort, VersionFilter};
use vl_core::{Error, Result};

use crate::loader::ModLoader;

/// The public API.
pub const DEFAULT_BASE: &str = "https://api.curseforge.com";

/// CurseForge's id of Minecraft.
pub const GAME_ID: u32 = 432;

/// The largest page CurseForge serves.
pub const MAX_PAGE_SIZE: u32 = 50;

/// CurseForge serves results only while `index + pageSize` stays within this.
pub const MAX_RESULT_WINDOW: u32 = 10_000;

/// The most category ids one search may filter by.
pub const MAX_CATEGORY_IDS: usize = 10;

/// The most fingerprints one `/v1/fingerprints` request carries.
pub const MAX_FINGERPRINTS_PER_REQUEST: usize = 1_000;

/// The most ids one `POST /v1/mods` or `POST /v1/mods/files` request carries.
pub const MAX_IDS_PER_REQUEST: usize = 500;

/// The longest slug accepted.
const MAX_SLUG_LEN: usize = 128;

/// The longest numeric id accepted (in digits).
const MAX_ID_DIGITS: usize = 12;

/// The CurseForge class (`classId`) of a project type; `None` for [`ProjectType::Other`].
pub fn class_id(kind: ProjectType) -> Option<u32> {
    match kind {
        ProjectType::Mod => Some(6),
        ProjectType::Modpack => Some(4471),
        ProjectType::ResourcePack => Some(12),
        ProjectType::Shader => Some(6552),
        ProjectType::DataPack => Some(6945),
        _ => None,
    }
}

/// The project type of a CurseForge class; [`ProjectType::Other`] for classes the launcher does
/// not handle (worlds, plugins, customization, ...).
pub fn project_type(class_id: u64) -> ProjectType {
    match class_id {
        6 => ProjectType::Mod,
        4471 => ProjectType::Modpack,
        12 => ProjectType::ResourcePack,
        6552 => ProjectType::Shader,
        6945 => ProjectType::DataPack,
        _ => ProjectType::Other,
    }
}

/// A page of a CurseForge list (`index`, `pageSize`) inside the result window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// How many results to skip.
    pub index: u32,
    /// How many results to return.
    pub page_size: u32,
}

impl Page {
    /// The page from `offset` with up to `limit` results: the size is cut to [`MAX_PAGE_SIZE`]
    /// and so that `index + pageSize` stays within [`MAX_RESULT_WINDOW`]. An offset at or past
    /// the window, or a zero limit, is refused.
    pub fn new(offset: u32, limit: u32) -> Result<Self> {
        if offset >= MAX_RESULT_WINDOW {
            return Err(Error::invalid_input(format!(
                "CurseForge serves only the first {MAX_RESULT_WINDOW} results; narrow the search"
            )));
        }
        let page_size = limit.min(MAX_PAGE_SIZE).min(MAX_RESULT_WINDOW - offset);
        if page_size == 0 {
            return Err(Error::invalid_input("a page holds at least one result"));
        }
        Ok(Self {
            index: offset,
            page_size,
        })
    }
}

/// Builds request URLs for one API base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    base: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::new(DEFAULT_BASE)
    }
}

impl Endpoints {
    /// Endpoints under `base` (a trailing slash is ignored).
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    /// `GET /v1/mods/search`. The categories of the query must be numeric category ids (at most
    /// [`MAX_CATEGORY_IDS`]); the platform source turns slugs into ids first. The page follows
    /// [`Page::new`]. A loader CurseForge does not know is refused.
    pub fn search(&self, query: &SearchQuery) -> Result<String> {
        query.validate()?;
        let page = Page::new(query.offset, query.limit)?;
        let mut params = vec![format!("gameId={GAME_ID}")];
        if let Some(kind) = query.project_type {
            let class = class_id(kind).ok_or_else(|| {
                Error::unsupported(format!("CurseForge has no {} projects", kind.name()))
            })?;
            params.push(format!("classId={class}"));
        }
        let text = query.query.trim();
        if !text.is_empty() {
            params.push(format!("searchFilter={}", encode(text)));
        }
        if let Some(version) = query.game_version.as_deref() {
            params.push(format!("gameVersion={}", encode(version.trim())));
        }
        if let Some(loader) = query.loader.as_deref() {
            let loader = ModLoader::parse(loader).ok_or_else(|| {
                Error::invalid_input(format!("CurseForge does not know the loader {loader:?}"))
            })?;
            params.push(format!("modLoaderType={}", loader.id()));
        }
        if !query.categories.is_empty() {
            let ids = category_ids(&query.categories)?;
            params.push(format!("categoryIds={}", encode(&ids)));
        }
        params.push(format!("sortField={}", sort_field(query.sort)));
        params.push("sortOrder=desc".to_string());
        params.push(format!("index={}", page.index));
        params.push(format!("pageSize={}", page.page_size));
        Ok(format!("{}/v1/mods/search?{}", self.base, params.join("&")))
    }

    /// `GET /v1/mods/search` for the projects with this slug (of one class when given).
    pub fn search_slug(&self, slug: &str, kind: Option<ProjectType>) -> Result<String> {
        let slug = slug.trim();
        if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
            return Err(Error::invalid_input(format!(
                "a slug is 1 to {MAX_SLUG_LEN} characters"
            )));
        }
        let mut params = vec![format!("gameId={GAME_ID}")];
        if let Some(class) = kind.and_then(class_id) {
            params.push(format!("classId={class}"));
        }
        params.push(format!("slug={}", encode(slug)));
        params.push(format!("pageSize={MAX_PAGE_SIZE}"));
        Ok(format!("{}/v1/mods/search?{}", self.base, params.join("&")))
    }

    /// `GET /v1/mods/{modId}`.
    pub fn project(&self, mod_id: u64) -> String {
        format!("{}/v1/mods/{mod_id}", self.base)
    }

    /// `GET /v1/mods/{modId}/files`, newest first. CurseForge filters by one game version and
    /// one loader: a filter with exactly one of either is sent, anything else is left to the
    /// caller ([`crate::parse::fits`]).
    pub fn files(&self, mod_id: u64, filter: &VersionFilter, page: Page) -> String {
        let mut params = Vec::new();
        if let [version] = clean(&filter.game_versions).as_slice() {
            params.push(format!("gameVersion={}", encode(version)));
        }
        if let [loader] = clean(&filter.loaders).as_slice()
            && let Some(loader) = ModLoader::parse(loader)
        {
            params.push(format!("modLoaderType={}", loader.id()));
        }
        params.push(format!("index={}", page.index));
        params.push(format!("pageSize={}", page.page_size));
        format!("{}/v1/mods/{mod_id}/files?{}", self.base, params.join("&"))
    }

    /// `GET /v1/mods/{modId}/files/{fileId}`.
    pub fn file(&self, mod_id: u64, file_id: u64) -> String {
        format!("{}/v1/mods/{mod_id}/files/{file_id}", self.base)
    }

    /// `GET /v1/categories` of Minecraft, of one class when given.
    pub fn categories(&self, kind: Option<ProjectType>) -> String {
        match kind.and_then(class_id) {
            Some(class) => format!(
                "{}/v1/categories?gameId={GAME_ID}&classId={class}",
                self.base
            ),
            None => format!("{}/v1/categories?gameId={GAME_ID}", self.base),
        }
    }

    /// `GET /v1/games/432`: Minecraft's game record, a small answer that needs a valid key (the
    /// key check).
    pub fn game(&self) -> String {
        format!("{}/v1/games/{GAME_ID}", self.base)
    }

    /// `POST /v1/fingerprints/432`: the URL.
    pub fn fingerprints_url(&self) -> String {
        format!("{}/v1/fingerprints/{GAME_ID}", self.base)
    }

    /// `POST /v1/fingerprints/432`: the JSON body for at most [`MAX_FINGERPRINTS_PER_REQUEST`]
    /// fingerprints.
    pub fn fingerprints_body(&self, fingerprints: &[u32]) -> Result<Vec<u8>> {
        if fingerprints.len() > MAX_FINGERPRINTS_PER_REQUEST {
            return Err(Error::invalid_input(format!(
                "at most {MAX_FINGERPRINTS_PER_REQUEST} fingerprints per request"
            )));
        }
        serde_json::to_vec(&serde_json::json!({ "fingerprints": fingerprints }))
            .map_err(|e| Error::internal(e.to_string()))
    }

    /// `POST /v1/mods/files`: the URL.
    pub fn files_by_ids_url(&self) -> String {
        format!("{}/v1/mods/files", self.base)
    }

    /// `POST /v1/mods/files`: the JSON body for at most [`MAX_IDS_PER_REQUEST`] file ids.
    pub fn files_by_ids_body(&self, file_ids: &[u64]) -> Result<Vec<u8>> {
        ids_body("fileIds", file_ids)
    }

    /// `POST /v1/mods`: the URL.
    pub fn mods_by_ids_url(&self) -> String {
        format!("{}/v1/mods", self.base)
    }

    /// `POST /v1/mods`: the JSON body for at most [`MAX_IDS_PER_REQUEST`] mod ids.
    pub fn mods_by_ids_body(&self, mod_ids: &[u64]) -> Result<Vec<u8>> {
        ids_body("modIds", mod_ids)
    }
}

fn ids_body(key: &str, ids: &[u64]) -> Result<Vec<u8>> {
    if ids.len() > MAX_IDS_PER_REQUEST {
        return Err(Error::invalid_input(format!(
            "at most {MAX_IDS_PER_REQUEST} ids per request"
        )));
    }
    serde_json::to_vec(&serde_json::json!({ key: ids })).map_err(|e| Error::internal(e.to_string()))
}

/// The part of `filter` that [`Endpoints::files`] does not send to the server: check it on the
/// answer with [`crate::parse::fits`].
pub fn unsent_filter(filter: &VersionFilter) -> VersionFilter {
    let game_versions = match clean(&filter.game_versions).as_slice() {
        [_] => Vec::new(),
        _ => filter.game_versions.clone(),
    };
    let loaders = match clean(&filter.loaders).as_slice() {
        [loader] if ModLoader::parse(loader).is_some() => Vec::new(),
        _ => filter.loaders.clone(),
    };
    VersionFilter {
        loaders,
        game_versions,
    }
}

/// A numeric CurseForge id (a project or file id) from text; `None` when it is not one.
pub fn numeric_id(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits =
        !text.is_empty() && text.len() <= MAX_ID_DIGITS && text.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| text.parse().ok()).flatten()
}

/// `ModsSearchSortField` for a sort order.
fn sort_field(sort: SearchSort) -> u32 {
    match sort {
        SearchSort::Downloads => 6,
        SearchSort::Follows => 2,
        SearchSort::Newest => 11,
        SearchSort::Updated => 3,
        // Relevance, and orders added later: the platform's featured order.
        _ => 1,
    }
}

/// The category filter as a JSON array of ids.
fn category_ids(categories: &[String]) -> Result<String> {
    if categories.len() > MAX_CATEGORY_IDS {
        return Err(Error::invalid_input(format!(
            "CurseForge filters by at most {MAX_CATEGORY_IDS} categories"
        )));
    }
    let mut ids = Vec::with_capacity(categories.len());
    for category in categories {
        let id = numeric_id(category).ok_or_else(|| {
            Error::invalid_input("CurseForge categories are filtered by numeric id")
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    serde_json::to_string(&ids).map_err(|e| Error::internal(e.to_string()))
}

fn clean(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_carries_class_filters_sort_and_paging() {
        let q = SearchQuery {
            query: "fast & light".into(),
            project_type: Some(ProjectType::Mod),
            game_version: Some("1.20.1".into()),
            loader: Some("NeoForge".into()),
            categories: vec!["423".into(), "412".into(), "423".into()],
            sort: SearchSort::Downloads,
            offset: 40,
            limit: 20,
            ..SearchQuery::default()
        };
        let url = Endpoints::default().search(&q).unwrap();
        assert_eq!(
            url,
            "https://api.curseforge.com/v1/mods/search?gameId=432&classId=6\
             &searchFilter=fast%20%26%20light&gameVersion=1.20.1&modLoaderType=6\
             &categoryIds=%5B423%2C412%5D&sortField=6&sortOrder=desc&index=40&pageSize=20"
        );
    }

    #[test]
    fn modpack_search_uses_its_class() {
        let q = SearchQuery {
            project_type: Some(ProjectType::Modpack),
            ..SearchQuery::default()
        };
        let url = Endpoints::new("http://localhost:1/").search(&q).unwrap();
        assert_eq!(
            url,
            "http://localhost:1/v1/mods/search?gameId=432&classId=4471\
             &sortField=1&sortOrder=desc&index=0&pageSize=20"
        );
    }

    #[test]
    fn paging_stays_inside_the_result_window() {
        assert_eq!(
            Page::new(0, 100).unwrap(),
            Page {
                index: 0,
                page_size: 50
            }
        );
        assert_eq!(Page::new(9_990, 50).unwrap().page_size, 10);
        assert!(Page::new(10_000, 1).is_err());
        assert!(Page::new(5, 0).is_err());
        let q = SearchQuery {
            offset: 9_995,
            limit: 20,
            ..SearchQuery::default()
        };
        let url = Endpoints::default().search(&q).unwrap();
        assert!(url.ends_with("&index=9995&pageSize=5"), "{url}");
    }

    #[test]
    fn invalid_searches_are_refused() {
        let e = Endpoints::default();
        let mut q = SearchQuery::text("x");
        q.loader = Some("rift".into());
        assert!(e.search(&q).is_err());
        let mut q = SearchQuery::text("x");
        q.categories = vec!["technology".into()];
        assert!(e.search(&q).is_err());
        let mut q = SearchQuery::text("x");
        q.categories = (0..11).map(|i| i.to_string()).collect();
        assert!(e.search(&q).is_err());
        let mut q = SearchQuery::text("x");
        q.project_type = Some(ProjectType::Other);
        assert_eq!(
            e.search(&q).unwrap_err().kind,
            vl_core::ErrorKind::Unsupported
        );
        let mut q = SearchQuery::text("x");
        q.offset = 10_000;
        assert!(e.search(&q).is_err());
    }

    #[test]
    fn slug_search_is_encoded() {
        let e = Endpoints::default();
        assert_eq!(
            e.search_slug(" jei ", Some(ProjectType::Mod)).unwrap(),
            "https://api.curseforge.com/v1/mods/search?gameId=432&classId=6&slug=jei&pageSize=50"
        );
        assert!(e.search_slug("a/b", None).unwrap().contains("slug=a%2Fb"));
        assert!(e.search_slug(" ", None).is_err());
    }

    #[test]
    fn files_url_sends_single_filters_only() {
        let e = Endpoints::default();
        let page = Page::new(0, 50).unwrap();
        let one = VersionFilter {
            loaders: vec!["fabric".into()],
            game_versions: vec!["1.21.1".into(), " ".into()],
        };
        assert_eq!(
            e.files(238_222, &one, page),
            "https://api.curseforge.com/v1/mods/238222/files?gameVersion=1.21.1\
             &modLoaderType=4&index=0&pageSize=50"
        );
        let many = VersionFilter {
            loaders: vec!["fabric".into(), "quilt".into()],
            game_versions: vec!["1.21".into(), "1.21.1".into()],
        };
        assert_eq!(
            e.files(1, &many, page),
            "https://api.curseforge.com/v1/mods/1/files?index=0&pageSize=50"
        );
    }

    #[test]
    fn unsent_filter_keeps_what_the_server_cannot_check() {
        let one = VersionFilter {
            loaders: vec!["fabric".into()],
            game_versions: vec!["1.21.1".into()],
        };
        assert_eq!(unsent_filter(&one), VersionFilter::default());
        let rest = VersionFilter {
            loaders: vec!["rift".into()],
            game_versions: vec!["1.21".into(), "1.21.1".into()],
        };
        assert_eq!(unsent_filter(&rest), rest);
    }

    #[test]
    fn other_urls() {
        let e = Endpoints::default();
        assert_eq!(e.project(42), "https://api.curseforge.com/v1/mods/42");
        assert_eq!(
            e.file(42, 7),
            "https://api.curseforge.com/v1/mods/42/files/7"
        );
        assert_eq!(
            e.categories(Some(ProjectType::Modpack)),
            "https://api.curseforge.com/v1/categories?gameId=432&classId=4471"
        );
        assert_eq!(
            e.categories(None),
            "https://api.curseforge.com/v1/categories?gameId=432"
        );
        assert_eq!(
            e.fingerprints_url(),
            "https://api.curseforge.com/v1/fingerprints/432"
        );
        assert_eq!(e.game(), "https://api.curseforge.com/v1/games/432");
    }

    #[test]
    fn fingerprint_body() {
        let e = Endpoints::default();
        let body = e.fingerprints_body(&[1, 4_294_967_295]).unwrap();
        assert_eq!(body, br#"{"fingerprints":[1,4294967295]}"#);
        let many = vec![0; MAX_FINGERPRINTS_PER_REQUEST + 1];
        assert!(e.fingerprints_body(&many).is_err());
    }

    #[test]
    fn batch_requests() {
        let e = Endpoints::default();
        assert_eq!(
            e.files_by_ids_url(),
            "https://api.curseforge.com/v1/mods/files"
        );
        assert_eq!(e.mods_by_ids_url(), "https://api.curseforge.com/v1/mods");
        assert_eq!(
            e.files_by_ids_body(&[5_100_200, 7]).unwrap(),
            br#"{"fileIds":[5100200,7]}"#
        );
        assert_eq!(e.mods_by_ids_body(&[1]).unwrap(), br#"{"modIds":[1]}"#);
        let many = vec![1; MAX_IDS_PER_REQUEST + 1];
        assert!(e.files_by_ids_body(&many).is_err());
        assert!(e.mods_by_ids_body(&many).is_err());
    }

    #[test]
    fn classes_round_trip() {
        for kind in ProjectType::SEARCHABLE {
            let class = class_id(kind).unwrap();
            assert_eq!(project_type(u64::from(class)), kind);
        }
        assert_eq!(project_type(17), ProjectType::Other);
    }

    #[test]
    fn numeric_ids() {
        assert_eq!(numeric_id(" 238222 "), Some(238_222));
        assert_eq!(numeric_id("jei"), None);
        assert_eq!(numeric_id("-1"), None);
        assert_eq!(numeric_id(""), None);
        assert_eq!(numeric_id(&"9".repeat(13)), None);
    }
}
