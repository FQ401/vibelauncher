//! `storage …`: where the data is kept, moving a section, cleanup.

use std::path::PathBuf;
use std::time::Duration;

use vl_core::log::LogSource;
use vl_core::storage::{
    MoveMode, MoveRequest, PathCheck, PathIssue, RootSource, Section, StorageCommand,
};
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::{ProgressPrinter, human_bytes};

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(500);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["show"] => show(facade, false, out),
        ["show", "--sizes"] => show(facade, true, out),
        ["check", section, path] => {
            let check = check(facade, section.parse()?, PathBuf::from(path))?;
            print_check(&check, out)
        }
        ["move", section, path, flags @ ..] => {
            let section: Section = section.parse()?;
            let (mode, yes) = flags_of(flags)?;
            let target = PathBuf::from(path);
            let check = check(facade, section, target.clone())?;
            print_check(&check, out)?;
            if !check.usable() {
                return Err("the folder cannot be used".into());
            }
            if !check.issues.is_empty() && !yes {
                return Err("there are warnings; add --yes to move anyway".into());
            }
            move_section(facade, section, Some(target), mode, out)
        }
        ["reset", section, flags @ ..] => {
            let (mode, _) = flags_of(flags)?;
            move_section(facade, section.parse()?, None, mode, out)
        }
        ["cleanup"] => cleanup(facade, false, out),
        ["cleanup", "--trash"] => cleanup(facade, true, out),
        _ => Err(USAGE.into()),
    }
}

fn flags_of(flags: &[&str]) -> Result<(MoveMode, bool), CliError> {
    let mut mode = MoveMode::Copy;
    let mut yes = false;
    for flag in flags {
        match *flag {
            "--switch-only" => mode = MoveMode::SwitchOnly,
            "--yes" => yes = true,
            _ => return Err(USAGE.into()),
        }
    }
    Ok((mode, yes))
}

fn show(facade: &dyn Facade, sizes: bool, out: Out<'_>) -> Result<(), CliError> {
    let info = match call(facade, Command::Storage(StorageCommand::Show { sizes }))? {
        Response::Storage(info) => info,
        other => return Err(unexpected(&other)),
    };
    let source = match info.source {
        RootSource::Argument => "--dir",
        RootSource::Environment => "VIBELAUNCHER_DIR",
        RootSource::Portable => "portable marker",
        RootSource::Pointer => "pointer file",
        _ => "default",
    };
    writeln!(out, "data folder: {} ({source})", info.root.display())?;
    for s in &info.sections {
        let size = s
            .size
            .map(|b| format!("  {}", human_bytes(b)))
            .unwrap_or_default();
        let place = if s.default { "" } else { "  [moved]" };
        let missing = if s.available { "" } else { "  [unavailable]" };
        writeln!(
            out,
            "  {:<10} {}{size}{place}{missing}",
            s.section.as_str(),
            s.path.display()
        )?;
    }
    if !info.leftovers.is_empty() {
        writeln!(out, "old copies (removed by `storage cleanup`):")?;
        for l in &info.leftovers {
            let size = l
                .size
                .map(|b| format!("  {}", human_bytes(b)))
                .unwrap_or_default();
            writeln!(
                out,
                "  {:<10} {}{size}",
                l.section.as_str(),
                l.path.display()
            )?;
        }
    }
    if info.restart_required {
        writeln!(out, "a section was moved: restart the launcher")?;
    }
    Ok(())
}

fn check(facade: &dyn Facade, section: Section, path: PathBuf) -> Result<PathCheck, CliError> {
    match call(
        facade,
        Command::Storage(StorageCommand::Check { section, path }),
    )? {
        Response::PathCheck(check) => Ok(check),
        other => Err(unexpected(&other)),
    }
}

fn print_check(check: &PathCheck, out: Out<'_>) -> Result<(), CliError> {
    let free = check
        .free
        .map(|b| format!(" ({} free)", human_bytes(b)))
        .unwrap_or_default();
    writeln!(out, "{}{free}", check.path.display())?;
    for issue in &check.issues {
        let kind = if issue.is_error() { "error" } else { "warning" };
        writeln!(out, "  {kind}: {}", describe(issue))?;
    }
    Ok(())
}

fn describe(issue: &PathIssue) -> String {
    match issue {
        PathIssue::NotWritable { detail } => format!("cannot write there ({detail})"),
        PathIssue::ProgramFiles => "inside the folder for installed programs".into(),
        PathIssue::SystemFolder => "inside an operating system folder".into(),
        PathIssue::SystemDriveRoot => "the root of the system drive".into(),
        PathIssue::Overlaps { section } => format!("overlaps the folder of {section}"),
        PathIssue::NotEmpty => "the folder is not empty".into(),
        PathIssue::Missing => "the folder does not hold the data yet".into(),
        PathIssue::NotEnoughSpace { free, needed } => format!(
            "not enough space: {} free, {} needed",
            human_bytes(*free),
            human_bytes(*needed)
        ),
        PathIssue::CloudFolder { service } => {
            format!("synchronized by {service}: files may be locked or kept online only")
        }
        PathIssue::NetworkDrive => "a network drive".into(),
        PathIssue::RemovableDrive => {
            "a removable drive: the data is missing while it is out".into()
        }
        PathIssue::LongPath { length, limit } => {
            format!("a long path ({length} characters, more than {limit})")
        }
        PathIssue::NonAscii => "characters outside ASCII: some Java versions mishandle them".into(),
        PathIssue::LowSpace { free } => format!("little free space ({})", human_bytes(*free)),
        other => format!("{other:?}"),
    }
}

fn move_section(
    facade: &dyn Facade,
    section: Section,
    target: Option<PathBuf>,
    mode: MoveMode,
    out: Out<'_>,
) -> Result<(), CliError> {
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let request = MoveRequest {
        section,
        target,
        mode,
    };
    let task = match call(facade, Command::Storage(StorageCommand::Move(request)))? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "move");
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    writeln!(out, "[move] done")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("move cancelled; nothing was changed".into()),
                None => {}
            },
            Event::Log(line) if line.source == LogSource::Launcher => {
                writeln!(out, "[move] {}", line.text)?;
            }
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

fn cleanup(facade: &dyn Facade, trash: bool, out: Out<'_>) -> Result<(), CliError> {
    let report = match call(facade, Command::Storage(StorageCommand::Cleanup { trash }))? {
        Response::StorageCleaned(report) => report,
        other => return Err(unexpected(&other)),
    };
    for path in &report.removed {
        writeln!(out, "removed {}", path.display())?;
    }
    for (path, why) in &report.failed {
        writeln!(out, "kept {}: {why}", path.display())?;
    }
    writeln!(out, "freed {}", human_bytes(report.freed))?;
    Ok(())
}
