//! JSON metadata: Fabric, Quilt and legacy Forge.

use serde_json::Value;
use vl_core::Error;
use vl_core::resources::{DependencyKind, ModDependency, ModLoader, ModMetadata};

use crate::{damaged, non_empty};

fn parse(what: &str, bytes: &[u8]) -> Result<Value, Error> {
    // Some jars start the file with a byte order mark.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice(bytes).map_err(|e| damaged(what, e))
}

fn str_of(v: Option<&Value>) -> Option<String> {
    non_empty(v.and_then(Value::as_str))
}

/// `"1.2"`, `["1.2", "1.3"]` → one string; an array joins with ` || ` (any of).
fn version_range(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => non_empty(Some(s)),
        Value::Array(items) => {
            let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            non_empty(Some(&parts.join(" || ")))
        }
        _ => None,
    }
}

/// Names from an array of strings or `{ "name": ... }` objects.
fn people(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| match p {
            Value::String(s) => non_empty(Some(s)),
            Value::Object(_) => str_of(p.get("name")),
            _ => None,
        })
        .collect()
}

/// A string, or the entry of an object with the largest numeric key (icon sizes).
fn icon_path(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => non_empty(Some(s)),
        Value::Object(map) => map
            .iter()
            .filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.as_str()?)))
            .max_by_key(|(size, _)| *size)
            .and_then(|(_, path)| non_empty(Some(path))),
        _ => None,
    }
}

fn game_versions(deps: &[ModDependency]) -> Option<String> {
    deps.iter()
        .find(|d| d.id == "minecraft" && d.kind == DependencyKind::Required)
        .and_then(|d| d.versions.clone())
}

fn dep(id: &str, kind: DependencyKind, versions: Option<String>) -> ModDependency {
    ModDependency {
        id: id.to_owned(),
        kind,
        versions,
    }
}

/// `fabric.mod.json`.
pub fn parse_fabric(bytes: &[u8]) -> Result<ModMetadata, Error> {
    let v = parse("fabric.mod.json", bytes)?;
    let id = str_of(v.get("id")).ok_or_else(|| damaged("fabric.mod.json", "no mod id"))?;
    let mut dependencies = Vec::new();
    for (key, kind) in [
        ("depends", DependencyKind::Required),
        ("recommends", DependencyKind::Optional),
        ("suggests", DependencyKind::Optional),
        ("conflicts", DependencyKind::Conflicts),
        ("breaks", DependencyKind::Conflicts),
    ] {
        if let Some(Value::Object(map)) = v.get(key) {
            for (dep_id, range) in map {
                dependencies.push(dep(dep_id, kind, version_range(Some(range))));
            }
        }
    }
    Ok(ModMetadata {
        loader: ModLoader::Fabric,
        name: str_of(v.get("name")).unwrap_or_else(|| id.clone()),
        version: str_of(v.get("version")),
        authors: people(v.get("authors")),
        description: str_of(v.get("description")),
        icon: icon_path(v.get("icon")),
        game_versions: game_versions(&dependencies),
        dependencies,
        id,
    })
}

/// `quilt.mod.json`.
pub fn parse_quilt(bytes: &[u8]) -> Result<ModMetadata, Error> {
    let v = parse("quilt.mod.json", bytes)?;
    let loader = v
        .get("quilt_loader")
        .ok_or_else(|| damaged("quilt.mod.json", "no quilt_loader"))?;
    let id = str_of(loader.get("id")).ok_or_else(|| damaged("quilt.mod.json", "no mod id"))?;
    let meta = loader.get("metadata");
    let mut dependencies = Vec::new();
    for (key, kind) in [
        ("depends", DependencyKind::Required),
        ("breaks", DependencyKind::Conflicts),
    ] {
        let Some(Value::Array(items)) = loader.get(key) else {
            continue;
        };
        for item in items {
            match item {
                Value::String(s) => dependencies.push(dep(s, kind, None)),
                Value::Object(_) => {
                    let Some(dep_id) = str_of(item.get("id")) else {
                        continue;
                    };
                    let optional = item.get("optional").and_then(Value::as_bool) == Some(true);
                    let kind = if optional && kind == DependencyKind::Required {
                        DependencyKind::Optional
                    } else {
                        kind
                    };
                    dependencies.push(dep(&dep_id, kind, version_range(item.get("versions"))));
                }
                _ => {}
            }
        }
    }
    // Contributors map a name to a role; owners come first.
    let mut authors: Vec<(bool, String)> = Vec::new();
    if let Some(Value::Object(map)) = meta.and_then(|m| m.get("contributors")) {
        for (name, role) in map {
            authors.push((role.as_str() != Some("Owner"), name.clone()));
        }
    }
    authors.sort_by_key(|(not_owner, _)| *not_owner);
    Ok(ModMetadata {
        loader: ModLoader::Quilt,
        name: str_of(meta.and_then(|m| m.get("name"))).unwrap_or_else(|| id.clone()),
        version: str_of(loader.get("version")),
        authors: authors.into_iter().map(|(_, n)| n).collect(),
        description: str_of(meta.and_then(|m| m.get("description"))),
        icon: icon_path(meta.and_then(|m| m.get("icon"))),
        game_versions: game_versions(&dependencies),
        dependencies,
        id,
    })
}

/// `mcmod.info`: an array of mods, or an object with a `modList` array.
pub fn parse_mcmod_info(bytes: &[u8]) -> Result<Vec<ModMetadata>, Error> {
    let v = parse("mcmod.info", bytes)?;
    let list = match &v {
        Value::Array(items) => items.as_slice(),
        Value::Object(_) => match v.get("modList") {
            Some(Value::Array(items)) => items.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    let mods: Vec<ModMetadata> = list
        .iter()
        .filter_map(|m| {
            let id = str_of(m.get("modid"))?;
            let mut authors = people(m.get("authorList"));
            if authors.is_empty() {
                authors = people(m.get("authors"));
            }
            let mut dependencies = Vec::new();
            for key in ["requiredMods", "dependencies"] {
                if let Some(Value::Array(items)) = m.get(key) {
                    for dep_id in items.iter().filter_map(Value::as_str) {
                        // Entries may carry a version after `@`.
                        let dep_id = dep_id.split('@').next().unwrap_or(dep_id);
                        dependencies.push(dep(dep_id, DependencyKind::Required, None));
                    }
                }
            }
            Some(ModMetadata {
                loader: ModLoader::LegacyForge,
                name: str_of(m.get("name")).unwrap_or_else(|| id.clone()),
                version: str_of(m.get("version")),
                authors,
                description: str_of(m.get("description")),
                icon: str_of(m.get("logoFile")),
                game_versions: str_of(m.get("mcversion")),
                dependencies,
                id,
            })
        })
        .collect();
    if mods.is_empty() {
        return Err(damaged("mcmod.info", "no mod with an id"));
    }
    Ok(mods)
}
