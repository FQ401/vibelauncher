//! `instances delete …` and `trash list|restore|purge`: the launcher's trash of deleted
//! instances.

use vl_core::instance::InstanceCommand;
use vl_core::trash::{PurgeReport, PurgeTarget, TrashList};
use vl_core::{Command, Facade, InstanceId, Response};

use super::catalog::confirmed_with;
use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::human_bytes;

/// `instances delete <instance> [--comment <text>] [--permanent] [--yes]`.
pub(crate) fn delete(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let [instance, flags @ ..] = args else {
        return Err(USAGE.into());
    };
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let mut comment = String::new();
    let mut permanent = false;
    let mut yes = false;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--comment" => (*it.next().ok_or(CliError::from(USAGE))?).clone_into(&mut comment),
            "--permanent" => permanent = true,
            "--yes" => yes = true,
            _ => return Err(USAGE.into()),
        }
    }
    if permanent && !yes && !confirmed_with(out, &format!("erase {id} for good?"))? {
        writeln!(out, "not deleted; add --yes to erase it")?;
        return Ok(());
    }
    let command = Command::Instances(InstanceCommand::Delete {
        id: id.clone(),
        comment,
        permanent,
    });
    match call(facade, command)? {
        Response::Done if permanent => writeln!(out, "{id} erased")?,
        Response::Done => writeln!(
            out,
            "{id} moved to the trash; `trash restore` brings it back"
        )?,
        other => return Err(unexpected(&other)),
    }
    Ok(())
}

/// `trash …`.
pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["list"] => list(facade, out),
        ["restore", item] => {
            let command = Command::Instances(InstanceCommand::Restore {
                item: (*item).to_owned(),
            });
            match call(facade, command)? {
                Response::Instance(i) => {
                    writeln!(out, "restored {item} as {} ({})", i.id, i.name)?;
                    Ok(())
                }
                other => Err(unexpected(&other)),
            }
        }
        ["purge", rest @ ..] => {
            let (target, yes) = purge_target(rest)?;
            let question = match &target {
                PurgeTarget::Item(item) => Some(format!("erase {item} for good?")),
                PurgeTarget::All => Some("erase everything in the trash for good?".to_owned()),
                _ => None,
            };
            if let Some(question) = question
                && !yes
                && !confirmed_with(out, &question)?
            {
                writeln!(out, "nothing erased; add --yes to erase")?;
                return Ok(());
            }
            match call(facade, Command::Instances(InstanceCommand::Purge(target)))? {
                Response::TrashPurged(report) => print_purge(&report, out),
                other => Err(unexpected(&other)),
            }
        }
        _ => Err(USAGE.into()),
    }
}

