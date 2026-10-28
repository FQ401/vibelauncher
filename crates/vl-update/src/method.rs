//! How the running launcher was installed, which decides how it is replaced.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vl_core::platform::Os;

use crate::ArtifactKind;

/// The file next to the executable that names the install method (`msi`, `deb`, `rpm`,
/// `portable`), written by the package that installed it.
pub const INSTALL_MARKER: &str = "vibelauncher.install";

/// How the launcher was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method")]
pub enum InstallMethod {
    /// A Windows Installer package: updated by running the new package.
    Msi,
    /// A folder unpacked from an archive: updated by unpacking the new archive over it.
    Portable {
        /// The launcher folder (the executable's folder).
        dir: PathBuf,
    },
    /// A macOS application bundle: updated by copying the new bundle from the disk image.
    MacApp {
        /// The `.app` folder.
        bundle: PathBuf,
    },
    /// A Linux AppImage: updated by renaming the new file over the old one.
    AppImage {
        /// The AppImage file.
        file: PathBuf,
    },
    /// A Debian package: updated by the package manager.
    Deb,
    /// An RPM package: updated by the package manager.
    Rpm,
    /// Not recognized: the user updates by hand.
    Unknown,
}

impl InstallMethod {
    /// The artifact kind that replaces this install.
    pub fn artifact_kind(&self) -> Option<ArtifactKind> {
        match self {
            Self::Msi => Some(ArtifactKind::Msi),
            Self::Portable { .. } => Some(ArtifactKind::Portable),
            Self::MacApp { .. } => Some(ArtifactKind::Dmg),
            Self::AppImage { .. } => Some(ArtifactKind::AppImage),
            Self::Deb => Some(ArtifactKind::Deb),
            Self::Rpm => Some(ArtifactKind::Rpm),
            Self::Unknown => None,
        }
    }

    /// Whether the launcher replaces itself; otherwise it only points to the release page.
    pub fn self_updates(&self) -> bool {
        matches!(
            self,
            Self::Msi | Self::Portable { .. } | Self::MacApp { .. } | Self::AppImage { .. }
        )
    }
}

/// What detection looks at; the native layer reads it from the running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectInputs {
    /// The OS the launcher runs on.
    pub os: Os,
    /// The running executable (symbolic links resolved).
    pub exe: PathBuf,
    /// The `APPIMAGE` environment variable: the AppImage file the launcher was started from.
    pub appimage: Option<PathBuf>,
    /// The content of [`INSTALL_MARKER`] next to the executable, if there is one.
    pub marker: Option<String>,
}

/// The install method, first match wins:
///
/// 1. an [`INSTALL_MARKER`] next to the executable names it (`msi`, `deb`, `rpm`, `portable`;
///    anything else is [`InstallMethod::Unknown`]);
/// 2. on Linux, an absolute `APPIMAGE` path: an AppImage;
/// 3. on macOS, an executable at `<name>.app/Contents/MacOS/<exe>`: that bundle;
/// 4. on Linux, an executable below `/usr/`: a system install of unknown origin;
/// 5. on Windows, macOS and Linux otherwise: a portable folder, the executable's folder.
pub fn detect(inputs: &DetectInputs) -> InstallMethod {
    let dir = inputs.exe.parent().map(Path::to_path_buf);
    let portable = || match &dir {
        Some(dir) if !dir.as_os_str().is_empty() => InstallMethod::Portable { dir: dir.clone() },
        _ => InstallMethod::Unknown,
    };
    if let Some(marker) = &inputs.marker {
        return match marker.trim().to_ascii_lowercase().as_str() {
            "msi" if inputs.os == Os::Windows => InstallMethod::Msi,
            "deb" if inputs.os == Os::Linux => InstallMethod::Deb,
            "rpm" if inputs.os == Os::Linux => InstallMethod::Rpm,
            "portable" => portable(),
            _ => InstallMethod::Unknown,
        };
    }
    match inputs.os {
        Os::Linux => {
            if let Some(file) = inputs.appimage.as_ref().filter(|p| p.has_root()) {
                return InstallMethod::AppImage { file: file.clone() };
            }
            if inputs.exe.starts_with("/usr") {
                return InstallMethod::Unknown;
            }
            portable()
        }
        Os::MacOs => app_bundle(&inputs.exe)
            .map(|bundle| InstallMethod::MacApp { bundle })
            .unwrap_or_else(portable),
        Os::Windows => portable(),
        Os::Other => InstallMethod::Unknown,
    }
}

