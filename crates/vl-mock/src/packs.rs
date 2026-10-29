//! Synthetic modpacks: installing one as a new instance, checking and updating the pack of an
//! instance, and exporting an instance. Both flavours exist: a Modrinth pack (`.mrpack`) and
//! CurseForge packs, one of which has a file the API will not serve.

use std::sync::Arc;

use vl_core::catalog::{ProjectType, Provider};
use vl_core::install::{
    ExportInclude, PackAction, PackChange, PackChanges, PackExport, PackExportRequest,
    PackInstallRequest, PackSource, PackUpdateInfo, PackUpdateRequest, PackVersionChange,
    VersionLabel,
};
use vl_core::instance::{InstanceEvent, InstanceState, InstanceSummary, Loader};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::resources::ResourceKind;
use vl_core::task::TaskKind;
use vl_core::{Error, ErrorKind, Event, InstanceId, Response, Result};

use crate::catalog::{self, BLOCKED_PACK, ENTRIES, VERSIONS};
use crate::{Inner, State, data, resources, sim};

/// The page a blocked file must be fetched from (invented).
const BLOCKED_FILE: &str = "mock-restricted-mod-1.0.0.jar";
const BLOCKED_PAGE: &str = "https://example.invalid/mock-restricted-mod";
/// Bytes one downloaded file of a pack update adds up to.
const FILE_BYTES: u64 = 1_310_720;

/// The modpack an instance was installed from.
#[derive(Debug, Clone)]
pub(crate) struct Origin {
    /// The catalog project; `None` for a pack installed from a file.
    pub project: Option<usize>,
    /// The index into [`VERSIONS`] of the installed version.
    pub version: usize,
    pub name: String,
    /// The catalog projects whose files came with the pack.
    pub members: Vec<usize>,
    /// A file of the pack could not be downloaded through the API.
    pub blocked: bool,
}

/// The pack the `creative` instance starts from, one version behind.
pub(crate) fn initial() -> Option<(InstanceId, Origin)> {
    Some((
        InstanceId::new("creative")?,
        Origin {
            project: Some(5),
            version: 2,
            name: ENTRIES[5].title.into(),
            members: vec![0, 1, 2],
            blocked: false,
        },
    ))
}

/// Whether the files of project `idx` came with the instance's pack.
pub(crate) fn is_pack_file(state: &State, instance: &InstanceId, idx: usize) -> bool {
    state
        .packs
        .get(instance)
        .is_some_and(|origin| origin.members.contains(&idx))
}

fn members(provider: Provider) -> Vec<usize> {
    match provider {
        Provider::CurseForge => vec![12, 13, 14],
        _ => vec![0, 1, 2],
    }
}

fn pack_loader(provider: Provider, v: usize) -> Loader {
    // Newer pack versions ship a newer loader.
    match (provider, v == VERSIONS.len() - 1) {
        (Provider::CurseForge, true) => Loader::Forge("52.0.1".into()),
        (Provider::CurseForge, false) => Loader::Forge("52.1.0".into()),
        (_, true) => Loader::Fabric("0.20.1".into()),
        (_, false) => Loader::Fabric("0.20.2".into()),
    }
}

fn loader_text(loader: &Loader) -> String {
    format!("{loader}").to_lowercase()
}

