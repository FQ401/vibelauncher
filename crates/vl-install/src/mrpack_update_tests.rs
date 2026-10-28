//! Tests of the modpack update on synthetic packs of an old and a new version.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha1::Sha1;
use sha2::{Digest, Sha512};
use vl_core::catalog::{
    HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion, Provider, ReleaseChannel,
    SearchPage, SearchQuery, VersionFilter,
};
use vl_core::install::{MinStability, PackAction};
use vl_core::{Error, ErrorKind, Result};
use vl_net::{CancelToken, Client, Control};
use vl_platforms::CatalogSource;

use crate::mrpack::{InstallInput, Pack, PackRecord, PackSourceRecord, install};
use crate::mrpack_origins::resolve;
use crate::mrpack_tests::{CDN, Dir, Packs, archive, client, entry, hex, pack_version};
use crate::mrpack_update::{Context, NEW_SUFFIX, Wanted, apply, check, prepare};
use crate::origin::OriginStore;

/// The pack project plus owners of the mod files by SHA-512.
struct Cat {
    packs: Packs,
    owners: HashMap<String, String>,
}

impl CatalogSource for Cat {
    fn provider(&self) -> Provider {
        Provider::Modrinth
    }
    fn search(&self, q: &SearchQuery, c: &Control) -> Result<SearchPage> {
        self.packs.search(q, c)
    }
    fn project(&self, id: &str, c: &Control) -> Result<Project> {
        self.packs.project(id, c)
    }
    fn versions(&self, p: &str, f: &VersionFilter, c: &Control) -> Result<Vec<ProjectVersion>> {
        self.packs.versions(p, f, c)
    }
    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        if algorithm != HashAlgorithm::Sha512 {
            return Ok(Vec::new());
        }
        Ok(hashes
            .iter()
            .filter_map(|h| {
                let project = self.owners.get(h)?;
                let mut version =
                    pack_version(&format!("ver-{project}"), ReleaseChannel::Release, b"");
                version.project_id = project.clone();
                Some(HashMatch {
                    hash: h.clone(),
                    version,
                })
            })
            .collect())
    }
}

fn sha512(data: &[u8]) -> String {
    hex(&Sha512::digest(data))
}

fn sha1(data: &[u8]) -> String {
    hex(&Sha1::digest(data))
}

fn mod_entry(path: &str, data: &[u8]) -> String {
    entry(path, data, &[&format!("{CDN}{path}")], "required")
}

fn index(version: &str, minecraft: &str, loader: &str, files: &[String]) -> String {
    format!(
        r#"{{"formatVersion":1,"game":"minecraft","versionId":"{version}","name":"Test Pack","files":[{}],"dependencies":{{"minecraft":"{minecraft}","fabric-loader":"{loader}"}}}}"#,
        files.join(",")
    )
}

type Mods<'a> = &'a [(&'a str, &'a [u8])];
type Configs<'a> = &'a [(&'a str, &'a [u8])];

/// Writes a pack archive, returns its bytes.
fn build(
    dir: &Path,
    name: &str,
    version: &str,
    game: (&str, &str),
    mods: Mods<'_>,
    configs: Configs<'_>,
) -> Vec<u8> {
    let files: Vec<String> = mods.iter().map(|(p, d)| mod_entry(p, d)).collect();
    let overrides: Vec<(String, &[u8])> = configs
        .iter()
        .map(|(p, d)| (format!("overrides/{p}"), *d))
        .collect();
    let entries: Vec<(&str, &[u8], u32)> =
        overrides.iter().map(|(p, d)| (p.as_str(), *d, 0)).collect();
    let path = dir.join(name);
    archive(&path, &index(version, game.0, game.1, &files), &entries);
    std::fs::read(path).unwrap()
}

struct World {
    dir: Dir,
    game: PathBuf,
    meta: PathBuf,
    net: Client,
    cat: Cat,
}

