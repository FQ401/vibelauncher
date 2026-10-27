//! Synthetic upstream for the install and launch tests: a fake HTTP transport that serves
//! generated documents, and temporary directories.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use vl_core::platform::{Arch, Os, Platform};
use vl_launch::{Installer, InstallerOptions, Layout};
use vl_net::{
    Client, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError, RetryPolicy,
};

pub const WINDOWS: Platform = Platform {
    os: Os::Windows,
    arch: Arch::X86_64,
};

pub const BASE: &str = "https://meta.example.test";

pub fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A directory removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "vl-launch-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Serves registered URLs and records every request.
#[derive(Default)]
pub struct FakeServer {
    files: Mutex<HashMap<String, Vec<u8>>>,
    pub requests: Mutex<Vec<String>>,
}

impl FakeServer {
    pub fn add(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), body);
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl HttpTransport for FakeServer {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.requests.lock().unwrap().push(request.url.clone());
        let body = self.files.lock().unwrap().get(&request.url).cloned();
        Ok(match body {
            Some(body) => HttpResponse {
                status: 200,
                headers: vec![("content-length".into(), body.len().to_string())],
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

pub fn net_config() -> NetConfig {
    let mut config = NetConfig::new("test");
    config.retry = RetryPolicy {
        attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
    };
    config.max_parallel = 4;
    config
}

pub fn installer(server: &Arc<FakeServer>, data: &Path) -> Installer {
    installer_with(server, data, net_config())
}

pub fn installer_with(server: &Arc<FakeServer>, data: &Path, config: NetConfig) -> Installer {
    let transport: Arc<dyn HttpTransport> = server.clone();
    Installer::new(
        Client::with_transport(config, transport),
        Layout::new(data),
        InstallerOptions {
            platform: WINDOWS,
            manifest_url: format!("{BASE}/manifest.json"),
            ..InstallerOptions::default()
        },
    )
}

/// A zip archive in memory.
pub fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    out.into_inner()
}

/// What `add_version` registered.
pub struct Published {
    pub json: Value,
    pub object_hashes: Vec<String>,
}

pub const OBJECT_A: &[u8] = b"sound a";
pub const OBJECT_B: &[u8] = b"language file";

/// Registers a version `id` with one library, one old-style native archive, a client jar, a
/// logging file and an asset index (`index_id`, with `flags` merged into the index document), and
/// the manifest that lists it. `extra` is merged into the version JSON.
pub fn add_version(server: &FakeServer, id: &str, index_id: &str, flags: Value, extra: Value) -> Published {
    let lib = b"library jar".to_vec();
    let lib_path = "com/example/lib/1.0/lib-1.0.jar";
    server.add(&format!("{BASE}/libraries/{lib_path}"), lib.clone());

    let native = zip_bytes(&[
        ("lwjgl.dll", b"dll"),
        ("sub/dir.dll", b"nested"),
        ("META-INF/MANIFEST.MF", b"excluded"),
    ]);
    let native_path = "com/example/native/1.0/native-1.0-natives-windows.jar";
    server.add(&format!("{BASE}/libraries/{native_path}"), native.clone());

    let client = b"client jar".to_vec();
    server.add(&format!("{BASE}/{id}/client.jar"), client.clone());

    let logging = b"<Configuration/>".to_vec();
    server.add(&format!("{BASE}/log/client-1.12.xml"), logging.clone());

    let mut objects = serde_json::Map::new();
    let mut hashes = Vec::new();
    for (name, data) in [("minecraft/sounds/a.ogg", OBJECT_A), ("lang/en.lang", OBJECT_B)] {
        let hash = sha1_hex(data);
        server.add(
            &format!(
                "https://resources.download.minecraft.net/{}/{hash}",
                &hash[..2]
            ),
            data.to_vec(),
        );
        objects.insert(name.into(), json!({"hash": hash, "size": data.len()}));
        hashes.push(hash);
    }
    let mut index = json!({ "objects": objects });
    if let (Some(dst), Some(src)) = (index.as_object_mut(), flags.as_object()) {
        dst.extend(src.clone());
    }
    let index_bytes = serde_json::to_vec(&index).unwrap();
    server.add(&format!("{BASE}/indexes/{index_id}.json"), index_bytes.clone());

    let mut version = json!({
        "id": id,
        "type": "release",
        "releaseTime": "2020-01-01T00:00:00+00:00",
        "mainClass": "net.example.Main",
        "javaVersion": {"component": "java-runtime-alpha", "majorVersion": 17},
        "assetIndex": {
            "id": index_id,
            "sha1": sha1_hex(&index_bytes),
            "size": index_bytes.len(),
            "totalSize": 100,
            "url": format!("{BASE}/indexes/{index_id}.json"),
        },
        "downloads": {
            "client": {
                "sha1": sha1_hex(&client),
                "size": client.len(),
                "url": format!("{BASE}/{id}/client.jar"),
            }
        },
        "logging": {"client": {
            "argument": "-Dlog4j.configurationFile=${path}",
            "file": {
                "id": "client-1.12.xml",
                "sha1": sha1_hex(&logging),
                "size": logging.len(),
                "url": format!("{BASE}/log/client-1.12.xml"),
            },
            "type": "log4j2-xml",
        }},
        "libraries": [
            {
                "name": "com.example:lib:1.0",
                "downloads": {"artifact": {
                    "path": lib_path,
                    "sha1": sha1_hex(&lib),
                    "size": lib.len(),
                    "url": format!("{BASE}/libraries/{lib_path}"),
                }},
            },
            {
                "name": "com.example:native:1.0",
                "natives": {"windows": "natives-windows", "linux": "natives-linux", "osx": "natives-osx"},
                "extract": {"exclude": ["META-INF/"]},
                "downloads": {"classifiers": {"natives-windows": {
                    "path": native_path,
                    "sha1": sha1_hex(&native),
                    "size": native.len(),
                    "url": format!("{BASE}/libraries/{native_path}"),
                }}},
            },
        ],
        "arguments": {
            "game": [
                "--username", "${auth_player_name}",
                "--version", "${version_name}",
                "--gameDir", "${game_directory}",
                "--assetsDir", "${assets_root}",
                "--assetIndex", "${assets_index_name}",
                "--uuid", "${auth_uuid}",
                "--accessToken", "${auth_access_token}",
                "--userType", "${user_type}",
                "--versionType", "${version_type}",
                {"rules": [{"action": "allow", "features": {"has_custom_resolution": true}}],
                 "value": ["--width", "${resolution_width}", "--height", "${resolution_height}"]},
                {"rules": [{"action": "allow", "features": {"is_demo_user": true}}], "value": "--demo"},
            ],
            "jvm": [
                {"rules": [{"action": "allow", "os": {"name": "osx"}}], "value": ["-XstartOnFirstThread"]},
                {"rules": [{"action": "allow", "os": {"name": "windows"}}], "value": "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump"},
                "-Djava.library.path=${natives_directory}",
                "-Dminecraft.launcher.brand=${launcher_name}",
                "-cp", "${classpath}",
            ],
        },
    });
    merge(&mut version, extra);
    let version_bytes = serde_json::to_vec(&version).unwrap();
    server.add(&format!("{BASE}/{id}.json"), version_bytes.clone());

    let manifest = json!({
        "latest": {"release": id, "snapshot": id},
        "versions": [{
            "id": id,
            "type": "release",
            "url": format!("{BASE}/{id}.json"),
            "time": "2020-01-01T00:00:00+00:00",
            "releaseTime": "2020-01-01T00:00:00+00:00",
            "sha1": sha1_hex(&version_bytes),
            "complianceLevel": 1,
        }],
    });
    server.add(&format!("{BASE}/manifest.json"), serde_json::to_vec(&manifest).unwrap());
    Published {
        json: version,
        object_hashes: hashes,
    }
}

fn merge(target: &mut Value, extra: Value) {
    if let (Some(t), Value::Object(e)) = (target.as_object_mut(), extra) {
        for (k, v) in e {
            if v.is_null() {
                t.remove(&k);
            } else {
                t.insert(k, v);
            }
        }
    }
}
