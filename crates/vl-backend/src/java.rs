//! Java commands, served by `vl-java`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vl_core::java::{JavaCommand, JavaEvent, JavaRuntime};
use vl_core::platform::Platform;
use vl_core::task::{ProgressUnit, TaskEvent, TaskInfo, TaskKind, TaskOutcome};
use vl_core::{Error, ErrorKind, Event, EventBus, Replier, Response, TaskId};
use vl_java::install::Installer;
use vl_java::select::{self, JavaRequirement, Resolution};
use vl_java::{FinderOptions, JavaFinder};
use vl_net::{Client, Control, NetConfig};

use vl_launch::Job;
use vl_tasks::TaskSpec;

use crate::Backend;

/// The Java runtimes of this machine: scanned on demand, remembered until the next rescan.
#[derive(Clone)]
pub(crate) struct JavaService {
    inner: Arc<Inner>,
}

struct Inner {
    finder: JavaFinder,
    events: Arc<EventBus>,
    /// The result of the last scan; held during a scan so scans never overlap.
    last: Mutex<Option<Vec<JavaRuntime>>>,
    next_task: AtomicU64,
    /// Where managed runtimes live (the Java section).
    managed_dir: PathBuf,
    /// The HTTP cache of runtime metadata.
    http_cache: PathBuf,
}

impl JavaService {
    pub(crate) fn new(
        managed_dir: PathBuf,
        data_dir: &Path,
        cache_dir: &Path,
        events: Arc<EventBus>,
    ) -> Self {
        let options = FinderOptions {
            managed_dir: Some(managed_dir.clone()),
            cache_file: Some(data_dir.join("java-cache.json")),
            ..FinderOptions::default()
        };
        Self {
            inner: Arc::new(Inner {
                finder: JavaFinder::new(options),
                events,
                last: Mutex::new(None),
                next_task: AtomicU64::new(1),
                managed_dir,
                http_cache: cache_dir.join("http"),
            }),
        }
    }

    /// The runtimes of the last scan; scans first when there was none.
    pub(crate) fn runtimes(&self) -> Vec<JavaRuntime> {
        self.inner.runtimes(false)
    }

    /// Checks one executable, for a Java the user chose by path.
    pub(crate) fn probe(&self, path: &Path) -> vl_core::Result<JavaRuntime> {
        self.inner.finder.probe(path).map_err(|e| {
            Error::new(
                ErrorKind::Java,
                format!("{} is not a working Java: {e}", path.display()),
            )
        })
    }

    /// The runtime to launch a game with: the user's explicit choice as it is (checked, never
    /// replaced), else the best installed one, else a managed runtime downloaded now.
    pub(crate) fn ensure(
        &self,
        requirement: &JavaRequirement,
        explicit: Option<&Path>,
        control: &Control,
    ) -> Result<JavaRuntime, Error> {
        if let Some(path) = explicit {
            return self
                .inner
                .finder
                .probe(path)
                .map_err(|e| Error::new(ErrorKind::Java, format!("{}: {e}", path.display())));
        }
        let list = self.inner.runtimes(false);
        match select::resolve(&list, requirement, Platform::current().arch, None) {
            Resolution::Use(runtime) => Ok(runtime),
            Resolution::Download(requirement) => self.install(&requirement, control),
        }
    }

    /// Installs a managed runtime (a no-op when one that fits is already managed) and adds it
    /// to the list.
    pub(crate) fn install(
        &self,
        requirement: &JavaRequirement,
        control: &Control,
    ) -> Result<JavaRuntime, Error> {
        let installed = self.inner.installer()?.install(requirement, control)?;
        self.inner.add_runtime(installed.runtime.clone());
        self.inner
            .events
            .publish(&Event::Java(JavaEvent::ListChanged));
        Ok(installed.runtime)
    }
}

impl Inner {
    fn installer(&self) -> Result<Installer, Error> {
        let config = NetConfig {
            cache_dir: Some(self.http_cache.clone()),
            ..NetConfig::default()
        };
        let client = Client::new(config)?;
        Ok(Installer::new(client, &self.managed_dir))
    }

    /// Puts a freshly installed runtime into the remembered list (when there is one).
    fn add_runtime(&self, runtime: JavaRuntime) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(list) = last.as_mut() {
            list.retain(|r| r.path != runtime.path);
            list.push(runtime);
            list.sort_by(|a, b| {
                b.major
                    .cmp(&a.major)
                    .then(b.managed.cmp(&a.managed))
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
    }

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
        JavaCommand::Install { major } => {
            if !(1..=99).contains(&major) {
                replier.send(Err(Error::invalid_input(format!("no Java {major}"))));
                return;
            }
            let service = backend.java.clone();
            let handle = backend.tasks().spawn(
                TaskSpec::new(TaskKind::JavaDownload).subject(&format!("Java {major}")),
                move |ctx| {
                    crate::launch::with_job(ctx, |job| {
                        let requirement = JavaRequirement::from_major(major);
                        service.install(&requirement, &job_control(job)).map(|_| ())
                    })
                },
            );
            replier.send(Ok(Response::Task(handle.id())));
        }
        _ => crate::not_yet(replier, "java", &command),
    }
}

/// A download control that follows the job's cancellation and reports its progress.
pub(crate) fn job_control(job: &Job) -> Control {
    let sink = job.clone();
    Control::with_cancel(job.cancel_token().clone()).on_progress(move |p| {
        sink.report("download", p.done, p.total, ProgressUnit::Bytes);
    })
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

    use crate::BackendConfig;

    fn backend(name: &str) -> (Backend, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("vl-backend-java-{name}-{}", std::process::id()));
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        (backend, dir)
    }

    #[test]
    fn lists_runtimes_without_failing() {
        let (backend, dir) = backend("list");
        let reply = backend.call(Command::Java(JavaCommand::List)).wait();
        assert!(matches!(reply, Ok(Response::Javas(_))));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_an_impossible_major_version() {
        let (backend, dir) = backend("major");
        let reply = backend
            .call(Command::Java(JavaCommand::Install { major: 0 }))
            .wait();
        assert_eq!(reply.unwrap_err().kind, ErrorKind::InvalidInput);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_explicit_runtime_that_is_not_a_java_is_an_error_not_a_fallback() {
        let (backend, dir) = backend("explicit");
        let missing = dir.join("nope").join("java");
        let err = backend
            .java
            .ensure(
                &JavaRequirement::legacy(),
                Some(&missing),
                &Control::default(),
            )
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Java);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
