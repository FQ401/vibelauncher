//! The window shell: left rail, top bar, tasks panel and the page of the current section.

use std::time::Duration;

use egui::{
    Align, Align2, CentralPanel, Context, CornerRadius, Frame, Key, Layout, Margin, Modifiers,
    Panel, RichText, Sense, Ui, vec2,
};
use vl_core::Facade;
use vl_core::instance::InstanceState;

use crate::icons;
use crate::model::Model;
use crate::nav::Section;
use crate::screens::{self, Action, Ctx};
use crate::strings;
use crate::theme::{self, Palette, ThemeMode};
use crate::widgets::{chip, gradient_rect, text_at};

const RAIL_WIDTH: f32 = 72.0;
const TASKS_WIDTH: f32 = 360.0;

/// The launcher interface. Create it once and call [`Launcher::ui`] every frame.
pub struct Launcher {
    model: Model,
    section: Section,
    theme: ThemeMode,
    tasks_open: bool,
}

impl Launcher {
    /// Applies the default (dark) theme to `ctx` and starts talking to `facade`.
    pub fn new(ctx: &Context, facade: Box<dyn Facade>) -> Self {
        let theme = ThemeMode::default();
        theme::apply(ctx, theme);
        Self {
            model: Model::new(facade),
            section: Section::default(),
            theme,
            tasks_open: false,
        }
    }

