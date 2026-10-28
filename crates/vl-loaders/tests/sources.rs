//! Fabric and Quilt sources on a substituted transport: version lists, profiles, the profile
//! store and the offline fallback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use vl_core::ErrorKind;
use vl_core::component::ComponentKind::{Fabric, Forge, Quilt};
use vl_loaders::meta::Endpoints;
use vl_loaders::{Loaders, LoadersOptions};
use vl_net::{
    Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError, RetryPolicy,
};

const FABRIC: &str = "https://fabric.example.test/v2";
const QUILT: &str = "https://quilt.example.test/v3";

/// Serves registered URLs, answers `404` for the rest, and fails every request while `down`.
#[derive(Default)]
struct FakeMeta {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
    down: Mutex<bool>,
}

impl FakeMeta {
    fn add(&self, url: &str, body: &Value) {
        self.files
            .lock()
            .unwrap()
            .insert(url.to_owned(), serde_json::to_vec(body).unwrap());
    }

    fn add_raw(&self, url: &str, body: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(url.to_owned(), body.to_vec());
    }

    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }

    fn requests(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for FakeMeta {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.requests.lock().unwrap().push(request.url.clone());
        if *self.down.lock().unwrap() {
            return Err(NetError::Connect("down".into()));
        }
        let body = self.files.lock().unwrap().get(&request.url).cloned();
        Ok(match body {
            Some(body) => HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::new(Cursor::new(body)),
            },
            None => HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Box::new(Cursor::new(Vec::new())),
            },
        })
    }
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "vl-loaders-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn loaders(server: &Arc<FakeMeta>, dir: &Path) -> Loaders {
    let mut config = NetConfig::new("test");
    config.retry = RetryPolicy {
        attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let transport: Arc<dyn HttpTransport> = server.clone();
    Loaders::new(
        Client::with_transport(config, transport),
        dir.join("profiles"),
        LoadersOptions {
            fabric: Endpoints::new(FABRIC),
            quilt: Endpoints::new(QUILT),
            ..LoadersOptions::default()
        },
    )
}

fn per_game(versions: &[(&str, Option<bool>)]) -> Value {
    Value::Array(
        versions
            .iter()
            .map(|(v, stable)| {
                let mut loader = json!({"separator": ".", "build": 1, "version": v});
                if let Some(s) = stable {
                    loader["stable"] = json!(s);
                }
                json!({"loader": loader, "intermediary": {"version": "1.21.1"}, "launcherMeta": {}})
            })
            .collect(),
    )
}

fn profile(kind_prefix: &str, game: &str, loader: &str, main_class: &str) -> Value {
    json!({
        "id": format!("{kind_prefix}-loader-{loader}-{game}"),
        "inheritsFrom": game,
        "type": "release",
        "releaseTime": "2025-01-01T00:00:00+00:00",
        "mainClass": main_class,
        "arguments": {"game": [], "jvm": ["-DFabricMcEmu= net.minecraft.client.main.Main "]},
        "libraries": [{
            "name": format!("net.example:{kind_prefix}-loader:{loader}"),
            "url": "https://maven.example.test/",
            "sha1": "1111111111111111111111111111111111111111",
            "size": 1000,
        }],
    })
}

const KNOT: &str = "net.fabricmc.loader.impl.launch.knot.KnotClient";
const NOW: i64 = 1_800_000_000;

fn control() -> Control {
    Control::default()
}

#[test]
fn lists_versions_and_picks_the_newest_stable_one() {
    let server = Arc::new(FakeMeta::default());
    server.add(
        &format!("{FABRIC}/versions/loader/1.21.1"),
        &per_game(&[
            ("0.19.0-beta.1", Some(false)),
            ("0.18.4", Some(true)),
            ("0.18.3", Some(true)),
        ]),
    );
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());

    let list = loaders.versions(Fabric, "1.21.1", &control()).unwrap();
    assert_eq!(list.versions.len(), 3);
    assert!(!list.versions[0].stable);

    let default = loaders.pick(Fabric, "1.21.1", None, &control()).unwrap();
    assert_eq!(default.version, "0.18.4");
    let asked = loaders
        .pick(Fabric, "1.21.1", Some("0.19.0-beta.1"), &control())
        .unwrap();
    assert!(!asked.stable);

    let err = loaders
        .pick(Fabric, "1.21.1", Some("9.9.9"), &control())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn quilt_versions_without_a_stable_flag_are_judged_by_their_name() {
    let server = Arc::new(FakeMeta::default());
    server.add(
        &format!("{QUILT}/versions/loader/1.14.4"),
        &per_game(&[("0.31.0-beta.4", None), ("0.30.0", None)]),
    );
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());
    let picked = loaders.pick(Quilt, "1.14.4", None, &control()).unwrap();
    assert_eq!(picked.version, "0.30.0");
}

#[test]
fn a_game_the_loader_does_not_support_is_unsupported() {
    let server = Arc::new(FakeMeta::default());
    // An empty list, and the 404 the servers give for an unknown game version.
    server.add(&format!("{FABRIC}/versions/loader/1.2.5"), &json!([]));
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());
    for game in ["1.2.5", "rd-132211"] {
        let err = loaders.pick(Fabric, game, None, &control()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported, "{game}: {err}");
        assert!(err.message.contains("Fabric"), "{err}");
    }
    let err = loaders
        .pick(Fabric, "1.2.5", Some("0.18.4"), &control())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
}

