//! A version JSON: main class, Java version, downloads, logging, inheritance.
//!
//! [`VersionJson::parse`] reads one upstream document (vanilla, or a loader profile with
//! `inheritsFrom`) into typed data that serializes back to the upstream shape.
//! [`VersionJson::resolve`] merges the `inheritsFrom` chain into a [`ResolvedVersion`], which has
//! everything needed to prepare and launch the game.
//!
//! Unknown fields are ignored. Damaged data is an [`ErrorKind::Integrity`] error: a document that
//! is not JSON, is too large, has a field of the wrong type, an invalid hash or a non-`https` URL,
//! or an `inheritsFrom` chain that is too long, cyclic or unresolvable.
//!
//! Libraries are kept as [`LibraryEntry`] (name, rules, and the remaining fields untouched);
//! turning them into artifacts and natives is the job of [`crate::libraries`].
//!
//! # Merge rules (`inheritsFrom`)
//!
//! The chain is merged from the root parent down to the child. For a pair `(parent, child)`:
//!
//! - `id`, `type`, `releaseTime`: the child's (the parent's when the child's is empty).
//! - `mainClass`, `assetIndex`, `assets`, `javaVersion`, `logging`: the child's, else the parent's.
//! - `arguments`: the child's `game` and `jvm` lists are appended after the parent's. A child that
//!   has only a legacy `minecraftArguments` replaces the parent's game arguments and keeps its
//!   JVM arguments. A legacy parent contributes [`default_jvm`] arguments.
//! - `libraries`: the child's entries come first, then the parent's entries whose
//!   `group:artifact[:classifier]` the child does not list (the child's version wins).
//! - `downloads`: merged by key; the child's entry wins.
//! - Client jar: the first `jar` field found from the child up, else the root parent's `id`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use vl_core::error::{Error, ErrorKind, Result};

use crate::arguments::{Arguments, default_jvm};
use crate::manifest::is_sha1;
use crate::rules::Rule;

/// The largest version JSON accepted, in bytes. Real ones are up to about 200 KiB.
pub const MAX_VERSION_BYTES: usize = 8 * 1024 * 1024;

/// The longest `inheritsFrom` chain accepted, counting the version itself.
pub const MAX_INHERITANCE_DEPTH: usize = 8;

/// A downloadable file (`downloads.client`, `downloads.server`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// The SHA-1.
    pub sha1: String,
    /// The size in bytes.
    pub size: u64,
    /// The `https` URL.
    pub url: String,
}

/// The asset index a version uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    /// The index id (e.g. `12`, `legacy`, `pre-1.6`).
    pub id: String,
    /// The SHA-1 of the index file.
    pub sha1: String,
    /// The size of the index file in bytes.
    pub size: u64,
    /// The total size of all assets in bytes (`0` when upstream omits it).
    #[serde(default)]
    pub total_size: u64,
    /// The `https` URL of the index file.
    pub url: String,
}

/// The Java runtime a version asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    /// The runtime component name (e.g. `jre-legacy`, `java-runtime-delta`).
    pub component: String,
    /// The Java major version.
    pub major_version: u32,
}

impl JavaVersion {
    /// What a version without `javaVersion` (before Minecraft 1.17, mostly 1.6 and older) runs on.
    pub fn legacy_default() -> Self {
        Self {
            component: "jre-legacy".to_owned(),
            major_version: 8,
        }
    }
}

/// The log configuration file of a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingFile {
    /// The file id (its name).
    pub id: String,
    /// The SHA-1.
    pub sha1: String,
    /// The size in bytes.
    pub size: u64,
    /// The `https` URL.
    pub url: String,
}

/// A JVM log configuration: an argument template (`-Dlog4j.configurationFile=${path}`) and the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// The JVM argument template; `${path}` is the downloaded file.
    pub argument: String,
    /// The configuration file.
    pub file: LoggingFile,
    /// The configuration type (e.g. `log4j2-xml`).
    #[serde(default, rename = "type")]
    pub kind: String,
}

/// Upstream `logging`: configurations by side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logging {
    /// The client's configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<LoggingConfig>,
}

