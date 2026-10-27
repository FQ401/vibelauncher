//! `versions …`.

use vl_core::version::{VersionCommand, VersionFilter};
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let ["list", flags @ ..] = args else {
        return Err(USAGE.into());
    };
    let mut filter = VersionFilter::default();
    for flag in flags {
        match *flag {
            "--snapshots" => filter.snapshots = true,
            "--old" => filter.old = true,
            _ => return Err(USAGE.into()),
        }
    }
    match call(facade, Command::Versions(VersionCommand::List(filter)))? {
        Response::Versions(versions) => {
            for v in versions {
                writeln!(out, "{:<20} {:?}  {}", v.id, v.kind, v.release_time)?;
            }
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}
