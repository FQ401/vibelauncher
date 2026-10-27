//! Where the launcher keeps its files.
//!
//! - Data, cache and log directories by OS convention (Windows `%APPDATA%`, macOS
//!   `Application Support`, Linux XDG), or one portable directory (`--dir <path>` or a marker file
//!   next to the executable).
//! - The on-disk format version and the migration chain, with a snapshot before each migration.
//! - A lock that keeps a second launcher process out of the same data directory.
//! - Atomic file writes (temporary file, then rename) for every other native crate.
//!
//! The directory layout is described in `docs/architecture.md`.
