//! `cargo xtask package portable [--target <triple>]`: the portable Windows archive.
//!
//! Builds `vibelauncher` and `vl-cli` in release mode for a Windows MSVC target and writes
//! `target/dist/vibelauncher-<version>-windows-<arch>-portable.zip` with a `.sha256` file next to
//! it. The archive's root is the launcher folder: both executables, the portable marker (data in
//! a `data` folder next to the executables) and the license. Its layout is what the self-update
//! unpacks over a portable folder.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

use crate::{read_version, root, run};

/// The marker file that switches on portable mode (`vl_store::PORTABLE_MARKER`).
const PORTABLE_MARKER: &str = "vibelauncher.portable";
/// The executables in the archive, as `(package, binary)`.
const BINARIES: [(&str, &str); 2] = [("vl-app", "vibelauncher"), ("vl-cli", "vl-cli")];

/// Builds the portable archive for `target` (the host when `None`); returns its path.
pub fn portable(target: Option<&str>) -> Result<PathBuf, String> {
    let target = match target {
        Some(t) => t.to_owned(),
        None => host_target()?,
    };
    let arch = windows_arch(&target)?;
    let version = read_version()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut build = vec!["build", "--release", "--locked", "--target", &target];
    for (package, _) in BINARIES {
        build.extend(["-p", package]);
    }
    run("build", &cargo, &build)?;

    let release = root().join("target").join(&target).join("release");
    let dist = root().join("target").join("dist");
    fs::create_dir_all(&dist).map_err(|e| format!("{}: {e}", dist.display()))?;
    let name = archive_name(&version, arch);
    let path = dist.join(&name);
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for (_, bin) in BINARIES {
        let exe = format!("{bin}.exe");
        entries.push((exe.clone(), read(&release.join(&exe))?));
    }
    entries.push((PORTABLE_MARKER.to_owned(), Vec::new()));
    entries.push(("LICENSE.txt".to_owned(), read(&root().join("LICENSE"))?));
    let bytes = zip_of(&entries)?;
    fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;

    let sha = hex(&Sha256::digest(&bytes));
    let sums = dist.join(format!("{name}.sha256"));
    fs::write(&sums, format!("{sha}  {name}\n")).map_err(|e| format!("{}: {e}", sums.display()))?;
    println!("{} ({} bytes, sha256 {sha})", path.display(), bytes.len());
    Ok(path)
}

/// `vibelauncher-<version>-windows-<arch>-portable.zip`.
fn archive_name(version: &str, arch: &str) -> String {
    format!("vibelauncher-{version}-windows-{arch}-portable.zip")
}

/// The architecture name used in file names for a Windows MSVC target triple.
fn windows_arch(target: &str) -> Result<&'static str, String> {
    let arch = match target {
        "x86_64-pc-windows-msvc" => "x64",
        "aarch64-pc-windows-msvc" => "arm64",
        "i686-pc-windows-msvc" => "x86",
        _ => {
            return Err(format!(
                "the portable archive is built for a Windows MSVC target, not `{target}` \
                 (pass --target x86_64-pc-windows-msvc)"
            ));
        }
    };
    Ok(arch)
}

/// The host target triple, from `rustc -vV`.
fn host_target() -> Result<String, String> {
    let output = std::process::Command::new("rustc")
        .arg("-vV")
        .current_dir(root())
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_owned())
        .ok_or_else(|| "rustc -vV printed no host".to_owned())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// A zip archive of `entries` (name, content) at its root, deflated, with the fixed zip epoch as
/// the time of every entry so the same files give the same archive.
fn zip_of(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);
    for (name, content) in entries {
        zip.start_file(name.as_str(), options)
            .map_err(|e| format!("{name}: {e}"))?;
        zip.write_all(content).map_err(|e| format!("{name}: {e}"))?;
    }
    zip.finish()
        .map(std::io::Cursor::into_inner)
        .map_err(|e| format!("zip: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn names_follow_the_target() {
        assert_eq!(windows_arch("x86_64-pc-windows-msvc"), Ok("x64"));
        assert_eq!(windows_arch("aarch64-pc-windows-msvc"), Ok("arm64"));
        assert!(windows_arch("x86_64-unknown-linux-gnu").is_err());
        assert!(windows_arch("x86_64-pc-windows-gnu").is_err());
        assert_eq!(
            archive_name("1.2.3", "x64"),
            "vibelauncher-1.2.3-windows-x64-portable.zip"
        );
    }

    #[test]
    fn the_archive_holds_the_files_at_its_root_and_is_reproducible() {
        let entries = vec![
            ("vibelauncher.exe".to_owned(), b"exe".to_vec()),
            (PORTABLE_MARKER.to_owned(), Vec::new()),
        ];
        let bytes = zip_of(&entries).unwrap();
        assert_eq!(bytes, zip_of(&entries).unwrap());
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["vibelauncher.exe", PORTABLE_MARKER]);
        let mut exe = String::new();
        zip.by_name("vibelauncher.exe")
            .unwrap()
            .read_to_string(&mut exe)
            .unwrap();
        assert_eq!(exe, "exe");
    }
}
