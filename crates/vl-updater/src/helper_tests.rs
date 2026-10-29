//! Tests of the update helper against real processes: copies of this test binary play the
//! launcher that has to exit, the launcher started again, and the helper process itself (the
//! ignored tests below, selected with `--exact … --ignored`).

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vl_update::{Artifact, ArtifactKind, InstallMethod, Restart, Version};

use crate::helper::{
    self, HelperJob, HelperOutcome, ParentProcess, execute, prepare, read_job, restart_launcher,
};
use crate::testkit::{Dir, sha256};
use crate::{NEW_DIR, OLD_DIR, StagedUpdate, UpdateDir, UpdaterError, clean_portable};

const SLEEP_VAR: &str = "VL_TEST_FAKE_LAUNCHER_MS";
const LOCK_VAR: &str = "VL_TEST_LOCK_DIR";
const JOB_VAR: &str = "VL_TEST_HELPER_JOB";
/// A file in the working folder that lets `fake_restarted` write its proof.
const EXPECT_RESTART: &str = "expect-restart";
const RESTART_PROOF: &str = "restarted.proof";

/// Plays the launcher: holds the data folder lock of `VL_TEST_LOCK_DIR` (if set) and runs for
/// `VL_TEST_FAKE_LAUNCHER_MS` milliseconds. Does nothing when run by hand.
#[test]
#[ignore = "a process started by the helper tests"]
fn fake_launcher() {
    let Some(ms) = std::env::var(SLEEP_VAR).ok().and_then(|v| v.parse().ok()) else {
        return;
    };
    let _lock = std::env::var_os(LOCK_VAR).map(|d| vl_store::DirLock::acquire(Path::new(&d)));
    std::thread::sleep(Duration::from_millis(ms));
}

/// Plays the launcher started after the update: leaves a proof in its working folder.
#[test]
#[ignore = "a process started by the helper tests"]
fn fake_restarted() {
    if Path::new(EXPECT_RESTART).exists() {
        std::fs::write(RESTART_PROOF, b"ok").unwrap();
    }
}

/// Plays the helper process: runs the job file named by `VL_TEST_HELPER_JOB`.
#[test]
#[ignore = "a process started by the helper tests"]
fn helper_process() {
    if let Some(job) = std::env::var_os(JOB_VAR) {
        std::process::exit(helper::run(Path::new(&job)));
    }
}

fn exe_name() -> String {
    format!("vibelauncher{}", std::env::consts::EXE_SUFFIX)
}

