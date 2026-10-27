//! Synthetic logs: one per signature and loader, plus the levels without a signature.

use vl_crash::{CrashInput, Level, ModInfo, Signatures, diagnose};

fn mods() -> Vec<ModInfo> {
    vec![
        ModInfo {
            id: "alpha".into(),
            name: "Alpha".into(),
            packages: vec!["com.example.alpha".into()],
        },
        ModInfo {
            id: "beta".into(),
            name: "Beta".into(),
            packages: vec!["org.sample.beta".into(), "org/sample/shared".into()],
        },
        ModInfo {
            id: "gamma".into(),
            name: "Gamma".into(),
            packages: vec!["org.sample.shared".into()],
        },
    ]
}

fn log(text: &str) -> CrashInput<'_> {
    CrashInput {
        latest_log: text,
        ..CrashInput::default()
    }
}

fn expect(text: &str, signature: &str, message: &str, action: &str, culprits: &[&str]) {
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.level, Level::Signature, "{text}");
    assert_eq!(d.signature.as_deref(), Some(signature), "{text}");
    assert_eq!(d.message_key, message);
    assert_eq!(d.action_key.as_deref(), Some(action));
    assert_eq!(d.culprit_mods, culprits, "{text}");
    assert!(!d.evidence_lines.is_empty());
}

#[test]
fn builtin_table_is_valid() {
    let ids = Signatures::builtin().ids();
    assert_eq!(
        ids,
        [
            "wrong_java",
            "native_memory",
            "out_of_memory",
            "jvm_crash",
            "wrong_game_version",
            "wrong_loader_version",
            "missing_dependency",
            "duplicate_mod",
            "mixin_conflict"
        ]
    );
}

#[test]
fn missing_dependency_on_fabric() {
    let text = "[main/INFO]: Loading\n\
        net.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible\n\
        \t- Mod 'Alpha' (alpha) 1.2.0 requires any version of Fabric API (fabric-api), which is missing!\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("missing_dependency"));
    assert_eq!(d.culprit_mods, ["alpha"]);
    assert_eq!(
        d.args.get("dependency").map(String::as_str),
        Some("fabric-api")
    );
}

#[test]
fn missing_dependency_on_forge() {
    let text = "Missing or unsupported mandatory dependencies:\n\
        \tMod ID: 'beta_lib', Requested by: 'alpha', Expected range: '[1.0,)', Actual version: '[MISSING]'\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("missing_dependency"));
    assert_eq!(d.culprit_mods, ["alpha"]);
    assert_eq!(
        d.args.get("dependency").map(String::as_str),
        Some("beta_lib")
    );
    assert_eq!(d.evidence_lines.len(), 1);
    assert_eq!(d.evidence_lines[0].line, 2);
}

#[test]
fn missing_dependency_on_neoforge_and_quilt_style() {
    expect(
        "Could not find required mod: alpha requires {beta_lib @ [1.0,)}",
        "missing_dependency",
        "crash-missing-dependency",
        "crash-action-install-dependency",
        &["alpha"],
    );
}

#[test]
fn duplicate_mod() {
    expect(
        "Caused by: net.fabricmc.loader.impl.FormattedException: Duplicate mod ID 'alpha'\n",
        "duplicate_mod",
        "crash-duplicate-mod",
        "crash-action-remove-duplicate",
        &["alpha"],
    );
    expect(
        "Found duplicate mods:\n\tMod ID: 'beta' from mod files: beta-1.jar, beta-2.jar\n",
        "duplicate_mod",
        "crash-duplicate-mod",
        "crash-action-remove-duplicate",
        &["beta"],
    );
}

#[test]
fn wrong_game_version() {
    expect(
        "\t- Mod 'Alpha' (alpha) 1.2.0 requires version 1.20.1 of minecraft (minecraft), but only the wrong version is present: 1.21!\n",
        "wrong_game_version",
        "crash-wrong-game-version",
        "crash-action-change-game-version",
        &["alpha"],
    );
    let text = "\tMod ID: 'minecraft', Requested by: 'beta', Expected range: '[1.20.1]', Actual version: '1.21'\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("wrong_game_version"));
    assert_eq!(d.args.get("expected").map(String::as_str), Some("[1.20.1]"));
    assert_eq!(d.args.get("found").map(String::as_str), Some("1.21"));
}

