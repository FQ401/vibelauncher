//! The task engine behind every long operation (downloads, installs, sign-in, launch).
//!
//! - A task has weighted stages and nested subtasks; the overall progress is their weighted sum.
//! - Cancellation is cooperative and flows from a parent to its subtasks ([`CancelToken`]); a task
//!   ends as succeeded, failed (including a timeout and a panic) or cancelled
//!   ([`vl_core::task::TaskOutcome`]).
//! - Every change is published as a [`vl_core::task::TaskEvent`] on the facade's event bus.
//! - A pool of worker threads limits how many tasks run at the same time; the rest wait queued.
//!
//! # Using the engine
//!
//! ```
//! use std::sync::Arc;
//! use vl_core::EventBus;
//! use vl_core::task::{TaskKind, TaskOutcome};
//! use vl_tasks::{TaskEngine, TaskSpec};
//!
//! let engine = TaskEngine::new(Arc::new(EventBus::default()), 4);
//! let handle = engine.spawn(TaskSpec::new(TaskKind::InstallVersion).subject("1.21"), |task| {
//!     task.expect_weight(4.0); // the weights of all stages, so progress does not jump
//!     task.run_stage("libraries", 3.0, |stage| {
//!         for done in 1..=10 {
//!             stage.check()?; // stops here when cancelled or timed out
//!             stage.report_items(done, 10);
//!         }
//!         Ok(())
//!     })?;
//!     task.run_stage("assets", 1.0, |_| Ok(()))
//! });
//! assert_eq!(handle.wait(), TaskOutcome::Succeeded);
//! ```
//!
//! Work that needs a task from a command: `spawn` returns a [`TaskHandle`] whose `id()` goes into
//! `Response::Task`. Blocking calls inside the work (HTTP, file copies) check
//! [`TaskContext::check`] between steps; the engine never kills a thread.

mod cancel;
mod engine;
mod node;

pub use cancel::{CancelToken, StopReason};
pub use engine::{TaskEngine, TaskHandle, TaskSpec};
pub use node::TaskContext;
