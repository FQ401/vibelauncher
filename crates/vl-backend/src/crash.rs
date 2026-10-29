//! Crash diagnosis: reads the logs of a run and hands them to `vl-crash`.
//!
//! After a run that ended in a crash the backend diagnoses it on its own thread and publishes
//! `LaunchEvent::Diagnosed` plus a line in the launcher log; `LaunchCommand::Diagnose` does the
//! same for the newest logs of an instance. Inputs are capped: the tail of the output log, the
//! head of a crash report or JVM error log (their first lines carry the cause).

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::SystemTime;

use vl_core::crash::{CrashCause, CrashDiagnosis, DiagnosisLevel, EvidenceLine, EvidenceSource};
use vl_core::launch::LaunchEvent;
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::{Event, EventBus, InstanceId, Replier, Response};
use vl_crash::{Cause, CrashInput, Level, Source};
use vl_resources::Resources;

use crate::Backend;

/// The most bytes read from the end of an output log.
const MAX_LOG_BYTES: u64 = 512 * 1024;
/// The most bytes read from the start of a crash report or JVM error log.
const MAX_REPORT_BYTES: u64 = 256 * 1024;
pub(crate) const RUN_LOG_PREFIX: &str = "vl-game-";

/// Where the logs of an instance are.
#[derive(Debug, Clone)]
pub(crate) struct Sources {
    pub game_dir: PathBuf,
    /// `<instance>/logs`, where the launcher keeps the output of each run.
    pub log_dir: PathBuf,
    /// The crash report of a known run; `None` takes the newest one.
    pub crash_report: Option<PathBuf>,
    /// The JVM error log of a known run; `None` takes the newest one.
    pub hs_err: Option<PathBuf>,
    /// The output log of a known run; `None` takes the newest one.
    pub run_log: Option<PathBuf>,
}

impl Sources {
    pub(crate) fn newest(game_dir: PathBuf, log_dir: PathBuf) -> Self {
        Self {
            game_dir,
            log_dir,
            crash_report: None,
            hs_err: None,
            run_log: None,
        }
    }
}

/// `LaunchCommand::Diagnose`.
pub(crate) fn handle(backend: &Backend, instance: InstanceId, replier: Replier) {
    let sources = backend
        .instances
        .game_dir(&instance)
        .and_then(|game| Ok((game, backend.instances.dir(&instance)?)))
        .and_then(|(game, dir)| {
            if dir.is_dir() {
                Ok(Sources::newest(game, dir.join("logs")))
            } else {
                Err(vl_core::Error::not_found(format!("instance {instance}")))
            }
        });
    let sources = match sources {
        Ok(s) => s,
        Err(e) => return replier.send(Err(e)),
    };
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name("diagnose".into())
        .spawn(move || replier.send(diagnose(&sources).map(Response::Diagnosis)));
}

/// Diagnoses a finished run on its own thread and publishes the result.
pub(crate) fn spawn_after_exit(events: Arc<EventBus>, id: InstanceId, sources: Sources) {
    let _ = thread::Builder::new()
        .name("diagnose".into())
        .spawn(move || match diagnose(&sources) {
            Ok(diagnosis) => {
                events.publish(&Event::Log(LogLine {
                    source: LogSource::Launcher,
                    level: LogLevel::Warn,
                    text: describe(&diagnosis),
                }));
                events.publish(&Event::Launch(LaunchEvent::Diagnosed {
                    instance: id,
                    diagnosis,
                }));
            }
            Err(e) => events.publish(&Event::Log(LogLine {
                source: LogSource::Launcher,
                level: LogLevel::Warn,
                text: format!("cannot diagnose the crash: {}", e.message),
            })),
        });
}

/// The journal line of a diagnosis.
fn describe(d: &CrashDiagnosis) -> String {
    let mut text = format!("crash diagnosis: {} ({:?})", d.message_key, d.cause);
    if !d.mods.is_empty() {
        text.push_str(&format!(", mods: {}", d.mods.join(", ")));
    }
    for (key, value) in &d.args {
        text.push_str(&format!(", {key}={value}"));
    }
    if let Some(action) = &d.action_key {
        text.push_str(&format!("; suggested: {action}"));
    }
    if d.loader_reported {
        text.push_str("; reported by the mod loader");
    }
    text
}

/// Reads the logs and diagnoses them with the class index of the instance's mods.
pub(crate) fn diagnose(sources: &Sources) -> vl_core::Result<CrashDiagnosis> {
    diagnose_with_text(sources, true).map(|(d, _)| d)
}

