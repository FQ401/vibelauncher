//! NeoForge: the version list from the Maven metadata and the launch profile of an installer.
//!
//! NeoForge publishes every version as `net.neoforged:neoforge` in its Maven repository; the
//! `maven-metadata.xml` lists them all, in publishing order, with no game version attached. The
//! game version follows from the NeoForge version:
//!
//! | NeoForge | Minecraft |
//! |---|---|
//! | `<minor>.<patch>.<build>` (`21.1.77`, `20.2.3-beta`) | `1.<minor>.<patch>`, or `1.<minor>` when `patch` is 0 |
//! | `<year>.<drop>.<hotfix>.<build>` (`26.1.2.12`) | `<year>.<drop>.<hotfix>`, or `<year>.<drop>` when `hotfix` is 0 |
//!
//! A `-beta` or `-alpha` suffix marks an unstable build of a released game version; a build
//! metadata suffix (`26.1.0.0-alpha.1+snapshot-1`) marks a build for a game snapshot, and such
//! versions and names that fit neither scheme (April Fools' builds) are left out.
//!
//! Minecraft 1.20.1 is the exception: NeoForge's builds for it were published as
//! `net.neoforged:forge`, versions `1.20.1-47.1.<build>`, with the same installer format. They
//! are listed as NeoForge versions of 1.20.1 in the short form (`47.1.106`), merged with the
//! `net.neoforged:neoforge` list ([`versions_for_game_with_legacy`]).
//!
//! The launch profile is the installer's `version.json`, read with
//! [`launch_profile`].

use std::cmp::Ordering;

use vl_core::component::ComponentKind;
use vl_core::error::{Error, Result};

use crate::install_profile::launch_profile;
use crate::loader::{LoaderProfile, LoaderVersion, LoaderVersions};

pub use crate::maven::{MAX_METADATA_BYTES, parse_maven_versions};

/// The only game version whose NeoForge builds are published as `net.neoforged:forge`.
pub const LEGACY_GAME: &str = "1.20.1";

/// Whether `game` has NeoForge builds in the `net.neoforged:forge` artifact.
pub fn is_legacy_game(game: &str) -> bool {
    game == LEGACY_GAME
}

/// The NeoForge version of `game` without the game prefix: `1.20.1-47.1.106` gives `47.1.106`.
/// Any other text is returned as it is.
pub fn short_version<'a>(game: &str, version: &'a str) -> &'a str {
    if is_legacy_game(game)
        && let Some(rest) = version.strip_prefix(game).and_then(|r| r.strip_prefix('-'))
    {
        return rest;
    }
    version
}

/// The versions of the `net.neoforged:forge` list (`1.20.1-47.1.106`) for `game`, in the short
/// form. Empty for a game version other than [`LEGACY_GAME`].
fn legacy_parsed(game: &str, all: &[String]) -> Vec<(Parsed, String)> {
    if !is_legacy_game(game) {
        return Vec::new();
    }
    all.iter()
        .filter_map(|v| {
            let short = v.strip_prefix(game)?.strip_prefix('-')?;
            let p = parse(short)?;
            (p.numbers.len() == 3 && p.numbers[0] == 47).then(|| (p, short.to_owned()))
        })
        .collect()
}

/// A NeoForge version split into its numbers and its suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    numbers: Vec<u64>,
    stable: bool,
}

fn parse(version: &str) -> Option<Parsed> {
    if version.contains('+') {
        return None;
    }
    let (core, suffix) = match version.split_once('-') {
        Some((c, s)) => (c, Some(s)),
        None => (version, None),
    };
    if suffix.is_some_and(|s| {
        s.is_empty()
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    }) {
        return None;
    }
    let numbers = core
        .split('.')
        .map(|p| {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                p.parse().ok()
            }
        })
        .collect::<Option<Vec<u64>>>()?;
    Some(Parsed {
        numbers,
        stable: suffix.is_none(),
    })
}

/// The game version a NeoForge version is for, or `None` for a snapshot build or a name that
/// fits no scheme (see the module documentation).
pub fn game_version(neoforge: &str) -> Option<String> {
    let p = parse(neoforge)?;
    match p.numbers.as_slice() {
        [minor, patch, _build] if (20..26).contains(minor) => Some(if *patch == 0 {
            format!("1.{minor}")
        } else {
            format!("1.{minor}.{patch}")
        }),
        [year, drop, hotfix, _build] if *year >= 26 => Some(if *hotfix == 0 {
            format!("{year}.{drop}")
        } else {
            format!("{year}.{drop}.{hotfix}")
        }),
        _ => None,
    }
}

fn compare(a: &Parsed, b: &Parsed) -> Ordering {
    a.numbers
        .cmp(&b.numbers)
        .then_with(|| a.stable.cmp(&b.stable))
}

/// The NeoForge versions for `game` out of the full list, newest first.
///
/// # Errors
///
/// `Integrity` for a repeated version.
pub fn versions_for_game(game: &str, all: &[String]) -> Result<LoaderVersions> {
    versions_for_game_with_legacy(game, all, &[])
}

