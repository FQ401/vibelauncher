//! `instances bisect auto …`: the search with the launcher starting the game for every step.
//!
//! Follows the `Bisect` task and prints one line per run; Ctrl+C cancels the task, which stops
//! the game and restores the mods and the world.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use vl_core::bisect::{
    AutoBisectReport, AutoBisectRequest, AutoVerdict, BisectCommand, BisectEvent, BisectRun,
    BisectTarget, RunPurpose, RunResult,
};
use vl_core::crash::CrashDiagnosis;
use vl_core::task::{TaskCommand, TaskOutcome};
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

const POLL: Duration = Duration::from_millis(100);

static INTERRUPTS: AtomicU32 = AtomicU32::new(0);

/// `auto <instance> [flags]`.
pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let [instance, flags @ ..] = args else {
        return Err(USAGE.into());
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let request = parse(instance.clone(), flags)?;
    let _ = ctrlc::set_handler(|| {
        INTERRUPTS.fetch_add(1, Ordering::SeqCst);
    });
    let seen = INTERRUPTS.load(Ordering::SeqCst);
    let mut cancelled = false;
    let events = facade.subscribe();
    let task = match call(facade, Command::Bisect(BisectCommand::Auto(request)))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut progress = ProgressPrinter::new(task, "bisect");
    let mut report: Option<AutoBisectReport> = None;
    loop {
        if INTERRUPTS.load(Ordering::SeqCst) > seen && !cancelled {
            cancelled = call(facade, Command::Tasks(TaskCommand::Cancel { id: task })).is_ok();
            if cancelled {
                writeln!(
                    out,
                    "[vl-cli] cancelling: the mods and the world are restored"
                )?;
            }
        }
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(event) => match progress.handle(&event, out)? {
                Some(TaskOutcome::Succeeded) => break,
                Some(TaskOutcome::Cancelled) => {
                    writeln!(out, "[bisect] cancelled; mods and world restored")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                None => {}
            },
            Event::Bisect(BisectEvent::Run {
                instance: i,
                run,
                steps_done,
                steps_estimate,
            }) if i == instance => print_run(&run, steps_done, steps_estimate, out)?,
            Event::Bisect(BisectEvent::Finished {
                instance: i,
                report: r,
            }) if i == instance => {
                report = Some(r);
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
    let report = match report {
        Some(r) => r,
        None => match call(
            facade,
            Command::Bisect(BisectCommand::AutoReport { instance }),
        )? {
            Response::BisectReport(r) => r,
            other => return Err(unexpected(&other)),
        },
    };
    print_report(&report, out)
}

/// `report <instance>`: the last automatic search.
pub(crate) fn report(facade: &dyn Facade, instance: &str, out: Out<'_>) -> Result<(), CliError> {
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    match call(
        facade,
        Command::Bisect(BisectCommand::AutoReport { instance }),
    )? {
        Response::BisectReport(report) => {
            for run in &report.runs {
                print_run(run, 0, 0, out)?;
            }
            print_report(&report, out)
        }
        other => Err(unexpected(&other)),
    }
}

fn parse(instance: InstanceId, flags: &[&str]) -> Result<AutoBisectRequest, CliError> {
    let mut request = AutoBisectRequest::new(instance);
    let number = |v: Option<&&str>| -> Result<u32, CliError> {
        v.and_then(|v| v.parse().ok()).ok_or(CliError::from(USAGE))
    };
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--world" => {
                let name = it.next().ok_or(CliError::from(USAGE))?;
                request.world = Some((*name).to_owned());
                request.target = BisectTarget::World;
            }
            "--stage" => {
                request.target = match it.next().copied() {
                    Some("menu") => BisectTarget::Menu,
                    Some("world") => BisectTarget::World,
                    _ => return Err(USAGE.into()),
                };
            }
            "--stable-secs" => request.stable_secs = number(it.next())?,
            "--repeats" => request.repeats = number(it.next())?.max(1),
            "--timeout-secs" => request.timeout_secs = number(it.next())?.max(1),
            "--after-crash" => request.after_crash = true,
            "--offline" => request.offline = true,
            _ => return Err(USAGE.into()),
        }
    }
    if request.target == BisectTarget::World && request.world.is_none() {
        return Err("--stage world needs --world <name>".into());
    }
    Ok(request)
}

