//! The NeoForge source on a substituted transport and a substituted processor runner: version
//! lists, the installer and its profile, the processors, reuse of an earlier installation.

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
use vl_core::component::ComponentKind::NeoForge;
use vl_loaders::neoforge::{InstallContext, Installation, Maven};
use vl_loaders::processors::{Invocation, ProcessorRunner};
use vl_loaders::{Loaders, LoadersOptions};
use vl_net::{
    CancelToken, Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError,
    RetryPolicy,
};

const MAVEN: &str = "https://maven.example.test/releases";
const VERSION: &str = "21.1.7";
const GAME: &str = "1.21.1";
const PATCHED_BYTES: &[u8] = b"patched game";

#[derive(Default)]
struct FakeMaven {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
    down: Mutex<bool>,
}

impl FakeMaven {
    fn add(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), body);
    }

    fn requests(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for FakeMaven {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.requests.lock().unwrap().push(request.url.clone());
        if *self.down.lock().unwrap() {
            return Err(NetError::Connect("down".into()));
        }
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
            "vl-neoforge-test-{}-{}",
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

fn library(server: &FakeMaven, name: &str, path: &str, bytes: Vec<u8>) -> Value {
    let url = format!("https://libs.example.test/{path}");
    let lib = json!({
        "name": name,
        "downloads": {"artifact": {"path": path, "url": url, "sha1": sha1_hex(&bytes), "size": bytes.len()}}
    });
    server.add(&url, bytes);
    lib
}

fn install_profile(server: &FakeMaven, patched_sha: &str) -> Value {
    json!({
        "spec": 1,
        "version": format!("neoforge-{VERSION}"),
        "minecraft": GAME,
        "json": "/version.json",
        "data": {
            "MAPPINGS": {"client": "[org.example:maps:1.0:mappings@txt]", "server": "[org.example:maps:1.0:mappings@txt]"},
            "BINPATCH": {"client": "/data/client.lzma", "server": "/data/server.lzma"},
            "PATCHED": {"client": format!("[net.neoforged:neoforge:{VERSION}:client]"), "server": "[x.y:z:1:server]"},
            "PATCHED_SHA": {"client": format!("'{patched_sha}'"), "server": "''"}
        },
        "processors": [
            {
                "jar": "org.example:tool:1.0",
                "classpath": ["org.example:tool:1.0", "org.example:dep:1.0"],
                "args": ["--input", "{MINECRAFT_JAR}", "--output", "{MAPPINGS}", "--side", "{SIDE}"]
            },
            {
                "sides": ["server"],
                "jar": "org.example:tool:1.0",
                "classpath": ["org.example:tool:1.0"],
                "args": ["--never"]
            },
            {
                "sides": ["client"],
                "jar": "org.example:patcher:1.0",
                "classpath": ["org.example:patcher:1.0"],
                "args": ["--clean", "{MAPPINGS}", "--apply", "{BINPATCH}", "--output", "{PATCHED}"],
                "outputs": {"{PATCHED}": "{PATCHED_SHA}"}
            }
        ],
        "libraries": [
            library(server, "org.example:tool:1.0", "org/example/tool/1.0/tool-1.0.jar", tool_jar("org.example.Tool")),
            library(server, "org.example:dep:1.0", "org/example/dep/1.0/dep-1.0.jar", b"dep".to_vec()),
            library(server, "org.example:patcher:1.0", "org/example/patcher/1.0/patcher-1.0.jar", tool_jar("org.example.Patcher"))
        ]
    })
}

fn version_json() -> Value {
    json!({
        "id": format!("neoforge-{VERSION}"),
        "inheritsFrom": GAME,
        "type": "release",
        "releaseTime": "2026-01-01T00:00:00",
        "mainClass": "org.example.Bootstrap",
        "arguments": {"game": [], "jvm": ["-DignoreList=client-extra,${version_name}.jar"]},
        "libraries": []
    })
}

/// Publishes an installer for [`VERSION`] whose patched jar must have `patched_sha`.
fn publish(server: &FakeMaven, patched_sha: &str) {
    let profile = serde_json::to_vec(&install_profile(server, patched_sha)).unwrap();
    let version = serde_json::to_vec(&version_json()).unwrap();
    let jar = zip(&[
        ("install_profile.json", &profile),
        ("version.json", &version),
        ("data/client.lzma", b"binary patches"),
    ]);
    let url = format!("{MAVEN}/net/neoforged/neoforge/{VERSION}/neoforge-{VERSION}-installer.jar");
    server.add(&format!("{url}.sha1"), sha1_hex(&jar).into_bytes());
    server.add(&url, jar);
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
            neoforge: Maven::new(MAVEN),
            ..LoadersOptions::default()
        },
    )
}

/// Writes each step's `--output` file; fails or waits for cancellation on request.
#[derive(Default)]
struct FakeRunner {
    calls: Mutex<Vec<Invocation>>,
    fail: bool,
    block_until_cancelled: bool,
}

