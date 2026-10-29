//! End-to-end tests on a fake server and temporary folders.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vl_core::platform::{Arch, Os, Platform};
use vl_net::{CancelToken, Client, Control, NetConfig, NetError};
use vl_update::{
    ArtifactKind, InstallMethod, Offer, RELEASE_PUBLIC_KEY, Replacement, UpdateError, Version, plan,
};

use crate::testkit::{Dir, Files, TestSigner, sha256, zip_of};
use crate::{
    NEW_DIR, OLD_DIR, UpdateDir, Updater, UpdaterError, apply, clean_portable, replace_appimage,
};

const MANIFEST_URL: &str = "https://updates.example.test/stable/latest.json";
const SIG_URL: &str = "https://updates.example.test/stable/latest.json.minisig";
const FILE_URL: &str = "https://downloads.example.test/vibelauncher.zip";
const WIN: Platform = Platform {
    os: Os::Windows,
    arch: Arch::X86_64,
};

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// A manifest of `version` with one Windows portable artifact of `file`.
fn manifest(version: &str, file: &[u8]) -> String {
    format!(
        r#"{{"schema":1,"channel":"stable","version":"{version}","date":"2026-10-01","minimum_version":"0.1.0","page":"https://example.test/releases","artifacts":[{{"os":"windows","arch":"x86_64","kind":"portable","url":"{FILE_URL}","sha256":"{}","size":{}}}]}}"#,
        sha256(file),
        file.len()
    )
}

struct World {
    dir: Dir,
    files: Arc<Files>,
    signer: TestSigner,
    updater: Updater,
}

impl World {
    fn new() -> Self {
        let dir = Dir::new();
        let files = Arc::new(Files::default());
        let mut config = NetConfig::new("0.0.0");
        config.retry.attempts = 1;
        let client = Client::with_transport(config, files.clone());
        let signer = TestSigner::new();
        let updater = Updater::with_key(
            client,
            signer.key(),
            MANIFEST_URL,
            UpdateDir::new(dir.path().join("updates")),
        );
        Self {
            dir,
            files,
            signer,
            updater,
        }
    }

    /// Publishes a signed manifest of `version` for `file`.
    fn publish(&self, version: &str, file: &[u8]) {
        let json = manifest(version, file);
        self.files.put(MANIFEST_URL, json.clone());
        self.files.put(SIG_URL, self.signer.sign(json.as_bytes()));
        self.files.put(FILE_URL, file.to_vec());
    }

    fn portable(&self) -> InstallMethod {
        InstallMethod::Portable {
            dir: self.dir.path().join("app"),
        }
    }

    fn check(&self, current: &str) -> Result<Offer, UpdaterError> {
        self.updater
            .check(&v(current), WIN, &self.portable(), &Control::default())
    }
}

#[test]
fn release_updater_fails_closed_without_a_release_key() {
    if RELEASE_PUBLIC_KEY.is_some() {
        return;
    }
    let client = Client::with_transport(NetConfig::new("0.0.0"), Arc::new(Files::default()));
    let result = Updater::release(client, MANIFEST_URL, UpdateDir::new("unused"));
    assert!(matches!(
        result,
        Err(UpdaterError::Update(UpdateError::NoReleaseKey))
    ));
    let error: vl_core::Error = UpdaterError::Update(UpdateError::NoReleaseKey).into();
    assert_eq!(error.kind, vl_core::ErrorKind::Unsupported);
}

#[test]
fn check_offers_a_signed_update_and_remembers_its_version() {
    let w = World::new();
    w.publish("1.4.0", b"new launcher");
    let offer = w.check("1.3.0").unwrap();
    let Offer::Update {
        version, artifact, ..
    } = offer
    else {
        panic!("{offer:?}");
    };
    assert_eq!(version, v("1.4.0"));
    assert_eq!(artifact.kind, ArtifactKind::Portable);
    assert_eq!(w.updater.dir().highest_seen(), Some(v("1.4.0")));
    assert_eq!(w.check("1.4.0").unwrap(), Offer::UpToDate);
}

#[test]
fn check_rejects_a_tampered_manifest_and_a_wrong_signature() {
    let w = World::new();
    let json = manifest("1.4.0", b"new launcher");
    w.files.put(SIG_URL, w.signer.sign(json.as_bytes()));
    w.files
        .put(MANIFEST_URL, json.replace(FILE_URL, "https://evil.test/x"));
    assert!(matches!(
        w.check("1.3.0"),
        Err(UpdaterError::Update(UpdateError::BadSignature(_)))
    ));
    // Signed by a key the launcher does not trust.
    w.files.put(MANIFEST_URL, json.clone());
    w.files
        .put(SIG_URL, TestSigner::new().sign(json.as_bytes()));
    assert!(matches!(
        w.check("1.3.0"),
        Err(UpdaterError::Update(UpdateError::BadSignature(_)))
    ));
    // No signature at all.
    let w = World::new();
    w.files.put(MANIFEST_URL, json);
    assert!(matches!(
        w.check("1.3.0"),
        Err(UpdaterError::Net(NetError::Status { status: 404, .. }))
    ));
    assert_eq!(w.updater.dir().highest_seen(), None);
}

