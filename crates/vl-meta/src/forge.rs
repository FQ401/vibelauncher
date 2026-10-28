//! Forge: the version list from the Maven metadata and the promotions, and the launch profile of
//! an installer.
//!
//! Forge publishes every version as `net.minecraftforge:forge` in its Maven repository. A Maven
//! version is `<game>-<forge>` (`1.20.1-47.3.0`), for some older builds with a branch suffix
//! (`1.7.10-10.13.4.1614-1.7.10`); the launcher calls the version by its Forge part (`47.3.0`,
//! `10.13.4.1614`) and accepts the Maven form without a branch as input too. The Forge part is
//! unique per game version, so [`find_maven_version`] finds the Maven version with its branch.
//!
//! Forge versions are not divided into stable and unstable ones: every version counts as stable.
//! The promotions document (`promotions_slim.json`) names a `<game>-recommended` build, which
//! gets the `recommended` flag; the newest version of a game version gets the `latest` flag. The
//! default choice ([`LoaderVersions::recommended`]) is the recommended build, else the latest.
//! When the promotions are not known, no version is recommended.
//!
//! Forge ships an installer jar for Minecraft 1.5.2 and newer. Since 1.13 (and for the last
//! 1.12.2 builds) it holds an `install_profile.json` with processors (spec 0 up to 1.16, spec 1
//! since 1.17), handled by [`crate::install_profile`]; older installers hold the legacy
//! `install` + `versionInfo` profile, handled by [`crate::forge_legacy`]. Game versions before
//! 1.5.2 have no installer (Forge is a jar mod there) and are `Unsupported` ([`check_game`]).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Deserialize;
use vl_core::component::ComponentKind;
use vl_core::error::{Error, ErrorKind, Result};

use crate::install_profile::launch_profile;
use crate::loader::{LoaderProfile, LoaderVersion, LoaderVersions};

pub use crate::maven::{MAX_METADATA_BYTES, parse_maven_versions};

/// The largest `promotions_slim.json` accepted.
pub const MAX_PROMOTIONS_BYTES: usize = 1024 * 1024;

/// The most entries accepted in the promotions document.
const MAX_PROMOTIONS: usize = 10_000;

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// The minor and patch numbers of a `1.<minor>[.<patch>]` game version (`1.7.10_pre4` gives
/// `(7, 10)`), or `None` for any other form.
fn minor_patch(game: &str) -> Option<(u32, u32)> {
    let mut parts = game.split('.');
    if parts.next() != Some("1") {
        return None;
    }
    let number = |part: Option<&str>| -> Option<u32> {
        let digits: String = part?.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let minor = number(parts.next())?;
    Some((minor, number(parts.next()).unwrap_or(0)))
}

/// Whether `game` is older than Minecraft 1.13 (a `1.<minor>` version with `minor` below 13).
/// Forge installers of these game versions may have the legacy format.
pub fn is_legacy_game(game: &str) -> bool {
    minor_patch(game).is_some_and(|(minor, _)| minor < 13)
}

/// Checks that Forge for `game` is supported: Minecraft 1.5.2 and newer.
///
/// # Errors
///
/// `Unsupported` for Minecraft older than 1.5.2.
pub fn check_game(game: &str) -> Result<()> {
    if minor_patch(game).is_some_and(|v| v < (5, 2)) {
        return Err(Error::unsupported(format!(
            "Forge for Minecraft {game} is not supported: only Minecraft 1.5.2 and newer"
        )));
    }
    Ok(())
}

/// The numbers of a Forge version (`47.3.0`), or `None` when it is not dot-separated numbers.
fn numbers(forge: &str) -> Option<Vec<u64>> {
    if forge.is_empty() {
        return None;
    }
    forge
        .split('.')
        .map(|p| {
            if p.is_empty() || p.len() > 18 || !p.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                p.parse().ok()
            }
        })
        .collect()
}

fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
}

/// Splits a Maven version (`1.20.1-47.3.0`, `1.7.10-10.13.4.1614-1.7.10`) into the game version
/// and the Forge version; a branch suffix is dropped. `None` when it is not of that form.
pub fn split_maven_version(maven: &str) -> Option<(&str, &str)> {
    let (game, rest) = maven.split_once('-')?;
    let (forge, branch) = match rest.split_once('-') {
        Some((forge, branch)) => (forge, Some(branch)),
        None => (rest, None),
    };
    (safe_name(game) && numbers(forge).is_some() && branch.is_none_or(safe_name))
        .then_some((game, forge))
}

