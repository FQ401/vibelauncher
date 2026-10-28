//! The Modrinth modpack format (`.mrpack`): the `modrinth.index.json` document and the rules
//! that make its paths and download URLs safe.
//!
//! An `.mrpack` is a zip with `modrinth.index.json` at its root, plus optional `overrides/` and
//! `client-overrides/` folders that are copied into the game directory. This module only reads
//! the index from bytes (opening the zip is the native layer's job). The document is hostile
//! input: its size, the number of files, every path and every download URL are checked.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;
use vl_core::{Error, ErrorKind, Result};

/// Name of the index inside the archive.
pub const INDEX_NAME: &str = "modrinth.index.json";
/// Folder copied into the game directory.
pub const OVERRIDES_DIR: &str = "overrides";
/// Folder copied after [`OVERRIDES_DIR`], on top of it.
pub const CLIENT_OVERRIDES_DIR: &str = "client-overrides";
/// Largest accepted index.
pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
/// Most files listed in one index.
pub const MAX_FILES: usize = 20_000;
/// Longest accepted relative path.
pub const MAX_PATH_BYTES: usize = 512;
/// Largest size a listed file may declare.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Hosts a pack may download from (the list Modrinth allows for uploaded packs).
pub const ALLOWED_HOSTS: [&str; 4] = [
    "cdn.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "gitlab.com",
];

const MAX_NAME: usize = 256;
const MAX_DOWNLOADS: usize = 8;
const MAX_URL: usize = 2_048;

/// Whether a file is wanted on a side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Needed.
    Required,
    /// May be left out.
    Optional,
    /// Not for this side.
    Unsupported,
}

impl Support {
    fn parse(text: Option<&str>) -> Self {
        match text {
            Some("optional") => Self::Optional,
            Some("unsupported") => Self::Unsupported,
            _ => Self::Required,
        }
    }
}

/// One file the pack downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    /// Safe relative path in the game directory, `/`-separated.
    pub path: String,
    /// Lowercase hex SHA-1.
    pub sha1: String,
    /// Lowercase hex SHA-512.
    pub sha512: String,
    /// Support on the client (a missing `env` is required).
    pub client: Support,
    /// Support on the server.
    pub server: Support,
    /// Allowed `https` URLs, in the pack's order.
    pub downloads: Vec<String>,
    /// Size in bytes.
    pub size: u64,
}

/// A mod loader a pack asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackLoader {
    /// `fabric-loader`.
    Fabric,
    /// `quilt-loader`.
    Quilt,
    /// `forge`.
    Forge,
    /// `neoforge`.
    NeoForge,
}

impl PackLoader {
    /// The dependency key in the index.
    pub fn key(self) -> &'static str {
        match self {
            Self::Fabric => "fabric-loader",
            Self::Quilt => "quilt-loader",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    /// The name shown to the user.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        }
    }
}

/// A parsed `modrinth.index.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackIndex {
    /// The pack's own version label.
    pub version_id: String,
    /// The pack's name.
    pub name: String,
    /// A short description.
    pub summary: Option<String>,
    /// Files to download.
    pub files: Vec<PackFile>,
    /// Dependency key to version (`minecraft`, `fabric-loader`, ...).
    pub dependencies: BTreeMap<String, String>,
}

impl PackIndex {
    /// The Minecraft version the pack runs on.
    pub fn game_version(&self) -> Option<&str> {
        self.dependencies.get("minecraft").map(String::as_str)
    }

    /// The loader the pack asks for with its version; the first of Fabric, Quilt, NeoForge and
    /// Forge that is listed.
    pub fn loader(&self) -> Option<(PackLoader, &str)> {
        [
            PackLoader::Fabric,
            PackLoader::Quilt,
            PackLoader::NeoForge,
            PackLoader::Forge,
        ]
        .into_iter()
        .find_map(|l| self.dependencies.get(l.key()).map(|v| (l, v.as_str())))
    }
}

