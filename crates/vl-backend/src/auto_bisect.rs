//! Automatic bisection: the launcher starts the game for every step and judges the run itself.
//!
//! The search runs over a [`vl_resources::Bisect`] session and a [`GameStarter`] (the backend's
//! launch in the launcher, a fake in tests):
//!
//! 1. **The problem.** Either the newest logs right after a crash (`after_crash`: that crash
//!    counts as the check that it reproduces) or a run with every mod. A loader that named the
//!    problem itself, or a diagnosis that points at the setup, ends the search before it starts.
//! 2. **Without mods.** One run with every searched mod disabled, up to the main menu: a crash
//!    there means the mods are not the cause.
//! 3. **The search.** Each step of the session is run; a crash counts as bad only when its crash
//!    key (signature and root exception) matches the problem's, another crash or a hang says
//!    nothing (skipped). A set is bad from its first crash and good after `repeats` clean runs.
//!    Mod ids named by the diagnosis are tried first.
//! 4. **The check.** Every mod but the culprits (and the mods that need them) must run clean.
//!
//! A run is good when the game reaches the target and keeps running for the stable time; the
//! launcher then stops it. The main menu is recognized by lines of the game output (data in
//! `vl-bisect`); a world is entered with Quick Play (1.20 and later) and recognized by the Quick
//! Play log. On older versions the world target is semi-manual: the player enters the world and
//! closes the game once it works, the launcher only watches for the crash.
//!
//! The world is copied to `<instance>/.vibelauncher/bisect-world/` before the first run and put
//! back after every run. Every way the search ends (verdict, error, cancel) restores the mods and
//! the world; `BisectCommand::Stop` restores a world left behind by a launcher that was closed
//! mid-search.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vl_bisect::auto::{CrashKey, Trial, quick_play_args, reached_menu, supports_quick_play};
use vl_core::bisect::{
    AutoBisectReport, AutoVerdict, BisectOutcome, BisectRun, BisectState, BisectTarget,
    BisectVerdict, RunPurpose, RunResult,
};
use vl_core::crash::{CrashCause, CrashDiagnosis};
use vl_core::launch::GameExit;
use vl_core::{Error, ErrorKind};
use vl_resources::{Bisect, Resources};

use crate::crash::{Sources, diagnose_with_text};

/// Folder of the world copy inside the instance's metadata folder.
pub const WORLD_BACKUP_DIR: &str = "bisect-world";
/// The Quick Play log of the runs, inside the instance's metadata folder.
const QUICK_PLAY_LOG: &str = "bisect-quickplay.json";
/// How often a run is checked.
const POLL: Duration = Duration::from_millis(200);
/// How long a stopped game may take to exit.
const STOP_GRACE: Duration = Duration::from_secs(60);

/// What a running game reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameSignal {
    /// The process is running (the launch preparation is over).
    Started,
    /// Lines of game output.
    Lines(Vec<String>),
    /// The process ended.
    Exited(GameExit),
    /// The game could not be started.
    Failed(Error),
}

/// One game started by a [`GameStarter`].
pub trait GameProcess: Send {
    /// The next signal, waiting up to `timeout`.
    fn next(&mut self, timeout: Duration) -> Option<GameSignal>;
    /// Stops the game (or its launch); the exit still arrives as a signal.
    fn stop(&mut self);
}

/// Starts the game of the instance with its current mods.
pub trait GameStarter: Send {
    /// Starts it; `extra_args` go after the game arguments of the version.
    fn start(&mut self, extra_args: &[String]) -> vl_core::Result<Box<dyn GameProcess>>;
}

/// Where the instance is and what the search looks for.
#[derive(Debug, Clone)]
pub struct Setup {
    /// `<instance>/minecraft`.
    pub game_dir: PathBuf,
    /// `<instance>`.
    pub instance_dir: PathBuf,
    /// The game version (decides whether Quick Play exists).
    pub game_version: String,
    /// How far a run must get.
    pub target: BisectTarget,
    /// The world for [`BisectTarget::World`].
    pub world: Option<String>,
    /// How long the game must keep running after reaching the target.
    pub stable: Duration,
    /// Clean runs in a row for a good set.
    pub repeats: u32,
    /// How long a run may take to reach the target once the process runs.
    pub timeout: Duration,
    /// Search the problem of the newest logs instead of running every mod first.
    pub after_crash: bool,
}

