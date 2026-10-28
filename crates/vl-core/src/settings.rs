//! Settings: global values, per-instance overrides by group, and how they combine.
//!
//! The effective settings of an instance are, group by group: the instance's override if it has
//! one, otherwise the global group, which starts from the built-in defaults. An instance
//! overrides a whole group or nothing of it: a `Some` group in [`Overrides`] replaces the global
//! group, `None` inherits it.
//!
//! Every type keeps the fields this build does not know ([`Extra`]), so a file written by a newer
//! launcher is not stripped when an older one saves it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::ids::InstanceId;

/// The version of the settings schema written to disk and understood by this build.
pub const SCHEMA_VERSION: u32 = 1;

/// Smallest allowed heap, MiB.
pub const MEMORY_MIN_MIB: u32 = 64;
/// Largest allowed heap, MiB (1 TiB).
pub const MEMORY_MAX_MIB: u32 = 1 << 20;
/// Smallest allowed window side, pixels.
pub const WINDOW_MIN_PX: u32 = 100;
/// Largest allowed window side, pixels.
pub const WINDOW_MAX_PX: u32 = 16384;
/// Smallest allowed console history, lines.
pub const CONSOLE_MIN_LINES: u32 = 100;
/// Largest allowed console history, lines.
pub const CONSOLE_MAX_LINES: u32 = 1_000_000;

/// Smallest allowed number of launcher log files kept.
pub const LOG_FILES_MIN: u32 = 1;
/// Largest allowed number of launcher log files kept.
pub const LOG_FILES_MAX: u32 = 10_000;
/// Smallest allowed size limit of the launcher logs, MiB.
pub const LOG_MIB_MIN: u32 = 1;
/// Largest allowed size limit of the launcher logs, MiB (64 GiB).
pub const LOG_MIB_MAX: u32 = 64 * 1024;
/// Smallest allowed size limit of the cache, MiB.
pub const CACHE_MIB_MIN: u32 = 16;
/// Largest allowed size limit of the cache, MiB (1 TiB).
pub const CACHE_MIB_MAX: u32 = 1 << 20;

/// Fields a build does not know, kept so that saving does not lose them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extra(pub BTreeMap<String, serde_json::Value>);

impl Extra {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Java used to run the game.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaSettings {
    /// A fixed `java` executable; `None` picks one automatically for the game version.
    pub path: Option<PathBuf>,
    /// Extra JVM arguments.
    pub extra_args: Vec<String>,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// Memory given to the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    /// Initial heap (`-Xms`), MiB.
    pub min_mib: u32,
    /// Maximum heap (`-Xmx`), MiB.
    pub max_mib: u32,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            min_mib: 512,
            max_mib: 4096,
            extra: Extra::default(),
        }
    }
}

/// The game window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Start maximized.
    pub maximized: bool,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 854,
            height: 480,
            maximized: false,
            extra: Extra::default(),
        }
    }
}

/// The launcher's game console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsoleSettings {
    /// Show the console when the game starts.
    pub show_on_launch: bool,
    /// Show the console when the game crashes.
    pub show_on_crash: bool,
    /// Close the console when the game exits normally.
    pub close_on_exit: bool,
    /// Lines of game log kept in the console.
    pub max_lines: u32,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

impl Default for ConsoleSettings {
    fn default() -> Self {
        Self {
            show_on_launch: false,
            show_on_crash: true,
            close_on_exit: false,
            max_lines: 10_000,
            extra: Extra::default(),
        }
    }
}

/// Commands run around the game. An empty string means none.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandSettings {
    /// Run before the game starts; a failure stops the launch.
    pub pre_launch: String,
    /// Prefix of the game command line (for example a profiler).
    pub wrapper: String,
    /// Run after the game exits.
    pub post_exit: String,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// Environment variables set for the game process.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvSettings {
    /// Variable name to value.
    pub vars: BTreeMap<String, String>,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// Performance helpers, mainly for Linux.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    /// Run the game under Feral GameMode.
    pub gamemode: bool,
    /// Show the MangoHud overlay.
    pub mangohud: bool,
    /// Run on the discrete GPU of a hybrid-graphics machine.
    pub discrete_gpu: bool,
    /// Render OpenGL through Zink (Vulkan).
    pub zink: bool,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// Use the system's libraries instead of the ones bundled with the game: workarounds for natives
