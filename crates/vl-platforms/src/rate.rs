//! Keeping within a server's rate limit.
//!
//! [`RateLimiter`] plugs into `vl-net` as [`RequestHooks`]: before each network attempt it waits
//! until sending is allowed, after each response it learns from the `X-Ratelimit-*` headers. Two
//! rules apply together:
//!
//! - a window budget: at most [`RateConfig::budget`] requests in any [`RateConfig::window`],
//!   below the server's published limit;
//! - the server's word: when it reports no requests remaining, or answers `429`, nothing is sent
//!   until its reset time (or `Retry-After`) has passed.
//!
//! Time comes from a [`Clock`], so the rules are tested without sleeping.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use vl_net::{CancelToken, Control, NetError, RequestHooks};

/// The longest the limiter waits for one request, whatever the server says.
const MAX_WAIT: Duration = Duration::from_secs(120);

/// Time and sleeping, replaceable in tests.
pub trait Clock: Send + Sync {
    /// A monotonic reading.
    fn now(&self) -> Duration;

    /// Sleeps for `total`, returning early with `Cancelled` when `cancel` is set.
    fn sleep(&self, total: Duration, cancel: &CancelToken) -> Result<(), NetError>;
}

/// The real clock.
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&self, total: Duration, cancel: &CancelToken) -> Result<(), NetError> {
        let slice = Duration::from_millis(20);
        let mut left = total;
        while !left.is_zero() {
            if cancel.is_cancelled() {
                return Err(NetError::Cancelled);
            }
            let step = left.min(slice);
            std::thread::sleep(step);
            left -= step;
        }
        if cancel.is_cancelled() {
            return Err(NetError::Cancelled);
        }
        Ok(())
    }
}

/// How the limiter paces requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateConfig {
    /// The length of the counting window.
    pub window: Duration,
    /// The most requests sent inside one window.
    pub budget: u32,
}

impl Default for RateConfig {
    /// Modrinth allows 300 requests a minute per address; the launcher stays at 250.
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            budget: 250,
        }
    }
}

#[derive(Default)]
struct State {
    /// When the requests inside the window were sent, oldest first.
    sent: VecDeque<Duration>,
    /// The limit the server last reported.
    limit: Option<u32>,
    /// Requests the server said were left, decreased as requests are sent.
    remaining: Option<u32>,
    /// When the server's counter resets.
    reset_at: Option<Duration>,
    /// Nothing is sent before this time (after a `429`).
    blocked_until: Option<Duration>,
}

/// Paces requests to one server.
pub struct RateLimiter {
    clock: std::sync::Arc<dyn Clock>,
    config: RateConfig,
    state: Mutex<State>,
}

impl RateLimiter {
    /// A limiter on the real clock.
    pub fn new(config: RateConfig) -> Self {
        Self::with_clock(config, std::sync::Arc::new(SystemClock::default()))
    }

    /// A limiter on a given clock.
    pub fn with_clock(config: RateConfig, clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            clock,
            config,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How long to wait before sending now; zero means a slot was taken.
    fn take_slot(&self) -> Duration {
        let now = self.clock.now();
        let mut state = self.lock();
        while state
            .sent
            .front()
            .is_some_and(|&t| now.saturating_sub(t) >= self.config.window)
        {
            state.sent.pop_front();
        }
        if state.reset_at.is_some_and(|t| t <= now) {
            state.reset_at = None;
            state.remaining = None;
        }
        if state.blocked_until.is_some_and(|t| t <= now) {
            state.blocked_until = None;
        }
        let mut wait = Duration::ZERO;
        if let Some(until) = state.blocked_until {
            wait = wait.max(until.saturating_sub(now));
        }
        if state.remaining == Some(0)
            && let Some(reset) = state.reset_at
        {
            wait = wait.max(reset.saturating_sub(now));
        }
        let budget = match state.limit {
            // Leave a sixth of the server's limit to other users of the same address.
            Some(limit) => self.config.budget.min(limit - limit / 6),
            None => self.config.budget,
        }
        .max(1) as usize;
        if state.sent.len() >= budget
            && let Some(&oldest) = state.sent.front()
        {
            wait = wait.max((oldest + self.config.window).saturating_sub(now));
        }
        if wait.is_zero() {
            state.sent.push_back(now);
            state.remaining = state.remaining.map(|r| r.saturating_sub(1));
        }
        wait.min(MAX_WAIT)
    }
}

impl RequestHooks for RateLimiter {
    fn before_request(&self, control: &Control) -> Result<(), NetError> {
        loop {
            control.cancel.check_cancelled()?;
            let wait = self.take_slot();
            if wait.is_zero() {
                return Ok(());
            }
            self.clock.sleep(wait, &control.cancel)?;
        }
    }

