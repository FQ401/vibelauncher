//! Legacy Forge installers on a substituted transport, one per era: an inheriting profile with
//! a branch in its Maven version (Minecraft 1.7.10), a standalone profile that strips the game
//! jar and needs FML deobfuscation data (1.5.2), and a 1.12.2 build whose installer has the
//! processor format without processors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
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
const MIRROR_A: &str = "https://mirror-a.example.test/fmllibs/";
const MIRROR_B: &str = "https://mirror-b.example.test/fmllibs";
const DEOBF: &[u8] = b"synthetic deobfuscation data";

#[derive(Default)]
struct FakeServer {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl FakeServer {
    fn add(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), body);
    }

    fn requested(&self, url: &str) -> bool {
        self.requests.lock().unwrap().iter().any(|r| r == url)
    }
}

impl HttpTransport for FakeServer {
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
            "vl-forge-legacy-test-{}-{}",
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

fn entry_names(bytes: &[u8]) -> Vec<String> {
    let zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
    zip.file_names().map(|n| n.unwrap().into_owned()).collect()
}

fn entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
    let mut out = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut out).unwrap();
    out
}

fn publish_installer(server: &FakeServer, maven: &str, entries: &[(&str, &[u8])]) {
    let jar = zip(entries);
    let url = format!("{MAVEN}/net/minecraftforge/forge/{maven}/forge-{maven}-installer.jar");
    server.add(&format!("{url}.sha1"), sha1_hex(&jar).into_bytes());
    server.add(&url, jar);
}

fn universal_1710() -> Vec<u8> {
    zip(&[
        ("org/example/fml/Tweaker.class", b"tweaker"),
        ("fmlversion.properties", b"fmlbuild.mcversion=1.7.10\n"),
    ])
}

fn universal_152() -> Vec<u8> {
    let props = format!(
        "fmlbuild.mcversion=1.5.2\nfmlbuild.deobfuscation.hash={}\n",
        sha1_hex(DEOBF)
    );
    zip(&[
        ("META-INF/FORGE.SF", b"forge signature"),
        ("net/minecraft/client/Patched.class", b"patched"),
        ("fmlversion.properties", props.as_bytes()),
    ])
}

