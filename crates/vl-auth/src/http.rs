//! The HTTP seam of the sign-in flow.
//!
//! Sign-in needs POST requests with form and JSON bodies, which the metadata client of `vl-net`
//! does not offer; this small trait is what the flow needs, and tests substitute it.

use std::io::Read;

use vl_net::{NetConfig, NetError, ProxyConfig, redact_url};

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

/// The real implementation on `ureq` with rustls, configured like the rest of the launcher's
/// network access (timeouts, proxy, `User-Agent`).
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl UreqHttp {
    /// Builds an agent from the shared network configuration.
    pub fn new(config: &NetConfig) -> Result<Self, NetError> {
        let mut builder = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(config.user_agent())
            .timeout_connect(Some(config.connect_timeout))
            .timeout_per_read(Some(config.read_timeout))
            .timeout_recv_response(Some(config.read_timeout))
            .timeout_global(Some(config.request_timeout));
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

impl AuthHttp for UreqHttp {
    fn send(&self, request: &AuthRequest) -> Result<AuthResponse, NetError> {
        if !request.url.starts_with("https://") && !request.url.starts_with("http://127.0.0.1") {
            return Err(NetError::InvalidUrl("sign-in requires https".into()));
        }
        let result = match request.method {
            Method::Get => {
                let mut call = self.agent.get(&request.url);
                for (k, v) in &request.headers {
                    call = call.header(k, v);
                }
                call.call()
            }
            Method::Post => {
                let mut call = self.agent.post(&request.url);
                for (k, v) in &request.headers {
                    call = call.header(k, v);
                }
                match &request.body {
                    Body::None => call.send_empty(),
                    Body::Form(fields) => call
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .send(form_encode(fields).as_bytes()),
                    Body::Json(text) => call
                        .header("Content-Type", "application/json")
                        .send(text.as_bytes()),
                }
            }
        };
        let response = result.map_err(|e| map_error(&e, &request.url))?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
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

fn map_error(error: &ureq::Error, url: &str) -> NetError {
    let url = redact_url(url);
    match error {
        ureq::Error::Timeout(t) => NetError::Timeout(format!("{t} for {url}")),
        ureq::Error::BadUri(_) | ureq::Error::InvalidProxyUrl => {
            NetError::InvalidUrl("bad url".into())
        }
        ureq::Error::Io(e) => NetError::Connect(format!("{} ({url})", e.kind())),
        _ => NetError::Connect(format!("request to {url} failed")),
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
