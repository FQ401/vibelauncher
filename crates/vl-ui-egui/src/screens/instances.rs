//! The instances section: every instance in a table.

use egui::{Grid, RichText, ScrollArea, Ui};
use vl_core::instance::{InstanceState, InstanceSummary};

use super::{Ctx, loader_label};
use crate::strings;
use crate::widgets::empty_state;

/// Draws the instance table.
pub fn instances(ui: &mut Ui, cx: &mut Ctx<'_>) {
    let p = cx.palette;
    let i18n = cx.i18n;
    if cx.model.instances.is_empty() {
        empty_state(ui, &p, &i18n.t(strings::NO_INSTANCES), "");
        return;
    }
    let rows: Vec<InstanceSummary> = cx.model.instances.clone();
    let selected = cx.model.featured().map(|i| i.id.clone());
    let mut pick = None;
    let mut launch = None;
    let mut stop = None;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Grid::new("instances")
                .num_columns(6)
                .striped(true)
                .spacing([28.0, 12.0])
                .min_col_width(80.0)
                .show(ui, |ui| {
                    for head in [
                        i18n.t(strings::COL_NAME),
                        i18n.t(strings::COL_VERSION),
                        i18n.t(strings::COL_LOADER),
                        i18n.t(strings::COL_PLAYED),
                        String::new(),
                        String::new(),
                    ] {
                        ui.label(RichText::new(head).small().color(p.text_muted));
                    }
                    ui.end_row();
                    for row in &rows {
                        let is_selected = selected.as_ref() == Some(&row.id);
                        if ui.selectable_label(is_selected, &row.name).clicked() {
                            pick = Some(row.id.clone());
                        }
                        ui.label(&row.game_version);
                        ui.label(loader_label(&row.loader));
                        ui.label(super::duration(i18n, row.play_time_secs));
                        match (&row.problem, &row.state) {
                            (Some(problem), _) => {
                                ui.colored_label(
                                    p.danger,
                                    i18n.t_args(
                                        "state-problem",
                                        &[("problem", problem.as_str().into())],
                                    ),
                                );
                            }
                            (None, InstanceState::Stopped) => {
                                ui.label("");
                            }
                            (None, InstanceState::Preparing { .. }) => {
                                ui.label(i18n.t(strings::PREPARING));
                            }
                            (None, InstanceState::Running { .. }) => {
                                ui.colored_label(p.success, i18n.t(strings::PLAYING));
                            }
                        }
                        ui.horizontal(|ui| match (&row.problem, &row.state) {
                            (Some(_), _) => {}
                            (None, InstanceState::Stopped) => {
                                if ui.button(i18n.t(strings::PLAY)).clicked() {
                                    launch = Some(row.id.clone());
                                }
                            }
                            (None, _) => {
                                if ui.button(i18n.t(strings::STOP)).clicked() {
                                    stop = Some(row.id.clone());
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    if let Some(id) = pick {
        cx.model.selected = Some(id);
    }
    if let Some(id) = launch {
        cx.model.launch(&id);
    }
    if let Some(id) = stop {
        cx.model.stop(&id);
    }
}
