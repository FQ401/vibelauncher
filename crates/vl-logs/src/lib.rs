//! The launcher's own log files, and sharing a game log on mclo.gs.
//!
//! - [`LauncherLog`]: structured records (time, level, target, message, `key=value` fields) in
//!   `launcher.log` of the logs section, rotated by size and count, every record masked with a
//!   [`Redactor`](vl_core::redact::Redactor) before it is written.
//! - [`mclogs`]: a game log prepared for upload (masked, cut to the service's limits) and the
//!   upload itself through `vl-net`.
//!
//! The game's own output files are written by `vl-launch`; this crate only reads them.

mod file;
pub mod mclogs;
mod time;

pub use file::{FILE_NAME, LauncherLog, MAX_FILE_BYTES, MAX_FILES, Rotation, log_panics};
