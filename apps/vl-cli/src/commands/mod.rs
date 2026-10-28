//! Command groups; each group lives in its own module.

mod accounts;
mod bisect;
mod bisect_auto;
mod catalog;
mod datapacks;
mod diagnose;
mod info;
mod instances;
mod java;
mod launch;
mod mods;
mod packs;
mod storage;
mod verify;
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
  instances verify <instance>         check every game file and download damaged ones again
  instances diagnose <instance>       analyse the newest logs of an instance (no launch)
  instances mods list <instance> [--kind <k>]
  instances mods enable|disable|remove <instance> <file> [--kind <k>]
  instances mods add <instance> <path> [--kind <k>]
                                      manage mods (default) or --kind resourcepacks|shaderpacks|
                                      datapacks|global-datapacks; remove moves to the trash
  instances mods identify <instance>  find where hand-added files come from (by hash)
  instances mods updates <instance> [--channel <c>] [--include-pack-files]
                                      list newer versions of mods, packs and shaders
  instances mods update <instance> [<mod>...|--all] [--version <id>] [--channel <c>]
                        [--include-pack-files] [--yes]
                                      update, or switch one mod to a version (older only by id)
  instances worlds list <instance>    list the worlds in the instance's saves folder
  instances datapacks <instance> <world> list
  instances datapacks <instance> <world> enable|disable|remove <file>
  instances datapacks <instance> <world> add <path.zip>
  instances datapacks <instance> <world> identify
  instances datapacks <instance> <world> updates|update [<pack>...|--all] [--channel <c>] [--yes]
                                      data packs of one world (saves/<world>/datapacks)
  instances bisect start <instance> [--suspect <mod id>]...
                                      find the mod that crashes the game: switches halves of
                                      the enabled mods off and on
  instances bisect good|bad|skip <instance>
                                      report how the game ran with the current mods
  instances bisect status|stop <instance>
                                      show the step; stop restores every mod (and a world
                                      left by an automatic search)
  instances bisect auto <instance> [--world <name>] [--stage menu|world] [--stable-secs <n>]
                        [--repeats <k>] [--timeout-secs <n>] [--after-crash] [--offline]
                                      the same search with the launcher starting the game for
                                      every step: a run is good when the game reaches the main
                                      menu (or enters the world with Quick Play, 1.20+; older
                                      versions: you close the game when it works) and keeps
                                      running --stable-secs (20); bad only when it crashes the
                                      same way; a set is good after --repeats (1) clean runs;
                                      --after-crash takes the newest crash as the problem.
                                      The world is copied and put back after every run;
                                      Ctrl+C cancels and restores everything
  instances bisect report <instance>  the result of the last automatic search
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
  java pick <major> [heap_mib]        show the Java a launch would choose (heap default 4096)
  launch <instance> [--offline]       launch and follow the game log
  catalog search <query> [--type <mod|modpack|resourcepack|shader|datapack>] [--version <v>]
                         [--loader <l>] [--category <c>] [--sort <relevance|downloads|follows|newest|updated>]
                         [--offset <n>] [--limit <n>]
                                      search Modrinth
  catalog project <slug>              show a project
  catalog versions <slug> [--version <v>] [--loader <l>]
                                      list a project's versions
  catalog lookup <sha1>...            find which versions own files with these hashes
  catalog install <slug|id> --instance <id> [--world <name>] [--version <id>]
                  [--channel <release|beta|alpha>] [--yes]
                                      install a project and its required dependencies into an
                                      instance; prints the plan first, --yes skips the question.
                                      Data packs need --world (the world's folder name)
  packs install <file.mrpack|file.zip|slug|id> [--provider <modrinth|curseforge>] [--name <n>]
                [--version <id>] [--group <g>] [--channel <c>]
                                      install a Modrinth or CurseForge modpack as a new instance
                                      (name, Minecraft version and loader come from the pack).
                                      CurseForge needs your API key; files whose authors do not
                                      allow other apps to download them are listed with the page
                                      to download them from by hand
  packs export <instance> --out <file.mrpack> [--name <n>] [--version <v>] [--summary <t>]
               [--include <config,kubejs,resourcepacks,shaderpacks,options|none>]
                                      export the instance as a Modrinth modpack; files the catalog
                                      knows become download links, the rest goes inside the pack
                                      (you answer for their licenses). Disabled mods, saves, logs
                                      and screenshots are never included; default is config,
                                      resourcepacks, shaderpacks
  packs updates <instance> [--channel <release|beta|alpha>]
                                      look for a newer version of the instance's modpack
  packs update <instance> [--version <id>] [--file <pack.mrpack>] [--channel <c>] [--reset] [--yes]
                                      update the modpack; prints what changes first. Files you
                                      deleted, disabled or changed are kept, changed configs get
                                      the new version beside them as <file>.pack-new, replaced
                                      files are backed up under .vibelauncher/backup; --reset puts
                                      every pack file back as the pack has it; --file updates a
                                      pack that was installed from a file
  storage show [--sizes]              the data folder and the folder of every section
                                      (data, instances, game, java, cache, backups, logs)
  storage check <section> <path>      check a folder for a section (errors and warnings)
  storage move <section> <path> [--switch-only] [--yes]
                                      copy a section to a folder and switch to it after a
                                      restart; the old copy stays until a cleanup;
                                      --switch-only uses files you copied yourself; --yes
                                      moves despite warnings
  storage suggest                     suggest a data folder off the system drive, with the drives,
                                      free space and the space a typical set of data takes
  storage reset <section> [--switch-only]
                                      move a section back to its default place
  storage cleanup [--trash]           remove old copies left by moves, prune logs and the
                                      cache; --trash also empties the trash folders";

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
        ["storage", rest @ ..] => storage::run(facade, rest, out),
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
        assert!(
            out.contains("Fabric 0.20.1") && !out.contains("Fabric("),
            "{out}"
        );
        let out = run_mock(&["instances", "create", "Plain", "26.3"]).unwrap();
        assert!(out.contains("none"), "{out}");
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
    fn storage_arguments_are_checked() {
        let err = run_mock(&["storage", "reset", "nope"]).unwrap_err();
        assert!(err.to_string().contains("unknown storage section"), "{err}");
        assert!(run_mock(&["storage", "move", "cache", "x", "--bad"]).is_err());
        assert!(run_mock(&["storage"]).is_err());
    }

    #[test]
    fn reports_usage_and_errors() {
        assert!(run_mock(&["nonsense"]).is_err());
        let err = run_mock(&["launch", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }
}
