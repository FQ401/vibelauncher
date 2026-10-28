//! Self-update rules as pure functions: the signed release manifest in, a decision and a
//! replacement plan out.
//!
//! Nothing here touches the network or the disk. The native layer (`vl-updater`) fetches the
//! manifest and its signature, downloads and replaces files; this crate decides what is trusted
//! and what to do:
//!
//! - [`key`]: the release public key embedded in the binary ([`RELEASE_PUBLIC_KEY`]) and minisign
//!   signature checks. Without a release key every check fails closed with
//!   [`UpdateError::NoReleaseKey`].
//! - [`manifest`]: `latest.json` — version, date, minimum version, release page and one artifact
//!   per platform and install method with its `https` URL, SHA-256 and size. The signature is
//!   checked over the exact bytes before they are parsed; sizes, counts and fields are capped.
//! - [`Version`]: `MAJOR.MINOR.PATCH[-pre]` with Semantic Versioning order.
//! - [`method`]: how the running launcher was installed (MSI, portable, macOS app, AppImage, deb,
//!   rpm) and which artifact kind replaces it.
//! - [`offer`]: the decision for one manifest — up to date, an update with its artifact, or a
//!   newer version to install by hand — with rollback refused: a manifest older than the running
//!   version or than the newest manifest seen before is an error.
//! - [`plan`]: the replacement as data (run the new MSI, unpack the portable archive, copy the app
//!   from the disk image, rename the AppImage over the old one, or open the release page) and the
//!   program to start afterwards.

mod error;
pub mod key;
pub mod manifest;
pub mod method;
pub mod offer;
pub mod plan;
mod version;

pub use error::UpdateError;
pub use key::{RELEASE_PUBLIC_KEY, UpdateKey};
pub use manifest::{Artifact, ArtifactKind, Channel, Manifest, verify_manifest};
pub use method::{DetectInputs, INSTALL_MARKER, InstallMethod, detect};
pub use offer::{CheckInput, ManualReason, Offer, evaluate};
pub use plan::{Replacement, Restart, UpdatePlan, plan};
pub use version::Version;

#[cfg(test)]
pub(crate) mod testkit;
