//! Checking the signed manifest and downloading the release file into the staging folder.

use std::path::PathBuf;

use vl_core::platform::Platform;
use vl_net::{Checksum, Client, Control, Download, GetOptions};
use vl_update::key::MAX_SIGNATURE_BYTES;
use vl_update::manifest::MAX_MANIFEST_BYTES;
use vl_update::{
    Artifact, ArtifactKind, CheckInput, InstallMethod, Offer, UpdateKey, Version, evaluate,
    verify_manifest,
};

use crate::{UpdateDir, UpdaterError};

/// The suffix of the detached signature next to the manifest URL.
pub const SIGNATURE_SUFFIX: &str = ".minisig";

/// Checks for updates and downloads them.
#[derive(Clone)]
pub struct Updater {
    client: Client,
    key: UpdateKey,
    manifest_url: String,
    dir: UpdateDir,
}

impl Updater {
    /// An updater that trusts the release key embedded in the binary. Fails closed with
    /// [`vl_update::UpdateError::NoReleaseKey`] when the build has none.
    pub fn release(
        client: Client,
        manifest_url: impl Into<String>,
        dir: UpdateDir,
    ) -> Result<Self, UpdaterError> {
        Ok(Self::with_key(
            client,
            UpdateKey::release()?,
            manifest_url,
            dir,
        ))
    }

    /// An updater that trusts `key`.
    pub fn with_key(
        client: Client,
        key: UpdateKey,
        manifest_url: impl Into<String>,
        dir: UpdateDir,
    ) -> Self {
        Self {
            client,
            key,
            manifest_url: manifest_url.into(),
            dir,
        }
    }

    /// The update folder.
    pub fn dir(&self) -> &UpdateDir {
        &self.dir
    }

    /// Fetches the manifest and its signature (`<manifest url>.minisig`) past the HTTP cache,
    /// each with a size cap, verifies the signature over the exact bytes, refuses a rollback and
    /// decides. The version of every manifest that passes is remembered as the new rollback
    /// floor.
    pub fn check(
        &self,
        current: &Version,
        platform: Platform,
        method: &InstallMethod,
        control: &Control,
    ) -> Result<Offer, UpdaterError> {
        if !self.manifest_url.starts_with("https://") {
            return Err(UpdaterError::Net(vl_net::NetError::InvalidUrl(
                "the update manifest url is not https".into(),
            )));
        }
        let fetch = |url: &str, cap: usize| {
            let options = GetOptions {
                max_bytes: Some(cap as u64),
                max_age: None,
                use_cache: false,
            };
            self.client.get_bytes(url, &options, control)
        };
        let manifest = fetch(&self.manifest_url, MAX_MANIFEST_BYTES)?;
        let signature = fetch(
            &format!("{}{SIGNATURE_SUFFIX}", self.manifest_url),
            MAX_SIGNATURE_BYTES,
        )?;
        let manifest = verify_manifest(&manifest.bytes, &signature.bytes, &self.key)?;
        let input = CheckInput {
            current: current.clone(),
            highest_seen: self.dir.highest_seen(),
            platform,
            method: method.clone(),
        };
        let offer = evaluate(&manifest, &input)?;
        self.dir.record_seen(&manifest.version)?;
        Ok(offer)
    }

    /// Downloads `artifact` of `version` into its staging folder with the size and SHA-256 of the
    /// manifest checked before the file appears; an already staged file that matches is kept.
    /// Downloads of other versions are removed. Progress and cancellation come from `control`.
    pub fn stage(
        &self,
        version: &Version,
        artifact: &Artifact,
        control: &Control,
    ) -> Result<PathBuf, UpdaterError> {
        let checksum = Checksum::sha256(&artifact.sha256)
            .ok_or_else(|| UpdaterError::Integrity("invalid sha256 in the manifest".into()))?;
        self.dir.clear_staging(Some(version));
        let dest = self
            .dir
            .staging(version)
            .join(file_name(version, artifact.kind));
        let spec = Download::new(artifact.url.clone(), dest)
            .checksum(checksum)
            .size(artifact.size);
        Ok(self.client.download(&spec, control)?.path)
    }
}

/// `vibelauncher-<version>.<extension of the kind>`.
pub fn file_name(version: &Version, kind: ArtifactKind) -> String {
    let ext = match kind {
        ArtifactKind::Msi => "msi",
        ArtifactKind::Portable => "zip",
        ArtifactKind::Dmg => "dmg",
        ArtifactKind::AppImage => "AppImage",
        ArtifactKind::Deb => "deb",
        ArtifactKind::Rpm => "rpm",
    };
    format!("vibelauncher-{version}.{ext}")
}
