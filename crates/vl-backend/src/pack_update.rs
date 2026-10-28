//! Checking and updating the modpack an instance was installed from.
//!
//! Checks and plans are answered from a worker thread. An update runs as an `UpdatePack` task:
//! it fetches the new pack, plans against the folder as it is when the task starts, and applies
//! the plan in one all-or-nothing step together with the change of the instance's game version
//! and loader (see [`vl_install::mrpack_update`]). Packs installed from CurseForge are updated
//! the same way; their files are resolved through the CurseForge API, so without a key every
//! command answers `NoApiKey` before anything is sent, and files the user has to download by
//! hand are launcher log warnings.

use std::path::PathBuf;
use std::sync::Arc;

use vl_core::catalog::Provider;
use vl_core::component::ComponentKind;
use vl_core::install::{PackAction, PackChanges, PackUpdateRequest};
use vl_core::instance::{InstanceEvent, Loader, LoaderRequest};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::task::TaskKind;
use vl_core::{Error, Event, InstanceId, Replier, Response, Result};
use vl_install::mrpack::{PackKind, PackRecord};
use vl_install::mrpack_update::{Context, Prepared, Wanted, apply, check as check_pack, prepare};
use vl_net::{Client, Control};
use vl_platforms::CurseForgeSource;
use vl_platforms::Platforms;
use vl_tasks::TaskSpec;

use crate::Backend;
use crate::catalog::component_of;
use crate::updates::spawn_reply;

/// Everything an update of one instance needs, gathered before the work starts.
struct Setup {
    id: InstanceId,
    name: String,
    game_dir: PathBuf,
    meta: PathBuf,
    record: PackRecord,
    provider: Provider,
    platforms: Arc<Platforms>,
    client: Client,
    instances: vl_instance::Instances,
    loaders: vl_loaders::Loaders,
}

fn setup(backend: &Backend, id: &InstanceId) -> Result<Setup> {
    let config = backend.instances.load(id)?;
    if backend.launch.is_active(id) {
        return Err(Error::conflict(format!("instance {id} is running")));
    }
    let meta = backend.instances.dir(id)?.join(".vibelauncher");
    let record = PackRecord::load(&meta).ok_or_else(|| {
        Error::not_found(format!("instance {id} was not installed from a modpack"))
    })?;
    let provider = match (&record.source, record.kind) {
        (Some(s), _) => s.provider,
        (None, PackKind::CurseForge) => Provider::CurseForge,
        (None, _) => Provider::default(),
    };
    Ok(Setup {
        id: id.clone(),
        name: config.name,
        game_dir: backend.instances.game_dir(id)?,
        provider,
        meta,
        record,
        platforms: Arc::clone(&backend.catalog.platforms),
        client: backend.catalog.client.clone(),
        instances: backend.instances.clone(),
        loaders: backend.launch.loaders().clone(),
    })
}

/// What an update of `setup` works on.
fn context<'a>(setup: &'a Setup, control: &'a Control) -> Result<Context<'a>> {
    Ok(Context {
        source: setup.platforms.source(setup.provider)?,
        curseforge: Some(curseforge(setup)),
        client: &setup.client,
        game_dir: &setup.game_dir,
        meta_dir: &setup.meta,
        control,
    })
}

fn curseforge(setup: &Setup) -> &CurseForgeSource {
    setup.platforms.curseforge()
}

fn wanted<'a>(request: &'a PackUpdateRequest, file: Option<&'a std::path::Path>) -> Wanted<'a> {
    Wanted {
        min: request.min_stability,
        version: request.version.as_deref(),
        file,
        reset: request.reset,
    }
}

