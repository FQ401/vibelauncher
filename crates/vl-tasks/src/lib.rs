//! The task engine behind every long operation (downloads, installs, sign-in, launch).
//!
//! - A task has stages with weights and nested subtasks; the overall progress is their weighted sum.
//! - Cancellation is cooperative and flows from a parent to its subtasks; a task ends as
//!   succeeded, failed or cancelled (`vl_core::task::TaskOutcome`).
//! - Every change is published as a `vl_core::task::TaskEvent` on the facade's event stream.
//! - A limit on the number of tasks that run at the same time.
