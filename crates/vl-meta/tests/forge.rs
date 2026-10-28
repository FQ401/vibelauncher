//! Forge metadata on synthetic documents: the Maven version list with the promotions, the
//! installer profiles of both specs, and the launch profile.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use vl_core::ErrorKind;
use vl_core::component::ComponentKind;
use vl_meta::forge::{
    Promotions, parse_maven_versions, parse_promotions, profile, versions_for_game,
};
use vl_meta::install_profile::{InstallProfile, PlanContext, Side, local_libraries};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn all() -> Vec<String> {
    let xml = "<metadata><versioning><versions>\
        <version>1.20.1-47.1.0</version><version>1.20.1-47.10.2</version>\
        <version>1.20.1-47.2.0</version><version>1.20.1-47.3.0</version>\
        <version>1.21.1-52.0.1</version><version>1.21-51.0.3</version>\
        <version>1.12.2-14.23.5.2860</version><version>1.7.10-10.13.4.1614-1.7.10</version>\
        <version>1.20.1-bad</version>\
        </versions></versioning></metadata>";
    parse_maven_versions(xml.as_bytes()).unwrap()
}

fn promotions() -> Promotions {
    parse_promotions(
        br#"{"homepage": "https://example.test", "promos": {
            "1.20.1-recommended": "47.2.0", "1.20.1-latest": "47.10.2",
            "1.21-latest": "51.0.3", "1.21-weird": "1", "odd": "1.0", "1.21.1-latest": 7
        }}"#,
    )
    .unwrap()
}

fn names(game: &str, promos: Option<&Promotions>) -> Vec<(String, bool)> {
    versions_for_game(game, &all(), promos)
        .unwrap()
        .versions
        .into_iter()
        .map(|v| (v.version, v.stable))
        .collect()
}

#[test]
fn reads_the_promotions() {
    let p = promotions();
    assert_eq!(
        p.recommended.get("1.20.1").map(String::as_str),
        Some("47.2.0")
    );
    assert_eq!(p.latest.get("1.20.1").map(String::as_str), Some("47.10.2"));
    assert_eq!(p.latest.get("1.21").map(String::as_str), Some("51.0.3"));
    assert!(
        !p.latest.contains_key("1.21.1"),
        "a value that is not a version"
    );
    assert_eq!(p.recommended.len(), 1);
    for bad in [&b"[]"[..], b"{}", b"not json", b"{\"promos\": []}"] {
        assert_eq!(
            parse_promotions(bad).unwrap_err().kind,
            ErrorKind::Integrity
        );
    }
}

#[test]
fn lists_versions_newest_first_up_to_the_recommended_one_as_stable() {
    let p = promotions();
    assert_eq!(
        names("1.20.1", Some(&p)),
        [
            ("47.10.2".to_owned(), false),
            ("47.3.0".to_owned(), false),
            ("47.2.0".to_owned(), true),
            ("47.1.0".to_owned(), true),
        ]
    );
    let list = versions_for_game("1.20.1", &all(), Some(&p)).unwrap();
    assert_eq!(list.kind, ComponentKind::Forge);
    assert_eq!(list.recommended().unwrap().version, "47.2.0");

    // No recommended build: nothing is stable, the newest is used.
    let list = versions_for_game("1.21", &all(), Some(&p)).unwrap();
    assert_eq!(list.recommended().unwrap().version, "51.0.3");
    assert!(!list.versions[0].stable);

    // Without promotions every version counts as stable.
    assert!(names("1.20.1", None).iter().all(|(_, stable)| *stable));
    let list = versions_for_game("1.20.1", &all(), None).unwrap();
    assert_eq!(list.recommended().unwrap().version, "47.10.2");

    // "1.21" does not match "1.21.1" builds, and an unknown game version has none.
    assert_eq!(names("1.21", None).len(), 1);
    assert!(names("1.19.2", None).is_empty());
}

#[test]
fn game_versions_before_1_13_are_unsupported() {
    for game in ["1.12.2", "1.7.10"] {
        let e = versions_for_game(game, &all(), None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unsupported, "{game}");
        assert!(e.message.contains("1.13"), "{}", e.message);
    }
}

fn library(name: &str, path: &str, url: &str) -> Value {
    json!({
        "name": name,
        "downloads": {"artifact": {"path": path, "url": url, "sha1": SHA_A, "size": 10}}
    })
}

