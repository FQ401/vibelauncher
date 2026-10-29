//! Synthetic instance resources: the mods, packs and shaders of a few instances, and the worlds
//! with their data packs. The files are in memory and change with the commands.

use std::sync::Arc;

use vl_core::instance::{InstanceSummary, Loader};
use vl_core::resources::{
    DependencyKind, ModDependency, ModLoader, ModMetadata, PackMetadata, ResourceCommand,
    ResourceEntry, ResourceKind, WorldInfo,
};
use vl_core::{Error, InstanceId, Response, Result};

use crate::catalog::{self, ENTRIES, VERSIONS};
use crate::{Inner, State};

/// The key of a resource folder in the state.
pub(crate) fn area(kind: ResourceKind) -> String {
    kind.dir().into()
}

/// The key of a world's data pack folder in the state.
pub(crate) fn world_area(world: &str) -> String {
    format!("world/{world}")
}

/// The name of the loader a mod must be for, `None` for the game without one.
pub(crate) fn loader_name(loader: &Loader) -> Option<&'static str> {
    match loader {
        Loader::Fabric(_) => Some("fabric"),
        Loader::Quilt(_) => Some("quilt"),
        Loader::NeoForge(_) => Some("neoforge"),
        Loader::Forge(_) => Some("forge"),
        _ => None,
    }
}

/// The instance if it exists and is usable.
pub(crate) fn usable<'a>(state: &'a State, id: &InstanceId) -> Result<&'a InstanceSummary> {
    let instance = state
        .instances
        .iter()
        .find(|i| &i.id == id)
        .ok_or_else(|| Error::not_found(format!("instance {id}")))?;
    match &instance.problem {
        Some(problem) => Err(Error::invalid_input(problem.clone())),
        None => Ok(instance),
    }
}

/// The worlds of an instance: `(folder, display name, last played)`.
fn world_rows(id: &InstanceId) -> &'static [(&'static str, &'static str, i64)] {
    match id.as_str() {
        "creative" => &[
            ("creative-world", "Creative World", 1_792_500_000),
            ("build-test", "Build Test", 1_792_000_000),
        ],
        "survival" => &[("survival-world", "Survival World", 1_793_000_000)],
        _ => &[],
    }
}

/// The initial files of a folder: `(name, enabled)`.
fn seed_rows(id: &InstanceId, area: &str) -> &'static [(&'static str, bool)] {
    match (id.as_str(), area) {
        ("creative", "mods") => &[
            ("mock-library-2.0.0.jar", true),
            ("mock-renderer-1.1.0.jar", true),
            ("mock-minimap-2.0.0.jar", true),
            ("mock-weather-fx-1.1.0.jar", true),
            ("mock-inventory-tweaks-1.1.0.jar", false),
            ("mock-pocket-storage-1.1.0.jar", true),
            ("mock-cf-library-2.0.0.jar", true),
            ("local-tweak.jar", true),
        ],
        ("creative", "resourcepacks") => {
            &[("mock-textures-1.1.0.zip", true), ("hand-made-pack", true)]
        }
        ("creative", "shaderpacks") => &[("mock-shaders-soft-2.0.0.zip", true)],
        ("survival", "mods") => &[("stray-library.jar", true)],
        ("survival", "resourcepacks") => &[("mock-pixel-pack-2.0.0.zip", true)],
        ("creative", "world/creative-world") => &[("mock-recipes-datapack-1.1.0.zip", true)],
        ("survival", "world/survival-world") => &[
            ("mock-recipes-datapack-2.0.0.zip", true),
            ("hand-made-datapack", false),
        ],
        _ => &[],
    }
}

fn kind_of_area(area: &str) -> ResourceKind {
    ResourceKind::ALL
        .into_iter()
        .find(|k| k.dir() == area)
        .unwrap_or(ResourceKind::DataPacks)
}

/// A file as the folder lists it. Catalog files carry their project's metadata, anything else a
/// made-up local one.
pub(crate) fn make_entry(kind: ResourceKind, file: &str, enabled: bool) -> ResourceEntry {
    let seed = file.bytes().map(u64::from).sum::<u64>();
    let is_dir = !file.contains('.');
    let known = catalog::find_by_file(file);
    let mods = if kind == ResourceKind::Mods {
        let (id, name, version, dependencies) = match known {
            Some((idx, v)) => {
                let entry = &ENTRIES[idx];
                let mut dependencies = vec![ModDependency {
                    id: "minecraft".into(),
                    kind: DependencyKind::Required,
                    versions: Some(">=1.21".into()),
                }];
                let library = catalog::library_of(entry.provider);
                if idx != library {
                    dependencies.push(ModDependency {
                        id: ENTRIES[library].slug.into(),
                        kind: DependencyKind::Required,
                        versions: None,
                    });
                }
                (
                    entry.slug.to_owned(),
                    entry.title.to_owned(),
                    VERSIONS[v].0,
                    dependencies,
                )
            }
            None => (
                file.trim_end_matches(".jar").to_owned(),
                "Local Tweak".to_owned(),
                "1.0.0",
                Vec::new(),
            ),
        };
        vec![ModMetadata {
            loader: ModLoader::Fabric,
            id,
            name,
            version: Some(version.into()),
            authors: vec!["mock-author".into()],
            description: Some("A synthetic mod.".into()),
            icon: None,
            dependencies,
            game_versions: Some(">=1.21".into()),
        }]
    } else {
        Vec::new()
    };
    let pack = (kind != ResourceKind::Mods && !is_dir).then(|| PackMetadata {
        pack_format: Some(46),
        min_format: None,
        max_format: None,
        description: Some(known.map_or_else(
            || "A synthetic pack.".to_owned(),
            |(idx, _)| ENTRIES[idx].title.to_owned(),
        )),
    });
    ResourceEntry {
        file: file.into(),
        enabled,
        size: if is_dir {
            0
        } else {
            20_000 + seed * 311 % 500_000
        },
        modified: Some(1_790_000_000 + seed as i64),
        is_dir,
        mods,
        nested_jars: Vec::new(),
        problem: None,
        pack,
    }
}

