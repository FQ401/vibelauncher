//! The HTTP seam of the sign-in flow.
//!
//! Sign-in sends POST requests with form and JSON bodies; this small trait is what the flow
//! needs, and tests substitute it. [`NetHttp`] runs it on the `vl-net` transport.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use vl_net::{
    HttpRequest, HttpTransport, NetConfig, NetError, PostRequest, UreqTransport, redact_url,
};

/// The longest response body the sign-in flow accepts.
const MAX_BODY: u64 = 1024 * 1024;

/// An HTTP method used by the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// GET.
    Get,
    /// POST.
    Post,
}

/// A request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// No body.
    None,
    /// `application/x-www-form-urlencoded` fields (encoded by the transport).
    Form(Vec<(String, String)>),
    /// `application/json` text.
    Json(String),
}

/// A request. Bodies and headers can carry secrets: never log a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequest {
    /// The method.
    pub method: Method,
    /// Absolute `https` URL.
    pub url: String,
    /// Extra headers (such as `Authorization`).
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

/// A response with its body read (capped at 1 MiB). 4xx and 5xx are returned, not raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResponse {
    /// The status code.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

/// An HTTP stack for the sign-in flow.
pub trait AuthHttp: Send + Sync {
    /// Sends the request and reads the response.
    fn send(&self, request: &AuthRequest) -> Result<AuthResponse, NetError>;
}

/// The real implementation on the shared [`HttpTransport`] of `vl-net` (timeouts, proxy,
/// `User-Agent` come from its configuration).
pub struct NetHttp {
    transport: Arc<dyn HttpTransport>,
    timeout: Duration,
}

impl NetHttp {
    /// An implementation over `transport`; `timeout` bounds each response body.
    pub fn new(transport: Arc<dyn HttpTransport>, timeout: Duration) -> Self {
        Self { transport, timeout }
    }

    /// The real stack built from the shared network configuration.
    pub fn from_config(config: &NetConfig) -> Result<Self, NetError> {
        Ok(Self::new(
            Arc::new(UreqTransport::new(config)?),
            config.request_timeout,
        ))
    }
}

impl AuthHttp for NetHttp {
    fn send(&self, request: &AuthRequest) -> Result<AuthResponse, NetError> {
        if !request.url.starts_with("https://") && !request.url.starts_with("http://127.0.0.1") {
            return Err(NetError::InvalidUrl("sign-in requires https".into()));
        }
        let response = match request.method {
            Method::Get => self.transport.get(&HttpRequest {
                url: request.url.clone(),
                headers: request.headers.clone(),
                body_timeout: self.timeout,
            })?,
            Method::Post => {
                let (content_type, body) = match &request.body {
                    Body::None => (None, Vec::new()),
                    Body::Form(fields) => (
                        Some("application/x-www-form-urlencoded".to_owned()),
                        form_encode(fields).into_bytes(),
                    ),
                    Body::Json(text) => (
                        Some("application/json".to_owned()),
                        text.clone().into_bytes(),
                    ),
                };
                self.transport.post(&PostRequest {
                    url: request.url.clone(),
                    headers: request.headers.clone(),
                    content_type,
                    body,
                    body_timeout: self.timeout,
                })?
            }
        };
        let status = response.status;
        let mut body = Vec::new();
        response
            .body
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|e| NetError::Connect(format!("{} while reading the response", e.kind())))?;
        if body.len() as u64 > MAX_BODY {
            return Err(NetError::TooLarge {
                limit: MAX_BODY,
                url: redact_url(&request.url),
            });
        }
        Ok(AuthResponse { status, body })
    }
}

/// Encodes `name=value&…` with percent-encoding.
pub(crate) fn form_encode(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        percent_encode_into(&mut out, k);
        out.push('=');
        percent_encode_into(&mut out, v);
    }
    out
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    percent_encode_into(&mut out, value);
    out
}

fn percent_encode_into(out: &mut String, value: &str) {
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

/// Decodes `%XX` escapes and `+` as space; invalid escapes are kept as they are.
pub(crate) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |n: usize| {
            bytes
                .get(n)
                .and_then(|c| char::from(*c).to_digit(16))
                .and_then(|d| u8::try_from(d).ok())
        };
        match b {
            b'%' => match (hex(i + 1), hex(i + 2)) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => out.push(b),
            },
            b'+' => out.push(b' '),
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes() {
        let fields = vec![
            (
                "scope".to_owned(),
                "XboxLive.signin offline_access".to_owned(),
            ),
            ("x".to_owned(), "a&b=c/é".to_owned()),
        ];
        let encoded = form_encode(&fields);
        assert_eq!(
            encoded,
            "scope=XboxLive.signin%20offline_access&x=a%26b%3Dc%2F%C3%A9"
        );
        assert_eq!(percent_decode("a%26b%3Dc%2F%C3%A9+z%2"), "a&b=c/é z%2");
    }
}
