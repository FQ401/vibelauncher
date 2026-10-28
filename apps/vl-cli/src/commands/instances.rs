//! `instances …`.

use vl_core::instance::{InstanceCommand, LoaderRequest, NewInstance};
use vl_core::{Command, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, loader_flags, parse_loader, unexpected};
use super::{bisect, datapacks, diagnose, mods};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["mods", rest @ ..] => mods::run(facade, rest, out),
        ["bisect", rest @ ..] => bisect::run(facade, rest, out),
        ["diagnose", rest @ ..] => diagnose::run(facade, rest, out),
        ["worlds", rest @ ..] => datapacks::run_worlds(facade, rest, out),
        ["datapacks", rest @ ..] => datapacks::run(facade, rest, out),
        ["list"] => match call(facade, Command::Instances(InstanceCommand::List))? {
            Response::Instances(instances) => {
                for i in instances {
                    let problem = i
                        .problem
                        .map(|p| format!("  [problem: {p}]"))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "{:<16} {:<24} {:<10} {}{problem}",
                        i.id, i.name, i.game_version, i.loader
                    )?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        ["create", name, version, flags @ ..] => {
            let command = Command::Instances(InstanceCommand::Create(NewInstance {
                name: (*name).into(),
                game_version: (*version).into(),
                group: None,
                loader: loader_flags(flags)?,
            }));
            match call(facade, command)? {
                Response::Instance(i) => {
                    writeln!(out, "created {} ({}) {}", i.id, i.name, i.loader)?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        ["set-loader", instance, loader, flags @ ..] => {
            let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
            let request = if *loader == "none" {
                if !flags.is_empty() {
                    return Err(USAGE.into());
                }
                None
            } else {
                let kind = parse_loader(loader)?;
                let version = match flags {
                    [] => None,
                    ["--loader-version", v] => Some((*v).to_owned()),
                    _ => return Err(USAGE.into()),
                };
                Some(LoaderRequest { kind, version })
            };
            let command = Command::Instances(InstanceCommand::SetLoader {
                id,
                loader: request,
            });
            match call(facade, command)? {
                Response::Instance(i) => {
                    writeln!(out, "{} now runs {}", i.id, i.loader)?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        _ => Err(USAGE.into()),
    }
}
