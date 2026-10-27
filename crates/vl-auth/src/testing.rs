//! Test doubles: scripted HTTP answers and a fake clock. Every value is synthetic.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use vl_net::{CancelToken, NetError};

use crate::error::AuthError;
use crate::flow::Clock;
use crate::http::{AuthHttp, AuthRequest, AuthResponse, Body};

struct Route {
    url_part: String,
    answers: VecDeque<AuthResponse>,
}

/// An [`AuthHttp`] that answers from a script and records what it was asked. The last answer of
/// a route repeats. A request with no matching route fails like a dead connection.
#[derive(Default)]
pub(crate) struct Scripted {
    routes: Mutex<Vec<Route>>,
    seen: Mutex<Vec<AuthRequest>>,
}

impl Scripted {
    /// Answers requests whose URL contains `url_part`; a later route for the same part replaces
    /// the earlier one.
    pub fn route(&self, url_part: &str, answers: impl IntoIterator<Item = AuthResponse>) {
        let mut routes = self.routes.lock().unwrap();
        routes.retain(|r| r.url_part != url_part);
        routes.push(Route {
            url_part: url_part.to_owned(),
            answers: answers.into_iter().collect(),
        });
    }

    pub fn urls(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect()
    }

    /// The request bodies as text (form fields percent-encoded).
    pub fn bodies(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| match &r.body {
                Body::None => String::new(),
                Body::Json(t) => t.clone(),
                Body::Form(f) => crate::http::form_encode(f),
            })
            .collect()
    }
}

impl AuthHttp for Scripted {
    fn send(&self, request: &AuthRequest) -> Result<AuthResponse, NetError> {
        self.seen.lock().unwrap().push(request.clone());
        let mut routes = self.routes.lock().unwrap();
        let route = routes
            .iter_mut()
            .find(|r| request.url.contains(&r.url_part))
            .ok_or_else(|| NetError::Connect("no route".into()))?;
        if route.answers.len() > 1 {
            Ok(route.answers.pop_front().unwrap())
        } else {
            route
                .answers
                .front()
                .cloned()
                .ok_or_else(|| NetError::Connect("empty route".into()))
        }
    }
}

pub(crate) fn json(status: u16, body: &str) -> AuthResponse {
    AuthResponse {
        status,
        body: body.as_bytes().to_vec(),
    }
}

pub(crate) fn status(status: u16) -> AuthResponse {
    json(status, "")
}

/// A clock that moves only when the flow sleeps.
pub(crate) struct FakeClock {
    now: Mutex<u64>,
    slept: Mutex<Vec<u64>>,
}

impl FakeClock {
    pub fn new(now: u64) -> Self {
        Self {
            now: Mutex::new(now),
            slept: Mutex::new(Vec::new()),
        }
    }

    pub fn sleeps(&self) -> Vec<u64> {
        self.slept.lock().unwrap().clone()
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration, cancel: &CancelToken) -> Result<(), AuthError> {
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        *self.now.lock().unwrap() += duration.as_secs();
        self.slept.lock().unwrap().push(duration.as_secs());
        Ok(())
    }
}
