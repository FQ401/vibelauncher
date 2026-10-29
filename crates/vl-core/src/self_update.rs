//! Updating the launcher itself: checking the signed release manifest, downloading the release
//! file, and installing it through the update helper after the launcher exits.
//!
//! Versions are carried as text (`MAJOR.MINOR.PATCH[-pre]`); the rules behind them live in
//! `vl-update`.

use serde::{Deserialize, Serialize};

/// A self-update command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SelfUpdateCommand {
    /// What is known without the network → `Response::SelfUpdate`.
    Status,
    /// Fetches and verifies the release manifest → `Response::SelfUpdate` with
    /// [`SelfUpdateStatus::offer`] set. A build without a release key answers
    /// `ErrorKind::Unsupported`; a refused manifest answers `ErrorKind::Integrity`.
    Check,
    /// Checks again and downloads the release file into the staging folder → `Response::Task`
    /// (kind `DownloadUpdate`, cancellable, byte progress). Fails when there is nothing to
    /// download automatically.
    Download,
    /// Installs the downloaded release → `Response::Done`. Refused while a game or a task runs,
    /// when nothing newer is downloaded, and for install methods the launcher cannot replace.
    /// On success the update helper has started and waits for this process to exit: the front
    /// end exits right away. The helper replaces the files, and with `restart` starts the
    /// launcher again with those arguments; a failed replacement is undone and reported by
    /// [`SelfUpdateStatus::last_error`] on the next start.
    Apply {
        /// The arguments the launcher is started with afterwards; `None` does not start it.
        #[serde(default)]
        restart: Option<Vec<String>>,
    },
}

/// What the launcher knows about its own updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfUpdateStatus {
    /// The running version.
    pub current: String,
    /// How the launcher was installed: `msi`, `portable`, `mac_app`, `app_image`, `deb`, `rpm`
    /// or `unknown`.
    pub install: String,
    /// Whether this build can verify updates (it has a release key).
    pub enabled: bool,
    /// The newest release version verified on this machine.
    pub newest_seen: Option<String>,
    /// The result of the last check in this session.
    pub offer: Option<UpdateOffer>,
    /// The downloaded version waiting to be installed.
    pub staged: Option<String>,
    /// The last failed installation, reported once after it.
    pub last_error: Option<UpdateFailure>,
    /// The update installed right before this start.
    pub just_updated: Option<JustUpdated>,
}

/// The result of a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "offer")]
#[non_exhaustive]
pub enum UpdateOffer {
    /// The running version is the newest.
    UpToDate,
    /// The launcher can download and install `version` itself.
    Update {
        /// The new version.
        version: String,
        /// The release date, `YYYY-MM-DD`.
        date: String,
        /// The size of the download in bytes.
        size: u64,
    },
    /// A newer version exists; the user installs it from `page`.
    Manual {
        /// The new version.
        version: String,
        /// The release page.
        page: String,
        /// Why: `package_manager`, `unknown_install`, `too_old`, `no_artifact`.
        reason: String,
    },
}

/// An installation that failed and was undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateFailure {
    /// The version that was being installed.
    pub version: String,
    /// What went wrong (English technical detail).
    pub message: String,
    /// When, in seconds since the Unix epoch.
    pub at_unix: u64,
}

/// An update installed right before this start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JustUpdated {
    /// The version before the update.
    pub from: String,
    /// The version installed.
    pub to: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_without_restart_reads_from_short_json() {
        let c: SelfUpdateCommand = serde_json::from_str(r#"{"apply":{}}"#).unwrap();
        assert_eq!(c, SelfUpdateCommand::Apply { restart: None });
        let offer = UpdateOffer::Update {
            version: "1.2.0".into(),
            date: "2026-10-01".into(),
            size: 7,
        };
        let json = serde_json::to_string(&offer).unwrap();
        assert!(json.contains(r#""offer":"update""#), "{json}");
        assert_eq!(serde_json::from_str::<UpdateOffer>(&json).unwrap(), offer);
    }
}
