//! The error type of this crate and its mapping to the facade error.

use std::fmt;
use std::path::Path;

use vl_core::{Error, ErrorKind};
use vl_net::NetError;
use vl_update::UpdateError;

/// What went wrong while checking for, downloading or installing an update.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdaterError {
    /// The manifest, its signature or the key was refused.
    Update(UpdateError),
    /// A network operation failed (including size and hash checks of the download).
    Net(NetError),
    /// A file-system operation failed.
    Io(String),
    /// The downloaded file is damaged or unsafe (hash, size, archive layout).
    Integrity(String),
    /// A game is running; the update waits until it exits.
    GameRunning,
    /// This replacement is not implemented on this build yet.
    NotImplemented(&'static str),
    /// The plan does not replace anything (the user installs the release by hand).
    NothingToReplace,
}

impl UpdaterError {
    pub(crate) fn io(path: &Path, e: &std::io::Error) -> Self {
        Self::Io(format!("{}: {e}", path.display()))
    }
}

impl fmt::Display for UpdaterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Update(e) => e.fmt(f),
            Self::Net(e) => e.fmt(f),
            Self::Io(m) => write!(f, "io error: {m}"),
            Self::Integrity(m) => write!(f, "the update file is damaged: {m}"),
            Self::GameRunning => f.write_str("a game is running; the update waits until it exits"),
            Self::NotImplemented(what) => write!(f, "{what} is not implemented yet"),
            Self::NothingToReplace => {
                f.write_str("this install is updated by hand from the release page")
            }
        }
    }
}

impl std::error::Error for UpdaterError {}

impl From<UpdateError> for UpdaterError {
    fn from(e: UpdateError) -> Self {
        Self::Update(e)
    }
}

impl From<NetError> for UpdaterError {
    fn from(e: NetError) -> Self {
        Self::Net(e)
    }
}

impl From<UpdaterError> for Error {
    fn from(e: UpdaterError) -> Self {
        match e {
            UpdaterError::Update(e) => e.into(),
            UpdaterError::Net(e) => e.into(),
            other => {
                let kind = match &other {
                    UpdaterError::Io(_) => ErrorKind::Io,
                    UpdaterError::Integrity(_) => ErrorKind::Integrity,
                    UpdaterError::GameRunning => ErrorKind::Conflict,
                    _ => ErrorKind::Unsupported,
                };
                Error::new(kind, other.to_string())
            }
        }
    }
}
