use std::collections::BTreeSet;

use vl_core::bisect::{BisectOutcome, BisectVerdict};

use crate::{Candidate, Session};

fn cand(file: &str, requires: &[&str]) -> Candidate {
    Candidate {
        file: file.to_owned(),
        provides: vec![file.trim_end_matches(".jar").to_owned()],
        requires: requires.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// `n` plain mods `m0.jar` .. `m{n-1}.jar`.
fn plain(n: usize) -> Vec<Candidate> {
    (0..n).map(|i| cand(&format!("m{i}.jar"), &[])).collect()
}

/// Runs a search against `oracle` (true when the problem shows with the enabled set), checking
/// every probe is closed under dependencies. Returns the session and the probes asked.
fn run(
    candidates: Vec<Candidate>,
    hints: &[&str],
    oracle: impl Fn(&BTreeSet<String>) -> BisectOutcome,
) -> (Session, Vec<BTreeSet<String>>) {
    let hints: Vec<String> = hints.iter().map(|s| (*s).to_owned()).collect();
    let mut session = Session::new(candidates.clone(), &hints).unwrap();
    let mut asked = Vec::new();
    let mut guard = 0;
    while let Some(probe) = session.probe() {
        guard += 1;
        assert!(guard < 200, "the search does not end");
        let enabled: BTreeSet<String> = probe.into_iter().collect();
        for c in &candidates {
            if enabled.contains(&c.file) {
                for r in &c.requires {
                    let provider = candidates.iter().find(|o| o.provides.contains(r));
                    if let Some(p) = provider {
                        assert!(enabled.contains(&p.file), "{} enabled without {r}", c.file);
                    }
                }
            }
        }
        let outcome = oracle(&enabled);
        asked.push(enabled);
        session.answer(outcome).unwrap();
    }
    (session, asked)
}

fn crashes_with(culprits: &'static [&'static str]) -> impl Fn(&BTreeSet<String>) -> BisectOutcome {
    move |enabled| {
        if culprits.iter().all(|c| enabled.contains(*c)) {
            BisectOutcome::Bad
        } else {
            BisectOutcome::Good
        }
    }
}

fn found(session: &Session) -> (Vec<String>, Vec<String>) {
    match session.verdict() {
        Some(BisectVerdict::Found { culprits, needs }) => (culprits.clone(), needs.clone()),
        other => panic!("expected a culprit, got {other:?}"),
    }
}

#[test]
fn finds_one_culprit_in_about_log2_steps() {
    for target in [0, 7, 19, 31] {
        let name: &'static str = Box::leak(format!("m{target}.jar").into_boxed_str());
        let culprits: &'static [&'static str] = Box::leak(vec![name].into_boxed_slice());
        let (session, asked) = run(plain(32), &[], crashes_with(culprits));
        assert_eq!(found(&session).0, vec![name.to_owned()]);
        // The check of the starting point, then at most two probes per halving.
        assert!(asked.len() <= 1 + 2 * 5, "{} steps for {name}", asked.len());
        assert!(session.is_finished());
        assert_eq!(session.steps_estimate(), 0);
    }
}

#[test]
fn estimate_is_log2_of_the_mods() {
    let session = Session::new(plain(32), &[]).unwrap();
    // Five halvings and the check of the starting point.
    assert_eq!(session.steps_estimate(), 6);
}

#[test]
fn finds_two_mods_that_crash_together() {
    let (session, _) = run(plain(16), &[], crashes_with(&["m3.jar", "m12.jar"]));
    let (culprits, _) = found(&session);
    assert_eq!(culprits, vec!["m3.jar", "m12.jar"]);
}

#[test]
fn finds_two_neighbours_and_far_pairs() {
    for (a, b) in [
        ("m0.jar", "m1.jar"),
        ("m0.jar", "m15.jar"),
        ("m7.jar", "m8.jar"),
    ] {
        let culprits: &'static [&'static str] = Box::leak(vec![a, b].into_boxed_slice());
        let (session, _) = run(plain(16), &[], crashes_with(culprits));
        let mut got = found(&session).0;
        got.sort();
        assert_eq!(got, vec![a.to_owned(), b.to_owned()]);
    }
}

fn with_libraries() -> Vec<Candidate> {
    let mut mods = vec![cand("fabric-api.jar", &[]), cand("cloth-config.jar", &[])];
    for i in 0..8 {
        let needs: &[&str] = match i % 3 {
            0 => &["fabric-api"],
            1 => &["fabric-api", "cloth-config"],
            _ => &[],
        };
        mods.push(cand(&format!("m{i}.jar"), needs));
    }
    mods
}

#[test]
fn libraries_stay_enabled_while_other_mods_are_searched() {
    let (session, asked) = run(
        with_libraries(),
        &[],
        crashes_with(&["m4.jar", "cloth-config.jar"]),
    );
    let (culprits, needs) = found(&session);
    assert_eq!(culprits, vec!["m4.jar"]);
    assert_eq!(needs, vec!["fabric-api.jar", "cloth-config.jar"]);
    // Every probe after the first two keeps both libraries on.
    for probe in asked.iter().skip(2) {
        assert!(probe.contains("fabric-api.jar") && probe.contains("cloth-config.jar"));
    }
}

#[test]
fn a_library_can_be_the_culprit() {
    let (session, asked) = run(with_libraries(), &[], crashes_with(&["cloth-config.jar"]));
    let (culprits, needs) = found(&session);
    assert_eq!(culprits, vec!["cloth-config.jar"]);
    assert!(needs.is_empty());
    // With the libraries alone the problem stays: the other mods are cleared in one step.
    assert!(asked.len() <= 5, "{} steps", asked.len());
}

