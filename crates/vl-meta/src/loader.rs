//! Mod loader metadata: version lists, the normalized loader profile, the merge with the vanilla
//! version JSON, and the cache envelope.
//!
//! Every loader source ends in the same two products:
//!
//! - a [`LoaderVersions`] list (which loader versions exist for a game version, stable or not);
//! - a [`LoaderProfile`]: the main class, extra libraries (with hashes), JVM and game arguments
//!   and the game version it inherits from.
//!
//! Fabric and Quilt serve a ready launch profile (`.../profile/json`); [`LoaderProfile::normalize`]
//! reads such a document. Sources that produce a profile some other way (Forge and NeoForge
//! installers) build a [`LoaderProfile`] from their own parsers and share everything below:
//! [`LoaderProfile::merge`], [`CachedProfile`] and the cache key.
//!
//! All input is hostile: unknown fields are ignored, a damaged document (bad JSON, invalid
//! SHA-1, duplicate libraries, wrong game version, no main class) is an
//! [`ErrorKind::Integrity`] error, never a panic.
//!
//! # Normalization of libraries
//!
//! A library that carries its hash and URL at the top level (Fabric/Quilt style: `url` is a Maven
//! repository, `sha1` and `size` describe the jar) is rewritten to the Mojang shape,
//! `downloads.artifact` with the derived path, so that [`crate::libraries::plan`] sees one format.
//! Entries that already have `downloads` are kept as they are.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use vl_core::component::{ComponentKind, ComponentRef};
use vl_core::error::{Error, ErrorKind, Result};

use crate::arguments::Arguments;
use crate::manifest::is_sha1;
use crate::version::{LibraryEntry, MAX_VERSION_BYTES, ResolvedVersion, VersionJson};

/// The format of [`LoaderProfile`] and [`CachedProfile`] as written by this launcher.
pub const PROFILE_FORMAT: u32 = 1;

/// The most loader versions accepted in one list.
pub const MAX_LOADER_VERSIONS: usize = 100_000;

fn damaged(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Integrity, message)
}

/// One loader version offered for a game version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderVersion {
    /// The loader's version string.
    pub version: String,
    /// Whether it is a stable release (not a beta, alpha or snapshot). Forge has no such
    /// division: all its versions count as stable.
    pub stable: bool,
    /// Whether the loader's authors promote it as the recommended build for the game version.
    #[serde(default)]
    pub recommended: bool,
    /// Whether it is the newest version for the game version.
    #[serde(default)]
    pub latest: bool,
}

/// The loader versions available for one game version, newest first (as the source orders them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderVersions {
    /// The loader.
    pub kind: ComponentKind,
    /// The game version the list is for.
    pub game_version: String,
    /// The versions.
    pub versions: Vec<LoaderVersion>,
}

#[derive(Deserialize)]
struct RawLoaderVersion {
    version: String,
    #[serde(default)]
    stable: Option<bool>,
}

/// A version string with a `-` suffix (`0.31.0-beta.4`, `1.0-rc1`) is a pre-release.
pub fn looks_stable(version: &str) -> bool {
    !version.contains('-') && !version.contains('+')
}

impl LoaderVersions {
    /// Builds a list from versions the source parser has already extracted.
    ///
    /// # Errors
    ///
    /// `Integrity` for an empty or repeated version.
    pub fn new(
        kind: ComponentKind,
        game_version: &str,
        versions: Vec<LoaderVersion>,
    ) -> Result<Self> {
        let mut seen = HashSet::new();
        for v in &versions {
            if v.version.is_empty() {
                return Err(damaged("a loader version is empty"));
            }
            if !seen.insert(v.version.as_str()) {
                return Err(damaged(format!("duplicate loader version {}", v.version)));
            }
        }
        let mut versions = versions;
        if let Some(first) = versions.first_mut()
            && !first.latest
        {
            // Lists are newest first.
            first.latest = true;
        }
        Ok(Self {
            kind,
            game_version: game_version.to_owned(),
            versions,
        })
    }

