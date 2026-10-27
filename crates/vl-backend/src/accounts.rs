//! Account commands, served by `vl-auth`.

use vl_core::Replier;
use vl_core::account::AccountCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: AccountCommand, replier: Replier) {
    crate::not_yet(replier, "accounts", &command);
}
