//! Reading and writing the settings files.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use vl_core::settings::{Overrides, SCHEMA_VERSION, Settings};
use vl_core::{Error, InstanceId};

use crate::codec;

/// A value read from disk and the warnings produced while reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    /// The value; defaults stand in for whatever could not be read.
    pub value: T,
    /// What was damaged or invalid (English, no secrets). Empty for a healthy or missing file.
    pub warnings: Vec<String>,
}

/// An instance's overrides and the settings that result from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSettings {
    /// The instance's overrides.
    pub overrides: Overrides,
    /// Global settings with the overrides applied.
    pub effective: Settings,
}

/// The settings files under `<data>/settings`.
///
/// The store holds no state: every call reads or writes the files, so it can be shared freely.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    dir: PathBuf,
}

impl SettingsStore {
    /// A store for the settings of the data directory `data_dir`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("settings"),
        }
    }

    /// The path of the global settings file.
    pub fn global_path(&self) -> PathBuf {
        self.dir.join("global.toml")
    }

    /// The path of an instance's overrides file.
    pub fn instance_path(&self, id: &InstanceId) -> PathBuf {
        self.dir
            .join("instances")
            .join(format!("{}.toml", file_stem(id.as_str())))
    }

    /// The global settings. A missing file gives the defaults.
    pub fn global(&self) -> Loaded<Settings> {
        load(&self.global_path(), codec::decode_settings)
    }

    /// Replaces the global settings. Invalid values are refused and nothing is written.
    pub fn set_global(&self, settings: &Settings) -> Result<(), Error> {
        settings.validate()?;
        save(&self.global_path(), settings)
    }

    /// An instance's overrides. A missing file means no overrides.
    pub fn overrides(&self, id: &InstanceId) -> Loaded<Overrides> {
        load(&self.instance_path(id), codec::decode_overrides)
    }

    /// Replaces an instance's overrides. Invalid values are refused and nothing is written.
    /// Empty overrides remove the instance's file.
    pub fn set_overrides(&self, id: &InstanceId, overrides: &Overrides) -> Result<(), Error> {
        overrides.validate()?;
        let path = self.instance_path(id);
        if *overrides == Overrides::default() {
            return remove(&path);
        }
        save(&path, overrides)
    }

    /// An instance's overrides and its effective settings (global settings with the overrides
    /// applied).
    pub fn instance(&self, id: &InstanceId) -> Loaded<InstanceSettings> {
        let global = self.global();
        let overrides = self.overrides(id);
        let effective = global.value.with_overrides(&overrides.value);
        let mut warnings = global.warnings;
        warnings.extend(overrides.warnings);
        Loaded {
            value: InstanceSettings {
                overrides: overrides.value,
                effective,
            },
            warnings,
        }
    }

    /// The effective settings of an instance.
    pub fn effective(&self, id: &InstanceId) -> Loaded<Settings> {
        let l = self.instance(id);
        Loaded {
            value: l.value.effective,
            warnings: l.warnings,
        }
    }

    /// Deletes an instance's overrides (when the instance is deleted).
    pub fn remove_instance(&self, id: &InstanceId) -> Result<(), Error> {
        remove(&self.instance_path(id))
    }
}

fn load<T: Default>(path: &Path, decode: impl FnOnce(&str) -> codec::Decoded<T>) -> Loaded<T> {
    let fallback = |warning: String| Loaded {
        value: T::default(),
        warnings: vec![warning],
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Loaded {
                value: T::default(),
                warnings: Vec::new(),
            };
        }
        Err(e) => {
            return fallback(format!(
                "{}: cannot read file ({e}); using defaults",
                name(path)
            ));
        }
    };
    let n = name(path);
    let Ok(text) = std::str::from_utf8(&bytes) else {
        let mut l = fallback(format!("{n}: not valid UTF-8; using defaults"));
        l.warnings.extend(keep_copy(path, &bytes));
        return l;
    };
    let mut decoded = decode(text);
    if !decoded.warnings.is_empty() {
        for w in &mut decoded.warnings {
            *w = format!("{n}: {w}");
        }
        decoded.warnings.extend(keep_copy(path, &bytes));
    }
    Loaded {
        value: decoded.value,
        warnings: decoded.warnings,
    }
}

/// Copies a file that needed repair to `<file>.bad` so that saving does not destroy it.
fn keep_copy(path: &Path, bytes: &[u8]) -> Option<String> {
    let mut bad = path.as_os_str().to_owned();
    bad.push(".bad");
    vl_store::write_atomic(Path::new(&bad), bytes)
        .err()
        .map(|e| format!("could not keep a copy of the damaged file: {e}"))
}

