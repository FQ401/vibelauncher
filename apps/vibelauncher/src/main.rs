//! `vibelauncher`: the desktop app. An eframe (wgpu) window around the shared egui interface.
//!
//! ```text
//! vibelauncher [--light] [--section home|instances|catalog|account|settings]
//! vibelauncher --version
//! ```
//! The interface currently runs on the mock facade (`vl-mock`): synthetic instances and tasks.
//! `--light` starts with the light theme; `--section` opens a section. `--version` prints the
//! version and exits without opening a window.
//!
//! The window's application ID (Wayland app ID, X11 class) is `me.vibefork.VibeLauncher`
//! (`APP_ID`), the name of the desktop entry and icon of the Linux packages.
//!
//! Language: English is built in. The OS locale (or `VL_LANG`) selects another language when a
//! `<tag>/vibelauncher.ftl` file exists in `VL_LOCALES_DIR` or in a `locales` directory next to
//! the executable; the tag is tried in full (`pt-BR`) and then by language (`pt`).
//!
//! Windows: release builds use the GUI subsystem (no console window), and the process takes the
//! AppUserModelID `VibeLauncher.VibeLauncher` (`APP_USER_MODEL_ID`), which taskbar grouping and
//! toast notifications use; an installer gives its shortcut the same ID.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use eframe::egui;
use vl_mock::{MockFacade, MockOptions};
use vl_ui_egui::{Launcher, Section, ThemeMode};

/// The Windows AppUserModelID of the desktop app (`Company.Product`, no spaces). Shortcuts made by
/// an installer carry the same value.
#[cfg(windows)]
const APP_USER_MODEL_ID: &str = "VibeLauncher.VibeLauncher";

/// The application ID: the Wayland app ID and X11 class of the window, which match the desktop
/// entry, the icon and the AppStream metadata of the Linux packages.
const APP_ID: &str = "me.vibefork.VibeLauncher";

/// Directories searched for extra languages.
fn locale_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("VL_LOCALES_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("locales")))
    {
        dirs.push(dir);
    }
    dirs
}

/// The translation for the user's language, if one is installed: `(tag, Fluent source)`.
fn find_translation(tag: &str, dirs: &[PathBuf]) -> Option<(String, String)> {
    let language = tag.split(['-', '_']).next().unwrap_or(tag);
    for candidate in [tag, language] {
        if candidate.is_empty() || candidate.eq_ignore_ascii_case("en") {
            continue;
        }
        for dir in dirs {
            let file = Path::new(dir).join(candidate).join("vibelauncher.ftl");
            if let Ok(text) = std::fs::read_to_string(file) {
                return Some((candidate.to_owned(), text));
            }
        }
    }
    None
}

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
    // Best effort: without it Windows derives an ID from the executable path.
    #[cfg(windows)]
    let _ = winsafe::SetCurrentProcessExplicitAppUserModelID(APP_USER_MODEL_ID);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("vibelauncher {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let light = args.iter().any(|a| a == "--light");
    let section = args
        .iter()
        .position(|a| a == "--section")
        .and_then(|i| args.get(i + 1))
        .and_then(|name| Section::from_name(name));
    let tag = std::env::var("VL_LANG")
        .ok()
        .or_else(sys_locale::get_locale)
        .unwrap_or_default();
    let translation = find_translation(&tag, &locale_dirs());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VibeLauncher")
            .with_app_id(APP_ID)
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
            if let Some((lang, ftl)) = &translation {
                launcher.set_language(lang, ftl);
            }
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
