//! Client configuration.

use std::path::PathBuf;
use std::time::Duration;

/// Which proxy to use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProxyConfig {
    /// The standard environment (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY`).
    #[default]
    System,
    /// Connect directly.
    None,
    /// An `http://`, `https://`, `socks5://` or `socks5h://` proxy URL (may carry credentials).
    Url(String),
}

/// How retries are spaced: `base * 2^(failures - 1)`, capped at `max_delay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total tries including the first one (at least 1).
    pub attempts: u32,
    /// Delay before the second try.
    pub base_delay: Duration,
    /// Longest delay between tries.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// The delay after the failed try number `failed` (1 for the first failure).
    pub fn delay(&self, failed: u32) -> Duration {
        let shift = failed.saturating_sub(1).min(20);
        self.base_delay
            .saturating_mul(1u32 << shift)
            .min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

/// Everything a [`Client`](crate::Client) needs.
#[derive(Debug, Clone)]
pub struct NetConfig {
    /// The launcher version for the `User-Agent`.
    pub app_version: String,
    /// Connection setup timeout.
    pub connect_timeout: Duration,
    /// Longest silence while waiting for the response head or for more body bytes.
    pub read_timeout: Duration,
    /// Longest total time of one metadata request (`get_bytes` / `get_json`).
    pub request_timeout: Duration,
    /// Longest total time of one file download attempt.
    pub download_timeout: Duration,
    /// Retry spacing.
    pub retry: RetryPolicy,
    /// Downloads running at once across the whole client.
    pub max_parallel: usize,
    /// Default cap for a metadata response.
    pub max_response_bytes: u64,
    /// Default cap for a downloaded file without a known size.
    pub max_download_bytes: u64,
    /// The proxy.
    pub proxy: ProxyConfig,
    /// Directory of the HTTP cache; `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    /// Never touch the network: answer from the cache or fail.
    pub offline: bool,
}

impl NetConfig {
    /// Defaults for the launcher at `app_version`.
    pub fn new(app_version: impl Into<String>) -> Self {
        Self {
            app_version: app_version.into(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(60),
            download_timeout: Duration::from_secs(30 * 60),
            retry: RetryPolicy::default(),
            max_parallel: 8,
            max_response_bytes: 64 * 1024 * 1024,
            max_download_bytes: 2 * 1024 * 1024 * 1024,
            proxy: ProxyConfig::System,
            cache_dir: None,
            offline: false,
        }
    }

    /// `VibeLauncher/<version> (+https://github.com/FQ401/vibelauncher)`.
    pub fn user_agent(&self) -> String {
        format!(
            "VibeLauncher/{} (+https://github.com/FQ401/vibelauncher)",
            self.app_version
        )
    }
}

impl Default for NetConfig {
    fn default() -> Self {
        Self::new(env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            attempts: 6,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        let d: Vec<_> = (1..=5).map(|n| p.delay(n).as_millis()).collect();
        assert_eq!(d, [100, 200, 400, 500, 500]);
    }

    #[test]
    fn user_agent_format() {
        assert_eq!(
            NetConfig::new("1.2.3").user_agent(),
            "VibeLauncher/1.2.3 (+https://github.com/FQ401/vibelauncher)"
        );
    }
}
