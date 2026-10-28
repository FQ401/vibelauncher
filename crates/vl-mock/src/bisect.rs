//! Synthetic mod bisection: a manual session that narrows the mods of an instance down by the
//! user's reports, and an automatic search that runs as a task and finds a fixed culprit.

use std::sync::Arc;

use vl_core::bisect::{
    AutoBisectReport, AutoBisectRequest, AutoVerdict, BisectCommand, BisectEvent, BisectOutcome,
    BisectRun, BisectState, BisectStatus, BisectVerdict, RunPurpose, RunResult,
};
use vl_core::resources::ResourceKind;
use vl_core::task::{TaskKind, TaskOutcome};
use vl_core::{Error, Event, InstanceId, Response, Result};

use crate::catalog::{self, ENTRIES};
use crate::{Inner, State, resources, sim};

/// The mod every automatic search finds when the instance has it.
const CULPRIT_SLUG: &str = "mock-weather-fx";

/// A manual session.
#[derive(Debug, Clone)]
pub(crate) struct Session {
    /// Every file of the mods folder and whether it was enabled when the session started.
    original: Vec<(String, bool)>,
    /// The files under test.
    candidates: Vec<String>,
    /// Files that ran without the problem.
    good: Vec<String>,
    /// Files the cause is narrowed down to.
    suspects: Vec<String>,
    /// The suspects enabled in the current step.
    tested: Vec<String>,
    steps_done: u32,
    verdict: Option<BisectVerdict>,
}

/// About how many halvings `n` suspects still need.
fn estimate(n: usize) -> u32 {
    if n < 2 {
        0
    } else {
        usize::BITS - (n - 1).leading_zeros()
    }
}

fn mod_states(state: &State, id: &InstanceId) -> Vec<(String, bool)> {
    let mut mods: Vec<_> = resources::snapshot(state, id, &resources::area(ResourceKind::Mods))
        .into_iter()
        .map(|e| (e.file, e.enabled))
        .collect();
    mods.sort();
    mods
}

/// The candidates of a search: the enabled mods.
fn candidates(state: &State, id: &InstanceId) -> Result<Vec<String>> {
    resources::usable(state, id)?;
    let found: Vec<String> = mod_states(state, id)
        .into_iter()
        .filter_map(|(file, enabled)| enabled.then_some(file))
        .collect();
    if found.len() < 2 {
        return Err(Error::invalid_input(
            "bisecting needs at least two enabled mods",
        ));
    }
    Ok(found)
}

fn set_mods(state: &mut State, id: &InstanceId, wanted: impl Fn(&str) -> Option<bool>) {
    for entry in resources::folder(state, id, &resources::area(ResourceKind::Mods)) {
        if let Some(enabled) = wanted(&entry.file) {
            entry.enabled = enabled;
        }
    }
}

/// Applies the current step: the good files and the tested suspects run, other candidates not.
fn apply_step(state: &mut State, id: &InstanceId, session: &Session) {
    set_mods(state, id, |file| {
        session.candidates.iter().any(|c| c == file).then(|| {
            session.good.iter().any(|g| g == file) || session.tested.iter().any(|t| t == file)
        })
    });
}

fn restore(state: &mut State, id: &InstanceId, session: &Session) {
    set_mods(state, id, |file| {
        session
            .original
            .iter()
            .find(|(f, _)| f == file)
            .map(|(_, enabled)| *enabled)
    });
}

fn status(session: &Session) -> BisectStatus {
    let finished = session.verdict.is_some();
    let enabled: Vec<String> = if finished {
        Vec::new()
    } else {
        session
            .candidates
            .iter()
            .filter(|c| session.good.contains(c) || session.tested.contains(c))
            .cloned()
            .collect()
    };
    BisectStatus {
        state: if finished {
            BisectState::Finished
        } else {
            BisectState::Running
        },
        steps_done: session.steps_done,
        steps_estimate: if finished {
            0
        } else {
            estimate(session.suspects.len())
        },
        candidates: session.candidates.len() as u32,
        suspects: session.suspects.clone(),
        disabled: if finished {
            0
        } else {
            (session.candidates.len() - enabled.len()) as u32
        },
        enabled,
        verdict: session.verdict.clone(),
    }
}

