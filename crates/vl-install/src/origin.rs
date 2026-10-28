//! Where installed files came from.
//!
//! The launcher records, for each file it installs, the platform, project id and version id in
//! `<instance>/.vibelauncher/resources.json`. The record is a hint, not the truth: the folder
//! belongs to the user, who may rename, disable, replace or delete files. So a record counts for a
//! file only while the file's SHA-1 equals the recorded one (a renamed or disabled file keeps its
//! origin, a replaced file loses it), records of deleted files are dropped when the file is next
//! written, and a damaged file or damaged entry is skipped, never fatal.
//!
//! Update checks read the records through [`OriginStore::load`].

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vl_core::catalog::Provider;
use vl_core::resources::ResourceKind;
use vl_core::{Error, Result};

/// The format of `resources.json` that this launcher writes.
pub const ORIGIN_FORMAT: u32 = 1;

/// The most records read from one file.
const MAX_RECORDS: usize = 20_000;

/// How one installed file came to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The folder.
    pub kind: ResourceKind,
    /// The file name without `.disabled`, as installed.
    pub file: String,
    /// Lowercase hex SHA-1 of the installed file.
    pub sha1: String,
    /// Lowercase hex SHA-512 of the installed file.
    #[serde(default)]
    pub sha512: Option<String>,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// The platform.
    pub provider: Provider,
    /// Project id on the platform.
    pub project_id: String,
    /// Project slug when known.
    #[serde(default)]
    pub slug: Option<String>,
    /// Version id on the platform.
    pub version_id: String,
    /// Version number when known.
    #[serde(default)]
    pub version_number: Option<String>,
    /// When it was installed, Unix seconds.
    #[serde(default)]
    pub installed_unix: i64,
    /// Installed only because another project needs it.
    #[serde(default)]
    pub dependency: bool,
}

/// What [`OriginStore::load`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Loaded {
    /// The records that could be read.
    pub origins: Vec<Origin>,
    /// The file or some of its entries were unreadable and skipped.
    pub damaged: bool,
}

/// The origin records of one instance.
#[derive(Debug, Clone)]
pub struct OriginStore {
    path: PathBuf,
}

/// The seconds since the Unix epoch.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl OriginStore {
    /// The records in `path` (nothing is read or created yet).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file of the records.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the records. A missing file is no records; a damaged one is reported and skipped.
    pub fn load(&self) -> Loaded {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
            Err(_) => {
                return Loaded {
                    origins: Vec::new(),
                    damaged: true,
                };
            }
        };
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            return Loaded {
                origins: Vec::new(),
                damaged: true,
            };
        };
        let Some(list) = document.get("resources").and_then(Value::as_array) else {
            return Loaded {
                origins: Vec::new(),
                damaged: true,
            };
        };
        let mut damaged = list.len() > MAX_RECORDS;
        let mut origins = Vec::new();
        for entry in list.iter().take(MAX_RECORDS) {
            match serde_json::from_value::<Origin>(entry.clone()) {
                Ok(o) if is_hex(&o.sha1, 40) => origins.push(o),
                _ => damaged = true,
            }
        }
        Loaded { origins, damaged }
    }

    /// Writes the records atomically. A damaged file that is being replaced is kept beside it as
    /// `resources.json.bad`.
    pub fn save(&self, origins: &[Origin]) -> Result<()> {
        if self.load().damaged && self.path.is_file() {
            let mut bad = self.path.clone().into_os_string();
            bad.push(".bad");
            let _ = std::fs::rename(&self.path, PathBuf::from(bad));
        }
        let document = json!({ "format": ORIGIN_FORMAT, "resources": origins });
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|e| Error::internal(format!("origin records: {e}")))?;
        vl_store::write_atomic(&self.path, &bytes)?;
        Ok(())
    }

    /// Adds `added` and drops what is superseded or gone: records of the same folder and file
    /// name or the same SHA-1 as an added one, and records for which `exists` is false.
    pub fn record(
        &self,
        added: Vec<Origin>,
        exists: impl Fn(&Origin) -> bool,
        removed: &[(ResourceKind, String)],
    ) -> Result<()> {
        let mut all: Vec<Origin> = self
            .load()
            .origins
            .into_iter()
            .filter(|o| {
                !added
                    .iter()
                    .any(|a| a.sha1 == o.sha1 || (a.kind == o.kind && a.file == o.file))
                    && !removed.iter().any(|(k, f)| *k == o.kind && *f == o.file)
                    && exists(o)
            })
            .collect();
        all.extend(added);
        self.save(&all)
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}
