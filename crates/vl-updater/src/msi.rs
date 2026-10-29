//! Windows: installing an update by running the new Windows Installer package.
//!
//! The release package is per-user (`%LOCALAPPDATA%\Programs\VibeLauncher`), so it installs
//! without elevation. Its major upgrade removes the installed version and installs the new one
//! into the same folder. `msiexec /i <package> /passive /norestart` shows only a progress bar and
//! never restarts Windows; the log goes next to the package (`<package>.log`), which stays in
//! the staging folder when the installation fails.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::UpdaterError;

/// Installs the Windows Installer package `package` and waits for it.
pub fn install_msi(package: &Path) -> Result<(), UpdaterError> {
    if !cfg!(windows) {
        return Err(UpdaterError::NotImplemented(
            "installing an MSI package outside Windows",
        ));
    }
    run_msiexec(Command::new(msiexec()), package)
}

/// `msiexec.exe` in the system folder. Named by its full path: a program name alone is looked up
/// in the helper's own folder first.
fn msiexec() -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| OsString::from(r"C:\Windows"));
    PathBuf::from(root).join("System32").join("msiexec.exe")
}

/// The log file of installing `package`.
pub fn log_path(package: &Path) -> PathBuf {
    let mut name = package.as_os_str().to_owned();
    name.push(".log");
    PathBuf::from(name)
}

/// The arguments of `msiexec` that install `package` with a progress bar only, without a
/// restart, logging to `log`.
pub fn msiexec_args(package: &Path, log: &Path) -> Vec<OsString> {
    vec![
        "/i".into(),
        package.into(),
        "/passive".into(),
        "/norestart".into(),
        "/l*".into(),
        log.into(),
    ]
}

/// Runs `command` (`msiexec` or a stand-in) with the arguments for `package` and waits for it.
fn run_msiexec(mut command: Command, package: &Path) -> Result<(), UpdaterError> {
    let log = log_path(package);
    let status = command
        .args(msiexec_args(package, &log))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| UpdaterError::Io(format!("msiexec: {e}")))?;
    exit_result(status.code(), &log)
}

/// The result of `msiexec` exiting with `code`: success, also when Windows asks for a restart
/// (3010, 1641), else an error naming the log.
pub fn exit_result(code: Option<i32>, log: &Path) -> Result<(), UpdaterError> {
    let what = match code {
        Some(0 | 3010 | 1641) => return Ok(()),
        Some(1602) => "the installation was cancelled".to_owned(),
        Some(1618) => "another installation is in progress".to_owned(),
        Some(1638) => "another version of the launcher is installed for all users".to_owned(),
        Some(code) => format!("msiexec failed with exit code {code}"),
        None => "msiexec was stopped".to_owned(),
    };
    Err(UpdaterError::Io(format!(
        "installing the package: {what} (log: {})",
        log.display()
    )))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::testkit::Dir;

    const ARGS_VAR: &str = "VL_TEST_FAKE_MSIEXEC_ARGS";
    const CODE_VAR: &str = "VL_TEST_FAKE_MSIEXEC_CODE";

    /// Plays `msiexec`: writes its arguments, one per line, to the file named by
    /// `VL_TEST_FAKE_MSIEXEC_ARGS` and exits with `VL_TEST_FAKE_MSIEXEC_CODE`. Does nothing when
    /// run by hand.
    #[test]
    #[ignore = "a process started by the MSI tests"]
    fn fake_msiexec() {
        let Some(out) = std::env::var_os(ARGS_VAR) else {
            return;
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        fs::write(out, args.join("\n")).unwrap();
        let code = std::env::var(CODE_VAR).ok().and_then(|c| c.parse().ok());
        std::process::exit(code.unwrap_or(0));
    }

    /// This test binary as `msiexec`, exiting with `code`; its arguments land in `args`.
    fn fake(args: &Path, code: i32) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "msi::tests::fake_msiexec",
                "--ignored",
                "--test-threads=1",
                "--quiet",
            ])
            .env(ARGS_VAR, args)
            .env(CODE_VAR, code.to_string());
        command
    }

    #[test]
    fn runs_the_package_passively_without_a_restart() {
        let dir = Dir::new();
        let package = dir.path().join("vibelauncher-2.0.0-windows-x64.msi");
        let args = dir.path().join("args.txt");
        run_msiexec(fake(&args, 0), &package).unwrap();
        let seen = fs::read_to_string(&args).unwrap();
        let seen: Vec<&str> = seen.lines().collect();
        let package_arg = package.to_string_lossy();
        let log_arg = log_path(&package).to_string_lossy().into_owned();
        let tail = [
            "/i",
            &package_arg,
            "/passive",
            "/norestart",
            "/l*",
            &log_arg,
        ];
        assert!(seen.ends_with(&tail), "{seen:?}");
        assert!(log_arg.ends_with(".msi.log"));
    }

    #[test]
    fn a_failed_installation_names_its_log() {
        let dir = Dir::new();
        let package = dir.path().join("update.msi");
        let args = dir.path().join("args.txt");
        let err = run_msiexec(fake(&args, 1603), &package).unwrap_err();
        let UpdaterError::Io(message) = err else {
            panic!("{err:?}");
        };
        assert!(message.contains("exit code 1603"), "{message}");
        assert!(message.contains("update.msi.log"), "{message}");
        // A restart Windows asks for is still a success.
        run_msiexec(fake(&args, 3010), &package).unwrap();
    }

    #[test]
    fn exit_codes() {
        let log = Path::new("x.log");
        for ok in [0, 3010, 1641] {
            assert_eq!(exit_result(Some(ok), log), Ok(()));
        }
        for (code, text) in [
            (Some(1602), "cancelled"),
            (Some(1618), "in progress"),
            (Some(1638), "for all users"),
            (Some(1620), "exit code 1620"),
            (None, "stopped"),
        ] {
            let err = exit_result(code, log).unwrap_err().to_string();
            assert!(err.contains(text), "{err}");
        }
    }

    #[test]
    fn a_missing_installer_is_an_error() {
        let dir = Dir::new();
        let result = run_msiexec(
            Command::new(dir.path().join("no-msiexec")),
            &dir.path().join("p.msi"),
        );
        assert!(matches!(result, Err(UpdaterError::Io(_))));
    }

    #[test]
    fn outside_windows_nothing_is_run() {
        if !cfg!(windows) {
            assert!(matches!(
                install_msi(Path::new("p.msi")),
                Err(UpdaterError::NotImplemented(_))
            ));
        } else {
            assert!(msiexec().ends_with(r"System32\msiexec.exe"));
        }
    }

    /// The release package named by `VL_TEST_MSI` (the `package` workflow sets it) installs
    /// through the self-update's path for the current user: run by the CI, which checks the
    /// installed launcher afterwards.
    #[test]
    fn the_release_package_installs() {
        let Some(release) = std::env::var_os("VL_TEST_MSI") else {
            return;
        };
        let dir = Dir::new();
        let package = dir.path().join("update.msi");
        fs::copy(release, &package).unwrap();
        let result = install_msi(&package);
        let log = fs::read(log_path(&package)).unwrap_or_default();
        assert!(
            result.is_ok(),
            "{result:?}\n{}",
            String::from_utf8_lossy(&log)
        );
    }
}
