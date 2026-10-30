//! What the launcher asks of the app that hosts its window.
//!
//! The launcher decides from the settings (`launcher.after_launch`, `launcher.on_game_exit`) and
//! the game's life what should happen to its window; the app owns the window and the tray and
//! carries the request out. Between windows the app keeps the launcher alive without drawing it
//! ([`Launcher::watch`](crate::Launcher::watch)).

/// A change of the window the launcher asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRequest {
    /// A game is running and the launcher watches it from the tray: close the window and
    /// release its graphics, keep the launcher running.
    Hide,
    /// Bring the window back (a crash, a normal exit with "show the window", or a question to
    /// the user). Nothing to do when it is open.
    Show,
    /// End the launcher process.
    Quit,
}

/// The texts of the tray icon and its menu, in the interface language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayTexts {
    /// The icon's tooltip.
    pub tooltip: String,
    /// Show the launcher window.
    pub show: String,
    /// Stop the running game (asks first, in the window).
    pub stop: String,
    /// Quit the launcher once no game runs.
    pub quit_after_game: String,
}

/// How the window around the launcher is framed. The app chooses it when it opens the window
/// (from the platform and the setting `launcher.system_frame`) and tells the launcher, which
/// draws its title bar to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowChrome {
    /// The operating system's frame and title bar. The launcher's bar draws no window buttons
    /// and does not move the window.
    #[default]
    System,
    /// No system frame (Windows, Linux): the launcher's bar moves the window, a double click on
    /// it maximizes, it has the minimize, maximize and close buttons, and the window edges
    /// resize the window.
    Custom,
    /// macOS: a transparent title bar with the native window buttons over the launcher's bar,
    /// whose content starts to their right.
    Transparent,
}
