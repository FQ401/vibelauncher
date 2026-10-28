//! Installing catalog projects into instances.
//!
//! [`plan`] works out what an install needs: the version of the project that fits the instance's
//! game version and loader (the most stable channel allowed, then the newest), its required
//! dependencies followed recursively, what is already installed (recognized by origin record,
//! by file hash through the platform, or by mod id), and what would clash (`incompatible`
//! dependencies, a different file with the same name). Optional dependencies are only listed.
//!
//! [`execute`] downloads the planned files into a scratch directory, checking size and
//! SHA-512 (or SHA-1), then adds them to the resource folders. If any step fails the folders are
//! put back as they were. Every installed file gets an [`Origin`] record in
//! `<instance>/.vibelauncher/resources.json` (see [`origin`]).
//!
//! Plans and execution are blocking and are meant to run inside a task.

mod apply;
mod hashing;
pub mod origin;
mod plan;
mod scan;

pub use apply::{ApplyInput, Report, execute};
pub use hashing::{FileHashes, hash_file};
pub use origin::{Origin, OriginStore};
pub use plan::{PlanInput, Target, plan, resource_kind};

#[cfg(test)]
mod tests;
