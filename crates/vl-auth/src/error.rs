//! Sign-in failures and their mapping to the facade error.

use std::fmt;

use vl_core::{Error, ErrorKind};
use vl_net::NetError;

/// Why an Xbox Live (XSTS) step refused the account, from the `XErr` code of its 401 answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XboxProblem {
    /// The Microsoft account has no Xbox profile (`2148916233`).
    NoXboxAccount,
    /// Xbox Live is not available in the account's country or region (`2148916235`).
    RegionUnavailable,
    /// The account needs adult verification (`2148916236`, `2148916237`).
    AdultVerificationRequired,
    /// A child account that a family organizer has not allowed to play online (`2148916238`).
    ChildAccount,
    /// Any other code.
    Other(u64),
}

impl XboxProblem {
    /// Classifies an `XErr` code.
    pub fn from_code(code: u64) -> Self {
        match code {
            2_148_916_233 => Self::NoXboxAccount,
            2_148_916_235 => Self::RegionUnavailable,
            2_148_916_236 | 2_148_916_237 => Self::AdultVerificationRequired,
            2_148_916_238 => Self::ChildAccount,
            other => Self::Other(other),
        }
    }
}

impl fmt::Display for XboxProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoXboxAccount => {
                f.write_str("the Microsoft account has no Xbox profile (XErr 2148916233)")
            }
            Self::RegionUnavailable => f.write_str(
                "Xbox Live is not available in the account's country or region (XErr 2148916235)",
            ),
            Self::AdultVerificationRequired => {
                f.write_str("the account needs adult verification on Xbox (XErr 2148916236/7)")
            }
            Self::ChildAccount => f.write_str(
                "child account: a family organizer must allow online play (XErr 2148916238)",
            ),
            Self::Other(code) => write!(f, "Xbox Live refused the account (XErr {code})"),
        }
    }
}

/// What went wrong while signing in or refreshing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthError {
    /// A network failure.
    Net(NetError),
    /// The user cancelled.
    Cancelled,
    /// The user declined the sign-in.
    Declined,
    /// The device code or the sign-in request expired before the user finished.
    Expired,
    /// Microsoft rejected the refresh token: the user must sign in again.
    SignInRequired,
    /// Xbox Live refused the account.
    Xbox(XboxProblem),
    /// The Minecraft services refused this application (HTTP 403): Mojang has not approved the
    /// application ID yet.
    AppNotPermitted,
    /// The Minecraft services rejected the token (HTTP 401).
    Unauthorized,
    /// The account does not own the game.
    NoGame,
    /// The account owns the game but has no Minecraft profile yet.
    NoProfile,
    /// A response did not have the expected shape or status.
    Protocol(String),
    /// Storing tokens or the account list failed.
    Store(String),
    /// The system keyring cannot be used; the text explains what to check.
    KeyringUnavailable(String),
}

impl AuthError {
    /// A stable short code for logs and for front ends that localize.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Net(_) => "network",
            Self::Cancelled => "cancelled",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::SignInRequired => "sign_in_required",
            Self::Xbox(XboxProblem::NoXboxAccount) => "no_xbox_account",
            Self::Xbox(XboxProblem::RegionUnavailable) => "xbox_region",
            Self::Xbox(XboxProblem::AdultVerificationRequired) => "xbox_adult_verification",
            Self::Xbox(XboxProblem::ChildAccount) => "xbox_child_account",
            Self::Xbox(_) => "xbox_refused",
            Self::AppNotPermitted => "app_not_permitted",
            Self::Unauthorized => "minecraft_unauthorized",
            Self::NoGame => "no_game",
            Self::NoProfile => "no_profile",
            Self::Protocol(_) => "protocol",
            Self::Store(_) => "store",
            Self::KeyringUnavailable(_) => "keyring_unavailable",
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net(e) => write!(f, "network error: {e}"),
            Self::Cancelled => f.write_str("sign-in cancelled"),
            Self::Declined => f.write_str("the sign-in was declined"),
            Self::Expired => f.write_str("the sign-in code expired"),
            Self::SignInRequired => f.write_str("the stored sign-in expired; sign in again"),
            Self::Xbox(p) => write!(f, "{p}"),
            Self::AppNotPermitted => f.write_str(
                "app not permitted yet: the Minecraft services refused this application ID \
                 (HTTP 403); Mojang has not approved it",
            ),
            Self::Unauthorized => f.write_str("the Minecraft services rejected the sign-in token"),
            Self::NoGame => f.write_str("the account does not own Minecraft: Java Edition"),
            Self::NoProfile => f.write_str(
                "the account owns the game but has no Minecraft profile; start the official \
                 launcher once to create it",
            ),
            Self::Protocol(m) => write!(f, "unexpected response: {m}"),
            Self::Store(m) => write!(f, "cannot store account data: {m}"),
            Self::KeyringUnavailable(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<NetError> for AuthError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Cancelled => Self::Cancelled,
            other => Self::Net(other),
        }
    }
}

impl From<AuthError> for Error {
    fn from(e: AuthError) -> Self {
        let kind = match &e {
            AuthError::Net(_) => ErrorKind::Network,
            AuthError::Cancelled => ErrorKind::Cancelled,
            AuthError::Protocol(_) => ErrorKind::Integrity,
            AuthError::Store(_) | AuthError::KeyringUnavailable(_) => ErrorKind::Io,
            _ => ErrorKind::Auth,
        };
        Error::new(kind, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xerr_codes_are_classified() {
        assert_eq!(
            XboxProblem::from_code(2_148_916_233),
            XboxProblem::NoXboxAccount
        );
        assert_eq!(
            XboxProblem::from_code(2_148_916_238),
            XboxProblem::ChildAccount
        );
        assert_eq!(XboxProblem::from_code(5), XboxProblem::Other(5));
    }

    #[test]
    fn kinds_map_to_the_facade() {
        let kind = |e: AuthError| Error::from(e).kind;
        assert_eq!(kind(AuthError::AppNotPermitted), ErrorKind::Auth);
        assert_eq!(kind(AuthError::NoGame), ErrorKind::Auth);
        assert_eq!(
            kind(AuthError::Net(NetError::Timeout("x".into()))),
            ErrorKind::Network
        );
        assert_eq!(kind(AuthError::Cancelled), ErrorKind::Cancelled);
        assert!(
            Error::from(AuthError::AppNotPermitted)
                .message
                .contains("app not permitted yet")
        );
    }
}
