//! The Mojang version manifest (v2): game versions, their kinds and release times.
//!
//! [`VersionManifest::parse`] reads the upstream document and normalizes it; the result is
//! serializable, so the native layer can cache it as is.
//!
//! Damaged entries do not fail the whole manifest. An entry is skipped (and counted in
//! [`VersionManifest::skipped`]) when its `id` is empty or repeats an earlier entry, its `type`
//! is unknown, its `url` is not `https`, or its `sha1` is not 40 hexadecimal digits. The
//! manifest as a whole fails when it is not JSON, is too large, or has no usable `versions`.

use serde::{Deserialize, Serialize};
use vl_core::error::{Error, ErrorKind, Result};
use vl_core::version::{GameVersion, VersionFilter, VersionKind};

/// The largest manifest accepted, in bytes. The real one is about 300 KiB.
pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

/// The newest release and snapshot ids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Latest {
    /// The newest release id.
    pub release: String,
    /// The newest snapshot id.
    pub snapshot: String,
}

/// One version in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// The version id, e.g. `26.3` or `1.12.2`. Not parseable as `1.x.y`: treat as opaque.
    pub id: String,
    /// The kind.
    pub kind: VersionKind,
    /// The URL of this version's JSON (`https`).
    pub url: String,
    /// The SHA-1 of that JSON, 40 lowercase hexadecimal digits.
    pub sha1: String,
    /// When the manifest entry was last changed (RFC 3339), as given.
    pub time: String,
    /// The release time (RFC 3339), as given.
    pub release_time: String,
    /// `complianceLevel` (0 for old versions, 1 for versions with safety features).
    pub compliance_level: u32,
}

/// The normalized version manifest, newest first as upstream orders it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionManifest {
    /// The newest release and snapshot.
    pub latest: Latest,
    /// The usable entries.
    pub versions: Vec<ManifestEntry>,
    /// How many upstream entries were skipped as damaged or unknown.
    pub skipped: usize,
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    latest: Latest,
    versions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawEntry {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
    sha1: String,
    #[serde(default)]
    time: String,
    #[serde(default, rename = "releaseTime")]
    release_time: String,
    #[serde(default, rename = "complianceLevel")]
    compliance_level: u32,
}

/// Whether `s` is a SHA-1 in hexadecimal (40 digits, either case).
pub fn is_sha1(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn entry(raw: serde_json::Value) -> Option<ManifestEntry> {
    let raw: RawEntry = serde_json::from_value(raw).ok()?;
    if raw.id.is_empty() || !raw.url.starts_with("https://") || !is_sha1(&raw.sha1) {
        return None;
    }
    Some(ManifestEntry {
        kind: VersionKind::from_mojang_name(&raw.kind)?,
        id: raw.id,
        url: raw.url,
        sha1: raw.sha1.to_ascii_lowercase(),
        time: raw.time,
        release_time: raw.release_time,
        compliance_level: raw.compliance_level,
    })
}

impl VersionManifest {
    /// Parses and normalizes a `version_manifest_v2.json` document.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Integrity`] when the document is too large, is not a manifest, or contains
    /// no usable version.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::new(
                ErrorKind::Integrity,
                format!("version manifest is larger than {MAX_MANIFEST_BYTES} bytes"),
            ));
        }
        let raw: RawManifest = serde_json::from_slice(bytes).map_err(|e| {
            Error::new(
                ErrorKind::Integrity,
                format!("invalid version manifest: {e}"),
            )
        })?;
        let total = raw.versions.len();
        let mut versions: Vec<ManifestEntry> = Vec::with_capacity(total);
        let mut seen = std::collections::HashSet::new();
        for value in raw.versions {
            match entry(value) {
                Some(e) if seen.insert(e.id.clone()) => versions.push(e),
                _ => {}
            }
        }
        if versions.is_empty() {
            return Err(Error::new(
                ErrorKind::Integrity,
                "version manifest has no usable version",
            ));
        }
        Ok(Self {
            latest: raw.latest,
            skipped: total - versions.len(),
            versions,
        })
    }

    /// The entry with `id`.
    pub fn find(&self, id: &str) -> Option<&ManifestEntry> {
        self.versions.iter().find(|v| v.id == id)
    }

    /// The versions that pass `filter`, in manifest order (newest first).
    pub fn game_versions(&self, filter: VersionFilter) -> Vec<GameVersion> {
        self.versions
            .iter()
            .filter(|v| filter.accepts(v.kind))
            .map(|v| GameVersion {
                id: v.id.clone(),
                kind: v.kind,
                release_time: v.release_time.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/manifest/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    #[test]
    fn parses_v2_manifest() {
        let m = VersionManifest::parse(&fixture("v2.json")).unwrap();
        assert_eq!(m.latest.release, "26.3");
        assert_eq!(m.latest.snapshot, "26.4-snapshot-6");
        assert_eq!(m.versions.len(), 8);
        assert_eq!(m.skipped, 0);
        let e = m.find("26.3").unwrap();
        assert_eq!(e.kind, VersionKind::Release);
        assert_eq!(e.sha1, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(e.compliance_level, 1);
        assert_eq!(m.find("b1.7.3").unwrap().kind, VersionKind::OldBeta);
        assert_eq!(m.find("a1.2.6").unwrap().kind, VersionKind::OldAlpha);
        assert_eq!(m.find("rd-132211").unwrap().compliance_level, 0);
    }

    #[test]
    fn filters_versions() {
        let m = VersionManifest::parse(&fixture("v2.json")).unwrap();
        let ids = |f| -> Vec<String> { m.game_versions(f).into_iter().map(|v| v.id).collect() };
        assert_eq!(ids(VersionFilter::default()), ["26.3", "1.21.11", "1.12.2"]);
        let all = VersionFilter {
            releases: true,
            snapshots: true,
            old: true,
        };
        assert_eq!(ids(all).len(), 8);
        let old = VersionFilter {
            releases: false,
            snapshots: false,
            old: true,
        };
        assert_eq!(ids(old), ["b1.7.3", "a1.2.6", "rd-132211"]);
    }

    #[test]
    fn damaged_entries_are_skipped_not_fatal() {
        let m = VersionManifest::parse(&fixture("damaged.json")).unwrap();
        assert_eq!(m.versions.len(), 2);
        assert_eq!(m.versions[0].id, "1.20.1");
        assert_eq!(m.versions[1].id, "1.19.4");
        assert_eq!(
            m.versions[1].sha1,
            "89abcdef0123456789abcdef0123456789abcdef"
        );
        // bad sha1, non-https url, unknown type, duplicate id, missing field, wrong type, empty id
        assert_eq!(m.skipped, 7);
        assert_eq!(m.latest, Latest::default());
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            b"{\"versions\": 1}",
            b"{\"versions\": []}",
            b"{\"versions\": [1, null]}",
            &[0xff, 0xfe, 0x00],
        ] {
            let e = VersionManifest::parse(bad).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Integrity);
        }
    }

    #[test]
    fn rejects_oversized() {
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert!(VersionManifest::parse(&big).is_err());
    }

    #[test]
    fn normalized_form_round_trips() {
        let m = VersionManifest::parse(&fixture("v2.json")).unwrap();
        let back: VersionManifest =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, back);
    }
}
