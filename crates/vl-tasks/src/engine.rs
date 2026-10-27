//! The engine: submits tasks to a pool of workers and finds them by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vl_core::task::{TaskKind, TaskOutcome, TaskSnapshot};
use vl_core::{Error, EventBus, TaskId};

use crate::cancel::CancelToken;
use crate::node::{Job, Node, Queue, Shared, TaskContext, lock, worker};

/// What a task is, before it runs.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub(crate) kind: TaskKind,
    pub(crate) subject: Option<String>,
    pub(crate) stage: Option<String>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) cancellable: bool,
}

impl TaskSpec {
    /// A cancellable task of `kind` without a time limit.
    pub fn new(kind: TaskKind) -> Self {
        Self {
            kind,
            subject: None,
            stage: None,
            timeout: None,
            cancellable: true,
        }
    }

    /// What the task works on (an instance name, a version id).
    #[must_use]
    pub fn subject(mut self, subject: &str) -> Self {
        self.subject = Some(subject.to_owned());
        self
    }

    /// The stage a subtask stands for; the parent shows it while the subtask runs.
    #[must_use]
    pub fn stage(mut self, stage: &str) -> Self {
        self.stage = Some(stage.to_owned());
        self
    }

    /// A time limit counted from when the task starts running (not from when it is queued). The
    /// work notices it through its cancel token and the task ends as failed.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Marks the task as one `TaskCommand::Cancel` must refuse to stop.
    #[must_use]
    pub fn not_cancellable(mut self) -> Self {
        self.cancellable = false;
        self
    }
}

/// A submitted task.
#[derive(Clone)]
pub struct TaskHandle {
    node: Arc<Node>,
}

impl TaskHandle {
    pub(crate) fn new(node: Arc<Node>) -> Self {
        Self { node }
    }

    /// The id to answer `Response::Task` with.
    pub fn id(&self) -> TaskId {
        self.node.info.id
    }

    /// Asks the task and its subtasks to stop.
    pub fn cancel(&self) {
        self.node.cancel();
    }

    /// The token of the task.
    pub fn token(&self) -> &CancelToken {
        &self.node.token
    }

    /// How the task ended, if it has.
    pub fn try_outcome(&self) -> Option<TaskOutcome> {
        self.node.outcome()
    }

    /// Blocks until the task ended.
    pub fn wait(&self) -> TaskOutcome {
        self.node.wait_outcome()
    }
}

/// Stops the workers when the last engine handle goes away.
struct Guard(Arc<Shared>);

impl Drop for Guard {
    fn drop(&mut self) {
        let jobs = {
            let mut queue = lock(&self.0.queue);
            queue.shutdown = true;
            std::mem::take(&mut queue.jobs)
        };
        self.0.wake.notify_all();
        for job in jobs {
            job.node.cancel();
        }
    }
}

/// Runs tasks with at most `max_parallel` of them at the same time; cloning shares the engine.
#[derive(Clone)]
pub struct TaskEngine {
    shared: Arc<Shared>,
    _guard: Arc<Guard>,
}

impl TaskEngine {
    /// An engine that publishes task events on `bus` and runs up to `max_parallel` tasks at once
    /// (at least one); more wait in a queue. Subtasks do not count against the limit.
    pub fn new(bus: Arc<EventBus>, max_parallel: usize) -> Self {
        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            tasks: Mutex::new(HashMap::new()),
            bus,
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            workers: AtomicUsize::new(0),
        });
        for n in 0..max_parallel.max(1) {
            let worker_shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new()
                .name(format!("vl-task-{n}"))
                .spawn(move || worker(&worker_shared));
            if spawned.is_ok() {
                shared.workers.fetch_add(1, Ordering::Relaxed);
            }
        }
        Self {
            _guard: Arc::new(Guard(Arc::clone(&shared))),
            shared,
        }
    }

    /// Submits a task. `Started` is published at once; the work runs when a worker is free. The
    /// task ends as succeeded on `Ok`, cancelled on a `Cancelled` error, failed on any other
    /// error, a timeout or a panic.
    pub fn spawn(
        &self,
        spec: TaskSpec,
        work: impl FnOnce(&TaskContext) -> Result<(), Error> + Send + 'static,
    ) -> TaskHandle {
        let node = Node::create(&self.shared, None, &spec, 1.0, true);
        let handle = TaskHandle::new(Arc::clone(&node));
        let refused = {
            let mut queue = lock(&self.shared.queue);
            if queue.shutdown || self.shared.workers.load(Ordering::Relaxed) == 0 {
                true
            } else {
                queue.jobs.push_back(Job {
                    node: Arc::clone(&node),
                    work: Box::new(work),
                });
                false
            }
        };
        if refused {
            node.finish(TaskOutcome::Failed(Error::internal(
                "the task engine cannot run tasks",
            )));
        } else {
            self.shared.wake.notify_one();
        }
        handle
    }

    /// Asks a task (or subtask) and its subtasks to stop. A queued task ends at once.
    ///
    /// Fails with `NotFound` for an unknown or finished task and `Conflict` for a task that
    /// cannot be cancelled.
    pub fn cancel(&self, id: TaskId) -> Result<(), Error> {
        let node = lock(&self.shared.tasks)
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("no running {id}")))?;
        if !node.info.cancellable {
            return Err(Error::conflict(format!("{id} cannot be cancelled")));
        }
        node.cancel();
        Ok(())
    }

    /// The tasks that have not finished (subtasks included), oldest first.
    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let mut nodes: Vec<Arc<Node>> = lock(&self.shared.tasks).values().cloned().collect();
        nodes.sort_by_key(|n| n.info.id);
        nodes.iter().map(|n| n.snapshot()).collect()
    }
}