#[test]
fn wrong_loader_version() {
    expect(
        "\t- Mod 'Beta' (beta) 2.0 requires version 0.16.0 or later of fabricloader, but only the wrong version is present: 0.15.0!\n",
        "wrong_loader_version",
        "crash-wrong-loader-version",
        "crash-action-change-loader-version",
        &["beta"],
    );
    expect(
        "\tMod ID: 'neoforge', Requested by: 'alpha', Expected range: '[21.1.200,)', Actual version: '21.1.1'\n",
        "wrong_loader_version",
        "crash-wrong-loader-version",
        "crash-action-change-loader-version",
        &["alpha"],
    );
}

#[test]
fn wrong_java_from_class_version() {
    let text = "Caused by: java.lang.UnsupportedClassVersionError: com/example/alpha/Main has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("wrong_java"));
    assert_eq!(d.message_key, "crash-wrong-java");
    assert_eq!(d.args.get("java_major").map(String::as_str), Some("21"));
    assert_eq!(
        d.args.get("supported_version").map(String::as_str),
        Some("61")
    );
}

#[test]
fn wrong_java_from_loader_message() {
    let text = "\t- Mod 'Alpha' (alpha) 1.0 requires version 21 or later of java, but only the wrong version is present: 17!\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("wrong_java"));
    assert_eq!(d.args.get("java_major").map(String::as_str), Some("21"));
}

#[test]
fn mixin_conflict() {
    let text = "[main/ERROR]: Mixin apply for mod alpha failed alpha.mixins.json:client.ScreenMixin from mod alpha -> net.minecraft.client.gui.screens.Screen: org.spongepowered.asm.mixin.injection.throwables.InvalidInjectionException Critical injection callback\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("mixin_conflict"));
    assert_eq!(d.culprit_mods, ["alpha"]);
    assert_eq!(d.message_key, "crash-mixin-conflict");
}

#[test]
fn mixin_conflict_without_mod_name_has_no_culprit() {
    let text = "Caused by: org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: An unexpected critical error was encountered\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.signature.as_deref(), Some("mixin_conflict"));
    assert!(d.culprit_mods.is_empty());
}

#[test]
fn out_of_memory() {
    let text = "---- Minecraft Crash Report ----\nDescription: Exception in server tick loop\njava.lang.OutOfMemoryError: Java heap space\n\tat java.base/java.util.Arrays.copyOf(Arrays.java:3512)\n";
    let input = CrashInput {
        crash_report: text,
        ..CrashInput::default()
    };
    let d = diagnose(&input, &mods());
    assert_eq!(d.signature.as_deref(), Some("out_of_memory"));
    assert_eq!(d.action_key.as_deref(), Some("crash-action-raise-memory"));
    assert_eq!(
        d.args.get("kind").map(String::as_str),
        Some("Java heap space")
    );
    assert_eq!(d.evidence_lines[0].line, 3);
}

#[test]
fn heap_cannot_be_reserved() {
    expect(
        "Error occurred during initialization of VM\nCould not reserve enough space for 16777216KB object heap\n",
        "native_memory",
        "crash-native-memory",
        "crash-action-lower-memory",
        &[],
    );
}

#[test]
fn jvm_crash_from_hs_err() {
    let text = "#\n# A fatal error has been detected by the Java Runtime Environment:\n#\n#  EXCEPTION_ACCESS_VIOLATION (0xc0000005) at pc=0x00007ff8, pid=4242, tid=17\n#\n# Problematic frame:\n# C  [synthetic_driver64.dll+0x1a2b3c]\n#\n";
    let input = CrashInput {
        hs_err: text,
        ..CrashInput::default()
    };
    let d = diagnose(&input, &mods());
    assert_eq!(d.signature.as_deref(), Some("jvm_crash"));
    assert_eq!(d.message_key, "crash-jvm-crash");
    assert_eq!(
        d.args.get("frame").map(String::as_str),
        Some("synthetic_driver64.dll")
    );
}

#[test]
fn jvm_markers_in_a_game_log_are_not_a_jvm_crash() {
    let d = diagnose(&log("note: SIGSEGV handler installed\n"), &mods());
    assert_eq!(d.level, Level::Undetermined);
}

const ONE_MOD_STACK: &str = "---- Minecraft Crash Report ----\n\
Description: Ticking entity\n\
\n\
java.lang.NullPointerException: Ticking entity\n\
\tat com.example.alpha.entity.Thing.tick(Thing.java:42)\n\
\tat net.minecraft.world.level.Level.guardEntityTick(Level.java:100)\n\
\tat java.base/java.lang.Thread.run(Thread.java:1583)\n\
\n\
-- System Details --\n";

