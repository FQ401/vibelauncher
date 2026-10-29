//! The update helper: a second process that installs a downloaded update once the launcher has
//! exited, then starts the launcher again.
//!
//! The helper is a mode of the launcher binary, selected by [`HELPER_ARG`] and the path of a job
//! file. The launcher prepares a [`HelperJob`] ([`prepare`]), writes it to
//! `<updates>/helper/job.json`, starts the helper ([`launch`]) and exits. The helper never runs
//! from the files it replaces: it is a copy of the launcher executable in `<updates>/helper/`, or
//! for an AppImage the AppImage itself (Linux keeps a renamed-over file open).
//!
//! The helper ([`run`], [`execute`]):
//! 1. waits until the launcher process has exited (by process id and start time, up to
//!    [`HelperJob::wait_secs`]), then takes the lock of the data folder, so no launcher starts
//!    in the middle of the replacement;
//! 2. carries out the plan ([`crate::apply`]: the size and SHA-256 are checked again, a portable
//!    folder renames each old item away before the new one moves in, a failure moves everything
//!    back);
//! 3. on success writes the "just updated" marker and removes the download; on failure writes
//!    the failure record (`last-error.json`) that the next start reports;
//! 4. starts the launcher again when the job asks for it — the new version, or the old one after
//!    a failure. When the launcher does not exit in time nothing is replaced and nothing is
//!    started.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
use vl_update::{InstallMethod, Replacement, Restart, UpdatePlan, Version};

use crate::{StagedUpdate, UpdateDir, UpdaterError, verify_file};

/// The command-line argument that starts the launcher binary as the update helper; the path of
/// the job file follows it.
pub const HELPER_ARG: &str = "--vl-update-helper";
/// The job file inside the helper folder.
pub const JOB_FILE: &str = "job.json";
/// How long the helper waits for the launcher to exit by default.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(60);
/// The job format this helper reads.
const JOB_FORMAT: u32 = 1;
/// The largest job file read.
const MAX_JOB_BYTES: u64 = 64 * 1024;
/// How often the helper looks at the launcher process and the lock.
const POLL: Duration = Duration::from_millis(100);

/// The launcher process the helper waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentProcess {
    /// Its process id.
    pub pid: u32,
    /// Its start time in seconds since the Unix epoch, when known: a later process that reuses
    /// the id is not mistaken for it.
    pub start_time: Option<u64>,
}

impl ParentProcess {
    /// The running process.
    pub fn current() -> Self {
        Self::of(std::process::id())
    }

    /// Process `pid` with its start time, if it runs.
    pub fn of(pid: u32) -> Self {
        Self {
            pid,
            start_time: process(pid).map(|(start, _)| start),
        }
    }

    /// Whether the process still runs (a zombie counts as exited).
    pub fn is_running(&self) -> bool {
        process(self.pid).is_some_and(|(start, status)| {
            !matches!(status, ProcessStatus::Zombie | ProcessStatus::Dead)
                && self.start_time.is_none_or(|t| t == start)
        })
    }
}

/// The start time and status of process `pid`, if it exists.
fn process(pid: u32) -> Option<(u64, ProcessStatus)> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).map(|p| (p.start_time(), p.status()))
}

/// Everything the helper needs, written by the launcher before it exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperJob {
    /// The job format ([`JOB_FORMAT`]).
    pub format: u32,
    /// The launcher process to wait for.
    pub parent: ParentProcess,
    /// The data folder, locked while the files are replaced.
    pub data_dir: PathBuf,
    /// The update folder (`<data>/updates`).
    pub updates_dir: PathBuf,
    /// The version being replaced.
    pub from: Version,
    /// What to do.
    pub plan: UpdatePlan,
    /// The arguments to start the launcher with afterwards; `None` does not start it.
    pub restart: Option<Vec<String>>,
    /// How long to wait for the launcher to exit, in seconds.
    pub wait_secs: u64,
}

/// How a helper run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperOutcome {
    /// The update is installed.
    Installed,
    /// The replacement failed and was undone; the failure is recorded.
    Failed(String),
    /// The launcher did not exit (or kept its data folder locked) in time; nothing was replaced.
    LauncherRunning,
}

/// The job for installing `staged` over the launcher of version `from`, installed as `method`
/// and running as `exe`. Refused before the launcher exits when the install method is not
/// replaced automatically, when its replacement is not implemented yet, and when the download
/// no longer matches its size and SHA-256.
pub fn prepare(
    dir: &UpdateDir,
    staged: &StagedUpdate,
    from: &Version,
    method: &InstallMethod,
    exe: &Path,
    data_dir: &Path,
    restart: Option<Vec<String>>,
) -> Result<HelperJob, UpdaterError> {
    let plan = vl_update::plan(method, exe, &staged.version, &staged.artifact, &staged.file);
    match &plan.replacement {
        Replacement::Link => return Err(UpdaterError::NothingToReplace),
        Replacement::RunInstaller { .. } => {
            return Err(UpdaterError::NotImplemented("installing an MSI update"));
        }
        Replacement::CopyFromDmg { .. } if !cfg!(target_os = "macos") => {
            return Err(UpdaterError::NotImplemented(
                "installing a disk image outside macOS",
            ));
        }
        Replacement::Unpack { .. }
        | Replacement::RenameOver { .. }
        | Replacement::CopyFromDmg { .. } => {}
    }
    verify_file(&plan.file, plan.size, &plan.sha256)?;
    Ok(HelperJob {
        format: JOB_FORMAT,
        parent: ParentProcess::current(),
        data_dir: data_dir.to_path_buf(),
        updates_dir: dir.path().to_path_buf(),
        from: from.clone(),
        plan,
        restart,
        wait_secs: DEFAULT_WAIT.as_secs(),
    })
}

