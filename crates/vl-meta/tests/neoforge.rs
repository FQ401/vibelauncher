//! NeoForge metadata on synthetic documents: the Maven version list, the installer profile and
//! its processor plan, and the launch profile.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use vl_core::ErrorKind;
use vl_meta::install_profile::{InstallProfile, PlanContext, Side, manifest_main_class};
use vl_meta::neoforge::{parse_maven_versions, profile, versions_for_game};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn library(name: &str, path: &str, url: &str) -> Value {
    json!({
        "name": name,
        "downloads": {"artifact": {"path": path, "url": url, "sha1": SHA_A, "size": 10}}
    })
}

/// A profile shaped like the NeoForge 1.21 installers, with made-up tools.
fn install_profile() -> Value {
    json!({
        "spec": 1,
        "profile": "NeoForge",
        "version": "neoforge-21.1.7",
        "minecraft": "1.21.1",
        "json": "/version.json",
        "data": {
            "MAPPINGS": {"client": "[org.example:maps:1.0:mappings@txt]", "server": "[org.example:maps:1.0:server@txt]"},
            "BINPATCH": {"client": "/data/client.lzma", "server": "/data/server.lzma"},
            "PATCHED": {"client": "[org.example:game:21.1.7:client]", "server": "[org.example:game:21.1.7:server]"},
            "MCP_VERSION": {"client": "'1.21.1-1'", "server": "'1.21.1-1'"},
            "PLAIN": {"client": "as-is", "server": "as-is"},
            "PATCHED_SHA": {"client": format!("'{SHA_B}'"), "server": "''"}
        },
        "processors": [
            {
                "sides": ["server"],
                "jar": "org.example:tool:1.0",
                "classpath": ["org.example:tool:1.0"],
                "args": ["--server-only", "{MINECRAFT_JAR}"]
            },
            {
                "jar": "org.example:tool:1.0",
                "classpath": ["org.example:tool:1.0", "org.example:dep:2.0"],
                "args": ["--task", "MAPS", "--version", "{MINECRAFT_VERSION}", "--side", "{SIDE}",
                         "--input", "[org.example:zip:1.0@zip]", "--output", "{MAPPINGS}",
                         "--to", "{ROOT}/libraries/", "--key", "{MCP_VERSION}", "{PLAIN}"]
            },
            {
                "sides": ["client"],
                "jar": "org.example:patcher:1.0:fatjar",
                "classpath": ["org.example:patcher:1.0:fatjar"],
                "args": ["--clean", "{MINECRAFT_JAR}", "--output", "{PATCHED}", "--apply", "{BINPATCH}",
                         "--installer", "{INSTALLER}", "--libs", "{LIBRARY_DIR}"],
                "outputs": {"{PATCHED}": "{PATCHED_SHA}"}
            }
        ],
        "libraries": [
            library("org.example:tool:1.0", "org/example/tool/1.0/tool-1.0.jar", "https://repo.example.test/org/example/tool/1.0/tool-1.0.jar"),
            library("org.example:dep:2.0", "org/example/dep/2.0/dep-2.0.jar", "https://repo.example.test/org/example/dep/2.0/dep-2.0.jar"),
            library("org.example:bundled:1.0", "org/example/bundled/1.0/bundled-1.0.jar", "")
        ]
    })
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn ctx(side: Side) -> PlanContext {
    PlanContext {
        side,
        minecraft_jar: "/d/versions/1.21.1/1.21.1.jar".into(),
        root: "/d".into(),
        library_dir: "/d/libraries".into(),
        installer: "/d/nf/installer.jar".into(),
        extracted_dir: "/d/nf".into(),
        separator: '/',
    }
}

#[test]
fn plans_the_client_processors() {
    let p = InstallProfile::parse(&bytes(&install_profile())).unwrap();
    assert_eq!(p.minecraft, "1.21.1");
    let steps = p.plan(&ctx(Side::Client)).unwrap();
    assert_eq!(steps.len(), 2, "the server-only processor is left out");

    let maps = &steps[0];
    assert_eq!(maps.jar, "/d/libraries/org/example/tool/1.0/tool-1.0.jar");
    assert_eq!(
        maps.classpath,
        [
            "/d/libraries/org/example/tool/1.0/tool-1.0.jar",
            "/d/libraries/org/example/dep/2.0/dep-2.0.jar"
        ]
    );
    assert_eq!(
        maps.args,
        [
            "--task",
            "MAPS",
            "--version",
            "1.21.1",
            "--side",
            "client",
            "--input",
            "/d/libraries/org/example/zip/1.0/zip-1.0.zip",
            "--output",
            "/d/libraries/org/example/maps/1.0/maps-1.0-mappings.txt",
            "--to",
            "/d/libraries/",
            "--key",
            "1.21.1-1",
            "as-is",
        ]
    );
    assert!(maps.outputs.is_empty());

    let patch = &steps[1];
    assert_eq!(
        patch.args,
        [
            "--clean",
            "/d/versions/1.21.1/1.21.1.jar",
            "--output",
            "/d/libraries/org/example/game/21.1.7/game-21.1.7-client.jar",
            "--apply",
            "/d/nf/data/client.lzma",
            "--installer",
            "/d/nf/installer.jar",
            "--libs",
            "/d/libraries",
        ]
    );
    assert_eq!(
        patch.outputs,
        [(
            "/d/libraries/org/example/game/21.1.7/game-21.1.7-client.jar".to_owned(),
            SHA_B.to_owned()
        )]
    );
}

#[test]
fn the_server_side_has_its_own_steps_and_values() {
    let server = InstallProfile::parse(&bytes(&install_profile())).unwrap();
    let steps = server.plan(&ctx(Side::Server)).unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0].args,
        ["--server-only", "/d/versions/1.21.1/1.21.1.jar"]
    );
    assert!(steps[1].args.contains(&"server".to_owned()));
    assert!(
        steps[1]
            .args
            .contains(&"/d/libraries/org/example/maps/1.0/maps-1.0-server.txt".to_owned())
    );
}

