//! The shared egui user interface of the desktop app.
//!
//! The UI depends on `vl-core` only: it sends `vl_core::Command`s through a
//! `vl_core::Facade`, drains the event stream once per frame and never touches files, the network
//! or processes itself. The app that hosts it decides whether the facade is the real backend
//! (`vl-backend`) or the mock (`vl-mock`).
//!
//! The crate depends on `egui` only (no windowing), so it builds for `wasm32-unknown-unknown`
//! as well as for the desktop.

mod app;
mod failure;
mod i18n;
mod icons;
mod model;
mod nav;
mod screens;
mod strings;
mod theme;
mod widgets;
mod window;

pub use app::Launcher;
pub use failure::StartupFailure;
pub use nav::Section;
pub use theme::ThemeMode;
pub use window::{TrayTexts, WindowRequest};
