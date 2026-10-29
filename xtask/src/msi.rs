//! `cargo xtask package msi [--target <triple>]`: the per-user Windows Installer package.
//!
//! Builds `vibelauncher` and `vl-cli` in release mode for a Windows MSVC target, stages them with
//! the install marker (`msi`) and the license, and builds `packaging/windows/vibelauncher.wxs`
//! with WiX 7 into `target/dist/vibelauncher-<version>-windows-<arch>.msi` with a `.sha256` file
//! next to it, checked with `wix msi validate`. The package installs into
//! `%LOCALAPPDATA%\Programs\VibeLauncher` without elevation; the self-update installs the next
//! release by running its package.
//!
//! WiX is the .NET tool pinned in `.config/dotnet-tools.json` (`dotnet tool restore`, then
//! `dotnet tool run wix`), or the `wix` executable named by the `VL_WIX` variable. Its binary
//! release comes with the Open Source Maintenance Fee EULA, which the project owner accepted:
//! the build passes `-acceptEula wix7`.

use std::fs;
use std::path::Path;

use crate::package::{BINARIES, build_release, host_target, read, windows_arch, write_with_sums};
use crate::{read_version, root};

/// The upgrade code of the package. Fixed forever: a package with another upgrade code would
/// install next to the existing one instead of upgrading it.
pub const UPGRADE_CODE: &str = "A5F7397D-6AC9-45E3-8E72-BDE8D702D003";
/// The WiX EULA accepted for the build (WiX 7's Open Source Maintenance Fee agreement).
const WIX_EULA: &str = "wix7";
/// The WiX source of the package, below the workspace root.
const SOURCE: [&str; 3] = ["packaging", "windows", "vibelauncher.wxs"];

/// Builds the package for `target` (the host when `None`).
pub fn package(target: Option<&str>) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("the MSI package is built on Windows (WiX 7)".into());
    }
    let target = match target {
        Some(t) => t.to_owned(),
        None => host_target()?,
    };
    let arch = windows_arch(&target)?;
    let version = read_version()?;
    let product_version = product_version(&version)?;
    build_release(&target, false)?;

    let release = root().join("target").join(&target).join("release");
    let dist = root().join("target").join("dist");
    let staging = dist.join("staging-msi");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let files = staging.join("files");
    fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;
    for (_, bin) in BINARIES {
        let exe = format!("{bin}.exe");
        write(&files.join(&exe), &read(&release.join(&exe))?)?;
    }
    write(&files.join(vl_update::INSTALL_MARKER), b"msi\n")?;
    write(&files.join("LICENSE.txt"), &read(&root().join("LICENSE"))?)?;

    let name = package_name(&version, arch);
    let out = staging.join(&name);
    let icon = root().join("assets").join("icons").join("vibelauncher.ico");
    let args = wix_args(&source(), arch, product_version, &files, &icon, &out);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    wix(&args)?;
    let out_arg = out.to_string_lossy();
    wix(&validate_args(&out_arg))?;
    write_with_sums(&dist, &name, &read(&out)?)
}

fn source() -> std::path::PathBuf {
    SOURCE.iter().fold(root(), |path, part| path.join(part))
}

/// `vibelauncher-<version>-windows-<arch>.msi`.
fn package_name(version: &str, arch: &str) -> String {
    format!("vibelauncher-{version}-windows-{arch}.msi")
}

/// The package's `ProductVersion`: the numeric `X.Y.Z` of `version` (a pre-release suffix
/// dropped). Windows Installer compares the first three fields only and limits them to 255, 255
/// and 65535.
fn product_version(version: &str) -> Result<&str, String> {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let parts: Vec<u32> = core
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<_>>()
        .ok_or_else(|| format!("`{version}` is not X.Y.Z"))?;
    match parts.as_slice() {
        [major, minor, patch] if *major <= 255 && *minor <= 255 && *patch <= 65_535 => Ok(core),
        _ => Err(format!(
            "`{version}` does not fit a Windows Installer version (at most 255.255.65535)"
        )),
    }
}

