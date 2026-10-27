//! File downloads: hash and size checks, temporary file with atomic rename, parallel batches.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::client::{Client, check_url, content_length, read_error};
use crate::control::{Control, Tally};
use crate::error::{NetError, redact_url};
use crate::hash::{Checksum, Hasher};
use crate::transport::HttpRequest;

/// One file to fetch.
#[derive(Debug, Clone)]
pub struct Download {
    /// Absolute `http` / `https` URL.
    pub url: String,
    /// Where the verified file ends up. Parent directories are created.
    pub dest: PathBuf,
    /// The digest the content must match.
    pub checksum: Option<Checksum>,
    /// The exact size in bytes.
    pub size: Option<u64>,
    /// Size cap used when `size` is unknown; the config default when `None`.
    pub max_bytes: Option<u64>,
}

impl Download {
    /// A download of `url` to `dest` with no expectations.
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            checksum: None,
            size: None,
            max_bytes: None,
        }
    }

    /// Requires the content to match `checksum`.
    pub fn checksum(mut self, checksum: Checksum) -> Self {
        self.checksum = Some(checksum);
        self
    }

    /// Requires the content to be exactly `size` bytes.
    pub fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// Caps the size when the exact size is unknown.
    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// The verified file.
    pub path: PathBuf,
    /// Its size in bytes.
    pub bytes: u64,
    /// The file was already there and matched, so nothing was fetched.
    pub reused: bool,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique sibling path for writing `path` before the atomic rename.
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(format!(".{}.{n}.part", std::process::id()));
    path.with_file_name(name)
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Client {
    /// Downloads one file, verifying size and checksum before it appears at `dest`. An existing
    /// file that already matches the expectations is kept. Waits for a free download slot.
    pub fn download(&self, spec: &Download, control: &Control) -> Result<Downloaded, NetError> {
        let tally = Tally::new(spec.size, control.progress.clone());
        self.download_counted(spec, control, &tally)
    }

    /// Downloads many files, at most `max_parallel` at a time, and returns one result per spec in
    /// order. Progress is the sum over all files. `dest` paths must differ. After a cancel the
    /// files not yet started report [`NetError::Cancelled`].
    pub fn download_all(
        &self,
        specs: &[Download],
        control: &Control,
    ) -> Vec<Result<Downloaded, NetError>> {
        let total = specs
            .iter()
            .try_fold(0u64, |sum, s| s.size.map(|n| sum.saturating_add(n)));
        let tally = Tally::new(total, control.progress.clone());
        let results: Mutex<Vec<Option<Result<Downloaded, NetError>>>> =
            Mutex::new(specs.iter().map(|_| None).collect());
        let next = AtomicUsize::new(0);
        let work = || {
            loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(spec) = specs.get(i) else { return };
                let result = self.download_counted(spec, control, &tally);
                if let Some(slot) = results.lock().unwrap_or_else(|e| e.into_inner()).get_mut(i) {
                    *slot = Some(result);
                }
            }
        };
        let workers = self.inner.config.max_parallel.clamp(1, specs.len().max(1));
        std::thread::scope(|scope| {
            for n in 1..workers {
                // If a thread cannot be started, the remaining workers still drain the queue.
                let _ = std::thread::Builder::new()
                    .name(format!("vl-net-{n}"))
                    .spawn_scoped(scope, work);
            }
            work();
        });
        results
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into_iter()
            .map(|r| r.unwrap_or(Err(NetError::Cancelled)))
            .collect()
    }

    fn download_counted(
        &self,
        spec: &Download,
        control: &Control,
        tally: &Tally,
    ) -> Result<Downloaded, NetError> {
        check_url(&spec.url)?;
        control.cancel.check()?;
        if let Some(bytes) = existing_matches(spec) {
            tally.add(bytes);
            return Ok(Downloaded {
                path: spec.dest.clone(),
                bytes,
                reused: true,
            });
        }
        if self.inner.config.offline {
            return Err(NetError::Offline(redact_url(&spec.url)));
        }
        let _slot = self.inner.slots.acquire(&control.cancel)?;
        let bytes = self.with_retries(control, || {
            let mut added = 0;
            let result = self.download_once(spec, control, tally, &mut added);
            if result.is_err() {
                tally.sub(added);
            }
            result
        })?;
        Ok(Downloaded {
            path: spec.dest.clone(),
            bytes,
            reused: false,
        })
    }

    fn download_once(
        &self,
        spec: &Download,
        control: &Control,
        tally: &Tally,
        added: &mut u64,
    ) -> Result<u64, NetError> {
        let config = &self.inner.config;
        let response = self.inner.transport.get(&HttpRequest {
            url: spec.url.clone(),
            headers: Vec::new(),
            body_timeout: config.download_timeout,
        })?;
        if response.status != 200 {
            return Err(NetError::Status {
                status: response.status,
                url: redact_url(&spec.url),
            });
        }
        let cap = spec
            .size
            .or(spec.max_bytes)
            .unwrap_or(config.max_download_bytes);
        let too_large = || NetError::TooLarge {
            limit: cap,
            url: redact_url(&spec.url),
        };
        let declared = content_length(&response.headers);
        if declared.is_some_and(|n| n > cap) {
            return Err(too_large());
        }
        if let (Some(declared), Some(size)) = (declared, spec.size)
            && declared != size
        {
            return Err(NetError::SizeMismatch {
                expected: size,
                actual: declared,
            });
        }
        if let Some(parent) = spec.dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = TempFile(temp_path(&spec.dest));
        let mut file = fs::File::create(&temp.0)?;
        let mut hasher = Hasher::new(spec.checksum.as_ref());
        let mut body = response.body;
        let mut chunk = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            control.cancel.check()?;
            let n = body.read(&mut chunk).map_err(read_error)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > cap {
                return Err(too_large());
            }
            let data = &chunk[..n];
            file.write_all(data)?;
            hasher.update(data);
            tally.add(n as u64);
            *added += n as u64;
        }
        if declared.is_some_and(|n| n != total) {
            return Err(NetError::Connect("response body ended early".into()));
        }
        if let Some(size) = spec.size
            && size != total
        {
            return Err(NetError::SizeMismatch {
                expected: size,
                actual: total,
            });
        }
        hasher.verify(spec.checksum.as_ref())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp.0, &spec.dest)?;
        Ok(total)
    }
}

/// The size of `spec.dest` when it exists and satisfies every expectation of `spec`. A spec with
/// no expectations never reuses a file.
fn existing_matches(spec: &Download) -> Option<u64> {
    if spec.size.is_none() && spec.checksum.is_none() {
        return None;
    }
    let len = fs::metadata(&spec.dest).ok().filter(|m| m.is_file())?.len();
    if spec.size.is_some_and(|s| s != len) {
        return None;
    }
    if spec.checksum.is_some() {
        let mut hasher = Hasher::new(spec.checksum.as_ref());
        let mut file = fs::File::open(&spec.dest).ok()?;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&chunk[..n]);
        }
        hasher.verify(spec.checksum.as_ref()).ok()?;
    }
    Some(len)
}
