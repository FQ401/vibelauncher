//! Data packs: installed into a world, never into the instance; updates and origins per world.

use super::*;
use crate::Selection;
use crate::updates::{UpdateInput, UpdateOptions, check, identify, plan_update};

/// A zip with a `pack.mcmeta`, as a data pack file.
fn pack(format: u32) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("pack.mcmeta", zip::write::SimpleFileOptions::default())
        .unwrap();
    let text = format!(r#"{{"pack":{{"pack_format":{format},"description":"test"}}}}"#);
    zip.write_all(text.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

fn pack_ver(id: &str, project: &str, number: &str, format: u32) -> Ver {
    let mut v = ver(id, project, number)
        .loaders(&["datapack"])
        .file_name(&format!("{}-{number}.zip", project.to_lowercase()));
    v.bytes = pack(format);
    v
}

fn world_target() -> Target {
    Target {
        world: true,
        ..fabric()
    }
}

/// The world `name` of the test instance, as the backend opens it.
fn open_world(w: &World, name: &str) -> (Resources, OriginStore) {
    let dir = w.dir.path().join("game").join("saves").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("level.dat"), b"stub").unwrap();
    let resources = w.resources.world(name).unwrap();
    let origins = OriginStore::new(
        resources
            .root()
            .join(".vibelauncher")
            .join("resources.json"),
    );
    (resources, origins)
}

fn plan_in(w: &World, world: &(Resources, OriginStore), project: &str) -> Result<InstallPlan> {
    let target = world_target();
    let input = PlanInput {
        source: &w.catalog,
        resources: &world.0,
        origins: &world.1,
        target: &target,
        control: &w.control,
    };
    plan(&input, project, None, MinStability::Beta)
}

fn install_in(w: &World, world: &(Resources, OriginStore), plan: &InstallPlan) {
    let staging = w.staging();
    let input = ApplyInput {
        client: &w.client,
        resources: &world.0,
        origins: &world.1,
        staging: &staging,
    };
    execute(&input, plan, &w.control).unwrap();
}

fn files_in(world: &Resources) -> Vec<String> {
    world
        .list(ResourceKind::DataPacks)
        .unwrap()
        .into_iter()
        .map(|e| e.file)
        .collect()
}

#[test]
fn a_data_pack_is_not_installed_into_the_instance() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(pack_ver("c1", "Caves", "1", 48));
    let w = World::new(catalog);
    let err = w.plan("caves").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("into a world"), "{err}");
    assert!(!w.resources.dir(ResourceKind::DataPacks).exists());
}

#[test]
fn installs_into_the_chosen_world_only() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(pack_ver("c1", "Caves", "1", 48));
    let w = World::new(catalog);
    let first = open_world(&w, "Мир с пробелом");
    let second = open_world(&w, "Other");

    let p = plan_in(&w, &first, "caves").unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].kind, ResourceKind::DataPacks);
    install_in(&w, &first, &p);

    assert_eq!(files_in(&first.0), ["caves-1.zip"]);
    assert!(files_in(&second.0).is_empty());
    assert!(
        w.dir
            .path()
            .join("game/saves/Мир с пробелом/datapacks/caves-1.zip")
            .is_file()
    );
    assert!(!w.resources.dir(ResourceKind::DataPacks).exists());
    // The origin lives with the world.
    let records = first.1.load().origins;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].project_id, "Caves");
    assert!(second.1.load().origins.is_empty());
    // The pack's metadata is listed.
    let listed = first.0.list(ResourceKind::DataPacks).unwrap();
    assert_eq!(listed[0].pack.as_ref().unwrap().pack_format, Some(48));
    // Installing again is a no-op.
    let again = plan_in(&w, &first, "caves").unwrap();
    assert!(again.downloads().next().is_none());
}

#[test]
fn the_same_pack_can_be_in_two_worlds() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(pack_ver("c1", "Caves", "1", 48));
    let w = World::new(catalog);
    let a = open_world(&w, "A");
    let b = open_world(&w, "B");
    for world in [&a, &b] {
        let p = plan_in(&w, world, "caves").unwrap();
        assert!(p.downloads().next().is_some());
        install_in(&w, world, &p);
    }
    assert_eq!(files_in(&a.0), ["caves-1.zip"]);
    assert_eq!(files_in(&b.0), ["caves-1.zip"]);
}

