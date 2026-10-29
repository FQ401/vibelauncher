//! The game process: start, stream the output, stop, report the exit.
//!
//! The output of both streams is read by one thread each, which only hands lines over: a game
//! never waits for the launcher's front ends. One collector thread parses the lines
//! ([`gamelog`](crate::gamelog)), writes the full log to a file ([`logfile`](crate::logfile)),
//! keeps the newest lines in a bounded console buffer ([`ConsoleBuffer`]), and delivers the lines
//! to the caller in batches at a fixed pace. When the game writes faster than a front end can
//! show, the surplus is counted instead of delivered; it is in the file and in the buffer.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vl_core::console::{
    ConsoleBuffer, ConsoleMatches, ConsolePage, ConsoleQuery, ConsoleStats, DEFAULT_MAX_BYTES,
};
use vl_core::launch::GameExit;
use vl_core::log::LogEntry;
use vl_core::{Error, ErrorKind, Result};

use crate::command::CommandLine;
use crate::crash::Baseline;
use crate::gamelog::{LineParser, Parsed};
use crate::logfile::{DEFAULT_KEEP, GameLogFile};

/// The longest line delivered at once; a longer one arrives in pieces.
const MAX_LINE: u64 = 64 * 1024;
/// How long the output readers get to finish after the process exited.
const DRAIN: Duration = Duration::from_secs(2);
/// How often the exit of the process is checked.
const POLL: Duration = Duration::from_millis(50);
/// Lines waiting between the readers and the collector; a full queue slows the readers.
const QUEUE: usize = 4096;
/// How many of the last lines the exit report carries.
pub const TAIL_LINES: usize = 200;

/// How the output of a game is handled.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// The directory for the full log file of this run; `None` writes no file.
    pub log_dir: Option<PathBuf>,
    /// How many launch logs to keep in `log_dir`, the new one included.
    pub keep_logs: usize,
    /// How many of the last lines to keep in memory (the console history limit).
    pub history_lines: usize,
    /// The most memory the kept lines may use, whatever the line limit says.
    pub history_bytes: usize,
    /// How often collected lines are delivered.
    pub batch_interval: Duration,
    /// The most lines delivered per interval; more are counted as skipped.
    pub max_batch_lines: usize,
}

impl SpawnOptions {
    /// Defaults: no file, a buffer of at most [`DEFAULT_MAX_BYTES`], a batch every 100 ms of at
    /// most 500 lines (at most ten batches a second).
    pub fn new(history_lines: usize) -> Self {
        Self {
            log_dir: None,
            keep_logs: DEFAULT_KEEP,
            history_lines,
            history_bytes: DEFAULT_MAX_BYTES,
            batch_interval: Duration::from_millis(100),
            max_batch_lines: 500,
        }
    }
}

/// Output lines collected over one interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameBatch {
    /// The lines in output order.
    pub lines: Vec<LogEntry>,
    /// Lines of the interval that were not delivered; they are in the file and the history.
    pub skipped: u64,
}

struct Shared {
    child: Mutex<Child>,
    stopped: AtomicBool,
    console: Mutex<ConsoleBuffer>,
    /// When the process was seen ended; the collector then gives the readers [`DRAIN`] to finish.
    ended: Mutex<Option<Instant>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A started game process.
#[derive(Clone)]
pub struct RunningGame {
    pid: u32,
    shared: Arc<Shared>,
    log_file: Option<PathBuf>,
}

impl RunningGame {
    /// The OS process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The file the full output of this run goes to, when there is one.
    pub fn log_file(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    /// The texts of the kept output lines, oldest first.
    pub fn history(&self) -> Vec<String> {
        locked(&self.shared.console).tail_texts(usize::MAX)
    }

    /// The counters of the console buffer.
    pub fn console_stats(&self) -> ConsoleStats {
        locked(&self.shared.console).stats()
    }

    /// Lines of the console buffer (see [`ConsoleBuffer::page`]).
    pub fn console_page(&self, from: Option<u64>, limit: u32) -> ConsolePage {
        locked(&self.shared.console).page(from, limit)
    }

    /// Searches the console buffer (see [`ConsoleBuffer::search`]).
    pub fn console_search(&self, query: &ConsoleQuery) -> ConsoleMatches {
        locked(&self.shared.console).search(query)
    }

    /// Whether the process has ended.
    pub fn has_exited(&self) -> bool {
        locked(&self.shared.ended).is_some()
    }

    /// Kills the process and everything it started. The exit is then reported with
    /// `stopped_by_user`.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let mut child = locked(&self.shared.child);
        // The descendants first: once the parent is gone the tree cannot be walked any more.
        kill_tree(self.pid);
        // The process may have ended already; that is the outcome wanted.
        let _ = child.kill();
    }
}

/// Ends the descendants of `pid` too: on Windows `java.exe` can be a launcher shim that starts
/// the real runtime as a child, and killing the shim alone leaves the game running.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// The game leads its own process group (see [`spawn`]); the whole group gets SIGKILL.
#[cfg(unix)]
fn kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(windows, unix)))]
fn kill_tree(_pid: u32) {}

