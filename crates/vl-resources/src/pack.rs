//! Reading `pack.mcmeta` of a resource pack or data pack.
//!
//! Packs are untrusted: only the one small file is read, its size is capped, and anything
//! unexpected gives a problem text instead of an error.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use vl_core::resources::PackMetadata;
use zip::ZipArchive;

/// Largest `pack.mcmeta` that is read.
const MAX_BYTES: u64 = 256 * 1024;
/// Longest description kept, in characters.
const MAX_DESCRIPTION: usize = 300;
/// Deepest text component followed.
const MAX_DEPTH: usize = 8;

/// Reads the `pack.mcmeta` of the pack at `path`: a zip file or an unpacked folder.
///
/// # Errors
///
/// A short text saying why the metadata could not be read.
pub(crate) fn read_pack(path: &Path, is_dir: bool) -> Result<PackMetadata, String> {
    let mut bytes = Vec::new();
    if is_dir {
        File::open(path.join("pack.mcmeta"))
            .map_err(|e| format!("no readable pack.mcmeta: {e}"))?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("pack.mcmeta: {e}"))?;
    } else {
        let file = File::open(path).map_err(|e| format!("cannot open: {e}"))?;
        let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable zip: {e}"))?;
        let mut entry = archive
            .by_name("pack.mcmeta")
            .map_err(|_| "no pack.mcmeta in the archive".to_owned())?;
        entry
            .by_ref()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("pack.mcmeta: {e}"))?;
    }
    if bytes.len() as u64 > MAX_BYTES {
        return Err("pack.mcmeta is too large".into());
    }
    parse(&bytes)
}

/// Parses the contents of a `pack.mcmeta`.
pub(crate) fn parse(bytes: &[u8]) -> Result<PackMetadata, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let document: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("pack.mcmeta is not valid JSON: {e}"))?;
    let pack = document
        .get("pack")
        .and_then(Value::as_object)
        .ok_or_else(|| "pack.mcmeta has no \"pack\" section".to_owned())?;
    let mut meta = PackMetadata {
        pack_format: pack.get("pack_format").and_then(number),
        min_format: pack.get("min_format").and_then(number),
        max_format: pack.get("max_format").and_then(number),
        description: pack
            .get("description")
            .map(|d| {
                let mut text = String::new();
                component_text(d, &mut text, 0);
                text.chars().take(MAX_DESCRIPTION).collect::<String>()
            })
            .filter(|t| !t.trim().is_empty()),
    };
    // `supported_formats`: a number, `[min, max]` or `{min_inclusive, max_inclusive}`.
    if let Some(range) = pack.get("supported_formats") {
        let (low, high) = match range {
            Value::Array(a) => (a.first().and_then(number), a.get(1).and_then(number)),
            Value::Object(o) => (
                o.get("min_inclusive").and_then(number),
                o.get("max_inclusive").and_then(number),
            ),
            other => (number(other), number(other)),
        };
        meta.min_format = meta.min_format.or(low);
        meta.max_format = meta.max_format.or(high);
    }
    Ok(meta)
}

/// A format number: an integer, or the major part of `[major, minor]`.
fn number(value: &Value) -> Option<u32> {
    match value {
        Value::Array(a) => a.first().and_then(number),
        other => other.as_u64().and_then(|n| u32::try_from(n).ok()),
    }
}

/// The plain text of a JSON text component, without formatting.
fn component_text(value: &Value, out: &mut String, depth: usize) {
    if depth > MAX_DEPTH || out.chars().count() > MAX_DESCRIPTION {
        return;
    }
    match value {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => {
            for item in items {
                component_text(item, out, depth + 1);
            }
        }
        Value::Object(o) => {
            if let Some(Value::String(s)) = o.get("text") {
                out.push_str(s);
            }
            if let Some(extra) = o.get("extra") {
                component_text(extra, out, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_formats_and_text_components() {
        let meta =
            parse(br#"{"pack":{"pack_format":48,"description":[{"text":"Hello "},"world"]}}"#)
                .unwrap();
        assert_eq!(meta.pack_format, Some(48));
        assert_eq!(meta.description.as_deref(), Some("Hello world"));
        let meta = parse(
            br#"{"pack":{"min_format":[94,1],"max_format":[94,1],"description":{"text":"x"}}}"#,
        )
        .unwrap();
        assert_eq!((meta.min_format, meta.max_format), (Some(94), Some(94)));
        let meta = parse(br#"{"pack":{"pack_format":15,"supported_formats":[10,20]}}"#).unwrap();
        assert_eq!((meta.min_format, meta.max_format), (Some(10), Some(20)));
    }

    #[test]
    fn rejects_what_is_not_a_pack() {
        assert!(parse(b"nope").is_err());
        assert!(parse(br#"{"x":1}"#).is_err());
    }
}
