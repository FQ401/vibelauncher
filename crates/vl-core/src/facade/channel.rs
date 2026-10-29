//! The plumbing every facade implementation shares: one-shot replies and the event bus.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use super::{Event, Response};
use crate::error::{Error, Result};

/// Creates a reply and the sender that fills it.
pub fn reply_channel() -> (Replier, Reply) {
    let (tx, rx) = mpsc::sync_channel(1);
    (Replier(tx), Reply(rx))
}

/// The backend's end of a [`Reply`]: send the result once.
#[derive(Debug)]
pub struct Replier(SyncSender<Result<Response>>);

impl Replier {
    /// Sends the result. A front end that dropped its reply simply never sees it.
    pub fn send(self, result: Result<Response>) {
        // The only possible failure is a dropped `Reply`, which means nobody waits for it.
        let _ = self.0.try_send(result);
    }
}

/// The front end's end of a command: holds exactly one result.
#[derive(Debug)]
pub struct Reply(Receiver<Result<Response>>);

impl Reply {
    /// A reply that already holds `result`.
    pub fn ready(result: Result<Response>) -> Self {
        let (replier, reply) = reply_channel();
        replier.send(result);
        reply
    }

    /// The result if it arrived; `None` while the command still runs. Once the result was taken,
    /// later calls report an internal error.
    pub fn try_take(&self) -> Option<Result<Response>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(dropped())),
        }
    }

    /// Blocks until the result arrives (native front ends only).
    pub fn wait(self) -> Result<Response> {
        self.0.recv().unwrap_or_else(|_| Err(dropped()))
    }

    /// Blocks up to `timeout`; `None` if the result did not arrive in time.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<Result<Response>> {
        match self.0.recv_timeout(timeout) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(dropped())),
        }
    }
}

fn dropped() -> Error {
    Error::internal("the backend dropped the command without a reply")
}

struct Subscriber {
    tx: SyncSender<Event>,
    missed: Arc<AtomicU64>,
}

/// Fans events out to every [`EventStream`]. Each stream has a bounded queue; when a stream is
/// full its events are dropped and counted, and the stream reports [`Event::Lagged`] — a slow
/// front end never slows the launcher down or grows its memory.
pub struct EventBus {
    capacity: usize,
    subscribers: Mutex<Vec<Subscriber>>,
    observer: OnceLock<Observer>,
}

/// Sees every event as it is published, on the publishing thread, before the streams do (the
/// launcher log file). It must be quick and must not publish itself.
pub type Observer = Box<dyn Fn(&Event) + Send + Sync>;

impl EventBus {
    /// The default queue length of each stream.
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// A bus whose streams queue up to `capacity` events each (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            subscribers: Mutex::new(Vec::new()),
            observer: OnceLock::new(),
        }
    }

    /// Sets the observer of every published event; there is at most one, and a second one is
    /// refused (returns `false`).
    pub fn observe(&self, observer: Observer) -> bool {
        self.observer.set(observer).is_ok()
    }

    /// Opens a stream of the events published from now on.
    pub fn subscribe(&self) -> EventStream {
        let (tx, rx) = mpsc::sync_channel(self.capacity);
        let missed = Arc::new(AtomicU64::new(0));
        self.lock().push(Subscriber {
            tx,
            missed: Arc::clone(&missed),
        });
        EventStream { rx, missed }
    }

    /// Sends `event` to every open stream; closed streams are forgotten.
    pub fn publish(&self, event: &Event) {
        if let Some(observer) = self.observer.get() {
            observer(event);
        }
        self.lock().retain(|s| match s.tx.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                s.missed.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscriber>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

/// A front end's view of the events. Dropping it unsubscribes.
pub struct EventStream {
    rx: Receiver<Event>,
    missed: Arc<AtomicU64>,
}

impl EventStream {
    /// The next queued event, without blocking. After the queue drains, dropped events are
    /// reported once as [`Event::Lagged`].
    pub fn try_next(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(_) => self.lagged(),
        }
    }

    /// Blocks up to `timeout` for the next event (native front ends only).
    pub fn next_timeout(&self, timeout: Duration) -> Option<Event> {
        if let Some(event) = self.try_next() {
            return Some(event);
        }
        self.rx.recv_timeout(timeout).ok()
    }

    /// Every event queued now; the UI calls this once per frame.
    pub fn drain(&self) -> impl Iterator<Item = Event> + '_ {
        std::iter::from_fn(|| self.try_next())
    }

    fn lagged(&self) -> Option<Event> {
        match self.missed.swap(0, Ordering::Relaxed) {
            0 => None,
            missed => Some(Event::Lagged { missed }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::VersionEvent;

    fn event() -> Event {
        Event::Version(VersionEvent::ListChanged)
    }

    #[test]
    fn a_reply_holds_one_result() {
        let (replier, reply) = reply_channel();
        assert!(reply.try_take().is_none());
        replier.send(Ok(Response::Done));
        assert_eq!(reply.try_take(), Some(Ok(Response::Done)));
        assert!(matches!(reply.try_take(), Some(Err(_))));
    }

    #[test]
    fn a_dropped_replier_is_an_internal_error() {
        let (replier, reply) = reply_channel();
        drop(replier);
        let err = reply.wait().unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::Internal);
    }

    #[test]
    fn streams_only_see_events_after_subscribing() {
        let bus = EventBus::default();
        bus.publish(&event());
        let stream = bus.subscribe();
        assert!(stream.try_next().is_none());
        bus.publish(&event());
        assert_eq!(stream.drain().count(), 1);
    }

    #[test]
    fn a_full_stream_drops_and_reports_lag() {
        let bus = EventBus::new(2);
        let stream = bus.subscribe();
        for _ in 0..5 {
            bus.publish(&event());
        }
        let events: Vec<Event> = stream.drain().collect();
        assert_eq!(events, vec![event(), event(), Event::Lagged { missed: 3 }]);
    }

    #[test]
    fn the_observer_sees_every_event_and_there_is_one() {
        let bus = EventBus::new(1);
        let seen = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&seen);
        assert!(bus.observe(Box::new(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        })));
        assert!(!bus.observe(Box::new(|_| {})));
        let _stream = bus.subscribe();
        for _ in 0..3 {
            bus.publish(&event());
        }
        assert_eq!(seen.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn dropped_streams_are_forgotten() {
        let bus = EventBus::default();
        drop(bus.subscribe());
        bus.publish(&event());
        assert!(bus.lock().is_empty());
    }
}
