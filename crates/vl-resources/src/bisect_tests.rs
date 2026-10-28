use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;

use vl_core::ErrorKind;
use vl_core::bisect::{BisectOutcome, BisectState, BisectStatus, BisectVerdict};
use vl_core::resources::ResourceKind;

use crate::{Bisect, Resources};

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vl-bisect-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("minecraft").join("mods")).unwrap();
        Self(dir)
    }

    fn mods(&self) -> PathBuf {
        self.0.join("minecraft").join("mods")
    }

    fn bisect(&self) -> Bisect {
        let resources = Resources::new(self.0.join("minecraft"), self.0.join(".trash"));
        Bisect::new(resources, &self.0.join(".vibelauncher"))
    }

    /// Writes a Fabric jar for mod `id` requiring `needs`.
    fn jar(&self, id: &str, needs: &[&str]) {
        let depends: Vec<String> = needs.iter().map(|n| format!("\"{n}\":\"*\"")).collect();
        let json = format!(
            "{{\"id\":\"{id}\",\"version\":\"1\",\"depends\":{{{}}}}}",
            depends.join(",")
        );
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(json.as_bytes()).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        fs::write(self.mods().join(format!("{id}.jar")), bytes).unwrap();
    }

    /// The files that are visible to the game.
    fn enabled(&self) -> BTreeSet<String> {
        let list = Resources::new(self.0.join("minecraft"), self.0.join(".trash"))
            .list(ResourceKind::Mods)
            .unwrap();
        list.into_iter()
            .filter(|e| e.enabled)
            .map(|e| e.file)
            .collect()
    }

    fn state(&self) -> Vec<(String, bool)> {
        Resources::new(self.0.join("minecraft"), self.0.join(".trash"))
            .list(ResourceKind::Mods)
            .unwrap()
            .into_iter()
            .map(|e| (e.file, e.enabled))
            .collect()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> Temp {
    let t = Temp::new(name);
    t.jar("fabric-api", &[]);
    for i in 0..6 {
        let needs: &[&str] = if i % 2 == 0 { &["fabric-api"] } else { &[] };
        t.jar(&format!("mod{i}"), needs);
    }
    t
}

/// Answers every step as the game would if `culprit` is the broken mod.
fn drive(t: &Temp, bisect: &Bisect, mut status: BisectStatus, culprit: &str) -> BisectStatus {
    while status.state == BisectState::Running {
        let on = t.enabled();
        assert_eq!(on, status.enabled.iter().cloned().collect::<BTreeSet<_>>());
        let outcome = if on.contains(culprit) {
            BisectOutcome::Bad
        } else {
            BisectOutcome::Good
        };
        status = bisect.report(outcome).unwrap();
    }
    status
}

#[test]
fn finds_the_mod_and_restores_everything() {
    let t = fixture("find");
    // A mod the user had disabled before stays disabled.
    t.jar("old", &[]);
    fs::rename(t.mods().join("old.jar"), t.mods().join("old.jar.disabled")).unwrap();
    let before = t.state();
    let bisect = t.bisect();
    let start = bisect.start(&[]).unwrap();
    assert_eq!(start.state, BisectState::Running);
    assert_eq!(start.candidates, 7);
    assert!(
        !t.state().iter().all(|(_, on)| *on),
        "the first step disables some mods"
    );
    let end = drive(&t, &bisect, start, "mod3.jar");
    assert_eq!(
        end.verdict,
        Some(BisectVerdict::Found {
            culprits: vec!["mod3.jar".into()],
            needs: Vec::new()
        })
    );
    assert_eq!(t.state(), before);
    assert_eq!(bisect.status().unwrap().state, BisectState::Finished);
    bisect.stop().unwrap();
    assert_eq!(bisect.status().unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn never_disables_a_mod_that_an_enabled_one_needs() {
    let t = fixture("deps");
    let bisect = t.bisect();
    let mut status = bisect.start(&[]).unwrap();
    while status.state == BisectState::Running {
        let on = t.enabled();
        for even in [0, 2, 4] {
            if on.contains(&format!("mod{even}.jar")) {
                assert!(on.contains("fabric-api.jar"), "mod{even} lost its library");
            }
        }
        status = bisect.report(BisectOutcome::Good).unwrap();
    }
    assert_eq!(status.verdict, Some(BisectVerdict::NotReproduced));
    assert_eq!(t.enabled().len(), 7);
}

#[test]
fn stop_restores_a_session_in_progress() {
    let t = fixture("stop");
    let bisect = t.bisect();
    bisect.start(&[]).unwrap();
    bisect.report(BisectOutcome::Bad).unwrap();
    assert!(t.enabled().len() < 7);
    bisect.stop().unwrap();
    assert_eq!(t.enabled().len(), 7);
    assert!(!t.0.join(".vibelauncher").join("bisect.json").exists());
    // Stopping again is harmless.
    bisect.stop().unwrap();
}

#[test]
fn a_new_handle_continues_and_restores_after_a_restart() {
    let t = fixture("restart");
    let first = t.bisect().start(&[]).unwrap();
    assert_eq!(first.enabled.len(), 7, "the first step runs every mod");
    // The launcher was closed: a fresh handle reads the file.
    let again = t.bisect();
    assert_eq!(again.status().unwrap(), first);
    let end = drive(&t, &again, first, "mod1.jar");
    assert!(matches!(end.verdict, Some(BisectVerdict::Found { .. })));
    // A crash mid-session: simulate it by starting again and only stopping.
    again.start(&[]).unwrap();
    drop(again);
    t.bisect().stop().unwrap();
    assert_eq!(t.enabled().len(), 7);
}

#[test]
fn refuses_a_second_session_but_replaces_a_finished_one() {
    let t = fixture("second");
    let bisect = t.bisect();
    bisect.start(&[]).unwrap();
    assert_eq!(bisect.start(&[]).unwrap_err().kind, ErrorKind::Conflict);
    // The refused start left the running session alone.
    assert_eq!(bisect.status().unwrap().state, BisectState::Running);
    bisect.stop().unwrap();
    let status = bisect.start(&[]).unwrap();
    let end = drive(&t, &bisect, status, "mod0.jar");
    assert_eq!(end.state, BisectState::Finished);
    assert_eq!(bisect.start(&[]).unwrap().state, BisectState::Running);
    bisect.stop().unwrap();
}

#[test]
fn a_failure_while_applying_a_step_restores_the_mods() {
    let t = fixture("fail");
    let bisect = t.bisect();
    bisect.start(&[]).unwrap();
    // The first step runs every mod; the second disables most of them.
    let status = bisect.report(BisectOutcome::Bad).unwrap();
    // A file appears next to a disabled one: renaming it back would replace data, so it fails.
    let off = status
        .suspects
        .iter()
        .find(|f| !status.enabled.contains(f))
        .cloned()
        .unwrap();
    fs::write(t.mods().join(&off), b"intruder").unwrap();
    let err = bisect.report(BisectOutcome::Good).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    // The conflict also blocks restoring that one file, so the session stays for a retry.
    assert!(err.message.contains("bisect stop"), "{}", err.message);
    assert!(t.0.join(".vibelauncher").join("bisect.json").exists());
    assert_eq!(t.enabled().len(), 7);
    fs::remove_file(t.mods().join(&off)).unwrap();
    bisect.stop().unwrap();
    assert_eq!(t.enabled().len(), 7);
    assert!(!t.0.join(".vibelauncher").join("bisect.json").exists());
}

#[test]
fn hints_are_tried_first() {
    let t = fixture("hint");
    let bisect = t.bisect();
    let status = bisect.start(&["mod5".to_owned()]).unwrap();
    // A mod without dependencies is tried with the libraries enabled.
    assert_eq!(
        status.enabled,
        vec!["fabric-api.jar".to_owned(), "mod5.jar".to_owned()]
    );
    // The libraries alone are checked next, so a broken library is not blamed on the hint.
    let zero = bisect.report(BisectOutcome::Bad).unwrap();
    assert_eq!(zero.enabled, vec!["fabric-api.jar".to_owned()]);
    let end = bisect.report(BisectOutcome::Good).unwrap();
    assert_eq!(end.state, BisectState::Finished);
    assert!(matches!(
        end.verdict,
        Some(BisectVerdict::Found { ref culprits, .. }) if culprits == &["mod5.jar"]
    ));
    assert_eq!(t.enabled().len(), 7);
}

#[test]
fn a_session_needs_two_mods_and_a_readable_file() {
    let t = Temp::new("few");
    t.jar("only", &[]);
    assert_eq!(
        t.bisect().start(&[]).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    let dir = t.0.join(".vibelauncher");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("bisect.json"), b"{ not json").unwrap();
    assert_eq!(t.bisect().status().unwrap_err().kind, ErrorKind::Integrity);
    assert_eq!(
        t.bisect().start(&[]).unwrap_err().kind,
        ErrorKind::Integrity
    );
    assert_eq!(t.enabled().len(), 1);
}

#[test]
fn sets_outside_the_steps_are_restored_by_stop() {
    let t = fixture("sets");
    let bisect = t.bisect();
    assert_eq!(bisect.enabled_count().unwrap(), 7);
    assert_eq!(
        bisect.apply_set(&[]).unwrap_err().kind,
        ErrorKind::NotFound,
        "a set needs a session"
    );
    bisect.start(&[]).unwrap();
    bisect.apply_set(&[]).unwrap();
    assert!(t.enabled().is_empty());
    let without = bisect.without(&["fabric-api.jar".to_owned()]).unwrap();
    assert_eq!(without, ["mod1.jar", "mod3.jar", "mod5.jar"]);
    bisect.apply_set(&without).unwrap();
    assert_eq!(t.enabled().len(), 3);
    bisect.stop().unwrap();
    assert_eq!(t.enabled().len(), 7);
}

#[test]
fn a_bundled_module_counts_as_provided_by_its_jar() {
    let t = Temp::new("bundled");
    // `api.jar` bundles `screen-api`; `menu` requires the module, not the api.
    let module = {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(br#"{"id":"screen-api","version":"1"}"#)
            .unwrap();
        w.finish().unwrap().into_inner()
    };
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    w.start_file("fabric.mod.json", options).unwrap();
    w.write_all(br#"{"id":"api","version":"1","jars":[{"file":"META-INF/jars/screen-api.jar"}]}"#)
        .unwrap();
    w.start_file("META-INF/jars/screen-api.jar", options)
        .unwrap();
    w.write_all(&module).unwrap();
    fs::write(t.mods().join("api.jar"), w.finish().unwrap().into_inner()).unwrap();
    t.jar("menu", &["screen-api"]);
    t.jar("other", &[]);
    let bisect = t.bisect();
    let mut status = bisect.start(&[]).unwrap();
    while status.state == BisectState::Running {
        let on = t.enabled();
        if on.contains("menu.jar") {
            assert!(on.contains("api.jar"), "menu lost the jar of its module");
        }
        status = bisect.report(BisectOutcome::Bad).unwrap();
    }
    bisect.stop().unwrap();
}
