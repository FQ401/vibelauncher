#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use serde_json::json;
use vl_core::ErrorKind;
use vl_core::java::JavaRuntime;
use vl_core::platform::Arch;
use vl_core::settings::{MemorySettings, Settings, WindowSettings};
use vl_launch::command::{self, CommandInput};
use vl_launch::{Installed, Installer, Job, LaunchSession, PrepareInput, Secret, prepare};

const TOKEN: &str = "tok-SECRET-123";

fn session() -> LaunchSession {
    LaunchSession {
        player_name: "Steve".into(),
        uuid: "11111111-2222-3333-4444-555555555555".into(),
        access_token: Secret::new(TOKEN),
        user_type: "msa".into(),
        xuid: Some("42".into()),
        client_id: None,
    }
}

fn setup(
    index_id: &str,
    flags: serde_json::Value,
    extra: serde_json::Value,
) -> (TempDir, Installer, Installed) {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", index_id, flags, extra);
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let game = dir.path().join("game");
    let installed = installer
        .install("1.0", Some(&game), &Job::new())
        .expect("install");
    (dir, installer, installed)
}

fn build(
    installer: &Installer,
    installed: &Installed,
    game: &std::path::Path,
    extra: &[String],
) -> command::CommandLine {
    command::build(&CommandInput {
        installed,
        layout: installer.layout(),
        java: &PathBuf::from("java"),
        game_dir: game,
        session: &session(),
        memory: MemorySettings {
            min_mib: 512,
            max_mib: 2048,
            ..MemorySettings::default()
        },
        window: WindowSettings {
            width: 1000,
            height: 600,
            maximized: false,
            ..WindowSettings::default()
        },
        extra_jvm_args: extra,
        env: &std::collections::BTreeMap::new(),
        launcher_name: "VibeLauncher",
        launcher_version: "1.2.3",
    })
    .expect("command")
}

fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let i = args.iter().position(|a| a == flag).expect(flag);
    &args[i + 1]
}

#[test]
fn the_modern_format_gets_memory_classpath_natives_and_game_arguments() {
    let (dir, installer, installed) = setup("1.0", json!({}), json!({}));
    let game = dir.path().join("game");
    let line = build(&installer, &installed, &game, &["-Dextra=1".to_owned()]);
    let args = &line.args;

    // JVM part: memory first, user flags, then the version's.
    assert_eq!(args[0], "-Xms512M");
    assert_eq!(args[1], "-Xmx2048M");
    assert_eq!(args[2], "-Dextra=1");
    assert!(args.contains(&"-Dminecraft.launcher.brand=VibeLauncher".to_owned()));
    assert!(args.iter().any(|a| a.starts_with("-XX:HeapDumpPath"))); // the windows rule
    assert!(!args.contains(&"-XstartOnFirstThread".to_owned())); // the osx rule
    let natives = installed.natives_dir.to_string_lossy().into_owned();
    assert!(args.contains(&format!("-Djava.library.path={natives}")));
    let classpath = value_after(args, "-cp");
    let parts: Vec<&str> = classpath.split(';').collect();
    assert!(
        parts[0]
            .replace('\\', "/")
            .ends_with("libraries/com/example/lib/1.0/lib-1.0.jar")
    );
    assert_eq!(PathBuf::from(parts[1]), installed.client_jar);
    assert_eq!(parts.len(), 2);

    // Logging, then the main class, then the game arguments.
    let log_arg = args
        .iter()
        .position(|a| a.starts_with("-Dlog4j.configurationFile="))
        .unwrap();
    let main = args.iter().position(|a| a == "net.example.Main").unwrap();
    assert!(log_arg < main);
    let game_args = &args[main + 1..];
    assert_eq!(value_after(game_args, "--username"), "Steve");
    assert_eq!(value_after(game_args, "--version"), "1.0");
    assert_eq!(PathBuf::from(value_after(game_args, "--gameDir")), game);
    assert_eq!(
        PathBuf::from(value_after(game_args, "--assetsDir")),
        installer.layout().assets_dir()
    );
    assert_eq!(value_after(game_args, "--assetIndex"), "1.0");
    assert_eq!(
        value_after(game_args, "--uuid"),
        "11111111222233334444555555555555"
    );
    assert_eq!(value_after(game_args, "--accessToken"), TOKEN);
    assert_eq!(value_after(game_args, "--userType"), "msa");
    assert_eq!(value_after(game_args, "--versionType"), "release");
    assert_eq!(value_after(game_args, "--width"), "1000");
    assert_eq!(value_after(game_args, "--height"), "600");
    assert!(!game_args.contains(&"--demo".to_owned()));
    assert_eq!(line.cwd, game);
    assert!(
        args.iter().all(|a| !a.contains("${")),
        "unresolved variable in {args:?}"
    );
}

#[test]
fn the_token_never_appears_in_anything_printable() {
    let (dir, installer, installed) = setup("1.0", json!({}), json!({}));
    let line = build(&installer, &installed, &dir.path().join("game"), &[]);
    assert!(line.args.iter().any(|a| a == TOKEN));
    assert!(!line.display_redacted().contains(TOKEN));
    assert!(line.redacted_args().iter().all(|a| !a.contains(TOKEN)));
    assert!(!format!("{line:?}").contains(TOKEN));
    assert!(!format!("{:?}", session()).contains(TOKEN));
    assert!(line.display_redacted().contains("***"));
}

