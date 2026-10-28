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
//! The launch profile is the installer's `version.json`, read with
//! [`launch_profile`].

use std::cmp::Ordering;

use vl_core::component::ComponentKind;
use vl_core::error::{Error, Result};

use crate::install_profile::launch_profile;
use crate::loader::{LoaderProfile, LoaderVersion, LoaderVersions};

pub use crate::maven::{MAX_METADATA_BYTES, parse_maven_versions};

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
    let mut found: Vec<(Parsed, &String)> = all
        .iter()
        .filter(|v| game_version(v).as_deref() == Some(game))
        .filter_map(|v| parse(v).map(|p| (p, v)))
        .collect();
    found.sort_by(|a, b| compare(&b.0, &a.0));
    let versions = found
        .into_iter()
        .map(|(p, v)| LoaderVersion {
            version: v.clone(),
            stable: p.stable,
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
    fn rejects_unsafe_installer_versions() {
        assert!(installer_path("21.1.77").is_ok());
        assert!(installer_path("../x").is_err());
        assert!(installer_path("21.1.77/../../x").is_err());
        assert!(installer_path("21.1.0-beta/../x").is_err());
    }
}
