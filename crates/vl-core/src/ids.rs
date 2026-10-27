//! Identifiers shared by commands, responses and events.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier; surrounding whitespace is trimmed, empty input is rejected.
            pub fn new(value: &str) -> Option<Self> {
                let value = value.trim();
                (!value.is_empty()).then(|| Self(value.to_owned()))
            }

            /// The identifier as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(&value).ok_or_else(|| format!("empty {}", stringify!($name)))
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(&self.0)
            }
        }
    };
}

string_id!(
    /// Identifier of an instance: the name of its directory under the instances root.
    InstanceId
);

string_id!(
    /// Identifier of an account, stable across renames (the profile UUID for Microsoft accounts).
    AccountId
);

/// Identifier of a running or finished task, unique within one backend session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskId(pub u64);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_rejects_empty_ids() {
        assert_eq!(InstanceId::new("  main ").unwrap().as_str(), "main");
        assert!(InstanceId::new("   ").is_none());
        assert!(AccountId::new("").is_none());
    }

    #[test]
    fn deserializing_validates_ids() {
        let id: InstanceId = serde_json::from_str("\" survival \"").unwrap();
        assert_eq!(id.as_str(), "survival");
        assert!(serde_json::from_str::<InstanceId>("\"  \"").is_err());
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"survival\"");
    }
}
