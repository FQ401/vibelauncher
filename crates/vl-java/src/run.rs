//! Running an external program with a timeout and an output cap.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Why a program run produced no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The program could not be started.
    Spawn(String),
    /// The program did not finish in time and was killed.
    Timeout,
}

/// What a finished program printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    /// Whether the exit status was zero.
    pub success: bool,
    /// Standard output followed by standard error, each cut at the cap, lossily decoded.
    pub text: String,
}

/// Runs `program` with `args` and no input. At most `cap` bytes of each stream are kept; the
/// rest is read and dropped so a chatty program never blocks on a full pipe. The program is
/// killed after `timeout`.
pub(crate) fn run_capped(
    program: &Path,
    args: &[&str],
    clear_env: &[&str],
    timeout: Duration,
    cap: usize,
) -> Result<Output, RunError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in clear_env {
        command.env_remove(name);
    }
    hide_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| RunError::Spawn(e.to_string()))?;

    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, false, cap, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, true, cap, tx.clone());
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Spawn(e.to_string()));
            }
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Timeout);
    };

    // Readers end when the pipes close; a grandchild holding a pipe must not hang us.
    let mut out = Vec::new();
    let mut err = Vec::new();
    let grace = Instant::now() + Duration::from_millis(500);
    for _ in 0..2 {
        let left = grace.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((false, bytes)) => out = bytes,
            Ok((true, bytes)) => err = bytes,
            Err(_) => break,
        }
    }
    out.push(b'\n');
    out.extend_from_slice(&err);
    Ok(Output {
        success: status.success(),
        text: String::from_utf8_lossy(&out).into_owned(),
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut source: R,
    is_err: bool,
    cap: usize,
    tx: mpsc::Sender<(bool, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        let _ = tx.send((is_err, kept));
    });
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}