#[test]
fn a_mod_with_a_data_pack_version_installs_as_a_data_pack_in_a_world() {
    let catalog = Catalog::default()
        .project("Both")
        .version(ver("m1", "Both", "1").date("2025-03-01T00:00:00Z"))
        .version(pack_ver("d1", "Both", "1", 48).date("2025-01-01T00:00:00Z"));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    let p = plan_in(&w, &world, "both").unwrap();
    assert_eq!(p.items[0].version_id, "d1");
    assert_eq!(p.items[0].kind, ResourceKind::DataPacks);
    // In the instance the same project is the mod.
    assert_eq!(w.plan("both").unwrap().items[0].version_id, "m1");
}

#[test]
fn a_mod_without_a_data_pack_version_says_so() {
    let catalog = Catalog::default()
        .project("OnlyMod")
        .version(ver("m1", "OnlyMod", "1"));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    let err = plan_in(&w, &world, "onlymod").unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    assert!(err.message.contains("as a data pack"), "{err}");
}

#[test]
fn other_project_types_are_refused_in_a_world() {
    let catalog = Catalog::default()
        .typed("Pretty", ProjectType::ResourcePack)
        .version(ver("r1", "Pretty", "1"));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    let err = plan_in(&w, &world, "pretty").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("only data packs"), "{err}");
}

#[test]
fn a_mod_dependency_of_a_data_pack_is_not_installed() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .project("Lib")
        .version(pack_ver("c1", "Caves", "1", 48).dep("Lib", DependencyKind::Required))
        .version(ver("l1", "Lib", "1"));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    let p = plan_in(&w, &world, "caves").unwrap();
    assert_eq!(titles(&p), ["Caves"]);
    assert!(p.warnings.iter().any(|m| m.contains("needs Lib")), "{p:?}");
}

#[test]
fn updates_and_origins_work_per_world() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(pack_ver("c1", "Caves", "1", 48))
        .version(pack_ver("c2", "Caves", "2", 49).date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    // A pack added by hand.
    let dir = world.0.dir(ResourceKind::DataPacks);
    std::fs::create_dir_all(&dir).unwrap();
    let old = pack_ver("c1", "Caves", "1", 48);
    std::fs::write(dir.join("whatever.zip"), &old.bytes).unwrap();

    let target = world_target();
    let meta = world.0.root().join(".vibelauncher");
    let input = UpdateInput {
        source: &w.catalog,
        resources: &world.0,
        origins: &world.1,
        target: &target,
        meta_dir: &meta,
        control: &w.control,
    };
    let found = identify(&input).unwrap();
    assert_eq!(found.identified.len(), 1);
    assert_eq!(found.identified[0].kind, ResourceKind::DataPacks);

    let report = check(&input, UpdateOptions::default()).unwrap();
    assert_eq!(report.updates.len(), 1, "{report:?}");
    assert_eq!(report.updates[0].file, "whatever.zip");
    assert_eq!(report.updates[0].latest.number, "2");

    let p = plan_update(&input, UpdateOptions::default(), Selection::All, None).unwrap();
    install_in(&w, &world, &p);
    assert_eq!(files_in(&world.0), ["caves-2.zip"]);
}

#[test]
fn a_disabled_pack_stays_disabled_and_keeps_its_origin() {
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(pack_ver("c1", "Caves", "1", 48));
    let w = World::new(catalog);
    let world = open_world(&w, "W");
    let p = plan_in(&w, &world, "caves").unwrap();
    install_in(&w, &world, &p);
    world
        .0
        .set_enabled(ResourceKind::DataPacks, "caves-1.zip", false)
        .unwrap();
    let again = plan_in(&w, &world, "caves").unwrap();
    assert!(again.downloads().next().is_none(), "{again:?}");
    let listed = world.0.list(ResourceKind::DataPacks).unwrap();
    assert!(!listed[0].enabled);
    assert_eq!(listed[0].pack.as_ref().unwrap().pack_format, Some(48));
}
