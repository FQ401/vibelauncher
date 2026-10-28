//! Updating an installed CurseForge modpack, on synthetic zips and a fake API and CDN.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vl_core::ErrorKind;
use vl_core::catalog::Provider;
use vl_core::install::{MinStability, PackAction, PackChanges};
use vl_net::Control;
use vl_platforms::{CatalogSource, CurseForgeOptions, CurseForgeSource};

use crate::cfpack::{BlockReason, CfPack, install, record, resolve};
use crate::cfpack_tests::{API, Key, World, archive, file, manifest, project, sha1};
use crate::mrpack::{PackKind, PackRecord, PackSourceRecord};
use crate::mrpack_update::{Context, Wanted, apply, check, prepare};
use crate::origin::OriginStore;

fn cdn(id: u64, name: &str) -> String {
    format!(
        "https://edge.forgecdn.net/files/{}/{}/{name}",
        id / 1000,
        id % 1000
    )
}

/// A manifest of `files` with the pack version `version`.
fn versioned(files: &[(u64, u64, bool)], version: &str) -> Value {
    let mut m = manifest(files, "forge-47.2.0");
    m["version"] = json!(version);
    m
}

/// Serves `files` (`file id, project id, name, bytes, whether the API gives a URL`) through the
/// batch answers and the CDN, and the projects as mods.
fn serve(w: &World, files: &[(u64, u64, &str, &[u8], bool)]) {
    let entries: Vec<Value> = files
        .iter()
        .map(|(id, p, name, data, url)| {
            let link = cdn(*id, name);
            file(*id, *p, name, data, url.then_some(link.as_str()))
        })
        .collect();
    w.server
        .post(&format!("{API}/v1/mods/files"), &json!({ "data": entries }));
    let mut projects: Vec<u64> = files.iter().map(|f| f.1).collect();
    projects.sort_unstable();
    projects.dedup();
    let projects: Vec<Value> = projects
        .iter()
        .map(|p| project(*p, &format!("p{p}"), 6))
        .collect();
    w.server
        .post(&format!("{API}/v1/mods"), &json!({ "data": projects }));
    for (id, _, name, data, url) in files {
        if *url {
            w.server.get(&cdn(*id, name), data);
        }
    }
}

struct Installed {
    game: PathBuf,
    meta: PathBuf,
}

/// Installs the pack at `zip` like the backend does: files, overrides, `pack.json` and
/// `resources.json`.
fn install_pack(w: &World, zip: &Path, source: Option<PackSourceRecord>) -> Installed {
    let control = Control::default();
    let game = w.game();
    let meta = w.dir.path().join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    let pack = CfPack::open(zip).unwrap();
    let resolved = resolve(&w.source, &pack, &control).unwrap();
    let staging = w.dir.path().join("staging");
    let report = install(&pack, &resolved, &w.input(&game, &staging, &control)).unwrap();
    let (record, origins) = record(&pack, &report, source).unwrap();
    record.save(&meta).unwrap();
    OriginStore::new(meta.join("resources.json"))
        .save(&origins)
        .unwrap();
    Installed { game, meta }
}

fn ctx<'a>(w: &'a World, i: &'a Installed, control: &'a Control) -> Context<'a> {
    Context {
        source: &w.source,
        curseforge: Some(&w.source),
        client: &w.client,
        game_dir: &i.game,
        meta_dir: &i.meta,
        control,
    }
}

