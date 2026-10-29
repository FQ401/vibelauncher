//! Synthetic installs, update checks and updates of catalog files. Files are recognized by their
//! names (`<slug>-<version>.<ext>`), so the catalog needs no per-file records.

use std::sync::Arc;

use vl_core::catalog::{ProjectType, VersionFile};
use vl_core::install::{
    IdentifyReport, InstallPlan, InstallRequest, MatchedBy, MinStability, PackFile, PlanAction,
    PlanItem, PlanRole, Recommendation, Satisfied, UncheckedFile, UncheckedReason, UpdateCandidate,
    UpdateCheckRequest, UpdateConflict, UpdateReport, UpdateRequest,
};
use vl_core::resources::ResourceKind;
use vl_core::task::TaskKind;
use vl_core::{Error, InstanceId, Response, Result};

use crate::catalog::{self, ENTRIES, VERSIONS};
use crate::{Inner, State, packs, resources, sim};

/// The folders an update check looks at.
const CHECKED: [ResourceKind; 3] = [
    ResourceKind::Mods,
    ResourceKind::ResourcePacks,
    ResourceKind::ShaderPacks,
];

fn kind_of(project: ProjectType) -> Result<ResourceKind> {
    match project {
        ProjectType::Mod => Ok(ResourceKind::Mods),
        ProjectType::ResourcePack => Ok(ResourceKind::ResourcePacks),
        ProjectType::Shader => Ok(ResourceKind::ShaderPacks),
        ProjectType::DataPack => Ok(ResourceKind::DataPacks),
        ProjectType::Modpack => Err(Error::invalid_input(
            "a modpack is installed as a new instance, not into one",
        )),
        _ => Err(Error::invalid_input(
            "this kind of project cannot be installed",
        )),
    }
}

fn describe(state: &State, instance: &InstanceId) -> Result<(String, Option<String>)> {
    let i = resources::usable(state, instance)?;
    Ok((
        i.game_version.clone(),
        resources::loader_name(&i.loader).map(Into::into),
    ))
}

fn item(idx: usize, v: usize, role: PlanRole, action: PlanAction) -> Result<PlanItem> {
    let entry = ENTRIES
        .get(idx)
        .ok_or_else(|| Error::internal("no such project"))?;
    let file: VersionFile = catalog::version_file(idx, v);
    let label = catalog::label(idx, v);
    Ok(PlanItem {
        provider: entry.provider,
        project_id: catalog::id_of(idx),
        slug: entry.slug.into(),
        title: entry.title.into(),
        project_type: entry.kind,
        kind: kind_of(entry.kind)?,
        role,
        version_id: label.id,
        version_number: label.number,
        channel: label.channel,
        file,
        action,
    })
}

/// The data pack folder key for `world`, else the kind's folder.
fn area_for(kind: ResourceKind, world: Option<&str>) -> String {
    match (kind, world) {
        (ResourceKind::DataPacks, Some(world)) => resources::world_area(world),
        _ => resources::area(kind),
    }
}

