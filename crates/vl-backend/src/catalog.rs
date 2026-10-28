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

use vl_core::catalog::{CatalogCommand, Provider};
use vl_core::component::ComponentKind;
use vl_core::install::{InstallRequest, PackInstallRequest, PackSource};
use vl_core::instance::{InstanceEvent, InstanceSummary, Loader, LoaderRequest, NewInstance};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::task::TaskKind;
use vl_core::{Error, Event, Replier, Response, Result};
use vl_install::{ApplyInput, OriginStore, PlanInput, Target};
use vl_modrinth::mrpack::PackLoader;
use vl_net::{Client, Control, NetConfig};
use vl_platforms::Platforms;
use vl_tasks::TaskSpec;

use crate::Backend;
use crate::pack_update;
use crate::updates;

/// Folder of the launcher's own files inside an instance.
const META_DIR: &str = ".vibelauncher";

/// The platforms behind the catalog commands.
pub(crate) struct Service {
    pub(crate) platforms: Arc<Platforms>,
    pub(crate) client: Client,
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
        CatalogCommand::InstallPack(request) => {
            return replier.send(install_pack(backend, request));
        }
        CatalogCommand::CheckPackUpdate(request) => {
            return pack_update::check(backend, request, replier);
        }
        CatalogCommand::PlanPackUpdate(request) => {
            return pack_update::plan(backend, request, replier);
        }
        CatalogCommand::UpdatePack(request) => {
            return replier.send(pack_update::update(backend, request));
        }
        CatalogCommand::CheckUpdates(request) => {
            return updates::check(backend, request, replier);
        }
        CatalogCommand::PlanUpdate(request) => return updates::plan(backend, request, replier),
        CatalogCommand::Update(request) => return replier.send(updates::update(backend, request)),
        CatalogCommand::IdentifyMods {
            instance,
            provider,
            world,
        } => {
            return updates::identify(backend, &instance, provider, world.as_deref(), replier);
        }
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
pub(crate) struct Job {
    pub(crate) platforms: Arc<Platforms>,
    pub(crate) client: Client,
    pub(crate) resources: vl_resources::Resources,
    pub(crate) origins: OriginStore,
    pub(crate) staging: std::path::PathBuf,
    pub(crate) target: Target,
    pub(crate) meta: std::path::PathBuf,
    pub(crate) request: InstallRequest,
}

fn job(backend: &Backend, request: InstallRequest) -> Result<(Job, String)> {
    let id = request.instance.clone();
    let (mut job, name) = context(backend, &id, request.world.as_deref())?;
    job.request = request;
    Ok((job, name))
}

/// What work on the instance `id` needs; `request` is filled in by the caller that has one.
///
/// With a `world` the work is on that world's data packs: the resources are the world's, and so
/// are the origin records (`saves/<world>/.vibelauncher/resources.json`), so they stay with the
/// world when it is copied or backed up.
pub(crate) fn context(
    backend: &Backend,
    id: &vl_core::InstanceId,
    world: Option<&str>,
) -> Result<(Job, String)> {
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
    let mut resources = crate::resources::open(backend, id)?;
    let staging = meta.join("staging");
    let mut records = meta;
    if let Some(world) = world {
        resources = resources.world(world)?;
        records = resources.root().join(META_DIR);
    }
    let job = Job {
        platforms: Arc::clone(&backend.catalog.platforms),
        client: backend.catalog.client.clone(),
        origins: OriginStore::new(records.join("resources.json")),
        resources,
        staging,
        meta: records,
        target: Target {
            game_version,
            loader: loader.map(str::to_owned),
            world: world.is_some(),
        },
        request: InstallRequest {
            instance: id.clone(),
            provider: Default::default(),
            project: String::new(),
            version: None,
            min_stability: Default::default(),
            world: world.map(str::to_owned),
        },
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

/// Installs a modpack as a new instance. Nothing is visible until the pack is fully installed:
/// any failure or cancel removes the half-made instance.
fn install_pack(backend: &Backend, request: PackInstallRequest) -> Result<Response> {
    let instances = backend.instances.clone();
    let loaders = backend.launch.loaders().clone();
    let platforms = Arc::clone(&backend.catalog.platforms);
    let client = backend.catalog.client.clone();
    let events = Arc::clone(&backend.events);
    let subject = match &request.source {
        PackSource::File { path } => path.clone(),
        PackSource::Catalog { project, .. } => project.clone(),
        _ => return Err(Error::unsupported("this pack source")),
    };
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::InstallPack).subject(&subject),
        move |ctx| {
            crate::launch::with_job(ctx, |wired| {
                let control = crate::java::job_control(wired);
                let work = PackWork {
                    instances: &instances,
                    loaders: &loaders,
                    platforms: &platforms,
                    client: &client,
                    control: &control,
                };
                let (created, notes) = run_pack(&work, &request, || wired.check())?;
                events.publish(&Event::Instance(InstanceEvent::Added(created)));
                for (level, text) in notes {
                    events.publish(&Event::Log(LogLine {
                        source: LogSource::Launcher,
                        level,
                        text,
                    }));
                }
                Ok(())
            })
        },
    );
    Ok(Response::Task(handle.id()))
}

struct PackWork<'a> {
    instances: &'a vl_instance::Instances,
    loaders: &'a vl_loaders::Loaders,
    platforms: &'a Platforms,
    client: &'a Client,
    control: &'a Control,
}

