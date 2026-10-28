//! Launch commands, served by `vl-launch`.
//!
//! `Launch` runs a `Launch` task that installs the version, takes the session from the account,
//! chooses Java, builds the command line and starts the process; the task ends once the game is
//! running. The instance then moves `Preparing → Running → Stopped` and the process end is
//! published as `LaunchEvent::Exited`. `Stop` cancels a preparing launch or kills a running game.
//!
//! A loader that needs an installation step (NeoForge and Forge: their installer's processors
//! patch the game jar with Java) gets it after the vanilla game files are installed and before
//! the loader's libraries are, with the Java the game would use.
//!
//! The same module runs `VersionCommand::Install` (see `versions.rs`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use vl_core::component::{ComponentKind, ComponentSet};
use vl_core::instance::{InstanceState, LoaderRequest};
use vl_core::java::JavaRuntime;
use vl_core::launch::{LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::log::{LogBatch, LogLevel, LogLine, LogSource};
use vl_core::settings::Settings;
use vl_core::task::{Progress, ProgressUnit, TaskKind};
use vl_core::{Error, ErrorKind, Event, EventBus, InstanceId, Replier, Response};
use vl_java::select::JavaRequirement;
use vl_launch::process::{self, GameBatch, RunningGame, SpawnOptions};
use vl_launch::{Installer, InstallerOptions, Job, Layout, PrepareInput};
use vl_loaders::installer::{InstallContext, Installation};
use vl_loaders::processors::JavaRunner;
use vl_loaders::{Loaders, LoadersOptions};
use vl_meta::loader::LoaderProfile;
use vl_net::{CancelToken, Client, Control, NetConfig};
use vl_store::Store;
use vl_tasks::{TaskContext, TaskHandle, TaskSpec};

use crate::Backend;

const LAUNCHER_NAME: &str = "VibeLauncher";
const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the backend keeps for an instance that is not stopped.
enum Active {
    /// The launch task is running.
    Preparing(TaskHandle),
    /// The game process runs.
    Running(RunningGame),
}

/// Launching and installing.
#[derive(Clone)]
pub(crate) struct Service {
    installer: Arc<Installer>,
    loaders: Arc<Loaders>,
    active: Arc<Mutex<HashMap<InstanceId, Active>>>,
    events: Arc<EventBus>,
}

impl Service {
    pub(crate) fn new(store: &Store, events: Arc<EventBus>) -> vl_core::Result<Self> {
        let mut config = NetConfig::new(LAUNCHER_VERSION);
        config.cache_dir = Some(store.cache_dir().join("http"));
        let client = Client::new(config)?;
        let loaders = Loaders::new(
            client.clone(),
            store.data_dir().join("loaders"),
            LoadersOptions::default(),
        );
        let installer = Installer::new(
            client,
            Layout::new(store.data_dir()),
            InstallerOptions::default(),
        );
        Ok(Self {
            installer: Arc::new(installer),
            loaders: Arc::new(loaders),
            active: Arc::default(),
            events,
        })
    }

    /// The mod loader sources.
    pub(crate) fn loaders(&self) -> &Loaders {
        &self.loaders
    }

    /// Whether the instance is preparing or running.
    pub(crate) fn is_active(&self, instance: &InstanceId) -> bool {
        self.active().contains_key(instance)
    }

    fn active(&self) -> MutexGuard<'_, HashMap<InstanceId, Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self, instance: &InstanceId, state: InstanceState) {
        self.events
            .publish(&Event::Launch(LaunchEvent::StateChanged {
                instance: instance.clone(),
                state,
            }));
    }

    fn log(&self, source: LogSource, level: LogLevel, text: String) {
        self.events.publish(&Event::Log(LogLine {
            source,
            level,
            text,
        }));
    }
}

pub(crate) fn handle(backend: &Backend, command: LaunchCommand, replier: Replier) {
    replier.send(match command {
        LaunchCommand::Launch(request) => launch(backend, request),
        LaunchCommand::Stop { instance } => stop(&backend.launch, &instance),
        other => Err(Error::unsupported(format!(
            "vl-backend does not handle launch {other:?}"
        ))),
    });
}

