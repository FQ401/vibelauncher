//! Choosing the data, cache and log directories.
//!
//! The choice is a pure function of an [`Env`] (the OS, the relevant environment variables and the
//! executable's directory), so every OS is testable on any host. [`Env::from_process`] reads the
//! real values.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vl_core::platform::{Os, Platform};

use crate::StoreError;

/// Directory name on Windows and macOS.
pub const APP_DIR: &str = "VibeLauncher";
/// Directory name on Linux (XDG convention: lowercase).
pub const APP_DIR_XDG: &str = "vibelauncher";
/// A file with this name next to the executable switches on portable mode.
pub const PORTABLE_MARKER: &str = "vibelauncher.portable";
/// Directory next to the executable that holds the data in marker-file portable mode.
pub const PORTABLE_SUBDIR: &str = "data";

/// The three directories the launcher uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Settings, instances, shared game files, Java runtimes, metadata cache, accounts.
    pub data: PathBuf,
    /// Files that can be deleted at any time.
    pub cache: PathBuf,
    /// Launcher logs.
    pub logs: PathBuf,
    /// Everything lives under one directory chosen by the user.
    pub portable: bool,
}

/// What directory selection depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The operating system.
    pub os: Os,
    /// The user's home directory (`HOME`, `USERPROFILE` is not needed on Windows).
    pub home: Option<PathBuf>,
    /// `%APPDATA%` (Windows).
    pub appdata: Option<PathBuf>,
    /// `%LOCALAPPDATA%` (Windows).
    pub local_appdata: Option<PathBuf>,
    /// `$XDG_DATA_HOME` (Linux).
    pub xdg_data_home: Option<PathBuf>,
    /// `$XDG_CACHE_HOME` (Linux).
    pub xdg_cache_home: Option<PathBuf>,
    /// `$XDG_STATE_HOME` (Linux).
    pub xdg_state_home: Option<PathBuf>,
    /// The directory of the running executable, where the portable marker is looked up.
    pub exe_dir: Option<PathBuf>,
}

impl Default for Env {
    /// An environment with nothing set, on the OS this was compiled for.
    fn default() -> Self {
        Self {
            os: Platform::current().os,
            home: None,
            appdata: None,
            local_appdata: None,
            xdg_data_home: None,
            xdg_cache_home: None,
            xdg_state_home: None,
            exe_dir: None,
        }
    }
}

impl Env {
    /// Reads the real environment of this process.
    pub fn from_process() -> Self {
        fn var(name: &str) -> Option<PathBuf> {
            let value: OsString = std::env::var_os(name)?;
            (!value.is_empty()).then(|| PathBuf::from(value))
        }
        Self {
            os: Platform::current().os,
            home: var("HOME").or_else(|| var("USERPROFILE")),
            appdata: var("APPDATA"),
            local_appdata: var("LOCALAPPDATA"),
            xdg_data_home: var("XDG_DATA_HOME"),
            xdg_cache_home: var("XDG_CACHE_HOME"),
            xdg_state_home: var("XDG_STATE_HOME"),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
        }
    }
}

/// Chooses the directories: `portable_dir` (`--dir`) first, then the marker file next to the
/// executable, then the OS conventions.
pub fn resolve(env: &Env, portable_dir: Option<&Path>) -> Result<Dirs, StoreError> {
    if let Some(dir) = portable_dir {
        if dir.as_os_str().is_empty() {
            return Err(StoreError::NoDirectory("--dir is empty".into()));
        }
        let root = std::path::absolute(dir)
            .map_err(|e| StoreError::NoDirectory(format!("--dir {}: {e}", dir.display())))?;
        return Ok(portable(root));
    }
    if let Some(exe_dir) = &env.exe_dir
        && exe_dir.join(PORTABLE_MARKER).is_file()
    {
        return Ok(portable(exe_dir.join(PORTABLE_SUBDIR)));
    }
    os_dirs(env)
}

fn portable(root: PathBuf) -> Dirs {
    Dirs {
        cache: root.join("cache"),
        logs: root.join("logs"),
        data: root,
        portable: true,
    }
}