fn print_run(run: &BisectRun, done: u32, estimate: u32, out: Out<'_>) -> Result<(), CliError> {
    let purpose = match run.purpose {
        RunPurpose::Reproduce => "every mod",
        RunPurpose::WithoutMods => "no mods",
        RunPurpose::Verify => "check",
        _ => "step",
    };
    let result = match run.result {
        RunResult::Reached => "ran clean",
        RunResult::Crashed => "crashed (the problem)",
        RunResult::OtherCrash => "crashed differently (ignored)",
        RunResult::Hung => "did not get there in time (ignored)",
        _ => "closed before the target (ignored)",
    };
    let mut line = format!(
        "[bisect] {purpose}: {} mod(s) on, {result} after {} s",
        run.enabled, run.seconds
    );
    if let Some(crash) = &run.crash {
        line.push_str(&format!(" [{crash}]"));
    }
    if run.purpose == RunPurpose::Step && done + estimate > 0 {
        line.push_str(&format!("; step {done} of ~{}", done + estimate));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

fn diagnosis_line(d: &CrashDiagnosis) -> String {
    let mut text = d.message_key.clone();
    if !d.mods.is_empty() {
        text.push_str(&format!(" (mods: {})", d.mods.join(", ")));
    }
    for (key, value) in &d.args {
        text.push_str(&format!(", {key}={value}"));
    }
    text
}

fn print_report(report: &AutoBisectReport, out: Out<'_>) -> Result<(), CliError> {
    if report.semi_manual {
        writeln!(
            out,
            "semi-manual: runs counted as good when you closed the game without a crash"
        )?;
    }
    match &report.verdict {
        AutoVerdict::Found {
            culprits,
            needs,
            confirmed,
        } => {
            writeln!(out, "culprit: {}", culprits.join(", "))?;
            if !needs.is_empty() {
                writeln!(out, "it needs: {}", needs.join(", "))?;
            }
            if *confirmed {
                writeln!(out, "confirmed: the game runs clean without it")?;
            } else {
                writeln!(out, "not confirmed: the check without it did not run clean")?;
            }
        }
        AutoVerdict::NotReproduced => writeln!(out, "the problem did not happen with every mod")?,
        AutoVerdict::Hangs => writeln!(
            out,
            "the game does not get there in time with every mod (a hang is not searched)"
        )?,
        AutoVerdict::Inconclusive { suspects } if suspects.is_empty() => {
            writeln!(out, "inconclusive")?;
        }
        AutoVerdict::Inconclusive { suspects } => {
            writeln!(
                out,
                "inconclusive; the cause is among: {}",
                suspects.join(", ")
            )?;
        }
        AutoVerdict::NotMods { diagnosis } => {
            writeln!(
                out,
                "not the mods: the game fails without them or the setup is at fault"
            )?;
            if let Some(d) = diagnosis {
                writeln!(out, "cause: {}", diagnosis_line(d))?;
            }
        }
        AutoVerdict::LoaderReported { diagnosis } => {
            writeln!(out, "the mod loader named the problem; no search needed")?;
            writeln!(out, "cause: {}", diagnosis_line(diagnosis))?;
        }
        _ => writeln!(out, "finished")?,
    }
    writeln!(out, "{} run(s); mods and world restored", report.runs.len())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_flags() {
        let id = InstanceId::new("a").unwrap();
        let r = parse(
            id.clone(),
            &[
                "--world",
                "My World",
                "--stable-secs",
                "5",
                "--repeats",
                "3",
                "--offline",
            ],
        )
        .unwrap();
        assert_eq!(r.target, BisectTarget::World);
        assert_eq!(r.world.as_deref(), Some("My World"));
        assert_eq!((r.stable_secs, r.repeats, r.offline), (5, 3, true));
        let r = parse(id.clone(), &["--stage", "menu", "--after-crash"]).unwrap();
        assert!(r.after_crash && r.target == BisectTarget::Menu);
        assert!(parse(id.clone(), &["--stage", "world"]).is_err());
        assert!(parse(id.clone(), &["--repeats", "x"]).is_err());
        assert!(parse(id, &["--nope"]).is_err());
    }
}
