//! Installer tests on a substituted transport with synthetic manifests and archives.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use vl_net::{HttpRequest, HttpResponse, HttpTransport, NetConfig, RetryPolicy, sha256_hex};

use super::*;
use crate::discover::testutil::TempDir;

const LINUX: Platform = Platform {
    os: Os::Linux,
    arch: Arch::X86_64,
};

/// Serves fixed bodies by URL prefix and counts requests.
#[derive(Default)]
struct Fake {
    routes: Mutex<Vec<(String, Vec<u8>)>>,
    hits: AtomicUsize,
}

impl Fake {
    fn add(&self, url: &str, body: Vec<u8>) {
        self.routes.lock().unwrap().push((url.to_owned(), body));
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl HttpTransport for Fake {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        let found = self
            .routes
            .lock()
            .unwrap()
            .iter()
            .find(|(prefix, _)| request.url.starts_with(prefix.as_str()))
            .map(|(_, body)| body.clone());
        Ok(HttpResponse {
            status: if found.is_some() { 200 } else { 404 },
            headers: Vec::new(),
            body: Box::new(io::Cursor::new(found.unwrap_or_default())),
        })
    }
}

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A fake check: the executable's text is its major version; `broken` is not a Java.
fn fake_probe() -> ProbeFn {
    Arc::new(|java| {
        let text = fs::read_to_string(java).map_err(|e| ProbeError::Spawn(e.to_string()))?;
        let major: u32 = text.trim().parse().map_err(|_| ProbeError::NotJava)?;
        Ok(JavaRuntime {
            path: java.to_path_buf(),
            major,
            version: format!("{major}.0.1"),
            arch: Arch::X86_64,
            vendor: "Synthetic".into(),
            managed: false,
        })
    })
}

fn installer(fake: &Arc<Fake>, root: &Path, platform: Platform) -> Installer {
    let mut config = NetConfig::new("test");
    config.retry = RetryPolicy {
        attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    let client = Client::with_transport(config, fake.clone());
    Installer::new(client, root)
        .with_platform(platform)
        .with_probe(fake_probe())
        .with_sources(Sources {
            mojang_index: "https://meta.test/all.json".into(),
            adoptium_api: "https://api.test".into(),
        })
}

/// One file of a synthetic component.
struct File {
    path: &'static str,
    data: Vec<u8>,
    exec: bool,
}

fn file(path: &'static str, data: &str, exec: bool) -> File {
    File {
        path,
        data: data.as_bytes().to_vec(),
        exec,
    }
}

/// Publishes a component manifest and its files; returns the index entry.
fn publish_component(
    fake: &Fake,
    version: &str,
    files: &[File],
    extra: Vec<(&str, Value)>,
) -> Value {
    let mut nodes = serde_json::Map::new();
    for f in files {
        let sha = sha1_hex(&f.data);
        let url = format!("https://objects.test/{sha}");
        fake.add(&url, f.data.clone());
        nodes.insert(
            f.path.to_owned(),
            json!({
                "type": "file",
                "executable": f.exec,
                "downloads": { "raw": { "sha1": sha, "size": f.data.len(), "url": url } },
            }),
        );
    }
    for (name, node) in extra {
        nodes.insert(name.to_owned(), node);
    }
    let manifest = serde_json::to_vec(&json!({ "files": nodes })).unwrap();
    let sha = sha1_hex(&manifest);
    let url = format!("https://meta.test/{sha}/manifest.json");
    fake.add(&url, manifest.clone());
    json!({
        "availability": { "group": 1, "progress": 100 },
        "manifest": { "sha1": sha, "size": manifest.len(), "url": url },
        "version": { "name": version, "released": "2025-01-01T00:00:00+00:00" },
    })
}

fn publish_index(fake: &Fake, platform_key: &str, components: Vec<(&str, Value)>) {
    let mut map = serde_json::Map::new();
    for (name, entry) in components {
        map.insert(name.to_owned(), json!([entry]));
    }
    let index = json!({ platform_key: map, "gamecore": {} });
    fake.add(
        "https://meta.test/all.json",
        serde_json::to_vec(&index).unwrap(),
    );
}

fn runtime_files(major: &str) -> Vec<File> {
    vec![
        file("bin/java", major, true),
        file("lib/modules", "modules", false),
        file("release", "JAVA_VERSION", false),
    ]
}

fn leftovers(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn control() -> Control {
    Control::default()
}

#[test]
fn installs_from_the_mojang_manifest_and_reuses_it_without_the_network() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let delta = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    let legacy = publish_component(&fake, "8u202", &runtime_files("8"), Vec::new());
    publish_index(
        &fake,
        "linux",
        vec![("java-runtime-delta", delta), ("jre-legacy", legacy)],
    );
    let installer = installer(&fake, tmp.path(), LINUX);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let progress = Control::default().on_progress(move |p| sink.lock().unwrap().push(p.done));
    let req = JavaRequirement::new("java-runtime-delta", 21);
    let done = installer.install(&req, &progress).unwrap();

    assert_eq!(done.source, Some(Source::Mojang));
    assert_eq!(done.component, "java-runtime-delta");
    assert_eq!(done.runtime.major, 21);
    assert!(done.runtime.managed);
    assert_eq!(
        done.runtime.path,
        tmp.path().join("java-runtime-delta/bin/java")
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("java-runtime-delta/lib/modules")).unwrap(),
        "modules"
    );
    assert!(done.downloaded > 0);
    assert_eq!(seen.lock().unwrap().last().copied(), Some(done.downloaded));
    assert!(
        leftovers(tmp.path()).is_empty(),
        "{:?}",
        leftovers(tmp.path())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&done.runtime.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        let data = fs::metadata(tmp.path().join("java-runtime-delta/lib/modules")).unwrap();
        assert_eq!(data.permissions().mode() & 0o111, 0);
    }

    // Asking again touches no network, by component or by major version only.
    let hits = fake.hits();
    let again = installer.install(&req, &control()).unwrap();
    assert_eq!((again.source, again.downloaded), (None, 0));
    assert_eq!(again.runtime, done.runtime);
    let by_major = installer
        .install(&JavaRequirement::from_major(21), &control())
        .unwrap();
    assert_eq!(by_major.runtime, done.runtime);
    assert_eq!(fake.hits(), hits);

    // A major version alone finds the component by its version name (`8u202` is Java 8).
    let old = installer
        .install(&JavaRequirement::from_major(8), &control())
        .unwrap();
    assert_eq!(old.component, "jre-legacy");
    assert_eq!(old.runtime.major, 8);
}

#[test]
fn a_wrong_file_hash_installs_nothing() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let entry = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    // Corrupt one object after the manifest has recorded its hash.
    let sha = sha1_hex(b"modules");
    fake.routes.lock().unwrap().insert(
        0,
        (format!("https://objects.test/{sha}"), b"tampered".to_vec()),
    );
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let installer = installer(&fake, tmp.path(), LINUX);

