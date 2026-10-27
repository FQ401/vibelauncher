//! Command groups; each group lives in its own module.

mod accounts;
mod info;
mod instances;
mod java;
mod launch;
mod versions;

use std::fmt;
use std::io::{self, Write};

use vl_core::{Command, Facade, Response};

pub(crate) const USAGE: &str = "usage: vl-cli [--mock] [--dir <path>] <command>
commands:
  info                                which backend answers
  instances list                      list instances
  instances create <name> <version>   create a vanilla instance
  versions list [--snapshots] [--old] list game versions
  versions install <version>          download a version's files without starting it
  accounts list                       list accounts
  accounts login                      sign in with Microsoft (device code)
  accounts remove <account>           remove an account (id or name)
  accounts refresh <account>          renew a Microsoft sign-in
  java list                           list Java runtimes
  launch <instance> [--offline]       launch and follow the game log";

/// A failed command line.
#[derive(Debug)]
pub(crate) struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<vl_core::Error> for CliError {
    fn from(e: vl_core::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        Self(format!("output: {e}"))
    }
}

impl From<&str> for CliError {
    fn from(e: &str) -> Self {
        Self(e.to_owned())
    }
}

/// Where a command writes its output.
pub(crate) type Out<'a> = &'a mut dyn Write;

/// Runs one command line against `facade`, writing to `out`.
pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["info"] => info::run(facade, out),
        ["instances", rest @ ..] => instances::run(facade, rest, out),
        ["versions", rest @ ..] => versions::run(facade, rest, out),
        ["accounts", rest @ ..] => accounts::run(facade, rest, out),
        ["java", rest @ ..] => java::run(facade, rest, out),
        ["launch", rest @ ..] => launch::run(facade, rest, out),
        _ => Err(USAGE.into()),
    }
}

/// Sends `command` and waits for its response.
pub(crate) fn call(facade: &dyn Facade, command: Command) -> Result<Response, CliError> {
    Ok(facade.call(command).wait()?)
}

/// The error for a response of the wrong kind.
pub(crate) fn unexpected(response: &Response) -> CliError {
    CliError(format!("unexpected response: {response:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_mock::{MockFacade, MockOptions};

    fn run_mock(args: &[&str]) -> Result<String, CliError> {
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        run(&facade, args, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn lists_instances() {
        let out = run_mock(&["instances", "list"]).unwrap();
        assert!(out.contains("survival"), "{out}");
        assert!(out.contains("Broken pack"), "{out}");
    }

    #[test]
    fn launches_and_prints_progress_and_the_game_log() {
        let out = run_mock(&["launch", "survival"]).unwrap();
        assert!(out.contains("libraries 12/12"), "{out}");
        assert!(out.contains("Mock game line 5"), "{out}");
        assert!(out.contains("exited with code 0"), "{out}");
    }

    #[test]
    fn reports_usage_and_errors() {
        assert!(run_mock(&["nonsense"]).is_err());
        let err = run_mock(&["launch", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }
}
