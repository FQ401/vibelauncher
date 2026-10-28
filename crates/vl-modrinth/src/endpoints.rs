//! Request URLs and bodies of the Modrinth API.

use vl_core::catalog::{HashAlgorithm, ProjectType, SearchQuery, VersionFilter};
use vl_core::{Error, Result};

/// The public API, version 2.
pub const DEFAULT_BASE: &str = "https://api.modrinth.com/v2";

/// The most hashes one `/version_files` request carries.
pub const MAX_HASHES_PER_REQUEST: usize = 500;

/// The longest project id or slug accepted.
const MAX_ID_LEN: usize = 128;

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

    /// `GET /search`.
    pub fn search(&self, query: &SearchQuery) -> Result<String> {
        query.validate()?;
        let mut params = Vec::new();
        if !query.query.trim().is_empty() {
            params.push(format!("query={}", encode(query.query.trim())));
        }
        let facets = facets(query);
        if !facets.is_empty() {
            params.push(format!("facets={}", encode(&json(&facets)?)));
        }
        params.push(format!("index={}", query.sort.name()));
        params.push(format!("offset={}", query.offset));
        params.push(format!("limit={}", query.limit));
        Ok(format!("{}/search?{}", self.base, params.join("&")))
    }

    /// `GET /project/{id|slug}`.
    pub fn project(&self, id: &str) -> Result<String> {
        Ok(format!("{}/project/{}", self.base, id_segment(id)?))
    }

    /// `GET /project/{id|slug}/version` with the filter.
    pub fn versions(&self, project: &str, filter: &VersionFilter) -> Result<String> {
        let mut params = vec!["include_changelog=false".to_string()];
        for (name, values) in [
            ("loaders", &filter.loaders),
            ("game_versions", &filter.game_versions),
        ] {
            let values = clean(values);
            if !values.is_empty() {
                params.push(format!("{name}={}", encode(&json(&values)?)));
            }
        }
        Ok(format!(
            "{}/project/{}/version?{}",
            self.base,
            id_segment(project)?,
            params.join("&")
        ))
    }

    /// `GET /version_file/{hash}`.
    pub fn version_by_hash(&self, hash: &str, algorithm: HashAlgorithm) -> Result<String> {
        check_hash(hash, algorithm)?;
        Ok(format!(
            "{}/version_file/{}?algorithm={}",
            self.base,
            hash.to_ascii_lowercase(),
            algorithm_name(algorithm)
        ))
    }

    /// `POST /version_files`: the URL.
    pub fn versions_by_hashes_url(&self) -> String {
        format!("{}/version_files", self.base)
    }

    /// `POST /version_files`: the JSON body for at most [`MAX_HASHES_PER_REQUEST`] hashes.
    pub fn versions_by_hashes_body(
        &self,
        hashes: &[String],
        algorithm: HashAlgorithm,
    ) -> Result<Vec<u8>> {
        if hashes.len() > MAX_HASHES_PER_REQUEST {
            return Err(Error::invalid_input(format!(
                "at most {MAX_HASHES_PER_REQUEST} hashes per request"
            )));
        }
        let mut lower = Vec::with_capacity(hashes.len());
        for hash in hashes {
            check_hash(hash, algorithm)?;
            lower.push(hash.to_ascii_lowercase());
        }
        serde_json::to_vec(&serde_json::json!({
            "hashes": lower,
            "algorithm": algorithm_name(algorithm),
        }))
        .map_err(|e| Error::internal(e.to_string()))
    }
}

fn algorithm_name(algorithm: HashAlgorithm) -> &'static str {
    match algorithm {
        HashAlgorithm::Sha512 => "sha512",
        _ => "sha1",
    }
}

/// The facet groups of a search: groups are ANDed, entries inside a group are ORed.
fn facets(query: &SearchQuery) -> Vec<Vec<String>> {
    let mut groups = Vec::new();
    if let Some(kind) = query.project_type
        && kind != ProjectType::Other
    {
        groups.push(vec![format!("project_type:{}", kind.name())]);
    }
    if let Some(version) = query.game_version.as_deref() {
        groups.push(vec![format!("versions:{}", version.trim())]);
    }
    if let Some(loader) = query.loader.as_deref() {
        groups.push(vec![format!("categories:{}", loader.trim().to_lowercase())]);
    }
    for category in &query.categories {
        groups.push(vec![format!(
            "categories:{}",
            category.trim().to_lowercase()
        )]);
    }
    groups
}

fn clean(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect()
}

fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::internal(e.to_string()))
}

