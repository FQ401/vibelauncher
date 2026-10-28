//! `mods.toml` (Forge) and `neoforge.mods.toml` (NeoForge).

use toml::Value;
use toml::map::Map;
use vl_core::Error;
use vl_core::resources::{DependencyKind, ModDependency, ModLoader, ModMetadata};

use crate::{damaged, non_empty};

fn str_of(t: &Map<String, Value>, key: &str) -> Option<String> {
    non_empty(t.get(key).and_then(Value::as_str))
}

/// Parses a `mods.toml` file of `loader` (`Forge` or `NeoForge`).
pub fn parse_mods_toml(bytes: &[u8], loader: ModLoader) -> Result<Vec<ModMetadata>, Error> {
    let what = if loader == ModLoader::NeoForge {
        "neoforge.mods.toml"
    } else {
        "mods.toml"
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|e| damaged(what, e))?
        .trim_start_matches('\u{feff}');
    let root: Map<String, Value> = toml::from_str(text).map_err(|e| damaged(what, e))?;
    let top_authors = str_of(&root, "authors");
    let top_logo = str_of(&root, "logoFile");
    let Some(Value::Array(entries)) = root.get("mods") else {
        return Err(damaged(what, "no [[mods]] table"));
    };
    let mods: Vec<ModMetadata> = entries
        .iter()
        .filter_map(|e| {
            let t = e.as_table()?;
            let id = str_of(t, "modId")?;
            let dependencies = dependencies_of(&root, &id);
            let game_versions = dependencies
                .iter()
                .find(|d| d.id == "minecraft" && d.kind == DependencyKind::Required)
                .and_then(|d| d.versions.clone());
            let authors = str_of(t, "authors")
                .or_else(|| top_authors.clone())
                .map(|a| {
                    a.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Some(ModMetadata {
                loader,
                name: str_of(t, "displayName").unwrap_or_else(|| id.clone()),
                version: str_of(t, "version"),
                authors,
                description: str_of(t, "description"),
                icon: str_of(t, "logoFile").or_else(|| top_logo.clone()),
                game_versions,
                dependencies,
                id,
            })
        })
        .collect();
    if mods.is_empty() {
        return Err(damaged(what, "no mod with a modId"));
    }
    Ok(mods)
}

/// The `[[dependencies.<id>]]` tables of one mod.
fn dependencies_of(root: &Map<String, Value>, mod_id: &str) -> Vec<ModDependency> {
    let Some(Value::Array(items)) = root
        .get("dependencies")
        .and_then(Value::as_table)
        .and_then(|d| d.get(mod_id))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|d| {
            let t = d.as_table()?;
            let id = str_of(t, "modId")?;
            // NeoForge: `type`; Forge: `mandatory`.
            let kind = match str_of(t, "type").as_deref() {
                Some("required") => DependencyKind::Required,
                Some("optional" | "discouraged") => DependencyKind::Optional,
                Some("incompatible") => DependencyKind::Conflicts,
                _ => {
                    if t.get("mandatory").and_then(Value::as_bool) == Some(false) {
                        DependencyKind::Optional
                    } else {
                        DependencyKind::Required
                    }
                }
            };
            Some(ModDependency {
                id,
                kind,
                versions: str_of(t, "versionRange"),
            })
        })
        .collect()
}