/// that do not work on a given system.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NativesSettings {
    /// System GLFW.
    pub system_glfw: bool,
    /// System OpenAL.
    pub system_openal: bool,
    /// System SDL.
    pub system_sdl: bool,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// Limits of the folders that grow by themselves. Global only: an instance cannot override them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedSettings {
    /// Launcher log files kept; the oldest are removed beyond this.
    pub log_max_files: u32,
    /// Size limit of the launcher logs, MiB.
    pub log_max_mib: u32,
    /// Size limit of the cache, MiB.
    pub cache_max_mib: u32,
    /// Unknown fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            log_max_files: 30,
            log_max_mib: 64,
            cache_max_mib: 2048,
            extra: Extra::default(),
        }
    }
}

impl AdvancedSettings {
    /// Checks the ranges.
    pub fn validate(&self) -> Result<(), Error> {
        let a = "advanced";
        check_range(
            a,
            "log_max_files",
            self.log_max_files,
            LOG_FILES_MIN,
            LOG_FILES_MAX,
        )?;
        check_range(a, "log_max_mib", self.log_max_mib, LOG_MIB_MIN, LOG_MIB_MAX)?;
        check_range(
            a,
            "cache_max_mib",
            self.cache_max_mib,
            CACHE_MIB_MIN,
            CACHE_MIB_MAX,
        )
    }
}

fn check_range(group: &str, field: &str, value: u32, min: u32, max: u32) -> Result<(), Error> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(Error::invalid_input(format!(
            "{group}.{field} = {value} is outside {min}..={max}"
        )))
    }
}

impl MemorySettings {
    /// Checks the ranges and that the initial heap does not exceed the maximum.
    pub fn validate(&self) -> Result<(), Error> {
        let (lo, hi) = (MEMORY_MIN_MIB, MEMORY_MAX_MIB);
        check_range("memory", "min_mib", self.min_mib, lo, hi)?;
        check_range("memory", "max_mib", self.max_mib, lo, hi)?;
        if self.min_mib > self.max_mib {
            return Err(Error::invalid_input(format!(
                "memory.min_mib = {} exceeds memory.max_mib = {}",
                self.min_mib, self.max_mib
            )));
        }
        Ok(())
    }
}

impl WindowSettings {
    /// Checks the ranges.
    pub fn validate(&self) -> Result<(), Error> {
        let (lo, hi) = (WINDOW_MIN_PX, WINDOW_MAX_PX);
        check_range("window", "width", self.width, lo, hi)?;
        check_range("window", "height", self.height, lo, hi)
    }
}

impl ConsoleSettings {
    /// Checks the ranges.
    pub fn validate(&self) -> Result<(), Error> {
        let (lo, hi) = (CONSOLE_MIN_LINES, CONSOLE_MAX_LINES);
        check_range("console", "max_lines", self.max_lines, lo, hi)
    }
}

impl EnvSettings {
    /// Checks that names are non-empty and free of `=` and NUL, and values free of NUL.
    pub fn validate(&self) -> Result<(), Error> {
        for (name, value) in &self.vars {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(Error::invalid_input(format!(
                    "env.vars: invalid variable {name:?}"
                )));
            }
        }
        Ok(())
    }
}

/// The settings groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Group {
    /// Java.
    Java,
    /// Memory.
    Memory,
    /// Game window.
    Window,
    /// Game console.
    Console,
    /// Commands around the game.
    Commands,
    /// Environment variables.
    Env,
    /// Performance helpers.
    Performance,
    /// Natives workarounds.
    Natives,
}

impl Group {
    /// All groups.
    pub const ALL: [Group; 8] = [
        Group::Java,
        Group::Memory,
        Group::Window,
        Group::Console,
        Group::Commands,
        Group::Env,
        Group::Performance,
        Group::Natives,
    ];
}

