//! Network access for the launcher; everything that comes from the network is hostile.
//!
//! - [`Client::get_bytes`] / [`Client::get_json`]: metadata requests with an HTTP cache
//!   (`ETag` / `Last-Modified`), offline reads from that cache, retries with exponential backoff,
//!   timeouts and a response size cap.
//! - [`Client::download`] / [`Client::download_all`]: files with a parallelism limit, size and
//!   SHA-1 / SHA-256 checks, a temporary file and an atomic rename.
//! - System, HTTP and SOCKS5 proxies; the launcher's own `User-Agent` without personal details.
//! - Progress and cancellation through a callback and a [`CancelToken`] ([`Control`]); the task
//!   engine wires them, this crate does not depend on it.
//! - [`HttpTransport`] is the seam to the HTTP stack, so other crates test against a fake.
//!
//! The client is blocking: calls run on the caller's thread (batches use scoped worker threads),
//! so callers run them inside a task. There is no async runtime.

mod cache;
mod client;
mod config;
mod control;
mod download;
mod error;
mod hash;
mod transport;

pub use client::{CacheStatus, Client, Fetched, GetOptions};
pub use config::{NetConfig, ProxyConfig, RetryPolicy};
pub use control::{CancelToken, Control, Progress, ProgressFn};
pub use download::{Download, Downloaded};
pub use error::{NetError, redact_url};
pub use hash::{Checksum, sha256_hex};
pub use transport::{HttpRequest, HttpResponse, HttpTransport, PostRequest, UreqTransport};
