# Architecture

VibeLauncher is a Rust workspace. The launcher core has no user interface: every front end — the
desktop app and the headless `vl-cli` — reaches it through one facade of commands and events. The
user interface runs unchanged on a mock implementation of that facade.

## Principles

- **Layers point down.** Pure code, then code that touches the machine, then the code that
  composes it, then front ends. `cargo xtask ci` rejects a dependency that points the wrong way.
- **One entry point.** Front ends depend on the facade contract (`vl-core`) and on a backend that
  implements it; they never call a native crate directly.
- **Never crash.** Non-test code has no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`
  or `unreachable!` (denied workspace-wide); `unsafe` is forbidden. A damaged file, a bad response
  or a missing Java is an error the user can act on.
- **Everything from the network is hostile.** Sizes are capped, hashes are checked, input is
  never indexed without a check.
- **Secrets stay secret.** Tokens and keys live in the OS keyring and never appear in files,
  logs, events or error messages.

## Workspace layout

```text
crates/          libraries, one directory per package
apps/            binaries: vl-cli (and the desktop app)
xtask/           cargo xtask ci | version
docs/            this document
```

**Naming.** Every workspace package is named `vl-<name>` and lives in a directory of the same
name. Packages are internal (`publish = false`) and are referenced by path through
`[workspace.dependencies]`; a name taken on crates.io does not matter. Binaries are `vl-cli` and
`vibelauncher` (the desktop app). The only package outside this rule is `xtask`.

**Layer declaration.** Every package states its layer in its own manifest:

```toml
[package.metadata.vl]
layer = "native"
```

## Layers

| Layer | Packages | May depend on | Guarantees |
|---|---|---|---|
| `core` | `vl-core`, `vl-meta`, `vl-mods`, `vl-crash`, `vl-bisect`, `vl-modrinth` | `core` | Pure: no files, network, processes or clock; spawns no threads. Builds for `wasm32-unknown-unknown`. |
| `native` | `vl-store`, `vl-net`, `vl-tasks`, `vl-settings`, `vl-auth`, `vl-java`, `vl-instance`, `vl-loaders`, `vl-resources`, `vl-platforms`, `vl-install`, `vl-launch` | `core`, `native` | Desktop only: files, network, processes, threads, OS integration. No user interface. |
| `backend` | `vl-backend`, `vl-mock` | `core`, `native` | Implements the facade. `vl-backend` is the only package that composes the native layer. |
| `ui` | `vl-ui-egui` | `core` (dev: also `backend`) | The shared egui interface. Talks to the launcher through the facade only. |
| `app` | `vl-cli`, the desktop app | `core`, `backend`, `ui` | Binaries: choose a backend, own the window or the terminal. Live in `apps/`. |

Rules checked by `cargo xtask ci` (`xtask/src/layers.rs`):

- each package is named `vl-<name>` and declares a known layer;
- each dependency on a workspace package is allowed by the table above (dev-dependencies of a
  `ui` package may also use a `backend` package, so UI tests run on the mock);
- `apps/` holds only `app` packages, and `app` packages live only in `apps/`;
- the `core` layer and `vl-ui-egui` build for `wasm32-unknown-unknown`.

Cargo itself rejects dependency cycles, including cycles inside a layer.

```text
app       vl-cli · vibelauncher          → core, backend, ui
ui        vl-ui-egui                     → core
backend   vl-backend · vl-mock           → core, native
native    vl-store · vl-net · vl-tasks · vl-settings · vl-auth · vl-java · vl-instance · vl-loaders · vl-resources · vl-platforms · vl-install · vl-launch
                                         → core, native
