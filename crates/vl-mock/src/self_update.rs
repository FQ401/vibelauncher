//! Synthetic self-update answers: version 0.1.0 is always offered, a download is a short task,
//! and applying it answers `Done` without exiting.

use std::sync::{Arc, Mutex, PoisonError};

use vl_core::self_update::{SelfUpdateCommand, SelfUpdateStatus, UpdateOffer};
use vl_core::task::TaskKind;
use vl_core::{Error, Response, Result};

use crate::{Inner, sim, unsupported};

/// The version the mock offers.
pub(crate) const OFFERED: &str = "0.1.0";
/// The size of the synthetic download.
const SIZE: u64 = 25 * 1024 * 1024;

/// What the mock remembers between self-update commands.
#[derive(Debug, Default)]
pub(crate) struct State {
    checked: bool,
    staged: bool,
}

fn state(inner: &Inner) -> std::sync::MutexGuard<'_, State> {
    inner
        .self_update
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn status(inner: &Inner) -> SelfUpdateStatus {
    let state = state(inner);
    SelfUpdateStatus {
        current: env!("CARGO_PKG_VERSION").into(),
        install: "portable".into(),
        enabled: true,
        newest_seen: state.checked.then(|| OFFERED.into()),
        offer: state.checked.then(|| UpdateOffer::Update {
            version: OFFERED.into(),
            date: "2026-10-01".into(),
            size: SIZE,
        }),
        staged: state.staged.then(|| OFFERED.into()),
        last_error: None,
        just_updated: None,
    }
}

pub(crate) fn handle(inner: &Arc<Inner>, command: SelfUpdateCommand) -> Result<Response> {
    match command {
        SelfUpdateCommand::Status => Ok(Response::SelfUpdate(status(inner))),
        SelfUpdateCommand::Check => {
            state(inner).checked = true;
            Ok(Response::SelfUpdate(status(inner)))
        }
        SelfUpdateCommand::Download => {
            state(inner).checked = true;
            sim::steps(
                inner,
                TaskKind::DownloadUpdate,
                Some(OFFERED.into()),
                5,
                |inner| state(inner).staged = true,
            )
        }
        SelfUpdateCommand::Apply { .. } => {
            if state(inner).staged {
                Ok(Response::Done)
            } else {
                Err(Error::not_found("no update is downloaded"))
            }
        }
        other => Err(unsupported(&other)),
    }
}

/// A fresh state for a new mock.
pub(crate) fn new_state() -> Mutex<State> {
    Mutex::default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vl_core::task::{TaskEvent, TaskOutcome};
    use vl_core::{Command, ErrorKind, Event, Facade};

    use super::*;
    use crate::{MockFacade, MockOptions};

    fn status_of(mock: &MockFacade, command: SelfUpdateCommand) -> SelfUpdateStatus {
        match mock.call(Command::SelfUpdate(command)).wait().unwrap() {
            Response::SelfUpdate(s) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn check_download_apply() {
        let mock = MockFacade::new(MockOptions::instant());
        let s = status_of(&mock, SelfUpdateCommand::Status);
        assert_eq!(s.offer, None);
        let err = mock
            .call(Command::SelfUpdate(SelfUpdateCommand::Apply {
                restart: None,
            }))
            .wait()
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        let s = status_of(&mock, SelfUpdateCommand::Check);
        assert!(
            matches!(s.offer, Some(UpdateOffer::Update { ref version, .. }) if version == OFFERED)
        );

        let events = mock.subscribe();
        let task = match mock
            .call(Command::SelfUpdate(SelfUpdateCommand::Download))
            .wait()
            .unwrap()
        {
            Response::Task(id) => id,
            other => panic!("{other:?}"),
        };
        loop {
            match events.next_timeout(Duration::from_secs(10)) {
                Some(Event::Task(TaskEvent::Finished { id, outcome })) if id == task => {
                    assert_eq!(outcome, TaskOutcome::Succeeded);
                    break;
                }
                Some(_) => {}
                None => panic!("the download did not finish"),
            }
        }
        assert_eq!(
            status_of(&mock, SelfUpdateCommand::Status)
                .staged
                .as_deref(),
            Some(OFFERED)
        );
        assert_eq!(
            mock.call(Command::SelfUpdate(SelfUpdateCommand::Apply {
                restart: Some(Vec::new())
            }))
            .wait()
            .unwrap(),
            Response::Done
        );
    }
}
