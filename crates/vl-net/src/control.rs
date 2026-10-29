//! Cancellation, progress reporting and the global download limit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::error::NetError;

/// A cooperative cancellation flag shared between clones.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks every operation holding this token to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn check(&self) -> Result<(), NetError> {
        if self.is_cancelled() {
            Err(NetError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Sleeps for `total`, waking early with `Cancelled` when cancelled.
    pub(crate) fn sleep(&self, total: Duration) -> Result<(), NetError> {
        let slice = Duration::from_millis(10);
        let mut left = total;
        while !left.is_zero() {
            self.check()?;
            let step = left.min(slice);
            std::thread::sleep(step);
            left -= step;
        }
        self.check()
    }
}

/// Bytes transferred so far. For a batch, the sum over all files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes received (retried bytes are subtracted again).
    pub done: u64,
    /// Total bytes when known in advance.
    pub total: Option<u64>,
}

/// A progress callback. It is called from download threads and must be cheap and non-blocking.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Cancellation and progress for one operation. The task engine owns the token and the callback;
/// this crate does not depend on it.
#[derive(Clone, Default)]
pub struct Control {
    /// The cancellation flag.
    pub cancel: CancelToken,
    /// Where progress goes, if anywhere.
    pub progress: Option<ProgressFn>,
}

impl Control {
    /// A control with the given token and no progress callback.
    pub fn with_cancel(cancel: CancelToken) -> Self {
        Self {
            cancel,
            progress: None,
        }
    }

    /// Sets the progress callback.
    pub fn on_progress(mut self, f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(f));
        self
    }
}

/// Counts bytes of one operation or batch and reports them. The count and its report change
/// together, so concurrent workers deliver the counts in order.
pub(crate) struct Tally {
    done: Mutex<u64>,
    total: Option<u64>,
    sink: Option<ProgressFn>,
}

impl Tally {
    pub(crate) fn new(total: Option<u64>, sink: Option<ProgressFn>) -> Self {
        Self {
            done: Mutex::new(0),
            total,
            sink,
        }
    }

    pub(crate) fn add(&self, n: u64) {
        self.update(|d| d + n);
    }

    pub(crate) fn sub(&self, n: u64) {
        self.update(|d| d.saturating_sub(n));
    }

    fn update(&self, change: impl FnOnce(u64) -> u64) {
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        *done = change(*done);
        if let Some(sink) = &self.sink {
            sink(Progress {
                done: *done,
                total: self.total,
            });
        }
    }
}

/// A counting semaphore that gives up when cancelled.
pub(crate) struct Semaphore {
    free: Mutex<usize>,
    wake: Condvar,
}

pub(crate) struct Permit<'a>(&'a Semaphore);

impl Semaphore {
    pub(crate) fn new(permits: usize) -> Self {
        Self {
            free: Mutex::new(permits.max(1)),
            wake: Condvar::new(),
        }
    }

    pub(crate) fn acquire(&self, cancel: &CancelToken) -> Result<Permit<'_>, NetError> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            cancel.check()?;
            if *free > 0 {
                *free -= 1;
                return Ok(Permit(self));
            }
            let (guard, _) = self
                .wake
                .wait_timeout(free, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner());
            free = guard;
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut free = self.0.free.lock().unwrap_or_else(|e| e.into_inner());
        *free += 1;
        self.0.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semaphore_blocks_until_release_and_honours_cancel() {
        let sem = Semaphore::new(1);
        let token = CancelToken::new();
        let first = sem.acquire(&token).ok();
        assert!(first.is_some());
        let other = CancelToken::new();
        other.cancel();
        assert_eq!(sem.acquire(&other).err(), Some(NetError::Cancelled));
        drop(first);
        assert!(sem.acquire(&token).is_ok());
    }

    #[test]
    fn tally_reports_and_subtracts() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let tally = Tally::new(
            Some(10),
            Some(Arc::new(move |p| {
                s.lock().unwrap_or_else(|e| e.into_inner()).push(p.done);
            })),
        );
        tally.add(5);
        tally.add(3);
        tally.sub(8);
        assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), [5, 8, 0]);
    }
}