core      vl-core · vl-meta · vl-mods · vl-crash · vl-bisect · vl-modrinth → core
```

## Crates

| Package | Layer | Responsibility |
|---|---|---|
| `vl-core` | core | Domain types and the facade contract: ids, errors, platform, instances, versions, accounts, Java runtimes, settings model and override rules, tasks and progress, log lines; `Command`, `Response`, `Event`, the `Facade` trait, replies and the event bus. One module per domain. |
| `vl-meta` | core | Game metadata: parsing and normalizing upstream documents (version manifest, version JSON, rules, arguments, libraries and natives, asset indexes, loader profiles, installer profiles with their processor plans, the NeoForge and Forge version lists) from bytes into the launcher's own types. Platform rules take a `Platform` argument, so every OS is testable on any host. |
| `vl-mods` | core | Mod metadata: parsing `fabric.mod.json`, `quilt.mod.json`, `META-INF/mods.toml`, `META-INF/neoforge.mods.toml` and `mcmod.info` from bytes into `ModMetadata` (id, name, version, authors, description, icon path, dependencies, loader, game versions). |
| `vl-crash` | core | Crash diagnosis: text of `latest.log`, a crash report or `hs_err` plus a class-to-mod index in, a `Diagnosis` out (confidence level, signature, suspect mods, localization keys, evidence lines). Signatures are data (`signatures.toml`); a mod is named with certainty only when a signature matched. |
| `vl-bisect` | core | Mod bisection engine: a serializable `Session` over the enabled mod files (provided and required mod ids) hands out the next set of files to enable and takes the answer (bad, good, skipped). Delta debugging with dependency closure (a file is never disabled while an enabled one requires it), libraries kept enabled until the other mods are cleared, optional hints from a crash diagnosis, several culprits at once, and a verdict (found, not reproduced, inconclusive). About `log2(n)` steps for one culprit. |
| `vl-store` | native | Data, cache and log directories by OS convention or one portable directory; the on-disk format version and migrations; the lock against a second launcher process; atomic writes. |
| `vl-net` | native | HTTP and downloads: parallelism limit, retries, timeouts, size caps, hash checks, HTTP cache (`ETag`, `Last-Modified`), offline reads, proxies, the launcher's `User-Agent`. |
| `vl-tasks` | native | The task engine: stages with weights, nested subtasks, overall progress, cooperative cancellation, timeouts, a concurrency limit; publishes task events. |
| `vl-settings` | native | Settings files: global settings and per-instance overrides as TOML, range checks, defaults on damage. |
| `vl-auth` | native | Accounts: Microsoft sign-in (device code, Xbox Live, XSTS, Minecraft services), token refresh, offline accounts, secrets in the OS keyring. |
| `vl-java` | native | Java runtimes: discovery on each OS, probing with a timeout, managed downloads, choosing a runtime for a game version. |
| `vl-instance` | native | Instances on disk: format, create, list, rename, delete; damaged instances are listed with a problem. |
| `vl-loaders` | native | Mod loader sources: version lists per game version and launch profiles of Fabric, Quilt, NeoForge and Forge through `vl-net`, the on-disk store of the profiles in use, and the installation of NeoForge and Forge (installer, its libraries, processors run with Java behind the `ProcessorRunner` trait). |
| `vl-resources` | native | Instance resource folders (`mods`, `resourcepacks`, `shaderpacks`, `datapacks`, `global_packs/required_data`): listing with mod metadata read from jars (capped entries and sizes, bundled jars listed not opened, damaged jars reported as a problem), enable and disable by renaming to `<name>.disabled`, add by copy without replacing, remove into `<instance>/.trash/`, and the `ClassIndex` of `vl-crash` built from the packages of the enabled jars. `Bisect` runs a `vl-bisect` session on the mods folder: the session file `<instance>/.vibelauncher/bisect.json` is saved before any file is renamed, and ending the session in any way (finish, stop, a failed step) re-enables every file it started with. The game is run by the user between steps. |
| `vl-launch` | native | Preparing game files, building the command line, running and stopping the game process, streaming its log, detecting crashes. |
| `vl-backend` | backend | The real facade: routes each command domain to its module, converts native errors into `vl_core::Error`, publishes events. |
| `vl-mock` | backend | The facade on synthetic in-memory data, with simulated tasks, launches and game logs. No files, network or processes. |
| `vl-ui-egui` | ui | The shared egui interface. |
| `vl-modrinth` | core | Modrinth API v2 as pure functions: request URLs with search facets, filters and hash lookups, and tolerant parsing of responses into the catalog types of `vl-core` (size limits, unknown fields ignored, damaged list entries skipped, unsafe file names and non-`https` download URLs dropped). The `mrpack` module parses `modrinth.index.json` (`https` downloads from `cdn.modrinth.com`, `github.com`, `raw.githubusercontent.com` and `gitlab.com` only, safe relative paths, capped sizes and counts). |
| `vl-platforms` | native | Online mod platforms behind the `CatalogSource` trait; `ModrinthSource` fetches through `vl-net` (HTTP cache, retries, size caps) and paces itself with a rate limiter that follows the server's `X-Ratelimit-*` headers. A second platform adds a module and a `Provider` variant. |
| `vl-install` | native | Installing a catalog project into an instance: picking the version for the instance's game version and loader, following required dependencies, recognizing what is installed, the plan, verified download, all-or-nothing add to the resource folders, and the origin records (`<instance>/.vibelauncher/resources.json`). The `mrpack` module installs a Modrinth modpack (`.mrpack`, from a file or a catalog project) into a new game directory: archive checks (entry names, symlinks, entry counts), verified downloads of the client files, `overrides/` then `client-overrides/`, and the pack record `<instance>/.vibelauncher/pack.json`. The `mrpack_update` module updates an installed pack while keeping the user's changes. |
| `vl-cli` | app | The headless command line. |

Third-party crates are declared once in `[workspace.dependencies]` and used with
`workspace = true`.

## The facade

The facade is defined in `vl-core` (`facade` module) and is the only interface between front
ends and the launcher.

```rust
pub trait Facade: Send + Sync {
    fn call(&self, command: Command) -> Reply;   // never blocks
    fn subscribe(&self) -> EventStream;          // events published from now on
}
```

### Commands and responses

- A `Command` is grouped by domain: `Hello`, `Instances(InstanceCommand)`,
  `Versions(VersionCommand)`, `Accounts(AccountCommand)`, `Java(JavaCommand)`,
  `Settings(SettingsCommand)`, `Launch(LaunchCommand)`, `Tasks(TaskCommand)`,
  `Catalog(CatalogCommand)`. Each domain's
  command enum lives in that domain's module of `vl-core` and documents the `Response` variant it
  answers.
- `call` returns at once with a `Reply` that receives exactly one `Result<Response, Error>`. A UI
  polls it (`try_take`) once per frame; `vl-cli` blocks on it (`wait`).
- A command that takes long answers `Response::Task(id)` immediately; the rest of its work is
  reported as task events.
- Commands, responses and events are plain serializable data. Command, event and error enums are
  `#[non_exhaustive]`: adding a variant does not break other crates.