impl World {
    fn game(&self) -> PathBuf {
        self.game.clone()
    }
    fn meta(&self) -> PathBuf {
        self.meta.clone()
    }
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.game().join(rel)).ok()
    }
    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.game().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    fn remove(&self, rel: &str) {
        std::fs::remove_file(self.game().join(rel)).unwrap();
    }
    fn ctx<'a>(&'a self, control: &'a Control) -> Context<'a> {
        Context {
            source: &self.cat,
            curseforge: None,
            client: &self.net,
            game_dir: &self.game,
            meta_dir: &self.meta,
            control,
        }
    }
}

const OLD_MODS: &[(&str, &[u8])] = &[
    ("mods/a-1.0.jar", b"a one"),
    ("mods/b-1.0.jar", b"b one"),
    ("mods/c-1.0.jar", b"c one"),
    ("mods/d-1.0.jar", b"d one"),
    ("mods/k-1.0.jar", b"k same"),
];
const NEW_MODS: &[(&str, &[u8])] = &[
    ("mods/a-2.0.jar", b"a two"),
    ("mods/b-2.0.jar", b"b two"),
    ("mods/c-2.0.jar", b"c two"),
    ("mods/e-1.0.jar", b"e new"),
    ("mods/k-1.0.jar", b"k same"),
];
const OLD_CONFIGS: &[(&str, &[u8])] = &[
    ("config/x.toml", b"x1"),
    ("config/y.toml", b"y1"),
    ("config/z.toml", b"z1"),
    ("config/v.toml", b"v1"),
];
const NEW_CONFIGS: &[(&str, &[u8])] = &[
    ("config/x.toml", b"x2"),
    ("config/y.toml", b"y2"),
    ("config/z.toml", b"z1"),
    ("config/v.toml", b"v2"),
    ("config/w.toml", b"w2"),
];

/// A catalog and a transport with the old (`v1`) and the new (`v2`) pack installed from `v1`.
fn installed(game2: (&str, &str)) -> World {
    let dir = Dir::new();
    let packs = dir.path().join("packs");
    std::fs::create_dir_all(&packs).unwrap();
    let v1 = build(
        &packs,
        "v1.mrpack",
        "1",
        ("1.21.1", "0.16.5"),
        OLD_MODS,
        OLD_CONFIGS,
    );
    let v2 = build(&packs, "v2.mrpack", "2", game2, NEW_MODS, NEW_CONFIGS);
    let mut urls: Vec<(String, Vec<u8>)> = OLD_MODS
        .iter()
        .chain(NEW_MODS)
        .map(|(p, d)| (format!("{CDN}{p}"), d.to_vec()))
        .collect();
    urls.push((format!("{CDN}v1.mrpack"), v1.clone()));
    urls.push((format!("{CDN}v2.mrpack"), v2.clone()));
    let refs: Vec<(&str, &[u8])> = urls
        .iter()
        .map(|(u, d)| (u.as_str(), d.as_slice()))
        .collect();
    let net = client(&refs);
    let owners = OLD_MODS
        .iter()
        .chain(NEW_MODS)
        .map(|(p, d)| {
            let name = p.rsplit('/').next().unwrap();
            (sha512(d), format!("P-{}", &name[..1]))
        })
        .collect();
    let cat = Cat {
        packs: Packs {
            versions: vec![
                pack_version("v2", ReleaseChannel::Release, &v2),
                pack_version("v1", ReleaseChannel::Release, &v1),
            ],
            kind: ProjectType::Modpack,
        },
        owners,
    };
    let (game, meta) = (dir.path().join("game"), dir.path().join(".vibelauncher"));
    let world = World {
        dir,
        game,
        meta,
        net,
        cat,
    };

    // The install of v1, as the backend does it.
    let pack = Pack::open(&packs.join("v1.mrpack")).unwrap();
    let control = Control::default();
    let report = install(
        &pack,
        &InstallInput {
            client: &world.net,
            game_dir: &world.game(),
            staging: &world.dir.path().join("staging"),
            control: &control,
        },
    )
    .unwrap();
    let resolved = resolve(&world.cat, &pack, &report, &control).unwrap();
    std::fs::create_dir_all(world.meta()).unwrap();
    OriginStore::new(world.meta().join("resources.json"))
        .save(&resolved.origins)
        .unwrap();
    let known: Vec<String> = resolved
        .origins
        .iter()
        .map(|o| format!("{}/{}", o.kind.dir(), o.file))
        .collect();
    let source = PackSourceRecord {
        provider: Provider::Modrinth,
        project_id: "P1".into(),
        slug: "pack".into(),
        version_id: "v1".into(),
        version_number: "n-v1".into(),
    };
    PackRecord::new(&pack, &report, Some(source))
        .with_known_origins(&known.iter().map(String::as_str).collect())
        .with_overrides(pack.override_hashes().unwrap())
        .save(&world.meta())
        .unwrap();
    world
}