    let err = installer
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap_err();
    // Mojang's failure comes first in the report.
    assert!(err.to_string().contains("download failed"), "{err}");
    assert!(!tmp.path().join("java-runtime-delta").exists());
    assert!(
        leftovers(tmp.path()).is_empty(),
        "{:?}",
        leftovers(tmp.path())
    );
}

#[test]
fn cancelling_stops_the_install_and_cleans_up() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let entry = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let installer = installer(&fake, tmp.path(), LINUX);

    let token = CancelToken::new();
    let trigger = token.clone();
    let control = Control::with_cancel(token).on_progress(move |_| trigger.cancel());
    let err = installer
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control)
        .unwrap_err();
    assert_eq!(err, InstallError::Cancelled);
    assert_eq!(Error::from(err).kind, ErrorKind::Cancelled);
    assert!(!tmp.path().join("java-runtime-delta").exists());
    assert!(
        leftovers(tmp.path()).is_empty(),
        "{:?}",
        leftovers(tmp.path())
    );
}

#[test]
fn manifest_paths_cannot_leave_the_runtime() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let mut files = runtime_files("21");
    files.push(file("../escaped", "x", false));
    let entry = publish_component(&fake, "21.0.7", &files, Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let installer = installer(&fake, tmp.path(), LINUX);

    // The Mojang failure is reported; the fallback has no Adoptium data and fails too.
    let err = installer
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap_err();
    assert!(err.to_string().contains("unsafe path"), "{err}");
    assert!(!tmp.path().join("escaped").exists());
    assert!(!tmp.path().parent().unwrap().join("escaped").exists());
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn a_link_that_escapes_is_refused() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let link = json!({ "type": "link", "target": "../../outside" });
    let entry = publish_component(
        &fake,
        "21.0.7",
        &runtime_files("21"),
        vec![("lib/bad", link)],
    );
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let err = installer(&fake, tmp.path(), LINUX)
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap_err();
    assert!(err.to_string().contains("outside"), "{err}");
}

