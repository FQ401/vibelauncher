//! The launcher without a window: keeps the facade's events flowing into the launcher's state
//! while a game runs, with a tray icon to come back.
//!
//! Nothing is drawn and no graphics context exists. The thread sleeps on the event stream:
//! it wakes for an event (game output arrives as at most ten batches a second) or every
//! [`TICK`] to look at the tray. On macOS it sleeps on the main thread's AppKit events instead
//! (the status item lives there), at most [`TICK`], and then takes the launcher's events. On
//! Windows the working set is trimmed once the graphics driver has been unloaded ([`TRIM_AFTER`]), as Windows does for a minimized window: the pages the
//! window used leave memory, what watching needs comes back on use.

use std::time::{Duration, Instant};

use vl_ui_egui::{Launcher, WindowRequest};

use crate::tray::{self, Tray, TrayAction};

/// The longest sleep between two looks at the tray (macOS: at the launcher's events).
const TICK: Duration = Duration::from_millis(250);

/// When the working set is trimmed after the window closed: the Direct3D driver releases its
/// memory a few seconds after its device is gone.
const TRIM_AFTER: Duration = Duration::from_secs(10);

/// How watching ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Open the window again.
    Show,
    /// End the process.
    Quit,
}

/// Watches until the launcher asks for its window or to quit, or the user picks it in the tray.
pub fn watch(launcher: &mut Launcher) -> Outcome {
    let outcome = watch_with_tray(launcher);
    // The tray is gone here; a window follows.
    if outcome == Outcome::Show {
        tray::bring_to_front();
    }
    outcome
}

fn watch_with_tray(launcher: &mut Launcher) -> Outcome {
    let tray = Tray::show(&launcher.tray_texts());
    let since = Instant::now();
    let mut trimmed = false;
    loop {
        wait(launcher);
        while let Some(action) = tray.as_ref().and_then(Tray::next_action) {
            match action {
                TrayAction::Show => return Outcome::Show,
                TrayAction::StopGame => launcher.ask_stop_game(),
                TrayAction::QuitAfterGame => launcher.quit_after_game(),
            }
        }
        match launcher.take_window_request() {
            Some(WindowRequest::Show) => return Outcome::Show,
            Some(WindowRequest::Quit) => return Outcome::Quit,
            _ => {}
        }
        // Without a tray there is no other way back once the game is over.
        if tray.is_none() && !launcher.game_running() {
            return Outcome::Show;
        }
        if !trimmed && since.elapsed() >= TRIM_AFTER {
            trimmed = true;
            trim_working_set();
        }
    }
}

/// Waits up to [`TICK`] for the launcher's events.
#[cfg(not(target_os = "macos"))]
fn wait(launcher: &mut Launcher) {
    launcher.watch(TICK);
}

/// macOS: the status item's clicks are AppKit events of the main thread, so the thread waits for
/// those (up to [`TICK`]) and then takes the launcher's events.
#[cfg(target_os = "macos")]
fn wait(launcher: &mut Launcher) {
    if tray::dispatch_app_events(TICK) {
        launcher.watch(Duration::ZERO);
    } else {
        launcher.watch(TICK);
    }
}

/// Lets the OS take back the pages of the process that are not in use (best effort).
#[cfg(windows)]
fn trim_working_set() {
    let _ = winsafe::HPROCESS::GetCurrentProcess().EmptyWorkingSet();
}

#[cfg(not(windows))]
fn trim_working_set() {}
