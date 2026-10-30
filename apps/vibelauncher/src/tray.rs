//! The tray icon shown while the launcher watches a running game without its window.
//!
//! The icon (`tray-icon`) has a menu: show the launcher, stop the game (asked in the window),
//! quit after the game. Windows: the icon lives on its own thread with a hidden window that runs
//! the message loop (`winsafe::gui`), so the main thread needs none; a left click shows the
//! launcher. Linux and the BSDs: a D-Bus StatusNotifierItem (`ksni`), no GTK; the icon is
//! missing when the desktop has no StatusNotifierWatcher; a left click shows the launcher.
//! macOS: a status item in the menu bar, whose menu opens on a click. AppKit allows it on the
//! main thread only, and its clicks arrive as events of the shared `NSApplication` (the one
//! winit made for the window); between windows nobody runs that application, so the watcher
//! hands the main thread's events to it ([`dispatch_app_events`]). While the status item is
//! shown the app has no Dock icon (the `Accessory` activation policy); the Dock icon comes back,
//! and the app comes to the front, when the item goes. Everything goes through the safe
//! bindings of `objc2` (the same ones `tray-icon` uses), so the workspace stays without `unsafe`.

use std::sync::mpsc;
#[cfg(target_os = "macos")]
use std::time::Duration;

use vl_ui_egui::TrayTexts;

/// What the user picked in the tray. Nothing builds it on macOS, which has no tray icon yet.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Show the launcher window.
    Show,
    /// Stop the running game (the window asks first).
    StopGame,
    /// Quit the launcher once no game runs.
    QuitAfterGame,
}

/// A tray icon, removed on drop.
pub struct Tray {
    actions: mpsc::Receiver<TrayAction>,
    _icon: platform::Icon,
}

impl Tray {
    /// Shows the icon; `None` when this system has no tray (the reason is printed).
    pub fn show(texts: &TrayTexts) -> Option<Tray> {
        let (tx, actions) = mpsc::channel();
        match platform::Icon::show(texts, tx) {
            Ok(icon) => Some(Tray {
                actions,
                _icon: icon,
            }),
            Err(reason) => {
                eprintln!("vibelauncher: no tray icon: {reason}");
                None
            }
        }
    }

    /// The next thing the user picked, if any.
    pub fn next_action(&self) -> Option<TrayAction> {
        self.actions.try_recv().ok()
    }
}

/// macOS: hands the main thread's AppKit events to the application for up to `timeout` (until
/// the first event, then everything already queued): the status item's clicks and its menu.
/// `false` off the main thread, where nothing is done.
#[cfg(target_os = "macos")]
pub fn dispatch_app_events(timeout: Duration) -> bool {
    use objc2::MainThreadMarker;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSApplication, NSEventMask};
    use objc2_foundation::{NSDate, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(mtm);
        // The value of `NSDefaultRunLoopMode`.
        let mode = NSString::from_str("kCFRunLoopDefaultMode");
        let mut until = NSDate::dateWithTimeIntervalSinceNow(timeout.as_secs_f64());
        while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            &mode,
            true,
        ) {
            app.sendEvent(&event);
            until = NSDate::distantPast();
        }
    });
    true
}

/// macOS: makes the app the active one, so the window that follows opens in front (picking an
/// item of the status menu does not activate the app). Elsewhere nothing is needed.
pub fn bring_to_front() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        // `activate` needs macOS 14; the app runs on 11.
        #[allow(deprecated)]
        objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// The picture of the tray icon as `(RGBA, width, height)`: the logo at 32 px in full colour; on
/// macOS the 64 px single-colour mark, a template image the menu bar tints itself.
fn icon_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    #[cfg(target_os = "macos")]
    let png = &include_bytes!("../../../assets/icons/png/mono-dark-64.png")[..];
    #[cfg(not(target_os = "macos"))]
    let png = &include_bytes!("../../../assets/icons/png/32.png")[..];
    let icon = eframe::icon_data::from_png_bytes(png).map_err(|e| e.to_string())?;
    Ok((icon.rgba, icon.width, icon.height))
}

mod platform {
    use std::sync::mpsc;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    #[cfg(not(target_os = "macos"))]
    use tray_icon::{MouseButton, MouseButtonState};
    use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
    use vl_ui_egui::TrayTexts;

    use super::TrayAction;

    const SHOW: &str = "vl-show";
    const STOP: &str = "vl-stop";
    const QUIT_AFTER: &str = "vl-quit-after-game";

