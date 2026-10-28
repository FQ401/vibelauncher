//! The Forge source on a substituted transport and a substituted processor runner: version
//! lists with promotions, a spec 0 installer that carries libraries, a launch library made by
//! the processors, and older game versions.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use vl_core::ErrorKind;
use vl_core::component::ComponentKind::Forge;
use vl_loaders::forge::Maven;
use vl_loaders::installer::{InstallContext, Installation};
use vl_loaders::processors::{Invocation, ProcessorRunner};
use vl_loaders::{Loaders, LoadersOptions};
use vl_net::{
    CancelToken, Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError,
    RetryPolicy,
};

const MAVEN: &str = "https://maven.example.test";
const PROMOTIONS: &str = "https://files.example.test/promotions_slim.json";
const GAME: &str = "1.16.5";
const FORGE: &str = "36.2.0";
const FULL: &str = "1.16.5-36.2.0";
const PATCHED_BYTES: &[u8] = b"patched game";
const UNIVERSAL_BYTES: &[u8] = b"universal jar";
const LAUNCHER_BYTES: &[u8] = b"launcher jar";

#[derive(Default)]
struct FakeMaven {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl FakeMaven {
    fn add(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), body);
    }

    fn remove(&self, url: &str) {
        self.files.lock().unwrap().remove(url);
    }

    fn requests(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for FakeMaven {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.requests.lock().unwrap().push(request.url.clone());
        let body = self.files.lock().unwrap().get(&request.url).cloned();
        let (status, body) = match body {
            Some(b) => (200, b),
            None => (404, Vec::new()),
        };
        Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body: Box::new(Cursor::new(body)),
        })
    }
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "vl-forge-test-{}-{}",
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

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        out.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(bytes).unwrap();
    }
    out.finish().unwrap().into_inner()
}

fn tool_jar(main: &str) -> Vec<u8> {
    let manifest = format!("Manifest-Version: 1.0\r\nMain-Class: {main}\r\n\r\n");
    zip(&[("META-INF/MANIFEST.MF", manifest.as_bytes())])
}

fn library(name: &str, path: &str, url: &str, bytes: &[u8]) -> Value {
    json!({
        "name": name,
        "downloads": {"artifact": {"path": path, "url": url, "sha1": sha1_hex(bytes), "size": bytes.len()}}
    })
}

fn served(server: &FakeMaven, name: &str, path: &str, bytes: Vec<u8>) -> Value {
    let url = format!("https://libs.example.test/{path}");
    let lib = library(name, path, &url, &bytes);
    server.add(&url, bytes);
    lib
}

fn universal_path() -> String {
    format!("net/minecraftforge/forge/{FULL}/forge-{FULL}-universal.jar")
}

fn launcher_path() -> String {
    format!("net/minecraftforge/forge/{FULL}/forge-{FULL}.jar")
}

fn client_path() -> String {
    format!("net/minecraftforge/forge/{FULL}/forge-{FULL}-client.jar")
}

/// A spec 0 profile like the Forge 1.13–1.16 installers: no `sides`, the universal jar carried
/// by the installer.
fn install_profile(server: &FakeMaven) -> Value {
    json!({
        "spec": 0,
        "profile": "forge",
        "version": format!("{GAME}-forge-{FORGE}"),
        "path": format!("net.minecraftforge:forge:{FULL}"),
        "minecraft": GAME,
        "json": "/version.json",
        "data": {
            "BINPATCH": {"client": "/data/client.lzma", "server": "/data/server.lzma"},
            "PATCHED": {"client": format!("[net.minecraftforge:forge:{FULL}:client]"), "server": "[x.y:z:1:server]"},
            "PATCHED_SHA": {"client": format!("'{}'", sha1_hex(PATCHED_BYTES)), "server": "''"}
        },
        "processors": [
            {
                "jar": "org.example:patcher:1.0",
                "classpath": ["org.example:patcher:1.0"],
                "args": ["--clean", "{MINECRAFT_JAR}", "--apply", "{BINPATCH}", "--output", "{PATCHED}"],
                "outputs": {"{PATCHED}": "{PATCHED_SHA}"}
            }
        ],
        "libraries": [
            served(server, "org.example:patcher:1.0", "org/example/patcher/1.0/patcher-1.0.jar", tool_jar("org.example.Patcher")),
            library(&format!("net.minecraftforge:forge:{FULL}:universal"), &universal_path(), "", UNIVERSAL_BYTES)
        ]
    })
}

