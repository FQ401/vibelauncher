use std::collections::{BTreeSet, VecDeque};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use vl_core::bisect::AutoBisectRequest;
use vl_core::resources::ResourceKind;

use super::*;

/// What the fake game does with a set of mods.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plays {
    /// Reaches the main menu (and the world, with Quick Play) and runs until stopped.
    Fine,
    /// Crashes with a crash report naming `exception`.
    Crash(&'static str),
    /// Prints a Fabric loader error and exits.
    LoaderError,
    /// Never reaches the target.
    Hang,
    /// Exits on its own with code 0 (a player closing the game).
    Quit,
}

type Script = dyn Fn(&BTreeSet<String>, u32) -> Plays + Send + Sync;

struct Fake {
    game_dir: PathBuf,
    script: Arc<Script>,
    runs: Arc<AtomicU32>,
    args: Arc<Mutex<Vec<Vec<String>>>>,
}

struct FakeGame {
    signals: VecDeque<GameSignal>,
    /// The exit sent once the game is stopped.
    on_stop: Option<GameExit>,
    stopped: bool,
}

fn exit(crashed: bool, stopped: bool) -> GameExit {
    GameExit {
        code: Some(i32::from(crashed)),
        crashed,
        stopped_by_user: stopped,
        session_ms: 1000,
        crash_report: None,
        hs_err: None,
        log_file: None,
        tail: Vec::new(),
    }
}

impl GameStarter for Fake {
    fn start(&mut self, extra_args: &[String]) -> vl_core::Result<Box<dyn GameProcess>> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        self.args.lock().unwrap().push(extra_args.to_vec());
        let enabled: BTreeSet<String> = Resources::new(self.game_dir.clone(), PathBuf::new())
            .list(ResourceKind::Mods)?
            .into_iter()
            .filter(|e| e.enabled)
            .map(|e| e.file)
            .collect();
        let mut signals = VecDeque::from([GameSignal::Started]);
        let mut on_stop = None;
        match (self.script)(&enabled, n) {
            Plays::Fine => {
                signals.push_back(GameSignal::Lines(vec![
                    "[Render thread/INFO]: Reloading ResourceManager: vanilla".into(),
                    "[Render thread/INFO]: Sound engine started".into(),
                ]));
                // A Quick Play run enters the world: the log appears and the world changes.
                if let Some(i) = extra_args.iter().position(|a| a == "--quickPlayPath") {
                    let log = PathBuf::from(&extra_args[i + 1]);
                    let world = &extra_args[i + 3];
                    let saves = self.game_dir.join("saves").join(world);
                    fs::write(saves.join("touched.dat"), b"changed").unwrap();
                    fs::write(log, b"{}").unwrap();
                }
                on_stop = Some(exit(false, true));
            }
            Plays::Crash(exception) => {
                let dir = self.game_dir.join("crash-reports");
                fs::create_dir_all(&dir).unwrap();
                let report = dir.join(format!("crash-{n}.txt"));
                let text = format!(
                    "---- Minecraft Crash Report ----\nDescription: Initializing game\n\n\
                     {exception}: synthetic failure\n\tat example.synthetic.Main.init(Main.java:1)\n"
                );
                fs::write(&report, text).unwrap();
                let mut crashed = exit(true, false);
                crashed.crash_report = Some(report);
                signals.push_back(GameSignal::Exited(crashed));
            }
            Plays::LoaderError => {
                let log = self.game_dir.join(format!("run-{n}.log"));
                fs::write(
                    &log,
                    "[main/INFO]: start\nSome of your mods are incompatible with the game or each \
                     other!\n\t - Mod 'Example' (example) 1.0 requires version 2.0 or later of \
                     fabric-api, which is missing!\n",
                )
                .unwrap();
                let mut crashed = exit(true, false);
                crashed.log_file = Some(log);
                signals.push_back(GameSignal::Exited(crashed));
            }
            Plays::Hang => on_stop = Some(exit(false, true)),
            Plays::Quit => signals.push_back(GameSignal::Exited(exit(false, false))),
        }
        Ok(Box::new(FakeGame {
            signals,
            on_stop,
            stopped: false,
        }))
    }
}

impl GameProcess for FakeGame {
    fn next(&mut self, _timeout: Duration) -> Option<GameSignal> {
        if let Some(signal) = self.signals.pop_front() {
            return Some(signal);
        }
        if self.stopped {
            return self.on_stop.take().map(GameSignal::Exited);
        }
        std::thread::sleep(Duration::from_millis(5));
        None
    }

    fn stop(&mut self) {
        self.stopped = true;
    }
}

#[derive(Default)]
struct Recorder {
    cancel_after: Option<usize>,
    runs: Vec<BisectRun>,
    stages: Vec<String>,
}

