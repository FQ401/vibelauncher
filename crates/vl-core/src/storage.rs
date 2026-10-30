//! Where the launcher keeps its data: the data folder and the sections that can live elsewhere.
//!
//! The data folder (the root) holds settings, accounts and the format version. Each growing
//! [`Section`] — instances, shared game files, Java runtimes, the cache, backups and logs — has a
//! path of its own; by default every section lives inside the root. A section is moved by copying
//! it to the new place and switching its path; the old copy stays until an explicit cleanup.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A part of the launcher's data with a path of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// The data folder itself: settings, accounts, the format version, the storage layout.
    Data,
    /// Instances (`instances/`).
    Instances,
    /// Shared game files: `versions/`, `libraries/`, `assets/` and `loaders/`.
    Game,
    /// Managed Java runtimes (`java/`).
    Java,
    /// Files that can be deleted at any time (`cache/`).
    Cache,
    /// Backups and snapshots (`snapshots/`).
    Backups,
    /// Launcher logs (`logs/`).
    Logs,
}

impl Section {
    /// Every section, the data folder first.
    pub const ALL: [Section; 7] = [
        Section::Data,
        Section::Instances,
        Section::Game,
        Section::Java,
        Section::Cache,
        Section::Backups,
        Section::Logs,
    ];

    /// The name used in files and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Data => "data",
            Section::Instances => "instances",
            Section::Game => "game",
            Section::Java => "java",
            Section::Cache => "cache",
            Section::Backups => "backups",
            Section::Logs => "logs",
        }
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Section {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Section::ALL
            .into_iter()
            .find(|section| section.as_str() == s)
            .ok_or_else(|| {
                crate::Error::invalid_input(format!(
                    "unknown storage section {s:?} (data, instances, game, java, cache, backups, logs)"
                ))
            })
    }
}

/// What chose the data folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RootSource {
    /// The `--dir` argument.
    Argument,
    /// The `VIBELAUNCHER_DIR` environment variable.
    Environment,
    /// The portable marker file next to the executable.
    Portable,
    /// The pointer file in the OS folder for settings.
    Pointer,
    /// The OS convention.
    Default,
}

impl RootSource {
    /// Whether the data folder is fixed from outside (argument, variable or marker) and cannot be
    /// moved by the launcher.
    pub fn is_fixed(self) -> bool {
        matches!(
            self,
            RootSource::Argument | RootSource::Environment | RootSource::Portable
        )
    }
}

/// One section as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionInfo {
    /// The section.
    pub section: Section,
    /// Its folder. For [`Section::Game`] the folder that holds `versions/`, `libraries/`,
    /// `assets/` and `loaders/`.
    pub path: PathBuf,
    /// Whether the path is the default one inside the data folder.
    pub default: bool,
    /// Whether the folder exists; an explicit folder on a disconnected drive does not.
    pub available: bool,
    /// Bytes used, when sizes were asked for.
    pub size: Option<u64>,
}

/// An old copy left behind by a move, removed only by [`StorageCommand::Cleanup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftover {
    /// The section it was a copy of.
    pub section: Section,
    /// The folder it is in.
    pub path: PathBuf,
    /// Bytes used, when sizes were asked for.
    pub size: Option<u64>,
}

/// The launcher's trash of deleted instances (`<data>/trash`); its size is not counted in the
/// data folder's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashFolder {
    /// The folder.
    pub path: PathBuf,
    /// Bytes used, when sizes were asked for.
    pub size: Option<u64>,
}

/// The storage layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInfo {
    /// The data folder.
    pub root: PathBuf,
    /// What chose it.
    pub source: RootSource,
    /// Every section, the data folder first.
    pub sections: Vec<SectionInfo>,
    /// The trash of deleted instances.
    pub trash: TrashFolder,
    /// Old copies that a cleanup would remove.
    pub leftovers: Vec<Leftover>,
    /// A section was moved; the launcher uses the new place after a restart and refuses other
    /// commands until then.
    pub restart_required: bool,
}

/// Something about a folder the launcher would store data in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PathIssue {
    /// Error: the folder cannot be created or written.
    NotWritable {
        /// The OS message.
        detail: String,
    },
    /// Error: inside the folder for installed programs (Program Files).
    ProgramFiles,
    /// Error: an operating system folder.
    SystemFolder,
    /// Error: the root of the system drive.
    SystemDriveRoot,
    /// Error: overlaps the folder of another section.
    Overlaps {
        /// The other section.
        section: Section,
    },
    /// Error: a copy needs an empty or new folder.
    NotEmpty,
    /// Error: "already moved" needs the folder to exist.
    Missing,
    /// Error: less free space than the copy needs.
    NotEnoughSpace {
        /// Free bytes on the drive.
        free: u64,
        /// Bytes the copy needs.
        needed: u64,
    },
    /// Warning: inside a folder synchronized by a cloud service.
    CloudFolder {
        /// The service, as far as it is known.
        service: String,
    },
    /// Warning: on a network drive.
    NetworkDrive,
    /// Warning: on a removable drive.
    RemovableDrive,
    /// Warning: a long path; files deep inside an instance may exceed the Windows path limit.
    LongPath {
        /// Characters in the path.
        length: usize,
        /// The length above which this is reported.
        limit: usize,
    },
    /// Warning: characters outside ASCII, which some Java versions and mods mishandle on Windows.
    NonAscii,
    /// Warning: little free space on the drive.
    LowSpace {
        /// Free bytes on the drive.
        free: u64,
    },
}

impl PathIssue {
    /// Whether the folder cannot be used; the others are warnings.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            PathIssue::NotWritable { .. }
                | PathIssue::ProgramFiles
                | PathIssue::SystemFolder
                | PathIssue::SystemDriveRoot
                | PathIssue::Overlaps { .. }
                | PathIssue::NotEmpty
                | PathIssue::Missing
                | PathIssue::NotEnoughSpace { .. }
        )
    }
}

