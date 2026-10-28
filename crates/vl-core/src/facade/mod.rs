//! The facade: the only way front ends reach the launcher.
//!
//! - [`Facade::call`] takes a [`Command`] and returns at once with a [`Reply`] that later holds
//!   exactly one `Result<Response>`. A command that takes long answers `Response::Task(id)` and
//!   reports the rest as task events.
//! - [`Facade::subscribe`] opens an [`EventStream`] that receives every [`Event`] published after
//!   it was opened: task progress, logs, instance and account changes.
//!
//! Nothing here needs an async runtime: an immediate-mode UI polls the reply and drains the event
//! stream once per frame, the CLI blocks on them. Commands, responses and events are plain data
//! (serializable), so the same contract can be carried over a process boundary.

mod channel;

use serde::{Deserialize, Serialize};

pub use channel::{EventBus, EventStream, Replier, Reply, reply_channel};

use crate::account::{Account, AccountCommand, AccountEvent};
use crate::bisect::{AutoBisectReport, BisectCommand, BisectEvent, BisectStatus};
use crate::catalog::{CatalogCommand, HashMatch, Project, ProjectVersion, SearchPage};
use crate::crash::CrashDiagnosis;
use crate::ids::TaskId;
use crate::install::{
    IdentifyReport, InstallPlan, PackChanges, PackExport, PackUpdateInfo, UpdateReport,
};
use crate::instance::{InstanceCommand, InstanceEvent, InstanceSummary};
use crate::java::{JavaCommand, JavaEvent, JavaRuntime};
use crate::launch::{LaunchCommand, LaunchEvent};
use crate::log::{LogBatch, LogLine};
use crate::resources::{ResourceCommand, ResourceEntry, WorldInfo};
use crate::settings::{Overrides, Settings, SettingsCommand, SettingsEvent};
use crate::storage::{CleanupReport, PathCheck, StorageCommand, StorageInfo, StorageSuggestion};
use crate::task::{TaskCommand, TaskEvent, TaskSnapshot};
use crate::version::{GameVersion, LoaderVersionInfo, VersionCommand, VersionEvent};

/// The launcher behind a front end: the real backend or the mock.
pub trait Facade: Send + Sync {
    /// Sends a command; never blocks. The reply holds exactly one result.
    fn call(&self, command: Command) -> Reply;

    /// Opens a stream of the events published from now on.
    fn subscribe(&self) -> EventStream;
}

/// A request to the launcher. Each domain's commands live in its module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
// Settings values are large and rarely sent; boxing them would only complicate every caller.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Which backend this is → `Response::Hello`.
    Hello,
    /// Instances.
    Instances(InstanceCommand),
    /// Game versions.
    Versions(VersionCommand),
    /// Accounts.
    Accounts(AccountCommand),
    /// Java runtimes.
    Java(JavaCommand),
    /// Settings.
    Settings(SettingsCommand),
    /// Launching and stopping the game.
    Launch(LaunchCommand),
    /// Tasks.
    Tasks(TaskCommand),
    /// Mods, packs and other instance resources.
    Resources(ResourceCommand),
    /// Mod bisection.
    Bisect(BisectCommand),
    /// The online mod catalog.
    Catalog(CatalogCommand),
    /// Where the data is kept.
    Storage(StorageCommand),
}

/// The answer to a [`Command`]; each command documents which variant it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
// See `Command`.
#[allow(clippy::large_enum_variant)]
pub enum Response {
    /// The command is done and returns nothing.
    Done,
    /// The command started a task; follow it on the event stream.
    Task(TaskId),
    /// The backend's identity.
    Hello(BackendInfo),
    /// Instances.
    Instances(Vec<InstanceSummary>),
    /// One instance.
    Instance(InstanceSummary),
    /// Game versions.
    Versions(Vec<GameVersion>),
    /// Versions of a mod loader for a game version, newest first.
    LoaderVersions(Vec<LoaderVersionInfo>),
    /// Accounts.
    Accounts(Vec<Account>),
    /// One account.
    Account(Account),
    /// Java runtimes.
    Javas(Vec<JavaRuntime>),
    /// Global settings.
    Settings(Settings),
    /// An instance's settings.
    InstanceSettings {
        /// The instance's overrides.
        overrides: Overrides,
        /// The global settings with the overrides applied.
        effective: Settings,
    },
    /// Running tasks.
    Tasks(Vec<TaskSnapshot>),
    /// The entries of a resource folder.
    Resources(Vec<ResourceEntry>),
    /// The worlds of an instance.
    Worlds(Vec<WorldInfo>),
    /// A bisection session.
    Bisect(BisectStatus),
    /// The report of an automatic bisection.
    BisectReport(AutoBisectReport),
    /// A page of catalog search results.
    CatalogSearch(SearchPage),
    /// One catalog project.
    CatalogProject(Project),
    /// Versions of a catalog project.
    CatalogVersions(Vec<ProjectVersion>),
    /// Versions found by file hash.
    CatalogHashMatches(Vec<HashMatch>),
    /// What installing a project would do.
    InstallPlan(InstallPlan),
    /// The result of an update check.
    UpdateReport(UpdateReport),
    /// The result of identifying files.
    Identified(IdentifyReport),
    /// The result of checking a modpack for a newer version.
    PackUpdate(PackUpdateInfo),
    /// What updating a modpack would change.
    PackChanges(PackChanges),
    /// What a modpack export wrote.
    PackExported(PackExport),
    /// The diagnosis of an instance's newest logs.
    Diagnosis(CrashDiagnosis),
    /// The storage layout.
    Storage(StorageInfo),
    /// The result of checking a folder.
    PathCheck(PathCheck),
    /// What a storage cleanup did.
    StorageCleaned(CleanupReport),
    /// The suggested data folder.
    StorageSuggestion(StorageSuggestion),
}

/// Something that happened in the launcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// Task lifecycle and progress.
    Task(TaskEvent),
    /// A launcher or game log line.
    Log(LogLine),
    /// A group of game output lines.
    LogBatch(LogBatch),
    /// Instance list changes.
    Instance(InstanceEvent),
    /// Game process lifecycle.
    Launch(LaunchEvent),
    /// Account changes and sign-in prompts.
    Account(AccountEvent),
    /// Version list changes.
    Version(VersionEvent),
    /// Java runtime list changes.
    Java(JavaEvent),
    /// Settings changes.
    Settings(SettingsEvent),
    /// Automatic bisection progress.
    Bisect(BisectEvent),
    /// This stream fell behind and `missed` events were dropped; state shown from events should
    /// be re-read with commands.
    Lagged {
        /// How many events were dropped.
        missed: u64,
    },
}

/// Which backend answers the facade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    /// The backend's name (`vl-backend`, `vl-mock`).
    pub name: String,
    /// The launcher version.
    pub version: String,
    /// Whether the data is synthetic.
    pub mock: bool,
}
