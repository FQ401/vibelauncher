//! Long operations as front ends see them: tasks with progress, cancellation and an outcome.
//!
//! A command that takes long answers `Response::Task(id)` at once; the task then reports
//! [`TaskEvent`]s on the event stream until exactly one `Finished`.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::ids::TaskId;

/// What a task does; front ends name tasks from this (localized), plus [`TaskInfo::subject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskKind {
    /// Preparing and starting the game.
    Launch,
    /// Installing a game version's files.
    InstallVersion,
    /// Downloading files.
    Download,
    /// Installing a catalog project and its dependencies into an instance.
    InstallResource,
    /// Installing a modpack as a new instance.
    InstallPack,
    /// Updating the modpack of an instance.
    UpdatePack,
    /// Refreshing the version list.
    RefreshVersions,
    /// Signing in to an account.
    SignIn,
    /// Scanning for Java runtimes.
    JavaScan,
    /// Downloading a Java runtime.
    JavaDownload,
    /// Moving a storage section to another folder.
    MoveStorage,
    /// Checking an instance's game files and repairing them.
    VerifyFiles,
    /// Searching the mods for the cause of a crash, launching the game for each step.
    Bisect,
}

/// A task as announced when it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// The task.
    pub id: TaskId,
    /// The task this one is a subtask of.
    pub parent: Option<TaskId>,
    /// What it does.
    pub kind: TaskKind,
    /// What it works on (an instance name, a version id), if anything.
    pub subject: Option<String>,
    /// Whether `TaskCommand::Cancel` can stop it.
    pub cancellable: bool,
}

/// The unit of [`Progress::done`] and [`Progress::total`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressUnit {
    /// Bytes.
    Bytes,
    /// Items (files, libraries).
    Items,
    /// Abstract steps.
    Steps,
}

/// How far a task is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Work done.
    pub done: u64,
    /// Total work, when known.
    pub total: Option<u64>,
    /// The unit of `done` and `total`.
    pub unit: ProgressUnit,
    /// The current stage (English technical text, e.g. `libraries`), if any.
    pub stage: Option<String>,
}

impl Progress {
    /// The fraction done in `0.0..=1.0`, when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(1.0);
        }
        #[allow(clippy::cast_precision_loss)] // progress bars need no 64-bit precision
        let fraction = self.done.min(total) as f32 / total as f32;
        Some(fraction)
    }
}

/// How a task ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    /// It did its work.
    Succeeded,
    /// It failed.
    Failed(Error),
    /// It was cancelled.
    Cancelled,
}

/// A running task and its latest progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// The task.
    pub info: TaskInfo,
    /// Its latest progress, if it reported any.
    pub progress: Option<Progress>,
}

/// Task commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskCommand {
    /// Running tasks → `Response::Tasks`.
    List,
    /// Asks a task and its subtasks to stop → `Response::Done`; the task ends with
    /// `TaskOutcome::Cancelled` unless it finishes first.
    Cancel {
        /// The task.
        id: TaskId,
    },
}

/// Task lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskEvent {
    /// A task started.
    Started(TaskInfo),
    /// A task made progress.
    Progress {
        /// The task.
        id: TaskId,
        /// Its progress.
        progress: Progress,
    },
    /// A task ended; no more events follow for it.
    Finished {
        /// The task.
        id: TaskId,
        /// How it ended.
        outcome: TaskOutcome,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_is_clamped_and_needs_a_total() {
        let mut p = Progress {
            done: 5,
            total: Some(10),
            unit: ProgressUnit::Items,
            stage: None,
        };
        assert_eq!(p.fraction(), Some(0.5));
        p.done = 50;
        assert_eq!(p.fraction(), Some(1.0));
        p.total = Some(0);
        assert_eq!(p.fraction(), Some(1.0));
        p.total = None;
        assert_eq!(p.fraction(), None);
    }
}
