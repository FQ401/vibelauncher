//! Instance commands, served by `vl-instance`.
//!
//! Delete moves the instance into the launcher's own trash inside the instances directory (see
//! `vl_instance::Instances::delete`); no command erases data.

use vl_core::instance::{InstanceCommand, InstanceEvent};
use vl_core::{Event, Replier, Response};

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: InstanceCommand, replier: Replier) {
    let service = &backend.instances;
    let result = match command {
        InstanceCommand::List => service.list().map(Response::Instances),
        InstanceCommand::Create(new) => service.create(&new).map(|i| {
            backend
                .events
                .publish(&Event::Instance(InstanceEvent::Added(i.clone())));
            Response::Instance(i)
        }),
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
