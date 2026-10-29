//! The launcher log file and its rotation.
//!
//! Records go to `launcher.log` in the logs folder, one record a line:
//!
//! ```text
//! 2026-10-29T12:34:56.789Z INFO  launch: game exited instance=survival code=0 crashed=false
//! ```
//!
//! A message of several lines continues on lines indented by four spaces. A field value with
//! spaces, quotes or `=` is quoted and escaped. Each record is masked and then written with one
//! `write` call without a buffer, so a crash loses nothing that was logged before it.
//!
//! When a record would take the file past [`Rotation::max_file_bytes`], the files shift:
//! `launcher.log` becomes `launcher.1.log`, `launcher.1.log` becomes `launcher.2.log`, and so on;
//! the file beyond [`Rotation::max_files`] is deleted. A rename that fails (a file held open by
//! another program on Windows) keeps the current file and is tried again at the next record.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::SystemTime;

use vl_core::log::LogLevel;
use vl_core::redact::Redactor;

use crate::time::rfc3339;

/// The name of the current launcher log; older ones are `launcher.<n>.log`.
pub const FILE_NAME: &str = "launcher.log";
/// The size a launcher log file is rotated at by default.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// How many launcher log files are kept by default, the current one included.
pub const MAX_FILES: usize = 5;

const STEM: &str = "launcher";
const EXTENSION: &str = "log";

/// When the launcher log rotates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rotation {
    /// A file is rotated before a record would take it past this size (a single larger record
    /// still goes into a file of its own).
    pub max_file_bytes: u64,
    /// Files kept, the current one included (at least 1).
    pub max_files: usize,
}

impl Rotation {
    /// The default rotation, shrunk so that every file fits in a folder limited to `max_bytes`
    /// and `max_files` (files are never rotated below 64 KiB).
    pub fn within(max_bytes: u64, max_files: usize) -> Self {
        let files = MAX_FILES.min(max_files).max(1);
        Self {
            max_file_bytes: MAX_FILE_BYTES.min(max_bytes / files as u64).max(64 * 1024),
            max_files: files,
        }
    }
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            max_file_bytes: MAX_FILE_BYTES,
            max_files: MAX_FILES,
        }
    }
}

/// The launcher log of one data folder. Shared between threads; a write error never reaches the
/// caller (logging must not break the launcher), the file is opened again at the next record.
#[derive(Debug)]
pub struct LauncherLog {
    dir: PathBuf,
    rotation: Rotation,
    state: Mutex<State>,
    redactor: RwLock<Redactor>,
}

#[derive(Debug)]
struct State {
    file: Option<File>,
    size: u64,
}

