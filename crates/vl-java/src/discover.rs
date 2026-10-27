//! Finding Java executables that may exist on this machine. Nothing here starts a Java; every
//! result is a candidate for [`crate::probe`].

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vl_core::platform::{Os, Platform};

use crate::run::run_capped;

/// How deep below a root a Java home may sit (`runtime/<name>/<platform>/<name>/bin/java`, or
/// `x.jdk/Contents/Home/bin/java` on macOS).
const MAX_DEPTH: usize = 5;
/// How many directory entries one root may cost.
const MAX_ENTRIES: usize = 4000;
/// Timeout of the helper programs (`reg`, `java_home`).
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// Output cap of the helper programs.
const HELPER_CAP: usize = 1024 * 1024;

/// A Java executable that may be usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The `java` executable.
    pub path: PathBuf,
    /// Whether it lives in the launcher's own runtime directory.
    pub managed: bool,
}

/// The parts of the process environment that discovery reads.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// The operating system to look on.
    pub os: Option<Os>,
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// `JAVA_HOME`, `JDK_HOME` and `JRE_HOME`, in that order, when set.
    pub java_homes: Vec<PathBuf>,
    /// The directories of `PATH`.
    pub path: Vec<PathBuf>,
    /// `ProgramFiles`, `ProgramFiles(x86)` and `ProgramW6432` (Windows).
    pub program_files: Vec<PathBuf>,
    /// `LOCALAPPDATA` (Windows).
    pub local_app_data: Option<PathBuf>,
    /// `APPDATA` (Windows).
    pub app_data: Option<PathBuf>,
    /// `XDG_DATA_HOME`, when set (Linux).
    pub xdg_data: Option<PathBuf>,
}

impl Env {
    /// Reads the environment of this process.
    pub fn from_process() -> Self {
        let var = |name: &str| {
            env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = var("HOME").or_else(|| var("USERPROFILE"));
        let mut program_files = Vec::new();
        for name in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(p) = var(name)
                && !program_files.contains(&p)
            {
                program_files.push(p);
            }
        }
        Self {
            os: Some(Platform::current().os),
            home,
            java_homes: ["JAVA_HOME", "JDK_HOME", "JRE_HOME"]
                .iter()
                .filter_map(|n| var(n))
                .collect(),
            path: env::var_os("PATH")
                .map(|p| env::split_paths(&p).collect())
                .unwrap_or_default(),
            program_files,
            local_app_data: var("LOCALAPPDATA"),
            app_data: var("APPDATA"),
            xdg_data: var("XDG_DATA_HOME"),
        }
    }

    fn os(&self) -> Os {
        self.os.unwrap_or(Os::Other)
    }
}

/// The name of the Java executable on `os`.
pub fn exe_name(os: Os) -> &'static str {
    if os == Os::Windows {
        "java.exe"
    } else {
        "java"
    }
}

/// `home/bin/java`, when it is a file.
pub fn home_exe(home: &Path, os: Os) -> Option<PathBuf> {
    let exe = home.join("bin").join(exe_name(os));
    exe.is_file().then_some(exe)
}

/// Finds Java homes at or below `root` (at most [`MAX_DEPTH`] levels, [`MAX_ENTRIES`] entries)
/// and pushes their executables. A directory that is a Java home is not searched further.
pub fn walk_homes(root: &Path, os: Os, out: &mut Vec<PathBuf>) {
    let mut budget = MAX_ENTRIES;
    walk(root, os, MAX_DEPTH, &mut budget, out);
}

fn walk(dir: &Path, os: Os, depth: usize, budget: &mut usize, out: &mut Vec<PathBuf>) {
    if let Some(exe) = home_exe(dir, os) {
        out.push(exe);
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in read.flatten() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        children.push(entry.path());
    }
    children.sort();
    for child in children {
        if child.is_dir() {
            walk(&child, os, depth - 1, budget, out);
        }
    }
}