fn wanted() -> Wanted<'static> {
    Wanted {
        min: MinStability::Release,
        ..Wanted::default()
    }
}

fn action(changes: &vl_core::install::PackChanges, path: &str) -> Option<PackAction> {
    changes
        .changes
        .iter()
        .find(|c| c.path == path)
        .map(|c| c.action)
}

/// What the user does to the installed v1: disables b, deletes c and v.toml, adds a mod of
/// their own, edits x.toml and z.toml.
fn user_edits(w: &World) {
    std::fs::rename(
        w.game().join("mods/b-1.0.jar"),
        w.game().join("mods/b-1.0.jar.disabled"),
    )
    .unwrap();
    w.remove("mods/c-1.0.jar");
    w.remove("config/v.toml");
    w.write("mods/mine.jar", b"my own mod");
    w.write("config/x.toml", b"x mine");
    w.write("config/z.toml", b"z mine");
}

#[test]
fn an_update_keeps_what_the_user_changed() {
    let w = installed(("1.21.1", "0.16.5"));
    user_edits(&w);
    let control = Control::default();
    let ctx = w.ctx(&control);
    let record = PackRecord::load(&w.meta()).unwrap();

    let info = check(&ctx, &record, MinStability::Release).unwrap();
    assert_eq!(info.current.id, "v1");
    assert_eq!(info.latest.as_ref().unwrap().id, "v2");

    let prepared = prepare(&ctx, &record, &wanted()).unwrap().unwrap();
    let ch = &prepared.changes;
    assert_eq!((ch.from.as_str(), ch.to.as_str()), ("n-v1", "n-v2"));
    assert_eq!(action(ch, "mods/a-2.0.jar"), Some(PackAction::Add));
    assert_eq!(action(ch, "mods/a-1.0.jar"), Some(PackAction::Remove));
    assert_eq!(action(ch, "mods/d-1.0.jar"), Some(PackAction::Remove));
    assert_eq!(action(ch, "mods/e-1.0.jar"), Some(PackAction::Add));
    let b2 = ch
        .changes
        .iter()
        .find(|c| c.path == "mods/b-2.0.jar")
        .unwrap();
    assert_eq!((b2.action, b2.disabled), (PackAction::Add, true));
    assert_eq!(action(ch, "mods/c-2.0.jar"), Some(PackAction::KeepDeleted));
    assert_eq!(action(ch, "config/x.toml"), Some(PackAction::SaveNew));
    assert_eq!(action(ch, "config/y.toml"), Some(PackAction::Update));
    assert_eq!(action(ch, "config/v.toml"), Some(PackAction::KeepDeleted));
    assert_eq!(action(ch, "config/w.toml"), Some(PackAction::Add));
    // z.toml: the pack did not change it, so there is nothing to say about the user's copy.
    assert_eq!(action(ch, "config/z.toml"), None);
    assert!(ch.unchanged >= 2);
    // Nothing is touched by planning.
    assert!(w.read("mods/a-1.0.jar").is_some());

    let applied = apply(prepared, &ctx, |_| Ok(())).unwrap();
    assert!(w.read("mods/a-1.0.jar").is_none());
    assert_eq!(w.read("mods/a-2.0.jar").unwrap(), b"a two");
    assert!(w.read("mods/b-1.0.jar.disabled").is_none());
    assert_eq!(w.read("mods/b-2.0.jar.disabled").unwrap(), b"b two");
    assert!(w.read("mods/b-2.0.jar").is_none());
    assert!(w.read("mods/c-1.0.jar").is_none() && w.read("mods/c-2.0.jar").is_none());
    assert!(w.read("mods/d-1.0.jar").is_none());
    assert_eq!(w.read("mods/e-1.0.jar").unwrap(), b"e new");
    assert_eq!(w.read("mods/k-1.0.jar").unwrap(), b"k same");
    assert_eq!(w.read("mods/mine.jar").unwrap(), b"my own mod");
    assert_eq!(w.read("config/x.toml").unwrap(), b"x mine");
    assert_eq!(
        w.read(&format!("config/x.toml{NEW_SUFFIX}")).unwrap(),
        b"x2"
    );
    assert_eq!(w.read("config/y.toml").unwrap(), b"y2");
    assert_eq!(w.read("config/z.toml").unwrap(), b"z mine");
    assert!(w.read(&format!("config/z.toml{NEW_SUFFIX}")).is_none());
    assert!(w.read("config/v.toml").is_none());
    assert_eq!(w.read("config/w.toml").unwrap(), b"w2");

    // The previous versions of the changed files are in the backup, user files are not.
    assert_eq!(
        std::fs::read(applied.backup.join("mods/a-1.0.jar")).unwrap(),
        b"a one"
    );
    assert_eq!(
        std::fs::read(applied.backup.join("config/y.toml")).unwrap(),
        b"y1"
    );
    assert!(!applied.backup.join("mods/mine.jar").exists());

    // The records describe the new pack.
    let record = PackRecord::load(&w.meta()).unwrap();
    assert_eq!(record.source.as_ref().unwrap().version_id, "v2");
    assert_eq!(record.pack_version, "2");
    assert!(
        record
            .files
            .iter()
            .any(|f| f.path == "mods/a-2.0.jar" && f.origin_known)
    );
    assert!(
        record
            .overrides
            .iter()
            .any(|o| o.path == "config/x.toml" && o.sha1 == sha1(b"x2"))
    );
    let origins = OriginStore::new(w.meta().join("resources.json"))
        .load()
        .origins;
    assert!(
        origins
            .iter()
            .any(|o| o.file == "a-2.0.jar" && o.project_id == "P-a")
    );
    assert!(!origins.iter().any(|o| o.file == "a-1.0.jar"));

    // A second update has nothing left to do and does not touch the user's choices again.
    let again = check(&ctx, &record, MinStability::Release).unwrap();
    assert!(again.latest.is_none());
    assert!(prepare(&ctx, &record, &wanted()).unwrap().is_none());
}