fn bad(why: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::Integrity,
        format!("unusable {INDEX_NAME}: {why}"),
    )
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(rename = "formatVersion")]
    format_version: u64,
    game: String,
    #[serde(rename = "versionId", default)]
    version_id: String,
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<Value>,
    #[serde(default)]
    dependencies: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct RawFile {
    path: String,
    hashes: BTreeMap<String, String>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
    downloads: Vec<String>,
    #[serde(rename = "fileSize")]
    file_size: u64,
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parses the index.
///
/// # Errors
///
/// `Integrity` for a document that is too large, is not JSON, lacks a required field, lists
/// too many files, or has a file with an unsafe or repeated path, no usable hash, a size over
/// [`MAX_FILE_BYTES`], or no download URL on an allowed host; `Unsupported` for another
/// `formatVersion` or `game`.
pub fn parse_index(bytes: &[u8]) -> Result<PackIndex> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(bad(format!("larger than {MAX_INDEX_BYTES} bytes")));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(bad)?;
    let raw: RawIndex = serde_json::from_value(value).map_err(bad)?;
    if raw.format_version != 1 {
        return Err(Error::unsupported(format!(
            "modpack format version {}",
            raw.format_version
        )));
    }
    if raw.game != "minecraft" {
        return Err(Error::unsupported(format!(
            "modpack for game {:?}",
            raw.game
        )));
    }
    if raw.files.len() > MAX_FILES {
        return Err(bad(format!("more than {MAX_FILES} files")));
    }
    let name = cut(raw.name.trim(), MAX_NAME);
    if name.is_empty() {
        return Err(bad("the pack has no name"));
    }
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(raw.files.len());
    for value in raw.files {
        let file: RawFile = serde_json::from_value(value).map_err(bad)?;
        let path =
            safe_path(&file.path).ok_or_else(|| bad(format!("unsafe path {:?}", file.path)))?;
        if !seen.insert(path.to_ascii_lowercase()) {
            return Err(bad(format!("the path {path} is listed twice")));
        }
        let hash = |key: &str, len: usize| {
            file.hashes
                .get(key)
                .filter(|h| is_hex(h, len))
                .map(|h| h.to_ascii_lowercase())
                .ok_or_else(|| bad(format!("{path} has no valid {key} hash")))
        };
        let sha1 = hash("sha1", 40)?;
        let sha512 = hash("sha512", 128)?;
        if file.file_size > MAX_FILE_BYTES {
            return Err(bad(format!("{path} is larger than {MAX_FILE_BYTES} bytes")));
        }
        let downloads: Vec<String> = file
            .downloads
            .into_iter()
            .filter(|u| is_allowed_download(u))
            .take(MAX_DOWNLOADS)
            .collect();
        if downloads.is_empty() {
            return Err(bad(format!(
                "{path} has no https download from an allowed host ({})",
                ALLOWED_HOSTS.join(", ")
            )));
        }
        let side = |key: &str| {
            Support::parse(
                file.env
                    .as_ref()
                    .and_then(|e| e.get(key))
                    .map(String::as_str),
            )
        };
        files.push(PackFile {
            path,
            sha1,
            sha512,
            client: side("client"),
            server: side("server"),
            downloads,
            size: file.file_size,
        });
    }
    let dependencies = raw
        .dependencies
        .into_iter()
        .filter_map(|(k, v)| Some((cut(&k, 64), cut(v.as_str()?, 128))))
        .collect();
    Ok(PackIndex {
        version_id: cut(&raw.version_id, 128),
        name,
        summary: raw
            .summary
            .map(|s| cut(&s, 2_048))
            .filter(|s| !s.trim().is_empty()),
        files,
        dependencies,
    })
}

fn is_reserved_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let numbered = (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.as_bytes().get(3), Some(b'1'..=b'9'));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered
}

/// Checks a path from the pack (an index entry or a name inside `overrides/`) and returns it.
/// Accepted: relative, `/`-separated, made of plain names. Refused: empty, absolute,
/// drive-qualified or backslashed paths, `.` and `..` components, empty components, control
/// characters, `:`, names Windows cannot hold (trailing dot or space, `CON`, `NUL`, ...), paths
/// longer than [`MAX_PATH_BYTES`], and anything inside the launcher's `.vibelauncher` folder.
pub fn safe_path(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return None;
    }
    for (n, part) in path.split('/').enumerate() {
        let ok = !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
            && !part.chars().any(char::is_control)
            && !part.ends_with(['.', ' '])
            && !is_reserved_name(part)
            && !(n == 0 && part.eq_ignore_ascii_case(".vibelauncher"));
        if !ok {
            return None;
        }
    }
    Some(path.to_owned())
}

