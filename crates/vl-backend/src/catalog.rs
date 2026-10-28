//! Catalog commands, served by `vl-platforms` and `vl-install`.
//!
//! Every command goes to the network, so each is answered from a worker thread, never from the
//! caller's. Answers come from the HTTP cache while young and when offline.
//!
//! `Install` runs as an `InstallResource` task: it works out the plan, downloads the files and
//! adds them to the instance's folder. The origin of each file is kept in
//! `<instance>/.vibelauncher/resources.json`.

use std::path::Path;
use std::sync::Arc;
use std::thread;

use vl_core::catalog::CatalogCommand;
use vl_core::install::InstallRequest;
use vl_core::instance::Loader;
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::task::TaskKind;
use vl_core::{Error, Event, Replier, Response, Result};
use vl_install::{ApplyInput, OriginStore, PlanInput, Target};
use vl_net::{Client, Control, NetConfig};
use vl_platforms::Platforms;
use vl_tasks::TaskSpec;

use crate::Backend;

/// Folder of the launcher's own files inside an instance.
const META_DIR: &str = ".vibelauncher";

/// The platforms behind the catalog commands.
pub(crate) struct Service {
    platforms: Arc<Platforms>,
    client: Client,
}

impl Service {
    /// Builds the network client with its HTTP cache under `cache_dir`.
    pub(crate) fn new(cache_dir: &Path) -> Result<Self> {
        let config = NetConfig {
            cache_dir: Some(cache_dir.join("http")),
            ..NetConfig::new(env!("CARGO_PKG_VERSION"))
        };
        let client = Client::new(config)?;
        Ok(Self {
            platforms: Arc::new(Platforms::new(client.clone())),
            client,
        })
    }
}

pub(crate) fn handle(backend: &Backend, command: CatalogCommand, replier: Replier) {
    match command {
        CatalogCommand::PlanInstall(request) => return plan_install(backend, request, replier),
        CatalogCommand::Install(request) => return replier.send(install(backend, request)),
        _ => {}
    }
    let platforms = Arc::clone(&backend.catalog.platforms);
    let spawned = thread::Builder::new()
        .name("catalog".into())
        .spawn(move || replier.send(run(&platforms, command)));
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _: std::result::Result<_, Error> = spawned.map_err(|e| Error::internal(e.to_string()));
}

/// What an install into an instance works on, gathered before the work starts.
struct Job {
    platforms: Arc<Platforms>,
    client: Client,
    resources: vl_resources::Resources,
    origins: OriginStore,
    staging: std::path::PathBuf,
    target: Target,
    request: InstallRequest,
}

fn job(backend: &Backend, request: InstallRequest) -> Result<(Job, String)> {
    let id = &request.instance;
    let config = backend.instances.load(id)?;
    if backend.launch.is_active(id) {
        return Err(Error::conflict(format!("instance {id} is running")));
    }
    let game_version = config
        .game_version()
        .ok_or_else(|| Error::invalid_input(format!("instance {id} has no game version")))?
        .to_owned();
    let loader = match config.loader() {
        Loader::Fabric(_) => Some("fabric"),
        Loader::Quilt(_) => Some("quilt"),
        Loader::NeoForge(_) => Some("neoforge"),
        Loader::Forge(_) => Some("forge"),
        _ => None,
    };
    let meta = backend.instances.dir(id)?.join(META_DIR);
    let job = Job {
        platforms: Arc::clone(&backend.catalog.platforms),
        client: backend.catalog.client.clone(),
        resources: crate::resources::open(backend, id)?,
        origins: OriginStore::new(meta.join("resources.json")),
        staging: meta.join("staging"),
        target: Target {
            game_version,
            loader: loader.map(str::to_owned),
        },
        request,
    };
    Ok((job, config.name))
}

fn make_plan(job: &Job, control: &Control) -> Result<vl_core::install::InstallPlan> {
    let source = job.platforms.source(job.request.provider)?;
    let input = PlanInput {
        source,
        resources: &job.resources,
        origins: &job.origins,
        target: &job.target,
        control,
    };
    vl_install::plan(
        &input,
        &job.request.project,
        job.request.version.as_deref(),
        job.request.min_stability,
    )
}

fn plan_install(backend: &Backend, request: InstallRequest, replier: Replier) {
    let job = match job(backend, request) {
        Ok((job, _)) => job,
        Err(e) => return replier.send(Err(e)),
    };
    let spawned = thread::Builder::new()
        .name("catalog-plan".into())
        .spawn(move || {
            replier.send(make_plan(&job, &Control::default()).map(Response::InstallPlan));
        });
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _: std::result::Result<_, Error> = spawned.map_err(|e| Error::internal(e.to_string()));
}

fn install(backend: &Backend, request: InstallRequest) -> Result<Response> {
    let (job, name) = job(backend, request)?;
    let events = Arc::clone(&backend.events);
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::InstallResource).subject(&name),
        move |ctx| {
            crate::launch::with_job(ctx, |wired| {
                let control = crate::java::job_control(wired);
                let plan = make_plan(&job, &control)?;
                wired.check()?;
                let input = ApplyInput {
                    client: &job.client,
                    resources: &job.resources,
                    origins: &job.origins,
                    staging: &job.staging,
                };
                let report = vl_install::execute(&input, &plan, &control)?;
                for text in plan.warnings.iter().chain(&report.warnings) {
                    events.publish(&Event::Log(LogLine {
                        source: LogSource::Launcher,
                        level: LogLevel::Warn,
                        text: text.clone(),
                    }));
                }
                Ok(())
            })
        },
    );
    Ok(Response::Task(handle.id()))
}

fn run(platforms: &Platforms, command: CatalogCommand) -> Result<Response> {
    let control = Control::default();
    match command {
        CatalogCommand::Search(query) => {
            let source = platforms.source(query.provider)?;
            Ok(Response::CatalogSearch(source.search(&query, &control)?))
        }
        CatalogCommand::Project { id } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogProject(source.project(&id, &control)?))
        }
        CatalogCommand::Versions { project, filter } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogVersions(
                source.versions(&project, &filter, &control)?,
            ))
        }
        CatalogCommand::VersionsByHashes { algorithm, hashes } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogHashMatches(
                source.versions_by_hashes(algorithm, &hashes, &control)?,
            ))
        }
        other => Err(Error::unsupported(format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use vl_core::install::InstallRequest;
    use vl_core::{Command, ErrorKind, Facade, InstanceId};

    use super::*;
    use crate::BackendConfig;

    #[test]
    fn install_commands_check_the_instance_first() {
        let dir = std::env::temp_dir().join(format!("vl-backend-cat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let request = InstallRequest {
            instance: InstanceId::new("nope").unwrap(),
            provider: Default::default(),
            project: "sodium".into(),
            version: None,
            min_stability: Default::default(),
        };
        for command in [
            CatalogCommand::PlanInstall(request.clone()),
            CatalogCommand::Install(request),
        ] {
            let err = backend.call(Command::Catalog(command)).wait().unwrap_err();
            assert_eq!(err.kind, ErrorKind::NotFound);
        }
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
