//! The progress events a preparation publishes, with a scripted job and no network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use vl_core::task::{TaskEvent, TaskOutcome};
use vl_tasks::TaskEngine;

use super::*;

/// Runs `script` as a task through [`with_job`] and returns the progress events it published, as
/// `(stage, done, total, unit)`.
fn published(
    script: impl FnOnce(&Job) + Send + 'static,
) -> Vec<(String, u64, Option<u64>, ProgressUnit)> {
    let bus = Arc::new(vl_core::EventBus::default());
    let events = bus.subscribe();
    let engine = TaskEngine::new(bus, 1);
    let handle = engine.spawn(TaskSpec::new(TaskKind::InstallVersion), move |ctx| {
        with_job(ctx, |job| {
            script(job);
            Ok(())
        })
    });
    assert_eq!(handle.wait(), TaskOutcome::Succeeded);
    let mut seen = Vec::new();
    while let Some(event) = events.next_timeout(Duration::from_millis(500)) {
        match event {
            vl_core::Event::Task(TaskEvent::Progress { progress, .. }) => seen.push((
                progress.stage.unwrap_or_default(),
                progress.done,
                progress.total,
                progress.unit,
            )),
            vl_core::Event::Task(TaskEvent::Finished { .. }) => break,
            _ => {}
        }
    }
    seen
}

#[test]
fn every_stage_keeps_its_own_counters_and_its_last_value() {
    let seen = published(|job| {
        job.report("metadata", 100, Some(400), ProgressUnit::Bytes);
        job.report("downloading", 10, Some(100), ProgressUnit::Bytes);
        job.report("downloading", 100, Some(100), ProgressUnit::Bytes);
        job.report("natives", 0, Some(0), ProgressUnit::Items);
        job.report("assets", 1, Some(2), ProgressUnit::Items);
    });
    // A burst inside a stage may be shortened to its newest value; each stage ends with its
    // final value, in order, and no stage carries a number of another.
    let stages: Vec<_> = seen.iter().map(|e| e.0.as_str()).collect();
    let mut order = stages.clone();
    order.dedup();
    assert_eq!(order, ["metadata", "downloading", "assets"], "{seen:?}");
    let last = |stage: &str| seen.iter().rev().find(|e| e.0 == stage).unwrap().clone();
    assert_eq!(
        last("metadata"),
        ("metadata".into(), 100, Some(400), ProgressUnit::Bytes)
    );
    assert_eq!(
        last("downloading"),
        ("downloading".into(), 100, Some(100), ProgressUnit::Bytes)
    );
    assert_eq!(
        last("assets"),
        ("assets".into(), 1, Some(2), ProgressUnit::Items)
    );
}

#[test]
fn the_last_report_before_the_work_ends_is_delivered() {
    let seen = published(|job| {
        job.report("downloading", 877, Some(877), ProgressUnit::Bytes);
    });
    assert_eq!(
        seen.last().unwrap(),
        &("downloading".into(), 877, Some(877), ProgressUnit::Bytes)
    );
}
