//! The user's API keys of the catalog platforms: set, remove, check.
//!
//! Only CurseForge takes a key. The key is stored in the OS keyring (entry
//! `api-key:curseforge`); the `VL_CURSEFORGE_API_KEY` environment variable, when set, is used
//! instead, and the status says so. The key never appears in a response, an event, a log line or
//! an error: it is carried as a [`Secret`] and wrapped in an [`ApiKey`], both with masked
//! `Debug` output.

use vl_core::catalog::{ApiKeyOrigin, ApiKeyStatus, Provider};
use vl_core::secret::Secret;
use vl_core::{Error, Response, Result};
use vl_net::Control;
use vl_platforms::Platforms;
use vl_platforms::curseforge::ApiKey;

fn curseforge_only(provider: Provider) -> Result<()> {
    if provider == Provider::CurseForge {
        Ok(())
    } else {
        Err(Error::invalid_input(format!(
            "{provider:?} takes no API key; only CurseForge does"
        )))
    }
}

/// Stores the key in the keyring after checking its shape.
pub(crate) fn set(platforms: &Platforms, provider: Provider, key: &Secret) -> Result<Response> {
    curseforge_only(provider)?;
    let key = ApiKey::new(key.expose())?;
    platforms.curseforge_keys()?.set(&key)?;
    Ok(Response::Done)
}

/// Removes the key from the keyring.
pub(crate) fn remove(platforms: &Platforms, provider: Provider) -> Result<Response> {
    curseforge_only(provider)?;
    platforms.curseforge_keys()?.remove()?;
    Ok(Response::Done)
}

/// Where the key comes from and what one request with it says.
pub(crate) fn check(
    platforms: &Platforms,
    provider: Provider,
    control: &Control,
) -> Result<Response> {
    curseforge_only(provider)?;
    let places = platforms.curseforge_keys()?.places()?;
    let state = platforms.curseforge().check_key(control)?;
    let source = if places.environment {
        Some(ApiKeyOrigin::Environment)
    } else if places.keyring {
        Some(ApiKeyOrigin::Keyring)
    } else {
        None
    };
    Ok(Response::ApiKey(ApiKeyStatus {
        provider,
        state,
        source,
        in_environment: places.environment,
        in_keyring: places.keyring,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use vl_auth::MemoryVault;
    use vl_core::ErrorKind;
    use vl_core::catalog::{ApiKeyState, CatalogCommand};
    use vl_net::{Client, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError};
    use vl_platforms::curseforge::{CurseForgeKeys, EnvApiKey, KEYRING_ENTRY, KeyringApiKey};
    use vl_platforms::{CurseForgeOptions, CurseForgeSource, ModrinthSource};

    use super::*;

    /// A synthetic key; real keys never appear in tests.
    const KEY: &str = "synthetic-test-key-0000";

    /// Answers with the scripted statuses and remembers every request.
    #[derive(Default)]
    struct Fake {
        statuses: Mutex<VecDeque<u16>>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl HttpTransport for Fake {
        fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
            self.seen.lock().unwrap().push(request.clone());
            let status = self.statuses.lock().unwrap().pop_front().unwrap_or(500);
            Ok(HttpResponse {
                status,
                headers: Vec::new(),
                body: Box::new(Cursor::new(br#"{"data":{"id":432}}"#.to_vec())),
            })
        }
    }

    struct Rig {
        platforms: Platforms,
        vault: Arc<MemoryVault>,
        fake: Arc<Fake>,
    }

    /// Platforms over the fake server with an in-memory keyring and a variable that is never
    /// set, so neither the machine's keyring nor its environment is touched.
    fn rig() -> Rig {
        let fake = Arc::new(Fake::default());
        let mut config = NetConfig::new("0.0.0");
        config.retry.attempts = 1;
        let client = Client::with_transport(config, fake.clone());
        let vault = Arc::new(MemoryVault::new());
        let keys = CurseForgeKeys::new(
            EnvApiKey::named("VL_TEST_CURSEFORGE_KEY_THAT_IS_NEVER_SET"),
            Arc::new(KeyringApiKey::new(vault.clone())),
        );
        let options = CurseForgeOptions {
            endpoints: vl_curseforge::Endpoints::new("https://curseforge.test"),
            ..CurseForgeOptions::default()
        };
        let platforms = Platforms::with_keys(
            ModrinthSource::new(client.clone(), Default::default()),
            CurseForgeSource::new(client, options, keys.source()),
            keys,
        );
        Rig {
            platforms,
            vault,
            fake,
        }
    }

    fn status(rig: &Rig) -> ApiKeyStatus {
        match check(&rig.platforms, Provider::CurseForge, &Control::default()).unwrap() {
            Response::ApiKey(status) => status,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_key_is_set_checked_and_removed() {
        let rig = rig();
        let none = status(&rig);
        assert_eq!(none.state, ApiKeyState::Missing);
        assert_eq!(none.source, None);
        assert!(rig.fake.seen.lock().unwrap().is_empty(), "nothing is sent");

        let secret = Secret::new(format!("  {KEY}\n"));
        set(&rig.platforms, Provider::CurseForge, &secret).unwrap();
        assert_eq!(rig.vault.raw(KEYRING_ENTRY).as_deref(), Some(KEY));

        rig.fake.statuses.lock().unwrap().extend([200, 401]);
        let ok = status(&rig);
        assert_eq!(ok.state, ApiKeyState::Valid);
        assert_eq!(ok.source, Some(ApiKeyOrigin::Keyring));
        assert!(ok.in_keyring && !ok.in_environment);
        assert!(!ok.environment_overrides_keyring());
        assert_eq!(status(&rig).state, ApiKeyState::Rejected);
        let unreachable = status(&rig);
        assert!(
            matches!(&unreachable.state, ApiKeyState::Unreachable { detail } if !detail.contains(KEY)),
            "{unreachable:?}"
        );
        let seen = rig.fake.seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .all(|r| r.url == "https://curseforge.test/v1/games/432")
        );
        // Nothing the facade answers holds the key.
        assert!(!format!("{ok:?}{unreachable:?}").contains(KEY));

        remove(&rig.platforms, Provider::CurseForge).unwrap();
        assert_eq!(rig.vault.raw(KEYRING_ENTRY), None);
        assert_eq!(status(&rig).state, ApiKeyState::Missing);
    }

    #[test]
    fn bad_keys_and_other_platforms_are_refused_without_the_key_in_the_error() {
        let rig = rig();
        let err = set(
            &rig.platforms,
            Provider::CurseForge,
            &Secret::new("two words-synthetic"),
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(!err.message.contains("synthetic"), "{}", err.message);
        assert_eq!(rig.vault.raw(KEYRING_ENTRY), None);

        let err = set(&rig.platforms, Provider::Modrinth, &Secret::new(KEY)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(!err.message.contains(KEY));
        assert!(remove(&rig.platforms, Provider::Modrinth).is_err());

        // A command the backend refuses is described with the key masked.
        let command = CatalogCommand::SetApiKey {
            provider: Provider::CurseForge,
            key: Secret::new(KEY),
        };
        assert!(!format!("{command:?}").contains(KEY));
    }
}