/// Like [`versions_for_game`], with the `net.neoforged:forge` list (`legacy`, full Maven
/// versions) merged in for 1.20.1 (see the module documentation).
///
/// # Errors
///
/// `Integrity` for a repeated version.
pub fn versions_for_game_with_legacy(
    game: &str,
    all: &[String],
    legacy: &[String],
) -> Result<LoaderVersions> {
    let mut found: Vec<(Parsed, String)> = all
        .iter()
        .filter(|v| game_version(v).as_deref() == Some(game))
        .filter_map(|v| parse(v).map(|p| (p, v.clone())))
        .collect();
    found.extend(legacy_parsed(game, legacy));
    found.sort_by(|a, b| compare(&b.0, &a.0));
    let versions = found
        .into_iter()
        .map(|(p, version)| LoaderVersion {
            version,
            stable: p.stable,
            ..LoaderVersion::default()
        })
        .collect();
    LoaderVersions::new(ComponentKind::NeoForge, game, versions)
}

/// The repository path of a NeoForge installer, relative to the Maven root.
///
/// # Errors
///
/// `InvalidInput` for a version that cannot be a path part.
pub fn installer_path(version: &str) -> Result<String> {
    if parse(version).is_none() {
        return Err(Error::invalid_input(format!(
            "invalid NeoForge version {version:?}"
        )));
    }
    Ok(format!(
        "net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar"
    ))
}

/// The repository path of the installer of NeoForge `version` for `game`: the
/// `net.neoforged:forge` artifact for 1.20.1 (`version` in either form), else
/// [`installer_path`].
///
/// # Errors
///
/// `InvalidInput` for a version that cannot be a path part.
pub fn installer_path_for(game: &str, version: &str) -> Result<String> {
    if !is_legacy_game(game) {
        return installer_path(version);
    }
    let short = short_version(game, version);
    if parse(short).is_none_or(|p| p.numbers.len() != 3) {
        return Err(Error::invalid_input(format!(
            "invalid NeoForge version {version:?}"
        )));
    }
    let full = format!("{game}-{short}");
    Ok(format!(
        "net/neoforged/forge/{full}/forge-{full}-installer.jar"
    ))
}

/// The launch profile of a NeoForge version from the installer's `version.json`.
///
/// # Errors
///
/// `Integrity` for a damaged document or one for another game version.
pub fn profile(game: &str, loader_version: &str, version_json: &[u8]) -> Result<LoaderProfile> {
    launch_profile(ComponentKind::NeoForge, game, loader_version, version_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_versions_to_the_game() {
        let cases = [
            ("21.1.77", Some("1.21.1")),
            ("21.0.167", Some("1.21")),
            ("20.2.3-beta", Some("1.20.2")),
            ("21.4.161", Some("1.21.4")),
            ("21.11.48", Some("1.21.11")),
            ("26.1.2.124", Some("26.1.2")),
            ("26.3.0.99", Some("26.3")),
            ("26.1.0.1-beta", Some("26.1")),
            ("26.1.0.0-alpha.1+snapshot-1", None),
            ("0.25w14craftmine.3-beta", None),
            ("47.1.3", None),
            ("21.1", None),
            ("", None),
        ];
        for (v, game) in cases {
            assert_eq!(game_version(v).as_deref(), game, "{v}");
        }
    }

    #[test]
    fn merges_the_legacy_artifact_for_1_20_1() {
        let legacy: Vec<String> = [
            "1.20.1-47.1.9",
            "1.20.1-47.1.106",
            "1.20.1-47.1.5",
            "47.1.82",
        ]
        .map(String::from)
        .into();
        let list = versions_for_game_with_legacy("1.20.1", &[], &legacy).unwrap();
        let names: Vec<_> = list.versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(names, ["47.1.106", "47.1.9", "47.1.5"]);
        assert!(list.versions.iter().all(|v| v.stable));
        let other = versions_for_game_with_legacy("1.21.1", &[], &legacy).unwrap();
        assert!(other.versions.is_empty());
        assert_eq!(short_version("1.20.1", "1.20.1-47.1.9"), "47.1.9");
        assert_eq!(short_version("1.21.1", "1.20.1-47.1.9"), "1.20.1-47.1.9");
        assert_eq!(
            installer_path_for("1.20.1", "47.1.9").unwrap(),
            "net/neoforged/forge/1.20.1-47.1.9/forge-1.20.1-47.1.9-installer.jar"
        );
        assert_eq!(
            installer_path_for("1.20.1", "1.20.1-47.1.9").unwrap(),
            installer_path_for("1.20.1", "47.1.9").unwrap()
        );
        assert!(installer_path_for("1.20.1", "../x").is_err());
        assert!(installer_path_for("1.20.1", "47.1.9/../../x").is_err());
        assert_eq!(
            installer_path_for("1.21.1", "21.1.77").unwrap(),
            installer_path("21.1.77").unwrap()
        );
    }

    #[test]
    fn rejects_unsafe_installer_versions() {
        assert!(installer_path("21.1.77").is_ok());
        assert!(installer_path("../x").is_err());
        assert!(installer_path("21.1.77/../../x").is_err());
        assert!(installer_path("21.1.0-beta/../x").is_err());
    }
}
