//! The client: cached metadata requests with retries.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::cache::HttpCache;
use crate::config::NetConfig;
use crate::control::{Control, Semaphore};
use crate::error::{NetError, redact_url};
use crate::transport::{HttpRequest, HttpResponse, HttpTransport, PostRequest, UreqTransport};

/// Where the bytes of a metadata request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Downloaded now (no cache, a new version, or no earlier entry).
    Network,
    /// The server confirmed the cached copy (`304 Not Modified`).
    Revalidated,
    /// The cached copy was young enough that the network was not asked.
    Fresh,
    /// The network was unavailable (offline mode, timeout, server error): an old copy was used.
    Stale,
}

/// The body of a metadata request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The response body.
    pub bytes: Vec<u8>,
    /// Where it came from.
    pub status: CacheStatus,
}

/// Options of [`Client::get_bytes`] and [`Client::get_json`].
#[derive(Debug, Clone)]
pub struct GetOptions {
    /// Cap on the response size; the config default when `None`.
    pub max_bytes: Option<u64>,
    /// Skip the network when the cached copy was confirmed less than this long ago.
    pub max_age: Option<Duration>,
    /// Use the HTTP cache (when the client has one).
    pub use_cache: bool,
}

impl Default for GetOptions {
    fn default() -> Self {
        Self {
            max_bytes: None,
            max_age: None,
            use_cache: true,
        }
    }
}

/// Observes the network requests of one call: lets a caller pace itself against a server's rate
/// limit. Cache hits and offline reads send no request and do not reach the hooks.
pub trait RequestHooks: Send + Sync {
    /// Called before every network attempt (retries included); may block to wait, and fails to
    /// stop the call (for example when cancelled).
    fn before_request(&self, control: &Control) -> Result<(), NetError>;

    /// Called with the status and the lowercase-named headers of every response, whatever its
    /// status.
    fn after_response(&self, status: u16, headers: &[(String, String)]);

