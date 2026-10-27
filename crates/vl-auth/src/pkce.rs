//! Authorization code flow with PKCE through the system browser and a loopback redirect.
//!
//! The launcher listens on `127.0.0.1` on a free port, the browser is sent to the Microsoft
//! sign-in page with that address as the redirect, and the answer carries the code. The
//! `vibelauncher://` scheme is not used until the launcher registers it with the OS.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use vl_net::CancelToken;

use crate::error::AuthError;
use crate::flow::{Authenticator, Session};
use crate::http::{percent_decode, percent_encode};
use crate::msa;
use crate::secret::Secret;

/// How the redirect address is spelled. Both are registered for the application; the second is
/// the fallback for browsers that mishandle the IP form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopbackHost {
    /// `http://127.0.0.1:<port>`.
    #[default]
    Ip,
    /// `http://localhost:<port>`.
    Localhost,
}

impl LoopbackHost {
    fn name(self) -> &'static str {
        match self {
            Self::Ip => "127.0.0.1",
            Self::Localhost => "localhost",
        }
    }
}

/// A browser sign-in in progress: the page to open and the listener that waits for the answer.
pub struct BrowserLogin {
    url: String,
    redirect_uri: String,
    state: String,
    verifier: Secret,
    listener: TcpListener,
}

impl BrowserLogin {
    /// Binds the loopback listener and builds the sign-in URL.
    pub fn begin(auth: &Authenticator, host: LoopbackHost) -> Result<Self, AuthError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| AuthError::Protocol(format!("cannot listen on loopback: {}", e.kind())))?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::Protocol(format!("loopback address: {}", e.kind())))?
            .port();
        let redirect_uri = format!("http://{}:{port}", host.name());
        let verifier = random_token(32)?;
        let state = random_token(16)?;
        let challenge = challenge_for(&verifier);
        let config = auth.config();
        let query = [
            ("client_id", config.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("response_mode", "query"),
            ("scope", &config.scope),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
        Ok(Self {
            url: format!("{}?{query}", config.endpoint("authorize")),
            redirect_uri,
            state,
            verifier: Secret::new(verifier),
            listener,
        })
    }

    /// The sign-in page to open in the browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The redirect address (without a path).
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits for the browser to come back and returns the authorization code.
    fn wait_for_code(&self, timeout: Duration, cancel: &CancelToken) -> Result<String, AuthError> {
        self.listener
            .set_nonblocking(true)
            .map_err(|e| AuthError::Protocol(format!("loopback listener: {}", e.kind())))?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_cancelled() {
                return Err(AuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(AuthError::Expired);
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = self.handle_connection(stream) {
                        return result;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    return Err(AuthError::Protocol(format!(
                        "loopback accept: {}",
                        e.kind()
                    )));
                }
            }
        }
    }

    /// Reads one request. `None` means it was not the redirect (a favicon request, junk).
    fn handle_connection(&self, mut stream: TcpStream) -> Option<Result<String, AuthError>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut line = String::new();
        BufReader::new((&stream).take(8192))
            .read_line(&mut line)
            .ok()?;
        let query = request_query(&line)?;
        let get = |name: &str| {
            query
                .split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| *k == name)
                .map(|(_, v)| percent_decode(v))
        };
        if get("state").as_deref() != Some(self.state.as_str()) {
            respond(
                &mut stream,
                400,
                "Sign-in failed: the answer did not match this launcher.",
            );
            return Some(Err(AuthError::Protocol("loopback state mismatch".into())));
        }
        if let Some(error) = get("error") {
            respond(
                &mut stream,
                200,
                "Sign-in was not completed. You can close this tab.",
            );
            return Some(Err(if error == "access_denied" {
                AuthError::Declined
            } else {
                AuthError::Protocol(format!("authorization: {error}"))
            }));
        }
        match get("code") {
            Some(code) if !code.is_empty() => {
                respond(
                    &mut stream,
                    200,
                    "Signed in. You can close this tab and return to the launcher.",
                );
                Some(Ok(code))
            }
            _ => {
                respond(&mut stream, 400, "Sign-in failed: no code in the answer.");
                Some(Err(AuthError::Protocol(
                    "authorization answer has no code".into(),
                )))
            }
        }
    }
}

