//! Game metadata: upstream documents in, normalized launcher types out.
//!
//! Pure functions over bytes: no network, no files, no clock. Fetching and caching belong to the
//! native layer; this crate parses what was fetched (hostile input: sizes are capped, nothing is
//! indexed without a check) and normalizes it into the launcher's own types, which the native
//! layer caches on disk. Platform rules take a `vl_core::platform::Platform` argument, so every
//! OS and architecture is testable on any host.
//!
//! Each upstream document has its own module; tests use small committed fixtures.

pub mod arguments;
pub mod assets;
pub mod libraries;
pub mod manifest;
pub mod rules;
pub mod version;
