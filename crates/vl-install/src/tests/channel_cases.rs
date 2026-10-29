//! Update channels that follow the installed version, files the platform no longer finds by
//! hash, two files of one project in a folder, and the global data packs folder.

use std::sync::atomic::Ordering;

use super::*;
use crate::updates::{
    Selection, UpdateInput, UpdateOptions, check, check_all, plan_update, plan_update_all,
};
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
        version_number: Some("0.9".into()),
        installed_unix: 0,
        dependency: false,
    }
}

/// The stored setting: releases, widened by what is installed.
fn setting(min: MinStability) -> UpdateOptions {
    UpdateOptions {
        min,
        follow_installed: true,
        ..UpdateOptions::default()
    }
}

/// `A`: release 1, beta 2, release 3 is older than beta 4.
fn channels() -> Catalog {
    Catalog::default()
        .project("A")
        .project("B")
        .version(ver("a1", "A", "1"))
        .version(
            ver("a2", "A", "2-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-02-01T00:00:00Z"),
        )
        .version(ver("a3", "A", "3").date("2025-03-01T00:00:00Z"))
        .version(
            ver("a4", "A", "4-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-04-01T00:00:00Z"),
        )
        .version(ver("b1", "B", "1"))
        .version(
            ver("b2", "B", "2-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-02-01T00:00:00Z"),
        )
}

#[test]
fn a_file_installed_from_a_beta_is_offered_newer_betas_by_the_setting() {
    let w = World::new(channels());
    w.put_mod("a-2.jar", &ver("a2", "A", "2-beta").bytes);
    w.put_mod("b-1.jar", &ver("b1", "B", "1").bytes);
    let (target, meta) = (fabric(), meta(&w));

    // The setting (releases) widened for the beta file only: one request per floor.
    let report = check(&input(&w, &target, &meta), setting(MinStability::Release)).unwrap();
    let latest = |file: &str| {
        report
            .updates
            .iter()
            .find(|u| u.file == file)
            .map(|u| u.latest.number.clone())
    };
    assert_eq!(latest("a-2.jar").as_deref(), Some("4-beta"));
    assert_eq!(latest("b-1.jar"), None, "B stays on releases");
    assert_eq!(report.up_to_date, 1);
    assert_eq!(w.catalog.update_calls.load(Ordering::Relaxed), 2);
    let asks = w.catalog.update_asks.lock().unwrap();
    assert!(asks.iter().any(|(_, c)| c == &[ReleaseChannel::Release]));
    assert!(
        asks.iter()
            .any(|(_, c)| c == &[ReleaseChannel::Release, ReleaseChannel::Beta])
    );
    drop(asks);

    // A channel the caller names is used as it is.
    let named = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    let a = named.updates.iter().find(|u| u.file == "a-2.jar").unwrap();
    assert_eq!(a.latest.number, "3");

    // The plan of the widened check pins the beta.
    let plan = plan_update(
        &input(&w, &target, &meta),
        setting(MinStability::Release),
        Selection::All,
        None,
    )
    .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].version_id, "a4");
}

#[test]
fn a_file_whose_version_was_deleted_is_updated_from_its_project_versions() {
    let w = World::new(channels());
    // Bytes no version has: the platform does not find the file, the origin record names it.
    let gone = jar("a", "0.9");
    w.put_mod("a-0.9.jar", &gone);
    w.origins
        .save(&[recorded("a-0.9.jar", &gone, Provider::Modrinth, "A", "a0")])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), setting(MinStability::Release)).unwrap();
    assert!(report.unchecked.is_empty(), "{report:?}");
    assert_eq!(report.updates.len(), 1);
    let u = &report.updates[0];
    assert_eq!(
        (u.current.id.as_str(), u.current.number.as_str()),
        ("a0", "0.9")
    );
    assert_eq!(u.current.channel, ReleaseChannel::Unknown);
    assert_eq!(
        u.latest.id, "a3",
        "the newest release; the setting is not widened"
    );

    let plan = plan_update(
        &input(&w, &target, &meta),
        setting(MinStability::Release),
        Selection::All,
        None,
    )
    .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].version_id, "a3");
    assert_eq!(
        plan.items[0].action,
        PlanAction::Replace {
            file: "a-0.9.jar".into(),
            old_version: Some("0.9".into()),
        }
    );
    w.install(&plan).unwrap();
    assert_eq!(w.mod_files(), ["a-3.jar"]);
}