/// `[<item>|--all] [--yes]`; nothing is the limits of the settings.
fn purge_target(args: &[&str]) -> Result<(PurgeTarget, bool), CliError> {
    let mut target = None;
    let mut yes = false;
    for arg in args {
        match *arg {
            "--yes" => yes = true,
            "--all" if target.is_none() => target = Some(PurgeTarget::All),
            item if target.is_none() && !item.starts_with("--") => {
                target = Some(PurgeTarget::Item(item.to_owned()));
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok((target.unwrap_or(PurgeTarget::Limits), yes))
}

fn list(facade: &dyn Facade, out: Out<'_>) -> Result<(), CliError> {
    let trash: TrashList = match call(facade, Command::Instances(InstanceCommand::ListTrash))? {
        Response::Trash(trash) => trash,
        other => return Err(unexpected(&other)),
    };
    writeln!(
        out,
        "trash: {} item(s), {} (kept {} days, up to {} GiB)",
        trash.items.len(),
        human_bytes(trash.size),
        trash.keep_days,
        trash.max_gib
    )?;
    for item in &trash.items {
        writeln!(
            out,
            "  {:<20} {:<24} {:<10} {:>10}  deleted {} UTC",
            item.id,
            item.name,
            item.game_version,
            human_bytes(item.size),
            utc(item.deleted_unix)
        )?;
        if !item.comment.is_empty() {
            writeln!(out, "      comment: {}", item.comment)?;
        }
        if let Some(group) = &item.group {
            writeln!(out, "      group: {group}")?;
        }
        if let Some(problem) = &item.problem {
            writeln!(out, "      [problem: {problem}]")?;
        }
    }
    Ok(())
}

fn print_purge(report: &PurgeReport, out: Out<'_>) -> Result<(), CliError> {
    for item in &report.removed {
        writeln!(out, "erased {item}")?;
    }
    for (item, why) in &report.failed {
        writeln!(out, "kept {item}: {why}")?;
    }
    writeln!(out, "freed {}", human_bytes(report.freed))?;
    Ok(())
}

/// `YYYY-MM-DD HH:MM` of a Unix time, in UTC.
fn utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_mock::{MockFacade, MockOptions};

    fn run_on(facade: &MockFacade, args: &[&str]) -> Result<String, CliError> {
        let mut out = Vec::new();
        super::super::run(facade, args, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn delete_list_restore_and_purge() {
        let facade = MockFacade::new(MockOptions::instant());
        let out = run_on(
            &facade,
            &["instances", "delete", "survival", "--comment", "too slow"],
        )
        .unwrap();
        assert!(out.contains("moved to the trash"), "{out}");
        let out = run_on(&facade, &["instances", "list"]).unwrap();
        assert!(!out.contains("survival "), "{out}");
        let out = run_on(&facade, &["trash", "list"]).unwrap();
        assert!(out.contains("1 item(s)"), "{out}");
        assert!(out.contains("comment: too slow"), "{out}");
        assert!(out.contains("kept 30 days, up to 20 GiB"), "{out}");
        let out = run_on(&facade, &["trash", "restore", "survival"]).unwrap();
        assert!(out.contains("restored survival as survival"), "{out}");

        run_on(&facade, &["instances", "delete", "survival"]).unwrap();
        // Without a terminal and without --yes nothing is erased.
        let out = run_on(&facade, &["trash", "purge", "--all"]).unwrap();
        assert!(out.contains("nothing erased"), "{out}");
        let out = run_on(&facade, &["trash", "purge", "--all", "--yes"]).unwrap();
        assert!(out.contains("erased survival"), "{out}");
        let out = run_on(&facade, &["trash", "purge"]).unwrap();
        assert!(out.contains("freed 0 B"), "{out}");
    }

    #[test]
    fn a_permanent_delete_asks_first() {
        let facade = MockFacade::new(MockOptions::instant());
        let out = run_on(&facade, &["instances", "delete", "survival", "--permanent"]).unwrap();
        assert!(out.contains("not deleted"), "{out}");
        let out = run_on(
            &facade,
            &["instances", "delete", "survival", "--permanent", "--yes"],
        )
        .unwrap();
        assert!(out.contains("survival erased"), "{out}");
        let out = run_on(&facade, &["trash", "list"]).unwrap();
        assert!(out.contains("0 item(s)"), "{out}");
    }

    #[test]
    fn arguments_are_checked() {
        let facade = MockFacade::new(MockOptions::instant());
        assert!(run_on(&facade, &["instances", "delete"]).is_err());
        assert!(run_on(&facade, &["instances", "delete", "survival", "--comment"]).is_err());
        assert!(run_on(&facade, &["instances", "delete", "survival", "--bad"]).is_err());
        assert!(run_on(&facade, &["trash", "purge", "a", "b"]).is_err());
        assert!(run_on(&facade, &["trash", "purge", "--all", "a"]).is_err());
        assert!(run_on(&facade, &["trash"]).is_err());
        let err = run_on(&facade, &["trash", "restore", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }

    #[test]
    fn times_are_shown_in_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00");
        assert_eq!(utc(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(utc(951_825_600), "2000-02-29 12:00");
    }
}
