//! Network access for the launcher; everything that comes from the network is hostile.
//!
//! - A download queue with a parallelism limit, retries with exponential backoff, connect and
//!   read timeouts.
//! - Size caps on every response, SHA-1 / SHA-256 and size checks, writes to a temporary file and
//!   an atomic rename.
//! - An HTTP cache (`ETag` / `Last-Modified`) for metadata, and offline reads from that cache.
//! - System, HTTP and SOCKS5 proxies; the launcher's own `User-Agent` without personal details.
//! - Progress and cancellation through `vl-tasks`.
