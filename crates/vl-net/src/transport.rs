//! The HTTP transport seam: [`HttpTransport`] is what the client needs from an HTTP stack, so
//! tests and other crates can substitute it. [`UreqTransport`] is the real implementation
//! (blocking, rustls, no OpenSSL).

use std::io::Read;
use std::time::Duration;

use crate::config::{NetConfig, ProxyConfig};
use crate::error::{NetError, redact_url};

/// A GET request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Absolute `http` / `https` URL.
    pub url: String,
    /// Extra headers (conditional request headers); `User-Agent` is added by the transport.
    pub headers: Vec<(String, String)>,
    /// Longest the whole body transfer may take.
    pub body_timeout: Duration,
}

/// A response whose body is still to be read.
pub struct HttpResponse {
    /// The status code (redirects are already followed; 4xx / 5xx are returned, not raised).
    pub status: u16,
    /// Response headers with lowercase names.
    pub headers: Vec<(String, String)>,
    /// The body stream.
    pub body: Box<dyn Read + Send>,
}

impl HttpResponse {
    /// The value of the header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// An HTTP stack. Implementations return non-success statuses as responses and map transport
/// failures to [`NetError::Timeout`] / [`NetError::Connect`].
pub trait HttpTransport: Send + Sync {
    /// Sends the request and returns the response head with a streaming body.
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError>;
}

/// The real transport on `ureq` with rustls.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// Builds an agent from the configuration (timeouts, proxy, user agent).
    pub fn new(config: &NetConfig) -> Result<Self, NetError> {
        let mut builder = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(5)
            .user_agent(config.user_agent())
            .timeout_connect(Some(config.connect_timeout))
            .timeout_per_read(Some(config.read_timeout))
            .timeout_recv_response(Some(config.read_timeout));
        match &config.proxy {
            ProxyConfig::System => {}
            ProxyConfig::None => builder = builder.proxy(None),
            ProxyConfig::Url(url) => {
                let proxy = ureq::Proxy::new(url)
                    .map_err(|_| NetError::InvalidUrl("invalid proxy url".into()))?;
                builder = builder.proxy(Some(proxy));
            }
        }
        Ok(Self {
            agent: ureq::Agent::new_with_config(builder.build()),
        })
    }
}

impl HttpTransport for UreqTransport {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        let mut call = self
            .agent
            .get(&request.url)
            .config()
            .timeout_recv_body(Some(request.body_timeout))
            .build();
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }
        let response = call.call().map_err(|e| map_error(&e, &request.url))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().ok()?.to_string(),
                ))
            })
            .collect();
        Ok(HttpResponse {
            status,
            headers,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

fn map_error(error: &ureq::Error, url: &str) -> NetError {
    let url = redact_url(url);
    match error {
        ureq::Error::Timeout(t) => NetError::Timeout(format!("{t} for {url}")),
        ureq::Error::BadUri(_) | ureq::Error::InvalidProxyUrl => {
            NetError::InvalidUrl("bad url".into())
        }
        ureq::Error::Io(e) => NetError::Connect(format!("{} ({url})", e.kind())),
        // Other variants may embed URLs with credentials; keep the message generic.
        _ => NetError::Connect(format!("request to {url} failed")),
    }
}
