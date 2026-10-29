//! Engine behavior, driven by channels instead of sleeps.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vl_core::task::{Progress, ProgressUnit, TaskEvent, TaskKind, TaskOutcome};
use vl_core::{Error, ErrorKind, Event, EventBus, EventStream, TaskId};
use vl_tasks::{TaskEngine, TaskSpec};

const WAIT: Duration = Duration::from_secs(20);

fn engine(limit: usize) -> (TaskEngine, EventStream) {
    let bus = Arc::new(EventBus::default());
    let events = bus.subscribe();
    (TaskEngine::new(bus, limit), events)
}

/// Reads events until `count` tasks finished; returns all task events in order.
fn collect(events: &EventStream, count: usize) -> Vec<TaskEvent> {
    let mut all = Vec::new();
    let mut finished = 0;
    while finished < count {
        let event = events
            .next_timeout(WAIT)
            .expect("timed out waiting for task events");
        if let Event::Task(event) = event {
            if matches!(event, TaskEvent::Finished { .. }) {
                finished += 1;
            }
            all.push(event);
        }
    }
    all
}

fn outcomes(events: &[TaskEvent]) -> HashMap<TaskId, Vec<TaskOutcome>> {
    let mut map: HashMap<TaskId, Vec<TaskOutcome>> = HashMap::new();
    for event in events {
        if let TaskEvent::Finished { id, outcome } = event {
            map.entry(*id).or_default().push(outcome.clone());
        }
    }
    map
}

fn progress_of(events: &[TaskEvent], task: TaskId) -> Vec<Progress> {
    events
        .iter()
        .filter_map(|e| match e {
            TaskEvent::Progress { id, progress } if *id == task => Some(progress.clone()),
            _ => None,
        })
        .collect()
}

/// Every task starts once, finishes once, and nothing is published for it after `Finished`.
fn assert_lifecycle(events: &[TaskEvent]) {
    let mut started = Vec::new();
    let mut finished = Vec::new();
    for event in events {
        match event {
            TaskEvent::Started(info) => {
                assert!(!started.contains(&info.id), "{} started twice", info.id);
                started.push(info.id);
            }
            TaskEvent::Progress { id, .. } => {
                assert!(started.contains(id), "{id} progressed before it started");
                assert!(!finished.contains(id), "{id} progressed after it finished");
            }
            TaskEvent::Finished { id, .. } => {
                assert!(started.contains(id), "{id} finished before it started");
                assert!(!finished.contains(id), "{id} finished twice");
                finished.push(*id);
            }
            _ => {}
        }
    }
}

#[test]
fn weighted_stages_add_up_to_the_overall_progress() {
    let (engine, events) = engine(2);
    let handle = engine.spawn(TaskSpec::new(TaskKind::InstallVersion), |task| {
        task.expect_weight(4.0);
        task.run_stage("libraries", 3.0, |stage| {
            stage.report_items(5, 10);
            stage.report_items(10, 10);
            Ok(())
        })?;
        task.run_stage("assets", 1.0, |stage| {
            stage.report_items(1, 2);
            Ok(())
        })
    });
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    let all = collect(&events, 3);
    assert_lifecycle(&all);
    let root = progress_of(&all, handle.id());
    let done: Vec<u64> = root.iter().map(|p| p.done).collect();
    // libraries half: 3*0.5/4; libraries done: 3/4; assets half: (3+0.5)/4; assets done.
    assert_eq!(done, [0, 375, 750, 750, 875, 1000]);
    assert_eq!(root[1].stage.as_deref(), Some("libraries"));
    assert_eq!(root[4].stage.as_deref(), Some("assets"));
    assert!(root.iter().all(|p| p.total == Some(1000)));
}

#[test]
fn the_own_progress_of_a_parent_counts_by_its_weight() {
    let (engine, events) = engine(1);
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download), |task| {
        task.set_own_weight(1.0);
        task.report_bytes(50, Some(100));
        task.run_subtask(TaskSpec::new(TaskKind::Download), 1.0, |_| Ok(()))
    });
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    let all = collect(&events, 2);
    let done: Vec<u64> = progress_of(&all, handle.id())
        .iter()
        .map(|p| p.done)
        .collect();
    assert!(done.contains(&250), "{done:?}"); // own half at weight 1 of 2
    assert!(done.contains(&750), "{done:?}"); // plus the finished subtask
}

#[test]
fn a_leaf_reports_its_own_units() {
    let (engine, events) = engine(1);
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download).subject("a.jar"), |task| {
        task.report_bytes(10, Some(40));
        task.report_bytes(40, Some(40));
        Ok(())
    });
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    let all = collect(&events, 1);
    let progress = progress_of(&all, handle.id());
    assert_eq!(progress.len(), 2);
    assert_eq!(progress[0].done, 10);
    assert_eq!(progress[1].fraction(), Some(1.0));
    assert!(matches!(&all[0], TaskEvent::Started(i)
        if i.subject.as_deref() == Some("a.jar") && i.parent.is_none()));
}

