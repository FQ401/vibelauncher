//! Banners and toasts of the shell, per the layout references «Баннеры — справочник» (40228)
//! and «Тосты — справочник» (40333) of the «Первый запуск и оболочка» page.
//!
//! A banner lives at the top of a section's content while the state it reports exists; it is
//! not modal and takes no focus. A toast is a short event message: a stack of at most three in
//! the window's bottom right, expiring on its own (errors stay until closed).

use std::time::Duration;

use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Label, Layout, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::theme::Palette;

/// The badge of a banner: a neutral dot, or the dark disc of a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerIcon {
    Dot,
    Problem,
}

/// A right-side action of a banner or a toast; the primary one is dark.
#[derive(Debug, Clone)]
pub struct NoticeAction {
    /// What the caller gets back when the action is clicked.
    pub id: String,
    pub label: String,
    pub primary: bool,
}

impl NoticeAction {
    pub fn primary(id: &str, label: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            label: label.into(),
            primary: true,
        }
    }

    #[allow(dead_code)] // the crash banner on the instance page uses the light action
    pub fn secondary(id: &str, label: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            label: label.into(),
            primary: false,
        }
    }
}

/// What a banner or a toast reports back after drawing.
#[derive(Debug, Default, PartialEq)]
pub struct NoticeOutcome {
    /// The `id` of the clicked action, if any.
    pub action: Option<String>,
    /// The close sign was clicked.
    pub dismissed: bool,
}

/// A banner's content.
pub struct BannerSpec {
    pub icon: BannerIcon,
    /// 13 pt semi-bold, one line.
    pub title: String,
    /// 12 pt, one line.
    pub body: String,
    pub actions: Vec<NoticeAction>,
    /// The × on the right; banners like «Нет сети» have none.
    pub dismissible: bool,
}

/// A banner on top of the section content: 60 tall, 10 radius, a 28 badge, the two-line text
/// and up to two action buttons. Returns what was clicked.
pub fn banner(ui: &mut Ui, p: &Palette, spec: &BannerSpec) -> NoticeOutcome {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
    let stroke = if spec.icon == BannerIcon::Problem {
        Stroke::new(1.5, p.border_strong)
    } else {
        Stroke::new(1.0, p.border)
    };
    let fill = if spec.icon == BannerIcon::Problem {
        p.surface
    } else {
        p.surface_sunken
    };
    let painter = ui.painter();
    let radius = CornerRadius::same(10);
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);

    let content = rect.shrink2(vec2(16.0, 12.0));
    // The badge: a circle, with «!» on the problem one.
    let badge = Rect::from_min_size(content.min, vec2(28.0, 28.0));
    let badge_fill = match spec.icon {
        BannerIcon::Dot => p.dots,
        BannerIcon::Problem => p.accent_solid,
    };
    painter.circle_filled(badge.center(), 14.0, badge_fill);
    if spec.icon == BannerIcon::Problem {
        crate::widgets::text_at(
            ui,
            badge.center(),
            Align2::CENTER_CENTER,
            "!",
            13.0,
            p.on_accent,
        );
    }

    // The right side first, to know how much room the text keeps.
    let close_width = if spec.dismissible { 20.0 } else { 0.0 };
    let mut actions_width = 0.0;
    for (i, action) in spec.actions.iter().enumerate() {
        actions_width += measure(ui, &action.label) + 24.0 + 2.0;
        if i + 1 < spec.actions.len() {
            actions_width += 8.0;
        }
    }
    let right_width = actions_width
        + if spec.dismissible {
            8.0 + close_width
        } else {
            0.0
        };
    let right = Rect::from_min_max(
        pos2(
            (content.right() - right_width).max(content.left()),
            content.top(),
        ),
        content.max,
    );
    let mut outcome = NoticeOutcome::default();
    if spec.dismissible {
        outcome.dismissed =
            close_sign(ui, right.right(), right.center().y, 20.0, p.text_muted, "×");
    }
    let actions_right = if spec.dismissible {
        right.right() - 8.0 - 20.0
    } else {
        right.right()
    };
    if actions_right > right.left() {
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(right.left(), right.top()),
                    pos2(actions_right, right.bottom()),
                ))
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                for action in spec.actions.iter().rev() {
                    let button = action_button(ui, p, action, 29.0);
                    if button.clicked() {
                        outcome.action = Some(action.id.clone());
                    }
                }
            },
        );
    }

    // The two-line text between the badge and the actions.
    let text_left = content.min.x + 28.0 + 14.0;
    let text_rect = Rect::from_min_max(
        pos2(text_left, content.top()),
        pos2(right.left() - 12.0, content.bottom()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.add_space(1.0);
            ui.add(
                Label::new(RichText::new(&spec.title).size(13.0).strong().color(p.text)).truncate(),
            );
            ui.add_space(3.0);
            ui.add(Label::new(RichText::new(&spec.body).size(12.0).color(p.text_muted)).truncate());
        },
    );
    outcome
}