/// Writes `job` into the helper folder and starts the helper detached from this process: the
/// AppImage of an AppImage install, else a copy of `exe` in the helper folder. The caller exits
/// right after.
pub fn launch(
    dir: &UpdateDir,
    job: &HelperJob,
    method: &InstallMethod,
    exe: &Path,
) -> Result<(), UpdaterError> {
    let helper_dir = dir.helper_dir();
    // A helper left from an earlier update may still hold its copy; it is replaced if possible.
    let _ = fs::remove_dir_all(&helper_dir);
    fs::create_dir_all(&helper_dir).map_err(|e| UpdaterError::io(&helper_dir, &e))?;
    let job_path = helper_dir.join(JOB_FILE);
    let bytes =
        serde_json::to_vec_pretty(job).map_err(|e| UpdaterError::Io(format!("json: {e}")))?;
    vl_store::write_atomic(&job_path, &bytes).map_err(|e| UpdaterError::Io(e.to_string()))?;
    let program = match method {
        InstallMethod::AppImage { file } => file.clone(),
        _ => {
            let mut name = format!("vibelauncher-update-{}", std::process::id());
            if let Some(ext) = exe.extension().and_then(OsStr::to_str) {
                name = format!("{name}.{ext}");
            }
            let copy = helper_dir.join(name);
            fs::copy(exe, &copy).map_err(|e| UpdaterError::io(&copy, &e))?;
            copy
        }
    };
    let args = [HELPER_ARG.as_ref(), job_path.as_os_str()];
    spawn_detached(&program, &args, Some(&helper_dir)).map_err(|e| UpdaterError::io(&program, &e))
}

/// The helper's entry point: reads the job file, runs it and starts the launcher again. Returns
/// the process exit code: 0 installed, 1 failed or the launcher kept running, 2 unreadable job.
pub fn run(job_path: &Path) -> i32 {
    let Some(job) = read_job(job_path) else {
        return 2;
    };
    let outcome = execute(&job, &restart_launcher);
    let _ = fs::remove_file(job_path);
    match outcome {
        HelperOutcome::Installed => 0,
        HelperOutcome::Failed(_) | HelperOutcome::LauncherRunning => 1,
    }
}

/// Reads a job file; `None` when it is missing, too large, damaged or of another format.
pub fn read_job(path: &Path) -> Option<HelperJob> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_JOB_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_JOB_BYTES {
        return None;
    }
    let job: HelperJob = serde_json::from_slice(&bytes).ok()?;
    (job.format == JOB_FORMAT).then_some(job)
}

/// Runs `job` (see the module documentation) and starts the launcher with `restart`.
pub fn execute(
    job: &HelperJob,
    restart: &dyn Fn(&Restart, &[String]) -> io::Result<()>,
) -> HelperOutcome {
    let dir = UpdateDir::new(&job.updates_dir);
    let version = &job.plan.version;
    let deadline = Instant::now() + Duration::from_secs(job.wait_secs);
    let lock = wait_for_exit(&job.parent, deadline)
        .then(|| wait_for_lock(&job.data_dir, deadline))
        .flatten();
    let Some(lock) = lock else {
        let _ = dir.record_failure(
            version,
            &format!(
                "the launcher did not exit within {} s; the update was not installed",
                job.wait_secs
            ),
        );
        return HelperOutcome::LauncherRunning;
    };
    // The launcher refused to start the helper while a game ran, and it has exited since: no
    // game it started can be running now.
    let outcome = match crate::apply(&job.plan, &|| false) {
        Ok(()) => {
            let _ = dir.take_failure();
            let _ = dir.mark_updated(&job.from, version);
            dir.clear_staged();
            HelperOutcome::Installed
        }
        Err(e) => {
            let message = e.to_string();
            let _ = dir.record_failure(version, &message);
            HelperOutcome::Failed(message)
        }
    };
    drop(lock);
    if let (Some(args), Some(target)) = (&job.restart, &job.plan.restart) {
        let _ = restart(target, args);
    }
    outcome
}

/// Waits until `parent` has exited; `false` when it still runs at `deadline`.
pub fn wait_for_exit(parent: &ParentProcess, deadline: Instant) -> bool {
    loop {
        if !parent.is_running() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Takes the lock of the data folder, waiting until `deadline`. A folder that cannot be locked
/// for another reason (it is gone) is not waited for: the replacement goes ahead.
fn wait_for_lock(data_dir: &Path, deadline: Instant) -> Option<Option<vl_store::DirLock>> {
    loop {
        match vl_store::DirLock::acquire(data_dir) {
            Ok(lock) => return Some(Some(lock)),
            Err(vl_store::StoreError::Locked { .. }) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Err(vl_store::StoreError::Locked { .. }) => return None,
            Err(_) => return Some(None),
        }
    }
}

/// Starts the launcher again: an executable with `args` in its own folder, or a macOS bundle
/// with `open -n <bundle> --args <args>`.
pub fn restart_launcher(target: &Restart, args: &[String]) -> io::Result<()> {
    match target {
        Restart::Executable(exe) => {
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            spawn_detached(exe, &args, exe.parent())
        }
        Restart::Bundle(bundle) => {
            let mut all: Vec<&OsStr> = vec!["-n".as_ref(), bundle.as_os_str(), "--args".as_ref()];
            all.extend(args.iter().map(OsStr::new));
            spawn_detached(Path::new("open"), &all, None)
        }
    }
}

/// Starts `program` without a console or standard streams, in a process group of its own, so it
/// outlives this process.
fn spawn_detached(program: &Path, args: &[&OsStr], cwd: Option<&Path>) -> io::Result<()> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map(|_| ())
}
