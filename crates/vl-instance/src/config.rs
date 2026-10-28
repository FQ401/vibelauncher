//! The per-instance config file (`instance.json`) and its components.

use serde::{Deserialize, Serialize};
use vl_core::component::{ComponentKind, ComponentSet};
use vl_core::instance::{Loader, LoaderRequest};
use vl_core::{Error, Result};

/// The config format this launcher writes and reads.
pub const CONFIG_FORMAT: u32 = 1;

/// Component uid of the game itself.
pub const GAME_UID: &str = "net.minecraft";
/// Component uid of Fabric Loader.
pub const FABRIC_UID: &str = "net.fabricmc.fabric-loader";
/// Component uid of Quilt Loader.
pub const QUILT_UID: &str = "org.quiltmc.quilt-loader";
/// Component uid of NeoForge.
pub const NEOFORGE_UID: &str = "net.neoforged";
/// Component uid of Forge.
pub const FORGE_UID: &str = "net.minecraftforge";

/// One part of an instance: the game, a loader or a patch, identified by `uid`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// What the component is (reverse-DNS style, e.g. `net.minecraft`).
    pub uid: String,
    /// Its version.
    pub version: String,
}

/// The contents of `instance.json`. Unknown fields written by newer launchers are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// The config format version.
    pub format: u32,
    /// The display name.
    pub name: String,
    /// The icon key, if one was chosen.
    #[serde(default)]
    pub icon: Option<String>,
    /// The group the instance is shown in.
    #[serde(default)]
    pub group: Option<String>,
    /// Free-form notes.
    #[serde(default)]
    pub notes: String,
    /// Creation time, Unix seconds.
    #[serde(default)]
    pub created_unix: i64,
    /// Last time the instance was active (launched or opened by the user), Unix seconds.
    #[serde(default)]
    pub last_active_unix: Option<i64>,
    /// Last time the game was started, Unix seconds.
    #[serde(default)]
    pub last_played_unix: Option<i64>,
    /// Total play time in seconds.
    #[serde(default)]
    pub play_time_secs: u64,
    /// Game, loaders and patches.
    #[serde(default)]
    pub components: Vec<Component>,
}

impl InstanceConfig {
    /// A vanilla instance of the given game version.
    pub fn vanilla(name: &str, game_version: &str, group: Option<String>, now: i64) -> Self {
        Self {
            format: CONFIG_FORMAT,
            name: name.to_owned(),
            icon: None,
            group,
            notes: String::new(),
            created_unix: now,
            last_active_unix: None,
            last_played_unix: None,
            play_time_secs: 0,
            components: vec![Component {
                uid: GAME_UID.to_owned(),
                version: game_version.to_owned(),
            }],
        }
    }

    /// The game version, if the instance has one.
    pub fn game_version(&self) -> Option<&str> {
        self.component(GAME_UID)
    }

    fn component(&self, uid: &str) -> Option<&str> {
        self.components
            .iter()
            .find(|c| c.uid == uid)
            .map(|c| c.version.as_str())
    }

    /// Replaces the loader component (`None` removes it). The request must name a loader and its
    /// version; choosing a default version is the caller's job.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for the game as a loader, a missing or unsafe version, or an instance
    /// without a game version.
    pub fn set_loader(&mut self, loader: Option<&LoaderRequest>) -> Result<()> {
        let game = self
            .game_version()
            .ok_or_else(|| Error::invalid_input("the instance has no game version"))?
            .to_owned();
        let new = match loader {
            Some(request) => {
                if !request.kind.is_loader() {
                    return Err(Error::invalid_input("the game is not a loader"));
                }
                let version = request
                    .version
                    .as_deref()
                    .ok_or_else(|| Error::invalid_input("the loader version is not chosen"))?;
                // Reuses the version checks of the component set.
                ComponentSet::from_pairs([
                    (GAME_UID, game.as_str()),
                    (request.kind.uid(), version),
                ])?;
                Some(Component {
                    uid: request.kind.uid().to_owned(),
                    version: version.to_owned(),
                })
            }
            None => None,
        };
        self.components
            .retain(|c| !ComponentKind::from_uid(&c.uid).is_some_and(ComponentKind::is_loader));
        self.components.extend(new);
        Ok(())
    }

    /// The loader derived from the components.
    pub fn loader(&self) -> Loader {
        let get = |uid| self.component(uid).map(str::to_owned);
        if let Some(v) = get(FABRIC_UID) {
            Loader::Fabric(v)
        } else if let Some(v) = get(QUILT_UID) {
            Loader::Quilt(v)
        } else if let Some(v) = get(NEOFORGE_UID) {
            Loader::NeoForge(v)
        } else if let Some(v) = get(FORGE_UID) {
            Loader::Forge(v)
        } else {
            Loader::Vanilla
        }
    }
}
