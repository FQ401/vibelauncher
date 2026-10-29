//! The release manifest `latest.json` and its detached minisign signature `latest.json.minisig`.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "channel": "stable",
//!   "version": "1.4.0",
//!   "date": "2026-11-02",
//!   "minimum_version": "1.0.0",
//!   "page": "https://example.org/releases/1.4.0",
//!   "artifacts": [
//!     { "os": "windows", "arch": "x86_64", "kind": "msi",
//!       "url": "https://example.org/1.4.0/VibeLauncher-1.4.0-x86_64.msi",
//!       "sha256": "…64 hex digits…", "size": 31457280 }
//!   ]
//! }
//! ```
//!
//! - `minimum_version`: the oldest launcher that may update itself to this release; an older one
//!   is sent to `page`.
//! - `page`: the release page, for installs that update through a package manager or by hand.
//! - `os` and `arch` use the names of [`vl_core::platform`] (`windows`, `mac_os`, `linux`;
//!   `x86_64`, `aarch64`, …); `kind` is an [`ArtifactKind`]. An artifact of an unknown OS,
//!   architecture or kind is skipped, so new ones can be added without breaking older launchers.
//!   Unknown fields are ignored.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vl_core::platform::{Arch, Os, Platform};

use crate::key::UpdateKey;
use crate::{UpdateError, Version};

/// The address of the stable manifest: the `VL_UPDATE_MANIFEST_URL` environment variable at
/// build time, else the `latest.json` asset of the newest release of the project on GitHub. The
/// signature is read from the same address with `.minisig` appended.
pub const MANIFEST_URL: &str = match option_env!("VL_UPDATE_MANIFEST_URL") {
    Some(url) => url,
    None => "https://github.com/FQ401/vibelauncher/releases/latest/download/latest.json",
};

/// The largest manifest accepted.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// The most artifacts a manifest may list.
pub const MAX_ARTIFACTS: usize = 64;
/// The largest artifact a manifest may announce.
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
/// The longest URL accepted.
pub const MAX_URL_LEN: usize = 2048;
/// The manifest format this launcher reads.
pub const SCHEMA: u32 = 1;

/// The release channel of a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Channel {
    /// Releases without a pre-release part.
    Stable,
}

/// What an artifact is and which install method it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A Windows Installer package.
    Msi,
    /// An archive of the launcher folder: zip (Windows) or tar.gz (Linux).
    Portable,
    /// A macOS disk image holding `VibeLauncher.app`.
    Dmg,
    /// A Linux AppImage.
    AppImage,
    /// A Debian package.
    Deb,
    /// An RPM package.
    Rpm,
}

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The platform it runs on.
    pub platform: Platform,
    /// What it is.
    pub kind: ArtifactKind,
    /// Its `https` address.
    pub url: String,
    /// Its SHA-256, 64 lowercase hex digits.
    pub sha256: String,
    /// Its exact size in bytes.
    pub size: u64,
}

/// A verified release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format version ([`SCHEMA`]).
    pub schema: u32,
    /// The channel.
    pub channel: Channel,
    /// The released version.
    pub version: Version,
    /// The release date, `YYYY-MM-DD`.
    pub date: String,
    /// The oldest version that may update itself to this release.
    pub minimum_version: Version,
    /// The `https` release page.
    pub page: String,
    /// The downloadable files, at most one per platform and kind.
    pub artifacts: Vec<Artifact>,
}

impl Manifest {
    /// The artifact of `kind` for `platform`.
    pub fn artifact(&self, platform: Platform, kind: ArtifactKind) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|a| a.platform == platform && a.kind == kind)
    }

    /// Parses manifest bytes that are already verified. Use [`verify_manifest`] for bytes from the
    /// network.
    pub fn parse(bytes: &[u8]) -> Result<Self, UpdateError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(too_large());
        }
        let raw: RawManifest =
            serde_json::from_slice(bytes).map_err(|e| bad(format!("not a manifest: {e}")))?;
        if raw.schema != SCHEMA {
            return Err(UpdateError::UnsupportedSchema(raw.schema));
        }
        let channel = match raw.channel.as_str() {
            "stable" => Channel::Stable,
            other => return Err(bad(format!("unknown channel {other:?}"))),
        };
        let version: Version = raw.version.parse()?;
        let minimum_version: Version = raw.minimum_version.parse()?;
        if channel == Channel::Stable && version.is_prerelease() {
            return Err(bad(format!("pre-release {version} on the stable channel")));
        }
        if minimum_version > version {
            return Err(bad(format!(
                "minimum version {minimum_version} is newer than {version}"
            )));
        }
        if !valid_date(&raw.date) {
            return Err(bad(format!("invalid date {:?}", raw.date)));
        }
        check_url(&raw.page)?;
        if raw.artifacts.len() > MAX_ARTIFACTS {
            return Err(bad(format!("more than {MAX_ARTIFACTS} artifacts")));
        }
        let mut artifacts = Vec::new();
        let mut seen = HashSet::new();
        for raw in raw.artifacts {
            let Some(artifact) = artifact(raw)? else {
                continue;
            };
            if !seen.insert((artifact.platform, artifact.kind)) {
                return Err(bad(format!(
                    "two {:?} artifacts for {:?}",
                    artifact.kind, artifact.platform
                )));
            }
            artifacts.push(artifact);
        }
        Ok(Self {
            schema: raw.schema,
            channel,
            version,
            date: raw.date,
            minimum_version,
            page: raw.page,
            artifacts,
        })
    }
}

