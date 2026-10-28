//! Installer profiles (`install_profile.json`, spec 1) of loaders that ship an installer jar
//! (NeoForge, and Forge since 1.13).
//!
//! An installer jar holds `install_profile.json`, the launch profile (`version.json`) and data
//! files (`/data/client.lzma`). The install profile lists:
//!
//! - `libraries` — Mojang-shaped library entries needed by the processors;
//! - `data` — named values per side: `[group:artifact:version[:classifier][@ext]]` is a library
//!   file, `'text'` is a literal, `/path` is a file inside the installer jar, anything else is
//!   used as is;
//! - `processors` — Java programs (`jar` with its `classpath`, `args`, optional `sides` and
//!   `outputs` with expected SHA-1 hashes) that turn the vanilla client jar into the patched one.
//!
//! [`InstallProfile::parse`] checks the document (hostile input: sizes and counts are capped,
//! coordinates and in-jar paths must be safe). [`InstallProfile::plan`] resolves the processors
//! of one side into ready argument lists: `{NAME}` is a data value or one of the built-in
//! values of [`PlanContext`], `[coordinate]` is a path below the library directory. Running them
//! is the native layer's job.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use vl_core::error::{Error, ErrorKind, Result};

use crate::loader::{check_coordinate, maven_path};
use crate::manifest::is_sha1;
use crate::version::LibraryEntry;

/// The largest `install_profile.json` accepted.
pub const MAX_INSTALL_PROFILE_BYTES: usize = 8 * 1024 * 1024;

/// The most processors, data entries or libraries accepted in one profile.
pub const MAX_ENTRIES: usize = 4096;

/// The most arguments or class-path entries of one processor.
pub const MAX_PROCESSOR_ARGS: usize = 1024;

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// Which installation a processor or data value is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The game client.
    Client,
    /// A dedicated server.
    Server,
}

impl Side {
    /// The name used in the profile (`client`, `server`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

/// A data value for both sides.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SidedValue {
    /// The client value.
    #[serde(default)]
    pub client: String,
    /// The server value.
    #[serde(default)]
    pub server: String,
}

impl SidedValue {
    /// The value for `side`.
    pub fn get(&self, side: Side) -> &str {
        match side {
            Side::Client => &self.client,
            Side::Server => &self.server,
        }
    }
}

/// One processor: a Java program run during installation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Processor {
    /// The coordinate of the jar whose manifest names the main class.
    pub jar: String,
    /// The coordinates of the class path (usually the jar first).
    #[serde(default)]
    pub classpath: Vec<String>,
    /// The arguments, with `{NAME}` and `[coordinate]` references.
    #[serde(default)]
    pub args: Vec<String>,
    /// The sides it runs on; absent means both.
    #[serde(default)]
    pub sides: Option<Vec<String>>,
    /// Files it produces and their SHA-1 hashes, both with references.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

impl Processor {
    /// Whether it runs for `side`.
    pub fn runs_on(&self, side: Side) -> bool {
        self.sides
            .as_ref()
            .is_none_or(|s| s.iter().any(|x| x == side.as_str()))
    }
}

/// A parsed `install_profile.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallProfile {
    /// The format, 0 or 1.
    pub spec: u32,
    /// The id of the version it installs (`neoforge-21.1.77`).
    pub version: String,
    /// The game version it is for.
    pub minecraft: String,
    /// The path of the launch profile inside the installer (`/version.json`).
    pub json: String,
    /// Named data values.
    pub data: BTreeMap<String, SidedValue>,
    /// The processors, in order.
    pub processors: Vec<Processor>,
    /// Libraries the processors need.
    pub libraries: Vec<LibraryEntry>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    spec: u32,
    version: String,
    minecraft: String,
    #[serde(default = "default_json")]
    json: String,
    #[serde(default)]
    data: BTreeMap<String, SidedValue>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<LibraryEntry>,
}

fn default_json() -> String {
    "/version.json".to_owned()
}

/// A library file the processors need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLibrary {
    /// The coordinate.
    pub name: String,
    /// The path below the library directory, `/`-separated.
    pub path: String,
    /// Where to download it; `None` when the installer carries it at `maven/<path>`.
    pub url: Option<String>,
    /// The SHA-1, when given.
    pub sha1: Option<String>,
    /// The size, when given.
    pub size: Option<u64>,
}

/// The values a plan needs from the machine. Paths are absolute, in the host's form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    /// The side to install.
    pub side: Side,
    /// `{MINECRAFT_JAR}`: the vanilla jar of that side.
    pub minecraft_jar: String,
    /// `{ROOT}`: the directory that holds `libraries/`.
    pub root: String,
    /// `{LIBRARY_DIR}`: the library directory.
    pub library_dir: String,
    /// `{INSTALLER}`: the installer jar.
    pub installer: String,
    /// Where the installer's own files (`/data/...`) were extracted.
    pub extracted_dir: String,
    /// The path separator of the host.
    pub separator: char,
}

/// A processor ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorStep {
    /// The jar whose manifest names the main class.
    pub jar: String,
    /// The class path, the jar included.
    pub classpath: Vec<String>,
    /// The resolved arguments.
    pub args: Vec<String>,
    /// Files the step must produce and their SHA-1 hashes (lowercase).
    pub outputs: Vec<(String, String)>,
}

