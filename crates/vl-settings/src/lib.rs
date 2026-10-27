//! Settings on disk: the global settings file and each instance's overrides.
//!
//! The settings model, its defaults and the override rules live in `vl_core::settings`; this crate
//! reads and writes them as TOML with atomic writes, checks ranges, and falls back to defaults with
//! a warning when a file is damaged.
