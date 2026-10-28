//! The error every facade call and every failed task reports.
//!
//! Native crates keep their own detailed error types and convert them into [`Error`] at the
//! backend boundary. The [`ErrorKind`] is what front ends act on (and what the UI localizes); the
//! message is English technical detail for logs and "details" views. Messages never contain
//! secrets (tokens, keys) or personal data beyond what the user typed.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What went wrong, in terms a front end can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorKind {
    /// The command's arguments are invalid (empty name, unknown version id, out-of-range value).
    InvalidInput,
    /// The instance, account, task or version does not exist.
    NotFound,
    /// The command conflicts with the current state (the instance is running, the name is taken).
    Conflict,
    /// A network request failed or timed out; retrying later may help.
    Network,
    /// A file-system operation failed (permissions, full disk, damaged file).
    Io,
    /// Downloaded or stored data failed a hash, size or format check.
    Integrity,
    /// Signing in failed or the account needs to sign in again; also a platform that refused
    /// the user's API key.
    Auth,
    /// A platform needs an API key and none is set (CurseForge: the user enters their own key).
    NoApiKey,
    /// No suitable Java runtime, or the runtime failed its check.
    Java,
    /// The game process failed to start or exited abnormally.
    Game,
    /// The operation was cancelled by the user.
    Cancelled,
    /// This backend does not implement the command.
    Unsupported,
    /// A bug in the launcher: an invariant did not hold.
    Internal,
}

/// A failure reported through the facade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// What went wrong.
    pub kind: ErrorKind,
    /// English technical detail, safe to log.
    pub message: String,
}

/// `Result` with the facade's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// An error of `kind` with a technical `message`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// [`ErrorKind::InvalidInput`].
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    /// [`ErrorKind::NotFound`].
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    /// [`ErrorKind::Conflict`].
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    /// [`ErrorKind::Unsupported`].
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    /// [`ErrorKind::Internal`].
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// [`ErrorKind::Cancelled`].
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "cancelled")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}
