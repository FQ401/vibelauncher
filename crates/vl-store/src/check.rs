//! Checking a folder before the launcher stores a section in it.
//!
//! Errors: the folder cannot be written, it is inside Program Files or an OS folder, it is the
//! root of the system drive, it overlaps another section, there is not enough space for a copy.
//! Warnings: a cloud-synchronized folder (by name, by the `OneDrive` variables, and on Windows by
//! the Files On-Demand attributes), a network or removable drive, a long path or characters
//! outside ASCII (Windows), little free space.

use std::fs;
use std::path::{Component, Path, PathBuf};

use vl_core::platform::Os;
use vl_core::storage::{MoveMode, PathCheck, PathIssue, Section};

use crate::layout::{GAME_ENTRIES, Layout, owned};
use crate::paths::{Env, is_within, normalize};
use crate::{format, write_atomic};

/// A data folder longer than this many characters is reported: files deep inside an instance
/// (`instances/<name>/minecraft/mods/<file>.jar`, config trees) can then pass the Windows limit
/// of 260 characters.
pub const LONG_PATH_LIMIT: usize = 100;
/// Less free space than this on the drive is reported.
pub const LOW_SPACE: u64 = 5 << 30;

/// Folder names of cloud services, compared without case; a name ending in `*` is a prefix.
const CLOUD_NAMES: &[(&str, &str)] = &[
    ("onedrive", "OneDrive"),
    ("onedrive - *", "OneDrive"),
    ("dropbox", "Dropbox"),
    ("google drive", "Google Drive"),
    ("googledrive", "Google Drive"),
    ("my drive", "Google Drive"),
    ("icloud drive", "iCloud Drive"),
    ("iclouddrive", "iCloud Drive"),
    ("mobile documents", "iCloud Drive"),
    ("cloudstorage", "a cloud storage provider"),
    ("yandexdisk", "Yandex Disk"),
    ("yandex.disk", "Yandex Disk"),
    ("pcloud drive", "pCloud"),
    ("nextcloud", "Nextcloud"),
];

/// File systems reached over a network.
const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smbfs",
    "smb2",
    "smb3",
    "afpfs",
    "sshfs",
    "fuse.sshfs",
    "9p",
    "davfs",
    "webdav",
    "fuse.rclone",
];

/// Checks `path` as the new folder of `section` in `layout`.
///
/// `mode` decides whether the folder must be empty (a copy) or must already hold the data (a
/// switch); `needed` is the space a copy needs.
pub fn check_path(
    env: &Env,
    layout: &Layout,
    section: Section,
    path: &Path,
    mode: Option<MoveMode>,
    needed: Option<u64>,
) -> PathCheck {
    let path = match normalize(path) {
        Ok(p) => p,
        Err(e) => {
            return PathCheck {
                path: path.to_path_buf(),
                issues: vec![PathIssue::NotWritable {
                    detail: e.to_string(),
                }],
                free: None,
            };
        }
    };
    let mut issues = static_issues(env, &path);
    issues.extend(overlaps(layout, section, &path));
    match mode {
        Some(MoveMode::Copy) if !is_empty_for(section, &path) => issues.push(PathIssue::NotEmpty),
        Some(MoveMode::SwitchOnly) if !holds_data(section, &path) => {
            issues.push(PathIssue::Missing);
        }
        _ => {}
    }
    if let Err(detail) = probe_writable(&path) {
        issues.push(PathIssue::NotWritable { detail });
    }
    if cloud_attributes(&path)
        && !issues
            .iter()
            .any(|i| matches!(i, PathIssue::CloudFolder { .. }))
    {
        issues.push(PathIssue::CloudFolder {
            service: "OneDrive".into(),
        });
    }
    let disk = disk_of(&path);
    let free = disk.as_ref().map(|d| d.free);
    if let Some(disk) = &disk {
        if disk.removable {
            issues.push(PathIssue::RemovableDrive);
        }
        if disk.network && !issues.contains(&PathIssue::NetworkDrive) {
            issues.push(PathIssue::NetworkDrive);
        }
        match needed {
            Some(needed) if disk.free < needed => issues.push(PathIssue::NotEnoughSpace {
                free: disk.free,
                needed,
            }),
            _ if disk.free < LOW_SPACE => issues.push(PathIssue::LowSpace { free: disk.free }),
            _ => {}
        }
    }
    issues.sort_by_key(|i| !i.is_error());
    PathCheck { path, issues, free }
}