fn os_dirs(env: &Env) -> Result<Dirs, StoreError> {
    let need = |value: &Option<PathBuf>, what: &str| {
        value
            .clone()
            .ok_or_else(|| StoreError::NoDirectory(format!("{what} is not set")))
    };
    match env.os {
        Os::Windows => {
            let data = need(&env.appdata, "%APPDATA%")?.join(APP_DIR);
            let cache = need(&env.local_appdata, "%LOCALAPPDATA%")?
                .join(APP_DIR)
                .join("cache");
            Ok(Dirs {
                logs: data.join("logs"),
                data,
                cache,
                portable: false,
            })
        }
        Os::MacOs => {
            let home = need(&env.home, "HOME")?;
            Ok(Dirs {
                data: home.join("Library/Application Support").join(APP_DIR),
                cache: home.join("Library/Caches").join(APP_DIR),
                logs: home.join("Library/Logs").join(APP_DIR),
                portable: false,
            })
        }
        // Linux and anything else follow the XDG base directory convention.
        Os::Linux | Os::Other => {
            // A relative XDG value is invalid by the specification and is ignored.
            let xdg = |value: &Option<PathBuf>, fallback: &str| -> Result<PathBuf, StoreError> {
                match value {
                    Some(p) if p.is_absolute() => Ok(p.clone()),
                    _ => Ok(need(&env.home, "HOME")?.join(fallback)),
                }
            };
            Ok(Dirs {
                data: xdg(&env.xdg_data_home, ".local/share")?.join(APP_DIR_XDG),
                cache: xdg(&env.xdg_cache_home, ".cache")?.join(APP_DIR_XDG),
                logs: xdg(&env.xdg_state_home, ".local/state")?
                    .join(APP_DIR_XDG)
                    .join("logs"),
                portable: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn env(os: Os) -> Env {
        Env {
            os,
            ..Env::default()
        }
    }

    fn abs(p: &str) -> PathBuf {
        std::env::temp_dir().join(p)
    }

    #[test]
    fn windows_uses_appdata_and_local_appdata() {
        let e = Env {
            appdata: Some(abs("roaming")),
            local_appdata: Some(abs("local")),
            ..env(Os::Windows)
        };
        let d = resolve(&e, None).unwrap();
        assert_eq!(d.data, abs("roaming").join("VibeLauncher"));
        assert_eq!(d.cache, abs("local").join("VibeLauncher").join("cache"));
        assert_eq!(d.logs, d.data.join("logs"));
        assert!(!d.portable);
    }

    #[test]
    fn macos_uses_library_directories() {
        let e = Env {
            home: Some(abs("home")),
            ..env(Os::MacOs)
        };
        let d = resolve(&e, None).unwrap();
        let home = abs("home");
        assert_eq!(
            d.data,
            home.join("Library/Application Support/VibeLauncher")
        );
        assert_eq!(d.cache, home.join("Library/Caches/VibeLauncher"));
        assert_eq!(d.logs, home.join("Library/Logs/VibeLauncher"));
    }

    #[test]
    fn linux_uses_xdg_variables() {
        let e = Env {
            home: Some(abs("home")),
            xdg_data_home: Some(abs("xd")),
            xdg_cache_home: Some(abs("xc")),
            xdg_state_home: Some(abs("xs")),
            ..env(Os::Linux)
        };
        let d = resolve(&e, None).unwrap();
        assert_eq!(d.data, abs("xd").join("vibelauncher"));
        assert_eq!(d.cache, abs("xc").join("vibelauncher"));
        assert_eq!(d.logs, abs("xs").join("vibelauncher").join("logs"));
    }

    #[test]
    fn linux_falls_back_to_home_and_ignores_relative_xdg_values() {
        let e = Env {
            home: Some(abs("home")),
            xdg_data_home: Some(PathBuf::from("relative/data")),
            ..env(Os::Linux)
        };
        let d = resolve(&e, None).unwrap();
        let home = abs("home");
        assert_eq!(d.data, home.join(".local/share/vibelauncher"));
        assert_eq!(d.cache, home.join(".cache/vibelauncher"));
        assert_eq!(d.logs, home.join(".local/state/vibelauncher/logs"));
    }

    #[test]
    fn missing_variables_are_an_error() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert!(
                matches!(resolve(&env(os), None), Err(StoreError::NoDirectory(_))),
                "{os:?}"
            );
        }
    }

    #[test]
    fn dir_option_puts_everything_under_one_directory() {
        let root = abs("portable-root");
        let d = resolve(&env(Os::Windows), Some(&root)).unwrap();
        assert_eq!(d.data, root);
        assert_eq!(d.cache, root.join("cache"));
        assert_eq!(d.logs, root.join("logs"));
        assert!(d.portable);
        assert!(matches!(
            resolve(&env(Os::Windows), Some(Path::new(""))),
            Err(StoreError::NoDirectory(_))
        ));
    }

    #[test]
    fn marker_file_next_to_the_executable_switches_on_portable_mode() {
        let exe_dir = TempDir::new();
        let e = Env {
            exe_dir: Some(exe_dir.path().to_path_buf()),
            home: Some(abs("home")),
            ..env(Os::Linux)
        };
        assert!(!resolve(&e, None).unwrap().portable);
        std::fs::write(exe_dir.path().join(PORTABLE_MARKER), b"").unwrap();
        let d = resolve(&e, None).unwrap();
        assert!(d.portable);
        assert_eq!(d.data, exe_dir.path().join("data"));
    }

    #[test]
    fn dir_option_beats_the_marker_file() {
        let exe_dir = TempDir::new();
        std::fs::write(exe_dir.path().join(PORTABLE_MARKER), b"").unwrap();
        let e = Env {
            exe_dir: Some(exe_dir.path().to_path_buf()),
            ..env(Os::Linux)
        };
        let root = abs("explicit");
        assert_eq!(resolve(&e, Some(&root)).unwrap().data, root);
    }
}
