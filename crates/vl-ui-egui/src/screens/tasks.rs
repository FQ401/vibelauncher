//! The tasks panel: progress of every running task.

use egui::{ProgressBar, RichText, ScrollArea, Ui};
use vl_core::task::{TaskKind, TaskSnapshot};

use super::Ctx;
use crate::i18n::I18n;
use crate::strings;
use crate::widgets::empty_state;

fn title(i18n: &I18n, task: &TaskSnapshot) -> String {
    let key = match task.info.kind {
        TaskKind::Launch => "task-launch",
        TaskKind::InstallVersion => "task-install-version",
        TaskKind::Download => "task-download",
        TaskKind::RefreshVersions => "task-refresh-versions",
        TaskKind::SignIn => "task-sign-in",
        TaskKind::JavaScan => "task-java-scan",
        TaskKind::JavaDownload => "task-java-download",
        _ => "task-other",
    };
    let name = i18n.t(key);
    match &task.info.subject {
        Some(subject) => i18n.t_args(
            "task-with-subject",
            &[
                ("task", name.as_str().into()),
                ("subject", subject.as_str().into()),
            ],
        ),
        None => name,
    }
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
    if cx.model.tasks.is_empty() {
        empty_state(ui, &p, &i18n.t(strings::NO_TASKS), "");
        return;
    }
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