/// The issues that follow from the path text and the environment alone.
pub fn static_issues(env: &Env, path: &Path) -> Vec<PathIssue> {
    let mut issues = Vec::new();
    if env.program_dirs.iter().any(|p| is_within(path, p)) {
        issues.push(PathIssue::ProgramFiles);
    }
    if is_system_folder(env, path) {
        issues.push(PathIssue::SystemFolder);
    }
    if is_system_drive_root(env, path) {
        issues.push(PathIssue::SystemDriveRoot);
    }
    if let Some(service) = cloud_service(env, path) {
        issues.push(PathIssue::CloudFolder { service });
    }
    if is_unc(path) {
        issues.push(PathIssue::NetworkDrive);
    }
    if env.os == Os::Windows {
        let length = path.as_os_str().to_string_lossy().chars().count();
        if length > LONG_PATH_LIMIT {
            issues.push(PathIssue::LongPath {
                length,
                limit: LONG_PATH_LIMIT,
            });
        }
        if !path.as_os_str().to_string_lossy().is_ascii() {
            issues.push(PathIssue::NonAscii);
        }
    }
    issues
}

fn is_system_folder(env: &Env, path: &Path) -> bool {
    match env.os {
        Os::Windows => env.system_root.as_ref().is_some_and(|r| is_within(path, r)),
        Os::MacOs | Os::Linux | Os::Other => {
            const SYSTEM: &[&str] = &[
                "/bin", "/boot", "/dev", "/etc", "/lib", "/proc", "/sbin", "/sys", "/usr",
                "/System",
            ];
            SYSTEM.iter().any(|s| is_within(path, Path::new(s)))
        }
    }
}

fn is_system_drive_root(env: &Env, path: &Path) -> bool {
    let is_root = path.parent().is_none();
    match env.os {
        Os::Windows => {
            let drive = env.system_drive.as_ref().map(|d| {
                d.to_string_lossy()
                    .trim_end_matches(['\\', '/'])
                    .to_lowercase()
            });
            let Some(drive) = drive else {
                return false;
            };
            let mut parts = path.components();
            let prefix = parts
                .next()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase());
            let rest: Vec<_> = parts.filter(|c| *c != Component::RootDir).collect();
            rest.is_empty() && prefix.as_deref() == Some(drive.as_str())
        }
        _ => is_root,
    }
}

fn cloud_service(env: &Env, path: &Path) -> Option<String> {
    if env.cloud_dirs.iter().any(|d| is_within(path, d)) {
        return Some("OneDrive".into());
    }
    path.components().find_map(|c| {
        let Component::Normal(name) = c else {
            return None;
        };
        let name = name.to_string_lossy().to_lowercase();
        CLOUD_NAMES.iter().find_map(|(pattern, service)| {
            let hit = match pattern.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == *pattern,
            };
            hit.then(|| (*service).to_owned())
        })
    })
}

fn is_unc(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    text.starts_with(r"\\") && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\")
        || text.starts_with(r"\\?\UNC\")
}

/// Whether a copy of `section` can go into `path`: it does not exist or is empty (for the shared
/// game files: none of their directories exist in it).
fn is_empty_for(section: Section, path: &Path) -> bool {
    match section {
        Section::Game => GAME_ENTRIES.iter().all(|e| !path.join(e).exists()),
        _ => match fs::read_dir(path) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => !path.exists(),
        },
    }
}

/// Whether `path` already holds `section` for a switch: it exists, and a data folder has its
/// format file.
fn holds_data(section: Section, path: &Path) -> bool {
    match section {
        Section::Data => path.join(format::FORMAT_FILE).is_file(),
        _ => path.is_dir(),
    }
}