pub(crate) fn plan_install(state: &State, request: &InstallRequest) -> Result<InstallPlan> {
    let (game_version, loader) = describe(state, &request.instance)?;
    let idx = catalog::find(&request.project)?;
    let entry = &ENTRIES[idx];
    if entry.provider != request.provider {
        return Err(Error::not_found(format!(
            "project {} on {}",
            request.project,
            catalog::provider_name(request.provider)
        )));
    }
    catalog::check_key(state, entry.provider)?;
    let kind = kind_of(entry.kind)?;
    match (kind, request.world.as_deref()) {
        (ResourceKind::DataPacks, None) => {
            return Err(Error::invalid_input("a data pack needs a world"));
        }
        (ResourceKind::DataPacks, Some(world)) => resources::check_world(&request.instance, world)?,
        (_, Some(_)) => return Err(Error::invalid_input("only data packs go into a world")),
        _ => {}
    }
    let v = catalog::pick_version(idx, request.version.as_deref(), request.min_stability)?;
    let mut plan = InstallPlan {
        game_version,
        loader: loader.clone(),
        ..InstallPlan::default()
    };
    if kind == ResourceKind::Mods && loader.is_none() {
        plan.warnings
            .push("the instance has no mod loader; the mod will not load".into());
    }

    // The required library first.
    let library = catalog::library_of(entry.provider);
    if kind == ResourceKind::Mods && idx != library {
        let mods = resources::snapshot(state, &request.instance, &resources::area(kind));
        let present = mods
            .iter()
            .find(|e| catalog::find_by_file(&e.file).is_some_and(|(i, _)| i == library));
        match present {
            Some(file) => {
                let version = catalog::find_by_file(&file.file).map(|(_, v)| VERSIONS[v].0.into());
                plan.satisfied.push(Satisfied {
                    project_id: catalog::id_of(library),
                    slug: ENTRIES[library].slug.into(),
                    title: ENTRIES[library].title.into(),
                    file: file.file.clone(),
                    version,
                    matched_by: MatchedBy::Origin,
                });
            }
            None => plan.items.push(item(
                library,
                catalog::newest(vl_core::install::MinStability::Release),
                PlanRole::Dependency {
                    required_by: entry.title.into(),
                },
                PlanAction::Install,
            )?),
        }
    }

    let area = area_for(kind, request.world.as_deref());
    let files = resources::snapshot(state, &request.instance, &area);
    let existing = files.iter().find_map(|e| {
        catalog::find_by_file(&e.file)
            .filter(|(i, _)| *i == idx)
            .map(|(_, old)| (e, old))
    });
    let action = match existing {
        Some((file, old)) if old == v => PlanAction::Present {
            file: file.file.clone(),
        },
        Some((file, old)) => PlanAction::Replace {
            file: file.file.clone(),
            old_version: Some(VERSIONS[old].0.into()),
        },
        None => PlanAction::Install,
    };
    plan.items.push(item(idx, v, PlanRole::Requested, action)?);

    if idx == 1 {
        let suggested = 9;
        plan.recommended.push(Recommendation {
            project_id: catalog::id_of(suggested),
            slug: Some(ENTRIES[suggested].slug.into()),
            title: Some(ENTRIES[suggested].title.into()),
            suggested_by: entry.title.into(),
        });
    }
    Ok(plan)
}

/// A task stage per downloaded item.
fn stages(plan: &InstallPlan) -> Vec<(String, u64)> {
    let mut stages: Vec<(String, u64)> = plan
        .downloads()
        .map(|i| (format!("download {}", i.slug), 4))
        .collect();
    stages.push(("verify".into(), 1));
    stages
}

/// Puts the plan's files into the instance's folders.
fn apply(state: &mut State, instance: &InstanceId, world: Option<&str>, plan: &InstallPlan) {
    for planned in plan.downloads() {
        let area = area_for(planned.kind, world);
        let files = resources::folder(state, instance, &area);
        let mut enabled = true;
        if let PlanAction::Replace { file, .. } = &planned.action {
            if let Some(old) = files.iter().find(|e| &e.file == file) {
                enabled = old.enabled;
            }
            files.retain(|e| &e.file != file);
        }
        files.push(resources::make_entry(
            planned.kind,
            &planned.file.filename,
            enabled,
        ));
    }
}

fn run_plan(
    inner: &Arc<Inner>,
    plan: InstallPlan,
    instance: InstanceId,
    world: Option<String>,
    subject: String,
) -> Result<Response> {
    if plan.downloads().next().is_none() {
        return Err(Error::conflict("nothing to install: all files are present"));
    }
    let stages = stages(&plan);
    sim::staged(
        inner,
        TaskKind::InstallResource,
        Some(subject),
        stages,
        move |inner| {
            apply(&mut inner.lock(), &instance, world.as_deref(), &plan);
            Ok(())
        },
    )
}

pub(crate) fn install(inner: &Arc<Inner>, request: &InstallRequest) -> Result<Response> {
    let plan = plan_install(&inner.lock(), request)?;
    let subject = plan
        .items
        .iter()
        .find(|i| i.role == PlanRole::Requested)
        .map_or_else(String::new, |i| i.title.clone());
    run_plan(
        inner,
        plan,
        request.instance.clone(),
        request.world.clone(),
        subject,
    )
}

/// The folders the check looks at, with their keys.
fn checked_areas(world: Option<&str>) -> Vec<(ResourceKind, String)> {
    match world {
        Some(world) => vec![(ResourceKind::DataPacks, resources::world_area(world))],
        None => CHECKED.iter().map(|k| (*k, resources::area(*k))).collect(),
    }
}