### Events

`subscribe` opens an `EventStream` of `Event`s:

| Event | Carries |
|---|---|
| `Task(TaskEvent)` | `Started(TaskInfo)`, `Progress { id, progress }`, `Finished { id, outcome }` |
| `Log(LogLine)` | a launcher log line with its source and level |
| `LogBatch(LogBatch)` | a group of game output lines (level, time, text) with a count of lines left out |
| `Instance(InstanceEvent)` | `Added`, `Changed`, `Removed` |
| `Launch(LaunchEvent)` | `StateChanged { instance, state }`, `Exited { instance, exit }`, `Diagnosed { instance, diagnosis }` |
| `Account(AccountEvent)` | `DeviceCode(prompt)` for sign-in, `ListChanged` |
| `Version`, `Java`, `Settings` | list or value changed; read it again with a command |
| `Lagged { missed }` | this stream fell behind and dropped `missed` events |

Each stream has a bounded queue (`EventBus`). When a front end does not keep up, its events are
dropped and counted, and the stream reports `Lagged` once its queue drains; the front end then
re-reads state with commands. A slow front end never slows the launcher or grows its memory. A
front end subscribes before sending a command whose events it follows.

### Tasks

- Every long operation (refreshing versions, installing files, signing in, scanning Java,
  launching) is a task with a `TaskId`, a `TaskKind`, an optional subject (an instance name, a
  version id) and an optional parent task.
- Progress is `done` of `total` in a unit (bytes, items, steps) with an optional stage;
  `Progress::fraction` gives `0.0..=1.0`.
- `TaskCommand::Cancel { id }` asks a cancellable task and its subtasks to stop. Cancellation is
  cooperative; the task ends with exactly one `Finished` whose outcome is `Succeeded`,
  `Failed(Error)` or `Cancelled`.
- `TaskCommand::List` returns running tasks with their latest progress, so a front end that
  starts late or lagged can catch up.

### Launch flow

`LaunchCommand::Launch` answers `Response::Task` for the preparation task. The instance moves
`Preparing → Running → Stopped` (`LaunchEvent::StateChanged`); game output arrives as
`Event::LogBatch` with `LogSource::Game(instance)`, a few batches a second; the end of the process
is `LaunchEvent::Exited` with the exit code, the session time, whether it was a crash or a user
stop, the crash report and `hs_err_pid*.log` written during the run, the log file and the last
lines. `LaunchCommand::Stop` stops a
preparing or running game.

