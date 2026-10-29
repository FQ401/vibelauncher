//! NeoForge and Forge version lists on excerpts of the real upstream documents: every naming
//! scheme and the odd entries of the `maven-metadata.xml` files and `promotions_slim.json` (as
//! published on 2026-10-29), mapped to the game versions of the Mojang version manifest.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use vl_core::ErrorKind;
use vl_meta::{forge, neoforge};

fn read(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/loader/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn neoforge_all() -> Vec<String> {
    neoforge::parse_maven_versions(&read("neoforge_maven_metadata.xml")).unwrap()
}

fn neoforge_legacy() -> Vec<String> {
    neoforge::parse_maven_versions(&read("neoforge_legacy_maven_metadata.xml")).unwrap()
}

fn forge_all() -> Vec<String> {
    forge::parse_maven_versions(&read("forge_maven_metadata.xml")).unwrap()
}

/// Version and stable flag of the NeoForge list of `game`, newest first.
fn neoforge_names(game: &str) -> Vec<(String, bool)> {
    neoforge::versions_for_game_with_legacy(game, &neoforge_all(), &neoforge_legacy())
        .unwrap()
        .versions
        .into_iter()
        .map(|v| (v.version, v.stable))
        .collect()
}

#[test]
fn maps_every_neoforge_scheme_to_the_game_version() {
    let cases = [
        // `<minor>.<patch>.<build>`, Minecraft 1.20.2 to 1.21.11.
        ("20.2.3-beta", Some("1.20.2")),
        ("20.2.93", Some("1.20.2")),
        ("20.3.8-beta", Some("1.20.3")),
        ("20.5.21-beta", Some("1.20.5")),
        ("21.0.0-beta", Some("1.21")),
        ("21.0.167", Some("1.21")),
        ("21.1.265", Some("1.21.1")),
        ("21.9.16-beta", Some("1.21.9")),
        ("21.10.64", Some("1.21.10")),
        ("21.11.48", Some("1.21.11")),
        // `<year>.<drop>.<hotfix>.<build>`, Minecraft 26.1 and newer.
        ("26.1.0.1-beta", Some("26.1")),
        ("26.1.1.15-beta", Some("26.1.1")),
        ("26.1.2.124", Some("26.1.2")),
        ("26.2.0.93", Some("26.2")),
        ("26.3.0.99", Some("26.3")),
        // Builds for game snapshots and April Fools' versions are left out.
        ("26.1.0.0-alpha.1+snapshot-1", None),
        ("26.1.0.0-alpha.15+pre-3", None),
        ("0.25w14craftmine.5-beta", None),
    ];
    for (version, game) in cases {
        assert_eq!(
            neoforge::game_version(version).as_deref(),
            game,
            "{version}"
        );
    }
    for version in neoforge_all() {
        assert_eq!(
            neoforge::installer_path(&version).is_ok(),
            neoforge::game_version(&version).is_some(),
            "{version}"
        );
    }
}

#[test]
fn lists_neoforge_versions_per_game_newest_first() {
    let own = |list: &[(&str, bool)]| -> Vec<(String, bool)> {
        list.iter().map(|(v, s)| ((*v).to_owned(), *s)).collect()
    };
    assert_eq!(
        neoforge_names("1.20.2"),
        own(&[("20.2.93", true), ("20.2.3-beta", false)])
    );
    assert_eq!(
        neoforge_names("1.21"),
        own(&[("21.0.167", true), ("21.0.0-beta", false)])
    );
    assert_eq!(
        neoforge_names("26.1"),
        own(&[("26.1.0.19-beta", false), ("26.1.0.1-beta", false)])
    );
    assert_eq!(
        neoforge_names("26.1.2"),
        own(&[
            ("26.1.2.124", true),
            ("26.1.2.123", true),
            ("26.1.2.1-beta", false)
        ])
    );
    assert_eq!(
        neoforge_names("26.3"),
        own(&[
            ("26.3.0.99", true),
            ("26.3.0.76", true),
            ("26.3.0.1-beta", false)
        ])
    );
    // 1.20.1 comes from `net.neoforged:forge`; the stray `47.1.82` without the game prefix is
    // not a version of that artifact's scheme.
    assert_eq!(
        neoforge_names("1.20.1"),
        own(&[
            ("47.1.106", true),
            ("47.1.83", true),
            ("47.1.81", true),
            ("47.1.5", true)
        ])
    );
    for game in [
        "26.1-snapshot-1",
        "26.1-pre-3",
        "25w14craftmine",
        "1.20",
        "1.19.4",
    ] {
        assert!(neoforge_names(game).is_empty(), "{game}");
    }
    assert_eq!(
        neoforge::installer_path_for("1.20.1", "47.1.106").unwrap(),
        "net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-installer.jar"
    );
    assert_eq!(
        neoforge::installer_path_for("26.3", "26.3.0.99").unwrap(),
        "net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-installer.jar"
    );
}

#[test]
fn lists_forge_versions_of_every_era_with_the_promotions() {
    let all = forge_all();
    let promotions = forge::parse_promotions(&read("forge_promotions_slim.json")).unwrap();
    // Game version, versions newest first, the recommended one.
    let cases: [(&str, &[&str], Option<&str>); 11] = [
        ("26.3", &["66.1.0", "66.0.0"], Some("66.1.0")),
        ("26.2", &["65.1.5"], Some("65.1.5")),
        ("26.1.2", &["64.1.5"], Some("64.1.5")),
        ("26.1.1", &["63.0.2"], None),
        ("26.1", &["62.0.9"], None),
        ("1.21.11", &["61.2.4"], Some("61.2.4")),
        ("1.20.1", &["47.4.28", "47.3.0"], Some("47.4.28")),
        (
            "1.12.2",
            &["14.23.5.2864", "14.23.5.2859", "14.23.4.2720"],
            Some("14.23.5.2859"),
        ),
        (
            "1.9",
            &["12.16.1.1938", "12.16.1.1887", "12.16.0.1885"],
            Some("12.16.1.1887"),
        ),
        (
            "1.7.10",
            &["10.13.4.1614", "10.13.3.1401"],
            Some("10.13.4.1614"),
        ),
        ("1.5.2", &["7.8.1.738"], Some("7.8.1.738")),
    ];
    for (game, versions, recommended) in cases {
        let list = forge::versions_for_game(game, &all, Some(&promotions)).unwrap();
        let names: Vec<_> = list.versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(names, versions, "{game}");
        assert_eq!(
            list.versions[0].version, promotions.latest[game],
            "{game} latest"
        );
        assert_eq!(
            list.recommended().map(|v| v.version.as_str()),
            recommended.or(Some(versions[0])),
            "{game} recommended"
        );
    }
    // The pre-release keeps Forge's own name; the Mojang id `1.7.10-pre4` has no Forge builds.
    let pre = forge::versions_for_game("1.7.10_pre4", &all, Some(&promotions)).unwrap();
    assert_eq!(pre.versions[0].version, "10.12.2.1149");
    assert!(
        forge::versions_for_game("1.7.10-pre4", &all, None)
            .unwrap()
            .versions
            .is_empty()
    );
    assert_eq!(
        forge::versions_for_game("1.4.7", &all, None)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
}

#[test]
fn finds_the_maven_version_with_its_branch() {
    let all = forge_all();
    let cases = [
        ("26.3", "66.1.0", "26.3-66.1.0"),
        ("26.1.2", "64.1.5", "26.1.2-64.1.5"),
        ("1.12.2", "14.23.4.2720", "1.12.2-14.23.4.2720-4627"),
        ("1.9", "12.16.1.1938", "1.9-12.16.1.1938-1.9.0"),
        ("1.9", "12.16.0.1885", "1.9-12.16.0.1885-1.9"),
        ("1.9", "12.16.1.1887", "1.9-12.16.1.1887"),
        ("1.7.10", "10.13.4.1614", "1.7.10-10.13.4.1614-1.7.10"),
        ("1.7.10", "10.13.3.1401", "1.7.10-10.13.3.1401-1710ls"),
        (
            "1.7.10_pre4",
            "10.12.2.1149",
            "1.7.10_pre4-10.12.2.1149-prerelease",
        ),
    ];
    for (game, version, maven) in cases {
        assert_eq!(
            forge::find_maven_version(game, version, &all),
            Some(maven),
            "{game} {version}"
        );
    }
    assert_eq!(
        forge::installer_path_of("26.3-66.1.0").unwrap(),
        "net/minecraftforge/forge/26.3-66.1.0/forge-26.3-66.1.0-installer.jar"
    );
}
