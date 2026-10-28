//! Legacy Forge installer profiles on synthetic documents, one per era: an inheriting
//! `versionInfo` (Minecraft 1.7.10 to 1.12.2), a standalone one with `stripMeta` (1.5.2), and
//! the last 1.12.2 builds, whose installer has the processor format without processors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use vl_core::ErrorKind;
use vl_meta::arguments::Argument;
use vl_meta::forge::profile;
use vl_meta::forge_legacy::{
    FmlLibrary, InstallerFormat, LegacyInstaller, fml_deobfuscation_data, installer_format,
};
use vl_meta::install_profile::{InstallProfile, local_libraries};
use vl_meta::version::{LibraryEntry, VersionJson};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

/// Shaped like the Forge 1.7.10 installers: inherits from the game, the Forge jar carried by
/// the installer, libraries from the Forge repository (one at an old address) with checksums
/// and from the Mojang one without.
fn inheriting() -> Value {
    json!({
        "install": {
            "profileName": "Forge",
            "target": "1.7.10-Forge10.13.4.1614-1.7.10",
            "path": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
            "version": "Forge 10.13.4.1614-1.7.10",
            "filePath": "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar",
            "minecraft": "1.7.10"
        },
        "versionInfo": {
            "id": "1.7.10-Forge10.13.4.1614-1.7.10",
            "type": "release",
            "releaseTime": "2026-01-01T00:00:00-0000",
            "minecraftArguments": "--username ${auth_player_name} --gameDir ${game_directory} --tweakClass org.example.fml.Tweaker",
            "mainClass": "net.minecraft.launchwrapper.Launch",
            "inheritsFrom": "1.7.10",
            "jar": "1.7.10",
            "libraries": [
                {"name": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10", "url": "https://maven.minecraftforge.net/"},
                {"name": "net.minecraft:launchwrapper:1.12", "serverreq": true},
                {"name": "org.example:scala-lib:2.11.1", "url": "https://maven.minecraftforge.net/",
                 "checksums": [SHA_A, SHA_B], "serverreq": true, "clientreq": true},
                {"name": "org.example:old-host:1.0", "url": "http://files.minecraftforge.net/maven/",
                 "checksums": [SHA_C]},
                {"name": "org.example:no-checksum:1.0", "url": "https://maven.minecraftforge.net/"},
                {"name": "com.google.guava:guava:17.0", "serverreq": true}
            ]
        }
    })
}

/// Shaped like the Forge 1.5.2 installer: a standalone `versionInfo` that repeats the vanilla
/// libraries (some only on some platforms, one also from the Forge repository), and `stripMeta`.
fn standalone() -> Value {
    json!({
        "install": {
            "target": "1.5.2-Forge7.8.1.738",
            "filePath": "minecraftforge-universal-1.5.2-7.8.1.738.jar",
            "path": "net.minecraftforge:minecraftforge:7.8.1.738",
            "minecraft": "1.5.2",
            "stripMeta": true
        },
        "versionInfo": {
            "id": "1.5.2-Forge7.8.1.738",
            "type": "release",
            "minecraftArguments": "${auth_player_name} ${auth_session} --gameDir ${game_directory} --tweakClass org.example.legacy.FixerTweaker",
            "mainClass": "net.minecraft.launchwrapper.Launch",
            "libraries": [
                {"name": "net.minecraftforge:minecraftforge:7.8.1.738", "url": "https://maven.minecraftforge.net/"},
                {"name": "org.ow2.asm:asm-all:4.1", "url": "https://maven.minecraftforge.net/", "checksums": [SHA_A]},
                {"name": "org.example:fixer:1.0", "url": "https://maven.minecraftforge.net/", "checksums": [SHA_B]},
                {"name": "net.minecraft:launchwrapper:1.5"},
                {"name": "org.ow2.asm:asm-all:4.1"},
                {"name": "org.lwjgl.lwjgl:lwjgl:2.9.0", "rules": [{"action": "allow"}]},
                {"name": "org.lwjgl.lwjgl:lwjgl:2.9.1-nightly", "rules": [{"action": "allow", "os": {"name": "osx"}}]},
                {"name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.0",
                 "natives": {"windows": "natives-windows", "linux": "natives-linux", "osx": "natives-osx"},
                 "extract": {"exclude": ["META-INF/"]}},
                {"name": "net.java.jinput:jinput-platform:2.0.5", "natives": {"windows": "natives-windows"}},
                {"name": "net.java.jinput:jinput-platform:2.0.5", "natives": {"windows": "natives-windows"}}
            ]
        }
    })
}

fn names(libraries: &[LibraryEntry]) -> Vec<&str> {
    libraries.iter().map(|l| l.name.as_str()).collect()
}

fn artifact<'a>(libraries: &'a [LibraryEntry], name: &str) -> &'a Value {
    libraries
        .iter()
        .find(|l| l.name == name)
        .unwrap()
        .rest
        .get("downloads")
        .and_then(|d| d.get("artifact"))
        .unwrap()
}