/// Removes a directory when dropped, unless it was marked to keep.
struct Cleanup(std::path::PathBuf, bool);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// The loader component a pack's loader needs.
pub(crate) fn component_of(kind: PackLoader) -> Result<ComponentKind> {
    match kind {
        PackLoader::Fabric => Ok(ComponentKind::Fabric),
        PackLoader::Quilt => Ok(ComponentKind::Quilt),
        PackLoader::NeoForge => Ok(ComponentKind::NeoForge),
        PackLoader::Forge => Ok(ComponentKind::Forge),
    }
}

/// Lines for the launcher log after a pack install.
type Notes = Vec<(LogLevel, String)>;

/// Installs a pack as a new instance: a Modrinth `.mrpack`, or a CurseForge zip (a catalog
/// project of CurseForge, or a file with `manifest.json` and no `modrinth.index.json`).
fn run_pack(
    work: &PackWork<'_>,
    request: &PackInstallRequest,
    check: impl Fn() -> Result<()>,
) -> Result<(InstanceSummary, Notes)> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let scratch = work.instances.root().join(format!(
        ".pack-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _scratch = Cleanup(scratch.clone(), false);
    let (file, source) = match &request.source {
        PackSource::File { path } => (std::path::PathBuf::from(path), None),
        PackSource::Catalog {
            provider: Provider::CurseForge,
            project,
            version,
        } => {
            let (file, record) = vl_install::cfpack::fetch(
                work.platforms.curseforge(),
                work.client,
                project,
                version.as_deref(),
                request.min_stability,
                &scratch,
                work.control,
            )?;
            (file, Some(record))
        }
        PackSource::Catalog {
            provider,
            project,
            version,
        } => {
            let source = work.platforms.source(*provider)?;
            let (file, record) = vl_install::mrpack::fetch(
                source,
                work.client,
                project,
                version.as_deref(),
                request.min_stability,
                &scratch,
                work.control,
            )?;
            (file, Some(record))
        }
        _ => return Err(Error::unsupported("this pack source")),
    };
    check()?;
    let curseforge = match &source {
        Some(record) => record.provider == Provider::CurseForge,
        None => vl_install::cfpack::is_curseforge_pack(&file)?,
    };
    if curseforge {
        return run_curseforge_pack(work, request, &check, &file, source);
    }
    let pack = vl_install::mrpack::Pack::open(&file)?;
    let game_version = pack
        .index
        .game_version()
        .ok_or_else(|| Error::invalid_input("the pack does not name a Minecraft version"))?
        .to_owned();
    let loader = pack_loader(work, &game_version, pack.index.loader())?;
    check()?;
    let created = create_instance(work, request, &pack.index.name, game_version, loader)?;
    let dir = work.instances.dir(&created.id)?;
    let mut undo = Cleanup(dir.clone(), false);
    let meta = dir.join(META_DIR);
    let report = vl_install::mrpack::install(
        &pack,
        &vl_install::mrpack::InstallInput {
            client: work.client,
            game_dir: &work.instances.game_dir(&created.id)?,
            staging: &meta.join("staging"),
            control: work.control,
        },
    )?;
    check()?;
    let mut notes = Vec::new();
    let origin_source = match source.as_ref() {
        Some(record) => work.platforms.source(record.provider),
        None => work.platforms.source(Default::default()),
    };
    let mut known = std::collections::HashSet::new();
    let mut known_paths = Vec::new();
    if let Ok(origin_source) = origin_source {
        let resolved =
            vl_install::mrpack_origins::resolve(origin_source, &pack, &report, work.control)?;
        if resolved.lookup_failed {
            notes.push(
                "could not look up the origin of the pack files; update checks skip them".into(),
            );
        }
        for origin in &resolved.origins {
            known_paths.push(format!("{}/{}", origin.kind.dir(), origin.file));
        }
        known.extend(known_paths.iter().map(String::as_str));
        OriginStore::new(meta.join("resources.json")).save(&resolved.origins)?;
    }
    vl_install::mrpack::PackRecord::new(&pack, &report, source)
        .with_known_origins(&known)
        .with_overrides(pack.override_hashes()?)
        .save(&meta)?;
    // The instance is complete: keep it.
    undo.1 = true;
    if !report.optional.is_empty() {
        notes.push(format!(
            "installed optional files of the pack: {}",
            report.optional.join(", ")
        ));
    }
    Ok((
        created,
        notes.into_iter().map(|n| (LogLevel::Info, n)).collect(),
    ))
}

/// The loader request of a pack's loader, with the version the loader source has.
fn pack_loader(
    work: &PackWork<'_>,
    game_version: &str,
    loader: Option<(PackLoader, &str)>,
) -> Result<Option<LoaderRequest>> {
    let Some((kind, version)) = loader else {
        return Ok(None);
    };
    let component = component_of(kind)?;
    let chosen = work
        .loaders
        .pick(component, game_version, Some(version), work.control)?;
    Ok(Some(LoaderRequest {
        kind: component,
        version: Some(chosen.version),
    }))
}

/// Creates the instance of a pack: the requested name, else the pack's.
fn create_instance(
    work: &PackWork<'_>,
    request: &PackInstallRequest,
    pack_name: &str,
    game_version: String,
    loader: Option<LoaderRequest>,
) -> Result<InstanceSummary> {
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(pack_name)
        .to_owned();
    work.instances.create(&NewInstance {
        name,
        game_version,
        group: request.group.clone(),
        loader,
    })
}

/// Installs a CurseForge pack (see [`vl_install::cfpack`]). The files are resolved through the
/// API before the instance is created, so a missing key (`NoApiKey`) or a refused one leaves
/// nothing behind. Files the API cannot serve do not stop the install: each one is a warning in
/// the launcher log with the page to download it from, and they are kept in `pack.json`.
fn run_curseforge_pack(
    work: &PackWork<'_>,
    request: &PackInstallRequest,
    check: &impl Fn() -> Result<()>,
    file: &Path,
    source: Option<vl_install::mrpack::PackSourceRecord>,
) -> Result<(InstanceSummary, Notes)> {
    use vl_install::cfpack;
    let pack = cfpack::CfPack::open(file)?;
    let loader = pack.loader()?;
    let resolved = cfpack::resolve(work.platforms.curseforge(), &pack, work.control)?;
    check()?;
    let game_version = pack.game_version().to_owned();
    let loader = pack_loader(
        work,
        &game_version,
        loader.as_ref().map(|(k, v)| (*k, v.as_str())),
    )?;
    check()?;
    let created = create_instance(work, request, &pack.manifest.name, game_version, loader)?;
    let dir = work.instances.dir(&created.id)?;
    let mut undo = Cleanup(dir.clone(), false);
    let meta = dir.join(META_DIR);
    let report = cfpack::install(
        &pack,
        &resolved,
        &vl_install::mrpack::InstallInput {
            client: work.client,
            game_dir: &work.instances.game_dir(&created.id)?,
            staging: &meta.join("staging"),
            control: work.control,
        },
    )?;
    check()?;
    let (record, origins) = cfpack::record(&pack, &report, source)?;
    OriginStore::new(meta.join("resources.json")).save(&origins)?;
    record.save(&meta)?;
    // The instance is complete: keep it.
    undo.1 = true;
    let blocked = report.blocked.len();
    let mut notes: Notes = report
        .notes()
        .into_iter()
        .enumerate()
        .map(|(n, text)| {
            let level = if n < blocked {
                LogLevel::Warn
            } else {
                LogLevel::Info
            };
            (level, text)
        })
        .collect();
    if blocked > 0 {
        notes.push((
            LogLevel::Warn,
            format!(
                "{blocked} file(s) of the pack must be downloaded by hand; the list is kept in \
                 .vibelauncher/pack.json"
            ),
        ));
    }
    Ok((created, notes))
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
            world: None,
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

    struct Offline;

    impl vl_net::HttpTransport for Offline {
        fn get(
            &self,
            _: &vl_net::HttpRequest,
        ) -> std::result::Result<vl_net::HttpResponse, vl_net::NetError> {
            Ok(vl_net::HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Box::new(std::io::Cursor::new(Vec::new())),
            })
        }
    }

    fn pack(path: &std::path::Path, dependencies: &str, files: &str) {
        use std::io::Write;
        let index = format!(
            r#"{{"formatVersion":1,"game":"minecraft","versionId":"1","name":"Mini Pack","files":[{files}],"dependencies":{{{dependencies}}}}}"#
        );
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("modrinth.index.json", o).unwrap();
        w.write_all(index.as_bytes()).unwrap();
        w.start_file("overrides/config/a.txt", o).unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();
    }

    fn run_pack_file(
        root: &std::path::Path,
        pack_file: &std::path::Path,
    ) -> (Result<(InstanceSummary, Notes)>, vl_instance::Instances) {
        run_pack_with(root, pack_file, std::sync::Arc::new(Offline))
    }

    fn run_pack_with(
        root: &std::path::Path,
        pack_file: &std::path::Path,
        transport: std::sync::Arc<dyn vl_net::HttpTransport>,
    ) -> (Result<(InstanceSummary, Notes)>, vl_instance::Instances) {
        let client = Client::with_transport(NetConfig::new("0.0.0"), transport);
        let instances = vl_instance::Instances::open(&root.join("instances")).unwrap();
        let loaders = vl_loaders::Loaders::new(
            client.clone(),
            root.join("profiles"),
            vl_loaders::LoadersOptions::default(),
        );
        // No CurseForge key, whatever the machine has.
        let platforms = Platforms::with_sources(
            vl_platforms::ModrinthSource::new(client.clone(), Default::default()),
            vl_platforms::CurseForgeSource::new(
                client.clone(),
                Default::default(),
                std::sync::Arc::new(vl_platforms::curseforge::ApiKeys::default()),
            ),
        );
        let control = Control::default();
        let work = PackWork {
            instances: &instances,
            loaders: &loaders,
            platforms: &platforms,
            client: &client,
            control: &control,
        };
        let request = PackInstallRequest {
            source: PackSource::File {
                path: pack_file.to_string_lossy().into_owned(),
            },
            name: None,
            group: None,
            min_stability: Default::default(),
        };
        let result = run_pack(&work, &request, || Ok(()));
        (result, instances)
    }

    #[test]
    fn a_pack_becomes_an_instance_and_a_failed_one_leaves_nothing() {
        let root = std::env::temp_dir().join(format!("vl-backend-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("p.mrpack");

        pack(&file, r#""minecraft":"1.21.1""#, "");
        let (result, instances) = run_pack_file(&root, &file);
        let (summary, _) = result.unwrap();
        assert_eq!(summary.name, "Mini Pack");
        assert_eq!(summary.game_version, "1.21.1");
        let dir = instances.dir(&summary.id).unwrap();
        assert_eq!(
            std::fs::read(dir.join("minecraft/config/a.txt")).unwrap(),
            b"hello"
        );
        assert!(dir.join(".vibelauncher/pack.json").is_file());
        assert!(!dir.join(".vibelauncher/staging").exists());
        assert_eq!(instances.list().unwrap().len(), 1);

        // A file that cannot be downloaded: no new instance remains.
        let missing = format!(
            r#"{{"path":"mods/a.jar","hashes":{{"sha1":"{}","sha512":"{}"}},"downloads":["https://cdn.modrinth.com/a.jar"],"fileSize":3}}"#,
            "a".repeat(40),
            "b".repeat(128)
        );
        pack(&file, r#""minecraft":"1.21.1""#, &missing);
        let (result, instances) = run_pack_file(&root, &file);
        assert!(result.is_err());
        let names: Vec<_> = std::fs::read_dir(instances.root())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(instances.list().unwrap().len(), 1, "{names:?}");
        assert_eq!(names.len(), 1, "{names:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_curseforge_zip_without_a_key_is_refused_and_leaves_nothing() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("vl-backend-cf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("pack.zip");
        let manifest = r#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-47.2.0","primary":true}]},
            "manifestType":"minecraftModpack","manifestVersion":1,"name":"CF Pack",
            "files":[{"projectID":1,"fileID":2,"required":true}],"overrides":"overrides"}"#;
        let mut w = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("manifest.json", o).unwrap();
        w.write_all(manifest.as_bytes()).unwrap();
        w.start_file("overrides/config/a.txt", o).unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();

        let (result, instances) = run_pack_file(&root, &file);
        assert_eq!(result.unwrap_err().kind, ErrorKind::NoApiKey);
        assert!(instances.list().unwrap().is_empty());
        let left: Vec<_> = std::fs::read_dir(instances.root())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Serves the NeoForge and Forge version lists and nothing else.
    struct Maven;

    impl vl_net::HttpTransport for Maven {
        fn get(
            &self,
            request: &vl_net::HttpRequest,
        ) -> std::result::Result<vl_net::HttpResponse, vl_net::NetError> {
            let forge = request
                .url
                .ends_with("/net/minecraftforge/forge/maven-metadata.xml");
            let legacy = request
                .url
                .ends_with("/net/neoforged/forge/maven-metadata.xml");
            let xml = if legacy {
                "<metadata><versioning><versions><version>1.20.1-47.1.5</version><version>1.20.1-47.1.106</version></versions></versioning></metadata>"
            } else if forge {
                "<metadata><versioning><versions><version>1.21.1-52.0.0</version><version>1.21.1-52.1.2</version><version>1.20.1-47.3.0</version></versions></versioning></metadata>"
            } else {
                "<metadata><versioning><versions><version>21.1.2</version>                <version>21.1.7</version><version>21.4.1</version></versions></versioning></metadata>"
            };
            let found = forge || legacy || request.url.ends_with("/neoforge/maven-metadata.xml");
            Ok(vl_net::HttpResponse {
                status: if found { 200 } else { 404 },
                headers: Vec::new(),
                body: Box::new(std::io::Cursor::new(if found {
                    xml.as_bytes().to_vec()
                } else {
                    Vec::new()
                })),
            })
        }
    }

    #[test]
    fn a_neoforge_pack_creates_an_instance_on_that_loader() {
        let root = std::env::temp_dir().join(format!("vl-backend-neo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("p.mrpack");

        pack(&file, r#""minecraft":"1.21.1","neoforge":"21.1.7""#, "");
        let (result, instances) = run_pack_with(&root, &file, std::sync::Arc::new(Maven));
        let (summary, _) = result.unwrap();
        assert!(
            matches!(&summary.loader, Loader::NeoForge(v) if v == "21.1.7"),
            "{:?}",
            summary.loader
        );
        let dir = instances.dir(&summary.id).unwrap();
        assert!(dir.join(".vibelauncher/resources.json").is_file());
        assert!(dir.join(".vibelauncher/pack.json").is_file());

        // A version the list does not have is refused and creates nothing.
        pack(&file, r#""minecraft":"1.21.1","neoforge":"9.9.9""#, "");
        let (result, instances) = run_pack_with(&root, &file, std::sync::Arc::new(Maven));
        assert!(result.is_err());
        assert_eq!(instances.list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_neoforge_pack_for_1_20_1_uses_the_forge_artifact_versions() {
        let root =
            std::env::temp_dir().join(format!("vl-backend-neo-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("p.mrpack");

        pack(&file, r#""minecraft":"1.20.1","neoforge":"47.1.106""#, "");
        let (result, _) = run_pack_with(&root, &file, std::sync::Arc::new(Maven));
        let (summary, _) = result.unwrap();
        assert!(
            matches!(&summary.loader, Loader::NeoForge(v) if v == "47.1.106"),
            "{:?}",
            summary.loader
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_forge_pack_creates_an_instance_on_that_loader() {
        let root = std::env::temp_dir().join(format!("vl-backend-forge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("p.mrpack");

        pack(&file, r#""minecraft":"1.21.1","forge":"52.0.0""#, "");
        let (result, instances) = run_pack_with(&root, &file, std::sync::Arc::new(Maven));
        let (summary, _) = result.unwrap();
        assert!(
            matches!(&summary.loader, Loader::Forge(v) if v == "52.0.0"),
            "{:?}",
            summary.loader
        );
        let dir = instances.dir(&summary.id).unwrap();
        assert!(dir.join(".vibelauncher/pack.json").is_file());

        // A version the list does not have is refused and creates nothing.
        pack(&file, r#""minecraft":"1.21.1","forge":"9.9.9""#, "");
        let (result, instances) = run_pack_with(&root, &file, std::sync::Arc::new(Maven));
        assert!(result.is_err());
        assert_eq!(instances.list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
