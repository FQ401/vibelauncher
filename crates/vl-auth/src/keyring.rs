//! Tokens in the operating system's credential store.
//!
//! One keyring entry per account holds the JSON of its [`StoredTokens`]. The backends are the
//! Windows Credential Manager, the macOS Keychain and, elsewhere, the Secret Service over D-Bus
//! (pure Rust, no GTK or libsecret). Without a usable store [`default_token_store`] falls back to
//! memory: tokens are never written to disk in the clear.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use keyring_core::{Entry, Error as KeyError};
use serde::{Deserialize, Serialize};

use crate::error::AuthError;
use crate::secret::{MemoryTokenStore, Secret, StoredTokens, TokenStore};

/// The keyring service name all entries are filed under.
pub const KEYRING_SERVICE: &str = "VibeLauncher";

/// A place that keeps one text secret per key. The OS keyring implements this; tests substitute it.
pub trait SecretVault: Send + Sync {
    /// The text stored under `key`, if any.
    fn get(&self, key: &str) -> Result<Option<String>, AuthError>;
    /// Stores (replaces) the text under `key`.
    fn set(&self, key: &str, value: &str) -> Result<(), AuthError>;
    /// Deletes `key`; an absent key is not an error.
    fn delete(&self, key: &str) -> Result<(), AuthError>;
}

/// The vault on the OS credential store.
pub struct OsVault {
    store: Arc<keyring_core::api::CredentialStore>,
}

impl OsVault {
    /// Opens the credential store of this platform. Fails with
    /// [`AuthError::KeyringUnavailable`] when there is none (for instance no Secret Service on a
    /// headless Linux machine).
    pub fn open() -> Result<Self, AuthError> {
        let store = platform_store().map_err(|e| map_error(&e))?;
        Ok(Self { store })
    }

    fn entry(&self, key: &str) -> Result<Entry, AuthError> {
        self.store
            .build(KEYRING_SERVICE, key, None)
            .map_err(|e| map_error(&e))
    }
}

#[cfg(windows)]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::api::CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(target_vendor = "apple")]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::api::CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::api::CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(not(any(windows, unix)))]
fn platform_store() -> keyring_core::Result<Arc<keyring_core::api::CredentialStore>> {
    Err(KeyError::NotSupportedByStore(
        "no credential store on this platform".into(),
    ))
}

/// Maps a keyring failure without echoing anything the store may have included about the secret.
fn map_error(error: &KeyError) -> AuthError {
    match error {
        KeyError::NoStorageAccess(_) | KeyError::PlatformFailure(_) | KeyError::NoDefaultStore => {
            AuthError::KeyringUnavailable(
                "The system keyring cannot be reached (is a Secret Service such as GNOME \
                 Keyring or KWallet running and unlocked?)."
                    .into(),
            )
        }
        KeyError::NotSupportedByStore(_) => {
            AuthError::KeyringUnavailable("This platform has no supported system keyring.".into())
        }
        _ => AuthError::Store("the system keyring rejected the request".into()),
    }
}

impl SecretVault for OsVault {
    fn get(&self, key: &str) -> Result<Option<String>, AuthError> {
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            // Missing, or not text and so not ours: the user signs in again.
            Err(KeyError::NoEntry | KeyError::BadEncoding(_)) => Ok(None),
            Err(e) => Err(map_error(&e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), AuthError> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| map_error(&e))
    }

    fn delete(&self, key: &str) -> Result<(), AuthError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(KeyError::NoEntry) => Ok(()),
            Err(e) => Err(map_error(&e)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    refresh_token: String,
    minecraft_token: String,
    minecraft_expires_at: u64,
}

/// A [`TokenStore`] that keeps each account's tokens as one entry of a [`SecretVault`].
pub struct KeyringTokenStore {
    vault: Arc<dyn SecretVault>,
}

impl KeyringTokenStore {
    /// A store on `vault`.
    pub fn new(vault: Arc<dyn SecretVault>) -> Self {
        Self { vault }
    }

    fn key(account: &str) -> String {
        format!("account:{account}")
    }
}

impl TokenStore for KeyringTokenStore {
    fn get(&self, account: &str) -> Result<Option<StoredTokens>, AuthError> {
        let Some(text) = self.vault.get(&Self::key(account))? else {
            return Ok(None);
        };
        // An unreadable record is as good as none: the user signs in again.
        Ok(serde_json::from_str::<Record>(&text)
            .ok()
            .map(|r| StoredTokens {
                refresh_token: Secret::new(r.refresh_token),
                minecraft_token: Secret::new(r.minecraft_token),
                minecraft_expires_at: r.minecraft_expires_at,
            }))
    }

