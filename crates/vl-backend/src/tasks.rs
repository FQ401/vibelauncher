//! Task commands, served by `vl-tasks`.

use vl_core::task::TaskCommand;
use vl_core::{Replier, Response};

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: TaskCommand, replier: Replier) {
    replier.send(match command {
        TaskCommand::List => Ok(Response::Tasks(backend.tasks().snapshots())),
        TaskCommand::Cancel { id } => backend.tasks().cancel(id).map(|()| Response::Done),
        other => Err(vl_core::Error::unsupported(format!(
            "vl-backend does not handle tasks {other:?}"
        ))),
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use vl_core::task::{TaskEvent, TaskKind, TaskOutcome};
    use vl_core::{Command, ErrorKind, Event, Facade, TaskId};
    use vl_tasks::TaskSpec;

    use super::*;
    use crate::BackendConfig;

    /// A scratch data directory under the system temp directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("vl-backend-tasks-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lists_and_cancels_tasks_over_the_facade() {
        let dir = Scratch::new();
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.0.clone()),
        })
        .unwrap();
        let events = backend.subscribe();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let handle = backend.tasks().spawn(
            TaskSpec::new(TaskKind::Download).subject("x"),
            move |task| {
                started_tx.send(()).ok();
                release_rx.recv().ok();
                task.check()
            },
        );
        started_rx.recv_timeout(Duration::from_secs(20)).unwrap();

        let listed = backend
            .call(Command::Tasks(TaskCommand::List))
            .wait()
            .unwrap();
        let Response::Tasks(tasks) = listed else {
            panic!("expected tasks");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].info.id, handle.id());

        let done = backend
            .call(Command::Tasks(TaskCommand::Cancel { id: handle.id() }))
            .wait()
            .unwrap();
        assert_eq!(done, Response::Done);
        release_tx.send(()).unwrap();
        assert_eq!(handle.wait(), TaskOutcome::Cancelled);

        let missing = backend
            .call(Command::Tasks(TaskCommand::Cancel { id: TaskId(4040) }))
            .wait()
            .unwrap_err();
        assert_eq!(missing.kind, ErrorKind::NotFound);

        let mut finished = 0;
        while let Some(event) = events.next_timeout(Duration::from_millis(200)) {
            if matches!(event, Event::Task(TaskEvent::Finished { .. })) {
                finished += 1;
            }
        }
        assert_eq!(finished, 1);
    }
}