fn check_hash(hash: &str, algorithm: HashAlgorithm) -> Result<()> {
    if algorithm.is_valid(hash) {
        Ok(())
    } else {
        Err(Error::invalid_input("not a hex digest of the right length"))
    }
}

/// A project id or slug as one path segment.
fn id_segment(id: &str) -> Result<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(Error::invalid_input(format!(
            "a project id is 1 to {MAX_ID_LEN} characters"
        )));
    }
    Ok(encode(id))
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn encode(text: &str) -> String {
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
    use vl_core::catalog::SearchSort;

    /// Decodes `%XX` escapes (test helper; input is ASCII).
    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok());
            match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
                (b'%', Some(b)) => {
                    out.push(b);
                    i += 3;
                }
                (b, _) => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn search_url_carries_facets_sort_and_paging() {
        let q = SearchQuery {
            query: "fast & light".into(),
            project_type: Some(ProjectType::Mod),
            game_version: Some("1.21.4".into()),
            loader: Some("Fabric".into()),
            categories: vec!["optimization".into()],
            sort: SearchSort::Downloads,
            offset: 40,
            limit: 20,
            ..SearchQuery::default()
        };
        let url = Endpoints::default().search(&q).unwrap_or_default();
        assert!(url.starts_with("https://api.modrinth.com/v2/search?query=fast%20%26%20light&"));
        assert!(url.contains("index=downloads&offset=40&limit=20"), "{url}");
        let facets = url
            .split('&')
            .find_map(|p| p.strip_prefix("facets="))
            .map(decode)
            .unwrap_or_default();
        assert_eq!(
            facets,
            r#"[["project_type:mod"],["versions:1.21.4"],["categories:fabric"],["categories:optimization"]]"#
        );
    }

    #[test]
    fn empty_search_has_no_query_or_facets() {
        let url = Endpoints::default().search(&SearchQuery::default());
        assert_eq!(
            url.as_deref().ok(),
            Some("https://api.modrinth.com/v2/search?index=relevance&offset=0&limit=20")
        );
    }

    #[test]
    fn invalid_queries_are_refused() {
        let mut q = SearchQuery::text("x");
        q.limit = 101;
        assert!(Endpoints::default().search(&q).is_err());
    }

    #[test]
    fn project_ids_are_one_encoded_segment() {
        let e = Endpoints::new("http://localhost:1/v2/");
        assert_eq!(
            e.project("sodium").as_deref().ok(),
            Some("http://localhost:1/v2/project/sodium")
        );
        assert_eq!(
            e.project("a/../b").as_deref().ok(),
            Some("http://localhost:1/v2/project/a%2F..%2Fb")
        );
        assert!(e.project("  ").is_err());
        assert!(e.project(&"x".repeat(200)).is_err());
    }

    #[test]
    fn versions_url_filters() {
        let f = VersionFilter {
            loaders: vec!["fabric".into(), " ".into()],
            game_versions: vec!["1.21.4".into()],
        };
        let url = Endpoints::default().versions("abc", &f).unwrap_or_default();
        assert_eq!(
            decode(&url),
            r#"https://api.modrinth.com/v2/project/abc/version?include_changelog=false&loaders=["fabric"]&game_versions=["1.21.4"]"#
        );
        let bare = Endpoints::default().versions("abc", &VersionFilter::default());
        assert_eq!(
            bare.as_deref().ok(),
            Some("https://api.modrinth.com/v2/project/abc/version?include_changelog=false")
        );
    }

    #[test]
    fn hash_requests_check_the_digest() {
        let e = Endpoints::default();
        let sha1 = "AB".repeat(20);
        let url = e
            .version_by_hash(&sha1, HashAlgorithm::Sha1)
            .unwrap_or_default();
        assert!(url.ends_with(&format!("/version_file/{}?algorithm=sha1", "ab".repeat(20))));
        assert!(e.version_by_hash("zz", HashAlgorithm::Sha1).is_err());
        assert!(e.version_by_hash(&sha1, HashAlgorithm::Sha512).is_err());

        let body = e
            .versions_by_hashes_body(&[sha1], HashAlgorithm::Sha1)
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&body).into_owned();
        assert!(text.contains(r#""algorithm":"sha1""#) && text.contains(&"ab".repeat(20)));
        let many = vec!["a".repeat(40); MAX_HASHES_PER_REQUEST + 1];
        assert!(
            e.versions_by_hashes_body(&many, HashAlgorithm::Sha1)
                .is_err()
        );
    }
}
