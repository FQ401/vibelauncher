//! A task in the tree: its state, aggregated progress and the context its work uses.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vl_core::task::{Progress, ProgressUnit, TaskEvent, TaskInfo, TaskOutcome, TaskSnapshot};
use vl_core::{Error, ErrorKind, Event, EventBus, TaskId};

use crate::cancel::{CancelToken, StopReason, timeout_error};
use crate::engine::{TaskHandle, TaskSpec};

/// The resolution of an aggregated progress (`done` out of this many steps).
const AGGREGATE_STEPS: u32 = 1000;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The work of a root task, run on a pool thread.
pub(crate) type Work = Box<dyn FnOnce(&TaskContext) -> Result<(), Error> + Send>;

pub(crate) struct Job {
    pub(crate) node: Arc<Node>,
    pub(crate) work: Work,
}

#[derive(Default)]
pub(crate) struct Queue {
    pub(crate) jobs: VecDeque<Job>,
    pub(crate) shutdown: bool,
}

/// What every node of one engine shares.
pub(crate) struct Shared {
    pub(crate) next_id: AtomicU64,
    pub(crate) tasks: Mutex<HashMap<TaskId, Arc<Node>>>,
    pub(crate) bus: Arc<EventBus>,
    pub(crate) queue: Mutex<Queue>,
    pub(crate) wake: Condvar,
    pub(crate) workers: AtomicUsize,
}

impl Shared {
    fn publish(&self, event: TaskEvent) {
        self.bus.publish(&Event::Task(event));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Finished,
}

struct Child {
    id: TaskId,
    weight: f64,
    fraction: f64,
}

struct State {
    phase: Phase,
    own: Option<Progress>,
    own_weight: f64,
    planned_weight: f64,
    stage: Option<String>,
    children: Vec<Child>,
    live_children: usize,
    fraction: f64,
    last: Option<Progress>,
    outcome: Option<TaskOutcome>,
}

pub(crate) struct Node {
    shared: Arc<Shared>,
    pub(crate) info: TaskInfo,
    pub(crate) token: CancelToken,
    timeout: Option<Duration>,
    parent: Option<Arc<Node>>,
    state: Mutex<State>,
    changed: Condvar,
}

fn weight_of(weight: f64) -> f64 {
    if weight.is_finite() && weight > 0.0 {
        weight
    } else {
        0.0
    }
}

impl Node {
    /// Creates a node, announces it (`Started`) and registers it as live.
    pub(crate) fn create(
        shared: &Arc<Shared>,
        parent: Option<&Arc<Node>>,
        spec: &TaskSpec,
        weight: f64,
        queued: bool,
    ) -> Arc<Node> {
        let id = TaskId(shared.next_id.fetch_add(1, Ordering::Relaxed));
        let token = parent.map_or_else(CancelToken::new, |p| p.token.child());
        let info = TaskInfo {
            id,
            parent: parent.map(|p| p.info.id),
            kind: spec.kind,
            subject: spec.subject.clone(),
            cancellable: spec.cancellable,
        };
        let node = Arc::new(Node {
            shared: Arc::clone(shared),
            info,
            token,
            timeout: spec.timeout,
            parent: parent.cloned(),
            state: Mutex::new(State {
                phase: if queued {
                    Phase::Queued
                } else {
                    Phase::Running
                },
                own: None,
                own_weight: 0.0,
                planned_weight: 0.0,
                stage: None,
                children: Vec::new(),
                live_children: 0,
                fraction: 0.0,
                last: None,
                outcome: None,
            }),
            changed: Condvar::new(),
        });
        if !queued && let Some(timeout) = spec.timeout {
            node.token.arm_timeout(timeout);
        }
        shared.publish(TaskEvent::Started(node.info.clone()));
        lock(&shared.tasks).insert(id, Arc::clone(&node));
        if let Some(p) = parent {
            p.add_child(id, weight_of(weight), spec.stage.as_deref());
        }
        node
    }