    /// Opens a section.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
    }

    /// Switches the light or dark theme.
    pub fn set_theme(&mut self, ctx: &Context, theme: ThemeMode) {
        self.theme = theme;
        theme::apply(ctx, theme);
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.model.poll();
        self.shortcuts(&ctx);
        let delay = if self.model.busy() { 80 } else { 400 };
        ctx.request_repaint_after(Duration::from_millis(delay));

        let palette = match self.theme {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        };
        self.rail(ui, &palette);
        self.top_bar(ui, &palette);
        if self.tasks_open {
            Panel::right("tasks")
                .exact_size(TASKS_WIDTH)
                .frame(
                    Frame::new()
                        .fill(palette.surface)
                        .inner_margin(Margin::same(16)),
                )
                .show(ui, |ui| {
                    let mut cx = Ctx {
                        model: &mut self.model,
                        palette,
                    };
                    screens::tasks_panel(ui, &mut cx);
                });
        }
        let mut action = None;
        CentralPanel::default()
            .frame(Frame::new().fill(palette.bg).inner_margin(Margin::same(24)))
            .show(ui, |ui| {
                let mut cx = Ctx {
                    model: &mut self.model,
                    palette,
                };
                action = match self.section {
                    Section::Home => screens::home(ui, &mut cx),
                    Section::Instances => {
                        screens::instances(ui, &mut cx);
                        None
                    }
                    Section::Catalog => {
                        screens::stub(ui, &palette, strings::STUB_CATALOG);
                        None
                    }
                    Section::Account => {
                        screens::stub(ui, &palette, strings::STUB_ACCOUNT);
                        None
                    }
                    Section::Settings => {
                        screens::stub(ui, &palette, strings::STUB_SETTINGS);
                        None
                    }
                };
            });
        if let Some(Action::OpenInstances) = action {
            self.section = Section::Instances;
        }
    }

    /// Ctrl+1 Home, Ctrl+2 Instances, Ctrl+3 Account, Ctrl+, Settings, Ctrl+J Tasks.
    fn shortcuts(&mut self, ctx: &Context) {
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key));
        if pressed(Key::Num1) {
            self.section = Section::Home;
        }
        if pressed(Key::Num2) {
            self.section = Section::Instances;
        }
        if pressed(Key::Num3) {
            self.section = Section::Account;
        }
        if pressed(Key::Comma) {
            self.section = Section::Settings;
        }
        if pressed(Key::J) {
            self.tasks_open = !self.tasks_open;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) && self.tasks_open {
            self.tasks_open = false;
        }
    }

    fn rail(&mut self, ui: &mut Ui, p: &Palette) {
        Panel::left("rail")
            .exact_size(RAIL_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(0, 16)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    let (logo, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    gradient_rect(ui, logo, 12, p.accent, p.accent_hover);
                    text_at(
                        ui,
                        logo.center(),
                        Align2::CENTER_CENTER,
                        "V",
                        20.0,
                        p.on_accent,
                    );
                    ui.add_space(12.0);
                    for section in Section::ALL {
                        if rail_button(
                            ui,
                            p,
                            section.label(),
                            self.section == section,
                            |painter, rect, color| {
                                icons::paint_section(painter, rect, section, color);
                            },
                        ) {
                            self.section = section;
                        }
                    }
                });
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    let fraction = self
                        .model
                        .tasks
                        .iter()
                        .find_map(|t| t.progress.as_ref().and_then(|pr| pr.fraction()));
                    let active = !self.model.tasks.is_empty();
                    if rail_button(
                        ui,
                        p,
                        strings::NAV_TASKS,
                        self.tasks_open,
                        |painter, rect, color| {
                            icons::paint_ring(painter, rect.center(), fraction, color, p.border);
                            if active {
                                painter.circle_filled(
                                    rect.center() + vec2(14.0, -14.0),
                                    4.0,
                                    p.accent,
                                );
                            }
                        },
                    ) {
                        self.tasks_open = !self.tasks_open;
                    }
                    let dark = self.theme == ThemeMode::Dark;
                    let label = if dark {
                        strings::THEME_LIGHT
                    } else {
                        strings::THEME_DARK
                    };
                    if rail_button(ui, p, label, false, |painter, rect, color| {
                        icons::paint_theme(painter, rect.center(), dark, color, p.surface);
                    }) {
                        let next = self.theme.toggled();
                        self.set_theme(&ui.ctx().clone(), next);
                    }
                });
            });
    }

    fn top_bar(&self, ui: &mut Ui, p: &Palette) {
        Panel::top("top_bar")
            .exact_size(56.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(self.section.label()).heading().color(p.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.model.backend.is_none() {
                            chip(ui, strings::BACKEND_LOST, p.danger, p.surface_sunken);
                        }
                        if self.model.backend.as_ref().is_some_and(|b| b.mock) {
                            chip(ui, strings::MOCK_DATA, p.accent_subtle, p.accent);
                        }
                        let playing = self
                            .model
                            .instances
                            .iter()
                            .any(|i| matches!(i.state, InstanceState::Running { .. }));
                        if playing {
                            chip(ui, strings::PLAYING, p.success, p.surface_sunken);
                        }
                    });
                });
            });
    }
}

/// A 48x48 rail button; `paint` draws its icon. Returns whether it was clicked.
fn rail_button(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    selected: bool,
    paint: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::click());
    let fill = if selected {
        Some(p.accent_subtle)
    } else if response.hovered() {
        Some(p.surface_hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
    }
    let color = if selected { p.accent } else { p.text_muted };
    paint(ui.painter(), rect, color);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let clicked = response.clicked();
    response.on_hover_text(label);
    clicked
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    fn frame(ctx: &Context, launcher: &mut Launcher) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| launcher.ui(ui));
        output.textures_delta.clear();
    }

    #[test]
    fn every_section_draws_on_the_mock_in_both_themes() {
        let ctx = Context::default();
        let mut launcher = Launcher::new(&ctx, Box::new(MockFacade::new(MockOptions::instant())));
        let started = Instant::now();
        while launcher.model.featured().is_none() && started.elapsed() < Duration::from_secs(5) {
            frame(&ctx, &mut launcher);
        }
        assert!(
            launcher.model.featured().is_some(),
            "instances arrive from the mock"
        );
        for theme in [ThemeMode::Dark, ThemeMode::Light] {
            launcher.set_theme(&ctx, theme);
            for section in Section::ALL {
                launcher.set_section(section);
                frame(&ctx, &mut launcher);
            }
        }
    }
}