#[test]
fn the_legacy_format_with_the_virtual_layout() {
    // 1.7.10 style: one `minecraftArguments` string, no `arguments`.
    let extra = json!({
        "arguments": null,
        "minecraftArguments": "--username ${auth_player_name} --session ${auth_session} --version ${version_name} --gameDir ${game_directory} --assetsDir ${game_assets} --userProperties ${user_properties}",
        "javaVersion": null,
    });
    let (dir, installer, installed) = setup("legacy", json!({"virtual": true}), extra);
    let game = dir.path().join("game");
    let line = build(&installer, &installed, &game, &[]);
    let args = &line.args;
    // The default JVM arguments are added for a legacy version.
    assert!(args.iter().any(|a| a.starts_with("-Djava.library.path=")));
    assert!(args.contains(&"-Dminecraft.launcher.version=1.2.3".to_owned()));
    assert!(args.contains(&"-cp".to_owned()));
    let main = args.iter().position(|a| a == "net.example.Main").unwrap();
    let game_args = &args[main + 1..];
    assert_eq!(
        value_after(game_args, "--session"),
        format!("token:{TOKEN}:11111111222233334444555555555555")
    );
    let assets = PathBuf::from(value_after(game_args, "--assetsDir"));
    assert_eq!(
        assets,
        installer
            .layout()
            .assets_dir()
            .join("virtual")
            .join("legacy")
    );
    assert_eq!(value_after(game_args, "--userProperties"), "{}");
    assert!(args.iter().all(|a| !a.contains("${")));
}

#[test]
fn the_pre_1_6_format_points_assets_at_the_game_directory() {
    let extra = json!({
        "arguments": null,
        "minecraftArguments": "${auth_player_name} ${auth_session}",
        "javaVersion": null,
        "logging": null,
    });
    let (dir, installer, installed) = setup("pre-1.6", json!({"map_to_resources": true}), extra);
    let game = dir.path().join("game");
    let line = build(&installer, &installed, &game, &[]);
    let main = line
        .args
        .iter()
        .position(|a| a == "net.example.Main")
        .unwrap();
    assert_eq!(line.args[main + 1], "Steve");
    assert!(line.args[main + 2].starts_with("token:"));
    assert_eq!(line.args.len(), main + 3);
    assert!(game.join("resources/lang/en.lang").is_file());
}

#[test]
fn memory_is_validated_and_min_is_capped_by_max() {
    let (dir, installer, installed) = setup("1.0", json!({}), json!({}));
    let game = dir.path().join("game");
    let mut input = CommandInput {
        installed: &installed,
        layout: installer.layout(),
        java: &PathBuf::from("java"),
        game_dir: &game,
        session: &session(),
        memory: MemorySettings {
            min_mib: 4096,
            max_mib: 1024,
            ..MemorySettings::default()
        },
        window: WindowSettings::default(),
        extra_jvm_args: &[],
        env: &std::collections::BTreeMap::new(),
        launcher_name: "L",
        launcher_version: "1",
    };
    let line = command::build(&input).expect("command");
    assert_eq!(&line.args[..2], ["-Xms1024M", "-Xmx1024M"]);
    input.memory.max_mib = 8;
    assert_eq!(
        command::build(&input).expect_err("small").kind,
        ErrorKind::InvalidInput
    );
}

fn runtime(major: u32, arch: Arch) -> JavaRuntime {
    JavaRuntime {
        path: PathBuf::from(format!("/jvm/{major}/bin/java")),
        major,
        version: major.to_string(),
        arch,
        vendor: "Test".into(),
        managed: false,
    }
}

#[test]
fn prepare_installs_chooses_java_and_warns() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let game = dir.path().join("instances/one/minecraft");
    // More than any machine has free.
    let settings = Settings {
        memory: MemorySettings {
            min_mib: 512,
            max_mib: u32::MAX,
            ..MemorySettings::default()
        },
        ..Settings::default()
    };
    let session = session();
    let runtimes = [runtime(21, Arch::X86_64), runtime(8, Arch::X86_64)];
    let input = PrepareInput {
        version: "1.0",
        game_dir: &game,
        session: &session,
        settings: &settings,
        runtimes: &runtimes,
        fixed_java: None,
        launcher_name: "VibeLauncher",
        launcher_version: "1",
    };
    let prepared = prepare(&installer, &input, &Job::new()).expect("prepare");
    // The version asks for Java 17; 21 is the lowest newer one.
    assert_eq!(prepared.java.runtime.major, 21);
    assert!(!prepared.java.exact);
    assert_eq!(prepared.command.program, PathBuf::from("/jvm/21/bin/java"));
    assert!(game.is_dir());
    assert!(prepared.warnings.iter().any(|w| w.contains("Java 17")));
    assert!(
        prepared
            .warnings
            .iter()
            .any(|w| w.contains("out of memory"))
    );

    // Only Java 8: a clear Java error, nothing started.
    let only_old = [runtime(8, Arch::X86_64)];
    let input = PrepareInput {
        runtimes: &only_old,
        ..input
    };
    let err = prepare(&installer, &input, &Job::new()).expect_err("no java");
    assert_eq!(err.kind, ErrorKind::Java);
}