`vl-launch` does the work behind it:

- `Installer::install` makes a version ready below the shared game files (`libraries/`, `assets/`,
  `versions/<id>/`): version JSON and the version manifest (HTTP cache; an installed version needs
  no network), client jar, libraries, logging configuration, asset objects in one verified,
  cancellable download batch with byte progress, natives unpacked with `extract.exclude`, and the
  files of the `virtual` and `map_to_resources` asset layouts restored. `VersionCommand::Install`
  runs it as a task without starting the game.
- `Installer::install_with` takes an optional `LoaderProfile` for the game version: the profile
  stands in for the root version JSON, the vanilla version it inherits from is installed as
  usual, and the merged result (loader main class, loader libraries first on the class path,
  loader JVM arguments) is installed and launched like any version. `Installed::id` is the
  profile id (`${version_name}`). `VersionCommand::Install` accepts a loader too.
- `prepare` adds the Java choice (the version's major version; a game started through
  LaunchWrapper is kept on Java 8), the memory check (a warning when `-Xmx` exceeds the free
  memory, a 1 GiB limit on 32-bit Java), and the command line: memory, user JVM arguments, the
  version's JVM arguments, logging, main class, game arguments with `${variables}` substituted.
- The player's session (`LaunchSession`: name, UUID, access token, user type) is a parameter of
  `prepare`. The backend takes it from the account and refreshes an expiring token first.
- `process::spawn` starts Java without a shell in the instance's game directory and reports the
  exit; a stop kills the whole process tree (`taskkill /T` on Windows, a process group on Unix).
  Reader threads only hand lines over, so the game never waits for a front end. A collector parses
  them (log4j XML events and plain lines into levels; the access token is masked), writes the full
  output to `<instance>/logs/vl-game-<UTC time>.log` (the newest 10 are kept), keeps the last
  `console.max_lines` lines in memory (at most 16 MiB) and delivers batches every 100 ms of at
  most 500 lines; the surplus is counted as skipped. A run is a crash when the code is not 0 or a
  new file appeared in `crash-reports/` or as `hs_err_pid*.log` in the game directory.
- Crash diagnosis (`vl-backend`): after a run that crashed (not stopped by the user) the backend
  reads the tail (512 KiB) of the run's output file, else of `logs/latest.log`, and the head
  (256 KiB) of the run's crash report and JVM error log, builds a `CrashInput`, and runs
  `vl_crash::diagnose` with the `ModClassIndex` of the instance's enabled mods on its own thread.
  The result is `LaunchEvent::Diagnosed` (a `vl_core::crash::CrashDiagnosis`, after `Exited`) and a
  warning line in the launcher log. `LaunchCommand::Diagnose { instance }` does the same for the
  newest logs without launching (`Response::Diagnosis`; `vl-cli instances diagnose <instance>`).
  A diagnosis carries `cause` (`ModLikely`, `Environment`: Java, memory, graphics driver, damaged
  game files, `Unknown`) and `loader_reported` (the mod loader named the problem itself:
  Fabric/Quilt "Incompatible mods found!", Forge/NeoForge mod loading errors, missing or wrong
  dependency versions); `bisect_useful()` is true only for `ModLikely` without `loader_reported`.
  Message and action keys are Fluent ids in the interface locale (`crash-*`, `crash-action-*`).
- Apple silicon: LWJGL 3 versions without arm64 natives get LWJGL 3.3.3 from Maven Central;
  LWJGL 2 versions (up to 1.12.2) are installed for `x86_64` and need an `x86_64` Java (Rosetta).


### Threading

The facade needs no async runtime: replies and event streams are channels. A backend runs its
work on its own threads or runtime; `vl-core` stays free of both, which keeps it buildable for
the browser.

## Errors

- The facade reports `vl_core::Error { kind, message }`.
- `ErrorKind` is what front ends act on and what the UI localizes: `InvalidInput`, `NotFound`,
  `Conflict`, `Network`, `Io`, `Integrity`, `Auth`, `Java`, `Game`, `Cancelled`, `Unsupported`,
  `Internal`.
- `message` is English technical detail for logs and "details" views. It never contains secrets.
- Native crates keep their own detailed error types; `vl-backend` converts them into
  `vl_core::Error` at the facade boundary. A failed task carries its error in
  `TaskOutcome::Failed`.
