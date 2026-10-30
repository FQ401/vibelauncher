//! The window shell: the title bar, left rail, tasks panel and the page of the current section.

use std::time::Duration;

use egui::{
    Align, Align2, CentralPanel, Context, CornerRadius, Frame, Id, Key, KeyboardShortcut, Layout,
    Margin, Modifiers, Panel, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2,
};
use vl_core::instance::InstanceState;
use vl_core::launch::GameExit;
use vl_core::settings::{AfterLaunch, OnGameExit};
use vl_core::{BackendInfo, Facade, InstanceId};

use crate::i18n::I18n;
use crate::icons;
use crate::model::{GameSignal, Model};
use crate::nav::Section;
use crate::screens::{self, Action, Ctx};
use crate::strings;
use crate::theme::{self, Palette, ThemeMode};
use crate::title_bar::{self, BAR_HEIGHT, ButtonColors, ButtonLabels, MAC_BUTTONS_INSET};
use crate::widgets::{chip, gradient_rect, text_at};
use crate::window::{TrayTexts, WindowChrome, WindowRequest};

const RAIL_WIDTH: f32 = 72.0;
const TASKS_WIDTH: f32 = 360.0;
/// The widest the search field of the title bar grows.
const SEARCH_MAX_WIDTH: f32 = 400.0;
/// Ctrl+K (Cmd+K on macOS) focuses the search field.
const SEARCH_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);

/// The id of the search field of the title bar.
fn search_id() -> Id {
    Id::unique("vl-title-search")
}

/// The launcher interface. Create it once and call [`Launcher::ui`] every frame.
pub struct Launcher {
    model: Model,
    i18n: I18n,
    section: Section,
    theme: ThemeMode,
    tasks_open: bool,
    /// What the window should do, for the app to take.
    request: Option<WindowRequest>,
    /// Quit once no game runs (asked from the tray).
    quit_after_game: bool,
    /// The "stop the game?" question is open.
    confirm_stop: bool,
    /// How the window is framed.
    chrome: WindowChrome,
    /// The text of the search field.
    search: String,
}

impl Launcher {
    /// Applies the default (dark) theme to `ctx` and starts talking to `facade`.
    pub fn new(ctx: &Context, facade: Box<dyn Facade>) -> Self {
        let theme = ThemeMode::default();
        theme::apply(ctx, theme);
        Self {
            model: Model::new(facade),
            i18n: I18n::default(),
            section: Section::default(),
            theme,
            tasks_open: false,
            request: None,
            quit_after_game: false,
            confirm_stop: false,
            chrome: WindowChrome::default(),
            search: String::new(),
        }
    }

    /// Tells the launcher how its window is framed; the title bar draws to match.
    pub fn set_window_chrome(&mut self, chrome: WindowChrome) {
        self.chrome = chrome;
    }

    /// How the launcher believes its window is framed.
    pub fn window_chrome(&self) -> WindowChrome {
        self.chrome
    }

    /// Whether the user wants the operating system's window frame (`launcher.system_frame`);
    /// `None` until the settings are read.
    pub fn wants_system_frame(&self) -> Option<bool> {
        self.model
            .settings
            .as_ref()
            .map(|s| s.launcher.system_frame)
    }

    /// Launches an instance (by its id) with the default account. Returns `false` for an
    /// invalid id; a failure to launch shows as the error chip.
    pub fn launch(&mut self, instance: &str) -> bool {
        match InstanceId::new(instance) {
            Some(id) => {
                self.model.launch(&id);
                true
            }
            None => false,
        }
    }

    /// Moves the interface to a new egui context (a new window): applies the theme to it.
    pub fn attach(&mut self, ctx: &Context) {
        theme::apply(ctx, self.theme);
    }

    /// Keeps the launcher working without a window: waits up to `timeout` for the next event,
    /// then folds in everything that is ready and decides what the window should do. A game's
    /// exit or crash is noticed here while the window is closed.
    pub fn watch(&mut self, timeout: Duration) {
        self.model.wait(timeout);
        self.handle_signals();
    }