fn log(inner: &Inner, level: LogLevel, text: String) {
    inner.publish(Event::Log(LogLine {
        source: LogSource::Launcher,
        level,
        text,
    }));
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

/// What a request resolves to.
struct Resolved {
    name: String,
    project: Option<usize>,
    version: usize,
    provider: Provider,
    blocked: bool,
    corrupt: bool,
}

fn resolve(state: &State, request: &PackInstallRequest) -> Result<Resolved> {
    let mut resolved = match &request.source {
        PackSource::File { path } => {
            let lower = path.to_lowercase();
            let provider = if lower.ends_with(".mrpack") {
                Provider::Modrinth
            } else if lower.ends_with(".zip") {
                Provider::CurseForge
            } else {
                return Err(Error::invalid_input(
                    "a modpack file is a .mrpack or a CurseForge .zip",
                ));
            };
            let stem = file_stem(path);
            Resolved {
                blocked: provider == Provider::CurseForge && stem.contains("blocked"),
                corrupt: stem.contains("corrupt"),
                name: stem,
                project: None,
                version: catalog::newest(vl_core::install::MinStability::Release),
                provider,
            }
        }
        PackSource::Catalog {
            provider,
            project,
            version,
        } => {
            let idx = catalog::find(project)?;
            let entry = &ENTRIES[idx];
            if entry.provider != *provider {
                return Err(Error::not_found(format!(
                    "project {project} on {}",
                    catalog::provider_name(*provider)
                )));
            }
            if entry.kind != ProjectType::Modpack {
                return Err(Error::invalid_input(format!(
                    "{} is not a modpack",
                    entry.title
                )));
            }
            Resolved {
                name: entry.title.into(),
                project: Some(idx),
                version: catalog::pick_version(idx, version.as_deref(), request.min_stability)?,
                provider: *provider,
                blocked: idx == BLOCKED_PACK,
                corrupt: false,
            }
        }
        other => {
            return Err(Error::unsupported(format!(
                "vl-mock does not simulate {other:?}"
            )));
        }
    };
    catalog::check_key(state, resolved.provider)?;
    if let Some(name) = &request.name {
        resolved.name = name.trim().to_owned();
        if resolved.name.is_empty() {
            return Err(Error::invalid_input("empty instance name"));
        }
    }
    Ok(resolved)
}

pub(crate) fn install(inner: &Arc<Inner>, request: &PackInstallRequest) -> Result<Response> {
    let resolved = resolve(&inner.lock(), request)?;
    let subject = resolved.name.clone();
    let group = request.group.clone();
    let stages = vec![
        ("index".to_owned(), 2),
        ("download".to_owned(), 8),
        ("overrides".to_owned(), 3),
    ];
    sim::staged(
        inner,
        TaskKind::InstallPack,
        Some(subject),
        stages,
        move |inner| finish_install(inner, resolved, group),
    )
}

fn finish_install(inner: &Inner, resolved: Resolved, group: Option<String>) -> Result<()> {
    if resolved.corrupt {
        return Err(Error::new(
            ErrorKind::Integrity,
            "the pack index is damaged: it is not valid JSON",
        ));
    }
    let members = members(resolved.provider);
    let summary = {
        let mut state = inner.lock();
        let id = data::slug(&resolved.name, |c| {
            state.instances.iter().any(|i| i.id.as_str() == c)
        })
        .ok_or_else(|| Error::internal("no instance id"))?;
        let summary = InstanceSummary {
            id: id.clone(),
            name: resolved.name.clone(),
            group,
            game_version: "1.21.11".into(),
            loader: pack_loader(resolved.provider, resolved.version),
            play_time_secs: 0,
            last_played_unix: None,
            state: InstanceState::Stopped,
            problem: None,
        };
        state.instances.push(summary.clone());
        *resources::folder(&mut state, &id, "mods") = members
            .iter()
            .map(|idx| {
                resources::make_entry(
                    ResourceKind::Mods,
                    &catalog::file_name(*idx, resolved.version),
                    true,
                )
            })
            .collect();
        state.packs.insert(
            id,
            Origin {
                project: resolved.project,
                version: resolved.version,
                name: resolved.name.clone(),
                members,
                blocked: resolved.blocked,
            },
        );
        summary
    };
    inner.publish(Event::Instance(InstanceEvent::Added(summary)));
    if resolved.blocked {
        log(
            inner,
            LogLevel::Warn,
            format!(
                "{BLOCKED_FILE}: the author does not allow downloads through the API; download \
                 it from {BLOCKED_PAGE} and put it into the mods folder"
            ),
        );
        log(
            inner,
            LogLevel::Warn,
            "1 file(s) of the pack must be downloaded by hand; the list is kept in \
             .vibelauncher/pack.json"
                .into(),
        );
    }
    log(
        inner,
        LogLevel::Info,
        format!("installed the pack {}", resolved.name),
    );
    Ok(())
}

/// The origin of an instance's pack.
fn origin<'a>(state: &'a State, id: &InstanceId) -> Result<&'a Origin> {
    resources::usable(state, id)?;
    state
        .packs
        .get(id)
        .ok_or_else(|| Error::conflict(format!("instance {id} was not installed from a modpack")))
}

fn current_label(origin: &Origin) -> VersionLabel {
    match origin.project {
        Some(idx) => catalog::label(idx, origin.version),
        None => VersionLabel {
            id: "file".into(),
            number: VERSIONS[origin.version].0.into(),
            channel: VERSIONS[origin.version].1,
        },
    }
}

fn blocked_warning(origin: &Origin) -> Vec<String> {
    if origin.blocked {
        vec![format!(
            "1 file of the pack must be downloaded by hand: {BLOCKED_FILE} ({BLOCKED_PAGE})"
        )]
    } else {
        Vec::new()
    }
}