fn stop(service: &Service, instance: &InstanceId) -> vl_core::Result<Response> {
    match service.active().get(instance) {
        Some(Active::Preparing(task)) => task.cancel(),
        Some(Active::Running(game)) => game.stop(),
        None => {
            return Err(Error::not_found(format!(
                "instance {instance} has no running game"
            )));
        }
    }
    Ok(Response::Done)
}

/// Everything a launch task needs, gathered before the task starts.
struct Plan {
    request: LaunchRequest,
    components: ComponentSet,
    game_dir: std::path::PathBuf,
    log_dir: std::path::PathBuf,
    settings: Settings,
    accounts: crate::accounts::Service,
    java: crate::java::JavaService,
    instances: vl_instance::Instances,
}

fn launch(backend: &Backend, request: LaunchRequest) -> vl_core::Result<Response> {
    let service = backend.launch.clone();
    let id = request.instance.clone();
    let config = backend.instances.load(&id)?;
    let components =
        ComponentSet::from_pairs(config.components.iter().map(|c| (&*c.uid, &*c.version)))
            .map_err(|e| Error::new(e.kind, format!("instance {id}: {}", e.message)))?;
    let plan = Plan {
        game_dir: backend.instances.game_dir(&id)?,
        log_dir: backend.instances.dir(&id)?.join("logs"),
        settings: backend.settings().effective(&id).value,
        accounts: backend.accounts.clone(),
        java: backend.java.clone(),
        instances: backend.instances.clone(),
        components,
        request,
    };

    let mut active = service.active();
    if active.contains_key(&id) {
        return Err(Error::conflict(format!("instance {id} is already running")));
    }
    // The task waits for this signal, so `Preparing` is published before anything it causes.
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let worker = service.clone();
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::Launch).subject(&config.name),
        move |ctx| {
            let _ = go_rx.recv();
            let id = plan.request.instance.clone();
            let result = run_launch(&worker, ctx, &plan);
            if result.is_err() {
                worker.active().remove(&id);
                worker.state(&id, InstanceState::Stopped);
            }
            result
        },
    );
    let task = handle.id();
    active.insert(id.clone(), Active::Preparing(handle));
    drop(active);
    service.state(&id, InstanceState::Preparing { task });
    let _ = go_tx.send(());
    Ok(Response::Task(task))
}

