//! The file with the full output of one launch, and the rotation of old ones.
//!
//! Files are named `vl-game-YYYYMMDD-HHMMSS.log` (UTC) in the instance's `logs/` directory. Before
//! a new file is made the oldest ones beyond the limit are deleted; files of other names (the
//! game's own `latest.log`, for instance) are never touched.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PREFIX: &str = "vl-game-";
const SUFFIX: &str = ".log";

/// How many launch logs are kept by default, the running one included.
pub const DEFAULT_KEEP: usize = 10;

/// The log file of a launch. A write error ends the writing but never the game.
#[derive(Debug)]
pub struct GameLogFile {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl GameLogFile {
    /// Creates a new log file in `dir` after deleting the oldest launch logs so that at most
    /// `keep` (at least 1) remain with the new one.
    ///
    /// # Errors
    ///
    /// The I/O error when the directory or the file cannot be created.
    pub fn create(dir: &Path, keep: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        rotate(dir, keep.max(1).saturating_sub(1));
        let stamp = stamp(SystemTime::now());
        for n in 0..1000 {
            let name = if n == 0 {
                format!("{PREFIX}{stamp}{SUFFIX}")
            } else {
                format!("{PREFIX}{stamp}-{n}{SUFFIX}")
            };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        writer: Some(BufWriter::with_capacity(64 * 1024, file)),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free log file name",
        ))
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one line. After a write error nothing is written any more.
    pub fn write_line(&mut self, text: &str) {
        if let Some(writer) = &mut self.writer
            && (writer.write_all(text.as_bytes()).is_err() || writer.write_all(b"\n").is_err())
        {
            self.writer = None;
        }
    }

    /// Pushes buffered lines to the file.
    pub fn flush(&mut self) {
        if let Some(writer) = &mut self.writer
            && writer.flush().is_err()
        {
            self.writer = None;
        }
    }
}

/// Opens `path` with the system's default application for it (`explorer` on Windows, `open` on
/// macOS, `xdg-open` elsewhere) without waiting for that application.
///
/// # Errors
///
/// `NotFound` when the file does not exist; `Io` when the opener cannot be started.
pub fn open_with_system(path: &Path) -> vl_core::Result<()> {
    use vl_core::{Error, ErrorKind};
    if !path.is_file() {
        return Err(Error::not_found(format!(
            "the log file {} does not exist",
            path.display()
        )));
    }
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::new(ErrorKind::Io, format!("cannot start {program}: {e}")))?;
    // Collect the opener's exit so it does not linger; its result says nothing useful
    // (`explorer` exits with 1 after opening a file).
    let _ = std::thread::Builder::new()
        .name("vl-open-file".to_owned())
        .spawn(move || child.wait());
    Ok(())
}

/// Deletes the oldest launch logs in `dir` until at most `keep` are left.
fn rotate(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
        .collect();
    // The names sort by time.
    logs.sort();
    let excess = logs.len().saturating_sub(keep);
    for name in logs.into_iter().take(excess) {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// `YYYYMMDD-HHMMSS` in UTC.
fn stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The calendar date of a day count since 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-logfile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn formats_utc_stamps() {
        assert_eq!(stamp(UNIX_EPOCH), "19700101-000000");
        let t = UNIX_EPOCH + Duration::from_secs(1_709_210_096);
        assert_eq!(stamp(t), "20240229-123456");
    }

    #[test]
    fn writes_lines_and_rotates_old_logs() {
        let dir = temp("rotate");
        fs::create_dir_all(&dir).unwrap();
        for n in 0..5 {
            fs::write(
                dir.join(format!("{PREFIX}2020010{n}-000000{SUFFIX}")),
                "old",
            )
            .unwrap();
        }
        fs::write(dir.join("latest.log"), "game's own").unwrap();
        let mut file = GameLogFile::create(&dir, 3).unwrap();
        file.write_line("hello");
        file.write_line("world");
        file.flush();
        assert_eq!(fs::read_to_string(file.path()).unwrap(), "hello\nworld\n");

        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), 4, "{names:?}");
        assert!(names.contains(&"latest.log".to_owned()));
        // The oldest were deleted, the newest old ones and the new file remain.
        assert!(
            !names.iter().any(|n| n.contains("20200100")
                || n.contains("20200101")
                || n.contains("20200102"))
        );
        assert!(names.iter().any(|n| n.contains("20200104")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_logs_in_one_second_get_different_names() {
        let dir = temp("same-second");
        let a = GameLogFile::create(&dir, 10).unwrap();
        let b = GameLogFile::create(&dir, 10).unwrap();
        assert_ne!(a.path(), b.path());
        let _ = fs::remove_dir_all(&dir);
    }
}