/// Starts `command` without a shell, in its working directory, with no input.
///
/// `on_batch` receives the parsed output of both streams in batches, from the collector thread.
/// `on_exit` runs once, on another thread, after the process ended and its output was delivered.
/// The exit is a crash when the process ended with a non-zero code or left a new crash report or
/// JVM error log, unless the user stopped it.
///
/// # Errors
///
/// `Game` when the process cannot be started; the message names the program and the OS error,
/// never the arguments.
pub fn spawn(
    command: &CommandLine,
    options: &SpawnOptions,
    on_batch: impl FnMut(GameBatch) + Send + 'static,
    on_exit: impl FnOnce(GameExit) + Send + 'static,
) -> Result<RunningGame> {
    let baseline = Baseline::capture(&command.cwd);
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .envs(&command.env)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A group of its own: a stop reaches the descendants too.
        process.process_group(0);
    }
    let mut child = process.spawn().map_err(|e| {
        Error::new(
            ErrorKind::Game,
            format!("cannot start {}: {e}", command.program.display()),
        )
    })?;
    let started = Instant::now();
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let shared = Arc::new(Shared {
        child: Mutex::new(child),
        stopped: AtomicBool::new(false),
        console: Mutex::new(ConsoleBuffer::new(
            options.history_lines,
            options.history_bytes,
        )),
        ended: Mutex::new(None),
    });
    let file = options
        .log_dir
        .as_ref()
        .and_then(|dir| GameLogFile::create(dir, options.keep_logs).ok());
    let log_file = file.as_ref().map(|f| f.path().to_path_buf());

    let secrets: Arc<Vec<String>> = Arc::new(command.secrets().to_vec());
    let (tx, rx) = mpsc::sync_channel::<RawLine>(QUEUE);
    if let Some(out) = stdout {
        read_lines(out, false, &secrets, &tx);
    }
    if let Some(err) = stderr {
        read_lines(err, true, &secrets, &tx);
    }
    drop(tx);

    let collector = Collector {
        rx,
        parsers: [LineParser::new(), LineParser::new()],
        file,
        shared: Arc::clone(&shared),
        interval: options.batch_interval,
        max_batch: options.max_batch_lines.max(1),
        on_batch,
    };
    let collecting = thread::Builder::new()
        .name("vl-game-log".to_owned())
        .spawn(move || collector.run());
    let collecting = match collecting {
        Ok(handle) => handle,
        Err(e) => {
            abort(&shared, pid);
            return Err(Error::new(
                ErrorKind::Game,
                format!("cannot watch the game output: {e}"),
            ));
        }
    };

    let waiter = Arc::clone(&shared);
    let game_dir = command.cwd.clone();
    let exit_log = log_file.clone();
    let spawned = thread::Builder::new()
        .name("vl-game-wait".to_owned())
        .spawn(move || {
            let status = loop {
                let polled = locked(&waiter.child).try_wait();
                match polled {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => thread::sleep(POLL),
                    Err(_) => break None,
                }
            };
            *locked(&waiter.ended) = Some(Instant::now());
            // The collector ends once the readers did, or after the drain time.
            let _ = collecting.join();
            let stopped_by_user = waiter.stopped.load(Ordering::SeqCst);
            let code = status.and_then(|s| s.code());
            let found = baseline.detect(&game_dir, pid);
            let tail = locked(&waiter.console).tail_texts(TAIL_LINES);
            let crashed = !stopped_by_user
                && (code != Some(0) || found.crash_report.is_some() || found.hs_err.is_some());
            on_exit(GameExit {
                code,
                crashed,
                stopped_by_user,
                session_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                crash_report: found.crash_report,
                hs_err: found.hs_err,
                log_file: exit_log,
                tail,
            });
        });
    if let Err(e) = spawned {
        abort(&shared, pid);
        return Err(Error::new(
            ErrorKind::Game,
            format!("cannot watch the game process: {e}"),
        ));
    }
    Ok(RunningGame {
        pid,
        shared,
        log_file,
    })
}