#[test]
fn check_refuses_a_replayed_older_manifest() {
    let w = World::new();
    w.publish("1.4.0", b"new launcher");
    w.check("1.0.0").unwrap();
    // A mirror serves an older, validly signed manifest.
    w.publish("1.3.0", b"older launcher");
    assert!(matches!(
        w.check("1.0.0"),
        Err(UpdaterError::Update(UpdateError::Rollback { .. }))
    ));
    assert_eq!(w.updater.dir().highest_seen(), Some(v("1.4.0")));
    // Older than the running version.
    let w = World::new();
    w.publish("1.3.0", b"older launcher");
    assert!(matches!(
        w.check("1.4.0"),
        Err(UpdaterError::Update(UpdateError::Rollback { .. }))
    ));
}

#[test]
fn check_caps_the_manifest_size_and_needs_https() {
    let w = World::new();
    w.publish("1.4.0", b"new launcher");
    let huge = " ".repeat(vl_update::manifest::MAX_MANIFEST_BYTES + 1);
    w.files.put(MANIFEST_URL, huge);
    assert!(matches!(
        w.check("1.3.0"),
        Err(UpdaterError::Net(NetError::TooLarge { .. }))
    ));

    let client = Client::with_transport(NetConfig::new("0.0.0"), Arc::new(Files::default()));
    let plain = Updater::with_key(
        client,
        w.signer.key(),
        "http://updates.example.test/latest.json",
        UpdateDir::new(w.dir.path()),
    );
    assert!(matches!(
        plain.check(
            &v("1.0.0"),
            WIN,
            &InstallMethod::Unknown,
            &Control::default()
        ),
        Err(UpdaterError::Net(NetError::InvalidUrl(_)))
    ));
}

#[test]
fn stage_downloads_and_verifies_with_progress() {
    let w = World::new();
    let body = vec![7u8; 100_000];
    w.publish("1.4.0", &body);
    let Offer::Update {
        version, artifact, ..
    } = w.check("1.3.0").unwrap()
    else {
        panic!()
    };
    // A download of another version is cleared.
    let old = w.updater.dir().staging(&v("1.2.0"));
    fs::create_dir_all(&old).unwrap();
    let seen = Arc::new(AtomicU64::new(0));
    let progress = seen.clone();
    let control = Control::default().on_progress(move |p| {
        progress.store(p.done, Ordering::SeqCst);
        assert_eq!(p.total, Some(100_000));
    });
    let path = w.updater.stage(&version, &artifact, &control).unwrap();
    assert_eq!(fs::read(&path).unwrap(), body);
    assert_eq!(
        path,
        w.updater
            .dir()
            .staging(&version)
            .join("vibelauncher-1.4.0.zip")
    );
    assert_eq!(seen.load(Ordering::SeqCst), 100_000);
    assert!(!old.exists());
}

#[test]
fn stage_rejects_a_swapped_file_and_stops_when_cancelled() {
    let w = World::new();
    w.publish("1.4.0", b"the real file");
    let Offer::Update {
        version, artifact, ..
    } = w.check("1.3.0").unwrap()
    else {
        panic!()
    };
    // Same size, other bytes.
    w.files.put(FILE_URL, b"the fake file".to_vec());
    let result = w.updater.stage(&version, &artifact, &Control::default());
    assert!(matches!(
        result,
        Err(UpdaterError::Net(NetError::HashMismatch { .. }))
    ));
    let dest = w
        .updater
        .dir()
        .staging(&version)
        .join("vibelauncher-1.4.0.zip");
    assert!(!dest.exists());

    let cancel = CancelToken::new();
    cancel.cancel();
    let result = w
        .updater
        .stage(&version, &artifact, &Control::with_cancel(cancel));
    assert!(matches!(
        result,
        Err(UpdaterError::Net(NetError::Cancelled))
    ));
}

