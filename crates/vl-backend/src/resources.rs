//! Resource commands, served by `vl-resources`.
//!
//! Removed files go to `<instance>/.trash/`; nothing is erased. Listing reads mod jars, so every
//! command runs on its own thread.

use std::thread;

use vl_core::resources::{ResourceCommand, ResourceKind};
use vl_core::{InstanceId, Replier, Response};
use vl_resources::Resources;

use crate::Backend;

const TRASH_DIR: &str = ".trash";

pub(crate) fn open(backend: &Backend, id: &InstanceId) -> vl_core::Result<Resources> {
    let instance_dir = backend.instances.dir(id)?;
    if !instance_dir.is_dir() {
        return Err(vl_core::Error::not_found(format!("instance {id}")));
    }
    Ok(Resources::new(
        backend.instances.game_dir(id)?,
        instance_dir.join(TRASH_DIR),
    ))
}

pub(crate) fn handle(backend: &Backend, command: ResourceCommand, replier: Replier) {
    let opened = match &command {
        ResourceCommand::List { instance, .. }
        | ResourceCommand::SetEnabled { instance, .. }
        | ResourceCommand::Add { instance, .. }
        | ResourceCommand::Remove { instance, .. }
        | ResourceCommand::Worlds { instance }
        | ResourceCommand::WorldList { instance, .. }
        | ResourceCommand::WorldSetEnabled { instance, .. }
        | ResourceCommand::WorldAdd { instance, .. }
        | ResourceCommand::WorldRemove { instance, .. } => open(backend, instance),
        other => return crate::not_yet(replier, "resources", other),
    };
    let resources = match opened {
        Ok(r) => r,
        Err(e) => return replier.send(Err(e)),
    };
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name("resources".into())
        .spawn(move || replier.send(run(&resources, command)));
}