    pub(crate) fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            info: self.info.clone(),
            progress: lock(&self.state).last.clone(),
        }
    }

    pub(crate) fn outcome(&self) -> Option<TaskOutcome> {
        lock(&self.state).outcome.clone()
    }

    pub(crate) fn wait_outcome(&self) -> TaskOutcome {
        let mut state = lock(&self.state);
        loop {
            if let Some(outcome) = &state.outcome {
                return outcome.clone();
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Moves a queued node to running; false if it already finished (cancelled while queued).
    fn begin(&self) -> bool {
        let mut state = lock(&self.state);
        if state.phase != Phase::Queued {
            return false;
        }
        state.phase = Phase::Running;
        if let Some(timeout) = self.timeout {
            self.token.arm_timeout(timeout);
        }
        true
    }

    /// Asks the node and its subtasks to stop; a queued node ends at once.
    pub(crate) fn cancel(&self) {
        self.token.cancel();
        let queued = lock(&self.state).phase == Phase::Queued;
        if queued {
            self.finish(TaskOutcome::Cancelled);
        }
    }

    fn add_child(&self, id: TaskId, weight: f64, stage: Option<&str>) {
        let fraction = {
            let mut state = lock(&self.state);
            state.children.push(Child {
                id,
                weight,
                fraction: 0.0,
            });
            state.live_children += 1;
            if let Some(stage) = stage {
                state.stage = Some(stage.to_owned());
            }
            self.refresh(&mut state)
        };
        self.propagate(fraction, false);
    }

    /// Recomputes the fraction and publishes the progress if it changed. Called with the lock held
    /// so a node's events keep their order and nothing follows its `Finished`.
    fn refresh(&self, state: &mut State) -> f64 {
        if state.phase == Phase::Finished {
            return state.fraction;
        }
        let own_fraction = state
            .own
            .as_ref()
            .and_then(Progress::fraction)
            .map_or(0.0, f64::from);
        let progress = if state.children.is_empty() {
            state.fraction = own_fraction;
            state.own.clone().map(|mut p| {
                p.stage.clone_from(&state.stage);
                p
            })
        } else {
            let total: f64 = (state.own_weight
                + state.children.iter().map(|c| c.weight).sum::<f64>())
            .max(state.planned_weight);
            state.fraction = if total > 0.0 {
                let sum: f64 = own_fraction * state.own_weight
                    + state
                        .children
                        .iter()
                        .map(|c| c.fraction * c.weight)
                        .sum::<f64>();
                (sum / total).clamp(0.0, 1.0)
            } else {
                0.0
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let done = (state.fraction * f64::from(AGGREGATE_STEPS)).round() as u64;
            Some(Progress {
                done,
                total: Some(u64::from(AGGREGATE_STEPS)),
                unit: ProgressUnit::Steps,
                stage: state.stage.clone(),
            })
        };
        if let Some(progress) = progress
            && state.last.as_ref() != Some(&progress)
        {
            state.last = Some(progress.clone());
            self.shared.publish(TaskEvent::Progress {
                id: self.info.id,
                progress,
            });
        }
        state.fraction
    }

    /// Passes a changed fraction up the ancestors; holds one lock at a time.
    fn propagate(&self, mut fraction: f64, finished: bool) {
        let mut id = self.info.id;
        let mut finished = finished;
        let mut next = self.parent.clone();
        while let Some(node) = next {
            {
                let mut state = lock(&node.state);
                if let Some(child) = state.children.iter_mut().find(|c| c.id == id) {
                    child.fraction = fraction;
                }
                if finished {
                    state.live_children = state.live_children.saturating_sub(1);
                    node.changed.notify_all();
                }
                fraction = node.refresh(&mut state);
            }
            id = node.info.id;
            finished = false;
            next = node.parent.clone();
        }
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        let fraction = {
            let mut state = lock(&self.state);
            if state.phase == Phase::Finished {
                return;
            }
            change(&mut state);
            self.refresh(&mut state)
        };
        self.propagate(fraction, false);
    }

    /// Ends the node with `outcome`; only the first call has an effect, so `Finished` is
    /// published exactly once.
    pub(crate) fn finish(&self, outcome: TaskOutcome) {
        let fraction = {
            let mut state = lock(&self.state);
            if state.phase == Phase::Finished {
                return;
            }
            if outcome == TaskOutcome::Succeeded {
                state.fraction = 1.0;
            }
            state.phase = Phase::Finished;
            state.outcome = Some(outcome.clone());
            self.shared.publish(TaskEvent::Finished {
                id: self.info.id,
                outcome,
            });
            lock(&self.shared.tasks).remove(&self.info.id);
            self.changed.notify_all();
            state.fraction
        };
        self.propagate(fraction, true);
    }

    fn wait_children(&self) {
        let mut state = lock(&self.state);
        while state.live_children > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Runs `work` for this (running) node and finishes the node with its result.
    ///
    /// A panic becomes a failure; on any non-success the subtasks are cancelled and awaited, so
    /// the node always ends after them.
    pub(crate) fn run<T>(
        self: &Arc<Self>,
        work: impl FnOnce(&TaskContext) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let ctx = TaskContext {
            node: Arc::clone(self),
        };
        let result = match catch_unwind(AssertUnwindSafe(|| work(&ctx))) {
            Ok(result) => result,
            Err(payload) => Err(Error::internal(format!(
                "task panicked: {}",
                panic_message(payload.as_ref())
            ))),
        };
        let result = result.map_err(|e| {
            if e.kind == ErrorKind::Cancelled && self.token.reason() == Some(StopReason::TimedOut) {
                timeout_error()
            } else {
                e
            }
        });
        let outcome = match &result {
            Ok(_) => TaskOutcome::Succeeded,
            Err(e) if e.kind == ErrorKind::Cancelled => TaskOutcome::Cancelled,
            Err(e) => TaskOutcome::Failed(e.clone()),
        };
        if outcome != TaskOutcome::Succeeded {
            self.token.cancel();
        }
        self.wait_children();
        self.finish(outcome);
        result
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

/// What a task's work uses: cancellation, progress reporting and subtasks.
pub struct TaskContext {
    node: Arc<Node>,
}

impl TaskContext {
    /// This task's id.
    pub fn id(&self) -> TaskId {
        self.node.info.id
    }

    /// The token that stops this task (and is stopped by its parent).
    pub fn token(&self) -> &CancelToken {
        &self.node.token
    }

    /// Whether the task should stop (cancelled, parent stopped or timed out).
    pub fn is_cancelled(&self) -> bool {
        self.node.token.is_cancelled()
    }

    /// `Ok` while the work may continue; `Err` to return with `?` once it should stop.
    pub fn check(&self) -> Result<(), Error> {
        self.node.token.check()
    }

    /// Reports own progress. Without subtasks it is the task's progress; with subtasks it counts
    /// for the share set by [`TaskContext::set_own_weight`].
    pub fn report(&self, done: u64, total: Option<u64>, unit: ProgressUnit) {
        self.node.update(|state| {
            state.own = Some(Progress {
                done,
                total,
                unit,
                stage: state.stage.clone(),
            });
        });
    }

    /// [`TaskContext::report`] in bytes.
    pub fn report_bytes(&self, done: u64, total: Option<u64>) {
        self.report(done, total, ProgressUnit::Bytes);
    }

    /// [`TaskContext::report`] in items.
    pub fn report_items(&self, done: u64, total: u64) {
        self.report(done, Some(total), ProgressUnit::Items);
    }

    /// Names the current stage (English technical text, e.g. `libraries`).
    pub fn set_stage(&self, stage: &str) {
        self.node
            .update(|state| state.stage = Some(stage.to_owned()));
    }

    /// The weight of this task's own reported progress next to its subtasks (default 0, so only
    /// the subtasks count).
    pub fn set_own_weight(&self, weight: f64) {
        self.node
            .update(|state| state.own_weight = weight_of(weight));
    }

    /// Declares the sum of the weights of all subtasks this task will run (plus its own weight),
    /// so progress does not jump while later subtasks are not created yet. Without it the total
    /// is the weights of the subtasks created so far.
    pub fn expect_weight(&self, total: f64) {
        self.node
            .update(|state| state.planned_weight = weight_of(total));
    }

    /// Runs a subtask on this thread and returns its result; `?` passes its failure on.
    /// `weight` is its share of this task's progress. Fails at once, without creating the
    /// subtask, when this task should already stop.
    pub fn run_subtask<T>(
        &self,
        spec: TaskSpec,
        weight: f64,
        work: impl FnOnce(&TaskContext) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.check()?;
        let child = Node::create(&self.node.shared, Some(&self.node), &spec, weight, false);
        child.run(work)
    }

    /// A subtask of this task's own kind and subject that names a stage.
    pub fn run_stage<T>(
        &self,
        name: &str,
        weight: f64,
        work: impl FnOnce(&TaskContext) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut spec = TaskSpec::new(self.node.info.kind).stage(name);
        if let Some(subject) = &self.node.info.subject {
            spec = spec.subject(subject);
        }
        self.run_subtask(spec, weight, work)
    }

    /// Starts a subtask on its own thread (outside the pool limit, so a parent waiting on its
    /// subtasks cannot starve the pool) for work that runs in parallel. The task ends only after
    /// all its subtasks ended. A failing subtask does not stop its siblings; cancel
    /// [`TaskContext::token`] or return its failure to do that.
    pub fn spawn_subtask(
        &self,
        spec: TaskSpec,
        weight: f64,
        work: impl FnOnce(&TaskContext) -> Result<(), Error> + Send + 'static,
    ) -> TaskHandle {
        let child = Node::create(&self.node.shared, Some(&self.node), &spec, weight, false);
        let runner = Arc::clone(&child);
        let spawned = std::thread::Builder::new()
            .name("vl-subtask".into())
            .spawn(move || {
                let _ = runner.run(work);
            });
        if spawned.is_err() {
            child.finish(TaskOutcome::Failed(Error::internal(
                "cannot start a thread for the subtask",
            )));
        }
        TaskHandle::new(child)
    }
}

/// Runs queued jobs until the engine shuts down.
pub(crate) fn worker(shared: &Arc<Shared>) {
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if let Some(job) = queue.jobs.pop_front() {
                    break Some(job);
                }
                if queue.shutdown {
                    break None;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(Job { node, work }) = job else {
            return;
        };
        if node.begin() {
            let _ = node.run(work);
        }
    }
}
