//! Store tests on synthetic folders: opening, moving sections (with cancel, failures and
//! rollback), reopening after a move, cleanup.

use std::fs;
use std::path::{Path, PathBuf};

use vl_core::platform::Os;
use vl_core::storage::{MoveMode, MoveRequest, RootSource, Section};

use super::testutil::TempDir;
use super::*;

fn env() -> Env {
    Env {
        os: Os::Linux,
        ..Env::default()
    }
}

fn portable(dir: PathBuf) -> StoreOptions {
    StoreOptions {
        portable_dir: Some(dir),
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn request(section: Section, target: Option<PathBuf>, mode: MoveMode) -> MoveRequest {
    MoveRequest {
        section,
        target,
        mode,
    }
}

fn run(store: &Store, req: &MoveRequest) -> Result<MoveReport, StoreError> {
    let (plan, _) = store.plan_move(req)?;
    mover::execute(&plan, &mut |_| Ok(()))
}

/// A data folder of the layout before sections existed.
fn old_layout(root: &Path) {
    write(&root.join(FORMAT_FILE), "{\"format\": 1}");
    write(&root.join("settings/global.toml"), "schema_version = 1\n");
    write(&root.join("instances/survival/instance.json"), "{}");
    write(
        &root.join("instances/survival/minecraft/options.txt"),
        "fov:70",
    );
    write(&root.join("libraries/org/lib/1.0/lib-1.0.jar"), "jar");
    write(&root.join("versions/1.21/1.21.json"), "{}");
    write(&root.join("assets/indexes/17.json"), "{}");
    write(&root.join("java/jre-21/bin/java"), "java");
}

#[test]
fn portable_mode_writes_only_inside_its_directory() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("portable");
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    assert!(store.is_portable());
    assert_eq!(store.source(), RootSource::Argument);
    write_atomic(&store.data_dir().join("settings.toml"), b"x = 1\n").unwrap();
    let root = paths::normalize(&root).unwrap();
    for section in Section::ALL {
        assert!(store.dir(section).starts_with(&root), "{section}");
    }
    drop(store);
    let siblings: Vec<_> = fs::read_dir(sandbox.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(siblings, ["portable"]);
}

#[test]
fn a_second_store_on_the_same_directory_is_refused() {
    let sandbox = TempDir::new();
    let options = portable(sandbox.path().join("p"));
    let first = Store::open_with(&env(), &options).unwrap();
    let second = Store::open_with(&env(), &options).unwrap_err();
    assert!(matches!(second, StoreError::Locked { .. }), "{second}");
    drop(first);
    Store::open_with(&env(), &options).unwrap();
}

#[test]
fn a_new_store_gets_the_current_format_without_a_snapshot() {
    let sandbox = TempDir::new();
    let store = Store::open_with(&env(), &portable(sandbox.path().join("p"))).unwrap();
    assert_eq!(
        format::read_version(store.data_dir()).unwrap(),
        Some(CURRENT_FORMAT)
    );
    assert!(store.migration().is_none());
    assert!(!store.data_dir().join(format::SNAPSHOT_DIR).exists());
}

#[test]
fn existing_data_keeps_its_places() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("\u{414}\u{430}\u{43d}\u{43d}\u{44b}\u{435} \u{441} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b}\u{430}\u{43c}\u{438}");
    old_layout(&root);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    let root = paths::normalize(&root).unwrap();
    assert_eq!(store.instances_dir(), root.join("instances"));
    assert_eq!(store.game_dir(), root);
    assert_eq!(store.java_dir(), root.join("java"));
    assert_eq!(store.cache_dir(), root.join("cache"));
    assert_eq!(store.backups_dir(), root.join("snapshots"));
    assert_eq!(store.logs_dir(), root.join("logs"));
    let info = store.info(true);
    assert!(info.sections.iter().all(|s| s.default && s.available));
    assert!(info.leftovers.is_empty());
    let game = info
        .sections
        .iter()
        .find(|s| s.section == Section::Game)
        .unwrap();
    assert_eq!(game.size, Some(3 + 2 + 2));
    assert!(!root.join(layout::STORAGE_FILE).exists());
}

#[test]
fn a_moved_section_is_used_after_reopening_and_the_old_copy_waits_for_cleanup() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let options = portable(root.clone());
    let target = sandbox
        .path()
        .join("\u{418}\u{433}\u{440}\u{44b} \u{438} \u{441}\u{431}\u{43e}\u{440}\u{43a}\u{438}")
        .join("instances");
    let store = Store::open_with(&env(), &options).unwrap();
    let old = store.instances_dir();
    let report = run(
        &store,
        &request(Section::Instances, Some(target.clone()), MoveMode::Copy),
    )
    .unwrap();
    assert_eq!(report.files, 2);
    assert!(read(&old.join("survival/minecraft/options.txt")).contains("fov"));
    drop(store);

    let store = Store::open_with(&env(), &options).unwrap();
    let target = paths::normalize(&target).unwrap();
    assert_eq!(store.instances_dir(), target);
    assert_eq!(
        read(&target.join("survival/minecraft/options.txt")),
        "fov:70"
    );
    let info = store.info(true);
    assert_eq!(info.leftovers.len(), 1);
    assert_eq!(info.leftovers[0].path, old);
    assert_eq!(info.leftovers[0].size, Some(2 + 6));
    assert!(!fs::read_dir(target.parent().unwrap()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".vl-moving")
    }));

    let cleaned = store.cleanup().unwrap();
    assert!(cleaned.failed.is_empty(), "{cleaned:?}");
    assert_eq!(cleaned.removed, [old.join("survival")]);
    assert!(!old.exists());
    assert!(read(&target.join("survival/instance.json")).contains('{'));
    drop(store);
    let store = Store::open_with(&env(), &options).unwrap();
    assert!(store.info(false).leftovers.is_empty());
    assert_eq!(store.instances_dir(), target);
}

