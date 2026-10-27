//! `launch <instance> [--offline]`: follows the launch task and the game log until the game exits.
//!
//! Ctrl+C stops the game (or cancels the preparation) and the command then reports the exit.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use vl_core::launch::{GameExit, LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::log::LogSource;
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(100);

/// How many times Ctrl+C was pressed in this process.
static INTERRUPTS: AtomicU32 = AtomicU32::new(0);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let (instance, offline) = match args {
        [instance] => (*instance, false),
        [instance, "--offline"] => (*instance, true),
        _ => return Err(USAGE.into()),
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    // Only one handler can be installed per process; if it fails Ctrl+C ends the program as usual.
    let _ = ctrlc::set_handler(|| {
        INTERRUPTS.fetch_add(1, Ordering::SeqCst);
    });
    let seen = INTERRUPTS.load(Ordering::SeqCst);
    let mut asked_to_stop = false;
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
    let mut progress = ProgressPrinter::new(task, "launch");
    let game = LogSource::Game(instance.clone());
    loop {
        // Ask again until the instance accepts it: right after the start there is a short moment
        // in which the launch is not yet registered as stoppable.
        if INTERRUPTS.load(Ordering::SeqCst) > seen && !asked_to_stop {
            let stop = Command::Launch(LaunchCommand::Stop {
                instance: instance.clone(),
            });
            asked_to_stop = call(facade, stop).is_ok();
            if asked_to_stop {
                writeln!(out, "[vl-cli] stopping")?;
            }
        }
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(event) => match progress.handle(&event, out)? {
                Some(TaskOutcome::Succeeded) => writeln!(out, "[launch] prepared")?,
                Some(TaskOutcome::Cancelled) => writeln!(out, "[launch] cancelled")?,
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                None => {}
            },
            Event::Log(line) if line.source == game => writeln!(out, "{}", line.text)?,
            Event::LogBatch(batch) if batch.source == game => {
                for line in &batch.lines {
                    writeln!(out, "{}", line.text)?;
                }
                if batch.skipped > 0 {
                    writeln!(
                        out,
                        "[vl-cli] {} lines not shown (see the log file)",
                        batch.skipped
                    )?;
                }
            }
            Event::Launch(LaunchEvent::Exited { instance: i, exit }) if i == instance => {
                report_exit(&exit, out)?;
                return Ok(());
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

/// Prints how the game ended: code, session time, and the files a crash left.
fn report_exit(exit: &GameExit, out: Out<'_>) -> Result<(), CliError> {
    let secs = exit.session_ms / 1000;
    let how = match (exit.stopped_by_user, exit.crashed) {
        (true, _) => "stopped",
        (false, true) => "crashed",
        (false, false) => "exited",
    };
    match exit.code {
        Some(code) => writeln!(out, "[game] {how} with code {code} after {secs} s")?,
        None => writeln!(out, "[game] {how} after {secs} s")?,
    }
    if let Some(path) = &exit.crash_report {
        writeln!(out, "[game] crash report: {}", path.display())?;
    }
    if let Some(path) = &exit.hs_err {
        writeln!(out, "[game] JVM error log: {}", path.display())?;
    }
    if let Some(path) = &exit.log_file {
        writeln!(out, "[game] full log: {}", path.display())?;
    }
    Ok(())
}
