//! Tests of the modpack export on synthetic game directories and a fake catalog.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::Path;

use sha1::{Digest, Sha1};
use sha2::Sha512;
use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectVersion, Provider, ReleaseChannel, SearchPage,
    SearchQuery, VersionFile, VersionFilter,
};
use vl_core::install::{ExportInclude, PackExport};
use vl_core::{Error, ErrorKind, Result};
use vl_modrinth::mrpack::{PackLoader, Support};
use vl_net::Control;
use vl_platforms::CatalogSource;

use crate::mrpack::{InstallInput, Pack, install};
use crate::mrpack_export::{ExportInput, export};
use crate::mrpack_tests::{CDN, Dir, client, hex};

/// A catalog that owns the files whose bytes it was given.
struct Catalog {
    by_sha512: HashMap<String, ProjectVersion>,
    fail: bool,
}

impl Catalog {
    fn new(known: &[(&str, &[u8])]) -> Self {
        Self::on(CDN, known)
    }

    /// A catalog whose download links start with `base`.
    fn on(base: &str, known: &[(&str, &[u8])]) -> Self {
        let by_sha512 = known
            .iter()
            .map(|(name, data)| {
                let sha512 = hex(&Sha512::digest(data));
                let version = ProjectVersion {
                    id: format!("v-{name}"),
                    project_id: format!("p-{name}"),
                    name: (*name).into(),
                    version_number: "1".into(),
                    channel: ReleaseChannel::Release,
                    game_versions: vec!["1.21.4".into()],
                    loaders: vec!["fabric".into()],
                    date_published: String::new(),
                    downloads: 0,
                    files: vec![VersionFile {
                        filename: (*name).into(),
                        url: format!("{base}{name}"),
                        size: data.len() as u64,
                        sha1: Some(hex(&Sha1::digest(data))),
                        sha512: Some(sha512.clone()),
                        primary: true,
                    }],
                    dependencies: Vec::new(),
                };
                (sha512, version)
            })
            .collect();
        Self {
            by_sha512,
            fail: false,
        }
    }
}

impl CatalogSource for Catalog {
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
        if self.fail {
            return Err(Error::new(ErrorKind::Network, "down"));
        }
        // Only SHA-512 is indexed; a SHA-1 lookup finds nothing, like an unknown file.
        if algorithm != HashAlgorithm::Sha512 {
            return Ok(Vec::new());
        }
        Ok(hashes
            .iter()
            .filter_map(|h| {
                Some(HashMatch {
                    hash: h.clone(),
                    version: self.by_sha512.get(h)?.clone(),
                })
            })
            .collect())
    }
}

const SODIUM: &[u8] = b"sodium jar";
const LITHIUM: &[u8] = b"lithium jar";
const LOCAL: &[u8] = b"my own mod";
const PACK_RP: &[u8] = b"a resource pack";

