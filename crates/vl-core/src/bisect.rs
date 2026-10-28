//! Mod bisection: finding the mod that makes the game crash by switching halves of an
//! instance's mods off and on.
//!
//! A session is one search. Each step enables a set of mods (the rest stay disabled); the user
//! starts the game and reports what happened with [`BisectCommand::Report`]. Ending a session
//! in any way puts every mod back to the state it had when the session started.

use serde::{Deserialize, Serialize};

use crate::InstanceId;

/// What happened when the game ran with a step's mods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectOutcome {
    /// The problem happened.
    Bad,
    /// The game ran without the problem.
    Good,
    /// The step says nothing: the game failed before the problem could show, or the run was
    /// not finished.
    Skipped,
}

/// The result of a finished search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectVerdict {
    /// Removing any file of `culprits` makes the problem go away.
    Found {
        /// The files that cause the problem together (one file in the common case).
        culprits: Vec<String>,
        /// Files the culprits need to run (their required dependencies).
        needs: Vec<String>,
    },
    /// The problem did not happen with every mod enabled.
    NotReproduced,
    /// Skipped steps left the search without an answer; the cause is among `suspects`.
    Inconclusive {
        /// The files the search narrowed the cause down to.
        suspects: Vec<String>,
    },
}

/// Where a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectState {
    /// A step is applied and waits for the user's report.
    Running,
    /// The search is over, the mods are back to their original state.
    Finished,
}

/// A snapshot of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectStatus {
    /// Where the session stands.
    pub state: BisectState,
    /// Reports received so far.
    pub steps_done: u32,
    /// About how many more steps are needed, assuming every step halves the suspects.
    pub steps_estimate: u32,
    /// Mod files under test (those enabled when the session started).
    pub candidates: u32,
    /// Files the cause is currently narrowed down to.
    pub suspects: Vec<String>,
    /// Files enabled in the current step (empty when finished).
    pub enabled: Vec<String>,
    /// Number of candidate files disabled in the current step.
    pub disabled: u32,
    /// The result, once finished.
    pub verdict: Option<BisectVerdict>,
}

/// Commands on bisection sessions; at most one session exists per instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectCommand {
    /// Starts a session over the instance's enabled mods and applies the first step
    /// → `Response::Bisect`. Fails when an unfinished session exists.
    Start {
        /// The instance.
        instance: InstanceId,
        /// Mod ids to try first (for example from a crash diagnosis); may be empty.
        suspects: Vec<String>,
    },
    /// The current session → `Response::Bisect`; `NotFound` when there is none.
    Status {
        /// The instance.
        instance: InstanceId,
    },
    /// Reports what the current step did and applies the next one → `Response::Bisect`.
    Report {
        /// The instance.
        instance: InstanceId,
        /// What happened.
        outcome: BisectOutcome,
    },
    /// Ends the session, finished or not, and restores the mods → `Response::Done`.
    Stop {
        /// The instance.
        instance: InstanceId,
    },
}
