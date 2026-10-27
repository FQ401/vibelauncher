//! `info`: which backend answers.

use vl_core::{Command, Facade, Response};

use super::{CliError, Out, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, out: Out<'_>) -> Result<(), CliError> {
    match call(facade, Command::Hello)? {
        Response::Hello(info) => {
            let kind = if info.mock { " (synthetic data)" } else { "" };
            writeln!(out, "{} {}{kind}", info.name, info.version)?;
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}