impl Hooks for Recorder {
    fn cancelled(&self) -> bool {
        self.cancel_after.is_some_and(|n| self.runs.len() >= n)
    }

    fn progress(&mut self, stage: &str, _done: u32, _total: u32) {
        self.stages.push(stage.to_owned());
    }

    fn run(&mut self, run: &BisectRun, _steps_done: u32, _steps_estimate: u32) {
        self.runs.push(run.clone());
    }
}

struct Instance {
    dir: PathBuf,
}

impl Instance {
    /// Six mods and a library; `bad.jar` needs the library.
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vl-auto-bisect-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("minecraft").join("mods")).unwrap();
        let t = Self { dir };
        t.jar("fabric-api", &[]);
        for i in 0..5 {
            let needs: &[&str] = if i % 2 == 0 { &["fabric-api"] } else { &[] };
            t.jar(&format!("mod{i}"), needs);
        }
        t.jar("bad", &["fabric-api"]);
        t
    }

    fn game_dir(&self) -> PathBuf {
        self.dir.join("minecraft")
    }

    /// A world folder with a `level.dat`.
    fn world(&self, name: &str) -> PathBuf {
        let world = self.game_dir().join("saves").join(name);
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("level.dat"), b"original").unwrap();
        world
    }

    fn jar(&self, id: &str, needs: &[&str]) {
        let depends: Vec<String> = needs.iter().map(|n| format!("\"{n}\":\"*\"")).collect();
        let json = format!(
            "{{\"id\":\"{id}\",\"version\":\"1\",\"depends\":{{{}}}}}",
            depends.join(",")
        );
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(json.as_bytes()).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let path = self.game_dir().join("mods").join(format!("{id}.jar"));
        fs::write(path, bytes).unwrap();
    }

    fn mods_state(&self) -> Vec<(String, bool)> {
        Resources::new(self.game_dir(), self.dir.join(".trash"))
            .list(ResourceKind::Mods)
            .unwrap()
            .into_iter()
            .map(|e| (e.file, e.enabled))
            .collect()
    }

    fn setup(&self) -> Setup {
        Setup {
            game_dir: self.game_dir(),
            instance_dir: self.dir.clone(),
            game_version: "1.21.4".into(),
            target: BisectTarget::Menu,
            world: None,
            stable: Duration::ZERO,
            repeats: 1,
            timeout: Duration::from_millis(300),
            after_crash: false,
        }
    }

    fn fake(
        &self,
        script: impl Fn(&BTreeSet<String>, u32) -> Plays + Send + Sync + 'static,
    ) -> Fake {
        Fake {
            game_dir: self.game_dir(),
            script: Arc::new(script),
            runs: Arc::default(),
            args: Arc::default(),
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const ISE: &str = "java.lang.IllegalStateException";
const NPE: &str = "java.lang.NullPointerException";

fn bad_crashes(enabled: &BTreeSet<String>, _run: u32) -> Plays {
    if enabled.contains("bad.jar") {
        Plays::Crash(ISE)
    } else {
        Plays::Fine
    }
}

fn purposes(runs: &[BisectRun]) -> Vec<RunPurpose> {
    runs.iter().map(|r| r.purpose).collect()
}

#[test]
fn finds_the_culprit_checks_it_and_restores_the_mods() {
    let t = Instance::new("find");
    let before = t.mods_state();
    let mut fake = t.fake(bad_crashes);
    let mut hooks = Recorder::default();
    let report = run(&t.setup(), &mut fake, &mut hooks).unwrap();
    assert_eq!(
        report.verdict,
        AutoVerdict::Found {
            culprits: vec!["bad.jar".into()],
            needs: vec!["fabric-api.jar".into()],
            confirmed: true,
        }
    );
    let kinds = purposes(&report.runs);
    assert_eq!(kinds.first(), Some(&RunPurpose::Reproduce));
    assert_eq!(kinds.get(1), Some(&RunPurpose::WithoutMods));
    assert_eq!(kinds.last(), Some(&RunPurpose::Verify));
    assert!(kinds.iter().filter(|k| **k == RunPurpose::Step).count() <= 6);
    assert_eq!(report.runs[0].result, RunResult::Crashed);
    assert_eq!(
        report.runs[0].crash.as_deref(),
        Some("crash-undetermined (java.lang.IllegalStateException)")
    );
    assert_eq!(report.runs, hooks.runs);
    assert!(hooks.stages.contains(&"search".to_owned()));
    assert!(!report.semi_manual);
    assert_eq!(t.mods_state(), before);
    assert!(!t.dir.join(".vibelauncher").join("bisect.json").exists());
}

#[test]
fn a_crash_of_another_kind_is_never_bad() {
    let t = Instance::new("other");
    // mod1 without the culprit crashes differently: those runs say nothing.
    let mut fake = t.fake(|enabled, _| {
        if enabled.contains("bad.jar") {
            Plays::Crash(ISE)
        } else if enabled.contains("mod1.jar") {
            Plays::Crash(NPE)
        } else {
            Plays::Fine
        }
    });
    let report = run(&t.setup(), &mut fake, &mut Recorder::default()).unwrap();
    assert!(
        report
            .runs
            .iter()
            .any(|r| r.result == RunResult::OtherCrash)
    );
    match report.verdict {
        AutoVerdict::Found {
            culprits,
            confirmed,
            ..
        } => {
            assert_eq!(culprits, ["bad.jar"]);
            // The check without the culprit still has mod1, which crashes in its own way.
            assert!(!confirmed);
        }
        AutoVerdict::Inconclusive { suspects } => assert!(suspects.contains(&"bad.jar".into())),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_problem_named_by_the_loader_needs_no_search() {
    let t = Instance::new("loader");
    let before = t.mods_state();
    let mut fake = t.fake(|_, _| Plays::LoaderError);
    let report = run(&t.setup(), &mut fake, &mut Recorder::default()).unwrap();
    match &report.verdict {
        AutoVerdict::LoaderReported { diagnosis } => {
            assert_eq!(diagnosis.signature.as_deref(), Some("missing_dependency"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(report.runs.len(), 1);
    assert_eq!(t.mods_state(), before);
}

#[test]
fn a_crash_without_mods_is_not_the_mods() {
    let t = Instance::new("notmods");
    let before = t.mods_state();
    let mut fake = t.fake(|_, _| Plays::Crash(ISE));
    let report = run(&t.setup(), &mut fake, &mut Recorder::default()).unwrap();
    assert!(matches!(
        report.verdict,
        AutoVerdict::NotMods { diagnosis: Some(_) }
    ));
    assert_eq!(
        purposes(&report.runs),
        [RunPurpose::Reproduce, RunPurpose::WithoutMods]
    );
    assert_eq!(report.runs[1].enabled, 0);
    assert_eq!(t.mods_state(), before);
}

#[test]
fn no_problem_and_a_hang_end_early() {
    let t = Instance::new("early");
    let mut fake = t.fake(|_, _| Plays::Fine);
    let report = run(&t.setup(), &mut fake, &mut Recorder::default()).unwrap();
    assert_eq!(report.verdict, AutoVerdict::NotReproduced);
    let mut fake = t.fake(|_, _| Plays::Hang);
    let report = run(&t.setup(), &mut fake, &mut Recorder::default()).unwrap();
    assert_eq!(report.verdict, AutoVerdict::Hangs);
    assert_eq!(report.runs[0].result, RunResult::Hung);
}

#[test]
fn good_needs_k_clean_runs_and_bad_one_crash() {
    let t = Instance::new("repeats");
    // The culprit crashes only on every other run.
    let mut fake = t.fake(|enabled, n| {
        if enabled.contains("bad.jar") && n % 2 == 0 {
            Plays::Crash(ISE)
        } else {
            Plays::Fine
        }
    });
    let mut setup = t.setup();
    setup.repeats = 3;
    let report = run(&setup, &mut fake, &mut Recorder::default()).unwrap();
    let verify = report
        .runs
        .iter()
        .filter(|r| r.purpose == RunPurpose::Verify)
        .count();
    assert_eq!(verify, 3, "a good set is run K times");
    assert!(matches!(report.verdict, AutoVerdict::Found { .. }));
}

#[test]
fn after_a_crash_the_newest_logs_are_the_problem() {
    let t = Instance::new("after");
    let dir = t.game_dir().join("crash-reports");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("crash-old.txt"),
        format!("---- Minecraft Crash Report ----\n\n{ISE}: x\n\tat a.B.c(B.java:1)\n"),
    )
    .unwrap();
    let mut fake = t.fake(bad_crashes);
    let mut setup = t.setup();
    setup.after_crash = true;
    let report = run(&setup, &mut fake, &mut Recorder::default()).unwrap();
    assert_eq!(report.runs[0].purpose, RunPurpose::WithoutMods);
    assert!(matches!(report.verdict, AutoVerdict::Found { .. }));
    assert!(report.reference.is_some());

    let empty = Instance::new("after-none");
    let mut setup = empty.setup();
    setup.after_crash = true;
    let mut fake = empty.fake(bad_crashes);
    let err = run(&setup, &mut fake, &mut Recorder::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn quick_play_enters_the_world_and_the_world_is_put_back() {
    let t = Instance::new("world");
    let world = t.game_dir().join("saves").join("Test World");
    fs::create_dir_all(world.join("region")).unwrap();
    fs::write(world.join("level.dat"), b"original").unwrap();
    let mut fake = t.fake(bad_crashes);
    let args = Arc::clone(&fake.args);
    let mut setup = t.setup();
    setup.target = BisectTarget::World;
    setup.world = Some("Test World".into());
    let report = run(&setup, &mut fake, &mut Recorder::default()).unwrap();
    assert!(matches!(report.verdict, AutoVerdict::Found { .. }));
    let args = args.lock().unwrap();
    assert!(args[0].iter().any(|a| a == "--quickPlaySingleplayer"));
    assert!(args[0].iter().any(|a| a == "Test World"));
    // The run without mods only goes to the main menu.
    assert!(args[1].is_empty());
    assert!(args.iter().skip(2).all(|a| !a.is_empty()));
    assert!(!world.join("touched.dat").exists(), "the world is put back");
    assert_eq!(fs::read(world.join("level.dat")).unwrap(), b"original");
    assert!(!t.dir.join(".vibelauncher").join(WORLD_BACKUP_DIR).exists());
}

#[test]
fn old_versions_run_semi_manually() {
    let t = Instance::new("manual");
    t.world("w");
    let mut fake = t.fake(|enabled, _| {
        if enabled.contains("bad.jar") {
            Plays::Crash(ISE)
        } else if enabled.is_empty() {
            Plays::Fine
        } else {
            Plays::Quit
        }
    });
    let args = Arc::clone(&fake.args);
    let mut setup = t.setup();
    setup.game_version = "1.16.5".into();
    setup.target = BisectTarget::World;
    setup.world = Some("w".into());
    let report = run(&setup, &mut fake, &mut Recorder::default()).unwrap();
    assert!(report.semi_manual);
    assert!(args.lock().unwrap().iter().all(Vec::is_empty));
    assert!(matches!(
        report.verdict,
        AutoVerdict::Found {
            confirmed: true,
            ..
        }
    ));
    assert!(
        report
            .runs
            .iter()
            .filter(|r| r.purpose == RunPurpose::Step)
            .any(|r| r.result == RunResult::Reached)
    );
}

#[test]
fn cancelling_restores_the_mods_and_the_world() {
    let t = Instance::new("cancel");
    let world = t.world("w");
    let before = t.mods_state();
    let mut fake = t.fake(bad_crashes);
    let mut hooks = Recorder {
        cancel_after: Some(3),
        ..Recorder::default()
    };
    let mut setup = t.setup();
    setup.target = BisectTarget::World;
    setup.world = Some("w".into());
    let err = run(&setup, &mut fake, &mut hooks).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert_eq!(t.mods_state(), before);
    assert!(!world.join("touched.dat").exists());
    assert!(!t.dir.join(".vibelauncher").join("bisect.json").exists());
    assert!(!t.dir.join(".vibelauncher").join(WORLD_BACKUP_DIR).exists());
}

#[test]
fn a_world_left_behind_is_restored() {
    let t = Instance::new("leftover");
    let world = t.game_dir().join("saves").join("w");
    fs::create_dir_all(&world).unwrap();
    fs::write(world.join("touched.dat"), b"changed").unwrap();
    let copy = t.dir.join(".vibelauncher").join(WORLD_BACKUP_DIR).join("w");
    fs::create_dir_all(&copy).unwrap();
    fs::write(copy.join("level.dat"), b"original").unwrap();
    restore_leftover_world(&t.game_dir(), &t.dir).unwrap();
    assert!(!world.join("touched.dat").exists());
    assert_eq!(fs::read(world.join("level.dat")).unwrap(), b"original");
    assert!(!t.dir.join(".vibelauncher").join(WORLD_BACKUP_DIR).exists());
    // Nothing left: nothing to do.
    restore_leftover_world(&t.game_dir(), &t.dir).unwrap();
}

#[test]
fn a_search_needs_two_mods_and_a_world_name() {
    let t = Instance::new("checks");
    let mut setup = t.setup();
    setup.target = BisectTarget::World;
    let mut fake = t.fake(bad_crashes);
    let err = run(&setup, &mut fake, &mut Recorder::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    setup.world = Some("missing".into());
    let err = run(&setup, &mut fake, &mut Recorder::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let lonely = Instance::new("lonely");
    for entry in fs::read_dir(lonely.game_dir().join("mods")).unwrap() {
        let path = entry.unwrap().path();
        if !path.ends_with("bad.jar") {
            fs::remove_file(path).unwrap();
        }
    }
    let err = run(&lonely.setup(), &mut fake, &mut Recorder::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    // The request defaults are what the facade documents.
    let request = AutoBisectRequest::new(vl_core::InstanceId::new("x").unwrap());
    assert_eq!(request.repeats, 1);
    assert_eq!(request.target, BisectTarget::Menu);
}