fn action(changes: &PackChanges, path: &str) -> PackAction {
    changes
        .changes
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("no change for {path}: {changes:?}"))
        .action
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn an_update_keeps_what_the_user_changed_and_lists_what_must_be_fetched_by_hand() {
    let w = World::new(Some(crate::cfpack_tests::KEY));
    let old_zip = w.dir.path().join("old.zip");
    archive(
        &old_zip,
        &versioned(
            &[
                (1, 101, true),
                (2, 201, true),
                (3, 301, true),
                (4, 401, false),
                (7, 701, true),
                (8, 801, true),
            ],
            "1.0",
        ),
        &[
            ("overrides/config/x.toml", b"x = 1", 0),
            ("overrides/config/y.toml", b"y = 1", 0),
        ],
    );
    serve(
        &w,
        &[
            (101, 1, "a-1.jar", b"a one", true),
            (201, 2, "b-1.jar", b"b one", true),
            (301, 3, "c-1.jar", b"c one", true),
            (401, 4, "opt-1.jar", b"opt one", true),
            (701, 7, "g-1.jar", b"g one", true),
            (801, 8, "h-1.jar", b"h one", true),
        ],
    );
    let i = install_pack(&w, &old_zip, None);
    let mods = i.game.join("mods");
    assert_eq!(
        files_in(&mods),
        [
            "a-1.jar",
            "b-1.jar",
            "c-1.jar",
            "g-1.jar",
            "h-1.jar",
            "opt-1.jar.disabled"
        ]
    );

    // The user disables b, deletes c and edits one config.
    std::fs::rename(mods.join("b-1.jar"), mods.join("b-1.jar.disabled")).unwrap();
    std::fs::remove_file(mods.join("c-1.jar")).unwrap();
    std::fs::write(i.game.join("config/y.toml"), b"y = mine").unwrap();

    // Version 2: a, b and c change; opt stays; n is new and optional; f is new and cannot be
    // downloaded; g's new file is gone from CurseForge; h's new file cannot be downloaded.
    let new_zip = w.dir.path().join("new.zip");
    archive(
        &new_zip,
        &versioned(
            &[
                (1, 102, true),
                (2, 202, true),
                (3, 302, true),
                (4, 401, false),
                (5, 501, false),
                (6, 601, true),
                (7, 702, true),
                (8, 802, true),
            ],
            "2.0",
        ),
        &[
            ("overrides/config/x.toml", b"x = 2", 0),
            ("overrides/config/y.toml", b"y = 2", 0),
            ("overrides/config/z.toml", b"z = 1", 0),
        ],
    );
    serve(
        &w,
        &[
            (102, 1, "a-2.jar", b"a two", true),
            (202, 2, "b-2.jar", b"b two", true),
            (302, 3, "c-2.jar", b"c two", true),
            (401, 4, "opt-1.jar", b"opt one", true),
            (501, 5, "n-1.jar", b"n one", true),
            (601, 6, "f-1.jar", b"f one", false),
            (802, 8, "h-2.jar", b"h two", false),
        ],
    );
    // f and h are not on the CDN either.
    w.server.get(&cdn(601, "f-1.jar"), b"");
    w.server.get(&cdn(802, "h-2.jar"), b"");

    let control = Control::default();
    let context = ctx(&w, &i, &control);
    let baseline = PackRecord::load(&i.meta).unwrap();
    let wanted = Wanted {
        min: MinStability::Release,
        version: None,
        file: Some(&new_zip),
        reset: false,
    };
    let prepared = prepare(&context, &baseline, &wanted).unwrap().unwrap();
    let ch = prepared.changes.clone();
    assert_eq!((ch.from.as_str(), ch.to.as_str()), ("1.0", "2.0"));
    assert_eq!(action(&ch, "mods/a-1.jar"), PackAction::Remove);
    assert_eq!(action(&ch, "mods/a-2.jar"), PackAction::Add);
    let b2 = ch
        .changes
        .iter()
        .find(|c| c.path == "mods/b-2.jar")
        .unwrap();
    assert!(b2.action == PackAction::Add && b2.disabled, "{b2:?}");
    assert_eq!(action(&ch, "mods/c-2.jar"), PackAction::KeepDeleted);
    let n = ch
        .changes
        .iter()
        .find(|c| c.path == "mods/n-1.jar")
        .unwrap();
    assert!(n.action == PackAction::Add && n.disabled, "{n:?}");
    assert_eq!(action(&ch, "mods/g-1.jar"), PackAction::KeepModified);
    assert_eq!(action(&ch, "config/x.toml"), PackAction::Update);
    assert_eq!(action(&ch, "config/y.toml"), PackAction::SaveNew);
    assert_eq!(action(&ch, "config/z.toml"), PackAction::Add);
    assert!(
        ch.warnings.iter().any(|w| w.contains("projects/7")),
        "g's missing file is named: {:?}",
        ch.warnings
    );
    assert!(ch.changes.iter().all(|c| !c.path.contains("opt-1")));

    let applied = apply(prepared, &context, |_| Ok(())).unwrap();
    assert_eq!(
        files_in(&mods),
        [
            "a-2.jar",
            "b-2.jar.disabled",
            "g-1.jar",
            "h-1.jar",
            "n-1.jar.disabled",
            "opt-1.jar.disabled"
        ],
        "h-1 stays because its new file could not be fetched"
    );
    assert_eq!(std::fs::read(mods.join("a-2.jar")).unwrap(), b"a two");
    assert_eq!(
        std::fs::read(i.game.join("config/x.toml")).unwrap(),
        b"x = 2"
    );
    assert_eq!(
        std::fs::read(i.game.join("config/y.toml")).unwrap(),
        b"y = mine"
    );
    assert_eq!(
        std::fs::read(i.game.join("config/y.toml.pack-new")).unwrap(),
        b"y = 2"
    );
    assert_eq!(
        std::fs::read(i.game.join("config/z.toml")).unwrap(),
        b"z = 1"
    );
    let mut blocked: Vec<(u64, BlockReason)> = applied
        .blocked
        .iter()
        .map(|b| (b.file_id, b.reason))
        .collect();
    blocked.sort_by_key(|b| b.0);
    assert_eq!(
        blocked,
        [
            (601, BlockReason::DistributionDisabled),
            (702, BlockReason::NotFound),
            (802, BlockReason::DistributionDisabled)
        ]
    );

    let record = PackRecord::load(&i.meta).unwrap();
    assert_eq!(record.kind, PackKind::CurseForge);
    assert_eq!(record.pack_version, "2.0");
    assert_eq!(record.blocked.len(), 3);
    let paths: Vec<&str> = record.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"mods/a-2.jar") && paths.contains(&"mods/c-2.jar"));
    assert!(!paths.contains(&"mods/f-1.jar") && !paths.contains(&"mods/h-2.jar"));
    let a2 = record
        .files
        .iter()
        .find(|f| f.path == "mods/a-2.jar")
        .unwrap();
    assert_eq!(a2.sha1, sha1(b"a two"));
    assert_eq!(record.overrides.len(), 3);

    let origins = OriginStore::new(i.meta.join("resources.json"))
        .load()
        .origins;
    let a = origins.iter().find(|o| o.file == "a-2.jar").unwrap();
    assert_eq!(
        (a.provider, a.project_id.as_str(), a.version_id.as_str()),
        (Provider::CurseForge, "1", "102")
    );
    assert_eq!(a.sha1, sha1(b"a two"));
    assert!(origins.iter().any(|o| o.file == "b-2.jar"));
    assert!(
        !origins
            .iter()
            .any(|o| o.file == "a-1.jar" || o.file == "b-1.jar")
    );
    assert!(
        origins.iter().any(|o| o.file == "h-1.jar"),
        "h-1 keeps its record"
    );

    // The backup holds the replaced files.
    assert!(applied.backup.join("mods/a-1.jar").is_file());
}

