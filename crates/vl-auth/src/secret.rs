//! Secrets and where they are kept.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::AuthError;

/// A token or other secret. Its `Debug` output is masked and it has no `Display`, so it cannot
/// end up in a log by accident; read it with [`expose`](Self::expose) where it is sent.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value; call only to send it to the service it belongs to.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The tokens kept for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTokens {
    /// The Microsoft refresh token.
    pub refresh_token: Secret,
    /// The Minecraft services access token last obtained.
    pub minecraft_token: Secret,
    /// When the Minecraft token expires (Unix seconds).
    pub minecraft_expires_at: u64,
}

/// Where tokens are kept, by account id. The OS keyring implements this; [`MemoryTokenStore`]
/// keeps them for the life of the process.
pub trait TokenStore: Send + Sync {
    /// The tokens of `account`, if any.
    fn get(&self, account: &str) -> Result<Option<StoredTokens>, AuthError>;
    /// Stores (replaces) the tokens of `account`.
    fn put(&self, account: &str, tokens: StoredTokens) -> Result<(), AuthError>;
    /// Forgets the tokens of `account`; absent tokens are not an error.
    fn remove(&self, account: &str) -> Result<(), AuthError>;
    /// A warning for the user when the store is not keeping tokens safely across restarts
    /// (for instance, the system keyring is unavailable and tokens live in memory only).
    fn notice(&self) -> Option<String> {
        None
    }
}

/// A [`TokenStore`] in memory.
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<String, StoredTokens>>,
}

impl MemoryTokenStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, StoredTokens>> {
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl TokenStore for MemoryTokenStore {
    fn get(&self, account: &str) -> Result<Option<StoredTokens>, AuthError> {
        Ok(self.lock().get(account).cloned())
    }

    fn put(&self, account: &str, tokens: StoredTokens) -> Result<(), AuthError> {
        self.lock().insert(account.to_owned(), tokens);
        Ok(())
    }

    fn remove(&self, account: &str) -> Result<(), AuthError> {
        self.lock().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_value() {
        let t = StoredTokens {
            refresh_token: Secret::new("refresh-value"),
            minecraft_token: Secret::new("mc-value"),
            minecraft_expires_at: 1,
        };
        let shown = format!("{t:?} {:?}", Secret::new("abc"));
        assert!(!shown.contains("refresh-value") && !shown.contains("mc-value"));
        assert!(!shown.contains("abc") && shown.contains("redacted"));
    }

    #[test]
    fn memory_store_round_trips() {
        let s = MemoryTokenStore::new();
        let t = StoredTokens {
            refresh_token: Secret::new("r"),
            minecraft_token: Secret::new("m"),
            minecraft_expires_at: 9,
        };
        assert_eq!(s.get("a").unwrap(), None);
        s.put("a", t.clone()).unwrap();
        assert_eq!(s.get("a").unwrap(), Some(t));
        s.remove("a").unwrap();
        s.remove("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
    }
}