/// A banner or toast action button: 8 radius, [6, 12] padding; the primary one is dark.
fn action_button(ui: &mut Ui, p: &Palette, action: &NoticeAction, height: f32) -> egui::Response {
    let text = if action.primary {
        RichText::new(&action.label)
            .size(12.0)
            .strong()
            .color(p.on_accent)
    } else {
        RichText::new(&action.label).size(12.0).color(p.text)
    };
    let button = egui::Button::new(text)
        .fill(if action.primary {
            p.accent_solid
        } else {
            p.surface
        })
        .stroke(if action.primary {
            Stroke::NONE
        } else {
            Stroke::new(1.0, p.border)
        })
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(0.0, height));
    let response = ui
        .allocate_ui(vec2(f32::INFINITY, height), |ui| ui.add(button))
        .inner;
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &action.label));
    response
}

/// The width of `text` at 12 pt.
fn measure(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::new(12.0, FontFamily::Proportional),
            Color32::WHITE,
        )
        .size()
        .x
}

/// The close sign of a notice: an exact 20×20 hit rect at `right_edge` with a frameless glyph,
/// so its position never depends on font metrics.
fn close_sign(
    ui: &mut Ui,
    right_edge: f32,
    center_y: f32,
    glyph: f32,
    color: Color32,
    label: &str,
) -> bool {
    let rect = Rect::from_min_size(pos2(right_edge - 20.0, center_y - 10.0), vec2(20.0, 20.0));
    let response = ui.allocate_rect(rect, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "×",
        FontId::new(glyph, FontFamily::Proportional),
        color,
    );
    response.clicked()
}

/// A toast's lifetime: plain messages pass in 6 s, ones with an action in 8 s, errors stay
/// until closed.
const TOAST_SECONDS: f32 = 6.0;
const TOAST_ACTION_SECONDS: f32 = 8.0;
/// No more than this many toasts stack up; a new one closes the oldest.
const TOAST_STACK: usize = 3;
/// Toasts keep this distance from the window's right and bottom edges.
const TOAST_MARGIN: f32 = 24.0;
/// The vertical shift between stacked toasts.
const TOAST_STEP: f32 = 8.0;
/// The fixed width of a toast.
const TOAST_WIDTH: f32 = 400.0;
/// The fixed height of a toast (two lines of text).
const TOAST_HEIGHT: f32 = 79.0;

/// One toast of the stack.
#[derive(Debug, Clone)]
pub struct Toast {
    pub title: String,
    pub body: String,
    pub action: Option<NoticeAction>,
    /// Seconds left; `None` stays until closed (errors).
    remaining: Option<f32>,
}

impl Toast {
    /// A passing message.
    #[allow(dead_code)] // joined by the sections' events as they land
    pub fn message(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            action: None,
            remaining: Some(TOAST_SECONDS),
        }
    }

    /// A passing message with an action; stays a bit longer.
    pub fn action(title: impl Into<String>, body: impl Into<String>, action: NoticeAction) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            action: Some(action),
            remaining: Some(TOAST_ACTION_SECONDS),
        }
    }

    /// An error: stays until closed.
    pub fn error(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            action: None,
            remaining: None,
        }
    }
}

/// The stack of toasts, bottom right of the window, above the content and below modals.
#[derive(Debug, Default)]
pub struct Toasts {
    toasts: Vec<Toast>,
    /// Whether the pointer hovered any toast last frame (pauses the timers).
    hovered: bool,
}

impl Toasts {
    /// Adds a toast, closing the oldest beyond the stack's limit.
    pub fn push(&mut self, toast: Toast) {
        self.toasts.push(toast);
        while self.toasts.len() > TOAST_STACK {
            self.toasts.remove(0);
        }
    }

    /// How many toasts stack up now.
    #[allow(dead_code)] // the stack's size matters to the shell's tests
    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// Counts down the lifetimes; `paused` is true while the timers must stand (the pointer is
    /// over a toast or the window has no focus).
    pub fn tick(&mut self, dt: Duration, paused: bool) {
        if paused {
            return;
        }
        let secs = dt.as_secs_f32();
        for toast in &mut self.toasts {
            if let Some(remaining) = &mut toast.remaining {
                *remaining -= secs;
            }
        }
        self.toasts
            .retain(|toast| toast.remaining.is_none_or(|left| left > 0.0));
    }