    /// Parses a Fabric-style list, a JSON array of `{ "version": ..., "stable": ... }` (Fabric and
    /// Quilt). When `stable` is absent it is guessed with [`looks_stable`].
    ///
    /// # Errors
    ///
    /// `Integrity` for a document that is not such an array, is too long, or has duplicates.
    pub fn parse_array(kind: ComponentKind, game_version: &str, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(damaged("loader version list is too large"));
        }
        let raw: Vec<RawLoaderVersion> = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid loader version list: {e}")))?;
        if raw.len() > MAX_LOADER_VERSIONS {
            return Err(damaged("loader version list has too many entries"));
        }
        let versions = raw
            .into_iter()
            .map(|r| LoaderVersion {
                stable: r.stable.unwrap_or_else(|| looks_stable(&r.version)),
                version: r.version,
                ..LoaderVersion::default()
            })
            .collect();
        Self::new(kind, game_version, versions)
    }

    /// Parses the per-game list of Fabric and Quilt (`.../versions/loader/<game>`): a JSON array
    /// of objects whose `loader` member is `{ "version": ..., "stable": ... }`. Everything else in
    /// the entries is ignored. An empty array means the loader does not support the game.
    ///
    /// # Errors
    ///
    /// `Integrity` for a document that is not such an array, is too long, or has duplicates.
    pub fn parse_for_game(kind: ComponentKind, game_version: &str, bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Entry {
            loader: RawLoaderVersion,
        }
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(damaged("loader version list is too large"));
        }
        let raw: Vec<Entry> = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid loader version list: {e}")))?;
        if raw.len() > MAX_LOADER_VERSIONS {
            return Err(damaged("loader version list has too many entries"));
        }
        let versions = raw
            .into_iter()
            .map(|e| LoaderVersion {
                stable: e
                    .loader
                    .stable
                    .unwrap_or_else(|| looks_stable(&e.loader.version)),
                version: e.loader.version,
                ..LoaderVersion::default()
            })
            .collect();
        Self::new(kind, game_version, versions)
    }

    /// The versions, optionally only the stable ones.
    pub fn filtered(&self, stable_only: bool) -> Vec<&LoaderVersion> {
        self.versions
            .iter()
            .filter(|v| !stable_only || v.stable)
            .collect()
    }

    /// The version to use when none is asked for: the one marked recommended, else the newest
    /// stable one, else the newest of any kind. For Forge, whose versions are all stable, that is
    /// the recommended build, or the latest when the game version has none.
    pub fn recommended(&self) -> Option<&LoaderVersion> {
        self.versions
            .iter()
            .find(|v| v.recommended)
            .or_else(|| self.versions.iter().find(|v| v.stable))
            .or(self.versions.first())
    }

    /// Finds `version` for this game version.
    ///
    /// # Errors
    ///
    /// `Unsupported` when the loader has no version for the game at all; `NotFound` when only the
    /// requested version is missing.
    pub fn check(&self, version: &str) -> Result<&LoaderVersion> {
        if self.versions.is_empty() {
            return Err(Error::unsupported(format!(
                "{} does not support Minecraft {}",
                self.kind.display_name(),
                self.game_version
            )));
        }
        self.versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| {
                Error::not_found(format!(
                    "{} {version} is not available for Minecraft {}",
                    self.kind.display_name(),
                    self.game_version
                ))
            })
    }
}

/// A loader profile in the launcher's own format: what a loader adds to the vanilla game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoaderProfile {
    /// The format version, [`PROFILE_FORMAT`].
    pub format: u32,
    /// The loader.
    pub kind: ComponentKind,
    /// The loader version.
    pub loader_version: String,
    /// The profile's own version id (becomes the id of the merged version).
    pub id: String,
    /// The game version it inherits from.
    pub inherits_from: String,
    /// The version type as the source states it (usually `release`).
    pub version_type: String,
    /// The release time, as given.
    pub release_time: String,
    /// The main class that replaces the game's.
    pub main_class: String,
    /// Extra JVM and game arguments, appended after the game's.
    pub arguments: Arguments,
    /// Whether the game arguments replace the game's instead of being appended (old profiles
    /// with a single `minecraftArguments` string).
    #[serde(default)]
    pub replaces_game_arguments: bool,
    /// Extra libraries, with hashes where the source gives them.
    pub libraries: Vec<LibraryEntry>,
}

