//! Version commands, served by `vl-meta`, `vl-net` and `vl-launch`.

use vl_core::Replier;
use vl_core::version::VersionCommand;

use crate::Backend;

pub(crate) fn handle(backend: &Backend, command: VersionCommand, replier: Replier) {
    match command {
        VersionCommand::Install { id } => replier.send(crate::launch::install(backend, &id)),
        other => crate::not_yet(replier, "versions", &other),
    }
}
