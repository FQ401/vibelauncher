//! `catalog key set | remove | status [curseforge]`: the user's CurseForge API key.
//!
//! The key is never an argument (the command line of a process is visible to other users and
//! kept in shell history): `set` reads it from a prompt that does not echo when the input is a
//! terminal, else the first line of standard input (`vl-cli catalog key set < key.txt`). The key
//! is never printed.

use std::io::{BufRead, IsTerminal};

use vl_core::catalog::{ApiKeyOrigin, ApiKeyState, ApiKeyStatus, CatalogCommand, Provider};
use vl_core::secret::Secret;
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

/// The longest line read from standard input as a key.
const MAX_LINE: u64 = 4096;

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let (verb, provider) = match args {
        [verb] => (*verb, Provider::CurseForge),
        [verb, name] => (*verb, super::packs::parse_provider(name)?),
        _ => return Err(USAGE.into()),
    };
    if provider != Provider::CurseForge {
        return Err("only CurseForge takes an API key".into());
    }
    match verb {
        "set" => {
            let key = read_key()?;
            done(facade, CatalogCommand::SetApiKey { provider, key })?;
            writeln!(out, "the CurseForge API key is stored in the OS keyring")?;
            status(facade, provider, out)
        }
        "remove" => {
            done(facade, CatalogCommand::RemoveApiKey { provider })?;
            writeln!(out, "the CurseForge API key is removed from the OS keyring")?;
            status(facade, provider, out)
        }
        "status" => status(facade, provider, out),
        _ => Err(USAGE.into()),
    }
}

fn done(facade: &dyn Facade, command: CatalogCommand) -> Result<(), CliError> {
    match call(facade, Command::Catalog(command))? {
        Response::Done => Ok(()),
        other => Err(unexpected(&other)),
    }
}

fn status(facade: &dyn Facade, provider: Provider, out: Out<'_>) -> Result<(), CliError> {
    match call(
        facade,
        Command::Catalog(CatalogCommand::CheckApiKey { provider }),
    )? {
        Response::ApiKey(status) => print_status(&status, out),
        other => Err(unexpected(&other)),
    }
}

/// The key from a prompt without echo, or from the first line of standard input.
fn read_key() -> Result<Secret, CliError> {
    let stdin = std::io::stdin();
    let text = if stdin.is_terminal() {
        rpassword::prompt_password("CurseForge API key (not shown): ")
            .map_err(|e| CliError::from(format!("cannot read the key: {e}")))?
    } else {
        first_line(stdin.lock())?
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("no key was entered".into());
    }
    Ok(Secret::new(text))
}

/// The first line of `input`, without its line end.
fn first_line(input: impl BufRead) -> Result<String, CliError> {
    let mut line = String::new();
    std::io::Read::take(input, MAX_LINE)
        .read_line(&mut line)
        .map_err(|e| CliError::from(format!("cannot read the key: {e}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn print_status(status: &ApiKeyStatus, out: Out<'_>) -> Result<(), CliError> {
    let state = match &status.state {
        ApiKeyState::Missing => "not set".to_owned(),
        ApiKeyState::Valid => "works".to_owned(),
        ApiKeyState::Rejected => "rejected by CurseForge".to_owned(),
        ApiKeyState::Unreachable { detail } => {
            format!("not checked, CurseForge could not be reached ({detail})")
        }
        _ => "unknown".to_owned(),
    };
    let from = match status.source {
        Some(ApiKeyOrigin::Environment) => " (from the VL_CURSEFORGE_API_KEY variable)",
        Some(ApiKeyOrigin::Keyring) => " (from the OS keyring)",
        _ => "",
    };
    writeln!(out, "CurseForge API key: {state}{from}")?;
    if status.environment_overrides_keyring() {
        writeln!(
            out,
            "note: VL_CURSEFORGE_API_KEY is set, so the key in the OS keyring is not used"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    fn printed(status: &ApiKeyStatus) -> String {
        let mut out = Vec::new();
        print_status(status, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn status(state: ApiKeyState, source: Option<ApiKeyOrigin>) -> ApiKeyStatus {
        ApiKeyStatus {
            provider: Provider::CurseForge,
            state,
            source,
            in_environment: source == Some(ApiKeyOrigin::Environment),
            in_keyring: source == Some(ApiKeyOrigin::Keyring),
        }
    }

    #[test]
    fn prints_every_state_and_the_override() {
        let out = printed(&status(ApiKeyState::Missing, None));
        assert_eq!(out, "CurseForge API key: not set\n");
        let out = printed(&status(ApiKeyState::Valid, Some(ApiKeyOrigin::Keyring)));
        assert_eq!(out, "CurseForge API key: works (from the OS keyring)\n");
        let out = printed(&status(ApiKeyState::Rejected, Some(ApiKeyOrigin::Keyring)));
        assert!(out.contains("rejected"), "{out}");
        let unreachable = ApiKeyState::Unreachable {
            detail: "timed out".into(),
        };
        let out = printed(&status(unreachable, Some(ApiKeyOrigin::Keyring)));
        assert!(out.contains("could not be reached (timed out)"), "{out}");

        let mut both = status(ApiKeyState::Valid, Some(ApiKeyOrigin::Environment));
        both.in_keyring = true;
        let out = printed(&both);
        assert!(out.contains("VL_CURSEFORGE_API_KEY variable"), "{out}");
        assert!(out.contains("not used"), "{out}");
    }

    #[test]
    fn reads_only_the_first_line() {
        let key = first_line(&b"synthetic-key\r\nsecond line\n"[..]).unwrap();
        assert_eq!(key, "synthetic-key");
        assert_eq!(first_line(&b""[..]).unwrap(), "");
    }

    #[test]
    fn arguments_are_checked_and_a_key_is_never_one() {
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        assert!(run(&facade, &[], &mut out).is_err());
        assert!(run(&facade, &["set", "curseforge", "the-key"], &mut out).is_err());
        let err = run(&facade, &["status", "modrinth"], &mut out).unwrap_err();
        assert!(err.to_string().contains("only CurseForge"), "{err}");
        assert!(run(&facade, &["status", "other"], &mut out).is_err());
        assert!(run(&facade, &["forget"], &mut out).is_err());
    }
}