#[test]
fn a_runtime_that_fails_the_check_never_replaces_a_working_one() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    // The index promises Java 21, the downloaded executable is not a Java at all.
    let broken = vec![file("bin/java", "garbage", true)];
    let entry = publish_component(&fake, "21.0.7", &broken, Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let installer = installer(&fake, tmp.path(), LINUX);
    // An old directory with the same name exists but does not satisfy the request.
    let old = tmp.path().join("java-runtime-delta/bin");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("java"), "17").unwrap();

    let err = installer
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap_err();
    assert!(err.to_string().contains("failed its check"), "{err}");
    assert_eq!(fs::read_to_string(old.join("java")).unwrap(), "17");
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn a_new_install_replaces_an_unfit_directory_atomically() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let entry = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    let stale = tmp.path().join("java-runtime-delta/bin");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("java"), "17").unwrap();
    fs::write(tmp.path().join("java-runtime-delta/old-file"), "x").unwrap();

    let done = installer(&fake, tmp.path(), LINUX)
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap();
    assert_eq!(done.runtime.major, 21);
    assert!(!tmp.path().join("java-runtime-delta/old-file").exists());
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn stale_temporary_directories_of_a_crashed_run_are_removed() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let entry = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", entry)]);
    fs::create_dir_all(tmp.path().join(".tmp-1-0/runtime")).unwrap();
    fs::create_dir_all(tmp.path().join(".old-1-1")).unwrap();
    installer(&fake, tmp.path(), LINUX)
        .install(&JavaRequirement::new("java-runtime-delta", 21), &control())
        .unwrap();
    assert!(leftovers(tmp.path()).is_empty());
}

fn adoptium_listing(fake: &Fake, major: u32, name: &str, archive: &[u8]) {
    let link = format!("https://adopt.test/{name}");
    fake.add(&link, archive.to_vec());
    let listing = json!([
        { "binary": { "package": {
            "name": "OpenJDK-installer.msi", "link": "https://adopt.test/x.msi",
            "checksum": "0".repeat(64), "size": 1 } } },
        { "binary": { "package": {
            "name": name, "link": link, "checksum": sha256_hex(archive), "size": archive.len() } } },
    ]);
    fake.add(
        &format!("https://api.test/v3/assets/latest/{major}/hotspot"),
        serde_json::to_vec(&listing).unwrap(),
    );
}