#[test]
fn a_cancel_in_the_middle_leaves_everything_as_it_was() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    let target = sandbox.path().join("game files");
    let (plan, _) = store
        .plan_move(&request(
            Section::Game,
            Some(target.clone()),
            MoveMode::Copy,
        ))
        .unwrap();
    assert_eq!(plan.files, 3);
    let mut calls = 0;
    let err = mover::execute(&plan, &mut |p| {
        calls += 1;
        if p.files >= 1 {
            Err(StoreError::Cancelled)
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(matches!(err, StoreError::Cancelled), "{err}");
    assert_eq!(calls, 1);
    assert!(!target.exists());
    assert_eq!(
        fs::read_dir(sandbox.path()).unwrap().count(),
        1,
        "no temporary folder left"
    );
    assert!(!store.data_dir().join(layout::STORAGE_FILE).exists());
    drop(store);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    assert_eq!(store.game_dir(), paths::normalize(&root).unwrap());
    assert_eq!(read(&root.join("libraries/org/lib/1.0/lib-1.0.jar")), "jar");
}

#[test]
fn a_failure_in_the_middle_rolls_back() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    let target = sandbox.path().join("java");
    let (plan, _) = store
        .plan_move(&request(
            Section::Java,
            Some(target.clone()),
            MoveMode::Copy,
        ))
        .unwrap();
    let err = mover::execute(&plan, &mut |_| {
        Err(StoreError::io(
            Path::new("disk"),
            std::io::Error::other("device removed"),
        ))
    })
    .unwrap_err();
    assert!(err.to_string().contains("device removed"), "{err}");
    assert!(!target.exists());
    assert_eq!(store.layout().config(), &StorageConfig::default());
}

#[test]
fn a_failed_switch_puts_the_copy_away_again() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    // A folder where the layout file goes makes the switch fail after the copy is in place.
    fs::create_dir_all(store.data_dir().join(layout::STORAGE_FILE).join("x")).unwrap();
    let target = sandbox.path().join("cache-elsewhere");
    write(&store.cache_dir().join("http/a.body"), "body");
    let err = run(
        &store,
        &request(Section::Cache, Some(target.clone()), MoveMode::Copy),
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::Io { .. }), "{err}");
    assert!(!target.exists());
    assert_eq!(read(&store.cache_dir().join("http/a.body")), "body");
    assert_eq!(fs::read_dir(sandbox.path()).unwrap().count(), 1);
}