fn game_args(args: &[Argument]) -> Vec<String> {
    args.iter().flat_map(|a| a.values.clone()).collect()
}

#[test]
fn tells_the_installer_formats_apart() {
    assert_eq!(
        installer_format(&bytes(&inheriting())).unwrap(),
        InstallerFormat::Legacy
    );
    let processors = json!({"spec": 0, "json": "/version.json", "minecraft": "1.12.2"});
    assert_eq!(
        installer_format(&bytes(&processors)).unwrap(),
        InstallerFormat::Processors
    );
    for bad in [&b"[]"[..], b"{}", b"not json", b"{\"install\": {}}"] {
        assert_eq!(
            installer_format(bad).unwrap_err().kind,
            ErrorKind::Integrity
        );
    }
}

#[test]
fn reads_an_inheriting_profile() {
    let installer = LegacyInstaller::parse(&bytes(&inheriting())).unwrap();
    assert!(!installer.strip_meta);
    assert_eq!(
        installer.file_path,
        "forge-1.7.10-10.13.4.1614-1.7.10-universal.jar"
    );
    assert_eq!(
        installer.library_path().unwrap(),
        "net/minecraftforge/forge/1.7.10-10.13.4.1614-1.7.10/forge-1.7.10-10.13.4.1614-1.7.10.jar"
    );
    let p = installer.profile("1.7.10", "10.13.4.1614").unwrap();
    assert_eq!(p.id, "1.7.10-Forge10.13.4.1614-1.7.10");
    assert_eq!(p.loader_version, "10.13.4.1614");
    assert_eq!(p.inherits_from, "1.7.10");
    assert_eq!(p.main_class, "net.minecraft.launchwrapper.Launch");
    assert_eq!(p.jar, None, "the vanilla jar is used as it is");
    assert!(p.replaces_game_arguments);
    assert!(p.arguments.jvm.is_empty());
    assert!(game_args(&p.arguments.game).ends_with(&[
        "--tweakClass".to_owned(),
        "org.example.fml.Tweaker".to_owned()
    ]));
    assert_eq!(
        names(&p.libraries),
        [
            "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
            "net.minecraft:launchwrapper:1.12",
            "org.example:scala-lib:2.11.1",
            "org.example:old-host:1.0",
            "org.example:no-checksum:1.0",
            "com.google.guava:guava:17.0"
        ]
    );
    // The carried jar has no address; it is the only local library.
    let local = local_libraries(&p.libraries).unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].path, installer.library_path().unwrap());
    // The first checksum is the jar's; old addresses move to the Forge repository.
    let scala = artifact(&p.libraries, "org.example:scala-lib:2.11.1");
    assert_eq!(scala["sha1"], SHA_A);
    assert_eq!(
        scala["url"],
        "https://maven.minecraftforge.net/org/example/scala-lib/2.11.1/scala-lib-2.11.1.jar"
    );
    let old = artifact(&p.libraries, "org.example:old-host:1.0");
    assert_eq!(
        old["url"],
        "https://maven.minecraftforge.net/org/example/old-host/1.0/old-host-1.0.jar"
    );
    assert!(
        artifact(&p.libraries, "org.example:no-checksum:1.0")
            .get("sha1")
            .is_none()
    );
    assert_eq!(
        artifact(&p.libraries, "com.google.guava:guava:17.0")["url"],
        "https://libraries.minecraft.net/com/google/guava/guava/17.0/guava-17.0.jar"
    );
}

