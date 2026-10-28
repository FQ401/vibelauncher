//! Crash diagnosis results.
//!
//! After a game run ends in a crash the backend reads the run's logs and publishes
//! `LaunchEvent::Diagnosed`; `LaunchCommand::Diagnose` does the same on demand for the newest
//! logs of an instance and answers `Response::Diagnosis`. The texts live in the interface locales;
//! a diagnosis carries only keys and values for their placeholders.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How sure the diagnosis is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiagnosisLevel {
    /// A known signature matched: the cause is stated with confidence.
    Signature,
    /// No signature; the stack touches exactly one mod ("possibly").
    SingleMod,
    /// No signature; several mods in the stack (a list, no verdict).
    SeveralMods,
    /// Nothing found.
    Undetermined,
}

/// What kind of problem the logs point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CrashCause {
    /// A mod or a mod combination is the likely cause; a mod search (bisection) can help.
    ModLikely,
    /// The setup is at fault (Java, memory, graphics driver, damaged game files); removing mods
    /// does not help.
    Environment,
    /// Nothing points anywhere.
    Unknown,
}

/// Which file a line of evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceSource {
    /// The game output (`latest.log` or the launcher's copy of the run's output).
    Log,
    /// A `crash-reports/*.txt` file.
    CrashReport,
    /// A JVM `hs_err_pid*.log` file.
    HsErr,
}

/// A log line that supports the diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLine {
    /// The file.
    pub source: EvidenceSource,
    /// 1-based line number in the text that was analysed (the tail of a long log).
    pub line: u32,
    /// The line, trimmed and shortened.
    pub text: String,
}

/// The diagnosis of a failed run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashDiagnosis {
    /// How sure it is.
    pub level: DiagnosisLevel,
    /// The kind of problem.
    pub cause: CrashCause,
    /// Whether the mod loader itself named the problem, so no culprit search is needed.
    pub loader_reported: bool,
    /// The id of the matched signature.
    pub signature: Option<String>,
    /// Mod ids involved.
    pub mods: Vec<String>,
    /// Localization key of the explanation.
    pub message_key: String,
    /// Localization key of the suggested action.
    pub action_key: Option<String>,
    /// Values for the message placeholders (`dependency`, `java_major`, `expected`, `found`, ...).
    pub args: BTreeMap<String, String>,
    /// The lines that justify the verdict.
    pub evidence: Vec<EvidenceLine>,
    /// Names of the files that were analysed.
    pub files: Vec<String>,
}

impl CrashDiagnosis {
    /// Whether searching the mods by bisection is worth offering: a mod is the likely cause and
    /// the loader did not already name it.
    pub fn bisect_useful(&self) -> bool {
        self.cause == CrashCause::ModLikely && !self.loader_reported
    }
}
