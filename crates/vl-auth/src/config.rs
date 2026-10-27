//! Build-time constants and run-time overrides of the Microsoft application registration.

/// The application (client) ID of the official build. It is public (a public client without a
/// secret). Forks use their own registration through [`MsaConfig::from_env`].
pub const DEFAULT_CLIENT_ID: &str = "5ac99224-9cbb-426e-9faa-8cd19c94d130";

/// The environment variable that overrides the client ID (forks and development).
pub const CLIENT_ID_ENV: &str = "VL_MS_CLIENT_ID";

/// The tenant for personal Microsoft accounts; `common` and organization tenants do not work for
/// Minecraft.
const TENANT: &str = "consumers";

const SCOPE: &str = "XboxLive.signin offline_access";

/// Which application registration signs users in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsaConfig {
    /// The application (client) ID.
    pub client_id: String,
    /// The OAuth scope.
    pub scope: String,
    /// The tenant segment of the endpoint URLs.
    pub tenant: String,
}

impl MsaConfig {
    /// The official registration.
    pub fn official() -> Self {
        Self::with_client_id(DEFAULT_CLIENT_ID)
    }

    /// The registration with `client_id` and the standard scope and tenant.
    pub fn with_client_id(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            scope: SCOPE.into(),
            tenant: TENANT.into(),
        }
    }

    /// The official registration, or the one named by `VL_MS_CLIENT_ID` when it is set.
    pub fn from_env() -> Self {
        Self::from_override(std::env::var(CLIENT_ID_ENV).ok().as_deref())
    }

    /// [`from_env`](Self::from_env) with the variable's value passed in.
    pub fn from_override(client_id: Option<&str>) -> Self {
        match client_id.map(str::trim).filter(|v| !v.is_empty()) {
            Some(id) => Self::with_client_id(id),
            None => Self::official(),
        }
    }

    pub(crate) fn endpoint(&self, path: &str) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/{path}",
            self.tenant
        )
    }
}

impl Default for MsaConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_replaces_the_client_id_only() {
        let c = MsaConfig::from_override(Some(" my-id "));
        assert_eq!(c.client_id, "my-id");
        assert_eq!(c.tenant, "consumers");
        assert_eq!(c.scope, "XboxLive.signin offline_access");
        assert_eq!(
            MsaConfig::from_override(Some("  ")).client_id,
            DEFAULT_CLIENT_ID
        );
        assert_eq!(MsaConfig::from_override(None), MsaConfig::official());
    }

    #[test]
    fn endpoints_use_the_consumers_tenant() {
        assert_eq!(
            MsaConfig::official().endpoint("token"),
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token"
        );
    }
}
