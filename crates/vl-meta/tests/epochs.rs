//! Version epochs: one synthetic version JSON per era of Minecraft's metadata format, taken
//! through the whole pipeline (merge, libraries and natives, assets, arguments) on Windows x64,
//! macOS arm64 and Linux x64.
//!
//! The expectations below are written by hand from the format rules, not derived from the code
//! under test. The fixtures under `tests/fixtures/epochs/<epoch>/` are small, synthetic and
//! only similar in shape to upstream documents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use vl_core::platform::{Arch, Os, Platform};
use vl_meta::arguments::{Variables, resolve};
use vl_meta::assets::{AssetIndex, AssetLayout, AssetPlan};
use vl_meta::libraries::{
    LibraryPlan, Replacement, apply_replacements, classpath_separator, plan as plan_libraries,
};
use vl_meta::rules::{Features, RuleContext, feature};
use vl_meta::version::{JavaVersion, ResolvedVersion, VersionJson};

const WIN: Platform = Platform {
    os: Os::Windows,
    arch: Arch::X86_64,
};
const MAC_ARM: Platform = Platform {
    os: Os::MacOs,
    arch: Arch::Aarch64,
};
const LINUX: Platform = Platform {
    os: Os::Linux,
    arch: Arch::X86_64,
};
const PLATFORMS: [Platform; 3] = [WIN, MAC_ARM, LINUX];

const LIBS_DIR: &str = "/g/libraries";
const ASSETS_ROOT: &str = "/g/assets";
const GAME_DIR: &str = "/g/instance";
const NATIVES_DIR: &str = "/g/natives";

fn fixture(epoch: &str, file: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "epochs",
        epoch,
        file,
    ]
    .iter()
    .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// One epoch: where its fixtures are and what the pipeline must produce.
struct Epoch {
    dir: &'static str,
    /// The fixture directory of the `inheritsFrom` parent, if any.
    parent: Option<&'static str>,
    /// The directory holding `assets.json`.
    assets_dir: &'static str,
    id: &'static str,
    main_class: &'static str,
    /// `javaVersion.majorVersion`; `None` when the document has no `javaVersion`.
    java: Option<u32>,
    layout: AssetLayout,
    asset_index_id: &'static str,
    /// Consecutive game arguments that must appear (default features, user `Steve`).
    game_has: &'static [&'static [&'static str]],
    /// Whether the document is the old `minecraftArguments` string.
    legacy_args: bool,
    /// File-name suffixes of the unpacked native archives, per platform.
    natives: fn(Platform) -> Vec<&'static str>,
}

fn os_token(p: Platform) -> &'static str {
    match p.os {
        Os::Windows => "windows",
        Os::MacOs => "osx",
        _ => "linux",
    }
}

/// LWJGL 2 / JInput natives (old `natives` map, classifier `natives-<os>`).
fn lwjgl2_natives(p: Platform) -> Vec<&'static str> {
    match p.os {
        Os::Windows => vec!["natives-windows.jar", "natives-windows.jar"],
        Os::MacOs => vec!["natives-osx.jar", "natives-osx.jar"],
        _ => vec!["natives-linux.jar", "natives-linux.jar"],
    }
}

fn lwjgl2_plus_twitch(p: Platform) -> Vec<&'static str> {
    let mut v = lwjgl2_natives(p);
    if p.os == Os::Windows {
        v.push("stream-platform-6.0-natives-windows-64.jar");
    }
    v
}

fn lwjgl3_old_style(p: Platform) -> Vec<&'static str> {
    match p.os {
        Os::Windows => vec!["lwjgl-3.2.2-natives-windows.jar"],
        Os::MacOs => vec!["lwjgl-3.2.2-natives-macos.jar"],
        _ => vec!["lwjgl-3.2.2-natives-linux.jar"],
    }
}

fn no_unpacked_natives(_: Platform) -> Vec<&'static str> {
    Vec::new()
}

const MODERN_GAME: &[&[&str]] = &[
    &["--username", "Steve"],
    &["--assetIndex"],
    &["--accessToken", "tok-123"],
    &["--versionType", "release"],
];

