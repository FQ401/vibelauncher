//! The console of simulated games: a deterministic synthetic log stream, the bounded buffer and
//! the console commands.
//!
//! A simulated game writes a greeting, `MockOptions::game_log_lines` paced lines (every seventh a
//! warning), an error record with a stack trace, and a burst of [`SPAM_LINES`] lines at once. The
//! lines go into the instance's [`ConsoleBuffer`] and out as `Event::LogBatch`; like the real
//! backend, a batch carries at most [`BATCH_LINES`] lines and counts the rest as skipped.

use std::path::PathBuf;
use std::sync::Arc;

use vl_core::console::{ConsoleBuffer, ConsoleCommand, ConsoleStatus, DEFAULT_MAX_BYTES};
use vl_core::log::{LogBatch, LogEntry, LogLevel, LogSource};
use vl_core::{Error, Event, InstanceId, Response, Result};

use crate::Inner;

/// The lines of the burst a simulated game writes at once.
pub const SPAM_LINES: usize = 2_000;
/// The most lines of one batch event.
pub const BATCH_LINES: usize = 500;
/// The time the synthetic lines carry, milliseconds since the Unix epoch.
const TIME_MS: u64 = 1_800_000_000_000;

/// The console of an instance's current or last simulated run.
pub(crate) struct Console {
    buffer: ConsoleBuffer,
}

/// One synthetic line and whether it continues the record before it.
type Line = (LogEntry, bool);

fn header(level: LogLevel, thread: &str, logger: Option<&str>, stamp: &str, text: String) -> Line {
    let mut entry = LogEntry::new(TIME_MS, level, text);
    entry.thread = Some(thread.to_owned());
    entry.logger = logger.map(str::to_owned);
    entry.stamp = Some(stamp.to_owned());
    (entry, false)
}

fn continuation(level: LogLevel, text: &str) -> Line {
    (LogEntry::new(TIME_MS, level, text), true)
}

/// The greeting.
pub(crate) fn greeting(user: &str) -> Vec<Line> {
    vec![header(
        LogLevel::Info,
        "Render thread",
        None,
        "12:00:00",
        format!("[12:00:00] [Render thread/INFO]: Setting user: {user}"),
    )]
}

/// Paced line `n` (from 1).
pub(crate) fn paced(n: usize) -> Vec<Line> {
    let (level, text) = crate::data::game_log_line(n);
    let stamp = format!("12:00:{:02}", n % 60);
    vec![header(level, "Render thread", None, &stamp, text)]
}

/// An error record with a stack trace.
pub(crate) fn stack_trace() -> Vec<Line> {
    let error = LogLevel::Error;
    vec![
        header(
            error,
            "Render thread",
            Some("net.example.mockmod.MockMod"),
            "12:01:00",
            "[12:01:00] [Render thread/ERROR] [net.example.mockmod.MockMod]: Exception ticking \
             a mock entity"
                .to_owned(),
        ),
        continuation(
            error,
            "java.lang.IllegalStateException: Mock entity has no world",
        ),
        continuation(
            error,
            "\tat net.example.mockmod.MockEntity.tick(MockEntity.java:42)",
        ),
        continuation(
            error,
            "\tat net.minecraft.world.level.Level.tickEntities(Level.java:512)",
        ),
        continuation(error, "Caused by: java.lang.NullPointerException: world"),
        continuation(error, "\t... 12 more"),
    ]
}

/// The burst: [`SPAM_LINES`] lines, mostly debug output with a warning every tenth line.
pub(crate) fn spam() -> Vec<Line> {
    (0..SPAM_LINES)
        .map(|n| {
            let (level, name) = if n % 10 == 9 {
                (LogLevel::Warn, "WARN")
            } else {
                (LogLevel::Debug, "DEBUG")
            };
            header(
                level,
                "Server thread",
                Some("net.example.mockmod.Chunks"),
                "12:01:01",
                format!(
                    "[12:01:01] [Server thread/{name}] [net.example.mockmod.Chunks]: Mock chunk \
                     {n} loaded"
                ),
            )
        })
        .collect()
}

/// Starts a fresh console for a run of `instance`, replacing the earlier run's.
pub(crate) fn start(inner: &Inner, instance: &InstanceId) {
    let mut state = inner.lock();
    let max_lines = state.settings.console.max_lines as usize;
    state.consoles.insert(
        instance.clone(),
        Console {
            buffer: ConsoleBuffer::new(max_lines, DEFAULT_MAX_BYTES),
        },
    );
}

/// Adds `lines` to the instance's console and publishes them as one batch.
pub(crate) fn emit(inner: &Arc<Inner>, instance: &InstanceId, lines: Vec<Line>) {
    let mut delivered = Vec::new();
    let mut skipped = 0u64;
    {
        let mut state = inner.lock();
        let Some(console) = state.consoles.get_mut(instance) else {
            return;
        };
        for (entry, continues) in lines {
            let stored = console.buffer.push(entry, continues);
            if delivered.len() < BATCH_LINES {
                delivered.push(stored.clone());
            } else {
                skipped += 1;
            }
        }
        console.buffer.add_skipped(skipped);
    }
    if delivered.is_empty() && skipped == 0 {
        return;
    }
    inner.publish(Event::LogBatch(LogBatch {
        source: LogSource::Game(instance.clone()),
        lines: delivered,
        skipped,
    }));
}

/// The synthetic path of the full log file of a run.
fn log_file(instance: &InstanceId) -> PathBuf {
    PathBuf::from(format!("mock/instances/{instance}/logs/vl-game-mock.log"))
}

pub(crate) fn handle(inner: &Inner, command: ConsoleCommand) -> Result<Response> {
    let state = inner.lock();
    let console = |instance: &InstanceId| {
        state
            .consoles
            .get(instance)
            .ok_or_else(|| Error::not_found(format!("instance {instance} has no game output")))
    };
    match command {
        ConsoleCommand::Status { instance } => Ok(Response::ConsoleStatus(ConsoleStatus {
            running: state.games.contains_key(&instance),
            stats: console(&instance)?.buffer.stats(),
            log_file: Some(log_file(&instance)),
        })),
        ConsoleCommand::Fetch {
            instance,
            from,
            limit,
        } => Ok(Response::ConsolePage(
            console(&instance)?.buffer.page(from, limit),
        )),
        ConsoleCommand::Search { instance, query } => Ok(Response::ConsoleMatches(
            console(&instance)?.buffer.search(&query),
        )),
        // Nothing is opened: the mock touches no files.
        ConsoleCommand::OpenLogFile { instance } => console(&instance).map(|_| Response::Done),
        other => Err(Error::unsupported(format!(
            "vl-mock does not handle console {other:?}"
        ))),
    }
}
