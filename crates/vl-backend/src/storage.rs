//! Storage commands, served by `vl-store`: the layout, folder checks, moving a section, cleanup.
//!
//! The services of the backend hold the section folders they were started with, so a move ends
//! with a restart: while a move runs and after it succeeded, every command except storage, tasks
//! and `Hello` is refused. The same happens while the folder of instances, shared game files, Java
//! or the cache is unavailable (an explicit folder on a disconnected drive).

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::storage::{CleanupReport, MoveRequest, Section, StorageCommand};
use vl_core::task::TaskKind;
use vl_core::{Command, Error, Event, Replier, Response};
use vl_store::{StoreError, mover, retention};
use vl_tasks::TaskSpec;

use crate::Backend;
use crate::updates::spawn_reply;

/// Sections without which the services cannot work.
const REQUIRED: [Section; 4] = [
    Section::Instances,
    Section::Game,
    Section::Java,
    Section::Cache,
];

/// Whether commands may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Open,
    Moving,
    Restart,
}

/// The storage state shared with the move task.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gate(Arc<Mutex<State>>);

impl Gate {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(&self) -> State {
        *self.lock()
    }
}

/// Sets the gate when the move task ends, however it ends (a panic included).
struct MoveGuard {
    gate: Gate,
    succeeded: bool,
}

impl Drop for MoveGuard {
    fn drop(&mut self) {
        *self.gate.lock() = if self.succeeded {
            State::Restart
        } else {
            State::Open
        };
    }
}

/// Refuses a command that needs the section folders while they are moving, moved or unavailable.
pub(crate) fn admit(backend: &Backend, command: &Command) -> Result<(), Error> {
    if matches!(
        command,
        Command::Hello | Command::Storage(_) | Command::Tasks(_)
    ) {
        return Ok(());
    }
    match backend.storage.get() {
        State::Moving => {
            return Err(Error::conflict(
                "a storage section is being moved; wait until the task ends",
            ));
        }
        State::Restart => {
            return Err(Error::conflict(
                "a storage section was moved; restart the launcher to use the new folder",
            ));
        }
        State::Open => {}
    }
    let missing: Vec<String> = REQUIRED
        .into_iter()
        .filter(|s| backend.store.unavailable().contains(s))
        .map(|s| format!("{s} ({})", backend.store.dir(s).display()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::conflict(format!(
            "the folder of {} is unavailable: reconnect its drive, or switch the section to \
             another folder",
            missing.join(", ")
        )))
    }
}

pub(crate) fn handle(backend: &Backend, command: StorageCommand, replier: Replier) {
    match command {
        StorageCommand::Show { sizes } => {
            let restart = backend.storage.get() == State::Restart;
            let view = backend.store.view();
            if sizes {
                spawn_reply("storage-show", replier, move || {
                    let mut info = view.info(true);
                    info.restart_required = restart;
                    Ok(Response::Storage(info))
                });
            } else {
                let mut info = view.info(false);
                info.restart_required = restart;
                replier.send(Ok(Response::Storage(info)));
            }
        }
        StorageCommand::Check { section, path } => {
            let view = backend.store.view();
            spawn_reply("storage-check", replier, move || {
                Ok(Response::PathCheck(view.check(section, &path)))
            });
        }
        StorageCommand::Move(request) => replier.send(start_move(backend, request)),
        StorageCommand::Cleanup { trash } => {
            if backend.storage.get() != State::Open {
                return replier.send(Err(Error::conflict(
                    "restart the launcher after a move before cleaning up",
                )));
            }
            let view = backend.store.view();
            let instances = backend.store.instances_dir();
            spawn_reply("storage-cleanup", replier, move || {
                let mut report = view.cleanup().map_err(Error::from)?;
                if trash {
                    empty_trash(&instances, &mut report);
                }
                Ok(Response::StorageCleaned(report))
            });
        }
        other => crate::not_yet(replier, "storage", &other),
    }
}

fn start_move(backend: &Backend, request: MoveRequest) -> vl_core::Result<Response> {
    let mut state = backend.storage.lock();
    if *state != State::Open {
        return Err(Error::conflict(
            "a storage section was moved or is moving; restart the launcher first",
        ));
    }
    if !backend.tasks().snapshots().is_empty() {
        return Err(Error::conflict(
            "tasks are running; move the folder when they have ended",
        ));
    }
    if backend.launch.any_active() {
        return Err(Error::conflict("a game is running; close it first"));
    }
    let (plan, _) = backend.store.plan_move(&request)?;
    *state = State::Moving;
    drop(state);

    let section = request.section;
    let guard = MoveGuard {
        gate: backend.storage.clone(),
        succeeded: false,
    };
    let data_dir = backend.store.data_dir().to_path_buf();
    let events = Arc::clone(&backend.events);
    let spec = TaskSpec::new(TaskKind::MoveStorage)
        .subject(section.as_str())
        .stage("copying");
    let handle = backend.tasks().spawn(spec, move |ctx| {
        // Moved in whole, so the gate is set when the task ends, not when this function returns.
        let mut guard = guard;
        let report = mover::execute(&plan, &mut |p| {
            ctx.report_bytes(p.bytes, Some(p.total_bytes));
            if ctx.is_cancelled() {
                Err(StoreError::Cancelled)
            } else {
                Ok(())
            }
        })?;
        guard.succeeded = true;
        if section == Section::Java {
            // Probe results name the old runtime paths; the next scan rebuilds them.
            let _ = fs::remove_file(data_dir.join("java-cache.json"));
        }
        for link in &report.skipped_links {
            events.publish(&Event::Log(LogLine {
                source: LogSource::Launcher,
                level: LogLevel::Warn,
                text: format!("storage move: link not copied: {}", link.display()),
            }));
        }
        events.publish(&Event::Log(LogLine {
            source: LogSource::Launcher,
            level: LogLevel::Info,
            text: format!(
                "storage move: {section} is now in {} ({} files); the old copy stays in {} until \
                 a cleanup; restart the launcher",
                report.to.display(),
                report.files,
                report.from.display()
            ),
        }));
        Ok(())
    });
    Ok(Response::Task(handle.id()))
}

