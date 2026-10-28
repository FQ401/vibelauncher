//! Exporting an instance as a Modrinth modpack (see [`vl_install::mrpack_export`]).
//!
//! The export hashes the instance's files and asks the catalog about them, so it is answered
//! from a worker thread.

use std::path::Path;
use std::sync::Arc;

use vl_core::catalog::Provider;
use vl_core::install::PackExportRequest;
use vl_core::instance::Loader;
use vl_core::{Error, Replier, Response};
use vl_install::mrpack_export::{ExportInput, export};
use vl_modrinth::mrpack::PackLoader;
use vl_net::Control;

use crate::Backend;
use crate::updates::spawn_reply;

/// The default version label of an exported pack.
const DEFAULT_VERSION: &str = "1.0.0";

pub(crate) fn handle(backend: &Backend, request: PackExportRequest, replier: Replier) {
    let id = request.instance.clone();
    let config = match backend.instances.load(&id) {
        Ok(c) => c,
        Err(e) => return replier.send(Err(e)),
    };
    if backend.launch.is_active(&id) {
        return replier.send(Err(Error::conflict(format!("instance {id} is running"))));
    }
    let Some(game_version) = config.game_version().map(str::to_owned) else {
        return replier.send(Err(Error::invalid_input(format!(
            "instance {id} has no game version"
        ))));
    };
    let loader = match config.loader() {
        Loader::Fabric(v) => Some((PackLoader::Fabric, v)),
        Loader::Quilt(v) => Some((PackLoader::Quilt, v)),
        Loader::NeoForge(v) => Some((PackLoader::NeoForge, v)),
        Loader::Forge(v) => Some((PackLoader::Forge, v)),
        _ => None,
    };
    let game_dir = match backend.instances.game_dir(&id) {
        Ok(d) => d,
        Err(e) => return replier.send(Err(e)),
    };
    let platforms = Arc::clone(&backend.catalog.platforms);
    spawn_reply("pack-export", replier, move || {
        let source = platforms.source(Provider::default())?;
        let name = request.name.as_deref().unwrap_or(&config.name);
        let report = export(&ExportInput {
            source,
            game_dir: &game_dir,
            out: Path::new(&request.out),
            name,
            version_id: request.version.as_deref().unwrap_or(DEFAULT_VERSION),
            summary: request.summary.as_deref(),
            game_version: &game_version,
            loader: loader.as_ref().map(|(l, v)| (*l, v.as_str())),
            include: &request.include,
            control: &Control::default(),
        })?;
        Ok(Response::PackExported(report))
    });
}
