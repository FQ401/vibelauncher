//! Instance commands, served by `vl-instance`.
//!
//! Delete moves the instance into the launcher's own trash inside the instances directory (see
//! `vl_instance::Instances::delete`); no command erases data.

use std::thread;

use vl_core::instance::{InstanceCommand, InstanceEvent, InstanceSummary, LoaderRequest};
use vl_core::{Error, Event, InstanceId, Replier, Response};
use vl_net::Control;

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: InstanceCommand, replier: Replier) {
    let service = &backend.instances;
    let result = match command {
        InstanceCommand::List => service.list().map(Response::Instances),
        InstanceCommand::Create(new) if new.loader.is_some() => {
            return create_with_loader(backend, new, replier);
        }
        InstanceCommand::Create(new) => service.create(&new).map(|i| {
            backend
                .events
                .publish(&Event::Instance(InstanceEvent::Added(i.clone())));
            Response::Instance(i)
        }),
        InstanceCommand::SetLoader { id, loader } => {
            return set_loader(backend, id, loader, replier);
        }
        InstanceCommand::Rename { id, name } => service.rename(&id, &name).map(|i| {
            if i.id != id {
                backend
                    .events
                    .publish(&Event::Instance(InstanceEvent::Removed(id)));
                backend
                    .events
                    .publish(&Event::Instance(InstanceEvent::Added(i.clone())));
            } else {
                backend
                    .events
                    .publish(&Event::Instance(InstanceEvent::Changed(i.clone())));
            }
            Response::Instance(i)
        }),
        InstanceCommand::Delete { id } => service.delete(&id).map(|_| {
            backend
                .events
                .publish(&Event::Instance(InstanceEvent::Removed(id)));
            Response::Done
        }),
        other => {
            return crate::not_yet(replier, "instances", &other);
        }
    };
    replier.send(result);
}

/// Runs `work` on its own thread (it may wait for the network) and answers with its result.
fn answer_later(
    replier: Replier,
    work: impl FnOnce() -> vl_core::Result<Response> + Send + 'static,
) {
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _ = thread::Builder::new()
        .name("instance-loader".into())
        .spawn(move || replier.send(work()));
}

fn create_with_loader(
    backend: &Backend,
    mut new: vl_core::instance::NewInstance,
    replier: Replier,
) {
    let backend_instances = backend.instances.clone();
    let events = std::sync::Arc::clone(&backend.events);
    let loaders = backend.launch.loaders().clone();
    answer_later(replier, move || {
        if let Some(request) = &new.loader {
            let chosen = loaders.pick(
                request.kind,
                new.game_version.trim(),
                request.version.as_deref(),
                &Control::default(),
            )?;
            new.loader = Some(LoaderRequest {
                kind: request.kind,
                version: Some(chosen.version),
            });
        }
        let created = backend_instances.create(&new)?;
        events.publish(&Event::Instance(InstanceEvent::Added(created.clone())));
        Ok(Response::Instance(created))
    });
}

fn set_loader(backend: &Backend, id: InstanceId, loader: Option<LoaderRequest>, replier: Replier) {
    if backend.launch.is_active(&id) {
        replier.send(Err(Error::conflict(format!("instance {id} is running"))));
        return;
    }
    let instances = backend.instances.clone();
    let events = std::sync::Arc::clone(&backend.events);
    let loaders = backend.launch.loaders().clone();
    answer_later(replier, move || {
        let loader = match loader {
            Some(request) => {
                let game = instances
                    .load(&id)?
                    .game_version()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::invalid_input(format!("instance {id} has no game")))?;
                if !request.kind.is_loader() {
                    return Err(Error::invalid_input("the game is not a loader"));
                }
                let chosen = loaders.pick(
                    request.kind,
                    &game,
                    request.version.as_deref(),
                    &Control::default(),
                )?;
                Some(LoaderRequest {
                    kind: request.kind,
                    version: Some(chosen.version),
                })
            }
            None => None,
        };
        let summary: InstanceSummary = instances.set_loader(&id, loader.as_ref())?;
        events.publish(&Event::Instance(InstanceEvent::Changed(summary.clone())));
        Ok(Response::Instance(summary))
    });
}

#[cfg(test)]
mod tests {
    use vl_core::instance::{InstanceCommand, NewInstance};
    use vl_core::{Command, Facade, Response};

    use crate::{Backend, BackendConfig};

    #[test]
    fn create_rename_delete_publish_events() {
        let dir = std::env::temp_dir().join(format!("vl-backend-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let events = backend.subscribe();
        let call = |c| backend.call(Command::Instances(c)).wait();
        let Ok(Response::Instance(a)) = call(InstanceCommand::Create(NewInstance {
            name: "One".into(),
            game_version: "1.21.1".into(),
            group: None,
            loader: None,
        })) else {
            panic!("create failed");
        };
        let Ok(Response::Instance(b)) = call(InstanceCommand::Rename {
            id: a.id.clone(),
            name: "Two".into(),
        }) else {
            panic!("rename failed");
        };
        assert!(matches!(
            call(InstanceCommand::Delete { id: b.id }),
            Ok(Response::Done)
        ));
        assert!(matches!(call(InstanceCommand::List), Ok(Response::Instances(i)) if i.is_empty()));
        assert_eq!(events.drain().count(), 4);
        drop(events);
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
