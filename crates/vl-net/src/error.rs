//! The error type of this crate and its mapping to the facade error.

use std::fmt;

use vl_core::{Error, ErrorKind};

/// What went wrong with a network operation. Messages never contain credentials or query strings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetError {
    /// The URL is not an absolute `http` / `https` URL.
    InvalidUrl(String),
    /// A connect, request or read timeout.
    Timeout(String),
    /// The connection could not be made or broke.
    Connect(String),
    /// The server answered with an unusable status.
    Status {
        /// The HTTP status code.
        status: u16,
        /// The redacted URL.
        url: String,
    },
    /// The response is larger than the allowed size.
    TooLarge {
        /// The cap in bytes.
        limit: u64,
        /// The redacted URL.
        url: String,
    },
    /// The size of the data differs from the expected size.
    SizeMismatch {
        /// Expected bytes.
        expected: u64,
        /// Received bytes.
        actual: u64,
    },
    /// The hash of the data differs from the expected hash.
    HashMismatch {
        /// The algorithm name.
        algorithm: &'static str,
        /// Expected lowercase hex digest.
        expected: String,
        /// Computed lowercase hex digest.
        actual: String,
    },
    /// The body is not valid for the requested type (for example, not JSON).
    Decode(String),
    /// Offline mode and nothing usable in the cache.
    Offline(String),
    /// The operation was cancelled.
    Cancelled,
    /// A local file-system failure.
    Io(String),
}

impl NetError {
    /// Whether trying the same request again may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout(_) | Self::Connect(_) | Self::HashMismatch { .. } => true,
            Self::Status { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// Whether the server was not reached, the transfer broke, or the server failed.
    pub(crate) fn is_transport(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Connect(_))
            || matches!(self, Self::Status { status, .. } if *status >= 500)
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(m) => write!(f, "invalid url: {m}"),
            Self::Timeout(m) => write!(f, "timed out: {m}"),
            Self::Connect(m) => write!(f, "connection failed: {m}"),
            Self::Status { status, url } => write!(f, "http status {status} for {url}"),
            Self::TooLarge { limit, url } => {
                write!(f, "response from {url} exceeds the limit of {limit} bytes")
            }
            Self::SizeMismatch { expected, actual } => {
                write!(f, "size mismatch: expected {expected} bytes, got {actual}")
            }
            Self::HashMismatch {
                algorithm,
                expected,
                actual,
            } => write!(f, "{algorithm} mismatch: expected {expected}, got {actual}"),
            Self::Decode(m) => write!(f, "cannot decode response: {m}"),
            Self::Offline(url) => write!(f, "offline and {url} is not cached"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<NetError> for Error {
    fn from(e: NetError) -> Self {
        let kind = match &e {
            NetError::InvalidUrl(_) => ErrorKind::InvalidInput,
            NetError::Status { status: 404, .. } => ErrorKind::NotFound,
            NetError::Timeout(_)
            | NetError::Connect(_)
            | NetError::Status { .. }
            | NetError::Offline(_) => ErrorKind::Network,
            NetError::TooLarge { .. }
            | NetError::SizeMismatch { .. }
            | NetError::HashMismatch { .. }
            | NetError::Decode(_) => ErrorKind::Integrity,
            NetError::Cancelled => ErrorKind::Cancelled,
            NetError::Io(_) => ErrorKind::Io,
        };
        Error::new(kind, e.to_string())
    }
}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Removes user info, query and fragment from a URL so it is safe to log.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), url),
    };
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let rest = &rest[..end];
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}{host}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_and_query() {
        assert_eq!(
            redact_url("https://user:pw@example.com:8443/a/b?token=x#f"),
            "https://example.com:8443/a/b"
        );
        assert_eq!(redact_url("http://h/p"), "http://h/p");
    }

    #[test]
    fn maps_to_facade_kinds() {
        let e: Error = NetError::HashMismatch {
            algorithm: "sha1",
            expected: "a".into(),
            actual: "b".into(),
        }
        .into();
        assert_eq!(e.kind, ErrorKind::Integrity);
        let e: Error = NetError::Status {
            status: 404,
            url: "u".into(),
        }
        .into();
        assert_eq!(e.kind, ErrorKind::NotFound);
        assert_eq!(Error::from(NetError::Cancelled).kind, ErrorKind::Cancelled);
    }

    #[test]
    fn retry_classification() {
        let s = |status| NetError::Status {
            status,
            url: String::new(),
        };
        assert!(s(503).is_retryable() && s(429).is_retryable());
        assert!(!s(404).is_retryable() && !s(403).is_retryable());
        assert!(!NetError::Cancelled.is_retryable());
    }
}
