//! `versions list | install <id>`.

use std::time::Duration;

use vl_core::task::TaskOutcome;
use vl_core::version::{VersionCommand, VersionFilter};
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list", flags @ ..] => list(facade, flags, out),
        ["install", id] => install(facade, id, out),
        _ => Err(USAGE.into()),
    }
}

fn list(facade: &dyn Facade, flags: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let mut filter = VersionFilter::default();
    for flag in flags {
        match *flag {
            "--snapshots" => filter.snapshots = true,
            "--old" => filter.old = true,
            _ => return Err(USAGE.into()),
        }
    }
    match call(facade, Command::Versions(VersionCommand::List(filter)))? {
        Response::Versions(versions) => {
            for v in versions {
                writeln!(out, "{:<20} {:?}  {}", v.id, v.kind, v.release_time)?;
            }
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

/// Installs a version's files without starting the game and prints the progress.
fn install(facade: &dyn Facade, id: &str, out: Out<'_>) -> Result<(), CliError> {
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let command = Command::Versions(VersionCommand::Install { id: id.to_owned() });
    let task = match call(facade, command)? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "install");
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    writeln!(out, "[install] {id} is installed")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("install cancelled".into()),
                None => {}
            },
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    #[test]
    fn the_mock_does_not_install_versions() {
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        let err = run(&facade, &["install", "1.21"], &mut out).unwrap_err();
        assert!(err.to_string().contains("Unsupported"), "{err}");
    }
}