#[test]
fn without_a_key_an_update_sends_nothing() {
    let w = World::new(Some(crate::cfpack_tests::KEY));
    let zip = w.dir.path().join("p.zip");
    archive(&zip, &versioned(&[(1, 101, true)], "1.0"), &[]);
    serve(&w, &[(101, 1, "a-1.jar", b"a one", true)]);
    let i = install_pack(&w, &zip, None);
    let before = w.server.seen().len();

    let keyless = CurseForgeSource::new(
        w.client.clone(),
        CurseForgeOptions {
            endpoints: vl_curseforge::Endpoints::new(API),
            ..CurseForgeOptions::default()
        },
        std::sync::Arc::new(Key(None)),
    );
    let control = Control::default();
    let context = Context {
        source: &keyless,
        curseforge: Some(&keyless),
        client: &w.client,
        game_dir: &i.game,
        meta_dir: &i.meta,
        control: &control,
    };
    let baseline = PackRecord::load(&i.meta).unwrap();
    let wanted = Wanted {
        min: MinStability::Release,
        version: None,
        file: Some(&zip),
        reset: false,
    };
    let err = prepare(&context, &baseline, &wanted).err().unwrap();
    assert_eq!(err.kind, ErrorKind::NoApiKey);

    // A catalog pack: the check fails the same way.
    let mut from_catalog = baseline.clone();
    from_catalog.source = Some(PackSourceRecord {
        provider: Provider::CurseForge,
        project_id: "9".into(),
        slug: "example".into(),
        version_id: "10".into(),
        version_number: "1.0".into(),
    });
    let err = check(&context, &from_catalog, MinStability::Release).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
    assert_eq!(w.server.seen().len(), before, "no request was sent");
}