fn run_launch(service: &Service, ctx: &TaskContext, plan: &Plan) -> vl_core::Result<()> {
    let id = &plan.request.instance;
    let prepared = with_job(ctx, |job| {
        ctx.set_stage("account");
        let session = plan
            .accounts
            .launch_session(plan.request.account.as_ref(), plan.request.offline)?;
        job.check()?;
        if plan.settings.java.path.is_none() {
            ensure_java(
                &service.installer,
                &plan.java,
                plan.components.game_version(),
                Some(&plan.game_dir),
                job,
            )?;
        }
        let runtimes = plan.java.runtimes();
        let fixed = match &plan.settings.java.path {
            Some(path) => Some(plan.java.probe(path)?),
            None => None,
        };
        let loader = match plan.components.loader() {
            Some(l) => {
                ctx.set_stage("loader");
                let control = Control::with_cancel(job.cancel_token().clone());
                Some(service.loaders.profile(
                    l.kind,
                    plan.components.game_version(),
                    &l.version,
                    &control,
                )?)
            }
            None => None,
        };
        if let (Some(kind), Some(profile)) = (plan.components.loader().map(|l| l.kind), &loader)
            && Loaders::needs_install(kind)
        {
            let done = install_loader(
                service,
                kind,
                profile,
                Some(&plan.game_dir),
                fixed.as_ref(),
                &runtimes,
                job,
            )?;
            if let Some(done) = done {
                service.log(LogSource::Launcher, LogLevel::Info, describe(&done));
            }
        }
        job.check()?;
        let input = PrepareInput {
            version: plan.components.game_version(),
            loader: loader.as_ref(),
            game_dir: &plan.game_dir,
            session: &session,
            settings: &plan.settings,
            runtimes: &runtimes,
            fixed_java: fixed.as_ref(),
            launcher_name: LAUNCHER_NAME,
            launcher_version: LAUNCHER_VERSION,
        };
        vl_launch::prepare(&service.installer, &input, job)
    })?;
    ctx.check()?;

    for warning in &prepared.warnings {
        service.log(LogSource::Launcher, LogLevel::Warn, warning.clone());
    }
    service.log(
        LogSource::Launcher,
        LogLevel::Info,
        prepared.command.display_redacted(),
    );

    let started = Instant::now();
    let line_service = service.clone();
    let line_source = LogSource::Game(id.clone());
    let exit_service = service.clone();
    let instances = plan.instances.clone();
    let exit_id = id.clone();
    let mut options = SpawnOptions::new(plan.settings.console.max_lines as usize);
    options.log_dir = Some(plan.log_dir.clone());
    let game = process::spawn(
        &prepared.command,
        &options,
        move |batch: GameBatch| {
            line_service.events.publish(&Event::LogBatch(LogBatch {
                source: line_source.clone(),
                lines: batch.lines,
                skipped: batch.skipped,
            }));
        },
        move |exit| {
            exit_service.active().remove(&exit_id);
            if let Err(e) = instances.record_play(&exit_id, started.elapsed().as_secs()) {
                exit_service.log(
                    LogSource::Launcher,
                    LogLevel::Warn,
                    format!("cannot record the play time: {}", e.message),
                );
            }
            exit_service
                .events
                .publish(&Event::Launch(LaunchEvent::Exited {
                    instance: exit_id.clone(),
                    exit,
                }));
            exit_service.state(&exit_id, InstanceState::Stopped);
        },
    )?;
    let pid = game.pid();
    if let Some(file) = game.log_file() {
        service.log(
            LogSource::Launcher,
            LogLevel::Info,
            format!("game log: {}", file.display()),
        );
    }
    service.active().insert(id.clone(), Active::Running(game));
    service.state(id, InstanceState::Running { pid: Some(pid) });
    Ok(())
}

/// Downloads a managed Java when no installed one runs the version, so the launch never stops
/// at "no Java found". A Java fixed in the settings is never replaced.
fn ensure_java(
    installer: &Installer,
    java: &crate::java::JavaService,
    game: &str,
    game_dir: Option<&std::path::Path>,
    job: &Job,
) -> vl_core::Result<()> {
    let installed = installer.install(game, game_dir, job)?;
    let wanted = installed.version.java_version_or_default();
    let arch = installed.platform.arch;
    if vl_launch::java::choose(&java.runtimes(), wanted.major_version, None, arch).is_ok() {
        return Ok(());
    }
    job.report("java", 0, None, ProgressUnit::Bytes);
    let requirement = JavaRequirement::new(wanted.component, wanted.major_version);
    java.ensure(&requirement, None, &crate::java::job_control(job))
        .map(|_| ())
}

/// The installation step of a loader that needs one (see [`Loaders::needs_install`]): the vanilla
/// game files are installed first, then the loader's processors run on the Java the game would
/// use (`fixed`, else the best of `runtimes`). The loader's own libraries are installed after
/// this step (some of them are its output).
fn install_loader(
    service: &Service,
    kind: ComponentKind,
    profile: &LoaderProfile,
    game_dir: Option<&std::path::Path>,
    fixed: Option<&JavaRuntime>,
    runtimes: &[JavaRuntime],
    job: &Job,
) -> vl_core::Result<Option<Installation>> {
    let installed = service
        .installer
        .install_with(&profile.inherits_from, None, game_dir, job)?;
    job.check()?;
    let required = installed.version.java_version_or_default().major_version;
    let arch = installed.platform.arch;
    let java = match fixed {
        Some(runtime) => vl_launch::java::check_fixed(runtime, required, arch)?,
        None => vl_launch::java::choose(runtimes, required, None, arch)?,
    };
    let progress_job = job.clone();
    let control = Control::with_cancel(job.cancel_token().clone()).on_progress(move |p| {
        progress_job.report("loader", p.done, p.total, ProgressUnit::Bytes);
    });
    let on_step = |i: usize, n: usize| {
        job.report("processors", i as u64, Some(n as u64), ProgressUnit::Steps);
    };
    let layout = service.installer.layout();
    let libraries_dir = layout.libraries_dir();
    let ctx = InstallContext {
        java: &java.runtime.path,
        minecraft_jar: &installed.client_jar,
        root: layout.root(),
        libraries_dir: &libraries_dir,
        runner: &JavaRunner::default(),
        control: &control,
        on_step: &on_step,
    };
    service
        .loaders
        .install(kind, &profile.inherits_from, &profile.loader_version, &ctx)
}

