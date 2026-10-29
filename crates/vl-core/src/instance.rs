//! Instances: what the launcher lists, creates, renames and deletes.

use serde::{Deserialize, Serialize};

use crate::component::ComponentKind;
use crate::ids::{InstanceId, TaskId};
use crate::trash::PurgeTarget;

/// The mod loader an instance runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "version")]
#[non_exhaustive]
pub enum Loader {
    /// The game without a mod loader.
    Vanilla,
    /// Fabric Loader of the given version.
    Fabric(String),
    /// Quilt Loader of the given version.
    Quilt(String),
    /// NeoForge of the given version.
    NeoForge(String),
    /// Forge of the given version.
    Forge(String),
}

impl std::fmt::Display for Loader {
    /// `Fabric 0.20.1`, or `none` for the game without a loader.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Loader::Vanilla => f.write_str("none"),
            Loader::Fabric(v) => write!(f, "Fabric {v}"),
            Loader::Quilt(v) => write!(f, "Quilt {v}"),
            Loader::NeoForge(v) => write!(f, "NeoForge {v}"),
            Loader::Forge(v) => write!(f, "Forge {v}"),
        }
    }
}

/// What an instance's game process is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstanceState {
    /// No game process.
    Stopped,
    /// The launch task is checking and downloading files.
    Preparing {
        /// The launch task.
        task: TaskId,
    },
    /// The game process is running.
    Running {
        /// The OS process id, when known.
        pid: Option<u32>,
    },
}

/// One row of the instance list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSummary {
    /// The instance's identifier.
    pub id: InstanceId,
    /// The display name.
    pub name: String,
    /// The group the instance is shown in, if any.
    pub group: Option<String>,
    /// The game version id (as in the version manifest).
    pub game_version: String,
    /// The mod loader.
    pub loader: Loader,
    /// Total play time in seconds.
    pub play_time_secs: u64,
    /// When the game was last started, in Unix seconds.
    pub last_played_unix: Option<i64>,
    /// What the game process is doing.
    pub state: InstanceState,
    /// Why the instance cannot be used (a damaged config), if it cannot.
    pub problem: Option<String>,
}

/// A mod loader to put on an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderRequest {
    /// Which loader; must be a loader, not the game.
    pub kind: ComponentKind,
    /// The loader version; `None` picks the newest stable version for the game version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The parameters of a new instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewInstance {
    /// The display name; the backend derives a free directory name from it.
    pub name: String,
    /// The game version id.
    pub game_version: String,
    /// The group to show the instance in.
    pub group: Option<String>,
    /// The mod loader to install on it; `None` creates a vanilla instance.
    #[serde(default)]
    pub loader: Option<LoaderRequest>,
}

/// Instance commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstanceCommand {
    /// All instances → `Response::Instances`.
    List,
    /// Creates an instance → `Response::Instance`. With a loader the answer waits for the
    /// loader's version list.
    Create(NewInstance),
    /// Replaces the instance's loader (`None` removes it) → `Response::Instance`. The loader
    /// must support the instance's game version.
    SetLoader {
        /// The instance.
        id: InstanceId,
        /// The new loader.
        loader: Option<LoaderRequest>,
    },
    /// Renames an instance and its directory → `Response::Instance`.
    Rename {
        /// The instance.
        id: InstanceId,
        /// The new display name.
        name: String,
    },
    /// Deletes an instance → `Response::Done`. Refused while its game runs or a task works on
    /// it. The instance goes to the launcher's trash with `comment`, unless `permanent` erases it
    /// at once.
    Delete {
        /// The instance.
        id: InstanceId,
        /// What the user wrote about it (at most [`crate::trash::COMMENT_MAX_CHARS`] characters).
        #[serde(default)]
        comment: String,
        /// Erase it at once instead of keeping it in the trash.
        #[serde(default)]
        permanent: bool,
    },
    /// The deleted instances → `Response::Trash`.
    ListTrash,
    /// Puts a deleted instance back → `Response::Instance`. A taken id gets a suffix (`-2`), and
    /// a display name another instance has gets one too (` (2)`).
    Restore {
        /// The trash item.
        item: String,
    },
    /// Erases deleted instances → `Response::TrashPurged`.
    Purge(PurgeTarget),
}

/// Changes to the instance list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstanceEvent {
    /// An instance was created or found.
    Added(InstanceSummary),
    /// An instance's summary changed (name, play time, state).
    Changed(InstanceSummary),
    /// An instance was deleted.
    Removed(InstanceId),
    /// The trash changed (an item was added, restored or purged); read it with
    /// `InstanceCommand::ListTrash`.
    TrashChanged,
}

#[cfg(test)]
mod loader_display_tests {
    use super::Loader;

    #[test]
    fn loader_is_shown_in_plain_words() {
        assert_eq!(Loader::Fabric("0.20.1".into()).to_string(), "Fabric 0.20.1");
        assert_eq!(
            Loader::NeoForge("21.1.5".into()).to_string(),
            "NeoForge 21.1.5"
        );
        assert_eq!(Loader::Vanilla.to_string(), "none");
    }
}
