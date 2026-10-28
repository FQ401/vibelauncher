//! `packs install <file.mrpack|file.zip|slug> [--provider <p>] [--name X] [--version <id>]
//! [--group G] [--channel <c>]`,
//! `packs updates <instance>` and `packs update <instance> ...`.

use std::path::Path;
use std::time::Duration;

use vl_core::catalog::{CatalogCommand, Provider};
use vl_core::install::{
    ExportInclude, MinStability, PackAction, PackChange, PackChanges, PackExportRequest,
    PackInstallRequest, PackSource, PackUpdateInfo, PackUpdateRequest,
};
use vl_core::instance::InstanceEvent;
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::catalog::confirmed_with;
use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::ProgressPrinter;

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(250);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["install", pack, flags @ ..] => install(facade, pack, flags, out),
        ["updates", instance, flags @ ..] => {
            let request = parse_update(instance, flags)?.0;
            let command = Command::Catalog(CatalogCommand::CheckPackUpdate(request));
            match call(facade, command)? {
                Response::PackUpdate(info) => print_info(&info, out),
                other => Err(unexpected(&other)),
            }
        }
        ["update", instance, flags @ ..] => update(facade, instance, flags, out),
        ["export", instance, flags @ ..] => export(facade, instance, flags, out),
        _ => Err(USAGE.into()),
    }
}

/// A path that names a file (or ends in `.mrpack` or `.zip`) is a local pack, Modrinth or
/// CurseForge (the launcher tells them apart by content); anything else is a project of
/// `provider`.
fn parse_source(
    pack: &str,
    provider: Option<Provider>,
    version: Option<String>,
) -> Result<PackSource, CliError> {
    let lower = pack.to_ascii_lowercase();
    let is_file =
        lower.ends_with(".mrpack") || lower.ends_with(".zip") || Path::new(pack).is_file();
    if is_file {
        if version.is_some() {
            return Err("--version applies to catalog packs, not files".into());
        }
        if provider.is_some() {
            return Err("--provider applies to catalog packs, not files".into());
        }
        let path = std::path::absolute(pack)
            .map_err(|e| CliError::from(format!("{pack}: {e}")))?
            .to_string_lossy()
            .into_owned();
        Ok(PackSource::File { path })
    } else {
        Ok(PackSource::Catalog {
            provider: provider.unwrap_or_default(),
            project: pack.to_owned(),
            version,
        })
    }
}

