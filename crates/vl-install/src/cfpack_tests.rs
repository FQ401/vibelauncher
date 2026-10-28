//! Tests of the CurseForge modpack install on synthetic zips and a fake API and CDN.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use vl_core::ErrorKind;
use vl_core::catalog::Provider;
use vl_core::install::MinStability;
use vl_core::resources::ResourceKind;
use vl_curseforge::{ApiKey, Endpoints};
use vl_net::{
    Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError, PostRequest,
};
use vl_platforms::curseforge::ApiKeySource;
use vl_platforms::{CurseForgeOptions, CurseForgeSource};

use crate::cfpack::{BlockReason, CfPack, fetch, install, is_curseforge_pack, record, resolve};
use crate::mrpack::{InstallInput, PackKind};
use crate::mrpack_tests::{Dir, hex};

pub(crate) const API: &str = "https://cf.test";
/// A synthetic key; real keys never appear in tests.
pub(crate) const KEY: &str = "$2a$10$synthetic-test-key-0000000000";

/// Serves fixed answers by URL (GET) and by URL plus body (POST); counts every request.
#[derive(Default)]
pub(crate) struct Server {
    get: Mutex<HashMap<String, Vec<u8>>>,
    post: Mutex<HashMap<String, Vec<u8>>>,
    seen: Mutex<Vec<String>>,
}

impl Server {
    pub(crate) fn get(&self, url: &str, body: &[u8]) {
        self.get
            .lock()
            .unwrap()
            .insert(url.to_owned(), body.to_vec());
    }

    pub(crate) fn json(&self, url: &str, body: &Value) {
        self.get(url, &serde_json::to_vec(body).unwrap());
    }

    pub(crate) fn post(&self, url: &str, body: &Value) {
        self.post
            .lock()
            .unwrap()
            .insert(url.to_owned(), serde_json::to_vec(body).unwrap());
    }

    pub(crate) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn answer(body: Option<Vec<u8>>) -> HttpResponse {
        HttpResponse {
            status: if body.is_some() { 200 } else { 404 },
            headers: Vec::new(),
            body: Box::new(Cursor::new(body.unwrap_or_default())),
        }
    }
}

impl HttpTransport for Server {
    fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("GET {}", request.url));
        Ok(Self::answer(
            self.get.lock().unwrap().get(&request.url).cloned(),
        ))
    }

    fn post(&self, request: &PostRequest) -> std::result::Result<HttpResponse, NetError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("POST {}", request.url));
        Ok(Self::answer(
            self.post.lock().unwrap().get(&request.url).cloned(),
        ))
    }
}

pub(crate) struct Key(pub(crate) Option<&'static str>);

impl ApiKeySource for Key {
    fn api_key(&self) -> vl_core::Result<Option<ApiKey>> {
        self.0.map(ApiKey::new).transpose()
    }
}

pub(crate) struct World {
    pub(crate) server: Arc<Server>,
    pub(crate) client: Client,
    pub(crate) source: CurseForgeSource,
    pub(crate) dir: Dir,
}

impl World {
    pub(crate) fn new(key: Option<&'static str>) -> Self {
        let server = Arc::new(Server::default());
        let client = Client::with_transport(NetConfig::new("0.0.0"), server.clone());
        let source = CurseForgeSource::new(
            client.clone(),
            CurseForgeOptions {
                endpoints: Endpoints::new(API),
                ..CurseForgeOptions::default()
            },
            Arc::new(Key(key)),
        );
        Self {
            server,
            client,
            source,
            dir: Dir::new(),
        }
    }

    pub(crate) fn game(&self) -> PathBuf {
        self.dir.path().join("game")
    }

    pub(crate) fn input<'a>(
        &'a self,
        game: &'a Path,
        staging: &'a Path,
        control: &'a Control,
    ) -> InstallInput<'a> {
        InstallInput {
            client: &self.client,
            game_dir: game,
            staging,
            control,
        }
    }
}

pub(crate) fn sha1(data: &[u8]) -> String {
    hex(&Sha1::digest(data))
}

/// A `POST /v1/mods/files` entry.
pub(crate) fn file(id: u64, mod_id: u64, name: &str, data: &[u8], url: Option<&str>) -> Value {
    json!({
        "id": id, "modId": mod_id, "displayName": format!("{name} file"), "fileName": name,
        "fileLength": data.len(), "downloadUrl": url,
        "hashes": [{"value": sha1(data), "algo": 1}, {"value": "0".repeat(32), "algo": 2}]
    })
}

