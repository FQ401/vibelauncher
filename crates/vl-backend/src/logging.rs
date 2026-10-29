//! The launcher log file: what the backend does, written to `launcher.log` in the logs section.
//!
//! The log observes the event bus, so every launcher log line published as an event, every task
//! start and end, the game's state changes, exits and crash diagnoses become records without the
//! domain modules knowing about the file. Game output itself stays in the run's own log file.
//! Front ends add records of their own with [`Backend::log`](crate::Backend::log).
//!
//! Records are masked (`vl_core::redact`): secrets always, the home folder while the setting
//! `advanced.log_mask_home` is on.

use std::sync::Arc;

use vl_core::bisect::BisectEvent;
use vl_core::launch::LaunchEvent;
use vl_core::log::{LogLevel, LogSource};
use vl_core::redact::Redactor;
use vl_core::settings::SettingsEvent;
use vl_core::storage::Section;
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{Event, EventBus};
use vl_logs::{LauncherLog, Rotation};
use vl_settings::SettingsStore;
use vl_store::Store;

/// The masking of launcher records and uploaded logs under the current settings.
pub(crate) fn redactor(settings: &SettingsStore) -> Redactor {
    let redactor = Redactor::new();
    if !settings.global().value.advanced.log_mask_home {
        return redactor;
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };
    match home.and_then(|h| h.into_string().ok()) {
        // Windows and macOS file systems ignore case.
        Some(home) => redactor.with_home(&home, cfg!(any(windows, target_os = "macos"))),
        None => redactor,
    }
}

/// Opens the launcher log of `store` and has it observe `events`; `None` when the logs folder is
/// unavailable or cannot be written (the launcher works on without a log file).
pub(crate) fn open(
    store: &Store,
    settings: &SettingsStore,
    events: &EventBus,
) -> Option<Arc<LauncherLog>> {
    if store.unavailable().contains(&Section::Logs) {
        return None;
    }
    // The rotation stays within the limits of the logs folder, so a prune never takes the
    // current file.
    let limits = settings.global().value.advanced;
    let rotation = Rotation::within(
        u64::from(limits.log_max_mib) << 20,
        limits.log_max_files as usize,
    );
    let log = Arc::new(LauncherLog::open(&store.logs_dir(), rotation, redactor(settings)).ok()?);
    let source = format!("{:?}", store.source());
    let data = store.data_dir().display().to_string();
    log.write(
        LogLevel::Info,
        "launcher",
        "started",
        &[
            ("version", env!("CARGO_PKG_VERSION")),
            ("os", std::env::consts::OS),
            ("arch", std::env::consts::ARCH),
            ("data", &data),
            ("source", &source),
        ],
    );
    let observer = Arc::clone(&log);
    events.observe(Box::new(move |event| record(&observer, event)));
    Some(log)
}

/// The record of `event`, if it makes one.
fn record(log: &LauncherLog, event: &Event) {
    match event {
        Event::Log(line) => {
            let target = match &line.source {
                LogSource::Launcher => "launcher".to_owned(),
                LogSource::Game(instance) => format!("game {instance}"),
            };
            log.write(line.level, &target, &line.text, &[]);
        }
        Event::Task(TaskEvent::Started(info)) => {
            let id = info.id.0.to_string();
            let kind = format!("{:?}", info.kind);
            let mut fields = vec![("id", id.as_str()), ("kind", kind.as_str())];
            if let Some(subject) = &info.subject {
                fields.push(("subject", subject));
            }
            log.write(LogLevel::Info, "task", "started", &fields);
        }
        Event::Task(TaskEvent::Finished { id, outcome }) => {
            let id = id.0.to_string();
            match outcome {
                TaskOutcome::Failed(e) => {
                    let kind = format!("{:?}", e.kind);
                    log.write(
                        LogLevel::Error,
                        "task",
                        "failed",
                        &[("id", &id), ("error", &kind), ("message", &e.message)],
                    );
                }
                TaskOutcome::Cancelled => {
                    log.write(LogLevel::Info, "task", "cancelled", &[("id", &id)]);
                }
                _ => log.write(LogLevel::Info, "task", "succeeded", &[("id", &id)]),
            }
        }
        Event::Launch(LaunchEvent::StateChanged { instance, state }) => {
            let state = format!("{state:?}");
            log.write(
                LogLevel::Info,
                "launch",
                "state",
                &[("instance", instance.as_str()), ("state", &state)],
            );
        }
        Event::Launch(LaunchEvent::Exited { instance, exit }) => {
            let code = exit
                .code
                .map_or_else(|| "none".to_owned(), |c| c.to_string());
            let seconds = (exit.session_ms / 1000).to_string();
            let level = if exit.crashed {
                LogLevel::Warn
            } else {
                LogLevel::Info
            };
            log.write(
                level,
                "launch",
                "game exited",
                &[
                    ("instance", instance.as_str()),
                    ("code", &code),
                    ("crashed", bool_str(exit.crashed)),
                    ("stopped_by_user", bool_str(exit.stopped_by_user)),
                    ("seconds", &seconds),
                ],
            );
        }
        Event::Launch(LaunchEvent::Diagnosed {
            instance,
            diagnosis,
        }) => {
            let cause = format!("{:?}", diagnosis.cause);
            log.write(
                LogLevel::Info,
                "crash",
                "diagnosed",
                &[
                    ("instance", instance.as_str()),
                    ("cause", &cause),
                    (
                        "signature",
                        diagnosis.signature.as_deref().unwrap_or("none"),
                    ),
                    ("message", &diagnosis.message_key),
                ],
            );
        }
        Event::Bisect(BisectEvent::Finished { instance, .. }) => {
            log.write(
                LogLevel::Info,
                "bisect",
                "finished",
                &[("instance", instance.as_str())],
            );
        }
        Event::Settings(SettingsEvent::Warning { instance, message }) => {
            let instance = instance.as_ref().map_or("global", |i| i.as_str());
            log.write(
                LogLevel::Warn,
                "settings",
                message,
                &[("instance", instance)],
            );
        }
        _ => {}
    }
}

fn bool_str(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use vl_core::log::LogLine;
    use vl_core::{Error, ErrorKind, TaskId};

    use super::*;
    use crate::{Backend, BackendConfig};

    #[test]
    fn events_and_front_end_records_reach_the_file_masked() {
        let dir = std::env::temp_dir().join(format!("vl-backend-logging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        backend.events().publish(&Event::Log(LogLine {
            source: LogSource::Launcher,
            level: LogLevel::Warn,
            text: "token refresh: refresh_token=synthetic-refresh-value".into(),
        }));
        backend.events().publish(&Event::Task(TaskEvent::Finished {
            id: TaskId(7),
            outcome: TaskOutcome::Failed(Error::new(ErrorKind::Network, "offline")),
        }));
        backend.log(LogLevel::Info, "app", "window opened");

        let path = backend.store().logs_dir().join(vl_logs::FILE_NAME);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(" INFO  launcher: started version="), "{text}");
        assert!(
            text.contains("WARN  launcher: token refresh: refresh_token=<redacted>"),
            "{text}"
        );
        assert!(!text.contains("synthetic-refresh-value"), "{text}");
        assert!(
            text.contains("ERROR task: failed id=7 error=Network message=offline"),
            "{text}"
        );
        assert!(text.contains("INFO  app: window opened"), "{text}");
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