/// The global settings, or an instance's effective settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Java.
    pub java: JavaSettings,
    /// Memory.
    pub memory: MemorySettings,
    /// Game window.
    pub window: WindowSettings,
    /// Game console.
    pub console: ConsoleSettings,
    /// Commands around the game.
    pub commands: CommandSettings,
    /// Environment variables.
    pub env: EnvSettings,
    /// Performance helpers.
    pub performance: PerformanceSettings,
    /// Natives workarounds.
    pub natives: NativesSettings,
    /// Limits of logs and the cache (global only).
    pub advanced: AdvancedSettings,
    /// Unknown groups and fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// An instance's overrides: each `Some` group replaces the global one.
///
/// A `Some` field is the "override this group" switch together with its values.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    /// Java.
    pub java: Option<JavaSettings>,
    /// Memory.
    pub memory: Option<MemorySettings>,
    /// Game window.
    pub window: Option<WindowSettings>,
    /// Game console.
    pub console: Option<ConsoleSettings>,
    /// Commands around the game.
    pub commands: Option<CommandSettings>,
    /// Environment variables.
    pub env: Option<EnvSettings>,
    /// Performance helpers.
    pub performance: Option<PerformanceSettings>,
    /// Natives workarounds.
    pub natives: Option<NativesSettings>,
    /// Unknown groups and fields, kept on save.
    #[serde(flatten, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

impl Settings {
    /// The effective settings of an instance with `overrides`.
    pub fn with_overrides(&self, overrides: &Overrides) -> Self {
        macro_rules! pick {
            ($($g:ident),*) => {
                Self {
                    $($g: overrides.$g.clone().unwrap_or_else(|| self.$g.clone()),)*
                    advanced: self.advanced.clone(),
                    extra: self.extra.clone(),
                }
            };
        }
        pick!(
            java,
            memory,
            window,
            console,
            commands,
            env,
            performance,
            natives
        )
    }

    /// Checks every group.
    pub fn validate(&self) -> Result<(), Error> {
        self.memory.validate()?;
        self.window.validate()?;
        self.console.validate()?;
        self.env.validate()?;
        self.advanced.validate()
    }

    /// Replaces every invalid group with its default; returns a message for each one replaced.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        macro_rules! fix {
            ($($g:ident),*) => {$(
                if let Err(e) = self.$g.validate() {
                    problems.push(format!("{e}; group {} reset to defaults", stringify!($g)));
                    self.$g = Default::default();
                }
            )*};
        }
        fix!(memory, window, console, env, advanced);
        problems
    }
}

impl Overrides {
    /// Whether `group` is overridden.
    pub fn is_overridden(&self, group: Group) -> bool {
        match group {
            Group::Java => self.java.is_some(),
            Group::Memory => self.memory.is_some(),
            Group::Window => self.window.is_some(),
            Group::Console => self.console.is_some(),
            Group::Commands => self.commands.is_some(),
            Group::Env => self.env.is_some(),
            Group::Performance => self.performance.is_some(),
            Group::Natives => self.natives.is_some(),
        }
    }

    /// The overridden groups.
    pub fn overridden(&self) -> Vec<Group> {
        Group::ALL
            .into_iter()
            .filter(|g| self.is_overridden(*g))
            .collect()
    }

    /// Checks every overridden group.
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(g) = &self.memory {
            g.validate()?;
        }
        if let Some(g) = &self.window {
            g.validate()?;
        }
        if let Some(g) = &self.console {
            g.validate()?;
        }
        if let Some(g) = &self.env {
            g.validate()?;
        }
        Ok(())
    }

    /// Drops every invalid overridden group (the instance then inherits it); returns a message
    /// for each one dropped.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut problems = Vec::new();
        macro_rules! fix {
            ($($g:ident),*) => {$(
                if let Some(Err(e)) = self.$g.as_ref().map(|g| g.validate()) {
                    problems.push(format!("{e}; override of group {} dropped", stringify!($g)));
                    self.$g = None;
                }
            )*};
        }
        fix!(memory, window, console, env);
        problems
    }
}