/// A `POST /v1/mods` entry.
pub(crate) fn project(id: u64, slug: &str, class: u64) -> Value {
    json!({
        "id": id, "name": format!("Project {slug}"), "slug": slug, "classId": class,
        "links": {"websiteUrl": format!("https://www.curseforge.com/minecraft/x/{slug}")}
    })
}

/// A pack zip with the manifest and `(name, bytes, unix mode)` entries; mode 1 is a symlink.
pub(crate) fn archive(path: &Path, manifest: &Value, entries: &[(&str, &[u8], u32)]) {
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let plain = zip::write::SimpleFileOptions::default();
    w.start_file("manifest.json", plain).unwrap();
    w.write_all(&serde_json::to_vec(manifest).unwrap()).unwrap();
    for (name, data, mode) in entries {
        if *mode == 1 {
            w.add_symlink(*name, std::str::from_utf8(data).unwrap(), plain)
                .unwrap();
            continue;
        }
        w.start_file(*name, plain).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap();
}

pub(crate) fn manifest(files: &[(u64, u64, bool)], loader: &str) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|(p, f, r)| json!({"projectID": p, "fileID": f, "required": r}))
        .collect();
    json!({
        "minecraft": {"version": "1.20.1", "modLoaders": [{"id": loader, "primary": true}]},
        "manifestType": "minecraftModpack", "manifestVersion": 1,
        "name": "Example Adventures", "version": "3.1.0", "author": "someone",
        "files": files, "overrides": "overrides"
    })
}

const JAR: &[u8] = b"jar bytes";
const PACK: &[u8] = b"resource pack bytes";
const SHADER: &[u8] = b"shader bytes";
const OPTIONAL: &[u8] = b"optional jar";

/// A pack with: a mod with a download URL, a resource pack and a shader without one (the CDN
/// serves the resource pack only), an optional mod, a file CurseForge does not know, and a
/// world (a class the launcher does not place).
fn full_world() -> (World, PathBuf) {
    let w = World::new(Some(KEY));
    let pack = w.dir.path().join("pack.zip");
    archive(
        &pack,
        &manifest(
            &[
                (1, 101, true),
                (2, 202, true),
                (3, 303, true),
                (4, 404, false),
                (5, 505, true),
                (6, 606, true),
            ],
            "forge-47.2.0",
        ),
        &[
            ("overrides/config/a.toml", b"a = 1", 0),
            ("overrides/mods/local.jar", b"local", 0),
            ("modlist.html", b"<ul></ul>", 0),
        ],
    );
    w.server.post(
        &format!("{API}/v1/mods/files"),
        &json!({"data": [
            file(101, 1, "a.jar", JAR, Some("https://edge.forgecdn.net/files/0/101/a.jar")),
            file(202, 2, "Soft Pack.zip", PACK, None),
            file(303, 3, "shader.zip", SHADER, None),
            file(404, 4, "opt.jar", OPTIONAL, Some("https://edge.forgecdn.net/files/0/404/opt.jar")),
            file(606, 6, "world.zip", b"w", Some("https://edge.forgecdn.net/files/0/606/world.zip"))
        ]}),
    );
    w.server.post(
        &format!("{API}/v1/mods"),
        &json!({"data": [
            project(1, "a", 6), project(2, "soft", 12), project(3, "shady", 6552),
            project(4, "opt", 6), project(6, "a-world", 17)
        ]}),
    );
    w.server
        .get("https://edge.forgecdn.net/files/0/101/a.jar", JAR);
    w.server.get(
        "https://edge.forgecdn.net/files/0/202/Soft%20Pack.zip",
        PACK,
    );
    w.server
        .get("https://edge.forgecdn.net/files/0/404/opt.jar", OPTIONAL);
    (w, pack)
}

