#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use serde_json::json;
use vl_core::ErrorKind;
use vl_core::task::{Progress, ProgressUnit};
use vl_launch::Job;
use vl_meta::assets::AssetLayout;
use vl_net::CancelToken;

#[test]
fn installs_everything_a_version_needs() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());

    let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let job = Job::new().on_progress(move |p| sink.lock().unwrap().push(p));
    let installed = installer.install("1.0", None, &job).expect("install");

    let layout = installer.layout();
    assert!(layout.version_json("1.0").is_file());
    assert_eq!(
        std::fs::read(layout.client_jar("1.0")).unwrap(),
        b"client jar"
    );
    assert!(
        layout
            .libraries_dir()
            .join("com/example/lib/1.0/lib-1.0.jar")
            .is_file()
    );
    assert!(layout.log_configs_dir().join("client-1.12.xml").is_file());
    assert!(layout.asset_index_file("1.0").is_file());
    for hash in [sha1_hex(OBJECT_A), sha1_hex(OBJECT_B)] {
        let object = layout
            .assets_dir()
            .join("objects")
            .join(&hash[..2])
            .join(&hash);
        assert!(object.is_file(), "object {hash}");
    }

    // Natives: unpacked, the excluded prefix is not.
    assert_eq!(
        std::fs::read(installed.natives_dir.join("lwjgl.dll")).unwrap(),
        b"dll"
    );
    assert_eq!(
        std::fs::read(installed.natives_dir.join("sub/dir.dll")).unwrap(),
        b"nested"
    );
    assert!(!installed.natives_dir.join("META-INF").exists());

    // The native archive is not on the class path; the library is.
    assert_eq!(
        installed.libraries.classpath,
        ["com/example/lib/1.0/lib-1.0.jar"]
    );
    assert_eq!(installed.asset_layout, AssetLayout::Objects);
    assert_eq!(installed.assets_index_id, "1.0");
    assert!(installed.logging.is_some());

    // Progress ends at the byte total and names its stages.
    let seen = seen.lock().unwrap();
    let last_bytes = seen
        .iter()
        .rev()
        .find(|p| p.unit == ProgressUnit::Bytes)
        .expect("byte progress");
    assert_eq!(Some(last_bytes.done), last_bytes.total);
    let stages: Vec<_> = seen.iter().filter_map(|p| p.stage.as_deref()).collect();
    assert!(
        stages.contains(&"metadata")
            && stages.contains(&"downloading")
            && stages.contains(&"natives")
    );
}

#[test]
fn an_installed_version_needs_no_network() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    installer(&server, dir.path())
        .install("1.0", None, &Job::new())
        .expect("install");
    assert!(server.request_count() > 0);

    let empty = Arc::new(FakeServer::default());
    let mut config = net_config();
    config.offline = true;
    let offline = installer_with(&empty, dir.path(), config);
    let again = offline
        .install("1.0", None, &Job::new())
        .expect("offline install");
    assert_eq!(empty.request_count(), 0);
    assert_eq!(again.version.main_class, "net.example.Main");
}

#[test]
fn a_damaged_file_is_fetched_again_on_a_full_check() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    installer
        .install("1.0", None, &Job::new())
        .expect("install");
    let lib = installer
        .layout()
        .libraries_dir()
        .join("com/example/lib/1.0/lib-1.0.jar");
    // Same size, different content: a quick check keeps it.
    std::fs::write(&lib, b"LIBRARY JAR").unwrap();
    installer.install("1.0", None, &Job::new()).expect("quick");
    assert_eq!(std::fs::read(&lib).unwrap(), b"LIBRARY JAR");
    // Missing: fetched again.
    std::fs::remove_file(&lib).unwrap();
    installer.install("1.0", None, &Job::new()).expect("repair");
    assert_eq!(std::fs::read(&lib).unwrap(), b"library jar");
}