/// The launch profile: the launcher jar is carried by the installer, the client jar is made by
/// the processors.
fn version_json() -> Value {
    json!({
        "id": format!("{GAME}-forge-{FORGE}"),
        "inheritsFrom": GAME,
        "type": "release",
        "releaseTime": "2026-01-01T00:00:00",
        "mainClass": "org.example.Launcher",
        "arguments": {"game": ["--launchTarget", "fmlclient", "--fml.forgeVersion", FORGE], "jvm": []},
        "libraries": [
            library(&format!("net.minecraftforge:forge:{FULL}"), &launcher_path(), "", LAUNCHER_BYTES),
            library(&format!("net.minecraftforge:forge:{FULL}:client"), &client_path(), "", PATCHED_BYTES)
        ]
    })
}

fn publish(server: &FakeMaven) {
    let profile = serde_json::to_vec(&install_profile(server)).unwrap();
    let version = serde_json::to_vec(&version_json()).unwrap();
    let universal = format!("maven/{}", universal_path());
    let launcher = format!("maven/{}", launcher_path());
    let jar = zip(&[
        ("install_profile.json", &profile),
        ("version.json", &version),
        ("data/client.lzma", b"binary patches"),
        (&universal, UNIVERSAL_BYTES),
        (&launcher, LAUNCHER_BYTES),
    ]);
    let url = format!("{MAVEN}/net/minecraftforge/forge/{FULL}/forge-{FULL}-installer.jar");
    server.add(&format!("{url}.sha1"), sha1_hex(&jar).into_bytes());
    server.add(&url, jar);
    let xml = "<metadata><versioning><versions>\
        <version>1.16.5-36.1.0</version><version>1.16.5-36.2.0</version>\
        <version>1.16.5-36.2.5</version><version>1.20.1-47.3.0</version>\
        <version>1.12.2-14.23.5.2860</version></versions></versioning></metadata>";
    server.add(
        &format!("{MAVEN}/net/minecraftforge/forge/maven-metadata.xml"),
        xml.as_bytes().to_vec(),
    );
    server.add(
        PROMOTIONS,
        br#"{"promos": {"1.16.5-recommended": "36.2.0", "1.16.5-latest": "36.2.5"}}"#.to_vec(),
    );
}

fn loaders(server: &Arc<FakeMaven>, dir: &Path) -> Loaders {
    let mut config = NetConfig::new("test");
    config.retry = RetryPolicy {
        attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let transport: Arc<dyn HttpTransport> = server.clone();
    Loaders::new(
        Client::with_transport(config, transport),
        dir.join("loaders"),
        LoadersOptions {
            forge: Maven::new(MAVEN, PROMOTIONS),
            ..LoadersOptions::default()
        },
    )
}

/// Writes each step's `--output` file with `bytes`.
struct FakeRunner {
    calls: Mutex<Vec<Invocation>>,
    bytes: Option<&'static [u8]>,
}

impl FakeRunner {
    fn writing(bytes: Option<&'static [u8]>) -> Self {
        Self {
            calls: Mutex::default(),
            bytes,
        }
    }
}

impl ProcessorRunner for FakeRunner {
    fn run(
        &self,
        invocation: &Invocation,
        _cancel: &CancelToken,
        line: &mut dyn FnMut(&str),
    ) -> vl_core::Result<()> {
        self.calls.lock().unwrap().push(invocation.clone());
        line(&format!("running {}", invocation.main_class));
        if let Some(bytes) = self.bytes {
            let pos = invocation
                .args
                .iter()
                .position(|a| a == "--output")
                .unwrap();
            let out = PathBuf::from(&invocation.args[pos + 1]);
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            std::fs::write(out, bytes).unwrap();
        }
        Ok(())
    }
}

struct Setup {
    temp: Temp,
    server: Arc<FakeMaven>,
    loaders: Loaders,
}

fn setup() -> Setup {
    let temp = Temp::new();
    let server = Arc::new(FakeMaven::default());
    publish(&server);
    let jar = temp
        .path()
        .join("versions")
        .join(GAME)
        .join(format!("{GAME}.jar"));
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    std::fs::write(&jar, b"vanilla").unwrap();
    let loaders = loaders(&server, temp.path());
    Setup {
        temp,
        server,
        loaders,
    }
}

fn control() -> Control {
    Control::with_cancel(CancelToken::new())
}

fn install(
    s: &Setup,
    runner: &dyn ProcessorRunner,
    version: &str,
) -> vl_core::Result<Installation> {
    let root = s.temp.path();
    let jar = root.join("versions").join(GAME).join(format!("{GAME}.jar"));
    let libraries = root.join("libraries");
    let java = PathBuf::from("java");
    let control = control();
    let ctx = InstallContext {
        java: &java,
        minecraft_jar: &jar,
        root,
        libraries_dir: &libraries,
        versions_dir: &root.join("versions"),
        game_dir: None,
        runner,
        control: &control,
        on_step: &|_, _| {},
    };
    s.loaders
        .install(Forge, GAME, version, &ctx)
        .map(Option::unwrap)
}

fn library_file(s: &Setup, path: &str) -> PathBuf {
    path.split('/')
        .fold(s.temp.path().join("libraries"), |p, part| p.join(part))
}

#[test]
fn lists_versions_and_picks_the_recommended_one() {
    let s = setup();
    let list = s.loaders.versions(Forge, GAME, &control()).unwrap();
    let names: Vec<(&str, bool, bool)> = list
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.recommended, v.latest))
        .collect();
    assert_eq!(
        names,
        [
            ("36.2.5", false, true),
            ("36.2.0", true, false),
            ("36.1.0", false, false)
        ]
    );
    let pick = |requested| s.loaders.pick(Forge, GAME, requested, &control());
    assert_eq!(pick(None).unwrap().version, "36.2.0");
    assert_eq!(pick(Some("36.2.5")).unwrap().version, "36.2.5");
    assert_eq!(pick(Some("1.16.5-36.1.0")).unwrap().version, "36.1.0");
    assert_eq!(pick(Some("9.9.9")).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(
        s.loaders
            .pick(Forge, "1.19.2", None, &control())
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );

    // Without the promotions the newest version is used.
    s.server.remove(PROMOTIONS);
    let fresh = loaders(&s.server, &s.temp.path().join("other"));
    let newest = fresh.pick(Forge, GAME, None, &control()).unwrap();
    assert_eq!(newest.version, "36.2.5");
}

