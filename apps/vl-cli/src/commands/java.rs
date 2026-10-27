//! `java …`.

use vl_core::java::JavaCommand;
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => match call(facade, Command::Java(JavaCommand::List))? {
            Response::Javas(javas) => {
                for j in javas {
                    writeln!(
                        out,
                        "{:<3} {:<14} {:?} {:<14} {}",
                        j.major,
                        j.version,
                        j.arch,
                        j.vendor,
                        j.path.display()
                    )?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        _ => Err(USAGE.into()),
    }
}
