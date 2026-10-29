//! Update checks, updates and origin discovery for the files of an instance.
//!
//! A check, plan or update names a platform, or none: then Modrinth and CurseForge are asked
//! together (`vl_install::check_all`), each file going to the platform of its origin record,
//! else to the first that knows it, so nothing is offered twice. CurseForge is left out with a
//! warning while no key is set.
//!
//! The channel an update may come from is the request's when it names one; otherwise the
//! instance's `updates.channel` setting (its override, else the global value), widened file by
//! file to the channel of the installed version.
//!
//! Checks and plans are answered from a worker thread. An update runs as an `InstallResource`
//! task: it plans again when it starts, downloads the new files and swaps them in all together
//! (the folders are put back if any step fails); disabled files stay disabled.

use std::sync::Arc;
use std::thread;

use vl_core::catalog::Provider;
use vl_core::install::{
    IdentifyReport, InstallPlan, UpdateCheckRequest, UpdateReport, UpdateRequest,
};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::task::TaskKind;
use vl_core::{Error, ErrorKind, Event, InstanceId, Replier, Response, Result};
use vl_install::{ApplyInput, Selection, UpdateInput, UpdateOptions};
use vl_net::Control;
use vl_tasks::TaskSpec;

use crate::Backend;
use crate::catalog::{Job, context};

/// How a check of `check` chooses: the channel it names, else the instance's setting followed
/// by each file's installed channel.
fn options(backend: &Backend, check: &UpdateCheckRequest) -> UpdateOptions {
    let (min, follow_installed) = match check.min_stability {
        Some(min) => (min, false),
        None => (
            backend
                .settings()
                .effective(&check.instance)
                .value
                .updates
                .channel,
            true,
        ),
    };
    UpdateOptions {
        min,
        follow_installed,
        include_pack_files: check.include_pack_files,
    }
}

fn input<'a>(job: &'a Job, provider: Provider, control: &'a Control) -> Result<UpdateInput<'a>> {
    Ok(UpdateInput {
        source: job.platforms.source(provider)?,
        resources: &job.resources,
        origins: &job.origins,
        target: &job.target,
        meta_dir: &job.meta,
        control,
    })
}

/// The platforms a check asks, in order: the one named, else Modrinth then CurseForge. Without
/// a CurseForge key CurseForge is left out (no file is fingerprinted for nothing) and the
/// returned note says so.
fn inputs<'a>(
    job: &'a Job,
    provider: Option<Provider>,
    control: &'a Control,
) -> Result<(Vec<UpdateInput<'a>>, Option<String>)> {
    if let Some(provider) = provider {
        return Ok((vec![input(job, provider, control)?], None));
    }
    let mut inputs = vec![input(job, Provider::Modrinth, control)?];
    let mut note = None;
    if job.platforms.curseforge().has_key()? {
        inputs.push(input(job, Provider::CurseForge, control)?);
    } else {
        note = Some("CurseForge was not checked: no CurseForge API key is set".to_owned());
    }
    Ok((inputs, note))
}

/// Checks on the platform of the request (all of them when it names none).
fn check_on(
    job: &Job,
    request: &UpdateCheckRequest,
    options: UpdateOptions,
    control: &Control,
) -> Result<UpdateReport> {
    let (inputs, note) = inputs(job, request.provider, control)?;
    let mut report = vl_install::check_all(&inputs, options)?;
    report.warnings.extend(note);
    Ok(report)
}

/// Plans an update on the platform of the request (all of them when it names none).
fn plan_on(
    job: &Job,
    request: &UpdateRequest,
    options: UpdateOptions,
    control: &Control,
) -> Result<InstallPlan> {
    let (inputs, note) = inputs(job, request.check.provider, control)?;
    let mut plan = vl_install::plan_update_all(
        &inputs,
        options,
        selection(request)?,
        request.version.as_deref(),
    )?;
    plan.warnings.extend(note);
    Ok(plan)
}

fn selection(request: &UpdateRequest) -> Result<Selection<'_>> {
    match (request.all, request.files.is_empty()) {
        (true, true) => Ok(Selection::All),
        (false, false) => Ok(Selection::Files(&request.files)),
        (true, false) => Err(Error::invalid_input("choose all files or some, not both")),
        (false, true) => Err(Error::invalid_input(
            "name the files to update, or ask for all",
        )),
    }
}

pub(crate) fn spawn_reply(
    name: &str,
    replier: Replier,
    work: impl FnOnce() -> Result<Response> + Send + 'static,
) {
    let spawned = thread::Builder::new()
        .name(name.into())
        .spawn(move || replier.send(work()));
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _: std::result::Result<_, Error> = spawned.map_err(|e| Error::internal(e.to_string()));
}