/// Like [`diagnose`], plus the text the crash is best described by (the crash report, else the
/// output log). With `newest` false only the files named in `sources` are read: a run that left
/// no crash report is not judged by an older one.
pub(crate) fn diagnose_with_text(
    sources: &Sources,
    newest: bool,
) -> vl_core::Result<(CrashDiagnosis, String)> {
    let mut files = Vec::new();
    let mut take = |path: Option<PathBuf>, head: bool| -> String {
        let Some(path) = path else {
            return String::new();
        };
        let text = if head {
            read_head(&path, MAX_REPORT_BYTES)
        } else {
            read_tail(&path, MAX_LOG_BYTES)
        };
        match text {
            Some(text) => {
                files.push(file_name(&path));
                text
            }
            None => String::new(),
        }
    };
    let run_log = sources.run_log.clone().or_else(|| {
        newest
            .then(|| newest_in(&sources.log_dir, |n| n.starts_with(RUN_LOG_PREFIX)))
            .flatten()
    });
    let latest = sources.game_dir.join("logs").join("latest.log");
    let mut log = take(run_log, false);
    if log.is_empty() {
        log = take(Some(latest), false);
    }
    let report = take(
        sources.crash_report.clone().or_else(|| {
            newest
                .then(|| {
                    newest_in(&sources.game_dir.join("crash-reports"), |n| {
                        n.ends_with(".txt")
                    })
                })
                .flatten()
        }),
        true,
    );
    let hs_err = take(
        sources.hs_err.clone().or_else(|| {
            newest
                .then(|| {
                    newest_in(&sources.game_dir, |n| {
                        n.starts_with("hs_err_pid") && n.ends_with(".log")
                    })
                })
                .flatten()
        }),
        true,
    );

    let trash = sources.log_dir.join("..").join(".trash");
    let cache = sources.log_dir.join("..").join(vl_resources::META_DIR);
    let index = Resources::new(sources.game_dir.clone(), trash)
        .with_cache_dir(cache)
        .class_index()?;
    let input = CrashInput {
        latest_log: &log,
        crash_report: &report,
        hs_err: &hs_err,
    };
    let diagnosis = convert(vl_crash::diagnose(&input, &index), files);
    let text = if report.is_empty() { log } else { report };
    Ok((diagnosis, text))
}

fn convert(d: vl_crash::Diagnosis, files: Vec<String>) -> CrashDiagnosis {
    CrashDiagnosis {
        level: match d.level {
            Level::Signature => DiagnosisLevel::Signature,
            Level::SingleMod => DiagnosisLevel::SingleMod,
            Level::SeveralMods => DiagnosisLevel::SeveralMods,
            Level::Undetermined => DiagnosisLevel::Undetermined,
        },
        cause: match d.cause {
            Cause::ModLikely => CrashCause::ModLikely,
            Cause::Environment => CrashCause::Environment,
            Cause::Unknown => CrashCause::Unknown,
        },
        loader_reported: d.loader_reported,
        signature: d.signature,
        mods: d.culprit_mods,
        message_key: d.message_key,
        action_key: d.action_key,
        args: d.args,
        evidence: d
            .evidence_lines
            .into_iter()
            .map(|e| EvidenceLine {
                source: match e.source {
                    Source::LatestLog => EvidenceSource::Log,
                    Source::CrashReport => EvidenceSource::CrashReport,
                    Source::HsErr => EvidenceSource::HsErr,
                },
                line: u32::try_from(e.line).unwrap_or(u32::MAX),
                text: e.text,
            })
            .collect(),
        files,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The most recently modified file in `dir` whose name satisfies `accept`.
pub(crate) fn newest_in(dir: &Path, accept: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !accept(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let time = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| time > *t) {
            best = Some((time, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// The last `max` bytes of a file as text, starting at a line boundary.
fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        return Some(match text.split_once('\n') {
            Some((_, rest)) => rest.to_owned(),
            None => String::new(),
        });
    }
    Some(text)
}

/// The first `max` bytes of a file as text, ending at a line boundary.
fn read_head(path: &Path, max: u64) -> Option<String> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if len > max
        && let Some(end) = text.rfind('\n')
    {
        text.truncate(end);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vl-backend-crash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tail_starts_at_a_line_and_head_ends_at_one() {
        let dir = temp("cut");
        let file = dir.join("a.log");
        fs::write(&file, "first line\nsecond line\nthird line\n").unwrap();
        assert_eq!(
            read_tail(&file, 14).unwrap(),
            "third line
"
        );
        assert_eq!(
            read_tail(&file, 24).unwrap(),
            "second line
third line
"
        );
        assert_eq!(
            read_tail(&file, 1000).unwrap(),
            "first line\nsecond line\nthird line\n"
        );
        assert_eq!(read_head(&file, 15).unwrap(), "first line");
        assert!(read_head(&dir.join("missing"), 10).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn diagnoses_the_newest_logs_of_a_game_directory() {
        let dir = temp("diag");
        let game = dir.join("game");
        let logs = dir.join("logs");
        fs::create_dir_all(game.join("crash-reports")).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("vl-game-20260101-000000.log"),
            "[main/INFO]: start\nSome of your mods are incompatible with the game or each other!\n\t - Mod 'Example' (example) 1.0 requires version 2.0 or later of fabric-api, which is missing!\n",
        )
        .unwrap();
        let sources = Sources::newest(game, logs);
        let d = diagnose(&sources).unwrap();
        assert_eq!(d.level, DiagnosisLevel::Signature);
        assert_eq!(d.signature.as_deref(), Some("missing_dependency"));
        assert_eq!(d.cause, CrashCause::ModLikely);
        assert!(d.loader_reported && !d.bisect_useful());
        assert_eq!(d.mods, ["example"]);
        assert_eq!(
            d.args.get("dependency").map(String::as_str),
            Some("fabric-api")
        );
        assert_eq!(d.files, ["vl-game-20260101-000000.log"]);
        assert!(describe(&d).contains("crash-missing-dependency"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_english_locale_has_every_diagnosis_key() {
        let ftl = include_str!("../../vl-ui-egui/locales/en/vibelauncher.ftl");
        for key in vl_crash::localization_keys() {
            assert!(
                ftl.lines().any(|l| l.starts_with(&format!("{key} ="))),
                "missing {key}"
            );
        }
    }

    #[test]
    fn no_logs_is_undetermined() {
        let dir = temp("none");
        let sources = Sources::newest(dir.join("game"), dir.join("logs"));
        let d = diagnose(&sources).unwrap();
        assert_eq!(d.level, DiagnosisLevel::Undetermined);
        assert_eq!(d.cause, CrashCause::Unknown);
        assert!(d.files.is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
