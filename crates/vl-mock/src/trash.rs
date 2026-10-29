//! Synthetic trash of deleted instances: deletion keeps the instance in memory with its comment
//! and a synthetic size; restore, purge and the limits of the `storage` settings work on that
//! list. Time is a synthetic clock that advances a minute with each deletion.

use vl_core::instance::{InstanceEvent, InstanceState, InstanceSummary};
use vl_core::trash::{COMMENT_MAX_CHARS, PurgeReport, PurgeTarget, TrashItem, TrashList};
use vl_core::{Error, Event, InstanceId, Response, Result};

use crate::{Inner, State, data, find_instance};

/// The synthetic time of the first deletion (Unix seconds).
const START_UNIX: i64 = 1_790_000_000;
const DAY_SECS: i64 = 24 * 60 * 60;

/// One deleted instance and what restoring it brings back.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    item: TrashItem,
    summary: InstanceSummary,
}

/// A synthetic size for an instance: larger the longer it was played.
fn size_of(summary: &InstanceSummary) -> u64 {
    (256 << 20) + summary.play_time_secs.saturating_mul(4096)
}

pub(crate) fn delete(
    inner: &Inner,
    id: InstanceId,
    comment: &str,
    permanent: bool,
) -> Result<Response> {
    let comment = comment.trim();
    if comment.chars().count() > COMMENT_MAX_CHARS {
        return Err(Error::invalid_input(format!(
            "the comment is longer than {COMMENT_MAX_CHARS} characters"
        )));
    }
    {
        let mut state = inner.lock();
        let summary = find_instance(&mut state, &id)?.clone();
        if summary.state != InstanceState::Stopped || state.games.contains_key(&id) {
            return Err(Error::conflict(format!(
                "the game of instance {id} is running; close it first"
            )));
        }
        state.instances.retain(|i| i.id != id);
        state.overrides.remove(&id);
        state.resources.retain(|(owner, _), _| owner != &id);
        state.packs.remove(&id);
        state.bisects.remove(&id);
        state.bisect_reports.remove(&id);
        if !permanent {
            state.trash_clock = state.trash_clock.max(START_UNIX) + 60;
            let deleted_unix = state.trash_clock;
            let item_id = data::slug(id.as_str(), |c| state.trash.iter().any(|e| e.item.id == c))
                .map_or_else(|| id.to_string(), |s| s.to_string());
            state.trash.push(Entry {
                item: TrashItem {
                    id: item_id,
                    instance: id.to_string(),
                    name: summary.name.clone(),
                    group: summary.group.clone(),
                    game_version: summary.game_version.clone(),
                    loader: summary.loader.clone(),
                    deleted_unix,
                    size: size_of(&summary),
                    comment: comment.to_owned(),
                    problem: None,
                },
                summary,
            });
        }
    }
    inner.publish(Event::Instance(InstanceEvent::Removed(id)));
    if !permanent {
        inner.publish(Event::Instance(InstanceEvent::TrashChanged));
        let report = purge_limits(&mut inner.lock());
        if !report.removed.is_empty() {
            inner.publish(Event::Instance(InstanceEvent::TrashChanged));
        }
    }
    Ok(Response::Done)
}

pub(crate) fn list(inner: &Inner) -> Response {
    let state = inner.lock();
    let mut items: Vec<TrashItem> = state.trash.iter().map(|e| e.item.clone()).collect();
    items.sort_by(|a, b| {
        b.deleted_unix
            .cmp(&a.deleted_unix)
            .then_with(|| a.id.cmp(&b.id))
    });
    Response::Trash(TrashList {
        size: items.iter().map(|i| i.size).sum(),
        items,
        keep_days: state.settings.storage.trash_days,
        max_gib: state.settings.storage.trash_max_gib,
    })
}

pub(crate) fn restore(inner: &Inner, item: &str) -> Result<Response> {
    let summary = {
        let mut state = inner.lock();
        let at = state
            .trash
            .iter()
            .position(|e| e.item.id == item)
            .ok_or_else(|| Error::not_found(format!("trash item {item}")))?;
        let entry = state.trash.remove(at);
        let mut summary = entry.summary;
        let taken = |state: &State, id: &str| state.instances.iter().any(|i| i.id.as_str() == id);
        if taken(&state, summary.id.as_str()) {
            let mut n = 2u32;
            while taken(&state, &format!("{}-{n}", summary.id)) {
                n += 1;
            }
            summary.id = InstanceId::new(&format!("{}-{n}", summary.id))
                .ok_or_else(|| Error::internal("empty instance id"))?;
        }
        let name_taken = |state: &State, name: &str| {
            state
                .instances
                .iter()
                .any(|i| i.name.eq_ignore_ascii_case(name))
        };
        if name_taken(&state, &summary.name) {
            let base = summary.name.clone();
            let mut n = 2u32;
            while name_taken(&state, &format!("{base} ({n})")) {
                n += 1;
            }
            summary.name = format!("{base} ({n})");
        }
        state.instances.push(summary.clone());
        summary
    };
    inner.publish(Event::Instance(InstanceEvent::Added(summary.clone())));
    inner.publish(Event::Instance(InstanceEvent::TrashChanged));
    Ok(Response::Instance(summary))
}