#[test]
fn long_paths_are_copied() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let mut deep = root.join("instances/survival/minecraft/config");
    for n in 0..8 {
        deep = deep.join(format!("a-rather-long-directory-name-number-{n}"));
    }
    let file = deep.join("settings-with-a-long-name.json");
    write(&file, "deep");
    assert!(file.as_os_str().len() > 300);
    let store = Store::open_with(&env(), &portable(root)).unwrap();
    let target = sandbox.path().join("long instances");
    run(
        &store,
        &request(Section::Instances, Some(target.clone()), MoveMode::Copy),
    )
    .unwrap();
    let relative = file
        .strip_prefix(store.data_dir().join("instances"))
        .unwrap_or_else(|_| {
            file.strip_prefix(sandbox.path().join("root/instances"))
                .unwrap()
        });
    assert_eq!(read(&target.join(relative)), "deep");
}

#[test]
fn shared_game_files_move_out_and_back() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let options = portable(root.clone());
    let target = sandbox.path().join("shared");
    let store = Store::open_with(&env(), &options).unwrap();
    run(
        &store,
        &request(Section::Game, Some(target.clone()), MoveMode::Copy),
    )
    .unwrap();
    drop(store);
    let store = Store::open_with(&env(), &options).unwrap();
    let target = paths::normalize(&target).unwrap();
    assert_eq!(store.game_dir(), target);
    assert_eq!(read(&target.join("versions/1.21/1.21.json")), "{}");
    assert!(!target.join("instances").exists());

    // The old copy is still in the data folder: copying back needs a cleanup first.
    let back = request(Section::Game, None, MoveMode::Copy);
    assert!(matches!(
        store.plan_move(&back),
        Err(StoreError::InvalidInput(_))
    ));
    store.cleanup().unwrap();
    assert!(!root.join("libraries").exists());
    assert!(root.join("instances/survival").is_dir());
    run(&store, &back).unwrap();
    drop(store);
    let store = Store::open_with(&env(), &options).unwrap();
    assert!(store.layout().is_default(Section::Game));
    assert_eq!(read(&root.join("libraries/org/lib/1.0/lib-1.0.jar")), "jar");
}

#[test]
fn a_switch_only_move_takes_the_folder_as_it_is() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let options = portable(root.clone());
    let store = Store::open_with(&env(), &options).unwrap();
    let mine = sandbox.path().join("copied by hand");
    write(&mine.join("other/instance.json"), "{}");
    let missing = request(
        Section::Instances,
        Some(sandbox.path().join("nope")),
        MoveMode::SwitchOnly,
    );
    assert!(store.plan_move(&missing).is_err());
    let report = run(
        &store,
        &request(Section::Instances, Some(mine.clone()), MoveMode::SwitchOnly),
    )
    .unwrap();
    assert_eq!(report.files, 0);
    drop(store);
    let store = Store::open_with(&env(), &options).unwrap();
    assert!(store.instances_dir().join("other").is_dir());
    assert_eq!(store.info(false).leftovers.len(), 1);
}

#[test]
fn an_explicit_folder_that_is_gone_is_unavailable_not_recreated() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let options = portable(root.clone());
    let target = sandbox.path().join("usb").join("instances");
    let store = Store::open_with(&env(), &options).unwrap();
    run(
        &store,
        &request(Section::Instances, Some(target.clone()), MoveMode::Copy),
    )
    .unwrap();
    drop(store);
    fs::remove_dir_all(sandbox.path().join("usb")).unwrap();
    let store = Store::open_with(&env(), &options).unwrap();
    assert_eq!(store.unavailable(), [Section::Instances]);
    assert!(!target.exists());
    let copy = request(Section::Instances, None, MoveMode::Copy);
    assert!(matches!(
        store.plan_move(&copy),
        Err(StoreError::Conflict(_))
    ));
    // The old copy is still in the data folder: switching back to it works.
    run(
        &store,
        &request(Section::Instances, None, MoveMode::SwitchOnly),
    )
    .unwrap();
    drop(store);
    let store = Store::open_with(&env(), &options).unwrap();
    assert!(store.unavailable().is_empty());
    assert!(
        store.info(false).leftovers.is_empty(),
        "the old copy is live again"
    );
}