fn run(resources: &Resources, command: ResourceCommand) -> vl_core::Result<Response> {
    match command {
        ResourceCommand::List { kind, .. } => resources.list(kind).map(Response::Resources),
        ResourceCommand::SetEnabled {
            kind,
            file,
            enabled,
            ..
        } => {
            resources.set_enabled(kind, &file, enabled)?;
            Ok(Response::Done)
        }
        ResourceCommand::Add { kind, source, .. } => {
            resources.add(kind, std::path::Path::new(&source))?;
            Ok(Response::Done)
        }
        ResourceCommand::Remove { kind, file, .. } => {
            resources.remove(kind, &file)?;
            Ok(Response::Done)
        }
        ResourceCommand::Worlds { .. } => resources.worlds().map(Response::Worlds),
        ResourceCommand::WorldList { world, .. } => resources
            .world(&world)?
            .list(ResourceKind::DataPacks)
            .map(Response::Resources),
        ResourceCommand::WorldSetEnabled {
            world,
            file,
            enabled,
            ..
        } => {
            resources
                .world(&world)?
                .set_enabled(ResourceKind::DataPacks, &file, enabled)?;
            Ok(Response::Done)
        }
        ResourceCommand::WorldAdd { world, source, .. } => {
            resources
                .world(&world)?
                .add(ResourceKind::DataPacks, std::path::Path::new(&source))?;
            Ok(Response::Done)
        }
        ResourceCommand::WorldRemove { world, file, .. } => {
            resources
                .world(&world)?
                .remove(ResourceKind::DataPacks, &file)?;
            Ok(Response::Done)
        }
        other => Err(vl_core::Error::unsupported(format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use vl_core::instance::{InstanceCommand, NewInstance};
    use vl_core::{Command, ErrorKind, Facade, Response};

    use super::*;
    use crate::BackendConfig;

    #[test]
    fn manages_mods_of_an_instance() {
        let dir = std::env::temp_dir().join(format!("vl-backend-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let call = |c: ResourceCommand| backend.call(Command::Resources(c)).wait();
        let created = backend
            .call(Command::Instances(InstanceCommand::Create(NewInstance {
                name: "Modded".into(),
                game_version: "1.20.1".into(),
                group: None,
                loader: None,
            })))
            .wait()
            .unwrap();
        let Response::Instance(summary) = created else {
            panic!("unexpected {created:?}");
        };
        let id = summary.id;
        let source = dir.join("incoming.jar");
        std::fs::write(&source, b"not really a jar").unwrap();

        let add = ResourceCommand::Add {
            instance: id.clone(),
            kind: ResourceKind::Mods,
            source: source.to_string_lossy().into_owned(),
        };
        assert_eq!(call(add).unwrap(), Response::Done);
        let off = ResourceCommand::SetEnabled {
            instance: id.clone(),
            kind: ResourceKind::Mods,
            file: "incoming.jar".into(),
            enabled: false,
        };
        assert_eq!(call(off).unwrap(), Response::Done);
        let list = ResourceCommand::List {
            instance: id.clone(),
            kind: ResourceKind::Mods,
        };
        let Response::Resources(entries) = call(list).unwrap() else {
            panic!("expected resources");
        };
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].enabled && entries[0].problem.is_some());
        let remove = ResourceCommand::Remove {
            instance: id,
            kind: ResourceKind::Mods,
            file: "incoming.jar".into(),
        };
        assert_eq!(call(remove).unwrap(), Response::Done);
        let missing = ResourceCommand::List {
            instance: vl_core::InstanceId::new("nope").unwrap(),
            kind: ResourceKind::Mods,
        };
        assert_eq!(call(missing).unwrap_err().kind, ErrorKind::NotFound);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn manages_data_packs_of_a_world_and_refuses_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("vl-backend-world-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let call = |c: ResourceCommand| backend.call(Command::Resources(c)).wait();
        let created = backend
            .call(Command::Instances(InstanceCommand::Create(NewInstance {
                name: "Worlds".into(),
                game_version: "1.20.1".into(),
                group: None,
                loader: None,
            })))
            .wait()
            .unwrap();
        let Response::Instance(summary) = created else {
            panic!("unexpected {created:?}");
        };
        let instance = summary.id;
        let worlds = || {
            let Response::Worlds(w) = call(ResourceCommand::Worlds {
                instance: instance.clone(),
            })
            .unwrap() else {
                panic!("expected worlds");
            };
            w
        };
        assert!(worlds().is_empty());
        let missing = ResourceCommand::WorldList {
            instance: instance.clone(),
            world: "Test World".into(),
        };
        let err = call(missing).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        assert!(err.message.contains("create a world first"), "{err}");

        let game_dir = backend.instances.game_dir(&instance).unwrap();
        let world_dir = game_dir.join("saves").join("Test World");
        std::fs::create_dir_all(&world_dir).unwrap();
        std::fs::write(world_dir.join("level.dat"), b"stub").unwrap();
        assert_eq!(worlds().len(), 1);

        let source = dir.join("caves.zip");
        std::fs::write(&source, b"zip").unwrap();
        let add = ResourceCommand::WorldAdd {
            instance: instance.clone(),
            world: "Test World".into(),
            source: source.to_string_lossy().into_owned(),
        };
        assert_eq!(call(add).unwrap(), Response::Done);
        assert!(world_dir.join("datapacks").join("caves.zip").is_file());
        let off = ResourceCommand::WorldSetEnabled {
            instance: instance.clone(),
            world: "Test World".into(),
            file: "caves.zip".into(),
            enabled: false,
        };
        assert_eq!(call(off).unwrap(), Response::Done);
        assert!(
            world_dir
                .join("datapacks")
                .join("caves.zip.disabled")
                .is_file()
        );
        let escape = ResourceCommand::WorldList {
            instance: instance.clone(),
            world: "../..".into(),
        };
        assert_eq!(call(escape).unwrap_err().kind, ErrorKind::InvalidInput);
        let remove = ResourceCommand::WorldRemove {
            instance,
            world: "Test World".into(),
            file: "caves.zip".into(),
        };
        assert_eq!(call(remove).unwrap(), Response::Done);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