/// This test binary started to run only the ignored test `name`.
fn test_process(exe: &Path, name: &str) -> Command {
    let mut command = Command::new(exe);
    command
        .args(["--exact", name, "--ignored", "--test-threads=1", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// The arguments that make a restarted copy of this test binary run `fake_restarted`.
fn restart_args() -> Vec<String> {
    [
        "--exact",
        "helper_tests::fake_restarted",
        "--ignored",
        "--quiet",
    ]
    .map(String::from)
    .to_vec()
}

/// A launcher process that runs for `ms` milliseconds, holding the lock of `data` if given.
fn fake_launcher_process(exe: &Path, ms: u64, data: Option<&Path>) -> Child {
    let mut command = test_process(exe, "helper_tests::fake_launcher");
    command.env(SLEEP_VAR, ms.to_string());
    if let Some(data) = data {
        command.env(LOCK_VAR, data);
    }
    command.spawn().unwrap()
}

/// A process id that has just exited.
fn exited_parent() -> ParentProcess {
    let mut child = test_process(&std::env::current_exe().unwrap(), "no-such-test")
        .spawn()
        .unwrap();
    child.wait().unwrap();
    ParentProcess {
        pid: child.id(),
        start_time: None,
    }
}

/// An uncompressed zip with `(name, content)` entries.
fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in entries {
        w.start_file(*name, options).unwrap();
        w.write_all(content).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// A portable install of version 1.0.0 with a downloaded 1.1.0 archive.
struct Install {
    root: Dir,
    dir: UpdateDir,
    staged: StagedUpdate,
}

impl Install {
    /// The install folder holds a copy of this test binary as the launcher and `old.txt`; the
    /// archive holds `version.txt` and, with `new_exe`, a launcher executable too.
    fn new(archive_entries: &[(&str, &[u8])]) -> Self {
        let root = Dir::new();
        std::fs::create_dir_all(root.path().join("app")).unwrap();
        std::fs::create_dir_all(root.path().join("data")).unwrap();
        std::fs::copy(
            std::env::current_exe().unwrap(),
            root.path().join("app").join(exe_name()),
        )
        .unwrap();
        std::fs::write(root.path().join("app/old.txt"), b"old").unwrap();
        let dir = UpdateDir::new(root.path().join("data/updates"));
        let version = Version::new(1, 1, 0);
        let archive = stored_zip(archive_entries);
        let file = dir.staging(&version).join("vibelauncher-1.1.0.zip");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &archive).unwrap();
        let artifact = Artifact {
            platform: vl_core::platform::Platform::current(),
            kind: ArtifactKind::Portable,
            url: "https://downloads.example.test/vibelauncher.zip".into(),
            sha256: sha256(&archive),
            size: archive.len() as u64,
        };
        dir.record_staged(&version, &artifact, &file).unwrap();
        let staged = dir.staged().unwrap();
        Self { root, dir, staged }
    }

    fn app(&self) -> PathBuf {
        self.root.path().join("app")
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn exe(&self) -> PathBuf {
        self.app().join(exe_name())
    }

    fn job(&self, restart: Option<Vec<String>>) -> HelperJob {
        prepare(
            &self.dir,
            &self.staged,
            &Version::new(1, 0, 0),
            &InstallMethod::Portable { dir: self.app() },
            &self.exe(),
            &self.data(),
            restart,
        )
        .unwrap()
    }
}

fn wait_for_file(path: &Path, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn waits_for_the_launcher_installs_and_starts_the_new_version() {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let name = exe_name();
    let install = Install::new(&[(name.as_str(), &exe), ("version.txt", b"new")]);
    let mut job = install.job(Some(restart_args()));
    // The launcher runs from the install folder and holds the data folder for another second.
    let mut launcher = fake_launcher_process(&install.exe(), 1000, Some(&install.data()));
    job.parent = ParentProcess::of(launcher.id());
    assert!(job.parent.is_running());
    std::fs::write(install.app().join(EXPECT_RESTART), b"").unwrap();
    // Reap the launcher when it exits, as its parent would.
    let reaper = std::thread::spawn(move || launcher.wait());

    let start = Instant::now();
    assert_eq!(execute(&job, &restart_launcher), HelperOutcome::Installed);
    assert!(start.elapsed() >= Duration::from_millis(500), "it waited");
    reaper.join().unwrap().unwrap();

    let app = install.app();
    assert_eq!(std::fs::read(app.join("version.txt")).unwrap(), b"new");
    assert_eq!(std::fs::read(app.join("old.txt")).unwrap(), b"old");
    assert!(app.join(OLD_DIR).join(&name).exists());
    assert!(!app.join(NEW_DIR).exists());
    let marker = install
        .dir
        .take_just_updated(&Version::new(1, 1, 0))
        .unwrap();
    assert_eq!(marker.from, Version::new(1, 0, 0));
    assert_eq!(install.dir.staged(), None);
    assert_eq!(install.dir.take_failure(), None);
    // The new launcher was started from the install folder.
    assert!(wait_for_file(
        &app.join(RESTART_PROOF),
        Duration::from_secs(30)
    ));
    // Let it exit before the folder is removed.
    std::thread::sleep(Duration::from_millis(500));
    clean_portable(&app);
}

#[test]
fn a_failed_replacement_is_undone_recorded_and_the_old_launcher_started() {
    // `data` at the top level of the archive is refused while unpacking.
    let install = Install::new(&[("version.txt", b"new"), ("data/x", b"x")]);
    let mut job = install.job(Some(vec!["--again".into()]));
    job.parent = exited_parent();
    let started: Mutex<Vec<(Restart, Vec<String>)>> = Mutex::default();
    let outcome = execute(&job, &|target, args| {
        started
            .lock()
            .unwrap()
            .push((target.clone(), args.to_vec()));
        Ok(())
    });
    assert!(matches!(&outcome, HelperOutcome::Failed(m) if m.contains("data")));
    assert!(!install.app().join("version.txt").exists());
    assert_eq!(
        std::fs::read(install.app().join("old.txt")).unwrap(),
        b"old"
    );
    let failure = install.dir.take_failure().unwrap();
    assert_eq!(failure.version, Version::new(1, 1, 0));
    assert!(failure.message.contains("data"), "{}", failure.message);
    assert_eq!(install.dir.take_just_updated(&Version::new(1, 1, 0)), None);
    // The download stays for another try; the old launcher is started again.
    assert!(install.dir.staged().is_some());
    assert_eq!(
        *started.lock().unwrap(),
        vec![(Restart::Executable(install.exe()), vec!["--again".into()])]
    );
}

#[test]
fn nothing_is_replaced_while_the_launcher_keeps_running() {
    let install = Install::new(&[("version.txt", b"new")]);
    let mut job = install.job(Some(Vec::new()));
    let mut launcher = fake_launcher_process(&install.exe(), 20_000, None);
    job.parent = ParentProcess::of(launcher.id());
    job.wait_secs = 1;
    let restarted = Mutex::new(false);
    let outcome = execute(&job, &|_, _| {
        *restarted.lock().unwrap() = true;
        Ok(())
    });
    let _ = launcher.kill();
    let _ = launcher.wait();
    assert_eq!(outcome, HelperOutcome::LauncherRunning);
    assert!(!*restarted.lock().unwrap());
    assert!(!install.app().join("version.txt").exists());
    let failure = install.dir.take_failure().unwrap();
    assert!(
        failure.message.contains("did not exit"),
        "{}",
        failure.message
    );
    assert!(install.dir.staged().is_some());
}

#[test]
fn the_helper_process_runs_a_job_file() {
    let install = Install::new(&[("version.txt", b"new")]);
    let mut job = install.job(None);
    job.parent = exited_parent();
    let job_path = install.root.path().join("job.json");
    std::fs::write(&job_path, serde_json::to_vec(&job).unwrap()).unwrap();
    let status = test_process(
        &std::env::current_exe().unwrap(),
        "helper_tests::helper_process",
    )
    .env(JOB_VAR, &job_path)
    .status()
    .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(!job_path.exists(), "the job file is removed");
    assert_eq!(
        std::fs::read(install.app().join("version.txt")).unwrap(),
        b"new"
    );
    assert!(
        install
            .dir
            .take_just_updated(&Version::new(1, 1, 0))
            .is_some()
    );
}

#[test]
fn a_running_executable_is_renamed_away_and_replaced() {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let name = exe_name();
    let install = Install::new(&[(name.as_str(), &exe), ("version.txt", b"new")]);
    let archive = &install.staged.file;
    // The old launcher still runs while its folder is replaced (Windows locks a running image
    // against writing and deleting, not against renaming).
    let mut launcher = fake_launcher_process(&install.exe(), 20_000, None);
    let result = crate::replace_portable(archive, &install.app());
    let _ = launcher.kill();
    let _ = launcher.wait();
    let names = result.unwrap();
    assert!(names.contains(&name));
    assert!(install.app().join(OLD_DIR).join(&name).exists());
    assert_eq!(std::fs::read(install.exe()).unwrap(), exe);
    clean_portable(&install.app());
    assert!(!install.app().join(OLD_DIR).exists());
}

#[test]
fn prepare_refuses_what_it_cannot_install() {
    let install = Install::new(&[("version.txt", b"new")]);
    let from = Version::new(1, 0, 0);
    let prepare_for = |method: InstallMethod| {
        prepare(
            &install.dir,
            &install.staged,
            &from,
            &method,
            &install.exe(),
            &install.data(),
            None,
        )
    };
    assert_eq!(
        prepare_for(InstallMethod::Deb).unwrap_err(),
        UpdaterError::NothingToReplace
    );
    assert!(matches!(
        prepare_for(InstallMethod::Msi).unwrap_err(),
        UpdaterError::NotImplemented(_)
    ));
    assert!(matches!(
        prepare_for(InstallMethod::MacApp {
            bundle: "/Applications/VibeLauncher.app".into()
        })
        .unwrap_err(),
        UpdaterError::NotImplemented(_)
    ));
    // A download changed after it was checked is refused before the launcher exits.
    std::fs::write(&install.staged.file, b"changed").unwrap();
    assert!(matches!(
        prepare_for(InstallMethod::Portable { dir: install.app() }).unwrap_err(),
        UpdaterError::Integrity(_)
    ));
}

#[test]
fn job_files_round_trip_and_damaged_ones_are_refused() {
    let install = Install::new(&[("version.txt", b"new")]);
    let job = install.job(Some(vec!["--x".into()]));
    assert_eq!(job.parent.pid, std::process::id());
    let path = install.root.path().join("job.json");
    std::fs::write(&path, serde_json::to_vec(&job).unwrap()).unwrap();
    assert_eq!(read_job(&path), Some(job.clone()));
    let mut other = serde_json::to_value(&job).unwrap();
    other["format"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
    assert_eq!(read_job(&path), None);
    std::fs::write(&path, b"{").unwrap();
    assert_eq!(read_job(&path), None);
    std::fs::write(&path, vec![b' '; 70 * 1024]).unwrap();
    assert_eq!(read_job(&path), None);
    assert_eq!(read_job(&install.root.path().join("missing.json")), None);
    assert_eq!(helper::run(&install.root.path().join("missing.json")), 2);
}

#[test]
fn the_running_process_is_running() {
    assert!(ParentProcess::current().is_running());
    assert!(ParentProcess::current().start_time.is_some());
    assert!(!exited_parent().is_running());
    // Another process with the same id but a different start time is not the launcher.
    let mut me = ParentProcess::current();
    me.start_time = me.start_time.map(|t| t + 1);
    assert!(!me.is_running());
}
