//! `accounts …`.

use vl_core::account::AccountCommand;
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => match call(facade, Command::Accounts(AccountCommand::List))? {
            Response::Accounts(accounts) => {
                for a in accounts {
                    let default = if a.is_default { " (default)" } else { "" };
                    writeln!(out, "{:<16} {:?}{default}", a.name, a.kind)?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        _ => Err(USAGE.into()),
    }
}
