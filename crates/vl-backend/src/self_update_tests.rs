//! Self-update through the facade, on a fake server signed with a key made for the test.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use minisign::KeyPair;
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{ErrorKind, Event, Facade, TaskId};
use vl_net::{HttpRequest, HttpResponse, HttpTransport, NetError};

use super::*;
use crate::BackendConfig;

const MANIFEST_URL: &str = "https://updates.example.test/latest.json";
const FILE_URL: &str = "https://downloads.example.test/vibelauncher.zip";

/// Serves fixed bytes by URL; anything else is `404`.
struct Files(HashMap<String, Vec<u8>>);

impl HttpTransport for Files {
    fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
        Ok(match self.0.get(&request.url) {
            Some(b) => HttpResponse {
                status: 200,
                headers: vec![("content-length".into(), b.len().to_string())],
                body: Box::new(Cursor::new(b.clone())),
            },
            None => HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Box::new(Cursor::new(Vec::new())),
            },
        })
    }
}

struct World {
    dir: PathBuf,
    backend: Backend,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A backend whose self-update trusts a test key and reads a server that publishes `version`
/// (with a portable file for this platform), for a launcher of version 1.0.0 installed as
/// `install`. Without `signed`, the build has no release key.
fn world(name: &str, version: &str, signed: bool, install: InstallMethod) -> World {
    let dir = std::env::temp_dir().join(format!("vl-backend-update-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut backend = Backend::new(BackendConfig {
        data_dir: Some(dir.clone()),
    })
    .unwrap();
    let file = b"new launcher".to_vec();
    let platform = Platform::current();
    let manifest = format!(
        r#"{{"schema":1,"channel":"stable","version":"{version}","date":"2026-10-01","minimum_version":"0.1.0","page":"https://example.test/releases","artifacts":[{{"os":"{}","arch":"{}","kind":"portable","url":"{FILE_URL}","sha256":"{}","size":{}}}]}}"#,
        serde_json::to_value(platform.os).unwrap().as_str().unwrap(),
        serde_json::to_value(platform.arch)
            .unwrap()
            .as_str()
            .unwrap(),
        vl_net::sha256_hex(&file),
        file.len()
    );
    let pair = KeyPair::generate_unencrypted_keypair().unwrap();
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        Cursor::new(manifest.as_bytes()),
        None,
        None,
    )
    .unwrap()
    .into_string();
    let files = Files(HashMap::from([
        (MANIFEST_URL.to_owned(), manifest.into_bytes()),
        (format!("{MANIFEST_URL}.minisig"), signature.into_bytes()),
        (FILE_URL.to_owned(), file),
    ]));
    let mut config = NetConfig::new("0.0.0");
    config.retry.attempts = 1;
    let client = Client::with_transport(config, Arc::new(files));
    let key = signed.then(|| UpdateKey::from_text(&pair.pk.to_base64()).unwrap());
    backend.self_update = Service::with(
        dir.clone(),
        client,
        key,
        MANIFEST_URL.into(),
        Version::new(1, 0, 0),
        install,
    );
    World { dir, backend }
}

fn portable(world: &World) -> InstallMethod {
    InstallMethod::Portable {
        dir: world.dir.join("app"),
    }
}

fn status(backend: &Backend) -> SelfUpdateStatus {
    match backend
        .call(Command::SelfUpdate(SelfUpdateCommand::Status))
        .wait()
        .unwrap()
    {
        Response::SelfUpdate(s) => s,
        other => panic!("{other:?}"),
    }
}

fn finish(events: &vl_core::EventStream, task: TaskId) -> TaskOutcome {
    loop {
        match events.next_timeout(Duration::from_secs(30)) {
            Some(Event::Task(TaskEvent::Finished { id, outcome })) if id == task => {
                return outcome;
            }
            Some(_) => {}
            None => panic!("the task did not finish"),
        }
    }
}

#[test]
fn a_build_without_a_release_key_refuses_checks_and_downloads() {
    let w = world("nokey", "1.1.0", false, InstallMethod::Unknown);
    let s = status(&w.backend);
    assert_eq!(s.current, "1.0.0");
    assert!(!s.enabled);
    assert_eq!(s.offer, None);
    assert_eq!(s.staged, None);
    for command in [SelfUpdateCommand::Check, SelfUpdateCommand::Download] {
        let err = w
            .backend
            .call(Command::SelfUpdate(command))
            .wait()
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported, "{err}");
    }
}