/// The Maven version of Forge `forge` (short form or `<game>-<forge>`) for `game` in the full
/// Maven list, with its branch suffix if it has one.
pub fn find_maven_version<'a>(game: &str, forge: &str, all: &'a [String]) -> Option<&'a str> {
    let short = short_version(game, forge);
    all.iter()
        .map(String::as_str)
        .find(|v| split_maven_version(v) == Some((game, short)))
}

/// The Maven version of Forge `forge` for `game`. `forge` may already be the full form.
///
/// # Errors
///
/// `InvalidInput` for a version that is not `<numbers>` or `<game>-<numbers>`.
pub fn maven_version(game: &str, forge: &str) -> Result<String> {
    let short = short_version(game, forge);
    let full = format!("{game}-{short}");
    match split_maven_version(&full) {
        Some((g, f)) if g == game && f == short => Ok(full),
        _ => Err(Error::invalid_input(format!(
            "invalid Forge version {forge:?} for Minecraft {game}"
        ))),
    }
}

/// The Forge part of `version`: `1.20.1-47.3.0` → `47.3.0`; a short version is returned as is.
pub fn short_version<'a>(game: &str, version: &'a str) -> &'a str {
    version
        .strip_prefix(game)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(version)
}

/// The repository path of the installer of Forge `forge` for `game`, relative to the Maven root.
///
/// # Errors
///
/// `InvalidInput` for a version that cannot be a path part.
pub fn installer_path(game: &str, forge: &str) -> Result<String> {
    installer_path_of(&maven_version(game, forge)?)
}

/// The repository path of the installer of the Maven version `maven` (with its branch, if any).
///
/// # Errors
///
/// `InvalidInput` for a version that is not a Forge Maven version.
pub fn installer_path_of(maven: &str) -> Result<String> {
    if split_maven_version(maven).is_none() {
        return Err(Error::invalid_input(format!(
            "invalid Forge version {maven:?}"
        )));
    }
    Ok(format!(
        "net/minecraftforge/forge/{maven}/forge-{maven}-installer.jar"
    ))
}

/// The promoted builds per game version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Promotions {
    /// Game version to its recommended Forge version.
    pub recommended: BTreeMap<String, String>,
    /// Game version to its latest Forge version.
    pub latest: BTreeMap<String, String>,
}

/// Parses `promotions_slim.json` (`{"promos": {"1.20.1-recommended": "47.3.0", ...}}`). Keys of
/// another shape and values that are not Forge versions are ignored.
///
/// # Errors
///
/// `Integrity` for a document that is too large, not JSON of this shape, or has too many entries.
pub fn parse_promotions(bytes: &[u8]) -> Result<Promotions> {
    #[derive(Deserialize)]
    struct Raw {
        promos: BTreeMap<String, serde_json::Value>,
    }
    if bytes.len() > MAX_PROMOTIONS_BYTES {
        return Err(damaged("forge promotions are too large"));
    }
    let raw: Raw = serde_json::from_slice(bytes)
        .map_err(|e| damaged(format!("invalid forge promotions: {e}")))?;
    if raw.promos.len() > MAX_PROMOTIONS {
        return Err(damaged("forge promotions have too many entries"));
    }
    let mut out = Promotions::default();
    for (key, value) in raw.promos {
        let Some(version) = value.as_str().filter(|v| numbers(v).is_some()) else {
            continue;
        };
        let Some((game, kind)) = key.rsplit_once('-') else {
            continue;
        };
        let map = match kind {
            "recommended" => &mut out.recommended,
            "latest" => &mut out.latest,
            _ => continue,
        };
        map.insert(game.to_owned(), version.to_owned());
    }
    Ok(out)
}

