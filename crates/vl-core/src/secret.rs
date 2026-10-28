//! Secret text carried in a command, such as an API key the user enters.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Text that must never be shown: its `Debug` output is masked and it has no `Display`, so a
/// command that carries it can be logged or put in an error message without leaking it. Read it
/// with [`expose`](Self::expose) only where it is stored or sent.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wraps `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text; call only to store or send it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_masked_and_serde_is_plain() {
        let secret = Secret::new("synthetic-value-123");
        assert!(!format!("{secret:?}").contains("synthetic"));
        assert_eq!(secret.expose(), "synthetic-value-123");
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"synthetic-value-123\"");
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back, secret);
    }
}