impl Authenticator {
    /// Browser sign-in: `open` receives the page to show (open it in the system browser or print
    /// it); the call waits up to `timeout` for the browser to return, then runs the rest of the
    /// chain.
    pub fn browser_sign_in(
        &self,
        host: LoopbackHost,
        open: &mut dyn FnMut(&str),
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<Session, AuthError> {
        let login = BrowserLogin::begin(self, host)?;
        open(login.url());
        let code = login.wait_for_code(timeout, cancel)?;
        let tokens = msa::exchange_code(
            self.http.as_ref(),
            &self.config,
            &code,
            &login.verifier,
            login.redirect_uri(),
        )?;
        self.session_from_tokens(tokens, None)
    }
}

/// The query string of a `GET /?… HTTP/1.1` request line.
fn request_query(line: &str) -> Option<&str> {
    let target = line.strip_prefix("GET ")?.split_whitespace().next()?;
    let (_, query) = target.split_once('?')?;
    Some(query)
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>VibeLauncher</title></head>\
         <body><p>{message}</p></body></html>"
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// `bytes` random bytes as unpadded base64url.
fn random_token(bytes: usize) -> Result<String, AuthError> {
    let mut buf = vec![0_u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|_| AuthError::Protocol("no system randomness available".into()))?;
    Ok(base64url(&buf))
}

/// The S256 code challenge of a verifier.
fn challenge_for(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            let index = usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0);
            out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'A')));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::MsaConfig;
    use crate::testing::{FakeClock, Scripted, json};

    #[test]
    fn base64url_and_challenge_match_the_rfc_vector() {
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        // RFC 7636, appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    fn authenticator(http: &Arc<Scripted>) -> Authenticator {
        Authenticator::with_clock(
            http.clone(),
            Arc::new(FakeClock::new(0)),
            MsaConfig::with_client_id("test-client"),
        )
    }

    fn browser(login_url: &str, redirect: &str, extra: &str) {
        // Plays the browser: reads state from the sign-in URL and calls the redirect back.
        let state = login_url
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("state="))
            .unwrap()
            .to_owned();
        let port = redirect.rsplit(':').next().unwrap().to_owned();
        let path = format!("/?{extra}state={state}");
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply);
        });
    }

    #[test]
    fn browser_flow_exchanges_the_code() {
        let http = Arc::new(Scripted::default());
        http.route(
            "oauth2/v2.0/token",
            [json(200, r#"{"access_token":"A","refresh_token":"R"}"#)],
        );
        http.route("user.auth.xboxlive.com", [json(401, "{}")]);
        let auth = authenticator(&http);
        let mut opened = String::new();
        let err = auth
            .browser_sign_in(
                LoopbackHost::Ip,
                &mut |url| {
                    opened = url.to_owned();
                    let redirect = url
                        .split('&')
                        .find_map(|p| p.strip_prefix("redirect_uri="))
                        .map(percent_decode)
                        .unwrap();
                    browser(url, &redirect, "code=CODE%2F1&");
                },
                Duration::from_secs(10),
                &CancelToken::new(),
            )
            .unwrap_err();
        // The chain continued past the code exchange (and stopped at the scripted 401).
        assert!(matches!(err, AuthError::Protocol(_)));
        assert!(
            opened
                .starts_with("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?")
        );
        assert!(opened.contains("code_challenge_method=S256"));
        assert!(opened.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"));
        let body = &http.bodies()[0];
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=CODE%2F1"));
        assert!(body.contains("code_verifier="));
        // The challenge sent in the URL belongs to the verifier sent in the exchange.
        let verifier = body
            .split('&')
            .find_map(|p| p.strip_prefix("code_verifier="))
            .unwrap();
        assert!(opened.contains(&format!("code_challenge={}", challenge_for(verifier))));
    }

    #[test]
    fn denied_and_mismatched_answers_fail() {
        let http = Arc::new(Scripted::default());
        let auth = authenticator(&http);
        let err = auth
            .browser_sign_in(
                LoopbackHost::Localhost,
                &mut |url| {
                    let redirect = url
                        .split('&')
                        .find_map(|p| p.strip_prefix("redirect_uri="))
                        .map(percent_decode)
                        .unwrap();
                    assert!(redirect.starts_with("http://localhost:"));
                    browser(url, &redirect, "error=access_denied&");
                },
                Duration::from_secs(10),
                &CancelToken::new(),
            )
            .unwrap_err();
        assert_eq!(err, AuthError::Declined);

        let login = BrowserLogin::begin(&auth, LoopbackHost::Ip).unwrap();
        let port = login.redirect_uri().rsplit(':').next().unwrap().to_owned();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            write!(s, "GET /?code=X&state=wrong HTTP/1.1\r\n\r\n").unwrap();
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply);
        });
        let err = login
            .wait_for_code(Duration::from_secs(10), &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, AuthError::Protocol(_)));
    }

    #[test]
    fn waiting_can_be_cancelled_or_time_out() {
        let http = Arc::new(Scripted::default());
        let auth = authenticator(&http);
        let login = BrowserLogin::begin(&auth, LoopbackHost::Ip).unwrap();
        assert_eq!(
            login.wait_for_code(Duration::from_millis(50), &CancelToken::new()),
            Err(AuthError::Expired)
        );
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            login.wait_for_code(Duration::from_secs(5), &cancel),
            Err(AuthError::Cancelled)
        );
    }

    #[test]
    fn request_line_parsing() {
        assert_eq!(request_query("GET /?a=1&b=2 HTTP/1.1\r\n"), Some("a=1&b=2"));
        assert_eq!(request_query("GET /favicon.ico HTTP/1.1\r\n"), None);
        assert_eq!(request_query("POST /?a=1 HTTP/1.1\r\n"), None);
    }
}
