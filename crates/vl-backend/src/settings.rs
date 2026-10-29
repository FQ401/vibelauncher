//! Settings commands, served by `vl-settings`.

use vl_core::settings::{SettingsCommand, SettingsEvent};
use vl_core::{Error, Event, InstanceId, Replier, Response};
use vl_settings::Loaded;

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: SettingsCommand, replier: Replier) {
    replier.send(run(backend, command));
}

fn run(backend: &Backend, command: SettingsCommand) -> vl_core::Result<Response> {
    let store = backend.settings();
    match command {
        SettingsCommand::GetGlobal => {
            let Loaded { value, warnings } = store.global();
            warn(backend, None, warnings);
            Ok(Response::Settings(value))
        }
        SettingsCommand::SetGlobal(settings) => {
            store.set_global(&settings)?;
            backend.settings_changed();
            changed(backend, SettingsEvent::GlobalChanged);
            Ok(Response::Done)
        }
        SettingsCommand::GetInstance { id } => {
            let Loaded { value, warnings } = store.instance(&id);
            warn(backend, Some(id), warnings);
            Ok(Response::InstanceSettings {
                overrides: value.overrides,
                effective: value.effective,
            })
        }
        SettingsCommand::SetInstance { id, overrides } => {
            store.set_overrides(&id, &overrides)?;
            changed(backend, SettingsEvent::InstanceChanged(id));
            Ok(Response::Done)
        }
        other => Err(Error::unsupported(format!("{other:?}"))),
    }
}

fn changed(backend: &Backend, event: SettingsEvent) {
    backend.events().publish(&Event::Settings(event));
}

fn warn(backend: &Backend, instance: Option<InstanceId>, warnings: Vec<String>) {
    for message in warnings {
        changed(
            backend,
            SettingsEvent::Warning {
                instance: instance.clone(),
                message,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BackendConfig;
    use vl_core::Facade;
    use vl_core::settings::{MemorySettings, Overrides, Settings};

    fn backend(tag: &str) -> (Backend, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("vl-backend-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        (backend, dir)
    }

    fn call(b: &Backend, c: SettingsCommand) -> vl_core::Result<Response> {
        b.call(vl_core::Command::Settings(c)).wait()
    }

    #[test]
    fn settings_flow_through_the_facade_with_events() {
        let (b, dir) = backend("flow");
        let events = b.subscribe();
        let id = InstanceId::new("main").unwrap();

        let mut global = Settings::default();
        global.memory.max_mib = 3000;
        call(&b, SettingsCommand::SetGlobal(global.clone())).unwrap();
        let overrides = Overrides {
            memory: Some(MemorySettings {
                min_mib: 256,
                max_mib: 9000,
                ..Default::default()
            }),
            ..Default::default()
        };
        call(
            &b,
            SettingsCommand::SetInstance {
                id: id.clone(),
                overrides: overrides.clone(),
            },
        )
        .unwrap();

        match call(&b, SettingsCommand::GetInstance { id: id.clone() }).unwrap() {
            Response::InstanceSettings {
                overrides: o,
                effective,
            } => {
                assert_eq!(o, overrides);
                assert_eq!(effective.memory.max_mib, 9000);
            }
            other => panic!("unexpected {other:?}"),
        }
        match call(&b, SettingsCommand::GetGlobal).unwrap() {
            Response::Settings(s) => assert_eq!(s, global),
            other => panic!("unexpected {other:?}"),
        }
        let seen: Vec<Event> = events.drain().collect();
        assert!(seen.contains(&Event::Settings(SettingsEvent::GlobalChanged)));
        assert!(seen.contains(&Event::Settings(SettingsEvent::InstanceChanged(id))));

        let mut bad = Settings::default();
        bad.window.width = 1;
        let err = call(&b, SettingsCommand::SetGlobal(bad)).unwrap_err();
        assert_eq!(err.kind, vl_core::ErrorKind::InvalidInput);

        drop(b);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_does_not_stop_the_backend() {
        let (b, dir) = backend("damaged");
        let events = b.subscribe();
        let path = b.settings().global_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "][ broken").unwrap();
        match call(&b, SettingsCommand::GetGlobal).unwrap() {
            Response::Settings(s) => assert_eq!(s, Settings::default()),
            other => panic!("unexpected {other:?}"),
        }
        let warned = events.drain().into_iter().any(|e| {
            matches!(
                e,
                Event::Settings(SettingsEvent::Warning { instance: None, .. })
            )
        });
        assert!(warned);
        drop(b);
        let _ = std::fs::remove_dir_all(dir);
    }
}
