//! Accounts as front ends see them: public profile data only, never tokens.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, TaskId};

/// How an account signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccountKind {
    /// A Microsoft account that owns the game.
    Microsoft,
    /// An offline account: a name only, no sign-in.
    Offline,
}

/// One account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The identifier.
    pub id: AccountId,
    /// How it signs in.
    pub kind: AccountKind,
    /// The player name.
    pub name: String,
    /// The profile UUID (hyphenated).
    pub uuid: String,
    /// Whether launches use this account when none is chosen.
    pub is_default: bool,
    /// Whether the stored sign-in expired and the user must sign in again.
    pub needs_sign_in: bool,
}

/// What the user needs to finish a device-code sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodePrompt {
    /// The sign-in task waiting for the user.
    pub task: TaskId,
    /// The code to enter.
    pub user_code: String,
    /// The page to enter it on.
    pub verification_uri: String,
    /// Seconds until the code expires.
    pub expires_in_secs: u64,
}

/// Account commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccountCommand {
    /// All accounts → `Response::Accounts`.
    List,
    /// Starts a Microsoft device-code sign-in → `Response::Task`; the code arrives as
    /// `AccountEvent::DeviceCode`, the new account as `AccountEvent::ListChanged`.
    SignInMicrosoft,
    /// Adds an offline account → `Response::Account`.
    AddOffline {
        /// The player name.
        name: String,
    },
    /// Renews a Microsoft account's sign-in from its stored refresh token → `Response::Task`;
    /// when the token is rejected the account is marked `needs_sign_in` and the task fails.
    Refresh {
        /// The account.
        id: AccountId,
    },
    /// Removes an account and its stored secrets → `Response::Done`.
    Remove {
        /// The account.
        id: AccountId,
    },
    /// Makes an account the default → `Response::Done`.
    SetDefault {
        /// The account.
        id: AccountId,
    },
}

/// Account changes and prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AccountEvent {
    /// A sign-in waits for the user to enter a code.
    DeviceCode(DeviceCodePrompt),
    /// Accounts were added, removed or changed; list them again.
    ListChanged,
}
