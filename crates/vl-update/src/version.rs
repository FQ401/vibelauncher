//! Launcher versions: `MAJOR.MINOR.PATCH[-pre]` ordered as in Semantic Versioning.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::UpdateError;

/// The longest version string accepted.
const MAX_LEN: usize = 64;

/// A launcher version. Build metadata (`+…`) and leading zeros are not accepted; a version with a
/// pre-release part (`1.2.0-beta.1`) orders before the release (`1.2.0`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Version {
    /// `MAJOR.MINOR.PATCH` without a pre-release part.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    /// Parses a version; `None` when it is not `MAJOR.MINOR.PATCH[-pre]`.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > MAX_LEN {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre.split('.').map(ident).collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// Whether the version has a pre-release part.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

/// A numeric part: ASCII digits, no leading zero unless it is `0`.
fn number(s: &str) -> Option<u64> {
    let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

fn ident(s: &str) -> Option<Ident> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        number(s).map(Ident::Num)
    } else {
        Some(Ident::Text(s.to_owned()))
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, part) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match part {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Text(t) => f.write_str(t)?,
            }
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = UpdateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| UpdateError::BadManifest(format!("invalid version {s:?}")))
    }
}

impl TryFrom<String> for Version {
    type Error = UpdateError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Version> for String {
    fn from(v: Version) -> Self {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_and_prints() {
        for s in ["0.0.0", "1.2.3", "10.20.30", "1.0.0-beta.2", "1.0.0-rc-1.x"] {
            assert_eq!(v(s).to_string(), s);
        }
        assert!(v("1.0.0-alpha").is_prerelease());
        assert!(!v("1.0.0").is_prerelease());
    }

    #[test]
    fn rejects_malformed() {
        for s in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-",
            "1.2.3-beta..1",
            "1.2.3-01",
            "1.2.3+build",
            "v1.2.3",
            " 1.2.3",
            "1.2.x",
            "99999999999999999999.0.0",
        ] {
            assert_eq!(Version::parse(s), None, "{s:?}");
        }
        assert_eq!(Version::parse(&"1".repeat(MAX_LEN + 1)), None);
    }

    #[test]
    fn orders_like_semver() {
        let ordered = [
            "0.9.9",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.2.3").cmp(&v("1.2.3")), Ordering::Equal);
    }

    #[test]
    fn serde_round_trip() {
        let json = serde_json::to_string(&v("1.2.3-rc.1")).unwrap();
        assert_eq!(json, "\"1.2.3-rc.1\"");
        let back: Version = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v("1.2.3-rc.1"));
        assert!(serde_json::from_str::<Version>("\"1.2\"").is_err());
    }
}
