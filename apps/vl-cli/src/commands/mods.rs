//! `instances mods …`: the files in an instance's mods, packs and shaders folders.

use vl_core::resources::{ResourceCommand, ResourceEntry, ResourceKind};
use vl_core::{Command, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list", instance, flags @ ..] => {
            let (instance, kind) = target(instance, flags)?;
            let command = Command::Resources(ResourceCommand::List { instance, kind });
            match call(facade, command)? {
                Response::Resources(entries) => {
                    if entries.is_empty() {
                        writeln!(out, "no {}", kind.name())?;
                    }
                    for entry in &entries {
                        writeln!(out, "{}", describe(entry))?;
                    }
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        [verb @ ("enable" | "disable"), instance, file, flags @ ..] => {
            let (instance, kind) = target(instance, flags)?;
            let enabled = *verb == "enable";
            let command = Command::Resources(ResourceCommand::SetEnabled {
                instance,
                kind,
                file: (*file).to_owned(),
                enabled,
            });
            done(facade, command)?;
            writeln!(out, "{file} {verb}d")?;
            Ok(())
        }
        ["add", instance, source, flags @ ..] => {
            let (instance, kind) = target(instance, flags)?;
            let command = Command::Resources(ResourceCommand::Add {
                instance,
                kind,
                source: (*source).to_owned(),
            });
            done(facade, command)?;
            writeln!(out, "added {source}")?;
            Ok(())
        }
        ["remove", instance, file, flags @ ..] => {
            let (instance, kind) = target(instance, flags)?;
            let command = Command::Resources(ResourceCommand::Remove {
                instance,
                kind,
                file: (*file).to_owned(),
            });
            done(facade, command)?;
            writeln!(out, "{file} moved to the instance trash")?;
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn done(facade: &dyn Facade, command: Command) -> Result<(), CliError> {
    match call(facade, command)? {
        Response::Done => Ok(()),
        other => Err(unexpected(&other)),
    }
}

/// The instance and the optional `--kind <name>` (mods by default).
fn target(instance: &str, flags: &[&str]) -> Result<(InstanceId, ResourceKind), CliError> {
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let kind = match flags {
        [] => ResourceKind::Mods,
        ["--kind", name] => ResourceKind::parse(name)
            .ok_or_else(|| CliError::from(format!("unknown kind {name:?}").as_str()))?,
        _ => return Err(USAGE.into()),
    };
    Ok((id, kind))
}

fn describe(entry: &ResourceEntry) -> String {
    let state = if entry.enabled { "on " } else { "off" };
    let mut line = format!("[{state}] {}", entry.file);
    for m in &entry.mods {
        let version = m.version.as_deref().unwrap_or("?");
        line.push_str(&format!("  {} ({}) {version}", m.name, m.id));
    }
    if !entry.nested_jars.is_empty() {
        line.push_str(&format!("  [{} bundled jars]", entry.nested_jars.len()));
    }
    if let Some(problem) = &entry.problem {
        line.push_str(&format!("  [problem: {problem}]"));
    }
    line
}