/// A library entry of a version JSON, as far as version merging needs it. `rules` is parsed (see
/// [`crate::rules::allows`]); every other field (`downloads`, `natives`, `extract`, `url`, ...)
/// is kept untouched in `rest` for [`crate::libraries`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// The Maven coordinates, `group:artifact:version[:classifier][@ext]`.
    pub name: String,
    /// The rules guarding the library; empty means always used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// All other fields as upstream wrote them.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl LibraryEntry {
    /// The identity used when a child overrides a parent's library: `group:artifact` plus the
    /// classifier, if any; the version and extension are not part of it.
    pub fn key(&self) -> String {
        let mut parts = self.name.split(':');
        let group = parts.next().unwrap_or_default();
        let artifact = parts.next().unwrap_or_default();
        let _version = parts.next();
        let classifier = parts.next().map(|c| c.split('@').next().unwrap_or(c));
        match classifier {
            Some(c) => format!("{group}:{artifact}:{c}"),
            None => format!("{group}:{artifact}"),
        }
    }
}

/// One version JSON, as upstream wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    /// The version id.
    pub id: String,
    /// The parent version this one extends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits_from: Option<String>,
    /// `release`, `snapshot`, `old_beta`, `old_alpha`, or a loader's own type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// The main class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_class: Option<String>,
    /// New-format arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,
    /// Legacy game arguments (a single string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_arguments: Option<String>,
    /// The libraries.
    #[serde(default)]
    pub libraries: Vec<LibraryEntry>,
    /// Downloads by name (`client`, `server`, `client_mappings`, ...).
    #[serde(default)]
    pub downloads: BTreeMap<String, Download>,
    /// The asset index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_index: Option<AssetIndexRef>,
    /// The asset index id (older versions have this without `assetIndex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<String>,
    /// The Java runtime asked for; absent in old versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_version: Option<JavaVersion>,
    /// The log configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
    /// The id of the version whose client jar to use, when not the version itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jar: Option<String>,
    /// The release time (RFC 3339), as given.
    #[serde(default)]
    pub release_time: String,
}

/// A version with its `inheritsFrom` chain merged: everything needed to launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedVersion {
    /// The id of the requested (child-most) version.
    pub id: String,
    /// The ids of the chain, child first (starts with `id`).
    pub chain: Vec<String>,
    /// The version type.
    pub kind: String,
    /// The release time (RFC 3339), as given.
    pub release_time: String,
    /// The main class.
    pub main_class: String,
    /// Game and JVM arguments, both formats normalized.
    pub arguments: Arguments,
    /// The libraries, in class path order.
    pub libraries: Vec<LibraryEntry>,
    /// Downloads by name.
    pub downloads: BTreeMap<String, Download>,
    /// The asset index, if the version has one.
    pub asset_index: Option<AssetIndexRef>,
    /// The asset index id, if given.
    pub assets: Option<String>,
    /// The Java runtime asked for, if any.
    pub java_version: Option<JavaVersion>,
    /// The client log configuration, if any.
    pub logging: Option<LoggingConfig>,
    /// The id of the version whose client jar (`downloads.client`) is used.
    pub client_jar: String,
}

impl ResolvedVersion {
    /// The Java runtime to use: `javaVersion`, or [`JavaVersion::legacy_default`] when the
    /// version has none.
    pub fn java_version_or_default(&self) -> JavaVersion {
        self.java_version
            .clone()
            .unwrap_or_else(JavaVersion::legacy_default)
    }

    /// The asset index id: `assetIndex.id`, else `assets`, else `legacy`.
    pub fn asset_index_id(&self) -> &str {
        self.asset_index
            .as_ref()
            .map(|a| a.id.as_str())
            .or(self.assets.as_deref())
            .unwrap_or("legacy")
    }
}

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

fn check_download(what: &str, sha1: &str, url: &str) -> Result<()> {
    if !is_sha1(sha1) {
        return Err(damaged(format!("{what}: invalid sha1")));
    }
    if !url.starts_with("https://") {
        return Err(damaged(format!("{what}: url is not https")));
    }
    Ok(())
}

