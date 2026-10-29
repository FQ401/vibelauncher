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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Unparsed game output.
    #[default]
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

/// One line of game output: an entry of a [`LogBatch`] and of the console buffer
/// ([`ConsoleBuffer`](crate::console::ConsoleBuffer)).
///
/// A record is a line with its header (level, thread, logger, time as the game wrote them) and
/// the lines that continue it: further message lines and a stack trace. Continuation lines carry
/// the level of their record and the `seq` of its first line in `record`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// When the launcher received the line, milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// Its level.
    pub level: LogLevel,
    /// The text, without the trailing newline.
    pub text: String,
    /// The number of the line in the game's output, from 0, without gaps; lines left out of
    /// the event stream keep their numbers, so a gap in a stream marks skipped lines.
    #[serde(default)]
    pub seq: u64,
    /// The `seq` of the first line of the record this line belongs to; equal to `seq` for the
    /// first line.
    #[serde(default)]
    pub record: u64,
    /// Whether the line came from the error stream.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stderr: bool,
    /// The thread named in the header (`Render thread`), on a record's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// The logger named in the header (`net.minecraft.client.Minecraft`, `FML`), on a record's
    /// first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    /// The time as the game wrote it in a text header (`12:34:56`, `2013-01-01 12:00:00`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<String>,
    /// The time of a log4j XML event, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_time_ms: Option<u64>,
}

impl LogEntry {
    /// A line of a record of its own, with no header details.
    pub fn new(time_ms: u64, level: LogLevel, text: impl Into<String>) -> Self {
        Self {
            time_ms,
            level,
            text: text.into(),
            seq: 0,
            record: 0,
            stderr: false,
            thread: None,
            logger: None,
            stamp: None,
            game_time_ms: None,
        }
    }

    /// Whether the line continues the record of an earlier line (a message or stack trace line).
    pub fn is_continuation(&self) -> bool {
        self.record != self.seq
    }
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