#[test]
fn game_versions_without_an_installer_are_unsupported() {
    let s = setup();
    let before = s.server.requests();
    for e in [
        s.loaders.versions(Forge, "1.4.7", &control()).unwrap_err(),
        s.loaders
            .profile(Forge, "1.5.1", "7.7.2.682", &control())
            .unwrap_err(),
    ] {
        assert_eq!(e.kind, ErrorKind::Unsupported);
        assert!(e.message.contains("1.5.2"), "{}", e.message);
    }
    assert_eq!(s.server.requests(), before);
}

#[test]
fn reads_the_profile_from_the_installer() {
    let s = setup();
    let p = s.loaders.profile(Forge, GAME, FORGE, &control()).unwrap();
    assert_eq!(p.id, format!("{GAME}-forge-{FORGE}"));
    assert_eq!(p.loader_version, FORGE);
    assert_eq!(p.main_class, "org.example.Launcher");
    // The full form names the same version.
    let again = s.loaders.profile(Forge, GAME, FULL, &control()).unwrap();
    assert_eq!(again, p);
    assert_eq!(
        s.loaders
            .profile(Forge, GAME, "../x", &control())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        s.loaders
            .profile(Forge, GAME, "36.9.9", &control())
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
}

#[test]
fn installs_the_carried_libraries_and_runs_the_processors() {
    let s = setup();
    let runner = FakeRunner::writing(Some(PATCHED_BYTES));
    let done = install(&s, &runner, FORGE).unwrap();
    assert_eq!((done.reused, done.ran, done.skipped), (false, 1, 0));
    assert!(
        s.temp
            .path()
            .join("loaders/forge")
            .join(FULL)
            .join("installed.json")
            .is_file()
    );
    assert_eq!(
        std::fs::read(library_file(&s, &universal_path())).unwrap(),
        UNIVERSAL_BYTES
    );
    assert_eq!(
        std::fs::read(library_file(&s, &launcher_path())).unwrap(),
        LAUNCHER_BYTES
    );
    assert_eq!(
        std::fs::read(library_file(&s, &client_path())).unwrap(),
        PATCHED_BYTES
    );
    let calls = runner.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "a processor without sides runs for the client"
    );
    assert_eq!(calls[0].main_class, "org.example.Patcher");

    // A launch library that went missing is put back without running the processors.
    std::fs::remove_file(library_file(&s, &launcher_path())).unwrap();
    let again = install(&s, &runner, FORGE).unwrap();
    assert!(again.reused);
    assert_eq!(
        std::fs::read(library_file(&s, &launcher_path())).unwrap(),
        LAUNCHER_BYTES
    );
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[test]
fn a_processor_that_writes_nothing_is_an_integrity_error() {
    let s = setup();
    let e = install(&s, &FakeRunner::writing(None), FORGE).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity, "{e:?}");
    assert!(
        !s.temp
            .path()
            .join("loaders/forge")
            .join(FULL)
            .join("installed.json")
            .exists()
    );
}

#[test]
fn forge_needs_installation() {
    assert!(Loaders::needs_install(Forge));
}