    /// Builds the icon and routes its events into `tx`. Call on the thread that keeps it.
    pub(super) fn build(
        texts: &TrayTexts,
        tx: mpsc::Sender<TrayAction>,
    ) -> Result<TrayIcon, String> {
        let menu = Menu::new();
        for (id, text) in [
            (SHOW, &texts.show),
            (STOP, &texts.stop),
            (QUIT_AFTER, &texts.quit_after_game),
        ] {
            menu.append(&MenuItem::with_id(id, text, true, None))
                .map_err(|e| e.to_string())?;
        }
        // A left click shows the launcher, except on macOS, where a click opens the menu (the
        // convention of the menu bar) and the menu has the same item.
        #[cfg(not(target_os = "macos"))]
        {
            let click_tx = tx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let _ = click_tx.send(TrayAction::Show);
                }
            }));
        }
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id().as_ref() {
                SHOW => TrayAction::Show,
                STOP => TrayAction::StopGame,
                QUIT_AFTER => TrayAction::QuitAfterGame,
                _ => return,
            };
            let _ = tx.send(action);
        }));
        let (rgba, width, height) = super::icon_rgba()?;
        let icon = tray_icon::Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?;
        let builder = TrayIconBuilder::new()
            .with_id("vibelauncher")
            .with_tooltip(&texts.tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(cfg!(target_os = "macos"));
        // On macOS the icon is a template image: the menu bar tints it for light and dark.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon);
        // On macOS a title is text drawn next to the icon in the menu bar.
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_title("VibeLauncher");
        builder.build().map_err(|e| e.to_string())
    }

    /// Stops routing tray events (the handlers hold the sender of a closed tray).
    pub(super) fn forget_handlers() {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    }

    #[cfg(windows)]
    pub(super) use windows::Icon;

    #[cfg(target_os = "macos")]
    pub(super) use macos::Icon;

    #[cfg(not(any(windows, target_os = "macos")))]
    pub(super) use dbus::Icon;

    /// Windows: the icon on its own thread with a hidden window running the message loop.
    #[cfg(windows)]
    mod windows {
        use std::cell::RefCell;
        use std::rc::Rc;
        use std::sync::mpsc;
        use std::thread::JoinHandle;

        use vl_ui_egui::TrayTexts;
        use winsafe::{co, gui, prelude::*};

        use super::super::TrayAction;

        pub struct Icon {
            window: gui::WindowMain,
            thread: Option<JoinHandle<()>>,
        }

        impl Icon {
            pub fn show(texts: &TrayTexts, tx: mpsc::Sender<TrayAction>) -> Result<Self, String> {
                let texts = texts.clone();
                let (ready_tx, ready_rx) = mpsc::channel::<Result<gui::WindowMain, String>>();
                let thread = std::thread::Builder::new()
                    .name("vl-tray".into())
                    .spawn(move || {
                        let window = gui::WindowMain::new(gui::WindowMainOpts {
                            title: "VibeLauncher tray",
                            size: (1, 1),
                            style: co::WS::POPUP,
                            ..Default::default()
                        });
                        let icon = Rc::new(RefCell::new(None));
                        let slot = Rc::clone(&icon);
                        let me = window.clone();
                        window.on().wm_create(move |_| {
                            match super::build(&texts, tx.clone()) {
                                Ok(built) => {
                                    *slot.borrow_mut() = Some(built);
                                    let _ = ready_tx.send(Ok(me.clone()));
                                }
                                Err(e) => {
                                    let _ = ready_tx.send(Err(e));
                                    me.close();
                                }
                            }
                            Ok(0)
                        });
                        let _ = window.run_main(Some(co::SW::HIDE));
                        icon.borrow_mut().take();
                    })
                    .map_err(|e| e.to_string())?;
                match ready_rx.recv() {
                    Ok(Ok(window)) => Ok(Self {
                        window,
                        thread: Some(thread),
                    }),
                    Ok(Err(e)) => {
                        let _ = thread.join();
                        Err(e)
                    }
                    Err(_) => {
                        let _ = thread.join();
                        Err("the tray thread ended".into())
                    }
                }
            }
        }

        impl Drop for Icon {
            fn drop(&mut self) {
                self.window.close();
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
                super::forget_handlers();
            }
        }
    }

    /// Linux and the BSDs: a StatusNotifierItem served over D-Bus on ksni's own thread.
    #[cfg(not(any(windows, target_os = "macos")))]
    mod dbus {
        use std::sync::mpsc;

        use tray_icon::TrayIcon;
        use vl_ui_egui::TrayTexts;

        use super::super::TrayAction;

        pub struct Icon(Option<TrayIcon>);

        impl Icon {
            pub fn show(texts: &TrayTexts, tx: mpsc::Sender<TrayAction>) -> Result<Self, String> {
                super::build(texts, tx).map(|icon| Self(Some(icon)))
            }
        }

        impl Drop for Icon {
            fn drop(&mut self) {
                self.0.take();
                super::forget_handlers();
            }
        }
    }

    /// macOS: a status item in the menu bar, on the main thread; the app leaves the Dock while
    /// it is shown.
    #[cfg(target_os = "macos")]
    mod macos {
        use std::sync::mpsc;

        use objc2::MainThreadMarker;
        use objc2::rc::Retained;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        use tray_icon::TrayIcon;
        use vl_ui_egui::TrayTexts;

        use super::super::TrayAction;

        pub struct Icon {
            icon: Option<TrayIcon>,
            app: Retained<NSApplication>,
        }

        impl Icon {
            pub fn show(texts: &TrayTexts, tx: mpsc::Sender<TrayAction>) -> Result<Self, String> {
                let mtm = MainThreadMarker::new()
                    .ok_or_else(|| String::from("the status item needs the main thread"))?;
                let icon = super::build(texts, tx)?;
                let app = NSApplication::sharedApplication(mtm);
                // Only the status item while no window is open.
                app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                Ok(Self {
                    icon: Some(icon),
                    app,
                })
            }
        }

        impl Drop for Icon {
            fn drop(&mut self) {
                self.icon.take();
                super::forget_handlers();
                self.app
                    .setActivationPolicy(NSApplicationActivationPolicy::Regular);
            }
        }
    }
}
