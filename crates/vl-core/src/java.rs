//! Java runtimes the launcher found or manages.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::platform::Arch;

/// A checked Java runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaRuntime {
    /// The `java` executable.
    pub path: PathBuf,
    /// The major version (8, 17, 21…).
    pub major: u32,
    /// The full version (`java.version`).
    pub version: String,
    /// The architecture (`os.arch`).
    pub arch: Arch,
    /// The vendor (`java.vendor`).
    pub vendor: String,
    /// Whether the launcher downloaded and manages this runtime.
    pub managed: bool,
}

/// Java commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum JavaCommand {
    /// The runtimes from the last scan → `Response::Javas`.
    List,
    /// Scans and checks runtimes again → `Response::Task`.
    Rescan,
    /// Makes a launcher-managed runtime of this major version available: downloads it unless one
    /// is already installed → `Response::Task` (kind `JavaDownload`, cancellable); the list
    /// changes when it succeeds.
    Install {
        /// The Java major version (8, 17, 21...).
        major: u32,
    },
}

/// Changes to the runtime list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum JavaEvent {
    /// The list changed; list it again.
    ListChanged,
}
