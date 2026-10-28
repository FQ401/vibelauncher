//! The mock's end-to-end scenarios through the facade only.
#![allow(clippy::panic)] // test helpers fail the test by panicking

use std::time::Duration;

use vl_core::instance::{InstanceCommand, InstanceState, NewInstance};
use vl_core::launch::{LaunchCommand, LaunchEvent, LaunchRequest};
use vl_core::log::LogSource;
use vl_core::task::{TaskCommand, TaskEvent, TaskKind, TaskOutcome};
use vl_core::{Command, ErrorKind, Event, EventStream, Facade, InstanceId, Response};
use vl_mock::{MockFacade, MockOptions};

const WAIT: Duration = Duration::from_secs(10);

/// Collects events until `last` matches one (or the wait times out).
fn collect_until(events: &EventStream, last: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    while let Some(event) = events.next_timeout(WAIT) {
        let done = last(&event);
        seen.push(event);
        if done {
            return seen;
        }
    }
    panic!("timed out; events so far: {seen:#?}");
}

fn exited(instance: &InstanceId) -> impl Fn(&Event) -> bool + '_ {
    move |e| matches!(e, Event::Launch(LaunchEvent::Exited { instance: i, .. }) if i == instance)
}

#[test]
fn list_instances_then_launch_then_read_the_game_log() {
    let facade = MockFacade::new(MockOptions::instant());
    let events = facade.subscribe();

    let Response::Instances(instances) = facade
        .call(Command::Instances(InstanceCommand::List))
        .wait()
        .unwrap()
    else {
        panic!("expected instances");
    };
    assert!(instances.len() >= 3);
    let instance = instances
        .iter()
        .find(|i| i.problem.is_none())
        .unwrap()
        .id
        .clone();

    let Response::Task(task) = facade
        .call(Command::Launch(LaunchCommand::Launch(LaunchRequest {
            instance: instance.clone(),
            account: None,
            offline: false,
        })))
        .wait()
        .unwrap()
    else {
        panic!("expected a task");
    };

    let seen = collect_until(&events, exited(&instance));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Started(info)) if info.id == task && info.kind == TaskKind::Launch
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Progress { id, progress }) if *id == task && progress.fraction() == Some(1.0)
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Finished { id, outcome: TaskOutcome::Succeeded }) if *id == task
    )));
    let states: Vec<&InstanceState> = seen
        .iter()
        .filter_map(|e| match e {
            Event::Launch(LaunchEvent::StateChanged { state, .. }) => Some(state),
            _ => None,
        })
        .collect();
    assert!(matches!(
        states.as_slice(),
        [
            InstanceState::Preparing { .. },
            InstanceState::Running { .. },
            InstanceState::Stopped
        ]
    ));
    let game_lines = seen
        .iter()
        .filter(
            |e| matches!(e, Event::Log(line) if line.source == LogSource::Game(instance.clone())),
        )
        .count();
    assert_eq!(game_lines, MockOptions::instant().game_log_lines + 1);
}

#[test]
fn a_running_game_stops_on_request_and_cannot_start_twice() {
    let options = MockOptions {
        exit_after_log: false,
        ..MockOptions::instant()
    };
    let facade = MockFacade::new(options);
    let events = facade.subscribe();
    let instance = InstanceId::new("survival").unwrap();
    let launch = || {
        facade
            .call(Command::Launch(LaunchCommand::Launch(LaunchRequest {
                instance: instance.clone(),
                account: None,
                offline: true,
            })))
            .wait()
    };

    launch().unwrap();
    collect_until(&events, |e| {
        matches!(
            e,
            Event::Launch(LaunchEvent::StateChanged {
                state: InstanceState::Running { .. },
                ..
            })
        )
    });
    assert_eq!(launch().unwrap_err().kind, ErrorKind::Conflict);

    facade
        .call(Command::Launch(LaunchCommand::Stop {
            instance: instance.clone(),
        }))
        .wait()
        .unwrap();
    let seen = collect_until(&events, exited(&instance));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Launch(LaunchEvent::Exited { exit, .. }) if exit.stopped_by_user
    )));
}

#[test]
fn instances_are_created_renamed_and_deleted_with_events() {
    let facade = MockFacade::new(MockOptions::instant());
    let events = facade.subscribe();
    let Response::Instance(created) = facade
        .call(Command::Instances(InstanceCommand::Create(NewInstance {
            name: "Survival".into(),
            game_version: "26.3".into(),
            group: None,
            loader: None,
        })))
        .wait()
        .unwrap()
    else {
        panic!("expected an instance");
    };
    assert_eq!(created.id.as_str(), "survival-2");

    facade
        .call(Command::Instances(InstanceCommand::Rename {
            id: created.id.clone(),
            name: "Hardcore".into(),
        }))
        .wait()
        .unwrap();
    facade
        .call(Command::Instances(InstanceCommand::Delete {
            id: created.id.clone(),
        }))
        .wait()
        .unwrap();
    assert_eq!(events.drain().count(), 3);

    let missing = facade
        .call(Command::Instances(InstanceCommand::Create(NewInstance {
            name: "Future".into(),
            game_version: "99.0".into(),
            group: None,
            loader: None,
        })))
        .wait()
        .unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);
}

#[test]
fn a_task_can_be_cancelled() {
    let options = MockOptions {
        step_delay: Duration::from_millis(20),
        ..MockOptions::instant()
    };
    let facade = MockFacade::new(options);
    let events = facade.subscribe();
    let Response::Task(task) = facade
        .call(Command::Java(vl_core::java::JavaCommand::Rescan))
        .wait()
        .unwrap()
    else {
        panic!("expected a task");
    };
    facade
        .call(Command::Tasks(TaskCommand::Cancel { id: task }))
        .wait()
        .unwrap();
    let seen = collect_until(&events, |e| {
        matches!(e, Event::Task(TaskEvent::Finished { .. }))
    });
    assert!(matches!(
        seen.last(),
        Some(Event::Task(TaskEvent::Finished {
            outcome: TaskOutcome::Cancelled,
            ..
        }))
    ));
}