#[test]
fn a_pack_installs_and_lists_what_the_user_must_fetch() {
    let (w, path) = full_world();
    let control = Control::default();
    let pack = CfPack::open(&path).unwrap();
    assert_eq!(pack.game_version(), "1.20.1");
    assert_eq!(
        pack.loader().unwrap(),
        Some((vl_modrinth::mrpack::PackLoader::Forge, "47.2.0".to_owned()))
    );
    assert_eq!(pack.override_count(), 2);

    let resolved = resolve(&w.source, &pack, &control).unwrap();
    assert_eq!(resolved.files.len(), 4);
    assert_eq!(resolved.skipped.len(), 1, "the world is not placed");
    assert_eq!(resolved.blocked.len(), 1);
    assert_eq!(resolved.blocked[0].reason, BlockReason::NotFound);
    assert_eq!(resolved.blocked[0].file_id, 505);
    assert_eq!(
        resolved.blocked[0].page_url,
        "https://www.curseforge.com/projects/5"
    );
    let posts = w
        .server
        .seen()
        .iter()
        .filter(|s| s.starts_with("POST"))
        .count();
    assert_eq!(posts, 2, "one batch for files, one for projects");

    let game = w.game();
    let staging = w.dir.path().join("staging");
    let report = install(&pack, &resolved, &w.input(&game, &staging, &control)).unwrap();

    assert_eq!(std::fs::read(game.join("mods/a.jar")).unwrap(), JAR);
    assert_eq!(
        std::fs::read(game.join("resourcepacks/Soft Pack.zip")).unwrap(),
        PACK,
        "fetched from the CDN address"
    );
    assert_eq!(
        std::fs::read(game.join("mods/opt.jar.disabled")).unwrap(),
        OPTIONAL,
        "an optional file is installed disabled"
    );
    assert!(!game.join("shaderpacks/shader.zip").exists());
    assert_eq!(std::fs::read(game.join("config/a.toml")).unwrap(), b"a = 1");
    assert_eq!(
        std::fs::read(game.join("mods/local.jar")).unwrap(),
        b"local"
    );
    assert!(!game.join("modlist.html").exists());
    assert!(!staging.exists());
    assert_eq!(report.overrides, 2);
    assert_eq!(report.installed.len(), 3);

    let blocked: Vec<(u64, BlockReason)> = report
        .blocked
        .iter()
        .map(|b| (b.file_id, b.reason))
        .collect();
    assert_eq!(
        blocked,
        [
            (505, BlockReason::NotFound),
            (303, BlockReason::DistributionDisabled)
        ]
    );
    let shader = &report.blocked[1];
    assert_eq!(shader.folder, "shaderpacks");
    assert_eq!(shader.name, "Project shady");
    assert_eq!(
        shader.file_url.as_deref(),
        Some("https://www.curseforge.com/minecraft/x/shady/files/303")
    );
    let notes = report.notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("shady/files/303") && n.contains("shaderpacks/")),
        "{notes:?}"
    );
    assert!(notes.iter().any(|n| n.contains("class 17")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("opt.jar")), "{notes:?}");

    let (record, origins) = record(&pack, &report, None).unwrap();
    assert_eq!(record.kind, PackKind::CurseForge);
    assert_eq!(record.name, "Example Adventures");
    assert_eq!(record.pack_version, "3.1.0");
    assert_eq!(record.game_version, "1.20.1");
    assert_eq!(record.loader, Some(("forge".into(), "47.2.0".into())));
    assert_eq!(record.files.len(), 3);
    assert!(
        record
            .files
            .iter()
            .any(|f| f.path == "mods/opt.jar" && f.optional)
    );
    assert_eq!(record.overrides.len(), 2);
    assert_eq!(record.blocked.len(), 2);
    let meta = w.dir.path().join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    record.save(&meta).unwrap();
    let loaded = crate::mrpack::PackRecord::load(&meta).unwrap();
    assert_eq!(loaded, record);

    assert_eq!(origins.len(), 3);
    let jar = origins.iter().find(|o| o.file == "a.jar").unwrap();
    assert_eq!(jar.provider, Provider::CurseForge);
    assert_eq!(jar.project_id, "1");
    assert_eq!(jar.version_id, "101");
    assert_eq!(jar.slug.as_deref(), Some("a"));
    assert_eq!(jar.sha1, sha1(JAR));
    let soft = origins.iter().find(|o| o.file == "Soft Pack.zip").unwrap();
    assert_eq!(soft.kind, ResourceKind::ResourcePacks);
}

