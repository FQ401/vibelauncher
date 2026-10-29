//! The game console: the bounded in-memory buffer of a game's output and the commands that read
//! it.
//!
//! While a game runs, its output arrives as `Event::LogBatch` (a few batches a second, each
//! bounded). A console that opens late, scrolls back, fills a gap of skipped lines or searches
//! reads the buffer with [`ConsoleCommand`]s. The buffer keeps the newest lines within a line
//! limit and a byte limit; older lines leave it (they stay in the run's log file) and are
//! counted. The buffer of the last run of an instance stays readable after the game exits, until
//! the next launch of that instance.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::InstanceId;
use crate::log::{LogEntry, LogLevel};

/// The most memory a console buffer uses by default, whatever its line limit.
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// The most lines one [`ConsoleCommand::Fetch`] or [`ConsoleCommand::Search`] answers.
pub const MAX_PAGE_LINES: u32 = 5_000;

/// Console commands. Each names the instance whose current or last game run it reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsoleCommand {
    /// The state of the buffer → `Response::ConsoleStatus`.
    Status {
        /// The instance.
        instance: InstanceId,
    },
    /// Lines of the buffer → `Response::ConsolePage`.
    Fetch {
        /// The instance.
        instance: InstanceId,
        /// The `seq` of the first line wanted; `None` answers the newest `limit` lines. A line
        /// that left the buffer answers from the oldest line kept.
        from: Option<u64>,
        /// The most lines answered (at most [`MAX_PAGE_LINES`]).
        limit: u32,
    },
    /// Lines of the buffer that match a query → `Response::ConsoleMatches`.
    Search {
        /// The instance.
        instance: InstanceId,
        /// What to look for.
        query: ConsoleQuery,
    },
    /// Opens the full log file of the run with the system's default application →
    /// `Response::Done`; `NotFound` when the run has no log file.
    OpenLogFile {
        /// The instance.
        instance: InstanceId,
    },
    /// Prepares a game log of the instance for upload to a paste service and describes what
    /// would be sent → `Response::LogUploadPreview`. Nothing leaves the machine.
    PreviewUpload {
        /// The instance.
        instance: InstanceId,
        /// The file to share (see [`LogUploadPreview::file`] for the default).
        file: Option<PathBuf>,
    },
    /// Uploads a game log of the instance to the paste service → `Response::LogUploaded`. The
    /// content is prepared as for [`PreviewUpload`](Self::PreviewUpload); a front end sends this
    /// only after the user confirmed that preview.
    Upload {
        /// The instance.
        instance: InstanceId,
        /// The file to share (see [`LogUploadPreview::file`] for the default).
        file: Option<PathBuf>,
    },
}

/// What a log upload sends: the file, its masked content within the service's limits.
///
/// Secrets (tokens, keys, authorization values) are masked, and so is the home folder when the
/// setting `advanced.log_mask_home` is on. A log larger than the service accepts keeps its
/// first lines and its newest lines; a marker line in the content says how many lines were left
/// out between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogUploadPreview {
    /// The log file. The default is the output log of the instance's newest run
    /// (`<instance>/logs/vl-game-*.log`), else the game's `logs/latest.log`; a relative path
    /// names a file in the instance's game folder.
    pub file: PathBuf,
    /// The service the content goes to (`mclo.gs`).
    pub service: String,
    /// The size of the file, bytes.
    pub file_bytes: u64,
    /// The lines of the file.
    pub file_lines: u64,
    /// The size of the content sent, bytes.
    pub bytes: u64,
    /// The lines of the content sent, the marker line included.
    pub lines: u64,
    /// The lines of the file left out to fit the service's limits; 0 when it fits.
    pub omitted_lines: u64,
    /// Whether the home folder was masked.
    pub home_masked: bool,
}

impl LogUploadPreview {
    /// Whether lines were left out to fit the service's limits.
    pub fn truncated(&self) -> bool {
        self.omitted_lines > 0
    }
}

/// What [`ConsoleCommand::Upload`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedLog {
    /// The page of the log on the service (`https://mclo.gs/<id>`).
    pub url: String,
    /// The raw content, when the service names it.
    pub raw_url: Option<String>,
    /// What was sent.
    pub preview: LogUploadPreview,
}

/// A search over the console buffer: lines whose text contains `text` (ignoring case) and whose
/// level is one of `levels`. An empty `text` matches every line, so a query of levels alone is a
/// level filter. Continuation lines carry the level of their record, so a level filter keeps
/// stack traces with their record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsoleQuery {
    /// The text to find, ignoring case; empty matches every line.
    pub text: String,
    /// The levels wanted; empty means every level.
    pub levels: Vec<LogLevel>,
    /// The `seq` the answered matches start at (in the direction of the search); `None` starts
    /// at the oldest line, or the newest with `backwards`.
    pub from: Option<u64>,
    /// Searches from newer lines to older ones.
    pub backwards: bool,
    /// The most matching lines answered (at most [`MAX_PAGE_LINES`]); 0 answers only the count.
    pub limit: u32,
}

