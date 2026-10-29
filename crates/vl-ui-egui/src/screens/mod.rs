//! The screens of the sections.

mod home;
mod instances;
mod settings;
mod stub;
mod tasks;

pub use home::home;
pub use instances::instances;
pub use settings::settings;
pub use stub::stub;
pub use tasks::tasks_panel;

use vl_core::instance::{InstanceSummary, Loader};

use crate::i18n::I18n;
use crate::model::Model;
use crate::theme::Palette;

/// Something a screen asks the shell to do.
pub enum Action {
    /// Open the instances section.
    OpenInstances,
}

/// What every screen draws with.
pub struct Ctx<'a> {
    pub model: &'a mut Model,
    pub palette: Palette,
    pub i18n: &'a I18n,
}

/// "Fabric 0.20.1", "Vanilla".
pub fn loader_label(loader: &Loader) -> String {
    match loader {
        Loader::Vanilla => "Vanilla".to_owned(),
        Loader::Fabric(v) => format!("Fabric {v}"),
        Loader::Quilt(v) => format!("Quilt {v}"),
        Loader::NeoForge(v) => format!("NeoForge {v}"),
        Loader::Forge(v) => format!("Forge {v}"),
        _ => "Loader".to_owned(),
    }
}

/// The version line of an instance: "1.21.11 · Fabric 0.20.1".
pub fn version_line(instance: &InstanceSummary) -> String {
    format!(
        "{} · {}",
        instance.game_version,
        loader_label(&instance.loader)
    )
}

/// "45 min" or "12 h 5 min", localized.
pub fn duration(i18n: &I18n, secs: u64) -> String {
    let minutes = secs / 60;
    if minutes < 60 {
        i18n.t_args("time-minutes", &[("minutes", minutes.into())])
    } else {
        i18n.t_args(
            "time-hours-minutes",
            &[
                ("hours", (minutes / 60).into()),
                ("minutes", (minutes % 60).into()),
            ],
        )
    }
}
