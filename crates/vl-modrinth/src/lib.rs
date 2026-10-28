//! Modrinth API v2 as pure functions: request URLs and bodies out, catalog types from response
//! bytes.
//!
//! Nothing here touches the network. The native layer (`vl-platforms`) sends the requests; this
//! crate builds them ([`Endpoints`]) and parses what comes back ([`parse`]). Responses are hostile
//! input: their size is checked before parsing, unknown fields are ignored, a field of the wrong
//! type reads as missing, one damaged item does not spoil a list, and every string and list is cut
//! to a limit. File names that could leave their directory and download URLs that are not `https`
//! are dropped.

mod endpoints;
pub mod parse;

pub use endpoints::{DEFAULT_BASE, Endpoints, MAX_HASHES_PER_REQUEST};