/// The children of `dir` whose names satisfy `keep`.
fn children_where(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .flatten()
        .filter(|e| keep(&e.file_name().to_string_lossy().to_ascii_lowercase()))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

fn looks_like_java(name: &str) -> bool {
    [
        "jdk", "jre", "java", "temurin", "corretto", "zulu", "liberica", "semeru", "graal",
    ]
    .iter()
    .any(|k| name.contains(k))
}

/// Candidates found on the file system: `JAVA_HOME`, `PATH`, the usual install directories,
/// other launchers' runtimes and SDK managers.
pub fn file_candidates(env: &Env) -> Vec<PathBuf> {
    let os = env.os();
    let mut out = Vec::new();

    for home in &env.java_homes {
        if let Some(exe) = home_exe(home, os) {
            out.push(exe);
        }
    }
    for dir in &env.path {
        let exe = dir.join(exe_name(os));
        // The macOS /usr/bin/java is a stub that may open an installer dialog.
        let is_mac_stub = os == Os::MacOs && dir == Path::new("/usr/bin");
        if !is_mac_stub && exe.is_file() {
            out.push(exe);
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    let home = env.home.as_deref();
    let in_home = |rel: &str| home.map(|h| h.join(rel));

    match os {
        Os::Windows => {
            for pf in &env.program_files {
                for vendor in [
                    "Java",
                    "Eclipse Adoptium",
                    "Eclipse Foundation",
                    "AdoptOpenJDK",
                    "Zulu",
                    "Microsoft",
                    "BellSoft",
                    "Amazon Corretto",
                    "Semeru",
                    "Temurin",
                ] {
                    roots.push(pf.join(vendor));
                }
            }
            if let Some(local) = &env.local_app_data {
                for vendor in ["Programs\\Eclipse Adoptium", "Programs\\Microsoft"] {
                    roots.push(local.join(vendor));
                }
                // The official launcher's Store package keeps its runtimes in its own cache.
                for package in children_where(&local.join("Packages"), |n| {
                    n.starts_with("microsoft.4297127d64ec6")
                }) {
                    roots.push(package.join("LocalCache").join("Local").join("runtime"));
                }
            }
            if let Some(app) = &env.app_data {
                for dir in [
                    ".minecraft\\runtime",
                    "PrismLauncher\\java",
                    "MultiMC\\java",
                    "ATLauncher\\runtimes",
                    "ModrinthApp\\meta\\java_versions",
                    "gdlauncher_carbon\\data\\java",
                    ".tlauncher\\runtime",
                ] {
                    roots.push(app.join(dir));
                }
            }
            for rel in [
                ".jdks",
                ".gradle\\jdks",
                ".jabba\\jdk",
                ".sdkman\\candidates\\java",
                "curseforge\\minecraft\\Install\\runtime",
            ] {
                roots.extend(in_home(rel));
            }
            if let Some(h) = home {
                roots.extend(children_where(
                    &h.join("scoop").join("apps"),
                    looks_like_java,
                ));
            }
        }
        Os::MacOs => {
            roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
            roots.push(PathBuf::from("/System/Library/Java/JavaVirtualMachines"));
            for brew in ["/opt/homebrew/opt", "/usr/local/opt"] {
                roots.extend(children_where(Path::new(brew), |n| {
                    n.starts_with("openjdk")
                }));
            }
            for rel in [
                "Library/Java/JavaVirtualMachines",
                "Library/Application Support/minecraft/runtime",
                "Library/Application Support/PrismLauncher/java",
                "Library/Application Support/MultiMC/java",
                "Library/Application Support/ATLauncher/runtimes",
                "Library/Application Support/ModrinthApp/meta/java_versions",
                "Library/Application Support/gdlauncher_carbon/data/java",
                ".jdks",
                ".gradle/jdks",
                ".sdkman/candidates/java",
                ".asdf/installs/java",
                ".jabba/jdk",
                ".local/share/mise/installs/java",
            ] {
                roots.extend(in_home(rel));
            }
        }
        Os::Linux | Os::Other => {
            for dir in [
                "/usr/lib/jvm",
                "/usr/lib64/jvm",
                "/usr/lib32/jvm",
                "/usr/java",
                "/usr/local/java",
                "/usr/local/lib/jvm",
                "/opt/java",
                "/opt/jdk",
                "/opt/jvm",
                "/usr/lib/sdk/openjdk",
            ] {
                roots.push(PathBuf::from(dir));
            }
            roots.extend(children_where(Path::new("/opt"), looks_like_java));
            let data = env
                .xdg_data
                .clone()
                .or_else(|| in_home(".local/share"))
                .unwrap_or_default();
            for rel in [
                "minecraft/runtime",
                "PrismLauncher/java",
                "multimc/java",
                "ATLauncher/runtimes",
                "ModrinthApp/meta/java_versions",
                "gdlauncher_carbon/data/java",
                "mise/installs/java",
            ] {
                if !data.as_os_str().is_empty() {
                    roots.push(data.join(rel));
                }
            }
            for rel in [
                ".minecraft/runtime",
                ".jdks",
                ".gradle/jdks",
                ".sdkman/candidates/java",
                ".asdf/installs/java",
                ".jabba/jdk",
                ".var/app/com.mojang.Minecraft/data/minecraft/runtime",
            ] {
                roots.extend(in_home(rel));
            }
        }
    }

    let mut seen = HashSet::new();
    for root in roots {
        if seen.insert(root.clone()) {
            walk_homes(&root, os, &mut out);
        }
    }
    out
}

/// The Java homes named by `reg query … /s` output: values `JavaHome`, `Path` and
/// `InstallationPath` of type `REG_SZ` or `REG_EXPAND_SZ`.
pub fn parse_reg_query(text: &str) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        for kind in ["REG_SZ", "REG_EXPAND_SZ"] {
            let Some((name, value)) = line.split_once(kind) else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if ["JavaHome", "Path", "InstallationPath"]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
                && !value.is_empty()
            {
                homes.push(PathBuf::from(value));
            }
            break;
        }
    }
    homes
}

/// The Java homes listed by `/usr/libexec/java_home -V` (macOS): lines such as
/// `    17.0.2 (arm64) "Vendor" - "Name" /Library/…/Contents/Home`.
pub fn parse_java_home_v(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let path = line.rsplit_once("\" ").map(|(_, p)| p.trim())?;
            (path.starts_with('/') && line.starts_with(|c: char| c.is_ascii_digit()))
                .then(|| PathBuf::from(path))
        })
        .collect()
}

