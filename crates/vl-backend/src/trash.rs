//! Deleting instances and the launcher's trash, served by `vl_instance::Trash`.
//!
//! The trash is `<data>/trash`. Delete, restore and purge run on their own threads (a move to
//! another volume copies the instance). Erasing what is beyond the limits of the `storage`
//! settings group runs as a `CleanTrash` task: when the backend starts (after
//! `Trash::recover` has finished what a crash interrupted) and after each deletion into the
//! trash, and only when there is something to erase.

use std::sync::Arc;
use std::thread;

use vl_core::instance::{InstanceEvent, InstanceSummary};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::storage::Section;
use vl_core::task::{ProgressUnit, TaskKind};
use vl_core::trash::{PurgeReport, PurgeTarget, TrashList};
use vl_core::{Error, Event, EventBus, InstanceId, Replier, Response};
use vl_instance::Trash;
use vl_settings::SettingsStore;
use vl_tasks::{TaskEngine, TaskSpec};

use crate::Backend;

/// Task kinds whose subject names the instance they work on.
const INSTANCE_TASKS: [TaskKind; 5] = [
    TaskKind::Launch,
    TaskKind::InstallResource,
    TaskKind::UpdatePack,
    TaskKind::VerifyFiles,
    TaskKind::Bisect,
];

/// What the trash work needs on another thread.
#[derive(Clone)]
struct Jobs {
    trash: Trash,
    settings: SettingsStore,
    events: Arc<EventBus>,
    tasks: TaskEngine,
}

impl Jobs {
    fn of(backend: &Backend) -> Self {
        Self {
            trash: backend.trash.clone(),
            settings: backend.settings.clone(),
            events: Arc::clone(&backend.events),
            tasks: backend.tasks.clone(),
        }
    }

    fn log(&self, level: LogLevel, text: String) {
        self.events.publish(&Event::Log(LogLine {
            source: LogSource::Launcher,
            level,
            text,
        }));
    }

    fn changed(&self) {
        self.events
            .publish(&Event::Instance(InstanceEvent::TrashChanged));
    }

    /// Starts a `CleanTrash` task when something is beyond the limits or left to erase.
    fn schedule_cleanup(&self) {
        let limits = self.settings.global().value.storage;
        let due = self
            .trash
            .due(limits.trash_days, limits.trash_max_bytes())
            .unwrap_or_default();
        if due.is_empty() && !self.trash.has_garbage() {
            return;
        }
        let jobs = self.clone();
        let spec = TaskSpec::new(TaskKind::CleanTrash).not_cancellable();
        self.tasks.spawn(spec, move |ctx| {
            jobs.trash.collect_garbage();
            for note in jobs.trash.adopt_legacy() {
                jobs.log(LogLevel::Warn, note);
            }
            // The limits as they are now; a setting may have changed meanwhile.
            let limits = jobs.settings.global().value.storage;
            let due = jobs
                .trash
                .due(limits.trash_days, limits.trash_max_bytes())?;
            let total = u64::try_from(due.len()).unwrap_or(u64::MAX);
            let mut report = PurgeReport::default();
            for (n, item) in due.iter().enumerate() {
                ctx.report(
                    u64::try_from(n).unwrap_or(u64::MAX),
                    Some(total),
                    ProgressUnit::Items,
                );
                let one = jobs.trash.purge_items(std::slice::from_ref(item));
                report.removed.extend(one.removed);
                report.failed.extend(one.failed);
                report.freed += one.freed;
            }
            ctx.report(total, Some(total), ProgressUnit::Items);
            if !report.removed.is_empty() {
                jobs.log(
                    LogLevel::Info,
                    format!(
                        "trash: erased {} beyond the limits ({} days, {} GiB): {}",
                        report.removed.len(),
                        limits.trash_days,
                        limits.trash_max_gib,
                        report.removed.join(", ")
                    ),
                );
            }
            for (item, why) in &report.failed {
                jobs.log(LogLevel::Warn, format!("trash: kept {item}: {why}"));
            }
            jobs.changed();
            Ok(())
        });
    }
}

/// Finishes interrupted trash work and erases what is beyond the limits; called once when the
/// backend starts. Nothing happens while the instances folder is unavailable.
pub(crate) fn on_start(backend: &Backend) {
    if backend.store.unavailable().contains(&Section::Instances) {
        return;
    }
    let jobs = Jobs::of(backend);
    for note in jobs.trash.recover() {
        jobs.log(LogLevel::Warn, note);
    }
    jobs.schedule_cleanup();
}