#[test]
fn a_listed_version_unknown_by_hash_is_compared_and_a_gone_project_stays_unchecked() {
    let w = World::new(channels().project("Gone"));
    // Version a3 is listed but the file's bytes are not the platform's: nothing newer than a3
    // is a release, so it is up to date.
    let changed = jar("a", "3-patched");
    w.put_mod("a-3.jar", &changed);
    let other = jar("gone", "1");
    w.put_mod("gone-1.jar", &other);
    w.origins
        .save(&[
            recorded("a-3.jar", &changed, Provider::Modrinth, "A", "a3"),
            recorded("gone-1.jar", &other, Provider::Modrinth, "Gone", "g1"),
        ])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), setting(MinStability::Release)).unwrap();
    assert!(report.updates.is_empty(), "{report:?}");
    assert_eq!(report.up_to_date, 1);
    assert_eq!(report.unchecked.len(), 1);
    assert_eq!(report.unchecked[0].file, "gone-1.jar");
    assert_eq!(
        report.unchecked[0].reason,
        UncheckedReason::UnknownToPlatform
    );

    // With betas allowed, the newer beta of the listed version is offered.
    let beta = check(&input(&w, &target, &meta), setting(MinStability::Beta)).unwrap();
    assert_eq!(beta.updates.len(), 1);
    assert_eq!(beta.updates[0].latest.id, "a4");
    assert_eq!(beta.updates[0].current.number, "3");
}

#[test]
fn a_curseforge_file_whose_file_was_deleted_is_updated_from_the_project_files() {
    let catalog = Catalog::default()
        .curseforge()
        .project("100")
        .version(ver("11", "100", "1"))
        .version(ver("12", "100", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    let gone = jar("100", "0.5");
    w.put_mod("cf-0.5.jar", &gone);
    w.origins
        .save(&[recorded(
            "cf-0.5.jar",
            &gone,
            Provider::CurseForge,
            "100",
            "10",
        )])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), setting(MinStability::Release)).unwrap();
    assert_eq!(report.updates.len(), 1, "{report:?}");
    assert_eq!(report.updates[0].provider, Provider::CurseForge);
    assert_eq!(report.updates[0].latest.id, "12");
    let inputs = [input(&w, &target, &meta)];
    let plan = plan_update_all(
        &inputs,
        setting(MinStability::Release),
        Selection::All,
        None,
    )
    .unwrap();
    assert_eq!(plan.items[0].version_id, "12");
}

#[test]
fn two_files_of_one_project_are_a_conflict_not_a_silent_choice() {
    let w = World::new(channels());
    w.put_mod("a-1.jar", &ver("a1", "A", "1").bytes);
    w.put_mod("a-3.jar", &ver("a3", "A", "3").bytes);
    w.put_mod("b-1.jar", &ver("b1", "B", "1").bytes);
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert!(report.updates.is_empty(), "{report:?}");
    assert_eq!(report.conflicts.len(), 1);
    let c = &report.conflicts[0];
    assert_eq!(c.project_id, "A");
    assert_eq!(c.files, ["a-1.jar", "a-3.jar"]);
    assert_eq!(report.unchecked.len(), 2);
    assert!(
        report
            .unchecked
            .iter()
            .all(|u| u.reason == UncheckedReason::SameProject)
    );
    assert_eq!(report.up_to_date, 1);

    let named = ["a-1.jar".to_owned()];
    let err = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::Files(&named),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.message.contains("a-3.jar"), "{}", err.message);
    let all = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    assert!(all.items.is_empty());
    assert!(all.warnings.iter().any(|m| m.contains("same project")));

    // The combined check reports it once.
    let inputs = [input(&w, &target, &meta)];
    let combined = check_all(&inputs, UpdateOptions::default()).unwrap();
    assert_eq!(combined.conflicts, report.conflicts);

    // Installing the project again does not pick one of the two either.
    let err = w.plan("A").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
}

#[test]
fn global_data_packs_are_checked_and_updated_in_their_folder() {
    let old = ver("c1", "Caves", "1")
        .loaders(&["datapack"])
        .file_name("caves-1.zip");
    let catalog = Catalog::default()
        .typed("Caves", ProjectType::DataPack)
        .version(old.clone())
        .version(
            ver("c2", "Caves", "2")
                .loaders(&["datapack"])
                .file_name("caves-2.zip")
                .date("2025-02-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    let dir = w.resources.dir(ResourceKind::GlobalDataPacks);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("caves-1.zip"), &old.bytes).unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let report = check(&input(&w, &target, &meta), UpdateOptions::default()).unwrap();
    assert_eq!(report.updates.len(), 1, "{report:?}");
    assert_eq!(report.updates[0].kind, ResourceKind::GlobalDataPacks);
    let plan = plan_update(
        &input(&w, &target, &meta),
        UpdateOptions::default(),
        Selection::All,
        None,
    )
    .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].kind, ResourceKind::GlobalDataPacks);
    w.install(&plan).unwrap();
    assert!(dir.join("caves-2.zip").exists());
    assert!(!dir.join("caves-1.zip").exists());
}