impl Setup {
    fn meta_dir(&self) -> PathBuf {
        self.instance_dir.join(".vibelauncher")
    }

    fn log_dir(&self) -> PathBuf {
        self.instance_dir.join("logs")
    }

    fn resources(&self) -> Resources {
        Resources::new(self.game_dir.clone(), self.instance_dir.join(".trash"))
    }
}

/// What the search reports while it runs.
pub trait Hooks {
    /// Whether the search should stop.
    fn cancelled(&self) -> bool;
    /// Progress: what is being done, steps done and about how many in all.
    fn progress(&mut self, stage: &str, done: u32, total: u32);
    /// A run ended.
    fn run(&mut self, run: &BisectRun, steps_done: u32, steps_estimate: u32);
}

/// Runs the whole search. The mods and the world are restored however it ends.
pub fn run(
    setup: &Setup,
    starter: &mut dyn GameStarter,
    hooks: &mut dyn Hooks,
) -> vl_core::Result<AutoBisectReport> {
    let world = match (setup.target, &setup.world) {
        (BisectTarget::World, None) => {
            return Err(Error::invalid_input(
                "a search into a world needs the world",
            ));
        }
        (_, Some(name)) => Some(WorldCopy::new(setup, name)?),
        (_, None) => None,
    };
    let quick_play = supports_quick_play(&setup.game_version);
    let watch = match setup.target {
        BisectTarget::World if quick_play => Watch::QuickPlay,
        BisectTarget::World => Watch::Manual,
        _ => Watch::Menu,
    };
    let mut driver = Driver {
        setup,
        bisect: Bisect::new(setup.resources(), &setup.meta_dir()),
        starter,
        hooks,
        world,
        watch,
        reference: None,
        runs: Vec::new(),
        known: BTreeMap::new(),
        session: false,
    };
    if let Some(world) = &driver.world {
        world.save()?;
    }
    let searched = driver.search();
    let restored = driver.restore_all();
    let verdict = searched?;
    restored?;
    Ok(AutoBisectReport {
        verdict,
        reference: driver.reference.map(|(d, _)| d),
        semi_manual: watch == Watch::Manual,
        runs: driver.runs,
    })
}

/// Puts back a world copy left by a search that did not end (the launcher was closed).
pub fn restore_leftover_world(game_dir: &Path, instance_dir: &Path) -> vl_core::Result<()> {
    let backups = instance_dir.join(".vibelauncher").join(WORLD_BACKUP_DIR);
    let Ok(entries) = fs::read_dir(&backups) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let target = game_dir.join("saves").join(&name);
        replace_dir(&entry.path(), &target)?;
    }
    remove_dir(&backups)
}

/// How a run decides that the target is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watch {
    /// A main menu line in the output.
    Menu,
    /// The Quick Play log appears.
    QuickPlay,
    /// The player closes the game without a crash.
    Manual,
}

/// What one run showed.
struct Seen {
    result: RunResult,
    diagnosis: Option<(CrashDiagnosis, CrashKey)>,
}

struct Driver<'a> {
    setup: &'a Setup,
    bisect: Bisect,
    starter: &'a mut dyn GameStarter,
    hooks: &'a mut dyn Hooks,
    world: Option<WorldCopy>,
    watch: Watch,
    /// The problem searched for.
    reference: Option<(CrashDiagnosis, CrashKey)>,
    runs: Vec<BisectRun>,
    /// Outcomes known without a run, by the set of enabled files.
    known: BTreeMap<BTreeSet<String>, BisectOutcome>,
    /// Whether a bisect session was started (and must be ended).
    session: bool,
}

