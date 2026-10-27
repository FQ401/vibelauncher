//! Instance components: the game plus at most one mod loader, in profile order.
//!
//! An instance is a list of components, each a `uid` and a version (the shape stored in the
//! instance config). [`ComponentSet`] checks that list and gives it a meaning: exactly one game
//! component, at most one loader, applied in a fixed order (the game first, then the loader whose
//! profile inherits from the game). Loaders are added by extending [`ComponentKind`]; nothing
//! else here depends on which loader it is.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// What a component is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComponentKind {
    /// Minecraft itself.
    Game,
    /// Fabric Loader.
    Fabric,
    /// Quilt Loader.
    Quilt,
    /// NeoForge.
    NeoForge,
    /// Forge.
    Forge,
    /// LiteLoader.
    LiteLoader,
}

impl ComponentKind {
    /// Every kind, the game first.
    pub const ALL: [ComponentKind; 6] = [
        Self::Game,
        Self::Fabric,
        Self::Quilt,
        Self::NeoForge,
        Self::Forge,
        Self::LiteLoader,
    ];

    /// The stable identifier stored in instance configs.
    pub const fn uid(self) -> &'static str {
        match self {
            Self::Game => "net.minecraft",
            Self::Fabric => "net.fabricmc.fabric-loader",
            Self::Quilt => "org.quiltmc.quilt-loader",
            Self::NeoForge => "net.neoforged",
            Self::Forge => "net.minecraftforge",
            Self::LiteLoader => "com.mumfrey.liteloader",
        }
    }

    /// The kind with the given uid.
    pub fn from_uid(uid: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.uid() == uid)
    }

    /// The name shown to the user.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Game => "Minecraft",
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
            Self::NeoForge => "NeoForge",
            Self::Forge => "Forge",
            Self::LiteLoader => "LiteLoader",
        }
    }

    /// Whether this is a mod loader (anything but the game).
    pub const fn is_loader(self) -> bool {
        !matches!(self, Self::Game)
    }
}

/// A component of a given version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComponentRef {
    /// What it is.
    pub kind: ComponentKind,
    /// Its version, as the source names it.
    pub version: String,
}

impl ComponentRef {
    /// A component of `kind` and `version`.
    pub fn new(kind: ComponentKind, version: impl Into<String>) -> Self {
        Self {
            kind,
            version: version.into(),
        }
    }
}

/// How a change of components affects the prepared game files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuild {
    /// Nothing changed.
    None,
    /// Only the loader changed: its profile and the class path are rebuilt; game files stay.
    Classpath,
    /// The game version changed: everything is prepared again.
    Full,
}

/// The components of an instance: one game version and at most one loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentSet {
    game_version: String,
    loader: Option<ComponentRef>,
}

impl ComponentSet {
    /// A vanilla set.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when the version is empty or unsafe as a file-name part.
    pub fn vanilla(game_version: &str) -> Result<Self> {
        check_version("game", game_version)?;
        Ok(Self {
            game_version: game_version.to_owned(),
            loader: None,
        })
    }