fn parse_request(pack: &str, flags: &[&str]) -> Result<PackInstallRequest, CliError> {
    let (mut name, mut version, mut group, mut provider) = (None, None, None, None);
    let mut min_stability = MinStability::default();
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(CliError::from(USAGE))?;
        match *flag {
            "--name" => name = Some((*value).to_owned()),
            "--version" => version = Some((*value).to_owned()),
            "--group" => group = Some((*value).to_owned()),
            "--provider" => provider = Some(parse_provider(value)?),
            "--channel" => {
                min_stability = MinStability::parse(value)
                    .ok_or_else(|| CliError::from("channel is release, beta or alpha"))?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(PackInstallRequest {
        source: parse_source(pack, provider, version)?,
        name,
        group,
        min_stability,
    })
}

pub(crate) fn parse_provider(name: &str) -> Result<Provider, CliError> {
    match name.to_ascii_lowercase().as_str() {
        "modrinth" => Ok(Provider::Modrinth),
        "curseforge" => Ok(Provider::CurseForge),
        _ => Err("provider is modrinth or curseforge".into()),
    }
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
                                "[pack] installed as instance {} ({}, Minecraft {}, {})",
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

/// The request of `packs export`.
fn parse_export(instance: &str, flags: &[&str]) -> Result<PackExportRequest, CliError> {
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let mut request = PackExportRequest::new(id, String::new());
    let mut out = None;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = (*it.next().ok_or(CliError::from(USAGE))?).to_owned();
        match *flag {
            "--out" => {
                let path = std::path::absolute(&value)
                    .map_err(|e| CliError::from(format!("{value}: {e}")))?;
                out = Some(path.to_string_lossy().into_owned());
            }
            "--name" => request.name = Some(value),
            "--version" => request.version = Some(value),
            "--summary" => request.summary = Some(value),
            "--include" => {
                request.include = value
                    .split(',')
                    .filter(|p| !p.is_empty() && *p != "none")
                    .map(|p| {
                        ExportInclude::parse(p).ok_or_else(|| {
                            CliError::from(
                                "include is a comma-separated list of config, kubejs,                                  resourcepacks, shaderpacks, options (or none)",
                            )
                        })
                    })
                    .collect::<Result<_, _>>()?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    request.out = out.ok_or(CliError::from(USAGE))?;
    Ok(request)
}

/// `export <instance> --out <file>`: writes the pack and says what went in.
fn export(
    facade: &dyn Facade,
    instance: &str,
    flags: &[&str],
    out: Out<'_>,
) -> Result<(), CliError> {
    let request = parse_export(instance, flags)?;
    let command = Command::Catalog(CatalogCommand::ExportPack(request));
    let Response::PackExported(report) = call(facade, command)? else {
        return Err("unexpected answer to the export".into());
    };
    writeln!(
        out,
        "wrote {} ({} KiB): {} files linked to the catalog, {} files inside the pack",
        report.path,
        report.size.div_ceil(1024),
        report.linked,
        report.bundled
    )?;
    if report.disabled > 0 {
        writeln!(out, "{} disabled files were left out", report.disabled)?;
    }
    for path in &report.unlisted {
        writeln!(out, "not in the catalog, carried inside the pack: {path}")?;
    }
    for w in &report.warnings {
        writeln!(out, "warning: {w}")?;
    }
    Ok(())
}

/// The request and `--yes` of `packs update`.
fn parse_update(instance: &str, flags: &[&str]) -> Result<(PackUpdateRequest, bool), CliError> {
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let mut request = PackUpdateRequest::new(id);
    let mut yes = false;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--reset" => request.reset = true,
            "--yes" => yes = true,
            "--version" | "--file" | "--channel" => {
                let value = it.next().ok_or(CliError::from(USAGE))?;
                match *flag {
                    "--version" => request.version = Some((*value).to_owned()),
                    "--file" => {
                        let path = std::path::absolute(value)
                            .map_err(|e| CliError::from(format!("{value}: {e}")))?;
                        request.file = Some(path.to_string_lossy().into_owned());
                    }
                    _ => {
                        request.min_stability = MinStability::parse(value)
                            .ok_or_else(|| CliError::from("channel is release, beta or alpha"))?;
                    }
                }
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok((request, yes))
}

fn print_info(info: &PackUpdateInfo, out: Out<'_>) -> Result<(), CliError> {
    writeln!(out, "{}: installed {}", info.name, info.current.number)?;
    match &info.latest {
        Some(latest) => writeln!(
            out,
            "update available: {} ({:?}); run `packs update` to see what changes",
            latest.number, latest.channel
        )?,
        None if info.from_file => writeln!(
            out,
            "installed from a file; update it with `packs update <instance> --file <pack file>`"
        )?,
        None => writeln!(out, "up to date")?,
    }
    for w in &info.warnings {
        writeln!(out, "warning: {w}")?;
    }
    Ok(())
}

fn print_group(
    out: Out<'_>,
    title: &str,
    changes: &PackChanges,
    pick: impl Fn(&PackChange) -> bool,
) -> Result<(), CliError> {
    let items: Vec<&PackChange> = changes.changes.iter().filter(|c| pick(c)).collect();
    if items.is_empty() {
        return Ok(());
    }
    writeln!(out, "{title}:")?;
    for c in items {
        write!(out, "  {}", c.path)?;
        if c.disabled {
            write!(out, " (disabled)")?;
        }
        if let Some(note) = &c.note {
            write!(out, " - {note}")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Prints what an update changes, grouped by what happens to the files.
fn print_changes(changes: &PackChanges, out: Out<'_>) -> Result<(), CliError> {
    writeln!(
        out,
        "{}: {} -> {}{}",
        changes.name,
        changes.from,
        changes.to,
        if changes.reset { " (reset)" } else { "" }
    )?;
    if let Some(g) = &changes.game_version {
        writeln!(out, "Minecraft: {} -> {}", g.from, g.to)?;
    }
    if let Some(l) = &changes.loader {
        writeln!(out, "loader: {} -> {}", l.from, l.to)?;
    }
    let is = |c: &PackChange, a: PackAction, d: bool| c.action == a && c.download == d;
    type Pick<'a> = &'a dyn Fn(&PackChange) -> bool;
    let groups: [(&str, Pick<'_>); 9] = [
        ("mods and resources to add", &|c| {
            is(c, PackAction::Add, true)
        }),
        ("mods and resources to replace", &|c| {
            is(c, PackAction::Update, true)
        }),
        ("mods and resources to remove", &|c| {
            is(c, PackAction::Remove, true)
        }),
        ("mods and resources to enable", &|c| {
            is(c, PackAction::Enable, true)
        }),
        ("configs and other files to add", &|c| {
            is(c, PackAction::Add, false)
        }),
        ("configs and other files to replace", &|c| {
            is(c, PackAction::Update, false) || is(c, PackAction::Enable, false)
        }),
        ("configs and other files to remove", &|c| {
            is(c, PackAction::Remove, false)
        }),
        (
            "configs you changed (the new version is saved beside them)",
            &|c| c.action == PackAction::SaveNew,
        ),
        ("kept as you left them", &|c| {
            matches!(c.action, PackAction::KeepDeleted | PackAction::KeepModified)
        }),
    ];
    for (title, pick) in groups {
        print_group(out, title, changes, pick)?;
    }
    writeln!(
        out,
        "{} files of the pack stay as they are; download {} MiB",
        changes.unchanged,
        changes.download_bytes.div_ceil(1 << 20)
    )?;
    for w in &changes.warnings {
        writeln!(out, "warning: {w}")?;
    }
    Ok(())
}

/// `update <instance> ...`: prints the plan, asks (or takes `--yes`), then follows the task.
fn update(
    facade: &dyn Facade,
    instance: &str,
    flags: &[&str],
    out: Out<'_>,
) -> Result<(), CliError> {
    let (request, yes) = parse_update(instance, flags)?;
    let planned = call(
        facade,
        Command::Catalog(CatalogCommand::PlanPackUpdate(request.clone())),
    )?;
    let Response::PackChanges(changes) = planned else {
        return Err(unexpected(&planned));
    };
    print_changes(&changes, out)?;
    if changes.is_empty() && !request.reset {
        writeln!(out, "nothing to update")?;
        return Ok(());
    }
    if !yes && !confirmed_with(out, "update?")? {
        writeln!(out, "not updated; add --yes to update")?;
        return Ok(());
    }
    let events = facade.subscribe();
    let command = Command::Catalog(CatalogCommand::UpdatePack(request));
    let task = match call(facade, command)? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "pack");
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    writeln!(out, "[pack] done")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("pack update cancelled".into()),
                None => {}
            },
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
    fn parses_export_flags() {
        let r = parse_export(
            "inst",
            &[
                "--out",
                "a.mrpack",
                "--name",
                "N",
                "--include",
                "config,kubejs",
            ],
        )
        .unwrap();
        assert!(r.out.ends_with("a.mrpack"));
        assert_eq!(r.name.as_deref(), Some("N"));
        assert_eq!(r.include, [ExportInclude::Config, ExportInclude::Kubejs]);
        assert!(parse_export("inst", &["--name", "N"]).is_err());
        assert!(parse_export("inst", &["--out", "a", "--include", "saves"]).is_err());
        let r = parse_export("inst", &["--out", "a", "--include", "none"]).unwrap();
        assert!(r.include.is_empty());
    }

    #[test]
    fn tells_files_from_projects() {
        let r = parse_request("pack.mrpack", &["--name", "X"]).unwrap();
        assert!(matches!(r.source, PackSource::File { .. }));
        assert_eq!(r.name.as_deref(), Some("X"));
        let r = parse_request("fabulously-optimized", &["--version", "abc"]).unwrap();
        assert!(matches!(
            r.source,
            PackSource::Catalog { version: Some(ref v), provider: Provider::Modrinth, .. }
                if v == "abc"
        ));
        let r = parse_request("pack.zip", &[]).unwrap();
        assert!(matches!(r.source, PackSource::File { .. }));
        let r = parse_request("396246", &["--provider", "CurseForge", "--version", "7"]).unwrap();
        assert!(matches!(
            r.source,
            PackSource::Catalog { provider: Provider::CurseForge, version: Some(ref v), .. }
                if v == "7"
        ));
    }

    #[test]
    fn parses_update_flags() {
        let (r, yes) = parse_update("inst", &["--reset", "--yes", "--version", "v2"]).unwrap();
        assert!(r.reset && yes);
        assert_eq!(r.version.as_deref(), Some("v2"));
        let (r, _) = parse_update("inst", &["--file", "p.mrpack", "--channel", "beta"]).unwrap();
        assert!(r.file.is_some());
        assert_eq!(r.min_stability, MinStability::Beta);
        assert!(parse_update("inst", &["--version"]).is_err());
        assert!(parse_update("inst", &["--nope"]).is_err());
    }

    #[test]
    fn checks_its_arguments() {
        assert!(parse_request("p.mrpack", &["--version", "1"]).is_err());
        assert!(parse_request("p", &["--name"]).is_err());
        assert!(parse_request("p", &["--nope", "1"]).is_err());
        assert!(parse_request("p", &["--channel", "nightly"]).is_err());
        assert!(parse_request("p", &["--provider", "other"]).is_err());
        assert!(parse_request("p.zip", &["--provider", "curseforge"]).is_err());
    }
}
