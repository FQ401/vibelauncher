//! Settings: global values, per-instance overrides by group, and how they combine.
//!
//! An instance overrides a whole group or nothing of it: a `Some` group in [`Overrides`]
//! replaces the global group, `None` inherits it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::InstanceId;

/// Java used to run the game.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaSettings {
    /// A fixed `java` executable; `None` picks one automatically for the game version.
    pub path: Option<PathBuf>,
    /// Extra JVM arguments.
    pub extra_args: Vec<String>,
}

/// Memory given to the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    /// Initial heap (`-Xms`), MiB.
    pub min_mib: u32,
    /// Maximum heap (`-Xmx`), MiB.
    pub max_mib: u32,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            min_mib: 512,
            max_mib: 4096,
        }
    }
}

/// The game window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Start maximized.
    pub maximized: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 854,
            height: 480,
            maximized: false,
        }
    }
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
}

/// An instance's overrides: each `Some` group replaces the global one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    /// Java.
    pub java: Option<JavaSettings>,
    /// Memory.
    pub memory: Option<MemorySettings>,
    /// Game window.
    pub window: Option<WindowSettings>,
}

impl Settings {
    /// The effective settings of an instance with `overrides`.
    pub fn with_overrides(&self, overrides: &Overrides) -> Self {
        Self {
            java: overrides.java.clone().unwrap_or_else(|| self.java.clone()),
            memory: overrides.memory.unwrap_or(self.memory),
            window: overrides.window.unwrap_or(self.window),
        }
    }
}

/// Settings commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingsCommand {
    /// The global settings → `Response::Settings`.
    GetGlobal,
    /// Replaces the global settings → `Response::Done`.
    SetGlobal(Settings),
    /// An instance's overrides and effective settings → `Response::InstanceSettings`.
    GetInstance {
        /// The instance.
        id: InstanceId,
    },
    /// Replaces an instance's overrides → `Response::Done`.
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
            },
            ..Settings::default()
        };
        let overrides = Overrides {
            window: Some(WindowSettings {
                width: 1280,
                height: 720,
                maximized: true,
            }),
            ..Overrides::default()
        };
        let effective = global.with_overrides(&overrides);
        assert_eq!(effective.memory, global.memory);
        assert_eq!(effective.window.width, 1280);
        assert!(effective.window.maximized);
        assert_eq!(global.with_overrides(&Overrides::default()), global);
    }
}