/// Counters and limits of a console buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleStats {
    /// The `seq` of the oldest line kept (equal to `next_seq` when the buffer is empty).
    pub first_seq: u64,
    /// The `seq` the next line will get: the number of lines the game has written so far.
    pub next_seq: u64,
    /// Lines kept.
    pub lines: u64,
    /// Memory used by the kept lines, approximately, in bytes.
    pub bytes: u64,
    /// Lines that left the buffer to stay within its limits (they are in the log file).
    pub dropped: u64,
    /// Lines that were left out of the event stream because the game wrote faster than the
    /// stream's pace allows (they are in the buffer, while it keeps them, and in the log file).
    pub skipped: u64,
    /// The line limit.
    pub max_lines: u64,
    /// The byte limit.
    pub max_bytes: u64,
}

/// What [`ConsoleCommand::Status`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleStatus {
    /// Whether the game of this buffer is still running.
    pub running: bool,
    /// The buffer's counters.
    pub stats: ConsoleStats,
    /// The file with the full output of the run, when there is one.
    pub log_file: Option<PathBuf>,
}

/// What [`ConsoleCommand::Fetch`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsolePage {
    /// The lines, oldest first, with consecutive `seq`s.
    pub lines: Vec<LogEntry>,
    /// The buffer's counters at the time of the answer.
    pub stats: ConsoleStats,
}

/// What [`ConsoleCommand::Search`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleMatches {
    /// Matching lines in the order of the search (oldest first, or newest first when
    /// backwards), at most the query's limit.
    pub lines: Vec<LogEntry>,
    /// How many lines of the whole buffer match.
    pub total: u64,
    /// Where to continue for more matches (the next `from`), when there are more.
    pub next: Option<u64>,
    /// The buffer's counters at the time of the answer.
    pub stats: ConsoleStats,
}

/// The newest lines of a game's output, bounded by a line limit and a byte limit. It numbers the
/// lines it receives and links continuation lines to their record.
#[derive(Debug, Clone)]
pub struct ConsoleBuffer {
    lines: VecDeque<LogEntry>,
    bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    next_seq: u64,
    dropped: u64,
    skipped: u64,
    /// The record each stream's last line belongs to (stdout, stderr).
    open_record: [Option<u64>; 2],
}