/// Candidates from OS services: the registry on Windows, `java_home` on macOS.
pub fn system_candidates(os: Os) -> Vec<PathBuf> {
    match os {
        Os::Windows => registry_homes()
            .into_iter()
            .filter_map(|h| home_exe(&h, os))
            .collect(),
        Os::MacOs => {
            let java_home = Path::new("/usr/libexec/java_home");
            if !java_home.is_file() {
                return Vec::new();
            }
            run_capped(java_home, &["-V"], &[], HELPER_TIMEOUT, HELPER_CAP)
                .map(|o| parse_java_home_v(&o.text))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|h| home_exe(&h, os))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn registry_homes() -> Vec<PathBuf> {
    const VENDORS: [&str; 10] = [
        "JavaSoft",
        "Eclipse Adoptium",
        "Eclipse Foundation",
        "AdoptOpenJDK",
        "Azul Systems",
        "Microsoft\\JDK",
        "BellSoft",
        "Amazon Corretto",
        "IBM",
        "Semeru",
    ];
    let mut queries: Vec<(String, &'static str)> = Vec::new();
    for vendor in VENDORS {
        queries.push((format!("HKLM\\SOFTWARE\\{vendor}"), "/reg:64"));
        queries.push((format!("HKLM\\SOFTWARE\\{vendor}"), "/reg:32"));
        queries.push((format!("HKCU\\SOFTWARE\\{vendor}"), "/reg:64"));
    }
    let mut homes = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = queries
            .iter()
            .map(|(key, view)| {
                scope.spawn(move || {
                    run_capped(
                        Path::new("reg"),
                        &["query", key, "/s", view],
                        &[],
                        HELPER_TIMEOUT,
                        HELPER_CAP,
                    )
                    .map(|o| parse_reg_query(&o.text))
                    .unwrap_or_default()
                })
            })
            .collect();
        for h in handles {
            homes.extend(h.join().unwrap_or_default());
        }
    });
    homes
}

/// Every candidate on this machine, `managed_dir` first.
pub fn all_candidates(env: &Env, managed_dir: Option<&Path>) -> Vec<Candidate> {
    let os = env.os();
    let mut out = Vec::new();
    if let Some(dir) = managed_dir {
        let mut found = Vec::new();
        walk_homes(dir, os, &mut found);
        out.extend(found.into_iter().map(|path| Candidate {
            path,
            managed: true,
        }));
    }
    let others = file_candidates(env)
        .into_iter()
        .chain(system_candidates(os));
    out.extend(others.map(|path| Candidate {
        path,
        managed: false,
    }));
    out
}

/// Drops candidates that resolve to an executable already listed (symlinks, `JAVA_HOME` that is
/// also on `PATH`); the first spelling stays.
pub fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen: HashSet<OsString> = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| {
            let real = fs::canonicalize(&c.path).unwrap_or_else(|_| c.path.clone());
            seen.insert(strip_verbatim(real).into_os_string())
        })
        .collect()
}

