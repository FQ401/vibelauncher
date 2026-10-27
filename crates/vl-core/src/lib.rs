//! VibeLauncher core: domain types and the facade contract.
//!
//! This crate is the bottom of the workspace. It is pure — no files, network, processes or
//! clock — and builds for `wasm32-unknown-unknown`. Every front end (the desktop UI, `vl-cli`)
//! talks to the launcher only through [`Facade`]: it sends a [`Command`], receives one
//! [`Response`] through a [`Reply`], and follows everything else on an [`EventStream`].
//!
//! Each domain module holds its data types together with its command and event enums, so work on
//! one domain stays in one file.

pub mod account;
pub mod component;
pub mod error;
pub mod facade;
pub mod ids;
pub mod instance;
pub mod java;
pub mod launch;
pub mod log;
pub mod platform;
pub mod settings;
pub mod task;
pub mod version;

pub use error::{Error, ErrorKind, Result};
pub use facade::{
    BackendInfo, Command, Event, EventBus, EventStream, Facade, Replier, Reply, Response,
    reply_channel,
};
pub use ids::{AccountId, InstanceId, TaskId};