/// Ends a process that cannot be watched.
fn abort(shared: &Shared, pid: u32) {
    kill_tree(pid);
    let _ = locked(&shared.child).kill();
}

/// One line as read from a stream.
struct RawLine {
    stderr: bool,
    text: String,
}

/// Starts a thread that reads `stream` line by line into `queue`. If the thread cannot start the
/// stream is dropped, which only loses that stream's output.
fn read_lines(
    stream: impl Read + Send + 'static,
    stderr: bool,
    secrets: &Arc<Vec<String>>,
    queue: &SyncSender<RawLine>,
) {
    let secrets = Arc::clone(secrets);
    let queue = queue.clone();
    let _ = thread::Builder::new()
        .name("vl-game-out".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                let mut text = String::from_utf8_lossy(&buf).into_owned();
                for secret in secrets.iter().filter(|s| !s.is_empty()) {
                    text = text.replace(secret.as_str(), "***");
                }
                if queue.send(RawLine { stderr, text }).is_err() {
                    break;
                }
            }
        });
}

/// Parses, stores and batches the lines of both streams.
struct Collector<F> {
    rx: Receiver<RawLine>,
    /// Stdout and stderr.
    parsers: [LineParser; 2],
    file: Option<GameLogFile>,
    shared: Arc<Shared>,
    interval: Duration,
    max_batch: usize,
    on_batch: F,
}

#[derive(Default)]
struct Pending {
    lines: Vec<LogEntry>,
    skipped: u64,
}

impl<F: FnMut(GameBatch)> Collector<F> {
    fn run(mut self) {
        let mut pending = Pending::default();
        let mut parsed = Vec::new();
        let mut deadline = Instant::now() + self.interval;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now()).min(POLL);
            match self.rx.recv_timeout(wait) {
                Ok(raw) => self.accept(&raw, &mut parsed, &mut pending),
                Err(RecvTimeoutError::Timeout) => {
                    let ended = *locked(&self.shared.ended);
                    if ended.is_some_and(|at| at.elapsed() > DRAIN) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if Instant::now() >= deadline {
                self.deliver(&mut pending);
                deadline = Instant::now() + self.interval;
            }
        }
        while let Ok(raw) = self.rx.try_recv() {
            self.accept(&raw, &mut parsed, &mut pending);
        }
        for stderr in [false, true] {
            self.parsers[usize::from(stderr)].flush(&mut parsed);
            self.store(&mut parsed, stderr, &mut pending);
        }
        self.deliver(&mut pending);
    }

    fn accept(&mut self, raw: &RawLine, parsed: &mut Vec<Parsed>, pending: &mut Pending) {
        self.parsers[usize::from(raw.stderr)].push(&raw.text, parsed);
        self.store(parsed, raw.stderr, pending);
    }

    /// Writes `parsed` (lines of one stream) to the file and the console buffer and queues it
    /// for delivery.
    fn store(&mut self, parsed: &mut Vec<Parsed>, stderr: bool, pending: &mut Pending) {
        if parsed.is_empty() {
            return;
        }
        let time_ms = now_ms();
        if let Some(file) = &mut self.file {
            for line in parsed.iter() {
                file.write_line(&line.text);
            }
        }
        let mut console = locked(&self.shared.console);
        let mut skipped = 0;
        for line in parsed.drain(..) {
            let entry = LogEntry {
                stderr,
                thread: line.thread,
                logger: line.logger,
                stamp: line.stamp,
                game_time_ms: line.game_time_ms,
                ..LogEntry::new(time_ms, line.level, line.text)
            };
            let stored = console.push(entry, line.continues);
            if pending.lines.len() < self.max_batch {
                pending.lines.push(stored.clone());
            } else {
                skipped += 1;
            }
        }
        console.add_skipped(skipped);
        pending.skipped += skipped;
    }

