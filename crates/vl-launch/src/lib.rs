//! Game preparation and launch.
//!
//! - Installation ([`install`]): a version's metadata, client jar, libraries, natives, asset index
//!   and objects are fetched through `vl-net` into the shared game files ([`layout`]), verified,
//!   and the legacy asset layouts are restored. One call, progress and cancellation through a
//!   [`Job`].
//! - The command line ([`command`]): the Java runtime for the version ([`java`]), memory and
//!   window settings, JVM and game arguments with their `${variables}` substituted, classpath and
//!   natives directory. The player's session is an explicit parameter ([`LaunchSession`]).
//! - The game process ([`process`]): start without a shell in the instance's game directory,
//!   stream its output line by line, stop it, report the exit.
//! - Checks before the start ([`memory`]): the requested heap against the memory that is free.
//!
//! [`prepare()`] ties installation, Java choice, the memory check and the command line together;
//! [`process::spawn`] starts the result. The crate never reads accounts or settings by itself:
//! the caller passes the session, the effective settings and the known Java runtimes.

pub mod command;
pub mod install;
pub mod java;
pub mod layout;
pub mod memory;
pub mod process;
pub mod replace;
pub mod session;

mod job;
mod natives;
mod prepare;

pub use command::{CommandInput, CommandLine};
pub use install::{Installed, Installer, InstallerOptions, Verify};
pub use job::{Job, ProgressSink};
pub use layout::Layout;
pub use prepare::{PrepareInput, Prepared, prepare};
pub use session::{LaunchSession, Secret};
