//! `instances …`.

use vl_core::instance::{InstanceCommand, NewInstance};
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => match call(facade, Command::Instances(InstanceCommand::List))? {
            Response::Instances(instances) => {
                for i in instances {
                    let problem = i
                        .problem
                        .map(|p| format!("  [problem: {p}]"))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "{:<16} {:<24} {:<10} {:?}{problem}",
                        i.id, i.name, i.game_version, i.loader
                    )?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        ["create", name, version] => {
            let command = Command::Instances(InstanceCommand::Create(NewInstance {
                name: (*name).into(),
                game_version: (*version).into(),
                group: None,
            }));
            match call(facade, command)? {
                Response::Instance(i) => {
                    writeln!(out, "created {} ({})", i.id, i.name)?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        _ => Err(USAGE.into()),
    }
}