- A command a backend does not implement answers `ErrorKind::Unsupported`.

## Data, settings and secrets

`vl-store` owns the locations; every other native crate receives its directories from it.

| Location | Windows | macOS | Linux |
|---|---|---|---|
| Data | `%APPDATA%\VibeLauncher` | `~/Library/Application Support/VibeLauncher` | `$XDG_DATA_HOME/vibelauncher` |
| Cache | `%LOCALAPPDATA%\VibeLauncher\cache` | `~/Library/Caches/VibeLauncher` | `$XDG_CACHE_HOME/vibelauncher` |
| Logs | `<data>/logs` | `~/Library/Logs/VibeLauncher` | `$XDG_STATE_HOME/vibelauncher/logs` |

In portable mode all three live under that one directory (`<dir>`, `<dir>/cache`, `<dir>/logs`)
and nothing is written outside it. `--dir <path>` selects it; otherwise a `vibelauncher.portable`
file next to the executable selects `<executable directory>/data`. `--dir` wins over the marker.

`vl-store` also provides, for every native crate:

- `write_atomic` / `write_atomic_with` — write a temporary file next to the target, flush, rename;
  readers see the old or the new file, never a partial one.
- `Store::open` — creates the directories, takes an exclusive lock on the data directory
  (`.lock`, released by the OS on exit or crash, so a second launcher is refused with a conflict
  error) and migrates the data to the current format.
- The format version (`format.json` in the data directory) and the migration chain
  (`format::MIGRATIONS`, one step per version). A directory with content and no version file is
  format 0. Before the first step the data is copied to `snapshots/format-<n>`; the version is
  recorded after each step. Data from a newer format is refused and left untouched.

The data directory holds:

- the format version, checked at start; older formats migrate forward after a snapshot;
- the global settings file and each instance's overrides (TOML, written atomically; a damaged
  file falls back to defaults with a warning);
- `instances/` — one directory per instance, owned by `vl-instance`: `instance.json` (name, icon,
  group, notes, creation, last active and last played times, play time, components: the game
  version, with room for loaders and patches) and `minecraft/` (the game directory). The instance
  id is the directory name. Configs are written atomically; creation is staged in a hidden
  directory and renamed into place; rename moves the directory and rolls back if the config
  cannot be written. A directory with a missing, damaged or newer config is listed with a
  problem and does not affect the others. Delete moves the instance into `instances/.trash/`
  (recoverable, nothing is erased); only an explicit purge empties it;
- loader profiles in the metadata cache: `vl_core::component` models an instance as the game plus
  at most one loader (`ComponentSet`); `vl_meta::loader` normalizes every loader source into one
  `LoaderProfile` (main class, libraries with hashes, arguments, the game it inherits from),
  merges it with the vanilla version JSON, and wraps it for the cache (`CachedProfile`, with the
  `ETag`; a stale entry is used when upstream is unreachable). Loader sources only add parsers;
- the online catalog (`vl_core::catalog`, `vl-modrinth`, `vl-platforms`): `CatalogCommand` searches
  projects (type, game version, loader and category facets, sort order, paging), reads a project
  by id or slug, lists its versions (loader and game version filters) with files, hashes and
  dependencies, and finds the versions that own given file hashes (SHA-1 or SHA-512, batched
  500 per request), which tells where an installed file came from. Modrinth is read without a
  key at `api.modrinth.com/v2` with the launcher's `User-Agent`; requests stay below the 300 a
  minute per address limit (a window budget plus the `X-Ratelimit-*` headers; a `429` waits for
  the reset), a `410` means the API version was retired and is reported as `Unsupported`. Answers
  go through the HTTP cache: young ones are reused, old ones serve offline. Catalog data is shown
  as the platform's data, with its name and links, and is not used to train models;
