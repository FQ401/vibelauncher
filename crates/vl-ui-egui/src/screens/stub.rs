//! Placeholder pages for sections that are not built yet.

use egui::Ui;

use crate::i18n::I18n;
use crate::strings;
use crate::theme::Palette;
use crate::widgets::empty_state;

/// A page that says the section comes later.
pub fn stub(ui: &mut Ui, p: &Palette, i18n: &I18n, hint: &str) {
    empty_state(ui, p, &i18n.t(strings::SOON), &i18n.t(hint));
}