fn publish(server: &FakeServer) {
    let profile_1710 = json!({
        "install": {
            "path": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10",
            "filePath": "forge-universal.jar",
            "minecraft": "1.7.10"
        },
        "versionInfo": {
            "id": "1.7.10-Forge10.13.4.1614-1.7.10",
            "inheritsFrom": "1.7.10",
            "jar": "1.7.10",
            "type": "release",
            "mainClass": "net.minecraft.launchwrapper.Launch",
            "minecraftArguments": "--username ${auth_player_name} --tweakClass org.example.fml.Tweaker",
            "libraries": [
                {"name": "net.minecraftforge:forge:1.7.10-10.13.4.1614-1.7.10", "url": "https://maven.minecraftforge.net/"},
                {"name": "org.example:scala-lib:2.11.1", "url": "https://maven.minecraftforge.net/",
                 "checksums": [sha1_hex(b"scala")]}
            ]
        }
    });
    publish_installer(
        server,
        "1.7.10-10.13.4.1614-1.7.10",
        &[
            (
                "install_profile.json",
                &serde_json::to_vec(&profile_1710).unwrap(),
            ),
            ("forge-universal.jar", &universal_1710()),
        ],
    );

    let profile_152 = json!({
        "install": {
            "path": "net.minecraftforge:minecraftforge:7.8.1.738",
            "filePath": "minecraftforge-universal-1.5.2-7.8.1.738.jar",
            "minecraft": "1.5.2",
            "stripMeta": true
        },
        "versionInfo": {
            "id": "1.5.2-Forge7.8.1.738",
            "type": "release",
            "mainClass": "net.minecraft.launchwrapper.Launch",
            "minecraftArguments": "${auth_player_name} ${auth_session} --tweakClass org.example.legacy.Fixer",
            "libraries": [
                {"name": "net.minecraftforge:minecraftforge:7.8.1.738", "url": "https://maven.minecraftforge.net/"},
                {"name": "net.minecraft:launchwrapper:1.5"},
                {"name": "org.lwjgl.lwjgl:lwjgl:2.9.0", "rules": [{"action": "allow"}]}
            ]
        }
    });
    publish_installer(
        server,
        "1.5.2-7.8.1.738",
        &[
            (
                "install_profile.json",
                &serde_json::to_vec(&profile_152).unwrap(),
            ),
            (
                "minecraftforge-universal-1.5.2-7.8.1.738.jar",
                &universal_152(),
            ),
        ],
    );

    let forge_1122 = b"forge 1.12.2 jar";
    let local = json!({"name": "net.minecraftforge:forge:1.12.2-14.23.5.2860", "downloads": {"artifact": {
        "path": "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
        "url": "", "sha1": sha1_hex(forge_1122), "size": forge_1122.len()}}});
    let install_1122 = json!({
        "spec": 0, "profile": "forge", "version": "1.12.2-forge-14.23.5.2860",
        "path": "net.minecraftforge:forge:1.12.2-14.23.5.2860", "minecraft": "1.12.2",
        "json": "/version.json", "data": {}, "processors": [], "libraries": [local]
    });
    let version_1122 = json!({
        "id": "1.12.2-forge-14.23.5.2860", "inheritsFrom": "1.12.2", "type": "release",
        "mainClass": "net.minecraft.launchwrapper.Launch",
        "minecraftArguments": "--username ${auth_player_name} --tweakClass org.example.fml.Tweaker",
        "libraries": [local]
    });
    publish_installer(
        server,
        "1.12.2-14.23.5.2860",
        &[
            (
                "install_profile.json",
                &serde_json::to_vec(&install_1122).unwrap(),
            ),
            ("version.json", &serde_json::to_vec(&version_1122).unwrap()),
            (
                "maven/net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
                forge_1122,
            ),
        ],
    );

    let xml = "<metadata><versioning><versions>\
        <version>1.5.2-7.8.1.738</version><version>1.7.10-10.13.4.1566-1.7.10</version>\
        <version>1.7.10-10.13.4.1614-1.7.10</version><version>1.12.2-14.23.5.2860</version>\
        </versions></versioning></metadata>";
    server.add(
        &format!("{MAVEN}/net/minecraftforge/forge/maven-metadata.xml"),
        xml.as_bytes().to_vec(),
    );
    server.add(
        PROMOTIONS,
        br#"{"promos": {"1.7.10-recommended": "10.13.4.1614"}}"#.to_vec(),
    );
    // The first mirror serves damaged data, the second the right one.
    server.add(
        &format!("{MIRROR_A}deobfuscation_data_1.5.2.zip"),
        b"damaged".to_vec(),
    );
    server.add(
        &format!("{MIRROR_B}/deobfuscation_data_1.5.2.zip"),
        DEOBF.to_vec(),
    );
}

struct Setup {
    temp: Temp,
    server: Arc<FakeServer>,
    loaders: Loaders,
}