/// The least stable channel an update of a file at version `current` may come from: the
/// request's channel, else the instance's setting widened to the installed version's channel.
fn floor(state: &State, request: &UpdateCheckRequest, current: usize) -> MinStability {
    match request.min_stability {
        Some(min) => min,
        None => {
            let overrides = state
                .overrides
                .get(&request.instance)
                .cloned()
                .unwrap_or_default();
            let setting = state.settings.with_overrides(&overrides).updates.channel;
            VERSIONS
                .get(current)
                .map_or(setting, |(_, channel, _)| setting.allowing(*channel))
        }
    }
}

pub(crate) fn check_updates(state: &State, request: &UpdateCheckRequest) -> Result<UpdateReport> {
    let (game_version, loader) = describe(state, &request.instance)?;
    if let Some(provider) = request.provider {
        catalog::check_key(state, provider)?;
    }
    if let Some(world) = &request.world {
        resources::check_world(&request.instance, world)?;
    }
    let mut key_missing = false;
    let mut report = UpdateReport {
        game_version,
        loader: loader.clone(),
        ..UpdateReport::default()
    };
    for (kind, area) in checked_areas(request.world.as_deref()) {
        let files = resources::snapshot(state, &request.instance, &area);
        // Two files of one project: a conflict, neither is offered an update.
        let mut by_project: Vec<(usize, Vec<String>)> = Vec::new();
        for file in &files {
            if let Some((idx, _)) = catalog::find_by_file(&file.file) {
                match by_project.iter_mut().find(|(i, _)| *i == idx) {
                    Some((_, names)) => names.push(file.file.clone()),
                    None => by_project.push((idx, vec![file.file.clone()])),
                }
            }
        }
        let first_conflict = report.conflicts.len();
        for (idx, mut names) in by_project {
            if names.len() > 1 {
                names.sort();
                report.conflicts.push(UpdateConflict {
                    kind,
                    provider: ENTRIES[idx].provider,
                    project_id: catalog::id_of(idx),
                    files: names,
                });
            }
        }
        for file in files {
            let unchecked = |reason| UncheckedFile {
                kind,
                file: file.file.clone(),
                reason,
            };
            let Some((idx, current)) = catalog::find_by_file(&file.file) else {
                report.unchecked.push(unchecked(UncheckedReason::NoOrigin));
                continue;
            };
            let conflicting = report.conflicts[first_conflict..]
                .iter()
                .any(|c| c.files.contains(&file.file));
            if conflicting {
                report
                    .unchecked
                    .push(unchecked(UncheckedReason::SameProject));
                continue;
            }
            let newest = catalog::newest(floor(state, request, current));
            let skipped = request.provider.is_none()
                && catalog::check_key(state, ENTRIES[idx].provider).is_err();
            if skipped {
                key_missing = true;
                report
                    .unchecked
                    .push(unchecked(UncheckedReason::OtherPlatform));
            } else if kind == ResourceKind::Mods && loader.is_none() {
                report.unchecked.push(unchecked(UncheckedReason::NoLoader));
            } else if request.provider.is_some_and(|p| p != ENTRIES[idx].provider) {
                report
                    .unchecked
                    .push(unchecked(UncheckedReason::OtherPlatform));
            } else if packs::is_pack_file(state, &request.instance, idx)
                && !request.include_pack_files
            {
                report.from_pack.push(PackFile {
                    kind,
                    file: file.file.clone(),
                });
            } else if newest < current {
                report.updates.push(UpdateCandidate {
                    kind,
                    file: file.file.clone(),
                    enabled: file.enabled,
                    name: ENTRIES[idx].title.into(),
                    provider: ENTRIES[idx].provider,
                    project_id: catalog::id_of(idx),
                    current: catalog::label(idx, current),
                    latest: catalog::label(idx, newest),
                    from_pack: packs::is_pack_file(state, &request.instance, idx),
                });
            } else {
                report.up_to_date += 1;
            }
        }
    }
    if key_missing {
        report
            .warnings
            .push("CurseForge was not checked: no API key is set".into());
    }
    Ok(report)
}

