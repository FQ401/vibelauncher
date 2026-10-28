//! Suggesting a data folder that keeps the system drive from growing.
//!
//! On Windows, the local fixed drive other than the system drive with the most free space (above
//! [`MIN_FREE`] and the typical need) gets `<drive>:\VibeLauncher`. Everywhere else, and when no
//! such drive exists, the OS convention is suggested. Drives come from a [`DiskSource`], so the
//! choice can be tested on synthetic lists.

use std::path::{Component, Path};

use vl_core::platform::Os;
use vl_core::storage::{DriveInfo, StorageEstimate, StorageSuggestion, SuggestionReason};

use crate::check::is_network_fs;
use crate::paths::{self, Env, is_within};

/// A drive is suggested only with more free space than this.
pub const MIN_FREE: u64 = 20 << 30;
/// The folder created on the suggested drive.
pub const DRIVE_FOLDER: &str = "VibeLauncher";

/// Where the list of drives comes from.
pub trait DiskSource {
    /// Every mounted drive; `system` is filled in by the caller.
    fn drives(&self) -> Vec<DriveInfo>;
}

/// The drives of this machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDisks;

impl DiskSource for SystemDisks {
    fn drives(&self) -> Vec<DriveInfo> {
        sysinfo::Disks::new_with_refreshed_list()
            .list()
            .iter()
            .map(|d| DriveInfo {
                mount: d.mount_point().to_path_buf(),
                total: d.total_space(),
                free: d.available_space(),
                system: false,
                removable: d.is_removable(),
                network: is_network_fs(&d.file_system().to_string_lossy()),
            })
            .collect()
    }
}

/// Suggests a data folder given the drives of `source`; `current_root` is the folder in use.
pub fn suggest(env: &Env, source: &dyn DiskSource, current_root: &Path) -> StorageSuggestion {
    let mut drives: Vec<DriveInfo> = source
        .drives()
        .into_iter()
        .filter(|d| !d.removable && !d.network && d.total > 0)
        .collect();
    for d in &mut drives {
        d.system = is_system_mount(env, &d.mount);
    }
    drives.sort_by(|a, b| a.mount.cmp(&b.mount));

    let os_folder = paths::default_root(env).unwrap_or_else(|_| current_root.to_path_buf());
    let estimate = StorageEstimate::TYPICAL;
    let needed = MIN_FREE.max(estimate.total);

    let other = (env.os == Os::Windows)
        .then(|| {
            drives
                .iter()
                .filter(|d| !d.system && d.free > needed)
                .max_by_key(|d| d.free)
        })
        .flatten();

    let (folder, reason, drive) = match other {
        Some(d) => (
            d.mount.join(DRIVE_FOLDER),
            SuggestionReason::OtherDrive,
            Some(d.clone()),
        ),
        None => (
            os_folder.clone(),
            SuggestionReason::OsFolder,
            drive_of(env, &drives, &os_folder).cloned(),
        ),
    };
    let on_system_drive = on_system_drive(env, &drives, &folder);
    StorageSuggestion {
        enough_space: drive.as_ref().is_none_or(|d| d.free > estimate.total),
        on_system_drive,
        current_on_system_drive: self::on_system_drive(env, &drives, current_root),
        current_root: current_root.to_path_buf(),
        os_folder,
        folder,
        reason,
        drive,
        drives,
        estimate,
    }
}

/// Whether `path` is on the system drive; a path on no known drive counts as on it.
fn on_system_drive(env: &Env, drives: &[DriveInfo], path: &Path) -> bool {
    drive_of(env, drives, path).is_none_or(|d| d.system)
}

/// The drive holding `path`: on Windows by drive letter, elsewhere the longest mount point that
/// contains it.
fn drive_of<'a>(env: &Env, drives: &'a [DriveInfo], path: &Path) -> Option<&'a DriveInfo> {
    if env.os == Os::Windows {
        let letter = drive_letter(path)?;
        return drives
            .iter()
            .find(|d| drive_letter(&d.mount) == Some(letter));
    }
    drives
        .iter()
        .filter(|d| is_within(path, &d.mount))
        .max_by_key(|d| d.mount.components().count())
}

fn is_system_mount(env: &Env, mount: &Path) -> bool {
    if env.os == Os::Windows {
        let system = [&env.system_drive, &env.system_root, &env.local_appdata]
            .into_iter()
            .flatten()
            .find_map(|p| drive_letter(p));
        system.is_some() && drive_letter(mount) == system
    } else {
        mount.components().all(|c| matches!(c, Component::RootDir))
    }
}