fn setup() -> Setup {
    let temp = Temp::new();
    let server = Arc::new(FakeServer::default());
    publish(&server);
    let mut config = NetConfig::new("test");
    config.retry = RetryPolicy {
        attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let transport: Arc<dyn HttpTransport> = server.clone();
    let loaders = Loaders::new(
        Client::with_transport(config, transport),
        temp.path().join("loaders"),
        LoadersOptions {
            forge: Maven::new(MAVEN, PROMOTIONS)
                .with_fml_libraries(vec![MIRROR_A.to_owned(), MIRROR_B.to_owned()]),
            ..LoadersOptions::default()
        },
    );
    Setup {
        temp,
        server,
        loaders,
    }
}

fn control() -> Control {
    Control::with_cancel(CancelToken::new())
}

struct NoProcessors;

impl ProcessorRunner for NoProcessors {
    fn run(
        &self,
        _invocation: &Invocation,
        _cancel: &CancelToken,
        _line: &mut dyn FnMut(&str),
    ) -> vl_core::Result<()> {
        panic!("legacy installers have no processors");
    }
}

/// A vanilla jar signed like the old Mojang ones.
fn vanilla_jar() -> Vec<u8> {
    zip(&[
        ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
        ("META-INF/MOJANG_C.SF", b"signature"),
        ("net/minecraft/client/Minecraft.class", b"vanilla"),
        ("a.class", b"obfuscated"),
    ])
}

fn install(
    s: &Setup,
    game: &str,
    forge: &str,
    game_dir: Option<&Path>,
) -> vl_core::Result<Installation> {
    let root = s.temp.path();
    let jar = root.join("versions").join(game).join(format!("{game}.jar"));
    if !jar.is_file() {
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, vanilla_jar()).unwrap();
    }
    let libraries = root.join("libraries");
    let versions = root.join("versions");
    let java = PathBuf::from("java");
    let control = control();
    let ctx = InstallContext {
        java: &java,
        minecraft_jar: &jar,
        root,
        libraries_dir: &libraries,
        versions_dir: &versions,
        game_dir,
        runner: &NoProcessors,
        control: &control,
        on_step: &|_, _| {},
    };
    s.loaders
        .install(Forge, game, forge, &ctx)
        .map(Option::unwrap)
}

fn library(s: &Setup, path: &str) -> PathBuf {
    path.split('/')
        .fold(s.temp.path().join("libraries"), |p, part| p.join(part))
}

fn names(list: &vl_meta::loader::LoaderVersions) -> Vec<(&str, bool)> {
    list.versions
        .iter()
        .map(|v| (v.version.as_str(), v.recommended))
        .collect()
}

#[test]
fn lists_legacy_versions_without_their_branch() {
    let s = setup();
    let list = s.loaders.versions(Forge, "1.7.10", &control()).unwrap();
    assert_eq!(
        names(&list),
        [("10.13.4.1614", true), ("10.13.4.1566", false)]
    );
    let list = s.loaders.versions(Forge, "1.5.2", &control()).unwrap();
    assert_eq!(names(&list), [("7.8.1.738", false)]);
    assert_eq!(
        s.loaders
            .pick(Forge, "1.7.10", None, &control())
            .unwrap()
            .version,
        "10.13.4.1614"
    );
}