/// The library files the culprits need.
fn needs(candidates: &[String], culprits: &[String]) -> Vec<String> {
    let mut needs = Vec::new();
    for culprit in culprits {
        let Some((idx, _)) = catalog::find_by_file(culprit) else {
            continue;
        };
        let library = catalog::library_of(ENTRIES[idx].provider);
        if idx == library {
            continue;
        }
        needs.extend(
            candidates
                .iter()
                .filter(|c| catalog::find_by_file(c).is_some_and(|(i, _)| i == library))
                .cloned(),
        );
    }
    needs
}

fn next_step(session: &mut Session) {
    let half = session.suspects.len() / 2;
    session.tested = session.suspects.iter().take(half).cloned().collect();
}

fn finish(state: &mut State, id: &InstanceId, session: &mut Session, verdict: BisectVerdict) {
    session.verdict = Some(verdict);
    session.tested.clear();
    restore(state, id, session);
}

fn report(state: &mut State, id: &InstanceId, outcome: BisectOutcome) -> Result<BisectStatus> {
    let mut session = state
        .bisects
        .remove(id)
        .ok_or_else(|| Error::not_found(format!("bisection of {id}")))?;
    if session.verdict.is_some() {
        state.bisects.insert(id.clone(), session);
        return Err(Error::conflict(format!(
            "the bisection of {id} is finished"
        )));
    }
    session.steps_done += 1;
    match outcome {
        BisectOutcome::Bad => session.suspects = std::mem::take(&mut session.tested),
        BisectOutcome::Good => {
            let tested = std::mem::take(&mut session.tested);
            session.suspects.retain(|s| !tested.contains(s));
            session.good.extend(tested);
        }
        _ => {
            let suspects = session.suspects.clone();
            finish(
                state,
                id,
                &mut session,
                BisectVerdict::Inconclusive { suspects },
            );
        }
    }
    if session.verdict.is_none() {
        if session.suspects.len() == 1 {
            let culprits = session.suspects.clone();
            let needs = needs(&session.candidates, &culprits);
            finish(
                state,
                id,
                &mut session,
                BisectVerdict::Found { culprits, needs },
            );
        } else {
            next_step(&mut session);
            apply_step(state, id, &session);
        }
    }
    let result = status(&session);
    state.bisects.insert(id.clone(), session);
    Ok(result)
}

fn start(state: &mut State, id: &InstanceId) -> Result<BisectStatus> {
    if state.bisects.get(id).is_some_and(|s| s.verdict.is_none()) {
        return Err(Error::conflict(format!("{id} has an unfinished bisection")));
    }
    let candidates = candidates(state, id)?;
    let mut session = Session {
        original: mod_states(state, id),
        good: Vec::new(),
        suspects: candidates.clone(),
        candidates,
        tested: Vec::new(),
        steps_done: 0,
        verdict: None,
    };
    next_step(&mut session);
    apply_step(state, id, &session);
    let result = status(&session);
    state.bisects.insert(id.clone(), session);
    Ok(result)
}

/// The runs and the report of a search whose culprit is `culprit`.
fn simulate(request: &AutoBisectRequest, candidates: &[String], culprit: &str) -> AutoBisectReport {
    let n = candidates.len() as u32;
    let clean = request.stable_secs + 18;
    let crash = || Some("mock-render-null-pointer".to_owned());
    let mut runs = vec![
        BisectRun {
            purpose: RunPurpose::Reproduce,
            enabled: n,
            result: RunResult::Crashed,
            crash: crash(),
            seconds: 14,
        },
        BisectRun {
            purpose: RunPurpose::WithoutMods,
            enabled: 0,
            result: RunResult::Reached,
            crash: None,
            seconds: clean,
        },
    ];
    let mut suspects: Vec<&str> = candidates.iter().map(String::as_str).collect();
    let mut good = 0;
    while suspects.len() > 1 {
        let half = suspects.len() / 2;
        let crashed = suspects.iter().take(half).any(|s| *s == culprit);
        runs.push(BisectRun {
            purpose: RunPurpose::Step,
            enabled: good + half as u32,
            result: if crashed {
                RunResult::Crashed
            } else {
                RunResult::Reached
            },
            crash: if crashed { crash() } else { None },
            seconds: if crashed { 14 } else { clean },
        });
        if crashed {
            suspects.truncate(half);
        } else {
            good += half as u32;
            suspects.drain(..half);
        }
    }
    runs.push(BisectRun {
        purpose: RunPurpose::Verify,
        enabled: n - 1,
        result: RunResult::Reached,
        crash: None,
        seconds: clean,
    });
    let culprits = vec![culprit.to_owned()];
    AutoBisectReport {
        verdict: AutoVerdict::Found {
            needs: needs(candidates, &culprits),
            culprits,
            confirmed: true,
        },
        reference: None,
        semi_manual: false,
        runs,
    }
}

