//! Choosing the data folder (the root).
//!
//! The order is: the `--dir` argument, the [`DIR_VAR`] environment variable, the portable marker
//! next to the executable, the pointer file in the OS folder for settings, the OS convention. The
//! choice is a function of an [`Env`] (the OS, the relevant environment variables and the
//! executable's directory), so every OS is testable on any host. [`Env::from_process`] reads the
//! real values.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use vl_core::platform::{Os, Platform};
use vl_core::storage::{RootSource, Section};

use crate::{StoreError, pointer};

/// Directory name on Windows and macOS.
pub const APP_DIR: &str = "VibeLauncher";
/// Directory name on Linux (XDG convention: lowercase).
pub const APP_DIR_XDG: &str = "vibelauncher";
/// A file with this name next to the executable switches on portable mode.
pub const PORTABLE_MARKER: &str = "vibelauncher.portable";
/// Directory next to the executable that holds the data in marker-file portable mode.
pub const PORTABLE_SUBDIR: &str = "data";
/// The environment variable that selects the data folder, like `--dir`.
pub const DIR_VAR: &str = "VIBELAUNCHER_DIR";

/// What directory selection and path checks depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The operating system.
    pub os: Os,
    /// The user's home directory (`HOME`, else `USERPROFILE`).
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
    /// `$XDG_CONFIG_HOME` (Linux).
    pub xdg_config_home: Option<PathBuf>,
    /// The directory of the running executable, where the portable marker is looked up.
    pub exe_dir: Option<PathBuf>,
    /// [`DIR_VAR`].
    pub dir_override: Option<PathBuf>,
    /// Folders for installed programs (`%ProgramFiles%`, `%ProgramFiles(x86)%`,
    /// `%ProgramW6432%`).
    pub program_dirs: Vec<PathBuf>,
    /// The Windows folder (`%SystemRoot%`).
    pub system_root: Option<PathBuf>,
    /// The system drive (`%SystemDrive%`, such as `C:`).
    pub system_drive: Option<PathBuf>,
    /// Cloud folders named by the environment (`%OneDrive%`, `%OneDriveConsumer%`,
    /// `%OneDriveCommercial%`).
    pub cloud_dirs: Vec<PathBuf>,
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
            xdg_config_home: None,
            exe_dir: None,
            dir_override: None,
            program_dirs: Vec::new(),
            system_root: None,
            system_drive: None,
            cloud_dirs: Vec::new(),
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
            xdg_config_home: var("XDG_CONFIG_HOME"),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            dir_override: var(DIR_VAR),
            program_dirs: ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
                .into_iter()
                .filter_map(var)
                .collect(),
            system_root: var("SystemRoot").or_else(|| var("windir")),
            system_drive: var("SystemDrive"),
            cloud_dirs: ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
                .into_iter()
                .filter_map(var)
                .collect(),
        }
    }

    fn need(value: &Option<PathBuf>, what: &str) -> Result<PathBuf, StoreError> {
        value
            .clone()
            .ok_or_else(|| StoreError::NoDirectory(format!("{what} is not set")))
    }

    /// An XDG base directory: the variable when it is absolute (a relative value is invalid by
    /// the specification), else `$HOME/<fallback>`.
    fn xdg(&self, value: &Option<PathBuf>, fallback: &str) -> Result<PathBuf, StoreError> {
        match value {
            Some(p) if p.is_absolute() => Ok(p.clone()),
            _ => Ok(Self::need(&self.home, "HOME")?.join(fallback)),
        }
    }
}

/// The chosen data folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// The folder.
    pub dir: PathBuf,
    /// What chose it.
    pub source: RootSource,
}

/// Chooses the data folder: `arg` (`--dir`) first, then [`DIR_VAR`], the marker file next to
/// the executable, the pointer file, the OS convention.
///
/// A pointer that names a folder which does not exist is an error, not a reason to start over
/// with an empty folder: the drive may only be disconnected.
pub fn resolve(env: &Env, arg: Option<&Path>) -> Result<Root, StoreError> {
    if let Some(dir) = arg {
        return Ok(Root {
            dir: explicit(dir, "--dir")?,
            source: RootSource::Argument,
        });
    }
    if let Some(dir) = &env.dir_override {
        return Ok(Root {
            dir: explicit(dir, DIR_VAR)?,
            source: RootSource::Environment,
        });
    }
    if let Some(exe_dir) = &env.exe_dir
        && exe_dir.join(PORTABLE_MARKER).is_file()
    {
        return Ok(Root {
            dir: exe_dir.join(PORTABLE_SUBDIR),
            source: RootSource::Portable,
        });
    }
    if let Some(file) = pointer_file(env)
        && let Some(target) = pointer::read(&file)?
    {
        if !target.root.is_dir() {
            return Err(StoreError::NoDirectory(format!(
                "the data folder {} named in {} is unavailable: reconnect its drive, or delete {} \
                 to start in the default folder",
                target.root.display(),
                file.display(),
                file.display()
            )));
        }
        return Ok(Root {
            dir: target.root,
            source: RootSource::Pointer,
        });
    }
    Ok(Root {
        dir: default_root(env)?,
        source: RootSource::Default,
    })
}

