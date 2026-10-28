//! The CurseForge API key.

use std::fmt;

use vl_core::{Error, ErrorKind, Result};

/// The request header that carries the key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// The environment variable a developer sets to use their key without the keyring.
pub const API_KEY_ENV: &str = "VL_CURSEFORGE_API_KEY";

/// The longest key accepted.
pub const MAX_KEY_LEN: usize = 256;

/// A CurseForge API key. Its `Debug` and `Display` output is masked, so it cannot end up in a
/// log, an event or an error message by accident; read it with [`expose`](Self::expose) only to
/// put it in the request header.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Checks and wraps a key: surrounding whitespace is dropped; the rest must be 1 to
    /// [`MAX_KEY_LEN`] visible ASCII characters (so it is a valid header value). The error never
    /// repeats the value.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(Error::invalid_input("the CurseForge API key is empty"));
        }
        if trimmed.len() > MAX_KEY_LEN || !trimmed.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::invalid_input(format!(
                "a CurseForge API key is at most {MAX_KEY_LEN} visible ASCII characters"
            )));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The value; call only to send it to CurseForge.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The error when no key is set: [`ErrorKind::NoApiKey`], which the interface explains by asking
/// for the user's own key.
pub fn no_api_key() -> Error {
    Error::new(
        ErrorKind::NoApiKey,
        format!(
            "CurseForge needs an API key: enter your own key in the settings \
             (developers may set {API_KEY_ENV})"
        ),
    )
}

/// The error when CurseForge refuses the key (`401` or `403`).
pub fn rejected_api_key() -> Error {
    Error::new(
        ErrorKind::Auth,
        "CurseForge rejected the API key; check the key in the settings",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUE: &str = "$2a$10$synthetic.key.for.tests.only";

    #[test]
    fn debug_and_display_hide_the_value() {
        let key = ApiKey::new(VALUE).unwrap();
        let shown = format!("{key:?} {key} {:?}", Some(&key));
        assert!(!shown.contains("synthetic"), "{shown}");
        assert!(shown.contains("redacted"));
        assert_eq!(key.expose(), VALUE);
    }

    #[test]
    fn keys_are_trimmed_and_checked_without_echo() {
        assert_eq!(ApiKey::new(format!("  {VALUE}\n")).unwrap().expose(), VALUE);
        for bad in [
            String::new(),
            "   ".into(),
            "two words".into(),
            "line\r\nbreak".into(),
            "ключ".into(),
            "x".repeat(MAX_KEY_LEN + 1),
        ] {
            let err = ApiKey::new(bad.clone()).unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidInput);
            if bad.trim().len() > 3 {
                assert!(!err.message.contains(bad.trim()), "{err}");
            }
        }
    }

    #[test]
    fn missing_and_rejected_keys_are_typed() {
        assert_eq!(no_api_key().kind, ErrorKind::NoApiKey);
        assert!(no_api_key().message.contains(API_KEY_ENV));
        assert_eq!(rejected_api_key().kind, ErrorKind::Auth);
    }
}