#[test]
fn a_library_that_needs_a_library_is_found_with_it() {
    let mods = vec![
        cand("core-lib.jar", &[]),
        cand("api.jar", &["core-lib"]),
        cand("a.jar", &["api"]),
        cand("b.jar", &[]),
    ];
    let (session, _) = run(mods, &[], crashes_with(&["core-lib.jar"]));
    assert_eq!(found(&session).0, vec!["core-lib.jar"]);
}

#[test]
fn a_dependent_culprit_keeps_what_it_needs() {
    let mods = vec![
        cand("lib.jar", &[]),
        cand("a.jar", &["lib"]),
        cand("b.jar", &["lib"]),
        cand("c.jar", &[]),
        cand("d.jar", &[]),
    ];
    let (session, _) = run(mods, &[], crashes_with(&["b.jar"]));
    let (culprits, needs) = found(&session);
    assert_eq!(culprits, vec!["b.jar"]);
    assert_eq!(needs, vec!["lib.jar"]);
}

#[test]
fn a_problem_that_does_not_reproduce_is_reported() {
    let (session, asked) = run(plain(8), &[], |_| BisectOutcome::Good);
    assert_eq!(session.verdict(), Some(&BisectVerdict::NotReproduced));
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].len(), 8);
}

#[test]
fn a_skipped_baseline_is_inconclusive() {
    let (session, _) = run(plain(4), &[], |_| BisectOutcome::Skipped);
    assert!(matches!(
        session.verdict(),
        Some(BisectVerdict::Inconclusive { suspects }) if suspects.len() == 4
    ));
}

#[test]
fn skipped_steps_do_not_hide_a_found_culprit() {
    // The first half of every split is "skipped"; the search still ends with a culprit or an
    // honest list of suspects that contains it.
    let calls = std::cell::Cell::new(0);
    let (session, _) = run(plain(8), &[], |enabled| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            BisectOutcome::Skipped
        } else if enabled.contains("m5.jar") {
            BisectOutcome::Bad
        } else {
            BisectOutcome::Good
        }
    });
    match session.verdict() {
        Some(BisectVerdict::Found { culprits, .. }) => assert_eq!(culprits, &["m5.jar"]),
        Some(BisectVerdict::Inconclusive { suspects }) => {
            assert!(suspects.contains(&"m5.jar".to_owned()));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn hints_shorten_the_search() {
    let (session, asked) = run(plain(64), &["m41"], crashes_with(&["m41.jar"]));
    assert_eq!(found(&session).0, vec!["m41.jar"]);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0], BTreeSet::from(["m41.jar".to_owned()]));
}

#[test]
fn wrong_hints_fall_back_to_the_full_search() {
    let (session, asked) = run(plain(16), &["m2"], crashes_with(&["m9.jar"]));
    assert_eq!(found(&session).0, vec!["m9.jar"]);
    assert!(asked.len() <= 1 + 1 + 2 * 4);
}

#[test]
fn a_hinted_library_is_checked_alone_first() {
    let (session, asked) = run(
        with_libraries(),
        &["fabric-api"],
        crashes_with(&["fabric-api.jar"]),
    );
    assert_eq!(found(&session).0, vec!["fabric-api.jar"]);
    assert_eq!(asked[0], BTreeSet::from(["fabric-api.jar".to_owned()]));
}

#[test]
fn a_hint_that_is_part_of_a_pair_still_ends_with_the_pair() {
    let (session, _) = run(plain(16), &["m3"], crashes_with(&["m3.jar", "m12.jar"]));
    let mut got = found(&session).0;
    got.sort();
    assert_eq!(got, vec!["m12.jar", "m3.jar"]);
}

#[test]
fn needs_two_mods_and_rejects_duplicates() {
    assert!(Session::new(plain(1), &[]).is_err());
    let mut dup = plain(2);
    dup[1].file = dup[0].file.clone();
    assert!(Session::new(dup, &[]).is_err());
}

#[test]
fn answering_a_finished_search_is_refused() {
    let (mut session, _) = run(plain(4), &[], |_| BisectOutcome::Good);
    assert!(session.answer(BisectOutcome::Bad).is_err());
}

#[test]
fn a_session_survives_serialization_in_the_middle() {
    let candidates = with_libraries();
    let oracle = crashes_with(&["m6.jar"]);
    let mut session = Session::new(candidates, &[]).unwrap();
    while let Some(probe) = session.probe() {
        let enabled: BTreeSet<String> = probe.into_iter().collect();
        let json = serde_json::to_string(&session).unwrap();
        session = serde_json::from_str(&json).unwrap();
        session.validate().unwrap();
        session.answer(oracle(&enabled)).unwrap();
    }
    assert_eq!(found(&session).0, vec!["m6.jar"]);
}

#[test]
fn a_damaged_session_is_rejected() {
    let session = Session::new(plain(4), &[]).unwrap();
    let mut json: serde_json::Value = serde_json::to_value(&session).unwrap();
    json["current"] = serde_json::json!([99]);
    let damaged: Session = serde_json::from_value(json).unwrap();
    assert!(damaged.validate().is_err());
}

#[test]
fn without_drops_the_files_and_what_needs_them() {
    let candidates = vec![
        cand("lib.jar", &[]),
        cand("a.jar", &["lib"]),
        cand("b.jar", &["a"]),
        cand("c.jar", &[]),
    ];
    let session = Session::new(candidates, &[]).unwrap();
    assert_eq!(session.without(&["a.jar".to_owned()]), ["lib.jar", "c.jar"]);
    assert_eq!(session.without(&["lib.jar".to_owned()]), ["c.jar"]);
    assert_eq!(session.without(&[]).len(), 4);
}
