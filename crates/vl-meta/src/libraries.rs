//! Libraries and natives per OS and architecture: artifacts, classifiers, extraction rules.
//!
//! [`plan`] turns the libraries of a resolved version ([`LibraryEntry`]) into a [`LibraryPlan`]
//! for one platform: the files to download (with their path below the shared `libraries`
//! directory, URL, SHA-1 and size), the classpath order, and the native archives to unpack into
//! the launch directory. It is pure: nothing is fetched, read or written here.
//!
//! # Formats understood
//!
//! - `downloads.artifact`: a jar for the classpath.
//! - Old natives: a `natives` map from the Mojang OS name to a classifier, which may contain
//!   `${arch}` (replaced by the pointer width, `32` or `64`); the archive is
//!   `downloads.classifiers[<classifier>]`. Such an archive is unpacked, never put on the
//!   classpath. `extract.exclude` lists entry-name prefixes that are not unpacked.
//! - New natives: a library whose coordinate carries a `natives-*` classifier and whose `rules`
//!   select the OS and architecture. It is an ordinary classpath jar (the game loads the
//!   natives from it itself).
//! - No `downloads` at all (loader profiles, very old versions): the path is derived from the
//!   Maven coordinate and the URL is `url` (default `https://libraries.minecraft.net/`) plus that
//!   path. SHA-1 and size are then unknown.
//!
//! A library with a `natives` map and no `downloads` is not put on the classpath; with
//! `downloads.artifact` it is.
//!
//! Classpath order is the order of the libraries; the caller appends the client jar.
//!
//! # Replacements
//!
//! Old versions use LWJGL 2, which has no arm64 build. [`apply_replacements`] swaps libraries for
//! a platform using data the caller supplies, before [`plan`].
//!
//! All upstream data is hostile: paths and coordinates are checked so that nothing can point
//! outside the libraries directory or the natives directory.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use vl_core::error::{Error, ErrorKind, Result};
use vl_core::platform::{Arch, Os, Platform};

use crate::manifest::is_sha1;
use crate::rules::{RuleContext, allows};
use crate::version::LibraryEntry;

/// The Maven repository used when a library names none.
pub const DEFAULT_REPOSITORY: &str = "https://libraries.minecraft.net/";

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// One file of the shared libraries directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFile {
    /// The path below the libraries directory, `/`-separated, never absolute or containing `..`.
    pub path: String,
    /// Where to download it; `None` when the file must already exist (installer-provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The expected SHA-1 (40 hex digits), when upstream states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// The expected size in bytes, when upstream states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// A native archive to unpack into the launch directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeArchive {
    /// The archive.
    pub file: LibraryFile,
    /// Entry-name prefixes that are not unpacked (`extract.exclude`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl NativeArchive {
    /// The relative path to unpack the archive entry `entry_name` to, or `None` when it must be
    /// skipped: excluded, a directory, or unsafe (absolute, `..`, backslashes, drive letters).
    pub fn target(&self, entry_name: &str) -> Option<String> {
        if entry_name.ends_with('/')
            || self
                .exclude
                .iter()
                .any(|p| entry_name.starts_with(p.as_str()))
        {
            return None;
        }
        safe_relative(entry_name).then(|| entry_name.to_owned())
    }
}

/// What to do for one platform.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryPlan {
    /// Every file to have on disk (classpath jars and native archives), without duplicates.
    pub downloads: Vec<LibraryFile>,
    /// The classpath jars, as paths below the libraries directory, in order.
    pub classpath: Vec<String>,
    /// The native archives to unpack.
    pub natives: Vec<NativeArchive>,
    /// Libraries that declare natives for this OS but have no archive for it.
    pub missing_natives: Vec<String>,
}

impl LibraryPlan {
    /// The classpath as a string: each jar below `libraries_dir`, then `extra` (such as the
    /// client jar), joined with `separator`.
    pub fn classpath_string(&self, libraries_dir: &str, extra: &[&str], separator: char) -> String {
        let dir = libraries_dir.trim_end_matches(['/', '\\']);
        let mut parts: Vec<String> = self
            .classpath
            .iter()
            .map(|p| format!("{dir}/{p}"))
            .collect();
        parts.extend(extra.iter().map(|e| (*e).to_owned()));
        parts.join(&separator.to_string())
    }
}