fn explicit(dir: &Path, what: &str) -> Result<PathBuf, StoreError> {
    if dir.as_os_str().is_empty() {
        return Err(StoreError::NoDirectory(format!("{what} is empty")));
    }
    std::path::absolute(dir)
        .map_err(|e| StoreError::NoDirectory(format!("{what} {}: {e}", dir.display())))
}

/// The data folder by OS convention.
pub fn default_root(env: &Env) -> Result<PathBuf, StoreError> {
    match env.os {
        Os::Windows => Ok(Env::need(&env.appdata, "%APPDATA%")?.join(APP_DIR)),
        Os::MacOs => Ok(Env::need(&env.home, "HOME")?
            .join("Library/Application Support")
            .join(APP_DIR)),
        // Linux and anything else follow the XDG base directory convention.
        Os::Linux | Os::Other => Ok(env
            .xdg(&env.xdg_data_home, ".local/share")?
            .join(APP_DIR_XDG)),
    }
}

/// The pointer file that names a data folder outside the OS convention; `None` when the
/// environment lacks the variables to place it.
pub fn pointer_file(env: &Env) -> Option<PathBuf> {
    let dir = match env.os {
        Os::Windows => env.local_appdata.as_ref()?.join(APP_DIR),
        Os::MacOs => env.home.as_ref()?.join("Library/Preferences").join(APP_DIR),
        Os::Linux | Os::Other => env
            .xdg(&env.xdg_config_home, ".config")
            .ok()?
            .join(APP_DIR_XDG),
    };
    Some(dir.join(pointer::POINTER_FILE))
}

/// Where earlier versions kept the cache and logs outside the data folder; listed for cleanup.
pub fn legacy_dirs(env: &Env) -> Vec<(Section, PathBuf)> {
    let mut dirs = Vec::new();
    match env.os {
        Os::Windows => {
            if let Some(local) = &env.local_appdata {
                dirs.push((Section::Cache, local.join(APP_DIR).join("cache")));
            }
        }
        Os::MacOs => {
            if let Some(home) = &env.home {
                dirs.push((Section::Cache, home.join("Library/Caches").join(APP_DIR)));
                dirs.push((Section::Logs, home.join("Library/Logs").join(APP_DIR)));
            }
        }
        Os::Linux | Os::Other => {
            if let Ok(cache) = env.xdg(&env.xdg_cache_home, ".cache") {
                dirs.push((Section::Cache, cache.join(APP_DIR_XDG)));
            }
            if let Ok(state) = env.xdg(&env.xdg_state_home, ".local/state") {
                dirs.push((Section::Logs, state.join(APP_DIR_XDG)));
            }
        }
    }
    dirs
}

/// `path` made absolute with links resolved, as the launcher stores it. A part that does not exist
/// yet is appended to the resolved part that does. On Windows the `\\?\` prefix of a resolved path
/// is dropped again, so the path stays readable by other programs (Java among them).
pub fn normalize(path: &Path) -> Result<PathBuf, StoreError> {
    let absolute = std::path::absolute(path).map_err(|e| StoreError::io(path, e))?;
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(resolved) => {
                let mut out = simplify(resolved);
                for part in rest.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(_) => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Ok(absolute);
                };
                rest.push(name.to_os_string());
                existing = parent;
            }
        }
    }
}