fn seed(id: &InstanceId, area: &str) -> Vec<ResourceEntry> {
    let kind = kind_of_area(area);
    seed_rows(id, area)
        .iter()
        .map(|(file, enabled)| make_entry(kind, file, *enabled))
        .collect()
}

/// The files of a folder, without creating it.
pub(crate) fn snapshot(state: &State, id: &InstanceId, area: &str) -> Vec<ResourceEntry> {
    state
        .resources
        .get(&(id.clone(), area.to_owned()))
        .cloned()
        .unwrap_or_else(|| seed(id, area))
}

/// The files of a folder, created from the seed on first use.
pub(crate) fn folder<'a>(
    state: &'a mut State,
    id: &InstanceId,
    area: &str,
) -> &'a mut Vec<ResourceEntry> {
    state
        .resources
        .entry((id.clone(), area.to_owned()))
        .or_insert_with(|| seed(id, area))
}

/// The folder name of `world` if the instance has it.
pub(crate) fn check_world(id: &InstanceId, world: &str) -> Result<()> {
    if world_rows(id).iter().any(|(name, ..)| *name == world) {
        Ok(())
    } else {
        Err(Error::not_found(format!("world {world}")))
    }
}

fn sorted(mut files: Vec<ResourceEntry>) -> Vec<ResourceEntry> {
    files.sort_by(|a, b| a.file.cmp(&b.file));
    files
}

fn file_of(source: &str) -> Result<String> {
    let name = source.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() {
        Err(Error::invalid_input("the source has no file name"))
    } else {
        Ok(name.into())
    }
}

fn set_enabled(files: &mut [ResourceEntry], file: &str, enabled: bool) -> Result<()> {
    let entry = files
        .iter_mut()
        .find(|e| e.file == file)
        .ok_or_else(|| Error::not_found(format!("file {file}")))?;
    entry.enabled = enabled;
    Ok(())
}

fn add(files: &mut Vec<ResourceEntry>, kind: ResourceKind, source: &str) -> Result<()> {
    let file = file_of(source)?;
    if files.iter().any(|e| e.file == file) {
        return Err(Error::conflict(format!("{file} exists")));
    }
    files.push(make_entry(kind, &file, true));
    Ok(())
}

fn remove(files: &mut Vec<ResourceEntry>, file: &str) -> Result<()> {
    let before = files.len();
    files.retain(|e| e.file != file);
    if files.len() == before {
        return Err(Error::not_found(format!("file {file}")));
    }
    Ok(())
}

pub(crate) fn handle(inner: &Arc<Inner>, command: ResourceCommand) -> Result<Response> {
    let mut state = inner.lock();
    let state = &mut *state;
    match command {
        ResourceCommand::List { instance, kind } => {
            usable(state, &instance)?;
            Ok(Response::Resources(sorted(snapshot(
                state,
                &instance,
                &area(kind),
            ))))
        }
        ResourceCommand::SetEnabled {
            instance,
            kind,
            file,
            enabled,
        } => {
            usable(state, &instance)?;
            set_enabled(folder(state, &instance, &area(kind)), &file, enabled)?;
            Ok(Response::Done)
        }
        ResourceCommand::Add {
            instance,
            kind,
            source,
        } => {
            usable(state, &instance)?;
            add(folder(state, &instance, &area(kind)), kind, &source)?;
            Ok(Response::Done)
        }
        ResourceCommand::Remove {
            instance,
            kind,
            file,
        } => {
            usable(state, &instance)?;
            remove(folder(state, &instance, &area(kind)), &file)?;
            Ok(Response::Done)
        }
        ResourceCommand::Worlds { instance } => {
            usable(state, &instance)?;
            let worlds = world_rows(&instance)
                .iter()
                .map(|(name, display, played)| WorldInfo {
                    name: (*name).into(),
                    display_name: Some((*display).into()),
                    version: state
                        .instances
                        .iter()
                        .find(|i| i.id == instance)
                        .map(|i| i.game_version.clone()),
                    last_played: Some(*played),
                    datapacks: snapshot(state, &instance, &world_area(name)).len() as u32,
                })
                .collect();
            Ok(Response::Worlds(worlds))
        }
        ResourceCommand::WorldList { instance, world } => {
            usable(state, &instance)?;
            check_world(&instance, &world)?;
            Ok(Response::Resources(sorted(snapshot(
                state,
                &instance,
                &world_area(&world),
            ))))
        }
        ResourceCommand::WorldSetEnabled {
            instance,
            world,
            file,
            enabled,
        } => {
            usable(state, &instance)?;
            check_world(&instance, &world)?;
            set_enabled(
                folder(state, &instance, &world_area(&world)),
                &file,
                enabled,
            )?;
            Ok(Response::Done)
        }
        ResourceCommand::WorldAdd {
            instance,
            world,
            source,
        } => {
            usable(state, &instance)?;
            check_world(&instance, &world)?;
            add(
                folder(state, &instance, &world_area(&world)),
                ResourceKind::DataPacks,
                &source,
            )?;
            Ok(Response::Done)
        }
        ResourceCommand::WorldRemove {
            instance,
            world,
            file,
        } => {
            usable(state, &instance)?;
            check_world(&instance, &world)?;
            remove(folder(state, &instance, &world_area(&world)), &file)?;
            Ok(Response::Done)
        }
        other => Err(Error::unsupported(format!(
            "vl-mock does not simulate {other:?}"
        ))),
    }
}