#[test]
fn a_failure_after_the_files_restores_everything() {
    let w = installed(("1.21.1", "0.16.5"));
    user_edits(&w);
    let control = Control::default();
    let ctx = w.ctx(&control);
    let record = PackRecord::load(&w.meta()).unwrap();
    let before_record = std::fs::read(w.meta().join("pack.json")).unwrap();
    let before_origins = std::fs::read(w.meta().join("resources.json")).unwrap();

    let prepared = prepare(&ctx, &record, &wanted()).unwrap().unwrap();
    let err = apply(prepared, &ctx, |_| {
        Err(Error::new(ErrorKind::Io, "disk full"))
    })
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Io);
    assert_eq!(w.read("mods/a-1.0.jar").unwrap(), b"a one");
    assert!(w.read("mods/a-2.0.jar").is_none());
    assert!(w.read("mods/d-1.0.jar").is_some());
    assert!(w.read("mods/b-1.0.jar.disabled").is_some());
    assert!(w.read("mods/e-1.0.jar").is_none());
    assert_eq!(w.read("config/y.toml").unwrap(), b"y1");
    assert_eq!(w.read("config/x.toml").unwrap(), b"x mine");
    assert!(w.read(&format!("config/x.toml{NEW_SUFFIX}")).is_none());
    assert!(w.read("config/w.toml").is_none());
    assert_eq!(
        std::fs::read(w.meta().join("pack.json")).unwrap(),
        before_record
    );
    assert_eq!(
        std::fs::read(w.meta().join("resources.json")).unwrap(),
        before_origins
    );
    // The failed attempt leaves no backup folder behind.
    let left = std::fs::read_dir(w.meta().join("backup"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(left, 0);
}

#[test]
fn a_cancel_before_the_changes_leaves_the_instance_alone() {
    let w = installed(("1.21.1", "0.16.5"));
    let record = PackRecord::load(&w.meta()).unwrap();
    let token = CancelToken::new();
    let control = Control::with_cancel(token.clone());
    let ctx = w.ctx(&control);
    let prepared = prepare(&ctx, &record, &wanted()).unwrap().unwrap();
    token.cancel();
    let err = apply(prepared, &ctx, |_| Ok(())).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert_eq!(w.read("mods/a-1.0.jar").unwrap(), b"a one");
    assert!(w.read("mods/a-2.0.jar").is_none());
    assert_eq!(PackRecord::load(&w.meta()).unwrap().pack_version, "1");
}

#[test]
fn a_reset_puts_the_pack_back_but_keeps_the_users_own_files() {
    let w = installed(("1.21.1", "0.16.5"));
    user_edits(&w);
    let control = Control::default();
    let ctx = w.ctx(&control);
    let record = PackRecord::load(&w.meta()).unwrap();
    let prepared = prepare(
        &ctx,
        &record,
        &Wanted {
            reset: true,
            ..wanted()
        },
    )
    .unwrap()
    .unwrap();
    assert!(prepared.changes.reset);
    apply(prepared, &ctx, |_| Ok(())).unwrap();
    assert_eq!(w.read("mods/b-2.0.jar").unwrap(), b"b two");
    assert_eq!(w.read("mods/c-2.0.jar").unwrap(), b"c two");
    assert_eq!(w.read("config/x.toml").unwrap(), b"x2");
    assert_eq!(w.read("config/z.toml").unwrap(), b"z1");
    assert_eq!(w.read("config/v.toml").unwrap(), b"v2");
    assert!(w.read("mods/a-1.0.jar").is_none());
    assert!(w.read("mods/b-1.0.jar.disabled").is_none());
    assert_eq!(w.read("mods/mine.jar").unwrap(), b"my own mod");
    assert!(w.read(&format!("config/x.toml{NEW_SUFFIX}")).is_none());
    // The original x.toml is in the backup.
    let backups: Vec<_> = std::fs::read_dir(w.meta().join("backup"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        std::fs::read(backups[0].join("config/x.toml")).unwrap(),
        b"x mine"
    );
}

#[test]
fn a_mod_the_user_swapped_for_another_version_is_not_brought_back() {
    let w = installed(("1.21.1", "0.16.5"));
    // The user replaced a-1.0 by a build of their own that the catalog also knows as project a.
    let own: &[u8] = b"a three";
    w.remove("mods/a-1.0.jar");
    w.write("mods/a-3.0.jar", own);
    let mut store = OriginStore::new(w.meta().join("resources.json"))
        .load()
        .origins;
    let mut o = store
        .iter()
        .find(|o| o.file == "a-1.0.jar")
        .unwrap()
        .clone();
    o.file = "a-3.0.jar".into();
    o.sha1 = sha1(own);
    o.version_id = "own".into();
    store.push(o);
    OriginStore::new(w.meta().join("resources.json"))
        .save(&store)
        .unwrap();

    let control = Control::default();
    let ctx = w.ctx(&control);
    let record = PackRecord::load(&w.meta()).unwrap();
    let prepared = prepare(&ctx, &record, &wanted()).unwrap().unwrap();
    assert_eq!(
        action(&prepared.changes, "mods/a-2.0.jar"),
        Some(PackAction::KeepDeleted)
    );
    apply(prepared, &ctx, |_| Ok(())).unwrap();
    assert!(w.read("mods/a-2.0.jar").is_none());
    assert_eq!(w.read("mods/a-3.0.jar").unwrap(), own);
}

#[test]
fn a_pack_from_a_file_updates_from_a_new_file_and_reports_game_and_loader_changes() {
    let w = installed(("1.21.4", "0.16.9"));
    let mut record = PackRecord::load(&w.meta()).unwrap();
    record.source = None;
    record.save(&w.meta()).unwrap();
    let control = Control::default();
    let ctx = w.ctx(&control);
    // Without a file there is nothing to update from.
    let err = prepare(&ctx, &record, &wanted()).err().unwrap();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    let info = check(&ctx, &record, MinStability::Release).unwrap();
    assert!(info.from_file && info.latest.is_none());

    let file = w.dir.path().join("packs/v2.mrpack");
    let prepared = prepare(
        &ctx,
        &record,
        &Wanted {
            file: Some(&file),
            ..wanted()
        },
    )
    .unwrap()
    .unwrap();
    let ch = &prepared.changes;
    let game = ch.game_version.as_ref().unwrap();
    assert_eq!((game.from.as_str(), game.to.as_str()), ("1.21.1", "1.21.4"));
    assert_eq!(ch.loader.as_ref().unwrap().to, "fabric-loader 0.16.9");
    assert_eq!(prepared.new_game_version, "1.21.4");
    let mut ran = false;
    apply(prepared, &ctx, |p| {
        ran = p.new_game_version == "1.21.4";
        Ok(())
    })
    .unwrap();
    assert!(ran);
    assert_eq!(w.read("mods/a-2.0.jar").unwrap(), b"a two");
    let record = PackRecord::load(&w.meta()).unwrap();
    assert!(record.source.is_none());
    assert_eq!(record.game_version, "1.21.4");
}

#[test]
fn a_record_without_override_hashes_keeps_every_differing_config() {
    let w = installed(("1.21.1", "0.16.5"));
    let mut record = PackRecord::load(&w.meta()).unwrap();
    record.overrides.clear();
    record.save(&w.meta()).unwrap();
    let control = Control::default();
    let ctx = w.ctx(&control);
    let prepared = prepare(&ctx, &record, &wanted()).unwrap().unwrap();
    // y.toml is untouched by the user but cannot be told from a changed one.
    assert_eq!(
        action(&prepared.changes, "config/y.toml"),
        Some(PackAction::SaveNew)
    );
    assert!(
        prepared
            .changes
            .warnings
            .iter()
            .any(|w| w.contains("configs"))
    );
    apply(prepared, &ctx, |_| Ok(())).unwrap();
    assert_eq!(w.read("config/y.toml").unwrap(), b"y1");
    assert_eq!(
        w.read(&format!("config/y.toml{NEW_SUFFIX}")).unwrap(),
        b"y2"
    );
}

#[test]
fn the_channel_limits_which_pack_version_counts_as_an_update() {
    let mut w = installed(("1.21.1", "0.16.5"));
    // v2 is only a beta.
    w.cat.packs.versions[0].channel = ReleaseChannel::Beta;
    let control = Control::default();
    let ctx = w.ctx(&control);
    let record = PackRecord::load(&w.meta()).unwrap();
    assert!(
        check(&ctx, &record, MinStability::Release)
            .unwrap()
            .latest
            .is_none()
    );
    assert_eq!(
        check(&ctx, &record, MinStability::Beta)
            .unwrap()
            .latest
            .unwrap()
            .id,
        "v2"
    );
    assert!(prepare(&ctx, &record, &wanted()).unwrap().is_none());
    // A version can still be asked for by id.
    let prepared = prepare(
        &ctx,
        &record,
        &Wanted {
            version: Some("v2"),
            ..wanted()
        },
    )
    .unwrap();
    assert!(prepared.is_some());
}