    fn put(&self, account: &str, tokens: StoredTokens) -> Result<(), AuthError> {
        let text = serde_json::to_string(&Record {
            refresh_token: tokens.refresh_token.expose().to_owned(),
            minecraft_token: tokens.minecraft_token.expose().to_owned(),
            minecraft_expires_at: tokens.minecraft_expires_at,
        })
        .map_err(|_| AuthError::Store("cannot encode the tokens".into()))?;
        self.vault.set(&Self::key(account), &text)
    }

    fn remove(&self, account: &str) -> Result<(), AuthError> {
        self.vault.delete(&Self::key(account))
    }
}

/// The keyring when it works; otherwise tokens live in memory until the launcher closes and
/// [`notice`](TokenStore::notice) says why. Nothing is ever written to disk in the clear.
pub struct FallbackTokenStore {
    primary: Option<Arc<dyn TokenStore>>,
    memory: MemoryTokenStore,
    degraded: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl FallbackTokenStore {
    /// A store that tries `primary` first.
    pub fn new(primary: Arc<dyn TokenStore>) -> Self {
        Self {
            primary: Some(primary),
            memory: MemoryTokenStore::new(),
            degraded: AtomicBool::new(false),
            reason: Mutex::new(None),
        }
    }

    /// A store that is in memory from the start, because of `reason`.
    pub fn memory_only(reason: &AuthError) -> Self {
        let store = Self {
            primary: None,
            memory: MemoryTokenStore::new(),
            degraded: AtomicBool::new(false),
            reason: Mutex::new(None),
        };
        store.degrade(reason);
        store
    }

    fn degrade(&self, reason: &AuthError) {
        self.degraded.store(true, Ordering::SeqCst);
        *self.reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(match reason {
            AuthError::KeyringUnavailable(m) => m.clone(),
            other => other.to_string(),
        });
    }

    fn primary(&self) -> Option<&Arc<dyn TokenStore>> {
        if self.degraded.load(Ordering::SeqCst) {
            None
        } else {
            self.primary.as_ref()
        }
    }
}

impl TokenStore for FallbackTokenStore {
    fn get(&self, account: &str) -> Result<Option<StoredTokens>, AuthError> {
        if let Some(primary) = self.primary() {
            match primary.get(account) {
                Err(e @ AuthError::KeyringUnavailable(_)) => self.degrade(&e),
                other => return other,
            }
        }
        self.memory.get(account)
    }

    fn put(&self, account: &str, tokens: StoredTokens) -> Result<(), AuthError> {
        if let Some(primary) = self.primary() {
            match primary.put(account, tokens.clone()) {
                Err(e @ AuthError::KeyringUnavailable(_)) => self.degrade(&e),
                other => return other,
            }
        }
        self.memory.put(account, tokens)
    }

    fn remove(&self, account: &str) -> Result<(), AuthError> {
        self.memory.remove(account)?;
        match self.primary() {
            Some(primary) => match primary.remove(account) {
                Err(e @ AuthError::KeyringUnavailable(_)) => {
                    self.degrade(&e);
                    Ok(())
                }
                other => other,
            },
            None => Ok(()),
        }
    }

    fn notice(&self) -> Option<String> {
        if !self.degraded.load(Ordering::SeqCst) {
            return None;
        }
        let reason = self
            .reason
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        Some(format!(
            "{reason} Sign-ins are kept in memory only and are lost when the launcher closes; \
             nothing is written to disk."
        ))
    }
}

/// The token store for this machine: the OS keyring, or memory with a notice when there is none.
pub fn default_token_store() -> Arc<dyn TokenStore> {
    match OsVault::open() {
        Ok(vault) => Arc::new(FallbackTokenStore::new(Arc::new(KeyringTokenStore::new(
            Arc::new(vault),
        )))),
        Err(e) => Arc::new(FallbackTokenStore::memory_only(&e)),
    }
}

/// A [`SecretVault`] in memory, for tests of the layers above it.
#[derive(Default)]
pub struct MemoryVault {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryVault {
    /// An empty vault.
    pub fn new() -> Self {
        Self::default()
    }

