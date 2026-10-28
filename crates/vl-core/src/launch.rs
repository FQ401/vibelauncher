//! Starting and stopping the game.
//!
//! `LaunchCommand::Launch` answers `Response::Task` for the preparation; then the instance moves
//! `Preparing → Running → Stopped` (`LaunchEvent::StateChanged`), the game's output arrives as
//! `Event::LogBatch` with `LogSource::Game` (groups of lines, a few per second), and the end of the process as `LaunchEvent::Exited`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::crash::CrashDiagnosis;
use crate::ids::{AccountId, InstanceId};
use crate::instance::InstanceState;

/// What to launch and as whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRequest {
    /// The instance.
    pub instance: InstanceId,
    /// The account; `None` uses the default account.
    pub account: Option<AccountId>,
    /// Launch without network access to accounts (the stored profile, no token refresh).
    pub offline: bool,
}

/// How a game process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameExit {
    /// The exit code, when the OS reports one.
    pub code: Option<i32>,
    /// Whether the launcher considers it a crash: a non-zero exit, a new crash report or a new
    /// JVM error log, unless the user stopped the game.
    pub crashed: bool,
    /// Whether the user stopped it.
    pub stopped_by_user: bool,
    /// How long the process ran, in milliseconds.
    #[serde(default)]
    pub session_ms: u64,
    /// The crash report the game wrote during this run (`crash-reports/`), when there is one.
    #[serde(default)]
    pub crash_report: Option<PathBuf>,
    /// The JVM fatal error log (`hs_err_pid*.log`) written during this run, when there is one.
    #[serde(default)]
    pub hs_err: Option<PathBuf>,
    /// The file with the full output of this run.
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    /// The last lines of the output, oldest first.
    #[serde(default)]
    pub tail: Vec<String>,
}

/// Launch commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LaunchCommand {
    /// Prepares and starts the game → `Response::Task`.
    Launch(LaunchRequest),
    /// Stops the instance's game process → `Response::Done`.
    Stop {
        /// The instance.
        instance: InstanceId,
    },
    /// Reads the instance's newest logs (`latest.log`, the newest crash report and JVM error
    /// log, the launcher's copy of the last run's output) and diagnoses them, without launching
    /// → `Response::Diagnosis`.
    Diagnose {
        /// The instance.
        instance: InstanceId,
    },
}

/// Game process lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LaunchEvent {
    /// An instance's process state changed.
    StateChanged {
        /// The instance.
        instance: InstanceId,
        /// The new state.
        state: InstanceState,
    },
    /// An instance's game process ended.
    Exited {
        /// The instance.
        instance: InstanceId,
        /// How it ended.
        exit: GameExit,
    },
    /// The logs of a crashed run were analysed; follows `Exited` of the same run.
    Diagnosed {
        /// The instance.
        instance: InstanceId,
        /// The diagnosis.
        diagnosis: CrashDiagnosis,
    },
}
