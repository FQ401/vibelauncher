//! Instance resources on disk: mods, resource packs, shader packs and data packs.
//!
//! [`Resources`] works on one instance's game directory:
//!
//! - lists a folder, reading mod metadata from jars ([`jar::read_jar`]);
//! - enables and disables a file by renaming it to `<name>.disabled` and back;
//! - copies a file in (never replacing one);
//! - removes a file by moving it into the instance's trash directory (nothing is erased).
//!
//! [`Resources::class_index`] builds the [`ModClassIndex`] that `vl-crash` uses to attribute a
//! class in a stack trace to a mod.

pub mod bisect;
mod index;
pub mod jar;
mod store;

pub use bisect::Bisect;
pub use index::ModClassIndex;
pub use store::{DISABLED_SUFFIX, Resources};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bisect_tests;
