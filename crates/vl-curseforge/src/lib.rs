//! CurseForge API v1 as pure functions: request URLs and bodies out, catalog types from response
//! bytes, and the CurseForge file fingerprint.
//!
//! Nothing here touches the network. The native layer (`vl-platforms`) sends the requests with
//! the user's [`ApiKey`] in the [`API_KEY_HEADER`] header; this crate builds them ([`Endpoints`])
//! and parses what comes back ([`parse`]). Responses are hostile input: their size is checked
//! before parsing, unknown fields are ignored, a field of the wrong type reads as missing, one
//! damaged item does not spoil a list, and every string and list is cut to a limit. File names
//! that could leave their directory and download URLs that are not `https` are dropped.
//!
//! [`manifest`] reads `manifest.json`, the document of a CurseForge modpack zip.
//!
//! The launcher ships no CurseForge key: the user enters their own, and [`no_api_key`] is the
//! error when there is none.

mod endpoints;
mod fingerprint;
mod key;
mod loader;
pub mod manifest;
pub mod parse;

pub use endpoints::{
    DEFAULT_BASE, Endpoints, GAME_ID, MAX_CATEGORY_IDS, MAX_FINGERPRINTS_PER_REQUEST,
    MAX_IDS_PER_REQUEST, MAX_PAGE_SIZE, MAX_RESULT_WINDOW, Page, class_id, numeric_id,
    project_type, unsent_filter,
};
pub use fingerprint::{Fingerprinter, fingerprint, fingerprint_len, murmur2};
pub use key::{API_KEY_ENV, API_KEY_HEADER, ApiKey, MAX_KEY_LEN, no_api_key, rejected_api_key};
pub use loader::ModLoader;
pub use manifest::{Manifest, ManifestFile, ManifestLoader, parse_manifest};
pub use parse::{
    Category, FingerprintMatch, ModFile, ModSummary, download_url, edge_url, project_page_url,
};