fn put(root: &Path, rel: &str, data: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

/// A game directory with known and unknown mods and everything an export must leave alone.
fn game(root: &Path) {
    put(root, "mods/sodium.jar", SODIUM);
    put(root, "mods/lithium.jar", LITHIUM);
    put(root, "mods/mine.jar", LOCAL);
    put(root, "mods/off.jar.disabled", b"disabled");
    put(root, "resourcepacks/rp.zip", PACK_RP);
    put(root, "resourcepacks/mine.zip", b"my pack");
    put(root, "config/sodium.json", b"{}");
    put(root, "defaultconfigs/d.toml", b"d");
    put(root, "kubejs/server_scripts/a.js", b"//");
    put(root, "shaderpacks/s.zip.txt", b"settings");
    put(root, "options.txt", b"fov:90");
    put(root, "saves/World/level.dat", b"world");
    put(root, "logs/latest.log", b"log");
    put(root, "crash-reports/c.txt", b"crash");
    put(root, "screenshots/s.png", b"png");
    put(root, ".vibelauncher/pack.json", b"{}");
    put(root, ".vibelauncher/resources.json", b"{}");
    put(root, "cache/x", b"x");
}

fn catalog() -> Catalog {
    Catalog::new(&[
        ("sodium.jar", SODIUM),
        ("lithium.jar", LITHIUM),
        ("rp.zip", PACK_RP),
    ])
}

fn run(
    source: &dyn CatalogSource,
    game: &Path,
    out: &Path,
    include: &[ExportInclude],
) -> Result<PackExport> {
    export(&ExportInput {
        source,
        game_dir: game,
        out,
        name: "My Pack",
        version_id: "1.2.3",
        summary: Some("A test"),
        game_version: "1.21.4",
        loader: Some((PackLoader::Fabric, "0.16.9")),
        include,
        control: &Control::default(),
    })
}

fn entries(pack: &Path) -> BTreeSet<String> {
    let mut zip = zip::ZipArchive::new(fs::File::open(pack).unwrap()).unwrap();
    (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect()
}

fn read(pack: &Path, name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(fs::File::open(pack).unwrap()).unwrap();
    let mut bytes = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn known_files_are_linked_and_the_rest_is_bundled() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    game(&root);
    let out = dir.path().join("p.mrpack");
    let report = run(&catalog(), &root, &out, &ExportInclude::DEFAULT).unwrap();

    assert_eq!(report.linked, 3);
    assert_eq!(report.disabled, 1);
    assert_eq!(report.unlisted, ["mods/mine.jar", "resourcepacks/mine.zip"]);
    assert!(report.warnings.iter().any(|w| w.contains("responsible")));
    assert!(report.size > 0);

    let names = entries(&out);
    let expected: BTreeSet<String> = [
        "modrinth.index.json",
        "overrides/mods/mine.jar",
        "overrides/resourcepacks/mine.zip",
        "overrides/config/sodium.json",
        "overrides/defaultconfigs/d.toml",
        "overrides/shaderpacks/s.zip.txt",
    ]
    .map(String::from)
    .into();
    assert_eq!(names, expected);

    let pack = Pack::open(&out).unwrap();
    assert_eq!(pack.index.name, "My Pack");
    assert_eq!(pack.index.version_id, "1.2.3");
    assert_eq!(pack.index.summary.as_deref(), Some("A test"));
    assert_eq!(pack.index.game_version(), Some("1.21.4"));
    assert_eq!(pack.index.loader(), Some((PackLoader::Fabric, "0.16.9")));
    let paths: Vec<&str> = pack.index.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "mods/lithium.jar",
            "mods/sodium.jar",
            "resourcepacks/rp.zip"
        ]
    );
    let sodium = &pack.index.files[1];
    assert_eq!(sodium.downloads, [format!("{CDN}sodium.jar")]);
    assert_eq!(sodium.sha1, hex(&Sha1::digest(SODIUM)));
    assert_eq!(sodium.sha512, hex(&Sha512::digest(SODIUM)));
    assert_eq!(sodium.size, SODIUM.len() as u64);
    assert_eq!(
        (sodium.client, sodium.server),
        (Support::Required, Support::Optional)
    );
    assert_eq!(pack.index.files[2].server, Support::Unsupported);
    assert!(!dir.path().join("p.mrpack.part").exists());
}

#[test]
fn what_to_include_is_chosen() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    game(&root);

    let none = dir.path().join("none.mrpack");
    run(&catalog(), &root, &none, &[]).unwrap();
    let names = entries(&none);
    assert!(names.contains("overrides/mods/mine.jar"));
    assert!(
        !names
            .iter()
            .any(|n| n.contains("config") || n.contains("resourcepacks"))
    );

    let more = dir.path().join("more.mrpack");
    let all = [
        ExportInclude::Config,
        ExportInclude::Kubejs,
        ExportInclude::Options,
    ];
    run(&catalog(), &root, &more, &all).unwrap();
    let names = entries(&more);
    for wanted in [
        "overrides/kubejs/server_scripts/a.js",
        "overrides/options.txt",
        "overrides/config/sodium.json",
    ] {
        assert!(names.contains(wanted), "{wanted}");
    }
    assert!(!names.iter().any(|n| n.contains("shaderpacks")));
    // Never carried, whatever is asked.
    for n in &names {
        for banned in [
            "saves",
            "logs",
            "crash-reports",
            "screenshots",
            ".vibelauncher",
            "cache",
        ] {
            assert!(!n.contains(banned), "{n}");
        }
    }
}

