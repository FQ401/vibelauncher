use std::sync::atomic::{AtomicI64, Ordering};

use vl_core::instance::NewInstance;

use super::*;
use crate::GAME_DIR;

const T0: i64 = 1_800_000_000;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("vl-trash-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    tmp: Tmp,
    instances: Instances,
    trash: Trash,
    time: Arc<AtomicI64>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let tmp = Tmp::new(tag);
        let instances = Instances::open(&tmp.0.join("instances")).unwrap();
        let time = Arc::new(AtomicI64::new(T0));
        let clock = Arc::clone(&time);
        let trash = Trash::new(&tmp.0.join("trash"), instances.clone())
            .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)));
        Self {
            tmp,
            instances,
            trash,
            time,
        }
    }

    fn at(&self, unix: i64) {
        self.time.store(unix, Ordering::SeqCst);
    }

    /// An instance with a file of `bytes` bytes in its game directory.
    fn instance(&self, name: &str, bytes: usize) -> InstanceId {
        let summary = self
            .instances
            .create(&NewInstance {
                name: name.into(),
                game_version: "1.21.1".into(),
                group: Some("Tests".into()),
                loader: None,
            })
            .unwrap();
        let game = self.instances.game_dir(&summary.id).unwrap();
        fs::create_dir_all(game.join("saves/World")).unwrap();
        fs::write(game.join("saves/World/level.dat"), vec![7u8; bytes]).unwrap();
        summary.id
    }

    fn trash_dir(&self) -> PathBuf {
        self.tmp.0.join("trash")
    }

    fn hidden(&self) -> Vec<String> {
        let mut names = Vec::new();
        for dir in [self.trash_dir(), self.instances.root().to_path_buf()] {
            if let Ok(entries) = fs::read_dir(&dir) {
                names.extend(
                    entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| n.starts_with('.')),
                );
            }
        }
        names
    }
}

fn level(instances: &Instances, id: &str) -> Vec<u8> {
    let game = instances.game_dir(&InstanceId::new(id).unwrap()).unwrap();
    fs::read(game.join("saves/World/level.dat")).unwrap()
}

fn delete_and_restore(f: &Fixture) {
    let id = f.instance("Survival", 1000);
    let deleted = f.trash.delete(&id, "  too many mods  ").unwrap();
    assert!(deleted.skipped_links.is_empty());
    let item = deleted.item;
    assert_eq!(item.id, "Survival");
    assert_eq!(item.instance, "Survival");
    assert_eq!(item.name, "Survival");
    assert_eq!(item.group.as_deref(), Some("Tests"));
    assert_eq!(item.game_version, "1.21.1");
    assert_eq!(item.deleted_unix, T0);
    assert_eq!(item.comment, "too many mods");
    assert!(item.size >= 1000, "{}", item.size);
    assert!(f.instances.list().unwrap().is_empty());
    assert_eq!(f.trash.list().unwrap(), vec![item.clone()]);
    assert!(f.trash_dir().join("Survival.json").is_file());

    let restored = f.trash.restore("Survival").unwrap();
    assert_eq!(restored.id.as_str(), "Survival");
    assert_eq!(restored.name, "Survival");
    assert_eq!(level(&f.instances, "Survival"), vec![7u8; 1000]);
    assert!(f.trash.list().unwrap().is_empty());
    assert!(!f.trash_dir().join("Survival.json").exists());
    assert!(f.hidden().is_empty(), "{:?}", f.hidden());
}

#[test]
fn delete_moves_into_the_trash_and_restore_puts_it_back() {
    delete_and_restore(&Fixture::new("move"));
}

#[test]
fn across_volumes_the_folder_is_copied_checked_and_the_source_erased() {
    let mut f = Fixture::new("copy");
    f.trash = f.trash.clone().always_copying();
    delete_and_restore(&f);
}

#[test]
fn a_taken_id_and_name_get_a_suffix_on_restore() {
    let f = Fixture::new("conflict");
    let first = f.instance("Survival", 10);
    f.trash.delete(&first, "").unwrap();
    let second = f.instance("Survival", 20);
    assert_eq!(second.as_str(), "Survival");
    f.trash.delete(&second, "").unwrap();
    let ids: Vec<String> = f.trash.list().unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(ids, ["Survival", "Survival-2"]);

    f.instance("Survival", 30);
    let back = f.trash.restore("Survival").unwrap();
    assert_eq!(back.id.as_str(), "Survival-2");
    assert_eq!(back.name, "Survival (2)");
    assert_eq!(level(&f.instances, "Survival-2"), vec![7u8; 10]);
    assert_eq!(
        f.instances
            .load(&InstanceId::new("Survival-2").unwrap())
            .unwrap()
            .name,
        "Survival (2)"
    );
    let again = f.trash.restore("Survival-2").unwrap();
    assert_eq!(again.id.as_str(), "Survival-3");
    assert_eq!(again.name, "Survival (3)");
}

