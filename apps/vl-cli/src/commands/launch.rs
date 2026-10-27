//! `launch <instance> [--offline]`: follows the launch task and the game log until the game exits.

use std::time::Duration;

use vl_core::launch::{LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::log::LogSource;
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let (instance, offline) = match args {
        [instance] => (*instance, false),
        [instance, "--offline"] => (*instance, true),
        _ => return Err(USAGE.into()),
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let request = LaunchRequest {
        instance: instance.clone(),
        account: None,
        offline,
    };
    let task = match call(facade, Command::Launch(LaunchCommand::Launch(request)))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let game = LogSource::Game(instance.clone());
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(TaskEvent::Progress { id, progress }) if id == task => {
                let stage = progress.stage.unwrap_or_default();
                match progress.total {
                    Some(total) => writeln!(out, "[launch] {stage} {}/{total}", progress.done)?,
                    None => writeln!(out, "[launch] {stage} {}", progress.done)?,
                }
            }
            Event::Task(TaskEvent::Finished { id, outcome }) if id == task => match outcome {
                TaskOutcome::Succeeded => writeln!(out, "[launch] prepared")?,
                TaskOutcome::Cancelled => writeln!(out, "[launch] cancelled")?,
                TaskOutcome::Failed(e) => return Err(e.into()),
            },
            Event::Log(line) if line.source == game => writeln!(out, "{}", line.text)?,
            Event::Launch(LaunchEvent::Exited { instance: i, exit }) if i == instance => {
                match exit.code {
                    Some(code) => writeln!(out, "[game] exited with code {code}")?,
                    None => writeln!(out, "[game] exited")?,
                }
                return Ok(());
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}
