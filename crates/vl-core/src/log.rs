//! Log lines: the launcher's own log and each game process's output.

use serde::{Deserialize, Serialize};

use crate::ids::InstanceId;

/// Where a log line comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    /// The launcher itself.
    Launcher,
    /// A game process of an instance.
    Game(InstanceId),
}

/// The level of a log line; game output that has none is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Unparsed game output.
    Unknown,
    /// Trace.
    Trace,
    /// Debug.
    Debug,
    /// Info.
    Info,
    /// Warning.
    Warn,
    /// Error.
    Error,
}

/// One log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Where it comes from.
    pub source: LogSource,
    /// Its level.
    pub level: LogLevel,
    /// The text, without the trailing newline.
    pub text: String,
}
