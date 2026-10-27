//! Expected content checks and streaming hashing.

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::error::NetError;

/// A digest the data must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// SHA-1, 40 hex digits.
    Sha1(String),
    /// SHA-256, 64 hex digits.
    Sha256(String),
}

impl Checksum {
    /// A SHA-1 checksum from hex (any case); `None` when malformed.
    pub fn sha1(hex: &str) -> Option<Self> {
        valid_hex(hex, 40).then(|| Self::Sha1(hex.to_ascii_lowercase()))
    }

    /// A SHA-256 checksum from hex (any case); `None` when malformed.
    pub fn sha256(hex: &str) -> Option<Self> {
        valid_hex(hex, 64).then(|| Self::Sha256(hex.to_ascii_lowercase()))
    }

    fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha1(_) => "sha1",
            Self::Sha256(_) => "sha256",
        }
    }

    fn expected(&self) -> &str {
        match self {
            Self::Sha1(h) | Self::Sha256(h) => h,
        }
    }
}

fn valid_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Incremental hasher for an optional [`Checksum`].
pub(crate) enum Hasher {
    None,
    Sha1(Box<Sha1>),
    Sha256(Box<Sha256>),
}

impl Hasher {
    pub(crate) fn new(checksum: Option<&Checksum>) -> Self {
        match checksum {
            None => Self::None,
            Some(Checksum::Sha1(_)) => Self::Sha1(Box::new(Sha1::new())),
            Some(Checksum::Sha256(_)) => Self::Sha256(Box::new(Sha256::new())),
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            Self::None => {}
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
        }
    }

    pub(crate) fn verify(self, checksum: Option<&Checksum>) -> Result<(), NetError> {
        let Some(checksum) = checksum else {
            return Ok(());
        };
        let actual = match self {
            Self::None => return Ok(()),
            Self::Sha1(h) => to_hex(&h.finalize()),
            Self::Sha256(h) => to_hex(&h.finalize()),
        };
        if actual == checksum.expected() {
            Ok(())
        } else {
            Err(NetError::HashMismatch {
                algorithm: checksum.algorithm(),
                expected: checksum.expected().to_string(),
                actual,
            })
        }
    }
}

/// Hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let c = Checksum::sha1("A9993E364706816ABA3E25717850C26C9CD0D89D");
        let mut h = Hasher::new(c.as_ref());
        h.update(b"ab");
        h.update(b"c");
        assert!(h.verify(c.as_ref()).is_ok());
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!(Checksum::sha1("zz").is_none());
        assert!(Checksum::sha256(&"a".repeat(63)).is_none());
    }

    #[test]
    fn mismatch_is_reported() {
        let c = Checksum::sha256(&"0".repeat(64));
        let mut h = Hasher::new(c.as_ref());
        h.update(b"x");
        assert!(matches!(
            h.verify(c.as_ref()),
            Err(NetError::HashMismatch { .. })
        ));
    }
}