#[test]
fn purge_by_age_removes_items_older_than_the_limit() {
    let f = Fixture::new("age");
    let old = f.instance("Old", 10);
    f.trash.delete(&old, "").unwrap();
    f.at(T0 + 10 * DAY_SECS);
    let new = f.instance("New", 10);
    f.trash.delete(&new, "").unwrap();

    f.at(T0 + 29 * DAY_SECS);
    assert!(f.trash.due(30, u64::MAX).unwrap().is_empty());
    f.at(T0 + 30 * DAY_SECS);
    let due = f.trash.due(30, u64::MAX).unwrap();
    assert_eq!(due, ["Old"]);
    let report = f.trash.purge_items(&due);
    assert_eq!(report.removed, ["Old"]);
    assert!(report.failed.is_empty());
    assert!(report.freed >= 10);
    let left: Vec<String> = f.trash.list().unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(left, ["New"]);
    assert!(f.hidden().is_empty(), "{:?}", f.hidden());

    f.at(T0 + 40 * DAY_SECS);
    assert_eq!(f.trash.due(30, u64::MAX).unwrap(), ["New"]);
}

#[test]
fn purge_by_size_removes_the_oldest_first_and_keeps_the_newest() {
    let f = Fixture::new("size");
    for (n, name) in ["A", "B", "C"].into_iter().enumerate() {
        f.at(T0 + i64::try_from(n).unwrap() * 60);
        let id = f.instance(name, 10_000);
        f.trash.delete(&id, "").unwrap();
    }
    let sizes: Vec<u64> = f.trash.list().unwrap().iter().map(|i| i.size).collect();
    let one = sizes[0];
    assert!(sizes.iter().all(|s| *s == one));
    assert!(f.trash.due(30, 3 * one).unwrap().is_empty());
    assert_eq!(f.trash.due(30, 2 * one).unwrap(), ["A"]);
    assert_eq!(f.trash.due(30, one + 1).unwrap(), ["A", "B"]);
    // The newest item stays even alone over the limit.
    assert_eq!(f.trash.due(30, 1).unwrap(), ["A", "B"]);
    // Age first, then size among the rest.
    f.at(T0 + 30 * DAY_SECS + 30);
    assert_eq!(f.trash.due(30, one + 1).unwrap(), ["A", "B"]);
}

#[test]
fn manual_purge_of_one_item_and_of_everything() {
    let f = Fixture::new("manual");
    for name in ["A", "B", "C"] {
        let id = f.instance(name, 10);
        f.trash.delete(&id, "").unwrap();
    }
    let report = f.trash.purge("B").unwrap();
    assert_eq!(report.removed, ["B"]);
    assert!(!f.trash_dir().join("B").exists());
    assert!(!f.trash_dir().join("B.json").exists());
    assert_eq!(f.trash.purge("B").unwrap_err().kind, ErrorKind::NotFound);
    let report = f.trash.purge_all().unwrap();
    assert_eq!(report.removed, ["A", "C"]);
    assert!(f.trash.list().unwrap().is_empty());
    assert!(fs::read_dir(f.trash_dir()).unwrap().next().is_none());
}

#[test]
fn a_permanent_delete_bypasses_the_trash() {
    let f = Fixture::new("permanent");
    let id = f.instance("Gone", 10);
    f.trash.delete_permanently(&id).unwrap();
    assert!(f.instances.list().unwrap().is_empty());
    assert!(f.trash.list().unwrap().is_empty());
    assert!(f.hidden().is_empty(), "{:?}", f.hidden());
    assert_eq!(
        f.trash.delete_permanently(&id).unwrap_err().kind,
        ErrorKind::NotFound
    );
}

