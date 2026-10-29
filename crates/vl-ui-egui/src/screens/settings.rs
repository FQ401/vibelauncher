//! The settings section: the launcher's behaviour around a running game. The rest of the
//! settings come later.

use egui::Ui;
use vl_core::settings::{AfterLaunch, OnGameExit};

use crate::screens::{Ctx, stub};
use crate::strings;

/// Draws the settings page.
pub fn settings(ui: &mut Ui, cx: &mut Ctx<'_>) {
    let i18n = cx.i18n;
    let current = cx.model.launcher_settings();
    let mut next = current.clone();
    let ready = cx.model.settings.is_some();
    ui.add_enabled_ui(ready, |ui| {
        ui.label(i18n.t(strings::SETTINGS_AFTER_LAUNCH));
        for (value, id) in [
            (AfterLaunch::HideToTray, strings::SETTINGS_AFTER_LAUNCH_TRAY),
            (AfterLaunch::KeepOpen, strings::SETTINGS_AFTER_LAUNCH_KEEP),
            (
                AfterLaunch::CloseLauncher,
                strings::SETTINGS_AFTER_LAUNCH_CLOSE,
            ),
        ] {
            ui.radio_value(&mut next.after_launch, value, i18n.t(id));
        }
        ui.add_space(12.0);
        ui.label(i18n.t(strings::SETTINGS_ON_EXIT));
        for (value, id) in [
            (OnGameExit::ShowWindow, strings::SETTINGS_ON_EXIT_SHOW),
            (OnGameExit::Nothing, strings::SETTINGS_ON_EXIT_NOTHING),
        ] {
            ui.radio_value(&mut next.on_game_exit, value, i18n.t(id));
        }
    });
    if next != current {
        cx.model.set_launcher_settings(next);
    }
    ui.add_space(24.0);
    stub(ui, &cx.palette, i18n, strings::STUB_SETTINGS);
}
