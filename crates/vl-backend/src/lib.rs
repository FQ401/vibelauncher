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
mod instances;
mod java;
mod launch;
mod settings;
mod tasks;
mod versions;

use std::path::PathBuf;

use vl_core::{
    BackendInfo, Command, Error, EventBus, EventStream, Facade, Replier, Reply, Response,
    reply_channel,
};

/// How the backend starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendConfig {
    /// A portable data directory (`--dir`); `None` uses the OS conventions.
    pub data_dir: Option<PathBuf>,
}

/// The real launcher behind the facade.
pub struct Backend {
    config: BackendConfig,
    events: EventBus,
}

impl Backend {
    /// Starts the backend.
    pub fn new(config: BackendConfig) -> vl_core::Result<Self> {
        Ok(Self {
            config,
            events: EventBus::default(),
        })
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
    use vl_core::ErrorKind;
    use vl_core::instance::InstanceCommand;

    #[test]
    fn says_hello_and_reports_unhandled_commands() {
        let backend = Backend::new(BackendConfig::default()).unwrap();
        let hello = backend.call(Command::Hello).wait().unwrap();
        assert!(matches!(hello, Response::Hello(info) if !info.mock));
        let err = backend
            .call(Command::Instances(InstanceCommand::List))
            .wait()
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unsupported);
    }
}