#[test]
fn cancelling_a_parent_cancels_its_subtasks() {
    let (engine, events) = engine(1);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let handle = engine.spawn(TaskSpec::new(TaskKind::InstallVersion), move |task| {
        let mut children = Vec::new();
        for _ in 0..2 {
            let ready = ready_tx.clone();
            let release = Arc::clone(&release_rx);
            children.push(task.spawn_subtask(
                TaskSpec::new(TaskKind::Download),
                1.0,
                move |child| {
                    ready.send(()).ok();
                    release.lock().expect("lock").recv().ok();
                    child.check() // the cancel arrives before the release
                },
            ));
        }
        for child in &children {
            child.wait();
        }
        task.check()
    });
    ready_rx.recv_timeout(WAIT).expect("first subtask");
    ready_rx.recv_timeout(WAIT).expect("second subtask");
    engine.cancel(handle.id()).expect("cancel");
    release_tx.send(()).expect("release");
    release_tx.send(()).expect("release");
    assert_eq!(handle.wait(), TaskOutcome::Cancelled);
    let all = collect(&events, 3);
    assert_lifecycle(&all);
    let ends = outcomes(&all);
    assert_eq!(ends.len(), 3);
    assert!(
        ends.values().all(|o| o == &[TaskOutcome::Cancelled]),
        "{ends:?}"
    );
    // Children finish before their parent.
    assert!(matches!(
        all.last(),
        Some(TaskEvent::Finished { id, .. }) if *id == handle.id()
    ));
}

#[test]
fn a_failing_subtask_fails_the_parent_and_stops_the_rest() {
    let (engine, events) = engine(1);
    let ran_second = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&ran_second);
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download), move |task| {
        task.run_stage("first", 1.0, |_| {
            Err::<(), _>(Error::new(ErrorKind::Network, "connection reset"))
        })?;
        task.run_stage("second", 1.0, |_| {
            flag.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let TaskOutcome::Failed(error) = handle.wait() else {
        panic!("expected a failure");
    };
    assert_eq!(error.kind, ErrorKind::Network);
    assert_eq!(error.message, "connection reset");
    let all = collect(&events, 2);
    assert_lifecycle(&all);
    assert_eq!(ran_second.load(Ordering::SeqCst), 0);
    let started = all
        .iter()
        .filter(|e| matches!(e, TaskEvent::Started(_)))
        .count();
    assert_eq!(started, 2); // the parent and the first stage only
    let ends = outcomes(&all);
    assert!(
        ends.values()
            .all(|o| matches!(o.as_slice(), [TaskOutcome::Failed(_)]))
    );
}

#[test]
fn a_failing_parent_cancels_running_subtasks_and_waits_for_them() {
    let (engine, events) = engine(1);
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download), move |task| {
        task.spawn_subtask(TaskSpec::new(TaskKind::Download), 1.0, move |child| {
            ready_tx.send(()).ok();
            release_rx.recv().ok();
            child.check()
        });
        Err(Error::new(ErrorKind::Io, "disk full"))
    });
    ready_rx.recv_timeout(WAIT).expect("subtask");
    release_tx.send(()).expect("release");
    assert!(matches!(handle.wait(), TaskOutcome::Failed(e) if e.kind == ErrorKind::Io));
    let all = collect(&events, 2);
    assert_lifecycle(&all);
    assert!(matches!(
        all.last(),
        Some(TaskEvent::Finished { id, .. }) if *id == handle.id()
    ));
}

#[test]
fn at_most_the_limit_of_tasks_run_at_once() {
    let (engine, _events) = engine(2);
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel::<usize>();
    let mut releases = Vec::new();
    let mut handles = Vec::new();
    for n in 0..5 {
        let running = Arc::clone(&running);
        let peak = Arc::clone(&peak);
        let started = started_tx.clone();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        releases.push(release_tx);
        handles.push(engine.spawn(TaskSpec::new(TaskKind::Download), move |_| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            started.send(n).ok();
            release_rx.recv().ok();
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }));
    }
    // The first two run; the rest wait until one of them is released.
    let mut seen = vec![
        started_rx.recv_timeout(WAIT).expect("first"),
        started_rx.recv_timeout(WAIT).expect("second"),
    ];
    assert_eq!(running.load(Ordering::SeqCst), 2);
    assert!(started_rx.try_recv().is_err());
    assert_eq!(engine.snapshots().len(), 5);
    for release in &releases {
        release.send(()).ok();
    }
    for handle in &handles {
        assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    }
    seen.extend(started_rx.try_iter());
    assert_eq!(seen.len(), 5);
    assert!(peak.load(Ordering::SeqCst) <= 2);
    assert!(engine.snapshots().is_empty());
}