#[test]
fn loaders_without_a_source_are_unsupported() {
    let server = Arc::new(FakeMeta::default());
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());
    let err = loaders.versions(Forge, "1.20.1", &control()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert_eq!(server.requests(), 0);
}

#[test]
fn a_damaged_version_list_is_an_integrity_error() {
    let server = Arc::new(FakeMeta::default());
    server.add_raw(
        &format!("{FABRIC}/versions/loader/1.21.1"),
        b"<html>not json</html>",
    );
    let dir = Temp::new();
    let err = loaders(&server, dir.path())
        .versions(Fabric, "1.21.1", &control())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity);
}

#[test]
fn a_network_failure_without_a_cached_list_is_a_network_error() {
    let server = Arc::new(FakeMeta::default());
    server.set_down(true);
    let dir = Temp::new();
    let err = loaders(&server, dir.path())
        .versions(Fabric, "1.21.1", &control())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
}

#[test]
fn fetches_normalizes_and_stores_a_profile() {
    let server = Arc::new(FakeMeta::default());
    server.add(
        &format!("{FABRIC}/versions/loader/1.21.1/0.18.4/profile/json"),
        &profile("fabric", "1.21.1", "0.18.4", KNOT),
    );
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());

    let p = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW)
        .unwrap();
    assert_eq!(p.main_class, KNOT);
    assert_eq!(p.id, "fabric-loader-0.18.4-1.21.1");
    assert_eq!(p.inherits_from, "1.21.1");
    // Hash and URL moved to the shape the library planner reads.
    let lib = &p.libraries[0];
    assert!(lib.rest.contains_key("downloads"), "{lib:?}");
    assert!(
        dir.path()
            .join("profiles/loader/net.fabricmc.fabric-loader/1.21.1/0.18.4.json")
            .is_file()
    );
    assert_eq!(server.requests(), 1);

    // Young enough: the store answers, the network is not asked.
    let again = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW + 60)
        .unwrap();
    assert_eq!(again, p);
    assert_eq!(server.requests(), 1);
}

#[test]
fn quilt_profiles_come_from_the_quilt_server() {
    let server = Arc::new(FakeMeta::default());
    server.add(
        &format!("{QUILT}/versions/loader/1.14.4/0.30.0/profile/json"),
        &profile(
            "quilt",
            "1.14.4",
            "0.30.0",
            "org.quiltmc.loader.impl.launch.knot.KnotClient",
        ),
    );
    let dir = Temp::new();
    let p = loaders(&server, dir.path())
        .profile_at(Quilt, "1.14.4", "0.30.0", &control(), NOW)
        .unwrap();
    assert_eq!(p.kind, Quilt);
    assert_eq!(
        p.main_class,
        "org.quiltmc.loader.impl.launch.knot.KnotClient"
    );
}

#[test]
fn an_old_profile_is_renewed_and_survives_an_unreachable_server() {
    let server = Arc::new(FakeMeta::default());
    let url = format!("{FABRIC}/versions/loader/1.21.1/0.18.4/profile/json");
    server.add(&url, &profile("fabric", "1.21.1", "0.18.4", KNOT));
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());
    let first = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW)
        .unwrap();

    // Two days later the server is asked again (the answer is the same document).
    let week = NOW + 2 * 24 * 3600;
    loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), week)
        .unwrap();
    assert_eq!(server.requests(), 2);

    // Offline: the stored copy launches the game.
    server.set_down(true);
    let offline = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), week + 3 * 24 * 3600)
        .unwrap();
    assert_eq!(offline, first);

    // A pair that was never fetched has nothing to fall back on.
    let err = loaders
        .profile_at(Fabric, "1.21.1", "0.18.3", &control(), week)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
}

#[test]
fn a_damaged_answer_keeps_the_stored_profile_and_without_one_fails() {
    let server = Arc::new(FakeMeta::default());
    let url = format!("{FABRIC}/versions/loader/1.21.1/0.18.4/profile/json");
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());

    server.add_raw(&url, b"{\"id\": ");
    let err = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity);

    server.add(&url, &profile("fabric", "1.21.1", "0.18.4", KNOT));
    loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW)
        .unwrap();
    server.add_raw(&url, b"{\"id\": ");
    let kept = loaders
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW + 3 * 24 * 3600)
        .unwrap();
    assert_eq!(kept.main_class, KNOT);
}

#[test]
fn a_profile_for_another_game_version_is_rejected() {
    let server = Arc::new(FakeMeta::default());
    server.add(
        &format!("{FABRIC}/versions/loader/1.21.1/0.18.4/profile/json"),
        &profile("fabric", "1.20.4", "0.18.4", KNOT),
    );
    let dir = Temp::new();
    let err = loaders(&server, dir.path())
        .profile_at(Fabric, "1.21.1", "0.18.4", &control(), NOW)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity);
}

#[test]
fn an_unknown_pair_is_not_found() {
    let server = Arc::new(FakeMeta::default());
    let dir = Temp::new();
    let err = loaders(&server, dir.path())
        .profile_at(Fabric, "1.21.1", "9.9.9", &control(), NOW)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

#[test]
fn unsafe_versions_never_reach_the_network() {
    let server = Arc::new(FakeMeta::default());
    let dir = Temp::new();
    let loaders = loaders(&server, dir.path());
    let err = loaders
        .profile_at(Fabric, "1.21.1", "../../x", &control(), NOW)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert_eq!(server.requests(), 0);
}