/// Empties the trash of the instances folder and of every instance.
fn empty_trash(instances: &Path, report: &mut CleanupReport) {
    let mut trashes = vec![instances.join(".trash")];
    if let Ok(entries) = fs::read_dir(instances) {
        trashes.extend(
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path().join(".trash")),
        );
    }
    for trash in trashes.into_iter().filter(|t| t.is_dir()) {
        let size = retention::size_of(&trash);
        match fs::remove_dir_all(&trash) {
            Ok(()) => {
                report.freed += size;
                report.removed.push(trash);
            }
            Err(e) => report.failed.push((trash, e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use vl_core::instance::InstanceCommand;
    use vl_core::storage::{MoveMode, StorageInfo};
    use vl_core::task::{TaskEvent, TaskOutcome};
    use vl_core::{ErrorKind, Facade};

    use super::*;
    use crate::BackendConfig;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dir(name: &str) -> Dir {
        let path =
            std::env::temp_dir().join(format!("vl-backend-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Dir(path)
    }

    fn show(backend: &Backend) -> StorageInfo {
        match backend
            .call(Command::Storage(StorageCommand::Show { sizes: true }))
            .wait()
            .unwrap()
        {
            Response::Storage(info) => info,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_move_runs_as_a_task_and_asks_for_a_restart() {
        let root = dir("move");
        let config = BackendConfig {
            data_dir: Some(root.0.join("data")),
        };
        let backend = Backend::new(config.clone()).unwrap();
        fs::write(backend.store.instances_dir().join("notes.txt"), "synthetic").unwrap();
        let info = show(&backend);
        assert!(info.sections.iter().all(|s| s.default));
        assert!(!info.restart_required);

        let events = backend.subscribe();
        let target = root.0.join("\u{421}\u{431}\u{43e}\u{440}\u{43a}\u{438} \u{43d}\u{430} \u{434}\u{440}\u{443}\u{433}\u{43e}\u{43c} \u{434}\u{438}\u{441}\u{43a}\u{435}");
        let task = match backend
            .call(Command::Storage(StorageCommand::Move(MoveRequest {
                section: Section::Instances,
                target: Some(target.clone()),
                mode: MoveMode::Copy,
            })))
            .wait()
            .unwrap()
        {
            Response::Task(id) => id,
            other => panic!("{other:?}"),
        };
        let outcome = loop {
            match events.next_timeout(Duration::from_secs(10)) {
                Some(Event::Task(TaskEvent::Finished { id, outcome })) if id == task => {
                    break outcome;
                }
                Some(_) => {}
                None => panic!("the move did not finish"),
            }
        };
        assert_eq!(outcome, TaskOutcome::Succeeded);
        assert!(show(&backend).restart_required);
        let refused = backend
            .call(Command::Instances(InstanceCommand::List))
            .wait()
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Conflict);
        drop(backend);

        let backend = Backend::new(config).unwrap();
        let info = show(&backend);
        let instances = info
            .sections
            .iter()
            .find(|s| s.section == Section::Instances)
            .unwrap();
        assert!(!instances.default);
        assert_eq!(
            fs::read_to_string(instances.path.join("notes.txt")).unwrap(),
            "synthetic"
        );
        assert_eq!(info.leftovers.len(), 1);
        backend
            .call(Command::Instances(InstanceCommand::List))
            .wait()
            .unwrap();
        match backend
            .call(Command::Storage(StorageCommand::Cleanup { trash: true }))
            .wait()
            .unwrap()
        {
            Response::StorageCleaned(report) => assert!(report.failed.is_empty(), "{report:?}"),
            other => panic!("{other:?}"),
        }
        assert!(show(&backend).leftovers.is_empty());
    }

    #[test]
    fn a_bad_target_is_refused_before_any_task() {
        let root = dir("refuse");
        let backend = Backend::new(BackendConfig {
            data_dir: Some(root.0.join("data")),
        })
        .unwrap();
        let inside = backend.store.instances_dir().join("cache");
        let err = backend
            .call(Command::Storage(StorageCommand::Move(MoveRequest {
                section: Section::Cache,
                target: Some(inside),
                mode: MoveMode::Copy,
            })))
            .wait()
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(!show(&backend).restart_required);
        backend
            .call(Command::Instances(InstanceCommand::List))
            .wait()
            .unwrap();
    }
}