impl ConsoleBuffer {
    /// An empty buffer; each limit is at least one line and at least one line's worth.
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            max_lines: max_lines.max(1),
            max_bytes: max_bytes.max(1),
            next_seq: 0,
            dropped: 0,
            skipped: 0,
            open_record: [None, None],
        }
    }

    /// Adds a line. Its `seq` is assigned here; `continues` links it to the record of the last
    /// line of the same stream (a line with no earlier line on its stream starts a record).
    /// The oldest lines leave until both limits hold again (the new line always stays). Returns
    /// the stored line.
    pub fn push(&mut self, mut entry: LogEntry, continues: bool) -> &LogEntry {
        let seq = self.next_seq;
        self.next_seq += 1;
        let stream = usize::from(entry.stderr);
        entry.seq = seq;
        entry.record = match self.open_record[stream] {
            Some(record) if continues => record,
            _ => seq,
        };
        self.open_record[stream] = Some(entry.record);
        self.bytes += entry_bytes(&entry);
        self.lines.push_back(entry);
        while self.lines.len() > self.max_lines
            || (self.bytes > self.max_bytes && self.lines.len() > 1)
        {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= entry_bytes(&old);
                self.dropped += 1;
            }
        }
        // Not empty: the new line was just added and is never the one removed.
        &self.lines[self.lines.len() - 1]
    }

    /// Counts lines left out of the event stream.
    pub fn add_skipped(&mut self, lines: u64) {
        self.skipped += lines;
    }

    /// The counters and limits.
    pub fn stats(&self) -> ConsoleStats {
        ConsoleStats {
            first_seq: self.first_seq(),
            next_seq: self.next_seq,
            lines: self.lines.len() as u64,
            bytes: self.bytes as u64,
            dropped: self.dropped,
            skipped: self.skipped,
            max_lines: self.max_lines as u64,
            max_bytes: self.max_bytes as u64,
        }
    }

    /// The number of lines kept.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether no line is kept.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn first_seq(&self) -> u64 {
        self.lines.front().map_or(self.next_seq, |l| l.seq)
    }

    /// The kept lines, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &LogEntry> + ExactSizeIterator {
        self.lines.iter()
    }

    /// The last `n` lines' texts, oldest first.
    pub fn tail_texts(&self, n: usize) -> Vec<String> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines
            .iter()
            .skip(skip)
            .map(|l| l.text.clone())
            .collect()
    }

    /// The index of `seq` in `lines`, clamped to the kept range.
    fn index_of(&self, seq: u64) -> usize {
        let offset = seq.saturating_sub(self.first_seq());
        usize::try_from(offset).map_or(self.lines.len(), |i| i.min(self.lines.len()))
    }

    /// Up to `limit` lines from `from` (see [`ConsoleCommand::Fetch`]).
    pub fn page(&self, from: Option<u64>, limit: u32) -> ConsolePage {
        let limit = limit.min(MAX_PAGE_LINES) as usize;
        let start = match from {
            Some(seq) => self.index_of(seq),
            None => self.lines.len().saturating_sub(limit),
        };
        ConsolePage {
            lines: self.lines.range(start..).take(limit).cloned().collect(),
            stats: self.stats(),
        }
    }

    /// The lines that match `query` (see [`ConsoleQuery`]).
    pub fn search(&self, query: &ConsoleQuery) -> ConsoleMatches {
        let matcher = Matcher::new(&query.text);
        let limit = query.limit.min(MAX_PAGE_LINES) as usize;
        let wanted = |l: &LogEntry| {
            (query.levels.is_empty() || query.levels.contains(&l.level)) && matcher.matches(&l.text)
        };
        let mut lines = Vec::new();
        let mut total = 0u64;
        let mut next = None;
        let mut take = |l: &LogEntry, in_range: bool| {
            if !wanted(l) {
                return;
            }
            total += 1;
            if !in_range {
                return;
            }
            if lines.len() < limit {
                lines.push(l.clone());
            } else if next.is_none() {
                next = Some(l.seq);
            }
        };
        if query.backwards {
            let from = query.from.unwrap_or(u64::MAX);
            for l in self.lines.iter().rev() {
                take(l, l.seq <= from);
            }
        } else {
            let from = query.from.unwrap_or(0);
            for l in &self.lines {
                take(l, l.seq >= from);
            }
        }
        ConsoleMatches {
            lines,
            total,
            next,
            stats: self.stats(),
        }
    }
}

/// The memory a kept line takes, approximately: the entry itself and its strings.
fn entry_bytes(entry: &LogEntry) -> usize {
    std::mem::size_of::<LogEntry>()
        + entry.text.len()
        + entry.thread.as_ref().map_or(0, String::len)
        + entry.logger.as_ref().map_or(0, String::len)
        + entry.stamp.as_ref().map_or(0, String::len)
}

/// A case-insensitive substring test that does not allocate per line for an ASCII needle.
struct Matcher {
    needle: String,
    ascii: bool,
}

impl Matcher {
    fn new(text: &str) -> Self {
        let ascii = text.is_ascii();
        Self {
            needle: if ascii {
                text.to_ascii_lowercase()
            } else {
                text.to_lowercase()
            },
            ascii,
        }
    }

