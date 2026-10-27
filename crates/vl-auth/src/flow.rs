//! The whole sign-in chain: Microsoft → Xbox Live → XSTS → Minecraft services → profile.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vl_net::CancelToken;

use crate::config::MsaConfig;
use crate::error::AuthError;
use crate::http::AuthHttp;
use crate::minecraft;
use crate::msa::{self, DeviceCode, MsaTokens};
use crate::secret::{Secret, StoredTokens};
use crate::xbox;

/// Time for the flow, replaceable in tests.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now_unix(&self) -> u64;
    /// Waits for `duration`; returns [`AuthError::Cancelled`] early when `cancel` is set.
    fn sleep(&self, duration: Duration, cancel: &CancelToken) -> Result<(), AuthError>;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn sleep(&self, duration: Duration, cancel: &CancelToken) -> Result<(), AuthError> {
        let slice = Duration::from_millis(50);
        let mut left = duration;
        while !left.is_zero() {
            if cancel.is_cancelled() {
                return Err(AuthError::Cancelled);
            }
            let step = left.min(slice);
            std::thread::sleep(step);
            left -= step;
        }
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        Ok(())
    }
}

/// The Minecraft profile of a signed-in account (public data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The profile UUID (hyphenated).
    pub uuid: String,
    /// The player name.
    pub name: String,
    /// The active skin texture URL, if any.
    pub skin_url: Option<String>,
    /// The skin model (`CLASSIC` or `SLIM`), if known.
    pub skin_variant: Option<String>,
}

/// A completed sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The profile.
    pub profile: Profile,
    /// The Minecraft services access token the game is launched with.
    pub minecraft_token: Secret,
    /// When it expires (Unix seconds).
    pub minecraft_expires_at: u64,
    /// The Microsoft refresh token to keep.
    pub refresh_token: Secret,
}

impl Session {
    /// The secrets of this session, for a [`TokenStore`](crate::TokenStore).
    pub fn stored_tokens(&self) -> StoredTokens {
        StoredTokens {
            refresh_token: self.refresh_token.clone(),
            minecraft_token: self.minecraft_token.clone(),
            minecraft_expires_at: self.minecraft_expires_at,
        }
    }
}

/// Runs the Microsoft sign-in chain on an [`AuthHttp`].
pub struct Authenticator {
    pub(crate) http: Arc<dyn AuthHttp>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) config: MsaConfig,
}

impl Authenticator {
    /// An authenticator for `config` on the system clock.
    pub fn new(http: Arc<dyn AuthHttp>, config: MsaConfig) -> Self {
        Self::with_clock(http, Arc::new(SystemClock), config)
    }

    /// An authenticator with a given clock (for tests).
    pub fn with_clock(http: Arc<dyn AuthHttp>, clock: Arc<dyn Clock>, config: MsaConfig) -> Self {
        Self {
            http,
            clock,
            config,
        }
    }

    /// The registration in use.
    pub fn config(&self) -> &MsaConfig {
        &self.config
    }

    /// Device-code sign-in: asks for a code, hands it to `on_prompt` (show the code and the page
    /// to the user), waits until the user finishes, then runs the rest of the chain.
    pub fn device_code_sign_in(
        &self,
        on_prompt: &mut dyn FnMut(&DeviceCode),
        cancel: &CancelToken,
    ) -> Result<Session, AuthError> {
        let code = msa::request_device_code(self.http.as_ref(), &self.config)?;
        on_prompt(&code);
        let tokens = msa::poll_device_code(
            self.http.as_ref(),
            self.clock.as_ref(),
            &self.config,
            &code,
            cancel,
        )?;
        self.session_from_tokens(tokens, None)
    }

    /// Renews a session from a stored refresh token. [`AuthError::SignInRequired`] means the
    /// user must sign in again.
    pub fn refresh(&self, refresh_token: &Secret) -> Result<Session, AuthError> {
        let tokens = msa::refresh(self.http.as_ref(), &self.config, refresh_token)?;
        self.session_from_tokens(tokens, Some(refresh_token))
    }

