//! Command groups; each group lives in its own module.

mod accounts;
mod catalog;
mod info;
mod instances;
mod java;
mod launch;
mod mods;
mod packs;
mod versions;

use std::fmt;
use std::io::{self, Write};

use vl_core::component::ComponentKind;
use vl_core::instance::LoaderRequest;
use vl_core::{Command, Facade, Response};

pub(crate) const USAGE: &str = "usage: vl-cli [--mock] [--dir <path>] <command>
commands:
  info                                which backend answers
  instances list                      list instances
  instances create <name> <version> [--loader <fabric|quilt|neoforge|forge>] [--loader-version <v>]
                                      create an instance (newest stable loader by default)
  instances set-loader <instance> <fabric|quilt|neoforge|forge|none> [--loader-version <v>]
                                      change or remove an instance's loader
  instances mods list <instance> [--kind <k>]
  instances mods enable|disable|remove <instance> <file> [--kind <k>]
  instances mods add <instance> <path> [--kind <k>]
  instances mods identify <instance>  find where hand-added files come from (by hash)
  instances mods updates <instance> [--channel <c>] [--include-pack-files]
                                      list newer versions of mods, packs and shaders
  instances mods update <instance> [<mod>...|--all] [--version <id>] [--channel <c>]
                        [--include-pack-files] [--yes]
                                      update, or switch one mod to a version (older only by id)
                                      manage mods (default) or --kind resourcepacks|shaderpacks|
                                      datapacks|global-datapacks; remove moves to the trash
  versions list [--snapshots] [--old] list game versions
  versions install <version> [--loader <fabric|quilt|neoforge|forge>] [--loader-version <v>]
                                      download a version's files without starting it
  versions loaders <fabric|quilt|neoforge|forge> <version>
                                      list loader versions for a game version
  accounts list                       list accounts
  accounts login                      sign in with Microsoft (device code)
  accounts remove <account>           remove an account (id or name)
  accounts refresh <account>          renew a Microsoft sign-in
  java list                           list Java runtimes
  java install <major>                download a Java runtime (8, 17, 21...)
  launch <instance> [--offline]       launch and follow the game log
  catalog search <query> [--type <mod|modpack|resourcepack|shader|datapack>] [--version <v>]
                         [--loader <l>] [--category <c>] [--sort <relevance|downloads|follows|newest|updated>]
                         [--offset <n>] [--limit <n>]
                                      search Modrinth
  catalog project <slug>              show a project
  catalog versions <slug> [--version <v>] [--loader <l>]
                                      list a project's versions
  catalog lookup <sha1>...            find which versions own files with these hashes
  catalog install <slug|id> --instance <id> [--version <id>] [--channel <release|beta|alpha>] [--yes]
                                      install a project and its required dependencies into an
                                      instance; prints the plan first, --yes skips the question
  packs install <file.mrpack|slug> [--name <n>] [--version <id>] [--group <g>] [--channel <c>]
                                      install a Modrinth modpack as a new instance (name, Minecraft
                                      version and loader come from the pack)";

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
        ["catalog", rest @ ..] => catalog::run(facade, rest, out),
        ["packs", rest @ ..] => packs::run(facade, rest, out),
        _ => Err(USAGE.into()),
    }
}

/// Reads `--loader <name>` and `--loader-version <v>`; both may be absent.
pub(crate) fn loader_flags(flags: &[&str]) -> Result<Option<LoaderRequest>, CliError> {
    let mut kind = None;
    let mut version = None;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(CliError::from(USAGE))?;
        match *flag {
            "--loader" => kind = Some(parse_loader(value)?),
            "--loader-version" => version = Some((*value).to_owned()),
            _ => return Err(USAGE.into()),
        }
    }
    match (kind, version) {
        (Some(kind), version) => Ok(Some(LoaderRequest { kind, version })),
        (None, None) => Ok(None),
        (None, Some(_)) => Err("--loader-version needs --loader".into()),
    }
}

/// A loader name as typed on the command line.
pub(crate) fn parse_loader(name: &str) -> Result<ComponentKind, CliError> {
    match name {
        "fabric" => Ok(ComponentKind::Fabric),
        "quilt" => Ok(ComponentKind::Quilt),
        "neoforge" => Ok(ComponentKind::NeoForge),
        "forge" => Ok(ComponentKind::Forge),
        other => Err(CliError(format!(
            "unknown loader {other:?} (fabric, quilt, neoforge, forge)"
        ))),
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
    fn creates_an_instance_with_a_loader() {
        let out = run_mock(&[
            "instances",
            "create",
            "Modded",
            "26.3",
            "--loader",
            "fabric",
            "--loader-version",
            "0.20.1",
        ])
        .unwrap();
        assert!(out.contains("Fabric(\"0.20.1\")"), "{out}");
        let out = run_mock(&["instances", "create", "Plain", "26.3"]).unwrap();
        assert!(out.contains("Vanilla"), "{out}");
    }

    #[test]
    fn loader_flags_are_checked() {
        assert!(run_mock(&["instances", "create", "X", "26.3", "--loader", "nope"]).is_err());
        let err =
            run_mock(&["instances", "create", "X", "26.3", "--loader-version", "1"]).unwrap_err();
        assert!(err.to_string().contains("--loader"), "{err}");
        assert!(run_mock(&["instances", "create", "X", "26.3", "--loader"]).is_err());
        assert!(run_mock(&["instances", "set-loader", "survival", "none", "x"]).is_err());
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
    fn java_install_checks_its_argument() {
        let err = run_mock(&["java", "install", "latest"]).unwrap_err();
        assert!(err.to_string().contains("number"), "{err}");
        assert!(run_mock(&["java", "install"]).is_err());
    }

    #[test]
    fn reports_usage_and_errors() {
        assert!(run_mock(&["nonsense"]).is_err());
        let err = run_mock(&["launch", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }
}
