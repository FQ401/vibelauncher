//! Home: the showcase of the last active instance and a strip of other instances.

use egui::{Align, Align2, Color32, Layout, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use vl_core::instance::{InstanceState, InstanceSummary};

use super::{Action, Ctx, version_line};
use crate::strings;
use crate::theme::RADIUS_LG;
use crate::widgets::{
    card_plate, chip, empty_state, glass_button, gradient_for, gradient_rect, primary_button,
    text_at,
};

const ON_GRADIENT: Color32 = Color32::from_rgb(0xF3, 0xF0, 0xFA);
const STRIP_HEIGHT: f32 = 96.0;
const CARDS: usize = 4;

/// Draws the home page; returns a navigation request, if any.
pub fn home(ui: &mut Ui, cx: &mut Ctx<'_>) -> Option<Action> {
    let p = cx.palette;
    let i18n = cx.i18n;
    let Some(featured) = cx.model.featured().cloned() else {
        empty_state(ui, &p, &i18n.t(strings::NO_INSTANCES), "");
        return None;
    };
    let mut action = None;

    let avail = ui.available_size();
    let hero_height = (avail.y - STRIP_HEIGHT - 48.0).max(220.0);
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, hero_height), Sense::hover());
    let (from, to) = gradient_for(featured.id.as_str());
    gradient_rect(ui, rect, RADIUS_LG, from, to);
    // A soft glow: stacked translucent discs.
    let glow = ui.painter_at(rect);
    let glow_center = pos2(
        rect.right() - rect.width() * 0.25,
        rect.top() + rect.height() * 0.3,
    );
    for step in 0..14 {
        let radius = rect.height() * 0.4 * (1.0 - step as f32 / 14.0);
        glow.circle_filled(
            glow_center,
            radius,
            Color32::from_rgba_unmultiplied(139, 92, 246, 7),
        );
    }

    let inner = rect.shrink(32.0);
    let mut hero = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::bottom_up(Align::Min)),
    );
    hero.visuals_mut().override_text_color = Some(ON_GRADIENT);
    // Bottom-up: the first widget added sits lowest.
    hero.horizontal(|ui| {
        action = hero_buttons(ui, cx, &featured);
    });
    hero.add_space(8.0);
    hero.label(RichText::new(i18n.t_args(
        "home-play-time",
        &[(
            "time",
            super::duration(i18n, featured.play_time_secs).into(),
        )],
    )));
    hero.label(
        RichText::new(&featured.name)
            .size(44.0)
            .strong()
            .color(ON_GRADIENT),
    );
    hero.horizontal(|ui| {
        let plate = Color32::from_white_alpha(32);
        chip(ui, &featured.game_version, plate, ON_GRADIENT);
        chip(
            ui,
            &super::loader_label(&featured.loader),
            plate,
            ON_GRADIENT,
        );
        if matches!(featured.state, InstanceState::Running { .. }) {
            chip(ui, &i18n.t(strings::PLAYING), p.success, Color32::BLACK);
        }
    });

    ui.add_space(16.0);
    ui.label(
        RichText::new(i18n.t(strings::OTHER_INSTANCES))
            .strong()
            .color(p.text),
    );
    other_instances(ui, cx, &featured);
    action
}

fn hero_buttons(ui: &mut Ui, cx: &mut Ctx<'_>, featured: &InstanceSummary) -> Option<Action> {
    let p = cx.palette;
    let i18n = cx.i18n;
    let mut action = None;
    if let Some(problem) = &featured.problem {
        ui.label(RichText::new(i18n.t_args(
            "state-problem",
            &[("problem", problem.as_str().into())],
        )));
        return None;
    }
    match &featured.state {
        InstanceState::Stopped => {
            if primary_button(ui, &p, &i18n.t(strings::PLAY)).clicked() {
                cx.model.launch(&featured.id);
            }
        }
        InstanceState::Preparing { .. } => {
            let label = match cx.model.launch_fraction(featured) {
                Some(f) => i18n.t_args(
                    "state-preparing-percent",
                    &[("percent", format!("{:.0}", f * 100.0).into())],
                ),
                None => format!("{}…", i18n.t(strings::PREPARING)),
            };
            ui.add_enabled(false, egui::Button::new(label).min_size(vec2(180.0, 44.0)));
            if glass_button(ui, &i18n.t(strings::CANCEL)).clicked() {
                cx.model.stop(&featured.id);
            }
        }
        InstanceState::Running { .. } => {
            ui.add_enabled(
                false,
                egui::Button::new(i18n.t(strings::PLAYING)).min_size(vec2(140.0, 44.0)),
            );
            if glass_button(ui, &i18n.t(strings::STOP)).clicked() {
                cx.model.stop(&featured.id);
            }
        }
    }
    if glass_button(ui, &i18n.t(strings::CONFIGURE)).clicked() {
        cx.model.selected = Some(featured.id.clone());
        action = Some(Action::OpenInstances);
    }
    action
}

fn other_instances(ui: &mut Ui, cx: &mut Ctx<'_>, featured: &InstanceSummary) {
    let p = cx.palette;
    let mut others: Vec<InstanceSummary> = cx
        .model
        .instances
        .iter()
        .filter(|i| i.id != featured.id)
        .cloned()
        .collect();
    others.sort_by_key(|i| std::cmp::Reverse(i.last_played_unix.unwrap_or(i64::MIN)));
    others.truncate(CARDS);
    let gap = 12.0;
    let width = ((ui.available_width() - gap * (CARDS as f32 - 1.0)) / CARDS as f32).max(120.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for instance in &others {
            let (rect, response) = ui.allocate_exact_size(vec2(width, 72.0), Sense::click());
            card_plate(ui, rect, &p, response.hovered());
            let left = rect.left() + 16.0;
            text_at(
                ui,
                pos2(left, rect.top() + 14.0),
                Align2::LEFT_TOP,
                &instance.name,
                15.0,
                p.text,
            );
            text_at(
                ui,
                pos2(left, rect.top() + 38.0),
                Align2::LEFT_TOP,
                &version_line(instance),
                12.0,
                p.text_muted,
            );
            if instance.problem.is_some() {
                ui.painter().circle_filled(
                    pos2(rect.right() - 16.0, rect.top() + 20.0),
                    4.0,
                    p.danger,
                );
            } else if instance.state != InstanceState::Stopped {
                ui.painter().circle_filled(
                    pos2(rect.right() - 16.0, rect.top() + 20.0),
                    4.0,
                    p.success,
                );
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, &instance.name)
            });
            if response.clicked() {
                cx.model.selected = Some(instance.id.clone());
            }
        }
    });
}
