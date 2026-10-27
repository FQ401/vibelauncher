//! Task commands, served by `vl-tasks`.

use vl_core::Replier;
use vl_core::task::TaskCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: TaskCommand, replier: Replier) {
    crate::not_yet(replier, "tasks", &command);
}
