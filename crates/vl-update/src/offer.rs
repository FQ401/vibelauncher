//! What a verified manifest means for the running launcher.

use serde::{Deserialize, Serialize};
use vl_core::platform::Platform;

use crate::{Artifact, InstallMethod, Manifest, UpdateError, Version};

/// The running launcher, as the decision sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckInput {
    /// The running version.
    pub current: Version,
    /// The newest manifest version verified before (kept by the native layer), if any.
    pub highest_seen: Option<Version>,
    /// The platform of the running binary.
    pub platform: Platform,
    /// How it was installed.
    pub method: InstallMethod,
}

/// Why a newer version has to be installed by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ManualReason {
    /// The launcher came from a system package; the package manager updates it.
    PackageManager,
    /// The install method was not recognized.
    UnknownInstall,
    /// The running version is older than the manifest's minimum version.
    TooOld {
        /// The oldest version that may update itself.
        minimum: Version,
    },
    /// The release has no file for this platform and install method.
    NoArtifact,
}

/// The decision for one manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "offer")]
pub enum Offer {
    /// The running version is the released one.
    UpToDate,
    /// The launcher can replace itself with `artifact`.
    Update {
        /// The new version.
        version: Version,
        /// The release date.
        date: String,
        /// The file to download.
        artifact: Artifact,
    },
    /// A newer version exists; the user installs it from `page`.
    Manual {
        /// The new version.
        version: Version,
        /// The release page.
        page: String,
        /// Why it cannot be installed automatically.
        reason: ManualReason,
    },
}

/// Decides what `manifest` means for the launcher in `input`.
///
/// Rollback is refused: a manifest older than the running version or than the newest manifest
/// verified before is [`UpdateError::Rollback`], so an old signed manifest replayed by a mirror
/// or an attacker can neither downgrade the launcher nor hide newer releases.
pub fn evaluate(manifest: &Manifest, input: &CheckInput) -> Result<Offer, UpdateError> {
    let floor = match &input.highest_seen {
        Some(seen) if *seen > input.current => seen,
        _ => &input.current,
    };
    if manifest.version < *floor {
        return Err(UpdateError::Rollback {
            offered: manifest.version.clone(),
            floor: floor.clone(),
        });
    }
    if manifest.version <= input.current {
        return Ok(Offer::UpToDate);
    }
    let manual = |reason| Offer::Manual {
        version: manifest.version.clone(),
        page: manifest.page.clone(),
        reason,
    };
    let reason = match &input.method {
        InstallMethod::Deb | InstallMethod::Rpm => Some(ManualReason::PackageManager),
        InstallMethod::Unknown => Some(ManualReason::UnknownInstall),
        _ if input.current < manifest.minimum_version => Some(ManualReason::TooOld {
            minimum: manifest.minimum_version.clone(),
        }),
        _ => None,
    };
    if let Some(reason) = reason {
        return Ok(manual(reason));
    }
    let artifact = input
        .method
        .artifact_kind()
        .and_then(|kind| manifest.artifact(input.platform, kind));
    Ok(match artifact {
        Some(artifact) => Offer::Update {
            version: manifest.version.clone(),
            date: manifest.date.clone(),
            artifact: artifact.clone(),
        },
        None => manual(ManualReason::NoArtifact),
    })
}

#[cfg(test)]
mod tests {
    use vl_core::platform::{Arch, Os};

    use super::*;
    use crate::ArtifactKind;
    use crate::testkit::manifest_json;

    const WIN: Platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };

    fn manifest(version: &str) -> Manifest {
        Manifest::parse(manifest_json(version).as_bytes()).unwrap()
    }

    fn input(current: &str, method: InstallMethod) -> CheckInput {
        CheckInput {
            current: Version::parse(current).unwrap(),
            highest_seen: None,
            platform: WIN,
            method,
        }
    }

    #[test]
    fn offers_the_artifact_of_the_install_method() {
        let offer = evaluate(&manifest("1.4.0"), &input("1.3.0", InstallMethod::Msi)).unwrap();
        let Offer::Update {
            version, artifact, ..
        } = offer
        else {
            panic!("{offer:?}");
        };
        assert_eq!(version, Version::new(1, 4, 0));
        assert_eq!(artifact.kind, ArtifactKind::Msi);
        assert_eq!(artifact.platform, WIN);

        let portable = InstallMethod::Portable { dir: "/vl".into() };
        let offer = evaluate(&manifest("1.4.0"), &input("1.3.0", portable)).unwrap();
        assert!(
            matches!(&offer, Offer::Update { artifact, .. } if artifact.kind == ArtifactKind::Portable)
        );
    }

    #[test]
    fn up_to_date() {
        let offer = evaluate(&manifest("1.4.0"), &input("1.4.0", InstallMethod::Msi)).unwrap();
        assert_eq!(offer, Offer::UpToDate);
    }

    #[test]
    fn refuses_rollback() {
        let err = evaluate(&manifest("1.3.0"), &input("1.4.0", InstallMethod::Msi)).unwrap_err();
        assert_eq!(
            err,
            UpdateError::Rollback {
                offered: Version::new(1, 3, 0),
                floor: Version::new(1, 4, 0),
            }
        );
        // A replayed manifest older than one seen before is refused even when it is newer than
        // the running version.
        let mut i = input("1.0.0", InstallMethod::Msi);
        i.highest_seen = Some(Version::new(1, 4, 0));
        let err = evaluate(&manifest("1.3.0"), &i).unwrap_err();
        assert!(
            matches!(err, UpdateError::Rollback { floor, .. } if floor == Version::new(1, 4, 0))
        );
        // The same manifest again is fine.
        assert!(matches!(
            evaluate(&manifest("1.4.0"), &i).unwrap(),
            Offer::Update { .. }
        ));
        // A seen version older than the running one does not lower the floor.
        let mut i = input("1.4.0", InstallMethod::Msi);
        i.highest_seen = Some(Version::new(1, 0, 0));
        assert!(evaluate(&manifest("1.3.0"), &i).is_err());
    }

    #[test]
    fn manual_cases() {
        let m = manifest("1.4.0");
        let reason = |i: &CheckInput| match evaluate(&m, i).unwrap() {
            Offer::Manual { reason, page, .. } => {
                assert_eq!(page, m.page);
                reason
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            reason(&input("1.3.0", InstallMethod::Deb)),
            ManualReason::PackageManager
        );
        assert_eq!(
            reason(&input("1.3.0", InstallMethod::Unknown)),
            ManualReason::UnknownInstall
        );
        assert_eq!(
            reason(&input("0.0.9", InstallMethod::Msi)),
            ManualReason::TooOld {
                minimum: Version::new(0, 1, 0)
            }
        );
        // No dmg for Windows, no rpm in the manifest at all.
        let app = InstallMethod::MacApp {
            bundle: "/Applications/VibeLauncher.app".into(),
        };
        assert_eq!(reason(&input("1.3.0", app)), ManualReason::NoArtifact);
        let mut linux = input("1.3.0", InstallMethod::Rpm);
        linux.platform.os = Os::Linux;
        assert_eq!(reason(&linux), ManualReason::PackageManager);
    }

    #[test]
    fn prerelease_current_updates_to_the_release() {
        let offer = evaluate(&manifest("1.4.0"), &input("1.4.0-rc.1", InstallMethod::Msi)).unwrap();
        assert!(matches!(offer, Offer::Update { .. }));
    }
}