/// The upper-case drive letter of a Windows path (`c:\x`, `C:`, `\\?\C:\x`).
fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_str()?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| letter.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const GIB: u64 = 1 << 30;

    struct Fake(Vec<DriveInfo>);

    impl DiskSource for Fake {
        fn drives(&self) -> Vec<DriveInfo> {
            self.0.clone()
        }
    }

    fn drive(mount: &str, total_gib: u64, free_gib: u64) -> DriveInfo {
        DriveInfo {
            mount: PathBuf::from(mount),
            total: total_gib * GIB,
            free: free_gib * GIB,
            system: false,
            removable: false,
            network: false,
        }
    }

    fn windows() -> Env {
        Env {
            os: Os::Windows,
            appdata: Some(PathBuf::from(r"C:\Users\Alex\AppData\Roaming")),
            local_appdata: Some(PathBuf::from(r"C:\Users\Alex\AppData\Local")),
            system_drive: Some(PathBuf::from("C:")),
            system_root: Some(PathBuf::from(r"C:\Windows")),
            ..Env::default()
        }
    }

    fn linux() -> Env {
        Env {
            os: Os::Linux,
            home: Some(PathBuf::from("/home/alex")),
            ..Env::default()
        }
    }

    fn windows_suggestion(drives: Vec<DriveInfo>) -> StorageSuggestion {
        suggest(
            &windows(),
            &Fake(drives),
            Path::new(r"C:\Users\Alex\AppData\Roaming\VibeLauncher"),
        )
    }

    #[test]
    fn the_roomiest_other_drive_wins() {
        let s = windows_suggestion(vec![
            drive(r"C:\", 500, 300),
            drive(r"D:\", 1000, 200),
            drive(r"E:\", 2000, 900),
        ]);
        assert_eq!(s.reason, SuggestionReason::OtherDrive);
        assert_eq!(s.folder, PathBuf::from(r"E:\").join("VibeLauncher"));
        assert!(!s.on_system_drive && s.enough_space);
        assert!(s.current_on_system_drive);
        assert_eq!(s.drives.len(), 3);
        assert!(s.drives[0].system && !s.drives[1].system);
    }

    #[test]
    fn the_system_drive_is_not_chosen_even_when_it_is_the_roomiest() {
        let s = windows_suggestion(vec![drive(r"C:\", 2000, 1900), drive(r"D:\", 500, 100)]);
        assert_eq!(s.folder, PathBuf::from(r"D:\").join("VibeLauncher"));
    }

    #[test]
    fn a_single_drive_gives_the_os_folder() {
        let s = windows_suggestion(vec![drive(r"C:\", 500, 300)]);
        assert_eq!(s.reason, SuggestionReason::OsFolder);
        assert_eq!(
            s.folder,
            PathBuf::from(r"C:\Users\Alex\AppData\Roaming").join("VibeLauncher")
        );
        assert!(s.on_system_drive && s.current_on_system_drive);
        assert_eq!(s.drive.as_ref().map(|d| d.system), Some(true));
    }

    #[test]
    fn a_second_drive_below_the_threshold_is_not_enough() {
        let s = windows_suggestion(vec![drive(r"C:\", 500, 300), drive(r"D:\", 100, 20)]);
        assert_eq!(s.reason, SuggestionReason::OsFolder);
        let s = windows_suggestion(vec![drive(r"C:\", 500, 300), drive(r"D:\", 100, 21)]);
        assert_eq!(s.reason, SuggestionReason::OtherDrive);
    }

    #[test]
    fn removable_network_and_empty_drives_are_ignored() {
        let mut usb = drive(r"E:\", 500, 400);
        usb.removable = true;
        let mut share = drive(r"F:\", 500, 400);
        share.network = true;
        let s = windows_suggestion(vec![
            drive(r"C:\", 500, 300),
            usb,
            share,
            drive(r"G:\", 0, 0),
        ]);
        assert_eq!(s.reason, SuggestionReason::OsFolder);
        assert_eq!(s.drives.len(), 1);
    }

    #[test]
    fn the_system_drive_letter_is_found_without_case_and_prefix() {
        let mut env = windows();
        env.system_drive = None;
        env.system_root = Some(PathBuf::from(r"\\?\c:\Windows"));
        let s = suggest(
            &env,
            &Fake(vec![drive(r"C:\", 500, 300), drive(r"d:\", 500, 200)]),
            Path::new(r"d:\Games\VibeLauncher"),
        );
        assert!(s.drives[0].system);
        assert!(!s.current_on_system_drive);
        assert_eq!(s.folder, PathBuf::from(r"d:\").join("VibeLauncher"));
    }

    #[test]
    fn the_current_folder_on_another_drive_is_not_flagged() {
        let s = suggest(
            &windows(),
            &Fake(vec![drive(r"C:\", 500, 300), drive(r"D:\", 500, 200)]),
            Path::new(r"D:\VibeLauncher"),
        );
        assert!(!s.current_on_system_drive);
    }

    #[test]
    fn other_systems_get_the_os_folder_whatever_the_drives() {
        let s = suggest(
            &linux(),
            &Fake(vec![drive("/", 500, 300), drive("/mnt/data", 4000, 3000)]),
            Path::new("/home/alex/.local/share/vibelauncher"),
        );
        assert_eq!(s.reason, SuggestionReason::OsFolder);
        assert_eq!(
            s.folder,
            PathBuf::from("/home/alex/.local/share/vibelauncher")
        );
        assert!(s.on_system_drive);
    }

    #[test]
    fn a_separate_home_volume_is_not_the_system_drive() {
        let s = suggest(
            &linux(),
            &Fake(vec![drive("/", 100, 30), drive("/home", 4000, 3000)]),
            Path::new("/home/alex/.local/share/vibelauncher"),
        );
        assert!(!s.on_system_drive && !s.current_on_system_drive);
        assert_eq!(s.drive.map(|d| d.mount), Some(PathBuf::from("/home")));
    }

    #[test]
    fn little_free_space_on_the_suggested_drive_is_flagged() {
        let s = windows_suggestion(vec![drive(r"C:\", 100, 8)]);
        assert!(!s.enough_space);
    }

    #[test]
    fn no_drives_at_all_still_gives_a_folder() {
        let s = windows_suggestion(Vec::new());
        assert_eq!(s.reason, SuggestionReason::OsFolder);
        assert!(s.drive.is_none() && s.enough_space);
    }
}
