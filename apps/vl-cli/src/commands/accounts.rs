//! `accounts login | list | remove <account> | refresh <account>`.

use std::time::Duration;

use vl_core::account::{Account, AccountCommand, AccountEvent};
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{Command, Event, EventStream, Facade, Response, TaskId};

use super::{CliError, Out, USAGE, call, unexpected};

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => list(facade, out),
        ["login"] => login(facade, out),
        ["remove", who] => {
            let account = find(facade, who)?;
            match call(
                facade,
                Command::Accounts(AccountCommand::Remove {
                    id: account.id.clone(),
                }),
            )? {
                Response::Done => {
                    writeln!(out, "removed {}", account.name)?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        ["refresh", who] => {
            let account = find(facade, who)?;
            let events = facade.subscribe();
            let task = start_task(
                facade,
                AccountCommand::Refresh {
                    id: account.id.clone(),
                },
            )?;
            follow(&events, task, out)?;
            writeln!(out, "refreshed {}", account.name)?;
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn list(facade: &dyn Facade, out: Out<'_>) -> Result<(), CliError> {
    for a in accounts(facade)? {
        let default = if a.is_default { " (default)" } else { "" };
        let stale = if a.needs_sign_in {
            " (sign-in required)"
        } else {
            ""
        };
        writeln!(
            out,
            "{:<16} {:?} {}{default}{stale}",
            a.name, a.kind, a.uuid
        )?;
    }
    Ok(())
}

/// Device-code sign-in: prints the code and the page, then waits for the user.
fn login(facade: &dyn Facade, out: Out<'_>) -> Result<(), CliError> {
    // Subscribe before the command so the code is not missed.
    let events = facade.subscribe();
    let task = start_task(facade, AccountCommand::SignInMicrosoft)?;
    follow(&events, task, out)?;
    writeln!(out, "signed in")?;
    Ok(())
}

fn start_task(facade: &dyn Facade, command: AccountCommand) -> Result<TaskId, CliError> {
    match call(facade, Command::Accounts(command))? {
        Response::Task(task) => Ok(task),
        other => Err(unexpected(&other)),
    }
}

/// Prints sign-in prompts of `task` until it ends; a failed task is an error.
fn follow(events: &EventStream, task: TaskId, out: Out<'_>) -> Result<(), CliError> {
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Account(AccountEvent::DeviceCode(p)) if p.task == task => {
                writeln!(
                    out,
                    "To sign in, open {} and enter the code {}",
                    p.verification_uri, p.user_code
                )?;
                writeln!(
                    out,
                    "The code expires in {} minutes.",
                    p.expires_in_secs / 60
                )?;
            }
            Event::Task(TaskEvent::Finished { id, outcome }) if id == task => {
                return match outcome {
                    TaskOutcome::Succeeded => Ok(()),
                    TaskOutcome::Cancelled => Err("cancelled".into()),
                    TaskOutcome::Failed(e) => Err(e.into()),
                };
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

fn accounts(facade: &dyn Facade) -> Result<Vec<Account>, CliError> {
    match call(facade, Command::Accounts(AccountCommand::List))? {
        Response::Accounts(accounts) => Ok(accounts),
        other => Err(unexpected(&other)),
    }
}

/// The account whose id or name (any case) is `who`.
fn find(facade: &dyn Facade, who: &str) -> Result<Account, CliError> {
    let all = accounts(facade)?;
    let mut matches = all
        .iter()
        .filter(|a| a.id.as_str() == who || a.name.eq_ignore_ascii_case(who));
    match (matches.next(), matches.next()) {
        (Some(a), None) => Ok(a.clone()),
        (None, _) => Err(CliError::from(format!("no account named {who}").as_str())),
        _ => Err(CliError::from(
            format!("several accounts match {who}; use the account id").as_str(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    fn run_mock(args: &[&str]) -> Result<String, CliError> {
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        run(&facade, args, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn login_prints_the_code_and_the_page() {
        let out = run_mock(&["login"]).unwrap();
        assert!(out.contains("enter the code MOCK-CODE"), "{out}");
        assert!(out.contains("https://example.invalid/device"), "{out}");
        assert!(out.contains("signed in"), "{out}");
    }

    #[test]
    fn lists_and_removes_by_name() {
        let out = run_mock(&["list"]).unwrap();
        assert!(out.contains("Offline"), "{out}");
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        let name = accounts(&facade).unwrap()[0].name.clone();
        run(&facade, &["remove", &name.to_lowercase()], &mut out).unwrap();
        assert!(accounts(&facade).unwrap().is_empty());
    }

    #[test]
    fn unknown_account_and_bad_usage_are_errors() {
        assert!(run_mock(&["remove", "nobody"]).is_err());
        assert!(run_mock(&["remove"]).is_err());
        // The mock does not implement refresh: the error reaches the user, no panic.
        let first = run_mock(&["list"]).unwrap();
        let name = first.split_whitespace().next().unwrap().to_owned();
        assert!(run_mock(&["refresh", &name]).is_err());
    }
}