    /// Xbox Live → XSTS → Minecraft login → entitlements → profile, from Microsoft tokens.
    /// `previous_refresh` is kept when the answer carries no new refresh token.
    pub(crate) fn session_from_tokens(
        &self,
        tokens: MsaTokens,
        previous_refresh: Option<&Secret>,
    ) -> Result<Session, AuthError> {
        let http = self.http.as_ref();
        let refresh_token = tokens
            .refresh_token
            .or_else(|| previous_refresh.cloned())
            .ok_or_else(|| AuthError::Protocol("no refresh token was issued".into()))?;
        let user = xbox::user_token(http, &tokens.access_token)?;
        let xsts = xbox::xsts_token(http, &user)?;
        let mc = minecraft::login(http, &xsts)?;
        if !minecraft::owns_game(http, &mc.token)? {
            return Err(AuthError::NoGame);
        }
        let profile = minecraft::profile(http, &mc.token)?;
        Ok(Session {
            profile,
            minecraft_expires_at: self.clock.now_unix().saturating_add(mc.expires_in),
            minecraft_token: mc.token,
            refresh_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use vl_core::{Error, ErrorKind};

    use super::*;
    use crate::error::XboxProblem;
    use crate::testing::{FakeClock, Scripted, json, status};

    const MS_TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
    const MS_DEVICE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";

    fn setup(http: &Arc<Scripted>) -> (Authenticator, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::new(1_000));
        let auth = Authenticator::with_clock(
            http.clone(),
            clock.clone(),
            MsaConfig::with_client_id("test-client"),
        );
        (auth, clock)
    }

    fn device_response() -> crate::http::AuthResponse {
        json(
            200,
            r#"{"device_code":"DEVCODE-SECRET","user_code":"ABCD1234","verification_uri":"https://www.microsoft.com/link","expires_in":900,"interval":5}"#,
        )
    }

    fn tokens_response() -> crate::http::AuthResponse {
        json(
            200,
            r#"{"access_token":"MSA-ACCESS-SECRET","refresh_token":"MSA-REFRESH-SECRET","expires_in":3600}"#,
        )
    }

    fn script_chain(http: &Scripted) {
        http.route(
            "user.auth.xboxlive.com",
            [json(
                200,
                r#"{"Token":"XBL-SECRET","DisplayClaims":{"xui":[{"uhs":"hash123"}]}}"#,
            )],
        );
        http.route(
            "xsts.auth.xboxlive.com",
            [json(
                200,
                r#"{"Token":"XSTS-SECRET","DisplayClaims":{"xui":[{"uhs":"hash123"}]}}"#,
            )],
        );
        http.route(
            "login_with_xbox",
            [json(
                200,
                r#"{"access_token":"MC-SECRET","expires_in":86400}"#,
            )],
        );
        http.route(
            "entitlements/mcstore",
            [json(200, r#"{"items":[{"name":"game_minecraft"}]}"#)],
        );
        http.route(
            "minecraft/profile",
            [json(
                200,
                r#"{"id":"0123456789abcdef0123456789abcdef","name":"TestPlayer","skins":[{"state":"ACTIVE","url":"https://textures.example/skin","variant":"SLIM"}]}"#,
            )],
        );
    }

    fn sign_in(http: &Arc<Scripted>) -> Result<(Session, Vec<DeviceCode>), AuthError> {
        let (auth, _) = setup(http);
        let mut prompts = Vec::new();
        let session =
            auth.device_code_sign_in(&mut |c| prompts.push(c.clone()), &CancelToken::new())?;
        Ok((session, prompts))
    }

    #[test]
    fn device_code_chain_succeeds() {
        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(
            MS_TOKEN,
            [
                json(400, r#"{"error":"authorization_pending"}"#),
                json(400, r#"{"error":"slow_down"}"#),
                json(400, r#"{"error":"authorization_pending"}"#),
                tokens_response(),
            ],
        );
        script_chain(&http);
        let (auth, clock) = setup(&http);
        let mut prompts = Vec::new();
        let session = auth
            .device_code_sign_in(&mut |c| prompts.push(c.clone()), &CancelToken::new())
            .unwrap();

        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].user_code, "ABCD1234");
        assert_eq!(
            prompts[0].verification_uri,
            "https://www.microsoft.com/link"
        );
        assert_eq!(session.profile.name, "TestPlayer");
        assert_eq!(session.profile.uuid, "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(session.profile.skin_variant.as_deref(), Some("SLIM"));
        assert_eq!(session.minecraft_token.expose(), "MC-SECRET");
        assert_eq!(session.refresh_token.expose(), "MSA-REFRESH-SECRET");
        assert_eq!(session.minecraft_expires_at, 1_030 + 86_400);
        // The interval grows by five seconds after `slow_down`.
        assert_eq!(clock.sleeps(), [5, 5, 10, 10]);

        let urls = http.urls();
        assert_eq!(urls.len(), 10);
        assert!(urls[5].contains("user.auth.xboxlive.com"));
        assert!(urls[6].contains("xsts.auth.xboxlive.com"));
        assert!(urls[7].contains("login_with_xbox"));
        let bodies = http.bodies();
        assert!(bodies[0].contains("client_id=test-client"));
        assert!(bodies[0].contains("scope=XboxLive.signin%20offline_access"));
        assert!(bodies[1].contains("device_code=DEVCODE-SECRET"));
        assert!(bodies[5].contains("d=MSA-ACCESS-SECRET"));
        assert!(bodies[6].contains("rp://api.minecraftservices.com/"));
        assert!(bodies[7].contains("XBL3.0 x=hash123;XSTS-SECRET"));
    }

    #[test]
    fn secrets_stay_out_of_debug_and_errors() {
        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(MS_TOKEN, [tokens_response()]);
        script_chain(&http);
        let (session, prompts) = sign_in(&http).unwrap();
        let shown = format!("{session:?} {prompts:?} {:?}", session.stored_tokens());
        for secret in [
            "DEVCODE-SECRET",
            "MC-SECRET",
            "MSA-REFRESH-SECRET",
            "MSA-ACCESS",
        ] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
    }

    #[test]
    fn declined_expired_and_cancelled_polling() {
        for (body, expected) in [
            (r#"{"error":"authorization_declined"}"#, AuthError::Declined),
            (r#"{"error":"expired_token"}"#, AuthError::Expired),
        ] {
            let http = Arc::new(Scripted::default());
            http.route(MS_DEVICE, [device_response()]);
            http.route(MS_TOKEN, [json(400, body)]);
            assert_eq!(sign_in(&http).unwrap_err(), expected);
        }

        // The code runs out while the user is still deciding.
        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(
            MS_TOKEN,
            [json(400, r#"{"error":"authorization_pending"}"#)],
        );
        assert_eq!(sign_in(&http).unwrap_err(), AuthError::Expired);

        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        let (auth, _) = setup(&http);
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = auth.device_code_sign_in(&mut |_| {}, &cancel).unwrap_err();
        assert_eq!(err, AuthError::Cancelled);
    }

    #[test]
    fn xsts_errors_become_specific_problems() {
        for (xerr, problem) in [
            (2_148_916_233_u64, XboxProblem::NoXboxAccount),
            (2_148_916_235, XboxProblem::RegionUnavailable),
            (2_148_916_236, XboxProblem::AdultVerificationRequired),
            (2_148_916_238, XboxProblem::ChildAccount),
            (2_148_916_999, XboxProblem::Other(2_148_916_999)),
        ] {
            let http = Arc::new(Scripted::default());
            http.route(MS_DEVICE, [device_response()]);
            http.route(MS_TOKEN, [tokens_response()]);
            script_chain(&http);
            http.route(
                "xsts.auth.xboxlive.com",
                [json(
                    401,
                    &format!(r#"{{"Identity":"0","XErr":{xerr},"Message":""}}"#),
                )],
            );
            let err = sign_in(&http).unwrap_err();
            assert_eq!(err, AuthError::Xbox(problem));
            assert_eq!(Error::from(err).kind, ErrorKind::Auth);
        }
    }

    #[test]
    fn forbidden_minecraft_login_means_app_not_permitted() {
        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(MS_TOKEN, [tokens_response()]);
        script_chain(&http);
        http.route("login_with_xbox", [status(403)]);
        let err = sign_in(&http).unwrap_err();
        assert_eq!(err, AuthError::AppNotPermitted);
        let facade = Error::from(err);
        assert_eq!(facade.kind, ErrorKind::Auth);
        assert!(facade.message.contains("app not permitted yet"));
        // Nothing after the refused login was requested.
        assert!(http.urls().iter().all(|u| !u.contains("entitlements")));
    }

    #[test]
    fn no_game_and_no_profile_are_reported() {
        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(MS_TOKEN, [tokens_response()]);
        script_chain(&http);
        http.route("entitlements/mcstore", [json(200, r#"{"items":[]}"#)]);
        assert_eq!(sign_in(&http).unwrap_err(), AuthError::NoGame);

        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(MS_TOKEN, [tokens_response()]);
        script_chain(&http);
        http.route(
            "minecraft/profile",
            [json(
                404,
                r#"{"path":"/minecraft/profile","errorType":"NOT_FOUND"}"#,
            )],
        );
        assert_eq!(sign_in(&http).unwrap_err(), AuthError::NoProfile);
    }

    #[test]
    fn network_and_garbage_answers_do_not_panic() {
        let http = Arc::new(Scripted::default());
        let err = sign_in(&http).unwrap_err();
        assert!(matches!(err, AuthError::Net(_)));
        assert_eq!(Error::from(err).kind, ErrorKind::Network);

        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [json(200, "not json")]);
        assert!(matches!(
            sign_in(&http).unwrap_err(),
            AuthError::Protocol(_)
        ));

        let http = Arc::new(Scripted::default());
        http.route(MS_DEVICE, [device_response()]);
        http.route(MS_TOKEN, [tokens_response()]);
        script_chain(&http);
        http.route(
            "user.auth.xboxlive.com",
            [json(200, r#"{"Token":"x","DisplayClaims":{"xui":[]}}"#)],
        );
        assert!(matches!(
            sign_in(&http).unwrap_err(),
            AuthError::Protocol(_)
        ));
    }

    #[test]
    fn refresh_renews_the_session() {
        let http = Arc::new(Scripted::default());
        http.route(
            MS_TOKEN,
            [json(
                200,
                r#"{"access_token":"NEW-ACCESS","expires_in":3600}"#,
            )],
        );
        script_chain(&http);
        let (auth, _) = setup(&http);
        let old = Secret::new("OLD-REFRESH");
        let session = auth.refresh(&old).unwrap();
        // No new refresh token was issued: the old one stays.
        assert_eq!(session.refresh_token, old);
        let body = &http.bodies()[0];
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=OLD-REFRESH"));
    }

    #[test]
    fn rejected_refresh_token_requires_a_new_sign_in() {
        let http = Arc::new(Scripted::default());
        http.route(
            MS_TOKEN,
            [json(
                400,
                r#"{"error":"invalid_grant","error_description":"expired"}"#,
            )],
        );
        let (auth, _) = setup(&http);
        let err = auth.refresh(&Secret::new("OLD")).unwrap_err();
        assert_eq!(err, AuthError::SignInRequired);
        assert!(!err.to_string().contains("OLD"));
    }
}