/// The arguments of `wix build`.
fn wix_args(
    source: &Path,
    arch: &str,
    version: &str,
    files: &Path,
    icon: &Path,
    out: &Path,
) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut args: Vec<String> = ["build", "-acceptEula", WIX_EULA, "-arch", arch]
        .map(str::to_owned)
        .to_vec();
    for (name, value) in [
        ("Version", version.to_owned()),
        ("UpgradeCode", UPGRADE_CODE.to_owned()),
        ("Files", path(files)),
        ("Icon", path(icon)),
    ] {
        args.extend(["-d".to_owned(), format!("{name}={value}")]);
    }
    args.extend([path(source), "-o".to_owned(), path(out)]);
    args
}

/// The arguments of `wix msi validate`, which runs the Windows Installer consistency checks
/// (ICE) on `package`. Two warnings are expected and left out: ICE61, as a package of the same
/// version replaces the installed one on purpose, and ICE91, as the package is per-user only.
fn validate_args(package: &str) -> [&str; 9] {
    [
        "msi",
        "validate",
        "-acceptEula",
        WIX_EULA,
        "-sice",
        "ICE61",
        "-sice",
        "ICE91",
        package,
    ]
}

/// Runs WiX with `args`: the `VL_WIX` executable when set, else the pinned .NET tool. (`WIX` is
/// taken: the WiX 3 installer sets it to its folder, as on the GitHub Windows runners.)
fn wix(args: &[&str]) -> Result<(), String> {
    if let Some(wix) = std::env::var_os("VL_WIX").filter(|w| !w.is_empty()) {
        let wix = wix.to_string_lossy().into_owned();
        return crate::run("wix", &wix, args);
    }
    crate::run("wix", "dotnet", &["tool", "restore"])?;
    let mut all = vec!["tool", "run", "wix", "--"];
    all.extend_from_slice(args);
    crate::run("wix", "dotnet", &all)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions() {
        assert_eq!(
            package_name("1.2.3", "x64"),
            "vibelauncher-1.2.3-windows-x64.msi"
        );
        assert_eq!(product_version("1.2.3"), Ok("1.2.3"));
        assert_eq!(product_version("1.2.3-beta.1"), Ok("1.2.3"));
        assert_eq!(product_version("255.255.65535"), Ok("255.255.65535"));
        for bad in ["256.0.0", "1.256.0", "1.0.65536", "1.2", "1.2.x"] {
            assert!(product_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wix_accepts_the_eula_and_gets_every_variable() {
        let args = wix_args(
            Path::new("p.wxs"),
            "x64",
            "1.2.3",
            Path::new("files"),
            Path::new("i.ico"),
            Path::new("o.msi"),
        );
        let joined = args.join(" ");
        assert!(joined.starts_with("build -acceptEula wix7 -arch x64 "));
        for var in ["Version=1.2.3", "Files=files", "Icon=i.ico"] {
            assert!(args.iter().any(|a| a == var), "{var}");
        }
        assert!(args.contains(&format!("UpgradeCode={UPGRADE_CODE}")));
        assert!(joined.ends_with("p.wxs -o o.msi"));
    }

    #[test]
    fn the_source_uses_every_variable_and_the_apps_user_model_id() {
        let wxs = fs::read_to_string(source()).unwrap();
        for var in ["Version", "UpgradeCode", "Files", "Icon"] {
            assert!(wxs.contains(&format!("$(var.{var})")), "{var}");
        }
        assert!(wxs.contains("Scope=\"perUser\""));
        assert!(wxs.contains(&format!("\\{}\"", vl_update::INSTALL_MARKER)));
        // The shortcut carries the ID the desktop app sets for itself.
        let app = fs::read_to_string(root().join("apps/vibelauncher/src/main.rs")).unwrap();
        let id = app
            .lines()
            .find_map(|l| l.trim().strip_prefix("const APP_USER_MODEL_ID: &str = \""))
            .and_then(|rest| rest.strip_suffix("\";"))
            .unwrap();
        assert!(wxs.contains(&format!("Key=\"System.AppUserModel.ID\" Value=\"{id}\"")));
    }
}