/// The Forge versions for `game` out of the full Maven list, newest first, with the
/// `recommended` and `latest` flags (see the module documentation).
///
/// # Errors
///
/// `Unsupported` for a game version older than 1.5.2; `Integrity` for a repeated version.
pub fn versions_for_game(
    game: &str,
    all: &[String],
    promotions: Option<&Promotions>,
) -> Result<LoaderVersions> {
    check_game(game)?;
    let mut found: Vec<(Vec<u64>, &str)> = all
        .iter()
        .filter_map(|v| split_maven_version(v))
        .filter(|(g, _)| *g == game)
        .filter_map(|(_, forge)| numbers(forge).map(|n| (n, forge)))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let recommended = promotions.and_then(|p| p.recommended.get(game).and_then(|v| numbers(v)));
    let versions = found
        .into_iter()
        .map(|(n, forge)| LoaderVersion {
            version: forge.to_owned(),
            stable: true,
            recommended: recommended
                .as_ref()
                .is_some_and(|r| n.cmp(r) == Ordering::Equal),
            latest: false,
        })
        .collect();
    LoaderVersions::new(ComponentKind::Forge, game, versions)
}

/// The launch profile of Forge `forge` (the short version) from the installer's `version.json`.
///
/// # Errors
///
/// `Integrity` for a damaged document or one for another game version.
pub fn profile(game: &str, forge: &str, version_json: &[u8]) -> Result<LoaderProfile> {
    launch_profile(ComponentKind::Forge, game, forge, version_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_legacy_game_versions() {
        for legacy in ["1.12.2", "1.7.10", "1.7.10_pre4", "1.8", "1.5.2", "1.6"] {
            assert!(is_legacy_game(legacy), "{legacy}");
            assert!(check_game(legacy).is_ok(), "{legacy}");
        }
        for old in ["1.5.1", "1.5", "1.4.7", "1.2.5"] {
            assert!(is_legacy_game(old), "{old}");
            assert_eq!(check_game(old).unwrap_err().kind, ErrorKind::Unsupported);
        }
        for modern in ["1.13.2", "1.16.5", "1.20.1", "1.21", "26.1.2", "26.3"] {
            assert!(!is_legacy_game(modern), "{modern}");
            assert!(check_game(modern).is_ok(), "{modern}");
        }
    }

    #[test]
    fn splits_and_builds_maven_versions() {
        assert_eq!(
            split_maven_version("1.20.1-47.3.0"),
            Some(("1.20.1", "47.3.0"))
        );
        assert_eq!(split_maven_version("26.3-66.1.0"), Some(("26.3", "66.1.0")));
        assert_eq!(
            split_maven_version("1.7.10-10.13.4.1614-1.7.10"),
            Some(("1.7.10", "10.13.4.1614"))
        );
        assert_eq!(split_maven_version("1.7.10-10.13.4.1614-../x"), None);
        assert_eq!(split_maven_version("1.7.10-10.13.4.1614-a-b"), None);
        let all = vec![
            "1.7.10-10.13.4.1614-1.7.10".to_owned(),
            "1.12.2-14.23.5.2860".to_owned(),
        ];
        assert_eq!(
            find_maven_version("1.7.10", "10.13.4.1614", &all),
            Some("1.7.10-10.13.4.1614-1.7.10")
        );
        assert_eq!(
            find_maven_version("1.12.2", "1.12.2-14.23.5.2860", &all),
            Some("1.12.2-14.23.5.2860")
        );
        assert_eq!(find_maven_version("1.7.10", "10.13.4.1", &all), None);
        assert_eq!(
            installer_path_of("1.7.10-10.13.4.1614-1.7.10").unwrap(),
            "net/minecraftforge/forge/1.7.10-10.13.4.1614-1.7.10/forge-1.7.10-10.13.4.1614-1.7.10-installer.jar"
        );
        assert!(installer_path_of("../x").is_err());
        assert_eq!(split_maven_version("1.20.1"), None);
        assert_eq!(split_maven_version("../x-1.0"), None);
        assert_eq!(maven_version("1.20.1", "47.3.0").unwrap(), "1.20.1-47.3.0");
        assert_eq!(
            maven_version("1.20.1", "1.20.1-47.3.0").unwrap(),
            "1.20.1-47.3.0"
        );
        assert!(maven_version("1.20.1", "1.21.1-52.0.1").is_err());
        assert!(maven_version("1.20.1", "../x").is_err());
        assert!(maven_version("1.20.1", "47.3.0/../x").is_err());
        assert_eq!(
            installer_path("1.20.1", "47.3.0").unwrap(),
            "net/minecraftforge/forge/1.20.1-47.3.0/forge-1.20.1-47.3.0-installer.jar"
        );
    }
}
