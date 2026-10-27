//! Version-list commands, served by `vl-meta` and `vl-net` with the metadata cache.

use vl_core::Replier;
use vl_core::version::VersionCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: VersionCommand, replier: Replier) {
    crate::not_yet(replier, "versions", &command);
}
