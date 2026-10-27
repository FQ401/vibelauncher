//! Java commands, served by `vl-java`.

use vl_core::Replier;
use vl_core::java::JavaCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: JavaCommand, replier: Replier) {
    crate::not_yet(replier, "java", &command);
}
