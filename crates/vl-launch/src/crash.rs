//! Finding what a crashed game left behind.
//!
//! Before the start a [`Baseline`] notes the files that already exist; after the exit
//! [`Baseline::detect`] returns the crash report (`crash-reports/*.txt`) and the JVM fatal error
//! log (`hs_err_pid*.log` in the game directory) that are new or were written since the start.
//! Only the paths are collected; reading them is up to the caller.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// File times are coarse on some file systems; a file this much older than the start still counts.
const SLACK: Duration = Duration::from_secs(2);

/// What a run left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// The newest crash report written during the run.
    pub crash_report: Option<PathBuf>,
    /// The JVM fatal error log written during the run.
    pub hs_err: Option<PathBuf>,
}

/// The state of the game directory before the start.
#[derive(Debug, Clone)]
pub struct Baseline {
    started: SystemTime,
    reports: HashSet<OsString>,
    hs_errs: HashSet<OsString>,
}

impl Baseline {
    /// Notes the crash reports and error logs that exist in `game_dir` now.
    pub fn capture(game_dir: &Path) -> Self {
        Self {
            started: SystemTime::now(),
            reports: names(&game_dir.join("crash-reports"), is_crash_report),
            hs_errs: names(game_dir, is_hs_err),
        }
    }

    /// The new files in `game_dir`. `pid` prefers the `hs_err_pid<pid>.log` of that process.
    pub fn detect(&self, game_dir: &Path, pid: u32) -> Findings {
        let report = newest(
            &game_dir.join("crash-reports"),
            is_crash_report,
            &self.reports,
            self.started,
        );
        let mut errs = fresh(game_dir, is_hs_err, &self.hs_errs, self.started);
        let own = format!("hs_err_pid{pid}.log");
        let hs_err = errs
            .iter()
            .find(|(path, _)| path.file_name().is_some_and(|n| n == own.as_str()))
            .map(|(path, _)| path.clone())
            .or_else(|| {
                errs.sort_by_key(|(_, time)| *time);
                errs.pop().map(|(path, _)| path)
            });
        Findings {
            crash_report: report,
            hs_err,
        }
    }
}

fn is_crash_report(name: &str) -> bool {
    name.ends_with(".txt")
}

fn is_hs_err(name: &str) -> bool {
    name.starts_with("hs_err_pid") && name.ends_with(".log")
}

fn names(dir: &Path, wanted: fn(&str) -> bool) -> HashSet<OsString> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| n.to_str().is_some_and(wanted))
        .collect()
}

/// The files of `dir` that are not in `known` or were modified since `started`, with their times.
fn fresh(
    dir: &Path,
    wanted: fn(&str) -> bool,
    known: &HashSet<OsString>,
    started: SystemTime,
) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let threshold = started.checked_sub(SLACK).unwrap_or(started);
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(wanted))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            let is_new = !known.contains(&e.file_name());
            (is_new || modified >= threshold).then(|| (e.path(), modified))
        })
        .collect()
}

fn newest(
    dir: &Path,
    wanted: fn(&str) -> bool,
    known: &HashSet<OsString>,
    started: SystemTime,
) -> Option<PathBuf> {
    fresh(dir, wanted, known, started)
        .into_iter()
        .max_by_key(|(_, time)| *time)
        .map(|(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-crash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("crash-reports")).unwrap();
        dir
    }

    #[test]
    fn nothing_new_nothing_found() {
        let dir = temp("none");
        fs::write(dir.join("crash-reports/crash-old.txt"), "old").unwrap();
        fs::write(dir.join("hs_err_pid1.log"), "old").unwrap();
        // Make them old enough not to count by their time.
        let old = SystemTime::now() - Duration::from_secs(3600);
        for p in [
            dir.join("crash-reports/crash-old.txt"),
            dir.join("hs_err_pid1.log"),
        ] {
            fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let baseline = Baseline::capture(&dir);
        assert_eq!(baseline.detect(&dir, 7), Findings::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_files_are_found_and_the_own_hs_err_wins() {
        let dir = temp("new");
        let baseline = Baseline::capture(&dir);
        fs::write(dir.join("crash-reports/crash-1.txt"), "r").unwrap();
        fs::write(dir.join("hs_err_pid41.log"), "other").unwrap();
        fs::write(dir.join("hs_err_pid42.log"), "own").unwrap();
        fs::write(dir.join("notes.txt"), "not a report").unwrap();
        let found = baseline.detect(&dir, 42);
        assert_eq!(
            found.crash_report,
            Some(dir.join("crash-reports/crash-1.txt"))
        );
        assert_eq!(found.hs_err, Some(dir.join("hs_err_pid42.log")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_game_directory_finds_nothing() {
        let dir = std::env::temp_dir().join("vl-crash-missing-dir-xyz");
        let baseline = Baseline::capture(&dir);
        assert_eq!(baseline.detect(&dir, 1), Findings::default());
    }
}
