//! `instances verify <instance>`: checks every game file of an instance and repairs the
//! downloadable ones.

use std::time::Duration;

use vl_core::launch::LaunchCommand;
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, instance: &str, out: Out<'_>) -> Result<(), CliError> {
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let task = match call(facade, Command::Launch(LaunchCommand::Verify { instance }))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "verify");
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    writeln!(out, "[verify] all files are in place")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("verify cancelled".into()),
                None => {}
            },
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}