#[test]
fn cancelling_stops_the_install() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let token = CancelToken::new();
    token.cancel();
    let err = installer(&server, dir.path())
        .install("1.0", None, &Job::with_cancel(token))
        .expect_err("cancelled");
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn cancelling_during_the_download_reports_cancelled() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let token = CancelToken::new();
    let trigger = token.clone();
    // Cancel as soon as the first byte of the batch arrives.
    let job = Job::with_cancel(token).on_progress(move |p| {
        if p.stage.as_deref() == Some("downloading") {
            trigger.cancel();
        }
    });
    let err = installer(&server, dir.path())
        .install("1.0", None, &job)
        .expect_err("cancelled");
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn corrupted_downloads_are_refused() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    // Same length, different bytes.
    server.add(
        &format!("{BASE}/libraries/com/example/lib/1.0/lib-1.0.jar"),
        b"LIBRARY JAR".to_vec(),
    );
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let err = installer
        .install("1.0", None, &Job::new())
        .expect_err("integrity");
    assert_eq!(err.kind, ErrorKind::Integrity);
    assert!(
        !installer
            .layout()
            .libraries_dir()
            .join("com/example/lib/1.0/lib-1.0.jar")
            .exists()
    );
}

#[test]
fn an_unknown_version_is_not_found() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let err = installer(&server, dir.path())
        .install("9.9", None, &Job::new())
        .expect_err("unknown");
    assert_eq!(err.kind, ErrorKind::NotFound);
    let err = installer(&server, dir.path())
        .install("../evil", None, &Job::new())
        .expect_err("unsafe");
    assert_eq!(err.kind, ErrorKind::InvalidInput);
}

#[test]
fn the_virtual_layout_is_restored_below_the_assets_directory() {
    let server = Arc::new(FakeServer::default());
    add_version(
        &server,
        "1.0",
        "legacy",
        json!({"virtual": true}),
        json!({}),
    );
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let installed = installer
        .install("1.0", None, &Job::new())
        .expect("install");
    assert_eq!(installed.asset_layout, AssetLayout::Virtual);
    let restored = installer
        .layout()
        .assets_dir()
        .join("virtual/legacy/minecraft/sounds/a.ogg");
    assert_eq!(std::fs::read(restored).unwrap(), OBJECT_A);
}

#[test]
fn the_resources_layout_is_restored_into_the_game_directory() {
    let server = Arc::new(FakeServer::default());
    add_version(
        &server,
        "1.0",
        "pre-1.6",
        json!({"map_to_resources": true}),
        json!({}),
    );
    let dir = TempDir::new();
    let game = TempDir::new();
    let installer = installer(&server, dir.path());
    let installed = installer
        .install("1.0", Some(game.path()), &Job::new())
        .expect("install");
    assert_eq!(installed.asset_layout, AssetLayout::Resources);
    assert_eq!(
        std::fs::read(game.path().join("resources/lang/en.lang")).unwrap(),
        OBJECT_B
    );
}

#[test]
fn a_version_inheriting_another_uses_its_jar_and_libraries() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let child = json!({
        "id": "1.0-loader",
        "inheritsFrom": "1.0",
        "type": "release",
        "mainClass": "net.example.LoaderMain",
        "libraries": [{"name": "com.example:loader:1.0", "url": format!("{BASE}/libraries/")}],
        "arguments": {"game": ["--tweak"], "jvm": []},
    });
    server.add(
        &format!("{BASE}/libraries/com/example/loader/1.0/loader-1.0.jar"),
        b"loader".to_vec(),
    );
    let installer = installer(&server, dir.path());
    let path = installer.layout().version_json("1.0-loader");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&child).unwrap()).unwrap();

    let installed = installer
        .install("1.0-loader", None, &Job::new())
        .expect("install");
    assert_eq!(installed.version.main_class, "net.example.LoaderMain");
    assert_eq!(installed.client_jar, installer.layout().client_jar("1.0"));
    assert_eq!(
        installed.libraries.classpath[0],
        "com/example/loader/1.0/loader-1.0.jar"
    );
    assert!(
        installed
            .libraries
            .classpath
            .contains(&"com/example/lib/1.0/lib-1.0.jar".to_owned())
    );
}