    /// Draws the stack and reports clicks; the clicked or closed toast is removed. The newest
    /// toast sits at the bottom.
    pub fn show(&mut self, ui: &mut Ui, p: &Palette, close_label: &str) -> NoticeOutcome {
        let mut outcome = NoticeOutcome::default();
        if self.toasts.is_empty() {
            self.hovered = false;
            return outcome;
        }
        let screen = ui.ctx().content_rect();
        let left = screen.right() - TOAST_MARGIN - TOAST_WIDTH;
        let mut bottom = screen.bottom() - TOAST_MARGIN;
        let mut hovered = false;
        let mut clicked: Option<usize> = None;
        for (i, toast) in self.toasts.iter().enumerate() {
            bottom -= TOAST_HEIGHT;
            let rect = Rect::from_min_size(pos2(left, bottom), vec2(TOAST_WIDTH, TOAST_HEIGHT));
            hovered |= ui
                .ctx()
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pos| rect.contains(pos));
            let one = toast_ui(ui, p, rect, toast, close_label);
            if one.dismissed || one.action.is_some() {
                clicked = Some(i);
                outcome = one;
            }
            bottom -= TOAST_STEP;
        }
        if let Some(i) = clicked {
            self.toasts.remove(i);
        }
        self.hovered = hovered;
        outcome
    }

    /// Whether the pointer is over the stack (the timers stand).
    pub fn hovered(&self) -> bool {
        self.hovered
    }
}

