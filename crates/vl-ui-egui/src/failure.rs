//! The page shown instead of the launcher when the facade could not start.

use egui::{CentralPanel, Context, Frame, Margin, Ui};

use crate::i18n::I18n;
use crate::strings;
use crate::theme::{self, Palette, ThemeMode};
use crate::widgets::empty_state;

/// A window that says the launcher could not start and why (for example, the data folder is in
/// use by another launcher or cannot be created). Create it once and call
/// [`StartupFailure::ui`] every frame.
pub struct StartupFailure {
    i18n: I18n,
    theme: ThemeMode,
    message: String,
}

impl StartupFailure {
    /// Applies the default (dark) theme to `ctx`; `message` is the technical reason in English.
    pub fn new(ctx: &Context, message: impl Into<String>) -> Self {
        let theme = ThemeMode::default();
        theme::apply(ctx, theme);
        Self {
            i18n: I18n::default(),
            theme,
            message: message.into(),
        }
    }

    /// Shows the page in `lang` using `ftl` (Fluent source); English fills any gaps.
    /// Returns whether `lang` is a valid language tag.
    pub fn set_language(&mut self, lang: &str, ftl: &str) -> bool {
        self.i18n.set_language(lang, ftl)
    }

    /// Switches the light or dark theme.
    pub fn set_theme(&mut self, ctx: &Context, theme: ThemeMode) {
        self.theme = theme;
        theme::apply(ctx, theme);
    }

    /// The reason the launcher could not start.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut Ui) {
        let p = match self.theme {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        };
        CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin::same(24)))
            .show(ui, |ui| {
                empty_state(ui, &p, &self.i18n.t(strings::STARTUP_FAILED), &self.message);
            });
    }
}