#[test]
fn no_key_fails_before_any_request() {
    let (w, path) = full_world();
    let w = World {
        source: CurseForgeSource::new(
            w.client.clone(),
            CurseForgeOptions {
                endpoints: Endpoints::new(API),
                ..CurseForgeOptions::default()
            },
            Arc::new(Key(None)),
        ),
        ..w
    };
    let pack = CfPack::open(&path).unwrap();
    let err = resolve(&w.source, &pack, &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    assert!(w.server.seen().is_empty());

    let err = fetch(
        &w.source,
        &w.client,
        "example",
        None,
        MinStability::default(),
        w.dir.path(),
        &Control::default(),
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    assert!(w.server.seen().is_empty());
}

#[test]
fn a_file_the_api_offers_must_download_and_match() {
    let (w, path) = full_world();
    // The CDN serves other bytes than the SHA-1 CurseForge gives.
    w.server
        .get("https://edge.forgecdn.net/files/0/101/a.jar", b"tampered");
    let control = Control::default();
    let pack = CfPack::open(&path).unwrap();
    let resolved = resolve(&w.source, &pack, &control).unwrap();
    let game = w.game();
    let staging = w.dir.path().join("staging");
    let err = install(&pack, &resolved, &w.input(&game, &staging, &control)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity, "{err:?}");
    assert!(err.message.contains("a.jar"), "{}", err.message);
}

#[test]
fn cancel_stops_the_install() {
    let (w, path) = full_world();
    let control = Control::default();
    let pack = CfPack::open(&path).unwrap();
    let resolved = resolve(&w.source, &pack, &control).unwrap();
    control.cancel.cancel();
    let game = w.game();
    let staging = w.dir.path().join("staging");
    let err = install(&pack, &resolved, &w.input(&game, &staging, &control)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn unsafe_archives_are_refused() {
    let dir = Dir::new();
    let path = dir.path().join("p.zip");
    let m = manifest(&[], "fabric-0.16.5");
    for entries in [
        vec![("overrides/../evil.txt", b"x".as_slice(), 0)],
        vec![("overrides/C:/evil.txt", b"x".as_slice(), 0)],
        vec![("overrides/link", b"/etc/passwd".as_slice(), 1)],
    ] {
        archive(&path, &m, &entries);
        let err = CfPack::open(&path).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Integrity, "{entries:?}");
    }
    std::fs::write(&path, b"not a zip").unwrap();
    assert_eq!(CfPack::open(&path).unwrap_err().kind, ErrorKind::Integrity);
    let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    w.start_file("other.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.finish().unwrap();
    assert_eq!(CfPack::open(&path).unwrap_err().kind, ErrorKind::Integrity);
}

#[test]
fn unsupported_loaders_are_refused() {
    let dir = Dir::new();
    let path = dir.path().join("p.zip");
    archive(&path, &manifest(&[], "liteloader-1.0"), &[]);
    let pack = CfPack::open(&path).unwrap();
    assert_eq!(pack.loader().unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn an_empty_pack_needs_no_request() {
    let w = World::new(Some(KEY));
    let path = w.dir.path().join("p.zip");
    archive(
        &path,
        &json!({"minecraft": {"version": "1.21.1"}, "overrides": "extra"}),
        &[
            ("extra/options.txt", b"o", 0),
            ("overrides/ignored.txt", b"i", 0),
        ],
    );
    let pack = CfPack::open(&path).unwrap();
    assert_eq!(pack.loader().unwrap(), None);
    let control = Control::default();
    let resolved = resolve(&w.source, &pack, &control).unwrap();
    assert!(resolved.files.is_empty());
    assert!(w.server.seen().is_empty());
    let game = w.game();
    let staging = w.dir.path().join("staging");
    let report = install(&pack, &resolved, &w.input(&game, &staging, &control)).unwrap();
    assert_eq!(report.overrides, 1);
    assert!(game.join("options.txt").is_file());
    assert!(!game.join("ignored.txt").exists());
}

#[test]
fn formats_are_told_apart() {
    let dir = Dir::new();
    let cf = dir.path().join("cf.zip");
    archive(&cf, &manifest(&[], "forge-47.2.0"), &[]);
    assert!(is_curseforge_pack(&cf).unwrap());
    let mr = dir.path().join("mr.zip");
    archive(
        &mr,
        &manifest(&[], "forge-47.2.0"),
        &[("modrinth.index.json", b"{}", 0)],
    );
    assert!(!is_curseforge_pack(&mr).unwrap());
    std::fs::write(dir.path().join("x.zip"), b"nope").unwrap();
    assert!(is_curseforge_pack(&dir.path().join("x.zip")).is_err());
}

/// The catalog side: the newest release file of a modpack project, or a pinned one.
#[test]
fn a_catalog_pack_is_fetched_and_verified() {
    let w = World::new(Some(KEY));
    let zip_path = w.dir.path().join("served.zip");
    archive(&zip_path, &manifest(&[], "fabric-0.16.5"), &[]);
    let bytes = std::fs::read(&zip_path).unwrap();
    let pack_file = |id: u64, date: &str, release: u64| {
        json!({
            "id": id, "modId": 9, "displayName": format!("Pack {id}"),
            "fileName": format!("pack-{id}.zip"), "releaseType": release, "fileDate": date,
            "fileLength": bytes.len(), "gameVersions": ["1.20.1", "Fabric"],
            "downloadUrl": format!("https://edge.forgecdn.net/files/0/{id}/pack-{id}.zip"),
            "hashes": [{"value": sha1(&bytes), "algo": 1}]
        })
    };
    w.server.json(
        &format!("{API}/v1/mods/9"),
        &json!({"data": {"id": 9, "slug": "example-pack", "name": "Example Pack", "classId": 4471}}),
    );
    w.server.json(
        &format!("{API}/v1/mods/9/files?index=0&pageSize=50"),
        &json!({"data": [
            pack_file(30, "2026-03-01T00:00:00Z", 3),
            pack_file(20, "2026-02-01T00:00:00Z", 1),
            pack_file(10, "2026-01-01T00:00:00Z", 1)
        ], "pagination": {"index": 0, "pageSize": 50, "resultCount": 3, "totalCount": 3}}),
    );
    w.server.json(
        &format!("{API}/v1/mods/9/files/10"),
        &json!({"data": pack_file(10, "2026-01-01T00:00:00Z", 1)}),
    );
    for id in [10, 20, 30] {
        w.server.get(
            &format!("https://edge.forgecdn.net/files/0/{id}/pack-{id}.zip"),
            &bytes,
        );
    }
    let control = Control::default();
    let dest = w.dir.path().join("dl");
    let (file, source) = fetch(
        &w.source,
        &w.client,
        "9",
        None,
        MinStability::Release,
        &dest,
        &control,
    )
    .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(source.provider, Provider::CurseForge);
    assert_eq!(source.project_id, "9");
    assert_eq!(source.slug, "example-pack");
    assert_eq!(source.version_id, "20", "the newest release, not the alpha");
    assert_eq!(source.version_number, "Pack 20");

    let (_, pinned) = fetch(
        &w.source,
        &w.client,
        "9",
        Some("10"),
        MinStability::Release,
        &dest,
        &control,
    )
    .unwrap();
    assert_eq!(pinned.version_id, "10");

    // A project that is not a modpack is refused.
    w.server.json(
        &format!("{API}/v1/mods/8"),
        &json!({"data": {"id": 8, "slug": "a-mod", "name": "A Mod", "classId": 6}}),
    );
    let err = fetch(
        &w.source,
        &w.client,
        "8",
        None,
        MinStability::Release,
        &dest,
        &control,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
}

/// Live test against the real API: it runs only when `VL_CURSEFORGE_API_KEY` is set and is
/// skipped (it passes without doing anything) otherwise. It resolves the files of a small real
/// modpack without downloading them.
#[test]
fn live_resolve_a_real_pack() {
    use vl_platforms::curseforge::EnvApiKey;
    if EnvApiKey::default().api_key().ok().flatten().is_none() {
        eprintln!("skipped: {} is not set", vl_curseforge::API_KEY_ENV);
        return;
    }
    let client = Client::new(NetConfig::new("0.0.0")).unwrap();
    let source = CurseForgeSource::new(
        client.clone(),
        CurseForgeOptions::default(),
        Arc::new(EnvApiKey::default()),
    );
    let dir = Dir::new();
    let control = Control::default();
    // Fabulously Optimized: a small, long-lived Fabric modpack.
    let (file, origin) = fetch(
        &source,
        &client,
        "396246",
        None,
        MinStability::Release,
        dir.path(),
        &control,
    )
    .unwrap();
    assert_eq!(origin.provider, Provider::CurseForge);
    assert_eq!(origin.project_id, "396246");
    assert!(is_curseforge_pack(&file).unwrap());
    let pack = CfPack::open(&file).unwrap();
    assert!(!pack.manifest.files.is_empty());
    let (loader, _) = pack.loader().unwrap().unwrap();
    assert_eq!(loader, vl_modrinth::mrpack::PackLoader::Fabric);
    let resolved = resolve(&source, &pack, &control).unwrap();
    let total = resolved.files.len() + resolved.blocked.len() + resolved.skipped.len();
    assert_eq!(total, pack.manifest.files.len());
    assert!(
        resolved
            .files
            .iter()
            .all(|f| f.sha1.is_some() && !f.fallback.is_empty()),
        "{resolved:?}"
    );
    assert!(
        resolved
            .files
            .iter()
            .any(|f| f.kind == ResourceKind::Mods && f.file_name.ends_with(".jar"))
    );
}