fn set(files: &[String]) -> BTreeSet<String> {
    files.iter().cloned().collect()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Driver<'_> {
    fn search(&mut self) -> vl_core::Result<AutoVerdict> {
        let candidates = self.bisect.enabled_count()?;
        if candidates < 2 {
            return Err(Error::invalid_input(
                "an automatic search needs at least two enabled mods",
            ));
        }
        if self.setup.after_crash {
            let sources = Sources::newest(self.setup.game_dir.clone(), self.setup.log_dir());
            let (diagnosis, text) = diagnose_with_text(&sources, true)?;
            if diagnosis.files.is_empty() {
                return Err(Error::not_found(
                    "no logs of the crash were found; search without the crash",
                ));
            }
            let key = CrashKey::new(&diagnosis, &text);
            self.reference = Some((diagnosis, key));
        } else {
            self.hooks.progress("reproduce", 0, 1);
            let (outcome, seen) = self.trial(RunPurpose::Reproduce, candidates, self.watch)?;
            match outcome {
                BisectOutcome::Bad => self.reference = seen.diagnosis,
                BisectOutcome::Good => return Ok(AutoVerdict::NotReproduced),
                _ if seen.result == RunResult::Hung => return Ok(AutoVerdict::Hangs),
                _ => {
                    return Ok(AutoVerdict::Inconclusive {
                        suspects: Vec::new(),
                    });
                }
            }
        }
        let Some((diagnosis, _)) = self.reference.clone() else {
            return Err(Error::internal("a crash without a diagnosis"));
        };
        if diagnosis.loader_reported {
            return Ok(AutoVerdict::LoaderReported { diagnosis });
        }
        if diagnosis.cause == CrashCause::Environment {
            return Ok(AutoVerdict::NotMods {
                diagnosis: Some(diagnosis),
            });
        }

        let mut status = self.bisect.start(&diagnosis.mods)?;
        self.session = true;
        let all = set(&self.bisect.without(&[])?);
        self.known.insert(all, BisectOutcome::Bad);

        self.hooks
            .progress("without mods", 0, status.steps_estimate + 1);
        self.bisect.apply_set(&[])?;
        let (outcome, seen) = self.trial(RunPurpose::WithoutMods, 0, Watch::Menu)?;
        match seen.result {
            RunResult::Crashed | RunResult::OtherCrash => {
                return Ok(AutoVerdict::NotMods {
                    diagnosis: seen.diagnosis.map(|(d, _)| d),
                });
            }
            _ if outcome == BisectOutcome::Good => {
                self.known.insert(BTreeSet::new(), BisectOutcome::Good);
            }
            _ => {}
        }
        self.bisect.apply_set(&status.enabled)?;

        while status.state == BisectState::Running {
            let enabled = set(&status.enabled);
            let outcome = match self.known.get(&enabled) {
                Some(outcome) => *outcome,
                None => {
                    let done = status.steps_done;
                    self.hooks
                        .progress("search", done, done + status.steps_estimate);
                    let watch = self.watch;
                    self.trial(RunPurpose::Step, enabled.len(), watch)?.0
                }
            };
            status = self.bisect.report(outcome)?;
        }
        match status.verdict {
            Some(BisectVerdict::Found { culprits, needs }) => {
                let rest = self.bisect.without(&culprits)?;
                let confirmed = match self.known.get(&set(&rest)) {
                    Some(outcome) => *outcome == BisectOutcome::Good,
                    None => {
                        self.hooks
                            .progress("verify", status.steps_done, status.steps_done + 1);
                        self.bisect.apply_set(&rest)?;
                        let watch = self.watch;
                        self.trial(RunPurpose::Verify, rest.len(), watch)?.0 == BisectOutcome::Good
                    }
                };
                Ok(AutoVerdict::Found {
                    culprits,
                    needs,
                    confirmed,
                })
            }
            Some(BisectVerdict::Inconclusive { suspects }) => {
                Ok(AutoVerdict::Inconclusive { suspects })
            }
            _ => Ok(AutoVerdict::NotReproduced),
        }
    }

    /// Runs one set until the repeat rule decides.
    fn trial(
        &mut self,
        purpose: RunPurpose,
        enabled: usize,
        watch: Watch,
    ) -> vl_core::Result<(BisectOutcome, Seen)> {
        let mut trial = Trial::new(self.setup.repeats);
        loop {
            let seen = self.run_once(purpose, enabled, watch)?;
            if let Some(outcome) = trial.record(seen.result) {
                return Ok((outcome, seen));
            }
        }
    }

    fn run_once(
        &mut self,
        purpose: RunPurpose,
        enabled: usize,
        watch: Watch,
    ) -> vl_core::Result<Seen> {
        if let Some(world) = &self.world {
            world.restore()?;
        }
        let quick_play_log = self.setup.meta_dir().join(QUICK_PLAY_LOG);
        let mut args = Vec::new();
        if watch == Watch::QuickPlay {
            remove_file(&quick_play_log)?;
            let world = self.setup.world.as_deref().unwrap_or_default();
            args = quick_play_args(world, &quick_play_log.to_string_lossy());
        }
        let watched = self.watch_run(&args, watch, &quick_play_log);
        if let Some(world) = &self.world {
            world.restore()?;
        }
        let (exit, reached, hung, seconds) = watched?;
        let crashed = exit.crashed && !exit.stopped_by_user;
        let mut diagnosis = None;
        let result = if crashed {
            let sources = Sources {
                game_dir: self.setup.game_dir.clone(),
                log_dir: self.setup.log_dir(),
                crash_report: exit.crash_report.clone(),
                hs_err: exit.hs_err.clone(),
                run_log: exit.log_file.clone(),
            };
            let (d, text) = diagnose_with_text(&sources, false)?;
            let key = CrashKey::new(&d, &text);
            let same = self
                .reference
                .as_ref()
                .is_none_or(|(_, reference)| reference.same(&key));
            diagnosis = Some((d, key));
            if same {
                RunResult::Crashed
            } else {
                RunResult::OtherCrash
            }
        } else if hung {
            RunResult::Hung
        } else if reached || watch == Watch::Manual {
            RunResult::Reached
        } else {
            RunResult::Closed
        };
        let run = BisectRun {
            purpose,
            enabled: count(enabled),
            result,
            crash: diagnosis.as_ref().map(|(_, key)| key.to_string()),
            seconds,
        };
        let status = if self.session {
            self.bisect.status().ok()
        } else {
            None
        };
        let (done, estimate) = status.map_or((0, 0), |s| (s.steps_done, s.steps_estimate));
        self.hooks.run(&run, done, estimate);
        self.runs.push(run);
        Ok(Seen { result, diagnosis })
    }

    /// Starts the game and watches it until it exits: the exit, whether the target was reached,
    /// whether it hung, and the seconds it ran.
    fn watch_run(
        &mut self,
        args: &[String],
        watch: Watch,
        quick_play_log: &Path,
    ) -> vl_core::Result<(GameExit, bool, bool, u32)> {
        let mut game = self.starter.start(args)?;
        let mut running: Option<Instant> = None;
        let mut reached: Option<Instant> = None;
        let mut stopping: Option<Instant> = None;
        let mut hung = false;
        let mut cancelled = false;
        let exit = loop {
            if self.hooks.cancelled() && stopping.is_none() {
                game.stop();
                stopping = Some(Instant::now());
                cancelled = true;
            }
            match game.next(POLL) {
                Some(GameSignal::Started) => {
                    running.get_or_insert_with(Instant::now);
                }
                Some(GameSignal::Lines(lines)) => {
                    running.get_or_insert_with(Instant::now);
                    if watch == Watch::Menu
                        && reached.is_none()
                        && lines.iter().any(|l| reached_menu(l))
                    {
                        reached = Some(Instant::now());
                    }
                }
                Some(GameSignal::Exited(exit)) => break exit,
                Some(GameSignal::Failed(e)) => {
                    return Err(if cancelled {
                        Error::new(ErrorKind::Cancelled, "the search was cancelled")
                    } else {
                        e
                    });
                }
                None => {}
            }
            if watch == Watch::QuickPlay && reached.is_none() && quick_play_log.is_file() {
                reached = Some(Instant::now());
            }
            match (stopping, reached, running) {
                (Some(at), _, _) => {
                    if at.elapsed() > STOP_GRACE {
                        return Err(Error::new(ErrorKind::Game, "the game does not stop"));
                    }
                }
                (None, Some(at), _) if at.elapsed() >= self.setup.stable => {
                    game.stop();
                    stopping = Some(Instant::now());
                }
                (None, None, Some(at))
                    if watch != Watch::Manual && at.elapsed() >= self.setup.timeout =>
                {
                    game.stop();
                    stopping = Some(Instant::now());
                    hung = true;
                }
                _ => {}
            }
        };
        if cancelled {
            return Err(Error::new(ErrorKind::Cancelled, "the search was cancelled"));
        }
        let seconds = u32::try_from(exit.session_ms / 1000).unwrap_or(u32::MAX);
        Ok((exit, reached.is_some(), hung, seconds))
    }

    /// Ends the session (restoring the mods) and puts the world back.
    fn restore_all(&mut self) -> vl_core::Result<()> {
        let mods = if self.session {
            self.bisect.stop()
        } else {
            Ok(())
        };
        let world = match &self.world {
            Some(world) => world.finish(),
            None => Ok(()),
        };
        let _ = remove_file(&self.setup.meta_dir().join(QUICK_PLAY_LOG));
        mods.and(world)
    }
}

