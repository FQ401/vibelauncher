//! Accounts.
//!
//! - Microsoft sign-in: device code (primary) or authorization code with PKCE through the system
//!   browser and a loopback redirect, then Xbox Live, XSTS, Minecraft services, entitlements and
//!   profile ([`Authenticator`]).
//! - Token refresh before launch ([`Authenticator::refresh`]).
//! - Secrets (refresh and access tokens) are [`Secret`]s kept in a [`TokenStore`]; the account
//!   list on disk ([`AccountBook`]) holds public profile data only. Secrets never reach logs,
//!   events or error messages.
//! - The OS keyring ([`OsVault`] under [`KeyringTokenStore`], split into small entries by
//!   [`ChunkedVault`]) keeps the secrets;
//!   [`default_token_store`] falls back to memory, with a notice, when there is none. Tokens are
//!   never written to disk in the clear.
//! - HTTP goes through the [`AuthHttp`] seam, so tests run on scripted responses.

mod book;
mod config;
mod error;
mod flow;
mod http;
mod keyring;
mod minecraft;
mod msa;
mod pkce;
mod secret;
mod xbox;

pub use book::AccountBook;
pub use config::{CLIENT_ID_ENV, DEFAULT_CLIENT_ID, MsaConfig};
pub use error::{AuthError, XboxProblem};
pub use flow::{Authenticator, Clock, FRESH_MARGIN, Profile, Session, SystemClock};
pub use http::{AuthHttp, AuthRequest, AuthResponse, Body, Method, NetHttp};
pub use keyring::{
    ChunkedVault, FallbackTokenStore, KEYRING_SERVICE, KeyringTokenStore, MemoryVault, OsVault,
    PART_BYTES, SecretVault, default_token_store,
};
pub use msa::{DeviceCode, MsaTokens};
pub use pkce::{BrowserLogin, LoopbackHost};
pub use secret::{MemoryTokenStore, Secret, StoredTokens, TokenStore};

#[cfg(test)]
mod testing;
