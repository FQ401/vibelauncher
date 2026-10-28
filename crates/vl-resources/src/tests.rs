use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use vl_core::ErrorKind;
use vl_core::resources::{ModLoader, ResourceKind};
use vl_crash::ClassIndex;

use crate::Resources;
use crate::jar::{MAX_METADATA_BYTES, class_package, read_jar, reduce_packages};

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vl-resources-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A jar in memory with the given entries.
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

const FABRIC: &[u8] =
    br#"{"id":"alpha","version":"1.0","name":"Alpha","depends":{"minecraft":"1.20.1"},
  "icon":"assets/alpha/icon.png"}"#;

fn fixture(t: &Temp) -> Resources {
    Resources::new(t.0.join("minecraft"), t.0.join(".trash"))
}

#[test]
fn reads_fabric_jar_with_nested_jars_and_packages() {
    let t = Temp::new("fabric");
    let jar = zip_bytes(&[
        ("fabric.mod.json", FABRIC),
        ("META-INF/jars/lib-1.jar", b"PK-not-opened"),
        ("com/example/alpha/Main.class", b""),
        ("com/example/alpha/util/Util.class", b""),
        ("module-info.class", b""),
        ("META-INF/versions/9/com/example/alpha/Main.class", b""),
    ]);
    write(&t.0.join("a.jar"), &jar);
    let info = read_jar(&t.0.join("a.jar"));
    assert_eq!(info.problem, None);
    assert_eq!(info.mods[0].id, "alpha");
    assert_eq!(info.mods[0].loader, ModLoader::Fabric);
    assert_eq!(info.mods[0].icon.as_deref(), Some("assets/alpha/icon.png"));
    assert_eq!(info.nested_jars, ["META-INF/jars/lib-1.jar"]);
    assert_eq!(info.packages, ["com.example.alpha"]);
}

