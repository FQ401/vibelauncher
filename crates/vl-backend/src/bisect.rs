//! Bisection commands, served by `vl-resources`.
//!
//! The session lives in `<instance>/.vibelauncher/bisect.json`. Every command touches files, so
//! each runs on its own thread. `Auto` runs the search as a task (see `auto_bisect`); its last
//! report is kept in `<instance>/.vibelauncher/bisect-report.json`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use vl_core::bisect::{AutoBisectReport, AutoBisectRequest, BisectCommand, BisectEvent, BisectRun};
use vl_core::component::ComponentSet;
use vl_core::launch::{LaunchEvent, LaunchRequest};
use vl_core::log::LogSource;
use vl_core::task::{ProgressUnit, TaskEvent, TaskKind, TaskOutcome};
use vl_core::{Error, ErrorKind, Event, EventBus, EventStream, InstanceId, Replier, Response};
use vl_resources::Bisect;
use vl_tasks::{TaskContext, TaskSpec};

use crate::Backend;
use crate::auto_bisect::{self, GameProcess, GameSignal, GameStarter, Hooks, Setup};
use crate::launch::Launcher;

const META_DIR: &str = ".vibelauncher";
const REPORT_FILE: &str = "bisect-report.json";

/// The instances with an automatic search running.
#[derive(Clone, Default)]
pub(crate) struct Running(Arc<Mutex<HashSet<InstanceId>>>);

impl Running {
    fn contains(&self, id: &InstanceId) -> bool {
        self.lock().contains(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<InstanceId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) fn handle(backend: &Backend, command: BisectCommand, replier: Replier) {
    let instance = match &command {
        BisectCommand::Start { instance, .. }
        | BisectCommand::Status { instance }
        | BisectCommand::Report { instance, .. }
        | BisectCommand::Stop { instance }
        | BisectCommand::AutoReport { instance } => instance.clone(),
        BisectCommand::Auto(request) => {
            let request = request.clone();
            return replier.send(auto(backend, request));
        }
        other => return crate::not_yet(replier, "bisect", other),
    };
    let busy = !matches!(
        command,
        BisectCommand::Status { .. } | BisectCommand::AutoReport { .. }
    ) && backend.bisect.contains(&instance);
    if busy {
        return replier.send(Err(Error::conflict(format!(
            "an automatic search runs on instance {instance}; cancel its task first"
        ))));
    }
    let opened = crate::resources::open(backend, &instance).and_then(|resources| {
        let dir = backend.instances.dir(&instance)?;
        let bisect = Bisect::new(resources, &dir.join(META_DIR));
        Ok((bisect, dir, backend.instances.game_dir(&instance)?))
    });
    let (bisect, dir, game_dir) = match opened {
        Ok(b) => b,
        Err(e) => return replier.send(Err(e)),
    };
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name("bisect".into())
        .spawn(move || replier.send(run(&bisect, &dir, &game_dir, command)));
}

fn run(
    bisect: &Bisect,
    dir: &Path,
    game_dir: &Path,
    command: BisectCommand,
) -> vl_core::Result<Response> {
    match command {
        BisectCommand::Start { suspects, .. } => bisect.start(&suspects).map(Response::Bisect),
        BisectCommand::Status { .. } => bisect.status().map(Response::Bisect),
        BisectCommand::Report { outcome, .. } => bisect.report(outcome).map(Response::Bisect),
        BisectCommand::Stop { .. } => {
            let mods = bisect.stop();
            let world = auto_bisect::restore_leftover_world(game_dir, dir);
            mods.and(world).map(|()| Response::Done)
        }
        BisectCommand::AutoReport { .. } => read_report(&report_file(dir)),
        other => Err(Error::unsupported(format!("{other:?}"))),
    }
}

fn report_file(instance_dir: &Path) -> PathBuf {
    instance_dir.join(META_DIR).join(REPORT_FILE)
}

fn io_err(path: &Path, e: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

fn read_report(path: &Path) -> vl_core::Result<Response> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::not_found("automatic bisect report"));
        }
        Err(e) => return Err(io_err(path, &e)),
    };
    serde_json::from_slice::<AutoBisectReport>(&bytes)
        .map(Response::BisectReport)
        .map_err(|e| Error::new(ErrorKind::Integrity, format!("{}: {e}", path.display())))
}

fn write_report(path: &Path, report: &AutoBisectReport) -> vl_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|e| Error::internal(e.to_string()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io_err(dir, &e))?;
    }
    fs::write(path, bytes).map_err(|e| io_err(path, &e))
}

/// `BisectCommand::Auto`: checks the request and starts the `Bisect` task.
fn auto(backend: &Backend, request: AutoBisectRequest) -> vl_core::Result<Response> {
    let id = request.instance.clone();
    let config = backend.instances.load(&id)?;
    let components =
        ComponentSet::from_pairs(config.components.iter().map(|c| (&*c.uid, &*c.version)))
            .map_err(|e| Error::new(e.kind, format!("instance {id}: {}", e.message)))?;
    if backend.launch.is_active(&id) {
        return Err(Error::conflict(format!(
            "instance {id} is running; close the game first"
        )));
    }
    let instance_dir = backend.instances.dir(&id)?;
    let setup = Setup {
        game_dir: backend.instances.game_dir(&id)?,
        instance_dir,
        game_version: components.game_version().to_owned(),
        target: request.target,
        world: request.world.clone(),
        stable: Duration::from_secs(request.stable_secs.into()),
        repeats: request.repeats,
        timeout: Duration::from_secs(request.timeout_secs.max(1).into()),
        after_crash: request.after_crash,
    };
    let running = backend.bisect.clone();
    if !running.lock().insert(id.clone()) {
        return Err(Error::conflict(format!(
            "an automatic search already runs on instance {id}"
        )));
    }
    let mut starter = BackendStarter {
        launcher: Launcher::new(backend),
        events: Arc::clone(&backend.events),
        request: LaunchRequest {
            instance: id.clone(),
            account: request.account.clone(),
            offline: request.offline,
        },
    };
    let events = Arc::clone(&backend.events);
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::Bisect).subject(&config.name),
        move |ctx| {
            let mut hooks = TaskHooks {
                ctx,
                events: &events,
                instance: id.clone(),
            };
            let result = auto_bisect::run(&setup, &mut starter, &mut hooks);
            running.lock().remove(&id);
            let report = result?;
            write_report(&report_file(&setup.instance_dir), &report)?;
            events.publish(&Event::Bisect(BisectEvent::Finished {
                instance: id.clone(),
                report: Box::new(report),
            }));
            Ok(())
        },
    );
    Ok(Response::Task(handle.id()))
}