pub(crate) fn check(backend: &Backend, request: UpdateCheckRequest, replier: Replier) {
    let (job, _) = match context(backend, &request.instance, request.world.as_deref()) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    let options = options(backend, &request);
    spawn_reply("catalog-updates", replier, move || {
        let control = Control::default();
        check_on(&job, &request, options, &control).map(Response::UpdateReport)
    });
}

pub(crate) fn plan(backend: &Backend, request: UpdateRequest, replier: Replier) {
    let (job, _) = match context(
        backend,
        &request.check.instance,
        request.check.world.as_deref(),
    ) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    let options = options(backend, &request.check);
    spawn_reply("catalog-update-plan", replier, move || {
        let control = Control::default();
        plan_on(&job, &request, options, &control).map(Response::InstallPlan)
    });
}

pub(crate) fn update(backend: &Backend, request: UpdateRequest) -> Result<Response> {
    selection(&request)?;
    let (job, name) = context(
        backend,
        &request.check.instance,
        request.check.world.as_deref(),
    )?;
    let options = options(backend, &request.check);
    let events = Arc::clone(&backend.events);
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::InstallResource).subject(&name),
        move |ctx| {
            crate::launch::with_job(ctx, |wired| {
                let control = crate::java::job_control(wired);
                let plan = plan_on(&job, &request, options, &control)?;
                wired.check()?;
                let apply = ApplyInput {
                    client: &job.client,
                    resources: &job.resources,
                    origins: &job.origins,
                    staging: &job.staging,
                };
                let report = vl_install::execute(&apply, &plan, &control)?;
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

pub(crate) fn identify(
    backend: &Backend,
    instance: &InstanceId,
    provider: Option<Provider>,
    world: Option<&str>,
    replier: Replier,
) {
    let (job, _) = match context(backend, instance, world) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("catalog-identify", replier, move || {
        let control = Control::default();
        identify_on(&job, provider, &control).map(Response::Identified)
    });
}

/// Identifies on the platform named, else on Modrinth and then on CurseForge for the files
/// Modrinth did not identify (a file both know is recorded as Modrinth's). CurseForge is left
/// out with a warning while no key is set, so no file is fingerprinted for nothing, and when it
/// cannot be reached; what Modrinth identified stays recorded.
fn identify_on(job: &Job, provider: Option<Provider>, control: &Control) -> Result<IdentifyReport> {
    if let Some(provider) = provider {
        return vl_install::identify(&input(job, provider, control)?);
    }
    let modrinth = vl_install::identify(&input(job, Provider::Modrinth, control)?)?;
    let skipped = |why: &str| {
        let mut report = modrinth.clone();
        report
            .warnings
            .push(format!("CurseForge was not checked: {why}"));
        Ok(report)
    };
    if !job.platforms.curseforge().has_key()? {
        return skipped("no CurseForge API key is set");
    }
    let curseforge = match vl_install::identify(&input(job, Provider::CurseForge, control)?) {
        Ok(report) => report,
        Err(e)
            if matches!(
                e.kind,
                ErrorKind::NoApiKey | ErrorKind::Auth | ErrorKind::Network
            ) =>
        {
            return skipped(&e.message);
        }
        Err(e) => return Err(e),
    };
    // The second pass counts what the first one recorded as already known and repeats its
    // damaged-records warning.
    let found = modrinth.identified.len();
    let mut warnings = modrinth.warnings;
    for w in curseforge.warnings {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let mut identified = modrinth.identified;
    identified.extend(curseforge.identified);
    Ok(IdentifyReport {
        identified,
        already_known: curseforge
            .already_known
            .saturating_sub(u32::try_from(found).unwrap_or(u32::MAX)),
        unknown: curseforge.unknown,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BackendConfig;
    use vl_core::install::MinStability;
    use vl_core::settings::{Overrides, Settings, UpdateSettings};

    #[test]
    fn the_channel_comes_from_the_request_else_from_the_settings() {
        let dir = std::env::temp_dir().join(format!("vl-backend-updates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let a = InstanceId::new("a").unwrap();
        let b = InstanceId::new("b").unwrap();
        let mut global = Settings::default();
        global.updates.channel = MinStability::Beta;
        backend.settings().set_global(&global).unwrap();
        let overrides = Overrides {
            updates: Some(UpdateSettings {
                channel: MinStability::Alpha,
                ..UpdateSettings::default()
            }),
            ..Overrides::default()
        };
        backend.settings().set_overrides(&a, &overrides).unwrap();

        let from_settings = options(&backend, &UpdateCheckRequest::new(a.clone()));
        assert_eq!(from_settings.min, MinStability::Alpha);
        assert!(from_settings.follow_installed);
        assert_eq!(
            options(&backend, &UpdateCheckRequest::new(b)).min,
            MinStability::Beta
        );
        let named = UpdateCheckRequest {
            min_stability: Some(MinStability::Release),
            ..UpdateCheckRequest::new(a)
        };
        let named = options(&backend, &named);
        assert_eq!(named.min, MinStability::Release);
        assert!(!named.follow_installed);
        drop(backend);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
