//! Launch commands, served by `vl-launch`.

use vl_core::Replier;
use vl_core::launch::LaunchCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: LaunchCommand, replier: Replier) {
    crate::not_yet(replier, "launch", &command);
}
