//! Self-update commands, served by `vl-updater`.
//!
//! - `Status` reads the update folder `<data>/updates/`: the newest verified version, the
//!   download waiting to be installed, and what the start of this process found — the "just
//!   updated" marker and the record of a failed installation (both read once, at start).
//! - `Check` fetches and verifies the release manifest on a worker thread; the offer is kept for
//!   `Status` until the launcher exits.
//! - `Download` runs a `DownloadUpdate` task: it checks again and downloads the release file with
//!   byte progress and cancellation, then records it as staged.
//! - `Apply` refuses while a game runs (the launch service's own tracking) or another task runs,
//!   prepares the helper job (the replacement is supported, the download still matches), starts
//!   the update helper and closes the backend: every later command but `Hello`, `Status` and the
//!   task commands answers `Conflict`, so nothing starts while the front end exits.
//!
//! A build without a release key answers `Check` and `Download` with `Unsupported`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vl_core::platform::Platform;
use vl_core::self_update::{
    JustUpdated, SelfUpdateCommand, SelfUpdateStatus, UpdateFailure, UpdateOffer,
};
use vl_core::task::TaskKind;
use vl_core::{Command, Error, Replier, Response, Result};
use vl_net::{Client, Control, NetConfig};
use vl_store::Store;
use vl_tasks::TaskSpec;
use vl_update::{InstallMethod, ManualReason, Offer, UpdateError, UpdateKey, Version};
use vl_updater::{GameGuard, UpdateDir, Updater, helper};

use crate::Backend;

const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The self-update state of the running launcher.
#[derive(Clone)]
pub(crate) struct Service {
    inner: Arc<Inner>,
}

struct Inner {
    dir: UpdateDir,
    data_dir: PathBuf,
    client: Client,
    /// The release key; `None` in a build without one.
    key: Option<UpdateKey>,
    manifest_url: String,
    current: Version,
    install: InstallMethod,
    exe: Option<PathBuf>,
    offer: Mutex<Option<Offer>>,
    just_updated: Option<vl_updater::JustUpdated>,
    last_error: Option<vl_updater::UpdateFailure>,
    /// The helper has started; the launcher is about to exit.
    closing: AtomicBool,
}

impl Service {
    /// Opens the update folder of `store` and reads what the last update left: the "just
    /// updated" marker, the failure record, the helper folder and, for a portable install or an
    /// application bundle, the replaced files.
    pub(crate) fn new(store: &Store) -> Result<Self> {
        let current = Version::parse(LAUNCHER_VERSION)
            .ok_or_else(|| Error::internal(format!("bad launcher version {LAUNCHER_VERSION}")))?;
        let client = Client::new(NetConfig::new(LAUNCHER_VERSION))?;
        let key = UpdateKey::release().ok();
        let service = Self::with(
            store.data_dir().to_path_buf(),
            client,
            key,
            vl_update::MANIFEST_URL.into(),
            current,
            vl_updater::current_install(),
        );
        let inner = &service.inner;
        let _ = std::fs::remove_dir_all(inner.dir.helper_dir());
        match &inner.install {
            InstallMethod::Portable { dir } => vl_updater::clean_portable(dir),
            InstallMethod::MacApp { bundle } => vl_updater::clean_app_bundle(bundle),
            _ => {}
        }
        Ok(service)
    }

    fn with(
        data_dir: PathBuf,
        client: Client,
        key: Option<UpdateKey>,
        manifest_url: String,
        current: Version,
        install: InstallMethod,
    ) -> Self {
        let dir = UpdateDir::new(data_dir.join("updates"));
        let just_updated = dir.take_just_updated(&current);
        let last_error = dir.take_failure();
        Self {
            inner: Arc::new(Inner {
                dir,
                data_dir,
                client,
                key,
                manifest_url,
                current,
                install,
                exe: vl_updater::current_exe(),
                offer: Mutex::default(),
                just_updated,
                last_error,
                closing: AtomicBool::new(false),
            }),
        }
    }
}

impl Inner {
    fn updater(&self) -> Result<Updater> {
        let key = self.key.clone().ok_or(UpdateError::NoReleaseKey)?;
        Ok(Updater::with_key(
            self.client.clone(),
            key,
            self.manifest_url.clone(),
            self.dir.clone(),
        ))
    }

    fn check(&self, control: &Control) -> Result<Offer> {
        let offer =
            self.updater()?
                .check(&self.current, Platform::current(), &self.install, control)?;
        *self.offer.lock().unwrap_or_else(PoisonError::into_inner) = Some(offer.clone());
        Ok(offer)
    }

    fn status(&self) -> SelfUpdateStatus {
        let offer = self
            .offer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(offer_of);
        SelfUpdateStatus {
            current: self.current.to_string(),
            install: method_name(&self.install).into(),
            enabled: self.key.is_some(),
            newest_seen: self.dir.highest_seen().map(|v| v.to_string()),
            offer,
            staged: self
                .dir
                .staged()
                .filter(|s| s.version > self.current)
                .map(|s| s.version.to_string()),
            last_error: self.last_error.as_ref().map(|f| UpdateFailure {
                version: f.version.to_string(),
                message: f.message.clone(),
                at_unix: f.at_unix,
            }),
            just_updated: self.just_updated.as_ref().map(|j| JustUpdated {
                from: j.from.to_string(),
                to: j.to.to_string(),
            }),
        }
    }

