//! The game process: start, stream the output, stop, report the exit.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use vl_core::launch::GameExit;
use vl_core::{Error, ErrorKind, Result};

use crate::command::CommandLine;

/// The longest line delivered at once; a longer one arrives in pieces.
const MAX_LINE: u64 = 64 * 1024;
/// How long the output readers get to finish after the process exited.
const DRAIN: Duration = Duration::from_secs(2);
/// How often the exit of the process is checked.
const POLL: Duration = Duration::from_millis(50);

/// One line of the game's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLine {
    /// The text without the line ending, secrets hidden.
    pub text: String,
    /// Whether it came from the error stream.
    pub stderr: bool,
}

struct Shared {
    child: Mutex<Child>,
    stopped: AtomicBool,
}

/// A started game process.
#[derive(Clone)]
pub struct RunningGame {
    pid: u32,
    shared: Arc<Shared>,
}

impl RunningGame {
    /// The OS process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Kills the process. The exit is then reported with `stopped_by_user`.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let mut child = self
            .shared
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // The descendants first: once the parent is gone the tree cannot be walked any more.
        #[cfg(windows)]
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

/// Starts `command` without a shell, in its working directory, with no input.
///
/// `on_line` receives the output of both streams, line by line, from reader threads. `on_exit`
/// runs once, on another thread, after the process ended and its output was delivered. A
/// non-zero exit that the user did not ask for counts as a crash.
///
/// # Errors
///
/// `Game` when the process cannot be started; the message names the program and the OS error,
/// never the arguments.
pub fn spawn(
    command: &CommandLine,
    on_line: impl Fn(GameLine) + Send + Sync + 'static,
    on_exit: impl FnOnce(GameExit) + Send + 'static,
) -> Result<RunningGame> {
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
    let mut child = process.spawn().map_err(|e| {
        Error::new(
            ErrorKind::Game,
            format!("cannot start {}: {e}", command.program.display()),
        )
    })?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let shared = Arc::new(Shared {
        child: Mutex::new(child),
        stopped: AtomicBool::new(false),
    });

    let on_line = Arc::new(on_line);
    let secrets: Arc<Vec<String>> = Arc::new(command.secrets().to_vec());
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let mut readers = 0;
    if let Some(out) = stdout {
        readers += usize::from(read_lines(out, false, &on_line, &secrets, &done_tx));
    }
    if let Some(err) = stderr {
        readers += usize::from(read_lines(err, true, &on_line, &secrets, &done_tx));
    }
    drop(done_tx);

    let waiter = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("vl-game-wait".to_owned())
        .spawn(move || {
            let status = loop {
                let polled = {
                    let mut child = waiter.child.lock().unwrap_or_else(PoisonError::into_inner);
                    child.try_wait()
                };
                match polled {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => thread::sleep(POLL),
                    Err(_) => break None,
                }
            };
            for _ in 0..readers {
                if done_rx.recv_timeout(DRAIN).is_err() {
                    break;
                }
            }
            let stopped_by_user = waiter.stopped.load(Ordering::SeqCst);
            let code = status.and_then(|s| s.code());
            on_exit(GameExit {
                code,
                crashed: !stopped_by_user && code != Some(0),
                stopped_by_user,
            });
        });
    if let Err(e) = spawned {
        let mut child = shared.child.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = child.kill();
        return Err(Error::new(
            ErrorKind::Game,
            format!("cannot watch the game process: {e}"),
        ));
    }
    Ok(RunningGame { pid, shared })
}

/// Starts a thread that reads `stream` line by line; `false` when the thread cannot start.
fn read_lines(
    stream: impl Read + Send + 'static,
    stderr: bool,
    on_line: &Arc<impl Fn(GameLine) + Send + Sync + 'static>,
    secrets: &Arc<Vec<String>>,
    done: &mpsc::Sender<()>,
) -> bool {
    let on_line = Arc::clone(on_line);
    let secrets = Arc::clone(secrets);
    let done = done.clone();
    thread::Builder::new()
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
                on_line(GameLine { text, stderr });
            }
            let _ = done.send(());
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    fn run(command: &CommandLine) -> (Vec<GameLine>, GameExit) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let (tx, rx) = mpsc::channel();
        spawn(
            command,
            move |l| sink.lock().unwrap().push(l),
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
        assert!(lines.iter().any(|l| l.text.trim() == "hello" && !l.stderr));
        assert!(lines.iter().any(|l| l.text.trim() == "***" && l.stderr));
        assert!(!lines.iter().any(|l| l.text.contains("token-1234")));
        assert_eq!(exit.code, Some(3));
        assert!(exit.crashed);
        assert!(!exit.stopped_by_user);
    }

    #[test]
    fn a_clean_exit_is_not_a_crash() {
        let (_, exit) = run(&shell("exit 0", "exit 0", Vec::new()));
        assert_eq!(exit.code, Some(0));
        assert!(!exit.crashed);
    }

    #[test]
    fn stop_kills_the_process_and_marks_the_exit() {
        let command = shell("sleep 30", "ping -n 30 127.0.0.1 >nul", Vec::new());
        let (tx, rx) = mpsc::channel();
        let game = spawn(
            &command,
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
        let err = spawn(&command, |_| {}, |_| {})
            .map(|_| ())
            .expect_err("fails");
        assert_eq!(err.kind, ErrorKind::Game);
        assert!(!err.message.contains("--secret"));
    }
}
