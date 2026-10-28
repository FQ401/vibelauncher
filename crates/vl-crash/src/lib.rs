//! Crash diagnosis: game logs in, a [`Diagnosis`] out.
//!
//! Pure text analysis: no files, no network, no clock. The caller reads `latest.log`, a
//! `crash-reports/*.txt` file or `hs_err_pid*.log` and passes the text together with a
//! [`ClassIndex`] that maps classes to the mods of the instance. The result carries a confidence
//! [`Level`], message and action keys for localization (texts live in the interface locales),
//! the mods involved, and the log lines that justify the verdict.
//!
//! A mod is only named with certainty when a signature matched ([`Level::Signature`]). A stack
//! that touches one mod gives [`Level::SingleMod`] ("possibly"), several mods give
//! [`Level::SeveralMods`] (a list, no verdict), and everything else is [`Level::Undetermined`].
//! The input is hostile: nothing here indexes, unwraps or panics on odd text.

mod index;
mod signatures;
mod stack;

pub use index::{ClassIndex, ModInfo};
pub use signatures::{Signature, Signatures};

use std::collections::BTreeMap;

/// Longest evidence line kept, in characters.
const MAX_EVIDENCE_CHARS: usize = 300;
/// Most evidence lines in one diagnosis.
const MAX_EVIDENCE_LINES: usize = 8;

/// The text of the available game output; empty strings mean "not available".
#[derive(Debug, Clone, Copy, Default)]
pub struct CrashInput<'a> {
    /// `logs/latest.log`.
    pub latest_log: &'a str,
    /// A `crash-reports/*.txt` file.
    pub crash_report: &'a str,
    /// A `hs_err_pid*.log` file.
    pub hs_err: &'a str,
}

/// Where a line of evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    LatestLog,
    CrashReport,
    HsErr,
}

/// How sure the diagnosis is. Ordered from most to least certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 1: a known signature matched; state the cause and one action with confidence.
    Signature,
    /// 2: no signature, the stack touches exactly one mod; say "possibly".
    SingleMod,
    /// 3: no signature, several mods in the stack; list them without a verdict.
    SeveralMods,
    /// 4: nothing found; say so and offer to export the report.
    Undetermined,
}

impl Level {
    /// The level number, 1 to 4.
    pub fn number(self) -> u8 {
        match self {
            Level::Signature => 1,
            Level::SingleMod => 2,
            Level::SeveralMods => 3,
            Level::Undetermined => 4,
        }
    }
}

/// What kind of problem the logs point at; decides whether searching the mods can help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A mod (or a mod combination) is the likely cause: bisection can find it.
    ModLikely,
    /// The setup is at fault: wrong Java, not enough memory, a graphics driver crash, damaged
    /// game files. Removing mods does not help.
    Environment,
    /// Nothing points anywhere.
    Unknown,
}

/// A line of the logs that supports the diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceLine {
    pub source: Source,
    /// 1-based line number in that source.
    pub line: usize,
    /// The line, trimmed and shortened.
    pub text: String,
}

/// The result of [`diagnose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub level: Level,
    /// The id of the matched signature (level 1 only).
    pub signature: Option<String>,
    /// The kind of problem.
    pub cause: Cause,
    /// Whether the mod loader itself named the problem (its incompatibility or loading error
    /// message); then no search for the culprit is needed.
    pub loader_reported: bool,
    /// Mod ids involved: named by the signature, or found in the stack.
    pub culprit_mods: Vec<String>,
    /// Localization key of the explanation.
    pub message_key: String,
    /// Localization key of the one suggested action.
    pub action_key: Option<String>,
    /// Values for the message placeholders (`dependency`, `java_major`, ...).
    pub args: BTreeMap<String, String>,
    pub evidence_lines: Vec<EvidenceLine>,
}

/// Every localization key [`diagnose`] can return, signature keys and stack keys alike.
pub fn localization_keys() -> Vec<String> {
    let mut keys: Vec<String> = Signatures::builtin()
        .keys()
        .into_iter()
        .map(str::to_owned)
        .collect();
    keys.extend(stack::KEYS.iter().map(|k| (*k).to_owned()));
    keys.sort();
    keys.dedup();
    keys
}

/// Diagnoses a failed launch with the built-in signature set.
pub fn diagnose<I: ClassIndex + ?Sized>(input: &CrashInput<'_>, mods: &I) -> Diagnosis {
    diagnose_with(Signatures::builtin(), input, mods)
}

/// Diagnoses a failed launch with a given signature set.
pub fn diagnose_with<I: ClassIndex + ?Sized>(
    signatures: &Signatures,
    input: &CrashInput<'_>,
    mods: &I,
) -> Diagnosis {
    let crash_report = strip_loading_reports(input.crash_report);
    let latest_log = strip_loading_reports(input.latest_log);
    let sources = [
        (Source::CrashReport, &*crash_report),
        (Source::HsErr, input.hs_err),
        (Source::LatestLog, &*latest_log),
    ];
    if let Some(hit) = signatures.find(&sources) {
        return hit_to_diagnosis(hit);
    }
    stack::suspects(
        &CrashInput {
            latest_log: &latest_log,
            crash_report: &crash_report,
            hs_err: input.hs_err,
        },
        mods,
    )
}

/// The marker of the loading report that Forge 1.7.10 prints on a normal start.
const LOADING_REPORT_MARKER: &str = "THIS IS NOT A ERROR";
const REPORT_HEADER: &str = "---- Minecraft Crash Report ----";
/// How far below its header a report may carry the marker.
const MARKER_WINDOW: usize = 12;

/// Removes the "Minecraft Crash Report" blocks that say `THIS IS NOT A ERROR`: Forge 1.7.10
/// prints one while loading, and it describes no failure. A block runs from its header to the
/// next log line (one that starts with `[`) or to the end of the text.
fn strip_loading_reports(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(LOADING_REPORT_MARKER) {
        return std::borrow::Cow::Borrowed(text);
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let window_end = (i + 1 + MARKER_WINDOW).min(lines.len());
        if line.contains(REPORT_HEADER)
            && lines[i + 1..window_end]
                .iter()
                .any(|l| l.contains(LOADING_REPORT_MARKER))
        {
            i += 1;
            while i < lines.len() && !lines[i].starts_with('[') {
                i += 1;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

fn hit_to_diagnosis(hit: signatures::Hit) -> Diagnosis {
    let mut args = hit.args;
    // A class file version N needs Java N - 44 (52 is Java 8, 65 is Java 21).
    let from_class = args
        .get("class_version")
        .and_then(|v| v.parse::<u32>().ok())
        .and_then(|v| v.checked_sub(44))
        .filter(|v| *v >= 1);
    if let Some(java) = from_class {
        args.insert("java_major".to_owned(), java.to_string());
    } else if let Some(required) = args.get("required").cloned() {
        args.insert("java_major".to_owned(), required);
    }
    Diagnosis {
        level: Level::Signature,
        signature: Some(hit.id),
        cause: hit.cause,
        loader_reported: hit.loader_reported,
        culprit_mods: hit.mods,
        message_key: hit.message_key,
        action_key: hit.action_key,
        args,
        evidence_lines: hit.evidence,
    }
}

pub(crate) fn evidence(source: Source, line: usize, text: &str) -> EvidenceLine {
    let text: String = text.trim().chars().take(MAX_EVIDENCE_CHARS).collect();
    EvidenceLine { source, line, text }
}

pub(crate) fn push_evidence(list: &mut Vec<EvidenceLine>, line: EvidenceLine) {
    if list.len() < MAX_EVIDENCE_LINES && !list.contains(&line) {
        list.push(line);
    }
}