/// A vanilla 1.5.2-like version with hashed libraries.
fn vanilla_152() -> VersionJson {
    VersionJson::parse(&bytes(&json!({
        "id": "1.5.2",
        "type": "release",
        "mainClass": "net.minecraft.launchwrapper.Launch",
        "minecraftArguments": "${auth_player_name} ${auth_session}",
        "downloads": {"client": {"sha1": SHA_C, "size": 10, "url": "https://example.test/client.jar"}},
        "libraries": [
            {"name": "org.lwjgl.lwjgl:lwjgl:2.9.0", "downloads": {"artifact": {
                "path": "org/lwjgl/lwjgl/lwjgl/2.9.0/lwjgl-2.9.0.jar", "sha1": SHA_B, "size": 1,
                "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl/2.9.0/lwjgl-2.9.0.jar"}}},
            {"name": "org.ow2.asm:asm-all:4.1", "downloads": {"artifact": {
                "path": "org/ow2/asm/asm-all/4.1/asm-all-4.1.jar", "sha1": SHA_A, "size": 1,
                "url": "https://libraries.minecraft.net/org/ow2/asm/asm-all/4.1/asm-all-4.1.jar"}}}
        ]
    })))
    .unwrap()
}

#[test]
fn reads_a_standalone_profile_that_strips_the_game_jar() {
    let installer = LegacyInstaller::parse(&bytes(&standalone())).unwrap();
    assert!(installer.strip_meta);
    let p = installer.profile("1.5.2", "7.8.1.738").unwrap();
    assert_eq!(p.inherits_from, "1.5.2");
    assert_eq!(p.jar.as_deref(), Some("1.5.2-Forge7.8.1.738"));
    // Platform libraries the vanilla version lists are dropped, the repeated asm-all keeps the
    // entry from the Forge repository.
    assert_eq!(
        names(&p.libraries),
        [
            "net.minecraftforge:minecraftforge:7.8.1.738",
            "org.ow2.asm:asm-all:4.1",
            "org.example:fixer:1.0",
            "net.minecraft:launchwrapper:1.5"
        ]
    );
    assert_eq!(
        artifact(&p.libraries, "org.ow2.asm:asm-all:4.1")["sha1"],
        SHA_A
    );

    // Merged with the game: the game's LWJGL stays, the client jar is the stripped copy.
    let merged = p.merge(&vanilla_152()).unwrap();
    assert_eq!(merged.client_jar, "1.5.2-Forge7.8.1.738");
    assert!(merged.downloads.contains_key("client"));
    let lwjgl = merged
        .libraries
        .iter()
        .find(|l| l.name == "org.lwjgl.lwjgl:lwjgl:2.9.0")
        .unwrap();
    assert!(lwjgl.rest.contains_key("downloads"));
    assert_eq!(merged.main_class, "net.minecraft.launchwrapper.Launch");
    assert_eq!(
        merged.libraries[0].name,
        "net.minecraftforge:minecraftforge:7.8.1.738"
    );
    // FML of this era finds the game directory through a system property; the game arguments
    // are the profile's line, the JVM arguments the defaults plus that property.
    let target = "-Dminecraft.applet.TargetDirectory=${game_directory}".to_owned();
    assert_eq!(game_args(&p.arguments.jvm), std::slice::from_ref(&target));
    let jvm = game_args(&merged.arguments.jvm);
    assert_eq!(jvm.last(), Some(&target));
    assert!(jvm.len() > 1, "the default JVM arguments stay");
    let game = game_args(&merged.arguments.game);
    assert_eq!(game[0], "${auth_player_name}");
    assert!(game.ends_with(&[
        "--tweakClass".to_owned(),
        "org.example.legacy.FixerTweaker".to_owned()
    ]));
}