#[test]
fn uses_the_host_separator() {
    let p = InstallProfile::parse(&bytes(&install_profile())).unwrap();
    let mut c = ctx(Side::Client);
    c.library_dir = r"C:\vl\libraries\".into();
    c.extracted_dir = r"C:\vl\nf".into();
    c.separator = '\\';
    let steps = p.plan(&c).unwrap();
    assert_eq!(
        steps[0].jar,
        r"C:\vl\libraries\org\example\tool\1.0\tool-1.0.jar"
    );
    assert!(
        steps[1]
            .args
            .contains(&r"C:\vl\nf\data\client.lzma".to_owned())
    );
}

#[test]
fn lists_installer_files_and_data_artifacts() {
    let p = InstallProfile::parse(&bytes(&install_profile())).unwrap();
    assert_eq!(
        p.installer_files(Side::Client).unwrap(),
        [
            "data/client.lzma",
            "maven/org/example/bundled/1.0/bundled-1.0.jar"
        ]
    );
    let artifacts = p.data_artifacts(&ctx(Side::Client)).unwrap();
    let names: Vec<&str> = artifacts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["MAPPINGS", "PATCHED"]);
    let libs = p.libraries().unwrap();
    assert_eq!(libs.len(), 3);
    assert_eq!(
        libs[2].url, None,
        "an empty URL means bundled in the installer"
    );
}

type Edit = Box<dyn Fn(&mut Value)>;

