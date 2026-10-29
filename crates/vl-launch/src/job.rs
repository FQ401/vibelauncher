//! Cancellation and progress of one preparation.

use std::sync::Arc;

use vl_core::task::{Progress, ProgressUnit};
use vl_core::{Error, Result};
use vl_net::{CancelToken, Control};

/// Where progress goes. Called from download threads; it must be cheap and must not block.
pub type ProgressSink = Arc<dyn Fn(Progress) + Send + Sync>;

/// The cancellation flag and the progress sink of one operation. The caller (the task engine in
/// the backend) owns both; this crate does not depend on it.
#[derive(Clone, Default)]
pub struct Job {
    cancel: CancelToken,
    sink: Option<ProgressSink>,
}

impl Job {
    /// A job that reports nowhere and is never cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// A job with the given cancellation flag.
    pub fn with_cancel(cancel: CancelToken) -> Self {
        Self { cancel, sink: None }
    }

    /// Sets the progress sink.
    #[must_use]
    pub fn on_progress(mut self, sink: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.sink = Some(Arc::new(sink));
        self
    }

    /// The cancellation flag.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Whether the operation was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// `Err(Cancelled)` once cancelled.
    ///
    /// # Errors
    ///
    /// `Cancelled` when the job was cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }

    /// Reports progress of `stage`. A stage whose total is zero has nothing to do and reports
    /// nothing.
    pub fn report(&self, stage: &str, done: u64, total: Option<u64>, unit: ProgressUnit) {
        if total == Some(0) {
            return;
        }
        if let Some(sink) = &self.sink {
            sink(Progress {
                done,
                total,
                unit,
                stage: Some(stage.to_owned()),
            });
        }
    }

    /// A [`Control`] for `vl-net` whose byte progress is reported as `stage`, against `total`
    /// bytes overall (`0` when the size is not known).
    pub(crate) fn net_control(&self, stage: &'static str, total: u64) -> Control {
        let total = (total > 0).then_some(total);
        let mut control = Control::with_cancel(self.cancel.clone());
        if let Some(sink) = self.sink.clone() {
            control = control.on_progress(move |p| {
                sink(Progress {
                    done: p.done,
                    total,
                    unit: ProgressUnit::Bytes,
                    stage: Some(stage.to_owned()),
                });
            });
        }
        control
    }
}
