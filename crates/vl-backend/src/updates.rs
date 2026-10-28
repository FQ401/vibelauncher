//! Update checks, updates and origin discovery for the files of an instance.
//!
//! Checks and plans are answered from a worker thread. An update runs as an `InstallResource`
//! task: it plans again when it starts, downloads the new files and swaps them in all together
//! (the folders are put back if any step fails); disabled files stay disabled.

use std::sync::Arc;
use std::thread;

use vl_core::catalog::Provider;
use vl_core::install::{UpdateCheckRequest, UpdateRequest};
use vl_core::log::{LogLevel, LogLine, LogSource};
use vl_core::task::TaskKind;
use vl_core::{Error, Event, InstanceId, Replier, Response, Result};
use vl_install::{ApplyInput, Selection, UpdateInput, UpdateOptions};
use vl_net::Control;
use vl_tasks::TaskSpec;

use crate::Backend;
use crate::catalog::{Job, context};

fn options(check: &UpdateCheckRequest) -> UpdateOptions {
    UpdateOptions {
        min: check.min_stability,
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
    let (job, _) = match context(backend, &request.instance) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("catalog-updates", replier, move || {
        let control = Control::default();
        let input = input(&job, request.provider, &control)?;
        vl_install::check(&input, options(&request)).map(Response::UpdateReport)
    });
}

pub(crate) fn plan(backend: &Backend, request: UpdateRequest, replier: Replier) {
    let (job, _) = match context(backend, &request.check.instance) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("catalog-update-plan", replier, move || {
        let control = Control::default();
        let input = input(&job, request.check.provider, &control)?;
        vl_install::plan_update(
            &input,
            options(&request.check),
            selection(&request)?,
            request.version.as_deref(),
        )
        .map(Response::InstallPlan)
    });
}

pub(crate) fn update(backend: &Backend, request: UpdateRequest) -> Result<Response> {
    selection(&request)?;
    let (job, name) = context(backend, &request.check.instance)?;
    let events = Arc::clone(&backend.events);
    let handle = backend.tasks().spawn(
        TaskSpec::new(TaskKind::InstallResource).subject(&name),
        move |ctx| {
            crate::launch::with_job(ctx, |wired| {
                let control = crate::java::job_control(wired);
                let plan = {
                    let input = input(&job, request.check.provider, &control)?;
                    vl_install::plan_update(
                        &input,
                        options(&request.check),
                        selection(&request)?,
                        request.version.as_deref(),
                    )?
                };
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
    provider: Provider,
    replier: Replier,
) {
    let (job, _) = match context(backend, instance) {
        Ok(j) => j,
        Err(e) => return replier.send(Err(e)),
    };
    spawn_reply("catalog-identify", replier, move || {
        let control = Control::default();
        let input = input(&job, provider, &control)?;
        vl_install::identify(&input).map(Response::Identified)
    });
}
