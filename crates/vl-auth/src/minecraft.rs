//! Minecraft services: token, entitlements and profile.

use serde::Deserialize;
use serde_json::json;

use crate::error::AuthError;
use crate::flow::Profile;
use crate::http::{AuthHttp, AuthRequest, AuthResponse, Body, Method};
use crate::secret::Secret;
use crate::xbox::Xsts;

const LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const ENTITLEMENTS_URL: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/// Seconds a Minecraft token lives when the answer omits `expires_in`.
const DEFAULT_LIFETIME: u64 = 86_400;

/// A Minecraft services token and its lifetime in seconds.
pub(crate) struct McToken {
    pub token: Secret,
    pub expires_in: u64,
}

#[derive(Deserialize)]
struct LoginBody {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct EntitlementsBody {
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ProfileBody {
    id: String,
    name: String,
    #[serde(default)]
    skins: Vec<SkinBody>,
}

#[derive(Deserialize)]
struct SkinBody {
    #[serde(default)]
    state: String,
    url: String,
    #[serde(default)]
    variant: Option<String>,
}

fn bearer(token: &Secret) -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {}", token.expose())),
    ]
}

fn get(http: &dyn AuthHttp, url: &str, token: &Secret) -> Result<AuthResponse, AuthError> {
    Ok(http.send(&AuthRequest {
        method: Method::Get,
        url: url.to_owned(),
        headers: bearer(token),
        body: Body::None,
    })?)
}

/// Signs in to the Minecraft services with an XSTS token. A 403 means Mojang has not approved
/// this application ID.
pub(crate) fn login(http: &dyn AuthHttp, xsts: &Xsts) -> Result<McToken, AuthError> {
    let identity = format!("XBL3.0 x={};{}", xsts.user_hash, xsts.token.expose());
    let response = http.send(&AuthRequest {
        method: Method::Post,
        url: LOGIN_URL.to_owned(),
        headers: vec![("Accept".into(), "application/json".into())],
        body: Body::Json(json!({ "identityToken": identity }).to_string()),
    })?;
    match response.status {
        200 => {
            let b: LoginBody = serde_json::from_slice(&response.body)
                .map_err(|_| AuthError::Protocol("Minecraft login response is not valid".into()))?;
            Ok(McToken {
                token: Secret::new(b.access_token),
                expires_in: b.expires_in.unwrap_or(DEFAULT_LIFETIME),
            })
        }
        403 => Err(AuthError::AppNotPermitted),
        401 => Err(AuthError::Unauthorized),
        status => Err(AuthError::Protocol(format!(
            "Minecraft login: status {status}"
        ))),
    }
}

/// Whether the account owns the game. An empty entitlement list means it does not.
pub(crate) fn owns_game(http: &dyn AuthHttp, token: &Secret) -> Result<bool, AuthError> {
    let response = get(http, ENTITLEMENTS_URL, token)?;
    match response.status {
        200 => {
            let b: EntitlementsBody = serde_json::from_slice(&response.body)
                .map_err(|_| AuthError::Protocol("entitlements response is not valid".into()))?;
            Ok(!b.items.is_empty())
        }
        401 => Err(AuthError::Unauthorized),
        403 => Err(AuthError::AppNotPermitted),
        status => Err(AuthError::Protocol(format!(
            "entitlements: status {status}"
        ))),
    }
}

/// The Minecraft profile; a 404 means the account has none.
pub(crate) fn profile(http: &dyn AuthHttp, token: &Secret) -> Result<Profile, AuthError> {
    let response = get(http, PROFILE_URL, token)?;
    match response.status {
        200 => {
            let b: ProfileBody = serde_json::from_slice(&response.body)
                .map_err(|_| AuthError::Protocol("profile response is not valid".into()))?;
            let uuid = hyphenate(&b.id)
                .ok_or_else(|| AuthError::Protocol("profile id is not a UUID".into()))?;
            let skin = b
                .skins
                .into_iter()
                .find(|s| s.state.eq_ignore_ascii_case("ACTIVE"));
            Ok(Profile {
                uuid,
                name: b.name,
                skin_url: skin.as_ref().map(|s| s.url.clone()),
                skin_variant: skin.and_then(|s| s.variant),
            })
        }
        404 => Err(AuthError::NoProfile),
        401 => Err(AuthError::Unauthorized),
        403 => Err(AuthError::AppNotPermitted),
        status => Err(AuthError::Protocol(format!("profile: status {status}"))),
    }
}

/// `0123…` (32 hex digits) → `01234567-89ab-cdef-0123-456789abcdef`.
fn hyphenate(id: &str) -> Option<String> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let lower = id.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        lower.get(0..8)?,
        lower.get(8..12)?,
        lower.get(12..16)?,
        lower.get(16..20)?,
        lower.get(20..32)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphenates_profile_ids() {
        assert_eq!(
            hyphenate("0123456789ABCDEF0123456789abcdef").as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert_eq!(hyphenate("short"), None);
        assert_eq!(hyphenate(&"g".repeat(32)), None);
    }
}
