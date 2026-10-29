//! `versions list | install <id>`.

use std::time::Duration;

use vl_core::component::ComponentKind;
use vl_core::instance::LoaderRequest;
use vl_core::task::TaskOutcome;
use vl_core::version::{VersionCommand, VersionFilter};
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, loader_flags, parse_loader, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list", flags @ ..] => list(facade, flags, out),
        ["install", id, flags @ ..] => install(facade, id, loader_flags(flags)?, out),
        ["loaders", loader, game] => loaders(facade, loader, game, out),
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

/// Prints the versions of a loader for a game version with their marks: `recommended` and
/// `latest`, `stable` for the loaders that divide their versions into stable and not, and
/// `(unstable)` for a build for a snapshot, pre-release or April Fools' game version.
fn loaders(facade: &dyn Facade, loader: &str, game: &str, out: Out<'_>) -> Result<(), CliError> {
    let loader = parse_loader(loader)?;
    let command = Command::Versions(VersionCommand::LoaderVersions {
        loader,
        game_version: game.to_owned(),
    });
    match call(facade, command)? {
        Response::LoaderVersions(versions) => {
            for v in versions {
                let mut marks = Vec::new();
                if v.recommended {
                    marks.push("recommended");
                }
                if v.latest {
                    marks.push("latest");
                }
                if v.stable && loader != ComponentKind::Forge {
                    marks.push("stable");
                }
                if v.unstable_game {
                    marks.push("(unstable)");
                }
                writeln!(out, "{:<24} {}", v.version, marks.join(" "))?;
            }
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

/// Installs a version's files without starting the game and prints the progress.
fn install(
    facade: &dyn Facade,
    id: &str,
    loader: Option<LoaderRequest>,
    out: Out<'_>,
) -> Result<(), CliError> {
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let command = Command::Versions(VersionCommand::Install {
        id: id.to_owned(),
        loader,
    });
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