pub(crate) fn check(backend: &Backend, request: PackUpdateRequest, replier: Replier) {
    let setup = match setup(backend, &request.instance) {
        Ok(s) => s,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("pack-update-check", replier, move || {
        let control = Control::default();
        let ctx = context(&setup, &control)?;
        check_pack(&ctx, &setup.record, request.min_stability).map(Response::PackUpdate)
    });
}

fn empty_changes(setup: &Setup, reset: bool) -> PackChanges {
    let current = setup.record.source.as_ref().map_or_else(
        || setup.record.pack_version.clone(),
        |s| s.version_number.clone(),
    );
    PackChanges {
        name: setup.record.name.clone(),
        from: current.clone(),
        to: current,
        reset,
        ..PackChanges::default()
    }
}

pub(crate) fn plan(backend: &Backend, request: PackUpdateRequest, replier: Replier) {
    let setup = match setup(backend, &request.instance) {
        Ok(s) => s,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("pack-update-plan", replier, move || {
        let control = Control::default();
        let ctx = context(&setup, &control)?;
        let file = request.file.as_deref().map(std::path::Path::new);
        let changes = match prepare(&ctx, &setup.record, &wanted(&request, file))? {
            Some(p) => p.changes,
            None => empty_changes(&setup, request.reset),
        };
        Ok(Response::PackChanges(changes))
    });
}

type Target = (String, Option<LoaderRequest>);

/// The game version and loader the instance gets, or `None` when it already has them.
fn components(setup: &Setup, prepared: &Prepared, control: &Control) -> Result<Option<Target>> {
    let config = setup.instances.load(&setup.id)?;
    let game = prepared.new_game_version.clone();
    let game_changed = config.game_version() != Some(game.as_str());
    let Some((kind, version)) = &prepared.loader else {
        return Ok(game_changed.then_some((game, None)));
    };
    let component = component_of(*kind)?;
    let chosen = setup
        .loaders
        .pick(component, &game, Some(version), control)?;
    let same = match (config.loader(), component) {
        (Loader::Fabric(v), ComponentKind::Fabric)
        | (Loader::Quilt(v), ComponentKind::Quilt)
        | (Loader::NeoForge(v), ComponentKind::NeoForge)
        | (Loader::Forge(v), ComponentKind::Forge) => v == chosen.version,
        _ => false,
    };
    if same && !game_changed {
        return Ok(None);
    }
    Ok(Some((
        game,
        Some(LoaderRequest {
            kind: component,
            version: Some(chosen.version),
        }),
    )))
}

pub(crate) fn update(backend: &Backend, request: PackUpdateRequest) -> Result<Response> {
    let setup = setup(backend, &request.instance)?;
    let events = Arc::clone(&backend.events);
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::UpdatePack).subject(&setup.name),
        move |ctx| {
            crate::launch::with_job(ctx, |wired| {
                let control = crate::java::job_control(wired);
                let log = |text: String| {
                    events.publish(&Event::Log(LogLine {
                        source: LogSource::Launcher,
                        level: LogLevel::Info,
                        text,
                    }));
                };
                let context = context(&setup, &control)?;
                let file = request.file.as_deref().map(std::path::Path::new);
                let Some(prepared) = prepare(&context, &setup.record, &wanted(&request, file))?
                else {
                    log("the modpack is up to date".into());
                    return Ok(());
                };
                wired.check()?;
                // Everything that can fail before a file is touched fails here.
                let target = components(&setup, &prepared, &control)?;
                let changes = prepared.changes.clone();
                let mut summary = None;
                let applied = apply(prepared, &context, |_| {
                    if let Some((game, loader)) = &target {
                        summary = Some(setup.instances.set_version(
                            &setup.id,
                            game,
                            loader.as_ref(),
                        )?);
                    }
                    Ok(())
                })?;
                if let Some(summary) = summary {
                    events.publish(&Event::Instance(InstanceEvent::Changed(summary)));
                }
                for c in &changes.changes {
                    let kept = matches!(
                        c.action,
                        PackAction::SaveNew | PackAction::KeepDeleted | PackAction::KeepModified
                    );
                    if let (true, Some(note)) = (kept, &c.note) {
                        log(format!("{}: {note}", c.path));
                    }
                }
                for blocked in &applied.blocked {
                    events.publish(&Event::Log(LogLine {
                        source: LogSource::Launcher,
                        level: LogLevel::Warn,
                        text: blocked.describe(),
                    }));
                }
                log(format!(
                    "modpack updated to {}; previous versions of changed files are in {}",
                    changes.to,
                    applied.backup.display()
                ));
                Ok(())
            })
        },
    );
    Ok(Response::Task(handle.id()))
}
