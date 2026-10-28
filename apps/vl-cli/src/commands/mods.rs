//! `instances mods …`: the files in an instance's mods, packs and shaders folders.

use vl_core::catalog::CatalogCommand;
use vl_core::install::{
    IdentifyReport, MinStability, UncheckedReason, UpdateCheckRequest, UpdateReport, UpdateRequest,
};
use vl_core::resources::{ResourceCommand, ResourceEntry, ResourceKind};
use vl_core::{Command, Facade, InstanceId, Response};

use super::catalog::{confirmed, print_plan, run_task};
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
        ["updates", instance, flags @ ..] => {
            let (check, rest) = check_request(instance, flags)?;
            if !rest.is_empty() {
                return Err(USAGE.into());
            }
            match call(
                facade,
                Command::Catalog(CatalogCommand::CheckUpdates(check)),
            )? {
                Response::UpdateReport(report) => print_report(&report, out),
                other => Err(unexpected(&other)),
            }
        }
        ["identify", instance] => {
            let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
            let command = Command::Catalog(CatalogCommand::IdentifyMods {
                instance,
                provider: Default::default(),
            });
            match call(facade, command)? {
                Response::Identified(report) => print_identified(&report, out),
                other => Err(unexpected(&other)),
            }
        }
        ["update", instance, flags @ ..] => update(facade, instance, flags, out),
        _ => Err(USAGE.into()),
    }
}

/// `update <instance> [<mod>...|--all] [--version <id>] [--channel c] [--include-pack-files]
/// [--yes]`: prints the plan, asks (or takes `--yes`), then follows the task.
fn update(
    facade: &dyn Facade,
    instance: &str,
    flags: &[&str],
    out: Out<'_>,
) -> Result<(), CliError> {
    let (check, rest) = check_request(instance, flags)?;
    let mut request = UpdateRequest {
        check,
        all: false,
        files: Vec::new(),
        version: None,
    };
    let mut yes = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--all" => request.all = true,
            "--yes" => yes = true,
            "--version" => {
                let v = it.next().ok_or(CliError::from(USAGE))?;
                request.version = Some((*v).to_owned());
            }
            name if !name.starts_with("--") => request.files.push(name.to_owned()),
            _ => return Err(USAGE.into()),
        }
    }
    if request.all != request.files.is_empty() {
        return Err("name the mods to update, or pass --all".into());
    }
    let planned = call(
        facade,
        Command::Catalog(CatalogCommand::PlanUpdate(request.clone())),
    )?;
    let plan = match planned {
        Response::InstallPlan(plan) => plan,
        other => return Err(unexpected(&other)),
    };
    print_plan(&request.check.instance, &plan, out)?;
    if plan.downloads().next().is_none() {
        writeln!(out, "nothing to update")?;
        return Ok(());
    }
    if !yes && !confirmed(out)? {
        writeln!(out, "not updated; add --yes to update")?;
        return Ok(());
    }
    run_task(
        facade,
        Command::Catalog(CatalogCommand::Update(request)),
        out,
    )
}

/// The instance and the check flags (`--channel`, `--include-pack-files`); the rest is returned.
fn check_request<'a>(
    instance: &str,
    flags: &[&'a str],
) -> Result<(UpdateCheckRequest, Vec<&'a str>), CliError> {
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let mut check = UpdateCheckRequest::new(id);
    let mut rest = Vec::new();
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--include-pack-files" => check.include_pack_files = true,
            "--channel" => {
                let v = it.next().ok_or(CliError::from(USAGE))?;
                check.min_stability = MinStability::parse(v)
                    .ok_or_else(|| CliError::from("channel is release, beta or alpha"))?;
            }
            other => rest.push(other),
        }
    }
    Ok((check, rest))
}

