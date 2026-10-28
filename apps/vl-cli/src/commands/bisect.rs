//! `instances bisect …`: finding the mod that crashes the game.

use vl_core::bisect::{BisectCommand, BisectOutcome, BisectState, BisectStatus, BisectVerdict};
use vl_core::{Command, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let command = match args {
        ["start", instance, flags @ ..] => BisectCommand::Start {
            instance: id(instance)?,
            suspects: suspects(flags)?,
        },
        ["status", instance] => BisectCommand::Status {
            instance: id(instance)?,
        },
        [verb @ ("good" | "bad" | "skip"), instance] => BisectCommand::Report {
            instance: id(instance)?,
            outcome: match *verb {
                "good" => BisectOutcome::Good,
                "bad" => BisectOutcome::Bad,
                _ => BisectOutcome::Skipped,
            },
        },
        ["auto", rest @ ..] => return super::bisect_auto::run(facade, rest, out),
        ["report", instance] => return super::bisect_auto::report(facade, instance, out),
        ["stop", instance] => {
            let instance = id(instance)?;
            let stop = BisectCommand::Stop { instance };
            return match call(facade, Command::Bisect(stop))? {
                Response::Done => {
                    writeln!(out, "bisect session ended, mods restored")?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            };
        }
        _ => return Err(USAGE.into()),
    };
    match call(facade, Command::Bisect(command))? {
        Response::Bisect(status) => print_status(&status, out),
        other => Err(unexpected(&other)),
    }
}

fn id(instance: &str) -> Result<InstanceId, CliError> {
    InstanceId::new(instance).ok_or(CliError::from(USAGE))
}

/// The mod ids of the repeated `--suspect <id>` flags.
fn suspects(flags: &[&str]) -> Result<Vec<String>, CliError> {
    let mut ids = Vec::new();
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match (*flag, it.next()) {
            ("--suspect", Some(mod_id)) => ids.push((*mod_id).to_owned()),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(ids)
}

fn print_status(status: &BisectStatus, out: Out<'_>) -> Result<(), CliError> {
    match (&status.state, &status.verdict) {
        (BisectState::Running, _) => {
            writeln!(
                out,
                "step {}: {} of {} mods enabled, {} disabled; about {} more step(s)",
                status.steps_done + 1,
                status.enabled.len(),
                status.candidates,
                status.disabled,
                status.steps_estimate
            )?;
            writeln!(out, "suspects: {}", status.suspects.len())?;
            writeln!(
                out,
                "start the game, then report: bisect good | bad | skip <instance> (skip: the game \
                 failed for another reason or did not reach the problem)"
            )?;
        }
        (_, Some(BisectVerdict::Found { culprits, needs })) => {
            writeln!(out, "culprit: {}", culprits.join(", "))?;
            if !needs.is_empty() {
                writeln!(out, "it needs: {}", needs.join(", "))?;
            }
            writeln!(out, "done in {} step(s); mods restored", status.steps_done)?;
        }
        (_, Some(BisectVerdict::NotReproduced)) => {
            writeln!(
                out,
                "the problem did not happen with every mod enabled; mods restored"
            )?;
        }
        (_, Some(BisectVerdict::Inconclusive { suspects })) => {
            writeln!(
                out,
                "inconclusive; the cause is among: {}; mods restored",
                suspects.join(", ")
            )?;
        }
        _ => writeln!(out, "finished; mods restored")?,
    }
    Ok(())
}