- installing from the catalog (`vl-install`): `CatalogCommand::PlanInstall` returns an
  `InstallPlan` and `CatalogCommand::Install` runs it as an `InstallResource` task. The version
  is the newest one that lists the instance's game version and loader (Quilt also takes Fabric
  versions, its own first), preferring `release` over `beta` over `alpha` down to the least
  stable channel the request allows (default `beta`); the platform's filtering is checked again
  on the response. Required dependencies are followed recursively (depth and count capped, cycles
  cut) and placed before the project that needs them. A file counts as installed when its SHA-1
  matches an origin record, is known to the platform (`version_files`), or, for a required
  dependency only, when a jar declares a mod id equal to the project slug. Such a dependency is
  left alone whatever its version; the requested project is replaced when another version is
  installed (the old file moves to the instance trash, a disabled file stays disabled). An
  `incompatible` dependency of any chosen version, or of an installed file whose version is
  known, is a `Conflict` that names both sides; optional dependencies are listed as
  recommendations and never installed. Files are downloaded to `<instance>/.vibelauncher/staging`
  with size and SHA-512 (SHA-1 when that is all the platform gives) checks, then added one by one;
  a failure puts the folders back as they were. `resources.json` records, per installed file,
  platform, project, version, hashes and whether it came as a dependency; a record counts only
  while the file's SHA-1 matches, a damaged file or entry is skipped and the damaged file is kept
  as `resources.json.bad`;
- updates and origin discovery (`vl-install::updates`): `CatalogCommand::IdentifyMods` looks
  files without an origin record up by SHA-1 (`version_files`) and writes their records.
  `CheckUpdates` hashes every file of mods, resource packs, shader packs and data packs, asks
  `version_files` once for all of them, then `POST /version_files/update` once for the mods
  (loaders of the instance, its game version, `version_types` from the channel setting, release
  only by default) and once for the other kinds (the loaders their installed versions have). The
  answer is checked again: it must be the same project, fit the game version and loader, be of an
  allowed channel and be published later than the installed version, so a check never proposes a
  downgrade. The report lists `updates` (current to new), `from_pack` (files recorded in
  `pack.json`, skipped unless `include_pack_files`; the pack is updated as a whole), and
  `unchecked` (no origin and unknown to the platform, an origin the platform no longer knows, or
  a mod in an instance without a loader). `PlanUpdate` and `Update` (an `InstallResource` task)
  pin the chosen versions in the same planner as an install, reusing the check's scan and
  versions, so new required dependencies are resolved the same way; the swap is the install
  execution (all or nothing, disabled files stay disabled, the dependency flag of a record is
  kept). `UpdateRequest::version` switches exactly one file to a given version id, older ones
  included; a downgrade happens only that way;
- modpack updates (`vl-install::mrpack_update`): `CheckPackUpdate` lists the pack project's
  versions and reports the first one newer than the installed version in an allowed channel
  (releases only by default); a pack installed from a file has no source and is updated with
  `PackUpdateRequest::file`. `PlanPackUpdate` downloads the new pack and compares three states:
  the baseline in `pack.json` (files with hashes, and since updates exist the SHA-1 of every
  override file), the new index and overrides, and the folder as it is. A file the pack changes
  and the folder still has as delivered is replaced (a disabled file is replaced in place and
  stays disabled); a file the pack drops and the user did not touch is removed; a new file
  that replaces a mod of the same project (matched by origin records, else by file name without
  version) inherits the user's choice for the old one: disabled stays disabled, deleted or
  swapped for another version is not brought back. A file the user changed is left alone. An
  override the pack changed that the user also changed is kept and the pack's version is saved as
  `<file>.pack-new`; a record without override hashes treats every differing config that way.
  Files the pack never had are never touched. `reset` makes the folder match the pack instead.
  `UpdatePack` (an `UpdatePack` task) downloads and extracts everything first, copies each file
  it is about to change or remove into `.vibelauncher/backup/<UTC time>/` (the newest five
  backups are kept), applies the changes, rewrites `pack.json` and `resources.json`, then changes
  the instance's game version and loader as the last step; a failure or a cancel before the end
  restores every file and both records;
- modpack export (`vl-install::mrpack_export`): `ExportPack` writes an instance as an `.mrpack`
  (answered from a worker thread; the target file must not exist). The pack's dependencies are
  the instance's game version and loader. The enabled mods are always carried; `include`
  adds `config` (`config/`, `defaultconfigs/`), `kubejs`, `resourcepacks`, `shaderpacks` and
  `options` (`options.txt`), by default config, resource packs and shaders. Disabled files
  (`*.disabled`) are left out, and `saves/`, `logs/`, `crash-reports/`, `screenshots/`,
  `.vibelauncher/` and caches are never walked. Mods, resource packs and shaders are hashed and
  looked up in the catalog (SHA-512, then SHA-1): a file whose version lists a matching file with
  an allowed download link becomes an index entry (path, SHA-1, SHA-512, size, link; `env` is
  client required and server optional for mods, server unsupported otherwise); every other file
  goes into `overrides/`, and the report lists the unknown mods, resource packs and shaders and
  warns that the right to distribute them is the author's. A catalog that cannot be reached fails
  the export. The archive is deterministic: the index first, then `overrides/` sorted by path,
  every entry deflated with the same date and mode;