/// Refuses to delete an instance whose game runs or that a task works on.
fn refuse_if_busy(backend: &Backend, id: &InstanceId) -> vl_core::Result<()> {
    if backend.launch.is_active(id) {
        return Err(Error::conflict(format!(
            "the game of instance {id} is running; close it first"
        )));
    }
    if backend.bisect.contains(id) {
        return Err(Error::conflict(format!(
            "a mod search is running on instance {id}; stop it first"
        )));
    }
    let name = backend.instances.load(id).ok().map(|cfg| cfg.name);
    let busy = backend.tasks.snapshots().into_iter().find(|t| {
        INSTANCE_TASKS.contains(&t.info.kind)
            && t.info
                .subject
                .as_deref()
                .is_some_and(|s| s == id.as_str() || Some(s) == name.as_deref())
    });
    match busy {
        Some(task) => Err(Error::conflict(format!(
            "task {} ({:?}) works on instance {id}; wait for it or cancel it",
            task.info.id, task.info.kind
        ))),
        None => Ok(()),
    }
}

/// Runs `work` on its own thread and answers with its result.
fn answer_later(
    name: &str,
    replier: Replier,
    work: impl FnOnce() -> vl_core::Result<Response> + Send + 'static,
) {
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name(name.into())
        .spawn(move || replier.send(work()));
}

pub(crate) fn delete(
    backend: &Backend,
    id: InstanceId,
    comment: String,
    permanent: bool,
    replier: Replier,
) {
    if let Err(e) = refuse_if_busy(backend, &id) {
        return replier.send(Err(e));
    }
    let jobs = Jobs::of(backend);
    answer_later("instance-delete", replier, move || {
        if permanent {
            jobs.trash.delete_permanently(&id)?;
            jobs.events
                .publish(&Event::Instance(InstanceEvent::Removed(id)));
            return Ok(Response::Done);
        }
        let deleted = jobs.trash.delete(&id, &comment)?;
        for link in &deleted.skipped_links {
            jobs.log(
                LogLevel::Warn,
                format!(
                    "trash: link not copied to the trash on another drive: {}",
                    link.display()
                ),
            );
        }
        jobs.events
            .publish(&Event::Instance(InstanceEvent::Removed(id)));
        jobs.changed();
        jobs.schedule_cleanup();
        Ok(Response::Done)
    });
}

pub(crate) fn list(backend: &Backend, replier: Replier) {
    let jobs = Jobs::of(backend);
    answer_later("trash-list", replier, move || {
        let items = jobs.trash.list()?;
        let limits = jobs.settings.global().value.storage;
        Ok(Response::Trash(TrashList {
            size: items.iter().map(|i| i.size).sum(),
            items,
            keep_days: limits.trash_days,
            max_gib: limits.trash_max_gib,
        }))
    });
}

pub(crate) fn restore(backend: &Backend, item: String, replier: Replier) {
    let jobs = Jobs::of(backend);
    answer_later("trash-restore", replier, move || {
        let restored: InstanceSummary = jobs.trash.restore(&item)?;
        jobs.events
            .publish(&Event::Instance(InstanceEvent::Added(restored.clone())));
        jobs.changed();
        Ok(Response::Instance(restored))
    });
}

