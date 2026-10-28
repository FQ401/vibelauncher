//! Running the processors of an installer profile.
//!
//! A processor is a Java program: `java -cp <jar and class path> <Main-Class of the jar> <args>`.
//! [`ProcessorRunner`] is the seam between planning and running; [`JavaRunner`] starts a real
//! Java without a shell (and without a console window on Windows), streams its output lines,
//! kills it on cancellation or after a timeout. Tests substitute their own runner.
//!
//! [`run_steps`] runs a plan: a step whose `outputs` all exist with the expected SHA-1 is
//! skipped, a step that leaves an output missing or with another hash is an `Integrity` error.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use sha1::{Digest, Sha1};
use vl_core::{Error, ErrorKind, Result};
use vl_meta::install_profile::{ProcessorStep, manifest_main_class};
use vl_net::CancelToken;
use zip::ZipArchive;

/// The largest jar manifest read.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The longest output line kept; longer lines are cut.
const MAX_LINE_CHARS: usize = 4096;

/// One processor run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The Java executable.
    pub java: PathBuf,
    /// The class path.
    pub classpath: Vec<PathBuf>,
    /// The main class.
    pub main_class: String,
    /// The arguments after the main class.
    pub args: Vec<String>,
    /// The working directory.
    pub work_dir: PathBuf,
}

/// Runs processors.
pub trait ProcessorRunner: Send + Sync {
    /// Runs `invocation` to its end, handing every output line to `line`. Stops early when
    /// `cancel` is set.
    ///
    /// # Errors
    ///
    /// `Java` when the program cannot start, exits with an error or times out; `Cancelled`.
    fn run(
        &self,
        invocation: &Invocation,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str),
    ) -> Result<()>;
}

/// Runs processors with a real Java.
#[derive(Debug, Clone)]
pub struct JavaRunner {
    /// How long one processor may run.
    pub timeout: Duration,
}

impl Default for JavaRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30 * 60),
        }
    }
}

impl ProcessorRunner for JavaRunner {
    fn run(
        &self,
        invocation: &Invocation,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let classpath = std::env::join_paths(&invocation.classpath).map_err(|e| {
            Error::new(ErrorKind::InvalidInput, format!("unusable class path: {e}"))
        })?;
        let mut command = Command::new(&invocation.java);
        command
            .arg("-cp")
            .arg(classpath)
            .arg(&invocation.main_class)
            .args(&invocation.args)
            .current_dir(&invocation.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_window(&mut command);
        let mut child = command.spawn().map_err(|e| {
            Error::new(
                ErrorKind::Java,
                format!("cannot start {}: {e}", invocation.java.display()),
            )
        })?;
        let (tx, rx) = mpsc::channel::<String>();
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, tx.clone());
        }
        drop(tx);
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            while let Ok(text) = rx.try_recv() {
                line(&text);
            }
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::cancelled());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::new(
                        ErrorKind::Java,
                        format!("{} did not finish in time", invocation.main_class),
                    ));
                }
                Ok(None) => {
                    if let Ok(text) = rx.recv_timeout(Duration::from_millis(50)) {
                        line(&text);
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::new(ErrorKind::Java, e.to_string()));
                }
            }
        };
        // The readers end when the pipes close; a grandchild holding one must not hang us.
        let grace = Instant::now() + Duration::from_millis(500);
        while let Ok(text) = rx.recv_timeout(grace.saturating_duration_since(Instant::now())) {
            line(&text);
        }
        if status.success() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Java,
                format!("{} exited with {status}", invocation.main_class),
            ))
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(source: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(source);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf);
                    let text: String = text.trim_end().chars().take(MAX_LINE_CHARS).collect();
                    if tx.send(text).is_err() {
                        break;
                    }
                }
            }
        }
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