/// A portable install: the executable, a library folder, the user's data and the marker.
fn portable_install(dir: &Path) {
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::create_dir_all(dir.join("data/instances")).unwrap();
    fs::write(dir.join("vibelauncher.exe"), b"old exe").unwrap();
    fs::write(dir.join("lib/old.dll"), b"old lib").unwrap();
    fs::write(dir.join("data/instances/world.txt"), b"user data").unwrap();
    fs::write(dir.join(vl_store::PORTABLE_MARKER), b"").unwrap();
}

fn read(path: impl AsRef<Path>) -> Vec<u8> {
    fs::read(path).unwrap()
}

#[test]
fn portable_update_end_to_end() {
    let w = World::new();
    let app = w.dir.path().join("app");
    portable_install(&app);
    let archive = zip_of(&[
        ("vibelauncher.exe", b"new exe"),
        ("lib/", b""),
        ("lib/new.dll", b"new lib"),
        ("README.txt", b"readme"),
    ]);
    w.publish("1.4.0", &archive);
    let Offer::Update {
        version, artifact, ..
    } = w.check("1.3.0").unwrap()
    else {
        panic!()
    };
    let staged = w
        .updater
        .stage(&version, &artifact, &Control::default())
        .unwrap();
    let exe = app.join("vibelauncher.exe");
    let plan = plan(&w.portable(), &exe, &version, &artifact, &staged);
    assert!(matches!(plan.replacement, Replacement::Unpack { .. }));

    // Not while a game runs.
    let running = || true;
    assert_eq!(apply(&plan, &running), Err(UpdaterError::GameRunning));
    assert_eq!(read(&exe), b"old exe");

    apply(&plan, &|| false).unwrap();
    assert_eq!(read(&exe), b"new exe");
    assert_eq!(read(app.join("lib/new.dll")), b"new lib");
    assert!(!app.join("lib/old.dll").exists());
    assert_eq!(read(app.join("README.txt")), b"readme");
    // The user's data and the portable marker are untouched.
    assert_eq!(read(app.join("data/instances/world.txt")), b"user data");
    assert!(app.join(vl_store::PORTABLE_MARKER).exists());
    // The replaced files wait in the old folder until the next start cleans up.
    assert_eq!(read(app.join(OLD_DIR).join("vibelauncher.exe")), b"old exe");
    assert!(!app.join(NEW_DIR).exists());
    w.updater.dir().mark_updated(&v("1.3.0"), &version).unwrap();
    clean_portable(&app);
    assert!(!app.join(OLD_DIR).exists());
    assert!(w.updater.dir().take_just_updated(&version).is_some());
}

/// The downloadable portable archive carries the portable marker: an update from it keeps the
/// folder's marker as it is and leaves a folder without one without.
#[test]
fn the_portable_marker_in_the_archive_is_skipped() {
    let archive_bytes = zip_of(&[
        ("vibelauncher.exe", b"new exe"),
        ("vibelauncher.portable", b"from the archive"),
    ]);
    for has_marker in [true, false] {
        let dir = Dir::new();
        let app = dir.path().join("app");
        portable_install(&app);
        let marker = app.join(vl_store::PORTABLE_MARKER);
        if !has_marker {
            fs::remove_file(&marker).unwrap();
        }
        let archive = dir.path().join("new.zip");
        fs::write(&archive, &archive_bytes).unwrap();
        let names = crate::replace_portable(&archive, &app).unwrap();
        assert_eq!(names, ["vibelauncher.exe"]);
        assert_eq!(read(app.join("vibelauncher.exe")), b"new exe");
        assert_eq!(marker.exists(), has_marker);
        if has_marker {
            assert_eq!(read(&marker), b"");
        }
    }
}

#[test]
fn apply_rechecks_the_staged_file() {
    let w = World::new();
    let app = w.dir.path().join("app");
    portable_install(&app);
    let archive = zip_of(&[("vibelauncher.exe", b"new exe")]);
    w.publish("1.4.0", &archive);
    let Offer::Update {
        version, artifact, ..
    } = w.check("1.3.0").unwrap()
    else {
        panic!()
    };
    let staged = w
        .updater
        .stage(&version, &artifact, &Control::default())
        .unwrap();
    let plan = plan(
        &w.portable(),
        &app.join("vibelauncher.exe"),
        &version,
        &artifact,
        &staged,
    );
    // The staged file was changed after the download.
    let evil = zip_of(&[("vibelauncher.exe", b"evil ex")]);
    fs::write(&staged, &evil).unwrap();
    assert!(matches!(
        apply(&plan, &|| false),
        Err(UpdaterError::Integrity(_))
    ));
    fs::write(&staged, b"short").unwrap();
    assert!(matches!(
        apply(&plan, &|| false),
        Err(UpdaterError::Integrity(_))
    ));
    assert_eq!(read(app.join("vibelauncher.exe")), b"old exe");
}