pub(crate) fn purge(inner: &Inner, target: PurgeTarget) -> Result<Response> {
    let report = {
        let mut state = inner.lock();
        match target {
            PurgeTarget::Item(item) => {
                let at = state
                    .trash
                    .iter()
                    .position(|e| e.item.id == item)
                    .ok_or_else(|| Error::not_found(format!("trash item {item}")))?;
                let entry = state.trash.remove(at);
                PurgeReport {
                    removed: vec![entry.item.id],
                    freed: entry.item.size,
                    failed: Vec::new(),
                }
            }
            PurgeTarget::All => {
                let entries = std::mem::take(&mut state.trash);
                PurgeReport {
                    freed: entries.iter().map(|e| e.item.size).sum(),
                    removed: entries.into_iter().map(|e| e.item.id).collect(),
                    failed: Vec::new(),
                }
            }
            PurgeTarget::Limits => purge_limits(&mut state),
            other => return Err(crate::unsupported(&other)),
        }
    };
    if !report.removed.is_empty() {
        inner.publish(Event::Instance(InstanceEvent::TrashChanged));
    }
    Ok(Response::TrashPurged(report))
}

/// Erases items older than the age limit, then the oldest while over the size limit; the most
/// recently deleted item stays.
fn purge_limits(state: &mut State) -> PurgeReport {
    let limits = state.settings.storage.clone();
    let horizon = state
        .trash_clock
        .saturating_sub(i64::from(limits.trash_days).saturating_mul(DAY_SECS));
    state.trash.sort_by_key(|e| e.item.deleted_unix);
    let mut report = PurgeReport::default();
    let (expired, mut kept): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut state.trash)
        .into_iter()
        .partition(|e| e.item.deleted_unix <= horizon);
    for entry in expired {
        report.freed += entry.item.size;
        report.removed.push(entry.item.id);
    }
    let mut total: u64 = kept.iter().map(|e| e.item.size).sum();
    while total > limits.trash_max_bytes() && kept.len() > 1 {
        let entry = kept.remove(0);
        total -= entry.item.size;
        report.freed += entry.item.size;
        report.removed.push(entry.item.id);
    }
    state.trash = kept;
    report
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vl_core::instance::InstanceCommand;
    use vl_core::launch::{LaunchCommand, LaunchRequest};
    use vl_core::{Command, ErrorKind, Facade};

    use super::*;
    use crate::{MockFacade, MockOptions};

    fn call(facade: &MockFacade, command: InstanceCommand) -> Result<Response> {
        facade.call(Command::Instances(command)).wait()
    }

    fn delete(facade: &MockFacade, id: &str, permanent: bool) -> Result<Response> {
        call(
            facade,
            InstanceCommand::Delete {
                id: InstanceId::new(id).unwrap(),
                comment: "synthetic".into(),
                permanent,
            },
        )
    }

    #[test]
    fn a_running_game_blocks_deletion() {
        let facade = MockFacade::new(MockOptions {
            step_delay: Duration::ZERO,
            exit_after_log: false,
            ..MockOptions::instant()
        });
        let instance = InstanceId::new("survival").unwrap();
        facade
            .call(Command::Launch(LaunchCommand::Launch(LaunchRequest {
                instance: instance.clone(),
                account: None,
                offline: true,
            })))
            .wait()
            .unwrap();
        for permanent in [false, true] {
            let err = delete(&facade, "survival", permanent).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Conflict);
        }
    }

    #[test]
    fn restore_suffixes_a_taken_id_and_name_and_limits_purge_the_oldest() {
        let facade = MockFacade::new(MockOptions::instant());
        delete(&facade, "survival", false).unwrap();
        let Ok(Response::Trash(list)) = call(&facade, InstanceCommand::ListTrash) else {
            panic!("no trash");
        };
        assert_eq!(list.items.len(), 1);
        let name = list.items[0].name.clone();
        facade
            .call(Command::Instances(InstanceCommand::Create(
                vl_core::instance::NewInstance {
                    name: name.clone(),
                    game_version: "26.3".into(),
                    group: None,
                    loader: None,
                },
            )))
            .wait()
            .unwrap();
        let Ok(Response::Instance(back)) = call(
            &facade,
            InstanceCommand::Restore {
                item: "survival".into(),
            },
        ) else {
            panic!("not restored");
        };
        assert_eq!(back.id.as_str(), "survival-2");
        assert_eq!(back.name, format!("{name} (2)"));

        facade.inner.lock().settings.storage.trash_max_gib = 1;
        delete(&facade, "survival", false).unwrap();
        delete(&facade, "survival-2", false).unwrap();
        delete(&facade, "survival", false).unwrap_err();
        let Ok(Response::Trash(list)) = call(&facade, InstanceCommand::ListTrash) else {
            panic!("no trash");
        };
        // The newest stays, even alone over the limit.
        let ids: Vec<&str> = list.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids[0], "survival-2");
        assert!(list.size <= 1 << 30 || ids.len() == 1, "{ids:?}");
    }
}