    /// Reads the `(uid, version)` pairs of an instance config.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a missing or repeated game, more than one loader, or an invalid
    /// version; `Unsupported` for an unknown uid (written by a newer launcher).
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut game = None;
        let mut loader: Option<ComponentRef> = None;
        for (uid, version) in pairs {
            let kind = ComponentKind::from_uid(uid)
                .ok_or_else(|| Error::unsupported(format!("unknown component {uid}")))?;
            check_version(kind.display_name(), version)?;
            if kind == ComponentKind::Game {
                if game.replace(version.to_owned()).is_some() {
                    return Err(Error::invalid_input("the game component is listed twice"));
                }
            } else if let Some(other) = &loader {
                return Err(Error::invalid_input(format!(
                    "only one loader is allowed, found {} and {}",
                    other.kind.display_name(),
                    kind.display_name()
                )));
            } else {
                loader = Some(ComponentRef::new(kind, version));
            }
        }
        Ok(Self {
            game_version: game.ok_or_else(|| Error::invalid_input("no game component"))?,
            loader,
        })
    }

    /// The `(uid, version)` pairs in application order (game, then loader).
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        self.ordered()
            .into_iter()
            .map(|c| (c.kind.uid(), c.version))
            .collect()
    }

    /// The game version.
    pub fn game_version(&self) -> &str {
        &self.game_version
    }

    /// The loader, if any.
    pub fn loader(&self) -> Option<&ComponentRef> {
        self.loader.as_ref()
    }

    /// The components in the order their profiles are applied: the game, then the loader.
    pub fn ordered(&self) -> Vec<ComponentRef> {
        let mut v = vec![ComponentRef::new(
            ComponentKind::Game,
            self.game_version.clone(),
        )];
        v.extend(self.loader.clone());
        v
    }

    /// Replaces the loader (or removes it with `None`) and says what must be rebuilt. Whether the
    /// loader supports the game version is checked by the caller against the loader's version
    /// list.
    ///
    /// # Errors
    ///
    /// `InvalidInput` when the component is the game or its version is invalid.
    pub fn with_loader(&self, loader: Option<ComponentRef>) -> Result<(Self, Rebuild)> {
        if let Some(l) = &loader {
            if !l.kind.is_loader() {
                return Err(Error::invalid_input("the game is not a loader"));
            }
            check_version(l.kind.display_name(), &l.version)?;
        }
        let rebuild = if loader == self.loader {
            Rebuild::None
        } else {
            Rebuild::Classpath
        };
        Ok((
            Self {
                game_version: self.game_version.clone(),
                loader,
            },
            rebuild,
        ))
    }

    /// Replaces the game version. The loader is kept only with `keep_loader`, which the caller
    /// sets after confirming the loader supports the new version.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for an invalid version.
    pub fn with_game_version(&self, version: &str, keep_loader: bool) -> Result<(Self, Rebuild)> {
        check_version("game", version)?;
        let loader = if keep_loader {
            self.loader.clone()
        } else {
            None
        };
        let rebuild = if version == self.game_version && loader == self.loader {
            Rebuild::None
        } else {
            Rebuild::Full
        };
        Ok((
            Self {
                game_version: version.to_owned(),
                loader,
            },
            rebuild,
        ))
    }
}

/// A version is a non-empty identifier that is safe as a file-name part.
fn check_version(what: &str, version: &str) -> Result<()> {
    let bad_char = |c: char| {
        c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    };
    if version.is_empty()
        || version.len() > 128
        || version == "."
        || version == ".."
        || version.chars().any(bad_char)
    {
        return Err(Error::invalid_input(format!("invalid {what} version")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn uids_round_trip() {
        for k in ComponentKind::ALL {
            assert_eq!(ComponentKind::from_uid(k.uid()), Some(k));
        }
        assert_eq!(ComponentKind::from_uid("x"), None);
        assert!(!ComponentKind::Game.is_loader());
    }

    #[test]
    fn reads_and_orders_pairs() {
        let s = ComponentSet::from_pairs([
            ("net.fabricmc.fabric-loader", "0.20.1"),
            ("net.minecraft", "1.21.11"),
        ])
        .unwrap();
        assert_eq!(s.game_version(), "1.21.11");
        assert_eq!(
            s.to_pairs(),
            [
                ("net.minecraft", "1.21.11".to_owned()),
                ("net.fabricmc.fabric-loader", "0.20.1".to_owned())
            ]
        );
    }

    #[test]
    fn rejects_bad_sets() {
        let kind = |r: Result<ComponentSet>| r.unwrap_err().kind;
        assert_eq!(kind(ComponentSet::from_pairs([])), ErrorKind::InvalidInput);
        assert_eq!(
            kind(ComponentSet::from_pairs([
                ("net.minecraft", "1"),
                ("net.minecraft", "2")
            ])),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            kind(ComponentSet::from_pairs([
                ("net.minecraft", "1"),
                ("net.minecraftforge", "2"),
                ("net.neoforged", "3")
            ])),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            kind(ComponentSet::from_pairs([
                ("net.minecraft", "1"),
                ("a.b", "2")
            ])),
            ErrorKind::Unsupported
        );
        assert_eq!(kind(ComponentSet::vanilla("../x")), ErrorKind::InvalidInput);
    }

    #[test]
    fn loader_change_rebuilds_classpath_only() {
        let s = ComponentSet::vanilla("1.21.11").unwrap();
        let fabric = ComponentRef::new(ComponentKind::Fabric, "0.20.1");
        let (s, r) = s.with_loader(Some(fabric.clone())).unwrap();
        assert_eq!(r, Rebuild::Classpath);
        assert_eq!(s.with_loader(Some(fabric)).unwrap().1, Rebuild::None);
        let (s2, r) = s.with_game_version("1.21.10", false).unwrap();
        assert_eq!((r, s2.loader()), (Rebuild::Full, None));
        let game = ComponentRef::new(ComponentKind::Game, "1");
        assert!(s.with_loader(Some(game)).is_err());
    }
}