    fn matches(&self, hay: &str) -> bool {
        if self.needle.is_empty() {
            return true;
        }
        if self.ascii {
            let needle = self.needle.as_bytes();
            let first = needle[0];
            let hay = hay.as_bytes();
            hay.len() >= needle.len()
                && hay
                    .windows(needle.len())
                    .any(|w| w[0].to_ascii_lowercase() == first && w.eq_ignore_ascii_case(needle))
        } else {
            hay.to_lowercase().contains(&self.needle)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, level: LogLevel) -> LogEntry {
        LogEntry::new(0, level, text)
    }

    fn filled(n: u64, max_lines: usize) -> ConsoleBuffer {
        let mut buffer = ConsoleBuffer::new(max_lines, usize::MAX);
        for i in 0..n {
            buffer.push(line(&format!("line {i}"), LogLevel::Info), false);
        }
        buffer
    }

    #[test]
    fn keeps_the_newest_lines_within_the_line_limit() {
        let buffer = filled(10, 3);
        let stats = buffer.stats();
        assert_eq!(stats.lines, 3);
        assert_eq!(stats.first_seq, 7);
        assert_eq!(stats.next_seq, 10);
        assert_eq!(stats.dropped, 7);
        assert_eq!(buffer.tail_texts(10), ["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn keeps_within_the_byte_limit_but_never_drops_the_newest_line() {
        let mut buffer = ConsoleBuffer::new(usize::MAX, 64 * 1024);
        let chunk = "x".repeat(1000);
        for _ in 0..1000 {
            buffer.push(line(&chunk, LogLevel::Info), false);
        }
        let stats = buffer.stats();
        assert!(stats.bytes <= 64 * 1024, "{stats:?}");
        assert!(stats.lines >= 50);
        assert_eq!(stats.lines + stats.dropped, 1000);

        let mut tiny = ConsoleBuffer::new(10, 1);
        tiny.push(line("big line", LogLevel::Info), false);
        assert_eq!(tiny.len(), 1);
    }

    #[test]
    fn links_continuations_per_stream() {
        let mut buffer = ConsoleBuffer::new(100, usize::MAX);
        buffer.push(line("[main/ERROR]: boom", LogLevel::Error), false);
        let mut err = line("stderr line", LogLevel::Unknown);
        err.stderr = true;
        buffer.push(err, false);
        let trace = buffer.push(line("\tat a.B.c(B.java:1)", LogLevel::Error), true);
        assert_eq!((trace.seq, trace.record), (2, 0));
        assert!(trace.is_continuation());
        // The first line of a stream cannot continue anything.
        let mut first = ConsoleBuffer::new(10, usize::MAX);
        let orphan = first.push(line("\tat x", LogLevel::Unknown), true);
        assert!(!orphan.is_continuation());
    }

    #[test]
    fn pages_from_a_seq_and_from_the_end() {
        let buffer = filled(100, 50);
        let tail = buffer.page(None, 5);
        let seqs: Vec<_> = tail.lines.iter().map(|l| l.seq).collect();
        assert_eq!(seqs, [95, 96, 97, 98, 99]);
        let from = buffer.page(Some(60), 3);
        assert_eq!(from.lines[0].seq, 60);
        assert_eq!(from.lines.len(), 3);
        // A line that left the buffer answers from the oldest one kept.
        assert_eq!(buffer.page(Some(0), 1).lines[0].seq, 50);
        assert!(buffer.page(Some(1000), 10).lines.is_empty());
        assert_eq!(
            buffer.page(None, u32::MAX).lines.len(),
            50.min(MAX_PAGE_LINES as usize)
        );
    }

    #[test]
    fn searches_ignoring_case_with_a_level_filter_and_paging() {
        let mut buffer = ConsoleBuffer::new(100, usize::MAX);
        buffer.push(line("Loading Mods", LogLevel::Info), false);
        buffer.push(line("mod failed: OptiFine", LogLevel::Error), false);
        buffer.push(line("\tat MOD.Thing", LogLevel::Error), true);
        buffer.push(line("Ünïcode MÖD", LogLevel::Warn), false);
        buffer.push(line("unrelated", LogLevel::Info), false);

        let all = buffer.search(&ConsoleQuery {
            text: "MOD".into(),
            limit: 10,
            ..ConsoleQuery::default()
        });
        assert_eq!(all.total, 3);
        let texts: Vec<_> = all.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Loading Mods", "mod failed: OptiFine", "\tat MOD.Thing"]
        );

        let unicode = buffer.search(&ConsoleQuery {
            text: "möd".into(),
            limit: 10,
            ..ConsoleQuery::default()
        });
        assert_eq!(unicode.total, 1);

        let errors = buffer.search(&ConsoleQuery {
            levels: vec![LogLevel::Error],
            limit: 10,
            ..ConsoleQuery::default()
        });
        assert_eq!(errors.total, 2);
        assert!(errors.lines.iter().all(|l| l.level == LogLevel::Error));

        let first = buffer.search(&ConsoleQuery {
            text: "mod".into(),
            limit: 1,
            ..ConsoleQuery::default()
        });
        assert_eq!(first.lines.len(), 1);
        assert_eq!(first.next, Some(1));
        let rest = buffer.search(&ConsoleQuery {
            text: "mod".into(),
            limit: 10,
            from: first.next,
            ..ConsoleQuery::default()
        });
        assert_eq!(rest.lines.len(), 2);
        assert_eq!(rest.next, None);
        assert_eq!(rest.total, 3);

        let back = buffer.search(&ConsoleQuery {
            text: "mod".into(),
            backwards: true,
            limit: 1,
            ..ConsoleQuery::default()
        });
        assert_eq!(back.lines[0].seq, 2);
        assert_eq!(back.next, Some(1));

        let count = buffer.search(&ConsoleQuery {
            text: "mod".into(),
            ..ConsoleQuery::default()
        });
        assert!(count.lines.is_empty());
        assert_eq!(count.total, 3);
    }

    #[test]
    fn search_text_never_matches_a_longer_needle() {
        let mut buffer = ConsoleBuffer::new(10, usize::MAX);
        buffer.push(line("ab", LogLevel::Info), false);
        let found = buffer.search(&ConsoleQuery {
            text: "abc".into(),
            limit: 1,
            ..ConsoleQuery::default()
        });
        assert_eq!(found.total, 0);
    }
}
