//! Java commands, served by `vl-java`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vl_core::java::{JavaCommand, JavaEvent, JavaRuntime};
use vl_core::task::{TaskEvent, TaskInfo, TaskKind, TaskOutcome};
use vl_core::{Error, Event, EventBus, Replier, Response, TaskId};
use vl_java::{FinderOptions, JavaFinder};

use crate::{Backend, BackendConfig};

/// The Java runtimes of this machine: scanned on demand, remembered until the next rescan.
pub(crate) struct JavaService {
    inner: Arc<Inner>,
}

struct Inner {
    finder: JavaFinder,
    events: Arc<EventBus>,
    /// The result of the last scan; held during a scan so scans never overlap.
    last: Mutex<Option<Vec<JavaRuntime>>>,
    next_task: AtomicU64,
}

impl JavaService {
    pub(crate) fn new(config: &BackendConfig, events: Arc<EventBus>) -> Self {
        let options = match &config.data_dir {
            Some(dir) => FinderOptions {
                managed_dir: Some(dir.join("java")),
                cache_file: Some(dir.join("java-cache.json")),
                ..FinderOptions::default()
            },
            None => FinderOptions::default(),
        };
        Self {
            inner: Arc::new(Inner {
                finder: JavaFinder::new(options),
                events,
                last: Mutex::new(None),
                next_task: AtomicU64::new(1),
            }),
        }
    }
}

impl Inner {
    /// Scans (or, when `force` is false and a result exists, reuses it) and stores the result.
    fn runtimes(&self, force: bool) -> Vec<JavaRuntime> {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        match &*last {
            Some(list) if !force => list.clone(),
            _ => {
                let list = self.finder.scan();
                *last = Some(list.clone());
                list
            }
        }
    }
}

pub(crate) fn handle(backend: &Backend, command: JavaCommand, replier: Replier) {
    let inner = Arc::clone(&backend.java.inner);
    match command {
        JavaCommand::List => spawn(replier, move |replier| {
            replier.send(Ok(Response::Javas(inner.runtimes(false))));
        }),
        JavaCommand::Rescan => {
            let id = TaskId(inner.next_task.fetch_add(1, Ordering::Relaxed));
            inner
                .events
                .publish(&Event::Task(TaskEvent::Started(TaskInfo {
                    id,
                    parent: None,
                    kind: TaskKind::JavaScan,
                    subject: None,
                    cancellable: false,
                })));
            replier.send(Ok(Response::Task(id)));
            let worker = Arc::clone(&inner);
            let started = thread::Builder::new()
                .name("java-scan".into())
                .spawn(move || {
                    worker.runtimes(true);
                    worker.events.publish(&Event::Java(JavaEvent::ListChanged));
                    finish(&worker, id, TaskOutcome::Succeeded);
                });
            if started.is_err() {
                let err = Error::internal("cannot start the Java scan thread");
                finish(&inner, id, TaskOutcome::Failed(err));
            }
        }
        _ => crate::not_yet(replier, "java", &command),
    }
}

fn finish(inner: &Inner, id: TaskId, outcome: TaskOutcome) {
    inner
        .events
        .publish(&Event::Task(TaskEvent::Finished { id, outcome }));
}

/// Runs `work` on its own thread so a slow scan never blocks the caller of `call`.
fn spawn(replier: Replier, work: impl FnOnce(Replier) + Send + 'static) {
    // The replier moves into the thread; if the thread cannot start it is dropped, and the
    // caller's `wait` reports that the backend dropped the command.
    let _ = thread::Builder::new()
        .name("java-list".into())
        .spawn(move || work(replier));
}

#[cfg(test)]
mod tests {
    use vl_core::{Command, Facade};

    use super::*;

    #[test]
    fn lists_runtimes_without_failing() {
        let backend = Backend::new(BackendConfig::default()).unwrap();
        let reply = backend.call(Command::Java(JavaCommand::List)).wait();
        assert!(matches!(reply, Ok(Response::Javas(_))));
    }
}