/// Verifies `signature` over the exact `manifest` bytes with `key`, then parses them. Nothing is
/// parsed before the signature holds.
pub fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    key: &UpdateKey,
) -> Result<Manifest, UpdateError> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(too_large());
    }
    key.verify(manifest, signature)?;
    Manifest::parse(manifest)
}

#[derive(Deserialize)]
struct RawManifest {
    schema: u32,
    channel: String,
    version: String,
    date: String,
    minimum_version: String,
    page: String,
    artifacts: Vec<RawArtifact>,
}

#[derive(Deserialize)]
struct RawArtifact {
    os: String,
    arch: String,
    kind: String,
    url: String,
    sha256: String,
    size: u64,
}

/// The artifact, `None` when its OS, architecture or kind is unknown to this launcher.
fn artifact(raw: RawArtifact) -> Result<Option<Artifact>, UpdateError> {
    let (Some(os), Some(arch), Some(kind)) = (
        known::<Os>(&raw.os).filter(|o| *o != Os::Other),
        known::<Arch>(&raw.arch).filter(|a| *a != Arch::Other),
        known::<ArtifactKind>(&raw.kind),
    ) else {
        return Ok(None);
    };
    check_url(&raw.url)?;
    let hex = raw.sha256.to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(format!("invalid sha256 for {}", raw.url)));
    }
    if raw.size == 0 || raw.size > MAX_ARTIFACT_BYTES {
        return Err(bad(format!("invalid size {} for {}", raw.size, raw.url)));
    }
    Ok(Some(Artifact {
        platform: Platform { os, arch },
        kind,
        url: raw.url,
        sha256: hex,
        size: raw.size,
    }))
}

fn known<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(Value::String(name.to_owned())).ok()
}

/// An absolute `https` URL with a host, without spaces or control characters.
fn check_url(url: &str) -> Result<(), UpdateError> {
    let host = url
        .strip_prefix("https://")
        .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or_default());
    let ok = url.len() <= MAX_URL_LEN
        && host.is_some_and(|h| !h.is_empty() && !h.contains('@'))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(bad(format!("not an https url: {url:.80}")))
    }
}