    /// The change of the window the launcher asks for, once.
    pub fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.request.take()
    }

    /// Whether a game is preparing or running.
    pub fn game_running(&self) -> bool {
        self.model.game_active()
    }

    /// Whether closing the window should keep the launcher watching from the tray: a game runs
    /// and the setting hides the launcher to the tray after a launch.
    pub fn watches_from_tray(&self) -> bool {
        self.game_running()
            && self.model.launcher_settings().after_launch == AfterLaunch::HideToTray
    }

    /// Asks the user, in the window, whether to stop the running game, and asks for the window.
    pub fn ask_stop_game(&mut self) {
        if self.game_running() {
            self.confirm_stop = true;
            self.request(WindowRequest::Show);
        }
    }

    /// Quits the launcher once no game runs; at once when none does.
    pub fn quit_after_game(&mut self) {
        self.quit_after_game = true;
        if !self.game_running() {
            self.request(WindowRequest::Quit);
        }
    }

    /// The texts of the tray icon and its menu.
    pub fn tray_texts(&self) -> TrayTexts {
        TrayTexts {
            tooltip: self.i18n.t(strings::TRAY_TOOLTIP),
            show: self.i18n.t(strings::TRAY_SHOW),
            stop: self.i18n.t(strings::TRAY_STOP),
            quit_after_game: self.i18n.t(strings::TRAY_QUIT_AFTER),
        }
    }

    /// Records a request; hiding never replaces a pending show or quit.
    fn request(&mut self, request: WindowRequest) {
        if request == WindowRequest::Hide && self.request.is_some() {
            return;
        }
        self.request = Some(request);
    }

    /// Turns game starts and exits into window requests by the settings.
    fn handle_signals(&mut self) {
        for signal in std::mem::take(&mut self.model.signals) {
            let settings = self.model.launcher_settings();
            match signal {
                GameSignal::Started(_) => match settings.after_launch {
                    AfterLaunch::HideToTray => self.request(WindowRequest::Hide),
                    AfterLaunch::CloseLauncher => self.request(WindowRequest::Quit),
                    _ => {}
                },
                GameSignal::Exited(instance, exit) => {
                    if exit.crashed && !exit.stopped_by_user {
                        self.model.last_error = Some(self.crash_message(&instance, &exit));
                        self.model.selected = Some(instance);
                        self.section = Section::Home;
                        self.request(WindowRequest::Show);
                    } else if self.quit_after_game && !self.game_running() {
                        self.request(WindowRequest::Quit);
                    } else if settings.on_game_exit == OnGameExit::ShowWindow {
                        self.request(WindowRequest::Show);
                    }
                }
            }
        }
    }

    /// The crash in English technical form, for the error chip.
    fn crash_message(&self, instance: &InstanceId, exit: &GameExit) -> String {
        let name = self
            .model
            .instances
            .iter()
            .find(|i| &i.id == instance)
            .map_or_else(|| instance.to_string(), |i| i.name.clone());
        let mut text = format!("The game of {name} crashed");
        if let Some(code) = exit.code {
            text.push_str(&format!(" (exit code {code})"));
        }
        text.push('.');
        for path in [&exit.crash_report, &exit.hs_err, &exit.log_file]
            .into_iter()
            .flatten()
        {
            text.push_str(&format!("\n{}", path.display()));
        }
        text
    }

    /// The "stop the game?" question.
    fn stop_question(&mut self, ui: &mut Ui) {
        if !self.confirm_stop {
            return;
        }
        if !self.game_running() {
            self.confirm_stop = false;
            return;
        }
        let i18n = &self.i18n;
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("stop-game")).show(ui.ctx(), |ui| {
            ui.label(i18n.t(strings::STOP_CONFIRM));
            ui.horizontal(|ui| {
                if ui.button(i18n.t(strings::STOP_CONFIRM_YES)).clicked() {
                    answer = Some(true);
                }
                if ui.button(i18n.t(strings::CANCEL)).clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        if let Some(stop) = answer {
            self.confirm_stop = false;
            if stop {
                self.model.stop_all();
            }
        }
    }

    /// Shows the interface in `lang` using `ftl` (Fluent source); English fills any gaps.
    /// Returns whether `lang` is a valid language tag.
    pub fn set_language(&mut self, lang: &str, ftl: &str) -> bool {
        self.i18n.set_language(lang, ftl)
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

    /// What answered the hello: the backend's name and version, and whether it is the mock.
    /// `None` until the reply arrives, or when the facade does not answer.
    pub fn backend(&self) -> Option<&BackendInfo> {
        self.model.backend.as_ref()
    }

    /// The latest failed command or task, in English technical form.
    pub fn last_error(&self) -> Option<&str> {
        self.model.last_error.as_deref()
    }

    /// Draws one frame.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.model.poll();
        self.handle_signals();
        self.shortcuts(&ctx);
        let delay = if self.model.busy() { 80 } else { 400 };
        ctx.request_repaint_after(Duration::from_millis(delay));

        let palette = match self.theme {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
        };
        self.title_bar(ui, &palette);
        self.rail(ui, &palette);
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
                        i18n: &self.i18n,
                        chrome: self.chrome,
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
                    i18n: &self.i18n,
                    chrome: self.chrome,
                };
                action = match self.section {
                    Section::Home => screens::home(ui, &mut cx),
                    Section::Instances => {
                        screens::instances(ui, &mut cx);
                        None
                    }
                    Section::Catalog => {
                        screens::stub(ui, &palette, &self.i18n, strings::STUB_CATALOG);
                        None
                    }
                    Section::Account => {
                        screens::stub(ui, &palette, &self.i18n, strings::STUB_ACCOUNT);
                        None
                    }
                    Section::Settings => {
                        screens::settings(ui, &mut cx);
                        None
                    }
                };
            });
        if let Some(Action::OpenInstances) = action {
            self.section = Section::Instances;
        }
        self.stop_question(ui);
        if self.chrome == WindowChrome::Custom {
            title_bar::handle_resize(&ctx, title_bar::frame_of(&ctx, self.chrome));
        }
    }

    /// Ctrl+1 Home, Ctrl+2 Instances, Ctrl+3 Account, Ctrl+, Settings, Ctrl+J Tasks, Ctrl+K
    /// search.
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
        if ctx.input_mut(|i| i.consume_shortcut(&SEARCH_SHORTCUT)) {
            ctx.memory_mut(|m| m.request_focus(search_id()));
        }
        if self.tasks_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.tasks_open = false;
        }
    }

    fn rail(&mut self, ui: &mut Ui, p: &Palette) {
        let i18n = &self.i18n;
        let mut theme_change = None;
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
                    for section in Section::ALL {
                        if rail_button(
                            ui,
                            p,
                            &i18n.t(section.label_id()),
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
                    let dark = self.theme == ThemeMode::Dark;
                    let label = if dark {
                        i18n.t(strings::THEME_LIGHT)
                    } else {
                        i18n.t(strings::THEME_DARK)
                    };
                    if rail_button(ui, p, &label, false, |painter, rect, color| {
                        icons::paint_theme(painter, rect.center(), dark, color, p.surface);
                    }) {
                        theme_change = Some(self.theme.toggled());
                    }
                });
            });
        if let Some(theme) = theme_change {
            self.set_theme(&ui.ctx().clone(), theme);
        }
    }

    /// The title bar: the logo, the section, the search field, the status chips, the tasks
    /// indicator and, without the system frame, the window buttons. Its empty parts move the
    /// window unless the system frame does.
    fn title_bar(&mut self, ui: &mut Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let chrome = self.chrome;
        let frame = title_bar::frame_of(&ctx, chrome);
        let mut dismiss = false;
        let mut toggle_tasks = false;
        Panel::top("title_bar")
            .exact_size(BAR_HEIGHT)
            .resizable(false)
            .frame(Frame::new().fill(p.bg))
            .show(ui, |ui| {
                let bar = ui.max_rect();
                let background = ui.interact(
                    bar,
                    ui.scope_id().with("background"),
                    Sense::click_and_drag(),
                );
                if chrome != WindowChrome::System {
                    title_bar::handle_bar(&ctx, &background, frame);
                }
                // The rail's column goes on up through the bar.
                let column = Rect::from_min_size(bar.min, vec2(RAIL_WIDTH, bar.height()));
                ui.painter()
                    .rect_filled(column, CornerRadius::ZERO, p.surface);
                let logo_slot = if chrome == WindowChrome::Transparent {
                    Rect::from_min_size(
                        pos2(bar.left() + MAC_BUTTONS_INSET, bar.top()),
                        vec2(48.0, bar.height()),
                    )
                } else {
                    column
                };
                let logo = Rect::from_center_size(logo_slot.center(), vec2(40.0, 40.0));
                gradient_rect(ui, logo, 12, p.accent, p.accent_hover);
                text_at(
                    ui,
                    logo.center(),
                    Align2::CENTER_CENTER,
                    "V",
                    20.0,
                    p.on_accent,
                );

                if chrome == WindowChrome::Custom {
                    let colors = ButtonColors {
                        glyph: p.text,
                        hover: p.surface_hover,
                        pressed: p.border,
                    };
                    let labels = ButtonLabels {
                        minimize: self.i18n.t(strings::WINDOW_MINIMIZE),
                        maximize: self.i18n.t(strings::WINDOW_MAXIMIZE),
                        restore: self.i18n.t(strings::WINDOW_RESTORE),
                        close: self.i18n.t(strings::WINDOW_CLOSE),
                    };
                    title_bar::window_buttons(ui, frame, &colors, &labels);
                }

                let middle = Rect::from_min_max(
                    pos2(logo_slot.right() + 24.0, bar.top()),
                    pos2(
                        bar.right() - title_bar::buttons_width(chrome) - 12.0,
                        bar.bottom(),
                    ),
                );
                let right = ui
                    .scope_builder(
                        UiBuilder::new()
                            .max_rect(middle)
                            .layout(Layout::right_to_left(Align::Center)),
                        |ui| {
                            toggle_tasks = self.tasks_indicator(ui, p);
                            dismiss = self.status_chips(ui, p);
                        },
                    )
                    .response
                    .rect;
                let left = Rect::from_min_max(
                    middle.min,
                    pos2((right.left() - 16.0).max(middle.left()), middle.bottom()),
                );
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(left)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.label(
                            RichText::new(self.i18n.t(self.section.label_id()))
                                .heading()
                                .color(p.text),
                        );
                        ui.add_space(16.0);
                        self.search_field(ui, &ctx);
                    },
                );
            });
        if dismiss {
            self.model.last_error = None;
        }
        if toggle_tasks {
            self.tasks_open = !self.tasks_open;
        }
    }

    /// The search field (Ctrl+K). It narrows with the window.
    fn search_field(&mut self, ui: &mut Ui, ctx: &Context) {
        let width = ui.available_width().min(SEARCH_MAX_WIDTH);
        if width < 48.0 {
            return;
        }
        let hint = format!(
            "{}  {}",
            self.i18n.t(strings::SEARCH),
            ctx.format_shortcut(&SEARCH_SHORTCUT)
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(search_id())
                .hint_text(hint)
                .desired_width(width)
                .margin(Margin::symmetric(12, 8)),
        );
    }

    /// The tasks (downloads) indicator: a ring of the first task's progress. Returns whether it
    /// was clicked.
    fn tasks_indicator(&self, ui: &mut Ui, p: &Palette) -> bool {
        let fraction = self
            .model
            .tasks
            .iter()
            .find_map(|t| t.progress.as_ref().and_then(|pr| pr.fraction()));
        let active = !self.model.tasks.is_empty();
        rail_button(
            ui,
            p,
            &self.i18n.t(strings::NAV_TASKS),
            self.tasks_open,
            |painter, rect, color| {
                icons::paint_ring(painter, rect.center(), fraction, color, p.border);
                if active {
                    painter.circle_filled(rect.center() + vec2(14.0, -14.0), 4.0, p.accent);
                }
            },
        )
    }

    /// The chips of the bar, right to left: playing, test data, core lost, the latest error.
    /// Returns whether the error chip was clicked.
    fn status_chips(&self, ui: &mut Ui, p: &Palette) -> bool {
        let i18n = &self.i18n;
        let playing = self
            .model
            .instances
            .iter()
            .any(|i| matches!(i.state, InstanceState::Running { .. }));
        if playing {
            chip(ui, &i18n.t(strings::PLAYING), p.success, p.surface_sunken);
        }
        if self.model.backend.as_ref().is_some_and(|b| b.mock) {
            chip(ui, &i18n.t(strings::MOCK_DATA), p.accent_subtle, p.accent);
        }
        if self.model.backend.is_none() {
            chip(
                ui,
                &i18n.t(strings::BACKEND_LOST),
                p.danger,
                p.surface_sunken,
            );
        }
        let mut dismiss = false;
        if let Some(error) = &self.model.last_error {
            dismiss = error_chip(ui, p, &i18n.t(strings::LAST_ERROR))
                .on_hover_text(format!("{error}\n\n{}", i18n.t(strings::DISMISS_HINT)))
                .clicked();
        }
        dismiss
    }
}

