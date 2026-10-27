//! `vibelauncher`: the desktop app. An eframe (wgpu) window around the shared egui interface.
//!
//! ```text
//! vibelauncher [--light] [--section home|instances|catalog|account|settings]
//! ```
//! The interface currently runs on the mock facade (`vl-mock`): synthetic instances and tasks.
//! `--light` starts with the light theme; `--section` opens a section.

use eframe::egui;
use vl_mock::{MockFacade, MockOptions};
use vl_ui_egui::{Launcher, Section, ThemeMode};

/// The eframe application: a window around [`Launcher`].
struct App {
    launcher: Launcher,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.launcher.ui(ui);
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let light = args.iter().any(|a| a == "--light");
    let section = args
        .iter()
        .position(|a| a == "--section")
        .and_then(|i| args.get(i + 1))
        .and_then(|name| Section::from_name(name));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VibeLauncher")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "VibeLauncher",
        options,
        Box::new(move |cc| {
            let mut launcher = Launcher::new(
                &cc.egui_ctx,
                Box::new(MockFacade::new(MockOptions::default())),
            );
            if light {
                launcher.set_theme(&cc.egui_ctx, ThemeMode::Light);
            }
            if let Some(section) = section {
                launcher.set_section(section);
            }
            Ok(Box::new(App { launcher }))
        }),
    )
}
