//! `packs install <file.mrpack|slug> [--name X] [--version <id>] [--group G] [--channel <c>]`.

use std::path::Path;
use std::time::Duration;

use vl_core::catalog::{CatalogCommand, Provider};
use vl_core::install::{MinStability, PackInstallRequest, PackSource};
use vl_core::instance::InstanceEvent;
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(250);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["install", pack, flags @ ..] => install(facade, pack, flags, out),
        _ => Err(USAGE.into()),
    }
}

/// A path that names a file (or ends in `.mrpack`) is a local pack; anything else is a project.
fn parse_source(pack: &str, version: Option<String>) -> Result<PackSource, CliError> {
    let is_file = pack.to_ascii_lowercase().ends_with(".mrpack") || Path::new(pack).is_file();
    if is_file {
        if version.is_some() {
            return Err("--version applies to catalog packs, not files".into());
        }
        let path = std::path::absolute(pack)
            .map_err(|e| CliError::from(format!("{pack}: {e}")))?
            .to_string_lossy()
            .into_owned();
        Ok(PackSource::File { path })
    } else {
        Ok(PackSource::Catalog {
            provider: Provider::default(),
            project: pack.to_owned(),
            version,
        })
    }
}

fn parse_request(pack: &str, flags: &[&str]) -> Result<PackInstallRequest, CliError> {
    let (mut name, mut version, mut group) = (None, None, None);
    let mut min_stability = MinStability::default();
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(CliError::from(USAGE))?;
        match *flag {
            "--name" => name = Some((*value).to_owned()),
            "--version" => version = Some((*value).to_owned()),
            "--group" => group = Some((*value).to_owned()),
            "--channel" => {
                min_stability = MinStability::parse(value)
                    .ok_or_else(|| CliError::from("channel is release, beta or alpha"))?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(PackInstallRequest {
        source: parse_source(pack, version)?,
        name,
        group,
        min_stability,
    })
}

fn install(facade: &dyn Facade, pack: &str, flags: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let request = parse_request(pack, flags)?;
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let task = match call(
        facade,
        Command::Catalog(CatalogCommand::InstallPack(request)),
    )? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "pack");
    let mut created = None;
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    match created {
                        Some(ref i) => {
                            let i: &vl_core::instance::InstanceSummary = i;
                            writeln!(
                                out,
                                "[pack] installed as instance {} ({}, Minecraft {}, {:?})",
                                i.id, i.name, i.game_version, i.loader
                            )?;
                        }
                        None => writeln!(out, "[pack] done")?,
                    }
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("pack install cancelled".into()),
                None => {}
            },
            Event::Instance(InstanceEvent::Added(summary)) => created = Some(summary),
            Event::Log(line) => writeln!(out, "[pack] {}", line.text)?,
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_files_from_projects() {
        let r = parse_request("pack.mrpack", &["--name", "X"]).unwrap();
        assert!(matches!(r.source, PackSource::File { .. }));
        assert_eq!(r.name.as_deref(), Some("X"));
        let r = parse_request("fabulously-optimized", &["--version", "abc"]).unwrap();
        assert!(matches!(
            r.source,
            PackSource::Catalog { version: Some(ref v), .. } if v == "abc"
        ));
    }

    #[test]
    fn checks_its_arguments() {
        assert!(parse_request("p.mrpack", &["--version", "1"]).is_err());
        assert!(parse_request("p", &["--name"]).is_err());
        assert!(parse_request("p", &["--nope", "1"]).is_err());
        assert!(parse_request("p", &["--channel", "nightly"]).is_err());
    }
}