#[test]
fn a_loader_with_its_own_copy_of_the_game_jar_runs_from_it() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let child = json!({
        "id": "1.0-legacy",
        "inheritsFrom": "1.0",
        "jar": "1.0-legacy",
        "type": "release",
        "mainClass": "net.example.LoaderMain",
        "libraries": [],
    });
    let installer = installer(&server, dir.path());
    let layout = installer.layout();
    let path = layout.version_json("1.0-legacy");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&child).unwrap()).unwrap();

    // The copy is made by the loader's installation; without it the version cannot run.
    let e = installer
        .install("1.0-legacy", None, &Job::new())
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::NotFound, "{e:?}");

    std::fs::write(layout.client_jar("1.0-legacy"), b"stripped").unwrap();
    let installed = installer
        .install("1.0-legacy", None, &Job::new())
        .expect("install");
    assert_eq!(installed.client_jar, layout.client_jar("1.0-legacy"));
    assert_eq!(
        std::fs::read(layout.client_jar("1.0-legacy")).unwrap(),
        b"stripped"
    );
    // The vanilla jar is downloaded to its own place.
    assert!(layout.client_jar("1.0").is_file());
}

/// A loader profile with one library its installer makes: it has a checksum and no address.
fn installer_made_library(installer: &vl_launch::Installer, body: &[u8]) -> std::path::PathBuf {
    let child = json!({
        "id": "1.0-installer",
        "inheritsFrom": "1.0",
        "type": "release",
        "mainClass": "net.example.LoaderMain",
        "libraries": [{
            "name": "net.example:made:1.0",
            "downloads": {"artifact": {
                "path": "net/example/made/1.0/made-1.0.jar",
                "url": "",
                "sha1": sha1_hex(body),
                "size": body.len(),
            }},
        }],
    });
    let path = installer.layout().version_json("1.0-installer");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&child).unwrap()).unwrap();
    let lib = installer
        .layout()
        .libraries_dir()
        .join("net/example/made/1.0/made-1.0.jar");
    std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
    lib
}

#[test]
fn a_full_check_keeps_an_installer_made_library_that_matches() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let lib = installer_made_library(&installer, b"made by the installer");
    std::fs::write(&lib, b"made by the installer").unwrap();
    let full = installer.with_verify(vl_launch::Verify::Full);
    full.install("1.0-installer", None, &Job::new())
        .expect("full check");
    assert_eq!(std::fs::read(&lib).unwrap(), b"made by the installer");
}

#[test]
fn a_full_check_names_a_damaged_installer_made_library() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    let lib = installer_made_library(&installer, b"made by the installer");
    // Same size, other content: only the checksum tells.
    std::fs::write(&lib, b"made by the instalLER").unwrap();
    installer
        .install("1.0-installer", None, &Job::new())
        .expect("a quick check keeps it");
    let full = installer.with_verify(vl_launch::Verify::Full);
    let e = full
        .install("1.0-installer", None, &Job::new())
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity, "{e:?}");
    assert!(e.message.contains("made-1.0.jar"), "{}", e.message);
    assert!(
        e.message.contains("install the mod loader again"),
        "{}",
        e.message
    );
    // Never downloaded, never replaced.
    assert_eq!(std::fs::read(&lib).unwrap(), b"made by the instalLER");
}

#[test]
fn a_missing_installer_made_library_asks_for_the_loader_again() {
    let server = Arc::new(FakeServer::default());
    add_version(&server, "1.0", "1.0", json!({}), json!({}));
    let dir = TempDir::new();
    let installer = installer(&server, dir.path());
    installer_made_library(&installer, b"made by the installer");
    let full = installer.with_verify(vl_launch::Verify::Full);
    let e = full
        .install("1.0-installer", None, &Job::new())
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::NotFound, "{e:?}");
    assert!(
        e.message.contains("install the mod loader again"),
        "{}",
        e.message
    );
}