impl VersionJson {
    /// Parses and validates one version JSON document.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Integrity`] for a damaged document (see the module documentation).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(damaged(format!(
                "version JSON is larger than {MAX_VERSION_BYTES} bytes"
            )));
        }
        let v: Self = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid version JSON: {e}")))?;
        v.validate()?;
        Ok(v)
    }

    fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(damaged("version JSON has no id"));
        }
        for (name, d) in &self.downloads {
            check_download(&format!("downloads.{name}"), &d.sha1, &d.url)?;
        }
        if let Some(a) = &self.asset_index {
            check_download("assetIndex", &a.sha1, &a.url)?;
        }
        if let Some(c) = self.logging.as_ref().and_then(|l| l.client.as_ref()) {
            check_download("logging.client.file", &c.file.sha1, &c.file.url)?;
        }
        if self.libraries.iter().any(|l| l.name.is_empty()) {
            return Err(damaged("a library has an empty name"));
        }
        Ok(())
    }

    /// Merges the `inheritsFrom` chain starting at `self`.
    ///
    /// `load_parent` returns the (parsed) version JSON with the given id, or an error such as
    /// `NotFound`; it is called once per parent, child to root. Pass a closure over the cached
    /// documents.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::Integrity`] when the chain is longer than [`MAX_INHERITANCE_DEPTH`], repeats
    /// an id, or the merged version has no main class; any error from `load_parent` as is.
    pub fn resolve(
        self,
        mut load_parent: impl FnMut(&str) -> Result<VersionJson>,
    ) -> Result<ResolvedVersion> {
        let mut chain = vec![self];
        let mut seen: HashSet<String> = chain.iter().map(|v| v.id.clone()).collect();
        while let Some(parent_id) = chain.last().and_then(|v| v.inherits_from.clone()) {
            if chain.len() >= MAX_INHERITANCE_DEPTH {
                return Err(damaged("inheritsFrom chain is too long"));
            }
            if !seen.insert(parent_id.clone()) {
                return Err(damaged(format!("inheritsFrom cycle at {parent_id}")));
            }
            let parent = load_parent(&parent_id)?;
            if parent.id != parent_id {
                return Err(damaged(format!(
                    "expected version {parent_id}, got {}",
                    parent.id
                )));
            }
            chain.push(parent);
        }
        merge(chain)
    }

    /// [`resolve`](Self::resolve) for a version without `inheritsFrom` (an error if it has one).
    ///
    /// # Errors
    ///
    /// As [`resolve`](Self::resolve); a parent is reported as `NotFound`.
    pub fn resolve_standalone(self) -> Result<ResolvedVersion> {
        self.resolve(|id| Err(Error::not_found(format!("parent version {id}"))))
    }
}