#[test]
fn a_queued_task_cancels_without_running() {
    let (engine, events) = engine(1);
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let blocker = engine.spawn(TaskSpec::new(TaskKind::Download), move |_| {
        started_tx.send(()).ok();
        release_rx.recv().ok();
        Ok(())
    });
    started_rx.recv_timeout(WAIT).expect("blocker");
    let ran = Arc::new(AtomicUsize::new(0));
    let flag = Arc::clone(&ran);
    let queued = engine.spawn(TaskSpec::new(TaskKind::Download), move |_| {
        flag.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    engine.cancel(queued.id()).expect("cancel queued");
    assert_eq!(queued.wait(), TaskOutcome::Cancelled);
    release_tx.send(()).expect("release");
    assert_eq!(blocker.wait(), TaskOutcome::Succeeded);
    // Run something after it to be sure the worker skipped the cancelled job.
    let after = engine.spawn(TaskSpec::new(TaskKind::Download), |_| Ok(()));
    assert_eq!(after.wait(), TaskOutcome::Succeeded);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_lifecycle(&collect(&events, 3));
}

#[test]
fn a_timeout_fails_the_task_and_a_panic_is_contained() {
    let (engine, events) = engine(2);
    let timed = engine.spawn(
        TaskSpec::new(TaskKind::Download).timeout(Duration::ZERO),
        |task| {
            task.run_stage("never", 1.0, |_| Ok(()))?;
            Ok(())
        },
    );
    let TaskOutcome::Failed(error) = timed.wait() else {
        panic!("expected a timeout failure");
    };
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("timed out"));
    let panicked = engine.spawn(TaskSpec::new(TaskKind::Download), |_| {
        panic!("boom");
    });
    let TaskOutcome::Failed(error) = panicked.wait() else {
        panic!("expected a failure");
    };
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("boom"));
    // The pool still works after a panic.
    let fine = engine.spawn(TaskSpec::new(TaskKind::Download), |_| Ok(()));
    assert_eq!(fine.wait(), TaskOutcome::Succeeded);
    assert_lifecycle(&collect(&events, 3));
}

#[test]
fn a_panicking_subtask_finishes_once_and_fails_the_parent() {
    let (engine, events) = engine(1);
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download), |task| {
        task.run_stage("bad", 1.0, |_| -> Result<(), Error> { panic!("inner") })
    });
    assert!(matches!(handle.wait(), TaskOutcome::Failed(e) if e.kind == ErrorKind::Internal));
    let all = collect(&events, 2);
    assert_lifecycle(&all);
    assert_eq!(outcomes(&all).len(), 2);
}

#[test]
fn cancel_racing_completion_still_finishes_exactly_once() {
    let (engine, events) = engine(4);
    let handles: Vec<_> = (0..64)
        .map(|_| {
            engine.spawn(TaskSpec::new(TaskKind::Download), |task| {
                task.run_stage("s", 1.0, |_| Ok(()))
            })
        })
        .collect();
    let cancellers: Vec<_> = handles
        .iter()
        .cloned()
        .map(|h| std::thread::spawn(move || h.cancel()))
        .collect();
    for c in cancellers {
        c.join().expect("canceller");
    }
    for h in &handles {
        assert!(matches!(
            h.wait(),
            TaskOutcome::Succeeded | TaskOutcome::Cancelled
        ));
    }
    let mut all = Vec::new();
    while let Some(event) = events.next_timeout(Duration::from_millis(200)) {
        if let Event::Task(e) = event {
            all.push(e);
        }
    }
    assert_lifecycle(&all);
    let ends = outcomes(&all);
    assert!(ends.values().all(|o| o.len() == 1));
    let roots = handles
        .iter()
        .filter(|h| ends.contains_key(&h.id()))
        .count();
    assert_eq!(roots, 64);
}

#[test]
fn cancel_reports_unknown_and_protected_tasks() {
    let (engine, _events) = engine(1);
    assert_eq!(
        engine.cancel(TaskId(9999)).unwrap_err().kind,
        ErrorKind::NotFound
    );
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let handle = engine.spawn(
        TaskSpec::new(TaskKind::Launch).not_cancellable(),
        move |_| {
            started_tx.send(()).ok();
            release_rx.recv().ok();
            Ok(())
        },
    );
    started_rx.recv_timeout(WAIT).expect("started");
    assert_eq!(
        engine.cancel(handle.id()).unwrap_err().kind,
        ErrorKind::Conflict
    );
    let snapshots = engine.snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(!snapshots[0].info.cancellable);
    release_tx.send(()).expect("release");
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
}

#[test]
fn an_unsubscribed_stream_does_not_disturb_the_engine() {
    let (engine, events) = engine(1);
    drop(events);
    let handle = engine.spawn(TaskSpec::new(TaskKind::Download), |_| Ok(()));
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
}

#[test]
fn a_new_stage_does_not_show_the_counters_of_the_previous_one() {
    let (engine, events) = engine(1);
    let handle = engine.spawn(TaskSpec::new(TaskKind::InstallVersion), |task| {
        task.report_stage("downloading", 90, Some(100), ProgressUnit::Bytes);
        task.set_stage("natives");
        task.report_stage("natives", 1, Some(2), ProgressUnit::Items);
        Ok(())
    });
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    let all = collect(&events, 1);
    let seen: Vec<_> = progress_of(&all, handle.id())
        .into_iter()
        .map(|p| (p.stage, p.done, p.total, p.unit))
        .collect();
    assert_eq!(
        seen,
        [
            (
                Some("downloading".to_owned()),
                90,
                Some(100),
                ProgressUnit::Bytes
            ),
            (Some("natives".to_owned()), 1, Some(2), ProgressUnit::Items),
        ]
    );
}