#[test]
fn rejects_damaged_legacy_profiles() {
    let installer = LegacyInstaller::parse(&bytes(&inheriting())).unwrap();
    // Another game version.
    assert_eq!(
        installer.profile("1.7.2", "10.13.4.1614").unwrap_err().kind,
        ErrorKind::Integrity
    );
    let mut wrong_parent = inheriting();
    wrong_parent["versionInfo"]["inheritsFrom"] = json!("1.7.2");
    let mut no_main = inheriting();
    no_main["versionInfo"]
        .as_object_mut()
        .unwrap()
        .remove("mainClass");
    let mut ftp = inheriting();
    ftp["versionInfo"]["libraries"][2]["url"] = json!("ftp://example.test/");
    let mut bad_name = inheriting();
    bad_name["versionInfo"]["libraries"][1]["name"] = json!("../../x:y:1");
    let mut bad_sha = inheriting();
    bad_sha["versionInfo"]["libraries"][2]["checksums"] = json!(["not-a-sha1"]);
    for doc in [wrong_parent, no_main, ftp, bad_name] {
        let installer = LegacyInstaller::parse(&bytes(&doc)).unwrap();
        assert_eq!(
            installer
                .profile("1.7.10", "10.13.4.1614")
                .unwrap_err()
                .kind,
            ErrorKind::Integrity,
            "{doc}"
        );
    }
    // A checksum that is not a SHA-1 is ignored.
    let p = LegacyInstaller::parse(&bytes(&bad_sha))
        .unwrap()
        .profile("1.7.10", "10.13.4.1614")
        .unwrap();
    assert!(
        artifact(&p.libraries, "org.example:scala-lib:2.11.1")
            .get("sha1")
            .is_none()
    );

    let mut unsafe_file = inheriting();
    unsafe_file["install"]["filePath"] = json!("../forge.jar");
    let mut unsafe_path = inheriting();
    unsafe_path["install"]["path"] = json!("a:b");
    for doc in [
        unsafe_file,
        unsafe_path,
        json!({"install": {}, "versionInfo": {}}),
    ] {
        assert_eq!(
            LegacyInstaller::parse(&bytes(&doc)).unwrap_err().kind,
            ErrorKind::Integrity,
            "{doc}"
        );
    }
    let huge = vec![b' '; 9 * 1024 * 1024];
    assert_eq!(
        LegacyInstaller::parse(&huge).unwrap_err().kind,
        ErrorKind::Integrity
    );
}

#[test]
fn reads_the_last_1_12_2_builds_without_processors() {
    let install = json!({
        "spec": 0,
        "profile": "forge",
        "version": "1.12.2-forge-14.23.5.2860",
        "path": "net.minecraftforge:forge:1.12.2-14.23.5.2860",
        "minecraft": "1.12.2",
        "json": "/version.json",
        "data": {},
        "processors": [],
        "libraries": [
            {"name": "net.minecraftforge:forge:1.12.2-14.23.5.2860", "downloads": {"artifact": {
                "path": "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
                "url": "", "sha1": SHA_A, "size": 10}}}
        ]
    });
    assert_eq!(
        installer_format(&bytes(&install)).unwrap(),
        InstallerFormat::Processors
    );
    let parsed = InstallProfile::parse(&bytes(&install)).unwrap();
    assert!(parsed.processors.is_empty());
    let version = json!({
        "id": "1.12.2-forge-14.23.5.2860",
        "inheritsFrom": "1.12.2",
        "type": "release",
        "mainClass": "net.minecraft.launchwrapper.Launch",
        "logging": {},
        "minecraftArguments": "--username ${auth_player_name} --tweakClass org.example.fml.Tweaker --versionType Forge",
        "libraries": [
            {"name": "net.minecraftforge:forge:1.12.2-14.23.5.2860", "downloads": {"artifact": {
                "path": "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
                "url": "", "sha1": SHA_A, "size": 10}}},
            {"name": "net.minecraft:launchwrapper:1.12", "downloads": {"artifact": {
                "path": "net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar",
                "url": "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar",
                "sha1": SHA_B, "size": 10}}}
        ]
    });
    let p = profile("1.12.2", "14.23.5.2860", &bytes(&version)).unwrap();
    assert!(p.replaces_game_arguments);
    assert_eq!(p.jar, None);
    assert_eq!(local_libraries(&p.libraries).unwrap().len(), 1);
}

#[test]
fn reads_the_deobfuscation_data_fml_expects() {
    let props = format!(
        "#comment\nfmlbuild.mcversion=1.5.2\nfmlbuild.deobfuscation.hash={}\n",
        SHA_A.to_uppercase()
    );
    assert_eq!(
        fml_deobfuscation_data(&props, false),
        Some(FmlLibrary {
            name: "deobfuscation_data_1.5.2.zip".to_owned(),
            sha1: SHA_A.to_owned()
        })
    );
    assert_eq!(fml_deobfuscation_data(&props, true), None);
    // A build-time placeholder, an unsafe game version, or nothing at all.
    for bad in [
        "fmlbuild.mcversion=1.6.4\nfmlbuild.deobfuscation.hash=${deobf.checksum}\n".to_owned(),
        format!("fmlbuild.mcversion=../x\nfmlbuild.deobfuscation.hash={SHA_A}\n"),
        String::new(),
    ] {
        assert_eq!(fml_deobfuscation_data(&bad, false), None, "{bad}");
    }
}