fn auto(inner: &Arc<Inner>, request: AutoBisectRequest) -> Result<Response> {
    let (name, candidates) = {
        let state = inner.lock();
        let candidates = candidates(&state, &request.instance)?;
        if state
            .bisects
            .get(&request.instance)
            .is_some_and(|s| s.verdict.is_none())
        {
            return Err(Error::conflict("a manual bisection is running"));
        }
        let name = resources::usable(&state, &request.instance)?.name.clone();
        (name, candidates)
    };
    let culprit = candidates
        .iter()
        .find(|c| catalog::find_by_file(c).is_some_and(|(i, _)| ENTRIES[i].slug == CULPRIT_SLUG))
        .or_else(|| candidates.get(candidates.len() / 2))
        .cloned()
        .unwrap_or_default();
    let report = simulate(&request, &candidates, &culprit);
    let task = sim::Task::start(inner, TaskKind::Bisect, Some(name));
    sim::spawn(task, move |task| {
        let total = report.runs.len() as u64;
        let mut steps_done = 0;
        let mut suspects = candidates.len();
        for (n, run) in report.runs.iter().enumerate() {
            if task.cancelled() {
                task.finish(TaskOutcome::Cancelled);
                return;
            }
            sim::pause(&task.inner);
            task.progress(&format!("run {}", n + 1), n as u64 + 1, total);
            if run.purpose == RunPurpose::Step {
                steps_done += 1;
                // A crash keeps the first half, a clean run the second: the larger one is kept.
                suspects = suspects.div_ceil(2);
            }
            task.inner.publish(Event::Bisect(BisectEvent::Run {
                instance: request.instance.clone(),
                run: run.clone(),
                steps_done,
                steps_estimate: estimate(suspects),
            }));
        }
        task.inner
            .lock()
            .bisect_reports
            .insert(request.instance.clone(), report.clone());
        task.inner.publish(Event::Bisect(BisectEvent::Finished {
            instance: request.instance.clone(),
            report: Box::new(report),
        }));
        task.finish(TaskOutcome::Succeeded);
    })
}

pub(crate) fn handle(inner: &Arc<Inner>, command: BisectCommand) -> Result<Response> {
    match command {
        BisectCommand::Start { instance, .. } => {
            start(&mut inner.lock(), &instance).map(Response::Bisect)
        }
        BisectCommand::Status { instance } => inner
            .lock()
            .bisects
            .get(&instance)
            .map(|s| Response::Bisect(status(s)))
            .ok_or_else(|| Error::not_found(format!("bisection of {instance}"))),
        BisectCommand::Report { instance, outcome } => {
            report(&mut inner.lock(), &instance, outcome).map(Response::Bisect)
        }
        BisectCommand::Stop { instance } => {
            let mut state = inner.lock();
            let session = state
                .bisects
                .remove(&instance)
                .ok_or_else(|| Error::not_found(format!("bisection of {instance}")))?;
            if session.verdict.is_none() {
                restore(&mut state, &instance, &session);
            }
            Ok(Response::Done)
        }
        BisectCommand::Auto(request) => auto(inner, request),
        BisectCommand::AutoReport { instance } => inner
            .lock()
            .bisect_reports
            .get(&instance)
            .cloned()
            .map(Response::BisectReport)
            .ok_or_else(|| Error::not_found(format!("bisection report of {instance}"))),
        other => Err(Error::unsupported(format!(
            "vl-mock does not simulate {other:?}"
        ))),
    }
}
