//! Origin discovery, update checks, updates and version changes against a synthetic catalog.

use std::sync::atomic::Ordering;

use super::*;
use crate::mrpack::{PackFileRecord, PackRecord, RECORD_FORMAT};
use crate::updates::{Selection, UpdateInput, UpdateOptions, check, identify, plan_update};
use vl_core::install::UncheckedReason;

fn meta(w: &World) -> PathBuf {
    w.dir.path().join(".vibelauncher")
}

fn input<'a>(w: &'a World, target: &'a Target, meta: &'a Path) -> UpdateInput<'a> {
    UpdateInput {
        source: &w.catalog,
        resources: &w.resources,
        origins: &w.origins,
        target,
        meta_dir: meta,
        control: &w.control,
    }
}

fn two_versions() -> (Ver, Catalog) {
    let old = ver("v1", "A", "1");
    let catalog = Catalog::default()
        .project("A")
        .version(old.clone())
        .version(ver("v2", "A", "2").date("2025-02-01T00:00:00Z"))
        .version(
            ver("v3", "A", "3-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-03-01T00:00:00Z"),
        );
    (old, catalog)
}

#[test]
fn identifies_hand_added_files_by_hash_and_records_them() {
    let (old, catalog) = two_versions();
    let w = World::new(catalog);
    w.put_mod("whatever.jar", &old.bytes);
    w.put_mod("mine.jar", &jar("mine", "1"));
    let (target, meta) = (fabric(), meta(&w));
    let report = identify(&input(&w, &target, &meta)).unwrap();
    assert_eq!(report.identified.len(), 1);
    assert_eq!(report.identified[0].file, "whatever.jar");
    assert_eq!(report.identified[0].project_id, "A");
    assert_eq!(report.identified[0].version.number, "1");
    assert_eq!(report.unknown.len(), 1);
    let records = w.origins.load().origins;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].version_id, "v1");
    assert_eq!(records[0].file, "whatever.jar");
    assert!(!records[0].dependency);

    // A second run finds nothing new.
    let again = identify(&input(&w, &target, &meta)).unwrap();
    assert!(again.identified.is_empty());
    assert_eq!(again.already_known, 1);
}

#[test]
fn a_check_uses_one_lookup_and_one_update_request_and_offers_only_releases() {
    let (old, catalog) = two_versions();
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    w.put_mod("mine.jar", &jar("mine", "1"));
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert_eq!(report.updates.len(), 1);
    let u = &report.updates[0];
    assert_eq!(
        (u.file.as_str(), u.current.number.as_str()),
        ("a-1.jar", "1")
    );
    assert_eq!(u.latest.number, "2");
    assert!(u.enabled && !u.from_pack);
    assert_eq!(report.unchecked.len(), 1);
    assert_eq!(report.unchecked[0].file, "mine.jar");
    assert_eq!(report.unchecked[0].reason, UncheckedReason::NoOrigin);
    assert_eq!(w.catalog.lookup_calls.load(Ordering::Relaxed), 1);
    assert_eq!(w.catalog.update_calls.load(Ordering::Relaxed), 1);
    let asks = w.catalog.update_asks.lock().unwrap();
    assert_eq!(asks[0].0.loaders, ["fabric"]);
    assert_eq!(asks[0].0.game_versions, ["1.21.4"]);
    assert_eq!(asks[0].1, [ReleaseChannel::Release]);
    drop(asks);

    let beta = UpdateOptions {
        min: MinStability::Beta,
        ..UpdateOptions::default()
    };
    let report = check(&input(&w, &target, &meta), beta).unwrap();
    assert_eq!(report.updates[0].latest.number, "3-beta");
}

#[test]
fn an_update_never_goes_back_to_an_older_version() {
    let (_, catalog) = two_versions();
    let newest = ver("v3", "A", "3-beta")
        .channel(ReleaseChannel::Beta)
        .date("2025-03-01T00:00:00Z");
    let w = World::new(catalog);
    w.put_mod("a-3.jar", &newest.bytes);
    let (target, meta) = (fabric(), meta(&w));
    // Releases only: the newest release (v2) is older than the installed beta.
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(report.updates.is_empty());
    assert_eq!(report.up_to_date, 1);
}

