//! `vibelauncher`: the desktop app. An eframe (wgpu) window around the shared egui interface.
//!
//! ```text
//! vibelauncher [--mock [--mock-game run|exit|crash]] [--dir <path>] [--light]
//!              [--section home|instances|catalog|account|settings] [--launch <instance id>]
//! vibelauncher --version
//! ```
//! The real backend (`vl-backend`) serves the interface. `--dir` selects the data folder (as does
//! the `VIBELAUNCHER_DIR` variable); otherwise the portable marker next to the executable, the
//! pointer file or the OS convention decides, the same way as for `vl-cli`. `--mock` runs on
//! synthetic data (`vl-mock`) and touches no files. When the backend cannot start (the data
//! folder is in use by another launcher or cannot be created), the window shows the reason
//! instead of the interface. `--light` starts with the light theme; `--section` opens a section.
//! `--launch` starts an instance with the default account right away. `--mock-game` makes a
//! simulated game run until stopped (`run`, the default), exit by itself (`exit`) or crash
//! (`crash`). `--version` prints the version and exits without opening a window.
//!
//! **Watching a game.** The window is an eframe session; the process outlives it. Once a game
//! runs and the setting `launcher.after_launch` is `hide_to_tray` (the default), the window closes
//! and its wgpu device, surface and textures are released (eframe returns from `run_native`);
//! the launcher keeps working without drawing ([`watcher`]): game output still goes to the log
//! file and the console buffer, play time is counted, an exit or a crash is noticed, and a tray
//! icon offers to show the launcher, stop the game or quit after it. A crash opens a new window
//! with the error; a normal exit does so when `launcher.on_game_exit` is `show_window`. Closing
//! the window while a game runs in this mode keeps watching from the tray as well.
//! `keep_open` keeps the window; `close_launcher` ends the launcher once the game runs.
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

mod tray;
mod watcher;

use std::path::{Path, PathBuf};

use eframe::egui;
use vl_backend::{Backend, BackendConfig};
use vl_core::Facade;
use vl_mock::{MockFacade, MockOptions};
use vl_ui_egui::{Launcher, Section, StartupFailure, ThemeMode, WindowRequest};

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

/// How a game simulated by `--mock` ends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum MockGame {
    /// It runs until stopped.
    #[default]
    Run,
    /// It exits by itself after its log.
    Exit,
    /// It crashes after its log.
    Crash,
}

/// The command line of the app.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Options {
    /// Run on `vl-mock` instead of the real backend.
    mock: bool,
    /// How a simulated game ends.
    mock_game: MockGame,
    /// The instance to launch at start.
    launch: Option<String>,
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
            mock_game: match value("--mock-game").map(String::as_str) {
                Some("exit") => MockGame::Exit,
                Some("crash") => MockGame::Crash,
                _ => MockGame::Run,
            },
            launch: value("--launch").cloned(),
            dir: value("--dir").map(PathBuf::from),
            light: args.iter().any(|a| a == "--light"),
            section: value("--section").and_then(|name| Section::from_name(name)),
        }
    }
}

