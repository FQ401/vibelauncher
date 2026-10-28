//! Mod bisection: finding the mod that makes the game crash by switching halves of an
//! instance's mods off and on.
//!
//! A session is one search. Each step enables a set of mods (the rest stay disabled); the user
//! starts the game and reports what happened with [`BisectCommand::Report`]. Ending a session
//! in any way puts every mod back to the state it had when the session started.
//!
//! [`BisectCommand::Auto`] runs the whole search as a task: the launcher starts the game for each
//! step, decides the outcome from the process exit, the crash files and the diagnosis, and
//! publishes [`BisectEvent`]s; the result is an [`AutoBisectReport`].

use serde::{Deserialize, Serialize};

use crate::InstanceId;
use crate::crash::CrashDiagnosis;
use crate::ids::AccountId;

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
    /// Runs the whole search automatically as a `Bisect` task → `Response::Task`. Each run is
    /// published as `BisectEvent::Run`, the result as `BisectEvent::Finished`. Cancelling the
    /// task stops the game and restores the mods and the world.
    Auto(AutoBisectRequest),
    /// The report of the last automatic search → `Response::BisectReport`; `NotFound` when there
    /// is none.
    AutoReport {
        /// The instance.
        instance: InstanceId,
    },
}

/// How far the game must get in an automatic run for the run to count as good.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectTarget {
    /// The main menu, then the stable time.
    Menu,
    /// Inside a world (Quick Play, Minecraft 1.20 and later), then the stable time. Older
    /// versions run semi-manually: the launcher watches for the crash and the player closes the
    /// game once it works.
    World,
}

/// An automatic search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoBisectRequest {
    /// The instance.
    pub instance: InstanceId,
    /// The account; `None` uses the default account.
    pub account: Option<AccountId>,
    /// Launch without network access to accounts.
    pub offline: bool,
    /// How far a run must get.
    pub target: BisectTarget,
    /// The world (folder name in `saves/`) for [`BisectTarget::World`]. It is copied before the
    /// search and put back after every run.
    pub world: Option<String>,
    /// Seconds the game must keep running after reaching the target.
    pub stable_secs: u32,
    /// Clean runs in a row needed before a set counts as good (a crash counts at once).
    pub repeats: u32,
    /// Seconds a run may take to reach the target before it counts as a hang.
    pub timeout_secs: u32,
    /// The game has just crashed: the newest logs are the problem to search for, and they count
    /// as the check that it reproduces.
    pub after_crash: bool,
}

impl AutoBisectRequest {
    /// Default [`AutoBisectRequest::stable_secs`].
    pub const DEFAULT_STABLE_SECS: u32 = 20;
    /// Default [`AutoBisectRequest::repeats`].
    pub const DEFAULT_REPEATS: u32 = 1;
    /// Default [`AutoBisectRequest::timeout_secs`].
    pub const DEFAULT_TIMEOUT_SECS: u32 = 600;

    /// A search up to the main menu with the default times.
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            account: None,
            offline: false,
            target: BisectTarget::Menu,
            world: None,
            stable_secs: Self::DEFAULT_STABLE_SECS,
            repeats: Self::DEFAULT_REPEATS,
            timeout_secs: Self::DEFAULT_TIMEOUT_SECS,
            after_crash: false,
        }
    }
}

/// Why an automatic run was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunPurpose {
    /// Every mod: does the problem happen?
    Reproduce,
    /// No mod: is it the mods at all?
    WithoutMods,
    /// A step of the search.
    Step,
    /// Every mod but the culprits: the final check.
    Verify,
}

/// What one automatic run showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunResult {
    /// The game reached the target and kept running for the stable time (semi-manual: the
    /// player closed it without a crash).
    Reached,
    /// The game crashed with the problem under search.
    Crashed,
    /// The game crashed in another way; the step says nothing.
    OtherCrash,
    /// The game did not reach the target in time.
    Hung,
    /// The game ended before the target without a crash.
    Closed,
}

/// One automatic run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectRun {
    /// Why it was made.
    pub purpose: RunPurpose,
    /// How many of the searched mod files were enabled.
    pub enabled: u32,
    /// What it showed.
    pub result: RunResult,
    /// The crash key (signature id and exception class) of a crash.
    pub crash: Option<String>,
    /// How long the game ran, in seconds.
    pub seconds: u32,
}

/// The result of an automatic search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AutoVerdict {
    /// Removing any file of `culprits` makes the problem go away.
    Found {
        /// The files that cause the problem together.
        culprits: Vec<String>,
        /// Files the culprits need to run.
        needs: Vec<String>,
        /// Whether every mod but the culprits (and the mods that need them) ran clean.
        confirmed: bool,
    },
    /// The problem did not happen with every mod enabled.
    NotReproduced,
    /// The game hangs with every mod enabled; a hang is not searched.
    Hangs,
    /// Skipped steps left the search without an answer; the cause is among `suspects`.
    Inconclusive {
        /// The files the search narrowed the cause down to.
        suspects: Vec<String>,
    },
    /// The problem is not in the mods: the game fails without them too, or the diagnosis points
    /// at the setup (Java, memory, graphics driver, game files).
    NotMods {
        /// The diagnosis of that failure.
        diagnosis: Option<CrashDiagnosis>,
    },
    /// The mod loader named the problem itself; no search is needed.
    LoaderReported {
        /// The diagnosis with the loader's message.
        diagnosis: CrashDiagnosis,
    },
}

/// What an automatic search did and found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoBisectReport {
    /// The result.
    pub verdict: AutoVerdict,
    /// The diagnosis of the problem searched for.
    pub reference: Option<CrashDiagnosis>,
    /// Whether the player confirmed the good runs (a world target on a version without Quick
    /// Play).
    pub semi_manual: bool,
    /// Every run, in order.
    pub runs: Vec<BisectRun>,
}

/// Progress of an automatic search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BisectEvent {
    /// A run ended.
    Run {
        /// The instance.
        instance: InstanceId,
        /// The run.
        run: BisectRun,
        /// Search steps answered so far.
        steps_done: u32,
        /// About how many more steps are needed.
        steps_estimate: u32,
    },
    /// The search ended; the mods and the world are restored.
    Finished {
        /// The instance.
        instance: InstanceId,
        /// What it found.
        report: Box<AutoBisectReport>,
    },
}
