//! Tests of the modpack install on synthetic `.mrpack` archives and a fake transport.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use sha1::Sha1;
use sha2::{Digest, Sha512};
use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion, Provider, ReleaseChannel,
    SearchPage, SearchQuery, SideSupport, VersionFile, VersionFilter,
};
use vl_core::install::MinStability;
use vl_core::{Error, ErrorKind, Result};
use vl_net::{
    CancelToken, Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError,
};
use vl_platforms::CatalogSource;

use vl_core::resources::ResourceKind;

use crate::mrpack::{InstallInput, Pack, PackRecord, fetch, install};
use crate::mrpack_origins::resolve;
use crate::origin::OriginStore;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vl-mrpack-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Files(Mutex<HashMap<String, Vec<u8>>>);

impl HttpTransport for Files {
    fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
        let body = self.0.lock().unwrap().get(&request.url).cloned();
        Ok(HttpResponse {
            status: if body.is_some() { 200 } else { 404 },
            headers: Vec::new(),
            body: Box::new(Cursor::new(body.unwrap_or_default())),
        })
    }
}

fn client(files: &[(&str, &[u8])]) -> Client {
    let map = files
        .iter()
        .map(|(u, b)| ((*u).to_owned(), b.to_vec()))
        .collect();
    Client::with_transport(NetConfig::new("0.0.0"), Arc::new(Files(Mutex::new(map))))
}

/// An index entry for `data` at `path`, downloadable from `urls`.
fn entry(path: &str, data: &[u8], urls: &[&str], client_env: &str) -> String {
    let urls: Vec<String> = urls.iter().map(|u| format!("\"{u}\"")).collect();
    format!(
        r#"{{"path":"{path}","hashes":{{"sha1":"{}","sha512":"{}"}},"env":{{"client":"{client_env}","server":"required"}},"downloads":[{}],"fileSize":{}}}"#,
        hex(&Sha1::digest(data)),
        hex(&Sha512::digest(data)),
        urls.join(","),
        data.len()
    )
}

fn index(files: &[String]) -> String {
    format!(
        r#"{{"formatVersion":1,"game":"minecraft","versionId":"2.0","name":"Test Pack","summary":"x","files":[{}],"dependencies":{{"minecraft":"1.21.1","fabric-loader":"0.16.5"}}}}"#,
        files.join(",")
    )
}

/// An archive with the index and the given `(name, bytes, unix mode)` entries.
fn archive(path: &Path, index: &str, entries: &[(&str, &[u8], u32)]) {
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let plain = zip::write::SimpleFileOptions::default();
    w.start_file("modrinth.index.json", plain).unwrap();
    w.write_all(index.as_bytes()).unwrap();
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

const CDN: &str = "https://cdn.modrinth.com/data/";

struct Setup {
    dir: Dir,
    pack: PathBuf,
}

impl Setup {
    fn new(index: &str, entries: &[(&str, &[u8], u32)]) -> Self {
        let dir = Dir::new();
        let pack = dir.path().join("p.mrpack");
        archive(&pack, index, entries);
        Self { dir, pack }
    }

    fn game(&self) -> PathBuf {
        self.dir.path().join("game")
    }

    fn run(&self, client: &Client, control: &Control) -> Result<crate::mrpack::Report> {
        let pack = Pack::open(&self.pack)?;
        let staging = self.dir.path().join("staging");
        let game = self.game();
        install(
            &pack,
            &InstallInput {
                client,
                game_dir: &game,
                staging: &staging,
                control,
            },
        )
    }
}

#[test]
fn installs_files_and_overrides_with_client_overrides_on_top() {
    let a = b"mod a";
    let b = b"optional b";
    let c = b"server only";
    let ua = format!("{CDN}a.jar");
    let ub = format!("{CDN}b.jar");
    let uc = format!("{CDN}c.jar");
    let idx = index(&[
        entry("mods/a.jar", a, &[&ua], "required"),
        entry("mods/b.jar", b, &[&ub], "optional"),
        entry("mods/c.jar", c, &[&uc], "unsupported"),
    ]);
    let s = Setup::new(
        &idx,
        &[
            ("overrides/config/x.toml", b"plain", 0),
            ("overrides/config/only.toml", b"only", 0),
            ("client-overrides/config/x.toml", b"client", 0),
            ("server-overrides/config/s.toml", b"server", 0),
            ("overrides/options/", b"", 0),
        ],
    );
    let client = client(&[(&ua, a), (&ub, b)]);
    let report = s.run(&client, &Control::default()).unwrap();
    let game = s.game();
    assert_eq!(std::fs::read(game.join("mods/a.jar")).unwrap(), a);
    assert_eq!(std::fs::read(game.join("mods/b.jar")).unwrap(), b);
    assert!(!game.join("mods/c.jar").exists());
    assert_eq!(
        std::fs::read(game.join("config/x.toml")).unwrap(),
        b"client"
    );
    assert_eq!(
        std::fs::read(game.join("config/only.toml")).unwrap(),
        b"only"
    );
    assert!(!game.join("config/s.toml").exists());
    assert_eq!(report.installed, ["mods/a.jar", "mods/b.jar"]);
    assert_eq!(report.optional, ["mods/b.jar"]);
    assert_eq!(report.skipped, ["mods/c.jar"]);
    assert_eq!(report.overrides, 3);
    assert!(!s.dir.path().join("staging").exists());

    let pack = Pack::open(&s.pack).unwrap();
    let meta = s.dir.path().join(".vibelauncher");
    PackRecord::new(&pack, &report, None).save(&meta).unwrap();
    let record = PackRecord::load(&meta).unwrap();
    assert_eq!(record.files.len(), 2);
    assert_eq!(
        record.loader,
        Some(("fabric-loader".into(), "0.16.5".into()))
    );
    assert_eq!(record.pack_version, "2.0");
}

#[test]
fn zip_slip_and_unsafe_override_names_refuse_the_pack() {
    let names = [
        "overrides/../evil.txt",
        "overrides/config/../../evil.txt",
        "overrides//abs.txt",
        "overrides/C:/evil.txt",
        "overrides/..\\evil.txt",
        "client-overrides/../../evil.txt",
        "overrides/.vibelauncher/pack.json",
    ];
    for name in names {
        let s = Setup::new(&index(&[]), &[(name, b"x", 0)]);
        let err = Pack::open(&s.pack).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Integrity, "{name}");
        assert!(!s.dir.path().join("evil.txt").exists());
    }
    let s = Setup::new(&index(&[]), &[("overrides/link", b"/etc/passwd", 1)]);
    assert!(Pack::open(&s.pack).is_err());
}