impl LoaderProfile {
    /// Reads an upstream launch profile (the Fabric/Quilt `profile/json` document, or any
    /// version-JSON-shaped profile) into the normalized form.
    ///
    /// `game_version` and `loader_version` are what was requested; the document must agree on the
    /// game version.
    ///
    /// # Errors
    ///
    /// `Integrity` for a damaged document (see the module documentation).
    pub fn normalize(
        kind: ComponentKind,
        game_version: &str,
        loader_version: &str,
        bytes: &[u8],
    ) -> Result<Self> {
        let doc = VersionJson::parse(bytes)?;
        let inherits_from = doc
            .inherits_from
            .ok_or_else(|| damaged(format!("{}: profile has no inheritsFrom", doc.id)))?;
        if inherits_from != game_version {
            return Err(damaged(format!(
                "profile {} is for Minecraft {inherits_from}, not {game_version}",
                doc.id
            )));
        }
        let (arguments, replaces_game_arguments) = match (doc.arguments, doc.minecraft_arguments) {
            (Some(a), _) => (a, false),
            (None, Some(legacy)) => (
                Arguments {
                    jvm: Vec::new(),
                    ..Arguments::from_legacy(&legacy)
                },
                true,
            ),
            (None, None) => (Arguments::default(), false),
        };
        let profile = Self {
            format: PROFILE_FORMAT,
            kind,
            loader_version: loader_version.to_owned(),
            id: doc.id,
            inherits_from,
            version_type: doc.kind,
            release_time: doc.release_time,
            main_class: doc
                .main_class
                .filter(|m| !m.is_empty())
                .ok_or_else(|| damaged("profile has no mainClass"))?,
            arguments,
            replaces_game_arguments,
            libraries: doc
                .libraries
                .into_iter()
                .map(normalize_library)
                .collect::<Result<_>>()?,
        };
        profile.validate()?;
        Ok(profile)
    }

    /// Checks the invariants every profile source must keep.
    ///
    /// # Errors
    ///
    /// `Integrity` for a wrong format, a missing field, a duplicate library, or an invalid hash.
    pub fn validate(&self) -> Result<()> {
        if self.format != PROFILE_FORMAT {
            return Err(damaged(format!("unknown profile format {}", self.format)));
        }
        if !self.kind.is_loader() {
            return Err(damaged("a loader profile cannot be for the game"));
        }
        if self.id.is_empty() || self.main_class.is_empty() || self.inherits_from.is_empty() {
            return Err(damaged("profile is missing id, mainClass or inheritsFrom"));
        }
        let mut seen = HashSet::new();
        for lib in &self.libraries {
            if !seen.insert(lib.key()) {
                return Err(damaged(format!("duplicate library {}", lib.key())));
            }
            check_library_hashes(lib)?;
        }
        Ok(())
    }

    /// The component this profile belongs to.
    pub fn component(&self) -> ComponentRef {
        ComponentRef::new(self.kind, self.loader_version.clone())
    }