/// Merges a chain given child first.
fn merge(chain: Vec<VersionJson>) -> Result<ResolvedVersion> {
    let ids: Vec<String> = chain.iter().map(|v| v.id.clone()).collect();
    let client_jar = chain
        .iter()
        .find_map(|v| v.jar.clone())
        .or_else(|| chain.last().map(|v| v.id.clone()))
        .unwrap_or_default();

    let mut main_class = None;
    let mut kind = String::new();
    let mut release_time = String::new();
    let mut args: Option<Arguments> = None;
    let mut libraries: Vec<LibraryEntry> = Vec::new();
    let mut downloads = BTreeMap::new();
    let mut asset_index = None;
    let mut assets = None;
    let mut java_version = None;
    let mut logging = None;

    // Root parent first, child last, so the child overrides.
    for v in chain.into_iter().rev() {
        if v.main_class.is_some() {
            main_class = v.main_class;
        }
        if !v.kind.is_empty() {
            kind = v.kind;
        }
        if !v.release_time.is_empty() {
            release_time = v.release_time;
        }
        match (v.arguments, v.minecraft_arguments) {
            (Some(new), _) => match args.as_mut() {
                Some(acc) => acc.extend(new),
                None => args = Some(new),
            },
            (None, Some(legacy)) => {
                let new = Arguments::from_legacy(&legacy);
                match args.as_mut() {
                    Some(acc) => acc.game = new.game,
                    None => args = Some(new),
                }
            }
            (None, None) => {}
        }
        let own: HashSet<String> = v.libraries.iter().map(LibraryEntry::key).collect();
        libraries.retain(|l| !own.contains(&l.key()));
        let mut merged = v.libraries;
        merged.append(&mut libraries);
        libraries = merged;
        downloads.extend(v.downloads);
        asset_index = v.asset_index.or(asset_index);
        assets = v.assets.or(assets);
        java_version = v.java_version.or(java_version);
        logging = v.logging.and_then(|l| l.client).or(logging);
    }

    let id = ids.first().cloned().unwrap_or_default();
    Ok(ResolvedVersion {
        main_class: main_class.ok_or_else(|| damaged(format!("{id}: no mainClass")))?,
        id,
        chain: ids,
        kind,
        release_time,
        arguments: args.unwrap_or_else(|| Arguments {
            game: Vec::new(),
            jvm: default_jvm(),
        }),
        libraries,
        downloads,
        asset_index,
        assets,
        java_version,
        logging,
        client_jar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arguments::{Variables, resolve};
    use crate::rules::{Features, RuleContext};
    use vl_core::platform::{Arch, Os, Platform};

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/version/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    fn load(name: &str) -> VersionJson {
        VersionJson::parse(&fixture(name)).unwrap()
    }

    fn loader(id: &str) -> Result<VersionJson> {
        match id {
            "1.21.11" => Ok(load("modern.json")),
            "1.12.2" => Ok(load("legacy_args.json")),
            other => Err(Error::not_found(other)),
        }
    }

    #[test]
    fn modern_format() {
        let r = load("modern.json").resolve_standalone().unwrap();
        assert_eq!(r.id, "1.21.11");
        assert_eq!(r.main_class, "net.minecraft.client.main.Main");
        assert_eq!(r.kind, "release");
        assert_eq!(r.java_version.as_ref().unwrap().major_version, 21);
        assert_eq!(r.asset_index_id(), "99");
        assert_eq!(r.client_jar, "1.21.11");
        assert_eq!(r.downloads["client"].size, 1234);
        assert_eq!(r.downloads["client_mappings"].size, 99);
        assert_eq!(r.libraries.len(), 3);
        assert_eq!(r.libraries[1].rules.len(), 2);
        assert!(r.libraries[2].rest.contains_key("natives"));
        let log = r.logging.as_ref().unwrap();
        assert_eq!(log.file.id, "client-1.12.xml");
        assert!(!r.arguments.jvm.is_empty());
        // Unknown top-level fields are ignored.
        assert!(serde_json::to_string(&r).unwrap().len() > 100);
    }

    #[test]
    fn legacy_format_without_java_version() {
        let r = load("legacy_args.json").resolve_standalone().unwrap();
        assert_eq!(r.java_version, None);
        assert_eq!(r.java_version_or_default(), JavaVersion::legacy_default());
        assert_eq!(r.asset_index_id(), "1.12");
        let f = Features::new();
        let ctx = RuleContext::new(
            Platform {
                os: Os::Linux,
                arch: Arch::X86_64,
            },
            &f,
        );
        let vars = Variables::new()
            .with("auth_player_name", "Steve")
            .with("version_name", "1.12.2")
            .with("classpath", "cp");
        let game = resolve(&r.arguments.game, &ctx, &vars);
        assert_eq!(game[..4], ["--username", "Steve", "--version", "1.12.2"]);
        let jvm = resolve(&r.arguments.jvm, &ctx, &vars);
        assert_eq!(jvm.last().map(String::as_str), Some("cp"));
    }

    #[test]
    fn very_old_format_without_asset_index() {
        let r = load("ancient.json").resolve_standalone().unwrap();
        assert_eq!(r.asset_index_id(), "pre-1.6");
        assert_eq!(r.asset_index, None);
        assert_eq!(r.logging, None);
    }

    #[test]
    fn loader_profile_inherits_modern() {
        let r = load("loader_modern.json").resolve(loader).unwrap();
        assert_eq!(r.id, "fabric-loader-0.20.1-1.21.11");
        assert_eq!(r.chain, ["fabric-loader-0.20.1-1.21.11", "1.21.11"]);
        assert_eq!(
            r.main_class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(r.client_jar, "1.21.11");
        assert_eq!(r.java_version.as_ref().unwrap().major_version, 21);
        assert_eq!(r.asset_index_id(), "99");
        assert_eq!(
            r.downloads["client"].size, 1234,
            "downloads come from the parent"
        );
        // Child libraries first; the child's newer asm replaces the parent's.
        let names: Vec<&str> = r.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "net.fabricmc:fabric-loader:0.20.1",
                "org.ow2.asm:asm:9.9",
                "com.example:plain:1.0",
                "com.example:natives-lib:1.0",
            ]
        );
        // Arguments: parent's first, then the child's additions.
        let f = Features::new();
        let ctx = RuleContext::new(
            Platform {
                os: Os::Windows,
                arch: Arch::X86_64,
            },
            &f,
        );
        let vars = Variables::new().with("classpath", "cp");
        let jvm = resolve(&r.arguments.jvm, &ctx, &vars);
        assert_eq!(jvm.first().map(String::as_str), Some("-Xss1M"));
        assert_eq!(
            jvm.last().map(String::as_str),
            Some("-DFabricMcEmu= net.minecraft.client.main.Main ")
        );
    }

    #[test]
    fn loader_profile_inherits_legacy() {
        let r = load("loader_legacy.json").resolve(loader).unwrap();
        assert_eq!(r.chain, ["1.12.2-forge-14.23.5.2860", "1.12.2"]);
        assert_eq!(r.main_class, "net.minecraft.launchwrapper.Launch");
        // The child's minecraftArguments replace the parent's game arguments.
        let game: Vec<&str> = r
            .arguments
            .game
            .iter()
            .flat_map(|a| a.values.iter().map(String::as_str))
            .collect();
        assert_eq!(game.last(), Some(&"FMLTweaker"));
        assert!(game.contains(&"--tweakClass"));
        assert!(!game.contains(&"--version"));
        assert_eq!(r.arguments.jvm, crate::arguments::default_jvm());
        assert_eq!(r.client_jar, "1.12.2");
        assert_eq!(r.java_version, None);
    }

    #[test]
    fn jar_field_selects_client_jar() {
        let mut v = load("modern.json");
        v.jar = Some("other".to_owned());
        assert_eq!(v.resolve_standalone().unwrap().client_jar, "other");
    }

    #[test]
    fn inheritance_errors() {
        let child = load("loader_modern.json");
        let e = child
            .clone()
            .resolve(|_| Err(Error::not_found("x")))
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::NotFound);
        let e = child.clone().resolve_standalone().unwrap_err();
        assert_eq!(e.kind, ErrorKind::NotFound);

        // A cycle: the parent points back to the child.
        let mut parent = load("modern.json");
        parent.inherits_from = Some(child.id.clone());
        let e = child.clone().resolve(|_| Ok(parent.clone())).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Integrity);

        // The loader returns the wrong document.
        let e = child
            .clone()
            .resolve(|_| Ok(load("ancient.json")))
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Integrity);

        // A chain that never ends.
        let e = child
            .resolve(|id| {
                let mut v = load("modern.json");
                v.id = id.to_owned();
                v.inherits_from = Some(format!("{id}x"));
                Ok(v)
            })
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Integrity);
    }

    #[test]
    fn missing_main_class_is_an_error() {
        let mut v = load("modern.json");
        v.main_class = None;
        assert_eq!(
            v.resolve_standalone().unwrap_err().kind,
            ErrorKind::Integrity
        );
    }

    #[test]
    fn damaged_documents_are_errors() {
        let ok = String::from_utf8(fixture("modern.json")).unwrap();
        let bad: Vec<String> = vec![
            String::new(),
            "[]".to_owned(),
            "{\"id\":\"x\",\"libraries\":5}".to_owned(),
            "{\"libraries\":[]}".to_owned(),
            "{\"id\":\"\"}".to_owned(),
            "{\"id\":\"x\",\"libraries\":[{\"rules\":[]}]}".to_owned(),
            "{\"id\":\"x\",\"arguments\":{\"game\":[7]}}".to_owned(),
            ok.replace(
                "\"url\": \"https://example.invalid/client.jar\"",
                "\"url\": \"http://example.invalid/client.jar\"",
            ),
            ok.replace(
                "\"sha1\": \"cccccccccccccccccccccccccccccccccccccccc\"",
                "\"sha1\": \"nothex\"",
            ),
            ok.replace("\"size\": 1234", "\"size\": -1"),
        ];
        for (i, doc) in bad.iter().enumerate() {
            let e = VersionJson::parse(doc.as_bytes()).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Integrity, "case {i}: {doc}");
        }
        assert!(VersionJson::parse(&vec![b' '; MAX_VERSION_BYTES + 1]).is_err());
        assert!(VersionJson::parse(&[0xff, 0x00]).is_err());
    }

    #[test]
    fn upstream_shape_round_trips() {
        for name in [
            "modern.json",
            "legacy_args.json",
            "ancient.json",
            "loader_modern.json",
        ] {
            let v = load(name);
            let back: VersionJson =
                serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
            assert_eq!(v, back, "{name}");
        }
    }

    #[test]
    fn library_keys() {
        let key = |n: &str| {
            LibraryEntry {
                name: n.to_owned(),
                rules: vec![],
                rest: serde_json::Map::new(),
            }
            .key()
        };
        assert_eq!(key("a.b:c:1.0"), "a.b:c");
        assert_eq!(key("a.b:c:1.0:natives-linux"), "a.b:c:natives-linux");
        assert_eq!(key("a.b:c:1.0@zip"), "a.b:c");
        assert_eq!(key("weird"), "weird:");
    }
}