fn describe(done: &Installation) -> String {
    if done.reused {
        "loader files are installed".to_owned()
    } else {
        format!(
            "loader processors: {} ran, {} skipped (log: {})",
            done.ran,
            done.skipped,
            done.log_file.display()
        )
    }
}

/// Runs `work` with a [`Job`] wired to the task: the task's cancellation stops the job's
/// downloads, and the job's progress becomes the task's progress.
pub(crate) fn with_job<T>(
    ctx: &TaskContext,
    work: impl FnOnce(&Job) -> vl_core::Result<T>,
) -> vl_core::Result<T> {
    let net_cancel = CancelToken::new();
    let latest: Arc<Mutex<Option<Progress>>> = Arc::default();
    let sink = Arc::clone(&latest);
    let job = Job::with_cancel(net_cancel.clone()).on_progress(move |p| {
        *sink.lock().unwrap_or_else(PoisonError::into_inner) = Some(p);
    });
    let finished = AtomicBool::new(false);
    thread::scope(|scope| {
        let bridge = || {
            loop {
                if ctx.is_cancelled() {
                    net_cancel.cancel();
                }
                let progress = latest.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(p) = progress {
                    if let Some(stage) = &p.stage {
                        ctx.set_stage(stage);
                    }
                    ctx.report(p.done, p.total, p.unit);
                }
                if finished.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        };
        // Without the bridge the work still runs; only cancelling and progress are lost.
        let watcher = thread::Builder::new()
            .name("vl-task-bridge".into())
            .spawn_scoped(scope, bridge);
        let result = work(&job);
        finished.store(true, Ordering::SeqCst);
        drop(watcher);
        result
    })
}

/// `VersionCommand::Install`: a task that installs `version` (and the profile and libraries of
/// `loader`, when given) without starting it.
pub(crate) fn install(
    backend: &Backend,
    version: &str,
    loader: Option<LoaderRequest>,
) -> vl_core::Result<Response> {
    if !vl_launch::layout::is_safe_component(version) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("invalid version id {version:?}"),
        ));
    }
    if loader.as_ref().is_some_and(|l| !l.kind.is_loader()) {
        return Err(Error::invalid_input("the game is not a loader"));
    }
    let service = backend.launch.clone();
    let java = backend.java.clone();
    let version = version.to_owned();
    let subject = match &loader {
        Some(l) => format!("{version} + {}", l.kind.display_name()),
        None => version.clone(),
    };
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::InstallVersion).subject(&subject),
        move |ctx| {
            with_job(ctx, |job| {
                let loaders = &service.loaders;
                let profile = match &loader {
                    Some(l) => {
                        let control = Control::with_cancel(job.cancel_token().clone());
                        let chosen =
                            loaders.pick(l.kind, &version, l.version.as_deref(), &control)?;
                        Some(loaders.profile(l.kind, &version, &chosen.version, &control)?)
                    }
                    None => None,
                };
                if let Some(profile) = profile.as_ref().filter(|p| Loaders::needs_install(p.kind)) {
                    ensure_java(&service.installer, &java, &version, None, job)?;
                    let runtimes = java.runtimes();
                    if let Some(done) =
                        install_loader(&service, profile.kind, profile, None, None, &runtimes, job)?
                    {
                        service.log(LogSource::Launcher, LogLevel::Info, describe(&done));
                    }
                }
                service
                    .installer
                    .install_with(&version, profile.as_ref(), None, job)
                    .map(|_| ())
            })
        },
    );
    Ok(Response::Task(handle.id()))
}