    /// The profile as a version JSON that inherits from the game.
    pub fn to_version_json(&self) -> VersionJson {
        let (arguments, minecraft_arguments) = if self.replaces_game_arguments {
            let line = self
                .arguments
                .game
                .iter()
                .flat_map(|a| a.values.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            (None, Some(line))
        } else {
            (Some(self.arguments.clone()), None)
        };
        VersionJson {
            arguments,
            minecraft_arguments,
            id: self.id.clone(),
            inherits_from: Some(self.inherits_from.clone()),
            kind: self.version_type.clone(),
            main_class: Some(self.main_class.clone()),
            libraries: self.libraries.clone(),
            release_time: self.release_time.clone(),
            ..VersionJson::default()
        }
    }

    /// Merges the profile into the vanilla version it inherits from: the loader's main class
    /// wins, its libraries come first on the class path (replacing same-named vanilla ones), its
    /// arguments follow the game's. See [`crate::version`] for the full rules.
    ///
    /// # Errors
    ///
    /// `Integrity` when `vanilla` is not the version the profile inherits from or itself needs a
    /// parent.
    pub fn merge(&self, vanilla: &VersionJson) -> Result<ResolvedVersion> {
        if vanilla.id != self.inherits_from {
            return Err(damaged(format!(
                "profile {} needs {}, got {}",
                self.id, self.inherits_from, vanilla.id
            )));
        }
        self.to_version_json().resolve(|id| {
            if id == vanilla.id {
                Ok(vanilla.clone())
            } else {
                Err(Error::not_found(format!("parent version {id}")))
            }
        })
    }
}

fn normalize_library(mut lib: LibraryEntry) -> Result<LibraryEntry> {
    check_coordinate(&lib.name)?;
    if lib.rest.contains_key("downloads") {
        return Ok(lib);
    }
    let sha1 = lib.rest.remove("sha1");
    let size = lib.rest.remove("size");
    let url = lib
        .rest
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let (Some(sha1), Some(url)) = (sha1.as_ref(), url) else {
        // No hash or no repository: keep the entry for the derived-path rules of the plan; a
        // hash without a repository is put back so it is not lost.
        if let Some(s) = sha1 {
            lib.rest.insert("sha1".to_owned(), s);
        }
        if let Some(s) = size {
            lib.rest.insert("size".to_owned(), s);
        }
        return Ok(lib);
    };
    let sha1 = sha1
        .as_str()
        .filter(|s| is_sha1(s))
        .ok_or_else(|| damaged(format!("{}: invalid sha1", lib.name)))?;
    if !url.starts_with("https://") {
        return Err(damaged(format!("{}: repository is not https", lib.name)));
    }
    let size = size
        .and_then(|s| s.as_u64())
        .ok_or_else(|| damaged(format!("{}: missing or invalid size", lib.name)))?;
    let path = maven_path(&lib.name)?;
    let base = if url.ends_with('/') {
        url
    } else {
        format!("{url}/")
    };
    let mut downloads = Map::new();
    downloads.insert(
        "artifact".to_owned(),
        json!({ "path": path, "url": format!("{base}{path}"), "sha1": sha1, "size": size }),
    );
    lib.rest.remove("url");
    lib.rest
        .insert("downloads".to_owned(), Value::Object(downloads));
    Ok(lib)
}

fn check_library_hashes(lib: &LibraryEntry) -> Result<()> {
    let bad = |what: &str| damaged(format!("{}: invalid sha1 in {what}", lib.name));
    if let Some(s) = lib.rest.get("sha1").and_then(Value::as_str)
        && !is_sha1(s)
    {
        return Err(bad("library"));
    }
    let Some(downloads) = lib.rest.get("downloads") else {
        return Ok(());
    };
    let artifacts = downloads.get("artifact").into_iter().chain(
        downloads
            .get("classifiers")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(Map::values),
    );
    for a in artifacts {
        if let Some(s) = a.get("sha1").and_then(Value::as_str)
            && !is_sha1(s)
        {
            return Err(bad("downloads"));
        }
    }
    Ok(())
}

fn safe_part(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains(['/', '\\', ':'])
        && !s.chars().any(char::is_control)
}

pub(crate) fn check_coordinate(name: &str) -> Result<()> {
    let base = name.split('@').next().unwrap_or(name);
    let parts: Vec<&str> = base.split(':').collect();
    if parts.len() < 3 || parts.len() > 4 || !parts.iter().all(|p| safe_part(p)) {
        return Err(damaged(format!("invalid library coordinate {name}")));
    }
    Ok(())
}

/// The repository path of a `group:artifact:version[:classifier][@ext]` coordinate.
///
/// # Errors
///
/// `Integrity` for a coordinate that is malformed or unsafe as a path.
pub fn maven_path(name: &str) -> Result<String> {
    check_coordinate(name)?;
    let (base, ext) = name.split_once('@').unwrap_or((name, "jar"));
    let p: Vec<&str> = base.split(':').collect();
    let (group, artifact, version) = (p[0].replace('.', "/"), p[1], p[2]);
    let classifier = p.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    if !safe_part(ext) {
        return Err(damaged(format!("invalid library coordinate {name}")));
    }
    Ok(format!(
        "{group}/{artifact}/{version}/{artifact}-{version}{classifier}.{ext}"
    ))
}

/// The key of a profile in the cache, a relative path with `/` separators:
/// `loader/<uid>/<game version>/<loader version>.json`.
///
/// # Errors
///
/// `InvalidInput` when a version cannot be a file-name part.
pub fn profile_cache_key(
    kind: ComponentKind,
    game_version: &str,
    loader_version: &str,
) -> Result<String> {
    if !kind.is_loader() || !safe_part(game_version) || !safe_part(loader_version) {
        return Err(Error::invalid_input("invalid loader profile key"));
    }
    Ok(format!(
        "loader/{}/{game_version}/{loader_version}.json",
        kind.uid()
    ))
}

/// A normalized profile as stored in the metadata cache, with what the network layer needs to
/// revalidate it (`ETag`) and when it was fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedProfile {
    /// The format version, [`PROFILE_FORMAT`].
    pub format: u32,
    /// The `ETag` of the upstream response, if it sent one.
    #[serde(default)]
    pub etag: Option<String>,
    /// When the profile was fetched, Unix seconds.
    pub fetched_unix: i64,
    /// The profile.
    pub profile: LoaderProfile,
}

