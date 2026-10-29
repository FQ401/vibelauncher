//! Tokens in the operating system's credential store.
//!
//! Each account's [`StoredTokens`] are one JSON record. The Windows Credential Manager takes at
//! most 2560 bytes (1280 UTF-16 characters) per entry, less than a Microsoft refresh token and a
//! Minecraft token together, so [`ChunkedVault`] splits the record on every platform: the entry
//! `account:<id>` holds a small header (format version, number of parts, length, SHA-256) and the
//! entries `account:<id>#0`, `#1`, ... hold parts of at most [`PART_BYTES`] bytes. Parts are
//! written before the header; a set that is incomplete or does not match its header reads as
//! missing and the user signs in again. A record from before the split (one entry with the whole
//! JSON) is still read. Calls into the OS store are serialized within the process.
//!
//! The backends are the Windows Credential Manager, the macOS Keychain and, elsewhere, the Secret
//! Service over D-Bus (pure Rust, no GTK or libsecret). Without a usable store
//! [`default_token_store`] falls back to memory: tokens are never written to disk in the clear.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use keyring_core::{Entry, Error as KeyError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
        KeyError::TooLong(_, limit) => AuthError::Store(format!(
            "the entry is too long for the system keyring (limit {limit})"
        )),
        KeyError::BadEncoding(_) => {
            AuthError::Store("the system keyring cannot encode the entry".into())
        }
        _ => AuthError::Store("the system keyring rejected the request".into()),
    }
}

/// Serializes the calls into the OS store within this process: the Windows Credential Manager
/// was seen to lose deletes made concurrently from several threads.
static OS_STORE: Mutex<()> = Mutex::new(());

fn os_store_lock() -> std::sync::MutexGuard<'static, ()> {
    OS_STORE.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SecretVault for OsVault {
    fn get(&self, key: &str) -> Result<Option<String>, AuthError> {
        let _serial = os_store_lock();
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            // Missing, or not text and so not ours: the user signs in again.
            Err(KeyError::NoEntry | KeyError::BadEncoding(_)) => Ok(None),
            Err(e) => Err(map_error(&e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), AuthError> {
        let _serial = os_store_lock();
        self.entry(key)?
            .set_password(value)
            .map_err(|e| map_error(&e))
    }

    fn delete(&self, key: &str) -> Result<(), AuthError> {
        let _serial = os_store_lock();
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(KeyError::NoEntry) => Ok(()),
            Err(e) => Err(map_error(&e)),
        }
    }
}

/// The longest part [`ChunkedVault`] writes, in UTF-8 bytes. A part is then at most as many
/// UTF-16 units, well under the 1280 a Windows credential holds.
pub const PART_BYTES: usize = 1000;

/// The most parts a value may have; a header that claims more is damaged.
const MAX_PARTS: usize = 64;

/// The header format version.
const CHUNKED_VERSION: u32 = 2;

/// The header entry of a split value.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    v: u32,
    parts: usize,
    len: usize,
    sha256: String,
}

/// A [`SecretVault`] that stores each value as a header entry and parts small enough for any
/// keyring (see the module documentation). Reading also accepts a value stored whole, as
/// written before the split.
pub struct ChunkedVault {
    inner: Arc<dyn SecretVault>,
    /// Held for each whole operation, so the entries of one value are never interleaved.
    busy: Mutex<()>,
}

