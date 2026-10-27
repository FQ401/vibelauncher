//! Placeholder pages for sections that are not built yet.

use egui::Ui;

use crate::strings;
use crate::theme::Palette;
use crate::widgets::empty_state;

/// A page that says the section comes later.
pub fn stub(ui: &mut Ui, p: &Palette, hint: &str) {
    empty_state(ui, p, strings::SOON, hint);
}