impl CachedProfile {
    /// Wraps a profile for storing.
    pub fn new(profile: LoaderProfile, etag: Option<String>, fetched_unix: i64) -> Self {
        Self {
            format: PROFILE_FORMAT,
            etag,
            fetched_unix,
            profile,
        }
    }

    /// The bytes to write to the cache.
    ///
    /// # Errors
    ///
    /// `Internal` if serialization fails (it does not for valid profiles).
    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
    }

    /// Reads a cache file. A file that is damaged, of another format, or whose profile fails
    /// validation is an `Integrity` error; the caller drops it and fetches again.
    ///
    /// # Errors
    ///
    /// `Integrity`, as described.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(damaged("cached profile is too large"));
        }
        let c: Self = serde_json::from_slice(bytes)
            .map_err(|e| damaged(format!("invalid cached profile: {e}")))?;
        if c.format != PROFILE_FORMAT {
            return Err(damaged(format!("unknown cache format {}", c.format)));
        }
        c.profile.validate()?;
        Ok(c)
    }

    /// Whether the entry is younger than `max_age_secs` at `now`. An entry from the future (clock
    /// change) is not fresh.
    pub fn is_fresh(&self, now: i64, max_age_secs: i64) -> bool {
        (0..=max_age_secs).contains(&(now - self.fetched_unix))
    }
}

/// Where a profile comes from, in the order the caller should try: a fresh cache entry, the
/// network, a stale cache entry (offline), then failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    /// Use the cache entry as is.
    Cache,
    /// Fetch (sending the cached `ETag`, if any).
    Fetch,
}

/// Decides whether a cached profile can be used without the network.
pub fn choose_source(cached: Option<&CachedProfile>, now: i64, max_age_secs: i64) -> ProfileSource {
    match cached {
        Some(c) if c.is_fresh(now, max_age_secs) => ProfileSource::Cache,
        _ => ProfileSource::Fetch,
    }
}

/// The result of an attempt to fetch a profile, as the network layer reports it.
#[derive(Debug)]
pub enum FetchOutcome {
    /// A new document (`200`) with its `ETag`.
    Fresh(Vec<u8>, Option<String>),
    /// The cached copy is current (`304`).
    NotModified,
    /// The source could not be reached or answered with an error.
    Failed(Error),
}

/// Combines a fetch attempt with the cache into the profile to use. Falls back to a stale cache
/// entry when the source is unreachable, so installed versions launch offline.
///
/// # Errors
///
/// The fetch error (with its kind) when there is no cache to fall back on; `Integrity` when the
/// fresh document is damaged and there is no usable cache; `NotFound` for a `304` without cache.
pub fn resolve_profile(
    kind: ComponentKind,
    game_version: &str,
    loader_version: &str,
    cached: Option<CachedProfile>,
    outcome: FetchOutcome,
    now: i64,
) -> Result<CachedProfile> {
    match outcome {
        FetchOutcome::Fresh(bytes, etag) => {
            match LoaderProfile::normalize(kind, game_version, loader_version, &bytes) {
                Ok(p) => Ok(CachedProfile::new(p, etag, now)),
                Err(e) => cached.ok_or(e),
            }
        }
        FetchOutcome::NotModified => cached
            .map(|c| CachedProfile {
                fetched_unix: now,
                ..c
            })
            .ok_or_else(|| Error::not_found("source answered not modified but nothing is cached")),
        FetchOutcome::Failed(e) => cached.ok_or(e),
    }
}