/// An environment where the data folder comes from the OS convention or the pointer.
fn os_env(sandbox: &Path) -> Env {
    Env {
        os: Os::Linux,
        home: Some(sandbox.join("home")),
        xdg_data_home: Some(sandbox.join("share")),
        xdg_config_home: Some(sandbox.join("config")),
        xdg_cache_home: Some(sandbox.join("legacy-cache")),
        xdg_state_home: Some(sandbox.join("legacy-state")),
        ..Env::default()
    }
}

#[test]
fn the_data_folder_moves_through_the_pointer() {
    let sandbox = TempDir::new();
    let env = os_env(sandbox.path());
    let default_root = sandbox.path().join("share/vibelauncher");
    old_layout(&default_root);
    write(
        &sandbox.path().join("legacy-cache/vibelauncher/http/x.body"),
        "old cache",
    );
    let store = Store::open_with(&env, &StoreOptions::default()).unwrap();
    assert_eq!(store.source(), RootSource::Default);
    let legacy = store.info(false).leftovers;
    assert_eq!(legacy.len(), 1, "{legacy:?}");
    assert_eq!(legacy[0].section, Section::Cache);

    let new_root = sandbox.path().join("D drive").join("VibeLauncher");
    run(
        &store,
        &request(Section::Data, Some(new_root.clone()), MoveMode::Copy),
    )
    .unwrap();
    drop(store);

    let store = Store::open_with(&env, &StoreOptions::default()).unwrap();
    let new_root = paths::normalize(&new_root).unwrap();
    assert_eq!(store.source(), RootSource::Pointer);
    assert_eq!(store.data_dir(), new_root);
    assert_eq!(
        read(&new_root.join("settings/global.toml")),
        "schema_version = 1\n"
    );
    assert_eq!(store.instances_dir(), new_root.join("instances"));
    let pointer = pointer::read(&paths::pointer_file(&env).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(pointer.root, new_root);
    let info = store.info(false);
    assert!(info.leftovers.iter().any(|l| l.section == Section::Data));

    let cleaned = store.cleanup().unwrap();
    assert!(cleaned.failed.is_empty(), "{cleaned:?}");
    assert!(!default_root.exists());
    assert!(!sandbox.path().join("legacy-cache/vibelauncher").exists());
    assert!(store.info(false).leftovers.is_empty());

    // A fixed data folder cannot be moved by the launcher.
    drop(store);
    let fixed = Store::open_with(&env, &portable(sandbox.path().join("fixed"))).unwrap();
    let err = run(
        &fixed,
        &request(
            Section::Data,
            Some(sandbox.path().join("x")),
            MoveMode::Copy,
        ),
    );
    assert!(matches!(err, Err(StoreError::Conflict(_))));
    drop(fixed);

    // Back to the OS convention: the pointer goes away.
    let store = Store::open_with(&env, &StoreOptions::default()).unwrap();
    run(&store, &request(Section::Data, None, MoveMode::Copy)).unwrap();
    drop(store);
    assert!(!paths::pointer_file(&env).unwrap().exists());
    let store = Store::open_with(&env, &StoreOptions::default()).unwrap();
    assert_eq!(store.source(), RootSource::Default);
    assert!(
        store
            .instances_dir()
            .join("survival/instance.json")
            .is_file()
    );
}

#[test]
fn moves_that_make_no_sense_are_refused() {
    let sandbox = TempDir::new();
    let root = sandbox.path().join("root");
    old_layout(&root);
    let store = Store::open_with(&env(), &portable(root.clone())).unwrap();
    let here = request(Section::Instances, None, MoveMode::Copy);
    assert!(matches!(
        store.plan_move(&here),
        Err(StoreError::InvalidInput(_))
    ));
    let inside = request(
        Section::Cache,
        Some(root.join("instances/cache")),
        MoveMode::Copy,
    );
    let err = store.plan_move(&inside).unwrap_err();
    assert!(err.to_string().contains("overlaps"), "{err}");
}
