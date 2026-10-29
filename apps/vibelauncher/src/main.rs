//! `vibelauncher`: the desktop app. An eframe (wgpu) window around the shared egui interface.
//!
//! ```text
//! vibelauncher [--mock] [--dir <path>] [--light] [--section home|instances|catalog|account|settings]
//! vibelauncher --version
//! ```
//! The real backend (`vl-backend`) serves the interface. `--dir` selects the data folder (as does
//! the `VIBELAUNCHER_DIR` variable); otherwise the portable marker next to the executable, the
//! pointer file or the OS convention decides, the same way as for `vl-cli`. `--mock` runs on
//! synthetic data (`vl-mock`) and touches no files. When the backend cannot start (the data
//! folder is in use by another launcher or cannot be created), the window shows the reason
//! instead of the interface. `--light` starts with the light theme; `--section` opens a section.
//! `--version` prints the version and exits without opening a window.
//!
//! Started as `vibelauncher --vl-update-helper <job file>`, the process is the self-update
//! helper: it applies the update and exits without opening a window.
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
use vl_backend::{Backend, BackendConfig};
use vl_core::Facade;
use vl_mock::{MockFacade, MockOptions};
use vl_ui_egui::{Launcher, Section, StartupFailure, ThemeMode};

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

/// The command line of the app.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Options {
    /// Run on `vl-mock` instead of the real backend.
    mock: bool,
    /// The data folder (`--dir`).
    dir: Option<PathBuf>,
    /// Start with the light theme.
    light: bool,
    /// The section to open.
    section: Option<Section>,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
        };
        Self {
            mock: args.iter().any(|a| a == "--mock"),
            dir: value("--dir").map(PathBuf::from),
            light: args.iter().any(|a| a == "--light"),
            section: value("--section").and_then(|name| Section::from_name(name)),
        }
    }
}

/// Starts the facade the options ask for; the error is the reason in English.
fn start_facade(options: &Options) -> Result<Box<dyn Facade>, String> {
    if options.mock {
        return Ok(Box::new(MockFacade::new(MockOptions::default())));
    }
    let config = BackendConfig {
        data_dir: options.dir.clone(),
    };
    match Backend::new(config) {
        Ok(backend) => Ok(Box::new(backend)),
        Err(e) => Err(e.to_string()),
    }
}

/// What the window shows: the launcher, or why it could not start.
enum Shell {
    Launcher(Box<Launcher>),
    Failure(Box<StartupFailure>),
}

impl Shell {
    /// Starts the facade and builds the interface around it.
    fn new(ctx: &egui::Context, options: &Options, translation: Option<&(String, String)>) -> Self {
        let theme = if options.light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        match start_facade(options) {
            Ok(facade) => {
                let mut launcher = Launcher::new(ctx, facade);
                if let Some((lang, ftl)) = translation {
                    launcher.set_language(lang, ftl);
                }
                launcher.set_theme(ctx, theme);
                if let Some(section) = options.section {
                    launcher.set_section(section);
                }
                Shell::Launcher(Box::new(launcher))
            }
            Err(message) => {
                let mut failure = StartupFailure::new(ctx, message);
                if let Some((lang, ftl)) = translation {
                    failure.set_language(lang, ftl);
                }
                failure.set_theme(ctx, theme);
                Shell::Failure(Box::new(failure))
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        match self {
            Shell::Launcher(launcher) => launcher.ui(ui),
            Shell::Failure(failure) => failure.ui(ui),
        }
    }
}

/// The eframe application: a window around [`Shell`].
struct App {
    shell: Shell,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.ui(ui);
    }
}

fn main() -> eframe::Result {
    if let Some(code) = vl_backend::run_helper_if_requested() {
        std::process::exit(code);
    }
    // Best effort: without it Windows derives an ID from the executable path.
    #[cfg(windows)]
    let _ = winsafe::SetCurrentProcessExplicitAppUserModelID(APP_USER_MODEL_ID);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("vibelauncher {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let options = Options::parse(&args);
    let tag = std::env::var("VL_LANG")
        .ok()
        .or_else(sys_locale::get_locale)
        .unwrap_or_default();
    let translation = find_translation(&tag, &locale_dirs());
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VibeLauncher")
            .with_app_id(APP_ID)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "VibeLauncher",
        native,
        Box::new(move |cc| {
            let shell = Shell::new(&cc.egui_ctx, &options, translation.as_ref());
            Ok(Box::new(App { shell }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use vl_core::BackendInfo;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vl-app-test-{name}-{}", std::process::id()))
    }

    fn frame(ctx: &egui::Context, shell: &mut Shell) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| shell.ui(ui));
        output.textures_delta.clear();
    }

    fn launcher(shell: &mut Shell) -> &mut Launcher {
        match shell {
            Shell::Launcher(launcher) => launcher,
            Shell::Failure(failure) => panic!("the launcher did not start: {}", failure.message()),
        }
    }

    /// Draws frames until the backend answers the hello, then every section in both themes.
    fn draw_every_section(ctx: &egui::Context, shell: &mut Shell) -> BackendInfo {
        let started = Instant::now();
        while launcher(shell).backend().is_none() && started.elapsed() < Duration::from_secs(5) {
            frame(ctx, shell);
        }
        let info = launcher(shell)
            .backend()
            .cloned()
            .expect("the backend answers the hello");
        for theme in [ThemeMode::Dark, ThemeMode::Light] {
            launcher(shell).set_theme(ctx, theme);
            for section in Section::ALL {
                launcher(shell).set_section(section);
                frame(ctx, shell);
            }
        }
        info
    }

    #[test]
    fn reads_the_options() {
        let options = Options::parse(&args(&[
            "--dir",
            "data",
            "--light",
            "--section",
            "settings",
        ]));
        assert_eq!(
            options,
            Options {
                mock: false,
                dir: Some(PathBuf::from("data")),
                light: true,
                section: Some(Section::Settings),
            }
        );
        assert!(Options::parse(&args(&["--mock"])).mock);
        assert_eq!(Options::parse(&[]), Options::default());
    }

    #[test]
    fn starts_on_the_mock_when_asked() {
        let ctx = egui::Context::default();
        let options = Options::parse(&args(&["--mock", "--light"]));
        let mut shell = Shell::new(&ctx, &options, None);
        assert!(draw_every_section(&ctx, &mut shell).mock);
    }

    #[test]
    fn starts_on_the_real_backend_in_an_empty_data_folder() {
        let dir = temp_dir("empty");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        let options = Options::parse(&args(&["--dir", &dir.display().to_string()]));
        let mut shell = Shell::new(&ctx, &options, None);
        let info = draw_every_section(&ctx, &mut shell);
        assert!(!info.mock);
        assert_eq!(info.name, "vl-backend");
        assert_eq!(launcher(&mut shell).last_error(), None);
        assert!(dir.is_dir(), "the data folder is created where --dir says");
        drop(shell);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shows_the_reason_when_the_data_folder_is_in_use() {
        let dir = temp_dir("in-use");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        let options = Options::parse(&args(&["--dir", &dir.display().to_string()]));
        let first = Shell::new(&ctx, &options, None);
        assert!(matches!(first, Shell::Launcher(_)));
        let mut second = Shell::new(&ctx, &options, None);
        match &second {
            Shell::Failure(failure) => assert!(!failure.message().is_empty()),
            Shell::Launcher(_) => panic!("a second launcher on the same folder must not start"),
        }
        frame(&ctx, &mut second);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
