//! The on-disk HTTP cache for metadata: one body file and one small JSON record per URL.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::NetError;
use crate::hash::sha256_hex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Meta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Seconds since the Unix epoch when the body was last confirmed by the server.
    pub checked_at: u64,
    pub len: u64,
}

pub(crate) struct Entry {
    pub meta: Meta,
    pub body: Vec<u8>,
}

impl Entry {
    /// Seconds since the entry was last confirmed.
    pub(crate) fn age_secs(&self) -> u64 {
        now_secs().saturating_sub(self.meta.checked_at)
    }
}

pub(crate) struct HttpCache {
    dir: PathBuf,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl HttpCache {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The file stem for `url`. Query strings are part of the key, never of a file name.
    fn stem(&self, url: &str) -> PathBuf {
        self.dir.join(&sha256_hex(url.as_bytes())[..32])
    }

    pub(crate) fn load(&self, url: &str) -> Option<Entry> {
        let stem = self.stem(url);
        let meta: Meta =
            serde_json::from_slice(&fs::read(stem.with_extension("json")).ok()?).ok()?;
        let body = fs::read(stem.with_extension("body")).ok()?;
        (body.len() as u64 == meta.len).then_some(Entry { meta, body })
    }

    pub(crate) fn store(
        &self,
        url: &str,
        body: &[u8],
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<(), NetError> {
        let stem = self.stem(url);
        let meta = Meta {
            etag,
            last_modified,
            checked_at: now_secs(),
            len: body.len() as u64,
        };
        fs::create_dir_all(&self.dir)?;
        // Drop the record first so a crash between the two writes reads as a miss.
        let _ = fs::remove_file(stem.with_extension("json"));
        write_atomic(&stem.with_extension("body"), body)?;
        write_atomic(&stem.with_extension("json"), &to_json(&meta)?)
    }

    /// Records that the server confirmed the cached body (a `304`).
    pub(crate) fn touch(&self, url: &str, entry: &Entry) -> Result<(), NetError> {
        let meta = Meta {
            checked_at: now_secs(),
            ..entry.meta.clone()
        };
        write_atomic(&self.stem(url).with_extension("json"), &to_json(&meta)?)
    }
}

fn to_json(meta: &Meta) -> Result<Vec<u8>, NetError> {
    serde_json::to_vec(meta).map_err(|e| NetError::Io(e.to_string()))
}

/// Writes `data` to a sibling temporary file, flushes it and renames it over `path`.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), NetError> {
    use std::io::Write;
    let tmp = crate::download::temp_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(NetError::from)
}