#[test]
fn bad_input_is_refused() {
    let f = Fixture::new("input");
    let id = f.instance("X", 10);
    let long = "x".repeat(COMMENT_MAX_CHARS + 1);
    assert_eq!(
        f.trash.delete(&id, &long).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    let missing = InstanceId::new("missing").unwrap();
    assert_eq!(
        f.trash.delete(&missing, "").unwrap_err().kind,
        ErrorKind::NotFound
    );
    for item in ["..", "../instances/X", "a/b", "a\\b", ".incoming-X", ""] {
        assert!(f.trash.restore(item).is_err(), "{item}");
        assert!(f.trash.purge(item).is_err(), "{item}");
    }
    assert_eq!(f.instances.list().unwrap().len(), 1);
}

#[test]
fn a_damaged_or_missing_record_is_listed_with_a_problem() {
    let f = Fixture::new("record");
    let a = f.instance("Damaged", 10);
    f.trash.delete(&a, "note").unwrap();
    let b = f.instance("Missing", 10);
    f.trash.delete(&b, "").unwrap();
    fs::write(f.trash_dir().join("Damaged.json"), b"{ not json").unwrap();
    fs::remove_file(f.trash_dir().join("Missing.json")).unwrap();

    let items = f.trash.list().unwrap();
    assert_eq!(items.len(), 2);
    for item in &items {
        let problem = item.problem.as_deref().unwrap_or_default();
        assert!(problem.contains("record"), "{problem}");
        // The rest comes from the instance's own config.
        assert_eq!(item.name, item.id);
        assert_eq!(item.game_version, "1.21.1");
        assert!(item.size >= 10);
    }
    assert_eq!(f.trash.restore("Damaged").unwrap().id.as_str(), "Damaged");
    assert!(!f.trash_dir().join("Damaged.json").exists());
    assert_eq!(f.trash.purge("Missing").unwrap().removed, ["Missing"]);
    assert!(f.trash.list().unwrap().is_empty());
}

#[test]
fn ids_from_a_record_never_leave_the_instances_folder() {
    let f = Fixture::new("traversal");
    let id = f.instance("Safe", 10);
    f.trash.delete(&id, "").unwrap();
    let path = f.trash_dir().join("Safe.json");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("\"instance\": \"Safe\"", "\"instance\": \"../../escaped\"");
    assert!(text.contains("escaped"));
    fs::write(&path, text).unwrap();
    assert_eq!(f.trash.list().unwrap()[0].instance, "Safe");
    let back = f.trash.restore("Safe").unwrap();
    assert_eq!(back.id.as_str(), "Safe");
    assert!(!f.tmp.0.join("escaped").exists());
    assert!(!f.tmp.0.parent().unwrap().join("escaped").exists());
}

#[test]
fn an_oversized_or_newer_record_is_not_trusted() {
    let f = Fixture::new("oversized");
    let id = f.instance("Big", 10);
    f.trash.delete(&id, "").unwrap();
    let path = f.trash_dir().join("Big.json");
    fs::write(&path, vec![b' '; 70 * 1024]).unwrap();
    assert!(f.trash.list().unwrap()[0].problem.is_some());
    fs::write(
        &path,
        br#"{"format": 99, "instance": "Big", "name": "Big", "deleted_unix": 1, "size": 1}"#,
    )
    .unwrap();
    let problem = f.trash.list().unwrap()[0].problem.clone().unwrap();
    assert!(problem.contains("newer"), "{problem}");
}

#[test]
fn recover_finishes_or_undoes_an_interrupted_deletion() {
    let f = Fixture::new("recover-delete");
    // Committed: the copy reached the trash, the parked source was not erased yet.
    let done = f.instance("Done", 10);
    f.trash.delete(&done, "").unwrap();
    let park = f.instances.root().join(".deleting-Done");
    fs::create_dir_all(park.join(GAME_DIR)).unwrap();
    // Not committed: the source is parked and a partial copy is in the trash.
    let open = f.instance("Open", 10);
    let open_dir = f.instances.dir(&open).unwrap();
    fs::create_dir_all(f.trash_dir()).unwrap();
    fs::write(
        f.trash_dir().join("Open.json"),
        br#"{"format": 1, "instance": "Open", "name": "Open", "deleted_unix": 1, "size": 1}"#,
    )
    .unwrap();
    fs::rename(&open_dir, f.instances.root().join(".deleting-Open")).unwrap();
    fs::create_dir_all(f.trash_dir().join(".incoming-Open/minecraft")).unwrap();
    // A record whose folder never arrived.
    fs::write(f.trash_dir().join("Ghost.json"), b"{}").unwrap();

    let notes = f.trash.recover();
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(f.trash.has_garbage());
    f.trash.collect_garbage();
    assert!(f.hidden().is_empty(), "{:?}", f.hidden());
    let listed: Vec<String> = f
        .instances
        .list()
        .unwrap()
        .into_iter()
        .map(|i| i.id.to_string())
        .collect();
    assert_eq!(listed, ["Open"]);
    assert_eq!(level(&f.instances, "Open"), vec![7u8; 10]);
    let items: Vec<String> = f.trash.list().unwrap().into_iter().map(|i| i.id).collect();
    assert_eq!(items, ["Done"]);
    assert!(!f.trash_dir().join("Open.json").exists());
    assert!(!f.trash_dir().join("Ghost.json").exists());
    assert!(!f.trash.has_garbage());
}

#[test]
fn recover_finishes_or_undoes_an_interrupted_restore() {
    let f = Fixture::new("recover-restore");
    for name in ["Placed", "Parked"] {
        let id = f.instance(name, 10);
        f.trash.delete(&id, "").unwrap();
    }
    let mark = |item: &str, target: &str| {
        let path = f.trash_dir().join(format!("{item}.json"));
        let mut meta: Meta = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        meta.restoring_to = Some(target.into());
        fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
        fs::rename(
            f.trash_dir().join(item),
            f.trash_dir().join(format!(".restoring-{item}")),
        )
        .unwrap();
    };
    // Committed: the copy is in place as an instance.
    mark("Placed", "Placed");
    let placed = f.instances.root().join("Placed");
    fs::create_dir_all(placed.join(GAME_DIR)).unwrap();
    fs::copy(
        f.trash_dir().join(".restoring-Placed/instance.json"),
        placed.join("instance.json"),
    )
    .unwrap();
    // Not committed: a partial copy in the instances folder.
    mark("Parked", "Parked");
    fs::create_dir_all(f.instances.root().join(".restoring-Parked")).unwrap();

    let notes = f.trash.recover();
    assert_eq!(notes.len(), 2, "{notes:?}");
    f.trash.collect_garbage();
    assert!(f.hidden().is_empty(), "{:?}", f.hidden());
    let listed: Vec<String> = f
        .instances
        .list()
        .unwrap()
        .into_iter()
        .map(|i| i.id.to_string())
        .collect();
    assert_eq!(listed, ["Placed"]);
    let items = f.trash.list().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "Parked");
    assert!(items[0].problem.is_none());
    assert_eq!(f.trash.restore("Parked").unwrap().id.as_str(), "Parked");
}