    fn after_response(&self, status: u16, headers: &[(String, String)]) {
        let number = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| v.trim().parse::<u64>().ok())
        };
        let now = self.clock.now();
        let reset = number("x-ratelimit-reset").map(|s| Duration::from_secs(s).min(MAX_WAIT));
        let mut state = self.lock();
        if let Some(limit) = number("x-ratelimit-limit") {
            state.limit = u32::try_from(limit).ok().filter(|&l| l > 0);
        }
        if let Some(remaining) = number("x-ratelimit-remaining") {
            state.remaining = Some(u32::try_from(remaining).unwrap_or(u32::MAX));
            if let Some(reset) = reset {
                state.reset_at = Some(now + reset);
            }
        }
        if status == 429 {
            let retry_after = number("retry-after").map(|s| Duration::from_secs(s).min(MAX_WAIT));
            let pause = reset
                .into_iter()
                .chain(retry_after)
                .max()
                .unwrap_or(Duration::from_secs(10))
                .max(Duration::from_secs(1));
            state.blocked_until = Some(now + pause);
            state.remaining = Some(0);
            state.reset_at = Some(now + pause);
        }
    }
}

trait CheckCancelled {
    fn check_cancelled(&self) -> Result<(), NetError>;
}

impl CheckCancelled for CancelToken {
    fn check_cancelled(&self) -> Result<(), NetError> {
        if self.is_cancelled() {
            Err(NetError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A clock that sleeps by moving its own time; shared with the tests of this crate.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Default)]
    pub(crate) struct FakeClock {
        millis: AtomicU64,
        slept: Mutex<Vec<Duration>>,
    }

    impl FakeClock {
        pub(crate) fn advance(&self, by: Duration) {
            self.millis
                .fetch_add(by.as_millis() as u64, Ordering::SeqCst);
        }

        pub(crate) fn slept(&self) -> Vec<Duration> {
            self.slept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.millis.load(Ordering::SeqCst))
        }

        fn sleep(&self, total: Duration, cancel: &CancelToken) -> Result<(), NetError> {
            if cancel.is_cancelled() {
                return Err(NetError::Cancelled);
            }
            self.slept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(total);
            self.advance(total);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::fake::FakeClock;
    use super::*;

    fn limiter(budget: u32) -> (RateLimiter, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::default());
        let config = RateConfig {
            window: Duration::from_secs(60),
            budget,
        };
        (RateLimiter::with_clock(config, clock.clone()), clock)
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_budget_waits_for_the_window_to_move() {
        let (limiter, clock) = limiter(3);
        let control = Control::default();
        for _ in 0..3 {
            clock.advance(Duration::from_secs(1));
            limiter.before_request(&control).unwrap();
        }
        assert!(clock.slept().is_empty());
        // The fourth request waits until the first one leaves the window (at 1s + 60s).
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept(), [Duration::from_secs(58)]);
        assert_eq!(clock.now(), Duration::from_secs(61));
    }

    #[test]
    fn no_remaining_requests_wait_for_the_reset() {
        let (limiter, clock) = limiter(250);
        let control = Control::default();
        limiter.before_request(&control).unwrap();
        limiter.after_response(
            200,
            &headers(&[
                ("x-ratelimit-limit", "300"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "7"),
            ]),
        );
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept(), [Duration::from_secs(7)]);
        // After the reset the counter is forgotten and nothing waits.
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept().len(), 1);
    }

    #[test]
    fn a_429_blocks_until_the_server_allows_again() {
        let (limiter, clock) = limiter(250);
        let control = Control::default();
        limiter.before_request(&control).unwrap();
        limiter.after_response(429, &headers(&[("x-ratelimit-reset", "5")]));
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept(), [Duration::from_secs(5)]);

        // Without any header the pause is ten seconds; `Retry-After` is honoured too.
        limiter.after_response(429, &[]);
        limiter.before_request(&control).unwrap();
        limiter.after_response(429, &headers(&[("retry-after", "30")]));
        limiter.before_request(&control).unwrap();
        let slept = clock.slept();
        assert_eq!(slept[1], Duration::from_secs(10));
        assert_eq!(slept[2], Duration::from_secs(30));
    }

    #[test]
    fn waits_are_capped_and_cancellable() {
        let (limiter, clock) = limiter(250);
        let control = Control::default();
        limiter.after_response(
            200,
            &headers(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "99999"),
            ]),
        );
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept(), [MAX_WAIT]);

        limiter.after_response(429, &[]);
        control.cancel.cancel();
        assert_eq!(
            limiter.before_request(&control).err(),
            Some(NetError::Cancelled)
        );
    }

    #[test]
    fn the_servers_limit_lowers_the_budget() {
        let (limiter, clock) = limiter(250);
        let control = Control::default();
        limiter.after_response(200, &headers(&[("x-ratelimit-limit", "6")]));
        // 6 - 6/6 = 5 requests per window.
        for _ in 0..5 {
            limiter.before_request(&control).unwrap();
        }
        assert!(clock.slept().is_empty());
        limiter.before_request(&control).unwrap();
        assert_eq!(clock.slept(), [Duration::from_secs(60)]);
    }
}
