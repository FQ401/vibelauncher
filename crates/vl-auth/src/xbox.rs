//! Xbox Live: user token and XSTS token for the Minecraft services.

use serde::Deserialize;
use serde_json::json;

use crate::error::{AuthError, XboxProblem};
use crate::http::{AuthHttp, AuthRequest, Body, Method};
use crate::secret::Secret;

const USER_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
/// The relying party that makes an XSTS token valid for the Minecraft services.
const MINECRAFT_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

/// An XSTS token with the user hash it belongs to.
pub(crate) struct Xsts {
    pub token: Secret,
    pub user_hash: String,
}

#[derive(Deserialize)]
struct XboxBody {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    claims: Claims,
}

#[derive(Deserialize)]
struct Claims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Deserialize)]
struct XerrBody {
    #[serde(rename = "XErr")]
    code: u64,
}

fn post_json(
    http: &dyn AuthHttp,
    url: &str,
    body: &serde_json::Value,
) -> Result<crate::http::AuthResponse, AuthError> {
    Ok(http.send(&AuthRequest {
        method: Method::Post,
        url: url.to_owned(),
        headers: vec![("Accept".into(), "application/json".into())],
        body: Body::Json(body.to_string()),
    })?)
}

fn parse(body: &[u8]) -> Result<(Secret, String), AuthError> {
    let b: XboxBody = serde_json::from_slice(body)
        .map_err(|_| AuthError::Protocol("Xbox response is not valid".into()))?;
    let uhs = b
        .claims
        .xui
        .into_iter()
        .next()
        .map(|x| x.uhs)
        .ok_or_else(|| AuthError::Protocol("Xbox response has no user hash".into()))?;
    Ok((Secret::new(b.token), uhs))
}

/// Exchanges the Microsoft access token for an Xbox Live user token.
pub(crate) fn user_token(http: &dyn AuthHttp, msa_access: &Secret) -> Result<Secret, AuthError> {
    let response = post_json(
        http,
        USER_AUTH_URL,
        &json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={}", msa_access.expose()),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }),
    )?;
    if response.status != 200 {
        return Err(AuthError::Protocol(format!(
            "Xbox Live user token: status {}",
            response.status
        )));
    }
    Ok(parse(&response.body)?.0)
}

/// Exchanges the user token for an XSTS token; a 401 carries the `XErr` reason.
pub(crate) fn xsts_token(http: &dyn AuthHttp, user_token: &Secret) -> Result<Xsts, AuthError> {
    let response = post_json(
        http,
        XSTS_URL,
        &json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [user_token.expose()],
            },
            "RelyingParty": MINECRAFT_RELYING_PARTY,
            "TokenType": "JWT",
        }),
    )?;
    match response.status {
        200 => {
            let (token, user_hash) = parse(&response.body)?;
            Ok(Xsts { token, user_hash })
        }
        401 => match serde_json::from_slice::<XerrBody>(&response.body) {
            Ok(e) => Err(AuthError::Xbox(XboxProblem::from_code(e.code))),
            Err(_) => Err(AuthError::Protocol("XSTS: status 401".into())),
        },
        status => Err(AuthError::Protocol(format!("XSTS: status {status}"))),
    }
}