#[test]
fn unsafe_paths_in_the_index_refuse_the_pack() {
    let url = format!("{CDN}a.jar");
    for path in [
        "../a.jar",
        "/a.jar",
        "mods/../../a.jar",
        "C:/a.jar",
        "mods\\\\a.jar",
    ] {
        let s = Setup::new(&index(&[entry(path, b"a", &[&url], "required")]), &[]);
        assert!(Pack::open(&s.pack).is_err(), "{path}");
    }
    let bad_host = entry(
        "mods/a.jar",
        b"a",
        &["https://evil.example/a.jar"],
        "required",
    );
    let s = Setup::new(&index(&[bad_host]), &[]);
    assert!(Pack::open(&s.pack).is_err());
}

#[test]
fn a_damaged_archive_or_a_missing_index_is_refused() {
    let dir = Dir::new();
    let junk = dir.path().join("junk.mrpack");
    std::fs::write(&junk, b"not a zip").unwrap();
    assert_eq!(Pack::open(&junk).unwrap_err().kind, ErrorKind::Integrity);
    assert_eq!(
        Pack::open(&dir.path().join("none.mrpack"))
            .unwrap_err()
            .kind,
        ErrorKind::Io
    );
    let empty = dir.path().join("empty.mrpack");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&empty).unwrap());
    w.start_file("other.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.finish().unwrap();
    assert!(Pack::open(&empty).is_err());
}

#[test]
fn a_wrong_hash_fails_and_the_next_url_is_tried() {
    let data = b"real";
    let bad_url = format!("{CDN}bad.jar");
    let good_url = format!("{CDN}good.jar");
    let idx = index(&[entry(
        "mods/a.jar",
        data,
        &[&bad_url, &good_url],
        "required",
    )]);
    // The first URL serves other bytes, the second the right ones.
    let s = Setup::new(&idx, &[]);
    let c = client(&[(&bad_url, b"tampered"), (&good_url, data)]);
    s.run(&c, &Control::default()).unwrap();
    assert_eq!(std::fs::read(s.game().join("mods/a.jar")).unwrap(), data);

    let s = Setup::new(&idx, &[]);
    let c = client(&[(&bad_url, b"tampered"), (&good_url, b"also wrong")]);
    let err = s.run(&c, &Control::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity, "{err:?}");
    assert!(!s.dir.path().join("staging").exists());
}