#[test]
fn the_archive_is_deterministic() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    game(&root);
    let (a, b) = (dir.path().join("a.mrpack"), dir.path().join("b.mrpack"));
    run(&catalog(), &root, &a, &ExportInclude::DEFAULT).unwrap();
    // Rewrite a file between the exports: the bytes do not depend on dates.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    put(&root, "config/sodium.json", b"{}");
    run(&catalog(), &root, &b, &ExportInclude::DEFAULT).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn a_link_off_the_allowed_hosts_is_bundled_instead() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    put(&root, "mods/sodium.jar", SODIUM);
    let odd = Catalog::on("https://example.com/", &[("sodium.jar", SODIUM)]);
    let out = dir.path().join("p.mrpack");
    let report = run(&odd, &root, &out, &[]).unwrap();
    assert_eq!((report.linked, report.bundled), (0, 1));
    assert_eq!(read(&out, "overrides/mods/sodium.jar"), SODIUM);
}

#[test]
fn an_existing_file_or_a_failed_lookup_leaves_nothing_behind() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    game(&root);
    let out = dir.path().join("p.mrpack");
    fs::write(&out, b"mine").unwrap();
    let err = run(&catalog(), &root, &out, &[]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(fs::read(&out).unwrap(), b"mine");

    let mut down = catalog();
    down.fail = true;
    let other = dir.path().join("q.mrpack");
    let err = run(&down, &root, &other, &[]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    assert!(!other.exists());
    assert!(!dir.path().join("q.mrpack.part").exists());
}

/// Every file under the given folders of `base` except disabled ones, with its bytes.
fn tree(base: &Path, dirs: &[&str]) -> BTreeSet<(String, Vec<u8>)> {
    let mut set = BTreeSet::new();
    for d in dirs {
        let mut stack = vec![base.join(d)];
        while let Some(path) = stack.pop() {
            let Ok(read) = fs::read_dir(&path) else {
                continue;
            };
            for e in read {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if !p.to_string_lossy().ends_with(".disabled") {
                    let rel = p
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    set.insert((rel, fs::read(&p).unwrap()));
                }
            }
        }
    }
    set
}

#[test]
fn an_exported_pack_installs_to_the_same_mods() {
    let dir = Dir::new();
    let root = dir.path().join("game");
    game(&root);
    let out = dir.path().join("p.mrpack");
    run(&catalog(), &root, &out, &ExportInclude::DEFAULT).unwrap();

    let net = client(&[
        (&format!("{CDN}sodium.jar"), SODIUM),
        (&format!("{CDN}lithium.jar"), LITHIUM),
        (&format!("{CDN}rp.zip"), PACK_RP),
    ]);
    let fresh = dir.path().join("fresh");
    let staging = dir.path().join("staging");
    let pack = Pack::open(&out).unwrap();
    install(
        &pack,
        &InstallInput {
            client: &net,
            game_dir: &fresh,
            staging: &staging,
            control: &Control::default(),
        },
    )
    .unwrap();

    let dirs = [
        "mods",
        "resourcepacks",
        "config",
        "defaultconfigs",
        "shaderpacks",
    ];
    let before = tree(&root, &dirs);
    assert_eq!(before.len(), 8);
    assert_eq!(before, tree(&fresh, &dirs));
    assert!(!fresh.join("saves").exists());
    assert!(!fresh.join("options.txt").exists());
}