/// The classpath separator of an OS: `;` on Windows, `:` elsewhere.
pub const fn classpath_separator(os: Os) -> char {
    match os {
        Os::Windows => ';',
        _ => ':',
    }
}

/// A substitution for one platform: libraries whose coordinate starts with one of `drop` (a
/// `group` or `group:artifact` prefix) are removed and `libraries` are added at the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    /// The OS it applies to; `None` for any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<Os>,
    /// The architecture it applies to.
    pub arch: Arch,
    /// Coordinate prefixes of the libraries to remove.
    pub drop: Vec<String>,
    /// The libraries to add.
    pub libraries: Vec<LibraryEntry>,
}

/// Applies the replacements matching `platform` to `libraries`, in order.
pub fn apply_replacements(
    libraries: &[LibraryEntry],
    replacements: &[Replacement],
    platform: Platform,
) -> Vec<LibraryEntry> {
    let mut out = libraries.to_vec();
    for r in replacements {
        if r.arch != platform.arch || r.os.is_some_and(|os| os != platform.os) {
            continue;
        }
        out.retain(|l| !r.drop.iter().any(|p| matches_prefix(&l.name, p)));
        out.extend(r.libraries.iter().cloned());
    }
    out
}

/// Whether `name` is `prefix`, or continues it at a `:` (or, for a bare group, at a `.`).
fn matches_prefix(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(':') || (!prefix.contains(':') && rest.starts_with('.'))
}

#[derive(Deserialize, Clone)]
struct RawFile {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize, Default)]
struct RawDownloads {
    #[serde(default)]
    artifact: Option<RawFile>,
    #[serde(default)]
    classifiers: BTreeMap<String, RawFile>,
}

#[derive(Deserialize, Default)]
struct RawExtract {
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct RawLibrary {
    #[serde(default)]
    downloads: Option<RawDownloads>,
    #[serde(default)]
    natives: BTreeMap<String, String>,
    #[serde(default)]
    extract: Option<RawExtract>,
    #[serde(default)]
    url: Option<String>,
}

/// A parsed Maven coordinate.
struct Coordinate<'a> {
    group: &'a str,
    artifact: &'a str,
    version: &'a str,
    classifier: Option<&'a str>,
    ext: &'a str,
}

fn safe_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains(['/', '\\', ':', '@'])
        && !s.chars().any(char::is_control)
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

fn coordinate(name: &str) -> Result<Coordinate<'_>> {
    let (coords, ext) = name.split_once('@').unwrap_or((name, "jar"));
    let mut parts = coords.split(':');
    let (Some(group), Some(artifact), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(damaged(format!(
            "library name {name:?} is not a Maven coordinate"
        )));
    };
    let classifier = parts.next();
    if parts.next().is_some()
        || !safe_component(artifact)
        || !safe_component(version)
        || !safe_component(ext)
        || !group.split('.').all(safe_component)
        || classifier.is_some_and(|c| !safe_component(c))
    {
        return Err(damaged(format!(
            "library name {name:?} is not a safe Maven coordinate"
        )));
    }
    Ok(Coordinate {
        group,
        artifact,
        version,
        classifier,
        ext,
    })
}

impl Coordinate<'_> {
    /// The repository path with the given classifier.
    fn path(&self, classifier: Option<&str>) -> String {
        let mut file = format!("{}-{}", self.artifact, self.version);
        if let Some(c) = classifier {
            file.push('-');
            file.push_str(c);
        }
        format!(
            "{}/{}/{}/{file}.{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.ext
        )
    }
}

fn file_from(name: &str, raw: RawFile, fallback_path: &str) -> Result<LibraryFile> {
    let path = match raw.path {
        Some(p) if safe_relative(&p) => p,
        Some(p) => return Err(damaged(format!("library {name}: unsafe path {p:?}"))),
        None => fallback_path.to_owned(),
    };
    let url = raw.url.filter(|u| !u.is_empty());
    if url.as_deref().is_some_and(|u| !u.starts_with("https://")) {
        return Err(damaged(format!("library {name}: url is not https")));
    }
    if raw.sha1.as_deref().is_some_and(|s| !is_sha1(s)) {
        return Err(damaged(format!("library {name}: invalid sha1")));
    }
    Ok(LibraryFile {
        path,
        url,
        sha1: raw.sha1,
        size: raw.size,
    })
}