#[test]
fn a_catalog_pack_finds_its_newer_zip_and_a_modrinth_file_is_refused() {
    let w = World::new(Some(crate::cfpack_tests::KEY));
    let zip = w.dir.path().join("p.zip");
    archive(&zip, &versioned(&[(1, 101, true)], "1.0"), &[]);
    serve(&w, &[(101, 1, "a-1.jar", b"a one", true)]);
    let source = PackSourceRecord {
        provider: Provider::CurseForge,
        project_id: "9".into(),
        slug: "example".into(),
        version_id: "10".into(),
        version_number: "Pack 10".into(),
    };
    let i = install_pack(&w, &zip, Some(source));
    let pack_file = |id: u64, date: &str, release: u64| {
        json!({
            "id": id, "modId": 9, "displayName": format!("Pack {id}"),
            "fileName": format!("pack-{id}.zip"), "releaseType": release, "fileDate": date,
            "fileLength": 1, "gameVersions": ["1.20.1", "Forge"],
            "hashes": [{"value": "0".repeat(40), "algo": 1}]
        })
    };
    w.server.json(
        &format!("{API}/v1/mods/9/files?index=0&pageSize=50"),
        &json!({"data": [
            pack_file(30, "2026-03-01T00:00:00Z", 2),
            pack_file(20, "2026-02-01T00:00:00Z", 1),
            pack_file(10, "2026-01-01T00:00:00Z", 1)
        ], "pagination": {"index": 0, "pageSize": 50, "totalCount": 3}}),
    );
    let control = Control::default();
    let context = ctx(&w, &i, &control);
    let record = PackRecord::load(&i.meta).unwrap();
    let info = check(&context, &record, MinStability::Release).unwrap();
    assert_eq!(info.current.id, "10");
    assert_eq!(info.latest.map(|l| l.id).as_deref(), Some("20"));
    let info = check(&context, &record, MinStability::Beta).unwrap();
    assert_eq!(info.latest.map(|l| l.id).as_deref(), Some("30"));

    // An installed-from-file CurseForge pack refuses an .mrpack.
    let mut from_file = record.clone();
    from_file.source = None;
    let mrpack = w.dir.path().join("x.mrpack");
    crate::mrpack_tests::archive(&mrpack, "{}", &[]);
    let wanted = Wanted {
        min: MinStability::Release,
        version: None,
        file: Some(&mrpack),
        reset: false,
    };
    let err = prepare(&context, &from_file, &wanted).err().unwrap();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert_eq!(w.source.provider(), Provider::CurseForge);
}

/// Live test against the real API: it runs only when `VL_CURSEFORGE_API_KEY` is set and is
/// skipped (it passes without doing anything) otherwise. An older file of a real modpack is
/// checked for an update, and the update is planned (the new pack is downloaded and its files
/// resolved; nothing is applied).
#[test]
fn live_plan_an_update_of_a_real_pack() {
    use vl_platforms::curseforge::{ApiKeySource, EnvApiKey};
    if EnvApiKey::default().api_key().ok().flatten().is_none() {
        eprintln!("skipped: {} is not set", vl_curseforge::API_KEY_ENV);
        return;
    }
    let client = vl_net::Client::new(vl_net::NetConfig::new("0.0.0")).unwrap();
    let source = CurseForgeSource::new(
        client.clone(),
        CurseForgeOptions::default(),
        std::sync::Arc::new(EnvApiKey::default()),
    );
    let control = Control::default();
    // Fabulously Optimized: a small, long-lived Fabric modpack.
    let versions = source
        .versions(
            "396246",
            &vl_core::catalog::VersionFilter::default(),
            &control,
        )
        .unwrap();
    let releases: Vec<_> = versions
        .iter()
        .filter(|v| v.channel == vl_core::catalog::ReleaseChannel::Release)
        .collect();
    assert!(releases.len() > 3, "{versions:?}");
    let older = releases[3];
    let dir = crate::mrpack_tests::Dir::new();
    let (game, meta) = (dir.path().join("game"), dir.path().join("meta"));
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&meta).unwrap();
    let record = PackRecord {
        format: crate::mrpack::RECORD_FORMAT,
        name: "Fabulously Optimized".into(),
        pack_version: older.version_number.clone(),
        source: Some(PackSourceRecord {
            provider: Provider::CurseForge,
            project_id: "396246".into(),
            slug: "fabulously-optimized".into(),
            version_id: older.id.clone(),
            version_number: older.version_number.clone(),
        }),
        game_version: String::new(),
        loader: None,
        installed_unix: 0,
        files: Vec::new(),
        overrides: Vec::new(),
        kind: PackKind::CurseForge,
        blocked: Vec::new(),
    };
    let context = Context {
        source: &source,
        curseforge: Some(&source),
        client: &client,
        game_dir: &game,
        meta_dir: &meta,
        control: &control,
    };
    let info = check(&context, &record, MinStability::Release).unwrap();
    assert_eq!(info.current.id, older.id);
    let latest = info.latest.expect("a newer release");
    assert_eq!(latest.id, releases[0].id);
    let wanted = Wanted {
        min: MinStability::Release,
        version: None,
        file: None,
        reset: false,
    };
    let prepared = prepare(&context, &record, &wanted).unwrap().unwrap();
    let ch = &prepared.changes;
    assert_eq!(ch.to, latest.number);
    assert!(
        ch.changes
            .iter()
            .any(|c| c.download && c.action == PackAction::Add && c.path.starts_with("mods/")),
        "{ch:?}"
    );
    assert!(ch.download_bytes > 0);
    assert_eq!(
        prepared.loader.as_ref().map(|l| l.0),
        Some(vl_modrinth::mrpack::PackLoader::Fabric)
    );
}
