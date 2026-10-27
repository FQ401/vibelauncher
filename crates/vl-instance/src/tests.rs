use super::*;

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("vl-instance-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn new(name: &str) -> NewInstance {
    NewInstance {
        name: name.into(),
        game_version: "1.21.1".into(),
        group: None,
    }
}

#[test]
fn creates_lists_and_picks_free_directory_names() {
    let t = Tmp::new("create");
    let inst = Instances::open(&t.0.join("instances")).unwrap();
    let a = inst.create(&new("Survival")).unwrap();
    let b = inst.create(&new("Survival")).unwrap();
    assert_eq!(a.id.as_str(), "Survival");
    assert_eq!(b.id.as_str(), "Survival-2");
    assert!(inst.game_dir(&a.id).unwrap().is_dir());
    let list = inst.list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].game_version, "1.21.1");
    assert_eq!(list[0].loader, Loader::Vanilla);
    assert!(list.iter().all(|i| i.problem.is_none()));
}

#[test]
fn rejects_bad_input() {
    let t = Tmp::new("input");
    let inst = Instances::open(&t.0).unwrap();
    assert!(inst.create(&new("  ")).is_err());
    let mut n = new("x");
    n.game_version = String::new();
    assert!(inst.create(&n).is_err());
    let id = InstanceId::new("..").unwrap();
    assert!(inst.dir(&id).is_err());
    assert!(inst.load(&InstanceId::new("nope").unwrap()).is_err());
}

#[test]
fn damaged_instances_are_listed_with_a_problem() {
    let t = Tmp::new("damaged");
    let inst = Instances::open(&t.0).unwrap();
    inst.create(&new("good")).unwrap();
    fs::create_dir_all(t.0.join("broken")).unwrap();
    fs::write(t.0.join("broken").join(CONFIG_FILE), b"{ not json").unwrap();
    fs::create_dir_all(t.0.join("empty")).unwrap();
    fs::create_dir_all(t.0.join("newer")).unwrap();
    fs::write(
        t.0.join("newer").join(CONFIG_FILE),
        br#"{"format":99,"name":"n","components":[{"uid":"net.minecraft","version":"1"}]}"#,
    )
    .unwrap();
    fs::write(t.0.join("stray.txt"), b"x").unwrap();
    let list = inst.list().unwrap();
    let by = |n: &str| list.iter().find(|i| i.id.as_str() == n).unwrap();
    assert_eq!(list.len(), 4);
    assert!(by("good").problem.is_none());
    assert!(by("broken").problem.as_deref().unwrap().contains("damaged"));
    assert!(by("empty").problem.as_deref().unwrap().contains("missing"));
    assert!(by("newer").problem.as_deref().unwrap().contains("newer"));
}

#[test]
fn rename_moves_the_directory_and_keeps_data() {
    let t = Tmp::new("rename");
    let inst = Instances::open(&t.0).unwrap();
    let a = inst.create(&new("Old")).unwrap();
    let world = inst.game_dir(&a.id).unwrap().join("saves");
    fs::create_dir_all(&world).unwrap();
    fs::write(world.join("level.dat"), b"data").unwrap();
    inst.record_play(&a.id, 60).unwrap();
    let b = inst.rename(&a.id, "New name").unwrap();
    assert_eq!(b.id.as_str(), "New name");
    assert_eq!(b.name, "New name");
    assert_eq!(b.play_time_secs, 60);
    assert!(!t.0.join("Old").exists());
    let saved = inst
        .game_dir(&b.id)
        .unwrap()
        .join("saves")
        .join("level.dat");
    assert_eq!(fs::read(saved).unwrap(), b"data");
    // A name that maps to the same directory only changes the config.
    let c = inst.rename(&b.id, "New name").unwrap();
    assert_eq!(c.id, b.id);
}

#[test]
fn rename_never_overwrites_another_instance() {
    let t = Tmp::new("rename-clash");
    let inst = Instances::open(&t.0).unwrap();
    inst.create(&new("A")).unwrap();
    let b = inst.create(&new("B")).unwrap();
    let r = inst.rename(&b.id, "A").unwrap();
    assert_eq!(r.id.as_str(), "A-2");
    assert_eq!(inst.load(&InstanceId::new("A").unwrap()).unwrap().name, "A");
}

#[test]
fn delete_moves_to_trash_and_purge_erases() {
    let t = Tmp::new("delete");
    let inst = Instances::open(&t.0).unwrap();
    let a = inst.create(&new("Gone")).unwrap();
    let moved = inst.delete(&a.id).unwrap();
    assert!(moved.join(CONFIG_FILE).is_file());
    assert!(inst.list().unwrap().is_empty());
    assert!(inst.delete(&a.id).is_err());
    // The same name can be created and deleted again without clashing in the trash.
    let b = inst.create(&new("Gone")).unwrap();
    inst.delete(&b.id).unwrap();
    inst.purge_trash().unwrap();
    assert!(!t.0.join(".trash").exists());
}

#[test]
fn leftover_staging_directories_are_ignored_and_cleaned() {
    let t = Tmp::new("leftover");
    let inst = Instances::open(&t.0).unwrap();
    fs::create_dir_all(t.0.join(".creating-X").join(GAME_DIR)).unwrap();
    assert!(inst.list().unwrap().is_empty());
    inst.create(&new("X")).unwrap();
    assert!(!t.0.join(".creating-X").exists());
}

#[test]
fn unknown_config_fields_and_missing_optionals_are_accepted() {
    let t = Tmp::new("compat");
    let inst = Instances::open(&t.0).unwrap();
    fs::create_dir_all(t.0.join("old")).unwrap();
    fs::write(
        t.0.join("old").join(CONFIG_FILE),
        br#"{"format":1,"name":"Old","future":true,
        "components":[{"uid":"net.minecraft","version":"1.20"},
        {"uid":"net.fabricmc.fabric-loader","version":"0.16"}]}"#,
    )
    .unwrap();
    let list = inst.list().unwrap();
    assert_eq!(list[0].loader, Loader::Fabric("0.16".into()));
    assert!(list[0].problem.is_none());
}