#[test]
fn one_mod_in_stack_is_a_hedged_suspect() {
    let input = CrashInput {
        crash_report: ONE_MOD_STACK,
        ..CrashInput::default()
    };
    let d = diagnose(&input, &mods());
    assert_eq!(d.level, Level::SingleMod);
    assert_eq!(d.signature, None);
    assert_eq!(d.culprit_mods, ["alpha"]);
    assert_eq!(d.message_key, "crash-suspect-single");
    assert_eq!(d.evidence_lines[0].line, 5);
}

#[test]
fn several_mods_in_stack_give_a_list() {
    let text = "java.lang.IllegalStateException: boom\n\
\tat TRANSFORMER/beta@1.0/org.sample.beta.Util.call(Util.java:1)\n\
\tat com.example.alpha.Main.run(Main.java:9)\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.level, Level::SeveralMods);
    assert_eq!(d.culprit_mods, ["beta", "alpha"]);
    assert_eq!(d.message_key, "crash-suspect-several");
}

#[test]
fn shared_package_is_ambiguous() {
    let text =
        "java.lang.IllegalStateException: boom\n\tat org.sample.shared.Lib.call(Lib.java:1)\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.level, Level::SeveralMods);
    assert_eq!(d.culprit_mods, ["beta", "gamma"]);
}

#[test]
fn only_the_last_trace_of_a_log_counts() {
    let text = "WARN harmless\n\
java.lang.Exception: old\n\
\tat org.sample.beta.Old.run(Old.java:1)\n\
[main/INFO]: moving on\n\
java.lang.Exception: new\n\
\tat com.example.alpha.New.run(New.java:1)\n";
    let d = diagnose(&log(text), &mods());
    assert_eq!(d.level, Level::SingleMod);
    assert_eq!(d.culprit_mods, ["alpha"]);
}

#[test]
fn game_and_loader_frames_are_never_suspects() {
    let text = "java.lang.Exception: x\n\tat net.minecraft.Foo.bar(Foo.java:1)\n\tat net.fabricmc.loader.Knot.run(Knot.java:2)\n";
    let all_prefixes = vec![ModInfo {
        id: "greedy".into(),
        name: "Greedy".into(),
        packages: vec!["net.minecraft".into(), "net.fabricmc".into()],
    }];
    let d = diagnose(&log(text), &all_prefixes);
    assert_eq!(d.level, Level::Undetermined);
}

#[test]
fn no_signature_is_undetermined() {
    let d = diagnose(
        &log("[main/INFO]: Stopping!\n[main/INFO]: Saving worlds\n"),
        &mods(),
    );
    assert_eq!(d.level, Level::Undetermined);
    assert_eq!(d.level.number(), 4);
    assert_eq!(d.signature, None);
    assert!(d.culprit_mods.is_empty());
    assert_eq!(d.message_key, "crash-undetermined");
    assert_eq!(d.action_key.as_deref(), Some("crash-action-export-report"));
}

#[test]
fn garbage_input_never_panics() {
    let long = "x".repeat(200_000);
    let multibyte = "\u{436}\u{0}\u{feff}at \tat (((\n".repeat(500);
    let samples = [
        "",
        "\0\0\0",
        "at ",
        "\tat (",
        "\tat .",
        "\tat /.(",
        "Caused by:",
        "...",
        long.as_str(),
        multibyte.as_str(),
        "\u{1F600}\r\n\r\n",
        "UnsupportedClassVersionError class file version 99999999999999999999",
        "class file version 3",
        "UnsupportedClassVersionError class file version 0",
    ];
    for sample in samples {
        let input = CrashInput {
            latest_log: sample,
            crash_report: sample,
            hs_err: sample,
        };
        let d = diagnose(&input, &mods());
        assert!((1..=4).contains(&d.level.number()));
        let empty: Vec<ModInfo> = Vec::new();
        let _ = diagnose(&input, &empty);
    }
}

#[test]
fn custom_tables_are_validated() {
    assert!(Signatures::parse("[[signature]]\nid='x'\nmessage_key='k'\ntriggers=['(']").is_err());
    assert!(Signatures::parse("[[signature]]\nid='x'\nmessage_key='k'\ntriggers=[]").is_err());
    assert!(
        Signatures::parse(
            "[[signature]]\nid='x'\nmessage_key='k'\nsources=['nope']\ntriggers=['a']"
        )
        .is_err()
    );
}