/// Whether a download URL is `https` on one of [`ALLOWED_HOSTS`] (no user info, no port).
pub fn is_allowed_download(url: &str) -> bool {
    if url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c == ' ' || c == '\\') {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.to_ascii_lowercase();
    ALLOWED_HOSTS.contains(&host.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(files: &str) -> String {
        format!(
            r#"{{"formatVersion":1,"game":"minecraft","versionId":"1.0","name":"Pack","summary":"s",
"files":[{files}],"dependencies":{{"minecraft":"1.21.1","fabric-loader":"0.16.0","other":"x"}}}}"#
        )
    }

    fn file(path: &str, url: &str) -> String {
        format!(
            r#"{{"path":"{path}","hashes":{{"sha1":"{}","sha512":"{}"}},"env":{{"client":"optional","server":"unsupported"}},"downloads":["{url}"],"fileSize":10}}"#,
            "a".repeat(40),
            "B".repeat(128)
        )
    }

    #[test]
    fn parses_a_pack() {
        let text = index(&file("mods/a.jar", "https://cdn.modrinth.com/data/x/a.jar"));
        let p = parse_index(text.as_bytes()).unwrap();
        assert_eq!(p.name, "Pack");
        assert_eq!(p.game_version(), Some("1.21.1"));
        assert_eq!(p.loader(), Some((PackLoader::Fabric, "0.16.0")));
        assert_eq!(p.files[0].client, Support::Optional);
        assert_eq!(p.files[0].server, Support::Unsupported);
        assert_eq!(p.files[0].sha512, "b".repeat(128));
    }

    #[test]
    fn refuses_bad_documents() {
        let ok = file("mods/a.jar", "https://cdn.modrinth.com/a.jar");
        assert!(parse_index(b"[]").is_err());
        let v2 = index(&ok).replace("\"formatVersion\":1", "\"formatVersion\":2");
        assert!(parse_index(v2.as_bytes()).is_err());
        let game = index(&ok).replace("\"minecraft\",", "\"terraria\",");
        assert!(parse_index(game.as_bytes()).is_err());
        let dup = format!(
            "{ok},{}",
            file("MODS/a.jar", "https://cdn.modrinth.com/b.jar")
        );
        assert!(parse_index(index(&dup).as_bytes()).is_err());
        for path in [
            "../x.jar",
            "/etc/x",
            "mods/../../x",
            "C:/x",
            "a\\\\b",
            "mods//x",
            ".vibelauncher/a",
        ] {
            let text = index(&file(path, "https://cdn.modrinth.com/a.jar"));
            assert!(parse_index(text.as_bytes()).is_err(), "{path}");
        }
        for url in [
            "http://cdn.modrinth.com/a.jar",
            "https://evil.example/a.jar",
            "https://cdn.modrinth.com@evil.example/a.jar",
            "https://cdn.modrinth.com.evil.example/a.jar",
            "https://cdn.modrinth.com:8443/a.jar",
        ] {
            let text = index(&file("mods/a.jar", url));
            assert!(parse_index(text.as_bytes()).is_err(), "{url}");
        }
        let nohash = ok.replace(&"a".repeat(40), "zz");
        assert!(parse_index(index(&nohash).as_bytes()).is_err());
        assert!(parse_index(&vec![b' '; MAX_INDEX_BYTES + 1]).is_err());
    }

    #[test]
    fn safe_paths() {
        assert_eq!(safe_path("config/a.toml").as_deref(), Some("config/a.toml"));
        for bad in [
            "", "/a", "a/", "a/./b", "a/..", "a\\b", "x:y", "aux.txt", "a/nul", "a.", "a/b ",
            "a\0b",
        ] {
            assert!(safe_path(bad).is_none(), "{bad:?}");
        }
    }
}