/// `YYYY-MM-DD` with a month of 1–12 and a day of 1–31.
fn valid_date(date: &str) -> bool {
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    let num = |s: &str, len: usize| {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    matches!(
        (num(y, 4), num(m, 2), num(d, 2)),
        (Some(_), Some(1..=12), Some(1..=31))
    )
}

fn bad(message: String) -> UpdateError {
    UpdateError::BadManifest(message)
}

fn too_large() -> UpdateError {
    UpdateError::TooLarge {
        what: "manifest",
        limit: MAX_MANIFEST_BYTES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{TestSigner, manifest_json};

    fn edit(json: &str, f: impl FnOnce(&mut Value)) -> String {
        let mut v: Value = serde_json::from_str(json).unwrap();
        f(&mut v);
        v.to_string()
    }

    #[test]
    fn verifies_and_parses_a_signed_manifest() {
        let signer = TestSigner::new();
        let json = manifest_json("1.4.0");
        let sig = signer.sign(json.as_bytes());
        let m = verify_manifest(json.as_bytes(), sig.as_bytes(), &signer.key()).unwrap();
        assert_eq!(m.version, Version::new(1, 4, 0));
        assert_eq!(m.minimum_version, Version::new(0, 1, 0));
        assert_eq!(m.channel, Channel::Stable);
        assert_eq!(m.artifacts.len(), 6);
        let win = Platform {
            os: Os::Windows,
            arch: Arch::X86_64,
        };
        let msi = m.artifact(win, ArtifactKind::Msi).unwrap();
        assert!(msi.url.ends_with(".msi"));
        assert_eq!(msi.size, 1000);
        assert!(m.artifact(win, ArtifactKind::Dmg).is_none());
    }

    #[test]
    fn a_tampered_manifest_is_rejected() {
        let signer = TestSigner::new();
        let json = manifest_json("1.4.0");
        let sig = signer.sign(json.as_bytes());
        // A different download address, hash or version under the old signature.
        for tampered in [
            json.replace("downloads.example.test", "evil.example.test"),
            json.replacen("abab", "abac", 1),
            json.replace("\"1.4.0\"", "\"9.9.9\""),
            format!("{json} "),
        ] {
            assert!(matches!(
                verify_manifest(tampered.as_bytes(), sig.as_bytes(), &signer.key()),
                Err(UpdateError::BadSignature(_))
            ));
        }
    }

    #[test]
    fn a_signature_of_another_key_is_rejected() {
        let json = manifest_json("1.4.0");
        let sig = TestSigner::new().sign(json.as_bytes());
        let result = verify_manifest(json.as_bytes(), sig.as_bytes(), &TestSigner::new().key());
        assert!(matches!(result, Err(UpdateError::BadSignature(_))));
    }

    #[test]
    fn size_is_checked_before_the_signature() {
        let signer = TestSigner::new();
        let big = format!(
            "{}{}",
            manifest_json("1.0.0"),
            " ".repeat(MAX_MANIFEST_BYTES)
        );
        let sig = signer.sign(big.as_bytes());
        assert!(matches!(
            verify_manifest(big.as_bytes(), sig.as_bytes(), &signer.key()),
            Err(UpdateError::TooLarge {
                what: "manifest",
                ..
            })
        ));
    }

    #[test]
    fn a_signed_but_invalid_manifest_is_rejected() {
        let base = manifest_json("1.4.0");
        let cases: Vec<String> = vec![
            "not json".into(),
            edit(&base, |v| v["schema"] = 2.into()),
            edit(&base, |v| v["channel"] = "nightly".into()),
            edit(&base, |v| v["version"] = "1.4.0-beta.1".into()),
            edit(&base, |v| v["version"] = "1.4".into()),
            edit(&base, |v| v["minimum_version"] = "2.0.0".into()),
            edit(&base, |v| v["date"] = "2026-13-01".into()),
            edit(&base, |v| v["date"] = "yesterday".into()),
            edit(&base, |v| v["page"] = "http://example.test/".into()),
            edit(&base, |v| {
                v["artifacts"][0]["url"] = "http://x.test/a".into()
            }),
            edit(&base, |v| v["artifacts"][0]["url"] = "https:///a".into()),
            edit(&base, |v| {
                v["artifacts"][0]["url"] = "https://user@x.test/a".into()
            }),
            edit(&base, |v| {
                v["artifacts"][0]["url"] = "https://x.test/a b".into()
            }),
            edit(&base, |v| {
                v["artifacts"][0]["url"] = format!("https://x.test/{}", "a".repeat(3000)).into()
            }),
            edit(&base, |v| v["artifacts"][0]["sha256"] = "abc".into()),
            edit(&base, |v| {
                v["artifacts"][0]["sha256"] = "zz".repeat(32).into()
            }),
            edit(&base, |v| v["artifacts"][0]["size"] = 0.into()),
            edit(&base, |v| {
                v["artifacts"][0]["size"] = (MAX_ARTIFACT_BYTES + 1).into()
            }),
            edit(&base, |v| {
                let first = v["artifacts"][0].clone();
                v["artifacts"].as_array_mut().unwrap().push(first);
            }),
            edit(&base, |v| {
                let first = v["artifacts"][0].clone();
                v["artifacts"] = Value::Array(vec![first; MAX_ARTIFACTS + 1]);
            }),
            edit(&base, |v| {
                v.as_object_mut().unwrap().remove("page");
            }),
        ];
        let signer = TestSigner::new();
        for case in cases {
            let sig = signer.sign(case.as_bytes());
            let result = verify_manifest(case.as_bytes(), sig.as_bytes(), &signer.key());
            assert!(
                matches!(
                    result,
                    Err(UpdateError::BadManifest(_) | UpdateError::UnsupportedSchema(_))
                ),
                "{case}: {result:?}"
            );
        }
    }

    #[test]
    fn unknown_artifacts_and_fields_are_skipped() {
        let json = edit(&manifest_json("1.4.0"), |v| {
            v["future"] = "field".into();
            let artifacts = v["artifacts"].as_array_mut().unwrap();
            let mut next = artifacts[0].clone();
            next["kind"] = "flatpak".into();
            artifacts.push(next);
            let mut next = artifacts[0].clone();
            next["os"] = "plan9".into();
            artifacts.push(next);
            let mut next = artifacts[0].clone();
            next["arch"] = "other".into();
            artifacts.push(next);
        });
        let m = Manifest::parse(json.as_bytes()).unwrap();
        assert_eq!(m.artifacts.len(), 6);
    }

    #[test]
    fn hashes_are_lowercased() {
        let json = edit(&manifest_json("1.4.0"), |v| {
            v["artifacts"][0]["sha256"] = "AB".repeat(32).into()
        });
        let m = Manifest::parse(json.as_bytes()).unwrap();
        assert_eq!(m.artifacts[0].sha256, "ab".repeat(32));
    }
}