fn epochs() -> Vec<Epoch> {
    vec![
        Epoch {
            dir: "pre-classic",
            parent: None,
            assets_dir: "pre-classic",
            id: "rd-synth-1",
            main_class: "net.minecraft.client.Minecraft",
            java: None,
            layout: AssetLayout::Resources,
            asset_index_id: "pre-1.6",
            game_has: &[&["Steve", "sess-456"]],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
        Epoch {
            dir: "alpha",
            parent: None,
            assets_dir: "alpha",
            id: "a1.2.6-synth",
            main_class: "net.minecraft.client.Minecraft",
            java: None,
            layout: AssetLayout::Resources,
            asset_index_id: "pre-1.6",
            game_has: &[&[
                "Steve",
                "sess-456",
                "--gameDir",
                "/g/instance",
                "--assetsDir",
            ]],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
        Epoch {
            dir: "beta",
            parent: None,
            assets_dir: "beta",
            id: "b1.7.3-synth",
            main_class: "net.minecraft.client.Minecraft",
            java: None,
            layout: AssetLayout::Resources,
            asset_index_id: "pre-1.6",
            game_has: &[&["--username", "Steve", "--session", "sess-456"]],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
        Epoch {
            dir: "1.5.2",
            parent: None,
            assets_dir: "1.5.2",
            id: "1.5.2-synth",
            main_class: "net.minecraft.client.Minecraft",
            java: None,
            layout: AssetLayout::Resources,
            asset_index_id: "pre-1.6",
            game_has: &[&["--version", "1.5.2-synth", "--gameDir", "/g/instance"]],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
        Epoch {
            dir: "1.7.10",
            parent: None,
            assets_dir: "1.7.10",
            id: "1.7.10-synth",
            main_class: "net.minecraft.client.main.Main",
            java: None,
            layout: AssetLayout::Virtual,
            asset_index_id: "1.7.10",
            game_has: &[
                &["--assetIndex", "1.7.10"],
                &["--accessToken", "tok-123"],
                &["--userProperties", "{}", "--userType", "msa"],
            ],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
        Epoch {
            dir: "1.12.2",
            parent: None,
            assets_dir: "1.12.2",
            id: "1.12.2-synth",
            main_class: "net.minecraft.client.main.Main",
            java: Some(8),
            layout: AssetLayout::Objects,
            asset_index_id: "1.12",
            game_has: &[&["--assetIndex", "1.12"], &["--versionType", "release"]],
            legacy_args: true,
            natives: lwjgl2_plus_twitch,
        },
        Epoch {
            dir: "1.16.5",
            parent: None,
            assets_dir: "1.16.5",
            id: "1.16.5-synth",
            main_class: "net.minecraft.client.main.Main",
            java: Some(8),
            layout: AssetLayout::Objects,
            asset_index_id: "1.16",
            game_has: MODERN_GAME,
            legacy_args: false,
            natives: lwjgl3_old_style,
        },
        Epoch {
            dir: "1.20.4",
            parent: None,
            assets_dir: "1.20.4",
            id: "1.20.4-synth",
            main_class: "net.minecraft.client.main.Main",
            java: Some(17),
            layout: AssetLayout::Objects,
            asset_index_id: "12",
            game_has: MODERN_GAME,
            legacy_args: false,
            natives: no_unpacked_natives,
        },
        Epoch {
            dir: "26.x",
            parent: None,
            assets_dir: "26.x",
            id: "26.3-synth",
            main_class: "net.minecraft.client.main.Main",
            java: Some(25),
            layout: AssetLayout::Objects,
            asset_index_id: "30",
            game_has: MODERN_GAME,
            legacy_args: false,
            natives: no_unpacked_natives,
        },
        Epoch {
            dir: "snapshot",
            parent: None,
            assets_dir: "snapshot",
            id: "26.4-snapshot-6-synth",
            main_class: "net.minecraft.client.main.Main",
            java: Some(25),
            layout: AssetLayout::Objects,
            asset_index_id: "31",
            game_has: &[&["--username", "Steve"], &["--versionType", "snapshot"]],
            legacy_args: false,
            natives: no_unpacked_natives,
        },
        Epoch {
            dir: "inherits-modern",
            parent: Some("1.20.4"),
            assets_dir: "1.20.4",
            id: "fabricish-loader-0.20.1-1.20.4",
            main_class: "net.example.loader.launch.KnotClient",
            java: Some(17),
            layout: AssetLayout::Objects,
            asset_index_id: "12",
            game_has: MODERN_GAME,
            legacy_args: false,
            natives: no_unpacked_natives,
        },
        Epoch {
            dir: "inherits-legacy",
            parent: Some("1.7.10"),
            assets_dir: "1.7.10",
            id: "1.7.10-forgeish-10.13.4",
            main_class: "net.minecraft.launchwrapper.Launch",
            java: None,
            layout: AssetLayout::Virtual,
            asset_index_id: "1.7.10",
            game_has: &[
                &["--assetIndex", "1.7.10"],
                &["--tweakClass", "example.loader.tweak.TweakerClient"],
            ],
            legacy_args: true,
            natives: lwjgl2_natives,
        },
    ]
}

fn resolve_epoch(e: &Epoch) -> ResolvedVersion {
    let version = VersionJson::parse(&fixture(e.dir, "version.json")).unwrap();
    match e.parent {
        None => version.resolve_standalone().unwrap(),
        Some(parent) => version
            .resolve(|_| Ok(VersionJson::parse(&fixture(parent, "version.json")).unwrap()))
            .unwrap(),
    }
}

/// Everything the pipeline produces for one epoch on one platform.
struct Launch {
    resolved: ResolvedVersion,
    libs: LibraryPlan,
    assets: AssetPlan,
    classpath: String,
    jvm: Vec<String>,
    game: Vec<String>,
}

impl Launch {
    /// The complete command line after the Java executable.
    fn command(&self) -> Vec<String> {
        let mut c = self.jvm.clone();
        c.push(self.resolved.main_class.clone());
        c.extend(self.game.iter().cloned());
        c
    }
}

fn variables(resolved: &ResolvedVersion, layout: AssetLayout, classpath: &str) -> Variables {
    let index_id = resolved.asset_index_id();
    // `game_assets` is where pre-1.6 and virtual layouts are read from.
    let game_assets = match layout {
        AssetLayout::Objects => format!("{ASSETS_ROOT}/objects"),
        AssetLayout::Virtual => format!("{ASSETS_ROOT}/virtual/{index_id}"),
        AssetLayout::Resources => format!("{GAME_DIR}/resources"),
    };
    Variables::new()
        .with("auth_player_name", "Steve")
        .with("auth_session", "sess-456")
        .with("auth_uuid", "00000000-0000-0000-0000-000000000001")
        .with("auth_access_token", "tok-123")
        .with("user_type", "msa")
        .with("user_properties", "{}")
        .with("version_name", resolved.id.clone())
        .with("version_type", resolved.kind.clone())
        .with("game_directory", GAME_DIR)
        .with("assets_root", ASSETS_ROOT)
        .with("assets_index_name", index_id)
        .with("game_assets", game_assets)
        .with("natives_directory", NATIVES_DIR)
        .with("launcher_name", "vibelauncher")
        .with("launcher_version", "0.0.0")
        .with("classpath", classpath)
        .with("resolution_width", "854")
        .with("resolution_height", "480")
        .with("quickPlayPath", "/g/quickplay.json")
        .with("quickPlaySingleplayer", "World")
        .with("quickPlayMultiplayer", "play.example.invalid")
}

fn run_with(
    e: &Epoch,
    platform: Platform,
    features: &Features,
    replacements: &[Replacement],
) -> Launch {
    let mut resolved = resolve_epoch(e);
    resolved.libraries = apply_replacements(&resolved.libraries, replacements, platform);
    let ctx = RuleContext::new(platform, features);
    let libs = plan_libraries(&resolved.libraries, &ctx).unwrap();

    let index = AssetIndex::parse(&fixture(e.assets_dir, "assets.json")).unwrap();
    let assets = index.plan(resolved.asset_index_id()).unwrap();

    let client = format!(
        "{GAME_DIR}/versions/{0}/{0}.jar",
        resolved.client_jar.as_str()
    );
    let classpath = libs.classpath_string(
        LIBS_DIR,
        &[client.as_str()],
        classpath_separator(platform.os),
    );
    let vars = variables(&resolved, assets.layout, &classpath);
    let jvm = resolve(&resolved.arguments.jvm, &ctx, &vars);
    let game = resolve(&resolved.arguments.game, &ctx, &vars);
    Launch {
        resolved,
        libs,
        assets,
        classpath,
        jvm,
        game,
    }
}

fn run(e: &Epoch, platform: Platform) -> Launch {
    run_with(e, platform, &Features::new(), &[])
}

fn contains_seq(haystack: &[String], needle: &[&str]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[test]
fn every_epoch_resolves_with_the_expected_identity() {
    for e in epochs() {
        let r = resolve_epoch(&e);
        assert_eq!(r.id, e.id, "{}", e.dir);
        assert_eq!(r.main_class, e.main_class, "{}", e.dir);
        assert_eq!(
            r.java_version.as_ref().map(|j| j.major_version),
            e.java,
            "{}",
            e.dir
        );
        if e.java.is_none() {
            assert_eq!(r.java_version_or_default(), JavaVersion::legacy_default());
        }
        assert_eq!(r.asset_index_id(), e.asset_index_id, "{}", e.dir);
        assert!(!r.libraries.is_empty(), "{}", e.dir);
        assert!(r.downloads.contains_key("client"), "{}", e.dir);
    }
}

#[test]
fn argument_format_matches_the_epoch() {
    for e in epochs() {
        let doc = VersionJson::parse(&fixture(e.dir, "version.json")).unwrap();
        assert_eq!(
            doc.minecraft_arguments.is_some(),
            e.legacy_args,
            "{}",
            e.dir
        );
        if e.dir.starts_with("inherits") {
            continue;
        }
        assert_eq!(doc.arguments.is_some(), !e.legacy_args, "{}", e.dir);
    }
}

#[test]
fn natives_are_the_right_ones_for_each_os() {
    for e in epochs() {
        for p in PLATFORMS {
            let l = run(&e, p);
            let want = (e.natives)(p);
            let got: Vec<&str> = l
                .libs
                .natives
                .iter()
                .map(|n| file_name(&n.file.path))
                .collect();
            assert_eq!(got.len(), want.len(), "{} on {p:?}: {got:?}", e.dir);
            for suffix in &want {
                assert!(
                    got.iter().any(|g| g.ends_with(suffix)),
                    "{} on {p:?}: no {suffix} in {got:?}",
                    e.dir
                );
            }
            // No native archive for another OS, and the pipeline reports nothing missing.
            for g in &got {
                for other in ["windows", "osx", "macos", "linux"] {
                    let own = other == os_token(p) || (p.os == Os::MacOs && other == "macos");
                    assert!(own || !g.contains(other), "{} on {p:?}: {g}", e.dir);
                }
            }
            assert!(l.libs.missing_natives.is_empty(), "{} on {p:?}", e.dir);
        }
    }
}

#[test]
fn classpath_has_no_unpacked_archive_and_no_foreign_natives() {
    for e in epochs() {
        for p in PLATFORMS {
            let l = run(&e, p);
            assert!(!l.libs.classpath.is_empty(), "{} on {p:?}", e.dir);
            for n in &l.libs.natives {
                assert!(
                    !l.libs.classpath.contains(&n.file.path),
                    "{} on {p:?}: {} is both unpacked and on the classpath",
                    e.dir,
                    n.file.path
                );
            }
            // Every downloaded file is either on the classpath or an unpacked archive.
            for d in &l.libs.downloads {
                let on_cp = l.libs.classpath.contains(&d.path);
                let unpacked = l.libs.natives.iter().any(|n| n.file.path == d.path);
                assert!(on_cp || unpacked, "{} on {p:?}: stray {}", e.dir, d.path);
            }
            let want = match p.os {
                Os::Windows => "natives-windows",
                Os::MacOs => "natives-macos-arm64",
                _ => "natives-linux",
            };
            for jar in l.libs.classpath.iter().filter(|j| j.contains("natives-")) {
                assert!(
                    file_name(jar).ends_with(&format!("{want}.jar")),
                    "{} on {p:?}: {jar}",
                    e.dir
                );
            }
        }
    }
}

#[test]
fn natives_in_jars_are_on_the_classpath_of_modern_epochs() {
    for dir in ["1.20.4", "26.x", "snapshot"] {
        let e = epochs().into_iter().find(|e| e.dir == dir).unwrap();
        for (p, suffix) in [
            (WIN, "natives-windows.jar"),
            (MAC_ARM, "natives-macos-arm64.jar"),
            (LINUX, "natives-linux.jar"),
        ] {
            let l = run(&e, p);
            let native_jars: Vec<&String> = l
                .libs
                .classpath
                .iter()
                .filter(|j| j.contains("natives-"))
                .collect();
            // Four LWJGL modules, one natives jar each.
            assert_eq!(native_jars.len(), 4, "{dir} on {p:?}");
            assert!(native_jars.iter().all(|j| j.ends_with(suffix)), "{dir}");
            assert!(l.libs.natives.is_empty(), "{dir} on {p:?}");
        }
    }
}

#[test]
fn modern_natives_follow_the_architecture() {
    let e = epochs().into_iter().find(|e| e.dir == "1.20.4").unwrap();
    let cases = [
        (Os::Windows, Arch::Aarch64, "natives-windows-arm64.jar"),
        (Os::Windows, Arch::X86, "natives-windows-x86.jar"),
        (Os::MacOs, Arch::X86_64, "natives-macos.jar"),
        (Os::Linux, Arch::Aarch64, "natives-linux-arm64.jar"),
    ];
    for (os, arch, suffix) in cases {
        let l = run(&e, Platform { os, arch });
        let jars: Vec<&String> = l
            .libs
            .classpath
            .iter()
            .filter(|j| j.contains("natives-"))
            .collect();
        assert_eq!(jars.len(), 4, "{os:?} {arch:?}");
        assert!(jars.iter().all(|j| j.ends_with(suffix)), "{os:?} {arch:?}");
    }
}

#[test]
fn old_natives_keep_the_extract_exclusions() {
    let e = epochs().into_iter().find(|e| e.dir == "beta").unwrap();
    let l = run(&e, WIN);
    assert!(!l.libs.natives.is_empty());
    for n in &l.libs.natives {
        assert_eq!(n.exclude, vec!["META-INF/".to_owned()]);
        assert_eq!(n.target("lwjgl.dll").as_deref(), Some("lwjgl.dll"));
        assert_eq!(n.target("META-INF/MANIFEST.MF"), None);
        assert_eq!(n.target("../evil.dll"), None);
    }
}

#[test]
fn twelve_two_arch_placeholder_picks_the_64_bit_classifier() {
    let e = epochs().into_iter().find(|e| e.dir == "1.12.2").unwrap();
    let on_win = run(&e, WIN);
    assert!(on_win.libs.natives.iter().any(|n| {
        n.file
            .path
            .ends_with("stream-platform-6.0-natives-windows-64.jar")
    }));
    // The library only declares Windows natives, and has no classpath jar of its own.
    for p in [MAC_ARM, LINUX] {
        let l = run(&e, p);
        assert!(
            l.libs
                .natives
                .iter()
                .all(|n| !n.file.path.contains("stream-platform"))
        );
        assert!(
            l.libs
                .classpath
                .iter()
                .all(|j| !j.contains("stream-platform"))
        );
    }
    let x86 = run(
        &e,
        Platform {
            os: Os::Windows,
            arch: Arch::X86,
        },
    );
    assert!(x86.libs.natives.iter().any(|n| {
        n.file
            .path
            .ends_with("stream-platform-6.0-natives-windows-32.jar")
    }));
}

#[test]
fn lwjgl2_on_apple_silicon_needs_a_replacement() {
    let e = epochs().into_iter().find(|e| e.dir == "beta").unwrap();
    // Without data the x86-only natives are selected: the plan itself does not guess.
    let plain = run(&e, MAC_ARM);
    assert!(
        plain
            .libs
            .natives
            .iter()
            .all(|n| n.file.path.ends_with("natives-osx.jar"))
    );

    let replacements: Vec<Replacement> =
        serde_json::from_slice(&fixture("beta", "replacements.json")).unwrap();
    let l = run_with(&e, MAC_ARM, &Features::new(), &replacements);
    let natives: Vec<&str> = l
        .libs
        .natives
        .iter()
        .map(|n| file_name(&n.file.path))
        .collect();
    assert_eq!(
        natives,
        vec![
            "lwjgl-platform-2.9.4-arm64-natives-osx-arm64.jar",
            "jinput-platform-2.0.5-arm64-natives-osx-arm64.jar"
        ]
    );
    // The jars that are not platform-specific stay.
    assert!(
        l.libs
            .classpath
            .iter()
            .any(|j| j.ends_with("lwjgl-2.9.1.jar"))
    );
    // The replacement is for macOS arm64 only.
    let win = run_with(&e, WIN, &Features::new(), &replacements);
    assert!(
        win.libs
            .natives
            .iter()
            .all(|n| n.file.path.ends_with("natives-windows.jar"))
    );
}

#[test]
fn assets_use_the_layout_of_the_epoch() {
    for e in epochs() {
        let l = run(&e, LINUX);
        let a = &l.assets;
        assert_eq!(a.layout, e.layout, "{}", e.dir);
        // Four index entries, two of them share a hash.
        assert_eq!(a.total_count, 4, "{}", e.dir);
        assert_eq!(a.downloads.len(), 3, "{}", e.dir);
        match e.layout {
            AssetLayout::Objects => assert!(a.restores.is_empty(), "{}", e.dir),
            AssetLayout::Virtual => {
                assert_eq!(a.restores.len(), 4, "{}", e.dir);
                let prefix = format!("virtual/{}/", e.asset_index_id);
                assert!(a.restores.iter().all(|r| r.target.starts_with(&prefix)));
            }
            AssetLayout::Resources => {
                assert_eq!(a.restores.len(), 4, "{}", e.dir);
                assert!(
                    a.restores
                        .iter()
                        .all(|r| r.target.starts_with("resources/"))
                );
            }
        }
        for r in &a.restores {
            assert!(r.source.starts_with("objects/"), "{}", e.dir);
        }
        // Objects are fetched by hash from the resources host.
        for d in &a.downloads {
            assert!(d.url().ends_with(&format!("/{}/{}", &d.hash[..2], d.hash)));
        }
    }
}

#[test]
fn asset_modes_are_the_ones_expected_by_era() {
    let by_dir = |d: &str| epochs().into_iter().find(|e| e.dir == d).unwrap().layout;
    for d in ["pre-classic", "alpha", "beta", "1.5.2"] {
        assert_eq!(by_dir(d), AssetLayout::Resources, "{d}");
    }
    assert_eq!(by_dir("1.7.10"), AssetLayout::Virtual);
    for d in ["1.12.2", "1.16.5", "1.20.4", "26.x", "snapshot"] {
        assert_eq!(by_dir(d), AssetLayout::Objects, "{d}");
    }
}

#[test]
fn arguments_are_substituted_completely_on_every_platform() {
    for e in epochs() {
        for p in PLATFORMS {
            let l = run(&e, p);
            let cmd = l.command();
            assert!(
                cmd.iter().all(|a| !a.contains("${")),
                "{} on {p:?}: {cmd:?}",
                e.dir
            );
            // The main class sits between the JVM and the game arguments.
            let at = cmd.iter().position(|a| a == e.main_class).unwrap();
            assert_eq!(at, l.jvm.len(), "{} on {p:?}", e.dir);
            assert_eq!(cmd.len(), l.jvm.len() + 1 + l.game.len());
            for needle in e.game_has {
                assert!(
                    contains_seq(&l.game, needle),
                    "{} on {p:?}: {needle:?} not in {:?}",
                    e.dir,
                    l.game
                );
            }
            assert!(
                contains_seq(&l.jvm, &["-cp", l.classpath.as_str()]),
                "{} on {p:?}",
                e.dir
            );
            assert!(
                l.jvm
                    .contains(&format!("-Djava.library.path={NATIVES_DIR}")),
                "{} on {p:?}",
                e.dir
            );
            assert!(
                l.jvm
                    .contains(&"-Dminecraft.launcher.brand=vibelauncher".to_owned())
            );
        }
    }
}

#[test]
fn classpath_is_joined_with_the_os_separator_and_ends_with_the_client_jar() {
    for e in epochs() {
        for p in PLATFORMS {
            let l = run(&e, p);
            let sep = if p.os == Os::Windows { ';' } else { ':' };
            let parts: Vec<&str> = l.classpath.split(sep).collect();
            assert_eq!(
                parts.len(),
                l.libs.classpath.len() + 1,
                "{} on {p:?}",
                e.dir
            );
            let jar = &l.resolved.client_jar;
            assert_eq!(
                parts.last().copied(),
                Some(format!("{GAME_DIR}/versions/{jar}/{jar}.jar").as_str()),
                "{} on {p:?}",
                e.dir
            );
            assert!(parts[0].starts_with(LIBS_DIR));
        }
    }
}

#[test]
fn platform_jvm_arguments_follow_the_rules() {
    for e in epochs().into_iter().filter(|e| !e.legacy_args) {
        let mac = run(&e, MAC_ARM).jvm;
        let win = run(&e, WIN).jvm;
        let lin = run(&e, LINUX).jvm;
        assert!(
            mac.contains(&"-XstartOnFirstThread".to_owned()),
            "{}",
            e.dir
        );
        assert!(
            !win.contains(&"-XstartOnFirstThread".to_owned()),
            "{}",
            e.dir
        );
        assert!(
            !lin.contains(&"-XstartOnFirstThread".to_owned()),
            "{}",
            e.dir
        );
        let heap = |v: &[String]| v.iter().any(|a| a.starts_with("-XX:HeapDumpPath="));
        assert!(heap(&win), "{}", e.dir);
        assert!(!heap(&mac) && !heap(&lin), "{}", e.dir);
    }
}

#[test]
fn legacy_epochs_get_the_default_jvm_arguments() {
    for e in epochs().into_iter().filter(|e| e.legacy_args) {
        for p in PLATFORMS {
            let l = run(&e, p);
            assert_eq!(
                l.jvm,
                vec![
                    format!("-Djava.library.path={NATIVES_DIR}"),
                    "-Dminecraft.launcher.brand=vibelauncher".to_owned(),
                    "-Dminecraft.launcher.version=0.0.0".to_owned(),
                    "-cp".to_owned(),
                    l.classpath.clone(),
                ],
                "{} on {p:?}",
                e.dir
            );
        }
    }
}

#[test]
fn legacy_asset_arguments_point_at_the_layout() {
    let find = |d: &str| epochs().into_iter().find(|e| e.dir == d).unwrap();
    let l = run(&find("beta"), LINUX);
    assert!(contains_seq(
        &l.game,
        &["--assetsDir", "/g/instance/resources"]
    ));
    let l = run(&find("1.5.2"), LINUX);
    assert!(contains_seq(
        &l.game,
        &["--assetsDir", "/g/instance/resources"]
    ));
    let l = run(&find("1.7.10"), LINUX);
    assert!(contains_seq(&l.game, &["--assetsDir", "/g/assets"]));
    assert!(contains_seq(&l.game, &["--assetIndex", "1.7.10"]));
}

#[test]
fn optional_game_arguments_follow_the_features() {
    for dir in ["1.16.5", "1.20.4", "26.x", "snapshot"] {
        let e = epochs().into_iter().find(|e| e.dir == dir).unwrap();
        let off = run(&e, LINUX).game;
        assert!(!off.contains(&"--demo".to_owned()), "{dir}");
        assert!(!off.contains(&"--width".to_owned()), "{dir}");

        let features = Features::new()
            .with(feature::DEMO)
            .with(feature::CUSTOM_RESOLUTION);
        let on = run_with(&e, LINUX, &features, &[]).game;
        assert!(on.contains(&"--demo".to_owned()), "{dir}");
        assert!(
            contains_seq(&on, &["--width", "854", "--height", "480"]),
            "{dir}"
        );
    }
    for dir in ["1.20.4", "26.x", "snapshot"] {
        let e = epochs().into_iter().find(|e| e.dir == dir).unwrap();
        let features = Features::new()
            .with(feature::QUICK_PLAYS_SUPPORT)
            .with(feature::QUICK_PLAY_SINGLEPLAYER);
        let on = run_with(&e, LINUX, &features, &[]).game;
        assert!(
            contains_seq(&on, &["--quickPlayPath", "/g/quickplay.json"]),
            "{dir}"
        );
        assert!(
            contains_seq(&on, &["--quickPlaySingleplayer", "World"]),
            "{dir}"
        );
        assert!(!on.contains(&"--quickPlayMultiplayer".to_owned()), "{dir}");
    }
}

#[test]
fn features_do_not_change_legacy_arguments() {
    let features = Features::new()
        .with(feature::DEMO)
        .with(feature::CUSTOM_RESOLUTION);
    for e in epochs().into_iter().filter(|e| e.legacy_args) {
        assert_eq!(
            run(&e, WIN).game,
            run_with(&e, WIN, &features, &[]).game,
            "{}",
            e.dir
        );
    }
}

#[test]
fn logging_is_present_from_the_epoch_that_has_it() {
    let find = |d: &str| resolve_epoch(&epochs().into_iter().find(|e| e.dir == d).unwrap());
    for d in ["pre-classic", "alpha", "beta", "1.5.2", "1.7.10"] {
        assert!(find(d).logging.is_none(), "{d}");
    }
    for d in ["1.12.2", "1.20.4", "26.x", "snapshot"] {
        assert!(find(d).logging.is_some(), "{d}");
    }
}

#[test]
fn new_numbering_and_snapshots_are_ordinary_versions() {
    let find = |d: &str| resolve_epoch(&epochs().into_iter().find(|e| e.dir == d).unwrap());
    let latest = find("26.x");
    assert!(latest.id.starts_with("26."));
    assert_eq!(latest.kind, "release");
    assert!(latest.downloads.contains_key("client_mappings"));
    let snap = find("snapshot");
    assert_eq!(snap.kind, "snapshot");
    assert!(snap.id.contains("snapshot"));
    // A snapshot-only library is planned like any other.
    let l = run(
        &epochs().into_iter().find(|e| e.dir == "snapshot").unwrap(),
        LINUX,
    );
    assert!(
        l.libs
            .classpath
            .iter()
            .any(|j| j.contains("new-snapshot-lib"))
    );
}

#[test]
fn inherited_modern_profile_merges_with_its_parent() {
    let e = epochs()
        .into_iter()
        .find(|e| e.dir == "inherits-modern")
        .unwrap();
    for p in PLATFORMS {
        let l = run(&e, p);
        let r = &l.resolved;
        assert_eq!(r.chain, vec![e.id.to_owned(), "1.20.4-synth".to_owned()]);
        assert_eq!(r.client_jar, "1.20.4-synth");
        assert_eq!(r.release_time, "2026-01-05T00:00:00+00:00");
        // The child's library replaces the parent's of the same coordinate (other version).
        assert!(
            l.libs
                .classpath
                .iter()
                .any(|j| j.ends_with("brigadier-1.3.0.jar"))
        );
        assert!(
            l.libs
                .classpath
                .iter()
                .all(|j| !j.ends_with("brigadier-1.2.9.jar"))
        );
        // Loader libraries are derived from the coordinate and their own repository.
        let loader = l
            .libs
            .downloads
            .iter()
            .find(|d| d.path == "net/example/loader/0.20.1/loader-0.20.1.jar")
            .unwrap();
        assert_eq!(
            loader.url.as_deref(),
            Some("https://maven.example.invalid/net/example/loader/0.20.1/loader-0.20.1.jar")
        );
        assert_eq!(loader.sha1, None);
        // Parent arguments and platform libraries survive; the child's JVM argument is added.
        assert!(l.jvm.contains(&"-DloaderMarker=1".to_owned()));
        assert!(contains_seq(&l.game, &["--username", "Steve"]));
        assert_eq!(
            l.libs
                .classpath
                .iter()
                .filter(|j| j.contains("natives-"))
                .count(),
            4
        );
        assert_eq!(r.java_version.as_ref().map(|j| j.major_version), Some(17));
        // The child's main class comes first on the command line.
        assert_eq!(
            l.command()[l.jvm.len()],
            "net.example.loader.launch.KnotClient"
        );
    }
}

#[test]
fn inherited_legacy_profile_replaces_arguments_and_keeps_the_parent_jar() {
    let e = epochs()
        .into_iter()
        .find(|e| e.dir == "inherits-legacy")
        .unwrap();
    for p in PLATFORMS {
        let l = run(&e, p);
        let r = &l.resolved;
        assert_eq!(r.client_jar, "1.7.10-synth");
        assert_eq!(r.asset_index_id(), "1.7.10");
        assert!(
            l.libs
                .classpath
                .iter()
                .any(|j| j.ends_with("guava-16.0.jar"))
        );
        assert!(
            l.libs
                .classpath
                .iter()
                .all(|j| !j.ends_with("guava-15.0.jar"))
        );
        assert!(
            l.libs
                .classpath
                .iter()
                .any(|j| j.ends_with("launchwrapper-1.12.jar"))
        );
        // Natives of the parent are still unpacked for this OS.
        assert_eq!(l.libs.natives.len(), 2, "{p:?}");
        // The child's game arguments replace the parent's (no duplicated flags).
        assert_eq!(l.game.iter().filter(|a| *a == "--username").count(), 1);
        assert_eq!(l.game.iter().filter(|a| *a == "--tweakClass").count(), 1);
        // The legacy JVM defaults still apply.
        assert!(contains_seq(&l.jvm, &["-cp", l.classpath.as_str()]));
    }
}

#[test]
fn missing_parent_is_an_error_not_a_panic() {
    let child = VersionJson::parse(&fixture("inherits-modern", "version.json")).unwrap();
    let err = child.resolve_standalone().unwrap_err();
    assert_eq!(err.kind, vl_core::error::ErrorKind::NotFound);
}

#[test]
fn unusual_platforms_do_not_panic() {
    let odd = [
        Platform {
            os: Os::Linux,
            arch: Arch::Aarch64,
        },
        Platform {
            os: Os::Windows,
            arch: Arch::X86,
        },
        Platform {
            os: Os::MacOs,
            arch: Arch::X86_64,
        },
        Platform {
            os: Os::Other,
            arch: Arch::Other,
        },
    ];
    for e in epochs() {
        for p in odd {
            let l = run(&e, p);
            let cmd = l.command();
            assert!(cmd.iter().any(|a| a == e.main_class), "{} on {p:?}", e.dir);
        }
    }
}
