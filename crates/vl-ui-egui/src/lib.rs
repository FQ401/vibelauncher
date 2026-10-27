//! The shared egui user interface of the desktop app.
//!
//! The UI depends on `vl-core` only: it sends `vl_core::Command`s through a
//! `vl_core::Facade`, drains the event stream once per frame and never touches files, the network
//! or processes itself. The app that hosts it decides whether the facade is the real backend
//! (`vl-backend`) or the mock (`vl-mock`).