fn matches(candidate: &UpdateCandidate, key: &str) -> bool {
    candidate.file == key
        || catalog::find(key).is_ok_and(|idx| catalog::id_of(idx) == candidate.project_id)
}

/// Whether `key` names a file of the instance, or the project of one.
fn known(state: &State, instance: &InstanceId, world: Option<&str>, key: &str) -> bool {
    checked_areas(world).iter().any(|(_, area)| {
        resources::snapshot(state, instance, area).iter().any(|e| {
            e.file == key
                || catalog::find(key)
                    .is_ok_and(|idx| catalog::find_by_file(&e.file).is_some_and(|(i, _)| i == idx))
        })
    })
}

pub(crate) fn plan_update(state: &State, request: &UpdateRequest) -> Result<InstallPlan> {
    let check = &request.check;
    let report = check_updates(state, check)?;
    for key in &request.files {
        if !known(state, &check.instance, check.world.as_deref(), key) {
            return Err(Error::not_found(format!("file or project {key}")));
        }
    }
    let mut plan = InstallPlan {
        game_version: report.game_version.clone(),
        loader: report.loader.clone(),
        warnings: report.warnings.clone(),
        ..InstallPlan::default()
    };
    if let Some(version) = &request.version {
        let [key] = request.files.as_slice() else {
            return Err(Error::invalid_input(
                "switching versions needs exactly one file",
            ));
        };
        for (_, area) in checked_areas(check.world.as_deref()) {
            for file in resources::snapshot(state, &check.instance, &area) {
                let Some((idx, current)) = catalog::find_by_file(&file.file) else {
                    continue;
                };
                if file.file != *key && !catalog::find(key).is_ok_and(|k| k == idx) {
                    continue;
                }
                let min = floor(state, check, current);
                let target = catalog::pick_version(idx, Some(version), min)?;
                if target == current {
                    return Err(Error::conflict(format!(
                        "{} is already that version",
                        file.file
                    )));
                }
                plan.items.push(item(
                    idx,
                    target,
                    PlanRole::Requested,
                    PlanAction::Replace {
                        file: file.file.clone(),
                        old_version: Some(VERSIONS[current].0.into()),
                    },
                )?);
                return Ok(plan);
            }
        }
        return Err(Error::not_found(format!("file or project {key}")));
    }
    if !request.all && request.files.is_empty() {
        return Err(Error::invalid_input(
            "name the files to update or ask for all",
        ));
    }
    for candidate in &report.updates {
        if request.all || request.files.iter().any(|key| matches(candidate, key)) {
            let (idx, current) = catalog::find_by_file(&candidate.file)
                .ok_or_else(|| Error::internal("candidate without a catalog file"))?;
            let latest = catalog::newest(floor(state, check, current));
            plan.items.push(item(
                idx,
                latest,
                PlanRole::Requested,
                PlanAction::Replace {
                    file: candidate.file.clone(),
                    old_version: Some(VERSIONS[current].0.into()),
                },
            )?);
        }
    }
    Ok(plan)
}

pub(crate) fn update(inner: &Arc<Inner>, request: &UpdateRequest) -> Result<Response> {
    let (plan, name) = {
        let state = inner.lock();
        let plan = plan_update(&state, request)?;
        let name = resources::usable(&state, &request.check.instance)?
            .name
            .clone();
        (plan, name)
    };
    if plan.items.is_empty() {
        return Err(Error::conflict("nothing to update"));
    }
    run_plan(
        inner,
        plan,
        request.check.instance.clone(),
        request.check.world.clone(),
        name,
    )
}

/// Files the catalog recognizes already have a record in the mock; the rest stays unknown.
pub(crate) fn identify(
    state: &State,
    instance: &InstanceId,
    world: Option<&str>,
) -> Result<IdentifyReport> {
    resources::usable(state, instance)?;
    if let Some(world) = world {
        resources::check_world(instance, world)?;
    }
    let mut report = IdentifyReport::default();
    for (kind, area) in checked_areas(world) {
        for file in resources::snapshot(state, instance, &area) {
            if catalog::find_by_file(&file.file).is_some() {
                report.already_known += 1;
            } else {
                report.unknown.push(PackFile {
                    kind,
                    file: file.file,
                });
            }
        }
    }
    Ok(report)
}