#[test]
fn unsafe_portable_archives_change_nothing() {
    let cases: Vec<Vec<u8>> = vec![
        zip_of(&[("vibelauncher.exe", b"x"), ("../evil.exe", b"x")]),
        zip_of(&[
            ("vibelauncher.exe", b"x"),
            ("data/instances/world.txt", b"x"),
        ]),
        zip_of(&[
            ("vibelauncher.portable/", b""),
            ("vibelauncher.portable/a", b"x"),
        ]),
        zip_of(&[("vibelauncher.portable", b"")]),
        zip_of(&[("vibelauncher.install", b"msi")]),
        zip_of(&[("sub/a:b", b"x")]),
        zip_of(&[("a\\..\\b", b"x")]),
        zip_of(&[]),
        b"not a zip".to_vec(),
    ];
    for archive_bytes in cases {
        let dir = Dir::new();
        let app = dir.path().join("app");
        portable_install(&app);
        let archive = dir.path().join("new.zip");
        fs::write(&archive, &archive_bytes).unwrap();
        let result = crate::replace_portable(&archive, &app);
        assert!(
            matches!(result, Err(UpdaterError::Integrity(_))),
            "{result:?}"
        );
        assert_eq!(read(app.join("vibelauncher.exe")), b"old exe");
        assert_eq!(read(app.join("data/instances/world.txt")), b"user data");
        assert!(!app.join(NEW_DIR).exists());
        assert!(!app.join("evil.exe").exists() && !dir.path().join("evil.exe").exists());
    }
}

#[test]
fn symbolic_links_in_portable_archives_are_refused() {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    w.add_symlink("link", "/etc/passwd", options).unwrap();
    w.start_file("vibelauncher", options).unwrap();
    w.write_all(b"x").unwrap();
    let bytes = w.finish().unwrap().into_inner();
    let dir = Dir::new();
    let app = dir.path().join("app");
    portable_install(&app);
    let archive = dir.path().join("new.zip");
    fs::write(&archive, bytes).unwrap();
    assert!(matches!(
        crate::replace_portable(&archive, &app),
        Err(UpdaterError::Integrity(_))
    ));
}

#[test]
fn appimage_is_renamed_over_the_old_one() {
    let dir = Dir::new();
    let target = dir.path().join("VibeLauncher.AppImage");
    let new = dir.path().join("staged.AppImage");
    fs::write(&target, b"old image").unwrap();
    fs::write(&new, b"new image").unwrap();
    let artifact = vl_update::Artifact {
        platform: Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
        },
        kind: ArtifactKind::AppImage,
        url: FILE_URL.into(),
        sha256: sha256(b"new image"),
        size: 9,
    };
    let method = InstallMethod::AppImage {
        file: target.clone(),
    };
    let plan = plan(&method, &target, &v("2.0.0"), &artifact, &new);
    apply(&plan, &|| false).unwrap();
    assert_eq!(read(&target), b"new image");
    assert!(
        new.exists(),
        "the staged file stays until staging is cleared"
    );
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    // A target in a missing folder fails and leaves no temporary file.
    let missing = dir.path().join("nope/VL.AppImage");
    assert!(matches!(
        replace_appimage(&new, &missing),
        Err(UpdaterError::Io(_))
    ));
}

#[test]
fn msi_and_disk_image_are_planned_but_not_implemented() {
    let dir = Dir::new();
    let file = dir.path().join("f");
    fs::write(&file, b"pkg").unwrap();
    let artifact = vl_update::Artifact {
        platform: WIN,
        kind: ArtifactKind::Msi,
        url: FILE_URL.into(),
        sha256: sha256(b"pkg"),
        size: 3,
    };
    let exe = dir.path().join("vibelauncher.exe");
    let version = v("2.0.0");
    let msi = plan(&InstallMethod::Msi, &exe, &version, &artifact, &file);
    assert!(matches!(
        apply(&msi, &|| false),
        Err(UpdaterError::NotImplemented(_))
    ));
    let app = InstallMethod::MacApp {
        bundle: dir.path().join("VibeLauncher.app"),
    };
    let dmg = plan(&app, &exe, &version, &artifact, &file);
    assert!(matches!(
        apply(&dmg, &|| false),
        Err(UpdaterError::NotImplemented(_))
    ));
    let deb = plan(&InstallMethod::Deb, &exe, &version, &artifact, &file);
    assert_eq!(apply(&deb, &|| false), Err(UpdaterError::NothingToReplace));
    let error: vl_core::Error = UpdaterError::GameRunning.into();
    assert_eq!(error.kind, vl_core::ErrorKind::Conflict);
}