fn overlaps(layout: &Layout, section: Section, path: &Path) -> Vec<PathIssue> {
    let mut issues = Vec::new();
    for other in Section::ALL {
        let hit = if other == Section::Data {
            // The data folder holds the default sections; only covering it is a problem. A new
            // data folder must not be inside the current one.
            (section != Section::Game && is_within(&layout.root, path))
                || (section == Section::Data && is_within(path, &layout.root))
        } else if other == section {
            layout
                .owned(other)
                .iter()
                .any(|o| is_within(path, o) || (section != Section::Game && is_within(o, path)))
        } else if section == Section::Data && layout.is_default(other) {
            false
        } else {
            let mine = owned(path, section);
            layout
                .owned(other)
                .iter()
                .any(|o| mine.iter().any(|m| is_within(m, o) || is_within(o, m)))
        };
        if hit {
            issues.push(PathIssue::Overlaps { section: other });
        }
    }
    issues
}

/// Creates what is missing of `path`, writes and removes a probe file, then removes what it
/// created.
fn probe_writable(path: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut cursor = Some(path);
    while let Some(dir) = cursor {
        if dir.exists() {
            break;
        }
        missing.push(dir.to_path_buf());
        cursor = dir.parent();
    }
    let result = fs::create_dir_all(path)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            let probe = path.join(".vl-write-probe");
            write_atomic(&probe, b"probe").map_err(|e| e.to_string())?;
            fs::remove_file(&probe).map_err(|e| e.to_string())
        });
    for dir in missing {
        let _ = fs::remove_dir(dir);
    }
    result
}

/// The nearest existing ancestor of `path` (itself included).
fn existing(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|p| p.exists()).map(Path::to_path_buf)
}

/// Whether the folder carries the Windows cloud placeholder attributes (Files On-Demand).
#[cfg(windows)]
fn cloud_attributes(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const PINNED: u32 = 0x0008_0000;
    const UNPINNED: u32 = 0x0010_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    existing(path)
        .and_then(|p| fs::metadata(p).ok())
        .is_some_and(|m| {
            m.file_attributes() & (RECALL_ON_OPEN | PINNED | UNPINNED | RECALL_ON_DATA_ACCESS) != 0
        })
}

#[cfg(not(windows))]
fn cloud_attributes(_path: &Path) -> bool {
    false
}

/// The drive a folder is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    /// Free bytes.
    pub free: u64,
    /// A removable drive.
    pub removable: bool,
    /// A network file system.
    pub network: bool,
}

