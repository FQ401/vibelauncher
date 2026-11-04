//! The tasks panel: the «Нужно действие» block of failed tasks, then the running ones.

use egui::{Id, ProgressBar, RichText, ScrollArea, Ui};
use vl_core::task::{TaskKind, TaskSnapshot};

use super::Ctx;
use crate::i18n::I18n;
use crate::strings;
use crate::widgets::empty_state;

/// The localized name of a task from its kind and subject (VL-A-2: «Ошибка фоновой задачи»).
pub(crate) fn task_title(i18n: &I18n, kind: Option<TaskKind>, subject: Option<&str>) -> String {
    let key = match kind {
        Some(TaskKind::Launch) => "task-launch",
        Some(TaskKind::InstallVersion) => "task-install-version",
        Some(TaskKind::Download) => "task-download",
        Some(TaskKind::RefreshVersions) => "task-refresh-versions",
        Some(TaskKind::SignIn) => "task-sign-in",
        Some(TaskKind::JavaScan) => "task-java-scan",
        Some(TaskKind::JavaDownload) => "task-java-download",
        Some(TaskKind::InstallResource) => "task-install-resource",
        Some(TaskKind::InstallPack) => "task-install-pack",
        Some(TaskKind::UpdatePack) => "task-update-pack",
        _ => "task-other",
    };
    let name = i18n.t(key);
    match subject {
        Some(subject) => i18n.t_args(
            "task-with-subject",
            &[("task", name.as_str().into()), ("subject", subject.into())],
        ),
        None => name,
    }
}

fn title(i18n: &I18n, task: &TaskSnapshot) -> String {
    task_title(i18n, Some(task.info.kind), task.info.subject.as_deref())
}

/// Draws the panel content.
pub fn tasks_panel(ui: &mut Ui, cx: &mut Ctx<'_>) {
    let p = cx.palette;
    let i18n = cx.i18n;
    ui.label(
        RichText::new(i18n.t(strings::NAV_TASKS))
            .heading()
            .color(p.text),
    );
    ui.add_space(8.0);
    let mut dismiss = None;
    if !cx.model.failures.is_empty() {
        ui.label(
            RichText::new(i18n.t(strings::NEEDS_ACTION))
                .size(13.0)
                .strong()
                .color(p.text),
        );
        ui.add_space(4.0);
        for failure in &cx.model.failures {
            let card = egui::Frame::new()
                .fill(p.surface_sunken)
                .stroke(egui::Stroke::new(1.0, p.border))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(12, 8));
            card.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(task_title(i18n, failure.kind, failure.subject.as_deref()))
                            .color(p.text),
                    );
                    // «Подробнее» is N10's «Подробности»: the technical details, on the spot.
                    let expanded = ui
                        .data_mut(|d| d.get_temp::<bool>(Id::unique(("failure", failure.id.0))))
                        .unwrap_or(false);
                    if ui
                        .selectable_label(expanded, i18n.t(strings::DETAILS))
                        .clicked()
                    {
                        ui.data_mut(|d| {
                            d.insert_temp(Id::unique(("failure", failure.id.0)), !expanded)
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            dismiss = Some(failure.id);
                        }
                    });
                });
                if ui
                    .data_mut(|d| d.get_temp::<bool>(Id::unique(("failure", failure.id.0))))
                    .unwrap_or(false)
                {
                    ui.label(RichText::new(&failure.error).small().color(p.text_muted));
                }
            });
            ui.add_space(4.0);
        }
        ui.add_space(4.0);
    }
    if cx.model.tasks.is_empty() {
        if cx.model.failures.is_empty() {
            empty_state(ui, &p, &i18n.t(strings::NO_TASKS), "");
        }
    } else {
        let mut cancel = None;
        ScrollArea::vertical().show(ui, |ui| {
            for task in &cx.model.tasks {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title(i18n, task)).color(p.text));
                    let progress = task.progress.as_ref();
                    let mut bar = match progress.and_then(|pr| pr.fraction()) {
                        Some(f) => ProgressBar::new(f).show_percentage(),
                        None => ProgressBar::new(0.0).animate(true),
                    };
                    bar = bar.fill(p.accent_solid);
                    ui.add(bar);
                    if let Some(stage) = progress.and_then(|pr| pr.stage.as_deref()) {
                        ui.label(RichText::new(stage).small().color(p.text_muted));
                    }
                    if task.info.cancellable && ui.button(i18n.t(strings::CANCEL)).clicked() {
                        cancel = Some(task.info.id);
                    }
                });
            }
        });
        if let Some(id) = cancel {
            cx.model.cancel_task(id);
        }
    }
    if let Some(id) = dismiss {
        cx.model.dismiss_failure(id);
    }
}