#[test]
fn a_cancelled_install_reports_cancelled() {
    let data = b"a";
    let url = format!("{CDN}a.jar");
    let s = Setup::new(
        &index(&[entry("mods/a.jar", data, &[&url], "required")]),
        &[],
    );
    let token = CancelToken::new();
    token.cancel();
    let err = s
        .run(&client(&[(&url, data)]), &Control::with_cancel(token))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

/// A catalog with one modpack project whose versions carry `.mrpack` files.
struct Packs {
    versions: Vec<ProjectVersion>,
    kind: ProjectType,
}

fn pack_version(id: &str, channel: ReleaseChannel, bytes: &[u8]) -> ProjectVersion {
    ProjectVersion {
        id: id.into(),
        project_id: "P1".into(),
        name: id.into(),
        version_number: format!("n-{id}"),
        channel,
        game_versions: vec!["1.21.1".into()],
        loaders: vec!["fabric".into()],
        date_published: String::new(),
        downloads: 0,
        files: vec![VersionFile {
            filename: format!("{id}.mrpack"),
            url: format!("{CDN}{id}.mrpack"),
            size: bytes.len() as u64,
            sha1: Some(hex(&Sha1::digest(bytes))),
            sha512: Some(hex(&Sha512::digest(bytes))),
            primary: true,
        }],
        dependencies: Vec::new(),
    }
}

impl CatalogSource for Packs {
    fn provider(&self) -> Provider {
        Provider::Modrinth
    }
    fn search(&self, _: &SearchQuery, _: &Control) -> Result<SearchPage> {
        Ok(SearchPage::default())
    }
    fn project(&self, id: &str, _: &Control) -> Result<Project> {
        if id != "P1" && id != "pack" {
            return Err(Error::not_found(id));
        }
        Ok(Project {
            id: "P1".into(),
            slug: "pack".into(),
            title: "Pack".into(),
            description: String::new(),
            body: String::new(),
            project_type: self.kind,
            categories: Vec::new(),
            loaders: Vec::new(),
            game_versions: Vec::new(),
            downloads: 0,
            followers: 0,
            icon_url: None,
            license: None,
            source_url: None,
            issues_url: None,
            client_side: SideSupport::default(),
            server_side: SideSupport::default(),
            updated: String::new(),
            version_ids: Vec::new(),
        })
    }
    fn versions(&self, _: &str, _: &VersionFilter, _: &Control) -> Result<Vec<ProjectVersion>> {
        Ok(self.versions.clone())
    }
    fn versions_by_hashes(
        &self,
        _: HashAlgorithm,
        _: &[String],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        Ok(Vec::new())
    }
}

#[test]
fn fetches_the_best_version_or_the_pinned_one() {
    let (new_beta, rel, old) = (&b"beta bytes"[..], &b"release bytes"[..], &b"old bytes"[..]);
    let source = Packs {
        versions: vec![
            pack_version("vb", ReleaseChannel::Beta, new_beta),
            pack_version("vr", ReleaseChannel::Release, rel),
            pack_version("vo", ReleaseChannel::Release, old),
        ],
        kind: ProjectType::Modpack,
    };
    let (ub, ur, uo) = (
        format!("{CDN}vb.mrpack"),
        format!("{CDN}vr.mrpack"),
        format!("{CDN}vo.mrpack"),
    );
    let c = client(&[(&ub, new_beta), (&ur, rel), (&uo, old)]);
    let dir = Dir::new();
    let ctl = Control::default();
    let min = MinStability::Beta;
    let (file, record) = fetch(&source, &c, "pack", None, min, dir.path(), &ctl).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), rel);
    assert_eq!(
        (record.project_id.as_str(), record.version_id.as_str()),
        ("P1", "vr")
    );
    let (file, record) = fetch(&source, &c, "pack", Some("vo"), min, dir.path(), &ctl).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), old);
    assert_eq!(record.version_number, "n-vo");
    let err = fetch(&source, &c, "pack", Some("zz"), min, dir.path(), &ctl).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);

    let not_pack = Packs {
        versions: Vec::new(),
        kind: ProjectType::Mod,
    };
    let err = fetch(&not_pack, &c, "pack", None, min, dir.path(), &ctl).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
}

/// A catalog that knows files by hash and records each lookup.
struct Known {
    by_sha512: HashMap<String, ProjectVersion>,
    by_sha1: HashMap<String, ProjectVersion>,
    fail: bool,
    calls: Mutex<Vec<(HashAlgorithm, usize)>>,
}

