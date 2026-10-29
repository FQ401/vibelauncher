//! The tray icon shown while the launcher watches a running game without its window.
//!
//! The icon (`tray-icon`) has a menu: show the launcher, stop the game (asked in the window),
//! quit after the game; a left click shows the launcher. Windows: the icon lives on its own
//! thread with a hidden window that runs the message loop (`winsafe::gui`), so the main thread
//! needs none. Linux and the BSDs: a D-Bus StatusNotifierItem (`ksni`), no GTK; the icon is
//! missing when the desktop has no StatusNotifierWatcher. macOS: no tray icon yet (its status
//! item needs the main thread's run loop, which belongs to the window between sessions); the
//! window comes back on an exit or a crash by the settings.

use std::sync::mpsc;

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

/// A 32x32 RGBA picture of the icon: a rounded square in the accent color with a "V".
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn icon_rgba() -> (Vec<u8>, u32) {
    const SIZE: u32 = 32;
    let accent = [0x7c, 0x5c, 0xff, 0xff];
    let white = [0xff, 0xff, 0xff, 0xff];
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded corners of radius 7.
            let cx = fx.clamp(7.0, SIZE as f32 - 7.0);
            let cy = fy.clamp(7.0, SIZE as f32 - 7.0);
            let inside = (fx - cx).powi(2) + (fy - cy).powi(2) <= 49.0;
            // The two strokes of a "V" from (9,8) and (23,8) to (16,25).
            let stroke = |x0: f32, y0: f32, x1: f32, y1: f32| {
                let (dx, dy) = (x1 - x0, y1 - y0);
                let t = (((fx - x0) * dx + (fy - y0) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                let (px, py) = (x0 + t * dx, y0 + t * dy);
                (fx - px).powi(2) + (fy - py).powi(2) <= 4.0
            };
            let v = stroke(9.0, 8.0, 16.0, 25.0) || stroke(23.0, 8.0, 16.0, 25.0);
            let pixel = match (inside, v) {
                (true, true) => white,
                (true, false) => accent,
                (false, _) => [0, 0, 0, 0],
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    (rgba, SIZE)
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::mpsc;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
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
        let menu_tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id().as_ref() {
                SHOW => TrayAction::Show,
                STOP => TrayAction::StopGame,
                QUIT_AFTER => TrayAction::QuitAfterGame,
                _ => return,
            };
            let _ = menu_tx.send(action);
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = tx.send(TrayAction::Show);
            }
        }));
        let (rgba, size) = super::icon_rgba();
        let icon = tray_icon::Icon::from_rgba(rgba, size, size).map_err(|e| e.to_string())?;
        TrayIconBuilder::new()
            .with_id("vibelauncher")
            .with_icon(icon)
            .with_tooltip(&texts.tooltip)
            .with_title("VibeLauncher")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())
    }

    /// Stops routing tray events (the handlers hold the sender of a closed tray).
    pub(super) fn forget_handlers() {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    }

    #[cfg(windows)]
    pub(super) use windows::Icon;

    #[cfg(not(windows))]
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
    #[cfg(not(windows))]
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
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::mpsc;

    use vl_ui_egui::TrayTexts;

    use super::TrayAction;

    pub struct Icon;

    impl Icon {
        pub fn show(_texts: &TrayTexts, _tx: mpsc::Sender<TrayAction>) -> Result<Self, String> {
            Err("not available on macOS yet".into())
        }
    }
}
