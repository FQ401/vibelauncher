//! Catalog commands, served by `vl-platforms`.
//!
//! Every command goes to the network, so each is answered from a worker thread, never from the
//! caller's. Answers come from the HTTP cache while young and when offline.

use std::path::Path;
use std::sync::Arc;
use std::thread;

use vl_core::catalog::CatalogCommand;
use vl_core::{Error, Replier, Response, Result};
use vl_net::{Client, Control, NetConfig};
use vl_platforms::Platforms;

use crate::Backend;

/// The platforms behind the catalog commands.
pub(crate) struct Service {
    platforms: Arc<Platforms>,
}

impl Service {
    /// Builds the network client with its HTTP cache under `cache_dir`.
    pub(crate) fn new(cache_dir: &Path) -> Result<Self> {
        let config = NetConfig {
            cache_dir: Some(cache_dir.join("http")),
            ..NetConfig::new(env!("CARGO_PKG_VERSION"))
        };
        let client = Client::new(config)?;
        Ok(Self {
            platforms: Arc::new(Platforms::new(client)),
        })
    }
}

pub(crate) fn handle(backend: &Backend, command: CatalogCommand, replier: Replier) {
    let platforms = Arc::clone(&backend.catalog.platforms);
    let spawned = thread::Builder::new()
        .name("catalog".into())
        .spawn(move || replier.send(run(&platforms, command)));
    // A thread that cannot start drops the replier; the caller sees a dropped command.
    let _: std::result::Result<_, Error> = spawned.map_err(|e| Error::internal(e.to_string()));
}

fn run(platforms: &Platforms, command: CatalogCommand) -> Result<Response> {
    let control = Control::default();
    match command {
        CatalogCommand::Search(query) => {
            let source = platforms.source(query.provider)?;
            Ok(Response::CatalogSearch(source.search(&query, &control)?))
        }
        CatalogCommand::Project { id } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogProject(source.project(&id, &control)?))
        }
        CatalogCommand::Versions { project, filter } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogVersions(
                source.versions(&project, &filter, &control)?,
            ))
        }
        CatalogCommand::VersionsByHashes { algorithm, hashes } => {
            let source = platforms.source(Default::default())?;
            Ok(Response::CatalogHashMatches(
                source.versions_by_hashes(algorithm, &hashes, &control)?,
            ))
        }
        other => Err(Error::unsupported(format!("{other:?}"))),
    }
}
