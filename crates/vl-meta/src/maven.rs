//! Maven repository metadata (`maven-metadata.xml`) of the loaders published as Maven artifacts
//! (NeoForge, Forge).

use vl_core::error::{Error, ErrorKind, Result};

use crate::loader::MAX_LOADER_VERSIONS;

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// The largest `maven-metadata.xml` accepted.
pub const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// The `<version>` entries of a Maven `maven-metadata.xml`, in document order.
///
/// # Errors
///
/// `Integrity` for a document that is too large, not UTF-8, has no `<versions>` list or too many
/// entries.
pub fn parse_maven_versions(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(damaged("maven metadata is too large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| damaged("maven metadata is not UTF-8"))?;
    let list =
        section(text, "versions").ok_or_else(|| damaged("maven metadata has no versions"))?;
    let mut out = Vec::new();
    let mut rest = list;
    while let Some(start) = rest.find("<version>") {
        let after = &rest[start + "<version>".len()..];
        let end = after
            .find("</version>")
            .ok_or_else(|| damaged("maven metadata has an unclosed version"))?;
        let version = after[..end].trim();
        if !version.is_empty() {
            out.push(version.to_owned());
        }
        if out.len() > MAX_LOADER_VERSIONS {
            return Err(damaged("maven metadata has too many versions"));
        }
        rest = &after[end + "</version>".len()..];
    }
    Ok(out)
}

fn section<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    text.get(start..end)
}