/// Removes the `\\?\` prefix `canonicalize` adds on Windows, so spellings compare equal.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest.to_ascii_lowercase()),
        _ if cfg!(windows) => PathBuf::from(text.to_ascii_lowercase()),
        _ => path,
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temporary directory removed on drop.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "vl-java-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempDir;
    use super::*;

    fn os() -> Os {
        Platform::current().os
    }

    /// Creates `<home>/bin/java[.exe]`.
    fn make_home(home: &Path) -> PathBuf {
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(exe_name(os()));
        fs::write(&exe, b"").unwrap();
        exe
    }

    #[test]
    fn walks_nested_homes_and_stops_at_a_home() {
        let tmp = TempDir::new();
        let a = make_home(&tmp.path().join("jdk-17"));
        let b = make_home(&tmp.path().join("x.jdk/Contents/Home"));
        let c = make_home(&tmp.path().join("runtime/comp/plat/comp"));
        // Below a home nothing is searched.
        make_home(&tmp.path().join("jdk-17/inner"));
        fs::create_dir_all(tmp.path().join("empty/dir")).unwrap();
        fs::write(tmp.path().join("file"), b"x").unwrap();

        let mut found = Vec::new();
        walk_homes(tmp.path(), os(), &mut found);
        found.sort();
        let mut want = vec![a, b, c];
        want.sort();
        assert_eq!(found, want);
    }

    #[test]
    fn walking_a_missing_root_finds_nothing() {
        let mut found = Vec::new();
        walk_homes(Path::new("/definitely/not/here"), os(), &mut found);
        assert!(found.is_empty());
    }

    #[test]
    fn reads_java_home_and_path() {
        let tmp = TempDir::new();
        let home_exe = make_home(&tmp.path().join("home"));
        let path_exe = make_home(&tmp.path().join("onpath"));
        let env = Env {
            os: Some(os()),
            java_homes: vec![tmp.path().join("home"), tmp.path().join("missing")],
            path: vec![tmp.path().join("onpath/bin"), tmp.path().join("nothing")],
            ..Env::default()
        };
        let found = file_candidates(&env);
        assert!(found.contains(&home_exe));
        assert!(found.contains(&path_exe));
    }

    #[test]
    fn finds_launcher_and_sdk_manager_runtimes_under_the_home() {
        let tmp = TempDir::new();
        let sdkman = make_home(&tmp.path().join(".sdkman/candidates/java/21.0.1-tem"));
        let jdks = make_home(&tmp.path().join(".jdks/corretto-17"));
        let env = Env {
            os: Some(os()),
            home: Some(tmp.path().to_path_buf()),
            ..Env::default()
        };
        let found = file_candidates(&env);
        assert!(found.contains(&sdkman));
        assert!(found.contains(&jdks));
    }

    #[test]
    fn managed_runtimes_come_first_and_are_flagged() {
        let tmp = TempDir::new();
        let managed = make_home(&tmp.path().join("managed/jdk-21"));
        let env = Env {
            os: Some(os()),
            ..Env::default()
        };
        let all = all_candidates(&env, Some(&tmp.path().join("managed")));
        assert_eq!(
            all.first().map(|c| (&c.path, c.managed)),
            Some((&managed, true))
        );
    }

    #[test]
    fn dedupes_the_same_executable() {
        let tmp = TempDir::new();
        let exe = make_home(&tmp.path().join("jdk"));
        let list = vec![
            Candidate {
                path: exe.clone(),
                managed: false,
            },
            Candidate {
                path: tmp.path().join("jdk/bin/../bin").join(exe_name(os())),
                managed: false,
            },
            Candidate {
                path: tmp.path().join("gone/java"),
                managed: false,
            },
        ];
        let out = dedupe(list);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, exe);
    }

    #[test]
    fn parses_registry_output() {
        let text = "
HKEY_LOCAL_MACHINE\\SOFTWARE\\Eclipse Adoptium\\JDK\\17.0.9.9\\hotspot\\MSI
    Path    REG_SZ    C:\\Program Files\\Eclipse Adoptium\\jdk-17.0.9.9-hotspot\\
    Version    REG_SZ    17.0.9.9

HKEY_LOCAL_MACHINE\\SOFTWARE\\JavaSoft\\Java Runtime Environment\\1.8
    JavaHome    REG_SZ    C:\\Program Files\\Java\\jre1.8.0_391
    RuntimeLib    REG_SZ    C:\\Program Files\\Java\\jre1.8.0_391\\bin\\client\\jvm.dll
HKEY_LOCAL_MACHINE\\SOFTWARE\\Azul Systems\\Zulu\\zulu-21
    InstallationPath    REG_EXPAND_SZ    %ProgramFiles%\\Zulu\\zulu-21
    Empty    REG_SZ
";
        let homes = parse_reg_query(text);
        assert_eq!(
            homes,
            vec![
                PathBuf::from("C:\\Program Files\\Eclipse Adoptium\\jdk-17.0.9.9-hotspot\\"),
                PathBuf::from("C:\\Program Files\\Java\\jre1.8.0_391"),
                PathBuf::from("%ProgramFiles%\\Zulu\\zulu-21"),
            ]
        );
    }

    #[test]
    fn parses_java_home_listing() {
        let text = "Matching Java Virtual Machines (2):
    21.0.1 (arm64) \"Eclipse Adoptium\" - \"OpenJDK 21.0.1\" /Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home
    1.8.0_391 (x86_64) \"Azul Systems, Inc.\" - \"Zulu 8.74.0.17\" /Library/Java/JavaVirtualMachines/zulu-8.jdk/Contents/Home

/Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home
";
        assert_eq!(
            parse_java_home_v(text),
            vec![
                PathBuf::from("/Library/Java/JavaVirtualMachines/temurin-21.jdk/Contents/Home"),
                PathBuf::from("/Library/Java/JavaVirtualMachines/zulu-8.jdk/Contents/Home"),
            ]
        );
        assert!(parse_java_home_v("").is_empty());
    }
}
