//! The meta servers of Fabric and Quilt, which share one API shape:
//!
//! ```text
//! <base>/versions/loader/<game>                          loader versions for a game version
//! <base>/versions/loader/<game>/<loader>/profile/json    ready launch profile
//! ```
//!
//! The sources differ only in their base URL ([`crate::fabric`], [`crate::quilt`]).

/// The URLs of one meta server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    base: String,
}

impl Endpoints {
    /// The endpoints below `base` (for example `https://meta.fabricmc.net/v2`).
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    /// The URL of the loader versions that support `game`.
    pub fn versions_url(&self, game: &str) -> String {
        format!("{}/versions/loader/{}", self.base, encode(game))
    }

    /// The URL of the launch profile for a game and loader version.
    pub fn profile_url(&self, game: &str, loader: &str) -> String {
        format!(
            "{}/versions/loader/{}/{}/profile/json",
            self.base,
            encode(game),
            encode(loader)
        )
    }
}

/// Percent-encodes a path segment: everything but unreserved characters.
fn encode(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
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
    fn builds_urls_and_encodes_segments() {
        let e = Endpoints::new("https://meta.example.test/v2/");
        assert_eq!(
            e.versions_url("1.21.1"),
            "https://meta.example.test/v2/versions/loader/1.21.1"
        );
        assert_eq!(
            e.profile_url("1.14 Pre-Release 5", "0.16.0"),
            "https://meta.example.test/v2/versions/loader/1.14%20Pre-Release%205/0.16.0/profile/json"
        );
        assert!(!e.versions_url("a?b#c").contains(['?', '#']));
    }
}