/// A spec 0 profile shaped like the Forge 1.13–1.16 installers: processors without `sides`, the
/// universal jar carried by the installer.
fn spec0() -> Value {
    json!({
        "_comment_": ["synthetic"],
        "spec": 0,
        "profile": "forge",
        "version": "1.16.5-forge-36.2.0",
        "path": "net.minecraftforge:forge:1.16.5-36.2.0",
        "minecraft": "1.16.5",
        "json": "/version.json",
        "logo": "/big_logo.png",
        "mirrorList": "https://example.test/mirrors",
        "welcome": "hello",
        "data": {
            "MAPPINGS": {"client": "[org.example:config:1.16.5-1:mappings@txt]", "server": "[org.example:config:1.16.5-1:mappings@txt]"},
            "BINPATCH": {"client": "/data/client.lzma", "server": "/data/server.lzma"},
            "MC_SLIM": {"client": "[net.minecraft:client:1.16.5-1:slim]", "server": "[net.minecraft:server:1.16.5-1:slim]"},
            "MC_SLIM_SHA": {"client": format!("'{SHA_A}'"), "server": "''"},
            "PATCHED": {"client": "[net.minecraftforge:forge:1.16.5-36.2.0:client]", "server": "[net.minecraftforge:forge:1.16.5-36.2.0:server]"}
        },
        "processors": [
            {
                "jar": "org.example:splitter:1.0",
                "classpath": ["org.example:splitter:1.0"],
                "args": ["--input", "{MINECRAFT_JAR}", "--slim", "{MC_SLIM}", "--srg", "{MAPPINGS}"],
                "outputs": {"{MC_SLIM}": "{MC_SLIM_SHA}"}
            },
            {
                "jar": "org.example:patcher:1.0",
                "classpath": ["org.example:dep:1.0"],
                "args": ["--clean", "{MC_SLIM}", "--output", "{PATCHED}", "--apply", "{BINPATCH}"]
            }
        ],
        "libraries": [
            library("org.example:splitter:1.0", "org/example/splitter/1.0/splitter-1.0.jar", "https://repo.example.test/org/example/splitter/1.0/splitter-1.0.jar"),
            library("org.example:patcher:1.0", "org/example/patcher/1.0/patcher-1.0.jar", "https://repo.example.test/org/example/patcher/1.0/patcher-1.0.jar"),
            library("net.minecraftforge:forge:1.16.5-36.2.0:universal", "net/minecraftforge/forge/1.16.5-36.2.0/forge-1.16.5-36.2.0-universal.jar", "")
        ]
    })
}

#[test]
fn plans_a_spec_0_profile_for_the_client() {
    let p = InstallProfile::parse(&serde_json::to_vec(&spec0()).unwrap()).unwrap();
    assert_eq!(p.spec, 0);
    let ctx = PlanContext {
        side: Side::Client,
        minecraft_jar: "/d/versions/1.16.5/1.16.5.jar".into(),
        root: "/d".into(),
        library_dir: "/d/libraries".into(),
        installer: "/d/forge/installer.jar".into(),
        extracted_dir: "/d/forge".into(),
        separator: '/',
    };
    let steps = p.plan(&ctx).unwrap();
    assert_eq!(
        steps.len(),
        2,
        "processors without sides run for the client"
    );
    assert_eq!(
        steps[0].outputs,
        [(
            "/d/libraries/net/minecraft/client/1.16.5-1/client-1.16.5-1-slim.jar".to_owned(),
            SHA_A.to_owned()
        )]
    );
    assert_eq!(steps[1].args[5], "/d/forge/data/client.lzma");
    assert_eq!(
        steps[1].classpath,
        [
            "/d/libraries/org/example/patcher/1.0/patcher-1.0.jar",
            "/d/libraries/org/example/dep/1.0/dep-1.0.jar"
        ],
        "the jar a profile leaves out of the class path comes first"
    );
    assert_eq!(steps[0].classpath.len(), 1);
    assert_eq!(
        p.installer_files(Side::Client).unwrap(),
        [
            "data/client.lzma",
            "maven/net/minecraftforge/forge/1.16.5-36.2.0/forge-1.16.5-36.2.0-universal.jar"
        ]
    );
}

fn version_json(main: &str, jvm: Value, libraries: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "1.20.1-forge-47.3.0",
        "inheritsFrom": "1.20.1",
        "type": "release",
        "time": "2026-01-01T00:00:00+00:00",
        "releaseTime": "2026-01-01T00:00:00+00:00",
        "mainClass": main,
        "arguments": {"game": ["--launchTarget", "forgeclient", "--fml.forgeVersion", "47.3.0"], "jvm": jvm},
        "libraries": libraries
    }))
    .unwrap()
}

#[test]
fn reads_the_launch_profile_and_its_local_libraries() {
    let json = version_json(
        "org.example.BootstrapLauncher",
        json!([
            "-DignoreList=bootstrap,client-extra,forge-,${version_name}.jar",
            "-p",
            "x"
        ]),
        json!([
            library("net.minecraftforge:fmlloader:1.20.1-47.3.0", "net/minecraftforge/fmlloader/1.20.1-47.3.0/fmlloader-1.20.1-47.3.0.jar", "https://repo.example.test/fml.jar"),
            library("net.minecraftforge:forge:1.20.1-47.3.0:client", "net/minecraftforge/forge/1.20.1-47.3.0/forge-1.20.1-47.3.0-client.jar", ""),
            {"name": "org.example:plain:1.0"},
            {"name": "org.example:plain:1.0"}
        ]),
    );
    let p = profile("1.20.1", "47.3.0", &json).unwrap();
    assert_eq!(p.kind, ComponentKind::Forge);
    assert_eq!(p.loader_version, "47.3.0");
    assert_eq!(p.main_class, "org.example.BootstrapLauncher");
    assert_eq!(
        p.arguments.jvm[0].values,
        ["-DignoreList=bootstrap,client-extra,forge-,1.20.1.jar"]
    );
    assert_eq!(
        p.libraries.len(),
        3,
        "an identical repeated entry is dropped"
    );
    let local = local_libraries(&p.libraries).unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(
        local[0].path,
        "net/minecraftforge/forge/1.20.1-47.3.0/forge-1.20.1-47.3.0-client.jar"
    );
    assert_eq!(local[0].url, None);

    // Repeated entries that differ are still damaged.
    let repeated = version_json(
        "org.example.Main",
        json!([]),
        json!([{"name": "org.example:plain:1.0"}, {"name": "org.example:plain:2.0"}]),
    );
    assert_eq!(
        profile("1.20.1", "47.3.0", &repeated).unwrap_err().kind,
        ErrorKind::Integrity
    );

    // A profile for another game version is damaged.
    assert_eq!(
        profile("1.20.2", "47.3.0", &json).unwrap_err().kind,
        ErrorKind::Integrity
    );
}
