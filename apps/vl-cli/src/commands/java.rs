//! `java …`.

use std::time::Duration;

use vl_core::java::{JavaCommand, JavaRuntime};
use vl_core::platform::Arch;
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => {
            let javas = list(facade)?;
            if javas.is_empty() {
                writeln!(out, "no Java runtimes found")?;
            }
            for j in javas {
                writeln!(out, "{}", describe(&j))?;
            }
            Ok(())
        }
        ["install", major] => {
            let major: u32 = major
                .parse()
                .map_err(|_| CliError::from("java install: the major version must be a number"))?;
            install(facade, major, out)
        }
        _ => Err(USAGE.into()),
    }
}

fn list(facade: &dyn Facade) -> Result<Vec<JavaRuntime>, CliError> {
    match call(facade, Command::Java(JavaCommand::List))? {
        Response::Javas(javas) => Ok(javas),
        other => Err(unexpected(&other)),
    }
}

/// Installs a managed runtime of `major` and follows the download until it ends.
fn install(facade: &dyn Facade, major: u32, out: Out<'_>) -> Result<(), CliError> {
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let task = match call(facade, Command::Java(JavaCommand::Install { major }))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut last_percent = None;
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(TaskEvent::Progress { id, progress }) if id == task => {
                let percent = progress.fraction().map(|f| (f * 100.0) as u32);
                if percent != last_percent {
                    last_percent = percent;
                    match (percent, progress.total) {
                        (Some(p), Some(total)) => writeln!(
                            out,
                            "[java] downloading {p}% ({} of {} MiB)",
                            progress.done >> 20,
                            total >> 20
                        )?,
                        _ => writeln!(out, "[java] downloaded {} MiB", progress.done >> 20)?,
                    }
                }
            }
            Event::Task(TaskEvent::Finished { id, outcome }) if id == task => {
                return match outcome {
                    TaskOutcome::Succeeded => {
                        let installed = list(facade)?
                            .into_iter()
                            .filter(|j| j.managed && j.major >= major)
                            .min_by_key(|j| j.major);
                        match installed {
                            Some(j) => writeln!(out, "[java] installed: {}", describe(&j))?,
                            None => writeln!(out, "[java] installed")?,
                        }
                        Ok(())
                    }
                    TaskOutcome::Cancelled => {
                        writeln!(out, "[java] cancelled")?;
                        Ok(())
                    }
                    TaskOutcome::Failed(e) => Err(e.into()),
                };
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

fn describe(j: &JavaRuntime) -> String {
    format!(
        "{:<3} {:<14} {:<8} {:<24} {}{}",
        j.major,
        j.version,
        arch_name(j.arch),
        j.vendor,
        j.path.display(),
        if j.managed { "  [managed]" } else { "" }
    )
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86 => "x86",
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::Arm => "arm",
        _ => "other",
    }
}