/// The `Main-Class` of a jar.
///
/// # Errors
///
/// `NotFound` / `Io` when the jar cannot be read, `Integrity` when it is no zip or has no main
/// class.
pub fn main_class(jar: &Path) -> Result<String> {
    let file = fs::File::open(jar).map_err(|e| io_error(jar, &e))?;
    let mut zip = ZipArchive::new(file).map_err(|e| damaged_jar(jar, &e.to_string()))?;
    let mut entry = zip
        .by_name("META-INF/MANIFEST.MF")
        .map_err(|_| damaged_jar(jar, "no manifest"))?;
    let mut bytes = Vec::new();
    (&mut entry)
        .take(MAX_MANIFEST_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| damaged_jar(jar, &e.to_string()))?;
    manifest_main_class(&String::from_utf8_lossy(&bytes))
        .ok_or_else(|| damaged_jar(jar, "no Main-Class"))
}

fn damaged_jar(jar: &Path, what: &str) -> Error {
    Error::new(ErrorKind::Integrity, format!("{}: {what}", jar.display()))
}

pub(crate) fn io_error(path: &Path, e: &std::io::Error) -> Error {
    let kind = if e.kind() == std::io::ErrorKind::NotFound {
        ErrorKind::NotFound
    } else {
        ErrorKind::Io
    };
    Error::new(kind, format!("{}: {e}", path.display()))
}

/// The SHA-1 of a file, lowercase hex.
///
/// # Errors
///
/// `NotFound` / `Io` when it cannot be read.
pub fn sha1_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| io_error(path, &e))?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Whether every file exists with its hash. An empty list is not "all present": a step without
/// declared outputs always runs.
fn outputs_present(outputs: &[(String, String)]) -> bool {
    !outputs.is_empty()
        && outputs
            .iter()
            .all(|(file, sha1)| sha1_file(Path::new(file)).is_ok_and(|h| &h == sha1))
}

/// How [`run_steps`] reports.
pub struct StepHooks<'a> {
    /// Called before each step with its index (from 0) and the number of steps.
    pub on_step: &'a dyn Fn(usize, usize),
    /// Receives every line worth logging: the command of each step and its output.
    pub on_line: &'a mut dyn FnMut(&str),
}

/// What [`run_steps`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepsRun {
    /// Steps that ran.
    pub ran: usize,
    /// Steps skipped because their outputs were already there.
    pub skipped: usize,
}

/// Runs the steps of a plan in order with `java`.
///
/// # Errors
///
/// What the runner reports, `Integrity` for a jar without a main class or an output that is
/// missing or has another hash after its step, `Cancelled`.
pub fn run_steps(
    steps: &[ProcessorStep],
    java: &Path,
    work_dir: &Path,
    runner: &dyn ProcessorRunner,
    cancel: &CancelToken,
    hooks: &mut StepHooks<'_>,
) -> Result<StepsRun> {
    let mut done = StepsRun::default();
    for (i, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Error::cancelled());
        }
        (hooks.on_step)(i, steps.len());
        if outputs_present(&step.outputs) {
            (hooks.on_line)(&format!("step {}: outputs present, skipped", i + 1));
            done.skipped += 1;
            continue;
        }
        let jar = PathBuf::from(&step.jar);
        let invocation = Invocation {
            java: java.to_path_buf(),
            main_class: main_class(&jar)?,
            classpath: step.classpath.iter().map(PathBuf::from).collect(),
            args: step.args.clone(),
            work_dir: work_dir.to_path_buf(),
        };
        (hooks.on_line)(&format!(
            "step {}: {} {}",
            i + 1,
            invocation.main_class,
            invocation.args.join(" ")
        ));
        runner.run(&invocation, cancel, hooks.on_line)?;
        for (file, sha1) in &step.outputs {
            let actual = sha1_file(Path::new(file)).map_err(|e| {
                Error::new(
                    ErrorKind::Integrity,
                    format!(
                        "processor {} did not write {file}: {}",
                        invocation.main_class, e.message
                    ),
                )
            })?;
            if &actual != sha1 {
                let _ = fs::remove_file(file);
                return Err(Error::new(
                    ErrorKind::Integrity,
                    format!("{file}: sha1 {actual}, expected {sha1}"),
                ));
            }
        }
        done.ran += 1;
    }
    Ok(done)
}