#[test]
fn reads_every_metadata_format_from_a_jar() {
    let t = Temp::new("formats");
    let forge = b"[[mods]]\nmodId=\"forgey\"\nversion=\"${file.jarVersion}\"\n";
    let manifest = b"Manifest-Version: 1.0\nImplementation-Version: 3.1\n";
    type Case<'a> = (&'a str, Vec<(&'a str, &'a [u8])>, ModLoader);
    let cases: Vec<Case<'_>> = vec![
        (
            "q",
            vec![("quilt.mod.json", br#"{"quilt_loader":{"id":"q"}}"#)],
            ModLoader::Quilt,
        ),
        (
            "f",
            vec![
                ("META-INF/mods.toml", forge),
                ("META-INF/MANIFEST.MF", manifest),
            ],
            ModLoader::Forge,
        ),
        (
            "n",
            vec![("META-INF/neoforge.mods.toml", forge)],
            ModLoader::NeoForge,
        ),
        (
            "l",
            vec![("mcmod.info", br#"[{"modid":"legacy"}]"#)],
            ModLoader::LegacyForge,
        ),
    ];
    for (name, entries, loader) in cases {
        let path = t.0.join(format!("{name}.jar"));
        write(&path, &zip_bytes(&entries));
        let info = read_jar(&path);
        assert_eq!(info.mods.len(), 1, "{name}: {:?}", info.problem);
        assert_eq!(info.mods[0].loader, loader);
        if name == "f" {
            assert_eq!(info.mods[0].version.as_deref(), Some("3.1"));
        }
    }
}

#[test]
fn damaged_jars_give_a_problem_not_a_panic() {
    let t = Temp::new("damaged");
    write(&t.0.join("garbage.jar"), b"this is not a zip");
    write(&t.0.join("empty.jar"), b"");
    let bad_meta = zip_bytes(&[("fabric.mod.json", b"{broken")]);
    write(&t.0.join("badmeta.jar"), &bad_meta);
    let huge = vec![b' '; usize::try_from(MAX_METADATA_BYTES).unwrap() + 10];
    write(
        &t.0.join("huge.jar"),
        &zip_bytes(&[("fabric.mod.json", &huge)]),
    );
    let mut truncated = zip_bytes(&[("fabric.mod.json", FABRIC)]);
    truncated.truncate(truncated.len() / 2);
    write(&t.0.join("cut.jar"), &truncated);
    let plain = zip_bytes(&[("a/B.class", b"")]);
    write(&t.0.join("plain.jar"), &plain);

    for name in ["garbage", "empty", "badmeta", "huge", "cut"] {
        let info = read_jar(&t.0.join(format!("{name}.jar")));
        assert!(info.mods.is_empty(), "{name}");
        assert!(info.problem.is_some(), "{name}");
    }
    assert!(read_jar(&t.0.join("missing.jar")).problem.is_some());
    // A library without metadata is not a problem.
    let info = read_jar(&t.0.join("plain.jar"));
    assert!(info.problem.is_none() && info.mods.is_empty());
}

#[test]
fn package_helpers() {
    assert_eq!(class_package("a/b/C.class").as_deref(), Some("a.b"));
    assert_eq!(class_package("C.class"), None);
    assert_eq!(class_package("module-info.class"), None);
    assert_eq!(class_package("a/b/readme.txt"), None);
    assert_eq!(class_package("a//C.class"), None);
    assert_eq!(
        class_package("META-INF/versions/17/x/y/Z.class").as_deref(),
        Some("x.y")
    );
    assert_eq!(class_package("META-INF/foo/Z.class"), None);
    let set = ["a.b", "a.b.c", "a.bc", "x"].map(String::from).into();
    assert_eq!(reduce_packages(set), ["a.b", "a.bc", "x"]);
}

#[test]
fn lists_and_toggles_mods() {
    let t = Temp::new("toggle");
    let r = fixture(&t);
    let mods = r.dir(ResourceKind::Mods);
    write(
        &mods.join("a.jar"),
        &zip_bytes(&[("fabric.mod.json", FABRIC)]),
    );
    write(
        &mods.join("b.jar.disabled"),
        &zip_bytes(&[("x/Y.class", b"")]),
    );
    write(&mods.join("notes.txt"), b"hi");
    write(&mods.join(".hidden"), b"");

    let list = r.list(ResourceKind::Mods).unwrap();
    let names: Vec<_> = list.iter().map(|e| (e.file.as_str(), e.enabled)).collect();
    assert_eq!(
        names,
        [("a.jar", true), ("b.jar", false), ("notes.txt", true)]
    );
    assert_eq!(list[0].mods[0].name, "Alpha");

    r.set_enabled(ResourceKind::Mods, "a.jar", false).unwrap();
    assert!(mods.join("a.jar.disabled").is_file() && !mods.join("a.jar").exists());
    r.set_enabled(ResourceKind::Mods, "a.jar", false).unwrap();
    r.set_enabled(ResourceKind::Mods, "a.jar.disabled", true)
        .unwrap();
    assert!(mods.join("a.jar").is_file());
    let err = r
        .set_enabled(ResourceKind::Mods, "nope.jar", true)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    for bad in ["", "..", "../a.jar", "a/b.jar", "c:x"] {
        let err = r.set_enabled(ResourceKind::Mods, bad, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput, "{bad}");
    }
    write(&mods.join("b.jar"), b"x");
    let err = r
        .set_enabled(ResourceKind::Mods, "b.jar", true)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
}

#[test]
fn adds_without_replacing_and_checks_the_extension() {
    let t = Temp::new("add");
    let r = fixture(&t);
    let src = t.0.join("src");
    write(&src.join("new.jar"), b"jar");
    write(&src.join("pack.zip"), b"zip");
    write(&src.join("readme.txt"), b"txt");

    assert_eq!(
        r.add(ResourceKind::Mods, &src.join("new.jar")).unwrap(),
        "new.jar"
    );
    assert_eq!(
        fs::read(r.dir(ResourceKind::Mods).join("new.jar")).unwrap(),
        b"jar"
    );
    let err = r.add(ResourceKind::Mods, &src.join("new.jar")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    r.set_enabled(ResourceKind::Mods, "new.jar", false).unwrap();
    let err = r.add(ResourceKind::Mods, &src.join("new.jar")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(
        r.add(ResourceKind::Mods, &src.join("pack.zip"))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        r.add(ResourceKind::ResourcePacks, &src.join("readme.txt"))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        r.add(ResourceKind::GlobalDataPacks, &src.join("pack.zip"))
            .unwrap(),
        "pack.zip"
    );
    assert!(
        t.0.join("minecraft/global_packs/required_data/pack.zip")
            .is_file()
    );
    assert_eq!(
        r.add(ResourceKind::ShaderPacks, &src.join("missing.zip"))
            .unwrap_err()
            .kind,
        ErrorKind::NotFound
    );
    // No temporary files stay behind.
    assert_eq!(r.list(ResourceKind::Mods).unwrap().len(), 1);
}

#[test]
fn removal_moves_to_the_trash() {
    let t = Temp::new("remove");
    let r = fixture(&t);
    let mods = r.dir(ResourceKind::Mods);
    write(&mods.join("a.jar.disabled"), b"data");
    let moved = r.remove(ResourceKind::Mods, "a.jar").unwrap();
    assert_eq!(fs::read(&moved).unwrap(), b"data");
    assert!(moved.starts_with(t.0.join(".trash")));
    assert!(r.list(ResourceKind::Mods).unwrap().is_empty());
    assert_eq!(
        r.remove(ResourceKind::Mods, "a.jar").unwrap_err().kind,
        ErrorKind::NotFound
    );
    // Two removals of the same name in the same second do not collide.
    write(&mods.join("a.jar"), b"1");
    let first = r.remove(ResourceKind::Mods, "a.jar").unwrap();
    write(&mods.join("a.jar"), b"2");
    let second = r.remove(ResourceKind::Mods, "a.jar").unwrap();
    assert_ne!(first, second);
    assert!(r.list(ResourceKind::DataPacks).unwrap().is_empty());
}

#[test]
fn class_index_attributes_classes_to_enabled_mods() {
    let t = Temp::new("index");
    let r = fixture(&t);
    let mods = r.dir(ResourceKind::Mods);
    write(
        &mods.join("alpha.jar"),
        &zip_bytes(&[
            ("fabric.mod.json", FABRIC),
            ("com/example/alpha/Main.class", b""),
        ]),
    );
    write(
        &mods.join("lib.jar"),
        &zip_bytes(&[("org/lib/Thing.class", b"")]),
    );
    write(
        &mods.join("off.jar.disabled"),
        &zip_bytes(&[("org/off/X.class", b"")]),
    );
    write(&mods.join("broken.jar"), b"junk");

    let index = r.class_index().unwrap();
    assert_eq!(index.owners("com.example.alpha.Main"), ["alpha"]);
    assert_eq!(index.owners("com.example.alpha.sub.Deep"), ["alpha"]);
    assert_eq!(index.owners("org.lib.Thing"), ["lib"]);
    assert!(index.owners("org.off.X").is_empty());
    assert!(index.owners("java.lang.String").is_empty());
    assert_eq!(index.mods().len(), 2);
}
