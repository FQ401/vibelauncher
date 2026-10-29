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
//! | `<year>.<drop>.<hotfix>.<build>-alpha.<n>+<kind>-<k>` (`26.1.0.0-alpha.1+snapshot-1`, `26.1.0.0-alpha.15+pre-3`) | the game version above, then `-<kind>-<k>` (`26.1-snapshot-1`, `26.1-pre-3`); `kind` is `snapshot`, `pre` or `rc` |
//! | `0.<id>.<build>-beta` (`0.25w14craftmine.5-beta`) | `<id>` (`25w14craftmine`), an April Fools' version |
//!
//! A `-beta` or `-alpha` suffix marks an unstable build. The last two schemes are builds for
//! game versions that are not releases (snapshots, pre-releases, release candidates and April
//! Fools' versions); they are listed only for that game version and marked
//! [`LoaderVersion::unstable_game`]. Names that fit no scheme are left out. The installers of
//! such builds name the same game version in `inheritsFrom` (checked on 2026-10-29 for
//! `26.1.0.0-alpha.1+snapshot-1`, `26.1.0.0-alpha.15+pre-3` and `0.25w14craftmine.5-beta`), and
//! the repository serves the `+` of their paths as it is.
//!
//! Minecraft 1.20.1 is the exception: NeoForge's builds for it were published as
//! `net.neoforged:forge`, versions `1.20.1-47.1.<build>`, with the same installer format. They
//! are listed as NeoForge versions of 1.20.1 in the short form (`47.1.106`), merged with the
//! `net.neoforged:neoforge` list ([`versions_for_game_with_legacy`]); an entry of that artifact
//! without the game prefix (`47.1.82`) is skipped.
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

/// A NeoForge version split into its numbers, its suffix and its build metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    numbers: Vec<u64>,
    stable: bool,
    /// The numbers of the suffix (`12` of `alpha.12`), which order the builds of one game
    /// version with the same numbers.
    suffix_numbers: Vec<u64>,
    /// The game version an April Fools' build names (`25w14craftmine`).
    april_game: Option<String>,
    /// The game version tag of the build metadata (`snapshot-1` of `+snapshot-1`).
    tag: Option<String>,
}

/// The kinds of game versions a build metadata tag can name, as the Mojang version manifest
/// spells them after the release: `26.1-snapshot-1`, `26.1-pre-3`, `26.3-rc-1`.
const GAME_TAGS: [&str; 3] = ["snapshot", "pre", "rc"];

/// Whether `tag` is `<kind>-<number>` with a kind of [`GAME_TAGS`].
fn is_game_tag(tag: &str) -> bool {
    tag.split_once('-')
        .is_some_and(|(kind, k)| GAME_TAGS.contains(&kind) && number(k).is_some())
}

fn number(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        part.parse().ok()
    }
}

