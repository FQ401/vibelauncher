//! Settings commands, served by `vl-settings`.

use vl_core::Replier;
use vl_core::settings::SettingsCommand;

use crate::Backend;

pub(crate) fn handle(_backend: &Backend, command: SettingsCommand, replier: Replier) {
    crate::not_yet(replier, "settings", &command);
}