impl InstallProfile {
    /// Parses and checks an `install_profile.json`.
    ///
    /// # Errors
    ///
    /// `Integrity` for a document that is too large, not JSON of this shape, of an unknown spec,
    /// has too many entries, an unsafe coordinate or in-jar path, or a library without a file.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_INSTALL_PROFILE_BYTES {
            return Err(damaged("install profile is too large"));
        }
        let raw: Raw = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid install profile: {e}")))?;
        if raw.spec > 1 {
            return Err(Error::unsupported(format!(
                "install profile spec {} is not supported",
                raw.spec
            )));
        }
        if raw.version.is_empty() || raw.minecraft.is_empty() {
            return Err(damaged("install profile has no version or minecraft"));
        }
        if raw.data.len() > MAX_ENTRIES
            || raw.processors.len() > MAX_ENTRIES
            || raw.libraries.len() > MAX_ENTRIES
        {
            return Err(damaged("install profile has too many entries"));
        }
        for p in &raw.processors {
            check_coordinate(&p.jar)?;
            if p.classpath.len() > MAX_PROCESSOR_ARGS
                || p.args.len() > MAX_PROCESSOR_ARGS
                || p.outputs.len() > MAX_PROCESSOR_ARGS
            {
                return Err(damaged(format!("processor {} is too large", p.jar)));
            }
            for c in &p.classpath {
                check_coordinate(c)?;
            }
        }
        for v in raw.data.values() {
            for side in [Side::Client, Side::Server] {
                if let Some(inner) = v.get(side).strip_prefix('/') {
                    check_inner_path(inner)?;
                }
            }
        }
        check_inner_path(raw.json.trim_start_matches('/'))?;
        let profile = Self {
            spec: raw.spec,
            version: raw.version,
            minecraft: raw.minecraft,
            json: raw.json,
            data: raw.data,
            processors: raw.processors,
            libraries: raw.libraries,
        };
        profile.libraries()?;
        Ok(profile)
    }

    /// The libraries of the profile as files to fetch.
    ///
    /// # Errors
    ///
    /// `Integrity` for an entry without `downloads.artifact`, with an unsafe path, an invalid
    /// hash or a non-`https` URL.
    pub fn libraries(&self) -> Result<Vec<ProfileLibrary>> {
        self.libraries.iter().map(profile_library).collect()
    }

    /// The files inside the installer jar that a plan for `side` reads: data values starting
    /// with `/` and libraries without a URL (stored at `maven/<path>`). `/`-separated, without
    /// the leading `/`.
    ///
    /// # Errors
    ///
    /// As [`libraries`](Self::libraries).
    pub fn installer_files(&self, side: Side) -> Result<Vec<String>> {
        let mut files: Vec<String> = self
            .data
            .values()
            .filter_map(|v| v.get(side).strip_prefix('/'))
            .map(str::to_owned)
            .collect();
        for lib in self.libraries()? {
            if lib.url.is_none() {
                files.push(format!("maven/{}", lib.path));
            }
        }
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// The library files named by the data values of `side` (`[coordinate]` values), resolved
    /// below the library directory: what the processors read and write.
    ///
    /// # Errors
    ///
    /// `Integrity` for a malformed coordinate.
    pub fn data_artifacts(&self, ctx: &PlanContext) -> Result<Vec<(String, String)>> {
        self.data
            .iter()
            .filter_map(|(k, v)| {
                let value = v.get(ctx.side);
                let coord = value.strip_prefix('[')?.strip_suffix(']')?;
                Some(library_file(ctx, coord).map(|p| (k.clone(), p)))
            })
            .collect()
    }

    /// The processors of `ctx.side` with every reference resolved.
    ///
    /// # Errors
    ///
    /// `Integrity` for a reference to an unknown value, a malformed coordinate, or an output
    /// hash that is not a SHA-1.
    pub fn plan(&self, ctx: &PlanContext) -> Result<Vec<ProcessorStep>> {
        let values = self.values(ctx)?;
        self.processors
            .iter()
            .filter(|p| p.runs_on(ctx.side))
            .map(|p| {
                let args = p
                    .args
                    .iter()
                    .map(|a| resolve_arg(ctx, &values, a))
                    .collect::<Result<Vec<_>>>()?;
                let outputs = p
                    .outputs
                    .iter()
                    .map(|(file, hash)| {
                        let file = resolve_arg(ctx, &values, file)?;
                        let hash = resolve_arg(ctx, &values, hash)?;
                        let hash = hash.trim_matches('\'').to_ascii_lowercase();
                        if !is_sha1(&hash) {
                            return Err(damaged(format!("output {file}: invalid sha1")));
                        }
                        Ok((file, hash))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(ProcessorStep {
                    jar: library_file(ctx, &p.jar)?,
                    classpath: p
                        .classpath
                        .iter()
                        .map(|c| library_file(ctx, c))
                        .collect::<Result<_>>()?,
                    args,
                    outputs,
                })
            })
            .collect()
    }

    /// The data values of `ctx.side` plus the built-in ones, resolved.
    fn values(&self, ctx: &PlanContext) -> Result<BTreeMap<String, String>> {
        let mut values = BTreeMap::new();
        for (name, v) in &self.data {
            let value = v.get(ctx.side);
            let resolved = if let Some(coord) =
                value.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            {
                library_file(ctx, coord)?
            } else if let Some(text) = value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                text.to_owned()
            } else if let Some(inner) = value.strip_prefix('/') {
                join(&ctx.extracted_dir, inner, ctx.separator)
            } else {
                value.to_owned()
            };
            values.insert(name.clone(), resolved);
        }
        for (name, value) in [
            ("SIDE", ctx.side.as_str().to_owned()),
            ("MINECRAFT_JAR", ctx.minecraft_jar.clone()),
            ("MINECRAFT_VERSION", self.minecraft.clone()),
            ("ROOT", ctx.root.clone()),
            ("INSTALLER", ctx.installer.clone()),
            ("LIBRARY_DIR", ctx.library_dir.clone()),
        ] {
            values.insert(name.to_owned(), value);
        }
        Ok(values)
    }
}

/// The `Main-Class` of a jar manifest (`META-INF/MANIFEST.MF`), from its main section.
pub fn manifest_main_class(manifest: &str) -> Option<String> {
    let mut value: Option<String> = None;
    for line in manifest.lines() {
        if line.is_empty() {
            // The end of the main section.
            break;
        }
        if let Some(cont) = line.strip_prefix(' ') {
            if let Some(v) = value.as_mut() {
                v.push_str(cont);
            }
            continue;
        }
        if value.is_some() {
            break;
        }
        if let Some((key, v)) = line.split_once(':')
            && key.eq_ignore_ascii_case("Main-Class")
        {
            value = Some(v.trim_start().to_owned());
        }
    }
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn profile_library(lib: &LibraryEntry) -> Result<ProfileLibrary> {
    check_coordinate(&lib.name)?;
    let artifact = lib
        .rest
        .get("downloads")
        .and_then(|d| d.get("artifact"))
        .ok_or_else(|| damaged(format!("{}: no downloads.artifact", lib.name)))?;
    let text = |key: &str| {
        artifact
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let path = match text("path") {
        Some(p) => {
            check_inner_path(p)?;
            p.to_owned()
        }
        None => maven_path(&lib.name)?,
    };
    let url = text("url").map(str::to_owned);
    if let Some(u) = &url
        && !u.starts_with("https://")
    {
        return Err(damaged(format!("{}: download is not https", lib.name)));
    }
    let sha1 = match text("sha1") {
        Some(s) if is_sha1(s) => Some(s.to_ascii_lowercase()),
        Some(_) => return Err(damaged(format!("{}: invalid sha1", lib.name))),
        None => None,
    };
    Ok(ProfileLibrary {
        name: lib.name.clone(),
        path,
        url,
        sha1,
        size: artifact.get("size").and_then(Value::as_u64),
    })
}

/// A relative `/`-separated path inside an archive or below a directory, with no way out.
fn check_inner_path(path: &str) -> Result<()> {
    let ok = !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', ':'])
                && !part.chars().any(char::is_control)
        });
    if ok {
        Ok(())
    } else {
        Err(damaged(format!("unsafe path {path:?}")))
    }
}

fn join(base: &str, relative: &str, separator: char) -> String {
    let mut out = base.trim_end_matches(['/', '\\']).to_owned();
    for part in relative.split('/') {
        out.push(separator);
        out.push_str(part);
    }
    out
}

fn library_file(ctx: &PlanContext, coord: &str) -> Result<String> {
    Ok(join(&ctx.library_dir, &maven_path(coord)?, ctx.separator))
}

/// Resolves one argument: a whole `[coordinate]` is a library file; `{NAME}` anywhere is a value.
fn resolve_arg(ctx: &PlanContext, values: &BTreeMap<String, String>, arg: &str) -> Result<String> {
    if let Some(coord) = arg.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return library_file(ctx, coord);
    }
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| damaged(format!("unclosed reference in {arg:?}")))?;
        let name = &after[..end];
        let value = values
            .get(name)
            .ok_or_else(|| damaged(format!("unknown value {{{name}}} in {arg:?}")))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_main_class_of_a_manifest() {
        let m = "Manifest-Version: 1.0\r\nMain-Class: net.example.tools.Ma\r\n in\r\n\r\nName: x/\r\nMain-Class: other\r\n";
        assert_eq!(
            manifest_main_class(m).as_deref(),
            Some("net.example.tools.Main")
        );
        assert_eq!(manifest_main_class("Manifest-Version: 1.0\n"), None);
        assert_eq!(
            manifest_main_class("main-class:  a.B \n").as_deref(),
            Some("a.B")
        );
    }

    #[test]
    fn inner_paths_cannot_escape() {
        assert!(check_inner_path("data/client.lzma").is_ok());
        for bad in ["", "../x", "a//b", "a/../b", "C:/x", "a\\b"] {
            assert!(check_inner_path(bad).is_err(), "{bad}");
        }
    }
}
