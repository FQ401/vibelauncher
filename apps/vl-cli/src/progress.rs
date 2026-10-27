//! Progress lines shared by every `vl-cli` command that follows a task.

use std::io::{self, Write};

use vl_core::TaskId;
use vl_core::task::{Progress, ProgressUnit, TaskEvent, TaskOutcome};

/// Prints a task's progress as lines and reports how it ended.
///
/// Feed it every [`TaskEvent`]; it ignores other tasks and prints a line only when the text of
/// the progress changes.
pub(crate) struct ProgressPrinter {
    task: TaskId,
    label: String,
    last: Option<String>,
}

impl ProgressPrinter {
    /// Follows `task`, prefixing its lines with `[label]`.
    pub(crate) fn new(task: TaskId, label: &str) -> Self {
        Self {
            task,
            label: label.to_owned(),
            last: None,
        }
    }

    /// Handles one event; returns the outcome once the followed task finished.
    pub(crate) fn handle(
        &mut self,
        event: &TaskEvent,
        out: &mut dyn Write,
    ) -> io::Result<Option<TaskOutcome>> {
        match event {
            TaskEvent::Progress { id, progress } if *id == self.task => {
                let line = format_progress(progress);
                if self.last.as_deref() != Some(line.as_str()) {
                    writeln!(out, "[{}] {line}", self.label)?;
                    self.last = Some(line);
                }
                Ok(None)
            }
            TaskEvent::Finished { id, outcome } if *id == self.task => Ok(Some(outcome.clone())),
            _ => Ok(None),
        }
    }
}

/// One progress as text: `stage 42%`, `stage 12/30`, `stage 1.5 KiB / 3.0 KiB (50%)`.
pub(crate) fn format_progress(progress: &Progress) -> String {
    let stage = progress.stage.as_deref().unwrap_or_default();
    let amount = match (progress.unit, progress.total) {
        (ProgressUnit::Steps, Some(total)) if total > 0 => {
            format!("{}%", progress.done.min(total) * 100 / total)
        }
        (ProgressUnit::Bytes, Some(total)) if total > 0 => format!(
            "{} / {} ({}%)",
            human_bytes(progress.done),
            human_bytes(total),
            progress.done.min(total) * 100 / total
        ),
        (ProgressUnit::Bytes, _) => human_bytes(progress.done),
        (_, Some(total)) => format!("{}/{total}", progress.done),
        (_, None) => progress.done.to_string(),
    };
    if stage.is_empty() {
        amount
    } else {
        format!("{stage} {amount}")
    }
}

/// `1536` as `1.5 KiB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)] // display only
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(
        done: u64,
        total: Option<u64>,
        unit: ProgressUnit,
        stage: Option<&str>,
    ) -> Progress {
        Progress {
            done,
            total,
            unit,
            stage: stage.map(str::to_owned),
        }
    }

    #[test]
    fn formats_each_unit() {
        let steps = progress(250, Some(1000), ProgressUnit::Steps, Some("libraries"));
        assert_eq!(format_progress(&steps), "libraries 25%");
        let items = progress(3, Some(12), ProgressUnit::Items, None);
        assert_eq!(format_progress(&items), "3/12");
        let bytes = progress(1536, Some(3072), ProgressUnit::Bytes, Some("client.jar"));
        assert_eq!(
            format_progress(&bytes),
            "client.jar 1.5 KiB / 3.0 KiB (50%)"
        );
        let unknown = progress(2048, None, ProgressUnit::Bytes, None);
        assert_eq!(format_progress(&unknown), "2.0 KiB");
    }

    #[test]
    fn prints_changes_of_the_followed_task_only_and_returns_the_outcome() {
        let mut printer = ProgressPrinter::new(TaskId(1), "download");
        let mut out = Vec::new();
        let step = |id, done| TaskEvent::Progress {
            id: TaskId(id),
            progress: progress(done, Some(100), ProgressUnit::Steps, None),
        };
        for event in [step(1, 10), step(1, 10), step(2, 50), step(1, 20)] {
            assert_eq!(printer.handle(&event, &mut out).unwrap(), None);
        }
        let end = TaskEvent::Finished {
            id: TaskId(1),
            outcome: TaskOutcome::Succeeded,
        };
        assert_eq!(
            printer.handle(&end, &mut out).unwrap(),
            Some(TaskOutcome::Succeeded)
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[download] 10%\n[download] 20%\n"
        );
    }
}
