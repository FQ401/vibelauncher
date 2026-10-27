//! The tasks panel: progress of every running task.

use egui::{ProgressBar, RichText, ScrollArea, Ui};
use vl_core::task::{TaskKind, TaskSnapshot};

use super::Ctx;
use crate::strings;
use crate::widgets::empty_state;

fn kind_label(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Launch => "Запуск игры",
        TaskKind::InstallVersion => "Установка версии",
        TaskKind::Download => "Загрузка файлов",
        TaskKind::RefreshVersions => "Обновление списка версий",
        TaskKind::SignIn => "Вход в аккаунт",
        TaskKind::JavaScan => "Поиск Java",
        TaskKind::JavaDownload => "Загрузка Java",
        _ => "Задача",
    }
}

fn title(task: &TaskSnapshot) -> String {
    match &task.info.subject {
        Some(subject) => format!("{}: {subject}", kind_label(task.info.kind)),
        None => kind_label(task.info.kind).to_owned(),
    }
}

/// Draws the panel content.
pub fn tasks_panel(ui: &mut Ui, cx: &mut Ctx<'_>) {
    let p = cx.palette;
    ui.label(RichText::new(strings::NAV_TASKS).heading().color(p.text));
    ui.add_space(8.0);
    if cx.model.tasks.is_empty() {
        empty_state(ui, &p, strings::NO_TASKS, "");
        return;
    }
    let mut cancel = None;
    ScrollArea::vertical().show(ui, |ui| {
        for task in &cx.model.tasks {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title(task)).color(p.text));
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
                if task.info.cancellable && ui.button(strings::CANCEL).clicked() {
                    cancel = Some(task.info.id);
                }
            });
        }
    });
    if let Some(id) = cancel {
        cx.model.cancel_task(id);
    }
}