/// The world under test and its copy.
struct WorldCopy {
    live: PathBuf,
    copy: PathBuf,
}

impl WorldCopy {
    fn new(setup: &Setup, name: &str) -> vl_core::Result<Self> {
        let live = setup.resources().world(name)?.root().to_path_buf();
        Ok(Self {
            live,
            copy: setup.meta_dir().join(WORLD_BACKUP_DIR).join(name),
        })
    }

    /// Copies the world. A copy left by an earlier search is the original and is kept.
    fn save(&self) -> vl_core::Result<()> {
        if self.copy.is_dir() {
            return self.restore();
        }
        let temp = self.copy.with_extension("partial");
        remove_dir(&temp)?;
        copy_dir(&self.live, &temp)?;
        fs::rename(&temp, &self.copy).map_err(|e| io_err(&self.copy, &e))
    }

    /// Puts the copy back in place of the world.
    fn restore(&self) -> vl_core::Result<()> {
        replace_dir(&self.copy, &self.live)
    }

    /// Puts the copy back and deletes it.
    fn finish(&self) -> vl_core::Result<()> {
        self.restore()?;
        remove_dir(&self.copy)?;
        if let Some(parent) = self.copy.parent() {
            let _ = fs::remove_dir(parent);
        }
        Ok(())
    }
}

fn io_err(path: &Path, e: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

/// Replaces `target` with a copy of `source`.
fn replace_dir(source: &Path, target: &Path) -> vl_core::Result<()> {
    remove_dir(target)?;
    copy_dir(source, target)
}

fn copy_dir(source: &Path, target: &Path) -> vl_core::Result<()> {
    fs::create_dir_all(target).map_err(|e| io_err(target, &e))?;
    for entry in fs::read_dir(source).map_err(|e| io_err(source, &e))? {
        let entry = entry.map_err(|e| io_err(source, &e))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| io_err(&from, &e))?;
        if kind.is_dir() {
            copy_dir(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).map_err(|e| io_err(&from, &e))?;
        }
    }
    Ok(())
}

fn remove_dir(path: &Path) -> vl_core::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, &e)),
    }
}

fn remove_file(path: &Path) -> vl_core::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, &e)),
    }
}

#[cfg(test)]
#[path = "auto_bisect_tests.rs"]
mod tests;
