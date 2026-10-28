//! CurseForge's mod loader types.

/// A mod loader as CurseForge numbers it (`ModLoaderType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModLoader {
    /// Forge (`1`).
    Forge,
    /// Cauldron (`2`).
    Cauldron,
    /// LiteLoader (`3`).
    LiteLoader,
    /// Fabric (`4`).
    Fabric,
    /// Quilt (`5`).
    Quilt,
    /// NeoForge (`6`).
    NeoForge,
}

impl ModLoader {
    const ALL: [ModLoader; 6] = [
        Self::Forge,
        Self::Cauldron,
        Self::LiteLoader,
        Self::Fabric,
        Self::Quilt,
        Self::NeoForge,
    ];

    /// The number CurseForge uses in requests and responses.
    pub fn id(self) -> u32 {
        match self {
            Self::Forge => 1,
            Self::Cauldron => 2,
            Self::LiteLoader => 3,
            Self::Fabric => 4,
            Self::Quilt => 5,
            Self::NeoForge => 6,
        }
    }

    /// The loader with CurseForge number `id`; `None` for `0` (any) and unknown numbers.
    pub fn from_id(id: u64) -> Option<Self> {
        Self::ALL.into_iter().find(|l| u64::from(l.id()) == id)
    }

    /// The launcher's name of the loader (`forge`, `fabric`, `quilt`, `neoforge`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::Cauldron => "cauldron",
            Self::LiteLoader => "liteloader",
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::NeoForge => "neoforge",
        }
    }

    /// Parses a launcher loader name, or the label CurseForge puts among a file's game versions
    /// (`Forge`, `NeoForge`), ignoring case.
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names_round_trip() {
        for loader in ModLoader::ALL {
            assert_eq!(ModLoader::from_id(u64::from(loader.id())), Some(loader));
            assert_eq!(ModLoader::parse(loader.name()), Some(loader));
        }
        assert_eq!(ModLoader::parse("NeoForge"), Some(ModLoader::NeoForge));
        assert_eq!(ModLoader::from_id(0), None);
        assert_eq!(ModLoader::parse("rift"), None);
    }
}