fn print_report(report: &UpdateReport, out: Out<'_>) -> Result<(), CliError> {
    writeln!(
        out,
        "Minecraft {}, {}:",
        report.game_version,
        report.loader.as_deref().unwrap_or("no loader")
    )?;
    for u in &report.updates {
        writeln!(
            out,
            "  update    {} [{}] {} -> {} ({}){}{}",
            u.name,
            u.kind.name(),
            u.current.number,
            u.latest.number,
            u.file,
            if u.enabled { "" } else { " [disabled]" },
            if u.from_pack { " [from pack]" } else { "" }
        )?;
    }
    for f in &report.from_pack {
        writeln!(
            out,
            "  from pack {} [{}] (skipped; use --include-pack-files)",
            f.file,
            f.kind.name()
        )?;
    }
    for f in &report.unchecked {
        let why = match f.reason {
            UncheckedReason::NoOrigin => "origin unknown; try identify",
            UncheckedReason::UnknownToPlatform => "the platform no longer knows this file",
            UncheckedReason::NoLoader => "the instance has no mod loader",
            _ => "not checked",
        };
        writeln!(out, "  unchecked {} [{}]: {why}", f.file, f.kind.name())?;
    }
    for w in &report.warnings {
        writeln!(out, "  warning   {w}")?;
    }
    writeln!(
        out,
        "{} update(s), {} up to date, {} from pack, {} not checked",
        report.updates.len(),
        report.up_to_date,
        report.from_pack.len(),
        report.unchecked.len()
    )?;
    Ok(())
}

fn print_identified(report: &IdentifyReport, out: Out<'_>) -> Result<(), CliError> {
    for f in &report.identified {
        writeln!(
            out,
            "  identified {} [{}]: project {} {}",
            f.file,
            f.kind.name(),
            f.project_id,
            f.version.number
        )?;
    }
    for f in &report.unknown {
        writeln!(out, "  unknown    {} [{}]", f.file, f.kind.name())?;
    }
    for w in &report.warnings {
        writeln!(out, "  warning    {w}")?;
    }
    writeln!(
        out,
        "{} identified, {} already known, {} unknown",
        report.identified.len(),
        report.already_known,
        report.unknown.len()
    )?;
    Ok(())
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
    fn update_flags_are_checked_before_anything_is_asked() {
        assert!(run_mock(&["update", "main"]).is_err(), "mods or --all");
        assert!(run_mock(&["update", "main", "--channel", "nightly", "--all"]).is_err());
        assert!(run_mock(&["update", "main", "--nope"]).is_err());
        assert!(run_mock(&["updates", "main", "extra"]).is_err());
        assert!(run_mock(&["identify"]).is_err());
        // `--all` or names pass the argument check (the mock then has no such instance).
        for args in [
            &["update", "main", "--all"][..],
            &["update", "main", "a", "b"][..],
        ] {
            let err = run_mock(args).unwrap_err().to_string();
            assert!(!err.contains("name the mods"), "{err}");
        }
        assert!(run_mock(&["update", "main", "a", "--all"]).is_err());
    }

    #[test]
    fn prints_a_report() {
        use vl_core::install::{PackFile, UncheckedFile, UpdateCandidate, VersionLabel};
        let label = |n: &str| VersionLabel {
            id: n.into(),
            number: n.into(),
            channel: Default::default(),
        };
        let report = UpdateReport {
            game_version: "1.21.4".into(),
            loader: Some("fabric".into()),
            updates: vec![UpdateCandidate {
                kind: ResourceKind::Mods,
                file: "a-1.jar".into(),
                enabled: false,
                name: "A".into(),
                project_id: "p".into(),
                current: label("1"),
                latest: label("2"),
                from_pack: false,
            }],
            up_to_date: 3,
            from_pack: vec![PackFile {
                kind: ResourceKind::Mods,
                file: "b.jar".into(),
            }],
            unchecked: vec![UncheckedFile {
                kind: ResourceKind::Mods,
                file: "c.jar".into(),
                reason: UncheckedReason::NoOrigin,
            }],
            warnings: Vec::new(),
        };
        let mut out = Vec::new();
        print_report(&report, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("update    A [mods] 1 -> 2 (a-1.jar) [disabled]"),
            "{out}"
        );
        assert!(out.contains("from pack b.jar"), "{out}");
        assert!(
            out.contains("unchecked c.jar [mods]: origin unknown"),
            "{out}"
        );
        assert!(
            out.contains("1 update(s), 3 up to date, 1 from pack, 1 not checked"),
            "{out}"
        );
    }
}
