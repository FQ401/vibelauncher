//! `instances worlds …` and `instances datapacks …`: the worlds of an instance and the data
//! packs inside them.

use vl_core::resources::{ResourceCommand, ResourceEntry, WorldInfo};
use vl_core::{Command, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, mods, unexpected};

/// `worlds list <instance>`.
pub(crate) fn run_worlds(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let ["list", instance] = args else {
        return Err(USAGE.into());
    };
    let instance = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    match call(
        facade,
        Command::Resources(ResourceCommand::Worlds { instance }),
    )? {
        Response::Worlds(worlds) => {
            if worlds.is_empty() {
                writeln!(out, "no worlds; create a world in the game first")?;
            }
            for world in &worlds {
                writeln!(out, "{}", describe_world(world))?;
            }
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

/// `datapacks <instance> <world> list|enable|disable|add|remove|updates|update|identify …`.
pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let [instance, world, verb, rest @ ..] = args else {
        return Err(USAGE.into());
    };
    let id = InstanceId::new(instance).ok_or(CliError::from(USAGE))?;
    let world_name = (*world).to_owned();
    let command = |c: fn(InstanceId, String) -> ResourceCommand| {
        Command::Resources(c(id.clone(), world_name.clone()))
    };
    match (*verb, rest) {
        ("list", []) => match call(
            facade,
            command(|instance, world| ResourceCommand::WorldList { instance, world }),
        )? {
            Response::Resources(entries) => {
                if entries.is_empty() {
                    writeln!(out, "no data packs in {world}")?;
                }
                for entry in &entries {
                    writeln!(out, "{}", describe(entry))?;
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
        },
        ("enable" | "disable", [file]) => {
            let enabled = *verb == "enable";
            let c = ResourceCommand::WorldSetEnabled {
                instance: id,
                world: world_name,
                file: (*file).to_owned(),
                enabled,
            };
            done(facade, c)?;
            writeln!(out, "{file} {verb}d")?;
            Ok(())
        }
        ("add", [source]) => {
            let c = ResourceCommand::WorldAdd {
                instance: id,
                world: world_name,
                source: (*source).to_owned(),
            };
            done(facade, c)?;
            writeln!(out, "added {source}")?;
            Ok(())
        }
        ("remove", [file]) => {
            let c = ResourceCommand::WorldRemove {
                instance: id,
                world: world_name,
                file: (*file).to_owned(),
            };
            done(facade, c)?;
            writeln!(out, "{file} moved to the instance trash")?;
            Ok(())
        }
        ("updates" | "update" | "identify", rest) => {
            // The same commands as for mods, aimed at the world's data packs.
            let mut forwarded = vec![*verb, *instance, "--world", *world];
            forwarded.extend_from_slice(rest);
            mods::run(facade, &forwarded, out)
        }
        _ => Err(USAGE.into()),
    }
}

fn done(facade: &dyn Facade, command: ResourceCommand) -> Result<(), CliError> {
    match call(facade, Command::Resources(command))? {
        Response::Done => Ok(()),
        other => Err(unexpected(&other)),
    }
}

fn describe_world(world: &WorldInfo) -> String {
    let mut line = world.name.clone();
    if let Some(shown) = &world.display_name {
        line.push_str(&format!(" (\"{shown}\")"));
    }
    line.push_str(&format!(
        "  version {}",
        world.version.as_deref().unwrap_or("?")
    ));
    if let Some(t) = world.last_played {
        line.push_str(&format!("  last played {}", format_time(t)));
    }
    line.push_str(&format!("  {} data pack(s)", world.datapacks));
    line
}

fn describe(entry: &ResourceEntry) -> String {
    let state = if entry.enabled { "on " } else { "off" };
    let mut line = format!("[{state}] {}", entry.file);
    if let Some(pack) = &entry.pack {
        match (pack.pack_format, pack.min_format, pack.max_format) {
            (Some(f), _, _) => line.push_str(&format!("  format {f}")),
            (None, Some(lo), Some(hi)) if lo != hi => {
                line.push_str(&format!("  format {lo}-{hi}"));
            }
            (None, Some(f), _) | (None, None, Some(f)) => {
                line.push_str(&format!("  format {f}"));
            }
            _ => {}
        }
        if let Some(text) = &pack.description {
            let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            line.push_str(&format!("  {one_line}"));
        }
    }
    if let Some(problem) = &entry.problem {
        line.push_str(&format!("  [problem: {problem}]"));
    }
    line
}

/// `2026-10-28 12:30 UTC` for Unix seconds.
fn format_time(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3_600,
        rest % 3_600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_time() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_700_000_000), "2023-11-14 22:13 UTC");
    }

    #[test]
    fn arguments_are_checked() {
        use vl_mock::{MockFacade, MockOptions};
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        assert!(run_worlds(&facade, &["list"], &mut out).is_err());
        assert!(run(&facade, &["main", "World"], &mut out).is_err());
        assert!(run(&facade, &["main", "World", "frobnicate"], &mut out).is_err());
        assert!(run(&facade, &["main", "World", "enable"], &mut out).is_err());
    }
}
