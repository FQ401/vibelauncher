//! Settings on disk: the global settings file and each instance's overrides.
//!
//! The settings model, its defaults and the override rules live in `vl_core::settings`; this crate
//! reads and writes them as TOML with atomic writes, checks ranges, and falls back to defaults with
//! a warning when a file is damaged.
//!
//! # Layout
//!
//! Under `<data>/settings/`:
//!
//! - `global.toml` — the global settings;
//! - `instances/<name>.toml` — one instance's overrides (`<name>` is the instance id, with unsafe
//!   characters percent-encoded).
//!
//! Both files start with `schema_version`, followed by one table per group (`[java]`, `[memory]`,
//! `[window]`, `[console]`, `[commands]`, `[env]`, `[performance]`, `[natives]`, `[updates]`). A
//! missing key takes its default. In an instance file a table present means "this group is overridden"; an
//! absent table inherits the global group.
//!
//! # Robustness
//!
//! - Unknown keys and groups are kept through a load and save.
//! - A file that cannot be parsed gives defaults; a group that is malformed or out of range is
//!   replaced by its default (in an instance file: dropped, so it inherits). Each case produces a
//!   warning, and the original file is copied to `<name>.toml.bad` before anything overwrites it.
//! - A file written by a newer schema is read as far as it is understood and written back with its
//!   schema version unchanged.

mod codec;
mod store;

pub use store::{InstanceSettings, Loaded, SettingsStore};