#[test]
fn check_download_and_status() {
    let mut w = world("flow", "1.1.0", true, InstallMethod::Unknown);
    let install = portable(&w);
    w.backend.self_update = Service::with(
        w.dir.clone(),
        w.backend.self_update.inner.client.clone(),
        w.backend.self_update.inner.key.clone(),
        MANIFEST_URL.into(),
        Version::new(1, 0, 0),
        install,
    );
    let s = match w
        .backend
        .call(Command::SelfUpdate(SelfUpdateCommand::Check))
        .wait()
        .unwrap()
    {
        Response::SelfUpdate(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(s.enabled);
    assert_eq!(s.install, "portable");
    assert_eq!(s.newest_seen.as_deref(), Some("1.1.0"));
    assert_eq!(
        s.offer,
        Some(UpdateOffer::Update {
            version: "1.1.0".into(),
            date: "2026-10-01".into(),
            size: 12,
        })
    );
    assert_eq!(s.staged, None);

    let events = w.backend.subscribe();
    let task = match w
        .backend
        .call(Command::SelfUpdate(SelfUpdateCommand::Download))
        .wait()
        .unwrap()
    {
        Response::Task(id) => id,
        other => panic!("{other:?}"),
    };
    assert_eq!(finish(&events, task), TaskOutcome::Succeeded);
    assert_eq!(status(&w.backend).staged.as_deref(), Some("1.1.0"));
}

#[test]
fn a_package_install_gets_the_release_page() {
    let w = world("manual", "1.1.0", true, InstallMethod::Deb);
    let events = w.backend.subscribe();
    let task = match w
        .backend
        .call(Command::SelfUpdate(SelfUpdateCommand::Download))
        .wait()
        .unwrap()
    {
        Response::Task(id) => id,
        other => panic!("{other:?}"),
    };
    let TaskOutcome::Failed(err) = finish(&events, task) else {
        panic!("the download must fail");
    };
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("https://example.test/releases"));
    assert!(matches!(
        status(&w.backend).offer,
        Some(UpdateOffer::Manual { reason, .. }) if reason == "package_manager"
    ));
}

#[test]
fn apply_is_refused_while_a_game_or_a_task_runs_and_without_a_download() {
    let w = world("apply", "1.1.0", true, InstallMethod::Deb);
    let inner = &w.backend.self_update.inner;
    let err = inner.apply(&|| true, false, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.message.contains("game"), "{err}");
    let err = inner.apply(&|| false, true, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    let err = inner.apply(&|| false, false, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);

    // A download for a package install is refused before the launcher exits.
    let file = inner.dir.staging(&Version::new(1, 1, 0)).join("f.zip");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"new launcher").unwrap();
    let artifact = vl_update::Artifact {
        platform: Platform::current(),
        kind: vl_update::ArtifactKind::Portable,
        url: FILE_URL.into(),
        sha256: vl_net::sha256_hex(b"new launcher"),
        size: 12,
    };
    inner
        .dir
        .record_staged(&Version::new(1, 1, 0), &artifact, &file)
        .unwrap();
    let err = w
        .backend
        .call(Command::SelfUpdate(SelfUpdateCommand::Apply {
            restart: None,
        }))
        .wait()
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported, "{err}");
    assert!(!inner.closing.load(Ordering::SeqCst));
    // An older download is dropped.
    inner
        .dir
        .record_staged(
            &Version::new(1, 0, 0),
            &artifact,
            &inner.dir.staging(&Version::new(1, 0, 0)).join("f.zip"),
        )
        .unwrap();
    assert_eq!(status(&w.backend).staged, None);
}

#[test]
fn a_closing_backend_refuses_new_work() {
    let w = world("closing", "1.1.0", true, InstallMethod::Unknown);
    w.backend
        .self_update
        .inner
        .closing
        .store(true, Ordering::SeqCst);
    let err = w
        .backend
        .call(Command::Instances(vl_core::instance::InstanceCommand::List))
        .wait()
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(w.backend.call(Command::Hello).wait().is_ok());
    status(&w.backend);
}

#[test]
fn the_start_reports_the_last_update_and_failure_once() {
    let dir = std::env::temp_dir().join(format!("vl-backend-update-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let updates = UpdateDir::new(dir.join("updates"));
    updates
        .mark_updated(&Version::new(0, 9, 0), &Version::new(1, 0, 0))
        .unwrap();
    updates
        .record_failure(&Version::new(1, 1, 0), "disk full")
        .unwrap();
    let client = Client::new(NetConfig::new("0.0.0")).unwrap();
    let service = Service::with(
        dir.clone(),
        client.clone(),
        None,
        MANIFEST_URL.into(),
        Version::new(1, 0, 0),
        InstallMethod::Unknown,
    );
    let s = service.inner.status();
    assert_eq!(
        s.just_updated,
        Some(JustUpdated {
            from: "0.9.0".into(),
            to: "1.0.0".into()
        })
    );
    let failure = s.last_error.unwrap();
    assert_eq!(failure.version, "1.1.0");
    assert_eq!(failure.message, "disk full");
    // Both are read once: the next start does not report them again.
    let again = Service::with(
        dir.clone(),
        client,
        None,
        MANIFEST_URL.into(),
        Version::new(1, 0, 0),
        InstallMethod::Unknown,
    );
    assert_eq!(again.inner.status().just_updated, None);
    assert_eq!(again.inner.status().last_error, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_helper_mode_is_not_requested_by_the_test_binary() {
    assert_eq!(run_helper_if_requested(), None);
}
