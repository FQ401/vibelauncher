//! Why an update check was refused.

use std::fmt;

use vl_core::{Error, ErrorKind};

use crate::Version;

/// A refused manifest, signature or key. Every variant fails closed: nothing from a refused
/// manifest is used.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateError {
    /// This build has no release public key, so no manifest can be trusted.
    NoReleaseKey,
    /// The public key is not a minisign Ed25519 public key.
    BadKey,
    /// The manifest or its signature is larger than allowed.
    TooLarge {
        /// What was too large (`manifest`, `signature`).
        what: &'static str,
        /// The limit in bytes.
        limit: usize,
    },
    /// The signature is malformed, made with another key, or does not match the bytes.
    BadSignature(String),
    /// The signed manifest is not a valid manifest.
    BadManifest(String),
    /// The manifest uses a format this launcher does not know.
    UnsupportedSchema(u32),
    /// The manifest offers a version older than the running one or than a manifest seen before.
    Rollback {
        /// The version the manifest offers.
        offered: Version,
        /// The oldest version still acceptable.
        floor: Version,
    },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoReleaseKey => f.write_str(
                "this build has no release public key; updates cannot be verified and are off",
            ),
            Self::BadKey => f.write_str("the release public key is not a valid minisign key"),
            Self::TooLarge { what, limit } => {
                write!(f, "the update {what} exceeds the limit of {limit} bytes")
            }
            Self::BadSignature(m) => write!(f, "the update manifest signature is invalid: {m}"),
            Self::BadManifest(m) => write!(f, "the update manifest is invalid: {m}"),
            Self::UnsupportedSchema(n) => {
                write!(f, "the update manifest format {n} is not supported")
            }
            Self::Rollback { offered, floor } => write!(
                f,
                "the update manifest offers {offered}, older than {floor}; refusing a rollback"
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<UpdateError> for Error {
    fn from(e: UpdateError) -> Self {
        let kind = match e {
            UpdateError::NoReleaseKey => ErrorKind::Unsupported,
            UpdateError::BadKey => ErrorKind::Internal,
            _ => ErrorKind::Integrity,
        };
        Error::new(kind, e.to_string())
    }
}
