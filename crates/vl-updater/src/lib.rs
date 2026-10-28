//! Self-update on the machine, around the pure rules of `vl-update`.
//!
//! - [`current_install`]: the install method of the running launcher (executable path, the
//!   `APPIMAGE` variable, the install marker next to the executable).
//! - [`Updater::check`]: fetches `latest.json` and `latest.json.minisig` past the HTTP cache with
//!   size caps, verifies the signature with the embedded release key, refuses a rollback below
//!   the running version or the newest manifest seen before ([`UpdateDir`] remembers it), and
//!   returns the [`vl_update::Offer`]. [`Updater::release`] fails closed when the build has no
//!   release key.
//! - [`Updater::stage`]: downloads the release file through `vl-net` into
//!   `<updates>/staging/<version>/` with its size and SHA-256 checked, progress and cancellation
//!   from a [`vl_net::Control`].
//! - [`apply`]: carries out a [`vl_update::UpdatePlan`] — never while a game runs
//!   ([`GameGuard`]), and only after the file's size and SHA-256 are checked again. Portable
//!   folders ([`replace_portable`]) and AppImages ([`replace_appimage`]) are replaced; the MSI
//!   and macOS disk image replacements answer [`UpdaterError::NotImplemented`].
//! - [`UpdateDir::mark_updated`] / [`UpdateDir::take_just_updated`]: the "just updated" marker
//!   for the first start of the new version.

mod apply;
mod check;
mod detect;
mod error;
mod store;

pub use apply::{
    GameGuard, MAX_ARCHIVE_ENTRIES, MAX_UNPACKED_BYTES, NEW_DIR, OLD_DIR, apply, clean_portable,
    replace_appimage, replace_portable, verify_file,
};
pub use check::{SIGNATURE_SUFFIX, Updater, file_name};
pub use detect::{current_exe, current_install, inputs_for};
pub use error::UpdaterError;
pub use store::{JustUpdated, UpdateDir};

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tests;