    /// Headers added to every request of the call (such as an API key). The values are sent
    /// only; they are never logged, cached or put into errors.
    fn headers(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

pub(crate) struct Inner {
    pub transport: Arc<dyn HttpTransport>,
    pub config: NetConfig,
    pub cache: Option<HttpCache>,
    pub slots: Semaphore,
}

/// The network client. Cheap to clone; clones share the cache, the transport and the download
/// limit.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

impl Client {
    /// A client on the real HTTP stack.
    pub fn new(config: NetConfig) -> Result<Self, NetError> {
        let transport = UreqTransport::new(&config)?;
        Ok(Self::with_transport(config, Arc::new(transport)))
    }

    /// A client on a given transport (for tests and other crates' tests).
    pub fn with_transport(config: NetConfig, transport: Arc<dyn HttpTransport>) -> Self {
        let cache = config.cache_dir.clone().map(HttpCache::new);
        let slots = Semaphore::new(config.max_parallel);
        Self {
            inner: Arc::new(Inner {
                transport,
                config,
                cache,
                slots,
            }),
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &NetConfig {
        &self.inner.config
    }

    /// GET `url` and return the body, using the HTTP cache: conditional requests with `ETag` /
    /// `Last-Modified`, `304` answered from disk, and an old copy when the network fails.
    pub fn get_bytes(
        &self,
        url: &str,
        options: &GetOptions,
        control: &Control,
    ) -> Result<Fetched, NetError> {
        self.get_bytes_with(url, options, control, None)
    }

    /// [`get_bytes`](Self::get_bytes) with [`RequestHooks`] around every network attempt.
    pub fn get_bytes_with(
        &self,
        url: &str,
        options: &GetOptions,
        control: &Control,
        hooks: Option<&dyn RequestHooks>,
    ) -> Result<Fetched, NetError> {
        check_url(url)?;
        control.cancel.check()?;
        let cache = if options.use_cache {
            self.inner.cache.as_ref()
        } else {
            None
        };
        let entry = cache.and_then(|c| c.load(url));
        let stale = |entry: crate::cache::Entry| Fetched {
            bytes: entry.body,
            status: CacheStatus::Stale,
        };
        if self.inner.config.offline {
            return entry
                .map(stale)
                .ok_or_else(|| NetError::Offline(redact_url(url)));
        }
        if let (Some(e), Some(max_age)) = (&entry, options.max_age)
            && e.age_secs() <= max_age.as_secs()
        {
            return Ok(Fetched {
                bytes: entry.map(|e| e.body).unwrap_or_default(),
                status: CacheStatus::Fresh,
            });
        }
        let cap = options
            .max_bytes
            .unwrap_or(self.inner.config.max_response_bytes);
        let result = self.with_retries(control, || {
            self.fetch_once(url, entry.as_ref(), cap, control, hooks)
        });
        match (result, entry) {
            (Ok(Fetch::NotModified), Some(entry)) => {
                if let Some(cache) = cache {
                    let _ = cache.touch(url, &entry);
                }
                Ok(Fetched {
                    bytes: entry.body,
                    status: CacheStatus::Revalidated,
                })
            }
            (Ok(Fetch::NotModified), None) => Err(NetError::Status {
                status: 304,
                url: redact_url(url),
            }),
            (Ok(Fetch::Body(body)), _) => {
                if let Some(cache) = cache {
                    // The cache is best effort: a full disk must not fail the request.
                    let _ = cache.store(url, &body.bytes, body.etag, body.last_modified);
                }
                Ok(Fetched {
                    bytes: body.bytes,
                    status: CacheStatus::Network,
                })
            }
            (Err(e), Some(entry)) if e.is_transport() => Ok(stale(entry)),
            (Err(e), _) => Err(e),
        }
    }

    /// POST `body` to `url` and return the response body. Nothing is cached; retries, timeouts
    /// and the size cap are the same as for a GET. Redirects are not followed.
    pub fn post_bytes(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        max_bytes: Option<u64>,
        control: &Control,
        hooks: Option<&dyn RequestHooks>,
    ) -> Result<Vec<u8>, NetError> {
        check_url(url)?;
        control.cancel.check()?;
        if self.inner.config.offline {
            return Err(NetError::Offline(redact_url(url)));
        }
        let cap = max_bytes.unwrap_or(self.inner.config.max_response_bytes);
        self.with_retries(control, || {
            if let Some(h) = hooks {
                h.before_request(control)?;
            }
            let response = self.inner.transport.post(&PostRequest {
                url: url.to_string(),
                headers: hooks.map(|h| h.headers()).unwrap_or_default(),
                content_type: Some(content_type.to_string()),
                body: body.to_vec(),
                body_timeout: self.inner.config.request_timeout,
            })?;
            if let Some(h) = hooks {
                h.after_response(response.status, &response.headers);
            }
            if response.status != 200 {
                return Err(NetError::Status {
                    status: response.status,
                    url: redact_url(url),
                });
            }
            read_capped(response, cap, url, control)
        })
    }

    /// [`get_bytes`](Self::get_bytes) and parse the body as JSON.
    pub fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        options: &GetOptions,
        control: &Control,
    ) -> Result<(T, CacheStatus), NetError> {
        let fetched = self.get_bytes(url, options, control)?;
        let value = serde_json::from_slice(&fetched.bytes)
            .map_err(|e| NetError::Decode(format!("{} ({})", e.classify_str(), redact_url(url))))?;
        Ok((value, fetched.status))
    }

    /// Runs `attempt` until it succeeds, fails with a permanent error, is cancelled or runs out of
    /// tries, sleeping with exponential backoff between tries.
    pub(crate) fn with_retries<T>(
        &self,
        control: &Control,
        mut attempt: impl FnMut() -> Result<T, NetError>,
    ) -> Result<T, NetError> {
        let policy = self.inner.config.retry;
        let tries = policy.attempts.max(1);
        let mut failed = 0;
        loop {
            control.cancel.check()?;
            match attempt() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    failed += 1;
                    if !e.is_retryable() || failed >= tries {
                        return Err(e);
                    }
                    control.cancel.sleep(policy.delay(failed))?;
                }
            }
        }
    }

    fn fetch_once(
        &self,
        url: &str,
        entry: Option<&crate::cache::Entry>,
        cap: u64,
        control: &Control,
        hooks: Option<&dyn RequestHooks>,
    ) -> Result<Fetch, NetError> {
        let mut headers = Vec::new();
        if let Some(entry) = entry {
            if let Some(etag) = &entry.meta.etag {
                headers.push(("If-None-Match".to_string(), etag.clone()));
            }
            if let Some(modified) = &entry.meta.last_modified {
                headers.push(("If-Modified-Since".to_string(), modified.clone()));
            }
        }
        if let Some(h) = hooks {
            headers.extend(h.headers());
        }
        let request = HttpRequest {
            url: url.to_string(),
            headers,
            body_timeout: self.inner.config.request_timeout,
        };
        if let Some(h) = hooks {
            h.before_request(control)?;
        }
        let response = self.inner.transport.get(&request)?;
        if let Some(h) = hooks {
            h.after_response(response.status, &response.headers);
        }
        if response.status == 304 && entry.is_some() {
            return Ok(Fetch::NotModified);
        }
        if response.status != 200 {
            return Err(NetError::Status {
                status: response.status,
                url: redact_url(url),
            });
        }
        let etag = response.header("etag").map(str::to_string);
        let last_modified = response.header("last-modified").map(str::to_string);
        let bytes = read_capped(response, cap, url, control)?;
        Ok(Fetch::Body(Body {
            bytes,
            etag,
            last_modified,
        }))
    }
}

/// Reads a response body up to `cap` bytes, checking the declared length.
fn read_capped(
    response: HttpResponse,
    cap: u64,
    url: &str,
    control: &Control,
) -> Result<Vec<u8>, NetError> {
    let too_large = || NetError::TooLarge {
        limit: cap,
        url: redact_url(url),
    };
    let declared = content_length(&response.headers);
    if declared.is_some_and(|n| n > cap) {
        return Err(too_large());
    }
    let mut body = response.body;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        control.cancel.check()?;
        let n = body.read(&mut chunk).map_err(read_error)?;
        if n == 0 {
            break;
        }
        if (bytes.len() + n) as u64 > cap {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    if declared.is_some_and(|n| n != bytes.len() as u64) {
        return Err(NetError::Connect("response body ended early".into()));
    }
    Ok(bytes)
}

enum Fetch {
    NotModified,
    Body(Body),
}

struct Body {
    bytes: Vec<u8>,
    etag: Option<String>,
    last_modified: Option<String>,
}

pub(crate) fn content_length(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.trim().parse().ok())
}

pub(crate) fn read_error(e: std::io::Error) -> NetError {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            NetError::Timeout("reading the response".into())
        }
        kind => NetError::Connect(format!("reading the response failed ({kind})")),
    }
}

/// Accepts absolute `http` / `https` URLs only.
pub(crate) fn check_url(url: &str) -> Result<(), NetError> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(())
    } else {
        Err(NetError::InvalidUrl(
            "only http and https are allowed".into(),
        ))
    }
}

trait ClassifyStr {
    fn classify_str(&self) -> &'static str;
}

impl ClassifyStr for serde_json::Error {
    fn classify_str(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "invalid json",
            serde_json::error::Category::Data => "unexpected json shape",
            serde_json::error::Category::Eof => "truncated json",
        }
    }
}
