//! Cooperative cancellation with timeouts, inherited from a parent task.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vl_core::{Error, ErrorKind};

const LIVE: u8 = 0;
const CANCELLED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// Why work should stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The user (or a parent) cancelled it.
    Cancelled,
    /// Its time limit passed.
    TimedOut,
}

struct Inner {
    state: AtomicU8,
    deadline: Mutex<Option<Instant>>,
    parent: Option<CancelToken>,
}

/// A flag the work polls; cloning shares it. A child token stops when its parent stops.
#[derive(Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("reason", &self.reason())
            .finish()
    }
}

impl CancelToken {
    /// A token that nothing has stopped.
    pub fn new() -> Self {
        Self::with_parent(None)
    }

    /// A token that also stops when `self` stops.
    pub fn child(&self) -> Self {
        Self::with_parent(Some(self.clone()))
    }

    fn with_parent(parent: Option<Self>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: AtomicU8::new(LIVE),
                deadline: Mutex::new(None),
                parent,
            }),
        }
    }

    /// Asks the work to stop; the first reason wins.
    pub fn cancel(&self) {
        let _ =
            self.inner
                .state
                .compare_exchange(LIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Starts a time limit counted from now.
    pub fn arm_timeout(&self, timeout: Duration) {
        *self
            .inner
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now().checked_add(timeout);
    }

    /// Why the work should stop, if it should.
    pub fn reason(&self) -> Option<StopReason> {
        match self.inner.state.load(Ordering::Acquire) {
            CANCELLED => return Some(StopReason::Cancelled),
            TIMED_OUT => return Some(StopReason::TimedOut),
            _ => {}
        }
        let deadline = *self
            .inner
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if deadline.is_some_and(|d| Instant::now() >= d) {
            // Keep the reason once seen, even if someone cancels afterwards.
            let _ = self.inner.state.compare_exchange(
                LIVE,
                TIMED_OUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return self.reason();
        }
        self.inner.parent.as_ref().and_then(Self::reason)
    }

    /// Whether the work should stop.
    pub fn is_cancelled(&self) -> bool {
        self.reason().is_some()
    }

    /// `Ok` while the work may continue; the error to return (with `?`) once it should stop.
    pub fn check(&self) -> Result<(), Error> {
        match self.reason() {
            None => Ok(()),
            Some(StopReason::Cancelled) => Err(Error::cancelled()),
            Some(StopReason::TimedOut) => Err(timeout_error()),
        }
    }
}

/// The error of a task that ran out of time.
pub(crate) fn timeout_error() -> Error {
    Error::new(ErrorKind::Network, "task timed out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_stops_with_its_parent_but_not_the_other_way() {
        let parent = CancelToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
        let other = parent.child();
        assert!(other.check().is_ok());
        parent.cancel();
        assert_eq!(other.reason(), Some(StopReason::Cancelled));
        assert_eq!(other.check().unwrap_err().kind, ErrorKind::Cancelled);
    }

    #[test]
    fn a_passed_deadline_is_a_timeout_and_stays_one() {
        let token = CancelToken::new();
        token.arm_timeout(Duration::ZERO);
        assert_eq!(token.reason(), Some(StopReason::TimedOut));
        token.cancel();
        assert_eq!(token.reason(), Some(StopReason::TimedOut));
        assert_eq!(token.check().unwrap_err(), timeout_error());
    }
}