#[test]
fn rejects_damaged_or_hostile_profiles() {
    let cases: Vec<(&str, Edit)> = vec![
        (
            "unknown value",
            Box::new(|d| d["processors"][1]["args"][0] = json!("{NOPE}")),
        ),
        (
            "unclosed reference",
            Box::new(|d| d["processors"][1]["args"][0] = json!("{ROOT")),
        ),
        (
            "bad coordinate",
            Box::new(|d| d["processors"][1]["jar"] = json!("org.example:../x:1")),
        ),
        (
            "bad classpath",
            Box::new(|d| d["processors"][1]["classpath"][0] = json!("nope")),
        ),
        (
            "escaping data file",
            Box::new(|d| d["data"]["BINPATCH"]["client"] = json!("/../x")),
        ),
        (
            "escaping json path",
            Box::new(|d| d["json"] = json!("/../../version.json")),
        ),
        (
            "http library",
            Box::new(|d| {
                d["libraries"][0]["downloads"]["artifact"]["url"] = json!("http://x.test/a.jar")
            }),
        ),
        (
            "bad library sha1",
            Box::new(|d| d["libraries"][0]["downloads"]["artifact"]["sha1"] = json!("zz")),
        ),
        (
            "library without artifact",
            Box::new(|d| d["libraries"][0]["downloads"] = json!({})),
        ),
        (
            "escaping library path",
            Box::new(|d| {
                d["libraries"][0]["downloads"]["artifact"]["path"] = json!("../../evil.jar")
            }),
        ),
        (
            "bad output hash",
            Box::new(|d| d["data"]["PATCHED_SHA"]["client"] = json!("'abc'")),
        ),
        ("no minecraft", Box::new(|d| d["minecraft"] = json!(""))),
    ];
    for (what, edit) in cases {
        let mut doc = install_profile();
        edit(&mut doc);
        let parsed = InstallProfile::parse(&bytes(&doc));
        let planned = parsed.and_then(|p| p.plan(&ctx(Side::Client)).map(|_| p));
        let e = planned.expect_err(what);
        assert_eq!(e.kind, ErrorKind::Integrity, "{what}: {e:?}");
    }

    let mut doc = install_profile();
    doc["spec"] = json!(2);
    assert_eq!(
        InstallProfile::parse(&bytes(&doc)).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert!(InstallProfile::parse(b"not json").is_err());
    let huge = vec![b' '; vl_meta::install_profile::MAX_INSTALL_PROFILE_BYTES + 1];
    assert!(InstallProfile::parse(&huge).is_err());
}

#[test]
fn reads_main_classes_of_processor_jars() {
    assert_eq!(
        manifest_main_class("Manifest-Version: 1.0\nMain-Class: org.example.Tool\n").as_deref(),
        Some("org.example.Tool")
    );
}

const METADATA: &str = r"<?xml version='1.0' encoding='UTF-8'?>
<metadata>
  <groupId>net.neoforged</groupId>
  <artifactId>neoforge</artifactId>
  <versioning>
    <latest>26.1.2.3</latest>
    <release>26.1.2.3</release>
    <versions>
      <version>20.2.3-beta</version>
      <version>21.1.9</version>
      <version>21.1.1-beta</version>
      <version>21.1.10</version>
      <version>21.1.2</version>
      <version>21.0.5</version>
      <version>21.10.1-beta</version>
      <version>26.1.0.0-alpha.1+snapshot-1</version>
      <version>26.1.0.4-beta</version>
      <version>26.1.0.12</version>
      <version>26.1.2.3</version>
      <version>0.25w14craftmine.3-beta</version>
    </versions>
    <lastUpdated>20261025061424</lastUpdated>
  </versioning>
</metadata>";

#[test]
fn lists_versions_for_a_game_version_newest_first() {
    let all = parse_maven_versions(METADATA.as_bytes()).unwrap();
    assert_eq!(all.len(), 12);

    let list = versions_for_game("1.21.1", &all).unwrap();
    let names: Vec<(&str, bool)> = list
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.stable))
        .collect();
    assert_eq!(
        names,
        [
            ("21.1.10", true),
            ("21.1.9", true),
            ("21.1.2", true),
            ("21.1.1-beta", false)
        ]
    );
    assert_eq!(list.recommended().unwrap().version, "21.1.10");

    let v = |game: &str| -> Vec<String> {
        versions_for_game(game, &all)
            .unwrap()
            .versions
            .into_iter()
            .map(|v| v.version)
            .collect()
    };
    assert_eq!(v("1.21"), ["21.0.5"]);
    assert_eq!(v("1.21.10"), ["21.10.1-beta"]);
    assert_eq!(v("1.20.2"), ["20.2.3-beta"]);
    assert_eq!(v("26.1"), ["26.1.0.12", "26.1.0.4-beta"]);
    assert_eq!(v("26.1.2"), ["26.1.2.3"]);
    assert!(v("1.20.1").is_empty());
    let none = versions_for_game("1.20.1", &all).unwrap();
    assert_eq!(none.check("1").unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn rejects_damaged_metadata() {
    assert!(parse_maven_versions(b"<metadata></metadata>").is_err());
    assert!(parse_maven_versions(b"<versions><version>1.0</versions>").is_err());
    assert!(parse_maven_versions(&[0xff, 0xfe]).is_err());
    let dup = "<versions><version>21.1.1</version><version>21.1.1</version></versions>";
    let all = parse_maven_versions(dup.as_bytes()).unwrap();
    assert_eq!(
        versions_for_game("1.21.1", &all).unwrap_err().kind,
        ErrorKind::Integrity
    );
}

fn version_json(game: &str) -> Value {
    json!({
        "id": "neoforge-21.1.7",
        "inheritsFrom": game,
        "type": "release",
        "releaseTime": "2026-01-01T00:00:00",
        "mainClass": "org.example.Bootstrap",
        "arguments": {
            "game": ["--fml.neoForgeVersion", "21.1.7"],
            "jvm": ["-DignoreList=client-extra,${version_name}.jar", "-DlibraryDirectory=${library_directory}"]
        },
        "libraries": [library("org.example:loader:1.0", "org/example/loader/1.0/loader-1.0.jar", "https://repo.example.test/org/example/loader/1.0/loader-1.0.jar")]
    })
}

#[test]
fn the_launch_profile_names_the_vanilla_jar_in_the_ignore_list() {
    let p = profile("1.21.1", "21.1.7", &bytes(&version_json("1.21.1"))).unwrap();
    assert_eq!(p.kind, vl_core::component::ComponentKind::NeoForge);
    assert_eq!(p.id, "neoforge-21.1.7");
    assert_eq!(p.main_class, "org.example.Bootstrap");
    let jvm: Vec<&str> = p
        .arguments
        .jvm
        .iter()
        .flat_map(|a| a.values.iter().map(String::as_str))
        .collect();
    assert_eq!(
        jvm,
        [
            "-DignoreList=client-extra,1.21.1.jar",
            "-DlibraryDirectory=${library_directory}"
        ]
    );
    assert_eq!(p.libraries.len(), 1);
    assert_eq!(
        profile("1.21.4", "21.1.7", &bytes(&version_json("1.21.1")))
            .unwrap_err()
            .kind,
        ErrorKind::Integrity
    );
}
