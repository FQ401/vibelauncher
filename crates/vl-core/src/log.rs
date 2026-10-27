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

/// One line of a [`LogBatch`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// When the launcher received the line, milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// Its level.
    pub level: LogLevel,
    /// The text, without the trailing newline.
    pub text: String,
}

/// Game output delivered in groups instead of one event per line, so a chatty game cannot flood
/// a front end. The full log is in the log file of the launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogBatch {
    /// Where the lines come from.
    pub source: LogSource,
    /// The lines in output order.
    pub lines: Vec<LogEntry>,
    /// Lines left out of this batch (and of the stream) because the game wrote faster than a
    /// front end can show; they are in the log file.
    pub skipped: u64,
}