    /// The raw text under `key`.
    pub fn raw(&self, key: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }
}

impl SecretVault for MemoryVault {
    fn get(&self, key: &str) -> Result<Option<String>, AuthError> {
        Ok(self.raw(key))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), AuthError> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), AuthError> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(tag: &str) -> StoredTokens {
        StoredTokens {
            refresh_token: Secret::new(format!("refresh-{tag}")),
            minecraft_token: Secret::new(format!("mc-{tag}")),
            minecraft_expires_at: 42,
        }
    }

    /// A vault that always fails like a missing Secret Service.
    struct Dead;

    impl SecretVault for Dead {
        fn get(&self, _: &str) -> Result<Option<String>, AuthError> {
            Err(AuthError::KeyringUnavailable("Dead.".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<(), AuthError> {
            Err(AuthError::KeyringUnavailable("Dead.".into()))
        }
        fn delete(&self, _: &str) -> Result<(), AuthError> {
            Err(AuthError::KeyringUnavailable("Dead.".into()))
        }
    }

    #[test]
    fn round_trips_through_the_vault() {
        let vault = Arc::new(MemoryVault::new());
        let store = KeyringTokenStore::new(vault.clone());
        assert_eq!(store.get("a").unwrap(), None);
        store.put("a", tokens("1")).unwrap();
        store.put("b", tokens("2")).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
        assert!(vault.raw("account:a").is_some());
        store.remove("a").unwrap();
        store.remove("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        assert_eq!(store.get("b").unwrap(), Some(tokens("2")));
    }

    #[test]
    fn a_damaged_record_reads_as_missing() {
        let vault = Arc::new(MemoryVault::new());
        vault.set("account:a", "not json").unwrap();
        let store = KeyringTokenStore::new(vault);
        assert_eq!(store.get("a").unwrap(), None);
    }

    #[test]
    fn fallback_uses_the_keyring_while_it_works() {
        let vault = Arc::new(MemoryVault::new());
        let store = FallbackTokenStore::new(Arc::new(KeyringTokenStore::new(vault.clone())));
        store.put("a", tokens("1")).unwrap();
        assert!(vault.raw("account:a").is_some());
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
        assert_eq!(store.notice(), None);
    }

    #[test]
    fn fallback_keeps_tokens_in_memory_when_the_keyring_dies() {
        let store = FallbackTokenStore::new(Arc::new(KeyringTokenStore::new(Arc::new(Dead))));
        store.put("a", tokens("1")).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
        let notice = store.notice().unwrap();
        assert!(notice.contains("memory only") && notice.contains("nothing is written to disk"));
        store.remove("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
    }

    #[test]
    fn memory_only_store_explains_itself() {
        let store =
            FallbackTokenStore::memory_only(&AuthError::KeyringUnavailable("No bus.".into()));
        store.put("a", tokens("1")).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
        assert!(store.notice().unwrap().starts_with("No bus."));
    }

    #[test]
    fn other_errors_are_not_swallowed() {
        struct Broken;
        impl TokenStore for Broken {
            fn get(&self, _: &str) -> Result<Option<StoredTokens>, AuthError> {
                Err(AuthError::Store("x".into()))
            }
            fn put(&self, _: &str, _: StoredTokens) -> Result<(), AuthError> {
                Err(AuthError::Store("x".into()))
            }
            fn remove(&self, _: &str) -> Result<(), AuthError> {
                Ok(())
            }
        }
        let store = FallbackTokenStore::new(Arc::new(Broken));
        assert!(store.put("a", tokens("1")).is_err());
        assert_eq!(store.notice(), None);
    }

    /// Talks to the real OS keyring with a synthetic value and removes it again. Run by hand:
    /// `cargo test -p vl-auth -- --ignored real_keyring`.
    #[test]
    #[ignore = "uses the real system keyring"]
    fn real_keyring_round_trip() {
        let store = KeyringTokenStore::new(Arc::new(OsVault::open().unwrap()));
        let id = format!("selftest-{}", std::process::id());
        assert_eq!(store.get(&id).unwrap(), None);
        store.put(&id, tokens("synthetic")).unwrap();
        let read = store.get(&id).unwrap();
        store.remove(&id).unwrap();
        assert_eq!(read, Some(tokens("synthetic")));
        assert_eq!(store.get(&id).unwrap(), None);
    }
}