/// Settings commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingsCommand {
    /// The global settings → `Response::Settings`.
    GetGlobal,
    /// Replaces the global settings → `Response::Done`. Invalid values are refused.
    SetGlobal(Settings),
    /// An instance's overrides and effective settings → `Response::InstanceSettings`.
    GetInstance {
        /// The instance.
        id: InstanceId,
    },
    /// Replaces an instance's overrides → `Response::Done`. Invalid values are refused.
    SetInstance {
        /// The instance.
        id: InstanceId,
        /// The new overrides.
        overrides: Overrides,
    },
}

/// Settings changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingsEvent {
    /// The global settings changed.
    GlobalChanged,
    /// An instance's overrides changed.
    InstanceChanged(InstanceId),
    /// A settings file was damaged or held invalid values; defaults were used for the affected
    /// part.
    Warning {
        /// The instance, or `None` for the global settings.
        instance: Option<InstanceId>,
        /// What happened (English, no secrets).
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overridden_group_replaces_the_global_one_whole() {
        let global = Settings {
            memory: MemorySettings {
                min_mib: 1024,
                max_mib: 8192,
                ..MemorySettings::default()
            },
            ..Settings::default()
        };
        let overrides = Overrides {
            window: Some(WindowSettings {
                width: 1280,
                height: 720,
                maximized: true,
                ..WindowSettings::default()
            }),
            ..Overrides::default()
        };
        let effective = global.with_overrides(&overrides);
        assert_eq!(effective.memory, global.memory);
        assert_eq!(effective.window.width, 1280);
        assert!(effective.window.maximized);
        assert_eq!(global.with_overrides(&Overrides::default()), global);
        assert_eq!(overrides.overridden(), [Group::Window]);
    }

    #[test]
    fn every_group_can_be_overridden() {
        let mut global = Settings::default();
        global.performance.gamemode = true;
        global.env.vars.insert("A".into(), "1".into());
        let overrides = Overrides {
            performance: Some(PerformanceSettings::default()),
            env: Some(EnvSettings::default()),
            commands: Some(CommandSettings {
                wrapper: "w".into(),
                ..CommandSettings::default()
            }),
            natives: Some(NativesSettings {
                system_glfw: true,
                ..NativesSettings::default()
            }),
            console: Some(ConsoleSettings::default()),
            java: Some(JavaSettings::default()),
            ..Overrides::default()
        };
        let e = global.with_overrides(&overrides);
        assert!(!e.performance.gamemode);
        assert!(e.env.vars.is_empty());
        assert_eq!(e.commands.wrapper, "w");
        assert!(e.natives.system_glfw);
        assert_eq!(overrides.overridden().len(), 6);
    }

    #[test]
    fn ranges_are_checked_and_sanitized() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.memory.min_mib = 9000;
        s.window.width = 5;
        s.env.vars.insert("BAD=NAME".into(), "x".into());
        assert!(s.validate().is_err());
        s.advanced.cache_max_mib = 0;
        assert_eq!(s.sanitize().len(), 4);
        assert_eq!(s, Settings::default());

        let mut o = Overrides {
            memory: Some(MemorySettings {
                min_mib: 2,
                ..MemorySettings::default()
            }),
            ..Overrides::default()
        };
        assert!(o.validate().is_err());
        assert_eq!(o.sanitize().len(), 1);
        assert!(o.memory.is_none());
    }

    #[test]
    fn advanced_defaults_are_the_former_constants_and_global_only() {
        let a = AdvancedSettings::default();
        assert_eq!(
            (a.log_max_files, a.log_max_mib, a.cache_max_mib),
            (30, 64, 2048)
        );
        let mut global = Settings::default();
        global.advanced.cache_max_mib = 512;
        let effective = global.with_overrides(&Overrides::default());
        assert_eq!(effective.advanced.cache_max_mib, 512);
    }

    #[test]
    fn unknown_fields_survive_a_json_round_trip() {
        let json = r#"{"memory":{"min_mib":1024,"max_mib":2048,"future":7},"newgroup":{"a":1}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.memory.min_mib, 1024);
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["memory"]["future"], 7);
        assert_eq!(back["newgroup"]["a"], 1);
    }
}
