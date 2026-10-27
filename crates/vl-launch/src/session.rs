//! The player's session as the game receives it.

use std::fmt;

/// A secret string: it prints as `***` and is never serialized, so it cannot reach a log or an
/// error message by accident. [`Secret::expose`] is the one way to read it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value; call only where it goes to the game process.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Who plays: the values behind `${auth_player_name}`, `${auth_uuid}`, `${auth_access_token}`,
/// `${user_type}` and friends.
///
/// The launch API takes the session as a parameter. Whoever calls it decides where the session
/// comes from; this crate neither stores nor refreshes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSession {
    /// The player name.
    pub player_name: String,
    /// The profile UUID, with or without hyphens.
    pub uuid: String,
    /// The access token the game sends to the session servers.
    pub access_token: Secret,
    /// The user type the game expects: `msa`, `mojang` or `legacy`.
    pub user_type: String,
    /// The Xbox user id (`${auth_xuid}`), if known.
    pub xuid: Option<String>,
    /// The client id (`${clientid}`), if known.
    pub client_id: Option<String>,
}

impl LaunchSession {
    /// The UUID without hyphens, the form old versions take.
    pub fn uuid_compact(&self) -> String {
        self.uuid.replace('-', "")
    }
}
