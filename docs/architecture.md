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
| `core` | `vl-core`, `vl-meta` | `core` | Pure: no files, network, processes or clock; spawns no threads. Builds for `wasm32-unknown-unknown`. |
| `native` | `vl-store`, `vl-net`, `vl-tasks`, `vl-settings`, `vl-auth`, `vl-java`, `vl-instance`, `vl-launch` | `core`, `native` | Desktop only: files, network, processes, threads, OS integration. No user interface. |
| `backend` | `vl-backend`, `vl-mock` | `core`, `native` | Implements the facade. `vl-backend` is the only package that composes the native layer. |
| `ui` | `vl-ui-egui` | `core` (dev: also `backend`) | The shared egui interface. Talks to the launcher through the facade only. |
| `app` | `vl-cli`, the desktop app | `core`, `backend`, `ui` | Binaries: choose a backend, own the window or the terminal. Live in `apps/`. |

Rules checked by `cargo xtask ci` (`xtask/src/layers.rs`):

- each package is named `vl-<name>` and declares a known layer;
- each dependency on a workspace package is allowed by the table above (dev-dependencies of a
  `ui` package may also use a `backend` package, so UI tests run on the mock);
- `apps/` holds only `app` packages, and `app` packages live only in `apps/`;
- the `core` layer builds for `wasm32-unknown-unknown`.

Cargo itself rejects dependency cycles, including cycles inside a layer.

```text
app       vl-cli · vibelauncher          → core, backend, ui
ui        vl-ui-egui                     → core
backend   vl-backend · vl-mock           → core, native
native    vl-store · vl-net · vl-tasks · vl-settings · vl-auth · vl-java · vl-instance · vl-launch
                                         → core, native
core      vl-core · vl-meta              → core
```

## Crates

| Package | Layer | Responsibility |
|---|---|---|
| `vl-core` | core | Domain types and the facade contract: ids, errors, platform, instances, versions, accounts, Java runtimes, settings model and override rules, tasks and progress, log lines; `Command`, `Response`, `Event`, the `Facade` trait, replies and the event bus. One module per domain. |
| `vl-meta` | core | Game metadata: parsing and normalizing upstream documents (version manifest, version JSON, rules, arguments, libraries and natives, asset indexes) from bytes into the launcher's own types. Platform rules take a `Platform` argument, so every OS is testable on any host. |
| `vl-store` | native | Data, cache and log directories by OS convention or one portable directory; the on-disk format version and migrations; the lock against a second launcher process; atomic writes. |
| `vl-net` | native | HTTP and downloads: parallelism limit, retries, timeouts, size caps, hash checks, HTTP cache (`ETag`, `Last-Modified`), offline reads, proxies, the launcher's `User-Agent`. |
| `vl-tasks` | native | The task engine: stages with weights, nested subtasks, overall progress, cooperative cancellation, timeouts, a concurrency limit; publishes task events. |
| `vl-settings` | native | Settings files: global settings and per-instance overrides as TOML, range checks, defaults on damage. |
| `vl-auth` | native | Accounts: Microsoft sign-in (device code, Xbox Live, XSTS, Minecraft services), token refresh, offline accounts, secrets in the OS keyring. |
| `vl-java` | native | Java runtimes: discovery on each OS, probing with a timeout, managed downloads, choosing a runtime for a game version. |
| `vl-instance` | native | Instances on disk: format, create, list, rename, delete; damaged instances are listed with a problem. |
| `vl-launch` | native | Preparing game files, building the command line, running and stopping the game process, streaming its log, detecting crashes. |
| `vl-backend` | backend | The real facade: routes each command domain to its module, converts native errors into `vl_core::Error`, publishes events. |
| `vl-mock` | backend | The facade on synthetic in-memory data, with simulated tasks, launches and game logs. No files, network or processes. |
| `vl-ui-egui` | ui | The shared egui interface. |
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
  `Settings(SettingsCommand)`, `Launch(LaunchCommand)`, `Tasks(TaskCommand)`. Each domain's
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
| `Log(LogLine)` | a launcher or game log line with its source and level |
| `Instance(InstanceEvent)` | `Added`, `Changed`, `Removed` |
| `Launch(LaunchEvent)` | `StateChanged { instance, state }`, `Exited { instance, exit }` |
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
`Event::Log` with `LogSource::Game(instance)`; the end of the process is `LaunchEvent::Exited`
with the exit code and whether it was a crash or a user stop. `LaunchCommand::Stop` stops a
preparing or running game.

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
warnings denied, `cargo test`, and a `wasm32-unknown-unknown` build of the `core` layer. CI runs
it on Linux and Windows for every pull request and on macOS weekly.