fn save<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    // A file from a newer schema keeps its version number.
    let version = fs::read_to_string(path)
        .ok()
        .and_then(|t| codec::declared_version(&t))
        .filter(|v| *v > i64::from(SCHEMA_VERSION))
        .unwrap_or(i64::from(SCHEMA_VERSION));
    let text = codec::encode(value, version).map_err(Error::internal)?;
    vl_store::write_atomic(path, text.as_bytes())?;
    Ok(())
}

fn remove(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(
            vl_core::ErrorKind::Io,
            format!("cannot remove {}: {e}", name(path)),
        )),
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A file name for an instance id: lowercase letters, digits, `-` and `_` stay, every other byte
/// becomes `%XX`, and a name Windows reserves for devices gets its first byte encoded.
fn file_stem(id: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(b as char),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    let numbered = |prefix: &str| {
        out.strip_prefix(prefix)
            .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    };
    let is_device =
        ["con", "prn", "aux", "nul"].contains(&out.as_str()) || numbered("com") || numbered("lpt");
    if is_device {
        let first = out.remove(0);
        out = format!("%{:02X}{out}", first as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::install::MinStability;
    use vl_core::settings::{
        AfterLaunch, CommandSettings, EnvSettings, LauncherSettings, MemorySettings, OnGameExit,
        PerformanceSettings, WindowSettings,
    };

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("vl-settings-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn id(s: &str) -> InstanceId {
        InstanceId::new(s).unwrap()
    }

    fn write(path: PathBuf, content: impl AsRef<[u8]>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn missing_files_give_defaults() {
        let d = Dir::new("missing");
        let s = SettingsStore::new(&d.0);
        let g = s.global();
        assert_eq!(g.value, Settings::default());
        assert!(g.warnings.is_empty());
        assert_eq!(s.overrides(&id("a")).value, Overrides::default());
    }

    #[test]
    fn global_settings_round_trip() {
        let d = Dir::new("roundtrip");
        let s = SettingsStore::new(&d.0);
        let mut v = Settings::default();
        v.memory.max_mib = 8192;
        v.java.path = Some(PathBuf::from("/opt/java/bin/java"));
        v.java.extra_args = vec!["-XX:+UseZGC".into()];
        v.commands.wrapper = "gamemoderun".into();
        v.env.vars.insert("MESA_GLTHREAD".into(), "true".into());
        v.performance.zink = true;
        v.natives.system_glfw = true;
        s.set_global(&v).unwrap();
        let text = fs::read_to_string(s.global_path()).unwrap();
        assert!(text.starts_with("schema_version = 1"));
        let back = s.global();
        assert_eq!(back.value, v);
        assert!(back.warnings.is_empty());
    }

    #[test]
    fn instance_overrides_merge_with_global() {
        let d = Dir::new("merge");
        let s = SettingsStore::new(&d.0);
        let mut g = Settings::default();
        g.memory.max_mib = 2048;
        g.performance.gamemode = true;
        s.set_global(&g).unwrap();

        let a = id("Modded 1.20");
        let o = Overrides {
            memory: Some(MemorySettings {
                min_mib: 1024,
                max_mib: 6144,
                ..Default::default()
            }),
            window: Some(WindowSettings {
                width: 1920,
                height: 1080,
                ..Default::default()
            }),
            ..Default::default()
        };
        s.set_overrides(&a, &o).unwrap();

        let view = s.instance(&a).value;
        assert_eq!(view.overrides, o);
        assert_eq!(view.effective.memory.max_mib, 6144);
        assert_eq!(view.effective.window.width, 1920);
        assert!(view.effective.performance.gamemode, "inherited from global");

        // A global change reaches groups the instance does not override only.
        g.memory.max_mib = 3000;
        g.window.width = 640;
        s.set_global(&g).unwrap();
        let e = s.effective(&a).value;
        assert_eq!(e.memory.max_mib, 6144);
        assert_eq!(e.window.width, 1920);

        // Another instance sees the global values.
        assert_eq!(s.effective(&id("other")).value.memory.max_mib, 3000);

        // Clearing the overrides removes the file.
        s.set_overrides(&a, &Overrides::default()).unwrap();
        assert!(!s.instance_path(&a).exists());
        assert_eq!(s.effective(&a).value.memory.max_mib, 3000);
    }

    #[test]
    fn instance_file_marks_overridden_groups_by_table() {
        let d = Dir::new("marks");
        let s = SettingsStore::new(&d.0);
        let a = id("a");
        let o = Overrides {
            performance: Some(PerformanceSettings {
                mangohud: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        s.set_overrides(&a, &o).unwrap();
        let text = fs::read_to_string(s.instance_path(&a)).unwrap();
        assert!(text.contains("[performance]"));
        assert!(!text.contains("[memory]"));
    }

    #[test]
    fn a_damaged_global_file_falls_back_to_defaults_with_a_warning() {
        let d = Dir::new("damaged");
        let s = SettingsStore::new(&d.0);
        write(s.global_path(), "memory = [[[ not toml");
        let g = s.global();
        assert_eq!(g.value, Settings::default());
        assert_eq!(g.warnings.len(), 1);
        assert!(g.warnings[0].contains("not valid TOML"));
        // The original is kept next to the file.
        let bad = fs::read_to_string(d.0.join("settings").join("global.toml.bad")).unwrap();
        assert!(bad.contains("not toml"));
        // The launcher can save over it.
        s.set_global(&Settings::default()).unwrap();
        assert!(s.global().warnings.is_empty());
    }

    #[test]
    fn binary_garbage_does_not_panic() {
        let d = Dir::new("garbage");
        let s = SettingsStore::new(&d.0);
        write(s.global_path(), [0xff, 0xfe, 0x00, 0x80]);
        let g = s.global();
        assert_eq!(g.value, Settings::default());
        assert!(!g.warnings.is_empty());
    }

    #[test]
    fn a_bad_group_resets_only_that_group() {
        let d = Dir::new("badgroup");
        let s = SettingsStore::new(&d.0);
        write(
            s.global_path(),
            "schema_version = 1\n[memory]\nmin_mib = \"lots\"\n[window]\nwidth = 1000\nheight = 700\n[console]\nmax_lines = 5\n",
        );
        let g = s.global();
        assert_eq!(g.value.memory, MemorySettings::default());
        assert_eq!(g.value.window.width, 1000);
        assert_eq!(g.value.console.max_lines, 10_000);
        // Two repairs plus the note about the kept copy do not occur: only the two repairs.
        assert_eq!(g.warnings.len(), 2, "{:?}", g.warnings);
    }

    #[test]
    fn an_out_of_range_override_is_dropped_so_the_instance_inherits() {
        let d = Dir::new("badoverride");
        let s = SettingsStore::new(&d.0);
        let a = id("a");
        write(
            s.instance_path(&a),
            "[memory]\nmin_mib = 8000\nmax_mib = 4000\n[performance]\nzink = true\n",
        );
        let l = s.instance(&a);
        assert!(l.value.overrides.memory.is_none());
        assert!(l.value.effective.performance.zink);
        assert_eq!(l.value.effective.memory, MemorySettings::default());
        assert_eq!(l.warnings.len(), 1);
    }

    #[test]
    fn advanced_limits_round_trip_and_a_bad_value_resets_the_group() {
        let d = Dir::new("advanced");
        let s = SettingsStore::new(&d.0);
        let mut v = Settings::default();
        v.advanced.log_max_files = 7;
        v.advanced.cache_max_mib = 512;
        s.set_global(&v).unwrap();
        assert_eq!(s.global().value.advanced, v.advanced);
        v.advanced.cache_max_mib = 1;
        assert!(s.set_global(&v).is_err());
        write(
            s.global_path(),
            "[advanced]
log_max_files = 0
",
        );
        let l = s.global();
        assert_eq!(l.value.advanced.log_max_files, 30);
        assert_eq!(l.warnings.len(), 1);
    }

    #[test]
    fn launcher_behaviour_round_trips_and_an_unknown_value_resets_the_group() {
        let d = Dir::new("launcher");
        let s = SettingsStore::new(&d.0);
        let mut v = Settings::default();
        v.launcher.after_launch = AfterLaunch::KeepOpen;
        v.launcher.on_game_exit = OnGameExit::Nothing;
        s.set_global(&v).unwrap();
        let text = fs::read_to_string(s.global_path()).unwrap();
        assert!(
            text.contains("[launcher]") && text.contains("after_launch = \"keep_open\""),
            "{text}"
        );
        assert_eq!(s.global().value.launcher, v.launcher);
        write(
            s.global_path(),
            "[launcher]
after_launch = \"float_away\"
",
        );
        let l = s.global();
        assert_eq!(l.value.launcher, LauncherSettings::default());
        assert_eq!(l.warnings.len(), 1);
    }

    #[test]
    fn the_update_channel_is_stored_globally_and_per_instance() {
        let d = Dir::new("updates");
        let s = SettingsStore::new(&d.0);
        let mut g = Settings::default();
        g.updates.channel = MinStability::Beta;
        s.set_global(&g).unwrap();
        let text = fs::read_to_string(s.global_path()).unwrap();
        assert!(
            text.contains("[updates]") && text.contains("channel = \"beta\""),
            "{text}"
        );
        assert_eq!(s.global().value.updates.channel, MinStability::Beta);

        let a = id("a");
        write(
            s.instance_path(&a),
            "[updates]
channel = \"alpha\"
",
        );
        assert_eq!(
            s.instance(&a).value.effective.updates.channel,
            MinStability::Alpha
        );
        assert_eq!(
            s.instance(&id("b")).value.effective.updates.channel,
            MinStability::Beta
        );

        // An unknown channel drops the override, so the instance inherits.
        write(
            s.instance_path(&a),
            "[updates]
channel = \"nightly\"
",
        );
        let l = s.instance(&a);
        assert!(l.value.overrides.updates.is_none());
        assert_eq!(l.value.effective.updates.channel, MinStability::Beta);
        assert_eq!(l.warnings.len(), 1, "{:?}", l.warnings);
    }

    #[test]
    fn invalid_values_are_refused_and_nothing_is_written() {
        let d = Dir::new("refuse");
        let s = SettingsStore::new(&d.0);
        let mut v = Settings::default();
        v.memory.min_mib = 9000;
        assert!(s.set_global(&v).is_err());
        assert!(!s.global_path().exists());
        let o = Overrides {
            env: Some(EnvSettings {
                vars: [("A=B".to_string(), "x".to_string())].into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(s.set_overrides(&id("a"), &o).is_err());
    }

    #[test]
    fn unknown_keys_and_groups_survive_a_load_and_save() {
        let d = Dir::new("unknown");
        let s = SettingsStore::new(&d.0);
        write(
            s.global_path(),
            "schema_version = 2\n[memory]\nmin_mib = 1024\nmax_mib = 2048\nfuture_knob = 7\n[hologram]\ndepth = \"deep\"\n",
        );
        let mut g = s.global();
        assert_eq!(g.warnings.len(), 1, "a newer schema is announced");
        g.value.window.width = 1111;
        s.set_global(&g.value).unwrap();
        let text = fs::read_to_string(s.global_path()).unwrap();
        assert!(text.starts_with("schema_version = 2"), "{text}");
        assert!(text.contains("future_knob = 7"));
        assert!(text.contains("[hologram]"));
        assert!(text.contains("depth = \"deep\""));
        assert_eq!(s.global().value.window.width, 1111);
    }

    #[test]
    fn unknown_keys_in_instance_overrides_survive() {
        let d = Dir::new("unknown-inst");
        let s = SettingsStore::new(&d.0);
        let a = id("a");
        write(
            s.instance_path(&a),
            "[commands]\nwrapper = \"w\"\nnew_hook = \"x\"\n",
        );
        let mut o = s.overrides(&a).value;
        let commands: &CommandSettings = o.commands.as_ref().unwrap();
        assert_eq!(commands.wrapper, "w");
        o.performance = Some(PerformanceSettings::default());
        s.set_overrides(&a, &o).unwrap();
        let text = fs::read_to_string(s.instance_path(&a)).unwrap();
        assert!(text.contains("new_hook"));
    }

    #[test]
    fn partial_files_take_defaults_for_missing_keys() {
        let d = Dir::new("partial");
        let s = SettingsStore::new(&d.0);
        write(s.global_path(), "[memory]\nmax_mib = 3000\n");
        let g = s.global();
        assert_eq!(g.value.memory.max_mib, 3000);
        assert_eq!(g.value.memory.min_mib, 512);
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn instance_ids_map_to_safe_distinct_file_names() {
        assert_eq!(file_stem("main"), "main");
        assert_eq!(file_stem("A b/../c"), "%41%20b%2F%2E%2E%2Fc");
        assert_ne!(file_stem("Ab"), file_stem("ab"));
        assert_eq!(file_stem("con"), "%63on");
        assert_eq!(file_stem("com1"), "%63om1");
        assert_eq!(file_stem("com10"), "com10");
        let d = Dir::new("names");
        let s = SettingsStore::new(&d.0);
        let p = s.instance_path(&id("../../escape"));
        assert!(p.starts_with(d.0.join("settings").join("instances")));
    }
}
