//! Reading a mod jar: metadata, bundled jars and Java packages.
//!
//! Jars are untrusted. Only the central directory and a few small metadata files are read; the
//! number of entries and the size of every file read are capped, and a damaged archive gives a
//! [`JarInfo`] with a `problem` instead of an error.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use vl_mods::{MANIFEST_FILE, METADATA_FILES, ModMetadata};
use zip::ZipArchive;

/// Most entries examined in one jar; a jar with more is reported as a problem.
pub const MAX_ENTRIES: usize = 200_000;
/// Largest metadata file (or manifest) that is read.
pub const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
/// Most bundled jars listed.
pub const MAX_NESTED: usize = 1_000;

/// Folders that hold bundled jars (Fabric and Quilt `META-INF/jars`, Forge `META-INF/jarjar`).
const NESTED_DIRS: [&str; 2] = ["META-INF/jars/", "META-INF/jarjar/"];

/// What is known about one jar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JarInfo {
    /// Mods declared by the jar's metadata files (empty for a library or a damaged jar).
    pub mods: Vec<ModMetadata>,
    /// Bundled jars (paths inside the jar); listed, never opened.
    pub nested_jars: Vec<String>,
    /// Java packages (dotted) that contain classes, reduced to the shortest prefixes.
    pub packages: Vec<String>,
    /// Why the jar or its metadata could not be read.
    pub problem: Option<String>,
}

/// Reads `path`; never fails.
pub fn read_jar(path: &Path) -> JarInfo {
    let mut info = JarInfo::default();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            info.problem = Some(format!("cannot open: {e}"));
            return info;
        }
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            info.problem = Some(format!("not a readable jar: {e}"));
            return info;
        }
    };
    if archive.len() > MAX_ENTRIES {
        info.problem = Some(format!("too many entries ({})", archive.len()));
        return info;
    }
    let names: Vec<String> = archive
        .file_names()
        .filter_map(|n| n.ok().map(std::borrow::Cow::into_owned))
        .collect();

    let mut packages = BTreeSet::new();
    for name in &names {
        if name.ends_with('/') {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if NESTED_DIRS.iter().any(|d| name.starts_with(d)) && lower.ends_with(".jar") {
            if info.nested_jars.len() < MAX_NESTED {
                info.nested_jars.push(name.clone());
            }
        } else if let Some(package) = class_package(name) {
            packages.insert(package);
        }
    }
    info.packages = reduce_packages(packages);

    let mut first_problem = None;
    for (meta_path, _) in METADATA_FILES {
        if !names.iter().any(|n| n == meta_path) {
            continue;
        }
        let parsed = read_entry(&mut archive, meta_path).and_then(|bytes| {
            vl_mods::parse_file(meta_path, &bytes)
                .ok_or_else(String::new)?
                .map_err(|e| e.message)
        });
        match parsed {
            Ok(mods) => info.mods.extend(mods),
            Err(e) => {
                first_problem.get_or_insert(format!("{meta_path}: {e}"));
            }
        }
    }
    if info
        .mods
        .iter()
        .any(|m| m.version.as_deref() == Some("${file.jarVersion}"))
    {
        let version = read_entry(&mut archive, MANIFEST_FILE)
            .ok()
            .and_then(|m| vl_mods::manifest_version(&m));
        vl_mods::resolve_jar_version(&mut info.mods, version.as_deref());
    }
    if info.mods.is_empty() {
        info.problem = first_problem;
    }
    info
}

/// Reads one entry fully, refusing one larger than [`MAX_METADATA_BYTES`].
fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let entry = archive.by_name(name).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    entry
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(format!("larger than {MAX_METADATA_BYTES} bytes"));
    }
    Ok(bytes)
}

/// The dotted package of a `.class` entry, or `None` for other files, `module-info`, classes
/// in the default package and entries that do not look like a class path.
pub(crate) fn class_package(entry: &str) -> Option<String> {
    let path = entry.strip_suffix(".class")?;
    // Multi-release classes live under `META-INF/versions/<n>/`.
    let path = match path.strip_prefix("META-INF/versions/") {
        Some(rest) => rest.split_once('/')?.1,
        None => path,
    };
    if path.starts_with("META-INF/") {
        return None;
    }
    let (package, class) = path.rsplit_once('/')?;
    if class == "module-info" || class == "package-info" && package.is_empty() {
        return None;
    }
    let valid = package
        .split('/')
        .all(|seg| !seg.is_empty() && !seg.contains(['.', '\\', ':']));
    valid.then(|| package.replace('/', "."))
}

/// Drops every package that has a shorter package of the set as its prefix.
pub(crate) fn reduce_packages(packages: BTreeSet<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    // Sorted order puts a prefix before everything below it.
    for package in packages {
        let covered = kept.iter().any(|k| {
            package
                .strip_prefix(k.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
        });
        if !covered {
            kept.push(package);
        }
    }
    kept
}