/// Draws one toast inside `rect`: the dark card, the two-line text, the action and ×.
fn toast_ui(
    ui: &mut Ui,
    p: &Palette,
    rect: Rect,
    toast: &Toast,
    close_label: &str,
) -> NoticeOutcome {
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(12);
    // The toast's drop shadow: 10 px down, 30 px blur, black at 25 %.
    let shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(64),
    };
    painter.add(shadow.as_shape(rect, radius));
    painter.rect_filled(rect, radius, p.inverse_surface);
    let content = rect.shrink2(vec2(16.0, 14.0));
    let action_width = toast
        .action
        .as_ref()
        .map_or(0.0, |a| measure(ui, &a.label) + 24.0 + 2.0 + 12.0);
    let right_width = action_width + 20.0;
    let right = Rect::from_min_max(
        pos2(
            (content.right() - right_width).max(content.left()),
            content.top(),
        ),
        content.max,
    );
    let mut outcome = NoticeOutcome {
        dismissed: close_sign(
            ui,
            right.right(),
            right.center().y,
            18.0,
            p.on_inverse_muted,
            close_label,
        ),
        ..NoticeOutcome::default()
    };
    if let Some(action) = &toast.action {
        // The toast's action is a light button on the dark card (and vice versa).
        let text = RichText::new(&action.label)
            .size(12.0)
            .strong()
            .color(p.inverse_surface);
        let button = egui::Button::new(text)
            .fill(p.on_inverse)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(0.0, 27.0));
        let response = ui
            .scope_builder(
                UiBuilder::new()
                    .max_rect(Rect::from_min_max(
                        pos2(right.left(), right.top()),
                        pos2(right.right() - 20.0 - 8.0, right.bottom()),
                    ))
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| ui.add(button),
            )
            .inner;
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &action.label));
        if response.clicked() {
            outcome.action = Some(action.id.clone());
        }
    }
    let text_rect = Rect::from_min_max(content.min, pos2(right.left() - 8.0, content.bottom()));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.add_space(1.0);
            ui.add(
                Label::new(
                    RichText::new(&toast.title)
                        .size(13.0)
                        .strong()
                        .color(p.on_inverse),
                )
                .truncate(),
            );
            ui.add_space(3.0);
            ui.add(
                Label::new(
                    RichText::new(&toast.body)
                        .size(12.0)
                        .color(p.on_inverse_muted),
                )
                .truncate(),
            );
        },
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Context;

    const SCREEN: f32 = 800.0;
    const WIDTH: f32 = 1280.0;

    fn click_frame(ctx: &Context, events: Vec<egui::Event>, mut draw: impl FnMut(&mut Ui)) {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(WIDTH, SCREEN))),
            ..Default::default()
        };
        input.events = events;
        let mut output = ctx.run_ui(input, |ui| draw(ui));
        output.textures_delta.clear();
    }

    fn click(ctx: &Context, x: f32, y: f32, mut draw: impl FnMut(&mut Ui)) {
        let pos = pos2(x, y);
        click_frame(ctx, vec![egui::Event::PointerMoved(pos)], &mut draw);
        click_frame(
            ctx,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut draw,
        );
        click_frame(
            ctx,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut draw,
        );
    }

    /// The spec's crash banner with two actions and a close sign.
    fn crash_banner() -> BannerSpec {
        BannerSpec {
            icon: BannerIcon::Problem,
            title: "The last launch crashed".to_owned(),
            body: "A mod may be the cause.".to_owned(),
            actions: vec![
                NoticeAction::primary("what", "What happened"),
                NoticeAction::secondary("find", "Find the culprit"),
            ],
            dismissible: true,
        }
    }

    /// The width of `text` at 12 pt, exactly as the banner measures its actions.
    fn text_width(ctx: &Context, text: &str) -> f32 {
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    FontId::new(12.0, FontFamily::Proportional),
                    Color32::WHITE,
                )
                .size()
                .x
        })
    }

    #[test]
    fn the_banner_reports_its_actions_and_the_close_sign() {
        // The banner spans the width; the × keeps an exact 20×20 hit rect at the content's
        // right edge (16 px of padding).
        let mut outcome = NoticeOutcome::default();
        let ctx = Context::default();
        click(&ctx, WIDTH - 16.0 - 10.0, 30.0, |ui| {
            outcome = banner(ui, &Palette::LIGHT, &crash_banner());
        });
        assert!(outcome.dismissed, "{outcome:?}");

        // In from the right: × (20), a gap (8), «Find the culprit», a gap (8), «What happened».
        let mut outcome = NoticeOutcome::default();
        let ctx = Context::default();
        // One frame primes the font atlas so the widths below can be measured.
        click_frame(&ctx, Vec::new(), |ui| {
            banner(ui, &Palette::LIGHT, &crash_banner());
        });
        let secondary = text_width(&ctx, "Find the culprit") + 24.0 + 2.0;
        let primary = text_width(&ctx, "What happened") + 24.0 + 2.0;
        let primary_center = WIDTH - 16.0 - 28.0 - secondary - 8.0 - primary / 2.0;
        click(&ctx, primary_center, 30.0, |ui| {
            outcome = banner(ui, &Palette::LIGHT, &crash_banner());
        });
        assert_eq!(outcome.action.as_deref(), Some("what"), "{outcome:?}");
    }

    #[test]
    fn toasts_expire_hold_errors_and_cap_the_stack() {
        let mut toasts = Toasts::default();
        toasts.push(Toast::message("a", "a"));
        toasts.push(Toast::error("b", "b"));
        toasts.push(Toast::message("c", "c"));
        toasts.push(Toast::message("d", "d"));
        assert_eq!(toasts.len(), TOAST_STACK);
        toasts.tick(Duration::from_secs(7), false);
        assert_eq!(toasts.len(), 1, "the error stays");
        toasts.tick(Duration::from_secs(3600), true);
        assert_eq!(toasts.len(), 1, "hover or no focus pauses the timers");
    }

    #[test]
    fn the_toasts_draw_over_the_content_and_close_on_click() {
        let ctx = Context::default();
        let mut toasts = Toasts::default();
        toasts.push(Toast::message("Ready", "Three instances are ready"));
        toasts.push(Toast::error("The download failed", "No connection"));
        let mut outcome = NoticeOutcome::default();
        click_frame(&ctx, Vec::new(), |ui| {
            ui.label("the content below the toasts");
            outcome = toasts.show(ui, &Palette::LIGHT, "Close");
        });
        assert_eq!(outcome, NoticeOutcome::default());
        // The newest toast sits at the bottom; its × keeps an exact 20×20 hit rect at the
        // card's right content edge.
        let bottom_toast_top = SCREEN - TOAST_MARGIN - TOAST_HEIGHT;
        let toast_right = WIDTH - TOAST_MARGIN;
        let mut outcome = NoticeOutcome::default();
        click(
            &ctx,
            toast_right - 16.0 - 10.0,
            bottom_toast_top + TOAST_HEIGHT / 2.0,
            |ui| {
                outcome = toasts.show(ui, &Palette::LIGHT, "Close");
            },
        );
        assert!(outcome.dismissed, "{outcome:?}");
        assert_eq!(toasts.len(), 1, "the clicked toast is gone");
    }
}