fn derived(name: &str, path: String, base: Option<&str>) -> Result<LibraryFile> {
    let base = base.filter(|b| !b.is_empty()).unwrap_or(DEFAULT_REPOSITORY);
    if !base.starts_with("https://") {
        return Err(damaged(format!(
            "library {name}: repository url is not https"
        )));
    }
    let sep = if base.ends_with('/') { "" } else { "/" };
    Ok(LibraryFile {
        url: Some(format!("{base}{sep}{path}")),
        path,
        sha1: None,
        size: None,
    })
}

/// Computes the files, classpath and natives of `libraries` for the platform of `ctx`.
///
/// # Errors
///
/// [`ErrorKind::Integrity`] for a damaged library: an unsafe path or coordinate, a non-`https`
/// URL, an invalid SHA-1, or fields of the wrong type.
pub fn plan(libraries: &[LibraryEntry], ctx: &RuleContext<'_>) -> Result<LibraryPlan> {
    let mut plan = LibraryPlan::default();
    let mut seen_files = HashSet::new();
    let mut seen_classpath = HashSet::new();
    for lib in libraries {
        if !allows(&lib.rules, ctx) {
            continue;
        }
        let raw: RawLibrary = serde_json::from_value(serde_json::Value::Object(lib.rest.clone()))
            .map_err(|e| damaged(format!("library {}: {e}", lib.name)))?;
        let coord = coordinate(&lib.name)?;
        let has_natives = !raw.natives.is_empty();

        // The classpath jar.
        let artifact = match &raw.downloads {
            Some(d) => d
                .artifact
                .clone()
                .map(|a| file_from(&lib.name, a, &coord.path(coord.classifier)))
                .transpose()?,
            None if !has_natives => Some(derived(
                &lib.name,
                coord.path(coord.classifier),
                raw.url.as_deref(),
            )?),
            None => None,
        };
        if let Some(a) = artifact {
            if seen_files.insert(a.path.clone()) {
                plan.downloads.push(a.clone());
            }
            if seen_classpath.insert(a.path.clone()) {
                plan.classpath.push(a.path);
            }
        }

        // The native archive of this platform (old format).
        let Some(os_name) = ctx.platform.os.mojang_name() else {
            continue;
        };
        let Some(template) = raw.natives.get(os_name) else {
            continue;
        };
        let classifier = if template.contains("${arch}") {
            ctx.platform
                .arch
                .bits()
                .map(|bits| template.replace("${arch}", &bits.to_string()))
        } else {
            Some(template.clone())
        };
        let Some(classifier) = classifier else {
            plan.missing_natives.push(lib.name.clone());
            continue;
        };
        let fallback = coord.path(Some(&classifier));
        let file = match &raw.downloads {
            None => Some(derived(&lib.name, fallback, raw.url.as_deref())?),
            Some(d) => d
                .classifiers
                .get(&classifier)
                .cloned()
                .map(|f| file_from(&lib.name, f, &fallback))
                .transpose()?,
        };
        match file {
            Some(file) => {
                if seen_files.insert(file.path.clone()) {
                    plan.downloads.push(file.clone());
                }
                let exclude = raw.extract.map(|e| e.exclude).unwrap_or_default();
                plan.natives.push(NativeArchive { file, exclude });
            }
            None => plan.missing_natives.push(lib.name.clone()),
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Features;
    use crate::version::VersionJson;

    fn libs(name: &str) -> Vec<LibraryEntry> {
        let path = format!(
            "{}/tests/fixtures/libraries/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        VersionJson::parse(&bytes)
            .unwrap()
            .resolve_standalone()
            .unwrap()
            .libraries
    }

    fn plat(os: Os, arch: Arch) -> Platform {
        Platform { os, arch }
    }

    fn plan_for(name: &str, p: Platform) -> LibraryPlan {
        let features = Features::new();
        plan(&libs(name), &RuleContext::new(p, &features)).unwrap()
    }

    fn cp(plan: &LibraryPlan) -> Vec<&str> {
        plan.classpath.iter().map(String::as_str).collect()
    }

    const LWJGL: &str = "org/example/lwjgl/3.3.3/lwjgl-3.3.3";

    #[test]
    fn modern_windows_x64() {
        let p = plan_for("modern.json", plat(Os::Windows, Arch::X86_64));
        assert_eq!(
            cp(&p),
            [
                "com/example/plain/1.0/plain-1.0.jar".to_owned(),
                format!("{LWJGL}.jar"),
                format!("{LWJGL}-natives-windows.jar"),
            ]
        );
        assert!(p.natives.is_empty());
        assert_eq!(p.downloads.len(), 3);
        let first = &p.downloads[0];
        assert_eq!(
            first.url.as_deref(),
            Some("https://example.invalid/plain-1.0.jar")
        );
        assert_eq!(first.size, Some(10));
        assert!(first.sha1.is_some());
    }

    #[test]
    fn modern_arm64_selection() {
        let mac = plan_for("modern.json", plat(Os::MacOs, Arch::Aarch64));
        assert!(cp(&mac).contains(&format!("{LWJGL}-natives-macos-arm64.jar").as_str()));
        assert!(!cp(&mac).iter().any(|n| n.ends_with("natives-windows.jar")));
        let linux = plan_for("modern.json", plat(Os::Linux, Arch::Aarch64));
        assert!(cp(&linux).contains(&format!("{LWJGL}-natives-linux-arm64.jar").as_str()));
        let linux64 = plan_for("modern.json", plat(Os::Linux, Arch::X86_64));
        assert!(cp(&linux64).contains(&format!("{LWJGL}-natives-linux.jar").as_str()));
        assert!(!cp(&linux64).iter().any(|n| n.contains("arm64")));
    }

    #[test]
    fn legacy_natives_per_os() {
        let win = plan_for("legacy.json", plat(Os::Windows, Arch::X86_64));
        assert_eq!(
            cp(&win),
            [
                "com/example/old-lib/2.0/old-lib-2.0.jar",
                "com/example/fabricish/0.1/fabricish-0.1.jar"
            ]
        );
        assert_eq!(win.natives.len(), 2);
        assert_eq!(
            win.natives[0].file.path,
            "com/example/lwjgl-platform/2.9.4/lwjgl-platform-2.9.4-natives-windows.jar"
        );
        assert_eq!(win.natives[0].exclude, ["META-INF/"]);
        assert_eq!(
            win.natives[1].file.path,
            "com/example/arch-native/1.0/arch-native-1.0-natives-windows-64.jar"
        );
        // Natives are downloaded but never on the classpath.
        assert_eq!(win.downloads.len(), 4);
        assert!(win.missing_natives.is_empty());

        let x86 = plan_for("legacy.json", plat(Os::Windows, Arch::X86));
        assert!(x86.natives[1].file.path.ends_with("natives-windows-32.jar"));

        let mac = plan_for("legacy.json", plat(Os::MacOs, Arch::Aarch64));
        assert_eq!(mac.natives.len(), 1);
        assert!(mac.natives[0].file.path.ends_with("natives-osx.jar"));
        assert_eq!(mac.missing_natives, ["com.example:no-arm:1.0"]);
    }

    #[test]
    fn legacy_derived_files_use_repository() {
        let p = plan_for("legacy.json", plat(Os::Linux, Arch::X86_64));
        let fabricish = p
            .downloads
            .iter()
            .find(|f| f.path.contains("fabricish"))
            .unwrap();
        assert_eq!(
            fabricish.url.as_deref(),
            Some("https://maven.example.invalid/repo/com/example/fabricish/0.1/fabricish-0.1.jar")
        );
        assert_eq!(fabricish.sha1, None);
        let old = p
            .downloads
            .iter()
            .find(|f| f.path.contains("old-lib"))
            .unwrap();
        assert!(old.url.as_deref().unwrap().starts_with(DEFAULT_REPOSITORY));
    }

    #[test]
    fn installer_provided_has_no_url() {
        let p = plan_for("legacy.json", plat(Os::Linux, Arch::X86_64));
        let f = p
            .downloads
            .iter()
            .find(|f| f.path.contains("installed"))
            .unwrap();
        assert_eq!(f.url, None);
        assert!(p.classpath.iter().any(|c| c.contains("installed")));
        let win = plan_for("legacy.json", plat(Os::Windows, Arch::X86_64));
        assert!(!win.classpath.iter().any(|c| c.contains("installed")));
    }

    #[test]
    fn replacements_swap_libraries_on_arm64() {
        let all = libs("legacy.json");
        let repl = Replacement {
            os: Some(Os::MacOs),
            arch: Arch::Aarch64,
            drop: vec!["com.example:lwjgl-platform".into()],
            libraries: libs("modern.json").into_iter().take(1).collect(),
        };
        let arm = apply_replacements(
            &all,
            std::slice::from_ref(&repl),
            plat(Os::MacOs, Arch::Aarch64),
        );
        assert!(!arm.iter().any(|l| l.name.contains("lwjgl-platform")));
        assert_eq!(arm.last().unwrap().name, "com.example:plain:1.0");
        let x64 = apply_replacements(&all, &[repl], plat(Os::MacOs, Arch::X86_64));
        assert_eq!(x64, all);
    }

    #[test]
    fn prefix_matching() {
        assert!(matches_prefix("org.lwjgl:lwjgl:3.3.3", "org.lwjgl"));
        assert!(matches_prefix("org.lwjgl.lwjgl:lwjgl:2.9.4", "org.lwjgl"));
        assert!(matches_prefix("org.lwjgl:lwjgl:3.3.3", "org.lwjgl:lwjgl"));
        assert!(!matches_prefix("org.lwjglx:lwjgl:3.3.3", "org.lwjgl"));
        assert!(!matches_prefix(
            "org.lwjgl:lwjgl-glfw:3.3.3",
            "org.lwjgl:lwjgl"
        ));
    }

    #[test]
    fn extract_rules() {
        let a = NativeArchive {
            file: LibraryFile {
                path: "x.jar".into(),
                url: None,
                sha1: None,
                size: None,
            },
            exclude: vec!["META-INF/".into()],
        };
        assert_eq!(a.target("lwjgl.dll").as_deref(), Some("lwjgl.dll"));
        assert_eq!(
            a.target("sub/liblwjgl.so").as_deref(),
            Some("sub/liblwjgl.so")
        );
        assert_eq!(a.target("META-INF/MANIFEST.MF"), None);
        assert_eq!(a.target("dir/"), None);
        assert_eq!(a.target(""), None);
        assert_eq!(a.target("../evil.dll"), None);
        assert_eq!(a.target("/abs.dll"), None);
        assert_eq!(a.target("a\\b.dll"), None);
        assert_eq!(a.target("C:/x.dll"), None);
    }

    #[test]
    fn classpath_string_joins() {
        let p = plan_for("modern.json", plat(Os::Windows, Arch::X86_64));
        let s = p.classpath_string(
            "C:/vl/libraries/",
            &["C:/vl/client.jar"],
            classpath_separator(Os::Windows),
        );
        assert!(s.starts_with("C:/vl/libraries/com/example/plain/1.0/plain-1.0.jar;"));
        assert!(s.ends_with(";C:/vl/client.jar"));
        assert_eq!(classpath_separator(Os::Linux), ':');
    }

    #[test]
    fn hostile_entries_are_rejected() {
        let features = Features::new();
        let ctx = RuleContext::new(plat(Os::Linux, Arch::X86_64), &features);
        let bad = |json: &str| {
            let l: LibraryEntry = serde_json::from_str(json).unwrap();
            plan(&[l], &ctx).unwrap_err().kind
        };
        let cases = [
            r#"{"name":"a:../b:1"}"#,
            r#"{"name":"nonsense"}"#,
            r#"{"name":"a:b:1","downloads":{"artifact":{"path":"../x.jar","url":"https://e.invalid/x"}}}"#,
            r#"{"name":"a:b:1","downloads":{"artifact":{"path":"/x.jar","url":"https://e.invalid/x"}}}"#,
            r#"{"name":"a:b:1","downloads":{"artifact":{"path":"x.jar","url":"http://e.invalid/x"}}}"#,
            r#"{"name":"a:b:1","downloads":{"artifact":{"path":"x.jar","sha1":"zz"}}}"#,
            r#"{"name":"a:b:1","url":"http://e.invalid/"}"#,
            r#"{"name":"a:b:1","natives":5}"#,
        ];
        for case in cases {
            assert_eq!(bad(case), ErrorKind::Integrity, "{case}");
        }
    }

    #[test]
    fn unknown_platform_has_no_natives() {
        let p = plan_for("legacy.json", plat(Os::Other, Arch::Other));
        assert!(p.natives.is_empty());
    }
}
