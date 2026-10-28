//! Instance resources: mods, resource packs, shader packs and data packs, and the metadata read
//! from mod jars.

use serde::{Deserialize, Serialize};

use crate::InstanceId;

/// A folder of an instance's game directory that holds user-managed files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceKind {
    /// `mods/`.
    Mods,
    /// `resourcepacks/`.
    ResourcePacks,
    /// `shaderpacks/`.
    ShaderPacks,
    /// `datapacks/`.
    DataPacks,
    /// `global_packs/required_data/`: data packs applied to every world.
    GlobalDataPacks,
}

impl ResourceKind {
    /// Every kind, in display order.
    pub const ALL: [ResourceKind; 5] = [
        Self::Mods,
        Self::ResourcePacks,
        Self::ShaderPacks,
        Self::DataPacks,
        Self::GlobalDataPacks,
    ];

    /// The folder, relative to the game directory, with `/` separators.
    pub fn dir(self) -> &'static str {
        match self {
            Self::Mods => "mods",
            Self::ResourcePacks => "resourcepacks",
            Self::ShaderPacks => "shaderpacks",
            Self::DataPacks => "datapacks",
            Self::GlobalDataPacks => "global_packs/required_data",
        }
    }

    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mods => "mods",
            Self::ResourcePacks => "resourcepacks",
            Self::ShaderPacks => "shaderpacks",
            Self::DataPacks => "datapacks",
            Self::GlobalDataPacks => "global-datapacks",
        }
    }

    /// The kind with the given command-line name.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// The mod loader a mod's metadata belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModLoader {
    /// `fabric.mod.json`.
    Fabric,
    /// `quilt.mod.json`.
    Quilt,
    /// `META-INF/mods.toml`.
    Forge,
    /// `META-INF/neoforge.mods.toml`.
    NeoForge,
    /// `mcmod.info`.
    LegacyForge,
}

/// How a mod relates to another mod or to the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DependencyKind {
    /// Must be present.
    Required,
    /// Works better with it, runs without it.
    Optional,
    /// Must not be present together with this mod.
    Conflicts,
}

/// A dependency declared by a mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModDependency {
    /// The other mod's id (`minecraft` for the game).
    pub id: String,
    /// What kind of relation it is.
    pub kind: DependencyKind,
    /// The accepted versions in the loader's own syntax, when declared.
    pub versions: Option<String>,
}

/// What a mod declares about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModMetadata {
    /// The loader whose metadata file this came from.
    pub loader: ModLoader,
    /// The mod id.
    pub id: String,
    /// The display name (the id when absent).
    pub name: String,
    /// The version, when declared.
    pub version: Option<String>,
    /// Authors, in declaration order.
    pub authors: Vec<String>,
    /// A description, when declared.
    pub description: Option<String>,
    /// Path of the icon inside the jar, when declared.
    pub icon: Option<String>,
    /// Declared dependencies, the game included.
    pub dependencies: Vec<ModDependency>,
    /// The accepted game versions in the loader's own syntax (from the `minecraft` dependency).
    pub game_versions: Option<String>,
}

/// One file or folder in a resource folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEntry {
    /// The name without the `.disabled` suffix; this is what commands take.
    pub file: String,
    /// Whether the game will see it.
    pub enabled: bool,
    /// Size in bytes (0 for a folder).
    pub size: u64,
    /// Last modification, seconds since the Unix epoch.
    pub modified: Option<i64>,
    /// Whether it is a folder (an unpacked pack).
    pub is_dir: bool,
    /// Mods declared in the file (mods folder only; empty when none could be read).
    pub mods: Vec<ModMetadata>,
    /// Jars bundled inside this jar (listed, not read).
    pub nested_jars: Vec<String>,
    /// Why the metadata could not be read, when it could not.
    pub problem: Option<String>,
}

/// Commands on instance resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceCommand {
    /// Lists a folder → `Response::Resources`.
    List {
        /// The instance.
        instance: InstanceId,
        /// The folder.
        kind: ResourceKind,
    },
    /// Enables or disables a file (renames it with or without `.disabled`) → `Response::Done`.
    SetEnabled {
        /// The instance.
        instance: InstanceId,
        /// The folder.
        kind: ResourceKind,
        /// The entry's `file`.
        file: String,
        /// The wanted state.
        enabled: bool,
    },
    /// Copies a local file into the folder → `Response::Done`. Refuses to replace a file.
    Add {
        /// The instance.
        instance: InstanceId,
        /// The folder.
        kind: ResourceKind,
        /// Path of the file to copy.
        source: String,
    },
    /// Moves a file into the instance's trash → `Response::Done`.
    Remove {
        /// The instance.
        instance: InstanceId,
        /// The folder.
        kind: ResourceKind,
        /// The entry's `file`.
        file: String,
    },
}