pub(crate) fn purge(backend: &Backend, target: PurgeTarget, replier: Replier) {
    let jobs = Jobs::of(backend);
    answer_later("trash-purge", replier, move || {
        let report = match target {
            PurgeTarget::Item(item) => jobs.trash.purge(&item)?,
            PurgeTarget::All => jobs.trash.purge_all()?,
            PurgeTarget::Limits => {
                let limits = jobs.settings.global().value.storage;
                let due = jobs
                    .trash
                    .due(limits.trash_days, limits.trash_max_bytes())?;
                jobs.trash.purge_items(&due)
            }
            other => {
                return Err(Error::unsupported(format!(
                    "vl-backend does not handle purge {other:?}"
                )));
            }
        };
        if !report.removed.is_empty() {
            jobs.changed();
        }
        Ok(Response::TrashPurged(report))
    });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use vl_core::instance::{InstanceCommand, NewInstance};
    use vl_core::storage::StorageCommand;
    use vl_core::trash::TrashItem;
    use vl_core::{Command, ErrorKind, Facade};

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
            std::env::temp_dir().join(format!("vl-backend-trash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        Dir(path)
    }

    fn open(dir: &Dir) -> Backend {
        Backend::new(BackendConfig {
            data_dir: Some(dir.0.join("data")),
        })
        .unwrap()
    }

    fn call(backend: &Backend, command: InstanceCommand) -> vl_core::Result<Response> {
        backend.call(Command::Instances(command)).wait()
    }

    fn create(backend: &Backend, name: &str) -> InstanceId {
        match call(
            backend,
            InstanceCommand::Create(NewInstance {
                name: name.into(),
                game_version: "1.21.1".into(),
                group: None,
                loader: None,
            }),
        ) {
            Ok(Response::Instance(i)) => {
                let game = backend.instances.game_dir(&i.id).unwrap();
                fs::write(game.join("options.txt"), vec![b'x'; 4096]).unwrap();
                i.id
            }
            other => panic!("{other:?}"),
        }
    }

    fn delete(backend: &Backend, id: &InstanceId, permanent: bool) -> vl_core::Result<Response> {
        call(
            backend,
            InstanceCommand::Delete {
                id: id.clone(),
                comment: "synthetic".into(),
                permanent,
            },
        )
    }

    fn trash(backend: &Backend) -> TrashList {
        match call(backend, InstanceCommand::ListTrash) {
            Ok(Response::Trash(list)) => list,
            other => panic!("{other:?}"),
        }
    }

    fn instances(backend: &Backend) -> usize {
        match call(backend, InstanceCommand::List) {
            Ok(Response::Instances(list)) => list.len(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn delete_list_restore_and_purge_through_the_facade() {
        let root = dir("flow");
        let backend = open(&root);
        let events = backend.subscribe();
        let id = create(&backend, "Survival");
        assert!(matches!(delete(&backend, &id, false), Ok(Response::Done)));
        assert_eq!(instances(&backend), 0);
        let list = trash(&backend);
        assert_eq!(list.keep_days, 30);
        assert_eq!(list.max_gib, 20);
        let [item]: [TrashItem; 1] = list.items.try_into().unwrap();
        assert_eq!(item.comment, "synthetic");
        assert!(item.size >= 4096);
        assert!(backend.store.trash_dir().join("Survival").is_dir());
        let seen: Vec<Event> = events.drain().collect();
        assert!(seen.contains(&Event::Instance(InstanceEvent::Removed(id.clone()))));
        assert!(seen.contains(&Event::Instance(InstanceEvent::TrashChanged)));

        let info = match backend
            .call(Command::Storage(StorageCommand::Show { sizes: true }))
            .wait()
            .unwrap()
        {
            Response::Storage(info) => info,
            other => panic!("{other:?}"),
        };
        assert!(info.trash.size.unwrap() >= 4096);
        let data = info.sections.iter().find(|s| s.section == Section::Data);
        assert!(data.unwrap().size.unwrap() < 4096);

        match call(&backend, InstanceCommand::Restore { item: item.id }) {
            Ok(Response::Instance(i)) => assert_eq!(i.id, id),
            other => panic!("{other:?}"),
        }
        assert_eq!(instances(&backend), 1);
        assert!(trash(&backend).items.is_empty());

        delete(&backend, &id, false).unwrap();
        match call(
            &backend,
            InstanceCommand::Purge(PurgeTarget::Item("Survival".into())),
        ) {
            Ok(Response::TrashPurged(report)) => assert_eq!(report.removed, ["Survival"]),
            other => panic!("{other:?}"),
        }
        assert!(trash(&backend).items.is_empty());
        let err = call(
            &backend,
            InstanceCommand::Purge(PurgeTarget::Item("../data".into())),
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn a_permanent_delete_and_a_cleanup_with_trash_leave_nothing() {
        let root = dir("permanent");
        let backend = open(&root);
        let a = create(&backend, "A");
        let b = create(&backend, "B");
        delete(&backend, &a, true).unwrap();
        assert!(trash(&backend).items.is_empty());
        delete(&backend, &b, false).unwrap();
        assert_eq!(trash(&backend).items.len(), 1);
        match backend
            .call(Command::Storage(StorageCommand::Cleanup { trash: true }))
            .wait()
            .unwrap()
        {
            Response::StorageCleaned(report) => {
                assert!(report.failed.is_empty(), "{report:?}");
                assert!(report.freed >= 4096);
            }
            other => panic!("{other:?}"),
        }
        assert!(trash(&backend).items.is_empty());
        assert_eq!(instances(&backend), 0);
    }

    #[test]
    fn delete_is_refused_while_a_task_works_on_the_instance() {
        let root = dir("busy");
        let backend = open(&root);
        let id = create(&backend, "Busy");
        let (release, wait) = mpsc::channel::<()>();
        let task = backend.tasks().spawn(
            TaskSpec::new(TaskKind::VerifyFiles).subject("Busy"),
            move |_| {
                let _ = wait.recv_timeout(Duration::from_secs(10));
                Ok(())
            },
        );
        for permanent in [false, true] {
            let err = delete(&backend, &id, permanent).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
        }
        release.send(()).unwrap();
        task.wait();
        delete(&backend, &id, false).unwrap();
        assert_eq!(instances(&backend), 0);
    }

    #[test]
    fn items_beyond_the_limits_are_erased_when_the_launcher_starts() {
        let root = dir("start");
        {
            let backend = open(&root);
            for name in ["Old", "New"] {
                let id = create(&backend, name);
                delete(&backend, &id, false).unwrap();
            }
        }
        let record = root.0.join("data/trash/Old.json");
        let text = fs::read_to_string(&record).unwrap();
        let mut meta: serde_json::Value = serde_json::from_str(&text).unwrap();
        meta["deleted_unix"] = serde_json::json!(1_000_000_000);
        fs::write(&record, serde_json::to_vec(&meta).unwrap()).unwrap();

        let backend = open(&root);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ids: Vec<String> = trash(&backend).items.into_iter().map(|i| i.id).collect();
            if ids == ["New"] {
                break;
            }
            assert!(Instant::now() < deadline, "{ids:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