impl ChunkedVault {
    /// Splits the values stored in `inner`.
    pub fn new(inner: Arc<dyn SecretVault>) -> Self {
        Self {
            inner,
            busy: Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.busy.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn part_key(key: &str, index: usize) -> String {
        format!("{key}#{index}")
    }

    /// The number of parts the header under `key` names, or 0 when there is none.
    fn known_parts(&self, key: &str) -> Result<usize, AuthError> {
        Ok(self
            .inner
            .get(key)?
            .and_then(|text| serde_json::from_str::<Header>(&text).ok())
            .map_or(0, |h| h.parts.min(MAX_PARTS)))
    }

    /// Deletes the parts of `key` from `from` on: those past `known` (the count a header named)
    /// while more are found, so parts left by an interrupted write or delete go too, then the
    /// known ones from the last down. Parts are always written and deleted so that what remains
    /// is a run without gaps.
    fn delete_parts(&self, key: &str, from: usize, known: usize) -> Result<(), AuthError> {
        let start = from.max(known);
        let mut end = start;
        while end < MAX_PARTS && self.inner.get(&Self::part_key(key, end))?.is_some() {
            end += 1;
        }
        for index in (from..end).rev() {
            self.inner.delete(&Self::part_key(key, index))?;
        }
        Ok(())
    }
}

/// Splits `text` into pieces of at most [`PART_BYTES`] bytes on character boundaries.
fn split(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(PART_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (part, tail) = rest.split_at(end);
        parts.push(part);
        rest = tail;
    }
    parts
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl SecretVault for ChunkedVault {
    fn get(&self, key: &str) -> Result<Option<String>, AuthError> {
        let _busy = self.lock();
        let Some(text) = self.inner.get(key)? else {
            return Ok(None);
        };
        let Ok(header) = serde_json::from_str::<Header>(&text) else {
            // A value stored whole, before the split.
            return Ok(Some(text));
        };
        if header.v != CHUNKED_VERSION || header.parts > MAX_PARTS {
            return Ok(None);
        }
        let mut value = String::new();
        for index in 0..header.parts {
            let Some(part) = self.inner.get(&Self::part_key(key, index))? else {
                return Ok(None);
            };
            value.push_str(&part);
            if value.len() > header.len {
                return Ok(None);
            }
        }
        // An incomplete or mixed set (an interrupted write) is as good as none.
        Ok((value.len() == header.len && sha256_hex(&value) == header.sha256).then_some(value))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), AuthError> {
        let _busy = self.lock();
        let parts = split(value);
        if parts.len() > MAX_PARTS {
            return Err(AuthError::Store(
                "the entry is too long for the system keyring".into(),
            ));
        }
        let known = self.known_parts(key)?;
        for (index, part) in parts.iter().enumerate() {
            self.inner.set(&Self::part_key(key, index), part)?;
        }
        let header = serde_json::to_string(&Header {
            v: CHUNKED_VERSION,
            parts: parts.len(),
            len: value.len(),
            sha256: sha256_hex(value),
        })
        .map_err(|_| AuthError::Store("cannot encode the entry header".into()))?;
        self.inner.set(key, &header)?;
        self.delete_parts(key, parts.len(), known)
    }

    fn delete(&self, key: &str) -> Result<(), AuthError> {
        let _busy = self.lock();
        let known = self.known_parts(key)?;
        // The header goes first: what an interrupted delete leaves reads as missing.
        self.inner.delete(key)?;
        self.delete_parts(key, 0, known)
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    refresh_token: String,
    minecraft_token: String,
    minecraft_expires_at: u64,
}

/// A [`TokenStore`] that keeps each account's tokens as one record in a [`SecretVault`], split
/// into entries the keyring accepts by a [`ChunkedVault`].
pub struct KeyringTokenStore {
    vault: ChunkedVault,
}

impl KeyringTokenStore {
    /// A store on `vault`.
    pub fn new(vault: Arc<dyn SecretVault>) -> Self {
        Self {
            vault: ChunkedVault::new(vault),
        }
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

    /// Synthetic tokens about as long as real ones (about 6 KB of JSON together).
    fn long_tokens(tag: &str) -> StoredTokens {
        StoredTokens {
            refresh_token: Secret::new(format!("refresh-{tag}-{}", "r".repeat(2000))),
            minecraft_token: Secret::new(format!("mc-{tag}-{}", "m".repeat(4000))),
            minecraft_expires_at: 42,
        }
    }

    /// A [`MemoryVault`] that refuses entries longer than a Windows credential holds.
    struct Limited(MemoryVault);

    impl SecretVault for Limited {
        fn get(&self, key: &str) -> Result<Option<String>, AuthError> {
            self.0.get(key)
        }
        fn set(&self, key: &str, value: &str) -> Result<(), AuthError> {
            if value.encode_utf16().count() > 1280 {
                return Err(map_error(&KeyError::TooLong("password".into(), 2560)));
            }
            self.0.set(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), AuthError> {
            self.0.delete(key)
        }
    }

    fn limited() -> Arc<Limited> {
        Arc::new(Limited(MemoryVault::new()))
    }

    fn part_count(vault: &Limited, key: &str) -> usize {
        (0..MAX_PARTS)
            .filter(|i| vault.0.raw(&format!("{key}#{i}")).is_some())
            .count()
    }

    #[test]
    fn a_long_record_is_split_into_parts_the_keyring_accepts() {
        let vault = limited();
        let store = KeyringTokenStore::new(vault.clone());
        store.put("a", long_tokens("1")).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(long_tokens("1")));
        let header: Header = serde_json::from_str(&vault.0.raw("account:a").unwrap()).unwrap();
        assert_eq!(header.v, 2);
        assert!(header.parts > 1);
        assert_eq!(part_count(&vault, "account:a"), header.parts);
        for i in 0..header.parts {
            assert!(vault.0.raw(&format!("account:a#{i}")).unwrap().len() <= PART_BYTES);
        }
        // Whole, the record is refused, as by the Windows Credential Manager.
        let whole = vault.0.raw("account:a#0").unwrap().repeat(header.parts);
        let refused = vault.set("x", &whole).unwrap_err();
        assert!(refused.to_string().contains("too long"));
    }

    #[test]
    fn overwriting_and_removing_leave_no_stale_parts() {
        let vault = limited();
        let store = KeyringTokenStore::new(vault.clone());
        store.put("a", long_tokens("1")).unwrap();
        store.put("b", long_tokens("2")).unwrap();
        let long = part_count(&vault, "account:a");
        store.put("a", tokens("short")).unwrap();
        assert_eq!(part_count(&vault, "account:a"), 1);
        assert_eq!(store.get("a").unwrap(), Some(tokens("short")));
        store.put("a", long_tokens("3")).unwrap();
        assert_eq!(part_count(&vault, "account:a"), long);
        store.remove("a").unwrap();
        assert_eq!(part_count(&vault, "account:a"), 0);
        assert_eq!(vault.0.raw("account:a"), None);
        assert_eq!(store.get("a").unwrap(), None);
        assert_eq!(store.get("b").unwrap(), Some(long_tokens("2")));
    }

    #[test]
    fn an_incomplete_or_damaged_set_reads_as_missing() {
        let vault = limited();
        let store = KeyringTokenStore::new(vault.clone());
        store.put("a", long_tokens("1")).unwrap();
        let part = vault.0.raw("account:a#1").unwrap();
        // A part replaced with other text of the same length (a write cut short).
        vault.0.set("account:a#1", &"z".repeat(part.len())).unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        vault.0.set("account:a#1", &part).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(long_tokens("1")));
        // A missing part.
        let last = format!("account:a#{}", part_count(&vault, "account:a") - 1);
        vault.0.delete(&last).unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        // A header of another version.
        vault
            .0
            .set("account:a", r#"{"v":3,"parts":1,"len":1,"sha256":""}"#)
            .unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        // Signing in again writes a whole set and clears what was left.
        store.put("a", tokens("2")).unwrap();
        assert_eq!(store.get("a").unwrap(), Some(tokens("2")));
        assert_eq!(part_count(&vault, "account:a"), 1);
    }

    #[test]
    fn a_record_stored_whole_is_still_read() {
        let vault = Arc::new(MemoryVault::new());
        vault
            .set(
                "account:a",
                r#"{"refresh_token":"refresh-1","minecraft_token":"mc-1","minecraft_expires_at":42}"#,
            )
            .unwrap();
        let store = KeyringTokenStore::new(vault.clone());
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
        store.put("a", tokens("1")).unwrap();
        assert!(vault.raw("account:a#0").is_some());
        assert_eq!(store.get("a").unwrap(), Some(tokens("1")));
    }

    #[test]
    fn parts_end_on_character_boundaries() {
        let text = "\u{e9}".repeat(PART_BYTES) + &"\u{1f600}".repeat(PART_BYTES);
        let parts = split(&text);
        assert!(parts.iter().all(|p| p.len() <= PART_BYTES && !p.is_empty()));
        assert_eq!(parts.concat(), text);
        let vault = ChunkedVault::new(limited());
        vault.set("k", &text).unwrap();
        assert_eq!(vault.get("k").unwrap(), Some(text));
        vault.set("k", "").unwrap();
        assert_eq!(vault.get("k").unwrap(), Some(String::new()));
    }

    #[test]
    fn keyring_errors_name_the_problem_without_the_secret() {
        let too_long = map_error(&KeyError::TooLong("password".into(), 2560)).to_string();
        assert!(too_long.contains("too long") && too_long.contains("2560"));
        let encoding = map_error(&KeyError::BadEncoding(b"secret".to_vec())).to_string();
        assert!(encoding.contains("encode") && !encoding.contains("secret"));
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

    /// Taken by the tests on the real keyring, so they do not run in parallel; the size probe
    /// calls the store directly, without the lock of [`OsVault`].
    static REAL_KEYRING: Mutex<()> = Mutex::new(());

    /// Talks to the real OS keyring with a synthetic value and removes it again. Run by hand:
    /// `cargo test -p vl-auth -- --ignored real_keyring`.
    #[test]
    #[ignore = "uses the real system keyring"]
    fn real_keyring_round_trip() {
        let _serial = REAL_KEYRING.lock().unwrap_or_else(PoisonError::into_inner);
        let store = KeyringTokenStore::new(Arc::new(OsVault::open().unwrap()));
        let id = format!("selftest-{}", std::process::id());
        assert_eq!(store.get(&id).unwrap(), None);
        store.put(&id, tokens("synthetic")).unwrap();
        let read = store.get(&id).unwrap();
        store.remove(&id).unwrap();
        assert_eq!(read, Some(tokens("synthetic")));
        assert_eq!(store.get(&id).unwrap(), None);
    }

    /// A record as long as a real one (about 6 KB) through the real OS keyring, overwritten with
    /// a short one and removed; checks that no part is left. Run by hand:
    /// `cargo test -p vl-auth -- --ignored real_keyring`.
    #[test]
    #[ignore = "uses the real system keyring"]
    fn real_keyring_round_trip_of_a_long_record() {
        let _serial = REAL_KEYRING.lock().unwrap_or_else(PoisonError::into_inner);
        let os = Arc::new(OsVault::open().unwrap());
        let store = KeyringTokenStore::new(os.clone());
        let id = format!("selftest-long-{}", std::process::id());
        let key = format!("account:{id}");
        store.put(&id, long_tokens("synthetic")).unwrap();
        let read = store.get(&id).unwrap();
        store.put(&id, tokens("synthetic")).unwrap();
        let short = store.get(&id).unwrap();
        let second_part = os.get(&format!("{key}#1")).unwrap();
        store.remove(&id).unwrap();
        let left: Vec<usize> = (0..MAX_PARTS)
            .filter(|i| os.get(&format!("{key}#{i}")).unwrap().is_some())
            .collect();
        assert!(
            read == Some(long_tokens("synthetic")),
            "long record differs"
        );
        assert_eq!(short, Some(tokens("synthetic")));
        assert_eq!(second_part, None);
        assert_eq!(store.get(&id).unwrap(), None);
        assert!(left.is_empty(), "parts left: {left:?}");
    }

    /// Measures what one entry of the real OS keyring accepts: stores synthetic values of
    /// growing length under `selftest-size-*` keys, prints the keyring-core error for each, and
    /// deletes them again. Run by hand: `cargo test -p vl-auth -- --ignored --nocapture
    /// real_keyring_entry_size`.
    #[test]
    #[ignore = "uses the real system keyring"]
    fn real_keyring_entry_size() {
        let _serial = REAL_KEYRING.lock().unwrap_or_else(PoisonError::into_inner);
        let store = platform_store().unwrap();
        for len in [1000, 1279, 1280, 1281, 1300, 2600, 6000] {
            let key = format!("selftest-size-{}-{len}", std::process::id());
            let entry = store.build(KEYRING_SERVICE, &key, None).unwrap();
            let outcome = match entry.set_password(&"x".repeat(len)) {
                Ok(()) => "stored".to_owned(),
                Err(KeyError::TooLong(name, limit)) => format!("TooLong({name}, {limit})"),
                Err(e) => format!("other error: {e}"),
            };
            println!("{len} chars: {outcome}");
            let _ = entry.delete_credential();
        }
    }
}