/// Starts the facade the options ask for; the error is the reason in English.
fn start_facade(options: &Options) -> Result<Box<dyn Facade>, String> {
    if options.mock {
        let mock = MockOptions {
            exit_after_log: options.mock_game != MockGame::Run,
            game_crashes: options.mock_game == MockGame::Crash,
            ..MockOptions::default()
        };
        return Ok(Box::new(MockFacade::new(mock)));
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
                if let Some(instance) = &options.launch {
                    launcher.launch(instance);
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

    /// Moves the interface to the context of a new window.
    fn attach(&mut self, ctx: &egui::Context) {
        match self {
            Shell::Launcher(launcher) => launcher.attach(ctx),
            // A failed start never gets a second window.
            Shell::Failure(_) => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        match self {
            Shell::Launcher(launcher) => launcher.ui(ui),
            Shell::Failure(failure) => failure.ui(ui),
        }
    }
}

/// What follows a closed window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterWindow {
    /// End the process.
    Quit,
    /// Watch the running game without a window.
    Watch,
}

/// Where the window was, so the next one opens there.
#[derive(Debug, Clone, Copy, Default)]
struct Geometry {
    /// The outer top-left corner, in points.
    position: Option<egui::Pos2>,
    /// The inner size, in points.
    size: Option<egui::Vec2>,
    maximized: bool,
}

/// One window session (an eframe run) around the shell that outlives it.
struct App<'a> {
    shell: &'a mut Shell,
    after: &'a mut AfterWindow,
    geometry: &'a mut Geometry,
    /// The window is closing because the launcher asked for it.
    closing: bool,
}

impl eframe::App for App<'_> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.ui(ui);
        let ctx = ui.ctx().clone();
        ctx.input(|i| {
            let viewport = i.viewport();
            if let Some(maximized) = viewport.maximized {
                self.geometry.maximized = maximized;
            }
            if !self.geometry.maximized {
                if let Some(outer) = viewport.outer_rect {
                    self.geometry.position = Some(outer.min);
                }
                if let Some(inner) = viewport.inner_rect {
                    self.geometry.size = Some(inner.size());
                }
            }
        });
        let Shell::Launcher(launcher) = &mut *self.shell else {
            return;
        };
        if !self.closing && ctx.input(|i| i.viewport().close_requested()) {
            // The user closed the window: keep watching a running game from the tray when the
            // settings say so, otherwise quit.
            *self.after = if launcher.watches_from_tray() {
                AfterWindow::Watch
            } else {
                AfterWindow::Quit
            };
        }
        let after = match launcher.take_window_request() {
            Some(WindowRequest::Hide) => AfterWindow::Watch,
            Some(WindowRequest::Quit) => AfterWindow::Quit,
            _ => return,
        };
        *self.after = after;
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The window's options; it opens where the last one was.
///
/// `direct3d_only` limits wgpu to Direct3D 12 (Windows). With every backend allowed, wgpu also
/// opens a Vulkan instance, whose driver stays loaded (and keeps tens of MiB) after the window
/// and its device are gone; the Direct3D driver is unloaded with the device.
fn native_options(geometry: &Geometry, direct3d_only: bool) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("VibeLauncher")
        .with_app_id(APP_ID)
        .with_inner_size(geometry.size.unwrap_or(egui::vec2(1280.0, 800.0)))
        .with_min_inner_size([960.0, 600.0])
        .with_maximized(geometry.maximized);
    if let Some(position) = geometry.position {
        viewport = viewport.with_position(position);
    }
    let mut native = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    if direct3d_only
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut native.wgpu_options.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    native
}

/// The commit and date a release build was made from (`VL_BUILD_COMMIT` and `VL_BUILD_DATE` at
/// compile time); `None` in a build without a commit.
fn build_info() -> Option<String> {
    let commit = option_env!("VL_BUILD_COMMIT").filter(|c| !c.is_empty())?;
    Some(
        match option_env!("VL_BUILD_DATE").filter(|d| !d.is_empty()) {
            Some(date) => format!("commit {commit}, built {date}"),
            None => format!("commit {commit}"),
        },
    )
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
        if let Some(build) = build_info() {
            println!("{build}");
        }
        return Ok(());
    }
    let options = Options::parse(&args);
    let tag = std::env::var("VL_LANG")
        .ok()
        .or_else(sys_locale::get_locale)
        .unwrap_or_default();
    let translation = find_translation(&tag, &locale_dirs());
    // The shell (and the facade in it) lives for the whole process; each window is one eframe
    // run around it. eframe keeps its winit event loop between runs.
    let mut shell: Option<Shell> = None;
    let mut geometry = Geometry::default();
    // Direct3D 12 only on Windows unless `WGPU_BACKEND` picks; every backend when it fails.
    let mut direct3d_only = cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none();
    loop {
        let mut after = AfterWindow::Quit;
        let session = eframe::run_native(
            "VibeLauncher",
            native_options(&geometry, direct3d_only),
            Box::new(|cc| {
                let fresh = shell.is_none();
                let shell = shell.get_or_insert_with(|| {
                    Shell::new(&cc.egui_ctx, &options, translation.as_ref())
                });
                if !fresh {
                    shell.attach(&cc.egui_ctx);
                }
                Ok(Box::new(App {
                    shell,
                    after: &mut after,
                    geometry: &mut geometry,
                    closing: false,
                }))
            }),
        );
        if let Err(e) = session {
            if direct3d_only {
                direct3d_only = false;
                continue;
            }
            return Err(e);
        }
        let (AfterWindow::Watch, Some(Shell::Launcher(launcher))) = (after, &mut shell) else {
            break;
        };
        if watcher::watch(launcher) == watcher::Outcome::Quit {
            break;
        }
    }
    Ok(())
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
            "--launch",
            "survival",
        ]));
        assert_eq!(
            options,
            Options {
                mock: false,
                mock_game: MockGame::Run,
                launch: Some("survival".into()),
                dir: Some(PathBuf::from("data")),
                light: true,
                section: Some(Section::Settings),
            }
        );
        assert!(Options::parse(&args(&["--mock"])).mock);
        assert_eq!(
            Options::parse(&args(&["--mock", "--mock-game", "crash"])).mock_game,
            MockGame::Crash
        );
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