#[test]
fn other_kinds_are_asked_with_their_own_loaders_and_an_unfit_answer_is_dropped() {
    let pack_old = ver("p1", "Pack", "1").loaders(&["minecraft"]);
    let catalog = Catalog::default()
        .typed("Pack", ProjectType::ResourcePack)
        .version(pack_old.clone())
        .version(
            ver("p2", "Pack", "2")
                .loaders(&["minecraft"])
                .date("2025-02-01T00:00:00Z"),
        )
        .project("A")
        .version(ver("v1", "A", "1"))
        // The catalog answers with a version for another game: not an update here.
        .version(
            ver("v2", "A", "2")
                .game("1.20.1")
                .date("2025-02-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    std::fs::create_dir_all(w.resources.dir(ResourceKind::ResourcePacks)).unwrap();
    std::fs::write(
        w.resources
            .dir(ResourceKind::ResourcePacks)
            .join("pack-1.zip"),
        &pack_old.bytes,
    )
    .unwrap();
    w.put_mod("a-1.jar", &ver("v1", "A", "1").bytes);
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert_eq!(w.catalog.lookup_calls.load(Ordering::Relaxed), 1);
    assert_eq!(w.catalog.update_calls.load(Ordering::Relaxed), 2);
    let asks = w.catalog.update_asks.lock().unwrap();
    assert!(asks.iter().any(|(f, _)| f.loaders == ["minecraft"]));
    drop(asks);
    assert_eq!(report.updates.len(), 1, "{report:?}");
    assert_eq!(report.updates[0].kind, ResourceKind::ResourcePacks);
    assert_eq!(report.updates[0].latest.number, "2");
}

#[test]
fn a_mod_in_an_instance_without_a_loader_cannot_be_checked() {
    let (old, catalog) = two_versions();
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    let target = Target {
        game_version: "1.21.4".into(),
        loader: None,
        world: false,
    };
    let meta = meta(&w);
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert_eq!(report.unchecked.len(), 1);
    assert_eq!(report.unchecked[0].reason, UncheckedReason::NoLoader);
    assert_eq!(w.catalog.update_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn pack_files_are_skipped_unless_asked_for() {
    let (old, catalog) = two_versions();
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    let meta = meta(&w);
    let record = PackRecord {
        format: RECORD_FORMAT,
        name: "Pack".into(),
        pack_version: "1".into(),
        source: None,
        game_version: "1.21.4".into(),
        loader: None,
        installed_unix: 0,
        files: vec![PackFileRecord {
            path: "mods/a-1.jar".into(),
            sha1: sha1(&old.bytes),
            sha512: sha512(&old.bytes),
            size: old.bytes.len() as u64,
            optional: false,
            origin_known: false,
        }],
        overrides: Vec::new(),
        kind: Default::default(),
        blocked: Vec::new(),
    };
    record.save(&meta).unwrap();
    let target = fabric();
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(report.updates.is_empty());
    assert_eq!(report.from_pack.len(), 1);
    assert_eq!(w.catalog.update_calls.load(Ordering::Relaxed), 0);

    let plan_all = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    assert!(plan_all.items.is_empty());
    let named = ["a-1.jar".to_owned()];
    let err = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::Files(&named),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);

    let with = UpdateOptions {
        include_pack_files: true,
        ..UpdateOptions::default()
    };
    let report = check(&input(&w, &target, &meta), with).unwrap();
    assert_eq!(report.updates.len(), 1);
    assert!(report.updates[0].from_pack);
}

#[test]
fn updating_all_swaps_files_keeps_disabled_ones_disabled_and_adds_new_dependencies() {
    let a1 = ver("a1", "A", "1");
    let b1 = ver("b1", "B", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .project("Lib")
        .version(a1.clone())
        .version(
            ver("a2", "A", "2")
                .date("2025-02-01T00:00:00Z")
                .dep("Lib", DependencyKind::Required),
        )
        .version(b1.clone())
        .version(ver("b2", "B", "2").date("2025-02-01T00:00:00Z"))
        .version(ver("l1", "Lib", "1"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar.disabled", &b1.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let plan = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    let mut titles = titles(&plan);
    titles.sort();
    assert_eq!(titles, ["A", "B", "Lib"]);
    // One request for the hashes: the plan reuses the scan of the check.
    assert_eq!(w.catalog.lookup_calls.load(Ordering::Relaxed), 1);
    let report = w.install(&plan).unwrap();
    assert_eq!(report.replaced.len(), 2);
    assert_eq!(w.mod_files(), ["a-2.jar", "b-2.jar.disabled", "lib-1.jar"]);
    let records = w.origins.load().origins;
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .any(|o| o.file == "lib-1.jar" && o.dependency)
    );

    // Now everything is current.
    let again = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(again.updates.is_empty());
    assert_eq!(again.up_to_date, 3);
}

#[test]
fn updates_selected_by_file_name_or_mod_id_and_unknown_names_fail() {
    let a1 = ver("a1", "A", "1");
    let b1 = ver("b1", "B", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(a1.clone())
        .version(ver("a2", "A", "2").date("2025-02-01T00:00:00Z"))
        .version(b1.clone())
        .version(ver("b2", "B", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar", &b1.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let select = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
        plan_update(
            &input(&w, &target, &meta),
            UpdateOptions::default(),
            Selection::Files(&names),
            None,
        )
    };
    assert_eq!(titles(&select(&["a-1.jar"]).unwrap()), ["A"]);
    assert_eq!(titles(&select(&["b"]).unwrap()), ["B"]);
    let both = select(&["a", "B"]).unwrap();
    assert_eq!(both.items.len(), 2);
    assert_eq!(select(&["nope"]).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_selected_mod_that_is_needed_by_another_update_gets_its_own_version() {
    let a1 = ver("a1", "A", "1");
    let l1 = ver("l1", "Lib", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("Lib")
        .version(a1.clone())
        .version(
            ver("a2", "A", "2")
                .date("2025-02-01T00:00:00Z")
                .dep("Lib", DependencyKind::Required),
        )
        .version(l1.clone())
        .version(ver("l2", "Lib", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("lib-1.jar", &l1.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let plan = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    let lib = plan.items.iter().find(|i| i.title == "Lib").unwrap();
    assert_eq!(lib.version_id, "l2");
    assert!(matches!(lib.action, PlanAction::Replace { .. }));
    assert_eq!(plan.items.len(), 2);
}

#[test]
fn a_version_can_be_chosen_explicitly_even_an_older_one() {
    let (_, catalog) = two_versions();
    let newest = ver("v2", "A", "2").date("2025-02-01T00:00:00Z");
    let w = World::new(catalog);
    w.put_mod("a-2.jar.disabled", &newest.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let named = ["a".to_owned()];
    let plan = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::Files(&named),
        Some("v1"),
    )
    .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].version_id, "v1");
    assert!(
        matches!(&plan.items[0].action, PlanAction::Replace { old_version, .. }
        if old_version.as_deref() == Some("2"))
    );
    w.install(&plan).unwrap();
    assert_eq!(w.mod_files(), ["a-1.jar.disabled"]);

    // A version needs exactly one mod.
    let err = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        Some("v1"),
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    // A version that does not exist fails the plan.
    let err = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::Files(&named),
        Some("missing"),
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

/// A record of `file` with these bytes, installed from `provider`.
fn recorded(file: &str, bytes: &[u8], provider: Provider, project: &str, version: &str) -> Origin {
    Origin {
        kind: ResourceKind::Mods,
        file: file.to_owned(),
        sha1: sha1(bytes),
        sha512: None,
        size: bytes.len() as u64,
        provider,
        project_id: project.to_owned(),
        slug: None,
        version_id: version.to_owned(),
        version_number: None,
        installed_unix: 0,
        dependency: false,
    }
}

#[test]
fn curseforge_identifies_files_by_fingerprint_and_keeps_records_of_other_platforms() {
    let a1 = ver("11", "100", "1");
    let b1 = ver("21", "200", "1");
    let catalog = Catalog::default()
        .curseforge()
        .project("100")
        .project("200")
        .version(a1.clone())
        .version(b1.clone());
    let w = World::new(catalog);
    w.put_mod("a.jar", &a1.bytes);
    w.put_mod("b.jar", &b1.bytes);
    w.put_mod("mine.jar", &jar("mine", "1"));
    // b.jar was installed from Modrinth: that record stays.
    w.origins
        .save(&[recorded(
            "b.jar",
            &b1.bytes,
            Provider::Modrinth,
            "mr-b",
            "mr-b1",
        )])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = identify(&input(&w, &target, &meta)).unwrap();
    let identified: Vec<&str> = report.identified.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(identified, ["a.jar"]);
    assert_eq!(report.already_known, 1);
    assert_eq!(report.unknown.len(), 1);
    let mut records = w.origins.load().origins;
    records.sort_by(|x, y| x.file.cmp(&y.file));
    assert_eq!(records.len(), 2);
    assert_eq!(
        (records[0].provider, records[0].project_id.as_str()),
        (Provider::CurseForge, "100")
    );
    assert_eq!(records[0].version_id, "11");
    assert_eq!(records[0].sha1, sha1(&a1.bytes));
    assert_eq!(records[1].provider, Provider::Modrinth);
    assert_eq!(w.catalog.lookup_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn curseforge_updates_are_found_by_fingerprint_and_installed_with_their_origin() {
    let a1 = ver("11", "100", "1");
    let b1 = ver("21", "200", "1");
    let catalog = Catalog::default()
        .curseforge()
        .project("100")
        .project("200")
        .version(a1.clone())
        .version(ver("12", "100", "2").date("2025-02-01T00:00:00Z"))
        .version(
            ver("13", "100", "3-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-03-01T00:00:00Z"),
        )
        .version(b1.clone())
        .version(ver("22", "200", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar.disabled", &b1.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    let mut found: Vec<(&str, &str)> = report
        .updates
        .iter()
        .map(|u| (u.file.as_str(), u.latest.id.as_str()))
        .collect();
    found.sort();
    assert_eq!(found, [("a-1.jar", "12"), ("b-1.jar", "22")]);
    let asks = w.catalog.update_asks.lock().unwrap();
    assert_eq!(asks[0].1, [ReleaseChannel::Release]);
    drop(asks);

    // The channel setting lets the beta through.
    let beta = UpdateOptions {
        min: MinStability::Beta,
        ..UpdateOptions::default()
    };
    let report = check(&input(&w, &target, &meta), beta).unwrap();
    assert!(report.updates.iter().any(|u| u.latest.id == "13"));

    let plan = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    assert!(
        plan.items
            .iter()
            .all(|i| i.provider == Provider::CurseForge)
    );
    w.install(&plan).unwrap();
    assert_eq!(w.mod_files(), ["100-2.jar", "200-2.jar.disabled"]);
    let records = w.origins.load().origins;
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|o| o.provider == Provider::CurseForge));
    assert!(
        records
            .iter()
            .any(|o| o.file == "100-2.jar" && o.version_id == "12")
    );
    let again = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(again.updates.is_empty());
    assert_eq!(again.up_to_date, 2);
}

#[test]
fn a_file_recorded_from_another_platform_is_not_checked_here() {
    let (old, catalog) = two_versions();
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    w.origins
        .save(&[recorded(
            "a-1.jar",
            &old.bytes,
            Provider::CurseForge,
            "100",
            "11",
        )])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(report.updates.is_empty());
    assert_eq!(report.unchecked.len(), 1);
    assert_eq!(report.unchecked[0].reason, UncheckedReason::OtherPlatform);
    let identified = identify(&input(&w, &target, &meta)).unwrap();
    assert!(identified.identified.is_empty());
    assert_eq!(identified.already_known, 1);
    assert_eq!(
        w.origins.load().origins[0].provider,
        Provider::CurseForge,
        "the record is kept"
    );
}

#[test]
fn a_curseforge_pack_without_a_loader_is_updated_by_a_file_without_one() {
    let old = ver("31", "300", "1").loaders(&[]).file_name("pack-1.zip");
    let catalog = Catalog::default()
        .curseforge()
        .typed("300", ProjectType::ResourcePack)
        .version(old.clone())
        .version(
            ver("32", "300", "2")
                .loaders(&[])
                .file_name("pack-2.zip")
                .date("2025-02-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    let dir = w.resources.dir(ResourceKind::ResourcePacks);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pack-1.zip"), &old.bytes).unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert_eq!(report.updates.len(), 1, "{report:?}");
    assert_eq!(report.updates[0].latest.id, "32");
}
