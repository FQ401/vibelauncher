//! The store's error type.

use std::fmt;
use std::path::{Path, PathBuf};

/// What can go wrong while locating, locking, migrating or writing the data directory.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// A file-system operation failed.
    Io {
        /// The path involved.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// Another launcher process uses this data directory.
    Locked {
        /// The data directory.
        dir: PathBuf,
    },
    /// The directories cannot be determined (no home directory, unusable `--dir`, ...).
    NoDirectory(String),
    /// The data was written by a newer launcher.
    FormatTooNew {
        /// The version on disk.
        found: u32,
        /// The newest version this launcher knows.
        supported: u32,
    },
    /// The format file or the migration chain is damaged.
    Corrupt {
        /// The path involved.
        path: PathBuf,
        /// What is wrong.
        detail: String,
    },
    /// A migration step failed; the data was left at the version before the step.
    Migration {
        /// The version the step started from.
        from: u32,
        /// The snapshot taken before the step, if one was taken.
        snapshot: Option<PathBuf>,
        /// The cause.
        detail: String,
    },
    /// The operation was cancelled; nothing was changed.
    Cancelled,
    /// The request cannot be carried out as given.
    InvalidInput(String),
    /// The request conflicts with the current state.
    Conflict(String),
}

impl StoreError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn corrupt(path: &Path, detail: impl Into<String>) -> Self {
        Self::Corrupt {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Locked { dir } => write!(
                f,
                "data directory {} is used by another launcher process",
                dir.display()
            ),
            Self::NoDirectory(why) => write!(f, "cannot determine the data directory: {why}"),
            Self::FormatTooNew { found, supported } => write!(
                f,
                "data format {found} is newer than the supported format {supported}"
            ),
            Self::Corrupt { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::Migration {
                from,
                snapshot,
                detail,
            } => {
                write!(f, "migration from format {from} failed: {detail}")?;
                if let Some(s) = snapshot {
                    write!(f, " (snapshot: {})", s.display())?;
                }
                Ok(())
            }
            Self::Cancelled => f.write_str("cancelled"),
            Self::InvalidInput(why) | Self::Conflict(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<StoreError> for vl_core::Error {
    fn from(e: StoreError) -> Self {
        use vl_core::ErrorKind;
        let kind = match &e {
            StoreError::Io { .. } | StoreError::NoDirectory(_) | StoreError::Migration { .. } => {
                ErrorKind::Io
            }
            StoreError::Locked { .. } => ErrorKind::Conflict,
            StoreError::FormatTooNew { .. } => ErrorKind::Unsupported,
            StoreError::Corrupt { .. } => ErrorKind::Integrity,
            StoreError::Cancelled => ErrorKind::Cancelled,
            StoreError::InvalidInput(_) => ErrorKind::InvalidInput,
            StoreError::Conflict(_) => ErrorKind::Conflict,
        };
        vl_core::Error::new(kind, e.to_string())
    }
}