/// The result of checking a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathCheck {
    /// The folder as the launcher would store it (absolute, links resolved).
    pub path: PathBuf,
    /// Errors and warnings, errors first.
    pub issues: Vec<PathIssue>,
    /// Free bytes on the drive, when known.
    pub free: Option<u64>,
}

impl PathCheck {
    /// Whether no issue is an error.
    pub fn usable(&self) -> bool {
        !self.issues.iter().any(PathIssue::is_error)
    }
}

/// How a section gets to its new folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MoveMode {
    /// Copy to the new folder, then switch; the old copy stays until a cleanup.
    #[default]
    Copy,
    /// The files are already there (the user copied them): only switch the path.
    SwitchOnly,
}

/// A request to move a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveRequest {
    /// The section.
    pub section: Section,
    /// The new folder; `None` is the default place inside the data folder (for the data folder:
    /// the OS convention).
    pub target: Option<PathBuf>,
    /// Copy, or only switch.
    #[serde(default)]
    pub mode: MoveMode,
}

/// What a cleanup did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Folders and files removed.
    pub removed: Vec<PathBuf>,
    /// Bytes freed.
    pub freed: u64,
    /// What could not be removed, with the reason; these stay listed as leftovers.
    pub failed: Vec<(PathBuf, String)>,
}

/// A local drive as the launcher sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveInfo {
    /// Where it is mounted (`D:\` on Windows).
    pub mount: PathBuf,
    /// Capacity in bytes.
    pub total: u64,
    /// Free bytes.
    pub free: u64,
    /// The drive holding the operating system.
    pub system: bool,
    /// A removable drive.
    pub removable: bool,
    /// A network file system.
    pub network: bool,
}

/// How much space the data of a typical user needs; see [`StorageEstimate::TYPICAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageEstimate {
    /// Instances.
    pub instances: u64,
    /// Shared game files: versions, libraries, assets and loaders.
    pub game: u64,
    /// Managed Java runtimes.
    pub java: u64,
    /// The sum.
    pub total: u64,
}

impl StorageEstimate {
    /// A typical amount for a few instances: three instances of 2 GiB each (a vanilla instance
    /// with worlds and resource packs; a large modpack takes several times that), 3 GiB of shared
    /// game files (a handful of versions with their libraries and asset indexes, plus loaders) and
    /// 1 GiB of Java runtimes (two or three at about 0.4 GiB). The numbers only decide whether a
    /// drive is roomy enough to be suggested; they are not limits.
    pub const TYPICAL: Self = Self {
        instances: 6 << 30,
        game: 3 << 30,
        java: 1 << 30,
        total: 10 << 30,
    };
}

/// Why a folder was suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SuggestionReason {
    /// A local fixed drive other than the system drive has the most free space.
    OtherDrive,
    /// No other drive is roomy enough, or the OS keeps its volumes elsewhere: the OS convention.
    OsFolder,
}

/// The suggested data folder for a first run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSuggestion {
    /// The suggested data folder.
    pub folder: PathBuf,
    /// Why it was chosen.
    pub reason: SuggestionReason,
    /// The drive the folder is on, when it is known.
    pub drive: Option<DriveInfo>,
    /// Whether the suggested folder is on the system drive.
    pub on_system_drive: bool,
    /// The OS-convention folder, the alternative.
    pub os_folder: PathBuf,
    /// The data folder in use now.
    pub current_root: PathBuf,
    /// Whether the data folder in use now is on the system drive, so the system drive grows with
    /// every instance.
    pub current_on_system_drive: bool,
    /// Local fixed drives, the system drive included, by mount point.
    pub drives: Vec<DriveInfo>,
    /// What the data of a typical user takes.
    pub estimate: StorageEstimate,
    /// Whether the suggested drive has room for [`Self::estimate`].
    pub enough_space: bool,
}

/// Storage commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StorageCommand {
    /// The layout → `Response::Storage`. With `sizes` every folder is measured, which can take a
    /// while.
    Show {
        /// Measure the folders.
        #[serde(default)]
        sizes: bool,
    },
    /// Checks a folder for a section → `Response::PathCheck`.
    Check {
        /// The section the folder is for.
        section: Section,
        /// The folder.
        path: PathBuf,
    },
    /// Moves a section → `Response::Task` (kind `MoveStorage`, cancellable). Refused while a game
    /// or another task runs. The copy goes to a temporary folder next to the target, is checked,
    /// renamed into place, and only then the path is switched; a failure or a cancel removes the
    /// partial copy and keeps the old path. Other commands are refused until a restart.
    Move(MoveRequest),
    /// Removes the old copies left by moves, prunes logs and the cache to their limits and, with
    /// `trash`, empties the instance and resource trash → `Response::StorageCleaned`.
    Cleanup {
        /// Also empty the trash folders.
        #[serde(default)]
        trash: bool,
    },
    /// Suggests a data folder: the local fixed drive other than the system drive with the most
    /// free space, if it has enough, else the OS folder -> `Response::StorageSuggestion`.
    Suggest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_names_round_trip() {
        for section in Section::ALL {
            assert_eq!(section.as_str().parse::<Section>().unwrap(), section);
        }
        assert!("nope".parse::<Section>().is_err());
    }

    #[test]
    fn errors_and_warnings_are_told_apart() {
        let check = PathCheck {
            path: PathBuf::from("x"),
            issues: vec![PathIssue::NonAscii, PathIssue::NetworkDrive],
            free: None,
        };
        assert!(check.usable());
        let check = PathCheck {
            issues: vec![PathIssue::ProgramFiles],
            ..check
        };
        assert!(!check.usable());
    }
}