fn targz(entries: &[(&str, &str, u32)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, name, data.as_bytes())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn zip_archive(entries: &[(&str, &str, u32)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (name, data, mode) in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(*mode);
        writer.start_file(*name, options).unwrap();
        writer.write_all(data.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn adoptium_fills_in_when_mojang_has_no_runtime_for_the_platform() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    // Mojang has Java 21 for this platform but no `jre-legacy`.
    let delta = publish_component(&fake, "21.0.7", &runtime_files("21"), Vec::new());
    publish_index(&fake, "linux", vec![("java-runtime-delta", delta)]);
    let archive = targz(&[
        ("jdk8u999-b01-jre/bin/java", "8", 0o755),
        ("jdk8u999-b01-jre/lib/rt.jar", "jar", 0o644),
    ]);
    adoptium_listing(
        &fake,
        8,
        "OpenJDK8U-jre_x64_linux_hotspot_8u999b01.tar.gz",
        &archive,
    );

    let done = installer(&fake, tmp.path(), LINUX)
        .install(&JavaRequirement::legacy(), &control())
        .unwrap();
    assert_eq!(done.source, Some(Source::Adoptium));
    assert_eq!(done.component, "temurin-8");
    assert_eq!(done.runtime.path, tmp.path().join("temurin-8/bin/java"));
    assert_eq!(done.runtime.major, 8);
    assert!(tmp.path().join("temurin-8/lib/rt.jar").is_file());
    assert!(leftovers(tmp.path()).is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&done.runtime.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}

#[test]
fn adoptium_zip_on_windows_for_an_architecture_without_a_mojang_runtime() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let archive = zip_archive(&[
        ("jdk-21.0.5+11-jre/bin/java.exe", "21", 0o755),
        ("jdk-21.0.5+11-jre/lib/modules", "m", 0o644),
    ]);
    adoptium_listing(
        &fake,
        21,
        "OpenJDK21U-jre_x64_windows_hotspot_21.zip",
        &archive,
    );
    // No Mojang index is served at all (404): the fallback still works.
    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    let done = installer(&fake, tmp.path(), platform)
        .install(&JavaRequirement::from_major(21), &control())
        .unwrap();
    assert_eq!(done.source, Some(Source::Adoptium));
    assert_eq!(
        done.runtime.path,
        tmp.path().join("temurin-21/bin/java.exe")
    );
}

#[test]
fn a_macos_bundle_is_unwrapped_to_its_home() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let archive = targz(&[
        ("jdk-17.0.9+9-jre/Contents/Home/bin/java", "17", 0o755),
        ("jdk-17.0.9+9-jre/Contents/Info.plist", "plist", 0o644),
    ]);
    adoptium_listing(
        &fake,
        17,
        "OpenJDK17U-jre_aarch64_mac_hotspot_17.tar.gz",
        &archive,
    );
    let platform = Platform {
        os: Os::MacOs,
        arch: Arch::Aarch64,
    };
    let mut installer = installer(&fake, tmp.path(), platform);
    installer = installer.with_probe(Arc::new(|java| {
        let mut r = fake_probe()(java)?;
        r.arch = Arch::Aarch64;
        Ok(r)
    }));
    let done = installer
        .install(&JavaRequirement::from_major(17), &control())
        .unwrap();
    assert_eq!(done.runtime.path, tmp.path().join("temurin-17/bin/java"));
}

#[test]
fn an_archive_with_a_path_outside_is_refused() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let archive = zip_archive(&[
        ("../escaped", "x", 0o644),
        ("jre/bin/java.exe", "21", 0o755),
    ]);
    adoptium_listing(&fake, 21, "evil.zip", &archive);
    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };
    let err = installer(&fake, tmp.path(), platform)
        .install(&JavaRequirement::from_major(21), &control())
        .unwrap_err();
    assert!(err.to_string().contains("unsafe path"), "{err}");
    assert!(!tmp.path().parent().unwrap().join("escaped").exists());
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn an_archive_with_a_wrong_checksum_is_refused() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let archive = targz(&[("jre/bin/java", "21", 0o755)]);
    adoptium_listing(&fake, 21, "ok.tar.gz", &archive);
    fake.routes.lock().unwrap().insert(
        0,
        (
            "https://adopt.test/ok.tar.gz".into(),
            b"not the archive".to_vec(),
        ),
    );
    let err = installer(&fake, tmp.path(), LINUX)
        .install(&JavaRequirement::from_major(21), &control())
        .unwrap_err();
    assert!(err.to_string().contains("mismatch"), "{err}");
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn nothing_is_available_for_an_unknown_platform() {
    let tmp = TempDir::new();
    let fake = Arc::new(Fake::default());
    let platform = Platform {
        os: Os::Other,
        arch: Arch::Other,
    };
    let err = installer(&fake, tmp.path(), platform)
        .install(&JavaRequirement::from_major(21), &control())
        .unwrap_err();
    assert!(matches!(err, InstallError::Unavailable(_)), "{err}");
    assert_eq!(Error::from(err).kind, ErrorKind::Java);
    assert_eq!(fake.hits(), 0);
}

#[test]
fn path_and_link_rules() {
    assert_eq!(
        safe_relative("bin/java").unwrap(),
        Some(PathBuf::from("bin/java"))
    );
    assert_eq!(safe_relative("./").unwrap(), None);
    assert_eq!(safe_relative("a/./b").unwrap(), Some(PathBuf::from("a/b")));
    for bad in ["../x", "a/../../x", "/etc/passwd", "a/b:stream"] {
        assert!(safe_relative(bad).is_err(), "{bad}");
    }
    assert!(link_stays_inside(Path::new("lib/libjli.so"), "../bin/x"));
    assert!(link_stays_inside(Path::new("lib/a"), "b"));
    assert!(!link_stays_inside(Path::new("lib/a"), "../../x"));
    assert!(!link_stays_inside(Path::new("a"), "../x"));
    assert!(!link_stays_inside(Path::new("a"), "/abs"));
}

#[test]
fn component_names_from_the_index_must_be_plain() {
    assert!(valid_name("java-runtime-delta").is_ok());
    assert!(valid_name("jre-legacy").is_ok());
    for bad in ["", ".hidden", "../x", "a/b", "a b"] {
        assert!(valid_name(bad).is_err(), "{bad:?}");
    }
}