/// `X.app` for an executable at `X.app/Contents/MacOS/<exe>`.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_app = bundle
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| bundle.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(os: Os, exe: &str) -> DetectInputs {
        DetectInputs {
            os,
            exe: PathBuf::from(exe),
            appimage: None,
            marker: None,
        }
    }

    #[test]
    fn marker_wins() {
        let mut i = inputs(Os::Windows, "/apps/vl/vibelauncher.exe");
        i.marker = Some("msi\n".into());
        assert_eq!(detect(&i), InstallMethod::Msi);
        i.marker = Some("Portable".into());
        assert_eq!(
            detect(&i),
            InstallMethod::Portable {
                dir: "/apps/vl".into()
            }
        );
        i.marker = Some("something".into());
        assert_eq!(detect(&i), InstallMethod::Unknown);
        // A marker of another OS's package is not believed.
        i.marker = Some("deb".into());
        assert_eq!(detect(&i), InstallMethod::Unknown);

        let mut i = inputs(Os::Linux, "/usr/bin/vibelauncher");
        i.appimage = Some("/home/u/VibeLauncher.AppImage".into());
        i.marker = Some("deb".into());
        assert_eq!(detect(&i), InstallMethod::Deb);
        i.marker = Some("rpm".into());
        assert_eq!(detect(&i), InstallMethod::Rpm);
    }

    #[test]
    fn appimage_on_linux() {
        let mut i = inputs(Os::Linux, "/tmp/.mount_VibeLa/usr/bin/vibelauncher");
        i.appimage = Some("/home/u/Apps/VibeLauncher.AppImage".into());
        assert_eq!(
            detect(&i),
            InstallMethod::AppImage {
                file: "/home/u/Apps/VibeLauncher.AppImage".into()
            }
        );
        // A relative path is not trusted; the mount below /tmp then reads as a portable folder.
        i.appimage = Some("VibeLauncher.AppImage".into());
        assert_eq!(
            detect(&i),
            InstallMethod::Portable {
                dir: "/tmp/.mount_VibeLa/usr/bin".into()
            }
        );
    }

    #[test]
    fn mac_bundle() {
        let i = inputs(
            Os::MacOs,
            "/Applications/VibeLauncher.app/Contents/MacOS/vibelauncher",
        );
        assert_eq!(
            detect(&i),
            InstallMethod::MacApp {
                bundle: "/Applications/VibeLauncher.app".into()
            }
        );
        let i = inputs(Os::MacOs, "/Users/u/bin/vibelauncher");
        assert_eq!(
            detect(&i),
            InstallMethod::Portable {
                dir: "/Users/u/bin".into()
            }
        );
    }

    #[test]
    fn linux_system_and_portable() {
        assert_eq!(
            detect(&inputs(Os::Linux, "/usr/bin/vibelauncher")),
            InstallMethod::Unknown
        );
        assert_eq!(
            detect(&inputs(Os::Linux, "/home/u/vl/vibelauncher")),
            InstallMethod::Portable {
                dir: "/home/u/vl".into()
            }
        );
        assert_eq!(
            detect(&inputs(Os::Other, "/home/u/vl/vibelauncher")),
            InstallMethod::Unknown
        );
        assert_eq!(
            detect(&inputs(Os::Windows, "vibelauncher.exe")),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn artifact_kinds() {
        assert_eq!(InstallMethod::Msi.artifact_kind(), Some(ArtifactKind::Msi));
        assert_eq!(InstallMethod::Unknown.artifact_kind(), None);
        assert!(InstallMethod::Msi.self_updates());
        assert!(!InstallMethod::Deb.self_updates());
        assert!(!InstallMethod::Unknown.self_updates());
    }
}
