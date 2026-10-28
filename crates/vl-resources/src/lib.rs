//! Instance resources on disk: mods, resource packs, shader packs and data packs.
//!
//! [`Resources`] works on one instance's game directory:
//!
//! - lists a folder, reading mod metadata from jars ([`jar::read_jar`]);
//! - enables and disables a file by renaming it to `<name>.disabled` and back;
//! - copies a file in (never replacing one);
//! - lists the worlds in `saves/` and opens one as [`Resources`] of its data packs;
//! - reads `pack.mcmeta` of resource packs and data packs;
//! - keeps the metadata read from jars and packs in a cache per folder, valid while a file's
//!   name, size and modification time are unchanged ([`Resources::with_cache_dir`]);
//! - removes a file by moving it into the instance's trash directory (nothing is erased).
//!
//! [`Resources::class_index`] builds the [`ModClassIndex`] that `vl-crash` uses to attribute a
//! class in a stack trace to a mod.

pub mod bisect;
mod cache;
mod index;
pub mod jar;
mod pack;
mod store;
mod worlds;

pub use bisect::Bisect;
pub use cache::Digests;
pub use index::ModClassIndex;
pub use store::{DISABLED_SUFFIX, META_DIR, Resources};
pub use worlds::SAVES_DIR;

#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bisect_tests;
