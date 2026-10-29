//! `update …`: the launcher's own updates.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use vl_core::self_update::{SelfUpdateCommand, SelfUpdateStatus, UpdateOffer};
use vl_core::task::{TaskCommand, TaskOutcome};
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::{ProgressPrinter, human_bytes};

/// How long `vl-cli` waits for an event before it looks at Ctrl+C again.
const POLL: Duration = Duration::from_millis(100);

static INTERRUPTS: AtomicU32 = AtomicU32::new(0);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["status"] => print_status(&status(facade, SelfUpdateCommand::Status)?, out),
        ["check"] => print_status(&status(facade, SelfUpdateCommand::Check)?, out),
        ["download"] => download(facade, out),
        ["apply"] => {
            match call(
                facade,
                Command::SelfUpdate(SelfUpdateCommand::Apply { restart: None }),
            )? {
                Response::Done => {}
                other => return Err(unexpected(&other)),
            }
            writeln!(
                out,
                "the update helper installs the update as soon as vl-cli exits; \
                 `vl-cli update status` shows the result"
            )?;
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn status(facade: &dyn Facade, command: SelfUpdateCommand) -> Result<SelfUpdateStatus, CliError> {
    match call(facade, Command::SelfUpdate(command))? {
        Response::SelfUpdate(status) => Ok(status),
        other => Err(unexpected(&other)),
    }
}

fn print_status(s: &SelfUpdateStatus, out: Out<'_>) -> Result<(), CliError> {
    writeln!(out, "version: {} ({})", s.current, s.install)?;
    if !s.enabled {
        writeln!(
            out,
            "updates: off (this build has no release key and cannot verify them)"
        )?;
    }
    if let Some(j) = &s.just_updated {
        writeln!(out, "just updated: {} -> {}", j.from, j.to)?;
    }
    if let Some(e) = &s.last_error {
        writeln!(
            out,
            "the last update to {} failed and was undone: {}",
            e.version, e.message
        )?;
    }
    if let Some(v) = &s.newest_seen {
        writeln!(out, "newest release seen: {v}")?;
    }
    match &s.offer {
        Some(UpdateOffer::UpToDate) => writeln!(out, "up to date")?,
        Some(UpdateOffer::Update {
            version,
            date,
            size,
        }) => writeln!(
            out,
            "update available: {version} ({date}, {}); `vl-cli update download`",
            human_bytes(*size)
        )?,
        Some(UpdateOffer::Manual {
            version,
            page,
            reason,
        }) => writeln!(
            out,
            "{version} is available; install it by hand from {page} ({reason})"
        )?,
        Some(_) | None => {}
    }
    if let Some(v) = &s.staged {
        writeln!(out, "downloaded: {v}; `vl-cli update apply` installs it")?;
    }
    Ok(())
}

/// Downloads the update and follows the task; Ctrl+C cancels it.
fn download(facade: &dyn Facade, out: Out<'_>) -> Result<(), CliError> {
    let _ = ctrlc::set_handler(|| {
        INTERRUPTS.fetch_add(1, Ordering::SeqCst);
    });
    let seen = INTERRUPTS.load(Ordering::SeqCst);
    let mut cancelled = false;
    let events = facade.subscribe();
    let task = match call(facade, Command::SelfUpdate(SelfUpdateCommand::Download))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut progress = ProgressPrinter::new(task, "update");
    loop {
        if INTERRUPTS.load(Ordering::SeqCst) > seen && !cancelled {
            cancelled = call(facade, Command::Tasks(TaskCommand::Cancel { id: task })).is_ok();
        }
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(event) => match progress.handle(&event, out)? {
                Some(TaskOutcome::Succeeded) => break,
                Some(TaskOutcome::Cancelled) => {
                    writeln!(out, "[update] cancelled")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                None => {}
            },
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
    print_status(&status(facade, SelfUpdateCommand::Status)?, out)
}

#[cfg(test)]
mod tests {
    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    fn run_mock(facade: &MockFacade, args: &[&str]) -> String {
        let mut out = Vec::new();
        run(facade, args, &mut out).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn check_download_apply_on_the_mock() {
        let facade = MockFacade::new(MockOptions::instant());
        assert!(run_mock(&facade, &["status"]).starts_with("version: "));
        assert!(run_mock(&facade, &["check"]).contains("update available: 0.1.0"));
        let out = run_mock(&facade, &["download"]);
        assert!(out.contains("downloaded: 0.1.0"), "{out}");
        assert!(run_mock(&facade, &["apply"]).contains("update helper"));
        let mut out = Vec::new();
        assert!(run(&facade, &["nope"], &mut out).is_err());
    }
}