- mod loaders (`vl-loaders`): Fabric (`meta.fabricmc.net/v2`) and Quilt (`meta.quiltmc.org/v3`)
  share one API shape, `versions/loader/<game>` (versions for a game version, empty or an error
  status when the game is not supported) and `versions/loader/<game>/<loader>/profile/json` (a
  ready launch profile). The version list goes through the HTTP cache. A profile is normalized
  and stored at `loaders/loader/<uid>/<game>/<loader>.json` in the data directory: a young entry
  is used as is, an old one is renewed, and an unreachable source falls back to the stored one,
  so an instance launches offline once it was launched or installed. NeoForge
  (`maven.neoforged.net/releases`, `net.neoforged:neoforge`) has no meta server: the version list
  is the artifact's `maven-metadata.xml`, mapped to game versions by the version scheme
  (`21.1.x` is 1.21.1, `21.0.x` is 1.21, `26.1.2.x` is 26.1.2, `26.1.0.x` is 26.1; `-beta` builds
  are unstable, snapshot builds are left out). Minecraft 1.20.1 is the exception: its builds are
  published as `net.neoforged:forge` (`1.20.1-47.1.x`) and listed, merged with the list above, by
  the short version (`47.1.106`, the full form is accepted as input) with installer
  `forge-1.20.1-<v>-installer.jar` and the same installer format. A version is installed from
  `neoforge-<v>-installer.jar` (checked against the repository's `.sha1`) kept with its
  `install_profile.json`, `version.json` and data files in `loaders/neoforge/<v>/`; the profile is
  the normalized `version.json` (its `-DignoreList` names the vanilla jar by the game's id). After
  the game files are installed, `Loaders::install` downloads the installer's libraries and runs
  its client processors (`java -cp <jar and class path> <Main-Class> <args>`, `{NAME}` and
  `[coordinate]` references resolved by `vl_meta::install_profile`) with the Java the game uses,
  as part of the launch or install task: progress per processor, cancellation kills the process,
  the output goes to `processors.log`, declared outputs are checked against their SHA-1. A record
  (`installed.json`) holds the SHA-1 of the vanilla jar and of every file the data values name;
  while they match, the processors do not run again. Forge for Minecraft 1.13 and newer
  (`maven.minecraftforge.net`, `net.minecraftforge:forge`) works the same way: Maven versions are
  `<game>-<forge>` (`1.20.1-47.3.0`), the launcher names a version by its Forge part, and
  `promotions_slim.json` marks the recommended build. Forge versions are not divided into stable
  and unstable ones: each loader version carries `recommended` and `latest` flags (latest is the
  newest version of the game version), and the default choice is the recommended build, else the
  latest; `versions loaders <loader> <game>` in the CLI prints the marks. Its installers use install profile spec 0 (1.13–1.16, `cpw.mods.modlauncher.Launcher`)
  or spec 1 (1.17+, the module-path bootstrap) and are kept in `loaders/forge/<game>-<forge>/`.
  Launch-profile libraries without a download address are taken from the installer's `maven/`
  directory or are made by the processors, so the installation step runs after the vanilla
  files and before the loader's libraries are installed. Forge for older game versions is
  `Unsupported`. An instance created with a
  loader without a version gets the default one (the recommended build, else the newest stable) (`InstanceCommand::Create` with
  `LoaderRequest`); `InstanceCommand::SetLoader` replaces or removes the loader of a stopped
  instance and the next launch rebuilds the class path from the new profile;
- shared game files (libraries, assets, version files) used by all instances;
- managed Java runtimes;
- the normalized metadata cache: upstream metadata (Mojang, loader metadata services) is fetched
  at run time, normalized by `vl-meta` and cached here, so installed versions launch offline and
  one upstream outage does not break the others;
- the account list with public profile data only.

The cache directory holds the HTTP cache and other files that can be deleted at any time.

Secrets — Microsoft refresh and access tokens, and API keys the user enters (such as a personal
CurseForge API key) — live in the OS keyring, never in the data directory, logs, events or error
messages. The launcher ships with no third-party API keys.

The keyring is the Windows Credential Manager, the macOS Keychain, or the Secret Service over
D-Bus on Linux (a pure-Rust client: no GTK, libsecret or libdbus). Each account has one entry under
the service name `VibeLauncher`. Without a usable keyring, tokens are kept in memory only until the
launcher closes and the user is told so; nothing secret is written to disk in the clear.

The Microsoft application (client) ID is a build constant (`vl_auth::DEFAULT_CLIENT_ID`); the `VL_MS_CLIENT_ID` environment variable replaces it, so forks and development builds use their own registration.

The settings model (`vl_core::settings`) has eight groups: Java, memory, window, console,
commands (pre-launch, wrapper, post-exit), environment variables, performance (GameMode,
MangoHud, discrete GPU, Zink) and natives (system GLFW, OpenAL, SDL). The effective value of a
group is the instance's override if it has one, else the global group, which starts from the
built-in defaults. An instance overrides a whole group or inherits it (`Option` per group in
`Overrides`); `Settings::with_overrides` computes the effective settings. Every group has range
checks (`validate`) and every type keeps unknown fields (`Extra`).

On disk (`vl-settings`, TOML, written with `vl_store::write_atomic`):

- `settings/global.toml` holds `schema_version` and one table per group;
- `settings/instances/<id>.toml` holds the same for an instance; a table present means the group is
  overridden, and a file with no tables does not exist;
- missing keys take their defaults, unknown keys and groups survive a load and save, and a file from
  a newer schema keeps its version number;
- a file that cannot be parsed gives defaults, a malformed or out-of-range group is reset to its
  default (in an instance file: dropped, so the instance inherits), each with a
  `SettingsEvent::Warning`; the original is copied to `<file>.bad` first.

## Front ends

**Desktop app.** `vl-ui-egui` holds a `Box<dyn Facade>`. Each frame it polls outstanding replies,
drains its event stream into view state and draws; user actions become commands. The app binary
chooses the backend: `vl-backend` normally, `vl-mock` for interface work.

The `vibelauncher` app (`apps/vibelauncher`, package `vl-app`) is an eframe window with the wgpu renderer around
`vl_ui_egui::Launcher`; it runs on `vl-mock` for now (`--light` starts the light theme, `--section
<name>` opens a section). `vl-ui-egui` depends on `egui` only, not on a windowing crate. Its
layout: a left rail of five sections (Home, Instances, Catalog, Account, Settings), a top bar, a
tasks panel (Ctrl+J) and the page of the current section; colors, spacing and type scale come from
the design tokens in `theme.rs`.

**`vl-cli`.** Parses arguments, starts a backend (`--mock` selects `vl-mock`, `--dir` selects a
portable data directory), sends commands and blocks on replies. Long commands (`launch`) subscribe
first, then print task progress and game log lines until the game exits. Each command group lives
in its own module under `apps/vl-cli/src/commands/`.

Both front ends speak only `vl-core` types; whatever one front end can do, the other can do
through the same commands.

## The mock

`vl-mock` implements every facade domain on synthetic data: several instances (including a damaged
one), game versions of each kind, an offline account, Java runtimes and default settings. Long
operations run on background threads: a launch reports preparation progress by stage, moves the
instance to `Running`, prints synthetic game log lines and exits (or runs until stopped); a
Microsoft sign-in shows a device code and adds a synthetic account; tasks can be cancelled.
`MockOptions` sets the pacing; `MockOptions::instant()` makes tests run without delays.

## Extension points

- **New domain.** A domain is a module in `vl-core` (types, command and event enums), a variant
  in `Command` and `Event`, a native crate that does the work, and a module in `vl-backend` that
  routes to it. The mock and the front ends opt in when they are ready.
- **Local services.** An integration with a separate local program — such as the Vibefork agent —
  is a facade domain served by a native crate that speaks that program's protocol. The program
  runs as its own process and is never linked into the launcher.

## Checks

`cargo xtask ci` runs, in order: the layer check, `cargo fmt --check`, `cargo clippy` with
warnings denied, `cargo test`, and a `wasm32-unknown-unknown` build of the `core` layer and
`vl-ui-egui`. CI runs it on Linux and Windows for every pull request and on macOS weekly.
