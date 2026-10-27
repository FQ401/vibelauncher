//! Accounts.
//!
//! - Microsoft sign-in: device code (primary) or authorization code with PKCE through the system
//!   browser and a loopback redirect, then Xbox Live, XSTS, Minecraft services, entitlements and
//!   profile ([`Authenticator`]).
//! - Token refresh before launch ([`Authenticator::refresh`]).
//! - Secrets (refresh and access tokens) are [`Secret`]s kept in a [`TokenStore`]; the account
//!   list on disk ([`AccountBook`]) holds public profile data only. Secrets never reach logs,
//!   events or error messages.
//! - HTTP goes through the [`AuthHttp`] seam, so tests run on scripted responses.

mod book;
mod config;
mod error;
mod flow;
mod http;
mod minecraft;
mod msa;
mod pkce;
mod secret;
mod xbox;

pub use book::AccountBook;
pub use config::{CLIENT_ID_ENV, DEFAULT_CLIENT_ID, MsaConfig};
pub use error::{AuthError, XboxProblem};
pub use flow::{Authenticator, Clock, Profile, Session, SystemClock};
pub use http::{AuthHttp, AuthRequest, AuthResponse, Body, Method, UreqHttp};
pub use msa::{DeviceCode, MsaTokens};
pub use pkce::{BrowserLogin, LoopbackHost};
pub use secret::{MemoryTokenStore, Secret, StoredTokens, TokenStore};

#[cfg(test)]
mod testing;
