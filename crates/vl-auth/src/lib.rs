//! Accounts.
//!
//! - Microsoft sign-in: device code, Xbox Live, XSTS, Minecraft services, entitlements, profile.
//! - Token refresh before launch; offline accounts.
//! - Secrets (refresh and access tokens) live in the OS keyring; the account list on disk holds
//!   public profile data only. Secrets never reach logs, events or error messages.