/// Drops the Windows verbatim prefix: `\\?\C:\x` is `C:\x`, `\\?\UNC\server\share` is
/// `\\server\share`.
pub fn simplify(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Whether `path` is `base` or inside it, compared by components (without case on Windows).
pub fn is_within(path: &Path, base: &Path) -> bool {
    let fold = |c: Component<'_>| {
        let text = c.as_os_str().to_string_lossy();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text.into_owned()
        }
    };
    let mut path = path.components().map(fold);
    base.components()
        .map(fold)
        .all(|b| path.next().is_some_and(|p| p == b))
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
    fn windows_uses_appdata_and_keeps_the_pointer_in_local_appdata() {
        let e = Env {
            appdata: Some(abs("roaming")),
            local_appdata: Some(abs("local")),
            ..env(Os::Windows)
        };
        let root = resolve(&e, None).unwrap();
        assert_eq!(root.dir, abs("roaming").join("VibeLauncher"));
        assert_eq!(root.source, RootSource::Default);
        assert_eq!(
            pointer_file(&e).unwrap(),
            abs("local").join("VibeLauncher").join("root.json")
        );
        assert_eq!(
            legacy_dirs(&e),
            [(
                Section::Cache,
                abs("local").join("VibeLauncher").join("cache")
            )]
        );
    }

    #[test]
    fn macos_uses_application_support() {
        let e = Env {
            home: Some(abs("home")),
            ..env(Os::MacOs)
        };
        let home = abs("home");
        assert_eq!(
            resolve(&e, None).unwrap().dir,
            home.join("Library/Application Support/VibeLauncher")
        );
        assert_eq!(
            pointer_file(&e).unwrap(),
            home.join("Library/Preferences/VibeLauncher/root.json")
        );
    }

    #[test]
    fn linux_uses_xdg_variables_and_ignores_relative_ones() {
        let e = Env {
            home: Some(abs("home")),
            xdg_data_home: Some(abs("xd")),
            xdg_config_home: Some(PathBuf::from("relative")),
            ..env(Os::Linux)
        };
        assert_eq!(
            resolve(&e, None).unwrap().dir,
            abs("xd").join("vibelauncher")
        );
        assert_eq!(
            pointer_file(&e).unwrap(),
            abs("home").join(".config/vibelauncher/root.json")
        );
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
    fn argument_beats_variable_beats_marker_beats_pointer() {
        let sandbox = TempDir::new();
        let exe_dir = sandbox.path().join("exe");
        let config = sandbox.path().join("config");
        let pointed = sandbox.path().join("pointed");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&pointed).unwrap();
        let mut e = Env {
            home: Some(sandbox.path().join("home")),
            xdg_config_home: Some(config.clone()),
            exe_dir: Some(exe_dir.clone()),
            ..env(Os::Linux)
        };
        let file = pointer_file(&e).unwrap();
        assert_eq!(resolve(&e, None).unwrap().source, RootSource::Default);

        pointer::write(&file, &pointed, None).unwrap();
        let root = resolve(&e, None).unwrap();
        assert_eq!(
            (root.dir, root.source),
            (pointed.clone(), RootSource::Pointer)
        );

        std::fs::write(exe_dir.join(PORTABLE_MARKER), b"").unwrap();
        let root = resolve(&e, None).unwrap();
        assert_eq!(
            (root.dir, root.source),
            (exe_dir.join("data"), RootSource::Portable)
        );

        e.dir_override = Some(sandbox.path().join("from-var"));
        let root = resolve(&e, None).unwrap();
        assert_eq!(
            (root.dir, root.source),
            (sandbox.path().join("from-var"), RootSource::Environment)
        );

        let arg = sandbox.path().join("from-arg");
        let root = resolve(&e, Some(&arg)).unwrap();
        assert_eq!((root.dir, root.source), (arg, RootSource::Argument));
        assert!(matches!(
            resolve(&e, Some(Path::new(""))),
            Err(StoreError::NoDirectory(_))
        ));
    }

    #[test]
    fn a_pointer_to_a_missing_folder_is_an_error_not_a_fresh_start() {
        let sandbox = TempDir::new();
        let e = Env {
            home: Some(sandbox.path().join("home")),
            xdg_config_home: Some(sandbox.path().join("config")),
            ..env(Os::Linux)
        };
        let file = pointer_file(&e).unwrap();
        pointer::write(&file, &sandbox.path().join("unplugged"), None).unwrap();
        let err = resolve(&e, None).unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        std::fs::write(&file, b"{ not json").unwrap();
        assert!(matches!(resolve(&e, None), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(
            simplify(PathBuf::from(r"\\?\C:\Games\VL")),
            PathBuf::from(r"C:\Games\VL")
        );
        assert_eq!(
            simplify(PathBuf::from(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        assert_eq!(simplify(PathBuf::from("/home/a")), PathBuf::from("/home/a"));
    }

    #[test]
    fn normalize_resolves_the_existing_part_and_keeps_the_rest() {
        let sandbox = TempDir::new();
        let base = normalize(sandbox.path()).unwrap();
        let path = sandbox.path().join("\u{418}\u{433}\u{440}\u{44b} \u{441} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b}\u{43e}\u{43c}").join("new");
        let normalized = normalize(&path).unwrap();
        assert_eq!(normalized, base.join("\u{418}\u{433}\u{440}\u{44b} \u{441} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b}\u{43e}\u{43c}").join("new"));
        assert!(!normalized.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn within_compares_whole_components() {
        let base = abs("a");
        assert!(is_within(&base.join("b"), &base));
        assert!(is_within(&base, &base));
        assert!(!is_within(&abs("ab"), &base));
        assert!(!is_within(&base, &base.join("b")));
    }
}