impl LauncherLog {
    /// Opens (creates or appends to) `launcher.log` in `dir`, creating the folder.
    ///
    /// # Errors
    ///
    /// The I/O error when the folder or the file cannot be created.
    pub fn open(dir: &Path, rotation: Rotation, redactor: Redactor) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let log = Self {
            dir: dir.to_path_buf(),
            rotation: Rotation {
                max_files: rotation.max_files.max(1),
                ..rotation
            },
            state: Mutex::new(State {
                file: None,
                size: 0,
            }),
            redactor: RwLock::new(redactor),
        };
        let file = log.open_current()?;
        let size = file.metadata().map_or(0, |m| m.len());
        *log.lock() = State {
            file: Some(file),
            size,
        };
        Ok(log)
    }

    /// The current file.
    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// Replaces the masking of the records written from now on.
    pub fn set_redactor(&self, redactor: Redactor) {
        *self
            .redactor
            .write()
            .unwrap_or_else(PoisonError::into_inner) = redactor;
    }

    /// Writes one record now.
    pub fn write(&self, level: LogLevel, target: &str, message: &str, fields: &[(&str, &str)]) {
        self.write_at(SystemTime::now(), level, target, message, fields);
    }

    /// Writes one record with the time `time`.
    pub fn write_at(
        &self,
        time: SystemTime,
        level: LogLevel,
        target: &str,
        message: &str,
        fields: &[(&str, &str)],
    ) {
        let record = format_record(time, level, target, message, fields);
        let record = self
            .redactor
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .redact(&record)
            .into_owned();
        self.append(record.as_bytes());
    }

    fn append(&self, bytes: &[u8]) {
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut state = self.lock();
        if state.size > 0 && state.size.saturating_add(len) > self.rotation.max_file_bytes {
            // Windows renames no file that is open, so close it first.
            state.file = None;
            if self.rotate().is_ok() {
                state.size = 0;
            }
        }
        if state.file.is_none() {
            match self.open_current() {
                Ok(file) => {
                    state.size = file.metadata().map_or(0, |m| m.len());
                    state.file = Some(file);
                }
                Err(_) => return,
            }
        }
        if let Some(file) = &mut state.file {
            if file.write_all(bytes).is_ok() {
                state.size = state.size.saturating_add(len);
            } else {
                state.file = None;
            }
        }
    }

    fn open_current(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path())
    }

    /// Shifts the files by one; the oldest one beyond the limit is deleted.
    fn rotate(&self) -> io::Result<()> {
        let last = self.rotation.max_files - 1;
        if last == 0 {
            return remove_if_present(&self.path());
        }
        remove_if_present(&self.numbered(last))?;
        for n in (1..last).rev() {
            rename_if_present(&self.numbered(n), &self.numbered(n + 1))?;
        }
        rename_if_present(&self.path(), &self.numbered(1))
    }

    fn numbered(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{STEM}.{n}.{EXTENSION}"))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// One record with its trailing newline.
fn format_record(
    time: SystemTime,
    level: LogLevel,
    target: &str,
    message: &str,
    fields: &[(&str, &str)],
) -> String {
    let level = match level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info | LogLevel::Unknown => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    };
    let mut lines = message.lines();
    let mut out = format!(
        "{} {level:<5} {target}: {}",
        rfc3339(time),
        lines.next().unwrap_or("")
    );
    for (key, value) in fields {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        if value.is_empty()
            || value
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=')
        {
            out.push_str(&format!("{value:?}"));
        } else {
            out.push_str(value);
        }
    }
    out.push('\n');
    for line in lines {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Writes every panic of the process into `log` (an error record with the message, the place and
/// the thread), then runs the panic hook that was set before.
pub fn log_panics(log: Arc<LauncherLog>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_owned());
        let place = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current();
        log.write(
            LogLevel::Error,
            "panic",
            &message,
            &[
                ("at", &place),
                ("thread", thread.name().unwrap_or("unnamed")),
            ],
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-logs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn records_are_structured_lines() {
        let t = UNIX_EPOCH + Duration::from_millis(1_709_210_096_789);
        assert_eq!(
            format_record(t, LogLevel::Info, "launch", "game exited", &[("code", "0")]),
            "2024-02-29T12:34:56.789Z INFO  launch: game exited code=0\n"
        );
        assert_eq!(
            format_record(
                t,
                LogLevel::Error,
                "task",
                "failed\ncaused by: disk full",
                &[("name", "My pack"), ("empty", ""), ("q", "a=\"b\"")]
            ),
            "2024-02-29T12:34:56.789Z ERROR task: failed name=\"My pack\" empty=\"\" \
             q=\"a=\\\"b\\\"\"\n    caused by: disk full\n"
        );
    }

    #[test]
    fn records_are_masked_before_they_are_written() {
        let dir = temp("masked");
        let log = LauncherLog::open(
            &dir,
            Rotation::default(),
            Redactor::new().with_home("/home/synthetic", false),
        )
        .unwrap();
        log.write(
            LogLevel::Info,
            "accounts",
            "refreshed with refresh_token=M.C123_BAY.synthetic-value",
            &[("file", "/home/synthetic/data/accounts.json")],
        );
        log.write(
            LogLevel::Warn,
            "net",
            "Authorization: Bearer eyJhbGciOiJub25lIn0.eyJzdWIiOiJzeW50aGV0aWMifQ.",
            &[],
        );
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(!text.contains("synthetic-value"), "{text}");
        assert!(!text.contains("eyJ"), "{text}");
        assert!(!text.contains("/home/synthetic"), "{text}");
        assert!(text.contains("refresh_token=<redacted>"), "{text}");
        assert!(text.contains("file=~/data/accounts.json"), "{text}");
        assert!(text.contains("Bearer <redacted>"), "{text}");

        log.set_redactor(Redactor::new());
        log.write(LogLevel::Info, "x", "/home/synthetic/now", &[]);
        assert!(
            fs::read_to_string(log.path())
                .unwrap()
                .contains("/home/synthetic/now")
        );
        drop(log);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rotates_by_size_and_keeps_a_bounded_number_of_files() {
        let dir = temp("rotation");
        let rotation = Rotation {
            max_file_bytes: 64 * 1024,
            max_files: 4,
        };
        let log = LauncherLog::open(&dir, rotation, Redactor::new()).unwrap();
        // About 2 MiB of synthetic records: thirty times the rotation size.
        let message = "x".repeat(200);
        for n in 0..10_000 {
            log.write(LogLevel::Info, "volume", &message, &[("n", &n.to_string())]);
        }
        assert_eq!(
            names(&dir),
            [
                "launcher.1.log",
                "launcher.2.log",
                "launcher.3.log",
                "launcher.log"
            ]
        );
        for name in names(&dir) {
            let size = fs::metadata(dir.join(&name)).unwrap().len();
            assert!(size <= rotation.max_file_bytes, "{name}: {size}");
        }
        for name in ["launcher.1.log", "launcher.2.log", "launcher.3.log"] {
            let size = fs::metadata(dir.join(name)).unwrap().len();
            assert!(size > rotation.max_file_bytes - 512, "{name}: {size}");
        }
        // The newest record is last in the current file; the files hold consecutive records.
        let current = fs::read_to_string(log.path()).unwrap();
        assert!(current.trim_end().ends_with("n=9999"), "{current}");
        let older = fs::read_to_string(dir.join("launcher.1.log")).unwrap();
        let last_older: u32 = older
            .lines()
            .last()
            .and_then(|l| l.rsplit("n=").next())
            .unwrap()
            .parse()
            .unwrap();
        let first_current: u32 = current
            .lines()
            .next()
            .and_then(|l| l.rsplit("n=").next())
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(first_current, last_older + 1);
        drop(log);

        // Reopening appends to the current file.
        let before = fs::metadata(dir.join(FILE_NAME)).unwrap().len();
        let log = LauncherLog::open(&dir, rotation, Redactor::new()).unwrap();
        log.write(LogLevel::Info, "volume", "again", &[]);
        assert!(fs::metadata(log.path()).unwrap().len() > before);
        drop(log);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_rotation_fits_the_folder_limits() {
        assert_eq!(Rotation::within(64 << 20, 30), Rotation::default());
        assert_eq!(
            Rotation::within(1 << 20, 2),
            Rotation {
                max_file_bytes: 512 * 1024,
                max_files: 2
            }
        );
        assert_eq!(Rotation::within(0, 0).max_file_bytes, 64 * 1024);
    }

    #[test]
    fn a_single_file_is_replaced_when_full() {
        let dir = temp("single");
        let rotation = Rotation {
            max_file_bytes: 1024,
            max_files: 0,
        };
        let log = LauncherLog::open(&dir, rotation, Redactor::new()).unwrap();
        for n in 0..100 {
            log.write(LogLevel::Info, "t", "record", &[("n", &n.to_string())]);
        }
        assert_eq!(names(&dir), [FILE_NAME]);
        assert!(fs::metadata(log.path()).unwrap().len() <= 1024);
        drop(log);
        let _ = fs::remove_dir_all(dir);
    }
}
