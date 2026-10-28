//! Reading the install method of the running process.

use std::io::Read;
use std::path::{Path, PathBuf};

use vl_core::platform::Platform;
use vl_update::{DetectInputs, INSTALL_MARKER, InstallMethod, detect};

/// The largest install marker read.
const MAX_MARKER_BYTES: u64 = 256;

/// The install method of the running launcher: its executable, the `APPIMAGE` environment
/// variable and the install marker next to the executable ([`vl_update::detect`]).
/// [`InstallMethod::Unknown`] when the executable cannot be found.
pub fn current_install() -> InstallMethod {
    let Some(exe) = current_exe() else {
        return InstallMethod::Unknown;
    };
    detect(&inputs_for(
        Platform::current(),
        exe,
        std::env::var_os("APPIMAGE").map(PathBuf::from),
    ))
}

/// The running executable with symbolic links resolved where the OS allows it.
pub fn current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Windows keeps the path as started: canonicalizing would add a `\\?\` prefix.
    if cfg!(windows) {
        Some(exe)
    } else {
        Some(exe.canonicalize().unwrap_or(exe))
    }
}

/// The detection inputs for `exe`, reading the install marker next to it.
pub fn inputs_for(platform: Platform, exe: PathBuf, appimage: Option<PathBuf>) -> DetectInputs {
    let marker = exe
        .parent()
        .and_then(|dir| read_marker(&dir.join(INSTALL_MARKER)));
    DetectInputs {
        os: platform.os,
        exe,
        appimage,
        marker,
    }
}

fn read_marker(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_MARKER_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use vl_core::platform::{Arch, Os};

    use super::*;
    use crate::testkit::Dir;

    const WIN: Platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
    };

    #[test]
    fn reads_the_marker_next_to_the_executable() {
        let dir = Dir::new();
        let exe = dir.path().join("vibelauncher.exe");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(
            detect(&inputs_for(WIN, exe.clone(), None)),
            InstallMethod::Portable {
                dir: dir.path().to_path_buf()
            }
        );
        std::fs::write(dir.path().join(INSTALL_MARKER), b"msi\r\n").unwrap();
        assert_eq!(
            detect(&inputs_for(WIN, exe.clone(), None)),
            InstallMethod::Msi
        );
        // An oversized marker is cut and so not recognized.
        std::fs::write(dir.path().join(INSTALL_MARKER), "msi".repeat(200)).unwrap();
        assert_eq!(detect(&inputs_for(WIN, exe, None)), InstallMethod::Unknown);
    }

    #[test]
    fn the_test_binary_has_some_method() {
        // Whatever it is, detection of the running process works without panicking.
        let _ = current_install();
        assert!(current_exe().is_some());
    }
}