/// The drive of `path`: the mounted disk with the longest mount point that contains it.
pub fn disk_of(path: &Path) -> Option<DiskInfo> {
    let path = existing(path)?;
    let path = normalize(&path).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| is_within(&path, d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| {
            let fs_name = d.file_system().to_string_lossy().to_lowercase();
            DiskInfo {
                free: d.available_space(),
                removable: d.is_removable(),
                network: NETWORK_FS.contains(&fs_name.as_str()),
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::StorageConfig;
    use crate::testutil::TempDir;
    use vl_core::storage::RootSource;

    #[cfg(windows)]
    fn windows() -> Env {
        Env {
            os: Os::Windows,
            program_dirs: vec![PathBuf::from(r"C:\Program Files")],
            system_root: Some(PathBuf::from(r"C:\Windows")),
            system_drive: Some(PathBuf::from("C:")),
            cloud_dirs: vec![PathBuf::from(r"C:\Users\Ann\OneDrive")],
            ..Env::default()
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_errors_and_warnings_from_the_path() {
        let e = windows();
        let issues = |p: &str| static_issues(&e, Path::new(p));
        assert_eq!(issues(r"C:\Program Files\VL"), [PathIssue::ProgramFiles]);
        assert_eq!(issues(r"c:\program files\vl"), [PathIssue::ProgramFiles]);
        assert_eq!(issues(r"C:\Windows\VL"), [PathIssue::SystemFolder]);
        assert_eq!(issues(r"C:\"), [PathIssue::SystemDriveRoot]);
        assert!(issues(r"D:\").is_empty());
        assert!(issues(r"D:\Games\VibeLauncher").is_empty());
        assert_eq!(
            issues(r"C:\Users\Ann\OneDrive\Games"),
            [PathIssue::CloudFolder {
                service: "OneDrive".into()
            }]
        );
        assert_eq!(
            issues(r"D:\OneDrive - Contoso\VL"),
            [PathIssue::CloudFolder {
                service: "OneDrive".into()
            }]
        );
        assert_eq!(issues(r"\\nas\games\VL"), [PathIssue::NetworkDrive]);
        assert_eq!(
            issues("D:\\\u{418}\u{433}\u{440}\u{44b}\\VL"),
            [PathIssue::NonAscii]
        );
        let long = format!(r"D:\{}", "a".repeat(120));
        assert!(matches!(
            issues(&long)[..],
            [PathIssue::LongPath { length: 123, .. }]
        ));
    }

    #[test]
    fn unix_system_folders_and_root_are_errors() {
        let e = Env {
            os: Os::Linux,
            ..Env::default()
        };
        let issues = |p: &str| static_issues(&e, Path::new(p));
        assert_eq!(issues("/usr/share/vl"), [PathIssue::SystemFolder]);
        assert_eq!(issues("/"), [PathIssue::SystemDriveRoot]);
        assert!(issues("/home/ann/\u{418}\u{433}\u{440}\u{44b} \u{441} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b}\u{43e}\u{43c}").is_empty());
        assert_eq!(
            issues("/home/ann/Dropbox/vl"),
            [PathIssue::CloudFolder {
                service: "Dropbox".into()
            }]
        );
    }

    fn layout(root: &Path) -> Layout {
        Layout::new(
            root.to_path_buf(),
            RootSource::Argument,
            StorageConfig::default(),
        )
    }

    #[test]
    fn a_copy_needs_an_empty_folder_and_a_switch_an_existing_one() {
        let sandbox = TempDir::new();
        let root = normalize(&sandbox.path().join("root")).unwrap();
        fs::create_dir_all(&root).unwrap();
        let l = layout(&root);
        let env = Env::default();
        let target = sandbox.path().join("\u{41d}\u{43e}\u{432}\u{430}\u{44f} \u{43f}\u{430}\u{43f}\u{43a}\u{430} \u{441} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b}\u{430}\u{43c}\u{438}");
        let check = check_path(
            &env,
            &l,
            Section::Instances,
            &target,
            Some(MoveMode::Copy),
            None,
        );
        assert!(!check.issues.iter().any(PathIssue::is_error), "{check:?}");
        assert!(!target.exists(), "the probe leaves nothing behind");

        let check = check_path(
            &env,
            &l,
            Section::Instances,
            &target,
            Some(MoveMode::SwitchOnly),
            None,
        );
        assert!(check.issues.contains(&PathIssue::Missing), "{check:?}");

        fs::create_dir_all(target.join("x")).unwrap();
        let check = check_path(
            &env,
            &l,
            Section::Instances,
            &target,
            Some(MoveMode::Copy),
            None,
        );
        assert!(check.issues.contains(&PathIssue::NotEmpty), "{check:?}");
        let check = check_path(
            &env,
            &l,
            Section::Instances,
            &target,
            Some(MoveMode::SwitchOnly),
            None,
        );
        assert!(check.usable(), "{check:?}");
    }

    #[test]
    fn sections_must_not_overlap() {
        let sandbox = TempDir::new();
        let root = normalize(&sandbox.path().join("root")).unwrap();
        fs::create_dir_all(&root).unwrap();
        let l = layout(&root);
        let env = Env::default();
        let overlaps = |section, path: PathBuf| {
            check_path(&env, &l, section, &path, None, None)
                .issues
                .into_iter()
                .filter_map(|i| match i {
                    PathIssue::Overlaps { section } => Some(section),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            overlaps(Section::Instances, root.join("java")),
            [Section::Java]
        );
        assert_eq!(
            overlaps(Section::Instances, root.join("instances").join("x")),
            [Section::Instances]
        );
        assert!(overlaps(Section::Cache, root.clone()).contains(&Section::Data));
        assert!(overlaps(Section::Game, root.clone()).is_empty());
        assert!(overlaps(Section::Instances, root.join("more-instances")).is_empty());
        assert_eq!(overlaps(Section::Data, root.join("inner")), [Section::Data]);
        assert!(overlaps(Section::Data, sandbox.path().join("elsewhere")).is_empty());
    }

    #[test]
    fn the_drive_of_a_folder_is_found() {
        let sandbox = TempDir::new();
        let disk = disk_of(sandbox.path()).expect("the temp folder is on a disk");
        assert!(disk.free > 0);
    }
}
