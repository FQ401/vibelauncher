//! The mock's resource folders, worlds and mod bisection through the facade only.
#![allow(clippy::panic, clippy::unwrap_used)] // test helpers fail the test by panicking

mod common;

use common::{call, files, id, mock, run_task};
use vl_core::bisect::{
    AutoBisectRequest, AutoVerdict, BisectCommand, BisectEvent, BisectOutcome, BisectState,
    BisectStatus, BisectVerdict, RunPurpose,
};
use vl_core::resources::{ResourceCommand, ResourceKind};
use vl_core::task::{TaskKind, TaskOutcome};
use vl_core::{Command, Error, ErrorKind, Event, Facade, Response};
use vl_mock::MockFacade;

fn resources(facade: &MockFacade, command: ResourceCommand) -> Result<Response, Error> {
    call(facade, Command::Resources(command))
}

fn bisect(facade: &MockFacade, command: BisectCommand) -> Result<Response, Error> {
    call(facade, Command::Bisect(command))
}

fn status(response: Response) -> BisectStatus {
    match response {
        Response::Bisect(status) => status,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn resource_files_are_listed_toggled_added_and_removed() {
    let facade = mock();
    let mods = files(&facade, "creative", ResourceKind::Mods);
    assert_eq!(mods.len(), 8);
    assert!(mods.windows(2).all(|w| w[0].0 <= w[1].0), "sorted");
    assert!(files(&facade, "old-beta", ResourceKind::Mods).is_empty());
    let Response::Resources(entries) = resources(
        &facade,
        ResourceCommand::List {
            instance: id("creative"),
            kind: ResourceKind::Mods,
        },
    )
    .unwrap() else {
        panic!("expected entries");
    };
    let renderer = entries
        .iter()
        .find(|e| e.file == "mock-renderer-1.1.0.jar")
        .unwrap();
    assert_eq!(renderer.mods[0].name, "Mock Renderer");
    assert!(renderer.mods[0].dependencies.len() == 2);
    let packs = files(&facade, "creative", ResourceKind::ResourcePacks);
    assert_eq!(packs.len(), 2);

    let creative = id("creative");
    let kind = ResourceKind::Mods;
    let toggle = |file: &str, enabled| {
        resources(
            &facade,
            ResourceCommand::SetEnabled {
                instance: creative.clone(),
                kind,
                file: file.into(),
                enabled,
            },
        )
    };
    toggle("local-tweak.jar", false).unwrap();
    assert!(files(&facade, "creative", kind).contains(&("local-tweak.jar".into(), false)));
    assert_eq!(
        toggle("nope.jar", true).unwrap_err().kind,
        ErrorKind::NotFound
    );

    let add = |source: &str| {
        resources(
            &facade,
            ResourceCommand::Add {
                instance: creative.clone(),
                kind,
                source: source.into(),
            },
        )
    };
    add("C:\\downloads\\extra.jar").unwrap();
    assert!(files(&facade, "creative", kind).contains(&("extra.jar".into(), true)));
    assert_eq!(add("/tmp/extra.jar").unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(add("/tmp/").unwrap_err().kind, ErrorKind::InvalidInput);

    let remove = |file: &str| {
        resources(
            &facade,
            ResourceCommand::Remove {
                instance: creative.clone(),
                kind,
                file: file.into(),
            },
        )
    };
    remove("extra.jar").unwrap();
    assert_eq!(remove("extra.jar").unwrap_err().kind, ErrorKind::NotFound);

    let list_broken = ResourceCommand::List {
        instance: id("broken"),
        kind,
    };
    assert_eq!(
        resources(&facade, list_broken).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn worlds_have_data_packs() {
    let facade = mock();
    let instance = id("creative");
    let Response::Worlds(worlds) = resources(
        &facade,
        ResourceCommand::Worlds {
            instance: instance.clone(),
        },
    )
    .unwrap() else {
        panic!("expected worlds");
    };
    assert_eq!(worlds.len(), 2);
    assert_eq!(worlds[0].datapacks, 1);
    assert_eq!(worlds[0].version.as_deref(), Some("1.21.11"));

    let world = "creative-world";
    let list = || match resources(
        &facade,
        ResourceCommand::WorldList {
            instance: instance.clone(),
            world: world.into(),
        },
    )
    .unwrap()
    {
        Response::Resources(entries) => entries,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(list().len(), 1);
    resources(
        &facade,
        ResourceCommand::WorldAdd {
            instance: instance.clone(),
            world: world.into(),
            source: "/tmp/extra-pack.zip".into(),
        },
    )
    .unwrap();
    resources(
        &facade,
        ResourceCommand::WorldSetEnabled {
            instance: instance.clone(),
            world: world.into(),
            file: "extra-pack.zip".into(),
            enabled: false,
        },
    )
    .unwrap();
    assert!(
        list()
            .iter()
            .any(|e| e.file == "extra-pack.zip" && !e.enabled)
    );
    resources(
        &facade,
        ResourceCommand::WorldRemove {
            instance: instance.clone(),
            world: world.into(),
            file: "extra-pack.zip".into(),
        },
    )
    .unwrap();
    assert_eq!(list().len(), 1);
    let missing = ResourceCommand::WorldList {
        instance: instance.clone(),
        world: "nowhere".into(),
    };
    assert_eq!(
        resources(&facade, missing).unwrap_err().kind,
        ErrorKind::NotFound
    );
}

#[test]
fn a_manual_bisection_narrows_down_to_the_culprit_and_restores_the_mods() {
    let facade = mock();
    let instance = id("creative");
    let before = files(&facade, "creative", ResourceKind::Mods);
    let culprit = "mock-weather-fx-1.1.0.jar";

    let mut now = status(
        bisect(
            &facade,
            BisectCommand::Start {
                instance: instance.clone(),
                suspects: vec![],
            },
        )
        .unwrap(),
    );
    assert_eq!(now.state, BisectState::Running);
    assert_eq!((now.candidates, now.steps_done), (7, 0));
    assert_eq!(now.steps_estimate, 3);
    // The folder shows the step.
    let in_folder = files(&facade, "creative", ResourceKind::Mods);
    assert_eq!(
        in_folder.iter().filter(|(_, on)| *on).count(),
        now.enabled.len()
    );
    assert_eq!(
        bisect(
            &facade,
            BisectCommand::Start {
                instance: instance.clone(),
                suspects: vec![],
            },
        )
        .unwrap_err()
        .kind,
        ErrorKind::Conflict
    );

    while now.state == BisectState::Running {
        let outcome = if now.enabled.iter().any(|f| f == culprit) {
            BisectOutcome::Bad
        } else {
            BisectOutcome::Good
        };
        now = status(
            bisect(
                &facade,
                BisectCommand::Report {
                    instance: instance.clone(),
                    outcome,
                },
            )
            .unwrap(),
        );
    }
    assert_eq!(now.steps_done, 3);
    assert_eq!(
        now.verdict,
        Some(BisectVerdict::Found {
            culprits: vec![culprit.into()],
            needs: vec!["mock-library-2.0.0.jar".into()],
        })
    );
    assert_eq!(files(&facade, "creative", ResourceKind::Mods), before);
    let again = status(
        bisect(
            &facade,
            BisectCommand::Status {
                instance: instance.clone(),
            },
        )
        .unwrap(),
    );
    assert_eq!(again, now);
    let report = BisectCommand::Report {
        instance: instance.clone(),
        outcome: BisectOutcome::Good,
    };
    assert_eq!(
        bisect(&facade, report).unwrap_err().kind,
        ErrorKind::Conflict
    );
    let stop = BisectCommand::Stop {
        instance: instance.clone(),
    };
    assert_eq!(bisect(&facade, stop.clone()).unwrap(), Response::Done);
    assert_eq!(bisect(&facade, stop).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_manual_bisection_can_be_skipped_or_stopped() {
    let facade = mock();
    let instance = id("creative");
    let before = files(&facade, "creative", ResourceKind::Mods);
    let start = BisectCommand::Start {
        instance: instance.clone(),
        suspects: vec![],
    };
    bisect(&facade, start.clone()).unwrap();
    let skipped = status(
        bisect(
            &facade,
            BisectCommand::Report {
                instance: instance.clone(),
                outcome: BisectOutcome::Skipped,
            },
        )
        .unwrap(),
    );
    assert!(matches!(
        skipped.verdict,
        Some(BisectVerdict::Inconclusive { .. })
    ));
    assert_eq!(files(&facade, "creative", ResourceKind::Mods), before);

    bisect(&facade, start).unwrap();
    bisect(
        &facade,
        BisectCommand::Stop {
            instance: instance.clone(),
        },
    )
    .unwrap();
    assert_eq!(files(&facade, "creative", ResourceKind::Mods), before);
    let none = BisectCommand::Status { instance };
    assert_eq!(bisect(&facade, none).unwrap_err().kind, ErrorKind::NotFound);

    for name in ["old-beta", "broken", "nope"] {
        let start = BisectCommand::Start {
            instance: id(name),
            suspects: vec![],
        };
        assert!(bisect(&facade, start).is_err(), "{name}");
    }
}

#[test]
fn an_automatic_bisection_runs_as_a_task_and_finds_the_culprit() {
    let facade = mock();
    let events = facade.subscribe();
    let instance = id("creative");
    let report_command = BisectCommand::AutoReport {
        instance: instance.clone(),
    };
    assert_eq!(
        bisect(&facade, report_command.clone()).unwrap_err().kind,
        ErrorKind::NotFound
    );
    let response = bisect(
        &facade,
        BisectCommand::Auto(AutoBisectRequest::new(instance)),
    )
    .unwrap();
    let (seen, outcome) = run_task(&events, response);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    assert!(seen.iter().any(|e| matches!(
        e,
        vl_core::Event::Task(vl_core::task::TaskEvent::Started(info)) if info.kind == TaskKind::Bisect
    )));
    let runs: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            Event::Bisect(BisectEvent::Run {
                run, steps_done, ..
            }) => Some((run.purpose, *steps_done)),
            _ => None,
        })
        .collect();
    assert_eq!(runs.first(), Some(&(RunPurpose::Reproduce, 0)));
    assert_eq!(runs.last(), Some(&(RunPurpose::Verify, 3)));
    assert_eq!(runs.len(), 6);
    let finished = seen.iter().find_map(|e| match e {
        Event::Bisect(BisectEvent::Finished { report, .. }) => Some(report.clone()),
        _ => None,
    });
    let Some(finished) = finished else {
        panic!("no finished event");
    };
    assert!(matches!(
        &finished.verdict,
        AutoVerdict::Found { culprits, confirmed: true, .. }
            if culprits == &["mock-weather-fx-1.1.0.jar".to_owned()]
    ));
    assert_eq!(
        bisect(&facade, report_command).unwrap(),
        Response::BisectReport(*finished)
    );

    // Too few mods to search.
    let few = BisectCommand::Auto(AutoBisectRequest::new(id("survival")));
    assert_eq!(
        bisect(&facade, few).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}