/// A clickable chip in the danger color: the latest failed command.
fn error_chip(ui: &mut Ui, p: &Palette, label: &str) -> egui::Response {
    let text = RichText::new(label).small().color(p.surface_sunken);
    ui.add(
        egui::Button::new(text)
            .fill(p.danger)
            .stroke(egui::Stroke::NONE)
            .corner_radius(CornerRadius::same(12)),
    )
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

    const WAIT: Duration = Duration::from_secs(10);

    fn mock(options: MockOptions) -> Launcher {
        let mut launcher = Launcher::new(&Context::default(), Box::new(MockFacade::new(options)));
        let started = Instant::now();
        while (launcher.model.settings.is_none() || launcher.model.instances.is_empty())
            && started.elapsed() < WAIT
        {
            launcher.watch(Duration::from_millis(20));
        }
        assert!(launcher.model.settings.is_some(), "settings arrive");
        launcher
    }

    /// Watches without a window until the launcher asks for something.
    fn next_request(launcher: &mut Launcher) -> Option<WindowRequest> {
        let started = Instant::now();
        while started.elapsed() < WAIT {
            launcher.watch(Duration::from_millis(20));
            if let Some(request) = launcher.take_window_request() {
                return Some(request);
            }
        }
        None
    }

    fn launch(launcher: &mut Launcher) -> InstanceId {
        let id = InstanceId::new("survival").expect("valid id");
        launcher.model.launch(&id);
        id
    }

    fn running_game() -> MockOptions {
        MockOptions {
            exit_after_log: false,
            ..MockOptions::instant()
        }
    }

    #[test]
    fn a_started_game_hides_the_window_and_a_normal_exit_brings_it_back() {
        let mut launcher = mock(running_game());
        launch(&mut launcher);
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Hide));
        assert!(launcher.game_running() && launcher.watches_from_tray());
        launcher.model.stop_all();
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Show));
        assert!(!launcher.game_running());
        assert_eq!(launcher.last_error(), None);
    }

    #[test]
    fn a_crash_brings_the_window_back_with_the_error_on_home() {
        let mut launcher = mock(MockOptions {
            game_crashes: true,
            ..MockOptions::instant()
        });
        launcher.set_section(Section::Settings);
        let id = launch(&mut launcher);
        // The start and the crash can arrive in one batch of events: the show then replaces the
        // hide, so the hide is optional and the show is what counts.
        let mut request = next_request(&mut launcher);
        if request == Some(WindowRequest::Hide) {
            request = next_request(&mut launcher);
        }
        assert_eq!(request, Some(WindowRequest::Show));
        let error = launcher.last_error().expect("the crash is shown");
        assert!(
            error.contains("crashed") && error.contains("exit code 1"),
            "{error}"
        );
        assert_eq!(launcher.section, Section::Home);
        assert_eq!(launcher.model.selected, Some(id));
    }

    #[test]
    fn the_settings_keep_the_window_or_close_the_launcher() {
        let mut launcher = mock(MockOptions::instant());
        let mut settings = launcher.model.launcher_settings();
        settings.after_launch = AfterLaunch::KeepOpen;
        settings.on_game_exit = OnGameExit::Nothing;
        launcher.model.set_launcher_settings(settings.clone());
        launch(&mut launcher);
        let started = Instant::now();
        let mut ran = false;
        while !(ran && !launcher.game_running()) && started.elapsed() < WAIT {
            launcher.watch(Duration::from_millis(20));
            ran |= launcher.game_running();
            assert_eq!(launcher.take_window_request(), None, "nothing to do");
        }
        assert!(ran && !launcher.game_running());

        settings.after_launch = AfterLaunch::CloseLauncher;
        launcher.model.set_launcher_settings(settings);
        launch(&mut launcher);
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Quit));
    }

    #[test]
    fn quit_after_the_game_waits_for_the_exit() {
        let mut launcher = mock(running_game());
        launcher.quit_after_game();
        assert_eq!(launcher.take_window_request(), Some(WindowRequest::Quit));
        launcher.quit_after_game = false;

        launch(&mut launcher);
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Hide));
        launcher.quit_after_game();
        assert_eq!(launcher.take_window_request(), None, "the game still runs");
        launcher.model.stop_all();
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Quit));
    }

    #[test]
    fn stopping_from_the_tray_asks_in_the_window() {
        let ctx = Context::default();
        let mut launcher = mock(running_game());
        launch(&mut launcher);
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Hide));
        launcher.ask_stop_game();
        assert_eq!(launcher.take_window_request(), Some(WindowRequest::Show));
        launcher.attach(&ctx);
        frame(&ctx, &mut launcher);
        assert!(launcher.confirm_stop && launcher.game_running());
        launcher.confirm_stop = false;
        launcher.model.stop_all();
        assert_eq!(next_request(&mut launcher), Some(WindowRequest::Show));
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

    /// A window of 1280x800 points around the launcher, driven by synthetic input.
    struct Window {
        ctx: Context,
        launcher: Launcher,
        time: f64,
    }

    impl Window {
        fn new(chrome: WindowChrome) -> Self {
            let ctx = Context::default();
            let mut launcher =
                Launcher::new(&ctx, Box::new(MockFacade::new(MockOptions::instant())));
            launcher.set_window_chrome(chrome);
            let mut window = Self {
                ctx,
                launcher,
                time: 0.0,
            };
            window.frame(Vec::new());
            window
        }

        /// Draws one frame with `events` and returns the viewport commands it sent.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<egui::ViewportCommand> {
            self.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let launcher = &mut self.launcher;
            let mut output = self.ctx.run_ui(input, |ui| launcher.ui(ui));
            output.textures_delta.clear();
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .map(|v| v.commands)
                .unwrap_or_default()
        }

        fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }
        }

        /// Clicks at `pos` and returns the commands of the press and the release frames.
        fn click(&mut self, x: f32, y: f32) -> Vec<egui::ViewportCommand> {
            let pos = pos2(x, y);
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            let mut commands = self.frame(vec![Self::button(pos, true)]);
            commands.extend(self.frame(vec![Self::button(pos, false)]));
            commands
        }
    }

    #[test]
    fn the_window_buttons_minimize_maximize_and_close() {
        use egui::ViewportCommand::{Close, Maximized, Minimized};
        let mut window = Window::new(WindowChrome::Custom);
        let close = 1280.0 - title_bar::BUTTON_WIDTH / 2.0;
        let maximize = close - title_bar::BUTTON_WIDTH;
        let minimize = maximize - title_bar::BUTTON_WIDTH;
        assert!(window.click(close, 30.0).contains(&Close));
        assert!(window.click(maximize, 30.0).contains(&Maximized(true)));
        assert!(window.click(minimize, 30.0).contains(&Minimized(true)));
        // The same place with the system frame is part of the bar.
        let mut window = Window::new(WindowChrome::System);
        assert!(!window.click(close, 30.0).contains(&Close));
        let mut window = Window::new(WindowChrome::Transparent);
        assert!(!window.click(close, 30.0).contains(&Close));
    }

    #[test]
    fn the_bar_moves_the_window_and_a_double_click_maximizes_it() {
        use egui::ViewportCommand::{Maximized, StartDrag};
        for chrome in [WindowChrome::Custom, WindowChrome::Transparent] {
            let mut window = Window::new(chrome);
            let start = pos2(36.0, 30.0);
            window.frame(vec![egui::Event::PointerMoved(start)]);
            let mut commands = window.frame(vec![Window::button(start, true)]);
            commands.extend(window.frame(vec![egui::Event::PointerMoved(start + vec2(30.0, 4.0))]));
            commands.extend(window.frame(vec![Window::button(start + vec2(30.0, 4.0), false)]));
            assert!(commands.contains(&StartDrag), "{chrome:?}: {commands:?}");

            let mut commands = window.click(36.0, 30.0);
            commands.extend(window.click(36.0, 30.0));
            assert!(
                commands.contains(&Maximized(true)),
                "{chrome:?}: {commands:?}"
            );
        }
        let mut window = Window::new(WindowChrome::System);
        let mut commands = window.click(36.0, 30.0);
        commands.extend(window.click(36.0, 30.0));
        assert!(
            commands.is_empty(),
            "the system frame moves the window: {commands:?}"
        );
    }

    #[test]
    fn the_edges_resize_a_window_without_the_system_frame() {
        use egui::ViewportCommand::BeginResize;
        use egui::viewport::ResizeDirection;
        let mut window = Window::new(WindowChrome::Custom);
        assert!(
            window
                .click(640.0, 1.0)
                .contains(&BeginResize(ResizeDirection::North))
        );
        assert!(
            window
                .click(1279.0, 799.0)
                .contains(&BeginResize(ResizeDirection::SouthEast))
        );
        assert!(
            window
                .click(0.0, 400.0)
                .contains(&BeginResize(ResizeDirection::West))
        );
        let mut window = Window::new(WindowChrome::System);
        assert!(window.click(0.0, 400.0).is_empty());
    }

    #[test]
    fn ctrl_k_focuses_the_search_field() {
        let mut window = Window::new(WindowChrome::Custom);
        assert_ne!(window.ctx.memory(|m| m.focused()), Some(search_id()));
        window.frame(vec![egui::Event::Key {
            key: Key::K,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::COMMAND,
        }]);
        window.frame(Vec::new());
        assert_eq!(window.ctx.memory(|m| m.focused()), Some(search_id()));
    }
}