/// The version an update goes to, if it is another than the installed one.
fn target(state: &State, origin: &Origin, request: &PackUpdateRequest) -> Result<Option<usize>> {
    let v = match origin.project {
        Some(idx) => {
            catalog::check_key(state, ENTRIES[idx].provider)?;
            catalog::pick_version(idx, request.version.as_deref(), request.min_stability)?
        }
        None => catalog::newest(request.min_stability),
    };
    // A newer version has a smaller index; an explicit version may also be an older one.
    let wanted = v < origin.version || (request.version.is_some() && v != origin.version);
    Ok(wanted.then_some(v))
}

pub(crate) fn check_update(state: &State, request: &PackUpdateRequest) -> Result<PackUpdateInfo> {
    let origin = origin(state, &request.instance)?;
    let current = current_label(origin);
    if origin.project.is_none() {
        return Ok(PackUpdateInfo {
            name: origin.name.clone(),
            current,
            latest: None,
            from_file: true,
            warnings: Vec::new(),
        });
    }
    let latest = target(state, origin, request)?
        .and_then(|v| origin.project.map(|idx| catalog::label(idx, v)));
    Ok(PackUpdateInfo {
        name: origin.name.clone(),
        current,
        latest,
        from_file: false,
        warnings: blocked_warning(origin),
    })
}

fn change(path: String, action: PackAction, download: bool, note: Option<String>) -> PackChange {
    PackChange {
        path,
        action,
        download,
        disabled: false,
        note,
    }
}

pub(crate) fn plan_update(state: &State, request: &PackUpdateRequest) -> Result<PackChanges> {
    let origin = origin(state, &request.instance)?;
    if origin.project.is_none() && request.file.is_none() {
        return Err(Error::invalid_input(
            "this pack was installed from a file; give the new file",
        ));
    }
    let instance = resources::usable(state, &request.instance)?;
    let from = VERSIONS[origin.version].0;
    let Some(v) = target(state, origin, request)? else {
        return Ok(PackChanges {
            name: origin.name.clone(),
            from: from.into(),
            to: from.into(),
            unchanged: 24,
            reset: request.reset,
            ..PackChanges::default()
        });
    };
    let to = VERSIONS[v].0;
    let slug = |n: usize| {
        origin
            .members
            .get(n)
            .and_then(|idx| ENTRIES.get(*idx))
            .map_or("mock-pack", |e| e.slug)
    };
    let reset = request.reset;
    let mut changes = vec![
        change(
            format!("mods/mock-pack-extra-{to}.jar"),
            PackAction::Add,
            true,
            None,
        ),
        change(
            format!("mods/{}-{to}.jar", slug(1)),
            PackAction::Update,
            true,
            Some(format!("replaces {}-{from}.jar", slug(1))),
        ),
        change(
            "mods/mock-pack-legacy-1.0.0.jar".into(),
            PackAction::Remove,
            true,
            None,
        ),
        change(
            format!("config/{}.toml", slug(1)),
            PackAction::SaveNew,
            false,
            Some(format!(
                "you changed this config; the pack's version is saved beside it as {}.toml.pack-new",
                slug(1)
            )),
        ),
    ];
    let mut disabled = change(
        format!("mods/{}-{to}.jar", slug(2)),
        if reset {
            PackAction::Enable
        } else {
            PackAction::Update
        },
        true,
        (!reset).then(|| "you disabled this file; it stays disabled".into()),
    );
    disabled.disabled = !reset;
    changes.push(disabled);
    changes.push(if reset {
        change(
            format!("mods/mock-pack-optional-{to}.jar"),
            PackAction::Add,
            true,
            Some("you deleted this file; the reset puts it back".into()),
        )
    } else {
        change(
            format!("mods/mock-pack-optional-{to}.jar"),
            PackAction::KeepDeleted,
            true,
            Some("you deleted this file; it stays deleted".into()),
        )
    });
    changes.push(if reset {
        change(
            format!("config/{}.toml", slug(2)),
            PackAction::Update,
            false,
            Some("you changed this file; the reset overwrites it".into()),
        )
    } else {
        change(
            format!("config/{}.toml", slug(2)),
            PackAction::KeepModified,
            false,
            Some("you changed this file; it stays as it is".into()),
        )
    });
    let download_bytes = changes
        .iter()
        .filter(|c| c.download && matches!(c.action, PackAction::Add | PackAction::Update))
        .count() as u64
        * FILE_BYTES;
    let provider = origin
        .project
        .and_then(|idx| ENTRIES.get(idx))
        .map_or(Provider::Modrinth, |e| e.provider);
    let new_loader = pack_loader(provider, v);
    let loader =
        (loader_text(&instance.loader) != loader_text(&new_loader)).then(|| PackVersionChange {
            from: loader_text(&instance.loader),
            to: loader_text(&new_loader),
        });
    Ok(PackChanges {
        name: origin.name.clone(),
        from: from.into(),
        to: to.into(),
        game_version: None,
        loader,
        changes,
        unchanged: 24,
        download_bytes,
        reset,
        warnings: blocked_warning(origin),
    })
}

