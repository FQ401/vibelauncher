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
//! [`updates`] finds the origin of files added by hand, checks for newer versions and plans
//! updates and version changes with the same planner.
//!
//! [`mrpack`] installs a Modrinth modpack (`.mrpack`) into a fresh game directory.
//!
//! [`cfpack`] installs a CurseForge modpack (a zip with `manifest.json`): its files are resolved
//! by id through the CurseForge API, and files the API cannot serve are listed for the user to
//! download by hand.
//!
//! [`mrpack_export`] writes an instance as a `.mrpack`: files the catalog knows by hash become
//! download links, everything else is carried in `overrides/`.
//!
//! [`mrpack_update`] updates an installed modpack to another version, keeping what the user
//! deleted, disabled, changed or added; [`cfpack_update`] gives it the files of a CurseForge
//! pack in the same shape.
//!
//! Plans and execution are blocking and are meant to run inside a task.

mod apply;
pub mod cfpack;
pub mod cfpack_update;
mod hashing;
pub mod mrpack;
pub mod mrpack_export;
pub mod mrpack_origins;
pub mod mrpack_update;
pub mod origin;
mod plan;
mod scan;
pub mod updates;

pub use apply::{ApplyInput, Report, execute};
pub use hashing::{FileHashes, hash_file};
pub use origin::{Origin, OriginStore};
pub use plan::{PlanInput, Target, plan, resource_kind};
pub use updates::{Selection, UpdateInput, UpdateOptions, check, identify, plan_update};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod mrpack_tests;

#[cfg(test)]
mod cfpack_tests;

#[cfg(test)]
mod cfpack_update_tests;

#[cfg(test)]
mod mrpack_update_tests;

#[cfg(test)]
mod mrpack_export_tests;
