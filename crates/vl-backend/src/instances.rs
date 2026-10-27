//! Instance commands, served by `vl-instance`.

use vl_core::Replier;
use vl_core::instance::InstanceCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: InstanceCommand, replier: Replier) {
    crate::not_yet(replier, "instances", &command);
}
