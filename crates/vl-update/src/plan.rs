//! The replacement as data: what the update helper does once the launcher has exited.
//!
//! | Install method | Replacement | Started afterwards |
//! |---|---|---|
//! | MSI | run the new package ([`Replacement::RunInstaller`]) | the executable |
//! | portable folder | unpack the archive over the folder ([`Replacement::Unpack`]) | the executable |
//! | macOS app | copy the bundle from the disk image ([`Replacement::CopyFromDmg`]) | the bundle |
//! | AppImage | rename the new file over the old one ([`Replacement::RenameOver`]) | the AppImage |
//! | deb, rpm, unknown | nothing; the release page is shown ([`Replacement::Link`]) | — |

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Artifact, InstallMethod, Version};

/// How the installed launcher is replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Replacement {
    /// Run the new Windows Installer package.
    RunInstaller {
        /// The verified package.
        package: PathBuf,
    },
    /// Unpack the archive over the launcher folder.
    Unpack {
        /// The verified zip archive.
        archive: PathBuf,
        /// The launcher folder.
        dir: PathBuf,
    },
    /// Copy the application bundle out of the disk image over the installed one.
    CopyFromDmg {
        /// The verified disk image.
        image: PathBuf,
        /// The installed `.app` folder.
        bundle: PathBuf,
    },
    /// Rename the new AppImage over the old one.
    RenameOver {
        /// The verified new AppImage.
        file: PathBuf,
        /// The AppImage the launcher runs from.
        target: PathBuf,
    },
    /// Nothing is replaced: the user installs the release from its page.
    Link,
}

/// What to start once the replacement is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "path")]
pub enum Restart {
    /// An executable file.
    Executable(PathBuf),
    /// A macOS application bundle (opened, not executed).
    Bundle(PathBuf),
}

/// One update, ready for the helper: the verified file, how to replace and what to restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlan {
    /// The version being installed.
    pub version: Version,
    /// The downloaded file.
    pub file: PathBuf,
    /// Its SHA-256 from the manifest, checked again right before the replacement.
    pub sha256: String,
    /// Its size from the manifest.
    pub size: u64,
    /// The replacement.
    pub replacement: Replacement,
    /// What to start afterwards.
    pub restart: Option<Restart>,
}

/// The plan for replacing the launcher installed as `method` and running as `exe` with the
/// downloaded `file` of `artifact`.
pub fn plan(
    method: &InstallMethod,
    exe: &Path,
    version: &Version,
    artifact: &Artifact,
    file: &Path,
) -> UpdatePlan {
    let exe = || Some(Restart::Executable(exe.to_path_buf()));
    let file_buf = file.to_path_buf();
    let (replacement, restart) = match method {
        InstallMethod::Msi => (Replacement::RunInstaller { package: file_buf }, exe()),
        InstallMethod::Portable { dir } => (
            Replacement::Unpack {
                archive: file_buf,
                dir: dir.clone(),
            },
            exe(),
        ),
        InstallMethod::MacApp { bundle } => (
            Replacement::CopyFromDmg {
                image: file_buf,
                bundle: bundle.clone(),
            },
            Some(Restart::Bundle(bundle.clone())),
        ),
        InstallMethod::AppImage { file: target } => (
            Replacement::RenameOver {
                file: file_buf,
                target: target.clone(),
            },
            Some(Restart::Executable(target.clone())),
        ),
        InstallMethod::Deb | InstallMethod::Rpm | InstallMethod::Unknown => {
            (Replacement::Link, None)
        }
    };
    UpdatePlan {
        version: version.clone(),
        file: file.to_path_buf(),
        sha256: artifact.sha256.clone(),
        size: artifact.size,
        replacement,
        restart,
    }
}

#[cfg(test)]
mod tests {
    use vl_core::platform::{Arch, Os, Platform};

    use super::*;
    use crate::ArtifactKind;

    fn artifact() -> Artifact {
        Artifact {
            platform: Platform {
                os: Os::Linux,
                arch: Arch::X86_64,
            },
            kind: ArtifactKind::AppImage,
            url: "https://downloads.example.test/a".into(),
            sha256: "cd".repeat(32),
            size: 42,
        }
    }

    #[test]
    fn table() {
        let v = Version::new(2, 0, 0);
        let exe = Path::new("/vl/vibelauncher");
        let file = Path::new("/data/updates/2.0.0/new");
        let p = |m: InstallMethod| plan(&m, exe, &v, &artifact(), file);

        let msi = p(InstallMethod::Msi);
        assert_eq!(
            msi.replacement,
            Replacement::RunInstaller {
                package: file.into()
            }
        );
        assert_eq!(msi.restart, Some(Restart::Executable(exe.into())));
        assert_eq!(msi.sha256, "cd".repeat(32));
        assert_eq!(msi.size, 42);

        let portable = p(InstallMethod::Portable { dir: "/vl".into() });
        assert_eq!(
            portable.replacement,
            Replacement::Unpack {
                archive: file.into(),
                dir: "/vl".into()
            }
        );

        let app = p(InstallMethod::MacApp {
            bundle: "/Applications/VibeLauncher.app".into(),
        });
        assert_eq!(
            app.restart,
            Some(Restart::Bundle("/Applications/VibeLauncher.app".into()))
        );

        let image = p(InstallMethod::AppImage {
            file: "/home/u/VL.AppImage".into(),
        });
        assert_eq!(
            image.replacement,
            Replacement::RenameOver {
                file: file.into(),
                target: "/home/u/VL.AppImage".into()
            }
        );
        assert_eq!(
            image.restart,
            Some(Restart::Executable("/home/u/VL.AppImage".into()))
        );

        for m in [
            InstallMethod::Deb,
            InstallMethod::Rpm,
            InstallMethod::Unknown,
        ] {
            let link = p(m);
            assert_eq!(link.replacement, Replacement::Link);
            assert_eq!(link.restart, None);
        }
    }

    #[test]
    fn plan_round_trips_as_json() {
        let plan = plan(
            &InstallMethod::Portable { dir: "/vl".into() },
            Path::new("/vl/vibelauncher"),
            &Version::new(2, 0, 0),
            &artifact(),
            Path::new("/data/new.zip"),
        );
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<UpdatePlan>(&json).unwrap(), plan);
    }
}