#[test]
fn deleted_instances_of_earlier_versions_are_adopted() {
    let f = Fixture::new("legacy");
    let id = f.instance("Old", 10);
    let legacy = f.instances.root().join(".trash");
    fs::create_dir_all(&legacy).unwrap();
    fs::rename(f.instances.dir(&id).unwrap(), legacy.join("Old-1700000000")).unwrap();
    fs::create_dir_all(legacy.join("Old-1700000000-1")).unwrap();
    fs::create_dir_all(legacy.join("odd name")).unwrap();
    assert!(f.trash.has_garbage());

    assert!(f.trash.adopt_legacy().is_empty());
    assert!(!legacy.exists());
    let items = f.trash.list().unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["odd name", "Old", "Old-2"]);
    let old = items.iter().find(|i| i.id == "Old").unwrap();
    assert_eq!(old.deleted_unix, 1_700_000_000);
    assert_eq!(old.name, "Old");
    assert_eq!(old.instance, "Old");
    assert!(f.trash.restore("Old").is_ok());
    assert_eq!(level(&f.instances, "Old"), vec![7u8; 10]);
}

#[test]
fn legacy_names_are_split_into_id_and_time() {
    assert_eq!(
        split_legacy_name("Survival-1700000000"),
        ("Survival".into(), Some(1_700_000_000))
    );
    assert_eq!(
        split_legacy_name("My-pack-1700000000-2"),
        ("My-pack".into(), Some(1_700_000_000))
    );
    assert_eq!(split_legacy_name("pack-2"), ("pack-2".into(), None));
    assert_eq!(split_legacy_name("1700000000"), ("1700000000".into(), None));
}

#[test]
fn the_busy_flag_follows_the_lock() {
    let f = Fixture::new("busy");
    assert!(!f.trash.is_busy());
    let guard = f.trash.guard();
    assert!(f.trash.clone().is_busy());
    drop(guard);
    assert!(!f.trash.is_busy());
}

#[cfg(unix)]
#[test]
fn links_are_never_followed_when_measuring_moving_or_erasing() {
    let f = Fixture::new("links");
    let outside = f.tmp.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious.dat"), vec![1u8; 50_000]).unwrap();
    let id = f.instance("Linked", 10);
    let game = f.instances.game_dir(&id).unwrap();
    std::os::unix::fs::symlink(&outside, game.join("link")).unwrap();

    let deleted = f.trash.clone().always_copying().delete(&id, "").unwrap();
    assert!(deleted.item.size < 50_000, "{}", deleted.item.size);
    assert_eq!(deleted.skipped_links.len(), 1);
    f.trash.purge_all().unwrap();
    assert!(outside.join("precious.dat").is_file());

    let id = f.instance("Linked2", 10);
    let game = f.instances.game_dir(&id).unwrap();
    std::os::unix::fs::symlink(&outside, game.join("link")).unwrap();
    f.trash.delete_permanently(&id).unwrap();
    assert!(outside.join("precious.dat").is_file());
}