impl FakeRunner {
    fn calls(&self) -> Vec<Invocation> {
        self.calls.lock().unwrap().clone()
    }
}

impl ProcessorRunner for FakeRunner {
    fn run(
        &self,
        invocation: &Invocation,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str),
    ) -> vl_core::Result<()> {
        self.calls.lock().unwrap().push(invocation.clone());
        line(&format!("running {}", invocation.main_class));
        if self.block_until_cancelled {
            cancel.cancel();
            return Err(vl_core::Error::cancelled());
        }
        if self.fail {
            line("something broke");
            return Err(vl_core::Error::new(ErrorKind::Java, "exit code 1"));
        }
        let pos = invocation
            .args
            .iter()
            .position(|a| a == "--output")
            .unwrap();
        let out = PathBuf::from(&invocation.args[pos + 1]);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let bytes: &[u8] = if invocation.main_class == "org.example.Patcher" {
            PATCHED_BYTES
        } else {
            b"mappings"
        };
        std::fs::write(out, bytes).unwrap();
        Ok(())
    }
}

struct Setup {
    temp: Temp,
    server: Arc<FakeMaven>,
    loaders: Loaders,
}

fn setup(patched_sha: &str) -> Setup {
    let temp = Temp::new();
    let server = Arc::new(FakeMaven::default());
    publish(&server, patched_sha);
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

fn install(
    s: &Setup,
    runner: &dyn ProcessorRunner,
    control: &Control,
) -> vl_core::Result<Installation> {
    let root = s.temp.path();
    let jar = root.join("versions").join(GAME).join(format!("{GAME}.jar"));
    let libraries = root.join("libraries");
    let java = PathBuf::from("java");
    let steps = Mutex::new(Vec::new());
    let on_step = |i: usize, n: usize| steps.lock().unwrap().push((i, n));
    let ctx = InstallContext {
        java: &java,
        minecraft_jar: &jar,
        root,
        libraries_dir: &libraries,
        runner,
        control,
        on_step: &on_step,
    };
    let result = s
        .loaders
        .install(NeoForge, GAME, VERSION, &ctx)
        .map(Option::unwrap);
    if result.as_ref().is_ok_and(|r| !r.reused) {
        assert_eq!(steps.lock().unwrap().first(), Some(&(0, 2)));
    }
    result
}

fn control() -> Control {
    Control::with_cancel(CancelToken::new())
}

#[test]
fn lists_versions_from_the_maven_metadata() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    let xml = "<metadata><versioning><versions>\
        <version>21.1.2</version><version>21.1.7</version><version>21.1.8-beta</version>\
        <version>21.4.1</version></versions></versioning></metadata>";
    s.server.add(
        &format!("{MAVEN}/net/neoforged/neoforge/maven-metadata.xml"),
        xml.as_bytes().to_vec(),
    );
    let list = s.loaders.versions(NeoForge, GAME, &control()).unwrap();
    let names: Vec<&str> = list.versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(names, ["21.1.8-beta", "21.1.7", "21.1.2"]);
    assert_eq!(
        s.loaders
            .pick(NeoForge, GAME, None, &control())
            .unwrap()
            .version,
        "21.1.7"
    );
    assert_eq!(
        s.loaders
            .pick(NeoForge, "1.20.1", None, &control())
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        s.loaders
            .pick(NeoForge, GAME, Some("9.9.9"), &control())
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
}

#[test]
fn reads_the_profile_from_the_installer_and_keeps_it() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    let p = s
        .loaders
        .profile(NeoForge, GAME, VERSION, &control())
        .unwrap();
    assert_eq!(p.id, format!("neoforge-{VERSION}"));
    assert_eq!(p.inherits_from, GAME);
    assert_eq!(p.main_class, "org.example.Bootstrap");
    assert_eq!(
        p.arguments.jvm[0].values,
        ["-DignoreList=client-extra,1.21.1.jar"]
    );

    // A NeoForge version never changes: the stored profile is used without the network.
    *s.server.down.lock().unwrap() = true;
    let before = s.server.requests();
    let again = s
        .loaders
        .profile(NeoForge, GAME, VERSION, &control())
        .unwrap();
    assert_eq!(again, p);
    assert_eq!(s.server.requests(), before);

    // Another game version is refused.
    *s.server.down.lock().unwrap() = false;
    assert_eq!(
        s.loaders
            .profile(NeoForge, "1.21.4", VERSION, &control())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn unknown_versions_and_damaged_installers_are_errors() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    assert_eq!(
        s.loaders
            .profile(NeoForge, GAME, "21.1.99", &control())
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        s.loaders
            .profile(NeoForge, GAME, "../x", &control())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    let url =
        format!("{MAVEN}/net/neoforged/neoforge/{VERSION}/neoforge-{VERSION}-installer.jar.sha1");
    s.server
        .add(&url, b"0000000000000000000000000000000000000000".to_vec());
    assert_eq!(
        s.loaders
            .profile(NeoForge, GAME, VERSION, &control())
            .unwrap_err()
            .kind,
        ErrorKind::Integrity
    );
}

#[test]
fn runs_the_client_processors_then_reuses_their_files() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    let runner = FakeRunner::default();
    let done = install(&s, &runner, &control()).unwrap();
    assert!(!done.reused);
    assert_eq!((done.ran, done.skipped), (2, 0));

    let calls = runner.calls();
    assert_eq!(calls.len(), 2, "the server-only step does not run");
    let libs = s.temp.path().join("libraries");
    assert_eq!(calls[0].main_class, "org.example.Tool");
    assert_eq!(
        calls[0].classpath,
        [
            libs.join("org/example/tool/1.0/tool-1.0.jar"),
            libs.join("org/example/dep/1.0/dep-1.0.jar")
        ]
        .map(|p| PathBuf::from(
            p.to_str()
                .unwrap()
                .replace('/', std::path::MAIN_SEPARATOR_STR)
        ))
    );
    assert_eq!(calls[0].args[5], "client");
    assert!(calls[0].args[1].ends_with(&format!("{GAME}.jar")));
    assert_eq!(calls[1].main_class, "org.example.Patcher");
    let patch = PathBuf::from(&calls[1].args[3]);
    assert_eq!(std::fs::read(&patch).unwrap(), b"binary patches");
    let patched = libs
        .join("net/neoforged/neoforge")
        .join(VERSION)
        .join(format!("neoforge-{VERSION}-client.jar"));
    assert_eq!(std::fs::read(&patched).unwrap(), PATCHED_BYTES);
    let log = std::fs::read_to_string(&done.log_file).unwrap();
    assert!(log.contains("running org.example.Patcher"), "{log}");

    // Everything matches: nothing runs again, even offline.
    *s.server.down.lock().unwrap() = true;
    let again = install(&s, &runner, &control()).unwrap();
    assert!(again.reused);
    assert_eq!(runner.calls().len(), 2);

    // A changed product runs the processors again; the declared output of the patcher still
    // matches its hash, so that step alone is skipped.
    *s.server.down.lock().unwrap() = false;
    let mappings = libs.join("org/example/maps/1.0/maps-1.0-mappings.txt");
    std::fs::write(&mappings, b"edited").unwrap();
    let third = install(&s, &runner, &control()).unwrap();
    assert_eq!((third.reused, third.ran, third.skipped), (false, 1, 1));

    // A new vanilla jar runs them again too.
    std::fs::write(
        s.temp
            .path()
            .join("versions")
            .join(GAME)
            .join(format!("{GAME}.jar")),
        b"vanilla 2",
    )
    .unwrap();
    assert!(!install(&s, &runner, &control()).unwrap().reused);
}

#[test]
fn an_output_with_another_hash_is_an_integrity_error() {
    let s = setup(&sha1_hex(b"something else"));
    let e = install(&s, &FakeRunner::default(), &control()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity, "{e:?}");
    let patched = s
        .temp
        .path()
        .join("libraries/net/neoforged/neoforge")
        .join(VERSION)
        .join(format!("neoforge-{VERSION}-client.jar"));
    assert!(!patched.exists(), "a wrong output is removed");
}

#[test]
fn a_failing_processor_reports_its_output() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    let runner = FakeRunner {
        fail: true,
        ..FakeRunner::default()
    };
    let e = install(&s, &runner, &control()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Java);
    assert!(e.message.contains("something broke"), "{}", e.message);
    assert!(e.message.contains("processors.log"), "{}", e.message);
    assert!(
        !s.temp
            .path()
            .join("loaders/neoforge")
            .join(VERSION)
            .join("installed.json")
            .exists()
    );
}

#[test]
fn cancelling_stops_the_processors() {
    let s = setup(&sha1_hex(PATCHED_BYTES));
    let runner = FakeRunner {
        block_until_cancelled: true,
        ..FakeRunner::default()
    };
    let e = install(&s, &runner, &control()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert_eq!(runner.calls().len(), 1);

    let cancelled = control();
    cancelled.cancel.cancel();
    let e = install(&s, &FakeRunner::default(), &cancelled).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
}

#[test]
fn other_loaders_need_no_installation() {
    assert!(Loaders::needs_install(NeoForge));
    assert!(!Loaders::needs_install(
        vl_core::component::ComponentKind::Fabric
    ));
}

#[test]
fn a_missing_java_is_a_java_error() {
    let temp = Temp::new();
    let invocation = Invocation {
        java: temp.path().join("no-java-here"),
        classpath: vec![temp.path().join("a.jar")],
        main_class: "org.example.Main".into(),
        args: Vec::new(),
        work_dir: temp.path().to_path_buf(),
    };
    let runner = vl_loaders::processors::JavaRunner::default();
    let e = runner
        .run(&invocation, &CancelToken::new(), &mut |_| {})
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Java);
}
