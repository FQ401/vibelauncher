//! Microsoft identity platform requests: device code, authorization code, refresh.

use std::time::Duration;

use serde::Deserialize;
use vl_net::CancelToken;

use crate::config::MsaConfig;
use crate::error::AuthError;
use crate::flow::Clock;
use crate::http::{AuthHttp, AuthRequest, Body, Method};
use crate::secret::Secret;

/// What the user needs to finish a device-code sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The code to enter.
    pub user_code: String,
    /// The page to enter it on.
    pub verification_uri: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Seconds between polls.
    pub interval: u64,
    pub(crate) device_code: Secret,
}

/// The tokens of a Microsoft sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsaTokens {
    /// The access token for Xbox Live.
    pub access_token: Secret,
    /// The refresh token (a refresh may omit it; then the old one stays valid).
    pub refresh_token: Option<Secret>,
}

#[derive(Deserialize)]
struct DeviceCodeBody {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Polls that failed on the network in a row before the sign-in gives up.
const MAX_NET_FAILURES: u32 = 3;

fn form(
    http: &dyn AuthHttp,
    url: String,
    fields: Vec<(&str, String)>,
) -> Result<crate::http::AuthResponse, AuthError> {
    let body = Body::Form(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
    Ok(http.send(&AuthRequest {
        method: Method::Post,
        url,
        headers: vec![("Accept".into(), "application/json".into())],
        body,
    })?)
}

/// The OAuth `error` code of an error answer, if the body has one.
fn error_code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|e| e.error)
}

pub(crate) fn request_device_code(
    http: &dyn AuthHttp,
    config: &MsaConfig,
) -> Result<DeviceCode, AuthError> {
    let response = form(
        http,
        config.endpoint("devicecode"),
        vec![
            ("client_id", config.client_id.clone()),
            ("scope", config.scope.clone()),
        ],
    )?;
    if response.status != 200 {
        let code = error_code(&response.body).unwrap_or_default();
        return Err(AuthError::Protocol(format!(
            "device code request: status {} {code}",
            response.status
        )));
    }
    let b: DeviceCodeBody = serde_json::from_slice(&response.body)
        .map_err(|_| AuthError::Protocol("device code response is not valid".into()))?;
    Ok(DeviceCode {
        user_code: b.user_code,
        verification_uri: b.verification_uri,
        expires_in: b.expires_in,
        interval: b.interval.unwrap_or(5).max(1),
        device_code: Secret::new(b.device_code),
    })
}

/// Polls the token endpoint until the user finishes, declines or the code expires.
pub(crate) fn poll_device_code(
    http: &dyn AuthHttp,
    clock: &dyn Clock,
    config: &MsaConfig,
    code: &DeviceCode,
    cancel: &CancelToken,
) -> Result<MsaTokens, AuthError> {
    let deadline = clock.now_unix().saturating_add(code.expires_in);
    let mut interval = code.interval;
    let mut net_failures = 0;
    loop {
        clock.sleep(Duration::from_secs(interval), cancel)?;
        if clock.now_unix() >= deadline {
            return Err(AuthError::Expired);
        }
        let response = match form(
            http,
            config.endpoint("token"),
            vec![
                ("grant_type", DEVICE_GRANT.to_owned()),
                ("client_id", config.client_id.clone()),
                ("device_code", code.device_code.expose().to_owned()),
            ],
        ) {
            Ok(r) => r,
            Err(AuthError::Net(e)) if e.is_retryable() && net_failures < MAX_NET_FAILURES => {
                net_failures += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        net_failures = 0;
        if response.status == 200 {
            return parse_tokens(&response.body);
        }
        match error_code(&response.body).as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("authorization_declined") => return Err(AuthError::Declined),
            Some("expired_token") => return Err(AuthError::Expired),
            Some(other) => {
                return Err(AuthError::Protocol(format!("device code polling: {other}")));
            }
            None => {
                return Err(AuthError::Protocol(format!(
                    "device code polling: status {}",
                    response.status
                )));
            }
        }
    }
}

/// Exchanges a refresh token for new tokens. A rejected token is [`AuthError::SignInRequired`].
pub(crate) fn refresh(
    http: &dyn AuthHttp,
    config: &MsaConfig,
    refresh_token: &Secret,
) -> Result<MsaTokens, AuthError> {
    let response = form(
        http,
        config.endpoint("token"),
        vec![
            ("grant_type", "refresh_token".to_owned()),
            ("client_id", config.client_id.clone()),
            ("refresh_token", refresh_token.expose().to_owned()),
            ("scope", config.scope.clone()),
        ],
    )?;
    if response.status == 200 {
        return parse_tokens(&response.body);
    }
    match error_code(&response.body).as_deref() {
        Some("invalid_grant" | "interaction_required" | "invalid_client") => {
            Err(AuthError::SignInRequired)
        }
        Some(other) => Err(AuthError::Protocol(format!("token refresh: {other}"))),
        None => Err(AuthError::Protocol(format!(
            "token refresh: status {}",
            response.status
        ))),
    }
}

/// Exchanges an authorization code (with its PKCE verifier) for tokens.
pub(crate) fn exchange_code(
    http: &dyn AuthHttp,
    config: &MsaConfig,
    code: &str,
    verifier: &Secret,
    redirect_uri: &str,
) -> Result<MsaTokens, AuthError> {
    let response = form(
        http,
        config.endpoint("token"),
        vec![
            ("grant_type", "authorization_code".to_owned()),
            ("client_id", config.client_id.clone()),
            ("code", code.to_owned()),
            ("code_verifier", verifier.expose().to_owned()),
            ("redirect_uri", redirect_uri.to_owned()),
            ("scope", config.scope.clone()),
        ],
    )?;
    if response.status == 200 {
        return parse_tokens(&response.body);
    }
    let code = error_code(&response.body).unwrap_or_else(|| format!("status {}", response.status));
    Err(AuthError::Protocol(format!("code exchange: {code}")))
}

fn parse_tokens(body: &[u8]) -> Result<MsaTokens, AuthError> {
    let t: TokenBody = serde_json::from_slice(body)
        .map_err(|_| AuthError::Protocol("token response is not valid".into()))?;
    Ok(MsaTokens {
        access_token: Secret::new(t.access_token),
        refresh_token: t.refresh_token.map(Secret::new),
    })
}
