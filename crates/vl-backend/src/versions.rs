//! Version commands, served by `vl-meta`, `vl-net`, `vl-launch` and `vl-loaders`.

use std::thread;

use vl_core::version::{LoaderVersionInfo, VersionCommand};
use vl_core::{Error, Replier, Response};
use vl_net::Control;

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: VersionCommand, replier: Replier) {
    match command {
        VersionCommand::Install { id, loader } => {
            replier.send(crate::launch::install(backend, &id, loader));
        }
        VersionCommand::LoaderVersions {
            loader,
            game_version,
        } => {
            let loaders = backend.launch.loaders().clone();
            // The list may need the network: answer from a thread, never from the caller's.
            let spawned = thread::Builder::new()
                .name("loader-versions".into())
                .spawn(move || {
                    let result = loaders
                        .versions(loader, &game_version, &Control::default())
                        .and_then(|list| {
                            if list.versions.is_empty() {
                                list.check("")?;
                            }
                            Ok(Response::LoaderVersions(
                                list.versions
                                    .into_iter()
                                    .map(|v| LoaderVersionInfo {
                                        version: v.version,
                                        stable: v.stable,
                                    })
                                    .collect(),
                            ))
                        });
                    replier.send(result);
                });
            // A thread that cannot start drops the replier; the caller sees a dropped command.
            let _: Result<_, Error> = spawned.map_err(|e| Error::internal(e.to_string()));
        }
        other => crate::not_yet(replier, "versions", &other),
    }
}