    /// Starts the helper for the staged download (see the module documentation).
    fn apply(
        &self,
        guard: &dyn GameGuard,
        tasks_running: bool,
        restart: Option<Vec<String>>,
    ) -> Result<()> {
        if guard.game_running() {
            return Err(vl_updater::UpdaterError::GameRunning.into());
        }
        if tasks_running {
            return Err(Error::conflict(
                "tasks are running; install the update when they have ended",
            ));
        }
        let staged = self
            .dir
            .staged()
            .ok_or_else(|| Error::not_found("no update is downloaded"))?;
        if staged.version <= self.current {
            self.dir.clear_staged();
            return Err(Error::not_found(format!(
                "the downloaded version {} is not newer than {}",
                staged.version, self.current
            )));
        }
        let exe = self
            .exe
            .clone()
            .ok_or_else(|| Error::internal("the launcher executable is unknown"))?;
        let job = helper::prepare(
            &self.dir,
            &staged,
            &self.current,
            &self.install,
            &exe,
            &self.data_dir,
            restart,
        )?;
        helper::launch(&self.dir, &job, &self.install, &exe)?;
        self.closing.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Refuses every command but `Hello`, `Status` and the task commands once the update helper has
/// started.
pub(crate) fn admit(backend: &Backend, command: &Command) -> Result<()> {
    if !backend.self_update.inner.closing.load(Ordering::SeqCst) {
        return Ok(());
    }
    match command {
        Command::Hello | Command::Tasks(_) | Command::SelfUpdate(SelfUpdateCommand::Status) => {
            Ok(())
        }
        _ => Err(Error::conflict(
            "the launcher is closing to install an update",
        )),
    }
}

pub(crate) fn handle(backend: &Backend, command: SelfUpdateCommand, replier: Replier) {
    let inner = Arc::clone(&backend.self_update.inner);
    match command {
        SelfUpdateCommand::Status => replier.send(Ok(Response::SelfUpdate(inner.status()))),
        SelfUpdateCommand::Check => spawn(replier, move |replier| {
            let result = inner
                .check(&Control::default())
                .map(|_| Response::SelfUpdate(inner.status()));
            replier.send(result);
        }),
        SelfUpdateCommand::Download => {
            if inner.key.is_none() {
                replier.send(Err(UpdateError::NoReleaseKey.into()));
                return;
            }
            let handle = backend.tasks().spawn(
                TaskSpec::new(TaskKind::DownloadUpdate).stage("checking"),
                move |ctx| {
                    crate::launch::with_job(ctx, |job| {
                        let control = crate::java::job_control(job);
                        download(&inner, &control)
                    })
                },
            );
            replier.send(Ok(Response::Task(handle.id())));
        }
        SelfUpdateCommand::Apply { restart } => {
            let launch = backend.launch.clone();
            let tasks_running = !backend.tasks().snapshots().is_empty();
            spawn(replier, move |replier| {
                let guard = move || launch.any_active();
                let result = inner
                    .apply(&guard, tasks_running, restart)
                    .map(|()| Response::Done);
                replier.send(result);
            });
        }
        other => crate::not_yet(replier, "self-update", &other),
    }
}

/// Checks, downloads the offered release file and records it.
fn download(inner: &Inner, control: &Control) -> Result<()> {
    let offer = inner.check(control)?;
    let (version, artifact) = match offer {
        Offer::Update {
            version, artifact, ..
        } => (version, artifact),
        Offer::UpToDate => return Err(Error::conflict("the launcher is up to date")),
        Offer::Manual { version, page, .. } => {
            return Err(Error::unsupported(format!(
                "{version} is installed by hand from {page}"
            )));
        }
    };
    let updater = inner.updater()?;
    let file = updater.stage(&version, &artifact, control)?;
    inner.dir.record_staged(&version, &artifact, &file)?;
    Ok(())
}

fn offer_of(offer: &Offer) -> UpdateOffer {
    match offer {
        Offer::UpToDate => UpdateOffer::UpToDate,
        Offer::Update {
            version,
            date,
            artifact,
        } => UpdateOffer::Update {
            version: version.to_string(),
            date: date.clone(),
            size: artifact.size,
        },
        Offer::Manual {
            version,
            page,
            reason,
        } => UpdateOffer::Manual {
            version: version.to_string(),
            page: page.clone(),
            reason: match reason {
                ManualReason::PackageManager => "package_manager",
                ManualReason::UnknownInstall => "unknown_install",
                ManualReason::TooOld { .. } => "too_old",
                ManualReason::NoArtifact => "no_artifact",
            }
            .into(),
        },
    }
}

fn method_name(method: &InstallMethod) -> &'static str {
    match method {
        InstallMethod::Msi => "msi",
        InstallMethod::Portable { .. } => "portable",
        InstallMethod::MacApp { .. } => "mac_app",
        InstallMethod::AppImage { .. } => "app_image",
        InstallMethod::Deb => "deb",
        InstallMethod::Rpm => "rpm",
        InstallMethod::Unknown => "unknown",
    }
}

/// Runs `work` on its own thread so a network check never blocks the caller of `call`.
fn spawn(replier: Replier, work: impl FnOnce(Replier) + Send + 'static) {
    // If the thread cannot start the replier is dropped, and `wait` reports that.
    let _ = thread::Builder::new()
        .name("self-update".into())
        .spawn(move || work(replier));
}

/// The entry point of the update helper mode of a launcher binary: when the process was started
/// as `<exe> --vl-update-helper <job file>`, runs the helper and returns its exit code; otherwise
/// `None` and the binary starts as usual. Front ends call it first thing in `main`.
pub fn run_helper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != helper::HELPER_ARG {
        return None;
    }
    let job = args.next()?;
    Some(helper::run(std::path::Path::new(&job)))
}

#[cfg(test)]
#[path = "self_update_tests.rs"]
mod tests;