#[test]
fn installs_an_inheriting_legacy_version() {
    let s = setup();
    let p = s
        .loaders
        .profile(Forge, "1.7.10", "10.13.4.1614", &control())
        .unwrap();
    assert_eq!(p.id, "1.7.10-Forge10.13.4.1614-1.7.10");
    assert_eq!(p.loader_version, "10.13.4.1614");
    assert_eq!(p.jar, None);
    assert!(s.server.requested(&format!(
        "{MAVEN}/net/minecraftforge/forge/1.7.10-10.13.4.1614-1.7.10/forge-1.7.10-10.13.4.1614-1.7.10-installer.jar"
    )));

    let done = install(&s, "1.7.10", "10.13.4.1614", None).unwrap();
    assert!(!done.reused);
    assert_eq!((done.ran, done.skipped), (0, 0));
    let forge = library(
        &s,
        "net/minecraftforge/forge/1.7.10-10.13.4.1614-1.7.10/forge-1.7.10-10.13.4.1614-1.7.10.jar",
    );
    assert_eq!(std::fs::read(&forge).unwrap(), universal_1710());
    // No copy of the game jar and no FML files for this era.
    assert!(!s.temp.path().join("versions").join(&p.id).exists());
    assert!(install(&s, "1.7.10", "10.13.4.1614", None).unwrap().reused);

    // The stored profile is used without the network.
    let fresh = s
        .loaders
        .profile(Forge, "1.7.10", "1.7.10-10.13.4.1614", &control())
        .unwrap();
    assert_eq!(fresh, p);
    assert_eq!(
        s.loaders
            .profile(Forge, "1.7.10", "10.13.9.9", &control())
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
}

#[test]
fn installs_a_standalone_legacy_version_with_its_game_jar_and_fml_files() {
    let s = setup();
    let p = s
        .loaders
        .profile(Forge, "1.5.2", "7.8.1.738", &control())
        .unwrap();
    assert_eq!(p.jar.as_deref(), Some("1.5.2-Forge7.8.1.738"));
    let game_dir = s.temp.path().join("instance");

    let done = install(&s, "1.5.2", "7.8.1.738", Some(&game_dir)).unwrap();
    assert!(!done.reused);
    assert!(done.log_file.is_file());
    let copy = s
        .temp
        .path()
        .join("versions/1.5.2-Forge7.8.1.738/1.5.2-Forge7.8.1.738.jar");
    let bytes = std::fs::read(&copy).unwrap();
    assert_eq!(
        entry_names(&bytes),
        ["net/minecraft/client/Minecraft.class", "a.class"]
    );
    assert_eq!(entry(&bytes, "a.class"), b"obfuscated");
    // The carried Forge jar keeps its signature.
    let forge = library(
        &s,
        "net/minecraftforge/minecraftforge/7.8.1.738/minecraftforge-7.8.1.738.jar",
    );
    assert_eq!(std::fs::read(&forge).unwrap(), universal_152());
    // The damaged copy of the first mirror is refused, the second mirror's is used.
    assert_eq!(
        std::fs::read(game_dir.join("lib/deobfuscation_data_1.5.2.zip")).unwrap(),
        DEOBF
    );

    // Nothing to do the second time; another instance gets the kept FML file.
    assert!(
        install(&s, "1.5.2", "7.8.1.738", Some(&game_dir))
            .unwrap()
            .reused
    );
    let other = s.temp.path().join("other");
    let again = install(&s, "1.5.2", "7.8.1.738", Some(&other)).unwrap();
    assert!(!again.reused);
    assert_eq!(
        std::fs::read(other.join("lib/deobfuscation_data_1.5.2.zip")).unwrap(),
        DEOBF
    );

    // A changed copy of the game jar is made again.
    std::fs::write(&copy, b"changed").unwrap();
    assert!(!install(&s, "1.5.2", "7.8.1.738", None).unwrap().reused);
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
}

#[test]
fn fml_files_that_no_mirror_serves_right_are_an_error() {
    let s = setup();
    s.server.add(
        &format!("{MIRROR_B}/deobfuscation_data_1.5.2.zip"),
        b"also damaged".to_vec(),
    );
    let e = install(&s, "1.5.2", "7.8.1.738", None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Integrity, "{e:?}");
    assert!(e.message.contains("deobfuscation_data_1.5.2.zip"), "{e}");
}

#[test]
fn a_1_12_2_installer_with_the_processor_format_needs_no_processors() {
    let s = setup();
    let p = s
        .loaders
        .profile(Forge, "1.12.2", "14.23.5.2860", &control())
        .unwrap();
    assert_eq!(p.id, "1.12.2-forge-14.23.5.2860");
    assert!(p.replaces_game_arguments);
    let done = install(&s, "1.12.2", "14.23.5.2860", None).unwrap();
    assert_eq!(done.ran, 0);
    assert_eq!(
        std::fs::read(library(
            &s,
            "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar"
        ))
        .unwrap(),
        b"forge 1.12.2 jar"
    );
}

#[test]
fn legacy_profiles_name_their_libraries() {
    let s = setup();
    let p = s
        .loaders
        .profile(Forge, "1.5.2", "7.8.1.738", &control())
        .unwrap();
    let names: Vec<&str> = p.libraries.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "net.minecraftforge:minecraftforge:7.8.1.738",
            "net.minecraft:launchwrapper:1.5"
        ]
    );
    let url: Option<&Value> = p.libraries[1]
        .rest
        .get("downloads")
        .and_then(|d| d.get("artifact"))
        .and_then(|a| a.get("url"));
    assert_eq!(
        url,
        Some(&json!(
            "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.5/launchwrapper-1.5.jar"
        ))
    );
}
