use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use vl_core::resources::ResourceKind;
use vl_crash::ClassIndex;

use crate::Resources;
use crate::cache::{MAX_BYTES, file_name};

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("vl-resources-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn meta(&self) -> PathBuf {
        self.0.join("meta")
    }

    fn cache_file(&self, kind: ResourceKind) -> PathBuf {
        self.meta().join(file_name(kind))
    }

    fn resources(&self) -> Resources {
        Resources::new(self.0.join("minecraft"), self.0.join(".trash")).with_cache_dir(self.meta())
    }

    fn mods(&self) -> PathBuf {
        self.0.join("minecraft").join("mods")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn jar(id: &str, version: &str) -> Vec<u8> {
    let meta = format!(r#"{{"id":"{id}","version":"{version}","name":"{id}"}}"#);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer.start_file("fabric.mod.json", options).unwrap();
    writer.write_all(meta.as_bytes()).unwrap();
    writer
        .start_file(format!("com/example/{id}/Main.class"), options)
        .unwrap();
    writer.write_all(b"").unwrap();
    writer.finish().unwrap().into_inner()
}

fn put(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

fn set_mtime(path: &Path, time: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// Overwrites the file with bytes of the same length and gives it back its modification time,
/// so only an unwanted read of the jar can notice.
fn swap_keeping_stamp(path: &Path, bytes: &[u8]) {
    let before = mtime(path);
    assert_eq!(fs::metadata(path).unwrap().len(), bytes.len() as u64);
    fs::write(path, bytes).unwrap();
    set_mtime(path, before);
}

fn versions(r: &Resources) -> Vec<Option<String>> {
    r.list(ResourceKind::Mods)
        .unwrap()
        .iter()
        .map(|e| e.mods.first().and_then(|m| m.version.clone()))
        .collect()
}

fn some(v: &str) -> Vec<Option<String>> {
    vec![Some(v.to_owned())]
}

#[test]
fn a_second_list_does_not_open_the_jar() {
    let t = Temp::new("hit");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    assert_eq!(versions(&r), some("1.0"));
    assert!(t.cache_file(ResourceKind::Mods).is_file());

    swap_keeping_stamp(&t.mods().join("a.jar"), &jar("alpha", "2.0"));
    assert_eq!(versions(&r), some("1.0"));
}

#[test]
fn a_changed_file_is_read_again() {
    let t = Temp::new("changed");
    let r = t.resources();
    let path = t.mods().join("a.jar");
    put(&path, &jar("alpha", "1.0"));
    versions(&r);
    // A longer version changes the size.
    put(&path, &jar("alpha", "1.10.0"));
    assert_eq!(versions(&r), some("1.10.0"));

    // Same size, later modification time.
    let later = mtime(&path) + Duration::from_secs(5);
    fs::write(&path, jar("alpha", "2.0.0")).unwrap();
    set_mtime(&path, later);
    assert_eq!(versions(&r), some("2.0.0"));
}

#[test]
fn disabling_and_enabling_keep_the_cache_entry() {
    let t = Temp::new("toggle");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    versions(&r);
    r.set_enabled(ResourceKind::Mods, "a.jar", false).unwrap();
    swap_keeping_stamp(&t.mods().join("a.jar.disabled"), &jar("alpha", "2.0"));
    let listed = r.list(ResourceKind::Mods).unwrap();
    assert!(!listed[0].enabled);
    assert_eq!(listed[0].mods[0].version.as_deref(), Some("1.0"));
}

#[test]
fn a_damaged_cache_is_rebuilt() {
    let t = Temp::new("damaged");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    versions(&r);
    let cache = t.cache_file(ResourceKind::Mods);
    for garbage in [&b"{ not json"[..], b"", b"\xff\xfe\x00", br#"{"format":1}"#] {
        fs::write(&cache, garbage).unwrap();
        assert_eq!(versions(&r), some("1.0"));
        // The entry is remembered again.
        let text = fs::read_to_string(&cache).unwrap();
        assert!(text.contains("\"a.jar\""), "{text}");
    }
}

#[test]
fn another_format_version_or_a_huge_file_is_ignored() {
    let t = Temp::new("format");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    versions(&r);
    let cache = t.cache_file(ResourceKind::Mods);

    let text = fs::read_to_string(&cache).unwrap();
    fs::write(&cache, text.replace("\"format\":1", "\"format\":99")).unwrap();
    swap_keeping_stamp(&t.mods().join("a.jar"), &jar("alpha", "3.0"));
    assert_eq!(versions(&r), some("3.0"));

    let huge = File::options().write(true).open(&cache).unwrap();
    huge.set_len(MAX_BYTES + 1).unwrap();
    drop(huge);
    swap_keeping_stamp(&t.mods().join("a.jar"), &jar("alpha", "4.0"));
    assert_eq!(versions(&r), some("4.0"));
    assert!(fs::metadata(&cache).unwrap().len() < MAX_BYTES);
}

#[test]
fn removed_and_renamed_files_leave_the_cache() {
    let t = Temp::new("gone");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    put(&t.mods().join("b.jar"), &jar("beta", "1.0"));
    put(&t.mods().join("c.jar"), &jar("gamma", "1.0"));
    versions(&r);

    r.remove(ResourceKind::Mods, "a.jar").unwrap();
    let cache = t.cache_file(ResourceKind::Mods);
    assert!(!fs::read_to_string(&cache).unwrap().contains("\"a.jar\""));

    // Deleted or renamed outside the launcher: pruned by the next listing.
    fs::remove_file(t.mods().join("b.jar")).unwrap();
    fs::rename(t.mods().join("c.jar"), t.mods().join("d.jar")).unwrap();
    let names: Vec<String> = r
        .list(ResourceKind::Mods)
        .unwrap()
        .into_iter()
        .map(|e| e.file)
        .collect();
    assert_eq!(names, ["d.jar"]);
    let text = fs::read_to_string(&cache).unwrap();
    assert!(text.contains("\"d.jar\""));
    assert!(!text.contains("\"b.jar\"") && !text.contains("\"c.jar\""));
}

#[test]
fn adding_a_file_over_a_stale_entry_reads_it() {
    let t = Temp::new("add");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    versions(&r);
    r.remove(ResourceKind::Mods, "a.jar").unwrap();

    let source = t.0.join("src").join("a.jar");
    put(&source, &jar("alpha", "9.9"));
    r.add(ResourceKind::Mods, &source).unwrap();
    assert_eq!(versions(&r), some("9.9"));
}

#[test]
fn the_class_index_comes_from_the_cache() {
    let t = Temp::new("index");
    let r = t.resources();
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    put(&t.mods().join("b.jar"), &jar("beta", "1.0"));
    versions(&r);
    r.set_enabled(ResourceKind::Mods, "b.jar", false).unwrap();
    assert_eq!(
        r.class_index().unwrap().owners("com.example.alpha.Main"),
        ["alpha"]
    );
    // Not opened again, and the entry of the disabled jar survives the index build.
    swap_keeping_stamp(&t.mods().join("a.jar"), &jar("omega", "1.0"));
    let index = r.class_index().unwrap();
    assert_eq!(index.owners("com.example.alpha.Main"), ["alpha"]);
    assert!(index.owners("com.example.beta.Main").is_empty());
    let text = fs::read_to_string(t.cache_file(ResourceKind::Mods)).unwrap();
    assert!(text.contains("\"b.jar\""));
    // The listing shares the entries.
    assert_eq!(versions(&r).len(), 2);
}

#[test]
fn every_folder_and_world_has_its_own_cache() {
    let t = Temp::new("scopes");
    let r = t.resources();
    let mcmeta = br#"{"pack":{"pack_format":15,"description":"hi"}}"#;
    let pack = {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("pack.mcmeta", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(mcmeta).unwrap();
        w.finish().unwrap().into_inner()
    };
    put(&t.0.join("minecraft/mods/a.jar"), &jar("alpha", "1.0"));
    put(&t.0.join("minecraft/resourcepacks/p.zip"), &pack);
    put(&t.0.join("minecraft/saves/w/level.dat"), b"x");
    put(&t.0.join("minecraft/saves/w/datapacks/d.zip"), &pack);

    r.list(ResourceKind::Mods).unwrap();
    let packs = r.list(ResourceKind::ResourcePacks).unwrap();
    assert!(packs[0].pack.is_some());
    let world = r.world("w").unwrap();
    assert!(
        world.list(ResourceKind::DataPacks).unwrap()[0]
            .pack
            .is_some()
    );

    assert!(t.cache_file(ResourceKind::Mods).is_file());
    assert!(t.cache_file(ResourceKind::ResourcePacks).is_file());
    assert!(!t.cache_file(ResourceKind::DataPacks).exists());
    let in_world =
        t.0.join("minecraft/saves/w/.vibelauncher")
            .join(file_name(ResourceKind::DataPacks));
    assert!(in_world.is_file());

    // A pack is served from its cache too.
    swap_keeping_stamp(
        &t.0.join("minecraft/resourcepacks/p.zip"),
        &vec![0u8; pack.len()],
    );
    assert!(
        r.list(ResourceKind::ResourcePacks).unwrap()[0]
            .pack
            .is_some()
    );
}

#[test]
fn without_a_cache_directory_nothing_is_written() {
    let t = Temp::new("off");
    let r = Resources::new(t.0.join("minecraft"), t.0.join(".trash"));
    put(&t.mods().join("a.jar"), &jar("alpha", "1.0"));
    versions(&r);
    assert!(!t.meta().exists());
}

/// 300 synthetic mods: first listing (cold cache) against the second (warm cache).
/// Run with `cargo test -p vl-resources --release -- --ignored --nocapture`.
#[test]
#[ignore = "timing measurement"]
fn timing_of_300_mods() {
    let t = Temp::new("timing");
    let r = t.resources();
    for i in 0..300 {
        let id = format!("mod{i}");
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for n in 0..400 {
            w.start_file(format!("com/example/{id}/c{n}/Class{n}.class"), options)
                .unwrap();
            w.write_all(&[7u8; 512]).unwrap();
        }
        w.start_file("fabric.mod.json", options).unwrap();
        w.write_all(format!(r#"{{"id":"{id}","version":"1.0"}}"#).as_bytes())
            .unwrap();
        put(
            &t.mods().join(format!("{id}.jar")),
            &w.finish().unwrap().into_inner(),
        );
    }
    let first = Instant::now();
    assert_eq!(r.list(ResourceKind::Mods).unwrap().len(), 300);
    let first = first.elapsed();
    let second = Instant::now();
    assert_eq!(r.list(ResourceKind::Mods).unwrap().len(), 300);
    let second = second.elapsed();
    println!("300 mods: first list {first:?}, second list {second:?}");
}

/// Digests made from the first byte of a file, counting how often the file is read.
fn digest_all(r: &Resources, files: &[(&str, bool)], reads: &mut u32) -> Vec<crate::Digests> {
    let names: Vec<(String, bool)> = files.iter().map(|(n, e)| ((*n).to_owned(), *e)).collect();
    r.digests(ResourceKind::Mods, &names, |path, known| {
        if known.fingerprint.is_some() {
            return Ok(known.clone());
        }
        *reads += 1;
        let bytes = fs::read(path)?;
        Ok(crate::Digests {
            size: Some(bytes.len() as u64),
            sha1: Some(format!("{:02x}", bytes.first().copied().unwrap_or(0))),
            sha512: None,
            fingerprint: Some(u32::from(bytes.first().copied().unwrap_or(0))),
        })
    })
    .into_iter()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn digests_are_remembered_while_the_file_is_unchanged() {
    let t = Temp::new("digests");
    let r = t.resources();
    let path = t.mods().join("a.jar");
    put(&path, b"A-file");
    let mut reads = 0;
    let first = digest_all(&r, &[("a.jar", true)], &mut reads);
    assert_eq!(first[0].fingerprint, Some(u32::from(b'A')));
    assert_eq!(reads, 1);

    // Same name, size and time: the file is not read again.
    swap_keeping_stamp(&path, b"B-file");
    let again = digest_all(&r, &[("a.jar", true)], &mut reads);
    assert_eq!(again, first);
    assert_eq!(reads, 1);

    // A listing keeps them, and so do disabling and the metadata of the jar.
    versions(&r);
    r.set_enabled(ResourceKind::Mods, "a.jar", false).unwrap();
    assert_eq!(digest_all(&r, &[("a.jar", false)], &mut reads), first);
    assert_eq!(reads, 1);
    r.set_enabled(ResourceKind::Mods, "a.jar", true).unwrap();

    // A later modification time: read again.
    let later = mtime(&path) + Duration::from_secs(5);
    set_mtime(&path, later);
    let changed = digest_all(&r, &[("a.jar", true)], &mut reads);
    assert_eq!(changed[0].fingerprint, Some(u32::from(b'B')));
    assert_eq!(reads, 2);

    // A missing file is an error and nothing is remembered for it.
    let names = vec![("gone.jar".to_owned(), true)];
    let results = r.digests(ResourceKind::Mods, &names, |p, _| {
        fs::read(p).map(|_| crate::Digests::default())
    });
    assert!(results[0].is_err());
}

#[test]
fn digests_of_removed_files_leave_the_cache() {
    let t = Temp::new("digests-gone");
    let r = t.resources();
    put(&t.mods().join("a.jar"), b"A");
    put(&t.mods().join("b.jar"), b"B");
    let mut reads = 0;
    digest_all(&r, &[("a.jar", true), ("b.jar", true)], &mut reads);
    let cache = t.cache_file(ResourceKind::Mods);
    let text = fs::read_to_string(&cache).unwrap();
    assert!(
        text.contains("\"digests\"") && text.contains("\"b.jar\""),
        "{text}"
    );

    r.remove(ResourceKind::Mods, "a.jar").unwrap();
    fs::remove_file(t.mods().join("b.jar")).unwrap();
    r.list(ResourceKind::Mods).unwrap();
    let text = fs::read_to_string(&cache).unwrap();
    assert!(!text.contains("a.jar") && !text.contains("b.jar"), "{text}");

    // A file that is not a jar keeps its digests through a listing.
    put(&t.mods().join("notes.txt"), b"N");
    digest_all(&r, &[("notes.txt", true)], &mut reads);
    r.list(ResourceKind::Mods).unwrap();
    let before = reads;
    digest_all(&r, &[("notes.txt", true)], &mut reads);
    assert_eq!(reads, before);
}

#[test]
fn without_a_cache_folder_every_digest_is_computed() {
    let t = Temp::new("digests-nocache");
    let r = Resources::new(t.0.join("minecraft"), t.0.join(".trash"));
    put(&t.mods().join("a.jar"), b"A");
    let mut reads = 0;
    digest_all(&r, &[("a.jar", true)], &mut reads);
    digest_all(&r, &[("a.jar", true)], &mut reads);
    assert_eq!(reads, 2);
    assert!(!t.meta().exists());
}