    fn deliver(&mut self, pending: &mut Pending) {
        if let Some(file) = &mut self.file {
            file.flush();
        }
        if pending.lines.is_empty() && pending.skipped == 0 {
            return;
        }
        let batch = GameBatch {
            lines: std::mem::take(&mut pending.lines),
            skipped: std::mem::take(&mut pending.skipped),
        };
        (self.on_batch)(batch);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn shell(script_unix: &str, script_windows: &str, secrets: Vec<String>) -> CommandLine {
        #[cfg(windows)]
        let (program, flag, script) = ("cmd", "/C", script_windows);
        #[cfg(not(windows))]
        let (program, flag, script) = ("sh", "-c", script_unix);
        let _ = (script_unix, script_windows);
        CommandLine::new(
            PathBuf::from(program),
            vec![flag.to_owned(), script.to_owned()],
            std::env::temp_dir(),
            secrets,
        )
    }

    fn run(command: &CommandLine) -> (Vec<LogEntry>, GameExit) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let (tx, rx) = mpsc::channel();
        spawn(
            command,
            &SpawnOptions::new(1000),
            move |b| sink.lock().unwrap().extend(b.lines),
            move |e| {
                let _ = tx.send(e);
            },
        )
        .expect("spawn");
        let exit = rx.recv_timeout(Duration::from_secs(20)).expect("exit");
        let lines = lines.lock().unwrap().clone();
        (lines, exit)
    }

    #[test]
    fn streams_output_hides_secrets_and_reports_the_exit_code() {
        let command = shell(
            "echo hello; echo token-1234 >&2; exit 3",
            "echo hello& echo token-1234 1>&2& exit 3",
            vec!["token-1234".to_owned()],
        );
        let (lines, exit) = run(&command);
        assert!(lines.iter().any(|l| l.text.trim() == "hello"));
        assert!(lines.iter().any(|l| l.text.trim() == "***"));
        assert!(!lines.iter().any(|l| l.text.contains("token-1234")));
        assert!(!exit.tail.iter().any(|l| l.contains("token-1234")));
        assert_eq!(exit.code, Some(3));
        assert!(exit.crashed);
        assert!(!exit.stopped_by_user);
    }

    #[test]
    fn a_clean_exit_is_not_a_crash() {
        let (_, exit) = run(&shell("exit 0", "exit 0", Vec::new()));
        assert_eq!(exit.code, Some(0));
        assert!(!exit.crashed);
        assert!(exit.crash_report.is_none() && exit.hs_err.is_none());
    }

    #[test]
    fn stop_kills_the_process_and_marks_the_exit() {
        let command = shell("sleep 30", "ping -n 30 127.0.0.1 >nul", Vec::new());
        let (tx, rx) = mpsc::channel();
        let game = spawn(
            &command,
            &SpawnOptions::new(10),
            |_| {},
            move |e| {
                let _ = tx.send(e);
            },
        )
        .expect("spawn");
        assert!(game.pid() > 0);
        thread::sleep(Duration::from_millis(200));
        game.stop();
        let exit = rx.recv_timeout(Duration::from_secs(20)).expect("exit");
        assert!(exit.stopped_by_user);
        assert!(!exit.crashed);
    }

    #[test]
    fn a_missing_program_is_a_game_error() {
        let command = CommandLine::new(
            PathBuf::from("definitely-not-a-program-vl"),
            vec!["--secret=abc".to_owned()],
            std::env::temp_dir(),
            Vec::new(),
        );
        let err = spawn(&command, &SpawnOptions::new(10), |_| {}, |_| {})
            .map(|_| ())
            .expect_err("fails");
        assert_eq!(err.kind, ErrorKind::Game);
        assert!(!err.message.contains("--secret"));
    }
}
