//! The real facade: composes the native crates and serves commands and events.
//!
//! [`Backend`] is the only place that knows every native crate. It routes each [`Command`] to
//! its domain module, which answers through a [`Replier`] (at once, or from a task later), and
//! publishes everything else on one [`EventBus`]. Native error types are converted into
//! [`vl_core::Error`] here.
//!
//! Each domain module is owned by the work on that domain; a command a module does not handle
//! yet answers [`vl_core::ErrorKind::Unsupported`].

mod accounts;
mod catalog;
mod instances;
mod java;
mod launch;
mod pack_update;
mod resources;
mod settings;
mod tasks;
mod updates;
mod versions;

use std::path::PathBuf;
use std::sync::Arc;

use vl_settings::SettingsStore;
use vl_store::{Store, StoreOptions};

use vl_core::{
    BackendInfo, Command, Error, EventBus, EventStream, Facade, Replier, Reply, Response,
    reply_channel,
};
use vl_tasks::TaskEngine;

/// How many tasks run at the same time; more wait queued.
const MAX_PARALLEL_TASKS: usize = 4;

/// How the backend starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendConfig {
    /// A portable data directory (`--dir`); `None` uses the OS conventions.
    pub data_dir: Option<PathBuf>,
}

impl BackendConfig {
    fn store_options(&self) -> StoreOptions {
        StoreOptions {
            portable_dir: self.data_dir.clone(),
        }
    }
}

/// The real launcher behind the facade.
pub struct Backend {
    config: BackendConfig,
    store: Store,
    instances: vl_instance::Instances,
    settings: SettingsStore,
    events: Arc<EventBus>,
    java: java::JavaService,
    tasks: TaskEngine,
    accounts: accounts::Service,
    launch: launch::Service,
    catalog: catalog::Service,
}

impl Backend {
    /// Starts the backend: opens the data directory (locking it against a second launcher and
    /// migrating it to the current format).
    pub fn new(config: BackendConfig) -> vl_core::Result<Self> {
        let store = Store::open(&config.store_options())?;
        let instances = vl_instance::Instances::open(&store.data_dir().join("instances"))?;
        let settings = SettingsStore::new(store.data_dir());
        let events = Arc::new(EventBus::default());
        let tasks = TaskEngine::new(Arc::clone(&events), MAX_PARALLEL_TASKS);
        let accounts = accounts::Service::new(&store, Arc::clone(&events))?;
        let launch = launch::Service::new(&store, Arc::clone(&events))?;
        let catalog = catalog::Service::new(store.cache_dir())?;
        Ok(Self {
            java: java::JavaService::new(store.data_dir(), store.cache_dir(), Arc::clone(&events)),
            config,
            store,
            instances,
            settings,
            events,
            tasks,
            accounts,
            launch,
            catalog,
        })
    }

    /// The opened data directory.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The engine every domain module runs its long operations on.
    pub fn tasks(&self) -> &TaskEngine {
        &self.tasks
    }

    /// The settings files.
    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    /// The event bus shared by the domain modules.
    pub(crate) fn events(&self) -> &EventBus {
        &self.events
    }

    /// The configuration the backend started with.
    pub fn config(&self) -> &BackendConfig {
        &self.config
    }
}

impl Facade for Backend {
    fn call(&self, command: Command) -> Reply {
        let (replier, reply) = reply_channel();
        match command {
            Command::Hello => replier.send(Ok(Response::Hello(BackendInfo {
                name: "vl-backend".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                mock: false,
            }))),
            Command::Instances(c) => instances::handle(self, c, replier),
            Command::Versions(c) => versions::handle(self, c, replier),
            Command::Accounts(c) => accounts::handle(self, c, replier),
            Command::Java(c) => java::handle(self, c, replier),
            Command::Settings(c) => settings::handle(self, c, replier),
            Command::Launch(c) => launch::handle(self, c, replier),
            Command::Tasks(c) => tasks::handle(self, c, replier),
            Command::Resources(c) => resources::handle(self, c, replier),
            Command::Catalog(c) => catalog::handle(self, c, replier),
            other => replier.send(Err(Error::unsupported(format!("{other:?}")))),
        }
        reply
    }

    fn subscribe(&self) -> EventStream {
        self.events.subscribe()
    }
}

/// The answer of a command whose domain module does not handle it yet.
pub(crate) fn not_yet(replier: Replier, domain: &str, command: &impl std::fmt::Debug) {
    replier.send(Err(Error::unsupported(format!(
        "vl-backend does not handle {domain} {command:?} yet"
    ))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_core::instance::InstanceCommand;

    #[test]
    fn says_hello_and_lists_no_instances_in_a_fresh_directory() {
        let dir = std::env::temp_dir().join(format!("vl-backend-test-{}", std::process::id()));
        let backend = Backend::new(BackendConfig {
            data_dir: Some(dir.clone()),
        })
        .unwrap();
        let hello = backend.call(Command::Hello).wait().unwrap();
        assert!(matches!(hello, Response::Hello(info) if !info.mock));
        let list = backend
            .call(Command::Instances(InstanceCommand::List))
            .wait()
            .unwrap();
        assert!(matches!(list, Response::Instances(i) if i.is_empty()));
        drop(backend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
