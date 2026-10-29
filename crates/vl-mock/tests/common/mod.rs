//! Helpers shared by the mock's scenario tests.
#![allow(dead_code, clippy::panic, clippy::unwrap_used)] // each test file uses some of them; failures panic

use std::time::Duration;

use vl_core::catalog::{CatalogCommand, Provider};
use vl_core::instance::{InstanceCommand, InstanceSummary};
use vl_core::resources::{ResourceCommand, ResourceKind};
use vl_core::secret::Secret;
use vl_core::task::{TaskEvent, TaskOutcome};
use vl_core::{Command, Error, Event, EventStream, Facade, InstanceId, Response};
use vl_mock::{MockFacade, MockOptions};

const WAIT: Duration = Duration::from_secs(10);

/// A mock whose CurseForge API key is set, through the facade like a user would.
pub fn mock() -> MockFacade {
    let facade = MockFacade::new(MockOptions::instant());
    let command = Command::Catalog(CatalogCommand::SetApiKey {
        provider: Provider::CurseForge,
        key: Secret::new("synthetic-key"),
    });
    assert_eq!(call(&facade, command), Ok(Response::Done));
    facade
}

pub fn call(facade: &MockFacade, command: Command) -> Result<Response, Error> {
    facade.call(command).wait()
}

pub fn id(name: &str) -> InstanceId {
    InstanceId::new(name).unwrap()
}

/// Collects events until the task of `response` finishes; returns them and the outcome.
pub fn run_task(events: &EventStream, response: Response) -> (Vec<Event>, TaskOutcome) {
    let Response::Task(task) = response else {
        panic!("expected a task, got {response:?}");
    };
    let mut seen = Vec::new();
    while let Some(event) = events.next_timeout(WAIT) {
        let outcome = match &event {
            Event::Task(TaskEvent::Finished { id, outcome }) if *id == task => {
                Some(outcome.clone())
            }
            _ => None,
        };
        seen.push(event);
        if let Some(outcome) = outcome {
            return (seen, outcome);
        }
    }
    panic!("timed out; events so far: {seen:#?}");
}

/// The files of a folder: name and whether enabled.
pub fn files(facade: &MockFacade, instance: &str, kind: ResourceKind) -> Vec<(String, bool)> {
    let command = Command::Resources(ResourceCommand::List {
        instance: id(instance),
        kind,
    });
    match call(facade, command).unwrap() {
        Response::Resources(files) => files.into_iter().map(|f| (f.file, f.enabled)).collect(),
        other => panic!("unexpected {other:?}"),
    }
}

pub fn instances(facade: &MockFacade) -> Vec<InstanceSummary> {
    match call(facade, Command::Instances(InstanceCommand::List)).unwrap() {
        Response::Instances(list) => list,
        other => panic!("unexpected {other:?}"),
    }
}