pub(crate) fn update(inner: &Arc<Inner>, request: &PackUpdateRequest) -> Result<Response> {
    let (changes, v, name, provider) = {
        let state = inner.lock();
        let changes = plan_update(&state, request)?;
        let origin = origin(&state, &request.instance)?;
        let v = target(&state, origin, request)?;
        let provider = origin
            .project
            .and_then(|idx| ENTRIES.get(idx))
            .map_or(Provider::Modrinth, |e| e.provider);
        (changes, v, origin.name.clone(), provider)
    };
    let Some(v) = v.filter(|_| !changes.is_empty()) else {
        return Err(Error::conflict("the pack is up to date"));
    };
    let instance = request.instance.clone();
    let stages = vec![("download".to_owned(), 6), ("apply".to_owned(), 4)];
    sim::staged(
        inner,
        TaskKind::UpdatePack,
        Some(name),
        stages,
        move |inner| {
            let summary = {
                let mut state = inner.lock();
                apply_update(&mut state, &instance, v, provider)?
            };
            inner.publish(Event::Instance(InstanceEvent::Changed(summary)));
            Ok(())
        },
    )
}

/// Moves the pack's files and the loader to version `v`; the user's own files stay.
fn apply_update(
    state: &mut State,
    instance: &InstanceId,
    v: usize,
    provider: Provider,
) -> Result<InstanceSummary> {
    let members = state
        .packs
        .get(instance)
        .map(|o| o.members.clone())
        .ok_or_else(|| Error::not_found(format!("pack of {instance}")))?;
    for file in resources::folder(state, instance, "mods") {
        if let Some((idx, _)) =
            catalog::find_by_file(&file.file).filter(|(i, _)| members.contains(i))
        {
            let enabled = file.enabled;
            *file = resources::make_entry(ResourceKind::Mods, &catalog::file_name(idx, v), enabled);
        }
    }
    if let Some(origin) = state.packs.get_mut(instance) {
        origin.version = v;
    }
    let summary = state
        .instances
        .iter_mut()
        .find(|i| &i.id == instance)
        .ok_or_else(|| Error::not_found(format!("instance {instance}")))?;
    summary.loader = pack_loader(provider, v);
    Ok(summary.clone())
}

pub(crate) fn export(state: &State, request: &PackExportRequest) -> Result<PackExport> {
    let instance = resources::usable(state, &request.instance)?;
    let out = request.out.trim();
    if out.is_empty() {
        return Err(Error::invalid_input("empty output file"));
    }
    let mut folders = vec![ResourceKind::Mods];
    if request.include.contains(&ExportInclude::ResourcePacks) {
        folders.push(ResourceKind::ResourcePacks);
    }
    if request.include.contains(&ExportInclude::ShaderPacks) {
        folders.push(ResourceKind::ShaderPacks);
    }
    let (mut linked, mut disabled) = (0, 0);
    let mut unlisted = Vec::new();
    for kind in folders {
        for file in resources::snapshot(state, &request.instance, &resources::area(kind)) {
            if !file.enabled {
                disabled += 1;
            } else if catalog::find_by_file(&file.file).is_some() {
                linked += 1;
            } else {
                unlisted.push(file.file);
            }
        }
    }
    let config = |include, files| {
        if request.include.contains(&include) {
            files
        } else {
            0
        }
    };
    let bundled = unlisted.len()
        + config(ExportInclude::Config, 12)
        + config(ExportInclude::Kubejs, 6)
        + config(ExportInclude::Options, 1);
    let mut warnings = Vec::new();
    if !out.to_lowercase().ends_with(".mrpack") {
        warnings.push("the file name does not end with .mrpack".to_owned());
    }
    if !unlisted.is_empty() {
        warnings.push(format!(
            "{} file(s) are not on the catalog and are carried inside the pack",
            unlisted.len()
        ));
    }
    if instance.loader == Loader::Vanilla {
        warnings.push("the instance has no mod loader".into());
    }
    Ok(PackExport {
        path: out.into(),
        linked,
        bundled,
        unlisted,
        disabled,
        size: linked as u64 * 4_096 + bundled as u64 * 20_480 + 1_024,
        warnings,
    })
}