fn parse(version: &str) -> Option<Parsed> {
    let (version, tag) = match version.split_once('+') {
        Some((v, t)) => (v, Some(t)),
        None => (version, None),
    };
    if tag.is_some_and(|t| !is_game_tag(t)) {
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
    let suffix_numbers = suffix
        .map(|s| s.split(['.', '-']).filter_map(number).collect())
        .unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    let (numbers, april_game) = match parts.as_slice() {
        // `0.<game>.<build>`: an April Fools' build. The game id has a letter, else the
        // version would be an ordinary one.
        ["0", game, build]
            if tag.is_none()
                && game
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && game.bytes().any(|b| b.is_ascii_lowercase()) =>
        {
            (vec![0, number(build)?], Some((*game).to_owned()))
        }
        _ => (
            parts
                .iter()
                .map(|p| number(p))
                .collect::<Option<Vec<u64>>>()?,
            None,
        ),
    };
    Some(Parsed {
        numbers,
        stable: suffix.is_none() && tag.is_none(),
        suffix_numbers,
        april_game,
        tag: tag.map(str::to_owned),
    })
}

impl Parsed {
    /// The game version (see the module documentation), and whether it is a snapshot, a
    /// pre-release, a release candidate or an April Fools' version.
    fn game(&self) -> Option<(String, bool)> {
        if let Some(game) = &self.april_game {
            return Some((game.clone(), true));
        }
        match (self.numbers.as_slice(), &self.tag) {
            ([minor, patch, _build], None) if (20..26).contains(minor) => Some((
                if *patch == 0 {
                    format!("1.{minor}")
                } else {
                    format!("1.{minor}.{patch}")
                },
                false,
            )),
            ([year, drop, hotfix, _build], tag) if *year >= 26 => {
                let release = if *hotfix == 0 {
                    format!("{year}.{drop}")
                } else {
                    format!("{year}.{drop}.{hotfix}")
                };
                Some(match tag {
                    Some(tag) => (format!("{release}-{tag}"), true),
                    None => (release, false),
                })
            }
            _ => None,
        }
    }
}

/// The game version a NeoForge version is for, or `None` for a name that fits no scheme (see
/// the module documentation).
pub fn game_version(neoforge: &str) -> Option<String> {
    parse(neoforge)?.game().map(|(game, _)| game)
}

/// Whether a NeoForge version is a build for a game snapshot, pre-release, release candidate or
/// April Fools' version (`26.1.0.0-alpha.1+snapshot-1`, `0.25w14craftmine.5-beta`).
pub fn is_for_unstable_game(neoforge: &str) -> bool {
    parse(neoforge)
        .and_then(|p| p.game())
        .is_some_and(|(_, unstable)| unstable)
}

fn compare(a: &Parsed, b: &Parsed) -> Ordering {
    a.numbers
        .cmp(&b.numbers)
        .then_with(|| a.stable.cmp(&b.stable))
        .then_with(|| a.suffix_numbers.cmp(&b.suffix_numbers))
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
        .filter_map(|v| parse(v).map(|p| (p, v.clone())))
        .filter(|(p, _)| p.game().is_some_and(|(g, _)| g == game))
        .collect();
    found.extend(legacy_parsed(game, legacy));
    found.sort_by(|a, b| compare(&b.0, &a.0));
    let versions = found
        .into_iter()
        .map(|(p, version)| {
            let unstable_game = p.game().is_some_and(|(_, unstable)| unstable);
            LoaderVersion {
                version,
                stable: p.stable && !unstable_game,
                unstable_game,
                ..LoaderVersion::default()
            }
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
            ("26.1.0.0-alpha.1+snapshot-1", Some("26.1-snapshot-1")),
            ("26.1.0.0-alpha.15+pre-3", Some("26.1-pre-3")),
            ("26.3.0.0-alpha.2+rc-1", Some("26.3-rc-1")),
            ("26.1.2.0-alpha.1+rc-1", Some("26.1.2-rc-1")),
            ("0.25w14craftmine.3-beta", Some("25w14craftmine")),
            ("26.1.0.0-alpha.1+snapshot", None),
            ("26.1.0.0-alpha.1+build-1", None),
            ("26.1.0.0-alpha.1+snapshot-1/../x", None),
            ("21.1.77+pre-1", None),
            ("0.25W14craftmine.3-beta", None),
            ("0.25w14craftmine.x-beta", None),
            ("1.25w14craftmine.3-beta", None),
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
        assert!(installer_path("26.1.0.0-alpha.1+snapshot-1/../x").is_err());
        assert!(installer_path("26.1.0.0-alpha.1+../x").is_err());
        assert!(installer_path("0.25w14craftmine/../x.5-beta").is_err());
        // Builds for unstable game versions: `+` stays as it is, the Maven layout of the
        // repository (which serves the encoded `%2B` form too).
        assert_eq!(
            installer_path("26.1.0.0-alpha.15+pre-3").unwrap(),
            "net/neoforged/neoforge/26.1.0.0-alpha.15+pre-3/neoforge-26.1.0.0-alpha.15+pre-3-installer.jar"
        );
        assert_eq!(
            installer_path("0.25w14craftmine.5-beta").unwrap(),
            "net/neoforged/neoforge/0.25w14craftmine.5-beta/neoforge-0.25w14craftmine.5-beta-installer.jar"
        );
    }

    #[test]
    fn marks_builds_for_unstable_game_versions() {
        let all: Vec<String> = [
            "26.1.0.0-alpha.1+snapshot-1",
            "26.1.0.0-alpha.10+snapshot-1",
            "26.1.0.0-alpha.2+snapshot-1",
            "26.1.0.0-alpha.15+pre-3",
            "26.1.0.1-beta",
            "26.1.0.19",
        ]
        .map(String::from)
        .into();
        let list = versions_for_game("26.1-snapshot-1", &all).unwrap();
        let names: Vec<_> = list
            .versions
            .iter()
            .map(|v| (v.version.as_str(), v.stable, v.unstable_game))
            .collect();
        assert_eq!(
            names,
            [
                ("26.1.0.0-alpha.10+snapshot-1", false, true),
                ("26.1.0.0-alpha.2+snapshot-1", false, true),
                ("26.1.0.0-alpha.1+snapshot-1", false, true)
            ]
        );
        let release = versions_for_game("26.1", &all).unwrap();
        assert!(release.versions.iter().all(|v| !v.unstable_game));
        assert_eq!(release.versions.len(), 2);
        assert!(is_for_unstable_game("0.25w14craftmine.5-beta"));
        assert!(is_for_unstable_game("26.1.0.0-alpha.15+pre-3"));
        assert!(!is_for_unstable_game("26.1.0.19"));
        assert!(!is_for_unstable_game("x"));
    }
}
