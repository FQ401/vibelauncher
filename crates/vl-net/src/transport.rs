//! The HTTP transport seam: [`HttpTransport`] is what the client needs from an HTTP stack, so
//! tests and other crates can substitute it. [`UreqTransport`] is the real implementation
//! (blocking, rustls, no OpenSSL).

use std::fmt;
use std::io::Read;
use std::time::Duration;

use crate::config::{NetConfig, ProxyConfig};
use crate::error::{NetError, redact_url};

/// A GET request. Its `Debug` output names the headers without their values.
#[derive(Clone)]
pub struct HttpRequest {
    /// Absolute `http` / `https` URL.
    pub url: String,
    /// Extra headers (conditional request headers); `User-Agent` is added by the transport.
    pub headers: Vec<(String, String)>,
    /// Longest the whole body transfer may take.
    pub body_timeout: Duration,
}

/// A POST request with an in-memory body. Its `Debug` output names the headers without their
/// values and gives the body's length only.
#[derive(Clone)]
pub struct PostRequest {
    /// Absolute `http` / `https` URL.
    pub url: String,
    /// Extra headers; `User-Agent` is added by the transport.
    pub headers: Vec<(String, String)>,
    /// The `Content-Type` of the body; `None` sends an empty body without one.
    pub content_type: Option<String>,
    /// The body.
    pub body: Vec<u8>,
    /// Longest the whole body transfer may take.
    pub body_timeout: Duration,
}

/// Header names only: a value may be a secret (an API key).
struct HeaderNames<'a>(&'a [(String, String)]);

impl fmt::Debug for HeaderNames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &redact_url(&self.url))
            .field("headers", &HeaderNames(&self.headers))
            .field("body_timeout", &self.body_timeout)
            .finish()
    }
}

impl fmt::Debug for PostRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostRequest")
            .field("url", &redact_url(&self.url))
            .field("headers", &HeaderNames(&self.headers))
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .field("body_timeout", &self.body_timeout)
            .finish()
    }
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

    /// Sends a POST request. Redirects are not followed: the 3xx answer is returned. Transports
    /// that only serve downloads (such as test fakes) keep the default, which fails.
    fn post(&self, request: &PostRequest) -> Result<HttpResponse, NetError> {
        let _ = request;
        Err(NetError::Connect(
            "this transport does not support POST".into(),
        ))
    }
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
        Ok(into_response(response))
    }

    fn post(&self, request: &PostRequest) -> Result<HttpResponse, NetError> {
        let mut call = self
            .agent
            .post(&request.url)
            .config()
            .max_redirects(0)
            .timeout_recv_body(Some(request.body_timeout))
            .build();
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }
        let result = match &request.content_type {
            Some(content_type) => call
                .header("Content-Type", content_type)
                .send(request.body.as_slice()),
            None => call.send_empty(),
        };
        let response = result.map_err(|e| map_error(&e, &request.url))?;
        Ok(into_response(response))
    }
}

fn into_response(response: ureq::http::Response<ureq::Body>) -> HttpResponse {
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
    HttpResponse {
        status,
        headers,
        body: Box::new(response.into_body().into_reader()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_header_values_and_bodies() {
        let headers = vec![("x-api-key".to_string(), "secret-value".to_string())];
        let get = HttpRequest {
            url: "https://api.example.test/v1?token=query-secret".into(),
            headers: headers.clone(),
            body_timeout: Duration::from_secs(1),
        };
        let post = PostRequest {
            url: "https://api.example.test/v1".into(),
            headers,
            content_type: Some("application/json".into()),
            body: b"body-secret".to_vec(),
            body_timeout: Duration::from_secs(1),
        };
        let shown = format!("{get:?} {post:?}");
        assert!(shown.contains("x-api-key"), "{shown}");
        for secret in ["secret-value", "query-secret", "body-secret"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}