impl CatalogSource for Known {
    fn provider(&self) -> Provider {
        Provider::Modrinth
    }
    fn search(&self, _: &SearchQuery, _: &Control) -> Result<SearchPage> {
        Ok(SearchPage::default())
    }
    fn project(&self, id: &str, _: &Control) -> Result<Project> {
        Err(Error::not_found(id))
    }
    fn versions(&self, _: &str, _: &VersionFilter, _: &Control) -> Result<Vec<ProjectVersion>> {
        Ok(Vec::new())
    }
    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        self.calls.lock().unwrap().push((algorithm, hashes.len()));
        if self.fail {
            return Err(Error::new(ErrorKind::Network, "down"));
        }
        let table = match algorithm {
            HashAlgorithm::Sha512 => &self.by_sha512,
            _ => &self.by_sha1,
        };
        Ok(hashes
            .iter()
            .filter_map(|h| {
                Some(HashMatch {
                    hash: h.clone(),
                    version: table.get(h)?.clone(),
                })
            })
            .collect())
    }
}

fn known_version(id: &str) -> ProjectVersion {
    let mut v = pack_version(id, ReleaseChannel::Release, b"");
    v.project_id = format!("proj-{id}");
    v.files.clear();
    v
}

#[test]
fn pack_files_get_origin_records_by_hash() {
    let (a, b, c, d) = (b"mod a", b"mod b", b"mod c", b"shader d");
    let entries = [
        entry("mods/a.jar", a, &[&format!("{CDN}a")], "required"),
        entry("mods/b.jar", b, &[&format!("{CDN}b")], "required"),
        entry("mods/c.jar", c, &[&format!("{CDN}c")], "required"),
        entry("config/d.zip", d, &[&format!("{CDN}d")], "required"),
    ];
    let setup = Setup::new(&index(&entries), &[]);
    let net = client(&[
        (&format!("{CDN}a"), a),
        (&format!("{CDN}b"), b),
        (&format!("{CDN}c"), c),
        (&format!("{CDN}d"), d),
    ]);
    let report = setup.run(&net, &Control::default()).unwrap();
    let pack = Pack::open(&setup.pack).unwrap();

    let mut source = Known {
        by_sha512: HashMap::new(),
        by_sha1: HashMap::new(),
        fail: false,
        calls: Mutex::new(Vec::new()),
    };
    // `a` is known by SHA-512, `b` only by SHA-1, `c` and the file outside a resource folder
    // are not in the catalog (or not placed in one).
    source
        .by_sha512
        .insert(hex(&Sha512::digest(a)), known_version("va"));
    source
        .by_sha1
        .insert(hex(&Sha1::digest(b)), known_version("vb"));
    source
        .by_sha512
        .insert(hex(&Sha512::digest(d)), known_version("vd"));
    let found = resolve(&source, &pack, &report, &Control::default()).unwrap();
    assert!(!found.lookup_failed);
    let mut names: Vec<_> = found
        .origins
        .iter()
        .map(|o| {
            (
                o.file.as_str(),
                o.project_id.as_str(),
                o.version_id.as_str(),
            )
        })
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [("a.jar", "proj-va", "va"), ("b.jar", "proj-vb", "vb")]
    );
    let mut unknown = found.unknown.clone();
    unknown.sort_unstable();
    assert_eq!(unknown, ["config/d.zip", "mods/c.jar"]);
    assert_eq!(found.origins[0].sha1, hex(&Sha1::digest(a)));
    assert_eq!(found.origins[0].kind, ResourceKind::Mods);
    // One SHA-512 request for all files, then one SHA-1 request for the three left.
    assert_eq!(
        *source.calls.lock().unwrap(),
        [(HashAlgorithm::Sha512, 4), (HashAlgorithm::Sha1, 2)]
    );

    // The records are the ones the catalog install writes: the store reads them back, and the
    // pack record marks which files have one.
    let store = OriginStore::new(setup.dir.path().join("resources.json"));
    store.save(&found.origins).unwrap();
    let loaded = store.load();
    assert!(!loaded.damaged);
    assert_eq!(loaded.origins.len(), 2);
    let known: std::collections::HashSet<&str> = ["mods/a.jar", "mods/b.jar"].into();
    let record = PackRecord::new(&pack, &report, None).with_known_origins(&known);
    let flags: Vec<_> = record
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.origin_known))
        .collect();
    assert_eq!(
        flags,
        [
            ("mods/a.jar", true),
            ("mods/b.jar", true),
            ("mods/c.jar", false),
            ("config/d.zip", false)
        ]
    );

    // A failing lookup leaves the pack installed with every file unknown.
    source.fail = true;
    let failed = resolve(&source, &pack, &report, &Control::default()).unwrap();
    assert!(failed.lookup_failed);
    assert!(failed.origins.is_empty());
    assert_eq!(failed.unknown.len(), 4);
}
