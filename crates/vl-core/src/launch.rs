//! Starting and stopping the game.
//!
//! `LaunchCommand::Launch` answers `Response::Task` for the preparation; then the instance moves
//! `Preparing → Running → Stopped` (`LaunchEvent::StateChanged`), the game's output arrives as
//! `Event::Log` with `LogSource::Game`, and the end of the process as `LaunchEvent::Exited`.

use serde::{Deserialize, Serialize};

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
    /// Whether the launcher considers it a crash.
    pub crashed: bool,
    /// Whether the user stopped it.
    pub stopped_by_user: bool,
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
}