struct TaskHooks<'a> {
    ctx: &'a TaskContext,
    events: &'a EventBus,
    instance: InstanceId,
}

impl Hooks for TaskHooks<'_> {
    fn cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }

    fn progress(&mut self, stage: &str, done: u32, total: u32) {
        self.ctx.set_stage(stage);
        self.ctx.report(
            done.into(),
            Some(total.max(done).into()),
            ProgressUnit::Items,
        );
    }

    fn run(&mut self, run: &BisectRun, steps_done: u32, steps_estimate: u32) {
        self.events.publish(&Event::Bisect(BisectEvent::Run {
            instance: self.instance.clone(),
            run: run.clone(),
            steps_done,
            steps_estimate,
        }));
    }
}

/// Starts the game through the backend's launch, with the instance's account and settings.
struct BackendStarter {
    launcher: Launcher,
    events: Arc<EventBus>,
    request: LaunchRequest,
}

impl GameStarter for BackendStarter {
    fn start(&mut self, extra_args: &[String]) -> vl_core::Result<Box<dyn GameProcess>> {
        // Subscribe first: no event of this launch may be missed.
        let stream = self.events.subscribe();
        let task = self
            .launcher
            .launch(self.request.clone(), extra_args.to_vec())?;
        Ok(Box::new(BackendGame {
            stream,
            task,
            instance: self.request.instance.clone(),
            launcher: self.launcher.clone(),
        }))
    }
}

struct BackendGame {
    stream: EventStream,
    task: vl_core::TaskId,
    instance: InstanceId,
    launcher: Launcher,
}

impl GameProcess for BackendGame {
    fn next(&mut self, timeout: Duration) -> Option<GameSignal> {
        let deadline = Instant::now() + timeout;
        let source = LogSource::Game(self.instance.clone());
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.stream.next_timeout(left)? {
                Event::LogBatch(batch) if batch.source == source => {
                    let lines = batch.lines.into_iter().map(|l| l.text).collect();
                    return Some(GameSignal::Lines(lines));
                }
                Event::Launch(LaunchEvent::Exited { instance, exit })
                    if instance == self.instance =>
                {
                    return Some(GameSignal::Exited(exit));
                }
                Event::Task(TaskEvent::Finished { id, outcome }) if id == self.task => {
                    return Some(match outcome {
                        TaskOutcome::Succeeded => GameSignal::Started,
                        TaskOutcome::Failed(e) => GameSignal::Failed(e),
                        TaskOutcome::Cancelled => GameSignal::Failed(Error::new(
                            ErrorKind::Cancelled,
                            "the launch was cancelled",
                        )),
                    });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    fn stop(&mut self) {
        // The game may have ended already; its exit arrives either way.
        let _ = self.launcher.stop(&self.instance);
    }
}

#[cfg(test)]
mod tests {
    use vl_core::bisect::BisectOutcome;
    use vl_core::instance::{InstanceCommand, NewInstance};
    use vl_core::{Command, ErrorKind, Facade};

    use super::*;
    use crate::BackendConfig;

    #[test]
    fn runs_a_session_through_the_facade() {
        let dir = std::env::temp_dir().join(format!("vl-backend-bisect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let call = |c: BisectCommand| backend.call(Command::Bisect(c)).wait();
        let created = backend
            .call(Command::Instances(InstanceCommand::Create(NewInstance {
                name: "Modded".into(),
                game_version: "1.20.1".into(),
                group: None,
                loader: None,
            })))
            .wait()
            .unwrap();
        let Response::Instance(summary) = created else {
            panic!("unexpected {created:?}");
        };
        let id = summary.id;
        let mods = backend.instances.game_dir(&id).unwrap().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        for name in ["a.jar", "b.jar", "c.jar", "d.jar"] {
            std::fs::write(mods.join(name), b"not really a jar").unwrap();
        }
        let status = || BisectCommand::Status {
            instance: id.clone(),
        };
        assert_eq!(call(status()).unwrap_err().kind, ErrorKind::NotFound);
        let started = call(BisectCommand::Start {
            instance: id.clone(),
            suspects: Vec::new(),
        })
        .unwrap();
        assert!(matches!(started, Response::Bisect(s) if s.candidates == 4));
        let reported = call(BisectCommand::Report {
            instance: id.clone(),
            outcome: BisectOutcome::Good,
        })
        .unwrap();
        assert!(matches!(reported, Response::Bisect(s) if s.verdict.is_some()));
        let stop = BisectCommand::Stop {
            instance: id.clone(),
        };
        assert_eq!(call(stop).unwrap(), Response::Done);
        assert_eq!(call(status()).unwrap_err().kind, ErrorKind::NotFound);
        let missing = BisectCommand::Status {
            instance: vl_core::InstanceId::new("nope").unwrap(),
        };
        assert_eq!(call(missing).unwrap_err().kind, ErrorKind::NotFound);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
